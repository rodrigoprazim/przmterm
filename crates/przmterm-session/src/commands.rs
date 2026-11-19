//! The `add`, `delete` and `export` commands of the PrzmTerm shell, shared
//! with `przm`: the words of an `add` line, the line `export` prints for a
//! session, folders found whatever their letter case, and the vault entry
//! `add -c` names.
//!
//! The words are read as a shell reads them, so `export`'s lines go back
//! through `add` unchanged.

use std::path::{Path, PathBuf};

use przmterm_core::Protocol;

use crate::{Session, SessionSettings, Target};

/// `add`'s options, for help texts.
pub const ADD_OPTIONS: &str = "\
  <target>          [user@]host[:port], telnet://host[:port] or serial://port[@baud]
  -f <folder>       the folder, e.g. Lab/Core (made if it is not there)
  -l <user>         the user (or user@ in the target)
  -p <port>         the port (or :port in the target)
  -c <credential>   an entry of the password vault
  -i <key file>     an SSH private key
  -J <jump host>    connect through this host
  -d <text>         a description
  --replace         write over a session of that name
";

/// The `add` line that makes `session` again: where it connects and how
/// it logs in, its folder and description (other settings stay the new
/// PrzmTerm's defaults).
pub fn export_line(session: &Session) -> Option<String> {
    let target = match &session.target {
        Target::Network {
            host,
            port,
            username,
        } => {
            let scheme = if session.protocol == Protocol::Telnet {
                "telnet://"
            } else {
                ""
            };
            let user = username
                .as_deref()
                .map(|u| format!("{u}@"))
                .unwrap_or_default();
            let host = if host.contains(':') {
                format!("[{host}]")
            } else {
                host.clone()
            };
            // The protocol's own port goes without saying.
            if session.protocol.default_port() == Some(*port) {
                format!("{scheme}{user}{host}")
            } else {
                format!("{scheme}{user}{host}:{port}")
            }
        }
        Target::Serial(line) => format!("serial://{}@{}", line.port, line.baud_rate),
        Target::Local => return None,
    };
    let mut words = vec!["add".to_string(), quote(&session.name), quote(&target)];
    let mut option = |flag: &str, value: &str| {
        words.push(flag.to_string());
        words.push(quote(value));
    };
    if !session.folder.is_empty() {
        option("-f", &session.folder.join("/"));
    }
    if let Some(c) = &session.credential {
        option("-c", c);
    }
    if let Some(key) = &session.identity_file {
        option("-i", &key.display().to_string());
    }
    if let Some(jump) = &session.jump_host {
        option("-J", jump);
    }
    if let Some(d) = &session.description {
        option("-d", d);
    }
    Some(words.join(" "))
}

/// A folder path as typed (`" Lab / Core/"`) as it is kept (`Lab/Core`).
pub fn clean_folder(folder: &str) -> String {
    folder
        .split('/')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// The sessions and folders inside `dir`, all the way down.
pub fn count_inside(dir: &std::path::Path) -> (usize, usize) {
    let mut counts = (0, 0);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            let (s, f) = count_inside(&path);
            counts = (counts.0 + s, counts.1 + f + 1);
        } else if path.extension().is_some_and(|e| e == "toml")
            && path.file_name().is_some_and(|n| n != crate::FOLDER_FILE)
        {
            counts.0 += 1;
        }
    }
    counts
}

/// The names of the folders in `dir`.
pub fn subfolders(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect()
}

/// What `add` makes, from its words.
#[derive(Debug)]
pub struct AddSpec {
    pub name: String,
    /// Folders, `A/B`; `""` for the top level.
    pub folder: String,
    pub settings: SessionSettings,
    /// Where it connects, to say so.
    pub shown: String,
    pub replace: bool,
}

impl AddSpec {
    pub fn parse(words: &[String]) -> Result<AddSpec, String> {
        let mut positional = Vec::new();
        let mut folder = String::new();
        let mut user = None;
        let mut port = None;
        let mut settings = SessionSettings::default();
        let mut replace = false;
        let mut words = words.iter();
        while let Some(word) = words.next() {
            let mut value = |flag: &str| {
                words
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            match word.as_str() {
                "-f" | "--folder" => folder = value(word)?,
                "-l" | "--user" => user = Some(value(word)?),
                "-p" | "--port" => port = Some(value(word)?),
                "-c" | "--credential" => settings.credential = Some(value(word)?),
                "-i" | "--key" => settings.identity_file = Some(value(word)?),
                "-J" | "--jump" => settings.jump_host = Some(value(word)?),
                "-d" | "--description" => settings.description = Some(value(word)?),
                "--replace" => replace = true,
                flag if flag.starts_with('-') && flag.len() > 1 => {
                    return Err(format!("unknown option {flag}"));
                }
                _ => positional.push(word.clone()),
            }
        }
        let (name, target) = match positional.as_slice() {
            [name, target] => (name.trim().to_string(), target.clone()),
            [_] => return Err("say where it connects: add <name> <target>".into()),
            [] => return Err("say a name and where it connects: add <name> <target>".into()),
            _ => {
                return Err(format!(
                    "too many words ({}): quote a name with spaces",
                    positional.join(" ")
                ));
            }
        };
        let folder = clean_folder(&folder);
        // The target as Quick connect takes it, with -l and -p in it.
        let serial = target.starts_with("serial://");
        let mut quick = target.clone();
        if let Some(user) = &user {
            if serial || target.contains('@') {
                return Err("a user is given twice (-l and user@)".into());
            }
            quick = match quick.split_once("://") {
                Some((scheme, rest)) => format!("{scheme}://{user}@{rest}"),
                None => format!("{user}@{quick}"),
            };
        }
        if let Some(port) = &port {
            if serial {
                return Err("-p is for a network host, not a serial port".into());
            }
            port.parse::<u16>()
                .map_err(|_| format!("`{port}` is not a port"))?;
            quick.push(':');
            quick.push_str(port);
        }
        let session = Session::quick_connect(&quick)?;
        settings.protocol = Some(session.protocol);
        match &session.target {
            Target::Network {
                host,
                port,
                username,
            } => {
                settings.host = Some(host.clone());
                // The protocol's own port is left to it.
                if session.protocol.default_port() != Some(*port) {
                    settings.port = Some(*port);
                }
                settings.username = username.clone();
            }
            Target::Serial(line) => {
                settings.serial_port = Some(line.port.clone());
                if target.contains('@') {
                    settings.baud_rate = Some(line.baud_rate);
                }
            }
            Target::Local => return Err("a local shell is not something to add".into()),
        }
        for value in [
            &mut settings.credential,
            &mut settings.identity_file,
            &mut settings.jump_host,
            &mut settings.description,
        ] {
            if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                *value = None;
            }
        }
        Ok(AddSpec {
            shown: format!("{} {}", session.protocol, session.display_target()),
            name,
            folder,
            settings,
            replace,
        })
    }

    /// The file it is written to.
    pub fn file(&self, config_dir: &std::path::Path) -> PathBuf {
        let mut path = config_dir.join(crate::SESSIONS_DIR);
        for part in self.folder.split('/').filter(|p| !p.is_empty()) {
            path.push(part);
        }
        path.join(format!("{}.toml", self.name))
    }
}

/// `folder` (`A/B`) with each part as the folder already there writes it,
/// letter case aside; parts not there as they are.
pub fn existing_folder(config_dir: &std::path::Path, folder: &str) -> String {
    let mut dir = config_dir.join(crate::SESSIONS_DIR);
    let mut parts = Vec::new();
    for part in folder.split('/').filter(|p| !p.is_empty()) {
        let found = std::fs::read_dir(&dir).ok().and_then(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .find(|name| name.eq_ignore_ascii_case(part))
        });
        let part = found.unwrap_or_else(|| part.to_string());
        dir.push(&part);
        parts.push(part);
    }
    parts.join("/")
}

/// The words of `text` as a shell splits them: spaces apart, '…' as it
/// is, "…" with \" and \\ in it, \ before a character outside quotes.
pub fn split_args(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("a ' is not closed".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("a \" is not closed".into()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("a \" is not closed".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(c) = chars.next() {
                    word.push(c);
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// `text` as one word for [`split_args`]: in quotes when it has to be.
pub fn quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '\\' | '#'));
    if plain {
        text.to_string()
    } else {
        format!(
            "\"{}\"",
            text.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', " ")
        )
    }
}

/// The folder `path` names (letter case aside) under `config_dir`: its
/// directory and its path as written there.
pub fn found_folder(config_dir: &Path, path: &str) -> Option<(PathBuf, String)> {
    let path = clean_folder(path);
    if path.is_empty() {
        return None;
    }
    let label = existing_folder(config_dir, &path);
    let mut at = config_dir.join(crate::SESSIONS_DIR);
    for part in label.split('/') {
        at.push(part);
    }
    at.is_dir().then_some((at, label))
}

/// Why `add -c` cannot use an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultMiss {
    /// There is no vault file.
    NoVault,
    /// The open vault has no such entry; its names.
    Unknown(Vec<String>),
    /// The vault is locked and no session uses the entry: opening the
    /// vault would tell.
    Locked,
}

impl VaultMiss {
    /// What to say, after `-c <name>: `.
    pub fn reason(&self) -> String {
        match self {
            VaultMiss::NoVault => "there is no password vault yet".into(),
            VaultMiss::Unknown(names) if names.is_empty() => {
                "the vault has no entry by that name (it is empty)".into()
            }
            VaultMiss::Unknown(names) if names.len() <= 8 => format!(
                "the vault has no entry by that name (it has {})",
                names.join(", ")
            ),
            VaultMiss::Unknown(names) => format!(
                "the vault has no entry by that name (it has {} entries)",
                names.len()
            ),
            VaultMiss::Locked => "the vault is locked, so the entry cannot be checked".into(),
        }
    }
}

/// The vault entry `name` stands for, as the vault writes it (letter case
/// aside). `open` is the open vault's names, `None` while it is locked:
/// then only an entry the sessions use can be found.
pub fn vault_entry(
    config_dir: &Path,
    name: &str,
    open: Option<&[String]>,
) -> Result<String, VaultMiss> {
    if !crate::vault::vault_path(config_dir).is_file() {
        return Err(VaultMiss::NoVault);
    }
    let found = |names: &[String]| names.iter().find(|n| n.eq_ignore_ascii_case(name)).cloned();
    match open {
        Some(names) => found(names).ok_or_else(|| VaultMiss::Unknown(names.to_vec())),
        None => found(&crate::vault::names_in_use(config_dir)).ok_or(VaultMiss::Locked),
    }
}

/// What `add` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// Folders and name, `Lab/Core DB`.
    pub label: String,
    /// It wrote over a session of that name.
    pub replaced: bool,
}

/// Writes the session `spec` makes under `config_dir`, into the folders
/// that are there (letter case aside), making the others. `known` is the
/// saved sessions by label and file: one with the same label (letter case
/// aside) or file is only written over with `--replace`. The vault entry
/// is checked before ([`vault_entry`]).
pub fn add_session(
    config_dir: &Path,
    mut spec: AddSpec,
    known: &[(String, PathBuf)],
) -> Result<Added, String> {
    spec.folder = existing_folder(config_dir, &spec.folder);
    let label = if spec.folder.is_empty() {
        spec.name.clone()
    } else {
        format!("{}/{}", spec.folder, spec.name)
    };
    let file = spec.file(config_dir);
    let taken = known
        .iter()
        .find(|(l, _)| l.eq_ignore_ascii_case(&label))
        .map(|(_, path)| path.clone())
        .or_else(|| file.is_file().then(|| file.clone()));
    if let Some(old) = &taken {
        if !spec.replace {
            return Err(format!(
                "{label} exists already (add --replace to write over it)"
            ));
        }
        if old != &file {
            let _ = std::fs::remove_file(old);
        }
    }
    crate::write_session(config_dir, &spec.folder, &spec.name, &spec.settings)
        .map_err(|e| e.reason())?;
    Ok(Added {
        label,
        replaced: taken.is_some(),
    })
}

/// The `add` lines of an exported list: one per line, blank lines and
/// `#` comments skipped, `add ` (or `new `) at the start optional.
pub fn add_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines().enumerate().filter_map(|(i, line)| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let rest = ["add ", "new "]
            .iter()
            .find_map(|c| line.strip_prefix(c))
            .unwrap_or(line);
        Some((i + 1, rest.trim_start()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_words_like_a_shell_and_quotes_back() {
        assert_eq!(
            split_args(r#"add "Core DB" admin@10.0.0.5 -d 'a b' x\ y "q\"t""#).unwrap(),
            [
                "add",
                "Core DB",
                "admin@10.0.0.5",
                "-d",
                "a b",
                "x y",
                "q\"t"
            ]
        );
        assert!(split_args("add \"open").is_err());
        for text in ["plain", "with space", "q\"uote", "back\\slash", "", "#tag"] {
            assert_eq!(split_args(&quote(text)).unwrap(), [text], "{text}");
        }
    }

    #[test]
    fn add_reads_targets_and_options() {
        let words = |t: &str| split_args(t).unwrap();
        let spec = AddSpec::parse(&words(
            "\"Core DB\" 10.0.0.5 -l admin -p 2222 -f ' Lab / Core ' -d \"main db\" -J bastion",
        ))
        .unwrap();
        assert_eq!(spec.name, "Core DB");
        assert_eq!(spec.folder, "Lab/Core");
        let s = &spec.settings;
        assert_eq!(s.protocol, Some(Protocol::Ssh));
        assert_eq!(s.host.as_deref(), Some("10.0.0.5"));
        assert_eq!(s.port, Some(2222));
        assert_eq!(s.username.as_deref(), Some("admin"));
        assert_eq!(s.description.as_deref(), Some("main db"));
        assert_eq!(s.jump_host.as_deref(), Some("bastion"));
        // The protocol's own port is not written.
        let spec = AddSpec::parse(&words("sw telnet://10.0.0.9")).unwrap();
        assert_eq!(spec.settings.protocol, Some(Protocol::Telnet));
        assert_eq!(spec.settings.port, None);
        // A user with @ in it (a vault proxy's): the last @ splits.
        let spec = AddSpec::parse(&words("seg 'a1[root@srv]@pam.example.com'")).unwrap();
        assert_eq!(spec.settings.username.as_deref(), Some("a1[root@srv]"));
        assert_eq!(spec.settings.host.as_deref(), Some("pam.example.com"));
        let spec = AddSpec::parse(&words("console serial:///dev/ttyUSB0@115200")).unwrap();
        assert_eq!(spec.settings.serial_port.as_deref(), Some("/dev/ttyUSB0"));
        assert_eq!(spec.settings.baud_rate, Some(115200));
        for bad in [
            "only-a-name",
            "a b c",
            "x admin@h -l root",
            "x h -p port",
            "x h -z",
        ] {
            assert!(AddSpec::parse(&words(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn add_lines_skip_comments_and_the_command() {
        let text = "# hosts\n\nadd a 10.0.0.1\n  new b h -f Lab\nc h2\n";
        let got: Vec<(usize, &str)> = add_lines(text).collect();
        assert_eq!(got, [(3, "a 10.0.0.1"), (4, "b h -f Lab"), (5, "c h2")]);
    }

    #[test]
    fn adds_into_folders_and_checks_vault_entries() {
        let dir = std::env::temp_dir().join(format!("przmterm-commands-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions/Data Center")).unwrap();
        let spec = |line: &str| AddSpec::parse(&split_args(line).unwrap()).unwrap();
        let added = add_session(&dir, spec("r1 10.0.0.1 -f \"data center/Net\""), &[]).unwrap();
        assert_eq!(added.label, "Data Center/Net/r1");
        assert!(!added.replaced);
        assert!(dir.join("sessions/Data Center/Net/r1.toml").is_file());
        let again = add_session(&dir, spec("r1 10.0.0.9 -f \"Data Center/net\""), &[]);
        assert!(again.unwrap_err().contains("exists already"));
        let replaced = add_session(
            &dir,
            spec("r1 10.0.0.9 -f \"Data Center/net\" --replace"),
            &[],
        )
        .unwrap();
        assert!(replaced.replaced);
        assert_eq!(
            found_folder(&dir, "DATA CENTER").map(|(_, l)| l).as_deref(),
            Some("Data Center")
        );
        assert_eq!(count_inside(&dir.join("sessions/Data Center")), (1, 1));

        assert_eq!(vault_entry(&dir, "x", None), Err(VaultMiss::NoVault));
        std::fs::write(crate::vault::vault_path(&dir), "").unwrap();
        let names = ["Lab Admin".to_string()];
        assert_eq!(
            vault_entry(&dir, "lab admin", Some(&names)).as_deref(),
            Ok("Lab Admin")
        );
        assert!(matches!(
            vault_entry(&dir, "x", Some(&names)),
            Err(VaultMiss::Unknown(_))
        ));
        assert_eq!(vault_entry(&dir, "x", None), Err(VaultMiss::Locked));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
