//! A pane: one terminal and the connection behind it.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

use przmterm_core::Protocol;
use przmterm_session::buttons::Chunk;
use przmterm_session::vault::VaultEntry;
use przmterm_session::{Session, Target};
use przmterm_term::Terminal;
use przmterm_term::log::TextLog;
use przmterm_transport::modem;
use przmterm_transport::serial::SerialTransport;
use przmterm_transport::ssh::{Credential, SavePassword, SshConfig, SshTransport, Store};
use przmterm_transport::telnet::{TelnetConfig, TelnetTransport};
use przmterm_transport::x11::X11Display;
use przmterm_transport::{Ending, Size, Transport, TransportError, Waker};

use przmterm_script::{Outcome, Request, Script, SessionInfo};
use przmterm_term::plain::PlainText;

use crate::demo::DemoShell;
use crate::keychain::Keychain;
use crate::view::TerminalView;

/// What a pane needs from the application.
pub struct Env {
    pub known_hosts: PathBuf,
    pub config_dir: Option<PathBuf>,
    /// Every saved session, for jump hosts named by session.
    pub sessions: Vec<Session>,
    /// The password vault's entries, while it is unlocked.
    pub credentials: Option<Arc<Vec<VaultEntry>>>,
}

/// The vault entries `session` and its jump hosts log in with.
pub fn vault_entries_used(session: &Session, env: &Env) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = Some(session.clone());
    for _ in 0..=MAX_JUMPS {
        let Some(s) = current.take() else { break };
        if s.protocol != Protocol::Ssh {
            break;
        }
        if let Some(name) = &s.credential
            && !names.contains(name)
        {
            names.push(name.clone());
        }
        current = s.jump_host.as_deref().and_then(|j| find_jump(j, env).ok());
    }
    names
}

/// Jump hosts deeper than this are taken as a loop.
const MAX_JUMPS: usize = 5;

/// The SSH settings of a session, with its jump hosts resolved.
fn ssh_config(session: &Session, env: &Env, depth: usize) -> Result<SshConfig, String> {
    let Target::Network {
        host,
        port,
        username,
    } = &session.target
    else {
        return Err(format!("{} is not an SSH session", session.name));
    };
    let mut config = SshConfig::new(host.clone(), *port, &env.known_hosts);
    config.username = username.clone();
    // A locked vault logs in as if there were no entry: everything is asked.
    if let Some(name) = &session.credential
        && let Some(entries) = &env.credentials
    {
        let entry = entries
            .iter()
            .find(|e| &e.name == name)
            .ok_or_else(|| format!("{}: the vault has no entry named {name}", session.name))?;
        if !entry.username.is_empty() {
            config.username = Some(entry.username.clone());
        }
        let some = |s: &String| (!s.is_empty()).then(|| s.clone());
        config.credential = Some(Credential {
            name: name.clone(),
            password: some(&entry.password),
            passphrase: some(&entry.passphrase),
        });
    }
    config.term_type = session.term_type.clone();
    config.identity_files = session.identity_file.iter().cloned().collect();
    config.debug = session.debug;
    config.keepalive = session.keepalive;
    config.legacy_algorithms = session.legacy_algorithms;
    if session.x11_forwarding && depth == 0 {
        let mut display = X11Display::from_env();
        display.cookie = xauth_cookie(&display.name);
        config.x11 = Some(display);
    }
    // A jump host's own tunnels are not opened.
    config.forwards = if depth == 0 {
        session.tunnels.clone()
    } else {
        Vec::new()
    };
    config.password_store = Some(Store(Arc::new(Keychain)));
    config.save_password = match session.save_password {
        None => SavePassword::Ask,
        Some(true) => SavePassword::Always,
        Some(false) => SavePassword::Never,
    };
    if let Some(jump) = &session.jump_host {
        if depth >= MAX_JUMPS {
            return Err(format!(
                "Too many jump hosts from {} (is there a loop?)",
                session.name
            ));
        }
        let via = find_jump(jump, env)?;
        config.jump = Some(Box::new(ssh_config(&via, env, depth + 1)?));
    }
    Ok(config)
}

/// The real cookie of a local X display, from `xauth` (XQuartz installs it
/// in /opt/X11/bin, which apps started from the Finder do not have in PATH).
fn xauth_cookie(display: &str) -> Option<Vec<u8>> {
    ["xauth", "/opt/X11/bin/xauth"].iter().find_map(|xauth| {
        let out = std::process::Command::new(xauth)
            .args(["list", display])
            .output()
            .ok()?;
        przmterm_transport::x11::cookie_from_xauth_list(&String::from_utf8_lossy(&out.stdout))
    })
}

/// A jump host: a saved session by name, else `user@host:port`.
fn find_jump(name: &str, env: &Env) -> Result<Session, String> {
    let name = name.trim();
    if let Some(s) = env
        .sessions
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name) && s.protocol == Protocol::Ssh)
    {
        return Ok(s.clone());
    }
    Session::quick_connect(name).map_err(|e| format!("Jump host `{name}`: {e}"))
}

/// A plain-text log of everything a pane receives.
pub struct SessionLog {
    file: BufWriter<File>,
    text: TextLog,
    pub path: PathBuf,
}

/// Log files currently open, so two panes never write to the same file.
fn open_logs() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static OPEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    OPEN.get_or_init(Default::default)
}

/// `path`, or `name-2.ext`, `name-3.ext`… if another pane has it open.
fn claim_log_path(path: PathBuf) -> PathBuf {
    let mut open = open_logs().lock().unwrap_or_else(|e| e.into_inner());
    let mut candidate = path.clone();
    let mut n = 2;
    while open.contains(&candidate) {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let name = match path.extension() {
            Some(ext) => format!("{stem}-{n}.{}", ext.to_string_lossy()),
            None => format!("{stem}-{n}"),
        };
        candidate = path.with_file_name(name);
        n += 1;
    }
    open.insert(candidate.clone());
    candidate
}

impl SessionLog {
    fn start(path: PathBuf, title: &str) -> std::io::Result<Self> {
        let path = claim_log_path(path);
        let opened = Self::open(path.clone(), title);
        if opened.is_err() {
            open_logs()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&path);
        }
        opened
    }

    fn open(path: PathBuf, title: &str) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let mut log = Self {
            file: BufWriter::new(file),
            text: TextLog::new(),
            path,
        };
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        writeln!(
            log.file,
            "===== PrzmTerm log: {title} — started {now} ====="
        )?;
        log.file.flush()?;
        Ok(log)
    }

    fn write(&mut self, bytes: &[u8]) {
        let text = self.text.feed(bytes);
        if !text.is_empty() {
            let _ = self.file.write_all(text.as_bytes());
            let _ = self.file.flush();
        }
    }
}

impl Drop for SessionLog {
    fn drop(&mut self) {
        let rest = self.text.finish();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = self.file.write_all(rest.as_bytes());
        let _ = writeln!(self.file, "===== log closed {now} =====");
        let _ = self.file.flush();
        open_logs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.path);
    }
}

/// One terminal and the connection behind it.
pub struct Pane {
    pub term: Terminal,
    pub transport: Box<dyn Transport>,
    pub view: TerminalView,
    /// Shown in the status bar, e.g. `ssh admin@10.0.0.1:22`.
    pub label: String,
    /// The session to reconnect to, if any.
    pub origin: Option<Session>,
    pub log: Option<SessionLog>,
    /// The file of the log running now or, once stopped, of the last one.
    pub log_path: Option<PathBuf>,
    closed_announced: bool,
    /// A pending automatic reconnect.
    retry: Option<Retry>,
    /// Failed attempts in a row, for the back-off.
    attempts: usize,
    /// Watches the output for the session's triggers.
    triggers: Option<przmterm_term::triggers::TriggerEngine>,
    /// Macro text still to send, and when the next piece is due (`\p`).
    queue: VecDeque<Chunk>,
    queue_at: Option<std::time::Instant>,
    /// The script driving this pane, if one runs.
    script: Option<Script>,
    /// Output as plain text, for the script.
    plain: PlainText,
    /// An Xmodem/Ymodem/Zmodem transfer: while it runs, the session's
    /// output goes to it instead of the terminal.
    transfer: Option<Transfer>,
    /// What transfers need from the application.
    transfer_events: Vec<TransferEvent>,
    /// The other side runs `rz` and we asked for files already.
    upload_asked: bool,
    /// Files for the `rz` we just typed, sent when it starts.
    queued_upload: Option<Vec<std::path::PathBuf>>,
    /// Where the `sz` we just typed saves to.
    queued_download: Option<std::path::PathBuf>,
    /// An Xmodem/Ymodem receive that starts once the typed `sx`/`sb` runs.
    delayed: Option<(std::time::Instant, modem::Kind, std::path::PathBuf, String)>,
    /// After typing `rz`/`sz`/…: the tool, until when to watch, and the
    /// output since (to see "command not found").
    watching: Option<(String, std::time::Instant, String)>,
}

pub struct Transfer {
    engine: Box<dyn modem::Engine>,
    /// The protocol, for messages: `Zmodem`, `SCP`…
    pub label: &'static str,
    pub sending: bool,
    /// Runs through the terminal (X/Y/Zmodem): the session's output goes
    /// to it and typing waits. SCP has a channel of its own.
    in_band: bool,
}

/// An SCP transfer seen as an [`modem::Engine`], so it shows and ends like
/// the others.
struct ScpEngine {
    transfer: przmterm_transport::scp::ScpTransfer,
    progress: modem::Progress,
    status: modem::Status,
}

impl modem::Engine for ScpEngine {
    fn input(&mut self, _: &[u8], _: std::time::Instant) {}
    fn output(&mut self) -> Vec<u8> {
        Vec::new()
    }
    fn tick(&mut self, _: std::time::Instant) {
        use przmterm_transport::scp::ScpEvent;
        for event in self.transfer.poll() {
            match event {
                ScpEvent::Progress(p) => self.progress = p,
                ScpEvent::Done(Ok(files)) => self.status = modem::Status::Finished(files),
                ScpEvent::Done(Err(e)) if e == "cancelled" => {
                    self.status = modem::Status::Cancelled;
                }
                ScpEvent::Done(Err(e)) => self.status = modem::Status::Failed(e),
            }
        }
    }
    fn progress(&self) -> modem::Progress {
        self.progress.clone()
    }
    fn status(&self) -> modem::Status {
        self.status.clone()
    }
    fn cancel(&mut self) {
        self.transfer.cancel();
    }
}

/// What a transfer needs from the application.
pub enum TransferEvent {
    /// The other side runs `rz`: ask which files to send.
    UploadWanted,
    /// A transfer ended; a message for the status bar.
    Ended(String),
}

/// What a running script needs from the application.
pub enum ScriptEvent {
    /// Ask the user, then answer on `reply` (`None` stops the script).
    Prompt {
        script: String,
        question: String,
        secret: bool,
        reply: std::sync::mpsc::Sender<Option<String>>,
    },
    /// The script ended; a message for the status bar.
    Ended(String),
}

/// When the next automatic reconnect happens.
#[derive(Debug, Clone, Copy)]
pub struct Retry {
    pub at: std::time::Instant,
}

/// Seconds before each automatic reconnect attempt; the last one repeats.
const RETRY_DELAYS: [u64; 5] = [2, 5, 10, 20, 30];

impl Pane {
    fn with(
        term: Terminal,
        transport: Box<dyn Transport>,
        view: TerminalView,
        label: String,
        origin: Option<Session>,
    ) -> Self {
        Self {
            term,
            transport,
            view,
            label,
            origin,
            log: None,
            log_path: None,
            closed_announced: false,
            retry: None,
            attempts: 0,
            triggers: None,
            queue: VecDeque::new(),
            queue_at: None,
            script: None,
            plain: PlainText::new(),
            transfer: None,
            transfer_events: Vec::new(),
            upload_asked: false,
            queued_upload: None,
            queued_download: None,
            delayed: None,
            watching: None,
        }
    }

    /// Types the command that starts the other side, and watches whether
    /// it runs.
    fn type_command(&mut self, cmd: &str) {
        let _ = self.transport.write(format!("{cmd}\r").as_bytes());
        let tool = cmd.split_whitespace().next().unwrap_or(cmd).to_string();
        self.watching = Some((
            tool,
            std::time::Instant::now() + std::time::Duration::from_secs(15),
            String::new(),
        ));
    }

    /// Looks at the output after a typed command: when it failed, nothing
    /// is started (and nothing is sent to the shell).
    fn watch_start(&mut self, data: &[u8]) {
        let Some((tool, until, seen)) = &mut self.watching else {
            return;
        };
        seen.push_str(&String::from_utf8_lossy(data));
        if seen.len() > 4096 {
            let cut = seen.len() - 2048;
            let cut = (cut..seen.len())
                .find(|&i| seen.is_char_boundary(i))
                .unwrap_or(0);
            seen.drain(..cut);
        }
        // Skip the echo of the command itself.
        let after = seen.split_once('\n').map_or("", |(_, rest)| rest);
        let failed = crate::transfer::start_failed(after);
        let late = std::time::Instant::now() > *until;
        let tool = tool.clone();
        if failed {
            self.abandon(&crate::transfer::not_started_message(&tool));
        } else if late {
            self.watching = None;
            if self.queued_upload.is_some() || self.queued_download.is_some() {
                self.abandon(&crate::transfer::not_started_message(&tool));
            }
        }
    }

    /// Gives up on a transfer that never started, without sending anything.
    fn abandon(&mut self, message: &str) {
        self.watching = None;
        self.queued_upload = None;
        self.queued_download = None;
        self.delayed = None;
        if self.transfer.take().is_some() {
            self.upload_asked = false;
        }
        self.term
            .feed(format!("\r\n\x1b[33m[{message}]\x1b[0m\r\n").as_bytes());
        self.transfer_events
            .push(TransferEvent::Ended(message.to_string()));
    }

    /// Starts what the transfer dialog asked for.
    pub fn begin(&mut self, start: crate::transfer::Start, waker: Waker) {
        use crate::transfer::Start;
        let now = std::time::Instant::now();
        match start {
            Start::Scp(request) => self.start_scp(request, waker),
            // Zmodem's own start (from rz/sz) begins the transfer, so the
            // shell never sees protocol bytes.
            Start::Send {
                kind: modem::Kind::Zmodem,
                files,
                command: Some(cmd),
            } => {
                self.queued_upload = Some(files);
                self.type_command(&cmd);
            }
            Start::Send {
                kind,
                files,
                command,
            } => self.start_send(kind, files, command),
            Start::Receive {
                kind: modem::Kind::Zmodem,
                dir,
                command: Some(cmd),
                ..
            } => {
                self.queued_download = Some(dir);
                self.type_command(&cmd);
            }
            Start::Receive {
                kind,
                dir,
                name,
                command: Some(cmd),
            } => {
                self.type_command(&cmd);
                // Time for the command to fail before we ask it for data.
                self.delayed = Some((
                    now + std::time::Duration::from_millis(1500),
                    kind,
                    dir,
                    name,
                ));
            }
            Start::Receive {
                kind, dir, name, ..
            } => self.start_receive(kind, dir, &name),
        }
    }

    /// Starts sending `files` with `kind`. With `command`, it is typed
    /// first to start the receiver on a Unix host (`rz`, `rb`, `rx NAME`).
    pub fn start_send(
        &mut self,
        kind: modem::Kind,
        files: Vec<std::path::PathBuf>,
        command: Option<String>,
    ) {
        if let Some(cmd) = command {
            self.type_command(&cmd);
        }
        match modem::send(kind, files, std::time::Instant::now()) {
            Ok(engine) => {
                self.term.feed(
                    format!("\r\n\x1b[2m[{} upload started]\x1b[0m\r\n", kind.label()).as_bytes(),
                );
                self.transfer = Some(Transfer {
                    engine,
                    label: kind.label(),
                    sending: true,
                    in_band: true,
                });
            }
            Err(e) => self
                .transfer_events
                .push(TransferEvent::Ended(format!("Cannot send: {e}"))),
        }
    }

    /// Starts an SCP transfer on a channel of its own.
    fn start_scp(&mut self, request: przmterm_transport::scp::Request, waker: Waker) {
        use przmterm_transport::scp::Direction;
        let sending = matches!(request.direction, Direction::Upload { .. });
        let who = request
            .shell
            .as_ref()
            .map(|s| format!(" in {s}"))
            .unwrap_or_default();
        match self.transport.scp(request, waker) {
            // The terminal is left alone: the shell keeps its prompt, and
            // the status bar shows the progress and the end.
            Some(transfer) => {
                tracing::info!("SCP started{who}");
                self.transfer = Some(Transfer {
                    engine: Box::new(ScpEngine {
                        transfer,
                        progress: modem::Progress::default(),
                        status: modem::Status::Running,
                    }),
                    label: "SCP",
                    sending,
                    in_band: false,
                });
            }
            None => self.transfer_events.push(TransferEvent::Ended(
                "SCP needs a connected SSH session.".into(),
            )),
        }
    }

    /// Starts receiving into `dir` (Xmodem names its file `name`).
    pub fn start_receive(&mut self, kind: modem::Kind, dir: std::path::PathBuf, name: &str) {
        self.term
            .feed(format!("\r\n\x1b[2m[{} download started]\x1b[0m\r\n", kind.label()).as_bytes());
        self.transfer = Some(Transfer {
            engine: modem::receive(kind, dir, name, std::time::Instant::now()),
            label: kind.label(),
            sending: false,
            in_band: true,
        });
    }

    /// A transfer runs through the terminal (X/Y/Zmodem).
    fn in_band_transfer(&self) -> bool {
        self.transfer.as_ref().is_some_and(|t| t.in_band)
    }

    /// The running transfer and how far it is.
    pub fn transfer(&self) -> Option<(&Transfer, modem::Progress)> {
        self.transfer.as_ref().map(|t| (t, t.engine.progress()))
    }

    pub fn cancel_transfer(&mut self) {
        if let Some(t) = &mut self.transfer {
            t.engine.cancel();
        }
    }

    /// Answers `rz` that there is nothing to send.
    pub fn refuse_upload(&mut self) {
        let mut cancel = vec![0x18u8; 8];
        cancel.extend([0x08; 8]);
        let _ = self.transport.write(&cancel);
        self.upload_asked = false;
    }

    pub fn take_transfer_events(&mut self) -> Vec<TransferEvent> {
        std::mem::take(&mut self.transfer_events)
    }

    /// Lets the transfer run: its timeouts, what it sends, and its end.
    fn run_transfer(&mut self) {
        let Some(t) = &mut self.transfer else {
            return;
        };
        t.engine.tick(std::time::Instant::now());
        let out = t.engine.output();
        if !out.is_empty() {
            match self.transport.write(&out) {
                Ok(()) => {}
                Err(_) => t.engine.cancel(),
            }
        }
        if self.transport.is_closed() && t.engine.status() == modem::Status::Running {
            t.engine.cancel();
        }
        let status = t.engine.status();
        if status == modem::Status::Running {
            return;
        }
        let (kind, sending) = (t.label, t.sending);
        let names = |files: &[std::path::PathBuf]| {
            files
                .iter()
                .filter_map(|f| f.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let message = match status {
            modem::Status::Finished(files) if sending => {
                format!("{kind}: sent {} file(s): {}", files.len(), names(&files))
            }
            modem::Status::Finished(files) => {
                format!(
                    "{kind}: received {} file(s): {}",
                    files.len(),
                    names(&files)
                )
            }
            modem::Status::Cancelled => format!("{kind}: transfer cancelled"),
            modem::Status::Failed(e) => format!("{kind}: transfer failed: {e}"),
            modem::Status::Running => unreachable!(),
        };
        if t.in_band {
            self.term
                .feed(format!("\r\n\x1b[2m[{message}]\x1b[0m\r\n").as_bytes());
        }
        self.transfer = None;
        self.upload_asked = false;
        self.watching = None;
        self.transfer_events.push(TransferEvent::Ended(message));
    }

    pub fn demo() -> Self {
        Self::with(
            Terminal::new(80, 24),
            Box::new(DemoShell::new()),
            TerminalView::default(),
            "demo shell".into(),
            None,
        )
    }

    pub fn open(session: &Session, env: &Env, size: Size, waker: Waker) -> Self {
        let transport = connect(session, env, size, waker.clone());
        let mut pane = Self::with(
            Terminal::with_scrollback(
                usize::from(size.cols),
                usize::from(size.rows),
                session.scrollback,
            ),
            transport,
            TerminalView::for_session(session),
            format!("{} {}", session.protocol, session.display_target()),
            Some(session.clone()),
        );
        pane.load_keywords(session, env);
        pane.load_triggers(session);
        if session.log_enabled {
            pane.start_log(env);
        }
        pane.start_logon_script(env, waker);
        pane
    }

    /// Runs the session's logon script, if it has one.
    fn start_logon_script(&mut self, env: &Env, waker: Waker) {
        if let Some(name) = self.origin.as_ref().and_then(|s| s.logon_script.clone()) {
            let path = przmterm_session::script_path(env.config_dir.as_deref(), &name);
            self.run_script(&path, waker);
        }
    }

    /// Runs the script at `path` on this pane, stopping one already running.
    pub fn run_script(&mut self, path: &std::path::Path, waker: Waker) {
        self.stop_script();
        let info = match &self.origin {
            Some(s) => SessionInfo {
                name: s.name.clone(),
                host: match &s.target {
                    Target::Network { host, .. } => host.clone(),
                    Target::Serial(line) => line.port.clone(),
                    Target::Local => String::new(),
                },
                username: match &s.target {
                    Target::Network { username, .. } => username.clone().unwrap_or_default(),
                    _ => String::new(),
                },
            },
            None => SessionInfo {
                name: self.label.clone(),
                ..SessionInfo::default()
            },
        };
        let script = Script::run_file(path, info, waker);
        self.term
            .feed(format!("\x1b[2m[Script {} started]\x1b[0m\r\n", script.name).as_bytes());
        if self.transport.is_closed() {
            script.session_closed();
        }
        self.plain.reset();
        self.script = Some(script);
    }

    /// The name of the running script, if any.
    pub fn script_name(&self) -> Option<&str> {
        self.script.as_ref().map(|s| s.name.as_str())
    }

    pub fn stop_script(&mut self) {
        if let Some(script) = &self.script {
            script.cancel();
        }
    }

    /// Carries out what the running script asked; prompts and the end are
    /// handed to the application.
    pub fn poll_script(&mut self) -> Vec<ScriptEvent> {
        let Some(script) = &mut self.script else {
            return Vec::new();
        };
        let name = script.name.clone();
        let mut events = Vec::new();
        let mut ended = false;
        for request in script.poll() {
            match request {
                Request::Send(text) => {
                    self.term.screen_mut().reset_display();
                    match self.transport.write(text.as_bytes()) {
                        Ok(()) | Err(TransportError::Closed) => {}
                        Err(e) => tracing::warn!("transport write failed: {e}"),
                    }
                }
                Request::Log(text) => {
                    let text = text.replace('\n', "\r\n");
                    // On a line of its own, not after the remote prompt.
                    let start = if self.term.screen().cursor().col > 0 {
                        "\r\n"
                    } else {
                        ""
                    };
                    self.term
                        .feed(format!("{start}\x1b[36m[{name}] {text}\x1b[0m\r\n").as_bytes());
                }
                Request::Prompt {
                    question,
                    secret,
                    reply,
                } => events.push(ScriptEvent::Prompt {
                    script: name.clone(),
                    question,
                    secret,
                    reply,
                }),
                Request::Done(outcome) => {
                    ended = true;
                    let message = match outcome {
                        Outcome::Finished => format!("Script {name} finished."),
                        Outcome::Cancelled => format!("Script {name} stopped."),
                        Outcome::Failed(e) => {
                            let shown = e.replace('\n', "\r\n");
                            self.term.feed(
                                format!("\x1b[31m[Script {name}: {shown}]\x1b[0m\r\n").as_bytes(),
                            );
                            format!("Script {name} failed: {e}")
                        }
                    };
                    events.push(ScriptEvent::Ended(message));
                }
            }
        }
        if ended {
            self.script = None;
        }
        events
    }

    /// Arms the session's triggers; problems are shown in the terminal.
    fn load_triggers(&mut self, session: &Session) {
        if session.triggers.is_empty() {
            self.triggers = None;
            return;
        }
        let (engine, problems) = przmterm_term::triggers::TriggerEngine::new(&session.triggers);
        for p in problems {
            self.term
                .feed(format!("\x1b[33m[Triggers: {p}]\x1b[0m\r\n").as_bytes());
        }
        self.triggers = (!engine.is_empty()).then_some(engine);
    }

    /// Reads the session's keyword set again, after it was edited.
    pub fn reload_keywords(&mut self, env: &Env) {
        self.view.highlighter = None;
        if let Some(session) = self.origin.clone() {
            self.load_keywords(&session, env);
        }
    }

    /// Sets up highlighting with the session's keyword set; problems with
    /// the set are shown in the terminal.
    fn load_keywords(&mut self, session: &Session, env: &Env) {
        let (Some(name), Some(dir)) = (&session.keywords, &env.config_dir) else {
            return;
        };
        match przmterm_session::read_keyword_set(dir, name) {
            Ok(set) => {
                let (h, problems) = przmterm_term::highlight::Highlighter::new(&set);
                for p in problems {
                    self.term
                        .feed(format!("\x1b[33m[Keywords {name}: {p}]\x1b[0m\r\n").as_bytes());
                }
                self.view.highlighter = Some(std::rc::Rc::new(h));
            }
            Err(e) => self
                .term
                .feed(format!("\x1b[33m[Keywords {name}: {e}]\x1b[0m\r\n").as_bytes()),
        }
    }

    /// Queues macro text for this pane; pauses are kept.
    pub fn enqueue(&mut self, chunks: &[Chunk]) {
        self.queue.extend(chunks.iter().cloned());
    }

    /// Sends what is due from the macro queue. Returns when to come back for
    /// the rest, if a pause is pending.
    pub fn run_queue(&mut self, now: std::time::Instant) -> Option<std::time::Instant> {
        if let Some(at) = self.queue_at {
            if at > now {
                return Some(at);
            }
            self.queue_at = None;
        }
        while let Some(chunk) = self.queue.pop_front() {
            match chunk {
                Chunk::Bytes(bytes) => {
                    self.term.screen_mut().reset_display();
                    self.send(&bytes);
                }
                Chunk::Pause(d) => {
                    let at = now + d;
                    self.queue_at = Some(at);
                    return Some(at);
                }
            }
        }
        None
    }

    /// Connects again on the same screen, keeping the scrollback and the log.
    pub fn reconnect(&mut self, env: &Env, waker: Waker) {
        let Some(session) = self.origin.clone() else {
            return;
        };
        self.retry = None;
        self.term.feed(b"\r\n");
        let _ = self.transport.close();
        self.transport = connect(&session, env, self.size(), waker.clone());
        self.closed_announced = false;
        if let Some(engine) = &mut self.triggers {
            engine.reset();
        }
        self.start_logon_script(env, waker);
    }

    /// The pending automatic reconnect, if any.
    pub fn retry(&self) -> Option<Retry> {
        self.retry
    }

    /// Stops a pending automatic reconnect (Ctrl+C while waiting).
    pub fn cancel_retry(&mut self) {
        if self.retry.take().is_some() {
            self.term
                .feed(b"\x1b[2mAutomatic reconnect stopped. Press Enter to reconnect.\x1b[0m\r\n");
        }
    }

    /// After the connection ended: schedule a reconnect if the session wants
    /// one and retrying can help, else offer Enter.
    fn after_close(&mut self) {
        let Some(session) = &self.origin else {
            return;
        };
        let ending = self.transport.ending();
        let retry =
            session.auto_reconnect && matches!(ending, Some(Ending::Lost | Ending::Unreachable));
        if !retry {
            self.term
                .feed(b"\x1b[2mPress Enter to reconnect.\x1b[0m\r\n");
            return;
        }
        // A connection that was up starts the back-off again.
        if ending == Some(Ending::Lost) {
            self.attempts = 0;
        }
        let delay = RETRY_DELAYS[self.attempts.min(RETRY_DELAYS.len() - 1)];
        self.attempts += 1;
        self.retry = Some(Retry {
            at: std::time::Instant::now() + std::time::Duration::from_secs(delay),
        });
        self.term.feed(
            format!(
                "\x1b[2mReconnecting in {delay} s (attempt {}). Enter: now, Ctrl+C: stop.\x1b[0m\r\n",
                self.attempts
            )
            .as_bytes(),
        );
    }

    /// Starts logging to the session's log file (or the default one).
    pub fn start_log(&mut self, env: &Env) {
        let session = self
            .origin
            .clone()
            .unwrap_or_else(|| Session::quick_connect("demo").expect("valid quick-connect"));
        let path = session.log_file(env.config_dir.as_deref(), &chrono::Local::now());
        match SessionLog::start(path.clone(), &session.name) {
            Ok(log) => {
                self.log_path = Some(log.path.clone());
                self.log = Some(log);
            }
            Err(e) => self
                .term
                .feed(format!("\x1b[31m[Cannot write the log file: {e}]\x1b[0m\r\n").as_bytes()),
        }
    }

    pub fn stop_log(&mut self) {
        self.log = None;
    }

    pub fn size(&self) -> Size {
        let s = self.term.screen();
        Size {
            cols: s.cols().min(usize::from(u16::MAX)) as u16,
            rows: s.rows().min(usize::from(u16::MAX)) as u16,
        }
    }

    /// Moves bytes transport → emulator and emulator replies → transport.
    /// Returns true if anything arrived.
    pub fn pump(&mut self) -> bool {
        let mut buf = [0u8; 16 * 1024];
        let mut got = false;
        loop {
            match self.transport.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    got = true;
                    let now = std::time::Instant::now();
                    if let Some(t) = self.transfer.as_mut().filter(|t| t.in_band) {
                        t.engine.input(&buf[..n], now);
                        // Data moving: the command ran. File contents must
                        // not be read as an error message.
                        if t.engine.progress().done > 0 {
                            self.watching = None;
                        }
                        if self.watching.is_some() {
                            self.watch_start(&buf[..n]);
                        }
                        continue;
                    }
                    // `sz` or `rz` started on the other side.
                    let mut data = &buf[..n];
                    let start = modem::find_zmodem(data);
                    let tail = start.map(|(at, _)| data[at..].to_vec());
                    if let Some((at, _)) = start {
                        data = &buf[..at];
                    }
                    self.term.feed(data);
                    if let Some(log) = &mut self.log {
                        log.write(&buf[..n]);
                    }
                    if let Some(engine) = &mut self.triggers {
                        for send in engine.feed(&buf[..n]) {
                            self.queue
                                .extend(przmterm_session::buttons::parse_send(&send));
                        }
                    }
                    if let Some(script) = &self.script {
                        script.feed(&self.plain.text(data));
                    }
                    if start.is_some() {
                        self.watching = None;
                    } else if self.watching.is_some() {
                        // After the terminal shows the error, so the note
                        // comes below it.
                        self.watch_start(&buf[..n]);
                    }
                    match (start, tail) {
                        (Some((_, modem::ZmodemStart::Download)), Some(tail)) => {
                            let dir = self
                                .queued_download
                                .take()
                                .unwrap_or_else(crate::transfer::downloads);
                            self.start_receive(modem::Kind::Zmodem, dir, "");
                            if let Some(t) = &mut self.transfer {
                                t.engine.input(&tail, now);
                            }
                        }
                        (Some((_, modem::ZmodemStart::Upload)), Some(tail)) => {
                            if let Some(files) = self.queued_upload.take() {
                                self.start_send(modem::Kind::Zmodem, files, None);
                                if let Some(t) = &mut self.transfer {
                                    t.engine.input(&tail, now);
                                }
                            } else if !self.upload_asked {
                                self.upload_asked = true;
                                self.transfer_events.push(TransferEvent::UploadWanted);
                            }
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    tracing::warn!("transport read failed: {e}");
                    break;
                }
            }
        }
        let replies = self.term.take_output();
        if !replies.is_empty() && !self.in_band_transfer() {
            self.send(&replies);
        }
        if self.watching.is_some() {
            self.watch_start(&[]);
        }
        if let Some((at, kind, dir, name)) = self.delayed.clone()
            && std::time::Instant::now() >= at
        {
            self.delayed = None;
            self.start_receive(kind, dir, &name);
        }
        if self.transfer.is_some() || self.delayed.is_some() || self.watching.is_some() {
            self.run_transfer();
            // Keep going while it runs.
            got = true;
        }
        if self.transport.is_closed() && !self.closed_announced {
            self.closed_announced = true;
            if let Some(script) = &self.script {
                script.session_closed();
            }
            self.after_close();
            got = true;
        }
        got
    }

    pub fn send(&mut self, bytes: &[u8]) {
        // Typing during a transfer would break it; Ctrl+C stops it.
        if self.in_band_transfer() {
            if bytes.contains(&3) {
                self.cancel_transfer();
            }
            return;
        }
        match self.transport.write(bytes) {
            Ok(()) | Err(TransportError::Closed) => {}
            Err(e) => tracing::warn!("transport write failed: {e}"),
        }
    }
}

/// Starts the connection behind a session.
fn connect(session: &Session, env: &Env, size: Size, waker: Waker) -> Box<dyn Transport> {
    match (&session.protocol, &session.target) {
        (Protocol::Ssh, Target::Network { .. }) => match ssh_config(session, env, 0) {
            Ok(config) => Box::new(SshTransport::connect(config, size, waker)),
            Err(e) => message(&format!("\x1b[31m{e}\x1b[0m\r\n")),
        },
        (Protocol::Telnet, Target::Network { host, port, .. }) => {
            let mut config = TelnetConfig::new(host.clone(), *port);
            config.term_type = session.term_type.clone();
            config.keepalive = session.keepalive;
            Box::new(TelnetTransport::connect(config, size, waker))
        }
        (Protocol::Serial, Target::Serial(line)) => {
            Box::new(SerialTransport::open(line.clone(), waker))
        }
        (protocol, _) => message(&format!(
            "\x1b[33m{protocol} sessions arrive in a later version of PrzmTerm.\x1b[0m\r\n"
        )),
    }
}

/// A transport that only shows `text` and is already closed.
fn message(text: &str) -> Box<dyn Transport> {
    Box::new(Message(text.as_bytes().to_vec().into()))
}

struct Message(VecDeque<u8>);

impl Transport for Message {
    fn protocol(&self) -> Protocol {
        Protocol::Local
    }
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let n = buf.len().min(self.0.len());
        for (slot, b) in buf.iter_mut().zip(self.0.drain(..n)) {
            *slot = b;
        }
        Ok(n)
    }
    fn write(&mut self, _: &[u8]) -> Result<(), TransportError> {
        Err(TransportError::Closed)
    }
    fn resize(&mut self, _: Size) -> Result<(), TransportError> {
        Ok(())
    }
    fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
    fn is_closed(&self) -> bool {
        true
    }
}

impl Pane {
    /// Tab and window title: what the remote side set, else the session name.
    pub fn title(&self) -> String {
        match self.term.screen().title() {
            "" => self
                .origin
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_else(|| self.label.clone()),
            t => t.to_string(),
        }
    }

    /// A new pane on the same session: another channel on the live
    /// connection when possible (no new login, so no 2FA prompt), else a
    /// fresh connection. Without a session, a demo shell.
    pub fn clone_session(&self, env: &Env, waker: Waker) -> Self {
        let Some(session) = &self.origin else {
            return Pane::demo();
        };
        let size = self.size();
        if let (Target::Serial(line), false) = (&session.target, self.transport.is_closed()) {
            // A port opens once: a second pane would only get "busy".
            let mut pane = Pane::demo();
            pane.transport = message(&format!(
                "\x1b[33m{}\r\nis open in another pane.\x1b[0m\r\n",
                line.port
            ));
            pane.label = self.label.clone();
            return pane;
        }
        let Some(transport) = (!self.transport.is_closed())
            .then(|| self.transport.duplicate(size, waker.clone()))
            .flatten()
        else {
            return Pane::open(session, env, size, waker);
        };
        let mut pane = Self::with(
            Terminal::with_scrollback(
                usize::from(size.cols),
                usize::from(size.rows),
                session.scrollback,
            ),
            transport,
            TerminalView::for_session(session),
            self.label.clone(),
            Some(session.clone()),
        );
        pane.load_triggers(session);
        pane.view.highlighter = self.view.highlighter.clone();
        // The clone looks like this pane, with any change made in Font….
        pane.view.look_like(&self.view);
        if session.log_enabled {
            pane.start_log(env);
        }
        pane
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transport that has already ended the given way.
    struct Ended(Ending);

    impl Transport for Ended {
        fn protocol(&self) -> Protocol {
            Protocol::Ssh
        }
        fn read(&mut self, _: &mut [u8]) -> Result<usize, TransportError> {
            Ok(0)
        }
        fn write(&mut self, _: &[u8]) -> Result<(), TransportError> {
            Err(TransportError::Closed)
        }
        fn resize(&mut self, _: Size) -> Result<(), TransportError> {
            Ok(())
        }
        fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
        fn is_closed(&self) -> bool {
            true
        }
        fn ending(&self) -> Option<Ending> {
            Some(self.0)
        }
    }

    fn ended_pane(ending: Ending, auto_reconnect: bool) -> Pane {
        let mut session = Session::quick_connect("10.0.0.1").unwrap();
        session.auto_reconnect = auto_reconnect;
        let mut pane = Pane::demo();
        pane.origin = Some(session);
        pane.transport = Box::new(Ended(ending));
        pane
    }

    fn session(toml: &str, name: &str) -> Session {
        let settings = przmterm_session::SessionSettings::from_toml(toml).unwrap();
        Session::resolve(name, Vec::new(), format!("{name}.toml"), &settings).unwrap()
    }

    #[test]
    fn vault_entries_fill_in_the_login_of_sessions_and_jump_hosts() {
        let bastion = session("host = 'bastion.example'\ncredential = 'Ops'", "Bastion");
        let db = session(
            "host = '10.0.0.5'\nusername = 'local'\njump_host = 'bastion'\ncredential = 'DB'",
            "db",
        );
        let entry = |name: &str, user: &str, password: &str| VaultEntry {
            name: name.into(),
            username: user.into(),
            password: password.into(),
            passphrase: String::new(),
        };
        let mut env = Env {
            known_hosts: PathBuf::from("/tmp/kh"),
            config_dir: None,
            sessions: vec![bastion, db.clone()],
            credentials: None,
        };
        assert_eq!(vault_entries_used(&db, &env), ["DB", "Ops"]);

        // Locked: the session's own user name, everything else asked.
        let config = ssh_config(&db, &env, 0).unwrap();
        assert_eq!(config.username.as_deref(), Some("local"));
        assert!(config.credential.is_none());

        env.credentials = Some(Arc::new(vec![
            entry("DB", "dba", "pw1"),
            entry("Ops", "", "pw2"),
        ]));
        let config = ssh_config(&db, &env, 0).unwrap();
        assert_eq!(
            config.username.as_deref(),
            Some("dba"),
            "the entry's user wins"
        );
        let c = config.credential.unwrap();
        assert_eq!(
            (c.name.as_str(), c.password.as_deref()),
            ("DB", Some("pw1"))
        );
        assert!(c.passphrase.is_none());
        let jump = config.jump.unwrap();
        assert_eq!(
            jump.username, None,
            "an entry without a user name asks for it"
        );
        assert_eq!(jump.credential.unwrap().password.as_deref(), Some("pw2"));

        env.credentials = Some(Arc::new(vec![entry("Ops", "", "pw2")]));
        let err = ssh_config(&db, &env, 0).unwrap_err();
        assert!(err.contains("no entry named DB"), "{err}");
    }

    #[test]
    fn jump_hosts_resolve_by_name_or_address_and_loops_stop() {
        let bastion = session("host = 'bastion.example'\nusername = 'ops'", "Bastion");
        let db = session("host = '10.0.0.5'\njump_host = 'bastion'", "db");
        let env = Env {
            known_hosts: PathBuf::from("/tmp/kh"),
            config_dir: None,
            credentials: None,
            sessions: vec![bastion, db.clone()],
        };
        let config = ssh_config(&db, &env, 0).unwrap();
        let jump = config.jump.unwrap();
        assert_eq!(jump.host, "bastion.example");
        assert_eq!(jump.username.as_deref(), Some("ops"));

        let direct = session("host = '10.0.0.6'\njump_host = 'me@gw:2200'", "x");
        let jump = ssh_config(&direct, &env, 0).unwrap().jump.unwrap();
        assert_eq!((jump.host.as_str(), jump.port), ("gw", 2200));

        let looped = session("host = 'a'\njump_host = 'loop'", "loop");
        let env = Env {
            sessions: vec![looped.clone()],
            ..env
        };
        assert!(ssh_config(&looped, &env, 0).unwrap_err().contains("loop"));
    }

    #[test]
    fn macro_queue_waits_at_pauses() {
        use przmterm_session::buttons::parse_send;
        let mut pane = Pane::demo();
        pane.enqueue(&parse_send("one\\r\\ptwo\\r"));
        let now = std::time::Instant::now();
        let next = pane.run_queue(now).expect("waits at the pause");
        assert_eq!(pane.queue.len(), 1, "only the part after the pause is left");
        assert_eq!(pane.run_queue(now), Some(next), "not due yet");
        assert_eq!(pane.run_queue(next), None, "the rest went out");
        assert!(pane.queue.is_empty());
    }

    #[test]
    fn retries_only_when_it_can_help() {
        for (ending, auto, retry) in [
            (Ending::Lost, true, true),
            (Ending::Unreachable, true, true),
            (Ending::Clean, true, false),
            (Ending::Failed, true, false),
            (Ending::Cancelled, true, false),
            (Ending::Lost, false, false),
        ] {
            let mut pane = ended_pane(ending, auto);
            pane.pump();
            assert_eq!(pane.retry().is_some(), retry, "{ending:?} auto={auto}");
        }
    }

    #[test]
    fn back_off_grows_and_ctrl_c_stops_it() {
        let mut pane = ended_pane(Ending::Unreachable, true);
        let mut delays = Vec::new();
        for _ in 0..7 {
            pane.closed_announced = false;
            pane.pump();
            let left = pane.retry().unwrap().at - std::time::Instant::now();
            delays.push((left.as_secs_f64()).round() as u64);
        }
        assert_eq!(delays, [2, 5, 10, 20, 30, 30, 30]);
        pane.cancel_retry();
        assert!(pane.retry().is_none());
        // A connection that was up starts again from the shortest delay.
        pane.transport = Box::new(Ended(Ending::Lost));
        pane.closed_announced = false;
        pane.pump();
        let left = pane.retry().unwrap().at - std::time::Instant::now();
        assert_eq!(left.as_secs_f64().round() as u64, 2);
    }

    #[test]
    fn concurrent_logs_get_distinct_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sw-20261101.log");
        let a = SessionLog::start(path.clone(), "sw").unwrap();
        let b = SessionLog::start(path.clone(), "sw").unwrap();
        assert_eq!(a.path, path);
        assert_eq!(b.path, dir.path().join("sw-20261101-2.log"));
        drop(a);
        let c = SessionLog::start(path.clone(), "sw").unwrap();
        assert_eq!(c.path, path, "freed when the first log closes");
        drop((b, c));
    }
}
