//! Fonts: the interface's symbol fallback, and the installed monospace
//! fonts the terminal text can use (loaded into egui when a pane asks).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use eframe::egui;

/// Shown for the font that comes with PrzmTerm.
pub const BUILT_IN: &str = "Built-in (Hack)";

/// What the system has, read in the background at start.
struct System {
    db: fontdb::Database,
    /// Monospace families, sorted.
    families: Vec<String>,
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
            let mut families: Vec<String> = db
                .faces()
                .filter(|f| f.monospaced)
                .filter_map(|f| f.families.first().map(|(name, _)| name.clone()))
                // Hidden system faces and colour emoji are not text fonts.
                .filter(|name| !name.starts_with('.') && !name.contains("Emoji"))
                .collect();
            families.sort_by_key(|f| f.to_lowercase());
            families.dedup();
            let _ = fill.set(System { db, families });
            repaint.request_repaint();
        });
        let fonts = Self {
            system,
            defs: base_definitions(),
            loaded: HashMap::new(),
            failed: HashSet::new(),
        };
        ctx.set_fonts(fonts.defs.clone());
        fonts
    }

    /// The installed monospace families, once read.
    pub fn families(&self) -> Option<&[String]> {
        self.system.get().map(|s| s.families.as_slice())
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
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional)
        && !family.iter().any(|f| f == "Hack")
    {
        family.push("Hack".to_owned());
    }
    fonts
}
