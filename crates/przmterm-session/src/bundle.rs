//! Backups and exports: configuration files packed into one `.przm` file
//! (TOML), to restore on this or another machine or to hand to someone.
//!
//! * A **backup** holds the whole configuration: global defaults, folders,
//!   sessions, keyword sets, buttons and `known_hosts`. Restoring it puts
//!   every file back, after saving a copy of the current configuration.
//! * An **export** holds chosen folders and sessions. Importing it adds them
//!   under a new folder of the session tree; nothing is replaced.
//!
//! Passwords are never in either: they live in the system keychain.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::vault::VAULT_FILE;
use crate::{DEFAULTS_FILE, KEYWORDS_DIR, KNOWN_HOSTS_FILE, SESSIONS_DIR, buttons::BUTTONS_FILE};

pub const EXTENSION: &str = "przm";
/// Copies of the configuration taken before a restore.
pub const BACKUPS_DIR: &str = "backups";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Backup,
    Export,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleFile {
    /// Relative, with `/`: from the configuration folder for a backup, from
    /// the exported folders' parent for an export.
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub kind: Kind,
    /// PrzmTerm version that wrote it.
    pub version: String,
    /// `2026-11-02 06:30`.
    pub created: String,
    #[serde(default, rename = "file")]
    pub files: Vec<BundleFile>,
}

/// What a bundle holds, for the confirmation before restoring or importing.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub sessions: usize,
    pub folders: usize,
    pub keyword_sets: usize,
    /// The password vault is in it (still encrypted).
    pub vault: bool,
    pub other: usize,
}

impl Bundle {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            version: przmterm_core::VERSION.to_string(),
            created: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            files: Vec::new(),
        }
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary::default();
        let session_prefix = format!("{SESSIONS_DIR}/");
        for f in &self.files {
            let path = match self.kind {
                Kind::Backup => match f.path.strip_prefix(&session_prefix) {
                    Some(rest) => rest,
                    None if f.path.starts_with(&format!("{KEYWORDS_DIR}/")) => {
                        s.keyword_sets += 1;
                        continue;
                    }
                    None if f.path == VAULT_FILE => {
                        s.vault = true;
                        continue;
                    }
                    None => {
                        s.other += 1;
                        continue;
                    }
                },
                Kind::Export => f.path.as_str(),
            };
            if path.ends_with(crate::FOLDER_FILE) {
                s.folders += 1;
            } else {
                s.sessions += 1;
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    pub fn from_text(text: &str) -> Result<Bundle, String> {
        let bundle: Bundle =
            toml::from_str(text).map_err(|e| format!("not a PrzmTerm backup or export: {e}"))?;
        for f in &bundle.files {
            safe_path(&f.path)?;
        }
        Ok(bundle)
    }
}

/// Refuses absolute paths and `..`, so a bundle cannot write outside the
/// configuration.
fn safe_path(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    if path.is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("unsafe path in the file: `{path}`"));
    }
    Ok(p.to_path_buf())
}

fn add_file(bundle: &mut Bundle, base: &Path, file: &Path) -> Result<(), String> {
    let rel = file
        .strip_prefix(base)
        .map_err(|_| format!("{} is outside {}", file.display(), base.display()))?;
    let path = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let content = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    bundle.files.push(BundleFile { path, content });
    Ok(())
}

/// Every `.toml` under `dir`, sorted.
fn toml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    files_with(dir, "toml", out);
}

fn files_with(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files_with(&path, extension, out);
        } else if path.extension().is_some_and(|e| e == extension) {
            out.push(path);
        }
    }
}

/// Packs the whole configuration. The password vault goes in as it is on
/// disk: encrypted, and opened with the same master password.
pub fn backup(config_dir: &Path) -> Result<Bundle, String> {
    let mut bundle = Bundle::new(Kind::Backup);
    for name in [
        DEFAULTS_FILE,
        BUTTONS_FILE,
        KNOWN_HOSTS_FILE,
        VAULT_FILE,
        crate::keymap::KEYMAP_FILE,
    ] {
        let path = config_dir.join(name);
        if path.is_file() {
            add_file(&mut bundle, config_dir, &path)?;
        }
    }
    let mut files = Vec::new();
    toml_files(&config_dir.join(SESSIONS_DIR), &mut files);
    toml_files(&config_dir.join(KEYWORDS_DIR), &mut files);
    files_with(&config_dir.join(crate::SCRIPTS_DIR), "rhai", &mut files);
    for file in files {
        add_file(&mut bundle, config_dir, &file)?;
    }
    Ok(bundle)
}

/// Packs chosen session files and folders (with everything inside),
/// keeping each folder's name.
pub fn export(paths: &[PathBuf]) -> Result<Bundle, String> {
    let mut bundle = Bundle::new(Kind::Export);
    for path in paths {
        let base = path.parent().unwrap_or(path);
        if path.is_dir() {
            let mut files = Vec::new();
            toml_files(path, &mut files);
            for file in files {
                add_file(&mut bundle, base, &file)?;
            }
        } else {
            add_file(&mut bundle, base, path)?;
        }
    }
    // The same file reached twice (a session inside an exported folder).
    bundle.files.dedup_by(|a, b| a.path == b.path);
    Ok(bundle)
}

pub fn write_bundle(path: &Path, bundle: &Bundle) -> Result<(), String> {
    std::fs::write(path, bundle.to_text()).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn read_bundle(path: &Path) -> Result<Bundle, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Bundle::from_text(&text)
}

/// Restores a backup over the configuration. A backup of the current
/// configuration is written first to `<config>/backups/`; its path is
/// returned with the number of files restored.
pub fn restore(config_dir: &Path, bundle: &Bundle) -> Result<(usize, PathBuf), String> {
    if bundle.kind != Kind::Backup {
        return Err("this file is an export; import it instead".into());
    }
    let safety_dir = config_dir.join(BACKUPS_DIR);
    std::fs::create_dir_all(&safety_dir).map_err(|e| e.to_string())?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let safety = safety_dir.join(format!("before-restore-{stamp}.{EXTENSION}"));
    write_bundle(&safety, &backup(config_dir)?)?;
    for f in &bundle.files {
        let path = config_dir.join(safe_path(&f.path)?);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, &f.content).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok((bundle.files.len(), safety))
}

/// Adds an export's folders and sessions in a new folder `<into>` of the
/// session tree; when that name is taken it becomes `<into> (2)`, `(3)`…,
/// so nothing already there is touched. Returns how many files were written
/// and the folder used.
pub fn import(config_dir: &Path, bundle: &Bundle, into: &str) -> Result<(usize, String), String> {
    let into = match into.trim() {
        "" => "Imported",
        name => name,
    };
    crate::validate_name(into)?;
    let sessions = config_dir.join(SESSIONS_DIR);
    let mut folder = into.to_string();
    let mut n = 2;
    while sessions.join(&folder).exists() || sessions.join(format!("{folder}.toml")).exists() {
        folder = format!("{into} ({n})");
        n += 1;
    }
    let base = sessions.join(&folder);
    let files: Vec<&BundleFile> = match bundle.kind {
        Kind::Export => bundle.files.iter().collect(),
        // A backup can be imported too: only its sessions.
        Kind::Backup => bundle
            .files
            .iter()
            .filter(|f| f.path.starts_with(&format!("{SESSIONS_DIR}/")))
            .collect(),
    };
    for f in &files {
        let rel = match bundle.kind {
            Kind::Export => safe_path(&f.path)?,
            Kind::Backup => safe_path(&f.path[SESSIONS_DIR.len() + 1..])?,
        };
        let path = base.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, &f.content).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok((files.len(), folder))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn sample(dir: &Path) {
        write(dir, "defaults.toml", "keepalive = 20\n");
        write(dir, "known_hosts", "host ssh-ed25519 AAAA\n");
        write(dir, "buttons.toml", "[[tab]]\nname = 'x'\n");
        write(dir, "sessions/core.toml", "host = '10.0.0.1'\n");
        write(dir, "sessions/Lab/folder.toml", "username = 'ops'\n");
        write(dir, "sessions/Lab/sw1.toml", "host = '10.0.1.1'\n");
        write(dir, "keywords/CORES.toml", "[[rule]]\ntext = 'up'\n");
        write(dir, "scripts/login.rhai", "session.send(\"ops\\r\");\n");
        write(dir, "logs/old.log", "not in backups\n");
    }

    #[test]
    fn backup_and_restore_on_another_machine() {
        let here = tempfile::tempdir().unwrap();
        sample(here.path());
        let bundle = backup(here.path()).unwrap();
        assert_eq!(
            bundle.summary(),
            Summary {
                sessions: 2,
                folders: 1,
                keyword_sets: 1,
                vault: false,
                other: 4
            }
        );
        assert!(!bundle.files.iter().any(|f| f.path.starts_with("logs")));

        let file = here.path().join("all.przm");
        write_bundle(&file, &bundle).unwrap();
        let read = read_bundle(&file).unwrap();
        assert_eq!(read, bundle);

        let there = tempfile::tempdir().unwrap();
        write(there.path(), "sessions/core.toml", "host = 'old'\n");
        let (n, safety) = restore(there.path(), &read).unwrap();
        assert_eq!(n, 8);
        assert!(there.path().join("scripts/login.rhai").is_file());
        assert_eq!(
            std::fs::read_to_string(there.path().join("sessions/core.toml")).unwrap(),
            "host = '10.0.0.1'\n"
        );
        // The replaced configuration was kept.
        let before = read_bundle(&safety).unwrap();
        assert_eq!(before.files[0].content, "host = 'old'\n");
        let tree = crate::SessionTree::load(there.path());
        assert!(tree.errors.is_empty());
        assert_eq!(tree.root.all_sessions().len(), 2);
    }

    #[test]
    fn the_vault_travels_encrypted_in_backups_only() {
        use crate::vault::{KdfCost, Vault, VaultEntry, vault_path};
        let here = tempfile::tempdir().unwrap();
        sample(here.path());
        let mut vault = Vault::create(
            &vault_path(here.path()),
            "correct horse",
            KdfCost::for_tests(),
        )
        .unwrap();
        vault
            .put(
                None,
                VaultEntry {
                    name: "NOC".into(),
                    username: "noc".into(),
                    password: "s3cret!".into(),
                    passphrase: String::new(),
                },
            )
            .unwrap();
        let bundle = backup(here.path()).unwrap();
        assert!(bundle.summary().vault);
        assert!(!bundle.to_text().contains("s3cret"));
        let there = tempfile::tempdir().unwrap();
        restore(there.path(), &bundle).unwrap();
        let vault = Vault::open(&vault_path(there.path()), "correct horse").unwrap();
        assert_eq!(vault.get("NOC").unwrap().password, "s3cret!");

        let sessions = here.path().join("sessions");
        let shared = export(std::slice::from_ref(&sessions)).unwrap();
        assert!(!shared.to_text().contains(VAULT_FILE));
    }

    #[test]
    fn export_and_import_into_a_new_folder() {
        let mine = tempfile::tempdir().unwrap();
        sample(mine.path());
        let sessions = mine.path().join("sessions");
        let bundle = export(&[sessions.join("Lab"), sessions.join("core.toml")]).unwrap();
        let paths: Vec<&str> = bundle.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["Lab/folder.toml", "Lab/sw1.toml", "core.toml"]);
        assert!(
            restore(mine.path(), &bundle).is_err(),
            "an export is not a backup"
        );

        let theirs = tempfile::tempdir().unwrap();
        write(
            theirs.path(),
            "sessions/From Rodrigo/core.toml",
            "host = 'theirs'\n",
        );
        assert_eq!(
            import(theirs.path(), &bundle, "From Rodrigo").unwrap(),
            (3, "From Rodrigo (2)".to_string()),
            "a new folder, next to the one already there"
        );
        let tree = crate::SessionTree::load(theirs.path());
        assert!(tree.errors.is_empty(), "{:?}", tree.errors);
        let names: Vec<String> = tree
            .root
            .all_sessions()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(
            names.iter().filter(|n| *n == "core").count(),
            2,
            "{names:?}"
        );
        let theirs_core = tree
            .root
            .all_sessions()
            .into_iter()
            .find(|s| s.folder == ["From Rodrigo"])
            .unwrap();
        assert!(
            theirs_core.display_target().contains("theirs"),
            "nothing replaced"
        );
        let sw1 = tree
            .root
            .all_sessions()
            .into_iter()
            .find(|s| s.name == "sw1")
            .unwrap();
        assert_eq!(sw1.folder, ["From Rodrigo (2)", "Lab"]);
        assert_eq!(
            sw1.display_target(),
            "ops@10.0.1.1:22",
            "the folder settings came along"
        );
    }

    #[test]
    fn refuses_unsafe_paths() {
        let text = "kind = 'export'\nversion = '1'\ncreated = 'x'\n\
                    [[file]]\npath = '../../etc/passwd'\ncontent = 'x'\n";
        assert!(Bundle::from_text(text).unwrap_err().contains("unsafe"));
        let abs = text.replace("../../etc/passwd", "/etc/passwd");
        assert!(Bundle::from_text(&abs).is_err());
    }
}
