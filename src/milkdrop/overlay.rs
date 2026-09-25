//! Text overlays for help, track details, and short messages.
//!
//! Text is rasterized on the CPU with skrifa and tiny-skia, uploaded as one
//! bitmap, and blended over projectM's frame on a textured quad.
//!
//! Sizes are based on a 480-pixel-tall reference and scale with the window.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use eframe::glow;
use eframe::glow::HasContext as _;
use skrifa::MetadataProvider as _;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};

/// How long the fade at the end of an overlay's stay takes.
const FADE: Duration = Duration::from_millis(900);
/// The screen the sizes below are in the pixels of.
const REFERENCE_HEIGHT: f32 = 480.0;
/// Space between the text and the window's edge.
const MARGIN: f32 = 14.0;
/// Space around the text inside its bitmap.
const PAD: u32 = 10;
/// Between the key column and what it does.
const COLUMN_GAP: f32 = 14.0;
/// The shadow's offset, right and down, as MilkDrop moved it.
const SHADOW: u32 = 1;
/// How much of black lies under the keys.
const BOX_ALPHA: u8 = 190;
/// The corner radius of that box.
const BOX_RADIUS: f32 = 8.0;
/// How much the playing song grows as it fades, so that it leaves the
/// picture rather than merely going out.
const DRIFT: f32 = 0.06;

/// The faces, in the order they are asked: Inter, the emoji face, then
/// what the system lends for scripts those cannot draw.
static FACES: LazyLock<Vec<(&'static [u8], u32)>> = LazyLock::new(|| {
    let mut faces: Vec<(&'static [u8], u32)> = vec![
        (fastframe_fonts::INTER, 0),
        (include_bytes!("../../assets/fonts/NotoEmoji.ttf"), 0),
    ];
    for fallback in fastframe_fonts::system::fallbacks() {
        faces.push((&fallback.bytes, fallback.index));
    }
    faces
});

/// Where an overlay sits in the window.
#[derive(Clone, Copy, PartialEq)]
pub enum Place {
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Place {
    /// Which edge the lines line up on: the one the block sits against.
    fn align(self) -> Align {
        match self {
            Place::Center => Align::Center,
            Place::TopLeft | Place::BottomLeft => Align::Left,
            Place::TopRight | Place::BottomRight => Align::Right,
        }
    }
}

/// Which edge a line of its own lines up on.
#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
    Right,
}

/// How an overlay sits behind its text.
#[derive(Clone, Copy, PartialEq)]
pub enum Backing {
    /// A shadow a pixel down and right, as the song title had.
    Shadow,
    /// Black under the whole block, so a page of keys stays readable
    /// whatever the picture is doing.
    Box,
}

/// A piece of text and how it is drawn.
#[derive(Clone)]
pub struct Span {
    pub text: String,
    /// The size on a 480-tall screen; the window grows it from there.
    pub px: f32,
    /// 400 plain, 600 for the keys themselves, 700 bold.
    pub weight: f32,
    /// White at 1.0; less for what should sit back.
    pub tint: f32,
}

impl Span {
    pub fn new(text: impl Into<String>, px: f32) -> Self {
        Self {
            text: text.into(),
            px,
            weight: 400.0,
            tint: 1.0,
        }
    }

    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    pub fn tint(mut self, tint: f32) -> Self {
        self.tint = tint;
        self
    }
}

/// A line of an overlay.
#[derive(Clone)]
pub enum Row {
    /// One span, on its own line, centred in the block.
    Line(Span),
    /// A key and what it does, in two columns.
    Keys { key: Span, does: Span },
    /// A heading over the keys under it.
    Heading(Span),
    /// A line's worth of nothing.
    Gap(f32),
}

/// White ink coverage, one byte a pixel, row zero on top.
struct Raster {
    width: u32,
    height: u32,
    alpha: Vec<u8>,
}

impl Raster {
    fn empty(px: f32) -> Self {
        let height = px.ceil().max(1.0) as u32;
        Self {
            width: 1,
            height,
            alpha: vec![0; height as usize],
        }
    }
}

/// One span's coverage, at `px` tall type.
fn raster(span: &Span, px: f32) -> Raster {
    let px = px.max(1.0);
    let size = Size::new(px);
    let fonts: Vec<skrifa::FontRef<'static>> = FACES
        .iter()
        .filter_map(|(bytes, index)| skrifa::FontRef::from_index(bytes, *index).ok())
        .collect();
    let Some(primary) = fonts.first() else {
        return Raster::empty(px);
    };
    // Inter's own axes: weight for the emphasis, and the optical size it
    // wants for type this large, which keeps big text from looking loose.
    let location = primary
        .axes()
        .location([("wght", span.weight), ("opsz", px.clamp(14.0, 32.0))]);
    let location = LocationRef::from(&location);
    let metrics = primary.metrics(size, location);
    let ascent = metrics.ascent.ceil();
    let height = (ascent + (-metrics.descent).ceil()).max(1.0) as u32;

    // The first face that has a character draws it; a question mark from
    // the first face stands in for what none of them have.
    let glyph_for = |character: char| {
        fonts
            .iter()
            .enumerate()
            .find_map(|(index, font)| font.charmap().map(character).map(|glyph| (index, glyph)))
            .or_else(|| fonts.first()?.charmap().map('?').map(|glyph| (0, glyph)))
    };

    let mut paths = Vec::new();
    let mut x = 0.0f32;
    for character in span.text.chars() {
        let Some((index, glyph)) = glyph_for(character) else {
            continue;
        };
        let font = &fonts[index];
        let advance = font
            .glyph_metrics(size, location)
            .advance_width(glyph)
            .unwrap_or(0.0);
        if let Some(outline) = font.outline_glyphs().get(glyph) {
            let mut pen = Pen::new(x, ascent);
            if outline
                .draw(DrawSettings::unhinted(size, location), &mut pen)
                .is_ok()
                && let Some(path) = pen.builder.finish()
            {
                paths.push(path);
            }
        }
        x += advance;
    }
    let width = (x.ceil() as u32).max(1);
    let Some(mut pixmap) = tiny_skia::Pixmap::new(width, height) else {
        return Raster::empty(px);
    };
    let mut paint = tiny_skia::Paint::default();
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
    let alpha = pixmap.pixels().iter().map(|pixel| pixel.alpha()).collect();
    Raster {
        width,
        height,
        alpha,
    }
}

/// One row, laid out: what to draw and where, within the block.
struct Placed {
    raster: Raster,
    /// Left edge, before padding; `None` centres the row in the block.
    left: Option<f32>,
    top: f32,
    tint: f32,
}

/// The rows laid out and inked into one RGBA bitmap. `grow` takes the
/// sizes above to this window's.
fn block_rgba(rows: &[Row], backing: Backing, align: Align, grow: f32) -> (u32, u32, Vec<u8>) {
    // The keys line up in a column of their own, as wide as the widest.
    let key_column = rows
        .iter()
        .filter_map(|row| match row {
            Row::Keys { key, .. } => Some(raster(key, key.px * grow).width as f32),
            _ => None,
        })
        .fold(0.0f32, f32::max);
    let gap = COLUMN_GAP * grow;

    let mut placed: Vec<Placed> = Vec::new();
    let mut top = 0.0f32;
    for row in rows {
        match row {
            Row::Gap(px) => top += px * grow,
            Row::Line(span) | Row::Heading(span) => {
                let ink = raster(span, span.px * grow);
                let height = ink.height as f32;
                let left = matches!(row, Row::Heading(_)).then_some(0.0);
                placed.push(Placed {
                    raster: ink,
                    left,
                    top,
                    tint: span.tint,
                });
                top += height;
            }
            Row::Keys { key, does } => {
                let key_ink = raster(key, key.px * grow);
                let does_ink = raster(does, does.px * grow);
                let height = key_ink.height.max(does_ink.height) as f32;
                // The key sits at the right of its column, so every
                // description starts on the same line down the block.
                let key_left = (key_column - key_ink.width as f32).max(0.0);
                placed.push(Placed {
                    raster: key_ink,
                    left: Some(key_left),
                    top,
                    tint: key.tint,
                });
                placed.push(Placed {
                    raster: does_ink,
                    left: Some(key_column + gap),
                    top,
                    tint: does.tint,
                });
                top += height;
            }
        }
    }

    let pad = (PAD as f32 * grow).round() as u32;
    let content_width = placed
        .iter()
        .map(|item| item.left.unwrap_or(0.0) + item.raster.width as f32)
        .fold(1.0f32, f32::max);
    let width = content_width.ceil() as u32 + pad * 2 + SHADOW;
    let height = top.ceil().max(1.0) as u32 + pad * 2 + SHADOW;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    // Straight alpha, composited over.
    let mut blend = |x: i64, y: i64, ink: [u8; 3], a: u8| {
        if a == 0 || x < 0 || y < 0 || x >= width as i64 || y >= height as i64 {
            return;
        }
        let at = ((y as u32 * width + x as u32) * 4) as usize;
        let src_a = a as u32;
        let dst_a = rgba[at + 3] as u32;
        let out_a = src_a + dst_a * (255 - src_a) / 255;
        if out_a == 0 {
            return;
        }
        for channel in 0..3 {
            let src = ink[channel] as u32 * src_a;
            let dst = rgba[at + channel] as u32 * dst_a * (255 - src_a) / 255;
            rgba[at + channel] = ((src + dst) / out_a) as u8;
        }
        rgba[at + 3] = out_a as u8;
    };

    if backing == Backing::Box {
        let radius = BOX_RADIUS * grow;
        for y in 0..height {
            for x in 0..width {
                if inside_rounded(x as f32, y as f32, width as f32, height as f32, radius) {
                    blend(x as i64, y as i64, [0, 0, 0], BOX_ALPHA);
                }
            }
        }
    }

    // The shadow first, where there is one, then the text over it.
    for pass in 0..2 {
        if pass == 0 && backing != Backing::Shadow {
            continue;
        }
        for item in &placed {
            let ink = &item.raster;
            let free = width as f32 - SHADOW as f32 - ink.width as f32;
            let left = match item.left {
                Some(left) => pad as f32 + left,
                None => match align {
                    Align::Left => pad as f32,
                    Align::Center => free / 2.0,
                    Align::Right => free - pad as f32,
                },
            };
            let (offset, colour) = if pass == 0 {
                (SHADOW as f32, [0u8, 0, 0])
            } else {
                (0.0, [255u8, 255, 255])
            };
            let x0 = (left + offset).round() as i64;
            let y0 = (pad as f32 + item.top + offset).round() as i64;
            for y in 0..ink.height {
                for x in 0..ink.width {
                    let a = ink.alpha[(y * ink.width + x) as usize];
                    let a = if pass == 0 {
                        a
                    } else {
                        (a as f32 * item.tint) as u8
                    };
                    blend(x0 + x as i64, y0 + y as i64, colour, a);
                }
            }
        }
    }
    (width, height, rgba)
}

/// Whether a pixel is inside a rectangle with rounded corners.
fn inside_rounded(x: f32, y: f32, width: f32, height: f32, radius: f32) -> bool {
    let radius = radius.min(width / 2.0).min(height / 2.0);
    let dx = (radius - x).max(x - (width - 1.0 - radius)).max(0.0);
    let dy = (radius - y).max(y - (height - 1.0 - radius)).max(0.0);
    dx * dx + dy * dy <= radius * radius
}

/// The overlay: one bitmap at a time, drawn until its stay runs out.
pub struct Overlay {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    texture: glow::Texture,
    alpha_at: glow::UniformLocation,
    size: (u32, u32),
    place: Place,
    grow: f32,
    shown: Option<(Instant, Duration)>,
}

impl Overlay {
    pub fn new(gl: &glow::Context) -> Option<Self> {
        // SAFETY: the context is current; everything made here lives as
        // long as the window's context, which dies with the process.
        unsafe {
            let program = gl.create_program().ok()?;
            let sources = [
                (
                    glow::VERTEX_SHADER,
                    "#version 330 core\n\
                     layout(location = 0) in vec4 quad;\n\
                     out vec2 v_uv;\n\
                     void main() {\n\
                         v_uv = quad.zw;\n\
                         gl_Position = vec4(quad.xy, 0.0, 1.0);\n\
                     }",
                ),
                (
                    glow::FRAGMENT_SHADER,
                    "#version 330 core\n\
                     in vec2 v_uv;\n\
                     uniform sampler2D ink;\n\
                     uniform float alpha;\n\
                     out vec4 color;\n\
                     void main() {\n\
                         color = texture(ink, v_uv);\n\
                         color.a *= alpha;\n\
                     }",
                ),
            ];
            for (kind, source) in sources {
                let shader = gl.create_shader(kind).ok()?;
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    return None;
                }
                gl.attach_shader(program, shader);
                gl.delete_shader(shader);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return None;
            }
            let alpha_at = gl.get_uniform_location(program, "alpha")?;
            let vao = gl.create_vertex_array().ok()?;
            let vbo = gl.create_buffer().ok()?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 4, glow::FLOAT, false, 16, 0);
            gl.bind_vertex_array(None);
            let texture = gl.create_texture().ok()?;
            Some(Self {
                program,
                vao,
                vbo,
                texture,
                alpha_at,
                size: (0, 0),
                place: Place::Center,
                grow: 1.0,
                shown: None,
            })
        }
    }

    /// Draws the rows into the bitmap and starts its stay.
    pub fn show(
        &mut self,
        gl: &glow::Context,
        rows: &[Row],
        place: Place,
        backing: Backing,
        hold: Duration,
        window: (u32, u32),
    ) {
        let grow = (window.1.max(1) as f32 / REFERENCE_HEIGHT).clamp(1.0, 3.0);
        let (width, height, rgba) = block_rgba(rows, backing, place.align(), grow);
        // SAFETY: the context is current; the texture is this overlay's own.
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                width as i32,
                height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&rgba)),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        self.size = (width, height);
        self.place = place;
        self.grow = grow;
        self.shown = Some((Instant::now(), hold));
    }

    /// Sends the overlay away early, wherever it is in its stay.
    pub fn hide(&mut self) {
        self.shown = None;
    }

    /// Whether something is on show right now.
    pub fn showing(&self) -> bool {
        self.shown.is_some()
    }

    /// Draws the overlay over the frame, fading at the end of its stay.
    /// Returns whether it is still on show, so frames keep coming.
    pub fn draw(&mut self, gl: &glow::Context, window: (u32, u32)) -> bool {
        let Some((since, hold)) = self.shown else {
            return false;
        };
        let age = since.elapsed();
        if age >= hold + FADE {
            self.shown = None;
            return false;
        }
        let gone = if age <= hold {
            0.0
        } else {
            (age - hold).as_secs_f32() / FADE.as_secs_f32()
        };
        // Fading out, the song drifts a little larger, so that it dissolves
        // into the picture rather than simply switching off.
        let alpha = 1.0 - gone;
        let drift = if self.place == Place::Center {
            1.0 + DRIFT * gone
        } else {
            1.0
        };
        let (win_w, win_h) = (window.0.max(1) as f32, window.1.max(1) as f32);
        let (w, h) = (self.size.0 as f32 * drift, self.size.1 as f32 * drift);
        let margin = MARGIN * self.grow;
        let right = (win_w - margin - w).max(0.0);
        let bottom = (win_h - margin - h).max(0.0);
        let (left, top) = match self.place {
            Place::Center => ((win_w - w) / 2.0, (win_h - h) / 2.0),
            Place::TopLeft => (margin, margin),
            Place::TopRight => (right, margin),
            Place::BottomLeft => (margin, bottom),
            Place::BottomRight => (right, bottom),
        };
        // Window pixels to clip space; row zero of the bitmap is its top.
        let x0 = left / win_w * 2.0 - 1.0;
        let x1 = (left + w) / win_w * 2.0 - 1.0;
        let y0 = 1.0 - top / win_h * 2.0;
        let y1 = 1.0 - (top + h) / win_h * 2.0;
        let quad: [f32; 16] = [
            x0, y0, 0.0, 0.0, //
            x1, y0, 1.0, 0.0, //
            x0, y1, 0.0, 1.0, //
            x1, y1, 1.0, 1.0,
        ];
        // SAFETY: the context is current; the state touched is put back.
        unsafe {
            // The engine leaves the viewport at the picture's inner size
            // when a lower resolution is on; clip space maps through the
            // viewport, so an overlay drawn into it came out small.
            gl.viewport(0, 0, window.0.max(1) as i32, window.1.max(1) as i32);
            gl.use_program(Some(self.program));
            gl.uniform_1_f32(Some(&self.alpha_at), alpha);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, quad_bytes(&quad), glow::STREAM_DRAW);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.disable(glow::DEPTH_TEST);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.disable(glow::BLEND);
            gl.bind_vertex_array(None);
            gl.use_program(None);
        }
        true
    }
}

/// The quad's floats as the bytes GL takes.
fn quad_bytes(quad: &[f32; 16]) -> &[u8] {
    // SAFETY: f32s reread as bytes, same length, no padding.
    unsafe { std::slice::from_raw_parts(quad.as_ptr().cast::<u8>(), std::mem::size_of_val(quad)) }
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

    /// The rasteriser inks what it is given, and a heavier weight leaves
    /// more of it at the same size.
    #[test]
    fn text_becomes_ink() {
        let short = raster(&Span::new("Keys", 20.0), 20.0);
        let long = raster(&Span::new("next preset, on the beat", 20.0), 20.0);
        assert!(short.alpha.iter().any(|a| *a > 0), "letters leave ink");
        assert!(long.width > short.width);

        let ink = |span: &Span| {
            raster(span, span.px)
                .alpha
                .iter()
                .map(|a| *a as u32)
                .sum::<u32>()
        };
        assert!(
            ink(&Span::new("Keys", 20.0).weight(700.0)) > ink(&Span::new("Keys", 20.0)),
            "bold is the heavier of the two"
        );
    }

    /// The keys line up: however wide the key is, the block is wide
    /// enough for the widest of them and its description beside it.
    #[test]
    fn the_key_column_lines_up() {
        let rows = [
            Row::Keys {
                key: Span::new("F", 14.0),
                does: Span::new("full screen", 14.0),
            },
            Row::Keys {
                key: Span::new("Ctrl+Shift+K", 14.0),
                does: Span::new("close", 14.0),
            },
        ];
        let (width, height, rgba) = block_rgba(&rows, Backing::Box, Align::Left, 1.0);
        assert_eq!(rgba.len(), (width * height * 4) as usize);
        let narrow = raster(&Span::new("F", 14.0), 14.0).width;
        let wide = raster(&Span::new("Ctrl+Shift+K", 14.0), 14.0).width;
        let close = raster(&Span::new("close", 14.0), 14.0).width;
        assert!(wide > narrow);
        assert!(
            width >= wide + close,
            "the widest key and its text both fit"
        );
    }

    /// The song sits on a shadow with the picture showing around it; the
    /// keys sit on a box, and its corners are rounded.
    #[test]
    fn backings_are_drawn_under_the_text() {
        let song = [Row::Line(
            Span::new("Wish You Were Here", 26.0).weight(700.0),
        )];
        let (_, _, shadowed) = block_rgba(&song, Backing::Shadow, Align::Center, 1.0);
        assert!(
            shadowed.chunks(4).any(|px| px[3] == 0),
            "a shadow leaves the picture showing around it"
        );

        let (width, height, boxed) = block_rgba(&song, Backing::Box, Align::Center, 1.0);
        let middle = ((height / 2 * width + width / 2) * 4) as usize;
        assert!(boxed[middle + 3] > 0, "the box lies under the block");
        assert_eq!(boxed[3], 0, "the box has rounded corners");
    }

    /// A block in a right-hand corner lines its text up on the right, so
    /// a short line sits under the end of a long one, not its middle.
    #[test]
    fn a_corner_lines_its_text_up_on_its_own_edge() {
        let rows = [
            Row::Line(Span::new("Wish You Were Here", 16.0)),
            Row::Line(Span::new("Incubus", 13.0)),
        ];
        // The ink of the short line, by the column it starts in.
        let starts_at = |align| {
            let (width, height, rgba) = block_rgba(&rows, Backing::Shadow, align, 1.0);
            let last = (0..height)
                .rev()
                .find(|y| (0..width).any(|x| rgba[((y * width + x) * 4 + 3) as usize] > 0))
                .expect("the second line leaves ink");
            let first_x = (0..width)
                .find(|x| rgba[((last * width + x) * 4 + 3) as usize] > 0)
                .expect("a row of ink starts somewhere");
            (first_x, width)
        };
        let (left_start, width) = starts_at(Align::Left);
        let (right_start, _) = starts_at(Align::Right);
        assert!(
            right_start > left_start,
            "lined up right, the short line starts further in"
        );
        assert!(right_start * 2 > width, "and past the middle of the block");
    }

    /// Sizes are in the pixels of a 480-tall screen: a taller window grows
    /// the type to keep the same picture.
    #[test]
    fn type_grows_with_the_window() {
        let rows = [Row::Line(Span::new("Keys", 24.0))];
        let (_, small, _) = block_rgba(&rows, Backing::Box, Align::Left, 1.0);
        let (_, large, _) = block_rgba(&rows, Backing::Box, Align::Left, 2.0);
        assert!(
            large > small * 3 / 2,
            "twice the window, nearly twice the type"
        );
    }
}
