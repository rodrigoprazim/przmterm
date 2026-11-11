//! The button bar above the status bar: macro buttons in tabs, and the
//! dialogs to create and change them.

use std::path::Path;

use eframe::egui;
use przmterm_session::buttons::{
    ButtonBar, ButtonTab, Chunk, MacroButton, parse_send, read_buttons, uses_vault, write_buttons,
};

/// A button was pressed: what to send to the current session.
pub struct Press {
    pub chunks: Vec<Chunk>,
    /// Run this script instead (a name in `<config>/scripts` or a path).
    pub script: Option<String>,
    /// A regular expression the line at the cursor must match; empty:
    /// always send.
    pub only_when: String,
}

impl Press {
    fn of(button: &MacroButton) -> Self {
        Self {
            chunks: parse_send(&button.send),
            script: (!button.script.trim().is_empty()).then(|| button.script.clone()),
            only_when: button.only_when.trim().to_string(),
        }
    }
}

struct ButtonEdit {
    tab: usize,
    /// `None` for a new button.
    index: Option<usize>,
    button: MacroButton,
    error: Option<String>,
    /// The label field got the focus once (on opening).
    focused: bool,
    /// The master password was given for this dialog: it may save a
    /// button that types from the vault.
    authorized: bool,
}

/// What waits for the master password.
enum Pending {
    /// Opening the dialog of a button that types from the vault.
    Edit(usize, usize),
    /// Saving the open dialog, whose button types from the vault.
    Save,
}

struct TabEdit {
    /// `None` for a new tab.
    index: Option<usize>,
    name: String,
    error: Option<String>,
}

pub struct ButtonBarUi {
    pub bar: ButtonBar,
    active: usize,
    pub visible: bool,
    button_edit: Option<ButtonEdit>,
    tab_edit: Option<TabEdit>,
    /// A tab waiting for "really delete?".
    delete_tab: Option<usize>,
    pub error: Option<String>,
    /// The Vault menu asked for the vault to be unlocked.
    pub unlock_request: bool,
    /// A change to a button that types from the vault needs the master
    /// password; answer with [`Self::master_given`] or
    /// [`Self::master_refused`].
    pub master_request: bool,
    pending: Option<Pending>,
}

impl ButtonBarUi {
    pub fn load(config_dir: Option<&Path>) -> Self {
        let (bar, error) = match config_dir.map(read_buttons) {
            Some(Ok(bar)) => {
                let note = (!bar.unknown.is_empty()).then(|| {
                    format!(
                        "{}: unknown setting(s) ignored: {}",
                        przmterm_session::buttons::BUTTONS_FILE,
                        bar.unknown.join(", ")
                    )
                });
                (bar, note)
            }
            Some(Err(e)) => (ButtonBar::default(), Some(e.to_string())),
            None => (ButtonBar::default(), None),
        };
        Self {
            // A broken file shows the bar, with the error on it.
            visible: !bar.tabs.is_empty() || error.is_some(),
            bar,
            active: 0,
            button_edit: None,
            tab_edit: None,
            delete_tab: None,
            error,
            unlock_request: false,
            master_request: false,
            pending: None,
        }
    }

    /// Every button as `Tab/Label`, for the keyboard shortcuts.
    pub fn ids(&self) -> Vec<String> {
        self.bar
            .tabs
            .iter()
            .flat_map(|t| {
                t.buttons
                    .iter()
                    .map(move |b| format!("{}/{}", t.name, b.label))
            })
            .collect()
    }

    /// What pressing the button `Tab/Label` does.
    pub fn press(&self, id: &str) -> Option<Press> {
        let (tab, label) = id.split_once('/')?;
        let button = self
            .bar
            .tabs
            .iter()
            .find(|t| t.name == tab)?
            .buttons
            .iter()
            .find(|b| b.label == label)?;
        Some(Press::of(button))
    }

    pub fn modal(&self) -> bool {
        self.button_edit.is_some() || self.tab_edit.is_some() || self.delete_tab.is_some()
    }

    /// The master password was given: the dialog may save vault buttons,
    /// and what waited for it goes on.
    pub fn master_given(&mut self, config_dir: Option<&Path>) {
        if let Some(edit) = &mut self.button_edit {
            edit.authorized = true;
        }
        match self.pending.take() {
            Some(Pending::Edit(tab, i)) => {
                if let Some(button) = self.bar.tabs.get(tab).and_then(|t| t.buttons.get(i)) {
                    self.button_edit = Some(ButtonEdit {
                        tab,
                        index: Some(i),
                        button: button.clone(),
                        error: None,
                        focused: false,
                        authorized: true,
                    });
                }
            }
            Some(Pending::Save) => self.commit_edit(config_dir),
            None => {}
        }
    }

    pub fn master_refused(&mut self) {
        if let (Some(Pending::Save), Some(edit)) = (self.pending.take(), &mut self.button_edit) {
            edit.error = Some(
                "Not saved: a button that types from the vault needs the master password.".into(),
            );
        }
    }

    /// Saves the dialog's button and closes it.
    fn commit_edit(&mut self, config_dir: Option<&Path>) {
        let Some(edit) = self.button_edit.take() else {
            return;
        };
        let mut button = edit.button;
        button.label = button.label.trim().to_string();
        button.only_when = button.only_when.trim().to_string();
        let Some(group) = self.bar.tabs.get_mut(edit.tab) else {
            return;
        };
        match edit.index {
            Some(i) if i < group.buttons.len() => group.buttons[i] = button,
            _ => group.buttons.push(button),
        }
        self.save(config_dir);
    }

    fn save(&mut self, config_dir: Option<&Path>) {
        if let Some(dir) = config_dir {
            self.error = write_buttons(dir, &self.bar).err().map(|e| e.to_string());
        }
    }

    /// Rename, add and delete groups, and the tooltips of the shown one.
    fn group_menu(&mut self, ui: &mut egui::Ui, changed: &mut bool) {
        let i = self.active;
        if ui.button("Rename group…").clicked() {
            self.tab_edit = Some(TabEdit {
                index: Some(i),
                name: self.bar.tabs[i].name.clone(),
                error: None,
            });
            ui.close();
        }
        if ui.button("New group…").clicked() {
            self.tab_edit = Some(TabEdit {
                index: None,
                name: String::new(),
                error: None,
            });
            ui.close();
        }
        if ui.button("Delete group…").clicked() {
            self.delete_tab = Some(i);
            ui.close();
        }
        ui.separator();
        for (label, on) in [("Show all tooltips", true), ("Hide all tooltips", false)] {
            if ui
                .button(label)
                .on_hover_text("For every button in this group")
                .clicked()
            {
                for button in &mut self.bar.tabs[i].buttons {
                    button.tooltip = on;
                }
                *changed = true;
                ui.close();
            }
        }
    }

    /// Draws the bar; returns the button pressed, if any.
    pub fn show(&mut self, ui: &mut egui::Ui, config_dir: Option<&Path>) -> Option<Press> {
        let mut press = None;
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            if self.bar.tabs.is_empty() {
                ui.weak("No buttons yet.");
                if ui.button("+ Add a group of buttons").clicked() {
                    self.tab_edit = Some(TabEdit {
                        index: None,
                        name: "Commands".into(),
                        error: None,
                    });
                }
                return;
            }
            self.active = self.active.min(self.bar.tabs.len() - 1);
            // The groups are a list to pick from, so many of them still
            // leave the bar to the buttons.
            let mut chosen = self.active;
            let combo = ui.scope(|ui| {
                crate::theme::sunken_combos(ui);
                egui::ComboBox::from_id_salt("button-groups")
                    .selected_text(egui::RichText::new(&self.bar.tabs[self.active].name).strong())
                    .width(75.0)
                    .height(400.0)
                    .show_ui(ui, |ui| {
                        // Group names are short: the list is half a menu's
                        // width, and grows for a longer name.
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.set_min_width(crate::theme::MENU_WIDTH / 2.0);
                        for (i, group) in self.bar.tabs.iter().enumerate() {
                            ui.selectable_value(&mut chosen, i, &group.name);
                        }
                    })
                    .response
            });
            self.active = chosen;
            combo.inner.on_hover_text("Button group");
            let more = crate::icons::button(ui, crate::icons::Icon::More, false)
                .on_hover_text("Group actions");
            egui::Popup::menu(&more).show(|ui| {
                crate::theme::menu(ui);
                self.group_menu(ui, &mut changed);
            });
            ui.separator();
            let tab = self.active;
            let count = self.bar.tabs[tab].buttons.len();
            let mut moved = None;
            let mut removed = None;
            let mut tooltip = None;
            for (i, button) in self.bar.tabs[tab].buttons.iter().enumerate() {
                let mut response = ui.button(&button.label);
                // The description, else what the button sends or runs;
                // nothing at all when the button has none of them.
                let tip = if !button.description.trim().is_empty() {
                    button.description.trim().to_string()
                } else if !button.script.trim().is_empty() {
                    format!("Runs {}", button.script.trim())
                } else {
                    button.send.trim().to_string()
                };
                if button.tooltip && !tip.is_empty() {
                    response = response.on_hover_text(tip);
                }
                if response.clicked() {
                    press = Some(Press::of(button));
                }
                response.context_menu(|ui| {
                    // Its few short entries: half a menu's width.
                    ui.set_min_width(crate::theme::MENU_WIDTH / 2.0);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    if ui.button("Edit…").clicked() {
                        if uses_vault(&button.send) {
                            // Its password is not shown, but its Only when
                            // could be taken off: the master password first.
                            self.pending = Some(Pending::Edit(tab, i));
                            self.master_request = true;
                        } else {
                            self.button_edit = Some(ButtonEdit {
                                tab,
                                index: Some(i),
                                button: button.clone(),
                                error: None,
                                focused: false,
                                authorized: false,
                            });
                        }
                        ui.close();
                    }
                    if ui
                        .add_enabled(i > 0, egui::Button::new("Move left"))
                        .clicked()
                    {
                        moved = Some((i, i - 1));
                        ui.close();
                    }
                    if ui
                        .add_enabled(i + 1 < count, egui::Button::new("Move right"))
                        .clicked()
                    {
                        moved = Some((i, i + 1));
                        ui.close();
                    }
                    if ui.button("Delete").clicked() {
                        removed = Some(i);
                        ui.close();
                    }
                    ui.separator();
                    let mut on = button.tooltip;
                    if crate::theme::checkbox(ui, &mut on, "Show tooltip").changed() {
                        tooltip = Some((i, on));
                    }
                });
            }
            if let Some((i, on)) = tooltip {
                self.bar.tabs[tab].buttons[i].tooltip = on;
                changed = true;
            }
            if let Some((a, b)) = moved {
                self.bar.tabs[tab].buttons.swap(a, b);
                changed = true;
            }
            if let Some(i) = removed {
                self.bar.tabs[tab].buttons.remove(i);
                changed = true;
            }
            if crate::icons::button(ui, crate::icons::Icon::Plus, false)
                .on_hover_text("Add a button to this group")
                .clicked()
            {
                self.button_edit = Some(ButtonEdit {
                    tab,
                    index: None,
                    button: MacroButton::default(),
                    error: None,
                    focused: false,
                    authorized: false,
                });
            }
        });
        if changed {
            self.save(config_dir);
        }
        press
    }

    /// The button and tab dialogs.
    /// `vault`: the vault's entry names while it is open, for the Vault
    /// menu.
    pub fn dialogs(
        &mut self,
        ctx: &egui::Context,
        config_dir: Option<&Path>,
        vault: Option<&[String]>,
    ) {
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter) && !i.modifiers.shift,
                i.key_pressed(egui::Key::Escape),
            )
        });
        let center = ctx.content_rect().center();

        let mut commit = false;
        if let Some(edit) = &mut self.button_edit {
            let mut done = None; // Some(true) save, Some(false) cancel
            egui::Window::new(if edit.index.is_some() {
                "Edit button"
            } else {
                "New button"
            })
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("button-edit")
                    .num_columns(2)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Label");
                        let label = ui.add(
                            egui::TextEdit::singleline(&mut edit.button.label)
                                .hint_text("Interfaces")
                                .desired_width(320.0),
                        );
                        if !edit.focused {
                            label.request_focus();
                            edit.focused = true;
                        }
                        ui.end_row();
                        ui.label("Send");
                        ui.add(
                            egui::TextEdit::multiline(&mut edit.button.send)
                                .hint_text("show ip interface brief\\r")
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(3)
                                .desired_width(320.0),
                        );
                        ui.end_row();
                        ui.label("");
                        ui.horizontal(|ui| {
                            ui.set_width(320.0);
                            ui.weak("\\r Enter   \\t Tab   \\e Esc   \\p pause 1 s");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    self.unlock_request |=
                                        vault_menu(ui, &mut edit.button.send, vault);
                                },
                            );
                        });
                        ui.end_row();
                        ui.label("Only when").on_hover_text(
                            "A regular expression: the button sends only when the line \
at the cursor matches it",
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut edit.button.only_when)
                                .hint_text("(?i)password:|senha:   (empty: always)")
                                .font(egui::TextStyle::Monospace)
                                .desired_width(320.0),
                        );
                        ui.end_row();
                        if edit.button.only_when.trim().is_empty()
                            && edit.button.send.contains("{vault:")
                        {
                            ui.label("");
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(
                                        "This button types a password. Set Only when to \
the prompt it answers, so a stray press does not show it on screen.",
                                    )
                                    .color(ui.visuals().warn_fg_color),
                                )
                                .wrap(),
                            );
                            ui.end_row();
                        }
                        ui.label("Script").on_hover_text(
                            "A Rhai script to run instead of sending the text above",
                        );
                        // The field and Browse… together as wide as the others.
                        ui.allocate_ui_with_layout(
                            egui::vec2(320.0, ui.spacing().interact_size.y),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.button("Browse…").clicked()
                                    && let Some(name) = pick_script(config_dir)
                                {
                                    edit.button.script = name;
                                }
                                ui.add(
                                    egui::TextEdit::singleline(&mut edit.button.script)
                                        .hint_text("none: the button sends the text")
                                        .desired_width(ui.available_width()),
                                );
                            },
                        );
                        ui.end_row();
                        ui.label("Description");
                        ui.add(
                            egui::TextEdit::singleline(&mut edit.button.description)
                                .hint_text("Shown when the pointer rests on the button")
                                .desired_width(320.0),
                        );
                        ui.end_row();
                        ui.label("");
                        crate::theme::checkbox(ui, &mut edit.button.tooltip, "Show tooltip")
                            .on_hover_text(
                                "For this button: the description, or else what it sends",
                            );
                        ui.end_row();
                    });
                if let Some(err) = &edit.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.add_space(14.0);
                // Cancel and Clear on the left, Save on the right.
                ui.horizontal(|ui| {
                    if ui
                        .button("Clear")
                        .on_hover_text("Empty every field")
                        .clicked()
                    {
                        edit.button = MacroButton::default();
                        edit.error = None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::theme::primary_button(ui, "Save").clicked() {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            done = Some(false);
                        }
                    });
                });
            });
            match done {
                Some(true) if edit.button.label.trim().is_empty() => {
                    edit.error = Some("Give the button a label.".into());
                }
                Some(true) if check_only_when(&edit.button.only_when).is_err() => {
                    edit.error = check_only_when(&edit.button.only_when).err();
                }
                Some(true) if uses_vault(&edit.button.send) && !edit.authorized => {
                    self.pending = Some(Pending::Save);
                    self.master_request = true;
                }
                Some(true) => commit = true,
                Some(false) => {
                    self.button_edit = None;
                    self.pending = None;
                }
                None => {}
            }
        }
        if commit {
            self.commit_edit(config_dir);
        }

        if let Some(edit) = &mut self.tab_edit {
            let mut done = None;
            egui::Window::new(if edit.index.is_some() {
                "Rename group"
            } else {
                "New group"
            })
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(center)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut edit.name)
                        .hint_text("Cisco, Huawei, Linux…")
                        .desired_width(240.0),
                )
                .request_focus();
                if let Some(err) = &edit.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::theme::primary_button(ui, "OK").clicked() || enter {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            done = Some(false);
                        }
                    });
                });
            });
            match done {
                Some(true) if edit.name.trim().is_empty() => {
                    edit.error = Some("Give the group a name.".into());
                }
                Some(true) => {
                    let name = edit.name.trim().to_string();
                    match edit.index {
                        Some(i) => self.bar.tabs[i].name = name,
                        None => {
                            self.bar.tabs.push(ButtonTab {
                                name,
                                buttons: Vec::new(),
                            });
                            self.active = self.bar.tabs.len() - 1;
                        }
                    }
                    self.tab_edit = None;
                    self.visible = true;
                    self.save(config_dir);
                }
                Some(false) => self.tab_edit = None,
                None => {}
            }
        }

        if let Some(i) = self.delete_tab {
            let mut done = None;
            let tab = &self.bar.tabs[i];
            egui::Window::new("Delete group")
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(center)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Delete the group {} and its {} button(s)?",
                        tab.name,
                        tab.buttons.len()
                    ));
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
                    self.bar.tabs.remove(i);
                    self.delete_tab = None;
                    self.save(config_dir);
                }
                Some(false) => self.delete_tab = None,
                None => {}
            }
        }
    }
}

/// Picks a script file: a file in `<config>/scripts` is kept by name, any
/// other by its full path.
/// A Vault ▾ menu that adds a `{vault:…}` reference to `text`: the entry's
/// password, user name or key passphrase is typed when the button or
/// shortcut is used, and only the entry's name is kept. Returns whether
/// the vault should be unlocked to list its entries.
/// Checks a button's Only when expression; empty is fine.
pub fn check_only_when(pattern: &str) -> Result<(), String> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Ok(());
    }
    fancy_regex::Regex::new(pattern)
        .map(|_| ())
        .map_err(|e| format!("Only when is not a valid regular expression: {e}"))
}

/// Whether `line` (the text before the cursor) lets a press with this
/// Only when expression through. An invalid expression lets nothing
/// through.
pub fn guard_allows(pattern: &str, line: &str) -> Result<bool, String> {
    if pattern.trim().is_empty() {
        return Ok(true);
    }
    let re = fancy_regex::Regex::new(pattern.trim())
        .map_err(|e| format!("Only when is not a valid regular expression: {e}"))?;
    re.is_match(line).map_err(|e| e.to_string())
}

pub fn vault_menu(ui: &mut egui::Ui, text: &mut String, names: Option<&[String]>) -> bool {
    use przmterm_session::buttons::{VaultField, vault_reference};
    let mut unlock = false;
    ui.menu_button("Vault ▾", |ui| {
        crate::theme::menu(ui);
        match names {
            None => {
                ui.weak("The vault is locked.");
                if ui.button("Unlock the vault…").clicked() {
                    unlock = true;
                    ui.close();
                }
            }
            Some([]) => {
                ui.weak("The vault has no entries yet.");
            }
            Some(names) => {
                for name in names {
                    ui.menu_button(name, |ui| {
                        crate::theme::menu(ui);
                        for (label, field) in [
                            ("Password", VaultField::Password),
                            ("User name", VaultField::Username),
                            ("Key passphrase", VaultField::Passphrase),
                        ] {
                            if ui.button(label).clicked() {
                                text.push_str(&vault_reference(name, field));
                                ui.close();
                            }
                        }
                    });
                }
            }
        }
    })
    .response
    .on_hover_text(
        "Type a password or user name from the vault. Only the entry's name is \
         kept here; the secret stays in the encrypted vault.",
    );
    unlock
}

pub fn pick_script(config_dir: Option<&Path>) -> Option<String> {
    let mut picker = rfd::FileDialog::new()
        .set_title("Choose a script")
        .add_filter("Rhai script", &[przmterm_script::EXTENSION]);
    let scripts = config_dir.map(|d| d.join(przmterm_session::SCRIPTS_DIR));
    if let Some(dir) = &scripts {
        let _ = std::fs::create_dir_all(dir);
        picker = picker.set_directory(dir);
    }
    let path = picker.pick_file()?;
    Some(
        match scripts.and_then(|d| path.strip_prefix(&d).ok().map(Path::to_path_buf)) {
            Some(rel) => rel.display().to_string(),
            None => path.display().to_string(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_when_checks_the_prompt() {
        assert_eq!(guard_allows("", "router#"), Ok(true));
        assert_eq!(guard_allows("  ", "router#"), Ok(true));
        let guard = "(P|p)assword:|(S|s)enha:";
        assert_eq!(guard_allows(guard, "Password:"), Ok(true));
        assert_eq!(guard_allows(guard, "senha:"), Ok(true));
        assert_eq!(guard_allows(guard, "router#"), Ok(false));
        assert_eq!(guard_allows("(?i)password:", "PASSWORD:"), Ok(true));
        assert!(guard_allows("(unclosed", "Password:").is_err());
        assert!(check_only_when("(unclosed").is_err());
        assert!(check_only_when("").is_ok());
        assert!(check_only_when("[Pp]assword:").is_ok());
    }

    #[test]
    fn vault_buttons_wait_for_the_master_password() {
        let dir = tempfile::tempdir().unwrap();
        let mut bar = ButtonBarUi::load(Some(dir.path()));
        bar.bar.tabs.push(ButtonTab {
            name: "Cisco".into(),
            buttons: vec![MacroButton {
                label: "Enable".into(),
                send: "{vault:Core}\\r".into(),
                only_when: "(?i)password:".into(),
                ..MacroButton::default()
            }],
        });
        // Refused: nothing opens.
        bar.pending = Some(Pending::Edit(0, 0));
        bar.master_refused();
        assert!(bar.button_edit.is_none() && bar.pending.is_none());
        // Given: the dialog opens, and may save without asking again.
        bar.pending = Some(Pending::Edit(0, 0));
        bar.master_given(Some(dir.path()));
        let edit = bar.button_edit.as_mut().unwrap();
        assert!(edit.authorized);
        edit.button.only_when.clear();
        bar.pending = Some(Pending::Save);
        bar.master_given(Some(dir.path()));
        assert!(bar.button_edit.is_none());
        assert_eq!(bar.bar.tabs[0].buttons[0].only_when, "");
        let saved = read_buttons(dir.path()).unwrap();
        assert_eq!(saved.tabs[0].buttons[0].label, "Enable");
    }
}
