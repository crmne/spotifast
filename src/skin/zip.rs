//! The zip reader behind `.wsz` skin files.
//!
//! A skin is a small archive of stored or deflated entries and nothing more.
//! The `zip` crate would be a new dependency for that narrow use, while
//! flate2 is already compiled for reqwest's gzip support, so inflating here
//! costs nothing new. The reader takes the sizes from the central directory,
//! which is also where streamed archives (the ones with data descriptors)
//! keep the real numbers.

use std::io::Read;

use thiserror::Error;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_RECORD: u32 = 0x0605_4b50;
const END_RECORD_LEN: usize = 22;
const MAX_COMMENT_LEN: usize = 65_535;
const STORED: u16 = 0;
const DEFLATED: u16 = 8;
const ENCRYPTED: u16 = 1;
/// The most an entry may inflate to. Skin bitmaps run to a few hundred
/// kilobytes; this stops a crafted archive from claiming gigabytes.
const ENTRY_LIMIT: usize = 64 << 20;

#[derive(Debug, Error)]
pub enum ZipError {
    #[error("not a zip archive")]
    NotAnArchive,
    #[error("the archive is truncated")]
    Truncated,
    #[error("{0} uses a compression method this reader does not support")]
    Unsupported(String),
    #[error("{0} is encrypted")]
    Encrypted(String),
    #[error("{0} is too large to be part of a skin")]
    TooLarge(String),
    #[error("{0} could not be inflated: {1}")]
    Inflate(String, std::io::Error),
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// The path inside the archive, as stored.
    pub name: String,
    method: u16,
    flags: u16,
    compressed_size: usize,
    size: usize,
    header_offset: usize,
}

impl Entry {
    /// The final path component in lower case, which is how skin files are
    /// looked up: Winamp ran on a case-insensitive file system, and skins
    /// often nest everything in a folder.
    pub fn file_name(&self) -> String {
        self.base_name().to_ascii_lowercase()
    }

    /// The final path component as stored, for files whose names are
    /// worth keeping as they are.
    pub fn base_name(&self) -> &str {
        self.name.rsplit(['/', '\\']).next().unwrap_or("")
    }

    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/') || self.name.ends_with('\\')
    }
}

pub struct Archive<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
}

impl<'a> Archive<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, ZipError> {
        let end = find_end_record(data).ok_or(ZipError::NotAnArchive)?;
        let count = usize::from(u16_at(data, end + 10)?);
        let mut cursor = u32_at(data, end + 16)? as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            if u32_at(data, cursor)? != CENTRAL_HEADER {
                return Err(ZipError::NotAnArchive);
            }
            let flags = u16_at(data, cursor + 8)?;
            let method = u16_at(data, cursor + 10)?;
            let compressed_size = u32_at(data, cursor + 20)? as usize;
            let size = u32_at(data, cursor + 24)? as usize;
            let name_len = usize::from(u16_at(data, cursor + 28)?);
            let extra_len = usize::from(u16_at(data, cursor + 30)?);
            let comment_len = usize::from(u16_at(data, cursor + 32)?);
            let header_offset = u32_at(data, cursor + 42)? as usize;
            let name = String::from_utf8_lossy(slice(data, cursor + 46, name_len)?).into_owned();
            entries.push(Entry {
                name,
                method,
                flags,
                compressed_size,
                size,
                header_offset,
            });
            cursor += 46 + name_len + extra_len + comment_len;
        }
        Ok(Self { data, entries })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>, ZipError> {
        if entry.flags & ENCRYPTED != 0 {
            return Err(ZipError::Encrypted(entry.name.clone()));
        }
        // 0xFFFF_FFFF is the zip64 marker; skins never need it.
        if entry.size > ENTRY_LIMIT || entry.compressed_size == u32::MAX as usize {
            return Err(ZipError::TooLarge(entry.name.clone()));
        }
        let data = self.data;
        let at = entry.header_offset;
        if u32_at(data, at)? != LOCAL_HEADER {
            return Err(ZipError::NotAnArchive);
        }
        let name_len = usize::from(u16_at(data, at + 26)?);
        let extra_len = usize::from(u16_at(data, at + 28)?);
        let raw = slice(data, at + 30 + name_len + extra_len, entry.compressed_size)?;
        match entry.method {
            // A stored entry is its own size; a larger one would slip past
            // the limit on what an entry says it holds.
            STORED if entry.compressed_size == entry.size => Ok(raw.to_vec()),
            STORED => Err(ZipError::TooLarge(entry.name.clone())),
            DEFLATED => {
                let mut out = Vec::with_capacity(entry.size);
                flate2::read::DeflateDecoder::new(raw)
                    .take(entry.size as u64)
                    .read_to_end(&mut out)
                    .map_err(|error| ZipError::Inflate(entry.name.clone(), error))?;
                Ok(out)
            }
            _ => Err(ZipError::Unsupported(entry.name.clone())),
        }
    }
}

/// The end-of-central-directory record sits at the very end, behind an
/// optional comment, so it is found by scanning backwards.
fn find_end_record(data: &[u8]) -> Option<usize> {
    if data.len() < END_RECORD_LEN {
        return None;
    }
    let last = data.len() - END_RECORD_LEN;
    let first = last.saturating_sub(MAX_COMMENT_LEN);
    (first..=last)
        .rev()
        .find(|&at| u32_at(data, at).ok() == Some(END_RECORD))
}

fn slice(data: &[u8], at: usize, len: usize) -> Result<&[u8], ZipError> {
    at.checked_add(len)
        .and_then(|end| data.get(at..end))
        .ok_or(ZipError::Truncated)
}

fn u16_at(data: &[u8], at: usize) -> Result<u16, ZipError> {
    let bytes = slice(data, at, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], at: usize) -> Result<u32, ZipError> {
    let bytes = slice(data, at, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// The DOS date every entry is stamped with: 1980-01-01, the epoch of the
/// format, since a skin's files carry no meaningful time.
const EPOCH_DATE: u16 = (1 << 5) | 1;

/// Writes an archive: one entry per `(name, bytes, deflate)`. The built-in
/// skin is packed with this, and tests use it to make skins to read back.
pub fn write(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    use std::io::Write;

    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data, deflate) in entries {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let (method, payload) = if *deflate {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(data).unwrap();
            (DEFLATED, encoder.finish().unwrap())
        } else {
            (STORED, data.to_vec())
        };
        let offset = out.len() as u32;
        let name_bytes = name.as_bytes();
        let header = |out: &mut Vec<u8>, signature: u32, central: bool| {
            out.extend_from_slice(&signature.to_le_bytes());
            if central {
                out.extend_from_slice(&20u16.to_le_bytes()); // made by
            }
            out.extend_from_slice(&20u16.to_le_bytes()); // needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // time
            out.extend_from_slice(&EPOCH_DATE.to_le_bytes());
            out.extend_from_slice(&crc.sum().to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra
            if central {
                out.extend_from_slice(&0u16.to_le_bytes()); // comment
                out.extend_from_slice(&0u16.to_le_bytes()); // disk
                out.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
                out.extend_from_slice(&0u32.to_le_bytes()); // external attributes
                out.extend_from_slice(&offset.to_le_bytes());
            }
            out.extend_from_slice(name_bytes);
        };
        header(&mut out, LOCAL_HEADER, false);
        out.extend_from_slice(&payload);
        header(&mut directory, CENTRAL_HEADER, true);
    }
    let directory_offset = out.len() as u32;
    out.extend_from_slice(&directory);
    out.extend_from_slice(&END_RECORD.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // disk
    out.extend_from_slice(&0u16.to_le_bytes()); // directory disk
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_and_deflated_entries_round_trip() {
        let text = b"[Text]\nNormal=#00FF00\n".as_slice();
        let bitmap: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let archive = write(&[
            ("PLEDIT.TXT", text, false),
            ("Skin/main.bmp", &bitmap, true),
        ]);
        let parsed = Archive::parse(&archive).unwrap();
        let names: Vec<String> = parsed.entries().iter().map(Entry::file_name).collect();
        assert_eq!(names, ["pledit.txt", "main.bmp"]);
        assert_eq!(parsed.read(&parsed.entries()[0]).unwrap(), text);
        assert_eq!(parsed.read(&parsed.entries()[1]).unwrap(), bitmap);
    }

    #[test]
    fn a_trailing_comment_does_not_hide_the_end_record() {
        let mut archive = write(&[("a.txt", b"hello", false)]);
        let comment = b"made with love";
        let len = archive.len();
        archive[len - 2..].copy_from_slice(&(comment.len() as u16).to_le_bytes());
        archive.extend_from_slice(comment);
        let parsed = Archive::parse(&archive).unwrap();
        assert_eq!(parsed.read(&parsed.entries()[0]).unwrap(), b"hello");
    }

    #[test]
    fn something_else_is_not_an_archive() {
        assert!(matches!(
            Archive::parse(b"BM this is a bitmap, not a zip"),
            Err(ZipError::NotAnArchive)
        ));
        assert!(matches!(Archive::parse(b""), Err(ZipError::NotAnArchive)));
    }

    /// Where the central directory starts, from the end record.
    fn directory_offset(archive: &[u8]) -> usize {
        u32_at(archive, archive.len() - END_RECORD_LEN + 16).unwrap() as usize
    }

    #[test]
    fn an_entry_that_runs_past_the_end_is_reported_rather_than_read_past() {
        let mut archive = write(&[("main.bmp", &[7u8; 300], true)]);
        let directory = directory_offset(&archive);
        let claimed = (archive.len() as u32 + 1000).to_le_bytes();
        archive[directory + 20..directory + 24].copy_from_slice(&claimed);
        let parsed = Archive::parse(&archive).unwrap();
        assert!(matches!(
            parsed.read(&parsed.entries()[0]),
            Err(ZipError::Truncated)
        ));
    }

    #[test]
    fn a_stored_entry_larger_than_it_says_is_refused() {
        let mut archive = write(&[("main.bmp", &[7u8; 300], false)]);
        let directory = directory_offset(&archive);
        archive[directory + 24..directory + 28].copy_from_slice(&10u32.to_le_bytes());
        let parsed = Archive::parse(&archive).unwrap();
        assert!(matches!(
            parsed.read(&parsed.entries()[0]),
            Err(ZipError::TooLarge(name)) if name == "main.bmp"
        ));
    }

    #[test]
    fn an_archive_without_its_end_record_is_not_one() {
        let archive = write(&[("main.bmp", &[7u8; 300], true)]);
        let cut = &archive[..archive.len() - 10];
        assert!(matches!(Archive::parse(cut), Err(ZipError::NotAnArchive)));
    }

    #[test]
    fn encrypted_and_exotic_entries_are_refused_by_name() {
        let mut archive = write(&[("secret.bmp", b"data", false)]);
        let directory = directory_offset(&archive);
        archive[directory + 8] |= ENCRYPTED as u8;
        let parsed = Archive::parse(&archive).unwrap();
        assert!(matches!(
            parsed.read(&parsed.entries()[0]),
            Err(ZipError::Encrypted(name)) if name == "secret.bmp"
        ));

        archive[directory + 8] &= !(ENCRYPTED as u8);
        archive[directory + 10] = 12; // bzip2
        let parsed = Archive::parse(&archive).unwrap();
        assert!(matches!(
            parsed.read(&parsed.entries()[0]),
            Err(ZipError::Unsupported(_))
        ));
    }

    #[test]
    fn folder_entries_and_backslashes_resolve_to_a_file_name() {
        let entry = |name: &str| Entry {
            name: name.to_string(),
            method: STORED,
            flags: 0,
            compressed_size: 0,
            size: 0,
            header_offset: 0,
        };
        assert!(entry("Skin/").is_dir());
        assert_eq!(entry("Skin\\MAIN.BMP").file_name(), "main.bmp");
        assert_eq!(entry("Nested/Deeper/Text.bmp").file_name(), "text.bmp");
    }
}
