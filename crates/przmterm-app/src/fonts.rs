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
                tracing::debug!("font {name:?} loaded");
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
    let (mut bytes, index) = system
        .db
        .with_face_data(id, |data, index| (data.to_vec(), index))?;
    if drop_bitmap_strikes(&mut bytes) {
        tracing::debug!("font {family:?}: its bitmap strikes left out, its outlines used");
    }
    let mut font = egui::FontData::from_owned(bytes);
    font.index = index;
    Some(font)
}

/// Hides a font's black-and-white bitmap glyphs (`EBDT`, Apple's `bdat`),
/// which older Windows fonts such as Lucida Console carry for small sizes,
/// so its outlines are drawn instead. egui draws bitmap glyphs as pictures
/// in their own colour: the terminal's text came out all white. The tables
/// are only renamed (`EBDS`, `bdas`), which keeps the table directory in
/// order. Colour glyphs (`CBDT`, `sbix`, `COLR`) are left alone. Whether
/// any was hidden.
fn drop_bitmap_strikes(font: &mut [u8]) -> bool {
    let read_u32 = |font: &[u8], at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(font.get(at..at + 4)?.try_into().ok()?))
    };
    // A collection lists the offset of each font's directory.
    let directories: Vec<usize> = if font.get(0..4) == Some(b"ttcf") {
        let count = read_u32(font, 8).unwrap_or(0) as usize;
        (0..count.min(256))
            .filter_map(|i| read_u32(font, 12 + 4 * i))
            .map(|o| o as usize)
            .collect()
    } else {
        vec![0]
    };
    let mut hidden = false;
    for start in directories {
        let Some(count) = font
            .get(start + 4..start + 6)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
        else {
            continue;
        };
        for i in 0..count {
            let at = start + 12 + 16 * i;
            let Some(tag) = font.get_mut(at..at + 4) else {
                break;
            };
            match &*tag {
                b"EBDT" => tag.copy_from_slice(b"EBDS"),
                b"bdat" => tag.copy_from_slice(b"bdas"),
                _ => continue,
            }
            hidden = true;
        }
    }
    hidden
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
    crate::icons::add_font(&mut fonts);
    fonts
}

/// The interface's symbol font (see [`base_definitions`]).
const SYMBOLS: &str = "Hack-symbols";
const SYMBOL_SCALE: f32 = 1.1;
const SYMBOL_RAISE: f32 = -0.12;

#[cfg(test)]
mod tests {
    use super::*;

    /// A font's table directory with `tags`, in order (no table data).
    fn directory(tags: &[&[u8; 4]]) -> Vec<u8> {
        let mut font = vec![0, 1, 0, 0];
        font.extend((tags.len() as u16).to_be_bytes());
        font.extend([0; 6]);
        for tag in tags {
            font.extend(*tag);
            font.extend([0; 12]);
        }
        font
    }

    fn tags(font: &[u8], start: usize) -> Vec<String> {
        let count = usize::from(u16::from_be_bytes([font[start + 4], font[start + 5]]));
        (0..count)
            .map(|i| {
                let at = start + 12 + 16 * i;
                String::from_utf8_lossy(&font[at..at + 4]).into_owned()
            })
            .collect()
    }

    #[test]
    fn bitmap_strikes_are_hidden_in_order() {
        let mut font = directory(&[b"DSIG", b"EBDT", b"EBLC", b"glyf", b"head"]);
        assert!(drop_bitmap_strikes(&mut font));
        let found = tags(&font, 0);
        assert_eq!(found, ["DSIG", "EBDS", "EBLC", "glyf", "head"]);
        let mut sorted = found.clone();
        sorted.sort();
        assert_eq!(
            found, sorted,
            "still sorted: tables are looked up by search"
        );

        let mut plain = directory(&[b"CBDT", b"CBLC", b"COLR", b"glyf"]);
        assert!(!drop_bitmap_strikes(&mut plain), "colour glyphs stay");
    }

    #[test]
    fn every_font_of_a_collection() {
        let one = directory(&[b"EBDT", b"glyf"]);
        let two = directory(&[b"bdat", b"glyf"]);
        let mut ttc = b"ttcf".to_vec();
        ttc.extend([0, 1, 0, 0]);
        ttc.extend(2u32.to_be_bytes());
        let first = 20;
        ttc.extend((first as u32).to_be_bytes());
        ttc.extend(((first + one.len()) as u32).to_be_bytes());
        ttc.extend(&one);
        ttc.extend(&two);
        assert!(drop_bitmap_strikes(&mut ttc));
        assert_eq!(tags(&ttc, first), ["EBDS", "glyf"]);
        assert_eq!(tags(&ttc, first + one.len()), ["bdas", "glyf"]);
        // Cut short: nothing read past the end.
        let mut short = ttc[..30].to_vec();
        drop_bitmap_strikes(&mut short);
    }
}
