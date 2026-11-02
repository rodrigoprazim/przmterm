//! End-to-end tests of `SshTransport` against an in-process russh server.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::collections::HashMap;
use std::sync::Mutex;

use przmterm_transport::ssh::{
    Forward, ForwardKind, PasswordStore, SavePassword, SshConfig, SshTransport, Store,
};
use przmterm_transport::{Size, Transport, noop_waker};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};

#[derive(Clone)]
struct TestServer {
    methods: Vec<MethodKind>,
    client_key: Option<russh::keys::PublicKey>,
    /// Keyboard-interactive asks "Password:" then "Two-Factor Token:".
    two_step: bool,
    stage: u8,
    /// Session channels allowed per connection (0 = unlimited).
    max_channels: usize,
    channels: usize,
    /// Session channels; tunnel channels are not echoed.
    shells: Vec<ChannelId>,
    /// The X11 request: protocol, cookie (hex), screen.
    x11: Option<(String, String, u32)>,
}

impl TestServer {
    /// Rejects but keeps offering the same methods, like OpenSSH.
    fn retry(&self) -> Auth {
        Auth::Reject {
            proceed_with_methods: Some(MethodSet::from(&self.methods[..])),
            partial_success: false,
        }
    }
}

impl server::Handler for TestServer {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == "alice" && password == "secret" {
            Auth::Accept
        } else {
            self.retry()
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        let ok = user == "alice"
            && self
                .client_key
                .as_ref()
                .is_some_and(|k| k.key_data() == key.public_key.key_data());
        Ok(if ok { Auth::Accept } else { self.retry() })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        if self.two_step {
            let ask = |prompt: &'static str| Auth::Partial {
                name: "".into(),
                instructions: "".into(),
                prompts: vec![(prompt.into(), false)].into(),
            };
            let answer = response.and_then(|mut r| r.next());
            return Ok(match (self.stage, answer.as_deref()) {
                (_, None) => ask("Password: "),
                (0, Some(b"secret")) => {
                    self.stage = 1;
                    ask("Two-Factor Token: ")
                }
                (0, Some(_)) => ask("Password: "),
                (_, Some(b"123456")) => Auth::Accept,
                _ => self.retry(),
            });
        }
        Ok(match response {
            None => Auth::Partial {
                name: "".into(),
                instructions: "Two-factor login".into(),
                prompts: vec![("Verification code: ".into(), false)].into(),
            },
            Some(mut answers) => {
                if answers.next().as_deref() == Some(&b"123456"[..]) {
                    Auth::Accept
                } else {
                    self.retry()
                }
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.max_channels > 0 && self.channels >= self.max_channels {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
        } else {
            self.channels += 1;
            self.shells.push(_channel.id());
            reply.accept().await;
        }
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(
            channel,
            format!("pty {term} {cols}x{rows}\r\n").into_bytes(),
        )?;
        session.channel_success(channel)?;
        Ok(())
    }

    async fn x11_request(
        &mut self,
        channel: ChannelId,
        _single_connection: bool,
        protocol: &str,
        cookie: &str,
        screen: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.x11 = Some((protocol.into(), cookie.into(), screen));
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, b"welcome\r\n".to_vec())?;
        session.channel_success(channel)?;
        // Like `xclock`: connect back to the client's display with the
        // cookie it gave us, and report how that went.
        if let Some((protocol, cookie, _)) = self.x11.clone() {
            let handle = session.handle();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let report = |text: String| {
                    let handle = handle.clone();
                    async move {
                        let _ = handle.data(channel, text.into_bytes()).await;
                    }
                };
                let Ok(x) = handle.channel_open_x11("10.9.8.7", 4321).await else {
                    return report("x11 refused\r\n".into()).await;
                };
                let mut x = x.into_stream();
                let cookie: Vec<u8> = (0..cookie.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&cookie[i..i + 2], 16).unwrap())
                    .collect();
                let mut setup = vec![b'l', 0, 11, 0, 0, 0];
                setup.extend((protocol.len() as u16).to_le_bytes());
                setup.extend((cookie.len() as u16).to_le_bytes());
                setup.extend([0, 0]);
                setup.extend(protocol.as_bytes());
                setup.extend(std::iter::repeat_n(0, (4 - protocol.len() % 4) % 4));
                setup.extend(&cookie);
                x.write_all(&setup).await.unwrap();
                let mut reply = [0u8; 3];
                match x.read_exact(&mut reply).await {
                    Ok(_) => report(format!("x11 {}\r\n", String::from_utf8_lossy(&reply))).await,
                    Err(e) => report(format!("x11 failed {e}\r\n")).await,
                }
            });
        }
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if !self.shells.contains(&channel) {
            return Ok(());
        }
        if data == b"exit\r" {
            session.exit_status_request(channel, 3)?;
            session.close(channel)?;
        } else {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }

    /// Like OpenSSH's `-L` side: connect where the client asks.
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let port = u16::try_from(port_to_connect).unwrap_or(0);
        match tokio::net::TcpStream::connect((host_to_connect, port)).await {
            Ok(mut socket) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                });
            }
            Err(_) => {
                reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            }
        }
        Ok(())
    }

    /// Like OpenSSH's `-R` side: listen and hand connections to the client.
    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        let Ok(listener) =
            tokio::net::TcpListener::bind((address, u16::try_from(*port).unwrap_or(0))).await
        else {
            return Ok(false);
        };
        let bound = u32::from(listener.local_addr().unwrap().port());
        *port = bound;
        let (handle, address) = (session.handle(), address.to_string());
        tokio::spawn(async move {
            while let Ok((mut socket, peer)) = listener.accept().await {
                let handle = handle.clone();
                let address = address.clone();
                tokio::spawn(async move {
                    if let Ok(channel) = handle
                        .channel_open_forwarded_tcpip(
                            address,
                            bound,
                            peer.ip().to_string(),
                            u32::from(peer.port()),
                        )
                        .await
                    {
                        let mut stream = channel.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                    }
                });
            }
        });
        Ok(true)
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, format!("size {cols}x{rows}\r\n").into_bytes())?;
        Ok(())
    }
}

/// Starts a password-only server on a random local port.
fn start_server(key: PrivateKey, connections: usize) -> u16 {
    start_server_with(key, connections, &[MethodKind::Password], None)
}

/// Keyboard-interactive server asking for a password, then a token.
fn start_two_step_server(key: PrivateKey, connections: usize) -> u16 {
    start_server_full(
        key,
        connections,
        &[MethodKind::KeyboardInteractive],
        None,
        true,
        0,
    )
}

/// Starts a server on a random local port, accepting `connections` clients.
fn start_server_with(
    key: PrivateKey,
    connections: usize,
    methods: &[MethodKind],
    client_key: Option<russh::keys::PublicKey>,
) -> u16 {
    start_server_full(key, connections, methods, client_key, false, 0)
}

fn start_server_full(
    key: PrivateKey,
    connections: usize,
    methods: &[MethodKind],
    client_key: Option<russh::keys::PublicKey>,
    two_step: bool,
    max_channels: usize,
) -> u16 {
    start_server_with_algorithms(
        key,
        connections,
        methods,
        client_key,
        two_step,
        max_channels,
        russh::Preferred::default(),
    )
}

/// Like an old switch: SHA-1 key exchange, CBC cipher, HMAC-SHA1 only.
fn start_legacy_server(key: PrivateKey, connections: usize) -> u16 {
    use russh::{cipher, kex, mac};
    let preferred = russh::Preferred {
        kex: vec![kex::DH_G14_SHA1, kex::EXTENSION_SUPPORT_AS_SERVER].into(),
        cipher: vec![cipher::AES_128_CBC].into(),
        mac: vec![mac::HMAC_SHA1].into(),
        ..russh::Preferred::default()
    };
    start_server_with_algorithms(
        key,
        connections,
        &[MethodKind::Password],
        None,
        false,
        0,
        preferred,
    )
}

fn start_server_with_algorithms(
    key: PrivateKey,
    connections: usize,
    methods: &[MethodKind],
    client_key: Option<russh::keys::PublicKey>,
    two_step: bool,
    max_channels: usize,
    preferred: russh::Preferred,
) -> u16 {
    let methods = methods.to_vec();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let config = Arc::new(server::Config {
                keys: vec![key],
                methods: MethodSet::from(&methods[..]),
                auth_rejection_time: Duration::from_millis(10),
                auth_rejection_time_initial: Some(Duration::ZERO),
                preferred,
                ..Default::default()
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap().port()).unwrap();
            let server = TestServer {
                methods: methods.clone(),
                client_key,
                two_step,
                stage: 0,
                max_channels,
                channels: 0,
                shells: Vec::new(),
                x11: None,
            };
            for _ in 0..connections {
                let (socket, _) = listener.accept().await.unwrap();
                let config = config.clone();
                let server = server.clone();
                tokio::spawn(async move {
                    if let Ok(session) = server::run_stream(config, socket, server.clone()).await {
                        let _ = session.await;
                    }
                });
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
    });
    rx.recv().unwrap()
}

fn host_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

fn config(port: u16, known_hosts: &Path) -> SshConfig {
    let mut c = SshConfig::new("127.0.0.1", port, known_hosts);
    c.username = Some("alice".into());
    c.use_agent = false;
    c.use_default_identities = false;
    c.extra_known_hosts.clear();
    c
}

/// Collects terminal output until `needle` appears.
struct Screen {
    transport: SshTransport,
    text: String,
}

impl Screen {
    fn connect(config: SshConfig) -> Self {
        Self {
            transport: SshTransport::connect(config, Size { cols: 90, rows: 20 }, noop_waker()),
            text: String::new(),
        }
    }

    fn pump(&mut self) {
        let mut buf = [0u8; 4096];
        while let Ok(n) = self.transport.read(&mut buf) {
            if n == 0 {
                break;
            }
            self.text.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }

    fn wait_for(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.pump();
            if self.text.contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {needle:?}; got:\n{}", self.text);
    }

    fn wait_closed(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.transport.is_closed() {
            self.pump();
            assert!(Instant::now() < deadline, "not closed; got:\n{}", self.text);
            std::thread::sleep(Duration::from_millis(10));
        }
        self.pump();
    }

    fn send(&mut self, text: &str) {
        self.transport.write(text.as_bytes()).unwrap();
    }
}

#[test]
fn first_connection_asks_then_remembers_host_key() {
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    let port = start_server(host_key(), 2);

    // First time: unknown host key, wrong password once, then the shell.
    let mut s = Screen::connect(config(port, &known_hosts));
    s.wait_for("continue connecting (yes/no)? ");
    assert!(s.text.contains("ssh-ed25519 SHA256:"));
    s.send("maybe\r");
    s.wait_for("Please type 'yes' or 'no': ");
    s.send("yes\r");
    s.wait_for("alice@127.0.0.1's password: ");
    s.send("wrong\r");
    s.wait_for("Permission denied, please try again.");
    s.send("secret\r");
    s.wait_for("welcome");
    assert!(s.text.contains("pty xterm-256color 90x20"));
    assert!(!s.text.contains("wrong"), "passwords must not be echoed");

    s.send("hello");
    s.wait_for("hello");
    s.transport
        .resize(Size {
            cols: 120,
            rows: 40,
        })
        .unwrap();
    s.wait_for("size 120x40");

    s.send("exit\r");
    s.wait_closed();
    assert!(s.text.contains("closed (exit status 3)"), "{}", s.text);
    assert!(
        std::fs::read_to_string(&known_hosts)
            .unwrap()
            .trim_start()
            .starts_with(&format!("[127.0.0.1]:{port} ssh-ed25519 "))
    );

    // Second time: the key is known, no question.
    let mut s = Screen::connect(config(port, &known_hosts));
    s.wait_for("password: ");
    assert!(!s.text.contains("authenticity"));
    s.send("secret\r");
    s.wait_for("welcome");
    s.transport.close().unwrap();
    s.wait_closed();
}

#[test]
fn changed_host_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");

    let port = start_server(host_key(), 1);
    let mut s = Screen::connect(config(port, &known_hosts));
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    s.wait_for("password: ");
    s.transport.close().unwrap();
    s.wait_closed();

    // Same host:port, new key.
    let recorded = std::fs::read_to_string(&known_hosts).unwrap();
    let port2 = start_server(host_key(), 1);
    std::fs::write(
        &known_hosts,
        recorded.replace(&format!("]:{port} "), &format!("]:{port2} ")),
    )
    .unwrap();

    let mut s = Screen::connect(config(port2, &known_hosts));
    s.wait_closed();
    assert!(s.text.contains("REMOTE HOST IDENTIFICATION HAS CHANGED"));
    assert!(s.text.contains("Offered:  ssh-ed25519 SHA256:"));
    assert!(s.text.contains("Recorded: ssh-ed25519 SHA256:"));
    assert!(s.text.contains("Host key verification failed."));
    assert!(!s.text.contains("password"));
}

#[test]
fn ctrl_c_at_the_host_key_question_cancels() {
    let dir = tempfile::tempdir().unwrap();
    let port = start_server(host_key(), 1);
    let mut s = Screen::connect(config(port, &dir.path().join("known_hosts")));
    s.wait_for("(yes/no)? ");
    s.send("\x03");
    s.wait_closed();
    assert!(s.text.contains("Host key verification failed."));
    assert!(!dir.path().join("known_hosts").exists());
}

#[test]
fn connection_refused_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    // Bind and drop to get a port nobody listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut s = Screen::connect(config(port, &dir.path().join("known_hosts")));
    s.wait_closed();
    assert!(s.text.contains("Cannot connect"), "{}", s.text);
    assert_eq!(
        s.transport.ending(),
        Some(przmterm_transport::Ending::Unreachable)
    );
}

#[test]
fn three_wrong_passwords_end_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    let port = start_server(host_key(), 1);
    let mut s = Screen::connect(config(port, &known_hosts));
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    for _ in 0..3 {
        s.wait_for("password: ");
        s.text.clear();
        s.send("nope\r");
    }
    s.wait_closed();
    assert!(
        s.text.contains("Permission denied (password)."),
        "{}",
        s.text
    );
    assert_eq!(
        s.transport.ending(),
        Some(przmterm_transport::Ending::Failed),
        "a failed login is not worth retrying"
    );
}

#[test]
fn debug_shows_why_a_login_fails() {
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    let port = start_server(host_key(), 2);

    // Without debug, none of it shows.
    let mut quiet = Screen::connect(config(port, &known_hosts));
    quiet.wait_for("(yes/no)? ");
    assert!(!quiet.text.contains("debug:"), "{}", quiet.text);
    quiet.send("yes\r");
    quiet.wait_for("password: ");
    drop(quiet);

    let mut c = config(port, &known_hosts);
    c.debug = true;
    let mut s = Screen::connect(c);
    s.wait_for("password: ");
    for needle in [
        "debug: 127.0.0.1 resolves to 127.0.0.1",
        "debug: Connecting to 127.0.0.1 port",
        "debug: Server version: SSH-2.0-",
        "debug: Key exchange: ",
        "debug: Server host key: ssh-ed25519 SHA256:",
        "is recorded in",
        "debug: Authenticating to 127.0.0.1 as 'alice'",
        "server now allows: password",
        "debug: Server does not accept public keys",
    ] {
        assert!(
            s.text.contains(needle),
            "missing {needle:?} in:\n{}",
            s.text
        );
    }
    s.send("nope\r");
    s.wait_for("debug: Password (typed): rejected; server now allows: password");
}

/// A TCP echo server on a random port, as the far end of tunnels.
fn echo_server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut reader = stream.try_clone().unwrap();
                let mut writer = stream;
                let _ = std::io::copy(&mut reader, &mut writer);
            });
        }
    });
    port
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Sends `msg` over `stream` and expects it back.
fn round_trip(stream: &mut std::net::TcpStream, msg: &[u8]) {
    use std::io::{Read, Write};
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(msg).unwrap();
    let mut back = vec![0u8; msg.len()];
    stream.read_exact(&mut back).unwrap();
    assert_eq!(back, msg);
}

fn tunnel(kind: ForwardKind, listen: &str, target: &str) -> przmterm_transport::ssh::Tunnel {
    Forward {
        kind,
        listen: listen.into(),
        target: target.into(),
    }
    .tunnel()
    .unwrap()
}

/// Logs in with the password, accepting the host key.
fn log_in(s: &mut Screen) {
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    s.wait_for("password: ");
    s.send("secret\r");
    s.wait_for("welcome");
}

#[test]
fn local_remote_and_dynamic_tunnels_carry_data() {
    use std::io::{Read, Write};
    let dir = tempfile::tempdir().unwrap();
    let echo = echo_server();
    let port = start_server(host_key(), 1);
    let (local, socks, remote) = (free_port(), free_port(), free_port());
    let mut c = config(port, &dir.path().join("known_hosts"));
    c.forwards = vec![
        tunnel(
            ForwardKind::Local,
            &local.to_string(),
            &format!("127.0.0.1:{echo}"),
        ),
        tunnel(ForwardKind::Dynamic, &socks.to_string(), ""),
        tunnel(
            ForwardKind::Remote,
            &format!("127.0.0.1:{remote}"),
            &format!("127.0.0.1:{echo}"),
        ),
    ];
    let mut s = Screen::connect(c);
    log_in(&mut s);
    s.wait_for(&format!("Tunnel R 127.0.0.1:{remote}"));
    assert!(
        s.text
            .contains(&format!("Tunnel L 127.0.0.1:{local} → 127.0.0.1:{echo}"))
    );

    // -L: here → server → echo.
    let mut l = std::net::TcpStream::connect(("127.0.0.1", local)).unwrap();
    round_trip(&mut l, b"through the local tunnel");

    // -D: SOCKS 5 by name, then SOCKS 4a.
    let mut d = std::net::TcpStream::connect(("127.0.0.1", socks)).unwrap();
    d.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    d.write_all(&[5, 1, 0]).unwrap();
    let mut hello = [0u8; 2];
    d.read_exact(&mut hello).unwrap();
    assert_eq!(hello, [5, 0]);
    let name = b"localhost";
    let mut req = vec![5, 1, 0, 3, name.len() as u8];
    req.extend(name);
    req.extend(echo.to_be_bytes());
    d.write_all(&req).unwrap();
    let mut reply = [0u8; 10];
    d.read_exact(&mut reply).unwrap();
    assert_eq!(reply[1], 0, "SOCKS 5 connect succeeded");
    round_trip(&mut d, b"through socks5");

    let mut d4 = std::net::TcpStream::connect(("127.0.0.1", socks)).unwrap();
    d4.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut req = vec![4, 1];
    req.extend(echo.to_be_bytes());
    req.extend([0, 0, 0, 1, 0]);
    req.extend(b"127.0.0.1\0");
    d4.write_all(&req).unwrap();
    let mut reply = [0u8; 8];
    d4.read_exact(&mut reply).unwrap();
    assert_eq!(reply[1], 0x5a, "SOCKS 4a connect succeeded");
    round_trip(&mut d4, b"through socks4a");

    // -R: server port → back through us → echo.
    let mut r = std::net::TcpStream::connect(("127.0.0.1", remote)).unwrap();
    round_trip(&mut r, b"through the remote tunnel");

    // The tunnels end with the session.
    s.send("exit\r");
    s.wait_closed();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(("127.0.0.1", local)).is_ok() {
        assert!(Instant::now() < deadline, "local tunnel still listening");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_tunnel_on_a_busy_port_is_reported_and_the_session_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let port = start_server(host_key(), 1);
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let mut c = config(port, &dir.path().join("known_hosts"));
    c.forwards = vec![tunnel(
        ForwardKind::Local,
        &busy_port.to_string(),
        "127.0.0.1:1",
    )];
    let mut s = Screen::connect(c);
    log_in(&mut s);
    s.wait_for("cannot listen");
    s.send("still here");
    s.wait_for("still here");
}

#[test]
fn logs_in_through_a_jump_host() {
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    // The server is its own jump host: two connections.
    let port = start_server(host_key(), 2);
    let mut c = config(port, &known_hosts);
    let mut jump = config(port, &known_hosts);
    jump.username = Some("alice".into());
    c.jump = Some(Box::new(jump));
    let mut s = Screen::connect(c);
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    s.wait_for("password: ");
    s.send("secret\r");
    s.wait_for("through 127.0.0.1");
    // Wait for the target's own password prompt (the second one).
    let deadline = Instant::now() + Duration::from_secs(10);
    while s.text.matches("password: ").count() < 2 {
        assert!(Instant::now() < deadline, "no second prompt:\n{}", s.text);
        s.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    // The same host key, already learnt: no second question.
    s.send("secret\r");
    s.wait_for("welcome");
    s.send("hi");
    s.wait_for("hi");
}

#[test]
fn old_equipment_needs_legacy_algorithms_and_gets_a_badge() {
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    let port = start_legacy_server(host_key(), 2);

    // Without legacy algorithms: a clear message, and no retrying.
    let mut s = Screen::connect(config(port, &known_hosts));
    s.wait_closed();
    assert!(
        s.text
            .contains("only offers old key exchange algorithms (diffie-hellman-group14-sha1).")
            && s.text.contains("Legacy algorithms"),
        "{}",
        s.text
    );
    assert_eq!(
        s.transport.ending(),
        Some(przmterm_transport::Ending::Failed)
    );

    let mut c = config(port, &known_hosts);
    c.legacy_algorithms = true;
    let mut s = Screen::connect(c);
    log_in(&mut s);
    assert_eq!(
        s.transport.weak_algorithms(),
        ["diffie-hellman-group14-sha1", "aes128-cbc", "hmac-sha1"]
    );
}

#[test]
fn x11_connections_reach_the_local_display_with_the_real_cookie() {
    use przmterm_transport::x11::X11Display;
    use std::io::{Read, Write};
    let dir = tempfile::tempdir().unwrap();
    let port = start_server(host_key(), 1);
    // A fake local X server on display :N (TCP port 6000 + N).
    let (listener, display) = (50..150)
        .find_map(|n| {
            std::net::TcpListener::bind(("127.0.0.1", 6000 + n))
                .ok()
                .map(|l| (l, n))
        })
        .unwrap();
    let real = vec![0xab; 16];
    let expected = real.clone();
    let x_server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut setup = vec![0u8; 12 + 20 + 16];
        s.read_exact(&mut setup).unwrap();
        let ok = &setup[12..30] == b"MIT-MAGIC-COOKIE-1" && setup[32..] == expected[..];
        s.write_all(if ok { b"YES" } else { b"BAD" }).unwrap();
    });

    let mut c = config(port, &dir.path().join("known_hosts"));
    let mut x11 = X11Display::parse(&format!("127.0.0.1:{display}")).unwrap();
    x11.cookie = Some(real);
    c.x11 = Some(x11);
    let mut s = Screen::connect(c);
    log_in(&mut s);
    s.wait_for("x11 YES");
    x_server.join().unwrap();
}

#[test]
fn modern_servers_get_no_badge() {
    let dir = tempfile::tempdir().unwrap();
    let port = start_server(host_key(), 1);
    let mut c = config(port, &dir.path().join("known_hosts"));
    c.legacy_algorithms = true; // offered, but a modern server never picks them
    let mut s = Screen::connect(c);
    log_in(&mut s);
    assert!(s.transport.weak_algorithms().is_empty());
}

#[test]
fn keyboard_interactive_prompts_in_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let port = start_server_with(host_key(), 1, &[MethodKind::KeyboardInteractive], None);
    let mut s = Screen::connect(config(port, &dir.path().join("known_hosts")));
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    s.wait_for("Verification code: ");
    assert!(s.text.contains("Two-factor login"));
    s.send("123456\r");
    s.wait_for("welcome");
    assert!(!s.text.contains("123456"));
    s.transport.close().unwrap();
    s.wait_closed();
}

#[test]
fn asks_for_the_user_name_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let port = start_server(host_key(), 1);
    let mut c = config(port, &dir.path().join("known_hosts"));
    c.username = None;
    let mut s = Screen::connect(c);
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    s.wait_for("login as: ");
    s.send("alice\r");
    s.wait_for("alice@127.0.0.1's password: ");
    s.send("secret\r");
    s.wait_for("welcome");
    s.transport.close().unwrap();
    s.wait_closed();
}

#[test]
fn logs_in_with_an_identity_file() {
    let dir = tempfile::tempdir().unwrap();
    let client_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let key_path = dir.path().join("id_test");
    std::fs::write(
        &key_path,
        client_key
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    let port = start_server_with(
        host_key(),
        1,
        &[MethodKind::PublicKey, MethodKind::Password],
        Some(client_key.public_key().clone()),
    );
    let mut c = config(port, &dir.path().join("known_hosts"));
    c.identity_files = vec![key_path];
    let mut s = Screen::connect(c);
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    s.wait_for("welcome");
    assert!(!s.text.contains("password"), "{}", s.text);
    s.transport.close().unwrap();
    s.wait_closed();
}

#[derive(Default)]
struct MemoryStore(Mutex<HashMap<String, String>>);

impl PasswordStore for MemoryStore {
    fn get(&self, account: &str) -> Option<String> {
        self.0.lock().unwrap().get(account).cloned()
    }
    fn set(&self, account: &str, password: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .insert(account.into(), password.into());
        Ok(())
    }
    fn delete(&self, account: &str) {
        self.0.lock().unwrap().remove(account);
    }
}

#[test]
fn saved_password_is_offered_used_and_dropped_when_wrong() {
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    let port = start_server(host_key(), 3);
    let store = Arc::new(MemoryStore::default());
    let account = format!("alice@127.0.0.1:{port}");
    let with_store = |mode| {
        let mut c = config(port, &known_hosts);
        c.password_store = Some(Store(store.clone()));
        c.save_password = mode;
        c
    };

    // 1. Typed password works; asked to save; say yes.
    let mut s = Screen::connect(with_store(SavePassword::Ask));
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    s.wait_for("password: ");
    s.send("secret\r");
    s.wait_for("Save this password in the system keychain? (yes/no) ");
    s.send("yes\r");
    s.wait_for("welcome");
    assert_eq!(store.get(&account).as_deref(), Some("secret"));
    s.transport.close().unwrap();
    s.wait_closed();

    // 2. Saved password logs in without a prompt.
    let mut s = Screen::connect(with_store(SavePassword::Ask));
    s.wait_for("welcome");
    assert!(s.text.contains("Using the password saved in the keychain."));
    assert!(!s.text.contains("password: "));
    s.transport.close().unwrap();
    s.wait_closed();

    // 3. A stale saved password is rejected, forgotten, and the user is asked.
    store.set(&account, "old").unwrap();
    let mut s = Screen::connect(with_store(SavePassword::Never));
    s.wait_for("The saved password was rejected.");
    s.wait_for("password: ");
    assert_eq!(store.get(&account), None);
    s.send("secret\r");
    s.wait_for("welcome");
    assert!(!s.text.contains("Save this password"));
    assert_eq!(store.get(&account), None);
    s.transport.close().unwrap();
    s.wait_closed();
}

#[test]
fn keyboard_interactive_password_prompt_uses_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let port = start_server_with(host_key(), 1, &[MethodKind::KeyboardInteractive], None);
    let store = Arc::new(MemoryStore::default());
    store
        .set(&format!("alice@127.0.0.1:{port}"), "123456")
        .unwrap();
    let mut c = config(port, &dir.path().join("known_hosts"));
    c.password_store = Some(Store(store.clone()));
    let mut s = Screen::connect(c);
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    // The test server's prompt is "Verification code:", not a password: no store use.
    s.wait_for("Verification code: ");
    assert!(!s.text.contains("saved in the keychain"));
    s.send("123456\r");
    s.wait_for("welcome");
    s.transport.close().unwrap();
    s.wait_closed();
}

#[test]
fn duplicate_reuses_the_authenticated_connection() {
    let dir = tempfile::tempdir().unwrap();
    // One TCP connection only: a second login would hang, not succeed.
    let port = start_server_with(host_key(), 1, &[MethodKind::KeyboardInteractive], None);
    let mut first = Screen::connect(config(port, &dir.path().join("known_hosts")));
    assert!(
        first
            .transport
            .share_connection(Size { cols: 80, rows: 24 }, noop_waker())
            .is_none(),
        "nothing to share before login"
    );
    first.wait_for("(yes/no)? ");
    first.send("yes\r");
    first.wait_for("Verification code: ");
    first.send("123456\r");
    first.wait_for("welcome");

    let mut second = Screen {
        transport: first
            .transport
            .share_connection(Size { cols: 70, rows: 20 }, noop_waker())
            .expect("connection is shared"),
        text: String::new(),
    };
    second.wait_for("welcome");
    assert!(second.text.contains("no new login"));
    assert!(second.text.contains("pty xterm-256color 70x20"));
    assert!(!second.text.contains("Verification code"));

    // Closing the first pane keeps the second one alive.
    first.transport.close().unwrap();
    first.wait_closed();
    second.send("still here");
    second.wait_for("still here");

    second.send("exit\r");
    second.wait_closed();
    assert!(second.text.contains("exit status 3"));
}

#[test]
fn password_then_token_saves_and_reuses_the_password() {
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    let port = start_two_step_server(host_key(), 3);
    let store = Arc::new(MemoryStore::default());
    let account = format!("alice@127.0.0.1:{port}");
    let with_store = |mode| {
        let mut c = config(port, &known_hosts);
        c.password_store = Some(Store(store.clone()));
        c.save_password = mode;
        c
    };

    // 1. Password, then token; the password is offered for saving.
    let mut s = Screen::connect(with_store(SavePassword::Ask));
    s.wait_for("(yes/no)? ");
    s.send("yes\r");
    s.wait_for("Password: ");
    s.send("secret\r");
    s.wait_for("Two-Factor Token: ");
    s.send("123456\r");
    s.wait_for("Save this password in the system keychain? (yes/no) ");
    s.send("yes\r");
    s.wait_for("welcome");
    assert_eq!(store.get(&account).as_deref(), Some("secret"));
    s.transport.close().unwrap();
    s.wait_closed();

    // 2. The saved password answers "Password:"; only the token is asked,
    //    and the token round does not count as a rejection.
    let mut s = Screen::connect(with_store(SavePassword::Ask));
    s.wait_for("Two-Factor Token: ");
    assert!(s.text.contains("Using the password saved in the keychain."));
    assert!(!s.text.contains("rejected"));
    s.send("123456\r");
    s.wait_for("welcome");
    assert_eq!(store.get(&account).as_deref(), Some("secret"));
    s.transport.close().unwrap();
    s.wait_closed();

    // 3. A stale password is detected when "Password:" comes back, then forgotten.
    store.set(&account, "old").unwrap();
    let mut s = Screen::connect(with_store(SavePassword::Always));
    s.wait_for("The saved password was rejected.");
    s.wait_for("Password: ");
    s.send("secret\r");
    s.wait_for("Two-Factor Token: ");
    s.send("123456\r");
    s.wait_for("welcome");
    assert!(
        !s.text.contains("Save this password"),
        "always saves without asking"
    );
    assert_eq!(store.get(&account).as_deref(), Some("secret"));
    s.transport.close().unwrap();
    s.wait_closed();
}

#[test]
fn refused_second_channel_falls_back_to_a_new_login() {
    let dir = tempfile::tempdir().unwrap();
    // One session per connection, like some bastions; two connections allowed.
    let port = start_server_full(
        host_key(),
        2,
        &[MethodKind::KeyboardInteractive],
        None,
        false,
        1,
    );
    let mut first = Screen::connect(config(port, &dir.path().join("known_hosts")));
    first.wait_for("(yes/no)? ");
    first.send("yes\r");
    first.wait_for("Verification code: ");
    first.send("123456\r");
    first.wait_for("welcome");

    let mut second = Screen {
        transport: first
            .transport
            .share_connection(Size { cols: 80, rows: 24 }, noop_waker())
            .unwrap(),
        text: String::new(),
    };
    second.wait_for("does not allow another session on the same connection");
    second.wait_for("Verification code: ");
    second.send("123456\r");
    second.wait_for("welcome");
    first.send("first still works");
    first.wait_for("first still works");
    first.transport.close().unwrap();
    second.transport.close().unwrap();
    first.wait_closed();
    second.wait_closed();
}
