//! End-to-end tests of `SshTransport` against an in-process russh server.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::collections::HashMap;
use std::sync::Mutex;

use przmterm_transport::ssh::{PasswordStore, SavePassword, SshConfig, SshTransport, Store};
use przmterm_transport::{Size, Transport, noop_waker};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};

#[derive(Clone)]
struct TestServer {
    methods: Vec<MethodKind>,
    client_key: Option<russh::keys::PublicKey>,
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
        reply.accept().await;
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
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, b"welcome\r\n".to_vec())?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if data == b"exit\r" {
            session.exit_status_request(channel, 3)?;
            session.close(channel)?;
        } else {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
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

/// Starts a server on a random local port, accepting `connections` clients.
fn start_server_with(
    key: PrivateKey,
    connections: usize,
    methods: &[MethodKind],
    client_key: Option<russh::keys::PublicKey>,
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
                ..Default::default()
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap().port()).unwrap();
            let server = TestServer {
                methods: methods.clone(),
                client_key,
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
