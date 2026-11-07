//! `przm run`: connects a session without a window, logs in, sends commands
//! and prints what comes back.
//!
//! The login happens as in the app: host key questions and prompts arrive in
//! the output. Saved passwords (keychain), vault entries and keys log in on
//! their own; anything else is asked on the terminal when there is one, or
//! makes the run fail with the prompt in the message.

use std::io::Write;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fancy_regex::Regex;
use przmterm_term::plain::PlainText;
use przmterm_term::triggers::TriggerEngine;
use przmterm_transport::{Size, Transport, Waker};

/// What `przm run` was asked to do.
pub struct Options {
    pub commands: Vec<String>,
    /// The last line of output is a prompt when it matches.
    pub prompt: Regex,
    /// How long to wait for the login, and for each command's prompt.
    pub timeout: Duration,
    /// Output quiet this long after a prompt: the command is done.
    pub settle: Duration,
    /// Answer "yes" to an unknown host key.
    pub accept_host_key: bool,
    /// Bytes as received instead of plain text.
    pub raw: bool,
    /// Show the login on stderr.
    pub verbose: bool,
    /// For password prompts: the value of this environment variable.
    pub password: Option<String>,
}

/// The default prompt: a line ending in `$`, `#`, `>`, `%` or `]`, with
/// optional spaces after it.
pub const DEFAULT_PROMPT: &str = r"[$#>%\]]\s*$";

/// How the run ended, as the process exit code.
#[derive(Debug, PartialEq, Eq)]
pub enum Ending {
    Done,
    /// A prompt did not come back in time.
    TimedOut(String),
}

/// Answers login prompts no saved password covers.
pub trait Asker {
    /// The answer to `prompt` (hidden when `secret`); `None`: nobody can
    /// answer.
    fn ask(&mut self, prompt: &str, secret: bool) -> Option<String>;
}

/// Asks on the terminal, when stdin is one.
pub struct TerminalAsker;

impl Asker for TerminalAsker {
    fn ask(&mut self, prompt: &str, secret: bool) -> Option<String> {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return None;
        }
        if secret {
            rpassword::prompt_password(prompt).ok()
        } else {
            eprint!("{prompt}");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).ok()?;
            Some(line.trim_end_matches(['\r', '\n']).to_string())
        }
    }
}

/// Wakes the loop when the transport has something.
#[derive(Default)]
struct Bell {
    rung: Mutex<bool>,
    cond: Condvar,
}

impl Bell {
    fn waker(self: &Arc<Self>) -> Waker {
        let bell = self.clone();
        Arc::new(move || {
            if let Ok(mut rung) = bell.rung.lock() {
                *rung = true;
                bell.cond.notify_one();
            }
        })
    }

    fn wait(&self, max: Duration) {
        let Ok(rung) = self.rung.lock() else { return };
        if let Ok((mut rung, _)) = self.cond.wait_timeout_while(rung, max, |r| !*r) {
            *rung = false;
        }
    }
}

pub fn new_waker() -> (Waker, impl Fn(Duration)) {
    let bell = Arc::new(Bell::default());
    let waker = bell.waker();
    (waker, move |max| bell.wait(max))
}

/// The output so far as plain text, and when it last changed.
struct Output {
    plain: PlainText,
    /// Plain text not yet printed.
    pending: String,
    /// The current (last, unfinished) line.
    line: String,
    last_change: Instant,
}

impl Output {
    fn new() -> Self {
        Self {
            plain: PlainText::new(),
            pending: String::new(),
            line: String::new(),
            last_change: Instant::now(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        let mut text = String::new();
        self.plain.push(bytes, &mut text);
        for c in text.chars() {
            match c {
                '\n' => self.line.clear(),
                // A carriage return alone starts the line over.
                '\r' => {}
                c => self.line.push(c),
            }
        }
        self.pending.push_str(&text);
        self.last_change = Instant::now();
    }

    fn quiet_for(&self) -> Duration {
        self.last_change.elapsed()
    }

    /// Takes the unprinted text, with line ends made `\n`.
    fn take(&mut self) -> String {
        let text = std::mem::take(&mut self.pending);
        text.replace("\r\n", "\n").replace('\r', "")
    }
}

/// What a prompt in the login asks for.
#[derive(Debug, PartialEq, Eq)]
enum Question {
    HostKey,
    SavePassword,
    Secret,
    User,
    Other,
}

fn question(line: &str) -> Option<Question> {
    let l = line.trim_end().to_lowercase();
    if l.ends_with("(yes/no)?") && l.contains("continue connecting") {
        Some(Question::HostKey)
    } else if l.ends_with("(yes/no)") && l.contains("keychain") {
        Some(Question::SavePassword)
    } else if (l.contains("password") || l.contains("passphrase") || l.contains("pin"))
        && l.ends_with(':')
    {
        Some(Question::Secret)
    } else if l.starts_with("login as") || l.ends_with("username:") || l.ends_with("login:") {
        Some(Question::User)
    } else if l.ends_with(':') || l.ends_with('?') {
        Some(Question::Other)
    } else {
        None
    }
}

/// Runs `opts` over `transport`: waits for the login, sends each command
/// and waits for the prompt after it, writing the output to `out` (and,
/// with `verbose`, the login to `log`).
pub fn drive(
    transport: &mut dyn Transport,
    triggers: &[przmterm_session::Trigger],
    opts: &Options,
    asker: &mut dyn Asker,
    wait: &dyn Fn(Duration),
    out: &mut dyn Write,
    log: &mut dyn Write,
) -> Result<Ending> {
    let (mut engine, _) = TriggerEngine::new(triggers);
    let mut output = Output::new();
    let mut buf = vec![0u8; 16 * 1024];
    let mut read = |transport: &mut dyn Transport,
                    output: &mut Output,
                    engine: &mut TriggerEngine,
                    mut raw: Option<&mut dyn Write>|
     -> Result<bool> {
        let mut got = false;
        loop {
            match transport.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    got = true;
                    output.push(&buf[..n]);
                    if let Some(raw) = raw.as_deref_mut() {
                        raw.write_all(&buf[..n])?;
                    }
                    for send in engine.feed(&buf[..n]) {
                        for chunk in przmterm_session::buttons::parse_send(&send) {
                            if let przmterm_session::buttons::Chunk::Bytes(b) = chunk {
                                let _ = transport.write(&b);
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }
        Ok(got)
    };

    // The login: answer what can be answered until a prompt shows.
    let start = Instant::now();
    let mut answered_line: Option<String> = None;
    loop {
        read(transport, &mut output, &mut engine, None)?;
        if opts.verbose {
            log.write_all(output.take().as_bytes())?;
        }
        let line = output.line.clone();
        let quiet = output.quiet_for();
        if !line.is_empty() && quiet >= opts.settle && opts.prompt.is_match(&line).unwrap_or(false)
        {
            break;
        }
        if transport.is_closed() {
            output.pending.clear();
            bail!(
                "the connection ended before a prompt showed{}",
                last_words(&line)
            );
        }
        if quiet >= Duration::from_millis(300)
            && answered_line.as_deref() != Some(line.as_str())
            && let Some(q) = question(&line)
        {
            let answer = match q {
                Question::HostKey if opts.accept_host_key => "yes".to_string(),
                Question::HostKey => bail!(
                    "the server's host key is not known yet; check it and pass \
                     --accept-host-key (or connect once in the app)"
                ),
                Question::SavePassword => "no".to_string(),
                Question::Secret => match &opts.password {
                    Some(p) => p.clone(),
                    None => asker
                        .ask(&format!("{} ", line.trim_end()), true)
                        .with_context(|| {
                            format!(
                                "the login asks `{}`: save the password in the keychain (log \
                             in once in the app), use a vault entry, or pass --password-env",
                                line.trim()
                            )
                        })?,
                },
                Question::User => bail!(
                    "the login asks for a user name: set one in the session or give \
                     user@host"
                ),
                Question::Other => asker
                    .ask(&format!("{} ", line.trim_end()), false)
                    .with_context(|| format!("the login asks `{}`", line.trim()))?,
            };
            transport.write(format!("{answer}\r").as_bytes())?;
            answered_line = Some(line);
            continue;
        }
        if start.elapsed() > opts.timeout {
            bail!(
                "no prompt after {} s{}",
                opts.timeout.as_secs(),
                last_words(&line)
            );
        }
        wait(Duration::from_millis(50));
    }

    // Logged in: from here on, the output is the result.
    if !opts.verbose {
        output.pending.clear();
    }
    let print = |output: &mut Output, out: &mut dyn Write, raw: bool| -> Result<()> {
        let text = output.take();
        if !raw {
            out.write_all(text.as_bytes())?;
        }
        out.flush()?;
        Ok(())
    };
    // The prompt line the first command is typed on.
    if !opts.raw {
        out.write_all(output.line.as_bytes())?;
    }
    for cmd in &opts.commands {
        transport.write(format!("{cmd}\r").as_bytes())?;
        let sent = Instant::now();
        let mut changed = false;
        loop {
            let raw: Option<&mut dyn Write> = if opts.raw { Some(&mut *out) } else { None };
            if read(transport, &mut output, &mut engine, raw)? {
                changed = true;
            }
            print(&mut output, out, opts.raw)?;
            let line = &output.line;
            if changed
                && output.quiet_for() >= opts.settle
                && opts.prompt.is_match(line).unwrap_or(false)
            {
                break;
            }
            if transport.is_closed() {
                // `exit` and the like: the end, not an error.
                writeln!(out)?;
                return Ok(Ending::Done);
            }
            if sent.elapsed() > opts.timeout {
                writeln!(out)?;
                return Ok(Ending::TimedOut(cmd.clone()));
            }
            wait(Duration::from_millis(50));
        }
    }
    if !opts.raw {
        writeln!(out)?;
    }
    Ok(Ending::Done)
}

fn last_words(line: &str) -> String {
    let line = line.trim();
    if line.is_empty() {
        String::new()
    } else {
        format!(" (last output: `{line}`)")
    }
}

/// The terminal size used: wide, so lines are not cut.
pub const SIZE: Size = Size {
    cols: 200,
    rows: 50,
};

#[cfg(test)]
mod tests {
    use super::*;
    use przmterm_transport::{Protocol, TransportError};
    use std::collections::VecDeque;

    /// A far end that answers each line written with a scripted reply.
    struct Script {
        /// (text written that triggers it, reply)
        replies: VecDeque<(String, String)>,
        out: VecDeque<u8>,
        typed: String,
        closed: bool,
    }

    impl Script {
        fn new(greeting: &str, replies: &[(&str, &str)]) -> Self {
            Self {
                replies: replies
                    .iter()
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .collect(),
                out: greeting.bytes().collect(),
                typed: String::new(),
                closed: false,
            }
        }
    }

    impl Transport for Script {
        fn protocol(&self) -> Protocol {
            Protocol::Ssh
        }
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            let n = buf.len().min(self.out.len());
            for (slot, b) in buf.iter_mut().zip(self.out.drain(..n)) {
                *slot = b;
            }
            Ok(n)
        }
        fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
            self.typed.push_str(&String::from_utf8_lossy(data));
            while let Some((want, reply)) = self.replies.front().cloned() {
                if !self.typed.contains(&want) {
                    break;
                }
                self.typed = self.typed.split_once(&want).unwrap().1.to_string();
                self.replies.pop_front();
                if reply == "<close>" {
                    self.closed = true;
                } else {
                    self.out.extend(reply.bytes());
                }
            }
            Ok(())
        }
        fn resize(&mut self, _: Size) -> Result<(), TransportError> {
            Ok(())
        }
        fn close(&mut self) -> Result<(), TransportError> {
            self.closed = true;
            Ok(())
        }
        fn is_closed(&self) -> bool {
            self.closed && self.out.is_empty()
        }
    }

    struct NoOne;
    impl Asker for NoOne {
        fn ask(&mut self, _: &str, _: bool) -> Option<String> {
            None
        }
    }

    fn opts(commands: &[&str]) -> Options {
        Options {
            commands: commands.iter().map(|c| c.to_string()).collect(),
            prompt: Regex::new(DEFAULT_PROMPT).unwrap(),
            timeout: Duration::from_secs(2),
            settle: Duration::from_millis(1),
            accept_host_key: false,
            raw: false,
            verbose: false,
            password: None,
        }
    }

    fn run(t: &mut Script, o: &Options) -> (Result<Ending>, String) {
        let mut out = Vec::new();
        let mut log = Vec::new();
        let wait = |d: Duration| std::thread::sleep(d.min(Duration::from_millis(2)));
        let r = drive(t, &[], o, &mut NoOne, &wait, &mut out, &mut log);
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn sends_commands_and_prints_the_answers() {
        let mut t = Script::new(
            "Welcome\r\n\x1b[1mrouter#\x1b[0m ",
            &[
                ("show clock\r", "show clock\r\n12:00:00\r\nrouter# "),
                ("show ver\r", "show ver\r\nVersion 1\r\nrouter# "),
            ],
        );
        let (r, out) = run(&mut t, &opts(&["show clock", "show ver"]));
        assert_eq!(r.unwrap(), Ending::Done);
        assert_eq!(
            out,
            "router# show clock\n12:00:00\nrouter# show ver\nVersion 1\nrouter# \n"
        );
    }

    #[test]
    fn answers_the_password_and_host_key_when_allowed() {
        let script = || {
            Script::new(
                "The authenticity of host 'r1' can't be established.\r\n\
                 Are you sure you want to continue connecting (yes/no)? ",
                &[
                    ("yes\r", "admin@r1's password: "),
                    ("secret\r", "\r\nr1$ "),
                    ("uptime\r", "uptime\r\nup 3 days\r\nr1$ "),
                ],
            )
        };
        let mut o = opts(&["uptime"]);
        let (r, _) = run(&mut script(), &o);
        assert!(r.unwrap_err().to_string().contains("--accept-host-key"));

        o.accept_host_key = true;
        let (r, _) = run(&mut script(), &o);
        assert!(r.unwrap_err().to_string().contains("password"));

        o.password = Some("secret".into());
        let (r, out) = run(&mut script(), &o);
        assert_eq!(r.unwrap(), Ending::Done);
        assert!(out.contains("up 3 days"), "{out}");
    }

    #[test]
    fn a_missing_prompt_times_out_and_exit_ends() {
        let mut t = Script::new("r1$ ", &[("sleep\r", "sleep\r\n")]);
        let mut o = opts(&["sleep"]);
        o.timeout = Duration::from_millis(100);
        let (r, _) = run(&mut t, &o);
        assert_eq!(r.unwrap(), Ending::TimedOut("sleep".into()));

        let mut t = Script::new("r1$ ", &[("exit\r", "<close>")]);
        let (r, _) = run(&mut t, &opts(&["exit", "never"]));
        assert_eq!(r.unwrap(), Ending::Done);
    }

    #[test]
    fn tells_prompts_apart() {
        assert_eq!(question("admin@r1's password: "), Some(Question::Secret));
        assert_eq!(
            question("Enter passphrase for key: "),
            Some(Question::Secret)
        );
        assert_eq!(question("login as: "), Some(Question::User));
        assert_eq!(question("Verification code: "), Some(Question::Other));
        assert_eq!(question("router#"), None);
    }
}
