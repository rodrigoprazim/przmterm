//! Editor window for sessions, folders and the global defaults.
//!
//! Only the values set in the file itself are written. Every empty field
//! shows the value it inherits. Global and folder settings can *force* a
//! field: the value then applies to everything below, overriding folders and
//! sessions, and those fields are shown locked further down.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use eframe::egui;
use przmterm_core::Protocol;
use przmterm_session::{Folder, Forced, LogSettings, Session, SessionSettings, TerminalSettings};

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

pub struct Editor {
    target: Target,
    title: String,
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
    save_password: Option<bool>,
    /// Fields this layer forces on everything below (global and folders).
    force: BTreeSet<String>,
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

/// The value a field has when nothing sets it.
fn builtin(key: &str, protocol: Protocol) -> String {
    match key {
        "protocol" => "ssh".into(),
        "port" => protocol.default_port().unwrap_or(22).to_string(),
        "username" => "asked at login".into(),
        "identity_file" => "agent and ~/.ssh keys".into(),
        "save_password" => "ask".into(),
        "serial_port" => "/dev/ttyUSB0 or COM3".into(),
        "baud_rate" => "9600".into(),
        "terminal.term_type" => "xterm-256color".into(),
        "terminal.scrollback" => "10000".into(),
        "log.enabled" => "no".into(),
        "log.path" => "%S-%Y%M%D-%h%m%s.log".into(),
        _ => String::new(),
    }
}

/// Everything a row needs to know about inheritance.
struct Context<'a> {
    /// Effective values coming from above.
    inherited: &'a SessionSettings,
    /// Where they come from, for hints ("folder", "global"…).
    source: &'a str,
    /// Fields locked by a layer above.
    forced: &'a [Forced],
    /// Whether this editor can force fields (global and folders).
    can_force: bool,
    protocol: Protocol,
}

impl Context<'_> {
    fn hint(&self, key: &str) -> String {
        match self.inherited.field_text(key) {
            Some(v) if self.source.is_empty() => v,
            Some(v) => format!("from {}: {v}", self.source),
            None => builtin(key, self.protocol),
        }
    }

    /// "from folder" when the inherited value comes from a file, else nothing.
    fn note(&self, key: &str) -> Option<String> {
        (self.inherited.field_text(key).is_some() && !self.source.is_empty())
            .then(|| format!("from {}", self.source))
    }

    fn locked(&self, key: &str) -> Option<String> {
        let forced = self.forced.iter().find(|f| f.key == key)?;
        let value = self
            .inherited
            .field_text(key)
            .unwrap_or_else(|| builtin(key, self.protocol));
        Some(format!("🔒 {value}  (forced by {})", forced.source))
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
        let settings = przmterm_session::read_session_settings(
            &folder.dir.join(przmterm_session::FOLDER_FILE),
        )
        .unwrap_or_default();
        Self::from_settings(
            Target::Folder {
                dir: folder.dir.clone(),
                trail: folder.trail.clone(),
            },
            &format!("Folder {}", folder.name),
            &folder.name,
            &settings,
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
            save_password: s.save_password,
            force: s.force.iter().cloned().collect(),
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
            },
            force: if session {
                Vec::new()
            } else {
                self.force.iter().cloned().collect()
            },
        })
    }

    fn save(&self, config_dir: &Path) -> Result<String, String> {
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
                przmterm_session::write_defaults(config_dir, &settings)
                    .map_err(|e| e.to_string())?;
                Ok("global settings".into())
            }
        }
    }

    /// Draws the window. `root` is the session tree, for the folder picker
    /// and the inherited values.
    pub fn show(&mut self, ctx: &egui::Context, config_dir: &Path, root: &Folder) -> EditorResult {
        let mut result = EditorResult::Open;
        let mut open = true;
        let none = SessionSettings::default();
        let (inherited, source, forced): (SessionSettings, &str, Vec<Forced>) = match &self.target {
            Target::Session { trail, .. } => match root.find(trail) {
                Some(f) => (
                    f.settings.clone(),
                    if trail.is_empty() { "global" } else { "folder" },
                    f.forced.clone(),
                ),
                None => (none.clone(), "", Vec::new()),
            },
            Target::Folder { trail, .. } => match root.find(trail) {
                Some(f) => (
                    f.inherited.clone(),
                    if trail.len() <= 1 {
                        "global"
                    } else {
                        "parent folder"
                    },
                    f.inherited_forced.clone(),
                ),
                None => (none.clone(), "", Vec::new()),
            },
            Target::Global => (none.clone(), "", Vec::new()),
        };
        let protocol = self.protocol.or(inherited.protocol).unwrap_or_default();
        let cx = Context {
            inherited: &inherited,
            source,
            forced: &forced,
            can_force: !self.is_session(),
            protocol,
        };
        let mut force = std::mem::take(&mut self.force);

        egui::Window::new(self.title.clone())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(500.0)
            .show(ctx, |ui| {
                ui.weak(match self.target {
                    Target::Global => {
                        "Defaults for every session. Tick “force” to override folders and sessions."
                    }
                    Target::Folder { .. } => {
                        "Defaults for the sessions in this folder. Tick “force” to override them."
                    }
                    Target::Session { .. } => "Empty fields use the value shown in grey.",
                });
                ui.weak("Enter saves, Esc cancels.");
                ui.add_space(6.0);
                egui::Grid::new("editor-grid")
                    .num_columns(3)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        if self.target != Target::Global {
                            ui.label(if self.is_session() {
                                "Name"
                            } else {
                                "Folder name"
                            });
                            ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(280.0));
                            ui.label("");
                            ui.end_row();
                        }
                        if let Target::Session { trail, .. } = &mut self.target {
                            ui.label("Folder");
                            folder_picker(ui, root, trail);
                            ui.label("");
                            ui.end_row();
                        }

                        row(ui, &cx, &mut force, "Protocol", "protocol", |ui| {
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt("protocol")
                                    .width(250.0)
                                    .selected_text(shown(
                                        self.protocol.map(|p| p.to_string()),
                                        cx.hint("protocol"),
                                    ))
                                    .show_ui(ui, |ui| {
                                        for p in [Protocol::Ssh, Protocol::Telnet, Protocol::Serial]
                                        {
                                            ui.selectable_value(
                                                &mut self.protocol,
                                                Some(p),
                                                p.to_string(),
                                            );
                                        }
                                    });
                                reset(ui, &mut self.protocol, &cx.hint("protocol"));
                            });
                        });

                        let session = self.is_session();
                        if protocol == Protocol::Serial {
                            if session {
                                text_row(
                                    ui,
                                    &cx,
                                    &mut force,
                                    "Serial port",
                                    "serial_port",
                                    &mut self.serial_port,
                                );
                            }
                            text_row(
                                ui,
                                &cx,
                                &mut force,
                                "Baud rate",
                                "baud_rate",
                                &mut self.baud_rate,
                            );
                        } else {
                            if session {
                                text_row(ui, &cx, &mut force, "Host", "host", &mut self.host);
                            }
                            text_row(ui, &cx, &mut force, "Port", "port", &mut self.port);
                            text_row(
                                ui,
                                &cx,
                                &mut force,
                                "User name",
                                "username",
                                &mut self.username,
                            );
                            row(ui, &cx, &mut force, "Key file", "identity_file", |ui| {
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
                            });
                            row(
                                ui,
                                &cx,
                                &mut force,
                                "Save password",
                                "save_password",
                                |ui| {
                                    save_pick(
                                        ui,
                                        &mut self.save_password,
                                        cx.inherited.save_password,
                                        cx.note("save_password"),
                                    );
                                },
                            );
                        }

                        row(
                            ui,
                            &cx,
                            &mut force,
                            "Terminal type",
                            "terminal.term_type",
                            |ui| {
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt("term-type")
                                        .width(250.0)
                                        .selected_text(shown(
                                            self.term_type.clone(),
                                            cx.hint("terminal.term_type"),
                                        ))
                                        .show_ui(ui, |ui| {
                                            for t in TERM_TYPES {
                                                ui.selectable_value(
                                                    &mut self.term_type,
                                                    Some(t.to_string()),
                                                    t,
                                                );
                                            }
                                            if let Some(custom) = self.term_type.clone()
                                                && !TERM_TYPES.contains(&custom.as_str())
                                            {
                                                ui.selectable_value(
                                                    &mut self.term_type,
                                                    Some(custom.clone()),
                                                    custom,
                                                );
                                            }
                                        });
                                    reset(ui, &mut self.term_type, &cx.hint("terminal.term_type"));
                                });
                            },
                        );
                        text_row(
                            ui,
                            &cx,
                            &mut force,
                            "Scrollback lines",
                            "terminal.scrollback",
                            &mut self.scrollback,
                        );
                        row(ui, &cx, &mut force, "Log to file", "log.enabled", |ui| {
                            bool_pick(
                                ui,
                                &mut self.log,
                                cx.inherited.log.enabled.unwrap_or(false),
                                ["yes", "no"],
                                cx.note("log.enabled"),
                                &cx.hint("log.enabled"),
                            );
                        });
                        text_row(
                            ui,
                            &cx,
                            &mut force,
                            "Log file",
                            "log.path",
                            &mut self.log_path,
                        );
                    });

                if let Some(err) = &self.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(8.0);
                // Enter saves and Escape cancels, unless a list is open.
                let popup = egui::Popup::is_any_open(ui.ctx());
                let (enter, escape) = ui.input(|i| {
                    (
                        i.key_pressed(egui::Key::Enter),
                        i.key_pressed(egui::Key::Escape),
                    )
                });
                if escape && !popup {
                    result = EditorResult::Cancel;
                }
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() || (enter && !popup) {
                        self.force = force.clone();
                        match self.save(config_dir) {
                            Ok(name) => result = EditorResult::Saved(name),
                            Err(e) => self.error = Some(e),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        result = EditorResult::Cancel;
                    }
                });
            });
        self.force = force;
        if !open {
            result = EditorResult::Cancel;
        }
        result
    }
}

/// One settings row: label, the widget (or the locked value when forced from
/// above) and, for global and folder settings, the “force” box.
fn row(
    ui: &mut egui::Ui,
    cx: &Context,
    force: &mut BTreeSet<String>,
    label: &str,
    key: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    ui.label(label);
    match cx.locked(key) {
        Some(locked) => {
            ui.weak(locked)
                .on_hover_text("Set and forced in a parent folder or the global settings");
            ui.label("");
        }
        None => {
            add(ui);
            if cx.can_force {
                let mut on = force.contains(key);
                if ui
                    .checkbox(&mut on, "force")
                    .on_hover_text(
                        "Apply this value to every folder and session below, overriding theirs",
                    )
                    .changed()
                {
                    if on {
                        force.insert(key.to_string());
                    } else {
                        force.remove(key);
                    }
                }
            } else {
                ui.label("");
            }
        }
    }
    ui.end_row();
}

fn text_row(
    ui: &mut egui::Ui,
    cx: &Context,
    force: &mut BTreeSet<String>,
    label: &str,
    key: &str,
    value: &mut String,
) {
    let hint = cx.hint(key);
    row(ui, cx, force, label, key, |ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(280.0),
        );
    });
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
    fn sessions_folders_and_global_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join(przmterm_session::SESSIONS_DIR);
        przmterm_session::create_folder(&sessions, "Lab").unwrap();

        let mut e = Editor::new_session(&["Lab".to_string()]);
        e.name = "sw1".into();
        e.host = "10.0.0.1".into();
        e.username = "me".into();
        e.term_type = Some("vt220".into());
        e.save(tmp.path()).unwrap();
        assert!(sessions.join("Lab/sw1.toml").exists());

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let lab = tree.root.find(&["Lab".to_string()]).unwrap();
        let mut f = Editor::folder(lab);
        f.port = "2222".into();
        f.force.insert("port".into());
        f.name = "Lab2".into();
        f.host = "ignored for folders".into();
        f.save(tmp.path()).unwrap();
        let folder_toml = std::fs::read_to_string(sessions.join("Lab2/folder.toml")).unwrap();
        assert_eq!(folder_toml, "port = 2222\nforce = [\"port\"]\n");

        let mut g = Editor::global(tmp.path());
        g.username = "ops".into();
        g.force.insert("username".into());
        g.save(tmp.path()).unwrap();

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let sw1 = tree.root.all_sessions()[0];
        assert_eq!(
            sw1.display_target(),
            "ops@10.0.0.1:2222",
            "global and folder force win"
        );
        assert_eq!(sw1.term_type, "vt220");

        let mut e = Editor::edit(sw1);
        assert!(e.port.is_empty(), "inherited values are not copied in");
        assert!(e.force.is_empty());
        e.target = Target::Session {
            original: Some(sw1.path.clone()),
            trail: vec![],
        };
        e.save(tmp.path()).unwrap();
        assert!(sessions.join("sw1.toml").exists());
        assert!(
            !sessions.join("Lab2/sw1.toml").exists(),
            "moved to the top level"
        );

        let mut bad = Editor::new_session(&[]);
        bad.name = "x".into();
        bad.port = "abc".into();
        assert!(bad.save(tmp.path()).unwrap_err().contains("Port"));
    }

    #[test]
    fn tilde_shortens_home_paths() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(tilde(&home.join(".ssh/id_lab")), "~/.ssh/id_lab");
        assert_eq!(tilde(Path::new("/etc/x")), "/etc/x");
    }
}
