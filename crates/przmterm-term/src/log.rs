//! Plain-text session logs.
//!
//! [`TextLog`] turns the raw byte stream from the remote side into readable
//! lines: escape sequences are dropped, `\r\n` becomes `\n`, backspace erases
//! and a bare `\r` (progress bars, `--More--` prompts) starts the line again.

use vte::{Params, Parser, Perform};

/// Converts terminal output to log text.
pub struct TextLog {
    parser: Parser,
    sink: Sink,
}

#[derive(Default)]
struct Sink {
    line: String,
    out: String,
    carriage_return: bool,
}

impl Sink {
    fn end_line(&mut self) {
        self.out.push_str(self.line.trim_end());
        self.out.push('\n');
        self.line.clear();
        self.carriage_return = false;
    }
}

impl Perform for Sink {
    fn print(&mut self, c: char) {
        if self.carriage_return {
            self.line.clear();
            self.carriage_return = false;
        }
        self.line.push(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.end_line(),
            b'\r' => self.carriage_return = true,
            0x08 => {
                self.line.pop();
            }
            b'\t' => self.line.push('\t'),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, _: &Params, _: &[u8], _: bool, action: char) {
        // Erase-in-line after a carriage return is how many devices redraw a line.
        if action == 'K' && self.carriage_return {
            self.line.clear();
        }
    }
}

impl Default for TextLog {
    fn default() -> Self {
        Self::new()
    }
}

impl TextLog {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            sink: Sink::default(),
        }
    }

    /// Feeds output and returns the lines completed so far.
    pub fn feed(&mut self, bytes: &[u8]) -> String {
        self.parser.advance(&mut self.sink, bytes);
        std::mem::take(&mut self.sink.out)
    }

    /// Returns the unfinished last line, if any (call when the log closes).
    pub fn finish(&mut self) -> String {
        if self.sink.line.is_empty() {
            String::new()
        } else {
            self.sink.end_line();
            std::mem::take(&mut self.sink.out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escapes_and_normalises_line_ends() {
        let mut log = TextLog::new();
        let out = log.feed(b"\x1b[1;32mok\x1b[0m done\r\nnext");
        assert_eq!(out, "ok done\n");
        assert_eq!(log.feed(b" line\r\n"), "next line\n");
        assert_eq!(log.finish(), "");
    }

    #[test]
    fn carriage_return_and_backspace_overwrite() {
        let mut log = TextLog::new();
        let out = log.feed(b"10%\r20%\r100%\r\nabcX\x08d\r\n");
        assert_eq!(out, "100%\nabcd\n");
    }

    #[test]
    fn more_prompt_erased_by_device_is_dropped() {
        let mut log = TextLog::new();
        let out = log.feed(b"line 1\r\n --More-- \r\x1b[Kline 2\r\n");
        assert_eq!(out, "line 1\nline 2\n");
    }

    #[test]
    fn finish_flushes_partial_line() {
        let mut log = TextLog::new();
        assert_eq!(log.feed(b"router# "), "");
        assert_eq!(log.finish(), "router#\n");
    }
}
