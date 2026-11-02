//! Xmodem (128-byte blocks, CRC or checksum), Xmodem-1K and Ymodem
//! (batch, with file names and sizes).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::crc::{checksum, crc16};
use super::{Engine, Kind, Progress, Status, free_name};

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const EOT: u8 = 0x04;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const CRC_START: u8 = b'C';
const SUB: u8 = 0x1A;

/// How long the other side may stay silent before we try again.
const TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the other side to start.
const START_TIMEOUT: Duration = Duration::from_secs(60);
const RETRIES: u32 = 10;

fn cancel_bytes() -> Vec<u8> {
    let mut v = vec![CAN; 8];
    v.extend([0x08; 8]);
    v
}

/// A block: header, sequence, data padded with SUB, CRC or checksum.
fn block(seq: u8, data: &[u8], size: usize, crc: bool) -> Vec<u8> {
    let mut payload = data.to_vec();
    payload.resize(size, if seq == 0 { 0 } else { SUB });
    let mut out = Vec::with_capacity(size + 5);
    out.push(if size == 1024 { STX } else { SOH });
    out.push(seq);
    out.push(!seq);
    out.extend_from_slice(&payload);
    if crc {
        out.extend(crc16(&payload).to_be_bytes());
    } else {
        out.push(checksum(&payload));
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SState {
    /// Waiting for `C` or NAK.
    Start,
    /// Ymodem: block 0 sent, waiting for its ACK.
    Header,
    /// Ymodem: header acknowledged, waiting for `C` before the data.
    DataStart,
    /// A data block of this many bytes sent, waiting for ACK.
    Data(usize),
    /// EOT sent.
    Eot,
    /// Ymodem: waiting for `C` to send the next header (or the empty one).
    Next,
    /// Ymodem: the empty header sent.
    End,
    Done,
}

pub struct Sender {
    kind: Kind,
    files: Vec<PathBuf>,
    index: usize,
    data: Vec<u8>,
    name: String,
    state: SState,
    crc: bool,
    seq: u8,
    offset: usize,
    /// The last thing sent, to send again on NAK or timeout.
    last: Vec<u8>,
    retries: u32,
    deadline: Instant,
    out: Vec<u8>,
    status: Status,
    sent: Vec<PathBuf>,
    cans: u8,
}

impl Sender {
    pub fn new(kind: Kind, mut files: Vec<PathBuf>, now: Instant) -> Result<Self, String> {
        if !kind.sends_names() {
            files.truncate(1);
        }
        let mut s = Self {
            kind,
            files,
            index: 0,
            data: Vec::new(),
            name: String::new(),
            state: SState::Start,
            crc: true,
            seq: 1,
            offset: 0,
            last: Vec::new(),
            retries: 0,
            deadline: now + START_TIMEOUT,
            out: Vec::new(),
            status: Status::Running,
            sent: Vec::new(),
            cans: 0,
        };
        s.load()?;
        Ok(s)
    }

    fn load(&mut self) -> Result<(), String> {
        let path = &self.files[self.index];
        self.data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.offset = 0;
        self.seq = 1;
        Ok(())
    }

    fn block_size(&self) -> usize {
        match self.kind {
            Kind::Xmodem => 128,
            _ => 1024,
        }
    }

    fn send(&mut self, bytes: Vec<u8>, state: SState, now: Instant) {
        self.out.extend_from_slice(&bytes);
        self.last = bytes;
        self.state = state;
        self.deadline = now + TIMEOUT;
    }

    fn header(&self, empty: bool) -> Vec<u8> {
        let mut info = Vec::new();
        if !empty {
            let mtime = std::fs::metadata(&self.files[self.index])
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            info.extend(self.name.as_bytes());
            info.push(0);
            info.extend(format!("{} {:o} 0", self.data.len(), mtime).as_bytes());
            info.push(0);
        }
        let size = if info.len() <= 128 { 128 } else { 1024 };
        block(0, &info, size, true)
    }

    fn next_data(&mut self, now: Instant) {
        if self.offset >= self.data.len() {
            self.send(vec![EOT], SState::Eot, now);
            return;
        }
        let size = self.block_size();
        let end = (self.offset + size).min(self.data.len());
        // A short last block goes as 128 bytes when that holds it.
        let size = if size == 1024 && end - self.offset <= 128 && self.kind != Kind::Xmodem1k {
            128
        } else {
            size
        };
        let bytes = block(self.seq, &self.data[self.offset..end], size, self.crc);
        self.send(bytes, SState::Data(end - self.offset), now);
    }

    fn fail(&mut self, why: &str) {
        self.out.extend(cancel_bytes());
        self.status = Status::Failed(why.into());
        self.state = SState::Done;
    }

    fn file_done(&mut self) {
        self.sent.push(self.files[self.index].clone());
    }
}

impl Engine for Sender {
    fn input(&mut self, data: &[u8], now: Instant) {
        for &b in data {
            if self.state == SState::Done {
                return;
            }
            if b == CAN {
                self.cans += 1;
                if self.cans >= 2 {
                    self.status = Status::Cancelled;
                    self.state = SState::Done;
                }
                continue;
            }
            self.cans = 0;
            match (self.state, b) {
                (SState::Start, CRC_START | NAK) => {
                    self.crc = b == CRC_START;
                    self.retries = 0;
                    if self.kind == Kind::Ymodem {
                        let h = self.header(false);
                        self.send(h, SState::Header, now);
                    } else {
                        self.next_data(now);
                    }
                }
                (SState::Header, ACK) => {
                    self.state = SState::DataStart;
                    self.deadline = now + TIMEOUT;
                }
                (SState::DataStart, CRC_START) => self.next_data(now),
                (SState::Data(len), ACK) => {
                    self.offset += len;
                    self.seq = self.seq.wrapping_add(1);
                    self.retries = 0;
                    self.next_data(now);
                }
                (SState::Eot, ACK) => {
                    self.file_done();
                    if self.kind == Kind::Ymodem {
                        self.state = SState::Next;
                        self.deadline = now + TIMEOUT;
                    } else {
                        self.status = Status::Finished(self.sent.clone());
                        self.state = SState::Done;
                    }
                }
                (SState::Next, CRC_START) => {
                    if self.index + 1 < self.files.len() {
                        self.index += 1;
                        if let Err(e) = self.load() {
                            self.fail(&e);
                            return;
                        }
                        let h = self.header(false);
                        self.send(h, SState::Header, now);
                    } else {
                        let h = self.header(true);
                        self.send(h, SState::End, now);
                    }
                }
                (SState::End, ACK) => {
                    self.status = Status::Finished(self.sent.clone());
                    self.state = SState::Done;
                }
                (SState::Header | SState::Data(_) | SState::Eot | SState::End, NAK) => {
                    self.retries += 1;
                    if self.retries > RETRIES {
                        self.fail("too many errors");
                        return;
                    }
                    let again = self.last.clone();
                    self.out.extend_from_slice(&again);
                    self.deadline = now + TIMEOUT;
                }
                _ => {}
            }
        }
    }

    fn output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    fn tick(&mut self, now: Instant) {
        if self.state == SState::Done || now < self.deadline {
            return;
        }
        match self.state {
            SState::Start => self.fail("the other side did not start receiving"),
            SState::DataStart | SState::Next => self.fail("the other side stopped answering"),
            _ => {
                self.retries += 1;
                if self.retries > RETRIES {
                    self.fail("the other side stopped answering");
                } else {
                    let again = self.last.clone();
                    self.out.extend_from_slice(&again);
                    self.deadline = now + TIMEOUT;
                }
            }
        }
    }

    fn progress(&self) -> Progress {
        Progress {
            file: self.name.clone(),
            done: self.offset as u64,
            total: Some(self.data.len() as u64),
            files: self.sent.len(),
        }
    }

    fn status(&self) -> Status {
        self.status.clone()
    }

    fn cancel(&mut self) {
        if self.state != SState::Done {
            self.out.extend(cancel_bytes());
            self.status = Status::Cancelled;
            self.state = SState::Done;
        }
    }
}

pub struct Receiver {
    kind: Kind,
    dir: PathBuf,
    /// Xmodem has no names: the file is called this.
    xname: String,
    crc: bool,
    started: bool,
    starts: u32,
    buf: Vec<u8>,
    expected: u8,
    /// Ymodem: waiting for a header (block 0).
    header: bool,
    file: Option<BufWriter<File>>,
    path: Option<PathBuf>,
    name: String,
    written: u64,
    size: Option<u64>,
    /// Xmodem: the last block, written once we know it is not the end
    /// (its SUB padding is cut off at the end).
    held: Vec<u8>,
    files: Vec<PathBuf>,
    out: Vec<u8>,
    deadline: Instant,
    retries: u32,
    status: Status,
    cans: u8,
}

impl Receiver {
    pub fn new(kind: Kind, dir: PathBuf, file_name: &str, now: Instant) -> Self {
        let mut r = Self {
            kind,
            dir,
            xname: file_name.to_string(),
            crc: true,
            started: false,
            starts: 0,
            buf: Vec::new(),
            expected: if kind == Kind::Ymodem { 0 } else { 1 },
            header: kind == Kind::Ymodem,
            file: None,
            path: None,
            name: String::new(),
            written: 0,
            size: None,
            held: Vec::new(),
            files: Vec::new(),
            out: Vec::new(),
            deadline: now,
            retries: 0,
            status: Status::Running,
            cans: 0,
        };
        r.poke(now);
        r
    }

    /// Asks the sender to start (`C`, or NAK for checksum mode).
    fn poke(&mut self, now: Instant) {
        self.starts += 1;
        // Old Xmodem senders only know the checksum: switch after a while.
        if self.kind == Kind::Xmodem && !self.started && self.starts > 4 {
            self.crc = false;
        }
        self.out.push(if self.crc { CRC_START } else { NAK });
        self.deadline = now + Duration::from_secs(3);
    }

    fn fail(&mut self, why: &str) {
        self.out.extend(cancel_bytes());
        self.status = Status::Failed(why.into());
        self.close(false);
    }

    fn open(&mut self, name: &str) -> Result<(), String> {
        let path = free_name(&self.dir, name);
        let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.file = Some(BufWriter::new(file));
        self.path = Some(path);
        self.written = 0;
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let data = match self.size {
            Some(size) => &data[..data.len().min(size.saturating_sub(self.written) as usize)],
            None => data,
        };
        if let Some(f) = &mut self.file {
            f.write_all(data).map_err(|e| e.to_string())?;
        }
        self.written += data.len() as u64;
        Ok(())
    }

    /// Ends the current file; `keep` false deletes a partial one.
    fn close(&mut self, keep: bool) {
        if let Some(mut f) = self.file.take() {
            let _ = f.flush();
        }
        if let Some(path) = self.path.take() {
            if keep {
                self.files.push(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    fn handle_block(&mut self, now: Instant) {
        let size = if self.buf[0] == STX { 1024 } else { 128 };
        let seq = self.buf[1];
        let ok_seq = self.buf[2] == !seq;
        let payload = self.buf[3..3 + size].to_vec();
        let ok_sum = if self.crc {
            u16::from_be_bytes([self.buf[3 + size], self.buf[4 + size]]) == crc16(&payload)
        } else {
            self.buf[3 + size] == checksum(&payload)
        };
        self.buf.clear();
        self.deadline = now + TIMEOUT;
        if !ok_seq || !ok_sum {
            self.retries += 1;
            if self.retries > RETRIES {
                self.fail("too many errors");
            } else {
                self.out.push(NAK);
            }
            return;
        }
        self.retries = 0;
        if seq == self.expected.wrapping_sub(1) && !(self.header && seq == 0) {
            // The sender missed our ACK.
            self.out.push(ACK);
            return;
        }
        if seq != self.expected {
            self.fail("blocks out of order");
            return;
        }
        if self.header {
            // Ymodem block 0: "name\0size ...\0", or empty at the end.
            let end = payload.iter().position(|&b| b == 0).unwrap_or(0);
            if end == 0 {
                self.out.push(ACK);
                self.status = Status::Finished(self.files.clone());
                return;
            }
            let name = String::from_utf8_lossy(&payload[..end]).into_owned();
            let rest = &payload[end + 1..];
            let info_end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            self.size = String::from_utf8_lossy(&rest[..info_end])
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok());
            if let Err(e) = self.open(&name) {
                self.fail(&e);
                return;
            }
            self.header = false;
            self.expected = 1;
            self.out.push(ACK);
            self.out.push(CRC_START);
            return;
        }
        if self.file.is_none() {
            let name = self.xname.clone();
            if let Err(e) = self.open(&name) {
                self.fail(&e);
                return;
            }
        }
        let result = if self.kind == Kind::Ymodem {
            self.write(&payload)
        } else {
            let previous = std::mem::replace(&mut self.held, payload);
            self.write(&previous)
        };
        if let Err(e) = result {
            self.fail(&e);
            return;
        }
        self.expected = self.expected.wrapping_add(1);
        self.out.push(ACK);
    }

    fn handle_eot(&mut self, now: Instant) {
        self.deadline = now + TIMEOUT;
        if self.kind != Kind::Ymodem {
            // The SUB padding of the last block is not part of the file.
            let mut last = std::mem::take(&mut self.held);
            while last.last() == Some(&SUB) {
                last.pop();
            }
            if let Err(e) = self.write(&last) {
                self.fail(&e);
                return;
            }
            self.out.push(ACK);
            self.close(true);
            self.status = Status::Finished(self.files.clone());
            return;
        }
        self.out.push(ACK);
        self.close(true);
        self.header = true;
        self.expected = 0;
        self.size = None;
        self.out.push(CRC_START);
    }
}

impl Engine for Receiver {
    fn input(&mut self, data: &[u8], now: Instant) {
        for &b in data {
            if self.status != Status::Running {
                return;
            }
            if self.buf.is_empty() {
                match b {
                    SOH | STX => {
                        self.started = true;
                        self.cans = 0;
                        self.buf.push(b);
                    }
                    EOT if self.started => {
                        self.cans = 0;
                        self.handle_eot(now);
                    }
                    CAN => {
                        self.cans += 1;
                        if self.cans >= 2 {
                            self.status = Status::Cancelled;
                            self.close(false);
                        }
                    }
                    // Anything else is terminal noise before the start.
                    _ => self.cans = 0,
                }
                continue;
            }
            self.buf.push(b);
            let size = if self.buf[0] == STX { 1024 } else { 128 };
            let total = 3 + size + if self.crc { 2 } else { 1 };
            if self.buf.len() == total {
                self.handle_block(now);
            }
        }
    }

    fn output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    fn tick(&mut self, now: Instant) {
        if self.status != Status::Running || now < self.deadline {
            return;
        }
        if !self.started {
            if self.starts >= 20 {
                self.fail("the other side did not start sending");
            } else {
                self.poke(now);
            }
            return;
        }
        self.retries += 1;
        if self.retries > RETRIES {
            self.fail("the other side stopped sending");
        } else {
            self.buf.clear();
            self.out.push(if self.header { CRC_START } else { NAK });
            self.deadline = now + TIMEOUT;
        }
    }

    fn progress(&self) -> Progress {
        Progress {
            file: self.name.clone(),
            done: self.written,
            total: self.size,
            files: self.files.len(),
        }
    }

    fn status(&self) -> Status {
        self.status.clone()
    }

    fn cancel(&mut self) {
        if self.status == Status::Running {
            self.out.extend(cancel_bytes());
            self.status = Status::Cancelled;
            self.close(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a sender against a receiver, passing bytes both ways.
    fn pair(kind: Kind, files: Vec<PathBuf>, dir: PathBuf) -> (Status, Status) {
        let now = Instant::now();
        let mut s = Sender::new(kind, files, now).unwrap();
        let mut r = Receiver::new(kind, dir, "x.bin", now);
        for _ in 0..100_000 {
            let to_s = r.output();
            s.input(&to_s, now);
            let to_r = s.output();
            r.input(&to_r, now);
            if s.status() != Status::Running && r.status() != Status::Running {
                break;
            }
        }
        (s.status(), r.status())
    }

    #[test]
    fn xmodem_and_ymodem_round_trip() {
        let src = tempfile::tempdir().unwrap();
        let a = src.path().join("a.bin");
        let b = src.path().join("b.txt");
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&a, &data).unwrap();
        std::fs::write(&b, b"hello").unwrap();
        for kind in [Kind::Xmodem, Kind::Xmodem1k] {
            let dst = tempfile::tempdir().unwrap();
            let (s, r) = pair(kind, vec![a.clone()], dst.path().into());
            assert!(matches!(s, Status::Finished(_)), "{kind:?} {s:?}");
            assert!(matches!(r, Status::Finished(_)), "{kind:?} {r:?}");
            assert_eq!(
                std::fs::read(dst.path().join("x.bin")).unwrap(),
                data,
                "{kind:?}"
            );
        }
        let dst = tempfile::tempdir().unwrap();
        let (s, r) = pair(Kind::Ymodem, vec![a.clone(), b.clone()], dst.path().into());
        assert_eq!(s, Status::Finished(vec![a, b]));
        assert!(
            matches!(r, Status::Finished(ref f) if f.len() == 2),
            "{r:?}"
        );
        assert_eq!(std::fs::read(dst.path().join("a.bin")).unwrap(), data);
        assert_eq!(std::fs::read(dst.path().join("b.txt")).unwrap(), b"hello");
    }

    #[test]
    fn noise_before_the_start_is_ignored_and_cancel_is_heard() {
        let now = Instant::now();
        let dst = tempfile::tempdir().unwrap();
        let mut r = Receiver::new(Kind::Xmodem, dst.path().into(), "x", now);
        assert_eq!(r.output(), [CRC_START]);
        r.input(b"rx: ready to receive x\r\n", now);
        assert_eq!(r.status(), Status::Running);
        r.input(&[CAN, CAN], now);
        assert_eq!(r.status(), Status::Cancelled);
        assert!(!dst.path().join("x").exists());
    }

    #[test]
    fn a_silent_receiver_times_out() {
        let now = Instant::now();
        let src = tempfile::tempdir().unwrap();
        let a = src.path().join("a");
        std::fs::write(&a, b"x").unwrap();
        let mut s = Sender::new(Kind::Xmodem, vec![a], now).unwrap();
        s.tick(now + Duration::from_secs(61));
        assert!(matches!(s.status(), Status::Failed(_)));
        assert!(s.output().starts_with(&[CAN, CAN]));
    }
}
