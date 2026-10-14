//! Catalogue reads over the streaming session, shaped as the Web API answers
//! them. The session speaks the protocol Spotify's own clients use, which has
//! no per-app quota, so a playlist someone else owns opens without waiting on
//! the shared app's rate limit.

use std::collections::{BTreeSet, HashMap};

use base64::Engine as _;
use librespot_core::{FileId, Session, SpotifyUri, error::ErrorKind, spotify_id::SpotifyId};
use librespot_metadata::artist::{Artist as SessionArtist, CountryTopTracks};
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
use librespot_protocol::playlist4_external::{Item, ListItems, MetaItem, SelectedListContent};
use protobuf::{EnumOrUnknown, Message as _};

use crate::api::ApiError;
use crate::api::models::{
    Album, ArtistRef, Episode, Image, Owner, Page, PlayableItem, Playlist, PlaylistItem, Show,
    Track, TrackCount, UserRef,
};

const IMAGE_HOST: &str = "https://i.scdn.co/image/";

/// Popular tracks in the playback account's market, retaining Spotify's ranking.
pub async fn artist_top_tracks(session: &Session, id: &str) -> Result<Vec<Track>, Failure> {
    use librespot_metadata::Metadata as _;
    let uri = SpotifyUri::Artist {
        id: SpotifyId::from_base62(id)?,
    };
    let artist = SessionArtist::get(session, &uri).await?;
    let uris = artist_top_track_uris(&artist.top_tracks, &session.country()).ok_or_else(|| {
        Failure::Retry(anyhow::anyhow!(
            "artist metadata has no popular tracks for this market"
        ))
    })?;
    let found = metadata(session, uris.iter()).await?;
    resolved_artist_top_tracks(&uris, &found)
}

/// Keep a genuinely empty ranking, but retry another source if no ranked song resolved.
fn resolved_artist_top_tracks(
    uris: &[SpotifyUri],
    found: &HashMap<String, PlayableItem>,
) -> Result<Vec<Track>, Failure> {
    let tracks: Vec<Track> = uris
        .iter()
        .map(SpotifyUri::to_uri)
        .filter_map(|uri| match found.get(&uri) {
            Some(PlayableItem::Track(track)) => Some(track.clone()),
            _ => None,
        })
        .collect();
    if tracks.is_empty() && !uris.is_empty() {
        return Err(Failure::Retry(anyhow::anyhow!(
            "no popular track details were available from the session"
        )));
    }
    Ok(tracks)
}

/// Select the account's market or the worldwide ranking, retaining its first ten URIs.
fn artist_top_track_uris(top_tracks: &CountryTopTracks, country: &str) -> Option<Vec<SpotifyUri>> {
    top_tracks
        .iter()
        .find(|top| top.country == country)
        .or_else(|| top_tracks.iter().find(|top| top.country == "ZZ"))
        .map(|top| top.tracks.iter().take(10).cloned().collect())
}

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

/// A future album's own mapping and ordered preview rows, through the same
/// playback session. An unknown row remains a non-playable placeholder.
pub async fn prerelease(
    session: &Session,
    web: &crate::api::ApiClient,
    uri: &str,
) -> anyhow::Result<crate::prerelease::Prerelease> {
    anyhow::ensure!(
        crate::link::parse(uri).as_deref() == Some(uri) && uri.starts_with("spotify:prerelease:"),
        "invalid prerelease URI"
    );
    let mut request = BatchedEntityRequest::new();
    request.entity_request.push(EntityRequest {
        entity_uri: uri.into(),
        query: vec![ExtensionQuery {
            extension_kind: EnumOrUnknown::new(ExtensionKind::PRERELEASE),
            ..Default::default()
        }],
        ..Default::default()
    });
    let response = session.spclient().get_extended_metadata(request).await?;
    let mut page = None;
    for array in &response.extended_metadata {
        if array.extension_kind.enum_value() != Ok(ExtensionKind::PRERELEASE) {
            continue;
        }
        anyhow::ensure!(
            matches!(array.header.provider_error_status, 0 | 200),
            "Spotify could not resolve this prerelease"
        );
        for data in &array.extension_data {
            if data.entity_uri != uri {
                continue;
            }
            anyhow::ensure!(
                matches!(data.header.status_code, 0 | 200),
                "Spotify prerelease answered {}",
                data.header.status_code
            );
            let bytes = &data
                .extension_data
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("empty prerelease response"))?
                .value;
            page = Some(crate::prerelease::Prerelease::decode(bytes, uri)?);
        }
    }
    let mut page = page.ok_or_else(|| anyhow::anyhow!("Spotify did not return this prerelease"))?;
    let login = session.login5().auth_token().await?;
    let client_token = session.spclient().client_token().await?;
    let mut offset = 0;
    loop {
        let query = serde_json::json!({
            "operationName":"getAlbum",
            "variables":{"uri":page.album.uri,"locale":"","offset":offset,"limit":50},
            "extensions":{"persistedQuery":{"version":1,"sha256Hash":"b9bfabef66ed756e5e13f68a942deb60bd4125ec1f1be8cc42769dc0259b4b10"}}
        });
        let body = web
            .partner_request(&login.access_token, &client_token, &query)
            .await?;
        let (rows, total) = page.album_preview(&body)?;
        anyhow::ensure!(
            offset == 0 || page.album.total_tracks == Some(total),
            "Spotify changed this track list while loading"
        );
        page.album.total_tracks = Some(total);
        offset += rows.len();
        page.tracks.extend(rows);
        if offset >= total as usize {
            break;
        }
        anyhow::ensure!(
            offset > 0 && offset <= 1000,
            "Spotify returned an incomplete track list"
        );
    }
    match prerelease_saved(session, &page.album.uri).await {
        Ok(saved) => page.saved = Some(saved),
        Err(_) => {
            page.save_error =
                Some("Could not check pre-save state. Open the link again to retry.".into())
        }
    }
    Ok(page)
}

fn collection_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    let value =
        reqwest::header::HeaderValue::from_static("application/vnd.collection-v2.spotify.proto");
    headers.insert(reqwest::header::CONTENT_TYPE, value.clone());
    headers.insert(reqwest::header::ACCEPT, value);
    headers
}

/// Whether this account has already pre-saved the album resolved from the countdown page.
pub async fn prerelease_saved(session: &Session, uri: &str) -> anyhow::Result<bool> {
    let body = crate::prerelease::contains_request(&session.username(), uri)?;
    let bytes = session
        .spclient()
        .request(
            &reqwest::Method::POST,
            "/collection/v2/contains",
            Some(collection_headers()),
            Some(&body),
        )
        .await?;
    crate::prerelease::contains_response(&bytes)
}

/// Pre-save or remove only after a user action, then confirm the collection's
/// answer. Opening a page never calls this mutation.
pub async fn set_prerelease_saved(
    session: &Session,
    http: &reqwest::Client,
    uri: &str,
    saved: bool,
) -> anyhow::Result<()> {
    let body = crate::prerelease::write_request(
        &session.username(),
        uri,
        saved,
        jiff::Timestamp::now().as_second(),
    )?;
    let endpoint = format!(
        "{}/collection/v2/write",
        session.spclient().base_url().await?
    );
    let login = session.login5().auth_token().await?;
    let client_token = session.spclient().client_token().await?;
    // SpClient retries failed requests, including writes. Only this transport
    // disables retry and redirect policies; concurrent playback keeps its own.
    write_collection(http, &endpoint, &login.access_token, &client_token, body).await?;
    anyhow::ensure!(
        prerelease_saved(session, uri).await? == saved,
        "Spotify has not confirmed the pre-save change. Open the link again to check."
    );
    Ok(())
}

async fn write_collection(
    http: &reqwest::Client,
    endpoint: &str,
    bearer: &str,
    client_token: &str,
    body: Vec<u8>,
) -> anyhow::Result<()> {
    let response = http
        .post(endpoint)
        .bearer_auth(bearer)
        .header("client-token", client_token)
        .headers(collection_headers())
        .body(body)
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    anyhow::ensure!(
        response.status().is_success(),
        "Spotify did not accept pre-save (HTTP {}). Open the link again to check.",
        response.status().as_u16()
    );
    Ok(())
}

/// A playlist's header: name, owner, cover, and snapshot.
pub async fn playlist(session: &Session, id: &str) -> Result<Playlist, Failure> {
    let list = window(session, id, 0, 0).await?;
    let mut playlist = header(id, &list);
    // `Playlist::owner_name` reads a missing name as Spotify's for its own
    // lists and as the id for anyone else's, until the app finds the name
    // where the Web API gave it: the account's own, or the library list.
    if let Some(owner) = playlist.owner.id.clone().filter(|owner| owner != "spotify") {
        playlist.owner.display_name = user_profile(session, &owner)
            .await
            .and_then(|profile| profile.name);
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

/// The account's rootlist, page by page in Spotify's order: one request per
/// 500 rows, each row decorated with its playlist's name, cover, owner,
/// length, and what the account may do to it.
pub async fn rootlist(session: &Session) -> anyhow::Result<Vec<ListItems>> {
    let mut pages = Vec::new();
    let mut from = 0usize;
    loop {
        let bytes = session
            .spclient()
            .get_rootlist(from, Some(ROOTLIST_PAGE))
            .await
            .map_err(|error| anyhow::anyhow!("rootlist: {error}"))?;
        let Some((contents, more)) =
            rootlist_answer_page(SelectedListContent::parse_from_bytes(&bytes)?, from)?
        else {
            break;
        };
        let count = contents.items.len();
        pages.push(contents);
        if !more {
            break;
        }
        from += count;
    }
    Ok(pages)
}

const ROOTLIST_PAGE: usize = 500;

/// One rootlist answer read at `from`: its page, and whether rows follow
/// it; `None` when the list ends before `from`. An answer that leaves rows
/// out while saying some remain, either with no page where the list's
/// length says rows are or with an empty page marked as cut short, is an
/// error, so the read is retried rather than taken as the whole list.
fn rootlist_answer_page(
    response: SelectedListContent,
    from: usize,
) -> anyhow::Result<Option<(ListItems, bool)>> {
    let length = response
        .has_length()
        .then(|| usize::try_from(response.length()).unwrap_or_default());
    let Some(contents) = response.contents.into_option() else {
        if let Some(length) = length.filter(|length| *length > from) {
            anyhow::bail!("rootlist answered no rows at {from} of {length}");
        }
        return Ok(None);
    };
    let truncated = contents.truncated();
    if truncated && contents.items.is_empty() {
        anyhow::bail!("rootlist answered an empty page at {from} marked as cut short");
    }
    Ok(Some((contents, truncated)))
}

/// The account's playlist library, as the Web API's list of the account's
/// playlists gives it: every playlist it saved or follows, Spotify's own
/// among them, in the order Spotify keeps them, read from the rootlist
/// without the shared app's quota (#698, #816). The rootlist names each
/// owner by id only, so their display names come from their profiles, as
/// for a playlist header read here; a name that does not arrive in time
/// leaves the id showing, as the header read does.
pub async fn library(session: &Session) -> Result<Vec<Playlist>, Failure> {
    let pages = rootlist(session).await.map_err(Failure::Retry)?;
    let mut playlists = library_rows(&pages)?;
    let names = owner_names(session, &playlists).await;
    for playlist in &mut playlists {
        if let Some(name) = playlist.owner.id.as_ref().and_then(|id| names.get(id)) {
            playlist.owner.display_name = Some(name.clone());
        }
    }
    Ok(playlists)
}

/// How long the library waits for its owners' names before showing ids.
const OWNER_NAMES_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Profile reads in flight at once, and owners asked about at most.
const OWNER_NAMES_AT_ONCE: usize = 4;
const OWNER_NAMES_MAX: usize = 100;

/// The display names of the owners of `playlists`, Spotify's own aside,
/// read from their profiles a few at a time within [`OWNER_NAMES_WAIT`].
async fn owner_names(session: &Session, playlists: &[Playlist]) -> HashMap<String, String> {
    let owners: BTreeSet<String> = playlists
        .iter()
        .filter_map(|playlist| playlist.owner.id.clone())
        .filter(|owner| owner != "spotify")
        .take(OWNER_NAMES_MAX)
        .collect();
    let mut names = HashMap::new();
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(OWNER_NAMES_AT_ONCE));
    let mut reads = tokio::task::JoinSet::new();
    for owner in owners {
        let session = session.clone();
        let slots = slots.clone();
        reads.spawn(async move {
            let _slot = slots.acquire_owned().await.ok()?;
            let name = user_profile(&session, &owner).await?.name?;
            Some((owner, name))
        });
    }
    let _ = tokio::time::timeout(OWNER_NAMES_WAIT, async {
        while let Some(read) = reads.join_next().await {
            if let Ok(Some((owner, name))) = read {
                names.insert(owner, name);
            }
        }
    })
    .await;
    names
}

/// The playlists among the rootlist's rows. A page whose rows came without
/// the details Spotify decorates them with would list nameless playlists,
/// so it is a retry, for the Web API to answer instead.
fn library_rows(pages: &[ListItems]) -> Result<Vec<Playlist>, Failure> {
    let mut playlists = Vec::new();
    for page in pages {
        if page.meta_items.len() != page.items.len() {
            return Err(Failure::Retry(anyhow::anyhow!(
                "rootlist page answered {} rows with {} details",
                page.items.len(),
                page.meta_items.len()
            )));
        }
        for (item, meta) in page.items.iter().zip(&page.meta_items) {
            if let Some(playlist) = library_row(item, meta) {
                playlists.push(playlist);
            }
        }
    }
    Ok(playlists)
}

/// One playlist row of the rootlist; `None` for a folder marker, and for a
/// playlist Spotify says is gone or hidden from this account, which the
/// Web API leaves out of the list too.
fn library_row(item: &Item, meta: &MetaItem) -> Option<Playlist> {
    let id = item.uri().strip_prefix("spotify:playlist:")?;
    if !matches!(meta.status_code(), 0 | 200) {
        return None;
    }
    let attributes = PlaylistAttributes::try_from(&*meta.attributes).ok()?;
    let owner = non_empty(meta.owner_username());
    Some(Playlist {
        id: id.to_string(),
        uri: item.uri().to_string(),
        description: non_empty(&attributes.description),
        images: playlist_images(&attributes),
        owner: Owner {
            uri: owner.as_ref().map(|owner| format!("spotify:user:{owner}")),
            id: owner,
            display_name: None,
        },
        public: item
            .attributes
            .as_ref()
            .filter(|attributes| attributes.has_public())
            .map(|attributes| attributes.public()),
        collaborative: attributes.is_collaborative,
        snapshot_id: meta.has_revision().then(|| snapshot(meta.revision())),
        items_count: meta.has_length().then(|| TrackCount {
            total: u32::try_from(meta.length()).unwrap_or_default(),
        }),
        name: attributes.name,
        ..Default::default()
    })
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
        .map(SpotifyUri::to_uri)
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
        .filter(|uri| seen.insert(uri.to_uri()))
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

/// What the profile view says about a user: their display name and avatar,
/// each when Spotify gives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserProfile {
    pub name: Option<String>,
    pub image_url: Option<String>,
}

/// The profile behind a user id, from the view Spotify's own clients read;
/// `None` when nothing answers.
pub async fn user_profile(session: &Session, user_id: &str) -> Option<UserProfile> {
    let bytes = session
        .spclient()
        .get_user_profile(user_id, Some(0), Some(0))
        .await
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some(profile_from_json(&json))
}

/// Reads the name and avatar out of a profile view answer.
fn profile_from_json(json: &serde_json::Value) -> UserProfile {
    let text = |key: &str| {
        json.get(key)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    UserProfile {
        name: text("name"),
        image_url: text("image_url"),
    }
}

/// What one friend is listening to, or last played, with names to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendActivity {
    pub user_uri: String,
    pub user_name: String,
    pub image_url: Option<String>,
    /// Milliseconds since the epoch.
    pub timestamp_ms: i64,
    /// Whether the friend is playing the song right now. Only the live feed
    /// says so; the older buddy list never does.
    pub playing: bool,
    pub track_uri: String,
    pub track_name: String,
    pub artist_uri: Option<String>,
    pub artist_name: String,
    pub album_uri: Option<String>,
    pub cover_url: Option<String>,
    pub context_uri: Option<String>,
    pub context_name: Option<String>,
}

/// The feed behind Spotify's Listening Activity panel. It lists the same
/// people the desktop app shows, with the song playing now, where the older
/// buddy list reports a song only once it has finished.
const LISTENING_ACTIVITY: &str = "https://spclient.wg.spotify.com/listening-activity/v1/feed";

/// Names that rarely change, kept between polls so each refresh asks only
/// for the songs.
#[derive(Default)]
pub struct FriendNames {
    profiles: HashMap<String, UserProfile>,
    playlists: HashMap<String, Option<String>>,
}

/// One friend's latest entry in the live feed, before the song and the
/// person have names.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveActivity {
    user_uri: String,
    track_uri: String,
    context_uri: Option<String>,
    playing: bool,
    timestamp_ms: i64,
}

/// What friends are listening to, newest first, as Spotify's own Listening
/// Activity shows it, read with the session's own token. There is no Web
/// API for it. When the live feed does not answer, the older buddy list
/// stands in.
pub async fn friend_activity(
    session: &Session,
    http: &reqwest::Client,
    names: &tokio::sync::Mutex<FriendNames>,
) -> anyhow::Result<Vec<FriendActivity>> {
    let live = match live_activity(session, http).await {
        Ok(live) => live,
        Err(error) => {
            log::warn!("listening activity feed unavailable, using the buddy list: {error:#}");
            return buddy_list(session).await;
        }
    };
    let uris: Vec<SpotifyUri> = live
        .iter()
        .filter_map(|entry| SpotifyUri::from_uri(&entry.track_uri).ok())
        .collect();
    let tracks = metadata(session, uris.iter())
        .await
        .map_err(|failure| match failure {
            Failure::Definitive(error) => anyhow::anyhow!("{error:?}"),
            Failure::Retry(error) => error,
        })?;
    let mut names = names.lock().await;
    let mut friends = Vec::with_capacity(live.len());
    for entry in live {
        let Some(PlayableItem::Track(track)) = tracks.get(&entry.track_uri) else {
            continue;
        };
        let user_id = entry
            .user_uri
            .rsplit(':')
            .next()
            .unwrap_or_default()
            .to_string();
        if !names.profiles.contains_key(&user_id) {
            let profile = user_profile(session, &user_id).await.unwrap_or_default();
            names.profiles.insert(user_id.clone(), profile);
        }
        let profile = &names.profiles[&user_id];
        let user_name = profile.name.clone().unwrap_or_else(|| user_id.clone());
        let image_url = profile.image_url.clone();
        let album = track.album.as_ref().filter(|album| !album.uri.is_empty());
        let artist = track.artists.first();
        // Without a context Spotify names the song's album, as its own
        // panel does.
        let context_uri = entry
            .context_uri
            .or_else(|| album.map(|album| album.uri.clone()));
        let context_name = match context_uri.as_deref() {
            Some(uri) if uri.contains(":playlist:") => {
                let id = uri.rsplit(':').next().unwrap_or_default().to_string();
                if !names.playlists.contains_key(&id) {
                    let name = playlist(session, &id).await.ok().map(|list| list.name);
                    names.playlists.insert(id.clone(), name);
                }
                names.playlists[&id].clone()
            }
            // A song played on its own names itself as where it plays from.
            Some(uri) if uri == entry.track_uri => Some(track.name.clone()),
            Some(uri) => album
                .filter(|album| album.uri == uri)
                .map(|album| album.name.clone())
                .or_else(|| {
                    track
                        .artists
                        .iter()
                        .find(|artist| artist.uri.as_deref() == Some(uri))
                        .map(|artist| artist.name.clone())
                }),
            None => None,
        };
        friends.push(FriendActivity {
            user_uri: entry.user_uri,
            user_name,
            image_url,
            timestamp_ms: entry.timestamp_ms,
            playing: entry.playing,
            track_uri: entry.track_uri,
            track_name: track.name.clone(),
            artist_uri: artist
                .and_then(|artist| artist.uri.clone())
                .filter(|uri| !uri.is_empty()),
            artist_name: artist.map(|artist| artist.name.clone()).unwrap_or_default(),
            album_uri: album
                .map(|album| album.uri.clone())
                .filter(|uri| !uri.is_empty()),
            cover_url: album
                .and_then(|album| crate::api::models::pick_image(&album.images, 128))
                .map(str::to_string),
            context_uri,
            context_name,
        });
    }
    Ok(friends)
}

async fn live_activity(
    session: &Session,
    http: &reqwest::Client,
) -> anyhow::Result<Vec<LiveActivity>> {
    let login = session.login5().auth_token().await?;
    let client_token = session.spclient().client_token().await?;
    let response = http
        .post(LISTENING_ACTIVITY)
        .bearer_auth(&login.access_token)
        .header("client-token", client_token)
        .json(&serde_json::json!({"unused": true, "resultLimit": 100}))
        .send()
        .await?
        .error_for_status()?;
    let json: serde_json::Value = response.json().await?;
    live_from_json(&json)
}

/// Reads the live feed: each friend's latest song, newest first. Entries
/// that are not a person playing a song are passed over; an answer without
/// its list fails, rather than pass for nobody listening.
fn live_from_json(json: &serde_json::Value) -> anyhow::Result<Vec<LiveActivity>> {
    let entities = json
        .get("entities")
        .and_then(|entities| entities.as_array())
        .ok_or_else(|| anyhow::anyhow!("listening activity answered without entities"))?;
    let mut latest: HashMap<String, LiveActivity> = HashMap::new();
    for item in entities {
        let Some(entity) = item.get("userEntity").or_else(|| item.get("followEntity")) else {
            continue;
        };
        let Some(user_uri) = entity
            .get("uri")
            .and_then(|uri| uri.as_str())
            .filter(|uri| uri.starts_with("spotify:user:"))
        else {
            continue;
        };
        let Some(activity) = entity.get("activity") else {
            continue;
        };
        let Some(track_uri) = activity
            .get("entityUri")
            .and_then(|uri| uri.as_str())
            .filter(|uri| uri.starts_with("spotify:track:"))
        else {
            continue;
        };
        let Some(timestamp_ms) = activity
            .get("timestamp")
            .and_then(|time| time.as_str())
            .and_then(|time| time.parse::<jiff::Timestamp>().ok())
            .map(|time| time.as_millisecond())
        else {
            continue;
        };
        let context_uri = activity
            .get("contextUri")
            .and_then(|uri| uri.as_str())
            .map(live_context)
            .filter(|uri| !uri.is_empty());
        let entry = LiveActivity {
            user_uri: user_uri.to_string(),
            track_uri: track_uri.to_string(),
            context_uri,
            playing: activity
                .get("isPlaying")
                .and_then(|playing| playing.as_bool())
                .unwrap_or(false),
            timestamp_ms,
        };
        if latest
            .get(user_uri)
            .is_none_or(|known| known.timestamp_ms < timestamp_ms)
        {
            latest.insert(user_uri.to_string(), entry);
        }
    }
    let mut live: Vec<LiveActivity> = latest.into_values().collect();
    live.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp_ms));
    Ok(live)
}

/// The live feed names play from an artist's Popular songs as a list of its
/// own; that is the artist.
fn live_context(uri: &str) -> String {
    uri.strip_prefix("spotify:list:popular-release-segments-main-roles:artist_")
        .map(|id| format!("spotify:artist:{id}"))
        .unwrap_or_else(|| uri.to_string())
}

/// The older buddy list Spotify's desktop app read before the live feed.
async fn buddy_list(session: &Session) -> anyhow::Result<Vec<FriendActivity>> {
    let bytes = session
        .spclient()
        .request(
            &reqwest::Method::GET,
            "/presence-view/v1/buddylist",
            None,
            None,
        )
        .await?;
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(friends_from_json(&json))
}

/// Reads the buddy list answer, most recent first, skipping friends whose
/// entry lacks a user or a track.
fn friends_from_json(json: &serde_json::Value) -> Vec<FriendActivity> {
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let mut friends: Vec<FriendActivity> = json
        .get("friends")
        .and_then(|friends| friends.as_array())
        .into_iter()
        .flatten()
        .filter_map(|friend| {
            let user = friend.get("user")?;
            let track = friend.get("track")?;
            let context = track.get("context");
            let artist = track.get("artist");
            Some(FriendActivity {
                user_uri: text(user, "uri")?,
                user_name: text(user, "name").or_else(|| text(user, "uri"))?,
                image_url: text(user, "imageUrl"),
                timestamp_ms: friend.get("timestamp")?.as_i64()?,
                playing: false,
                track_uri: text(track, "uri")?,
                track_name: text(track, "name")?,
                artist_uri: artist.and_then(|artist| text(artist, "uri")),
                artist_name: artist
                    .and_then(|artist| text(artist, "name"))
                    .unwrap_or_default(),
                album_uri: track.get("album").and_then(|album| text(album, "uri")),
                cover_url: text(track, "imageUrl"),
                context_uri: context.and_then(|context| text(context, "uri")),
                context_name: context.and_then(|context| text(context, "name")),
            })
        })
        .collect();
    friends.sort_by_key(|friend| std::cmp::Reverse(friend.timestamp_ms));
    friends
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
    let uri = row.id.to_uri();
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
        name: track_title.clone(),
        duration_ms: u32::try_from(duration.as_millis()).unwrap_or(u32::MAX),
        artists: non_empty(artist)
            .map(|name| ArtistRef {
                name,
                ..Default::default()
            })
            .into_iter()
            .collect(),
        album: non_empty(album_title).map(|name| Album {
            name,
            ..Default::default()
        }),
        is_local: true,
        ..Default::default()
    }))
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
        let text = uri.to_uri();
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
                .map(|id| id.to_base62());
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
        id: Some(track.id.to_id()),
        uri: track.id.to_uri(),
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
        id: album.id.to_id(),
        uri: album.id.to_uri(),
        images: images(&album.covers),
        artists: album.artists.iter().map(artist_ref).collect(),
        label: non_empty(&album.label),
        name: album.name,
        ..Default::default()
    }
}

fn episode(episode: SessionEpisode, show_id: Option<String>) -> Episode {
    Episode {
        id: episode.id.to_id(),
        uri: episode.id.to_uri(),
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
        id: Some(artist.id.to_id()),
        name: artist.name.clone(),
        uri: Some(artist.id.to_uri()),
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

    /// A lost reply is uncertain, including when the server accepted the write.
    /// HTTP errors and redirect responses must never resend the mutation either.
    #[tokio::test]
    async fn presave_writes_are_not_replayed_after_failure_or_redirect() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        for status in [None, Some(429), Some(503), Some(307)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let endpoint = format!("http://{address}/collection/v2/write");
            let location = endpoint.clone();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                while !bytes.ends_with(b"mutation") {
                    let received = socket.read(&mut buffer).await.unwrap();
                    assert!(received > 0);
                    bytes.extend_from_slice(&buffer[..received]);
                }
                assert!(bytes.starts_with(b"POST /collection/v2/write "));
                if let Some(status) = status {
                    let response = format!(
                        "HTTP/1.1 {status} Test\r\nRetry-After: 0\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                }
                drop(socket);
                assert!(
                    tokio::time::timeout(Duration::from_millis(250), listener.accept())
                        .await
                        .is_err(),
                    "one button press must send exactly one write"
                );
            });
            let http =
                crate::http::build_collection_client(&crate::settings::ProxyConfig::Off).unwrap();
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                write_collection(&http, &endpoint, "dummy", "dummy", b"mutation".to_vec()),
            )
            .await
            .expect("write must return without retrying");
            assert!(result.is_err());
            server.await.unwrap();
        }
    }
    use librespot_metadata::Metadata as _;

    use super::*;

    const TRACK: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";
    const EPISODE: &str = "spotify:episode:7GhIk7Il098yCjg4BQjzvb";

    /// Another market's ranking must not replace the account's own or worldwide list.
    #[test]
    fn popular_tracks_prefer_the_account_market_and_preserve_its_first_ten() {
        use librespot_metadata::artist::TopTracks;
        use librespot_metadata::track::Tracks;
        let ranked: Vec<SpotifyUri> = (1..=12)
            .map(|id| SpotifyUri::Track {
                id: SpotifyId::from_base62(&format!("{id:022}")).unwrap(),
            })
            .collect();
        let world = SpotifyUri::from_uri(TRACK).unwrap();
        let lists = CountryTopTracks(vec![
            TopTracks {
                country: "ZZ".into(),
                tracks: Tracks(vec![world.clone()]),
            },
            TopTracks {
                country: "US".into(),
                tracks: Tracks(ranked.clone()),
            },
        ]);
        assert_eq!(
            artist_top_track_uris(&lists, "US"),
            Some(ranked[..10].to_vec())
        );
        assert_eq!(artist_top_track_uris(&lists, "HU"), Some(vec![world]));
        let only_other_market = CountryTopTracks(vec![TopTracks {
            country: "DE".into(),
            tracks: Tracks(ranked),
        }]);
        assert_eq!(
            artist_top_track_uris(&only_other_market, "US"),
            None,
            "do not substitute another market's ranking"
        );
        assert_eq!(
            artist_top_track_uris(&CountryTopTracks::default(), "US"),
            None
        );
        let empty_market = CountryTopTracks(vec![TopTracks {
            country: "US".into(),
            tracks: Tracks::default(),
        }]);
        assert_eq!(
            artist_top_track_uris(&empty_market, "US"),
            Some(Vec::new()),
            "a known empty ranking is successful"
        );
    }

    /// An artist with an explicitly empty ranking needs no Web API fallback.
    #[test]
    fn an_empty_popular_ranking_is_a_successful_empty_answer() {
        assert!(
            resolved_artist_top_tracks(&[], &HashMap::new())
                .unwrap()
                .is_empty()
        );
    }

    /// A nonempty ranking whose metadata is entirely 404/451 must try the Web API.
    #[test]
    fn popular_tracks_retry_when_every_ranked_track_is_unavailable() {
        let gone = "spotify:track:0000000000000000000001";
        let uris: Vec<_> = [TRACK, gone]
            .into_iter()
            .map(|uri| SpotifyUri::from_uri(uri).unwrap())
            .collect();
        let found = answers(
            &asked(&[TRACK, gone]),
            response([
                answer(ExtensionKind::TRACK_V4, TRACK, 404, None),
                answer(ExtensionKind::TRACK_V4, gone, 451, None),
            ]),
        )
        .unwrap();
        assert!(matches!(
            resolved_artist_top_tracks(&uris, &found),
            Err(Failure::Retry(_))
        ));
    }

    /// A resolved episode cannot stand in for the only ranked artist track.
    #[test]
    fn popular_tracks_retry_when_metadata_contains_only_non_tracks() {
        let uris = [SpotifyUri::from_uri(TRACK).unwrap()];
        let found = HashMap::from([(TRACK.to_string(), PlayableItem::Episode(Episode::default()))]);
        assert!(matches!(
            resolved_artist_top_tracks(&uris, &found),
            Err(Failure::Retry(_))
        ));
    }

    /// Partial metadata preserves the remaining tracks in ranking order without fallback.
    #[test]
    fn partial_popular_metadata_keeps_the_available_ranking_order() {
        let other = "spotify:track:0000000000000000000001";
        let gone = "spotify:track:0000000000000000000002";
        let uris: Vec<_> = [other, gone, TRACK]
            .into_iter()
            .map(|uri| SpotifyUri::from_uri(uri).unwrap())
            .collect();
        let mut found = playables();
        found.insert(
            other.to_string(),
            PlayableItem::Track(Track {
                uri: other.into(),
                ..Default::default()
            }),
        );
        let tracks = resolved_artist_top_tracks(&uris, &found).unwrap();
        assert_eq!(
            tracks
                .iter()
                .map(|track| track.uri.as_str())
                .collect::<Vec<_>>(),
            [other, TRACK]
        );
    }

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

    /// The profile view gives a user's name and avatar; blanks and nulls
    /// read as absent.
    #[test]
    fn a_profile_read_carries_the_name_and_avatar() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"uri":"spotify:user:alice","name":"Alice","image_url":"https://i.scdn.co/image/avatar"}"#,
        )
        .unwrap();
        assert_eq!(
            profile_from_json(&json),
            UserProfile {
                name: Some("Alice".into()),
                image_url: Some("https://i.scdn.co/image/avatar".into()),
            }
        );

        let json: serde_json::Value =
            serde_json::from_str(r#"{"name":"Bob","image_url":null}"#).unwrap();
        assert_eq!(
            profile_from_json(&json),
            UserProfile {
                name: Some("Bob".into()),
                image_url: None,
            }
        );

        let json: serde_json::Value = serde_json::from_str("{}").unwrap();
        assert_eq!(profile_from_json(&json), UserProfile::default());
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
        SpotifyId::from_raw(gid).unwrap().to_base62()
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
            .map(|id| id.to_base62());
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
            .map(SpotifyUri::to_uri)
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

    /// A rootlist row and the details Spotify decorates it with.
    fn listed(uri: &str, decorate: impl FnOnce(&mut MetaItem)) -> (Item, MetaItem) {
        let mut item = Item::new();
        item.set_uri(uri.into());
        let mut meta = MetaItem::new();
        decorate(&mut meta);
        (item, meta)
    }

    fn rootlist_page(rows: Vec<(Item, MetaItem)>) -> ListItems {
        let mut page = ListItems::new();
        for (item, meta) in rows {
            page.items.push(item);
            page.meta_items.push(meta);
        }
        page
    }

    fn rootlist_answer(length: Option<i32>, contents: Option<ListItems>) -> SelectedListContent {
        let mut response = SelectedListContent::new();
        if let Some(length) = length {
            response.set_length(length);
        }
        if let Some(contents) = contents {
            response.contents = protobuf::MessageField::some(contents);
        }
        response
    }

    fn rootlist_contents(rows: usize, truncated: bool) -> ListItems {
        let mut contents = ListItems::new();
        contents.set_truncated(truncated);
        contents.items = (0..rows).map(|_| Item::new()).collect();
        contents
    }

    /// An answer that leaves rows out while saying some remain is retried,
    /// not taken as the whole list; one with no rows left ends the read,
    /// and so does a present empty page, the empty library's answer.
    #[test]
    fn a_rootlist_answer_missing_rows_it_owes_is_an_error() {
        let page =
            |length, contents, from| rootlist_answer_page(rootlist_answer(length, contents), from);

        assert!(
            page(Some(700), None, 500).is_err(),
            "no page with rows left"
        );
        assert!(page(Some(500), None, 500).unwrap().is_none(), "none left");
        assert!(page(None, None, 0).unwrap().is_none(), "no length at all");
        assert!(
            page(Some(700), Some(rootlist_contents(0, true)), 500).is_err(),
            "an empty page cut short"
        );

        let (empty, more) = page(Some(0), Some(rootlist_contents(0, false)), 0)
            .unwrap()
            .unwrap();
        assert!(empty.items.is_empty());
        assert!(!more);

        let (first, more) = page(Some(700), Some(rootlist_contents(500, true)), 0)
            .unwrap()
            .unwrap();
        assert_eq!(first.items.len(), 500);
        assert!(more);
        let (last, more) = page(Some(700), Some(rootlist_contents(200, false)), 500)
            .unwrap()
            .unwrap();
        assert_eq!(last.items.len(), 200);
        assert!(!more);
    }

    /// The rootlist's playlists become the library's rows in Spotify's
    /// order, Spotify's own among them, with the name, cover, owner, song
    /// count, snapshot, and public flag the Web API's list would carry.
    /// Folder markers, and playlists Spotify says are gone, are not rows.
    #[test]
    fn the_rootlist_becomes_the_playlist_library() {
        let mine = listed("spotify:playlist:mine", |meta| {
            meta.set_owner_username("me".into());
            meta.set_length(12);
            meta.set_revision(vec![1, 2, 3]);
            let attributes = meta.attributes.mut_or_insert_default();
            attributes.set_name("Road trip".into());
            attributes.set_description("Loud".into());
            let mut picture = librespot_protocol::playlist4_external::PictureSize::new();
            picture.set_target_name("default".into());
            picture.set_url("spotify:image:ab67".into());
            attributes.picture_size.push(picture);
        });
        let (mut item, meta) = listed("spotify:playlist:daily", |meta| {
            meta.set_owner_username("spotify".into());
            meta.set_status_code(200);
            meta.attributes
                .mut_or_insert_default()
                .set_name("Daily Mix 1".into());
        });
        item.attributes.mut_or_insert_default().set_public(false);
        let gone = listed("spotify:playlist:gone", |meta| {
            meta.set_status_code(404);
            meta.attributes
                .mut_or_insert_default()
                .set_name("Gone".into());
        });
        let pages = [
            rootlist_page(vec![
                listed("spotify:start-group:f1:Mixes", |_| {}),
                (item, meta),
                listed("spotify:end-group:f1", |_| {}),
            ]),
            rootlist_page(vec![gone, mine]),
        ];
        let Ok(rows) = library_rows(&pages) else {
            panic!("expected the library");
        };
        let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["daily", "mine"]);

        let daily = &rows[0];
        assert_eq!(daily.name, "Daily Mix 1");
        assert_eq!(daily.uri, "spotify:playlist:daily");
        assert_eq!(daily.owner.id.as_deref(), Some("spotify"));
        assert_eq!(daily.owner_name(), "Spotify");
        assert_eq!(daily.public, Some(false));
        assert_eq!(daily.items_count, None, "no length given");

        let mine = &rows[1];
        assert_eq!(mine.name, "Road trip");
        assert_eq!(mine.description.as_deref(), Some("Loud"));
        assert_eq!(mine.owner.uri.as_deref(), Some("spotify:user:me"));
        assert_eq!(mine.track_total(), 12);
        assert_eq!(mine.public, None, "unknown, not private");
        assert_eq!(mine.snapshot_id.as_deref(), Some("AQID"));
        assert_eq!(mine.images.len(), 1);
        assert_eq!(mine.images[0].url, "https://i.scdn.co/image/ab67");
        assert_eq!(mine.images[0].width, Some(300));
    }

    /// A page whose rows came without their details would list nameless
    /// playlists, so the Web API is asked instead.
    #[test]
    fn an_undecorated_rootlist_page_is_left_to_the_web_api() {
        let mut page = rootlist_page(vec![listed("spotify:playlist:a", |_| {})]);
        let mut bare = Item::new();
        bare.set_uri("spotify:playlist:b".into());
        page.items.push(bare);
        assert!(matches!(library_rows(&[page]), Err(Failure::Retry(_))));
        assert!(matches!(library_rows(&[]), Ok(rows) if rows.is_empty()));
    }

    /// The live feed keeps each friend's latest song, newest first, says
    /// who is playing now, and reads an artist's Popular list as the artist.
    #[test]
    fn the_live_feed_reads_each_friends_latest_song() {
        let json = serde_json::json!({"entities": [
            {"followEntity": {"uri": "spotify:user:a", "activity": {
                "entityUri": "spotify:track:1", "isPlaying": false,
                "contextUri": "spotify:playlist:p", "timestamp": "2026-10-14T17:00:00Z"}}},
            {"userEntity": {"uri": "spotify:user:b", "activity": {
                "entityUri": "spotify:track:2", "isPlaying": true,
                "contextUri": "spotify:list:popular-release-segments-main-roles:artist_X",
                "timestamp": "2026-10-14T17:30:00Z"}}},
            {"followEntity": {"uri": "spotify:user:a", "activity": {
                "entityUri": "spotify:track:3", "contextUri": "spotify:track:3",
                "timestamp": "2026-10-14T17:10:00Z"}}},
            {"followEntity": {"uri": "spotify:user:c", "activity": {
                "entityUri": "spotify:episode:4", "timestamp": "2026-10-14T17:20:00Z"}}},
            {"followEntity": {"uri": "spotify:artist:d"}},
        ]});
        let live = live_from_json(&json).unwrap();
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].user_uri, "spotify:user:b");
        assert!(live[0].playing);
        assert_eq!(live[0].context_uri.as_deref(), Some("spotify:artist:X"));
        assert_eq!(live[1].track_uri, "spotify:track:3");
        assert!(!live[1].playing);
        assert_eq!(live[1].context_uri.as_deref(), Some("spotify:track:3"));
        assert!(live_from_json(&serde_json::json!({})).is_err());
    }

    /// The buddy list reads newest first, and an entry without a user or a
    /// track is skipped rather than shown blank.
    #[test]
    fn friend_activity_reads_newest_first_and_skips_incomplete_entries() {
        let json = serde_json::json!({"friends": [
            {"timestamp": 1000, "user": {"uri": "spotify:user:a", "name": "Ann"},
             "track": {"uri": "spotify:track:1", "name": "Old", "imageUrl": "http://i.scdn.co/image/x",
                       "artist": {"uri": "spotify:artist:1", "name": "Band"},
                       "album": {"uri": "spotify:album:1", "name": "Record"},
                       "context": {"uri": "spotify:playlist:p", "name": "Mix", "index": 0}}},
            {"timestamp": 2000, "user": {"uri": "spotify:user:b", "name": ""},
             "track": {"uri": "spotify:track:2", "name": "New"}},
            {"timestamp": 3000, "user": {"uri": "spotify:user:c", "name": "Cy"}},
        ]});
        let friends = friends_from_json(&json);
        assert_eq!(friends.len(), 2);
        assert_eq!(friends[0].track_name, "New");
        assert_eq!(friends[0].user_name, "spotify:user:b");
        assert_eq!(friends[0].context_uri, None);
        assert_eq!(friends[1].user_name, "Ann");
        assert_eq!(friends[1].artist_name, "Band");
        assert_eq!(friends[1].artist_uri.as_deref(), Some("spotify:artist:1"));
        assert_eq!(friends[1].album_uri.as_deref(), Some("spotify:album:1"));
        assert_eq!(friends[0].album_uri, None);
        assert_eq!(friends[1].context_name.as_deref(), Some("Mix"));
        assert!(friends_from_json(&serde_json::json!({})).is_empty());
    }
}
