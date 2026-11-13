//! A tab's find bar: text (or a regular expression) looked for in the
//! session's history and screen, every match marked, one at a time looked
//! at. In a split tab it searches the pane with the keyboard, or all of
//! them.

use std::rc::Rc;

use eframe::egui;
use przmterm_term::search::Pattern;

use crate::icons::{self, Icon};
use crate::pane::Pane;

#[derive(Default)]
pub struct FindBar {
    pub open: bool,
    pub text: String,
    pub match_case: bool,
    pub regex: bool,
    /// Every pane of a split tab, not only the one with the keyboard.
    pub all_panes: bool,
    pattern: Option<Rc<Pattern>>,
    /// What `pattern` was made from.
    made_from: Option<(String, bool, bool)>,
    /// Why the text is not a usable regular expression.
    error: Option<String>,
    /// Look at the newest match once the panes have searched.
    jump: bool,
    /// Put the keyboard in the text field.
    focus: bool,
    /// The pane holding the match looked at.
    current_pane: Option<usize>,
}

/// What the bar asks of the app.
pub enum BarAction {
    None,
    /// Closed: the keyboard goes back to the terminal.
    Closed,
    /// Highlight the text searched for, in these panes.
    Highlight(Vec<usize>),
}

impl FindBar {
    /// Opens the bar, with `text` to look for if there is one.
    pub fn open_with(&mut self, text: &str) {
        self.open = true;
        self.focus = true;
        if !text.is_empty() {
            self.text = text.to_string();
        }
    }

    fn close(&mut self) {
        self.open = false;
        self.current_pane = None;
    }

    /// Whether pane `i` of `n` is searched.
    fn searches(&self, i: usize, n: usize, active: usize) -> bool {
        n == 1 || self.all_panes || i == active
    }

    /// Before the panes are drawn: gives each the search, or none.
    pub fn prepare(&mut self, panes: &mut [Pane], active: usize) {
        let key = (self.text.clone(), self.regex, self.match_case);
        if self.open && self.made_from.as_ref() != Some(&key) {
            match Pattern::new(&self.text, self.regex, self.match_case) {
                Ok(p) => {
                    self.pattern = p.map(Rc::new);
                    self.error = None;
                }
                Err(e) => {
                    self.pattern = None;
                    self.error = Some(e);
                }
            }
            self.made_from = Some(key);
            self.jump = true;
            self.current_pane = None;
        }
        let n = panes.len();
        for (i, pane) in panes.iter_mut().enumerate() {
            let searched = self.open && self.searches(i, n, active);
            pane.view
                .marks
                .found
                .set_pattern(searched.then(|| self.pattern.clone()).flatten());
        }
    }

    /// The matches in order: pane, and the match in it.
    fn order(&self, panes: &[Pane], active: usize) -> Vec<(usize, usize)> {
        let n = panes.len();
        panes
            .iter()
            .enumerate()
            .filter(|(i, _)| self.searches(*i, n, active))
            .flat_map(|(i, p)| (0..p.view.marks.found.len()).map(move |h| (i, h)))
            .collect()
    }

    /// Makes `at` the match looked at, and scrolls its pane to it.
    fn look_at(&mut self, panes: &mut [Pane], at: (usize, usize)) {
        for pane in panes.iter_mut() {
            pane.view.marks.found.set_current(None);
        }
        let (p, h) = at;
        let pane = &mut panes[p];
        pane.view.marks.found.set_current(Some(h));
        if let Some(row) = pane.view.marks.found.current_row(pane.term.screen()) {
            pane.view.reveal(&mut pane.term, row);
        }
        self.current_pane = Some(p);
    }

    /// Goes to the next older (`older`) or newer match.
    fn step(&mut self, panes: &mut [Pane], active: usize, older: bool) {
        let order = self.order(panes, active);
        if order.is_empty() {
            return;
        }
        let now = self.current_pane.and_then(|p| {
            let h = panes.get(p)?.view.marks.found.current()?;
            order.iter().position(|&x| x == (p, h))
        });
        let next = match (now, older) {
            (Some(i), true) => (i + order.len() - 1) % order.len(),
            (Some(i), false) => (i + 1) % order.len(),
            (None, _) => order.len() - 1,
        };
        self.look_at(panes, order[next]);
    }

    /// After the panes are drawn: the bar, at the top right of `area`.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        area: egui::Rect,
        top: f32,
        panes: &mut [Pane],
        active: usize,
    ) -> BarAction {
        if !self.open {
            return BarAction::None;
        }
        let order = self.order(panes, active);
        // A new search: look at the newest match, in the pane with the
        // keyboard if it has one.
        if self.jump && self.pattern.is_some() {
            self.jump = false;
            let pick = order
                .iter()
                .rev()
                .find(|(p, _)| *p == active)
                .or(order.last())
                .copied();
            match pick {
                Some(at) => self.look_at(panes, at),
                None => self.current_pane = None,
            }
        }
        let current = self.current_pane.and_then(|p| {
            let h = panes.get(p)?.view.marks.found.current()?;
            order.iter().position(|&x| x == (p, h))
        });
        let n = panes.len();
        let mut action = BarAction::None;
        let mut step = None;
        let pos = egui::pos2(area.right() - 18.0, area.top() + top + 6.0);
        egui::Area::new(egui::Id::unique("find-bar"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_TOP)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                        icons::draw(
                            ui.painter(),
                            Icon::Search,
                            rect,
                            ui.visuals().weak_text_color(),
                        );
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut self.text)
                                .hint_text("Find in the session")
                                .font(egui::TextStyle::Monospace)
                                .desired_width(220.0),
                        );
                        if std::mem::take(&mut self.focus) {
                            field.request_focus();
                            // Typing replaces what was looked for before.
                            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), field.id)
                            {
                                let end = egui::text::CCursor::new(self.text.chars().count());
                                state
                                    .cursor
                                    .set_char_range(Some(egui::text::CCursorRange::two(
                                        egui::text::CCursor::new(0),
                                        end,
                                    )));
                                state.store(ui.ctx(), field.id);
                            }
                        }
                        let enter =
                            field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if enter {
                            step = Some(!ui.input(|i| i.modifiers.shift));
                            field.request_focus();
                        }
                        if field.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            self.close();
                            action = BarAction::Closed;
                        }
                        ui.add_space(4.0);
                        if icons::button(ui, Icon::MatchCase, self.match_case)
                            .on_hover_text("Match case")
                            .clicked()
                        {
                            self.match_case = !self.match_case;
                        }
                        if icons::button(ui, Icon::Regex, self.regex)
                            .on_hover_text("Regular expression")
                            .clicked()
                        {
                            self.regex = !self.regex;
                        }
                        if n > 1
                            && icons::button(ui, Icon::AllPanes, self.all_panes)
                                .on_hover_text(if self.all_panes {
                                    "Searching every pane of this tab. Click to search only the \
                                     pane with the keyboard."
                                } else {
                                    "Searching the pane with the keyboard. Click to search every \
                                     pane of this tab."
                                })
                                .clicked()
                        {
                            self.all_panes = !self.all_panes;
                            self.jump = true;
                        }
                        ui.add_space(6.0);
                        // A slot of its own, so the bar keeps its size as
                        // the count changes.
                        let (text, color, hint) = match (&self.error, order.len(), current) {
                            (Some(e), _, _) => (
                                "Bad expression".to_string(),
                                ui.visuals().error_fg_color,
                                Some(e.clone()),
                            ),
                            _ if self.text.is_empty() => {
                                (String::new(), ui.visuals().weak_text_color(), None)
                            }
                            (None, 0, _) => (
                                "No matches".to_string(),
                                ui.visuals().weak_text_color(),
                                None,
                            ),
                            (None, total, at) => {
                                let count = match at {
                                    Some(i) => format!("{} of {total}", i + 1),
                                    None => format!("{total} found"),
                                };
                                let mut panes: Vec<usize> = order.iter().map(|x| x.0).collect();
                                panes.dedup();
                                let shown = if self.all_panes && n > 1 {
                                    format!(
                                        "{count} · {} pane{}",
                                        panes.len(),
                                        if panes.len() == 1 { "" } else { "s" }
                                    )
                                } else {
                                    count
                                };
                                (shown, ui.visuals().weak_text_color(), None)
                            }
                        };
                        let width = if n > 1 { 130.0 } else { 90.0 };
                        let (slot, slot_response) =
                            ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::hover());
                        ui.painter().text(
                            slot.left_center(),
                            egui::Align2::LEFT_CENTER,
                            text,
                            egui::TextStyle::Body.resolve(ui.style()),
                            color,
                        );
                        if let Some(hint) = hint {
                            slot_response.on_hover_text(hint);
                        }
                        ui.add_space(4.0);
                        let any = !order.is_empty();
                        if ui
                            .add_enabled_ui(any, |ui| icons::button(ui, Icon::ChevronUp, false))
                            .inner
                            .on_hover_text("Older match (Enter)")
                            .clicked()
                        {
                            step = Some(true);
                        }
                        if ui
                            .add_enabled_ui(any, |ui| icons::button(ui, Icon::ChevronDown, false))
                            .inner
                            .on_hover_text("Newer match (Shift+Enter)")
                            .clicked()
                        {
                            step = Some(false);
                        }
                        let can_mark = self.pattern.is_some();
                        if ui
                            .add_enabled_ui(can_mark, |ui| {
                                icons::button(ui, Icon::Highlight, false)
                            })
                            .inner
                            .on_hover_text("Highlight this text in the session, in a colour")
                            .clicked()
                        {
                            let searched =
                                (0..n).filter(|&i| self.searches(i, n, active)).collect();
                            action = BarAction::Highlight(searched);
                        }
                        if icons::button(ui, Icon::Close, false)
                            .on_hover_text("Close (Esc)")
                            .clicked()
                        {
                            self.close();
                            action = BarAction::Closed;
                        }
                    });
                });
            });
        if let Some(older) = step {
            self.step(panes, active, older);
        }
        if !self.open {
            for pane in panes.iter_mut() {
                pane.view.marks.found.set_pattern(None);
            }
        }
        action
    }
}
