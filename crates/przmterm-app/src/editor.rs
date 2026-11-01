//! Session editor window: create, edit and duplicate session files.
//!
//! Only the values set in the file itself are shown and written; empty fields
//! inherit from the folder and `defaults.toml`.

use std::path::{Path, PathBuf};

use eframe::egui;
use przmterm_core::Protocol;
use przmterm_session::{LogSettings, Session, SessionSettings, TerminalSettings};

/// A three-way choice: inherit, yes, no.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tri {
    Inherit,
    Yes,
    No,
}

impl From<Option<bool>> for Tri {
    fn from(v: Option<bool>) -> Self {
        match v {
            None => Tri::Inherit,
            Some(true) => Tri::Yes,
            Some(false) => Tri::No,
        }
    }
}

impl From<Tri> for Option<bool> {
    fn from(t: Tri) -> Self {
        match t {
            Tri::Inherit => None,
            Tri::Yes => Some(true),
            Tri::No => Some(false),
        }
    }
}

pub struct Editor {
    /// File being edited; `None` for a new session.
    original: Option<PathBuf>,
    title: String,
    name: String,
    folder: String,
    protocol: Option<Protocol>,
    host: String,
    port: String,
    username: String,
    identity_file: String,
    serial_port: String,
    baud_rate: String,
    term_type: String,
    scrollback: String,
    log: Tri,
    log_path: String,
    save_password: Tri,
    error: Option<String>,
}

/// What the editor asks the app to do.
pub enum EditorResult {
    Open,
    Cancel,
    Saved(PathBuf),
}

fn text(v: &Option<impl ToString>) -> String {
    v.as_ref().map(|x| x.to_string()).unwrap_or_default()
}

fn opt(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn folder_of(session: &Session) -> String {
    session.folder.join("/")
}

impl Editor {
    pub fn new_session(folder: &str) -> Self {
        Self::from_settings(None, "New session", "", folder, &SessionSettings::default())
    }

    pub fn edit(session: &Session) -> Self {
        let settings = przmterm_session::read_session_settings(&session.path).unwrap_or_default();
        Self::from_settings(
            Some(session.path.clone()),
            &format!("Edit {}", session.name),
            &session.name,
            &folder_of(session),
            &settings,
        )
    }

    pub fn duplicate(session: &Session) -> Self {
        let settings = przmterm_session::read_session_settings(&session.path).unwrap_or_default();
        Self::from_settings(
            None,
            &format!("Duplicate {}", session.name),
            &format!("{}-copy", session.name),
            &folder_of(session),
            &settings,
        )
    }

    fn from_settings(
        original: Option<PathBuf>,
        title: &str,
        name: &str,
        folder: &str,
        s: &SessionSettings,
    ) -> Self {
        Self {
            original,
            title: title.to_string(),
            name: name.to_string(),
            folder: folder.to_string(),
            protocol: s.protocol,
            host: text(&s.host),
            port: text(&s.port),
            username: text(&s.username),
            identity_file: text(&s.identity_file),
            serial_port: text(&s.serial_port),
            baud_rate: text(&s.baud_rate),
            term_type: text(&s.terminal.term_type),
            scrollback: text(&s.terminal.scrollback),
            log: s.log.enabled.into(),
            log_path: text(&s.log.path),
            save_password: s.save_password.into(),
            error: None,
        }
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
            host: opt(&self.host),
            port: number("Port", &self.port)?,
            username: opt(&self.username),
            identity_file: opt(&self.identity_file),
            save_password: self.save_password.into(),
            serial_port: opt(&self.serial_port),
            baud_rate: number("Baud rate", &self.baud_rate)?,
            terminal: TerminalSettings {
                term_type: opt(&self.term_type),
                scrollback: number("Scrollback", &self.scrollback)?,
            },
            log: LogSettings {
                enabled: self.log.into(),
                path: opt(&self.log_path),
            },
        })
    }

    fn save(&self, config_dir: &Path) -> Result<PathBuf, String> {
        przmterm_session::validate_name(&self.name)?;
        let settings = self.settings()?;
        let path = przmterm_session::write_session(config_dir, &self.folder, &self.name, &settings)
            .map_err(|e| e.to_string())?;
        if let Some(original) = &self.original
            && *original != path
        {
            std::fs::remove_file(original)
                .map_err(|e| format!("saved, but could not remove the old file: {e}"))?;
        }
        Ok(path)
    }

    /// Draws the window. `folders` are the existing folder paths.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        config_dir: &Path,
        folders: &[String],
    ) -> EditorResult {
        let mut result = EditorResult::Open;
        let mut open = true;
        egui::Window::new(self.title.clone())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.weak("Empty fields inherit from the folder and defaults.toml.");
                ui.add_space(6.0);
                egui::Grid::new("session-editor")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(260.0));
                        ui.end_row();

                        ui.label("Folder");
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("folder")
                                .selected_text(if self.folder.is_empty() {
                                    "(top level)"
                                } else {
                                    &self.folder
                                })
                                .show_ui(ui, |ui| {
                                    for f in folders {
                                        let label = if f.is_empty() { "(top level)" } else { f };
                                        ui.selectable_value(&mut self.folder, f.clone(), label);
                                    }
                                });
                            ui.add(
                                egui::TextEdit::singleline(&mut self.folder)
                                    .hint_text("or a new folder, e.g. Lab/Core")
                                    .desired_width(160.0),
                            );
                        });
                        ui.end_row();

                        ui.label("Protocol");
                        egui::ComboBox::from_id_salt("protocol")
                            .selected_text(match self.protocol {
                                None => "inherit".to_string(),
                                Some(p) => p.to_string(),
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.protocol, None, "inherit");
                                for p in [Protocol::Ssh, Protocol::Telnet, Protocol::Serial] {
                                    ui.selectable_value(&mut self.protocol, Some(p), p.to_string());
                                }
                            });
                        ui.end_row();

                        if self.protocol == Some(Protocol::Serial) {
                            field(
                                ui,
                                "Serial port",
                                &mut self.serial_port,
                                "/dev/ttyUSB0 or COM3",
                            );
                            field(ui, "Baud rate", &mut self.baud_rate, "9600");
                        } else {
                            field(ui, "Host", &mut self.host, "10.0.0.1 or router.example.com");
                            field(ui, "Port", &mut self.port, "22");
                            field(ui, "User name", &mut self.username, "asked when empty");
                            field(ui, "Key file", &mut self.identity_file, "~/.ssh/id_ed25519");
                            tri(ui, "Save password", &mut self.save_password, "ask");
                        }
                        field(ui, "Terminal type", &mut self.term_type, "xterm-256color");
                        field(ui, "Scrollback lines", &mut self.scrollback, "10000");
                        tri(ui, "Log", &mut self.log, "inherit");
                        field(ui, "Log file", &mut self.log_path, "%S-%Y%M%D-%h%m%s.log");
                    });

                if let Some(err) = &self.error {
                    ui.add_space(4.0);
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        match self.save(config_dir) {
                            Ok(path) => result = EditorResult::Saved(path),
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

fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(260.0),
    );
    ui.end_row();
}

fn tri(ui: &mut egui::Ui, label: &str, value: &mut Tri, inherit: &str) {
    ui.label(label);
    ui.horizontal(|ui| {
        ui.selectable_value(value, Tri::Inherit, inherit);
        ui.selectable_value(value, Tri::Yes, "yes");
        ui.selectable_value(value, Tri::No, "no");
    });
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_file_and_renames() {
        let tmp = tempfile::tempdir().unwrap();
        let mut e = Editor::new_session("Lab");
        e.name = "sw1".into();
        e.host = "10.0.0.1".into();
        e.port = "2222".into();
        e.log = Tri::Yes;
        let path = e.save(tmp.path()).unwrap();
        assert_eq!(path, tmp.path().join("sessions/Lab/sw1.toml"));

        let tree = przmterm_session::SessionTree::load(tmp.path());
        let session = tree.root.all_sessions()[0].clone();
        let mut e = Editor::edit(&session);
        assert_eq!((e.host.as_str(), e.port.as_str()), ("10.0.0.1", "2222"));
        assert!(e.username.is_empty(), "inherited values are not copied in");
        e.name = "sw1-renamed".into();
        let new_path = e.save(tmp.path()).unwrap();
        assert!(new_path.exists() && !path.exists());

        let mut bad = Editor::new_session("");
        bad.name = "x".into();
        bad.port = "abc".into();
        assert!(bad.save(tmp.path()).unwrap_err().contains("Port"));
    }
}
