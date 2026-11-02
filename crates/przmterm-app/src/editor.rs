//! Editor window for sessions, folders and the global defaults.
//!
//! Only the values set in the file itself are written. Every empty field
//! shows the value it inherits. Changing the global defaults asks first
//! whether existing sessions that use them should follow the change.

use std::path::{Path, PathBuf};

use eframe::egui;
use przmterm_core::Protocol;
use przmterm_session::{Folder, LogSettings, Session, SessionSettings, TerminalSettings};

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
    identity_file: String,
    serial_port: String,
    baud_rate: String,
    term_type: Option<String>,
    scrollback: String,
    log: Option<bool>,
    log_path: String,
    log_dir: String,
    save_password: Option<bool>,
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
        "identity_file" => "key file",
        "save_password" => "save password",
        "baud_rate" => "baud rate",
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
    /// Where they come from, for hints ("folder", "global"…); empty for built-ins.
    source: &'a str,
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
            "identity_file" => "agent and ~/.ssh keys".into(),
            "save_password" => "ask".into(),
            "serial_port" => "/dev/ttyUSB0 or COM3".into(),
            "baud_rate" => "9600".into(),
            "terminal.term_type" => "xterm-256color".into(),
            "terminal.scrollback" => "10000".into(),
            "log.enabled" => "no".into(),
            "log.path" => przmterm_session::DEFAULT_LOG_TEMPLATE.into(),
            "log.dir" => self.logs_dir.clone(),
            _ => String::new(),
        }
    }

    fn hint(&self, key: &str) -> String {
        match self.inherited.field_text(key) {
            Some(v) if self.source.is_empty() => v,
            Some(v) => format!("from {}: {v}", self.source),
            None => self.builtin(key),
        }
    }

    /// "from folder" when the inherited value comes from a file, else nothing.
    fn note(&self, key: &str) -> Option<String> {
        (self.inherited.field_text(key).is_some() && !self.source.is_empty())
            .then(|| format!("from {}", self.source))
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
            identity_file: text(&s.identity_file),
            serial_port: text(&s.serial_port),
            baud_rate: text(&s.baud_rate),
            term_type: s.terminal.term_type.clone(),
            scrollback: text(&s.terminal.scrollback),
            log: s.log.enabled,
            log_path: text(&s.log.path),
            log_dir: text(&s.log.dir),
            save_password: s.save_password,
            show_log_help: false,
            confirm: None,
            error: None,
        }
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
            identity_file: opt(&self.identity_file),
            save_password: self.save_password,
            serial_port: if session {
                opt(&self.serial_port)
            } else {
                None
            },
            baud_rate: number("Baud rate", &self.baud_rate)?,
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
                let path =
                    przmterm_session::write_session(config_dir, &trail.join("/"), &name, &settings)
                        .map_err(|e| e.to_string())?;
                if let Some(original) = original
                    && *original != path
                {
                    std::fs::remove_file(original)
                        .map_err(|e| format!("saved, but could not remove the old file: {e}"))?;
                }
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
        let (inherited, source): (SessionSettings, &str) = match &self.target {
            Target::Session { trail, .. } => match root.find(trail) {
                Some(f) => (
                    f.settings.clone(),
                    if trail.is_empty() { "global" } else { "folder" },
                ),
                None => (none.clone(), ""),
            },
            Target::Folder { trail, .. } => match root.find(trail) {
                Some(f) => (
                    f.inherited.clone(),
                    if trail.len() <= 1 {
                        "global"
                    } else {
                        "parent folder"
                    },
                ),
                None => (none.clone(), ""),
            },
            Target::Global => (none.clone(), ""),
        };
        let protocol = self.protocol.or(inherited.protocol).unwrap_or_default();
        let logs_dir = tilde(&config_dir.join(przmterm_session::LOGS_DIR));
        let cx = Context {
            inherited: &inherited,
            source,
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
                egui::Grid::new("editor-grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
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
        if self.target != Target::Global {
            ui.label(if self.is_session() {
                "Name"
            } else {
                "Folder name"
            });
            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(280.0));
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

        let session = self.is_session();
        if cx.protocol == Protocol::Serial {
            if session {
                field(
                    ui,
                    "Serial port",
                    &mut self.serial_port,
                    &cx.hint("serial_port"),
                );
            }
            field(ui, "Baud rate", &mut self.baud_rate, &cx.hint("baud_rate"));
        } else {
            if session {
                field(ui, "Host", &mut self.host, "10.0.0.1 or router.example.com");
            }
            field(ui, "Port", &mut self.port, &cx.hint("port"));
            field(ui, "User name", &mut self.username, &cx.hint("username"));
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
                cx.note("save_password"),
            );
            ui.end_row();
        }

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
            cx.note("log.enabled"),
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
    dialog.pick_file()
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
