//! The pictures on the skin cards in Settings.
//!
//! Previews are drawn once and kept as PNGs in the cache folder, named by a
//! hash of the skin's file name, size and modification time, so a skin that
//! changes gets a new one and old ones are swept away. Nothing here touches
//! the disk or a skin on the window's thread: one thread draws, one skin at a
//! time, whatever is on screen first, and only while cards are on screen; a
//! second turns stored PNGs into pixels, for cards on screen only, so that it
//! never waits behind a slow skin. The window's thread makes textures of a
//! few of those each frame and keeps a bounded number of them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, UNIX_EPOCH};

use egui::{ColorImage, TextureHandle, TextureOptions};

use crate::skin::{Bitmap, Skin, preview};
use crate::winamp::SkinChoice;

/// Changes when the way previews are drawn does, so old ones are redrawn.
const VERSION: u32 = 2;
/// Tries a skin gets. A mark of this many bytes means there is no picture to
/// be had; fewer say a try never finished, because the app was closed or
/// brought down meanwhile.
const MAX_TRIES: usize = 3;
/// The card of the built-in skin, which is no file.
pub const BUILT_IN: &str = "";
/// Textures kept: more than fit on the largest screen, and at most 512 KB
/// each, as a stored preview is 480 pixels at the longest.
const MAX_TEXTURES: usize = 160;
/// Textures made in one frame, so that scrolling to many at once stays smooth.
const UPLOADS_PER_FRAME: usize = 3;
/// How long drawing carries on after the last card was on screen.
const STAY_ACTIVE: Duration = Duration::from_secs(5);
/// Pictures decoded and waiting for the window to take them.
const DECODED_WAITING: usize = 4;
/// Scripts recurse on the stack, and the window's thread has this much.
const DRAWING_STACK: usize = 8 * 1024 * 1024;
const TEXTURE_OPTIONS: TextureOptions = TextureOptions {
    magnification: egui::TextureFilter::Nearest,
    minification: egui::TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: None,
};

/// A skin to draw: its name, and its file unless it is the built-in skin.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Job {
    name: String,
    path: Option<PathBuf>,
    stem: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Update {
    Ready { name: String, file: PathBuf },
    Failed { name: String },
    Gone { name: String },
}

struct Decoded {
    name: String,
    file: PathBuf,
    image: Option<ColorImage>,
}

#[derive(Default)]
struct Work {
    /// A fresh listing of the skins folder.
    listing: Option<Vec<(String, PathBuf)>>,
    queue: VecDeque<Job>,
    /// The names on screen, which go first.
    front: Vec<String>,
    /// Stored previews to decode, for cards on screen.
    decode: Vec<(String, PathBuf)>,
    /// When a card was last on screen.
    seen: Option<Instant>,
    closed: bool,
}

impl Work {
    fn next_job(&mut self, now: Instant) -> Option<Job> {
        if !self
            .seen
            .is_some_and(|seen| now.saturating_duration_since(seen) < STAY_ACTIVE)
        {
            return None;
        }
        let first = self
            .front
            .iter()
            .find_map(|name| self.queue.iter().position(|job| job.name == *name));
        match first {
            Some(at) => self.queue.remove(at),
            None => self.queue.pop_front(),
        }
    }
}

struct Link {
    work: Mutex<Work>,
    changed: Condvar,
}

impl Link {
    /// The lock, even if a thread died holding it: nothing in it can be left
    /// half changed in a way that matters.
    fn work(&self) -> MutexGuard<'_, Work> {
        self.work
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// The name a skin's preview is stored under, without an extension.
fn stem_of(name: &str, size: u64, modified: u128) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (VERSION, name, size, modified).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn stem_of_path(name: &str, path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    Some(stem_of(name, metadata.len(), modified))
}

/// The skins that come with the app change only with the app.
fn built_in_jobs() -> [Job; 3] {
    let [ribbon, wide] = crate::skin::SHIPPED;
    [BUILT_IN, ribbon, wide].map(|name| Job {
        name: name.to_owned(),
        path: None,
        stem: stem_of(&format!("{}{name}", env!("CARGO_PKG_VERSION")), 0, 0),
    })
}

/// Draws a skin and stores the result. A mark goes down first and comes up
/// after, so that a skin which keeps bringing the app down stops being tried,
/// while one that was only cut short by closing the app gets another go.
fn draw(job: &Job, cache: &Path) -> Option<PathBuf> {
    let png = cache.join(format!("{}.png", job.stem));
    let failed = cache.join(format!("{}.none", job.stem));
    if png.exists() {
        return Some(png);
    }
    let tries = std::fs::metadata(&failed).map_or(0, |mark| mark.len() as usize);
    if tries >= MAX_TRIES {
        return None;
    }
    let mark = |length: usize| {
        if let Err(error) = fastframe_file::write(&failed, vec![0; length]) {
            log::debug!("could not mark a skin preview: {error}");
        }
    };
    mark(tries + 1);
    let started = Instant::now();
    let picture = match &job.path {
        Some(path) => preview::of_file(path),
        None => match Skin::shipped(&job.name) {
            Some(skin) => skin.ok().and_then(|skin| preview::of_skin(&skin)),
            None => preview::of_skin(&Skin::builtin()),
        },
    };
    log::debug!(
        "skin preview of {:?} took {:?}",
        job.name,
        started.elapsed()
    );
    let Some(bytes) = picture.as_ref().and_then(preview::encode) else {
        mark(MAX_TRIES);
        return None;
    };
    if let Err(error) = fastframe_file::write(&png, bytes) {
        log::warn!("could not keep a skin preview: {error}");
        return None;
    }
    let _ = std::fs::remove_file(&failed);
    Some(png)
}

/// Takes in a listing: what is new or changed goes on the queue, what left
/// is reported, and files no skin wants any more are removed.
fn take_listing(
    listing: Vec<(String, PathBuf)>,
    known: &mut HashMap<String, String>,
    link: &Link,
    cache: &Path,
    updates: &Sender<Update>,
) {
    let mut jobs = Vec::new();
    let mut current = HashSet::new();
    for (name, path) in listing {
        if crate::skin::SHIPPED.contains(&name.as_str()) {
            continue;
        }
        let Some(stem) = stem_of_path(&name, &path) else {
            continue;
        };
        current.insert(name.clone());
        if known.get(&name) != Some(&stem) {
            known.insert(name.clone(), stem.clone());
            jobs.push(Job {
                name,
                path: Some(path),
                stem,
            });
        }
    }
    known.retain(|name, _| {
        let stays = name.as_str() == BUILT_IN
            || crate::skin::SHIPPED.contains(&name.as_str())
            || current.contains(name);
        if !stays {
            let _ = updates.send(Update::Gone { name: name.clone() });
        }
        stays
    });
    {
        let mut work = link.work();
        work.queue
            .retain(|job| known.get(&job.name) == Some(&job.stem));
        work.queue.extend(jobs);
    }
    let wanted: HashSet<&str> = known.values().map(String::as_str).collect();
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let stale = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| !wanted.contains(stem));
        if stale {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn drawing_thread(link: Arc<Link>, cache: PathBuf, updates: Sender<Update>, ctx: egui::Context) {
    let mut known = HashMap::new();
    for job in built_in_jobs() {
        known.insert(job.name.clone(), job.stem.clone());
        link.work().queue.push_front(job);
    }
    loop {
        let mut work = link.work();
        let job = loop {
            if work.closed {
                return;
            }
            if let Some(listing) = work.listing.take() {
                drop(work);
                take_listing(listing, &mut known, &link, &cache, &updates);
                ctx.request_repaint();
                work = link.work();
                continue;
            }
            if let Some(job) = work.next_job(Instant::now()) {
                break job;
            }
            work = link
                .changed
                .wait(work)
                .unwrap_or_else(|poison| poison.into_inner());
        };
        drop(work);
        let name = job.name.clone();
        let update = match draw(&job, &cache) {
            Some(file) => Update::Ready { name, file },
            None => Update::Failed { name },
        };
        if updates.send(update).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

fn decoding_thread(link: Arc<Link>, pictures: SyncSender<Decoded>, ctx: egui::Context) {
    loop {
        let mut work = link.work();
        let (name, file) = loop {
            if work.closed {
                return;
            }
            if let Some(next) = work.decode.pop() {
                break next;
            }
            work = link
                .changed
                .wait(work)
                .unwrap_or_else(|poison| poison.into_inner());
        };
        drop(work);
        let image = std::fs::read(&file)
            .ok()
            .and_then(|bytes| Bitmap::decode(&bytes))
            .map(|bitmap| {
                ColorImage::from_rgba_unmultiplied(
                    [bitmap.width as usize, bitmap.height as usize],
                    &bitmap.rgba,
                )
            });
        if pictures.send(Decoded { name, file, image }).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

#[derive(Default, PartialEq, Eq)]
enum State {
    /// Not drawn yet.
    #[default]
    Waiting,
    Stored(PathBuf),
    Failed,
}

#[derive(Default)]
struct Entry {
    state: State,
    texture: Option<TextureHandle>,
    /// Asked of the decoding thread.
    asked: bool,
    used: u64,
}

/// What a card shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Thumb {
    /// Not ready yet.
    Placeholder,
    Picture(egui::TextureId, [usize; 2]),
    /// There is no picture of this skin, nor will there be.
    Failed,
}

struct Threads {
    link: Arc<Link>,
    updates: Receiver<Update>,
    pictures: Receiver<Decoded>,
    generation: Option<u64>,
}

impl Drop for Threads {
    fn drop(&mut self) {
        self.link.work().closed = true;
        self.link.changed.notify_all();
    }
}

fn start(cache: PathBuf, ctx: &egui::Context) -> Option<Threads> {
    let link = Arc::new(Link {
        work: Mutex::default(),
        changed: Condvar::new(),
    });
    let (update_sender, updates) = channel();
    let (picture_sender, pictures) = sync_channel(DECODED_WAITING);
    let (for_drawing, context) = (link.clone(), ctx.clone());
    let spawn = std::thread::Builder::new()
        .name("skin-previews".into())
        .stack_size(DRAWING_STACK)
        .spawn(move || drawing_thread(for_drawing, cache, update_sender, context));
    let (for_decoding, context) = (link.clone(), ctx.clone());
    let spawn = spawn.and_then(|_| {
        std::thread::Builder::new()
            .name("skin-thumbnails".into())
            .spawn(move || decoding_thread(for_decoding, picture_sender, context))
    });
    if let Err(error) = spawn {
        log::warn!("could not start drawing skin previews: {error}");
        return None;
    }
    Some(Threads {
        link,
        updates,
        pictures,
        generation: None,
    })
}

/// The previews as the window knows them. Belongs to the window's thread.
#[derive(Default)]
pub struct SkinPreviews {
    threads: Option<Threads>,
    entries: HashMap<String, Entry>,
    clock: u64,
    /// Cards on screen this frame.
    visible: Vec<String>,
    textures: usize,
}

impl SkinPreviews {
    /// Starts a frame of Settings: takes in what the threads finished and,
    /// when the skins folder has been listed again, tells them.
    pub fn begin(
        &mut self,
        ctx: &egui::Context,
        cache: &Path,
        choices: &[SkinChoice],
        generation: u64,
        listed: bool,
    ) {
        self.clock += 1;
        self.visible.clear();
        if self.threads.is_none() {
            self.threads = start(cache.to_path_buf(), ctx);
        }
        let Some(threads) = &mut self.threads else {
            return;
        };
        if listed && threads.generation != Some(generation) {
            threads.generation = Some(generation);
            threads.link.work().listing = Some(
                choices
                    .iter()
                    .map(|choice| (choice.name.clone(), choice.path.clone()))
                    .collect(),
            );
            threads.link.changed.notify_all();
        }
        while let Ok(update) = threads.updates.try_recv() {
            match update {
                Update::Ready { name, file } => {
                    let entry = self.entries.entry(name).or_default();
                    if entry.state != State::Stored(file.clone()) {
                        self.textures -= usize::from(entry.texture.take().is_some());
                        entry.asked = false;
                        entry.state = State::Stored(file);
                    }
                }
                Update::Failed { name } => {
                    let entry = self.entries.entry(name).or_default();
                    self.textures -= usize::from(entry.texture.take().is_some());
                    entry.state = State::Failed;
                }
                Update::Gone { name } => {
                    if let Some(entry) = self.entries.remove(&name) {
                        self.textures -= usize::from(entry.texture.is_some());
                    }
                }
            }
        }
        for _ in 0..UPLOADS_PER_FRAME {
            let Ok(decoded) = threads.pictures.try_recv() else {
                break;
            };
            let Some(entry) = self.entries.get_mut(&decoded.name) else {
                continue;
            };
            entry.asked = false;
            // The skin may have changed while this was on its way.
            if entry.state != State::Stored(decoded.file) {
                continue;
            }
            match decoded.image {
                Some(image) => {
                    let label = format!("skin-preview-{}", decoded.name);
                    let handle = ctx.load_texture(label, image, TEXTURE_OPTIONS);
                    self.textures += usize::from(entry.texture.replace(handle).is_none());
                }
                None => entry.state = State::Failed,
            }
        }
    }

    /// What a card on screen shows now.
    pub fn thumb(&mut self, name: &str) -> Thumb {
        self.visible.push(name.to_owned());
        let Some(entry) = self.entries.get_mut(name) else {
            return Thumb::Placeholder;
        };
        entry.used = self.clock;
        match (&entry.state, &entry.texture) {
            (State::Failed, _) => Thumb::Failed,
            (_, Some(texture)) => Thumb::Picture(texture.id(), texture.size()),
            _ => Thumb::Placeholder,
        }
    }

    /// Ends a frame of Settings: says what is on screen, asks for the
    /// pictures it lacks, and lets go of the longest unused ones.
    pub fn finish(&mut self) {
        let Some(threads) = &self.threads else {
            return;
        };
        let wanted: Vec<(String, PathBuf)> = self
            .visible
            .iter()
            .filter_map(|name| {
                let entry = self.entries.get_mut(name)?;
                let State::Stored(file) = &entry.state else {
                    return None;
                };
                (entry.texture.is_none() && !entry.asked).then(|| {
                    entry.asked = true;
                    (name.clone(), file.clone())
                })
            })
            .collect();
        {
            let mut work = threads.link.work();
            // What has scrolled off is not worth decoding any more.
            let visible = &self.visible;
            let entries = &mut self.entries;
            work.decode.retain(|(name, _)| {
                let stays = visible.contains(name);
                if !stays && let Some(entry) = entries.get_mut(name) {
                    entry.asked = false;
                }
                stays
            });
            work.decode.extend(wanted);
            if !self.visible.is_empty() {
                work.seen = Some(Instant::now());
                work.front.clone_from(&self.visible);
            }
        }
        threads.link.changed.notify_all();
        while self.textures > MAX_TEXTURES {
            let oldest = self
                .entries
                .iter_mut()
                .filter(|(_, entry)| entry.texture.is_some() && entry.used < self.clock)
                .min_by_key(|(_, entry)| entry.used);
            let Some((_, entry)) = oldest else {
                break;
            };
            entry.texture = None;
            self.textures -= 1;
        }
    }

    /// The textures held now.
    pub fn texture_count(&self) -> usize {
        self.textures
    }

    /// Lets go of the textures, for when the window they belong to is gone.
    pub fn forget_textures(&mut self) {
        for entry in self.entries.values_mut() {
            entry.texture = None;
        }
        self.textures = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("spotifast-previews-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([9, 9, 9]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn classic(path: &Path) {
        let bytes = crate::skin::zip::write(&[("main.bmp", &png(275, 116), false)]);
        std::fs::write(path, bytes).unwrap();
    }

    fn choice(path: &Path) -> SkinChoice {
        SkinChoice {
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            path: path.to_path_buf(),
            kind: crate::winamp::SkinKind::Winamp,
        }
    }

    /// Frames of Settings with `names` on screen, until `done` says so.
    fn frames(
        previews: &mut SkinPreviews,
        ctx: &egui::Context,
        cache: &Path,
        choices: &[SkinChoice],
        generation: u64,
        mut done: impl FnMut(&mut SkinPreviews) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(previews) {
            assert!(Instant::now() < deadline, "the previews never came");
            previews.begin(ctx, cache, choices, generation, true);
            for choice in choices {
                previews.thumb(&choice.name);
            }
            previews.thumb(BUILT_IN);
            for name in crate::skin::SHIPPED {
                previews.thumb(name);
            }
            previews.finish();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn is_picture(thumb: Thumb) -> bool {
        matches!(thumb, Thumb::Picture(..))
    }

    #[test]
    fn a_stem_changes_with_the_name_the_size_and_the_time() {
        let base = stem_of("a.wsz", 10, 20);
        assert_eq!(base, stem_of("a.wsz", 10, 20));
        assert_ne!(base, stem_of("b.wsz", 10, 20));
        assert_ne!(base, stem_of("a.wsz", 11, 20));
        assert_ne!(base, stem_of("a.wsz", 10, 21));
    }

    #[test]
    fn what_is_on_screen_is_drawn_first_and_only_while_cards_show() {
        let job = |name: &str| Job {
            name: name.to_owned(),
            path: None,
            stem: name.to_owned(),
        };
        let mut work = Work {
            queue: VecDeque::from([job("a"), job("b"), job("c")]),
            front: vec!["c".to_owned()],
            ..Work::default()
        };
        let now = Instant::now();
        assert_eq!(work.next_job(now), None, "no card has been on screen");
        work.seen = Some(now);
        assert_eq!(work.next_job(now).map(|job| job.name), Some("c".into()));
        assert_eq!(work.next_job(now).map(|job| job.name), Some("a".into()));
        assert_eq!(work.next_job(now + STAY_ACTIVE), None);
    }

    /// A blank card left by the old renderer: the mark `VERSION` 1 wrote for
    /// this very file must not stop gnome's kind of skin getting its picture.
    #[test]
    fn a_mark_from_before_the_renderers_fixes_leaves_the_card_to_be_drawn() {
        let root = folder("stale-mark");
        let cache = root.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let skin = root.join("hidden.wmz");
        let definition =
            br##"<VIEW width="40" height="40" visible="false" backgroundColor="#c80a0a"/>"##;
        let bytes = crate::skin::zip::write(&[("skin.wms", definition.as_slice(), false)]);
        std::fs::write(&skin, bytes).unwrap();
        let name = "hidden.wmz";
        let meta = std::fs::metadata(&skin).unwrap();
        let modified = meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let old = {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (1u32, name, meta.len(), modified).hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        };
        std::fs::write(cache.join(format!("{old}.none")), [0; MAX_TRIES]).unwrap();
        let choices = [SkinChoice {
            kind: crate::winamp::SkinKind::WindowsMediaPlayer,
            ..choice(&skin)
        }];
        let ctx = egui::Context::default();
        let mut previews = SkinPreviews::default();
        frames(&mut previews, &ctx, &cache, &choices, 1, |previews| {
            is_picture(previews.thumb(name))
        });
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every `.wmz` in `$SPOTIFAST_SKIN_SAMPLES`, when set, through the cards'
    /// own path: stored, decoded and handed over as a picture.
    #[test]
    fn sample_skins_get_cards() {
        let Ok(dir) = std::env::var("SPOTIFAST_SKIN_SAMPLES") else {
            return;
        };
        let root = folder("samples");
        let cache = root.join("cache");
        let choices: Vec<SkinChoice> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|kind| kind == "wmz"))
            .map(|path| SkinChoice {
                kind: crate::winamp::SkinKind::WindowsMediaPlayer,
                ..choice(&path)
            })
            .collect();
        let ctx = egui::Context::default();
        let mut previews = SkinPreviews::default();
        frames(&mut previews, &ctx, &cache, &choices, 1, |previews| {
            choices
                .iter()
                .all(|choice| previews.thumb(&choice.name) != Thumb::Placeholder)
        });
        for choice in &choices {
            assert!(
                is_picture(previews.thumb(&choice.name)),
                "{} has no card",
                choice.name
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_skin_gets_a_card_and_a_stored_picture_a_broken_one_does_not() {
        let root = folder("cards");
        let cache = root.join("cache");
        let good = root.join("good.wsz");
        let broken = root.join("broken.wsz");
        classic(&good);
        std::fs::write(&broken, b"not a skin").unwrap();
        let choices = [choice(&good), choice(&broken)];
        let ctx = egui::Context::default();
        let mut previews = SkinPreviews::default();
        frames(&mut previews, &ctx, &cache, &choices, 1, |previews| {
            is_picture(previews.thumb("good.wsz"))
                && is_picture(previews.thumb(BUILT_IN))
                && crate::skin::SHIPPED
                    .iter()
                    .all(|name| is_picture(previews.thumb(name)))
                && previews.thumb("broken.wsz") == Thumb::Failed
        });
        let stored = |extension: &str| {
            std::fs::read_dir(&cache)
                .unwrap()
                .flatten()
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|kind| kind == extension)
                })
                .count()
        };
        assert_eq!((stored("png"), stored("none")), (4, 1));
        drop(previews);

        // A second run finds them; a removed skin takes its picture along.
        let drawn = std::fs::read_dir(&cache).unwrap().count();
        let mut previews = SkinPreviews::default();
        frames(&mut previews, &ctx, &cache, &choices[..1], 1, |previews| {
            is_picture(previews.thumb("good.wsz"))
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        while std::fs::read_dir(&cache).unwrap().count() >= drawn {
            assert!(Instant::now() < deadline, "the old file stayed");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(stored("none"), 0);
        drop(previews);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_try_cut_short_is_tried_again_and_a_skin_that_keeps_crashing_is_not() {
        let root = folder("tries");
        let path = root.join("skin.wsz");
        classic(&path);
        let job = Job {
            name: "skin.wsz".into(),
            path: Some(path),
            stem: "stem".into(),
        };
        let mark = root.join("stem.none");
        std::fs::write(&mark, vec![0; MAX_TRIES]).unwrap();
        assert_eq!(draw(&job, &root), None);
        std::fs::write(&mark, vec![0; MAX_TRIES - 1]).unwrap();
        assert_eq!(draw(&job, &root), Some(root.join("stem.png")));
        assert!(!mark.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_skin_replaced_under_its_name_gets_a_new_picture() {
        let root = folder("changed");
        let cache = root.join("cache");
        let path = root.join("skin.wsz");
        classic(&path);
        let choices = [choice(&path)];
        let ctx = egui::Context::default();
        let mut previews = SkinPreviews::default();
        frames(&mut previews, &ctx, &cache, &choices, 1, |previews| {
            is_picture(previews.thumb("skin.wsz"))
        });
        let before = previews.thumb("skin.wsz");
        // Longer, so that the size tells them apart on any file system.
        let bytes = crate::skin::zip::write(&[("main.bmp", &png(275, 120), false)]);
        std::fs::write(&path, bytes).unwrap();
        frames(&mut previews, &ctx, &cache, &choices, 2, |previews| {
            let now = previews.thumb("skin.wsz");
            is_picture(now.clone()) && now != before
        });
        drop(previews);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_skin_taken_away_leaves_no_card_behind() {
        let root = folder("gone");
        let cache = root.join("cache");
        let path = root.join("skin.wsz");
        classic(&path);
        let choices = [choice(&path)];
        let ctx = egui::Context::default();
        let mut previews = SkinPreviews::default();
        frames(&mut previews, &ctx, &cache, &choices, 1, |previews| {
            is_picture(previews.thumb("skin.wsz"))
        });
        frames(&mut previews, &ctx, &cache, &[], 2, |previews| {
            !previews.entries.contains_key("skin.wsz")
        });
        assert!(
            previews.texture_count() <= 1 + crate::skin::SHIPPED.len(),
            "at most the built-in skins'"
        );
        drop(previews);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn textures_stay_bounded_and_those_on_screen_stay() {
        let ctx = egui::Context::default();
        let mut previews = SkinPreviews::default();
        let image = || ColorImage::filled([2, 2], egui::Color32::RED);
        for index in 0..MAX_TEXTURES + 20 {
            let handle = ctx.load_texture("t", image(), TEXTURE_OPTIONS);
            previews.entries.insert(
                format!("{index}"),
                Entry {
                    state: State::Stored(PathBuf::from("x")),
                    texture: Some(handle),
                    asked: false,
                    used: index as u64,
                },
            );
            previews.textures += 1;
        }
        previews.threads = start(std::env::temp_dir().join("spotifast-unused"), &ctx);
        previews.clock = 1_000;
        previews.entries.get_mut("0").unwrap().used = 1_000;
        previews.finish();
        assert_eq!(previews.texture_count(), MAX_TEXTURES);
        assert!(previews.entries["0"].texture.is_some());
        assert!(previews.entries["1"].texture.is_none());
    }
}
