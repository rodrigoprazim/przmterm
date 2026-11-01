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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc as std_mpsc};
use std::time::Duration;

use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{self, HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::{ChannelMsg, Disconnect, MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};

use crate::{Protocol, Size, Transport, TransportError, Waker};

/// Default key files tried after the agent, in this order (like OpenSSH).
const DEFAULT_IDENTITIES: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];
const PASSWORD_ATTEMPTS: usize = 3;

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
        }
    }
}

/// An SSH session seen from the GUI: non-blocking reads and writes.
pub struct SshTransport {
    cmds: mpsc::UnboundedSender<Cmd>,
    output: std_mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pending_pos: usize,
    closed: Arc<AtomicBool>,
}

impl SshTransport {
    /// Starts connecting in the background and returns immediately.
    /// `waker` is called whenever new output is available.
    pub fn connect(config: SshConfig, size: Size, waker: Waker) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = std_mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let input = Arc::new(Mutex::new(Input { rx: cmd_rx, size }));
        let out = Output { tx: out_tx, waker };
        runtime().spawn(session_task(config, input, out, closed.clone()));
        Self {
            cmds: cmd_tx,
            output: out_rx,
            pending: Vec::new(),
            pending_pos: 0,
            closed,
        }
    }
}

impl Transport for SshTransport {
    fn protocol(&self) -> Protocol {
        Protocol::Ssh
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if self.pending_pos >= self.pending.len() {
            match self.output.try_recv() {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.pending_pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let rest = &self.pending[self.pending_pos..];
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        self.pending_pos += n;
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.cmds
            .send(Cmd::Data(data.to_vec()))
            .map_err(|_| TransportError::Closed)
    }

    fn resize(&mut self, size: Size) -> Result<(), TransportError> {
        self.cmds
            .send(Cmd::Resize(size))
            .map_err(|_| TransportError::Closed)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        let _ = self.cmds.send(Cmd::Close);
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

impl Drop for SshTransport {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Close);
    }
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("przmterm-net")
            .enable_all()
            .build()
            .expect("failed to start the network runtime")
    })
}

enum Cmd {
    Data(Vec<u8>),
    Resize(Size),
    Close,
}

/// Terminal-bound side of the task.
#[derive(Clone)]
struct Output {
    tx: std_mpsc::Sender<Vec<u8>>,
    waker: Waker,
}

impl Output {
    fn bytes(&self, data: &[u8]) {
        if !data.is_empty() && self.tx.send(data.to_vec()).is_ok() {
            (self.waker)();
        }
    }

    fn text(&self, text: &str) {
        self.bytes(text.as_bytes());
    }

    /// Normalises bare `\n` to `\r\n` for text that comes from the server.
    fn lines(&self, text: &str) {
        let normalised = text.replace("\r\n", "\n").replace('\n', "\r\n");
        self.text(&normalised);
    }

    fn notice(&self, text: &str) {
        self.text(&format!("\x1b[2m{text}\x1b[0m\r\n"));
    }

    fn error(&self, text: &str) {
        self.text(&format!("\x1b[31m{text}\x1b[0m\r\n"));
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
}

impl From<Cancelled> for End {
    fn from(_: Cancelled) -> Self {
        End::Cancelled
    }
}

fn failed(context: &str) -> impl Fn(russh::Error) -> End + '_ {
    move |e| End::Failed(format!("{context}: {e}"))
}

async fn session_task(
    config: SshConfig,
    input: Arc<Mutex<Input>>,
    out: Output,
    closed: Arc<AtomicBool>,
) {
    let end = match run_session(&config, &input, &out).await {
        Ok(status) => End::Closed(status),
        Err(end) => end,
    };
    match end {
        End::Closed(Some(code)) if code != 0 => out.notice(&format!(
            "Connection to {} closed (exit status {code}).",
            config.host
        )),
        End::Closed(_) => out.notice(&format!("Connection to {} closed.", config.host)),
        End::Cancelled => out.notice("Connection cancelled."),
        End::Failed(msg) => out.error(&msg),
    }
    closed.store(true, Ordering::Release);
    (out.waker)();
}

async fn run_session(
    config: &SshConfig,
    input: &Arc<Mutex<Input>>,
    out: &Output,
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

    let channel = handle
        .channel_open_session()
        .await
        .map_err(failed("Cannot open a session"))?;
    let mut input = input.lock().await;
    let size = input.size;
    channel
        .request_pty(
            false,
            &config.term_type,
            u32::from(size.cols),
            u32::from(size.rows),
            0,
            0,
            &[],
        )
        .await
        .map_err(failed("PTY request failed"))?;
    channel
        .request_shell(false)
        .await
        .map_err(failed("Shell request failed"))?;

    let mut channel = channel;
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
                Some(Cmd::Close) | None => {
                    let _ = channel.close().await;
                    break;
                }
            },
        }
    }
    let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
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

    if methods.contains(&MethodKind::KeyboardInteractive) {
        match keyboard_interactive_auth(handle, user, input, out).await? {
            Step::Done => return Ok(()),
            Step::Next(m) => methods = m,
        }
    }

    if methods.contains(&MethodKind::Password) {
        for attempt in 0..PASSWORD_ATTEMPTS {
            let password = {
                let mut input = input.lock().await;
                out.text(&format!("{user}@{}'s password: ", config.host));
                input.read_line(out, false).await?
            };
            match handle
                .authenticate_password(user, password)
                .await
                .map_err(failed("Authentication failed"))?
                .into()
            {
                Step::Done => return Ok(()),
                Step::Next(m) => {
                    methods = m;
                    if !methods.contains(&MethodKind::Password) {
                        break;
                    }
                    if attempt + 1 < PASSWORD_ATTEMPTS {
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

async fn keyboard_interactive_auth(
    handle: &mut Handle,
    user: &str,
    input: &Arc<Mutex<Input>>,
    out: &Output,
) -> Result<Step, End> {
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(failed("Authentication failed"))?;
    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(Step::Done),
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods, ..
            } => return Ok(Step::Next(remaining_methods)),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let mut answers = Vec::with_capacity(prompts.len());
                {
                    let mut input = input.lock().await;
                    for text in [name, instructions] {
                        if !text.is_empty() {
                            out.lines(&text);
                            out.text("\r\n");
                        }
                    }
                    for prompt in prompts {
                        out.lines(&prompt.prompt);
                        answers.push(input.read_line(out, prompt.echo).await?);
                    }
                }
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
