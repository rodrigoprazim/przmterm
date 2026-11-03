//! PrzmTerm desktop application.

mod buttonbar;
mod demo;
mod desktop;
mod editor;
mod input;
mod keychain;
mod keymap;
mod keywords;
mod pane;
mod scriptguide;
mod sftp;
mod transfer;
mod vault;
mod view;

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Key};
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
        Box::new(|cc| {
            symbol_fallback(&cc.egui_ctx);
            Ok(Box::new(PrzmApp::new()))
        }),
    )
}

/// The interface text falls back to the monospace font for symbols the
/// proportional one lacks (arrows such as ⇶ and ⇅, boxes such as ◫ and ▤),
/// so they show the same on every system instead of as empty squares.
fn symbol_fallback(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional)
        && !family.iter().any(|f| f == "Hack")
    {
        family.push("Hack".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// How the panes of a tab are arranged.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Two per row, as many rows as needed.
    Grid,
    /// Side by side.
    Columns,
    /// Stacked.
    Rows,
}

/// A script's question to the user.
struct ScriptPrompt {
    script: String,
    question: String,
    secret: bool,
    reply: std::sync::mpsc::Sender<Option<String>>,
    answer: String,
    focused: bool,
}

impl Drop for ScriptPrompt {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.answer);
    }
}

/// The "Split" dialog: what goes next to the current session.
struct SplitDialog {
    layout: Layout,
    /// Panes of other tabs (tab, pane) and whether they are picked.
    others: Vec<(usize, usize, bool)>,
}

/// Height of the caption above each pane of a split tab.
const PANE_CAPTION: f32 = 18.0;

const NO_OTHER_SESSION: &str =
    "No other session is open. Open one first, or use Clone Session in the tab menu.";

/// The session's name, with the title the remote side set when it differs.
fn pane_name(pane: &Pane) -> String {
    let title = pane.title();
    match &pane.origin {
        Some(s) if s.name != title => format!("{} — {title}", s.name),
        _ => title,
    }
}

/// Moves the panes `picks` (tab, pane) of other tabs into the tab `into`,
/// after its active pane, and drops the tabs left empty. Returns the new
/// index of `into`.
fn merge_panes(tabs: &mut Vec<Tab>, into: usize, picks: &[(usize, usize)]) -> usize {
    let mut picks: Vec<(usize, usize)> = picks
        .iter()
        .copied()
        .filter(|&(t, p)| t != into && t < tabs.len() && p < tabs[t].panes.len())
        .collect();
    picks.sort_unstable();
    picks.dedup();
    // Take from the back so the indices stay right, then keep the order.
    let mut moved = Vec::new();
    for &(t, p) in picks.iter().rev() {
        let tab = &mut tabs[t];
        moved.push(tab.panes.remove(p));
        if tab.active >= tab.panes.len() {
            tab.active = tab.panes.len().saturating_sub(1);
        } else if tab.active > p {
            tab.active -= 1;
        }
    }
    moved.reverse();
    let target = &mut tabs[into];
    let at = target.active + 1;
    let count = moved.len();
    for (i, pane) in moved.into_iter().enumerate() {
        target.panes.insert(at + i, pane);
    }
    if count > 0 {
        target.active = at + count - 1;
    }
    let mut into = into;
    let mut t = tabs.len();
    while t > 0 {
        t -= 1;
        if tabs[t].panes.is_empty() {
            tabs.remove(t);
            if t < into {
                into -= 1;
            }
        }
    }
    into
}

/// Moves the panes of tab `t` into tabs of their own, right after it: all
/// but the first, or only pane `only`. Returns the tab to show.
fn separate_panes(tabs: &mut Vec<Tab>, t: usize, only: Option<usize>) -> usize {
    let tab = &mut tabs[t];
    if tab.panes.len() < 2 {
        return t;
    }
    let moved: Vec<Pane> = match only {
        Some(p) if p < tab.panes.len() => vec![tab.panes.remove(p)],
        Some(_) => return t,
        None => tab.panes.drain(1..).collect(),
    };
    tab.active = tab.active.min(tab.panes.len() - 1);
    for (k, pane) in moved.into_iter().enumerate() {
        tabs.insert(t + 1 + k, Tab::new(pane));
    }
    // A pane moved out alone is the one being looked at.
    if only.is_some() { t + 1 } else { t }
}

/// A tab: one or more panes, one of them active.
struct Tab {
    panes: Vec<Pane>,
    active: usize,
    layout: Layout,
    /// Set with "Rename": shown instead of the session's title, until the
    /// tab closes (the session itself keeps its name).
    name: Option<String>,
}

impl Layout {
    const ALL: [Layout; 3] = [Layout::Grid, Layout::Columns, Layout::Rows];

    fn label(self) -> &'static str {
        match self {
            Layout::Grid => "Grid (two per row)",
            Layout::Columns => "Side by side",
            Layout::Rows => "Stacked",
        }
    }

    /// Where pane `i` of `n` goes in `area`.
    fn rect(self, area: egui::Rect, i: usize, n: usize, gap: f32) -> egui::Rect {
        let cell = |col: usize, cols: usize, row: usize, rows: usize| {
            let w = (area.width() - gap * (cols - 1) as f32) / cols as f32;
            let h = (area.height() - gap * (rows - 1) as f32) / rows as f32;
            egui::Rect::from_min_size(
                area.min + egui::vec2(col as f32 * (w + gap), row as f32 * (h + gap)),
                egui::vec2(w, h),
            )
        };
        match self {
            Layout::Columns => cell(i, n, 0, 1),
            Layout::Rows => cell(0, 1, i, n),
            Layout::Grid => {
                let rows = n.div_ceil(2);
                // A pane alone on the last row takes the whole width.
                if n % 2 == 1 && i == n - 1 {
                    cell(0, 1, i / 2, rows)
                } else {
                    cell(i % 2, 2.min(n), i / 2, rows)
                }
            }
        }
    }
}

impl Tab {
    fn new(pane: Pane) -> Self {
        Self {
            panes: vec![pane],
            active: 0,
            layout: Layout::Grid,
            name: None,
        }
    }

    fn pane(&self) -> &Pane {
        &self.panes[self.active]
    }

    fn pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active]
    }

    /// Panes still connected to a session.
    fn connected(&self) -> usize {
        self.panes
            .iter()
            .filter(|p| p.origin.is_some() && !p.transport.is_closed())
            .count()
    }

    fn title(&self) -> String {
        let title = self.name.clone().unwrap_or_else(|| self.pane().title());
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
    /// Asks what to put next to the current session.
    Split(Layout),
    /// Splits the current tab: the panes `others` (tab, pane) move in from
    /// other tabs.
    SplitWith {
        layout: Layout,
        others: Vec<(usize, usize)>,
    },
    /// Changes how the current tab's panes are arranged.
    Arrange(Layout),
    /// Moves the current tab's active pane into a tab of its own.
    SeparatePane,
    /// Gives every pane of this tab a tab of its own.
    SeparateAll(usize),
    /// Picks a script and runs it on the active pane of this tab.
    RunScript(usize),
    /// Stops the script of the active pane of this tab.
    StopScript(usize),
    /// Opens the script guide.
    ScriptGuide,
    /// Opens an SFTP browser on the active pane's connection of this tab.
    OpenSftp(usize),
    /// Connects a session and opens an SFTP browser once logged in.
    SftpFor(Box<Session>),
    /// The Xmodem/Ymodem/Zmodem dialog for this tab: sending or receiving.
    TransferDialog(usize, bool),
    /// Stops the transfer of this tab's active pane.
    CancelTransfer(usize),
    ClosePane,
    FocusPane(usize),
    Reconnect,
    ToggleLog,
    /// Opens the active pane's log file (the running one, or the last).
    OpenLog,
    /// Shows that file in Finder / Explorer.
    RevealLog,
    /// Opens the session editor, where the log file is set.
    LogSettings,
    SendBreak,
    Macro(buttonbar::Press),
    ToggleBroadcast,
    ToggleButtons,
    /// Close these tabs, asking first if any is connected.
    RequestClose(Vec<usize>),
    /// Close the active pane, asking first if it is connected.
    RequestClosePane,
    RenameTab(usize),
    ResetTabName(usize),
    ReconnectTab(usize),
    DisconnectTab(usize),
    CloneTab(usize),
    SessionOptions(usize),
    FontTab(usize),
    ToggleSessions,
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
    /// Sessions can be dragged between folders only while unlocked.
    drag_unlocked: bool,
    /// The session list is folded away for more terminal room.
    sessions_hidden: bool,
    /// Text the session list is filtered by (name, host or IP).
    session_filter: String,
    /// A message for the status bar, set anywhere during a frame.
    notice: Option<String>,
    /// The message the status bar shows, and since when.
    status: Option<(String, std::time::Instant)>,
    /// What the last SecureCRT import did, shown until dismissed.
    import_result: Option<Vec<String>>,
    /// The keyword set editor, when open.
    keyword_editor: Option<keywords::KeywordEditor>,
    /// The password vault and its windows.
    vault: vault::VaultUi,
    /// What to do once the vault is unlocked (opening a session that uses it).
    after_unlock: Option<Action>,
    /// Macro buttons above the status bar.
    buttons: buttonbar::ButtonBarUi,
    /// What is typed goes to every connected pane of the current tab.
    broadcast: bool,
    /// Asking before turning `broadcast` on.
    confirm_broadcast: bool,
    /// Asking before quitting with sessions open.
    confirm_quit: bool,
    /// Asking before closing connected tabs or panes.
    confirm_close: Option<CloseRequest>,
    /// The "Rename" dialog: tab and the name being typed.
    tab_rename: Option<(usize, String)>,
    /// The "Font…" dialog, for this tab.
    font_dialog: Option<usize>,
    /// The "Split" dialog.
    split_dialog: Option<SplitDialog>,
    /// The keyboard shortcuts in force.
    shortcuts: keymap::Shortcuts,
    /// The keyboard shortcut editor, when open.
    keymap_editor: Option<keymap::KeymapEditor>,
    /// The Xmodem/Ymodem/Zmodem dialog.
    transfer_dialog: Option<transfer::TransferDialog>,
    /// SFTP browsers.
    sftp_windows: Vec<sftp::SftpBrowser>,
    sftp_next: u64,
    /// A session whose SFTP browser opens once it is logged in.
    sftp_pending: Option<(PathBuf, std::time::Instant)>,
    /// The script guide, when open.
    script_guide: Option<scriptguide::ScriptGuide>,
    /// Questions from scripts, asked one at a time.
    script_prompts: std::collections::VecDeque<ScriptPrompt>,
    /// A backup or export being restored or imported, and the folder name
    /// for an import.
    bundle_dialog: Option<(przmterm_session::bundle::Bundle, String)>,
    /// The quit was confirmed: let the window close.
    quitting: bool,
}

/// Folder SecureCRT sessions are imported into.
const IMPORT_FOLDER: &str = "SecureCRT";

/// How long a status bar message stays.
const STATUS_TIME: std::time::Duration = std::time::Duration::from_secs(8);

/// What the user picked in the session tree.
enum TreeAction {
    Connect(Session),
    /// Connect (if needed) and open an SFTP browser.
    Sftp(Session),
    Edit(Session),
    Duplicate(Session),
    Delete(Session),
    ForgetPassword(Session),
    NewSession(Vec<String>),
    NewFolder(PathBuf),
    RenameFolder(PathBuf, String),
    FolderSettings(Vec<String>),
    DeleteFolder(PathBuf, String, usize),
    /// Move a session file into a folder.
    Move(PathBuf, PathBuf),
    /// Export a session file or folder to a `.przm` file (path, name).
    Export(PathBuf, String),
}

/// Drag-and-drop payload: the session file being moved.
struct DragSession(PathBuf);

/// A small dialog asking for a folder name.
struct NameDialog {
    title: &'static str,
    /// For a new folder: the parent. For a rename: the folder itself.
    dir: PathBuf,
    rename: bool,
    value: String,
    error: Option<String>,
}

/// What the close confirmation would close.
#[derive(Clone)]
enum CloseRequest {
    Tabs(Vec<usize>),
    Pane,
}

/// What a confirmation dialog would delete.
#[derive(Clone)]
enum Deletion {
    Session(Box<Session>),
    Folder {
        dir: PathBuf,
        name: String,
        sessions: usize,
    },
}

impl PrzmApp {
    fn new() -> Self {
        let config_dir = przmterm_session::default_config_dir();
        let shortcuts = keymap::Shortcuts::load(config_dir.as_deref());
        let buttons = buttonbar::ButtonBarUi::load(config_dir.as_deref());
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
            drag_unlocked: false,
            sessions_hidden: false,
            session_filter: String::new(),
            notice: None,
            status: None,
            import_result: None,
            keyword_editor: None,
            vault: vault::VaultUi::new(),
            after_unlock: None,
            buttons,
            broadcast: false,
            confirm_broadcast: false,
            confirm_quit: false,
            confirm_close: None,
            tab_rename: None,
            font_dialog: None,
            split_dialog: None,
            script_prompts: Default::default(),
            script_guide: None,
            transfer_dialog: None,
            sftp_windows: Vec::new(),
            sftp_next: 0,
            sftp_pending: None,
            shortcuts,
            keymap_editor: None,
            bundle_dialog: None,
            quitting: false,
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
            sessions: self
                .sessions
                .root
                .all_sessions()
                .into_iter()
                .cloned()
                .collect(),
            credentials: self.vault.credentials(),
        }
    }

    /// Why the vault must be unlocked before `action`, if it must: a session
    /// it connects logs in with a vault entry.
    fn needs_unlock(&self, action: &Action, env: &Env) -> Option<String> {
        if self.vault.is_unlocked() {
            return None;
        }
        let dir = self.config_dir.as_deref()?;
        if !przmterm_session::vault::vault_path(dir).is_file() {
            return None;
        }
        let closed_origins = |panes: &[Pane]| -> Vec<Session> {
            panes
                .iter()
                .filter(|p| p.transport.is_closed())
                .filter_map(|p| p.origin.clone())
                .collect()
        };
        let sessions = match action {
            Action::Open(session) => vec![(**session).clone()],
            Action::Reconnect => closed_origins(std::slice::from_ref(self.tab().pane())),
            Action::ReconnectTab(t) => closed_origins(&self.tabs[*t].panes),
            _ => return None,
        };
        sessions.iter().find_map(|s| {
            let names = pane::vault_entries_used(s, env);
            (!names.is_empty()).then(|| {
                format!(
                    "{} logs in with the vault entry {}.",
                    s.name,
                    names.join(" and ")
                )
            })
        })
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
        if let Some(why) = self.needs_unlock(&action, &env) {
            self.vault.ask_unlock(why);
            self.after_unlock = Some(action);
            return;
        }
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
            Action::SelectTab(index) => {
                self.active_tab = index;
                self.broadcast = false;
            }
            Action::Split(layout) => {
                let others: Vec<(usize, usize, bool)> = self
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|&(t, _)| t != self.active_tab)
                    .flat_map(|(t, tab)| {
                        tab.panes
                            .iter()
                            .enumerate()
                            .filter(|(_, p)| p.origin.is_some())
                            .map(move |(p, _)| (t, p, false))
                    })
                    .collect();
                if others.is_empty() {
                    self.notice = Some(NO_OTHER_SESSION.into());
                } else {
                    self.split_dialog = Some(SplitDialog { layout, others });
                }
            }
            Action::SplitWith { layout, others } => {
                self.active_tab = merge_panes(&mut self.tabs, self.active_tab, &others);
                self.tab_mut().layout = layout;
            }
            Action::Arrange(layout) => self.tab_mut().layout = layout,
            Action::SeparatePane => {
                let active = self.tab().active;
                self.active_tab = separate_panes(&mut self.tabs, self.active_tab, Some(active));
                self.broadcast = false;
            }
            Action::SeparateAll(t) => {
                self.active_tab = separate_panes(&mut self.tabs, t, None);
                self.broadcast = false;
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
            Action::ToggleSessions => self.sessions_hidden = !self.sessions_hidden,
            Action::RequestClose(mut tabs) => {
                tabs.sort_unstable();
                tabs.dedup();
                let connected: usize = tabs.iter().map(|&t| self.tabs[t].connected()).sum();
                if connected > 0 {
                    self.confirm_close = Some(CloseRequest::Tabs(tabs));
                } else {
                    // Highest first, so the indices stay right.
                    for t in tabs.into_iter().rev() {
                        self.apply(Action::CloseTab(t), ctx);
                    }
                }
            }
            Action::RequestClosePane => {
                let pane = self.tab().pane();
                if pane.origin.is_some() && !pane.transport.is_closed() {
                    self.confirm_close = Some(CloseRequest::Pane);
                } else {
                    self.apply(Action::ClosePane, ctx);
                }
            }
            Action::RenameTab(t) => {
                let name = self.tabs[t].title();
                self.tab_rename = Some((t, self.tabs[t].name.clone().unwrap_or(name)));
            }
            Action::ResetTabName(t) => self.tabs[t].name = None,
            Action::ReconnectTab(t) => {
                for pane in &mut self.tabs[t].panes {
                    if pane.transport.is_closed() {
                        let ctx2 = ctx.clone();
                        pane.reconnect(&env, Arc::new(move || ctx2.request_repaint()));
                    }
                }
            }
            Action::DisconnectTab(t) => {
                for pane in &mut self.tabs[t].panes {
                    pane.cancel_retry();
                    let _ = pane.transport.close();
                }
            }
            Action::CloneTab(t) => {
                let pane = self.tabs[t].pane().clone_session(&env, waker);
                self.tabs.insert(t + 1, Tab::new(pane));
                self.active_tab = t + 1;
            }
            Action::SessionOptions(t) => {
                if let Some(session) = self.tabs[t].pane().origin.clone() {
                    self.editor = Some(if session.path.as_os_str().is_empty() {
                        Editor::save_quick(&session)
                    } else {
                        Editor::edit(&session)
                    });
                }
            }
            Action::FontTab(t) => self.font_dialog = Some(t),
            Action::ToggleBroadcast => {
                if self.broadcast {
                    self.broadcast = false;
                } else {
                    self.confirm_broadcast = true;
                }
            }
            Action::ToggleButtons => self.buttons.visible = !self.buttons.visible,
            Action::Macro(press) if press.script.is_some() => {
                let name = press.script.unwrap_or_default();
                let path = przmterm_session::script_path(self.config_dir.as_deref(), &name);
                self.tab_mut().pane_mut().run_script(&path, waker);
            }
            Action::RunScript(t) => {
                let mut picker = rfd::FileDialog::new()
                    .set_title("Run a script")
                    .add_filter("Rhai script", &[przmterm_script::EXTENSION]);
                if let Some(dir) = &self.config_dir {
                    let scripts = dir.join(przmterm_session::SCRIPTS_DIR);
                    let _ = std::fs::create_dir_all(&scripts);
                    picker = picker.set_directory(scripts);
                }
                if let Some(path) = picker.pick_file() {
                    self.tabs[t].pane_mut().run_script(&path, waker);
                }
            }
            Action::StopScript(t) => self.tabs[t].pane_mut().stop_script(),
            Action::OpenSftp(t) => {
                let pane = self.tabs[t].pane();
                let name = pane
                    .origin
                    .as_ref()
                    .map_or_else(|| pane.title(), |s| s.name.clone());
                match pane.transport.sftp(waker) {
                    Some(client) => {
                        self.sftp_next += 1;
                        self.sftp_windows.push(sftp::SftpBrowser::new(
                            name,
                            self.sftp_next,
                            client,
                        ));
                    }
                    None => {
                        self.notice =
                            Some("SFTP needs a connected SSH session (log in first).".into())
                    }
                }
            }
            Action::SftpFor(session) => {
                self.sftp_pending = Some((session.path.clone(), std::time::Instant::now()));
                // Already connected: SFTP right away; otherwise connect first.
                let live = self.tabs.iter().any(|t| {
                    t.panes.iter().any(|p| {
                        !p.transport.is_closed()
                            && p.origin.as_ref().is_some_and(|o| o.path == session.path)
                    })
                });
                if !live {
                    self.apply(Action::Open(session), ctx);
                }
            }
            Action::TransferDialog(t, sending) => {
                let pane = self.tabs[t].pane();
                if pane.transport.is_closed() {
                    self.notice = Some("The session is closed.".into());
                } else {
                    // Unix hosts get the lrzsz command typed for them.
                    let unix = matches!(
                        pane.transport.protocol(),
                        przmterm_core::Protocol::Ssh | przmterm_core::Protocol::Telnet
                    );
                    self.transfer_dialog = Some(transfer::TransferDialog::new(t, sending, unix));
                }
            }
            Action::CancelTransfer(t) => self.tabs[t].pane_mut().cancel_transfer(),
            Action::ScriptGuide => {
                self.script_guide
                    .get_or_insert_with(scriptguide::ScriptGuide::new);
            }
            Action::Macro(press) => {
                let pane = self.tab_mut().pane_mut();
                if pane.transport.is_closed() {
                    self.notice = Some("The session is closed.".into());
                } else {
                    pane.enqueue(&press.chunks);
                }
                self.run_queues(ctx);
            }
            Action::SendBreak => {
                let pane = self.tab_mut().pane_mut();
                if pane.view.offer_break && !pane.transport.is_closed() {
                    match pane.transport.send_break() {
                        Ok(()) => self.notice = Some("BREAK sent.".into()),
                        Err(e) => self.notice = Some(format!("Could not send BREAK: {e}")),
                    }
                }
            }
            Action::ToggleLog => {
                let pane = self.tab_mut().pane_mut();
                if pane.log.is_some() {
                    pane.stop_log();
                } else {
                    pane.start_log(&env);
                }
            }
            Action::OpenLog | Action::RevealLog => {
                let open = matches!(action, Action::OpenLog);
                if let Some(path) = self.tab().pane().log_path.clone() {
                    let done = if open {
                        desktop::open_text(&path)
                    } else {
                        desktop::reveal(&path)
                    };
                    if let Err(e) = done {
                        self.notice = Some(format!("Could not open the log: {e}"));
                    }
                }
            }
            Action::LogSettings => {
                if let Some(session) = self.tab().pane().origin.clone()
                    && session.path.is_file()
                {
                    self.editor = Some(Editor::edit_log(&session));
                }
            }
            Action::Reconnect => self.tab_mut().pane_mut().reconnect(&env, waker),
        }
        let tab = self.tab_mut();
        tab.pane_mut().view.focus();
    }

    /// Sends macro text that is due, and wakes up for the next piece.
    /// Carries out what running scripts ask; their questions wait in line
    /// for the prompt dialog.
    fn run_scripts(&mut self) {
        for pane in self.tabs.iter_mut().flat_map(|t| &mut t.panes) {
            for event in pane.poll_script() {
                match event {
                    pane::ScriptEvent::Prompt {
                        script,
                        question,
                        secret,
                        reply,
                    } => self.script_prompts.push_back(ScriptPrompt {
                        script,
                        question,
                        secret,
                        reply,
                        answer: String::new(),
                        focused: false,
                    }),
                    pane::ScriptEvent::Ended(message) => self.notice = Some(message),
                }
            }
        }
    }

    /// Opens the SFTP browser asked for with "Connect SFTP…" once its
    /// session is logged in.
    fn run_sftp_pending(&mut self, ctx: &egui::Context) {
        let Some((path, since)) = self.sftp_pending.clone() else {
            return;
        };
        if since.elapsed() > std::time::Duration::from_secs(120) {
            self.sftp_pending = None;
            return;
        }
        let found = self.tabs.iter().enumerate().find_map(|(t, tab)| {
            tab.panes.iter().find_map(|p| {
                (!p.transport.is_closed() && p.origin.as_ref().is_some_and(|o| o.path == path))
                    .then_some(t)
            })
        });
        let Some(t) = found else {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
            return;
        };
        let ctx2 = ctx.clone();
        let waker: Waker = Arc::new(move || ctx2.request_repaint());
        let pane = self.tabs[t]
            .panes
            .iter()
            .find(|p| !p.transport.is_closed() && p.origin.as_ref().is_some_and(|o| o.path == path))
            .expect("found above");
        if let Some(client) = pane.transport.sftp(waker) {
            let name = pane
                .origin
                .as_ref()
                .map(|o| o.name.clone())
                .unwrap_or_default();
            self.sftp_next += 1;
            self.sftp_windows
                .push(sftp::SftpBrowser::new(name, self.sftp_next, client));
            self.sftp_pending = None;
        } else {
            // Not logged in yet.
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }

    /// Transfers' messages, and `rz` on the other side asking for files.
    fn run_transfers(&mut self, ctx: &egui::Context) {
        let mut running = false;
        for pane in self.tabs.iter_mut().flat_map(|t| &mut t.panes) {
            running |= pane.transfer().is_some();
            for event in pane.take_transfer_events() {
                match event {
                    pane::TransferEvent::Ended(message) => self.notice = Some(message),
                    pane::TransferEvent::UploadWanted => {
                        let files = rfd::FileDialog::new()
                            .set_title("Zmodem: files to send")
                            .pick_files();
                        match files {
                            Some(files) => pane.start_send(
                                przmterm_transport::modem::Kind::Zmodem,
                                files,
                                None,
                            ),
                            // Tell rz there is nothing.
                            None => pane.refuse_upload(),
                        }
                    }
                }
            }
        }
        // Xmodem and Ymodem wait for an answer after each block: run every
        // frame while a transfer is on.
        if running {
            ctx.request_repaint();
        }
    }

    fn run_queues(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let next = self
            .tabs
            .iter_mut()
            .flat_map(|t| &mut t.panes)
            .filter_map(|p| p.run_queue(now))
            .min();
        if let Some(at) = next {
            ctx.request_repaint_after(at.saturating_duration_since(now));
        }
    }

    /// Reconnects the panes whose automatic reconnect is due, and wakes up
    /// in time for the next one.
    fn run_retries(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        let env = self.env();
        let mut next: Option<std::time::Instant> = None;
        for pane in self.tabs.iter_mut().flat_map(|t| &mut t.panes) {
            let Some(retry) = pane.retry() else { continue };
            if retry.at <= now {
                let ctx2 = ctx.clone();
                pane.reconnect(&env, Arc::new(move || ctx2.request_repaint()));
                ctx.request_repaint();
            } else {
                next = Some(next.map_or(retry.at, |n| n.min(retry.at)));
            }
        }
        if let Some(at) = next {
            ctx.request_repaint_after(at - now);
        }
    }

    /// Window-level shortcuts (the keymap), taken before the terminal sees
    /// the keys.
    fn shortcuts(&mut self, ctx: &egui::Context) -> Option<Action> {
        // Recording a new shortcut, or typing in a dialog: keys are not
        // shortcuts then.
        if self.keymap_editor.is_some() {
            return None;
        }
        let binding = self.shortcuts.take(ctx)?;
        let t = self.active_tab;
        let n = self.tabs.len();
        use przmterm_session::keymap::Does;
        Some(match binding.does().ok()? {
            Does::Command(id) => match id {
                "new_tab" => Action::NewTab,
                "close_pane" => Action::RequestClosePane,
                "close_tab" => Action::RequestClose(vec![t]),
                "next_tab" => Action::SelectTab((t + 1) % n),
                "previous_tab" => Action::SelectTab((t + n - 1) % n),
                "split_grid" => Action::Split(Layout::Grid),
                "split_columns" => Action::Split(Layout::Columns),
                "split_rows" => Action::Split(Layout::Rows),
                "separate_panes" => Action::SeparateAll(t),
                "clone_session" => Action::CloneTab(t),
                "reconnect" => Action::ReconnectTab(t),
                "disconnect" => Action::DisconnectTab(t),
                "send_break" => Action::SendBreak,
                "toggle_sessions" => Action::ToggleSessions,
                "toggle_buttons" => Action::ToggleButtons,
                "toggle_log" => Action::ToggleLog,
                "run_script" => Action::RunScript(t),
                "stop_script" => Action::StopScript(t),
                "script_guide" => Action::ScriptGuide,
                "open_sftp" => Action::OpenSftp(t),
                "send_files" => Action::TransferDialog(t, true),
                "receive_files" => Action::TransferDialog(t, false),
                _ => return None,
            },
            Does::Send(text) => Action::Macro(buttonbar::Press {
                chunks: przmterm_session::buttons::parse_send(text),
                script: None,
            }),
            Does::Script(name) => Action::Macro(buttonbar::Press {
                chunks: Vec::new(),
                script: Some(name.to_string()),
            }),
            Does::Button(id) => match self.buttons.press(id) {
                Some(press) => Action::Macro(press),
                None => {
                    self.notice = Some(format!("No button {id} (see the keyboard shortcuts)."));
                    return None;
                }
            },
        })
    }

    /// The shortcut of an app command, for tooltips: ` (⌘⇧T)`.
    /// The status bar's log button: click starts or stops logging, and
    /// right-click offers the rest. While logging it names the file.
    fn log_button(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let pane = self.tab().pane();
        let name = pane
            .log_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let logging = pane.log.is_some();
        let green = if ui.visuals().dark_mode {
            egui::Color32::from_rgb(90, 200, 120)
        } else {
            egui::Color32::from_rgb(30, 130, 60)
        };
        let button = if logging {
            let text = format!("● Log  {}", short_name(name.as_deref().unwrap_or("")));
            egui::Button::new(egui::RichText::new(text).color(green))
                .fill(green.gamma_multiply(0.12))
                .stroke(egui::Stroke::new(1.0, green))
        } else {
            egui::Button::selectable(false, "○ Log")
        };
        let response = ui.add(button).on_hover_text(match (&name, logging) {
            (Some(name), true) => format!(
                "Logging to {name}.\nClick to stop{}; right-click for more.",
                self.keys_hint("toggle_log")
            ),
            _ => format!(
                "Not logging.\nClick to start{}; right-click for more.",
                self.keys_hint("toggle_log")
            ),
        });
        if response.clicked() {
            actions.push(Action::ToggleLog);
        }
        response.context_menu(|ui| {
            ui.set_min_width(220.0);
            match (&name, logging) {
                (Some(name), true) => {
                    ui.label(egui::RichText::new(format!("● {name}")).color(green))
                }
                (Some(name), false) => ui.weak(format!("Last log: {name}")),
                (None, _) => ui.weak("Not logging"),
            };
            ui.separator();
            let keys = self.shortcuts.label_for("toggle_log").unwrap_or_default();
            let toggle = if logging {
                "Stop Logging"
            } else {
                "Start Logging"
            };
            if ui
                .add(egui::Button::new(toggle).shortcut_text(keys))
                .clicked()
            {
                actions.push(Action::ToggleLog);
                ui.close();
            }
            let exists = pane.log_path.as_ref().is_some_and(|p| p.is_file());
            if ui
                .add_enabled(exists, egui::Button::new("Open Log"))
                .on_disabled_hover_text("No log file yet: start logging first")
                .clicked()
            {
                actions.push(Action::OpenLog);
                ui.close();
            }
            if ui
                .add_enabled(exists, egui::Button::new(desktop::file_manager()))
                .on_disabled_hover_text("No log file yet: start logging first")
                .clicked()
            {
                actions.push(Action::RevealLog);
                ui.close();
            }
            ui.separator();
            let saved = pane.origin.as_ref().is_some_and(|s| s.path.is_file());
            if ui
                .add_enabled(saved, egui::Button::new("Log Settings…"))
                .on_hover_text("The log folder and file name, in the session's settings")
                .on_disabled_hover_text("Only for saved sessions")
                .clicked()
            {
                actions.push(Action::LogSettings);
                ui.close();
            }
        });
    }

    fn keys_hint(&self, id: &str) -> String {
        self.shortcuts
            .label_for(id)
            .map(|k| format!(" ({k})"))
            .unwrap_or_default()
    }
}

impl PrzmApp {
    fn modal_open(&self) -> bool {
        self.editor.is_some()
            || self.name_dialog.is_some()
            || self.confirm_delete.is_some()
            || self.import_result.is_some()
            || self.keyword_editor.is_some()
            || self.buttons.modal()
            || self.confirm_broadcast
            || self.confirm_quit
            || self.confirm_close.is_some()
            || self.tab_rename.is_some()
            || self.font_dialog.is_some()
            || self.split_dialog.is_some()
            || !self.script_prompts.is_empty()
            || self.script_guide.is_some()
            || self.keymap_editor.is_some()
            || self.transfer_dialog.is_some()
            || self.bundle_dialog.is_some()
            || self.vault.modal()
    }

    /// Panes with a live connection to a session.
    fn open_sessions(&self) -> usize {
        self.tabs
            .iter()
            .flat_map(|t| &t.panes)
            .filter(|p| p.origin.is_some() && !p.transport.is_closed())
            .count()
    }

    /// Writes a backup of the whole configuration where the user chooses.
    fn back_up(&mut self) {
        let Some(dir) = self.config_dir.clone() else {
            return;
        };
        let name = format!(
            "przmterm-backup-{}.{}",
            chrono::Local::now().format("%Y%m%d"),
            przmterm_session::bundle::EXTENSION
        );
        let Some(path) = rfd::FileDialog::new()
            .set_title("Back up PrzmTerm settings")
            .set_file_name(&name)
            .add_filter("PrzmTerm", &[przmterm_session::bundle::EXTENSION])
            .save_file()
        else {
            return;
        };
        self.notice = Some(
            match przmterm_session::bundle::backup(&dir)
                .and_then(|b| przmterm_session::bundle::write_bundle(&path, &b).map(|()| b))
            {
                Ok(b) => format!("Backed up {} file(s).", b.files.len()),
                Err(e) => format!("Backup failed: {e}"),
            },
        );
    }

    /// Exports a session or folder to a file to share.
    fn export(&mut self, path: &std::path::Path, name: &str) {
        let file_name = format!("{name}.{}", przmterm_session::bundle::EXTENSION);
        let Some(target) = rfd::FileDialog::new()
            .set_title("Export to a file")
            .set_file_name(&file_name)
            .add_filter("PrzmTerm", &[przmterm_session::bundle::EXTENSION])
            .save_file()
        else {
            return;
        };
        self.notice = Some(
            match przmterm_session::bundle::export(&[path.to_path_buf()])
                .and_then(|b| przmterm_session::bundle::write_bundle(&target, &b).map(|()| b))
            {
                Ok(b) => format!(
                    "Exported {} file(s). Passwords are not included.",
                    b.files.len()
                ),
                Err(e) => format!("Export failed: {e}"),
            },
        );
    }

    /// Picks a backup or export and asks what to do with it.
    fn open_bundle(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("PrzmTerm backup or export")
            .add_filter("PrzmTerm", &[przmterm_session::bundle::EXTENSION])
            .pick_file()
        else {
            return;
        };
        match przmterm_session::bundle::read_bundle(&path) {
            Ok(bundle) => {
                let folder = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Imported".into());
                let mut folder = format!("Imported {folder}");
                przmterm_session::strip_forbidden(&mut folder);
                self.bundle_dialog = Some((bundle, folder));
            }
            Err(e) => self.notice = Some(format!("Cannot open the file: {e}")),
        }
    }

    /// Imports SecureCRT sessions into the `SecureCRT` folder and shows what
    /// happened.
    fn import_securecrt(&mut self, paths: &[PathBuf]) {
        use przmterm_session::securecrt;
        let Some(dir) = self.config_dir.clone() else {
            return;
        };
        let result = securecrt::parse_any(paths).and_then(|report| {
            let n = securecrt::write(&dir, IMPORT_FOLDER, &report)?;
            let mut lines = vec![if n == 0 && !report.keyword_sets.is_empty() {
                "Keywords imported.".to_string()
            } else if n == 0 {
                "No sessions found.".to_string()
            } else {
                format!("Imported {n} session(s) into the folder {IMPORT_FOLDER}.")
            }];
            if !report.keyword_sets.is_empty() {
                let names: Vec<&str> = report
                    .keyword_sets
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect();
                lines.push(format!(
                    "Imported keyword set(s): {}. Sessions that used them in SecureCRT \
                     highlight with them here too.",
                    names.join(", ")
                ));
            }
            lines.extend(
                report
                    .skipped
                    .iter()
                    .map(|(name, why)| format!("Skipped {name}: {why}.")),
            );
            lines.extend(report.notes.iter().cloned());
            Ok(lines)
        });
        self.import_result = Some(result.unwrap_or_else(|e| vec![format!("Import failed: {e}")]));
        self.reload_sessions();
    }

    /// The session editor and the delete confirmation.
    fn dialogs(&mut self, ctx: &egui::Context) {
        let center = ctx.content_rect().center();
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        if let Some(request) = self.confirm_close.clone() {
            let (title, text) = match &request {
                CloseRequest::Pane => (
                    "Close pane?".to_string(),
                    "The session in this pane is connected. Closing disconnects it.".to_string(),
                ),
                CloseRequest::Tabs(tabs) => {
                    let connected: usize = tabs
                        .iter()
                        .filter_map(|&t| self.tabs.get(t))
                        .map(Tab::connected)
                        .sum();
                    (
                        if tabs.len() == 1 {
                            "Close tab?".to_string()
                        } else {
                            format!("Close {} tabs?", tabs.len())
                        },
                        format!("{connected} session(s) are connected. Closing disconnects them."),
                    )
                }
            };
            let mut answer = None;
            egui::Window::new(title)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(center)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(text);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Close").clicked() || enter {
                                answer = Some(true);
                            }
                            if ui.button("Cancel").clicked() || escape {
                                answer = Some(false);
                            }
                        });
                    });
                });
            if let Some(yes) = answer {
                self.confirm_close = None;
                if yes {
                    match request {
                        CloseRequest::Pane => self.apply(Action::ClosePane, ctx),
                        CloseRequest::Tabs(tabs) => {
                            for t in tabs.into_iter().rev() {
                                if t < self.tabs.len() {
                                    self.apply(Action::CloseTab(t), ctx);
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some((t, name)) = &mut self.tab_rename {
            let mut answer = None;
            egui::Window::new("Rename tab")
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(center)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.add(egui::TextEdit::singleline(name).desired_width(260.0))
                        .request_focus();
                    ui.weak("Only the tab changes; the session keeps its name.");
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("OK").clicked() || enter {
                                answer = Some(true);
                            }
                            if ui.button("Cancel").clicked() || escape {
                                answer = Some(false);
                            }
                        });
                    });
                });
            if let Some(yes) = answer {
                let (t, name) = (*t, name.trim().to_string());
                if yes && let Some(tab) = self.tabs.get_mut(t) {
                    tab.name = (!name.is_empty()).then_some(name);
                }
                self.tab_rename = None;
            }
        }
        if let Some((bundle, folder)) = &mut self.bundle_dialog {
            use przmterm_session::bundle::Kind;
            let summary = bundle.summary();
            let mut answer = None; // Some(true) go, Some(false) cancel
            egui::Window::new(match bundle.kind {
                Kind::Backup => "Restore backup",
                Kind::Export => "Import sessions",
            })
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Made {} with PrzmTerm {}: {} session(s){}.",
                    bundle.created,
                    bundle.version,
                    summary.sessions,
                    if bundle.kind == Kind::Backup {
                        format!(
                            ", {} folder setting(s), {} keyword set(s), {} other file(s){}",
                            summary.folders,
                            summary.keyword_sets,
                            summary.other,
                            if summary.vault {
                                " and the password vault"
                            } else {
                                ""
                            }
                        )
                    } else {
                        String::new()
                    }
                ));
                match bundle.kind {
                    Kind::Backup => {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Files with the same name are replaced. Your current settings \
                             are saved first.",
                        );
                    }
                    Kind::Export => {
                        ui.horizontal(|ui| {
                            ui.label("Into the folder");
                            editor::name_field(ui, folder, "Imported");
                        });
                        ui.weak("Nothing is replaced: a folder that exists gets a new (2).");
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let go = match bundle.kind {
                        Kind::Backup => "Restore",
                        Kind::Export => "Import",
                    };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(go).clicked() {
                            answer = Some(true);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            answer = Some(false);
                        }
                    });
                });
            });
            if let Some(go) = answer {
                let (bundle, folder) = self.bundle_dialog.take().expect("dialog open");
                if go && let Some(dir) = self.config_dir.clone() {
                    let result = match bundle.kind {
                        Kind::Backup => {
                            przmterm_session::bundle::restore(&dir, &bundle).map(|(n, _)| {
                                format!("Restored {n} file(s); the previous settings were kept.")
                            })
                        }
                        Kind::Export => przmterm_session::bundle::import(&dir, &bundle, &folder)
                            .map(|(n, used)| {
                                format!("Imported {n} file(s) into the folder {used}.")
                            }),
                    };
                    self.notice = Some(result.unwrap_or_else(|e| format!("Failed: {e}")));
                    self.reload_sessions();
                    self.buttons = buttonbar::ButtonBarUi::load(Some(&dir));
                    // The vault file may have been replaced.
                    self.vault.lock();
                }
            }
        }
        if let Some(dialog) = &mut self.split_dialog {
            let tabs = &self.tabs;
            dialog
                .others
                .retain(|&(t, p, _)| t < tabs.len() && p < tabs[t].panes.len());
            let mut answer = None; // Some(true) split, Some(false) cancel
            egui::Window::new(format!("Split: {}", dialog.layout.label().to_lowercase()))
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(center)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Sessions open in other tabs to show next to this one:");
                    ui.add_space(4.0);
                    {
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for (t, p, picked) in &mut dialog.others {
                                    let pane = &self.tabs[*t].panes[*p];
                                    let state = if pane.transport.is_closed() {
                                        " (disconnected)"
                                    } else {
                                        ""
                                    };
                                    ui.checkbox(
                                        picked,
                                        format!("Tab {}: {}{state}", *t + 1, pane_name(pane)),
                                    );
                                }
                            });
                    }
                    ui.add_space(6.0);
                    let any = dialog.others.iter().any(|o| o.2);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(any, egui::Button::new("Split"))
                                .on_disabled_hover_text("Choose at least one session")
                                .clicked()
                                || (enter && any)
                            {
                                answer = Some(true);
                            }
                            if ui.button("Cancel").clicked() || escape {
                                answer = Some(false);
                            }
                        });
                    });
                });
            if let Some(go) = answer {
                let dialog = self.split_dialog.take().expect("dialog open");
                if go {
                    self.apply(
                        Action::SplitWith {
                            layout: dialog.layout,
                            others: dialog
                                .others
                                .iter()
                                .filter(|o| o.2)
                                .map(|&(t, p, _)| (t, p))
                                .collect(),
                        },
                        ctx,
                    );
                }
            }
        }
        if let (Some(editor), Some(dir)) = (&mut self.keymap_editor, self.config_dir.clone()) {
            let buttons = self.buttons.ids();
            if let keymap::KeymapResult::Closed = editor.show(ctx, &dir, &buttons) {
                self.keymap_editor = None;
                self.shortcuts = keymap::Shortcuts::load(Some(&dir));
            }
        }
        if let Some(dialog) = &mut self.transfer_dialog {
            match dialog.show(ctx) {
                transfer::Answer::Open => {}
                transfer::Answer::Cancel => self.transfer_dialog = None,
                transfer::Answer::Start(start) => {
                    let t = dialog.tab;
                    self.transfer_dialog = None;
                    if let Some(tab) = self.tabs.get_mut(t) {
                        tab.pane_mut().begin(start);
                    }
                }
            }
        }
        // Files dropped outside every SFTP window go to the newest one.
        let last = self.sftp_windows.len().saturating_sub(1);
        let mut i = 0;
        self.sftp_windows.retain_mut(|w| {
            let keep = w.show(ctx, i == last);
            i += 1;
            keep
        });
        if let (Some(guide), Some(dir)) = (&mut self.script_guide, self.config_dir.clone()) {
            let open = guide.show(ctx, &dir);
            if let Some(n) = guide.notice.take() {
                self.notice = Some(n);
            }
            if !open {
                self.script_guide = None;
            }
        }
        if let Some(prompt) = self.script_prompts.front_mut() {
            let mut answer = None; // Some(true) ok, Some(false) cancel
            egui::Window::new(format!("Script {}", prompt.script))
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(center)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(&prompt.question);
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut prompt.answer)
                            .password(prompt.secret)
                            .desired_width(300.0),
                    );
                    if !prompt.focused {
                        if field.has_focus() {
                            prompt.focused = true;
                        } else {
                            field.request_focus();
                        }
                    }
                    if field.lost_focus() && enter {
                        answer = Some(true);
                    }
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("OK").clicked() {
                                answer = Some(true);
                            }
                            if ui
                                .button("Stop script")
                                .on_hover_text("Cancel the question and stop the script")
                                .clicked()
                                || escape
                            {
                                answer = Some(false);
                            }
                        });
                    });
                });
            if let Some(ok) = answer {
                let prompt = self.script_prompts.pop_front().expect("prompt open");
                let _ = prompt.reply.send(ok.then(|| prompt.answer.clone()));
            }
        }
        if let Some(t) = self.font_dialog {
            let mut close = escape;
            if let Some(tab) = self.tabs.get_mut(t) {
                let mut size = tab.pane().view.font_size;
                egui::Window::new("Font")
                    .pivot(egui::Align2::CENTER_CENTER)
                    .default_pos(center)
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.add(
                            egui::Slider::new(&mut size, 8.0..=32.0)
                                .step_by(1.0)
                                .text("size"),
                        );
                        ui.weak("For the panes of this tab, until it closes.");
                        if ui.button("Close").clicked() || enter {
                            close = true;
                        }
                    });
                for pane in &mut tab.panes {
                    pane.view.font_size = size;
                }
            } else {
                close = true;
            }
            if close {
                self.font_dialog = None;
            }
        }
        if self.confirm_quit {
            let n = self.open_sessions();
            let mut answer = None;
            egui::Window::new("Quit PrzmTerm?")
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "{n} session(s) are still connected. Quitting disconnects them."
                    ));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Quit").clicked() {
                                answer = Some(true);
                            }
                            if ui.button("Cancel").clicked()
                                || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                answer = Some(false);
                            }
                        });
                    });
                });
            match answer {
                Some(true) => {
                    self.confirm_quit = false;
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Some(false) => self.confirm_quit = false,
                None => {}
            }
        }
        if self.confirm_broadcast {
            let n = self.tab().panes.len();
            let mut answer = None;
            egui::Window::new("Send to all sessions in this tab?")
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Everything you type will go to all {n} sessions in this tab, \
                         until you turn it off."
                    ));
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Check that every pane is where the commands should run.",
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Turn on").clicked() {
                                answer = Some(true);
                            }
                            if ui.button("Cancel").clicked()
                                || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                answer = Some(false);
                            }
                        });
                    });
                });
            if let Some(on) = answer {
                self.broadcast = on;
                self.confirm_broadcast = false;
            }
        }
        if let Some(editor) = &mut self.keyword_editor
            && let keywords::KeywordResult::Closed(changed) = editor.show(ctx)
        {
            self.keyword_editor = None;
            // Open panes using a changed set pick it up at once.
            let env = self.env();
            for pane in self.tabs.iter_mut().flat_map(|t| &mut t.panes) {
                if pane
                    .origin
                    .as_ref()
                    .and_then(|s| s.keywords.as_ref())
                    .is_some_and(|k| changed.contains(k))
                {
                    pane.reload_keywords(&env);
                }
            }
        }
        if let Some(editor) = &mut self.editor {
            editor.vault_names = self.vault.names();
            if editor.take_vault_request() {
                self.vault.open_window();
            }
        }
        if let Some(editor) = &mut self.editor
            && let Some(select) = editor.take_keyword_request()
            && let Some(dir) = &self.config_dir
        {
            self.keyword_editor = Some(keywords::KeywordEditor::new(dir, select.as_deref()));
        }
        if let Some(lines) = &self.import_result {
            let mut close =
                ctx.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape));
            egui::Window::new("SecureCRT import")
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .collapsible(false)
                .resizable(false)
                .default_width(460.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for (i, line) in lines.iter().enumerate() {
                                if i == 0 {
                                    ui.label(egui::RichText::new(line).strong());
                                } else {
                                    ui.label(line);
                                }
                                ui.add_space(4.0);
                            }
                        });
                    ui.add_space(4.0);
                    close |= ui.button("OK").clicked();
                });
            if close {
                self.import_result = None;
            }
        }
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
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    let edit = editor::name_field(ui, &mut dialog.value, "Folder name");
                    edit.request_focus();
                    let enter = ui.input(|i| i.key_pressed(Key::Enter));
                    if let Some(err) = &dialog.error {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                    }
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                            if ui.button("Cancel").clicked()
                                || ui.input(|i| i.key_pressed(Key::Escape))
                            {
                                close = true;
                            }
                        });
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
                Deletion::Session(s) => ("Delete session", format!("Delete “{}”?", s.name)),
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
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(question);
                    if ui.input(|i| i.key_pressed(Key::Escape)) {
                        keep_open = false;
                    }
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                });
            if !keep_open {
                self.confirm_delete = None;
            }
        }
    }
}

/// Draws the session tree, collecting what the user picked.
/// The right-click menu of a tab.
fn tab_menu(
    ui: &mut egui::Ui,
    tabs: &[Tab],
    i: usize,
    active: usize,
    broadcast: bool,
    actions: &mut Vec<Action>,
) {
    let tab = &tabs[i];
    let has_session = tab.panes.iter().any(|p| p.origin.is_some());
    let any_closed = tab
        .panes
        .iter()
        .any(|p| p.origin.is_some() && p.transport.is_closed());
    let mut item = |ui: &mut egui::Ui, enabled: bool, text: &str, action: Action| {
        if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
            actions.push(action);
            ui.close();
        }
    };
    item(ui, true, "Rename…", Action::RenameTab(i));
    item(
        ui,
        tab.name.is_some(),
        "Reset Name",
        Action::ResetTabName(i),
    );
    item(ui, any_closed, "Reconnect", Action::ReconnectTab(i));
    item(
        ui,
        tab.connected() > 0,
        "Disconnect",
        Action::DisconnectTab(i),
    );
    ui.separator();
    item(ui, true, "Close", Action::RequestClose(vec![i]));
    let disconnected: Vec<usize> = tabs
        .iter()
        .enumerate()
        .filter(|(_, t)| t.panes.iter().any(|p| p.origin.is_some()) && t.connected() == 0)
        .map(|(n, _)| n)
        .collect();
    item(
        ui,
        !disconnected.is_empty(),
        "Close Disconnected Tabs",
        Action::RequestClose(disconnected),
    );
    let others: Vec<usize> = (0..tabs.len()).filter(|&n| n != i).collect();
    item(
        ui,
        !others.is_empty(),
        "Close Other Tabs",
        Action::RequestClose(others),
    );
    let right: Vec<usize> = (i + 1..tabs.len()).collect();
    item(
        ui,
        !right.is_empty(),
        "Close Tabs to the Right",
        Action::RequestClose(right),
    );
    item(
        ui,
        true,
        "Close All Tabs",
        Action::RequestClose((0..tabs.len()).collect()),
    );
    ui.separator();
    item(ui, has_session, "Clone Session", Action::CloneTab(i));
    item(
        ui,
        tab.panes.len() > 1,
        "Separate Panes into Tabs",
        Action::SeparateAll(i),
    );
    ui.separator();
    let quick = tab
        .pane()
        .origin
        .as_ref()
        .is_some_and(|s| s.path.as_os_str().is_empty());
    item(
        ui,
        has_session,
        if quick {
            "Save Session…"
        } else {
            "Session Options…"
        },
        Action::SessionOptions(i),
    );
    item(ui, true, "Font…", Action::FontTab(i));
    ui.separator();
    match tab.pane().script_name() {
        Some(name) => item(
            ui,
            true,
            &format!("Stop Script {name}"),
            Action::StopScript(i),
        ),
        None => item(ui, true, "Run Script…", Action::RunScript(i)),
    }
    item(ui, true, "Script Guide…", Action::ScriptGuide);
    ui.separator();
    let ssh_live = tab
        .pane()
        .origin
        .as_ref()
        .is_some_and(|s| s.protocol == przmterm_core::Protocol::Ssh)
        && !tab.pane().transport.is_closed();
    item(ui, ssh_live, "Connect SFTP Session", Action::OpenSftp(i));

    ui.separator();
    // Typing into every pane of this tab: only for split tabs.
    let split = tab.panes.len() > 1;
    let on = broadcast && i == active;
    if ui
        .add_enabled(
            split,
            egui::Button::selectable(on, "Send Commands to This Tab"),
        )
        .on_disabled_hover_text("Split the tab first (◫ Split)")
        .clicked()
    {
        if i != active {
            actions.push(Action::SelectTab(i));
        }
        actions.push(Action::ToggleBroadcast);
        ui.close();
    }
}

/// A file name cut to fit the status bar: the start and the end stay.
fn short_name(name: &str) -> String {
    const MAX: usize = 32;
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= MAX {
        return name.to_string();
    }
    let head: String = chars[..MAX / 2 - 1].iter().collect();
    let tail: String = chars[chars.len() - (MAX / 2 - 1)..].iter().collect();
    format!("{head}…{tail}")
}

/// The colour that marks "send to all": the status bar button and the
/// frame of every pane that gets the keys.
fn broadcast_color(visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        egui::Color32::from_rgb(240, 170, 60)
    } else {
        egui::Color32::from_rgb(190, 110, 0)
    }
}

/// True if the session's name, host or address contains `filter`
/// (already lower-case).
fn session_matches(s: &Session, filter: &str) -> bool {
    filter.is_empty()
        || s.name.to_lowercase().contains(filter)
        || s.display_target().to_lowercase().contains(filter)
}

fn folder_ui(
    ui: &mut egui::Ui,
    folder: &Folder,
    picked: &mut Vec<TreeAction>,
    drag: bool,
    filter: &str,
    open: &std::collections::HashSet<PathBuf>,
) {
    for f in &folder.folders {
        if !filter.is_empty() && !f.all_sessions().iter().any(|s| session_matches(s, filter)) {
            continue;
        }
        let mut header = egui::CollapsingHeader::new(format!("📁 {}", f.name))
            .id_salt(&f.dir)
            .default_open(true);
        if !filter.is_empty() {
            header = header.open(Some(true)); // show every match while searching
        }
        let header = header.show(ui, |ui| folder_ui(ui, f, picked, drag, filter, open));
        if drag {
            let r = &header.header_response;
            if r.dnd_hover_payload::<DragSession>().is_some() {
                ui.painter().rect_stroke(
                    r.rect.expand(2.0),
                    egui::CornerRadius::same(3),
                    egui::Stroke::new(1.5, ui.visuals().selection.stroke.color),
                    egui::StrokeKind::Outside,
                );
            }
            if let Some(payload) = r.dnd_release_payload::<DragSession>() {
                picked.push(TreeAction::Move(payload.0.clone(), f.dir.clone()));
            }
        }
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
                if ui
                    .button("Export folder…")
                    .on_hover_text("Save this folder and its sessions to a file to share")
                    .clicked()
                {
                    picked.push(TreeAction::Export(f.dir.clone(), f.name.clone()));
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
        if !session_matches(s, filter) {
            continue;
        }
        if drag {
            ui.dnd_drag_source(
                ui.scope_id().with(("drag-session", &s.path)),
                DragSession(s.path.clone()),
                |ui| ui.label(format!("⠿ {}", s.name)),
            )
            .response
            .on_hover_text("Drag onto a folder to move it");
            continue;
        }
        // Sessions connected in a tab get a green dot.
        let is_open = open.contains(&s.path);
        let text: egui::WidgetText = if is_open {
            let mut job = egui::text::LayoutJob::default();
            let font = egui::TextStyle::Body.resolve(ui.style());
            job.append(
                "● ",
                0.0,
                egui::TextFormat::simple(font.clone(), egui::Color32::from_rgb(70, 190, 100)),
            );
            job.append(
                &s.name,
                0.0,
                egui::TextFormat::simple(font, ui.visuals().text_color()),
            );
            job.into()
        } else {
            s.name.clone().into()
        };
        let response = ui.selectable_label(false, text).on_hover_text(format!(
            "[{}] {}{}\nDouble-click to connect, right-click for more",
            s.protocol,
            s.display_target(),
            if is_open { "\nOpen in a tab" } else { "" }
        ));
        if response.double_clicked() {
            picked.push(TreeAction::Connect(s.clone()));
        }
        response.context_menu(|ui| {
            if ui.button("Connect").clicked() {
                picked.push(TreeAction::Connect(s.clone()));
                ui.close();
            }
            if s.protocol == przmterm_core::Protocol::Ssh
                && ui
                    .button("Connect SFTP…")
                    .on_hover_text("Browse and transfer files on this server")
                    .clicked()
            {
                picked.push(TreeAction::Sftp(s.clone()));
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
            if ui
                .button("Export session…")
                .on_hover_text("Save this session to a file to share")
                .clicked()
            {
                picked.push(TreeAction::Export(s.path.clone(), s.name.clone()));
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
        // Closing the window (or ⌘Q) with sessions open asks first.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.quitting
            && self.open_sessions() > 0
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_quit = true;
        }
        if let Some(notice) = self.notice.take() {
            self.status = Some((notice, std::time::Instant::now()));
        }
        if let Some((_, since)) = &self.status {
            match STATUS_TIME.checked_sub(since.elapsed()) {
                Some(left) => ctx.request_repaint_after(left),
                None => self.status = None,
            }
        }

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
        self.run_retries(&ctx);
        self.run_queues(&ctx);
        self.run_scripts();
        self.run_sftp_pending(&ctx);
        self.run_transfers(&ctx);
        // "Send to all" is for split panes: off when the tab has one pane.
        if self.tab().panes.len() < 2 {
            self.broadcast = false;
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
                    let keys = |id: &str| self.shortcuts.label_for(id).unwrap_or_default();
                    // In a split tab: give the panes their tabs back.
                    if self.tabs[self.active_tab].panes.len() > 1
                        && ui
                            .button("⇱ Separate")
                            .on_hover_text(format!(
                                "Give each pane its own tab again{}",
                                self.keys_hint("separate_panes")
                            ))
                            .clicked()
                    {
                        actions.push(Action::SeparateAll(self.active_tab));
                    }
                    let tab = &self.tabs[self.active_tab];
                    let split = tab.panes.len() > 1;
                    let elsewhere = self.tabs.iter().enumerate().any(|(t, other)| {
                        t != self.active_tab && other.panes.iter().any(|p| p.origin.is_some())
                    });
                    ui.menu_button("◫ Split ▾", |ui| {
                        ui.weak("Join sessions open in other tabs");
                        for (layout, id) in [
                            (Layout::Grid, "split_grid"),
                            (Layout::Columns, "split_columns"),
                            (Layout::Rows, "split_rows"),
                        ] {
                            if ui
                                .add_enabled(
                                    elsewhere,
                                    egui::Button::new(format!("{}…", layout.label()))
                                        .shortcut_text(keys(id)),
                                )
                                .on_disabled_hover_text(NO_OTHER_SESSION)
                                .clicked()
                            {
                                actions.push(Action::Split(layout));
                                ui.close();
                            }
                        }
                        if split {
                            ui.separator();
                            ui.weak("Arrange this tab");
                            for layout in Layout::ALL {
                                if ui
                                    .add(egui::Button::selectable(
                                        tab.layout == layout,
                                        layout.label(),
                                    ))
                                    .clicked()
                                {
                                    actions.push(Action::Arrange(layout));
                                    ui.close();
                                }
                            }
                            ui.separator();
                            if ui
                                .button("Move This Pane to a New Tab")
                                .on_hover_text("The active pane gets a tab of its own")
                                .clicked()
                            {
                                actions.push(Action::SeparatePane);
                                ui.close();
                            }
                            if ui
                                .button("Separate All Panes")
                                .on_hover_text(
                                    "Every pane gets a tab of its own; nothing is closed",
                                )
                                .clicked()
                            {
                                actions.push(Action::SeparateAll(self.active_tab));
                                ui.close();
                            }
                        }
                    });
                });
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
                let weak = pane.transport.weak_algorithms();
                if !weak.is_empty() {
                    ui.label(
                        egui::RichText::new(" legacy ")
                            .color(egui::Color32::BLACK)
                            .background_color(egui::Color32::from_rgb(240, 170, 60)),
                    )
                    .on_hover_text(format!(
                        "This connection uses old, weak algorithms: {}.\n\
                         The equipment offers nothing newer.",
                        weak.join(", ")
                    ));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.log_button(ui, &mut actions);
                    let split = self.tabs[self.active_tab].panes.len() > 1;
                    let broadcast = split && {
                        let button = if self.broadcast {
                            // Outlined in the same colour as the panes it types into.
                            let amber = broadcast_color(ui.visuals());
                            egui::Button::new(
                                egui::RichText::new("⇶ Typing to all").color(amber).strong(),
                            )
                            .fill(amber.gamma_multiply(0.12))
                            .stroke(egui::Stroke::new(1.0, amber))
                        } else {
                            egui::Button::selectable(false, "⇶ Send to all")
                        };
                        ui.add(button)
                            .on_hover_text(if self.broadcast {
                                "What you type goes to every session in this tab. Click to stop."
                            } else {
                                "When on, what you type goes to every session in this tab"
                            })
                            .clicked()
                    };
                    if broadcast {
                        actions.push(Action::ToggleBroadcast);
                    }
                    if pane.origin.is_some()
                        && !pane.transport.is_closed()
                        && pane.transfer().is_none()
                        && ui
                            .button("⇅ Transfer")
                            .on_hover_text("Send or receive files with Zmodem, Ymodem or Xmodem")
                            .clicked()
                    {
                        actions.push(Action::TransferDialog(self.active_tab, true));
                    }
                    if ui
                        .selectable_label(self.buttons.visible, "▤ Buttons")
                        .on_hover_text("Show or hide the button bar")
                        .clicked()
                    {
                        actions.push(Action::ToggleButtons);
                    }
                    if let Some((t, p)) = pane.transfer() {
                        if ui
                            .small_button("Cancel")
                            .on_hover_text("Stop the transfer (Ctrl+C in the terminal too)")
                            .clicked()
                        {
                            actions.push(Action::CancelTransfer(self.active_tab));
                        }
                        let size = |n: u64| sftp::size_text(n);
                        let amount = match p.total {
                            Some(total) if total > 0 => format!(
                                "{} of {} ({}%)",
                                size(p.done),
                                size(total),
                                p.done * 100 / total
                            ),
                            _ => size(p.done),
                        };
                        let arrow = if t.sending { "↑" } else { "↓" };
                        ui.label(
                            egui::RichText::new(format!(
                                "{arrow} {} {} {amount}",
                                t.kind.label(),
                                p.file
                            ))
                            .color(egui::Color32::from_rgb(110, 200, 255)),
                        );
                    }
                    if let Some(name) = pane.script_name() {
                        if ui
                            .small_button("■ Stop")
                            .on_hover_text("Stop the script")
                            .clicked()
                        {
                            actions.push(Action::StopScript(self.active_tab));
                        }
                        ui.label(
                            egui::RichText::new(format!("▶ {name}"))
                                .color(egui::Color32::from_rgb(110, 200, 255)),
                        );
                    }
                    if pane.view.offer_break
                        && ui
                            .add_enabled(
                                !pane.transport.is_closed(),
                                egui::Button::new("Break").small(),
                            )
                            .on_hover_text(format!("Send BREAK{}", self.keys_hint("send_break")))
                            .clicked()
                    {
                        actions.push(Action::SendBreak);
                    }
                    if let Some((text, _)) = &self.status {
                        ui.separator();
                        // Long messages (paths) are cut; the whole text shows
                        // on hover.
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(text)
                                    .color(egui::Color32::from_rgb(140, 200, 255)),
                            )
                            .truncate(),
                        );
                    }
                });
            });
        });

        if self.sessions_hidden {
            egui::Panel::left("sessions-folded")
                .resizable(false)
                .exact_size(30.0)
                .show(ui, |ui| {
                    if ui
                        .small_button("▶")
                        .on_hover_text(format!(
                            "Show the sessions{}",
                            self.keys_hint("toggle_sessions")
                        ))
                        .clicked()
                    {
                        actions.push(Action::ToggleSessions);
                    }
                });
        }
        let mut sessions_open = !self.sessions_hidden;
        egui::Panel::left("sessions")
            .resizable(true)
            .default_size(220.0)
            .show_collapsible(ui, &mut sessions_open, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .small_button("◀")
                        .on_hover_text(format!("Hide the sessions{}", self.keys_hint("toggle_sessions")))
                        .clicked()
                    {
                        actions.push(Action::ToggleSessions);
                    }
                    ui.strong("Sessions");
                    if ui.small_button("⟳").on_hover_text("Reload").clicked() {
                        self.reload_sessions();
                    }
                    let (lock, hint) = if self.drag_unlocked {
                        (
                            "🔓",
                            "Dragging is on: drag sessions onto folders. Click to lock.",
                        )
                    } else {
                        ("🔒", "Unlock to drag sessions between folders")
                    };
                    if ui
                        .selectable_label(self.drag_unlocked, lock)
                        .on_hover_text(hint)
                        .clicked()
                    {
                        self.drag_unlocked = !self.drag_unlocked;
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
                            ui.separator();
                            if ui
                                .button("Back up settings…")
                                .on_hover_text(
                                    "Everything (sessions, folders, keywords, buttons, \
                                     known hosts) in one file; passwords stay in the keychain",
                                )
                                .clicked()
                            {
                                ui.close();
                                self.back_up();
                            }
                            if ui
                                .button("Restore or import…")
                                .on_hover_text("A backup, or sessions someone exported")
                                .clicked()
                            {
                                ui.close();
                                self.open_bundle();
                            }
                            ui.separator();
                            if ui.button("Keyword sets…").clicked() {
                                if let Some(dir) = &self.config_dir {
                                    self.keyword_editor =
                                        Some(keywords::KeywordEditor::new(dir, None));
                                }
                                ui.close();
                            }
                            if ui
                                .button("Password vault…")
                                .on_hover_text(
                                    "User names, passwords and key passphrases in one \
                                     encrypted file",
                                )
                                .clicked()
                            {
                                self.vault.open_window();
                                ui.close();
                            }
                            if ui
                                .button("Keyboard shortcuts…")
                                .on_hover_text("Choose the keys for commands, text, scripts and buttons")
                                .clicked()
                            {
                                self.keymap_editor =
                                    Some(keymap::KeymapEditor::new(&self.shortcuts));
                                ui.close();
                            }
                            if ui
                                .button("Script guide…")
                                .on_hover_text("How to write scripts, with examples")
                                .clicked()
                            {
                                self.script_guide
                                    .get_or_insert_with(scriptguide::ScriptGuide::new);
                                ui.close();
                            }
                            ui.separator();
                            if ui
                                .button("Import from SecureCRT…")
                                .on_hover_text(
                                    "An XML export, session .ini files or keyword lists (.ini)",
                                )
                                .clicked()
                            {
                                ui.close();
                                if let Some(paths) = rfd::FileDialog::new()
                                    .set_title("SecureCRT export (.xml) or session files (.ini)")
                                    .add_filter("SecureCRT", &["xml", "ini"])
                                    .pick_files()
                                {
                                    self.import_securecrt(&paths);
                                }
                            }
                            if ui
                                .button("Import a SecureCRT Sessions folder…")
                                .on_hover_text(
                                    "SecureCRT's Config/Sessions folder, with its sub-folders",
                                )
                                .clicked()
                            {
                                ui.close();
                                if let Some(dir) = rfd::FileDialog::new()
                                    .set_title("SecureCRT Sessions folder")
                                    .pick_folder()
                                {
                                    self.import_securecrt(&[dir]);
                                }
                            }
                        });
                    }
                });
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.session_filter)
                        .hint_text("🔍 Name, host or IP")
                        .desired_width(f32::INFINITY),
                );
                // Enter connects to the first match; Esc clears the search.
                let search_enter =
                    search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                // Esc takes the focus off the field before we see it.
                if (search.has_focus() || search.lost_focus())
                    && ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    self.session_filter.clear();
                }
                ui.separator();
                // Long names stay on one line; the list scrolls sideways.
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    if self.sessions.root.all_sessions().is_empty() {
                        let hint = match &self.config_dir {
                            Some(_) => "No sessions yet.\n+ › New session… adds one.",
                            None => "No configuration directory.",
                        };
                        ui.weak(hint);
                    } else {
                        let mut picked = Vec::new();
                        let drag = self.drag_unlocked;
                        // Always shown while unlocked, so the tree does not move mid-drag.
                        if drag {
                            let (_, dropped) = ui.dnd_drop_zone::<DragSession, _>(
                                egui::Frame::default().inner_margin(4.0),
                                |ui| {
                                    ui.weak("⬆ Drop here for the top level · drag ⠿ sessions onto folders");
                                },
                            );
                            if let Some(payload) = dropped {
                                picked.push(TreeAction::Move(
                                    payload.0.clone(),
                                    self.sessions.root.dir.clone(),
                                ));
                            }
                        }
                        let filter = self.session_filter.trim().to_lowercase();
                        if !filter.is_empty()
                            && !self
                                .sessions
                                .root
                                .all_sessions()
                                .iter()
                                .any(|s| session_matches(s, &filter))
                        {
                            ui.weak("No session matches.");
                        }
                        if search_enter
                            && let Some(first) = self
                                .sessions
                                .root
                                .all_sessions()
                                .into_iter()
                                .find(|s| session_matches(s, &filter))
                        {
                            picked.push(TreeAction::Connect(first.clone()));
                        }
                        let open: std::collections::HashSet<PathBuf> = self
                            .tabs
                            .iter()
                            .flat_map(|t| &t.panes)
                            .filter(|p| !p.transport.is_closed())
                            .filter_map(|p| p.origin.as_ref().map(|o| o.path.clone()))
                            .filter(|p| !p.as_os_str().is_empty())
                            .collect();
                        folder_ui(ui, &self.sessions.root, &mut picked, drag, &filter, &open);
                        for p in picked {
                            match p {
                                TreeAction::Connect(s) => actions.push(Action::Open(Box::new(s))),
                                TreeAction::Sftp(s) => actions.push(Action::SftpFor(Box::new(s))),
                                TreeAction::Edit(s) => self.editor = Some(Editor::edit(&s)),
                                TreeAction::Duplicate(s) => {
                                    self.editor = Some(Editor::duplicate(&s))
                                }
                                TreeAction::Delete(s) => {
                                    self.confirm_delete = Some(Deletion::Session(Box::new(s)))
                                }
                                TreeAction::Move(path, dir) => {
                                    self.notice =
                                        Some(match przmterm_session::move_session(&path, &dir) {
                                            Ok(new) if new == path => "Already there.".into(),
                                            Ok(_) => format!(
                                                "Moved {}.",
                                                path.file_stem()
                                                    .unwrap_or_default()
                                                    .to_string_lossy()
                                            ),
                                            Err(e) => format!("Cannot move: {e}"),
                                        });
                                    self.reload_sessions();
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
                                TreeAction::Export(path, name) => self.export(&path, &name),
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

        // Added after the session list: the tabs and buttons span the
        // terminal only, and the session list reaches up to the toolbar.
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (i, tab) in self.tabs.iter().enumerate() {
                    let mut title = tab.title();
                    if title.chars().count() > 28 {
                        title = title.chars().take(27).collect::<String>() + "…";
                    }
                    if tab.pane().transport.is_closed() && tab.pane().origin.is_some() {
                        title = format!("○ {title}");
                    } else if !tab.pane().transport.weak_algorithms().is_empty() {
                        title = format!("⚠ {title}");
                    }
                    // A split tab lists its sessions.
                    let hover = if tab.panes.len() > 1 {
                        let names: Vec<String> = tab
                            .panes
                            .iter()
                            .map(|p| format!("• {}", pane_name(p)))
                            .collect();
                        format!("{}\nRight-click for more", names.join("\n"))
                    } else {
                        "Right-click for more".to_string()
                    };
                    let tab_button = ui
                        .selectable_label(i == self.active_tab, title)
                        .on_hover_text(hover);
                    if tab_button.clicked() {
                        actions.push(Action::SelectTab(i));
                    }
                    if tab_button.middle_clicked() {
                        actions.push(Action::RequestClose(vec![i]));
                    }
                    tab_button.context_menu(|ui| {
                        tab_menu(
                            ui,
                            &self.tabs,
                            i,
                            self.active_tab,
                            self.broadcast,
                            &mut actions,
                        );
                    });
                    if ui.small_button("×").on_hover_text("Close tab").clicked() {
                        actions.push(Action::RequestClose(vec![i]));
                    }
                    ui.separator();
                }
            });
        });

        if self.buttons.visible {
            egui::Panel::bottom("buttons").show(ui, |ui| {
                if let Some(press) = self.buttons.show(ui, self.config_dir.as_deref()) {
                    actions.push(Action::Macro(press));
                }
                if let Some(err) = &self.buttons.error {
                    ui.colored_label(ui.visuals().error_fg_color, format!("Buttons: {err}"));
                }
            });
        }
        let modal = self.modal_open();
        // What was typed in the active pane, for "send to all".
        let mut typed: Option<Vec<u8>> = None;
        let broadcast = self.broadcast;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let amber = broadcast_color(ui.visuals());
            let tab = &mut self.tabs[self.active_tab];
            let area = ui.available_rect_before_wrap();
            let n = tab.panes.len();
            let gap = 4.0;
            for (i, pane) in tab.panes.iter_mut().enumerate() {
                let mut rect = tab.layout.rect(area, i, n, gap);
                let active = i == tab.active;
                // In a split tab each pane is captioned with its session.
                if n > 1 {
                    let caption =
                        egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), PANE_CAPTION));
                    let visuals = ui.visuals();
                    let (fill, text) = if active {
                        (
                            egui::Color32::from_rgb(30, 70, 100),
                            egui::Color32::from_rgb(220, 240, 255),
                        )
                    } else {
                        (visuals.faint_bg_color, visuals.weak_text_color())
                    };
                    ui.painter().rect_filled(caption, 0.0, fill);
                    let dot = if pane.origin.is_some() && pane.transport.is_closed() {
                        "○ "
                    } else {
                        ""
                    };
                    ui.painter().with_clip_rect(caption).text(
                        caption.left_center() + egui::vec2(6.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        format!("{dot}{}", pane_name(pane)),
                        egui::FontId::proportional(12.0),
                        text,
                    );
                    // Typing to all: every pane that gets the keys says so.
                    if broadcast && !pane.transport.is_closed() {
                        ui.painter().with_clip_rect(caption).text(
                            caption.right_center() - egui::vec2(6.0, 0.0),
                            egui::Align2::RIGHT_CENTER,
                            "⇶ typing to all",
                            egui::FontId::proportional(11.0),
                            amber,
                        );
                    }
                    if ui
                        .interact(
                            caption,
                            ui.make_persistent_id(("pane-caption", i)),
                            egui::Sense::click(),
                        )
                        .clicked()
                        && !active
                    {
                        actions.push(Action::FocusPane(i));
                    }
                    rect.min.y += PANE_CAPTION;
                }
                let out = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        pane.view.show(ui, &mut pane.term, active && !modal)
                    })
                    .inner;
                if n > 1 && broadcast {
                    // The caption and the terminal framed together.
                    let framed = rect.with_min_y(rect.min.y - PANE_CAPTION);
                    ui.painter().rect_stroke(
                        framed,
                        egui::CornerRadius::ZERO,
                        egui::Stroke::new(if active { 2.0 } else { 1.0 }, amber),
                        egui::StrokeKind::Inside,
                    );
                } else if n > 1 && active {
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
                if out.send_break {
                    if !active {
                        actions.push(Action::FocusPane(i));
                    }
                    actions.push(Action::SendBreak);
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
                        } else if out.input.contains(&3) {
                            pane.cancel_retry();
                        }
                    } else {
                        pane.send(&out.input);
                        if active {
                            typed = Some(out.input.clone());
                        }
                    }
                    ctx.request_repaint();
                }
            }
        });

        if self.broadcast
            && let Some(bytes) = typed
        {
            let tab = self.tab_mut();
            let source = tab.active;
            for (i, pane) in tab.panes.iter_mut().enumerate() {
                if i != source && !pane.transport.is_closed() {
                    pane.send(&bytes);
                }
            }
        }

        self.dialogs(&ctx);
        self.buttons.dialogs(&ctx, self.config_dir.as_deref());
        if let Some(dir) = self.config_dir.clone() {
            match self.vault.show(&ctx, &dir) {
                vault::VaultEvent::Unlocked => actions.extend(self.after_unlock.take()),
                vault::VaultEvent::Cancelled => self.after_unlock = None,
                vault::VaultEvent::None => {}
            }
        }
        if let Some(notice) = self.vault.notice.take() {
            self.notice = Some(notice);
        }
        // Give the keyboard back to the terminal when the last dialog closes.
        if modal && !self.modal_open() {
            self.tab_mut().pane_mut().view.focus();
        }

        for action in actions {
            self.apply(action, &ctx);
        }
        // A message set this frame shows on the next one.
        if self.notice.is_some() {
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(labels: &[&str]) -> Tab {
        let mut panes = labels.iter().map(|l| {
            let mut p = Pane::demo();
            p.label = l.to_string();
            p
        });
        let mut tab = Tab::new(panes.next().unwrap());
        tab.panes.extend(panes);
        tab
    }

    #[test]
    fn long_log_names_keep_both_ends() {
        assert_eq!(short_name("sw-20261102.log"), "sw-20261102.log");
        let long = "core-switch-datacenter-sao-paulo-20261102-221500.log";
        let short = short_name(long);
        assert_eq!(short.chars().count(), 31);
        assert!(short.starts_with("core-switch-dat") && short.ends_with("1102-221500.log"));
    }

    fn labels(tabs: &[Tab]) -> Vec<Vec<String>> {
        tabs.iter()
            .map(|t| t.panes.iter().map(|p| p.label.clone()).collect())
            .collect()
    }

    #[test]
    fn separating_gives_panes_their_own_tabs_and_closes_nothing() {
        let mut tabs = vec![tab(&["x"]), tab(&["a", "b", "c"]), tab(&["y"])];
        tabs[1].active = 1;
        let shown = separate_panes(&mut tabs, 1, Some(1));
        assert_eq!(
            labels(&tabs),
            [vec!["x"], vec!["a", "c"], vec!["b"], vec!["y"]]
        );
        assert_eq!(shown, 2, "the moved pane is shown");
        assert_eq!(tabs[1].active, 1);
        let shown = separate_panes(&mut tabs, 1, None);
        assert_eq!(
            labels(&tabs),
            [vec!["x"], vec!["a"], vec!["c"], vec!["b"], vec!["y"]]
        );
        assert_eq!(shown, 1);
        assert_eq!(separate_panes(&mut tabs, 0, None), 0, "a single pane stays");
        assert_eq!(tabs.len(), 5);
    }

    #[test]
    fn the_grid_puts_two_per_row_and_a_last_odd_pane_across() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 90.0));
        let r = |i, n| Layout::Grid.rect(area, i, n, 0.0);
        assert_eq!(
            r(0, 2),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(50.0, 90.0))
        );
        assert_eq!(
            r(1, 3),
            egui::Rect::from_min_max(egui::pos2(50.0, 0.0), egui::pos2(100.0, 45.0))
        );
        assert_eq!(
            r(2, 3),
            egui::Rect::from_min_max(egui::pos2(0.0, 45.0), egui::pos2(100.0, 90.0))
        );
        assert_eq!(
            r(3, 4),
            egui::Rect::from_min_max(egui::pos2(50.0, 45.0), egui::pos2(100.0, 90.0))
        );
        assert_eq!(r(4, 5).min, egui::pos2(0.0, 60.0));
        assert_eq!(r(4, 5).width(), 100.0);
    }

    #[test]
    fn split_moves_panes_from_other_tabs_and_drops_empty_tabs() {
        let mut tabs = vec![tab(&["a"]), tab(&["b1", "b2"]), tab(&["c"]), tab(&["d"])];
        tabs[1].active = 1;
        // Into "c": all of tab 0, the first pane of tab 1.
        let into = merge_panes(&mut tabs, 2, &[(1, 0), (0, 0), (2, 0), (9, 9)]);
        assert_eq!(
            labels(&tabs),
            [vec!["b2"], vec!["c", "a", "b1"], vec!["d"]],
            "picks keep the tab order; its own pane and bad indices are ignored"
        );
        assert_eq!(into, 1);
        assert_eq!(tabs[1].active, 2, "the last pane moved in is active");
        assert_eq!(tabs[0].active, 0, "the tab that lost a pane stays valid");
    }
}
