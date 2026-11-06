//! The button bar: macro buttons grouped in tabs, saved in
//! `<config>/buttons.toml`:
//!
//! ```toml
//! [[tab]]
//! name = "Cisco"
//!
//! [[tab.button]]
//! label = "Interfaces"
//! send = 'show ip interface brief\r'
//!
//! [[tab.button]]
//! label = "Save"
//! send = 'copy run start\r\p\r'   # \p waits a second
//! description = "Saves the running configuration"
//! ```
//!
//! A button sends to the session that has the keyboard. `tooltips = false`
//! at the top of the file stops the descriptions from popping up.
//!
//! In `send`, `\r` is Enter, `\n` a line feed, `\t` Tab, `\e` Escape,
//! `\p` a one-second pause and `\\` a backslash. Write `send` in single
//! quotes so TOML keeps the backslashes for PrzmTerm.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::SessionError;

pub const BUTTONS_FILE: &str = "buttons.toml";
/// How long `\p` waits.
pub const PAUSE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacroButton {
    pub label: String,
    /// Text to send to the current session, with the escapes above.
    #[serde(default)]
    pub send: String,
    /// Shown when the pointer rests on the button.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Runs this script (see [`crate::script_path`]) instead of sending text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub script: String,
    /// Accepted and ignored: written by a development build of 0.6.
    #[doc(hidden)]
    #[serde(default, rename = "target", skip_serializing)]
    pub legacy_target: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonTab {
    pub name: String,
    #[serde(default, rename = "button", skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<MacroButton>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonBar {
    /// Show a button's description (or what it sends) when the pointer
    /// rests on it.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub tooltips: bool,
    #[serde(default, rename = "tab")]
    pub tabs: Vec<ButtonTab>,
}

impl Default for ButtonBar {
    fn default() -> Self {
        Self {
            tooltips: true,
            tabs: Vec::new(),
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// A piece of what a button sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    Bytes(Vec<u8>),
    Pause(Duration),
}

/// Turns `send` into bytes and pauses. Unknown escapes are sent as typed.
pub fn parse_send(text: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut bytes = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            bytes.extend(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => bytes.push(b'\r'),
            Some('n') => bytes.push(b'\n'),
            Some('t') => bytes.push(b'\t'),
            Some('e') => bytes.push(0x1b),
            Some('\\') => bytes.push(b'\\'),
            Some('p') => {
                if !bytes.is_empty() {
                    chunks.push(Chunk::Bytes(std::mem::take(&mut bytes)));
                }
                chunks.push(Chunk::Pause(PAUSE));
            }
            Some(other) => {
                bytes.push(b'\\');
                let mut buf = [0u8; 4];
                bytes.extend(other.encode_utf8(&mut buf).as_bytes());
            }
            None => bytes.push(b'\\'),
        }
    }
    if !bytes.is_empty() {
        chunks.push(Chunk::Bytes(bytes));
    }
    chunks
}

/// Reads `<config>/buttons.toml`; a missing file is an empty bar.
pub fn read_buttons(config_dir: &Path) -> Result<ButtonBar, SessionError> {
    let path = config_dir.join(BUTTONS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|source| SessionError::Parse { path, source }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ButtonBar::default()),
        Err(source) => Err(SessionError::Io { path, source }),
    }
}

pub fn write_buttons(config_dir: &Path, bar: &ButtonBar) -> Result<(), SessionError> {
    let path = config_dir.join(BUTTONS_FILE);
    std::fs::create_dir_all(config_dir).map_err(|source| SessionError::Io {
        path: config_dir.to_path_buf(),
        source,
    })?;
    std::fs::write(&path, toml::to_string(bar).unwrap_or_default())
        .map_err(|source| SessionError::Io { path, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_escapes_and_pauses() {
        assert_eq!(
            parse_send("show run\\r"),
            [Chunk::Bytes(b"show run\r".to_vec())]
        );
        assert_eq!(
            parse_send("copy run start\\r\\p\\r"),
            [
                Chunk::Bytes(b"copy run start\r".to_vec()),
                Chunk::Pause(PAUSE),
                Chunk::Bytes(b"\r".to_vec()),
            ]
        );
        assert_eq!(
            parse_send("a\\tb\\e\\\\c\\x"),
            [Chunk::Bytes(b"a\tb\x1b\\c\\x".to_vec())]
        );
        assert_eq!(parse_send("ç"), [Chunk::Bytes("ç".as_bytes().to_vec())]);
    }

    #[test]
    fn round_trips_the_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_buttons(dir.path()).unwrap(), ButtonBar::default());
        let bar = ButtonBar {
            tooltips: false,
            tabs: vec![ButtonTab {
                name: "Cisco".into(),
                buttons: vec![
                    MacroButton {
                        label: "Interfaces".into(),
                        send: "show ip int brief\\r".into(),
                        ..MacroButton::default()
                    },
                    MacroButton {
                        label: "Save".into(),
                        send: "wr\\r".into(),
                        description: "Saves the configuration".into(),
                        ..MacroButton::default()
                    },
                ],
            }],
        };
        write_buttons(dir.path(), &bar).unwrap();
        let text = std::fs::read_to_string(dir.path().join(BUTTONS_FILE)).unwrap();
        assert!(text.contains("tooltips = false"), "{text}");
        assert!(
            text.contains("description = \"Saves the configuration\""),
            "{text}"
        );
        assert_eq!(read_buttons(dir.path()).unwrap(), bar);
        // Files from before: `target` is accepted and dropped; tooltips on.
        std::fs::write(
            dir.path().join(BUTTONS_FILE),
            "[[tab]]\nname = 'x'\n[[tab.button]]\nlabel = 'a'\nsend = 'b'\ntarget = 'all'\n",
        )
        .unwrap();
        let old = read_buttons(dir.path()).unwrap();
        assert!(old.tooltips);
        assert_eq!(old.tabs[0].buttons[0].send, "b");
    }
}
