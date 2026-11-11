//! The button bar: macro buttons in groups, saved in
//! `<config>/buttons.toml`:
//!
//! ```toml
//! [[group]]
//! name = "Cisco"
//!
//! [[group.button]]
//! label = "Interfaces"
//! send = 'show ip interface brief\r'
//!
//! [[group.button]]
//! label = "Save"
//! send = 'copy run start\r\p\r'   # \p waits a second
//! description = "Saves the running configuration"
//! ```
//!
//! A button sends to the session that has the keyboard. `tooltip = false`
//! on a button stops its description from popping up. Files before 1.0
//! say `[[tab]]` for a group; they are read the same.
//!
//! In `send`, `\r` is Enter, `\n` a line feed, `\t` Tab, `\e` Escape,
//! `\p` a one-second pause and `\\` a backslash. Write `send` in single
//! quotes so TOML keeps the backslashes for PrzmTerm.
//!
//! `{vault:NAME}` types the password of the vault entry NAME, and
//! `{vault:NAME:username}` (or `:passphrase`) its other fields: the file
//! names the entry, the secret stays in the encrypted vault.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::SessionError;

pub const BUTTONS_FILE: &str = "buttons.toml";
/// How long `\p` waits.
pub const PAUSE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// A regular expression: the button sends only when the cursor's line
    /// matches it (the prompt it would answer), so a password is never
    /// typed at the wrong prompt. Empty: always.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub only_when: String,
    /// Show the description (or what the button sends) when the pointer
    /// rests on it.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub tooltip: bool,
    /// Accepted and ignored: written by a development build of 0.6.
    #[doc(hidden)]
    #[serde(default, rename = "target", skip_serializing)]
    pub legacy_target: Option<String>,
}

impl Default for MacroButton {
    fn default() -> Self {
        Self {
            label: String::new(),
            send: String::new(),
            description: String::new(),
            script: String::new(),
            only_when: String::new(),
            tooltip: true,
            legacy_target: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonTab {
    pub name: String,
    #[serde(default, rename = "button", skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<MacroButton>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonBar {
    /// The switch for every button's tooltip in files from 0.8.0: read
    /// into each button's `tooltip`, never written.
    #[doc(hidden)]
    #[serde(default = "yes", rename = "tooltips", skip_serializing)]
    pub legacy_tooltips: bool,
    /// The groups (`[[group]]`, `[[tab]]` before 1.0).
    #[serde(default, rename = "group", alias = "tab")]
    pub tabs: Vec<ButtonTab>,
    /// Keys in the file this version does not know (left out).
    #[serde(skip)]
    pub unknown: Vec<String>,
}

impl Default for ButtonBar {
    fn default() -> Self {
        Self {
            legacy_tooltips: true,
            tabs: Vec::new(),
            unknown: Vec::new(),
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
    /// A field of a vault entry, typed once the vault is open
    /// ([`resolve_vault`]).
    Vault {
        entry: String,
        field: VaultField,
    },
}

/// Which field of a vault entry `{vault:…}` types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultField {
    Username,
    Password,
    Passphrase,
}

impl VaultField {
    fn parse(name: &str) -> Option<VaultField> {
        match name.trim() {
            "username" | "user" => Some(VaultField::Username),
            "password" => Some(VaultField::Password),
            "passphrase" => Some(VaultField::Passphrase),
            _ => None,
        }
    }
}

/// The text that types `field` of the vault entry `entry`.
pub fn vault_reference(entry: &str, field: VaultField) -> String {
    match field {
        VaultField::Password => format!("{{vault:{entry}}}"),
        VaultField::Username => format!("{{vault:{entry}:username}}"),
        VaultField::Passphrase => format!("{{vault:{entry}:passphrase}}"),
    }
}

/// Whether the text of a button or shortcut types from the vault.
pub fn uses_vault(send: &str) -> bool {
    parse_send(send)
        .iter()
        .any(|c| matches!(c, Chunk::Vault { .. }))
}

/// The vault entries `chunks` type from.
pub fn vault_entries(chunks: &[Chunk]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for chunk in chunks {
        if let Chunk::Vault { entry, .. } = chunk
            && !names.contains(entry)
        {
            names.push(entry.clone());
        }
    }
    names
}

/// Puts the vault's values in place of the `{vault:…}` pieces. Fails,
/// naming the entry, when one is not in `entries` or has no such field.
pub fn resolve_vault(
    chunks: &[Chunk],
    entries: &[crate::vault::VaultEntry],
) -> Result<Vec<Chunk>, String> {
    chunks
        .iter()
        .map(|chunk| match chunk {
            Chunk::Vault { entry, field } => {
                let found = entries
                    .iter()
                    .find(|e| e.name == *entry)
                    .ok_or_else(|| format!("the vault has no entry {entry}"))?;
                let (value, what) = match field {
                    VaultField::Username => (&found.username, "user name"),
                    VaultField::Password => (&found.password, "password"),
                    VaultField::Passphrase => (&found.passphrase, "key passphrase"),
                };
                if value.is_empty() {
                    return Err(format!("the vault entry {entry} has no {what}"));
                }
                Ok(Chunk::Bytes(value.as_bytes().to_vec()))
            }
            other => Ok(other.clone()),
        })
        .collect()
}

/// Turns `send` into bytes, pauses and vault references. Unknown escapes
/// are sent as typed, and so is a `{vault:` without its closing `}`.
pub fn parse_send(text: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut bytes = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '{'
            && let Some(rest) = chars.as_str().strip_prefix("vault:")
            && let Some(end) = rest.find('}')
        {
            let inner = &rest[..end];
            let (entry, field) = match inner.rsplit_once(':') {
                Some((name, field)) if VaultField::parse(field).is_some() => (
                    name,
                    VaultField::parse(field).unwrap_or(VaultField::Password),
                ),
                _ => (inner, VaultField::Password),
            };
            if !bytes.is_empty() {
                chunks.push(Chunk::Bytes(std::mem::take(&mut bytes)));
            }
            chunks.push(Chunk::Vault {
                entry: entry.trim().to_string(),
                field,
            });
            chars = rest[end + 1..].chars();
            continue;
        }
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
        Ok(text) => {
            let (mut bar, unknown) = crate::from_toml_lenient::<ButtonBar>(&text)
                .map_err(|source| SessionError::Parse { path, source })?;
            bar.unknown = unknown;
            if !bar.legacy_tooltips {
                for button in bar.tabs.iter_mut().flat_map(|t| &mut t.buttons) {
                    button.tooltip = false;
                }
                bar.legacy_tooltips = true;
            }
            Ok(bar)
        }
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
    #[allow(clippy::field_reassign_with_default)] // VaultEntry is Drop
    fn vault_references_name_the_entry_and_are_filled_in() {
        let chunks = parse_send("enable\\r\\p{vault:Core sw}\\r{vault:Lab:username}{vault:x");
        assert_eq!(
            chunks,
            [
                Chunk::Bytes(b"enable\r".to_vec()),
                Chunk::Pause(PAUSE),
                Chunk::Vault {
                    entry: "Core sw".into(),
                    field: VaultField::Password
                },
                Chunk::Bytes(b"\r".to_vec()),
                Chunk::Vault {
                    entry: "Lab".into(),
                    field: VaultField::Username
                },
                Chunk::Bytes(b"{vault:x".to_vec()),
            ]
        );
        assert_eq!(vault_entries(&chunks), ["Core sw", "Lab"]);
        assert_eq!(
            parse_send(&vault_reference("A:b", VaultField::Password)),
            [Chunk::Vault {
                entry: "A:b".into(),
                field: VaultField::Password
            }]
        );

        let entry = |name: &str, user: &str, password: &str| {
            let mut e = crate::vault::VaultEntry::default();
            e.name = name.into();
            e.username = user.into();
            e.password = password.into();
            e
        };
        let vault = [entry("Core sw", "", "s3cret"), entry("Lab", "ops", "x")];
        let typed = resolve_vault(&chunks[..5], &vault).unwrap();
        assert_eq!(typed[2], Chunk::Bytes(b"s3cret".to_vec()));
        assert_eq!(typed[4], Chunk::Bytes(b"ops".to_vec()));
        let err = resolve_vault(&parse_send("{vault:Core sw:username}"), &vault).unwrap_err();
        assert!(err.contains("no user name"), "{err}");
        let err = resolve_vault(&parse_send("{vault:Gone}"), &vault).unwrap_err();
        assert!(err.contains("no entry Gone"), "{err}");
    }

    #[test]
    fn knows_what_types_from_the_vault() {
        assert!(uses_vault("enable\\r\\p{vault:Core sw}\\r"));
        assert!(uses_vault("{vault:Lab:username}"));
        assert!(!uses_vault("show run\\r"));
        assert!(!uses_vault("{vault:"));
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
                        ..MacroButton::default()
                    },
                    MacroButton {
                        label: "Save".into(),
                        send: "wr\\r".into(),
                        description: "Saves the configuration".into(),
                        tooltip: false,
                        ..MacroButton::default()
                    },
                    MacroButton {
                        label: "Enable".into(),
                        send: "{vault:Core}\\r".into(),
                        only_when: "(?i)password:".into(),
                        ..MacroButton::default()
                    },
                ],
            }],
            ..ButtonBar::default()
        };
        write_buttons(dir.path(), &bar).unwrap();
        let text = std::fs::read_to_string(dir.path().join(BUTTONS_FILE)).unwrap();
        // Only the button without a tooltip says so.
        assert_eq!(text.matches("tooltip = false").count(), 1, "{text}");
        assert!(!text.contains("tooltips"), "{text}");
        assert!(
            text.contains("description = \"Saves the configuration\""),
            "{text}"
        );
        assert!(text.starts_with("[[group]]"), "{text}");
        // Only the guarded button has only_when.
        assert_eq!(text.matches("only_when").count(), 1, "{text}");
        assert_eq!(read_buttons(dir.path()).unwrap(), bar);
        // Unknown keys are left out and noted.
        std::fs::write(
            dir.path().join(BUTTONS_FILE),
            "[[group]]\nname = 'x'\ncolor = 'red'\n[[group.button]]\nlabel = 'a'\n",
        )
        .unwrap();
        let newer = read_buttons(dir.path()).unwrap();
        assert_eq!(newer.tabs[0].buttons[0].label, "a");
        assert_eq!(newer.unknown, ["group.0.color"]);
        // Files from before: `target` is accepted and dropped; tooltips on.
        std::fs::write(
            dir.path().join(BUTTONS_FILE),
            "[[tab]]\nname = 'x'\n[[tab.button]]\nlabel = 'a'\nsend = 'b'\ntarget = 'all'\n",
        )
        .unwrap();
        let old = read_buttons(dir.path()).unwrap();
        assert!(old.tabs[0].buttons[0].tooltip);
        assert_eq!(old.tabs[0].buttons[0].send, "b");
        // 0.8.0's switch for the whole bar turns off every button's tooltip.
        std::fs::write(
            dir.path().join(BUTTONS_FILE),
            "tooltips = false\n[[tab]]\nname = 'x'\n[[tab.button]]\nlabel = 'a'\n\
             [[tab.button]]\nlabel = 'b'\n",
        )
        .unwrap();
        let old = read_buttons(dir.path()).unwrap();
        assert!(old.tabs[0].buttons.iter().all(|b| !b.tooltip));
        write_buttons(dir.path(), &old).unwrap();
        let text = std::fs::read_to_string(dir.path().join(BUTTONS_FILE)).unwrap();
        assert!(!text.contains("tooltips"), "{text}");
        assert_eq!(text.matches("tooltip = false").count(), 2, "{text}");
    }
}
