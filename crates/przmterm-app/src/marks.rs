//! What a pane marks in its text: the session's own highlights (set for
//! this session only, while it is open) and the matches of a search.
//!
//! Both are painted over everything else, the program's own colours
//! included: a highlight is there to be seen.

use std::rc::Rc;
use std::time::{Duration, Instant};

use przmterm_core::keywords::{Keyword, KeywordSet, format_color};
use przmterm_term::search::{self, Hit, Pattern};
use przmterm_term::{Screen, Terminal};

/// Marker colours (neon, to stand out in a log), in the order new
/// highlights take them.
pub const COLORS: [(&str, [u8; 3]); 8] = [
    ("Yellow", [0xff, 0xf0, 0x1f]),
    ("Green", [0x39, 0xff, 0x14]),
    ("Cyan", [0x00, 0xf0, 0xff]),
    ("Pink", [0xff, 0x2e, 0xb4]),
    ("Orange", [0xff, 0x8c, 0x1a]),
    ("Purple", [0xbc, 0x13, 0xfe]),
    ("Red", [0xff, 0x31, 0x31]),
    ("Blue", [0x1f, 0x51, 0xff]),
];

/// The matches of a search, and the one being looked at.
pub const FOUND: [u8; 3] = [0x8a, 0x6d, 0x1f];
pub const FOUND_CURRENT: [u8; 3] = [0xff, 0x9f, 0x1c];

/// Work over the whole history is redone at most this often while output
/// comes in.
const REFRESH: Duration = Duration::from_millis(300);

/// Text that reads on `bg`: near black on light colours, white on dark.
pub fn contrast([r, g, b]: [u8; 3]) -> [u8; 3] {
    let lum = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
    if lum > 140.0 {
        [0x14, 0x14, 0x14]
    } else {
        [0xff, 0xff, 0xff]
    }
}

/// One highlight of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub text: String,
    pub regex: bool,
    pub match_case: bool,
    pub color: [u8; 3],
    /// The whole line, not only the text.
    pub whole_line: bool,
    pub on: bool,
    /// Mark the tab when the text turns up while it is not looked at.
    pub alert: bool,
}

impl Rule {
    pub fn new(text: &str, regex: bool, match_case: bool, color: [u8; 3]) -> Rule {
        Rule {
            text: text.to_string(),
            regex,
            match_case,
            color,
            whole_line: false,
            on: true,
            alert: true,
        }
    }
}

/// A session's highlights, compiled.
#[derive(Default)]
pub struct Highlights {
    rules: Vec<Rule>,
    compiled: Vec<Result<Option<Pattern>, String>>,
    /// Changes with every change, so work done with them can tell.
    version: u64,
}

impl Highlights {
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn set(&mut self, rules: Vec<Rule>) {
        if rules == self.rules {
            return;
        }
        self.compiled = rules
            .iter()
            .map(|r| Pattern::new(&r.text, r.regex, r.match_case))
            .collect();
        self.rules = rules;
        self.version += 1;
    }

    pub fn add(&mut self, rule: Rule) {
        let mut rules = self.rules.clone();
        rules.push(rule);
        self.set(rules);
    }

    /// Why rule `i` cannot be used (a regular expression that does not
    /// compile).
    pub fn error(&self, i: usize) -> Option<&str> {
        self.compiled.get(i)?.as_ref().err().map(String::as_str)
    }

    /// The rules switched on that can be used.
    pub fn active(&self) -> impl Iterator<Item = (&Rule, &Pattern)> {
        self.rules
            .iter()
            .zip(&self.compiled)
            .filter(|(r, _)| r.on)
            .filter_map(|(r, p)| Some((r, p.as_ref().ok()?.as_ref()?)))
    }

    /// How many are switched on and usable.
    pub fn count(&self) -> usize {
        self.active().count()
    }

    /// The colour a new highlight takes: the first not in use.
    pub fn next_color(&self) -> [u8; 3] {
        COLORS
            .iter()
            .map(|(_, c)| *c)
            .find(|c| !self.rules.iter().any(|r| r.color == *c))
            .unwrap_or(COLORS[self.rules.len() % COLORS.len()].1)
    }

    fn alerting(&self) -> bool {
        self.active().any(|(r, _)| r.alert)
    }
}

/// Cells to paint: a combined row index, its columns (inclusive; `MAX`
/// to the end) and the colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paint {
    pub index: usize,
    pub cols: (usize, usize),
    pub bg: [u8; 3],
    pub fg: [u8; 3],
}

/// The paint of the highlights on the rows `rows`: whole lines first, so
/// marked text shows on a marked line.
pub fn highlight_paints(
    screen: &Screen,
    highlights: &Highlights,
    rows: std::ops::Range<usize>,
) -> Vec<Paint> {
    let mut lines = Vec::new();
    let mut texts = Vec::new();
    for (rule, pattern) in highlights.active() {
        let fg = contrast(rule.color);
        let mut last_line = None;
        for hit in search::search(screen, pattern, rows.clone()) {
            if rule.whole_line {
                if last_line == Some(hit.line.start) {
                    continue;
                }
                last_line = Some(hit.line.start);
                for index in hit.line.clone() {
                    if rows.contains(&index) {
                        lines.push(Paint {
                            index,
                            cols: (0, usize::MAX),
                            bg: rule.color,
                            fg,
                        });
                    }
                }
            } else {
                for index in hit.start.0..=hit.end.0 {
                    if let (true, Some(cols)) = (rows.contains(&index), hit.cols_in(index)) {
                        texts.push(Paint {
                            index,
                            cols,
                            bg: rule.color,
                            fg,
                        });
                    }
                }
            }
        }
    }
    lines.extend(texts);
    lines
}

/// Cells framed: a combined row index and its columns (inclusive).
pub type Frame = (usize, (usize, usize));

/// The matches of a search in one pane.
#[derive(Default)]
pub struct Found {
    pattern: Option<Rc<Pattern>>,
    hits: Vec<Hit>,
    /// The history base the hits were found at.
    base: u64,
    /// The contents they were found in.
    generation: u64,
    at: Option<Instant>,
    /// The match looked at: its line (with the history base) and column.
    current: Option<(u64, usize)>,
}

impl Found {
    pub fn searching(&self) -> bool {
        self.pattern.is_some()
    }

    /// Searches for `pattern` from now on (`None`: stops).
    pub fn set_pattern(&mut self, pattern: Option<Rc<Pattern>>) {
        let same = match (&self.pattern, &pattern) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.pattern = pattern;
            self.hits.clear();
            self.current = None;
            self.at = None;
        }
    }

    /// Finds the matches again if the contents changed, at most every
    /// [`REFRESH`] (a repaint is asked for when it waits).
    pub fn refresh(&mut self, term: &Terminal, ctx: &eframe::egui::Context) {
        let Some(pattern) = &self.pattern else {
            return;
        };
        let fresh = self.at.is_none();
        if !fresh && self.generation == term.generation() {
            return;
        }
        if let Some(at) = self.at
            && at.elapsed() < REFRESH
        {
            ctx.request_repaint_after(REFRESH - at.elapsed());
            return;
        }
        let screen = term.screen();
        self.hits = search::search(screen, pattern, 0..screen.total_lines());
        self.base = screen.history_base();
        self.generation = term.generation();
        self.at = Some(Instant::now());
    }

    /// How far the history moved since the hits were found.
    fn shift(&self, screen: &Screen) -> usize {
        screen.history_base().saturating_sub(self.base) as usize
    }

    /// The hits as of now: (row of the start, the hit as found).
    pub fn hits<'a>(&'a self, screen: &Screen) -> impl Iterator<Item = (usize, &'a Hit)> + 'a {
        let shift = self.shift(screen);
        self.hits
            .iter()
            .filter_map(move |h| Some((h.start.0.checked_sub(shift)?, h)))
    }

    pub fn len(&self) -> usize {
        self.hits.len()
    }

    /// Which hit is the current one.
    pub fn current(&self) -> Option<usize> {
        let (line, col) = self.current?;
        self.hits
            .iter()
            .position(|h| self.base + h.start.0 as u64 == line && h.start.1 == col)
    }

    /// Makes hit `i` the current one (`None`: none here).
    pub fn set_current(&mut self, i: Option<usize>) {
        self.current = i
            .and_then(|i| self.hits.get(i))
            .map(|h| (self.base + h.start.0 as u64, h.start.1));
    }

    /// The paint of the matches on the rows `rows`; the current one also
    /// as a frame: its first row and columns.
    pub fn paints(
        &self,
        screen: &Screen,
        rows: std::ops::Range<usize>,
    ) -> (Vec<Paint>, Vec<Frame>) {
        let shift = self.shift(screen);
        let current = self.current();
        let mut paints = Vec::new();
        let mut frame = Vec::new();
        for (i, hit) in self.hits.iter().enumerate() {
            let is_current = current == Some(i);
            let bg = if is_current { FOUND_CURRENT } else { FOUND };
            for index in hit.start.0..=hit.end.0 {
                let Some(now) = index.checked_sub(shift) else {
                    continue;
                };
                if !rows.contains(&now) {
                    continue;
                }
                if let Some(cols) = hit.cols_in(index) {
                    paints.push(Paint {
                        index: now,
                        cols,
                        bg,
                        fg: contrast(bg),
                    });
                    if is_current {
                        frame.push((now, cols));
                    }
                }
            }
        }
        (paints, frame)
    }

    /// The row the current hit starts on, as of now.
    pub fn current_row(&self, screen: &Screen) -> Option<usize> {
        let hit = self.hits.get(self.current()?)?;
        hit.start.0.checked_sub(self.shift(screen))
    }
}

/// Everything a pane marks.
#[derive(Default)]
pub struct Marks {
    pub highlights: Highlights,
    pub found: Found,
    /// Highlights in the whole history, for the scroll bar: line (with the
    /// history base) and colour.
    ticks: Vec<(u64, [u8; 3])>,
    /// What the ticks were worked out for: contents and highlights.
    ticks_for: Option<(u64, u64)>,
    ticks_at: Option<Instant>,
}

impl Marks {
    /// Brings the search and the scroll bar's marks up to date.
    pub fn refresh(&mut self, term: &Terminal, ctx: &eframe::egui::Context) {
        self.found.refresh(term, ctx);
        let key = (term.generation(), self.highlights.version);
        if self.ticks_for == Some(key) {
            return;
        }
        let rules_changed = self.ticks_for.map(|(_, v)| v) != Some(key.1);
        if !rules_changed
            && let Some(at) = self.ticks_at
            && at.elapsed() < REFRESH
        {
            ctx.request_repaint_after(REFRESH - at.elapsed());
            return;
        }
        let screen = term.screen();
        let base = screen.history_base();
        self.ticks.clear();
        for (rule, pattern) in self.highlights.active() {
            let mut last = None;
            for hit in search::search(screen, pattern, 0..screen.total_lines()) {
                if last != Some(hit.start.0) {
                    last = Some(hit.start.0);
                    self.ticks.push((base + hit.start.0 as u64, rule.color));
                }
            }
        }
        self.ticks_for = Some(key);
        self.ticks_at = Some(Instant::now());
    }

    /// Where the scroll bar marks something: row (as of now) and colour.
    pub fn ticks(&self, screen: &Screen) -> Vec<(usize, [u8; 3])> {
        let base = screen.history_base();
        let mut out: Vec<(usize, [u8; 3])> = self
            .ticks
            .iter()
            .filter_map(|&(line, color)| Some((line.checked_sub(base)? as usize, color)))
            .collect();
        out.extend(self.found.hits(screen).map(|(row, _)| (row, FOUND_CURRENT)));
        out
    }
}

/// Watches the output for highlights that should mark the tab.
#[derive(Default)]
pub struct Watch {
    /// The line coming in, not ended yet.
    partial: String,
    /// Chars of it already looked at.
    tested: usize,
}

/// Longest unfinished line kept.
const PARTIAL_MAX: usize = 8192;

impl Watch {
    /// Whether output should be turned into text for [`Watch::feed`].
    pub fn wanted(highlights: &Highlights) -> bool {
        highlights.alerting()
    }

    /// Looks at more output (plain text); returns the colour of an alerting
    /// highlight that turned up in it.
    pub fn feed(&mut self, text: &str, highlights: &Highlights) -> Option<[u8; 3]> {
        let mut found = None;
        let check = |line: &str, after: usize, found: &mut Option<[u8; 3]>| {
            if found.is_some() || line.is_empty() {
                return;
            }
            *found = highlights
                .active()
                .find(|(r, p)| r.alert && p.matches_after(line, after))
                .map(|(r, _)| r.color);
        };
        for c in text.chars() {
            if c == '\n' || c == '\r' || self.partial.len() >= PARTIAL_MAX {
                check(&self.partial, self.tested, &mut found);
                self.partial.clear();
                self.tested = 0;
                if c == '\n' || c == '\r' {
                    continue;
                }
            }
            self.partial.push(c);
        }
        check(&self.partial, self.tested, &mut found);
        self.tested = self.partial.chars().count();
        found
    }
}

/// The highlights as a keyword set, to use them in other sessions too:
/// text becomes a regular expression (it matches anywhere, as here), the
/// colour the background.
pub fn to_keyword_set(rules: &[Rule]) -> KeywordSet {
    let rules = rules
        .iter()
        .filter(|r| !r.text.is_empty())
        .map(|r| {
            let mut re = if r.regex {
                r.text.clone()
            } else {
                fancy_regex::escape(&r.text).into_owned()
            };
            if r.whole_line {
                re = format!(".*(?:{re}).*");
            }
            if r.match_case {
                re = format!("(?-i:{re})");
            }
            Keyword {
                regex: re,
                color: Some(format_color(contrast(r.color))),
                background: Some(format_color(r.color)),
                ..Keyword::default()
            }
        })
        .collect();
    KeywordSet {
        match_case: false,
        rules,
        ..KeywordSet::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn highlights(rules: Vec<Rule>) -> Highlights {
        let mut h = Highlights::default();
        h.set(rules);
        h
    }

    #[test]
    fn paints_text_and_whole_lines() {
        let mut t = Terminal::new(20, 3);
        t.feed(b"ok\r\nlink down here\r\nok");
        let mut line = Rule::new("down", false, false, [255, 0, 0]);
        line.whole_line = true;
        let word = Rule::new("here", false, false, [0, 0, 255]);
        let h = highlights(vec![line, word]);
        let s = t.screen();
        let paints = highlight_paints(s, &h, 0..s.total_lines());
        let row = s.scrollback_len() + 1;
        assert_eq!(
            paints,
            [
                Paint {
                    index: row,
                    cols: (0, usize::MAX),
                    bg: [255, 0, 0],
                    fg: [255, 255, 255],
                },
                Paint {
                    index: row,
                    cols: (10, 13),
                    bg: [0, 0, 255],
                    fg: [255, 255, 255],
                },
            ]
        );
    }

    #[test]
    fn rules_off_or_broken_paint_nothing() {
        let mut off = Rule::new("x", false, false, COLORS[0].1);
        off.on = false;
        let broken = Rule::new("(x", true, false, [1, 2, 3]);
        let h = highlights(vec![off, broken]);
        assert_eq!(h.count(), 0);
        assert!(h.error(1).is_some());
        assert_eq!(h.next_color(), COLORS[1].1, "the first colour is taken");
    }

    #[test]
    fn found_follows_the_history() {
        let ctx = eframe::egui::Context::default();
        let mut t = Terminal::with_scrollback(20, 2, 2);
        t.feed(b"a err\r\nb\r\nc err");
        let mut found = Found::default();
        let p = Rc::new(Pattern::new("err", false, false).unwrap().unwrap());
        found.set_pattern(Some(p));
        found.refresh(&t, &ctx);
        assert_eq!(found.len(), 2);
        found.set_current(Some(1));
        assert_eq!(found.current(), Some(1));
        let row = found.current_row(t.screen()).unwrap();
        assert_eq!(t.screen().line_at(row).unwrap()[2].ch, 'e');
    }

    #[test]
    fn alerts_once_per_new_match() {
        let h = highlights(vec![Rule::new("FAIL", false, false, [9, 9, 9])]);
        let mut w = Watch::default();
        assert_eq!(w.feed("all good\r\n", &h), None);
        assert_eq!(w.feed("job fa", &h), None);
        assert_eq!(w.feed("iled", &h), Some([9, 9, 9]), "split across reads");
        assert_eq!(w.feed(" again", &h), None, "the same match");
        assert_eq!(w.feed("\r\n", &h), None);
        let mut off = h.rules()[0].clone();
        off.alert = false;
        let h = highlights(vec![off]);
        assert!(!Watch::wanted(&h));
    }

    #[test]
    fn exports_a_keyword_set_that_matches_the_same() {
        let mut line = Rule::new("a.b", false, true, [0xf2, 0xc9, 0x4c]);
        line.whole_line = true;
        let set = to_keyword_set(&[line]);
        assert_eq!(set.rules[0].regex, r"(?-i:.*(?:a\.b).*)");
        assert_eq!(set.rules[0].background.as_deref(), Some("#f2c94c"));
        let (h, problems) = przmterm_term::highlight::Highlighter::new(&set);
        assert!(problems.is_empty());
        assert_eq!(h.spans("x a.b y").len(), 1);
        assert!(h.spans("x A.B y").is_empty(), "matches case");
        assert!(h.spans("x axb y").is_empty(), "the dot is a dot");
    }
}
