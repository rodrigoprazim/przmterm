//! The button bar above the status bar: macro buttons in tabs, and the
//! dialogs to create and change them.

use std::path::Path;

use eframe::egui;
use przmterm_session::buttons::{
    ButtonBar, ButtonTab, Chunk, MacroButton, Target, parse_send, read_buttons, write_buttons,
};

/// A button was pressed: what to send, and where.
pub struct Press {
    pub chunks: Vec<Chunk>,
    pub target: Target,
}

struct ButtonEdit {
    tab: usize,
    /// `None` for a new button.
    index: Option<usize>,
    button: MacroButton,
    error: Option<String>,
    /// The label field got the focus once (on opening).
    focused: bool,
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
}

impl ButtonBarUi {
    pub fn load(config_dir: Option<&Path>) -> Self {
        let (bar, error) = match config_dir.map(read_buttons) {
            Some(Ok(bar)) => (bar, None),
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
        }
    }

    pub fn modal(&self) -> bool {
        self.button_edit.is_some() || self.tab_edit.is_some() || self.delete_tab.is_some()
    }

    fn save(&mut self, config_dir: Option<&Path>) {
        if let Some(dir) = config_dir {
            self.error = write_buttons(dir, &self.bar).err().map(|e| e.to_string());
        }
    }

    /// Draws the bar; returns the button pressed, if any.
    pub fn show(&mut self, ui: &mut egui::Ui, config_dir: Option<&Path>) -> Option<Press> {
        let mut press = None;
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            if self.bar.tabs.is_empty() {
                ui.weak("No buttons yet.");
                if ui.small_button("+ Add a tab of buttons").clicked() {
                    self.tab_edit = Some(TabEdit {
                        index: None,
                        name: "Commands".into(),
                        error: None,
                    });
                }
                return;
            }
            self.active = self.active.min(self.bar.tabs.len() - 1);
            for i in 0..self.bar.tabs.len() {
                let name = self.bar.tabs[i].name.clone();
                let tab = ui.selectable_label(i == self.active, egui::RichText::new(name).strong());
                if tab.clicked() {
                    self.active = i;
                }
                tab.context_menu(|ui| {
                    if ui.button("Rename tab…").clicked() {
                        self.tab_edit = Some(TabEdit {
                            index: Some(i),
                            name: self.bar.tabs[i].name.clone(),
                            error: None,
                        });
                        ui.close();
                    }
                    if ui.button("New tab…").clicked() {
                        self.tab_edit = Some(TabEdit {
                            index: None,
                            name: String::new(),
                            error: None,
                        });
                        ui.close();
                    }
                    if ui.button("Delete tab…").clicked() {
                        self.delete_tab = Some(i);
                        ui.close();
                    }
                });
            }
            ui.separator();
            let tab = self.active;
            let count = self.bar.tabs[tab].buttons.len();
            let mut moved = None;
            let mut removed = None;
            for (i, button) in self.bar.tabs[tab].buttons.iter().enumerate() {
                let mark = match button.target {
                    Target::Current => "",
                    Target::Tab => " ⇉",
                    Target::All => " ⇶",
                };
                let response = ui
                    .button(format!("{}{mark}", button.label))
                    .on_hover_text(format!(
                        "Sends to the {}:\n{}",
                        button.target.label(),
                        button.send
                    ));
                if response.clicked() {
                    press = Some(Press {
                        chunks: parse_send(&button.send),
                        target: button.target,
                    });
                }
                response.context_menu(|ui| {
                    if ui.button("Edit…").clicked() {
                        self.button_edit = Some(ButtonEdit {
                            tab,
                            index: Some(i),
                            button: button.clone(),
                            error: None,
                            focused: false,
                        });
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
                });
            }
            if let Some((a, b)) = moved {
                self.bar.tabs[tab].buttons.swap(a, b);
                changed = true;
            }
            if let Some(i) = removed {
                self.bar.tabs[tab].buttons.remove(i);
                changed = true;
            }
            if ui
                .small_button("+")
                .on_hover_text("Add a button to this tab")
                .clicked()
            {
                self.button_edit = Some(ButtonEdit {
                    tab,
                    index: None,
                    button: MacroButton::default(),
                    error: None,
                    focused: false,
                });
            }
        });
        if changed {
            self.save(config_dir);
        }
        press
    }

    /// The button and tab dialogs.
    pub fn dialogs(&mut self, ctx: &egui::Context, config_dir: Option<&Path>) {
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter) && !i.modifiers.shift,
                i.key_pressed(egui::Key::Escape),
            )
        });
        let center = ctx.content_rect().center();

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
                        ui.weak("\\r Enter   \\n line feed   \\t Tab   \\e Esc   \\p pause 1 s");
                        ui.end_row();
                        ui.label("Send to");
                        egui::ComboBox::from_id_salt("button-target")
                            .width(320.0)
                            .selected_text(edit.button.target.label())
                            .show_ui(ui, |ui| {
                                for t in Target::ALL {
                                    ui.selectable_value(&mut edit.button.target, t, t.label());
                                }
                            });
                        ui.end_row();
                    });
                if let Some(err) = &edit.error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() || escape {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) if edit.button.label.trim().is_empty() => {
                    edit.error = Some("Give the button a label.".into());
                }
                Some(true) => {
                    let mut button = edit.button.clone();
                    button.label = button.label.trim().to_string();
                    let buttons = &mut self.bar.tabs[edit.tab].buttons;
                    match edit.index {
                        Some(i) => buttons[i] = button,
                        None => buttons.push(button),
                    }
                    self.button_edit = None;
                    self.save(config_dir);
                }
                Some(false) => self.button_edit = None,
                None => {}
            }
        }

        if let Some(edit) = &mut self.tab_edit {
            let mut done = None;
            egui::Window::new(if edit.index.is_some() {
                "Rename tab"
            } else {
                "New tab"
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
                    if ui.button("OK").clicked() || enter {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() || escape {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) if edit.name.trim().is_empty() => {
                    edit.error = Some("Give the tab a name.".into());
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
            egui::Window::new("Delete tab")
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(center)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Delete the tab {} and its {} button(s)?",
                        tab.name,
                        tab.buttons.len()
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Delete").clicked() {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() || escape {
                            done = Some(false);
                        }
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
