//! PrzmTerm desktop application.

mod demo;
mod input;
mod view;

use std::path::PathBuf;

use eframe::egui;
use przmterm_session::{Folder, SessionTree};
use przmterm_term::Terminal;
use przmterm_transport::{Size, Transport};

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

struct Tab {
    term: Terminal,
    transport: Box<dyn Transport>,
    view: TerminalView,
}

impl Tab {
    fn demo() -> Self {
        Self {
            term: Terminal::new(80, 24),
            transport: Box::new(DemoShell::new()),
            view: TerminalView::default(),
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
        if !replies.is_empty()
            && let Err(e) = self.transport.write(&replies)
        {
            tracing::warn!("transport write failed: {e}");
        }
        got
    }
}

struct PrzmApp {
    config_dir: Option<PathBuf>,
    sessions: SessionTree,
    tab: Tab,
    title: String,
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
        }
    }

    fn reload_sessions(&mut self) {
        if let Some(dir) = &self.config_dir {
            self.sessions = SessionTree::load(dir);
        }
    }
}

fn folder_ui(ui: &mut egui::Ui, folder: &Folder) {
    for f in &folder.folders {
        egui::CollapsingHeader::new(&f.name)
            .default_open(true)
            .show(ui, |ui| folder_ui(ui, f));
    }
    for s in &folder.sessions {
        ui.label(&s.name)
            .on_hover_text(format!("[{}] {}", s.protocol, s.display_target()));
    }
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

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let s = self.tab.term.screen();
                ui.label(format!("PrzmTerm {}", przmterm_core::VERSION));
                ui.separator();
                ui.label(format!("{}×{}", s.cols(), s.rows()));
                ui.separator();
                ui.label("demo shell");
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
                    } else {
                        folder_ui(ui, &self.sessions.root);
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
            if let Some((cols, rows)) = out.resized {
                let size = Size {
                    cols: cols.min(u16::MAX as usize) as u16,
                    rows: rows.min(u16::MAX as usize) as u16,
                };
                if let Err(e) = tab.transport.resize(size) {
                    tracing::warn!("resize failed: {e}");
                }
            }
            if !out.input.is_empty() {
                if let Err(e) = tab.transport.write(&out.input) {
                    tracing::warn!("transport write failed: {e}");
                }
                ui.ctx().request_repaint();
            }
        });
    }
}
