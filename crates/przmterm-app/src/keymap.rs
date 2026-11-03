//! Keyboard shortcuts: the built-in ones, the user's keymap, catching them
//! before the terminal sees the keys, and the window to change them.

use std::path::Path;

use eframe::egui::{self, Key, Modifiers};
use przmterm_session::keymap::{
    Binding, Chord, Does, Keymap, read_keymap, reset_keymap, write_keymap,
};

/// An app command a shortcut can run.
pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    /// The built-in shortcut, if any.
    pub default: Option<&'static str>,
}

pub const COMMANDS: [Command; 22] = [
    Command {
        id: "new_tab",
        label: "New tab (same session)",
        default: Some("Cmd+Shift+T"),
    },
    Command {
        id: "close_pane",
        label: "Close pane",
        default: Some("Cmd+Shift+W"),
    },
    Command {
        id: "close_tab",
        label: "Close tab",
        default: None,
    },
    Command {
        id: "next_tab",
        label: "Next tab",
        default: Some("Ctrl+Tab"),
    },
    Command {
        id: "previous_tab",
        label: "Previous tab",
        default: Some("Ctrl+Shift+Tab"),
    },
    Command {
        id: "split_grid",
        label: "Split: grid",
        default: Some("Cmd+Shift+G"),
    },
    Command {
        id: "split_columns",
        label: "Split: side by side",
        default: Some("Cmd+Shift+D"),
    },
    Command {
        id: "split_rows",
        label: "Split: stacked",
        default: Some("Cmd+Shift+E"),
    },
    Command {
        id: "separate_panes",
        label: "Separate panes into tabs",
        default: None,
    },
    Command {
        id: "clone_session",
        label: "Clone session",
        default: None,
    },
    Command {
        id: "reconnect",
        label: "Reconnect",
        default: None,
    },
    Command {
        id: "disconnect",
        label: "Disconnect",
        default: None,
    },
    Command {
        id: "send_break",
        label: "Send BREAK",
        default: Some("Cmd+Shift+B"),
    },
    Command {
        id: "toggle_sessions",
        label: "Hide / show the session list",
        default: Some("Cmd+Shift+S"),
    },
    Command {
        id: "toggle_buttons",
        label: "Hide / show the button bar",
        default: None,
    },
    Command {
        id: "toggle_log",
        label: "Start / stop the log",
        default: None,
    },
    Command {
        id: "run_script",
        label: "Run a script…",
        default: None,
    },
    Command {
        id: "stop_script",
        label: "Stop the script",
        default: None,
    },
    Command {
        id: "script_guide",
        label: "Script guide",
        default: None,
    },
    Command {
        id: "open_sftp",
        label: "Connect SFTP session",
        default: None,
    },
    Command {
        id: "send_files",
        label: "Send files (Zmodem, Ymodem, Xmodem)",
        default: None,
    },
    Command {
        id: "receive_files",
        label: "Receive files (Zmodem, Ymodem, Xmodem)",
        default: None,
    },
];

pub fn command(id: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// The built-in shortcuts.
pub fn defaults() -> Keymap {
    Keymap {
        bindings: COMMANDS
            .iter()
            .filter_map(|c| {
                c.default.map(|keys| Binding {
                    keys: keys.into(),
                    command: c.id.into(),
                    ..Default::default()
                })
            })
            .collect(),
    }
}

/// A chord as egui sees it, with `Cmd` resolved for this platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    pub key: Key,
    pub mods: Modifiers,
}

impl Keys {
    pub fn from_chord(chord: &Chord) -> Result<Keys, String> {
        let key = Key::from_name(&chord.key)
            .ok_or_else(|| format!("`{}`: unknown key `{}`", chord.text(), chord.key))?;
        let mac = cfg!(target_os = "macos");
        let mods = Modifiers {
            alt: chord.alt,
            ctrl: chord.ctrl || (chord.cmd && !mac),
            shift: chord.shift,
            mac_cmd: chord.cmd && mac,
            command: chord.cmd || (chord.ctrl && !mac),
        };
        Ok(Keys { key, mods })
    }

    /// Back to the file's notation.
    pub fn chord(&self) -> Chord {
        let mac = cfg!(target_os = "macos");
        Chord {
            cmd: if mac {
                self.mods.mac_cmd
            } else {
                self.mods.ctrl
            },
            ctrl: mac && self.mods.ctrl,
            alt: self.mods.alt,
            shift: self.mods.shift,
            key: self.key.name().to_string(),
        }
    }

    fn matches(&self, key: Key, mods: Modifiers) -> bool {
        key == self.key
            && mods.alt == self.mods.alt
            && mods.ctrl == self.mods.ctrl
            && mods.shift == self.mods.shift
            && mods.mac_cmd == self.mods.mac_cmd
    }

    /// How menus show it: `⌘⇧T` on macOS, `Ctrl+Shift+T` elsewhere.
    pub fn label(&self) -> String {
        let name = self.key.symbol_or_name();
        if cfg!(target_os = "macos") {
            let mut s = String::new();
            for (on, sym) in [
                (self.mods.ctrl, "⌃"),
                (self.mods.alt, "⌥"),
                (self.mods.shift, "⇧"),
                (self.mods.mac_cmd, "⌘"),
            ] {
                if on {
                    s.push_str(sym);
                }
            }
            s + name
        } else {
            let mut parts = Vec::new();
            for (on, text) in [
                (self.mods.ctrl, "Ctrl"),
                (self.mods.alt, "Alt"),
                (self.mods.shift, "Shift"),
            ] {
                if on {
                    parts.push(text);
                }
            }
            parts.push(name);
            parts.join("+")
        }
    }

    /// Why this would get in the way of typing, if it would.
    pub fn warning(&self) -> Option<&'static str> {
        let modified = self.mods.ctrl || self.mods.alt || self.mods.mac_cmd;
        let function_key = matches!(
            self.key,
            Key::F1
                | Key::F2
                | Key::F3
                | Key::F4
                | Key::F5
                | Key::F6
                | Key::F7
                | Key::F8
                | Key::F9
                | Key::F10
                | Key::F11
                | Key::F12
        );
        if !modified && !function_key {
            Some("Without Ctrl, Alt or ⌘ this key could no longer be typed.")
        } else if self.mods.ctrl && !self.mods.shift && !self.mods.alt && !self.mods.mac_cmd {
            Some("Ctrl with a key is often used by the remote side (Ctrl+C, Ctrl+R…).")
        } else {
            None
        }
    }
}

/// The shortcuts in force.
pub struct Shortcuts {
    pub bindings: Vec<(Keys, Binding)>,
    /// Problems in the keymap file.
    pub errors: Vec<String>,
}

impl Shortcuts {
    pub fn load(config_dir: Option<&Path>) -> Shortcuts {
        let (keymap, mut errors) = match config_dir.map(read_keymap) {
            Some(Ok(Some(k))) => (k, Vec::new()),
            Some(Err(e)) => (defaults(), vec![e]),
            _ => (defaults(), Vec::new()),
        };
        let (bindings, more) = compile(&keymap);
        errors.extend(more);
        Shortcuts { bindings, errors }
    }

    /// Takes the first shortcut pressed this frame out of the input, so
    /// the terminal does not see it.
    pub fn take(&self, ctx: &egui::Context) -> Option<Binding> {
        ctx.input_mut(|i| self.take_from(&mut i.events))
    }

    fn take_from(&self, events: &mut Vec<egui::Event>) -> Option<Binding> {
        let (n, binding) = events.iter().enumerate().find_map(|(n, event)| {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                return None;
            };
            self.bindings
                .iter()
                .find(|(keys, _)| keys.matches(*key, *modifiers))
                .map(|(_, b)| (n, b.clone()))
        })?;
        events.remove(n);
        // Alt+key also types a character on some systems; drop it.
        if matches!(events.get(n), Some(egui::Event::Text(_))) {
            events.remove(n);
        }
        Some(binding)
    }

    /// The label of the shortcut for an app command, for menus.
    pub fn label_for(&self, id: &str) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, b)| b.command == id)
            .map(|(k, _)| k.label())
    }
}

/// Resolves the keys of every binding; unusable ones are reported.
fn compile(keymap: &Keymap) -> (Vec<(Keys, Binding)>, Vec<String>) {
    let mut out: Vec<(Keys, Binding)> = Vec::new();
    let mut errors = Vec::new();
    for b in &keymap.bindings {
        let keys = match Chord::parse(&b.keys).and_then(|c| Keys::from_chord(&c)) {
            Ok(k) => k,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        match b.does() {
            Err(e) => {
                errors.push(e);
                continue;
            }
            Ok(Does::Command(id)) if command(id).is_none() => {
                errors.push(format!("{}: unknown command `{id}`", b.keys));
                continue;
            }
            Ok(_) => {}
        }
        if out.iter().any(|(k, _)| *k == keys) {
            errors.push(format!(
                "{} is used twice; the first one counts",
                keys.label()
            ));
            continue;
        }
        out.push((keys, b.clone()));
    }
    (out, errors)
}

/// What the editor's form is choosing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Command,
    Send,
    Script,
    Button,
}

struct Edit {
    /// Row being changed; `None` for a new shortcut.
    index: Option<usize>,
    keys: Option<Keys>,
    recording: bool,
    kind: Kind,
    command: &'static str,
    text: String,
    error: Option<String>,
}

/// The "Keyboard shortcuts" window.
pub struct KeymapEditor {
    rows: Vec<(Keys, Binding)>,
    edit: Option<Edit>,
    changed: bool,
    error: Option<String>,
}

pub enum KeymapResult {
    Open,
    /// Closed; the shortcuts may have changed.
    Closed,
}

impl KeymapEditor {
    pub fn new(shortcuts: &Shortcuts) -> Self {
        Self {
            rows: shortcuts.bindings.clone(),
            edit: None,
            changed: false,
            error: (!shortcuts.errors.is_empty()).then(|| shortcuts.errors.join("\n")),
        }
    }

    fn describe(b: &Binding) -> String {
        match b.does() {
            Ok(Does::Command(id)) => command(id).map_or(id.to_string(), |c| c.label.to_string()),
            Ok(Does::Send(t)) => format!("Send {t}"),
            Ok(Does::Script(s)) => format!("Run script {s}"),
            Ok(Does::Button(b)) => format!("Press button {b}"),
            Err(e) => e,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        config_dir: &Path,
        buttons: &[String],
    ) -> KeymapResult {
        let mut open = true;
        let mut close = false;
        let escape = ctx.input(|i| i.key_pressed(Key::Escape));
        egui::Window::new("Keyboard shortcuts")
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .default_width(600.0)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                }
                let mut remove = None;
                let mut change = None;
                // Rows use the whole width; the buttons sit at the right edge.
                let gap = ui.spacing().item_spacing.x;
                let width = ui.available_width();
                let keys_w = (width - 2.0 * gap) * 0.28;
                let does_w = (width - 2.0 * gap) * 0.45;
                let last = width - 2.0 * gap - keys_w - does_w;
                let row_h = ui.spacing().interact_size.y;
                let cell = |ui: &mut egui::Ui, w: f32, add: &mut dyn FnMut(&mut egui::Ui)| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(w, row_h),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_width(w);
                            add(ui);
                        },
                    );
                };
                ui.horizontal(|ui| {
                    cell(ui, keys_w, &mut |ui| {
                        ui.strong("Keys");
                    });
                    cell(ui, does_w, &mut |ui| {
                        ui.strong("Does");
                    });
                });
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, (keys, b)) in self.rows.iter().enumerate() {
                            let rect = egui::Rect::from_min_size(
                                ui.cursor().min,
                                egui::vec2(width, row_h),
                            );
                            if i % 2 == 0 {
                                ui.painter()
                                    .rect_filled(rect, 2.0, ui.visuals().faint_bg_color);
                            }
                            ui.horizontal(|ui| {
                                cell(ui, keys_w, &mut |ui| {
                                    ui.monospace(keys.label());
                                });
                                cell(ui, does_w, &mut |ui| {
                                    ui.label(Self::describe(b));
                                });
                                ui.allocate_ui_with_layout(
                                    egui::vec2(last, row_h),
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.set_min_width(last);
                                        if ui.small_button("Delete").clicked() {
                                            remove = Some(i);
                                        }
                                        if ui.small_button("Change…").clicked() {
                                            change = Some(i);
                                        }
                                    },
                                );
                            });
                        }
                    });
                if let Some(i) = remove {
                    self.rows.remove(i);
                    self.changed = true;
                }
                if let Some(i) = change {
                    let (keys, b) = &self.rows[i];
                    let (kind, command, text) = match b.does() {
                        Ok(Does::Command(id)) => (
                            Kind::Command,
                            command(id).map_or(COMMANDS[0].id, |c| c.id),
                            String::new(),
                        ),
                        Ok(Does::Send(t)) => (Kind::Send, COMMANDS[0].id, t.to_string()),
                        Ok(Does::Script(s)) => (Kind::Script, COMMANDS[0].id, s.to_string()),
                        Ok(Does::Button(s)) => (Kind::Button, COMMANDS[0].id, s.to_string()),
                        Err(_) => (Kind::Command, COMMANDS[0].id, String::new()),
                    };
                    self.edit = Some(Edit {
                        index: Some(i),
                        keys: Some(*keys),
                        recording: false,
                        kind,
                        command,
                        text,
                        error: None,
                    });
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Reset to defaults")
                        .on_hover_text("Go back to the built-in shortcuts")
                        .clicked()
                    {
                        self.rows = compile(&defaults()).0;
                        self.changed = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("New shortcut…").clicked() {
                            self.edit = Some(Edit {
                                index: None,
                                keys: None,
                                recording: true,
                                kind: Kind::Send,
                                command: COMMANDS[0].id,
                                text: String::new(),
                                error: None,
                            });
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.weak("Copy and paste (⌘C/⌘V, Ctrl+Shift+C/V) cannot be changed.");
            });
        self.edit_dialog(ctx, config_dir, buttons);
        if self.changed {
            self.changed = false;
            let defaults = compile(&defaults()).0;
            let same = defaults.len() == self.rows.len()
                && defaults.iter().zip(&self.rows).all(|(a, b)| a == b);
            let result = if same {
                reset_keymap(config_dir)
            } else {
                write_keymap(
                    config_dir,
                    &Keymap {
                        bindings: self
                            .rows
                            .iter()
                            .map(|(k, b)| Binding {
                                // Keep the text as written ("Ctrl+Tab" stays
                                // Ctrl on macOS) when it still means these keys.
                                keys: match Chord::parse(&b.keys).and_then(|c| Keys::from_chord(&c))
                                {
                                    Ok(same) if same == *k => b.keys.clone(),
                                    _ => k.chord().text(),
                                },
                                ..b.clone()
                            })
                            .collect(),
                    },
                )
            };
            self.error = result.err();
        }
        if !open || close || (escape && self.edit.is_none()) {
            KeymapResult::Closed
        } else {
            KeymapResult::Open
        }
    }

    fn edit_dialog(&mut self, ctx: &egui::Context, config_dir: &Path, buttons: &[String]) {
        let Some(edit) = &mut self.edit else { return };
        // Recording: the next key pressed with its modifiers.
        if edit.recording {
            let pressed = ctx.input_mut(|i| {
                let found = i.events.iter().position(|e| {
                    matches!(e, egui::Event::Key { pressed: true, key, .. } if *key != Key::Escape)
                });
                let n = found?;
                let egui::Event::Key { key, modifiers, .. } = i.events.remove(n) else {
                    return None;
                };
                if matches!(i.events.get(n), Some(egui::Event::Text(_))) {
                    i.events.remove(n);
                }
                Some(Keys {
                    key,
                    mods: Modifiers {
                        command: modifiers.command,
                        ..modifiers
                    },
                })
            });
            if let Some(keys) = pressed {
                edit.keys = Some(keys);
                edit.recording = false;
                edit.error = None;
            } else if ctx.input(|i| i.key_pressed(Key::Escape)) {
                edit.recording = false;
            }
            ctx.request_repaint();
        }
        let escape = !edit.recording && ctx.input(|i| i.key_pressed(Key::Escape));
        let mut done = None;
        egui::Window::new(if edit.index.is_some() {
            "Change shortcut"
        } else {
            "New shortcut"
        })
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("keymap-edit")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Keys");
                    ui.horizontal(|ui| {
                        let text = if edit.recording {
                            egui::RichText::new("Press the keys…").italics()
                        } else {
                            egui::RichText::new(edit.keys.map_or("none".to_string(), |k| k.label()))
                                .monospace()
                                .strong()
                        };
                        ui.label(text);
                        if !edit.recording && ui.button("Record").clicked() {
                            edit.recording = true;
                        }
                    });
                    ui.end_row();
                    if let Some(w) = edit.keys.and_then(|k| k.warning()) {
                        ui.label("");
                        ui.colored_label(ui.visuals().warn_fg_color, w);
                        ui.end_row();
                    }
                    ui.label("Does");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut edit.kind, Kind::Send, "Send text");
                        ui.selectable_value(&mut edit.kind, Kind::Command, "App command");
                        ui.selectable_value(&mut edit.kind, Kind::Script, "Run script");
                        ui.selectable_value(&mut edit.kind, Kind::Button, "Press button");
                    });
                    ui.end_row();
                    ui.label("");
                    match edit.kind {
                        Kind::Command => {
                            let label = command(edit.command).map_or("", |c| c.label);
                            egui::ComboBox::from_id_salt("keymap-command")
                                .width(280.0)
                                .selected_text(label)
                                .show_ui(ui, |ui| {
                                    for c in &COMMANDS {
                                        ui.selectable_value(&mut edit.command, c.id, c.label);
                                    }
                                });
                        }
                        Kind::Send => {
                            ui.vertical(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut edit.text)
                                        .hint_text("show ip interface brief\\r")
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(300.0),
                                );
                                ui.weak("\\r Enter   \\t Tab   \\e Esc   \\p pause 1 s");
                            });
                        }
                        Kind::Script => {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut edit.text)
                                        .hint_text("script name")
                                        .desired_width(220.0),
                                );
                                if ui.button("Browse…").clicked()
                                    && let Some(name) =
                                        crate::buttonbar::pick_script(Some(config_dir))
                                {
                                    edit.text = name;
                                }
                            });
                        }
                        Kind::Button => {
                            egui::ComboBox::from_id_salt("keymap-button")
                                .width(280.0)
                                .selected_text(if edit.text.is_empty() {
                                    "choose a button"
                                } else {
                                    edit.text.as_str()
                                })
                                .show_ui(ui, |ui| {
                                    if buttons.is_empty() {
                                        ui.weak("No buttons yet");
                                    }
                                    for b in buttons {
                                        ui.selectable_value(&mut edit.text, b.clone(), b);
                                    }
                                });
                        }
                    }
                    ui.end_row();
                });
            if let Some(e) = &edit.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save").clicked() {
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
                let Some(keys) = edit.keys else {
                    edit.error = Some("Record the keys first.".into());
                    return;
                };
                let text = edit.text.trim().to_string();
                let mut b = Binding {
                    keys: keys.chord().text(),
                    ..Default::default()
                };
                match edit.kind {
                    Kind::Command => b.command = edit.command.into(),
                    Kind::Send => b.send = edit.text.clone(),
                    Kind::Script => b.script = text,
                    Kind::Button => b.button = text,
                }
                if let Err(e) = b.does() {
                    edit.error = Some(match edit.kind {
                        Kind::Send => "Type the text to send.".into(),
                        Kind::Script => "Choose a script.".into(),
                        Kind::Button => "Choose a button.".into(),
                        Kind::Command => e,
                    });
                    return;
                }
                if let Some((i, _)) = self
                    .rows
                    .iter()
                    .enumerate()
                    .find(|(i, (k, _))| *k == keys && Some(*i) != edit.index)
                {
                    edit.error = Some(format!(
                        "{} already does: {}",
                        keys.label(),
                        Self::describe(&self.rows[i].1)
                    ));
                    return;
                }
                match edit.index {
                    Some(i) => self.rows[i] = (keys, b),
                    None => self.rows.push((keys, b)),
                }
                self.edit = None;
                self.changed = true;
            }
            Some(false) => self.edit = None,
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_compile_without_clashes() {
        let (rows, errors) = compile(&defaults());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            rows.len(),
            COMMANDS.iter().filter(|c| c.default.is_some()).count()
        );
        for c in &COMMANDS {
            assert!(command(c.id).is_some());
        }
    }

    #[test]
    fn keys_round_trip_and_clashes_are_reported() {
        for text in [
            "Cmd+Shift+T",
            "Ctrl+Tab",
            "Alt+F5",
            "F1",
            "Ctrl+Alt+Shift+K",
        ] {
            let keys = Keys::from_chord(&Chord::parse(text).unwrap()).unwrap();
            assert_eq!(
                Keys::from_chord(&keys.chord()).unwrap(),
                keys,
                "{text} → {}",
                keys.chord().text()
            );
        }
        let keymap = Keymap {
            bindings: vec![
                Binding {
                    keys: "F5".into(),
                    send: "a".into(),
                    ..Default::default()
                },
                Binding {
                    keys: "f5".into(),
                    send: "b".into(),
                    ..Default::default()
                },
                Binding {
                    keys: "F6".into(),
                    command: "fly".into(),
                    ..Default::default()
                },
                Binding {
                    keys: "Cmd+Nope".into(),
                    send: "c".into(),
                    ..Default::default()
                },
            ],
        };
        let (rows, errors) = compile(&keymap);
        assert_eq!(rows.len(), 1);
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn warnings_for_keys_that_get_in_the_way() {
        let keys = |t: &str| Keys::from_chord(&Chord::parse(t).unwrap()).unwrap();
        assert!(keys("A").warning().is_some());
        assert!(keys("F5").warning().is_none());
        assert!(keys("Cmd+Shift+T").warning().is_none());
        assert!(keys("Alt+1").warning().is_none());
    }

    #[test]
    fn a_shortcut_is_taken_from_the_input_with_exact_modifiers() {
        let shortcuts = Shortcuts {
            bindings: compile(&defaults()).0,
            errors: Vec::new(),
        };
        let press = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        // What the platform reports for ⌘ / Ctrl.
        let cmd = if cfg!(target_os = "macos") {
            Modifiers::MAC_CMD | Modifiers::COMMAND
        } else {
            Modifiers::CTRL | Modifiers::COMMAND
        };
        let mut events = vec![
            press(Key::T, cmd),
            press(Key::T, cmd | Modifiers::SHIFT),
            egui::Event::Text("x".into()),
        ];
        let taken = shortcuts.take_from(&mut events);
        let other = shortcuts.take_from(&mut events);
        assert_eq!(
            events.len(),
            1,
            "the shortcut and its text are gone: {events:?}"
        );
        assert_eq!(taken.unwrap().command, "new_tab", "only the exact chord");
        assert!(other.is_none(), "Cmd+T alone is not a shortcut");
        assert!(shortcuts.label_for("new_tab").is_some());
    }
}
