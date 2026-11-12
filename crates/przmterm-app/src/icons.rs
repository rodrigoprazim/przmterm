//! The interface's icons: [Lucide](https://lucide.dev) (ISC licence, in
//! `assets/fonts/Lucide-LICENSE.txt`), from a font cut down to the icons
//! used here (`scripts/lucide-icons.py`). Being a font, an icon takes any
//! colour and looks the same on every system.

use eframe::egui::{self, Color32, FontFamily, FontId, Pos2, Rect, Sense, Vec2};

/// The font family of the icons.
const FAMILY: &str = "lucide-icons";

/// Adds the icon font to `fonts`, as a family of its own.
pub fn add_font(fonts: &mut egui::FontDefinitions) {
    fonts.font_data.insert(
        FAMILY.into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../assets/fonts/lucide-icons.ttf"
        ))),
    );
    fonts
        .families
        .insert(FontFamily::Name(FAMILY.into()), vec![FAMILY.into()]);
}

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
    /// Something that can be dragged.
    Grip,
    /// A folder of sessions.
    Folder,
    /// An SSH session: a terminal.
    Session,
    /// A Telnet session: a screen.
    Telnet,
    /// A serial session: a cable.
    Serial,
    /// A local shell: a laptop.
    Local,
    /// Split side by side.
    SplitColumns,
    /// Split one above the other.
    SplitRows,
    /// More actions: three dots.
    More,
    /// A connection that is up.
    Connected,
    /// A connection that has ended.
    Disconnected,
    /// Output coming in now (a tab not looked at).
    Busy,
    /// Output came in and stopped (a tab not looked at): a command ended.
    Done,
}

impl Icon {
    /// The icon's character in the Lucide font.
    fn glyph(self) -> char {
        let code = match self {
            Icon::ChevronLeft => 0xE06E,
            Icon::ChevronRight => 0xE06F,
            Icon::ChevronDown => 0xE06D,
            Icon::Reload => 0xE149,
            Icon::Reset => 0xE148,
            Icon::Lock => 0xE10B,
            Icon::Unlock => 0xE10C,
            Icon::Gear => 0xE154,
            Icon::Plus => 0xE13D,
            Icon::Close => 0xE1B2,
            Icon::Up => 0xE04A,
            Icon::Grip => 0xE0EB,
            Icon::Folder => 0xE0D7,
            Icon::Session => 0xE20A,
            Icon::Telnet => 0xE11D,
            Icon::Serial => 0xE4E3,
            Icon::Local => 0xE1CD,
            Icon::SplitColumns => 0xE098,
            Icon::SplitRows => 0xE439,
            Icon::More => 0xE0B6,
            Icon::Connected => 0xE37F,
            Icon::Disconnected => 0xE45D,
            Icon::Busy => 0xE038,
            Icon::Done => 0xE42B,
        };
        char::from_u32(code).unwrap_or('?')
    }

    /// The icon of a session of `protocol`.
    pub fn of_protocol(protocol: przmterm_core::Protocol) -> Icon {
        match protocol {
            przmterm_core::Protocol::Ssh => Icon::Session,
            przmterm_core::Protocol::Telnet => Icon::Telnet,
            przmterm_core::Protocol::Serial => Icon::Serial,
            przmterm_core::Protocol::Local => Icon::Local,
        }
    }

    #[cfg(test)]
    const ALL: [Icon; 24] = [
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::Reload,
        Icon::Lock,
        Icon::Unlock,
        Icon::Gear,
        Icon::Plus,
        Icon::Close,
        Icon::Reset,
        Icon::Up,
        Icon::ChevronDown,
        Icon::Grip,
        Icon::Folder,
        Icon::Session,
        Icon::Telnet,
        Icon::Serial,
        Icon::Local,
        Icon::SplitColumns,
        Icon::SplitRows,
        Icon::More,
        Icon::Connected,
        Icon::Disconnected,
        Icon::Busy,
        Icon::Done,
    ];
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

/// Draws `icon` centred in `rect` in `color`, about 12 points big
/// whatever the size of `rect`.
pub fn draw(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32) {
    draw_scaled(painter, icon, rect, color, 1.0);
}

/// [`draw`], `scale` times as big.
pub fn draw_scaled(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32, scale: f32) {
    // The glyph fills about 20 of the font's 24 units, centred.
    let font = FontId::new(14.0 * scale, FontFamily::Name(FAMILY.into()));
    let galley = painter.layout_no_wrap(icon.glyph().to_string(), font, color);
    let size = galley.size();
    let pos = Pos2::new(
        rect.center().x - size.x / 2.0,
        rect.center().y - size.y / 2.0,
    );
    painter.galley(pos, galley, color);
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

#[cfg(test)]
mod tests {
    use super::*;
    use skrifa::MetadataProvider as _;

    #[test]
    fn the_font_has_every_icon() {
        let font =
            skrifa::FontRef::new(include_bytes!("../../../assets/fonts/lucide-icons.ttf")).unwrap();
        let charmap = font.charmap();
        for icon in Icon::ALL {
            assert!(charmap.map(icon.glyph()).is_some(), "{icon:?}");
        }
    }
}
