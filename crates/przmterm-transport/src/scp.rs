//! SCP over an SSH connection that is already logged in: sends files and
//! folders to the other side or fetches them, on a channel of its own, so
//! the terminal stays as it is.
//!
//! It talks the classic protocol to the remote `scp -t` (receive) or
//! `scp -f` (send), which Unix hosts and much network equipment (Cisco,
//! Huawei, Juniper…) answer. The remote end can run as root through `sudo`
//! or `su -`: the command then gets a terminal of its own, PrzmTerm answers
//! the password prompt, and switches that terminal to raw mode before the
//! transfer begins.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use russh::ChannelMsg;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::Waker;
use crate::modem::Progress;
use crate::pipe::runtime;
use crate::ssh::Handle;

/// Who the remote `scp` runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunAs {
    /// The user that logged in.
    #[default]
    Me,
    /// Root, through `sudo` (the user's own password, if it asks).
    Sudo,
    /// Root, through `su -` (root's password).
    Su,
}

impl RunAs {
    pub const ALL: [RunAs; 3] = [RunAs::Me, RunAs::Sudo, RunAs::Su];

    pub fn label(self) -> &'static str {
        match self {
            RunAs::Me => "the login user",
            RunAs::Sudo => "root, with sudo",
            RunAs::Su => "root, with su -",
        }
    }
}

/// What to transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Direction {
    /// Local files or folders into the remote folder `remote_dir` (empty:
    /// the home folder).
    Upload {
        files: Vec<PathBuf>,
        remote_dir: String,
    },
    /// A remote file or folder into the local folder `local_dir`. Nothing
    /// there is overwritten: a clash gets a name like `name (2).ext`.
    Download { remote: String, local_dir: PathBuf },
}

pub struct Request {
    pub direction: Direction,
    pub run_as: RunAs,
    /// For the password prompt of `sudo` or `su`.
    pub password: Zeroizing<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScpEvent {
    Progress(Progress),
    /// The end: the files sent or the local paths written, or why not.
    Done(Result<Vec<PathBuf>, String>),
}

/// The GUI's handle on a running transfer.
pub struct ScpTransfer {
    events: std_mpsc::Receiver<ScpEvent>,
    cancel: Arc<AtomicBool>,
}

impl ScpTransfer {
    pub fn poll(&self) -> Vec<ScpEvent> {
        self.events.try_iter().collect()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Starts `request` on a new channel of `handle`'s connection.
pub(crate) fn start(handle: Arc<Handle>, request: Request, waker: Waker) -> ScpTransfer {
    let (tx, events) = std_mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let ev = Events { tx, waker };
    let stop = cancel.clone();
    runtime().spawn(async move {
        let work = run_over_ssh(&handle, &request, &ev, &stop);
        // The work sees a cancel between chunks; one stuck waiting for the
        // other side is dropped (closing its channel) a moment later.
        let stuck = async {
            while !stop.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        };
        let result = tokio::select! {
            result = work => result,
            () = stuck => Err("cancelled".to_string()),
        };
        ev.send(ScpEvent::Done(result));
        crate::ssh::release(handle).await;
    });
    ScpTransfer { events, cancel }
}

#[derive(Clone)]
struct Events {
    tx: std_mpsc::Sender<ScpEvent>,
    waker: Waker,
}

impl Events {
    fn send(&self, e: ScpEvent) {
        let _ = self.tx.send(e);
        (self.waker)();
    }
}

async fn run_over_ssh(
    handle: &Handle,
    request: &Request,
    ev: &Events,
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    match &request.direction {
        Direction::Upload { files, remote_dir } => {
            let recursive = files.iter().any(|f| f.is_dir());
            let command = command(request.run_as, &scp_args(true, recursive, remote_dir));
            let mut conn = Conn::new(open_channel(handle, request.run_as, &command).await?);
            conn.elevate(request.run_as, &request.password).await?;
            let sent = send(&mut conn, files, ev, cancel).await;
            conn.link.finish(request.run_as).await;
            sent
        }
        Direction::Download { remote, local_dir } => {
            // Without -r first: some equipment does not know it. A folder
            // then answers "not a regular file", and -r follows.
            for recursive in [false, true] {
                let command = command(request.run_as, &scp_args(false, recursive, remote));
                let mut conn = Conn::new(open_channel(handle, request.run_as, &command).await?);
                conn.elevate(request.run_as, &request.password).await?;
                match receive(&mut conn, local_dir, ev, cancel).await {
                    Err(e) if !recursive && e.contains("not a regular file") => continue,
                    other => return other,
                }
            }
            unreachable!("the loop returns")
        }
    }
}

/// Printed by the shell once `sudo`/`su` let it run, just before `scp`.
const READY: &str = "PRZMTERM-SCP-READY";
/// The prompt asked of `sudo`, so it is recognised whatever the language.
const SUDO_PROMPT: &str = "[przmterm] password:";

/// The `scp` command line for the remote side.
fn scp_args(upload: bool, recursive: bool, path: &str) -> String {
    let path = match path.trim() {
        "" => ".".to_string(),
        p if p.starts_with('-') => format!("./{p}"),
        p => p.to_string(),
    };
    format!(
        "scp {}{} {}",
        if recursive { "-r " } else { "" },
        if upload { "-t" } else { "-f" },
        quote(&path)
    )
}

/// The command to run: `scp …` as it is, or through `sudo`/`su` with the
/// terminal made raw first.
fn command(run_as: RunAs, scp: &str) -> String {
    // On a terminal scp would draw its progress meter into the transfer:
    // -q keeps it quiet.
    let quiet = scp.replacen("scp ", "scp -q ", 1);
    let inner = format!("stty raw -echo; printf '%s\\n' {READY}; exec {quiet}");
    match run_as {
        RunAs::Me => scp.to_string(),
        RunAs::Sudo => format!(
            "sudo -k -p {} sh -c {}",
            quote(&format!("{SUDO_PROMPT} ")),
            quote(&inner)
        ),
        RunAs::Su => format!("su - root -c {}", quote(&inner)),
    }
}

/// `s` as one shell word; left bare when that is safe, so equipment
/// without a Unix shell still understands plain paths (`flash:/x.bin`).
fn quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%+=,~".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

async fn open_channel(handle: &Handle, run_as: RunAs, command: &str) -> Result<ChanLink, String> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("cannot open a channel: {e}"))?;
    if run_as != RunAs::Me {
        // sudo and su read the password from a terminal.
        channel
            .request_pty(false, "dumb", 200, 24, 0, 0, &[])
            .await
            .map_err(|e| format!("the server refused a terminal: {e}"))?;
    }
    channel
        .exec(true, command)
        .await
        .map_err(|e| format!("the server refused to run scp: {e}"))?;
    Ok(ChanLink {
        channel,
        stderr: String::new(),
    })
}

/// A byte pipe to the remote `scp`.
trait Link {
    /// The next bytes, or `None` once the other side is done.
    async fn recv(&mut self) -> Option<Vec<u8>>;
    async fn send(&mut self, data: &[u8]) -> Result<(), String>;
    /// What the other side printed on its error output.
    fn stderr(&self) -> &str;
}

struct ChanLink {
    channel: russh::Channel<russh::client::Msg>,
    stderr: String,
}

impl ChanLink {
    /// Ends the channel. A plain `scp -t` stops at end of input; behind
    /// sudo/su its terminal does not pass that on, so the channel closes
    /// (every file is complete once acknowledged).
    async fn finish(&mut self, run_as: RunAs) {
        if run_as == RunAs::Me {
            let _ = self.channel.eof().await;
            let wait = async { while self.channel.wait().await.is_some() {} };
            let _ = tokio::time::timeout(Duration::from_secs(3), wait).await;
        }
        let _ = self.channel.close().await;
    }
}

impl Link for ChanLink {
    async fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.channel.wait().await? {
                ChannelMsg::Data { data } => return Some(data.to_vec()),
                ChannelMsg::ExtendedData { data, .. } => {
                    self.stderr.push_str(&String::from_utf8_lossy(&data));
                }
                ChannelMsg::Eof | ChannelMsg::Close => return None,
                _ => {}
            }
        }
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.channel
            .data(data)
            .await
            .map_err(|_| "the connection closed".to_string())
    }

    fn stderr(&self) -> &str {
        &self.stderr
    }
}

/// The protocol's reading side over a [`Link`].
struct Conn<L> {
    link: L,
    buf: VecDeque<u8>,
    eof: bool,
}

impl<L: Link> Conn<L> {
    fn new(link: L) -> Self {
        Self {
            link,
            buf: VecDeque::new(),
            eof: false,
        }
    }

    /// Reads more into the buffer; false at the end.
    async fn fill(&mut self) -> bool {
        if self.eof {
            return false;
        }
        match self.link.recv().await {
            Some(data) => {
                self.buf.extend(data);
                true
            }
            None => {
                self.eof = true;
                false
            }
        }
    }

    /// Why the other side stopped, from its error output.
    fn closed(&self) -> String {
        let text = self.link.stderr().trim();
        if text.is_empty() {
            "the other side closed the transfer (is scp available there?)".into()
        } else {
            text.lines().next_back().unwrap_or(text).trim().to_string()
        }
    }

    async fn byte(&mut self) -> Result<u8, String> {
        loop {
            if let Some(b) = self.buf.pop_front() {
                return Ok(b);
            }
            if !self.fill().await {
                return Err(self.closed());
            }
        }
    }

    async fn line(&mut self) -> Result<String, String> {
        let mut line = Vec::new();
        loop {
            match self.byte().await? {
                b'\n' => return Ok(String::from_utf8_lossy(&line).into_owned()),
                b => line.push(b),
            }
            if line.len() > 64 * 1024 {
                return Err("the other side sent an overlong line".into());
            }
        }
    }

    /// The other side's answer: fine, or its error message.
    async fn ack(&mut self) -> Result<(), String> {
        match self.byte().await? {
            0 => Ok(()),
            1 | 2 => Err(clean(&self.line().await?)),
            b => Err(format!(
                "unexpected answer from the other side ({:?}{})",
                b as char,
                String::from_utf8_lossy(&self.peek_text())
            )),
        }
    }

    fn peek_text(&self) -> Vec<u8> {
        self.buf.iter().take(80).copied().collect()
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.link.send(data).await
    }

    /// True once the other side has nothing more.
    async fn at_end(&mut self) -> bool {
        self.buf.is_empty() && !self.fill().await
    }

    /// With `sudo` or `su`: answers the password prompt and waits until the
    /// shell says `scp` is about to start.
    async fn elevate(&mut self, run_as: RunAs, password: &str) -> Result<(), String> {
        if run_as == RunAs::Me {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        // Output since the password was answered (or since the start).
        let mut seen: Vec<u8> = Vec::new();
        let mut answered = false;
        let mut quiet = false;
        loop {
            seen.extend(self.buf.drain(..));
            if let Some(at) = find(&seen, READY.as_bytes()) {
                // What follows the marker line belongs to scp.
                let mut rest = &seen[at + READY.len()..];
                while let [b'\r' | b'\n', tail @ ..] = rest {
                    rest = tail;
                }
                self.buf.extend(rest);
                return Ok(());
            }
            let text = String::from_utf8_lossy(&seen).into_owned();
            let prompt = text.trim_end();
            // sudo is asked for a prompt of ours; su's depends on the
            // language, so a line ending in ':' that then waits will do.
            let asks = prompt.ends_with(SUDO_PROMPT)
                || (run_as == RunAs::Su && quiet && prompt.ends_with(':'));
            if asks {
                if answered {
                    return Err("wrong password".into());
                }
                if password.is_empty() {
                    return Err(match run_as {
                        RunAs::Sudo => "sudo asks for a password: fill it in".into(),
                        _ => "su asks for root's password: fill it in".into(),
                    });
                }
                let mut line = Zeroizing::new(password.as_bytes().to_vec());
                line.push(b'\n');
                self.send(&line).await?;
                answered = true;
                seen.clear();
                continue;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("no answer from sudo/su".into());
            }
            match tokio::time::timeout(left.min(Duration::from_millis(300)), self.fill()).await {
                Ok(true) => quiet = false,
                Err(_) => quiet = true,
                Ok(false) => {
                    let said = clean(&text);
                    return Err(if said.is_empty() { self.closed() } else { said });
                }
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A message from the other side, without terminal noise.
fn clean(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

const CHUNK: usize = 32 * 1024;

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

struct Report<'a> {
    ev: &'a Events,
    progress: Progress,
    last: Instant,
}

impl Report<'_> {
    fn update(&mut self, force: bool) {
        if force || self.last.elapsed() >= Duration::from_millis(100) {
            self.last = Instant::now();
            self.ev.send(ScpEvent::Progress(self.progress.clone()));
        }
    }
}

/// A local file or folder to send, with what the other side is told.
enum Item {
    File {
        path: PathBuf,
        name: String,
        size: u64,
        mode: u32,
    },
    Dir {
        name: String,
        mode: u32,
    },
    End,
}

fn mode_of(meta: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        if meta.is_dir() { 0o755 } else { 0o644 }
    }
}

fn wire_name(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{}: not a file", path.display()))?;
    if name.contains('\n') {
        return Err(format!("{name:?}: names with line breaks cannot be sent"));
    }
    Ok(name)
}

/// What sending `path` takes, in order.
fn plan(path: &Path, items: &mut Vec<Item>) -> Result<(), String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = wire_name(path)?;
    if meta.is_dir() {
        items.push(Item::Dir {
            name,
            mode: mode_of(&meta),
        });
        let mut children: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        children.sort();
        for child in children {
            plan(&child, items)?;
        }
        items.push(Item::End);
    } else {
        items.push(Item::File {
            path: path.to_path_buf(),
            name,
            size: meta.len(),
            mode: mode_of(&meta),
        });
    }
    Ok(())
}

/// Sends `files` to a remote `scp -t`.
async fn send<L: Link>(
    conn: &mut Conn<L>,
    files: &[PathBuf],
    ev: &Events,
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    let mut items = Vec::new();
    for f in files {
        plan(f, &mut items)?;
    }
    let total = items
        .iter()
        .map(|i| match i {
            Item::File { size, .. } => *size,
            _ => 0,
        })
        .sum();
    let mut report = Report {
        ev,
        progress: Progress {
            total: Some(total),
            ..Progress::default()
        },
        last: Instant::now(),
    };
    conn.ack().await?;
    let mut buf = vec![0u8; CHUNK];
    for item in &items {
        cancelled(cancel)?;
        match item {
            Item::Dir { name, mode } => {
                conn.send(format!("D{mode:04o} 0 {name}\n").as_bytes())
                    .await?;
                conn.ack().await?;
            }
            Item::End => {
                conn.send(b"E\n").await?;
                conn.ack().await?;
            }
            Item::File {
                path,
                name,
                size,
                mode,
            } => {
                report.progress.file = name.clone();
                report.update(true);
                let mut file = tokio::fs::File::open(path)
                    .await
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                conn.send(format!("C{mode:04o} {size} {name}\n").as_bytes())
                    .await?;
                conn.ack().await?;
                let mut left = *size;
                while left > 0 {
                    cancelled(cancel)?;
                    let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                    let n = file
                        .read(&mut buf[..want])
                        .await
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    if n == 0 {
                        return Err(format!("{}: the file got shorter", path.display()));
                    }
                    conn.send(&buf[..n]).await?;
                    left -= n as u64;
                    report.progress.done += n as u64;
                    report.update(false);
                }
                conn.send(&[0]).await?;
                conn.ack().await?;
                report.progress.files += 1;
                report.update(true);
            }
        }
    }
    Ok(files.to_vec())
}

/// A received name that is safe to create in a local folder.
fn safe_name(name: &str) -> Result<&str, String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        Err(format!("the other side sent an unsafe name: {name:?}"))
    } else {
        Ok(name)
    }
}

/// `C0644 1234 name` or `D0755 0 name`: the size and the name.
fn parse_record(line: &str) -> Result<(u64, &str), String> {
    let mut parts = line[1..].splitn(3, ' ');
    let (_mode, size, name) = (parts.next(), parts.next(), parts.next());
    let size = size
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad record from the other side: {line:?}"))?;
    let name = name.ok_or_else(|| format!("bad record from the other side: {line:?}"))?;
    Ok((size, safe_name(name)?))
}

/// Receives from a remote `scp -f` into `dir`.
async fn receive<L: Link>(
    conn: &mut Conn<L>,
    dir: &Path,
    ev: &Events,
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    let mut report = Report {
        ev,
        progress: Progress::default(),
        last: Instant::now(),
    };
    let mut written = Vec::new();
    // Where the next file goes: `dir`, then folders the other side opens.
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    let mut partial: Option<PathBuf> = None;
    let result: Result<(), String> = async {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        conn.send(&[0]).await?;
        loop {
            cancelled(cancel)?;
            if conn.at_end().await {
                break;
            }
            let kind = conn.byte().await?;
            match kind {
                b'C' | b'D' => {
                    let line = format!("{}{}", kind as char, conn.line().await?);
                    let (size, name) = parse_record(&line)?;
                    let here = stack.last().cloned().unwrap_or_else(|| dir.to_path_buf());
                    // Top-level names never overwrite: `name (2).ext`.
                    let path = if stack.len() == 1 {
                        crate::modem::free_name(&here, name)
                    } else {
                        here.join(name)
                    };
                    if kind == b'D' {
                        tokio::fs::create_dir_all(&path)
                            .await
                            .map_err(|e| format!("{}: {e}", path.display()))?;
                        if stack.len() == 1 {
                            written.push(path.clone());
                        }
                        stack.push(path);
                        conn.send(&[0]).await?;
                        continue;
                    }
                    report.progress.file = name.to_string();
                    report.progress.done = 0;
                    report.progress.total = Some(size);
                    report.update(true);
                    let mut file = tokio::fs::File::create(&path)
                        .await
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    partial = Some(path.clone());
                    conn.send(&[0]).await?;
                    let mut left = size;
                    while left > 0 {
                        cancelled(cancel)?;
                        if conn.buf.is_empty() && !conn.fill().await {
                            return Err(conn.closed());
                        }
                        let n = conn
                            .buf
                            .len()
                            .min(usize::try_from(left).unwrap_or(usize::MAX));
                        let chunk: Vec<u8> = conn.buf.drain(..n).collect();
                        file.write_all(&chunk)
                            .await
                            .map_err(|e| format!("{}: {e}", path.display()))?;
                        left -= n as u64;
                        report.progress.done += n as u64;
                        report.update(false);
                    }
                    file.flush()
                        .await
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    drop(file);
                    conn.ack().await?;
                    partial = None;
                    if stack.len() == 1 {
                        written.push(path);
                    }
                    report.progress.files += 1;
                    report.update(true);
                    conn.send(&[0]).await?;
                }
                b'E' => {
                    conn.line().await?;
                    if stack.len() > 1 {
                        stack.pop();
                    }
                    conn.send(&[0]).await?;
                }
                b'T' => {
                    conn.line().await?;
                    conn.send(&[0]).await?;
                }
                1 | 2 => return Err(clean(&conn.line().await?)),
                b => {
                    return Err(format!(
                        "unexpected data from the other side ({:?}{})",
                        b as char,
                        String::from_utf8_lossy(&conn.peek_text())
                    ));
                }
            }
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) if written.is_empty() => Err("nothing was received".into()),
        Ok(()) => Ok(written),
        Err(e) => {
            // Leave no half-written file behind.
            if let Some(p) = partial {
                let _ = tokio::fs::remove_file(p).await;
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_for_the_other_side() {
        assert_eq!(scp_args(true, false, ""), "scp -t .");
        assert_eq!(scp_args(true, true, "/tmp/a b"), "scp -r -t '/tmp/a b'");
        assert_eq!(
            scp_args(false, false, "flash:/fw.bin"),
            "scp -f flash:/fw.bin"
        );
        assert_eq!(scp_args(false, false, "-x"), "scp -f ./-x");
        assert_eq!(command(RunAs::Me, "scp -t ."), "scp -t .");
        let sudo = command(RunAs::Sudo, "scp -t /etc");
        assert!(sudo.starts_with("sudo -k -p '[przmterm] password: ' sh -c '"));
        assert!(sudo.contains("stty raw -echo") && sudo.contains("exec scp -q -t /etc"));
        assert!(command(RunAs::Su, "scp -t /etc").starts_with("su - root -c '"));
        assert_eq!(quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn records_and_unsafe_names() {
        assert_eq!(parse_record("C0644 12 a b.txt"), Ok((12, "a b.txt")));
        assert!(parse_record("C0644 12 ../x").is_err());
        assert!(parse_record("C0644 12 a/b").is_err());
        assert!(parse_record("C0644 x a").is_err());
        assert!(parse_record("D0755 0 ..").is_err());
    }

    #[cfg(unix)]
    /// Talks to a local `scp -t`/`scp -f`, as a remote host would run it.
    struct Proc {
        child: tokio::process::Child,
        stdin: tokio::process::ChildStdin,
        stdout: tokio::process::ChildStdout,
    }

    #[cfg(unix)]
    impl Proc {
        fn spawn(args: &[&str]) -> Option<Self> {
            let mut child = tokio::process::Command::new("scp")
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let stdin = child.stdin.take()?;
            let stdout = child.stdout.take()?;
            Some(Self {
                child,
                stdin,
                stdout,
            })
        }
    }

    #[cfg(unix)]
    impl Link for Proc {
        async fn recv(&mut self) -> Option<Vec<u8>> {
            let mut buf = vec![0u8; 8192];
            match self.stdout.read(&mut buf).await {
                Ok(0) | Err(_) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some(buf)
                }
            }
        }
        async fn send(&mut self, data: &[u8]) -> Result<(), String> {
            self.stdin.write_all(data).await.map_err(|e| e.to_string())
        }
        fn stderr(&self) -> &str {
            ""
        }
    }

    fn events() -> Events {
        let (tx, _rx) = std_mpsc::channel();
        Events {
            tx,
            waker: Arc::new(|| {}),
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn sends_and_receives_through_real_scp() {
        if std::process::Command::new("scp")
            .arg("-h")
            .output()
            .is_err()
        {
            eprintln!("scp not installed: skipped");
            return;
        }
        let src = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(src.path().join("one.bin"), &data).unwrap();
        std::fs::create_dir_all(src.path().join("tree/sub")).unwrap();
        std::fs::write(src.path().join("tree/a.txt"), "alpha").unwrap();
        std::fs::write(src.path().join("tree/sub/b.txt"), "beta").unwrap();
        let remote = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(false);
        let ev = events();
        rt().block_on(async {
            // Upload: the remote side is `scp -r -t DIR`.
            let dest = remote.path().to_str().unwrap();
            let mut proc = Proc::spawn(&["-r", "-t", dest]).unwrap();
            let files = vec![src.path().join("one.bin"), src.path().join("tree")];
            let mut conn = Conn::new(proc_link(&mut proc));
            send(&mut conn, &files, &ev, &cancel).await.unwrap();
            drop(conn);
            drop(proc.stdin);
            proc.child.wait().await.unwrap();
        });
        assert_eq!(std::fs::read(remote.path().join("one.bin")).unwrap(), data);
        assert_eq!(
            std::fs::read_to_string(remote.path().join("tree/sub/b.txt")).unwrap(),
            "beta"
        );

        let local = tempfile::tempdir().unwrap();
        std::fs::write(local.path().join("one.bin"), "keep me").unwrap();
        rt().block_on(async {
            // Download of a file: `scp -f FILE`; the existing one stays.
            let file = remote.path().join("one.bin");
            let mut proc = Proc::spawn(&["-f", file.to_str().unwrap()]).unwrap();
            let mut conn = Conn::new(proc_link(&mut proc));
            let got = receive(&mut conn, local.path(), &ev, &cancel)
                .await
                .unwrap();
            assert_eq!(got, vec![local.path().join("one (2).bin")]);
            // A folder without -r: refused, as the retry expects.
            let tree = remote.path().join("tree");
            let mut proc = Proc::spawn(&["-f", tree.to_str().unwrap()]).unwrap();
            let mut conn = Conn::new(proc_link(&mut proc));
            let err = receive(&mut conn, local.path(), &ev, &cancel)
                .await
                .unwrap_err();
            assert!(err.contains("not a regular file"), "{err}");
            let mut proc = Proc::spawn(&["-r", "-f", tree.to_str().unwrap()]).unwrap();
            let mut conn = Conn::new(proc_link(&mut proc));
            receive(&mut conn, local.path(), &ev, &cancel)
                .await
                .unwrap();
        });
        assert_eq!(
            std::fs::read(local.path().join("one (2).bin")).unwrap(),
            data
        );
        assert_eq!(
            std::fs::read(local.path().join("one.bin")).unwrap(),
            b"keep me"
        );
        assert_eq!(
            std::fs::read_to_string(local.path().join("tree/sub/b.txt")).unwrap(),
            "beta"
        );
    }

    #[cfg(unix)]
    /// Borrows the pipes of `proc` as a link.
    fn proc_link(proc: &mut Proc) -> ProcRef<'_> {
        ProcRef(proc)
    }

    #[cfg(unix)]
    struct ProcRef<'a>(&'a mut Proc);

    #[cfg(unix)]
    impl Link for ProcRef<'_> {
        async fn recv(&mut self) -> Option<Vec<u8>> {
            self.0.recv().await
        }
        async fn send(&mut self, data: &[u8]) -> Result<(), String> {
            self.0.send(data).await
        }
        fn stderr(&self) -> &str {
            ""
        }
    }

    /// The prompt of sudo or su is answered once; the marker starts scp.
    struct Scripted {
        replies: VecDeque<Vec<u8>>,
        sent: Vec<u8>,
    }

    impl Link for Scripted {
        async fn recv(&mut self) -> Option<Vec<u8>> {
            let next = self.replies.pop_front()?;
            if next.is_empty() {
                // A pause: the other side waits (for a password).
                tokio::time::sleep(Duration::from_secs(1)).await;
                return Some(Vec::new());
            }
            Some(next)
        }
        async fn send(&mut self, data: &[u8]) -> Result<(), String> {
            self.sent.extend(data);
            Ok(())
        }
        fn stderr(&self) -> &str {
            ""
        }
    }

    #[test]
    fn answers_the_password_prompt() {
        rt().block_on(async {
            let link = Scripted {
                replies: VecDeque::from(vec![
                    b"Senha: ".to_vec(),
                    Vec::new(),
                    b"\r\n".to_vec(),
                    format!("{READY}\n\0").into_bytes(),
                ]),
                sent: Vec::new(),
            };
            let mut conn = Conn::new(link);
            conn.elevate(RunAs::Su, "s3cret").await.unwrap();
            assert_eq!(conn.link.sent, b"s3cret\n");
            assert_eq!(conn.byte().await.unwrap(), 0, "scp's first answer is kept");

            let link = Scripted {
                replies: VecDeque::from(vec![
                    format!("{SUDO_PROMPT} ").into_bytes(),
                    format!("\r\nSorry, try again.\r\n{SUDO_PROMPT} ").into_bytes(),
                ]),
                sent: Vec::new(),
            };
            let mut conn = Conn::new(link);
            assert_eq!(
                conn.elevate(RunAs::Sudo, "nope").await.unwrap_err(),
                "wrong password"
            );

            // No password needed (NOPASSWD, or already root).
            let link = Scripted {
                replies: VecDeque::from(vec![format!("{READY}\n").into_bytes()]),
                sent: Vec::new(),
            };
            let mut conn = Conn::new(link);
            conn.elevate(RunAs::Sudo, "").await.unwrap();
            assert!(conn.link.sent.is_empty());

            let link = Scripted {
                replies: VecDeque::from(vec![b"su: Authentication failure\r\n".to_vec()]),
                sent: Vec::new(),
            };
            let mut conn = Conn::new(link);
            assert_eq!(
                conn.elevate(RunAs::Su, "x").await.unwrap_err(),
                "su: Authentication failure"
            );
        });
    }
}
