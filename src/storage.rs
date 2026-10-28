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

/// Clear only the disposable audio folder. The active librespot cache must
/// remove its own entries so its size limiter does not retain deleted files.
/// Keep bucket directories: a download may still be finishing into one.
pub(crate) fn clear_audio(
    root: &Path,
    cache: Option<&librespot_core::cache::Cache>,
) -> Result<u64, String> {
    let mut removed = 0;
    let mut paths = vec![root.to_owned()];
    while let Some(path) = paths.pop() {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        if metadata.is_symlink() {
            if path == root {
                return Err("The audio cache folder is a symbolic link.".into());
            }
            continue;
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path).map_err(|error| error.to_string())? {
                paths.push(entry.map_err(|error| error.to_string())?.path());
            }
        } else if metadata.is_file() {
            let deleted = if let Some(cache) = cache {
                let file = audio_file_id(root, &path)
                    .filter(|file| cache.file_path(*file).as_ref() == Some(&path))
                    .ok_or_else(|| format!("Unrecognized audio cache file: {}", path.display()))?;
                match cache.remove_file(file) {
                    Ok(()) => true,
                    Err(error) if error.kind == librespot_core::error::ErrorKind::NotFound => false,
                    Err(error) => return Err(error.to_string()),
                }
            } else {
                match std::fs::remove_file(&path) {
                    Ok(()) => true,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                    Err(error) => return Err(format!("{}: {error}", path.display())),
                }
            };
            if deleted {
                removed += metadata.len();
            }
        }
    }
    Ok(removed)
}

/// Librespot stores a 20-byte file ID as two hex characters for the bucket
/// and 38 for the file. Reject other paths rather than removing a different
/// cache entry or leaving its size accounting stale.
fn audio_file_id(root: &Path, path: &Path) -> Option<librespot_core::file_id::FileId> {
    let relative = path.strip_prefix(root).ok()?;
    let mut parts = relative.iter();
    let bucket = parts.next()?.to_str()?;
    let name = parts.next()?.to_str()?;
    if parts.next().is_some() || bucket.len() != 2 || name.len() != 38 {
        return None;
    }
    let hex = format!("{bucket}{name}");
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut id = [0; 20];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(librespot_core::file_id::FileId::from_raw(&id))
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
        let access = std::fs::read_dir(&nested);
        let measured = measure(dir.path(), true);
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700)).unwrap();
        match access {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                assert_eq!(
                    measured.unwrap_err().kind(),
                    io::ErrorKind::PermissionDenied
                );
            }
            // A privileged runner can traverse the directory despite its mode.
            Ok(_) => assert_eq!(measured.unwrap(), 13),
            Err(error) => panic!("unexpected directory access error: {error}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_size_does_not_require_reading_the_file_contents() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        let file = dir.path().join("unreadable");
        std::fs::write(&file, [0; 13]).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let measured = measure(&file, false);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(measured.unwrap(), 13);
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

    #[test]
    fn clearing_audio_updates_the_live_limiter_and_preserves_an_open_song() {
        use librespot_core::{cache::Cache, file_id::FileId};
        use std::io::{Cursor, Read};
        let dir = TestDir::new();
        let audio = dir.path().join("audio");
        let state = dir.path().join("state");
        let cache = Cache::new(None::<&Path>, Some(&state), Some(&audio), Some(100)).unwrap();
        let first = FileId::from_raw(&[0x11; 20]);
        cache.save_file(first, &mut Cursor::new([1; 40])).unwrap();
        cache
            .save_file(FileId::from_raw(&[0x22; 20]), &mut Cursor::new([2; 40]))
            .unwrap();
        cache.save_volume(123);
        let mut playing = cache.file(first).unwrap();
        assert_eq!(clear_audio(&audio, Some(&cache)).unwrap(), 80);
        assert_eq!(cache.volume(), Some(123));
        let mut bytes = Vec::new();
        playing.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, vec![1; 40]);
        // Stale accounting would try to evict the deleted files and return
        // NotFound here, although the new entry fits the entire cache budget.
        cache
            .save_file(FileId::from_raw(&[0x33; 20]), &mut Cursor::new([3; 70]))
            .unwrap();
        assert_eq!(measure(&audio, true).unwrap(), 70);
    }

    #[test]
    fn clearing_an_inactive_cache_removes_nested_files_but_not_sibling_state() {
        let dir = TestDir::new();
        let audio = dir.path().join("audio");
        assert_eq!(clear_audio(&audio, None).unwrap(), 0);
        std::fs::create_dir_all(audio.join("nested")).unwrap();
        std::fs::write(audio.join("nested/track"), [0; 27]).unwrap();
        let history = dir.path().join("history.json");
        std::fs::write(&history, "[]").unwrap();
        assert_eq!(clear_audio(&audio, None).unwrap(), 27);
        assert_eq!(measure(&audio, true).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(history).unwrap(), "[]");
    }

    #[cfg(unix)]
    #[test]
    fn clearing_audio_does_not_follow_links_or_hide_permission_errors() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let dir = TestDir::new();
        let audio = dir.path().join("audio");
        std::fs::create_dir(&audio).unwrap();
        let outside = dir.path().join("keep");
        std::fs::write(&outside, [0; 13]).unwrap();
        symlink(&outside, audio.join("link")).unwrap();
        symlink(dir.path(), audio.join("folder-link")).unwrap();
        assert_eq!(clear_audio(&audio, None).unwrap(), 0);
        assert!(outside.exists());
        assert!(clear_audio(&audio.join("folder-link"), None).is_err());
        std::fs::write(audio.join("locked"), [0; 7]).unwrap();
        let probe = audio.join("permission-probe");
        std::fs::write(&probe, []).unwrap();
        std::fs::set_permissions(&audio, std::fs::Permissions::from_mode(0o500)).unwrap();
        let access = std::fs::remove_file(&probe);
        let cleared = clear_audio(&audio, None);
        std::fs::set_permissions(&audio, std::fs::Permissions::from_mode(0o700)).unwrap();
        match access {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                assert!(cleared.is_err());
                assert!(audio.join("locked").exists());
            }
            // Creating/removing directory entries can bypass mode bits for root.
            Ok(()) => assert_eq!(cleared.unwrap(), 7),
            Err(error) => panic!("unexpected file removal error: {error}"),
        }
    }

    #[test]
    fn an_active_cache_rejects_files_it_cannot_account_for() {
        let dir = TestDir::new();
        let cache =
            librespot_core::cache::Cache::new(None::<&Path>, None, Some(dir.path()), Some(100))
                .unwrap();
        std::fs::write(dir.path().join("unexpected"), [0; 7]).unwrap();
        assert!(
            clear_audio(dir.path(), Some(&cache))
                .unwrap_err()
                .contains("Unrecognized")
        );
        assert!(dir.path().join("unexpected").exists());
    }
}
