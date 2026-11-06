//! Import of SecureCRT sessions.
//!
//! Three inputs are understood:
//!
//! * the XML export (*File › Export Settings*), which holds the whole session
//!   tree under `<key name="Sessions">`;
//! * a `Sessions` folder of `.ini` files (`Config/Sessions` in SecureCRT's
//!   configuration folder), where sub-folders are session folders;
//! * single session `.ini` files.
//!
//! Only what PrzmTerm can use is taken over: protocol, host, port, user name,
//! key file, jump host, serial line, terminal type, scrollback, keepalive and
//! logging. Passwords are never imported: SecureCRT stores them encrypted,
//! and PrzmTerm keeps passwords in the system keychain, asked at first login.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use przmterm_core::Protocol;
use przmterm_core::serial::{FlowControl, Parity};

use przmterm_core::keywords::{Keyword, KeywordSet, format_color};

use crate::{DEFAULT_KEEPALIVE, LogSettings, SessionSettings, TerminalSettings};

/// Scrollback above this many lines is capped on import (SecureCRT allows
/// far more than is sensible to keep in memory per pane).
pub const MAX_SCROLLBACK: usize = 100_000;

/// One value of a SecureCRT setting.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Text(String),
    Number(u32),
    /// Arrays (`Z:` in `.ini`, `<array>` in XML): their entries.
    List(Vec<String>),
}

type Fields = BTreeMap<String, Value>;

/// A session ready to be written.
#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    /// Folder names from the import folder down.
    pub folder: Vec<String>,
    pub name: String,
    pub settings: SessionSettings,
}

/// What an import found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub sessions: Vec<Imported>,
    /// Keyword highlighting lists (`Keywords/*.ini`), by name.
    pub keyword_sets: Vec<(String, KeywordSet)>,
    /// Sessions left out, with the reason.
    pub skipped: Vec<(String, String)>,
    /// Things worth telling, e.g. "12 saved passwords were not imported".
    pub notes: Vec<String>,
    passwords: usize,
    tunnels: usize,
    capped: usize,
    /// Sessions that run a logon script (Expect/Send) after connecting.
    scripted: Vec<String>,
}

impl Report {
    fn finish(mut self) -> Self {
        if self.passwords > 0 {
            self.notes.push(format!(
                "{} saved password(s) were not imported; PrzmTerm asks at the first login \
                 and can keep them in the system keychain.",
                self.passwords
            ));
        }
        if self.tunnels > 0 {
            self.notes.push(format!(
                "{} session(s) have port forwards, which were not imported; add them under \
                 Tunnels in the session editor.",
                self.tunnels
            ));
        }
        if !self.scripted.is_empty() {
            self.notes.push(format!(
                "{} session(s) use a SecureCRT logon script, which cannot be imported \
                 (SecureCRT stores it encrypted); they connect to their host and stop there: {}.",
                self.scripted.len(),
                self.scripted.join(", ")
            ));
        }
        if self.capped > 0 {
            self.notes.push(format!(
                "Scrollback was capped at {MAX_SCROLLBACK} lines in {} session(s).",
                self.capped
            ));
        }
        self
    }
}

fn text<'a>(f: &'a Fields, key: &str) -> Option<&'a str> {
    match f.get(key) {
        Some(Value::Text(s)) if !s.trim().is_empty() => Some(s.trim()),
        _ => None,
    }
}

fn number(f: &Fields, key: &str) -> Option<u32> {
    match f.get(key) {
        Some(Value::Number(n)) => Some(*n),
        _ => None,
    }
}

fn list_len(f: &Fields, key: &str) -> usize {
    match f.get(key) {
        Some(Value::List(l)) => l.len(),
        _ => 0,
    }
}

/// SecureCRT paths often point into its own folders or another machine
/// (`${VDS_USER_DATA_PATH}\SSH Logs\%H.log`, `C:\Users\…`). Those only
/// make sense on the machine they came from.
fn foreign_path(path: &str) -> bool {
    path.contains("${") || path.contains('\\') || path.get(1..2) == Some(":")
}

/// Turns one SecureCRT session into PrzmTerm settings, noting what is lost.
fn convert(name: &str, f: &Fields, report: &mut Report) -> Option<SessionSettings> {
    let protocol = match text(f, "Protocol Name").unwrap_or("SSH2") {
        "SSH2" | "SSH1" => Protocol::Ssh,
        "Telnet" => Protocol::Telnet,
        "Serial" => Protocol::Serial,
        other => {
            report.skipped.push((
                name.to_string(),
                format!("{other} sessions are not supported"),
            ));
            return None;
        }
    };
    let mut s = SessionSettings {
        protocol: (protocol != Protocol::Ssh).then_some(protocol),
        ..SessionSettings::default()
    };
    match protocol {
        Protocol::Ssh => {
            s.host = text(f, "Hostname").map(str::to_string);
            let port_key = if text(f, "Protocol Name") == Some("SSH1") {
                "[SSH1] Port"
            } else {
                "[SSH2] Port"
            };
            s.port = number(f, port_key)
                .and_then(|p| u16::try_from(p).ok())
                .filter(|&p| p != 22);
            s.username = text(f, "Username").map(str::to_string);
            if number(f, "Use Global Public Key") == Some(0)
                && let Some(key) = text(f, "Identity Filename V2")
                && !foreign_path(key)
            {
                s.identity_file = Some(key.to_string());
            }
            // A jump host is a "firewall" of type session: `Session:Folder/Name`.
            if let Some(fw) = text(f, "Firewall Name")
                && let Some(session) = fw.strip_prefix("Session:")
            {
                let jump = session.rsplit('/').next().unwrap_or(session).trim();
                if !jump.is_empty() {
                    s.jump_host = Some(jump.to_string());
                }
            }
            if number(f, "Forward X11") == Some(1) {
                s.x11_forwarding = Some(true);
            }
            if list_len(f, "Port Forward Table V3") + list_len(f, "Reverse Forward Table V3") > 0 {
                report.tunnels += 1;
            }
        }
        Protocol::Telnet => {
            s.host = text(f, "Hostname").map(str::to_string);
            s.port = number(f, "Port")
                .and_then(|p| u16::try_from(p).ok())
                .filter(|&p| p != 23);
        }
        Protocol::Serial => {
            s.serial_port = text(f, "Com Port").map(str::to_string);
            s.baud_rate = number(f, "Baud Rate").filter(|&b| b != crate::DEFAULT_BAUD_RATE);
            s.data_bits = number(f, "Data Bits")
                .and_then(|b| u8::try_from(b).ok())
                .filter(|&b| (5..8).contains(&b));
            s.parity = match number(f, "Parity") {
                Some(1) => Some(Parity::Odd),
                Some(2) => Some(Parity::Even),
                _ => None,
            };
            // 0 = one stop bit, 1 = one and a half, 2 = two.
            s.stop_bits = (number(f, "Stop Bits") == Some(2)).then_some(2);
            s.flow_control = if number(f, "CTS Flow").unwrap_or(0) != 0
                || number(f, "RTS Flow Control").unwrap_or(0) == 2
            {
                Some(FlowControl::Hardware)
            } else if number(f, "XON Flow").unwrap_or(0) != 0 {
                Some(FlowControl::Software)
            } else {
                None
            };
        }
        Protocol::Local => {}
    }
    if text(f, "Password V2").is_some() || text(f, "Password").is_some() {
        report.passwords += 1;
    }
    if number(f, "Use Login Script") == Some(1) {
        report.scripted.push(name.to_string());
    }

    let term_type = match text(f, "Emulation").unwrap_or("Xterm") {
        "Xterm" if number(f, "Enable Xterm-256color") == Some(1) => "xterm-256color",
        "Xterm" => "xterm",
        "VT100" => "vt100",
        "VT102" => "vt102",
        "VT220" | "VT320" => "vt220",
        "Linux" => "linux",
        "ANSI" | "SCOANSI" => "ansi",
        _ => "xterm-256color",
    };
    let scrollback = number(f, "Scrollback").map(|n| n as usize);
    if scrollback.is_some_and(|n| n > MAX_SCROLLBACK) {
        report.capped += 1;
    }
    s.terminal = TerminalSettings {
        term_type: (term_type != crate::DEFAULT_TERM_TYPE).then(|| term_type.to_string()),
        scrollback: scrollback
            .map(|n| n.min(MAX_SCROLLBACK))
            .filter(|&n| n != crate::DEFAULT_SCROLLBACK),
        ..TerminalSettings::default()
    };

    // "Send protocol NO-OP every N seconds".
    if number(f, "Idle NO-OP Check") == Some(1) {
        s.keepalive = number(f, "Idle NO-OP Timeout").filter(|&t| t != DEFAULT_KEEPALIVE);
    }
    if let Some(set) = text(f, "Keyword Set") {
        s.keywords = Some(clean_name(set));
    }
    if number(f, "Auto Reconnect") == Some(1) {
        s.auto_reconnect = Some(true);
    }

    if number(f, "Start Log Upon Connect") == Some(1) {
        let path = text(f, "Log Filename V2").map(|p| {
            // Keep the file name pattern; the folder belongs to the old machine.
            if foreign_path(p) {
                p.rsplit(['\\', '/']).next().unwrap_or(p).to_string()
            } else {
                p.to_string()
            }
        });
        s.log = LogSettings {
            enabled: Some(true),
            path: path.filter(|p| !p.is_empty() && p != crate::DEFAULT_LOG_TEMPLATE),
            dir: None,
        };
    }
    Some(s)
}

/// A file or folder name usable on every OS.
fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() || cleaned == "folder" {
        format!("session-{cleaned}")
    } else {
        cleaned
    }
}

// ---------------------------------------------------------------- XML export

/// Reads a SecureCRT XML export.
pub fn parse_xml(xml: &str) -> Result<Report, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("not valid XML: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "VanDyke" {
        return Err("not a SecureCRT export (no <VanDyke> element)".into());
    }
    let sessions = root
        .children()
        .find(|n| n.has_tag_name("key") && n.attribute("name") == Some("Sessions"))
        .ok_or("the export has no sessions (no <key name=\"Sessions\">)")?;
    let mut report = Report::default();
    walk_xml(sessions, &mut Vec::new(), &mut report);
    Ok(report.finish())
}

fn xml_fields(node: roxmltree::Node) -> Fields {
    let mut fields = Fields::new();
    for child in node.children().filter(|c| c.is_element()) {
        let Some(name) = child.attribute("name") else {
            continue;
        };
        let value = match child.tag_name().name() {
            "string" => Value::Text(child.text().unwrap_or("").to_string()),
            "dword" => match child.text().unwrap_or("").trim().parse() {
                Ok(n) => Value::Number(n),
                Err(_) => continue,
            },
            "array" => Value::List(
                child
                    .children()
                    .filter(|c| c.is_element())
                    .map(|c| c.text().unwrap_or("").to_string())
                    .collect(),
            ),
            _ => continue,
        };
        fields.insert(name.to_string(), value);
    }
    fields
}

/// SecureCRT's per-protocol session templates at the top of the tree.
fn is_template(name: &str, trail: &[String]) -> bool {
    trail.is_empty() && (name == "Default" || name.starts_with("Default_"))
}

fn walk_xml(node: roxmltree::Node, trail: &mut Vec<String>, report: &mut Report) {
    for key in node.children().filter(|c| c.has_tag_name("key")) {
        let name = key.attribute("name").unwrap_or("unnamed");
        if is_template(name, trail) {
            continue;
        }
        let fields = xml_fields(key);
        if number(&fields, "Is Session") == Some(1) {
            if let Some(settings) = convert(name, &fields, report) {
                report.sessions.push(Imported {
                    folder: trail.clone(),
                    name: clean_name(name),
                    settings,
                });
            }
        } else {
            trail.push(clean_name(name));
            walk_xml(key, trail, report);
            trail.pop();
        }
    }
}

// ---------------------------------------------------------------- .ini files

/// Parses SecureCRT's `.ini` format: `S:"Name"=text`, `D:"Name"=hex`,
/// `B:` binary and `Z:` arrays continued on lines that start with a space.
fn ini_fields(text: &str) -> Fields {
    let mut fields = Fields::new();
    let mut current_list: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix(' ') {
            if let Some(key) = &current_list
                && let Some(Value::List(items)) = fields.get_mut(key)
            {
                items.push(rest.to_string());
            }
            continue;
        }
        current_list = None;
        let Some((kind, rest)) = line.split_once(":\"") else {
            continue;
        };
        let Some((name, value)) = rest.split_once("\"=") else {
            continue;
        };
        match kind {
            "S" => {
                fields.insert(name.to_string(), Value::Text(value.to_string()));
            }
            "D" => {
                if let Ok(n) = u32::from_str_radix(value.trim(), 16) {
                    fields.insert(name.to_string(), Value::Number(n));
                }
            }
            "Z" => {
                fields.insert(name.to_string(), Value::List(Vec::new()));
                current_list = Some(name.to_string());
            }
            _ => {}
        }
    }
    fields
}

/// Reads a SecureCRT keyword list (`Keywords/<name>.ini`): entries
/// `"text",COLORREF,style,flags` where the colour is `00BBGGRR` and flag
/// bit 1 marks a regular expression. Returns the list's name and rules.
pub fn parse_keyword_ini(file_name: &str, text: &str) -> Option<(String, KeywordSet)> {
    let fields = ini_fields(text);
    let Some(Value::List(entries)) = fields.get("Keyword List V3") else {
        return None;
    };
    let name = text_field(&fields, "List Name")
        .unwrap_or(file_name)
        .to_string();
    let rules = entries
        .iter()
        .filter_map(|line| {
            // The text may contain commas, so take the numbers from the right.
            let mut parts = line.trim().rsplitn(4, ',');
            let flags = u32::from_str_radix(parts.next()?.trim(), 16).ok()?;
            let _style = parts.next()?;
            let color = u32::from_str_radix(parts.next()?.trim(), 16).ok()?;
            let quoted = parts.next()?.trim();
            let word = quoted.strip_prefix('"')?.strip_suffix('"')?.to_string();
            if word.is_empty() {
                return None;
            }
            let [r, g, b, _] = color.to_le_bytes();
            // Entries flagged as expressions that are plain words (`on`,
            // `permit`) would match inside other words ("connection"):
            // they are kept as whole words.
            let regex = flags & 2 != 0 && word.contains(|c| "\\^$.|?*+()[]{}".contains(c));
            Some(Keyword {
                text: if regex { String::new() } else { word.clone() },
                regex: if regex { word } else { String::new() },
                color: Some(format_color([r, g, b])),
                ..Keyword::default()
            })
        })
        .collect();
    Some((
        clean_name(&name),
        KeywordSet {
            match_case: number(&fields, "Match Case") == Some(1),
            rules,
        },
    ))
}

fn text_field<'a>(f: &'a Fields, key: &str) -> Option<&'a str> {
    text(f, key)
}

/// Reads one session `.ini`. `None` for files that are not sessions
/// (folder data, the default session).
pub fn parse_ini(name: &str, text: &str, folder: Vec<String>, report: &mut Report) {
    if let Some(set) = parse_keyword_ini(name, text) {
        report.keyword_sets.push(set);
        return;
    }
    let fields = ini_fields(text);
    if number(&fields, "Is Session") != Some(1) {
        return;
    }
    if let Some(settings) = convert(name, &fields, report) {
        report.sessions.push(Imported {
            folder,
            name: clean_name(name),
            settings,
        });
    }
}

/// Reads `.ini` session files, or every one under a `Sessions` folder.
pub fn parse_paths(paths: &[PathBuf]) -> Result<Report, String> {
    let mut report = Report::default();
    for path in paths {
        if path.is_dir() {
            scan_dir(path, &mut Vec::new(), &mut report)?;
        } else {
            read_ini(path, Vec::new(), &mut report)?;
        }
    }
    Ok(report.finish())
}

fn read_ini(path: &Path, folder: Vec<String>, report: &mut Report) -> Result<(), String> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");
    // SecureCRT's folder settings and session template are not sessions.
    if name == "__FolderData__" || is_template(name, &folder) {
        return Ok(());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Files are UTF-8, sometimes with a byte-order mark.
    let text = String::from_utf8_lossy(&bytes);
    parse_ini(name, text.trim_start_matches('\u{feff}'), folder, report);
    Ok(())
}

fn scan_dir(dir: &Path, trail: &mut Vec<String>, report: &mut Report) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("folder");
            trail.push(clean_name(name));
            scan_dir(&path, trail, report)?;
            trail.pop();
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ini"))
        {
            read_ini(&path, trail.clone(), report)?;
        }
    }
    Ok(())
}

/// Reads whatever was picked: an XML export, `.ini` files or folders.
pub fn parse_any(paths: &[PathBuf]) -> Result<Report, String> {
    match paths {
        [one]
            if one
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("xml")) =>
        {
            let text =
                std::fs::read_to_string(one).map_err(|e| format!("{}: {e}", one.display()))?;
            parse_xml(&text)
        }
        _ => parse_paths(paths),
    }
}

// ---------------------------------------------------------------- writing

/// Writes the imported sessions under `<config>/sessions/<into>/…`, keeping
/// SecureCRT's folders. Names already taken get ` (2)`, ` (3)`…; jump hosts
/// follow renamed sessions. Returns how many sessions were written.
pub fn write(config_dir: &Path, into: &str, report: &Report) -> Result<usize, String> {
    let into = into.trim().trim_matches('/');
    let mut taken: HashSet<(String, String)> = HashSet::new();
    let mut renamed: BTreeMap<String, String> = BTreeMap::new();
    let mut plan = Vec::new();
    for s in &report.sessions {
        let folder = std::iter::once(into)
            .filter(|f| !f.is_empty())
            .map(str::to_string)
            .chain(s.folder.iter().cloned())
            .collect::<Vec<_>>()
            .join("/");
        let dir = config_dir.join(crate::SESSIONS_DIR).join(&folder);
        let mut name = s.name.clone();
        let mut n = 2;
        while dir.join(format!("{name}.toml")).exists()
            || taken.contains(&(folder.clone(), name.clone()))
        {
            name = format!("{} ({n})", s.name);
            n += 1;
        }
        if name != s.name {
            renamed.entry(s.name.clone()).or_insert(name.clone());
        }
        taken.insert((folder.clone(), name.clone()));
        plan.push((folder, name, s.settings.clone()));
    }
    for (folder, name, mut settings) in plan {
        if let Some(jump) = &settings.jump_host
            && let Some(new) = renamed.get(jump)
        {
            settings.jump_host = Some(new.clone());
        }
        crate::write_session(config_dir, &folder, &name, &settings).map_err(|e| e.to_string())?;
    }
    for (name, set) in &report.keyword_sets {
        crate::write_keyword_set(config_dir, name, set).map_err(|e| e.to_string())?;
    }
    Ok(report.sessions.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<VanDyke version="3.0">
  <key name="Security"><string name="Passphrase">03:abc</string></key>
  <key name="Sessions">
    <key name="Lab">
      <key name="Core">
        <key name="core-sw/01">
          <dword name="Is Session">1</dword>
          <string name="Protocol Name">SSH2</string>
          <string name="Hostname">10.0.0.1</string>
          <dword name="[SSH2] Port">2222</dword>
          <string name="Username">admin</string>
          <string name="Password V2">03:deadbeef</string>
          <string name="Firewall Name">Session:Lab/bastion</string>
          <string name="Emulation">Xterm</string>
          <dword name="Enable Xterm-256color">1</dword>
          <dword name="Scrollback">1024000</dword>
          <dword name="Start Log Upon Connect">1</dword>
          <string name="Log Filename V2">${VDS_USER_DATA_PATH}\SSH Logs\%H_%D%M%Y-%h%m%s.log</string>
          <dword name="Idle NO-OP Check">1</dword>
          <dword name="Idle NO-OP Timeout">60</dword>
          <dword name="Use Login Script">1</dword>
          <dword name="Forward X11">1</dword>
          <array name="Port Forward Table V3"><string>web|127.0.0.1|8080|10.0.0.5|80</string></array>
        </key>
      </key>
      <key name="bastion">
        <dword name="Is Session">1</dword>
        <string name="Protocol Name">SSH2</string>
        <string name="Hostname">bastion.example</string>
        <dword name="[SSH2] Port">22</dword>
        <string name="Firewall Name">None</string>
      </key>
    </key>
    <key name="Default"><dword name="Is Session">1</dword></key>
    <key name="Default_RDP"><dword name="Is Session">1</dword></key>
    <key name="Remote desktop">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">RDP</string>
    </key>
  </key>
</VanDyke>"#;

    #[test]
    fn reads_the_xml_export() {
        let r = parse_xml(XML).unwrap();
        assert_eq!(r.sessions.len(), 2);
        let core = &r.sessions[0];
        assert_eq!(core.folder, ["Lab", "Core"]);
        assert_eq!(core.name, "core-sw-01", "slash replaced");
        let s = &core.settings;
        assert_eq!(s.protocol, None, "ssh is the default");
        assert_eq!(s.host.as_deref(), Some("10.0.0.1"));
        assert_eq!(s.port, Some(2222));
        assert_eq!(s.username.as_deref(), Some("admin"));
        assert_eq!(s.jump_host.as_deref(), Some("bastion"));
        assert_eq!(s.terminal.term_type, None, "xterm-256color is the default");
        assert_eq!(s.terminal.scrollback, Some(MAX_SCROLLBACK));
        assert_eq!(s.keepalive, Some(60));
        assert_eq!(s.x11_forwarding, Some(true));
        assert_eq!(s.log.enabled, Some(true));
        assert_eq!(s.log.path.as_deref(), Some("%H_%D%M%Y-%h%m%s.log"));
        assert_eq!(
            r.sessions[1].settings.port, None,
            "port 22 left to the default"
        );
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].1.contains("RDP"));
        let notes = r.notes.join("\n");
        assert!(notes.contains("1 saved password"), "{notes}");
        assert!(notes.contains("port forwards"), "{notes}");
        assert!(notes.contains("capped"), "{notes}");
        assert!(
            notes.contains("logon script") && notes.contains("core-sw/01"),
            "{notes}"
        );
        assert!(r.skipped.iter().all(|(n, _)| !n.starts_with("Default")));
        assert!(parse_xml("<other/>").is_err());
    }

    const INI: &str = "S:\"Username\"=root\r\n\
S:\"Password V2\"=03:00ff\r\n\
B:\"Normal Font v2\"=00000060\r\n\
 f0 ff ff ff 00 00 00 00\r\n\
D:\"Is Session\"=00000001\r\n\
S:\"Protocol Name\"=SSH2\r\n\
S:\"Hostname\"=192.168.0.10\r\n\
D:\"[SSH2] Port\"=00000016\r\n\
D:\"Use Global Public Key\"=00000000\r\n\
S:\"Identity Filename V2\"=/Users/me/.ssh/lab\r\n\
S:\"Emulation\"=VT100\r\n\
D:\"Scrollback\"=00000200\r\n\
Z:\"SCP Shell Prompts\"=00000001\r\n\
 \"? \",0,\"\\n\"\r\n\
Z:\"Port Forward Table V3\"=00000000\r\n";

    #[test]
    fn reads_ini_sessions() {
        let mut r = Report::default();
        parse_ini("VM", INI, vec!["Home".into()], &mut r);
        let s = &r.sessions[0].settings;
        assert_eq!(s.host.as_deref(), Some("192.168.0.10"));
        assert_eq!(s.port, None);
        assert_eq!(s.username.as_deref(), Some("root"));
        assert_eq!(s.identity_file.as_deref(), Some("/Users/me/.ssh/lab"));
        assert_eq!(s.terminal.term_type.as_deref(), Some("vt100"));
        assert_eq!(s.terminal.scrollback, Some(512));
        let r = r.finish();
        assert!(r.notes[0].contains("password"));
        assert_eq!(r.tunnels, 0, "an empty table is no tunnel");

        let telnet = "D:\"Is Session\"=00000001\nS:\"Protocol Name\"=Telnet\n\
                      S:\"Hostname\"=rtr\nD:\"Port\"=00000917\n";
        let serial = "D:\"Is Session\"=00000001\nS:\"Protocol Name\"=Serial\n\
                      S:\"Com Port\"=COM3\nD:\"Baud Rate\"=0001c200\nD:\"Data Bits\"=00000007\n\
                      D:\"Parity\"=00000002\nD:\"Stop Bits\"=00000002\nD:\"CTS Flow\"=00000001\n";
        let folder = "D:\"Is Session\"=00000000\n";
        let mut r = Report::default();
        parse_ini("rtr", telnet, vec![], &mut r);
        parse_ini("console", serial, vec![], &mut r);
        parse_ini("__FolderData__", folder, vec![], &mut r);
        assert_eq!(r.sessions.len(), 2);
        let t = &r.sessions[0].settings;
        assert_eq!((t.protocol, t.port), (Some(Protocol::Telnet), Some(2327)));
        let c = &r.sessions[1].settings;
        assert_eq!(c.serial_port.as_deref(), Some("COM3"));
        assert_eq!(c.baud_rate, Some(115_200));
        assert_eq!(c.data_bits, Some(7));
        assert_eq!(c.parity, Some(Parity::Even));
        assert_eq!(c.stop_bits, Some(2));
        assert_eq!(c.flow_control, Some(FlowControl::Hardware));
    }

    #[test]
    fn reads_keyword_lists() {
        let ini = "D:\"Match Case\"=00000000\r\nS:\"List Name\"=CORES\r\n\
Z:\"Keyword List V3\"=00000004\r\n \"no-\",00ffffff,00000000,00000001\r\n \
\"up\",0000ff00,00000000,00000001\r\n \
\"([0-9]{1,3})\\.([0-9]{1,3})\",00ff55ff,00000005,00000007\r\n \
\"permit\",00ffff00,00000000,00000003\r\n";
        let (name, set) = parse_keyword_ini("file", ini).unwrap();
        assert_eq!(name, "CORES");
        assert!(!set.match_case);
        assert_eq!(set.rules.len(), 4);
        assert_eq!(set.rules[3].text, "permit", "a plain word stays a word");
        assert_eq!(set.rules[0].text, "no-");
        assert_eq!(set.rules[0].color.as_deref(), Some("#ffffff"));
        assert_eq!(
            set.rules[1].color.as_deref(),
            Some("#00ff00"),
            "COLORREF is BGR"
        );
        assert_eq!(
            set.rules[2].regex, r"([0-9]{1,3})\.([0-9]{1,3})",
            "commas kept"
        );
        assert_eq!(set.rules[2].color.as_deref(), Some("#ff55ff"));
        // Through the normal import, written next to the sessions.
        let dir = tempfile::tempdir().unwrap();
        let mut r = Report::default();
        parse_ini("CORES", ini, vec![], &mut r);
        assert!(r.sessions.is_empty());
        write(dir.path(), "SecureCRT", &r).unwrap();
        assert_eq!(crate::keyword_sets(dir.path()), ["CORES"]);
        assert_eq!(crate::read_keyword_set(dir.path(), "CORES").unwrap(), set);
    }

    #[test]
    fn writes_into_a_folder_and_renames_clashes() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = parse_xml(XML).unwrap();
        assert_eq!(write(dir.path(), "SecureCRT", &r).unwrap(), 2);
        // A second import does not overwrite, and jump hosts follow renames.
        r.sessions.reverse();
        write(dir.path(), "SecureCRT", &r).unwrap();
        let tree = crate::SessionTree::load(dir.path());
        assert!(tree.errors.is_empty(), "{:?}", tree.errors);
        let all = tree.root.all_sessions();
        assert_eq!(all.len(), 4);
        let copy = all.iter().find(|s| s.name == "core-sw-01 (2)").unwrap();
        assert_eq!(copy.folder, ["SecureCRT", "Lab", "Core"]);
        assert_eq!(copy.jump_host.as_deref(), Some("bastion (2)"));
    }

    #[test]
    fn scans_a_sessions_folder() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("Sessions");
        std::fs::create_dir_all(sessions.join("Home")).unwrap();
        std::fs::write(sessions.join("Default.ini"), INI).unwrap();
        std::fs::write(
            sessions.join("__FolderData__.ini"),
            "S:\"Folder List\"=Home\n",
        )
        .unwrap();
        std::fs::write(sessions.join("Home/VM.ini"), format!("\u{feff}{INI}")).unwrap();
        let r = parse_any(&[sessions]).unwrap();
        assert_eq!(r.sessions.len(), 1, "Default.ini is a template");
        assert_eq!(r.sessions[0].folder, ["Home"]);
        assert_eq!(r.sessions[0].name, "VM");
    }
}
