//! SCP over an SSH connection that is already logged in: sends files and
//! folders to the other side or fetches them, on a channel of its own, so
//! the terminal stays as it is.
//!
//! It talks the classic protocol to the remote `scp -t` (receive) or
//! `scp -f` (send), which Unix hosts and much network equipment (Cisco,
//! Huawei, Juniper…) answer. Like WinSCP, the remote commands can run in a
//! chosen shell — `/bin/bash`, or `sudo su -` to work as root: the shell
//! gets a terminal of its own, PrzmTerm answers its password prompt, and
//! types the commands into it (the terminal made raw before a transfer).

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

/// Shells offered for SCP, as in WinSCP. `Default` is the login shell.
pub const SHELLS: [&str; 5] = ["Default", "/bin/bash", "/bin/ksh", "sudo su -", "su -"];

/// The shell to use, from a setting: `None` for the login shell.
pub fn shell_from(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && !text.eq_ignore_ascii_case("default")).then(|| text.to_string())
}

/// Returned when the shell asks for a password and none was given.
pub const PASSWORD_NEEDED: &str = "the shell asks for a password";
/// Returned when the shell asked again after the password.
pub const WRONG_PASSWORD: &str = "wrong password";

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
    /// See [`shell_from`]; `None` runs `scp` directly.
    pub shell: Option<String>,
    /// For the shell's password prompt (`sudo su -`, `su -`).
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
    let stop = cancel.clone();
    runtime().spawn(async move {
        let send = |e: ScpEvent| {
            let _ = tx.send(e);
            waker();
        };
        let report = |p: &Progress| send(ScpEvent::Progress(p.clone()));
        let work = transfer(
            &handle,
            &request.direction,
            request.shell.as_deref(),
            &request.password,
            &report,
            &stop,
        );
        let result = until_cancelled(work, &stop).await;
        send(ScpEvent::Done(result));
        crate::ssh::release(handle).await;
    });
    ScpTransfer { events, cancel }
}

/// Runs `work`; the work sees a cancel between chunks, and one stuck
/// waiting for the other side is dropped (closing its channel) a moment
/// after.
pub(crate) async fn until_cancelled<T>(
    work: impl std::future::Future<Output = Result<T, String>>,
    stop: &AtomicBool,
) -> Result<T, String> {
    let stuck = async {
        while !stop.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    tokio::select! {
        result = work => result,
        () = stuck => Err("cancelled".to_string()),
    }
}

/// Sends or fetches, as `direction` says.
pub(crate) async fn transfer(
    handle: &Handle,
    direction: &Direction,
    shell: Option<&str>,
    password: &str,
    report: &(dyn Fn(&Progress) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    match direction {
        Direction::Upload { files, remote_dir } => {
            let recursive = files.iter().any(|f| f.is_dir());
            let scp = scp_args(true, recursive, remote_dir);
            let mut conn = open_scp(handle, shell, password, &scp).await?;
            let sent = send(&mut conn, files, report, cancel).await;
            conn.link.finish(shell.is_none()).await;
            sent
        }
        Direction::Download { remote, local_dir } => {
            // Without -r first: some equipment does not know it. A folder
            // then answers "not a regular file", and -r follows.
            for recursive in [false, true] {
                let scp = scp_args(false, recursive, remote);
                let mut conn = open_scp(handle, shell, password, &scp).await?;
                match receive(&mut conn, local_dir, report, cancel).await {
                    Err(e) if !recursive && e.contains("not a regular file") => continue,
                    other => return other,
                }
            }
            unreachable!("the loop returns")
        }
    }
}

/// The remote `scp`, ready for the protocol.
async fn open_scp(
    handle: &Handle,
    shell: Option<&str>,
    password: &str,
    scp: &str,
) -> Result<Conn<ChanLink>, String> {
    let Some(shell) = shell else {
        return Ok(Conn::new(open_channel(handle, None, scp).await?));
    };
    let mut conn = Conn::new(open_channel(handle, Some(shell), shell).await?);
    conn.enter_shell(password).await?;
    conn.send(scp_line(scp).as_bytes()).await?;
    conn.wait_for(SCP_READY.as_bytes()).await?;
    Ok(conn)
}

/// Typed into the shell: raw terminal, a marker, then `scp` in its place.
fn scp_line(scp: &str) -> String {
    // On a terminal scp would draw its progress meter into the transfer:
    // -q keeps it quiet.
    let quiet = scp.replacen("scp ", "scp -q ", 1);
    format!("stty raw -echo; printf '%s%s\\n' {SCP_READY_A} {SCP_READY_B}; exec {quiet}\n")
}

// Markers printed by the shell. Each is typed in two halves, so the
// terminal's echo of the typed line is never taken for the output.
const SHELL_READY_A: &str = "PRZMTERM-SHELL-";
const SHELL_READY_B: &str = "READY";
const SHELL_READY: &str = "PRZMTERM-SHELL-READY";
const SCP_READY_A: &str = "PRZMTERM-SCP-";
const SCP_READY_B: &str = "READY";
const SCP_READY: &str = "PRZMTERM-SCP-READY";

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

/// `s` as one shell word; left bare when that is safe, so equipment
/// without a Unix shell still understands plain paths (`flash:/x.bin`).
pub(crate) fn quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%+=,~".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// A channel running `command`; with a shell, on a terminal of its own
/// (sudo and su read the password from one).
pub(crate) async fn open_channel(
    handle: &Handle,
    shell: Option<&str>,
    command: &str,
) -> Result<ChanLink, String> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("cannot open a channel: {e}"))?;
    if shell.is_some() {
        channel
            .request_pty(false, "dumb", 250, 24, 0, 0, &[])
            .await
            .map_err(|e| format!("the server refused a terminal: {e}"))?;
    }
    channel
        .exec(true, command)
        .await
        .map_err(|e| format!("the server refused to run {command}: {e}"))?;
    Ok(ChanLink {
        channel,
        stderr: String::new(),
    })
}

/// A byte pipe to the remote `scp`.
pub(crate) trait Link {
    /// The next bytes, or `None` once the other side is done.
    async fn recv(&mut self) -> Option<Vec<u8>>;
    async fn send(&mut self, data: &[u8]) -> Result<(), String>;
    /// What the other side printed on its error output.
    fn stderr(&self) -> &str;
}

pub(crate) struct ChanLink {
    channel: russh::Channel<russh::client::Msg>,
    stderr: String,
}

impl ChanLink {
    /// Ends the channel. A plain `scp -t` stops at end of input; behind a
    /// shell its terminal does not pass that on, so the channel closes
    /// (every file is complete once acknowledged).
    pub(crate) async fn finish(&mut self, plain: bool) {
        if plain {
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
pub(crate) struct Conn<L> {
    link: L,
    buf: VecDeque<u8>,
    eof: bool,
}

impl<L: Link> Conn<L> {
    pub(crate) fn new(link: L) -> Self {
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

    pub(crate) async fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.link.send(data).await
    }

    /// True once the other side has nothing more.
    async fn at_end(&mut self) -> bool {
        self.buf.is_empty() && !self.fill().await
    }

    /// In a shell just started (`sudo su -`, `/bin/bash`…): answers its
    /// password prompt and waits until it runs what is typed.
    pub(crate) async fn enter_shell(&mut self, password: &str) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(30);
        // Output since the start, or since the password was answered.
        let mut seen: Vec<u8> = Vec::new();
        let mut answered = false;
        let mut quiet = false;
        let mut probed: Option<Instant> = None;
        loop {
            seen.extend(self.buf.drain(..));
            if let Some(at) = find(&seen, SHELL_READY.as_bytes()) {
                let mut rest = &seen[at + SHELL_READY.len()..];
                while let [b'\r' | b'\n', tail @ ..] = rest {
                    rest = tail;
                }
                self.buf.extend(rest);
                return Ok(());
            }
            let text = String::from_utf8_lossy(&seen).into_owned();
            if quiet {
                // A line ending in ':' that then waits: a password prompt
                // (whatever the language).
                if text.trim_end().ends_with(':') {
                    if answered {
                        return Err(WRONG_PASSWORD.into());
                    }
                    if password.is_empty() {
                        return Err(PASSWORD_NEEDED.into());
                    }
                    let mut line = Zeroizing::new(password.as_bytes().to_vec());
                    line.push(b'\n');
                    self.send(&line).await?;
                    answered = true;
                    seen.clear();
                    probed = None;
                    quiet = false;
                    continue;
                }
                // Ask the shell to answer. Typed again now and then: what
                // is typed while sudo or su switch over can be thrown away.
                if probed.is_none_or(|t| t.elapsed() >= Duration::from_millis(1500)) {
                    let probe = format!("printf '%s%s\\n' {SHELL_READY_A} {SHELL_READY_B}\n");
                    self.send(probe.as_bytes()).await?;
                    probed = Some(Instant::now());
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("the shell did not answer".into());
            }
            match tokio::time::timeout(left.min(Duration::from_millis(400)), self.fill()).await {
                Ok(true) => quiet = false,
                Err(_) => quiet = true,
                Ok(false) => {
                    let said = clean(&text);
                    return Err(if said.is_empty() { self.closed() } else { said });
                }
            }
        }
    }

    /// Reads up to and past the line with `marker`; what came before is
    /// dropped (or, at the end, is the error).
    pub(crate) async fn wait_for(&mut self, marker: &[u8]) -> Result<(), String> {
        let mut seen: Vec<u8> = Vec::new();
        loop {
            seen.extend(self.buf.drain(..));
            if let Some(at) = find(&seen, marker) {
                let mut rest = &seen[at + marker.len()..];
                while let [b'\r' | b'\n', tail @ ..] = rest {
                    rest = tail;
                }
                self.buf.extend(rest);
                return Ok(());
            }
            if !self.fill().await {
                let said = clean(&String::from_utf8_lossy(&seen));
                return Err(if said.is_empty() { self.closed() } else { said });
            }
        }
    }

    /// What is read but not taken yet.
    pub(crate) fn take_buffered(&mut self) -> Vec<u8> {
        self.buf.drain(..).collect()
    }

    /// Reads more; false at the end.
    pub(crate) async fn more(&mut self) -> bool {
        self.fill().await
    }

    /// Everything until the other side is done.
    pub(crate) async fn read_all(&mut self) -> Vec<u8> {
        while self.fill().await {}
        self.buf.drain(..).collect()
    }

    pub(crate) fn stderr(&self) -> &str {
        self.link.stderr()
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A message from the other side, without terminal noise.
pub(crate) fn clean(text: &str) -> String {
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
    report: &'a (dyn Fn(&Progress) + Sync),
    progress: Progress,
    last: Instant,
}

impl Report<'_> {
    fn update(&mut self, force: bool) {
        if force || self.last.elapsed() >= Duration::from_millis(100) {
            self.last = Instant::now();
            (self.report)(&self.progress);
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
    report: &(dyn Fn(&Progress) + Sync),
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
        report,
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
    report: &(dyn Fn(&Progress) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<PathBuf>, String> {
    let mut report = Report {
        report,
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
                    report.progress.total = Some(report.progress.total.unwrap_or(0) + size);
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
        let line = scp_line("scp -t /etc");
        assert!(line.starts_with("stty raw -echo; printf '%s%s\\n' PRZMTERM-SCP- READY;"));
        assert!(line.ends_with("exec scp -q -t /etc\n"));
        assert!(
            !line.contains(SCP_READY),
            "the echo must not look like the marker"
        );
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(shell_from(" Default "), None);
        assert_eq!(shell_from(""), None);
        assert_eq!(shell_from("sudo su -").as_deref(), Some("sudo su -"));
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
        let ev = |_: &Progress| {};
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
            // su: a prompt in Portuguese, then the shell answers the probe.
            let link = Scripted {
                replies: VecDeque::from(vec![
                    b"Senha: ".to_vec(),
                    Vec::new(),
                    b"\r\n".to_vec(),
                    Vec::new(),
                    format!("{SHELL_READY}\r\n\0").into_bytes(),
                ]),
                sent: Vec::new(),
            };
            let mut conn = Conn::new(link);
            conn.enter_shell("s3cret").await.unwrap();
            let sent = String::from_utf8(conn.link.sent.clone()).unwrap();
            assert!(sent.starts_with("s3cret\n"), "{sent:?}");
            assert!(sent.contains("printf '%s%s\\n' PRZMTERM-SHELL- READY"));
            assert_eq!(conn.byte().await.unwrap(), 0, "what follows is kept");

            // sudo asking again: a wrong password.
            let link = Scripted {
                replies: VecDeque::from(vec![
                    b"[sudo] password for przm: ".to_vec(),
                    Vec::new(),
                    b"\r\nSorry, try again.\r\n[sudo] password for przm: ".to_vec(),
                    Vec::new(),
                ]),
                sent: Vec::new(),
            };
            let mut conn = Conn::new(link);
            assert_eq!(conn.enter_shell("nope").await.unwrap_err(), WRONG_PASSWORD);

            // A prompt and no password to give.
            let link = Scripted {
                replies: VecDeque::from(vec![b"Password: ".to_vec(), Vec::new()]),
                sent: Vec::new(),
            };
            let mut conn = Conn::new(link);
            assert_eq!(conn.enter_shell("").await.unwrap_err(), PASSWORD_NEEDED);

            // No password needed (NOPASSWD, /bin/bash…).
            let link = Scripted {
                replies: VecDeque::from(vec![
                    b"root@vm:~# ".to_vec(),
                    Vec::new(),
                    format!("printf '%s%s\\n' PRZMTERM-SHELL- READY\r\n{SHELL_READY}\r\n")
                        .into_bytes(),
                ]),
                sent: Vec::new(),
            };
            let mut conn = Conn::new(link);
            conn.enter_shell("").await.unwrap();

            let link = Scripted {
                replies: VecDeque::from(vec![
                    b"Password: ".to_vec(),
                    Vec::new(),
                    b"\r\nsu: Authentication failure\r\n".to_vec(),
                ]),
                sent: Vec::new(),
            };
            let mut conn = Conn::new(link);
            assert_eq!(
                conn.enter_shell("x").await.unwrap_err(),
                "su: Authentication failure"
            );
        });
    }
}
