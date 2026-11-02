//! Draws a view's [`Paint`] list in software, for a skin's preview in
//! Settings.
//!
//! The list is the same one the window draws, so a preview shows what the
//! skin shows. Pictures, fills and text are drawn; the things that move or
//! come from the player stand still here: a few fixed bars for a
//! visualisation, a plain square for the cover.

use std::sync::Arc;

use skrifa::MetadataProvider as _;
use skrifa::instance::{Location, LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use tiny_skia::{FillRule, Paint as Ink, PathBuilder, Pixmap, Transform};

use super::Box2;
use super::paint::{Measure, Paint, TextPaint, line_height};
use crate::skin::{Bitmap, Rgb};

/// The bars drawn for a visualisation, as shares of its height in tenths.
const BARS: [u8; 19] = [3, 5, 8, 7, 9, 6, 7, 5, 6, 4, 5, 7, 5, 3, 4, 3, 2, 3, 1];
const BAR_LOW: Rgb = [40, 200, 70];
const BAR_HIGH: Rgb = [230, 210, 50];
/// What stands in for the cover.
const COVER: Rgb = [58, 58, 66];
/// Longest text drawn, in characters; a preview cannot show more.
const MAX_TEXT: usize = 256;
/// The gap before scrolling text comes round again, as the window draws it.
const SCROLL_GAP: f32 = 25.0;

/// A see-through canvas of this size.
pub fn canvas(width: u32, height: u32) -> Bitmap {
    Bitmap {
        width,
        height,
        rgba: vec![0; 4 * width as usize * height as usize],
    }
}

/// `over` with `alpha` on top of `pixel`, neither premultiplied.
fn blend(pixel: &mut [u8], over: Rgb, alpha: u8) {
    let alpha = u32::from(alpha);
    if alpha == 0 {
        return;
    }
    if alpha == 255 {
        pixel[..3].copy_from_slice(&over);
        pixel[3] = 255;
        return;
    }
    let under = u32::from(pixel[3]) * (255 - alpha) / 255;
    let total = alpha + under;
    for (channel, top) in pixel[..3].iter_mut().zip(over) {
        *channel = ((u32::from(top) * alpha + u32::from(*channel) * under) / total) as u8;
    }
    pixel[3] = total as u8;
}

/// The part of `area` on a canvas, as `(left, top, right, bottom)`.
fn on_canvas(canvas: &Bitmap, area: Box2) -> (u32, u32, u32, u32) {
    let left = area.x.max(0) as u32;
    let top = area.y.max(0) as u32;
    let right = area.x.saturating_add(area.width).max(0) as u32;
    let bottom = area.y.saturating_add(area.height).max(0) as u32;
    (
        left,
        top,
        right.min(canvas.width),
        bottom.min(canvas.height),
    )
}

fn meet(a: Box2, b: Box2) -> Box2 {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let bottom =
        a.y.saturating_add(a.height)
            .min(b.y.saturating_add(b.height));
    Box2 {
        x,
        y,
        width: (right - x).max(0),
        height: (bottom - y).max(0),
    }
}

/// Runs `paint` for every canvas pixel of `area` inside `clip`.
fn each_pixel(
    canvas: &mut Bitmap,
    area: Box2,
    clip: &[Box2],
    mut paint: impl FnMut(u32, u32, &mut [u8]),
) {
    for rect in clip {
        let (left, top, right, bottom) = on_canvas(canvas, meet(area, *rect));
        let stride = 4 * canvas.width as usize;
        for y in top..bottom {
            let row = y as usize * stride;
            for x in left..right {
                let at = row + 4 * x as usize;
                paint(x, y, &mut canvas.rgba[at..at + 4]);
            }
        }
    }
}

/// Draws `part` of `bitmap` at `(x, y)`, `size` big, as a window would:
/// nearest neighbour, each pixel faded by `alpha`.
#[allow(clippy::too_many_arguments)]
pub fn draw_bitmap(
    canvas: &mut Bitmap,
    bitmap: &Bitmap,
    part: Box2,
    x: i32,
    y: i32,
    size: (i32, i32),
    alpha: u8,
    clip: &[Box2],
) {
    let (width, height) = size;
    if width <= 0 || height <= 0 || part.width <= 0 || part.height <= 0 {
        return;
    }
    let area = Box2 {
        x,
        y,
        width,
        height,
    };
    each_pixel(canvas, area, clip, |at_x, at_y, pixel| {
        let across = i64::from(at_x as i32 - x) * i64::from(part.width) / i64::from(width);
        let down = i64::from(at_y as i32 - y) * i64::from(part.height) / i64::from(height);
        let (from_x, from_y) = (i64::from(part.x) + across, i64::from(part.y) + down);
        if from_x < 0 || from_y < 0 {
            return;
        }
        if let Some(source) = bitmap.pixel(from_x as u32, from_y as u32) {
            let faded = u32::from(source[3]) * u32::from(alpha) / 255;
            blend(pixel, [source[0], source[1], source[2]], faded as u8);
        }
    });
}

pub fn fill(canvas: &mut Bitmap, area: Box2, color: Rgb, alpha: u8, clip: &[Box2]) {
    each_pixel(canvas, area, clip, |_, _, pixel| {
        blend(pixel, color, alpha);
    });
}

/// The strip at rest, in a preview: a row of dots.
fn wave(canvas: &mut Bitmap, area: Box2, color: Rgb, clip: &[Box2]) {
    for x in (0..area.width).step_by(3) {
        let dot = Box2 {
            x: area.x + x,
            y: area.y + area.height / 2,
            width: 1,
            height: 1,
        };
        fill(canvas, dot, color, 255, clip);
    }
}

fn effects(canvas: &mut Bitmap, area: Box2, clip: &[Box2]) {
    let slot = area.width / BARS.len() as i32;
    if slot < 2 {
        return;
    }
    for (index, tenths) in BARS.iter().enumerate() {
        let height = area.height * i32::from(*tenths) / 10;
        let bar = Box2 {
            x: area.x + slot * index as i32,
            y: area.y + area.height - height,
            width: slot - 1,
            height,
        };
        let shade = u32::from(*tenths);
        let mixed = |low: u8, high: u8| {
            ((u32::from(low) * (10 - shade) + u32::from(high) * shade) / 10) as u8
        };
        let color = [
            mixed(BAR_LOW[0], BAR_HIGH[0]),
            mixed(BAR_LOW[1], BAR_HIGH[1]),
            mixed(BAR_LOW[2], BAR_HIGH[2]),
        ];
        fill(canvas, bar, color, 255, clip);
    }
}

/// Collects a glyph's outline, moved to where the glyph goes.
struct Pen<'a> {
    path: &'a mut PathBuilder,
    x: f32,
    y: f32,
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to(self.x + x, self.y - y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to(self.x + x, self.y - y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.path
            .quad_to(self.x + cx, self.y - cy, self.x + x, self.y - y);
    }

    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        self.path.cubic_to(
            self.x + ax,
            self.y - ay,
            self.x + bx,
            self.y - by,
            self.x + x,
            self.y - y,
        );
    }

    fn close(&mut self) {
        self.path.close();
    }
}

/// The interface font, at a weight. Text a font cannot draw is left out: a
/// preview is too small for anyone to read it.
fn face() -> Option<skrifa::FontRef<'static>> {
    skrifa::FontRef::from_index(fastframe_fonts::INTER, 0).ok()
}

fn weight(font: &skrifa::FontRef<'_>, bold: bool) -> Location {
    font.axes()
        .location([("wght", if bold { 700.0 } else { 400.0 })])
}

/// How wide text is in the interface font.
pub fn measure() -> Arc<Measure> {
    Arc::new(|text: &str, size: f32, bold: bool, _mono: bool| {
        let Some(font) = face() else {
            return 0.0;
        };
        let location = weight(&font, bold);
        line_width(&font, &location, text, size)
    })
}

fn line_width(font: &skrifa::FontRef<'_>, location: &Location, text: &str, size: f32) -> f32 {
    let (map, metrics) = (
        font.charmap(),
        font.glyph_metrics(Size::new(size), LocationRef::from(location)),
    );
    text.chars()
        .filter_map(|character| map.map(character))
        .filter_map(|glyph| metrics.advance_width(glyph))
        .sum()
}

/// `text` cut into lines no wider than `width`, at spaces.
fn wrapped(
    font: &skrifa::FontRef<'_>,
    location: &Location,
    text: &str,
    size: f32,
    width: f32,
) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let longer = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && line_width(font, location, &longer, size) > width {
                lines.push(std::mem::replace(&mut line, word.to_owned()));
            } else {
                line = longer;
            }
        }
        lines.push(line);
    }
    lines
}

fn text(canvas: &mut Bitmap, paint: &TextPaint, clip: &[Box2]) {
    let Some(font) = face() else {
        return;
    };
    let shown: String = paint.text.chars().take(MAX_TEXT).collect();
    let location = weight(&font, paint.bold);
    let size = paint.size_px;
    let area_width = paint.area.width as f32;
    let lines = if paint.wrap {
        wrapped(&font, &location, &shown, size, area_width)
    } else {
        vec![shown]
    };
    let rows = font.metrics(Size::new(size), LocationRef::from(&location));
    let row_height = line_height(size) as f32;
    let (map, glyphs) = (font.charmap(), font.outline_glyphs());
    let advances = font.glyph_metrics(Size::new(size), LocationRef::from(&location));
    let mut path = PathBuilder::new();
    let mut add = |line: &str, from: f32, baseline: f32| {
        let mut x = from;
        for glyph in line.chars().filter_map(|character| map.map(character)) {
            if let Some(outline) = glyphs.get(glyph) {
                let mut pen = Pen {
                    path: &mut path,
                    x,
                    y: baseline,
                };
                let settings =
                    DrawSettings::unhinted(Size::new(size), LocationRef::from(&location));
                let _ = outline.draw(settings, &mut pen);
            }
            x += advances.advance_width(glyph).unwrap_or(0.0);
        }
    };
    for (row, line) in lines.iter().enumerate() {
        let width = line_width(&font, &location, line, size);
        let from = paint.area.x as f32
            + match paint.align {
                -1 => 0.0,
                0 => (area_width - width) / 2.0,
                _ => area_width - width,
            };
        let baseline = paint.area.y as f32 + row as f32 * row_height + rows.ascent;
        add(line, from - paint.scroll, baseline);
        if paint.scroll != 0.0 {
            let step = width + SCROLL_GAP;
            let step = if paint.scroll > 0.0 { step } else { -step };
            add(line, from - paint.scroll + step, baseline);
        }
    }
    let Some(path) = path.finish() else {
        return;
    };
    // Ink on a sheet just the size of the text's area.
    let (left, top, right, bottom) = on_canvas(canvas, paint.area);
    if right <= left || bottom <= top {
        return;
    }
    let Some(mut sheet) = Pixmap::new(right - left, bottom - top) else {
        return;
    };
    let mut ink = Ink::default();
    ink.set_color_rgba8(255, 255, 255, 255);
    ink.anti_alias = true;
    sheet.fill_path(
        &path,
        &ink,
        FillRule::Winding,
        Transform::from_translate(-(left as f32), -(top as f32)),
        None,
    );
    let width = sheet.width();
    let data = sheet.data();
    each_pixel(canvas, paint.area, clip, |x, y, pixel| {
        let at = 4 * ((y - top) * width + (x - left)) as usize;
        let coverage = u32::from(data[at + 3]) * u32::from(paint.alpha) / 255;
        blend(pixel, paint.color, coverage as u8);
    });
}

/// Draws the list, in order, on a canvas.
pub fn draw(paints: &[Paint], canvas: &mut Bitmap) {
    for paint in paints {
        match paint {
            Paint::Picture {
                picture,
                frame,
                x,
                y,
                part,
                stretch,
                alpha,
                clip,
            } => {
                if let Some(frame) = picture.frames.get(*frame) {
                    let size = stretch.unwrap_or((part.width, part.height));
                    draw_bitmap(canvas, &frame.bitmap, *part, *x, *y, size, *alpha, clip);
                }
            }
            Paint::Fill {
                area,
                color,
                alpha,
                clip,
            } => fill(canvas, *area, *color, *alpha, clip),
            Paint::Text { text: paint, clip } => text(canvas, paint, clip),
            Paint::Effects { area, clip, .. } => effects(canvas, *area, clip),
            Paint::Wave {
                area, color, clip, ..
            } => wave(canvas, *area, *color, clip),
            Paint::Art {
                area,
                background,
                clip,
            } => {
                if let Some(background) = background {
                    fill(canvas, *area, *background, 255, clip);
                }
                let side = area.width.min(area.height);
                let cover = Box2 {
                    x: area.x + (area.width - side) / 2,
                    y: area.y + (area.height - side) / 2,
                    width: side,
                    height: side,
                };
                fill(canvas, cover, COVER, 255, clip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(canvas: &Bitmap) -> Vec<Box2> {
        vec![Box2 {
            x: 0,
            y: 0,
            width: canvas.width as i32,
            height: canvas.height as i32,
        }]
    }

    #[test]
    fn half_see_through_over_nothing_keeps_its_colour() {
        let mut pixel = [0, 0, 0, 0];
        blend(&mut pixel, [200, 100, 50], 128);
        assert_eq!(pixel, [200, 100, 50, 128]);
    }

    #[test]
    fn half_see_through_over_solid_mixes() {
        let mut pixel = [0, 0, 0, 255];
        blend(&mut pixel, [200, 100, 50], 128);
        assert_eq!(pixel[3], 255);
        assert!((99..=101).contains(&pixel[0]), "{pixel:?}");
    }

    #[test]
    fn a_fill_stays_inside_its_clip_and_the_canvas() {
        let mut page = canvas(4, 4);
        let clip = [Box2 {
            x: 1,
            y: 1,
            width: 100,
            height: 1,
        }];
        let area = Box2 {
            x: -5,
            y: -5,
            width: 50,
            height: 50,
        };
        fill(&mut page, area, [9, 8, 7], 255, &clip);
        assert_eq!(page.pixel(1, 1), Some([9, 8, 7, 255]));
        assert_eq!(page.pixel(3, 1), Some([9, 8, 7, 255]));
        assert_eq!(page.pixel(1, 0), Some([0, 0, 0, 0]));
        assert_eq!(page.pixel(1, 2), Some([0, 0, 0, 0]));
    }

    #[test]
    fn a_picture_scales_by_nearest_neighbour_and_keeps_its_holes() {
        let source = Bitmap {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 0, 0],
        };
        let mut page = canvas(4, 2);
        let part = Box2 {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        let clip = whole(&page);
        draw_bitmap(&mut page, &source, part, 0, 0, (4, 2), 255, &clip);
        assert_eq!(page.pixel(1, 1), Some([255, 0, 0, 255]));
        assert_eq!(page.pixel(2, 0), Some([0, 0, 0, 0]));
    }

    #[test]
    fn a_picture_past_the_edges_of_its_bitmap_draws_what_is_there() {
        let source = Bitmap {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        };
        let mut page = canvas(3, 1);
        let part = Box2 {
            x: -1,
            y: 0,
            width: 3,
            height: 1,
        };
        let clip = whole(&page);
        draw_bitmap(&mut page, &source, part, 0, 0, (3, 1), 255, &clip);
        assert_eq!(page.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(page.pixel(1, 0), Some([1, 2, 3, 255]));
        assert_eq!(page.pixel(2, 0), Some([0, 0, 0, 0]));
    }

    #[test]
    fn text_leaves_ink_in_its_area_and_only_there() {
        let mut page = canvas(80, 40);
        let clip = whole(&page);
        let paint = TextPaint {
            text: "Hill".into(),
            area: Box2 {
                x: 10,
                y: 10,
                width: 60,
                height: 20,
            },
            size_px: 14.0,
            color: [255, 0, 0],
            alpha: 255,
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            mono: false,
            align: -1,
            scroll: 0.0,
            wrap: false,
        };
        text(&mut page, &paint, &clip);
        let inked: Vec<(u32, u32)> = (0..40)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .filter(|(x, y)| page.pixel(*x, *y).is_some_and(|pixel| pixel[3] > 0))
            .collect();
        assert!(inked.len() > 20, "{}", inked.len());
        assert!(
            inked
                .iter()
                .all(|(x, y)| paint.area.contains(*x as i32, *y as i32))
        );
    }

    #[test]
    fn text_wraps_at_spaces_when_it_may() {
        let font = face().unwrap();
        let location = weight(&font, false);
        let lines = wrapped(&font, &location, "aa bb cc", 14.0, 30.0);
        assert!(lines.len() >= 2, "{lines:?}");
        assert_eq!(lines.join(" "), "aa bb cc");
    }

    #[test]
    fn a_visualisation_stands_still_as_bars() {
        let mut page = canvas(95, 20);
        let clip = whole(&page);
        let area = clip[0];
        effects(&mut page, area, &clip);
        // The first bar reaches a third of the way up, and no higher.
        assert!(page.pixel(1, 19).is_some_and(|pixel| pixel[3] == 255));
        assert!(page.pixel(1, 5).is_some_and(|pixel| pixel[3] == 0));
    }
}
