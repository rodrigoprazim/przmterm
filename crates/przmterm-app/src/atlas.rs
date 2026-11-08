//! The glyph atlas of the GPU grid ([`crate::grid`]): each glyph drawn once
//! on the CPU, at the size and quarter-pixel offset it is shown at, and
//! packed into one single-channel image that the renderer copies into a
//! texture.
//!
//! Glyphs come from the same fonts egui uses for the terminal family (the
//! chosen font, then the fallbacks), outlined with `skrifa`, hinted, and
//! filled with `vello_cpu`, as egui does. Characters that only colour or
//! bitmap fonts have, or that no font in the family has, are left to egui
//! ([`Shape::Elsewhere`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use eframe::egui::{FontData, FontDefinitions, FontFamily};
use skrifa::MetadataProvider as _;
use skrifa::raw::TableProvider as _;
use vello_cpu::{color, kurbo};

/// Glyphs are drawn at this many horizontal offsets within a pixel.
pub const BINS: u8 = 4;
/// The image's side when it starts, and the most it grows to (a side every
/// GPU takes).
const START_SIDE: u32 = 512;
const MAX_SIDE: u32 = 2048;
/// Empty pixels around each glyph, so filtering never reads a neighbour.
const PAD: u32 = 1;

/// Where a glyph is in the atlas, and where it goes relative to the pen
/// (on the baseline, at the cell's left edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub left: i32,
    pub top: i32,
}

/// How a character is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// Nothing to draw (a space, or a glyph with no outline).
    Blank,
    /// From the atlas, moved down by `dy` pixels (the font's tweak).
    Drawn { slot: Slot, dy: f32 },
    /// Not here: egui draws it.
    Elsewhere,
}

/// What the renderer copies to the texture.
pub struct Upload<'a> {
    pub side: u32,
    /// Changes when the image was resized or cleared: copy all of it.
    pub generation: u64,
    pub pixels: &'a [u8],
    /// Rows changed since the last copy.
    pub rows: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Copy)]
enum Found {
    Glyph { face: usize, glyph: u32 },
    Elsewhere,
}

struct Face {
    data: Arc<FontData>,
    units_per_em: f32,
    ascent: f32,
    /// Colour (COLR, sbix, CBDT) or bitmap (EBDT) glyphs: left to egui.
    color: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    face: usize,
    glyph: u32,
    /// Pixels per em, as `f32` bits.
    ppem: u32,
    bin: u8,
}

/// A glyph drawn on the CPU: coverage, one byte per pixel.
struct Bitmap {
    w: u32,
    h: u32,
    left: i32,
    top: i32,
    alpha: Vec<u8>,
}

pub struct Atlas {
    side: u32,
    pixels: Vec<u8>,
    /// The shelf being filled: its top, its height so far, and the next x.
    shelf: (u32, u32, u32),
    families: BTreeMap<FontFamily, Vec<String>>,
    font_data: BTreeMap<String, Arc<FontData>>,
    faces: Vec<Face>,
    by_name: HashMap<String, Option<usize>>,
    chars: HashMap<(FontFamily, char), Found>,
    glyphs: HashMap<Key, Option<Slot>>,
    hinting: HashMap<(usize, u32), skrifa::outline::HintingInstance>,
    dirty: Option<(u32, u32)>,
    generation: u64,
    /// Out of room: glyphs not drawn yet go to egui until the next frame
    /// starts over with an empty image.
    full: bool,
}

impl Default for Atlas {
    fn default() -> Self {
        Self::with_side(START_SIDE)
    }
}

impl Atlas {
    fn with_side(side: u32) -> Self {
        Self {
            side,
            pixels: vec![0; (side * side) as usize],
            shelf: (PAD, 0, PAD),
            families: BTreeMap::new(),
            font_data: BTreeMap::new(),
            faces: Vec::new(),
            by_name: HashMap::new(),
            chars: HashMap::new(),
            glyphs: HashMap::new(),
            hinting: HashMap::new(),
            dirty: None,
            generation: 1,
            full: false,
        }
    }

    /// Takes the fonts egui was given. Families may change (a font chosen);
    /// a font replaced under the same name starts the atlas over.
    pub fn set_fonts(&mut self, defs: &FontDefinitions) {
        let replaced = self.faces.iter().any(|face| {
            !defs
                .font_data
                .values()
                .any(|data| Arc::ptr_eq(data, &face.data))
        });
        if replaced {
            let side = self.side;
            *self = Self::with_side(side);
            self.generation += 1;
        }
        self.families = defs.families.clone();
        self.font_data = defs.font_data.clone();
        self.chars.clear();
    }

    /// Call before drawing a frame: an atlas that filled up starts over.
    pub fn begin_frame(&mut self) {
        if self.full {
            self.clear();
        }
    }

    fn clear(&mut self) {
        self.pixels.fill(0);
        self.shelf = (PAD, 0, PAD);
        self.glyphs.clear();
        self.dirty = None;
        self.generation += 1;
        self.full = false;
    }

    /// The image for the texture, then forgets what changed.
    pub fn upload(&mut self) -> Upload<'_> {
        Upload {
            side: self.side,
            generation: self.generation,
            pixels: &self.pixels,
            rows: self.dirty.take(),
        }
    }

    /// The ascent of `family`'s first font at `size` points: from a row's
    /// top to its baseline.
    pub fn ascent(&mut self, family: &FontFamily, size: f32) -> f32 {
        let first = self.chain(family).into_iter().next();
        match first.and_then(|name| self.face(&name)) {
            Some(i) => {
                let face = &self.faces[i];
                face.ascent * size * face.data.tweak.scale / face.units_per_em
            }
            None => 0.8 * size,
        }
    }

    /// How `ch` is drawn in `family` at `size` points, `ppp` pixels per
    /// point, shifted right by `bin` / [`BINS`] of a pixel.
    pub fn glyph(&mut self, family: &FontFamily, ch: char, size: f32, ppp: f32, bin: u8) -> Shape {
        if ch == ' ' || ch == '\0' {
            return Shape::Blank;
        }
        let Found::Glyph { face, glyph } = self.find(family, ch) else {
            return Shape::Elsewhere;
        };
        let tweak = &self.faces[face].data.tweak;
        let points = size * tweak.scale;
        let dy = (points * tweak.y_offset_factor + tweak.y_offset) * ppp;
        let ppem = points * ppp;
        let key = Key {
            face,
            glyph,
            ppem: ppem.to_bits(),
            bin,
        };
        if let Some(slot) = self.glyphs.get(&key) {
            return slot.map_or(Shape::Blank, |slot| Shape::Drawn { slot, dy });
        }
        if self.full {
            return Shape::Elsewhere;
        }
        let Some(bitmap) = self.rasterize(face, glyph, ppem, bin) else {
            self.glyphs.insert(key, None);
            return Shape::Blank;
        };
        let Some((x, y)) = self.allocate(bitmap.w, bitmap.h) else {
            self.full = true;
            return Shape::Elsewhere;
        };
        for row in 0..bitmap.h {
            let from = (row * bitmap.w) as usize;
            let to = ((y + row) * self.side + x) as usize;
            self.pixels[to..to + bitmap.w as usize]
                .copy_from_slice(&bitmap.alpha[from..from + bitmap.w as usize]);
        }
        self.mark(y, y + bitmap.h);
        let slot = Slot {
            x,
            y,
            w: bitmap.w,
            h: bitmap.h,
            left: bitmap.left,
            top: bitmap.top,
        };
        self.glyphs.insert(key, Some(slot));
        Shape::Drawn { slot, dy }
    }

    fn chain(&self, family: &FontFamily) -> Vec<String> {
        self.families
            .get(family)
            .or_else(|| self.families.get(&FontFamily::Monospace))
            .cloned()
            .unwrap_or_default()
    }

    /// The font in `family` that has `ch`, the first in its chain.
    fn find(&mut self, family: &FontFamily, ch: char) -> Found {
        if let Some(&found) = self.chars.get(&(family.clone(), ch)) {
            return found;
        }
        let mut found = Found::Elsewhere;
        for name in self.chain(family) {
            let Some(face) = self.face(&name) else {
                continue;
            };
            let data = &self.faces[face].data;
            let Ok(font) = skrifa::FontRef::from_index((*data.font).as_ref(), data.index) else {
                continue;
            };
            let Some(glyph) = font.charmap().map(ch).filter(|g| g.to_u32() != 0) else {
                continue;
            };
            if !self.faces[face].color {
                found = Found::Glyph {
                    face,
                    glyph: glyph.to_u32(),
                };
            }
            break;
        }
        self.chars.insert((family.clone(), ch), found);
        found
    }

    /// The face of the font called `name`, read the first time.
    fn face(&mut self, name: &str) -> Option<usize> {
        if let Some(&known) = self.by_name.get(name) {
            return known;
        }
        let read = self.font_data.get(name).and_then(|data| {
            let font = skrifa::FontRef::from_index((*data.font).as_ref(), data.index).ok()?;
            let metrics = font.metrics(
                skrifa::instance::Size::unscaled(),
                skrifa::instance::LocationRef::default(),
            );
            let color = font.colr().is_ok()
                || font.sbix().is_ok()
                || font.cbdt().is_ok()
                || font.ebdt().is_ok();
            Some(Face {
                data: data.clone(),
                units_per_em: f32::from(metrics.units_per_em.max(1)),
                ascent: metrics.ascent,
                color,
            })
        });
        let index = read.map(|face| {
            self.faces.push(face);
            self.faces.len() - 1
        });
        self.by_name.insert(name.to_string(), index);
        index
    }

    fn rasterize(&mut self, face: usize, glyph: u32, ppem: f32, bin: u8) -> Option<Bitmap> {
        let data = self.faces[face].data.clone();
        let font = skrifa::FontRef::from_index((*data.font).as_ref(), data.index).ok()?;
        let outlines = font.outline_glyphs();
        let outline = outlines.get(skrifa::GlyphId::new(glyph))?;
        let size = skrifa::instance::Size::new(ppem);
        let location = skrifa::instance::LocationRef::default();
        let mut path = kurbo::BezPath::new();
        let mut pen = Pen {
            path: &mut path,
            dx: f64::from(bin) / f64::from(BINS),
        };
        let hinted = data
            .tweak
            .hinting
            .unwrap_or(true)
            .then(|| match self.hinting.entry((face, ppem.to_bits())) {
                std::collections::hash_map::Entry::Occupied(e) => Some(e.into_mut()),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let target: skrifa::outline::Target = data.tweak.hinting_target.into();
                    skrifa::outline::HintingInstance::new(&outlines, size, location, target)
                        .ok()
                        .map(|instance| e.insert(instance))
                }
            })
            .flatten();
        match hinted {
            Some(instance) => outline.draw(
                skrifa::outline::DrawSettings::hinted(instance, false),
                &mut pen,
            ),
            None => outline.draw(
                skrifa::outline::DrawSettings::unhinted(size, location),
                &mut pen,
            ),
        }
        .ok()?;

        let bounds = path.control_box().expand();
        let (w, h) = (bounds.width() as u32, bounds.height() as u32);
        if w == 0 || h == 0 || w > self.side_limit() || h > self.side_limit() {
            return None;
        }
        let mut ctx = vello_cpu::RenderContext::new(w as u16, h as u16);
        ctx.set_transform(kurbo::Affine::translate((-bounds.x0, -bounds.y0)));
        ctx.set_paint(color::OpaqueColor::<color::Srgb>::WHITE);
        ctx.fill_path(&path);
        let mut pixmap = vello_cpu::Pixmap::new(w as u16, h as u16);
        let mut resources = vello_cpu::Resources::new();
        ctx.render(&mut pixmap, &mut resources);
        // White on a dark background reads thin: egui's dark-mode curve.
        let alpha = pixmap
            .data_as_u8_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| {
                let c = f32::from(px[3]) / 255.0;
                ((2.0 * c - c * c) * 255.0).round() as u8
            })
            .collect();
        Some(Bitmap {
            w,
            h,
            left: bounds.x0 as i32,
            top: bounds.y0 as i32,
            alpha,
        })
    }

    /// No glyph is bigger than a quarter of the largest image.
    fn side_limit(&self) -> u32 {
        MAX_SIDE / 4
    }

    /// Room for a `w` × `h` glyph: on the current shelf, a new one below,
    /// or in a bigger image.
    fn allocate(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        loop {
            let (mut top, mut height, mut x) = self.shelf;
            if x + w + PAD > self.side {
                top += height + PAD;
                height = 0;
                x = PAD;
            }
            if top.max(PAD) + h + PAD <= self.side {
                let top = top.max(PAD);
                self.shelf = (top, height.max(h), x + w + PAD);
                return Some((x, top));
            }
            if self.side >= MAX_SIDE {
                return None;
            }
            self.grow();
        }
    }

    fn grow(&mut self) {
        let side = self.side * 2;
        let mut pixels = vec![0; (side * side) as usize];
        for row in 0..self.side {
            let from = (row * self.side) as usize;
            let to = (row * side) as usize;
            pixels[to..to + self.side as usize]
                .copy_from_slice(&self.pixels[from..from + self.side as usize]);
        }
        self.side = side;
        self.pixels = pixels;
        self.dirty = None;
        self.generation += 1;
    }

    fn mark(&mut self, from: u32, to: u32) {
        self.dirty = Some(match self.dirty {
            Some((a, b)) => (a.min(from), b.max(to)),
            None => (from, to),
        });
    }
}

/// Collects an outline into a path, y pointing down.
struct Pen<'a> {
    path: &'a mut kurbo::BezPath,
    dx: f64,
}

impl Pen<'_> {
    fn at(&self, x: f32, y: f32) -> (f64, f64) {
        (f64::from(x) + self.dx, -f64::from(y))
    }
}

impl skrifa::outline::OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.path.move_to(p);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.at(x, y);
        self.path.line_to(p);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (c, p) = (self.at(cx0, cy0), self.at(x, y));
        self.path.quad_to(c, p);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (c0, c1, p) = (self.at(cx0, cy0), self.at(cx1, cy1), self.at(x, y));
        self.path.curve_to(c0, c1, p);
    }

    fn close(&mut self) {
        self.path.close_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atlas() -> Atlas {
        let mut atlas = Atlas::default();
        atlas.set_fonts(&FontDefinitions::default());
        atlas
    }

    fn drawn(shape: Shape) -> Slot {
        match shape {
            Shape::Drawn { slot, .. } => slot,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn draws_each_glyph_once_per_size_and_offset() {
        let mut a = atlas();
        let mono = FontFamily::Monospace;
        let m = drawn(a.glyph(&mono, 'M', 14.0, 1.0, 0));
        // Above the baseline, about as wide as a cell.
        assert!(m.top < 0 && m.h >= 8 && (6..=10).contains(&m.w), "{m:?}");
        let ink: u32 = (0..m.h)
            .flat_map(|y| (0..m.w).map(move |x| (x, y)))
            .map(|(x, y)| u32::from(a.pixels[((m.y + y) * a.side + m.x + x) as usize]))
            .sum();
        assert!(ink > 0);
        let rows = a.upload().rows.unwrap();
        assert!(rows.0 <= m.y && rows.1 >= m.y + m.h);
        assert!(a.upload().rows.is_none());
        // The same glyph again: no new drawing.
        assert_eq!(drawn(a.glyph(&mono, 'M', 14.0, 1.0, 0)), m);
        assert!(a.upload().rows.is_none());
        // Another offset or size: another glyph.
        assert_ne!(drawn(a.glyph(&mono, 'M', 14.0, 1.0, 1)), m);
        assert_ne!(drawn(a.glyph(&mono, 'M', 14.0, 2.0, 0)), m);
        assert_eq!(a.glyph(&mono, ' ', 14.0, 1.0, 0), Shape::Blank);
        // Hack: 1901 of 2048 units above the baseline.
        assert!((a.ascent(&mono, 14.0) - 14.0 * 1901.0 / 2048.0).abs() < 0.01);
    }

    #[test]
    fn falls_back_through_the_family_and_then_to_egui() {
        let mut a = atlas();
        let mono = FontFamily::Monospace;
        // Hack lacks √: Ubuntu-Light, next in the chain, has it.
        assert!(matches!(
            a.glyph(&mono, '√', 14.0, 1.0, 0),
            Shape::Drawn { .. }
        ));
        // Nothing in the chain has it: egui (and the system fonts) try.
        assert_eq!(a.glyph(&mono, '\u{10FFFD}', 14.0, 1.0, 0), Shape::Elsewhere);
        // An unknown family reads as the monospace one.
        let other = FontFamily::Name("term:missing".into());
        assert!(matches!(
            a.glyph(&other, 'a', 14.0, 1.0, 0),
            Shape::Drawn { .. }
        ));
    }

    #[test]
    fn grows_and_starts_over_when_full() {
        let mut a = atlas();
        let mono = FontFamily::Monospace;
        let first = a.upload().generation;
        // Big glyphs fill the first image: it grows, keeping what it had.
        let mut size = 40.0;
        while a.side == START_SIDE {
            a.glyph(&mono, 'W', size, 1.0, 0);
            size += 1.0;
        }
        assert!(a.upload().generation > first);
        assert!(matches!(
            a.glyph(&mono, 'W', 40.0, 1.0, 0),
            Shape::Drawn { .. }
        ));
        // At the largest size, glyphs go to egui until the next frame.
        while !a.full {
            a.glyph(&mono, 'W', size, 1.0, 0);
            size += 1.0;
        }
        assert_eq!(a.side, MAX_SIDE);
        assert_eq!(a.glyph(&mono, 'Q', 13.0, 1.0, 0), Shape::Elsewhere);
        let generation = a.upload().generation;
        a.begin_frame();
        assert!(a.upload().generation > generation);
        assert!(matches!(
            a.glyph(&mono, 'Q', 13.0, 1.0, 0),
            Shape::Drawn { .. }
        ));
    }

    #[test]
    fn packs_glyphs_apart() {
        let mut a = Atlas::with_side(64);
        let mut placed = Vec::new();
        for _ in 0..20 {
            let (x, y) = a.allocate(10, 7).unwrap();
            placed.push((x, y));
        }
        for (i, &(x, y)) in placed.iter().enumerate() {
            assert!(x >= PAD && y >= PAD && x + 10 + PAD <= a.side && y + 7 + PAD <= a.side);
            for &(ox, oy) in &placed[..i] {
                let apart = x >= ox + 10 + PAD
                    || ox >= x + 10 + PAD
                    || y >= oy + 7 + PAD
                    || oy >= y + 7 + PAD;
                assert!(apart, "{x},{y} touches {ox},{oy}");
            }
        }
    }
}
