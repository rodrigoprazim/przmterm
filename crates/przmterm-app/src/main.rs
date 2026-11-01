//! PrzmTerm desktop application.

mod demo;
mod input;
mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;
use przmterm_core::Protocol;
use przmterm_session::{Folder, Session, SessionTree, Target};
use przmterm_term::Terminal;
use przmterm_transport::ssh::{SshConfig, SshTransport};
use przmterm_transport::{Size, Transport, TransportError, Waker};

use crate::demo::DemoShell;
use crate::view::TerminalView;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "przmterm=info,warn".into()),
        )
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PrzmTerm")
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([480.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "PrzmTerm",
        options,
        Box::new(|_cc| Ok(Box::new(PrzmApp::new()))),
    )
}

/// One terminal and the connection behind it.
struct Tab {
    term: Terminal,
    transport: Box<dyn Transport>,
    view: TerminalView,
    /// Shown in the status bar, e.g. `ssh admin@10.0.0.1:22`.
    label: String,
    /// The session to reconnect to, if any.
    origin: Option<Session>,
    closed_announced: bool,
}

impl Tab {
    fn demo() -> Self {
        Self {
            term: Terminal::new(80, 24),
            transport: Box::new(DemoShell::new()),
            view: TerminalView::default(),
            label: "demo shell".into(),
            origin: None,
            closed_announced: false,
        }
    }

    fn open(session: &Session, known_hosts: &Path, size: Size, waker: Waker) -> Self {
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

    fn size(&self) -> Size {
        let s = self.term.screen();
        Size {
            cols: s.cols().min(usize::from(u16::MAX)) as u16,
            rows: s.rows().min(usize::from(u16::MAX)) as u16,
        }
    }

    /// Moves bytes transport → emulator and emulator replies → transport.
    /// Returns true if anything arrived.
    fn pump(&mut self) -> bool {
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

    fn send(&mut self, bytes: &[u8]) {
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

struct Message(std::collections::VecDeque<u8>);

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

struct PrzmApp {
    config_dir: Option<PathBuf>,
    sessions: SessionTree,
    tab: Tab,
    title: String,
    quick_connect: String,
    quick_error: Option<String>,
}

impl PrzmApp {
    fn new() -> Self {
        let config_dir = przmterm_session::default_config_dir();
        let sessions = config_dir
            .as_deref()
            .map(SessionTree::load)
            .unwrap_or_default();
        for err in &sessions.errors {
            tracing::warn!("{err}");
        }
        Self {
            config_dir,
            sessions,
            tab: Tab::demo(),
            title: String::new(),
            quick_connect: String::new(),
            quick_error: None,
        }
    }

    fn reload_sessions(&mut self) {
        if let Some(dir) = &self.config_dir {
            self.sessions = SessionTree::load(dir);
        }
    }

    fn known_hosts(&self) -> PathBuf {
        self.config_dir
            .clone()
            .unwrap_or_else(std::env::temp_dir)
            .join(przmterm_session::KNOWN_HOSTS_FILE)
    }

    fn open(&mut self, session: &Session, ctx: &egui::Context) {
        let ctx = ctx.clone();
        let waker: Waker = Arc::new(move || ctx.request_repaint());
        let size = self.tab.size();
        let _ = self.tab.transport.close();
        self.tab = Tab::open(session, &self.known_hosts(), size, waker);
    }
}

/// Draws the session tree; returns the session the user double-clicked.
fn folder_ui<'a>(ui: &mut egui::Ui, folder: &'a Folder) -> Option<&'a Session> {
    let mut chosen = None;
    for f in &folder.folders {
        egui::CollapsingHeader::new(&f.name)
            .default_open(true)
            .show(ui, |ui| {
                if let Some(s) = folder_ui(ui, f) {
                    chosen = Some(s);
                }
            });
    }
    for s in &folder.sessions {
        let response = ui.selectable_label(false, &s.name).on_hover_text(format!(
            "[{}] {}\nDouble-click to connect",
            s.protocol,
            s.display_target()
        ));
        if response.double_clicked() {
            chosen = Some(s);
        }
    }
    chosen
}

impl eframe::App for PrzmApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.tab.pump() {
            ui.ctx().request_repaint();
        }

        let title = match self.tab.term.screen().title() {
            "" => "PrzmTerm".to_string(),
            t => format!("{t} — PrzmTerm"),
        };
        if title != self.title {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        let mut to_open: Option<Session> = None;

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Quick connect");
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.quick_connect)
                        .hint_text("user@host:port")
                        .desired_width(260.0),
                );
                let submitted = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Connect").clicked() || submitted {
                    match Session::quick_connect(&self.quick_connect) {
                        Ok(session) => {
                            self.quick_error = None;
                            to_open = Some(session);
                        }
                        Err(e) => self.quick_error = Some(e),
                    }
                }
                if let Some(err) = &self.quick_error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let s = self.tab.term.screen();
                ui.label(format!("PrzmTerm {}", przmterm_core::VERSION));
                ui.separator();
                ui.label(format!("{}×{}", s.cols(), s.rows()));
                ui.separator();
                ui.label(&self.tab.label);
                if self.tab.transport.is_closed() {
                    ui.weak("(closed)");
                }
            });
        });

        egui::Panel::left("sessions")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Sessions");
                    if ui.small_button("⟳").on_hover_text("Reload").clicked() {
                        self.reload_sessions();
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.sessions.root.all_sessions().is_empty() {
                        let hint = match &self.config_dir {
                            Some(d) => format!(
                                "No sessions yet.\nAdd .toml files to\n{}",
                                d.join(przmterm_session::SESSIONS_DIR).display()
                            ),
                            None => "No configuration directory.".to_string(),
                        };
                        ui.weak(hint);
                    } else if let Some(s) = folder_ui(ui, &self.sessions.root) {
                        to_open = Some(s.clone());
                    }
                    if !self.sessions.errors.is_empty() {
                        ui.separator();
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!("{} file(s) with errors", self.sessions.errors.len()),
                        )
                        .on_hover_text(
                            self.sessions
                                .errors
                                .iter()
                                .map(|e| e.to_string())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
                });
            });

        egui::CentralPanel::no_frame().show(ui, |ui| {
            let tab = &mut self.tab;
            let out = tab.view.show(ui, &mut tab.term);
            if out.resized.is_some() {
                let size = tab.size();
                match tab.transport.resize(size) {
                    Ok(()) | Err(TransportError::Closed) => {}
                    Err(e) => tracing::warn!("resize failed: {e}"),
                }
            }
            if !out.input.is_empty() {
                if tab.transport.is_closed() {
                    if out.input.contains(&b'\r') {
                        to_open = tab.origin.clone();
                    }
                } else {
                    tab.send(&out.input);
                }
                ui.ctx().request_repaint();
            }
        });

        if let Some(session) = to_open {
            self.open(&session, ui.ctx());
        }
    }
}
