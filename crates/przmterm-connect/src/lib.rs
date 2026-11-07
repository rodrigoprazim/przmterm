//! Connecting a session: the transport behind it, with jump hosts, vault
//! entries and saved passwords resolved. Shared by the desktop app and the
//! `przm` command line.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use przmterm_core::Protocol;
use przmterm_session::vault::VaultEntry;
use przmterm_session::{Session, Target};
use przmterm_transport::serial::SerialTransport;
use przmterm_transport::ssh::{
    Credential, PasswordStore, SavePassword, SshConfig, SshTransport, Store,
};
use przmterm_transport::telnet::{TelnetConfig, TelnetTransport};
use przmterm_transport::x11::X11Display;
use przmterm_transport::{Size, Transport, TransportError, Waker};

/// What connecting needs from the application.
pub struct Env {
    pub known_hosts: PathBuf,
    pub config_dir: Option<PathBuf>,
    /// Every saved session, for jump hosts named by session.
    pub sessions: Vec<Session>,
    /// The password vault's entries, while it is unlocked.
    pub credentials: Option<Arc<Vec<VaultEntry>>>,
    /// Nobody reads the terminal (`przm run`): never ask whether to save a
    /// password.
    pub headless: bool,
}

/// The vault entries `session` and its jump hosts log in with.
pub fn vault_entries_used(session: &Session, env: &Env) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = Some(session.clone());
    for _ in 0..=MAX_JUMPS {
        let Some(s) = current.take() else { break };
        if s.protocol != Protocol::Ssh {
            break;
        }
        if let Some(name) = &s.credential
            && !names.contains(name)
        {
            names.push(name.clone());
        }
        current = s.jump_host.as_deref().and_then(|j| find_jump(j, env).ok());
    }
    names
}

/// Jump hosts deeper than this are taken as a loop.
const MAX_JUMPS: usize = 5;

/// The SSH settings of a session, with its jump hosts resolved.
pub fn ssh_config(session: &Session, env: &Env, depth: usize) -> Result<SshConfig, String> {
    let Target::Network {
        host,
        port,
        username,
    } = &session.target
    else {
        return Err(format!("{} is not an SSH session", session.name));
    };
    let mut config = SshConfig::new(host.clone(), *port, &env.known_hosts);
    config.username = username.clone();
    // A locked vault logs in as if there were no entry: everything is asked.
    if let Some(name) = &session.credential
        && let Some(entries) = &env.credentials
    {
        let entry = entries
            .iter()
            .find(|e| &e.name == name)
            .ok_or_else(|| format!("{}: the vault has no entry named {name}", session.name))?;
        if !entry.username.is_empty() {
            config.username = Some(entry.username.clone());
        }
        let some = |s: &String| (!s.is_empty()).then(|| s.clone());
        config.credential = Some(Credential {
            name: name.clone(),
            password: some(&entry.password),
            passphrase: some(&entry.passphrase),
        });
    }
    config.term_type = session.term_type.clone();
    config.identity_files = session.identity_file.iter().cloned().collect();
    config.debug = session.debug;
    config.keepalive = session.keepalive;
    config.legacy_algorithms = session.legacy_algorithms;
    if session.x11_forwarding && depth == 0 {
        let mut display = X11Display::from_env();
        display.cookie = xauth_cookie(&display.name);
        config.x11 = Some(display);
    }
    // A jump host's own tunnels are not opened.
    config.forwards = if depth == 0 {
        session.tunnels.clone()
    } else {
        Vec::new()
    };
    config.password_store = Some(Store(Arc::new(Keychain)));
    config.save_password = match session.save_password {
        None if env.headless => SavePassword::Never,
        None => SavePassword::Ask,
        Some(true) => SavePassword::Always,
        Some(false) => SavePassword::Never,
    };
    if let Some(jump) = &session.jump_host {
        if depth >= MAX_JUMPS {
            return Err(format!(
                "Too many jump hosts from {} (is there a loop?)",
                session.name
            ));
        }
        let via = find_jump(jump, env)?;
        config.jump = Some(Box::new(ssh_config(&via, env, depth + 1)?));
    }
    Ok(config)
}

/// The real cookie of a local X display, from `xauth` (XQuartz installs it
/// in /opt/X11/bin, which apps started from the Finder do not have in PATH).
fn xauth_cookie(display: &str) -> Option<Vec<u8>> {
    ["xauth", "/opt/X11/bin/xauth"].iter().find_map(|xauth| {
        let out = std::process::Command::new(xauth)
            .args(["list", display])
            .output()
            .ok()?;
        przmterm_transport::x11::cookie_from_xauth_list(&String::from_utf8_lossy(&out.stdout))
    })
}

/// A jump host: a saved session by name, else `user@host:port`.
pub fn find_jump(name: &str, env: &Env) -> Result<Session, String> {
    let name = name.trim();
    if let Some(s) = env
        .sessions
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name) && s.protocol == Protocol::Ssh)
    {
        return Ok(s.clone());
    }
    Session::quick_connect(name).map_err(|e| format!("Jump host `{name}`: {e}"))
}

/// Starts the connection behind a session.
pub fn connect(session: &Session, env: &Env, size: Size, waker: Waker) -> Box<dyn Transport> {
    match (&session.protocol, &session.target) {
        (Protocol::Ssh, Target::Network { .. }) => match ssh_config(session, env, 0) {
            Ok(config) => Box::new(SshTransport::connect(config, size, waker)),
            Err(e) => message(&format!("\x1b[31m{e}\x1b[0m\r\n")),
        },
        (Protocol::Telnet, Target::Network { host, port, .. }) => {
            let mut config = TelnetConfig::new(host.clone(), *port);
            config.term_type = session.term_type.clone();
            config.keepalive = session.keepalive;
            Box::new(TelnetTransport::connect(config, size, waker))
        }
        (Protocol::Serial, Target::Serial(line)) => {
            Box::new(SerialTransport::open(line.clone(), waker))
        }
        (protocol, _) => message(&format!(
            "\x1b[33m{protocol} sessions arrive in a later version of PrzmTerm.\x1b[0m\r\n"
        )),
    }
}

/// A transport that only shows `text` and is already closed.
pub fn message(text: &str) -> Box<dyn Transport> {
    Box::new(Message(text.as_bytes().to_vec().into()))
}

struct Message(VecDeque<u8>);

impl Transport for Message {
    fn protocol(&self) -> Protocol {
        Protocol::Local
    }
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let n = buf.len().min(self.0.len());
        for (slot, b) in buf.iter_mut().zip(self.0.drain(..n)) {
            *slot = b;
        }
        Ok(n)
    }
    fn write(&mut self, _: &[u8]) -> Result<(), TransportError> {
        Err(TransportError::Closed)
    }
    fn resize(&mut self, _: Size) -> Result<(), TransportError> {
        Ok(())
    }
    fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
    fn is_closed(&self) -> bool {
        true
    }
}

const SERVICE: &str = "PrzmTerm";

/// Saved passwords in the OS keychain: Keychain on macOS, Credential Manager
/// on Windows, the Secret Service (GNOME Keyring, KWallet) on Linux.
///
/// On macOS, builds that are not signed count as a new app each time they
/// change, so the system asks for the login keychain password (the Mac
/// account password) and "Always Allow" again after every rebuild.
pub struct Keychain;

impl PasswordStore for Keychain {
    fn get(&self, account: &str) -> Option<String> {
        keyring::Entry::new(SERVICE, account)
            .ok()?
            .get_password()
            .ok()
    }

    fn set(&self, account: &str, password: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, account).map_err(|e| e.to_string())?;
        match entry.set_password(password) {
            Ok(()) => Ok(()),
            // macOS: an item saved by an earlier (differently signed) build
            // cannot be updated in place. Replace it.
            Err(e) if e.to_string().contains("already exists") => {
                let _ = entry.delete_credential();
                entry.set_password(password).map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn delete(&self, account: &str) {
        if let Ok(entry) = keyring::Entry::new(SERVICE, account) {
            let _ = entry.delete_credential();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(toml: &str, name: &str) -> Session {
        let settings = przmterm_session::SessionSettings::from_toml(toml).unwrap();
        Session::resolve(name, Vec::new(), format!("{name}.toml"), &settings).unwrap()
    }

    #[test]
    fn vault_entries_fill_in_the_login_of_sessions_and_jump_hosts() {
        let bastion = session("host = 'bastion.example'\ncredential = 'Ops'", "Bastion");
        let db = session(
            "host = '10.0.0.5'\nusername = 'local'\njump_host = 'bastion'\ncredential = 'DB'",
            "db",
        );
        let entry = |name: &str, user: &str, password: &str| VaultEntry {
            name: name.into(),
            username: user.into(),
            password: password.into(),
            passphrase: String::new(),
        };
        let mut env = Env {
            known_hosts: PathBuf::from("/tmp/kh"),
            config_dir: None,
            sessions: vec![bastion, db.clone()],
            credentials: None,
            headless: false,
        };
        assert_eq!(vault_entries_used(&db, &env), ["DB", "Ops"]);

        // Locked: the session's own user name, everything else asked.
        let config = ssh_config(&db, &env, 0).unwrap();
        assert_eq!(config.username.as_deref(), Some("local"));
        assert!(config.credential.is_none());

        env.credentials = Some(Arc::new(vec![
            entry("DB", "dba", "pw1"),
            entry("Ops", "", "pw2"),
        ]));
        let config = ssh_config(&db, &env, 0).unwrap();
        assert_eq!(
            config.username.as_deref(),
            Some("dba"),
            "the entry's user wins"
        );
        let c = config.credential.unwrap();
        assert_eq!(
            (c.name.as_str(), c.password.as_deref()),
            ("DB", Some("pw1"))
        );
        assert!(c.passphrase.is_none());
        let jump = config.jump.unwrap();
        assert_eq!(
            jump.username, None,
            "an entry without a user name asks for it"
        );
        assert_eq!(jump.credential.unwrap().password.as_deref(), Some("pw2"));

        env.credentials = Some(Arc::new(vec![entry("Ops", "", "pw2")]));
        let err = ssh_config(&db, &env, 0).unwrap_err();
        assert!(err.contains("no entry named DB"), "{err}");
    }

    #[test]
    fn jump_hosts_resolve_by_name_or_address_and_loops_stop() {
        let bastion = session("host = 'bastion.example'\nusername = 'ops'", "Bastion");
        let db = session("host = '10.0.0.5'\njump_host = 'bastion'", "db");
        let env = Env {
            known_hosts: PathBuf::from("/tmp/kh"),
            config_dir: None,
            credentials: None,
            sessions: vec![bastion, db.clone()],
            headless: false,
        };
        let config = ssh_config(&db, &env, 0).unwrap();
        let jump = config.jump.unwrap();
        assert_eq!(jump.host, "bastion.example");
        assert_eq!(jump.username.as_deref(), Some("ops"));

        let direct = session("host = '10.0.0.6'\njump_host = 'me@gw:2200'", "x");
        let jump = ssh_config(&direct, &env, 0).unwrap().jump.unwrap();
        assert_eq!((jump.host.as_str(), jump.port), ("gw", 2200));

        let looped = session("host = 'a'\njump_host = 'loop'", "loop");
        let env = Env {
            sessions: vec![looped.clone()],
            ..env
        };
        assert!(ssh_config(&looped, &env, 0).unwrap_err().contains("loop"));
    }
}
