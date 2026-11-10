//! The button bar above the status bar: macro buttons in tabs, and the
//! dialogs to create and change them.

use std::path::Path;

use eframe::egui;
use przmterm_session::buttons::{
    ButtonBar, ButtonTab, Chunk, MacroButton, parse_send, read_buttons, write_buttons,
};

/// A button was pressed: what to send to the current session.
pub struct Press {
    pub chunks: Vec<Chunk>,
    /// Run this script instead (a name in `<config>/scripts` or a path).
    pub script: Option<String>,
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
        Some(Press {
            chunks: parse_send(&button.send),
            script: (!button.script.trim().is_empty()).then(|| button.script.clone()),
        })
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
                if ui.button("+ Add a tab of buttons").clicked() {
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
                    ui.separator();
                    for (label, on) in [("Show all tooltips", true), ("Hide all tooltips", false)] {
                        if ui
                            .button(label)
                            .on_hover_text("For every button in this tab")
                            .clicked()
                        {
                            for button in &mut self.bar.tabs[i].buttons {
                                button.tooltip = on;
                            }
                            changed = true;
                            ui.close();
                        }
                    }
                });
            }
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
                    press = Some(Press {
                        chunks: parse_send(&button.send),
                        script: (!button.script.trim().is_empty()).then(|| button.script.clone()),
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
                        ui.label("Script").on_hover_text(
                            "A Rhai script to run instead of sending the text above",
                        );
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut edit.button.script)
                                    .hint_text("none: the button sends the text")
                                    .desired_width(240.0),
                            );
                            if ui.button("Browse…").clicked()
                                && let Some(name) = pick_script(config_dir)
                            {
                                edit.button.script = name;
                            }
                        });
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
