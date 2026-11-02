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
//! go to the OS keychain.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Datelike, Timelike};
use przmterm_core::Protocol;
pub use przmterm_core::forward::{Forward, ForwardKind, Tunnel};
pub use przmterm_core::serial::{
    COMMON_BAUD_RATES, DEFAULT_BAUD_RATE, DEFAULT_DATA_BITS, DEFAULT_STOP_BITS, FlowControl,
    Parity, SerialLine,
};
use serde::{Deserialize, Serialize};

pub const DEFAULTS_FILE: &str = "defaults.toml";
pub const FOLDER_FILE: &str = "folder.toml";
pub const SESSIONS_DIR: &str = "sessions";
/// PrzmTerm's own SSH known-hosts file, in OpenSSH format.
pub const KNOWN_HOSTS_FILE: &str = "known_hosts";

pub const DEFAULT_TERM_TYPE: &str = "xterm-256color";
pub const DEFAULT_SCROLLBACK: usize = 10_000;
/// Seconds between keepalives.
pub const DEFAULT_KEEPALIVE: u32 = 30;
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
    /// SSH: show every step of the connection and login (like `ssh -v`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<bool>,
    /// Seconds between keepalives (SSH keepalive, Telnet NOP); 0 turns them off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keepalive: Option<u32>,
    /// Reconnect on its own when the connection drops or cannot be made.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_reconnect: Option<bool>,
    /// SSH: connect through this server first: the name of another session,
    /// or `user@host:port`. Its own jump host is used too (chained).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jump_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_port: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baud_rate: Option<u32>,
    /// 5 to 8.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_bits: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parity: Option<Parity>,
    /// 1 or 2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_bits: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_control: Option<FlowControl>,
    /// SSH tunnels, `[[forward]]` tables. A layer that sets any replaces
    /// the inherited list.
    #[serde(rename = "forward", skip_serializing_if = "Option::is_none")]
    pub forwards: Option<Vec<Forward>>,
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
pub const FIELDS: [&str; 22] = [
    "protocol",
    "host",
    "port",
    "username",
    "identity_file",
    "save_password",
    "debug",
    "keepalive",
    "auto_reconnect",
    "jump_host",
    "forward",
    "serial_port",
    "baud_rate",
    "data_bits",
    "parity",
    "stop_bits",
    "flow_control",
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
            "debug" => self.debug = from.debug,
            "keepalive" => self.keepalive = from.keepalive,
            "auto_reconnect" => self.auto_reconnect = from.auto_reconnect,
            "jump_host" => self.jump_host = from.jump_host.clone(),
            "forward" => self.forwards = from.forwards.clone(),
            "serial_port" => self.serial_port = from.serial_port.clone(),
            "baud_rate" => self.baud_rate = from.baud_rate,
            "data_bits" => self.data_bits = from.data_bits,
            "parity" => self.parity = from.parity,
            "stop_bits" => self.stop_bits = from.stop_bits,
            "flow_control" => self.flow_control = from.flow_control,
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
            "debug" => self.debug.map(yes_no),
            "keepalive" => self.keepalive.map(|v| v.to_string()),
            "auto_reconnect" => self.auto_reconnect.map(yes_no),
            "jump_host" => self.jump_host.clone(),
            "forward" => self.forwards.as_ref().map(|f| {
                if f.is_empty() {
                    "none".to_string()
                } else {
                    f.iter()
                        .map(|f| f.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                }
            }),
            "serial_port" => self.serial_port.clone(),
            "baud_rate" => self.baud_rate.map(|v| v.to_string()),
            "data_bits" => self.data_bits.map(|v| v.to_string()),
            "parity" => self.parity.map(|v| v.name().to_string()),
            "stop_bits" => self.stop_bits.map(|v| v.to_string()),
            "flow_control" => self.flow_control.map(|v| v.name().to_string()),
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
            debug: pick(&self.debug, &over.debug),
            keepalive: pick(&self.keepalive, &over.keepalive),
            auto_reconnect: pick(&self.auto_reconnect, &over.auto_reconnect),
            jump_host: pick(&self.jump_host, &over.jump_host),
            forwards: pick(&self.forwards, &over.forwards),
            serial_port: pick(&self.serial_port, &over.serial_port),
            baud_rate: pick(&self.baud_rate, &over.baud_rate),
            data_bits: pick(&self.data_bits, &over.data_bits),
            parity: pick(&self.parity, &over.parity),
            stop_bits: pick(&self.stop_bits, &over.stop_bits),
            flow_control: pick(&self.flow_control, &over.flow_control),
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
            "data_bits" => s.data_bits = Some(DEFAULT_DATA_BITS),
            "parity" => s.parity = Some(Parity::None),
            "stop_bits" => s.stop_bits = Some(DEFAULT_STOP_BITS),
            "flow_control" => s.flow_control = Some(FlowControl::None),
            "terminal.term_type" => s.terminal.term_type = Some(DEFAULT_TERM_TYPE.into()),
            "terminal.scrollback" => s.terminal.scrollback = Some(DEFAULT_SCROLLBACK),
            "log.enabled" => s.log.enabled = Some(false),
            "debug" => s.debug = Some(false),
            "keepalive" => s.keepalive = Some(DEFAULT_KEEPALIVE),
            "auto_reconnect" => s.auto_reconnect = Some(false),
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
    Serial(SerialLine),
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
    /// See [`SessionSettings::debug`].
    pub debug: bool,
    /// `None` when keepalives are off.
    pub keepalive: Option<std::time::Duration>,
    pub auto_reconnect: bool,
    /// See [`SessionSettings::jump_host`].
    pub jump_host: Option<String>,
    pub tunnels: Vec<Tunnel>,
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
            Protocol::Serial => {
                let port = settings
                    .serial_port
                    .clone()
                    .filter(|p| !p.trim().is_empty())
                    .ok_or_else(|| invalid("`serial_port` is required for serial"))?;
                let data_bits = settings.data_bits.unwrap_or(DEFAULT_DATA_BITS);
                if !(5..=8).contains(&data_bits) {
                    return Err(invalid("`data_bits` must be 5, 6, 7 or 8"));
                }
                let stop_bits = settings.stop_bits.unwrap_or(DEFAULT_STOP_BITS);
                if !(1..=2).contains(&stop_bits) {
                    return Err(invalid("`stop_bits` must be 1 or 2"));
                }
                let baud_rate = settings.baud_rate.unwrap_or(DEFAULT_BAUD_RATE);
                if baud_rate == 0 {
                    return Err(invalid("`baud_rate` must be above 0"));
                }
                Target::Serial(SerialLine {
                    port,
                    baud_rate,
                    data_bits,
                    parity: settings.parity.unwrap_or_default(),
                    stop_bits,
                    flow_control: settings.flow_control.unwrap_or_default(),
                })
            }
            Protocol::Local => Target::Local,
        };
        Ok(Session {
            name: name.into(),
            folder,
            protocol,
            target,
            identity_file: settings.identity_file.as_deref().map(expand_tilde),
            save_password: settings.save_password,
            debug: settings.debug.unwrap_or(false),
            keepalive: match settings.keepalive.unwrap_or(DEFAULT_KEEPALIVE) {
                0 => None,
                secs => Some(std::time::Duration::from_secs(u64::from(secs))),
            },
            auto_reconnect: settings.auto_reconnect.unwrap_or(false),
            jump_host: settings.jump_host.clone().filter(|j| !j.trim().is_empty()),
            tunnels: settings
                .forwards
                .iter()
                .flatten()
                .enumerate()
                .map(|(i, f)| {
                    f.tunnel()
                        .map_err(|e| invalid(&format!("tunnel {}: {e}", i + 1)))
                })
                .collect::<Result<_, _>>()?,
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
    /// `host`, `user@host`, `host:port`, `user@[::1]:2222`, `telnet://host`,
    /// or `serial://PORT[@BAUD]` (`serial:///dev/ttyUSB0@115200`, `serial://COM3`).
    /// A leading `-v` turns on [`Session::debug`], as with `ssh -v`.
    pub fn quick_connect(target: &str) -> Result<Session, String> {
        let target = target.trim();
        if let Some(rest) = target
            .strip_prefix("-v ")
            .or_else(|| target.strip_prefix("-vvv "))
        {
            let mut session = Session::quick_connect(rest)?;
            session.debug = true;
            return Ok(session);
        }
        let (protocol, rest) = match target.split_once("://") {
            Some(("ssh", rest)) => (Protocol::Ssh, rest),
            Some(("telnet", rest)) => (Protocol::Telnet, rest),
            Some(("serial", rest)) => return Self::quick_serial(target, rest),
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

    fn quick_serial(target: &str, rest: &str) -> Result<Session, String> {
        let (port, baud) = match rest.rsplit_once('@') {
            Some((port, baud)) => {
                let baud = baud
                    .parse::<u32>()
                    .map_err(|_| format!("invalid baud rate `{baud}`"))?;
                (port, Some(baud))
            }
            None => (rest, None),
        };
        if port.is_empty() {
            return Err("enter a port, e.g. serial:///dev/ttyUSB0@9600 or serial://COM3".into());
        }
        let settings = SessionSettings {
            protocol: Some(Protocol::Serial),
            serial_port: Some(port.to_string()),
            baud_rate: baud,
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
            Target::Serial(line) => &line.port,
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

    /// Short description such as `admin@10.0.0.1:22` or `/dev/ttyUSB0 9600 8N1`.
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
            Target::Serial(line) => line.to_string(),
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
        assert_eq!(
            t("serial:///dev/ttyUSB0@115200"),
            Ok((Protocol::Serial, "/dev/ttyUSB0 115200 8N1".into()))
        );
        assert_eq!(
            t("serial://COM3"),
            Ok((Protocol::Serial, "COM3 9600 8N1".into()))
        );
        assert!(t("serial://COM3@fast").is_err());
        let debug = Session::quick_connect("-v admin@sw1").unwrap();
        assert!(debug.debug);
        assert_eq!(debug.display_target(), "admin@sw1:22");
        assert!(!Session::quick_connect("admin@sw1").unwrap().debug);
        assert!(t("serial://").is_err());
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
        assert_eq!(lab.display_target(), "/dev/ttyUSB0 115200 8N1");
        assert_eq!(tree.root.all_sessions().len(), 3);
    }

    #[test]
    fn serial_line_settings_resolve_and_validate() {
        let settings = SessionSettings::from_toml(
            "protocol = 'serial'\nserial_port = 'COM4'\nbaud_rate = 19200\n\
             data_bits = 7\nparity = 'even'\nstop_bits = 2\nflow_control = 'hardware'",
        )
        .unwrap();
        let s = Session::resolve("con", Vec::new(), "con.toml", &settings).unwrap();
        assert_eq!(s.display_target(), "COM4 19200 7E2 RTS/CTS");
        assert_eq!(s.keepalive, Some(std::time::Duration::from_secs(30)));
        assert!(!s.auto_reconnect);
        let mut off = settings.clone();
        off.keepalive = Some(0);
        off.auto_reconnect = Some(true);
        let s = Session::resolve("con", Vec::new(), "con.toml", &off).unwrap();
        assert_eq!(s.keepalive, None);
        assert!(s.auto_reconnect);
        assert_eq!(s.host_label(), "COM4");
        assert_eq!(settings.field_text("parity").as_deref(), Some("even"));
        assert_eq!(
            settings.field_text("flow_control").as_deref(),
            Some("hardware")
        );
        assert!(settings.to_toml().contains("flow_control = \"hardware\""));

        let mut bad = settings.clone();
        bad.data_bits = Some(9);
        assert!(Session::resolve("con", Vec::new(), "con.toml", &bad).is_err());
        bad = settings.clone();
        bad.stop_bits = Some(3);
        assert!(Session::resolve("con", Vec::new(), "con.toml", &bad).is_err());
        assert!(SessionSettings::from_toml("parity = 'mark'").is_err());
    }

    #[test]
    fn tunnels_and_jump_hosts() {
        let folder = SessionSettings::from_toml(
            "jump_host = 'bastion'\n[[forward]]\ntype = 'dynamic'\nlisten = '1080'",
        )
        .unwrap();
        let own = SessionSettings::from_toml(
            "host = 'db'\n[[forward]]\nlisten = '5432'\ntarget = 'localhost:5432'",
        )
        .unwrap();
        // The session's list replaces the folder's; the jump host is inherited.
        let merged = folder.overlay(&own);
        let s = Session::resolve("db", Vec::new(), "db.toml", &merged).unwrap();
        assert_eq!(s.jump_host.as_deref(), Some("bastion"));
        assert_eq!(s.tunnels.len(), 1);
        assert_eq!(
            s.tunnels[0].to_string(),
            "L 127.0.0.1:5432 → localhost:5432"
        );
        assert_eq!(
            folder.field_text("forward").as_deref(),
            Some("D 127.0.0.1:1080")
        );
        // Written back as [[forward]] tables.
        let toml = own.to_toml();
        assert!(toml.contains("[[forward]]"), "{toml}");
        assert_eq!(SessionSettings::from_toml(&toml).unwrap(), own);

        let bad = SessionSettings::from_toml("host = 'x'\n[[forward]]\nlisten = '80'").unwrap();
        let err = Session::resolve("x", Vec::new(), "x.toml", &bad).unwrap_err();
        assert!(err.to_string().contains("tunnel 1"), "{err}");
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
