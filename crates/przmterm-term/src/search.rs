//! Finding text in what a terminal holds: its history and its screen.
//!
//! A line soft-wrapped over several rows is searched as one, so a match
//! can run from one row onto the next. Positions are combined indexes
//! (history first, then the screen, as [`Screen::line_at`]) and columns.

use std::ops::Range;

use crate::Screen;

/// What to look for: plain text (anywhere, not only whole words) or a
/// regular expression, with or without matching case.
#[derive(Debug, Clone)]
pub struct Pattern {
    matcher: Matcher,
    match_case: bool,
}

#[derive(Debug, Clone)]
enum Matcher {
    /// Lower-cased unless matching case.
    Text(Vec<char>),
    Regex(fancy_regex::Regex),
}

impl Pattern {
    /// `None` for empty text; an error for a regular expression that does
    /// not compile.
    pub fn new(text: &str, regex: bool, match_case: bool) -> Result<Option<Pattern>, String> {
        if text.is_empty() {
            return Ok(None);
        }
        let matcher = if regex {
            let pattern = if match_case {
                text.to_string()
            } else {
                format!("(?i){text}")
            };
            Matcher::Regex(fancy_regex::Regex::new(&pattern).map_err(|e| e.to_string())?)
        } else {
            Matcher::Text(fold(text.chars(), match_case))
        };
        Ok(Some(Pattern {
            matcher,
            match_case,
        }))
    }

    /// The matches in `text`, as char ranges, left to right without
    /// overlapping. Empty regex matches are left out.
    pub fn find(&self, text: &str) -> Vec<Range<usize>> {
        match &self.matcher {
            Matcher::Text(needle) => {
                let hay = fold(text.chars(), self.match_case);
                let n = needle.len();
                let mut found = Vec::new();
                let mut at = 0;
                while n > 0 && at + n <= hay.len() {
                    if hay[at..at + n] == needle[..] {
                        found.push(at..at + n);
                        at += n;
                    } else {
                        at += 1;
                    }
                }
                found
            }
            Matcher::Regex(re) => {
                // Byte offset → char index.
                let mut char_at = vec![0usize; text.len() + 1];
                let mut count = 0;
                for (i, (b, _)) in text.char_indices().enumerate() {
                    char_at[b] = i;
                    count = i + 1;
                }
                char_at[text.len()] = count;
                re.find_iter(text)
                    .flatten()
                    .filter(|m| m.start() < m.end())
                    .map(|m| char_at[m.start()]..char_at[m.end()])
                    .collect()
            }
        }
    }

    /// Whether `text` holds a match ending after char `after`.
    pub fn matches_after(&self, text: &str, after: usize) -> bool {
        self.find(text).iter().any(|r| r.end > after)
    }
}

/// One char per char (the first of its lower case), so positions stay
/// aligned with the text.
fn fold(chars: impl Iterator<Item = char>, match_case: bool) -> Vec<char> {
    if match_case {
        chars.collect()
    } else {
        chars
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect()
    }
}

/// A match: from `start` to `end` (inclusive), each a combined index and a
/// column, in the line that spans the rows `line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub start: (usize, usize),
    pub end: (usize, usize),
    /// The rows of the whole (soft-wrapped) line the match is in.
    pub line: Range<usize>,
}

impl Hit {
    /// The columns of row `index` the match covers (inclusive), if any.
    pub fn cols_in(&self, index: usize) -> Option<(usize, usize)> {
        if index < self.start.0 || index > self.end.0 {
            return None;
        }
        let from = if index == self.start.0 {
            self.start.1
        } else {
            0
        };
        let to = if index == self.end.0 {
            self.end.1
        } else {
            usize::MAX
        };
        Some((from, to))
    }
}

/// Whether row `index` goes on in the next one (soft wrap).
fn wraps(screen: &Screen, index: usize) -> bool {
    screen
        .line_at(index)
        .and_then(|l| l.last())
        .is_some_and(|c| c.is_wrapped())
}

/// The first row of the line row `index` is part of.
fn line_start(screen: &Screen, mut index: usize) -> usize {
    while index > 0 && wraps(screen, index - 1) {
        index -= 1;
    }
    index
}

/// The matches of `pattern` in the lines that touch the rows `rows`, in
/// order.
pub fn search(screen: &Screen, pattern: &Pattern, rows: Range<usize>) -> Vec<Hit> {
    let total = screen.total_lines();
    let end = rows.end.min(total);
    if rows.start >= end {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let mut index = line_start(screen, rows.start);
    let mut text = String::new();
    // Where each char of `text` is: row and column, and the last column it
    // covers (a wide char takes two).
    let mut at: Vec<(usize, usize, usize)> = Vec::new();
    while index < total {
        let first = index;
        text.clear();
        at.clear();
        while let Some(cells) = screen.line_at(index) {
            for (col, cell) in cells.iter().enumerate() {
                if cell.is_spacer() {
                    continue;
                }
                text.push(cell.ch);
                let last = if cell.is_wide() { col + 1 } else { col };
                at.push((index, col, last));
            }
            let more = wraps(screen, index);
            index += 1;
            if !more || index >= total {
                break;
            }
        }
        // The blanks that fill the last row are not part of the line.
        let kept = text.trim_end_matches(' ').chars().count();
        if kept > 0 {
            let line = first..index;
            let text: String = text.chars().take(kept).collect();
            for r in pattern.find(&text) {
                let (sr, sc, _) = at[r.start];
                let (er, _, ec) = at[r.end - 1];
                hits.push(Hit {
                    start: (sr, sc),
                    end: (er, ec),
                    line: line.clone(),
                });
            }
        }
        if index >= end {
            break;
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Terminal;

    fn pattern(text: &str, regex: bool, case: bool) -> Pattern {
        Pattern::new(text, regex, case).unwrap().unwrap()
    }

    #[test]
    fn text_anywhere_and_case() {
        let p = pattern("err", false, false);
        assert_eq!(p.find("ERROR: no error"), [0..3, 10..13]);
        let p = pattern("err", false, true);
        assert_eq!(p.find("ERROR: no error"), vec![10..13]);
        assert!(Pattern::new("", false, false).unwrap().is_none());
        assert!(Pattern::new("(open", true, false).is_err());
    }

    #[test]
    fn regex_char_positions() {
        let p = pattern(r"\d+ms", true, false);
        assert_eq!(p.find("ação 12ms e 7MS"), [5..9, 12..15]);
        assert!(p.matches_after("x 12ms", 3));
        assert!(!p.matches_after("x 12ms", 6));
    }

    #[test]
    fn finds_across_history_and_wrapped_rows() {
        let mut t = Terminal::with_scrollback(10, 3, 100);
        t.feed(b"one error\r\ntwo\r\nlong line with error\r\nlast");
        let s = t.screen();
        let hits = search(s, &pattern("error", false, false), 0..s.total_lines());
        // "one error" on its own row; "long line with error" wraps onto a
        // second row of ten, which holds "with error".
        assert_eq!(hits.len(), 2, "{hits:?}");
        let first = &hits[0];
        assert_eq!((first.start, first.end), ((0, 4), (0, 8)));
        let second = &hits[1];
        assert_eq!(second.line.len(), 2);
        assert_eq!(s.line_at(second.start.0).unwrap()[second.start.1].ch, 'e');
        assert_eq!(second.end.0, second.start.0);
    }

    #[test]
    fn a_match_split_by_the_wrap() {
        let mut t = Terminal::with_scrollback(6, 3, 100);
        t.feed(b"abcdefghij");
        let s = t.screen();
        let hits = search(s, &pattern("efg", false, false), 0..s.total_lines());
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        let first = h.start.0;
        assert_eq!((h.start, h.end), ((first, 4), (first + 1, 0)));
        assert_eq!(h.cols_in(first), Some((4, usize::MAX)));
        assert_eq!(h.cols_in(first + 1), Some((0, 0)));
    }
}
