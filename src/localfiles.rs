//! Index and match local audio files for `spotify:local:` URIs.
//!
//! The index is a JSON file in the state directory. Scanning walks configured
//! folders recursively, probes tags and durations with symphonia using exactly
//! the same derivation librespot's playback engine uses, and merges unchanged
//! entries from the previous index so rescans are cheap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey};
use symphonia::core::probe::Hint;

/// One local audio file in the persisted index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFile {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
    pub mtime: u64,
    pub size: u64,
    pub uri: String,
}

/// A scanned library, persisted as JSON and queried by URI.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Index {
    pub files: Vec<LocalFile>,
    #[serde(skip)]
    by_title: HashMap<String, Vec<usize>>,
}

impl Index {
    /// Build a new index from a list of files and index it for lookups.
    pub fn from_files(files: Vec<LocalFile>) -> Self {
        let mut index = Self {
            files,
            by_title: HashMap::new(),
        };
        index.rebuild();
        index
    }

    fn rebuild(&mut self) {
        self.by_title.clear();
        self.by_title.reserve(self.files.len());
        for (index, file) in self.files.iter().enumerate() {
            let key = normalize(&file.title);
            if !key.is_empty() {
                self.by_title.entry(key).or_default().push(index);
            }
        }
    }

    /// Find the best match for a `spotify:local:` URI.
    ///
    /// Empty query fields match nothing, so they never act as wildcards.
    pub fn find(
        &self,
        artist: &str,
        album: &str,
        title: &str,
        duration: Duration,
    ) -> Option<&LocalFile> {
        let norm_title = normalize(title);
        if norm_title.is_empty() {
            return None;
        }
        let norm_artist = normalize(artist);
        let norm_album = normalize(album);
        let uri_ms = u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
        let candidates = self.by_title.get(&norm_title)?;

        // (a) Title + artist equality, preferring album match, then duration.
        if !norm_artist.is_empty() {
            let mut best: Option<&LocalFile> = None;
            // Lower is better: album mismatches sort behind matches, then the
            // duration distance breaks the tie.
            let mut best_score: (bool, i64) = (true, i64::MAX);
            for &idx in candidates {
                let file = &self.files[idx];
                // Accept either the lead name or the whole multi-value tag:
                // a saved playlist row can carry either shape.
                if normalize(&file.artist) != norm_artist
                    && normalize(&first_artist(&file.artist)) != norm_artist
                {
                    continue;
                }
                if !duration_compatible(uri_ms, file.duration_ms) {
                    continue;
                }
                let album_match = !norm_album.is_empty() && normalize(&file.album) == norm_album;
                let diff = duration_diff(uri_ms, file.duration_ms);
                let score = (!album_match, diff);
                if best.is_none() || score < best_score {
                    best = Some(file);
                    best_score = score;
                }
            }
            return best;
        }

        // (b) Title + album equality within duration tolerance.
        if !norm_album.is_empty() {
            let mut best: Option<&LocalFile> = None;
            let mut best_diff = i64::MAX;
            for &idx in candidates {
                let file = &self.files[idx];
                if normalize(&file.album) != norm_album {
                    continue;
                }
                if !duration_compatible(uri_ms, file.duration_ms) {
                    continue;
                }
                let diff = duration_diff(uri_ms, file.duration_ms);
                if diff < best_diff {
                    best = Some(file);
                    best_diff = diff;
                }
            }
            if best.is_some() {
                return best;
            }
        }

        // (c) Title only within duration tolerance.
        let mut best: Option<&LocalFile> = None;
        let mut best_diff = i64::MAX;
        for &idx in candidates {
            let file = &self.files[idx];
            if !duration_compatible(uri_ms, file.duration_ms) {
                continue;
            }
            let diff = duration_diff(uri_ms, file.duration_ms);
            if diff < best_diff {
                best = Some(file);
                best_diff = diff;
            }
        }
        best
    }
}

const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SCAN_DEPTH: usize = 32;
const PROGRESS_EVERY: usize = 64;
const DURATION_TOLERANCE_MS: i64 = 5000;

// librespot's own lookup supports exactly these four extensions. The lowercased
// extension is checked against this list, matching librespot behavior.
const AUDIO_EXTENSIONS: &[&str; 4] = &["mp3", "mp4", "m4p", "flac"];

/// Progress and result of a local-files scan.
#[derive(Clone, Debug)]
pub enum ScanEvent {
    Started {
        folders: usize,
    },
    Progress {
        done: usize,
    },
    Finished {
        scanned: usize,
        indexed: usize,
    },
    /// The walk stopped early; the previous index was left untouched.
    Cancelled,
    Failed(String),
}

/// Path to the persisted local-files index.
pub fn index_path(dirs: &crate::paths::AppDirs) -> PathBuf {
    dirs.state.join("local-files-index.json")
}

/// Load a previously persisted index, tolerating corrupt or missing files.
pub fn load(path: &Path) -> Option<Index> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut index: Index = serde_json::from_str(&text).ok()?;
    index.rebuild();
    Some(index)
}

/// Build the canonical `spotify:local:` URI from raw tag fields and whole
/// seconds. This must stay byte-for-byte identical to librespot's URI builder.
pub fn local_uri(artist: &str, album: &str, title: &str, secs: u64) -> String {
    format!(
        "spotify:local:{}:{}:{}:{}",
        encode_uri_part(artist),
        encode_uri_part(album),
        encode_uri_part(title),
        secs
    )
}

fn encode_uri_part(input: &str) -> String {
    form_urlencoded::byte_serialize(input.as_bytes()).collect::<String>()
}

/// Scan folders recursively and write a merged index to `index_file`.
///
/// This function is blocking and is meant to run on a thread spawned by the
/// caller. It sends progress events on `out` and checks `cancel` between files.
/// The index is written atomically (temp file + rename) when the scan finishes.
/// A cancelled scan reports [`ScanEvent::Cancelled`] and leaves the previous
/// index untouched.
pub fn scan(
    folders: &[PathBuf],
    index_file: &Path,
    out: std::sync::mpsc::Sender<ScanEvent>,
    cancel: Arc<AtomicBool>,
) {
    let _ = out.send(ScanEvent::Started {
        folders: folders.len(),
    });

    let previous = load(index_file).unwrap_or_default();
    let mut by_path: HashMap<PathBuf, LocalFile> = previous
        .files
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect();

    // Each configured folder is the traversal root as-is: read_dir follows a
    // symlinked root (junctions to a music drive are common) while the
    // interior walk skips symlinks, so no loop can form, and a dangling root
    // symlink errors below and lands in `unreadable`, which keeps its
    // previous index entries under the configured path instead of dropping
    // them.
    let mut stack: Vec<(PathBuf, usize)> = folders
        .iter()
        .map(|folder| (folder.to_path_buf(), 0))
        .collect();

    let mut indexed = Vec::new();
    let mut scanned = 0usize;
    let mut done = 0usize;
    let mut unreadable = Vec::new();
    // Configured folders may overlap (Music and Music/Rock): each file is
    // visited once, so saved entries and scan counts stay unique. The set
    // doubles as the dedupe for the unreadable-folder merge below.
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    while let Some((folder, depth)) = stack.pop() {
        if depth >= MAX_SCAN_DEPTH {
            log::debug!("local-files scan reached depth limit");
            continue;
        }

        let entries = match std::fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) => {
                log::debug!("unable to read local-files directory: {error}");
                unreadable.push(folder.clone());
                continue;
            }
        };

        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    log::debug!("unable to read local-files directory entry: {error}");
                    continue;
                }
            };

            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }

            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    log::debug!("unable to inspect local-files entry type: {error}");
                    continue;
                }
            };

            if file_type.is_symlink() {
                continue;
            }

            let path = entry.path();
            if file_type.is_dir() {
                if seen.insert(path.clone()) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
                continue;
            };
            if !AUDIO_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(ext))
            {
                continue;
            }
            if !seen.insert(path.clone()) {
                // Reached through another configured folder already.
                continue;
            }

            scanned += 1;
            done += 1;
            if done.is_multiple_of(PROGRESS_EVERY) {
                let _ = out.send(ScanEvent::Progress { done });
            }

            match index_file_entry(&path, &by_path) {
                Ok(Some(file)) => {
                    by_path.insert(path, file.clone());
                    indexed.push(file);
                }
                Ok(None) => {}
                Err(EntryError::Unreadable(error)) => {
                    // A file listed but not stattable right now (locked by
                    // another process, permissions mid-change, deleted
                    // mid-walk) is not a file whose tags vanished: keep what
                    // the previous index knew about it.
                    log::debug!(
                        "local file {} unreadable this scan: {error}",
                        path.display()
                    );
                    if let Some(previous) = by_path.get(&path) {
                        indexed.push(previous.clone());
                    }
                }
                Err(EntryError::Content(error)) => {
                    log::warn!("unable to index local file {}: {error}", path.display());
                }
            }
        }

        if cancel.load(Ordering::Relaxed) {
            break;
        }
    }

    if cancel.load(Ordering::Relaxed) {
        // A partial walk must never replace a complete index.
        let _ = out.send(ScanEvent::Cancelled);
        return;
    }

    // A folder that cannot be read right now (an unmounted drive, lost
    // permissions) is not a folder whose songs disappeared: keep what the
    // previous index knew about it. `seen` already holds every path the walk
    // visited, so `insert` also dedupes a folder nesting inside another
    // unreadable one.
    for folder in &unreadable {
        for file in by_path
            .values()
            .filter(|file| file.path.starts_with(folder))
        {
            if seen.insert(file.path.clone()) {
                indexed.push(file.clone());
            }
        }
    }

    let result = save_index(index_file, &indexed);
    let event = match result {
        Ok(()) => ScanEvent::Finished {
            scanned,
            indexed: indexed.len(),
        },
        Err(error) => ScanEvent::Failed(format!("Unable to save the local-files index: {error}")),
    };
    let _ = out.send(event);
}

fn save_index(path: &Path, files: &[LocalFile]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let index = Index::from_files(files.to_vec());
    let text = serde_json::to_string_pretty(&index)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    crate::util::replace_file(&temporary, path)
}

/// Why indexing one file failed.
enum EntryError {
    /// The file itself could not be inspected (stat or open failed): possibly
    /// transient, so the previous index entry stays a reasonable stand-in.
    Unreadable(String),
    /// The content is unusable (no engine-readable tags, parameters the
    /// decoder refuses): intentional, and the previous entry must not come
    /// back for a file that could only fail in the player.
    Content(String),
}

fn index_file_entry(
    path: &Path,
    previous: &HashMap<PathBuf, LocalFile>,
) -> Result<Option<LocalFile>, EntryError> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| EntryError::Unreadable(format!("could not read metadata: {error}")))?;
    let size = metadata.len();
    if size > MAX_FILE_BYTES {
        return Ok(None);
    }
    let mtime = metadata
        .modified()
        .map_err(|error| {
            EntryError::Unreadable(format!("could not read modification time: {error}"))
        })?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if let Some(existing) = previous.get(path)
        && existing.size == size
        && existing.mtime == mtime
    {
        return Ok(Some(existing.clone()));
    }

    let (tag_artist, tag_album, tag_title, duration_ms, duration_secs) = read_tags(path)?;
    let uri = local_uri(&tag_artist, &tag_album, &tag_title, duration_secs);
    let title = if tag_title.trim().is_empty() {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        tag_title
    };

    Ok(Some(LocalFile {
        path: path.to_path_buf(),
        title,
        artist: tag_artist,
        album: tag_album,
        duration_ms,
        mtime,
        size,
        uri,
    }))
}

fn read_tags(path: &Path) -> Result<(String, String, String, u32, u64), EntryError> {
    let file = std::fs::File::open(path)
        .map_err(|error| EntryError::Unreadable(format!("failed to open file: {error}")))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let fmt_opts: FormatOptions = Default::default();
    let meta_opts: MetadataOptions = Default::default();

    let mut probed = symphonia::default::get_probe()
        .format(&hint, mss, &fmt_opts, &meta_opts)
        .map_err(|error| EntryError::Content(format!("failed to probe file: {error}")))?;

    // Resolve tags exactly the way the engine's own local-file lookup does
    // (librespot's symphonia_util::get_latest_metadata): the container's
    // metadata first, metadata found while probing when the container has
    // none, then the latest revision. Without a tag container the engine
    // refuses the file a URI, so a row for it could never start.
    let mut metadata = probed.format.metadata();
    if metadata.current().is_none()
        && let Some(inner) = probed.metadata.get()
    {
        metadata = inner;
    }
    _ = metadata.skip_to_latest();

    let mut artist = String::new();
    let mut album = String::new();
    let mut title = String::new();

    let revision = metadata.current().ok_or_else(|| {
        EntryError::Content("no audio tags that the playback engine can read".into())
    })?;
    for tag in revision.tags() {
        if let Some(std_key) = tag.std_key {
            let value = tag.value.to_string();
            match std_key {
                StandardTagKey::Artist => artist = value,
                StandardTagKey::Album => album = value,
                StandardTagKey::TrackTitle => title = value,
                _ => {}
            }
        }
    }

    let track = probed
        .format
        .default_track()
        .ok_or_else(|| EntryError::Content("failed to find an audio track".into()))?;
    engine_decodable(&track.codec_params).map_err(EntryError::Content)?;
    let time_base = track
        .codec_params
        .time_base
        .ok_or_else(|| EntryError::Content("failed to calculate track duration".into()))?;
    let n_frames = track
        .codec_params
        .n_frames
        .ok_or_else(|| EntryError::Content("failed to calculate track duration".into()))?;
    let time = time_base.calc_time(n_frames);
    let duration_ms = u32::try_from(
        time.seconds
            .saturating_mul(1000)
            .saturating_add((time.frac * 1000.0) as u64),
    )
    .unwrap_or(u32::MAX);

    Ok((artist, album, title, duration_ms, time.seconds))
}

/// The engine's symphonia decoder is fixed to 44.1 kHz stereo (librespot's
/// own TODO notes the official client resamples other rates; this one does
/// not). A track whose declared parameters already say otherwise fails to
/// decode on every play, so keep it out of the index instead of offering a
/// row that can never start. Parameters the container leaves unstated pass
/// here: the decoder derives them when it can, and only a definitive
/// mismatch is certain to fail.
fn engine_decodable(params: &CodecParameters) -> Result<(), String> {
    if let Some(rate) = params.sample_rate
        && rate != librespot_playback::SAMPLE_RATE
    {
        return Err(format!(
            "the playback engine only decodes 44.1 kHz audio, and this file is {rate} Hz"
        ));
    }
    if let Some(channels) = params.channels
        && channels.count() != usize::from(librespot_playback::NUM_CHANNELS)
    {
        return Err(format!(
            "the playback engine only decodes stereo audio, and this file declares {channels}"
        ));
    }
    Ok(())
}

/// Extract the first artist from a multi-value tag.
fn first_artist(artist: &str) -> String {
    let lowered = artist.to_lowercase();
    // Case folding can change byte lengths (e.g. 'İ' grows a combining mark),
    // so offsets found in `lowered` are only valid in `lowered` itself.
    let mut cut = lowered.len();
    for separator in [";", ", ", " / ", " feat. "] {
        if let Some(pos) = lowered.find(separator) {
            cut = cut.min(pos);
        }
    }
    lowered[..cut].trim().to_string()
}

fn duration_compatible(uri_ms: u32, file_ms: u32) -> bool {
    if uri_ms == 0 || file_ms == 0 {
        return true;
    }
    (uri_ms as i64 - file_ms as i64).abs() <= DURATION_TOLERANCE_MS
}

fn duration_diff(uri_ms: u32, file_ms: u32) -> i64 {
    if uri_ms == 0 || file_ms == 0 {
        return i64::MAX;
    }
    (uri_ms as i64 - file_ms as i64).abs()
}

/// Lowercase, fold Latin accents, drop apostrophes, and collapse runs of
/// non-alphanumerics to a single space.
///
/// This deliberately duplicates the fold table in `src/lyrics.rs` so the
/// local-files matcher can stand alone without importing lyrics internals.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        let folded = fold(c);
        if folded == '\'' || folded == '’' || folded == '`' {
            continue;
        }
        if folded == '&' {
            for word in [' ', 'a', 'n', 'd', ' '] {
                push_normalized(&mut out, &mut space, word);
            }
            continue;
        }
        push_normalized(&mut out, &mut space, folded);
    }
    out.trim().to_string()
}

fn push_normalized(out: &mut String, space: &mut bool, c: char) {
    if c.is_alphanumeric() {
        out.extend(c.to_lowercase());
        *space = false;
    } else if !*space {
        out.push(' ');
        *space = true;
    }
}

/// The plain letter behind the Latin accents titles most often carry.
fn fold(c: char) -> char {
    match c {
        'À'..='Å' | 'à'..='å' => 'a',
        'Ç' | 'ç' => 'c',
        'È'..='Ë' | 'è'..='ë' => 'e',
        'Ì'..='Ï' | 'ì'..='ï' => 'i',
        'Ñ' | 'ñ' => 'n',
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' => 'o',
        'Ù'..='Ü' | 'ù'..='ü' => 'u',
        'Ý' | 'ý' | 'ÿ' => 'y',
        'ß' => 's',
        _ => c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "spotifast-localfiles-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    fn touch(path: &Path) {
        std::fs::write(path, b"not a real audio file").unwrap();
    }

    fn run_scan(folder: &Path, index_file: &Path) -> Vec<ScanEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        scan(&[folder.to_path_buf()], index_file, tx, cancel);
        rx.iter().collect()
    }

    #[test]
    fn local_uri_encoding_is_exact() {
        assert_eq!(
            local_uri("David Wise", "A Monkey's Island", "Cavern Plaza", 213),
            "spotify:local:David+Wise:A+Monkey%27s+Island:Cavern+Plaza:213"
        );
        assert_eq!(
            local_uri("", "Album", "Title", 0),
            "spotify:local::Album:Title:0"
        );
        assert_eq!(local_uri("Artist", "", "", 1), "spotify:local:Artist:::1");
        assert_eq!(
            local_uri("Møre & more", "日本語", "café", 999),
            "spotify:local:M%C3%B8re+%26+more:%E6%97%A5%E6%9C%AC%E8%AA%9E:caf%C3%A9:999"
        );
    }

    #[test]
    fn local_uri_matches_librespot_parse() {
        let uri_string = local_uri("David Wise", "A Monkey's Island", "Cavern Plaza", 213);
        let parsed = librespot_core::SpotifyUri::from_uri(&uri_string).unwrap();
        let expected = librespot_core::SpotifyUri::Local {
            artist: "David+Wise".into(),
            album_title: "A+Monkey%27s+Island".into(),
            track_title: "Cavern+Plaza".into(),
            duration: Duration::from_secs(213),
        };
        assert_eq!(parsed, expected);
    }

    #[test]
    fn extension_filter_only_attempts_supported_types() {
        let root = test_dir();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        touch(&root.join("supported.mp3"));
        touch(&root.join("supported.flac"));
        touch(&root.join("supported.mp4"));
        touch(&root.join("supported.m4p"));
        touch(&root.join("ignored.ogg"));
        touch(&root.join("ignored.wav"));
        touch(&root.join("ignored.txt"));

        let index_file = root.join("index.json");
        let events = run_scan(&root, &index_file);
        let finished = events.iter().find_map(|event| match event {
            ScanEvent::Finished { scanned, indexed } => Some((*scanned, *indexed)),
            _ => None,
        });
        // Only the four supported extensions are attempted; probes fail on the
        // dummy bytes so nothing is indexed, but unsupported files are ignored.
        assert_eq!(finished, Some((4, 0)));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn index_persists_and_reloads() {
        let root = test_dir();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("song.mp3");
        touch(&file);

        let index_file = root.join("index.json");
        let file_entry = LocalFile {
            path: file.clone(),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 90_000,
            mtime: std::fs::metadata(&file)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            size: std::fs::metadata(&file).unwrap().len(),
            uri: local_uri("Artist", "Album", "Song", 90),
        };
        let index = Index::from_files(vec![file_entry.clone()]);
        std::fs::write(&index_file, serde_json::to_string_pretty(&index).unwrap()).unwrap();

        let loaded = load(&index_file).unwrap();
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files[0], file_entry);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_keeps_existing_entries_for_unchanged_files() {
        let root = test_dir();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("keep.mp3");
        touch(&file);

        let index_file = root.join("index.json");
        let mtime = std::fs::metadata(&file)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let size = std::fs::metadata(&file).unwrap().len();
        let custom = LocalFile {
            path: file.clone(),
            title: "Custom Title".into(),
            artist: "Custom Artist".into(),
            album: "Custom Album".into(),
            duration_ms: 123_456,
            mtime,
            size,
            uri: local_uri("Custom Artist", "Custom Album", "Custom Title", 123),
        };
        let initial = Index::from_files(vec![custom.clone()]);
        std::fs::write(&index_file, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        // Rescan without changing the file. The existing entry must survive,
        // even though the dummy file cannot be probed.
        run_scan(&root, &index_file);
        let after = load(&index_file).unwrap();
        assert_eq!(after.files.len(), 1);
        assert_eq!(after.files[0], custom);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_stops_a_large_scan_and_preserves_the_previous_index() {
        let root = test_dir();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..300 {
            touch(&root.join(format!("track_{i}.mp3")));
        }

        // A previous index holds something worth keeping.
        let index_file = root.join("index.json");
        let kept = LocalFile {
            path: root.join("gone.mp3"),
            title: "Earlier".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 60_000,
            mtime: 1,
            size: 1,
            uri: local_uri("Artist", "Album", "Earlier", 60),
        };
        let initial = Index::from_files(vec![kept.clone()]);
        std::fs::write(&index_file, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(true));
        scan(std::slice::from_ref(&root), &index_file, tx, cancel);
        let events: Vec<ScanEvent> = rx.iter().collect();

        // With cancellation set from the start, the walk stops quickly with a
        // Cancelled event rather than a Finished one.
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ScanEvent::Cancelled))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ScanEvent::Finished { .. }))
        );

        // The partial walk must not replace the previous index.
        let after = load(&index_file).unwrap();
        assert_eq!(after.files.len(), 1);
        assert_eq!(after.files[0], kept);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unreadable_folder_keeps_its_previous_index_entries() {
        let root = test_dir();
        let gone = root.join("unmounted-drive");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // The previous index knows a song inside a folder that no longer
        // exists on disk (an unmounted drive, lost permissions).
        let index_file = root.join("index.json");
        let kept = LocalFile {
            path: gone.join("song.mp3"),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 90_000,
            mtime: 1,
            size: 1,
            uri: local_uri("Artist", "Album", "Song", 90),
        };
        let initial = Index::from_files(vec![kept.clone()]);
        std::fs::write(&index_file, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        let events = run_scan(&gone, &index_file);
        let finished = events.iter().find_map(|event| match event {
            ScanEvent::Finished { indexed, .. } => Some(*indexed),
            _ => None,
        });
        assert_eq!(finished, Some(1));

        let after = load(&index_file).unwrap();
        assert_eq!(after.files.len(), 1);
        assert_eq!(after.files[0], kept);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn engine_decodable_accepts_what_the_engine_plays() {
        use symphonia::core::audio::Channels;

        let mut params = CodecParameters::new();
        // Containers are free to leave either unstated; the decoder derives
        // them when it can.
        assert!(engine_decodable(&params).is_ok());

        params.sample_rate = Some(librespot_playback::SAMPLE_RATE);
        params.channels = Some(Channels::FRONT_LEFT | Channels::FRONT_RIGHT);
        assert!(engine_decodable(&params).is_ok());
    }

    #[test]
    fn engine_decodable_rejects_what_the_engine_refuses_to_decode() {
        use symphonia::core::audio::Channels;

        let mut params = CodecParameters::new();
        params.sample_rate = Some(48_000);
        let error = engine_decodable(&params).unwrap_err();
        assert!(error.contains("44.1 kHz"), "{error}");

        let mut params = CodecParameters::new();
        params.channels = Some(Channels::FRONT_CENTRE);
        let error = engine_decodable(&params).unwrap_err();
        assert!(error.contains("stereo"), "{error}");
    }

    fn make_index(files: Vec<LocalFile>) -> Index {
        let mut index = Index::from_files(files);
        index.rebuild();
        index
    }

    fn file(title: &str, artist: &str, album: &str, duration_ms: u32) -> LocalFile {
        LocalFile {
            path: PathBuf::from(format!("/{}.mp3", title.to_lowercase().replace(' ', "_"))),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            duration_ms,
            mtime: 0,
            size: 0,
            uri: local_uri(artist, album, title, duration_ms as u64 / 1000),
        }
    }

    #[test]
    fn exact_match_finds_the_right_file() {
        let index = make_index(vec![file("Song", "Artist", "Album", 200_000)]);
        let found = index.find("Artist", "Album", "Song", Duration::from_millis(200_000));
        assert!(found.is_some());
        assert_eq!(found.unwrap().path, PathBuf::from("/song.mp3"));
    }

    #[test]
    fn first_artist_splitting_uses_the_first_name() {
        let index = make_index(vec![file("Song", "Artist; Other", "Album", 100_000)]);
        assert!(
            index
                .find("Artist", "Album", "Song", Duration::from_millis(100_000))
                .is_some()
        );
        // A row carrying the whole multi-value tag also finds the file.
        assert!(
            index
                .find(
                    "Artist; Other",
                    "Album",
                    "Song",
                    Duration::from_millis(100_000)
                )
                .is_some()
        );
        assert!(
            index
                .find("Other", "Album", "Song", Duration::from_millis(100_000))
                .is_none()
        );
    }

    #[test]
    fn duration_tolerance_edges_reject_far_matches() {
        let index = make_index(vec![file("Song", "", "", 10_000)]);
        assert!(
            index
                .find("", "", "Song", Duration::from_millis(15_000))
                .is_some()
        );
        assert!(
            index
                .find("", "", "Song", Duration::from_millis(15_001))
                .is_none()
        );
    }

    #[test]
    fn empty_artist_query_fails_closed() {
        let index = make_index(vec![file("Song", "Artist", "Album", 60_000)]);
        // Empty query artist should not use title+artist equality; title-only
        // still matches, so the empty-artist rule is tested with a file that
        // has no title-only match.
        assert!(
            index
                .find("Artist", "", "Different", Duration::from_millis(60_000))
                .is_none()
        );
        assert!(
            index
                .find("", "Album", "Different", Duration::from_millis(60_000))
                .is_none()
        );
    }

    #[test]
    fn case_and_accent_folding_match() {
        let index = make_index(vec![LocalFile {
            path: PathBuf::from("/cafe_nandu.mp3"),
            title: "Café Ñandú".into(),
            artist: "".into(),
            album: "".into(),
            duration_ms: 90_000,
            mtime: 0,
            size: 0,
            uri: local_uri("", "", "Café Ñandú", 90),
        }]);
        let found = index.find("", "", "CAFE NANDU", Duration::from_millis(90_000));
        assert!(found.is_some());
    }

    #[test]
    fn album_match_beats_a_closer_duration_mismatch() {
        // Both files share title and artist and sit inside the duration
        // tolerance, but only one is from the queried album. The album match
        // must win even though the other's duration is closer.
        let index = make_index(vec![
            file("Song", "Artist", "Elsewhere", 100_000),
            LocalFile {
                path: PathBuf::from("/right.mp3"),
                ..file("Song", "Artist", "Right Album", 104_000)
            },
        ]);
        let found = index
            .find(
                "Artist",
                "Right Album",
                "Song",
                Duration::from_millis(100_000),
            )
            .unwrap();
        assert_eq!(found.path, PathBuf::from("/right.mp3"));
    }

    #[test]
    fn first_artist_handles_case_folding_that_changes_byte_length() {
        // 'İ' lowercases to 'i' + U+0307, growing by a byte. Offsets found in
        // the folded text can then exceed the original's length; the cut must
        // happen in the folded text itself.
        assert_eq!(first_artist("İrem; Cihan"), "i\u{307}rem");
        assert_eq!(first_artist("İİ;"), "i\u{307}i\u{307}");
    }

    #[test]
    fn changed_file_that_no_longer_probes_drops_out() {
        let root = test_dir();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("song.mp3");
        touch(&file);

        // The previous index remembers a different version of the file.
        let index_file = root.join("index.json");
        let stale = LocalFile {
            path: file.clone(),
            title: "Old Title".into(),
            artist: "Old Artist".into(),
            album: "Old Album".into(),
            duration_ms: 90_000,
            mtime: 1,
            size: 1,
            uri: local_uri("Old Artist", "Old Album", "Old Title", 90),
        };
        let initial = Index::from_files(vec![stale]);
        std::fs::write(&index_file, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        // The file changed and its new content is unusable (dummy bytes cannot
        // be probed): the stale entry must not survive into the new index.
        let events = run_scan(&root, &index_file);
        let finished = events.iter().find_map(|event| match event {
            ScanEvent::Finished { indexed, .. } => Some(*indexed),
            _ => None,
        });
        assert_eq!(finished, Some(0));
        assert!(load(&index_file).unwrap().files.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unreadable_file_keeps_its_prior_entry() {
        // Stat failure on a previously-indexed path is transient, so the scan
        // falls back to what the previous index knew.
        let previous: HashMap<PathBuf, LocalFile> = vec![LocalFile {
            path: PathBuf::from("/missing/song.mp3"),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 90_000,
            mtime: 1,
            size: 1,
            uri: local_uri("Artist", "Album", "Song", 90),
        }]
        .into_iter()
        .map(|file| (file.path.clone(), file))
        .collect();
        let error = index_file_entry(Path::new("/missing/song.mp3"), &previous)
            .expect_err("a missing file is unreadable");
        assert!(matches!(error, EntryError::Unreadable(_)));

        // Usable content failures stay content failures: a present file the
        // engine cannot tag must not resurrect its prior entry.
        let root = test_dir();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("song.mp3");
        touch(&file);
        let error = index_file_entry(&file, &previous).expect_err("dummy bytes cannot be probed");
        assert!(matches!(error, EntryError::Content(_)));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symlinked_root_scans_through_to_its_target() {
        let root = test_dir();
        let _ = std::fs::remove_dir_all(&root);
        let target = root.join("real-music");
        std::fs::create_dir_all(&target).unwrap();
        touch(&target.join("track.mp3"));
        let link = root.join("linked-music");
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, &link);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&target, &link);
        if linked.is_err() {
            // The host grants no symlink privilege to this test; nothing to
            // assert here.
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        let index_file = root.join("index.json");
        let events = run_scan(&link, &index_file);
        let finished = events.iter().find_map(|event| match event {
            ScanEvent::Finished { scanned, indexed } => Some((*scanned, *indexed)),
            _ => None,
        });
        // A skipped root would report zero scanned files; the followed one
        // sees the target's track (probe still fails on the dummy bytes).
        assert_eq!(finished, Some((1, 0)));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dangling_root_symlink_preserves_its_previous_entries() {
        let root = test_dir();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let link = root.join("linked-away");
        let missing_target = root.join("nowhere");
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&missing_target, &link);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&missing_target, &link);
        if linked.is_err() {
            // The host grants no symlink privilege to this test; nothing to
            // assert here.
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        // The previous index knows a song under the configured link path.
        let index_file = root.join("index.json");
        let kept = LocalFile {
            path: link.join("song.mp3"),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 90_000,
            mtime: 1,
            size: 1,
            uri: local_uri("Artist", "Album", "Song", 90),
        };
        let initial = Index::from_files(vec![kept.clone()]);
        std::fs::write(&index_file, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        // The dangling target makes the root unreadable; the prior entry must
        // survive because the walk never left the configured path.
        let events = run_scan(&link, &index_file);
        let finished = events.iter().find_map(|event| match event {
            ScanEvent::Finished { indexed, .. } => Some(*indexed),
            _ => None,
        });
        assert_eq!(finished, Some(1));
        let after = load(&index_file).unwrap();
        assert_eq!(after.files.len(), 1);
        assert_eq!(after.files[0], kept);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn overlapping_configured_folders_visit_each_file_once() {
        let root = test_dir();
        let _ = std::fs::remove_dir_all(&root);
        let sub = root.join("rock");
        std::fs::create_dir_all(&sub).unwrap();
        touch(&sub.join("track.mp3"));

        // Configuring both a folder and its child must not double-visit the
        // child: one file, counted once (probe fails on the dummy bytes, so
        // nothing is indexed either way).
        let index_file = root.join("index.json");
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        scan(&[root.clone(), sub.clone()], &index_file, tx, cancel);
        let events: Vec<ScanEvent> = rx.iter().collect();
        let finished = events.iter().find_map(|event| match event {
            ScanEvent::Finished { scanned, indexed } => Some((*scanned, *indexed)),
            _ => None,
        });
        assert_eq!(finished, Some((1, 0)));

        let _ = std::fs::remove_dir_all(&root);
    }
}
