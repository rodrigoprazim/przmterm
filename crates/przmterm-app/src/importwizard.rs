//! The SecureCRT import wizard: what was found, what to import, and what
//! to do with what is already there, before anything is written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eframe::egui;
use przmterm_session::securecrt::{self, Existing, Report};

/// What the wizard asks for once it is closed.
pub enum WizardResult {
    Open,
    Cancel,
    /// Import this (the chosen part of the report) into this folder.
    Import(Box<Report>, String, Existing),
}

pub struct ImportWizard {
    report: Report,
    config_dir: PathBuf,
    /// The folder below `sessions/` the sessions go to.
    into: String,
    /// What `exists` was worked out for.
    exists_for: String,
    sessions: Vec<bool>,
    /// Per session: already there in `into`.
    exists: Vec<bool>,
    keyword_sets: Vec<bool>,
    keyword_exists: Vec<bool>,
    groups: Vec<bool>,
    /// Per button group: buttons not there yet.
    new_buttons: Vec<usize>,
    existing: Existing,
}

impl ImportWizard {
    pub fn new(report: Report, config_dir: &Path, into: &str) -> Self {
        let keyword_exists = report
            .keyword_sets
            .iter()
            .map(|(name, _)| securecrt::keyword_set_exists(config_dir, name))
            .collect();
        let new_buttons: Vec<usize> = report
            .button_groups
            .iter()
            .map(|g| securecrt::new_buttons(config_dir, g))
            .collect();
        let mut wizard = Self {
            sessions: vec![true; report.sessions.len()],
            exists: Vec::new(),
            keyword_sets: vec![true; report.keyword_sets.len()],
            keyword_exists,
            // A group whose buttons are all there already: nothing to add.
            groups: new_buttons.iter().map(|&n| n > 0).collect(),
            new_buttons,
            report,
            config_dir: config_dir.to_path_buf(),
            into: into.to_string(),
            exists_for: String::new(),
            existing: Existing::Skip,
        };
        wizard.check_existing();
        wizard
    }

    /// Works out which sessions are already there, for the folder chosen.
    fn check_existing(&mut self) {
        if self.exists_for == self.into && self.exists.len() == self.report.sessions.len() {
            return;
        }
        self.exists = self
            .report
            .sessions
            .iter()
            .map(|s| securecrt::session_exists(&self.config_dir, &self.into, s))
            .collect();
        self.exists_for = self.into.clone();
    }

    /// The chosen part of the report.
    fn chosen(&self) -> Report {
        let mut report = self.report.clone();
        let mut keep = self.sessions.iter();
        report.sessions.retain(|_| *keep.next().unwrap_or(&false));
        let mut keep = self.keyword_sets.iter();
        report
            .keyword_sets
            .retain(|_| *keep.next().unwrap_or(&false));
        let mut keep = self.groups.iter();
        report
            .button_groups
            .retain(|_| *keep.next().unwrap_or(&false));
        report
    }

    pub fn show(&mut self, ctx: &egui::Context) -> WizardResult {
        self.check_existing();
        let mut result = WizardResult::Open;
        let mut open = true;
        let already = self.exists.iter().filter(|&&e| e).count()
            + self.keyword_exists.iter().filter(|&&e| e).count();
        egui::Window::new(crate::theme::window_title(ctx, "Import from SecureCRT"))
            .open(&mut open)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.horizontal(|ui| {
                    ui.label("Into the folder");
                    crate::editor::name_field(ui, &mut self.into, "top level", f32::INFINITY);
                });
                ui.add_space(6.0);
                let max = ctx.content_rect().height() * 0.45;
                egui::ScrollArea::vertical()
                    .max_height(max)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        self.sessions_ui(ui);
                        self.keywords_ui(ui);
                        self.buttons_ui(ui);
                    });
                if already > 0 {
                    ui.add_space(6.0);
                    ui.label(format!(
                        "{already} of these {} already here. For those:",
                        if already == 1 { "is" } else { "are" }
                    ));
                    ui.horizontal(|ui| {
                        for (choice, text, hint) in [
                            (
                                Existing::Skip,
                                "Skip them",
                                "Keep what is here; importing again adds nothing",
                            ),
                            (
                                Existing::Replace,
                                "Replace them",
                                "Write the imported ones over them",
                            ),
                            (
                                Existing::KeepBoth,
                                "Keep both",
                                "Import them with (2) after the name",
                            ),
                        ] {
                            if crate::theme::radio(ui, self.existing == choice, text)
                                .on_hover_text(hint)
                                .clicked()
                            {
                                self.existing = choice;
                            }
                        }
                    });
                }
                if !self.report.skipped.is_empty() {
                    ui.add_space(4.0);
                    egui::CollapsingHeader::new(format!(
                        "Not imported ({})",
                        self.report.skipped.len()
                    ))
                    .id_salt("import-skipped")
                    .show(ui, |ui| {
                        for (name, why) in &self.report.skipped {
                            ui.weak(format!("{name}: {why}"));
                        }
                    });
                }
                ui.add_space(10.0);
                let count = self.count();
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let text = if count > 0 {
                            format!("Import {count}")
                        } else {
                            "Nothing new".to_string()
                        };
                        let import = ui
                            .add_enabled_ui(count > 0, |ui| crate::theme::primary_button(ui, &text))
                            .inner;
                        if import.clicked() {
                            result = WizardResult::Import(
                                Box::new(self.chosen()),
                                self.into.trim().to_string(),
                                self.existing,
                            );
                        }
                        if ui.button("Cancel").clicked() {
                            result = WizardResult::Cancel;
                        }
                    });
                });
            });
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            result = WizardResult::Cancel;
        }
        result
    }

    /// How many things the Import button imports: what is chosen, less
    /// what is already here when that is skipped.
    fn count(&self) -> usize {
        let skip = self.existing == Existing::Skip;
        let on = |picks: &[bool], there: &[bool]| {
            picks
                .iter()
                .zip(there.iter().chain(std::iter::repeat(&false)))
                .filter(|&(&pick, &there)| pick && !(skip && there))
                .count()
        };
        on(&self.sessions, &self.exists)
            + on(&self.keyword_sets, &self.keyword_exists)
            + self.groups.iter().filter(|&&b| b).count()
    }

    /// The sessions, by folder: a box for each folder and each session.
    fn sessions_ui(&mut self, ui: &mut egui::Ui) {
        if self.report.sessions.is_empty() {
            return;
        }
        let mut folders: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, s) in self.report.sessions.iter().enumerate() {
            folders.entry(s.folder.join(" / ")).or_default().push(i);
        }
        section(ui, "Sessions", &mut self.sessions);
        for (folder, items) in folders {
            let all = items.iter().all(|&i| self.sessions[i]);
            let mut on = all;
            let name = if folder.is_empty() {
                "(top level)".to_string()
            } else {
                folder
            };
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                if crate::theme::checkbox(ui, &mut on, &name).changed() {
                    for &i in &items {
                        self.sessions[i] = on;
                    }
                }
            });
            for i in items {
                let s = &self.report.sessions[i];
                let target = s
                    .settings
                    .host
                    .clone()
                    .or_else(|| s.settings.serial_port.clone())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.add_space(32.0);
                    crate::theme::checkbox(ui, &mut self.sessions[i], &s.name);
                    ui.weak(target);
                    if self.exists[i] {
                        ui.colored_label(ui.visuals().warn_fg_color, "already here");
                    }
                });
            }
        }
    }

    fn keywords_ui(&mut self, ui: &mut egui::Ui) {
        if self.report.keyword_sets.is_empty() {
            return;
        }
        ui.add_space(6.0);
        section(ui, "Keyword sets", &mut self.keyword_sets);
        for (i, (name, set)) in self.report.keyword_sets.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                crate::theme::checkbox(ui, &mut self.keyword_sets[i], name);
                ui.weak(format!("{} rule(s)", set.rules.len()));
                if self.keyword_exists[i] {
                    ui.colored_label(ui.visuals().warn_fg_color, "already here");
                }
            });
        }
    }

    fn buttons_ui(&mut self, ui: &mut egui::Ui) {
        if self.report.button_groups.is_empty() {
            return;
        }
        ui.add_space(6.0);
        section(ui, "Button groups", &mut self.groups);
        for (i, group) in self.report.button_groups.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                crate::theme::checkbox(ui, &mut self.groups[i], &group.name);
                let new = self.new_buttons[i];
                let total = group.buttons.len();
                ui.weak(if new == total {
                    format!("{total} button(s)")
                } else if new == 0 {
                    format!("{total} button(s), all already here")
                } else {
                    format!("{total} button(s), {new} new")
                });
            });
        }
    }
}

/// A section's title with All and None.
fn section(ui: &mut egui::Ui, title: &str, picks: &mut [bool]) {
    ui.horizontal(|ui| {
        let on = picks.iter().filter(|&&b| b).count();
        ui.strong(format!("{title} ({on} of {})", picks.len()));
        if ui.small_button("All").clicked() {
            picks.iter_mut().for_each(|p| *p = true);
        }
        if ui.small_button("None").clicked() {
            picks.iter_mut().for_each(|p| *p = false);
        }
    });
}
