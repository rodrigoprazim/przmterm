//! Zmodem, sending and receiving, compatible with lrzsz (`sz` / `rz`).
//!
//! Frames are hex headers (`**` ZDLE `B` …), binary headers with a CRC-16
//! (ZDLE `A`) or CRC-32 (ZDLE `C`), and data subpackets ended by ZDLE and a
//! frame-end byte. Control bytes are escaped with ZDLE.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::crc::{crc16, crc16_update, crc32_update};
use super::{Engine, Progress, Status, free_name};

const ZPAD: u8 = b'*';
const ZDLE: u8 = 0x18;
const ZBIN: u8 = b'A';
const ZHEX: u8 = b'B';
const ZBIN32: u8 = b'C';

const ZRQINIT: u8 = 0;
const ZRINIT: u8 = 1;
const ZSINIT: u8 = 2;
const ZACK: u8 = 3;
const ZFILE: u8 = 4;
const ZSKIP: u8 = 5;
const ZNAK: u8 = 6;
const ZABORT: u8 = 7;
const ZFIN: u8 = 8;
const ZRPOS: u8 = 9;
const ZDATA: u8 = 10;
const ZEOF: u8 = 11;
const ZFERR: u8 = 12;
const ZCAN: u8 = 16;
const ZCOMMAND: u8 = 18;

const ZCRCE: u8 = b'h';
const ZCRCG: u8 = b'i';
const ZCRCQ: u8 = b'j';
const ZCRCW: u8 = b'k';
const ZRUB0: u8 = b'l';
const ZRUB1: u8 = b'm';

const CANFDX: u8 = 0x01;
const CANOVIO: u8 = 0x02;
const CANFC32: u8 = 0x20;

const XON: u8 = 0x11;
const XOFF: u8 = 0x13;

const SUBPACKET: usize = 1024;
/// Data a sender puts out per tick while streaming.
const BURST: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const RETRIES: u32 = 10;

fn cancel_bytes() -> Vec<u8> {
    let mut v = vec![ZDLE; 8];
    v.extend([0x08; 8]);
    v
}

fn escape(b: u8, out: &mut Vec<u8>) {
    match b {
        ZDLE | 0x10 | XON | XOFF | 0x90 | 0x91 | 0x93 => {
            out.push(ZDLE);
            out.push(b ^ 0x40);
        }
        _ => out.push(b),
    }
}

fn hex_header(kind: u8, data: [u8; 4]) -> Vec<u8> {
    let mut raw = vec![kind];
    raw.extend(data);
    let crc = crc16(&raw);
    let mut out = vec![ZPAD, ZPAD, ZDLE, ZHEX];
    for b in raw.iter().copied().chain(crc.to_be_bytes()) {
        out.extend(format!("{b:02x}").as_bytes());
    }
    out.extend(b"\r\n");
    if kind != ZFIN && kind != ZACK {
        out.push(XON);
    }
    out
}

fn bin_header(kind: u8, data: [u8; 4], crc32: bool) -> Vec<u8> {
    let mut raw = vec![kind];
    raw.extend(data);
    let mut out = vec![ZPAD, ZDLE, if crc32 { ZBIN32 } else { ZBIN }];
    for &b in &raw {
        escape(b, &mut out);
    }
    if crc32 {
        let crc = !crc32_update(0xFFFF_FFFF, &raw);
        for b in crc.to_le_bytes() {
            escape(b, &mut out);
        }
    } else {
        for b in crc16(&raw).to_be_bytes() {
            escape(b, &mut out);
        }
    }
    out
}

fn subpacket(data: &[u8], end: u8, crc32: bool, out: &mut Vec<u8>) {
    for &b in data {
        escape(b, out);
    }
    out.push(ZDLE);
    out.push(end);
    if crc32 {
        let crc = !crc32_update(crc32_update(0xFFFF_FFFF, data), &[end]);
        for b in crc.to_le_bytes() {
            escape(b, out);
        }
    } else {
        let crc = crc16_update(crc16_update(0, data), &[end]);
        for b in crc.to_be_bytes() {
            escape(b, out);
        }
    }
}

fn pos_bytes(pos: u64) -> [u8; 4] {
    (pos as u32).to_le_bytes()
}

fn pos_of(data: [u8; 4]) -> u64 {
    u64::from(u32::from_le_bytes(data))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Frame {
    Header {
        kind: u8,
        data: [u8; 4],
        crc32: bool,
    },
    Data {
        data: Vec<u8>,
        end: u8,
    },
    /// A header or subpacket with a wrong CRC.
    Bad,
    /// The other side cancelled (five ZDLE/CAN in a row).
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DState {
    Idle,
    Pad,
    Dle,
    Hex,
    Bin {
        crc32: bool,
    },
    Data {
        crc32: bool,
    },
    /// The CRC after a subpacket's end byte.
    DataCrc {
        crc32: bool,
        end: u8,
    },
}

/// Turns received bytes into frames.
struct Decoder {
    state: DState,
    buf: Vec<u8>,
    data: Vec<u8>,
    escaped: bool,
    cans: u8,
}

impl Decoder {
    fn new() -> Self {
        Self {
            state: DState::Idle,
            buf: Vec::new(),
            data: Vec::new(),
            escaped: false,
            cans: 0,
        }
    }

    /// After a header followed by data (ZFILE, ZDATA…): read subpackets.
    fn expect_data(&mut self, crc32: bool) {
        self.state = DState::Data { crc32 };
        self.data.clear();
        self.buf.clear();
        self.escaped = false;
    }

    fn idle(&mut self) {
        self.state = DState::Idle;
        self.buf.clear();
        self.escaped = false;
    }

    fn feed(&mut self, input: &[u8], frames: &mut Vec<Frame>) {
        for &b in input {
            if b == ZDLE {
                self.cans += 1;
                if self.cans >= 5 {
                    self.cans = 0;
                    self.idle();
                    frames.push(Frame::Cancel);
                    continue;
                }
            } else {
                self.cans = 0;
            }
            match self.state {
                DState::Idle => {
                    if b == ZPAD {
                        self.state = DState::Pad;
                    }
                }
                DState::Pad => {
                    self.state = match b {
                        ZPAD => DState::Pad,
                        ZDLE => DState::Dle,
                        _ => DState::Idle,
                    }
                }
                DState::Dle => {
                    self.buf.clear();
                    self.escaped = false;
                    self.state = match b {
                        ZHEX => DState::Hex,
                        ZBIN => DState::Bin { crc32: false },
                        ZBIN32 => DState::Bin { crc32: true },
                        _ => DState::Idle,
                    }
                }
                DState::Hex => {
                    if b.is_ascii_hexdigit() {
                        self.buf.push(b);
                        if self.buf.len() == 14 {
                            let text = String::from_utf8_lossy(&self.buf).into_owned();
                            let bytes: Vec<u8> = (0..7)
                                .filter_map(|i| {
                                    u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()
                                })
                                .collect();
                            self.idle();
                            if bytes.len() == 7
                                && crc16(&bytes[..5]) == u16::from_be_bytes([bytes[5], bytes[6]])
                            {
                                self.header(
                                    bytes[0],
                                    [bytes[1], bytes[2], bytes[3], bytes[4]],
                                    false,
                                    frames,
                                );
                            } else {
                                frames.push(Frame::Bad);
                            }
                        }
                    } else {
                        self.idle();
                    }
                }
                DState::Bin { crc32 } => {
                    let Some(v) = self.unescape(b) else { continue };
                    self.buf.push(v);
                    let need = if crc32 { 9 } else { 7 };
                    if self.buf.len() == need {
                        let raw = self.buf[..5].to_vec();
                        let ok = if crc32 {
                            let crc = u32::from_le_bytes([
                                self.buf[5],
                                self.buf[6],
                                self.buf[7],
                                self.buf[8],
                            ]);
                            !crc32_update(0xFFFF_FFFF, &raw) == crc
                        } else {
                            crc16(&raw) == u16::from_be_bytes([self.buf[5], self.buf[6]])
                        };
                        self.idle();
                        if ok {
                            self.header(raw[0], [raw[1], raw[2], raw[3], raw[4]], crc32, frames);
                        } else {
                            frames.push(Frame::Bad);
                        }
                    }
                }
                DState::Data { crc32 } => {
                    // Flow-control bytes are never data unescaped.
                    if !self.escaped && matches!(b, XON | XOFF | 0x91 | 0x93) {
                        continue;
                    }
                    if self.escaped {
                        self.escaped = false;
                        match b {
                            ZCRCE | ZCRCG | ZCRCQ | ZCRCW => {
                                self.state = DState::DataCrc { crc32, end: b };
                                self.buf.clear();
                            }
                            ZRUB0 => self.data.push(0x7f),
                            ZRUB1 => self.data.push(0xff),
                            _ => self.data.push(b ^ 0x40),
                        }
                    } else if b == ZDLE {
                        self.escaped = true;
                    } else {
                        self.data.push(b);
                    }
                    if self.data.len() > 16 * 1024 {
                        self.idle();
                        frames.push(Frame::Bad);
                    }
                }
                DState::DataCrc { crc32, end } => {
                    let Some(v) = self.unescape(b) else { continue };
                    self.buf.push(v);
                    let need = if crc32 { 4 } else { 2 };
                    if self.buf.len() == need {
                        let ok = if crc32 {
                            let crc = u32::from_le_bytes([
                                self.buf[0],
                                self.buf[1],
                                self.buf[2],
                                self.buf[3],
                            ]);
                            !crc32_update(crc32_update(0xFFFF_FFFF, &self.data), &[end]) == crc
                        } else {
                            crc16_update(crc16_update(0, &self.data), &[end])
                                == u16::from_be_bytes([self.buf[0], self.buf[1]])
                        };
                        let data = std::mem::take(&mut self.data);
                        self.buf.clear();
                        if !ok {
                            self.idle();
                            frames.push(Frame::Bad);
                            continue;
                        }
                        frames.push(Frame::Data { data, end });
                        // More subpackets follow ZCRCG and ZCRCQ.
                        self.state = if matches!(end, ZCRCG | ZCRCQ) {
                            DState::Data { crc32 }
                        } else {
                            DState::Idle
                        };
                    }
                }
            }
        }
    }

    /// A header arrived. Data subpackets follow some kinds at once, so the
    /// decoder switches to reading them before anything else is decoded.
    fn header(&mut self, kind: u8, data: [u8; 4], crc32: bool, frames: &mut Vec<Frame>) {
        frames.push(Frame::Header { kind, data, crc32 });
        if matches!(kind, ZFILE | ZDATA | ZSINIT | ZCOMMAND) {
            self.expect_data(crc32);
        }
    }

    /// One byte of a ZDLE-escaped stream; `None` while an escape is open.
    fn unescape(&mut self, b: u8) -> Option<u8> {
        if self.escaped {
            self.escaped = false;
            return Some(match b {
                ZRUB0 => 0x7f,
                ZRUB1 => 0xff,
                _ => b ^ 0x40,
            });
        }
        if b == ZDLE {
            self.escaped = true;
            return None;
        }
        Some(b)
    }
}

fn zrinit() -> Vec<u8> {
    hex_header(ZRINIT, [0, 0, 0, CANFDX | CANOVIO | CANFC32])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RState {
    /// Waiting for a file (or the end).
    Waiting,
    /// ZFILE header seen; its subpacket comes next.
    FileInfo,
    /// ZSINIT header seen; its subpacket comes next.
    Sinit,
    /// Receiving a file's data.
    Data,
    /// A bad subpacket: everything up to the next ZDATA is dropped.
    Resync,
    Done,
}

pub struct Receiver {
    dir: PathBuf,
    dec: Decoder,
    state: RState,
    file: Option<BufWriter<File>>,
    path: Option<PathBuf>,
    name: String,
    written: u64,
    size: Option<u64>,
    files: Vec<PathBuf>,
    out: Vec<u8>,
    deadline: Instant,
    retries: u32,
    status: Status,
}

impl Receiver {
    pub fn new(dir: PathBuf, now: Instant) -> Self {
        Self {
            dir,
            dec: Decoder::new(),
            state: RState::Waiting,
            file: None,
            path: None,
            name: String::new(),
            written: 0,
            size: None,
            files: Vec::new(),
            out: zrinit(),
            deadline: now + TIMEOUT,
            retries: 0,
            status: Status::Running,
        }
    }

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

    fn fail(&mut self, why: &str) {
        self.out.extend(cancel_bytes());
        self.close(false);
        self.status = Status::Failed(why.into());
        self.state = RState::Done;
    }

    fn rpos(&mut self) {
        let pos = self.written;
        self.out.extend(hex_header(ZRPOS, pos_bytes(pos)));
    }

    fn frame(&mut self, frame: Frame, now: Instant) {
        self.deadline = now + TIMEOUT;
        match frame {
            Frame::Cancel => {
                self.close(false);
                self.status = Status::Cancelled;
                self.state = RState::Done;
            }
            Frame::Bad => match self.state {
                RState::Data | RState::Resync => {
                    self.state = RState::Resync;
                    self.retries += 1;
                    if self.retries > RETRIES {
                        self.fail("too many errors");
                    } else {
                        self.rpos();
                    }
                }
                RState::FileInfo => {
                    self.state = RState::Waiting;
                    self.out.extend(hex_header(ZNAK, [0; 4]));
                }
                _ => self.out.extend(hex_header(ZNAK, [0; 4])),
            },
            Frame::Header { kind, data, .. } => match kind {
                ZRQINIT => self.out.extend(zrinit()),
                ZSINIT => self.state = RState::Sinit,
                ZFILE => self.state = RState::FileInfo,
                ZDATA => {
                    if self.file.is_none() {
                        return;
                    }
                    if pos_of(data) != self.written {
                        // Ask again from where we are.
                        self.state = RState::Resync;
                        self.rpos();
                        return;
                    }
                    self.state = RState::Data;
                }
                ZEOF => {
                    if self.file.is_some() && pos_of(data) == self.written {
                        self.close(true);
                        self.state = RState::Waiting;
                        self.out.extend(zrinit());
                    }
                }
                ZFIN => {
                    self.close(true);
                    self.out.extend(hex_header(ZFIN, [0; 4]));
                    self.status = Status::Finished(self.files.clone());
                    self.state = RState::Done;
                }
                ZCOMMAND => {
                    // Never run commands from the other side.
                    self.out.extend(hex_header(ZCOMPL_REFUSED, [0; 4]));
                }
                ZABORT | ZFERR | ZCAN => {
                    self.close(false);
                    self.status = Status::Cancelled;
                    self.state = RState::Done;
                }
                _ => {}
            },
            Frame::Data { data, end } => match self.state {
                RState::Sinit => {
                    self.state = RState::Waiting;
                    self.out.extend(hex_header(ZACK, [0; 4]));
                }
                RState::FileInfo => {
                    let name_end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
                    let name = String::from_utf8_lossy(&data[..name_end]).into_owned();
                    let info = data.get(name_end + 1..).unwrap_or(&[]);
                    let info_end = info.iter().position(|&b| b == 0).unwrap_or(info.len());
                    self.size = String::from_utf8_lossy(&info[..info_end])
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok());
                    let path = free_name(&self.dir, &name);
                    match File::create(&path) {
                        Ok(f) => {
                            self.name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            self.file = Some(BufWriter::new(f));
                            self.path = Some(path);
                            self.written = 0;
                            self.state = RState::Waiting;
                            self.rpos();
                        }
                        Err(e) => {
                            // Tell the sender to skip this one.
                            self.state = RState::Waiting;
                            self.out.extend(hex_header(ZSKIP, [0; 4]));
                            self.status = Status::Failed(format!("{}: {e}", path.display()));
                        }
                    }
                }
                RState::Data => {
                    if let Some(f) = &mut self.file
                        && let Err(e) = f.write_all(&data)
                    {
                        let e = e.to_string();
                        self.fail(&e);
                        return;
                    }
                    self.written += data.len() as u64;
                    self.retries = 0;
                    if matches!(end, ZCRCQ | ZCRCW) {
                        let pos = self.written;
                        self.out.extend(hex_header(ZACK, pos_bytes(pos)));
                    }
                    if end == ZCRCE || end == ZCRCW {
                        // The next header (ZDATA or ZEOF) follows.
                        self.state = RState::Data;
                    }
                }
                _ => {}
            },
        }
    }
}

/// Answer to a ZCOMMAND: completed, with an error status.
const ZCOMPL_REFUSED: u8 = 15;

impl Engine for Receiver {
    fn input(&mut self, data: &[u8], now: Instant) {
        if self.state == RState::Done {
            return;
        }
        let mut frames = Vec::new();
        self.dec.feed(data, &mut frames);
        for f in frames {
            if self.state == RState::Done {
                break;
            }
            self.frame(f, now);
        }
    }

    fn output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    fn tick(&mut self, now: Instant) {
        if self.state == RState::Done || now < self.deadline {
            return;
        }
        self.retries += 1;
        if self.retries > RETRIES {
            self.fail("the other side stopped sending");
            return;
        }
        self.deadline = now + TIMEOUT;
        if self.file.is_some() {
            self.rpos();
        } else {
            self.out.extend(zrinit());
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
        if self.state != RState::Done {
            self.out.extend(cancel_bytes());
            self.close(false);
            self.status = Status::Cancelled;
            self.state = RState::Done;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SState {
    /// ZRQINIT sent; waiting for ZRINIT.
    Init,
    /// ZFILE sent; waiting for ZRPOS (or ZSKIP).
    FileSent,
    /// Sending the file's data.
    Streaming,
    /// ZEOF sent; waiting for ZRINIT.
    EofSent,
    /// ZFIN sent; waiting for ZFIN.
    FinSent,
    Done,
}

pub struct Sender {
    files: Vec<PathBuf>,
    index: usize,
    file: Option<File>,
    name: String,
    size: u64,
    offset: u64,
    crc32: bool,
    dec: Decoder,
    state: SState,
    out: Vec<u8>,
    deadline: Instant,
    retries: u32,
    status: Status,
    sent: Vec<PathBuf>,
}

impl Sender {
    pub fn new(files: Vec<PathBuf>, now: Instant) -> Result<Self, String> {
        for f in &files {
            std::fs::metadata(f).map_err(|e| format!("{}: {e}", f.display()))?;
        }
        Ok(Self {
            files,
            index: 0,
            file: None,
            name: String::new(),
            size: 0,
            offset: 0,
            crc32: false,
            dec: Decoder::new(),
            state: SState::Init,
            // `rz` may already be waiting; ZRQINIT makes it say so again.
            out: hex_header(ZRQINIT, [0; 4]),
            deadline: now + START_TIMEOUT,
            retries: 0,
            status: Status::Running,
            sent: Vec::new(),
        })
    }

    fn fail(&mut self, why: &str) {
        self.out.extend(cancel_bytes());
        self.status = Status::Failed(why.into());
        self.state = SState::Done;
    }

    /// Offers the file at `index`.
    fn offer(&mut self, now: Instant) -> Result<(), String> {
        let path = self.files[self.index].clone();
        let file = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        self.size = meta.len();
        self.offset = 0;
        self.name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        let mut info = self.name.as_bytes().to_vec();
        info.push(0);
        info.extend(format!("{} {:o} 0", self.size, mtime).as_bytes());
        info.push(0);
        self.file = Some(file);
        self.out.extend(bin_header(ZFILE, [0; 4], self.crc32));
        subpacket(&info, ZCRCW, self.crc32, &mut self.out);
        self.state = SState::FileSent;
        self.deadline = now + TIMEOUT;
        Ok(())
    }

    fn next_or_finish(&mut self, now: Instant) {
        if self.index < self.files.len() {
            if let Err(e) = self.offer(now) {
                self.fail(&e);
            }
        } else {
            self.out.extend(hex_header(ZFIN, [0; 4]));
            self.state = SState::FinSent;
            self.deadline = now + TIMEOUT;
        }
    }

    fn start_data(&mut self, pos: u64, now: Instant) {
        self.offset = pos.min(self.size);
        if let Some(f) = &mut self.file
            && f.seek(SeekFrom::Start(self.offset)).is_err()
        {
            self.fail("cannot read the file");
            return;
        }
        self.out
            .extend(bin_header(ZDATA, pos_bytes(self.offset), self.crc32));
        self.state = SState::Streaming;
        self.deadline = now + TIMEOUT;
    }

    /// Puts out up to [`BURST`] bytes of the file.
    fn stream(&mut self, now: Instant) {
        let mut buf = vec![0u8; SUBPACKET];
        let mut put = 0;
        while put < BURST && self.state == SState::Streaming {
            let n = match self.file.as_mut().map(|f| f.read(&mut buf)) {
                Some(Ok(n)) => n,
                _ => {
                    self.fail("cannot read the file");
                    return;
                }
            };
            self.offset += n as u64;
            put += n;
            if self.offset >= self.size || n == 0 {
                subpacket(&buf[..n], ZCRCE, self.crc32, &mut self.out);
                let size = self.size;
                self.out.extend(hex_header(ZEOF, pos_bytes(size)));
                self.state = SState::EofSent;
                self.deadline = now + TIMEOUT;
                return;
            }
            subpacket(&buf[..n], ZCRCG, self.crc32, &mut self.out);
        }
        self.deadline = now + TIMEOUT;
    }

    fn frame(&mut self, frame: Frame, now: Instant) {
        match frame {
            Frame::Cancel => {
                self.status = Status::Cancelled;
                self.state = SState::Done;
            }
            Frame::Bad | Frame::Data { .. } => {}
            Frame::Header { kind, data, .. } => match (self.state, kind) {
                (SState::Init, ZRINIT) => {
                    self.crc32 = data[3] & CANFC32 != 0;
                    self.next_or_finish(now);
                }
                (SState::EofSent, ZRINIT) => {
                    self.sent.push(self.files[self.index].clone());
                    self.index += 1;
                    self.file = None;
                    self.next_or_finish(now);
                }
                (SState::FileSent | SState::Streaming | SState::EofSent, ZRPOS) => {
                    self.retries += 1;
                    if self.retries > RETRIES * 2 {
                        self.fail("too many errors");
                        return;
                    }
                    self.start_data(pos_of(data), now);
                }
                (SState::FileSent, ZSKIP) => {
                    self.index += 1;
                    self.file = None;
                    self.next_or_finish(now);
                }
                (SState::FileSent, ZNAK) => {
                    // Offer the file again.
                    if let Err(e) = self.offer(now) {
                        self.fail(&e);
                    }
                }
                (SState::FinSent, ZFIN) => {
                    self.out.extend(b"OO");
                    self.status = Status::Finished(self.sent.clone());
                    self.state = SState::Done;
                }
                (_, ZABORT | ZFERR | ZCAN) => {
                    self.status = Status::Cancelled;
                    self.state = SState::Done;
                }
                _ => {}
            },
        }
    }
}

impl Engine for Sender {
    fn input(&mut self, data: &[u8], now: Instant) {
        if self.state == SState::Done {
            return;
        }
        let mut frames = Vec::new();
        self.dec.feed(data, &mut frames);
        for f in frames {
            if self.state == SState::Done {
                break;
            }
            self.frame(f, now);
        }
    }

    fn output(&mut self) -> Vec<u8> {
        if self.state == SState::Streaming && self.out.len() < BURST {
            self.stream(Instant::now());
        }
        std::mem::take(&mut self.out)
    }

    fn tick(&mut self, now: Instant) {
        if self.state == SState::Done || self.state == SState::Streaming || now < self.deadline {
            return;
        }
        self.retries += 1;
        if self.retries > RETRIES {
            self.fail("the other side stopped answering");
            return;
        }
        self.deadline = now + TIMEOUT;
        match self.state {
            SState::Init => {
                if now >= self.deadline {
                    self.fail("the other side did not start receiving (run rz there)");
                } else {
                    self.out.extend(hex_header(ZRQINIT, [0; 4]));
                }
            }
            SState::FileSent => {
                if let Err(e) = self.offer(now) {
                    self.fail(&e);
                }
            }
            SState::EofSent => {
                let size = self.size;
                self.out.extend(hex_header(ZEOF, pos_bytes(size)));
            }
            SState::FinSent => self.out.extend(hex_header(ZFIN, [0; 4])),
            _ => {}
        }
    }

    fn progress(&self) -> Progress {
        Progress {
            file: self.name.clone(),
            done: self.offset,
            total: Some(self.size),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_and_subpackets_decode_back() {
        let mut frames = Vec::new();
        let mut d = Decoder::new();
        d.feed(&hex_header(ZRINIT, [1, 2, 3, CANFC32]), &mut frames);
        d.feed(&bin_header(ZDATA, pos_bytes(0x1234), true), &mut frames);
        assert_eq!(
            frames,
            [
                Frame::Header {
                    kind: ZRINIT,
                    data: [1, 2, 3, CANFC32],
                    crc32: false
                },
                Frame::Header {
                    kind: ZDATA,
                    data: pos_bytes(0x1234),
                    crc32: true
                },
            ]
        );
        // Every byte value survives escaping, with both CRCs.
        let payload: Vec<u8> = (0..=255u8).collect();
        for crc32 in [false, true] {
            let mut out = Vec::new();
            subpacket(&payload, ZCRCG, crc32, &mut out);
            subpacket(b"end", ZCRCE, crc32, &mut out);
            let mut frames = Vec::new();
            let mut d = Decoder::new();
            d.expect_data(crc32);
            d.feed(&out, &mut frames);
            assert_eq!(
                frames,
                [
                    Frame::Data {
                        data: payload.clone(),
                        end: ZCRCG
                    },
                    Frame::Data {
                        data: b"end".to_vec(),
                        end: ZCRCE
                    },
                ]
            );
        }
        // A flipped bit is caught.
        let mut out = bin_header(ZRPOS, pos_bytes(7), false);
        out[4] ^= 1;
        let mut frames = Vec::new();
        Decoder::new().feed(&out, &mut frames);
        assert_eq!(frames, [Frame::Bad]);
    }

    fn pair(files: Vec<PathBuf>, dir: PathBuf) -> (Status, Status) {
        let now = Instant::now();
        let mut s = Sender::new(files, now).unwrap();
        let mut r = Receiver::new(dir, now);
        for _ in 0..100_000 {
            let to_r = s.output();
            r.input(&to_r, now);
            let to_s = r.output();
            s.input(&to_s, now);
            if s.status() != Status::Running && r.status() != Status::Running {
                break;
            }
        }
        (s.status(), r.status())
    }

    #[test]
    fn sends_several_files_to_itself() {
        let src = tempfile::tempdir().unwrap();
        let a = src.path().join("a.bin");
        let b = src.path().join("empty.txt");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 31 % 256) as u8).collect();
        std::fs::write(&a, &data).unwrap();
        std::fs::write(&b, b"").unwrap();
        let dst = tempfile::tempdir().unwrap();
        let (s, r) = pair(vec![a.clone(), b.clone()], dst.path().into());
        assert_eq!(s, Status::Finished(vec![a, b]));
        assert!(
            matches!(r, Status::Finished(ref f) if f.len() == 2),
            "{r:?}"
        );
        assert_eq!(std::fs::read(dst.path().join("a.bin")).unwrap(), data);
        assert_eq!(std::fs::read(dst.path().join("empty.txt")).unwrap(), b"");
    }

    #[test]
    fn five_cans_cancel() {
        let now = Instant::now();
        let dst = tempfile::tempdir().unwrap();
        let mut r = Receiver::new(dst.path().into(), now);
        r.input(&[ZDLE; 5], now);
        assert_eq!(r.status(), Status::Cancelled);
    }
}
