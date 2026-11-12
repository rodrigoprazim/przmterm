//! The password vault window: create the vault, unlock it, and add, change
//! or delete its entries. Also the short "unlock the vault" prompt shown
//! when a session that uses the vault is opened.

use std::path::Path;
use std::sync::Arc;

use eframe::egui;
use przmterm_session::vault::{
    KdfCost, MIN_MASTER_LEN, Vault, VaultEntry, VaultError, files_using, rename_references,
    vault_path,
};
use zeroize::Zeroize;

/// What happened in the vault windows this frame.
#[derive(Debug, PartialEq, Eq)]
pub enum VaultEvent {
    None,
    /// The prompt (or the window) unlocked the vault.
    Unlocked,
    /// The prompt was dismissed without unlocking.
    Cancelled,
}

struct EntryEdit {
    /// `None` for a new entry.
    old_name: Option<String>,
    entry: VaultEntry,
    show: bool,
    error: Option<String>,
    focused: bool,
}

struct ChangeMaster {
    current: String,
    new: String,
    again: String,
    error: Option<String>,
}

impl Drop for ChangeMaster {
    fn drop(&mut self) {
        self.current.zeroize();
        self.new.zeroize();
        self.again.zeroize();
    }
}

pub struct VaultUi {
    vault: Option<Vault>,
    /// The entries, shared with the panes for logging in.
    snapshot: Option<Arc<Vec<VaultEntry>>>,
    /// The vault window is open.
    pub window: bool,
    /// The unlock prompt is open, saying why.
    prompt: Option<String>,
    /// The prompt asks for the master password even if the vault is open:
    /// to confirm a change, not to unlock.
    confirm: bool,
    master: String,
    again: String,
    error: Option<String>,
    /// The master password field should take the focus.
    focus: bool,
    edit: Option<EntryEdit>,
    /// An entry waiting for "really delete?", and how many files use it.
    delete: Option<(String, usize)>,
    change: Option<ChangeMaster>,
    /// A message for the status bar.
    pub notice: Option<String>,
}

impl VaultUi {
    pub fn new() -> Self {
        Self {
            vault: None,
            snapshot: None,
            window: false,
            prompt: None,
            confirm: false,
            master: String::new(),
            again: String::new(),
            error: None,
            focus: true,
            edit: None,
            delete: None,
            change: None,
            notice: None,
        }
    }

    /// The open vault's entries; `None` while locked.
    pub fn credentials(&self) -> Option<Arc<Vec<VaultEntry>>> {
        self.snapshot.clone()
    }

    pub fn is_unlocked(&self) -> bool {
        self.vault.is_some()
    }

    /// Entry names, while unlocked.
    pub fn names(&self) -> Option<Vec<String>> {
        self.vault.as_ref().map(Vault::names)
    }

    pub fn lock(&mut self) {
        self.vault = None;
        self.snapshot = None;
        self.edit = None;
        self.delete = None;
        self.change = None;
        self.clear_fields();
    }

    /// Opens the unlock prompt; `why` says which session needs it.
    pub fn ask_unlock(&mut self, why: String) {
        self.prompt = Some(why);
        self.confirm = false;
        self.clear_fields();
    }

    /// Asks for the master password even when the vault is open, to
    /// confirm a change; `why` says which. Answered like the unlock prompt
    /// ([`VaultEvent::Unlocked`] or [`VaultEvent::Cancelled`]).
    pub fn ask_master(&mut self, why: String) {
        self.prompt = Some(why);
        self.confirm = true;
        self.clear_fields();
    }

    pub fn open_window(&mut self) {
        self.window = true;
        self.clear_fields();
    }

    pub fn modal(&self) -> bool {
        self.window || self.prompt.is_some()
    }

    fn clear_fields(&mut self) {
        self.master.zeroize();
        self.again.zeroize();
        self.error = None;
        self.focus = true;
    }

    fn opened(&mut self, vault: Vault) {
        self.snapshot = Some(Arc::new(vault.entries.clone()));
        self.vault = Some(vault);
        self.clear_fields();
    }

    fn refresh(&mut self) {
        self.snapshot = self.vault.as_ref().map(|v| Arc::new(v.entries.clone()));
    }

    fn unlock(&mut self, config_dir: &Path) -> bool {
        match Vault::open(&vault_path(config_dir), &self.master) {
            Ok(vault) => {
                self.opened(vault);
                true
            }
            Err(e) => {
                self.error = Some(match e {
                    VaultError::WrongPassword => "Wrong master password.".to_string(),
                    e => e.to_string(),
                });
                self.master.zeroize();
                self.focus = true;
                false
            }
        }
    }

    /// The master password field, with the focus when it should have it.
    fn master_field(&mut self, ui: &mut egui::Ui, hint: &str, width: f32) -> egui::Response {
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.master)
                .password(true)
                .hint_text(hint)
                .desired_width(width),
        );
        // Asked until it sticks: the menu that opened the window may still
        // hold the focus on the first frame.
        if self.focus {
            if response.has_focus() {
                self.focus = false;
            } else {
                response.request_focus();
            }
        }
        response
    }

    /// Draws the prompt and the window.
    pub fn show(&mut self, ctx: &egui::Context, config_dir: &Path) -> VaultEvent {
        let mut event = VaultEvent::None;
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        let center = ctx.content_rect().center();

        if let Some(why) = self.prompt.clone() {
            let mut done = None; // Some(true) unlock, Some(false) cancel
            let (title, go) = if self.confirm {
                ("Master password", "Continue")
            } else {
                ("Unlock the vault", "Unlock")
            };
            egui::Window::new(title)
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(center)
                .collapsible(false)
                .auto_sized()
                .show(ctx, |ui| {
                    ui.set_width(crate::theme::DIALOG_WIDTH);
                    ui.label(&why);
                    let field = self.master_field(ui, "Master password", f32::INFINITY);
                    if field.lost_focus() && enter {
                        done = Some(true);
                    }
                    if let Some(err) = &self.error {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                    }
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(go).clicked() {
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
                    if self.unlock(config_dir) {
                        self.prompt = None;
                        event = VaultEvent::Unlocked;
                    }
                }
                Some(false) => {
                    self.prompt = None;
                    self.clear_fields();
                    event = VaultEvent::Cancelled;
                }
                None => {}
            }
            return event;
        }

        if !self.window {
            return event;
        }
        let mut open = true;
        let exists = vault_path(config_dir).is_file();
        // Sub-dialogs take Enter and Esc for themselves.
        let busy = self.edit.is_some() || self.delete.is_some() || self.change.is_some();
        // Locked or not created yet, the window fits its few fields; open,
        // it is wide enough for the entries. (Two ids: egui keeps a
        // window's size.)
        let unlocked = self.vault.is_some();
        let mut window = egui::Window::new(crate::theme::window_title(ctx, "Password vault"))
            .id(egui::Id::unique(if unlocked {
                "vault-window"
            } else {
                "vault-window-locked"
            }))
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .open(&mut open)
            .collapsible(false)
            .resizable(false);
        window = if unlocked {
            window.default_width(560.0)
        } else {
            // As narrow as its field, not egui's default width.
            window.auto_sized()
        };
        window.show(ctx, |ui| {
            if self.vault.is_some() {
                self.entries_ui(ui, config_dir);
            } else if exists {
                ui.set_max_width(260.0);
                ui.label("The vault is locked.");
                let field = self.master_field(ui, "Master password", 260.0);
                let mut go = field.lost_focus() && enter;
                if let Some(err) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        go |= ui.button("Unlock").clicked();
                    });
                });
                if go && self.unlock(config_dir) {
                    event = VaultEvent::Unlocked;
                }
            } else {
                self.create_ui(ui, config_dir, enter);
            }
        });
        if !open || (escape && !busy && self.prompt.is_none()) {
            self.window = false;
            self.edit = None;
            self.delete = None;
            self.change = None;
            self.clear_fields();
        }
        self.entry_dialog(ctx, config_dir, enter, escape);
        self.delete_dialog(ctx, escape);
        self.change_dialog(ctx, enter, escape);
        event
    }

    fn create_ui(&mut self, ui: &mut egui::Ui, config_dir: &Path, enter: bool) {
        ui.label(
            "The vault keeps user names, passwords and key passphrases in one \
             encrypted file.",
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Choose a master password. It cannot be recovered: without it the \
                 vault cannot be opened.",
            )
            .color(ui.visuals().warn_fg_color),
        );
        ui.add_space(4.0);
        egui::Grid::new("vault-create")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Master password");
                self.master_field(ui, &format!("at least {MIN_MASTER_LEN} characters"), 260.0);
                ui.end_row();
                ui.label("Again");
                let again = ui.add(
                    egui::TextEdit::singleline(&mut self.again)
                        .password(true)
                        .desired_width(260.0),
                );
                ui.end_row();
                if again.lost_focus() && enter {
                    self.create(config_dir);
                }
            });
        if let Some(err) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        if ui.button("Create the vault").clicked() {
            self.create(config_dir);
        }
    }

    fn create(&mut self, config_dir: &Path) {
        if self.master != self.again {
            self.error = Some("The two passwords differ.".into());
            return;
        }
        match Vault::create(&vault_path(config_dir), &self.master, KdfCost::default()) {
            Ok(vault) => {
                self.notice = Some("Vault created.".into());
                self.opened(vault);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn entries_ui(&mut self, ui: &mut egui::Ui, config_dir: &Path) {
        let Some(vault) = &self.vault else { return };
        let mut entries: Vec<&VaultEntry> = vault.entries.iter().collect();
        entries.sort_by_key(|e| e.name.to_lowercase());
        if entries.is_empty() {
            ui.weak("No entries yet.");
        } else {
            // Columns share the whole width; the buttons sit at the right edge.
            let gap = ui.spacing().item_spacing.x;
            let width = ui.available_width();
            let share = [0.27, 0.25, 0.14, 0.16];
            let columns: Vec<f32> = share.iter().map(|s| (width - 4.0 * gap) * s).collect();
            let last = width - 4.0 * gap - columns.iter().sum::<f32>();
            let row_height = ui.spacing().interact_size.y;
            let cell = |ui: &mut egui::Ui, w: f32, add: &mut dyn FnMut(&mut egui::Ui)| {
                ui.allocate_ui_with_layout(
                    egui::vec2(w, row_height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(w);
                        add(ui);
                    },
                );
            };
            ui.horizontal(|ui| {
                for (i, title) in ["Entry", "User name", "Password", "Key passphrase"]
                    .into_iter()
                    .enumerate()
                {
                    cell(ui, columns[i], &mut |ui| {
                        ui.strong(title);
                    });
                }
            });
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (row, entry) in entries.into_iter().enumerate() {
                        let rect = egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::vec2(width, row_height),
                        );
                        if row % 2 == 0 {
                            ui.painter()
                                .rect_filled(rect, 2.0, ui.visuals().faint_bg_color);
                        }
                        ui.horizontal(|ui| {
                            let hidden = |s: &str| {
                                if s.is_empty() {
                                    "—"
                                } else {
                                    "••••••"
                                }
                            };
                            cell(ui, columns[0], &mut |ui| {
                                ui.label(&entry.name);
                            });
                            cell(ui, columns[1], &mut |ui| {
                                ui.label(&entry.username);
                            });
                            cell(ui, columns[2], &mut |ui| {
                                ui.label(hidden(&entry.password));
                            });
                            cell(ui, columns[3], &mut |ui| {
                                ui.label(hidden(&entry.passphrase));
                            });
                            ui.allocate_ui_with_layout(
                                egui::vec2(last, row_height),
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.set_min_width(last);
                                    if ui.button("Delete…").clicked() {
                                        let users = files_using(config_dir, &entry.name).len();
                                        self.delete = Some((entry.name.clone(), users));
                                    }
                                    if ui.button("Edit…").clicked() {
                                        self.edit = Some(EntryEdit {
                                            old_name: Some(entry.name.clone()),
                                            entry: entry.clone(),
                                            show: false,
                                            error: None,
                                            focused: false,
                                        });
                                    }
                                },
                            );
                        });
                    }
                });
        }
        ui.add_space(6.0);
        // Lock on the left; Change master password and New entry on the right.
        ui.horizontal(|ui| {
            if ui
                .button("Lock")
                .on_hover_text("Forget the entries until the master password is typed again")
                .clicked()
            {
                self.lock();
                self.notice = Some("Vault locked.".into());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("New entry…").clicked() {
                    self.edit = Some(EntryEdit {
                        old_name: None,
                        entry: VaultEntry::default(),
                        show: false,
                        error: None,
                        focused: false,
                    });
                }
                if ui.button("Change master password…").clicked() {
                    self.change = Some(ChangeMaster {
                        current: String::new(),
                        new: String::new(),
                        again: String::new(),
                        error: None,
                    });
                }
            });
        });
    }

    fn entry_dialog(&mut self, ctx: &egui::Context, config_dir: &Path, enter: bool, escape: bool) {
        let Some(edit) = &mut self.edit else { return };
        let mut done = None;
        egui::Window::new(if edit.old_name.is_some() {
            "Edit entry"
        } else {
            "New entry"
        })
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(420.0);
            egui::Grid::new("vault-entry")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    let name = ui.add(
                        egui::TextEdit::singleline(&mut edit.entry.name)
                            .hint_text("NOC, Lab admin…")
                            .desired_width(f32::INFINITY),
                    );
                    if !edit.focused {
                        name.request_focus();
                        edit.focused = true;
                    }
                    ui.end_row();
                    ui.label("User name");
                    ui.add(
                        egui::TextEdit::singleline(&mut edit.entry.username)
                            .hint_text("asked at login when empty")
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                    ui.label("Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut edit.entry.password)
                            .password(!edit.show)
                            .hint_text("asked at login when empty")
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                    ui.label("Key passphrase")
                        .on_hover_text("Opens encrypted private key files");
                    ui.add(
                        egui::TextEdit::singleline(&mut edit.entry.passphrase)
                            .password(!edit.show)
                            .hint_text("for encrypted key files")
                            .desired_width(f32::INFINITY),
                    );
                    ui.end_row();
                    ui.label("");
                    crate::theme::checkbox(ui, &mut edit.show, "Show password and passphrase");
                    ui.end_row();
                });
            if let Some(err) = &edit.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save").clicked() || enter {
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
                let Some(vault) = &mut self.vault else { return };
                let old = edit.old_name.clone();
                let new = edit.entry.name.trim().to_string();
                match vault.put(old.as_deref(), edit.entry.clone()) {
                    Ok(()) => {
                        self.notice = Some(match old {
                            Some(old) if old != new => {
                                match rename_references(config_dir, &old, &new) {
                                    Ok(0) => format!("Saved {new}."),
                                    Ok(n) => format!(
                                        "Saved {new}; {n} session or folder file(s) now use the new name."
                                    ),
                                    Err(e) => format!("Saved {new}, but: {e}"),
                                }
                            }
                            _ => format!("Saved {new}."),
                        });
                        self.edit = None;
                        self.refresh();
                    }
                    Err(e) => edit.error = Some(e.to_string()),
                }
            }
            Some(false) => self.edit = None,
            None => {}
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context, escape: bool) {
        let Some((name, users)) = self.delete.clone() else {
            return;
        };
        let mut done = None;
        egui::Window::new("Delete entry")
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(format!("Delete the vault entry {name}?"));
                if users > 0 {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!(
                            "{users} session or folder file(s) use it. They will fail to \
                             connect until they get another entry."
                        ),
                    );
                }
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Delete").clicked() {
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
                if let Some(vault) = &mut self.vault {
                    self.notice = Some(match vault.remove(&name) {
                        Ok(()) => format!("Deleted {name}."),
                        Err(e) => format!("Could not delete {name}: {e}"),
                    });
                }
                self.delete = None;
                self.refresh();
            }
            Some(false) => self.delete = None,
            None => {}
        }
    }

    fn change_dialog(&mut self, ctx: &egui::Context, enter: bool, escape: bool) {
        let Some(change) = &mut self.change else {
            return;
        };
        let mut done = None;
        egui::Window::new("Change master password")
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_width(crate::theme::DIALOG_WIDTH + 40.0);
                egui::Grid::new("vault-master")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        for (label, value, hint) in [
                            ("Current", &mut change.current, String::new()),
                            (
                                "New",
                                &mut change.new,
                                format!("at least {MIN_MASTER_LEN} characters"),
                            ),
                            ("Again", &mut change.again, String::new()),
                        ] {
                            ui.label(label);
                            ui.add(
                                egui::TextEdit::singleline(value)
                                    .password(true)
                                    .hint_text(hint)
                                    .desired_width(f32::INFINITY),
                            );
                            ui.end_row();
                        }
                    });
                if let Some(err) = &change.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Change").clicked() || enter {
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
                let Some(vault) = &mut self.vault else { return };
                if change.new != change.again {
                    change.error = Some("The two new passwords differ.".into());
                } else if Vault::open(vault.path(), &change.current).is_err() {
                    change.error = Some("The current master password is wrong.".into());
                } else {
                    match vault.change_master(&change.new) {
                        Ok(()) => {
                            self.notice = Some("Master password changed.".into());
                            self.change = None;
                        }
                        Err(e) => change.error = Some(e.to_string()),
                    }
                }
            }
            Some(false) => self.change = None,
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlocking_shares_the_entries_and_locking_forgets_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::create(
            &vault_path(dir.path()),
            "correct horse",
            KdfCost::for_tests(),
        )
        .unwrap();
        vault
            .put(
                None,
                VaultEntry {
                    name: "NOC".into(),
                    username: "noc".into(),
                    password: "pw".into(),
                    passphrase: String::new(),
                },
            )
            .unwrap();
        let mut ui = VaultUi::new();
        assert!(ui.credentials().is_none());
        ui.master = "wrong horse".into();
        assert!(!ui.unlock(dir.path()));
        assert_eq!(ui.error.as_deref(), Some("Wrong master password."));
        assert!(ui.master.is_empty(), "a wrong password is cleared");
        ui.master = "correct horse".into();
        assert!(ui.unlock(dir.path()));
        assert_eq!(ui.credentials().unwrap()[0].password, "pw");
        assert_eq!(ui.names().unwrap(), ["NOC"]);
        assert!(ui.master.is_empty(), "the master password is not kept");
        ui.lock();
        assert!(ui.credentials().is_none() && !ui.is_unlocked());
    }
}
