//! The terminal widget: paints a [`Screen`] and turns keyboard input into bytes.
//!
//! The grid is drawn with wgpu from a glyph atlas ([`crate::grid`]) when
//! the window has wgpu; otherwise text is painted by egui in runs placed on
//! the cell grid, so double-width characters stay aligned either way.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, EventFilter, FontId, Key, PointerButton, Pos2, Rect,
    Sense, Stroke, Vec2,
    text::{LayoutJob, TextFormat},
};
use przmterm_session::CursorShape;
use przmterm_term::{Cell, Screen, Style, Terminal};

use crate::input::{encode_key, encode_paste};

pub const DEFAULT_FG: [u8; 3] = przmterm_session::DEFAULT_FOREGROUND;
pub const DEFAULT_BG: [u8; 3] = przmterm_session::DEFAULT_BACKGROUND;

/// The colours of plain text and of the background, for cells with no
/// colour of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ink {
    pub fg: [u8; 3],
    pub bg: [u8; 3],
}

impl Default for Ink {
    fn default() -> Self {
        Self {
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
        }
    }
}
/// The cursor colour when none is chosen.
pub const CURSOR: Color32 = Color32::from_rgb(110, 200, 255);
const SELECTION: Color32 = Color32::from_rgb(58, 92, 140);
/// Lines scrolled per wheel notch on the alternate screen (sent as arrow keys).
/// Width kept at the right of the terminal for the scroll bar.
const SCROLLBAR_W: f32 = 10.0;
const ALT_SCROLL_LINES: usize = 3;

/// The 16 ANSI colours, tuned to stay readable on the dark background.
/// Indices 16–255 keep the standard xterm values.
const ANSI_16: [[u8; 3]; 16] = [
    [0, 0, 0],
    [205, 49, 49],
    [13, 188, 121],
    [229, 229, 16],
    [36, 114, 200],
    [188, 63, 188],
    [17, 168, 205],
    [229, 229, 229],
    [102, 102, 102],
    [241, 76, 76],
    [35, 209, 139],
    [245, 245, 67],
    [59, 142, 234],
    [214, 112, 214],
    [41, 184, 219],
    [255, 255, 255],
];

pub(crate) fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn resolve(color: przmterm_term::Color, default: [u8; 3]) -> [u8; 3] {
    match color {
        przmterm_term::Color::Indexed(i @ 0..=15) => ANSI_16[usize::from(i)],
        other => other.to_rgb(default),
    }
}

/// Foreground and background after bold-brightening and reverse video.
fn colors(style: &Style, ink: Ink) -> (Color32, Color32) {
    let mut fg = style.fg;
    if style.bold
        && let przmterm_term::Color::Indexed(i @ 0..=7) = fg
    {
        fg = przmterm_term::Color::Indexed(i + 8);
    }
    let mut fg = resolve(fg, ink.fg);
    let mut bg = resolve(style.bg, ink.bg);
    if style.reverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if style.dim {
        fg = [
            fg[0] / 2 + bg[0] / 2,
            fg[1] / 2 + bg[1] / 2,
            fg[2] / 2 + bg[2] / 2,
        ];
    }
    (rgb(fg), rgb(bg))
}

/// A position that survives history scrolling: a stable line id
/// (combined index + [`Screen::history_base`]) and a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Point {
    line: u64,
    col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectMode {
    Cell,
    Word,
    Line,
}

#[derive(Debug, Clone, Copy)]
struct Selection {
    anchor: Point,
    head: Point,
    mode: SelectMode,
}

type Span = ((usize, usize), (usize, usize));

impl Selection {
    /// Ordered, mode-expanded range in combined `(line, col)` coordinates.
    fn span(&self, screen: &Screen) -> Option<Span> {
        let base = screen.history_base();
        let last = screen.total_lines().checked_sub(1)?;
        let to_index = |p: Point| -> (usize, usize) {
            match p.line.checked_sub(base) {
                Some(i) => ((i as usize).min(last), p.col),
                None => (0, 0), // scrolled out of history
            }
        };
        let (a, b) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        let (mut start, mut end) = (to_index(a), to_index(b));
        match self.mode {
            SelectMode::Cell => {}
            SelectMode::Word => {
                if let Some(line) = screen.line_at(start.0) {
                    start.1 = word_bounds(line, start.1).0;
                }
                if let Some(line) = screen.line_at(end.0) {
                    end.1 = word_bounds(line, end.1).1;
                }
            }
            SelectMode::Line => {
                start.1 = 0;
                end.1 = screen.cols().saturating_sub(1);
            }
        }
        Some((start, end))
    }

    fn text(&self, screen: &Screen) -> Option<String> {
        let (a, b) = self.span(screen)?;
        Some(screen.text_range(a, b)).filter(|t| !t.is_empty())
    }
}

/// Characters that end a word on double-click. Dots, dashes, slashes, colons
/// and `@` stay inside so IP addresses, paths and URLs select whole.
fn is_word_char(c: char) -> bool {
    !c.is_whitespace() && !"\"'`()[]{}<>|,;".contains(c)
}

/// Columns of the word around `col` (inclusive).
fn word_bounds(line: &[Cell], col: usize) -> (usize, usize) {
    let col = col.min(line.len().saturating_sub(1));
    let word = |i: usize| line[i].is_spacer() || is_word_char(line[i].ch);
    if line.is_empty() || !word(col) {
        return (col, col);
    }
    let mut start = col;
    while start > 0 && word(start - 1) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < line.len() && word(end + 1) {
        end += 1;
    }
    (start, end)
}

/// Result of drawing the terminal for one frame.
pub struct ViewOutput {
    /// Bytes typed or pasted by the user, to send to the transport.
    pub input: Vec<u8>,
    /// New size in cells when the widget was resized.
    pub resized: Option<(usize, usize)>,
    /// The terminal was clicked (focus moves to this pane).
    pub clicked: bool,
    /// "Send BREAK" was chosen in the context menu.
    pub send_break: bool,
    /// Pasted text with line breaks, to confirm before it is sent.
    pub paste: Option<String>,
}

/// A pane's font and cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub font_size: f32,
    pub font_name: Option<String>,
    pub cursor: CursorShape,
    pub cursor_blink: bool,
    pub cursor_color: Option<[u8; 3]>,
    pub background: Option<[u8; 3]>,
    pub foreground: Option<[u8; 3]>,
}

pub struct TerminalView {
    pub font_size: f32,
    /// The installed font family chosen (`None`: the built-in one).
    pub font_name: Option<String>,
    /// The egui family that draws it, set by the app each frame.
    pub family: egui::FontFamily,
    pub cursor: CursorShape,
    pub cursor_blink: bool,
    /// `None`: the default cursor colour.
    pub cursor_color: Option<[u8; 3]>,
    /// The background; `None`: the default one.
    pub background: Option<[u8; 3]>,
    /// Plain text; `None`: the default colour.
    pub foreground: Option<[u8; 3]>,
    /// When the cursor last moved or a key was typed: blinking restarts
    /// from "on" so the cursor is seen.
    blink_from: std::time::Instant,
    /// Offer "Send BREAK" (serial and Telnet sessions).
    pub offer_break: bool,
    /// Keyword highlighting for what is shown.
    pub highlighter: Option<std::rc::Rc<przmterm_term::highlight::Highlighter>>,
    focus_requested: bool,
    selection: Option<Selection>,
    scroll_accum: f32,
    /// Text being composed by the input method: a dead key (´ ~ " on many
    /// layouts) waiting for the next one, or CJK input. Shown at the cursor.
    preedit: String,
    /// A multi-line paste waiting for the app to confirm it.
    pending_paste: Option<String>,
}

impl Default for TerminalView {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            font_name: None,
            family: egui::FontFamily::Monospace,
            cursor: CursorShape::Block,
            cursor_blink: false,
            cursor_color: None,
            background: None,
            foreground: None,
            blink_from: std::time::Instant::now(),
            offer_break: false,
            highlighter: None,
            focus_requested: false,
            selection: None,
            scroll_accum: 0.0,
            preedit: String::new(),
            pending_paste: None,
        }
    }
}

impl TerminalView {
    pub fn for_protocol(protocol: przmterm_core::Protocol) -> Self {
        use przmterm_core::Protocol::{Serial, Telnet};
        Self {
            offer_break: matches!(protocol, Serial | Telnet),
            ..Self::default()
        }
    }

    /// The font and cursor a session asks for.
    pub fn for_session(session: &przmterm_session::Session) -> Self {
        let mut view = Self::for_protocol(session.protocol);
        view.font_size = f32::from(session.font_size);
        view.font_name = session.font.clone();
        view.cursor = session.cursor;
        view.cursor_blink = session.cursor_blink;
        view.cursor_color = session.cursor_color;
        view.background = session.background;
        view.foreground = session.foreground;
        view
    }

    /// Takes the font and cursor of `other` (a clone of its session).
    pub fn look_like(&mut self, other: &TerminalView) {
        self.set_look(&other.look());
    }

    /// The font and cursor, to put back later.
    pub fn look(&self) -> Look {
        Look {
            font_size: self.font_size,
            font_name: self.font_name.clone(),
            cursor: self.cursor,
            cursor_blink: self.cursor_blink,
            cursor_color: self.cursor_color,
            background: self.background,
            foreground: self.foreground,
        }
    }

    pub fn set_look(&mut self, look: &Look) {
        self.font_size = look.font_size;
        self.font_name = look.font_name.clone();
        self.cursor = look.cursor;
        self.cursor_blink = look.cursor_blink;
        self.cursor_color = look.cursor_color;
        self.background = look.background;
        self.foreground = look.foreground;
    }

    /// The colours of plain text and the background.
    pub fn ink(&self) -> Ink {
        Ink {
            fg: self.foreground.unwrap_or(DEFAULT_FG),
            bg: self.background.unwrap_or(DEFAULT_BG),
        }
    }

    /// The colour the cursor is drawn in.
    pub fn cursor_color(&self) -> Color32 {
        self.cursor_color
            .map(|[r, g, b]| Color32::from_rgb(r, g, b))
            .unwrap_or(CURSOR)
    }
}

fn read_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

impl TerminalView {
    /// Asks for keyboard focus on the next frame.
    pub fn focus(&mut self) {
        self.focus_requested = false;
    }

    pub fn show(&mut self, ui: &mut egui::Ui, term: &mut Terminal, active: bool) -> ViewOutput {
        let font_id = FontId::new(self.font_size, self.family.clone());
        let (cell_w, cell_h) = ui
            .ctx()
            .fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)));

        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let clicked = response.clicked() || response.drag_started();
        if clicked || (active && !self.focus_requested) {
            response.request_focus();
            self.focus_requested = true;
        }
        let focused = response.has_focus();

        // Fit the grid to the widget, leaving room for the scroll bar.
        let mut inner = rect.shrink(6.0);
        inner.max.x -= SCROLLBAR_W;
        let cols = ((inner.width() / cell_w).floor() as usize).max(1);
        let rows = ((inner.height() / cell_h).floor() as usize).max(1);
        let mut resized = None;
        if (cols, rows) != (term.screen().cols(), term.screen().rows()) {
            term.resize(cols, rows);
            resized = Some((cols, rows));
        }

        let mut input = Vec::new();
        self.handle_mouse(ui, &response, inner, (cell_w, cell_h), term, &mut input);

        let screen = term.screen();
        let painter = ui.painter_at(rect);
        let ink = self.ink();
        painter.rect_filled(rect, CornerRadius::ZERO, rgb(ink.bg));
        let span = self.selection.and_then(|s| s.span(screen));
        let mut grid = crate::grid::enabled().then(|| GridFrame {
            ppp: ui.ctx().pixels_per_point(),
            ascent: crate::grid::atlas().ascent(&self.family, self.font_size),
            ink,
            ..GridFrame::default()
        });
        for row in 0..screen.rows() {
            let index = screen.view_row_index(row);
            let selected = span.and_then(|((sl, sc), (el, ec))| {
                (index >= sl && index <= el).then(|| {
                    let from = if index == sl { sc } else { 0 };
                    let to = if index == el { ec } else { usize::MAX };
                    (from, to)
                })
            });
            let origin = Pos2::new(inner.left(), inner.top() + row as f32 * cell_h);
            let mut line = std::borrow::Cow::Borrowed(screen.view_line(row));
            if let Some(h) = &self.highlighter {
                h.apply(line.to_mut());
            }
            match &mut grid {
                Some(grid) => grid.row(&line, origin, (cell_w, cell_h), &font_id, selected),
                None => paint_row(
                    &painter,
                    &line,
                    origin,
                    (cell_w, cell_h),
                    &font_id,
                    selected,
                    ink,
                ),
            }
        }
        if let Some(grid) = grid {
            let leftovers = grid.leftovers;
            let mut quads = grid.backgrounds;
            quads.extend(grid.glyphs);
            painter.add(crate::grid::paint(rect, response.id.value(), quads));
            // What the atlas cannot draw, egui draws over the grid.
            for (at, ch, fg, italics) in leftovers {
                let format = TextFormat {
                    font_id: font_id.clone(),
                    color: fg,
                    italics,
                    ..TextFormat::default()
                };
                let galley = painter.layout_job(LayoutJob::single_section(ch.to_string(), format));
                painter.galley(at, galley, fg);
            }
        }

        let offset = screen.display_offset();
        let cursor = screen.cursor();
        let cursor_cell = Rect::from_min_size(
            Pos2::new(
                inner.left() + cursor.col as f32 * cell_w,
                inner.top() + (cursor.row + offset).min(screen.rows()) as f32 * cell_h,
            ),
            Vec2::new(cell_w, cell_h),
        );
        if screen.cursor_visible() && cursor.row + offset < screen.rows() {
            let min = Pos2::new(
                inner.left() + cursor.col as f32 * cell_w,
                inner.top() + (cursor.row + offset) as f32 * cell_h,
            );
            let cell = Rect::from_min_size(min, Vec2::new(cell_w, cell_h));
            let cursor_color = self.cursor_color();
            // Blinking: on for 530 ms, off for 530 ms, while focused.
            let shown = if focused && self.cursor_blink {
                let ms = self.blink_from.elapsed().as_millis();
                let left = 530 - (ms % 530) as u64;
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(left));
                (ms / 530) & 1 == 0
            } else {
                true
            };
            let thin = (cell_h / 8.0).clamp(1.0, 3.0);
            let mark = match self.cursor {
                CursorShape::Block => cell,
                CursorShape::Underline => cell.with_min_y(cell.max.y - thin),
                CursorShape::Bar => cell.with_max_x(cell.min.x + thin),
            };
            if shown && focused {
                let color = match self.cursor {
                    CursorShape::Block => cursor_color.gamma_multiply(0.6),
                    _ => cursor_color,
                };
                painter.rect_filled(mark, CornerRadius::ZERO, color);
            } else if !focused {
                match self.cursor {
                    CursorShape::Block => painter.rect_stroke(
                        cell,
                        CornerRadius::ZERO,
                        Stroke::new(1.0, cursor_color),
                        egui::StrokeKind::Inside,
                    ),
                    _ => painter.rect_filled(
                        mark,
                        CornerRadius::ZERO,
                        cursor_color.gamma_multiply(0.5),
                    ),
                };
            }
        }
        self.scroll_bar(ui, response.id, rect, term);
        if offset > 0 {
            painter.text(
                rect.right_top() + Vec2::new(-10.0, 8.0),
                Align2::RIGHT_TOP,
                format!("↑ {offset} lines · Shift+End to return"),
                FontId::proportional(12.0),
                CURSOR,
            );
        }

        // A dead key waiting for the next one.
        if focused && !self.preedit.is_empty() {
            let galley = painter.layout_no_wrap(self.preedit.clone(), font_id.clone(), CURSOR);
            let at = Rect::from_min_size(cursor_cell.min, galley.size().max(cursor_cell.size()));
            painter.rect_filled(at, CornerRadius::ZERO, rgb(self.ink().bg));
            painter.galley(at.min, galley, CURSOR);
            painter.line_segment(
                [at.left_bottom(), at.right_bottom()],
                Stroke::new(1.0, CURSOR),
            );
        }

        // `active` is false while a dialog is open: its fields get the keys.
        if focused && active {
            // Lets the input method work here: dead keys (quotes and accents
            // on ABNT2 and international layouts) and CJK input arrive as
            // composed text.
            ui.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Terminal,
                    rect,
                    cursor_rect: cursor_cell,
                    should_interrupt_composition: false,
                });
            });
            self.handle_keys(ui, response.id, term, &mut input);
        } else {
            self.preedit.clear();
        }
        let send_break = self.context_menu(&response, term, &mut input);

        if !input.is_empty() {
            term.screen_mut().reset_display();
            self.selection = None;
            self.blink_from = std::time::Instant::now();
        }
        ViewOutput {
            input,
            resized,
            clicked,
            send_break,
            paste: self.pending_paste.take(),
        }
    }

    /// The scroll bar at the right: where the view is in the history; drag
    /// the thumb or click the track to move through it.
    fn scroll_bar(&mut self, ui: &egui::Ui, id: egui::Id, rect: Rect, term: &mut Terminal) {
        let screen = term.screen();
        let history = screen.scrollback_len();
        if history == 0 || screen.in_alt_screen() {
            return;
        }
        let track = Rect::from_min_max(
            Pos2::new(rect.right() - SCROLLBAR_W, rect.top() + 2.0),
            Pos2::new(rect.right() - 2.0, rect.bottom() - 2.0),
        );
        let response = ui.interact(track, id.with("scrollbar"), Sense::click_and_drag());
        let rows = screen.rows();
        let total = (history + rows) as f32;
        let thumb_h = (track.height() * rows as f32 / total)
            .max(24.0)
            .min(track.height());
        let room = (track.height() - thumb_h).max(1.0);
        // The top of the view, in lines from the oldest one.
        let top_line = (history - screen.display_offset()) as f32;
        let thumb_top = track.top() + room * top_line / history as f32;
        let thumb = Rect::from_min_size(
            Pos2::new(track.left(), thumb_top),
            Vec2::new(track.width(), thumb_h),
        );
        if let Some(pointer) = response.interact_pointer_pos()
            && (response.dragged() || response.clicked())
        {
            // The thumb follows the pointer, centred on it.
            let y = (pointer.y - track.top() - thumb_h / 2.0).clamp(0.0, room);
            let top = (y / room * history as f32).round() as usize;
            let want = history - top.min(history);
            let now = screen.display_offset();
            term.screen_mut()
                .scroll_display(want as isize - now as isize);
        }
        let hot = response.hovered() || response.dragged();
        let painter = ui.painter();
        if hot {
            painter.rect_filled(track, CornerRadius::same(4), Color32::from_white_alpha(12));
        }
        painter.rect_filled(
            thumb,
            CornerRadius::same(4),
            Color32::from_white_alpha(if hot { 120 } else { 60 }),
        );
    }

    fn handle_mouse(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        inner: Rect,
        (cell_w, cell_h): (f32, f32),
        term: &mut Terminal,
        input: &mut Vec<u8>,
    ) {
        let point_at = |screen: &Screen, pos: Pos2| -> Point {
            let col = ((pos.x - inner.left()) / cell_w).floor().max(0.0) as usize;
            let row = ((pos.y - inner.top()) / cell_h).floor().max(0.0) as usize;
            let row = row.min(screen.rows() - 1);
            Point {
                line: screen.history_base() + screen.view_row_index(row) as u64,
                col: col.min(screen.cols() - 1),
            }
        };

        if let Some(pos) = response.interact_pointer_pos() {
            let screen = term.screen();
            let here = point_at(screen, pos);
            if response.triple_clicked() {
                self.selection = Some(Selection {
                    anchor: here,
                    head: here,
                    mode: SelectMode::Line,
                });
            } else if response.double_clicked() {
                self.selection = Some(Selection {
                    anchor: here,
                    head: here,
                    mode: SelectMode::Word,
                });
            } else if response.drag_started_by(PointerButton::Primary) {
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                let start = point_at(screen, origin);
                self.selection = Some(Selection {
                    anchor: start,
                    head: start,
                    mode: SelectMode::Cell,
                });
            } else if response.dragged_by(PointerButton::Primary) {
                // Drag past the edges to scroll through history.
                if pos.y < inner.top() {
                    term.screen_mut().scroll_display(1);
                } else if pos.y > inner.bottom() {
                    term.screen_mut().scroll_display(-1);
                }
                let here = point_at(term.screen(), pos);
                if let Some(sel) = &mut self.selection {
                    sel.head = here;
                }
            } else if response.clicked_by(PointerButton::Primary) {
                self.selection = None;
            }
        }

        if response.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            self.scroll_accum += dy;
            let lines = (self.scroll_accum / cell_h).trunc();
            self.scroll_accum -= lines * cell_h;
            let lines = lines as isize;
            if lines != 0 {
                let screen = term.screen();
                if screen.in_alt_screen() {
                    // Full-screen programs (less, vim, htop) scroll with arrow keys.
                    let key = if lines > 0 {
                        Key::ArrowUp
                    } else {
                        Key::ArrowDown
                    };
                    let app_cursor = screen.app_cursor_keys();
                    for _ in 0..lines.unsigned_abs() * ALT_SCROLL_LINES {
                        input.extend(
                            encode_key(key, egui::Modifiers::NONE, app_cursor).unwrap_or_default(),
                        );
                    }
                } else {
                    term.screen_mut().scroll_display(lines);
                }
            }
        }
    }

    fn handle_keys(
        &mut self,
        ui: &egui::Ui,
        id: egui::Id,
        term: &mut Terminal,
        input: &mut Vec<u8>,
    ) {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        let screen = term.screen();
        let app_cursor = screen.app_cursor_keys();
        let bracketed = screen.bracketed_paste();
        let page = screen.rows() as isize;
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                egui::Event::Text(text) => input.extend_from_slice(text.as_bytes()),
                egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                    self.preedit.clear();
                    input.extend_from_slice(text.as_bytes());
                }
                egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => self.preedit = text,
                egui::Event::Paste(text) => self.paste(text, bracketed, input),
                egui::Event::Copy => {
                    if let Some(text) = self.selection.and_then(|s| s.text(term.screen())) {
                        ui.ctx().copy_text(text);
                        self.selection = None;
                    } else if !cfg!(target_os = "macos") {
                        // Ctrl+C with nothing selected interrupts, as in Windows Terminal.
                        input.push(0x03);
                    }
                }
                egui::Event::Cut => {
                    if !cfg!(target_os = "macos") {
                        input.push(0x18);
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let ctrl_shift = modifiers.ctrl && modifiers.shift && !modifiers.alt;
                    match key {
                        Key::C if ctrl_shift => {
                            if let Some(text) = self.selection.and_then(|s| s.text(term.screen())) {
                                ui.ctx().copy_text(text);
                            }
                        }
                        Key::V if ctrl_shift => {
                            if let Some(text) = read_clipboard() {
                                self.paste(text, bracketed, input);
                            }
                        }
                        Key::PageUp if modifiers.shift => term.screen_mut().scroll_display(page),
                        Key::PageDown if modifiers.shift => term.screen_mut().scroll_display(-page),
                        Key::Home if modifiers.shift => {
                            term.screen_mut().scroll_display(isize::MAX / 2)
                        }
                        Key::End if modifiers.shift => term.screen_mut().reset_display(),
                        _ => {
                            if let Some(bytes) = encode_key(key, modifiers, app_cursor) {
                                input.extend(bytes);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Pastes `text`; with line breaks it waits for the user to confirm
    /// (each line would run as a command).
    fn paste(&mut self, text: String, bracketed: bool, input: &mut Vec<u8>) {
        if text.contains(['\n', '\r']) {
            self.pending_paste = Some(text);
        } else {
            input.extend(encode_paste(&text, bracketed));
        }
    }

    fn context_menu(
        &mut self,
        response: &egui::Response,
        term: &mut Terminal,
        input: &mut Vec<u8>,
    ) -> bool {
        let text = self.selection.and_then(|s| s.text(term.screen()));
        let bracketed = term.screen().bracketed_paste();
        let mut send_break = false;
        response.context_menu(|ui| {
            crate::theme::menu(ui);
            if ui
                .add_enabled(text.is_some(), egui::Button::new("Copy"))
                .clicked()
            {
                if let Some(t) = &text {
                    ui.ctx().copy_text(t.clone());
                }
                ui.close();
            }
            if ui.button("Paste").clicked() {
                if let Some(t) = read_clipboard() {
                    self.paste(t, bracketed, input);
                }
                ui.close();
            }
            ui.separator();
            if ui.button("Select all").clicked() {
                let screen = term.screen();
                let base = screen.history_base();
                self.selection = Some(Selection {
                    anchor: Point { line: base, col: 0 },
                    head: Point {
                        line: base + screen.total_lines() as u64 - 1,
                        col: screen.cols() - 1,
                    },
                    mode: SelectMode::Cell,
                });
                ui.close();
            }
            if self.offer_break {
                ui.separator();
                if ui
                    .button("Send BREAK")
                    .on_hover_text("Also in the status bar; the shortcut is in Keyboard shortcuts")
                    .clicked()
                {
                    send_break = true;
                    ui.close();
                }
            }
        });
        send_break
    }
}

/// The quads of the cells shown, for the GPU grid, and the characters the
/// atlas leaves to egui.
#[derive(Default)]
struct GridFrame {
    /// Physical pixels per point.
    ppp: f32,
    /// Points from a row's top to the baseline.
    ascent: f32,
    ink: Ink,
    /// Cell backgrounds and underlines, drawn under all the glyphs.
    backgrounds: Vec<crate::grid::Quad>,
    glyphs: Vec<crate::grid::Quad>,
    /// Where (in points), what, its colour and whether italic.
    leftovers: Vec<(Pos2, char, Color32, bool)>,
}

impl GridFrame {
    /// Adds one row, as [`paint_row`] paints it.
    fn row(
        &mut self,
        cells: &[Cell],
        origin: Pos2,
        (cell_w, cell_h): (f32, f32),
        font_id: &FontId,
        selected: Option<(usize, usize)>,
    ) {
        use crate::atlas::{BINS, Shape};
        use crate::grid::Quad;
        let ppp = self.ppp;
        // Edges on whole pixels, so neighbouring cells meet without seams.
        let x_at = |col: usize| (origin.x + col as f32 * cell_w) * ppp;
        let top = (origin.y * ppp).round();
        let bottom = ((origin.y + cell_h) * ppp).round();
        let baseline = ((origin.y + self.ascent) * ppp).round();
        let line = ppp.round().max(1.0);
        let is_selected = |col: usize| selected.is_some_and(|(a, b)| col >= a && col <= b);
        let mut atlas = crate::grid::atlas();
        let mut col = 0;
        while col < cells.len() {
            let cell = cells[col];
            if cell.is_spacer() {
                col += 1;
                continue;
            }
            let width = if cell.is_wide() { 2 } else { 1 };
            let (fg, mut bg) = colors(&cell.style, self.ink);
            if is_selected(col) {
                bg = SELECTION;
            }
            let (left, right) = (x_at(col).round(), x_at(col + width).round());
            if bg != rgb(self.ink.bg) {
                self.backgrounds
                    .push(Quad::solid([left, top], [right - left, bottom - top], bg));
            }
            if cell.style.underline {
                self.backgrounds
                    .push(Quad::solid([left, bottom - line], [right - left, line], fg));
            }
            let x = x_at(col);
            let mut whole = x.floor();
            let mut bin = ((x - whole) * f32::from(BINS)).round() as u8;
            if bin == BINS {
                whole += 1.0;
                bin = 0;
            }
            match atlas.glyph(&font_id.family, cell.ch, font_id.size, ppp, bin) {
                Shape::Blank => {}
                Shape::Drawn { slot, dy } => self.glyphs.push(Quad::glyph(
                    [
                        whole + slot.left as f32,
                        baseline + dy.round() + slot.top as f32,
                    ],
                    slot,
                    fg,
                    cell.style.italic,
                )),
                Shape::Elsewhere => self.leftovers.push((
                    Pos2::new(origin.x + col as f32 * cell_w, origin.y),
                    cell.ch,
                    fg,
                    cell.style.italic,
                )),
            }
            col += width;
        }
    }
}

/// Paints one row: backgrounds as rectangles, text in style runs placed on
/// the grid. `selected` is the inclusive column range highlighted in this row.
fn paint_row(
    painter: &egui::Painter,
    cells: &[Cell],
    origin: Pos2,
    (cell_w, cell_h): (f32, f32),
    font_id: &FontId,
    selected: Option<(usize, usize)>,
    ink: Ink,
) {
    let is_selected = |col: usize| selected.is_some_and(|(a, b)| col >= a && col <= b);
    let mut col = 0;
    while col < cells.len() {
        let cell = cells[col];
        if cell.is_spacer() {
            col += 1;
            continue;
        }
        let sel = is_selected(col);
        let start = col;
        let mut text = String::new();
        if cell.is_wide() {
            text.push(cell.ch);
            col += 2;
        } else {
            while col < cells.len() {
                let c = cells[col];
                if c.is_wide() || c.is_spacer() || c.style != cell.style || is_selected(col) != sel
                {
                    break;
                }
                text.push(c.ch);
                col += 1;
            }
        }
        let width = (col.min(cells.len()) - start) as f32 * cell_w;
        let (fg, mut bg) = colors(&cell.style, ink);
        if sel {
            bg = SELECTION;
        }
        let x = origin.x + start as f32 * cell_w;
        if bg != rgb(ink.bg) {
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(x, origin.y), Vec2::new(width, cell_h)),
                CornerRadius::ZERO,
                bg,
            );
        }
        if text.trim().is_empty() && !cell.style.underline {
            continue;
        }
        let format = TextFormat {
            font_id: font_id.clone(),
            color: fg,
            italics: cell.style.italic,
            underline: if cell.style.underline {
                Stroke::new(1.0, fg)
            } else {
                Stroke::NONE
            },
            ..TextFormat::default()
        };
        let galley = painter.layout_job(LayoutJob::single_section(text, format));
        painter.galley(Pos2::new(x, origin.y), galley, fg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use przmterm_term::Color;

    #[test]
    fn grid_rows_become_quads() {
        use crate::grid::{GLYPH, ITALIC};
        crate::grid::atlas().set_fonts(&egui::FontDefinitions::default());
        let cell = |ch: char| Cell {
            ch,
            ..Cell::default()
        };
        let mut red = cell('r');
        red.style.bg = Color::Indexed(1);
        red.style.underline = true;
        let mut italic = cell('i');
        italic.style.italic = true;
        let wide = Cell {
            ch: '漢',
            flags: Cell::WIDE,
            ..Cell::default()
        };
        let spacer = Cell {
            flags: Cell::SPACER,
            ..Cell::default()
        };
        let row = [red, italic, cell(' '), wide, spacer, cell('\u{10FFFD}')];
        let mut grid = GridFrame {
            ppp: 2.0,
            ascent: 12.0,
            ..GridFrame::default()
        };
        let font = FontId::monospace(14.0);
        grid.row(
            &row,
            Pos2::new(10.0, 20.0),
            (8.5, 17.0),
            &font,
            Some((2, 2)),
        );

        // A red background and its underline; the selected blank cell.
        let solid: Vec<_> = grid.backgrounds.iter().map(|q| (q.pos, q.size)).collect();
        assert_eq!(
            solid,
            [
                ([20.0, 40.0], [17.0, 34.0]),
                ([20.0, 72.0], [17.0, 2.0]),
                ([54.0, 40.0], [17.0, 34.0]),
            ]
        );
        assert_eq!(
            grid.backgrounds[2].color,
            u32::from_le_bytes(SELECTION.to_array())
        );
        // r and i (leaning) from the atlas, on whole pixels; what egui's
        // own fonts lack (CJK here: a system font has it) goes to egui, at
        // its cell, the spacer skipped.
        let flags: Vec<u32> = grid.glyphs.iter().map(|q| q.flags).collect();
        assert_eq!(flags, [GLYPH, GLYPH | ITALIC]);
        assert!(
            grid.glyphs
                .iter()
                .all(|q| q.pos[0].fract() == 0.0 && q.pos[1].fract() == 0.0)
        );
        let left: Vec<(Pos2, char)> = grid.leftovers.iter().map(|l| (l.0, l.1)).collect();
        assert_eq!(
            left,
            [
                (Pos2::new(10.0 + 3.0 * 8.5, 20.0), '漢'),
                (Pos2::new(10.0 + 5.0 * 8.5, 20.0), '\u{10FFFD}'),
            ]
        );
    }

    #[test]
    fn bold_brightens_and_reverse_swaps() {
        let bold_red = Style {
            fg: Color::Indexed(1),
            bold: true,
            ..Style::default()
        };
        assert_eq!(colors(&bold_red, Ink::default()).0, rgb(ANSI_16[9]));

        let rev = Style {
            reverse: true,
            ..Style::default()
        };
        assert_eq!(
            colors(&rev, Ink::default()),
            (rgb(DEFAULT_BG), rgb(DEFAULT_FG))
        );
        // Plain text takes the session's colours.
        let ink = Ink {
            fg: [0, 0, 0],
            bg: [255, 255, 255],
        };
        assert_eq!(
            colors(&Style::default(), ink),
            (rgb([0, 0, 0]), rgb([255, 255, 255]))
        );
    }

    fn select(
        t: &Terminal,
        a: (usize, usize),
        b: (usize, usize),
        mode: SelectMode,
    ) -> Option<String> {
        let base = t.screen().history_base();
        let p = |(line, col): (usize, usize)| Point {
            line: base + line as u64,
            col,
        };
        Selection {
            anchor: p(a),
            head: p(b),
            mode,
        }
        .text(t.screen())
    }

    #[test]
    fn word_selection_keeps_addresses_whole() {
        let mut t = Terminal::new(40, 2);
        t.feed(b"ping 10.0.0.1 (/etc/hosts)");
        assert_eq!(
            select(&t, (0, 7), (0, 7), SelectMode::Word).as_deref(),
            Some("10.0.0.1")
        );
        assert_eq!(
            select(&t, (0, 17), (0, 17), SelectMode::Word).as_deref(),
            Some("/etc/hosts")
        );
        assert_eq!(select(&t, (0, 4), (0, 4), SelectMode::Word), None);
    }

    #[test]
    fn line_and_reversed_cell_selection() {
        let mut t = Terminal::new(10, 3);
        t.feed(b"first\r\nsecond");
        assert_eq!(
            select(&t, (1, 2), (1, 2), SelectMode::Line).as_deref(),
            Some("second")
        );
        assert_eq!(
            select(&t, (1, 2), (0, 3), SelectMode::Cell).as_deref(),
            Some("st\nsec")
        );
    }

    #[test]
    fn selection_follows_lines_into_history() {
        let mut t = Terminal::with_scrollback(10, 2, 100);
        t.feed(b"keep me\r\nb");
        let sel = Selection {
            anchor: Point { line: 0, col: 0 },
            head: Point { line: 0, col: 6 },
            mode: SelectMode::Cell,
        };
        t.feed(b"\r\nc\r\nd");
        assert_eq!(sel.text(t.screen()).as_deref(), Some("keep me"));
    }
}
