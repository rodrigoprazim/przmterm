//! Terminal emulation for PrzmTerm.
//!
//! [`Terminal`] feeds raw bytes from a transport through the `vte` escape
//! sequence parser into a [`Screen`]: a grid of styled cells with a cursor,
//! a scrollback buffer and an alternate screen. Replies the remote side asks
//! for (cursor position reports, device attributes) are queued and collected
//! with [`Terminal::take_output`].
//!
//! Coverage in 0.1: VT100/VT220/xterm cursor movement, erase, insert/delete,
//! scroll regions, SGR with 16/256/truecolor, the alternate screen, DECCKM and
//! bracketed paste flags. Wide (CJK) characters and line-drawing charsets come
//! in a later version (see ROADMAP.md).

use std::collections::VecDeque;

use vte::{Params, Parser, Perform};

/// Default number of lines kept in scrollback.
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// A cell colour as the remote side requested it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// The theme's default foreground or background.
    #[default]
    Default,
    /// One of the 256 xterm palette entries.
    Indexed(u8),
    /// 24-bit truecolor.
    Rgb(u8, u8, u8),
}

impl Color {
    /// Resolves the colour to RGB, using `default` for [`Color::Default`].
    pub fn to_rgb(self, default: [u8; 3]) -> [u8; 3] {
        match self {
            Color::Default => default,
            Color::Indexed(i) => xterm_rgb(i),
            Color::Rgb(r, g, b) => [r, g, b],
        }
    }
}

/// RGB value of an xterm 256-colour palette entry.
pub fn xterm_rgb(index: u8) -> [u8; 3] {
    const BASE: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    match index {
        0..=15 => BASE[index as usize],
        16..=231 => {
            let n = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            [level(n / 36), level((n / 6) % 6), level(n % 6)]
        }
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            [v, v, v]
        }
    }
}

/// Graphic rendition of a cell (SGR state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

/// One character cell of the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: Style::default(),
        }
    }
}

/// Cursor position, zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Default)]
struct Saved {
    cursor: Cursor,
    pen: Style,
}

#[derive(Debug, Clone)]
struct Primary {
    lines: Vec<Vec<Cell>>,
    saved: Saved,
}

/// The visible grid plus everything that shapes how bytes land on it.
#[derive(Debug, Clone)]
pub struct Screen {
    cols: usize,
    rows: usize,
    lines: Vec<Vec<Cell>>,
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_limit: usize,
    cursor: Cursor,
    saved: Saved,
    pen: Style,
    wrap_pending: bool,
    autowrap: bool,
    cursor_visible: bool,
    app_cursor_keys: bool,
    bracketed_paste: bool,
    scroll_top: usize,
    scroll_bottom: usize,
    primary: Option<Primary>,
    title: String,
    bell: bool,
    output: Vec<u8>,
}

impl Screen {
    fn new(cols: usize, rows: usize, scrollback_limit: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            lines: vec![vec![Cell::default(); cols]; rows],
            scrollback: VecDeque::new(),
            scrollback_limit,
            cursor: Cursor::default(),
            saved: Saved::default(),
            pen: Style::default(),
            wrap_pending: false,
            autowrap: true,
            cursor_visible: true,
            app_cursor_keys: false,
            bracketed_paste: false,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            primary: None,
            title: String::new(),
            bell: false,
            output: Vec::new(),
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Cells of a visible row. Panics if `row >= rows()`.
    pub fn line(&self, row: usize) -> &[Cell] {
        &self.lines[row]
    }

    /// Text of a visible row with trailing blanks removed.
    pub fn line_text(&self, row: usize) -> String {
        cells_text(&self.lines[row])
    }

    /// All visible rows as text, one per line.
    pub fn text(&self) -> String {
        (0..self.rows)
            .map(|r| self.line_text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Window title set by the remote side (OSC 0 / OSC 2).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Whether arrow keys must be sent in application mode (`ESC O A`).
    pub fn app_cursor_keys(&self) -> bool {
        self.app_cursor_keys
    }

    /// Whether pasted text must be wrapped in `ESC [200~` … `ESC [201~`.
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub fn in_alt_screen(&self) -> bool {
        self.primary.is_some()
    }

    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// A scrollback line, 0 = oldest.
    pub fn scrollback_line(&self, index: usize) -> Option<&[Cell]> {
        self.scrollback.get(index).map(Vec::as_slice)
    }

    /// Returns `true` once after the remote side rang the bell.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell)
    }

    fn blank_cell(&self) -> Cell {
        Cell {
            ch: ' ',
            style: Style {
                bg: self.pen.bg,
                ..Style::default()
            },
        }
    }

    fn blank_line(&self) -> Vec<Cell> {
        vec![self.blank_cell(); self.cols]
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        for line in &mut self.lines {
            line.resize(cols, Cell::default());
        }
        for line in &mut self.scrollback {
            line.resize(cols, Cell::default());
        }
        while self.lines.len() > rows {
            if self.cursor.row > 0 {
                let line = self.lines.remove(0);
                if self.primary.is_none() {
                    self.push_scrollback(line);
                }
                self.cursor.row -= 1;
            } else {
                self.lines.pop();
            }
        }
        while self.lines.len() < rows {
            self.lines.push(vec![Cell::default(); cols]);
        }
        if let Some(primary) = &mut self.primary {
            for line in &mut primary.lines {
                line.resize(cols, Cell::default());
            }
            primary.lines.resize(rows, vec![Cell::default(); cols]);
        }
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.cursor.row = self.cursor.row.min(rows - 1);
        self.cursor.col = self.cursor.col.min(cols - 1);
        self.wrap_pending = false;
    }

    fn push_scrollback(&mut self, line: Vec<Cell>) {
        if self.scrollback_limit == 0 {
            return;
        }
        if self.scrollback.len() == self.scrollback_limit {
            self.scrollback.pop_front();
        }
        self.scrollback.push_back(line);
    }

    fn put_char(&mut self, ch: char) {
        if self.wrap_pending {
            self.cursor.col = 0;
            self.linefeed();
            self.wrap_pending = false;
        }
        let pen = self.pen;
        let Cursor { row, col } = self.cursor;
        self.lines[row][col] = Cell { ch, style: pen };
        if col + 1 >= self.cols {
            self.wrap_pending = self.autowrap;
        } else {
            self.cursor.col += 1;
        }
    }

    fn linefeed(&mut self) {
        if self.cursor.row == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.cursor.row == self.scroll_top {
            self.scroll_down(1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        for _ in 0..n {
            let line = self.lines.remove(self.scroll_top);
            if self.scroll_top == 0 && self.primary.is_none() {
                self.push_scrollback(line);
            }
            let blank = self.blank_line();
            self.lines.insert(self.scroll_bottom, blank);
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let n = n.min(self.scroll_bottom - self.scroll_top + 1);
        for _ in 0..n {
            self.lines.remove(self.scroll_bottom);
            let blank = self.blank_line();
            self.lines.insert(self.scroll_top, blank);
        }
    }

    fn move_to(&mut self, row: usize, col: usize) {
        self.cursor.row = row.min(self.rows - 1);
        self.cursor.col = col.min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn erase_cells(&mut self, row: usize, from: usize, to: usize) {
        let blank = self.blank_cell();
        let to = to.min(self.cols);
        for cell in &mut self.lines[row][from.min(to)..to] {
            *cell = blank;
        }
    }

    fn erase_display(&mut self, mode: usize) {
        let Cursor { row, col } = self.cursor;
        match mode {
            0 => {
                self.erase_cells(row, col, self.cols);
                for r in row + 1..self.rows {
                    self.erase_cells(r, 0, self.cols);
                }
            }
            1 => {
                for r in 0..row {
                    self.erase_cells(r, 0, self.cols);
                }
                self.erase_cells(row, 0, col + 1);
            }
            2 => {
                for r in 0..self.rows {
                    self.erase_cells(r, 0, self.cols);
                }
            }
            3 => self.scrollback.clear(),
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let Cursor { row, col } = self.cursor;
        match mode {
            0 => self.erase_cells(row, col, self.cols),
            1 => self.erase_cells(row, 0, col + 1),
            2 => self.erase_cells(row, 0, self.cols),
            _ => {}
        }
    }

    fn insert_lines(&mut self, n: usize) {
        let row = self.cursor.row;
        if row < self.scroll_top || row > self.scroll_bottom {
            return;
        }
        for _ in 0..n.min(self.scroll_bottom - row + 1) {
            self.lines.remove(self.scroll_bottom);
            let blank = self.blank_line();
            self.lines.insert(row, blank);
        }
        self.cursor.col = 0;
    }

    fn delete_lines(&mut self, n: usize) {
        let row = self.cursor.row;
        if row < self.scroll_top || row > self.scroll_bottom {
            return;
        }
        for _ in 0..n.min(self.scroll_bottom - row + 1) {
            self.lines.remove(row);
            let blank = self.blank_line();
            self.lines.insert(self.scroll_bottom, blank);
        }
        self.cursor.col = 0;
    }

    fn insert_chars(&mut self, n: usize) {
        let Cursor { row, col } = self.cursor;
        let blank = self.blank_cell();
        let line = &mut self.lines[row];
        for _ in 0..n.min(self.cols - col) {
            line.pop();
            line.insert(col, blank);
        }
    }

    fn delete_chars(&mut self, n: usize) {
        let Cursor { row, col } = self.cursor;
        let blank = self.blank_cell();
        let line = &mut self.lines[row];
        for _ in 0..n.min(self.cols - col) {
            line.remove(col);
            line.push(blank);
        }
    }

    fn save_cursor(&mut self) {
        self.saved = Saved {
            cursor: self.cursor,
            pen: self.pen,
        };
    }

    fn restore_cursor(&mut self) {
        let Saved { cursor, pen } = self.saved.clone();
        self.pen = pen;
        self.move_to(cursor.row, cursor.col);
    }

    fn enter_alt_screen(&mut self) {
        if self.primary.is_some() {
            return;
        }
        self.save_cursor();
        let blank = vec![vec![Cell::default(); self.cols]; self.rows];
        let lines = std::mem::replace(&mut self.lines, blank);
        self.primary = Some(Primary {
            lines,
            saved: self.saved.clone(),
        });
    }

    fn leave_alt_screen(&mut self) {
        if let Some(primary) = self.primary.take() {
            self.lines = primary.lines;
            self.saved = primary.saved;
            self.restore_cursor();
        }
    }

    fn reset(&mut self) {
        let fresh = Screen::new(self.cols, self.rows, self.scrollback_limit);
        let scrollback = std::mem::take(&mut self.scrollback);
        *self = fresh;
        self.scrollback = scrollback;
    }

    fn set_mode(&mut self, params: &[u16], private: bool, on: bool) {
        for &mode in params {
            match (private, mode) {
                (true, 1) => self.app_cursor_keys = on,
                (true, 7) => self.autowrap = on,
                (true, 25) => self.cursor_visible = on,
                (true, 47 | 1047 | 1049) => {
                    if on {
                        self.enter_alt_screen();
                    } else {
                        self.leave_alt_screen();
                    }
                }
                (true, 2004) => self.bracketed_paste = on,
                _ => {}
            }
        }
    }

    fn sgr(&mut self, params: &Params) {
        let groups: Vec<&[u16]> = params.iter().collect();
        if groups.is_empty() {
            self.pen = Style::default();
            return;
        }
        let mut i = 0;
        while i < groups.len() {
            let group = groups[i];
            let code = group[0];
            match code {
                0 => self.pen = Style::default(),
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                3 => self.pen.italic = true,
                4 => self.pen.underline = true,
                7 => self.pen.reverse = true,
                22 => {
                    self.pen.bold = false;
                    self.pen.dim = false;
                }
                23 => self.pen.italic = false,
                24 => self.pen.underline = false,
                27 => self.pen.reverse = false,
                30..=37 => self.pen.fg = Color::Indexed((code - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((code - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((code - 100 + 8) as u8),
                38 | 48 => {
                    // Colon form keeps the colour in one group (38:2:r:g:b);
                    // semicolon form spreads it over the following groups.
                    let (spec, used): (Vec<u16>, usize) = if group.len() > 1 {
                        (group[1..].to_vec(), 0)
                    } else {
                        let rest: Vec<u16> = groups[i + 1..].iter().map(|g| g[0]).collect();
                        let used = match rest.first() {
                            Some(5) => 2,
                            Some(2) => 4,
                            _ => 0,
                        };
                        (rest, used)
                    };
                    let color = match spec.as_slice() {
                        [5, n, ..] => Some(Color::Indexed(*n as u8)),
                        // Colon form may carry a colour-space id: 38:2::r:g:b.
                        [2, _, r, g, b] if group.len() > 1 => {
                            Some(Color::Rgb(*r as u8, *g as u8, *b as u8))
                        }
                        [2, r, g, b, ..] => Some(Color::Rgb(*r as u8, *g as u8, *b as u8)),
                        _ => None,
                    };
                    if let Some(color) = color {
                        if code == 38 {
                            self.pen.fg = color;
                        } else {
                            self.pen.bg = color;
                        }
                    }
                    i += used.min(groups.len() - i - 1);
                }
                _ => {}
            }
            i += 1;
        }
    }
}

fn cells_text(cells: &[Cell]) -> String {
    let text: String = cells.iter().map(|c| c.ch).collect();
    text.trim_end().to_string()
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.bell = true,
            0x08 => {
                self.cursor.col = self.cursor.col.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => {
                let next = (self.cursor.col / 8 + 1) * 8;
                self.cursor.col = next.min(self.cols - 1);
            }
            0x0A..=0x0C => {
                self.linefeed();
                self.wrap_pending = false;
            }
            0x0D => {
                self.cursor.col = 0;
                self.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let private = intermediates.first() == Some(&b'?');
        let raw: Vec<u16> = params.iter().map(|g| g[0]).collect();
        // Missing or zero parameters mean "use the default".
        let arg = |i: usize, default: usize| -> usize {
            match raw.get(i) {
                Some(&v) if v != 0 => v as usize,
                _ => default,
            }
        };
        let Cursor { row, col } = self.cursor;
        match (action, private) {
            ('A', false) => self.move_to(row.saturating_sub(arg(0, 1)), col),
            ('B' | 'e', false) => self.move_to(row + arg(0, 1), col),
            ('C' | 'a', false) => self.move_to(row, col + arg(0, 1)),
            ('D', false) => self.move_to(row, col.saturating_sub(arg(0, 1))),
            ('E', false) => self.move_to(row + arg(0, 1), 0),
            ('F', false) => self.move_to(row.saturating_sub(arg(0, 1)), 0),
            ('G' | '`', false) => self.move_to(row, arg(0, 1) - 1),
            ('d', false) => self.move_to(arg(0, 1) - 1, col),
            ('H' | 'f', false) => self.move_to(arg(0, 1) - 1, arg(1, 1) - 1),
            ('J', _) => self.erase_display(raw.first().copied().unwrap_or(0) as usize),
            ('K', _) => self.erase_line(raw.first().copied().unwrap_or(0) as usize),
            ('L', false) => self.insert_lines(arg(0, 1)),
            ('M', false) => self.delete_lines(arg(0, 1)),
            ('@', false) => self.insert_chars(arg(0, 1)),
            ('P', false) => self.delete_chars(arg(0, 1)),
            ('X', false) => self.erase_cells(row, col, col + arg(0, 1)),
            ('S', false) => self.scroll_up(arg(0, 1)),
            ('T', false) => self.scroll_down(arg(0, 1)),
            ('m', false) => self.sgr(params),
            ('r', false) => {
                let top = arg(0, 1) - 1;
                let bottom = arg(1, self.rows).min(self.rows) - 1;
                if top < bottom {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.move_to(0, 0);
                }
            }
            ('s', false) => self.save_cursor(),
            ('u', false) => self.restore_cursor(),
            ('h', _) => self.set_mode(&raw, private, true),
            ('l', _) => self.set_mode(&raw, private, false),
            ('n', false) => match arg(0, 0) {
                5 => self.output.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let reply = format!("\x1b[{};{}R", row + 1, col + 1);
                    self.output.extend_from_slice(reply.as_bytes());
                }
                _ => {}
            },
            ('c', false) if intermediates.is_empty() && arg(0, 0) == 0 => {
                // Primary device attributes: VT100 with advanced video option.
                self.output.extend_from_slice(b"\x1b[?1;2c");
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            // Charset designations (ESC ( 0 …) arrive with intermediates; not yet supported.
            return;
        }
        match byte {
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'D' => self.linefeed(),
            b'E' => {
                self.cursor.col = 0;
                self.linefeed();
            }
            b'M' => self.reverse_index(),
            b'c' => self.reset(),
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if let [kind, title, ..] = params
            && matches!(*kind, b"0" | b"2")
        {
            self.title = String::from_utf8_lossy(title).into_owned();
        }
    }
}

/// A terminal: escape-sequence parser plus the screen it draws on.
pub struct Terminal {
    parser: Parser,
    screen: Screen,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self::with_scrollback(cols, rows, DEFAULT_SCROLLBACK)
    }

    pub fn with_scrollback(cols: usize, rows: usize, scrollback_limit: usize) -> Self {
        Self {
            parser: Parser::new(),
            screen: Screen::new(cols, rows, scrollback_limit),
        }
    }

    /// Feeds bytes received from the remote side.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.screen, bytes);
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.screen.resize(cols, rows);
    }

    /// Bytes the terminal must send back to the remote side (DSR, DA replies).
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.screen.output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: usize, rows: usize, input: &str) -> Terminal {
        let mut t = Terminal::with_scrollback(cols, rows, 100);
        t.feed(input.as_bytes());
        t
    }

    #[test]
    fn prints_and_moves_with_cr_lf() {
        let t = term(10, 3, "ab\r\ncd");
        assert_eq!(t.screen().line_text(0), "ab");
        assert_eq!(t.screen().line_text(1), "cd");
        assert_eq!(t.screen().cursor(), Cursor { row: 1, col: 2 });
    }

    #[test]
    fn wraps_at_last_column() {
        let t = term(4, 3, "abcdef");
        assert_eq!(t.screen().line_text(0), "abcd");
        assert_eq!(t.screen().line_text(1), "ef");
    }

    #[test]
    fn last_column_waits_before_wrapping() {
        let t = term(4, 3, "abcd\r\nx");
        assert_eq!(t.screen().line_text(0), "abcd");
        assert_eq!(t.screen().line_text(1), "x");
    }

    #[test]
    fn scrolls_into_bounded_scrollback() {
        let mut t = Terminal::with_scrollback(5, 2, 2);
        t.feed(b"1\r\n2\r\n3\r\n4\r\n5");
        let s = t.screen();
        assert_eq!(s.text(), "4\n5");
        assert_eq!(s.scrollback_len(), 2);
        assert_eq!(cells_text(s.scrollback_line(0).unwrap()), "2");
        assert_eq!(cells_text(s.scrollback_line(1).unwrap()), "3");
    }

    #[test]
    fn cursor_position_and_erase() {
        let t = term(5, 3, "aaaaa\r\nbbbbb\r\nccccc\x1b[2;3H\x1b[K");
        assert_eq!(t.screen().text(), "aaaaa\nbb\nccccc");
        let t = term(5, 3, "aaaaa\r\nbbbbb\x1b[2J");
        assert_eq!(t.screen().text(), "\n\n");
    }

    #[test]
    fn insert_and_delete_characters() {
        let t = term(6, 1, "abcdef\x1b[1;2H\x1b[2P");
        assert_eq!(t.screen().line_text(0), "adef");
        let t = term(6, 1, "abcdef\x1b[1;2H\x1b[2@");
        assert_eq!(t.screen().line_text(0), "a  bcd");
    }

    #[test]
    fn scroll_region_keeps_lines_outside() {
        let t = term(3, 4, "top\r\nr1\r\nr2\r\nbot\x1b[2;3r\x1b[3;1H\n");
        assert_eq!(t.screen().text(), "top\nr2\n\nbot");
        assert_eq!(t.screen().scrollback_len(), 0);
    }

    #[test]
    fn sgr_colours_and_attributes() {
        let t = term(
            10,
            1,
            "\x1b[1;31ma\x1b[38;5;196mb\x1b[38;2;1;2;3;48;5;4mc\x1b[0;7md",
        );
        let line = t.screen().line(0);
        assert_eq!(line[0].style.fg, Color::Indexed(1));
        assert!(line[0].style.bold);
        assert_eq!(line[1].style.fg, Color::Indexed(196));
        assert_eq!(line[2].style.fg, Color::Rgb(1, 2, 3));
        assert_eq!(line[2].style.bg, Color::Indexed(4));
        assert_eq!(line[3].style.fg, Color::Default);
        assert!(line[3].style.reverse && !line[3].style.bold);
    }

    #[test]
    fn sgr_colon_form() {
        let t = term(10, 1, "\x1b[38:2::10:20:30ma\x1b[48:5:9mb");
        let line = t.screen().line(0);
        assert_eq!(line[0].style.fg, Color::Rgb(10, 20, 30));
        assert_eq!(line[1].style.bg, Color::Indexed(9));
    }

    #[test]
    fn osc_sets_title() {
        let t = term(10, 1, "\x1b]0;router-01\x07");
        assert_eq!(t.screen().title(), "router-01");
    }

    #[test]
    fn answers_cursor_position_and_device_attributes() {
        let mut t = term(10, 5, "\x1b[3;4H\x1b[6n\x1b[c");
        assert_eq!(t.take_output(), b"\x1b[3;4R\x1b[?1;2c");
        assert!(t.take_output().is_empty());
    }

    #[test]
    fn alternate_screen_restores_primary() {
        let mut t = term(10, 2, "shell$ ");
        t.feed(b"\x1b[?1049h\x1b[Hvim");
        assert!(t.screen().in_alt_screen());
        assert_eq!(t.screen().line_text(0), "vim");
        t.feed(b"\x1b[?1049l");
        assert!(!t.screen().in_alt_screen());
        assert_eq!(t.screen().line_text(0), "shell$");
        assert_eq!(t.screen().cursor(), Cursor { row: 0, col: 7 });
    }

    #[test]
    fn mode_flags() {
        let t = term(10, 2, "\x1b[?1h\x1b[?2004h\x1b[?25l");
        assert!(t.screen().app_cursor_keys());
        assert!(t.screen().bracketed_paste());
        assert!(!t.screen().cursor_visible());
    }

    #[test]
    fn shrinking_moves_top_lines_to_scrollback() {
        let mut t = term(5, 4, "a\r\nb\r\nc\r\nd");
        t.resize(3, 2);
        assert_eq!(t.screen().text(), "c\nd");
        assert_eq!(t.screen().scrollback_len(), 2);
        assert_eq!(t.screen().cursor(), Cursor { row: 1, col: 1 });
    }

    #[test]
    fn xterm_palette() {
        assert_eq!(xterm_rgb(1), [205, 0, 0]);
        assert_eq!(xterm_rgb(16), [0, 0, 0]);
        assert_eq!(xterm_rgb(196), [255, 0, 0]);
        assert_eq!(xterm_rgb(231), [255, 255, 255]);
        assert_eq!(xterm_rgb(232), [8, 8, 8]);
        assert_eq!(Color::Default.to_rgb([1, 2, 3]), [1, 2, 3]);
    }
}
