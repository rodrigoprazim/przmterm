//! Preferences of the app itself, not of sessions: `preferences.toml` in the
//! settings folder.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::SessionError;

pub const PREFERENCES_FILE: &str = "preferences.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// Look for a newer release on GitHub when the app starts.
    pub check_for_updates: bool,
    /// The interface's look (not the terminal's colours, which are the
    /// sessions').
    pub look: Look,
    /// A release the user chose to skip: not announced at startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_version: Option<String>,
    /// What the mouse does in the terminal.
    pub mouse: MouseOptions,
    /// The log file has PrzmTerm's steps in detail (*Diagnostics…*).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub debug_log: bool,
}

/// What the mouse does in the terminal (`[mouse]`). Everything is off
/// unless chosen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MouseOptions {
    /// Copy what is selected at once.
    pub copy_on_select: bool,
    /// A mouse button pastes: `paste_button`.
    pub paste: bool,
    /// The button that pastes, when `paste` is on.
    pub paste_button: PasteButton,
    /// Hide the pointer while typing, until it moves.
    pub hide_pointer: bool,
    /// Ctrl+click (⌘+click on a Mac) on a web address opens it.
    pub open_urls: bool,
    /// A triple-click selects the line with its line break.
    pub triple_click_newline: bool,
}

/// The mouse button that pastes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasteButton {
    #[default]
    Middle,
    /// The right button: the terminal then has no right-click menu.
    Right,
}

/// Dark, light, or as the system is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Look {
    #[default]
    Dark,
    Light,
    System,
}

impl Look {
    pub const ALL: [Look; 3] = [Look::Dark, Look::Light, Look::System];

    pub fn label(self) -> &'static str {
        match self {
            Look::Dark => "Dark",
            Look::Light => "Light",
            Look::System => "System",
        }
    }
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            check_for_updates: true,
            look: Look::Dark,
            skipped_version: None,
            mouse: MouseOptions::default(),
            debug_log: false,
        }
    }
}

impl Preferences {
    /// The preferences in `dir`; the defaults when there are none (or the
    /// file is broken: preferences are never worth refusing to start for).
    pub fn load(dir: &Path) -> Self {
        fs::read_to_string(dir.join(PREFERENCES_FILE))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<(), SessionError> {
        let path = dir.join(PREFERENCES_FILE);
        let io = |source| SessionError::Io {
            path: path.clone(),
            source,
        };
        fs::create_dir_all(dir).map_err(io)?;
        fs::write(&path, toml::to_string(self).unwrap_or_default()).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_and_read_back_with_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Preferences::load(tmp.path()).check_for_updates);
        let off = Preferences {
            check_for_updates: false,
            look: Look::Light,
            skipped_version: Some("1.0.0-rc.2".into()),
            mouse: MouseOptions {
                copy_on_select: true,
                paste: true,
                paste_button: PasteButton::Right,
                ..MouseOptions::default()
            },
            debug_log: true,
        };
        assert_eq!(
            Preferences::load(tmp.path()).look,
            Look::Dark,
            "dark when nothing is chosen"
        );
        off.save(tmp.path()).unwrap();
        assert_eq!(Preferences::load(tmp.path()), off);
        fs::write(tmp.path().join(PREFERENCES_FILE), "nonsense = [").unwrap();
        assert_eq!(Preferences::load(tmp.path()), Preferences::default());
    }
}
