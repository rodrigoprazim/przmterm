//! Keyboard → bytes, following xterm conventions.

use eframe::egui::{Key, Modifiers};

/// Bytes for a key press that does not produce text, or `None` to ignore it.
pub fn encode_key(key: Key, modifiers: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    // Ctrl+letter → C0 control character (Ctrl+A = 0x01 … Ctrl+Z = 0x1A).
    if modifiers.ctrl && !modifiers.alt && !modifiers.mac_cmd {
        let name = key.name();
        if name.len() == 1 {
            let c = name.as_bytes()[0].to_ascii_uppercase();
            if c.is_ascii_uppercase() {
                return Some(vec![c - b'A' + 1]);
            }
        }
        match key {
            Key::OpenBracket => return Some(vec![0x1b]),
            Key::Backslash => return Some(vec![0x1c]),
            Key::CloseBracket => return Some(vec![0x1d]),
            _ => {}
        }
    }

    let arrow = |c: u8| {
        if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    let bytes: Vec<u8> = match key {
        Key::Enter => b"\r".to_vec(),
        Key::Backspace => vec![0x7f],
        Key::Tab if modifiers.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => vec![0x1b],
        Key::ArrowUp => arrow(b'A'),
        Key::ArrowDown => arrow(b'B'),
        Key::ArrowRight => arrow(b'C'),
        Key::ArrowLeft => arrow(b'D'),
        Key::Home => arrow(b'H'),
        Key::End => arrow(b'F'),
        Key::Insert => b"\x1b[2~".to_vec(),
        Key::Delete => b"\x1b[3~".to_vec(),
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
        Key::F1 => b"\x1bOP".to_vec(),
        Key::F2 => b"\x1bOQ".to_vec(),
        Key::F3 => b"\x1bOR".to_vec(),
        Key::F4 => b"\x1bOS".to_vec(),
        Key::F5 => b"\x1b[15~".to_vec(),
        Key::F6 => b"\x1b[17~".to_vec(),
        Key::F7 => b"\x1b[18~".to_vec(),
        Key::F8 => b"\x1b[19~".to_vec(),
        Key::F9 => b"\x1b[20~".to_vec(),
        Key::F10 => b"\x1b[21~".to_vec(),
        Key::F11 => b"\x1b[23~".to_vec(),
        Key::F12 => b"\x1b[24~".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

/// Wraps pasted text for bracketed-paste mode and normalises line endings to CR.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{body}\x1b[201~").into_bytes()
    } else {
        body.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;

    #[test]
    fn control_letters() {
        assert_eq!(encode_key(Key::C, Modifiers::CTRL, false), Some(vec![3]));
        assert_eq!(encode_key(Key::Z, Modifiers::CTRL, false), Some(vec![26]));
        assert_eq!(
            encode_key(Key::OpenBracket, Modifiers::CTRL, false),
            Some(vec![0x1b])
        );
    }

    #[test]
    fn arrows_follow_cursor_key_mode() {
        assert_eq!(
            encode_key(Key::ArrowUp, NONE, false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(Key::ArrowUp, NONE, true),
            Some(b"\x1bOA".to_vec())
        );
    }

    #[test]
    fn editing_keys() {
        assert_eq!(encode_key(Key::Enter, NONE, false), Some(b"\r".to_vec()));
        assert_eq!(encode_key(Key::Backspace, NONE, false), Some(vec![0x7f]));
        assert_eq!(
            encode_key(Key::Tab, Modifiers::SHIFT, false),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(encode_key(Key::A, NONE, false), None);
    }

    #[test]
    fn paste_modes() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb");
        assert_eq!(encode_paste("x", true), b"\x1b[200~x\x1b[201~");
    }
}
