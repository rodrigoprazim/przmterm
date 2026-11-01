//! The terminal widget: paints a [`Screen`] and turns keyboard input into bytes.
//!
//! 0.1 paints through egui's text layout. A dedicated wgpu grid renderer with
//! a glyph atlas replaces it before 1.0 (see ROADMAP.md).

use eframe::egui::{
    self, Color32, CornerRadius, EventFilter, FontId, Pos2, Rect, Sense, Stroke, Vec2,
    text::{LayoutJob, TextFormat},
};
use przmterm_term::{Cell, Style, Terminal};

use crate::input::{encode_key, encode_paste};

pub const DEFAULT_FG: [u8; 3] = [216, 222, 233];
pub const DEFAULT_BG: [u8; 3] = [24, 26, 31];
const CURSOR: Color32 = Color32::from_rgb(110, 200, 255);

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

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn resolve(color: przmterm_term::Color, default: [u8; 3]) -> [u8; 3] {
    match color {
        przmterm_term::Color::Indexed(i @ 0..=15) => ANSI_16[usize::from(i)],
        other => other.to_rgb(default),
    }
}

/// Foreground and background after bold-brightening and reverse video.
fn colors(style: &Style) -> (Color32, Color32) {
    let mut fg = style.fg;
    if style.bold
        && let przmterm_term::Color::Indexed(i @ 0..=7) = fg
    {
        fg = przmterm_term::Color::Indexed(i + 8);
    }
    let mut fg = resolve(fg, DEFAULT_FG);
    let mut bg = resolve(style.bg, DEFAULT_BG);
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

/// Result of drawing the terminal for one frame.
pub struct ViewOutput {
    /// Bytes typed or pasted by the user, to send to the transport.
    pub input: Vec<u8>,
    /// New size in cells when the widget was resized.
    pub resized: Option<(usize, usize)>,
}

pub struct TerminalView {
    pub font_size: f32,
    focus_requested: bool,
}

impl Default for TerminalView {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            focus_requested: false,
        }
    }
}

impl TerminalView {
    pub fn show(&mut self, ui: &mut egui::Ui, term: &mut Terminal) -> ViewOutput {
        let font_id = FontId::monospace(self.font_size);
        let (cell_w, cell_h) = ui
            .ctx()
            .fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)));

        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        if response.clicked() || !self.focus_requested {
            response.request_focus();
            self.focus_requested = true;
        }
        let focused = response.has_focus();

        // Fit the grid to the widget.
        let pad = 6.0;
        let inner = rect.shrink(pad);
        let cols = ((inner.width() / cell_w).floor() as usize).max(1);
        let rows = ((inner.height() / cell_h).floor() as usize).max(1);
        let mut resized = None;
        if (cols, rows) != (term.screen().cols(), term.screen().rows()) {
            term.resize(cols, rows);
            resized = Some((cols, rows));
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, rgb(DEFAULT_BG));

        let screen = term.screen();
        for row in 0..screen.rows() {
            let job = row_job(screen.line(row), &font_id);
            let galley = painter.layout_job(job);
            let pos = Pos2::new(inner.left(), inner.top() + row as f32 * cell_h);
            painter.galley(pos, galley, rgb(DEFAULT_FG));
        }

        if screen.cursor_visible() {
            let c = screen.cursor();
            let min = Pos2::new(
                inner.left() + c.col as f32 * cell_w,
                inner.top() + c.row as f32 * cell_h,
            );
            let cell = Rect::from_min_size(min, Vec2::new(cell_w, cell_h));
            if focused {
                painter.rect_filled(cell, CornerRadius::ZERO, CURSOR.gamma_multiply(0.6));
            } else {
                painter.rect_stroke(
                    cell,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, CURSOR),
                    egui::StrokeKind::Inside,
                );
            }
        }

        let mut input = Vec::new();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            let app_cursor = screen.app_cursor_keys();
            let bracketed = screen.bracketed_paste();
            let events = ui.input(|i| i.events.clone());
            for event in events {
                match event {
                    egui::Event::Text(text) => input.extend_from_slice(text.as_bytes()),
                    egui::Event::Paste(text) => input.extend(encode_paste(&text, bracketed)),
                    // Without a selection yet, the platform copy/cut shortcut on
                    // Linux and Windows (Ctrl+C / Ctrl+X) must reach the remote side.
                    #[cfg(not(target_os = "macos"))]
                    egui::Event::Copy => input.push(0x03),
                    #[cfg(not(target_os = "macos"))]
                    egui::Event::Cut => input.push(0x18),
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        if let Some(bytes) = encode_key(key, modifiers, app_cursor) {
                            input.extend(bytes);
                        }
                    }
                    _ => {}
                }
            }
        }

        ViewOutput { input, resized }
    }
}

/// One row as a layout job, grouping runs of cells that share a style.
fn row_job(cells: &[Cell], font_id: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut run = String::new();
    let mut run_style: Option<Style> = None;
    let flush = |job: &mut LayoutJob, run: &mut String, style: &Style| {
        if run.is_empty() {
            return;
        }
        let (fg, bg) = colors(style);
        let format = TextFormat {
            font_id: font_id.clone(),
            color: fg,
            background: if bg == rgb(DEFAULT_BG) {
                Color32::TRANSPARENT
            } else {
                bg
            },
            italics: style.italic,
            underline: if style.underline {
                Stroke::new(1.0, fg)
            } else {
                Stroke::NONE
            },
            ..TextFormat::default()
        };
        job.append(run, 0.0, format);
        run.clear();
    };
    for cell in cells {
        if run_style != Some(cell.style) {
            if let Some(style) = run_style {
                flush(&mut job, &mut run, &style);
            }
            run_style = Some(cell.style);
        }
        run.push(cell.ch);
    }
    if let Some(style) = run_style {
        flush(&mut job, &mut run, &style);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use przmterm_term::Color;

    #[test]
    fn bold_brightens_and_reverse_swaps() {
        let bold_red = Style {
            fg: Color::Indexed(1),
            bold: true,
            ..Style::default()
        };
        assert_eq!(colors(&bold_red).0, rgb(ANSI_16[9]));

        let rev = Style {
            reverse: true,
            ..Style::default()
        };
        assert_eq!(colors(&rev), (rgb(DEFAULT_BG), rgb(DEFAULT_FG)));
    }

    #[test]
    fn rows_group_runs_by_style() {
        let mut t = Terminal::new(6, 1);
        t.feed(b"ab\x1b[31mcd");
        let job = row_job(t.screen().line(0), &FontId::monospace(12.0));
        assert_eq!(job.text, "abcd  ");
        assert_eq!(job.sections.len(), 3);
    }
}
