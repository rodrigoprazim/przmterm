//! PrzmTerm's look: the colours, sizes and fonts of the interface, in the
//! spirit of today's dark editors (flat surfaces, thin borders, one blue
//! accent). The terminal's own colours belong to the sessions.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// The window's own surfaces: the side panel, the bars.
pub const CHROME: Color32 = rgb(25, 26, 27);
/// The tab strip, dialogs and menus.
pub const RAISED: Color32 = rgb(32, 33, 34);
/// Text fields and lists inside dialogs.
pub const SUNKEN: Color32 = rgb(18, 19, 20);
/// Lines between areas.
pub const BORDER: Color32 = rgb(43, 44, 45);
/// Borders of controls (fields, buttons), a little lighter.
pub const CONTROL_BORDER: Color32 = rgb(58, 59, 61);
pub const HOVER: Color32 = rgb(45, 46, 48);
pub const PRESSED: Color32 = rgb(55, 56, 58);
pub const TEXT: Color32 = rgb(204, 204, 204);
pub const TEXT_STRONG: Color32 = rgb(240, 240, 240);
pub const TEXT_WEAK: Color32 = rgb(140, 141, 143);
/// The accent: focus, selection, links.
pub const ACCENT: Color32 = rgb(0, 120, 212);
pub const ACCENT_TEXT: Color32 = rgb(55, 148, 255);
/// A selected row or item.
pub const SELECTED: Color32 = rgb(4, 57, 94);

/// The family of the semibold interface font (titles).
pub fn semibold() -> FontFamily {
    FontFamily::Name("ui-semibold".into())
}

/// Puts Inter first for the interface text (egui's own fonts stay as
/// fallbacks) and adds its semibold for titles.
pub fn add_fonts(fonts: &mut egui::FontDefinitions) {
    let regular = "Inter";
    let bold = "Inter-SemiBold";
    fonts.font_data.insert(
        regular.into(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        bold.into(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../../../assets/fonts/Inter-SemiBold.ttf"
        ))),
    );
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, regular.into());
    let fallbacks: Vec<String> = proportional[1..].to_vec();
    let mut titles = vec![bold.to_string()];
    titles.extend(fallbacks);
    fonts.families.insert(semibold(), titles);
}

/// The strip the session tabs sit on: the tabs reach its bottom edge, so
/// the active one joins the terminal below.
pub fn tab_strip() -> egui::Frame {
    egui::Frame::new().fill(RAISED).inner_margin(Margin {
        left: 6,
        right: 6,
        top: 4,
        bottom: 0,
    })
}

/// Sets the interface's colours, sizes and spacing.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        use egui::TextStyle;
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Body, FontId::proportional(13.0)),
            (TextStyle::Button, FontId::proportional(13.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
            (TextStyle::Heading, FontId::new(13.5, semibold())),
        ]
        .into();

        let spacing = &mut style.spacing;
        spacing.item_spacing = egui::vec2(8.0, 6.0);
        spacing.button_padding = egui::vec2(10.0, 4.0);
        spacing.interact_size = egui::vec2(36.0, 24.0);
        spacing.window_margin = Margin::same(14);
        spacing.menu_margin = Margin::same(5);
        spacing.indent = 16.0;
        spacing.icon_width = 14.0;
        spacing.icon_width_inner = 8.0;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = CHROME;
        v.window_fill = RAISED;
        v.window_stroke = Stroke::new(1.0, CONTROL_BORDER);
        v.window_corner_radius = CornerRadius::same(8);
        v.window_shadow = Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(140),
        };
        v.popup_shadow = Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(120),
        };
        v.menu_corner_radius = CornerRadius::same(6);
        v.extreme_bg_color = SUNKEN;
        v.text_edit_bg_color = Some(SUNKEN);
        v.faint_bg_color = RAISED;
        v.code_bg_color = SUNKEN;
        v.hyperlink_color = ACCENT_TEXT;
        v.selection.bg_fill = SELECTED;
        v.selection.stroke = Stroke::new(1.0, ACCENT_TEXT);
        v.striped = false;
        v.indent_has_left_vline = false;
        v.collapsing_header_frame = false;

        let radius = CornerRadius::same(4);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CHROME;
        w.noninteractive.weak_bg_fill = CHROME;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = radius;

        w.inactive.bg_fill = SUNKEN;
        w.inactive.weak_bg_fill = rgb(42, 43, 45);
        w.inactive.bg_stroke = Stroke::new(1.0, CONTROL_BORDER);
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = radius;
        w.inactive.expansion = 0.0;

        w.hovered.bg_fill = HOVER;
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0, rgb(78, 79, 82));
        w.hovered.fg_stroke = Stroke::new(1.5, TEXT_STRONG);
        w.hovered.corner_radius = radius;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = PRESSED;
        w.active.weak_bg_fill = PRESSED;
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        w.active.fg_stroke = Stroke::new(1.5, TEXT_STRONG);
        w.active.corner_radius = radius;
        w.active.expansion = 0.0;

        w.open.bg_fill = HOVER;
        w.open.weak_bg_fill = HOVER;
        w.open.bg_stroke = Stroke::new(1.0, CONTROL_BORDER);
        w.open.fg_stroke = Stroke::new(1.0, TEXT_STRONG);
        w.open.corner_radius = radius;
    });
}
