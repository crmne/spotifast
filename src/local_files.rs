//! The local files the chosen folders hold, listed the way the engine
//! will play them.
//!
//! This mirrors the pinned librespot's `playback/src/local_file.rs`
//! (librespot PR #1595): the same extension filter, the same symphonia
//! probe, the same tags and duration, the same title falling back to the
//! file stem, and the same `spotify:local` URI built from them, byte for
//! byte. A list built any other way could
//! show a file the engine cannot match, or hide one it can. librespot
//! builds its lookup only when an engine starts; Spotifast rescans the
//! folders whenever the engine (re)connects or Apply hands it new ones.
//! Exposing librespot's lookup for reuse is proposed in librespot PR
//! #1792; until that lands, this file must stay in sync with the fork's
//! algorithm.
//!
//! The tags' embedded pictures librespot does not read at all; the
//! ID3v2 and FLAC picture frames are parsed here and served through the
//! artwork cache under `local-cover:` URIs.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{Metadata, MetadataOptions, StandardTagKey, Tag};
use symphonia::core::probe::{Hint, ProbeResult};

use crate::images::ArtLoader;

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
    /// The `local-cover:` URI of the picture embedded in the file's tags,
    /// placed in the artwork cache during the scan, when it has one.
    pub(crate) art: Option<String>,
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
            album: {
                // An album to show needs a name, a picture, or both; a file
                // with neither shows the plain placeholder.
                let images = self.art.as_deref().map(|url| {
                    vec![crate::api::models::Image {
                        url: url.to_string(),
                        width: None,
                        height: None,
                    }]
                });
                (images.is_some() || !self.album.is_empty()).then(|| crate::api::models::Album {
                    name: self.album.clone(),
                    images: images.unwrap_or_default(),
                    ..Default::default()
                })
            },
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
/// able to match, named as it will name it, with each file's embedded
/// picture placed in the artwork cache under the URI its row carries.
/// A folder that cannot be read is warned about and skipped, as librespot
/// does, and so is any file that fails to probe or carries no title.
pub(crate) fn scan(directories: &[PathBuf], art: &ArtLoader) -> Vec<LocalFileTrack> {
    let mut tracks = Vec::new();
    for directory in directories {
        if !directory.is_dir() {
            log::warn!(
                "Ignoring local file source {}: not a directory",
                directory.display()
            );
            continue;
        }
        if let Err(error) = visit_dir(directory, &mut tracks, art) {
            log::warn!(
                "Failed to load entries from local file source {}: {error}",
                directory.display()
            );
        }
    }
    deduplicate_and_sort(tracks)
}

fn visit_dir(dir: &Path, tracks: &mut Vec<LocalFileTrack>, art: &ArtLoader) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            visit_dir(&path, tracks, art)?;
        } else {
            let Some(file_extension) = path.extension().and_then(|extension| extension.to_str())
            else {
                continue;
            };
            let lowercase_extension = file_extension.to_lowercase();
            if SUPPORTED_FILE_EXTENSIONS.contains(&lowercase_extension.as_str())
                && let Some(track) = track_from_file(&path, &lowercase_extension, art)
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
/// cannot be computed. A file without a title tag is named by its file
/// stem, exactly as the engine names it, so untagged collections that
/// carry their track names in the file name still list and play.
fn track_from_file(
    audio_path: &Path,
    lowercase_extension: &str,
    art: &ArtLoader,
) -> Option<LocalFileTrack> {
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
    hint.with_extension(lowercase_extension);
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
    let title = matched_title(track_title, audio_path);

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

    let track = track_from_uri(uri_from_parts(
        artist.as_deref(),
        album_title.as_deref(),
        title.as_deref(),
        seconds,
    ));
    Some(with_embedded_cover(
        track,
        audio_path,
        lowercase_extension,
        art,
    ))
}

/// The `local-cover:` artwork URI for a scanned local file's picture.
/// It is the key the artwork cache files are named by, hashed so the URI
/// carries no path and stays an opaque key, stable across rescans of the
/// same matched file.
pub(crate) fn cover_uri(uri: &str) -> String {
    let digest = Sha1::digest(uri.as_bytes());
    let mut key = String::with_capacity("local-cover:".len() + 2 * digest.len());
    key.push_str("local-cover:");
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

/// Gives a matched row its embedded picture, if the file carries one the
/// container parsers can read and the artwork cache accepts: the row's
/// `art` then names the cache entry holding those bytes. A file without a
/// readable picture keeps `None` and shows the placeholder, and so does a
/// picture the cache refuses, without failing the scan.
fn with_embedded_cover(
    mut track: LocalFileTrack,
    audio_path: &Path,
    lowercase_extension: &str,
    art: &ArtLoader,
) -> LocalFileTrack {
    if let Some(picture) = embedded_cover(audio_path, lowercase_extension) {
        let url = cover_uri(&track.uri);
        if art.embed(&url, &picture) {
            track.art = Some(url);
        }
    }
    track
}

/// The picture embedded in a scanned file's tags, if it has one.
/// Symphonia reads a file's text tags but exposes no pictures, so the
/// containers' own picture frames are read directly here, bounded to a
/// size the artwork cache accepts.
fn embedded_cover(audio_path: &Path, lowercase_extension: &str) -> Option<Vec<u8>> {
    let mut src = File::open(audio_path).ok()?;
    match lowercase_extension {
        "mp3" => id3v2_cover(&mut src).ok().flatten(),
        "flac" => flac_cover(&mut src).ok().flatten(),
        _ => None,
    }
}

/// A u32 from four big-endian bytes.
fn be_u32(bytes: [u8; 4]) -> u32 {
    u32::from_be_bytes(bytes)
}

/// A v2.4 syncsafe u32: seven bits per byte, most significant first.
fn syncsafe(bytes: [u8; 4]) -> u32 {
    (u32::from(bytes[0]) << 21)
        | (u32::from(bytes[1]) << 14)
        | (u32::from(bytes[2]) << 7)
        | u32::from(bytes[3])
}

/// Moves `src` past `bytes` more bytes.
fn skip_past(src: &mut impl Seek, bytes: u64) -> io::Result<()> {
    src.seek(SeekFrom::Current(bytes as i64))?;
    Ok(())
}

/// The picture in the ID3v2 tag an mp3 file starts with, if it carries
/// one: an APIC frame naming a front cover when there is one, else the
/// first readable APIC frame. Frames the tag compressed, encrypted, or
/// unsynchronised are skipped rather than misread, and v2.2's PIC frames
/// and tags unknown to this reader read as no picture.
fn id3v2_cover(src: &mut (impl Read + Seek)) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; 10];
    src.read_exact(&mut header)?;
    if !header.starts_with(b"ID3") {
        return Ok(None);
    }
    let (major, flags) = (header[3], header[5]);
    if !(3..=4).contains(&major) || flags & 0x80 != 0 {
        return Ok(None);
    }
    let mut remaining = u64::from(syncsafe([header[6], header[7], header[8], header[9]]));
    if flags & 0x40 != 0 {
        // An extended header sits between the tag header and the frames.
        let mut size_bytes = [0u8; 4];
        src.read_exact(&mut size_bytes)?;
        let size = if major == 3 {
            // v2.3 names the bytes that follow; the four just read are
            // on top of that.
            u64::from(be_u32(size_bytes)) + 4
        } else {
            // v2.4 names the whole extended header, these bytes included.
            u64::from(syncsafe(size_bytes))
        };
        remaining = remaining.saturating_sub(size);
        skip_past(src, size.saturating_sub(4))?;
    }

    let mut fallback = None;
    while remaining >= 10 {
        let mut frame = [0u8; 10];
        src.read_exact(&mut frame)?;
        remaining -= 10;
        let id = [frame[0], frame[1], frame[2], frame[3]];
        if !id.iter().all(|byte| byte.is_ascii_alphanumeric()) {
            break; // The tag's padding begins; its frames are over.
        }
        let size = if major == 3 {
            u64::from(be_u32([frame[4], frame[5], frame[6], frame[7]]))
        } else {
            u64::from(syncsafe([frame[4], frame[5], frame[6], frame[7]]))
        };
        if size > remaining {
            break; // A truncated frame; nothing after it can be trusted.
        }
        remaining -= size;
        let format_flags = frame[9];
        let unreadable = if major == 3 {
            // v2.3: compression 0x80, encryption 0x40.
            format_flags & 0xc0 != 0
        } else {
            // v2.4: compression 0x08, encryption 0x04, unsync 0x02.
            format_flags & 0x0e != 0
        };
        if id != *b"APIC" || unreadable || size as usize > crate::images::MAX_ART_BYTES {
            skip_past(src, size)?;
            continue;
        }
        let mut body = vec![0u8; size as usize];
        src.read_exact(&mut body)?;
        let mut picture = body.as_slice();
        if major == 4 && format_flags & 0x01 != 0 {
            // A data length indicator precedes the frame's content.
            picture = picture.get(4..).unwrap_or(&[]);
        }
        match apic_picture(picture) {
            Some((true, bytes)) => return Ok(Some(bytes)),
            Some((false, bytes)) => fallback = fallback.or(Some(bytes)),
            None => {}
        }
    }
    Ok(fallback)
}

/// The picture in an APIC frame: its encoding byte, a null-terminated
/// Latin-1 MIME type, the picture type, a description terminated the way
/// its encoding terminates, then the picture bytes. Answered with whether
/// it is the front cover.
fn apic_picture(frame: &[u8]) -> Option<(bool, Vec<u8>)> {
    let encoding = *frame.first()?;
    let mut rest = frame.get(1..)?;
    let mime_end = rest.iter().position(|byte| *byte == 0)?;
    rest = rest.get(mime_end + 1..)?;
    let picture_type = *rest.first()?;
    rest = rest.get(1..)?;
    // The description ends in a single null in the single-byte encodings
    // and a double null on an even boundary in the UTF-16 ones.
    let terminator = match encoding {
        1 | 2 => rest
            .as_chunks::<2>()
            .0
            .iter()
            .position(|pair| *pair == [0, 0])
            .map(|pairs| 2 * pairs + 2)?,
        _ => rest.iter().position(|byte| *byte == 0).map(|at| at + 1)?,
    };
    let picture = rest.get(terminator..)?.to_vec();
    (!picture.is_empty()).then_some((picture_type == 3, picture))
}

/// The picture in a FLAC file's PICTURE metadata block, if it carries
/// one: the block naming a front cover when there is one, else the first
/// readable picture block. An ID3v2 tag may legally sit before the FLAC
/// stream; it is skipped whole, for its own APIC frames aside, the
/// picture block is where FLAC writers put art.
fn flac_cover(src: &mut (impl Read + Seek)) -> io::Result<Option<Vec<u8>>> {
    let mut magic = [0u8; 4];
    src.read_exact(&mut magic)?;
    if magic.starts_with(b"ID3") {
        // An ID3v2 tag may sit before the stream; skip it whole. Its own
        // APIC frames aside, the picture block is where FLAC writers put
        // art.
        let mut rest = [0u8; 6];
        src.read_exact(&mut rest)?;
        let size = u64::from(syncsafe([rest[2], rest[3], rest[4], rest[5]]));
        skip_past(src, size)?;
        src.read_exact(&mut magic)?;
    }
    if magic != *b"fLaC" {
        return Ok(None);
    }
    let mut fallback = None;
    loop {
        let mut block = [0u8; 4];
        src.read_exact(&mut block)?;
        let last = block[0] & 0x80 != 0;
        let kind = block[0] & 0x7f;
        let size = u64::from(u32::from_be_bytes([0, block[1], block[2], block[3]]));
        if kind != 6 || size > crate::images::MAX_ART_BYTES as u64 {
            skip_past(src, size)?;
        } else {
            let mut body = vec![0u8; size as usize];
            src.read_exact(&mut body)?;
            match flac_picture(&body) {
                Some((true, bytes)) => return Ok(Some(bytes)),
                Some((false, bytes)) => fallback = fallback.or(Some(bytes)),
                None => {}
            }
        }
        if last {
            return Ok(fallback);
        }
    }
}

/// The picture in a PICTURE metadata block: its type, a length-prefixed
/// MIME type and description, four dimensions nobody reads here, and the
/// length-prefixed picture bytes.
fn flac_picture(block: &[u8]) -> Option<(bool, Vec<u8>)> {
    let picture_type = be_u32(block.get(0..4)?.try_into().ok()?);
    let mut at = 4usize;
    let mime_len = be_u32(block.get(at..at + 4)?.try_into().ok()?) as usize;
    at += 4 + mime_len;
    let description_len = be_u32(block.get(at..at + 4)?.try_into().ok()?) as usize;
    at += 4 + description_len;
    at += 16; // Width, height, colour depth, and colour count.
    let data_len = be_u32(block.get(at..at + 4)?.try_into().ok()?) as usize;
    at += 4;
    let picture = block.get(at..at + data_len)?.to_vec();
    (!picture.is_empty()).then_some((picture_type == 3, picture))
}

/// The title a file is matched and listed under: its title tag, or the
/// file stem when the tag is missing or empty. This mirrors the fork's
/// `matched_title` byte for byte, so the URI a row carries is always the
/// URI the engine built for the same file.
fn matched_title(track_title: Option<String>, audio_path: &Path) -> Option<String> {
    track_title.filter(|title| !title.is_empty()).or_else(|| {
        audio_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    })
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
        art: None,
        uri,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LocalFileTrack, deduplicate_and_sort, matched_title, track_from_uri, uri_from_parts,
    };
    use std::path::Path;

    #[test]
    fn the_title_tag_wins_over_the_file_name() {
        assert_eq!(
            matched_title(Some("Snomads Island".into()), Path::new("bootleg.mp3")).as_deref(),
            Some("Snomads Island")
        );
    }

    #[test]
    fn a_missing_or_empty_title_falls_back_to_the_file_stem() {
        assert_eq!(
            matched_title(None, Path::new("10 Feet (v1.2).mp3")).as_deref(),
            Some("10 Feet (v1.2)")
        );
        assert_eq!(
            matched_title(Some(String::new()), Path::new("10 Feet (v1.2).mp3")).as_deref(),
            Some("10 Feet (v1.2)")
        );
    }

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

    // An APIC frame's body: encoding, null-terminated Latin-1 MIME type,
    // picture type, a description terminated the way the encoding says,
    // then the picture bytes.
    fn apic_body(encoding: u8, picture_type: u8, description: &[u8], picture: &[u8]) -> Vec<u8> {
        let mut body = vec![encoding];
        body.extend_from_slice(b"image/png\0");
        body.push(picture_type);
        body.extend_from_slice(description);
        body.extend_from_slice(picture);
        body
    }

    /// An ID3v2 tag holding `frames`, each `(id, flags, body)`.
    fn id3v2_tag(major: u8, tag_flags: u8, frames: &[([u8; 4], u8, Vec<u8>)]) -> Vec<u8> {
        let mut tag = Vec::new();
        for (id, flags, body) in frames {
            tag.extend_from_slice(id);
            let size = body.len() as u32
                + if major == 4 && flags & 0x01 != 0 {
                    4 // The data length indicator the flag promises.
                } else {
                    0
                };
            let size_bytes = if major == 3 {
                size.to_be_bytes()
            } else {
                // A v2.4 size is syncsafe: seven bits per byte.
                [
                    (size >> 21) as u8,
                    ((size >> 14) & 0x7f) as u8,
                    ((size >> 7) & 0x7f) as u8,
                    (size & 0x7f) as u8,
                ]
            };
            tag.extend_from_slice(&size_bytes);
            tag.push(0);
            tag.push(*flags);
            if major == 4 && flags & 0x01 != 0 {
                tag.extend_from_slice(&[0, 0, 0, 0]); // The indicator itself.
            }
            tag.extend_from_slice(body);
        }
        let mut bytes = vec![b'I', b'D', b'3', major, 0, tag_flags];
        let size = tag.len() as u32;
        bytes.extend_from_slice(&[
            (size >> 21) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ]);
        bytes.extend_from_slice(&tag);
        bytes
    }

    fn id3_cover_of(bytes: &[u8]) -> Option<Vec<u8>> {
        id3v2_cover(&mut Cursor::new(bytes)).expect("the tag reads")
    }

    fn flac_cover_of(bytes: &[u8]) -> Option<Vec<u8>> {
        flac_cover(&mut Cursor::new(bytes)).expect("the stream reads")
    }

    use super::{cover_uri, flac_cover, id3v2_cover};
    use std::io::Cursor;

    #[test]
    fn an_mp3s_apic_frame_is_read() {
        let picture = b"\x89PNG old cover".to_vec();
        let tag = id3v2_tag(
            3,
            0,
            &[
                (*b"TIT2", 0, b"Snomads Island".to_vec()),
                (*b"APIC", 0, apic_body(0, 3, b"\0", &picture)),
            ],
        );
        assert_eq!(
            id3_cover_of(&tag),
            Some(picture),
            "the front cover after a text frame is the picture"
        );
    }

    #[test]
    fn a_front_cover_wins_and_any_picture_stands_in() {
        let back = b"back cover".to_vec();
        let front = b"front cover".to_vec();
        let both = id3v2_tag(
            3,
            0,
            &[
                (*b"APIC", 0, apic_body(0, 4, b"back\0", &back)),
                (*b"APIC", 0, apic_body(0, 3, b"front\0", &front)),
            ],
        );
        assert_eq!(id3_cover_of(&both), Some(front));
        let only_back = id3v2_tag(3, 0, &[(*b"APIC", 0, apic_body(0, 4, b"\0", &back))]);
        assert_eq!(
            id3_cover_of(&only_back),
            Some(b"back cover".to_vec()),
            "a tag with no front cover keeps its one picture"
        );
    }

    #[test]
    fn v2_4_sizes_and_descriptions_read_the_same() {
        let picture = b"\xff\xd8 jpeg".to_vec();
        // A v2.4 tag: syncsafe sizes, a data length indicator on the frame,
        // and a UTF-8 description terminated by one null.
        let tag = id3v2_tag(
            4,
            0,
            &[(*b"APIC", 0x01, apic_body(3, 3, b"the cover\0", &picture))],
        );
        assert_eq!(id3_cover_of(&tag), Some(picture.clone()));
        // A UTF-16 description is terminated by a double null.
        let utf16 = id3v2_tag(
            4,
            0,
            &[(*b"APIC", 0, apic_body(1, 3, b"\x00U\x00,\0\0", &picture))],
        );
        assert_eq!(
            id3_cover_of(&utf16),
            Some(picture),
            "a UTF-16 description ends at its double null"
        );
    }

    #[test]
    fn tags_this_reader_cannot_trust_read_as_no_picture() {
        let picture = b"cover".to_vec();
        // Unsynchronised, and v2.2, and no tag at all.
        let unsynchronised = id3v2_tag(4, 0x80, &[(*b"APIC", 0, apic_body(0, 3, b"\0", &picture))]);
        assert_eq!(id3_cover_of(&unsynchronised), None);
        let mut v2_2 = id3v2_tag(3, 0, &[(*b"APIC", 0, apic_body(0, 3, b"\0", &picture))]);
        v2_2[3] = 2;
        assert_eq!(id3_cover_of(&v2_2), None);
        assert_eq!(id3_cover_of(b"mp3 data, no tag"), None);

        // A frame announcing more bytes than the file holds: the read
        // fails, which the scan's `ok()` reads as no picture.
        let mut truncated = id3v2_tag(3, 0, &[(*b"APIC", 0, apic_body(0, 3, b"\0", &picture))]);
        let size = truncated.len();
        truncated.truncate(size - 3);
        assert!(id3v2_cover(&mut Cursor::new(&truncated)).is_err());

        // A compressed or encrypted frame is skipped, not misread.
        let compressed = id3v2_tag(3, 0, &[(*b"APIC", 0x80, apic_body(0, 3, b"\0", &picture))]);
        assert_eq!(id3_cover_of(&compressed), None);
    }

    /// A FLAC PICTURE metadata block body.
    fn picture_block(picture_type: u32, picture: &[u8]) -> Vec<u8> {
        let mut block = Vec::new();
        block.extend_from_slice(&picture_type.to_be_bytes());
        block.extend_from_slice(&9u32.to_be_bytes());
        block.extend_from_slice(b"image/png");
        block.extend_from_slice(&0u32.to_be_bytes());
        block.extend_from_slice(&[0u8; 16]); // Dimensions nobody reads.
        block.extend_from_slice(&(picture.len() as u32).to_be_bytes());
        block.extend_from_slice(picture);
        block
    }

    /// A FLAC stream header: `fLaC`, then metadata blocks, each flagged
    /// last or not.
    fn flac_stream(blocks: &[(bool, u8, Vec<u8>)]) -> Vec<u8> {
        let mut bytes = b"fLaC".to_vec();
        for (last, kind, body) in blocks {
            bytes.push((u8::from(*last) << 7) | (kind & 0x7f));
            bytes.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..4]);
            bytes.extend_from_slice(body);
        }
        bytes
    }

    #[test]
    fn a_flac_picture_block_is_read() {
        let picture = b"\x89PNG flac cover".to_vec();
        let stream = flac_stream(&[
            (false, 4, vec![0, 0, 0, 0]), // A vorbis comment block walks past.
            (true, 6, picture_block(3, &picture)),
        ]);
        assert_eq!(
            flac_cover_of(&stream),
            Some(picture),
            "the PICTURE block's bytes are the cover"
        );
    }

    #[test]
    fn flac_prefers_a_front_cover_and_ignores_junk_before_the_stream() {
        let leaflet = b"leaflet".to_vec();
        let front = b"front".to_vec();
        let stream = flac_stream(&[
            (false, 6, picture_block(2, &leaflet)), // A band leaflet.
            (true, 6, picture_block(3, &front)),
        ]);
        assert_eq!(flac_cover_of(&stream), Some(front));

        let picture = b"cover".to_vec();
        let mut with_id3 = id3v2_tag(4, 0, &[(*b"TIT2", 0, b"t".to_vec())]);
        with_id3.extend_from_slice(&flac_stream(&[(true, 6, picture_block(3, &picture))]));
        assert_eq!(
            flac_cover_of(&with_id3),
            Some(picture),
            "an ID3v2 tag before fLaC is skipped whole"
        );

        assert_eq!(flac_cover_of(b"not a flac file"), None);
        assert_eq!(
            flac_cover_of(&flac_stream(&[(true, 4, vec![0, 0, 0, 0])])),
            None,
            "a stream with no picture block has no cover"
        );
    }

    #[test]
    fn a_rows_album_carries_its_embedded_picture() {
        let art = "local-cover:abc".to_string();
        let with_art = LocalFileTrack {
            uri: "spotify:local:a::c:1".into(),
            title: "c".into(),
            artist: String::new(),
            album: String::new(),
            duration_secs: 1,
            art: Some(art.clone()),
        };
        let track = with_art.as_track();
        assert_eq!(track.album.as_ref().unwrap().name, "");
        assert_eq!(track.image(640), Some(art.as_str()));

        let bare = track_from_uri("spotify:local:a::c:1".into());
        assert!(
            bare.as_track().album.is_none(),
            "with no album and no picture there is no album to show"
        );
    }

    /// The scan's picture pipeline end to end: a file's embedded picture
    /// lands in the artwork cache, and the row's art URI reads it back
    /// without any network at all.
    #[test]
    fn an_embedded_picture_is_cached_under_the_rows_art_uri() {
        let dir = std::env::temp_dir().join(format!(
            "spotifast-local-cover-{}-{}",
            std::process::id(),
            std::time::Instant::now().elapsed().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let loader = crate::images::ArtLoader::new(
            reqwest::Client::new(),
            runtime.handle().clone(),
            dir.clone(),
        );

        let picture = b"\x89PNG the cover".to_vec();
        let file = dir.join("track.mp3");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &file,
            id3v2_tag(3, 0, &[(*b"APIC", 0, apic_body(0, 3, b"\0", &picture))]),
        )
        .unwrap();

        let row = super::with_embedded_cover(
            track_from_uri("spotify:local:a:b:c:1".into()),
            &file,
            "mp3",
            &loader,
        );
        let url = row.art.expect("the picture was embedded");
        assert_eq!(url, cover_uri(&row.uri));
        assert!(url.starts_with("local-cover:"));
        assert_eq!(
            runtime.block_on(loader.fetch(&url)).unwrap().to_vec(),
            picture,
            "the art URI reads back exactly the embedded picture"
        );

        // A file with no picture, or one the cache refuses, keeps None.
        let bare = dir.join("bare.mp3");
        std::fs::write(&bare, id3v2_tag(3, 0, &[(*b"TIT2", 0, b"t".to_vec())])).unwrap();
        assert!(
            super::with_embedded_cover(
                track_from_uri("spotify:local:a:b:c:1".into()),
                &bare,
                "mp3",
                &loader
            )
            .art
            .is_none()
        );

        runtime.shutdown_timeout(std::time::Duration::from_secs(5));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cover_uris_are_opaque_and_stable() {
        assert_eq!(
            cover_uri("spotify:local:a:b:c:1"),
            cover_uri("spotify:local:a:b:c:1")
        );
        assert_ne!(
            cover_uri("spotify:local:a:b:c:1"),
            cover_uri("spotify:local:a:b:c:2")
        );
        assert!(cover_uri("spotify:local:a:b:c:1").starts_with("local-cover:"));
    }
}
