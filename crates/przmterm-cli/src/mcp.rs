//! `przm mcp`: a Model Context Protocol server on stdin and stdout, so an AI
//! assistant can open saved sessions, send commands and read the screen.
//!
//! Each session says what the assistant may do with it (`[mcp] access`):
//! nothing (the default: it does not even see the session), read (open it
//! and read the screen) or full (also send text and run scripts). Lines
//! matching the session's block list (`[mcp] block`, regular expressions)
//! are never sent, whoever asks.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use fancy_regex::Regex;
use przmterm_session::{McpAccess, Session, SessionTree};
use serde_json::{Value, json};

use crate::headless::{self, Conn, DEFAULT_PROMPT, Login, NoOne, Wait};

/// Output returned at most by one call; the start is cut.
const MAX_OUTPUT: usize = 16_000;

/// A session opened by the assistant.
struct Open {
    session: Session,
    conn: Conn,
    wait: Box<dyn Fn(Duration)>,
    prompt: Regex,
}

pub struct Server {
    config: Option<PathBuf>,
    open: BTreeMap<String, Open>,
    next: u32,
    /// The vault's entries, once opened.
    credentials: Option<std::sync::Arc<Vec<przmterm_session::vault::VaultEntry>>>,
}

/// Serves requests from stdin until it closes.
pub fn serve(config: Option<PathBuf>) -> Result<()> {
    let mut server = Server::new(config);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => server.handle(&message),
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    server.close_all();
    Ok(())
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

/// The tools, as `tools/list` describes them.
fn tools() -> Value {
    let id = json!({"type": "string", "description": "What open_session returned, e.g. \"s1\""});
    json!([
        {
            "name": "list_sessions",
            "description": "Lists the saved PrzmTerm sessions the user allows access to, \
                with what each allows: read (open, read the screen) or full (also send \
                commands and run scripts).",
            "inputSchema": {"type": "object", "properties": {
                "filter": {"type": "string", "description": "Only names, folders or hosts containing this"}
            }}
        },
        {
            "name": "open_session",
            "description": "Connects a saved session and logs in (with its key, vault entry \
                or saved password). Returns an id for the other tools and the screen.",
            "inputSchema": {"type": "object", "required": ["session"], "properties": {
                "session": {"type": "string", "description": "Name, or Folder/name, from list_sessions"},
                "prompt": {"type": "string", "description": "Regular expression the prompt line matches; default: ends in $ # > % or ]"}
            }}
        },
        {
            "name": "send",
            "description": "Types text into an open session (Enter after it unless enter is \
                false) and waits for the prompt to come back. Returns the output. Needs full \
                access; lines on the session's block list are refused.",
            "inputSchema": {"type": "object", "required": ["id", "text"], "properties": {
                "id": id,
                "text": {"type": "string", "description": "A command; several lines are sent one after another"},
                "enter": {"type": "boolean", "description": "Press Enter after the text (default true)"},
                "wait": {"type": "boolean", "description": "Wait for the prompt (default true)"},
                "timeout": {"type": "number", "description": "Seconds to wait for the prompt (default 30)"}
            }}
        },
        {
            "name": "read_screen",
            "description": "What the open session's screen shows now.",
            "inputSchema": {"type": "object", "required": ["id"], "properties": {"id": id}}
        },
        {
            "name": "run_script",
            "description": "Runs a PrzmTerm script (Rhai) on an open session: a file of the \
                user's scripts folder, or the source itself. Returns its log and the output. \
                Needs full access; what it sends is checked against the block list.",
            "inputSchema": {"type": "object", "required": ["id"], "properties": {
                "id": id,
                "script": {"type": "string", "description": "File name in the scripts folder, e.g. backup.rhai"},
                "source": {"type": "string", "description": "Rhai source, instead of a file"},
                "timeout": {"type": "number", "description": "Seconds before it is stopped (default 120)"}
            }}
        },
        {
            "name": "close_session",
            "description": "Disconnects an open session.",
            "inputSchema": {"type": "object", "required": ["id"], "properties": {"id": id}}
        }
    ])
}

/// The first line of `text` a pattern of `block` matches, with the pattern.
pub fn blocked(text: &str, block: &[String]) -> Result<Option<(String, String)>> {
    let patterns: Vec<(Regex, &String)> = block
        .iter()
        .map(|b| {
            Regex::new(&format!("(?i){b}"))
                .map(|r| (r, b))
                .map_err(|e| anyhow!("the block list has a broken pattern `{b}`: {e}"))
        })
        .collect::<Result<_>>()?;
    for line in text.split(['\r', '\n']) {
        for (re, pattern) in &patterns {
            if re.is_match(line).unwrap_or(true) {
                return Ok(Some((line.trim().to_string(), (*pattern).clone())));
            }
        }
    }
    Ok(None)
}

/// The end of `text` when it is too long.
fn cut(text: String) -> String {
    if text.len() <= MAX_OUTPUT {
        return text;
    }
    let mut start = text.len() - MAX_OUTPUT;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[… {} characters before]\n{}", start, &text[start..])
}

impl Server {
    pub fn new(config: Option<PathBuf>) -> Self {
        Self {
            config,
            open: BTreeMap::new(),
            next: 1,
            credentials: None,
        }
    }

    fn dir(&self) -> Option<PathBuf> {
        self.config
            .clone()
            .or_else(przmterm_session::default_config_dir)
    }

    /// The saved sessions, read again each time (the app may change them).
    fn sessions(&self) -> Vec<Session> {
        self.dir()
            .as_deref()
            .map(SessionTree::load)
            .unwrap_or_default()
            .root
            .all_sessions()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Answers one JSON-RPC message; notifications get no answer.
    pub fn handle(&mut self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let id = id?;
        let result = match method {
            "initialize" => {
                let version = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-06-18");
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "przmterm", "version": przmterm_core::VERSION},
                    "instructions": "Opens the user's saved PrzmTerm terminal sessions (SSH, \
                        Telnet, serial). Call list_sessions first; open_session returns an id. \
                        send types a command and returns its output once the prompt is back. \
                        Only sessions the user allowed are listed; some only allow reading."
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match self.call(name, &args) {
                    Ok(text) => text_result(text, false),
                    Err(e) => text_result(format!("{e:#}"), true),
                }
            }
            _ => return Some(error(id, -32601, &format!("unknown method {method}"))),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn call(&mut self, name: &str, args: &Value) -> Result<String> {
        let text = |key: &str| args.get(key).and_then(Value::as_str);
        let id = || -> Result<String> {
            text("id")
                .map(String::from)
                .ok_or_else(|| anyhow!("give the id open_session returned"))
        };
        match name {
            "list_sessions" => self.list(text("filter").unwrap_or("")),
            "open_session" => self.open_session(
                text("session").ok_or_else(|| anyhow!("give a session"))?,
                text("prompt"),
            ),
            "send" => self.send(
                &id()?,
                text("text").ok_or_else(|| anyhow!("give the text to send"))?,
                args.get("enter").and_then(Value::as_bool).unwrap_or(true),
                args.get("wait").and_then(Value::as_bool).unwrap_or(true),
                secs(args, 30.0),
            ),
            "read_screen" => self.read_screen(&id()?),
            "run_script" => {
                self.run_script(&id()?, text("script"), text("source"), secs(args, 120.0))
            }
            "close_session" => {
                let id = id()?;
                let mut open = self
                    .open
                    .remove(&id)
                    .ok_or_else(|| anyhow!("no open session {id}"))?;
                let _ = open.conn.transport.close();
                Ok(format!("Closed {id}."))
            }
            other => bail!("unknown tool {other}"),
        }
    }

    fn list(&self, filter: &str) -> Result<String> {
        let filter = filter.to_lowercase();
        let lines: Vec<String> = self
            .sessions()
            .iter()
            .filter(|s| s.mcp_access != McpAccess::None)
            .filter(|s| {
                filter.is_empty()
                    || crate::label(s).to_lowercase().contains(&filter)
                    || s.display_target().to_lowercase().contains(&filter)
            })
            .map(|s| {
                format!(
                    "{}  [{}] {}  access: {}",
                    crate::label(s),
                    s.protocol,
                    s.display_target(),
                    s.mcp_access.name()
                )
            })
            .collect();
        Ok(if lines.is_empty() {
            "No sessions allow access. The user turns it on in the session's settings \
             (AI access page) in PrzmTerm."
                .into()
        } else {
            lines.join("\n")
        })
    }

    fn open_session(&mut self, target: &str, prompt: Option<&str>) -> Result<String> {
        let sessions = self.sessions();
        let session = match crate::saved_sessions(&sessions, target).as_slice() {
            [one] => (*one).clone(),
            [] => bail!("no saved session called {target} (see list_sessions)"),
            many => bail!(
                "several sessions are called {target}: {}",
                many.iter()
                    .map(|s| crate::label(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        if session.mcp_access == McpAccess::None {
            bail!("{target} does not allow access (see list_sessions)");
        }
        let prompt = Regex::new(prompt.unwrap_or(DEFAULT_PROMPT))
            .map_err(|e| anyhow!("the prompt is not a valid regular expression: {e}"))?;
        let mut env = crate::headless_env(self.dir().as_deref(), sessions);
        env.credentials = self.credentials.clone();
        crate::unlock_vault(&session, &mut env, &mut NoOne)?;
        self.credentials = env.credentials.clone();
        let (waker, wait) = headless::new_waker();
        let transport = przmterm_connect::connect(&session, &env, headless::SIZE, waker);
        let mut conn = Conn::new(transport, &session.triggers);
        let login = Login {
            prompt: prompt.clone(),
            timeout: Duration::from_secs(30),
            settle: Duration::from_millis(400),
            // A new host key is for the user to check, in the app.
            accept_host_key: false,
            password: None,
        };
        if let Err(e) = conn.login(&login, &mut NoOne, &wait, None) {
            let _ = conn.transport.close();
            return Err(e);
        }
        let id = format!("s{}", self.next);
        self.next += 1;
        let screen = conn.screen();
        let access = session.mcp_access;
        let label = crate::label(&session);
        self.open.insert(
            id.clone(),
            Open {
                session,
                conn,
                wait: Box::new(wait),
                prompt,
            },
        );
        Ok(format!(
            "Opened {label} as {id} (access: {}).\n\n{}",
            access.name(),
            screen.trim_end()
        ))
    }

    fn get(&mut self, id: &str, full: bool) -> Result<&mut Open> {
        let open = self
            .open
            .get_mut(id)
            .ok_or_else(|| anyhow!("no open session {id} (open_session first)"))?;
        if full && open.session.mcp_access != McpAccess::Full {
            bail!(
                "{} only allows reading; the user can allow more in its settings",
                crate::label(&open.session)
            );
        }
        if open.conn.is_closed() {
            bail!("the connection of {id} has ended; open the session again");
        }
        Ok(open)
    }

    fn send(
        &mut self,
        id: &str,
        text: &str,
        enter: bool,
        wait: bool,
        timeout: Duration,
    ) -> Result<String> {
        let open = self.get(id, true)?;
        if let Some((line, pattern)) = blocked(text, &open.session.mcp_block)? {
            bail!("refused: `{line}` matches the block list (`{pattern}`)");
        }
        let mut output = String::new();
        let lines: Vec<&str> = if enter {
            text.lines().collect()
        } else {
            vec![text]
        };
        for line in lines {
            open.conn.pump(None)?;
            open.conn.take_text();
            open.conn.send(&if enter {
                format!("{line}\r")
            } else {
                line.into()
            })?;
            if !wait {
                (open.wait)(Duration::from_millis(300));
                open.conn.pump(None)?;
                output.push_str(&open.conn.take_text());
                continue;
            }
            let ended = open.conn.wait_prompt(
                &open.prompt,
                Duration::from_millis(400),
                timeout,
                &*open.wait,
                None,
                &mut |t| {
                    output.push_str(t);
                    Ok(())
                },
            )?;
            match ended {
                Wait::Prompt => {}
                Wait::Closed => {
                    output.push_str("\n[the connection ended]");
                    break;
                }
                Wait::TimedOut => {
                    output.push_str(&format!(
                        "\n[no prompt after {} s: it may still be running; read_screen shows more]",
                        timeout.as_secs()
                    ));
                    break;
                }
            }
        }
        Ok(cut(output))
    }

    fn read_screen(&mut self, id: &str) -> Result<String> {
        let open = self
            .open
            .get_mut(id)
            .ok_or_else(|| anyhow!("no open session {id}"))?;
        open.conn.pump(None)?;
        open.conn.take_text();
        let mut screen = open.conn.screen().trim_end().to_string();
        if open.conn.is_closed() {
            screen.push_str("\n[the connection has ended]");
        }
        Ok(screen)
    }

    fn run_script(
        &mut self,
        id: &str,
        file: Option<&str>,
        source: Option<&str>,
        timeout: Duration,
    ) -> Result<String> {
        use przmterm_script::{Outcome, Request, Script, SessionInfo};
        let dir = self.dir();
        let open = self.get(id, true)?;
        let info = SessionInfo {
            name: open.session.name.clone(),
            host: match &open.session.target {
                przmterm_session::Target::Network { host, .. } => host.clone(),
                przmterm_session::Target::Serial(line) => line.port.clone(),
                przmterm_session::Target::Local => String::new(),
            },
            username: match &open.session.target {
                przmterm_session::Target::Network { username, .. } => {
                    username.clone().unwrap_or_default()
                }
                _ => String::new(),
            },
        };
        let waker: przmterm_transport::Waker = std::sync::Arc::new(|| {});
        let mut script = match (file, source) {
            (_, Some(source)) => Script::run("mcp".into(), source.into(), info, waker),
            (Some(file), None) => Script::run_file(
                &przmterm_session::script_path(dir.as_deref(), file),
                info,
                waker,
            ),
            (None, None) => bail!("give a script file name or its source"),
        };
        let start = Instant::now();
        let mut log = Vec::new();
        let mut output = String::new();
        let outcome = loop {
            open.conn.pump(None)?;
            let text = open.conn.take_text();
            if !text.is_empty() {
                script.feed(&text);
                output.push_str(&text);
            }
            if open.conn.is_closed() {
                script.session_closed();
            }
            let mut done = None;
            for request in script.poll() {
                match request {
                    Request::Send(text) => {
                        if let Some((line, pattern)) = blocked(&text, &open.session.mcp_block)? {
                            script.cancel();
                            done = Some(Outcome::Failed(format!(
                                "refused: `{line}` matches the block list (`{pattern}`)"
                            )));
                            break;
                        }
                        open.conn.send(&text)?;
                    }
                    Request::Log(line) => log.push(line),
                    // Nobody to ask.
                    Request::Prompt { reply, .. } => {
                        let _ = reply.send(None);
                    }
                    Request::Done(outcome) => done = Some(outcome),
                }
            }
            if let Some(outcome) = done {
                break outcome;
            }
            if start.elapsed() > timeout {
                script.cancel();
                break Outcome::Failed(format!("stopped after {} s", timeout.as_secs()));
            }
            (open.wait)(Duration::from_millis(20));
        };
        let mut report = match &outcome {
            Outcome::Finished => "The script finished.".to_string(),
            Outcome::Cancelled => "The script was stopped.".to_string(),
            Outcome::Failed(e) => format!("The script failed: {e}"),
        };
        if !log.is_empty() {
            report.push_str("\n\nLog:\n");
            report.push_str(&log.join("\n"));
        }
        report.push_str("\n\nOutput:\n");
        report.push_str(&cut(output));
        if matches!(outcome, Outcome::Failed(_)) {
            bail!(report);
        }
        Ok(report)
    }

    fn close_all(&mut self) {
        for open in self.open.values_mut() {
            let _ = open.conn.transport.close();
        }
        self.open.clear();
    }
}

fn secs(args: &Value, default: f64) -> Duration {
    let s = args
        .get("timeout")
        .and_then(Value::as_f64)
        .unwrap_or(default);
    Duration::from_secs_f64(s.clamp(1.0, 3600.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::testing::{Script, wait};

    fn call(server: &mut Server, id: i64, method: &str, params: Value) -> Value {
        server
            .handle(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .unwrap()
    }

    fn opened(server: &mut Server, access: McpAccess, script: Script) -> String {
        let settings = przmterm_session::SessionSettings::from_toml("host = 'r1'").unwrap();
        let mut session = Session::resolve("r1", vec!["Lab".into()], "r1.toml", &settings).unwrap();
        session.mcp_access = access;
        let mut conn = Conn::new(Box::new(script), &[]);
        let login = Login {
            prompt: Regex::new(DEFAULT_PROMPT).unwrap(),
            timeout: Duration::from_secs(2),
            settle: Duration::from_millis(1),
            accept_host_key: false,
            password: None,
        };
        conn.login(&login, &mut NoOne, &wait, None).unwrap();
        let id = format!("s{}", server.next);
        server.next += 1;
        server.open.insert(
            id.clone(),
            Open {
                session,
                conn,
                wait: Box::new(wait),
                prompt: Regex::new(DEFAULT_PROMPT).unwrap(),
            },
        );
        id
    }

    fn tool(server: &mut Server, name: &str, args: Value) -> (bool, String) {
        let reply = call(
            server,
            9,
            "tools/call",
            json!({"name": name, "arguments": args}),
        );
        let result = &reply["result"];
        (
            result["isError"].as_bool().unwrap(),
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    #[test]
    fn speaks_the_protocol() {
        let mut server = Server::new(Some(PathBuf::from("/nonexistent")));
        let reply = call(
            &mut server,
            1,
            "initialize",
            json!({"protocolVersion": "2025-03-26"}),
        );
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], "przmterm");
        assert!(
            server
                .handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .is_none()
        );
        let tools = call(&mut server, 2, "tools/list", json!({}));
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "list_sessions",
                "open_session",
                "send",
                "read_screen",
                "run_script",
                "close_session"
            ]
        );
        assert_eq!(
            call(&mut server, 3, "nope", json!({}))["error"]["code"],
            -32601
        );
        let (err, text) = tool(&mut server, "list_sessions", json!({}));
        assert!(!err && text.contains("No sessions allow access"), "{text}");
    }

    #[test]
    fn sends_reads_and_respects_access_and_the_block_list() {
        let mut server = Server::new(None);
        let full = opened(
            &mut server,
            McpAccess::Full,
            Script::new("r1# ", &[("show clock\r", "show clock\r\n12:00\r\nr1# ")]),
        );
        let (err, out) = tool(
            &mut server,
            "send",
            json!({"id": full, "text": "show clock"}),
        );
        assert!(!err, "{out}");
        assert!(
            out.contains("12:00") && out.trim_end().ends_with("r1#"),
            "{out}"
        );
        let (err, screen) = tool(&mut server, "read_screen", json!({"id": full}));
        assert!(!err && screen.contains("12:00"), "{screen}");
        let (err, out) = tool(&mut server, "send", json!({"id": full, "text": "RELOAD"}));
        assert!(err && out.contains("block list"), "{out}");

        let read = opened(&mut server, McpAccess::Read, Script::new("r1# ", &[]));
        let (err, out) = tool(
            &mut server,
            "send",
            json!({"id": read, "text": "show clock"}),
        );
        assert!(err && out.contains("only allows reading"), "{out}");
        let (err, _) = tool(&mut server, "read_screen", json!({"id": read}));
        assert!(!err);
        let (err, out) = tool(&mut server, "close_session", json!({"id": read}));
        assert!(!err, "{out}");
        let (err, _) = tool(&mut server, "read_screen", json!({"id": read}));
        assert!(err);
    }

    #[test]
    fn scripts_run_and_their_sends_are_checked() {
        let mut server = Server::new(None);
        let id = opened(
            &mut server,
            McpAccess::Full,
            Script::new("r1# ", &[("show ver\r", "show ver\r\nVersion 9\r\nr1# ")]),
        );
        let (err, out) = tool(
            &mut server,
            "run_script",
            json!({"id": id, "source": "session.send(\"show ver\\r\"); \
                   if session.wait_for(\"Version\", 5) { log(\"found\"); }"}),
        );
        assert!(!err, "{out}");
        assert!(out.contains("found") && out.contains("Version 9"), "{out}");
        let (err, out) = tool(
            &mut server,
            "run_script",
            json!({"id": id, "source": "session.send(\"reboot\\r\");"}),
        );
        assert!(err && out.contains("block list"), "{out}");
    }

    #[test]
    fn the_default_block_list_stops_the_dangerous_ones() {
        let block: Vec<String> = przmterm_session::DEFAULT_MCP_BLOCK
            .iter()
            .map(|b| b.to_string())
            .collect();
        for bad in [
            "reload",
            "  Reload in 5",
            "reboot",
            "write erase",
            "erase startup-config",
            "format flash:",
            "delete flash:/x.bin",
            "rm -rf /tmp/x",
            "rm -fr x",
            "sudo rm -r -f dir",
            "mkfs.ext4 /dev/sdb1",
            "dd if=/dev/zero of=/dev/sda",
            "request system reboot",
            "show ver\nreload",
            "sudo reboot",
            "sudo -n shutdown -h now",
            "ls; reboot",
            "true && poweroff",
            "systemctl reboot",
            "sudo erase nvram:",
        ] {
            assert!(blocked(bad, &block).unwrap().is_some(), "{bad}");
        }
        for fine in [
            "show reload",
            "show running-config",
            "rm notes.txt",
            "ls -la",
            "ping 10.0.0.1",
            "show version",
            "configure terminal",
            "sudo systemctl status sshd",
            "show interfaces | include shutdown",
            "echo rebooted",
        ] {
            assert!(blocked(fine, &block).unwrap().is_none(), "{fine}");
        }
        assert!(blocked("x", &["(".to_string()]).is_err());
    }
}
