//! SFTP over an SSH connection that is already logged in: listing folders,
//! downloads and uploads (files or whole folders, with progress and
//! cancelling), new folders, renames and deletes.
//!
//! The GUI drives an [`SftpClient`]: calls queue work and return at once;
//! results come back as [`SftpEvent`]s, read with [`SftpClient::poll`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::Waker;
use crate::pipe::runtime;
use crate::ssh::Handle;

/// One entry of a remote folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: Option<u64>,
    /// Unix mode bits.
    pub permissions: Option<u32>,
}

/// Something that happened, for the GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SftpEvent {
    /// The session is ready; `home` is where it starts.
    Ready { home: String },
    /// The contents of `dir` (an absolute path), folders first.
    Listing { dir: String, entries: Vec<Entry> },
    /// Progress of a transfer, in bytes.
    Progress { id: u64, done: u64, total: u64 },
    /// A transfer ended.
    TransferDone { id: u64, result: Result<(), String> },
    /// `dir` changed (new folder, rename, delete, upload): list it again.
    Changed { dir: String },
    /// An operation failed.
    Failed { what: String, error: String },
    /// The session ended.
    Closed(String),
    /// SCP: the shell (`sudo su -`…) asks for a password; answer with
    /// [`SftpClient::set_password`]. The text says why (asked, or wrong).
    PasswordNeeded(String),
}

pub(crate) enum Job {
    List(String),
    Download {
        id: u64,
        remote: String,
        local: PathBuf,
    },
    Upload {
        id: u64,
        local: PathBuf,
        remote_dir: String,
    },
    Mkdir(String),
    Remove {
        path: String,
        is_dir: bool,
    },
    Rename {
        from: String,
        to: String,
    },
    /// For the shell of an SCP session.
    Password(zeroize::Zeroizing<String>),
}

/// The GUI's handle on an SFTP session (or an SCP one, which takes the
/// same requests).
pub struct SftpClient {
    jobs: mpsc::UnboundedSender<Job>,
    events: std_mpsc::Receiver<SftpEvent>,
    cancelled: Arc<Mutex<HashSet<u64>>>,
}

impl SftpClient {
    pub fn list(&self, dir: &str) {
        let _ = self.jobs.send(Job::List(dir.to_string()));
    }

    /// Downloads a remote file or folder to `local` (the new file's path).
    pub fn download(&self, id: u64, remote: &str, local: PathBuf) {
        let _ = self.jobs.send(Job::Download {
            id,
            remote: remote.to_string(),
            local,
        });
    }

    /// Uploads a local file or folder into `remote_dir`.
    pub fn upload(&self, id: u64, local: PathBuf, remote_dir: &str) {
        let _ = self.jobs.send(Job::Upload {
            id,
            local,
            remote_dir: remote_dir.to_string(),
        });
    }

    pub fn mkdir(&self, path: &str) {
        let _ = self.jobs.send(Job::Mkdir(path.to_string()));
    }

    /// Deletes a file, or a folder with everything in it.
    pub fn remove(&self, path: &str, is_dir: bool) {
        let _ = self.jobs.send(Job::Remove {
            path: path.to_string(),
            is_dir,
        });
    }

    pub fn rename(&self, from: &str, to: &str) {
        let _ = self.jobs.send(Job::Rename {
            from: from.to_string(),
            to: to.to_string(),
        });
    }

    /// The password the SCP shell asked for (see
    /// [`SftpEvent::PasswordNeeded`]).
    pub fn set_password(&self, password: &str) {
        let _ = self
            .jobs
            .send(Job::Password(zeroize::Zeroizing::new(password.to_string())));
    }

    pub fn cancel(&self, id: u64) {
        if let Ok(mut c) = self.cancelled.lock() {
            c.insert(id);
        }
    }

    pub fn poll(&self) -> Vec<SftpEvent> {
        self.events.try_iter().collect()
    }
}

/// Joins a remote folder and a name.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The folder above `path` (`/` stays `/`).
pub fn parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

#[derive(Clone)]
pub(crate) struct Events {
    tx: std_mpsc::Sender<SftpEvent>,
    waker: Waker,
}

impl Events {
    pub(crate) fn send(&self, e: SftpEvent) {
        let _ = self.tx.send(e);
        (self.waker)();
    }
}

/// Cancelled transfers, by id.
pub(crate) type Cancelled = Arc<Mutex<HashSet<u64>>>;

/// A client and the worker's ends of it.
pub(crate) fn client(
    waker: Waker,
) -> (SftpClient, mpsc::UnboundedReceiver<Job>, Events, Cancelled) {
    let (jobs, rx) = mpsc::unbounded_channel();
    let (tx, events) = std_mpsc::channel();
    let cancelled: Cancelled = Arc::default();
    let client = SftpClient {
        jobs,
        events,
        cancelled: cancelled.clone(),
    };
    (client, rx, Events { tx, waker }, cancelled)
}

/// Opens an SFTP channel on `handle`'s connection.
pub(crate) fn start(handle: Arc<Handle>, waker: Waker) -> SftpClient {
    let (client, rx, ev, cancelled) = client(waker);
    runtime().spawn(run(handle, rx, ev, cancelled));
    client
}

async fn open(handle: &Handle) -> Result<SftpSession, String> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("cannot open a channel: {e}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("the server refused SFTP: {e}"))?;
    SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("SFTP did not start: {e}"))
}

async fn run(
    handle: Arc<Handle>,
    mut jobs: mpsc::UnboundedReceiver<Job>,
    ev: Events,
    cancelled: Arc<Mutex<HashSet<u64>>>,
) {
    let sftp = match open(&handle).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            ev.send(SftpEvent::Closed(e));
            crate::ssh::release(handle).await;
            return;
        }
    };
    let home = sftp
        .canonicalize(".")
        .await
        .unwrap_or_else(|_| "/".to_string());
    ev.send(SftpEvent::Ready { home });
    while let Some(job) = jobs.recv().await {
        if handle.is_closed() {
            break;
        }
        let (sftp, ev2, cancelled) = (sftp.clone(), ev.clone(), cancelled.clone());
        match job {
            // Transfers run beside the rest, so browsing goes on meanwhile.
            Job::Download { id, remote, local } => {
                runtime().spawn(async move {
                    let result = download(&sftp, id, &remote, &local, &ev2, &cancelled).await;
                    if result.is_err() {
                        // Leave no half-written file behind.
                        if local.is_file() {
                            let _ = tokio::fs::remove_file(&local).await;
                        }
                    }
                    ev2.send(SftpEvent::TransferDone { id, result });
                });
            }
            Job::Upload {
                id,
                local,
                remote_dir,
            } => {
                runtime().spawn(async move {
                    let result = upload(&sftp, id, &local, &remote_dir, &ev2, &cancelled).await;
                    ev2.send(SftpEvent::TransferDone { id, result });
                    ev2.send(SftpEvent::Changed { dir: remote_dir });
                });
            }
            Job::List(dir) => match list(&sftp, &dir).await {
                Ok((dir, entries)) => ev.send(SftpEvent::Listing { dir, entries }),
                Err(e) => ev.send(SftpEvent::Failed {
                    what: format!("Cannot open {dir}"),
                    error: e,
                }),
            },
            Job::Mkdir(path) => {
                let r = sftp.create_dir(path.clone()).await;
                done(&ev, r.map_err(|e| e.to_string()), "Cannot create", &path);
            }
            Job::Remove { path, is_dir } => {
                let r = if is_dir {
                    remove_tree(&sftp, &path).await
                } else {
                    sftp.remove_file(path.clone())
                        .await
                        .map_err(|e| e.to_string())
                };
                done(&ev, r, "Cannot delete", &path);
            }
            Job::Rename { from, to } => {
                let r = sftp.rename(from.clone(), to).await;
                done(&ev, r.map_err(|e| e.to_string()), "Cannot rename", &from);
            }
            Job::Password(_) => {}
        }
    }
    let _ = sftp.close().await;
    ev.send(SftpEvent::Closed("the session ended".into()));
    crate::ssh::release(handle).await;
}

fn done(ev: &Events, result: Result<(), String>, what: &str, path: &str) {
    match result {
        Ok(()) => ev.send(SftpEvent::Changed { dir: parent(path) }),
        Err(e) => ev.send(SftpEvent::Failed {
            what: format!("{what} {path}"),
            error: e,
        }),
    }
}

async fn list(sftp: &SftpSession, dir: &str) -> Result<(String, Vec<Entry>), String> {
    let dir = sftp
        .canonicalize(dir.to_string())
        .await
        .map_err(|e| e.to_string())?;
    let mut entries: Vec<Entry> = sftp
        .read_dir(dir.clone())
        .await
        .map_err(|e| e.to_string())?
        .filter(|e| e.file_name() != "." && e.file_name() != "..")
        .map(|e| {
            let meta = e.metadata();
            let file_type = e.file_type();
            Entry {
                name: e.file_name(),
                is_dir: file_type.is_dir(),
                is_link: file_type.is_symlink(),
                size: meta.size.unwrap_or(0),
                modified: meta.mtime.map(u64::from),
                permissions: meta.permissions,
            }
        })
        .collect();
    // A link to a folder is opened like a folder.
    for e in entries.iter_mut().filter(|e| e.is_link) {
        if let Ok(meta) = sftp.metadata(join(&dir, &e.name)).await {
            e.is_dir = meta.is_dir();
        }
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((dir, entries))
}

/// Remote files under `path` (or `path` itself), with their sizes, and the
/// folders to create, relative to `path`'s parent.
async fn remote_tree(
    sftp: &SftpSession,
    path: &str,
) -> Result<(Vec<(String, u64)>, Vec<String>), String> {
    let meta = sftp
        .metadata(path.to_string())
        .await
        .map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Ok((vec![(path.to_string(), meta.size.unwrap_or(0))], Vec::new()));
    }
    let mut files = Vec::new();
    let mut dirs = vec![path.to_string()];
    let mut queue = vec![path.to_string()];
    while let Some(dir) = queue.pop() {
        let (_, entries) = list(sftp, &dir).await?;
        for e in entries {
            let p = join(&dir, &e.name);
            if e.is_dir && !e.is_link {
                dirs.push(p.clone());
                queue.push(p);
            } else if !e.is_dir {
                files.push((p, e.size));
            }
        }
    }
    Ok((files, dirs))
}

pub(crate) fn is_cancelled(cancelled: &Mutex<HashSet<u64>>, id: u64) -> bool {
    cancelled.lock().map(|c| c.contains(&id)).unwrap_or(false)
}

struct Progress<'a> {
    id: u64,
    done: u64,
    total: u64,
    last: Instant,
    ev: &'a Events,
}

impl Progress<'_> {
    fn add(&mut self, n: u64) {
        self.done += n;
        if self.last.elapsed() >= Duration::from_millis(100) {
            self.last = Instant::now();
            self.ev.send(SftpEvent::Progress {
                id: self.id,
                done: self.done,
                total: self.total,
            });
        }
    }
}

const CHUNK: usize = 64 * 1024;

async fn download(
    sftp: &SftpSession,
    id: u64,
    remote: &str,
    local: &Path,
    ev: &Events,
    cancelled: &Mutex<HashSet<u64>>,
) -> Result<(), String> {
    let (files, dirs) = remote_tree(sftp, remote).await?;
    let base = remote.trim_end_matches('/');
    let local_path = |p: &str| -> PathBuf {
        let rel = p.strip_prefix(base).unwrap_or("").trim_start_matches('/');
        if rel.is_empty() {
            local.to_path_buf()
        } else {
            local.join(rel)
        }
    };
    for d in &dirs {
        tokio::fs::create_dir_all(local_path(d))
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut progress = Progress {
        id,
        done: 0,
        total: files.iter().map(|(_, s)| s).sum(),
        last: Instant::now(),
        ev,
    };
    let mut buf = vec![0u8; CHUNK];
    for (path, _) in &files {
        let target = local_path(path);
        let mut from = sftp.open(path.clone()).await.map_err(|e| e.to_string())?;
        let mut to = tokio::fs::File::create(&target)
            .await
            .map_err(|e| format!("{}: {e}", target.display()))?;
        loop {
            if is_cancelled(cancelled, id) {
                drop(to);
                let _ = tokio::fs::remove_file(&target).await;
                return Err("cancelled".into());
            }
            let n = from.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            to.write_all(&buf[..n]).await.map_err(|e| e.to_string())?;
            progress.add(n as u64);
        }
        to.flush().await.map_err(|e| e.to_string())?;
    }
    ev.send(SftpEvent::Progress {
        id,
        done: progress.done,
        total: progress.total,
    });
    Ok(())
}

async fn upload(
    sftp: &SftpSession,
    id: u64,
    local: &Path,
    remote_dir: &str,
    ev: &Events,
    cancelled: &Mutex<HashSet<u64>>,
) -> Result<(), String> {
    let name = local
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("nothing to upload")?;
    // Files and folders under `local`, relative to its parent.
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    if local.is_dir() {
        let mut queue = vec![local.to_path_buf()];
        while let Some(dir) = queue.pop() {
            dirs.push(dir.clone());
            let mut read = tokio::fs::read_dir(&dir).await.map_err(|e| e.to_string())?;
            while let Some(e) = read.next_entry().await.map_err(|e| e.to_string())? {
                let p = e.path();
                let meta = e.metadata().await.map_err(|e| e.to_string())?;
                if meta.is_dir() {
                    queue.push(p);
                } else if meta.is_file() {
                    files.push((p, meta.len()));
                }
            }
        }
    } else {
        let size = tokio::fs::metadata(local)
            .await
            .map_err(|e| e.to_string())?
            .len();
        files.push((local.to_path_buf(), size));
    }
    let remote_path = |p: &Path| -> String {
        let rel = p.strip_prefix(local).unwrap_or(Path::new(""));
        let mut path = join(remote_dir, &name);
        for part in rel.components() {
            path = join(&path, &part.as_os_str().to_string_lossy());
        }
        path
    };
    for d in &dirs {
        let p = remote_path(d);
        if !sftp.try_exists(p.clone()).await.unwrap_or(false) {
            sftp.create_dir(p.clone())
                .await
                .map_err(|e| format!("{p}: {e}"))?;
        }
    }
    let mut progress = Progress {
        id,
        done: 0,
        total: files.iter().map(|(_, s)| s).sum(),
        last: Instant::now(),
        ev,
    };
    let mut buf = vec![0u8; CHUNK];
    for (path, _) in &files {
        let target = remote_path(path);
        let mut from = tokio::fs::File::open(path)
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut to = sftp
            .create(target.clone())
            .await
            .map_err(|e| format!("{target}: {e}"))?;
        loop {
            if is_cancelled(cancelled, id) {
                drop(to);
                let _ = sftp.remove_file(target.clone()).await;
                return Err("cancelled".into());
            }
            let n = from.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            to.write_all(&buf[..n]).await.map_err(|e| e.to_string())?;
            progress.add(n as u64);
        }
        to.shutdown().await.map_err(|e| e.to_string())?;
    }
    ev.send(SftpEvent::Progress {
        id,
        done: progress.done,
        total: progress.total,
    });
    Ok(())
}

/// Deletes a remote folder and everything in it.
async fn remove_tree(sftp: &SftpSession, path: &str) -> Result<(), String> {
    let (files, mut dirs) = remote_tree(sftp, path).await?;
    for (f, _) in files {
        sftp.remove_file(f.clone())
            .await
            .map_err(|e| format!("{f}: {e}"))?;
    }
    // Deepest first.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.len()));
    for d in dirs {
        sftp.remove_dir(d.clone())
            .await
            .map_err(|e| format!("{d}: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("/home/ops", "a.txt"), "/home/ops/a.txt");
        assert_eq!(parent("/home/ops/a.txt"), "/home/ops");
        assert_eq!(parent("/home"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(parent("/home/ops/"), "/home");
    }
}
