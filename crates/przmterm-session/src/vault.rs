//! The password vault: user names, passwords and key passphrases in one
//! encrypted file, opened with a master password.
//!
//! The file is TOML. Its header says how the key was made (Argon2id with
//! its salt and costs); `data` is the entry list, itself TOML, encrypted
//! with XChaCha20-Poly1305 under that key. The header is authenticated as
//! well, so changing the costs or salt makes the file fail to open rather
//! than open with weaker settings. A wrong master password and a damaged
//! file look the same: the data fails to authenticate.
//!
//! Sessions and folders name an entry (`credential = "NAME"`); the entry's
//! user name, password and key passphrase are then used to log in.

use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

/// The vault, in the configuration folder.
pub const VAULT_FILE: &str = "vault.toml";
const FORMAT: &str = "przmterm-vault";
const VERSION: u32 = 1;
const SALT_LEN: usize = 16;
/// Master passwords shorter than this are refused.
pub const MIN_MASTER_LEN: usize = 8;

/// Argon2id costs. The defaults take about half a second on a laptop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfCost {
    /// Memory in KiB.
    pub memory: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl Default for KdfCost {
    fn default() -> Self {
        Self {
            memory: 64 * 1024,
            iterations: 3,
            parallelism: 1,
        }
    }
}

impl KdfCost {
    /// Cheap costs for tests only.
    #[doc(hidden)]
    pub fn for_tests() -> Self {
        Self {
            memory: 64,
            iterations: 1,
            parallelism: 1,
        }
    }
}

/// One credential.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    /// For encrypted private keys.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub passphrase: String,
}

impl std::fmt::Debug for VaultEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultEntry")
            .field("name", &self.name)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

impl Drop for VaultEntry {
    fn drop(&mut self) {
        self.password.zeroize();
        self.passphrase.zeroize();
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Entries {
    #[serde(default, rename = "entry")]
    entries: Vec<VaultEntry>,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    version: u32,
    kdf: String,
    cost: KdfCost,
    salt: String,
    nonce: String,
    data: String,
}

impl Envelope {
    /// What the encryption authenticates besides the data.
    fn header(&self) -> Vec<u8> {
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.format,
            self.version,
            self.kdf,
            self.cost.memory,
            self.cost.iterations,
            self.cost.parallelism,
            self.salt
        )
        .into_bytes()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("wrong master password, or the vault file is damaged")]
    WrongPassword,
    #[error("the master password needs at least {MIN_MASTER_LEN} characters")]
    WeakPassword,
    #[error("{0}: not a PrzmTerm vault ({1})")]
    Format(PathBuf, String),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("an entry named {0} already exists")]
    Duplicate(String),
    #[error("give the entry a name")]
    NoName,
}

/// An open vault: the entries and the key to save them again.
pub struct Vault {
    path: PathBuf,
    key: Zeroizing<[u8; 32]>,
    cost: KdfCost,
    salt: [u8; SALT_LEN],
    pub entries: Vec<VaultEntry>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("entries", &self.entries)
            .finish_non_exhaustive()
    }
}

/// A password kept only in memory, to check it again later (a locked tab):
/// its Argon2id key with a random salt, never the password itself.
pub struct Lock {
    salt: [u8; SALT_LEN],
    key: Zeroizing<[u8; 32]>,
}

impl Lock {
    /// Lighter than the vault's: the key never leaves the process.
    const COST: KdfCost = KdfCost {
        memory: 19 * 1024,
        iterations: 2,
        parallelism: 1,
    };

    pub fn new(password: &str) -> Result<Lock, String> {
        let salt = <[u8; SALT_LEN]>::generate();
        let key = derive(password, &salt, Self::COST)?;
        Ok(Lock { salt, key })
    }

    /// Whether `password` is the one it was made with.
    pub fn opens(&self, password: &str) -> bool {
        let Ok(key) = derive(password, &self.salt, Self::COST) else {
            return false;
        };
        // Every byte compared, whatever the first difference.
        key.iter()
            .zip(self.key.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl std::fmt::Debug for Lock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lock")
    }
}

/// Where the vault lives.
pub fn vault_path(config_dir: &Path) -> PathBuf {
    config_dir.join(VAULT_FILE)
}

fn derive(master: &str, salt: &[u8], cost: KdfCost) -> Result<Zeroizing<[u8; 32]>, String> {
    let params = Params::new(cost.memory, cost.iterations, cost.parallelism, Some(32))
        .map_err(|e| e.to_string())?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(master.as_bytes(), salt, key.as_mut())
        .map_err(|e| e.to_string())?;
    Ok(key)
}

impl Vault {
    /// A new, empty vault at `path` (written at once).
    pub fn create(path: &Path, master: &str, cost: KdfCost) -> Result<Vault, VaultError> {
        if master.chars().count() < MIN_MASTER_LEN {
            return Err(VaultError::WeakPassword);
        }
        let salt = <[u8; SALT_LEN]>::generate();
        let key = derive(master, &salt, cost).map_err(|e| VaultError::Format(path.into(), e))?;
        let vault = Vault {
            path: path.to_path_buf(),
            key,
            cost,
            salt,
            entries: Vec::new(),
        };
        vault.save()?;
        Ok(vault)
    }

    /// Opens the vault at `path` with the master password.
    pub fn open(path: &Path, master: &str) -> Result<Vault, VaultError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| VaultError::Io(path.to_path_buf(), e))?;
        let bad = |why: &str| VaultError::Format(path.to_path_buf(), why.to_string());
        let env: Envelope = toml::from_str(&text).map_err(|e| bad(e.message()))?;
        if env.format != FORMAT {
            return Err(bad("unknown format"));
        }
        if env.version != VERSION {
            return Err(bad(&format!(
                "version {} is newer than this PrzmTerm",
                env.version
            )));
        }
        if env.kdf != "argon2id" {
            return Err(bad("unknown key derivation"));
        }
        let salt: [u8; SALT_LEN] = B64
            .decode(&env.salt)
            .ok()
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| bad("bad salt"))?;
        let nonce: [u8; 24] = B64
            .decode(&env.nonce)
            .ok()
            .and_then(|n| n.try_into().ok())
            .ok_or_else(|| bad("bad nonce"))?;
        let data = B64.decode(&env.data).map_err(|_| bad("bad data"))?;
        let key = derive(master, &salt, env.cost).map_err(|e| bad(&e))?;
        let cipher = XChaCha20Poly1305::new(&Key::from(*key));
        let plain = Zeroizing::new(
            cipher
                .decrypt(
                    &XNonce::from(nonce),
                    Payload {
                        msg: &data,
                        aad: &env.header(),
                    },
                )
                .map_err(|_| VaultError::WrongPassword)?,
        );
        let text = std::str::from_utf8(&plain).map_err(|_| bad("bad data"))?;
        let entries: Entries = toml::from_str(text).map_err(|e| bad(e.message()))?;
        Ok(Vault {
            path: path.to_path_buf(),
            key,
            cost: env.cost,
            salt,
            entries: entries.entries,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes the entries, encrypted with a fresh nonce. The file is
    /// replaced in one step, so a crash never leaves half a vault.
    pub fn save(&self) -> Result<(), VaultError> {
        let entries = Entries {
            entries: self.entries.clone(),
        };
        let plain = Zeroizing::new(toml::to_string(&entries).expect("entries serialise"));
        let nonce = XNonce::generate();
        let mut env = Envelope {
            format: FORMAT.into(),
            version: VERSION,
            kdf: "argon2id".into(),
            cost: self.cost,
            salt: B64.encode(self.salt),
            nonce: B64.encode(nonce),
            data: String::new(),
        };
        let cipher = XChaCha20Poly1305::new(&Key::from(*self.key));
        let data = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plain.as_bytes(),
                    aad: &env.header(),
                },
            )
            .expect("encryption does not fail");
        env.data = B64.encode(data);
        let text = format!(
            "# PrzmTerm password vault. Encrypted: open it in PrzmTerm with the master password.\n{}",
            toml::to_string(&env).expect("envelope serialises")
        );
        let io = |e| VaultError::Io(self.path.clone(), e);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &self.path).map_err(io)
    }

    /// Re-encrypts the vault under a new master password (and a new salt).
    pub fn change_master(&mut self, master: &str) -> Result<(), VaultError> {
        if master.chars().count() < MIN_MASTER_LEN {
            return Err(VaultError::WeakPassword);
        }
        let salt = <[u8; SALT_LEN]>::generate();
        self.key = derive(master, &salt, self.cost)
            .map_err(|e| VaultError::Format(self.path.clone(), e))?;
        self.salt = salt;
        self.save()
    }

    pub fn get(&self, name: &str) -> Option<&VaultEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.entries.iter().map(|e| e.name.clone()).collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    /// Adds `entry`, or replaces the one called `old_name`, and saves.
    pub fn put(&mut self, old_name: Option<&str>, mut entry: VaultEntry) -> Result<(), VaultError> {
        entry.name = entry.name.trim().to_string();
        if entry.name.is_empty() {
            return Err(VaultError::NoName);
        }
        let clash = self
            .entries
            .iter()
            .any(|e| e.name == entry.name && Some(e.name.as_str()) != old_name);
        if clash {
            return Err(VaultError::Duplicate(entry.name.clone()));
        }
        match old_name.and_then(|n| self.entries.iter().position(|e| e.name == n)) {
            Some(i) => self.entries[i] = entry,
            None => self.entries.push(entry),
        }
        self.save()
    }

    pub fn remove(&mut self, name: &str) -> Result<(), VaultError> {
        self.entries.retain(|e| e.name != name);
        self.save()
    }
}

/// Session and folder files (and the global settings) that name the vault
/// entry `name`.
pub fn files_using(config_dir: &Path, name: &str) -> Vec<PathBuf> {
    references(config_dir)
        .into_iter()
        .filter(|(_, n)| n == name)
        .map(|(path, _)| path)
        .collect()
}

/// The vault entries the sessions name, without opening the vault (whose
/// names are encrypted), sorted.
pub fn names_in_use(config_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = references(config_dir)
        .into_iter()
        .map(|(_, n)| n)
        .filter(|n| !n.is_empty())
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// Every file that names a vault entry, with the name.
fn references(config_dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut check = |path: &Path| {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(table) = toml::from_str::<toml::Table>(&text)
            && let Some(name) = table.get("credential").and_then(|v| v.as_str())
        {
            found.push((path.to_path_buf(), name.to_string()));
        }
    };
    check(&config_dir.join(crate::DEFAULTS_FILE));
    fn walk(dir: &Path, check: &mut dyn FnMut(&Path)) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                walk(&path, check);
            } else if path.extension().is_some_and(|e| e == "toml") {
                check(&path);
            }
        }
    }
    walk(&config_dir.join(crate::SESSIONS_DIR), &mut check);
    found
}

/// Points every file that names the entry `old` at `new` instead (after an
/// entry is renamed). Returns how many files changed.
pub fn rename_references(config_dir: &Path, old: &str, new: &str) -> Result<usize, String> {
    let files = files_using(config_dir, old);
    for path in &files {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut table: toml::Table =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        table.insert("credential".into(), toml::Value::String(new.into()));
        std::fs::write(path, toml::to_string(&table).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(files.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_opens_with_its_password_only() {
        let lock = Lock::new("tab secret").unwrap();
        assert!(lock.opens("tab secret"));
        assert!(!lock.opens("tab secreT"));
        assert!(!lock.opens(""));
    }

    fn entry(name: &str, user: &str, password: &str) -> VaultEntry {
        VaultEntry {
            name: name.into(),
            username: user.into(),
            password: password.into(),
            passphrase: String::new(),
        }
    }

    #[test]
    fn create_fill_reopen_and_change_the_master_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = vault_path(dir.path());
        assert!(matches!(
            Vault::create(&path, "short", KdfCost::for_tests()),
            Err(VaultError::WeakPassword)
        ));
        let mut vault = Vault::create(&path, "correct horse", KdfCost::for_tests()).unwrap();
        vault.put(None, entry("NOC", "noc", "s3cret!")).unwrap();
        vault.put(None, entry("lab", "admin", "")).unwrap();
        assert!(matches!(
            vault.put(None, entry("NOC", "x", "y")),
            Err(VaultError::Duplicate(_))
        ));
        assert!(matches!(
            vault.put(None, entry("  ", "x", "y")),
            Err(VaultError::NoName)
        ));

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("s3cret"), "{text}");
        assert!(!text.contains("noc"), "user names are encrypted too");

        assert!(matches!(
            Vault::open(&path, "wrong horse"),
            Err(VaultError::WrongPassword)
        ));
        let mut vault = Vault::open(&path, "correct horse").unwrap();
        assert_eq!(vault.names(), ["lab", "NOC"]);
        assert_eq!(vault.get("NOC").unwrap().password, "s3cret!");

        // Rename an entry in place.
        vault
            .put(Some("lab"), entry("Lab admin", "admin", "hunter2-lab"))
            .unwrap();
        vault.change_master("battery staple").unwrap();
        assert!(Vault::open(&path, "correct horse").is_err());
        let vault = Vault::open(&path, "battery staple").unwrap();
        assert_eq!(vault.names(), ["Lab admin", "NOC"]);
        // (Not "pw": the random temporary path in Debug may contain it.)
        assert!(
            !format!("{vault:?}").contains("hunter2"),
            "no secrets in Debug"
        );
    }

    #[test]
    fn tampering_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = vault_path(dir.path());
        let mut vault = Vault::create(&path, "correct horse", KdfCost::for_tests()).unwrap();
        vault.put(None, entry("NOC", "noc", "pw")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // Lowering the costs must not open the vault.
        let weaker = text.replace("iterations = 1", "iterations = 2");
        std::fs::write(&path, weaker).unwrap();
        assert!(matches!(
            Vault::open(&path, "correct horse"),
            Err(VaultError::WrongPassword)
        ));
        std::fs::write(&path, "format = 'something else'\n").unwrap();
        assert!(matches!(
            Vault::open(&path, "correct horse"),
            Err(VaultError::Format(..))
        ));
    }

    #[test]
    fn finds_and_renames_references() {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write("defaults.toml", "credential = 'NOC'\n");
        write("sessions/Core/folder.toml", "credential = 'NOC'\n");
        write("sessions/Core/r1.toml", "host = '10.0.0.1'\n");
        write(
            "sessions/r2.toml",
            "host = '10.0.0.2'\ncredential = 'lab'\n",
        );
        assert_eq!(files_using(dir.path(), "NOC").len(), 2);
        assert_eq!(names_in_use(dir.path()), ["lab", "NOC"]);
        assert_eq!(rename_references(dir.path(), "NOC", "Ops").unwrap(), 2);
        assert!(files_using(dir.path(), "NOC").is_empty());
        assert_eq!(files_using(dir.path(), "Ops").len(), 2);
        assert_eq!(files_using(dir.path(), "lab").len(), 1);
    }

    #[test]
    fn default_costs_are_strong() {
        let c = KdfCost::default();
        assert!(c.memory >= 64 * 1024 && c.iterations >= 3);
    }
}
