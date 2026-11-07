//! `przm run`: connects a session without a window, logs in, sends commands
//! and prints what comes back.

use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use fancy_regex::Regex;

use crate::headless::{Asker, Conn, Login, Wait};

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

/// How the run ended, as the process exit code.
#[derive(Debug, PartialEq, Eq)]
pub enum Ending {
    Done,
    /// A prompt did not come back in time.
    TimedOut(String),
}

/// Waits for the login, sends each command and waits for the prompt after
/// it, writing the output to `out` (and, with `verbose`, the login to
/// `log`).
pub fn drive(
    conn: &mut Conn,
    opts: &Options,
    asker: &mut dyn Asker,
    wait: &dyn Fn(Duration),
    out: &mut dyn Write,
    log: &mut dyn Write,
) -> Result<Ending> {
    let login = Login {
        prompt: opts.prompt.clone(),
        timeout: opts.timeout,
        settle: opts.settle,
        accept_host_key: opts.accept_host_key,
        password: opts.password.clone(),
    };
    conn.login(&login, asker, wait, opts.verbose.then_some(log))?;
    // The prompt line the first command is typed on.
    if !opts.raw {
        out.write_all(conn.line().as_bytes())?;
    }
    for cmd in &opts.commands {
        conn.send(&format!("{cmd}\r"))?;
        let ended = if opts.raw {
            let mut bytes = Vec::new();
            let r = conn.wait_prompt(
                &opts.prompt,
                opts.settle,
                opts.timeout,
                wait,
                Some(&mut bytes),
                &mut |_| Ok(()),
            );
            out.write_all(&bytes)?;
            out.flush()?;
            r?
        } else {
            conn.wait_prompt(
                &opts.prompt,
                opts.settle,
                opts.timeout,
                wait,
                None,
                &mut |text| {
                    out.write_all(text.as_bytes())?;
                    out.flush()?;
                    Ok(())
                },
            )?
        };
        match ended {
            Wait::Prompt => {}
            // `exit` and the like: the end, not an error.
            Wait::Closed => {
                writeln!(out)?;
                return Ok(Ending::Done);
            }
            Wait::TimedOut => {
                writeln!(out)?;
                return Ok(Ending::TimedOut(cmd.clone()));
            }
        }
    }
    if !opts.raw {
        writeln!(out)?;
    }
    Ok(Ending::Done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::testing::{Script, wait};
    use crate::headless::{DEFAULT_PROMPT, NoOne};

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

    fn run(t: Script, o: &Options) -> (Result<Ending>, String) {
        let mut out = Vec::new();
        let mut log = Vec::new();
        let mut conn = Conn::new(Box::new(t), &[]);
        let r = drive(&mut conn, o, &mut NoOne, &wait, &mut out, &mut log);
        (r, String::from_utf8(out).unwrap())
    }

    #[test]
    fn sends_commands_and_prints_the_answers() {
        let t = Script::new(
            "Welcome\r\n\x1b[1mrouter#\x1b[0m ",
            &[
                ("show clock\r", "show clock\r\n12:00:00\r\nrouter# "),
                ("show ver\r", "show ver\r\nVersion 1\r\nrouter# "),
            ],
        );
        let (r, out) = run(t, &opts(&["show clock", "show ver"]));
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
        let (r, _) = run(script(), &o);
        assert!(r.unwrap_err().to_string().contains("--accept-host-key"));

        o.accept_host_key = true;
        let (r, _) = run(script(), &o);
        assert!(r.unwrap_err().to_string().contains("password"));

        o.password = Some("secret".into());
        let (r, out) = run(script(), &o);
        assert_eq!(r.unwrap(), Ending::Done);
        assert!(out.contains("up 3 days"), "{out}");
    }

    #[test]
    fn a_missing_prompt_times_out_and_exit_ends() {
        let t = Script::new("r1$ ", &[("sleep\r", "sleep\r\n")]);
        let mut o = opts(&["sleep"]);
        o.timeout = Duration::from_millis(100);
        let (r, _) = run(t, &o);
        assert_eq!(r.unwrap(), Ending::TimedOut("sleep".into()));

        let t = Script::new("r1$ ", &[("exit\r", "<close>")]);
        let (r, _) = run(t, &opts(&["exit", "never"]));
        assert_eq!(r.unwrap(), Ending::Done);
    }
}
