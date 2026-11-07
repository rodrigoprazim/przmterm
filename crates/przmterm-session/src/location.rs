//! Where PrzmTerm keeps its settings.
//!
//! By default that is the system's folder for application settings
//! (`~/.config/przmterm`, `~/Library/Application Support/przmterm`,
//! `%APPDATA%\przmterm`). Another folder can be chosen, a synced one for
//! instance: its path is then kept in `location.toml` in the default folder,
//! which is the only file left there. `$PRZMTERM_HOME` overrides both.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::SessionError;

/// In the standard folder: the folder chosen instead.
pub const LOCATION_FILE: &str = "location.toml";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Location {
    path: PathBuf,
}

/// The system's settings folder for PrzmTerm.
pub fn standard_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("przmterm"))
}

/// `$PRZMTERM_HOME`, when set: it wins over any choice.
pub fn env_dir() -> Option<PathBuf> {
    std::env::var_os("PRZMTERM_HOME").map(PathBuf::from)
}

/// The folder chosen instead of `standard`, if one was.
pub fn chosen_dir(standard: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(standard.join(LOCATION_FILE)).ok()?;
    // A broken file is read as no choice: the standard folder still works.
    let location = toml::from_str::<Location>(&text).ok()?;
    let path = location.path.to_string_lossy();
    (!path.trim().is_empty()).then(|| crate::expand_tilde(path.trim()))
}

/// The settings folder: the chosen one, else `standard`.
pub fn resolve(standard: &Path) -> PathBuf {
    chosen_dir(standard).unwrap_or_else(|| standard.to_path_buf())
}

/// Keeps `dir` as the settings folder; `None` (or `standard` itself) goes
/// back to `standard`.
pub fn choose(standard: &Path, dir: Option<&Path>) -> Result<(), SessionError> {
    let file = standard.join(LOCATION_FILE);
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| SessionError::Io { path, source }
    };
    match dir.filter(|d| !same_dir(d, standard)) {
        None => match fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(&file)(e)),
            _ => Ok(()),
        },
        Some(dir) => {
            fs::create_dir_all(standard).map_err(io(standard))?;
            let text = toml::to_string(&Location {
                path: dir.to_path_buf(),
            })
            .unwrap_or_default();
            fs::write(&file, text).map_err(io(&file))
        }
    }
}

/// What a folder holds, for choosing it as the settings folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contents {
    /// Nothing (or only hidden files): the settings can be copied in.
    Empty,
    /// PrzmTerm settings: they are used as they are.
    Settings,
    /// Other files: not a place for PrzmTerm's.
    Other,
}

/// Files and folders that mark PrzmTerm settings.
const MARKERS: [&str; 5] = [
    crate::SESSIONS_DIR,
    crate::DEFAULTS_FILE,
    crate::buttons::BUTTONS_FILE,
    crate::keymap::KEYMAP_FILE,
    crate::vault::VAULT_FILE,
];

pub fn contents(dir: &Path) -> Contents {
    let Ok(entries) = fs::read_dir(dir) else {
        return Contents::Empty; // created when chosen
    };
    let mut empty = true;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if MARKERS.contains(&name.as_ref()) {
            return Contents::Settings;
        }
        if !name.starts_with('.') && name != LOCATION_FILE {
            empty = false;
        }
    }
    if empty {
        Contents::Empty
    } else {
        Contents::Other
    }
}

/// Whether `a` and `b` are the same folder.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Whether `inner` is `outer` or a folder inside it.
pub fn is_inside(inner: &Path, outer: &Path) -> bool {
    let canon = |p: &Path| {
        // The folder may not exist yet: resolve its nearest parent.
        let mut existing = p.to_path_buf();
        let mut rest = Vec::new();
        while !existing.exists() {
            match (existing.file_name(), existing.parent()) {
                (Some(name), Some(parent)) => {
                    rest.push(name.to_os_string());
                    existing = parent.to_path_buf();
                }
                _ => break,
            }
        }
        let mut full = fs::canonicalize(&existing).unwrap_or(existing);
        full.extend(rest.iter().rev());
        full
    };
    canon(inner).starts_with(canon(outer))
}

/// Copies the settings in `from` to `to` (every file and folder but
/// `location.toml`), leaving files already in `to` alone. Returns how many
/// files were copied.
pub fn copy_settings(from: &Path, to: &Path) -> Result<usize, SessionError> {
    fn copy(from: &Path, to: &Path, top: bool, count: &mut usize) -> Result<(), SessionError> {
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |source| SessionError::Io { path, source }
        };
        fs::create_dir_all(to).map_err(io(to))?;
        for entry in fs::read_dir(from).map_err(io(from))? {
            let entry = entry.map_err(io(from))?;
            let name = entry.file_name();
            if top && name == LOCATION_FILE {
                continue;
            }
            let (src, dst) = (entry.path(), to.join(&name));
            let kind = entry.file_type().map_err(io(&src))?;
            if kind.is_dir() {
                copy(&src, &dst, false, count)?;
            } else if kind.is_file() && !dst.exists() {
                fs::copy(&src, &dst).map_err(io(&dst))?;
                *count += 1;
            }
        }
        Ok(())
    }
    let mut count = 0;
    if from.is_dir() {
        copy(from, to, true, &mut count)?;
    } else {
        fs::create_dir_all(to).map_err(|source| SessionError::Io {
            path: to.to_path_buf(),
            source,
        })?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_folder_is_kept_and_forgotten() {
        let tmp = tempfile::tempdir().unwrap();
        let standard = tmp.path().join("std");
        let synced = tmp.path().join("Dropbox/przmterm");
        assert_eq!(resolve(&standard), standard);
        choose(&standard, Some(&synced)).unwrap();
        assert_eq!(resolve(&standard), synced);
        // Choosing the standard folder forgets the other one.
        choose(&standard, Some(&standard)).unwrap();
        assert_eq!(resolve(&standard), standard);
        assert!(!standard.join(LOCATION_FILE).exists());
        choose(&standard, None).unwrap();
    }

    #[test]
    fn tells_what_a_folder_holds() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(contents(&dir.join("missing")), Contents::Empty);
        fs::write(dir.join(".DS_Store"), "").unwrap();
        assert_eq!(contents(dir), Contents::Empty);
        fs::write(dir.join("notes.txt"), "").unwrap();
        assert_eq!(contents(dir), Contents::Other);
        fs::create_dir(dir.join(crate::SESSIONS_DIR)).unwrap();
        assert_eq!(contents(dir), Contents::Settings);
    }

    #[test]
    fn copies_everything_but_the_location() {
        let tmp = tempfile::tempdir().unwrap();
        let (from, to) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::create_dir_all(from.join("sessions/Lab")).unwrap();
        fs::write(from.join("sessions/Lab/r1.toml"), "host = 'r1'").unwrap();
        fs::write(from.join("defaults.toml"), "").unwrap();
        fs::write(from.join(LOCATION_FILE), "path = 'x'").unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(to.join("defaults.toml"), "port = 2").unwrap();
        assert_eq!(copy_settings(&from, &to).unwrap(), 1);
        assert!(to.join("sessions/Lab/r1.toml").is_file());
        assert!(!to.join(LOCATION_FILE).exists());
        // Files already there are kept.
        assert_eq!(
            fs::read_to_string(to.join("defaults.toml")).unwrap(),
            "port = 2"
        );
    }

    #[test]
    fn knows_folders_inside_others() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        fs::create_dir_all(&a).unwrap();
        assert!(is_inside(&a.join("new/deeper"), &a));
        assert!(is_inside(&a, &a));
        assert!(!is_inside(tmp.path(), &a));
    }
}
