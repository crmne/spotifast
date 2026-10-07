//! Shared-memory audio ring for the MilkDrop child process.
//!
//! The player writes half a second of stereo audio to a memory-mapped file;
//! the child reads it with the same lag as the analyser. It has one producer
//! and one consumer. A torn frame is acceptable for visualization.
//!
//! The layout is an atomic `u64` total-frame count followed by plain stereo
//! frames.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::MmapMut;

/// Stereo frames kept: half a second at 44.1 kHz, the same as the tap.
pub const FRAMES: usize = 22_050;
/// The count sits in the first eight bytes; the ring starts after a whole
/// sixteen, so the frames stay aligned.
const HEADER: usize = 16;
const FRAME_BYTES: usize = 8; // [f32; 2]
/// The whole mapping's size.
pub const SIZE: usize = HEADER + FRAMES * FRAME_BYTES;

/// A handle on the shared ring, held by both the writer and the reader.
pub struct Ring {
    map: MmapMut,
    // The backing file is unlinked by the host when it is done; the mapping
    // keeps working until both sides drop it.
    _file: File,
}

impl Ring {
    /// Makes the file, sizes it, and maps it: the writer's side.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(SIZE as u64)?;
        // SAFETY: the file is this process's own, freshly sized to `SIZE`.
        let map = unsafe { MmapMut::map_mut(&file)? };
        let ring = Self { map, _file: file };
        ring.count().store(0, Ordering::Release);
        Ok(ring)
    }

    /// Maps a file the host already made: the reader's side.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        if (file.metadata()?.len() as usize) < SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the MilkDrop audio buffer is too small",
            ));
        }
        // SAFETY: the file is at least `SIZE` bytes, sized by the host.
        let map = unsafe { MmapMut::map_mut(&file)? };
        Ok(Self { map, _file: file })
    }

    /// The frame counter at the head of the mapping.
    fn count(&self) -> &AtomicU64 {
        // SAFETY: the mapping is at least `HEADER` bytes and the base is page
        // aligned, so the first eight bytes are an aligned `AtomicU64`.
        unsafe { &*(self.map.as_ptr() as *const AtomicU64) }
    }

    /// The ring of frames, as a flat float slice (LRLR...).
    fn floats(&self) -> *mut f32 {
        // SAFETY: `HEADER` is a multiple of four, so this is aligned for f32,
        // and the mapping holds `FRAMES * 2` floats past it.
        unsafe { self.map.as_ptr().add(HEADER) as *mut f32 }
    }

    /// Appends stereo frames (interleaved LRLR), oldest dropped once full.
    pub fn push(&self, interleaved: &[f32]) {
        let frames = interleaved.len() / 2;
        if frames == 0 {
            return;
        }
        let base = self.floats();
        let mut total = self.count().load(Ordering::Relaxed);
        for frame in 0..frames {
            let slot = (total as usize % FRAMES) * 2;
            // SAFETY: `slot` is within the ring, which holds `FRAMES * 2`
            // floats; the reader tolerates a torn frame.
            unsafe {
                *base.add(slot) = interleaved[frame * 2];
                *base.add(slot + 1) = interleaved[frame * 2 + 1];
            }
            total += 1;
        }
        self.count().store(total, Ordering::Release);
    }

    /// The frames written since `cursor`, up to `lag` frames behind the
    /// newest, and moves the cursor past them. Mirrors `AudioTap::since`.
    pub fn since(&self, cursor: &mut u64, lag: usize) -> Vec<[f32; 2]> {
        let total = self.count().load(Ordering::Acquire);
        let end = total.saturating_sub(lag as u64);
        let oldest = total.saturating_sub(FRAMES as u64);
        let start = (*cursor).max(oldest).min(end);
        let base = self.floats();
        let out = (start..end)
            .map(|frame| {
                let slot = (frame as usize % FRAMES) * 2;
                // SAFETY: `slot` is within the ring.
                unsafe { [*base.add(slot), *base.add(slot + 1)] }
            })
            .collect();
        *cursor = end.max(*cursor);
        out
    }
}

// SAFETY: the ring is a single-producer, single-consumer buffer; the count is
// atomic and the frames tolerate a torn read, so sharing the handle across
// threads (the audio thread writes, elsewhere reads) is sound.
unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_written_are_read_back_once_and_behind_the_lag() {
        let path = std::env::temp_dir().join(format!("spotizgeg-shm-test-{}", std::process::id()));
        let writer = Ring::create(&path).unwrap();
        let reader = Ring::open(&path).unwrap();
        writer.push(&[0.5, -0.5, 1.0, 0.0, 0.2, 0.2, 0.3, 0.3]);
        let mut cursor = 0;
        assert_eq!(
            reader.since(&mut cursor, 1),
            [[0.5, -0.5], [1.0, 0.0], [0.2, 0.2]]
        );
        assert_eq!(cursor, 3);
        assert!(reader.since(&mut cursor, 1).is_empty());
        writer.push(&[0.9, 0.9]);
        assert_eq!(reader.since(&mut cursor, 1), [[0.3, 0.3]]);
        // A cursor behind the kept window starts at the oldest kept frame.
        let mut extra = vec![0.0f32; 2 * (FRAMES + 10)];
        for (i, sample) in extra.iter_mut().enumerate() {
            *sample = i as f32;
        }
        writer.push(&extra);
        let mut stale = 0;
        assert_eq!(reader.since(&mut stale, 0).len(), FRAMES);
        std::fs::remove_file(&path).unwrap();
    }
}
