//! The "File transfer" dialog: Zmodem, Ymodem and Xmodem through the
//! terminal, and SCP on a channel of its own (SSH).

use std::path::PathBuf;

use eframe::egui;
use przmterm_transport::modem::Kind;
use przmterm_transport::scp;
use zeroize::Zeroizing;

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
    /// SCP on a channel of the same SSH connection.
    Scp(scp::Request),
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
         (dnf install lrzsz, or apt install lrzsz), or use File Browser › SFTP. \
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
    /// The session is SSH, so SCP is offered.
    ssh: bool,
    /// SCP chosen instead of `kind`.
    scp: bool,
    /// SCP: the remote folder to send into, or the file to fetch.
    remote: String,
    /// SCP: the shell its commands run in (`sudo su -`…); empty: the login
    /// shell.
    shell: String,
    /// For the shell's password prompt.
    password: Zeroizing<String>,
    error: Option<String>,
}

pub enum Answer {
    Open,
    Cancel,
    Start(Start),
}

impl TransferDialog {
    pub fn new(tab: usize, sending: bool, unix: bool, ssh: bool, shell: Option<String>) -> Self {
        Self {
            tab,
            sending,
            kind: Kind::Zmodem,
            files: Vec::new(),
            name: String::new(),
            dir: downloads(),
            command: unix,
            ssh,
            scp: false,
            remote: String::new(),
            shell: shell.unwrap_or_default(),
            password: Zeroizing::new(String::new()),
            error: None,
        }
    }

    fn protocol_label(&self) -> &'static str {
        if self.scp { "SCP" } else { self.kind.label() }
    }

    /// The SCP request, or what is missing.
    fn scp_request(&self) -> Result<scp::Request, String> {
        let direction = if self.sending {
            if self.files.is_empty() {
                return Err("Choose the files to send.".into());
            }
            scp::Direction::Upload {
                files: self.files.clone(),
                remote_dir: self.remote.trim().to_string(),
            }
        } else {
            if self.remote.trim().is_empty() {
                return Err("Give the remote file or folder.".into());
            }
            scp::Direction::Download {
                remote: self.remote.trim().to_string(),
                local_dir: self.dir.clone(),
            }
        };
        let shell = scp::shell_from(&self.shell);
        Ok(scp::Request {
            direction,
            password: if shell.is_none() {
                Zeroizing::new(String::new())
            } else {
                self.password.clone()
            },
            shell,
        })
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

    /// The rows for SCP, after the protocol.
    fn scp_rows(&mut self, ui: &mut egui::Ui) {
        if self.sending {
            self.files_row(ui);
            ui.label("Remote folder");
            ui.add(
                egui::TextEdit::singleline(&mut self.remote)
                    .hint_text("home folder")
                    .desired_width(240.0),
            );
            ui.end_row();
        } else {
            ui.label("Remote file");
            ui.add(
                egui::TextEdit::singleline(&mut self.remote)
                    .hint_text("/var/log/messages, or a folder")
                    .desired_width(240.0),
            );
            ui.end_row();
            self.save_to_row(ui);
        }
        ui.label("Shell").on_hover_text(
            "Where scp runs on the other side: Default is the login shell; \
             sudo su - or su - work as root",
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.shell)
                    .hint_text("Default")
                    .desired_width(160.0),
            );
            ui.menu_button("▾", |ui| {
                for shell in scp::SHELLS {
                    let pick = if shell == "Default" { "" } else { shell };
                    if ui.selectable_label(self.shell == pick, shell).clicked() {
                        self.shell = pick.to_string();
                        ui.close();
                    }
                }
            });
        });
        ui.end_row();
        let shell = scp::shell_from(&self.shell);
        if shell.is_some() {
            ui.label("Password");
            ui.add(
                egui::TextEdit::singleline(&mut *self.password)
                    .password(true)
                    .hint_text("if the shell asks for one")
                    .desired_width(240.0),
            );
            ui.end_row();
        }
        ui.label("");
        ui.weak(match shell {
            None => "Runs scp on the other side, beside the terminal.".to_string(),
            Some(s) => format!("Runs scp in {s}, beside the terminal."),
        });
        ui.end_row();
    }

    fn files_row(&mut self, ui: &mut egui::Ui) {
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
    }

    fn save_to_row(&mut self, ui: &mut egui::Ui) {
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
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Answer {
        let mut answer = Answer::Open;
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        egui::Window::new("File transfer")
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .collapsible(false)
            .resizable(false)
            .min_width(420.0)
            .show(ctx, |ui| {
                // Send and Receive as tabs, half the row each.
                ui.horizontal(|ui| {
                    let gap = ui.spacing().item_spacing.x;
                    let size = egui::vec2(
                        (ui.available_width() - gap) / 2.0,
                        ui.spacing().interact_size.y + 4.0,
                    );
                    for (sending, text) in [(true, "Send"), (false, "Receive")] {
                        let tab = egui::Button::selectable(self.sending == sending, text);
                        if ui.add_sized(size, tab).clicked() {
                            self.sending = sending;
                            self.error = None;
                        }
                    }
                });
                ui.separator();
                egui::Grid::new("transfer")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Protocol");
                        egui::ComboBox::from_id_salt("transfer-protocol")
                            .width(160.0)
                            .selected_text(self.protocol_label())
                            .show_ui(ui, |ui| {
                                for k in Kind::ALL {
                                    if ui
                                        .selectable_label(!self.scp && self.kind == k, k.label())
                                        .clicked()
                                    {
                                        self.kind = k;
                                        self.scp = false;
                                    }
                                    // SCP right after Zmodem, for SSH sessions.
                                    if k == Kind::Zmodem
                                        && self.ssh
                                        && ui
                                            .selectable_label(self.scp, "SCP")
                                            .on_hover_text(
                                                "On a channel of its own: the terminal stays \
                                                 free, and it can run as root (sudo or su)",
                                            )
                                            .clicked()
                                    {
                                        self.scp = true;
                                    }
                                }
                            });
                        ui.end_row();
                        if self.scp {
                            self.scp_rows(ui);
                            return;
                        }
                        if self.sending {
                            self.files_row(ui);
                            if !self.kind.sends_names() && self.files.len() > 1 {
                                ui.label("");
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    "Xmodem sends one file: only the first.",
                                );
                                ui.end_row();
                            }
                        } else {
                            self.save_to_row(ui);
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
                        if ui.button("Start").clicked() {
                            if self.scp {
                                match self.scp_request() {
                                    Ok(request) => answer = Answer::Start(Start::Scp(request)),
                                    Err(e) => self.error = Some(e),
                                }
                            } else if self.sending && self.files.is_empty() {
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
                        if ui.button("Cancel").clicked() || escape {
                            answer = Answer::Cancel;
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
        let mut d = TransferDialog::new(0, true, true, true, None);
        assert_eq!(d.command().as_deref(), Some("rz"));
        d.kind = Kind::Xmodem;
        d.files = vec![PathBuf::from("/tmp/it's.bin")];
        assert_eq!(d.command().as_deref(), Some(r"rx 'it'\''s.bin'"));
        let mut d = TransferDialog::new(0, false, true, true, None);
        d.name = "fw.bin".into();
        assert_eq!(d.command().as_deref(), Some("sz 'fw.bin'"));
        d.command = false;
        assert_eq!(d.command(), None);
        let d = TransferDialog::new(0, true, false, false, None);
        assert_eq!(d.kind, Kind::Zmodem, "Zmodem first");
        assert!(!d.command, "devices: nothing typed");
    }
}
