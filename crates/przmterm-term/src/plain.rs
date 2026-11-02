//! Turns terminal output into plain text: escape sequences and control
//! characters other than CR, LF and Tab removed, UTF-8 characters split
//! across reads put back together. Used where output is matched as text
//! (triggers, scripts), so colours and cursor moves never hide a prompt.

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Esc {
    #[default]
    None,
    Start,
    /// `ESC [` … final byte.
    Csi,
    /// `ESC ]` … BEL or `ESC \`.
    Osc,
    OscEnd,
}

#[derive(Default)]
pub struct PlainText {
    esc: Esc,
    /// Bytes of a UTF-8 character split across reads.
    partial: Vec<u8>,
}

impl PlainText {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets a half-read sequence (for a new connection).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Appends the plain text of `bytes` to `out`.
    pub fn push(&mut self, bytes: &[u8], out: &mut String) {
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(bytes);
        // Keep an incomplete UTF-8 character for the next read.
        let cut = match std::str::from_utf8(&data) {
            Ok(_) => data.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => data.len(),
        };
        self.partial = data.split_off(cut);
        for c in String::from_utf8_lossy(&data).chars() {
            self.esc = match (self.esc, c) {
                (Esc::None, '\x1b') => Esc::Start,
                (Esc::None, '\n' | '\r' | '\t') => {
                    out.push(c);
                    Esc::None
                }
                (Esc::None, c) if c.is_control() => Esc::None,
                (Esc::None, c) => {
                    out.push(c);
                    Esc::None
                }
                (Esc::Start, '[') => Esc::Csi,
                (Esc::Start, ']') => Esc::Osc,
                (Esc::Start, _) => Esc::None,
                (Esc::Csi, c) if ('\x40'..='\x7e').contains(&c) => Esc::None,
                (Esc::Csi, _) => Esc::Csi,
                (Esc::Osc, '\x07') => Esc::None,
                (Esc::Osc, '\x1b') => Esc::OscEnd,
                (Esc::Osc, _) => Esc::Osc,
                (Esc::OscEnd, _) => Esc::None,
            };
        }
    }

    /// The plain text of `bytes`.
    pub fn text(&mut self, bytes: &[u8]) -> String {
        let mut out = String::new();
        self.push(bytes, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sequences_and_joins_split_characters() {
        let mut p = PlainText::new();
        assert_eq!(
            p.text(b"\x1b[1;32mPass\x1b[0mword:\x1b]0;title\x07 \x07ok\r\n"),
            "Password: ok\r\n"
        );
        let text = "ação".as_bytes();
        assert_eq!(p.text(&text[..2]), "a");
        assert_eq!(p.text(&text[2..]), "ção");
        assert_eq!(p.text(b"\x1b[3"), "");
        assert_eq!(p.text(b"1mred"), "red", "a sequence split across reads");
    }
}
