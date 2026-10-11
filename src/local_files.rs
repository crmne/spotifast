//! The local files the chosen folders hold, listed the way the engine
//! will play them.
//!
//! This mirrors the pinned librespot's `playback/src/local_file.rs`
//! (librespot PR #1595): the same extension filter, the same symphonia
//! probe, the same tags and duration, and the same `spotify:local` URI
//! built from them, byte for byte. A list built any other way could
//! show a file the engine cannot match, or hide one it can. librespot
//! builds its lookup only when an engine starts; Spotifast rescans the
//! folders whenever the engine (re)connects or Apply hands it new ones.
//! Exposing librespot's lookup for reuse is proposed in librespot PR
//! #1792; until that lands, this file must stay in sync with the fork's
//! algorithm.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{Metadata, MetadataOptions, StandardTagKey, Tag};
use symphonia::core::probe::{Hint, ProbeResult};

/// The extensions the engine can probe and decode. librespot's list also
/// carries mp4 and m4p, but its symphonia builds without isomp4, so those
/// entries never probe successfully; they stay out here so the list never
/// promises a file the engine cannot play.
const SUPPORTED_FILE_EXTENSIONS: &[&str; 2] = &["mp3", "flac"];

/// One tagged audio file, as the engine matched it to a `spotify:local`
/// URI. The display fields are decoded from the URI, so what a row says
/// is what the engine matched, never the encoding itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFileTrack {
    /// The URI the engine plays this file as.
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: u64,
}

impl LocalFileTrack {
    /// The row as the collection table draws it: a local track whose
    /// artist and album are the tags the engine matched, carrying no
    /// Spotify ids, so no row interaction can treat it as a catalogue
    /// item with a page of its own.
    pub(crate) fn as_track(&self) -> crate::api::models::Track {
        crate::api::models::Track {
            uri: self.uri.clone(),
            name: self.title.clone(),
            duration_ms: u32::try_from(self.duration_secs.saturating_mul(1000)).unwrap_or(u32::MAX),
            artists: (!self.artist.is_empty())
                .then(|| crate::api::models::ArtistRef {
                    name: self.artist.clone(),
                    ..Default::default()
                })
                .into_iter()
                .collect(),
            album: (!self.album.is_empty()).then(|| crate::api::models::Album {
                name: self.album.clone(),
                ..Default::default()
            }),
            is_local: true,
            ..Default::default()
        }
    }
}

/// The `spotify:local` URI for these tags, encoded exactly as librespot
/// encodes them: every part form-urlencoded from its raw bytes, a missing
/// tag an empty part, the duration plain seconds.
pub(crate) fn uri_from_parts(
    artist: Option<&str>,
    album: Option<&str>,
    title: Option<&str>,
    seconds: u64,
) -> String {
    let part = |part: Option<&str>| {
        part.map(|text| form_urlencoded::byte_serialize(text.as_bytes()).collect::<String>())
            .unwrap_or_default()
    };
    format!(
        "spotify:local:{}:{}:{}:{seconds}",
        part(artist),
        part(album),
        part(title)
    )
}

/// Walks the chosen folders and returns every file the engine will be
/// able to match, named as it will name it. A folder that cannot be read
/// is warned about and skipped, as librespot does, and so is any file
/// that fails to probe or carries no title.
pub(crate) fn scan(directories: &[PathBuf]) -> Vec<LocalFileTrack> {
    let mut tracks = Vec::new();
    for directory in directories {
        if !directory.is_dir() {
            log::warn!(
                "Ignoring local file source {}: not a directory",
                directory.display()
            );
            continue;
        }
        if let Err(error) = visit_dir(directory, &mut tracks) {
            log::warn!(
                "Failed to load entries from local file source {}: {error}",
                directory.display()
            );
        }
    }
    deduplicate_and_sort(tracks)
}

fn visit_dir(dir: &Path, tracks: &mut Vec<LocalFileTrack>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            visit_dir(&path, tracks)?;
        } else {
            let Some(file_extension) = path.extension().and_then(|extension| extension.to_str())
            else {
                continue;
            };
            let lowercase_extension = file_extension.to_lowercase();
            if SUPPORTED_FILE_EXTENSIONS.contains(&lowercase_extension.as_str())
                && let Some(track) = track_from_file(&path, file_extension)
            {
                tracks.push(track);
            }
        }
    }
    Ok(())
}

/// The tags and duration of one file, as the URI the engine builds from
/// them. `None` is the warn-and-skip of librespot's loader: the file
/// cannot be opened or probed, has no audio track, or its duration
/// cannot be computed. A file without a title is skipped too, since a
/// row that names nothing cannot be told apart from any other.
fn track_from_file(audio_path: &Path, file_extension: &str) -> Option<LocalFileTrack> {
    let skipped = |reason: &str| {
        log::warn!(
            "Failed to determine URI of local file {}: {reason}",
            audio_path.display()
        );
    };
    let src = match File::open(audio_path) {
        Ok(src) => src,
        Err(error) => {
            skipped(&error.to_string());
            return None;
        }
    };
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(file_extension);
    let mut probed = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(probed) => probed,
        Err(_) => {
            skipped("Failed to probe file");
            return None;
        }
    };

    let mut artist: Option<String> = None;
    let mut album_title: Option<String> = None;
    let mut track_title: Option<String> = None;
    for tag in latest_tags(&mut probed) {
        if let Some(std_key) = tag.std_key {
            match std_key {
                StandardTagKey::Album => {
                    album_title.replace(tag.value.to_string());
                }
                StandardTagKey::Artist => {
                    artist.replace(tag.value.to_string());
                }
                StandardTagKey::TrackTitle => {
                    track_title.replace(tag.value.to_string());
                }
                _ => continue,
            }
        }
    }
    let Some(title) = track_title else {
        skipped("No track title tag");
        return None;
    };

    let Some(first_track) = probed.format.default_track() else {
        skipped("Failed to find an audio track");
        return None;
    };
    let Some(time_base) = first_track.codec_params.time_base else {
        skipped("Failed to calculate track duration");
        return None;
    };
    let Some(num_frames) = first_track.codec_params.n_frames else {
        skipped("Failed to calculate track duration");
        return None;
    };
    let seconds = time_base.calc_time(num_frames).seconds;

    Some(track_from_uri(uri_from_parts(
        artist.as_deref(),
        album_title.as_deref(),
        Some(title.as_str()),
        seconds,
    )))
}

/// The tags of the latest metadata revision librespot reads: the
/// container's own, or the ones probing found when it has none. An
/// unreadable revision reads as no tags, which skips the file, exactly
/// like librespot failing to probe its tags.
fn latest_tags(probed: &mut ProbeResult) -> Vec<Tag> {
    let metadata = latest_metadata(probed);
    metadata
        .current()
        .map(|revision| revision.tags().to_vec())
        .unwrap_or_default()
}

fn latest_metadata(probed: &mut ProbeResult) -> Metadata<'_> {
    let mut metadata = probed.format.metadata();
    if metadata.current().is_none()
        && let Some(probe_metadata) = probed.metadata.get()
    {
        metadata = probe_metadata;
    }
    let _ = metadata.skip_to_latest();
    metadata
}

/// Later files claiming the same URI replace earlier ones, as librespot's
/// HashMap insert does, and the list is sorted by artist, album, then
/// title so it reads steadily between scans.
fn deduplicate_and_sort(tracks: Vec<LocalFileTrack>) -> Vec<LocalFileTrack> {
    let mut by_uri = HashMap::new();
    for track in tracks {
        by_uri.insert(track.uri.clone(), track);
    }
    let mut sorted: Vec<_> = by_uri.into_values().collect();
    sorted.sort_by(|a, b| (&a.artist, &a.album, &a.title).cmp(&(&b.artist, &b.album, &b.title)));
    sorted
}

/// Decodes a `spotify:local` URI back into the fields a row shows. The
/// parts are form-encoded, so a `:` inside a tag never splits them. A
/// part is a bare word to the parser, so it comes back as the name of
/// its pair.
fn track_from_uri(uri: String) -> LocalFileTrack {
    let decode = |encoded: &str| {
        form_urlencoded::parse(encoded.as_bytes())
            .next()
            .map(|(name, _)| name.into_owned())
            .unwrap_or_default()
    };
    let fields: Vec<&str> = uri.split(':').collect();
    // spotify, local, artist, album, track title, duration in seconds.
    let field = |index: usize| fields.get(index).copied().unwrap_or_default();
    LocalFileTrack {
        duration_secs: field(5).parse().unwrap_or_default(),
        artist: decode(field(2)),
        album: decode(field(3)),
        title: decode(field(4)),
        uri,
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalFileTrack, deduplicate_and_sort, track_from_uri, uri_from_parts};

    #[test]
    fn missing_tags_become_empty_parts() {
        assert_eq!(uri_from_parts(None, None, None, 0), "spotify:local::::0");
        assert_eq!(
            uri_from_parts(None, Some("Late Lessons"), None, 61),
            "spotify:local::Late+Lessons::61"
        );
    }

    #[test]
    fn empty_parts_stay_empty() {
        assert_eq!(
            uri_from_parts(Some(""), Some(""), Some(""), 61),
            "spotify:local::::61"
        );
    }

    #[test]
    fn spaces_and_colons_encode_the_way_librespot_encodes_them() {
        // The example librespot itself tests against.
        assert_eq!(
            uri_from_parts(
                Some("David Wise"),
                Some("Donkey Kong Country: Tropical Freeze"),
                Some("Snomads Island"),
                127,
            ),
            "spotify:local:David+Wise:Donkey+Kong+Country%3A+Tropical+Freeze:Snomads+Island:127"
        );
    }

    #[test]
    fn non_ascii_parts_encode_as_their_bytes() {
        assert_eq!(
            uri_from_parts(Some("Björk"), Some("Homogénico"), Some("Jóga"), 232),
            "spotify:local:Bj%C3%B6rk:Homog%C3%A9nico:J%C3%B3ga:232"
        );
    }

    #[test]
    fn the_uri_decodes_back_into_display_fields() {
        let uri =
            "spotify:local:David+Wise:Donkey+Kong+Country%3A+Tropical+Freeze:Snomads+Island:127";
        let track = track_from_uri(uri.into());
        assert_eq!(track.title, "Snomads Island");
        assert_eq!(track.artist, "David Wise");
        assert_eq!(track.album, "Donkey Kong Country: Tropical Freeze");
        assert_eq!(track.duration_secs, 127);
        assert_eq!(track.uri, uri);
    }

    #[test]
    fn missing_parts_decode_to_empty_fields() {
        let track = track_from_uri("spotify:local:::Snomads+Island:127".into());
        assert_eq!(track.title, "Snomads Island");
        assert_eq!(track.artist, "");
        assert_eq!(track.album, "");
        assert_eq!(track.duration_secs, 127);
    }

    /// A row as the scan would build it, with the title overridden to tell
    /// rows that share a URI apart.
    fn track(uri: &str, title: &str) -> LocalFileTrack {
        let mut track = track_from_uri(uri.into());
        track.title = title.into();
        track
    }

    #[test]
    fn the_same_uri_keeps_one_row() {
        let sorted = deduplicate_and_sort(vec![
            track("spotify:local:a:b:c:1", "first"),
            track("spotify:local:d:e:f:2", "other"),
            track("spotify:local:a:b:c:1", "last"),
        ]);
        assert_eq!(sorted.len(), 2, "two distinct URIs stay two rows");
        assert!(
            sorted
                .iter()
                .any(|sorted| sorted.uri == "spotify:local:a:b:c:1" && sorted.title == "last"),
            "the later file wins its URI"
        );
    }

    #[test]
    fn rows_sort_by_artist_album_then_title() {
        let sorted = deduplicate_and_sort(vec![
            track_from_uri("spotify:local:B:Y:B:1".into()),
            track_from_uri("spotify:local:A:Z:A:1".into()),
            track_from_uri("spotify:local:A:A:B:1".into()),
            track_from_uri("spotify:local:A:A:A:1".into()),
        ]);
        let uris: Vec<&str> = sorted.iter().map(|sorted| sorted.uri.as_str()).collect();
        assert_eq!(
            uris,
            [
                "spotify:local:A:A:A:1",
                "spotify:local:A:A:B:1",
                "spotify:local:A:Z:A:1",
                "spotify:local:B:Y:B:1",
            ]
        );
    }
}
