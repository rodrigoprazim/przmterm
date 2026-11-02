//! A pane: one terminal and the connection behind it.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use przmterm_core::Protocol;
use przmterm_session::{Session, Target};
use przmterm_term::Terminal;
use przmterm_term::log::TextLog;
use przmterm_transport::ssh::{SavePassword, SshConfig, SshTransport, Store};
use przmterm_transport::{Size, Transport, TransportError, Waker};

use crate::demo::DemoShell;
use crate::keychain::Keychain;
use crate::view::TerminalView;

/// Paths a pane needs from the application.
pub struct Env {
    pub known_hosts: PathBuf,
    pub config_dir: Option<PathBuf>,
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
}

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
        }
    }

    pub fn open(session: &Session, env: &Env, size: Size, waker: Waker) -> Self {
        let known_hosts: &Path = &env.known_hosts;
        let transport: Box<dyn Transport> = match (&session.protocol, &session.target) {
            (
                Protocol::Ssh,
                Target::Network {
                    host,
                    port,
                    username,
                },
            ) => {
                let mut config = SshConfig::new(host.clone(), *port, known_hosts);
                config.username = username.clone();
                config.term_type = session.term_type.clone();
                config.identity_files = session.identity_file.iter().cloned().collect();
                config.password_store = Some(Store(Arc::new(Keychain)));
                config.save_password = match session.save_password {
                    None => SavePassword::Ask,
                    Some(true) => SavePassword::Always,
                    Some(false) => SavePassword::Never,
                };
                Box::new(SshTransport::connect(config, size, waker))
            }
            (protocol, _) => {
                let version = match protocol {
                    Protocol::Telnet | Protocol::Serial => "0.4.0",
                    _ => "a later version",
                };
                message(&format!(
                    "\x1b[33m{protocol} sessions arrive in PrzmTerm {version}.\x1b[0m\r\n"
                ))
            }
        };
        let mut pane = Self {
            term: Terminal::with_scrollback(
                usize::from(size.cols),
                usize::from(size.rows),
                session.scrollback,
            ),
            transport,
            view: TerminalView::default(),
            label: format!("{} {}", session.protocol, session.display_target()),
            origin: Some(session.clone()),
            log: None,
            closed_announced: false,
        };
        if session.log_enabled {
            pane.start_log(env);
        }
        pane
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
            if self.origin.is_some() {
                self.term
                    .feed(b"\x1b[2mPress Enter to reconnect.\x1b[0m\r\n");
            }
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
            view: TerminalView::default(),
            label: self.label.clone(),
            origin: Some(session.clone()),
            log: None,
            closed_announced: false,
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
