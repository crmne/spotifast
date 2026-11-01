//! Local library indexing with the same URI metadata as librespot playback.
//! Directory traversal is iterative, skips links, and fails on incomplete reads.

use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use librespot_core::SpotifyUri;

use crate::api::models::{Album, ArtistRef, PlayableItem, Track};

/// Playable display rows and the matching URIs used for availability checks.
#[derive(Clone, Debug, Default)]
pub struct Library {
    pub tracks: Arc<Vec<PlayableItem>>,
    pub uris: Arc<HashMap<String, PathBuf>>,
}

/// Read the configured sources. This reads tags and must run off the UI thread.
pub fn scan(folders: &[PathBuf]) -> Result<Library, String> {
    let mut entries = Vec::new();
    for path in audio_files(folders, |folder| std::fs::read_dir(folder))? {
        if let Some(uri) = audio_uri(&path)? {
            entries.push((uri, path));
        }
    }
    Ok(library(
        entries.iter().map(|(uri, path)| (uri, path.as_path())),
    ))
}

/// Collect complete directory contents without following nested links or recursing.
/// The reader argument lets tests reproduce an unreadable child on every platform.
fn audio_files(
    folders: &[PathBuf],
    mut read_directory: impl FnMut(&Path) -> std::io::Result<std::fs::ReadDir>,
) -> Result<Vec<PathBuf>, String> {
    let mut pending = folders.to_vec();
    let mut visited = HashSet::new();
    let mut files = Vec::new();
    while let Some(folder) = pending.pop() {
        let fail = |error| {
            format!(
                "Cannot read local files folder {}: {error}",
                folder.display()
            )
        };
        let canonical = std::fs::canonicalize(&folder).map_err(fail)?;
        if !visited.insert(canonical) {
            continue;
        }
        for entry in read_directory(&folder).map_err(fail)? {
            let entry = entry.map_err(fail)?;
            let kind = entry.file_type().map_err(fail)?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file()
                && path.extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("mp3") || extension.eq_ignore_ascii_case("flac")
                })
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Probe once for both playable format and URI tags. Container metadata takes
/// precedence over probe metadata, matching the approved librespot lookup.
fn audio_uri(path: &Path) -> Result<Option<SpotifyUri>, String> {
    use symphonia::core::{
        codecs::DecoderOptions, io::MediaSourceStream, meta::StandardTagKey, probe::Hint,
    };

    let file = std::fs::File::open(path)
        .map_err(|error| format!("Cannot read local file {}: {error}", path.display()))?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let Ok(mut probed) = symphonia::default::get_probe().format(
        &hint,
        source,
        &Default::default(),
        &Default::default(),
    ) else {
        return Ok(None);
    };
    let Some(track) = probed.format.default_track() else {
        return Ok(None);
    };
    let Ok(decoder) =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())
    else {
        return Ok(None);
    };
    let params = decoder.codec_params();
    // Match the pinned player's format limits without constructing a resampling
    // filter for every file during indexing. Playback constructs that on demand.
    let playable = params
        .sample_rate
        .is_some_and(|rate| (8000..=384000).contains(&rate))
        && params.channels.is_some_and(|channels| channels.count() > 0);
    if !playable {
        return Ok(None);
    }
    let Some((time_base, frames)) = track
        .codec_params
        .time_base
        .zip(track.codec_params.n_frames)
    else {
        return Ok(None);
    };
    let duration = std::time::Duration::from_secs(time_base.calc_time(frames).seconds);
    let mut metadata = probed.format.metadata();
    if metadata.current().is_none()
        && let Some(probe_metadata) = probed.metadata.get()
    {
        metadata = probe_metadata;
    }
    let Some(revision) = metadata.skip_to_latest() else {
        return Ok(None);
    };
    let component = |key| {
        let value = revision
            .tags()
            .iter()
            .rev()
            .find(|tag| tag.std_key == Some(key))
            .map(|tag| tag.value.to_string())
            .unwrap_or_default();
        // Form encoding is part of librespot's local URI identity, including
        // spaces as '+' and the four unescaped punctuation characters.
        const ESCAPED: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
            .remove(b'*')
            .remove(b'-')
            .remove(b'.')
            .remove(b'_');
        percent_encoding::utf8_percent_encode(&value, ESCAPED)
            .to_string()
            .replace("%20", "+")
    };
    Ok(Some(SpotifyUri::Local {
        artist: component(StandardTagKey::Artist),
        album_title: component(StandardTagKey::Album),
        track_title: component(StandardTagKey::TrackTitle),
        duration,
    }))
}

/// Deduplicate by playback URI and sort rows by title, artist and URI so
/// repeated scans keep a stable display order.
fn library<'a>(entries: impl Iterator<Item = (&'a SpotifyUri, &'a Path)>) -> Library {
    let mut uris = HashMap::new();
    let mut tracks: Vec<_> = entries
        .filter_map(|(uri, path)| {
            let track = track(uri, path)?;
            match uris.entry(track.uri.clone()) {
                Entry::Occupied(_) => None,
                Entry::Vacant(entry) => {
                    entry.insert(path.to_path_buf());
                    Some(track)
                }
            }
        })
        .collect();
    tracks.sort_by_cached_key(|track| {
        (
            track.name.to_lowercase(),
            track.artist_names().to_lowercase(),
            track.uri.clone(),
        )
    });
    Library {
        tracks: Arc::new(tracks.into_iter().map(PlayableItem::Track).collect()),
        uris: Arc::new(uris),
    }
}

/// Build a local row from URI metadata, using the filename for a blank title.
/// Zero-length entries cannot supply the whole-second byte rate playback needs.
fn track(uri: &SpotifyUri, path: &Path) -> Option<Track> {
    let SpotifyUri::Local {
        artist,
        album_title,
        track_title,
        duration,
    } = uri
    else {
        return None;
    };
    // The local loader calculates a byte rate from whole seconds.
    if duration.as_secs() == 0 {
        return None;
    }
    let title = display_tag(track_title);
    let artist = display_tag(artist);
    let album = display_tag(album_title);
    Some(Track {
        uri: uri.to_uri(),
        name: if title.is_empty() {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        } else {
            title
        },
        duration_ms: u32::try_from(duration.as_millis()).unwrap_or(u32::MAX),
        artists: (!artist.is_empty())
            .then_some(ArtistRef {
                name: artist,
                ..ArtistRef::default()
            })
            .into_iter()
            .collect(),
        album: (!album.is_empty()).then_some(Album {
            name: album,
            ..Album::default()
        }),
        is_local: true,
        ..Track::default()
    })
}

/// Decode the form-encoded tag components in librespot's local URIs.
fn display_tag(tag: &str) -> String {
    percent_encoding::percent_decode_str(&tag.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private source folder whose links and files are removed after the test.
    struct Source(PathBuf);

    impl Source {
        /// Create an isolated source without reusing another test's directory.
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "spotifast-scan-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Source {
        /// Remove this test's files and links without following those links.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An unreadable nested directory must not return a partial list of songs.
    #[test]
    fn a_nested_read_failure_rejects_the_entire_scan() {
        let source = Source::new();
        let child = source.0.join("unreadable");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(source.0.join("song.mp3"), b"audio").unwrap();
        let result = audio_files(std::slice::from_ref(&source.0), |path| {
            if path == child {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            } else {
                std::fs::read_dir(path)
            }
        });
        assert!(result.unwrap_err().contains(&child.display().to_string()));
    }

    /// Indexing and the approved player must agree on every URI, including form
    /// encoding of punctuation and non-ASCII tags.
    #[test]
    fn indexed_uris_match_the_approved_player() {
        let source = Source::new();
        let value = "Björk ~ *-_.+: recordings";
        // Replace only the Vorbis comment block of the valid two-second FLAC.
        let original = include_bytes!("testdata/local-files/stereo-44100.flac");
        assert_eq!(original[42] & 0x7f, 4);
        let old_length = u32::from_be_bytes([0, original[43], original[44], original[45]]) as usize;
        let mut comments = [0u32.to_le_bytes(), 3u32.to_le_bytes()].concat();
        for key in ["TITLE", "ARTIST", "ALBUM"] {
            let tag = format!("{key}={value}");
            comments.extend((tag.len() as u32).to_le_bytes());
            comments.extend(tag.as_bytes());
        }
        let mut tagged = original[..43].to_vec();
        tagged.extend(&(comments.len() as u32).to_be_bytes()[1..]);
        tagged.extend(comments);
        tagged.extend(&original[46 + old_length..]);
        std::fs::write(source.0.join("tags.flac"), tagged).unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/testdata/local-files");
        let folders = vec![source.0.clone(), fixtures];
        let lookup = librespot_playback::local_file::create_local_file_lookup(&folders);
        let mut checked = 0;
        for (expected, path) in lookup.iter() {
            assert_eq!(audio_uri(path).unwrap().as_ref(), Some(expected));
            checked += 1;
        }
        assert_eq!(checked, 4);
    }

    /// A directory link to an ancestor cannot recurse or duplicate a song.
    #[test]
    fn directory_links_do_not_cycle() {
        let source = Source::new();
        let child = source.0.join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("song.mp3"), b"audio").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source.0, child.join("ancestor")).unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_dir(&source.0, child.join("ancestor")) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                return; // Windows requires Developer Mode or link privileges.
            }
            panic!("{error}");
        }
        assert_eq!(
            audio_files(std::slice::from_ref(&source.0), |path| std::fs::read_dir(
                path
            ))
            .unwrap(),
            [child.join("song.mp3")]
        );
    }

    #[test]
    fn scans_real_tags_and_accepts_supported_channels() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/testdata/local-files");
        let found = scan(&[folder.clone(), folder]).unwrap();
        let item = found
            .tracks
            .iter()
            .find(|item| item.name() == "Test Song")
            .expect("the tagged 44.1 kHz stereo fixture is indexed");
        assert_eq!(item.name(), "Test Song");
        assert_eq!(item.duration_ms(), 2_000);
        assert_eq!(
            item.uri(),
            "spotify:local:Test+Artist:Test+Album:Test+Song:2"
        );
        assert!(found.uris.contains_key(item.uri()));
        assert_eq!(found.uris.len(), found.tracks.len());
        assert_eq!(found.tracks.len(), 3);
        assert_eq!(
            found.uris[item.uri()].file_name().unwrap(),
            "stereo-44100.flac"
        );
        assert!(
            found
                .tracks
                .iter()
                .any(|item| item.name() == "Unsupported Rate")
        );
        assert!(
            found
                .tracks
                .iter()
                .any(|item| item.name() == "Unsupported Channels")
        );
    }

    fn local(uri: &str) -> SpotifyUri {
        SpotifyUri::from_uri(uri).unwrap()
    }

    #[test]
    fn lookup_metadata_stays_local_and_decodes_tags() {
        let uri = local("spotify:local:Bj%C3%B6rk:Home+Recordings:A%2BB%3A+C:180");
        let track = track(&uri, Path::new("music.mp3")).unwrap();
        assert_eq!(track.uri, uri.to_uri());
        assert_eq!(track.name, "A+B: C");
        assert_eq!(track.artist_names(), "Björk");
        assert_eq!(track.album.unwrap().name, "Home Recordings");
        assert_eq!(track.duration_ms, 180_000);
        assert!(track.id.is_none());
        assert!(track.is_local);
    }

    #[test]
    fn duplicate_sources_keep_one_row_and_a_stable_order() {
        let first = local("spotify:local:Artist:Album:alpha:100");
        let last = local("spotify:local:Artist:Album:Zulu:100");
        let entries = [
            (&last, Path::new("z.mp3")),
            (&first, Path::new("a.mp3")),
            (&last, Path::new("duplicate.mp3")),
        ];
        let found = library(entries.into_iter());
        assert_eq!(found.tracks.len(), 2);
        assert_eq!(found.uris.len(), 2);
        assert_eq!(found.tracks[0].uri(), first.to_uri());
        assert_eq!(found.tracks[1].uri(), last.to_uri());
    }

    #[test]
    fn missing_title_uses_the_filename_and_zero_length_is_skipped() {
        let untitled = local("spotify:local::::180");
        assert_eq!(
            track(&untitled, Path::new("Recording.mp3")).unwrap().name,
            "Recording"
        );
        assert!(track(&local("spotify:local::::0"), Path::new("empty.mp3")).is_none());
    }

    #[test]
    fn an_unreadable_source_returns_a_visible_error() {
        let missing = std::env::temp_dir().join(format!(
            "spotifast-missing-local-source-{}",
            std::process::id()
        ));
        let error = scan(std::slice::from_ref(&missing)).unwrap_err();
        assert!(error.contains(&missing.display().to_string()));
        assert!(error.starts_with("Cannot read local files folder"));
        assert!(scan(&[]).unwrap().tracks.is_empty());
    }

    #[test]
    fn corrupt_audio_is_not_offered_as_playable() {
        let path = std::env::temp_dir().join(format!(
            "spotifast-corrupt-local-{}-{}.mp3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"This is not an MP3").unwrap();
        assert!(audio_uri(&path).unwrap().is_none());
        std::fs::remove_file(path).unwrap();
    }
}
