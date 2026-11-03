//! An SCP session for the file browser, as WinSCP's SCP mode: folders are
//! listed with `ls`, changed with `mkdir`, `rm` and `mv`, and files move
//! with `scp` — all run in the chosen shell (`sudo su -` to work as root).
//! It answers the same [`SftpClient`] requests as an SFTP session.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::Waker;
use crate::modem::Progress;
use crate::pipe::runtime;
use crate::scp::{self, Conn, Direction, PASSWORD_NEEDED, WRONG_PASSWORD, open_channel, quote};
use crate::sftp::{self, Cancelled, Entry, Events, Job, SftpClient, SftpEvent, parent};
use crate::ssh::Handle;

/// Starts an SCP session on `handle`'s connection, its commands run in
/// `shell` (`None`: the login shell).
pub(crate) fn start(handle: Arc<Handle>, shell: Option<String>, waker: Waker) -> SftpClient {
    let (client, rx, ev, cancelled) = sftp::client(waker);
    let shared = Arc::new(Shared {
        handle,
        shell,
        password: Mutex::new(Zeroizing::new(String::new())),
        session: tokio::sync::Mutex::new(None),
    });
    runtime().spawn(run(shared, rx, ev, cancelled));
    client
}

struct Shared {
    handle: Arc<Handle>,
    shell: Option<String>,
    /// For the shell's prompt, once the user gave it.
    password: Mutex<Zeroizing<String>>,
    /// The shell that runs the commands, kept open between them so each
    /// one does not log in again.
    session: tokio::sync::Mutex<Option<Conn<scp::ChanLink>>>,
}

impl Shared {
    fn password(&self) -> Zeroizing<String> {
        self.password
            .lock()
            .map(|p| p.clone())
            .unwrap_or_else(|_| Zeroizing::new(String::new()))
    }

    fn set_password(&self, password: Zeroizing<String>) {
        if let Ok(mut p) = self.password.lock() {
            *p = password;
        }
    }
}

async fn run(
    shared: Arc<Shared>,
    mut jobs: mpsc::UnboundedReceiver<Job>,
    ev: Events,
    cancelled: Cancelled,
) {
    let mut ready = init(&shared, &ev).await;
    while let Some(job) = jobs.recv().await {
        if shared.handle.is_closed() {
            break;
        }
        match job {
            Job::Password(password) => {
                shared.set_password(password);
                if !ready {
                    ready = init(&shared, &ev).await;
                }
            }
            // Nothing works before the shell does.
            _ if !ready => {}
            Job::List(dir) => match list(&shared, &dir).await {
                Ok((dir, entries)) => ev.send(SftpEvent::Listing { dir, entries }),
                Err(e) => failed(&shared, &ev, &format!("Cannot open {dir}"), e),
            },
            Job::Mkdir(path) => {
                let r = command(&shared, &format!("mkdir -- {}", quote(&path))).await;
                done(&shared, &ev, r, "Cannot create", &path);
            }
            Job::Remove { path, is_dir } => {
                let flags = if is_dir { "-rf" } else { "-f" };
                let r = command(&shared, &format!("rm {flags} -- {}", quote(&path))).await;
                done(&shared, &ev, r, "Cannot delete", &path);
            }
            Job::Rename { from, to } => {
                let r = command(&shared, &format!("mv -- {} {}", quote(&from), quote(&to))).await;
                done(&shared, &ev, r, "Cannot rename", &from);
            }
            // Transfers run beside the rest, so browsing goes on meanwhile.
            Job::Download { id, remote, local } => {
                let (shared, ev, cancelled) = (shared.clone(), ev.clone(), cancelled.clone());
                runtime().spawn(async move {
                    let local_dir = local
                        .parent()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."));
                    let direction = Direction::Download { remote, local_dir };
                    let result = moved(&shared, id, &direction, &ev, &cancelled).await;
                    ev.send(SftpEvent::TransferDone { id, result });
                });
            }
            Job::Upload {
                id,
                local,
                remote_dir,
            } => {
                let (shared, ev, cancelled) = (shared.clone(), ev.clone(), cancelled.clone());
                runtime().spawn(async move {
                    let direction = Direction::Upload {
                        files: vec![local],
                        remote_dir: remote_dir.clone(),
                    };
                    let result = moved(&shared, id, &direction, &ev, &cancelled).await;
                    ev.send(SftpEvent::TransferDone { id, result });
                    ev.send(SftpEvent::Changed { dir: remote_dir });
                });
            }
        }
    }
    ev.send(SftpEvent::Closed("the session ended".into()));
    let handle = shared.handle.clone();
    drop(shared);
    crate::ssh::release(handle).await;
}

/// Finds the home folder: the session is ready once the shell answers.
async fn init(shared: &Shared, ev: &Events) -> bool {
    match command(shared, "pwd").await {
        Ok(out) => {
            let home = out.lines().next().unwrap_or("/").trim().to_string();
            ev.send(SftpEvent::Ready {
                home: if home.is_empty() { "/".into() } else { home },
            });
            true
        }
        Err(e) => {
            failed(shared, ev, "Cannot start the SCP session", e);
            false
        }
    }
}

/// Reports `e`; a password problem asks for the password instead.
fn failed(shared: &Shared, ev: &Events, what: &str, e: String) {
    let shell = shared.shell.as_deref().unwrap_or("the shell");
    if e == PASSWORD_NEEDED {
        ev.send(SftpEvent::PasswordNeeded(format!(
            "{shell} asks for a password."
        )));
    } else if e == WRONG_PASSWORD {
        shared.set_password(Zeroizing::new(String::new()));
        ev.send(SftpEvent::PasswordNeeded(format!(
            "Wrong password for {shell}. Try again."
        )));
    } else {
        ev.send(SftpEvent::Failed {
            what: what.to_string(),
            error: e,
        });
    }
}

fn done(shared: &Shared, ev: &Events, result: Result<String, String>, what: &str, path: &str) {
    match result {
        Ok(_) => ev.send(SftpEvent::Changed { dir: parent(path) }),
        Err(e) => failed(shared, ev, &format!("{what} {path}"), e),
    }
}

/// One transfer, with progress and cancelling by `id`.
async fn moved(
    shared: &Shared,
    id: u64,
    direction: &Direction,
    ev: &Events,
    cancelled: &Cancelled,
) -> Result<(), String> {
    let stop = AtomicBool::new(false);
    let report = |p: &Progress| {
        ev.send(SftpEvent::Progress {
            id,
            done: p.done,
            total: p.total.unwrap_or(0),
        });
    };
    let password = shared.password();
    let work = scp::transfer(
        &shared.handle,
        direction,
        shared.shell.as_deref(),
        &password,
        &report,
        &stop,
    );
    // The browser cancels by id; the transfer watches a flag.
    let watch = async {
        loop {
            if sftp::is_cancelled(cancelled, id) {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        std::future::pending::<()>().await;
    };
    let result = tokio::select! {
        r = scp::until_cancelled(work, &stop) => r,
        () = watch => unreachable!("never ends"),
    };
    match result {
        Ok(_) => Ok(()),
        Err(e) if e == PASSWORD_NEEDED || e == WRONG_PASSWORD => {
            failed(shared, ev, "", e);
            Err("the shell needs a password".into())
        }
        Err(e) => Err(e),
    }
}

// Markers around a command's output, typed in halves so the terminal's echo
// of the command never looks like them.
const BEGIN: &str = "PRZMTERM-BEGIN";
const END: &str = "PRZMTERM-END ";

/// Runs `script` in the session's shell; its output, or why it failed.
async fn command(shared: &Shared, script: &str) -> Result<String, String> {
    let body = format!(
        "printf '%s%s\\n' PRZMTERM- BEGIN; {script}; printf '\\n%s%s %s\\n' PRZMTERM- END \"$?\""
    );
    let (output, stderr) = match shared.shell.as_deref() {
        None => {
            let mut conn = Conn::new(open_channel(&shared.handle, None, &body).await?);
            let out = tokio::time::timeout(Duration::from_secs(60), conn.read_all())
                .await
                .map_err(|_| "no answer from the other side".to_string())?;
            (out, conn.stderr().to_string())
        }
        Some(shell) => (in_shell(shared, shell, &body).await?, String::new()),
    };
    parse_output(&String::from_utf8_lossy(&output), &stderr)
}

/// Runs `body` in the kept shell, starting it (again) when needed.
async fn in_shell(shared: &Shared, shell: &str, body: &str) -> Result<Vec<u8>, String> {
    let mut session = shared.session.lock().await;
    for attempt in 0..2 {
        if session.is_none() {
            let mut conn = Conn::new(open_channel(&shared.handle, Some(shell), shell).await?);
            conn.enter_shell(&shared.password()).await?;
            conn.send(b"stty -echo\n").await?;
            *session = Some(conn);
        }
        let Some(conn) = session.as_mut() else {
            continue;
        };
        if conn.send(format!("{body}\n").as_bytes()).await.is_err() {
            *session = None;
            continue;
        }
        match read_until_end(conn).await {
            Ok(out) if find(&out, END.as_bytes()).is_some() => return Ok(out),
            // The shell went away (logged out, timed out): once more.
            Ok(out) => {
                *session = None;
                if attempt == 1 {
                    return Ok(out);
                }
            }
            Err(e) => {
                *session = None;
                return Err(e);
            }
        }
    }
    Err("the shell did not answer".into())
}

/// Reads until the end marker's line is complete.
async fn read_until_end(conn: &mut Conn<scp::ChanLink>) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut out = Vec::new();
    loop {
        out.extend(conn.take_buffered());
        if let Some(at) = find(&out, END.as_bytes())
            && out[at..].contains(&b'\n')
        {
            return Ok(out);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, conn.more()).await {
            Ok(true) => {}
            Ok(false) => return Ok(out),
            Err(_) => return Err("no answer from the shell".into()),
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The output between the markers, or the error.
fn parse_output(raw: &str, stderr: &str) -> Result<String, String> {
    let text = raw.replace('\r', "");
    let error = |body: &str| -> String {
        let said = scp::clean(stderr);
        if said.is_empty() {
            let said = scp::clean(body);
            if said.is_empty() {
                "the command failed".into()
            } else {
                said
            }
        } else {
            said
        }
    };
    let Some(begin) = text.find(BEGIN) else {
        return Err(error(&text));
    };
    let after = &text[begin + BEGIN.len()..];
    let after = after.strip_prefix('\n').unwrap_or(after);
    let Some(end) = after.rfind(END) else {
        return Err(error(after));
    };
    let body = after[..end].trim_end_matches('\n');
    let status = after[end + END.len()..].lines().next().unwrap_or("").trim();
    if status == "0" {
        Ok(body.to_string())
    } else {
        Err(error(body))
    }
}

/// The contents of `dir`, as an absolute path and its entries, folders
/// first.
async fn list(shared: &Shared, dir: &str) -> Result<(String, Vec<Entry>), String> {
    let script = format!(
        "cd -- {} && pwd && {{ LC_ALL=C ls -laL --full-time 2>/dev/null || LC_ALL=C ls -laL; }}",
        quote(dir)
    );
    let out = command(shared, &script).await?;
    let mut lines = out.lines();
    let dir = lines.next().unwrap_or(dir).trim().to_string();
    let mut entries: Vec<Entry> = lines
        .filter_map(parse_ls_line)
        .filter(|e| e.name != "." && e.name != "..")
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((dir, entries))
}

/// The words of `line` with where each starts.
fn words(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                out.push((s, &line[s..i]));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, &line[s..]));
    }
    out
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn hms(text: &str) -> Option<i64> {
    let mut parts = text.split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let s: i64 = parts
        .next()
        .map(|s| s.split('.').next().unwrap_or("0").parse().unwrap_or(0))
        .unwrap_or(0);
    Some(h * 3600 + m * 60 + s)
}

/// The current year, for `ls` dates without one.
fn this_year() -> i64 {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    // Close enough: dates without a year are within the last six months.
    1970 + (days * 400 / 146_097)
}

/// One line of `ls -la` (GNU with or without `--full-time`, BSD, BusyBox).
fn parse_ls_line(line: &str) -> Option<Entry> {
    let line = line.trim_end_matches('\r');
    let w = words(line);
    let perms = w.first()?.1;
    let kind = perms.chars().next()?;
    if perms.len() < 10 || !"-dlcbpsD".contains(kind) {
        return None;
    }
    // perms, links, owner, group, size (a device has "major, minor").
    let mut i = 4;
    let token = w.get(i)?.1;
    let size = if token.ends_with(',') {
        i += 2;
        0
    } else if let Ok(n) = token.parse::<u64>() {
        i += 1;
        n
    } else if w.get(3)?.1.parse::<u64>().is_ok() {
        // No group column (some systems): the size came earlier.
        w[3].1.parse().ok()?
    } else {
        return None;
    };
    let first = w.get(i)?.1;
    let (modified, name_at) = if first.len() == 10 && first.as_bytes()[4] == b'-' {
        // 2026-11-02 22:57:04.123456789 -0300
        let mut parts = first.split('-').map(|p| p.parse::<i64>().ok());
        let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
        let time = hms(w.get(i + 1)?.1).unwrap_or(0);
        let mut next = i + 2;
        let mut offset = 0;
        if let Some((_, tz)) = w.get(next)
            && (tz.starts_with('+') || tz.starts_with('-'))
            && tz.len() == 5
        {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let h: i64 = tz[1..3].parse().unwrap_or(0);
            let mm: i64 = tz[3..5].parse().unwrap_or(0);
            offset = sign * (h * 3600 + mm * 60);
            next += 1;
        }
        let at = days_from_civil(y, m, d) * 86_400 + time - offset;
        (u64::try_from(at).ok(), next)
    } else {
        // [Sat] Nov  2 22:57[:04] [2026]   or   Nov  2  2025
        let mut j = i;
        if WEEKDAYS.contains(&w.get(j)?.1) {
            j += 1;
        }
        let month_name = w.get(j)?.1;
        let month = MONTHS.iter().position(|m| *m == month_name)? as i64 + 1;
        let day: i64 = w.get(j + 1)?.1.parse().ok()?;
        let third = w.get(j + 2)?.1;
        let mut next = j + 3;
        let (year, time) = if third.contains(':') {
            // BusyBox --full-time puts the year after the time.
            match w.get(next).and_then(|(_, y)| y.parse::<i64>().ok()) {
                Some(y) if y > 1900 => {
                    next += 1;
                    (y, hms(third).unwrap_or(0))
                }
                _ => (this_year(), hms(third).unwrap_or(0)),
            }
        } else {
            (third.parse().ok()?, 0)
        };
        let at = days_from_civil(year, month, day) * 86_400 + time;
        (u64::try_from(at).ok(), next)
    };
    let start = w.get(name_at)?.0;
    let mut name = &line[start..];
    if kind == 'l'
        && let Some((n, _target)) = name.split_once(" -> ")
    {
        name = n;
    }
    let mut mode = 0u32;
    for (k, c) in perms.chars().skip(1).take(9).enumerate() {
        if c != '-' && !matches!(c, 'S' | 'T') {
            mode |= 1 << (8 - k);
        }
    }
    Some(Entry {
        name: name.to_string(),
        is_dir: kind == 'd',
        is_link: kind == 'l',
        size,
        modified,
        permissions: Some(mode),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_lines_from_several_systems() {
        let e = parse_ls_line(
            "-rw-r--r-- 1 przm przm 300000 2026-11-02 22:57:04.123456789 -0300 my file.bin",
        )
        .unwrap();
        assert_eq!(e.name, "my file.bin");
        assert_eq!(e.size, 300_000);
        assert!(!e.is_dir);
        assert_eq!(e.permissions, Some(0o644));
        // 2026-11-03 01:57:04 UTC.
        assert_eq!(e.modified, Some(1_793_671_024));

        let e = parse_ls_line("drwxr-xr-x  2 root root 4096 Nov  2  2025 etc").unwrap();
        assert!(e.is_dir);
        assert_eq!(e.name, "etc");
        assert_eq!(e.permissions, Some(0o755));

        let e = parse_ls_line("drwxr-x---+ 3 root wheel 96 Jan 15 09:30 with  two spaces").unwrap();
        assert_eq!(e.name, "with  two spaces");

        // BusyBox --full-time.
        let e = parse_ls_line(
            "-rwxr-xr-x    1 root     root        1024 Sat Nov  2 22:57:04 2026 fw.bin",
        )
        .unwrap();
        assert_eq!(e.name, "fw.bin");
        assert_eq!(e.size, 1024);

        let e =
            parse_ls_line("crw-rw-rw- 1 root root 1, 3 2026-10-01 08:00:00.0 +0000 null").unwrap();
        assert_eq!(e.name, "null");
        assert_eq!(e.size, 0);

        let e =
            parse_ls_line("lrwxrwxrwx 1 root root 7 2026-10-01 08:00:00.0 +0000 bin -> usr/bin")
                .unwrap();
        assert_eq!(e.name, "bin");
        assert!(e.is_link);

        assert!(parse_ls_line("total 48").is_none());
        assert!(parse_ls_line("ls: cannot access 'x': No such file or directory").is_none());
    }

    #[test]
    fn output_between_the_markers() {
        let raw = "stty -echo; printf '%s%s\\n' PRZMTERM- BEGIN; pwd\r\n\
                   PRZMTERM-BEGIN\r\n/root\r\n\r\nPRZMTERM-END 0\r\nlogout\r\n";
        assert_eq!(parse_output(raw, "").unwrap(), "/root");
        let raw =
            "PRZMTERM-BEGIN\nmkdir: cannot create directory 'x': File exists\n\nPRZMTERM-END 1\n";
        assert_eq!(
            parse_output(raw, "").unwrap_err(),
            "mkdir: cannot create directory 'x': File exists"
        );
        let raw = "PRZMTERM-BEGIN\n\nPRZMTERM-END 1\n";
        assert_eq!(
            parse_output(raw, "rm: cannot remove 'y': Permission denied\n").unwrap_err(),
            "rm: cannot remove 'y': Permission denied"
        );
        assert_eq!(
            parse_output("sh: 1: printf: not found\n", "").unwrap_err(),
            "sh: 1: printf: not found"
        );
    }
}
