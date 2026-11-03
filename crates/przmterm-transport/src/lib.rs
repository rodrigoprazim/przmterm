//! Byte transports for PrzmTerm sessions.
//!
//! Every protocol (SSH, Telnet, serial, local process) implements the same
//! [`Transport`] contract: it moves raw bytes and knows nothing about terminal
//! emulation. The emulator in `przmterm-term` interprets what comes out.
//!
//! The contract is synchronous and non-blocking so the GUI can poll it every
//! frame. Network transports ([`ssh::SshTransport`]) run as a Tokio task and
//! call a [`Waker`] when bytes arrive, so the GUI repaints only when needed.

mod forward;
pub mod modem;
mod pipe;
pub mod scp;
mod scpfs;
pub mod serial;
pub mod sftp;
pub mod ssh;
pub mod telnet;
pub mod x11;

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;

/// Called from a network task when data arrives or the connection ends.
/// The GUI passes a closure that requests a repaint.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A waker that does nothing, for tests and headless use.
pub fn noop_waker() -> Waker {
    Arc::new(|| {})
}

/// Terminal size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Default for Size {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

pub use przmterm_core::Protocol;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("not connected")]
    NotConnected,
    #[error("connection closed by peer")]
    Closed,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Why a session ended, for deciding whether to reconnect on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The remote side closed the session normally (`exit`, logout).
    Clean,
    /// The user cancelled or closed it.
    Cancelled,
    /// The connection dropped after it was up (network, reboot, cable).
    Lost,
    /// Could not connect at all (refused, timed out, port missing).
    Unreachable,
    /// Refused for a reason retrying will not fix: login, host key.
    Failed,
}

/// A bidirectional byte stream behind a terminal session.
pub trait Transport: Send {
    fn protocol(&self) -> Protocol;

    /// Reads available bytes into `buf`. Returns 0 when nothing is pending.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError>;

    /// Writes all of `data` to the remote side.
    fn write(&mut self, data: &[u8]) -> Result<(), TransportError>;

    /// Informs the remote side that the terminal was resized (NAWS, window-change).
    fn resize(&mut self, size: Size) -> Result<(), TransportError>;

    /// Sends a serial BREAK or the protocol's equivalent. Default: unsupported no-op.
    fn send_break(&mut self) -> Result<(), TransportError> {
        Ok(())
    }

    fn close(&mut self) -> Result<(), TransportError>;

    /// True once the connection has ended and no more bytes will arrive.
    fn is_closed(&self) -> bool {
        false
    }

    /// Why the session ended, once it has (and the transport knows).
    fn ending(&self) -> Option<Ending> {
        None
    }

    /// Old, weak algorithms the connection uses (SSH), for a warning badge.
    fn weak_algorithms(&self) -> Vec<String> {
        Vec::new()
    }

    /// Another session over the same connection, without logging in again,
    /// if the protocol supports it (SSH channels). `None` otherwise.
    fn duplicate(&self, _size: Size, _waker: Waker) -> Option<Box<dyn Transport>> {
        None
    }

    /// An SFTP session on the same connection, if the protocol has one and
    /// the connection is logged in (SSH). `None` otherwise.
    fn sftp(&self, _waker: Waker) -> Option<sftp::SftpClient> {
        None
    }

    /// An SCP transfer on a channel of its own over the same connection, if
    /// the protocol has one and the connection is logged in (SSH).
    fn scp(&self, _request: scp::Request, _waker: Waker) -> Option<scp::ScpTransfer> {
        None
    }

    /// A file browser session over SCP (folders listed with `ls`), its
    /// commands run in `shell` (see [`scp::shell_from`]); SSH only.
    fn scp_browser(&self, _shell: Option<String>, _waker: Waker) -> Option<sftp::SftpClient> {
        None
    }
}

/// In-memory transport that echoes everything written back to the reader.
/// Used by tests and by the GUI before a real protocol is wired in.
#[derive(Debug, Default)]
pub struct Loopback {
    pending: VecDeque<u8>,
    size: Size,
    closed: bool,
}

impl Loopback {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes the "remote" side would produce, as if received from the network.
    pub fn inject(&mut self, data: &[u8]) {
        self.pending.extend(data);
    }

    pub fn size(&self) -> Size {
        self.size
    }
}

impl Transport for Loopback {
    fn protocol(&self) -> Protocol {
        Protocol::Local
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        let n = buf.len().min(self.pending.len());
        for (slot, byte) in buf.iter_mut().zip(self.pending.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        self.pending.extend(data);
        Ok(())
    }

    fn resize(&mut self, size: Size) -> Result<(), TransportError> {
        self.size = size;
        Ok(())
    }

    fn close(&mut self) -> Result<(), TransportError> {
        self.closed = true;
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_echoes_and_closes() {
        let mut t = Loopback::new();
        t.write(b"hello").unwrap();
        let mut buf = [0u8; 3];
        assert_eq!(t.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf, b"hel");
        assert_eq!(t.read(&mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"lo");
        assert_eq!(t.read(&mut buf).unwrap(), 0);

        t.resize(Size {
            cols: 132,
            rows: 50,
        })
        .unwrap();
        assert_eq!(
            t.size(),
            Size {
                cols: 132,
                rows: 50
            }
        );

        t.close().unwrap();
        assert!(matches!(t.write(b"x"), Err(TransportError::Closed)));
    }
}
