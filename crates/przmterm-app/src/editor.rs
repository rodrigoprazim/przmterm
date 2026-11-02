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
    ForwardKind, LogSettings, Parity, Session, SessionSettings, TerminalSettings, Trigger,
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
    /// Let existing sessions that use the default follow the new value.
    apply_existing: bool,
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
    /// The vault window was asked for.
    vault_request: bool,
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
    /// The keyword editor was asked for, for this set (if any).
    keyword_request: Option<Option<String>>,
    /// Tunnels set here; `None` inherits the list from above.
    forwards: Option<Vec<Forward>>,
    /// Triggers set here; `None` inherits the list from above.
    triggers: Option<Vec<Trigger>>,
    show_log_help: bool,
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
        "forward" => "tunnels",
        "trigger" => "triggers",
        "baud_rate" => "baud rate",
        "data_bits" => "data bits",
        "parity" => "parity",
        "stop_bits" => "stop bits",
        "flow_control" => "flow control",
        "terminal.term_type" => "terminal type",
        "terminal.scrollback" => "scrollback",
        "log.enabled" => "log to file",
        "log.path" => "log file",
        "log.dir" => "log folder",
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
            "log.enabled" => "no".into(),
            "log.path" => przmterm_session::DEFAULT_LOG_TEMPLATE.into(),
            "log.dir" => self.logs_dir.clone(),
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
            vault_request: false,
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
            keyword_request: None,
            forwards: s.forwards.clone(),
            triggers: s.triggers.clone(),
            show_log_help: false,
            confirm: None,
            error: None,
        }
    }

    /// The vault window was asked for (once).
    pub fn take_vault_request(&mut self) -> bool {
        std::mem::take(&mut self.vault_request)
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
            },
            log: LogSettings {
                enabled: self.log,
                path: opt(&self.log_path),
                dir: opt(&self.log_dir),
            },
            ..SessionSettings::default()
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
                    apply_existing: true,
                });
                return Ok(None);
            }
        }
        self.save(config_dir, true).map(Some)
    }

    /// Writes the file. For the global defaults, `apply_existing == false`
    /// first keeps the old values in the sessions that use them.
    fn save(&self, config_dir: &Path, apply_existing: bool) -> Result<String, String> {
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
                if !apply_existing {
                    let keys = self.original.changed_fields(&settings);
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
        };
        let popup = egui::Popup::is_any_open(ctx);
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });

        egui::Window::new(self.title.clone())
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .show(ctx, |ui| {
                if let Some(confirm) = &mut self.confirm {
                    // The confirmation replaces the form.
                    let mut back = false;
                    let mut go = false;
                    ui.label(egui::RichText::new("This change cannot be undone.").strong());
                    let names: Vec<&str> = confirm.keys.iter().map(|k| field_label(k)).collect();
                    ui.label(format!("Changed: {}.", names.join(", ")));
                    ui.add_space(6.0);
                    ui.checkbox(
                        &mut confirm.apply_existing,
                        "Apply the new values to existing sessions that use the default",
                    );
                    ui.weak(if confirm.apply_existing {
                        "Sessions and folders with their own value keep it."
                    } else {
                        "Existing sessions keep their current value; only new sessions use the new one."
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        go = ui.button("Save").clicked() || enter;
                        back = ui.button("Back").clicked() || escape;
                    });
                    if go {
                        let apply = confirm.apply_existing;
                        match self.save(config_dir, apply) {
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
                    Target::Global => "Defaults for every session. Folders and sessions can set their own.",
                    Target::Folder { .. } => "Defaults for the sessions in this folder.",
                    Target::Session { .. } => "Empty fields use the value shown in grey.",
                });
                ui.weak("Enter saves, Esc cancels.");
                ui.add_space(6.0);
                // Long forms scroll on small screens.
                egui::ScrollArea::vertical()
                    .max_height((ctx.content_rect().height() - 220.0).max(200.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| self.form(ui, &cx, root));

                if self.show_log_help {
                    ui.add_space(4.0);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.label(egui::RichText::new(LOG_HELP).monospace().size(12.0));
                        let template = opt(&self.log_path).unwrap_or_else(|| {
                            inherited
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
                                .or_else(|| inherited.log.dir.clone())
                                .map(|d| przmterm_session::expand_tilde(&d))
                                .unwrap_or_else(|| config_dir.join(przmterm_session::LOGS_DIR));
                            dir.join(file)
                        };
                        ui.add_space(4.0);
                        ui.label(format!("Next log file: {}", tilde(&file)));
                    });
                }

                if let Some(err) = &self.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(8.0);
                if escape && !popup {
                    result = EditorResult::Cancel;
                }
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() || (enter && !popup) {
                        match self.save_or_confirm(config_dir) {
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
        if !open {
            result = EditorResult::Cancel;
        }
        result
    }

    fn form(&mut self, ui: &mut egui::Ui, cx: &Context, root: &Folder) {
        let session = self.is_session();
        grid(ui, "editor-main", |ui| {
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
                egui::ComboBox::from_id_salt("protocol")
                    .width(250.0)
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

            if cx.protocol == Protocol::Serial {
                self.serial_fields(ui, cx);
            } else {
                self.network_fields(ui, cx);
            }
        });

        // Folders and the global defaults also carry the other protocols'
        // settings, out of the way until wanted.
        if !session {
            let (title, serial) = if cx.protocol == Protocol::Serial {
                ("SSH and Telnet", false)
            } else {
                ("Serial ports", true)
            };
            egui::CollapsingHeader::new(title)
                .id_salt("editor-other")
                .show(ui, |ui| {
                    grid(ui, "editor-other-grid", |ui| {
                        if serial {
                            self.serial_fields(ui, cx);
                        } else {
                            self.network_fields(ui, cx);
                        }
                    });
                });
        }

        grid(ui, "editor-terminal", |ui| self.terminal_fields(ui, cx));
    }

    /// Host, port and login, for SSH and Telnet.
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
        // Telnet logs in inside the terminal; these are SSH settings.
        if self.is_session() && cx.protocol == Protocol::Telnet {
            return;
        }
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

        ui.label("Jump host")
            .on_hover_text("Connect through this SSH server first (ProxyJump)");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.jump_host)
                    .hint_text(cx.hint("jump_host"))
                    .desired_width(200.0),
            );
            ui.menu_button("Sessions ▾", |ui| {
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

        ui.label("Tunnels")
            .on_hover_text("Port forwarding, opened after login and closed with the session");
        self.tunnels_ui(ui, cx);
        ui.end_row();
    }

    fn triggers_ui(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.vertical(|ui| {
            let Some(list) = &mut self.triggers else {
                ui.horizontal(|ui| {
                    ui.weak(cx.hint("trigger"));
                    if ui
                        .small_button("+ Add")
                        .on_hover_text("Set the triggers here")
                        .clicked()
                    {
                        let mut list = cx.inherited.triggers.clone().unwrap_or_default();
                        list.push(Trigger::default());
                        self.triggers = Some(list);
                    }
                });
                return;
            };
            let mut remove = None;
            for (i, t) in list.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut t.pattern)
                            .hint_text("--More--")
                            .font(egui::TextStyle::Monospace)
                            .desired_width(96.0),
                    );
                    ui.checkbox(&mut t.regex, "regex")
                        .on_hover_text("The text is a regular expression");
                    ui.label("→");
                    ui.add(
                        egui::TextEdit::singleline(&mut t.send)
                            .hint_text("\\r or text")
                            .font(egui::TextStyle::Monospace)
                            .desired_width(80.0),
                    )
                    .on_hover_text("\\r Enter, \\t Tab, \\e Esc, \\p pause 1 s");
                    ui.checkbox(&mut t.once, "once")
                        .on_hover_text("Only the first time per connection");
                    if ui.small_button("×").on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                list.remove(i);
            }
            let mut inherit = false;
            ui.horizontal(|ui| {
                if ui.small_button("+ Add trigger").clicked() {
                    list.push(Trigger::default());
                }
                inherit = ui
                    .small_button("↺")
                    .on_hover_text(format!(
                        "Back to the inherited triggers ({})",
                        cx.hint("trigger")
                    ))
                    .clicked();
            });
            ui.weak("Saved as plain text: keep passwords in the keychain, not here.");
            if inherit {
                self.triggers = None;
            }
        });
    }

    fn tunnels_ui(&mut self, ui: &mut egui::Ui, cx: &Context) {
        ui.vertical(|ui| {
            let Some(list) = &mut self.forwards else {
                ui.horizontal(|ui| {
                    ui.weak(cx.hint("forward"));
                    if ui
                        .small_button("+ Add")
                        .on_hover_text("Set the tunnels here")
                        .clicked()
                    {
                        let mut list = cx.inherited.forwards.clone().unwrap_or_default();
                        list.push(Forward::default());
                        self.forwards = Some(list);
                    }
                });
                return;
            };
            let mut remove = None;
            for (i, f) in list.iter_mut().enumerate() {
                ui.horizontal(|ui| {
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
                    if ui.small_button("×").on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                list.remove(i);
            }
            let mut inherit = false;
            ui.horizontal(|ui| {
                if ui.small_button("+ Add tunnel").clicked() {
                    list.push(Forward::default());
                }
                inherit = ui
                    .small_button("↺")
                    .on_hover_text(format!(
                        "Back to the inherited tunnels ({})",
                        cx.hint("forward")
                    ))
                    .clicked();
            });
            if inherit {
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
            egui::ComboBox::from_id_salt("credential")
                .width(200.0)
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
                            ui.weak("Unlock the vault to list its entries");
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
                .small_button(label)
                .on_hover_text("Open the password vault")
                .clicked()
            {
                self.vault_request = true;
            }
        });
        ui.end_row();
    }

    fn terminal_fields(&mut self, ui: &mut egui::Ui, cx: &Context) {
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
            egui::ComboBox::from_id_salt("keywords")
                .width(250.0)
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
                .small_button("Edit sets…")
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

        ui.label("Triggers").on_hover_text(
            "When this text appears in the output, type the answer: \
             --More-- → space, [confirm] → Enter…",
        );
        self.triggers_ui(ui, cx);
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

        ui.label("Terminal type");
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("term-type")
                .width(250.0)
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

        ui.label("Log to file");
        bool_pick(
            ui,
            &mut self.log,
            cx.inherited.log.enabled.unwrap_or(false),
            ["yes", "no"],
            None,
            &cx.hint("log.enabled"),
        );
        ui.end_row();

        ui.label("Log folder");
        ui.horizontal(|ui| {
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
        ui.end_row();

        ui.label("Log file name");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.log_path)
                    .hint_text(cx.hint("log.path"))
                    .desired_width(200.0),
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
        for &(v, text) in options {
            if ui.selectable_label(effective == v, text).clicked() {
                *value = Some(v);
            }
        }
        reset(ui, value, hint);
    });
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(280.0),
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
    ui.menu_button(format!("📁 {label}  ▾"), |ui| {
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
            if ui
                .selectable_label(selected, format!("📁 {}", f.name))
                .clicked()
            {
                *trail = f.trail.clone();
                ui.close();
            }
        } else {
            egui::CollapsingHeader::new(format!("📁 {}", f.name))
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

/// A small ↺ button that clears a value set here, back to the inherited one.
fn reset<T>(ui: &mut egui::Ui, value: &mut Option<T>, inherited: &str) {
    if value.is_some()
        && ui
            .small_button("↺")
            .on_hover_text(format!("Back to the inherited value ({inherited})"))
            .clicked()
    {
        *value = None;
    }
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
        for (text, v) in [(labels[0], true), (labels[1], false)] {
            if ui.selectable_label(effective == v, text).clicked() {
                *value = Some(v);
            }
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
        for (text, v) in [
            ("ask", None),
            ("always", Some(true)),
            ("never", Some(false)),
        ] {
            if ui.selectable_label(effective == v, text).clicked() {
                *value = v;
            }
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
    fn serial_settings_survive_an_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = Editor::new_session(&[]);
        e.name = "console".into();
        e.protocol = Some(Protocol::Serial);
        e.serial_port = "/dev/ttyUSB0".into();
        e.baud_rate = "115200".into();
        e.parity = Some(Parity::Even);
        e.flow_control = Some(FlowControl::Software);
        e.save(tmp.path(), true).unwrap();

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let console = tree.root.all_sessions()[0];
        assert_eq!(console.display_target(), "/dev/ttyUSB0 115200 8E1 XON/XOFF");
        // Editing something else keeps the line settings.
        let mut e = Editor::edit(console);
        e.scrollback = "500".into();
        e.save(tmp.path(), true).unwrap();
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
        let err = e.save(tmp.path(), true).unwrap_err();
        assert!(err.contains("Tunnel 1"), "{err}");
        e.forwards.as_mut().unwrap()[0].target = "localhost:5432".into();
        e.save(tmp.path(), true).unwrap();
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
        e.save(tmp.path(), true).unwrap();
        assert!(sessions.join("Lab/sw1.toml").exists());

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let lab = tree.root.find(&["Lab".to_string()]).unwrap();
        let mut f = Editor::folder(lab);
        f.port = "2222".into();
        f.name = "Lab2".into();
        f.host = "ignored for folders".into();
        f.save(tmp.path(), true).unwrap();
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
        e.save(tmp.path(), true).unwrap();
        assert!(sessions.join("sw1.toml").exists());
        assert!(
            !sessions.join("Lab2/sw1.toml").exists(),
            "moved to the top level"
        );

        let mut bad = Editor::new_session(&[]);
        bad.name = "x".into();
        bad.port = "abc".into();
        assert!(bad.save(tmp.path(), true).unwrap_err().contains("Port"));
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
        let msg = g.save(tmp.path(), false).unwrap();
        assert!(msg.contains("1 session(s) kept"), "{msg}");
        let a = std::fs::read_to_string(sessions.join("a.toml")).unwrap();
        assert!(a.contains("scrollback = 10000"), "{a}");

        // New sessions use the new default.
        std::fs::write(sessions.join("b.toml"), "host = \"b\"\n").unwrap();
        let tree = przmterm_session::SessionTree::load(tmp.path());
        let b = tree.root.sessions.iter().find(|s| s.name == "b").unwrap();
        assert_eq!(b.scrollback, 50000);
    }

    #[test]
    fn tilde_shortens_home_paths() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(tilde(&home.join(".ssh/id_lab")), "~/.ssh/id_lab");
        assert_eq!(tilde(Path::new("/etc/x")), "/etc/x");
    }
}
