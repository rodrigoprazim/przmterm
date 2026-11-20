//! The keyword sets, on the *Keywords* page of the session settings: the
//! set to edit (from a list), its rules, and a preview of how sample text
//! comes out. Choosing a set to edit does not choose it for the session.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, RichText};
use przmterm_core::keywords::{Keyword, KeywordSet, format_color, parse_color};
use przmterm_term::highlight::Highlighter;

use crate::icons::{self, Icon};

const SAMPLE: &str = "\
GigabitEthernet0/1   up      up      uplink to 10.0.0.1
GigabitEthernet0/2   down    down    admin-down
BGP peer 2001:db8::1 Established, uptime 12:34:56
Alarm: major LOS on port 3, mac 00:1a:2b:3c:4d:5e";

/// What waits for "unsaved changes will be lost?".
#[derive(Clone)]
enum Pending {
    Load(String),
    New,
}

/// What happened to the sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeywordChange {
    /// Saved (made, or its rules changed).
    Saved(String),
    /// Renamed: the files that chose it were pointed at the new name.
    Renamed(String, String),
    /// Deleted: the files that chose it no longer name it.
    Deleted(String),
}

pub struct KeywordEditor {
    config_dir: PathBuf,
    sets: Vec<String>,
    /// The set being edited, by its name on disk.
    current: Option<String>,
    /// A new set is being made (not saved yet). With no `current` either,
    /// there is nothing to edit: the page says how to start.
    creating: bool,
    name: String,
    set: KeywordSet,
    /// The set as last saved or loaded, to see unsaved changes.
    saved: KeywordSet,
    /// Who chose the set being edited (settings files), for "used by".
    users: Vec<PathBuf>,
    sample: String,
    /// Shows only rules containing this text.
    filter: String,
    /// Compiled for the preview, with the set it was built from.
    compiled: Option<(KeywordSet, Highlighter, Vec<String>)>,
    /// The "delete this set?" popup is open.
    confirm_delete: bool,
    changes: Vec<KeywordChange>,
    /// An action held back because of unsaved changes.
    pending: Option<Pending>,
    error: Option<String>,
}

impl KeywordEditor {
    /// The editor on the set `select`, else the first one. With no sets it
    /// says how to make one: no set is ever made unless asked for.
    pub fn new(config_dir: &Path, select: Option<&str>) -> Self {
        let mut editor = Self {
            config_dir: config_dir.to_path_buf(),
            sets: przmterm_session::keyword_sets(config_dir),
            current: None,
            creating: false,
            name: String::new(),
            set: KeywordSet::default(),
            saved: KeywordSet::default(),
            users: Vec::new(),
            sample: SAMPLE.to_string(),
            filter: String::new(),
            compiled: None,
            confirm_delete: false,
            changes: Vec::new(),
            pending: None,
            error: None,
        };
        let first = select
            .map(str::to_string)
            .filter(|s| editor.sets.contains(s))
            .or_else(|| editor.sets.first().cloned());
        if let Some(name) = first {
            editor.load(&name);
        }
        editor
    }

    /// The sets saved, renamed or deleted since the last call.
    pub fn take_changes(&mut self) -> Vec<KeywordChange> {
        std::mem::take(&mut self.changes)
    }

    /// The delete popup is open (Esc is its own).
    pub fn modal_open(&self) -> bool {
        self.confirm_delete
    }

    /// Saves the set being edited if it changed (with the session).
    pub fn save_if_dirty(&mut self) -> Result<(), String> {
        if self.dirty() {
            self.save()
                .map_err(|e| format!("Keyword set {}: {e}", self.name.trim()))
        } else {
            Ok(())
        }
    }

    fn dirty(&self) -> bool {
        if self.creating {
            // A new set matters once it has a rule.
            return self
                .set
                .rules
                .iter()
                .any(|k| !k.text.trim().is_empty() || !k.regex.trim().is_empty());
        }
        self.current.is_some()
            && (self.set != self.saved || self.current.as_deref() != Some(self.name.trim()))
    }

    fn load(&mut self, name: &str) {
        match przmterm_session::read_keyword_set(&self.config_dir, name) {
            Ok(set) => {
                self.current = Some(name.to_string());
                self.creating = false;
                self.name = name.to_string();
                self.saved = set.clone();
                self.set = set;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self.users = przmterm_session::keyword_set_users(&self.config_dir, name);
        self.confirm_delete = false;
    }

    /// Nothing chosen: the first set, or the "make one" page.
    fn load_first(&mut self) {
        match self.sets.first().cloned() {
            Some(first) => self.load(&first),
            None => {
                self.current = None;
                self.creating = false;
                self.users.clear();
            }
        }
    }

    fn new_set(&mut self) {
        let mut name = "New set".to_string();
        let mut n = 2;
        while self.sets.contains(&name) {
            name = format!("New set {n}");
            n += 1;
        }
        self.current = None;
        self.creating = true;
        self.name = name;
        self.set = KeywordSet {
            match_case: false,
            rules: vec![Keyword {
                color: Some("#ffff00".into()),
                ..Keyword::default()
            }],
            ..KeywordSet::default()
        };
        self.saved = KeywordSet::default();
        self.users.clear();
        self.confirm_delete = false;
    }

    fn save(&mut self) -> Result<(), String> {
        let name = self.name.trim().to_string();
        przmterm_session::validate_name(&name)?;
        if self.current.as_deref() != Some(&name) && self.sets.contains(&name) {
            return Err(format!("a set called {name} already exists"));
        }
        let rules: Vec<Keyword> = self
            .set
            .rules
            .iter()
            .filter(|k| !k.text.trim().is_empty() || !k.regex.trim().is_empty())
            .cloned()
            .collect();
        let set = KeywordSet {
            match_case: self.set.match_case,
            rules,
            ..KeywordSet::default()
        };
        let (_, problems) = Highlighter::new(&set);
        if let Some(p) = problems.first() {
            return Err(p.clone());
        }
        przmterm_session::write_keyword_set(&self.config_dir, &name, &set)
            .map_err(|e| e.to_string())?;
        if let Some(old) = self.current.clone()
            && old != name
        {
            // The sessions that chose it follow the new name.
            przmterm_session::rename_keyword_references(&self.config_dir, &old, Some(&name))
                .map_err(|e| e.to_string())?;
            let _ = przmterm_session::delete_keyword_set(&self.config_dir, &old);
            self.changes.push(KeywordChange::Renamed(old, name.clone()));
        }
        self.changes.push(KeywordChange::Saved(name.clone()));
        self.sets = przmterm_session::keyword_sets(&self.config_dir);
        self.current = Some(name.clone());
        self.creating = false;
        self.name = name.clone();
        self.set = set.clone();
        self.saved = set;
        self.users = przmterm_session::keyword_set_users(&self.config_dir, &name);
        Ok(())
    }

    fn delete(&mut self) {
        let Some(name) = self.current.clone() else {
            return;
        };
        match przmterm_session::delete_keyword_set(&self.config_dir, &name) {
            Ok(()) => {
                self.changes.push(KeywordChange::Deleted(name));
                self.sets = przmterm_session::keyword_sets(&self.config_dir);
                self.load_first();
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self.confirm_delete = false;
    }

    /// The editor, in the page.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.header(ui);
        self.delete_popup(ui.ctx());
        ui.add_space(6.0);
        if self.current.is_none() && !self.creating {
            self.empty_ui(ui);
        } else {
            self.set_ui(ui);
        }
        if let Some(err) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        if self.pending.is_some() {
            self.pending_ui(ui);
        } else if self.dirty() {
            ui.horizontal(|ui| {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Unsaved changes: saved with the settings.",
                );
                if ui.small_button("Discard").clicked() {
                    self.discard();
                }
            });
        }
    }

    /// The set being edited (a list to pick another), New, Delete, and who
    /// uses it.
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Editing");
            let shown = match (&self.current, self.creating) {
                (Some(name), _) => RichText::new(name.as_str()),
                (None, true) => {
                    RichText::new(format!("{} (new, not saved)", self.name.trim())).italics()
                }
                (None, false) => RichText::new("no set").weak(),
            };
            crate::theme::sunken_combos(ui);
            let mut pick = None;
            ui.add_enabled_ui(!self.sets.is_empty(), |ui| {
                egui::ComboBox::from_id_salt("keyword-set-pick")
                    .width(200.0)
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        for name in &self.sets {
                            if ui
                                .selectable_label(self.current.as_deref() == Some(name), name)
                                .clicked()
                            {
                                pick = Some(name.clone());
                            }
                        }
                    });
            });
            if let Some(name) = pick
                && self.current.as_deref() != Some(&name)
            {
                self.request(Pending::Load(name));
            }
            let text = ui.visuals().text_color();
            if icons::icon_text_button(ui, Icon::Plus, text, "New set", None, None).clicked() {
                self.request(Pending::New);
            }
            if self.current.is_some()
                && icons::icon_text_button(ui, Icon::Delete, text, "Delete…", None, None).clicked()
            {
                self.confirm_delete = true;
            }
        });
        // Choosing a set to edit does not apply it: say who uses it.
        let used = match (&self.current, self.users.len()) {
            (None, _) => String::new(),
            (Some(_), 0) => "No session uses this set yet: choose it above.".to_string(),
            (Some(_), _) => format!("Used by {}.", self.users_text()),
        };
        if !used.is_empty() {
            ui.weak(used);
        }
    }

    /// "Delete the set X?", over everything.
    fn delete_popup(&mut self, ctx: &egui::Context) {
        if !self.confirm_delete {
            return;
        }
        let Some(name) = self.current.clone() else {
            self.confirm_delete = false;
            return;
        };
        let mut delete = false;
        let mut keep = false;
        let modal = egui::Modal::new(egui::Id::unique("keyword-delete")).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new(format!("Delete the keyword set {name}?")).strong());
            ui.add_space(4.0);
            ui.label(if self.users.is_empty() {
                "No session uses it.".to_string()
            } else {
                format!(
                    "{} will have no keywords (or take the set of the folder above).",
                    upper_first(&self.users_text())
                )
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    delete = crate::theme::primary_button(ui, "Delete").clicked();
                    keep = ui.button("Cancel").clicked();
                });
            });
        });
        if delete {
            self.delete();
        } else if keep || modal.should_close() {
            self.confirm_delete = false;
        }
    }

    /// "the global settings and 3 sessions or folders".
    fn users_text(&self) -> String {
        let global = self.config_dir.join(przmterm_session::DEFAULTS_FILE);
        let others = self.users.iter().filter(|p| **p != global).count();
        let others = match others {
            0 => String::new(),
            1 => "1 session or folder".to_string(),
            n => format!("{n} sessions or folders"),
        };
        match (self.users.contains(&global), others.is_empty()) {
            (true, true) => "the global settings".to_string(),
            (true, false) => format!("the global settings and {others}"),
            (false, _) => others,
        }
    }

    /// No set to edit yet.
    fn empty_ui(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(crate::theme::p().sunken)
            .stroke(egui::Stroke::new(1.0, crate::theme::p().border))
            .corner_radius(4)
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    ui.strong("No keyword sets yet");
                    ui.add_space(4.0);
                    ui.weak(
                        "Make one to colour words like “down” or “error” in the \
                         output, then choose it above.",
                    );
                    ui.add_space(8.0);
                    let text = ui.visuals().text_color();
                    if icons::icon_text_button(ui, Icon::Plus, text, "New set", None, None)
                        .clicked()
                    {
                        self.request(Pending::New);
                    }
                });
            });
    }

    /// Does `action` now, or asks first if there are unsaved changes.
    fn request(&mut self, action: Pending) {
        if self.dirty() {
            self.pending = Some(action);
        } else {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: Pending) {
        match action {
            Pending::Load(name) => self.load(&name),
            Pending::New => self.new_set(),
        }
    }

    /// Puts back the set as it was last saved (or drops a new one).
    fn discard(&mut self) {
        match self.current.clone() {
            Some(name) => self.load(&name),
            None => self.load_first(),
        }
        self.error = None;
    }

    /// The "unsaved changes" question.
    fn pending_ui(&mut self, ui: &mut egui::Ui) {
        let Some(action) = self.pending.clone() else {
            return;
        };
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "The changes to {} are not saved. They will be lost if you go on.",
                    self.name.trim()
                ),
            );
            ui.horizontal(|ui| {
                if ui.button("Back").clicked() {
                    self.pending = None;
                }
                if ui.button("Discard changes").clicked() {
                    self.pending = None;
                    self.discard();
                    self.perform(action.clone());
                }
                if crate::theme::primary_button(ui, "Save and go on").clicked() {
                    self.pending = None;
                    match self.save() {
                        Ok(()) => self.perform(action.clone()),
                        Err(e) => self.error = Some(e),
                    }
                }
            });
        });
    }

    fn set_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Name");
            crate::editor::name_field(ui, &mut self.name, "Set name", 200.0);
            crate::theme::checkbox(ui, &mut self.set.match_case, "Match case");
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.strong("Rules");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("🔍 Find a rule")
                        .desired_width(150.0),
                );
            });
        });
        ui.weak(
            "Where matches overlap, the longest wins. Words match whole: “up” colours \
             “link-up”, not “backup”.",
        );
        ui.add_space(4.0);

        let mut remove = None;
        let mut swap = None;
        let count = self.set.rules.len();
        egui::Frame::new()
            .fill(crate::theme::p().sunken)
            .stroke(egui::Stroke::new(1.0, crate::theme::p().border))
            .corner_radius(4)
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                // A scroll bar that shows, beside the rows (not over them).
                ui.style_mut().spacing.scroll = egui::style::ScrollStyle {
                    // The handle in the text's colour: the usual one is the
                    // box's own and would not show.
                    foreground_color: true,
                    ..egui::style::ScrollStyle::solid()
                };
                rule_header(ui);
                ui.add(egui::Separator::default().spacing(6.0));
                // As tall as the rules, up to about eight of them.
                let row = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
                let height = (row * count.max(1) as f32).min(row * 8.0);
                egui::ScrollArea::vertical()
                    .id_salt("keyword-rules")
                    .min_scrolled_height(height)
                    .max_height(height)
                    .scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let filter = self.filter.trim().to_lowercase();
                        for (i, rule) in self.set.rules.iter_mut().enumerate() {
                            if !filter.is_empty()
                                && !rule.text.to_lowercase().contains(&filter)
                                && !rule.regex.to_lowercase().contains(&filter)
                            {
                                continue;
                            }
                            // Moving rules while filtered would be confusing.
                            let movable = filter.is_empty();
                            rule_row(ui, i, count, movable, rule, &mut remove, &mut swap);
                        }
                        if self.set.rules.is_empty() {
                            ui.weak("No rules yet.");
                        }
                    });
            });
        if let Some(i) = remove {
            self.set.rules.remove(i);
        }
        if let Some((a, b)) = swap {
            self.set.rules.swap(a, b);
        }
        ui.add_space(4.0);
        let text = ui.visuals().text_color();
        if icons::icon_text_button(ui, Icon::Plus, text, "Add rule", None, None).clicked() {
            self.set.rules.push(Keyword {
                color: Some("#ffff00".into()),
                ..Keyword::default()
            });
        }

        ui.add_space(8.0);
        ui.strong("Preview");
        self.preview(ui);
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        if self
            .compiled
            .as_ref()
            .is_none_or(|(s, _, _)| *s != self.set)
        {
            let (h, problems) = Highlighter::new(&self.set);
            self.compiled = Some((self.set.clone(), h, problems));
        }
        let Some((_, highlighter, problems)) = &self.compiled else {
            return;
        };
        for p in problems {
            ui.colored_label(ui.visuals().warn_fg_color, p);
        }
        let font = egui::FontId::monospace(13.0);
        let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap: f32| {
            let mut job = egui::text::LayoutJob::default();
            for (n, line) in text.as_str().split('\n').enumerate() {
                if n > 0 {
                    job.append("\n", 0.0, plain(&font));
                }
                let chars: Vec<char> = line.chars().collect();
                let mut at = 0;
                for h in highlighter.spans(line) {
                    let before: String = chars[at..h.range.start].iter().collect();
                    job.append(&before, 0.0, plain(&font));
                    let word: String = chars[h.range.clone()].iter().collect();
                    let mut format = plain(&font);
                    if let Some([r, g, b]) = h.fg {
                        format.color = Color32::from_rgb(r, g, b);
                    }
                    if let Some([r, g, b]) = h.bg {
                        format.background = Color32::from_rgb(r, g, b);
                    }
                    job.append(&word, 0.0, format);
                    at = h.range.end;
                }
                let rest: String = chars[at..].iter().collect();
                job.append(&rest, 0.0, plain(&font));
            }
            job.wrap.max_width = wrap;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        egui::Frame::new()
            .fill(Color32::from_rgb(20, 22, 26))
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.sample)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .frame(egui::Frame::NONE)
                        .layouter(&mut layouter),
                );
            });
        ui.weak("Type any text above to try the rules.");
    }
}

fn plain(font: &egui::FontId) -> egui::TextFormat {
    egui::TextFormat {
        font_id: font.clone(),
        color: Color32::from_rgb(220, 220, 220),
        ..Default::default()
    }
}

/// The widths of a rule's columns, but the text's: it takes the rest.
const MOVE_W: f32 = 48.0;
const KIND_W: f32 = 76.0;
const COLOR_W: f32 = 60.0;
const BACK_W: f32 = 60.0;
const BOLD_W: f32 = 52.0;
const REMOVE_W: f32 = 24.0;

fn upper_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The width left for a rule's text in `ui`.
fn text_width(ui: &egui::Ui) -> f32 {
    let gaps = ui.spacing().item_spacing.x * 6.0;
    (ui.available_width() - MOVE_W - KIND_W - COLOR_W - BACK_W - BOLD_W - REMOVE_W - gaps).max(80.0)
}

/// A cell `width` wide in a row as tall as a field.
fn cell<R>(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let height = ui.spacing().interact_size.y;
    cell_of(ui, width, height, add)
}

/// A cell `width` by `height`, its content centred up and down.
fn cell_of<R>(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            add(ui)
        },
    )
    .inner
}

/// The column titles, lined up with [`rule_row`].
fn rule_header(ui: &mut egui::Ui) {
    // The rows below leave room for their scroll bar.
    let text = text_width(ui) - ui.spacing().scroll.allocated_width();
    // Only as tall as the words: a title row, not a row of fields.
    let h = ui.text_style_height(&egui::TextStyle::Body);
    ui.horizontal(|ui| {
        cell_of(ui, MOVE_W, h, |_| ());
        cell_of(ui, KIND_W, h, |ui| ui.weak("Type"));
        cell_of(ui, text, h, |ui| ui.weak("Word or expression"));
        cell_of(ui, COLOR_W, h, |ui| ui.weak("Colour"));
        cell_of(ui, BACK_W, h, |ui| {
            ui.weak("Back").on_hover_text("Background colour")
        });
        cell_of(ui, BOLD_W, h, |_| ());
        cell_of(ui, REMOVE_W, h, |_| ());
    });
}

/// One rule: move buttons, word/regex, the text (as wide as there is room),
/// colours, bold, remove.
fn rule_row(
    ui: &mut egui::Ui,
    i: usize,
    count: usize,
    movable: bool,
    rule: &mut Keyword,
    remove: &mut Option<usize>,
    swap: &mut Option<(usize, usize)>,
) {
    let text = text_width(ui);
    ui.horizontal(|ui| {
        cell(ui, MOVE_W, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_enabled_ui(movable && i > 0, |ui| {
                if icons::button(ui, Icon::ChevronUp, false)
                    .on_hover_text("Move up")
                    .clicked()
                {
                    *swap = Some((i - 1, i));
                }
            });
            ui.add_enabled_ui(movable && i + 1 < count, |ui| {
                if icons::button(ui, Icon::ChevronDown, false)
                    .on_hover_text("Move down")
                    .clicked()
                {
                    *swap = Some((i, i + 1));
                }
            });
        });
        // A row with no text yet remembers which kind was picked.
        let kind_id = ui.make_persistent_id(("keyword-kind-empty", i));
        let is_regex = if !rule.regex.is_empty() {
            true
        } else if !rule.text.is_empty() {
            false
        } else {
            ui.data(|d| d.get_temp::<bool>(kind_id)).unwrap_or(false)
        };
        let mut kind = is_regex;
        cell(ui, KIND_W, |ui| {
            egui::ComboBox::from_id_salt(("keyword-kind", i))
                .width(KIND_W - 4.0)
                .selected_text(if kind { "regex" } else { "word" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut kind, false, "word");
                    ui.selectable_value(&mut kind, true, "regex");
                });
        });
        if kind != is_regex {
            if kind {
                rule.regex = std::mem::take(&mut rule.text);
            } else {
                rule.text = std::mem::take(&mut rule.regex);
            }
            ui.data_mut(|d| d.insert_temp(kind_id, kind));
        }
        let field = if kind {
            &mut rule.regex
        } else {
            &mut rule.text
        };
        ui.add_sized(
            [text, ui.spacing().interact_size.y],
            egui::TextEdit::singleline(field).font(egui::TextStyle::Monospace),
        );
        cell(ui, COLOR_W, |ui| {
            color_pick(ui, &mut rule.color, [255, 255, 0], "Text colour")
        });
        cell(ui, BACK_W, |ui| {
            color_pick(ui, &mut rule.background, [60, 60, 60], "Background")
        });
        cell(ui, BOLD_W, |ui| {
            crate::theme::checkbox(ui, &mut rule.bold, "bold")
        });
        cell(ui, REMOVE_W, |ui| {
            if icons::button(ui, Icon::Close, false)
                .on_hover_text("Remove the rule")
                .clicked()
            {
                *remove = Some(i);
            }
        });
    });
}

/// A colour that can be set or not: a checkbox and a picker.
fn color_pick(ui: &mut egui::Ui, value: &mut Option<String>, default: [u8; 3], hover: &str) {
    ui.horizontal(|ui| {
        let mut on = value.is_some();
        if crate::theme::checkbox(ui, &mut on, "")
            .on_hover_text(hover)
            .changed()
        {
            *value = on.then(|| format_color(default));
        }
        if let Some(text) = value {
            let mut rgb = parse_color(text).unwrap_or(default);
            if ui.color_edit_button_srgb(&mut rgb).changed() {
                *text = format_color(rgb);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_renames_and_checks_sets() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = KeywordEditor::new(dir.path(), None);
        assert!(
            e.current.is_none() && !e.creating,
            "no set is made when there are none"
        );
        e.new_set();
        e.name = "Cisco".into();
        e.set.rules[0].text = "down".into();
        e.set.rules.push(Keyword::default()); // a blank row is dropped
        e.save().unwrap();
        assert_eq!(przmterm_session::keyword_sets(dir.path()), ["Cisco"]);
        assert_eq!(e.set.rules.len(), 1);
        assert!(!e.dirty());

        // A bad expression is refused with the reason.
        e.set.rules.push(Keyword {
            regex: "(open".into(),
            color: Some("#ff0000".into()),
            ..Keyword::default()
        });
        assert!(e.save().unwrap_err().contains("regular expression"));
        e.set.rules.pop();

        // Renaming moves the file and the sessions that chose it.
        let session = dir.path().join("sessions/r1.toml");
        std::fs::create_dir_all(session.parent().unwrap()).unwrap();
        std::fs::write(&session, "host = \"10.0.0.1\"\nkeywords = \"Cisco\"\n").unwrap();
        e.name = "Network".into();
        e.save().unwrap();
        assert_eq!(przmterm_session::keyword_sets(dir.path()), ["Network"]);
        assert!(
            std::fs::read_to_string(&session)
                .unwrap()
                .contains("\"Network\"")
        );
        assert_eq!(e.users, std::slice::from_ref(&session));
        assert!(
            e.changes
                .contains(&KeywordChange::Renamed("Cisco".into(), "Network".into()))
        );

        // Unsaved changes hold back switching sets until answered.
        e.set.match_case = true;
        e.request(Pending::New);
        assert!(e.pending.is_some() && e.current.as_deref() == Some("Network"));
        e.pending = None;
        e.discard();
        assert!(!e.dirty() && !e.set.match_case);

        // A second set cannot take an existing name.
        e.new_set();
        e.name = "Network".into();
        assert!(e.save().unwrap_err().contains("already exists"));
        e.discard();

        // Deleting the last set leaves nothing (no new set in its place),
        // and the session no longer names it.
        e.delete();
        assert!(e.current.is_none() && !e.creating);
        assert!(przmterm_session::keyword_sets(dir.path()).is_empty());
        assert!(
            !std::fs::read_to_string(&session)
                .unwrap()
                .contains("keywords")
        );
        assert!(
            e.changes
                .contains(&KeywordChange::Deleted("Network".into()))
        );
    }
}
