//! The Highlights window: the text one session marks, each in its colour,
//! for as long as it is open. Changes show at once.

use eframe::egui;

use crate::icons::{self, Icon};
use crate::marks::{COLORS, Highlights, Rule};

pub struct HighlightsDialog {
    /// The tab (by id) and pane whose highlights these are.
    pub tab: u64,
    pub pane: usize,
    /// Put the keyboard in the last rule's text.
    focus_last: bool,
    /// Saving as a keyword set: the name being typed.
    save_name: Option<String>,
    /// Put the keyboard in the name.
    focus_name: bool,
    /// A colour menu was open last frame: Esc closes only it.
    menu_open: bool,
}

pub enum Out {
    Open,
    Close,
    /// Save the highlights as the keyword set of this name.
    Save(String),
}

impl HighlightsDialog {
    pub fn new(tab: u64, pane: usize, focus_last: bool) -> Self {
        Self {
            tab,
            pane,
            focus_last,
            save_name: None,
            focus_name: false,
            menu_open: false,
        }
    }

    /// `session`: whose highlights, for the title; `sets`: the keyword
    /// sets there are (`None`: no settings folder to save one in).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        session: &str,
        highlights: &mut Highlights,
        sets: Option<&[String]>,
    ) -> Out {
        let mut out = Out::Open;
        let mut open = true;
        let mut rules = highlights.rules().to_vec();
        let mut remove = None;
        let errors: Vec<Option<String>> = (0..rules.len())
            .map(|i| highlights.error(i).map(str::to_string))
            .collect();
        egui::Window::new(crate::theme::window_title(ctx, "Highlights"))
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_width(600.0);
                ui.label(format!("Text marked in {session} while it is open."));
                ui.weak(
                    "Shown over the program's own colours. Text is found anywhere in a line; \
                     with .* it is a regular expression.",
                );
                ui.add_space(8.0);
                let last = rules.len().saturating_sub(1);
                for (i, rule) in rules.iter_mut().enumerate() {
                    let focus = self.focus_last && i == last;
                    if rule_row(ui, i, rule, focus) {
                        remove = Some(i);
                    }
                    if let Some(Some(e)) = errors.get(i) {
                        ui.horizontal(|ui| {
                            ui.add_space(26.0);
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        });
                    }
                }
                self.focus_last = false;
                if rules.is_empty() {
                    ui.weak("None yet.");
                }
                ui.add_space(4.0);
                if icons::icon_text_button(
                    ui,
                    Icon::Plus,
                    ui.visuals().text_color(),
                    "Add",
                    None,
                    None,
                )
                .clicked()
                {
                    rules.push(Rule::new("", false, false, highlights.next_color()));
                    self.focus_last = true;
                }
                ui.add_space(8.0);
                ui.separator();
                let usable = rules.iter().any(|r| !r.text.is_empty());
                match (&mut self.save_name, sets) {
                    (Some(name), Some(sets)) => {
                        let field = ui
                            .horizontal(|ui| {
                                ui.label("Keyword set");
                                crate::editor::name_field(ui, name, "name", f32::INFINITY)
                            })
                            .inner;
                        if std::mem::take(&mut self.focus_name) {
                            field.request_focus();
                        }
                        let entered =
                            field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let trimmed = name.trim().to_string();
                        if sets.iter().any(|s| s.eq_ignore_ascii_case(&trimmed)) {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "A keyword set of this name exists: it will be replaced.",
                            );
                        }
                        ui.weak(
                            "Choose it in a session's or folder's settings (Keywords) to \
                             highlight the same everywhere.",
                        );
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let save = ui
                                        .add_enabled_ui(!trimmed.is_empty() && usable, |ui| {
                                            crate::theme::primary_button(ui, "Save")
                                        })
                                        .inner;
                                    if save.clicked() || (entered && !trimmed.is_empty() && usable)
                                    {
                                        out = Out::Save(trimmed.clone());
                                    }
                                    if ui.button("Cancel").clicked() {
                                        self.save_name = None;
                                    }
                                },
                            );
                        });
                    }
                    _ => {
                        ui.horizontal(|ui| {
                            if sets.is_some()
                                && ui
                                    .add_enabled(usable, egui::Button::new("Save as keyword set…"))
                                    .on_hover_text(
                                        "Keep these highlights to use in other sessions too",
                                    )
                                    .clicked()
                            {
                                self.save_name = Some(String::new());
                                self.focus_name = true;
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if crate::theme::primary_button(ui, "Close").clicked() {
                                        out = Out::Close;
                                    }
                                },
                            );
                        });
                    }
                }
            });
        if let Some(i) = remove {
            rules.remove(i);
        }
        highlights.set(rules);
        // Esc in the colours closes only them.
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !self.menu_open;
        self.menu_open = egui::Popup::is_any_open(ctx);
        if !open || (escape && self.save_name.is_none()) {
            out = Out::Close;
        }
        if let Out::Save(_) = out {
            self.save_name = None;
        }
        out
    }
}

/// One highlight: on, text, case, regex, colour, text or line, alert,
/// remove (returned).
fn rule_row(ui: &mut egui::Ui, i: usize, rule: &mut Rule, focus: bool) -> bool {
    let mut remove = false;
    ui.horizontal(|ui| {
        crate::theme::checkbox(ui, &mut rule.on, "").on_hover_text("On");
        let field = ui.add(
            egui::TextEdit::singleline(&mut rule.text)
                .hint_text("text to highlight")
                .font(egui::TextStyle::Monospace)
                .desired_width(200.0),
        );
        if focus {
            field.request_focus();
        }
        if icons::button(ui, Icon::MatchCase, rule.match_case)
            .on_hover_text("Match case")
            .clicked()
        {
            rule.match_case = !rule.match_case;
        }
        if icons::button(ui, Icon::Regex, rule.regex)
            .on_hover_text("Regular expression")
            .clicked()
        {
            rule.regex = !rule.regex;
        }
        ui.add_space(4.0);
        color_button(ui, i, &mut rule.color);
        ui.add_space(4.0);
        if let Some(pick) =
            crate::theme::segmented(ui, Some(usize::from(rule.whole_line)), &["Text", "Line"])
        {
            rule.whole_line = pick == 1;
        }
        ui.add_space(4.0);
        if icons::button(ui, Icon::Done, rule.alert)
            .on_hover_text(if rule.alert {
                "Marks the tab when this turns up while you are on another tab. Click to stop."
            } else {
                "Click to mark the tab when this turns up while you are on another tab"
            })
            .clicked()
        {
            rule.alert = !rule.alert;
        }
        if icons::button(ui, Icon::Delete, false)
            .on_hover_text("Remove")
            .clicked()
        {
            remove = true;
        }
    });
    remove
}

/// A swatch of the colour; clicked, the colours to choose from.
fn color_button(ui: &mut egui::Ui, i: usize, color: &mut [u8; 3]) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(34.0, 20.0), egui::Sense::click());
    let [r, g, b] = *color;
    let fill = egui::Color32::from_rgb(r, g, b);
    let chip = egui::Rect::from_center_size(rect.center(), egui::vec2(30.0, 16.0));
    ui.painter().rect_filled(chip, 4, fill);
    if response.hovered() {
        ui.painter().rect_stroke(
            chip.expand(1.5),
            5,
            egui::Stroke::new(1.0, ui.visuals().strong_text_color()),
            egui::StrokeKind::Outside,
        );
    }
    let response = response
        .on_hover_text("Colour")
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    // Stays open while the palette is dragged; a swatch closes it.
    egui::Popup::menu(&response)
        .id(egui::Id::unique(("highlight-colour", i)))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (name, c) in COLORS {
                    let (rect, swatch) =
                        ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
                    let center = rect.center();
                    ui.painter().circle_filled(
                        center,
                        8.0,
                        egui::Color32::from_rgb(c[0], c[1], c[2]),
                    );
                    if *color == c || swatch.hovered() {
                        ui.painter().circle_stroke(
                            center,
                            10.5,
                            egui::Stroke::new(1.5, ui.visuals().strong_text_color()),
                        );
                    }
                    if swatch.on_hover_text(name).clicked() {
                        *color = c;
                        ui.close();
                    }
                }
            });
            ui.add_space(4.0);
            ui.weak("Or any colour:");
            // As wide as the row of swatches above it.
            let swatches = COLORS.len() as f32 * 22.0 + (COLORS.len() - 1) as f32 * 4.0;
            ui.spacing_mut().slider_width = swatches;
            let mut picked = egui::Color32::from_rgb(color[0], color[1], color[2]);
            if egui::widgets::color_picker::color_picker_color32(
                ui,
                &mut picked,
                egui::widgets::color_picker::Alpha::Opaque,
            ) {
                *color = [picked.r(), picked.g(), picked.b()];
            }
        });
}
