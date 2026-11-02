//! SSH transport on `russh`.
//!
//! Each connection runs as a task on a shared Tokio runtime. The GUI side
//! ([`SshTransport`]) talks to it through two channels: commands in (bytes,
//! resize, close) and terminal bytes out.
//!
//! Everything interactive before the shell starts — the host key question,
//! `login as:`, passwords, key passphrases, keyboard-interactive prompts — is
//! written into the terminal and answered by typing, like OpenSSH or PuTTY.
//! That keeps the transport usable from the GUI, the CLI and scripts alike.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{self, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::{ChannelMsg, Disconnect, MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};

use crate::pipe::{Closed, Cmd, Output, Pipe, runtime};
use crate::x11::{X11Display, X11Forward};
use crate::{Ending, Protocol, Size, Transport, TransportError, Waker};
pub use przmterm_core::forward::{Forward, ForwardKind, HostPort, Tunnel};

/// Default key files tried after the agent, in this order (like OpenSSH).
const DEFAULT_IDENTITIES: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];
const PASSWORD_ATTEMPTS: usize = 3;

/// Saved-password storage, such as the OS keychain. Accounts are
/// `user@host:port`. Calls may block; they run off the network threads.
pub trait PasswordStore: Send + Sync {
    fn get(&self, account: &str) -> Option<String>;
    fn set(&self, account: &str, password: &str) -> Result<(), String>;
    fn delete(&self, account: &str);
}

/// A shared [`PasswordStore`].
#[derive(Clone)]
pub struct Store(pub Arc<dyn PasswordStore>);

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

/// Whether a password that worked is saved in the [`PasswordStore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SavePassword {
    /// Ask in the terminal after a successful login.
    #[default]
    Ask,
    Always,
    Never,
}

/// How to reach and authenticate to an SSH server.
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    /// Asked for in the terminal (`login as:`) when `None`.
    pub username: Option<String>,
    pub term_type: String,
    /// Key files tried before the default ones.
    pub identity_files: Vec<PathBuf>,
    /// Also try `~/.ssh/id_ed25519`, `id_ecdsa` and `id_rsa`.
    pub use_default_identities: bool,
    /// Try keys held by ssh-agent (Unix) or Pageant / the OpenSSH agent (Windows).
    pub use_agent: bool,
    /// PrzmTerm's own known-hosts file, in OpenSSH format. New keys are added here.
    pub known_hosts: PathBuf,
    /// Further known-hosts files consulted read-only, e.g. `~/.ssh/known_hosts`.
    pub extra_known_hosts: Vec<PathBuf>,
    pub connect_timeout: Duration,
    pub keepalive: Option<Duration>,
    /// Where saved passwords live; `None` disables saving.
    pub password_store: Option<Store>,
    pub save_password: SavePassword,
    /// Show every step of the connection and login in the terminal, like
    /// `ssh -v`, to find out why a login fails.
    pub debug: bool,
    /// Connect through this server first (ProxyJump). Jump hosts can have
    /// their own jump host.
    pub jump: Option<Box<SshConfig>>,
    /// Tunnels opened once logged in.
    pub forwards: Vec<Tunnel>,
    /// Also offer old algorithms (SHA-1 key exchange, CBC and 3DES ciphers,
    /// HMAC-SHA1, DSA host keys) for equipment that knows nothing newer.
    pub legacy_algorithms: bool,
    /// Forward X11 to this local display (`ssh -X`).
    pub x11: Option<X11Display>,
    /// Old algorithms the connection ended up using, for the "legacy" badge.
    pub(crate) weak: WeakAlgorithms,
}

/// Names of the old algorithms a connection negotiated.
pub(crate) type WeakAlgorithms = Arc<std::sync::Mutex<Vec<String>>>;

/// Algorithms offered only with [`SshConfig::legacy_algorithms`], or used by
/// default but weak (`ssh-rsa` signs with SHA-1): shown in the badge.
pub const LEGACY_ALGORITHMS: [&str; 12] = [
    "diffie-hellman-group1-sha1",
    "diffie-hellman-group14-sha1",
    "diffie-hellman-group-exchange-sha1",
    "aes128-cbc",
    "aes192-cbc",
    "aes256-cbc",
    "3des-cbc",
    "hmac-sha1",
    "hmac-sha1-etm@openssh.com",
    "ssh-rsa",
    "ssh-dss",
    "none",
];

/// The algorithms offered: russh's safe defaults plus the NIST ECDH key
/// exchanges (common on network equipment, and in OpenSSH's defaults), and
/// with `legacy` the old ones after them, so a modern server never picks them.
fn preferred(legacy: bool) -> russh::Preferred {
    use russh::{cipher, kex, mac};
    let base = russh::Preferred::DEFAULT;
    let (mut kexes, extensions): (Vec<kex::Name>, Vec<kex::Name>) =
        base.kex.iter().partition(|k| {
            !k.as_ref().starts_with("ext-info") && !k.as_ref().starts_with("kex-strict")
        });
    kexes.extend([
        kex::ECDH_SHA2_NISTP256,
        kex::ECDH_SHA2_NISTP384,
        kex::ECDH_SHA2_NISTP521,
    ]);
    let mut ciphers = base.cipher.to_vec();
    let mut macs = base.mac.to_vec();
    let mut keys = base.key.to_vec();
    if legacy {
        kexes.extend([kex::DH_G14_SHA1, kex::DH_GEX_SHA1, kex::DH_G1_SHA1]);
        ciphers.extend([
            cipher::AES_128_CBC,
            cipher::AES_192_CBC,
            cipher::AES_256_CBC,
            cipher::TRIPLE_DES_CBC,
        ]);
        macs.extend([mac::HMAC_SHA1_ETM, mac::HMAC_SHA1]);
        keys.push(keys::Algorithm::Dsa);
    }
    kexes.extend(extensions);
    russh::Preferred {
        kex: kexes.into(),
        cipher: ciphers.into(),
        mac: macs.into(),
        key: keys.into(),
        ..base
    }
}

/// Explains a failed algorithm negotiation: whether "legacy algorithms"
/// would help, or the server offers nothing PrzmTerm knows.
fn no_common_algorithm(kind: &russh::AlgorithmKind, theirs: &[String], legacy: bool) -> String {
    let what = match kind {
        russh::AlgorithmKind::Kex => "key exchange",
        russh::AlgorithmKind::Key => "host key",
        russh::AlgorithmKind::Cipher => "cipher",
        russh::AlgorithmKind::Mac => "MAC",
        russh::AlgorithmKind::Compression => "compression",
    };
    // Protocol extensions travel in the key exchange list; they are not algorithms.
    let theirs: Vec<String> = theirs
        .iter()
        .filter(|t| !t.starts_with("ext-info") && !t.starts_with("kex-strict"))
        .cloned()
        .collect();
    let offered = theirs.join(", ");
    let ours = preferred(true);
    let known: Vec<String> = ours
        .kex
        .iter()
        .map(|k| k.as_ref().to_string())
        .chain(ours.cipher.iter().map(|c| c.as_ref().to_string()))
        .chain(ours.mac.iter().map(|m| m.as_ref().to_string()))
        .chain(ours.key.iter().map(|k| k.as_str().to_string()))
        .collect();
    if !legacy && theirs.iter().any(|t| known.contains(t)) {
        format!(
            "The server only offers old {what} algorithms ({offered}). Turn on \
             \"Legacy algorithms\" in the session settings to connect."
        )
    } else {
        format!("No {what} algorithm in common with the server, which offers: {offered}.")
    }
}

impl SshConfig {
    pub fn new(host: impl Into<String>, port: u16, known_hosts: impl Into<PathBuf>) -> Self {
        let openssh_known_hosts = dirs::home_dir()
            .map(|h| h.join(".ssh").join("known_hosts"))
            .into_iter()
            .collect();
        Self {
            host: host.into(),
            port,
            username: None,
            term_type: "xterm-256color".into(),
            identity_files: Vec::new(),
            use_default_identities: true,
            use_agent: true,
            known_hosts: known_hosts.into(),
            extra_known_hosts: openssh_known_hosts,
            connect_timeout: Duration::from_secs(15),
            keepalive: Some(Duration::from_secs(30)),
            password_store: None,
            save_password: SavePassword::Ask,
            debug: false,
            jump: None,
            forwards: Vec::new(),
            legacy_algorithms: false,
            x11: None,
            weak: WeakAlgorithms::default(),
        }
    }
}

/// The authenticated connection, once there is one. Held weakly so that the
/// connection ends when its last channel does.
type Shared = Arc<std::sync::Mutex<Option<std::sync::Weak<Handle>>>>;

/// An SSH session seen from the GUI: non-blocking reads and writes.
pub struct SshTransport {
    pipe: Pipe,
    shared: Shared,
    config: SshConfig,
}

impl SshTransport {
    fn spawn(
        size: Size,
        waker: Waker,
        shared: Shared,
        config: SshConfig,
        task: impl FnOnce(Arc<Mutex<Input>>, Output, Closed) -> TaskFuture,
    ) -> Self {
        let (pipe, end) = Pipe::new(waker);
        let input = Arc::new(Mutex::new(Input { rx: end.cmds, size }));
        runtime().spawn(task(input, end.out, end.closed));
        Self {
            pipe,
            shared,
            config,
        }
    }

    /// Starts connecting in the background and returns immediately.
    /// `waker` is called whenever new output is available.
    pub fn connect(config: SshConfig, size: Size, waker: Waker) -> Self {
        let shared: Shared = Arc::default();
        let (shared2, config2) = (shared.clone(), config.clone());
        Self::spawn(size, waker, shared, config, move |input, out, closed| {
            Box::pin(session_task(config2, input, out, closed, shared2))
        })
    }

    /// Opens another shell on this transport's connection, without logging
    /// in again (like OpenSSH connection sharing). `None` if the connection
    /// is not authenticated yet or has ended. If the server refuses a second
    /// session on one connection, the new transport logs in on its own.
    pub fn share_connection(&self, size: Size, waker: Waker) -> Option<SshTransport> {
        let handle = self.shared.lock().ok()?.as_ref()?.upgrade()?;
        if handle.is_closed() {
            return None;
        }
        let (shared, config) = (self.shared.clone(), self.config.clone());
        Some(Self::spawn(
            size,
            waker,
            self.shared.clone(),
            self.config.clone(),
            move |input, out, closed| {
                Box::pin(channel_task(handle, config, input, out, closed, shared))
            },
        ))
    }
}

type TaskFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

impl Transport for SshTransport {
    fn protocol(&self) -> Protocol {
        Protocol::Ssh
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        self.pipe.read(buf)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.pipe.send(Cmd::Data(data.to_vec()))
    }

    fn resize(&mut self, size: Size) -> Result<(), TransportError> {
        self.pipe.send(Cmd::Resize(size))
    }

    fn close(&mut self) -> Result<(), TransportError> {
        let _ = self.pipe.send(Cmd::Close);
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.pipe.is_closed()
    }

    fn ending(&self) -> Option<Ending> {
        self.pipe.ending()
    }

    fn weak_algorithms(&self) -> Vec<String> {
        self.config
            .weak
            .lock()
            .map(|w| w.clone())
            .unwrap_or_default()
    }

    fn duplicate(&self, size: Size, waker: Waker) -> Option<Box<dyn Transport>> {
        self.share_connection(size, waker)
            .map(|t| Box::new(t) as Box<dyn Transport>)
    }
}

/// User-bound side of the task: keystrokes and resizes from the GUI.
struct Input {
    rx: mpsc::UnboundedReceiver<Cmd>,
    size: Size,
}

/// The user cancelled (Ctrl+C) or closed the session while we were asking.
#[derive(Debug)]
struct Cancelled;

impl Input {
    /// Reads one line typed in the terminal. With `echo == false` nothing is
    /// shown (passwords). Escape sequences such as arrow keys are ignored.
    async fn read_line(&mut self, out: &Output, echo: bool) -> Result<String, Cancelled> {
        let mut line: Vec<u8> = Vec::new();
        let mut escape = Escape::None;
        loop {
            let bytes = match self.rx.recv().await {
                Some(Cmd::Data(bytes)) => bytes,
                Some(Cmd::Resize(size)) => {
                    self.size = size;
                    continue;
                }
                Some(Cmd::Break) => continue,
                Some(Cmd::Close) | None => return Err(Cancelled),
            };
            for b in bytes {
                match escape {
                    Escape::Started => {
                        escape = if matches!(b, b'[' | b'O') {
                            Escape::Sequence
                        } else {
                            Escape::None
                        };
                        continue;
                    }
                    Escape::Sequence => {
                        if (0x40..=0x7e).contains(&b) {
                            escape = Escape::None;
                        }
                        continue;
                    }
                    Escape::None => {}
                }
                match b {
                    0x1b => escape = Escape::Started,
                    b'\r' | b'\n' => {
                        out.text("\r\n");
                        return Ok(String::from_utf8_lossy(&line).into_owned());
                    }
                    0x7f | 0x08 => {
                        if pop_char(&mut line) && echo {
                            out.text("\x08 \x08");
                        }
                    }
                    0x15 => {
                        // Ctrl+U: erase the whole line.
                        while pop_char(&mut line) {
                            if echo {
                                out.text("\x08 \x08");
                            }
                        }
                    }
                    0x03 => {
                        out.text("^C\r\n");
                        return Err(Cancelled);
                    }
                    0x04 if line.is_empty() => {
                        out.text("\r\n");
                        return Err(Cancelled);
                    }
                    b if b >= 0x20 => {
                        line.push(b);
                        if echo {
                            out.bytes(&[b]);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Escape {
    None,
    Started,
    Sequence,
}

/// Removes the last UTF-8 character. Returns false if the line was empty.
fn pop_char(line: &mut Vec<u8>) -> bool {
    while let Some(b) = line.pop() {
        if b & 0xC0 != 0x80 {
            return true;
        }
    }
    false
}

/// Result of looking a host key up in the known-hosts files.
#[derive(Debug, PartialEq, Eq)]
pub enum HostKeyStatus {
    Known,
    Unknown,
    /// A different key of the same type is recorded at `path`, line `line`.
    Changed {
        path: PathBuf,
        line: usize,
    },
}

/// Checks `key` for `host:port` in `own`, then in each of `extra`.
pub fn check_host_key(
    host: &str,
    port: u16,
    key: &PublicKey,
    own: &Path,
    extra: &[PathBuf],
) -> HostKeyStatus {
    lookup_host_key(host, port, key, own, extra).0
}

/// [`check_host_key`], plus the file where the key was found.
fn lookup_host_key(
    host: &str,
    port: u16,
    key: &PublicKey,
    own: &Path,
    extra: &[PathBuf],
) -> (HostKeyStatus, Option<PathBuf>) {
    for path in std::iter::once(own).chain(extra.iter().map(PathBuf::as_path)) {
        match keys::check_known_hosts_path(host, port, key, path) {
            Ok(true) => return (HostKeyStatus::Known, Some(path.to_path_buf())),
            Err(keys::Error::KeyChanged { line }) => {
                let path = path.to_path_buf();
                return (
                    HostKeyStatus::Changed {
                        path: path.clone(),
                        line,
                    },
                    Some(path),
                );
            }
            _ => {}
        }
    }
    (HostKeyStatus::Unknown, None)
}

/// `publickey,password` for the debug output.
fn method_list(methods: &MethodSet) -> String {
    let names: Vec<&str> = methods.iter().map(<&str>::from).collect();
    if names.is_empty() {
        "none".into()
    } else {
        names.join(",")
    }
}

/// Converts an authentication result, describing it when debugging.
fn step(out: &Output, what: &str, result: client::AuthResult) -> Step {
    match result {
        client::AuthResult::Success => {
            out.debug(&format!("{what}: accepted"));
            Step::Done
        }
        client::AuthResult::Failure {
            remaining_methods,
            partial_success,
        } => {
            out.debug(&format!(
                "{what}: {}; server now allows: {}",
                if partial_success {
                    "partial success, more is needed (two-factor)"
                } else {
                    "rejected"
                },
                method_list(&remaining_methods)
            ));
            Step::Next(remaining_methods)
        }
    }
}

fn fingerprint(key: &PublicKey) -> String {
    format!(
        "{} {}",
        key.algorithm().as_str(),
        key.fingerprint(HashAlg::Sha256)
    )
}

pub(crate) struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    extra_known_hosts: Vec<PathBuf>,
    input: Arc<Mutex<Input>>,
    out: Output,
    /// Where connections to our remote forwards go.
    remote: RemoteTargets,
    /// Where to record old algorithms in use (the session's own connection,
    /// not its jump hosts).
    weak: Option<WeakAlgorithms>,
    /// X11 forwarding, when on.
    x11: Option<Arc<X11Forward>>,
    /// The jump host this connection runs through, kept alive with it.
    _jump: Option<Arc<Handle>>,
}

/// Remote forwards by the port the server listens on, with their targets.
pub(crate) type RemoteTargets = Arc<std::sync::Mutex<Vec<(u32, HostPort)>>>;

impl ClientHandler {
    fn warn_changed(&self, offered: &PublicKey, path: &Path, line: usize) {
        let recorded = keys::known_hosts::known_host_keys_path(&self.host, self.port, path)
            .ok()
            .and_then(|ks| ks.into_iter().find(|(l, _)| *l == line))
            .map(|(_, k)| fingerprint(&k))
            .unwrap_or_else(|| "unknown".into());
        let bar = "@".repeat(59);
        self.out.error(&bar);
        self.out
            .error("@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @");
        self.out.error(&bar);
        self.out.lines(&format!(
            "The host key for {} differs from the one recorded in\n  {}:{}\n\
             Someone could be intercepting this connection, or the host key was replaced.\n\
             Offered:  {}\nRecorded: {}\n\
             If the change is expected, delete that line and connect again.\n",
            self.host,
            path.display(),
            line,
            fingerprint(offered),
            recorded,
        ));
    }

    async fn ask_unknown(&self, key: &PublicKey) -> bool {
        self.out.lines(&format!(
            "The authenticity of host '{}' (port {}) can't be established.\n\
             Key fingerprint: {}\n",
            self.host,
            self.port,
            fingerprint(key)
        ));
        let mut input = self.input.lock().await;
        self.out
            .text("Are you sure you want to continue connecting (yes/no)? ");
        loop {
            match input.read_line(&self.out, true).await {
                Ok(answer) => match answer.trim().to_ascii_lowercase().as_str() {
                    "yes" => break,
                    "no" => return false,
                    _ => self.out.text("Please type 'yes' or 'no': "),
                },
                Err(Cancelled) => return false,
            }
        }
        match keys::known_hosts::learn_known_hosts_path(
            &self.host,
            self.port,
            key,
            &self.known_hosts,
        ) {
            Ok(()) => self.out.notice(&format!(
                "Added '{}' to {}.",
                self.host,
                self.known_hosts.display()
            )),
            Err(e) => self.out.error(&format!("Could not save the host key: {e}")),
        }
        true
    }
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_public_key.public_key();
        self.out
            .debug(&format!("Server host key: {}", fingerprint(&key)));
        let (status, found_in) = lookup_host_key(
            &self.host,
            self.port,
            &key,
            &self.known_hosts,
            &self.extra_known_hosts,
        );
        match &found_in {
            Some(path) => self.out.debug(&format!(
                "Host key for {} is recorded in {}",
                self.host,
                path.display()
            )),
            None => self.out.debug(&format!(
                "Host key for {} is not in {} or {}",
                self.host,
                self.known_hosts.display(),
                self.extra_known_hosts
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
        match status {
            HostKeyStatus::Known => Ok(true),
            HostKeyStatus::Changed { path, line } => {
                self.warn_changed(&key, &path, line);
                Ok(false)
            }
            HostKeyStatus::Unknown => Ok(self.ask_unknown(&key).await),
        }
    }

    async fn auth_banner(
        &mut self,
        banner: &str,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        self.out.lines(banner);
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let target = self.remote.lock().ok().and_then(|r| {
            r.iter()
                .find(|(port, _)| *port == connected_port)
                .map(|(_, t)| t.clone())
        });
        let Some(target) = target else {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        };
        reply.accept().await;
        self.out.debug(&format!(
            "Remote tunnel: {originator_address}:{originator_port} → {target}"
        ));
        crate::forward::serve_remote(channel, target, self.out.clone());
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        channel: russh::Channel<client::Msg>,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let Some(x11) = self.x11.clone() else {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        };
        reply.accept().await;
        self.out.debug(&format!(
            "X11 connection from {originator_address}:{originator_port} to {}",
            x11.display.name
        ));
        crate::x11::serve(channel.into_stream(), x11, self.out.clone());
        Ok(())
    }

    async fn kex_done(
        &mut self,
        _shared_secret: Option<&[u8]>,
        names: &russh::Names,
        session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        if let Some(weak) = &self.weak {
            let cipher = names.cipher.as_ref();
            let mut used = vec![names.kex.as_ref(), names.key.as_str(), cipher];
            // An AEAD cipher authenticates on its own; the MAC is unused.
            if !cipher.contains("poly1305") && !cipher.contains("gcm") {
                used.extend([names.client_mac.as_ref(), names.server_mac.as_ref()]);
            }
            let mut old: Vec<String> = Vec::new();
            for name in used {
                if LEGACY_ALGORITHMS.contains(&name) && !old.iter().any(|o| o == name) {
                    old.push(name.to_string());
                }
            }
            if !old.is_empty() {
                self.out
                    .debug(&format!("Old algorithms in use: {}", old.join(", ")));
            }
            if let Ok(mut w) = weak.lock() {
                *w = old;
            }
        }
        if self.out.debugging() {
            self.out.debug(&format!(
                "Server version: {}",
                String::from_utf8_lossy(session.remote_sshid()).trim()
            ));
            self.out.debug(&format!(
                "Key exchange: {}, host key: {}",
                names.kex.as_ref(),
                names.key.as_str()
            ));
            let cipher = names.cipher.as_ref();
            // AEAD ciphers authenticate on their own; the MAC is unused.
            let mac = if cipher.contains("poly1305") || cipher.contains("gcm") {
                "built into the cipher".to_string()
            } else {
                format!(
                    "{} / {}",
                    names.client_mac.as_ref(),
                    names.server_mac.as_ref()
                )
            };
            self.out.debug(&format!(
                "Cipher: {cipher}, MAC: {mac}, compression: {:?}",
                names.client_compression,
            ));
        }
        Ok(())
    }

    async fn disconnected(
        &mut self,
        reason: client::DisconnectReason<Self::Error>,
    ) -> Result<(), Self::Error> {
        match reason {
            client::DisconnectReason::ReceivedDisconnect(info) => {
                self.out.debug(&format!(
                    "Server disconnected: {} ({:?})",
                    if info.message.is_empty() {
                        "no message"
                    } else {
                        info.message.as_str()
                    },
                    info.reason_code
                ));
                Ok(())
            }
            client::DisconnectReason::Error(e) => {
                self.out.debug(&format!("Connection error: {e}"));
                Err(e)
            }
        }
    }
}

/// Why a session ended.
enum End {
    Closed(Option<u32>),
    Cancelled,
    Failed(String),
    /// Could not reach the server or finish the handshake.
    Unreachable(String),
    /// The connection dropped while the session was up.
    Lost,
    /// The server would not open another session on this connection.
    Refused(String),
}

impl From<Cancelled> for End {
    fn from(_: Cancelled) -> Self {
        End::Cancelled
    }
}

fn failed(context: &str) -> impl Fn(russh::Error) -> End + '_ {
    move |e| End::Failed(format!("{context}: {e}"))
}

fn report(end: End, host: &str, out: &Output, closed: &Closed) {
    let ending = match end {
        End::Closed(Some(code)) if code != 0 => {
            out.notice(&format!(
                "Connection to {host} closed (exit status {code})."
            ));
            Ending::Clean
        }
        End::Closed(_) => {
            out.notice(&format!("Connection to {host} closed."));
            Ending::Clean
        }
        End::Cancelled => {
            out.notice("Connection cancelled.");
            Ending::Cancelled
        }
        End::Lost => {
            // The prompt is usually still on the line.
            out.error(&format!("\r\nConnection to {host} lost."));
            Ending::Lost
        }
        End::Unreachable(msg) => {
            out.error(&msg);
            Ending::Unreachable
        }
        End::Failed(msg) | End::Refused(msg) => {
            out.error(&msg);
            Ending::Failed
        }
    };
    crate::pipe::finish(closed, out, ending);
}

async fn session_task(
    config: SshConfig,
    input: Arc<Mutex<Input>>,
    out: Output,
    closed: Closed,
    shared: Shared,
) {
    let out = out.with_debug(config.debug);
    let end = match run_session(&config, &input, &out, &shared).await {
        Ok(status) => End::Closed(status),
        Err(end) => end,
    };
    report(end, &config.host, &out, &closed);
}

/// A further shell on an already authenticated connection, falling back to
/// a new login if the server refuses it.
async fn channel_task(
    handle: Arc<Handle>,
    config: SshConfig,
    input: Arc<Mutex<Input>>,
    out: Output,
    closed: Closed,
    shared: Shared,
) {
    let out = out.with_debug(config.debug);
    let host = config.host.clone();
    out.notice(&format!(
        "New session on the existing connection to {host} (no new login)."
    ));
    let result = run_channel(&handle, &config.term_type, &input, &out, true, None).await;
    release(handle).await;
    let end = match result {
        Ok(status) => End::Closed(status),
        Err(End::Refused(reason)) => {
            out.notice(&format!(
                "{host} does not allow another session on the same connection ({reason}). \
                 Logging in again."
            ));
            match run_session(&config, &input, &out, &shared).await {
                Ok(status) => End::Closed(status),
                Err(end) => end,
            }
        }
        Err(end) => end,
    };
    report(end, &host, &out, &closed);
}

/// How long to wait for the server to confirm a PTY or shell on a shared
/// connection before assuming it was accepted.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Waits for the server's answer to a request sent with `want_reply`.
/// Output that arrives meanwhile goes to the terminal.
async fn confirmed(
    channel: &mut russh::Channel<client::Msg>,
    what: &str,
    out: &Output,
) -> Result<(), End> {
    let wait = async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => return Ok(()),
                Some(ChannelMsg::Failure) => return Err(End::Refused(format!("{what} refused"))),
                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => {
                    return Err(End::Refused("channel closed".into()));
                }
                Some(ChannelMsg::Data { data }) => out.bytes(&data),
                Some(ChannelMsg::ExtendedData { data, .. }) => out.bytes(&data),
                Some(_) => {}
            }
        }
    };
    tokio::time::timeout(REPLY_TIMEOUT, wait)
        .await
        .unwrap_or(Ok(()))
}

/// Drops one user of the connection, disconnecting if it was the last.
async fn release(handle: Arc<Handle>) {
    if Arc::strong_count(&handle) == 1 {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
    }
}

async fn run_session(
    config: &SshConfig,
    input: &Arc<Mutex<Input>>,
    out: &Output,
    shared: &Shared,
) -> Result<Option<u32>, End> {
    let remote = RemoteTargets::default();
    let x11 = config.x11.clone().map(|d| Arc::new(X11Forward::new(d)));
    let handle = Arc::new(login(config, input, out, remote.clone(), x11.clone(), false).await?);
    if let Ok(mut slot) = shared.lock() {
        *slot = Some(Arc::downgrade(&handle));
    }
    let tunnels = crate::forward::start(&handle, &config.forwards, &remote, out).await;
    let result = run_channel(
        &handle,
        &config.term_type,
        input,
        out,
        false,
        x11.as_deref(),
    )
    .await;
    // The tunnels belong to this pane's session.
    for task in tunnels {
        task.abort();
    }
    release(handle).await;
    result
}

/// Something an SSH connection can run over: TCP, or a channel through a
/// jump host.
trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// Connects (through the jump hosts, if any) and logs in.
fn login<'a>(
    config: &'a SshConfig,
    input: &'a Arc<Mutex<Input>>,
    out: &'a Output,
    remote: RemoteTargets,
    x11: Option<Arc<X11Forward>>,
    is_jump: bool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Handle, End>> + Send + 'a>> {
    Box::pin(async move {
        let (stream, jump): (Box<dyn Stream>, Option<Arc<Handle>>) = match &config.jump {
            None => {
                out.notice(&format!("Connecting to {}:{}…", config.host, config.port));
                // Only resolving and the TCP connect are timed: the host key
                // question may take a while.
                let stream = connect_tcp(config, out).await?;
                let _ = stream.set_nodelay(true);
                (Box::new(stream), None)
            }
            Some(via) => {
                let jump =
                    Arc::new(login(via, input, out, RemoteTargets::default(), None, true).await?);
                out.notice(&format!(
                    "Connecting to {}:{} through {}…",
                    config.host, config.port, via.host
                ));
                let channel = jump
                    .channel_open_direct_tcpip(
                        config.host.as_str(),
                        u32::from(config.port),
                        "127.0.0.1",
                        0,
                    )
                    .await
                    .map_err(|e| {
                        End::Unreachable(format!(
                            "The jump host {} could not reach {}:{}: {e}",
                            via.host, config.host, config.port
                        ))
                    })?;
                (Box::new(channel.into_stream()), Some(jump))
            }
        };
        if let Some(every) = config.keepalive {
            out.debug(&format!("Keepalive every {} s", every.as_secs()));
        }

        if config.legacy_algorithms {
            out.debug("Old algorithms are offered too (legacy algorithms on)");
        }
        let ssh_config = Arc::new(client::Config {
            preferred: preferred(config.legacy_algorithms),
            keepalive_interval: config.keepalive,
            keepalive_max: 3,
            inactivity_timeout: None,
            nodelay: true,
            ..Default::default()
        });
        let handler = ClientHandler {
            host: config.host.clone(),
            port: config.port,
            known_hosts: config.known_hosts.clone(),
            extra_known_hosts: config.extra_known_hosts.clone(),
            input: input.clone(),
            out: out.clone(),
            remote,
            weak: (!is_jump).then(|| config.weak.clone()),
            x11,
            _jump: jump,
        };
        let mut handle = match client::connect_stream(ssh_config, stream, handler).await {
            Ok(h) => h,
            Err(russh::Error::UnknownKey) => {
                return Err(End::Failed("Host key verification failed.".into()));
            }
            Err(russh::Error::NoCommonAlgo { kind, theirs, .. }) => {
                return Err(End::Failed(no_common_algorithm(
                    &kind,
                    &theirs,
                    config.legacy_algorithms,
                )));
            }
            Err(e) => return Err(End::Unreachable(format!("SSH handshake failed: {e}"))),
        };

        let user = match config.username.as_deref().filter(|u| !u.is_empty()) {
            Some(u) => u.to_string(),
            None => {
                let mut input = input.lock().await;
                if is_jump {
                    out.text(&format!("login as (jump host {}): ", config.host));
                } else {
                    out.text("login as: ");
                }
                input.read_line(out, true).await?.trim().to_string()
            }
        };
        out.debug(&format!("Authenticating to {} as '{user}'", config.host));

        authenticate(&mut handle, config, &user, input, out).await?;
        Ok(handle)
    })
}

/// Resolves the host and connects to its addresses in turn.
async fn connect_tcp(config: &SshConfig, out: &Output) -> Result<tokio::net::TcpStream, End> {
    let timed_out = || {
        End::Unreachable(format!(
            "Connection to {}:{} timed out after {} s.",
            config.host,
            config.port,
            config.connect_timeout.as_secs()
        ))
    };
    let deadline = tokio::time::Instant::now() + config.connect_timeout;
    let addrs: Vec<std::net::SocketAddr> = match tokio::time::timeout_at(
        deadline,
        tokio::net::lookup_host((config.host.as_str(), config.port)),
    )
    .await
    {
        Err(_) => return Err(timed_out()),
        Ok(Err(e)) => return Err(End::Unreachable(format!("Cannot connect: {e}"))),
        Ok(Ok(addrs)) => addrs.collect(),
    };
    out.debug(&format!(
        "{} resolves to {}",
        config.host,
        addrs
            .iter()
            .map(|a| a.ip().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let mut last_error = None;
    for addr in addrs {
        out.debug(&format!(
            "Connecting to {} port {}…",
            addr.ip(),
            addr.port()
        ));
        match tokio::time::timeout_at(deadline, tokio::net::TcpStream::connect(addr)).await {
            Err(_) => return Err(timed_out()),
            Ok(Ok(stream)) => {
                if let Ok(local) = stream.local_addr() {
                    out.debug(&format!("Connected from {local}"));
                }
                return Ok(stream);
            }
            Ok(Err(e)) => {
                out.debug(&format!("{addr}: {e}"));
                last_error = Some(e);
            }
        }
    }
    Err(End::Unreachable(match last_error {
        Some(e) => format!("Cannot connect: {e}"),
        None => format!("Cannot connect: no address for {}", config.host),
    }))
}

/// Opens a shell channel with a PTY and relays it until it closes. With
/// `confirm`, refusals are reported as [`End::Refused`] (shared connections).
async fn run_channel(
    handle: &Handle,
    term_type: &str,
    input: &Arc<Mutex<Input>>,
    out: &Output,
    confirm: bool,
    x11: Option<&X11Forward>,
) -> Result<Option<u32>, End> {
    out.debug("Opening a session channel");
    let mut channel = match handle.channel_open_session().await {
        Ok(c) => c,
        Err(e) if confirm => return Err(End::Refused(e.to_string())),
        Err(e) => return Err(failed("Cannot open a session")(e)),
    };
    let mut input = input.lock().await;
    let size = input.size;
    out.debug(&format!(
        "Requesting a {term_type} terminal, {}x{}",
        size.cols, size.rows
    ));
    channel
        .request_pty(
            confirm,
            term_type,
            u32::from(size.cols),
            u32::from(size.rows),
            0,
            0,
            &[],
        )
        .await
        .map_err(failed("PTY request failed"))?;
    if confirm {
        confirmed(&mut channel, "terminal", out).await?;
    }
    if let Some(x11) = x11 {
        out.debug(&format!(
            "Requesting X11 forwarding to {} (screen {})",
            x11.display.name, x11.display.screen
        ));
        // A refusal is not fatal: the shell works without X11.
        if let Err(e) = channel
            .request_x11(
                false,
                false,
                crate::x11::MIT_MAGIC_COOKIE,
                crate::x11::to_hex(&x11.fake),
                x11.display.screen,
            )
            .await
        {
            out.error(&format!("X11 forwarding was not set up: {e}"));
        }
    }
    out.debug("Starting the shell");
    channel
        .request_shell(confirm)
        .await
        .map_err(failed("Shell request failed"))?;
    if confirm {
        confirmed(&mut channel, "shell", out).await?;
    }

    let mut exit_status = None;
    loop {
        tokio::select! {
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => out.bytes(&data),
                Some(ChannelMsg::ExtendedData { data, .. }) => out.bytes(&data),
                Some(ChannelMsg::ExitStatus { exit_status: code }) => exit_status = Some(code),
                Some(ChannelMsg::Close) => break,
                // The connection went away under the channel.
                None if exit_status.is_none() => return Err(End::Lost),
                None => break,
                Some(_) => {}
            },
            cmd = input.rx.recv() => match cmd {
                Some(Cmd::Data(bytes)) => {
                    if channel.data(&bytes[..]).await.is_err() {
                        return Err(End::Lost);
                    }
                }
                Some(Cmd::Resize(size)) => {
                    input.size = size;
                    let _ = channel
                        .window_change(u32::from(size.cols), u32::from(size.rows), 0, 0)
                        .await;
                }
                // russh has no client "break" request; nothing to send.
                Some(Cmd::Break) => {}
                Some(Cmd::Close) | None => {
                    let _ = channel.close().await;
                    break;
                }
            },
        }
    }
    Ok(exit_status)
}

/// Outcome of one authentication method.
enum Step {
    Done,
    /// Not accepted; the server lists what it still allows.
    Next(MethodSet),
}

impl From<client::AuthResult> for Step {
    fn from(r: client::AuthResult) -> Self {
        match r {
            client::AuthResult::Success => Step::Done,
            client::AuthResult::Failure {
                remaining_methods, ..
            } => Step::Next(remaining_methods),
        }
    }
}

pub(crate) type Handle = client::Handle<ClientHandler>;

async fn authenticate(
    handle: &mut Handle,
    config: &SshConfig,
    user: &str,
    input: &Arc<Mutex<Input>>,
    out: &Output,
) -> Result<(), End> {
    let mut methods = match step(
        out,
        "Login without credentials (method none)",
        handle
            .authenticate_none(user)
            .await
            .map_err(failed("Authentication failed"))?,
    ) {
        Step::Done => return Ok(()),
        Step::Next(m) => m,
    };
    let rsa_hash = match handle.best_supported_rsa_hash().await {
        Ok(Some(hash)) => hash,
        // The server did not say; every current OpenSSH accepts SHA-256.
        _ => Some(HashAlg::Sha256),
    };
    out.debug(&format!(
        "RSA keys sign with {}",
        match rsa_hash {
            Some(HashAlg::Sha512) => "rsa-sha2-512",
            Some(HashAlg::Sha256) => "rsa-sha2-256",
            None => "ssh-rsa (SHA-1)",
            Some(_) => "another hash",
        }
    ));

    if methods.contains(&MethodKind::PublicKey) {
        if config.use_agent {
            match agent_auth(handle, user, rsa_hash, out).await {
                Some(Step::Done) => return Ok(()),
                Some(Step::Next(m)) => methods = m,
                None => {}
            }
        } else {
            out.debug("SSH agent not used for this session");
        }
        match key_file_auth(handle, config, user, rsa_hash, input, out).await? {
            Some(Step::Done) => return Ok(()),
            Some(Step::Next(m)) => methods = m,
            None => {}
        }
    } else {
        out.debug("Server does not accept public keys; skipping the agent and key files");
    }

    let account = format!("{user}@{}:{}", config.host, config.port);
    let mut saved = SavedPassword::load(config, &account).await;

    if !methods.contains(&MethodKind::KeyboardInteractive)
        && !methods.contains(&MethodKind::Password)
    {
        out.debug("Server accepts neither keyboard-interactive nor password");
    }
    if methods.contains(&MethodKind::KeyboardInteractive) {
        match keyboard_interactive_auth(handle, user, &mut saved, input, out).await? {
            Step::Done => return Ok(()),
            Step::Next(m) => methods = m,
        }
    }

    if methods.contains(&MethodKind::Password) {
        for attempt in 0..PASSWORD_ATTEMPTS {
            let (password, from_store) = match saved.take() {
                Some(p) => {
                    out.notice("Using the password saved in the keychain.");
                    (p, true)
                }
                None => {
                    let mut input = input.lock().await;
                    out.text(&format!("{user}@{}'s password: ", config.host));
                    (input.read_line(out, false).await?, false)
                }
            };
            let what = if from_store {
                "Password (saved in the keychain)"
            } else {
                "Password (typed)"
            };
            match step(
                out,
                what,
                handle
                    .authenticate_password(user, password.clone())
                    .await
                    .map_err(failed("Authentication failed"))?,
            ) {
                Step::Done => {
                    if !from_store {
                        saved.offer_save(&password, input, out).await;
                    }
                    return Ok(());
                }
                Step::Next(m) => {
                    methods = m;
                    if from_store {
                        saved.reject(out).await;
                    }
                    if !methods.contains(&MethodKind::Password) {
                        break;
                    }
                    if attempt + 1 < PASSWORD_ATTEMPTS && !from_store {
                        out.text("Permission denied, please try again.\r\n");
                    }
                }
            }
        }
    }

    let allowed: Vec<&str> = methods.iter().map(<&str>::from).collect();
    Err(End::Failed(format!(
        "Permission denied ({}).",
        if allowed.is_empty() {
            "no methods left".to_string()
        } else {
            allowed.join(",")
        }
    )))
}

#[cfg(unix)]
async fn agent_auth(
    handle: &mut Handle,
    user: &str,
    rsa_hash: Option<HashAlg>,
    out: &Output,
) -> Option<Step> {
    match AgentClient::connect_env().await {
        Ok(agent) => agent_try_all(handle, user, rsa_hash, agent, "ssh-agent", out).await,
        Err(e) => {
            out.debug(&format!("No SSH agent (SSH_AUTH_SOCK): {e}"));
            None
        }
    }
}

#[cfg(windows)]
async fn agent_auth(
    handle: &mut Handle,
    user: &str,
    rsa_hash: Option<HashAlg>,
    out: &Output,
) -> Option<Step> {
    let mut last = None;
    match AgentClient::connect_pageant().await {
        Ok(agent) => match agent_try_all(handle, user, rsa_hash, agent, "Pageant", out).await {
            Some(Step::Done) => return Some(Step::Done),
            other => last = other,
        },
        Err(e) => out.debug(&format!("No Pageant: {e}")),
    }
    match AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        Ok(agent) => {
            match agent_try_all(handle, user, rsa_hash, agent, "OpenSSH agent", out).await {
                Some(Step::Done) => return Some(Step::Done),
                Some(other) => last = Some(other),
                None => {}
            }
        }
        Err(e) => out.debug(&format!("No OpenSSH agent: {e}")),
    }
    last
}

#[cfg(not(any(unix, windows)))]
async fn agent_auth(_: &mut Handle, _: &str, _: Option<HashAlg>, _: &Output) -> Option<Step> {
    None
}

/// Offers every key the agent holds. `None` if the agent had no usable keys.
async fn agent_try_all<S>(
    handle: &mut Handle,
    user: &str,
    rsa_hash: Option<HashAlg>,
    mut agent: AgentClient<S>,
    agent_name: &str,
    out: &Output,
) -> Option<Step>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let identities = match agent.request_identities().await {
        Ok(ids) => ids,
        Err(e) => {
            out.debug(&format!("{agent_name}: cannot list keys: {e}"));
            return None;
        }
    };
    out.debug(&format!("{agent_name} holds {} key(s)", identities.len()));
    let mut last = None;
    for identity in identities {
        let AgentIdentity::PublicKey { key, comment } = identity else {
            out.debug(&format!("{agent_name}: skipping a certificate"));
            continue;
        };
        let what = format!("{agent_name} key {} {comment}", fingerprint(&key));
        out.debug(&format!("Offering {what}"));
        let hash = if key.algorithm().is_rsa() {
            rsa_hash
        } else {
            None
        };
        match handle
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
        {
            Ok(result) => match step(out, &what, result) {
                Step::Done => return Some(Step::Done),
                next => last = Some(next),
            },
            Err(e) => out.debug(&format!("{what}: the agent could not sign: {e}")),
        }
    }
    last
}

fn is_public_key_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "pub")
}

/// For `id_ed25519.pub`, `id_ed25519` if it exists.
pub fn private_key_for(path: &Path) -> Option<PathBuf> {
    if !is_public_key_file(path) {
        return None;
    }
    let private = path.with_extension("");
    private.is_file().then_some(private)
}

/// Explicit identity files first, then the OpenSSH defaults that exist.
fn identity_candidates(config: &SshConfig) -> Vec<(PathBuf, bool)> {
    let mut out: Vec<(PathBuf, bool)> = config
        .identity_files
        .iter()
        .map(|p| (p.clone(), true))
        .collect();
    if config.use_default_identities
        && let Some(ssh_dir) = dirs::home_dir().map(|h| h.join(".ssh"))
    {
        for name in DEFAULT_IDENTITIES {
            let path = ssh_dir.join(name);
            if path.is_file() && !out.iter().any(|(p, _)| *p == path) {
                out.push((path, false));
            }
        }
    }
    out
}

async fn key_file_auth(
    handle: &mut Handle,
    config: &SshConfig,
    user: &str,
    rsa_hash: Option<HashAlg>,
    input: &Arc<Mutex<Input>>,
    out: &Output,
) -> Result<Option<Step>, End> {
    let mut last = None;
    let candidates = identity_candidates(config);
    if candidates.is_empty() {
        out.debug(
            "No key files: none set for the session and no ~/.ssh/id_ed25519, id_ecdsa or id_rsa",
        );
    }
    for (path, explicit) in candidates {
        // A `.pub` file is the public half; the private key sits next to it.
        let path = match private_key_for(&path) {
            Some(private) => {
                out.debug(&format!(
                    "{} is a public key; using the private key {}",
                    path.display(),
                    private.display()
                ));
                private
            }
            None if is_public_key_file(&path) => {
                out.error(&format!(
                    "{} is a public key. Choose the private key (the same name without .pub).",
                    path.display()
                ));
                continue;
            }
            None => path,
        };
        out.debug(&format!("Trying key file {}", path.display()));
        let key = match keys::load_secret_key(&path, None) {
            Ok(key) => key,
            Err(keys::Error::KeyIsEncrypted) => {
                out.debug("The key is encrypted; asking for its passphrase");
                match ask_passphrase(&path, input, out).await? {
                    Some(key) => key,
                    None => {
                        out.debug("No passphrase given; skipping the key");
                        continue;
                    }
                }
            }
            Err(e) => {
                if explicit {
                    out.error(&format!("Cannot read key {}: {e}", path.display()));
                } else {
                    out.debug(&format!("Cannot read the key: {e}"));
                }
                continue;
            }
        };
        let what = format!(
            "Key file {} ({})",
            path.display(),
            fingerprint(key.public_key())
        );
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash);
        match step(
            out,
            &what,
            handle
                .authenticate_publickey(user, key)
                .await
                .map_err(failed("Authentication failed"))?,
        ) {
            Step::Done => return Ok(Some(Step::Done)),
            next => last = Some(next),
        }
    }
    Ok(last)
}

/// Asks for a key's passphrase. An empty answer skips the key.
async fn ask_passphrase(
    path: &Path,
    input: &Arc<Mutex<Input>>,
    out: &Output,
) -> Result<Option<PrivateKey>, Cancelled> {
    let mut input = input.lock().await;
    for _ in 0..PASSWORD_ATTEMPTS {
        out.text(&format!("Enter passphrase for key '{}': ", path.display()));
        let passphrase = input.read_line(out, false).await?;
        if passphrase.is_empty() {
            return Ok(None);
        }
        match keys::load_secret_key(path, Some(&passphrase)) {
            Ok(key) => return Ok(Some(key)),
            Err(_) => out.text("Bad passphrase.\r\n"),
        }
    }
    Ok(None)
}

/// The saved password for one login attempt, and saving a new one.
struct SavedPassword {
    store: Option<Store>,
    account: String,
    password: Option<String>,
    mode: SavePassword,
}

impl SavedPassword {
    async fn load(config: &SshConfig, account: &str) -> Self {
        let store = config.password_store.clone();
        let password = match &store {
            Some(store) => {
                let (store, account) = (store.clone(), account.to_string());
                tokio::task::spawn_blocking(move || store.0.get(&account))
                    .await
                    .ok()
                    .flatten()
            }
            None => None,
        };
        Self {
            store,
            account: account.to_string(),
            password,
            mode: config.save_password,
        }
    }

    /// The saved password, at most once per connection.
    fn take(&mut self) -> Option<String> {
        self.password.take()
    }

    /// The server refused the saved password: forget it.
    async fn reject(&self, out: &Output) {
        out.text("The saved password was rejected.\r\n");
        if let Some(store) = self.store.clone() {
            let account = self.account.clone();
            let _ = tokio::task::spawn_blocking(move || store.0.delete(&account)).await;
        }
    }

    /// After a typed password worked: save it, asking first if configured so.
    async fn offer_save(&self, password: &str, input: &Arc<Mutex<Input>>, out: &Output) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let save = match self.mode {
            SavePassword::Never => false,
            SavePassword::Always => true,
            SavePassword::Ask => {
                let mut input = input.lock().await;
                out.text("Save this password in the system keychain? (yes/no) ");
                loop {
                    match input.read_line(out, true).await {
                        Ok(a) => match a.trim().to_ascii_lowercase().as_str() {
                            "yes" | "y" => break true,
                            "no" | "n" | "" => break false,
                            _ => out.text("Please type 'yes' or 'no': "),
                        },
                        Err(Cancelled) => break false,
                    }
                }
            }
        };
        if !save {
            return;
        }
        let (account, password) = (self.account.clone(), password.to_string());
        match tokio::task::spawn_blocking(move || store.0.set(&account, &password)).await {
            Ok(Ok(())) => out.notice("Password saved in the system keychain."),
            Ok(Err(e)) => out.error(&format!("Could not save the password: {e}")),
            Err(e) => out.error(&format!("Could not save the password: {e}")),
        }
    }
}

/// Index of the hidden "Password:" prompt in a round, if any. Other prompts
/// (one-time codes, tokens) are always asked.
fn password_prompt(prompts: &[client::Prompt]) -> Option<usize> {
    prompts
        .iter()
        .position(|p| !p.echo && p.prompt.to_ascii_lowercase().contains("password"))
}

/// Keyboard-interactive login, which may take several rounds, e.g. a
/// password and then a two-factor token. The saved password answers the
/// password prompt; everything else is asked in the terminal.
async fn keyboard_interactive_auth(
    handle: &mut Handle,
    user: &str,
    saved: &mut SavedPassword,
    input: &Arc<Mutex<Input>>,
    out: &Output,
) -> Result<Step, End> {
    out.debug("Trying keyboard-interactive");
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(failed("Authentication failed"))?;
    // The password the user typed in this login, offered for saving at the end.
    let mut typed: Option<String> = None;
    // The saved password went out in the last password round.
    let mut sent_saved = false;
    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => {
                out.debug("keyboard-interactive: accepted");
                if let Some(password) = &typed {
                    saved.offer_save(password, input, out).await;
                }
                return Ok(Step::Done);
            }
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods,
                partial_success,
            } => {
                out.debug(&format!(
                    "keyboard-interactive: {}; server now allows: {}",
                    if partial_success {
                        "partial success, more is needed"
                    } else {
                        "rejected"
                    },
                    method_list(&remaining_methods)
                ));
                if sent_saved {
                    saved.reject(out).await;
                }
                return Ok(Step::Next(remaining_methods));
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                out.debug(&format!(
                    "keyboard-interactive: the server asks {} question(s){}",
                    prompts.len(),
                    prompts
                        .iter()
                        .map(|p| format!(" [{}]", p.prompt.trim()))
                        .collect::<String>()
                ));
                let password_at = password_prompt(&prompts);
                // Asked for the password again: the saved one was wrong. A
                // round asking only for a token means the password worked.
                if password_at.is_some() && sent_saved {
                    saved.reject(out).await;
                    sent_saved = false;
                }
                let mut answers = Vec::with_capacity(prompts.len());
                let mut input = input.lock().await;
                let mut header_shown = false;
                for (i, prompt) in prompts.iter().enumerate() {
                    if Some(i) == password_at
                        && let Some(p) = saved.take()
                    {
                        out.notice("Using the password saved in the keychain.");
                        answers.push(p);
                        sent_saved = true;
                        continue;
                    }
                    if !header_shown {
                        for text in [&name, &instructions] {
                            if !text.is_empty() {
                                out.lines(text);
                                out.text("\r\n");
                            }
                        }
                        header_shown = true;
                    }
                    out.lines(&prompt.prompt);
                    let answer = input.read_line(out, prompt.echo).await?;
                    if Some(i) == password_at {
                        typed = Some(answer.clone());
                    }
                    answers.push(answer);
                }
                drop(input);
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(failed("Authentication failed"))?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> PublicKey {
        PrivateKey::random(&mut rand::rng(), keys::Algorithm::Ed25519)
            .unwrap()
            .public_key()
            .clone()
    }

    #[test]
    fn host_key_lookup_order_and_change_detection() {
        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("known_hosts");
        let extra = dir.path().join("openssh_known_hosts");
        let (k1, k2) = (key(), key());

        assert_eq!(
            check_host_key("h", 2222, &k1, &own, std::slice::from_ref(&extra)),
            HostKeyStatus::Unknown
        );

        keys::known_hosts::learn_known_hosts_path("h", 2222, &k1, &extra).unwrap();
        assert_eq!(
            check_host_key("h", 2222, &k1, &own, std::slice::from_ref(&extra)),
            HostKeyStatus::Known
        );
        assert_eq!(
            check_host_key("h", 22, &k1, &own, std::slice::from_ref(&extra)),
            HostKeyStatus::Unknown,
            "port is part of the identity"
        );
        assert!(matches!(
            check_host_key("h", 2222, &k2, &own, std::slice::from_ref(&extra)),
            HostKeyStatus::Changed { .. }
        ));
    }

    #[test]
    fn public_key_files_point_to_the_private_key() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("lab_key");
        let public = dir.path().join("lab_key.pub");
        std::fs::write(&public, "ssh-ed25519 AAAA").unwrap();
        assert_eq!(private_key_for(&public), None, "no private key yet");
        std::fs::write(&private, "secret").unwrap();
        assert_eq!(private_key_for(&public), Some(private.clone()));
        assert_eq!(private_key_for(&private), None);
    }

    #[test]
    fn pop_char_removes_whole_utf8_characters() {
        let mut line = "aé".as_bytes().to_vec();
        assert!(pop_char(&mut line));
        assert_eq!(line, b"a");
        assert!(pop_char(&mut line));
        assert!(!pop_char(&mut line));
    }
}
