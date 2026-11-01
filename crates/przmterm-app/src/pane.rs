//! A pane: one terminal and the connection behind it.

use std::collections::VecDeque;
use std::path::Path;

use przmterm_core::Protocol;
use przmterm_session::{Session, Target};
use przmterm_term::Terminal;
use przmterm_transport::ssh::{SshConfig, SshTransport};
use przmterm_transport::{Size, Transport, TransportError, Waker};

use crate::demo::DemoShell;
use crate::view::TerminalView;

/// One terminal and the connection behind it.
pub struct Pane {
    pub term: Terminal,
    pub transport: Box<dyn Transport>,
    pub view: TerminalView,
    /// Shown in the status bar, e.g. `ssh admin@10.0.0.1:22`.
    pub label: String,
    /// The session to reconnect to, if any.
    pub origin: Option<Session>,
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
            closed_announced: false,
        }
    }

    pub fn open(session: &Session, known_hosts: &Path, size: Size, waker: Waker) -> Self {
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
        Self {
            term: Terminal::with_scrollback(
                usize::from(size.cols),
                usize::from(size.rows),
                session.scrollback,
            ),
            transport,
            view: TerminalView::default(),
            label: format!("{} {}", session.protocol, session.display_target()),
            origin: Some(session.clone()),
            closed_announced: false,
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

    /// A new pane connected to the same session (or a fresh demo shell).
    pub fn clone_session(&self, known_hosts: &Path, waker: Waker) -> Self {
        match &self.origin {
            Some(session) => Pane::open(session, known_hosts, self.size(), waker),
            None => Pane::demo(),
        }
    }
}
