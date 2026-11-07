//! The PrzmTerm shell: the tab PrzmTerm opens with. It connects to saved
//! sessions (`ssh <name>`, with Tab completing the name) and to hosts that
//! are not saved (`ssh user@host`), like the Quick connect bar. It is not
//! a system shell: it only knows its own commands.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};

use przmterm_session::Session;
use przmterm_transport::{Protocol, Size, Transport, TransportError};

const PROMPT: &str = "\x1b[1;36mprzm\x1b[0m$ ";

/// The commands, for `help` and Tab.
const COMMANDS: [&str; 10] = [
    "ssh", "telnet", "open", "sessions", "help", "clear", "colors", "styles", "size", "version",
];

/// A saved session as the shell knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// Folders and name: `Lab/Core/router-lab`.
    pub label: String,
    pub protocol: Protocol,
    /// Where it connects, e.g. `admin@10.0.0.1:22`.
    pub target: String,
}

impl Entry {
    pub fn of(session: &Session) -> Self {
        let mut label = session.folder.join("/");
        if !label.is_empty() {
            label.push('/');
        }
        label.push_str(&session.name);
        Self {
            name: session.name.clone(),
            label,
            protocol: session.protocol,
            target: session.display_target(),
        }
    }
}

/// The saved sessions every PrzmTerm shell sees; set when they load.
static CATALOG: RwLock<Vec<Entry>> = RwLock::new(Vec::new());

pub fn set_catalog(entries: Vec<Entry>) {
    if let Ok(mut catalog) = CATALOG.write() {
        *catalog = entries;
    }
}

fn catalog() -> Vec<Entry> {
    CATALOG.read().map(|c| c.clone()).unwrap_or_default()
}

/// What the shell asks the application to do.
#[derive(Debug, Clone)]
pub enum Request {
    /// Open the saved session with this label (see [`Entry::label`]).
    Open(String),
    /// Connect to a host that is not saved.
    Connect(Box<Session>),
}

pub type Requests = Arc<Mutex<Vec<Request>>>;

pub struct Launcher {
    out: VecDeque<u8>,
    line: String,
    /// The start of a UTF-8 character typed in pieces.
    pending: Vec<u8>,
    size: Size,
    /// An escape sequence being read (arrow keys…).
    escape: Option<Vec<u8>>,
    history: Vec<String>,
    /// The history line shown, while going through it.
    browsing: Option<usize>,
    /// What was typed before going through the history.
    draft: String,
    closed: bool,
    requests: Requests,
}

impl Launcher {
    pub fn new(requests: Requests) -> Self {
        let mut shell = Self {
            out: VecDeque::new(),
            line: String::new(),
            pending: Vec::new(),
            size: Size::default(),
            escape: None,
            history: Vec::new(),
            browsing: None,
            draft: String::new(),
            closed: false,
            requests,
        };
        shell.emit(&format!(
            "\x1b[1mPrzmTerm {}\x1b[0m\r\n\
             Type \x1b[33mssh\x1b[0m and a session name to connect (\x1b[33mTab\x1b[0m \
             completes it), or \x1b[33mssh user@host\x1b[0m for a host that is not saved. \
             \x1b[33mhelp\x1b[0m lists the commands.\r\n\r\n{PROMPT}",
            przmterm_core::VERSION
        ));
        shell
    }

    fn emit(&mut self, s: &str) {
        self.out.extend(s.as_bytes());
    }

    fn error(&mut self, s: &str) {
        self.emit(&format!("\x1b[31m{s}\x1b[0m\r\n"));
    }

    fn request(&mut self, request: Request) {
        if let Ok(mut list) = self.requests.lock() {
            list.push(request);
        }
    }

    /// Shows the prompt and the line again (after it changed).
    fn redraw(&mut self) {
        let line = self.line.clone();
        self.emit(&format!("\r\x1b[K{PROMPT}{line}"));
    }

    fn run(&mut self, cmd: &str) {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let Some((&first, args)) = words.split_first() else {
            return;
        };
        match first {
            "help" | "?" => self.emit(
                "  ssh <session>            open a saved session (Tab completes the name)\r\n  \
                 ssh [user@]host[:port]   connect to a host that is not saved\r\n  \
                 \x20                        (-p port, -l user, -v shows the login steps)\r\n  \
                 telnet host [port]       connect with Telnet\r\n  \
                 open <session>           open a saved session of any kind (serial too)\r\n  \
                 sessions [text]          list the saved sessions, or those matching text\r\n  \
                 clear                    clear the screen (also Ctrl+L)\r\n\
                 \r\n  \
                 Terminal checks: colors, styles, size, version\r\n",
            ),
            "ssh" => self.ssh(args),
            "telnet" => self.telnet(args),
            "open" => match args {
                [name] => {
                    if let Some(entry) = self.find(name) {
                        self.request(Request::Open(entry.label));
                    }
                }
                _ => self.error("usage: open <session>"),
            },
            "sessions" | "ls" => self.list(&args.join(" ")),
            "colors" => self.colors(),
            "styles" => self.emit(
                "\x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \
                 \x1b[4munderline\x1b[0m \x1b[7mreverse\x1b[0m\r\n",
            ),
            "size" => {
                let Size { cols, rows } = self.size;
                self.emit(&format!("{cols} columns × {rows} rows\r\n"));
            }
            "clear" => self.emit("\x1b[2J\x1b[H"),
            "version" => self.emit(&format!("PrzmTerm {}\r\n", przmterm_core::VERSION)),
            "exit" | "logout" => {
                self.emit("This is PrzmTerm's own shell: close the tab with ×.\r\n")
            }
            other => self.error(&format!("{other}: command not found (try help)")),
        }
    }

    /// `ssh <session>` or `ssh [-p port] [-l user] [-v] [user@]host[:port]`.
    fn ssh(&mut self, args: &[&str]) {
        let mut port = None;
        let mut user = None;
        let mut verbose = false;
        let mut target = None;
        let mut args = args.iter();
        while let Some(&arg) = args.next() {
            match arg {
                "-p" => port = args.next().copied(),
                "-l" => user = args.next().copied(),
                "-v" | "-vv" | "-vvv" => verbose = true,
                flag if flag.starts_with('-') => {
                    return self.error(&format!("ssh: unknown option {flag}"));
                }
                t if target.is_none() => target = Some(t),
                _ => return self.error("ssh: one host or session at a time"),
            }
        }
        let Some(target) = target else {
            return self.error("usage: ssh <session> or ssh [user@]host[:port]");
        };
        // A saved session, unless options say otherwise.
        if port.is_none() && user.is_none() && !verbose {
            let found = matching(&catalog(), target);
            match found.len() {
                1 => return self.request(Request::Open(found[0].label.clone())),
                0 => {}
                _ => return self.ambiguous(target, &found),
            }
        }
        let mut quick = String::new();
        if verbose {
            quick.push_str("-v ");
        }
        if let Some(user) = user.filter(|_| !target.contains('@')) {
            quick.push_str(user);
            quick.push('@');
        }
        quick.push_str(target);
        if let Some(port) = port {
            if port.parse::<u16>().is_err() {
                return self.error(&format!("ssh: `{port}` is not a port"));
            }
            quick.push(':');
            quick.push_str(port);
        }
        match Session::quick_connect(&quick) {
            Ok(session) => self.request(Request::Connect(Box::new(session))),
            Err(e) => self.error(&format!("ssh: {e}")),
        }
    }

    fn telnet(&mut self, args: &[&str]) {
        let target = match args {
            [host] => format!("telnet://{host}"),
            [host, port] => format!("telnet://{host}:{port}"),
            _ => return self.error("usage: telnet host [port]"),
        };
        match Session::quick_connect(&target) {
            Ok(session) => self.request(Request::Connect(Box::new(session))),
            Err(e) => self.error(&format!("telnet: {e}")),
        }
    }

    /// The saved session `name` stands for, saying why when there is none.
    fn find(&mut self, name: &str) -> Option<Entry> {
        let found = matching(&catalog(), name);
        match found.len() {
            1 => Some(found[0].clone()),
            0 => {
                self.error(&format!(
                    "{name}: no saved session by that name (try sessions)"
                ));
                None
            }
            _ => {
                self.ambiguous(name, &found);
                None
            }
        }
    }

    fn ambiguous(&mut self, name: &str, found: &[Entry]) {
        let labels: Vec<&str> = found.iter().map(|e| e.label.as_str()).collect();
        self.error(&format!(
            "{name} is the name of several sessions; type one of: {}",
            labels.join(", ")
        ));
    }

    fn list(&mut self, filter: &str) {
        let entries = catalog();
        let wanted = filter.trim().to_lowercase();
        let shown: Vec<&Entry> = entries
            .iter()
            .filter(|e| {
                wanted.is_empty()
                    || e.label.to_lowercase().contains(&wanted)
                    || e.target.to_lowercase().contains(&wanted)
            })
            .collect();
        if shown.is_empty() {
            self.emit(if entries.is_empty() {
                "No saved sessions yet: add one with + above the session list.\r\n"
            } else {
                "No session matches.\r\n"
            });
            return;
        }
        let width = shown
            .iter()
            .map(|e| e.label.chars().count())
            .max()
            .unwrap_or(0);
        let mut text = String::new();
        for e in shown {
            let pad = " ".repeat(width - e.label.chars().count());
            text.push_str(&format!(
                "  {}{pad}  \x1b[2m{} {}\x1b[0m\r\n",
                e.label, e.protocol, e.target
            ));
        }
        self.emit(&text);
    }

    fn colors(&mut self) {
        let mut s = String::new();
        for i in 0..16 {
            s.push_str(&format!("\x1b[48;5;{i}m  "));
        }
        s.push_str("\x1b[0m\r\n");
        for row in 0..6 {
            for i in 0..36 {
                s.push_str(&format!("\x1b[48;5;{}m ", 16 + row * 36 + i));
            }
            s.push_str("\x1b[0m\r\n");
        }
        for i in 232..=255 {
            s.push_str(&format!("\x1b[48;5;{i}m "));
        }
        s.push_str("\x1b[0m\r\n");
        for i in 0..48 {
            let r = 255 - i * 5;
            let g = i * 5;
            s.push_str(&format!("\x1b[48;2;{r};{g};128m "));
        }
        s.push_str("\x1b[0m  truecolor\r\n");
        self.emit(&s);
    }

    /// Tab: completes the command, or the session name after `ssh`/`open`.
    fn complete(&mut self) {
        let (head, word) = match self.line.rfind(' ') {
            Some(i) => (self.line[..=i].to_string(), self.line[i + 1..].to_string()),
            None => (String::new(), self.line.clone()),
        };
        let command = head.split_whitespace().next().unwrap_or("");
        let options: Vec<String> = if head.trim().is_empty() {
            COMMANDS
                .iter()
                .filter(|c| c.starts_with(&word))
                .map(|c| c.to_string())
                .collect()
        } else if matches!(command, "ssh" | "open") && !word.starts_with('-') {
            completions(&catalog(), &word)
        } else {
            Vec::new()
        };
        match options.as_slice() {
            [] => self.emit("\x07"),
            [one] => {
                self.line = format!("{head}{one} ");
                self.redraw();
            }
            many => {
                let common = common_prefix(many);
                if common.chars().count() > word.chars().count() {
                    self.line = format!("{head}{common}");
                    self.redraw();
                } else {
                    let list = many.join("  ");
                    self.emit(&format!("\r\n{list}\r\n"));
                    self.redraw();
                }
            }
        }
    }

    /// Up (`back`) or down through the lines typed before.
    fn history_step(&mut self, back: bool) {
        let next = match (self.browsing, back) {
            (None, true) if !self.history.is_empty() => {
                self.draft = self.line.clone();
                Some(self.history.len() - 1)
            }
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), false) => None,
            (None, _) => return,
        };
        self.browsing = next;
        self.line = match next {
            Some(i) => self.history[i].clone(),
            None => std::mem::take(&mut self.draft),
        };
        self.redraw();
    }

    fn handle(&mut self, byte: u8) {
        if let Some(seq) = &mut self.escape {
            seq.push(byte);
            let done = match seq.as_slice() {
                [b'[' | b'O'] => false,
                [b'[' | b'O', .., last] => (0x40..=0x7e).contains(last),
                _ => true,
            };
            if done {
                let seq = self.escape.take().unwrap_or_default();
                match seq.as_slice() {
                    [b'[' | b'O', b'A'] => self.history_step(true),
                    [b'[' | b'O', b'B'] => self.history_step(false),
                    // Other keys (left, right, F-keys…) do nothing here.
                    _ => {}
                }
            }
            return;
        }
        if byte >= 0x80 {
            self.pending.push(byte);
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    let s = s.to_string();
                    self.pending.clear();
                    self.line.push_str(&s);
                    self.emit(&s);
                }
                Err(e) if e.error_len().is_some() || self.pending.len() >= 4 => {
                    self.pending.clear();
                }
                Err(_) => {}
            }
            return;
        }
        self.pending.clear();
        match byte {
            0x1b => self.escape = Some(Vec::new()),
            b'\r' | b'\n' => {
                self.emit("\r\n");
                let line = std::mem::take(&mut self.line);
                self.browsing = None;
                if !line.trim().is_empty() && self.history.last() != Some(&line) {
                    self.history.push(line.clone());
                }
                self.run(&line);
                self.emit(PROMPT);
            }
            b'\t' => self.complete(),
            0x7f | 0x08 => {
                if self.line.pop().is_some() {
                    self.emit("\x08 \x08");
                }
            }
            0x03 => {
                self.line.clear();
                self.browsing = None;
                self.emit("^C\r\n");
                self.emit(PROMPT);
            }
            // Ctrl+U: clear the line.
            0x15 => {
                self.line.clear();
                self.redraw();
            }
            0x0c => {
                self.emit("\x1b[2J\x1b[H");
                self.redraw();
            }
            b if b >= 0x20 => {
                self.line.push(b as char);
                self.out.push_back(b);
            }
            _ => {}
        }
    }
}

/// The saved sessions `name` stands for: the one with that label (folders
/// and name), else those with that name; case does not matter.
fn matching(entries: &[Entry], name: &str) -> Vec<Entry> {
    let name = name.trim_end_matches('/');
    let by_label: Vec<Entry> = entries
        .iter()
        .filter(|e| e.label.eq_ignore_ascii_case(name))
        .cloned()
        .collect();
    if !by_label.is_empty() {
        return by_label;
    }
    entries
        .iter()
        .filter(|e| e.name.eq_ignore_ascii_case(name))
        .cloned()
        .collect()
}

/// What `word` can complete to: session names, or the label (with its
/// folders) for a name several sessions share or when `word` has a `/`.
fn completions(entries: &[Entry], word: &str) -> Vec<String> {
    let lower = word.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for e in entries {
        let shared = entries.iter().filter(|o| o.name == e.name).count() > 1;
        let option = if word.contains('/') || shared {
            &e.label
        } else {
            &e.name
        };
        let hit = option.to_lowercase().starts_with(&lower)
            // A name that is also typed by its folder path.
            || e.name.to_lowercase().starts_with(&lower);
        if hit && !out.contains(option) {
            out.push(option.clone());
        }
    }
    out.sort_by_key(|o| o.to_lowercase());
    out
}

/// The longest start every option shares (case-insensitively), as the first
/// option writes it.
fn common_prefix(options: &[String]) -> String {
    let first: Vec<char> = options[0].chars().collect();
    let mut len = first.len();
    for other in &options[1..] {
        let other: Vec<char> = other.chars().collect();
        len = len.min(
            first
                .iter()
                .zip(&other)
                .take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
                .count(),
        );
    }
    first[..len].iter().collect()
}

impl Transport for Launcher {
    fn protocol(&self) -> Protocol {
        Protocol::Local
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let n = buf.len().min(self.out.len());
        for (slot, b) in buf.iter_mut().zip(self.out.drain(..n)) {
            *slot = b;
        }
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        for &b in data {
            self.handle(b);
        }
        Ok(())
    }

    fn resize(&mut self, size: Size) -> Result<(), TransportError> {
        self.size = size;
        Ok(())
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closed = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(t: &mut Launcher) -> String {
        let mut buf = [0u8; 8192];
        let n = t.read(&mut buf).unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    fn entry(label: &str) -> Entry {
        Entry {
            name: label.rsplit('/').next().unwrap().to_string(),
            label: label.to_string(),
            protocol: Protocol::Ssh,
            target: "admin@10.0.0.1:22".into(),
        }
    }

    fn shell() -> (Launcher, Requests) {
        let requests = Requests::default();
        let mut t = Launcher::new(requests.clone());
        drain(&mut t);
        (t, requests)
    }

    #[test]
    fn echoes_and_runs_commands() {
        let (mut t, _) = shell();
        t.write(b"versioX\x7fn\r").unwrap();
        let out = drain(&mut t);
        assert!(out.starts_with("versioX\x08 \x08n\r\n"));
        assert!(out.contains(&format!("PrzmTerm {}", przmterm_core::VERSION)));
    }

    #[test]
    fn up_and_down_go_through_the_history() {
        let (mut t, _) = shell();
        t.write(b"size\rversion\r").unwrap();
        drain(&mut t);
        t.write(b"x\x1b[A\x1b[A").unwrap();
        assert_eq!(t.line, "size");
        t.write(b"\x1b[B").unwrap();
        assert_eq!(t.line, "version");
        t.write(b"\x1bOB").unwrap();
        assert_eq!(t.line, "x");
        // Other keys are swallowed.
        t.write(b"\x1b[C\x1b[1;5D\x1b[15~").unwrap();
        assert_eq!(t.line, "x");
    }

    #[test]
    fn keeps_utf8_typed_in_pieces() {
        let (mut t, _) = shell();
        let bytes = "ç".as_bytes();
        t.write(&bytes[..1]).unwrap();
        t.write(&bytes[1..]).unwrap();
        assert_eq!(t.line, "ç");
        t.write(b"\x7f").unwrap();
        assert_eq!(t.line, "");
    }

    #[test]
    fn finds_sessions_by_name_or_label() {
        let entries = [
            entry("Lab/Core/router-lab"),
            entry("Lab/sw1"),
            entry("Other/sw1"),
            entry("gw"),
        ];
        assert_eq!(
            matching(&entries, "ROUTER-lab")[0].label,
            "Lab/Core/router-lab"
        );
        assert_eq!(matching(&entries, "Lab/Core/router-lab").len(), 1);
        assert_eq!(matching(&entries, "sw1").len(), 2);
        assert_eq!(matching(&entries, "Other/sw1").len(), 1);
        assert!(matching(&entries, "10.0.0.1").is_empty());

        assert_eq!(completions(&entries, "ro"), ["router-lab"]);
        assert_eq!(completions(&entries, "s"), ["Lab/sw1", "Other/sw1"]);
        assert_eq!(
            completions(&entries, "Lab/"),
            ["Lab/Core/router-lab", "Lab/sw1"]
        );
        assert_eq!(completions(&entries, "x"), Vec::<String>::new());
        assert_eq!(
            common_prefix(&["Lab/Core/router-lab".into(), "lab/sw1".into()]),
            "Lab/"
        );
    }

    #[test]
    fn tab_completes_commands_and_sessions() {
        set_catalog(vec![entry("Lab/Core/router-lab"), entry("gw")]);
        let (mut t, requests) = shell();
        t.write(b"ss\t").unwrap();
        assert_eq!(t.line, "ssh ");
        t.write(b"rou\t").unwrap();
        assert_eq!(t.line, "ssh router-lab ");
        t.write(b"\r").unwrap();
        let got = std::mem::take(&mut *requests.lock().unwrap());
        assert!(matches!(&got[..], [Request::Open(l)] if l == "Lab/Core/router-lab"));

        // Not a saved session: a quick connect.
        t.write(b"ssh -p 2222 -l admin 10.0.0.9\r").unwrap();
        let got = std::mem::take(&mut *requests.lock().unwrap());
        match &got[..] {
            [Request::Connect(s)] => assert_eq!(s.display_target(), "admin@10.0.0.9:2222"),
            other => panic!("{other:?}"),
        }
        t.write(b"open nothing\r").unwrap();
        assert!(drain(&mut t).contains("no saved session"));
        assert!(requests.lock().unwrap().is_empty());
    }
}
