//! PrzmTerm desktop application.

mod demo;
mod editor;
mod input;
mod keychain;
mod pane;
mod view;

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Key, Modifiers};
use przmterm_session::{Folder, Session, SessionTree};
use przmterm_transport::ssh::PasswordStore;
use przmterm_transport::{TransportError, Waker};

use crate::editor::{Editor, EditorResult};
use crate::keychain::Keychain;
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
    editor: Option<Editor>,
    confirm_delete: Option<Deletion>,
    name_dialog: Option<NameDialog>,
    notice: Option<String>,
}

/// What the user picked in the session tree.
enum TreeAction {
    Connect(Session),
    Edit(Session),
    Duplicate(Session),
    Delete(Session),
    ForgetPassword(Session),
    NewSession(Vec<String>),
    NewFolder(PathBuf),
    RenameFolder(PathBuf, String),
    FolderSettings(Vec<String>),
    DeleteFolder(PathBuf, String, usize),
}

/// A small dialog asking for a folder name.
struct NameDialog {
    title: &'static str,
    /// For a new folder: the parent. For a rename: the folder itself.
    dir: PathBuf,
    rename: bool,
    value: String,
    error: Option<String>,
}

/// What a confirmation dialog would delete.
#[derive(Clone)]
enum Deletion {
    Session(Session),
    Folder {
        dir: PathBuf,
        name: String,
        sessions: usize,
    },
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
            editor: None,
            confirm_delete: None,
            name_dialog: None,
            notice: None,
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
                // Already connected to this session? Open another channel on
                // that connection instead of logging in (and 2FA) again.
                let live = self.tabs.iter().flat_map(|t| &t.panes).find(|p| {
                    !p.transport.is_closed()
                        && p.origin
                            .as_ref()
                            .is_some_and(|o| o.path == session.path && o.name == session.name)
                });
                let pane = match live {
                    Some(p) => p.clone_session(&env, waker),
                    None => {
                        let size = self.tab().pane().size();
                        Pane::open(&session, &env, size, waker)
                    }
                };
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

impl PrzmApp {
    fn modal_open(&self) -> bool {
        self.editor.is_some() || self.name_dialog.is_some() || self.confirm_delete.is_some()
    }

    /// The session editor and the delete confirmation.
    fn dialogs(&mut self, ctx: &egui::Context) {
        if let (Some(editor), Some(dir)) = (&mut self.editor, self.config_dir.clone()) {
            match editor.show(ctx, &dir, &self.sessions.root) {
                EditorResult::Open => {}
                EditorResult::Cancel => self.editor = None,
                EditorResult::Saved(name) => {
                    self.editor = None;
                    self.notice = Some(format!("Saved {name}."));
                    self.reload_sessions();
                }
            }
        }

        if let Some(dialog) = &mut self.name_dialog {
            let mut close = false;
            let mut done: Option<String> = None;
            egui::Window::new(dialog.title)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut dialog.value)
                            .hint_text("Folder name")
                            .desired_width(240.0),
                    );
                    edit.request_focus();
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    if let Some(err) = &dialog.error {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || enter {
                            let result = if dialog.rename {
                                przmterm_session::rename_folder(&dialog.dir, &dialog.value)
                            } else {
                                przmterm_session::create_folder(&dialog.dir, &dialog.value)
                            };
                            match result {
                                Ok(_) => done = Some(dialog.value.trim().to_string()),
                                Err(e) => dialog.error = Some(e.to_string()),
                            }
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
                            close = true;
                        }
                    });
                });
            if let Some(name) = done {
                self.notice = Some(format!("Folder {name} saved."));
                self.reload_sessions();
                close = true;
            }
            if close {
                self.name_dialog = None;
            }
        }

        if let Some(deletion) = self.confirm_delete.clone() {
            let mut keep_open = true;
            let (title, question) = match &deletion {
                Deletion::Session(s) => (
                    "Delete session",
                    format!("Delete “{}”?\n{}", s.name, s.path.display()),
                ),
                Deletion::Folder { name, sessions, .. } => (
                    "Delete folder",
                    match sessions {
                        0 => format!("Delete the folder “{name}”?"),
                        1 => format!("Delete the folder “{name}” and the session in it?"),
                        n => format!("Delete the folder “{name}” and the {n} sessions in it?"),
                    },
                ),
            };
            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(question);
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        keep_open = false;
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            let result = match &deletion {
                                Deletion::Session(s) => {
                                    if let Some(account) = s.password_account() {
                                        Keychain.delete(&account);
                                    }
                                    std::fs::remove_file(&s.path)
                                        .map(|_| s.name.clone())
                                        .map_err(|e| e.to_string())
                                }
                                Deletion::Folder { dir, name, .. } => {
                                    przmterm_session::delete_folder(dir)
                                        .map(|_| name.clone())
                                        .map_err(|e| e.to_string())
                                }
                            };
                            self.notice = Some(match result {
                                Ok(name) => format!("Deleted {name}."),
                                Err(e) => format!("Cannot delete: {e}"),
                            });
                            keep_open = false;
                            self.reload_sessions();
                        }
                        if ui.button("Cancel").clicked() {
                            keep_open = false;
                        }
                    });
                });
            if !keep_open {
                self.confirm_delete = None;
            }
        }
    }
}

/// Draws the session tree, collecting what the user picked.
fn folder_ui(ui: &mut egui::Ui, folder: &Folder, picked: &mut Vec<TreeAction>) {
    for f in &folder.folders {
        let header = egui::CollapsingHeader::new(format!("📁 {}", f.name))
            .id_salt(&f.dir)
            .default_open(true)
            .show(ui, |ui| folder_ui(ui, f, picked));
        header
            .header_response
            .on_hover_text("Right-click to add sessions, rename or set folder defaults")
            .context_menu(|ui| {
                if ui.button("New session here…").clicked() {
                    picked.push(TreeAction::NewSession(f.trail.clone()));
                    ui.close();
                }
                if ui.button("New folder inside…").clicked() {
                    picked.push(TreeAction::NewFolder(f.dir.clone()));
                    ui.close();
                }
                ui.separator();
                if ui.button("Rename…").clicked() {
                    picked.push(TreeAction::RenameFolder(f.dir.clone(), f.name.clone()));
                    ui.close();
                }
                if ui
                    .button("Folder settings…")
                    .on_hover_text("Values every session in this folder inherits")
                    .clicked()
                {
                    picked.push(TreeAction::FolderSettings(f.trail.clone()));
                    ui.close();
                }
                ui.separator();
                if ui.button("Delete…").clicked() {
                    picked.push(TreeAction::DeleteFolder(
                        f.dir.clone(),
                        f.name.clone(),
                        f.session_count(),
                    ));
                    ui.close();
                }
            });
    }
    for s in &folder.sessions {
        let response = ui.selectable_label(false, &s.name).on_hover_text(format!(
            "[{}] {}\nDouble-click to connect, right-click for more",
            s.protocol,
            s.display_target()
        ));
        if response.double_clicked() {
            picked.push(TreeAction::Connect(s.clone()));
        }
        response.context_menu(|ui| {
            if ui.button("Connect").clicked() {
                picked.push(TreeAction::Connect(s.clone()));
                ui.close();
            }
            if ui.button("Edit…").clicked() {
                picked.push(TreeAction::Edit(s.clone()));
                ui.close();
            }
            if ui.button("Duplicate…").clicked() {
                picked.push(TreeAction::Duplicate(s.clone()));
                ui.close();
            }
            if s.password_account().is_some() && ui.button("Forget saved password").clicked() {
                picked.push(TreeAction::ForgetPassword(s.clone()));
                ui.close();
            }
            ui.separator();
            if ui.button("Delete…").clicked() {
                picked.push(TreeAction::Delete(s.clone()));
                ui.close();
            }
        });
    }
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
                    if let Some(dir) = &self.config_dir
                        && ui
                            .small_button("⚙")
                            .on_hover_text("Global settings for every session")
                            .clicked()
                    {
                        self.editor = Some(Editor::global(dir));
                    }
                    if self.config_dir.is_some() {
                        ui.menu_button("+", |ui| {
                            if ui.button("New session…").clicked() {
                                self.editor = Some(Editor::new_session(&[]));
                                ui.close();
                            }
                            if ui.button("New folder…").clicked() {
                                self.name_dialog = Some(NameDialog {
                                    title: "New folder",
                                    dir: self.sessions.root.dir.clone(),
                                    rename: false,
                                    value: String::new(),
                                    error: None,
                                });
                                ui.close();
                            }
                        });
                    }
                });
                if let Some(notice) = &self.notice {
                    ui.weak(notice);
                }
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
                        let mut picked = Vec::new();
                        folder_ui(ui, &self.sessions.root, &mut picked);
                        for p in picked {
                            match p {
                                TreeAction::Connect(s) => actions.push(Action::Open(Box::new(s))),
                                TreeAction::Edit(s) => self.editor = Some(Editor::edit(&s)),
                                TreeAction::Duplicate(s) => {
                                    self.editor = Some(Editor::duplicate(&s))
                                }
                                TreeAction::Delete(s) => {
                                    self.confirm_delete = Some(Deletion::Session(s))
                                }
                                TreeAction::NewSession(trail) => {
                                    self.editor = Some(Editor::new_session(&trail))
                                }
                                TreeAction::NewFolder(parent) => {
                                    self.name_dialog = Some(NameDialog {
                                        title: "New folder",
                                        dir: parent,
                                        rename: false,
                                        value: String::new(),
                                        error: None,
                                    })
                                }
                                TreeAction::RenameFolder(dir, name) => {
                                    self.name_dialog = Some(NameDialog {
                                        title: "Rename folder",
                                        dir,
                                        rename: true,
                                        value: name,
                                        error: None,
                                    })
                                }
                                TreeAction::FolderSettings(trail) => {
                                    if let Some(f) = self.sessions.root.find(&trail) {
                                        self.editor = Some(Editor::folder(f));
                                    }
                                }
                                TreeAction::DeleteFolder(dir, name, sessions) => {
                                    self.confirm_delete = Some(Deletion::Folder {
                                        dir,
                                        name,
                                        sessions,
                                    })
                                }
                                TreeAction::ForgetPassword(s) => {
                                    if let Some(account) = s.password_account() {
                                        Keychain.delete(&account);
                                        self.notice =
                                            Some(format!("Forgot the password for {account}."));
                                    }
                                }
                            }
                        }
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

        let modal = self.modal_open();
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
                        pane.view.show(ui, &mut pane.term, active && !modal)
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

        self.dialogs(&ctx);
        // Give the keyboard back to the terminal when the last dialog closes.
        if modal && !self.modal_open() {
            self.tab_mut().pane_mut().view.focus();
        }

        for action in actions {
            self.apply(action, &ctx);
        }
    }
}
