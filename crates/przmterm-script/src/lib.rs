//! Rhai scripts that drive a session: send text, wait for prompts, read
//! output, ask the user. A script runs on a thread of its own; the
//! application feeds it the session's output as plain text and carries out
//! what it asks ([`Request`]).
//!
//! ```rhai
//! session.send("terminal length 0\r");
//! if session.wait_for("#", 10) {
//!     let version = session.read_until("#");
//!     log("got " + version.len() + " characters");
//! }
//! ```

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rhai::{Array, Dynamic, Engine, EvalAltResult, Position, Scope};

/// Output kept while nothing waits for it.
const KEEP: usize = 64 * 1024;
/// File extension of scripts.
pub const EXTENSION: &str = "rhai";

/// What a script asks the application to do.
#[derive(Debug)]
pub enum Request {
    /// Type this into the session.
    Send(String),
    /// Show this line in the terminal (not sent).
    Log(String),
    /// Ask the user; the answer goes back on `reply` (`None` = cancelled).
    Prompt {
        question: String,
        secret: bool,
        reply: Sender<Option<String>>,
    },
    /// The script ended.
    Done(Outcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Finished,
    Cancelled,
    Failed(String),
}

/// Facts about the session, readable from the script.
#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    pub name: String,
    pub host: String,
    pub username: String,
}

#[derive(Default)]
struct Inbox {
    /// Output not consumed by a wait yet.
    text: String,
    /// The session ended: waits give up.
    closed: bool,
    cancelled: bool,
}

struct Shared {
    inbox: Mutex<Inbox>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inbox> {
        self.inbox.lock().unwrap_or_else(|e| e.into_inner())
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;
type R<T> = Result<T, Box<EvalAltResult>>;

/// The `session` object scripts see.
#[derive(Clone)]
struct Api {
    shared: Arc<Shared>,
    requests: Sender<Request>,
    waker: Waker,
    info: SessionInfo,
}

const CANCELLED: &str = "the script was stopped";

fn stop() -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorTerminated(
        Dynamic::from(CANCELLED),
        Position::NONE,
    ))
}

fn error(text: impl Into<String>) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(text.into()),
        Position::NONE,
    ))
}

enum Found {
    /// Which text, and the output before it.
    Match(usize, String),
    TimedOut,
    Closed,
}

impl Api {
    fn ask(&self, request: Request) -> Result<(), Box<EvalAltResult>> {
        if self.shared.lock().cancelled {
            return Err(stop());
        }
        self.requests.send(request).map_err(|_| stop())?;
        (self.waker)();
        Ok(())
    }

    /// Waits until one of `needles` shows up in the output, consuming the
    /// output up to the end of the earliest match.
    fn wait(
        &self,
        needles: &[String],
        timeout: Option<Duration>,
    ) -> Result<Found, Box<EvalAltResult>> {
        if needles.iter().all(|n| n.is_empty()) {
            return Err(error("wait_for needs some text to wait for"));
        }
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut inbox = self.shared.lock();
        loop {
            if inbox.cancelled {
                return Err(stop());
            }
            let best = needles
                .iter()
                .enumerate()
                .filter(|(_, n)| !n.is_empty())
                .filter_map(|(i, n)| inbox.text.find(n.as_str()).map(|at| (at, i, n.len())))
                .min_by_key(|&(at, i, _)| (at, i));
            if let Some((at, i, len)) = best {
                let before = inbox.text[..at].to_string();
                inbox.text.drain(..at + len);
                return Ok(Found::Match(i, before));
            }
            if inbox.closed {
                return Ok(Found::Closed);
            }
            let wait = match deadline {
                Some(d) => match d.checked_duration_since(Instant::now()) {
                    Some(left) if !left.is_zero() => left,
                    _ => return Ok(Found::TimedOut),
                },
                None => Duration::from_secs(3600),
            };
            inbox = self
                .shared
                .changed
                .wait_timeout(inbox, wait)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn prompt(&self, question: &str, secret: bool) -> Result<String, Box<EvalAltResult>> {
        let (reply, answer) = mpsc::channel();
        self.ask(Request::Prompt {
            question: question.to_string(),
            secret,
            reply,
        })?;
        loop {
            match answer.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(text)) => return Ok(text),
                Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => return Err(stop()),
                Err(mpsc::RecvTimeoutError::Timeout) if self.shared.lock().cancelled => {
                    return Err(stop());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn sleep(&self, ms: i64) -> Result<(), Box<EvalAltResult>> {
        let deadline = Instant::now() + Duration::from_millis(ms.max(0) as u64);
        let mut inbox = self.shared.lock();
        loop {
            if inbox.cancelled {
                return Err(stop());
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(());
            };
            if left.is_zero() {
                return Ok(());
            }
            inbox = self
                .shared
                .changed
                .wait_timeout(inbox, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

fn seconds(timeout: f64) -> Option<Duration> {
    (timeout > 0.0).then(|| Duration::from_secs_f64(timeout))
}

fn strings(list: &Array) -> Result<Vec<String>, Box<EvalAltResult>> {
    list.iter()
        .map(|v| {
            v.clone()
                .into_string()
                .map_err(|t| error(format!("wait_for: expected text, got {t}")))
        })
        .collect()
}

fn engine(api: &Api) -> Engine {
    let mut engine = Engine::new();
    engine
        .register_type_with_name::<Api>("Session")
        .register_get("name", |s: &mut Api| s.info.name.clone())
        .register_get("host", |s: &mut Api| s.info.host.clone())
        .register_get("username", |s: &mut Api| s.info.username.clone())
        .register_fn("send", |s: &mut Api, text: &str| {
            s.ask(Request::Send(text.to_string()))
        })
        // wait_for(text) → true when seen, false when the session closed;
        // with a timeout in seconds also false when it ran out.
        .register_fn("wait_for", |s: &mut Api, text: &str| -> R<bool> {
            Ok(matches!(
                s.wait(&[text.to_string()], None)?,
                Found::Match(..)
            ))
        })
        .register_fn(
            "wait_for",
            |s: &mut Api, text: &str, timeout: i64| -> R<bool> {
                Ok(matches!(
                    s.wait(&[text.to_string()], seconds(timeout as f64))?,
                    Found::Match(..)
                ))
            },
        )
        .register_fn(
            "wait_for",
            |s: &mut Api, text: &str, timeout: f64| -> R<bool> {
                Ok(matches!(
                    s.wait(&[text.to_string()], seconds(timeout))?,
                    Found::Match(..)
                ))
            },
        )
        // wait_for([a, b, …]) → the index of the text seen first, -1 if none.
        .register_fn("wait_for", |s: &mut Api, list: Array| -> R<i64> {
            Ok(match s.wait(&strings(&list)?, None)? {
                Found::Match(i, _) => i as i64,
                _ => -1,
            })
        })
        .register_fn(
            "wait_for",
            |s: &mut Api, list: Array, timeout: i64| -> R<i64> {
                Ok(match s.wait(&strings(&list)?, seconds(timeout as f64))? {
                    Found::Match(i, _) => i as i64,
                    _ => -1,
                })
            },
        )
        // read_until(text) → the output before `text`; an error if the
        // session closes or the timeout runs out first.
        .register_fn("read_until", |s: &mut Api, text: &str| {
            read_until(s, text, None)
        })
        .register_fn("read_until", |s: &mut Api, text: &str, timeout: i64| {
            read_until(s, text, seconds(timeout as f64))
        })
        .register_fn("read_until", |s: &mut Api, text: &str, timeout: f64| {
            read_until(s, text, seconds(timeout))
        });

    let a = api.clone();
    engine.register_fn("log", move |text: &str| {
        a.ask(Request::Log(text.to_string()))
    });
    let a = api.clone();
    engine.register_fn("sleep", move |ms: i64| a.sleep(ms));
    let a = api.clone();
    engine.register_fn("prompt", move |question: &str| a.prompt(question, false));
    let a = api.clone();
    engine.register_fn("prompt_secret", move |question: &str| {
        a.prompt(question, true)
    });
    let a = api.clone();
    engine.on_print(move |text| {
        let _ = a.ask(Request::Log(text.to_string()));
    });
    let a = api.clone();
    engine.on_debug(move |text, _, pos| {
        let _ = a.ask(Request::Log(format!("debug {pos}: {text}")));
    });
    let shared = api.shared.clone();
    engine.on_progress(move |_| shared.lock().cancelled.then(|| Dynamic::from(CANCELLED)));
    engine
}

fn read_until(
    s: &Api,
    text: &str,
    timeout: Option<Duration>,
) -> Result<String, Box<EvalAltResult>> {
    match s.wait(&[text.to_string()], timeout)? {
        Found::Match(_, before) => Ok(before),
        Found::TimedOut => Err(error(format!("timed out waiting for {text:?}"))),
        Found::Closed => Err(error(format!("the session closed before {text:?}"))),
    }
}

/// A running script.
pub struct Script {
    pub name: String,
    shared: Arc<Shared>,
    requests: Receiver<Request>,
    thread: Option<JoinHandle<()>>,
    done: bool,
}

impl Script {
    /// Runs the script in the file `path`.
    pub fn run_file(path: &Path, info: SessionInfo, waker: Waker) -> Script {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "script".into());
        match std::fs::read_to_string(path) {
            Ok(source) => Script::run(name, source, info, waker),
            Err(e) => {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(Request::Done(Outcome::Failed(format!(
                    "cannot read the script: {e}"
                ))));
                Script {
                    name,
                    shared: Arc::new(Shared {
                        inbox: Mutex::new(Inbox::default()),
                        changed: Condvar::new(),
                    }),
                    requests: rx,
                    thread: None,
                    done: false,
                }
            }
        }
    }

    /// Runs `source`.
    pub fn run(name: String, source: String, info: SessionInfo, waker: Waker) -> Script {
        let shared = Arc::new(Shared {
            inbox: Mutex::new(Inbox::default()),
            changed: Condvar::new(),
        });
        let (tx, rx) = mpsc::channel();
        let api = Api {
            shared: shared.clone(),
            requests: tx.clone(),
            waker: waker.clone(),
            info,
        };
        let thread = std::thread::Builder::new()
            .name(format!("script {name}"))
            .spawn(move || {
                let engine = engine(&api);
                let mut scope = Scope::new();
                scope.push_constant("session", api.clone());
                let outcome = match engine.run_with_scope(&mut scope, &source) {
                    Ok(()) => Outcome::Finished,
                    Err(e) if matches!(*e, EvalAltResult::ErrorTerminated(..)) => {
                        Outcome::Cancelled
                    }
                    Err(e) => Outcome::Failed(e.to_string()),
                };
                let _ = tx.send(Request::Done(outcome));
                waker();
            })
            .ok();
        Script {
            name,
            shared,
            requests: rx,
            thread,
            done: false,
        }
    }

    /// Output of the session, as plain text.
    pub fn feed(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut inbox = self.shared.lock();
        inbox.text.push_str(text);
        if inbox.text.len() > KEEP {
            let mut cut = inbox.text.len() - KEEP / 2;
            while !inbox.text.is_char_boundary(cut) {
                cut += 1;
            }
            inbox.text.drain(..cut);
        }
        drop(inbox);
        self.shared.changed.notify_all();
    }

    /// The session ended: waiting stops.
    pub fn session_closed(&self) {
        self.shared.lock().closed = true;
        self.shared.changed.notify_all();
    }

    pub fn cancel(&self) {
        self.shared.lock().cancelled = true;
        self.shared.changed.notify_all();
    }

    /// What the script asked since the last call. After
    /// [`Request::Done`] the script has ended.
    pub fn poll(&mut self) -> Vec<Request> {
        let requests: Vec<Request> = self.requests.try_iter().collect();
        if requests.iter().any(|r| matches!(r, Request::Done(_))) {
            self.done = true;
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
        requests
    }

    pub fn is_done(&self) -> bool {
        self.done
    }
}

impl Drop for Script {
    fn drop(&mut self) {
        // The thread ends at its next step; it is not waited for.
        self.cancel();
    }
}

/// A sample script shipped with the app.
pub struct Example {
    /// File name without `.rhai`.
    pub name: &'static str,
    /// One line on what it does.
    pub about: &'static str,
    pub source: &'static str,
}

/// The samples in `examples/scripts`.
pub const EXAMPLES: [Example; 6] = [
    Example {
        name: "linux-health",
        about: "Linux: uptime, disk and memory, with a summary line",
        source: include_str!("../../../examples/scripts/linux-health.rhai"),
    },
    Example {
        name: "ask-and-run",
        about: "Any device: asks for commands separated by ; and runs them",
        source: include_str!("../../../examples/scripts/ask-and-run.rhai"),
    },
    Example {
        name: "ping-watch",
        about: "Linux/macOS: logs when a host goes up or down, until stopped",
        source: include_str!("../../../examples/scripts/ping-watch.rhai"),
    },
    Example {
        name: "cisco-show",
        about: "Cisco: enable if needed, paging off, show commands",
        source: include_str!("../../../examples/scripts/cisco-show.rhai"),
    },
    Example {
        name: "huawei-show",
        about: "Huawei VRP: paging off and display commands",
        source: include_str!("../../../examples/scripts/huawei-show.rhai"),
    },
    Example {
        name: "logon-generic",
        about: "Logon script: answers the user name and password prompts",
        source: include_str!("../../../examples/scripts/logon-generic.rhai"),
    },
];

/// Checks a script for syntax errors without running it.
pub fn check(source: &str) -> Result<(), String> {
    Engine::new()
        .compile(source)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(source: &str) -> Script {
        Script::run(
            "test".into(),
            source.into(),
            SessionInfo {
                name: "core-sw".into(),
                host: "10.0.0.1".into(),
                username: "ops".into(),
            },
            Arc::new(|| {}),
        )
    }

    /// Collects requests until the script ends, answering prompts and
    /// feeding `output` whenever it sends something.
    fn drive(
        script: &mut Script,
        mut on_send: impl FnMut(&str, &Script),
    ) -> (Vec<String>, Outcome) {
        let mut log = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            for r in script.poll() {
                match r {
                    Request::Send(t) => {
                        log.push(format!("send {t}"));
                        on_send(&t, script);
                    }
                    Request::Log(t) => log.push(format!("log {t}")),
                    Request::Prompt {
                        question,
                        secret,
                        reply,
                    } => {
                        log.push(format!("prompt {question} {secret}"));
                        let _ = reply.send(Some("answer".into()));
                    }
                    Request::Done(o) => return (log, o),
                }
            }
            assert!(Instant::now() < deadline, "script did not end: {log:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn sends_waits_and_reads_like_a_login() {
        let mut s = start(
            r##"
            if !session.wait_for("Username:", 5) { throw "no prompt"; }
            session.send(session.username + "\r");
            session.wait_for("Password:");
            let pw = prompt_secret("Password for " + session.name);
            session.send(pw + "\r");
            let which = session.wait_for(["denied", "#"], 5);
            if which == 1 {
                session.send("show version\r");
                let v = session.read_until("#", 5);
                v.trim();
                print("version: " + v);
            }
            "##,
        );
        s.feed("\x07Username: ");
        let (log, outcome) = drive(&mut s, |sent, s| match sent {
            "ops\r" => s.feed("Password: "),
            "answer\r" => s.feed("\r\nrouter#"),
            "show version\r" => {
                s.feed("show version\r\nIOS 15.2\r");
                s.feed("\nrouter#");
            }
            _ => {}
        });
        assert_eq!(outcome, Outcome::Finished, "{log:?}");
        assert_eq!(
            log,
            [
                "send ops\r",
                "prompt Password for core-sw true",
                "send answer\r",
                "send show version\r",
                "log version: show version\r\nIOS 15.2\r\nrouter",
            ]
        );
    }

    #[test]
    fn timeouts_closed_sessions_and_errors() {
        let mut s = start(
            r##"print(session.wait_for("never", 0.05)); print(session.wait_for(["a", "b"], 1));"##,
        );
        s.session_closed();
        let (log, outcome) = drive(&mut s, |_, _| {});
        assert_eq!(outcome, Outcome::Finished);
        assert_eq!(log, ["log false", "log -1"]);

        let mut s = start(r##"session.read_until("#", 0.05);"##);
        let (_, outcome) = drive(&mut s, |_, _| {});
        match outcome {
            Outcome::Failed(e) => assert!(e.contains("timed out"), "{e}"),
            o => panic!("{o:?}"),
        }

        let mut s = start("let x = ;");
        let (_, outcome) = drive(&mut s, |_, _| {});
        assert!(matches!(outcome, Outcome::Failed(_)));
        assert!(check("let x = ;").is_err());
        assert!(check("let x = 1;").is_ok());
    }

    #[test]
    fn stopping_ends_waits_sleeps_and_loops() {
        for source in [
            r##"session.wait_for("never");"##,
            "sleep(60000);",
            "loop { }",
        ] {
            let mut s = start(source);
            std::thread::sleep(Duration::from_millis(30));
            s.cancel();
            let (_, outcome) = drive(&mut s, |_, _| {});
            assert_eq!(outcome, Outcome::Cancelled, "{source}");
        }
    }

    #[test]
    fn output_before_the_wait_is_not_lost_and_is_consumed_once() {
        let mut s = start(
            r##"
            session.wait_for("$");
            session.wait_for("$");
            print("two prompts");
            "##,
        );
        s.feed("a$ b$");
        let (log, outcome) = drive(&mut s, |_, _| {});
        assert_eq!(outcome, Outcome::Finished);
        assert_eq!(log, ["log two prompts"]);
    }

    #[test]
    fn functions_get_the_session_as_a_parameter() {
        let mut s = start(
            r##"
            fn run(s, command) { s.send(command + "\r"); }
            run(session, "show clock");
            "##,
        );
        let (log, outcome) = drive(&mut s, |_, _| {});
        assert_eq!(outcome, Outcome::Finished, "{log:?}");
        assert_eq!(log, ["send show clock\r"]);
    }

    #[test]
    fn the_examples_compile() {
        for e in &EXAMPLES {
            assert_eq!(check(e.source), Ok(()), "{}", e.name);
        }
    }

    #[test]
    fn a_missing_file_is_reported() {
        let mut s = Script::run_file(
            Path::new("/nonexistent/x.rhai"),
            SessionInfo::default(),
            Arc::new(|| {}),
        );
        let (_, outcome) = drive(&mut s, |_, _| {});
        match outcome {
            Outcome::Failed(e) => assert!(e.contains("cannot read"), "{e}"),
            o => panic!("{o:?}"),
        }
    }
}
