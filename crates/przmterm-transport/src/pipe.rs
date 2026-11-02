//! Plumbing shared by the network and serial transports: a background task
//! or thread on one side, the GUI's non-blocking [`Transport`] calls on the
//! other.
//!
//! [`Transport`]: crate::Transport

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc as std_mpsc};

use tokio::sync::mpsc;

use crate::{Ending, Size, TransportError, Waker};

/// Set once by the task when the session ends, read by the GUI.
#[derive(Clone, Default)]
pub(crate) struct Closed(Arc<(AtomicBool, Mutex<Option<Ending>>)>);

impl Closed {
    pub(crate) fn is_closed(&self) -> bool {
        self.0.0.load(Ordering::Acquire)
    }

    pub(crate) fn ending(&self) -> Option<Ending> {
        self.0.1.lock().ok().and_then(|e| *e)
    }

    /// Records the first ending; later calls keep it.
    pub(crate) fn set(&self, ending: Ending) {
        if let Ok(mut slot) = self.0.1.lock()
            && slot.is_none()
        {
            *slot = Some(ending);
        }
        self.0.0.store(true, Ordering::Release);
    }
}

/// The Tokio runtime shared by every network session.
pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("przmterm-net")
            .enable_all()
            .build()
            .expect("failed to start the network runtime")
    })
}

/// What the GUI asks of a session task.
pub(crate) enum Cmd {
    Data(Vec<u8>),
    Resize(Size),
    Break,
    Close,
}

/// Terminal-bound side of a session task.
#[derive(Clone)]
pub(crate) struct Output {
    tx: std_mpsc::Sender<Vec<u8>>,
    pub(crate) waker: Waker,
    /// Show [`Output::debug`] lines.
    debug: bool,
}

impl Output {
    pub(crate) fn with_debug(mut self, on: bool) -> Self {
        self.debug = on;
        self
    }

    /// A step of the connection, shown only with debugging on (like `ssh -v`).
    pub(crate) fn debug(&self, text: &str) {
        if self.debug {
            self.text(&format!("\x1b[36mdebug: {text}\x1b[0m\r\n"));
        }
    }

    pub(crate) fn debugging(&self) -> bool {
        self.debug
    }

    pub(crate) fn bytes(&self, data: &[u8]) {
        if !data.is_empty() && self.tx.send(data.to_vec()).is_ok() {
            (self.waker)();
        }
    }

    pub(crate) fn text(&self, text: &str) {
        self.bytes(text.as_bytes());
    }

    /// Normalises bare `\n` to `\r\n` for text that comes from the server.
    pub(crate) fn lines(&self, text: &str) {
        let normalised = text.replace("\r\n", "\n").replace('\n', "\r\n");
        self.text(&normalised);
    }

    pub(crate) fn notice(&self, text: &str) {
        self.text(&format!("\x1b[2m{text}\x1b[0m\r\n"));
    }

    pub(crate) fn error(&self, text: &str) {
        self.text(&format!("\x1b[31m{text}\x1b[0m\r\n"));
    }
}

/// The GUI's end of a session task.
pub(crate) struct Pipe {
    cmds: mpsc::UnboundedSender<Cmd>,
    output: std_mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pending_pos: usize,
    pub(crate) closed: Closed,
}

/// The task's end: commands in, terminal bytes out, and the closed flag to
/// set when the session ends.
pub(crate) struct TaskEnd {
    pub(crate) cmds: mpsc::UnboundedReceiver<Cmd>,
    pub(crate) out: Output,
    pub(crate) closed: Closed,
}

/// Marks the session as ended and wakes the GUI to notice.
pub(crate) fn finish(closed: &Closed, out: &Output, ending: Ending) {
    closed.set(ending);
    (out.waker)();
}

impl Pipe {
    pub(crate) fn new(waker: Waker) -> (Pipe, TaskEnd) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = std_mpsc::channel();
        let closed = Closed::default();
        let pipe = Pipe {
            cmds: cmd_tx,
            output: out_rx,
            pending: Vec::new(),
            pending_pos: 0,
            closed: closed.clone(),
        };
        let end = TaskEnd {
            cmds: cmd_rx,
            out: Output {
                tx: out_tx,
                waker,
                debug: false,
            },
            closed,
        };
        (pipe, end)
    }

    pub(crate) fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if self.pending_pos >= self.pending.len() {
            match self.output.try_recv() {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.pending_pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let rest = &self.pending[self.pending_pos..];
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        self.pending_pos += n;
        Ok(n)
    }

    pub(crate) fn send(&self, cmd: Cmd) -> Result<(), TransportError> {
        self.cmds.send(cmd).map_err(|_| TransportError::Closed)
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.is_closed()
    }

    pub(crate) fn ending(&self) -> Option<Ending> {
        self.closed.ending()
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Close);
    }
}
