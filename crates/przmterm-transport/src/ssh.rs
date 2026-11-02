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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{self, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::{ChannelMsg, Disconnect, MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};

use crate::pipe::{Cmd, Output, Pipe, runtime};
use crate::{Protocol, Size, Transport, TransportError, Waker};

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
        task: impl FnOnce(Arc<Mutex<Input>>, Output, Arc<AtomicBool>) -> TaskFuture,
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
    for path in std::iter::once(own).chain(extra.iter().map(PathBuf::as_path)) {
        match keys::check_known_hosts_path(host, port, key, path) {
            Ok(true) => return HostKeyStatus::Known,
            Err(keys::Error::KeyChanged { line }) => {
                return HostKeyStatus::Changed {
                    path: path.to_path_buf(),
                    line,
                };
            }
            _ => {}
        }
    }
    HostKeyStatus::Unknown
}

fn fingerprint(key: &PublicKey) -> String {
    format!(
        "{} {}",
        key.algorithm().as_str(),
        key.fingerprint(HashAlg::Sha256)
    )
}

struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    extra_known_hosts: Vec<PathBuf>,
    input: Arc<Mutex<Input>>,
    out: Output,
}

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
        match check_host_key(
            &self.host,
            self.port,
            &key,
            &self.known_hosts,
            &self.extra_known_hosts,
        ) {
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
}

/// Why a session ended.
enum End {
    Closed(Option<u32>),
    Cancelled,
    Failed(String),
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

fn report(end: End, host: &str, out: &Output, closed: &AtomicBool) {
    match end {
        End::Closed(Some(code)) if code != 0 => out.notice(&format!(
            "Connection to {host} closed (exit status {code})."
        )),
        End::Closed(_) => out.notice(&format!("Connection to {host} closed.")),
        End::Cancelled => out.notice("Connection cancelled."),
        End::Failed(msg) | End::Refused(msg) => out.error(&msg),
    }
    closed.store(true, Ordering::Release);
    (out.waker)();
}

async fn session_task(
    config: SshConfig,
    input: Arc<Mutex<Input>>,
    out: Output,
    closed: Arc<AtomicBool>,
    shared: Shared,
) {
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
    closed: Arc<AtomicBool>,
    shared: Shared,
) {
    let host = config.host.clone();
    out.notice(&format!(
        "New session on the existing connection to {host} (no new login)."
    ));
    let result = run_channel(&handle, &config.term_type, &input, &out, true).await;
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
    out.notice(&format!("Connecting to {}:{}…", config.host, config.port));

    // Only the TCP connect is timed: the host key question may take a while.
    let stream = match tokio::time::timeout(
        config.connect_timeout,
        tokio::net::TcpStream::connect((config.host.as_str(), config.port)),
    )
    .await
    {
        Err(_) => {
            return Err(End::Failed(format!(
                "Connection to {}:{} timed out after {} s.",
                config.host,
                config.port,
                config.connect_timeout.as_secs()
            )));
        }
        Ok(Err(e)) => return Err(End::Failed(format!("Cannot connect: {e}"))),
        Ok(Ok(s)) => s,
    };
    let _ = stream.set_nodelay(true);

    let ssh_config = Arc::new(client::Config {
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
    };
    let mut handle = match client::connect_stream(ssh_config, stream, handler).await {
        Ok(h) => h,
        Err(russh::Error::UnknownKey) => {
            return Err(End::Failed("Host key verification failed.".into()));
        }
        Err(e) => return Err(End::Failed(format!("SSH handshake failed: {e}"))),
    };

    let user = match config.username.as_deref().filter(|u| !u.is_empty()) {
        Some(u) => u.to_string(),
        None => {
            let mut input = input.lock().await;
            out.text("login as: ");
            input.read_line(out, true).await?.trim().to_string()
        }
    };

    authenticate(&mut handle, config, &user, input, out).await?;

    let handle = Arc::new(handle);
    if let Ok(mut slot) = shared.lock() {
        *slot = Some(Arc::downgrade(&handle));
    }
    let result = run_channel(&handle, &config.term_type, input, out, false).await;
    release(handle).await;
    result
}

/// Opens a shell channel with a PTY and relays it until it closes. With
/// `confirm`, refusals are reported as [`End::Refused`] (shared connections).
async fn run_channel(
    handle: &Handle,
    term_type: &str,
    input: &Arc<Mutex<Input>>,
    out: &Output,
    confirm: bool,
) -> Result<Option<u32>, End> {
    let mut channel = match handle.channel_open_session().await {
        Ok(c) => c,
        Err(e) if confirm => return Err(End::Refused(e.to_string())),
        Err(e) => return Err(failed("Cannot open a session")(e)),
    };
    let mut input = input.lock().await;
    let size = input.size;
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
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
            cmd = input.rx.recv() => match cmd {
                Some(Cmd::Data(bytes)) => {
                    if channel.data(&bytes[..]).await.is_err() {
                        break;
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

type Handle = client::Handle<ClientHandler>;

async fn authenticate(
    handle: &mut Handle,
    config: &SshConfig,
    user: &str,
    input: &Arc<Mutex<Input>>,
    out: &Output,
) -> Result<(), End> {
    let mut methods = match handle
        .authenticate_none(user)
        .await
        .map_err(failed("Authentication failed"))?
        .into()
    {
        Step::Done => return Ok(()),
        Step::Next(m) => m,
    };
    let rsa_hash = match handle.best_supported_rsa_hash().await {
        Ok(Some(hash)) => hash,
        // The server did not say; every current OpenSSH accepts SHA-256.
        _ => Some(HashAlg::Sha256),
    };

    if methods.contains(&MethodKind::PublicKey) {
        if config.use_agent {
            match agent_auth(handle, user, rsa_hash).await {
                Some(Step::Done) => return Ok(()),
                Some(Step::Next(m)) => methods = m,
                None => {}
            }
        }
        match key_file_auth(handle, config, user, rsa_hash, input, out).await? {
            Some(Step::Done) => return Ok(()),
            Some(Step::Next(m)) => methods = m,
            None => {}
        }
    }

    let account = format!("{user}@{}:{}", config.host, config.port);
    let mut saved = SavedPassword::load(config, &account).await;

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
            match handle
                .authenticate_password(user, password.clone())
                .await
                .map_err(failed("Authentication failed"))?
                .into()
            {
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
async fn agent_auth(handle: &mut Handle, user: &str, rsa_hash: Option<HashAlg>) -> Option<Step> {
    let agent = AgentClient::connect_env().await.ok()?;
    agent_try_all(handle, user, rsa_hash, agent).await
}

#[cfg(windows)]
async fn agent_auth(handle: &mut Handle, user: &str, rsa_hash: Option<HashAlg>) -> Option<Step> {
    let mut last = None;
    if let Ok(agent) = AgentClient::connect_pageant().await {
        match agent_try_all(handle, user, rsa_hash, agent).await {
            Some(Step::Done) => return Some(Step::Done),
            other => last = other,
        }
    }
    if let Ok(agent) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        match agent_try_all(handle, user, rsa_hash, agent).await {
            Some(Step::Done) => return Some(Step::Done),
            Some(other) => last = Some(other),
            None => {}
        }
    }
    last
}

#[cfg(not(any(unix, windows)))]
async fn agent_auth(_: &mut Handle, _: &str, _: Option<HashAlg>) -> Option<Step> {
    None
}

/// Offers every key the agent holds. `None` if the agent had no usable keys.
async fn agent_try_all<S>(
    handle: &mut Handle,
    user: &str,
    rsa_hash: Option<HashAlg>,
    mut agent: AgentClient<S>,
) -> Option<Step>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let identities = agent.request_identities().await.ok()?;
    let mut last = None;
    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let hash = if key.algorithm().is_rsa() {
            rsa_hash
        } else {
            None
        };
        match handle
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
        {
            Ok(client::AuthResult::Success) => return Some(Step::Done),
            Ok(failure) => last = Some(failure.into()),
            Err(e) => tracing::debug!("agent signature failed: {e}"),
        }
    }
    last
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
    for (path, explicit) in identity_candidates(config) {
        let key = match keys::load_secret_key(&path, None) {
            Ok(key) => key,
            Err(keys::Error::KeyIsEncrypted) => match ask_passphrase(&path, input, out).await? {
                Some(key) => key,
                None => continue,
            },
            Err(e) => {
                if explicit {
                    out.error(&format!("Cannot read key {}: {e}", path.display()));
                }
                continue;
            }
        };
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash);
        match handle
            .authenticate_publickey(user, key)
            .await
            .map_err(failed("Authentication failed"))?
            .into()
        {
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
                if let Some(password) = &typed {
                    saved.offer_save(password, input, out).await;
                }
                return Ok(Step::Done);
            }
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods, ..
            } => {
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
    fn pop_char_removes_whole_utf8_characters() {
        let mut line = "aé".as_bytes().to_vec();
        assert!(pop_char(&mut line));
        assert_eq!(line, b"a");
        assert!(pop_char(&mut line));
        assert!(!pop_char(&mut line));
    }
}
