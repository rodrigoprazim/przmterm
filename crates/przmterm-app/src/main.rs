//! PrzmTerm desktop application.

mod appmenu;
mod atlas;
mod buttonbar;
mod desktop;
mod editor;
mod fonts;
mod grid;
mod icons;
mod input;
mod keymap;
mod keywords;
mod launcher;
mod pane;
mod scriptguide;
mod sftp;
mod theme;
mod transfer;
mod update;
mod vault;
mod view;

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Key};
use przmterm_session::{Folder, Session, SessionTree};
use przmterm_transport::ssh::PasswordStore;
use przmterm_transport::{TransportError, Waker};

use crate::editor::{Editor, EditorResult};
use crate::pane::{Env, Pane};
use przmterm_connect::Keychain;

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
            .with_min_inner_size([480.0, 320.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-256.png"))
                    .unwrap_or_default(),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "PrzmTerm",
        options,
        Box::new(|cc| {
            if let Some(state) = &cc.wgpu_render_state {
                grid::install(state);
            }
            let fonts = fonts::TermFonts::new(&cc.egui_ctx);
            let mut app = PrzmApp::new(fonts);
            theme::apply(&cc.egui_ctx);
            app.menu_bar = appmenu::install(&cc.egui_ctx);
            if app.prefs.check_for_updates {
                app.update = Some(update::Check::start(&cc.egui_ctx));
            }
            app.share_sessions();
            Ok(Box::new(app))
        }),
    )
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
    /// A new PrzmTerm shell goes next to the session too.
    shell: bool,
    /// Panes of other tabs (tab, pane) and whether they are picked.
    others: Vec<(usize, usize, bool)>,
}

/// Height of the caption above each pane of a split tab.
const PANE_CAPTION: f32 = 18.0;

/// The session's name, with the title the remote side set when it differs.
fn pane_name(pane: &Pane) -> String {
    let title = pane.title();
    match &pane.origin {
        Some(s) if s.name != title => format!("{} — {title}", s.name),
        _ => title,
    }
}

/// The font list of the Font dialog and the session editor: the built-in
/// font, then the installed monospace ones.
pub fn font_picker(
    ui: &mut egui::Ui,
    id: &str,
    name: &mut Option<String>,
    families: Option<&fonts::FontList>,
) {
    let shown = name.clone().unwrap_or_else(|| fonts::BUILT_IN.to_string());
    egui::ComboBox::from_id_salt(id)
        .width(240.0)
        .selected_text(shown)
        .height(360.0)
        .show_ui(ui, |ui| {
            match fonts::font_menu(ui, families, name.as_deref()) {
                Some(fonts::FontChoice::BuiltIn) => *name = None,
                Some(fonts::FontChoice::Family(f)) => *name = Some(f),
                None => {}
            }
        });
}

pub fn cursor_label(shape: przmterm_session::CursorShape) -> &'static str {
    match shape {
        przmterm_session::CursorShape::Block => "█ Block",
        przmterm_session::CursorShape::Underline => "▁ Underline",
        przmterm_session::CursorShape::Bar => "▏ Bar",
    }
}

/// A session tab, as editors draw them: flat on the tab strip; the active
/// one in the terminal's colour, joined to it, with rounded top corners.
/// The × shows on the active tab and under the pointer. Returns the tab's
/// response (click selects, right-click opens its menu) and whether × was
/// clicked.
fn tab_widget(
    ui: &mut egui::Ui,
    title: &str,
    active: bool,
    locked: bool,
    group: Option<egui::Color32>,
) -> (egui::Response, bool) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text_color = if active {
        theme::TEXT_STRONG
    } else {
        theme::TEXT_WEAK
    };
    let galley = ui
        .painter()
        .layout_no_wrap(title.to_string(), font, text_color);
    let (pad, close_w, height) = (12.0, 22.0, 32.0);
    let lock_w = if locked { 18.0 } else { 0.0 };
    let width = pad + lock_w + galley.size().x + 6.0 + close_w + 6.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let close_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 6.0 - close_w / 2.0, rect.center().y),
        egui::vec2(close_w, close_w - 2.0),
    );
    // The × on top of the tab, so its clicks are its own.
    let close = ui.interact(close_rect, response.id.with("close"), egui::Sense::click());
    let hovered = response.hovered() || close.hovered();
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let top = egui::CornerRadius {
            nw: 6,
            ne: 6,
            sw: 0,
            se: 0,
        };
        // The active tab in the terminal's colour, the one under the pointer
        // a shade lighter than the strip: both shaped as a tab.
        if active {
            painter.rect_filled(rect, top, view::rgb(view::DEFAULT_BG));
        } else if hovered {
            painter.rect_filled(rect, top, theme::TAB_HOVER);
        }
        // A tab of a group starts with a bar in the group's colour.
        if let Some(color) = group {
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.left() + 3.0, rect.top() + 8.0),
                egui::pos2(rect.left() + 6.0, rect.bottom() - 8.0),
            );
            painter.rect_filled(bar, 2, color);
        }
        let mut x = rect.left() + pad;
        if locked {
            let lock_rect = egui::Rect::from_center_size(
                egui::pos2(x + 6.0, rect.center().y),
                egui::vec2(12.0, 12.0),
            );
            icons::draw(painter, icons::Icon::Lock, lock_rect, text_color);
            x += lock_w;
        }
        painter.galley(
            egui::pos2(x, rect.center().y - galley.size().y / 2.0),
            galley,
            text_color,
        );
        if active || hovered {
            if close.hovered() {
                painter.rect_filled(close_rect, 4, theme::PRESSED);
            }
            let mark = if close.hovered() {
                theme::TEXT_STRONG
            } else {
                text_color
            };
            icons::draw(painter, icons::Icon::Close, close_rect, mark);
        }
    }
    let close = close.on_hover_text("Close tab").clicked();
    (response, close)
}

/// The Font and Cursor dialog: its tab, and how the panes looked before,
/// for Cancel.
#[derive(Clone)]
struct FontDialog {
    tab: usize,
    before: Vec<view::Look>,
}

/// A multi-line paste waiting to be confirmed.
#[derive(Default)]
struct PasteConfirm {
    tab: usize,
    pane: usize,
    text: String,
}

/// `Cmd+key` on macOS, `Ctrl+key` elsewhere, for hints.
fn keys_label(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("⌘{key}")
    } else {
        format!("Ctrl+{key}")
    }
}

/// Where a pane is connected, for tooltips: `user@host:port` (the port only
/// when it is not the protocol's usual one), the serial line, or `None`.
fn pane_target(pane: &Pane) -> Option<String> {
    let session = pane.origin.as_ref()?;
    match &session.target {
        przmterm_session::Target::Network {
            host,
            port,
            username,
        } => {
            let usual = session.protocol.default_port().unwrap_or(0);
            let user = username
                .as_ref()
                .map(|u| format!("{u}@"))
                .unwrap_or_default();
            let port = if *port == usual {
                String::new()
            } else {
                format!(":{port}")
            };
            let protocol = match session.protocol {
                przmterm_core::Protocol::Telnet => "Telnet",
                _ => "SSH",
            };
            Some(format!("{protocol} {user}{host}{port}"))
        }
        przmterm_session::Target::Serial(line) => Some(format!("Serial {line}")),
        przmterm_session::Target::Local => None,
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
    /// Stays the same while the tab lives, whatever moves around it.
    id: u64,
    /// The window it shows in: 0 the main one ([`UNPLACED`] until placed).
    window: u32,
    /// The tab group it belongs to.
    group: Option<u32>,
    /// Locked with a password: the screen is hidden, and nothing is typed
    /// and the tab does not close until it is given again.
    lock: Option<przmterm_session::vault::Lock>,
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

/// A new tab's window, until it is placed in the window it was made in.
const UNPLACED: u32 = u32::MAX;

impl Tab {
    fn new(pane: Pane) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            window: UNPLACED,
            group: None,
            lock: None,
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

/// Another window, with tabs of its own.
struct Win {
    id: u32,
    /// Its active tab (by id).
    active: u64,
}

/// A tab group: its tabs sit together, under a coloured label.
struct Group {
    id: u32,
    name: String,
    color: egui::Color32,
}

/// The group name being typed: for this group, or (`None`) a new one made
/// of `tabs` (by id) only once the name is confirmed.
struct GroupName {
    group: Option<u32>,
    tabs: Vec<u64>,
    name: String,
}

/// The colours groups take in turn.
const GROUP_COLORS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(90, 160, 240),
    egui::Color32::from_rgb(110, 190, 110),
    egui::Color32::from_rgb(230, 160, 60),
    egui::Color32::from_rgb(200, 110, 200),
    egui::Color32::from_rgb(90, 190, 190),
    egui::Color32::from_rgb(220, 100, 100),
];

/// User actions, collected while drawing and applied afterwards.
enum Action {
    Open(Box<Session>),
    /// Opens a session in place of this pane (tab, pane): asked by the
    /// PrzmTerm shell there.
    OpenHere(usize, usize, Box<Session>),
    NewTab,
    /// A tab with the PrzmTerm shell.
    NewShellTab,
    CloseTab(usize),
    SelectTab(usize),
    /// Asks what to put next to the current session.
    Split(Layout),
    /// Splits the current tab: the panes `others` (tab, pane) move in from
    /// other tabs.
    SplitWith {
        layout: Layout,
        others: Vec<(usize, usize)>,
        /// And a new PrzmTerm shell.
        shell: bool,
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
    /// Opens an SCP browser on the active pane's connection of this tab,
    /// in the session's SCP shell.
    OpenScp(usize),
    /// Connects a session and opens its file browser once logged in:
    /// SCP with `true`, SFTP with `false`.
    BrowseFor(Box<Session>, bool),
    /// The Xmodem/Ymodem/Zmodem dialog for this tab: sending or receiving.
    TransferDialog(usize, bool),
    /// Stops the transfer of this tab's active pane.
    CancelTransfer(usize),
    ClosePane,
    FocusPane(usize),
    /// Swaps two panes of the active tab (dragged by the caption).
    SwapPanes(usize, usize),
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
    /// Puts these tabs in a new group.
    NewGroup(Vec<usize>),
    /// Puts the tab in this group (`None`: out of its group).
    SetGroup(usize, Option<u32>),
    RenameGroup(u32),
    /// Asks what to send to every session of the group.
    SendToGroup(u32),
    /// Closes the group's tabs (asking first if any is connected).
    CloseGroup(u32),
    /// The group's tabs stay, without the group.
    Ungroup(u32),
    /// Moves the tab to a new window; with `true`, a copy of it (a new
    /// connection to its session) goes instead.
    NewWindow(usize, bool),
    /// Moves the tab to the main window.
    ToMainWindow(usize),
    /// Asks a password and locks the tab.
    LockTab(usize),
}

struct PrzmApp {
    /// Interface fonts and the installed fonts the terminal can use.
    fonts: fonts::TermFonts,
    config_dir: Option<PathBuf>,
    sessions: SessionTree,
    tabs: Vec<Tab>,
    /// The active tab of the window being drawn (see `cur_window`).
    active_tab: usize,
    /// The window being drawn and acted on: 0 the main one.
    cur_window: u32,
    /// The other windows, each with tabs of its own.
    windows: Vec<Win>,
    next_window: u32,
    /// Tab groups, by id.
    groups: Vec<Group>,
    next_group: u32,
    /// The "Send to group" dialog: group, text, Enter after it.
    group_send: Option<(u32, String, bool)>,
    /// The dialog naming a new group, or renaming one.
    group_rename: Option<GroupName>,
    /// The "Lock" dialog: tab id, password, again.
    lock_dialog: Option<(u64, String, String)>,
    /// The password typed on a locked tab, and whether the last one was
    /// wrong.
    unlock_text: (String, bool),
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
    font_dialog: Option<FontDialog>,
    /// The "Split" dialog.
    split_dialog: Option<SplitDialog>,
    /// The keyboard shortcuts in force.
    shortcuts: keymap::Shortcuts,
    /// The keyboard shortcut editor, when open.
    keymap_editor: Option<keymap::KeymapEditor>,
    /// The Xmodem/Ymodem/Zmodem dialog.
    transfer_dialog: Option<transfer::TransferDialog>,
    /// Pasted text with line breaks, shown to check before it is sent.
    paste_confirm: Option<PasteConfirm>,
    /// SFTP browsers.
    sftp_windows: Vec<sftp::SftpBrowser>,
    sftp_next: u64,
    /// A session whose file browser opens once it is logged in (SCP when
    /// true).
    sftp_pending: Option<(PathBuf, std::time::Instant, bool)>,
    /// The script guide, when open.
    script_guide: Option<scriptguide::ScriptGuide>,
    /// The macOS menu bar (none elsewhere), kept while the app runs.
    menu_bar: Option<appmenu::MenuBar>,
    /// The About window, with the icon once loaded.
    about: Option<Option<egui::TextureHandle>>,
    /// The app's own preferences (`preferences.toml`).
    prefs: przmterm_session::preferences::Preferences,
    /// The last check for a newer release, and whether its window is open
    /// (a check at startup only shows in the status bar).
    update: Option<update::Check>,
    update_window: bool,
    /// Questions from scripts, asked one at a time.
    script_prompts: std::collections::VecDeque<ScriptPrompt>,
    /// A backup or export being restored or imported, and the folder name
    /// for an import.
    bundle_dialog: Option<(przmterm_session::bundle::Bundle, String)>,
    /// The "Settings folder" dialog: the folder being typed.
    folder_dialog: Option<String>,
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
    /// The file browser for a session, connecting it first if needed;
    /// `true` for SCP, `false` for SFTP.
    Browse(Session, bool),
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
/// A session file or a folder being dragged in the session list.
struct DragItem(PathBuf);

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
    fn new(fonts: fonts::TermFonts) -> Self {
        let config_dir = przmterm_session::default_config_dir();
        let prefs = config_dir
            .as_deref()
            .map(przmterm_session::preferences::Preferences::load)
            .unwrap_or_default();
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
            tabs: vec![Tab::new(Pane::launcher())],
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
            cur_window: 0,
            windows: Vec::new(),
            next_window: 1,
            groups: Vec::new(),
            next_group: 1,
            group_send: None,
            group_rename: None,
            lock_dialog: None,
            unlock_text: (String::new(), false),
            font_dialog: None,
            split_dialog: None,
            script_prompts: Default::default(),
            script_guide: None,
            menu_bar: None,
            about: None,
            prefs,
            update: None,
            update_window: false,
            transfer_dialog: None,
            paste_confirm: None,
            fonts,
            sftp_windows: Vec::new(),
            sftp_next: 0,
            sftp_pending: None,
            shortcuts,
            keymap_editor: None,
            bundle_dialog: None,
            folder_dialog: None,
            quitting: false,
        }
    }

    fn reload_sessions(&mut self) {
        if let Some(dir) = &self.config_dir {
            self.sessions = SessionTree::load(dir);
        }
        self.share_sessions();
        self.refresh_anti_idle();
    }

    /// Open sessions take a changed anti-idle at once: a saved session from
    /// its file (and folders), a quick connect from the global settings.
    fn refresh_anti_idle(&mut self) {
        let saved = self.sessions.root.all_sessions();
        let global = self.sessions.root.inherited.anti_idle();
        for pane in self.tabs.iter_mut().flat_map(|t| &mut t.panes) {
            if pane.is_launcher() {
                continue;
            }
            let Some(origin) = &mut pane.origin else {
                continue;
            };
            origin.anti_idle = if origin.path.as_os_str().is_empty() {
                global.clone()
            } else {
                match saved.iter().find(|s| s.path == origin.path) {
                    Some(session) => session.anti_idle.clone(),
                    None => continue,
                }
            };
        }
    }

    /// Tells the PrzmTerm shells which sessions there are, and gives them the
    /// font and cursor of the global settings.
    fn share_sessions(&mut self) {
        let mut defaults = self.sessions.root.inherited.clone();
        defaults.host = Some("localhost".into());
        if let Ok(session) = Session::resolve("PrzmTerm", Vec::new(), "", &defaults) {
            let look = view::TerminalView::for_session(&session).look();
            // Shells already open take a changed look too.
            if pane::shell_look().as_ref() != Some(&look) {
                pane::set_shell_look(look.clone());
                for pane in self.tabs.iter_mut().flat_map(|t| &mut t.panes) {
                    if pane.is_launcher() {
                        pane.view.set_look(&look);
                    }
                }
            }
        }
        launcher::set_catalog(
            self.sessions
                .root
                .all_sessions()
                .into_iter()
                .map(launcher::Entry::of)
                .collect(),
        );
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
            headless: false,
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
            Action::Open(session) | Action::OpenHere(_, _, session) => vec![(**session).clone()],
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

    /// A pane on `session`. Already connected to it? Another channel on that
    /// connection instead of logging in (and 2FA) again.
    fn pane_for(&self, session: &Session, env: &Env, waker: Waker) -> Pane {
        let live = self.tabs.iter().flat_map(|t| &t.panes).find(|p| {
            !p.transport.is_closed()
                && p.origin
                    .as_ref()
                    .is_some_and(|o| o.path == session.path && o.name == session.name)
        });
        match live {
            Some(p) => p.clone_session(env, waker),
            None => {
                let size = self.tab().pane().size();
                Pane::open(session, env, size, waker)
            }
        }
    }

    /// What the PrzmTerm shells asked for, as actions.
    fn launcher_requests(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        for (t, tab) in self.tabs.iter_mut().enumerate() {
            for (p, pane) in tab.panes.iter_mut().enumerate() {
                for request in pane.take_requests() {
                    let session = match request {
                        launcher::Request::Connect(target) => {
                            Session::quick_connect_with(&target, &self.sessions.root.inherited).ok()
                        }
                        launcher::Request::Open(label) => self
                            .sessions
                            .root
                            .all_sessions()
                            .into_iter()
                            .find(|s| launcher::Entry::of(s).label == label)
                            .cloned(),
                    };
                    match session {
                        Some(s) => actions.push(Action::OpenHere(t, p, Box::new(s))),
                        None => pane
                            .term
                            .feed(b"\x1b[31mThat session is gone; try sessions.\x1b[0m\r\n"),
                    }
                }
            }
        }
        actions
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        if self.locked_target(&action) {
            self.notice = Some("The tab is locked: unlock it first.".into());
            return;
        }
        self.apply_inner(action, ctx);
        self.settle_tabs();
    }

    /// Whether `action` would type into, close or reach the session of a
    /// locked tab.
    fn locked_target(&self, action: &Action) -> bool {
        let locked = |t: usize| self.tabs.get(t).is_some_and(|tab| tab.lock.is_some());
        let active = self.active_tab;
        match action {
            Action::Macro(_)
            | Action::SendBreak
            | Action::Reconnect
            | Action::RequestClosePane
            | Action::ClosePane
            | Action::Split(_)
            | Action::SplitWith { .. }
            | Action::SeparatePane
            | Action::NewTab
            | Action::ToggleBroadcast => locked(active),
            Action::CloseTab(t)
            | Action::ReconnectTab(t)
            | Action::DisconnectTab(t)
            | Action::CloneTab(t)
            | Action::SeparateAll(t)
            | Action::SessionOptions(t)
            | Action::RunScript(t)
            | Action::StopScript(t)
            | Action::OpenSftp(t)
            | Action::OpenScp(t)
            | Action::TransferDialog(t, _)
            | Action::NewWindow(t, true) => locked(*t),
            Action::RequestClose(tabs) => tabs.iter().any(|&t| locked(t)),
            _ => false,
        }
    }

    /// After tabs moved: new ones go in the current window, groups keep
    /// their tabs together, and the current window's active tab is one of
    /// its own (the main window always has one).
    fn settle_tabs(&mut self) {
        let window = self.cur_window;
        for tab in &mut self.tabs {
            if tab.window == UNPLACED {
                tab.window = window;
            }
        }
        let active = self.tabs.get(self.active_tab).map(|t| t.id);
        // Each group's tabs right after its first one.
        let mut order: Vec<Tab> = Vec::with_capacity(self.tabs.len());
        for tab in std::mem::take(&mut self.tabs) {
            match tab.group {
                Some(g) => match order.iter().rposition(|t| t.group == Some(g)) {
                    Some(last) => order.insert(last + 1, tab),
                    None => order.push(tab),
                },
                None => order.push(tab),
            }
        }
        self.tabs = order;
        self.groups
            .retain(|g| self.tabs.iter().any(|t| t.group == Some(g.id)));
        let mine = |t: &Tab| t.window == window;
        let found = active.and_then(|id| self.tabs.iter().position(|t| t.id == id && mine(t)));
        self.active_tab = match found {
            Some(i) => i,
            None => {
                // The nearest tab of this window, before the one that went.
                let before = self.active_tab.min(self.tabs.len());
                match self.tabs[..before]
                    .iter()
                    .rposition(mine)
                    .or_else(|| self.tabs.iter().position(mine))
                {
                    Some(i) => i,
                    None if window == 0 => {
                        let mut tab = Tab::new(Pane::launcher());
                        tab.window = 0;
                        self.tabs.push(tab);
                        self.tabs.len() - 1
                    }
                    // An emptied window closes; any tab will do meanwhile.
                    None => 0,
                }
            }
        };
    }

    /// Draws the other windows: each its tabs and its active tab's panes.
    /// Dialogs open in the main window.
    fn show_windows(&mut self, ctx: &egui::Context) {
        let main = self.tabs.get(self.active_tab).map(|t| t.id);
        let ids: Vec<u32> = self.windows.iter().map(|w| w.id).collect();
        for id in ids {
            let Some(w) = self.windows.iter().position(|w| w.id == id) else {
                continue;
            };
            let active = self.windows[w].active;
            let Some(index) = self
                .tabs
                .iter()
                .position(|t| t.id == active && t.window == id)
                .or_else(|| self.tabs.iter().position(|t| t.window == id))
            else {
                continue;
            };
            self.cur_window = id;
            self.active_tab = index;
            let title = format!("{} — PrzmTerm", self.tabs[index].title());
            let mut actions = Vec::new();
            let mut closing = false;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of(("przm-window", id)),
                egui::ViewportBuilder::default()
                    .with_title(title)
                    .with_inner_size([960.0, 600.0])
                    .with_min_inner_size([420.0, 260.0]),
                |ui, _| {
                    let ctx = ui.ctx().clone();
                    if ctx.input(|i| i.viewport().close_requested()) {
                        // Its tabs close first (asking if connected).
                        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                        closing = true;
                    }
                    actions.extend(self.shortcuts(&ctx));
                    egui::Panel::top("tabs")
                        .frame(theme::tab_strip())
                        .show_separator_line(false)
                        .show(ui, |ui| self.tab_bar(ui, &mut actions));
                    self.terminal_area(ui, &mut actions);
                },
            );
            if closing {
                actions.push(Action::RequestClose(self.window_tabs()));
                // The question shows in the main window.
                ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
            }
            for action in actions {
                self.apply(action, ctx);
            }
            if let Some(tab) = self.tabs.get(self.active_tab).filter(|t| t.window == id)
                && let Some(win) = self.windows.iter_mut().find(|w| w.id == id)
            {
                win.active = tab.id;
            }
        }
        // Back to the main window.
        self.cur_window = 0;
        self.windows
            .retain(|w| self.tabs.iter().any(|t| t.window == w.id));
        self.active_tab = main
            .and_then(|id| self.tabs.iter().position(|t| t.id == id && t.window == 0))
            .or_else(|| self.tabs.iter().position(|t| t.window == 0))
            .unwrap_or(0);
        self.settle_tabs();
    }

    /// The dialogs of tab groups and locking: naming a group, sending to
    /// it, and the password that locks a tab.
    fn group_dialogs(
        &mut self,
        ctx: &egui::Context,
        center: egui::Pos2,
        enter: bool,
        escape: bool,
    ) {
        let window = |title: &'static str| {
            egui::Window::new(title)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(center)
                .collapsible(false)
                .resizable(false)
        };
        let buttons = |ui: &mut egui::Ui, ok: &str, can: bool, answer: &mut Option<bool>| {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(can, egui::Button::new(ok)).clicked() {
                        *answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() || escape {
                        *answer = Some(false);
                    }
                });
            });
        };
        if let Some(naming) = &mut self.group_rename {
            let mut answer = None;
            let title = if naming.group.is_some() {
                "Rename group"
            } else {
                "New group"
            };
            window(title).show(ctx, |ui| {
                ui.add(egui::TextEdit::singleline(&mut naming.name).desired_width(240.0))
                    .request_focus();
                buttons(ui, "OK", true, &mut answer);
            });
            if enter {
                answer = Some(true);
            }
            if let Some(yes) = answer {
                let naming = self.group_rename.take().expect("open");
                let name = naming.name.trim().to_string();
                if yes {
                    match naming.group {
                        Some(g) => {
                            if !name.is_empty()
                                && let Some(group) = self.groups.iter_mut().find(|x| x.id == g)
                            {
                                group.name = name;
                            }
                        }
                        // Only now does the group exist.
                        None => self.new_group(&naming.tabs, name),
                    }
                }
            }
        }
        if let Some((g, text, enter_after)) = &mut self.group_send {
            let g = *g;
            let name = self
                .groups
                .iter()
                .find(|x| x.id == g)
                .map(|x| x.name.clone())
                .unwrap_or_default();
            let targets: Vec<(usize, usize)> = self
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, t)| t.group == Some(g) && t.lock.is_none())
                .flat_map(|(ti, t)| {
                    t.panes
                        .iter()
                        .enumerate()
                        .filter(|(_, p)| p.origin.is_some() && !p.transport.is_closed())
                        .map(move |(pi, _)| (ti, pi))
                })
                .collect();
            let mut answer = None;
            window("Send to group").show(ctx, |ui| {
                ui.label(format!(
                    "To {} connected session{} of {name}:",
                    targets.len(),
                    if targets.len() == 1 { "" } else { "s" }
                ));
                ui.add(
                    egui::TextEdit::multiline(text)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(3)
                        .desired_width(380.0)
                        .hint_text("one command per line"),
                )
                .request_focus();
                ui.checkbox(enter_after, "Press Enter after each line");
                ui.weak("Locked tabs are left out.");
                buttons(
                    ui,
                    "Send",
                    !targets.is_empty() && !text.is_empty(),
                    &mut answer,
                );
            });
            if let Some(yes) = answer {
                if yes {
                    let mut bytes = Vec::new();
                    if *enter_after {
                        for line in text.lines() {
                            bytes.extend_from_slice(line.as_bytes());
                            bytes.push(b'\r');
                        }
                    } else {
                        bytes.extend_from_slice(text.as_bytes());
                    }
                    for (t, p) in &targets {
                        self.tabs[*t].panes[*p].send(&bytes);
                    }
                    self.notice = Some(format!("Sent to {} session(s) of {name}.", targets.len()));
                }
                self.group_send = None;
            }
        }
        if let Some((id, first, again)) = &mut self.lock_dialog {
            let mut answer = None;
            let can = !first.is_empty() && first == again;
            window("Lock tab").show(ctx, |ui| {
                ui.set_max_width(300.0);
                ui.label(
                    "The tab hides its screen, and nothing is typed in it nor closes it \
                     until this password is given again. It is not saved anywhere.",
                );
                ui.add_space(4.0);
                let field = ui.add(
                    egui::TextEdit::singleline(first)
                        .password(true)
                        .hint_text("Password")
                        .desired_width(f32::INFINITY),
                );
                // Focus once, so Tab can move on to the second field.
                if ui.memory(|m| m.focused().is_none()) {
                    field.request_focus();
                }
                ui.add(
                    egui::TextEdit::singleline(again)
                        .password(true)
                        .hint_text("Again")
                        .desired_width(f32::INFINITY),
                );
                if !again.is_empty() && first != again {
                    ui.colored_label(ui.visuals().warn_fg_color, "The two do not match.");
                }
                buttons(ui, "Lock", can, &mut answer);
            });
            if enter && can {
                answer = Some(true);
            }
            if let Some(yes) = answer {
                if yes {
                    let id = *id;
                    match przmterm_session::vault::Lock::new(first) {
                        Ok(lock) => {
                            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                                tab.lock = Some(lock);
                            }
                            self.broadcast = false;
                        }
                        Err(e) => self.notice = Some(format!("Failed: {e}")),
                    }
                }
                self.lock_dialog = None;
            }
        }
    }

    /// "Group N" with the lowest N no open group is called.
    fn free_group_name(&self) -> String {
        (1..)
            .map(|n| format!("Group {n}"))
            .find(|name| {
                !self
                    .groups
                    .iter()
                    .any(|g| g.name.eq_ignore_ascii_case(name))
            })
            .unwrap_or_default()
    }

    /// A group of the tabs `tabs` (by id) called `name`.
    fn new_group(&mut self, tabs: &[u64], name: String) {
        let id = self.next_group;
        self.next_group += 1;
        let used: Vec<egui::Color32> = self.groups.iter().map(|g| g.color).collect();
        let color = GROUP_COLORS
            .iter()
            .copied()
            .find(|c| !used.contains(c))
            .unwrap_or(GROUP_COLORS[id as usize % GROUP_COLORS.len()]);
        self.groups.push(Group {
            id,
            name: if name.is_empty() {
                self.free_group_name()
            } else {
                name
            },
            color,
        });
        for tab in &mut self.tabs {
            if tabs.contains(&tab.id) {
                tab.group = Some(id);
            }
        }
        self.settle_tabs();
    }

    /// A locked tab: in place of its panes, the password that opens it.
    fn locked_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        ui.painter()
            .rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        let mut unlock = false;
        let dialog = self.modal_open();
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((rect.height() / 2.0 - 70.0).max(10.0));
                let (icon, _) =
                    ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                icons::draw_scaled(
                    ui.painter(),
                    icons::Icon::Lock,
                    icon,
                    ui.visuals().text_color(),
                    2.2,
                );
                ui.add_space(6.0);
                ui.strong("This tab is locked");
                ui.add_space(6.0);
                let (text, wrong) = &mut self.unlock_text;
                let field = ui.add(
                    egui::TextEdit::singleline(text)
                        .password(true)
                        .hint_text("Password")
                        .desired_width(220.0),
                );
                if !dialog {
                    field.request_focus();
                }
                if field.changed() {
                    *wrong = false;
                }
                if *wrong {
                    ui.colored_label(ui.visuals().error_fg_color, "Wrong password.");
                }
                ui.add_space(4.0);
                unlock = ui.button("Unlock").clicked()
                    || (field.has_focus() || field.lost_focus())
                        && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
        });
        if unlock {
            let tab = &mut self.tabs[self.active_tab];
            let (text, wrong) = &mut self.unlock_text;
            if tab.lock.as_ref().is_some_and(|l| l.opens(text)) {
                tab.lock = None;
            } else {
                *wrong = true;
            }
            text.clear();
        }
    }

    /// The tabs of the current window, in order.
    fn window_tabs(&self) -> Vec<usize> {
        (0..self.tabs.len())
            .filter(|&i| self.tabs[i].window == self.cur_window)
            .collect()
    }

    fn apply_inner(&mut self, action: Action, ctx: &egui::Context) {
        let ctx2 = ctx.clone();
        let waker: Waker = Arc::new(move || ctx2.request_repaint());
        let env = self.env();
        if let Some(why) = self.needs_unlock(&action, &env) {
            self.vault.ask_unlock(why);
            self.after_unlock = Some(action);
            return;
        }
        match action {
            Action::OpenHere(t, p, session) => {
                let here = self
                    .tabs
                    .get(t)
                    .and_then(|tab| tab.panes.get(p))
                    .is_some_and(|pane| pane.is_launcher());
                if !here {
                    // The tabs changed meanwhile (the vault was asked first).
                    return self.apply(Action::Open(session), ctx);
                }
                let pane = self.pane_for(&session, &env, waker);
                let old = std::mem::replace(&mut self.tabs[t].panes[p], pane);
                drop(old);
                self.active_tab = t;
                self.tabs[t].active = p;
            }
            Action::Open(session) => {
                let pane = self.pane_for(&session, &env, waker);
                // The first PrzmTerm shell is just a starting point: replace it.
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
                    self.tabs.push(Tab::new(Pane::launcher()));
                }
                // The tab before it in the same window, as browsers do.
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
                // With no other session, the shell is the only choice.
                let shell = others.is_empty();
                self.split_dialog = Some(SplitDialog {
                    layout,
                    shell,
                    others,
                });
            }
            Action::SplitWith {
                layout,
                others,
                shell,
            } => {
                self.active_tab = merge_panes(&mut self.tabs, self.active_tab, &others);
                let tab = self.tab_mut();
                tab.layout = layout;
                if shell {
                    tab.panes.push(Pane::launcher());
                    tab.active = tab.panes.len() - 1;
                }
            }
            Action::NewShellTab => {
                self.tabs.push(Tab::new(Pane::launcher()));
                self.active_tab = self.tabs.len() - 1;
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
            Action::SwapPanes(a, b) => {
                let tab = self.tab_mut();
                if a != b && a < tab.panes.len() && b < tab.panes.len() {
                    tab.panes.swap(a, b);
                    // The keyboard stays with the pane that moved.
                    tab.active = b;
                }
            }
            Action::ToggleSessions => self.sessions_hidden = !self.sessions_hidden,
            Action::NewGroup(tabs) => {
                // Made once it is named; Cancel leaves the tabs as they are.
                self.group_rename = Some(GroupName {
                    group: None,
                    tabs: tabs
                        .iter()
                        .filter_map(|&t| self.tabs.get(t).map(|tab| tab.id))
                        .collect(),
                    name: self.free_group_name(),
                });
            }
            Action::SetGroup(t, group) => {
                if let Some(tab) = self.tabs.get_mut(t) {
                    tab.group = group;
                }
            }
            Action::RenameGroup(g) => {
                if let Some(group) = self.groups.iter().find(|x| x.id == g) {
                    self.group_rename = Some(GroupName {
                        group: Some(g),
                        tabs: Vec::new(),
                        name: group.name.clone(),
                    });
                }
            }
            Action::SendToGroup(g) => self.group_send = Some((g, String::new(), true)),
            Action::CloseGroup(g) => {
                let tabs = (0..self.tabs.len())
                    .filter(|&t| self.tabs[t].group == Some(g))
                    .collect();
                self.apply_inner(Action::RequestClose(tabs), ctx);
            }
            Action::Ungroup(g) => {
                for tab in &mut self.tabs {
                    if tab.group == Some(g) {
                        tab.group = None;
                    }
                }
            }
            Action::NewWindow(t, copy) => {
                let id = self.next_window;
                self.next_window += 1;
                let tab_id = if copy {
                    let source = &self.tabs[t];
                    let pane = if source.pane().origin.is_some() {
                        source.pane().clone_session(&env, waker)
                    } else {
                        Pane::launcher()
                    };
                    let mut tab = Tab::new(pane);
                    tab.window = id;
                    let tab_id = tab.id;
                    self.tabs.push(tab);
                    tab_id
                } else {
                    let tab = &mut self.tabs[t];
                    tab.window = id;
                    // A group stays in one window.
                    tab.group = None;
                    tab.id
                };
                self.windows.push(Win { id, active: tab_id });
            }
            Action::ToMainWindow(t) => {
                if let Some(tab) = self.tabs.get_mut(t) {
                    tab.window = 0;
                    tab.group = None;
                }
            }
            Action::LockTab(t) => {
                if let Some(tab) = self.tabs.get(t) {
                    self.lock_dialog = Some((tab.id, String::new(), String::new()));
                }
            }
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
            Action::FontTab(t) => {
                if let Some(tab) = self.tabs.get(t) {
                    self.font_dialog = Some(FontDialog {
                        tab: t,
                        before: tab.panes.iter().map(|p| p.view.look()).collect(),
                    });
                }
            }
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
                            "SFTP",
                        ));
                    }
                    None => {
                        self.notice =
                            Some("SFTP needs a connected SSH session (log in first).".into())
                    }
                }
            }
            Action::OpenScp(t) => {
                let pane = self.tabs[t].pane();
                let name = pane
                    .origin
                    .as_ref()
                    .map_or_else(|| pane.title(), |s| s.name.clone());
                let shell = pane.origin.as_ref().and_then(|s| s.scp_shell.clone());
                match pane.transport.scp_browser(shell, waker) {
                    Some(client) => {
                        self.sftp_next += 1;
                        self.sftp_windows.push(sftp::SftpBrowser::new(
                            name,
                            self.sftp_next,
                            client,
                            "SCP",
                        ));
                    }
                    None => {
                        self.notice =
                            Some("SCP needs a connected SSH session (log in first).".into())
                    }
                }
            }
            Action::BrowseFor(session, scp) => {
                self.sftp_pending = Some((session.path.clone(), std::time::Instant::now(), scp));
                // Already connected: the browser right away; otherwise connect first.
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
                    // SCP runs on a channel of the same SSH connection.
                    let ssh = pane.transport.protocol() == przmterm_core::Protocol::Ssh;
                    let shell = pane.origin.as_ref().and_then(|s| s.scp_shell.clone());
                    self.transfer_dialog =
                        Some(transfer::TransferDialog::new(t, sending, unix, ssh, shell));
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

    /// Opens the file browser asked for from the session list once its
    /// session is logged in.
    fn run_sftp_pending(&mut self, ctx: &egui::Context) {
        let Some((path, since, scp)) = self.sftp_pending.clone() else {
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
        let client = if scp {
            let shell = pane.origin.as_ref().and_then(|o| o.scp_shell.clone());
            pane.transport.scp_browser(shell, waker)
        } else {
            pane.transport.sftp(waker)
        };
        if let Some(client) = client {
            let name = pane
                .origin
                .as_ref()
                .map(|o| o.name.clone())
                .unwrap_or_default();
            self.sftp_next += 1;
            let protocol = if scp { "SCP" } else { "SFTP" };
            self.sftp_windows.push(sftp::SftpBrowser::new(
                name,
                self.sftp_next,
                client,
                protocol,
            ));
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
            .flat_map(|p| [p.anti_idle(now), p.run_queue(now)])
            .flatten()
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
        // Next and previous go round the tabs of this window.
        let mine = self.window_tabs();
        let at = mine.iter().position(|&i| i == t).unwrap_or(0);
        let step = |by: usize| mine[(at + by) % mine.len()];
        let n = mine.len().max(1);
        use przmterm_session::keymap::Does;
        Some(match binding.does().ok()? {
            Does::Command(id) => match id {
                "new_tab" => Action::NewTab,
                "new_shell_tab" => Action::NewShellTab,
                "close_pane" => Action::RequestClosePane,
                "close_tab" => Action::RequestClose(vec![t]),
                "next_tab" => Action::SelectTab(step(1)),
                "previous_tab" => Action::SelectTab(step(n - 1)),
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
                "open_scp" => Action::OpenScp(t),
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
        // Shaped like the other status bar buttons: filled green while
        // logging, red while not, with a dot that keeps the state readable
        // without colour (full when logging, hollow-looking when not).
        let green = egui::Color32::from_rgb(40, 135, 65);
        let red = egui::Color32::from_rgb(160, 52, 52);
        let (text, fill, dot) = if logging {
            (
                format!("Log  {}", short_name(name.as_deref().unwrap_or(""))),
                green,
                egui::Color32::WHITE,
            )
        } else {
            ("Log".to_string(), red, egui::Color32::from_white_alpha(110))
        };
        let response = icons::dot_button(ui, dot, &text, Some(fill), Some(egui::Color32::WHITE))
            .on_hover_text(match (&name, logging) {
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
    /// Shows a multi-line paste to check (and edit) before it is sent.
    fn paste_dialog(&mut self, ctx: &egui::Context, center: egui::Pos2, escape: bool) {
        let Some(confirm) = &mut self.paste_confirm else {
            return;
        };
        // Taken before the text box sees it (there Enter adds a line).
        let send_keys =
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        let lines = confirm.text.lines().count().max(1);
        let mut done = None;
        // Wide by default; the corner drags it to any size.
        let rows = lines.clamp(3, 14) as f32;
        let row_h = ctx.fonts_mut(|f| f.row_height(&egui::FontId::monospace(14.0)));
        egui::Window::new("Paste")
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .collapsible(false)
            .resizable(true)
            .default_size([840.0, rows * row_h + 110.0])
            .min_size([420.0, 200.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.label(format!(
                    "The text has {lines} line{}: each line break works as Enter, \
                     so every line runs as a command. Check it, or edit it here.",
                    if lines == 1 { "" } else { "s" }
                ));
                ui.add_space(4.0);
                // The text fills the window; the buttons keep their row.
                let footer = ui.spacing().interact_size.y + ui.spacing().item_spacing.y * 2.0;
                egui::ScrollArea::both()
                    .auto_shrink(false)
                    .max_height((ui.available_height() - footer).max(60.0))
                    .show(ui, |ui| {
                        ui.add_sized(
                            ui.available_size(),
                            egui::TextEdit::multiline(&mut confirm.text)
                                .font(egui::TextStyle::Monospace)
                                .code_editor(),
                        );
                    });
                ui.horizontal(|ui| {
                    ui.weak(format!("{} to paste", keys_label("Enter")));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Paste").clicked() || send_keys {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            done = Some(false);
                        }
                    });
                });
            });
        match done {
            Some(true) => {
                let PasteConfirm { tab, pane, text } =
                    self.paste_confirm.take().unwrap_or_default();
                let broadcast = self.broadcast && tab == self.active_tab;
                if let Some(t) = self.tabs.get_mut(tab) {
                    let bracketed = t
                        .panes
                        .get(pane)
                        .is_some_and(|p| p.term.screen().bracketed_paste());
                    let bytes = crate::input::encode_paste(&text, bracketed);
                    for (i, p) in t.panes.iter_mut().enumerate() {
                        if (i == pane || broadcast) && !p.transport.is_closed() {
                            p.send(&bytes);
                        }
                    }
                }
            }
            Some(false) => self.paste_confirm = None,
            None => {}
        }
    }

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
            || self.paste_confirm.is_some()
            || self.bundle_dialog.is_some()
            || self.folder_dialog.is_some()
            || self.group_send.is_some()
            || self.group_rename.is_some()
            || self.lock_dialog.is_some()
            || self.vault.modal()
    }

    /// Runs a command of the app menu (macOS) or the **+** menu.
    fn app_command(&mut self, ctx: &egui::Context, command: appmenu::AppCommand) {
        use appmenu::AppCommand;
        match command {
            AppCommand::About => {
                self.about.get_or_insert(None);
            }
            AppCommand::CheckUpdates => {
                self.update = Some(update::Check::start(ctx));
                self.update_window = true;
            }
            AppCommand::Settings => {
                if let Some(dir) = &self.config_dir {
                    self.editor = Some(Editor::global(dir));
                }
            }
            AppCommand::Shortcuts => {
                self.keymap_editor = Some(keymap::KeymapEditor::new(&self.shortcuts));
            }
            AppCommand::Vault => self.vault.open_window(),
            AppCommand::Keywords => {
                if let Some(dir) = &self.config_dir {
                    self.keyword_editor = Some(keywords::KeywordEditor::new(dir, None));
                }
            }
            AppCommand::SettingsFolder => {
                self.folder_dialog = Some(
                    self.config_dir
                        .as_deref()
                        .map(|d| d.display().to_string())
                        .unwrap_or_default(),
                );
            }
            AppCommand::ScriptGuide => {
                self.script_guide
                    .get_or_insert_with(scriptguide::ScriptGuide::new);
            }
            AppCommand::Website => ctx.open_url(egui::OpenUrl::new_tab(appmenu::WEBSITE)),
            // As closing the window: asks first when sessions are open.
            AppCommand::Quit => {
                ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
            }
        }
    }

    /// The "Check for Updates" window.
    fn update_ui(&mut self, ctx: &egui::Context) {
        if !self.update_window {
            return;
        }
        let outcome = self.update.as_ref().and_then(update::Check::outcome);
        let mut open = true;
        let mut check_at_start = self.prefs.check_for_updates;
        egui::Window::new("Check for Updates")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .auto_sized()
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(320.0);
                match &outcome {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Looking for a newer release…");
                        });
                    }
                    Some(update::Outcome::UpToDate) => {
                        ui.label(format!(
                            "PrzmTerm {} is the newest version.",
                            przmterm_core::VERSION
                        ));
                    }
                    Some(update::Outcome::Newer(found)) => {
                        ui.strong(format!("PrzmTerm {} is available.", found.version));
                        ui.label(format!("You have {}.", przmterm_core::VERSION));
                        if ui.button("Open the Download Page").clicked() {
                            ctx.open_url(egui::OpenUrl::new_tab(&found.url));
                        }
                    }
                    Some(update::Outcome::Failed(e)) => {
                        ui.label("Could not reach GitHub to check:");
                        ui.weak(e);
                    }
                }
                ui.add_space(6.0);
                ui.checkbox(&mut check_at_start, "Check when PrzmTerm starts");
            });
        if check_at_start != self.prefs.check_for_updates {
            self.prefs.check_for_updates = check_at_start;
            if let Some(dir) = &self.config_dir
                && let Err(e) = self.prefs.save(dir)
            {
                self.notice = Some(format!("Preferences not saved: {e}"));
            }
        }
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.update_window = false;
        }
    }

    /// The About window.
    fn about_ui(&mut self, ctx: &egui::Context) {
        let Some(icon) = &mut self.about else {
            return;
        };
        let icon = icon.get_or_insert_with(|| {
            let png =
                eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-256.png"))
                    .unwrap_or_default();
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [png.width as usize, png.height as usize],
                &png.rgba,
            );
            ctx.load_texture("przmterm-icon", image, egui::TextureOptions::LINEAR)
        });
        let mut open = true;
        egui::Window::new("About PrzmTerm")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .auto_sized()
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| appmenu::about(ui, Some(icon)));
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.about = None;
        }
    }

    /// The "Settings folder" dialog.
    fn folder_dialog_ui(&mut self, ctx: &egui::Context, center: egui::Pos2, escape: bool) {
        use przmterm_session::location::{self, Contents};
        let Some(typed) = &mut self.folder_dialog else {
            return;
        };
        let standard = location::standard_dir();
        let forced = location::env_dir().is_some();
        let current = self.config_dir.clone();
        let wanted = przmterm_session::expand_tilde(typed.trim());
        // What choosing the typed folder would do, and whether it can.
        let (note, ok) = if forced {
            (
                "PRZMTERM_HOME is set: it decides the folder while it is.",
                None,
            )
        } else if typed.trim().is_empty() {
            ("Type or browse to a folder.", None)
        } else if !wanted.is_absolute() {
            ("Give the whole path of the folder.", Some(false))
        } else if current
            .as_deref()
            .is_some_and(|c| location::same_dir(&wanted, c))
        {
            ("This is the folder in use.", None)
        } else if current
            .as_deref()
            .is_some_and(|c| location::is_inside(&wanted, c))
        {
            ("Choose a folder outside the one in use.", Some(false))
        } else {
            match location::contents(&wanted) {
                Contents::Empty => (
                    "The settings are copied there; the ones in use now stay as they are.",
                    Some(true),
                ),
                Contents::Settings => (
                    "That folder has PrzmTerm settings: they are used as they are, \
                     and nothing is copied.",
                    Some(true),
                ),
                Contents::Other => (
                    "That folder has other files: choose an empty folder or one with \
                     PrzmTerm settings.",
                    Some(false),
                ),
            }
        };
        // `None`: nothing to choose yet; `Some(false)`: the folder will not do.
        let problem = ok == Some(false);
        let ok = ok == Some(true);
        let mut answer = None; // Some(true) use it, Some(false) cancel
        egui::Window::new("Settings folder")
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(
                    "Where PrzmTerm keeps sessions, defaults, buttons, keywords, scripts \
                     and the password vault. A synced folder (iCloud Drive, Dropbox, \
                     OneDrive) shares them between computers.",
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_enabled(
                        !forced,
                        egui::TextEdit::singleline(typed).desired_width(340.0),
                    );
                    if ui
                        .add_enabled(!forced, egui::Button::new("Browse…"))
                        .clicked()
                        && let Some(dir) = rfd::FileDialog::new()
                            .set_title("Folder for the PrzmTerm settings")
                            .pick_folder()
                    {
                        *typed = dir.display().to_string();
                    }
                });
                ui.add_space(2.0);
                if problem {
                    ui.colored_label(ui.visuals().warn_fg_color, note);
                } else {
                    ui.weak(note);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if let Some(standard) = &standard
                        && ui
                            .add_enabled(!forced, egui::Button::new("Default"))
                            .on_hover_text("The system's folder for application settings")
                            .clicked()
                    {
                        *typed = standard.display().to_string();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(ok, egui::Button::new("Use folder"))
                            .clicked()
                        {
                            answer = Some(true);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            answer = Some(false);
                        }
                    });
                });
            });
        match answer {
            Some(true) => {
                self.folder_dialog = None;
                let result = self.switch_config(&wanted);
                self.notice = Some(result.unwrap_or_else(|e| format!("Failed: {e}")));
            }
            Some(false) => self.folder_dialog = None,
            None => {}
        }
    }

    /// Makes `dir` the settings folder, copying the current settings there
    /// when it is empty, and loads what it holds.
    fn switch_config(&mut self, dir: &std::path::Path) -> Result<String, String> {
        use przmterm_session::location::{self, Contents};
        let standard = location::standard_dir().ok_or("no system settings folder")?;
        let copied = match (location::contents(dir), &self.config_dir) {
            (Contents::Other, _) => return Err("that folder has other files".into()),
            (Contents::Empty, Some(current)) => {
                location::copy_settings(current, dir).map_err(|e| e.to_string())?
            }
            (Contents::Empty, None) => {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                0
            }
            (Contents::Settings, _) => 0,
        };
        location::choose(&standard, Some(dir)).map_err(|e| e.to_string())?;
        self.config_dir = Some(dir.to_path_buf());
        self.reload_sessions();
        self.buttons = buttonbar::ButtonBarUi::load(Some(dir));
        self.shortcuts = keymap::Shortcuts::load(Some(dir));
        self.vault.lock();
        self.editor = None;
        self.keyword_editor = None;
        self.prefs = przmterm_session::preferences::Preferences::load(dir);
        Ok(if copied > 0 {
            format!("Settings folder changed; {copied} file(s) copied.")
        } else {
            "Settings folder changed.".to_string()
        })
    }

    /// The tabs of the current window, with + at the end.
    fn tab_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(2.0, 0.0);
            let mut last_group = None;
            for (i, tab) in self.tabs.iter().enumerate() {
                if tab.window != self.cur_window {
                    continue;
                }
                // A group starts with its label.
                if tab.group != last_group
                    && let Some(group) = tab
                        .group
                        .and_then(|g| self.groups.iter().find(|x| x.id == g))
                {
                    group_label(ui, group, actions);
                }
                last_group = tab.group;
                let mut title = tab.title();
                if title.chars().count() > 28 {
                    title = title.chars().take(27).collect::<String>() + "…";
                }
                if tab.pane().transport.is_closed() && tab.pane().origin.is_some() {
                    title = format!("○ {title}");
                } else if !tab.pane().transport.weak_algorithms().is_empty() {
                    title = format!("⚠ {title}");
                }
                // The session and where it is connected; a split tab
                // lists each of its sessions.
                let describe = |p: &Pane| match pane_target(p) {
                    Some(target) => format!("{}\n{target}", pane_name(p)),
                    None => pane_name(p),
                };
                let hover = if tab.panes.len() > 1 {
                    let names: Vec<String> = tab
                        .panes
                        .iter()
                        .map(|p| format!("• {}", describe(p).replace('\n', "\n   ")))
                        .collect();
                    format!("{}\nRight-click for more", names.join("\n"))
                } else {
                    format!("{}\nRight-click for more", describe(tab.pane()))
                };
                let color = tab
                    .group
                    .and_then(|g| self.groups.iter().find(|x| x.id == g))
                    .map(|g| g.color);
                let (tab_button, close) =
                    tab_widget(ui, &title, i == self.active_tab, tab.lock.is_some(), color);
                let tab_button = tab_button.on_hover_text(hover);
                if tab_button.clicked() {
                    actions.push(Action::SelectTab(i));
                }
                if tab_button.middle_clicked() || close {
                    actions.push(Action::RequestClose(vec![i]));
                }
                tab_button.context_menu(|ui| {
                    tab_menu(
                        ui,
                        &self.tabs,
                        &self.groups,
                        i,
                        self.active_tab,
                        self.broadcast,
                        actions,
                    );
                });
            }
            if icons::button(ui, icons::Icon::Plus, false)
                .on_hover_text(format!(
                    "New PrzmTerm tab{}\nAlso: double-click the empty bar",
                    self.keys_hint("new_shell_tab")
                ))
                .clicked()
            {
                actions.push(Action::NewShellTab);
            }
            // A double click on the free part of the bar does the same.
            let free = ui.available_rect_before_wrap();
            if free.width() > 0.0
                && ui
                    .interact(free, egui::Id::unique("tab-bar-free"), egui::Sense::click())
                    .double_clicked()
            {
                actions.push(Action::NewShellTab);
            }
        });
    }

    /// The current window's active tab: its panes, typing, pasting, and
    /// "send to all".
    fn terminal_area(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        if self.tab().lock.is_some() {
            egui::CentralPanel::default().show(ui, |ui| self.locked_ui(ui));
            return;
        }
        let modal = self.modal_open();
        // What was typed in the active pane, for "send to all".
        // Each pane draws in the font it asks for (loaded when first asked).
        for pane in &mut self.tabs[self.active_tab].panes {
            pane.view.family = self.fonts.family(&ctx, pane.view.font_name.as_deref());
        }
        let mut typed: Option<Vec<u8>> = None;
        let mut pasted: Option<(usize, String)> = None;
        let broadcast = self.broadcast;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let mark = broadcast_color(ui.visuals());
            let tab = &mut self.tabs[self.active_tab];
            let area = ui.available_rect_before_wrap();
            let n = tab.panes.len();
            let gap = 4.0;
            // A pane dragged by its caption: (from, where the pointer is).
            let mut dragging: Option<(usize, egui::Pos2)> = None;
            let mut dropped: Option<usize> = None;
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
                            mark,
                        );
                    }
                    let grip = ui
                        .interact(
                            caption,
                            ui.make_persistent_id(("pane-caption", i)),
                            egui::Sense::click_and_drag(),
                        )
                        .on_hover_cursor(egui::CursorIcon::Grab)
                        .on_hover_text("Drag onto another pane to swap them");
                    if grip.clicked() && !active {
                        actions.push(Action::FocusPane(i));
                    }
                    if grip.dragged()
                        && let Some(at) = grip.interact_pointer_pos()
                    {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        dragging = Some((i, at));
                    }
                    if grip.drag_stopped() {
                        dropped = Some(i);
                    }
                    grip.context_menu(|ui| {
                        let earlier = if tab.layout == Layout::Rows {
                            "Move Up"
                        } else {
                            "Move Left"
                        };
                        let later = if tab.layout == Layout::Rows {
                            "Move Down"
                        } else {
                            "Move Right"
                        };
                        if ui.add_enabled(i > 0, egui::Button::new(earlier)).clicked() {
                            actions.push(Action::SwapPanes(i, i - 1));
                            ui.close();
                        }
                        if ui
                            .add_enabled(i + 1 < n, egui::Button::new(later))
                            .clicked()
                        {
                            actions.push(Action::SwapPanes(i, i + 1));
                            ui.close();
                        }
                    });
                    rect.min.y += PANE_CAPTION;
                }
                let out = ui
                    .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        pane.view.show(ui, &mut pane.term, active && !modal)
                    })
                    .inner;
                // The pane with the keyboard is framed in blue; with "send to
                // all" every pane that gets the keys is.
                if n > 1 && (active || (broadcast && !pane.transport.is_closed())) {
                    ui.painter().rect_stroke(
                        rect,
                        egui::CornerRadius::ZERO,
                        egui::Stroke::new(1.0, mark),
                        egui::StrokeKind::Inside,
                    );
                }
                if out.clicked && !active {
                    actions.push(Action::FocusPane(i));
                }
                if let Some(text) = out.paste {
                    pasted = Some((i, text));
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
            // While a pane is dragged, the pane under the pointer lights up;
            // letting go there swaps the two.
            let target_at =
                |at: egui::Pos2| (0..n).find(|&j| tab.layout.rect(area, j, n, gap).contains(at));
            if let Some((from, at)) = dragging {
                if let Some(to) = target_at(at).filter(|&to| to != from) {
                    let target = tab.layout.rect(area, to, n, gap);
                    let blue = egui::Color32::from_rgb(110, 200, 255);
                    ui.painter()
                        .rect_filled(target, 0.0, blue.gamma_multiply(0.18));
                    ui.painter().rect_stroke(
                        target,
                        0.0,
                        egui::Stroke::new(2.0, blue),
                        egui::StrokeKind::Inside,
                    );
                }
                let name = pane_name(&tab.panes[from]);
                ui.painter().text(
                    at + egui::vec2(14.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    format!("⇄ {name}"),
                    egui::FontId::proportional(13.0),
                    ui.visuals().strong_text_color(),
                );
            }
            if let Some(from) = dropped
                && let Some(at) = ctx.pointer_latest_pos()
                && let Some(to) = target_at(at)
                && to != from
            {
                actions.push(Action::SwapPanes(from, to));
            }
        });

        if let Some((pane, text)) = pasted {
            self.paste_confirm = Some(PasteConfirm {
                tab: self.active_tab,
                pane,
                text,
            });
        }

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
        self.paste_dialog(ctx, center, escape);
        self.group_dialogs(ctx, center, enter, escape);
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
                            if theme::primary_button(ui, "OK").clicked() || enter {
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
        self.folder_dialog_ui(ctx, center, escape);
        self.about_ui(ctx);
        self.update_ui(ctx);
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
                    ui.label("Show next to this session:");
                    ui.add_space(4.0);
                    ui.checkbox(&mut dialog.shell, "A new PrzmTerm shell")
                        .on_hover_text("To connect to another host from there");
                    if !dialog.others.is_empty() {
                        ui.add_space(4.0);
                        ui.weak("Sessions open in other tabs:");
                    }
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
                    let any = dialog.shell || dialog.others.iter().any(|o| o.2);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(any, egui::Button::new("Split"))
                                .on_disabled_hover_text("Choose what goes next to it")
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
                            shell: dialog.shell,
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
                    let ctx2 = ctx.clone();
                    let waker: Waker = Arc::new(move || ctx2.request_repaint());
                    if let Some(tab) = self.tabs.get_mut(t) {
                        tab.pane_mut().begin(start, waker);
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
                            if theme::primary_button(ui, "OK").clicked() {
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
        if let Some(FontDialog { tab: t, before }) = self.font_dialog.clone() {
            // Esc or Cancel puts the panes back as they were.
            let mut close = escape;
            let mut revert = escape;
            let mut save = false;
            let mut reload = false;
            let families = self.fonts.families().cloned();
            if let Some(tab) = self.tabs.get_mut(t) {
                let view = &tab.pane().view;
                let (mut size, mut name, mut cursor, mut blink, mut color) = (
                    view.font_size,
                    view.font_name.clone(),
                    view.cursor,
                    view.cursor_blink,
                    view.cursor_color,
                );
                let saved_to = tab
                    .pane()
                    .origin
                    .as_ref()
                    .filter(|s| s.path.is_file())
                    .map(|s| s.name.clone());
                egui::Window::new("Font and cursor")
                    .pivot(egui::Align2::CENTER_CENTER)
                    .default_pos(center)
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label(egui::RichText::new("Font").strong());
                        egui::Grid::new("font-dialog-font")
                            .num_columns(2)
                            .min_col_width(60.0)
                            .spacing([12.0, 8.0])
                            .show(ui, |ui| {
                                ui.label("Family");
                                font_picker(ui, "font-dialog-family", &mut name, families.as_ref());
                                ui.end_row();
                                ui.label("Size");
                                let mut points = size as u16;
                                ui.add(
                                    egui::Slider::new(&mut points, przmterm_session::FONT_SIZES)
                                        .suffix(" pt"),
                                );
                                size = f32::from(points);
                                ui.end_row();
                            });
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Cursor").strong());
                        egui::Grid::new("font-dialog-cursor")
                            .num_columns(2)
                            .min_col_width(60.0)
                            .spacing([12.0, 8.0])
                            .show(ui, |ui| {
                                ui.label("Shape");
                                ui.horizontal(|ui| {
                                    for shape in przmterm_session::CursorShape::ALL {
                                        ui.selectable_value(
                                            &mut cursor,
                                            shape,
                                            cursor_label(shape),
                                        );
                                    }
                                });
                                ui.end_row();
                                ui.label("Color");
                                ui.horizontal(|ui| {
                                    let shown = tab.pane().view.cursor_color();
                                    let mut rgb = [shown.r(), shown.g(), shown.b()];
                                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                                        color = Some(rgb);
                                    }
                                    if color.is_some()
                                        && ui
                                            .button("Default")
                                            .on_hover_text("The usual light blue")
                                            .clicked()
                                    {
                                        color = None;
                                    }
                                });
                                ui.end_row();
                                ui.label("Blink");
                                ui.checkbox(&mut blink, "");
                                ui.end_row();
                            });
                        ui.add_space(4.0);
                        ui.weak(match &saved_to {
                            Some(name) => format!(
                                "Changes show at once. Apply keeps them in this tab; \
                                 Save keeps them for {name} too."
                            ),
                            None => "Changes show at once. Apply keeps them in this tab.".into(),
                        });
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if saved_to.is_some()
                                        && ui
                                            .button("Save")
                                            .on_hover_text("Keep them in the session's settings")
                                            .clicked()
                                    {
                                        save = true;
                                        close = true;
                                    }
                                    if ui
                                        .button("Apply")
                                        .on_hover_text("Keep them in this tab, until it closes")
                                        .clicked()
                                    {
                                        close = true;
                                    }
                                    if ui.button("Cancel").clicked() {
                                        close = true;
                                        revert = true;
                                    }
                                },
                            );
                        });
                    });
                if revert {
                    for (pane, look) in tab.panes.iter_mut().zip(&before) {
                        pane.view.set_look(look);
                    }
                } else {
                    for pane in &mut tab.panes {
                        pane.view.font_size = size;
                        pane.view.font_name = name.clone();
                        pane.view.cursor = cursor;
                        pane.view.cursor_blink = blink;
                        pane.view.cursor_color = color;
                    }
                }
                if save && let Some(session) = tab.pane_mut().origin.as_mut() {
                    let result = przmterm_session::update_session_file(&session.path, |s| {
                        s.terminal.font = Some(name.clone().unwrap_or_default());
                        s.terminal.font_size = Some(size as u16);
                        s.terminal.cursor = Some(cursor);
                        s.terminal.cursor_blink = Some(blink);
                        s.terminal.cursor_color =
                            Some(color.map(przmterm_session::color_text).unwrap_or_default());
                    });
                    match result {
                        Ok(()) => {
                            session.font = name.clone();
                            session.font_size = size as u16;
                            session.cursor = cursor;
                            session.cursor_blink = blink;
                            session.cursor_color = color;
                            self.notice =
                                Some(format!("Font and cursor saved for {}.", session.name));
                            reload = true;
                        }
                        Err(e) => self.notice = Some(format!("Could not save: {e}")),
                    }
                }
            } else {
                close = true;
            }
            if close {
                self.font_dialog = None;
            }
            if reload {
                self.reload_sessions();
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
            if editor.font_families.is_none() {
                editor.font_families = self.fonts.families().cloned();
            }
            // While the vault is locked its names are hidden: offer the ones
            // the sessions use (read once per editor).
            if editor.known_vault_names.is_none() {
                editor.known_vault_names = Some(
                    self.config_dir
                        .as_deref()
                        .map(przmterm_session::vault::names_in_use)
                        .unwrap_or_default(),
                );
            }
            if editor.take_unlock_request() {
                self.vault
                    .ask_unlock("Unlock the vault to list its entries.".into());
            }
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
                    close |= theme::primary_button(ui, "OK").clicked();
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
                            if theme::primary_button(ui, "OK").clicked() || enter {
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
/// A group's label before its tabs: click or right-click for what the
/// group can do.
fn group_label(ui: &mut egui::Ui, group: &Group, actions: &mut Vec<Action>) {
    let text = egui::RichText::new(&group.name)
        .color(egui::Color32::from_gray(20))
        .strong();
    let response = ui
        .add(
            egui::Button::new(text)
                .fill(group.color)
                .corner_radius(9.0)
                .min_size(egui::vec2(0.0, 20.0)),
        )
        .on_hover_text("Tab group: click for its commands");
    let menu = |ui: &mut egui::Ui| {
        let mut item = |ui: &mut egui::Ui, text: &str, action: Action| {
            if ui.button(text).clicked() {
                actions.push(action);
                ui.close();
            }
        };
        item(ui, "Send Command to Group…", Action::SendToGroup(group.id));
        item(ui, "Rename Group…", Action::RenameGroup(group.id));
        ui.separator();
        item(ui, "Ungroup", Action::Ungroup(group.id));
        item(ui, "Close Group", Action::CloseGroup(group.id));
    };
    egui::Popup::menu(&response).show(menu);
}

fn tab_menu(
    ui: &mut egui::Ui,
    tabs: &[Tab],
    groups: &[Group],
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
    // The other tabs are those of the same window.
    let window = tab.window;
    let same: Vec<usize> = (0..tabs.len())
        .filter(|&n| tabs[n].window == window)
        .collect();
    let disconnected: Vec<usize> = same
        .iter()
        .copied()
        .filter(|&n| {
            let t = &tabs[n];
            t.panes.iter().any(|p| p.origin.is_some()) && t.connected() == 0
        })
        .collect();
    item(
        ui,
        !disconnected.is_empty(),
        "Close Disconnected Tabs",
        Action::RequestClose(disconnected),
    );
    let others: Vec<usize> = same.iter().copied().filter(|&n| n != i).collect();
    item(
        ui,
        !others.is_empty(),
        "Close Other Tabs",
        Action::RequestClose(others),
    );
    let right: Vec<usize> = same.iter().copied().filter(|&n| n > i).collect();
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
        Action::RequestClose(same.clone()),
    );
    ui.separator();
    // Groups: those of this window.
    ui.menu_button("Group", |ui| {
        item(ui, true, "New Group", Action::NewGroup(vec![i]));
        for g in groups {
            let here = same.iter().any(|&n| tabs[n].group == Some(g.id));
            if here && tab.group != Some(g.id) {
                item(
                    ui,
                    true,
                    &format!("Add to {}", g.name),
                    Action::SetGroup(i, Some(g.id)),
                );
            }
        }
        item(
            ui,
            tab.group.is_some(),
            "Remove from Group",
            Action::SetGroup(i, None),
        );
    });
    ui.menu_button("Window", |ui| {
        item(
            ui,
            same.len() > 1 || window != 0,
            "Move to New Window",
            Action::NewWindow(i, false),
        );
        item(ui, true, "Clone to New Window", Action::NewWindow(i, true));
        item(
            ui,
            window != 0,
            "Move to Main Window",
            Action::ToMainWindow(i),
        );
    });
    item(ui, tab.lock.is_none(), "Lock…", Action::LockTab(i));
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
    item(ui, true, "Font and Cursor…", Action::FontTab(i));
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
    let shell = tab
        .pane()
        .origin
        .as_ref()
        .and_then(|s| s.scp_shell.as_deref());
    match browser_menu(ui, ssh_live, shell) {
        Some(true) => actions.push(Action::OpenScp(i)),
        Some(false) => actions.push(Action::OpenSftp(i)),
        None => {}
    }

    ui.separator();
    // Typing into every pane of this tab: only for split tabs.
    let split = tab.panes.len() > 1;
    let on = broadcast && i == active;
    if ui
        .add_enabled(
            split,
            egui::Button::selectable(on, "Send Commands to This Tab"),
        )
        .on_disabled_hover_text("Split the tab first (the split icons at the top right)")
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

/// The colour that frames the pane with the keyboard; with "send to all"
/// every pane that gets the keys is framed in it, and the status bar
/// button wears it too.
fn broadcast_color(visuals: &egui::Visuals) -> egui::Color32 {
    if visuals.dark_mode {
        egui::Color32::from_rgb(110, 200, 255)
    } else {
        egui::Color32::from_rgb(0, 105, 175)
    }
}

/// The "File Browser" submenu of a tab or a session: `Some(true)` when SCP
/// was picked, `Some(false)` for SFTP.
fn browser_menu(ui: &mut egui::Ui, enabled: bool, shell: Option<&str>) -> Option<bool> {
    let mut picked = None;
    ui.add_enabled_ui(enabled, |ui| {
        ui.menu_button("File Browser", |ui| {
            if ui
                .button("SFTP")
                .on_hover_text("Browse and transfer files over SFTP")
                .clicked()
            {
                picked = Some(false);
                ui.close();
            }
            let scp = match shell {
                Some(shell) => format!("SCP  ({shell})"),
                None => "SCP".to_string(),
            };
            if ui
                .button(scp)
                .on_hover_text(
                    "Browse and transfer files over SCP, in the session's SCP shell \
                     (sudo su - to work as root)",
                )
                .clicked()
            {
                picked = Some(true);
                ui.close();
            }
        })
        .response
        .on_disabled_hover_text("Needs a connected SSH session");
    });
    picked
}

/// True if the session's name, host or address contains `filter`
/// (already lower-case).
fn session_matches(s: &Session, filter: &str) -> bool {
    filter.is_empty()
        || s.name.to_lowercase().contains(filter)
        || s.display_target().to_lowercase().contains(filter)
}

/// One row of the session list: a grip while dragging is on, a folder icon
/// for folders, then the text. It reaches the list's right edge, so it is
/// easy to hit and to drop onto.
fn tree_row(ui: &mut egui::Ui, drag: bool, folder: bool, text: egui::WidgetText) -> egui::Response {
    let galley = text.into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    const ICON: f32 = 16.0;
    let lead = if drag { ICON } else { 0.0 } + if folder { ICON + 2.0 } else { 0.0 };
    let pad = ui.spacing().button_padding.x;
    let height = (galley.size().y + 4.0).max(ui.spacing().interact_size.y);
    let width =
        (pad * 2.0 + lead + galley.size().x).max(ui.clip_rect().right() - ui.cursor().left());
    let sense = if drag {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::click()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), sense);
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, false);
        let painter = ui.painter();
        if response.hovered() || response.dragged() {
            painter.rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        }
        let color = visuals.text_color();
        let mut x = rect.left() + pad;
        let icon_rect =
            |x: f32| egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(ICON, height));
        if drag {
            icons::draw(
                painter,
                icons::Icon::Grip,
                icon_rect(x),
                ui.visuals().weak_text_color(),
            );
            x += ICON;
        }
        if folder {
            icons::draw(painter, icons::Icon::Folder, icon_rect(x), color);
            x += ICON + 2.0;
        }
        let y = rect.center().y - galley.size().y / 2.0;
        painter.galley(egui::pos2(x, y), galley, color);
    }
    response
}

/// Starts dragging `path` from `response`, with its name following the
/// pointer.
fn drag_item(ui: &egui::Ui, response: &egui::Response, path: &std::path::Path, name: &str) {
    if response.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), DragItem(path.to_path_buf()));
    }
    if response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if let Some(pos) = ui.ctx().pointer_interact_pos() {
            egui::Area::new(egui::Id::unique("session-list-drag"))
                .order(egui::Order::Tooltip)
                .fixed_pos(pos + egui::vec2(14.0, 6.0))
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(name));
                });
        }
    }
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
        let id = ui.make_persistent_id(("folder", &f.dir));
        let mut state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
        if !filter.is_empty() {
            state.set_open(true); // show every match while searching
        }
        let (_, header, _) = state
            .show_header(ui, |ui| tree_row(ui, drag, true, f.name.clone().into()))
            .body(|ui| folder_ui(ui, f, picked, drag, filter, open));
        let r = header.inner;
        // A click on the name opens or closes it, like the arrow.
        if r.clicked()
            && let Some(mut state) = egui::collapsing_header::CollapsingState::load(ui.ctx(), id)
        {
            state.toggle(ui);
            state.store(ui.ctx());
        }
        if drag {
            drag_item(ui, &r, &f.dir, &f.name);
            // Not onto itself (nor, as move_folder checks, inside itself).
            let dropped = |p: &DragItem| p.0 != f.dir;
            if r.dnd_hover_payload::<DragItem>()
                .is_some_and(|p| dropped(&p))
            {
                ui.painter().rect_stroke(
                    r.rect.expand(1.0),
                    egui::CornerRadius::same(3),
                    egui::Stroke::new(1.5, ui.visuals().selection.stroke.color),
                    egui::StrokeKind::Outside,
                );
            }
            if let Some(payload) = r.dnd_release_payload::<DragItem>()
                && dropped(&payload)
            {
                picked.push(TreeAction::Move(payload.0.clone(), f.dir.clone()));
            }
        }
        r.on_hover_text(if drag {
            "Drag onto another folder to move it, or drop sessions and folders here"
        } else {
            "Right-click to add sessions, rename or set folder defaults"
        })
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
        let response = tree_row(ui, drag, false, text);
        if drag {
            drag_item(ui, &response, &s.path, &s.name);
        }
        let response = response.on_hover_text(if drag {
            "Drag onto a folder to move it".to_string()
        } else {
            format!(
                "[{}] {}{}\nDouble-click to connect, right-click for more",
                s.protocol,
                s.display_target(),
                if is_open { "\nOpen in a tab" } else { "" }
            )
        });
        if response.double_clicked() {
            picked.push(TreeAction::Connect(s.clone()));
        }
        response.context_menu(|ui| {
            if ui.button("Connect").clicked() {
                picked.push(TreeAction::Connect(s.clone()));
                ui.close();
            }
            if s.protocol == przmterm_core::Protocol::Ssh
                && let Some(scp) = browser_menu(ui, true, s.scp_shell.as_deref())
            {
                picked.push(TreeAction::Browse(s.clone(), scp));
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
    /// What sessions do on their own: read, log, answer triggers, run
    /// scripts and transfers, send anti-idle, reconnect. eframe calls this
    /// before each frame, and also while the window is minimized or hidden,
    /// when it draws nothing and calls no `ui`.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
        self.run_retries(ctx);
        self.run_queues(ctx);
        self.run_scripts();
        self.run_sftp_pending(ctx);
        self.run_transfers(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // A glyph atlas that filled up last frame starts over.
        grid::atlas().begin_frame();
        let ctx = ui.ctx().clone();
        for command in appmenu::take() {
            self.app_command(&ctx, command);
        }
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

        // The sessions' own work runs in `logic`, before this.
        actions.extend(self.launcher_requests());
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
                    match Session::quick_connect_with(
                        &self.quick_connect,
                        &self.sessions.root.inherited,
                    ) {
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
                    // Split icons as editors have them; the rest in a menu.
                    let more = icons::button(ui, icons::Icon::More, false)
                        .on_hover_text("More ways to split and arrange");
                    for (icon, layout, id, what) in [
                        (
                            icons::Icon::SplitRows,
                            Layout::Rows,
                            "split_rows",
                            "Split Down",
                        ),
                        (
                            icons::Icon::SplitColumns,
                            Layout::Columns,
                            "split_columns",
                            "Split Right",
                        ),
                    ] {
                        if icons::button(ui, icon, false)
                            .on_hover_text(format!("{what}…{}", self.keys_hint(id)))
                            .clicked()
                        {
                            actions.push(Action::Split(layout));
                        }
                    }
                    egui::Popup::menu(&more).show(|ui| {
                        ui.weak("A PrzmTerm shell or sessions of other tabs");
                        for (layout, id) in [
                            (Layout::Grid, "split_grid"),
                            (Layout::Columns, "split_columns"),
                            (Layout::Rows, "split_rows"),
                        ] {
                            if ui
                                .add(
                                    egui::Button::new(format!("{}…", layout.label()))
                                        .shortcut_text(keys(id)),
                                )
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

        // A newer release found at startup: a link in the status bar.
        let newer = match self.update.as_ref().and_then(update::Check::outcome) {
            Some(update::Outcome::Newer(found)) => Some(found.version),
            _ => None,
        };
        let mut open_update = false;
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let pane = self.tab().pane();
                let s = pane.term.screen();
                ui.label(format!("PrzmTerm {}", przmterm_core::VERSION));
                if let Some(version) = &newer
                    && ui
                        .link(format!("{version} available"))
                        .on_hover_text("A newer PrzmTerm: see what is new and download it")
                        .clicked()
                {
                    open_update = true;
                }
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
                            let mark = broadcast_color(ui.visuals());
                            egui::Button::new(
                                egui::RichText::new("⇶ Typing to all").color(mark).strong(),
                            )
                            .fill(mark.gamma_multiply(0.12))
                            .stroke(egui::Stroke::new(1.0, mark))
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
                            .button("Cancel")
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
                            egui::RichText::new(format!("{arrow} {} {} {amount}", t.label, p.file))
                                .color(egui::Color32::from_rgb(110, 200, 255)),
                        );
                    }
                    if let Some(name) = pane.script_name() {
                        if ui
                            .button("■ Stop")
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
        if open_update {
            self.update_window = true;
        }

        if self.sessions_hidden {
            egui::Panel::left("sessions-folded")
                .resizable(false)
                .exact_size(30.0)
                .show(ui, |ui| {
                    if icons::button(ui, icons::Icon::ChevronRight, false)
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
                let mut menu_command = None;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    if icons::button(ui, icons::Icon::ChevronLeft, false)
                        .on_hover_text(format!(
                            "Hide the sessions{}",
                            self.keys_hint("toggle_sessions")
                        ))
                        .clicked()
                    {
                        actions.push(Action::ToggleSessions);
                    }
                    ui.add_space(2.0);
                    ui.strong("Sessions");
                    ui.add_space(2.0);
                    if icons::button(ui, icons::Icon::Reload, false)
                        .on_hover_text("Reload")
                        .clicked()
                    {
                        self.reload_sessions();
                    }
                    let (lock, hint) = if self.drag_unlocked {
                        (
                            icons::Icon::Unlock,
                            "Dragging is on: drag sessions and folders by their dots onto a folder. Click to lock.",
                        )
                    } else {
                        (icons::Icon::Lock, "Unlock to drag sessions and folders between folders")
                    };
                    if icons::button(ui, lock, self.drag_unlocked)
                        .on_hover_text(hint)
                        .clicked()
                    {
                        self.drag_unlocked = !self.drag_unlocked;
                    }
                    if let Some(dir) = &self.config_dir
                        && icons::button(ui, icons::Icon::Gear, false)
                            .on_hover_text("Global settings for every session")
                            .clicked()
                    {
                        self.editor = Some(Editor::global(dir));
                    }
                    if self.config_dir.is_some() {
                        let plus = icons::button(ui, icons::Icon::Plus, false)
                            .on_hover_text("New session or folder, backup, vault, shortcuts…");
                        egui::Popup::menu(&plus).show(|ui| {
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
                            if ui
                                .button("Settings folder…")
                                .on_hover_text(
                                    "Where PrzmTerm keeps its settings: a synced folder \
                                     shares them between computers",
                                )
                                .clicked()
                            {
                                ui.close();
                                menu_command = Some(appmenu::AppCommand::SettingsFolder);
                            }
                            ui.separator();
                            if ui.button("Keyword sets…").clicked() {
                                menu_command = Some(appmenu::AppCommand::Keywords);
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
                                menu_command = Some(appmenu::AppCommand::Vault);
                                ui.close();
                            }
                            if ui
                                .button("Keyboard shortcuts…")
                                .on_hover_text(
                                    "Choose the keys for commands, text, scripts and buttons",
                                )
                                .clicked()
                            {
                                menu_command = Some(appmenu::AppCommand::Shortcuts);
                                ui.close();
                            }
                            if ui
                                .button("Script guide…")
                                .on_hover_text("How to write scripts, with examples")
                                .clicked()
                            {
                                menu_command = Some(appmenu::AppCommand::ScriptGuide);
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
                            ui.separator();
                            if ui.button("Check for Updates…").clicked() {
                                menu_command = Some(appmenu::AppCommand::CheckUpdates);
                                ui.close();
                            }
                            if ui.button("About PrzmTerm…").clicked() {
                                menu_command = Some(appmenu::AppCommand::About);
                                ui.close();
                            }
                        });
                    }
                });
                if let Some(command) = menu_command {
                    self.app_command(ui.ctx(), command);
                }
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
                // Solid scroll bars take their own room, so they never cover
                // the last session (or the end of a long name).
                ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        // Rows as close as a file tree's.
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.spacing_mut().interact_size.y = 22.0;
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
                                let (_, dropped) = ui.dnd_drop_zone::<DragItem, _>(
                                    egui::Frame::default().inner_margin(4.0),
                                    |ui| {
                                        ui.weak("Drop here for the top level");
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
                                    TreeAction::Connect(s) => {
                                        actions.push(Action::Open(Box::new(s)))
                                    }
                                    TreeAction::Browse(s, scp) => {
                                        actions.push(Action::BrowseFor(Box::new(s), scp));
                                    }
                                    TreeAction::Edit(s) => self.editor = Some(Editor::edit(&s)),
                                    TreeAction::Duplicate(s) => {
                                        self.editor = Some(Editor::duplicate(&s))
                                    }
                                    TreeAction::Delete(s) => {
                                        self.confirm_delete = Some(Deletion::Session(Box::new(s)))
                                    }
                                    TreeAction::Move(path, dir) => {
                                        let moved = if path.is_dir() {
                                            przmterm_session::move_folder(&path, &dir)
                                        } else {
                                            przmterm_session::move_session(&path, &dir)
                                        };
                                        self.notice = Some(match moved {
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
        egui::Panel::top("tabs")
            .frame(theme::tab_strip())
            .show_separator_line(false)
            .show(ui, |ui| self.tab_bar(ui, &mut actions));

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
        self.terminal_area(ui, &mut actions);

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
        self.show_windows(&ctx);
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
            let mut p = Pane::launcher();
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
