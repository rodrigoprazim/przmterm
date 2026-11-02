//! Watches a session's output for trigger text and says what to answer.
//!
//! The raw bytes are turned into plain text first (escape sequences and
//! control characters removed), so a prompt matches however the remote side
//! coloured or positioned it. Text is searched across reads, so a prompt that
//! arrives in two pieces still matches; once a trigger has answered some
//! text, that text never fires it again.

use przmterm_core::triggers::Trigger;

use crate::plain::PlainText;

/// Plain text kept for matching across reads.
const WINDOW: usize = 4096;

enum Matcher {
    Text(String),
    Regex(fancy_regex::Regex),
}

struct Armed {
    matcher: Matcher,
    send: String,
    once: bool,
    fired: bool,
}

pub struct TriggerEngine {
    triggers: Vec<Armed>,
    /// Recent plain text.
    text: String,
    /// Matches must start at or after this byte of `text` (what came before
    /// has been answered already).
    from: usize,
    plain: PlainText,
}

impl TriggerEngine {
    /// Compiles `triggers`; unusable ones are described in the list returned.
    pub fn new(triggers: &[Trigger]) -> (TriggerEngine, Vec<String>) {
        let mut problems = Vec::new();
        let mut armed = Vec::new();
        for (i, t) in triggers.iter().enumerate() {
            if t.pattern.is_empty() {
                continue;
            }
            let matcher = if t.regex {
                match fancy_regex::Regex::new(&t.pattern) {
                    Ok(re) => Matcher::Regex(re),
                    Err(e) => {
                        problems.push(format!("trigger {}: bad regular expression: {e}", i + 1));
                        continue;
                    }
                }
            } else {
                Matcher::Text(t.pattern.clone())
            };
            armed.push(Armed {
                matcher,
                send: t.send.clone(),
                once: t.once,
                fired: false,
            });
        }
        (
            TriggerEngine {
                triggers: armed,
                text: String::new(),
                from: 0,
                plain: PlainText::new(),
            },
            problems,
        )
    }

    pub fn is_empty(&self) -> bool {
        self.triggers.is_empty()
    }

    /// Starts over for a new connection: `once` triggers can fire again.
    pub fn reset(&mut self) {
        self.text.clear();
        self.from = 0;
        self.plain.reset();
        for t in &mut self.triggers {
            t.fired = false;
        }
    }

    /// Takes bytes from the session; returns the `send` text of each trigger
    /// that fired, in order.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        if self.triggers.is_empty() {
            return Vec::new();
        }
        self.plain.push(bytes, &mut self.text);

        let mut fired = Vec::new();
        // Answer every trigger that matches, earliest match first, and
        // never the same text twice.
        loop {
            let mut best: Option<(usize, usize, usize)> = None; // start, end, trigger
            for (i, t) in self.triggers.iter().enumerate() {
                if t.once && t.fired {
                    continue;
                }
                let hay = &self.text[self.from..];
                let found = match &t.matcher {
                    Matcher::Text(p) => hay.find(p.as_str()).map(|s| (s, s + p.len())),
                    Matcher::Regex(re) => re
                        .find(hay)
                        .ok()
                        .flatten()
                        .filter(|m| m.end() > m.start())
                        .map(|m| (m.start(), m.end())),
                };
                if let Some((s, e)) = found
                    && best.is_none_or(|(bs, _, _)| s < bs)
                {
                    best = Some((s, e, i));
                }
            }
            let Some((_, end, i)) = best else { break };
            self.from += end;
            let t = &mut self.triggers[i];
            t.fired = true;
            fired.push(t.send.clone());
        }

        // Forget old text, keeping what might still be part of a match.
        if self.text.len() > WINDOW {
            let mut drop = self.text.len() - WINDOW / 2;
            while !self.text.is_char_boundary(drop) {
                drop += 1;
            }
            self.text.drain(..drop);
            self.from = self.from.saturating_sub(drop);
        }
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(pattern: &str, regex: bool, send: &str, once: bool) -> Trigger {
        Trigger {
            pattern: pattern.into(),
            regex,
            send: send.into(),
            once,
        }
    }

    fn engine(triggers: &[Trigger]) -> TriggerEngine {
        let (e, problems) = TriggerEngine::new(triggers);
        assert!(problems.is_empty(), "{problems:?}");
        e
    }

    #[test]
    fn answers_prompts_split_across_reads_once_each() {
        let mut e = engine(&[trigger("--More--", false, " ", false)]);
        assert!(e.feed(b"line 1\r\nline 2\r\n --Mo").is_empty());
        assert_eq!(e.feed(b"re-- "), [" "]);
        // The same prompt is not answered again…
        assert!(e.feed(b"\x08\x08\x08\x08").is_empty());
        // …but the next one is.
        assert_eq!(e.feed(b"line 3\r\n --More-- "), [" "]);
    }

    #[test]
    fn colours_and_cursor_moves_do_not_hide_text() {
        let mut e = engine(&[trigger("Password:", false, "x\\r", false)]);
        assert_eq!(
            e.feed(b"\x1b[1;32mPass\x1b[0mword:\x1b]0;title\x07 "),
            ["x\\r"]
        );
    }

    #[test]
    fn regexes_and_once() {
        let mut e = engine(&[
            trigger(r"\[confirm\]$", true, "\\r", false),
            trigger("Password:", false, "pw\\r", true),
        ]);
        assert_eq!(
            e.feed(b"Destination filename [startup-config]? [confirm]"),
            ["\\r"]
        );
        assert_eq!(e.feed(b"\r\nPassword: "), ["pw\\r"]);
        assert!(e.feed(b"\r\nPassword: ").is_empty(), "once");
        e.reset();
        assert_eq!(
            e.feed(b"Password: "),
            ["pw\\r"],
            "a new connection arms it again"
        );
    }

    #[test]
    fn several_in_one_read_in_order() {
        let mut e = engine(&[
            trigger("B", false, "b", false),
            trigger("A", false, "a", false),
        ]);
        assert_eq!(e.feed(b"xAyBzA"), ["a", "b", "a"]);
    }

    #[test]
    fn long_output_and_utf8_split() {
        let mut e = engine(&[trigger("fim", false, "ok", false)]);
        for _ in 0..100 {
            assert!(e.feed(&[b'x'; 1000]).is_empty());
        }
        let text = "ação fim".as_bytes();
        assert!(e.feed(&text[..2]).is_empty(), "half of 'ç'");
        assert_eq!(e.feed(&text[2..]), ["ok"]);
        assert!(e.text.len() <= WINDOW);
    }

    #[test]
    fn reports_bad_expressions() {
        let (e, problems) = TriggerEngine::new(&[trigger("(open", true, "x", false)]);
        assert!(e.is_empty());
        assert_eq!(problems.len(), 1);
    }
}
