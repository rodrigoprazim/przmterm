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

use przmterm_core::Protocol;
use serde::{Deserialize, Serialize};

pub const DEFAULTS_FILE: &str = "defaults.toml";
pub const FOLDER_FILE: &str = "folder.toml";
pub const SESSIONS_DIR: &str = "sessions";

pub const DEFAULT_TERM_TYPE: &str = "xterm-256color";
pub const DEFAULT_SCROLLBACK: usize = 10_000;
pub const DEFAULT_BAUD_RATE: u32 = 9600;

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

/// Terminal options of a settings layer. `None` = inherit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalSettings {
    pub term_type: Option<String>,
    pub scrollback: Option<usize>,
}

/// Logging options of a settings layer. `None` = inherit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogSettings {
    pub enabled: Option<bool>,
    /// File name template, e.g. `~/logs/%S-%Y%M%D.log` (expansion comes in 0.3).
    pub path: Option<String>,
}

/// One settings layer: defaults, a folder or a session. `None` = inherit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionSettings {
    pub protocol: Option<Protocol>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub serial_port: Option<String>,
    pub baud_rate: Option<u32>,
    pub terminal: TerminalSettings,
    pub log: LogSettings,
}

impl SessionSettings {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
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
            serial_port: pick(&self.serial_port, &over.serial_port),
            baud_rate: pick(&self.baud_rate, &over.baud_rate),
            terminal: TerminalSettings {
                term_type: pick(&self.terminal.term_type, &over.terminal.term_type),
                scrollback: pick(&self.terminal.scrollback, &over.terminal.scrollback),
            },
            log: LogSettings {
                enabled: pick(&self.log.enabled, &over.log.enabled),
                path: pick(&self.log.path, &over.log.path),
            },
        }
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
    pub term_type: String,
    pub scrollback: usize,
    pub log_enabled: bool,
    pub log_path: Option<String>,
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
            term_type: settings
                .terminal
                .term_type
                .clone()
                .unwrap_or_else(|| DEFAULT_TERM_TYPE.to_string()),
            scrollback: settings.terminal.scrollback.unwrap_or(DEFAULT_SCROLLBACK),
            log_enabled: settings.log.enabled.unwrap_or(false),
            log_path: settings.log.path.clone(),
            path,
        })
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
    pub folders: Vec<Folder>,
    pub sessions: Vec<Session>,
}

impl Folder {
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
        }
        tree
    }
}

/// Default configuration directory: `$PRZMTERM_HOME`, else the OS config dir.
pub fn default_config_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("PRZMTERM_HOME") {
        return Some(PathBuf::from(home));
    }
    dirs::config_dir().map(|d| d.join("przmterm"))
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

fn load_folder(
    dir: &Path,
    name: String,
    parents: &[String],
    inherited: &SessionSettings,
    errors: &mut Vec<SessionError>,
) -> Folder {
    let settings = match read_settings(&dir.join(FOLDER_FILE)) {
        Ok(Some(own)) => inherited.overlay(&own),
        Ok(None) => inherited.clone(),
        Err(e) => {
            errors.push(e);
            inherited.clone()
        }
    };
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
