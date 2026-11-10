//! Preferences of the app itself, not of sessions: `preferences.toml` in the
//! settings folder.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::SessionError;

pub const PREFERENCES_FILE: &str = "preferences.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    /// Look for a newer release on GitHub when the app starts.
    pub check_for_updates: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            check_for_updates: true,
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
        };
        off.save(tmp.path()).unwrap();
        assert_eq!(Preferences::load(tmp.path()), off);
        fs::write(tmp.path().join(PREFERENCES_FILE), "nonsense = [").unwrap();
        assert_eq!(Preferences::load(tmp.path()), Preferences::default());
    }
}
