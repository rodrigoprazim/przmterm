//! Fonts: the interface's symbol fallback, and the installed fonts the
//! terminal text can use (loaded into egui when a pane asks).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use eframe::egui;

/// Shown for the font that comes with PrzmTerm.
pub const BUILT_IN: &str = "Built-in (Hack)";

/// The installed font families the terminal can use, sorted.
#[derive(Debug, Default, Clone)]
pub struct FontList {
    /// Every letter the same width: what a terminal wants.
    pub mono: Vec<String>,
    /// The rest; their letters do not line up in columns.
    pub other: Vec<String>,
}

/// What the system has, read in the background at start.
struct System {
    db: fontdb::Database,
    list: FontList,
}

pub struct TermFonts {
    system: Arc<OnceLock<System>>,
    defs: egui::FontDefinitions,
    /// Families added to egui, with the pass they were added in (usable
    /// from the next one).
    loaded: HashMap<String, u64>,
    /// Families that could not be loaded.
    failed: HashSet<String>,
}

impl TermFonts {
    /// Sets the interface fonts and starts reading the installed ones.
    pub fn new(ctx: &egui::Context) -> Self {
        let system: Arc<OnceLock<System>> = Arc::default();
        let fill = system.clone();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            let list = classify(&db);
            let _ = fill.set(System { db, list });
            repaint.request_repaint();
        });
        let fonts = Self {
            system,
            defs: base_definitions(),
            loaded: HashMap::new(),
            failed: HashSet::new(),
        };
        ctx.set_fonts(fonts.defs.clone());
        crate::grid::atlas().set_fonts(&fonts.defs);
        fonts
    }

    /// The installed families, once read.
    pub fn families(&self) -> Option<&FontList> {
        self.system.get().map(|s| &s.list)
    }

    /// The egui family to draw text in `name` (`None`: the built-in font).
    /// A family not loaded yet is loaded now and used from the next frame;
    /// until then, and when it cannot be found, the built-in font is used.
    pub fn family(&mut self, ctx: &egui::Context, name: Option<&str>) -> egui::FontFamily {
        let Some(name) = name.filter(|n| !n.is_empty() && *n != BUILT_IN) else {
            return egui::FontFamily::Monospace;
        };
        let pass = ctx.cumulative_pass_nr();
        if let Some(&added) = self.loaded.get(name) {
            return if pass > added {
                egui::FontFamily::Name(key(name).into())
            } else {
                egui::FontFamily::Monospace
            };
        }
        if self.failed.contains(name) {
            return egui::FontFamily::Monospace;
        }
        let Some(system) = self.system.get() else {
            return egui::FontFamily::Monospace;
        };
        match load(system, name) {
            Some(data) => {
                let key = key(name);
                self.defs.font_data.insert(key.clone(), Arc::new(data));
                // The chosen font first, then the built-in ones for the
                // characters it lacks.
                let mut chain = vec![key.clone()];
                chain.extend(
                    self.defs
                        .families
                        .get(&egui::FontFamily::Monospace)
                        .cloned()
                        .unwrap_or_default(),
                );
                self.defs
                    .families
                    .insert(egui::FontFamily::Name(key.into()), chain);
                ctx.set_fonts(self.defs.clone());
                crate::grid::atlas().set_fonts(&self.defs);
                self.loaded.insert(name.to_string(), pass);
                ctx.request_repaint();
            }
            None => {
                tracing::warn!("font {name:?} not found");
                self.failed.insert(name.to_string());
            }
        }
        egui::FontFamily::Monospace
    }
}

/// Sorts the families into monospace and the rest, leaving out those that
/// cannot draw text: bitmap-only fonts (nothing would show), symbol and
/// emoji fonts without Latin letters, and hidden system faces.
fn classify(db: &fontdb::Database) -> FontList {
    use skrifa::MetadataProvider;
    use skrifa::prelude::{LocationRef, Size};
    // One face per family, the regular one when there is one.
    let mut faces: std::collections::BTreeMap<String, (fontdb::ID, bool)> = Default::default();
    for face in db.faces() {
        let Some((name, _)) = face.families.first() else {
            continue;
        };
        if name.starts_with('.') || name.contains("Emoji") || name.contains("LastResort") {
            continue;
        }
        let regular = face.weight == fontdb::Weight::NORMAL && face.style == fontdb::Style::Normal;
        match faces.get(name) {
            Some((_, true)) => {}
            Some((_, false)) if !regular => {}
            _ => {
                faces.insert(name.clone(), (face.id, regular));
            }
        }
    }
    let mut list = FontList::default();
    for (name, (id, _)) in faces {
        let kind = db.with_face_data(id, |data, index| {
            let font = skrifa::FontRef::from_index(data, index).ok()?;
            let charmap = font.charmap();
            let outlines = font.outline_glyphs();
            let metrics = font.glyph_metrics(Size::unscaled(), LocationRef::default());
            let mut widths = Vec::new();
            for ch in ['M', 'i', 'W', '.'] {
                let glyph = charmap.map(ch)?;
                // A glyph with no outline: a bitmap font, which egui
                // cannot draw.
                outlines.get(glyph)?;
                widths.push(metrics.advance_width(glyph)?);
            }
            let mono = widths.iter().all(|w| (w - widths[0]).abs() < 0.5);
            Some(mono)
        });
        match kind.flatten() {
            Some(true) => list.mono.push(name),
            Some(false) => list.other.push(name),
            None => {}
        }
    }
    list.mono.sort_by_key(|f| f.to_lowercase());
    list.other.sort_by_key(|f| f.to_lowercase());
    list
}

/// What was picked in [`font_menu`].
pub enum FontChoice {
    BuiltIn,
    Family(String),
}

/// The contents of a font drop-down: a search box, the built-in font, the
/// monospace families and then the others. `current` is ticked.
pub fn font_menu(
    ui: &mut egui::Ui,
    list: Option<&FontList>,
    current: Option<&str>,
) -> Option<FontChoice> {
    let id = egui::Id::unique("font-filter");
    let mut filter: String = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.add(
        egui::TextEdit::singleline(&mut filter)
            .hint_text("Search fonts")
            .desired_width(f32::INFINITY),
    )
    .request_focus();
    ui.data_mut(|d| d.insert_temp(id, filter.clone()));
    let wanted = filter.trim().to_lowercase();
    let matches = |name: &str| wanted.is_empty() || name.to_lowercase().contains(&wanted);
    let mut picked = None;
    egui::ScrollArea::vertical()
        .max_height(300.0)
        .show(ui, |ui| {
            if matches(BUILT_IN) && ui.selectable_label(current.is_none(), BUILT_IN).clicked() {
                picked = Some(FontChoice::BuiltIn);
            }
            let Some(list) = list else {
                ui.weak("Reading the installed fonts…");
                return;
            };
            let mut section = |ui: &mut egui::Ui, title: &str, names: &[String]| {
                let shown: Vec<&String> = names.iter().filter(|n| matches(n)).collect();
                if shown.is_empty() {
                    return;
                }
                ui.separator();
                ui.weak(title);
                for name in shown {
                    if ui
                        .selectable_label(current == Some(name.as_str()), name.as_str())
                        .clicked()
                    {
                        picked = Some(FontChoice::Family(name.clone()));
                    }
                }
            };
            section(ui, "Monospace", &list.mono);
            section(
                ui,
                "Other fonts (letters will not line up in columns)",
                &list.other,
            );
        });
    if picked.is_some() {
        ui.data_mut(|d| d.remove::<String>(id));
    }
    picked
}

fn key(name: &str) -> String {
    format!("term:{name}")
}

/// The regular face of `family`.
fn load(system: &System, family: &str) -> Option<egui::FontData> {
    let query = fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    };
    let id = system.db.query(&query)?;
    let (bytes, index) = system
        .db
        .with_face_data(id, |data, index| (data.to_vec(), index))?;
    let mut font = egui::FontData::from_owned(bytes);
    font.index = index;
    Some(font)
}

/// egui's fonts, with the interface text falling back to the monospace
/// font for symbols the proportional one lacks (arrows such as ⇶ and ⇅,
/// boxes such as ◫ and ▤), so they show the same on every system instead
/// of as empty squares.
fn base_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    // A copy of Hack for those symbols, raised and enlarged a little so they
    // sit on the line of the Ubuntu text around them (Hack's symbols are
    // drawn for a terminal cell, lower and smaller).
    if let Some(hack) = fonts.font_data.get("Hack") {
        let mut symbols = (**hack).clone();
        symbols.tweak = egui::FontTweak {
            scale: SYMBOL_SCALE,
            y_offset_factor: SYMBOL_RAISE,
            ..Default::default()
        };
        fonts
            .font_data
            .insert(SYMBOLS.to_owned(), Arc::new(symbols));
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.push(SYMBOLS.to_owned());
        }
    }
    crate::theme::add_fonts(&mut fonts);
    fonts
}

/// The interface's symbol font (see [`base_definitions`]).
const SYMBOLS: &str = "Hack-symbols";
const SYMBOL_SCALE: f32 = 1.1;
const SYMBOL_RAISE: f32 = -0.12;
