//! Draws Spotizgeg's built-in Winamp skin: the one the skinned window
//! wears until the listener picks another, and the one any skin's missing
//! pieces are taken from. Winamp's own base skin cannot ship here, so this
//! is an original drawing in the app's dark palette.
//!
//! `cargo run --example default_skin` regenerates
//! `assets/skins/spotizgeg.wsz`, a classic skin in every respect (BMPs in
//! a zip) that Winamp itself could wear. Add `--preview <png>` to also
//! compose the main window at 2x through the same sprite and layout tables
//! the app uses, as a check that the art lines up.

use std::path::PathBuf;

use spotizgeg::skin::layout::{self, Area};
use spotizgeg::skin::sprites::{self, Sheet, Sprite};
use spotizgeg::skin::{Skin, font, zip};

type Rgb = [u8; 3];
/// A box on a canvas: x, y, width, height.
type Box = (i64, i64, i64, i64);

// Spotizgeg's dark palette (src/theme.rs), so the window matches the app.
const WINDOW: Rgb = [0x0f, 0x11, 0x14];
const PANEL: Rgb = [0x15, 0x18, 0x1c];
const SURFACE: Rgb = [0x1d, 0x21, 0x27];
const SURFACE_HOVER: Rgb = [0x26, 0x2b, 0x33];
const SURFACE_ACTIVE: Rgb = [0x2f, 0x35, 0x3f];
const OUTLINE: Rgb = [0x2a, 0x30, 0x38];
const TEXT: Rgb = [0xf2, 0xf4, 0xf6];
const SECONDARY: Rgb = [0xa9, 0xb1, 0xbc];
const DIM: Rgb = [0x6e, 0x77, 0x84];
const ACCENT: Rgb = [0x1e, 0xd7, 0x60];
const ON_ACCENT: Rgb = [0x0a, 0x14, 0x0e];
/// Lamps that are off: the accent, nearly out.
const ACCENT_DARK: Rgb = [0x14, 0x4a, 0x2a];

/// The 5x6 bitmap font, drawn four pixels wide with a blank column after
/// (M, V, and W take the fifth). Rows are separated by `/`; `#` is a pixel.
const FONT: &[(char, &str)] = &[
    ('A', ".##./#..#/####/#..#/#..#"),
    ('B', "###./#..#/###./#..#/###."),
    ('C', ".###/#.../#.../#.../.###"),
    ('D', "###./#..#/#..#/#..#/###."),
    ('E', "####/#.../###./#.../####"),
    ('F', "####/#.../###./#.../#..."),
    ('G', ".###/#.../#.##/#..#/.###"),
    ('H', "#..#/#..#/####/#..#/#..#"),
    ('I', "###/.#./.#./.#./###"),
    ('J', "..##/...#/...#/#..#/.##."),
    ('K', "#..#/#.#./##../#.#./#..#"),
    ('L', "#.../#.../#.../#.../####"),
    ('M', "#...#/##.##/#.#.#/#...#/#...#"),
    ('N', "#..#/##.#/#.##/#..#/#..#"),
    ('O', ".##./#..#/#..#/#..#/.##."),
    ('P', "###./#..#/###./#.../#..."),
    ('Q', ".##./#..#/#..#/#.#./.#.#"),
    ('R', "###./#..#/###./#.#./#..#"),
    ('S', ".###/#.../.##./...#/###."),
    ('T', "###/.#./.#./.#./.#."),
    ('U', "#..#/#..#/#..#/#..#/.##."),
    ('V', "#...#/#...#/.#.#./.#.#./..#.."),
    ('W', "#...#/#...#/#.#.#/##.##/#...#"),
    ('X', "#..#/#..#/.##./#..#/#..#"),
    ('Y', "#..#/#..#/.##./.#../.#.."),
    ('Z', "####/...#/.##./#.../####"),
    ('"', ".#.#/.#.#"),
    ('@', ".###/#..#/#.##/#.../.###"),
    ('0', ".##./#..#/#..#/#..#/.##."),
    ('1', ".#../##../.#../.#../###."),
    ('2', "###./...#/.##./#.../####"),
    ('3', "###./...#/.##./...#/###."),
    ('4', "#..#/#..#/####/...#/...#"),
    ('5', "####/#.../###./...#/###."),
    ('6', ".##./#.../###./#..#/.##."),
    ('7', "####/...#/..#./.#../.#.."),
    ('8', ".##./#..#/.##./#..#/.##."),
    ('9', ".##./#..#/.###/...#/.##."),
    ('\u{2026}', "..../..../..../..../#.#.#"),
    ('.', "..../..../..../..../.#.."),
    (':', "..../.#../..../.#../...."),
    ('(', ".#./#../#../#../.#."),
    (')', "#../.#./.#./.#./#.."),
    ('-', "..../..../.###/..../...."),
    ('\'', ".#../.#../..../..../...."),
    ('!', ".#../.#../.#../..../.#.."),
    ('_', "..../..../..../..../..../####"),
    ('+', "..../.#../###./.#../...."),
    ('\\', "#.../.#../.#../..#./...#"),
    ('/', "...#/..#./.#../.#../#..."),
    ('[', "##./#../#../#../##."),
    (']', "##./.#./.#./.#./##."),
    ('^', ".#../#.#./..../..../...."),
    ('&', ".#../#.#./.#../#.#./.##."),
    ('%', "#..#/...#/..#./.#../#..#"),
    (',', "..../..../..../..../.#../#..."),
    ('=', "..../####/..../####/...."),
    ('$', ".###/##../.##./..##/###."),
    ('#', ".#.#/####/.#.#/####/.#.#"),
    ('\u{c5}', ".##./..../.##./#..#/####/#..#"),
    ('\u{d6}', "#..#/..../.##./#..#/#..#/.##."),
    ('\u{c4}', "#..#/..../.##./#..#/####/#..#"),
    ('?', "###./...#/.##./..../.#.."),
    ('*', "..../#.#./.#../#.#./...."),
];

fn art(character: char) -> &'static str {
    if character == ' ' {
        return "";
    }
    let character = character.to_ascii_uppercase();
    FONT.iter()
        .find(|(c, _)| *c == character)
        .or_else(|| FONT.iter().find(|(c, _)| *c == '?'))
        .map(|(_, rows)| *rows)
        .unwrap_or("")
}

const PREVIOUS: &str = "#....#/#...##/#..###/#.####/#..###/#...##/#....#";
const PLAY: &str = "##..../###.../####../#####./####../###.../##....";
const PAUSE: &str = "##.##/##.##/##.##/##.##/##.##/##.##/##.##";
const STOP: &str = "######/######/######/######/######/######";
const NEXT: &str = "#....#/##...#/###..#/####.#/###..#/##...#/#....#";
const EJECT: &str = "...#.../..###../.#####./#######/......./#######/#######";
const CLOSE: &str = "#...#/.#.#./..#../.#.#./#...#";
const MINIMIZE: &str = "...../...../...../...../#####";
const SHADE: &str = "#####/...../...../...../.....";
const UNSHADE: &str = "...../...../#####/...../.....";

struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<Rgb>,
}

impl Canvas {
    fn new(width: u32, height: u32, fill: Rgb) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill; (width * height) as usize],
        }
    }

    fn set(&mut self, x: i64, y: i64, color: Rgb) {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return;
        }
        self.pixels[(y as u32 * self.width + x as u32) as usize] = color;
    }

    fn rect(&mut self, x: i64, y: i64, width: i64, height: i64, color: Rgb) {
        for py in y..y + height {
            for px in x..x + width {
                self.set(px, py, color);
            }
        }
    }

    /// A one pixel outline just inside the box.
    fn frame(&mut self, (x, y, width, height): Box, color: Rgb) {
        self.rect(x, y, width, 1, color);
        self.rect(x, y + height - 1, width, 1, color);
        self.rect(x, y, 1, height, color);
        self.rect(x + width - 1, y, 1, height, color);
    }

    /// A filled box lit from the top left.
    fn bevel(&mut self, (x, y, width, height): Box, fill: Rgb, light: Rgb, dark: Rgb) {
        self.rect(x, y, width, height, fill);
        self.rect(x, y, width, 1, light);
        self.rect(x, y, 1, height, light);
        self.rect(x, y + height - 1, width, 1, dark);
        self.rect(x + width - 1, y, 1, height, dark);
    }

    /// Pixel art from rows of `#` and `.`, its top left at (x, y).
    fn glyph(&mut self, x: i64, y: i64, rows: &str, color: Rgb) {
        for (dy, row) in rows.split('/').enumerate() {
            for (dx, cell) in row.chars().enumerate() {
                if cell == '#' {
                    self.set(x + dx as i64, y + dy as i64, color);
                }
            }
        }
    }

    /// Pixel art centred in a box, nudged down and right by `shift`.
    fn glyph_centred(&mut self, (x, y, width, height): Box, rows: &str, color: Rgb, shift: i64) {
        let glyph_width = rows
            .split('/')
            .map(|row| row.len() as i64)
            .max()
            .unwrap_or(0);
        let glyph_height = rows.split('/').count() as i64;
        self.glyph(
            x + (width - glyph_width) / 2 + shift,
            y + (height - glyph_height) / 2 + shift,
            rows,
            color,
        );
    }

    /// A line of the bitmap font, five pixels per character.
    fn text(&mut self, x: i64, y: i64, text: &str, color: Rgb) {
        for (index, character) in text.chars().enumerate() {
            self.glyph(x + 5 * index as i64, y, art(character), color);
        }
    }

    fn text_centred(&mut self, (x, y, width, height): Box, text: &str, color: Rgb) {
        let text_width = 5 * text.chars().count() as i64 - 1;
        self.text(
            x + (width - text_width) / 2,
            y + (height - 5) / 2,
            text,
            color,
        );
    }

    /// The app's mark in pixels: the disc with a darker rim and a lit top,
    /// and the play triangle.
    fn logo(&mut self, cx: i64, cy: i64, radius: i64, disc: Rgb, glyph: Rgb) {
        let shade = |c: Rgb, f: f32| c.map(|v| (f32::from(v) * f).round() as u8);
        let lift =
            |c: Rgb, f: f32| c.map(|v| (f32::from(v) + (255.0 - f32::from(v)) * f).round() as u8);
        let outer = radius * radius + radius / 2;
        let inner = (radius - 1) * (radius - 1) + (radius - 1) / 2;
        for y in -radius..=radius {
            for x in -radius..=radius {
                let d = x * x + y * y;
                if d > outer {
                    continue;
                }
                let colour = if d > inner {
                    shade(disc, 0.7)
                } else if y < 0 {
                    lift(disc, 0.25)
                } else {
                    disc
                };
                self.set(cx + x, cy + y, colour);
            }
        }
        let half = radius / 2;
        for dy in -half..=half {
            let span = half - dy.abs();
            for dx in 0..=span {
                self.set(cx - half / 2 + dx, cy + dy, glyph);
            }
        }
    }

    /// The canvas as a 24-bit BMP, the format Winamp reads.
    fn bmp(&self) -> Vec<u8> {
        let bytes: Vec<u8> = self.pixels.iter().flatten().copied().collect();
        let image = image::RgbImage::from_raw(self.width, self.height, bytes)
            .expect("the canvas is the size it says");
        let mut out = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut out, image::ImageFormat::Bmp)
            .expect("a bitmap encodes");
        out.into_inner()
    }
}

/// The pair of lines Winamp ran either side of a window's title.
fn title_lines(c: &mut Canvas, x0: i64, x1: i64, y: i64) {
    if x1 > x0 {
        c.rect(x0, y, x1 - x0, 1, DIM);
        c.rect(x0, y + 3, x1 - x0, 1, DIM);
    }
}

/// A raised button with pixel art on it.
fn button(canvas: &mut Canvas, area: Box, pressed: bool, rows: &str) {
    if pressed {
        canvas.bevel(area, SURFACE_ACTIVE, WINDOW, SURFACE_HOVER);
        canvas.glyph_centred(area, rows, ACCENT, 1);
    } else {
        canvas.bevel(area, SURFACE, SURFACE_HOVER, WINDOW);
        canvas.glyph_centred(area, rows, SECONDARY, 0);
    }
}

/// A raised button with a word on it, lit when on.
fn label_button(canvas: &mut Canvas, area: Box, pressed: bool, on: bool, label: &str) {
    let color = if on { ACCENT } else { DIM };
    let (x, y, width, height) = area;
    if pressed {
        canvas.bevel(area, SURFACE_ACTIVE, WINDOW, SURFACE_HOVER);
        canvas.text_centred((x + 1, y + 1, width, height), label, color);
    } else {
        canvas.bevel(area, SURFACE, SURFACE_HOVER, WINDOW);
        canvas.text_centred(area, label, color);
    }
}

/// A sunken display area.
fn display(canvas: &mut Canvas, area: Box) {
    let (x, y, width, height) = area;
    canvas.rect(x, y, width, height, WINDOW);
    canvas.frame(area, OUTLINE);
}

/// A slider's grip: a raised block with two grooves.
fn thumb(canvas: &mut Canvas, area: Box, pressed: bool) {
    let (x, y, width, height) = area;
    let (fill, light, dark, groove) = if pressed {
        (ACCENT, ACCENT, ACCENT_DARK, ON_ACCENT)
    } else {
        (SURFACE_HOVER, SURFACE_ACTIVE, WINDOW, DIM)
    };
    canvas.bevel(area, fill, light, dark);
    canvas.rect(x + width / 2 - 2, y + 2, 1, height - 4, groove);
    canvas.rect(x + width / 2 + 1, y + 2, 1, height - 4, groove);
}

fn main_sheet() -> Canvas {
    let mut c = Canvas::new(layout::WINDOW_WIDTH, layout::WINDOW_HEIGHT, PANEL);
    c.frame((0, 0, 275, 116), OUTLINE);
    // The display: time and visualiser on the left, the song and its
    // format on the right, with the sliders tucked under the right half.
    display(&mut c, (9, 21, 95, 46));
    display(&mut c, (106, 21, 165, 35));
    c.text(128, 43, "KBPS", DIM);
    c.text(168, 43, "KHZ", DIM);
    // The colon of the time display is part of the background.
    c.rect(72, 29, 2, 2, ACCENT);
    c.rect(72, 34, 2, 2, ACCENT);
    let about = layout::ABOUT;
    c.logo(
        i64::from(about.x + about.width / 2),
        i64::from(about.y + about.height / 2),
        6,
        ACCENT,
        ON_ACCENT,
    );
    c
}

fn title_bar(c: &mut Canvas, x: i64, y: i64, active: bool, shade: bool) {
    c.rect(x, y, 275, 14, SURFACE);
    c.frame((x, y, 275, 14), OUTLINE);
    let color = if active { TEXT } else { DIM };
    if shade {
        c.text(x + 20, y + 4, "SPOTIZGEG", color);
        // Shade mode's tiny transport lives in the bar itself.
        let glyphs: [(i64, i64, &str); 6] = [
            (169, 7, "#..#/#.##/####/#.##/#..#"),
            (176, 10, "#..../##.../###../##.../#...."),
            (186, 9, "#.#/#.#/#.#/#.#/#.#"),
            (195, 9, "###/###/###/###/###"),
            (204, 10, "#..#/##.#/####/##.#/#..#"),
            (215, 10, ".#./###/.../###/###"),
        ];
        for (gx, width, rows) in glyphs {
            c.glyph_centred((x + gx, y + 2, width, 10), rows, SECONDARY, 0);
        }
        // The time sits in a sunken box, as it does on the display.
        display(c, (x + 124, y + 2, 36, 10));
        display(c, (x + 226, y + 4, 17, 7));
    } else {
        c.text_centred((x, y + 1, 275, 12), "SPOTIZGEG", color);
        let width = 5 * 10 - 1;
        let start = x + (275 - width) / 2;
        title_lines(c, x + 18, start - 8, y + 5);
        title_lines(c, start + width + 8, x + 241, y + 5);
    }
}

fn titlebar_sheet() -> Canvas {
    let mut c = Canvas::new(344, 87, PANEL);
    title_bar(&mut c, 27, 0, true, false);
    title_bar(&mut c, 27, 15, false, false);
    title_bar(&mut c, 27, 29, true, true);
    title_bar(&mut c, 27, 42, false, true);
    // The bars Winamp swapped in for its easter egg: the same here.
    title_bar(&mut c, 27, 57, true, false);
    title_bar(&mut c, 27, 72, false, false);

    for (x, y, pressed) in [(0, 0, false), (0, 9, true)] {
        c.rect(x, y, 9, 9, SURFACE);
        let disc = if pressed { ACCENT_DARK } else { ACCENT };
        c.logo(x + 4, y + 4, 4, disc, ON_ACCENT);
    }
    let small: [(i64, i64, &str); 8] = [
        (9, 0, MINIMIZE),
        (9, 9, MINIMIZE),
        (0, 18, SHADE),
        (9, 18, SHADE),
        (18, 0, CLOSE),
        (18, 9, CLOSE),
        (0, 27, UNSHADE),
        (9, 27, UNSHADE),
    ];
    for (index, (x, y, rows)) in small.into_iter().enumerate() {
        let pressed = index % 2 == 1;
        let (fill, color) = if pressed {
            (SURFACE_ACTIVE, ACCENT)
        } else {
            (SURFACE, SECONDARY)
        };
        c.rect(x, y, 9, 9, fill);
        c.glyph_centred((x, y, 9, 9), rows, color, 0);
    }

    // Shade mode's seek bar and its thumb, at rest, left, and right.
    display(&mut c, (0, 36, 17, 7));
    c.rect(17, 36, 3, 7, SECONDARY);
    c.rect(20, 36, 3, 7, ACCENT);
    c.rect(23, 36, 3, 7, SECONDARY);

    // The clutter bar: O A I D V down the left of the display.
    let letters: [(char, i64); 5] = [('O', 3), ('A', 11), ('I', 18), ('D', 25), ('V', 33)];
    for (x, color) in [(304, DIM), (312, OUTLINE)] {
        c.rect(x, 0, 8, 43, WINDOW);
        for (letter, y) in letters {
            c.glyph_centred((x, y, 8, 7), art(letter), color, 0);
        }
    }
    let selected: [(i64, i64, i64); 5] = [
        (304, 47, 8),
        (312, 55, 7),
        (320, 62, 7),
        (328, 69, 8),
        (336, 77, 7),
    ];
    for ((letter, _), (x, y, height)) in letters.into_iter().zip(selected) {
        c.rect(x, y, 8, height, WINDOW);
        c.glyph_centred((x, y, 8, height), art(letter), ACCENT, 0);
    }
    c
}

fn cbuttons_sheet() -> Canvas {
    let mut c = Canvas::new(136, 36, PANEL);
    let buttons: [(i64, i64, i64, &str); 6] = [
        (0, 23, 18, PREVIOUS),
        (23, 23, 18, PLAY),
        (46, 23, 18, PAUSE),
        (69, 23, 18, STOP),
        (92, 22, 18, NEXT),
        (114, 22, 16, EJECT),
    ];
    for (x, width, height, rows) in buttons {
        button(&mut c, (x, 0, width, height), false, rows);
        button(&mut c, (x, height, width, height), true, rows);
    }
    c
}

fn shufrep_sheet() -> Canvas {
    let mut c = Canvas::new(92, 85, PANEL);
    let states = [(false, false), (true, false), (false, true), (true, true)];
    for (row, (pressed, on)) in states.into_iter().enumerate() {
        let y = 15 * row as i64;
        label_button(&mut c, (0, y, 28, 15), pressed, on, "REP");
        label_button(&mut c, (28, y, 47, 15), pressed, on, "SHUFFLE");
    }
    for (x, pressed) in [(0, false), (46, true)] {
        for (y, on) in [(61, false), (73, true)] {
            label_button(&mut c, (x, y, 23, 12), pressed, on, "EQ");
            label_button(&mut c, (x + 23, y, 23, 12), pressed, on, "PL");
        }
    }
    c
}

fn posbar_sheet() -> Canvas {
    let mut c = Canvas::new(307, 10, PANEL);
    display(&mut c, (0, 0, 248, 10));
    c.rect(2, 4, 244, 2, SURFACE_HOVER);
    thumb(&mut c, (248, 0, 29, 10), false);
    thumb(&mut c, (278, 0, 29, 10), true);
    c
}

fn slider_track(c: &mut Canvas, x: i64, y: i64, width: i64) {
    display(c, (x, y, width, 13));
    c.rect(x + 2, y + 5, width - 4, 3, SURFACE_HOVER);
}

fn volume_sheet() -> Canvas {
    let mut c = Canvas::new(68, 433, PANEL);
    for frame in 0..i64::from(sprites::SLIDER_FRAMES) {
        let y = 15 * frame;
        slider_track(&mut c, 0, y, 68);
        c.rect(2, y + 5, 64 * frame / 27, 3, ACCENT);
    }
    thumb(&mut c, (15, 422, 14, 11), false);
    thumb(&mut c, (0, 422, 14, 11), true);
    c
}

fn balance_sheet() -> Canvas {
    let mut c = Canvas::new(47, 433, PANEL);
    for frame in 0..i64::from(sprites::SLIDER_FRAMES) {
        let y = 15 * frame;
        slider_track(&mut c, 9, y, 38);
        let centre = 9 + 19;
        let half = 16 * frame / 27;
        c.rect(centre - half, y + 5, 2 * half + 1, 3, ACCENT);
        if frame == 0 {
            c.rect(centre, y + 4, 1, 5, DIM);
        }
    }
    thumb(&mut c, (15, 422, 14, 11), false);
    thumb(&mut c, (0, 422, 14, 11), true);
    c
}

fn monoster_sheet() -> Canvas {
    let mut c = Canvas::new(56, 24, WINDOW);
    for (y, color) in [(0, ACCENT), (12, ACCENT_DARK)] {
        c.text_centred((0, y, 29, 12), "STEREO", color);
        c.text_centred((29, y, 27, 12), "MONO", color);
    }
    c
}

fn playpaus_sheet() -> Canvas {
    let mut c = Canvas::new(42, 9, WINDOW);
    c.glyph(
        1,
        1,
        "#...../##..../###.../####../###.../##..../#.....",
        ACCENT,
    );
    c.glyph(10, 1, "##.##/##.##/##.##/##.##/##.##/##.##/##.##", ACCENT);
    c.rect(19, 1, 7, 7, ACCENT);
    c.rect(39, 3, 3, 3, ACCENT);
    c
}

/// A seven-segment digit, nine by thirteen: bit 0 is the top bar, the
/// bits go clockwise from there, and bit 6 is the middle bar.
fn led_digit(c: &mut Canvas, x: i64, y: i64, segments: u8) {
    let bars: [Box; 7] = [
        (1, 0, 7, 2),  // top
        (7, 1, 2, 5),  // top right
        (7, 7, 2, 5),  // bottom right
        (1, 11, 7, 2), // bottom
        (0, 7, 2, 5),  // bottom left
        (0, 1, 2, 5),  // top left
        (1, 5, 7, 2),  // middle
    ];
    for (bit, (bx, by, width, height)) in bars.into_iter().enumerate() {
        if segments & (1 << bit) != 0 {
            c.rect(x + bx, y + by, width, height, ACCENT);
        }
    }
}

const DIGIT_SEGMENTS: [u8; 10] = [
    0b011_1111, // 0
    0b000_0110, // 1
    0b101_1011, // 2
    0b100_1111, // 3
    0b110_0110, // 4
    0b110_1101, // 5
    0b111_1101, // 6
    0b000_0111, // 7
    0b111_1111, // 8
    0b110_1111, // 9
];

fn numbers_sheet(extended: bool) -> Canvas {
    let mut c = Canvas::new(if extended { 108 } else { 99 }, 13, WINDOW);
    for (digit, segments) in DIGIT_SEGMENTS.into_iter().enumerate() {
        led_digit(&mut c, 9 * digit as i64, 0, segments);
    }
    if extended {
        led_digit(&mut c, 99, 0, 0b100_0000);
    }
    c
}

fn text_sheet() -> Canvas {
    let mut c = Canvas::new(155, 18, WINDOW);
    for (character, rows) in FONT {
        let (row, column) = font::cell(*character);
        c.glyph(i64::from(5 * column), i64::from(6 * row), rows, SECONDARY);
    }
    c
}

fn pledit_sheet() -> Canvas {
    let mut c = Canvas::new(280, 186, PANEL);
    // The title row, active above inactive: corner, title, tile, corner.
    for (y, color) in [(0, TEXT), (21, DIM)] {
        c.rect(0, y, 178, 20, SURFACE);
        c.frame((0, y, 178, 20), OUTLINE);
        c.text_centred((26, y, 100, 20), "PLAYLIST", color);
        let width = 5 * 8 - 1;
        let start = 26 + (100 - width) / 2;
        // Across the left corner, the tiles, and the title on both sides
        // of its word; the right corner keeps its buttons.
        title_lines(&mut c, 4, 25, y + 8);
        title_lines(&mut c, 26, start - 8, y + 8);
        title_lines(&mut c, start + width + 8, 126, y + 8);
        title_lines(&mut c, 127, 152, y + 8);
        c.glyph_centred((156, y + 3, 9, 9), SHADE, SECONDARY, 0);
        c.glyph_centred((166, y + 3, 9, 9), CLOSE, SECONDARY, 0);
    }
    // The sides: a plain left edge and a right edge with the scroll track.
    c.rect(0, 42, 1, 29, OUTLINE);
    c.rect(50, 42, 1, 29, OUTLINE);
    c.rect(36, 42, 8, 29, WINDOW);
    c.rect(35, 42, 1, 29, OUTLINE);
    c.rect(44, 42, 1, 29, OUTLINE);
    // Pressed title buttons and the scroll handles.
    for (x, rows) in [(52, CLOSE), (62, SHADE), (150, UNSHADE)] {
        c.rect(x, 42, 9, 9, SURFACE_ACTIVE);
        c.glyph_centred((x, 42, 9, 9), rows, ACCENT, 0);
    }
    thumb(&mut c, (52, 53, 8, 18), false);
    thumb(&mut c, (61, 53, 8, 18), true);
    // Shade mode's bar: left end, tile, right end (inactive, then active).
    // A sunken strip runs along it for the song and its time; the
    // buttons sit on the plain bar at the right.
    for (x, y, width) in [(72, 42, 25), (72, 57, 25), (99, 57, 50), (99, 42, 50)] {
        c.rect(x, y, width, 14, SURFACE);
        c.rect(x, y, width, 1, OUTLINE);
        c.rect(x, y + 13, width, 1, OUTLINE);
    }
    c.rect(72, 42, 1, 14, OUTLINE);
    c.rect(148, 42, 1, 29, OUTLINE);
    // The strip is one sunken box across the whole bar: the left end
    // starts it three pixels in with its own edge, the tile carries it
    // straight across with no edges of its own, and the right end closes
    // it short of the buttons.
    c.rect(75, 44, 22, 10, WINDOW);
    c.rect(75, 44, 22, 1, OUTLINE);
    c.rect(75, 53, 22, 1, OUTLINE);
    c.rect(75, 44, 1, 10, OUTLINE);
    c.rect(72, 59, 25, 10, WINDOW);
    c.rect(72, 59, 25, 1, OUTLINE);
    c.rect(72, 68, 25, 1, OUTLINE);
    for y in [42, 57] {
        c.rect(99, y + 2, 27, 10, WINDOW);
        c.rect(99, y + 2, 27, 1, OUTLINE);
        c.rect(99, y + 11, 27, 1, OUTLINE);
        c.rect(125, y + 2, 1, 10, OUTLINE);
    }
    for (y, color) in [(42, SECONDARY), (57, DIM)] {
        c.glyph_centred((126, y + 3, 9, 9), UNSHADE, color, 0);
        c.glyph_centred((137, y + 3, 9, 9), CLOSE, color, 0);
    }
    // The bottom: both corners, the tile between them, the visualiser's
    // well, and a grip in the corner for resizing.
    c.frame((0, 72, 276, 38), OUTLINE);
    // The left half's menus, painted and quiet: Spotizgeg has no files
    // to add, but a bottom without them looks unfinished.
    for (x, label) in [(14, "ADD"), (43, "REM"), (72, "SEL"), (101, "MISC")] {
        label_button(&mut c, (x, 80, 22, 18), false, false, label);
    }
    // The right half: the running time's box, the little transport, the
    // song's time, LIST OPTS, and the grip. The sheet's right half is a
    // pixel over from where it lands in the window.
    display(&mut c, (130, 78, 64, 12));
    let mini: [&str; 6] = [
        "#..#/#.##/####/#.##/#..#",
        "#..../##.../###../##.../#....",
        "#.#/#.#/#.#/#.#/#.#",
        "###/###/###/###/###",
        "#..#/##.#/####/##.#/#..#",
        ".#./###/.../###/###",
    ];
    for (index, rows) in mini.into_iter().enumerate() {
        c.glyph_centred((129 + 10 * index as i64, 94, 10, 10), rows, SECONDARY, 0);
    }
    display(&mut c, (189, 93, 27, 12));
    label_button(&mut c, (232, 80, 22, 18), false, false, "");
    c.text(233, 83, "LIST", DIM);
    c.text(233, 90, "OPTS", DIM);
    for dy in 0..9 {
        for dx in 0..9 {
            if dx + dy >= 8 && (dx + dy) % 3 == 2 {
                c.set(265 + dx, 99 + dy, DIM);
            }
        }
    }
    c.rect(179, 0, 25, 1, OUTLINE);
    c.rect(179, 37, 25, 1, OUTLINE);
    display(&mut c, (205, 0, 75, 38));
    c
}

/// The equalizer window and its parts: the sliders' 28 frames, the
/// thumbs, the buttons in their states, the graph and the colour the
/// curve takes at each of its rows, and the preamp's line.
fn eqmain_sheet() -> Canvas {
    let mut c = Canvas::new(275, 315, PANEL);
    c.frame((0, 0, 275, 116), OUTLINE);
    c.text(43, 37, "+12 DB", DIM);
    c.text(48, 65, "+0 DB", DIM);
    c.text(43, 96, "-12 DB", DIM);
    c.text(9, 104, "PREAMP", DIM);
    let labels = [
        "60", "170", "310", "600", "1K", "3K", "6K", "12K", "14K", "16K",
    ];
    for (band, label) in labels.iter().enumerate() {
        let centre = 78 + 18 * band as i64 + 7;
        let width = 5 * label.len() as i64 - 1;
        c.text(centre - width / 2, 104, label, DIM);
    }
    for (y, color, button) in [(134, TEXT, SECONDARY), (149, DIM, DIM)] {
        c.rect(0, y, 275, 14, SURFACE);
        c.frame((0, y, 275, 14), OUTLINE);
        c.text_centred((0, y + 1, 275, 12), "EQUALIZER", color);
        let width = 5 * 9 - 1;
        let start = (275 - width) / 2;
        title_lines(&mut c, 18, start - 8, y + 5);
        title_lines(&mut c, start + width + 8, 241, y + 5);
        // The shade button rests in the bar itself; the X is a sprite.
        c.glyph_centred((254, y + 3, 9, 9), SHADE, button, 0);
    }
    c.rect(0, 116, 9, 9, SURFACE);
    c.glyph_centred((0, 116, 9, 9), CLOSE, SECONDARY, 0);
    c.rect(0, 125, 9, 9, SURFACE_ACTIVE);
    c.glyph_centred((0, 125, 9, 9), CLOSE, ACCENT, 0);
    // ON and AUTO: off, on, off pressed, on pressed, left to right.
    for (x, pressed, on) in [
        (10, false, false),
        (69, false, true),
        (128, true, false),
        (187, true, true),
    ] {
        label_button(&mut c, (x, 119, 26, 12), pressed, on, "ON");
        label_button(&mut c, (x + 26, 119, 32, 12), pressed, on, "AUTO");
    }
    // The sliders: a groove with the level filled from the centre.
    for frame in 0..28i64 {
        let x = 13 + (frame % 14) * 15;
        let y = 164 + (frame / 14) * 65;
        display(&mut c, (x, y, 14, 63));
        c.rect(x + 5, y + 31, 4, 1, DIM);
        let level = ((27 - frame) * 62 + 13) / 27;
        if level < 31 {
            c.rect(x + 5, y + level, 4, 31 - level + 1, ACCENT);
        } else if level > 31 {
            c.rect(x + 5, y + 31, 4, level - 31 + 1, ACCENT_DARK);
        }
    }
    thumb(&mut c, (0, 164, 11, 11), false);
    thumb(&mut c, (0, 176, 11, 11), true);
    label_button(&mut c, (224, 164, 44, 12), false, false, "PRESETS");
    label_button(&mut c, (224, 176, 44, 12), true, true, "PRESETS");
    display(&mut c, (0, 294, 113, 19));
    c.rect(1, 303, 111, 1, OUTLINE);
    for row in 0..19i64 {
        let t = row as f32 / 18.0;
        let mix = |channel: usize| {
            let from = f32::from(ACCENT[channel]);
            let to = f32::from(ACCENT_DARK[channel]);
            (from + (to - from) * t).round() as u8
        };
        c.set(115, 294 + row, [mix(0), mix(1), mix(2)]);
    }
    c.rect(0, 314, 113, 1, DIM);
    c
}

/// The equalizer rolled up, as Winamp 2.9 added it: the bar active and
/// inactive with sunken strips for the volume and balance, the three looks
/// of each mini slider's thumb, and the shade, unshade, and close buttons.
fn eq_ex_sheet() -> Canvas {
    let mut c = Canvas::new(275, 56, PANEL);
    for (y, color, button) in [(0, TEXT, SECONDARY), (15, DIM, DIM)] {
        c.rect(0, y, 275, 14, SURFACE);
        c.frame((0, y, 275, 14), OUTLINE);
        c.text(8, y + 4, "EQUALIZER", color);
        for (x, width) in [(61, 97), (164, 43)] {
            display(&mut c, (x, y + 3, width, 9));
            c.rect(x + 2, y + 7, width - 4, 1, SURFACE_HOVER);
        }
        c.text(212, y + 4, "VOL", DIM);
        c.text(231, y + 4, "BAL", DIM);
        c.glyph_centred((254, y + 3, 9, 9), UNSHADE, button, 0);
    }
    // Six thumbs, three pixels wide: low, middle, high for the volume,
    // then left, middle, right for the balance. The high and outer looks
    // light up, the way Winamp's base skin coloured them.
    for (index, x) in [1, 4, 7, 11, 14, 17].into_iter().enumerate() {
        let lit = matches!(index, 2 | 3 | 5);
        let (fill, light) = if lit {
            (ACCENT_DARK, ACCENT)
        } else {
            (SURFACE_HOVER, SURFACE_ACTIVE)
        };
        c.bevel((x, 30, 3, 7), fill, light, WINDOW);
    }
    for (x, y, rows, pressed) in [
        (1, 38, SHADE, true),
        (1, 47, UNSHADE, true),
        (11, 38, CLOSE, false),
        (11, 47, CLOSE, true),
    ] {
        let (fill, color) = if pressed {
            (SURFACE_ACTIVE, ACCENT)
        } else {
            (SURFACE, SECONDARY)
        };
        c.rect(x, y, 9, 9, fill);
        c.glyph_centred((x, y, 9, 9), rows, color, 0);
    }
    c
}

const PLEDIT_TXT: &str = "[Text]\r\nNormal=#A9B1BC\r\nCurrent=#1ED760\r\nNormalBG=#0F1114\r\nSelectedBG=#2F353F\r\nFont=Inter\r\n";

/// Background, grid, sixteen spectrum bands from the top down, the
/// oscilloscope's five shades, and the peak marks.
fn viscolor_txt() -> String {
    let line = |color: Rgb, note: &str| format!("{},{},{} // {note}", color[0], color[1], color[2]);
    let mut lines = vec![line(WINDOW, "background"), line(OUTLINE, "grid")];
    for band in 0..16u8 {
        let t = f32::from(band) / 15.0;
        let mix = |channel: usize| {
            let from = f32::from(ACCENT[channel]);
            let to = f32::from(ACCENT_DARK[channel]);
            (from + (to - from) * t).round() as u8
        };
        lines.push(line([mix(0), mix(1), mix(2)], &format!("band {band}")));
    }
    for shade in [TEXT, SECONDARY, SECONDARY, DIM, DIM] {
        lines.push(line(shade, "oscilloscope"));
    }
    lines.push(line(TEXT, "peaks"));
    lines.join("\r\n") + "\r\n"
}

/// Composes the main window from the sheets on disk, the way the app will.
fn preview(skin: &Skin) -> image::RgbaImage {
    let mut image = image::RgbaImage::new(layout::WINDOW_WIDTH, layout::WINDOW_HEIGHT);
    let mut blit = |sprite: Sprite, x: u32, y: u32| {
        let Some((bitmap, sprite)) = skin.sprite(sprite) else {
            return;
        };
        for dy in 0..sprite.height {
            for dx in 0..sprite.width {
                let Some(pixel) = bitmap.pixel(sprite.x + dx, sprite.y + dy) else {
                    continue;
                };
                if x + dx < image.width() && y + dy < image.height() {
                    image.put_pixel(x + dx, y + dy, image::Rgba(pixel));
                }
            }
        }
    };
    let placed: [(Sprite, Area); 12] = [
        (sprites::MAIN_BACKGROUND, Area::new(0, 0, 275, 116)),
        (sprites::TITLE_BAR_ACTIVE, layout::TITLE_BAR),
        (sprites::OPTIONS_BUTTON, layout::OPTIONS_BUTTON),
        (sprites::MINIMIZE_BUTTON, layout::MINIMIZE_BUTTON),
        (sprites::SHADE_BUTTON, layout::SHADE_BUTTON),
        (sprites::CLOSE_BUTTON, layout::CLOSE_BUTTON),
        (sprites::CLUTTER_BAR, layout::CLUTTER_BAR),
        (sprites::STATUS_PLAYING, layout::STATUS),
        (sprites::WORK_INDICATOR_OFF, layout::WORK_INDICATOR),
        (sprites::NUMS_EX_BLANK, layout::MINUS_EX),
        (sprites::MONO_OFF, layout::MONO),
        (sprites::STEREO_ON, layout::STEREO),
    ];
    for (sprite, area) in placed {
        blit(sprite, area.x, area.y);
    }
    for (digit, area) in [0, 3, 4, 7].into_iter().zip(layout::TIME_DIGITS) {
        blit(sprites::digit_ex(digit), area.x, area.y);
    }
    let mut text = |line: &str, area: Area| {
        for (index, character) in line.chars().enumerate() {
            let x = area.x + 5 * index as u32;
            if x + 5 <= area.x + area.width {
                blit(font::glyph(character), x, area.y);
            }
        }
    };
    text("SPOTIZGEG - A WINAMP SKIN *** ", layout::MARQUEE);
    text("320", layout::KBPS);
    text("44", layout::KHZ);
    let volume = layout::VOLUME;
    blit(sprites::volume_frame(19), volume.x, volume.y);
    blit(
        sprites::VOLUME_THUMB,
        volume.x + layout::VOLUME_TRAVEL * 7 / 10,
        volume.y + 1,
    );
    let balance = layout::BALANCE;
    blit(sprites::balance_frame(0), balance.x, balance.y);
    blit(
        sprites::BALANCE_THUMB,
        balance.x + layout::BALANCE_TRAVEL / 2,
        balance.y + 1,
    );
    let position = layout::POSITION;
    blit(sprites::POSITION_TRACK, position.x, position.y);
    blit(
        sprites::POSITION_THUMB,
        position.x + layout::POSITION_TRAVEL * 3 / 10,
        position.y,
    );
    let controls: [(Sprite, Area); 10] = [
        (sprites::EQ_OFF, layout::EQ_BUTTON),
        (sprites::PLAYLIST_ON, layout::PLAYLIST_BUTTON),
        (sprites::PREVIOUS, layout::PREVIOUS),
        (sprites::PLAY_PRESSED, layout::PLAY),
        (sprites::PAUSE, layout::PAUSE),
        (sprites::STOP, layout::STOP),
        (sprites::NEXT, layout::NEXT),
        (sprites::EJECT, layout::EJECT),
        (sprites::SHUFFLE_ON, layout::SHUFFLE),
        (sprites::REPEAT_OFF, layout::REPEAT),
    ];
    for (sprite, area) in controls {
        blit(sprite, area.x, area.y);
    }
    image
}

fn main() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/skins/spotizgeg.wsz");
    let sheets: [(Sheet, Canvas); 15] = [
        (Sheet::Main, main_sheet()),
        (Sheet::CButtons, cbuttons_sheet()),
        (Sheet::TitleBar, titlebar_sheet()),
        (Sheet::ShufRep, shufrep_sheet()),
        (Sheet::PosBar, posbar_sheet()),
        (Sheet::Volume, volume_sheet()),
        (Sheet::Balance, balance_sheet()),
        (Sheet::MonoSter, monoster_sheet()),
        (Sheet::PlayPaus, playpaus_sheet()),
        (Sheet::Numbers, numbers_sheet(false)),
        (Sheet::NumsEx, numbers_sheet(true)),
        (Sheet::Text, text_sheet()),
        (Sheet::PlEdit, pledit_sheet()),
        (Sheet::EqMain, eqmain_sheet()),
        (Sheet::EqEx, eq_ex_sheet()),
    ];
    let mut files: Vec<(String, Vec<u8>)> = sheets
        .iter()
        .map(|(sheet, canvas)| (format!("{}.bmp", sheet.file_stem()), canvas.bmp()))
        .collect();
    files.push(("pledit.txt".to_string(), PLEDIT_TXT.as_bytes().to_vec()));
    files.push(("viscolor.txt".to_string(), viscolor_txt().into_bytes()));
    let entries: Vec<(&str, &[u8], bool)> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice(), true))
        .collect();
    let archive = zip::write(&entries);
    std::fs::write(&path, &archive).expect("the skin can be written");
    println!(
        "wrote {} files ({} bytes) to {}",
        entries.len(),
        archive.len(),
        path.display()
    );

    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--preview") {
        let path = PathBuf::from(args.next().expect("--preview takes a path"));
        let skin = Skin::from_archive("Spotizgeg", &archive).expect("the skin just written reads");
        let window = preview(&skin);
        let scaled = image::imageops::resize(
            &window,
            window.width() * 2,
            window.height() * 2,
            image::imageops::FilterType::Nearest,
        );
        scaled.save(&path).expect("the preview can be written");
        println!("wrote the preview to {}", path.display());
    }
}
