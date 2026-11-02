//! Winamp window, skin, and display state. Drawing lives in `ui::winamp`.
//!
//! Skins are loaded from the config directory on a worker thread. The built-in
//! skin is immediately available. `App::tick` keeps the loaded skin in sync
//! with settings.

use std::collections::{HashMap, VecDeque};
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
/// Where a skin's own preferences are kept: beside the skins, in a folder
/// no skin listing looks in.
pub fn skin_preferences_path(folder: &Path, name: &str) -> PathBuf {
    let parent = folder.parent().unwrap_or(folder);
    parent.join("skin-preferences").join(format!("{name}.json"))
}

fn read_preferences(path: &Path) -> HashMap<String, String> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// File extensions a skin archive may carry.
const ARCHIVE_EXTENSIONS: [&str; 3] = ["wsz", "wmz", "zip"];

/// Which player a skin is made for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkinKind {
    Winamp,
    WindowsMediaPlayer,
}

/// A skin in the skins folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkinChoice {
    /// The file or folder name, which is what the settings store.
    pub name: String,
    pub path: PathBuf,
    pub kind: SkinKind,
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
    /// What a Windows Media Player skin saved last time.
    pub preferences: HashMap<String, String>,
}

struct WmpTexture {
    handle: egui::TextureHandle,
    _picture: Arc<crate::skin::wmp::picture::Picture>,
    bytes: usize,
    used: u64,
}

/// The most a skin's pictures may take on the graphics card, in bytes.
const MAX_WMP_TEXTURES: usize = 192 * 1024 * 1024;

pub struct WinampState {
    /// The skin on screen.
    pub skin: Arc<Skin>,
    /// Active skin setting. `None` means the built-in skin.
    pub worn: Option<String>,
    /// The skin's sheets on the graphics card, made on demand and dropped
    /// with the window, since textures belong to a window's context.
    textures: HashMap<Sheet, egui::TextureHandle>,
    /// Scripts the demo runs in the skin once it has started.
    #[cfg(feature = "demo")]
    pub demo_scripts: Vec<String>,
    /// Keep the tap fed, so that MilkDrop has the demo's sound to answer.
    #[cfg(feature = "demo")]
    pub demo_sounding: bool,
    /// The running Windows Media Player skin.
    pub runtime: Option<crate::skin::wmp::runtime::Runtime>,
    /// Its saved preferences, read with the skin, until it starts.
    skin_preferences: HashMap<String, String>,
    /// Its pictures' frames on the graphics card, keyed by which picture
    /// and frame; holding the picture keeps its address from being reused.
    wmp_textures: HashMap<(usize, usize), WmpTexture>,
    wmp_texture_bytes: usize,
    wmp_texture_clock: u64,
    loading: Option<mpsc::Receiver<Loaded>>,
    /// The skins folder's contents, as last listed.
    pub choices: Vec<SkinChoice>,
    choices_listed: Option<Instant>,
    /// A listing still being made on its own thread.
    listing: Option<mpsc::Receiver<Vec<SkinChoice>>>,
    /// Whether `choices` holds a finished listing yet.
    choices_known: bool,
    /// Counts the listings, so the previews see each new one.
    choices_generation: u64,
    /// Pictures of the skins, for their cards in Settings.
    pub previews: crate::skin_previews::SkinPreviews,
    /// What the demo types in the Settings filter.
    #[cfg(feature = "demo")]
    pub demo_settings_filter: Option<String>,
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
    /// Where the window is, or was last: from the session, then followed
    /// while it is open. Only the position is given to the next window,
    /// which takes its size from the skin.
    pub geometry: fastframe_shell::window::Geometry,
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
    /// The same for the MilkDrop panel.
    pub milkdrop_resize: f32,
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
            #[cfg(feature = "demo")]
            demo_scripts: Vec::new(),
            #[cfg(feature = "demo")]
            demo_sounding: false,
            runtime: None,
            skin_preferences: HashMap::new(),
            wmp_textures: HashMap::new(),
            wmp_texture_bytes: 0,
            wmp_texture_clock: 0,
            loading: None,
            choices: Vec::new(),
            choices_listed: None,
            listing: None,
            choices_known: false,
            choices_generation: 0,
            previews: crate::skin_previews::SkinPreviews::default(),
            #[cfg(feature = "demo")]
            demo_settings_filter: None,
            time_remaining: false,
            balance_preview: None,
            open_presets: false,
            marquee_text: String::new(),
            marquee_offset: 0,
            marquee_moved: None,
            geometry: fastframe_shell::window::Geometry {
                position: restore_pos,
                ..Default::default()
            },
            tap,
            analyser: Analyser::default(),
            eq,
            playlist_scroll: 0,
            playlist_selection: std::collections::HashSet::new(),
            playlist_wheel: 0.0,
            playlist_resize: 0.0,
            milkdrop_resize: 0.0,
            playlist_text: crate::ui::winamp::PixelText::default(),
            presets: crate::milkdrop::Presets::new(),
        }
    }

    /// Screen pixels per skin pixel, for everything that draws or sizes a
    /// skin: the setting, or else double size on this display for a Winamp
    /// skin, which is the size people remember it at. A Windows Media
    /// Player skin is made for a screen of its day, so by default one of
    /// its pixels is one pixel of this display.
    pub fn scale(settings: &Settings, pixels_per_point: f32, skin: &Skin) -> u32 {
        let default = if skin.wmp.is_some() {
            pixels_per_point
        } else {
            2.0 * pixels_per_point
        };
        let chosen = settings
            .skin_scale
            .map(u32::from)
            .unwrap_or_else(|| default.round() as u32);
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
        self.wmp_textures.clear();
        self.wmp_texture_bytes = 0;
    }

    /// The window is gone (or about to be), and with it the textures.
    pub fn forget_textures(&mut self) {
        self.textures.clear();
        self.wmp_textures.clear();
        self.wmp_texture_bytes = 0;
        self.playlist_text.clear();
        self.previews.forget_textures();
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

    /// A frame of a skin's picture as a texture, made now if it is not yet.
    pub fn wmp_texture(
        &mut self,
        ctx: &egui::Context,
        picture: &Arc<crate::skin::wmp::picture::Picture>,
        frame: usize,
    ) -> egui::TextureId {
        self.wmp_texture_clock += 1;
        let clock = self.wmp_texture_clock;
        let key = (Arc::as_ptr(picture) as usize, frame);
        if let Some(texture) = self.wmp_textures.get_mut(&key) {
            texture.used = clock;
            return texture.handle.id();
        }
        let bitmap = &picture.frames[frame].bitmap;
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [bitmap.width as usize, bitmap.height as usize],
            &bitmap.rgba,
        );
        let handle = ctx.load_texture("wmp-picture", image, egui::TextureOptions::NEAREST);
        let id = handle.id();
        self.wmp_texture_bytes += bitmap.rgba.len();
        self.wmp_textures.insert(
            key,
            WmpTexture {
                handle,
                _picture: picture.clone(),
                bytes: bitmap.rgba.len(),
                used: clock,
            },
        );
        id
    }

    /// Lets go of the longest unused frames once too many are held: a skin
    /// that plays an intro of hundreds of pictures needs them only once.
    pub fn trim_wmp_textures(&mut self) {
        while self.wmp_texture_bytes > MAX_WMP_TEXTURES && self.wmp_textures.len() > 1 {
            let Some(oldest) = self
                .wmp_textures
                .iter()
                .min_by_key(|(_, texture)| texture.used)
                .map(|(key, _)| *key)
            else {
                return;
            };
            if let Some(texture) = self.wmp_textures.remove(&oldest) {
                self.wmp_texture_bytes = self.wmp_texture_bytes.saturating_sub(texture.bytes);
            }
        }
    }

    /// The running skin's saved preferences, to hand it as it starts.
    pub fn take_skin_preferences(&mut self) -> HashMap<String, String> {
        std::mem::take(&mut self.skin_preferences)
    }

    pub fn set_skin_preferences(&mut self, preferences: HashMap<String, String>) {
        self.skin_preferences = preferences;
    }

    /// Reads a skin from the folder on another thread.
    pub fn load(&mut self, name: String, folder: &Path, ctx: &egui::Context) {
        let path = folder.join(&name);
        let preferences = skin_preferences_path(folder, &name);
        self.spawn(ctx, move || Loaded {
            result: Skin::shipped(&name).unwrap_or_else(|| Skin::load(&path)),
            preferences: read_preferences(&preferences),
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
                preferences: HashMap::new(),
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
    pub fn refresh_choices(&mut self, folder: &Path, ctx: &egui::Context) {
        if self.listing.is_none()
            && self
                .choices_listed
                .is_none_or(|at| at.elapsed() > CHOICES_FRESH)
        {
            self.list_choices(folder, ctx);
        }
    }

    /// Lists the folder on another thread, since telling the kinds of skin
    /// apart opens every archive: [`Self::poll_choices`] takes the answer.
    /// One already running is left to finish unheard.
    pub fn list_choices(&mut self, folder: &Path, ctx: &egui::Context) {
        self.choices_listed = Some(Instant::now());
        let (sender, receiver) = mpsc::channel();
        let folder = folder.to_path_buf();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("skin-lister".into())
            .spawn(move || {
                let _ = sender.send(with_shipped_skins(list_skins(&folder)));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.listing = Some(receiver),
            Err(error) => log::warn!("could not start listing the skins: {error}"),
        }
    }

    /// Takes a finished listing, if there is one.
    pub fn poll_choices(&mut self) {
        let Some(receiver) = self.listing.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(choices) => {
                self.choices = choices;
                self.choices_known = true;
                self.choices_generation += 1;
                self.listing = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.listing = None,
        }
    }

    /// Which listing `choices` is, counted from the first.
    pub fn choices_generation(&self) -> u64 {
        self.choices_generation
    }

    /// Whether a listing has come back since the app started.
    pub fn choices_known(&self) -> bool {
        self.choices_known
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
            let kind = if path.is_dir() {
                folder_kind(&path)
            } else if is_skin_file(&path) {
                Some(archive_kind(&path))
            } else {
                None
            };
            kind.map(|kind| SkinChoice { name, path, kind })
        })
        .collect();
    skins.sort_by_key(|skin| skin.name.to_lowercase());
    skins
}

/// What is in the skins folder, after what comes with the app.
fn with_shipped_skins(listed: Vec<SkinChoice>) -> Vec<SkinChoice> {
    let shipped = crate::skin::SHIPPED.map(|name| SkinChoice {
        name: name.to_owned(),
        path: PathBuf::new(),
        kind: SkinKind::WindowsMediaPlayer,
    });
    let listed = listed
        .into_iter()
        .filter(|skin| !crate::skin::SHIPPED.contains(&skin.name.as_str()));
    shipped.into_iter().chain(listed).collect()
}

/// What an archive holds, by its file names alone. One that cannot be read
/// is listed as a Winamp skin, and says what is wrong when it is chosen.
fn archive_kind(path: &Path) -> SkinKind {
    match crate::skin::zip::file_names(path) {
        Ok(names) if crate::skin::is_media_player_skin(names.iter().map(String::as_str)) => {
            SkinKind::WindowsMediaPlayer
        }
        _ => SkinKind::Winamp,
    }
}

/// What an unpacked skin is, or `None` when it has no main window bitmap
/// or definition, looked for the way [`Skin::from_dir`] reads one: in nested
/// folders too, down to the same depth, without following links. A Winamp
/// main window wins over a definition, as it does when the skin is read.
fn folder_kind(folder: &Path) -> Option<SkinKind> {
    let mut definition = false;
    let mut folders = VecDeque::from([(folder.to_path_buf(), 0)]);
    while let Some((folder, depth)) = folders.pop_front() {
        let Ok(entries) = std::fs::read_dir(folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth < crate::skin::MAX_SKIN_DEPTH {
                folders.push_back((entry.path(), depth + 1));
            } else if kind.is_file() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if name == "main.bmp" || name == "main.png" {
                    // Nothing found later can change the answer.
                    return Some(SkinKind::Winamp);
                }
                definition |= name.ends_with(".wms");
            }
        }
    }
    definition.then_some(SkinKind::WindowsMediaPlayer)
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
        let skin = Skin::builtin();
        assert_eq!(WinampState::scale(&settings, 1.0, &skin), 2);
        assert_eq!(WinampState::scale(&settings, 2.0, &skin), 4);
        assert_eq!(WinampState::scale(&settings, 1.2, &skin), 2);
        assert_eq!(WinampState::scale(&settings, 1.5, &skin), 3);
        settings.skin_scale = Some(1);
        assert_eq!(WinampState::scale(&settings, 2.0, &skin), 1);
        settings.skin_scale = Some(9);
        assert_eq!(WinampState::scale(&settings, 1.0, &skin), MAX_SCALE);
    }

    #[test]
    fn a_windows_media_player_skin_defaults_to_one_pixel_and_shares_the_size_setting() {
        let mut settings = Settings::default();
        let winamp = Skin::builtin();
        let archive = crate::skin::zip::write(&[(
            "player.wms",
            br#"<VIEW width="30" height="20"/>"#.as_slice(),
            false,
        )]);
        let player = Skin::from_archive("player", &archive).unwrap();
        assert!(player.wmp.is_some());
        assert_eq!(WinampState::scale(&settings, 1.0, &player), 1);
        assert_eq!(WinampState::scale(&settings, 2.0, &player), 2);
        assert_eq!(WinampState::scale(&settings, 1.0, &winamp), 2);
        // A stored size means the same for both kinds.
        settings.skin_scale = Some(3);
        assert_eq!(WinampState::scale(&settings, 1.0, &player), 3);
        assert_eq!(WinampState::scale(&settings, 1.0, &winamp), 3);
        assert_eq!(
            crate::ui::winamp::initial_size(&settings, &player),
            egui::vec2(90.0, 60.0)
        );
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

    /// A skin unpacked with its files a folder or more down, as Extract All
    /// leaves an archive that keeps them in a folder, is listed: the reader
    /// finds them there, down to the same depth.
    #[test]
    fn a_skin_unpacked_into_nested_folders_is_listed() {
        let dir = temp_dir("nested-skins");
        let inner = dir.join("Extracted").join("Some Skin");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("MAIN.BMP"), b"").unwrap();
        let mut too_deep = dir.join("Too deep");
        for _ in 0..=crate::skin::MAX_SKIN_DEPTH {
            too_deep = too_deep.join("nested");
        }
        std::fs::create_dir_all(&too_deep).unwrap();
        std::fs::write(too_deep.join("main.bmp"), b"").unwrap();
        let names: Vec<String> = list_skins(&dir).into_iter().map(|skin| skin.name).collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(names, ["Extracted"]);
    }

    #[test]
    fn labels_drop_the_archive_extension_only() {
        let choice = |name: &str| SkinChoice {
            name: name.to_string(),
            path: PathBuf::new(),
            kind: SkinKind::Winamp,
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
