//! SCP against a real OpenSSH server. Ignored by default; run with
//! `PRZM_SCP=user:password@host:port cargo test -p przmterm-transport
//! --test scp_live -- --ignored`. With `PRZM_SCP_ROOT=rootpassword` the
//! test also sends as root through sudo (the user's password) and su.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use przmterm_transport::scp::{Direction, Request, ScpEvent};
use przmterm_transport::sftp::{SftpClient, SftpEvent};
use przmterm_transport::ssh::{Credential, SshConfig, SshTransport};
use przmterm_transport::{Size, Transport, noop_waker};
use zeroize::Zeroizing;

fn transfer(t: &SshTransport, request: Request) -> Result<Vec<PathBuf>, String> {
    let scp = t.scp(request, noop_waker()).expect("logged in");
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        for e in scp.poll() {
            if let ScpEvent::Done(result) = e {
                return result;
            }
        }
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "needs a real SSH server (PRZM_SCP)"]
fn upload_and_download_as_user_and_root() {
    let spec = std::env::var("PRZM_SCP").expect("PRZM_SCP=user:password@host:port");
    let (cred, addr) = spec.split_once('@').unwrap();
    let (user, password) = cred.split_once(':').unwrap();
    let (host, port) = addr.split_once(':').unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut config = SshConfig::new(host, port.parse().unwrap(), dir.path().join("kh"));
    config.username = Some(user.into());
    config.use_agent = false;
    config.use_default_identities = false;
    config.extra_known_hosts.clear();
    config.credential = Some(Credential {
        name: "test".into(),
        password: Some(password.into()),
        passphrase: None,
    });
    let mut t = SshTransport::connect(config, Size { cols: 80, rows: 24 }, noop_waker());
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut text = String::new();
    loop {
        let mut buf = [0u8; 4096];
        while let Ok(n) = t.read(&mut buf) {
            if n == 0 {
                break;
            }
            text.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        if text.contains("(yes/no)") && !text.contains("[answered]") {
            t.write(b"yes\r").unwrap();
            text.push_str("[answered]");
        }
        if t.sftp(noop_waker()).is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "no login: {text}");
        std::thread::sleep(Duration::from_millis(50));
    }

    let local = dir.path().join("przm-scp-test");
    std::fs::create_dir_all(local.join("sub")).unwrap();
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(local.join("sub/data.bin"), &data).unwrap();
    let remote = format!("/tmp/przm-scp-{}", std::process::id());
    let none = || Zeroizing::new(String::new());

    // A folder up as the login user, and back.
    let r = transfer(
        &t,
        Request {
            direction: Direction::Upload {
                files: vec![local.clone()],
                remote_dir: "/tmp".into(),
            },
            shell: None,
            password: none(),
        },
    );
    assert!(r.is_ok(), "{r:?}");
    let back = dir.path().join("back");
    let r = transfer(
        &t,
        Request {
            direction: Direction::Download {
                remote: "/tmp/przm-scp-test".into(),
                local_dir: back.clone(),
            },
            shell: None,
            password: none(),
        },
    );
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(
        std::fs::read(back.join("przm-scp-test/sub/data.bin")).unwrap(),
        data
    );

    // A missing file is reported.
    let r = transfer(
        &t,
        Request {
            direction: Direction::Download {
                remote: format!("{remote}/missing"),
                local_dir: back.clone(),
            },
            shell: None,
            password: none(),
        },
    );
    assert!(r.unwrap_err().contains("No such file"));

    let Ok(root_pw) = std::env::var("PRZM_SCP_ROOT") else {
        return;
    };
    let file = local.join("sub/data.bin");
    for (shell, pw) in [
        ("sudo su -", password.to_string()),
        ("su -", root_pw.clone()),
    ] {
        let run_as = Some(shell.to_string());
        let r = transfer(
            &t,
            Request {
                direction: Direction::Upload {
                    files: vec![file.clone()],
                    remote_dir: "/root".into(),
                },
                shell: run_as.clone(),
                password: Zeroizing::new(pw.clone()),
            },
        );
        assert!(r.is_ok(), "{run_as:?}: {r:?}");
        let got = dir.path().join(shell.replace(' ', "_"));
        let r = transfer(
            &t,
            Request {
                direction: Direction::Download {
                    remote: "/root/data.bin".into(),
                    local_dir: got.clone(),
                },
                shell: run_as.clone(),
                password: Zeroizing::new(pw.clone()),
            },
        );
        assert!(r.is_ok(), "{run_as:?}: {r:?}");
        assert_eq!(
            std::fs::read(got.join("data.bin")).unwrap(),
            data,
            "{run_as:?}"
        );
        let r = transfer(
            &t,
            Request {
                direction: Direction::Upload {
                    files: vec![file.clone()],
                    remote_dir: "/root".into(),
                },
                shell: run_as.clone(),
                password: Zeroizing::new("wrong-password".into()),
            },
        );
        assert!(r.is_err(), "{run_as:?} with a wrong password");
        eprintln!("{run_as:?} wrong password: {:?}", r.unwrap_err());
    }

    // The browser as root: asks for the password, then works in /root.
    let client = t
        .scp_browser(Some("sudo su -".into()), noop_waker())
        .unwrap();
    let seen = wait(&client, |e| matches!(e, SftpEvent::PasswordNeeded(_)));
    eprintln!("asked: {:?}", seen.last());
    client.set_password(password);
    let home = match wait(&client, |e| matches!(e, SftpEvent::Ready { .. })).pop() {
        Some(SftpEvent::Ready { home }) => home,
        e => panic!("{e:?}"),
    };
    assert_eq!(home, "/root");
    client.list(&home);
    let entries = match wait(&client, |e| matches!(e, SftpEvent::Listing { .. })).pop() {
        Some(SftpEvent::Listing { entries, .. }) => entries,
        e => panic!("{e:?}"),
    };
    let data = entries
        .iter()
        .find(|e| e.name == "data.bin")
        .expect("data.bin");
    assert_eq!(data.size, 300_000);
    client.mkdir("/root/przm new dir");
    wait(&client, |e| matches!(e, SftpEvent::Changed { .. }));
    client.rename("/root/przm new dir", "/root/przm-renamed");
    wait(&client, |e| matches!(e, SftpEvent::Changed { .. }));
    client.remove("/root/przm-renamed", true);
    wait(&client, |e| matches!(e, SftpEvent::Changed { .. }));
    client.remove("/root/data.bin", false);
    wait(&client, |e| matches!(e, SftpEvent::Changed { .. }));
    client.list("/nowhere");
    let seen = wait(&client, |e| matches!(e, SftpEvent::Failed { .. }));
    eprintln!("missing folder: {:?}", seen.last());
    let back = dir.path().join("browser");
    std::fs::create_dir_all(&back).unwrap();
    client.upload(7, local.join("sub/data.bin"), "/root");
    wait(&client, |e| matches!(e, SftpEvent::TransferDone { .. }));
    client.download(8, "/root/data.bin", back.join("data.bin"));
    let seen = wait(&client, |e| {
        matches!(e, SftpEvent::TransferDone { id: 8, .. })
    });
    assert!(
        matches!(
            seen.last(),
            Some(SftpEvent::TransferDone { result: Ok(()), .. })
        ),
        "{seen:?}"
    );
    assert_eq!(
        std::fs::read(back.join("data.bin")).unwrap(),
        data_bytes(&local)
    );
}

fn data_bytes(local: &std::path::Path) -> Vec<u8> {
    std::fs::read(local.join("sub/data.bin")).unwrap()
}

fn wait(client: &SftpClient, mut want: impl FnMut(&SftpEvent) -> bool) -> Vec<SftpEvent> {
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut seen = Vec::new();
    loop {
        for e in client.poll() {
            let stop = want(&e);
            seen.push(e);
            if stop {
                return seen;
            }
        }
        assert!(Instant::now() < deadline, "timed out; got {seen:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
