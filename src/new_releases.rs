//! The latest releases from the artists the account follows, for Home's
//! **New releases** shelf and the page it opens.
//!
//! Spotify's own apps read them from the private Pathfinder feed behind their
//! "What's New" bell (`queryWhatsNewFeed`), with the playback session's tokens.
//! Spotify can change that query without notice, so the Web API's followed
//! artists and their latest albums stand in when it fails. The feed is only
//! read: the mutation that marks its items as seen is never sent, so reading
//! here leaves the bell in Spotify's apps as it was.

use std::time::{Duration, Instant};

use librespot_core::Session;

use crate::api::ApiError;
use crate::api::models::{Album, ArtistRef, Image};
use crate::http::Http;

const ENDPOINT: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const QUERY_HASH: &str = "d889c8c936ab192af8ced595427f5ba2acdf63478fdc0a181c8d477f8322630e";
const TIMEOUT: Duration = Duration::from_secs(30);

/// The most releases the shelf and its page hold, as many as the feed's
/// first page, which is all Spotify's own apps read.
pub const LIMIT: usize = 50;

/// The followed artists the Web API fallback asks about: one request each.
pub const FALLBACK_ARTISTS: usize = 50;

/// How long the Web API stand-in's answer is kept: it costs a request for
/// each followed artist, and releases come out once a day at most.
const FALLBACK_AGE: Duration = Duration::from_secs(60 * 60);

/// The feed reader, with the Web API stand-in's last answer for one account.
pub struct NewReleases {
    http: Http,
    fallback: tokio::sync::Mutex<Option<(String, Instant, Vec<Album>)>>,
}

impl NewReleases {
    pub fn new(http: Http) -> Self {
        Self {
            http,
            fallback: tokio::sync::Mutex::default(),
        }
    }

    /// Reads the account's feed of new releases over the playback session.
    pub async fn feed(&self, session: &Session) -> Result<Vec<Album>, ApiError> {
        tokio::time::timeout(TIMEOUT, read_feed(session, &self.http))
            .await
            .map_err(|_| {
                ApiError::Network("New releases took too long to load. Try again.".into())
            })?
    }

    /// The Web API stand-in's answer for `account`, while it is recent.
    pub async fn cached_fallback(&self, account: &str) -> Option<Vec<Album>> {
        self.fallback
            .lock()
            .await
            .as_ref()
            .filter(|(owner, at, _)| owner == account && at.elapsed() < FALLBACK_AGE)
            .map(|(_, _, albums)| albums.clone())
    }

    pub async fn keep_fallback(&self, account: &str, albums: &[Album]) {
        *self.fallback.lock().await = Some((account.to_owned(), Instant::now(), albums.to_vec()));
    }
}

async fn read_feed(session: &Session, http: &Http) -> Result<Vec<Album>, ApiError> {
    let client = http.client().map_err(ApiError::Network)?;
    let token = session
        .login5()
        .auth_token()
        .await
        .map_err(|_| session_error())?;
    let client_token = session
        .spclient()
        .client_token()
        .await
        .map_err(|_| session_error())?;
    let response = client
        .post(ENDPOINT)
        .bearer_auth(&token.access_token)
        .header("client-token", &client_token)
        .json(&query())
        .send()
        .await
        .map_err(|error| ApiError::Network(error.without_url().to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(ApiError::RateLimited);
    }
    if !status.is_success() {
        return Err(ApiError::Status {
            status: status.as_u16(),
            message: "Spotify couldn't load new releases through the playback session.".into(),
        });
    }
    let body = response
        .json::<serde_json::Value>()
        .await
        .map_err(|_| response_error())?;
    parse(&body)
}

fn session_error() -> ApiError {
    ApiError::Network("Couldn't authorize the playback session for new releases. Try again.".into())
}

fn response_error() -> ApiError {
    ApiError::Decode("New releases are unavailable in Spotify's response. Try again later.".into())
}

/// The first page of the feed, albums only, as Spotify's web player asks
/// for it.
fn query() -> serde_json::Value {
    serde_json::json!({
        "operationName": "queryWhatsNewFeed",
        "variables": {
            "offset": 0,
            "limit": LIMIT,
            "onlyUnPlayedItems": false,
            "includedContentTypes": ["ALBUM"],
            "includeEpisodeContentRatingsV2": false
        },
        "extensions": {"persistedQuery": {"version": 1, "sha256Hash": QUERY_HASH}}
    })
}

/// The releases in a feed answer, newest first. Items that are not albums,
/// such as episodes or event notices, are passed over; an answer without the
/// feed at all is an error rather than an empty shelf.
fn parse(body: &serde_json::Value) -> Result<Vec<Album>, ApiError> {
    if body.get("errors").is_some_and(|errors| {
        !errors.is_null() && errors.as_array().is_none_or(|errors| !errors.is_empty())
    }) {
        return Err(response_error());
    }
    let items = body
        .pointer("/data/whatsNewFeedItems/items")
        .and_then(|items| items.as_array())
        .ok_or_else(response_error)?;
    let mut albums = Vec::new();
    for item in items {
        if item
            .pointer("/content/__typename")
            .and_then(|kind| kind.as_str())
            != Some("AlbumResponseWrapper")
        {
            continue;
        }
        let Some(data) = item.pointer("/content/data") else {
            continue;
        };
        if let Some(album) = album(data)
            && !albums.iter().any(|known: &Album| known.uri == album.uri)
        {
            albums.push(album);
        }
    }
    Ok(newest_first(albums))
}

fn album(data: &serde_json::Value) -> Option<Album> {
    if data.get("__typename").and_then(|kind| kind.as_str()) != Some("Album") {
        return None;
    }
    let uri = data.get("uri")?.as_str()?;
    let id = uri
        .strip_prefix("spotify:album:")
        .filter(|id| id.len() == 22 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()))?;
    let name = data.get("name")?.as_str()?;
    let album_type = data
        .get("albumType")
        .and_then(|kind| kind.as_str())
        .map(|kind| kind.to_ascii_lowercase());
    let release_date = data
        .pointer("/date/isoString")
        .and_then(|date| date.as_str())
        .map(|date| release_date(date, data.pointer("/date/precision")));
    let artists = data
        .pointer("/artists/items")
        .and_then(|items| items.as_array())
        .into_iter()
        .flatten()
        .filter_map(|artist| {
            let name = artist.pointer("/profile/name")?.as_str()?;
            let uri = artist.get("uri").and_then(|uri| uri.as_str());
            Some(ArtistRef {
                id: uri
                    .and_then(|uri| uri.strip_prefix("spotify:artist:"))
                    .map(str::to_owned),
                name: name.to_owned(),
                uri: uri.map(str::to_owned),
            })
        })
        .collect();
    let images = data
        .pointer("/coverArt/sources")
        .and_then(|sources| sources.as_array())
        .into_iter()
        .flatten()
        .filter_map(|source| serde_json::from_value::<Image>(source.clone()).ok())
        .filter(|image| image.url.starts_with("https://"))
        .collect();
    Some(Album {
        id: id.to_owned(),
        uri: uri.to_owned(),
        name: name.to_owned(),
        album_type,
        release_date,
        artists,
        images,
        ..Album::default()
    })
}

/// A feed date in the Web API's form: `2026-10-16`, or `2026-10` and
/// `2026` when Spotify only knows the month or the year.
fn release_date(iso: &str, precision: Option<&serde_json::Value>) -> String {
    let day = iso.get(..10).unwrap_or(iso);
    match precision.and_then(|precision| precision.as_str()) {
        Some("YEAR") => day.get(..4).unwrap_or(day).to_owned(),
        Some("MONTH") => day.get(..7).unwrap_or(day).to_owned(),
        _ => day.to_owned(),
    }
}

/// Releases sorted newest first, ties kept in the order given, at most
/// [`LIMIT`] of them. Web API dates are `YYYY`, `YYYY-MM` or `YYYY-MM-DD`,
/// which sort as text; a release without a date goes last.
pub fn newest_first(mut albums: Vec<Album>) -> Vec<Album> {
    albums.sort_by(|a, b| b.release_date.cmp(&a.release_date));
    albums.truncate(LIMIT);
    albums
}

/// The Web API stand-in: each followed artist's latest albums and singles,
/// merged, without repeats, newest first.
pub fn merge_artist_releases(pages: Vec<Vec<Album>>) -> Vec<Album> {
    let mut albums: Vec<Album> = Vec::new();
    for album in pages.into_iter().flatten() {
        if matches!(
            album.album_group.as_deref(),
            Some("appears_on" | "compilation")
        ) {
            continue;
        }
        if !albums.iter().any(|known| known.uri == album.uri) {
            albums.push(album);
        }
    }
    newest_first(albums)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn feed_item(uri: &str, name: &str, kind: &str, date: &str) -> serde_json::Value {
        json!({
            "id": "music_release:1",
            "state": {"state": "NEW"},
            "content": {
                "__typename": "AlbumResponseWrapper",
                "data": {
                    "__typename": "Album",
                    "uri": uri,
                    "name": name,
                    "albumType": kind,
                    "date": {"isoString": date, "precision": "DAY"},
                    "artists": {"items": [{"uri": "spotify:artist:0000000000000000000001", "profile": {"name": "Artist"}}]},
                    "coverArt": {"sources": [{"url": "https://i.scdn.co/image/a", "width": 300, "height": 300}]}
                }
            }
        })
    }

    #[test]
    fn reads_the_feeds_albums_newest_first() {
        let body = json!({"data": {"whatsNewFeedItems": {"items": [
            feed_item("spotify:album:1111111111111111111111", "Older", "ALBUM", "2026-10-02T00:00:00Z"),
            feed_item("spotify:album:2222222222222222222222", "Newest", "SINGLE", "2026-10-16T00:00:00Z"),
            {"content": {"__typename": "EpisodeOrChapterResponseWrapper", "data": {"__typename": "Episode"}}},
            feed_item("spotify:album:2222222222222222222222", "Newest", "SINGLE", "2026-10-16T00:00:00Z"),
        ], "totalCount": 4}}});
        let albums = parse(&body).unwrap();
        assert_eq!(
            albums
                .iter()
                .map(|album| album.name.as_str())
                .collect::<Vec<_>>(),
            ["Newest", "Older"]
        );
        let newest = &albums[0];
        assert_eq!(newest.id, "2222222222222222222222");
        assert_eq!(newest.album_type.as_deref(), Some("single"));
        assert_eq!(newest.release_date.as_deref(), Some("2026-10-16"));
        assert_eq!(newest.artists[0].name, "Artist");
        assert_eq!(
            newest.artists[0].id.as_deref(),
            Some("0000000000000000000001")
        );
        assert_eq!(newest.images[0].width, Some(300));
    }

    #[test]
    fn a_refused_or_missing_feed_is_not_an_empty_shelf() {
        assert!(parse(&json!({"errors": [{"message": "PersistedQueryNotFound"}]})).is_err());
        assert!(parse(&json!({"data": null})).is_err());
        assert!(
            parse(&json!({"data": {"whatsNewFeedItems": {"items": []}}}))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn bad_uris_and_unknown_cards_are_passed_over() {
        let mut bad = feed_item(
            "spotify:album:short",
            "Bad",
            "ALBUM",
            "2026-10-02T00:00:00Z",
        );
        let mut unknown = feed_item(
            "spotify:album:3333333333333333333333",
            "Unknown",
            "ALBUM",
            "2026-10-02T00:00:00Z",
        );
        unknown["content"]["data"]["__typename"] = json!("FutureThing");
        bad["content"]["data"]["coverArt"] = json!(null);
        let body = json!({"data": {"whatsNewFeedItems": {"items": [bad, unknown]}}});
        assert!(parse(&body).unwrap().is_empty());
    }

    #[test]
    fn dates_keep_the_precision_spotify_knows() {
        assert_eq!(
            release_date("2026-10-16T00:00:00Z", Some(&json!("DAY"))),
            "2026-10-16"
        );
        assert_eq!(
            release_date("2026-10-01T00:00:00Z", Some(&json!("MONTH"))),
            "2026-10"
        );
        assert_eq!(
            release_date("2026-01-01T00:00:00Z", Some(&json!("YEAR"))),
            "2026"
        );
    }

    #[test]
    fn the_fallback_merges_artists_releases_newest_first_without_guest_spots() {
        let release = |id: char, date: &str, group: &str| Album {
            id: id.to_string().repeat(22),
            uri: format!("spotify:album:{}", id.to_string().repeat(22)),
            name: id.to_string(),
            release_date: Some(date.into()),
            album_group: Some(group.into()),
            ..Album::default()
        };
        let merged = merge_artist_releases(vec![
            vec![
                release('a', "2026-09-01", "album"),
                release('b', "2026-10-10", "single"),
            ],
            vec![
                release('b', "2026-10-10", "single"),
                release('c', "2026-10-12", "appears_on"),
                release('d', "2025", "album"),
            ],
        ]);
        assert_eq!(
            merged
                .iter()
                .map(|album| album.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "a", "d"]
        );
    }

    #[test]
    fn the_query_reads_albums_and_never_marks_them_seen() {
        let query = query();
        assert_eq!(query["operationName"], "queryWhatsNewFeed");
        assert_eq!(query["variables"]["includedContentTypes"], json!(["ALBUM"]));
        assert_eq!(query["variables"]["limit"], LIMIT);
        assert!(!query.to_string().contains("SetItemsStateInWhatsNewFeed"));
    }
}
