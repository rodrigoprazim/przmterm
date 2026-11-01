//! A tiny line-editing shell behind the [`Transport`] trait.
//!
//! It lets 0.1 exercise the emulator and the GUI end to end before the SSH
//! transport lands in 0.2. It is not a real shell.

use std::collections::VecDeque;

use przmterm_transport::{Protocol, Size, Transport, TransportError};

const PROMPT: &str = "\x1b[1;36mprzm\x1b[0m:\x1b[1;34mdemo\x1b[0m$ ";

pub struct DemoShell {
    out: VecDeque<u8>,
    line: String,
    size: Size,
    escape: Escape,
    closed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    None,
    Started,
    Sequence,
}

impl DemoShell {
    pub fn new() -> Self {
        let mut shell = Self {
            out: VecDeque::new(),
            line: String::new(),
            size: Size::default(),
            escape: Escape::None,
            closed: false,
        };
        shell.emit(&format!(
            "\x1b[1mPrzmTerm {}\x1b[0m — demo shell (SSH arrives in 0.2.0)\r\n\
             Type \x1b[33mhelp\x1b[0m for commands.\r\n\r\n{PROMPT}",
            przmterm_core::VERSION
        ));
        shell
    }

    fn emit(&mut self, s: &str) {
        self.out.extend(s.as_bytes());
    }

    fn run(&mut self, cmd: &str) {
        match cmd.trim() {
            "" => {}
            "help" => self.emit(
                "  help     this list\r\n  colors   16 and 256-colour palette\r\n  \
                 styles   bold, italic, underline, reverse\r\n  size     terminal size\r\n  \
                 clear    clear the screen\r\n  version  PrzmTerm version\r\n",
            ),
            "colors" => {
                let mut s = String::new();
                for i in 0..16 {
                    s.push_str(&format!("\x1b[48;5;{i}m  "));
                }
                s.push_str("\x1b[0m\r\n");
                for row in 0..6 {
                    for i in 0..36 {
                        s.push_str(&format!("\x1b[48;5;{}m ", 16 + row * 36 + i));
                    }
                    s.push_str("\x1b[0m\r\n");
                }
                for i in 232..=255 {
                    s.push_str(&format!("\x1b[48;5;{i}m "));
                }
                s.push_str("\x1b[0m\r\n");
                for i in 0..48 {
                    let r = 255 - i * 5;
                    let g = i * 5;
                    s.push_str(&format!("\x1b[48;2;{r};{g};128m "));
                }
                s.push_str("\x1b[0m  truecolor\r\n");
                self.emit(&s);
            }
            "styles" => self.emit(
                "\x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \
                 \x1b[4munderline\x1b[0m \x1b[7mreverse\x1b[0m\r\n",
            ),
            "size" => {
                let Size { cols, rows } = self.size;
                self.emit(&format!("{cols} columns × {rows} rows\r\n"));
            }
            "clear" => self.emit("\x1b[2J\x1b[H"),
            "version" => self.emit(&format!("PrzmTerm {}\r\n", przmterm_core::VERSION)),
            other => {
                let msg = format!("{other}: command not found (try help)\r\n");
                self.emit(&msg);
            }
        }
    }

    fn handle(&mut self, byte: u8) {
        // Swallow escape sequences (arrow keys etc.) — no history in the demo.
        match self.escape {
            Escape::Started => {
                self.escape = if matches!(byte, b'[' | b'O') {
                    Escape::Sequence
                } else {
                    Escape::None
                };
                return;
            }
            Escape::Sequence => {
                if (0x40..=0x7e).contains(&byte) {
                    self.escape = Escape::None;
                }
                return;
            }
            Escape::None => {}
        }
        match byte {
            0x1b => self.escape = Escape::Started,
            b'\r' | b'\n' => {
                self.emit("\r\n");
                let line = std::mem::take(&mut self.line);
                self.run(&line);
                self.emit(PROMPT);
            }
            0x7f | 0x08 => {
                if self.line.pop().is_some() {
                    self.emit("\x08 \x08");
                }
            }
            0x03 => {
                self.line.clear();
                self.emit("^C\r\n");
                self.emit(PROMPT);
            }
            0x0c => {
                self.emit("\x1b[2J\x1b[H");
                self.emit(PROMPT);
                let line = self.line.clone();
                self.emit(&line);
            }
            b if b >= 0x20 => {
                let c = b as char;
                self.line.push(c);
                self.out.push_back(b);
            }
            _ => {}
        }
    }
}

impl Transport for DemoShell {
    fn protocol(&self) -> Protocol {
        Protocol::Local
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let n = buf.len().min(self.out.len());
        for (slot, b) in buf.iter_mut().zip(self.out.drain(..n)) {
            *slot = b;
        }
        Ok(n)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Closed);
        }
        // Input arrives as UTF-8; the demo keeps it byte-oriented and only
        // accepts ASCII for its own commands, echoing other bytes as-is.
        for &b in data {
            if b >= 0x80 {
                self.out.push_back(b);
            } else {
                self.handle(b);
            }
        }
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

    fn drain(t: &mut DemoShell) -> String {
        let mut buf = [0u8; 4096];
        let n = t.read(&mut buf).unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    #[test]
    fn echoes_and_runs_commands() {
        let mut t = DemoShell::new();
        drain(&mut t);
        t.write(b"versioX\x7fn\r").unwrap();
        let out = drain(&mut t);
        assert!(out.starts_with("versioX\x08 \x08n\r\n"));
        assert!(out.contains(&format!("PrzmTerm {}", przmterm_core::VERSION)));
    }

    #[test]
    fn ignores_arrow_keys() {
        let mut t = DemoShell::new();
        drain(&mut t);
        t.write(b"\x1b[A\x1bOBx").unwrap();
        assert_eq!(drain(&mut t), "x");
    }
}
