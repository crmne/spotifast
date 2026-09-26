//! Winamp window, skin, and display state. Drawing lives in `ui::winamp`.
//!
//! Skins are loaded from the config directory on a worker thread. The built-in
//! skin is immediately available. `App::tick` keeps the loaded skin in sync
//! with settings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::settings::Settings;
use crate::skin::{Sheet, Skin, SkinError};
use crate::vis::{Analyser, AudioTap};

/// The most screen pixels a skin pixel may take.
pub const MAX_SCALE: u32 = 4;
/// How many characters the marquee shows: thirty whole ones and the edge
/// of a thirty-first.
pub const MARQUEE_CHARS: usize = 31;
/// Text this long fits the marquee without scrolling.
const MARQUEE_FITS: usize = 30;
/// How long the marquee waits between one-character steps.
const MARQUEE_STEP: Duration = Duration::from_millis(220);
/// What separates the end of a scrolling title from its start again.
const MARQUEE_GAP: &str = "  ***  ";

/// The text with the gap it scrolls through, for drawing it as pixels.
pub fn marquee_strip(text: &str) -> String {
    format!("{text}{MARQUEE_GAP}")
}
/// How long a listing of the skins folder is trusted.
const CHOICES_FRESH: Duration = Duration::from_secs(5);
/// File extensions a skin archive may carry.
const ARCHIVE_EXTENSIONS: [&str; 2] = ["wsz", "zip"];

/// A skin in the skins folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkinChoice {
    /// The file or folder name, which is what the settings store.
    pub name: String,
    pub path: PathBuf,
}

impl SkinChoice {
    /// The name without its extension, for showing.
    pub fn label(&self) -> &str {
        label(&self.name)
    }
}

/// A skin's name without the archive extension, for showing.
pub fn label(name: &str) -> &str {
    name.rsplit_once('.')
        .filter(|(_, extension)| is_archive_extension(extension))
        .map_or(name, |(stem, _)| stem)
}

/// What a loading thread reports back.
pub struct Loaded {
    /// The name the settings hold for this skin.
    pub name: String,
    pub result: Result<Skin, SkinError>,
    /// Whether the skin was just copied into the folder.
    pub installed: bool,
}

pub struct WinampState {
    /// The skin on screen.
    pub skin: Arc<Skin>,
    /// Active skin setting. `None` means the built-in skin.
    pub worn: Option<String>,
    /// The skin's sheets on the graphics card, made on demand and dropped
    /// with the window, since textures belong to a window's context.
    textures: HashMap<Sheet, egui::TextureHandle>,
    loading: Option<mpsc::Receiver<Loaded>>,
    /// The skins folder's contents, as last listed.
    pub choices: Vec<SkinChoice>,
    choices_listed: Option<Instant>,
    /// Count down instead of up; clicking the time toggles it.
    pub time_remaining: bool,
    /// The balance while its thumb is held, for the marquee to report.
    pub balance_preview: Option<f32>,
    /// Open the equalizer's presets menu on the next frame; the demo's
    /// `presets` surface asks for it.
    pub open_presets: bool,
    marquee_text: String,
    marquee_offset: usize,
    marquee_moved: Option<Instant>,
    /// Where the window opens, from the session or from where it last was.
    pub restore_pos: Option<[f32; 2]>,
    /// Where the window is, as last seen.
    pub last_pos: Option<[f32; 2]>,
    /// The sound on its way out, for the visualiser.
    pub tap: Arc<AudioTap>,
    pub analyser: Analyser,
    /// The equalizer as the player's thread reads it.
    pub eq: crate::eq::SharedEq,
    /// The playlist window: its first visible row, the row clicked, the
    /// wheel's leftover, and the corner drag's leftover.
    pub playlist_scroll: usize,
    /// The rows selected, by URI; Ctrl-click adds and removes, SEL has
    /// the rest.
    pub playlist_selection: std::collections::HashSet<usize>,
    pub playlist_wheel: f32,
    pub playlist_resize: f32,
    /// The playlist's rows as Winamp drew them, kept once drawn.
    pub playlist_text: crate::ui::winamp::PixelText,
    /// MilkDrop presets listed and downloaded from Settings.
    pub presets: crate::milkdrop::Presets,
}

impl WinampState {
    pub fn new(restore_pos: Option<[f32; 2]>, tap: Arc<AudioTap>, eq: crate::eq::SharedEq) -> Self {
        Self {
            skin: Skin::builtin(),
            worn: None,
            textures: HashMap::new(),
            loading: None,
            choices: Vec::new(),
            choices_listed: None,
            time_remaining: false,
            balance_preview: None,
            open_presets: false,
            marquee_text: String::new(),
            marquee_offset: 0,
            marquee_moved: None,
            restore_pos,
            last_pos: None,
            tap,
            analyser: Analyser::default(),
            eq,
            playlist_scroll: 0,
            playlist_selection: std::collections::HashSet::new(),
            playlist_wheel: 0.0,
            playlist_resize: 0.0,
            playlist_text: crate::ui::winamp::PixelText::default(),
            presets: crate::milkdrop::Presets::new(),
        }
    }

    /// Screen pixels per skin pixel: the setting, or else double size on
    /// this display, which is the size people remember Winamp at.
    pub fn scale(settings: &Settings, pixels_per_point: f32) -> u32 {
        let chosen = settings
            .skin_scale
            .map(u32::from)
            .unwrap_or_else(|| (2.0 * pixels_per_point).round() as u32);
        chosen.clamp(1, MAX_SCALE)
    }

    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Puts a skin on. The textures are remade from it at the next frame.
    pub fn wear(&mut self, name: Option<String>, skin: Arc<Skin>) {
        self.skin = skin;
        self.worn = name;
        self.textures.clear();
    }

    /// The window is gone (or about to be), and with it the textures.
    pub fn forget_textures(&mut self) {
        self.textures.clear();
        self.playlist_text.clear();
    }

    /// Keeps where the window was, for the next time it opens.
    pub fn remember_position(&mut self) {
        if let Some(pos) = self.last_pos {
            self.restore_pos = Some(pos);
        }
    }

    /// The skin's sheets as textures, made now if they are not yet.
    pub fn textures(&mut self, ctx: &egui::Context) -> HashMap<Sheet, egui::TextureId> {
        for sheet in Sheet::ALL {
            if self.textures.contains_key(&sheet) {
                continue;
            }
            let bitmap = self.skin.sheet(sheet);
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [bitmap.width as usize, bitmap.height as usize],
                &bitmap.rgba,
            );
            let handle = ctx.load_texture(
                format!("winamp-{}", sheet.file_stem()),
                image,
                egui::TextureOptions::NEAREST,
            );
            self.textures.insert(sheet, handle);
        }
        self.textures
            .iter()
            .map(|(sheet, handle)| (*sheet, handle.id()))
            .collect()
    }

    /// Reads a skin from the folder on another thread.
    pub fn load(&mut self, name: String, folder: &Path, ctx: &egui::Context) {
        let path = folder.join(&name);
        self.spawn(ctx, move || Loaded {
            result: Skin::load(&path),
            name,
            installed: false,
        });
    }

    /// Reads a skin from anywhere and, if it is one, copies it into the
    /// folder, on another thread.
    pub fn install(&mut self, file: PathBuf, folder: &Path, ctx: &egui::Context) {
        let folder = folder.to_path_buf();
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.spawn(ctx, move || {
            let result = Skin::load(&file).and_then(|skin| {
                let destination = folder.join(&name);
                if destination != file {
                    std::fs::create_dir_all(&folder)?;
                    std::fs::copy(&file, &destination)?;
                }
                Ok(skin)
            });
            Loaded {
                name,
                result,
                installed: true,
            }
        });
    }

    /// Starts a job; one already running is left to finish unheard, so the
    /// latest request is the one that counts.
    fn spawn(&mut self, ctx: &egui::Context, job: impl FnOnce() -> Loaded + Send + 'static) {
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("skin-loader".into())
            .spawn(move || {
                let _ = sender.send(job());
                ctx.request_repaint();
            });
        if let Err(error) = spawned {
            log::warn!("could not start reading the skin: {error}");
        }
        self.loading = Some(receiver);
    }

    /// What a loading thread has finished with, if anything.
    pub fn poll(&mut self) -> Option<Loaded> {
        let receiver = self.loading.as_ref()?;
        match receiver.try_recv() {
            Ok(loaded) => {
                self.loading = None;
                Some(loaded)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.loading = None;
                None
            }
        }
    }

    /// Lists the folder again if the listing is old.
    pub fn refresh_choices(&mut self, folder: &Path) {
        if self
            .choices_listed
            .is_none_or(|at| at.elapsed() > CHOICES_FRESH)
        {
            self.list_choices(folder);
        }
    }

    /// Lists the folder: `.wsz` files and folders with a `main.bmp`.
    pub fn list_choices(&mut self, folder: &Path) {
        self.choices_listed = Some(Instant::now());
        self.choices = list_skins(folder);
    }

    /// The characters the marquee shows now: the text itself when it
    /// fits, otherwise a window onto it that moves a character at a time.
    pub fn marquee(&mut self, text: &str, now: Instant) -> (String, usize) {
        if text != self.marquee_text {
            self.marquee_text = text.to_string();
            self.marquee_offset = 0;
            self.marquee_moved = Some(now);
        }
        let mut chars: Vec<char> = self.marquee_text.chars().collect();
        if chars.len() <= MARQUEE_FITS {
            return (self.marquee_text.clone(), 0);
        }
        chars.extend(MARQUEE_GAP.chars());
        let moved = self.marquee_moved.get_or_insert(now);
        let steps =
            (now.saturating_duration_since(*moved).as_millis() / MARQUEE_STEP.as_millis()) as usize;
        if steps > 0 {
            self.marquee_offset = self.marquee_offset.wrapping_add(steps);
            *moved += MARQUEE_STEP * steps as u32;
        }
        let offset = self.marquee_offset;
        let shown = (0..MARQUEE_CHARS)
            .map(|index| chars[(offset + index) % chars.len()])
            .collect();
        (shown, offset)
    }

    /// Whether the marquee is on the move, and so wants frames.
    pub fn marquee_scrolling(&self) -> bool {
        self.marquee_text.chars().count() > MARQUEE_FITS
    }
}

fn is_archive_extension(extension: &str) -> bool {
    ARCHIVE_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

/// Whether a dropped file could be a skin, by its name.
pub fn is_skin_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(is_archive_extension)
}

/// The skins in a folder, by name.
pub fn list_skins(folder: &Path) -> Vec<SkinChoice> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut skins: Vec<SkinChoice> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_skin = if path.is_dir() {
                has_main_bitmap(&path)
            } else {
                is_skin_file(&path)
            };
            is_skin.then_some(SkinChoice { name, path })
        })
        .collect();
    skins.sort_by_key(|skin| skin.name.to_lowercase());
    skins
}

fn has_main_bitmap(folder: &Path) -> bool {
    std::fs::read_dir(folder).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            name == "main.bmp" || name == "main.png"
        })
    })
}

/// A skin other than `current`, at random, from `candidates` (`None` is the
/// built-in skin). With nothing else to pick, `current` stays.
pub fn pick_another(
    candidates: &[Option<String>],
    current: &Option<String>,
    rng: &mut impl rand::Rng,
) -> Option<String> {
    let others: Vec<&Option<String>> = candidates
        .iter()
        .filter(|candidate| *candidate != current)
        .collect();
    if others.is_empty() {
        return current.clone();
    }
    others[rng.random_range(0..others.len())].clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spotifast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_size_defaults_to_double_on_this_display() {
        let mut settings = Settings::default();
        assert_eq!(WinampState::scale(&settings, 1.0), 2);
        assert_eq!(WinampState::scale(&settings, 2.0), 4);
        assert_eq!(WinampState::scale(&settings, 1.2), 2);
        assert_eq!(WinampState::scale(&settings, 1.5), 3);
        settings.skin_scale = Some(1);
        assert_eq!(WinampState::scale(&settings, 2.0), 1);
        settings.skin_scale = Some(9);
        assert_eq!(WinampState::scale(&settings, 1.0), MAX_SCALE);
    }

    #[test]
    fn a_short_title_sits_still_and_a_long_one_scrolls() {
        let mut state = WinampState::new(None, AudioTap::new(), crate::eq::shared());
        let start = Instant::now();
        assert_eq!(state.marquee("Spotifast", start).0, "Spotifast");
        assert!(!state.marquee_scrolling());

        let long = "Radiohead - Everything In Its Right Place (4:11)";
        let (first, offset) = state.marquee(long, start);
        assert_eq!(first.chars().count(), MARQUEE_CHARS);
        assert_eq!(offset, 0);
        assert!(long.starts_with(&first));
        assert!(state.marquee_scrolling());
        // Not yet time to move.
        assert_eq!(
            state.marquee(long, start + Duration::from_millis(100)).0,
            first
        );
        let (later, stepped) = state.marquee(long, start + MARQUEE_STEP);
        assert!(long[1..].starts_with(&later));
        assert_eq!(stepped, 1);
        // Eventually the start comes round again, after the gap.
        let round = long.chars().count() + MARQUEE_GAP.len();
        let again = state.marquee(long, start + MARQUEE_STEP * round as u32).0;
        assert_eq!(again, first);
        // A new title starts from its beginning.
        let other = "Something else entirely, and just as long as before";
        assert!(other.starts_with(&state.marquee(other, start + Duration::from_secs(9)).0));
    }

    #[test]
    fn the_folder_lists_archives_and_unpacked_skins_by_name() {
        let dir = temp_dir("skins");
        std::fs::write(dir.join("Zaxon.WSZ"), b"").unwrap();
        std::fs::write(dir.join("base.wsz"), b"").unwrap();
        std::fs::write(dir.join("readme.txt"), b"").unwrap();
        std::fs::create_dir_all(dir.join("Unpacked")).unwrap();
        std::fs::write(dir.join("Unpacked").join("MAIN.BMP"), b"").unwrap();
        std::fs::create_dir_all(dir.join("Empty folder")).unwrap();
        let names: Vec<String> = list_skins(&dir).into_iter().map(|skin| skin.name).collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(names, ["base.wsz", "Unpacked", "Zaxon.WSZ"]);
    }

    #[test]
    fn labels_drop_the_archive_extension_only() {
        let choice = |name: &str| SkinChoice {
            name: name.to_string(),
            path: PathBuf::new(),
        };
        assert_eq!(choice("Zaxon Remake 1.0.wsz").label(), "Zaxon Remake 1.0");
        assert_eq!(choice("Unpacked").label(), "Unpacked");
        assert_eq!(choice("v2.5").label(), "v2.5");
    }

    #[test]
    fn a_missing_folder_lists_nothing() {
        assert!(list_skins(Path::new("/nonexistent/skins")).is_empty());
        assert!(is_skin_file(Path::new("/tmp/x.WSZ")));
        assert!(!is_skin_file(Path::new("/tmp/x.bmp")));
    }

    /// Random never repeats the skin it replaces, reaches every other one,
    /// and keeps the only skin there is.
    #[test]
    fn a_random_skin_is_never_the_one_it_replaces() {
        let candidates = [None, Some("A.wsz".to_owned()), Some("B".to_owned())];
        let mut rng = rand::rng();
        let current = Some("A.wsz".to_owned());
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let picked = pick_another(&candidates, &current, &mut rng);
            assert_ne!(picked, current);
            seen.insert(picked);
        }
        assert_eq!(seen.len(), 2, "both others are reachable");
        assert_eq!(pick_another(&[None], &None, &mut rng), None);
        assert_eq!(
            pick_another(&[], &Some("gone".into()), &mut rng),
            Some("gone".into())
        );
    }
}
