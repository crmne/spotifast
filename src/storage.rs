//! File sizes for Settings, measured off the UI thread and only while needed.

use std::io;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::paths::AppDirs;

const REFRESH_AFTER: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Size {
    #[default]
    Checking,
    Bytes(u64),
    Unavailable,
}

impl From<io::Result<u64>> for Size {
    fn from(result: io::Result<u64>) -> Self {
        result.map_or(Self::Unavailable, Self::Bytes)
    }
}

/// One scan at a time. Invalidation discards an in-flight answer before
/// starting another, so a pre-clear measurement cannot restore old sizes.
#[derive(Default)]
pub(crate) struct Usage {
    pub sizes: [Size; 3],
    pending: Option<Receiver<[Size; 3]>>,
    checked: Option<Instant>,
    invalidated: bool,
}

impl Usage {
    pub fn invalidate(&mut self) {
        self.sizes = [Size::Checking; 3];
        self.checked = None;
        self.invalidated = true;
    }

    /// Called only when Settings draws its Storage section. Disk traversal
    /// never happens here, and closing Settings schedules no further scans.
    pub fn update(&mut self, dirs: &AppDirs, ctx: &egui::Context) {
        if let Some(pending) = &self.pending {
            match pending.try_recv() {
                Ok(sizes) => {
                    self.pending = None;
                    if !self.invalidated {
                        self.sizes = sizes;
                        self.checked = Some(Instant::now());
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.pending = None;
                    if !self.invalidated {
                        self.sizes = [Size::Unavailable; 3];
                        self.checked = Some(Instant::now());
                    }
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let remaining = self
            .checked
            .map(|checked| REFRESH_AFTER.saturating_sub(checked.elapsed()))
            .unwrap_or_default();
        if self.pending.is_none() && remaining.is_zero() {
            let dirs = dirs.clone();
            let ctx = ctx.clone();
            let (send, receive) = mpsc::channel();
            self.invalidated = false;
            match std::thread::Builder::new()
                .name("storage-usage".into())
                .spawn(move || {
                    let sizes = [
                        // Clear artwork removes only files directly in this folder.
                        measure(&dirs.art_cache_dir(), false).into(),
                        measure(&dirs.audio_cache_dir(), true).into(),
                        measure(&dirs.history_file(), false).into(),
                    ];
                    let _ = send.send(sizes);
                    ctx.request_repaint();
                }) {
                Ok(_) => self.pending = Some(receive),
                Err(error) => {
                    log::warn!("could not start storage measurement: {error}");
                    self.sizes = [Size::Unavailable; 3];
                    self.checked = Some(Instant::now());
                }
            }
        }
        ctx.request_repaint_after(if remaining.is_zero() {
            REFRESH_AFTER
        } else {
            remaining
        });
    }
}

/// Sum regular file lengths, without following links or counting directory
/// metadata. Missing files are empty; an unreadable entry makes the size
/// unavailable rather than presenting an incomplete sum as a total.
fn measure(path: &Path, recursive: bool) -> io::Result<u64> {
    let mut total = 0_u64;
    let mut paths = vec![path.to_owned()];
    while let Some(path) = paths.pop() {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .ok_or_else(|| io::Error::other("storage size overflow"))?;
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_file() || (recursive && kind.is_dir()) {
                    paths.push(entry.path());
                }
            }
        }
    }
    Ok(total)
}

/// Decimal units. Keep nonempty
/// files visibly nonzero, even below one kilobyte.
pub(crate) fn format_bytes(bytes: u64) -> String {
    if bytes < 1_000 {
        return format!("{bytes} B");
    }
    let units = ["kB", "MB", "GB", "TB", "PB", "EB"];
    let mut value = bytes as f64 / 1_000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < units.len() {
        value /= 1_000.0;
        unit += 1;
    }
    format!("{value:.1} {}", units[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("spotifast-storage-{}-{id}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_empty_and_nested_storage() {
        let dir = TestDir::new();
        let root = dir.path();
        assert_eq!(measure(&root.join("missing"), true).unwrap(), 0);
        assert_eq!(measure(root, true).unwrap(), 0);
        std::fs::write(root.join("cover"), [0; 13]).unwrap();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/track"), [0; 27]).unwrap();
        assert_eq!(measure(root, false).unwrap(), 13);
        assert_eq!(measure(root, true).unwrap(), 40);
        assert_eq!(measure(&root.join("cover"), false).unwrap(), 13);
    }

    #[cfg(unix)]
    #[test]
    fn links_do_not_count_targets_or_loop() {
        use std::os::unix::fs::symlink;
        let dir = TestDir::new();
        let root = dir.path();
        std::fs::write(root.join("file"), [0; 13]).unwrap();
        symlink(root.join("file"), root.join("alias")).unwrap();
        symlink(root, root.join("loop")).unwrap();
        symlink(root.join("missing"), root.join("broken")).unwrap();
        assert_eq!(measure(root, true).unwrap(), 13);
        assert_eq!(measure(&root.join("alias"), true).unwrap(), 0);
        assert_eq!(measure(&root.join("loop"), true).unwrap(), 0);
    }

    #[test]
    fn errors_are_unavailable_instead_of_zero() {
        assert_eq!(
            Size::from(Err(io::Error::other("unreadable"))),
            Size::Unavailable
        );
        assert_eq!(Size::from(Ok(0)), Size::Bytes(0));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_subfolder_does_not_report_a_partial_total() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        let nested = dir.path().join("nested");
        std::fs::write(dir.path().join("readable"), [0; 13]).unwrap();
        std::fs::create_dir(&nested).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o000)).unwrap();
        let measured = measure(dir.path(), true);
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            measured.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn invalidation_discards_an_answer_from_before_clearing() {
        let (send, receive) = mpsc::channel();
        let mut usage = Usage {
            sizes: [Size::Bytes(100); 3],
            pending: Some(receive),
            ..Usage::default()
        };
        usage.invalidate();
        send.send([Size::Bytes(100); 3]).unwrap();
        let dir = TestDir::new();
        let dirs = AppDirs {
            config: dir.path().join("config"),
            state: dir.path().join("state"),
            cache: dir.path().join("cache"),
        };
        usage.update(&dirs, &egui::Context::default());
        assert_eq!(usage.sizes, [Size::Checking; 3]);
        assert!(usage.pending.is_some());
    }

    #[test]
    fn readable_sizes_include_small_files_and_unit_boundaries() {
        for (bytes, expected) in [
            (0, "0 B"),
            (2, "2 B"),
            (999, "999 B"),
            (1_000, "1.0 kB"),
            (999_999, "1.0 MB"),
            (12_500_000, "12.5 MB"),
            (4_000_000_000, "4.0 GB"),
            (u64::MAX, "18.4 EB"),
        ] {
            assert_eq!(format_bytes(bytes), expected);
        }
    }
}
