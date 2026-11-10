//! PrzmTerm's look: the colours, sizes and fonts of the interface, in the
//! spirit of today's dark editors (flat surfaces, thin borders, one blue
//! accent). The terminal's own colours belong to the sessions.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, IntoAtoms as _, Margin, Shadow, Stroke,
};

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
/// A tab under the pointer, a shade above the tab strip.
pub const TAB_HOVER: Color32 = rgb(39, 40, 42);
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

/// The width of a text field or list in a form, so their right edges line
/// up.
pub const FIELD_WIDTH: f32 = 280.0;

/// The width of a combo box matching a field of [`FIELD_WIDTH`] (both
/// widths include the padding).
pub fn combo_width(_ui: &egui::Ui) -> f32 {
    FIELD_WIDTH
}

/// Combo boxes added to `ui` from now on look like text fields (sunken),
/// not like buttons; call it in the row that holds the combo box.
pub fn sunken_combos(ui: &mut egui::Ui) {
    let w = &mut ui.visuals_mut().widgets;
    w.inactive.weak_bg_fill = SUNKEN;
    w.open.weak_bg_fill = SUNKEN;
}

/// The button of a window's main action (Save, OK…), in the accent.
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for (state, fill) in [
            (&mut w.inactive, ACCENT),
            (&mut w.hovered, rgb(26, 137, 224)),
            (&mut w.active, rgb(0, 100, 180)),
        ] {
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        }
        ui.button(text)
    })
    .inner
}

/// One choice of several, as a row of joined segments (the chosen one in
/// the accent). Returns the index clicked, if any.
pub fn segmented(ui: &mut egui::Ui, selected: Option<usize>, options: &[&str]) -> Option<usize> {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let pad = 12.0;
    let height = ui.spacing().interact_size.y;
    let galleys: Vec<_> = options
        .iter()
        .map(|text| {
            ui.painter()
                .layout_no_wrap(text.to_string(), font.clone(), TEXT)
        })
        .collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 2.0 * pad).collect();
    let (rect, whole) = ui.allocate_exact_size(
        egui::vec2(widths.iter().sum(), height),
        egui::Sense::hover(),
    );
    let mut clicked = None;
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 4, SUNKEN);
    let mut x = rect.left();
    for (i, (galley, w)) in galleys.into_iter().zip(&widths).enumerate() {
        let cell = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(*w, height));
        let response = ui
            .interact(cell, whole.id.with(i), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let chosen = selected == Some(i);
        if chosen {
            painter.rect_filled(cell.shrink(2.0), 3, ACCENT);
        } else if response.hovered() {
            painter.rect_filled(cell.shrink(2.0), 3, HOVER);
        }
        let color = if chosen || response.hovered() {
            Color32::WHITE
        } else {
            TEXT
        };
        painter.galley(cell.center() - galley.size() / 2.0, galley, color);
        if response.clicked() {
            clicked = Some(i);
        }
        x += w;
    }
    painter.rect_stroke(
        rect,
        4,
        Stroke::new(1.0, CONTROL_BORDER),
        egui::StrokeKind::Inside,
    );
    clicked
}

/// A section's title in a form: larger and bolder than the labels, with a
/// line under it across the form.
pub fn section_title(ui: &mut egui::Ui, title: &str) {
    section_header(ui, title, |_| {});
}

/// [`section_title`] with more after the title on its row (a ? button).
pub fn section_header(ui: &mut egui::Ui, title: &str, after: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .font(FontId::new(14.5, semibold()))
                .color(TEXT_STRONG),
        );
        after(ui);
    });
    // The line goes under the whole row, from the form's left edge to its
    // right edge, whatever the row holds.
    let rect = ui.available_rect_before_wrap();
    ui.painter().hline(
        rect.left()..=rect.right(),
        rect.top() + 1.0,
        Stroke::new(1.0, BORDER),
    );
    ui.add_space(8.0);
}

/// A window title that sits in the middle of the title bar: egui centres
/// it between the left edge and the close button, so a blank as wide as
/// the button goes first. For windows with a close button.
pub fn window_title(ctx: &egui::Context, text: impl Into<String>) -> egui::Atoms<'static> {
    let side = ctx.fonts_mut(|f| f.row_height(&FontId::new(13.5, semibold())));
    (
        egui::Atom::custom(
            egui::IdSalt::new("przm-title-balance"),
            egui::vec2(side, side),
        ),
        text.into(),
    )
        .into_atoms()
}

/// A check box: a rounded square, filled with the accent and a white tick
/// when checked. Toggles `checked` when clicked.
pub fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    let response = mark_and_text(
        ui,
        text,
        |painter, rect, on, hovered| {
            let square = egui::Rect::from_center_size(rect.center(), egui::vec2(15.0, 15.0));
            if on {
                painter.rect_filled(square, 3, ACCENT);
                painter.add(egui::Shape::line(
                    vec![
                        square.lerp_inside(egui::vec2(0.22, 0.52)),
                        square.lerp_inside(egui::vec2(0.42, 0.72)),
                        square.lerp_inside(egui::vec2(0.78, 0.30)),
                    ],
                    Stroke::new(1.8, Color32::WHITE),
                ));
            } else {
                painter.rect_filled(square, 3, SUNKEN);
                let border = if hovered { TEXT_WEAK } else { CONTROL_BORDER };
                painter.rect_stroke(
                    square,
                    3,
                    Stroke::new(1.0, border),
                    egui::StrokeKind::Inside,
                );
            }
        },
        *checked,
    );
    let mut response = response;
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    response
}

/// A radio button: a circle, filled with the accent and a white dot when
/// chosen. Returns its response (clicked: choose it).
pub fn radio(ui: &mut egui::Ui, chosen: bool, text: impl Into<egui::WidgetText>) -> egui::Response {
    mark_and_text(
        ui,
        text,
        |painter, rect, on, hovered| {
            let c = rect.center();
            if on {
                painter.circle_filled(c, 7.5, ACCENT);
                painter.circle_filled(c, 3.0, Color32::WHITE);
            } else {
                painter.circle_filled(c, 7.5, SUNKEN);
                let border = if hovered { TEXT_WEAK } else { CONTROL_BORDER };
                painter.circle_stroke(c, 7.0, Stroke::new(1.0, border));
            }
        },
        chosen,
    )
}

/// A mark (box or circle, drawn by `paint` in a 16-point square) followed
/// by `text`, clickable as a whole.
fn mark_and_text(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    paint: impl FnOnce(&egui::Painter, egui::Rect, bool, bool),
    on: bool,
) -> egui::Response {
    let text: egui::WidgetText = text.into();
    let galley = text.into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Body,
    );
    let mark = 16.0;
    let gap = if galley.size().x > 0.0 { 7.0 } else { 0.0 };
    let size = egui::vec2(mark + gap + galley.size().x, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let mark_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.center().y - mark / 2.0),
            egui::vec2(mark, mark),
        );
        paint(ui.painter(), mark_rect, on, response.hovered());
        let color = if response.hovered() {
            TEXT_STRONG
        } else {
            TEXT
        };
        ui.painter().galley(
            egui::pos2(
                mark_rect.right() + gap,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            color,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
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
        // The title bar takes the top and bottom: kept low. Inter's capitals
        // sit above the middle of its line, so the top gets a little more.
        spacing.window_margin = Margin {
            left: 14,
            right: 14,
            top: 9,
            bottom: 8,
        };
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
