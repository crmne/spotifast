//! Cuts Spotifast Ribbon, the Windows Media Player skin the app ships, from
//! its generated art.
//!
//! The body is not drawn here. `assets/skins/ribbon/source/plate.png` and
//! `keys.png` are rendered images of the object, a white ceramic ribbon round
//! a floating smoked-glass core, on flat magenta. This takes them apart for
//! the skin: it keys out the magenta and scales down to a hard outline, since
//! a window has no soft edge; separates the glass from the ceramic; takes the
//! green out of the arch to get the light off and the light alone; and walks
//! along the ribbon to make the strip it sweeps round the core with. Only the
//! window glyphs and the slider parts are drawn, being a few pixels each.
//!
//! `cargo run --example ribbon_skin` regenerates
//! `assets/skins/spotifast-ribbon.wmz` from this file and from
//! `assets/skins/ribbon`, which holds the definition and script as well.

use std::collections::VecDeque;
use std::path::PathBuf;

use image::{Rgba, RgbaImage};
use spotifast::skin::zip;

const WIDTH: u32 = 240;
const HEIGHT: u32 = 420;
/// The square that shows the album art or the visualizer.
const PANE: (u32, u32, u32) = (62, 75, 126);
const PANE_ROUND: f32 = 9.0;
/// Frames of the ribbon sweeping round. The last is the whole ribbon.
const SWEEP_FRAMES: u32 = 14;
/// Where the ribbon starts from, at the pill's free end.
const SWEEP_FROM: (u32, u32) = (176, 338);
/// Where glass may be found, to tell it from the ceramic.
const GLASS_SEEDS: [(u32, u32); 2] = [(130, 240), (130, 395)];

type Color = [f32; 3];

/// A picture with its colours in 0 to 1 and which pixels are object.
struct Cut {
    width: u32,
    height: u32,
    colors: Vec<Color>,
    cover: Vec<f32>,
}

fn read(path: &str) -> image::RgbImage {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/skins/ribbon/source");
    image::open(root.join(path))
        .expect("the source art")
        .to_rgb8()
}

fn unit(pixel: &image::Rgb<u8>) -> Color {
    pixel.0.map(|channel| f32::from(channel) / 255.0)
}

fn is_chroma(color: Color) -> bool {
    color[0].min(color[2]) - color[1] > 0.3
}

/// Which pixels of the source are the object, less a thin rim, where its
/// edge has picked up the backdrop.
fn object_mask(source: &image::RgbImage) -> Vec<bool> {
    let (width, height) = source.dimensions();
    let mut keep: Vec<bool> = source
        .pixels()
        .map(|pixel| !is_chroma(unit(pixel)))
        .collect();
    for _ in 0..2 {
        let before = keep.clone();
        for y in 1..height - 1 {
            for x in 1..width - 1 {
                let at = (y * width + x) as usize;
                keep[at] = before[at]
                    && before[at - 1]
                    && before[at + 1]
                    && before[at - width as usize]
                    && before[at + width as usize];
            }
        }
    }
    keep
}

/// Averages a box of the source down to `width` by `height`, over the object
/// only, so that no backdrop bleeds into the edge.
fn shrink(
    source: &image::RgbImage,
    keep: &[bool],
    from: (u32, u32, u32, u32),
    width: u32,
    height: u32,
) -> Cut {
    let (x0, y0, box_width, box_height) = from;
    let mut cut = Cut {
        width,
        height,
        colors: vec![[0.0; 3]; (width * height) as usize],
        cover: vec![0.0; (width * height) as usize],
    };
    for y in 0..height {
        for x in 0..width {
            let left = x0 + x * box_width / width;
            let right = (x0 + (x + 1) * box_width / width).max(left + 1);
            let top = y0 + y * box_height / height;
            let bottom = (y0 + (y + 1) * box_height / height).max(top + 1);
            let (mut sum, mut seen, mut all) = ([0.0f32; 3], 0.0f32, 0.0f32);
            for sy in top..bottom {
                for sx in left..right {
                    all += 1.0;
                    if keep[(sy * source.width() + sx) as usize] {
                        let color = unit(source.get_pixel(sx, sy));
                        for channel in 0..3 {
                            sum[channel] += color[channel];
                        }
                        seen += 1.0;
                    }
                }
            }
            let at = (y * width + x) as usize;
            cut.cover[at] = seen / all;
            if seen > 0.0 {
                cut.colors[at] = sum.map(|channel| channel / seen);
            }
        }
    }
    cut
}

fn picture(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, y| {
        Rgba(pixel(x, y).map(|channel| (channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8))
    })
}

fn png(image: &RgbaImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("a png");
    out.into_inner()
}

/// How far a colour leans green past its other channels.
fn green_in(color: Color) -> f32 {
    color[1] - color[0].max(color[2])
}

/// The colour with the light out: the green eases away, with no step where
/// it ends, leaving the shade that was under it.
fn unlit(color: Color) -> Color {
    let share = (green_in(color) / 0.08).clamp(0.0, 1.0);
    let gray = color[0].max(color[2]);
    color.map(|channel| channel * (1.0 - share) + gray * share)
}

fn at(x: u32, y: u32) -> usize {
    (y * WIDTH + x) as usize
}

fn neighbours(x: u32, y: u32) -> impl Iterator<Item = (u32, u32)> {
    [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)]
        .into_iter()
        .filter_map(move |(dx, dy)| {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            (nx >= 0 && ny >= 0 && nx < WIDTH as i32 && ny < HEIGHT as i32)
                .then_some((nx as u32, ny as u32))
        })
}

/// The glass: dark pixels joined to the seeds.
fn glass_mask(plate: &Cut, solid: &[bool]) -> Vec<bool> {
    let dark = |x: u32, y: u32| {
        let color = plate.colors[at(x, y)];
        solid[at(x, y)] && color[0].max(color[1]).max(color[2]) < 0.34
    };
    let mut glass = vec![false; solid.len()];
    let mut queue: VecDeque<(u32, u32)> = GLASS_SEEDS.into_iter().collect();
    for (x, y) in GLASS_SEEDS {
        glass[at(x, y)] = true;
    }
    while let Some((x, y)) = queue.pop_front() {
        for (nx, ny) in neighbours(x, y) {
            if !glass[at(nx, ny)] && dark(nx, ny) {
                glass[at(nx, ny)] = true;
                queue.push_back((nx, ny));
            }
        }
    }
    smoothed(glass, solid)
}

/// Rounds a mask's edge: each pixel goes with most of those around it, so
/// that glass and ceramic part along a clean line.
fn smoothed(mut mask: Vec<bool>, solid: &[bool]) -> Vec<bool> {
    for _ in 0..3 {
        let before = mask.clone();
        for y in 2..HEIGHT - 2 {
            for x in 2..WIDTH - 2 {
                let around = (y - 2..=y + 2)
                    .flat_map(|ny| (x - 2..=x + 2).map(move |nx| (nx, ny)))
                    .filter(|(nx, ny)| before[at(*nx, *ny)])
                    .count();
                mask[at(x, y)] = around >= 13 && solid[at(x, y)];
            }
        }
    }
    mask
}

/// Steps along the ribbon from the pill, so that a front can run its length.
fn walk(ribbon: &[bool]) -> (Vec<u32>, u32) {
    let mut steps = vec![u32::MAX; ribbon.len()];
    let mut queue = VecDeque::from([SWEEP_FROM]);
    steps[at(SWEEP_FROM.0, SWEEP_FROM.1)] = 0;
    let mut longest = 0;
    while let Some((x, y)) = queue.pop_front() {
        let here = steps[at(x, y)];
        longest = longest.max(here);
        for (nx, ny) in neighbours(x, y) {
            if ribbon[at(nx, ny)] && steps[at(nx, ny)] == u32::MAX {
                steps[at(nx, ny)] = here + 1;
                queue.push_back((nx, ny));
            }
        }
    }
    (steps, longest)
}

/// Whether a pixel is inside the pane's rounded square, softly.
fn in_pane(x: u32, y: u32) -> f32 {
    let (left, top, side) = (PANE.0 as f32, PANE.1 as f32, PANE.2 as f32);
    let half = side / 2.0;
    let dx = (x as f32 + 0.5 - left - half).abs() - (half - PANE_ROUND);
    let dy = (y as f32 + 0.5 - top - half).abs() - (half - PANE_ROUND);
    let distance = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - PANE_ROUND;
    (0.5 - distance).clamp(0.0, 1.0)
}

/// One part of the key sheet, with its backdrop gone.
fn part(sheet: &image::RgbImage, keep: &[bool], middle: (u32, u32), half: u32, side: u32) -> Cut {
    shrink(
        sheet,
        keep,
        (middle.0 - half, middle.1 - half, half * 2, half * 2),
        side,
        side,
    )
}

fn with_alpha(cut: &Cut, color: impl Fn(Color) -> Color, alpha: f32) -> RgbaImage {
    picture(cut.width, cut.height, |x, y| {
        let index = (y * cut.width + x) as usize;
        let [r, g, b] = color(cut.colors[index]);
        [r, g, b, cut.cover[index] * alpha]
    })
}

/// A small glyph drawn from its distance, sampled four times a side.
fn glyph(side: u32, color: Color, shape: &dyn Fn(f32, f32) -> f32) -> RgbaImage {
    picture(side, side, |x, y| {
        let mut sum = 0.0;
        for row in 0..4 {
            for column in 0..4 {
                let px = x as f32 + (column as f32 + 0.5) / 4.0;
                let py = y as f32 + (row as f32 + 0.5) / 4.0;
                sum += f32::from(u8::from(shape(px, py) <= 0.0));
            }
        }
        [color[0], color[1], color[2], sum / 16.0]
    })
}

fn line(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let along = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
    (x - a.0 - dx * along).hypot(y - a.1 - dy * along)
}

fn outline(x: f32, y: f32, left: f32, top: f32, right: f32, bottom: f32) -> f32 {
    let (cx, cy) = ((left + right) / 2.0, (top + bottom) / 2.0);
    let qx = (x - cx).abs() - (right - left) / 2.0;
    let qy = (y - cy).abs() - (bottom - top) / 2.0;
    (qx.max(qy)).abs()
}

fn window_glyph(name: &str) -> impl Fn(f32, f32) -> f32 + use<'_> {
    move |x, y| match name {
        "min" => line(x, y, (2.5, 8.5), (8.5, 8.5)) - 0.7,
        "back" => outline(x, y, 2.0, 3.0, 9.0, 8.5) - 0.6,
        "close" => line(x, y, (2.5, 2.5), (8.5, 8.5)).min(line(x, y, (8.5, 2.5), (2.5, 8.5))) - 0.7,
        // The pane's two faces: a picture's frame with bars in it.
        _ => {
            outline(x, y, 1.5, 1.5, 10.5, 10.5)
                .min(line(x, y, (4.0, 8.5), (4.0, 6.5)))
                .min(line(x, y, (6.0, 8.5), (6.0, 4.0)))
                .min(line(x, y, (8.0, 8.5), (8.0, 5.5)))
                - 0.6
        }
    }
}

/// A slider's track or fill: a thin rounded line across its height.
fn track(width: u32, height: u32, color: Color) -> RgbaImage {
    let middle = height as f32 / 2.0;
    glyph(width.max(height), color, &|x, y| {
        line(x, y, (1.5, middle), (width as f32 - 1.5, middle)) - 1.2
    })
    .view_crop(width, height)
}

trait Crop {
    fn view_crop(&self, width: u32, height: u32) -> RgbaImage;
}

impl Crop for RgbaImage {
    fn view_crop(&self, width: u32, height: u32) -> RgbaImage {
        image::imageops::crop_imm(self, 0, 0, width, height).to_image()
    }
}

fn thumb(color: Color) -> RgbaImage {
    glyph(9, color, &|x, y| (x - 4.5).hypot(y - 4.5) - 4.0)
}

const WHITE: Color = [0.95, 0.957, 0.965];
const SOFT: Color = [0.66, 0.69, 0.74];
const GREEN: Color = [0.118, 0.843, 0.376];
const BRIGHT: Color = [0.337, 0.882, 0.533];

fn body(files: &mut Vec<(String, Vec<u8>)>) {
    let source = read("plate.png");
    let keep = object_mask(&source);
    let plate = shrink(
        &source,
        &keep,
        (0, 0, source.width(), source.height()),
        WIDTH,
        HEIGHT,
    );
    // The outline is hard: a pixel is the object or the desktop.
    let solid: Vec<bool> = plate.cover.iter().map(|cover| *cover >= 0.5).collect();
    let glass = glass_mask(&plate, &solid);
    let ribbon: Vec<bool> = solid
        .iter()
        .zip(&glass)
        .map(|(solid, glass)| *solid && !glass)
        .collect();
    let (steps, longest) = walk(&ribbon);

    let core = picture(WIDTH, HEIGHT, |x, y| {
        let index = at(x, y);
        let [r, g, b] = unlit(plate.colors[index]);
        let shown = if glass[index] {
            1.0 - in_pane(x, y)
        } else {
            0.0
        };
        [r, g, b, shown]
    });
    let shown_ribbon = |x: u32, y: u32, reach: u32| {
        let index = at(x, y);
        let [r, g, b] = unlit(plate.colors[index]);
        // What the walk could not reach, cut off from the rest, comes last.
        let on = ribbon[index] && (steps[index] <= reach || reach >= longest);
        [r, g, b, f32::from(u8::from(on))]
    };
    let mut sweep = RgbaImage::new(WIDTH, HEIGHT * SWEEP_FRAMES);
    for frame in 0..SWEEP_FRAMES {
        let reach = longest * (frame + 1) / SWEEP_FRAMES;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let pixel = shown_ribbon(x, y, reach);
                sweep.put_pixel(
                    x,
                    y + frame * HEIGHT,
                    Rgba(pixel.map(|channel| (channel * 255.0 + 0.5) as u8)),
                );
            }
        }
    }
    let whole = picture(WIDTH, HEIGHT, |x, y| shown_ribbon(x, y, longest));
    let light = picture(WIDTH, HEIGHT, |x, y| {
        let index = at(x, y);
        let [r, g, b] = plate.colors[index];
        let strength = if solid[index] {
            (green_in([r, g, b]) * 5.0).min(1.0)
        } else {
            0.0
        };
        [r, g, b, strength]
    });
    // Empty glass, to slide over the pane. Its colours are the plate's.
    let (left, top, side) = PANE;
    let veil = picture(side, side, |x, y| {
        let [r, g, b] = unlit(plate.colors[at(x + left, y + top)]);
        [r, g, b, 1.0]
    });
    files.push(("core.png".into(), png(&core)));
    files.push(("sweep.png".into(), png(&sweep)));
    files.push(("ribbon.png".into(), png(&whole)));
    files.push(("light.png".into(), png(&light)));
    files.push(("veil.png".into(), png(&veil)));
}

fn keys(files: &mut Vec<(String, Vec<u8>)>) {
    let sheet = read("keys.png");
    let keep = object_mask(&sheet);
    let disc = 30;
    let columns = [251, 626, 1002];
    let (top, middle) = (268, 644);
    let flat = |color: Color| color;
    let gray = |color: Color| {
        let level = (color[0] + color[1] + color[2]) / 3.0;
        [level; 3]
    };
    for (name, column) in [("pause", columns[0]), ("play", columns[1])] {
        let normal = part(&sheet, &keep, (column, top), 156, disc);
        let hover = part(&sheet, &keep, (column, middle), 156, disc);
        let pressed = part(&sheet, &keep, (columns[2], middle), 156, disc);
        // The pressed button is blank in the sheet: it takes the symbol from
        // the normal one.
        let down = picture(disc, disc, |x, y| {
            let index = (y * disc + x) as usize;
            let symbol = normal.colors[index];
            let base = pressed.colors[index];
            let share = (green_in(symbol) * 3.0).clamp(0.0, 1.0);
            let mixed = [0, 1, 2].map(|at| base[at] * (1.0 - share) + symbol[at] * share);
            [mixed[0], mixed[1], mixed[2], pressed.cover[index]]
        });
        files.push((format!("{name}.png"), png(&with_alpha(&normal, flat, 1.0))));
        files.push((
            format!("{name}_hover.png"),
            png(&with_alpha(&hover, flat, 1.0)),
        ));
        files.push((format!("{name}_down.png"), png(&down)));
        files.push((
            format!("{name}_off.png"),
            png(&with_alpha(&normal, gray, 1.0)),
        ));
    }
    for (name, column) in [("prev", columns[0]), ("next", columns[1])] {
        let shape = part(&sheet, &keep, (column, 1004), 62, 18);
        let ink = |color: Color| color;
        files.push((format!("{name}.png"), png(&with_alpha(&shape, ink, 1.0))));
        files.push((
            format!("{name}_hover.png"),
            png(&with_alpha(&shape, |_| GREEN, 1.0)),
        ));
        files.push((
            format!("{name}_down.png"),
            png(&with_alpha(&shape, |_| [0.02; 3], 1.0)),
        ));
        files.push((
            format!("{name}_off.png"),
            png(&with_alpha(&shape, ink, 0.35)),
        ));
    }
    let speaker = part(&sheet, &keep, (1002, 1004), 78, 16);
    files.push(("speaker.png".into(), png(&with_alpha(&speaker, flat, 1.0))));
    files.push((
        "speaker_hover.png".into(),
        png(&with_alpha(&speaker, |_| BRIGHT, 1.0)),
    ));
    files.push((
        "speaker_down.png".into(),
        png(&with_alpha(&speaker, |_| [0.43, 0.47, 0.52], 1.0)),
    ));
}

fn small_parts(files: &mut Vec<(String, Vec<u8>)>) {
    for name in ["min", "back", "close", "pane"] {
        let side = if name == "pane" { 12 } else { 11 };
        for (suffix, color) in [("", SOFT), ("_hover", WHITE), ("_down", GREEN)] {
            files.push((
                format!("w_{name}{suffix}.png"),
                png(&glyph(side, color, &window_glyph(name))),
            ));
        }
    }
    let dim = [0.26, 0.285, 0.32];
    for (name, width) in [("seek", 127), ("volume", 82)] {
        files.push((format!("{name}_back.png"), png(&track(width, 9, dim))));
        files.push((format!("{name}_fill.png"), png(&track(width, 9, BRIGHT))));
    }
    files.push(("thumb.png".into(), png(&thumb(WHITE))));
    files.push(("thumb_hot.png".into(), png(&thumb(BRIGHT))));
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/skins");
    let path = std::env::args()
        .nth(1)
        .map_or(root.join("spotifast-ribbon.wmz"), PathBuf::from);
    let mut files = Vec::new();
    body(&mut files);
    keys(&mut files);
    small_parts(&mut files);
    for name in ["ribbon.wms", "ribbon.js", "viscolor.txt"] {
        let bytes = std::fs::read(root.join("ribbon").join(name)).expect("the skin's source");
        files.push((name.to_owned(), bytes));
    }
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
}
