//! PrzmTerm desktop application.

mod demo;
mod input;
mod pane;
mod view;

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Key, Modifiers};
use przmterm_session::{Folder, Session, SessionTree};
use przmterm_transport::{TransportError, Waker};

use crate::pane::{Env, Pane};

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

/// How the panes of a tab are arranged.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Side by side.
    Columns,
    /// Stacked.
    Rows,
}

/// A tab: one or more panes, one of them active.
struct Tab {
    panes: Vec<Pane>,
    active: usize,
    layout: Layout,
}

impl Tab {
    fn new(pane: Pane) -> Self {
        Self {
            panes: vec![pane],
            active: 0,
            layout: Layout::Columns,
        }
    }

    fn pane(&self) -> &Pane {
        &self.panes[self.active]
    }

    fn pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active]
    }

    fn title(&self) -> String {
        let title = self.pane().title();
        if self.panes.len() > 1 {
            format!("{title} (+{})", self.panes.len() - 1)
        } else {
            title
        }
    }
}

/// User actions, collected while drawing and applied afterwards.
enum Action {
    Open(Box<Session>),
    NewTab,
    CloseTab(usize),
    SelectTab(usize),
    Split(Layout),
    ClosePane,
    FocusPane(usize),
    Reconnect,
    ToggleLog,
}

struct PrzmApp {
    config_dir: Option<PathBuf>,
    sessions: SessionTree,
    tabs: Vec<Tab>,
    active_tab: usize,
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
            tabs: vec![Tab::new(Pane::demo())],
            active_tab: 0,
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

    fn env(&self) -> Env {
        Env {
            known_hosts: self
                .config_dir
                .clone()
                .unwrap_or_else(std::env::temp_dir)
                .join(przmterm_session::KNOWN_HOSTS_FILE),
            config_dir: self.config_dir.clone(),
        }
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        let ctx2 = ctx.clone();
        let waker: Waker = Arc::new(move || ctx2.request_repaint());
        let env = self.env();
        match action {
            Action::Open(session) => {
                let size = self.tab().pane().size();
                let pane = Pane::open(&session, &env, size, waker);
                // The first demo shell is just a placeholder: replace it.
                let tab = self.tab_mut();
                if tab.panes.len() == 1 && tab.pane().origin.is_none() {
                    *tab = Tab::new(pane);
                } else {
                    self.tabs.push(Tab::new(pane));
                    self.active_tab = self.tabs.len() - 1;
                }
            }
            Action::NewTab => {
                let pane = self.tab().pane().clone_session(&env, waker);
                self.tabs.push(Tab::new(pane));
                self.active_tab = self.tabs.len() - 1;
            }
            Action::CloseTab(index) => {
                let mut tab = self.tabs.remove(index);
                for pane in &mut tab.panes {
                    let _ = pane.transport.close();
                }
                if self.tabs.is_empty() {
                    self.tabs.push(Tab::new(Pane::demo()));
                }
                if self.active_tab >= index && self.active_tab > 0 {
                    self.active_tab -= 1;
                }
                self.active_tab = self.active_tab.min(self.tabs.len() - 1);
            }
            Action::SelectTab(index) => self.active_tab = index,
            Action::Split(layout) => {
                let pane = self.tab().pane().clone_session(&env, waker);
                let tab = self.tab_mut();
                tab.layout = layout;
                tab.panes.insert(tab.active + 1, pane);
                tab.active += 1;
            }
            Action::ClosePane => {
                let tab = self.tab_mut();
                if tab.panes.len() == 1 {
                    let index = self.active_tab;
                    self.apply(Action::CloseTab(index), ctx);
                    return;
                }
                let mut pane = tab.panes.remove(tab.active);
                let _ = pane.transport.close();
                tab.active = tab.active.min(tab.panes.len() - 1);
            }
            Action::FocusPane(index) => self.tab_mut().active = index,
            Action::ToggleLog => {
                let pane = self.tab_mut().pane_mut();
                if pane.log.is_some() {
                    pane.stop_log();
                } else {
                    pane.start_log(&env);
                }
            }
            Action::Reconnect => {
                let tab = self.tab_mut();
                let old = tab.pane();
                if let Some(session) = old.origin.clone() {
                    let size = old.size();
                    tab.panes[tab.active] = Pane::open(&session, &env, size, waker);
                }
            }
        }
        let tab = self.tab_mut();
        tab.pane_mut().view.focus();
    }

    /// Window-level shortcuts, taken before the terminal sees the keys.
    /// Cmd on macOS, Ctrl elsewhere, always with Shift so plain Ctrl
    /// combinations still reach the remote side.
    fn shortcuts(&self, ctx: &egui::Context) -> Option<Action> {
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        ctx.input_mut(|i| {
            if i.consume_key(cmd_shift, Key::T) {
                Some(Action::NewTab)
            } else if i.consume_key(cmd_shift, Key::W) {
                Some(Action::ClosePane)
            } else if i.consume_key(cmd_shift, Key::D) {
                Some(Action::Split(Layout::Columns))
            } else if i.consume_key(cmd_shift, Key::E) {
                Some(Action::Split(Layout::Rows))
            } else if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab) {
                let n = self.tabs.len();
                Some(Action::SelectTab((self.active_tab + n - 1) % n))
            } else if i.consume_key(Modifiers::CTRL, Key::Tab) {
                Some(Action::SelectTab((self.active_tab + 1) % self.tabs.len()))
            } else {
                None
            }
        })
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
        let ctx = ui.ctx().clone();
        let mut actions: Vec<Action> = Vec::new();

        // Every pane keeps reading, visible or not.
        let mut got = false;
        for tab in &mut self.tabs {
            for pane in &mut tab.panes {
                got |= pane.pump();
            }
        }
        if got {
            ctx.request_repaint();
        }

        let title = format!("{} — PrzmTerm", self.tab().pane().title());
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        actions.extend(self.shortcuts(&ctx));

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Quick connect");
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.quick_connect)
                        .hint_text("user@host:port")
                        .desired_width(240.0),
                );
                let submitted = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if ui.button("Connect").clicked() || submitted {
                    match Session::quick_connect(&self.quick_connect) {
                        Ok(session) => {
                            self.quick_error = None;
                            actions.push(Action::Open(Box::new(session)));
                        }
                        Err(e) => self.quick_error = Some(e),
                    }
                }
                if let Some(err) = &self.quick_error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let shortcut = |k: &str| {
                        if cfg!(target_os = "macos") {
                            format!("⌘⇧{k}")
                        } else {
                            format!("Ctrl+Shift+{k}")
                        }
                    };
                    if ui
                        .button("✕ Pane")
                        .on_hover_text(format!("Close pane ({})", shortcut("W")))
                        .clicked()
                    {
                        actions.push(Action::ClosePane);
                    }
                    if ui
                        .button("⬓ Split")
                        .on_hover_text(format!("Split stacked ({})", shortcut("E")))
                        .clicked()
                    {
                        actions.push(Action::Split(Layout::Rows));
                    }
                    if ui
                        .button("◫ Split")
                        .on_hover_text(format!("Split side by side ({})", shortcut("D")))
                        .clicked()
                    {
                        actions.push(Action::Split(Layout::Columns));
                    }
                });
            });
        });

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, tab) in self.tabs.iter().enumerate() {
                    let mut title = tab.title();
                    if title.chars().count() > 28 {
                        title = title.chars().take(27).collect::<String>() + "…";
                    }
                    if tab.pane().transport.is_closed() && tab.pane().origin.is_some() {
                        title = format!("○ {title}");
                    }
                    let tab_button = ui.selectable_label(i == self.active_tab, title);
                    if tab_button.clicked() {
                        actions.push(Action::SelectTab(i));
                    }
                    if tab_button.middle_clicked() {
                        actions.push(Action::CloseTab(i));
                    }
                    if ui.small_button("×").on_hover_text("Close tab").clicked() {
                        actions.push(Action::CloseTab(i));
                    }
                    ui.separator();
                }
                let new_tab = if cfg!(target_os = "macos") {
                    "New tab with the same session (⌘⇧T)"
                } else {
                    "New tab with the same session (Ctrl+Shift+T)"
                };
                if ui.button("+").on_hover_text(new_tab).clicked() {
                    actions.push(Action::NewTab);
                }
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let pane = self.tab().pane();
                let s = pane.term.screen();
                ui.label(format!("PrzmTerm {}", przmterm_core::VERSION));
                ui.separator();
                ui.label(format!("{}×{}", s.cols(), s.rows()));
                ui.separator();
                ui.label(&pane.label);
                if pane.transport.is_closed() {
                    ui.weak("(closed)");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (text, hover) = match &pane.log {
                        Some(log) => (
                            egui::RichText::new("● Log")
                                .color(egui::Color32::from_rgb(230, 90, 90)),
                            format!("Logging to {}\nClick to stop", log.path.display()),
                        ),
                        None => (
                            egui::RichText::new("○ Log"),
                            "Start logging this pane".into(),
                        ),
                    };
                    if ui.small_button(text).on_hover_text(hover).clicked() {
                        actions.push(Action::ToggleLog);
                    }
                });
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
                        actions.push(Action::Open(Box::new(s.clone())));
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
            let tab = &mut self.tabs[self.active_tab];
            let area = ui.available_rect_before_wrap();
            let n = tab.panes.len();
            let gap = 4.0;
            for (i, pane) in tab.panes.iter_mut().enumerate() {
                let rect = match tab.layout {
                    Layout::Columns => {
                        let w = (area.width() - gap * (n - 1) as f32) / n as f32;
                        egui::Rect::from_min_size(
                            area.min + egui::vec2(i as f32 * (w + gap), 0.0),
                            egui::vec2(w, area.height()),
                        )
                    }
                    Layout::Rows => {
                        let h = (area.height() - gap * (n - 1) as f32) / n as f32;
                        egui::Rect::from_min_size(
                            area.min + egui::vec2(0.0, i as f32 * (h + gap)),
                            egui::vec2(area.width(), h),
                        )
                    }
                };
                let active = i == tab.active;
                let out = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        pane.view.show(ui, &mut pane.term, active)
                    })
                    .inner;
                if n > 1 && active {
                    ui.painter().rect_stroke(
                        rect,
                        egui::CornerRadius::ZERO,
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(110, 200, 255)),
                        egui::StrokeKind::Inside,
                    );
                }
                if out.clicked && !active {
                    actions.push(Action::FocusPane(i));
                }
                if out.resized.is_some() {
                    match pane.transport.resize(pane.size()) {
                        Ok(()) | Err(TransportError::Closed) => {}
                        Err(e) => tracing::warn!("resize failed: {e}"),
                    }
                }
                if !out.input.is_empty() {
                    if pane.transport.is_closed() {
                        if active && out.input.contains(&b'\r') {
                            actions.push(Action::Reconnect);
                        }
                    } else {
                        pane.send(&out.input);
                    }
                    ctx.request_repaint();
                }
            }
        });

        for action in actions {
            self.apply(action, &ctx);
        }
    }
}
