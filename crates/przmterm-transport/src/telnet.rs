//! Telnet transport (RFC 854) with option negotiation.
//!
//! [`Telnet`] is the protocol engine with no I/O: it turns received bytes
//! into terminal data plus replies, and keystrokes into wire bytes. The
//! transport runs it on a Tokio task over a TCP stream.
//!
//! Options PrzmTerm agrees to:
//!
//! | Option | Who | Why |
//! | --- | --- | --- |
//! | ECHO (1) | server | the server echoes what is typed (logins, CLIs) |
//! | SGA (3) | both | character-at-a-time instead of half-duplex |
//! | BINARY (0) | both | 8-bit clean, e.g. UTF-8 |
//! | TTYPE (24) | client | tells the server the terminal type |
//! | NAWS (31) | client | tells the server the window size, again on resize |
//!
//! Anything else is refused. Negotiation follows the "Q method" of RFC 1143
//! so that two sides that disagree never loop.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::pipe::{Cmd, Output, Pipe, TaskEnd, runtime};
use crate::{Ending, Protocol, Size, Transport, TransportError, Waker};

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const NOP: u8 = 241;
const BRK: u8 = 243;
const SE: u8 = 240;

const BINARY: u8 = 0;
const ECHO: u8 = 1;
const SGA: u8 = 3;
const TTYPE: u8 = 24;
const NAWS: u8 = 31;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

/// Longest subnegotiation kept; longer ones are cut (and ignored).
const MAX_SB: usize = 512;

/// How to reach a Telnet server.
#[derive(Debug, Clone)]
pub struct TelnetConfig {
    pub host: String,
    pub port: u16,
    /// Sent when the server asks (TTYPE).
    pub term_type: String,
    pub connect_timeout: Duration,
    /// Send a Telnet NOP this often so idle sessions are not dropped.
    pub keepalive: Option<Duration>,
}

impl TelnetConfig {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            term_type: "xterm-256color".into(),
            connect_timeout: Duration::from_secs(15),
            keepalive: Some(Duration::from_secs(30)),
        }
    }
}

/// One side's state of an option (RFC 1143).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Q {
    #[default]
    No,
    Yes,
    /// We asked to enable it and wait for the answer.
    WantYes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    /// After a CR in non-binary mode: a NUL that follows is dropped.
    Cr,
    Iac,
    Verb(u8),
    Sb,
    SbIac,
}

/// The Telnet protocol engine, without I/O.
#[derive(Debug)]
pub struct Telnet {
    term_type: String,
    size: Size,
    state: State,
    sb: Vec<u8>,
    /// Options we perform (WILL/WONT), indexed by option code.
    us: [Q; 256],
    /// Options the server performs (DO/DONT).
    him: [Q; 256],
}

/// What [`Telnet::receive`] produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Received {
    /// Bytes for the terminal.
    pub data: Vec<u8>,
    /// Bytes to send back to the server.
    pub reply: Vec<u8>,
}

impl Telnet {
    pub fn new(term_type: impl Into<String>, size: Size) -> Self {
        Self {
            term_type: term_type.into(),
            size,
            state: State::Data,
            sb: Vec::new(),
            us: [Q::No; 256],
            him: [Q::No; 256],
        }
    }

    /// What PrzmTerm offers as soon as the connection is up, like most
    /// clients: window size and terminal type, and asks the server to echo
    /// and to suppress go-ahead.
    pub fn start(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for opt in [NAWS, TTYPE, SGA] {
            self.us[opt as usize] = Q::WantYes;
            out.extend([IAC, WILL, opt]);
        }
        for opt in [SGA, ECHO] {
            self.him[opt as usize] = Q::WantYes;
            out.extend([IAC, DO, opt]);
        }
        out
    }

    /// True when the server echoes what is typed.
    pub fn server_echoes(&self) -> bool {
        self.him[ECHO as usize] == Q::Yes
    }

    fn binary_in(&self) -> bool {
        self.him[BINARY as usize] == Q::Yes
    }

    fn binary_out(&self) -> bool {
        self.us[BINARY as usize] == Q::Yes
    }

    /// Splits received bytes into terminal data and negotiation replies.
    pub fn receive(&mut self, input: &[u8]) -> Received {
        let mut r = Received::default();
        for &b in input {
            self.state = match self.state {
                State::Data | State::Cr => {
                    let after_cr = self.state == State::Cr;
                    match b {
                        IAC => State::Iac,
                        0 if after_cr => State::Data,
                        b'\r' if !self.binary_in() => {
                            r.data.push(b);
                            State::Cr
                        }
                        _ => {
                            r.data.push(b);
                            State::Data
                        }
                    }
                }
                State::Iac => match b {
                    IAC => {
                        r.data.push(IAC);
                        State::Data
                    }
                    WILL | WONT | DO | DONT => State::Verb(b),
                    SB => {
                        self.sb.clear();
                        State::Sb
                    }
                    // NOP, GA, DM, AYT and the rest carry nothing for us.
                    _ => State::Data,
                },
                State::Verb(verb) => {
                    self.negotiate(verb, b, &mut r.reply);
                    State::Data
                }
                State::Sb => match b {
                    IAC => State::SbIac,
                    _ => {
                        if self.sb.len() < MAX_SB {
                            self.sb.push(b);
                        }
                        State::Sb
                    }
                },
                State::SbIac => match b {
                    SE => {
                        self.subnegotiation(&mut r.reply);
                        State::Data
                    }
                    IAC => {
                        if self.sb.len() < MAX_SB {
                            self.sb.push(IAC);
                        }
                        State::Sb
                    }
                    // Malformed: treat as the end of the subnegotiation.
                    _ => State::Data,
                },
            };
        }
        r
    }

    fn accepts_him(opt: u8) -> bool {
        matches!(opt, ECHO | SGA | BINARY)
    }

    fn accepts_us(opt: u8) -> bool {
        matches!(opt, NAWS | TTYPE | SGA | BINARY)
    }

    fn negotiate(&mut self, verb: u8, opt: u8, reply: &mut Vec<u8>) {
        let i = opt as usize;
        match verb {
            WILL => match self.him[i] {
                Q::No if Self::accepts_him(opt) => {
                    self.him[i] = Q::Yes;
                    reply.extend([IAC, DO, opt]);
                }
                Q::No => reply.extend([IAC, DONT, opt]),
                Q::WantYes => self.him[i] = Q::Yes,
                Q::Yes => {}
            },
            WONT => match self.him[i] {
                Q::Yes => {
                    self.him[i] = Q::No;
                    reply.extend([IAC, DONT, opt]);
                }
                Q::WantYes => self.him[i] = Q::No,
                Q::No => {}
            },
            DO => match self.us[i] {
                Q::No if Self::accepts_us(opt) => {
                    self.us[i] = Q::Yes;
                    reply.extend([IAC, WILL, opt]);
                    self.enabled(opt, reply);
                }
                Q::No => reply.extend([IAC, WONT, opt]),
                Q::WantYes => {
                    self.us[i] = Q::Yes;
                    self.enabled(opt, reply);
                }
                Q::Yes => {}
            },
            DONT => match self.us[i] {
                Q::Yes => {
                    self.us[i] = Q::No;
                    reply.extend([IAC, WONT, opt]);
                }
                Q::WantYes => self.us[i] = Q::No,
                Q::No => {}
            },
            _ => {}
        }
    }

    /// Called when an option of ours has just been enabled.
    fn enabled(&self, opt: u8, reply: &mut Vec<u8>) {
        if opt == NAWS {
            reply.extend(self.naws());
        }
    }

    fn subnegotiation(&mut self, reply: &mut Vec<u8>) {
        if self.sb.first() == Some(&TTYPE)
            && self.sb.get(1) == Some(&TTYPE_SEND)
            && self.us[TTYPE as usize] == Q::Yes
        {
            reply.extend([IAC, SB, TTYPE, TTYPE_IS]);
            push_escaped(reply, self.term_type.as_bytes());
            reply.extend([IAC, SE]);
        }
    }

    fn naws(&self) -> Vec<u8> {
        let mut out = vec![IAC, SB, NAWS];
        let [c1, c2] = self.size.cols.to_be_bytes();
        let [r1, r2] = self.size.rows.to_be_bytes();
        push_escaped(&mut out, &[c1, c2, r1, r2]);
        out.extend([IAC, SE]);
        out
    }

    /// Records a new window size; returns what to send if NAWS is on.
    pub fn resize(&mut self, size: Size) -> Vec<u8> {
        self.size = size;
        if self.us[NAWS as usize] == Q::Yes {
            self.naws()
        } else {
            Vec::new()
        }
    }

    /// Keystrokes to wire bytes: IAC doubled and, outside binary mode, a
    /// lone CR sent as CR NUL (RFC 854's carriage return).
    pub fn encode(&self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 4);
        for (i, &b) in data.iter().enumerate() {
            match b {
                IAC => out.extend([IAC, IAC]),
                b'\r' if !self.binary_out() && data.get(i + 1) != Some(&b'\n') => {
                    out.extend([b'\r', 0]);
                }
                _ => out.push(b),
            }
        }
        out
    }

    /// The Telnet BREAK command.
    pub fn brk() -> [u8; 2] {
        [IAC, BRK]
    }

    /// The Telnet no-operation command, used as a keepalive.
    pub fn nop() -> [u8; 2] {
        [IAC, NOP]
    }
}

fn push_escaped(out: &mut Vec<u8>, data: &[u8]) {
    for &b in data {
        if b == IAC {
            out.push(IAC);
        }
        out.push(b);
    }
}

/// A Telnet session seen from the GUI.
pub struct TelnetTransport {
    pipe: Pipe,
}

impl TelnetTransport {
    /// Starts connecting in the background and returns immediately.
    pub fn connect(config: TelnetConfig, size: Size, waker: Waker) -> Self {
        let (pipe, end) = Pipe::new(waker);
        runtime().spawn(session_task(config, size, end));
        Self { pipe }
    }
}

impl Transport for TelnetTransport {
    fn protocol(&self) -> Protocol {
        Protocol::Telnet
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        self.pipe.read(buf)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.pipe.send(Cmd::Data(data.to_vec()))
    }

    fn resize(&mut self, size: Size) -> Result<(), TransportError> {
        self.pipe.send(Cmd::Resize(size))
    }

    fn send_break(&mut self) -> Result<(), TransportError> {
        self.pipe.send(Cmd::Break)
    }

    fn close(&mut self) -> Result<(), TransportError> {
        let _ = self.pipe.send(Cmd::Close);
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.pipe.is_closed()
    }

    fn ending(&self) -> Option<Ending> {
        self.pipe.ending()
    }
}

async fn session_task(config: TelnetConfig, size: Size, end: TaskEnd) {
    let TaskEnd {
        mut cmds,
        out,
        closed,
    } = end;
    let host = format!("{}:{}", config.host, config.port);
    out.notice(&format!("Connecting to {host}…"));
    let connected = tokio::time::timeout(
        config.connect_timeout,
        TcpStream::connect((config.host.as_str(), config.port)),
    )
    .await;
    let ending = match connected {
        Err(_) => {
            out.error(&format!("Could not connect to {host}: timed out"));
            Ending::Unreachable
        }
        Ok(Err(e)) => {
            out.error(&format!("Could not connect to {host}: {e}"));
            Ending::Unreachable
        }
        Ok(Ok(stream)) => {
            let _ = stream.set_nodelay(true);
            match run(stream, &config, size, &mut cmds, &out).await {
                Ok(ending) => {
                    if ending == Ending::Clean {
                        out.notice(&format!("Connection to {} closed.", config.host));
                    }
                    ending
                }
                Err(e) => {
                    out.error(&format!("\r\nConnection to {} lost: {e}", config.host));
                    Ending::Lost
                }
            }
        }
    };
    crate::pipe::finish(&closed, &out, ending);
}

async fn run(
    mut stream: TcpStream,
    config: &TelnetConfig,
    size: Size,
    cmds: &mut tokio::sync::mpsc::UnboundedReceiver<Cmd>,
    out: &Output,
) -> std::io::Result<Ending> {
    let mut telnet = Telnet::new(config.term_type.clone(), size);
    stream.write_all(&telnet.start()).await?;
    let mut buf = vec![0u8; 16 * 1024];
    // Without a keepalive the timer never fires.
    let period = config
        .keepalive
        .unwrap_or(Duration::from_secs(86_400 * 365));
    let mut keepalive = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    loop {
        tokio::select! {
            _ = keepalive.tick(), if config.keepalive.is_some() => {
                stream.write_all(&Telnet::nop()).await?;
            }
            n = stream.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    return Ok(Ending::Clean);
                }
                let r = telnet.receive(&buf[..n]);
                out.bytes(&r.data);
                if !r.reply.is_empty() {
                    stream.write_all(&r.reply).await?;
                }
            }
            cmd = cmds.recv() => match cmd {
                Some(Cmd::Data(bytes)) => stream.write_all(&telnet.encode(&bytes)).await?,
                Some(Cmd::Resize(size)) => {
                    let msg = telnet.resize(size);
                    if !msg.is_empty() {
                        stream.write_all(&msg).await?;
                    }
                }
                Some(Cmd::Break) => stream.write_all(&Telnet::brk()).await?,
                Some(Cmd::Close) | None => {
                    let _ = stream.shutdown().await;
                    return Ok(Ending::Cancelled);
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Telnet {
        Telnet::new("xterm-256color", Size { cols: 80, rows: 24 })
    }

    #[test]
    fn offers_and_accepts_answers_without_looping() {
        let mut t = engine();
        let start = t.start();
        assert_eq!(
            start,
            [
                IAC, WILL, NAWS, IAC, WILL, TTYPE, IAC, WILL, SGA, IAC, DO, SGA, IAC, DO, ECHO
            ]
        );
        // The server agrees to everything we asked: no further replies,
        // except the window size once NAWS is on.
        let r = t.receive(&[
            IAC, WILL, ECHO, IAC, WILL, SGA, IAC, DO, SGA, IAC, DO, TTYPE,
        ]);
        assert!(r.reply.is_empty(), "{:?}", r.reply);
        assert!(t.server_echoes());
        let r = t.receive(&[IAC, DO, NAWS]);
        assert_eq!(r.reply, [IAC, SB, NAWS, 0, 80, 0, 24, IAC, SE]);
        // Repeating an agreed option changes nothing.
        assert!(
            t.receive(&[IAC, WILL, ECHO, IAC, DO, NAWS])
                .reply
                .is_empty()
        );
    }

    #[test]
    fn refuses_unknown_options() {
        let mut t = engine();
        let r = t.receive(&[IAC, DO, 39, IAC, WILL, 34]);
        assert_eq!(r.reply, [IAC, WONT, 39, IAC, DONT, 34]);
        assert!(r.data.is_empty());
    }

    #[test]
    fn answers_terminal_type_and_window_size() {
        let mut t = engine();
        let mut r = t.receive(&[IAC, DO, TTYPE, IAC, SB, TTYPE, TTYPE_SEND, IAC, SE]);
        let mut expected = vec![IAC, WILL, TTYPE, IAC, SB, TTYPE, TTYPE_IS];
        expected.extend(b"xterm-256color");
        expected.extend([IAC, SE]);
        assert_eq!(r.reply, expected);
        assert!(
            t.resize(Size {
                cols: 100,
                rows: 30
            })
            .is_empty(),
            "NAWS is off"
        );
        r = t.receive(&[IAC, DO, NAWS]);
        assert_eq!(
            r.reply,
            [IAC, WILL, NAWS, IAC, SB, NAWS, 0, 100, 0, 30, IAC, SE]
        );
        // 255 columns must be escaped inside the subnegotiation.
        assert_eq!(
            t.resize(Size {
                cols: 255,
                rows: 40
            }),
            [IAC, SB, NAWS, 0, IAC, IAC, 0, 40, IAC, SE]
        );
    }

    #[test]
    fn data_survives_chunk_boundaries_and_escapes() {
        let mut t = engine();
        let mut data = Vec::new();
        for chunk in [
            &b"login: \r"[..],
            &[0, IAC],
            &[IAC, b'x', IAC],
            &[WILL],
            &[ECHO, b'!'],
        ] {
            let r = t.receive(chunk);
            data.extend(r.data);
        }
        assert_eq!(data, b"login: \r\xffx!");
        assert!(t.server_echoes());
    }

    #[test]
    fn encodes_keystrokes() {
        let mut t = engine();
        assert_eq!(t.encode(b"ls\r"), b"ls\r\0");
        assert_eq!(t.encode(b"a\r\nb"), b"a\r\nb");
        assert_eq!(t.encode(&[IAC]), [IAC, IAC]);
        // In binary mode a CR goes out as it is.
        t.receive(&[IAC, DO, BINARY]);
        assert_eq!(t.encode(b"\r"), b"\r");
    }
}
