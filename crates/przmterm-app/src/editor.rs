//! Editor window for sessions, folders and the global defaults.
//!
//! Only the values set in the file itself are written. Every empty field
//! shows the value it inherits. Changing the global defaults asks first
//! whether existing sessions that use them should follow the change.

use std::path::{Path, PathBuf};

use eframe::egui;
use przmterm_core::Protocol;
use przmterm_session::{
    COMMON_BAUD_RATES, DEFAULT_DATA_BITS, DEFAULT_STOP_BITS, FlowControl, Folder, Forward,
    ForwardKind, LogSettings, McpAccess, McpSettings, Parity, Session, SessionSettings,
    TerminalSettings, Trigger,
};
use przmterm_transport::serial::{PortInfo, available_ports};

/// Terminal types offered in the list. Any other value can still be typed in
/// the session file.
const TERM_TYPES: [&str; 9] = [
    "xterm-256color",
    "xterm",
    "vt100",
    "vt102",
    "vt220",
    "linux",
    "ansi",
    "screen-256color",
    "tmux-256color",
];

const LOG_HELP: &str = "\
Variables
  %S  session name      %H  host or serial port
  %Y  year (2026)       %M  month (01-12)
  %D  day (01-31)       %h  hour (00-23)
  %m  minute (00-59)    %s  second (00-59)
  %%  a literal %

Examples
  %S-%Y%M%D-%h%m%s.log  core-sw-01-20261101-221530.log
  %S-%Y%M%D.log         one file per session per day
  %H/%Y-%M-%D.log       a subfolder per host
  ~/logs/%S.log         absolute: ignores the log folder";

#[derive(Clone, PartialEq, Eq)]
enum Target {
    /// A session file; `original` is `None` for a new one.
    Session {
        original: Option<PathBuf>,
        trail: Vec<String>,
    },
    /// A folder's own settings (`folder.toml`), with rename.
    Folder { dir: PathBuf, trail: Vec<String> },
    /// `defaults.toml`.
    Global,
}

/// The confirmation shown before changing the global defaults.
struct Confirm {
    keys: Vec<&'static str>,
    /// Which existing sessions take the new values.
    scope: Scope,
}

/// Which existing sessions a change to the global settings reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Only new sessions; existing ones get the old value written in.
    NewOnly,
    /// Sessions that use the default; their own values stay.
    Inheriting,
    /// Every session and folder: their own values for these fields go.
    All,
}

/// The pages of the editor's side menu.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Page {
    General,
    Login,
    Ssh,
    Serial,
    Terminal,
    Automation,
    Logging,
    Mcp,
}

impl Page {
    fn label(self) -> &'static str {
        match self {
            Page::General => "Connection",
            Page::Login => "Login",
            Page::Ssh => "SSH",
            Page::Serial => "Serial port",
            Page::Terminal => "Terminal",
            Page::Automation => "Automation",
            Page::Logging => "Logging",
            Page::Mcp => "AI access",
        }
    }
}

pub struct Editor {
    target: Target,
    title: String,
    /// The settings as they were in the file, to see what changed.
    original: SessionSettings,
    name: String,
    protocol: Option<Protocol>,
    host: String,
    port: String,
    username: String,
    /// Vault entry name; `Some("")` turns an inherited one off.
    credential: Option<String>,
    /// Names in the vault, while it is unlocked (set by the app each frame).
    pub vault_names: Option<Vec<String>>,
    /// The entries the session files name, for when the vault is locked.
    pub known_vault_names: Option<Vec<String>>,
    /// The vault window was asked for.
    vault_request: bool,
    /// Unlocking (only) was asked for, to list the entries.
    unlock_request: bool,
    identity_file: String,
    serial_port: String,
    baud_rate: String,
    data_bits: Option<u8>,
    parity: Option<Parity>,
    stop_bits: Option<u8>,
    flow_control: Option<FlowControl>,
    /// Serial ports found, listed when the port menu opens.
    ports: Option<Vec<PortInfo>>,
    term_type: Option<String>,
    scrollback: String,
    /// `Some("")` is the built-in font (over an inherited one).
    font: Option<String>,
    font_size: String,
    cursor: Option<przmterm_session::CursorShape>,
    cursor_blink: Option<bool>,
    /// `#rrggbb`; `Some("")` is the default colour (over an inherited one).
    cursor_color: Option<String>,
    background: Option<String>,
    foreground: Option<String>,
    /// The installed monospace fonts, given by the app once read.
    pub font_families: Option<crate::fonts::FontList>,
    log: Option<bool>,
    log_path: String,
    log_dir: String,
    save_password: Option<bool>,
    debug: Option<bool>,
    keepalive: String,
    auto_reconnect: Option<bool>,
    jump_host: String,
    legacy_algorithms: Option<bool>,
    x11_forwarding: Option<bool>,
    keywords: Option<String>,
    logon_script: String,
    /// Kept as typed: a single space is a usual anti-idle text.
    anti_idle: String,
    anti_idle_interval: String,
    scp_shell: String,
    /// The keyword editor was asked for, for this set (if any).
    keyword_request: Option<Option<String>>,
    /// Tunnels set here; `None` inherits the list from above.
    forwards: Option<Vec<Forward>>,
    /// Triggers set here; `None` inherits the list from above.
    triggers: Option<Vec<Trigger>>,
    show_log_help: bool,
    mcp_access: Option<McpAccess>,
    /// One regular expression per line; empty inherits.
    mcp_block: String,
    /// The section whose explanation is open (its title).
    help: Option<&'static str>,
    /// Scroll to the log settings when the window first shows.
    scroll_to_log: bool,
    /// The page shown (side menu).
    page: Page,
    confirm: Option<Confirm>,
    error: Option<String>,
}

pub enum EditorResult {
    Open,
    Cancel,
    Saved(String),
}

fn text(v: &Option<impl ToString>) -> String {
    v.as_ref().map(|x| x.to_string()).unwrap_or_default()
}

fn opt(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// `/home/me/.ssh/id` → `~/.ssh/id`.
fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

/// Human name of a settings key, for the confirmation.
/// `reason` with its `key`s named as the form names them, capitalised:
/// "`serial_port` is required" becomes "Serial port is required".
fn readable(reason: &str) -> String {
    let mut out = String::new();
    let mut parts = reason.split('`');
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for (i, part) in parts.enumerate() {
        out.push_str(if i % 2 == 0 { field_label(part) } else { part });
    }
    let mut chars = out.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn field_label(key: &str) -> &'static str {
    match key {
        "protocol" => "protocol",
        "port" => "port",
        "username" => "user name",
        "credential" => "vault entry",
        "identity_file" => "key file",
        "save_password" => "save password",
        "debug" => "debug login",
        "keepalive" => "keepalive",
        "auto_reconnect" => "auto reconnect",
        "jump_host" => "jump host",
        "legacy_algorithms" => "legacy algorithms",
        "x11_forwarding" => "X11 forwarding",
        "keywords" => "keyword highlighting",
        "logon_script" => "logon script",
        "anti_idle" => "anti-idle text",
        "anti_idle_interval" => "anti-idle interval",
        "scp_shell" => "SCP shell",
        "forward" => "tunnels",
        "trigger" => "triggers",
        "baud_rate" => "baud rate",
        "data_bits" => "data bits",
        "parity" => "parity",
        "stop_bits" => "stop bits",
        "flow_control" => "flow control",
        "terminal.term_type" => "terminal type",
        "terminal.scrollback" => "scrollback",
        "terminal.font" => "font",
        "terminal.font_size" => "font size",
        "terminal.cursor" => "cursor",
        "terminal.cursor_blink" => "cursor blink",
        "terminal.cursor_color" => "cursor color",
        "terminal.background" => "background color",
        "terminal.foreground" => "text color",
        "log.enabled" => "log to file",
        "log.path" => "log file",
        "log.dir" => "log folder",
        "mcp.access" => "AI access",
        "mcp.block" => "AI blocked commands",
        "host" => "host",
        "serial_port" => "serial port",
        _ => "setting",
    }
}

/// Everything a row needs to know about inheritance.
struct Context<'a> {
    /// Effective values coming from above.
    inherited: &'a SessionSettings,
    /// Names of the saved SSH sessions, for the jump host list.
    ssh_sessions: Vec<String>,
    /// Keyword sets in `<config>/keywords`.
    keyword_sets: Vec<String>,
    protocol: Protocol,
    /// Default log folder, `<config>/logs`.
    logs_dir: String,
    config_dir: &'a Path,
}

impl Context<'_> {
    fn builtin(&self, key: &str) -> String {
        match key {
            "protocol" => "ssh".into(),
            "port" => self.protocol.default_port().unwrap_or(22).to_string(),
            "username" => "asked at login".into(),
            "credential" => "none".into(),
            "identity_file" => "agent and ~/.ssh keys".into(),
            "save_password" => "ask".into(),
            "debug" => "no".into(),
            "keepalive" => format!("{} (0 = off)", przmterm_session::DEFAULT_KEEPALIVE),
            "auto_reconnect" => "no".into(),
            "jump_host" => "none (direct), or a session name or user@host".into(),
            "legacy_algorithms" => "no".into(),
            "x11_forwarding" => "no".into(),
            "keywords" => "none".into(),
            "logon_script" => "none".into(),
            "anti_idle" => "off".into(),
            "anti_idle_interval" => przmterm_session::DEFAULT_ANTI_IDLE_INTERVAL.to_string(),
            "scp_shell" => "Default".into(),
            "forward" => "none".into(),
            "trigger" => "none".into(),
            "serial_port" => "/dev/ttyUSB0 or COM3".into(),
            "baud_rate" => przmterm_session::DEFAULT_BAUD_RATE.to_string(),
            "data_bits" => DEFAULT_DATA_BITS.to_string(),
            "parity" => "none".into(),
            "stop_bits" => DEFAULT_STOP_BITS.to_string(),
            "flow_control" => "none".into(),
            "terminal.term_type" => "xterm-256color".into(),
            "terminal.scrollback" => "10000".into(),
            "terminal.font" => crate::fonts::BUILT_IN.into(),
            "terminal.font_size" => przmterm_session::DEFAULT_FONT_SIZE.to_string(),
            "terminal.cursor" => "block".into(),
            "terminal.cursor_blink" => "no".into(),
            "terminal.cursor_color" => "default".into(),
            "terminal.background" => "default".into(),
            "terminal.foreground" => "default".into(),
            "log.enabled" => "no".into(),
            "log.path" => przmterm_session::DEFAULT_LOG_TEMPLATE.into(),
            "log.dir" => self.logs_dir.clone(),
            "mcp.access" => "none".into(),
            "mcp.block" => przmterm_session::DEFAULT_MCP_BLOCK.join("\n"),
            _ => String::new(),
        }
    }

    /// The inherited value, shown in grey in an empty field.
    fn hint(&self, key: &str) -> String {
        self.inherited
            .field_text(key)
            .unwrap_or_else(|| self.builtin(key))
    }
}

impl Editor {
    pub fn new_session(trail: &[String]) -> Self {
        Self::from_settings(
            Target::Session {
                original: None,
                trail: trail.to_vec(),
            },
            "New session",
            "",
            &SessionSettings::default(),
        )
    }

    /// A new saved session from one opened with Quick connect.
    pub fn save_quick(session: &Session) -> Self {
        let mut s = SessionSettings {
            protocol: (session.protocol != Protocol::Ssh).then_some(session.protocol),
            debug: session.debug.then_some(true),
            ..SessionSettings::default()
        };
        let name = match &session.target {
            przmterm_session::Target::Network {
                host,
                port,
                username,
            } => {
                s.host = Some(host.clone());
                s.port = (Some(*port) != session.protocol.default_port()).then_some(*port);
                s.username = username.clone();
                host.clone()
            }
            przmterm_session::Target::Serial(line) => {
                s.serial_port = Some(line.port.clone());
                s.baud_rate = Some(line.baud_rate);
                line.port
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("serial")
                    .to_string()
            }
            przmterm_session::Target::Local => "local".into(),
        };
        let mut name = name;
        przmterm_session::strip_forbidden(&mut name);
        Self::from_settings(
            Target::Session {
                original: None,
                trail: Vec::new(),
            },
            "Save session",
            &name,
            &s,
        )
    }

    pub fn edit(session: &Session) -> Self {
        let settings = przmterm_session::read_session_settings(&session.path).unwrap_or_default();
        Self::from_settings(
            Target::Session {
                original: Some(session.path.clone()),
                trail: session.folder.clone(),
            },
            &format!("Edit {}", session.name),
            &session.name,
            &settings,
        )
    }

    /// Like [`Editor::edit`], opened at the log settings.
    pub fn edit_log(session: &Session) -> Self {
        let mut editor = Self::edit(session);
        editor.scroll_to_log = true;
        editor.page = Page::Logging;
        editor
    }

    pub fn duplicate(session: &Session) -> Self {
        let settings = przmterm_session::read_session_settings(&session.path).unwrap_or_default();
        Self::from_settings(
            Target::Session {
                original: None,
                trail: session.folder.clone(),
            },
            &format!("Duplicate {}", session.name),
            &format!("{}-copy", session.name),
            &settings,
        )
    }

    pub fn folder(folder: &Folder) -> Self {
        Self::from_settings(
            Target::Folder {
                dir: folder.dir.clone(),
                trail: folder.trail.clone(),
            },
            &format!("Folder {}", folder.name),
            &folder.name,
            &folder.own,
        )
    }

    pub fn global(config_dir: &Path) -> Self {
        let settings = przmterm_session::read_session_settings(
            &config_dir.join(przmterm_session::DEFAULTS_FILE),
        )
        .unwrap_or_default();
        Self::from_settings(Target::Global, "Global settings", "", &settings)
    }

    fn from_settings(target: Target, title: &str, name: &str, s: &SessionSettings) -> Self {
        Self {
            target,
            title: title.to_string(),
            original: s.clone(),
            name: name.to_string(),
            protocol: s.protocol,
            host: text(&s.host),
            port: text(&s.port),
            username: text(&s.username),
            credential: s.credential.clone(),
            vault_names: None,
            known_vault_names: None,
            vault_request: false,
            unlock_request: false,
            identity_file: text(&s.identity_file),
            serial_port: text(&s.serial_port),
            baud_rate: text(&s.baud_rate),
            data_bits: s.data_bits,
            parity: s.parity,
            stop_bits: s.stop_bits,
            flow_control: s.flow_control,
            ports: None,
            term_type: s.terminal.term_type.clone(),
            scrollback: text(&s.terminal.scrollback),
            font: s.terminal.font.clone(),
            font_size: text(&s.terminal.font_size),
            cursor: s.terminal.cursor,
            cursor_blink: s.terminal.cursor_blink,
            cursor_color: s.terminal.cursor_color.clone(),
            background: s.terminal.background.clone(),
            foreground: s.terminal.foreground.clone(),
            font_families: None,
            log: s.log.enabled,
            log_path: text(&s.log.path),
            log_dir: text(&s.log.dir),
            save_password: s.save_password,
            debug: s.debug,
            keepalive: text(&s.keepalive),
            auto_reconnect: s.auto_reconnect,
            jump_host: text(&s.jump_host),
            legacy_algorithms: s.legacy_algorithms,
            x11_forwarding: s.x11_forwarding,
            keywords: s.keywords.clone(),
            logon_script: text(&s.logon_script),
            anti_idle: s.anti_idle.clone().unwrap_or_default(),
            anti_idle_interval: text(&s.anti_idle_interval),
            scp_shell: text(&s.scp_shell),
            keyword_request: None,
            forwards: s.forwards.clone(),
            triggers: s.triggers.clone(),
            show_log_help: false,
            mcp_access: s.mcp.access,
            mcp_block: s
                .mcp
                .block
                .as_ref()
                .map(|b| b.join("\n"))
                .unwrap_or_default(),
            help: None,
            scroll_to_log: false,
            page: Page::General,
            confirm: None,
            error: None,
        }
    }

    /// The vault window was asked for (once).
    pub fn take_vault_request(&mut self) -> bool {
        std::mem::take(&mut self.vault_request)
    }

    pub fn take_unlock_request(&mut self) -> bool {
        std::mem::take(&mut self.unlock_request)
    }

    /// The set to open the keyword editor on, once, when asked for.
    pub fn take_keyword_request(&mut self) -> Option<Option<String>> {
        self.keyword_request.take()
    }

    fn is_session(&self) -> bool {
        matches!(self.target, Target::Session { .. })
    }

    fn settings(&self) -> Result<SessionSettings, String> {
        fn number<T: std::str::FromStr>(field: &str, s: &str) -> Result<Option<T>, String> {
            match opt(s) {
                None => Ok(None),
                Some(v) => v
                    .parse()
                    .map(Some)
                    .map_err(|_| format!("{field}: `{v}` is not a valid number")),
            }
        }
        let session = self.is_session();
        Ok(SessionSettings {
            protocol: self.protocol,
            host: if session { opt(&self.host) } else { None },
            port: number("Port", &self.port)?,
            username: opt(&self.username),
            credential: self.credential.clone(),
            identity_file: opt(&self.identity_file),
            save_password: self.save_password,
            debug: self.debug,
            keepalive: number("Keepalive", &self.keepalive)?,
            auto_reconnect: self.auto_reconnect,
            jump_host: opt(&self.jump_host),
            legacy_algorithms: self.legacy_algorithms,
            x11_forwarding: self.x11_forwarding,
            keywords: self.keywords.clone(),
            logon_script: opt(&self.logon_script),
            anti_idle: (!self.anti_idle.is_empty()).then(|| self.anti_idle.clone()),
            anti_idle_interval: number("Anti-idle interval", &self.anti_idle_interval)?,
            scp_shell: opt(&self.scp_shell),
            triggers: match &self.triggers {
                None => None,
                Some(list) => {
                    let list: Vec<Trigger> = list
                        .iter()
                        .filter(|t| !t.pattern.is_empty())
                        .cloned()
                        .collect();
                    let (_, problems) = przmterm_term::triggers::TriggerEngine::new(&list);
                    if let Some(p) = problems.first() {
                        return Err(format!("Triggers: {p}"));
                    }
                    Some(list)
                }
            },
            forwards: match &self.forwards {
                None => None,
                Some(list) => {
                    let list: Vec<Forward> = list
                        .iter()
                        .filter(|f| !f.listen.trim().is_empty() || !f.target.trim().is_empty())
                        .cloned()
                        .collect();
                    for (i, f) in list.iter().enumerate() {
                        f.tunnel().map_err(|e| format!("Tunnel {}: {e}", i + 1))?;
                    }
                    Some(list)
                }
            },
            serial_port: if session {
                opt(&self.serial_port)
            } else {
                None
            },
            baud_rate: number("Baud rate", &self.baud_rate)?,
            data_bits: self.data_bits,
            parity: self.parity,
            stop_bits: self.stop_bits,
            flow_control: self.flow_control,
            terminal: TerminalSettings {
                term_type: self.term_type.clone(),
                scrollback: number("Scrollback", &self.scrollback)?,
                font: self.font.clone(),
                font_size: match number::<u16>("Font size", &self.font_size)? {
                    Some(n) if !przmterm_session::FONT_SIZES.contains(&n) => {
                        return Err(format!(
                            "Font size: from {} to {} points",
                            przmterm_session::FONT_SIZES.start(),
                            przmterm_session::FONT_SIZES.end()
                        ));
                    }
                    n => n,
                },
                cursor: self.cursor,
                cursor_blink: self.cursor_blink,
                cursor_color: self.cursor_color.clone(),
                background: self.background.clone(),
                foreground: self.foreground.clone(),
            },
            log: LogSettings {
                enabled: self.log,
                path: opt(&self.log_path),
                dir: opt(&self.log_dir),
            },
            mcp: McpSettings {
                access: self.mcp_access,
                block: match self.mcp_block.trim() {
                    "" => None,
                    text => {
                        let list: Vec<String> = text
                            .lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty())
                            .map(String::from)
                            .collect();
                        for line in &list {
                            fancy_regex::Regex::new(line)
                                .map_err(|e| format!("AI access, blocked `{line}`: {e}"))?;
                        }
                        Some(list)
                    }
                },
            },
            ..SessionSettings::default()
        })
    }

    /// A session must connect with what it sets and inherits: one without
    /// a serial port, say, would be saved but not listed.
    fn check_session(&self, inherited: &SessionSettings) -> Result<(), String> {
        let Target::Session { trail, .. } = &self.target else {
            return Ok(());
        };
        let settings = inherited.overlay(&self.settings()?);
        przmterm_session::Session::resolve(
            self.name.trim(),
            trail.clone(),
            PathBuf::new(),
            &settings,
        )
        .map(drop)
        .map_err(|e| match e {
            przmterm_session::SessionError::Invalid { reason, .. } => readable(&reason),
            e => e.to_string(),
        })
    }

    /// Saves, or for the global defaults with changes, asks first.
    fn save_or_confirm(&mut self, config_dir: &Path) -> Result<Option<String>, String> {
        if self.target == Target::Global {
            let new = self.settings()?;
            let keys = self.original.changed_fields(&new);
            if !keys.is_empty() {
                self.confirm = Some(Confirm {
                    keys,
                    scope: Scope::Inheriting,
                });
                return Ok(None);
            }
        }
        self.save(config_dir, Scope::Inheriting).map(Some)
    }

    /// Writes the file. For the global defaults, `apply_existing == false`
    /// first keeps the old values in the sessions that use them.
    fn save(&self, config_dir: &Path, scope: Scope) -> Result<String, String> {
        let settings = self.settings()?;
        match &self.target {
            Target::Session { original, trail } => {
                przmterm_session::validate_name(&self.name)?;
                let name = self.name.trim().to_string();
                przmterm_session::save_session(
                    config_dir,
                    &trail.join("/"),
                    &name,
                    &settings,
                    original.as_deref(),
                )
                .map_err(|e| e.to_string())?;
                Ok(name)
            }
            Target::Folder { dir, .. } => {
                przmterm_session::validate_name(&self.name)?;
                let name = self.name.trim().to_string();
                przmterm_session::write_folder_settings(dir, &settings)
                    .map_err(|e| e.to_string())?;
                przmterm_session::rename_folder(dir, &name).map_err(|e| e.to_string())?;
                Ok(format!("folder {name}"))
            }
            Target::Global => {
                let mut note = String::new();
                let keys = self.original.changed_fields(&settings);
                if scope == Scope::All {
                    let cleared = przmterm_session::clear_overrides(config_dir, &keys)
                        .map_err(|e| e.to_string())?;
                    if cleared > 0 {
                        note = format!(" {cleared} session(s) or folder(s) now follow it.");
                    }
                }
                if scope == Scope::NewOnly {
                    let (kept, skipped) =
                        przmterm_session::keep_old_defaults(config_dir, &self.original, &keys)
                            .map_err(|e| e.to_string())?;
                    note = format!(" {kept} session(s) kept their old values");
                    if skipped > 0 {
                        note.push_str(&format!(" ({skipped} unset value(s) could not be kept)"));
                    }
                    note.push('.');
                }
                przmterm_session::write_defaults(config_dir, &settings)
                    .map_err(|e| e.to_string())?;
                Ok(format!("global settings.{note}"))
            }
        }
    }

    /// Draws the window. `root` is the session tree, for the folder picker
    /// and the inherited values.
    pub fn show(&mut self, ctx: &egui::Context, config_dir: &Path, root: &Folder) -> EditorResult {
        let mut result = EditorResult::Open;
        let mut open = true;
        let none = SessionSettings::default();
        let inherited: SessionSettings = match &self.target {
            Target::Session { trail, .. } => root.find(trail).map(|f| f.settings.clone()),
            Target::Folder { trail, .. } => root.find(trail).map(|f| f.inherited.clone()),
            Target::Global => None,
        }
        .unwrap_or(none);
        let protocol = self.protocol.or(inherited.protocol).unwrap_or_default();
        let logs_dir = tilde(&config_dir.join(przmterm_session::LOGS_DIR));
        let cx = Context {
            inherited: &inherited,
            ssh_sessions: root
                .all_sessions()
                .into_iter()
                .filter(|s| s.protocol == Protocol::Ssh)
                .map(|s| s.name.clone())
                .collect(),
            keyword_sets: przmterm_session::keyword_sets(config_dir),
            protocol,
            logs_dir,
            config_dir,
        };
        let popup = egui::Popup::is_any_open(ctx);
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });

        // Anchored at its top: a page that grows the window pushes its
        // bottom down, and nothing moves under the pointer.
        let screen = ctx.content_rect();
        egui::Window::new(crate::theme::window_title(ctx, self.title.clone()))
            .pivot(egui::Align2::CENTER_TOP)
            .default_pos(egui::pos2(
                screen.center().x,
                screen.top() + (screen.height() * 0.12).max(40.0),
            ))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(660.0)
            .show(ctx, |ui| {
                if let Some(confirm) = &mut self.confirm {
                    // The confirmation replaces the form.
                    let mut back = false;
                    let mut go = false;
                    ui.label(egui::RichText::new("This change cannot be undone.").strong());
                    let names: Vec<&str> = confirm.keys.iter().map(|k| field_label(k)).collect();
                    ui.label(format!("Changed: {}.", names.join(", ")));
                    ui.add_space(6.0);
                    ui.label("Apply the new values to:");
                    ui.add_space(2.0);
                    for (scope, text, hint) in [
                        (
                            Scope::Inheriting,
                            "Sessions that use the default",
                            "Sessions and folders with their own value keep it.",
                        ),
                        (
                            Scope::All,
                            "All sessions, including customized ones",
                            "Their own values for these fields are removed.",
                        ),
                        (
                            Scope::NewOnly,
                            "New sessions only",
                            "Existing sessions keep their current value.",
                        ),
                    ] {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            if crate::theme::radio(ui, confirm.scope == scope, text).clicked() {
                                confirm.scope = scope;
                            }
                            ui.horizontal(|ui| {
                                ui.add_space(22.0);
                                ui.weak(hint);
                            });
                        });
                        ui.add_space(2.0);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            go = crate::theme::primary_button(ui, "Save").clicked() || enter;
                            back = ui.button("Back").clicked() || escape;
                        });
                    });
                    if go {
                        let scope = confirm.scope;
                        match self.save(config_dir, scope) {
                            Ok(msg) => result = EditorResult::Saved(msg),
                            Err(e) => {
                                self.error = Some(e);
                                self.confirm = None;
                            }
                        }
                    } else if back {
                        self.confirm = None;
                    }
                    return;
                }

                ui.weak(match self.target {
                    Target::Global => {
                        "Defaults for every session. Folders and sessions can set their own."
                    }
                    Target::Folder { .. } => "Defaults for the sessions in this folder.",
                    Target::Session { .. } => "Empty fields use the value shown in grey.",
                });
                ui.weak("Enter saves, Esc cancels.");
                ui.add_space(6.0);
                // A menu of pages on the left, the page on the right; the
                // window keeps one size whichever page is shown.
                let pages = self.pages(&cx);
                if !pages.contains(&self.page) {
                    self.page = Page::General;
                }
                // The page grows the window; it scrolls only past the screen.
                let max_height = (ctx.content_rect().height() - 240.0).max(210.0);
                let row = ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(118.0);
                        for page in pages {
                            let item = egui::Button::selectable(self.page == page, page.label())
                                .right_text("")
                                .min_size(egui::vec2(118.0, 26.0));
                            if ui.add(item).clicked() {
                                self.page = page;
                            }
                        }
                    });
                    // Room for the divider drawn below (a separator here
                    // would stretch the window to the screen's height).
                    ui.add_space(14.0);
                    // The page, as tall as it needs (at least as tall as the
                    // menu); a scroll bar only when the screen is too short.
                    ui.allocate_ui_with_layout(
                        egui::vec2(520.0, max_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_size(egui::vec2(520.0, 210.0));
                            // The scroll bar takes its own room instead of
                            // covering the buttons at the right edge.
                            ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
                            egui::ScrollArea::vertical()
                                .id_salt(("editor-page", self.page))
                                .max_height(max_height)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(self.page.label())
                                            .font(egui::FontId::new(17.0, crate::theme::semibold()))
                                            .color(crate::theme::p().text_strong),
                                    );
                                    ui.add_space(6.0);
                                    self.form(ui, &cx, root)
                                });
                        },
                    );
                });
                let x = row.response.rect.left() + 118.0 + 7.0;
                ui.painter().vline(
                    x,
                    row.response.rect.y_range(),
                    ui.visuals().widgets.noninteractive.bg_stroke,
                );

                if let Some(err) = &self.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(8.0);
                if escape && !popup {
                    result = EditorResult::Cancel;
                }
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::theme::primary_button(ui, "Save").clicked() || (enter && !popup) {
                            match self
                                .check_session(&inherited)
                                .and_then(|()| self.save_or_confirm(config_dir))
                            {
                                Ok(Some(msg)) => result = EditorResult::Saved(msg),
                                Ok(None) => {}
                                Err(e) => self.error = Some(e),
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            result = EditorResult::Cancel;
                        }
                    });
                });
            });
        if !open {
            result = EditorResult::Cancel;
        }
        result
    }

    /// The log file name's variables, and the next file's name.
    fn log_help(&self, ui: &mut egui::Ui, cx: &Context) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(egui::RichText::new(LOG_HELP).monospace().size(12.0));
            let template = opt(&self.log_path).unwrap_or_else(|| {
                cx.inherited
                    .log
                    .path
                    .clone()
                    .unwrap_or(przmterm_session::DEFAULT_LOG_TEMPLATE.into())
            });
            let name = opt(&self.name).unwrap_or_else(|| "session".into());
            let host = opt(&self.host).unwrap_or_else(|| "host".into());
            let file = przmterm_session::expand_log_template(
                &template,
                &name,
                &host,
                &chrono::Local::now(),
            );
            let file = if file.is_absolute() {
                file
            } else {
                let dir = opt(&self.log_dir)
                    .or_else(|| cx.inherited.log.dir.clone())
                    .map(|d| przmterm_session::expand_tilde(&d))
                    .unwrap_or_else(|| cx.config_dir.join(przmterm_session::LOGS_DIR));
                dir.join(file)
            };
            ui.add_space(4.0);
            ui.label(format!("Next log file: {}", tilde(&file)));
        });
    }

    /// The pages this editor shows, in the side menu's order.
    fn pages(&self, cx: &Context) -> Vec<Page> {
        let session = self.is_session();
        let mut pages = vec![Page::General];
        let telnet_session = session && cx.protocol == Protocol::Telnet;
        let serial_session = session && cx.protocol == Protocol::Serial;
        if !telnet_session && !serial_session {
            pages.push(Page::Login);
            pages.push(Page::Ssh);
        }
        // Folders and the defaults also carry serial settings.
        if !session {
            pages.push(Page::Serial);
        }
        pages.extend([Page::Terminal, Page::Automation, Page::Logging, Page::Mcp]);
        pages
    }

    /// The page chosen in the side menu.
    fn form(&mut self, ui: &mut egui::Ui, cx: &Context, root: &Folder) {
        let session = self.is_session();
        match self.page {
            Page::General => grid(ui, "editor-general", |ui| {
                if self.target != Target::Global {
                    ui.label(if session { "Name" } else { "Folder name" });
                    ui.vertical(|ui| name_field(ui, &mut self.name, ""));
                    ui.end_row();
                }
                if let Target::Session { trail, .. } = &mut self.target {
                    ui.label("Folder");
                    folder_picker(ui, root, trail);
                    ui.end_row();
                }

                ui.label("Protocol");
                ui.horizontal(|ui| {
                    crate::theme::sunken_combos(ui);
                    egui::ComboBox::from_id_salt("protocol")
                        .width(crate::theme::combo_width(ui))
                        .selected_text(shown(
                            self.protocol.map(|p| p.to_string()),
                            cx.hint("protocol"),
                        ))
                        .show_ui(ui, |ui| {
                            for p in [Protocol::Ssh, Protocol::Telnet, Protocol::Serial] {
                                ui.selectable_value(&mut self.protocol, Some(p), p.to_string());
                            }
                        });
                    reset(ui, &mut self.protocol, &cx.hint("protocol"));
                });
                ui.end_row();

                if session && cx.protocol == Protocol::Serial {
                    self.serial_fields(ui, cx);
                } else {
                    self.network_fields(ui, cx);
                }
            }),
            Page::Login => grid(ui, "editor-login", |ui| self.login_fields(ui, cx)),
            Page::Ssh => {
                grid(ui, "editor-ssh", |ui| self.ssh_fields(ui, cx));
                section_help(
                    ui,
                    &mut self.help,
                    "Tunnels",
                    "Port forwarding, opened after login and closed with the session. \
                     Local: a port here reaches host:port through the server. Remote: a \
                     port on the server reaches host:port from here. SOCKS: a proxy here \
                     that goes out through the server.",
                );
                self.tunnels_ui(ui, cx);
            }
            Page::Serial => grid(ui, "editor-serial", |ui| self.serial_fields(ui, cx)),
            Page::Terminal => {
                grid(ui, "editor-terminal", |ui| self.terminal_fields(ui, cx));
                section(ui, "Font", "");
                grid(ui, "editor-font", |ui| self.font_fields(ui, cx));
                section(ui, "Colors", "");
                grid(ui, "editor-colors", |ui| self.color_fields(ui, cx));
                section(ui, "Cursor", "");
                grid(ui, "editor-cursor", |ui| self.cursor_fields(ui, cx));
            }
            Page::Automation => {
                grid(ui, "editor-automation", |ui| self.automation_fields(ui, cx));
                section_help(
                    ui,
                    &mut self.help,
                    "Anti-idle",
                    "Sends one character (a space, say) when nothing was typed for that \
                     many seconds, so the far end does not close the session. No Enter \
                     is sent. Empty: off.",
                );
                grid(ui, "editor-anti-idle", |ui| self.anti_idle_fields(ui, cx));
                section_help(
                    ui,
                    &mut self.help,
                    "Triggers",
                    "When a text shows up in the output, type an answer: --More-- → space, \
                     [confirm] → \\r (Enter), a device's prompt → a command. In the answer \
                     \\r is Enter, \\t Tab, \\e Esc and \\p a pause of 1 s. Once: only \
                     the first time per connection. Saved as plain text: keep passwords in \
                     the keychain, not here.",
                );
                self.triggers_ui(ui, cx);
            }
            Page::Mcp => {
                section_help(
                    ui,
                    &mut self.help,
                    "AI access",
                    "What an AI assistant connected with `przm mcp` may do with this \
                     session. None: it does not see it. Read: open it and read the \
                     screen. Full: also send commands and run scripts, except lines that \
                     match the blocked list (regular expressions, case ignored; empty \
                     uses the list shown in grey).",
                );
                grid(ui, "editor-mcp", |ui| self.mcp_fields(ui, cx));
            }
            Page::Logging => {
                grid(ui, "editor-logging", |ui| self.log_fields(ui, cx));
                // Right under the file name it explains.
                if self.show_log_help {
                    ui.add_space(4.0);
                    self.log_help(ui, cx);
                }
            }
        }
    }

    /// Host, port and keepalive, for SSH and Telnet.
    fn network_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        if self.is_session() {
            field(ui, "Host", &mut self.host, "10.0.0.1 or router.example.com");
        }
        field(ui, "Port", &mut self.port, &cx.hint("port"));
        field(
            ui,
            "Keepalive (s)",
            &mut self.keepalive,
            &cx.hint("keepalive"),
        );
    }

    /// How SSH logs in: user, vault entry, key, saved password.
    fn login_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        // A user name for every session makes no sense globally.
        if self.target != Target::Global {
            field(ui, "User name", &mut self.username, &cx.hint("username"));
        }
        self.credential_field(ui, cx);
        ui.label("Key file");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.identity_file)
                    .hint_text(cx.hint("identity_file"))
                    .desired_width(200.0),
            );
            if ui.button("Browse…").clicked()
                && let Some(path) = pick_key_file()
            {
                self.identity_file = tilde(&path);
            }
        });
        ui.end_row();
        ui.label("Save password");
        save_pick(
            ui,
            &mut self.save_password,
            cx.inherited.save_password,
            None,
        );
        ui.end_row();
        ui.label("Debug login")
            .on_hover_text("Show each step of the connection and login, like ssh -v");
        bool_pick(
            ui,
            &mut self.debug,
            cx.inherited.debug.unwrap_or(false),
            ["yes", "no"],
            None,
            &cx.hint("debug"),
        );
        ui.end_row();
    }

    /// SSH extras: old algorithms, X11, SCP shell, jump host, tunnels.
    fn ssh_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.label("Legacy algorithms").on_hover_text(
            "Also offer old algorithms for equipment that knows nothing newer: \
             SHA-1 key exchange, CBC and 3DES ciphers, HMAC-SHA1, DSA host keys. \
             A modern server never picks them; a connection using them shows a \
             \"legacy\" badge.",
        );
        bool_pick(
            ui,
            &mut self.legacy_algorithms,
            cx.inherited.legacy_algorithms.unwrap_or(false),
            ["yes", "no"],
            None,
            &cx.hint("legacy_algorithms"),
        );
        ui.end_row();

        ui.label("X11 forwarding").on_hover_text(
            "Show remote graphical programs on this computer's X server \
             (XQuartz on macOS, VcXsrv or similar on Windows), like ssh -X",
        );
        bool_pick(
            ui,
            &mut self.x11_forwarding,
            cx.inherited.x11_forwarding.unwrap_or(false),
            ["yes", "no"],
            None,
            &cx.hint("x11_forwarding"),
        );
        ui.end_row();

        ui.label("SCP shell").on_hover_text(
            "Where the SCP file browser and SCP transfers run their commands. \
             Default is the login shell; sudo su - or su - work as root (the \
             password is asked when needed)",
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.scp_shell)
                    .hint_text(cx.hint("scp_shell"))
                    .desired_width(200.0),
            );
            let pick_shell = crate::icons::button(ui, crate::icons::Icon::ChevronDown, false)
                .on_hover_text("Common shells");
            egui::Popup::menu(&pick_shell).show(|ui| {
                crate::theme::menu(ui);
                for shell in przmterm_transport::scp::SHELLS {
                    let current = if shell == "Default" {
                        self.scp_shell.is_empty()
                    } else {
                        self.scp_shell == shell
                    };
                    if ui.selectable_label(current, shell).clicked() {
                        self.scp_shell = if shell == "Default" {
                            String::new()
                        } else {
                            shell.to_string()
                        };
                        ui.close();
                    }
                }
            });
        });
        ui.end_row();

        ui.label("Jump host")
            .on_hover_text("Connect through this SSH server first (ProxyJump)");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.jump_host)
                    .hint_text(cx.hint("jump_host"))
                    .desired_width(200.0),
            );
            let pick_jump = crate::icons::button(ui, crate::icons::Icon::ChevronDown, false)
                .on_hover_text("Your SSH sessions");
            egui::Popup::menu(&pick_jump).show(|ui| {
                crate::theme::menu(ui);
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        if ui
                            .selectable_label(self.jump_host.is_empty(), "none")
                            .clicked()
                        {
                            self.jump_host.clear();
                            ui.close();
                        }
                        for name in &cx.ssh_sessions {
                            if Some(name.as_str()) == opt(&self.name).as_deref() {
                                continue; // not through itself
                            }
                            if ui.selectable_label(self.jump_host == *name, name).clicked() {
                                self.jump_host = name.clone();
                                ui.close();
                            }
                        }
                    });
            });
        });
        ui.end_row();
    }

    fn triggers_ui(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.vertical(|ui| {
            let Some(list) = &mut self.triggers else {
                // Not set here: the ones from above, if any; + copies them
                // in to change.
                let inherited = cx.inherited.triggers.clone().unwrap_or_default();
                ui.horizontal(|ui| {
                    if !inherited.is_empty() {
                        ui.weak(format!("From above: {}", cx.hint("trigger")));
                    }
                    if crate::icons::button(ui, crate::icons::Icon::Plus, false)
                        .on_hover_text("Add a trigger")
                        .clicked()
                    {
                        let mut list = inherited;
                        list.push(Trigger::default());
                        self.triggers = Some(list);
                    }
                });
                return;
            };
            let mut remove = None;
            let mut add = false;
            for (i, t) in list.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut t.pattern)
                            .hint_text("--More--")
                            .font(egui::TextStyle::Monospace)
                            .desired_width(96.0),
                    );
                    crate::theme::checkbox(ui, &mut t.regex, "regex")
                        .on_hover_text("The text is a regular expression");
                    ui.label("→");
                    ui.add(
                        egui::TextEdit::singleline(&mut t.send)
                            .hint_text("\\r or text")
                            .font(egui::TextStyle::Monospace)
                            .desired_width(80.0),
                    )
                    .on_hover_text("\\r Enter, \\t Tab, \\e Esc, \\p pause 1 s");
                    crate::theme::checkbox(ui, &mut t.once, "once")
                        .on_hover_text("Only the first time per connection");
                    if crate::icons::button(ui, crate::icons::Icon::Close, false)
                        .on_hover_text("Remove")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    if i == 0
                        && crate::icons::button(ui, crate::icons::Icon::Plus, false)
                            .on_hover_text("Add a trigger")
                            .clicked()
                    {
                        add = true;
                    }
                });
            }
            if let Some(i) = remove {
                list.remove(i);
            }
            if add {
                list.push(Trigger::default());
            }
            // None left: back to the ones from above.
            if list.is_empty() {
                self.triggers = None;
            }
        });
    }

    fn tunnels_ui(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.vertical(|ui| {
            let Some(list) = &mut self.forwards else {
                // Not set here: the ones from above, if any; + copies them
                // in to change.
                let inherited = cx.inherited.forwards.clone().unwrap_or_default();
                ui.horizontal(|ui| {
                    if !inherited.is_empty() {
                        ui.weak(format!("From above: {}", cx.hint("forward")));
                    }
                    if crate::icons::button(ui, crate::icons::Icon::Plus, false)
                        .on_hover_text("Add a tunnel")
                        .clicked()
                    {
                        let mut list = inherited;
                        list.push(Forward::default());
                        self.forwards = Some(list);
                    }
                });
                return;
            };
            let mut remove = None;
            let mut add = false;
            for (i, f) in list.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    crate::theme::sunken_combos(ui);
                    egui::ComboBox::from_id_salt(("tunnel-kind", i))
                        .width(80.0)
                        .selected_text(match f.kind {
                            ForwardKind::Local => "local",
                            ForwardKind::Remote => "remote",
                            ForwardKind::Dynamic => "SOCKS",
                        })
                        .show_ui(ui, |ui| {
                            for k in ForwardKind::ALL {
                                ui.selectable_value(&mut f.kind, k, k.label());
                            }
                        });
                    ui.add(
                        egui::TextEdit::singleline(&mut f.listen)
                            .hint_text(match f.kind {
                                ForwardKind::Remote => "server port",
                                _ => "local port",
                            })
                            .desired_width(80.0),
                    );
                    if f.kind == ForwardKind::Dynamic {
                        ui.weak("SOCKS proxy");
                    } else {
                        ui.label("→");
                        ui.add(
                            egui::TextEdit::singleline(&mut f.target)
                                .hint_text("host:port")
                                .desired_width(110.0),
                        );
                    }
                    if crate::icons::button(ui, crate::icons::Icon::Close, false)
                        .on_hover_text("Remove")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    if i == 0
                        && crate::icons::button(ui, crate::icons::Icon::Plus, false)
                            .on_hover_text("Add a tunnel")
                            .clicked()
                    {
                        add = true;
                    }
                });
            }
            if let Some(i) = remove {
                list.remove(i);
            }
            if add {
                list.push(Forward::default());
            }
            // None left: back to the ones from above.
            if list.is_empty() {
                self.forwards = None;
            }
        });
    }

    /// Port and line settings for serial consoles.
    fn serial_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        if self.is_session() {
            ui.label("Serial port");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.serial_port)
                        .hint_text(cx.hint("serial_port"))
                        .desired_width(200.0),
                );
                ui.menu_button("Ports ▾", |ui| {
                    crate::theme::menu(ui);
                    let ports = self.ports.get_or_insert_with(available_ports);
                    if ports.is_empty() {
                        ui.weak("No serial ports found.");
                    }
                    for p in ports.iter() {
                        let text = if p.description.is_empty() {
                            p.name.clone()
                        } else {
                            format!("{}  —  {}", p.name, p.description)
                        };
                        if ui
                            .selectable_label(self.serial_port == p.name, text)
                            .clicked()
                        {
                            self.serial_port = p.name.clone();
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("↻ Refresh").clicked() {
                        self.ports = None;
                    }
                });
            });
            ui.end_row();
        }

        ui.label("Baud rate");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.baud_rate)
                    .hint_text(cx.hint("baud_rate"))
                    .desired_width(200.0),
            );
            ui.menu_button("Rates ▾", |ui| {
                crate::theme::menu(ui);
                for rate in COMMON_BAUD_RATES {
                    let rate = rate.to_string();
                    if ui.selectable_label(self.baud_rate == rate, &rate).clicked() {
                        self.baud_rate = rate;
                        ui.close();
                    }
                }
            });
        });
        ui.end_row();

        ui.label("Data bits");
        choice_pick(
            ui,
            &mut self.data_bits,
            cx.inherited.data_bits.unwrap_or(DEFAULT_DATA_BITS),
            &[(5, "5"), (6, "6"), (7, "7"), (8, "8")],
            &cx.hint("data_bits"),
        );
        ui.end_row();
        ui.label("Parity");
        choice_pick(
            ui,
            &mut self.parity,
            cx.inherited.parity.unwrap_or_default(),
            &Parity::ALL.map(|p| (p, p.name())),
            &cx.hint("parity"),
        );
        ui.end_row();
        ui.label("Stop bits");
        choice_pick(
            ui,
            &mut self.stop_bits,
            cx.inherited.stop_bits.unwrap_or(DEFAULT_STOP_BITS),
            &[(1, "1"), (2, "2")],
            &cx.hint("stop_bits"),
        );
        ui.end_row();
        ui.label("Flow control");
        choice_pick(
            ui,
            &mut self.flow_control,
            cx.inherited.flow_control.unwrap_or_default(),
            &FlowControl::ALL.map(|f| (f, f.label())),
            &cx.hint("flow_control"),
        );
        ui.end_row();
    }

    /// The vault entry picker.
    fn credential_field(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.label("Vault entry").on_hover_text(
            "Log in with a user name, password and key passphrase from the \
             password vault (its user name replaces the one above)",
        );
        ui.horizontal(|ui| {
            let shown_name = self.credential.as_ref().map(|c| {
                if c.is_empty() {
                    "none".to_string()
                } else {
                    c.clone()
                }
            });
            crate::theme::sunken_combos(ui);
            egui::ComboBox::from_id_salt("credential")
                .width(crate::theme::combo_width(ui) - 60.0)
                .selected_text(shown(shown_name, cx.hint("credential")))
                .show_ui(ui, |ui| {
                    // "" turns off an entry inherited from above.
                    ui.selectable_value(&mut self.credential, Some(String::new()), "none");
                    match &self.vault_names {
                        Some(names) => {
                            for name in names {
                                ui.selectable_value(&mut self.credential, Some(name.clone()), name);
                            }
                        }
                        None => {
                            // Locked: the names the sessions use, and the
                            // one set here, so a chosen entry is never
                            // missing from the list.
                            let mut names = self.known_vault_names.clone().unwrap_or_default();
                            if let Some(current) = self.credential.clone().filter(|c| !c.is_empty())
                                && !names.contains(&current)
                            {
                                names.push(current);
                            }
                            for name in &names {
                                ui.selectable_value(&mut self.credential, Some(name.clone()), name);
                            }
                            ui.separator();
                            if ui.button("Unlock to see every entry…").clicked() {
                                self.unlock_request = true;
                                ui.close();
                            }
                        }
                    }
                });
            reset(ui, &mut self.credential, &cx.hint("credential"));
            let label = if self.vault_names.is_some() {
                "Vault…"
            } else {
                "Unlock…"
            };
            if ui
                .button(label)
                .on_hover_text("Open the password vault")
                .clicked()
            {
                self.vault_request = true;
            }
        });
        ui.end_row();
    }

    /// Keywords, triggers, logon script, reconnecting.
    fn automation_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.label("Keywords")
            .on_hover_text("Highlight words and patterns in the output with a keyword set");
        ui.horizontal(|ui| {
            let shown_name = self.keywords.as_ref().map(|k| {
                if k.is_empty() {
                    "none".to_string()
                } else {
                    k.clone()
                }
            });
            crate::theme::sunken_combos(ui);
            egui::ComboBox::from_id_salt("keywords")
                .width(crate::theme::combo_width(ui))
                .selected_text(shown(shown_name, cx.hint("keywords")))
                .show_ui(ui, |ui| {
                    // "" turns off a set inherited from above.
                    ui.selectable_value(&mut self.keywords, Some(String::new()), "none");
                    for name in &cx.keyword_sets {
                        ui.selectable_value(&mut self.keywords, Some(name.clone()), name);
                    }
                });
            reset(ui, &mut self.keywords, &cx.hint("keywords"));
            if ui
                .button("Edit sets…")
                .on_hover_text("Create and change keyword sets")
                .clicked()
            {
                let current = self
                    .keywords
                    .clone()
                    .or_else(|| cx.inherited.keywords.clone())
                    .filter(|k| !k.is_empty());
                self.keyword_request = Some(current);
            }
        });
        ui.end_row();

        ui.label("Logon script").on_hover_text(
            "A Rhai script run each time the session connects, \
             e.g. to answer the login prompts",
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.logon_script)
                    .hint_text(cx.hint("logon_script"))
                    .desired_width(200.0),
            );
            if ui.button("Browse…").clicked()
                && let Some(name) = crate::buttonbar::pick_script(Some(cx.config_dir))
            {
                self.logon_script = name;
            }
        });
        ui.end_row();

        ui.label("Auto reconnect").on_hover_text(
            "Reconnect on its own when the connection drops or cannot be made \
             (not after a normal logout or a failed login)",
        );
        bool_pick(
            ui,
            &mut self.auto_reconnect,
            cx.inherited.auto_reconnect.unwrap_or(false),
            ["yes", "no"],
            None,
            &cx.hint("auto_reconnect"),
        );
        ui.end_row();
    }

    /// AI access: the level, and the commands never sent.
    fn mcp_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.label("Access");
        choice_pick(
            ui,
            &mut self.mcp_access,
            cx.inherited.mcp.access.unwrap_or_default(),
            &[
                (McpAccess::None, "none"),
                (McpAccess::Read, "read"),
                (McpAccess::Full, "full"),
            ],
            &cx.hint("mcp.access"),
        );
        ui.end_row();
        ui.label("Blocked");
        let hint = match &cx.inherited.mcp.block {
            Some(list) if list.is_empty() => "nothing".to_string(),
            Some(list) => list.join("\n"),
            None => przmterm_session::DEFAULT_MCP_BLOCK.join("\n"),
        };
        ui.add(
            egui::TextEdit::multiline(&mut self.mcp_block)
                .hint_text(hint)
                .font(egui::TextStyle::Monospace)
                .desired_rows(6)
                .desired_width(380.0),
        );
        ui.end_row();
    }

    /// The anti-idle text and how often, on one row.
    fn anti_idle_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.label("Send");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.anti_idle)
                    .char_limit(przmterm_session::ANTI_IDLE_MAX)
                    .font(egui::TextStyle::Monospace)
                    .hint_text(cx.hint("anti_idle"))
                    .desired_width(44.0),
            )
            .on_hover_text("One character, sent as typed");
            ui.label("every");
            ui.add(
                egui::TextEdit::singleline(&mut self.anti_idle_interval)
                    .char_limit(5)
                    .hint_text(cx.hint("anti_idle_interval"))
                    .desired_width(48.0),
            );
            ui.label("seconds");
        });
        ui.end_row();
    }

    /// Terminal type and history.
    fn terminal_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.label("Terminal type");
        ui.horizontal(|ui| {
            crate::theme::sunken_combos(ui);
            egui::ComboBox::from_id_salt("term-type")
                .width(crate::theme::combo_width(ui))
                .selected_text(shown(self.term_type.clone(), cx.hint("terminal.term_type")))
                .show_ui(ui, |ui| {
                    for t in TERM_TYPES {
                        ui.selectable_value(&mut self.term_type, Some(t.to_string()), t);
                    }
                    if let Some(custom) = self.term_type.clone()
                        && !TERM_TYPES.contains(&custom.as_str())
                    {
                        ui.selectable_value(&mut self.term_type, Some(custom.clone()), custom);
                    }
                });
            reset(ui, &mut self.term_type, &cx.hint("terminal.term_type"));
        });
        ui.end_row();
        field(
            ui,
            "Scrollback lines",
            &mut self.scrollback,
            &cx.hint("terminal.scrollback"),
        );
    }

    /// The terminal text's font.
    fn font_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.label("Family")
            .on_hover_text("The font of the terminal text: built in, or one installed here");
        ui.horizontal(|ui| {
            let shown_name = self.font.as_ref().map(|f| {
                if f.is_empty() {
                    crate::fonts::BUILT_IN.to_string()
                } else {
                    f.clone()
                }
            });
            crate::theme::sunken_combos(ui);
            egui::ComboBox::from_id_salt("terminal-font")
                .width(crate::theme::combo_width(ui))
                .height(320.0)
                .selected_text(shown(shown_name, cx.hint("terminal.font")))
                .show_ui(ui, |ui| {
                    let current = match self.font.as_deref() {
                        Some("") | None => None,
                        Some(f) => Some(f),
                    };
                    match crate::fonts::font_menu(ui, self.font_families.as_ref(), current) {
                        Some(crate::fonts::FontChoice::BuiltIn) => self.font = Some(String::new()),
                        Some(crate::fonts::FontChoice::Family(f)) => self.font = Some(f),
                        None => {}
                    }
                });
            reset(ui, &mut self.font, &cx.hint("terminal.font"));
        });
        ui.end_row();
        field(
            ui,
            "Size",
            &mut self.font_size,
            &cx.hint("terminal.font_size"),
        );
    }

    /// The cursor's shape, colour and blinking.
    /// The terminal's background and text colours, with a sample.
    fn color_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        // The colour in effect: set here, else inherited, else default.
        let effective = |own: &Option<String>, inherited: &Option<String>, default: [u8; 3]| {
            own.clone()
                .or_else(|| inherited.clone())
                .and_then(|c| przmterm_session::parse_color(&c))
                .unwrap_or(default)
        };
        let mut bg = effective(
            &self.background,
            &cx.inherited.terminal.background,
            przmterm_session::DEFAULT_BACKGROUND,
        );
        let mut fg = effective(
            &self.foreground,
            &cx.inherited.terminal.foreground,
            przmterm_session::DEFAULT_FOREGROUND,
        );
        ui.label("Background");
        if ui.color_edit_button_srgb(&mut bg).changed() {
            self.background = Some(przmterm_session::color_text(bg));
        }
        ui.end_row();
        ui.label("Text");
        if ui.color_edit_button_srgb(&mut fg).changed() {
            self.foreground = Some(przmterm_session::color_text(fg));
        }
        ui.end_row();
        ui.label("");
        color_sample(ui, bg, fg);
        ui.end_row();
    }

    fn cursor_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.label("Shape");
        ui.horizontal_top(|ui| {
            let effective = self
                .cursor
                .unwrap_or(cx.inherited.terminal.cursor.unwrap_or_default());
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for shape in przmterm_session::CursorShape::ALL {
                    if crate::theme::radio(ui, effective == shape, crate::cursor_label(shape))
                        .clicked()
                    {
                        self.cursor = Some(shape);
                    }
                }
            });
        });
        ui.end_row();
        ui.label("Color");
        ui.horizontal(|ui| {
            // The colour in effect: set here, else inherited, else default.
            let effective = self
                .cursor_color
                .clone()
                .or_else(|| cx.inherited.terminal.cursor_color.clone())
                .and_then(|c| przmterm_session::parse_color(&c));
            let mut color = effective.unwrap_or(default_cursor());
            if ui.color_edit_button_srgb(&mut color).changed() {
                self.cursor_color = Some(przmterm_session::color_text(color));
            }
        });
        ui.end_row();
        ui.label("");
        ui.horizontal(|ui| {
            let mut blink = self
                .cursor_blink
                .unwrap_or(cx.inherited.terminal.cursor_blink.unwrap_or(false));
            if crate::theme::checkbox(ui, &mut blink, "Blinking cursor").changed() {
                self.cursor_blink = Some(blink);
            }
        });
        ui.end_row();
    }

    /// Logging to a file.
    fn log_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
        let row = ui.label("Log to file");
        if std::mem::take(&mut self.scroll_to_log) {
            row.scroll_to_me(Some(egui::Align::Center));
        }
        ui.horizontal(|ui| {
            let mut on = self
                .log
                .unwrap_or(cx.inherited.log.enabled.unwrap_or(false));
            let chosen = Some(if on { 0 } else { 1 });
            if let Some(i) = crate::theme::segmented(ui, chosen, &["yes", "no"]) {
                on = i == 0;
                self.log = Some(on);
            }
        });
        ui.end_row();

        ui.label("Folder");
        // The file name's field reaches as far as Browse… does here.
        let row = ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.log_dir)
                    .hint_text(cx.hint("log.dir"))
                    .desired_width(200.0),
            );
            if ui.button("Browse…").clicked()
                && let Some(dir) = rfd::FileDialog::new()
                    .set_title("Folder for log files")
                    .pick_folder()
            {
                self.log_dir = tilde(&dir);
            }
        });
        let full_width = row.response.rect.width();
        ui.end_row();

        ui.label("File name");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.log_path)
                    .hint_text(cx.hint("log.path"))
                    .desired_width(full_width),
            );
            if ui
                .selectable_label(self.show_log_help, " ? ")
                .on_hover_text("Variables and examples")
                .clicked()
            {
                self.show_log_help = !self.show_log_help;
            }
        });
        ui.end_row();
    }
}

/// A text field for a session or folder name. Characters a file name cannot
/// have (`/ \ : * ? " < > |`) are dropped as they are typed, with a note.
pub fn name_field(ui: &mut egui::Ui, value: &mut String, hint: &str) -> egui::Response {
    let id = ui.make_persistent_id(("name-field-dropped", hint));
    let response = ui
        .add(
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(280.0),
        )
        .on_hover_text("Not allowed: / \\ : * ? \" < > |");
    if response.changed() && przmterm_session::strip_forbidden(value) {
        ui.data_mut(|d| d.insert_temp(id, std::time::Instant::now()));
    }
    let recent = ui
        .data(|d| d.get_temp::<std::time::Instant>(id))
        .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(4));
    if recent {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "/ \\ : * ? \" < > | cannot be used in names",
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }
    response
}

/// A two-column form grid; every grid uses the same label width so that
/// consecutive grids line up.
/// A titled group in a page, with an optional line about it.
/// A section title with a ? that shows `about` under it.
fn section_help(
    ui: &mut egui::Ui,
    open: &mut Option<&'static str>,
    title: &'static str,
    about: &str,
) {
    let shown = *open == Some(title);
    let mut toggle = false;
    crate::theme::section_header(ui, title, |ui| {
        toggle = ui
            .selectable_label(shown, " ? ")
            .on_hover_text("What is this?")
            .clicked();
    });
    if toggle {
        *open = if shown { None } else { Some(title) };
    }
    if *open == Some(title) {
        ui.weak(about);
        ui.add_space(4.0);
    }
}

fn section(ui: &mut egui::Ui, title: &str, about: &str) {
    crate::theme::section_title(ui, title);
    if !about.is_empty() {
        ui.weak(about);
        ui.add_space(4.0);
    }
}

fn grid(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .min_col_width(110.0)
        .spacing([12.0, 6.0])
        .show(ui, add);
}

/// One button per option with the effective value selected, plus ↺ when
/// the value is set here.
fn choice_pick<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    value: &mut Option<T>,
    inherited: T,
    options: &[(T, &str)],
    hint: &str,
) {
    ui.horizontal(|ui| {
        let effective = value.unwrap_or(inherited);
        let labels: Vec<&str> = options.iter().map(|o| o.1).collect();
        let chosen = options.iter().position(|o| o.0 == effective);
        if let Some(i) = crate::theme::segmented(ui, chosen, &labels) {
            *value = Some(options[i].0);
        }
        reset(ui, value, hint);
    });
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(crate::theme::FIELD_WIDTH),
    );
    ui.end_row();
}

fn pick_key_file() -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title("Choose an SSH private key");
    if let Some(ssh) = dirs::home_dir()
        .map(|h| h.join(".ssh"))
        .filter(|d| d.is_dir())
    {
        dialog = dialog.set_directory(ssh);
    }
    // Picking `key.pub` means the private key next to it.
    dialog
        .pick_file()
        .map(|path| przmterm_transport::ssh::private_key_for(&path).unwrap_or(path))
}

/// A compact folder chooser: a button showing the path, opening a scrollable tree.
fn folder_picker(ui: &mut egui::Ui, root: &Folder, trail: &mut Vec<String>) {
    let label = if trail.is_empty() {
        "Top level".to_string()
    } else {
        trail.join(" › ")
    };
    ui.menu_button(format!("{label}  ▾"), |ui| {
        crate::theme::menu(ui);
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                ui.set_min_width(240.0);
                if ui.selectable_label(trail.is_empty(), "Top level").clicked() {
                    trail.clear();
                    ui.close();
                }
                picker_level(ui, root, trail);
            });
    });
}

fn picker_level(ui: &mut egui::Ui, folder: &Folder, trail: &mut Vec<String>) {
    for f in &folder.folders {
        let selected = *trail == f.trail;
        if f.folders.is_empty() {
            if ui.selectable_label(selected, f.name.as_str()).clicked() {
                *trail = f.trail.clone();
                ui.close();
            }
        } else {
            egui::CollapsingHeader::new(f.name.as_str())
                .id_salt(&f.dir)
                .default_open(trail.starts_with(&f.trail))
                .show(ui, |ui| {
                    if ui
                        .selectable_label(selected, format!("(in {})", f.name))
                        .clicked()
                    {
                        *trail = f.trail.clone();
                        ui.close();
                    }
                    picker_level(ui, f, trail);
                });
        }
    }
}

/// The value a list shows: the one set here, or the inherited one in grey.
fn shown(value: Option<String>, inherited: String) -> egui::WidgetText {
    match value {
        Some(v) => v.into(),
        None => egui::RichText::new(inherited).weak().into(),
    }
}

/// The default cursor colour as red, green and blue.
fn default_cursor() -> [u8; 3] {
    let c = crate::view::CURSOR;
    [c.r(), c.g(), c.b()]
}

/// A small ↺ button that clears a value set here, back to the inherited one.
fn reset<T>(ui: &mut egui::Ui, value: &mut Option<T>, inherited: &str) {
    if value.is_some()
        && crate::icons::button(ui, crate::icons::Icon::Reset, false)
            .on_hover_text(format!("Back to the inherited value ({inherited})"))
            .clicked()
    {
        *value = None;
    }
}

/// A line of terminal text in `fg` on `bg`, as wide as a field.
pub fn color_sample(ui: &mut egui::Ui, bg: [u8; 3], fg: [u8; 3]) {
    egui::Frame::new()
        .fill(crate::view::rgb(bg))
        .stroke(egui::Stroke::new(1.0, crate::theme::p().control_border))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(crate::theme::FIELD_WIDTH - 16.0);
            ui.label(
                egui::RichText::new("user@host:~$ ls -l")
                    .monospace()
                    .color(crate::view::rgb(fg)),
            );
        });
}

/// Yes/no buttons with the effective value selected. `note` says where an
/// inherited value comes from.
fn bool_pick(
    ui: &mut egui::Ui,
    value: &mut Option<bool>,
    inherited: bool,
    labels: [&str; 2],
    note: Option<String>,
    hint: &str,
) {
    ui.horizontal(|ui| {
        let effective = value.unwrap_or(inherited);
        let chosen = Some(if effective { 0 } else { 1 });
        if let Some(i) = crate::theme::segmented(ui, chosen, &labels) {
            *value = Some(i == 0);
        }
        match (value.is_some(), note) {
            (true, _) => reset(ui, value, hint),
            (false, Some(note)) => {
                ui.weak(note);
            }
            (false, None) => {}
        }
    });
}

/// Ask / always / never for saving passwords. “Ask” is the unset state.
fn save_pick(
    ui: &mut egui::Ui,
    value: &mut Option<bool>,
    inherited: Option<bool>,
    note: Option<String>,
) {
    ui.horizontal(|ui| {
        let effective = value.or(inherited);
        let options = [None, Some(true), Some(false)];
        let chosen = options.iter().position(|o| *o == effective);
        if let Some(i) = crate::theme::segmented(ui, chosen, &["ask", "always", "never"]) {
            *value = options[i];
        }
        if value.is_none()
            && let Some(note) = note
        {
            ui.weak(note);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_that_cannot_connect_is_not_saved() {
        let none = SessionSettings::default();
        let mut e = Editor::new_session(&[]);
        e.name = "console".into();
        e.protocol = Some(Protocol::Serial);
        assert_eq!(
            e.check_session(&none),
            Err("Serial port is required for serial".to_string())
        );
        e.serial_port = "COM3".into();
        assert_eq!(e.check_session(&none), Ok(()));
        e.protocol = Some(Protocol::Telnet);
        assert!(e.check_session(&none).is_err(), "no host");
        let inherited = SessionSettings {
            host: Some("10.0.0.1".into()),
            ..SessionSettings::default()
        };
        assert_eq!(
            e.check_session(&inherited),
            Ok(()),
            "an inherited host does"
        );
    }

    #[test]
    fn serial_settings_survive_an_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = Editor::new_session(&[]);
        e.name = "console".into();
        e.protocol = Some(Protocol::Serial);
        e.serial_port = "/dev/ttyUSB0".into();
        e.baud_rate = "115200".into();
        e.parity = Some(Parity::Even);
        e.flow_control = Some(FlowControl::Software);
        e.save(tmp.path(), Scope::Inheriting).unwrap();

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let console = tree.root.all_sessions()[0];
        assert_eq!(console.display_target(), "/dev/ttyUSB0 115200 8E1 XON/XOFF");
        // Editing something else keeps the line settings.
        let mut e = Editor::edit(console);
        e.scrollback = "500".into();
        e.save(tmp.path(), Scope::Inheriting).unwrap();
        let toml = std::fs::read_to_string(tmp.path().join("sessions/console.toml")).unwrap();
        assert!(toml.contains("parity = \"even\""), "{toml}");
        assert!(toml.contains("flow_control = \"software\""), "{toml}");
        assert!(
            !toml.contains("data_bits"),
            "unset values stay unset: {toml}"
        );
    }

    #[test]
    fn tunnels_are_checked_and_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = Editor::new_session(&[]);
        e.name = "db".into();
        e.host = "10.0.0.5".into();
        e.jump_host = "bastion".into();
        e.forwards = Some(vec![
            Forward {
                kind: ForwardKind::Local,
                listen: "5432".into(),
                target: "localhost".into(),
            },
            Forward::default(),
        ]);
        let err = e.save(tmp.path(), Scope::Inheriting).unwrap_err();
        assert!(err.contains("Tunnel 1"), "{err}");
        e.forwards.as_mut().unwrap()[0].target = "localhost:5432".into();
        e.save(tmp.path(), Scope::Inheriting).unwrap();
        let toml = std::fs::read_to_string(tmp.path().join("sessions/db.toml")).unwrap();
        assert!(toml.contains("jump_host = \"bastion\""), "{toml}");
        assert_eq!(
            toml.matches("[[forward]]").count(),
            1,
            "blank rows dropped: {toml}"
        );
    }

    #[test]
    fn sessions_and_folders_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join(przmterm_session::SESSIONS_DIR);
        przmterm_session::create_folder(&sessions, "Lab").unwrap();

        let mut e = Editor::new_session(&["Lab".to_string()]);
        e.name = "sw1".into();
        e.host = "10.0.0.1".into();
        e.term_type = Some("vt220".into());
        e.log_dir = "/var/log/przm".into();
        e.save(tmp.path(), Scope::Inheriting).unwrap();
        assert!(sessions.join("Lab/sw1.toml").exists());

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let lab = tree.root.find(&["Lab".to_string()]).unwrap();
        let mut f = Editor::folder(lab);
        f.port = "2222".into();
        f.name = "Lab2".into();
        f.host = "ignored for folders".into();
        f.save(tmp.path(), Scope::Inheriting).unwrap();
        let folder_toml = std::fs::read_to_string(sessions.join("Lab2/folder.toml")).unwrap();
        assert_eq!(folder_toml, "port = 2222\n");

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let sw1 = tree.root.all_sessions()[0];
        assert_eq!(sw1.display_target(), "10.0.0.1:2222");
        assert_eq!(sw1.term_type, "vt220");
        assert_eq!(sw1.log_dir, Some(PathBuf::from("/var/log/przm")));

        let mut e = Editor::edit(sw1);
        assert!(e.port.is_empty(), "inherited values are not copied in");
        e.target = Target::Session {
            original: Some(sw1.path.clone()),
            trail: vec![],
        };
        e.save(tmp.path(), Scope::Inheriting).unwrap();
        assert!(sessions.join("sw1.toml").exists());
        assert!(
            !sessions.join("Lab2/sw1.toml").exists(),
            "moved to the top level"
        );

        let mut bad = Editor::new_session(&[]);
        bad.name = "x".into();
        bad.port = "abc".into();
        assert!(
            bad.save(tmp.path(), Scope::Inheriting)
                .unwrap_err()
                .contains("Port")
        );
    }

    #[test]
    fn global_change_asks_and_can_keep_old_values() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join(przmterm_session::SESSIONS_DIR);
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("a.toml"), "host = \"a\"\n").unwrap();

        // No change: saves at once.
        let mut g = Editor::global(tmp.path());
        assert!(g.save_or_confirm(tmp.path()).unwrap().is_some());

        // A change: asks first.
        let mut g = Editor::global(tmp.path());
        g.scrollback = "50000".into();
        assert!(g.save_or_confirm(tmp.path()).unwrap().is_none());
        let confirm = g.confirm.as_ref().unwrap();
        assert_eq!(confirm.keys, vec!["terminal.scrollback"]);

        // Keep old values: the session gets the old (built-in) value written in.
        let msg = g.save(tmp.path(), Scope::NewOnly).unwrap();
        assert!(msg.contains("1 session(s) kept"), "{msg}");
        let a = std::fs::read_to_string(sessions.join("a.toml")).unwrap();
        assert!(a.contains("scrollback = 10000"), "{a}");

        // New sessions use the new default.
        std::fs::write(sessions.join("b.toml"), "host = \"b\"\n").unwrap();
        let tree = przmterm_session::SessionTree::load(tmp.path());
        let b = tree.root.sessions.iter().find(|s| s.name == "b").unwrap();
        assert_eq!(b.scrollback, 50000);

        // All sessions: the value a.toml kept goes away.
        let mut g = Editor::global(tmp.path());
        g.scrollback = "20000".into();
        assert!(g.save_or_confirm(tmp.path()).unwrap().is_none());
        let msg = g.save(tmp.path(), Scope::All).unwrap();
        assert!(msg.contains("1 session(s) or folder(s)"), "{msg}");
        let a = std::fs::read_to_string(sessions.join("a.toml")).unwrap();
        assert!(!a.contains("scrollback"), "{a}");
    }

    #[test]
    fn tilde_shortens_home_paths() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(tilde(&home.join(".ssh/id_lab")), "~/.ssh/id_lab");
        assert_eq!(tilde(Path::new("/etc/x")), "/etc/x");
    }
}
