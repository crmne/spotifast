//! MilkDrop visualization through libprojectM.
//!
//! projectM reads MilkDrop `.milk` presets and renders them with OpenGL.
//! MilkDrop runs as this binary with `--milkdrop-child` because winit permits
//! one event loop per process and eframe owns the main one. Audio reaches the
//! child through a shared-memory ring.
//!
//! [`Presets`] lists the config directory's `milkdrop` folder and downloads
//! optional projectM preset packs. No presets are bundled.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The MilkDrop child process: its own window, context, and event loop.
#[cfg(feature = "milkdrop")]
pub mod child;
#[cfg(feature = "milkdrop")]
pub mod engine;
/// The app's side of the child process: spawning it and talking to it.
#[cfg(feature = "milkdrop")]
pub mod host;
/// Fading text over the picture: the keys, the playing song.
pub mod overlay;
/// The shared-memory audio ring between the app and the MilkDrop child.
#[cfg(feature = "milkdrop")]
pub mod shm;

/// How long a preset plays before the next fades in.
pub const DEFAULT_SECONDS: u32 = 10;
/// How many frames a second the window draws by default.
pub const DEFAULT_FPS: u32 = 60;
/// Configurable frame-rate range. Zero is handled separately as uncapped.
pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 10..=360;
/// Frame-rate slider stops: 30, 60, screen rate, current rate, and uncapped.
pub fn fps_stops(screen: u32, current: u32) -> Vec<u32> {
    let mut rates = vec![30, 60];
    for extra in [screen, current] {
        if extra > 0 && !rates.contains(&extra) {
            rates.push(extra);
        }
    }
    rates.sort_unstable();
    rates.push(0);
    rates
}

/// Label for a frame-rate stop, marking the screen's refresh rate.
pub fn fps_label(locale: crate::i18n::Locale, rate: u32, screen: u32) -> String {
    use crate::i18n::gettext;
    match rate {
        0 => gettext(locale, "Uncapped").into_owned(),
        rate if rate == screen => gettext(
            locale,
            // Translators: A frame rate that matches the display. Keep {rate}.
            "{rate} fps, your screen",
        )
        .replace("{rate}", &rate.to_string()),
        rate => format!("{rate} fps"),
    }
}

/// The window's size when it first opens, in logical points.
pub const DEFAULT_SIZE: [f32; 2] = [640.0, 480.0];
/// The smallest the window may be dragged.
pub const MIN_SIZE: [f32; 2] = [320.0, 240.0];
/// MilkDrop's preset crossfade duration.
pub const CROSSFADE_SECONDS: f64 = 2.7;
/// How far behind the newest sample the picture runs: the same lag as the
/// analyser, so the picture and the speaker agree.
pub use crate::vis::LAG;
/// How long a listing of the folder is trusted.
const LISTED_FRESH: Duration = Duration::from_secs(30);
/// How deep into the folder presets are looked for: packs come in a folder
/// of folders.
const MAX_DEPTH: usize = 4;

/// A preset pack projectM curates, fetched as a zip of `.milk` files.
pub struct Pack {
    pub name: &'static str,
    pub url: &'static str,
    /// What it holds, and roughly how big the download is.
    pub note: &'static str,
}

pub const PACKS: [Pack; 2] = [
    Pack {
        name: "MilkDrop's own",
        url: "https://github.com/projectM-visualizer/presets-milkdrop-original/archive/refs/heads/master.zip",
        note: "The 550 presets that shipped with MilkDrop 2; about 1 MB.",
    },
    Pack {
        name: "Cream of the Crop",
        url: "https://github.com/projectM-visualizer/presets-cream-of-the-crop/archive/refs/heads/master.zip",
        note: "Jason Fletcher's pick of 9,800 presets the community made; about 25 MB.",
    },
];

/// What the window wants of the engine at its next frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Fade into a preset, or cut straight to it.
    Load { path: PathBuf, smooth: bool },
}

/// The presets in the folder and the order they have played in.
pub struct Presets {
    files: Vec<PathBuf>,
    listed: Option<Instant>,
    /// Played presets and the current position for Previous and Next.
    history: Vec<PathBuf>,
    at: usize,
    /// Whether automatic preset changes are disabled.
    pub locked: bool,
    /// Random order when true; folder order when false.
    random: bool,
    pending: Option<Request>,
    download: Option<(&'static str, mpsc::Receiver<Result<usize, String>>)>,
}

impl Default for Presets {
    fn default() -> Self {
        Self::new()
    }
}

impl Presets {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            listed: None,
            history: Vec::new(),
            at: 0,
            locked: false,
            random: true,
            pending: None,
            download: None,
        }
    }

    /// Lists the folder again if the listing is old.
    pub fn refresh(&mut self, folder: &Path) {
        if self.listed.is_none_or(|at| at.elapsed() > LISTED_FRESH) {
            self.list(folder);
        }
    }

    /// Lists the folder: every `.milk` file in it, folders included.
    pub fn list(&mut self, folder: &Path) {
        self.listed = Some(Instant::now());
        self.files = list_presets(folder);
    }

    pub fn count(&self) -> usize {
        self.files.len()
    }

    /// Current preset, if one has been selected.
    pub fn current(&self) -> Option<&Path> {
        self.history.get(self.at).map(PathBuf::as_path)
    }

    /// Moves forward through history or selects the next preset.
    pub fn next(&mut self, hard: bool) {
        if self.at + 1 < self.history.len() {
            self.at += 1;
        } else {
            let Some(pick) = self.pick() else {
                return;
            };
            // Keep the most recent 100 presets.
            if self.history.len() >= 100 {
                self.history.remove(0);
            }
            self.history.push(pick);
            self.at = self.history.len() - 1;
        }
        self.request(!hard);
    }

    /// Back to the preset before this one.
    pub fn previous(&mut self) {
        if self.at == 0 {
            return;
        }
        self.at -= 1;
        self.request(true);
    }

    /// Toggles random and folder order, returning the new random state.
    pub fn toggle_order(&mut self) -> bool {
        self.random = !self.random;
        self.random
    }

    /// A preset chosen at random, not the one playing when there is a
    /// choice; in sequential order, the one after this one.
    fn pick(&self) -> Option<PathBuf> {
        if self.files.is_empty() {
            return None;
        }
        let current = self.current();
        if !self.random {
            let next = current
                .and_then(|path| self.files.iter().position(|file| file == path))
                .map(|at| (at + 1) % self.files.len())
                .unwrap_or(0);
            return Some(self.files[next].clone());
        }
        let mut index = rand::random_range(0..self.files.len());
        if self.files.len() > 1 && Some(self.files[index].as_path()) == current {
            index = (index + 1) % self.files.len();
        }
        Some(self.files[index].clone())
    }

    fn request(&mut self, smooth: bool) {
        if let Some(path) = self.current() {
            self.pending = Some(Request::Load {
                path: path.to_path_buf(),
                smooth,
            });
        }
    }

    /// Reloads the current preset without a fade after the engine restarts.
    pub fn reload_current(&mut self) {
        if self.current().is_some() {
            self.request(false);
        }
    }

    /// Takes the pending engine request.
    pub fn take_request(&mut self) -> Option<Request> {
        self.pending.take()
    }

    /// Downloads one preset pack on a worker thread.
    pub fn download(
        &mut self,
        pack: &'static Pack,
        folder: PathBuf,
        ctx: egui::Context,
        proxy: crate::settings::ProxyConfig,
    ) {
        if self.download.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("milkdrop-presets".into())
            .spawn(move || {
                let _ = sender.send(fetch_pack(pack, &folder, &proxy));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.download = Some((pack.name, receiver)),
            Err(error) => log::warn!("could not start fetching presets: {error}"),
        }
    }

    /// Fetches every pack in one go, for a first open with nothing
    /// there; failures after the first pack still land what arrived.
    pub fn download_missing(
        &mut self,
        folder: PathBuf,
        ctx: egui::Context,
        proxy: crate::settings::ProxyConfig,
    ) {
        if self.download.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("milkdrop-presets".into())
            .spawn(move || {
                let mut total = 0usize;
                let mut failed = None;
                for pack in &PACKS {
                    match fetch_pack(pack, &folder, &proxy) {
                        Ok(count) => total += count,
                        Err(error) => {
                            failed = Some(error);
                            break;
                        }
                    }
                }
                let outcome = match failed {
                    Some(error) if total == 0 => Err(error),
                    _ => Ok(total),
                };
                let _ = sender.send(outcome);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.download = Some(("the preset packs", receiver)),
            Err(error) => log::warn!("could not start fetching presets: {error}"),
        }
    }

    /// The pack on its way, if one is.
    pub fn downloading(&self) -> Option<&'static str> {
        self.download.as_ref().map(|(name, _)| *name)
    }

    /// How a download went, once it has: the number of presets added.
    pub fn poll(&mut self) -> Option<Result<usize, String>> {
        let (_, receiver) = self.download.as_ref()?;
        match receiver.try_recv() {
            Ok(result) => {
                self.download = None;
                // The folder has changed under the listing.
                self.listed = None;
                Some(result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.download = None;
                None
            }
        }
    }
}

/// Every `.milk` file in a folder and the folders inside it, sorted by
/// path without regard to case.
pub fn list_presets(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk(folder, 0, &mut files);
    files.sort_by_cached_key(|path| path.to_string_lossy().to_lowercase());
    files
}

fn walk(folder: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth + 1 < MAX_DEPTH {
                walk(&path, depth + 1, files);
            }
        } else if is_preset(&path) {
            files.push(path);
        }
    }
}

pub fn is_preset(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("milk"))
}

/// Downloads a pack and unpacks its presets into the folder, flat, since
/// the packs keep theirs in folders by style and the names do not clash.
/// Returns how many were written.
pub fn fetch_pack(
    pack: &Pack,
    folder: &Path,
    proxy: &crate::settings::ProxyConfig,
) -> Result<usize, String> {
    let http = crate::http::build_blocking(proxy, Duration::from_secs(300))
        .map_err(|error| error.to_string())?;
    let bytes = http
        .get(pack.url)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.bytes())
        .map_err(|error| format!("{}: {error}", pack.name))?;
    unpack_presets(&bytes, folder)
}

/// Writes the `.milk` files of a zip into the folder.
pub fn unpack_presets(zip: &[u8], folder: &Path) -> Result<usize, String> {
    let archive = crate::skin::zip::Archive::parse(zip).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let mut written = 0;
    for entry in archive.entries() {
        if entry.is_dir() {
            continue;
        }
        let file_name = entry.base_name();
        if file_name.is_empty() || !is_preset(Path::new(file_name)) {
            continue;
        }
        let bytes = archive.read(entry).map_err(|error| error.to_string())?;
        std::fs::write(folder.join(file_name), bytes).map_err(|error| error.to_string())?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spotizgeg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn presets_are_found_in_folders_and_sorted_without_case() {
        let dir = temp_dir("milkdrop-list");
        std::fs::create_dir_all(dir.join("Sparkle")).unwrap();
        std::fs::write(dir.join("zebra.milk"), b"").unwrap();
        std::fs::write(dir.join("Apple.MILK"), b"").unwrap();
        std::fs::write(dir.join("readme.txt"), b"").unwrap();
        std::fs::write(dir.join("Sparkle").join("glow.milk"), b"").unwrap();
        let names: Vec<String> = list_presets(&dir)
            .into_iter()
            .map(|path| {
                path.strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            names,
            ["Apple.MILK", "Sparkle/glow.milk", "zebra.milk"]
                .map(|name| name.replace('/', std::path::MAIN_SEPARATOR_STR))
        );
        assert!(list_presets(Path::new("/nonexistent/milkdrop")).is_empty());
    }

    /// The dial stops at the two rates everyone knows, the screen's own
    /// when it is neither of them, and uncapped, in that order.
    #[test]
    fn the_frame_rate_stops_where_it_is_worth_stopping() {
        assert_eq!(fps_stops(144, 144), vec![30, 60, 144, 0]);
        assert_eq!(fps_stops(0, 60), vec![30, 60, 0], "no screen has said");
        assert_eq!(fps_stops(60, 60), vec![30, 60, 0], "and no stop twice");
        assert_eq!(
            fps_stops(50, 60),
            vec![30, 50, 60, 0],
            "a slower screen takes its place in the order"
        );
        assert_eq!(
            fps_stops(144, 90),
            vec![30, 60, 90, 144, 0],
            "a rate set by hand keeps a stop of its own"
        );
        assert_eq!(fps_label(crate::i18n::Locale::English, 0, 144), "Uncapped");
        assert_eq!(
            fps_label(crate::i18n::Locale::English, 144, 144),
            "144 fps, your screen"
        );
        assert_eq!(fps_label(crate::i18n::Locale::English, 30, 144), "30 fps");
    }

    /// R switches between random and the folder's own order; in order,
    /// the next preset is the next file, wrapping at the end.
    #[test]
    fn sequential_order_walks_the_folder() {
        let mut presets = Presets::new();
        presets.files = vec![
            PathBuf::from("a.milk"),
            PathBuf::from("b.milk"),
            PathBuf::from("c.milk"),
        ];
        assert!(!presets.toggle_order(), "R turns the random order off");
        presets.next(true);
        assert_eq!(presets.current(), Some(Path::new("a.milk")));
        presets.next(true);
        assert_eq!(presets.current(), Some(Path::new("b.milk")));
        presets.next(true);
        assert_eq!(presets.current(), Some(Path::new("c.milk")));
        presets.next(true);
        assert_eq!(
            presets.current(),
            Some(Path::new("a.milk")),
            "the end of the folder wraps to its start"
        );
        assert!(presets.toggle_order(), "R turns it back on");
    }

    #[test]
    fn next_picks_anew_and_previous_walks_back() {
        let mut presets = Presets::new();
        presets.files = vec![PathBuf::from("a.milk"), PathBuf::from("b.milk")];
        assert_eq!(presets.current(), None);
        presets.next(false);
        let first = presets.current().unwrap().to_path_buf();
        assert_eq!(
            presets.take_request(),
            Some(Request::Load {
                path: first.clone(),
                smooth: true
            })
        );
        assert_eq!(presets.take_request(), None);
        // A second pick is never the same one while there is a choice.
        presets.next(true);
        let second = presets.current().unwrap().to_path_buf();
        assert_ne!(first, second);
        assert_eq!(
            presets.take_request(),
            Some(Request::Load {
                path: second.clone(),
                smooth: false
            })
        );
        presets.previous();
        assert_eq!(presets.current(), Some(first.as_path()));
        presets.previous();
        assert_eq!(presets.current(), Some(first.as_path()));
        presets.next(false);
        assert_eq!(presets.current(), Some(second.as_path()));
    }

    #[test]
    fn an_empty_folder_asks_for_nothing() {
        let mut presets = Presets::new();
        presets.next(false);
        assert_eq!(presets.current(), None);
        assert_eq!(presets.take_request(), None);
    }

    #[test]
    fn a_pack_is_unpacked_flat() {
        let dir = temp_dir("milkdrop-pack");
        let zip = crate::skin::zip::write(&[
            (
                "pack-master/Dancer/One.milk",
                b"[preset00]".as_slice(),
                true,
            ),
            ("pack-master/README.md", b"hello".as_slice(), true),
            ("pack-master/Two.milk", b"[preset00]".as_slice(), false),
        ]);
        assert_eq!(unpack_presets(&zip, &dir), Ok(2));
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(names, ["One.milk", "Two.milk"]);
    }
}
