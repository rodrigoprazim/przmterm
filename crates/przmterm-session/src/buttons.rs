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
//! label = "Save all"
//! send = 'copy run start\r\p\r'   # \p waits a second
//! target = "all"                 # current (default), tab or all
//! ```
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// The pane with the keyboard focus.
    #[default]
    Current,
    /// Every connected pane of the current tab.
    Tab,
    /// Every connected pane of every tab.
    All,
}

impl Target {
    pub const ALL: [Target; 3] = [Target::Current, Target::Tab, Target::All];

    pub fn label(self) -> &'static str {
        match self {
            Target::Current => "current session",
            Target::Tab => "all sessions in the tab",
            Target::All => "all sessions",
        }
    }
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacroButton {
    pub label: String,
    /// Text to send, with the escapes above.
    #[serde(default)]
    pub send: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub target: Target,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonTab {
    pub name: String,
    #[serde(default, rename = "button", skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<MacroButton>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ButtonBar {
    #[serde(default, rename = "tab")]
    pub tabs: Vec<ButtonTab>,
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
            tabs: vec![ButtonTab {
                name: "Cisco".into(),
                buttons: vec![
                    MacroButton {
                        label: "Interfaces".into(),
                        send: "show ip int brief\\r".into(),
                        target: Target::Current,
                    },
                    MacroButton {
                        label: "Save all".into(),
                        send: "wr\\r".into(),
                        target: Target::All,
                    },
                ],
            }],
        };
        write_buttons(dir.path(), &bar).unwrap();
        let text = std::fs::read_to_string(dir.path().join(BUTTONS_FILE)).unwrap();
        assert!(
            text.contains("[[tab.button]]") && text.contains("target = \"all\""),
            "{text}"
        );
        assert_eq!(read_buttons(dir.path()).unwrap(), bar);
    }
}
