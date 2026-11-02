//! Cuts the Windows Media Player skins the app ships, tall and wide, from
//! their generated art.
//!
//! The bodies are not drawn here. Each skin's `source/plate.png` and `keys.png`
//! are rendered images of the object, a white ceramic ribbon round a floating
//! smoked-glass core, on flat magenta. This takes them apart for the skin: it
//! keys out the magenta and scales down to a hard outline, since a window has
//! no soft edge; separates the glass from the ceramic; takes the green out of
//! the arch to get the light off and the light alone; and makes the opening's
//! frames from the plate, with a mask for each: a thread of light along the
//! outline, then a wipe that brings on ceramic and glass together. Only the
//! small glyphs and the slider parts are drawn, being a few pixels each.
//!
//! `cargo run --example ribbon_skin` regenerates
//! `assets/skins/spotifast-ribbon.wmz` and `spotifast-wide.wmz` from this file
//! and from `assets/skins/ribbon` and `assets/skins/wide`, which hold each
//! skin's definition and art, the first the script they share as well. A
//! definition's `@bands` line is filled in here.

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::PathBuf;

use image::{Rgba, RgbaImage};
use spotifast::skin::zip;

/// How one skin differs from the other.
struct Design {
    /// Its folder under `assets/skins`, and its definition's name there.
    folder: &'static str,
    archive: &'static str,
    width: u32,
    height: u32,
    /// The square that shows the album art: left, top and side.
    pane: (u32, u32, u32),
    /// The box of the glass the visualizer fills.
    visual: (u32, u32, u32, u32),
    /// Where glass may be found, to tell it from the ceramic. The first is
    /// in the part the visualizer fills.
    seeds: [(u32, u32); 2],
    /// Where the light starts: the outside of the arch.
    arch: (u32, u32),
    /// How dark the ground is behind the words over the visualizer, at a
    /// place in the window.
    scrim: fn(f32, f32) -> f32,
    /// The key sheet: its columns, the rows of buttons and glyphs, and how
    /// far a button reaches from its middle.
    columns: [u32; 3],
    rows: [u32; 3],
    reach: u32,
    seek: u32,
    volume: u32,
    /// Equalizer bands: the first's left, the step, the top and the height.
    bands: (u32, u32, u32, u32),
}

const TALL: Design = Design {
    folder: "ribbon",
    archive: "spotifast-ribbon.wmz",
    width: 240,
    height: 420,
    pane: (62, 75, 126),
    visual: (34, 46, 176, 276),
    seeds: [(130, 240), (130, 395)],
    arch: (84, 22),
    scrim: |_, y| {
        let low = ((y - 214.0) / 50.0).clamp(0.0, 1.0);
        let high = ((86.0 - y) / 34.0).clamp(0.0, 1.0);
        (low * low * (3.0 - 2.0 * low) * 0.66).max(high * 0.5)
    },
    columns: [251, 626, 1002],
    rows: [268, 644, 1004],
    reach: 156,
    seek: 127,
    volume: 82,
    bands: (78, 11, 102, 150),
};

const WIDE: Design = Design {
    folder: "wide",
    archive: "spotifast-wide.wmz",
    width: 460,
    height: 180,
    pane: (20, 44, 104),
    visual: (8, 28, 424, 140),
    seeds: [(240, 80), (350, 150)],
    arch: (300, 8),
    scrim: |x, y| {
        // The words have the right of the art to themselves, and the window's
        // buttons sit above them.
        let words = ((x - 70.0) / 150.0).clamp(0.0, 1.0);
        let top = ((70.0 - y) / 30.0).clamp(0.0, 1.0);
        (words * words * (3.0 - 2.0 * words) * 0.56).max(top * 0.36 * words)
    },
    columns: [249, 624, 1000],
    rows: [258, 664, 1026],
    reach: 165,
    seek: 150,
    volume: 90,
    bands: (140, 18, 42, 78),
};

thread_local! {
    /// The skin being cut.
    static DESIGN: Cell<&'static Design> = const { Cell::new(&TALL) };
}

fn design() -> &'static Design {
    DESIGN.with(Cell::get)
}

const PANE_ROUND: f32 = 9.0;
/// How wide the rim is that softens the visualizer's cut edge.
const RIM: u32 = 3;
/// The colour keyed out of the shape, in the definition's `clippingColor`.
const OUTSIDE: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
/// Frames of the thread of light going round, the first of them empty.
const TRACE_FRAMES: u32 = 14;
/// Frames of the wipe, the first empty and the last the whole object.
const WIPE_FRAMES: u32 = 8;
/// How far from the outline the thread's glow reaches.
const GLOW: i32 = 5;

type Color = [f32; 3];

/// A picture with its colours in 0 to 1 and which pixels are object.
struct Cut {
    width: u32,
    height: u32,
    colors: Vec<Color>,
    cover: Vec<f32>,
}

fn read(path: &str) -> image::RgbImage {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/skins")
        .join(design().folder)
        .join("source");
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
    (y * design().width + x) as usize
}

fn neighbours(x: u32, y: u32) -> impl Iterator<Item = (u32, u32)> {
    [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)]
        .into_iter()
        .filter_map(move |(dx, dy)| {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            (nx >= 0 && ny >= 0 && nx < design().width as i32 && ny < design().height as i32)
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
    let mut queue: VecDeque<(u32, u32)> = design().seeds.into_iter().collect();
    for (x, y) in design().seeds {
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
        for y in 2..design().height - 2 {
            for x in 2..design().width - 2 {
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

/// The object's outline as one line, from the arch and round.
fn outline_of(solid: &[bool]) -> Vec<(u32, u32)> {
    let mut edge: Vec<(u32, u32)> = Vec::new();
    for y in 0..design().height {
        for x in 0..design().width {
            let open = x == 0
                || y == 0
                || x == design().width - 1
                || y == design().height - 1
                || neighbours(x, y).any(|(nx, ny)| !solid[at(nx, ny)]);
            if solid[at(x, y)] && open {
                edge.push((x, y));
            }
        }
    }
    let far = |a: (u32, u32), b: (u32, u32)| {
        let (dx, dy) = (a.0 as i64 - b.0 as i64, a.1 as i64 - b.1 as i64);
        dx * dx + dy * dy
    };
    let start = (0..edge.len())
        .min_by_key(|index| far(edge[*index], design().arch))
        .expect("an outline");
    let mut here = edge.swap_remove(start);
    let mut line = vec![here];
    while let Some(next) = (0..edge.len()).min_by_key(|index| far(edge[*index], here)) {
        if far(edge[next], here) > 36 {
            break;
        }
        here = edge.swap_remove(next);
        line.push(here);
    }
    line
}

/// For each pixel near the outline, how far along it and how far from it.
fn near_outline(line: &[(u32, u32)]) -> Vec<(usize, f32)> {
    let mut near = vec![(usize::MAX, f32::MAX); (design().width * design().height) as usize];
    for (index, (x, y)) in line.iter().enumerate() {
        for dy in -GLOW..=GLOW {
            for dx in -GLOW..=GLOW {
                let (px, py) = (*x as i32 + dx, *y as i32 + dy);
                if px < 0 || py < 0 || px >= design().width as i32 || py >= design().height as i32 {
                    continue;
                }
                let distance = (dx as f32).hypot(dy as f32);
                let slot = &mut near[at(px as u32, py as u32)];
                if distance < slot.1 {
                    *slot = (index, distance);
                }
            }
        }
    }
    near
}

fn stack(frames: u32, frame: impl Fn(u32, u32, u32) -> [f32; 4]) -> RgbaImage {
    let mut strip = RgbaImage::new(design().width, design().height * frames);
    for at_frame in 0..frames {
        for y in 0..design().height {
            for x in 0..design().width {
                let pixel = frame(at_frame, x, y);
                strip.put_pixel(
                    x,
                    y + at_frame * design().height,
                    Rgba(pixel.map(|channel| (channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)),
                );
            }
        }
    }
    strip
}

/// Whether a pixel is inside the pane's rounded square, softly.
fn in_pane(x: u32, y: u32) -> f32 {
    let (left, top, side) = (
        design().pane.0 as f32,
        design().pane.1 as f32,
        design().pane.2 as f32,
    );
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

/// An equalizer band's track, upright, with a mark at no change.
fn eq_track(color: Color) -> RgbaImage {
    let height = design().bands.3;
    picture(9, height, |x, y| {
        let rail = x == 4 && (2..height - 2).contains(&y);
        let mark = y == height / 2 && (2..7).contains(&x);
        let shade = if mark { 2.0 } else { 1.0 };
        [
            color[0] * shade,
            color[1] * shade,
            color[2] * shade,
            f32::from(u8::from(rail || mark)),
        ]
    })
}

/// Three dots, one to a page, the page that is up in green.
fn page_dots(page: u32) -> RgbaImage {
    let lit = glyph(24, GREEN, &|x, y| {
        (x - (4.0 + page as f32 * 8.0)).hypot(y - 5.5) - 2.2
    });
    let rest = glyph(24, SOFT, &|x, y| {
        (0..3)
            .filter(|dot| *dot != page)
            .map(|dot| (x - (4.0 + dot as f32 * 8.0)).hypot(y - 5.5) - 1.8)
            .fold(f32::MAX, f32::min)
    });
    let mut out = rest;
    image::imageops::overlay(&mut out, &lit, 0, 0);
    out.view_crop(24, 11)
}

fn switch(on: bool) -> RgbaImage {
    let body = glyph(20, if on { GREEN } else { [0.26, 0.285, 0.32] }, &|x, y| {
        line(x, y, (5.0, 5.0), (15.0, 5.0)) - 4.5
    });
    let knob_at = if on { 15.0 } else { 5.0 };
    let knob = glyph(20, WHITE, &|x, y| (x - knob_at).hypot(y - 5.0) - 3.2);
    let mut out = body;
    image::imageops::overlay(&mut out, &knob, 0, 0);
    out.view_crop(20, 10)
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
        design().width,
        design().height,
    );
    // The outline is hard: a pixel is the object or the desktop.
    let solid: Vec<bool> = plate.cover.iter().map(|cover| *cover >= 0.5).collect();
    let glass = glass_mask(&plate, &solid);
    let ribbon: Vec<bool> = solid
        .iter()
        .zip(&glass)
        .map(|(solid, glass)| *solid && !glass)
        .collect();
    visual_parts(files, &plate, &glass);
    let line = outline_of(&solid);
    let near = near_outline(&line);
    let reach_of = |x: u32, y: u32| {
        (x as f32 - design().arch.0 as f32).hypot(y as f32 - design().arch.1 as f32)
    };
    let furthest = (0..design().height)
        .flat_map(|y| (0..design().width).map(move |x| (x, y)))
        .filter(|(x, y)| solid[at(*x, *y)])
        .map(|(x, y)| reach_of(x, y))
        .fold(0.0, f32::max);

    let trace = stack(TRACE_FRAMES, |frame, x, y| {
        let head = line.len() * frame as usize / (TRACE_FRAMES as usize - 1);
        let (index, distance) = near[at(x, y)];
        if index >= head {
            return [0.0; 4];
        }
        let thread = (1.6 - distance).clamp(0.0, 1.0);
        let glow = 0.42 * (1.0 - distance / (GLOW as f32 + 0.5)).max(0.0).powi(2);
        let hot = (1.0 - (head - index) as f32 / 40.0).max(0.0);
        let color = [0, 1, 2].map(|at| BRIGHT[at] * (1.0 - hot) + WHITE[at] * hot);
        [color[0], color[1], color[2], thread.max(glow)]
    });
    // The whole plate through one mask: ceramic and glass come on together,
    // behind a bright edge that carries on from the thread.
    let wipe = stack(WIPE_FRAMES, |frame, x, y| {
        let last = frame == WIPE_FRAMES - 1;
        let reach = furthest * frame as f32 / (WIPE_FRAMES - 1) as f32 + 1.0;
        let distance = reach_of(x, y);
        if !solid[at(x, y)] || frame == 0 || distance > reach {
            return [0.0; 4];
        }
        let [r, g, b] = plate.colors[at(x, y)];
        let edge = if last {
            0.0
        } else {
            (1.0 - (reach - distance) / 7.0).max(0.0)
        };
        [
            r * (1.0 - edge) + BRIGHT[0] * edge,
            g * (1.0 - edge) + BRIGHT[1] * edge,
            b * (1.0 - edge) + BRIGHT[2] * edge,
            1.0,
        ]
    });
    let core = picture(design().width, design().height, |x, y| {
        let index = at(x, y);
        let [r, g, b] = unlit(plate.colors[index]);
        let shown = if glass[index] {
            1.0 - in_pane(x, y)
        } else {
            0.0
        };
        [r, g, b, shown]
    });
    let whole = picture(design().width, design().height, |x, y| {
        let index = at(x, y);
        let [r, g, b] = unlit(plate.colors[index]);
        [r, g, b, f32::from(u8::from(ribbon[index]))]
    });
    let light = picture(design().width, design().height, |x, y| {
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
    let (left, top, side) = design().pane;
    let veil = picture(side, side, |x, y| {
        let [r, g, b] = unlit(plate.colors[at(x + left, y + top)]);
        [r, g, b, 1.0]
    });
    files.push(("core.png".into(), png(&core)));
    files.push(("trace.png".into(), png(&trace)));
    files.push(("wipe.png".into(), png(&wipe)));
    files.push(("ribbon.png".into(), png(&whole)));
    files.push(("light.png".into(), png(&light)));
    files.push(("veil.png".into(), png(&veil)));
}

/// What the full-glass visualizer needs: its shape, a rim of the plate's own
/// glass to lie over the cut edge, and a soft dark ground for what is drawn
/// over it, since some presets go white.
fn visual_parts(files: &mut Vec<(String, Vec<u8>)>, plate: &Cut, glass: &[bool]) {
    let (left, top, width, height) = design().visual;
    // The upper glass alone: the foot lies beyond the pill.
    let mut upper = vec![false; glass.len()];
    let mut queue = VecDeque::from([design().seeds[0]]);
    upper[at(design().seeds[0].0, design().seeds[0].1)] = true;
    while let Some((x, y)) = queue.pop_front() {
        for (nx, ny) in neighbours(x, y) {
            if glass[at(nx, ny)] && !upper[at(nx, ny)] {
                upper[at(nx, ny)] = true;
                queue.push_back((nx, ny));
            }
        }
    }
    let inside = |x: u32, y: u32| upper[at(x + left, y + top)];
    for y in 0..design().height {
        for x in 0..design().width {
            let beyond = x < left || y < top || x >= left + width || y >= top + height;
            assert!(!(upper[at(x, y)] && beyond), "the glass is outside the box");
        }
    }
    let mask = picture(width, height, |x, y| {
        if inside(x, y) {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            OUTSIDE
        }
    });
    let depth = |x: u32, y: u32| {
        let reach = RIM as i32 + 1;
        let mut nearest = reach as f32;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                let out = nx < 0
                    || ny < 0
                    || nx >= width as i32
                    || ny >= height as i32
                    || !inside(nx as u32, ny as u32);
                if out {
                    nearest = nearest.min((dx as f32).hypot(dy as f32));
                }
            }
        }
        nearest
    };
    let rim = picture(width, height, |x, y| {
        if !inside(x, y) {
            return [0.0; 4];
        }
        let [r, g, b] = unlit(plate.colors[at(x + left, y + top)]);
        let share = 1.0 - (depth(x, y) - 1.0) / RIM as f32;
        [r, g, b, share.clamp(0.0, 1.0)]
    });
    let scrim = picture(width, height, |x, y| {
        if !inside(x, y) {
            return [0.0; 4];
        }
        let ground = (design().scrim)((x + left) as f32, (y + top) as f32);
        [0.03, 0.035, 0.04, ground]
    });
    files.push(("glass_mask.png".into(), png(&mask)));
    files.push(("glass_rim.png".into(), png(&rim)));
    files.push(("scrim.png".into(), png(&scrim)));
}

fn keys(files: &mut Vec<(String, Vec<u8>)>) {
    let sheet = read("keys.png");
    let keep = object_mask(&sheet);
    let disc = 30;
    let columns = design().columns;
    let [top, middle, glyphs] = design().rows;
    let reach = design().reach;
    let flat = |color: Color| color;
    let gray = |color: Color| {
        let level = (color[0] + color[1] + color[2]) / 3.0;
        [level; 3]
    };
    for (name, column) in [("pause", columns[0]), ("play", columns[1])] {
        let normal = part(&sheet, &keep, (column, top), reach, disc);
        let hover = part(&sheet, &keep, (column, middle), reach, disc);
        let pressed = part(&sheet, &keep, (columns[2], middle), reach, disc);
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
        let shape = part(&sheet, &keep, (column, glyphs), 62, 18);
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
    let speaker = part(&sheet, &keep, (columns[2], glyphs), 78, 16);
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
    for (name, width) in [("seek", design().seek), ("volume", design().volume)] {
        files.push((format!("{name}_back.png"), png(&track(width, 9, dim))));
        files.push((format!("{name}_fill.png"), png(&track(width, 9, BRIGHT))));
    }
    files.push(("eq_back.png".into(), png(&eq_track(dim))));
    for page in 0..3 {
        files.push((format!("page{page}.png"), png(&page_dots(page))));
    }
    files.push(("switch_off.png".into(), png(&switch(false))));
    files.push(("switch_on.png".into(), png(&switch(true))));
    files.push(("thumb.png".into(), png(&thumb(WHITE))));
    files.push(("thumb_hot.png".into(), png(&thumb(BRIGHT))));
}

fn cut(skin: &'static Design) {
    DESIGN.with(|design| design.set(skin));
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/skins");
    let mut files = Vec::new();
    body(&mut files);
    keys(&mut files);
    small_parts(&mut files);
    let own = root.join(skin.folder);
    let definition = format!("{}.wms", skin.folder);
    let shared = root.join("ribbon");
    let sources = [
        (definition.as_str(), &own),
        ("shape.js", &own),
        ("ribbon.js", &shared),
        ("viscolor.txt", &shared),
    ];
    for (name, folder) in sources {
        let bytes = std::fs::read(folder.join(name)).expect("the skin's source");
        files.push((name.to_owned(), bytes));
    }
    let (first, step, top, height) = skin.bands;
    let band = |index: u32| {
        format!(
            "        <SLIDER id=\"band{number}\" left=\"{left}\" top=\"{top}\" width=\"9\" \
             height=\"{height}\" direction=\"vertical\" min=\"-12\" max=\"12\" \
             backgroundImage=\"eq_back.png\" thumbImage=\"thumb.png\" \
             thumbHoverImage=\"thumb_hot.png\" thumbDownImage=\"thumb_hot.png\" \
             value=\"wmpprop:eq.gainLevel{number}\" \
             value_onChange=\"eq.gainLevel{number} = value;\" cursor=\"hand\"/>\n",
            number = index + 1,
            left = first + index * step,
        )
    };
    let bands: String = (0..10).map(band).collect();
    for (name, bytes) in &mut files {
        if *name == definition {
            let text = String::from_utf8_lossy(bytes).replace("<!-- @bands -->\n", &bands);
            *bytes = text.into_bytes();
        }
    }
    let entries: Vec<(&str, &[u8], bool)> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice(), true))
        .collect();
    let archive = zip::write(&entries);
    let path = root.join(skin.archive);
    std::fs::write(&path, &archive).expect("the skin can be written");
    println!(
        "wrote {} files ({} bytes) to {}",
        entries.len(),
        archive.len(),
        path.display()
    );
}

fn main() {
    cut(&TALL);
    cut(&WIDE);
}
