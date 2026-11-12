//! The settings files with problems: which file, what is wrong and what
//! can be done about it, so a problem does not stay a count in the corner.

use std::path::{Path, PathBuf};

use eframe::egui;
use przmterm_session::SessionError;

use crate::icons::{self, Icon};

/// What the user chose to do about a problem.
pub enum Fix {
    /// Open the session dialog on this session file.
    Edit(PathBuf),
    /// Write the file again without the settings this version does not know.
    DropUnknown(PathBuf),
    /// Delete the file.
    Delete(PathBuf),
    /// Show the file in its folder.
    Reveal(PathBuf),
    /// Open the file in a text editor.
    OpenText(PathBuf),
    /// Read the settings again (after fixing a file by hand).
    Reload,
}

#[derive(Default)]
pub struct Problems {
    pub open: bool,
    /// The file whose Delete was clicked once: a second click deletes it.
    confirm_delete: Option<PathBuf>,
}

/// Whether `path` is a session file (not a folder's settings) below
/// `config_dir`.
fn is_session_file(path: &Path, config_dir: &Path) -> bool {
    path.starts_with(config_dir.join(przmterm_session::SESSIONS_DIR))
        && path.extension().is_some_and(|e| e == "toml")
        && path
            .file_name()
            .is_some_and(|n| n != przmterm_session::FOLDER_FILE)
}

/// What the file is, for the list.
fn kind_of(path: &Path, config_dir: &Path) -> &'static str {
    if is_session_file(path, config_dir) {
        "Session"
    } else if path
        .file_name()
        .is_some_and(|n| n == przmterm_session::FOLDER_FILE)
    {
        "Folder settings"
    } else {
        "Settings"
    }
}

/// What to say next to a problem, so the user knows what the buttons do.
fn advice(error: &SessionError, session: bool) -> &'static str {
    match error {
        SessionError::Invalid { .. } if session => {
            "The session is not listed until this is fixed: Edit it to fill in what is missing."
        }
        SessionError::Invalid { .. } => "Open the file to fix it.",
        SessionError::Unknown { .. } => {
            "Written by a newer PrzmTerm or misspelt. The rest of the file is used; \
             Remove them to write the file without these settings."
        }
        SessionError::Parse { .. } => {
            "The file is not valid TOML and is not used: open it to fix the line named \
             above, or delete it."
        }
        SessionError::Io { .. } => "The file could not be read.",
    }
}

impl Problems {
    /// Draws the window; returns what was chosen.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        errors: &[SessionError],
        config_dir: &Path,
    ) -> Option<Fix> {
        if !self.open {
            return None;
        }
        let mut fix = None;
        let mut open = true;
        egui::Window::new(crate::theme::window_title(ctx, "Settings problems"))
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(620.0)
            .collapsible(false)
            .show(ctx, |ui| {
                if errors.is_empty() {
                    ui.label("No settings file has a problem now.");
                }
                egui::ScrollArea::vertical()
                    .max_height(ctx.content_rect().height() * 0.6)
                    .show(ui, |ui| {
                        for (i, error) in errors.iter().enumerate() {
                            if i > 0 {
                                ui.separator();
                            }
                            fix = fix.take().or(self.problem(ui, error, config_dir));
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.weak("Fixed a file by hand?");
                    if ui.button("Reload").clicked() {
                        fix = Some(Fix::Reload);
                    }
                });
            });
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open = false;
            self.confirm_delete = None;
        }
        fix
    }

    fn problem(
        &mut self,
        ui: &mut egui::Ui,
        error: &SessionError,
        config_dir: &Path,
    ) -> Option<Fix> {
        let path = error.path();
        let session = is_session_file(path, config_dir);
        let shown = path
            .strip_prefix(config_dir)
            .unwrap_or(path)
            .display()
            .to_string();
        let mut fix = None;
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
            icons::draw(
                ui.painter(),
                Icon::Warning,
                rect,
                ui.visuals().warn_fg_color,
            );
            ui.strong(kind_of(path, config_dir));
            ui.monospace(&shown);
        });
        match error {
            // Its line and a caret under the place: lined up in monospace.
            SessionError::Parse { .. } => {
                ui.monospace(error.reason());
            }
            SessionError::Invalid { reason, .. } => {
                ui.label(crate::editor::readable(reason));
            }
            _ => {
                ui.label(error.reason());
            }
        }
        ui.weak(advice(error, session));
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let button = |ui: &mut egui::Ui, icon: Icon, text: &str| {
                icons::icon_text_button(ui, icon, ui.visuals().text_color(), text, None, None)
            };
            if session
                && matches!(error, SessionError::Invalid { .. })
                && button(ui, Icon::Edit, "Edit…").clicked()
            {
                fix = Some(Fix::Edit(path.to_path_buf()));
            }
            if matches!(error, SessionError::Unknown { .. })
                && button(ui, Icon::Erase, "Remove them")
                    .on_hover_text("Write the file again without these settings")
                    .clicked()
            {
                fix = Some(Fix::DropUnknown(path.to_path_buf()));
            }
            if path.is_file() && button(ui, Icon::TextFile, "Open").clicked() {
                fix = Some(Fix::OpenText(path.to_path_buf()));
            }
            if button(ui, Icon::ShowFile, "Show in folder").clicked() {
                fix = Some(Fix::Reveal(path.to_path_buf()));
            }
            if session && path.is_file() {
                let sure = self.confirm_delete.as_deref() == Some(path);
                let text = if sure { "Delete: sure?" } else { "Delete" };
                if button(ui, Icon::Delete, text).clicked() {
                    if sure {
                        self.confirm_delete = None;
                        fix = Some(Fix::Delete(path.to_path_buf()));
                    } else {
                        self.confirm_delete = Some(path.to_path_buf());
                    }
                }
            }
        });
        fix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_session_files_from_others() {
        let config = Path::new("/c");
        assert!(is_session_file(
            Path::new("/c/sessions/Lab/r1.toml"),
            config
        ));
        assert!(!is_session_file(
            Path::new("/c/sessions/Lab/folder.toml"),
            config
        ));
        assert!(!is_session_file(Path::new("/c/defaults.toml"), config));
        assert_eq!(
            kind_of(Path::new("/c/sessions/Lab/folder.toml"), config),
            "Folder settings"
        );
    }
}
