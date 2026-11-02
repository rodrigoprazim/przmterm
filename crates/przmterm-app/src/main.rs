//! PrzmTerm desktop application.

mod buttonbar;
mod demo;
mod editor;
mod input;
mod keychain;
mod keywords;
mod pane;
mod vault;
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

/// The "Split" dialog: what goes next to the current session.
struct SplitDialog {
    layout: Layout,
    /// A new pane on the current session.
    this: bool,
    /// Panes of other tabs (tab, pane) and whether they are picked.
    others: Vec<(usize, usize, bool)>,
}

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

/// A tab: one or more panes, one of them active.
struct Tab {
    panes: Vec<Pane>,
    active: usize,
    layout: Layout,
    /// Set with "Rename": shown instead of the session's title, until the
    /// tab closes (the session itself keeps its name).
    name: Option<String>,
}

impl Tab {
    fn new(pane: Pane) -> Self {
        Self {
            panes: vec![pane],
            active: 0,
            layout: Layout::Columns,
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
    /// Splits the current tab: a new pane on its session (`this`) and the
    /// panes `others` (tab, pane) moved in from other tabs.
    SplitWith {
        layout: Layout,
        this: bool,
        others: Vec<(usize, usize)>,
    },
    ClosePane,
    FocusPane(usize),
    Reconnect,
    ToggleLog,
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
                self.split_dialog = Some(SplitDialog {
                    layout,
                    // Alone, the current session is the obvious choice.
                    this: others.is_empty(),
                    others,
                });
            }
            Action::SplitWith {
                layout,
                this,
                others,
            } => {
                if this {
                    let pane = self.tab().pane().clone_session(&env, waker);
                    let tab = self.tab_mut();
                    tab.panes.insert(tab.active + 1, pane);
                    tab.active += 1;
                }
                self.active_tab = merge_panes(&mut self.tabs, self.active_tab, &others);
                self.tab_mut().layout = layout;
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
            Action::Reconnect => self.tab_mut().pane_mut().reconnect(&env, waker),
        }
        let tab = self.tab_mut();
        tab.pane_mut().view.focus();
    }

    /// Sends macro text that is due, and wakes up for the next piece.
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

    /// Window-level shortcuts, taken before the terminal sees the keys.
    /// Cmd on macOS, Ctrl elsewhere, always with Shift so plain Ctrl
    /// combinations still reach the remote side.
    fn shortcuts(&self, ctx: &egui::Context) -> Option<Action> {
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        ctx.input_mut(|i| {
            if i.consume_key(cmd_shift, Key::T) {
                Some(Action::NewTab)
            } else if i.consume_key(cmd_shift, Key::W) {
                Some(Action::RequestClosePane)
            } else if i.consume_key(cmd_shift, Key::D) {
                Some(Action::Split(Layout::Columns))
            } else if i.consume_key(cmd_shift, Key::E) {
                Some(Action::Split(Layout::Rows))
            } else if i.consume_key(cmd_shift, Key::B) {
                Some(Action::SendBreak)
            } else if i.consume_key(cmd_shift, Key::S) {
                Some(Action::ToggleSessions)
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
                        if ui.button("Close").clicked() || enter {
                            answer = Some(true);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            answer = Some(false);
                        }
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
                        if ui.button("OK").clicked() || enter {
                            answer = Some(true);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            answer = Some(false);
                        }
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
                    if ui.button(go).clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() || escape {
                        answer = Some(false);
                    }
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
            let current = pane_name(self.tabs[self.active_tab].pane());
            egui::Window::new(match dialog.layout {
                Layout::Columns => "Split side by side",
                Layout::Rows => "Split one above the other",
            })
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Show next to this session:");
                ui.add_space(4.0);
                ui.checkbox(
                    &mut dialog.this,
                    format!("{current} (itself, in a new pane)"),
                );
                if !dialog.others.is_empty() {
                    ui.add_space(4.0);
                    ui.weak("Open in other tabs (moved here):");
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
                let any = dialog.this || dialog.others.iter().any(|o| o.2);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() || escape {
                            answer = Some(false);
                        }
                        if ui
                            .add_enabled(any, egui::Button::new("Split"))
                            .on_disabled_hover_text("Choose at least one session")
                            .clicked()
                            || (enter && any)
                        {
                            answer = Some(true);
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
                            this: dialog.this,
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
    // Typing into every pane of this tab: only for split tabs.
    let split = tab.panes.len() > 1;
    let on = broadcast && i == active;
    if ui
        .add_enabled(
            split,
            egui::Button::selectable(on, "Send Commands to This Tab"),
        )
        .on_disabled_hover_text("Split the tab first (Ctrl+Shift+D, ⌘⇧D)")
        .clicked()
    {
        if i != active {
            actions.push(Action::SelectTab(i));
        }
        actions.push(Action::ToggleBroadcast);
        ui.close();
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
        let header = header.show(ui, |ui| folder_ui(ui, f, picked, drag, filter));
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
                    let shortcut = |k: &str| {
                        if cfg!(target_os = "macos") {
                            format!("⌘⇧{k}")
                        } else {
                            format!("Ctrl+Shift+{k}")
                        }
                    };
                    if self.tabs[self.active_tab].panes.len() > 1
                        && ui
                            .button("× Pane")
                            .on_hover_text(format!("Close pane ({})", shortcut("W")))
                            .clicked()
                    {
                        actions.push(Action::RequestClosePane);
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
                    // Green while logging, red while not; the dot keeps the
                    // state readable without colour.
                    let (text, fill, hover) = match &pane.log {
                        Some(_) => (
                            "● Log",
                            egui::Color32::from_rgb(40, 135, 65),
                            "Logging to a file. Click to stop".to_string(),
                        ),
                        None => (
                            "○ Log",
                            egui::Color32::from_rgb(170, 55, 55),
                            "Not logging. Click to start".into(),
                        ),
                    };
                    let button =
                        egui::Button::new(egui::RichText::new(text).color(egui::Color32::WHITE))
                            .fill(fill)
                            .small();
                    if ui.add(button).on_hover_text(hover).clicked() {
                        actions.push(Action::ToggleLog);
                    }
                    let split = self.tabs[self.active_tab].panes.len() > 1;
                    let broadcast = split
                        && ui
                            .selectable_label(
                                self.broadcast,
                                if self.broadcast {
                                    egui::RichText::new(" ⇶ Typing to all ")
                                        .color(egui::Color32::BLACK)
                                        .background_color(egui::Color32::from_rgb(240, 170, 60))
                                } else {
                                    egui::RichText::new("⇶ Send to all")
                                },
                            )
                            .on_hover_text(
                                "When on, what you type goes to every session in this tab",
                            )
                            .clicked();
                    if broadcast {
                        actions.push(Action::ToggleBroadcast);
                    }
                    if ui
                        .selectable_label(self.buttons.visible, "▤ Buttons")
                        .on_hover_text("Show or hide the button bar")
                        .clicked()
                    {
                        actions.push(Action::ToggleButtons);
                    }
                    if pane.view.offer_break
                        && ui
                            .add_enabled(
                                !pane.transport.is_closed(),
                                egui::Button::new("Break").small(),
                            )
                            .on_hover_text("Send BREAK (Ctrl+Shift+B, ⌘⇧B on macOS)")
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
                        .on_hover_text("Show the sessions (Ctrl+Shift+S, ⌘⇧S)")
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
                        .on_hover_text("Hide the sessions (Ctrl+Shift+S, ⌘⇧S)")
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
                        folder_ui(ui, &self.sessions.root, &mut picked, drag, &filter);
                        for p in picked {
                            match p {
                                TreeAction::Connect(s) => actions.push(Action::Open(Box::new(s))),
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
                    let tab_button = ui
                        .selectable_label(i == self.active_tab, title)
                        .on_hover_text("Right-click for more");
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

    fn labels(tabs: &[Tab]) -> Vec<Vec<String>> {
        tabs.iter()
            .map(|t| t.panes.iter().map(|p| p.label.clone()).collect())
            .collect()
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
