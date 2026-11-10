//! Keyboard shortcuts, in `<config>/keymap.toml`.
//!
//! ```toml
//! [[key]]
//! keys = "Cmd+Shift+T"
//! command = "new_tab"
//!
//! [[key]]
//! keys = "F5"
//! send = 'show ip interface brief\r'
//!
//! [[key]]
//! keys = "Alt+1"
//! script = "cisco-show.rhai"
//!
//! [[key]]
//! keys = "Ctrl+Alt+I"
//! button = "Cisco/Interfaces"
//! ```
//!
//! `Cmd` is ⌘ on macOS and Ctrl elsewhere; `Ctrl` is always the Control key.
//! When the file exists it holds every shortcut; without it the built-in
//! ones apply.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const KEYMAP_FILE: &str = "keymap.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keymap {
    #[serde(default, rename = "key")]
    pub bindings: Vec<Binding>,
    /// Keys in the file this version does not know (left out).
    #[serde(skip)]
    pub unknown: Vec<String>,
}

/// One shortcut and what it does: exactly one of `command`, `send`,
/// `script` and `button`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub keys: String,
    /// An app command, e.g. `new_tab`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// Text to send, with the button escapes (`\r`, `\p`…).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub send: String,
    /// A script to run (see [`crate::script_path`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub script: String,
    /// A button to press: `Group/Label`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub button: String,
}

/// What a binding does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Does<'a> {
    Command(&'a str),
    Send(&'a str),
    Script(&'a str),
    Button(&'a str),
}

impl Binding {
    pub fn does(&self) -> Result<Does<'_>, String> {
        let set: Vec<Does> = [
            (!self.command.is_empty()).then_some(Does::Command(&self.command)),
            (!self.send.is_empty()).then_some(Does::Send(&self.send)),
            (!self.script.is_empty()).then_some(Does::Script(&self.script)),
            (!self.button.is_empty()).then_some(Does::Button(&self.button)),
        ]
        .into_iter()
        .flatten()
        .collect();
        match set.as_slice() {
            [one] => Ok(*one),
            [] => Err(format!("{}: nothing to do", self.keys)),
            _ => Err(format!(
                "{}: choose one of command, send, script or button",
                self.keys
            )),
        }
    }
}

/// A key with its modifiers, as written in the file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Chord {
    /// ⌘ on macOS, Ctrl elsewhere.
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The key's name: `T`, `F5`, `Tab`, `ArrowUp`…
    pub key: String,
}

impl Chord {
    pub fn parse(text: &str) -> Result<Chord, String> {
        let mut chord = Chord::default();
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let Some((key, mods)) = parts.split_last() else {
            return Err("no key".into());
        };
        // "Cmd++" is the plus key.
        let key = if key.is_empty() && text.ends_with("++") {
            "Plus"
        } else {
            key
        };
        if key.is_empty() {
            return Err(format!("`{text}`: no key"));
        }
        for m in mods.iter().filter(|m| !m.is_empty()) {
            match m.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "⌘" => chord.cmd = true,
                "ctrl" | "control" => chord.ctrl = true,
                "alt" | "option" | "opt" | "⌥" => chord.alt = true,
                "shift" | "⇧" => chord.shift = true,
                other => return Err(format!("`{text}`: unknown modifier `{other}`")),
            }
        }
        chord.key = if key.chars().count() == 1 {
            key.to_uppercase()
        } else {
            key.to_string()
        };
        Ok(chord)
    }

    /// The text for the file: `Cmd+Shift+T`.
    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        for (on, name) in [
            (self.cmd, "Cmd"),
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ] {
            if on {
                parts.push(name);
            }
        }
        parts.push(&self.key);
        parts.join("+")
    }
}

/// The user's shortcuts; `None` when there is no file yet (use the
/// built-in ones).
pub fn read_keymap(config_dir: &Path) -> Result<Option<Keymap>, String> {
    let path = config_dir.join(KEYMAP_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => crate::from_toml_lenient::<Keymap>(&text)
            .map(|(mut keymap, unknown)| {
                keymap.unknown = unknown;
                Some(keymap)
            })
            .map_err(|e| format!("{KEYMAP_FILE}: {}", e.message())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{KEYMAP_FILE}: {e}")),
    }
}

pub fn write_keymap(config_dir: &Path, keymap: &Keymap) -> Result<(), String> {
    let text = format!(
        "# PrzmTerm keyboard shortcuts. Cmd is ⌘ on macOS and Ctrl elsewhere.\n{}",
        toml::to_string(keymap).map_err(|e| e.to_string())?
    );
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    std::fs::write(config_dir.join(KEYMAP_FILE), text).map_err(|e| e.to_string())
}

/// Deletes the file, so the built-in shortcuts apply again.
pub fn reset_keymap(config_dir: &Path) -> Result<(), String> {
    match std::fs::remove_file(config_dir.join(KEYMAP_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_parse_and_print() {
        let c = Chord::parse("cmd+shift+t").unwrap();
        assert!(c.cmd && c.shift && !c.ctrl && !c.alt);
        assert_eq!(c.key, "T");
        assert_eq!(c.text(), "Cmd+Shift+T");
        assert_eq!(
            Chord::parse("Shift+Ctrl+Tab").unwrap().text(),
            "Ctrl+Shift+Tab"
        );
        assert_eq!(Chord::parse("F5").unwrap().text(), "F5");
        assert_eq!(Chord::parse("Cmd++").unwrap().key, "Plus");
        assert!(Chord::parse("Hyper+T").is_err());
        assert!(Chord::parse("Cmd+").is_err());
    }

    #[test]
    fn bindings_do_one_thing_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_keymap(dir.path()).unwrap(), None);
        let keymap = Keymap {
            bindings: vec![
                Binding {
                    keys: "Cmd+Shift+T".into(),
                    command: "new_tab".into(),
                    ..Default::default()
                },
                Binding {
                    keys: "F5".into(),
                    send: "show clock\\r".into(),
                    ..Default::default()
                },
            ],
            ..Keymap::default()
        };
        write_keymap(dir.path(), &keymap).unwrap();
        let text = std::fs::read_to_string(dir.path().join(KEYMAP_FILE)).unwrap();
        assert!(text.contains("send = 'show clock\\r'"), "{text}");
        assert_eq!(read_keymap(dir.path()).unwrap(), Some(keymap.clone()));
        assert_eq!(keymap.bindings[0].does(), Ok(Does::Command("new_tab")));
        assert_eq!(keymap.bindings[1].does(), Ok(Does::Send("show clock\\r")));
        let both = Binding {
            keys: "F1".into(),
            send: "x".into(),
            script: "y".into(),
            ..Default::default()
        };
        assert!(both.does().is_err());
        assert!(Binding::default().does().is_err());
        reset_keymap(dir.path()).unwrap();
        assert_eq!(read_keymap(dir.path()).unwrap(), None);
    }
}
