//! The PrzmTerm shell: the tab PrzmTerm opens with. It connects to saved
//! sessions (`ssh <name>`, with Tab completing the name) and to hosts that
//! are not saved (`ssh user@host`), like the Quick connect bar, and adds,
//! deletes and exports saved sessions (`add`, `delete`, `export`: lines
//! that `export` prints in one PrzmTerm are pasted into another). It is not
//! a system shell: it only knows its own commands.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use przmterm_session::{Session, SessionSettings, Target};
use przmterm_transport::{Protocol, Size, Transport, TransportError};

const PROMPT: &str = "\x1b[1;36mprzmterm\x1b[0m \x1b[1;34m~>\x1b[0m ";

/// The commands, for `help` and Tab.
const COMMANDS: [&str; 13] = [
    "ssh", "telnet", "open", "sessions", "add", "delete", "export", "help", "clear", "colors",
    "styles", "size", "version",
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
    /// Its settings file.
    pub path: PathBuf,
    pub description: Option<String>,
    /// The `add` line that makes it again (`export`); `None` for what
    /// `add` cannot make (a local shell).
    pub export: Option<String>,
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
            path: session.path.clone(),
            description: session.description.clone(),
            export: export_line(session),
        }
    }
}

/// The `add` line that makes `session` again: where it connects and how
/// it logs in, its folder and description (other settings stay the new
/// PrzmTerm's defaults).
fn export_line(session: &Session) -> Option<String> {
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

/// The settings folder, where `add` and `delete` work; set at start.
static CONFIG_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

pub fn set_config_dir(dir: Option<PathBuf>) {
    if let Ok(mut slot) = CONFIG_DIR.write() {
        *slot = dir;
    }
}

fn config_dir() -> Option<PathBuf> {
    CONFIG_DIR.read().ok().and_then(|d| d.clone())
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
    /// Connect to a host that is not saved: a quick-connect string
    /// ([`Session::quick_connect`]), checked; the app adds the global
    /// settings.
    Connect(String),
    /// Sessions were added or deleted: read them again.
    Reload,
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
    /// A session `delete` asked about: the next line answers.
    confirm_delete: Option<Entry>,
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
            confirm_delete: None,
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
        // A comment (`export` starts with one).
        if cmd.trim_start().starts_with('#') {
            return;
        }
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let Some((&first, args)) = words.split_first() else {
            return;
        };
        // All after the command, as typed: session names can have spaces.
        let rest = unquote(cmd.trim_start()[first.len()..].trim());
        match first {
            "help" | "?" => self.emit(
                "  ssh <session>            open a saved session (Tab completes the name)\r\n  \
                 ssh [user@]host[:port]   connect to a host that is not saved\r\n  \
                 \x20                        (-p port, -l user, -v shows the login steps)\r\n  \
                 telnet host [port]       connect with Telnet\r\n  \
                 open <session>           open a saved session of any kind (serial too)\r\n  \
                 sessions [text]          list the saved sessions, or those matching text\r\n  \
                 add <name> <target> ...  save a session (add with no more says how)\r\n  \
                 delete <session>         delete a saved session, after asking\r\n  \
                 export [text]            print add lines for the saved sessions, to\r\n  \
                 \x20                        paste into another PrzmTerm\r\n  \
                 clear                    clear the screen (also Ctrl+L)\r\n\
                 \r\n  \
                 Terminal checks: colors, styles, size, version\r\n",
            ),
            "add" | "new" => self.add(cmd.trim_start()[first.len()..].trim()),
            "delete" | "rm" => match rest {
                "" => self.error("usage: delete <session>"),
                name => {
                    if let Some(entry) = self.find(name) {
                        self.emit(&format!(
                            "Delete \x1b[1m{}\x1b[0m ({} {})? Its settings file is removed. \
                             [y/N] ",
                            entry.label, entry.protocol, entry.target
                        ));
                        self.confirm_delete = Some(entry);
                    }
                }
            },
            "export" => self.export(&args.join(" ")),
            "ssh" => self.ssh(rest, args),
            "telnet" => self.telnet(args),
            "open" => match rest {
                "" => self.error("usage: open <session>"),
                name => {
                    if let Some(entry) = self.find(name) {
                        self.request(Request::Open(entry.label));
                    }
                }
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
    fn ssh(&mut self, rest: &str, args: &[&str]) {
        // A saved session, its name as typed (spaces and all).
        if !rest.is_empty() && !rest.starts_with('-') {
            let found = matching(&catalog(), rest);
            match found.len() {
                1 => return self.request(Request::Open(found[0].label.clone())),
                0 => {}
                _ => return self.ambiguous(rest, &found),
            }
        }
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
            Ok(_) => self.request(Request::Connect(quick)),
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
            Ok(_) => self.request(Request::Connect(target)),
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
        let shown = filtered(&entries, filter);
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
            let about = e
                .description
                .as_deref()
                .map(|d| {
                    let line = d.lines().next().unwrap_or_default();
                    let cut: String = line.chars().take(60).collect();
                    let more = cut.len() < d.trim_end().len();
                    format!("  \x1b[3m{cut}{}\x1b[0m", if more { "…" } else { "" })
                })
                .unwrap_or_default();
            text.push_str(&format!(
                "  {}{pad}  \x1b[2m{} {}\x1b[0m{about}\r\n",
                e.label, e.protocol, e.target
            ));
        }
        self.emit(&text);
    }

    /// `add <name> <target> [options]`: writes a session file.
    fn add(&mut self, text: &str) {
        let words = match split_args(text) {
            Ok(w) => w,
            Err(e) => return self.error(&format!("add: {e}")),
        };
        if words.is_empty() {
            return self.emit(ADD_USAGE);
        }
        let spec = match AddSpec::parse(&words) {
            Ok(spec) => spec,
            Err(e) => return self.error(&format!("add: {e}")),
        };
        let Some(dir) = config_dir() else {
            return self.error("add: there is no settings folder to save sessions in");
        };
        let mut spec = spec;
        // Into a folder that is there, as it is written (`lab` is `Lab`).
        spec.folder = existing_folder(&dir, &spec.folder);
        let label = if spec.folder.is_empty() {
            spec.name.clone()
        } else {
            format!("{}/{}", spec.folder, spec.name)
        };
        // Already there: by its file, or by its label (letter case aside).
        let file = spec.file(&dir);
        let taken = catalog()
            .into_iter()
            .find(|e| e.label.eq_ignore_ascii_case(&label))
            .map(|e| e.path)
            .or_else(|| file.is_file().then(|| file.clone()));
        if let Some(old) = &taken {
            if !spec.replace {
                return self.error(&format!(
                    "add: {label} exists already (add --replace to write over it)"
                ));
            }
            if old != &file {
                let _ = std::fs::remove_file(old);
            }
        }
        match przmterm_session::write_session(&dir, &spec.folder, &spec.name, &spec.settings) {
            Ok(_) => {
                self.emit(&format!(
                    "{} \x1b[1m{label}\x1b[0m \x1b[2m{}\x1b[0m\r\n",
                    if taken.is_some() { "Replaced" } else { "Added" },
                    spec.shown
                ));
                self.request(Request::Reload);
            }
            Err(e) => self.error(&format!("add: {}", e.reason())),
        }
    }

    /// The answer to `delete`'s question.
    fn answer_delete(&mut self, answer: &str, entry: Entry) {
        if !matches!(
            answer.trim().to_lowercase().as_str(),
            "y" | "yes" | "s" | "sim"
        ) {
            return self.emit("Not deleted.\r\n");
        }
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {
                self.emit(&format!("Deleted \x1b[1m{}\x1b[0m.\r\n", entry.label));
                self.request(Request::Reload);
            }
            Err(e) => self.error(&format!("delete: {}: {e}", entry.label)),
        }
    }

    /// `export [text]`: the `add` lines for the saved sessions (those
    /// matching text).
    fn export(&mut self, filter: &str) {
        let entries = catalog();
        let shown = filtered(&entries, filter);
        let lines: Vec<&str> = shown.iter().filter_map(|e| e.export.as_deref()).collect();
        if lines.is_empty() {
            return self.emit("No saved session to export.\r\n");
        }
        let mut text = format!(
            "# PrzmTerm {}: {} session{}. Paste these lines into another PrzmTerm \
             shell to add them.\r\n",
            przmterm_core::VERSION,
            lines.len(),
            if lines.len() == 1 { "" } else { "s" }
        );
        for line in lines {
            text.push_str(line);
            text.push_str("\r\n");
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

    /// Tab: completes the command, or the session name after `ssh`, `open`
    /// or `delete`.
    fn complete(&mut self) {
        let (head, word) = match self.line.rfind(' ') {
            Some(i) => (self.line[..=i].to_string(), self.line[i + 1..].to_string()),
            None => (String::new(), self.line.clone()),
        };
        // After `ssh` or `open` without options the word is all the rest:
        // session names can have spaces.
        let (head, word) = match self.line.split_once(' ') {
            Some((cmd @ ("ssh" | "open" | "delete" | "rm"), rest))
                if !rest.trim_start().starts_with('-') && !rest.trim().is_empty() =>
            {
                (format!("{cmd} "), rest.trim_start().to_string())
            }
            _ => (head, word),
        };
        let command = head.split_whitespace().next().unwrap_or("");
        let sessions = !head.trim().is_empty();
        let options: Vec<String> = if !sessions {
            COMMANDS
                .iter()
                .filter(|c| c.starts_with(&word))
                .map(|c| c.to_string())
                .collect()
        } else if matches!(command, "ssh" | "open" | "delete" | "rm") && !word.starts_with('-') {
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
                    let cols = usize::from(self.size.cols);
                    let list = if sessions {
                        by_folder(&catalog(), many, cols)
                    } else {
                        columns(many, cols, "")
                    };
                    self.emit(&format!("\r\n{list}"));
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
                if let Some(entry) = self.confirm_delete.take() {
                    self.answer_delete(&line, entry);
                    self.emit(PROMPT);
                    return;
                }
                if !line.trim().is_empty() && self.history.last() != Some(&line) {
                    self.history.push(line.clone());
                }
                self.run(&line);
                // A question (delete's) waits for its answer instead.
                if self.confirm_delete.is_none() {
                    self.emit(PROMPT);
                }
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
                if self.confirm_delete.take().is_some() {
                    self.emit("^C Not deleted.\r\n");
                    self.emit(PROMPT);
                    return;
                }
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

const ADD_USAGE: &str = "usage: add <name> <target> [options]\r\n  \
     <target>          [user@]host[:port], telnet://host[:port] or serial://port[@baud]\r\n  \
     -f <folder>       the folder, e.g. Lab/Core (made if it is not there)\r\n  \
     -l <user>         the user (or user@ in the target)\r\n  \
     -p <port>         the port (or :port in the target)\r\n  \
     -c <credential>   a credential of the vault\r\n  \
     -i <key file>     an SSH private key\r\n  \
     -J <jump host>    connect through this host\r\n  \
     -d <text>         a description\r\n  \
     --replace         write over a session of that name\r\n\
     Quote what has spaces: add \"Core DB\" admin@10.0.0.5 -f \"Data Center\" -d \"main DB\"\r\n";

/// What `add` makes, from its words.
#[derive(Debug)]
struct AddSpec {
    name: String,
    folder: String,
    settings: SessionSettings,
    /// Where it connects, to say so.
    shown: String,
    replace: bool,
}

impl AddSpec {
    fn parse(words: &[String]) -> Result<AddSpec, String> {
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
        let folder = folder
            .split('/')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("/");
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
    fn file(&self, config_dir: &std::path::Path) -> PathBuf {
        let mut path = config_dir.join(przmterm_session::SESSIONS_DIR);
        for part in self.folder.split('/').filter(|p| !p.is_empty()) {
            path.push(part);
        }
        path.join(format!("{}.toml", self.name))
    }
}

/// `folder` (`A/B`) with each part as the folder already there writes it,
/// letter case aside; parts not there as they are.
fn existing_folder(config_dir: &std::path::Path, folder: &str) -> String {
    let mut dir = config_dir.join(przmterm_session::SESSIONS_DIR);
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
fn split_args(text: &str) -> Result<Vec<String>, String> {
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
fn quote(text: &str) -> String {
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

/// The entries whose label, target or description has `filter` in it
/// (all of them for none).
fn filtered<'a>(entries: &'a [Entry], filter: &str) -> Vec<&'a Entry> {
    let wanted = filter.trim().to_lowercase();
    entries
        .iter()
        .filter(|e| {
            wanted.is_empty()
                || e.label.to_lowercase().contains(&wanted)
                || e.target.to_lowercase().contains(&wanted)
                || e.description
                    .as_deref()
                    .is_some_and(|d| d.to_lowercase().contains(&wanted))
        })
        .collect()
}

/// `text` without the quotes around it, if it has them.
fn unquote(text: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = text.strip_prefix(q).and_then(|t| t.strip_suffix(q)) {
            return inner;
        }
    }
    text
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

/// Session `options` under the folder each one is in (the top level first),
/// in columns that fit `cols`.
fn by_folder(entries: &[Entry], options: &[String], cols: usize) -> String {
    let folder = |option: &String| {
        entries
            .iter()
            .find(|e| &e.label == option || &e.name == option)
            .and_then(|e| e.label.rsplit_once('/'))
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default()
    };
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for option in options {
        let dir = folder(option);
        match groups.iter_mut().find(|(d, _)| *d == dir) {
            Some((_, items)) => items.push(option.clone()),
            None => groups.push((dir, vec![option.clone()])),
        }
    }
    groups.sort_by_key(|(d, _)| (!d.is_empty(), d.to_lowercase()));
    if let [(dir, items)] = groups.as_slice()
        && dir.is_empty()
    {
        return columns(items, cols, "");
    }
    let mut out = String::new();
    for (dir, items) in &groups {
        let title = if dir.is_empty() { "(top level)" } else { dir };
        out.push_str(&format!("\x1b[1;34m{title}\x1b[0m\r\n"));
        out.push_str(&columns(items, cols, "  "));
    }
    out
}

/// `items` in as many columns as fit `cols`, read down then across like
/// `ls`, every line starting with `indent`. Two spaces apart, or four when
/// names have spaces of their own.
fn columns(items: &[String], cols: usize, indent: &str) -> String {
    let gap = if items.iter().any(|i| i.contains(' ')) {
        4
    } else {
        2
    };
    let len = |s: &String| s.chars().count();
    let room = cols.saturating_sub(indent.len()).max(1);
    // The most columns whose widths (and gaps) fit the room.
    let mut layout = (items.len().max(1), vec![0]);
    for across in (1..=items.len()).rev() {
        let down = items.len().div_ceil(across);
        let widths: Vec<usize> = items
            .chunks(down)
            .map(|col| col.iter().map(len).max().unwrap_or(0))
            .collect();
        let total = widths.iter().sum::<usize>() + gap * (widths.len() - 1);
        if total <= room || across == 1 {
            layout = (down, widths);
            break;
        }
    }
    let (down, widths) = layout;
    let mut out = String::new();
    for row in 0..down {
        let mut line = String::from(indent);
        for (col, width) in widths.iter().enumerate() {
            if let Some(item) = items.get(col * down + row) {
                line.push_str(item);
                line.push_str(&" ".repeat(width - len(item) + gap));
            }
        }
        out.push_str(line.trim_end());
        out.push_str("\r\n");
    }
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
            path: PathBuf::new(),
            description: None,
            export: None,
        }
    }

    /// The catalog and settings folder are shared: tests that set them
    /// take turns.
    static SHARED: Mutex<()> = Mutex::new(());

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
        let _turn = SHARED.lock().unwrap_or_else(|e| e.into_inner());
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
            [Request::Connect(s)] => assert_eq!(s, "admin@10.0.0.9:2222"),
            other => panic!("{other:?}"),
        }
        // Names with spaces, typed or completed.
        set_catalog(vec![entry("GeoFibra/GeoFibra - DB"), entry("gw")]);
        t.write(b"ssh GeoFibra/GeoFibra - DB\r").unwrap();
        let got = std::mem::take(&mut *requests.lock().unwrap());
        assert!(matches!(&got[..], [Request::Open(l)] if l == "GeoFibra/GeoFibra - DB"));
        t.write(b"open \"GeoFibra - DB\"\r").unwrap();
        let got = std::mem::take(&mut *requests.lock().unwrap());
        assert!(matches!(&got[..], [Request::Open(l)] if l == "GeoFibra/GeoFibra - DB"));
        t.write(b"ssh GeoFibra - \t").unwrap();
        assert_eq!(t.line, "ssh GeoFibra - DB ");
        t.write(b"\x15").unwrap();
        t.write(b"open nothing\r").unwrap();
        assert!(drain(&mut t).contains("no saved session"));
        assert!(requests.lock().unwrap().is_empty());
    }

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
    fn adds_exports_and_deletes_sessions() {
        let _turn = SHARED.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("przmterm-launcher-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        set_config_dir(Some(dir.clone()));
        set_catalog(Vec::new());
        let load = || {
            let tree = przmterm_session::SessionTree::load(&dir);
            let entries: Vec<Entry> = tree
                .root
                .all_sessions()
                .into_iter()
                .map(Entry::of)
                .collect();
            set_catalog(entries.clone());
            entries
        };
        let (mut t, requests) = shell();
        t.write(b"add \"Core DB\" admin@10.0.0.5:2222 -f Lab -d \"main db\"\r")
            .unwrap();
        assert!(drain(&mut t).contains("Added"));
        assert!(matches!(
            &std::mem::take(&mut *requests.lock().unwrap())[..],
            [Request::Reload]
        ));
        let entries = load();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Lab/Core DB");
        assert_eq!(entries[0].description.as_deref(), Some("main db"));
        // Not twice, unless replaced.
        t.write(b"add \"Core DB\" 10.0.0.6 -f lab\r").unwrap();
        assert!(drain(&mut t).contains("exists already"));
        // Another into `lab`: the folder `Lab` there.
        t.write(b"add sw1 10.0.0.7 -f lab\r").unwrap();
        assert!(drain(&mut t).contains("Added"));
        assert!(load().iter().any(|e| e.label == "Lab/sw1"));
        t.write(b"rm sw1\ry\r").unwrap();
        drain(&mut t);
        // Exported, the line adds the same session again.
        let line = entries[0].export.clone().unwrap();
        assert_eq!(
            line,
            r#"add "Core DB" admin@10.0.0.5:2222 -f Lab -d "main db""#
        );
        let again = AddSpec::parse(&split_args(&line).unwrap()[1..]).unwrap();
        let tree = przmterm_session::SessionTree::load(&dir);
        let first =
            przmterm_session::read_session_settings(&tree.root.all_sessions()[0].path).unwrap();
        assert_eq!(again.settings, first);
        t.write(b"export lab\r").unwrap();
        assert!(drain(&mut t).contains(&line));
        // Deleted only once said yes.
        t.write(b"delete Core DB\r").unwrap();
        assert!(drain(&mut t).contains("[y/N]"));
        t.write(b"n\r").unwrap();
        assert!(drain(&mut t).contains("Not deleted"));
        assert_eq!(load().len(), 1);
        t.write(b"rm Core DB\ryes\r").unwrap();
        assert!(drain(&mut t).contains("Deleted"));
        assert!(load().is_empty());
        set_config_dir(None);
        set_catalog(Vec::new());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lists_many_sessions_in_columns_by_folder() {
        let items: Vec<String> = ["a", "bb", "c - d", "e", "ff"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // Down then across, as many columns as fit.
        assert_eq!(
            columns(&items, 14, ""),
            "a        e\r\nbb       ff\r\nc - d\r\n"
        );
        assert_eq!(
            columns(&items, 4, "  "),
            "  a\r\n  bb\r\n  c - d\r\n  e\r\n  ff\r\n"
        );
        let entries = vec![entry("Geo/Geo - DB"), entry("Geo/Geo - Host"), entry("gw")];
        let options = completions(&entries, "g");
        assert_eq!(
            by_folder(&entries, &options, 80),
            "\x1b[1;34m(top level)\x1b[0m\r\n  gw\r\n\
             \x1b[1;34mGeo\x1b[0m\r\n  Geo - DB    Geo - Host\r\n"
        );
        // All at the top level: no titles.
        let top = vec![entry("x1"), entry("x2")];
        assert_eq!(by_folder(&top, &completions(&top, "x"), 80), "x1  x2\r\n");
    }
}
