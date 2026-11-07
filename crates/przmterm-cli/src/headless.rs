//! A session without a window: the transport, its output as plain text and
//! on a screen, the login and waiting for prompts. Used by `przm run` and
//! `przm mcp`.
//!
//! The login happens as in the app: host key questions and prompts arrive in
//! the output. Saved passwords (keychain), vault entries and keys log in on
//! their own; anything else goes to an [`Asker`], or fails with the prompt
//! in the message.

use std::io::Write;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fancy_regex::Regex;
use przmterm_term::Terminal;
use przmterm_term::plain::PlainText;
use przmterm_term::triggers::TriggerEngine;
use przmterm_transport::{Size, Transport, Waker};

/// The default prompt: a line ending in `$`, `#`, `>`, `%` or `]`, with
/// optional spaces after it.
pub const DEFAULT_PROMPT: &str = r"[$#>%\]]\s*$";

/// The terminal size used: wide, so lines are not cut.
pub const SIZE: Size = Size {
    cols: 200,
    rows: 50,
};

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

/// Nobody to ask (`przm mcp`: stdin carries the protocol).
pub struct NoOne;

impl Asker for NoOne {
    fn ask(&mut self, _: &str, _: bool) -> Option<String> {
        None
    }
}

/// Wakes the loop when the transport has something.
#[derive(Default)]
struct Bell {
    rung: Mutex<bool>,
    cond: Condvar,
}

/// A waker for a transport, and a function that sleeps until it rings (or
/// at most the time given).
pub fn new_waker() -> (Waker, impl Fn(Duration)) {
    let bell = Arc::new(Bell::default());
    let ring = bell.clone();
    let waker: Waker = Arc::new(move || {
        if let Ok(mut rung) = ring.rung.lock() {
            *rung = true;
            ring.cond.notify_one();
        }
    });
    let wait = move |max: Duration| {
        let Ok(rung) = bell.rung.lock() else { return };
        if let Ok((mut rung, _)) = bell.cond.wait_timeout_while(rung, max, |r| !*r) {
            *rung = false;
        }
    };
    (waker, wait)
}

/// How the login goes.
pub struct Login {
    pub prompt: Regex,
    pub timeout: Duration,
    pub settle: Duration,
    /// Answer "yes" to an unknown host key.
    pub accept_host_key: bool,
    /// For password prompts.
    pub password: Option<String>,
}

/// How waiting for a command ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Wait {
    Prompt,
    /// The connection ended (`exit`).
    Closed,
    TimedOut,
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

fn last_words(line: &str) -> String {
    let line = line.trim();
    if line.is_empty() {
        String::new()
    } else {
        format!(" (last output: `{line}`)")
    }
}

/// A connected session.
pub struct Conn {
    pub transport: Box<dyn Transport>,
    plain: PlainText,
    /// Plain text not taken yet.
    pending: String,
    /// The current (last, unfinished) line.
    line: String,
    last_change: Instant,
    engine: TriggerEngine,
    term: Terminal,
    buf: Vec<u8>,
}

impl Conn {
    pub fn new(transport: Box<dyn Transport>, triggers: &[przmterm_session::Trigger]) -> Self {
        let (engine, _) = TriggerEngine::new(triggers);
        Self {
            transport,
            plain: PlainText::new(),
            pending: String::new(),
            line: String::new(),
            last_change: Instant::now(),
            engine,
            term: Terminal::new(usize::from(SIZE.cols), usize::from(SIZE.rows)),
            buf: vec![0u8; 16 * 1024],
        }
    }

    /// Reads what has arrived; `raw` also gets the bytes. True if anything
    /// came.
    pub fn pump(&mut self, mut raw: Option<&mut Vec<u8>>) -> Result<bool> {
        let mut got = false;
        loop {
            let n = match self.transport.read(&mut self.buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            got = true;
            let bytes = &self.buf[..n];
            self.term.feed(bytes);
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
            if let Some(raw) = raw.as_deref_mut() {
                raw.extend_from_slice(bytes);
            }
            for send in self.engine.feed(bytes) {
                for chunk in przmterm_session::buttons::parse_send(&send) {
                    if let przmterm_session::buttons::Chunk::Bytes(b) = chunk {
                        let _ = self.transport.write(&b);
                    }
                }
            }
        }
        // Replies the terminal owes the far end (cursor position…).
        let replies = self.term.take_output();
        if !replies.is_empty() {
            let _ = self.transport.write(&replies);
        }
        Ok(got)
    }

    /// The unfinished last line: where the prompt shows.
    pub fn line(&self) -> &str {
        &self.line
    }

    /// Takes the plain text not taken yet, with line ends made `\n`.
    pub fn take_text(&mut self) -> String {
        let text = std::mem::take(&mut self.pending);
        text.replace("\r\n", "\n").replace('\r', "")
    }

    /// What the screen shows now.
    pub fn screen(&self) -> String {
        self.term.screen().text()
    }

    pub fn quiet_for(&self) -> Duration {
        self.last_change.elapsed()
    }

    pub fn is_closed(&self) -> bool {
        self.transport.is_closed()
    }

    pub fn send(&mut self, text: &str) -> Result<()> {
        self.transport
            .write(text.as_bytes())
            .map_err(|e| anyhow::anyhow!("sending: {e}"))
    }

    /// Answers what can be answered until a prompt shows; the login's text
    /// goes to `log`.
    pub fn login(
        &mut self,
        opts: &Login,
        asker: &mut dyn Asker,
        wait: &dyn Fn(Duration),
        mut log: Option<&mut dyn Write>,
    ) -> Result<()> {
        let start = Instant::now();
        let mut answered: Option<String> = None;
        loop {
            self.pump(None)?;
            let text = self.take_text();
            if let Some(log) = log.as_deref_mut() {
                log.write_all(text.as_bytes())?;
            }
            let line = self.line.clone();
            let quiet = self.quiet_for();
            if !line.is_empty()
                && quiet >= opts.settle
                && opts.prompt.is_match(&line).unwrap_or(false)
            {
                return Ok(());
            }
            if self.is_closed() {
                bail!(
                    "the connection ended before a prompt showed{}",
                    last_words(&line)
                );
            }
            if quiet >= Duration::from_millis(300)
                && answered.as_deref() != Some(line.as_str())
                && let Some(q) = question(&line)
            {
                let answer = match q {
                    Question::HostKey if opts.accept_host_key => "yes".to_string(),
                    Question::HostKey => bail!(
                        "the server's host key is not known yet; check it and accept it \
                         (--accept-host-key, or connect once in the app)"
                    ),
                    Question::SavePassword => "no".to_string(),
                    Question::Secret => match &opts.password {
                        Some(p) => p.clone(),
                        None => asker
                            .ask(&format!("{} ", line.trim_end()), true)
                            .with_context(|| {
                                format!(
                                    "the login asks `{}`: save the password in the keychain \
                                     (log in once in the app), use a vault entry, or give it \
                                     with --password-env",
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
                self.send(&format!("{answer}\r"))?;
                answered = Some(line);
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
    }

    /// Waits until the prompt is back after something was sent: output came,
    /// then was quiet for `settle` with the last line matching `prompt`.
    /// `each` gets the plain text as it arrives (and the bytes, with `raw`).
    pub fn wait_prompt(
        &mut self,
        prompt: &Regex,
        settle: Duration,
        timeout: Duration,
        wait: &dyn Fn(Duration),
        mut raw: Option<&mut Vec<u8>>,
        each: &mut dyn FnMut(&str) -> Result<()>,
    ) -> Result<Wait> {
        let sent = Instant::now();
        let mut changed = false;
        loop {
            if self.pump(raw.as_deref_mut())? {
                changed = true;
            }
            let text = self.take_text();
            if !text.is_empty() {
                each(&text)?;
            }
            if changed && self.quiet_for() >= settle && prompt.is_match(&self.line).unwrap_or(false)
            {
                return Ok(Wait::Prompt);
            }
            if self.is_closed() {
                return Ok(Wait::Closed);
            }
            if sent.elapsed() > timeout {
                return Ok(Wait::TimedOut);
            }
            wait(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
pub mod testing {
    //! A scripted far end for tests.
    use przmterm_transport::{Protocol, Size, Transport, TransportError};
    use std::collections::VecDeque;

    /// Answers each text written with a scripted reply.
    pub struct Script {
        /// (text written that triggers it, reply; `<close>` ends)
        replies: VecDeque<(String, String)>,
        out: VecDeque<u8>,
        typed: String,
        closed: bool,
        /// Everything written, for checks.
        pub written: std::sync::Arc<std::sync::Mutex<String>>,
    }

    impl Script {
        pub fn new(greeting: &str, replies: &[(&str, &str)]) -> Self {
            Self {
                replies: replies
                    .iter()
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .collect(),
                out: greeting.bytes().collect(),
                typed: String::new(),
                closed: false,
                written: Default::default(),
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
            let text = String::from_utf8_lossy(data);
            self.written.lock().unwrap().push_str(&text);
            self.typed.push_str(&text);
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

    /// Sleeps a little: the scripted far end answers at once.
    pub fn wait(d: std::time::Duration) {
        std::thread::sleep(d.min(std::time::Duration::from_millis(2)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
