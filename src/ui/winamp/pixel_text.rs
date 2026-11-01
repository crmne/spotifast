//! Pixel-aligned, unsmoothed text for the Winamp playlist.
//!
//! Each line is rasterized once at skin resolution with monochrome hinting and
//! no anti-aliasing, then cached and nearest-neighbor scaled with the skin.
//!
//! A line is shaped before it is drawn, with the shaper epaint itself uses, so
//! Arabic letters join instead of standing as isolated forms, and its runs are
//! placed in visual order, so a right-to-left title reads the way every other
//! view draws it.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use harfrust::{Direction, ShapeOptions, ShaperData, UnicodeBuffer};
use skrifa::MetadataProvider as _;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, HintingInstance, OutlinePen, Target};
use unicode_bidi::{Level, ParagraphBidiInfo};

/// Eight points at 96 dpi, as Windows rounded it.
pub const SIZE_PX: u32 = 11;
/// Lines kept before the cache is emptied; a queue is far shorter.
const CACHE_LIMIT: usize = 512;

/// A line of text as a bitmap the size it would be on a screen of skin
/// pixels: white ink on nothing, tinted when drawn.
pub struct Line {
    pub texture: TextureHandle,
    pub width: u32,
    pub height: u32,
    /// The rows that actually carry ink: the face's metrics pad the
    /// raster above and below, and a drawing scaled by the full height
    /// leaves the glyphs small (#104). `ink_top` is the first such row.
    pub ink_top: u32,
    pub ink_height: u32,
}

/// Font fallback order: Arial or a system equivalent, Inter, emoji, then
/// system script fallbacks. This mirrors Windows font linking.
static FACES: LazyLock<Vec<(&'static [u8], u32)>> = LazyLock::new(|| {
    let mut faces: Vec<(&'static [u8], u32)> = match crate::system_fonts::pledit_face() {
        Some(face) => vec![(&face.bytes, face.index)],
        None => vec![(fastframe_fonts::INTER, 0)],
    };
    faces.push((include_bytes!("../../../assets/fonts/NotoEmoji.ttf"), 0));
    for fallback in fastframe_fonts::system::fallbacks() {
        faces.push((fallback.bytes, fallback.index));
    }
    faces
});

/// A face opened for drawing, with its hinting programme at the size.
struct Face {
    font: skrifa::FontRef<'static>,
    hinting: Option<HintingInstance>,
    /// The shaper's parsed GSUB and GPOS tables, so a run is shaped by the
    /// same face that draws it.
    shaper: ShaperData,
    /// Font units to skin pixels at `SIZE_PX`.
    scale: f32,
}

impl Face {
    /// The glyphs of a run in the order they are drawn, left to right: a
    /// right-to-left run comes back already in that order, with its letters
    /// joined and its ligatures formed.
    fn shape(&self, text: &str, rtl: bool) -> harfrust::GlyphBuffer {
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();
        buffer.set_direction(if rtl {
            Direction::RightToLeft
        } else {
            Direction::LeftToRight
        });
        self.shaper
            .shaper(&self.font)
            .build()
            .shape(buffer, ShapeOptions::new())
    }
}

/// Lines drawn so far, and the faces they are drawn with.
#[derive(Default)]
pub struct PixelText {
    lines: HashMap<String, Line>,
    /// A width for each line measured, kept because a row too long to fit
    /// is measured once for every length it is cut back to, and shaping
    /// every candidate again each frame would cost more than the rest of
    /// the list.
    widths: HashMap<String, f32>,
    faces: Vec<Face>,
}

impl PixelText {
    /// Drops every texture, for when the window they belong to is gone.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.widths.clear();
    }

    fn faces(&mut self) -> &[Face] {
        if self.faces.is_empty() {
            let size = Size::new(SIZE_PX as f32);
            for (bytes, index) in FACES.iter() {
                let Ok(font) = skrifa::FontRef::from_index(bytes, *index) else {
                    continue;
                };
                let hinting = HintingInstance::new(
                    &font.outline_glyphs(),
                    size,
                    LocationRef::default(),
                    Target::Mono,
                )
                .ok();
                let units = font
                    .metrics(Size::unscaled(), LocationRef::default())
                    .units_per_em;
                let shaper = ShaperData::new(&font);
                let scale = SIZE_PX as f32 / f32::from(units.max(1));
                self.faces.push(Face {
                    font,
                    hinting,
                    shaper,
                    scale,
                });
            }
        }
        &self.faces
    }

    /// The face that draws a character, and the character it draws: one no
    /// face has is drawn as the question mark, the way the skin's own font
    /// says what it cannot show.
    fn face_for(faces: &[Face], character: char) -> (usize, char) {
        let index = faces
            .iter()
            .position(|face| face.font.charmap().map(character).is_some());
        match index {
            Some(index) => (index, character),
            None => (0, '?'),
        }
    }

    /// How wide a line would be, in skin pixels, from the shaped advances.
    pub fn width(&mut self, text: &str) -> f32 {
        if let Some(width) = self.widths.get(text) {
            return *width;
        }
        let width = {
            let faces = self.faces();
            let mut width = 0.0;
            for (rtl, run) in runs(text) {
                for (index, segment) in segments(faces, &text[run], rtl) {
                    let face = &faces[index];
                    width += advance(&face.shape(&segment, rtl), face.scale);
                }
            }
            width
        };
        if self.widths.len() >= CACHE_LIMIT {
            self.widths.clear();
        }
        self.widths.insert(text.to_string(), width);
        width
    }

    /// A line, drawn now if it has not been.
    pub fn line(&mut self, ctx: &egui::Context, text: &str) -> &Line {
        if self.lines.len() >= CACHE_LIMIT {
            self.lines.clear();
            self.widths.clear();
        }
        if !self.lines.contains_key(text) {
            let image = self.rasterise(text);
            let [width, height] = image.size;
            let inked: Vec<u32> = (0..height)
                .filter(|y| (0..width).any(|x| image[(x, *y)].a() > 0))
                .map(|y| y as u32)
                .collect();
            let (ink_top, ink_height) = match (inked.first(), inked.last()) {
                (Some(first), Some(last)) => (*first, last - first + 1),
                _ => (0, height as u32),
            };
            let texture =
                ctx.load_texture(format!("pledit:{text}"), image, TextureOptions::NEAREST);
            self.lines.insert(
                text.to_string(),
                Line {
                    texture,
                    width: width as u32,
                    height: height as u32,
                    ink_top,
                    ink_height,
                },
            );
        }
        &self.lines[text]
    }

    fn rasterise(&mut self, text: &str) -> ColorImage {
        let faces = self.faces();
        let Some(primary) = faces.first() else {
            return ColorImage::filled([1, 1], Color32::TRANSPARENT);
        };
        let size = Size::new(SIZE_PX as f32);
        let location = LocationRef::default();
        let metrics = primary.font.metrics(size, location);
        let ascent = metrics.ascent.ceil();
        let height = (ascent + (-metrics.descent).ceil()).max(1.0) as u32;

        let mut paths = Vec::new();
        let mut x = 0.0f32;
        for (rtl, run) in runs(text) {
            draw_run(faces, &text[run], rtl, ascent, &mut x, &mut paths);
        }

        let width = (x.ceil() as u32).max(1);
        let mut image = ColorImage::filled([width as usize, height as usize], Color32::TRANSPARENT);
        let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
            return image;
        };
        let mut paint = tiny_skia::Paint {
            anti_alias: false,
            ..tiny_skia::Paint::default()
        };
        paint.set_color_rgba8(255, 255, 255, 255);
        for path in &paths {
            pixmap.fill_path(
                path,
                &paint,
                tiny_skia::FillRule::Winding,
                tiny_skia::Transform::identity(),
                None,
            );
        }
        for y in 0..height {
            for x in 0..width {
                if pixmap.pixel(x, y).is_some_and(|pixel| pixel.alpha() > 0) {
                    image[(x as usize, y as usize)] = Color32::WHITE;
                }
            }
        }
        image
    }
}

/// A line's runs in the order they are drawn, each with whether it reads
/// right to left. A row that mixes scripts, a Latin artist and an Arabic
/// title say, is one run each, and every run keeps its own direction.
fn runs(text: &str) -> Vec<(bool, Range<usize>)> {
    if text.is_ascii() {
        return vec![(false, 0..text.len())];
    }
    let info = ParagraphBidiInfo::new(text, None);
    let (levels, runs) = info.visual_runs(0..text.len());
    runs.into_iter()
        .map(|run| {
            let level = levels.get(run.start).copied().unwrap_or(Level::ltr());
            (level.is_rtl(), run)
        })
        .collect()
}

/// The stretches of a run one face draws, in the order they are drawn: a
/// character the first face lacks comes from the first later one that has
/// it, and shaping cannot carry from one face to another. A right-to-left
/// run is laid out from the right, so its stretches are taken from the last.
fn segments(faces: &[Face], text: &str, rtl: bool) -> Vec<(usize, String)> {
    let mut segments: Vec<(usize, String)> = Vec::new();
    for character in text.chars() {
        let (index, drawn) = PixelText::face_for(faces, character);
        match segments.last_mut() {
            Some((face, run)) if *face == index => run.push(drawn),
            _ => segments.push((index, drawn.to_string())),
        }
    }
    if rtl {
        segments.reverse();
    }
    segments
}

/// How far the glyphs of a shaped run carry the pen, in skin pixels.
fn advance(glyphs: &harfrust::GlyphBuffer, scale: f32) -> f32 {
    glyphs
        .glyph_positions()
        .iter()
        .map(|position| (position.x_advance as f32 * scale).round())
        .sum()
}

/// Draws a run from `x` on, and leaves `x` past its last glyph. Each glyph
/// is rounded to a whole skin pixel, which is what keeps the drawing sharp.
fn draw_run(
    faces: &[Face],
    text: &str,
    rtl: bool,
    ascent: f32,
    x: &mut f32,
    paths: &mut Vec<tiny_skia::Path>,
) {
    let size = Size::new(SIZE_PX as f32);
    let location = LocationRef::default();
    for (index, segment) in segments(faces, text, rtl) {
        let face = &faces[index];
        let glyphs = face.shape(&segment, rtl);
        for (info, position) in glyphs.glyph_infos().iter().zip(glyphs.glyph_positions()) {
            let glyph = skrifa::GlyphId::new(info.glyph_id);
            // A mark rides above or below its base, so it starts where the
            // shaper puts it rather than on the pen's own baseline.
            let dx = (position.x_offset as f32 * face.scale).round();
            let rise = (position.y_offset as f32 * face.scale).round();
            if let Some(outline) = face.font.outline_glyphs().get(glyph) {
                let mut pen = Pen::new((*x + dx).round(), ascent - rise);
                match &face.hinting {
                    Some(hinting) => {
                        let _ = outline.draw(DrawSettings::hinted(hinting, false), &mut pen);
                    }
                    None => {
                        let _ = outline.draw(DrawSettings::unhinted(size, location), &mut pen);
                    }
                }
                if let Some(path) = pen.builder.finish() {
                    paths.push(path);
                }
            }
            *x += (position.x_advance as f32 * face.scale).round();
        }
    }
}

/// Collects an outline into a path, moved to its place on the line and
/// turned the right way up.
struct Pen {
    builder: tiny_skia::PathBuilder,
    x: f32,
    baseline: f32,
}

impl Pen {
    fn new(x: f32, baseline: f32) -> Self {
        Self {
            builder: tiny_skia::PathBuilder::new(),
            x,
            baseline,
        }
    }

    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x, self.baseline - y)
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.builder.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.builder.line_to(x, y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (cx0, cy0) = self.at(cx0, cy0);
        let (x, y) = self.at(x, y);
        self.builder.quad_to(cx0, cy0, x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (cx0, cy0) = self.at(cx0, cy0);
        let (cx1, cy1) = self.at(cx1, cy1);
        let (x, y) = self.at(x, y);
        self.builder.cubic_to(cx0, cy0, cx1, cy1, x, y);
    }

    fn close(&mut self) {
        self.builder.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_ink_on_nothing_at_the_skin_size() {
        let mut text = PixelText::default();
        let image = text.rasterise("Bonobo - Rosewood");
        assert!(image.size[1] >= SIZE_PX as usize && image.size[1] <= 2 * SIZE_PX as usize);
        let ink = image
            .pixels
            .iter()
            .filter(|pixel| **pixel == Color32::WHITE)
            .count();
        assert!(ink > 40, "only {ink} pixels of ink");
        assert!(
            image
                .pixels
                .iter()
                .all(|pixel| *pixel == Color32::WHITE || *pixel == Color32::TRANSPARENT),
            "a pixel is neither ink nor nothing"
        );
        let blank = text.rasterise("");
        assert_eq!(blank.size[0], 1);
        assert!(
            blank
                .pixels
                .iter()
                .all(|pixel| *pixel == Color32::TRANSPARENT)
        );
    }

    #[test]
    fn a_script_the_face_lacks_comes_from_a_borrowed_face() {
        let mut text = PixelText::default();
        let character = '\u{591c}';
        let question = text.rasterise("?").size[0];
        let kanji = text.rasterise(&character.to_string()).size[0];
        // Whether any face on this machine has the character at all: a
        // CI runner has fallback faces but no CJK one, and then the kanji
        // is drawn as the question mark on purpose.
        let covered = text
            .faces()
            .iter()
            .any(|face| face.font.charmap().map(character).is_some());
        if covered {
            // A CJK glyph is square, so far wider than a question mark.
            assert!(kanji > question, "the kanji drew as a question mark");
        } else {
            assert_eq!(kanji, question);
        }
    }

    /// A row of two scripts is a run each, and the right-to-left run is
    /// drawn where it is read, at the right.
    #[test]
    fn a_line_of_two_scripts_is_a_run_each_in_drawing_order() {
        let word = "\u{633}\u{644}\u{627}\u{645}";
        let line = format!("Dallo - {word}");
        assert_eq!(runs("Bonobo"), vec![(false, 0..6)]);
        let mixed = runs(&line);
        assert_eq!(mixed.len(), 2, "runs {mixed:?}");
        let (left_rtl, left) = mixed[0].clone();
        let (right_rtl, right) = mixed[1].clone();
        assert!(!left_rtl && left.start == 0 && line[left].trim_end() == "Dallo -");
        assert!(right_rtl && &line[right] == word);
    }

    /// Arabic letters take their joined shapes, and a right-to-left run
    /// comes back in the order it is drawn: its last letter first.
    #[test]
    fn arabic_letters_join_and_read_from_the_right() {
        let mut text = PixelText::default();
        let word = "\u{633}\u{644}\u{627}\u{645}";
        // A machine with no Arabic face draws the word as question marks,
        // and then there is nothing here to check.
        let Some(index) = text.faces().iter().position(|face| {
            word.chars()
                .all(|character| face.font.charmap().map(character).is_some())
        }) else {
            return;
        };
        let faces = text.faces();
        let face = &faces[index];
        let shaped = face.shape(word, true);
        let clusters: Vec<u32> = shaped
            .glyph_infos()
            .iter()
            .map(|info| info.cluster)
            .collect();
        assert!(
            clusters.windows(2).all(|pair| pair[0] >= pair[1]),
            "clusters {clusters:?} are not in drawing order"
        );
        assert!(
            clusters
                .first()
                .zip(clusters.last())
                .is_some_and(|(first, last)| first > last),
            "the run starts at its first letter, not its last: {clusters:?}"
        );
        let shapes: Vec<u32> = shaped
            .glyph_infos()
            .iter()
            .map(|info| info.glyph_id)
            .collect();
        let isolated: Vec<u32> = word
            .chars()
            .filter_map(|character| face.font.charmap().map(character))
            .map(|glyph| glyph.to_u32())
            .collect();
        assert_ne!(shapes, isolated, "the word was drawn letter by letter");
    }

    /// A right-to-left run's stretches are drawn from the right, so a line
    /// a fallback face and the skin font share still reaches the screen in
    /// reading order.
    #[test]
    fn a_right_to_left_run_draws_its_face_stretches_from_the_right() {
        let mut text = PixelText::default();
        // The emoji is last in reading order, so it is leftmost on the
        // screen. It also comes from the bundled emoji face, so the run is
        // one stretch per face even on a machine with no Arabic face.
        let line = "\u{633}\u{644}\u{627}\u{645} \u{1F525}";
        let mut runs = runs(line);
        assert_eq!(runs.len(), 1, "{runs:?}");
        let (rtl, run) = runs.remove(0);
        assert!(rtl, "the line does not read right to left");
        let faces = text.faces();
        let drawn = segments(faces, &line[run], rtl);
        assert!(drawn.len() > 1, "the line is one face's: {drawn:?}");
        assert_eq!(drawn[0].1, "\u{1F525}", "drawn {drawn:?}");
    }

    #[test]
    fn width_grows_with_the_text_and_matches_the_drawing_roughly() {
        let mut text = PixelText::default();
        let short = text.width("Otomo");
        let long = text.width("Khruangbin - Otomo");
        assert!(long > short && short > 0.0);
        let drawn = text.rasterise("Khruangbin - Otomo").size[0] as f32;
        assert!(
            (drawn - long).abs() <= long * 0.15,
            "drawn {drawn}, measured {long}"
        );
    }

    #[test]
    fn emojis_are_drawn_from_bundled_emoji_face() {
        let mut text = PixelText::default();
        let image = text.rasterise("🔥 🎵 ❤️ 🚀");
        assert!(image.size[0] > 0 && image.size[1] > 0);
        // Ensure not all pixels are transparent (i.e. ink is drawn)
        assert!(image.pixels.contains(&Color32::WHITE));
    }
}
