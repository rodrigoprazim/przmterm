//! Keyword highlighting: colours words and patterns in what is shown,
//! without touching the terminal's contents.
//!
//! Only text the remote side left in the default colour is recoloured, so
//! programs that colour their own output (`ls`, `vim`) keep their colours.

use std::cell::RefCell;
use std::collections::HashMap;

use przmterm_core::keywords::{KeywordSet, parse_color};

use crate::{Cell, Color};

enum Matcher {
    /// A whole word or phrase, as chars (lower-cased unless matching case).
    Word(Vec<char>),
    Regex(fancy_regex::Regex),
}

struct Rule {
    matcher: Matcher,
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
}

/// A highlighted stretch of text, in chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlight {
    pub range: std::ops::Range<usize>,
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
    pub bold: bool,
}

/// A match: first char, end char, rule index.
type Span = (usize, usize, usize);

/// A compiled keyword set.
pub struct Highlighter {
    rules: Vec<Rule>,
    match_case: bool,
    /// Matches per line text: lines repeat from frame to frame.
    cache: RefCell<HashMap<String, Vec<Span>>>,
}

/// Lines remembered before the cache starts over.
const CACHE_LINES: usize = 4096;

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Highlighter {
    /// Compiles `set`. Rules that cannot be used (a bad regex, no colour)
    /// are left out and described in the returned list.
    pub fn new(set: &KeywordSet) -> (Highlighter, Vec<String>) {
        let mut problems = Vec::new();
        let mut rules = Vec::new();
        for (i, k) in set.rules.iter().enumerate() {
            let color = |c: &Option<String>, what: &str, problems: &mut Vec<String>| match c {
                None => None,
                Some(text) => match parse_color(text) {
                    Some([r, g, b]) => Some(Color::Rgb(r, g, b)),
                    None => {
                        problems.push(format!("rule {}: `{text}` is not a {what} colour", i + 1));
                        None
                    }
                },
            };
            let fg = color(&k.color, "text", &mut problems);
            let bg = color(&k.background, "background", &mut problems);
            if fg.is_none() && bg.is_none() && !k.bold {
                continue; // nothing to draw
            }
            let matcher = if !k.regex.is_empty() {
                let pattern = if set.match_case {
                    k.regex.clone()
                } else {
                    format!("(?i){}", k.regex)
                };
                match fancy_regex::Regex::new(&pattern) {
                    Ok(re) => Matcher::Regex(re),
                    Err(e) => {
                        problems.push(format!("rule {}: bad regular expression: {e}", i + 1));
                        continue;
                    }
                }
            } else if !k.text.is_empty() {
                Matcher::Word(if set.match_case {
                    k.text.chars().collect()
                } else {
                    k.text.chars().flat_map(char::to_lowercase).collect()
                })
            } else {
                continue;
            };
            rules.push(Rule {
                matcher,
                fg,
                bg,
                bold: k.bold,
            });
        }
        (
            Highlighter {
                rules,
                match_case: set.match_case,
                cache: RefCell::default(),
            },
            problems,
        )
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Matches in `text` as `(first char, end char, rule)`, without
    /// overlaps: the leftmost, then longest, then earliest rule wins.
    fn find(&self, text: &str) -> Vec<(usize, usize, usize)> {
        let chars: Vec<char> = text.chars().collect();
        let folded: Vec<char> = if self.match_case {
            chars.clone()
        } else {
            // One char per char, so positions stay aligned.
            chars
                .iter()
                .map(|c| c.to_lowercase().next().unwrap_or(*c))
                .collect()
        };
        // Byte offset → char index, for regex matches.
        let mut char_at = vec![0usize; text.len() + 1];
        for (i, (b, _)) in text.char_indices().enumerate() {
            char_at[b] = i;
        }
        char_at[text.len()] = chars.len();

        // Every candidate first; then the leftmost, longest one wins and
        // earlier rules break ties (`admin-down` beats `-down` and `down`).
        let mut candidates: Vec<Span> = Vec::new();
        let claim = |a: usize, b: usize, rule: usize, found: &mut Vec<Span>| {
            if a < b {
                found.push((a, b, rule));
            }
        };
        let found = &mut candidates;
        for (r, rule) in self.rules.iter().enumerate() {
            match &rule.matcher {
                Matcher::Word(word) => {
                    let n = word.len();
                    if n == 0 || n > folded.len() {
                        continue;
                    }
                    for start in 0..=folded.len() - n {
                        if folded[start..start + n] != word[..] {
                            continue;
                        }
                        // Whole words only: no word character glued on, at
                        // either end that is itself a word character.
                        let before_ok =
                            start == 0 || !is_word(word[0]) || !is_word(chars[start - 1]);
                        let after_ok = start + n == chars.len()
                            || !is_word(word[n - 1])
                            || !is_word(chars[start + n]);
                        if before_ok && after_ok {
                            claim(start, start + n, r, found);
                        }
                    }
                }
                Matcher::Regex(re) => {
                    for m in re.find_iter(text).flatten() {
                        claim(char_at[m.start()], char_at[m.end()], r, found);
                    }
                }
            }
        }
        candidates.sort_by_key(|&(a, b, r)| (a, std::cmp::Reverse(b - a), r));
        let mut taken = vec![false; chars.len()];
        let mut found = Vec::new();
        for (a, b, r) in candidates {
            if !taken[a..b].iter().any(|t| *t) {
                taken[a..b].iter_mut().for_each(|t| *t = true);
                found.push((a, b, r));
            }
        }
        found
    }

    /// What would be highlighted in `text`: char ranges with their text
    /// colour, background and boldness, for previews outside a terminal.
    pub fn spans(&self, text: &str) -> Vec<Highlight> {
        let rgb = |c: Option<Color>| match c {
            Some(Color::Rgb(r, g, b)) => Some([r, g, b]),
            _ => None,
        };
        let mut found: Vec<Highlight> = self
            .find(text)
            .into_iter()
            .map(|(a, b, r)| {
                let rule = &self.rules[r];
                Highlight {
                    range: a..b,
                    fg: rgb(rule.fg),
                    bg: rgb(rule.bg),
                    bold: rule.bold,
                }
            })
            .collect();
        found.sort_by_key(|h| h.range.start);
        found
    }

    /// Recolours the matches in one line of cells.
    pub fn apply(&self, cells: &mut [Cell]) {
        if self.rules.is_empty() {
            return;
        }
        // The line as text, and the cell of each char.
        let mut text = String::with_capacity(cells.len());
        let mut col_of = Vec::with_capacity(cells.len());
        for (col, cell) in cells.iter().enumerate() {
            if !cell.is_spacer() {
                text.push(cell.ch);
                col_of.push(col);
            }
        }
        if text.trim().is_empty() {
            return;
        }
        let matches = {
            let mut cache = self.cache.borrow_mut();
            if let Some(m) = cache.get(&text) {
                m.clone()
            } else {
                let m = self.find(&text);
                if cache.len() >= CACHE_LINES {
                    cache.clear();
                }
                cache.insert(text, m.clone());
                m
            }
        };
        for (a, b, r) in matches {
            let rule = &self.rules[r];
            for &col in &col_of[a..b] {
                let style = &mut cells[col].style;
                if style.fg != Color::Default || style.reverse {
                    continue;
                }
                if let Some(fg) = rule.fg {
                    style.fg = fg;
                }
                if let Some(bg) = rule.bg {
                    style.bg = bg;
                }
                style.bold |= rule.bold;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use przmterm_core::keywords::Keyword;

    fn word(text: &str, color: &str) -> Keyword {
        Keyword {
            text: text.into(),
            color: Some(color.into()),
            ..Keyword::default()
        }
    }

    fn regex(re: &str, color: &str) -> Keyword {
        Keyword {
            regex: re.into(),
            color: Some(color.into()),
            ..Keyword::default()
        }
    }

    fn set(rules: Vec<Keyword>) -> Highlighter {
        let (h, problems) = Highlighter::new(&KeywordSet {
            match_case: false,
            rules,
        });
        assert!(problems.is_empty(), "{problems:?}");
        h
    }

    /// The text each match covers.
    fn spans(h: &Highlighter, text: &str) -> Vec<(String, usize)> {
        let chars: Vec<char> = text.chars().collect();
        let mut found: Vec<_> = h
            .find(text)
            .into_iter()
            .map(|(a, b, r)| (a, chars[a..b].iter().collect::<String>(), r))
            .collect();
        found.sort();
        found.into_iter().map(|(_, s, r)| (s, r)).collect()
    }

    #[test]
    fn whole_words_first_rule_wins() {
        let h = set(vec![
            word("no-error", "#ffffff"),
            word("up", "#00ff00"),
            word("no", "#ff0000"),
            word("(dgi)", "#ffff00"),
        ]);
        assert_eq!(
            spans(&h, "Gi0/1 is UP, backup no, no-error (dgi) node"),
            [
                ("UP".into(), 1),
                ("no".into(), 2),
                ("no-error".into(), 0),
                ("(dgi)".into(), 3),
            ]
        );
    }

    #[test]
    fn the_longest_match_wins() {
        let h = set(vec![
            word("-down", "#ffffff"),
            word("down", "#ff0000"),
            word("admin-down", "#ffff00"),
        ]);
        assert_eq!(
            spans(&h, "admin-down, down"),
            [("admin-down".into(), 2), ("down".into(), 1)]
        );
    }

    #[test]
    fn regexes_with_lookaround() {
        let ipv6 = r"(?:(?:[0-9A-Fa-f]{1,4}:){7}[0-9A-Fa-f]{1,4}|(?=(?:[0-9A-Fa-f]{0,4}:){0,7}[0-9A-Fa-f]{0,4}(?![:.\w]))(([0-9A-Fa-f]{1,4}:){1,7}|:)((:[0-9A-Fa-f]{1,4}){1,7}|:))(?![:.\w])";
        let h = set(vec![
            regex(
                r"([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})",
                "#ff55ff",
            ),
            regex(ipv6, "#80ffff"),
            word("down", "#ff0000"),
        ]);
        assert_eq!(
            spans(&h, "peer 10.0.0.1 via fe80::1 is down"),
            [
                ("10.0.0.1".into(), 0),
                ("fe80::1".into(), 1),
                ("down".into(), 2),
            ]
        );
    }

    #[test]
    fn recolours_only_default_coloured_cells() {
        let h = set(vec![word("down", "#ff0000")]);
        let mut term = crate::Terminal::new(40, 2);
        term.feed(b"link down \x1b[32mdown\x1b[0m");
        let mut line = term.screen().view_line(0).to_vec();
        h.apply(&mut line);
        assert_eq!(line[5].style.fg, Color::Rgb(255, 0, 0), "plain text");
        assert_eq!(
            line[10].style.fg,
            Color::Indexed(2),
            "the program's own colour stays"
        );
        assert_eq!(line[0].style.fg, Color::Default);
    }

    #[test]
    fn reports_bad_rules() {
        let (h, problems) = Highlighter::new(&KeywordSet {
            match_case: false,
            rules: vec![regex("(unclosed", "#ff0000"), word("x", "red")],
        });
        assert!(h.is_empty());
        assert_eq!(problems.len(), 2, "{problems:?}");
    }
}
