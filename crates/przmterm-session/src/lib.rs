//! Session files for PrzmTerm.
//!
//! Layout of the configuration directory:
//!
//! ```text
//! <config>/
//!   defaults.toml          # global defaults (optional)
//!   sessions/
//!     folder.toml          # settings inherited by everything below (optional)
//!     core-sw-01.toml      # one session per file
//!     Cisco/
//!       folder.toml
//!       edge-rtr-01.toml
//! ```
//!
//! Settings resolve in three levels: global defaults, then each `folder.toml`
//! from the root down, then the session file. Passwords never live here; they
//! go to the OS keychain (a later version).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Datelike, Timelike};
use przmterm_core::Protocol;
use serde::{Deserialize, Serialize};

pub const DEFAULTS_FILE: &str = "defaults.toml";
pub const FOLDER_FILE: &str = "folder.toml";
pub const SESSIONS_DIR: &str = "sessions";
/// PrzmTerm's own SSH known-hosts file, in OpenSSH format.
pub const KNOWN_HOSTS_FILE: &str = "known_hosts";

pub const DEFAULT_TERM_TYPE: &str = "xterm-256color";
pub const DEFAULT_SCROLLBACK: usize = 10_000;
pub const DEFAULT_BAUD_RATE: u32 = 9600;
/// Log file name used when logging is on and no `path` is set, relative to `<config>/logs`.
pub const DEFAULT_LOG_TEMPLATE: &str = "%S-%Y%M%D-%h%m%s.log";
pub const LOGS_DIR: &str = "logs";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{path}: {reason}")]
    Invalid { path: PathBuf, reason: String },
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// Terminal options of a settings layer. `None` = inherit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub term_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollback: Option<usize>,
}

/// Logging options of a settings layer. `None` = inherit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// File name template, e.g. `%S-%Y%M%D.log`. See [`expand_log_template`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Folder for log files whose name is relative (default `<config>/logs`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

/// One settings layer: defaults, a folder or a session. `None` = inherit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Protocol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// SSH private key tried before the agent's and the default ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    /// Save passwords in the OS keychain: unset asks, `true` always, `false` never.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_password: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baud_rate: Option<u32>,
    #[serde(skip_serializing_if = "is_default")]
    pub terminal: TerminalSettings,
    #[serde(skip_serializing_if = "is_default")]
    pub log: LogSettings,
    /// Accepted and ignored: written by a development build of 0.3.
    #[doc(hidden)]
    #[serde(rename = "force", skip_serializing)]
    pub legacy_force: Vec<String>,
}

/// Keys of the individual settings, used to compare and copy them.
pub const FIELDS: [&str; 13] = [
    "protocol",
    "host",
    "port",
    "username",
    "identity_file",
    "save_password",
    "serial_port",
    "baud_rate",
    "terminal.term_type",
    "terminal.scrollback",
    "log.enabled",
    "log.path",
    "log.dir",
];

impl SessionSettings {
    /// Copies the field `key` (see [`FIELDS`]) from `from`, set or not.
    pub fn copy_field(&mut self, from: &SessionSettings, key: &str) {
        match key {
            "protocol" => self.protocol = from.protocol,
            "host" => self.host = from.host.clone(),
            "port" => self.port = from.port,
            "username" => self.username = from.username.clone(),
            "identity_file" => self.identity_file = from.identity_file.clone(),
            "save_password" => self.save_password = from.save_password,
            "serial_port" => self.serial_port = from.serial_port.clone(),
            "baud_rate" => self.baud_rate = from.baud_rate,
            "terminal.term_type" => self.terminal.term_type = from.terminal.term_type.clone(),
            "terminal.scrollback" => self.terminal.scrollback = from.terminal.scrollback,
            "log.enabled" => self.log.enabled = from.log.enabled,
            "log.path" => self.log.path = from.log.path.clone(),
            "log.dir" => self.log.dir = from.log.dir.clone(),
            _ => {}
        }
    }

    /// The field `key` as text, if set.
    pub fn field_text(&self, key: &str) -> Option<String> {
        let yes_no = |b: bool| if b { "yes" } else { "no" }.to_string();
        match key {
            "protocol" => self.protocol.map(|p| p.to_string()),
            "host" => self.host.clone(),
            "port" => self.port.map(|v| v.to_string()),
            "username" => self.username.clone(),
            "identity_file" => self.identity_file.clone(),
            "save_password" => self.save_password.map(yes_no),
            "serial_port" => self.serial_port.clone(),
            "baud_rate" => self.baud_rate.map(|v| v.to_string()),
            "terminal.term_type" => self.terminal.term_type.clone(),
            "terminal.scrollback" => self.terminal.scrollback.map(|v| v.to_string()),
            "log.enabled" => self.log.enabled.map(yes_no),
            "log.path" => self.log.path.clone(),
            "log.dir" => self.log.dir.clone(),
            _ => None,
        }
    }

    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// TOML with only the keys that are set.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Returns these settings with every value set in `over` replacing ours.
    pub fn overlay(&self, over: &SessionSettings) -> SessionSettings {
        fn pick<T: Clone>(base: &Option<T>, over: &Option<T>) -> Option<T> {
            over.clone().or_else(|| base.clone())
        }
        SessionSettings {
            protocol: pick(&self.protocol, &over.protocol),
            host: pick(&self.host, &over.host),
            port: pick(&self.port, &over.port),
            username: pick(&self.username, &over.username),
            identity_file: pick(&self.identity_file, &over.identity_file),
            save_password: pick(&self.save_password, &over.save_password),
            serial_port: pick(&self.serial_port, &over.serial_port),
            baud_rate: pick(&self.baud_rate, &over.baud_rate),
            terminal: TerminalSettings {
                term_type: pick(&self.terminal.term_type, &over.terminal.term_type),
                scrollback: pick(&self.terminal.scrollback, &over.terminal.scrollback),
            },
            log: LogSettings {
                enabled: pick(&self.log.enabled, &over.log.enabled),
                path: pick(&self.log.path, &over.log.path),
                dir: pick(&self.log.dir, &over.log.dir),
            },
            legacy_force: Vec::new(),
        }
    }

    /// The built-in value of `key` as a setting, for the fields that have one
    /// that can be written down (not e.g. the user name, which is asked).
    pub fn builtin(key: &str, protocol: Protocol) -> Option<SessionSettings> {
        let mut s = SessionSettings::default();
        match key {
            "protocol" => s.protocol = Some(Protocol::Ssh),
            "port" => s.port = protocol.default_port(),
            "baud_rate" => s.baud_rate = Some(DEFAULT_BAUD_RATE),
            "terminal.term_type" => s.terminal.term_type = Some(DEFAULT_TERM_TYPE.into()),
            "terminal.scrollback" => s.terminal.scrollback = Some(DEFAULT_SCROLLBACK),
            "log.enabled" => s.log.enabled = Some(false),
            "log.path" => s.log.path = Some(DEFAULT_LOG_TEMPLATE.into()),
            _ => return None,
        }
        s.field_text(key).is_some().then_some(s)
    }

    /// Keys whose value differs between `self` and `other`.
    pub fn changed_fields(&self, other: &SessionSettings) -> Vec<&'static str> {
        FIELDS
            .iter()
            .copied()
            .filter(|k| self.field_text(k) != other.field_text(k))
            .collect()
    }
}

/// Where a session connects to, after inheritance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Network {
        host: String,
        port: u16,
        username: Option<String>,
    },
    Serial {
        port: String,
        baud_rate: u32,
    },
    Local,
}

/// A fully resolved session, ready to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    /// Folder names from the root to the session's folder.
    pub folder: Vec<String>,
    pub path: PathBuf,
    pub protocol: Protocol,
    pub target: Target,
    /// SSH private key, with `~` expanded.
    pub identity_file: Option<PathBuf>,
    /// See [`SessionSettings::save_password`].
    pub save_password: Option<bool>,
    pub term_type: String,
    pub scrollback: usize,
    pub log_enabled: bool,
    pub log_path: Option<String>,
    /// Folder for relative log file names, with `~` expanded.
    pub log_dir: Option<PathBuf>,
}

impl Session {
    /// Resolves merged settings into a session, checking required fields.
    pub fn resolve(
        name: impl Into<String>,
        folder: Vec<String>,
        path: impl Into<PathBuf>,
        settings: &SessionSettings,
    ) -> Result<Session, SessionError> {
        let path = path.into();
        let protocol = settings.protocol.unwrap_or_default();
        let invalid = |reason: &str| SessionError::Invalid {
            path: path.clone(),
            reason: reason.to_string(),
        };
        let target = match protocol {
            Protocol::Ssh | Protocol::Telnet => {
                let host = settings
                    .host
                    .clone()
                    .filter(|h| !h.trim().is_empty())
                    .ok_or_else(|| invalid("`host` is required for ssh and telnet"))?;
                let port = settings
                    .port
                    .or(protocol.default_port())
                    .ok_or_else(|| invalid("`port` is required"))?;
                Target::Network {
                    host,
                    port,
                    username: settings.username.clone(),
                }
            }
            Protocol::Serial => Target::Serial {
                port: settings
                    .serial_port
                    .clone()
                    .ok_or_else(|| invalid("`serial_port` is required for serial"))?,
                baud_rate: settings.baud_rate.unwrap_or(DEFAULT_BAUD_RATE),
            },
            Protocol::Local => Target::Local,
        };
        Ok(Session {
            name: name.into(),
            folder,
            protocol,
            target,
            identity_file: settings.identity_file.as_deref().map(expand_tilde),
            save_password: settings.save_password,
            term_type: settings
                .terminal
                .term_type
                .clone()
                .unwrap_or_else(|| DEFAULT_TERM_TYPE.to_string()),
            scrollback: settings.terminal.scrollback.unwrap_or(DEFAULT_SCROLLBACK),
            log_enabled: settings.log.enabled.unwrap_or(false),
            log_path: settings.log.path.clone(),
            log_dir: settings.log.dir.as_deref().map(expand_tilde),
            path,
        })
    }

    /// Builds a session from a quick-connect string:
    /// `host`, `user@host`, `host:port`, `user@[::1]:2222`, or `telnet://host`.
    pub fn quick_connect(target: &str) -> Result<Session, String> {
        let target = target.trim();
        let (protocol, rest) = match target.split_once("://") {
            Some(("ssh", rest)) => (Protocol::Ssh, rest),
            Some(("telnet", rest)) => (Protocol::Telnet, rest),
            Some((scheme, _)) => return Err(format!("unsupported protocol `{scheme}`")),
            None => (Protocol::Ssh, target),
        };
        let (username, hostport) = match rest.rsplit_once('@') {
            Some((u, h)) if !u.is_empty() => (Some(u.to_string()), h),
            Some((_, h)) => (None, h),
            None => (None, rest),
        };
        let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
            let (host, after) = inner
                .split_once(']')
                .ok_or("missing `]` after IPv6 address")?;
            let port = match after.strip_prefix(':') {
                Some(p) => Some(p),
                None if after.is_empty() => None,
                None => return Err(format!("unexpected `{after}` after `]`")),
            };
            (host, port)
        } else if hostport.matches(':').count() == 1 {
            let (h, p) = hostport.split_once(':').unwrap_or((hostport, ""));
            (h, Some(p))
        } else {
            // No colon, or a bare IPv6 address without brackets.
            (hostport, None)
        };
        if host.is_empty() || host.contains(char::is_whitespace) {
            return Err("enter a host, e.g. admin@10.0.0.1:22".into());
        }
        let port = match port {
            Some(p) => p
                .parse::<u16>()
                .map_err(|_| format!("invalid port `{p}`"))?,
            None => protocol.default_port().unwrap_or(22),
        };
        let settings = SessionSettings {
            protocol: Some(protocol),
            host: Some(host.to_string()),
            port: Some(port),
            username,
            ..SessionSettings::default()
        };
        Session::resolve(target, Vec::new(), PathBuf::new(), &settings).map_err(|e| e.to_string())
    }

    /// Keychain account for saved passwords (`user@host:port`), when the user is known.
    pub fn password_account(&self) -> Option<String> {
        match &self.target {
            Target::Network {
                host,
                port,
                username: Some(user),
            } => Some(format!("{user}@{host}:{port}")),
            _ => None,
        }
    }

    /// Host name, serial port or `local`, as used in log file names.
    pub fn host_label(&self) -> &str {
        match &self.target {
            Target::Network { host, .. } => host,
            Target::Serial { port, .. } => port,
            Target::Local => "local",
        }
    }

    /// Log file for a connection starting at `now`. Relative names go in the
    /// session's log folder, by default `<config>/logs`.
    pub fn log_file(&self, config_dir: Option<&Path>, now: &(impl Datelike + Timelike)) -> PathBuf {
        let template = self.log_path.as_deref().unwrap_or(DEFAULT_LOG_TEMPLATE);
        let path = expand_log_template(template, &self.name, self.host_label(), now);
        if path.is_absolute() {
            return path;
        }
        let dir = match &self.log_dir {
            Some(dir) => dir.clone(),
            None => config_dir.map(|d| d.join(LOGS_DIR)).unwrap_or_default(),
        };
        dir.join(path)
    }

    /// Short description such as `admin@10.0.0.1:22` or `/dev/ttyUSB0 @ 9600`.
    pub fn display_target(&self) -> String {
        match &self.target {
            Target::Network {
                host,
                port,
                username,
            } => {
                let user = username
                    .as_deref()
                    .map(|u| format!("{u}@"))
                    .unwrap_or_default();
                format!("{user}{host}:{port}")
            }
            Target::Serial { port, baud_rate } => format!("{port} @ {baud_rate}"),
            Target::Local => "local shell".to_string(),
        }
    }
}

/// A folder of the session tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folder {
    pub name: String,
    /// Directory on disk.
    pub dir: PathBuf,
    /// Folder names from the root to this folder (empty for the root).
    pub trail: Vec<String>,
    /// Settings this folder passes down: defaults plus every `folder.toml`
    /// from the root to here.
    pub settings: SessionSettings,
    /// Settings this folder receives from above (without its own `folder.toml`).
    pub inherited: SessionSettings,
    /// This folder's own settings (`folder.toml`).
    pub own: SessionSettings,
    pub folders: Vec<Folder>,
    pub sessions: Vec<Session>,
}

impl Folder {
    /// The folder at `trail` below this one (`[]` is this folder).
    pub fn find(&self, trail: &[String]) -> Option<&Folder> {
        match trail.split_first() {
            None => Some(self),
            Some((first, rest)) => self
                .folders
                .iter()
                .find(|f| f.name == *first)
                .and_then(|f| f.find(rest)),
        }
    }

    /// Number of sessions in this folder and below.
    pub fn session_count(&self) -> usize {
        self.all_sessions().len()
    }

    /// Paths of this folder and every folder below it, e.g. `["", "Cisco", "Cisco/Core"]`.
    pub fn folder_paths(&self) -> Vec<String> {
        fn walk(folder: &Folder, prefix: &str, out: &mut Vec<String>) {
            for f in &folder.folders {
                let path = if prefix.is_empty() {
                    f.name.clone()
                } else {
                    format!("{prefix}/{}", f.name)
                };
                out.push(path.clone());
                walk(f, &path, out);
            }
        }
        let mut out = vec![String::new()];
        walk(self, "", &mut out);
        out
    }

    /// Every session in this folder and below, depth first.
    pub fn all_sessions(&self) -> Vec<&Session> {
        let mut out: Vec<&Session> = self.sessions.iter().collect();
        for f in &self.folders {
            out.extend(f.all_sessions());
        }
        out
    }
}

/// The loaded session tree plus the files that could not be used.
#[derive(Debug, Default)]
pub struct SessionTree {
    pub root: Folder,
    pub errors: Vec<SessionError>,
}

impl SessionTree {
    /// Loads `<config>/defaults.toml` and the tree under `<config>/sessions`.
    /// A missing directory yields an empty tree; broken files land in `errors`.
    pub fn load(config_dir: &Path) -> SessionTree {
        let mut tree = SessionTree::default();
        let defaults_path = config_dir.join(DEFAULTS_FILE);
        let defaults = match read_settings(&defaults_path) {
            Ok(Some(s)) => s,
            Ok(None) => SessionSettings::default(),
            Err(e) => {
                tree.errors.push(e);
                SessionSettings::default()
            }
        };
        let sessions_dir = config_dir.join(SESSIONS_DIR);
        if sessions_dir.is_dir() {
            tree.root = load_folder(
                &sessions_dir,
                String::new(),
                &[],
                &defaults,
                &mut tree.errors,
            );
        } else {
            tree.root = Folder {
                dir: sessions_dir,
                settings: defaults.clone(),
                inherited: defaults,
                ..Folder::default()
            };
        }
        tree
    }
}

/// Reads the settings written in one file, without inheritance.
pub fn read_session_settings(path: &Path) -> Result<SessionSettings, SessionError> {
    Ok(read_settings(path)?.unwrap_or_default())
}

/// Checks a session or folder name for use as a file name.
pub fn validate_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the name is empty".into());
    }
    if name.starts_with('.')
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        return Err(format!("`{name}` cannot be used as a file name"));
    }
    if format!("{name}.toml") == FOLDER_FILE {
        return Err("`folder` is reserved for folder settings".into());
    }
    Ok(())
}

/// Writes a session file `<config>/sessions/<folder>/<name>.toml` and returns
/// its path. `folder` uses `/` between levels; `""` is the top level.
pub fn write_session(
    config_dir: &Path,
    folder: &str,
    name: &str,
    settings: &SessionSettings,
) -> Result<PathBuf, SessionError> {
    let name = name.trim();
    let mut dir = config_dir.join(SESSIONS_DIR);
    for part in folder.split('/').map(str::trim).filter(|p| !p.is_empty()) {
        validate_name(part).map_err(|reason| SessionError::Invalid {
            path: dir.join(part),
            reason,
        })?;
        dir.push(part);
    }
    let path = dir.join(format!("{name}.toml"));
    validate_name(name).map_err(|reason| SessionError::Invalid {
        path: path.clone(),
        reason,
    })?;
    fs::create_dir_all(&dir).map_err(|source| SessionError::Io {
        path: dir.clone(),
        source,
    })?;
    fs::write(&path, settings.to_toml()).map_err(|source| SessionError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// Creates the folder `name` inside `parent` and returns its path.
pub fn create_folder(parent: &Path, name: &str) -> Result<PathBuf, SessionError> {
    let path = parent.join(name.trim());
    validate_name(name).map_err(|reason| SessionError::Invalid {
        path: path.clone(),
        reason,
    })?;
    if path.exists() {
        return Err(SessionError::Invalid {
            path,
            reason: "a folder with this name already exists".into(),
        });
    }
    fs::create_dir_all(&path).map_err(|source| SessionError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// Renames the folder at `dir` and returns its new path.
pub fn rename_folder(dir: &Path, new_name: &str) -> Result<PathBuf, SessionError> {
    let parent = dir.parent().unwrap_or(dir);
    let path = parent.join(new_name.trim());
    validate_name(new_name).map_err(|reason| SessionError::Invalid {
        path: path.clone(),
        reason,
    })?;
    if path == dir {
        return Ok(path);
    }
    if path.exists() {
        return Err(SessionError::Invalid {
            path,
            reason: "a folder with this name already exists".into(),
        });
    }
    fs::rename(dir, &path).map_err(|source| SessionError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    Ok(path)
}

/// Deletes the folder at `dir` with everything in it.
pub fn delete_folder(dir: &Path) -> Result<(), SessionError> {
    fs::remove_dir_all(dir).map_err(|source| SessionError::Io {
        path: dir.to_path_buf(),
        source,
    })
}

/// Writes a folder's own settings (`folder.toml`); empty settings remove the file.
pub fn write_folder_settings(dir: &Path, settings: &SessionSettings) -> Result<(), SessionError> {
    let path = dir.join(FOLDER_FILE);
    let io = |source| SessionError::Io {
        path: path.clone(),
        source,
    };
    if *settings == SessionSettings::default() {
        match fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(e)),
            _ => Ok(()),
        }
    } else {
        fs::create_dir_all(dir).map_err(|source| SessionError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        fs::write(&path, settings.to_toml()).map_err(io)
    }
}

/// Before the global defaults change, keeps the old value in the sessions that
/// take it from the global defaults (neither the session nor a folder above
/// it sets the field): for each key in `keys`, writes `old_global`'s value (or
/// the built-in one) into those session files. Returns how many files changed
/// and how many sessions could not keep a value (fields with no written form,
/// such as an unset user name, which is asked at login).
pub fn keep_old_defaults(
    config_dir: &Path,
    old_global: &SessionSettings,
    keys: &[&str],
) -> Result<(usize, usize), SessionError> {
    fn walk(
        folder: &Folder,
        set_above: &[&str],
        old_global: &SessionSettings,
        keys: &[&str],
        counts: &mut (usize, usize),
    ) -> Result<(), SessionError> {
        // Keys a folder on the way down already sets are not global for its sessions.
        let mut set_here: Vec<&str> = set_above.to_vec();
        for key in keys {
            if folder.own.field_text(key).is_some() {
                set_here.push(key);
            }
        }
        for session in &folder.sessions {
            let mut own = read_session_settings(&session.path)?;
            let mut changed = false;
            for key in keys {
                if set_here.contains(key) || own.field_text(key).is_some() {
                    continue;
                }
                let value = if old_global.field_text(key).is_some() {
                    Some(old_global.clone())
                } else {
                    SessionSettings::builtin(key, session.protocol)
                };
                match value {
                    Some(v) => {
                        own.copy_field(&v, key);
                        changed = true;
                    }
                    None => counts.1 += 1,
                }
            }
            if changed {
                fs::write(&session.path, own.to_toml()).map_err(|source| SessionError::Io {
                    path: session.path.clone(),
                    source,
                })?;
                counts.0 += 1;
            }
        }
        for f in &folder.folders {
            walk(f, &set_here, old_global, keys, counts)?;
        }
        Ok(())
    }
    let tree = SessionTree::load(config_dir);
    let mut counts = (0, 0);
    walk(&tree.root, &[], old_global, keys, &mut counts)?;
    Ok(counts)
}

/// Moves a session file into `folder_dir` and returns its new path.
pub fn move_session(path: &Path, folder_dir: &Path) -> Result<PathBuf, SessionError> {
    let file_name = path.file_name().ok_or_else(|| SessionError::Invalid {
        path: path.to_path_buf(),
        reason: "not a session file".into(),
    })?;
    let target = folder_dir.join(file_name);
    if target == path {
        return Ok(target);
    }
    if target.exists() {
        return Err(SessionError::Invalid {
            path: target,
            reason: "a session with this name already exists there".into(),
        });
    }
    fs::create_dir_all(folder_dir).map_err(|source| SessionError::Io {
        path: folder_dir.to_path_buf(),
        source,
    })?;
    fs::rename(path, &target).map_err(|source| SessionError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(target)
}

/// Writes the global defaults (`<config>/defaults.toml`).
pub fn write_defaults(config_dir: &Path, settings: &SessionSettings) -> Result<(), SessionError> {
    let path = config_dir.join(DEFAULTS_FILE);
    fs::create_dir_all(config_dir).map_err(|source| SessionError::Io {
        path: config_dir.to_path_buf(),
        source,
    })?;
    fs::write(&path, settings.to_toml()).map_err(|source| SessionError::Io { path, source })
}

/// Default configuration directory: `$PRZMTERM_HOME`, else the OS config dir.
pub fn default_config_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("PRZMTERM_HOME") {
        return Some(PathBuf::from(home));
    }
    dirs::config_dir().map(|d| d.join("przmterm"))
}

/// Expands a log file name template:
///
/// | Code | Meaning |
/// | --- | --- |
/// | `%S` | session name |
/// | `%H` | host (or serial port) |
/// | `%Y` `%M` `%D` | year, month, day |
/// | `%h` `%m` `%s` | hour, minute, second |
/// | `%%` | a literal `%` |
///
/// Characters that are not valid in file names are replaced with `_` in the
/// session name and host. A leading `~` is expanded.
pub fn expand_log_template(
    template: &str,
    session: &str,
    host: &str,
    now: &(impl Datelike + Timelike),
) -> PathBuf {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_control() || "/\\:*?\"<>|".contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .collect()
    };
    let mut out = String::new();
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('S') => out.push_str(&safe(session)),
            Some('H') => out.push_str(&safe(host)),
            Some('Y') => out.push_str(&format!("{:04}", now.year())),
            Some('M') => out.push_str(&format!("{:02}", now.month())),
            Some('D') => out.push_str(&format!("{:02}", now.day())),
            Some('h') => out.push_str(&format!("{:02}", now.hour())),
            Some('m') => out.push_str(&format!("{:02}", now.minute())),
            Some('s') => out.push_str(&format!("{:02}", now.second())),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    expand_tilde(&out)
}

/// Expands a leading `~` or `~/` to the home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), dirs::home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(path),
    }
}

fn read_settings(path: &Path) -> Result<Option<SessionSettings>, SessionError> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SessionError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    SessionSettings::from_toml(&text)
        .map(Some)
        .map_err(|source| SessionError::Parse {
            path: path.to_path_buf(),
            source,
        })
}

/// Loads a folder that inherits `inherited` from above.
fn load_folder(
    dir: &Path,
    name: String,
    parents: &[String],
    inherited: &SessionSettings,
    errors: &mut Vec<SessionError>,
) -> Folder {
    let own = match read_settings(&dir.join(FOLDER_FILE)) {
        Ok(own) => own.unwrap_or_default(),
        Err(e) => {
            errors.push(e);
            SessionSettings::default()
        }
    };
    let settings = inherited.overlay(&own);
    let mut trail = parents.to_vec();
    if !name.is_empty() {
        trail.push(name.clone());
    }

    let mut entries: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(source) => {
            errors.push(SessionError::Io {
                path: dir.to_path_buf(),
                source,
            });
            Vec::new()
        }
    };
    entries.sort();

    let mut folder = Folder {
        name,
        dir: dir.to_path_buf(),
        trail: trail.clone(),
        settings: settings.clone(),
        inherited: inherited.clone(),
        own,
        ..Folder::default()
    };
    for path in entries {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if file_name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            folder.folders.push(load_folder(
                &path,
                file_name.to_string(),
                &trail,
                &settings,
                errors,
            ));
        } else if file_name != FOLDER_FILE && path.extension().is_some_and(|e| e == "toml") {
            let session_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let result = read_settings(&path).and_then(|own| {
                let merged = settings.overlay(&own.unwrap_or_default());
                Session::resolve(session_name, trail.clone(), &path, &merged)
            });
            match result {
                Ok(s) => folder.sessions.push(s),
                Err(e) => errors.push(e),
            }
        }
    }
    folder
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn overlay_prefers_the_upper_layer() {
        let base = SessionSettings::from_toml("username = 'admin'\nport = 2222").unwrap();
        let over = SessionSettings::from_toml("port = 22\n[terminal]\nscrollback = 500").unwrap();
        let merged = base.overlay(&over);
        assert_eq!(merged.username.as_deref(), Some("admin"));
        assert_eq!(merged.port, Some(22));
        assert_eq!(merged.terminal.scrollback, Some(500));
    }

    #[test]
    fn quick_connect_forms() {
        let t = |s: &str| Session::quick_connect(s).map(|s| (s.protocol, s.display_target()));
        assert_eq!(t("10.0.0.1"), Ok((Protocol::Ssh, "10.0.0.1:22".into())));
        assert_eq!(
            t("admin@sw1:2222"),
            Ok((Protocol::Ssh, "admin@sw1:2222".into()))
        );
        assert_eq!(t("a@b@host"), Ok((Protocol::Ssh, "a@b@host:22".into())));
        assert_eq!(t("u@[::1]:2200"), Ok((Protocol::Ssh, "u@::1:2200".into())));
        assert_eq!(t("fe80::1"), Ok((Protocol::Ssh, "fe80::1:22".into())));
        assert_eq!(t("telnet://rtr"), Ok((Protocol::Telnet, "rtr:23".into())));
        assert!(t("host:99999").is_err());
        assert!(t("").is_err());
        assert!(t("ftp://x").is_err());
    }

    #[test]
    fn identity_file_expands_tilde() {
        let s = SessionSettings::from_toml("host = 'h'\nidentity_file = '~/.ssh/lab'").unwrap();
        let session = Session::resolve("h", vec![], "h.toml", &s).unwrap();
        let expected = dirs::home_dir().unwrap().join(".ssh/lab");
        assert_eq!(session.identity_file, Some(expected));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("~user/x"), PathBuf::from("~user/x"));
    }

    #[test]
    fn log_templates() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 3, 7)
            .unwrap()
            .and_hms_opt(9, 5, 2)
            .unwrap();
        assert_eq!(
            expand_log_template(
                "/var/log/%S_%H_%Y%M%D-%h%m%s.log",
                "core/sw:1",
                "10.0.0.1",
                &now
            ),
            PathBuf::from("/var/log/core_sw_1_10.0.0.1_20260307-090502.log")
        );
        assert_eq!(
            expand_log_template("100%%-%x", "s", "h", &now),
            PathBuf::from("100%-%x")
        );

        let s = Session::quick_connect("admin@rtr").unwrap();
        let file = s.log_file(Some(Path::new("/cfg")), &now);
        assert_eq!(
            file,
            PathBuf::from("/cfg/logs/admin@rtr-20260307-090502.log")
        );
    }

    #[test]
    fn writes_only_the_keys_that_are_set() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = SessionSettings {
            host: Some("10.0.0.9".into()),
            save_password: Some(false),
            log: LogSettings {
                enabled: Some(true),
                ..LogSettings::default()
            },
            ..SessionSettings::default()
        };
        let path = write_session(tmp.path(), "Lab/Core", "sw9", &settings).unwrap();
        assert_eq!(path, tmp.path().join("sessions/Lab/Core/sw9.toml"));
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "host = \"10.0.0.9\"\nsave_password = false\n\n[log]\nenabled = true\n"
        );
        assert_eq!(read_session_settings(&path).unwrap(), settings);

        let tree = SessionTree::load(tmp.path());
        assert_eq!(tree.root.folder_paths(), vec!["", "Lab", "Lab/Core"]);
        assert!(write_session(tmp.path(), "", "a/b", &settings).is_err());
        assert!(write_session(tmp.path(), "", "folder", &settings).is_err());
        assert!(write_session(tmp.path(), "../x", "ok", &settings).is_err());
    }

    #[test]
    fn folder_operations_and_effective_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(dir, "defaults.toml", "username = 'root'");
        let lab = create_folder(&dir.join(SESSIONS_DIR), "Lab").unwrap();
        assert!(create_folder(&dir.join(SESSIONS_DIR), "Lab").is_err());
        let core = create_folder(&lab, "Core").unwrap();
        write_folder_settings(&lab, &SessionSettings::from_toml("port = 2222").unwrap()).unwrap();
        write(dir, "sessions/Lab/Core/sw.toml", "host = 'sw'");

        let tree = SessionTree::load(dir);
        let core_folder = tree.root.find(&["Lab".into(), "Core".into()]).unwrap();
        assert_eq!(core_folder.dir, core);
        assert_eq!(
            core_folder.trail,
            vec!["Lab".to_string(), "Core".to_string()]
        );
        assert_eq!(core_folder.settings.port, Some(2222));
        assert_eq!(core_folder.settings.username.as_deref(), Some("root"));
        let lab_folder = tree.root.find(&["Lab".into()]).unwrap();
        assert_eq!(
            lab_folder.inherited.port, None,
            "own folder.toml not inherited"
        );
        assert_eq!(lab_folder.session_count(), 1);

        let renamed = rename_folder(&lab, "Datacenter").unwrap();
        let tree = SessionTree::load(dir);
        let sw = tree.root.all_sessions()[0];
        assert_eq!(
            sw.folder,
            vec!["Datacenter".to_string(), "Core".to_string()]
        );
        assert_eq!(sw.display_target(), "root@sw:2222");

        write_folder_settings(&renamed, &SessionSettings::default()).unwrap();
        assert!(!renamed.join(FOLDER_FILE).exists());
        delete_folder(&renamed).unwrap();
        assert!(SessionTree::load(dir).root.all_sessions().is_empty());
    }

    #[test]
    fn keep_old_defaults_pins_only_sessions_that_inherit_from_global() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(dir, "defaults.toml", "port = 2200");
        write(dir, "sessions/a.toml", "host = 'a'");
        write(dir, "sessions/b.toml", "host = 'b'\nport = 9");
        write(dir, "sessions/Lab/folder.toml", "port = 2222");
        write(dir, "sessions/Lab/c.toml", "host = 'c'");

        let old = read_session_settings(&dir.join(DEFAULTS_FILE)).unwrap();
        let new = SessionSettings::from_toml("port = 22\nusername = 'ops'").unwrap();
        let keys = old.changed_fields(&new);
        assert_eq!(keys, vec!["port", "username"]);
        let (changed, skipped) = keep_old_defaults(dir, &old, &keys).unwrap();
        write_defaults(dir, &new).unwrap();
        assert_eq!(changed, 1, "only a.toml takes the port from global");
        assert_eq!(skipped, 3, "an unset user name cannot be written down");

        let tree = SessionTree::load(dir);
        let target = |n: &str| {
            tree.root
                .all_sessions()
                .into_iter()
                .find(|s| s.name == n)
                .unwrap()
                .display_target()
        };
        assert_eq!(
            target("a"),
            "ops@a:2200",
            "kept the old port, got the new user"
        );
        assert_eq!(target("b"), "ops@b:9");
        assert_eq!(target("c"), "ops@c:2222");
    }

    #[test]
    fn moves_sessions_and_reads_legacy_force() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(dir, "sessions/a.toml", "host = 'a'");
        let lab = create_folder(&dir.join(SESSIONS_DIR), "Lab").unwrap();
        let moved = move_session(&dir.join("sessions/a.toml"), &lab).unwrap();
        assert_eq!(moved, lab.join("a.toml"));
        write(dir, "sessions/a.toml", "host = 'other'");
        assert!(move_session(&dir.join("sessions/a.toml"), &lab).is_err());

        let legacy = SessionSettings::from_toml("port = 1\nforce = ['port']").unwrap();
        assert_eq!(legacy.to_toml(), "port = 1\n");
    }

    #[test]
    fn log_folder_setting() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 1, 2)
            .unwrap()
            .and_hms_opt(3, 4, 5)
            .unwrap();
        let s = SessionSettings::from_toml("host = 'h'\n[log]\ndir = '/var/przm'\npath = '%H.log'")
            .unwrap();
        let session = Session::resolve("h", vec![], "h.toml", &s).unwrap();
        assert_eq!(
            session.log_file(Some(Path::new("/cfg")), &now),
            PathBuf::from("/var/przm/h.log")
        );
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(SessionSettings::from_toml("hots = 'typo'").is_err());
    }

    #[test]
    fn loads_tree_with_three_level_inheritance() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            dir,
            "defaults.toml",
            "username = 'rodrigo'\n[log]\nenabled = true",
        );
        write(dir, "sessions/core-sw-01.toml", "host = '10.0.0.1'");
        write(
            dir,
            "sessions/Cisco/folder.toml",
            "username = 'netadmin'\nprotocol = 'telnet'",
        );
        write(dir, "sessions/Cisco/edge-rtr-01.toml", "host = '10.0.1.1'");
        write(
            dir,
            "sessions/Console/lab.toml",
            "protocol = 'serial'\nserial_port = '/dev/ttyUSB0'\nbaud_rate = 115200",
        );

        let tree = SessionTree::load(dir);
        assert!(tree.errors.is_empty(), "{:?}", tree.errors);

        let core = &tree.root.sessions[0];
        assert_eq!(core.name, "core-sw-01");
        assert_eq!(core.display_target(), "rodrigo@10.0.0.1:22");
        assert!(core.log_enabled);

        let cisco = &tree.root.folders[0];
        assert_eq!(cisco.name, "Cisco");
        let edge = &cisco.sessions[0];
        assert_eq!(edge.folder, vec!["Cisco".to_string()]);
        assert_eq!(edge.protocol, Protocol::Telnet);
        assert_eq!(edge.display_target(), "netadmin@10.0.1.1:23");

        let lab = &tree.root.folders[1].sessions[0];
        assert_eq!(lab.display_target(), "/dev/ttyUSB0 @ 115200");
        assert_eq!(tree.root.all_sessions().len(), 3);
    }

    #[test]
    fn broken_files_are_reported_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(dir, "sessions/ok.toml", "host = 'a'");
        write(dir, "sessions/no-host.toml", "username = 'x'");
        write(dir, "sessions/bad.toml", "host = ");

        let tree = SessionTree::load(dir);
        assert_eq!(tree.root.sessions.len(), 1);
        assert_eq!(tree.errors.len(), 2);
        assert!(
            tree.errors
                .iter()
                .any(|e| matches!(e, SessionError::Invalid { .. }))
        );
        assert!(
            tree.errors
                .iter()
                .any(|e| matches!(e, SessionError::Parse { .. }))
        );
    }

    #[test]
    fn missing_directory_is_empty() {
        let tree = SessionTree::load(Path::new("/nonexistent/przmterm"));
        assert!(tree.root.all_sessions().is_empty());
        assert!(tree.errors.is_empty());
    }
}
