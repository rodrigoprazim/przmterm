//! Serial console transport on the `serialport` crate.
//!
//! A port is opened and driven by two plain threads: one writes keystrokes
//! and sends BREAK, the other reads with a short timeout and forwards what
//! arrives. Opening happens on the writer thread, so a slow or missing
//! device never blocks the GUI.

use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use przmterm_core::serial::{FlowControl, Parity, SerialLine};

use crate::pipe::{Closed, Cmd, Output, Pipe, TaskEnd, finish};
use crate::{Ending, Protocol, Size, Transport, TransportError, Waker};

const READ_TIMEOUT: Duration = Duration::from_millis(50);
/// How long the line is held in the break state.
const BREAK_LENGTH: Duration = Duration::from_millis(300);

/// A serial port found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    /// What to open: `/dev/ttyUSB0`, `/dev/cu.usbserial-1410`, `COM3`…
    pub name: String,
    /// Adapter maker and model when known, e.g. `FTDI FT232R USB UART`.
    pub description: String,
}

/// Serial ports on this machine, sorted by name. On macOS only the
/// `/dev/cu.*` call-out devices are listed: their `/dev/tty.*` twins wait
/// for a carrier signal that console cables do not provide.
pub fn available_ports() -> Vec<PortInfo> {
    let mut ports: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| !(cfg!(target_os = "macos") && p.port_name.starts_with("/dev/tty.")))
        .map(|p| PortInfo {
            description: describe(&p.port_type),
            name: p.port_name,
        })
        .collect();
    ports.sort_by(|a, b| a.name.cmp(&b.name));
    ports.dedup_by(|a, b| a.name == b.name);
    ports
}

fn describe(kind: &serialport::SerialPortType) -> String {
    use serialport::SerialPortType::*;
    match kind {
        UsbPort(usb) => {
            let parts: Vec<&str> = [usb.manufacturer.as_deref(), usb.product.as_deref()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect();
            if parts.is_empty() {
                format!("USB {:04x}:{:04x}", usb.vid, usb.pid)
            } else {
                parts.join(" ")
            }
        }
        PciPort => "built-in".into(),
        BluetoothPort => "Bluetooth".into(),
        Unknown => String::new(),
    }
}

/// A serial console seen from the GUI.
pub struct SerialTransport {
    pipe: Pipe,
}

impl SerialTransport {
    /// Opens the port in the background and returns immediately. Failures
    /// are reported in the terminal.
    pub fn open(line: SerialLine, waker: Waker) -> Self {
        let (pipe, end) = Pipe::new(waker);
        let spawned = std::thread::Builder::new()
            .name("przmterm-serial".into())
            .spawn(move || writer(line, end));
        if let Err(e) = spawned {
            tracing::error!("could not start the serial thread: {e}");
        }
        Self { pipe }
    }
}

impl Transport for SerialTransport {
    fn protocol(&self) -> Protocol {
        Protocol::Serial
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        self.pipe.read(buf)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.pipe.send(Cmd::Data(data.to_vec()))
    }

    /// A serial line has no window size.
    fn resize(&mut self, _size: Size) -> Result<(), TransportError> {
        Ok(())
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

fn open(line: &SerialLine) -> serialport::Result<Box<dyn serialport::SerialPort>> {
    use serialport::{DataBits, StopBits};
    let data_bits = match line.data_bits {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    };
    let stop_bits = if line.stop_bits == 2 {
        StopBits::Two
    } else {
        StopBits::One
    };
    let parity = match line.parity {
        Parity::None => serialport::Parity::None,
        Parity::Odd => serialport::Parity::Odd,
        Parity::Even => serialport::Parity::Even,
    };
    let flow = match line.flow_control {
        FlowControl::None => serialport::FlowControl::None,
        FlowControl::Software => serialport::FlowControl::Software,
        FlowControl::Hardware => serialport::FlowControl::Hardware,
    };
    serialport::new(&line.port, line.baud_rate)
        .data_bits(data_bits)
        .stop_bits(stop_bits)
        .parity(parity)
        .flow_control(flow)
        .timeout(READ_TIMEOUT)
        .open()
}

/// Advice for the usual reasons a port does not open.
fn hint(e: &serialport::Error) -> Option<&'static str> {
    match e.kind() {
        serialport::ErrorKind::NoDevice => Some(
            "Is the cable plugged in? The port list in the session editor shows what is connected.",
        ),
        serialport::ErrorKind::Io(ErrorKind::PermissionDenied) if cfg!(target_os = "linux") => {
            Some(
                "Add your user to the `dialout` group (or `uucp` on some systems) and log in again.",
            )
        }
        serialport::ErrorKind::Io(ErrorKind::NotFound) => Some(
            "Is the cable plugged in? The port list in the session editor shows what is connected.",
        ),
        _ if e.description.contains("busy") => Some("Another program has the port open."),
        _ => None,
    }
}

fn writer(line: SerialLine, end: TaskEnd) {
    let TaskEnd {
        mut cmds,
        out,
        closed,
    } = end;
    let mut port = match open(&line) {
        Ok(port) => port,
        Err(e) => {
            out.error(&format!("Could not open {}: {e}", line.port));
            if let Some(hint) = hint(&e) {
                out.notice(hint);
            }
            finish(&closed, &out, Ending::Unreachable);
            return;
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    match port.try_clone() {
        Ok(reading) => {
            let (out2, stop2, closed2) = (out.clone(), stop.clone(), closed.clone());
            let name = line.port.clone();
            let spawned = std::thread::Builder::new()
                .name("przmterm-serial-read".into())
                .spawn(move || reader(reading, &name, &out2, &stop2, &closed2));
            if let Err(e) = spawned {
                out.error(&format!("Could not read from {}: {e}", line.port));
                finish(&closed, &out, Ending::Failed);
                return;
            }
        }
        Err(e) => {
            out.error(&format!("Could not open {}: {e}", line.port));
            finish(&closed, &out, Ending::Failed);
            return;
        }
    }
    out.notice(&format!("Connected to {line}."));

    while let Some(cmd) = cmds.blocking_recv() {
        if closed.is_closed() {
            break;
        }
        let result = match cmd {
            Cmd::Data(bytes) => port.write_all(&bytes).and_then(|()| port.flush()),
            Cmd::Break => send_break(port.as_ref()),
            Cmd::Resize(_) => Ok(()),
            Cmd::Close => break,
        };
        if let Err(e) = result {
            out.error(&format!("Could not write to {}: {e}", line.port));
        }
    }
    stop.store(true, Ordering::Release);
}

fn send_break(port: &dyn serialport::SerialPort) -> std::io::Result<()> {
    port.set_break()?;
    std::thread::sleep(BREAK_LENGTH);
    port.clear_break()?;
    Ok(())
}

fn reader(
    mut port: Box<dyn serialport::SerialPort>,
    name: &str,
    out: &Output,
    stop: &AtomicBool,
    closed: &Closed,
) {
    let mut buf = vec![0u8; 4096];
    while !stop.load(Ordering::Acquire) {
        match port.read(&mut buf) {
            Ok(0) => std::thread::sleep(READ_TIMEOUT),
            Ok(n) => out.bytes(&buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            Err(e) => {
                out.error(&format!("{name} disconnected: {e}"));
                break;
            }
        }
    }
    let ending = if stop.load(Ordering::Acquire) {
        out.notice(&format!("{name} closed."));
        Ending::Cancelled
    } else {
        Ending::Lost
    };
    finish(closed, out, ending);
}
