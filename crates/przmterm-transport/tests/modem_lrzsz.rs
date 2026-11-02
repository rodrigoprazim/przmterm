//! The transfer engines against lrzsz (`sz`, `rz`, `sb`, `rb`, `sx`, `rx`).
//! Skipped where lrzsz is not installed.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use przmterm_transport::modem::{self, Engine, Kind, Status};

fn have(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Runs `engine` against `command` (in `dir`) until both are done.
fn run(mut engine: Box<dyn Engine>, command: &[&str], dir: &Path) -> Status {
    let mut child = Command::new(command[0])
        .args(&command[1..])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let now = Instant::now();
        while let Ok(data) = rx.try_recv() {
            engine.input(&data, now);
        }
        engine.tick(now);
        let out = engine.output();
        if !out.is_empty() && stdin.write_all(&out).is_err() {
            break;
        }
        let _ = stdin.flush();
        if engine.status() != Status::Running {
            // Let the tool see the last bytes and finish.
            let _ = stdin.flush();
            std::thread::sleep(Duration::from_millis(200));
            while let Ok(data) = rx.try_recv() {
                engine.input(&data, now);
            }
            let out = engine.output();
            let _ = stdin.write_all(&out);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {:?}",
            engine.progress()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    drop(stdin);
    let _ = child.wait();
    engine.status()
}

fn sample(dir: &Path, name: &str, len: usize) -> Vec<u8> {
    let data: Vec<u8> = (0..len).map(|i| (i * 131 % 256) as u8).collect();
    std::fs::write(dir.join(name), &data).unwrap();
    data
}

#[test]
fn zmodem_both_ways_with_lrzsz() {
    if !have("sz") {
        eprintln!("lrzsz not installed; skipped");
        return;
    }
    let remote = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    // Download: the other side runs `sz`.
    let a = sample(remote.path(), "fw.bin", 700_000);
    let b = sample(remote.path(), "small.txt", 10);
    let rx = modem::receive(Kind::Zmodem, local.path().into(), "", Instant::now());
    let status = run(rx, &["sz", "-q", "fw.bin", "small.txt"], remote.path());
    assert!(
        matches!(status, Status::Finished(ref f) if f.len() == 2),
        "{status:?}"
    );
    assert_eq!(std::fs::read(local.path().join("fw.bin")).unwrap(), a);
    assert_eq!(std::fs::read(local.path().join("small.txt")).unwrap(), b);

    // Upload: the other side runs `rz`.
    let up = tempfile::tempdir().unwrap();
    let tx = modem::send(
        Kind::Zmodem,
        vec![local.path().join("fw.bin"), local.path().join("small.txt")],
        Instant::now(),
    )
    .unwrap();
    let status = run(tx, &["rz", "-q", "-y"], up.path());
    assert!(
        matches!(status, Status::Finished(ref f) if f.len() == 2),
        "{status:?}"
    );
    assert_eq!(std::fs::read(up.path().join("fw.bin")).unwrap(), a);
    assert_eq!(std::fs::read(up.path().join("small.txt")).unwrap(), b);
}

#[test]
fn ymodem_both_ways_with_lrzsz() {
    if !have("sb") {
        eprintln!("lrzsz not installed; skipped");
        return;
    }
    let remote = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let a = sample(remote.path(), "image.bin", 150_000);
    let b = sample(remote.path(), "b.cfg", 333);
    let rx = modem::receive(Kind::Ymodem, local.path().into(), "", Instant::now());
    let status = run(rx, &["sb", "-q", "image.bin", "b.cfg"], remote.path());
    assert!(
        matches!(status, Status::Finished(ref f) if f.len() == 2),
        "{status:?}"
    );
    assert_eq!(std::fs::read(local.path().join("image.bin")).unwrap(), a);
    assert_eq!(std::fs::read(local.path().join("b.cfg")).unwrap(), b);

    let up = tempfile::tempdir().unwrap();
    let tx = modem::send(
        Kind::Ymodem,
        vec![local.path().join("image.bin"), local.path().join("b.cfg")],
        Instant::now(),
    )
    .unwrap();
    let status = run(tx, &["rb", "-q", "-y"], up.path());
    assert!(matches!(status, Status::Finished(_)), "{status:?}");
    assert_eq!(std::fs::read(up.path().join("image.bin")).unwrap(), a);
    assert_eq!(std::fs::read(up.path().join("b.cfg")).unwrap(), b);
}

#[test]
fn xmodem_both_ways_with_lrzsz() {
    if !have("sx") {
        eprintln!("lrzsz not installed; skipped");
        return;
    }
    for (kind, flag) in [(Kind::Xmodem, None), (Kind::Xmodem1k, Some("-k"))] {
        let remote = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        // A length that is not a multiple of the block size.
        let a = sample(remote.path(), "boot.bin", 100_001);
        let rx = modem::receive(kind, local.path().into(), "boot.bin", Instant::now());
        let mut cmd = vec!["sx", "-q"];
        cmd.extend(flag);
        cmd.push("boot.bin");
        let status = run(rx, &cmd, remote.path());
        assert!(matches!(status, Status::Finished(_)), "{kind:?} {status:?}");
        let got = std::fs::read(local.path().join("boot.bin")).unwrap();
        // Xmodem pads the last block; only trailing SUBs may differ.
        assert_eq!(&got[..], &a[..], "{kind:?}");

        let up = tempfile::tempdir().unwrap();
        let tx = modem::send(kind, vec![local.path().join("boot.bin")], Instant::now()).unwrap();
        let status = run(tx, &["rx", "-q", "out.bin"], up.path());
        assert!(matches!(status, Status::Finished(_)), "{kind:?} {status:?}");
        let got = std::fs::read(up.path().join("out.bin")).unwrap();
        assert!(got.starts_with(&a), "{kind:?}");
    }
}
