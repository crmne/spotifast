//! Finished MilkDrop frames, shared between the child that draws them and the
//! app that shows them inside a skin.
//!
//! A memory-mapped file holds a small header and three slots. The child writes
//! each frame into the slot after the one published last, then publishes it;
//! the app copies whichever slot is newest when it next paints. Neither side
//! ever waits for the other. Each slot is a sequence lock: a copy overtaken by
//! the writer shows up as a changed sequence, is thrown away, and the picture
//! already on screen stays until the next paint. With three slots that takes
//! two whole frames drawn during one copy.
//!
//! The header also carries what the app asks of the child, as plain atomics,
//! so that the UI thread never writes to a pipe that could be full.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};

use egui::Color32;
use memmap2::MmapMut;

/// The longest side of a pane's frame, in pixels. A pane or window bigger than
/// this is scaled up: every frame crosses the process boundary and the
/// graphics card twice, and MilkDrop has never needed the pixels.
pub const PANE_SIDE: u32 = 1280;
/// The longest side behind full screen lyrics, where the picture fills a
/// screen with nothing to hide its softness.
pub const FULL_SCREEN_SIDE: u32 = 1920;
/// The most a slot holds, whatever is asked.
pub const MAX_SIDE: u32 = FULL_SCREEN_SIDE;
const SLOTS: usize = 3;
const BYTES_PER_PIXEL: usize = 4;
const SLOT_BYTES: usize = (MAX_SIDE * MAX_SIDE) as usize * BYTES_PER_PIXEL;
/// A page, so that the pixels start aligned.
const HEADER: usize = 4096;
/// The whole mapping's size. The file is sparse: only what a frame of the
/// pane's size touches ever takes memory.
pub const SIZE: usize = HEADER + SLOTS * SLOT_BYTES;

#[repr(C)]
struct Slot {
    /// Odd while the child writes here, moved on by two for each frame.
    sequence: AtomicU64,
    /// Width in the high half, height in the low one.
    size: AtomicU64,
}

#[repr(C)]
struct Header {
    /// The newest frame: its count times four, plus its slot. Zero until
    /// there is one.
    latest: AtomicU64,
    slots: [Slot; SLOTS],
    /// The size the app wants frames at, packed like a slot's.
    wanted: AtomicU64,
    /// Moved on each time the app shows the picture.
    beat: AtomicU64,
    /// Presets to move on (or, negative, back) by, not yet taken.
    steps: AtomicI64,
    fps: AtomicU32,
    seconds: AtomicU32,
    /// Not zero while the picture holds still, as it does while music is
    /// paused behind full screen lyrics.
    frozen: AtomicU32,
}

const _: () = assert!(std::mem::size_of::<Header>() <= HEADER);

fn pack([width, height]: [u32; 2]) -> u64 {
    (u64::from(width) << 32) | u64::from(height)
}

fn unpack(packed: u64) -> [u32; 2] {
    [(packed >> 32) as u32, packed as u32]
}

/// The frame size for a picture this many physical pixels big, drawn `scale`
/// times smaller inside (the MilkDrop setting that eases a slow GPU): whole,
/// even, because projectM's half-size passes leave a seam otherwise, and no
/// longer on either side than [`MAX_SIDE`], keeping the picture's shape.
pub fn fit([width, height]: [f32; 2], scale: u32, side: u32) -> [u32; 2] {
    let scale = scale.max(1) as f32;
    let side = side.min(MAX_SIDE);
    let (width, height) = ((width / scale).max(2.0), (height / scale).max(2.0));
    let shrink = (side as f32 / width.max(height)).min(1.0);
    [width, height].map(|length| (((length * shrink).round() as u32) & !1).clamp(2, side))
}

/// How bright a frame's bright parts are, from 0 to 1: the luma nine samples
/// in ten stay under. Lyrics over it are dimmed by this, so a stray spark in
/// a dark preset does not dim the lot, and a white flash does.
pub fn glow(pixels: &[Color32]) -> f32 {
    const SAMPLES: usize = 256;
    let step = (pixels.len() / SAMPLES).max(1);
    let mut lumas: Vec<u16> = pixels
        .iter()
        .step_by(step)
        .map(|pixel| {
            let [r, g, b, _] = pixel.to_array().map(u16::from);
            (r * 3 + g * 6 + b) / 10
        })
        .collect();
    if lumas.is_empty() {
        return 0.0;
    }
    let at = (lumas.len() * 9 / 10).min(lumas.len() - 1);
    let (_, luma, _) = lumas.select_nth_unstable(at);
    f32::from(*luma) / 255.0
}

/// A handle on the shared frames, held by the app and by the child.
pub struct Frames {
    map: MmapMut,
    // The host unlinks the backing file; the mapping outlives that.
    _file: File,
}

impl Frames {
    /// Makes the file, sizes it, and maps it: the app's side. Nobody else on
    /// the machine may read it, and a name left by a dead run is replaced
    /// rather than followed.
    pub fn create(path: &Path) -> io::Result<Self> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(path)?;
        file.set_len(SIZE as u64)?;
        // SAFETY: the file is this process's own, freshly sized to `SIZE`.
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self { map, _file: file })
    }

    /// Maps a file the app already made: the child's side.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        if (file.metadata()?.len() as usize) < SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the MilkDrop frame buffer is too small",
            ));
        }
        // SAFETY: the file is at least `SIZE` bytes, sized by the app.
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self { map, _file: file })
    }

    fn header(&self) -> &Header {
        // SAFETY: the mapping is page aligned and at least `HEADER` bytes,
        // which holds a `Header`, and zeroed atomics are valid.
        unsafe { &*self.map.as_ptr().cast::<Header>() }
    }

    fn pixels(&self, slot: usize) -> *mut u8 {
        // SAFETY: `slot` is below `SLOTS`, so this is within the mapping.
        unsafe { self.map.as_ptr().add(HEADER + slot * SLOT_BYTES).cast_mut() }
    }

    /// Asks for frames of this size.
    pub fn ask(&self, size: [u32; 2]) {
        self.header().wanted.store(pack(size), Ordering::Relaxed);
    }

    /// Says the picture is being shown.
    pub fn beat(&self) {
        self.header().beat.fetch_add(1, Ordering::Relaxed);
    }

    /// Moves the preset on or back.
    pub fn step(&self, by: i32) {
        self.header()
            .steps
            .fetch_add(i64::from(by), Ordering::Relaxed);
    }

    /// Holds the picture still, or lets it go on.
    pub fn freeze(&self, frozen: bool) {
        self.header()
            .frozen
            .store(u32::from(frozen), Ordering::Relaxed);
    }

    /// Whether the picture is holding still.
    pub fn frozen(&self) -> bool {
        self.header().frozen.load(Ordering::Relaxed) != 0
    }

    /// Sets the frame rate and how long a preset plays.
    pub fn pace(&self, fps: u32, seconds: u32) {
        let header = self.header();
        header.fps.store(fps, Ordering::Relaxed);
        header.seconds.store(seconds, Ordering::Relaxed);
    }

    /// The size frames are wanted at, once the app has said, within the
    /// limits a slot holds whatever it said.
    pub fn wanted(&self) -> Option<[u32; 2]> {
        let [width, height] = unpack(self.header().wanted.load(Ordering::Relaxed));
        (width >= 2 && height >= 2).then(|| [width.min(MAX_SIDE), height.min(MAX_SIDE)])
    }

    /// How many times the picture has been shown.
    pub fn beats(&self) -> u64 {
        self.header().beat.load(Ordering::Relaxed)
    }

    /// Takes what the preset has been asked to move by.
    pub fn take_steps(&self) -> i64 {
        self.header().steps.swap(0, Ordering::Relaxed)
    }

    /// The frame rate and a preset's time, as last set.
    pub fn paced(&self) -> (u32, u32) {
        let header = self.header();
        (
            header.fps.load(Ordering::Relaxed),
            header.seconds.load(Ordering::Relaxed),
        )
    }

    /// Publishes a frame of `size`: `draw` fills its pixels, RGBA, bottom row
    /// first as OpenGL reads them.
    pub fn publish(&self, [width, height]: [u32; 2], draw: impl FnOnce(&mut [u8])) {
        let (width, height) = (width.min(MAX_SIDE), height.min(MAX_SIDE));
        let header = self.header();
        let latest = header.latest.load(Ordering::Relaxed);
        let slot = if latest == 0 {
            0
        } else {
            ((latest % 4) as usize + 1) % SLOTS
        };
        let entry = &header.slots[slot];
        let sequence = entry.sequence.load(Ordering::Relaxed);
        entry.sequence.store(sequence | 1, Ordering::Relaxed);
        fence(Ordering::Release);
        entry.size.store(pack([width, height]), Ordering::Relaxed);
        let bytes = width as usize * height as usize * BYTES_PER_PIXEL;
        // SAFETY: at most `MAX_SIDE` square, which is a slot, and only this
        // one process writes. The reader may be reading the slot at this
        // moment; its sequence check throws that copy away.
        draw(unsafe { std::slice::from_raw_parts_mut(self.pixels(slot), bytes) });
        entry
            .sequence
            .store((sequence | 1).wrapping_add(1), Ordering::Release);
        let count = latest / 4 + 1;
        header
            .latest
            .store(count * 4 + slot as u64, Ordering::Release);
    }

    /// Copies out the newest frame, if it is not the one in `seen`, and marks
    /// it seen. `None` also when the writer overtook the copy: the next
    /// paint gets a later frame. Never waits.
    pub fn take(&self, seen: &mut u64) -> Option<([usize; 2], Vec<Color32>)> {
        let header = self.header();
        let latest = header.latest.load(Ordering::Acquire);
        if latest == 0 || latest == *seen {
            return None;
        }
        let slot = (latest % 4) as usize;
        let entry = header.slots.get(slot)?;
        let sequence = entry.sequence.load(Ordering::Acquire);
        if sequence & 1 == 1 {
            return None;
        }
        let [width, height] = unpack(entry.size.load(Ordering::Relaxed));
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return None;
        }
        let count = width as usize * height as usize;
        let mut pixels = Vec::<Color32>::with_capacity(count);
        // SAFETY: the slot holds `count` pixels and the vector has room for
        // them. `Color32` is four bytes with no invalid values, so even a
        // torn copy is memory safe to keep until the check below.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.pixels(slot).cast_const(),
                pixels.as_mut_ptr().cast::<u8>(),
                count * BYTES_PER_PIXEL,
            );
            pixels.set_len(count);
        }
        fence(Ordering::Acquire);
        if entry.sequence.load(Ordering::Relaxed) != sequence {
            return None;
        }
        *seen = latest;
        Some(([width as usize, height as usize], pixels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    fn file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("spotifast-frames-{name}-{}", std::process::id()))
    }

    #[test]
    fn a_pane_is_drawn_at_its_pixels_up_to_the_limit() {
        assert_eq!(fit([400.0, 300.0], 1, PANE_SIDE), [400, 300]);
        assert_eq!(
            fit([401.0, 301.0], 1, PANE_SIDE),
            [400, 300],
            "even, for projectM"
        );
        assert_eq!(
            fit([400.0, 300.0], 2, PANE_SIDE),
            [200, 150],
            "the scale setting"
        );
        assert_eq!(
            fit([3840.0, 2160.0], 1, PANE_SIDE),
            [1280, 720],
            "a big window keeps its shape"
        );
        assert_eq!(
            fit([0.0, 0.0], 1, PANE_SIDE),
            [2, 2],
            "and nothing is ever empty"
        );
    }

    #[test]
    fn the_newest_frame_is_taken_once() {
        let path = file("newest");
        let app = Frames::create(&path).unwrap();
        let child = Frames::open(&path).unwrap();
        let mut seen = 0;
        assert!(app.take(&mut seen).is_none(), "nothing drawn yet");

        child.publish([2, 2], |pixels| pixels.fill(1));
        child.publish([4, 2], |pixels| pixels.fill(2));
        let (size, pixels) = app.take(&mut seen).unwrap();
        assert_eq!(size, [4, 2], "two frames came, the newest is shown");
        assert_eq!(pixels.len(), 8);
        assert!(pixels.iter().all(|pixel| pixel.to_array() == [2; 4]));
        assert!(app.take(&mut seen).is_none(), "and shown once");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn what_the_app_asks_reaches_the_child() {
        let path = file("asks");
        let app = Frames::create(&path).unwrap();
        let child = Frames::open(&path).unwrap();
        assert_eq!(child.wanted(), None, "not asked yet");
        app.ask([640, 360]);
        assert_eq!(child.wanted(), Some([640, 360]));
        app.ask([9000, 2]);
        assert_eq!(child.wanted(), Some([MAX_SIDE, 2]), "a slot's limit holds");
        app.step(1);
        app.step(1);
        app.step(-1);
        assert_eq!(child.take_steps(), 1);
        assert_eq!(child.take_steps(), 0, "each step is taken once");
        app.pace(30, 12);
        assert_eq!(child.paced(), (30, 12));
        app.beat();
        assert_eq!(child.beats(), 1);
        std::fs::remove_file(&path).unwrap();
    }

    /// The writer never waits and the reader only ever gets whole frames:
    /// every pixel of a frame carries that frame's number, and the writer
    /// runs flat out against a reader that takes as fast as it can.
    #[test]
    fn a_frame_is_never_torn_and_nobody_waits() {
        let path = file("race");
        let app = Frames::create(&path).unwrap();
        let child = Arc::new(Frames::open(&path).unwrap());
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let (child, done) = (Arc::clone(&child), Arc::clone(&done));
            std::thread::spawn(move || {
                let mut frame = 0u8;
                while !done.load(Ordering::Relaxed) {
                    frame = frame.wrapping_add(1);
                    child.publish([256, 256], |pixels| pixels.fill(frame));
                }
            })
        };
        let mut seen = 0;
        let mut whole = 0;
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(300) {
            if let Some((_, pixels)) = app.take(&mut seen) {
                let first = pixels[0];
                assert!(pixels.iter().all(|pixel| *pixel == first), "a torn frame");
                whole += 1;
            }
        }
        done.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        assert!(whole > 0, "frames did arrive");
        std::fs::remove_file(&path).unwrap();
    }
}
