//! Small icons drawn with lines, so they look the same on every system
//! (fonts differ in which symbols they have and where they place them).

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Shape, Stroke, Vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    /// Hide the side panel.
    ChevronLeft,
    /// Show the side panel.
    ChevronRight,
    Reload,
    Lock,
    Unlock,
    Gear,
    Plus,
    Close,
    /// Back to the inherited value (an arrow turning back).
    Reset,
    /// The parent folder.
    Up,
    /// Opens a list.
    ChevronDown,
    /// Something that can be dragged: two columns of three dots.
    Grip,
    /// A folder of sessions.
    Folder,
    /// Split side by side: a frame cut down the middle.
    SplitColumns,
    /// Split one above the other.
    SplitRows,
    /// More actions: three dots.
    More,
}

/// Size of an icon button: all of them match.
pub const BUTTON: Vec2 = Vec2::new(24.0, 20.0);

/// A button showing `icon`, flat until the pointer is on it; `selected`
/// draws it pressed. As tall as a text button, so rows of both line up.
pub fn button(ui: &mut egui::Ui, icon: Icon, selected: bool) -> egui::Response {
    let size = Vec2::new(BUTTON.x, BUTTON.y.max(ui.spacing().interact_size.y));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        let lit = selected || response.hovered() || response.is_pointer_button_down_on();
        if lit {
            ui.painter()
                .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        }
        let color = if lit {
            visuals.fg_stroke.color
        } else {
            ui.visuals().text_color()
        };
        draw(ui.painter(), icon, rect, color);
    }
    response
}

/// Draws `icon` centred in `rect` in `color`, in a box of 12 points
/// whatever the size of `rect`.
pub fn draw(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    draw_scaled(painter, icon, rect, color, 1.0);
}

/// [`draw`], `scale` times as big (lines too).
pub fn draw_scaled(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32, scale: f32) {
    let c = rect.center();
    let s = 6.0 * scale;
    let stroke = Stroke::new(1.5 * scale, color);
    let p = |x: f32, y: f32| Pos2::new(c.x + x * s, c.y + y * s);
    match icon {
        Icon::ChevronLeft => {
            painter.add(Shape::line(
                vec![p(0.35, -0.7), p(-0.35, 0.0), p(0.35, 0.7)],
                stroke,
            ));
        }
        Icon::ChevronRight => {
            painter.add(Shape::line(
                vec![p(-0.35, -0.7), p(0.35, 0.0), p(-0.35, 0.7)],
                stroke,
            ));
        }
        Icon::Up => {
            painter.add(Shape::line(
                vec![p(-0.6, -0.05), p(0.0, -0.7), p(0.6, -0.05)],
                stroke,
            ));
            painter.line_segment([p(0.0, -0.65), p(0.0, 0.75)], stroke);
        }
        Icon::ChevronDown => {
            painter.add(Shape::line(
                vec![p(-0.6, -0.3), p(0.0, 0.35), p(0.6, -0.3)],
                stroke,
            ));
        }
        Icon::Reset => {
            // An arc turning left, back to where it started.
            let r = 0.65;
            let (a0, a1) = (-0.85 * std::f32::consts::PI, 0.6 * std::f32::consts::PI);
            let points: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = a0 + (a1 - a0) * i as f32 / 24.0;
                    p(r * a.cos(), r * a.sin())
                })
                .collect();
            painter.add(Shape::line(points, stroke));
            let tip = p(r * a0.cos(), r * a0.sin());
            let along = Vec2::new(a0.sin(), -a0.cos()) * s * 0.5;
            let out = Vec2::new(a0.cos(), a0.sin()) * s * 0.38;
            painter.add(Shape::convex_polygon(
                vec![tip + along, tip - out, tip + out],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Plus => {
            painter.line_segment([p(-0.75, 0.0), p(0.75, 0.0)], stroke);
            painter.line_segment([p(0.0, -0.75), p(0.0, 0.75)], stroke);
        }
        Icon::Close => {
            let stroke = Stroke::new(1.6 * scale, color);
            painter.line_segment([p(-0.55, -0.55), p(0.55, 0.55)], stroke);
            painter.line_segment([p(-0.55, 0.55), p(0.55, -0.55)], stroke);
        }
        Icon::Reload => {
            // Most of a circle, open at the top right, ending in an arrow
            // head that points along it.
            let r = 0.75;
            let (a0, a1) = (-0.25 * std::f32::consts::PI, 1.35 * std::f32::consts::PI);
            let points: Vec<Pos2> = (0..=28)
                .map(|i| {
                    let a = a0 + (a1 - a0) * i as f32 / 28.0;
                    p(r * a.cos(), r * a.sin())
                })
                .collect();
            painter.add(Shape::line(points, stroke));
            // The head at the start of the arc, pointing back (clockwise).
            let tip = p(r * a0.cos(), r * a0.sin());
            let along = Vec2::new(a0.sin(), -a0.cos()) * s * 0.55; // tangent
            let out = Vec2::new(a0.cos(), a0.sin()) * s * 0.4; // radial
            painter.add(Shape::convex_polygon(
                vec![tip + along, tip - out, tip + out],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Lock | Icon::Unlock => {
            let body = Rect::from_min_max(p(-0.7, -0.05), p(0.7, 0.85));
            painter.rect_filled(body, 1.5 * scale, color);
            // The shackle: an arch over the body, lifted to one side when
            // open.
            let lift = if icon == Icon::Unlock { -0.35 } else { 0.0 };
            let shift = if icon == Icon::Unlock { 0.45 } else { 0.0 };
            let arch: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
                    p(shift + 0.42 * a.cos(), lift - 0.4 + 0.45 * a.sin())
                })
                .collect();
            let mut shackle = vec![p(shift - 0.42, -0.05)];
            shackle.extend(arch);
            if icon == Icon::Lock {
                shackle.push(p(0.42, -0.05));
            }
            painter.add(Shape::line(shackle, stroke));
        }
        Icon::Grip => {
            for x in [-0.3, 0.3] {
                for y in [-0.6, 0.0, 0.6] {
                    painter.circle_filled(p(x, y), 1.1 * scale, color);
                }
            }
        }
        Icon::SplitColumns | Icon::SplitRows => {
            let frame = Rect::from_min_max(p(-0.95, -0.8), p(0.95, 0.8));
            let thin = Stroke::new(1.2 * scale, color);
            painter.rect_stroke(frame, 2.0 * scale, thin, egui::StrokeKind::Middle);
            if matches!(icon, Icon::SplitColumns) {
                painter.line_segment([p(0.0, -0.8), p(0.0, 0.8)], thin);
            } else {
                painter.line_segment([p(-0.95, 0.0), p(0.95, 0.0)], thin);
            }
        }
        Icon::More => {
            for x in [-0.6, 0.0, 0.6] {
                painter.circle_filled(p(x, 0.0), 1.1 * scale, color);
            }
        }
        Icon::Folder => {
            // A tab on the top left, then the body.
            let outline = vec![
                p(-0.85, -0.55),
                p(-0.3, -0.55),
                p(-0.15, -0.35),
                p(0.85, -0.35),
                p(0.85, 0.65),
                p(-0.85, 0.65),
            ];
            painter.add(Shape::closed_line(outline, Stroke::new(1.3 * scale, color)));
        }
        Icon::Gear => {
            let teeth = 8;
            let thick = Stroke::new(2.4 * scale, color);
            for i in 0..teeth {
                let a = i as f32 / teeth as f32 * std::f32::consts::TAU;
                let (sin, cos) = a.sin_cos();
                painter.line_segment([p(0.5 * cos, 0.5 * sin), p(0.92 * cos, 0.92 * sin)], thick);
            }
            painter.circle_stroke(c, 0.5 * s, thick);
        }
    }
}

/// A button the size of egui's own, with a dot of `dot` colour before
/// `text` and an optional `fill` (else the usual button colours). The dot
/// is drawn, so it sits on the text's middle line whatever the font.
pub fn dot_button(
    ui: &mut egui::Ui,
    dot: Color32,
    text: &str,
    fill: Option<Color32>,
    text_color: Option<Color32>,
) -> egui::Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        font,
        text_color.unwrap_or(ui.visuals().text_color()),
    );
    let pad = ui.spacing().button_padding;
    let dot_w = 12.0;
    let size = Vec2::new(
        pad.x * 2.0 + dot_w + galley.size().x,
        (galley.size().y + pad.y * 2.0).max(ui.spacing().interact_size.y),
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let bg = match fill {
            Some(f) if response.hovered() => f.gamma_multiply(1.15),
            Some(f) => f,
            None => visuals.weak_bg_fill,
        };
        ui.painter().rect(
            rect,
            visuals.corner_radius,
            bg,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let center = Pos2::new(rect.left() + pad.x + 4.0, rect.center().y);
        ui.painter().circle_filled(center, 4.0, dot);
        let text_pos = Pos2::new(
            rect.left() + pad.x + dot_w,
            rect.center().y - galley.size().y / 2.0,
        );
        let color = text_color.unwrap_or(visuals.text_color());
        ui.painter().galley(text_pos, galley, color);
    }
    response
}
