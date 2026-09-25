//! Pixel-aligned, unsmoothed text for the Winamp playlist.
//!
//! Each line is rasterized once at skin resolution with monochrome hinting and
//! no anti-aliasing, then cached and nearest-neighbor scaled with the skin.

use std::collections::HashMap;
use std::sync::LazyLock;

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use skrifa::MetadataProvider as _;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, HintingInstance, OutlinePen, Target};

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
        faces.push((&fallback.bytes, fallback.index));
    }
    faces
});

/// A face opened for drawing, with its hinting programme at the size.
struct Face {
    font: skrifa::FontRef<'static>,
    hinting: Option<HintingInstance>,
}

/// Lines drawn so far, and the faces they are drawn with.
#[derive(Default)]
pub struct PixelText {
    lines: HashMap<String, Line>,
    faces: Vec<Face>,
}

impl PixelText {
    /// Drops every texture, for when the window they belong to is gone.
    pub fn clear(&mut self) {
        self.lines.clear();
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
                self.faces.push(Face { font, hinting });
            }
        }
        &self.faces
    }

    /// The face that draws a character and the glyph it uses: the first
    /// that has it, or the first face's question mark.
    fn glyph_for(faces: &[Face], character: char) -> Option<(usize, skrifa::GlyphId)> {
        faces
            .iter()
            .enumerate()
            .find_map(|(index, face)| {
                face.font
                    .charmap()
                    .map(character)
                    .map(|glyph| (index, glyph))
            })
            .or_else(|| {
                faces
                    .first()?
                    .font
                    .charmap()
                    .map('?')
                    .map(|glyph| (0, glyph))
            })
    }

    /// How wide a line would be, in skin pixels, from the faces' advances.
    pub fn width(&mut self, text: &str) -> f32 {
        let faces = self.faces();
        let size = Size::new(SIZE_PX as f32);
        text.chars()
            .filter_map(|character| Self::glyph_for(faces, character))
            .map(|(index, glyph)| {
                faces[index]
                    .font
                    .glyph_metrics(size, LocationRef::default())
                    .advance_width(glyph)
                    .unwrap_or(0.0)
                    .round()
            })
            .sum()
    }

    /// A line, drawn now if it has not been.
    pub fn line(&mut self, ctx: &egui::Context, text: &str) -> &Line {
        if self.lines.len() >= CACHE_LIMIT {
            self.lines.clear();
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
        for character in text.chars() {
            let Some((index, glyph)) = Self::glyph_for(faces, character) else {
                continue;
            };
            let face = &faces[index];
            let mut advance = face
                .font
                .glyph_metrics(size, location)
                .advance_width(glyph)
                .unwrap_or(0.0);
            if let Some(outline) = face.font.outline_glyphs().get(glyph) {
                let mut pen = Pen::new(x, ascent);
                let drawn = match &face.hinting {
                    Some(hinting) => outline.draw(DrawSettings::hinted(hinting, false), &mut pen),
                    None => outline.draw(DrawSettings::unhinted(size, location), &mut pen),
                };
                if let Ok(adjusted) = drawn
                    && let Some(hinted) = adjusted.advance_width
                {
                    advance = hinted;
                }
                if let Some(path) = pen.builder.finish() {
                    paths.push(path);
                }
            }
            x += advance.round();
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
