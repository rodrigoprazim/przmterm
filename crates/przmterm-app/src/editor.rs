//! Editor window for sessions and folders.
//!
//! Only the values set in the file itself are written. Every field shows the
//! value it inherits (from the folder, `defaults.toml` or the built-in
//! default) so it is clear what an empty field means.

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

#[derive(Clone, PartialEq, Eq)]
enum Target {
    /// A session file; `original` is `None` for a new one.
    Session {
        original: Option<PathBuf>,
        trail: Vec<String>,
    },
    /// A folder's own settings (`folder.toml`), with rename.
    Folder { dir: PathBuf, trail: Vec<String> },
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
            error: None,
        }
    }

    fn is_folder(&self) -> bool {
        matches!(self.target, Target::Folder { .. })
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
        Ok(SessionSettings {
            protocol: self.protocol,
            host: if self.is_folder() {
                None
            } else {
                opt(&self.host)
            },
            port: number("Port", &self.port)?,
            username: opt(&self.username),
            identity_file: opt(&self.identity_file),
            save_password: self.save_password,
            serial_port: opt(&self.serial_port),
            baud_rate: number("Baud rate", &self.baud_rate)?,
            terminal: TerminalSettings {
                term_type: self.term_type.clone(),
                scrollback: number("Scrollback", &self.scrollback)?,
            },
            log: LogSettings {
                enabled: self.log,
                path: opt(&self.log_path),
            },
        })
    }

    fn save(&self, config_dir: &Path) -> Result<String, String> {
        przmterm_session::validate_name(&self.name)?;
        let settings = self.settings()?;
        let name = self.name.trim().to_string();
        match &self.target {
            Target::Session { original, trail } => {
                let path =
                    przmterm_session::write_session(config_dir, &trail.join("/"), &name, &settings)
                        .map_err(|e| e.to_string())?;
                if let Some(original) = original
                    && *original != path
                {
                    std::fs::remove_file(original)
                        .map_err(|e| format!("saved, but could not remove the old file: {e}"))?;
                }
            }
            Target::Folder { dir, .. } => {
                przmterm_session::write_folder_settings(dir, &settings)
                    .map_err(|e| e.to_string())?;
                przmterm_session::rename_folder(dir, &name).map_err(|e| e.to_string())?;
            }
        }
        Ok(name)
    }

    /// Draws the window. `root` is the session tree, for the folder picker
    /// and the inherited values.
    pub fn show(&mut self, ctx: &egui::Context, config_dir: &Path, root: &Folder) -> EditorResult {
        let mut result = EditorResult::Open;
        let mut open = true;
        // What an empty field falls back to.
        let (inherited, source) = match &self.target {
            Target::Session { trail, .. } => (
                root.find(trail)
                    .map(|f| f.settings.clone())
                    .unwrap_or_default(),
                if trail.is_empty() {
                    "defaults"
                } else {
                    "folder"
                },
            ),
            Target::Folder { trail, .. } => (
                root.find(trail)
                    .map(|f| f.inherited.clone())
                    .unwrap_or_default(),
                if trail.len() <= 1 {
                    "defaults"
                } else {
                    "parent folder"
                },
            ),
        };
        let from = |value: Option<String>, builtin: &str| match value {
            Some(v) => format!("from {source}: {v}"),
            None => format!("default: {builtin}"),
        };
        let effective_protocol = self.protocol.or(inherited.protocol).unwrap_or_default();

        egui::Window::new(self.title.clone())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.weak("Empty fields inherit the value shown in grey.");
                ui.add_space(6.0);
                egui::Grid::new("editor-grid")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(if self.is_folder() {
                            "Folder name"
                        } else {
                            "Name"
                        });
                        ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(280.0));
                        ui.end_row();

                        if let Target::Session { trail, .. } = &mut self.target {
                            ui.label("Folder");
                            folder_picker(ui, root, trail);
                            ui.end_row();
                        }

                        ui.label("Protocol");
                        let inherit_label = from(inherited.protocol.map(|p| p.to_string()), "ssh");
                        egui::ComboBox::from_id_salt("protocol")
                            .width(280.0)
                            .selected_text(match self.protocol {
                                None => inherit_label.clone(),
                                Some(p) => p.to_string(),
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.protocol, None, inherit_label);
                                for p in [Protocol::Ssh, Protocol::Telnet, Protocol::Serial] {
                                    ui.selectable_value(&mut self.protocol, Some(p), p.to_string());
                                }
                            });
                        ui.end_row();

                        if effective_protocol == Protocol::Serial {
                            if !self.is_folder() {
                                field(
                                    ui,
                                    "Serial port",
                                    &mut self.serial_port,
                                    &from(inherited.serial_port.clone(), "/dev/ttyUSB0 or COM3"),
                                );
                            }
                            field(
                                ui,
                                "Baud rate",
                                &mut self.baud_rate,
                                &from(inherited.baud_rate.map(|b| b.to_string()), "9600"),
                            );
                        } else {
                            if !self.is_folder() {
                                field(ui, "Host", &mut self.host, "10.0.0.1 or router.example.com");
                            }
                            let port = effective_protocol.default_port().unwrap_or(22).to_string();
                            field(
                                ui,
                                "Port",
                                &mut self.port,
                                &from(inherited.port.map(|p| p.to_string()), &port),
                            );
                            field(
                                ui,
                                "User name",
                                &mut self.username,
                                &from(inherited.username.clone(), "asked at login"),
                            );
                            ui.label("Key file");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.identity_file)
                                        .hint_text(from(
                                            inherited.identity_file.clone(),
                                            "agent and ~/.ssh keys",
                                        ))
                                        .desired_width(200.0),
                                );
                                if ui.button("Browse…").clicked()
                                    && let Some(path) = pick_key_file()
                                {
                                    self.identity_file = tilde(&path);
                                }
                            });
                            ui.end_row();
                            choice(
                                ui,
                                "Save password",
                                &mut self.save_password,
                                &from(inherited.save_password.map(yes_no), "ask"),
                                [("always", true), ("never", false)],
                            );
                        }

                        ui.label("Terminal type");
                        let inherit_label =
                            from(inherited.terminal.term_type.clone(), "xterm-256color");
                        egui::ComboBox::from_id_salt("term-type")
                            .width(280.0)
                            .selected_text(
                                self.term_type
                                    .clone()
                                    .unwrap_or_else(|| inherit_label.clone()),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.term_type, None, inherit_label);
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
                        ui.end_row();

                        field(
                            ui,
                            "Scrollback lines",
                            &mut self.scrollback,
                            &from(
                                inherited.terminal.scrollback.map(|n| n.to_string()),
                                "10000",
                            ),
                        );
                        choice(
                            ui,
                            "Log to file",
                            &mut self.log,
                            &from(inherited.log.enabled.map(yes_no), "no"),
                            [("yes", true), ("no", false)],
                        );
                        field(
                            ui,
                            "Log file",
                            &mut self.log_path,
                            &from(inherited.log.path.clone(), "%S-%Y%M%D-%h%m%s.log"),
                        );
                    });

                if let Some(err) = &self.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
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
        if !open {
            result = EditorResult::Cancel;
        }
        result
    }
}

fn yes_no(b: bool) -> String {
    if b { "yes" } else { "no" }.to_string()
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
            let open_by_default = trail.starts_with(&f.trail);
            egui::CollapsingHeader::new(format!("📁 {}", f.name))
                .id_salt(&f.dir)
                .default_open(open_by_default)
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

fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(280.0),
    );
    ui.end_row();
}

/// An inherit / yes / no choice. `inherit` describes what inheriting gives.
fn choice(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Option<bool>,
    inherit: &str,
    options: [(&str, bool); 2],
) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.selectable_value(value, None, inherit)
            .on_hover_text("Use the inherited value");
        for (text, v) in options {
            ui.selectable_value(value, Some(v), text);
        }
    });
    ui.end_row();
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
        e.save(tmp.path()).unwrap();
        assert!(sessions.join("Lab/sw1.toml").exists());

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let lab = tree.root.find(&["Lab".to_string()]).unwrap();
        let mut f = Editor::folder(lab);
        f.port = "2222".into();
        f.name = "Lab2".into();
        f.host = "ignored for folders".into();
        f.save(tmp.path()).unwrap();
        let folder_toml = std::fs::read_to_string(sessions.join("Lab2/folder.toml")).unwrap();
        assert_eq!(folder_toml, "port = 2222\n");

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let sw1 = tree.root.all_sessions()[0];
        assert_eq!(sw1.display_target(), "10.0.0.1:2222");
        assert_eq!(sw1.term_type, "vt220");

        let mut e = Editor::edit(sw1);
        assert!(e.port.is_empty(), "inherited values are not copied in");
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
