//! The "Send File" / "Receive File" dialog for Xmodem, Ymodem and Zmodem.

use std::path::PathBuf;

use eframe::egui;
use przmterm_transport::modem::Kind;

/// What to start, once the dialog is answered.
pub enum Start {
    Send {
        kind: Kind,
        files: Vec<PathBuf>,
        /// Typed first to start the receiver (`rz`, `rb`, `rx NAME`).
        command: Option<String>,
    },
    Receive {
        kind: Kind,
        dir: PathBuf,
        name: String,
        /// Typed first to start the sender (`sz NAME`…).
        command: Option<String>,
    },
}

/// Where received files go unless chosen: the Downloads folder.
pub fn downloads() -> PathBuf {
    dirs::download_dir()
        .or_else(|| {
            dirs::home_dir()
                .map(|h| h.join("Downloads"))
                .filter(|d| d.is_dir())
        })
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Whether the output after a typed `rz`/`sz`/… says the command does not
/// exist or failed (shells in English and Portuguese, network equipment).
/// `text` is what came after the echo of the command.
pub fn start_failed(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "command not found",
        "comando não encontrado",
        "não encontrado",
        "not found",
        "no such file",
        "unknown command",
        "unrecognized command",
        "invalid input",
        "invalid command",
        "bad command",
        "error locates",
        "cannot open",
        "is not recognized",
    ]
    .iter()
    .any(|p| text.contains(p))
}

/// Why the other side did not start, and what to do.
pub fn not_started_message(tool: &str) -> String {
    format!(
        "`{tool}` did not start on the other side. On a Unix host install lrzsz \
         (dnf install lrzsz, or apt install lrzsz), or use Connect SFTP Session. \
         Network equipment rarely has it: use its own copy command (TFTP, FTP, SCP), \
         or Xmodem on the console port without \"Start the other side for me\"."
    )
}

pub struct TransferDialog {
    /// The tab whose active pane transfers.
    pub tab: usize,
    sending: bool,
    kind: Kind,
    files: Vec<PathBuf>,
    /// Receiving with Xmodem (no names): the file to create. With "type the
    /// command", the remote file to send.
    name: String,
    dir: PathBuf,
    /// Type the lrzsz command on the other side (Unix hosts).
    command: bool,
    error: Option<String>,
}

pub enum Answer {
    Open,
    Cancel,
    Start(Start),
}

impl TransferDialog {
    pub fn new(tab: usize, sending: bool, unix: bool) -> Self {
        Self {
            tab,
            sending,
            kind: if unix { Kind::Zmodem } else { Kind::Xmodem1k },
            files: Vec::new(),
            name: String::new(),
            dir: downloads(),
            command: unix,
            error: None,
        }
    }

    fn command(&self) -> Option<String> {
        if !self.command {
            return None;
        }
        let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
        Some(if self.sending {
            match self.kind {
                Kind::Xmodem | Kind::Xmodem1k => {
                    let name = self
                        .files
                        .first()
                        .and_then(|f| f.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    format!("rx {}", quote(&name))
                }
                Kind::Ymodem => "rb".into(),
                Kind::Zmodem => "rz".into(),
            }
        } else {
            let tool = match self.kind {
                Kind::Xmodem => "sx",
                Kind::Xmodem1k => "sx -k",
                Kind::Ymodem => "sb",
                Kind::Zmodem => "sz",
            };
            format!("{tool} {}", quote(self.name.trim()))
        })
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Answer {
        let mut answer = Answer::Open;
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        egui::Window::new(if self.sending {
            "Send files"
        } else {
            "Receive files"
        })
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("transfer")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Protocol");
                    ui.horizontal(|ui| {
                        for k in Kind::ALL {
                            ui.selectable_value(&mut self.kind, k, k.label());
                        }
                    });
                    ui.end_row();
                    if self.sending {
                        ui.label("Files");
                        ui.horizontal(|ui| {
                            let shown = match self.files.len() {
                                0 => "none".to_string(),
                                1 => self.files[0]
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                                n => format!("{n} files"),
                            };
                            ui.label(shown);
                            if ui.button("Choose…").clicked()
                                && let Some(files) = rfd::FileDialog::new()
                                    .set_title("Files to send")
                                    .pick_files()
                            {
                                self.files = files;
                                self.error = None;
                            }
                        });
                        ui.end_row();
                        if !self.kind.sends_names() && self.files.len() > 1 {
                            ui.label("");
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "Xmodem sends one file: only the first.",
                            );
                            ui.end_row();
                        }
                    } else {
                        ui.label("Save to");
                        ui.horizontal(|ui| {
                            let folder = self
                                .dir
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "folder".into());
                            ui.label(folder);
                            if ui.button("Choose…").clicked()
                                && let Some(dir) = rfd::FileDialog::new()
                                    .set_title("Save received files here")
                                    .set_directory(&self.dir)
                                    .pick_folder()
                            {
                                self.dir = dir;
                            }
                        });
                        ui.end_row();
                        if !self.kind.sends_names() || self.command {
                            ui.label(if self.command {
                                "Remote file"
                            } else {
                                "Save as"
                            });
                            ui.add(
                                egui::TextEdit::singleline(&mut self.name)
                                    .hint_text("firmware.bin")
                                    .desired_width(240.0),
                            );
                            ui.end_row();
                        }
                    }
                    ui.label("");
                    ui.checkbox(&mut self.command, "Start the other side for me")
                        .on_hover_text(
                            "On a Unix host with lrzsz installed, type its command (rz, sz NAME, \
                             rb, sb, rx, sx) first. Leave it off for a device that is already \
                             waiting (a bootloader, for example). Routers and switches \
                             usually have no lrzsz.",
                        );
                    ui.end_row();
                    if let Some(cmd) = self.command() {
                        ui.label("");
                        ui.weak(format!("Types: {cmd}"));
                        ui.end_row();
                    }
                });
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() || escape {
                        answer = Answer::Cancel;
                    }
                    if ui.button("Start").clicked() {
                        if self.sending && self.files.is_empty() {
                            self.error = Some("Choose the files to send.".into());
                        } else if !self.sending
                            && (self.command || !self.kind.sends_names())
                            && self.name.trim().is_empty()
                        {
                            self.error = Some("Give the file's name.".into());
                        } else {
                            answer = Answer::Start(if self.sending {
                                Start::Send {
                                    kind: self.kind,
                                    files: self.files.clone(),
                                    command: self.command(),
                                }
                            } else {
                                Start::Receive {
                                    kind: self.kind,
                                    dir: self.dir.clone(),
                                    name: self.name.trim().to_string(),
                                    command: self.command(),
                                }
                            });
                        }
                    }
                });
            });
        });
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_starts_are_recognised() {
        assert!(start_failed("-bash: rz: command not found\r\n"));
        assert!(start_failed("-bash: rz: comando não encontrado"));
        assert!(start_failed(
            "  % Unknown command, the error locates at '^'"
        ));
        assert!(start_failed("% Invalid input detected at '^' marker."));
        assert!(start_failed("sz: cannot open x: No such file or directory"));
        assert!(!start_failed("rz waiting to receive."));
        assert!(!start_failed("**\u{18}B00000000000000"));
    }

    #[test]
    fn commands_for_the_other_side() {
        let mut d = TransferDialog::new(0, true, true);
        assert_eq!(d.command().as_deref(), Some("rz"));
        d.kind = Kind::Xmodem;
        d.files = vec![PathBuf::from("/tmp/it's.bin")];
        assert_eq!(d.command().as_deref(), Some(r"rx 'it'\''s.bin'"));
        let mut d = TransferDialog::new(0, false, true);
        d.name = "fw.bin".into();
        assert_eq!(d.command().as_deref(), Some("sz 'fw.bin'"));
        d.command = false;
        assert_eq!(d.command(), None);
        let d = TransferDialog::new(0, true, false);
        assert_eq!(d.kind, Kind::Xmodem1k, "devices: Xmodem-1K by default");
    }
}
