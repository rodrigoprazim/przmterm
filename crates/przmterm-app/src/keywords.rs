//! The keyword set editor: sets on the left, the chosen set's rules on the
//! right, and a preview of how sample text comes out.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, RichText};
use przmterm_core::keywords::{Keyword, KeywordSet, format_color, parse_color};
use przmterm_term::highlight::Highlighter;

const SAMPLE: &str = "\
GigabitEthernet0/1   up      up      uplink to 10.0.0.1
GigabitEthernet0/2   down    down    admin-down
BGP peer 2001:db8::1 Established, uptime 12:34:56
Alarm: major LOS on port 3, mac 00:1a:2b:3c:4d:5e";

/// What waits for "unsaved changes will be lost?".
#[derive(Clone)]
enum Pending {
    Close,
    Load(String),
    New,
}

pub enum KeywordResult {
    Open,
    /// Closed; the named sets were saved or deleted.
    Closed(Vec<String>),
}

pub struct KeywordEditor {
    config_dir: PathBuf,
    sets: Vec<String>,
    /// The set being edited, by its name on disk (`None` for a new one).
    current: Option<String>,
    name: String,
    set: KeywordSet,
    /// The set as last saved or loaded, to see unsaved changes.
    saved: KeywordSet,
    sample: String,
    /// Shows only rules containing this text.
    filter: String,
    /// Compiled for the preview, with the set it was built from.
    compiled: Option<(KeywordSet, Highlighter, Vec<String>)>,
    confirm_delete: bool,
    /// Sets saved or deleted while the window was open.
    touched: Vec<String>,
    /// An action held back because of unsaved changes.
    pending: Option<Pending>,
    /// Close on the next frame (the pending close was confirmed).
    closing: bool,
    error: Option<String>,
}

impl KeywordEditor {
    pub fn new(config_dir: &Path, select: Option<&str>) -> Self {
        let mut editor = Self {
            config_dir: config_dir.to_path_buf(),
            sets: przmterm_session::keyword_sets(config_dir),
            current: None,
            name: String::new(),
            set: KeywordSet::default(),
            saved: KeywordSet::default(),
            sample: SAMPLE.to_string(),
            filter: String::new(),
            compiled: None,
            confirm_delete: false,
            touched: Vec::new(),
            pending: None,
            closing: false,
            error: None,
        };
        let first = select
            .map(str::to_string)
            .filter(|s| editor.sets.contains(s))
            .or_else(|| editor.sets.first().cloned());
        match first {
            Some(name) => editor.load(&name),
            None => editor.new_set(),
        }
        editor
    }

    fn dirty(&self) -> bool {
        self.set != self.saved || self.current.as_deref() != Some(self.name.trim())
    }

    fn load(&mut self, name: &str) {
        match przmterm_session::read_keyword_set(&self.config_dir, name) {
            Ok(set) => {
                self.current = Some(name.to_string());
                self.name = name.to_string();
                self.saved = set.clone();
                self.set = set;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self.confirm_delete = false;
    }

    fn new_set(&mut self) {
        let mut name = "New set".to_string();
        let mut n = 2;
        while self.sets.contains(&name) {
            name = format!("New set {n}");
            n += 1;
        }
        self.current = None;
        self.name = name;
        self.set = KeywordSet {
            match_case: false,
            rules: vec![Keyword {
                text: "down".into(),
                color: Some("#ff4040".into()),
                ..Keyword::default()
            }],
        };
        self.saved = KeywordSet::default();
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
        };
        let (_, problems) = Highlighter::new(&set);
        if let Some(p) = problems.first() {
            return Err(p.clone());
        }
        przmterm_session::write_keyword_set(&self.config_dir, &name, &set)
            .map_err(|e| e.to_string())?;
        if let Some(old) = &self.current
            && *old != name
        {
            let _ = przmterm_session::delete_keyword_set(&self.config_dir, old);
            self.touched.push(old.clone());
        }
        self.touched.push(name.clone());
        self.sets = przmterm_session::keyword_sets(&self.config_dir);
        self.current = Some(name.clone());
        self.name = name;
        self.set = set.clone();
        self.saved = set;
        Ok(())
    }

    pub fn show(&mut self, ctx: &egui::Context) -> KeywordResult {
        let mut open = true;
        let mut close = false;
        egui::Window::new("Keyword sets")
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .open(&mut open)
            .collapsible(false)
            .default_width(820.0)
            .default_height(560.0)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    self.list(ui);
                    ui.separator();
                    ui.vertical(|ui| self.set_ui(ui, &mut close));
                });
            });
        if !open {
            close = true;
        }
        if close {
            if self.dirty() {
                self.pending = Some(Pending::Close);
            } else {
                return KeywordResult::Closed(std::mem::take(&mut self.touched));
            }
        }
        if self.closing {
            return KeywordResult::Closed(std::mem::take(&mut self.touched));
        }
        KeywordResult::Open
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.set_width(170.0);
            ui.strong("Sets");
            egui::ScrollArea::vertical()
                .id_salt("keyword-sets")
                .max_height(420.0)
                .show(ui, |ui| {
                    let mut pick = None;
                    for name in &self.sets {
                        if ui
                            .selectable_label(self.current.as_deref() == Some(name), name)
                            .clicked()
                        {
                            pick = Some(name.clone());
                        }
                    }
                    if self.current.is_none() {
                        let _ = ui.selectable_label(true, RichText::new(&self.name).italics());
                    }
                    if let Some(name) = pick {
                        self.request(Pending::Load(name));
                    }
                });
            ui.add_space(6.0);
            if ui.button("+ New set").clicked() {
                self.request(Pending::New);
            }
            if self.current.is_some() {
                if self.confirm_delete {
                    ui.colored_label(ui.visuals().warn_fg_color, "Delete this set?");
                    ui.horizontal(|ui| {
                        if ui.button("Keep").clicked() {
                            self.confirm_delete = false;
                        }
                        if ui.button("Delete").clicked()
                            && let Some(name) = self.current.take()
                        {
                            match przmterm_session::delete_keyword_set(&self.config_dir, &name) {
                                Ok(()) => {
                                    self.touched.push(name);
                                    self.sets = przmterm_session::keyword_sets(&self.config_dir);
                                    match self.sets.first().cloned() {
                                        Some(next) => self.load(&next),
                                        None => self.new_set(),
                                    }
                                }
                                Err(e) => {
                                    self.error = Some(e.to_string());
                                    self.current = Some(name);
                                }
                            }
                            self.confirm_delete = false;
                        }
                    });
                } else if ui.button("Delete set…").clicked() {
                    self.confirm_delete = true;
                }
            }
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
            Pending::Close => self.closing = true,
            Pending::Load(name) => self.load(&name),
            Pending::New => self.new_set(),
        }
    }

    /// Puts back the set as it was last saved (or drops a new one).
    fn discard(&mut self) {
        match self.current.clone() {
            Some(name) => self.load(&name),
            None => match self.sets.first().cloned() {
                Some(first) => self.load(&first),
                None => self.new_set(),
            },
        }
        self.error = None;
    }

    /// The "unsaved changes" question, in place of the buttons.
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
            // Back out on the left, go on on the right.
            ui.horizontal(|ui| {
                if ui.button("Back").clicked() {
                    self.pending = None;
                }
                if ui.button("Discard changes").clicked() {
                    self.pending = None;
                    self.discard();
                    self.perform(action.clone());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Save and go on").clicked() {
                        match self.save() {
                            Ok(()) => {
                                self.pending = None;
                                self.perform(action.clone());
                            }
                            Err(e) => {
                                self.error = Some(e);
                                self.pending = None;
                            }
                        }
                    }
                });
            });
        });
    }

    fn set_ui(&mut self, ui: &mut egui::Ui, close: &mut bool) {
        ui.horizontal(|ui| {
            ui.label("Name");
            crate::editor::name_field(ui, &mut self.name, "Set name");
            ui.checkbox(&mut self.set.match_case, "Match case");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("🔍 Find a rule")
                    .desired_width(140.0),
            );
        });
        ui.weak("Where matches overlap, the longest wins. Words match whole: “up” colours “link-up”, not “backup”.");
        ui.add_space(4.0);

        let mut remove = None;
        let mut swap = None;
        let count = self.set.rules.len();
        egui::ScrollArea::vertical()
            .id_salt("keyword-rules")
            .max_height(300.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("keyword-grid")
                    .num_columns(6)
                    .spacing([8.0, 4.0])
                    .striped(true)
                    .show(ui, |ui| {
                        ui.weak("");
                        ui.weak("Type");
                        ui.weak("Word or expression");
                        ui.weak("Colour");
                        ui.weak("Background");
                        ui.weak("");
                        ui.end_row();
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
                            ui.end_row();
                        }
                    });
            });
        if let Some(i) = remove {
            self.set.rules.remove(i);
        }
        if let Some((a, b)) = swap {
            self.set.rules.swap(a, b);
        }
        if ui.button("+ Add rule").clicked() {
            self.set.rules.push(Keyword {
                color: Some("#ffff00".into()),
                ..Keyword::default()
            });
        }

        ui.add_space(6.0);
        ui.strong("Preview");
        self.preview(ui);

        if let Some(err) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        ui.add_space(6.0);
        if self.pending.is_some() {
            self.pending_ui(ui);
            return;
        }
        ui.horizontal(|ui| {
            let dirty = self.dirty();
            if ui.button("Close").clicked() {
                *close = true;
            }
            if ui
                .add_enabled(dirty, egui::Button::new("Cancel"))
                .on_hover_text("Throw away the changes since the last save")
                .clicked()
            {
                self.discard();
            }
            if dirty {
                ui.colored_label(ui.visuals().warn_fg_color, "Unsaved changes");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
                    self.error = self.save().err();
                }
            });
        });
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

/// One rule: move buttons, word/regex, the text, colours, bold, remove.
fn rule_row(
    ui: &mut egui::Ui,
    i: usize,
    count: usize,
    movable: bool,
    rule: &mut Keyword,
    remove: &mut Option<usize>,
    swap: &mut Option<(usize, usize)>,
) {
    ui.horizontal(|ui| {
        if ui
            .add_enabled(movable && i > 0, egui::Button::new("↑").small())
            .clicked()
        {
            *swap = Some((i - 1, i));
        }
        if ui
            .add_enabled(movable && i + 1 < count, egui::Button::new("↓").small())
            .clicked()
        {
            *swap = Some((i, i + 1));
        }
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
    egui::ComboBox::from_id_salt(("keyword-kind", i))
        .width(70.0)
        .selected_text(if kind { "regex" } else { "word" })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut kind, false, "word");
            ui.selectable_value(&mut kind, true, "regex");
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
    ui.add(
        egui::TextEdit::singleline(field)
            .font(egui::TextStyle::Monospace)
            .desired_width(260.0),
    );
    color_pick(ui, &mut rule.color, [255, 255, 0], "Text colour");
    ui.horizontal(|ui| {
        color_pick(ui, &mut rule.background, [60, 60, 60], "Background");
        ui.checkbox(&mut rule.bold, "bold");
    });
    if ui
        .small_button("×")
        .on_hover_text("Remove the rule")
        .clicked()
    {
        *remove = Some(i);
    }
}

/// A colour that can be set or not: a checkbox and a picker.
fn color_pick(ui: &mut egui::Ui, value: &mut Option<String>, default: [u8; 3], hover: &str) {
    ui.horizontal(|ui| {
        let mut on = value.is_some();
        if ui.checkbox(&mut on, "").on_hover_text(hover).changed() {
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
            e.current.is_none(),
            "starts with a new set when there are none"
        );
        e.name = "Cisco".into();
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

        // Renaming moves the file; the old name is reported as changed.
        e.name = "Network".into();
        e.save().unwrap();
        assert_eq!(przmterm_session::keyword_sets(dir.path()), ["Network"]);
        assert_eq!(e.touched, ["Cisco", "Cisco", "Network"]);

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
    }
}
