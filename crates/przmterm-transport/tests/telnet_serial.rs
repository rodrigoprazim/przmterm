//! Telnet against a scripted TCP server, serial against a pseudo-terminal.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use przmterm_transport::telnet::{TelnetConfig, TelnetTransport};
use przmterm_transport::{Size, Transport, noop_waker};

const IAC: u8 = 255;
const DO: u8 = 253;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const BRK: u8 = 243;

/// Reads from the transport until `needle` shows up (or panics after 5 s).
fn wait_for(t: &mut dyn Transport, seen: &mut String, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 4096];
    while !seen.contains(needle) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; got {seen:?}"
        );
        let n = t.read(&mut buf).unwrap();
        if n == 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Reads from the server socket until `needle` arrives.
fn expect(stream: &mut impl Read, got: &mut Vec<u8>, needle: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 1024];
    while !contains(got, needle) {
        assert!(
            Instant::now() < deadline,
            "server never got {needle:?}; got {got:?}"
        );
        match stream.read(&mut buf) {
            Ok(0) => panic!("client hung up; got {got:?}"),
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn telnet_negotiates_and_carries_a_login() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (done_tx, done_rx) = mpsc::channel();

    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut got = Vec::new();
        // Like a router: ask for the window size and terminal type, echo.
        s.write_all(&[IAC, DO, 31, IAC, DO, 24, IAC, WILL, 1, IAC, WILL, 3])
            .unwrap();
        expect(&mut s, &mut got, &[IAC, SB, 31, 0, 80, 0, 24, IAC, SE]);
        s.write_all(&[IAC, SB, 24, 1, IAC, SE]).unwrap();
        let mut ttype = vec![IAC, SB, 24, 0];
        ttype.extend(b"vt100");
        ttype.extend([IAC, SE]);
        expect(&mut s, &mut got, &ttype);
        s.write_all(b"\r\nUser Access Verification\r\n\r\nUsername: ")
            .unwrap();
        expect(&mut s, &mut got, b"admin\r\0");
        expect(&mut s, &mut got, &[IAC, SB, 31, 0, 132, 0, 43, IAC, SE]);
        expect(&mut s, &mut got, &[IAC, BRK]);
        s.write_all(b"\r\nbye\r\n").unwrap();
        done_tx.send(()).unwrap();
        // Dropping the socket ends the session.
    });

    let mut config = TelnetConfig::new("127.0.0.1", port);
    config.term_type = "vt100".into();
    let mut t = TelnetTransport::connect(config, Size { cols: 80, rows: 24 }, noop_waker());
    let mut seen = String::new();
    wait_for(&mut t, &mut seen, "Username: ");
    assert!(
        !seen.contains('\u{fffd}'),
        "negotiation leaked into the terminal: {seen:?}"
    );
    t.write(b"admin\r").unwrap();
    t.resize(Size {
        cols: 132,
        rows: 43,
    })
    .unwrap();
    t.send_break().unwrap();
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    wait_for(&mut t, &mut seen, "closed.");
    assert!(seen.contains("bye"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !t.is_closed() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    server.join().unwrap();
}

#[test]
fn telnet_reports_a_refused_connection() {
    // Bind and drop to get a port nobody listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut t = TelnetTransport::connect(
        TelnetConfig::new("127.0.0.1", port),
        Size::default(),
        noop_waker(),
    );
    let mut seen = String::new();
    wait_for(&mut t, &mut seen, "Could not connect to 127.0.0.1");
}

#[cfg(unix)]
mod serial {
    use super::*;
    use przmterm_transport::serial::{SerialLine, SerialTransport};
    #[cfg(target_os = "linux")]
    use serialport::{SerialPort, TTYPort};

    // macOS sets custom baud rates with an ioctl that pseudo-terminals reject.
    #[cfg(target_os = "linux")]
    #[test]
    fn serial_console_round_trip() {
        let (mut cable, device) = TTYPort::pair().unwrap();
        let name = device.name().unwrap();
        drop(device);
        cable.set_timeout(Duration::from_millis(100)).unwrap();

        let mut line = SerialLine::new(&name);
        line.baud_rate = 115_200;
        let mut t = SerialTransport::open(line, noop_waker());
        let mut seen = String::new();
        wait_for(&mut t, &mut seen, "115200 8N1");

        cable.write_all(b"\r\nSwitch>").unwrap();
        wait_for(&mut t, &mut seen, "Switch>");
        t.write(b"enable\r").unwrap();
        let mut got = Vec::new();
        expect(&mut cable, &mut got, b"enable\r");

        t.close().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !t.is_closed() {
            assert!(Instant::now() < deadline, "serial transport did not close");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn serial_reports_a_missing_port() {
        let mut t = SerialTransport::open(SerialLine::new("/dev/przmterm-nope"), noop_waker());
        let mut seen = String::new();
        wait_for(&mut t, &mut seen, "Could not open /dev/przmterm-nope");
        wait_for(&mut t, &mut seen, "plugged in");
    }
}
