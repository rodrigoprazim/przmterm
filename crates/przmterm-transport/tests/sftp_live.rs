//! SFTP against a real OpenSSH server. Ignored by default; run with
//! `PRZM_SFTP=user:password@host:port cargo test -p przmterm-transport
//! --test sftp_live -- --ignored`.

use std::time::{Duration, Instant};

use przmterm_transport::sftp::{SftpClient, SftpEvent, join};
use przmterm_transport::ssh::{Credential, SshConfig, SshTransport};
use przmterm_transport::{Size, Transport, noop_waker};

fn wait(client: &SftpClient, mut want: impl FnMut(&SftpEvent) -> bool) -> Vec<SftpEvent> {
    let deadline = Instant::now() + Duration::from_secs(20);
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

#[test]
#[ignore = "needs a real SSH server (PRZM_SFTP)"]
fn browse_upload_download_rename_delete() {
    let spec = std::env::var("PRZM_SFTP").expect("PRZM_SFTP=user:password@host:port");
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
    // Accept the host key, then wait for the login.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut text = String::new();
    let client = loop {
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
        if let Some(c) = t.sftp(noop_waker()) {
            break c;
        }
        assert!(Instant::now() < deadline, "no login: {text}");
        std::thread::sleep(Duration::from_millis(50));
    };
    let home = match wait(&client, |e| matches!(e, SftpEvent::Ready { .. })).pop() {
        Some(SftpEvent::Ready { home }) => home,
        e => panic!("{e:?}"),
    };

    // Upload a folder with a file, see it, download it back.
    let local = dir.path().join("przm-sftp-test");
    std::fs::create_dir_all(local.join("sub")).unwrap();
    std::fs::write(local.join("sub/data.bin"), vec![7u8; 300_000]).unwrap();
    client.upload(1, local.clone(), &home);
    let seen = wait(&client, |e| matches!(e, SftpEvent::TransferDone { .. }));
    assert!(
        matches!(
            seen.last(),
            Some(SftpEvent::TransferDone {
                id: 1,
                result: Ok(())
            })
        ),
        "{seen:?}"
    );
    let remote = join(&home, "przm-sftp-test");
    client.list(&join(&remote, "sub"));
    let seen = wait(&client, |e| matches!(e, SftpEvent::Listing { .. }));
    match seen.last() {
        Some(SftpEvent::Listing { entries, .. }) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].name, "data.bin");
            assert_eq!(entries[0].size, 300_000);
        }
        e => panic!("{e:?}"),
    }
    let back = dir.path().join("back");
    client.download(2, &remote, back.clone());
    let seen = wait(&client, |e| matches!(e, SftpEvent::TransferDone { .. }));
    assert!(
        matches!(
            seen.last(),
            Some(SftpEvent::TransferDone {
                id: 2,
                result: Ok(())
            })
        ),
        "{seen:?}"
    );
    assert_eq!(
        std::fs::read(back.join("sub/data.bin")).unwrap().len(),
        300_000
    );

    // Rename, then delete the whole folder.
    let renamed = join(&home, "przm-sftp-renamed");
    client.rename(&remote, &renamed);
    wait(&client, |e| matches!(e, SftpEvent::Changed { .. }));
    client.remove(&renamed, true);
    let seen = wait(&client, |e| {
        matches!(e, SftpEvent::Changed { .. } | SftpEvent::Failed { .. })
    });
    assert!(
        matches!(seen.last(), Some(SftpEvent::Changed { .. })),
        "{seen:?}"
    );
    client.list(&home);
    let seen = wait(&client, |e| matches!(e, SftpEvent::Listing { .. }));
    if let Some(SftpEvent::Listing { entries, .. }) = seen.last() {
        assert!(!entries.iter().any(|e| e.name.starts_with("przm-sftp")));
    }
    // A folder that is not there.
    client.list("/no/such/folder");
    let seen = wait(&client, |e| matches!(e, SftpEvent::Failed { .. }));
    assert!(matches!(seen.last(), Some(SftpEvent::Failed { .. })));
}
