//! File transfers inside the terminal: Xmodem, Xmodem-1K, Ymodem and
//! Zmodem, as used by serial consoles (firmware uploads) and by `sz`/`rz`
//! on Unix hosts.
//!
//! The engines do no I/O of their own on the connection: the pane feeds them
//! what the session receives ([`Engine::input`]), sends what they produce
//! ([`Engine::output`]) and lets time pass ([`Engine::tick`]) for timeouts.
//! Local files are read and written directly.

mod crc;
mod xymodem;
mod zmodem;

use std::path::PathBuf;
use std::time::Instant;

pub use crc::{crc16, crc32};

/// Which protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 128-byte blocks, CRC (falls back to the checksum when the other
    /// side asks for it).
    Xmodem,
    /// 1024-byte blocks.
    Xmodem1k,
    /// 1024-byte blocks, file names and sizes, several files.
    Ymodem,
    /// Streaming, file names, several files, resumes after errors.
    Zmodem,
}

impl Kind {
    /// The most capable first.
    pub const ALL: [Kind; 4] = [Kind::Zmodem, Kind::Ymodem, Kind::Xmodem1k, Kind::Xmodem];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Xmodem => "Xmodem",
            Kind::Xmodem1k => "Xmodem-1K",
            Kind::Ymodem => "Ymodem",
            Kind::Zmodem => "Zmodem",
        }
    }

    /// Xmodem sends one file, without its name.
    pub fn sends_names(self) -> bool {
        matches!(self, Kind::Ymodem | Kind::Zmodem)
    }

    /// The command that starts the other end on a Unix host (lrzsz).
    pub fn remote_receive_command(self) -> &'static str {
        match self {
            Kind::Xmodem | Kind::Xmodem1k => "rx",
            Kind::Ymodem => "rb",
            Kind::Zmodem => "rz",
        }
    }
}

/// How far a transfer has come.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// The file being transferred.
    pub file: String,
    pub done: u64,
    /// `None` when the other side did not say (Xmodem).
    pub total: Option<u64>,
    /// Files completed so far.
    pub files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Running,
    /// Done; the files written (when receiving) or sent.
    Finished(Vec<PathBuf>),
    Failed(String),
    Cancelled,
}

/// A transfer in progress.
pub trait Engine: Send {
    /// Bytes received from the other side.
    fn input(&mut self, data: &[u8], now: Instant);
    /// Bytes to send to the other side, taken out.
    fn output(&mut self) -> Vec<u8>;
    /// Lets time pass: retries and timeouts.
    fn tick(&mut self, now: Instant);
    fn progress(&self) -> Progress;
    fn status(&self) -> Status;
    /// Stops the transfer, telling the other side.
    fn cancel(&mut self);
}

/// Sends `files` (Xmodem takes the first one only).
pub fn send(kind: Kind, files: Vec<PathBuf>, now: Instant) -> Result<Box<dyn Engine>, String> {
    if files.is_empty() {
        return Err("no file to send".into());
    }
    Ok(match kind {
        Kind::Zmodem => Box::new(zmodem::Sender::new(files, now)?),
        _ => Box::new(xymodem::Sender::new(kind, files, now)?),
    })
}

/// Receives into the folder `dir`. Xmodem has no file names, so its file is
/// `file_name` there.
pub fn receive(kind: Kind, dir: PathBuf, file_name: &str, now: Instant) -> Box<dyn Engine> {
    match kind {
        Kind::Zmodem => Box::new(zmodem::Receiver::new(dir, now)),
        _ => Box::new(xymodem::Receiver::new(kind, dir, file_name, now)),
    }
}

/// What a Zmodem start seen in the output asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZmodemStart {
    /// The other side runs `sz`: it wants to send us files.
    Download,
    /// The other side runs `rz`: it waits for files from us.
    Upload,
}

/// Looks for the start of a Zmodem transfer (`**` ZDLE `B00` from `sz`,
/// `B01` from `rz`) in terminal output.
pub fn detect_zmodem(data: &[u8]) -> Option<ZmodemStart> {
    find_zmodem(data).map(|(_, start)| start)
}

/// Like [`detect_zmodem`], with where the start is in `data` (the output
/// before it is still for the terminal).
pub fn find_zmodem(data: &[u8]) -> Option<(usize, ZmodemStart)> {
    let pattern = b"**\x18B0";
    data.windows(pattern.len() + 1)
        .enumerate()
        .find_map(|(i, w)| {
            if &w[..pattern.len()] != pattern {
                return None;
            }
            match w[pattern.len()] {
                b'0' => Some((i, ZmodemStart::Download)),
                b'1' => Some((i, ZmodemStart::Upload)),
                _ => None,
            }
        })
}

/// A name for a received file that does not overwrite one already there:
/// `name (2).ext`. Directory parts of `name` are dropped.
pub(crate) fn free_name(dir: &std::path::Path, name: &str) -> PathBuf {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let name = if name.is_empty() || name == "." || name == ".." {
        "received"
    } else {
        name
    };
    let mut path = dir.join(name);
    let mut n = 2;
    while path.exists() {
        let p = std::path::Path::new(name);
        let stem = p.file_stem().unwrap_or_default().to_string_lossy();
        path = dir.join(match p.extension() {
            Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
            None => format!("{stem} ({n})"),
        });
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_zmodem_starts() {
        assert_eq!(
            detect_zmodem(b"rz\r**\x18B00000000000000\r\n"),
            Some(ZmodemStart::Download)
        );
        assert_eq!(
            detect_zmodem(b"**\x18B0100000023be50\r\n"),
            Some(ZmodemStart::Upload)
        );
        assert_eq!(detect_zmodem(b"** plain text **"), None);
    }

    #[test]
    fn received_names_never_overwrite_or_escape() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(free_name(dir.path(), "a.txt"), dir.path().join("a.txt"));
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        assert_eq!(free_name(dir.path(), "a.txt"), dir.path().join("a (2).txt"));
        assert_eq!(
            free_name(dir.path(), "../../etc/passwd"),
            dir.path().join("passwd")
        );
        assert_eq!(free_name(dir.path(), ".."), dir.path().join("received"));
    }
}
