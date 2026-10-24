//! Cover art and lyrics from a local file's own tags.
//!
//! Spotify knows a playlist's local file only by its artist, album, title
//! and length, so it has no cover or lyrics for one. Spotifast plays the
//! file itself from the local files folder, so it reads them from the file:
//! the picture in its ID3 `APIC` frame or FLAC `PICTURE` block, and the
//! words in a `.lrc` file beside it or its ID3 `USLT` frame or Vorbis
//! `LYRICS` comment (#1001). Every read here waits for the disk, so it runs
//! off the UI thread.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{Metadata, MetadataOptions, StandardTagKey, StandardVisualKey};
use symphonia::core::probe::Hint;

/// The scheme of a local file's cover, which [`crate::images::ArtLoader`]
/// reads from the file the rest of the URL names.
pub const ART_SCHEME: &str = "spotifast-local-art:";

/// The cover URL of the local file at `path`.
pub fn art_url(path: &Path) -> String {
    format!("{ART_SCHEME}{}", path.display())
}

/// The file a local cover URL reads from.
pub fn art_path(url: &str) -> Option<PathBuf> {
    url.strip_prefix(ART_SCHEME)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// What a local file's tags hold.
#[derive(Default)]
struct Embedded {
    picture: Option<Vec<u8>>,
    lyrics: Option<String>,
}

/// Reads the tags of the file at `path`, the ones before the audio and the
/// container's own, keeping the front cover over any other picture.
fn embedded(path: &Path) -> Embedded {
    let Ok(file) = File::open(path) else {
        return Embedded::default();
    };
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }
    let Ok(mut probed) = symphonia::default::get_probe().format(
        &hint,
        source,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return Embedded::default();
    };
    let mut found = Embedded::default();
    let mut front = false;
    let mut read = |metadata: Metadata<'_>| {
        let mut metadata = metadata;
        let Some(revision) = metadata.skip_to_latest() else {
            return;
        };
        for visual in revision.visuals() {
            let is_front = visual.usage == Some(StandardVisualKey::FrontCover);
            if found.picture.is_none() || (is_front && !front) {
                found.picture = Some(visual.data.to_vec());
                front = is_front;
            }
        }
        if found.lyrics.is_none() {
            found.lyrics = revision
                .tags()
                .iter()
                .find(|tag| tag.std_key == Some(StandardTagKey::Lyrics))
                .map(|tag| tag.value.to_string())
                .filter(|text| !text.trim().is_empty());
        }
    };
    if let Some(metadata) = probed.metadata.get() {
        read(metadata);
    }
    read(probed.format.metadata());
    found
}

/// The picture embedded in the local file at `path`, as the image file's
/// own bytes.
pub fn picture(path: &Path) -> Option<Vec<u8>> {
    embedded(path).picture
}

/// The words for the local file at `path`: a `.lrc` file of the same name
/// beside it first, then the lyrics in its tags.
pub fn lyrics(path: &Path) -> Option<crate::lyrics::Lyrics> {
    let sidecar = path.with_extension("lrc");
    if let Ok(bytes) = std::fs::read(&sidecar) {
        // A `.lrc` file saved on Windows often starts with a byte order
        // mark, and U+FEFF is not whitespace, so `trim_start` leaves it on
        // the first line: that line's stamp stops being a stamp and the
        // line is lost.
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        if let Some(found) = crate::lyrics::from_text(text) {
            return Some(found);
        }
    }
    embedded(path)
        .lyrics
        .and_then(|text| crate::lyrics::from_text(&restore_lrc_lines(&text)))
}

/// The tag reader drops the line breaks from ISO-8859-1 tags, so LRC lyrics
/// saved that way arrive as one line of stamps: break it before each.
fn restore_lrc_lines(text: &str) -> std::borrow::Cow<'_, str> {
    let stamped = |at: usize| {
        let rest = &text[at..];
        rest.starts_with('[') && rest[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
    };
    if text.contains('\n') || !stamped(0) {
        return text.into();
    }
    let mut lines = String::with_capacity(text.len() + 32);
    for (at, c) in text.char_indices() {
        // A stamp right after another one is the same line's next time.
        if at > 0 && stamped(at) && !text[..at].ends_with(']') {
            lines.push('\n');
        }
        lines.push(c);
    }
    lines.into()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An MP3 of one silent frame after an ID3v2.3 tag holding `frames`.
    pub(crate) fn mp3_with(frames: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = frames.concat();
        let size = body.len() as u32;
        let mut file = b"ID3\x03\x00\x00".to_vec();
        // Synchsafe size: seven bits a byte.
        file.extend([
            ((size >> 21) & 0x7f) as u8,
            ((size >> 14) & 0x7f) as u8,
            ((size >> 7) & 0x7f) as u8,
            (size & 0x7f) as u8,
        ]);
        file.extend(body);
        // MPEG-1 Layer III, 128 kbit/s, 44.1 kHz: 417 bytes a frame.
        for _ in 0..4 {
            let mut frame = vec![0xff, 0xfb, 0x90, 0x64];
            frame.resize(417, 0);
            file.extend(frame);
        }
        file
    }

    fn id3_frame(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut frame = id.to_vec();
        frame.extend((body.len() as u32).to_be_bytes());
        frame.extend([0, 0]);
        frame.extend(body);
        frame
    }

    pub(crate) fn apic(kind: u8, image: &[u8]) -> Vec<u8> {
        let mut body = vec![0];
        body.extend(b"image/png\0");
        body.push(kind);
        body.extend(b"\0");
        body.extend(image);
        id3_frame(b"APIC", &body)
    }

    /// A `USLT` frame in ISO-8859-1 (0) or UTF-8 (3).
    fn uslt(encoding: u8, text: &str) -> Vec<u8> {
        let mut body = vec![encoding];
        body.extend(b"eng\0");
        body.extend(text.as_bytes());
        id3_frame(b"USLT", &body)
    }

    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spotifast-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The front cover wins over another picture, wherever it sits in the
    /// tag, and the URL names the file it came from.
    #[test]
    fn a_local_file_shows_its_front_cover() {
        let dir = folder("cover");
        let song = dir.join("Song #1.mp3");
        std::fs::write(
            &song,
            mp3_with(&[apic(4, b"back cover"), apic(3, b"front cover")]),
        )
        .unwrap();
        assert_eq!(picture(&song).as_deref(), Some(&b"front cover"[..]));
        assert_eq!(art_path(&art_url(&song)), Some(song.clone()));
        assert_eq!(art_path("https://i.scdn.co/image/abc"), None);
        let bare = dir.join("bare.mp3");
        std::fs::write(&bare, mp3_with(&[])).unwrap();
        assert_eq!(picture(&bare), None);
        assert_eq!(picture(&dir.join("gone.mp3")), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A `.lrc` file beside the song comes first, then the lyrics in its
    /// tags, timed when they carry LRC stamps.
    #[test]
    fn a_local_file_reads_its_own_lyrics() {
        let dir = folder("lyrics");
        let song = dir.join("song.mp3");
        std::fs::write(&song, mp3_with(&[uslt(3, "[00:01.00]Tagged line")])).unwrap();
        let tagged = lyrics(&song).unwrap();
        assert!(tagged.synced);
        assert_eq!(tagged.lines[0].text, "Tagged line");
        assert_eq!(tagged.lines[0].at_ms, Some(1_000));

        std::fs::write(dir.join("song.lrc"), "[00:02.50]Sidecar line\n").unwrap();
        let sidecar = lyrics(&song).unwrap();
        assert_eq!(sidecar.lines[0].text, "Sidecar line");
        assert_eq!(sidecar.lines[0].at_ms, Some(2_500));

        let plain = dir.join("plain.mp3");
        std::fs::write(&plain, mp3_with(&[uslt(3, "First\nSecond")])).unwrap();
        let words = lyrics(&plain).unwrap();
        assert!(!words.synced);
        assert_eq!(words.lines.len(), 2);

        // ISO-8859-1 tags lose their line breaks on the way in.
        let latin = dir.join("latin.mp3");
        std::fs::write(
            &latin,
            mp3_with(&[uslt(0, "[00:01.00]One\n[00:02.00][00:04.00]Two")]),
        )
        .unwrap();
        let timed = lyrics(&latin).unwrap();
        assert!(timed.synced);
        let lines: Vec<_> = timed
            .lines
            .iter()
            .map(|line| (line.at_ms, line.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                (Some(1_000), "One"),
                (Some(2_000), "Two"),
                (Some(4_000), "Two")
            ]
        );

        let bare = dir.join("bare.mp3");
        std::fs::write(&bare, mp3_with(&[])).unwrap();
        assert!(lyrics(&bare).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A `.lrc` file saved with a byte order mark keeps its first line,
    /// whose stamp the mark used to hide.
    #[test]
    fn a_lyrics_file_saved_with_a_byte_order_mark_keeps_its_first_line() {
        let dir = folder("bom");
        let song = dir.join("song.mp3");
        std::fs::write(&song, mp3_with(&[])).unwrap();
        std::fs::write(
            dir.join("song.lrc"),
            b"\xef\xbb\xbf[00:01.00]First line\n[00:02.00]Second line\n",
        )
        .unwrap();
        let found = lyrics(&song).unwrap();
        assert!(found.synced);
        assert_eq!(found.lines.len(), 2);
        assert_eq!(found.lines[0].text, "First line");
        assert_eq!(found.lines[0].at_ms, Some(1_000));
        let _ = std::fs::remove_dir_all(dir);
    }
}
