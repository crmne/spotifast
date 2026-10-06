//! Catalogue reads over the streaming session, shaped as the Web API answers
//! them. The session speaks the protocol Spotify's own clients use, which has
//! no per-app quota, so a playlist someone else owns opens without waiting on
//! the shared app's rate limit.

use std::collections::{BTreeSet, HashMap};

use base64::Engine as _;
use librespot_core::{FileId, Session, SpotifyUri, error::ErrorKind, spotify_id::SpotifyId};
use librespot_metadata::artist::Artist as SessionArtist;
use librespot_metadata::image::Images;
use librespot_metadata::playlist::attribute::PlaylistAttributes;
use librespot_metadata::playlist::item::PlaylistItem as SessionRow;
use librespot_metadata::{
    Album as SessionAlbum, Episode as SessionEpisode, Playlist as SessionPlaylist,
    Track as SessionTrack,
};
use librespot_protocol::extended_metadata::{
    BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery,
};
use librespot_protocol::extension_kind::ExtensionKind;
use protobuf::{EnumOrUnknown, Message as _};

use crate::api::ApiError;
use crate::api::models::{
    Album, ArtistRef, Episode, Image, Owner, Page, PlayableItem, Playlist, PlaylistItem, Show,
    Track, TrackCount, UserRef,
};

const IMAGE_HOST: &str = "https://i.scdn.co/image/";

/// Why the session could not answer a read.
#[derive(Debug)]
pub enum Failure {
    /// Spotify's answer is final and the Web API would give the same, so
    /// show it rather than spend the shared quota asking again.
    Definitive(ApiError),
    /// The session could not be reached or understood; the Web API may do
    /// better.
    Retry(anyhow::Error),
}

/// Any error the session raises is a retry, unless the playlist read
/// itself was refused, which `refused` tells apart.
impl From<librespot_core::Error> for Failure {
    fn from(error: librespot_core::Error) -> Self {
        Self::Retry(error.into())
    }
}

/// How the playlist read failed: Spotify not having the playlist, or not
/// letting this account see it, is final and what the Web API would say
/// too. Only the playlist's own endpoint speaks for the playlist; any
/// other call's refusal is its own.
fn refused(error: librespot_core::Error) -> Failure {
    match error.kind {
        ErrorKind::NotFound => Failure::Definitive(ApiError::Status {
            status: 404,
            message: "Not Found".into(),
        }),
        ErrorKind::PermissionDenied => Failure::Definitive(ApiError::Status {
            status: 403,
            message: "Forbidden".into(),
        }),
        _ => error.into(),
    }
}

/// A playlist's header: name, owner, cover, and snapshot.
pub async fn playlist(session: &Session, id: &str) -> Result<Playlist, Failure> {
    let list = window(session, id, 0, 0).await?;
    let mut playlist = header(id, &list);
    // `Playlist::owner_name` reads a missing name as Spotify's for its own
    // lists and as the id for anyone else's, until the app finds the name
    // where the Web API gave it: the account's own, or the library list.
    if let Some(owner) = playlist.owner.id.clone().filter(|owner| owner != "spotify") {
        playlist.owner.display_name = user_display_name(session, &owner).await;
    }
    Ok(playlist)
}

/// One page of a playlist's rows, as the Web API pages them.
pub async fn items(
    session: &Session,
    id: &str,
    offset: u32,
    limit: u32,
) -> Result<Page<PlaylistItem>, Failure> {
    rows_page(session, id, offset, limit, true).await
}

/// One page of a playlist's rows with who added them and when, but no
/// song details: what the app samples a long list's tail for.
pub async fn sample(
    session: &Session,
    id: &str,
    offset: u32,
    limit: u32,
) -> Result<Page<PlaylistItem>, Failure> {
    rows_page(session, id, offset, limit, false).await
}

async fn rows_page(
    session: &Session,
    id: &str,
    offset: u32,
    limit: u32,
    details: bool,
) -> Result<Page<PlaylistItem>, Failure> {
    let list = window(session, id, offset, limit).await?;
    let rows = rows(&list, offset, limit);
    let total = total(&list);
    complete(rows.len(), offset, limit, total)?;
    let playables = if details {
        metadata(session, rows.iter().map(|row| &row.id)).await?
    } else {
        HashMap::new()
    };
    let items = rows.iter().map(|row| item(row, &playables)).collect();
    Ok(page(items, total, offset, limit))
}

/// A window Spotify cut short of the rows that remain is refused rather
/// than paged past, since the next page would start after rows nobody was
/// shown.
fn complete(count: usize, offset: u32, limit: u32, total: u32) -> Result<(), Failure> {
    if (count as u32) < limit && offset.saturating_add(count as u32) < total {
        return Err(Failure::Retry(anyhow::anyhow!(
            "window at {offset} answered {count} rows of {total}"
        )));
    }
    Ok(())
}

/// The rows at `offset`, at most `limit` of them; see [`range`].
fn rows(list: &SessionPlaylist, offset: u32, limit: u32) -> &[SessionRow] {
    let contents = &list.contents;
    &contents.items[range(contents.items.len(), contents.position, offset, limit)]
}

/// A page as the Web API shapes one; `next` is only ever tested for
/// presence, and is there while rows remain beyond this page.
fn page(items: Vec<PlaylistItem>, total: u32, offset: u32, limit: u32) -> Page<PlaylistItem> {
    let next = (offset.saturating_add(items.len() as u32) < total).then(String::new);
    Page {
        items,
        total,
        limit,
        offset,
        next,
    }
}

/// The songs of Spotify's radio `station`, in Spotify's order, with their
/// details from one batched request. Spotify mixes a station afresh each
/// time it is resolved, so the list returned here is the one to play.
pub async fn station(session: &Session, station: &str) -> anyhow::Result<Vec<Track>> {
    let context = session.spclient().get_context(station).await?;
    let uris = station_songs(&context);
    anyhow::ensure!(!uris.is_empty(), "Spotify has no songs for this radio");
    let found = metadata(session, uris.iter())
        .await
        .map_err(|failure| match failure {
            Failure::Definitive(error) => anyhow::anyhow!("{error}"),
            Failure::Retry(error) => error,
        })?;
    Ok(uris
        .iter()
        .filter_map(|uri| uri.to_uri().ok())
        .filter_map(|uri| match found.get(&uri) {
            Some(PlayableItem::Track(track)) => Some(track.clone()),
            _ => None,
        })
        .collect())
}

/// Each song of a resolved station once, named by URI or by raw id.
fn station_songs(context: &librespot_protocol::context::Context) -> Vec<SpotifyUri> {
    let mut seen = BTreeSet::new();
    context
        .pages
        .iter()
        .flat_map(|page| &page.tracks)
        .filter_map(
            |track| match track.uri.as_deref().filter(|uri| !uri.is_empty()) {
                Some(uri) => SpotifyUri::from_uri(uri).ok(),
                None => SpotifyId::from_raw(track.gid.as_deref()?)
                    .ok()
                    .map(|id| SpotifyUri::Track { id }),
            },
        )
        .filter(|uri| matches!(uri, SpotifyUri::Track { .. }))
        .filter(|uri| uri.to_uri().is_ok_and(|text| seen.insert(text)))
        .collect()
}

/// Which of the given show URIs Spotify's metadata marks as audiobooks, in
/// one batched request. librespot cannot play them. A show Spotify does not
/// answer for is treated as a podcast, so it stays visible.
pub async fn audiobook_shows(session: &Session, uris: &[String]) -> anyhow::Result<Vec<String>> {
    let request = show_request(uris);
    if request.entity_request.is_empty() {
        return Ok(Vec::new());
    }
    let response = session.spclient().get_extended_metadata(request).await?;
    Ok(audiobooks_in(&response))
}

fn show_request(uris: &[String]) -> BatchedEntityRequest {
    let mut request = BatchedEntityRequest::new();
    for uri in uris.iter().filter(|uri| uri.starts_with("spotify:show:")) {
        request.entity_request.push(EntityRequest {
            entity_uri: uri.clone(),
            query: vec![ExtensionQuery {
                extension_kind: EnumOrUnknown::new(ExtensionKind::SHOW_V4),
                ..Default::default()
            }],
            ..Default::default()
        });
    }
    request
}

fn audiobooks_in(response: &BatchedExtensionResponse) -> Vec<String> {
    let mut audiobooks = Vec::new();
    for array in &response.extended_metadata {
        if array.extension_kind.enum_value() != Ok(ExtensionKind::SHOW_V4) {
            continue;
        }
        for data in &array.extension_data {
            if !matches!(data.header.status_code, 0 | 200) {
                continue;
            }
            let is_audiobook = data
                .extension_data
                .as_ref()
                .and_then(|any| {
                    librespot_protocol::metadata::Show::parse_from_bytes(&any.value).ok()
                })
                .is_some_and(|show| show.is_audiobook());
            if is_audiobook {
                audiobooks.push(data.entity_uri.clone());
            }
        }
    }
    audiobooks
}

/// The display name behind a user id, from the profile view Spotify's
/// clients read; `None` when nothing answers.
pub async fn user_display_name(session: &Session, user_id: &str) -> Option<String> {
    let bytes = session
        .spclient()
        .get_user_profile(user_id, Some(0), Some(0))
        .await
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("name")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// The list's header and the `length` rows from `from`; none for the header
/// alone.
async fn window(
    session: &Session,
    id: &str,
    from: u32,
    length: u32,
) -> Result<SessionPlaylist, Failure> {
    let uri = SpotifyUri::Playlist {
        id: SpotifyId::from_base62(id)?,
        user: None,
    };
    SessionPlaylist::get_range(session, &uri, from as usize, length as usize)
        .await
        .map_err(refused)
}

/// The owner's user id, which the session decorates onto the playlist's URI.
fn owner_of(list: &SessionPlaylist) -> &str {
    match &list.id {
        SpotifyUri::Playlist {
            user: Some(user), ..
        } => user,
        _ => "",
    }
}

/// How many rows the whole list holds.
fn total(list: &SessionPlaylist) -> u32 {
    u32::try_from(list.length).unwrap_or_default()
}

fn header(id: &str, list: &SessionPlaylist) -> Playlist {
    let attributes = &list.attributes;
    let owner = non_empty(owner_of(list));
    Playlist {
        id: id.to_string(),
        name: attributes.name.clone(),
        uri: format!("spotify:playlist:{id}"),
        description: non_empty(&attributes.description),
        images: playlist_images(attributes),
        owner: Owner {
            uri: owner.as_ref().map(|owner| format!("spotify:user:{owner}")),
            id: owner,
            display_name: None,
        },
        collaborative: attributes.is_collaborative,
        snapshot_id: Some(snapshot(&list.revision)),
        items_count: Some(TrackCount { total: total(list) }),
        ..Default::default()
    }
}

/// The Web API's snapshot id is the playlist revision in base64, so a
/// header read here matches one read there, and the disk cache with both.
fn snapshot(revision: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(revision)
}

/// The cover, from the sized pictures when the list carries them, else the
/// one picture every list has.
fn playlist_images(attributes: &PlaylistAttributes) -> Vec<Image> {
    let sized: Vec<Image> = attributes
        .picture_sizes
        .iter()
        .filter_map(|picture| {
            Some(Image {
                url: image_url(&picture.url)?,
                width: match picture.target_name.as_str() {
                    "small" => Some(60),
                    "default" => Some(300),
                    "large" => Some(640),
                    _ => None,
                },
                height: None,
            })
        })
        .collect();
    if !sized.is_empty() {
        return sized;
    }
    if attributes.picture.is_empty() {
        return Vec::new();
    }
    vec![Image {
        url: format!("{IMAGE_HOST}{}", FileId::from_raw(&attributes.picture)),
        width: None,
        height: None,
    }]
}

/// A picture reference as the list gives it: a web address, or an image
/// URI naming a file on Spotify's image host.
fn image_url(reference: &str) -> Option<String> {
    if reference.starts_with("https://") || reference.starts_with("http://") {
        return Some(reference.to_string());
    }
    reference
        .strip_prefix("spotify:image:")
        .map(|hex| format!("{IMAGE_HOST}{hex}"))
}

/// The rows at `offset`, at most `limit` of them, within rows that start at
/// `position`: the `from` asked for, or zero should Spotify send the whole
/// list.
fn range(len: usize, position: i32, offset: u32, limit: u32) -> std::ops::Range<usize> {
    let position = u32::try_from(position).unwrap_or_default();
    let start = (offset.saturating_sub(position) as usize).min(len);
    let end = start.saturating_add(limit as usize).min(len);
    start..end
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

/// A row as the Web API gives it. The same song can sit in a list twice,
/// so its details are looked up, not taken. A local file has no details
/// to look up; its URI carries what the Web API would have said of it.
fn item(row: &SessionRow, playables: &HashMap<String, PlayableItem>) -> PlaylistItem {
    let uri = row.id.to_uri().unwrap_or_default();
    PlaylistItem {
        added_at: iso8601(row.attributes.timestamp.as_timestamp_ms()),
        added_by: non_empty(&row.attributes.added_by).map(|id| UserRef { id: Some(id) }),
        is_local: matches!(row.id, SpotifyUri::Local { .. }),
        item: playables
            .get(&uri)
            .cloned()
            .or_else(|| local_track(&row.id, uri)),
        ..Default::default()
    }
}

/// A local file as a track, from the only description Spotify holds of
/// it: the artist, album, title, and length written into its URI. Rows for
/// local files must stay in the list, or every row after them moves.
fn local_track(id: &SpotifyUri, uri: String) -> Option<PlayableItem> {
    let SpotifyUri::Local {
        artist,
        album_title,
        track_title,
        duration,
    } = id
    else {
        return None;
    };
    Some(PlayableItem::Track(Track {
        uri,
        name: decode_local_part(track_title),
        duration_ms: u32::try_from(duration.as_millis()).unwrap_or(u32::MAX),
        artists: non_empty(&decode_local_part(artist))
            .map(|name| ArtistRef {
                name,
                ..Default::default()
            })
            .into_iter()
            .collect(),
        album: {
            let name = decode_local_part(album_title);
            non_empty(&name).map(|name| Album {
                name,
                ..Default::default()
            })
        },
        is_local: true,
        ..Default::default()
    }))
}

/// One piece of a `spotify:local:` URI as the tag text it encodes. The
/// pieces are form-encoded: spaces are `+`, other bytes percent-encoded.
fn decode_local_part(part: &str) -> String {
    if !part.contains(['+', '%']) {
        return part.to_string();
    }
    let plus = part.replace('+', " ");
    percent_encoding::percent_decode_str(&plus)
        .decode_utf8()
        .map_or_else(|_| part.to_string(), |text| text.into_owned())
}

/// Track and episode details for the given URIs, in one batched request.
/// A URI Spotify says it has no item for is absent, as the Web API leaves
/// such a row's item empty. An answer Spotify could not give, or one this
/// cannot read, fails the page rather than pass for rows without songs.
async fn metadata(
    session: &Session,
    uris: impl Iterator<Item = &SpotifyUri>,
) -> Result<HashMap<String, PlayableItem>, Failure> {
    let (request, asked) = batch(uris);
    if asked.is_empty() {
        return Ok(HashMap::new());
    }
    let response = session.spclient().get_extended_metadata(request).await?;
    answers(&asked, response)
}

/// One request for the details of every track and episode among `uris`,
/// with the URIs it asks for. A playlist can hold the same song twice; it
/// is asked for once.
fn batch<'a>(
    uris: impl Iterator<Item = &'a SpotifyUri>,
) -> (BatchedEntityRequest, BTreeSet<String>) {
    let mut request = BatchedEntityRequest::new();
    let mut asked = BTreeSet::new();
    for uri in uris {
        let kind = match uri {
            SpotifyUri::Track { .. } => ExtensionKind::TRACK_V4,
            SpotifyUri::Episode { .. } => ExtensionKind::EPISODE_V4,
            _ => continue,
        };
        let Ok(text) = uri.to_uri() else {
            continue;
        };
        if !asked.insert(text.clone()) {
            continue;
        }
        request.entity_request.push(EntityRequest {
            entity_uri: text,
            query: vec![ExtensionQuery {
                extension_kind: EnumOrUnknown::new(kind),
                ..Default::default()
            }],
            ..Default::default()
        });
    }
    (request, asked)
}

/// The details Spotify answered with for the URIs `asked`, by URI. Spotify marks each
/// answer: a 404 is a song it no longer has, a 451 one it withholds for
/// legal reasons, and the Web API shows both as a row without one. Any
/// other refusal, a provider error over the whole batch, bytes that do not
/// read as a song, or a URI left unanswered is a retry, since a page cached
/// without those songs would stay wrong. Kinds the request never asked for
/// are passed over.
fn answers(
    asked: &BTreeSet<String>,
    response: BatchedExtensionResponse,
) -> Result<HashMap<String, PlayableItem>, Failure> {
    let retry = |reason: String| Failure::Retry(anyhow::anyhow!(reason));
    let mut unanswered: BTreeSet<&str> = asked.iter().map(String::as_str).collect();
    let mut playables = HashMap::new();
    for array in response.extended_metadata {
        // Spotify marks a batch it answered with 200, as it does each row.
        let provider = array.header.provider_error_status;
        if !matches!(provider, 0 | 200) {
            return Err(retry(format!("metadata provider answered {provider}")));
        }
        let kind = match array.extension_kind.enum_value() {
            Ok(kind @ (ExtensionKind::TRACK_V4 | ExtensionKind::EPISODE_V4)) => kind,
            _ => continue,
        };
        for data in array.extension_data {
            unanswered.remove(data.entity_uri.as_str());
            match data.header.status_code {
                0 | 200 => {}
                404 | 451 => continue,
                code => return Err(retry(format!("{} answered {code}", data.entity_uri))),
            }
            let item = data
                .extension_data
                .as_ref()
                .and_then(|any| playable(kind, &any.value))
                .ok_or_else(|| retry(format!("unreadable metadata for {}", data.entity_uri)))?;
            playables.insert(data.entity_uri, item);
        }
    }
    if let Some(uri) = unanswered.first() {
        return Err(retry(format!("no metadata answer for {uri}")));
    }
    Ok(playables)
}

fn playable(kind: ExtensionKind, bytes: &[u8]) -> Option<PlayableItem> {
    match kind {
        ExtensionKind::TRACK_V4 => {
            let message = librespot_protocol::metadata::Track::parse_from_bytes(bytes).ok()?;
            Some(PlayableItem::Track(track(
                SessionTrack::try_from(&message).ok()?,
            )))
        }
        ExtensionKind::EPISODE_V4 => {
            let message = librespot_protocol::metadata::Episode::parse_from_bytes(bytes).ok()?;
            // The typed episode keeps the show's name but not its id, which
            // the row's link to the show needs.
            let show_id = SpotifyId::from_raw(message.show.gid())
                .ok()
                .and_then(|id| id.to_base62().ok());
            Some(PlayableItem::Episode(episode(
                SessionEpisode::try_from(&message).ok()?,
                show_id,
            )))
        }
        _ => None,
    }
}

fn track(track: SessionTrack) -> Track {
    Track {
        id: track.id.to_id().ok(),
        uri: track.id.to_uri().unwrap_or_default(),
        name: track.name,
        duration_ms: u32::try_from(track.duration).unwrap_or_default(),
        explicit: track.is_explicit,
        artists: track.artists.iter().map(artist_ref).collect(),
        album: Some(album(track.album)),
        track_number: Some(u32::try_from(track.number).unwrap_or_default()),
        disc_number: Some(u32::try_from(track.disc_number).unwrap_or_default()),
        popularity: Some(track.popularity.clamp(0, 100) as u8),
        ..Default::default()
    }
}

fn album(album: SessionAlbum) -> Album {
    Album {
        id: album.id.to_id().unwrap_or_default(),
        uri: album.id.to_uri().unwrap_or_default(),
        images: images(&album.covers),
        artists: album.artists.iter().map(artist_ref).collect(),
        label: non_empty(&album.label),
        name: album.name,
        ..Default::default()
    }
}

fn episode(episode: SessionEpisode, show_id: Option<String>) -> Episode {
    Episode {
        id: episode.id.to_id().unwrap_or_default(),
        uri: episode.id.to_uri().unwrap_or_default(),
        name: episode.name,
        duration_ms: u32::try_from(episode.duration).unwrap_or_default(),
        description: episode.description,
        images: images(&episode.covers),
        release_date: iso8601(episode.publish_time.as_timestamp_ms()),
        explicit: episode.is_explicit,
        show: show_id.map(|id| Show {
            uri: format!("spotify:show:{id}"),
            id,
            name: episode.show_name,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn artist_ref(artist: &SessionArtist) -> ArtistRef {
    ArtistRef {
        id: artist.id.to_id().ok(),
        name: artist.name.clone(),
        uri: artist.id.to_uri().ok(),
    }
}

fn images(images: &Images) -> Vec<Image> {
    images
        .iter()
        .map(|image| Image {
            url: format!("{IMAGE_HOST}{}", image.id),
            width: u32::try_from(image.width).ok().filter(|width| *width > 0),
            height: u32::try_from(image.height)
                .ok()
                .filter(|height| *height > 0),
        })
        .collect()
}

/// Milliseconds since the epoch as the Web API writes a time; `None` for
/// the zero a list gives when it never recorded one.
fn iso8601(timestamp_ms: i64) -> Option<String> {
    jiff::Timestamp::from_millisecond(timestamp_ms)
        .ok()
        .filter(|_| timestamp_ms > 0)
        .map(|time| time.to_string())
}

#[cfg(test)]
mod tests {
    use librespot_metadata::Metadata as _;
    use librespot_protocol::playlist4_external::{Item, SelectedListContent};

    use super::*;

    const TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
    const EPISODE: &str = "spotify:episode:7GhIk7Il098yCjg4BQjzvb";

    fn playlist_uri() -> SpotifyUri {
        SpotifyUri::Playlist {
            id: SpotifyId::from_base62("37i9dQZF1DXbIbVYph0Zr5").unwrap(),
            user: None,
        }
    }

    /// A row as the session lists it, with the details Spotify attaches.
    fn row(uri: &str, added_by: &str, timestamp: i64) -> SessionRow {
        let mut message = Item::new();
        message.set_uri(uri.into());
        let attributes = message.attributes.mut_or_insert_default();
        attributes.set_added_by(added_by.into());
        attributes.set_timestamp(timestamp);
        SessionRow::try_from(&message).unwrap()
    }

    fn playables() -> HashMap<String, PlayableItem> {
        HashMap::from([
            (
                TRACK.to_string(),
                PlayableItem::Track(Track {
                    uri: TRACK.into(),
                    name: "Never Gonna Give You Up".into(),
                    ..Default::default()
                }),
            ),
            (
                EPISODE.to_string(),
                PlayableItem::Episode(Episode {
                    uri: EPISODE.into(),
                    name: "Episode 1".into(),
                    ..Default::default()
                }),
            ),
        ])
    }

    #[test]
    fn times_are_written_as_the_web_api_writes_them() {
        assert_eq!(
            iso8601(1_700_000_000_000).as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(iso8601(0), None, "never recorded");
    }

    #[test]
    fn pictures_resolve_to_the_image_host() {
        assert_eq!(
            image_url("spotify:image:ab12").as_deref(),
            Some("https://i.scdn.co/image/ab12")
        );
        assert_eq!(
            image_url("https://mosaic.scdn.co/300/x").as_deref(),
            Some("https://mosaic.scdn.co/300/x")
        );
        assert_eq!(image_url("spotify:mosaic:x"), None);
    }

    #[test]
    fn rows_are_taken_relative_to_where_the_answer_starts() {
        // Spotify answered the `from` asked for.
        assert_eq!(range(50, 100, 100, 50), 0..50);
        // Spotify sent the whole list.
        assert_eq!(range(120, 0, 100, 50), 100..120);
        // Nothing past the end.
        assert_eq!(range(0, 500, 500, 50), 0..0);
    }

    #[test]
    fn the_header_reads_as_a_web_api_playlist() {
        let mut message = SelectedListContent::new();
        message.set_revision(vec![0, 0, 0, 7]);
        message.set_length(3);
        message.set_owner_username("someone".into());
        let attributes = message.attributes.mut_or_insert_default();
        attributes.set_name("Road trip".into());
        attributes.set_collaborative(true);
        attributes.set_picture(vec![0xab, 0xcd]);
        let list = SessionPlaylist::parse(&message, &playlist_uri()).unwrap();

        let playlist = header("pl1", &list);
        let mut nobody = SelectedListContent::new();
        nobody.set_length(0);
        let nobody = header(
            "pl2",
            &SessionPlaylist::parse(&nobody, &playlist_uri()).unwrap(),
        );
        assert_eq!(nobody.owner.id, None, "no owner named means none");
        assert_eq!(nobody.owner.uri, None);
        assert!(nobody.images.is_empty(), "no picture at all");
        assert_eq!(playlist.name, "Road trip");
        assert_eq!(playlist.uri, "spotify:playlist:pl1");
        assert_eq!(playlist.description, None, "empty means none");
        assert!(playlist.collaborative);
        assert_eq!(playlist.owner.id.as_deref(), Some("someone"));
        assert_eq!(playlist.owner.uri.as_deref(), Some("spotify:user:someone"));
        assert_eq!(playlist.track_total(), 3);
        assert_eq!(playlist.snapshot_id.as_deref(), Some("AAAABw"));
        assert!(playlist.images[0].url.starts_with(IMAGE_HOST));
    }

    #[test]
    fn a_row_carries_who_added_it_and_when() {
        let mapped = item(&row(TRACK, "friend", 1_700_000_000_000), &playables());
        assert_eq!(
            mapped.added_by.and_then(|adder| adder.id).as_deref(),
            Some("friend")
        );
        assert_eq!(mapped.added_at.as_deref(), Some("2023-11-14T22:13:20Z"));
        assert!(!mapped.is_local);
        assert_eq!(
            mapped.item.as_ref().map(PlayableItem::uri),
            Some(TRACK),
            "the details are looked up by uri"
        );

        // A local file keeps its row, described from its URI, as the Web
        // API describes one.
        let local = item(
            &row("spotify:local:Artist:Album:Song:180", "", 0),
            &playables(),
        );
        assert!(local.is_local);
        assert_eq!(local.added_at, None);
        let Some(PlayableItem::Track(track)) = local.item else {
            panic!("a local file is a track");
        };
        assert!(track.is_local);
        assert_eq!(track.name, "Song");
        assert_eq!(track.artist_names(), "Artist");
        assert_eq!(
            track.album.map(|album| album.name).as_deref(),
            Some("Album")
        );
        assert_eq!(track.duration_ms, 180_000);
        assert_eq!(track.uri, "spotify:local:Artist:Album:Song:180");

        // The URI pieces carry form-encoded tag text; the row decodes them
        // for display.
        let encoded = item(
            &row(
                "spotify:local:David+Wise:A+Monkey%27s+Island:Cavern+Plaza:213",
                "",
                0,
            ),
            &playables(),
        );
        let Some(PlayableItem::Track(encoded)) = encoded.item else {
            panic!("a local file is a track");
        };
        assert_eq!(encoded.name, "Cavern Plaza");
        assert_eq!(encoded.artist_names(), "David Wise");
        assert_eq!(
            encoded.album.map(|album| album.name).as_deref(),
            Some("A Monkey's Island")
        );
        assert_eq!(encoded.duration_ms, 213_000);
    }

    #[test]
    fn a_track_listed_twice_keeps_its_details_both_times() {
        let playables = playables();
        let rows = [row(TRACK, "", 0), row(TRACK, "", 0)];
        let items: Vec<PlaylistItem> = rows.iter().map(|row| item(row, &playables)).collect();
        assert!(
            items.iter().all(|item| item.item.is_some()),
            "the second occurrence must not go without"
        );
    }

    #[test]
    fn an_episode_listed_twice_keeps_its_details_both_times() {
        let playables = playables();
        let rows = [row(EPISODE, "", 0), row(TRACK, "", 0), row(EPISODE, "", 0)];
        let items: Vec<PlaylistItem> = rows.iter().map(|row| item(row, &playables)).collect();
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(item.item, Some(PlayableItem::Episode(_))))
                .count(),
            2
        );
    }

    /// Sixteen bytes Spotify would hand out as a gid, and the id it names.
    const GID: [u8; 16] = [
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd,
        0xef,
    ];
    fn id_of(gid: &[u8]) -> String {
        SpotifyId::from_raw(gid).unwrap().to_base62().unwrap()
    }

    fn cover(file_id: u8, width: i32) -> librespot_protocol::metadata::Image {
        let mut image = librespot_protocol::metadata::Image::new();
        image.set_file_id(vec![file_id; 20]);
        image.set_width(width);
        image.set_height(width);
        image
    }

    /// A track as the metadata service describes it, mapped to what the
    /// Web API would have said: uris, the album cover on the image host,
    /// and the numbers the rows show.
    #[test]
    fn a_track_maps_with_its_album_and_artists() {
        let mut message = librespot_protocol::metadata::Track::new();
        message.set_gid(GID.to_vec());
        message.set_name("Never Gonna Give You Up".into());
        message.set_duration(213_000);
        message.set_number(1);
        message.set_disc_number(1);
        message.set_popularity(150);
        message.set_explicit(true);
        let mut artist = librespot_protocol::metadata::Artist::new();
        artist.set_gid(vec![0x02; 16]);
        artist.set_name("Rick Astley".into());
        message.artist.push(artist);
        let album = message.album.mut_or_insert_default();
        album.set_gid(vec![0x03; 16]);
        album.set_name("Whenever You Need Somebody".into());
        album.set_label("RCA".into());
        let covers = album.cover_group.mut_or_insert_default();
        covers.image.push(cover(0xab, 300));
        covers.image.push(cover(0xcd, 0));

        let track = track(SessionTrack::try_from(&message).unwrap());
        assert_eq!(track.uri, format!("spotify:track:{}", id_of(&GID)));
        assert_eq!(track.id.as_deref(), Some(id_of(&GID).as_str()));
        assert_eq!(track.name, "Never Gonna Give You Up");
        assert_eq!(track.duration_ms, 213_000);
        assert_eq!(track.track_number, Some(1));
        assert_eq!(track.disc_number, Some(1));
        assert!(track.explicit, "the E badge");
        assert_eq!(track.popularity, Some(100), "held to the Web API's range");
        assert_eq!(
            track.is_playable, None,
            "the session says nothing of the market"
        );
        assert_eq!(track.artist_names(), "Rick Astley");
        assert_eq!(
            track.artists[0].uri.as_deref(),
            Some(format!("spotify:artist:{}", id_of(&[0x02; 16])).as_str())
        );
        let album = track.album.as_ref().unwrap();
        assert_eq!(album.uri, format!("spotify:album:{}", id_of(&[0x03; 16])));
        assert_eq!(album.label.as_deref(), Some("RCA"));
        assert_eq!(
            track.image(300),
            Some(format!("{IMAGE_HOST}{}", "ab".repeat(20)).as_str()),
            "the cover is on the image host at its stated size"
        );
        assert_eq!(album.images[1].width, None, "a size Spotify left out");
    }

    /// An episode brings its show and its date along.
    #[test]
    fn an_episode_maps_with_its_show_and_date() {
        let mut message = librespot_protocol::metadata::Episode::new();
        message.set_gid(GID.to_vec());
        message.set_name("Episode 1".into());
        message.set_duration(1_800_000);
        message.set_description("The first one.".into());
        message.set_explicit(true);
        let published = message.publish_time.mut_or_insert_default();
        published.set_year(2024);
        published.set_month(5);
        published.set_day(6);
        message
            .cover_image
            .mut_or_insert_default()
            .image
            .push(cover(0xcd, 640));
        let show = message.show.mut_or_insert_default();
        show.set_name("The Show".into());
        show.set_gid(vec![0x04; 16]);

        let show_id = SpotifyId::from_raw(message.show.gid())
            .ok()
            .and_then(|id| id.to_base62().ok());
        let episode = episode(SessionEpisode::try_from(&message).unwrap(), show_id);
        assert_eq!(episode.uri, format!("spotify:episode:{}", id_of(&GID)));
        assert_eq!(episode.name, "Episode 1");
        assert_eq!(episode.duration_ms, 1_800_000);
        assert_eq!(episode.description, "The first one.");
        assert!(episode.explicit);
        assert_eq!(
            episode.release_date.as_deref(),
            Some("2024-05-06T00:00:00Z")
        );
        let show = episode.show.as_ref().unwrap();
        assert_eq!(show.name, "The Show");
        assert_eq!(
            show.uri,
            format!("spotify:show:{}", id_of(&[0x04; 16])),
            "the row's link to the show"
        );
        assert_eq!(episode.images[0].width, Some(640));
    }

    /// A list's sized pictures are preferred, with the widths the Web API
    /// would state, and references that name no file are left out.
    #[test]
    fn a_playlist_cover_prefers_the_sized_pictures() {
        let mut attributes = librespot_protocol::playlist4_external::ListAttributes::new();
        attributes.set_picture(vec![0xab, 0xcd]);
        for (target, url) in [
            ("default", "https://mosaic.scdn.co/300/x"),
            ("large", "spotify:image:abcd"),
            ("odd", "spotify:mosaic:zz"),
        ] {
            let mut picture = librespot_protocol::playlist4_external::PictureSize::new();
            picture.set_target_name(target.into());
            picture.set_url(url.into());
            attributes.picture_size.push(picture);
        }
        let attributes =
            librespot_metadata::playlist::attribute::PlaylistAttributes::try_from(&attributes)
                .unwrap();
        let images = playlist_images(&attributes);
        assert_eq!(images.len(), 2);
        assert_eq!(
            (images[0].url.as_str(), images[0].width),
            ("https://mosaic.scdn.co/300/x", Some(300))
        );
        assert_eq!(
            (images[1].url.as_str(), images[1].width),
            ("https://i.scdn.co/image/abcd", Some(640))
        );
    }

    /// Pages chain while rows remain and stop where the list does, as the
    /// app's paged list reads them.
    #[test]
    fn a_page_ends_where_the_list_does() {
        let full = |count: usize| vec![PlaylistItem::default(); count];
        let next = |items, total, offset| page(items, total, offset, 50).next_offset();
        assert_eq!(next(full(50), 170, 100), Some(150));
        assert_eq!(next(full(20), 170, 150), None, "the last, short page");
        assert_eq!(next(Vec::new(), 0, 0), None, "an empty list");
        assert_eq!(next(Vec::new(), 170, 500), None, "past the end");
    }

    /// A window cut short while rows remain is refused, so the Web API
    /// pages it rather than the next page starting past rows never shown.
    #[test]
    fn a_short_window_is_not_paged_past() {
        let short = complete(30, 0, 50, 500).unwrap_err();
        assert!(matches!(short, Failure::Retry(_)), "{short:?}");
        assert!(format!("{short:?}").contains("30 rows of 500"));
        assert!(complete(0, 0, 50, 500).is_err(), "nothing at all");
        assert!(complete(50, 100, 50, 170).is_ok());
        assert!(complete(20, 150, 50, 170).is_ok(), "the last, short page");
        assert!(complete(0, 0, 50, 0).is_ok(), "an empty list");
        assert!(complete(0, 500, 50, 170).is_ok(), "past the end");
    }

    #[test]
    fn a_missing_playlist_is_final_and_a_dropped_line_is_not() {
        let missing = refused(librespot_core::Error::not_found("gone"));
        assert!(matches!(
            missing,
            Failure::Definitive(ApiError::Status { status: 404, .. })
        ));
        let private = refused(librespot_core::Error::permission_denied("private"));
        assert!(matches!(
            private,
            Failure::Definitive(ApiError::Status { status: 403, .. })
        ));
        let dropped = refused(librespot_core::Error::unavailable("offline"));
        assert!(matches!(dropped, Failure::Retry(_)));
        // Any other call's refusal is its own, not the playlist's.
        let elsewhere = Failure::from(librespot_core::Error::not_found("no such profile"));
        assert!(matches!(elsewhere, Failure::Retry(_)));
    }

    /// The Web API writes a snapshot id as the revision in standard base64
    /// without padding: 24 bytes, 32 characters, with `+` and `/`. Cache
    /// files written from either source carry the same string for an
    /// unchanged playlist, so the id here has to match that exactly.
    #[test]
    fn the_snapshot_is_written_as_the_web_api_writes_it() {
        // As the Web API wrote it for one playlist, and the session later
        // wrote it again for the same, unchanged playlist.
        let written = "AAAABn+T46sp6JrgGzA8MWmCMCN72SB2";
        let revision = base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(written)
            .expect("standard alphabet, no padding");
        assert_eq!(revision.len(), 24, "a sequence number and a hash");
        assert_eq!(snapshot(&revision), written);
    }

    /// The rows are read from where the answer starts, and the total from
    /// the whole list, whatever window Spotify sent.
    #[test]
    fn rows_come_from_the_window_and_the_total_from_the_list() {
        let mut message = SelectedListContent::new();
        message.set_length(500);
        let contents = message.contents.mut_or_insert_default();
        contents.set_pos(100);
        for track in ["4uLU6hMCjMI75M1A2tKUQC", "7GhIk7Il098yCjg4BQjzvb"] {
            let mut item = Item::new();
            item.set_uri(format!("spotify:track:{track}"));
            contents.items.push(item);
        }
        let list = SessionPlaylist::parse(&message, &playlist_uri()).unwrap();
        assert_eq!(rows(&list, 100, 50).len(), 2);
        assert_eq!(rows(&list, 101, 50).len(), 1, "one row into the window");
        assert_eq!(total(&list), 500);
    }

    /// One request asks for each track and episode once, and for nothing
    /// Spotify has no details for.
    #[test]
    fn a_batch_asks_once_for_each_song_and_episode() {
        let uris = [
            SpotifyUri::from_uri(TRACK).unwrap(),
            SpotifyUri::from_uri(EPISODE).unwrap(),
            SpotifyUri::from_uri(TRACK).unwrap(),
            SpotifyUri::from_uri("spotify:local:Artist:Album:Song:180").unwrap(),
            SpotifyUri::from_uri("spotify:album:4uLU6hMCjMI75M1A2tKUQC").unwrap(),
        ];
        let (request, uris_asked) = batch(uris.iter());
        assert_eq!(uris_asked.len(), request.entity_request.len());
        let asked: Vec<(&str, ExtensionKind)> = request
            .entity_request
            .iter()
            .map(|entity| {
                (
                    entity.entity_uri.as_str(),
                    entity.query[0].extension_kind.enum_value().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            asked,
            [
                (TRACK, ExtensionKind::TRACK_V4),
                (EPISODE, ExtensionKind::EPISODE_V4)
            ]
        );
        assert!(batch([].iter()).1.is_empty());
    }

    /// One answer in Spotify's batch: the kind it is for, the URI, the code
    /// Spotify marks it with, and the bytes it carries, if any.
    fn answer(
        kind: ExtensionKind,
        uri: &str,
        status: i32,
        bytes: Option<Vec<u8>>,
    ) -> librespot_protocol::extended_metadata::EntityExtensionDataArray {
        use librespot_protocol::entity_extension_data::EntityExtensionData;
        use librespot_protocol::extended_metadata::EntityExtensionDataArray;
        let mut array = EntityExtensionDataArray::new();
        array.extension_kind = EnumOrUnknown::new(kind);
        let mut data = EntityExtensionData::new();
        data.entity_uri = uri.into();
        data.header.mut_or_insert_default().status_code = status;
        if let Some(bytes) = bytes {
            data.extension_data.mut_or_insert_default().value = bytes;
        }
        array.extension_data.push(data);
        array
    }

    fn track_bytes() -> Vec<u8> {
        let mut track = librespot_protocol::metadata::Track::new();
        track.set_gid(GID.to_vec());
        track.set_name("Never Gonna Give You Up".into());
        track.album.mut_or_insert_default().set_gid(vec![0x03; 16]);
        track.write_to_bytes().unwrap()
    }

    fn episode_bytes() -> Vec<u8> {
        let mut episode = librespot_protocol::metadata::Episode::new();
        episode.set_gid(vec![0x05; 16]);
        episode.set_name("Episode 1".into());
        episode.write_to_bytes().unwrap()
    }

    fn show_bytes(audiobook: bool) -> Vec<u8> {
        let mut show = librespot_protocol::metadata::Show::new();
        show.set_gid(vec![0x07; 16]);
        show.set_name("I, Robot".into());
        show.set_is_audiobook(audiobook);
        show.write_to_bytes().unwrap()
    }

    /// Only shows Spotify marks as audiobooks are reported. A show it does
    /// not answer for, or answers with another kind, stays a podcast.
    #[test]
    fn only_shows_marked_as_audiobooks_are_reported() {
        let book = "spotify:show:book";
        let podcast = "spotify:show:podcast";
        let answers = response([
            answer(ExtensionKind::SHOW_V4, book, 200, Some(show_bytes(true))),
            answer(ExtensionKind::SHOW_V4, podcast, 0, Some(show_bytes(false))),
            answer(ExtensionKind::SHOW_V4, "spotify:show:gone", 404, None),
            answer(
                ExtensionKind::TRACK_V4,
                "spotify:show:odd",
                200,
                Some(show_bytes(true)),
            ),
        ]);
        assert_eq!(audiobooks_in(&answers), vec![book.to_string()]);

        let request = show_request(&[book.into(), "spotify:episode:chapter".into()]);
        assert_eq!(
            request.entity_request.len(),
            1,
            "only shows are asked about"
        );
        assert_eq!(request.entity_request[0].entity_uri, book);
    }

    fn asked(uris: &[&str]) -> BTreeSet<String> {
        let uris: Vec<_> = uris
            .iter()
            .map(|uri| SpotifyUri::from_uri(uri).unwrap())
            .collect();
        batch(uris.iter()).1
    }

    fn response(
        arrays: impl IntoIterator<
            Item = librespot_protocol::extended_metadata::EntityExtensionDataArray,
        >,
    ) -> BatchedExtensionResponse {
        let mut response = BatchedExtensionResponse::new();
        response.extended_metadata.extend(arrays);
        response
    }

    /// Spotify's answer is read by URI and kind, whether it marks the batch
    /// and each answer with 200 or leaves them unmarked; a kind this does
    /// not read is passed over.
    #[test]
    fn an_answer_is_read_by_uri_and_kind() {
        let mut marked = answer(ExtensionKind::TRACK_V4, TRACK, 200, Some(track_bytes()));
        marked.header.mut_or_insert_default().provider_error_status = 200;
        let playables = answers(
            &asked(&[TRACK, EPISODE]),
            response([
                marked,
                answer(
                    ExtensionKind::ALBUM_V4,
                    "spotify:album:x",
                    200,
                    Some(track_bytes()),
                ),
                answer(ExtensionKind::EPISODE_V4, EPISODE, 0, Some(episode_bytes())),
            ]),
        )
        .unwrap();
        assert_eq!(
            playables.len(),
            2,
            "{:?}",
            playables.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            playables.get(TRACK).map(PlayableItem::uri),
            Some(format!("spotify:track:{}", id_of(&GID)).as_str())
        );
        assert!(matches!(
            playables.get(EPISODE),
            Some(PlayableItem::Episode(_))
        ));
    }

    /// A song Spotify no longer has is a row without one, as the Web API
    /// shows it, and the rest of the page keeps its songs.
    #[test]
    fn a_song_spotify_no_longer_has_leaves_its_row_empty() {
        let gone = "spotify:track:0000000000000000000001";
        let playables = answers(
            &asked(&[TRACK, gone]),
            response([
                answer(ExtensionKind::TRACK_V4, TRACK, 200, Some(track_bytes())),
                answer(ExtensionKind::TRACK_V4, gone, 404, None),
            ]),
        )
        .unwrap();
        assert!(playables.contains_key(TRACK));
        assert!(!playables.contains_key(gone));
        let rows = [row(TRACK, "", 0), row(gone, "", 0)];
        let items: Vec<_> = rows.iter().map(|row| item(row, &playables)).collect();
        assert!(items[0].item.is_some());
        assert!(items[1].item.is_none(), "an empty row, not a dropped one");
    }

    /// A song Spotify withholds for legal reasons, an episode blocked in
    /// the account's country, is likewise a row without one: the Web API
    /// answers `null` for it, and a page that fell back there on its
    /// account would land on the shared app's quota at every open.
    #[test]
    fn a_song_spotify_withholds_leaves_its_row_empty() {
        let playables = answers(
            &asked(&[TRACK, EPISODE]),
            response([
                answer(ExtensionKind::TRACK_V4, TRACK, 200, Some(track_bytes())),
                answer(ExtensionKind::EPISODE_V4, EPISODE, 451, None),
            ]),
        )
        .unwrap();
        assert!(playables.contains_key(TRACK));
        assert!(!playables.contains_key(EPISODE));
        let rows = [row(TRACK, "", 0), row(EPISODE, "", 0)];
        let items: Vec<_> = rows.iter().map(|row| item(row, &playables)).collect();
        assert!(items[0].item.is_some());
        assert!(items[1].item.is_none(), "an empty row, not a retried page");
    }

    /// One bad answer fails the page, so it is asked for again rather than
    /// cached without the song: Spotify refusing the song for any reason
    /// other than not having it, its provider failing the whole batch,
    /// bytes that do not read as a song, and a song it never answered for.
    #[test]
    fn a_partly_failed_batch_is_retried_not_cached_with_empty_rows() {
        let good = || answer(ExtensionKind::TRACK_V4, TRACK, 200, Some(track_bytes()));
        let retried = |arrays: Vec<_>| {
            let failure = answers(&asked(&[TRACK, EPISODE]), response(arrays)).unwrap_err();
            assert!(matches!(failure, Failure::Retry(_)), "{failure:?}");
            failure
        };

        let refused = retried(vec![
            good(),
            answer(ExtensionKind::EPISODE_V4, EPISODE, 500, None),
        ]);
        assert!(format!("{refused:?}").contains("500"));

        let mut failed = answer(
            ExtensionKind::EPISODE_V4,
            EPISODE,
            200,
            Some(episode_bytes()),
        );
        failed.header.mut_or_insert_default().provider_error_status = 503;
        retried(vec![good(), failed]);

        retried(vec![
            good(),
            answer(
                ExtensionKind::EPISODE_V4,
                EPISODE,
                200,
                Some(b"not an episode".to_vec()),
            ),
        ]);
        retried(vec![
            good(),
            answer(ExtensionKind::EPISODE_V4, EPISODE, 200, None),
        ]);

        let unanswered = retried(vec![good()]);
        assert!(format!("{unanswered:?}").contains(EPISODE));
    }

    /// Bytes that are not the kind asked for read as nothing.
    #[test]
    fn bytes_of_the_wrong_kind_read_as_nothing() {
        assert!(playable(ExtensionKind::TRACK_V4, b"not a track").is_none());
        assert!(playable(ExtensionKind::ALBUM_V4, &track_bytes()).is_none());
    }

    #[test]
    fn a_station_lists_each_song_once_in_spotify_order() {
        use librespot_protocol::context::Context;
        use librespot_protocol::context_page::ContextPage;
        use librespot_protocol::context_track::ContextTrack;
        let by_uri = |uri: &str| ContextTrack {
            uri: Some(uri.into()),
            ..Default::default()
        };
        let context = Context {
            pages: vec![
                ContextPage {
                    tracks: vec![
                        by_uri("spotify:track:3JA9Jsuxr4xgHXEawAdCp4"),
                        ContextTrack {
                            gid: Some(vec![0; 16]),
                            ..Default::default()
                        },
                        by_uri("spotify:episode:3JA9Jsuxr4xgHXEawAdCp4"),
                        ContextTrack::default(),
                    ],
                    ..Default::default()
                },
                ContextPage {
                    tracks: vec![
                        by_uri("spotify:track:3JA9Jsuxr4xgHXEawAdCp4"),
                        by_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC"),
                    ],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let uris: Vec<String> = station_songs(&context)
            .iter()
            .map(|uri| uri.to_uri().unwrap())
            .collect();
        assert_eq!(
            uris,
            [
                "spotify:track:3JA9Jsuxr4xgHXEawAdCp4",
                "spotify:track:0000000000000000000000",
                "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
            ]
        );
    }
}
