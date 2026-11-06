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
}

/// Size of an icon button: all of them match.
pub const BUTTON: Vec2 = Vec2::new(24.0, 20.0);

/// A framed button showing `icon`; `selected` draws it pressed.
pub fn button(ui: &mut egui::Ui, icon: Icon, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(BUTTON, Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        ui.painter().rect(
            rect,
            visuals.corner_radius,
            visuals.weak_bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        draw(ui.painter(), icon, rect, visuals.fg_stroke.color);
    }
    response
}

/// Draws `icon` centred in `rect` in `color`.
pub fn draw(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    let c = rect.center();
    // The icon's box: a square of 12 points, whatever the button's size.
    let s = 6.0;
    let stroke = Stroke::new(1.5, color);
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
        Icon::Plus => {
            painter.line_segment([p(-0.75, 0.0), p(0.75, 0.0)], stroke);
            painter.line_segment([p(0.0, -0.75), p(0.0, 0.75)], stroke);
        }
        Icon::Close => {
            let stroke = Stroke::new(1.6, color);
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
            painter.rect_filled(body, 1.5, color);
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
        Icon::Gear => {
            let teeth = 8;
            let thick = Stroke::new(2.4, color);
            for i in 0..teeth {
                let a = i as f32 / teeth as f32 * std::f32::consts::TAU;
                let (sin, cos) = a.sin_cos();
                painter.line_segment([p(0.5 * cos, 0.5 * sin), p(0.92 * cos, 0.92 * sin)], thick);
            }
            painter.circle_stroke(c, 0.5 * s, thick);
        }
    }
}
