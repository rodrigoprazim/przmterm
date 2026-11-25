//! Picking a colour: a swatch button that opens a picker with a palette,
//! the colours picked lately, a saturation/brightness square with a hue
//! bar, and the colour as hue/saturation/lightness, red/green/blue and hex
//! to type in. Used for every colour PrzmTerm asks for (cursor, terminal,
//! keywords, highlights).

use std::sync::Mutex;

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2, pos2, vec2};

/// Colours picked lately, newest first, for this run.
static RECENT: Mutex<Vec<[u8; 3]>> = Mutex::new(Vec::new());
const RECENT_MAX: usize = 8;

/// The palette: 8 hues in 5 shades, then greys from black to white.
const PALETTE_HUES: [f32; 8] = [0.0, 30.0, 55.0, 120.0, 175.0, 210.0, 265.0, 310.0];
const PALETTE_SHADES: [(f32, f32); 5] = [
    (1.0, 0.85),
    (1.0, 0.65),
    (1.0, 0.50),
    (1.0, 0.35),
    (1.0, 0.22),
];
const SWATCH: Vec2 = vec2(20.0, 16.0);
const SQUARE: Vec2 = vec2(200.0, 150.0);
const HUE_BAR: f32 = 16.0;
/// The number fields.
const FIELD: Vec2 = vec2(52.0, 20.0);

/// A swatch of `rgb`; a click opens the picker. `changed()` when edited,
/// as egui's own colour button.
pub fn button(ui: &mut egui::Ui, rgb: &mut [u8; 3]) -> egui::Response {
    let popup = ui.auto_id_with("przm-color-popup");
    let open = egui::Popup::is_id_open(ui.ctx(), popup);
    let size = ui.spacing().interact_size;
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = if open {
            &ui.visuals().widgets.open
        } else {
            ui.style().interact(&response)
        };
        let radius = visuals.corner_radius;
        ui.painter()
            .rect_filled(rect.shrink(1.0), radius, color32(*rgb));
        ui.painter()
            .rect_stroke(rect, radius, visuals.bg_stroke, egui::StrokeKind::Inside);
    }
    let shown = egui::Popup::menu(&response)
        .id(popup)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| picker(ui, popup, rgb));
    match shown {
        Some(inner) if inner.inner => response.mark_changed(),
        Some(_) => {}
        // Just closed: the colour joins the recent ones.
        None if open => remember(*rgb),
        None => {}
    }
    response.on_hover_text("Click to pick a colour")
}

/// The picker itself, for a popup or a dialog of its own. Whether `rgb`
/// changed. `id` keeps its state (the hue of a grey, the hex being typed).
pub fn picker(ui: &mut egui::Ui, id: egui::Id, rgb: &mut [u8; 3]) -> bool {
    let before = *rgb;
    // The colour as hue, saturation and value, kept between frames: a grey
    // has no hue of its own, and dragging through one must not lose it.
    let state_id = id.with("hsv");
    let (seen, mut hsv, start) = ui
        .data(|d| d.get_temp::<([u8; 3], [f32; 3], [u8; 3])>(state_id))
        .filter(|(seen, _, _)| *seen == *rgb)
        .unwrap_or((*rgb, rgb_to_hsv(*rgb), *rgb));
    let _ = seen;
    crate::theme::sunken_combos(ui);
    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);

    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.weak("Palette");
            swatches(ui, &palette(), rgb);
            let recent = RECENT.lock().map(|r| r.clone()).unwrap_or_default();
            if !recent.is_empty() {
                ui.add_space(2.0);
                ui.weak("Recent");
                swatches(ui, &recent, rgb);
            }
        });
        ui.add_space(4.0);
        // Saturation across, brightness up, for the hue on the bar beside.
        let (square, response) = ui.allocate_exact_size(SQUARE, Sense::click_and_drag());
        if let Some(p) = response.interact_pointer_pos() {
            hsv[1] = ((p.x - square.left()) / square.width()).clamp(0.0, 1.0);
            hsv[2] = 1.0 - ((p.y - square.top()) / square.height()).clamp(0.0, 1.0);
            *rgb = hsv_to_rgb(hsv);
        }
        paint_square(ui.painter(), square, hsv[0]);
        let at = pos2(
            square.left() + hsv[1] * square.width(),
            square.top() + (1.0 - hsv[2]) * square.height(),
        );
        ui.painter()
            .circle_stroke(at, 5.0, Stroke::new(2.0, Color32::WHITE));
        ui.painter()
            .circle_stroke(at, 6.5, Stroke::new(1.0, Color32::BLACK));

        let (bar, response) =
            ui.allocate_exact_size(vec2(HUE_BAR, SQUARE.y), Sense::click_and_drag());
        if let Some(p) = response.interact_pointer_pos() {
            hsv[0] = ((p.y - bar.top()) / bar.height()).clamp(0.0, 1.0) * 360.0;
            *rgb = hsv_to_rgb(hsv);
        }
        paint_hue_bar(ui.painter(), bar);
        let y = bar.top() + hsv[0] / 360.0 * bar.height();
        let marker = Rect::from_min_max(
            pos2(bar.left() - 2.0, y - 2.0),
            pos2(bar.right() + 2.0, y + 2.0),
        );
        ui.painter().rect_stroke(
            marker,
            1.0,
            Stroke::new(1.5, Color32::WHITE),
            egui::StrokeKind::Middle,
        );
        ui.painter().rect_stroke(
            marker.expand(1.0),
            1.0,
            Stroke::new(1.0, Color32::BLACK),
            egui::StrokeKind::Outside,
        );
    });

    ui.add_space(2.0);
    ui.horizontal_top(|ui| {
        // Before and now: a click on "before" goes back to it.
        ui.vertical(|ui| {
            let (rect, response) = ui.allocate_exact_size(vec2(64.0, 22.0), Sense::click());
            ui.painter().rect_filled(rect, 3.0, color32(start));
            if response
                .on_hover_text("As it was: click to go back")
                .clicked()
            {
                *rgb = start;
                hsv = rgb_to_hsv(start);
            }
            let (rect, _) = ui.allocate_exact_size(vec2(64.0, 22.0), Sense::hover());
            ui.painter().rect_filled(rect, 3.0, color32(*rgb));
            ui.weak("Before / now");
        });
        ui.add_space(8.0);
        egui::Grid::new(id.with("fields"))
            .num_columns(4)
            .spacing(vec2(8.0, 4.0))
            .show(ui, |ui| {
                // Hue, saturation and lightness (HSL), as other pickers show.
                let [h, mut s, mut l] = rgb_to_hsl(*rgb);
                let h = if s == 0.0 { hsv[0] } else { h };
                let mut hue = h.round() as i32 % 360;
                let mut sat = (s * 100.0).round() as i32;
                let mut light = (l * 100.0).round() as i32;
                let mut hsl_changed = false;
                ui.label("Hue");
                hsl_changed |= ui
                    .add_sized(
                        FIELD,
                        egui::DragValue::new(&mut hue).range(0..=359).suffix("°"),
                    )
                    .changed();
                let [mut r, mut g, mut b] = *rgb;
                ui.label("Red");
                let mut rgb_changed = ui.add_sized(FIELD, egui::DragValue::new(&mut r)).changed();
                ui.end_row();
                ui.label("Saturation");
                hsl_changed |= ui
                    .add_sized(
                        FIELD,
                        egui::DragValue::new(&mut sat).range(0..=100).suffix("%"),
                    )
                    .changed();
                ui.label("Green");
                rgb_changed |= ui.add_sized(FIELD, egui::DragValue::new(&mut g)).changed();
                ui.end_row();
                ui.label("Lightness");
                hsl_changed |= ui
                    .add_sized(
                        FIELD,
                        egui::DragValue::new(&mut light).range(0..=100).suffix("%"),
                    )
                    .changed();
                ui.label("Blue");
                rgb_changed |= ui.add_sized(FIELD, egui::DragValue::new(&mut b)).changed();
                ui.end_row();
                if hsl_changed {
                    s = sat as f32 / 100.0;
                    l = light as f32 / 100.0;
                    *rgb = hsl_to_rgb([hue as f32, s, l]);
                    hsv = rgb_to_hsv(*rgb);
                    hsv[0] = hue as f32;
                } else if rgb_changed {
                    *rgb = [r, g, b];
                    let hue = hsv[0];
                    hsv = rgb_to_hsv(*rgb);
                    if hsv[1] == 0.0 {
                        hsv[0] = hue;
                    }
                }
                ui.label("Hex");
                hex_field(ui, id.with("hex"), rgb, &mut hsv);
                ui.end_row();
            });
    });

    // A palette or recent swatch picked: the hue follows it.
    if *rgb != before && rgb_to_hsv(*rgb) != hsv && hsv_to_rgb(hsv) != *rgb {
        hsv = rgb_to_hsv(*rgb);
    }
    ui.data_mut(|d| d.insert_temp(state_id, (*rgb, hsv, start)));
    *rgb != before
}

/// Rows of swatches, 8 to a row; a click picks one.
fn swatches(ui: &mut egui::Ui, colors: &[[u8; 3]], rgb: &mut [u8; 3]) {
    ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
    for row in colors.chunks(8) {
        ui.horizontal(|ui| {
            for &c in row {
                let (rect, response) = ui.allocate_exact_size(SWATCH, Sense::click());
                ui.painter().rect_filled(rect, 2.0, color32(c));
                let stroke = if c == *rgb {
                    Stroke::new(2.0, ui.visuals().strong_text_color())
                } else if response.hovered() {
                    Stroke::new(1.0, ui.visuals().strong_text_color())
                } else {
                    Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
                };
                ui.painter()
                    .rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Outside);
                if response
                    .on_hover_text(format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]))
                    .clicked()
                {
                    *rgb = c;
                }
            }
        });
    }
}

/// `#rrggbb` to read and type; taken when it is a whole colour.
fn hex_field(ui: &mut egui::Ui, id: egui::Id, rgb: &mut [u8; 3], hsv: &mut [f32; 3]) {
    let shown = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    let mut text = ui
        .data(|d| d.get_temp::<(String, String)>(id))
        .filter(|(was, _)| *was == shown)
        .map_or_else(|| shown.clone(), |(_, typing)| typing);
    let response = ui.add_sized(
        vec2(76.0, FIELD.y),
        egui::TextEdit::singleline(&mut text)
            .id(id.with("edit"))
            .font(egui::TextStyle::Monospace),
    );
    if response.changed()
        && let Some(c) = parse_hex(&text)
    {
        *rgb = c;
        *hsv = rgb_to_hsv(c);
    }
    let shown_now = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
    if response.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, (shown_now, text)));
    } else {
        ui.data_mut(|d| d.remove::<(String, String)>(id));
    }
}

/// `#rrggbb`, `rrggbb` or `#rgb`.
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().trim_start_matches('#');
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    match digits.as_slice() {
        [r, g, b] => Some([r * 17, g * 17, b * 17]),
        [r1, r2, g1, g2, b1, b2] => Some([r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2]),
        _ => None,
    }
}

fn remember(rgb: [u8; 3]) {
    if let Ok(mut recent) = RECENT.lock() {
        recent.retain(|c| *c != rgb);
        recent.insert(0, rgb);
        recent.truncate(RECENT_MAX);
    }
}

fn palette() -> Vec<[u8; 3]> {
    let mut colors = Vec::with_capacity(48);
    for (s, l) in PALETTE_SHADES {
        for h in PALETTE_HUES {
            colors.push(hsl_to_rgb([h, s, l]));
        }
    }
    for i in 0..8u8 {
        let v = (u16::from(i) * 255 / 7) as u8;
        colors.push([v, v, v]);
    }
    colors
}

fn color32([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// The square for `hue`: saturation across, brightness down, from a grid
/// of vertices coloured exactly.
fn paint_square(painter: &egui::Painter, rect: Rect, hue: f32) {
    const N: u32 = 24;
    let mut mesh = egui::Mesh::default();
    for yi in 0..=N {
        for xi in 0..=N {
            let (x, y) = (xi as f32 / N as f32, yi as f32 / N as f32);
            let pos = pos2(
                rect.left() + x * rect.width(),
                rect.top() + y * rect.height(),
            );
            mesh.colored_vertex(pos, color32(hsv_to_rgb([hue, x, 1.0 - y])));
        }
    }
    grid_triangles(&mut mesh, N, N);
    painter.add(egui::Shape::mesh(mesh));
}

/// Every hue from top to bottom.
fn paint_hue_bar(painter: &egui::Painter, rect: Rect) {
    const N: u32 = 36;
    let mut mesh = egui::Mesh::default();
    for yi in 0..=N {
        let t = yi as f32 / N as f32;
        let color = color32(hsv_to_rgb([t * 360.0, 1.0, 1.0]));
        let y = rect.top() + t * rect.height();
        mesh.colored_vertex(Pos2::new(rect.left(), y), color);
        mesh.colored_vertex(Pos2::new(rect.right(), y), color);
    }
    grid_triangles(&mut mesh, 1, N);
    painter.add(egui::Shape::mesh(mesh));
}

/// Two triangles per cell of a `cols` × `rows` grid of vertices laid out
/// row by row.
fn grid_triangles(mesh: &mut egui::Mesh, cols: u32, rows: u32) {
    let w = cols + 1;
    for y in 0..rows {
        for x in 0..cols {
            let i = y * w + x;
            mesh.add_triangle(i, i + 1, i + w);
            mesh.add_triangle(i + 1, i + w + 1, i + w);
        }
    }
}

/// Hue in degrees, saturation and value 0–1, from sRGB bytes.
pub fn rgb_to_hsv([r, g, b]: [u8; 3]) -> [f32; 3] {
    let (r, g, b) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let s = if max == 0.0 { 0.0 } else { d / max };
    [hue(r, g, b, max, d), s, max]
}

pub fn hsv_to_rgb([h, s, v]: [f32; 3]) -> [u8; 3] {
    let c = v * s;
    from_chroma(h, c, v - c)
}

/// Hue in degrees, saturation and lightness 0–1 (HSL).
pub fn rgb_to_hsl([r, g, b]: [u8; 3]) -> [f32; 3] {
    let (r, g, b) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let l = (max + min) / 2.0;
    let s = if d == 0.0 {
        0.0
    } else {
        d / (1.0 - (2.0 * l - 1.0).abs())
    };
    [hue(r, g, b, max, d), s.clamp(0.0, 1.0), l]
}

pub fn hsl_to_rgb([h, s, l]: [f32; 3]) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    from_chroma(h, c, l - c / 2.0)
}

fn hue(r: f32, g: f32, b: f32, max: f32, d: f32) -> f32 {
    if d == 0.0 {
        return 0.0;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h * 60.0
}

/// The colour of hue `h` (degrees) with chroma `c`, every channel raised
/// by `m`.
fn from_chroma(h: f32, c: f32, m: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [byte(r), byte(g), byte(b)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_spaces_round_trip() {
        for rgb in [
            [0, 0, 0],
            [255, 255, 255],
            [0, 255, 0],
            [110, 200, 255],
            [205, 49, 49],
            [128, 128, 128],
            [12, 34, 56],
        ] {
            assert_eq!(hsv_to_rgb(rgb_to_hsv(rgb)), rgb, "hsv {rgb:?}");
            assert_eq!(hsl_to_rgb(rgb_to_hsl(rgb)), rgb, "hsl {rgb:?}");
        }
        assert_eq!(rgb_to_hsl([0, 255, 0]), [120.0, 1.0, 0.5]);
        assert_eq!(hsv_to_rgb([0.0, 1.0, 1.0]), [255, 0, 0]);
    }

    #[test]
    fn reads_hex() {
        assert_eq!(parse_hex("#00ff00"), Some([0, 255, 0]));
        assert_eq!(parse_hex(" 6EC8FF "), Some([110, 200, 255]));
        assert_eq!(parse_hex("#0f0"), Some([0, 255, 0]));
        assert_eq!(parse_hex("#00ff0"), None);
        assert_eq!(parse_hex("green"), None);
    }

    #[test]
    fn palette_has_six_rows() {
        let p = palette();
        assert_eq!(p.len(), 48);
        assert_eq!(p[40], [0, 0, 0]);
        assert_eq!(p[47], [255, 255, 255]);
    }
}
