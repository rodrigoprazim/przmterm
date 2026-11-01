//! Byte transports for PrzmTerm sessions.
//!
//! Every protocol (SSH, Telnet, serial, local process) implements the same
//! [`Transport`] contract: it moves raw bytes and knows nothing about terminal
//! emulation. The emulator in `przmterm-term` interprets what comes out.
//!
//! Phase 0 keeps the contract synchronous; the SSH implementation (russh) will
//! drive it from a Tokio task per session.

use std::collections::VecDeque;
use std::io;

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
