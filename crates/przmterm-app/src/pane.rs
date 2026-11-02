//! A pane: one terminal and the connection behind it.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

use przmterm_core::Protocol;
use przmterm_session::{Session, Target};
use przmterm_term::Terminal;
use przmterm_term::log::TextLog;
use przmterm_transport::serial::SerialTransport;
use przmterm_transport::ssh::{SavePassword, SshConfig, SshTransport, Store};
use przmterm_transport::telnet::{TelnetConfig, TelnetTransport};
use przmterm_transport::x11::X11Display;
use przmterm_transport::{Ending, Size, Transport, TransportError, Waker};

use crate::demo::DemoShell;
use crate::keychain::Keychain;
use crate::view::TerminalView;

/// What a pane needs from the application.
pub struct Env {
    pub known_hosts: PathBuf,
    pub config_dir: Option<PathBuf>,
    /// Every saved session, for jump hosts named by session.
    pub sessions: Vec<Session>,
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
    closed_announced: bool,
    /// A pending automatic reconnect.
    retry: Option<Retry>,
    /// Failed attempts in a row, for the back-off.
    attempts: usize,
}

/// When the next automatic reconnect happens.
#[derive(Debug, Clone, Copy)]
pub struct Retry {
    pub at: std::time::Instant,
}

/// Seconds before each automatic reconnect attempt; the last one repeats.
const RETRY_DELAYS: [u64; 5] = [2, 5, 10, 20, 30];

impl Pane {
    pub fn demo() -> Self {
        Self {
            term: Terminal::new(80, 24),
            transport: Box::new(DemoShell::new()),
            view: TerminalView::default(),
            label: "demo shell".into(),
            origin: None,
            log: None,
            closed_announced: false,
            retry: None,
            attempts: 0,
        }
    }

    pub fn open(session: &Session, env: &Env, size: Size, waker: Waker) -> Self {
        let transport = connect(session, env, size, waker);
        let mut pane = Self {
            term: Terminal::with_scrollback(
                usize::from(size.cols),
                usize::from(size.rows),
                session.scrollback,
            ),
            transport,
            view: TerminalView::for_protocol(session.protocol),
            label: format!("{} {}", session.protocol, session.display_target()),
            origin: Some(session.clone()),
            log: None,
            closed_announced: false,
            retry: None,
            attempts: 0,
        };
        if session.log_enabled {
            pane.start_log(env);
        }
        pane
    }

    /// Connects again on the same screen, keeping the scrollback and the log.
    pub fn reconnect(&mut self, env: &Env, waker: Waker) {
        let Some(session) = self.origin.clone() else {
            return;
        };
        self.retry = None;
        self.term.feed(b"\r\n");
        let _ = self.transport.close();
        self.transport = connect(&session, env, self.size(), waker);
        self.closed_announced = false;
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
                self.term
                    .feed(format!("\x1b[2m[Logging to {}]\x1b[0m\r\n", path.display()).as_bytes());
                self.log = Some(log);
            }
            Err(e) => self.term.feed(
                format!("\x1b[31m[Cannot log to {}: {e}]\x1b[0m\r\n", path.display()).as_bytes(),
            ),
        }
    }

    pub fn stop_log(&mut self) {
        if let Some(log) = self.log.take() {
            self.term.feed(
                format!("\x1b[2m[Log saved to {}]\x1b[0m\r\n", log.path.display()).as_bytes(),
            );
        }
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
                    self.term.feed(&buf[..n]);
                    if let Some(log) = &mut self.log {
                        log.write(&buf[..n]);
                    }
                    got = true;
                }
                Err(e) => {
                    tracing::warn!("transport read failed: {e}");
                    break;
                }
            }
        }
        let replies = self.term.take_output();
        if !replies.is_empty() {
            self.send(&replies);
        }
        if self.transport.is_closed() && !self.closed_announced {
            self.closed_announced = true;
            self.after_close();
            got = true;
        }
        got
    }

    pub fn send(&mut self, bytes: &[u8]) {
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
        let mut pane = Self {
            term: Terminal::with_scrollback(
                usize::from(size.cols),
                usize::from(size.rows),
                session.scrollback,
            ),
            transport,
            view: TerminalView::for_protocol(session.protocol),
            label: self.label.clone(),
            origin: Some(session.clone()),
            log: None,
            closed_announced: false,
            retry: None,
            attempts: 0,
        };
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
    fn jump_hosts_resolve_by_name_or_address_and_loops_stop() {
        let bastion = session("host = 'bastion.example'\nusername = 'ops'", "Bastion");
        let db = session("host = '10.0.0.5'\njump_host = 'bastion'", "db");
        let env = Env {
            known_hosts: PathBuf::from("/tmp/kh"),
            config_dir: None,
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
