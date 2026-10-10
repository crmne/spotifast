//! Public countdown-page metadata. A prerelease ID is not an album ID.
//! Only Spotify's response supplies the album we may navigate to.

use anyhow::{Context, ensure};
use protobuf::CodedInputStream;

use crate::api::models::{Album, ArtistRef, Image, Track};

#[derive(Clone, Debug, Default)]
pub struct Prerelease {
    pub uri: String,
    pub album: Album,
    pub release_unix: Option<i64>,
    pub tracks: Vec<Option<Track>>,
    pub saved: Option<bool>,
    pub saving: bool,
    pub save_error: Option<String>,
    pub color: Option<[u8; 3]>,
    pub artist_image: Option<String>,
}

/// Membership uses the resolved album URI. Countdown IDs name pages, not saved items.
/// The pinned protocol crate does not export its collection messages, so this
/// small request writes their documented wire fields with the existing codec.
pub fn contains_request(username: &str, uri: &str) -> anyhow::Result<Vec<u8>> {
    ensure!(
        crate::link::parse(uri).as_deref() == Some(uri) && uri.starts_with("spotify:album:"),
        "pre-save requires the resolved album URI"
    );
    let mut bytes = Vec::new();
    let mut writer = protobuf::CodedOutputStream::vec(&mut bytes);
    writer.write_string(1, username)?;
    writer.write_string(2, "collection")?;
    writer.write_bytes(3, &collection_item(uri, None)?)?;
    writer.flush()?;
    drop(writer);
    Ok(bytes)
}

/// Spotify has returned both packed and unpacked protobuf bool fields.
pub fn contains_response(bytes: &[u8]) -> anyhow::Result<bool> {
    let mut found = Vec::new();
    let mut input = CodedInputStream::from_bytes(bytes);
    while let Some(tag) = input.read_raw_tag_or_eof()? {
        match tag {
            8 => found.push(input.read_bool()?),
            10 => input.read_repeated_packed_bool_into(&mut found)?,
            _ => protobuf::rt::skip_field_for_tag(tag, &mut input)?,
        }
    }
    ensure!(
        found.len() == 1,
        "Spotify did not confirm this prerelease's saved state"
    );
    Ok(found[0])
}

/// One explicit user mutation. No pre-save is sent by opening or testing a page.
pub fn write_request(username: &str, uri: &str, saved: bool, now: i64) -> anyhow::Result<Vec<u8>> {
    ensure!(
        crate::link::parse(uri).as_deref() == Some(uri) && uri.starts_with("spotify:album:"),
        "pre-save requires the resolved album URI"
    );
    let mut bytes = Vec::new();
    let mut writer = protobuf::CodedOutputStream::vec(&mut bytes);
    writer.write_string(1, username)?;
    writer.write_string(2, "collection")?;
    writer.write_bytes(3, &collection_item(uri, Some((now, !saved)))?)?;
    writer.flush()?;
    drop(writer);
    Ok(bytes)
}

fn collection_item(uri: &str, mutation: Option<(i64, bool)>) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut writer = protobuf::CodedOutputStream::vec(&mut bytes);
    writer.write_string(1, uri)?;
    if let Some((now, removed)) = mutation {
        if removed {
            writer.write_bool(3, true)?;
        } else {
            writer.write_int64(2, now)?;
        }
    }
    writer.flush()?;
    drop(writer);
    Ok(bytes)
}

impl Prerelease {
    /// Preserve the server's preview order, hidden positions, playability and
    /// relinking. A page with provider errors must not become a successful empty list.
    pub fn album_preview(
        &mut self,
        body: &serde_json::Value,
    ) -> anyhow::Result<(Vec<Option<Track>>, u32)> {
        ensure!(
            body.get("errors")
                .is_none_or(|errors| errors.as_array().is_some_and(Vec::is_empty)),
            "Spotify could not return this track list"
        );
        let album = body
            .pointer("/data/albumUnion")
            .context("missing album response")?;
        ensure!(
            album.get("uri").and_then(|v| v.as_str()) == Some(self.album.uri.as_str()),
            "track list belongs to another album"
        );
        let page = album.get("tracksV2").context("missing track list")?;
        let total = page
            .get("totalCount")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
            .context("missing track count")?;
        let rows = page
            .get("items")
            .and_then(|v| v.as_array())
            .context("missing track rows")?;
        ensure!(
            rows.len() <= 50 && (total == 0 || !rows.is_empty()),
            "incomplete track list"
        );
        if let Some(copyrights) = album.pointer("/copyright/items") {
            self.album.copyrights = serde_json::from_value(copyrights.clone())?;
        }
        if let Some(date) = album.pointer("/date/isoString").and_then(|v| v.as_str()) {
            self.album.release_date = date.get(..10).map(str::to_string);
        }
        if let Some(images) = album.pointer("/artists/items/0/visuals/avatarImage/sources") {
            let images: Vec<Image> = serde_json::from_value(images.clone()).unwrap_or_default();
            self.artist_image = crate::api::models::pick_image(&images, 48)
                .filter(|url| url.starts_with("https://i.scdn.co/image/"))
                .map(str::to_string);
        }
        if let Some(hex) = album
            .pointer("/coverArt/extractedColors/colorLight/hex")
            .and_then(|v| v.as_str())
            .and_then(|v| v.strip_prefix('#'))
            .filter(|v| v.len() == 6)
        {
            self.color = u32::from_str_radix(hex, 16)
                .ok()
                .map(|rgb| [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]);
        }
        let tracks = rows
            .iter()
            .map(|row| {
                let Some(data) = row.get("track").filter(|data| data.is_object()) else {
                    return Ok(None);
                };
                let uri = data
                    .get("uri")
                    .and_then(|v| v.as_str())
                    .context("missing preview track URI")?;
                ensure!(
                    crate::link::parse(uri).as_deref() == Some(uri)
                        && uri.starts_with("spotify:track:"),
                    "invalid preview track URI"
                );
                let actual_uri = data
                    .pointer("/relinkingInformation/linkedTrack/uri")
                    .and_then(|v| v.as_str())
                    .filter(|uri| {
                        uri.starts_with("spotify:track:")
                            && crate::link::parse(uri).as_deref() == Some(uri)
                    })
                    .unwrap_or(uri);
                let artists = data
                    .pointer("/artists/items")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| {
                                let uri =
                                    item.get("uri").and_then(|v| v.as_str()).map(str::to_string);
                                ArtistRef {
                                    id: uri
                                        .as_deref()
                                        .and_then(crate::link::parse)
                                        .filter(|uri| uri.starts_with("spotify:artist:"))
                                        .and_then(|uri| uri.rsplit(':').next().map(str::to_string)),
                                    uri,
                                    name: item
                                        .pointer("/profile/name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or_default()
                                        .into(),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(Some(Track {
                    id: actual_uri.rsplit(':').next().map(str::to_string),
                    uri: actual_uri.into(),
                    name: data
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .into(),
                    artists,
                    duration_ms: data
                        .pointer("/duration/totalMilliseconds")
                        .and_then(|v| v.as_u64())
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or_default(),
                    is_playable: Some(
                        data.pointer("/playability/playable")
                            .and_then(|v| v.as_bool())
                            == Some(true),
                    ),
                    explicit: data
                        .pointer("/contentRating/label")
                        .and_then(|v| v.as_str())
                        == Some("EXPLICIT"),
                    album: Some(self.album.clone()),
                    ..Default::default()
                }))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok((tracks, total))
    }

    /// Decode the session's PRERELEASE extension, requiring the requested URI
    /// and a real album mapping. Unknown protobuf fields remain compatible.
    pub fn decode(bytes: &[u8], expected: &str) -> anyhow::Result<Self> {
        ensure!(
            bytes.len() <= 1024 * 1024,
            "prerelease metadata is too large"
        );
        let mut result = Self::default();
        let mut input = CodedInputStream::from_bytes(bytes);
        while let Some(tag) = input.read_raw_tag_or_eof()? {
            match tag {
                10 => result.uri = input.read_string()?,
                18 => result.release_unix = timestamp(&input.read_bytes()?)?,
                26 => result.album = release(&input.read_bytes()?)?,
                _ => protobuf::rt::skip_field_for_tag(tag, &mut input)?,
            }
        }
        ensure!(
            result.uri == expected,
            "prerelease response belongs to another page"
        );
        let canonical = crate::link::parse(&result.album.uri).context("missing album mapping")?;
        ensure!(
            canonical == result.album.uri && canonical.starts_with("spotify:album:"),
            "invalid album mapping"
        );
        ensure!(!result.album.name.is_empty(), "missing prerelease title");
        result.album.id = canonical.rsplit(':').next().unwrap_or_default().into();
        result.album.album_type = Some("album".into());
        Ok(result)
    }

    /// Four countdown units, absent when no release instant is known or when
    /// it has passed. This also bounds timer-driven native UI updates.
    pub fn countdown(&self, now: i64) -> Option<[i64; 4]> {
        let seconds = self.release_unix?.saturating_sub(now);
        (seconds > 0).then_some([
            seconds / 86400,
            seconds % 86400 / 3600,
            seconds % 3600 / 60,
            seconds % 60,
        ])
    }
}

fn timestamp(bytes: &[u8]) -> anyhow::Result<Option<i64>> {
    let mut input = CodedInputStream::from_bytes(bytes);
    let mut seconds = None;
    while let Some(tag) = input.read_raw_tag_or_eof()? {
        match tag {
            8 => seconds = Some(input.read_int64()?),
            _ => protobuf::rt::skip_field_for_tag(tag, &mut input)?,
        }
    }
    if let Some(seconds) = seconds {
        jiff::Timestamp::from_second(seconds).context("invalid release timestamp")?;
    }
    Ok(seconds)
}

fn release(bytes: &[u8]) -> anyhow::Result<Album> {
    let mut album = Album::default();
    let mut input = CodedInputStream::from_bytes(bytes);
    while let Some(tag) = input.read_raw_tag_or_eof()? {
        match tag {
            10 => album.uri = input.read_string()?,
            26 => album.name = input.read_string()?,
            34 => album.artists.push(artist(&input.read_bytes()?)?),
            42 => {
                let image = image(&input.read_bytes()?)?;
                // The standard artwork loader receives only Spotify's HTTPS
                // image host, never an arbitrary URL supplied by metadata.
                if image.url.starts_with("https://i.scdn.co/image/") {
                    album.images.push(image);
                }
            }
            _ => protobuf::rt::skip_field_for_tag(tag, &mut input)?,
        }
    }
    Ok(album)
}

fn artist(bytes: &[u8]) -> anyhow::Result<ArtistRef> {
    let mut artist = ArtistRef::default();
    let mut input = CodedInputStream::from_bytes(bytes);
    while let Some(tag) = input.read_raw_tag_or_eof()? {
        match tag {
            10 => artist.uri = Some(input.read_string()?),
            18 => artist.name = input.read_string()?,
            _ => protobuf::rt::skip_field_for_tag(tag, &mut input)?,
        }
    }
    artist.id = artist
        .uri
        .as_deref()
        .and_then(crate::link::parse)
        .filter(|uri| uri.starts_with("spotify:artist:"))
        .and_then(|uri| uri.rsplit(':').next().map(str::to_string));
    Ok(artist)
}

fn image(bytes: &[u8]) -> anyhow::Result<Image> {
    let mut image = Image::default();
    let mut input = CodedInputStream::from_bytes(bytes);
    while let Some(tag) = input.read_raw_tag_or_eof()? {
        match tag {
            10 => image.url = input.read_string()?,
            24 => image.width = Some(input.read_uint32()?),
            32 => image.height = Some(input.read_uint32()?),
            _ => protobuf::rt::skip_field_for_tag(tag, &mut input)?,
        }
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    const URI: &str = "spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL";
    const FIXTURE: &[u8] = include_bytes!("testdata/prerelease.pb");

    #[test]
    fn real_public_response_maps_to_a_different_album_id() {
        let page = Prerelease::decode(FIXTURE, URI).unwrap();
        assert_eq!(page.album.uri, "spotify:album:5BYKG4MHfySZthe2W6r7n4");
        assert_eq!(page.album.name, "Grand Theft Auto VI: The Album");
        assert_eq!(page.album.artists[0].name, "Grand Theft Auto VI");
        assert_eq!(page.album.images.len(), 3);
        assert_eq!(page.release_unix, Some(1795035600));
        assert!(page.tracks.is_empty());
    }

    #[test]
    fn malformed_missing_and_wrong_entity_metadata_are_refused() {
        assert!(Prerelease::decode(&[], URI).is_err());
        assert!(Prerelease::decode(&FIXTURE[..FIXTURE.len() - 1], URI).is_err());
        assert!(Prerelease::decode(FIXTURE, "spotify:prerelease:other").is_err());
    }

    #[test]
    fn missing_timestamp_and_unknown_fields_are_safe() {
        let mut input = CodedInputStream::from_bytes(FIXTURE);
        let mut output = Vec::new();
        while let Some(tag) = input.read_raw_tag_or_eof().unwrap() {
            let bytes = input.read_bytes().unwrap();
            if tag != 18 {
                let mut writer = protobuf::CodedOutputStream::vec(&mut output);
                writer.write_bytes(tag >> 3, &bytes).unwrap();
                writer.flush().unwrap();
            }
        }
        output.extend_from_slice(&[0xa0, 0x06, 0x01]);
        let page = Prerelease::decode(&output, URI).unwrap();
        assert_eq!(page.release_unix, None);
        assert_eq!(page.countdown(0), None);
    }

    #[test]
    fn countdown_handles_future_and_released_pages() {
        let page = Prerelease::decode(FIXTURE, URI).unwrap();
        let release = page.release_unix.unwrap();
        assert_eq!(page.countdown(release - 86400), Some([1, 0, 0, 0]));
        assert_eq!(page.countdown(release - 3661), Some([0, 1, 1, 1]));
        assert_eq!(page.countdown(release), None);
    }

    #[test]
    fn preview_keeps_hidden_positions_and_only_real_playable_uris() {
        let mut page = Prerelease::decode(FIXTURE, URI).unwrap();
        let body = serde_json::from_str(include_str!("testdata/prerelease-album.json")).unwrap();
        let (tracks, total) = page.album_preview(&body).unwrap();
        assert_eq!(total, 34);
        assert_eq!(tracks.len(), 34);
        assert_eq!(tracks[0].as_ref().unwrap().is_playable, Some(false));
        assert_eq!(tracks[1].as_ref().unwrap().is_playable, Some(true));
        assert_eq!(
            tracks[1].as_ref().unwrap().uri,
            "spotify:track:45TB2WNxWu2X4MmqmCAJnF"
        );
        assert_eq!(tracks[2].as_ref().unwrap().is_playable, Some(false));
    }

    #[test]
    fn failed_and_truncated_preview_are_not_successful_empty_lists() {
        let mut page = Prerelease::decode(FIXTURE, URI).unwrap();
        assert!(
            page.album_preview(&serde_json::json!({"errors":[{"message":"failed"}]}))
                .is_err()
        );
        assert!(page.album_preview(&serde_json::json!({"data":{"albumUnion":{"uri":page.album.uri,"tracksV2":{"totalCount":34,"items":[]}}}})).is_err());
    }

    #[test]
    fn collection_state_and_mutation_use_the_resolved_album_uri() {
        assert!(!contains_response(&[10, 1, 0]).unwrap());
        assert!(contains_response(&[8, 1]).unwrap());
        assert!(contains_response(&[]).is_err());
        assert!(contains_response(&[10, 2, 1, 0]).is_err());
        let album = "spotify:album:5BYKG4MHfySZthe2W6r7n4";
        let save = write_request("test-user", album, true, 1700000000).unwrap();
        let mut input = CodedInputStream::from_bytes(&save);
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(10));
        assert_eq!(input.read_string().unwrap(), "test-user");
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(18));
        assert_eq!(input.read_string().unwrap(), "collection");
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), Some(26));
        assert_eq!(
            input.read_bytes().unwrap(),
            collection_item(album, Some((1700000000, false))).unwrap()
        );
        assert_ne!(
            collection_item(album, Some((1700000000, false))).unwrap(),
            collection_item(album, Some((1700000000, true))).unwrap()
        );
        assert_eq!(input.read_raw_tag_or_eof().unwrap(), None);
        assert!(write_request("test-user", URI, true, 1700000000).is_err());
        assert!(!contains_request("test-user", album).unwrap().is_empty());
    }
}
