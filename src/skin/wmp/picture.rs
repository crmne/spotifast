//! A skin's pictures.
//!
//! A skin names hundreds of files, some of them animated with thousands of
//! frames between them, and scripts swap them as they run. They are decoded
//! on a worker thread as they are asked for and kept while memory allows, so
//! nothing here waits on the window's thread: a picture not yet ready simply
//! is not drawn until it is.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::sync::mpsc::{Receiver, Sender, channel};

use image::AnimationDecoder;

use super::super::{Bitmap, MAX_BITMAP_SIDE, Rgb};
use super::Box2;

/// What one skin may keep decoded, in bytes.
const MAX_DECODED: usize = 256 * 1024 * 1024;
/// The most bytes of frames one animated picture may decode to; the frames
/// after that are left out, so a crafted file ends early rather than fills
/// memory.
const MAX_ANIMATION: usize = 128 * 1024 * 1024;

/// A picture as an element wants it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ImageKey {
    /// Lower-case file name, without its folders.
    pub file: String,
    /// Colours that are see-through in it.
    pub clear: Vec<Rgb>,
    /// The picture's own top-left pixel is see-through as well: the colour
    /// `auto`.
    pub clear_corner: bool,
    /// Keep only the pixels where this mapping picture has this colour: one
    /// button of a button group.
    pub mask: Option<(String, Rgb)>,
}

pub struct Frame {
    pub bitmap: Bitmap,
    /// How long this frame shows, in milliseconds; 0 for a still picture.
    pub delay_ms: u32,
}

pub struct Picture {
    pub frames: Vec<Frame>,
    /// Where the frames' corner sits in the file's picture: masked pictures
    /// keep only the mask's box.
    pub origin: (i32, i32),
    /// The size of the whole picture, masked or not.
    pub size: (u32, u32),
    total_ms: u64,
    /// Times an animation plays; `None` for ever.
    loops: Option<u32>,
    runs: OnceLock<Arc<Vec<Box2>>>,
}

impl Picture {
    fn new(frames: Vec<Frame>, origin: (i32, i32), size: (u32, u32)) -> Self {
        let total_ms = frames.iter().map(|frame| u64::from(frame.delay_ms)).sum();
        Self {
            frames,
            origin,
            size,
            total_ms,
            loops: None,
            runs: OnceLock::new(),
        }
    }

    /// The first frame's solid part as rectangles, for clipping to it: runs
    /// of solid pixels on each row, with equal rows joined.
    pub fn runs(&self) -> Arc<Vec<Box2>> {
        self.runs
            .get_or_init(|| {
                let Some(frame) = self.frames.first() else {
                    return Arc::new(Vec::new());
                };
                Arc::new(solid_runs(&frame.bitmap, self.origin))
            })
            .clone()
    }

    /// Which frame shows `elapsed_ms` into a loop of the picture, and in how
    /// many milliseconds it gives way to the next.
    pub fn frame_at(&self, elapsed_ms: u64) -> (usize, Option<u64>) {
        if self.frames.len() < 2 || self.total_ms == 0 {
            return (0, None);
        }
        if let Some(loops) = self.loops
            && elapsed_ms >= self.total_ms.saturating_mul(u64::from(loops))
        {
            // Played out: some skins end on a see-through frame on purpose.
            return (self.frames.len() - 1, None);
        }
        let mut left = elapsed_ms % self.total_ms;
        for (index, frame) in self.frames.iter().enumerate() {
            let delay = u64::from(frame.delay_ms);
            if left < delay {
                return (index, Some(delay - left));
            }
            left -= delay;
        }
        (0, Some(1))
    }

    fn bytes(&self) -> usize {
        self.frames.iter().map(|frame| frame.bitmap.rgba.len()).sum()
    }
}

fn solid_runs(bitmap: &Bitmap, origin: (i32, i32)) -> Vec<Box2> {
    let mut boxes: Vec<Box2> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for y in 0..bitmap.height {
        let mut row: Vec<(i32, i32)> = Vec::new();
        let mut start = None;
        for x in 0..=bitmap.width {
            let solid = bitmap.pixel(x, y).is_some_and(|pixel| pixel[3] > 0);
            match (solid, start) {
                (true, None) => start = Some(x as i32),
                (false, Some(from)) => {
                    row.push((from, x as i32 - from));
                    start = None;
                }
                _ => {}
            }
        }
        let top = y as i32 + origin.1;
        let mut still_open = Vec::with_capacity(row.len());
        for (x, width) in row {
            let left = x + origin.0;
            let joined = open.iter().copied().find(|at| {
                let run: &Box2 = &boxes[*at];
                run.x == left && run.width == width
            });
            match joined {
                Some(at) => {
                    boxes[at].height += 1;
                    still_open.push(at);
                }
                None => {
                    boxes.push(Box2 {
                        x: left,
                        y: top,
                        width,
                        height: 1,
                    });
                    still_open.push(boxes.len() - 1);
                }
            }
        }
        open = still_open;
    }
    boxes
}

/// How many times a GIF plays: its Netscape block's count, for ever at 0;
/// once with no block.
fn gif_loops(bytes: &[u8]) -> Option<u32> {
    let marker = b"NETSCAPE2.0";
    let Some(at) = bytes.windows(marker.len()).position(|part| part == marker) else {
        return Some(1);
    };
    let block = bytes.get(at + marker.len()..at + marker.len() + 4)?;
    let count = u16::from_le_bytes([block[2], block[3]]);
    (count != 0).then_some(u32::from(count) + 1)
}

/// Decodes a file, with every frame if it is an animated GIF.
fn frames_of(bytes: &[u8]) -> Option<Vec<Frame>> {
    if bytes.starts_with(b"GIF8") {
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
        let (width, height) = image::ImageDecoder::dimensions(&decoder);
        if width > MAX_BITMAP_SIDE || height > MAX_BITMAP_SIDE {
            return None;
        }
        let mut frames = Vec::new();
        let mut total = 0usize;
        for frame in decoder.into_frames() {
            let Ok(frame) = frame else {
                break;
            };
            let (numerator, denominator) = frame.delay().numer_denom_ms();
            // Browsers and Windows alike slow a frame given next to no time.
            let delay_ms = (numerator / denominator.max(1)).max(20);
            let buffer = frame.into_buffer();
            total = total.saturating_add(buffer.as_raw().len());
            if total > MAX_ANIMATION {
                break;
            }
            frames.push(Frame {
                bitmap: Bitmap {
                    width: buffer.width(),
                    height: buffer.height(),
                    rgba: buffer.into_raw(),
                },
                delay_ms,
            });
        }
        if !frames.is_empty() {
            return Some(frames);
        }
    }
    Some(vec![Frame {
        bitmap: Bitmap::decode(bytes)?,
        delay_ms: 0,
    }])
}

/// The box of the pixels of `color` in a mapping picture.
pub fn region_of(bitmap: &Bitmap, color: Rgb) -> Option<(i32, i32, i32, i32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, -1, -1);
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            if bitmap.pixel(x, y).is_some_and(|p| p[..3] == color) {
                x0 = x0.min(x as i32);
                y0 = y0.min(y as i32);
                x1 = x1.max(x as i32);
                y1 = y1.max(y as i32);
            }
        }
    }
    (x1 >= 0).then_some((x0, y0, x1 - x0 + 1, y1 - y0 + 1))
}

/// Reads a picture as `key` wants it.
pub fn decode(files: &HashMap<String, Vec<u8>>, key: &ImageKey) -> Option<Picture> {
    let bytes = files.get(&key.file)?;
    let loops = if bytes.starts_with(b"GIF8") {
        gif_loops(bytes)
    } else {
        None
    };
    let mut picture = decode_frames(files, bytes, key)?;
    picture.loops = loops;
    Some(picture)
}

fn decode_frames(
    files: &HashMap<String, Vec<u8>>,
    bytes: &[u8],
    key: &ImageKey,
) -> Option<Picture> {
    let mut frames = frames_of(bytes)?;
    let corner: Option<Rgb> = key.clear_corner.then(|| {
        frames
            .first()
            .and_then(|frame| frame.bitmap.pixel(0, 0))
            .map_or([0; 3], |pixel| [pixel[0], pixel[1], pixel[2]])
    });
    let clear: Vec<Rgb> = key.clear.iter().copied().chain(corner).collect();
    for frame in &mut frames {
        if !clear.is_empty() {
            for pixel in frame.bitmap.rgba.as_chunks_mut::<4>().0 {
                if clear.iter().any(|clear| pixel[..3] == *clear) {
                    *pixel = [0, 0, 0, 0];
                }
            }
        }
    }
    let first = frames.first()?;
    let size = (first.bitmap.width, first.bitmap.height);
    let Some((map_file, color)) = &key.mask else {
        return Some(Picture::new(frames, (0, 0), size));
    };
    let map = Bitmap::decode(files.get(map_file)?)?;
    let (x, y, width, height) = region_of(&map, *color)?;
    let area = super::super::Sprite {
        sheet: super::super::Sheet::Main,
        x: x as u32,
        y: y as u32,
        width: width as u32,
        height: height as u32,
    };
    let mut kept = Vec::with_capacity(frames.len());
    for frame in frames {
        let delay_ms = frame.delay_ms;
        let Some(mut bitmap) = frame.bitmap.crop(area) else {
            continue;
        };
        let (kept_width, kept_height) = (bitmap.width, bitmap.height);
        for row in 0..kept_height {
            for column in 0..kept_width {
                let on = map
                    .pixel(area.x + column, area.y + row)
                    .is_some_and(|pixel| pixel[..3] == *color);
                if !on {
                    let at = 4 * (row * kept_width + column) as usize;
                    bitmap.rgba[at..at + 4].fill(0);
                }
            }
        }
        kept.push(Frame { bitmap, delay_ms });
    }
    (!kept.is_empty()).then(|| Picture::new(kept, (x, y), size))
}

type Decoded = (ImageKey, Option<Arc<Picture>>);

/// The pictures on hand, and the worker that makes more.
pub struct Images {
    ready: HashMap<ImageKey, (Arc<Picture>, u64)>,
    asked: HashSet<ImageKey>,
    /// Files that are not pictures, or not there: asked once.
    missing: HashSet<ImageKey>,
    to_worker: Sender<ImageKey>,
    from_worker: Receiver<Decoded>,
    decoded_bytes: usize,
    clock: u64,
    /// Bumped when pictures arrive, for whoever drew without them.
    pub arrivals: u64,
}

impl Images {
    /// Starts the worker. `arrived` runs on its thread after each picture,
    /// to wake the window.
    pub fn new(
        files: Arc<HashMap<String, Vec<u8>>>,
        arrived: impl Fn() + Send + 'static,
    ) -> Self {
        let (to_worker, requests) = channel::<ImageKey>();
        let (answers, from_worker) = channel();
        let started = std::thread::Builder::new()
            .name("skin-pictures".into())
            .spawn(move || {
                // Ends with `Images`, when the channel closes.
                for key in requests {
                    let picture = decode(&files, &key).map(Arc::new);
                    if answers.send((key, picture)).is_err() {
                        break;
                    }
                    arrived();
                }
            });
        if let Err(error) = started {
            log::warn!("could not start reading the skin's pictures: {error}");
        }
        Self {
            ready: HashMap::new(),
            asked: HashSet::new(),
            missing: HashSet::new(),
            to_worker,
            from_worker,
            decoded_bytes: 0,
            clock: 0,
            arrivals: 0,
        }
    }

    /// Takes in what the worker finished.
    pub fn poll(&mut self) {
        while let Ok((key, picture)) = self.from_worker.try_recv() {
            self.asked.remove(&key);
            self.arrivals = self.arrivals.wrapping_add(1);
            match picture {
                Some(picture) => {
                    self.decoded_bytes = self.decoded_bytes.saturating_add(picture.bytes());
                    self.clock += 1;
                    self.ready.insert(key, (picture, self.clock));
                    self.shrink();
                }
                None => {
                    if !key.file.is_empty() {
                        log::debug!("skin picture {} could not be read", key.file);
                    }
                    self.missing.insert(key);
                }
            }
        }
    }

    /// Forgets the longest unused pictures once too much is held. They are
    /// decoded again if they are wanted again.
    fn shrink(&mut self) {
        while self.decoded_bytes > MAX_DECODED && self.ready.len() > 1 {
            let Some(oldest) = self
                .ready
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            if let Some((picture, _)) = self.ready.remove(&oldest) {
                self.decoded_bytes = self.decoded_bytes.saturating_sub(picture.bytes());
            }
        }
    }

    /// A picture if it is ready; otherwise it is asked for.
    pub fn get(&mut self, key: &ImageKey) -> Option<Arc<Picture>> {
        self.clock += 1;
        let clock = self.clock;
        if let Some((picture, used)) = self.ready.get_mut(key) {
            *used = clock;
            return Some(picture.clone());
        }
        if !self.asked.contains(key)
            && !self.missing.contains(key)
            && self.to_worker.send(key.clone()).is_ok()
        {
            self.asked.insert(key.clone());
        }
        None
    }

    /// Whether anything asked for is still on its way.
    pub fn loading(&self) -> bool {
        !self.asked.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| image::Rgb(paint(x, y)));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn files(entries: Vec<(&str, Vec<u8>)>) -> HashMap<String, Vec<u8>> {
        entries
            .into_iter()
            .map(|(name, bytes)| (name.to_owned(), bytes))
            .collect()
    }

    #[test]
    fn the_transparency_colour_is_cleared() {
        let files = files(vec![(
            "bg.png",
            png(2, 1, |x, _| if x == 0 { [255, 0, 255] } else { [1, 2, 3] }),
        )]);
        let key = ImageKey {
            file: "bg.png".into(),
            clear: vec![[255, 0, 255]],
            ..ImageKey::default()
        };
        let picture = decode(&files, &key).unwrap();
        assert_eq!(picture.frames[0].bitmap.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(picture.frames[0].bitmap.pixel(1, 0), Some([1, 2, 3, 255]));
    }

    #[test]
    fn a_mask_keeps_only_its_own_pixels_in_their_box() {
        let files = files(vec![
            ("up.png", png(6, 4, |x, y| [x as u8 + 1, y as u8 + 1, 9])),
            // An L shape of green: its box has a corner that is not green.
            (
                "map.png",
                png(6, 4, |x, y| {
                    if (x == 2 && (1..3).contains(&y)) || (x == 3 && y == 2) {
                        [0, 255, 0]
                    } else {
                        [0, 0, 0]
                    }
                }),
            ),
        ]);
        let key = ImageKey {
            file: "up.png".into(),
            mask: Some(("map.png".into(), [0, 255, 0])),
            ..ImageKey::default()
        };
        let picture = decode(&files, &key).unwrap();
        assert_eq!(picture.origin, (2, 1));
        assert_eq!(picture.size, (6, 4));
        let bitmap = &picture.frames[0].bitmap;
        assert_eq!((bitmap.width, bitmap.height), (2, 2));
        assert_eq!(bitmap.pixel(0, 0), Some([3, 2, 9, 255]));
        assert_eq!(bitmap.pixel(1, 0), Some([0, 0, 0, 0]));
    }

    #[test]
    fn an_animated_gif_keeps_its_frames_and_times() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for (shade, delay) in [(10u8, 100u32), (200, 300)] {
                let buffer = image::RgbaImage::from_pixel(2, 2, image::Rgba([shade, 0, 0, 255]));
                encoder
                    .encode_frame(image::Frame::from_parts(
                        buffer,
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(delay, 1),
                    ))
                    .unwrap();
            }
        }
        let files = files(vec![("a.gif", bytes)]);
        let key = ImageKey {
            file: "a.gif".into(),
            ..ImageKey::default()
        };
        let picture = decode(&files, &key).unwrap();
        assert_eq!(picture.frames.len(), 2);
        assert_eq!(picture.frame_at(0).0, 0);
        assert_eq!(picture.frame_at(150), (1, Some(250)));
        assert_eq!(picture.frame_at(400), (1, None), "no loop block: once");
    }

    #[test]
    fn solid_parts_become_joined_rectangles() {
        // Two solid rows, then a row with a hole.
        let mut bitmap = Bitmap {
            width: 3,
            height: 3,
            rgba: vec![255; 36],
        };
        bitmap.rgba[4 * 7 + 3] = 0;
        let runs = solid_runs(&bitmap, (10, 20));
        assert_eq!(runs.len(), 3);
        assert_eq!(
            runs[0],
            Box2 {
                x: 10,
                y: 20,
                width: 3,
                height: 2
            }
        );
        assert_eq!(runs[1].width, 1);
        assert_eq!(runs[2].x, 12);
    }

    #[test]
    fn a_gif_says_how_often_it_plays() {
        assert_eq!(gif_loops(b"GIF89a no block"), Some(1));
        let forever = b"GIF89a..NETSCAPE2.0\x03\x01\x00\x00\x00";
        assert_eq!(gif_loops(forever), None);
        let twice = b"GIF89a..NETSCAPE2.0\x03\x01\x01\x00\x00";
        assert_eq!(gif_loops(twice), Some(2));
    }

    #[test]
    fn garbage_is_no_picture_and_is_asked_for_once() {
        let files = Arc::new(files(vec![("a.png", b"not a picture".to_vec())]));
        let mut images = Images::new(files, || {});
        let key = ImageKey {
            file: "a.png".into(),
            ..ImageKey::default()
        };
        assert!(images.get(&key).is_none());
        while images.loading() {
            images.poll();
            std::thread::yield_now();
        }
        assert!(images.get(&key).is_none());
        assert!(!images.loading());
    }
}
