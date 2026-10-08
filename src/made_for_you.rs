//! Spotify's personalized Home shelf, read with the existing playback session.
//!
//! The private Pathfinder contract follows Psst's homeSection integration
//! (jpochyla/psst#745). It can change independently of the public Web API.
//! This reader never retries a refusal through a different grant. The backend
//! may separately offer an explicitly labeled shared-search fallback.

use std::collections::HashSet;
use std::future::Future;
use std::time::{Duration, Instant};

use librespot_core::Session;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::api::ApiError;
use crate::api::models::{Image, Owner, Playlist};
use crate::http::Http;

const ENDPOINT: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const SECTION: &str = "spotify:section:0JQ5DAUnp4wcj0bCb3wh3S";
const QUERY_HASH: &str = "eb3fba2d388cf4fc4d696b1757a58584e9538a3b515ea742e9cc9465807340be";
// Spotify DJ is a special context that librespot cannot play as a playlist.
const UNSUPPORTED_DJ_URI: &str = "spotify:playlist:37i9dQZF1EYkqdzj48dyYq";
const CACHE_AGE: Duration = Duration::from_secs(600);
const LOAD_TIMEOUT: Duration = Duration::from_secs(60);
const PAGE_SIZE: usize = 20;
const MAX_PAGES: usize = 10;

/// One account's in-memory shelf and service cooldown. Holding the lock
/// across a read also combines concurrent refreshes into one cached result.
pub struct MadeForYou {
    state: Mutex<State>,
    http: Http,
}

#[derive(Default)]
struct State {
    account: String,
    cached: Option<(Instant, Vec<Playlist>)>,
    cooldown: Option<(Instant, Duration)>,
}

impl State {
    fn cached(&self, account: &str, force: bool) -> Option<Vec<Playlist>> {
        self.cached.as_ref().and_then(|(at, playlists)| {
            (self.account == account && !force && at.elapsed() < CACHE_AGE)
                .then(|| playlists.clone())
        })
    }

    fn cooling_down(&self) -> bool {
        self.cooldown
            .is_some_and(|(at, duration)| at.elapsed() < duration)
    }
}

impl MadeForYou {
    pub fn new(http: Http) -> Self {
        Self {
            state: Mutex::new(State::default()),
            http,
        }
    }

    /// Returns the account's cached shelf or coalesces one bounded refresh.
    pub async fn load<F, Fut>(
        &self,
        session: &Session,
        force: bool,
        context_token: F,
    ) -> Result<Vec<Playlist>, ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, ApiError>>,
    {
        let mut state = self.state.lock().await;
        let account = session.username();
        if let Some(cached) = state.cached(&account, force) {
            return Ok(cached);
        }
        if state.account != account {
            state.account = account;
            state.cached = None;
            state.cooldown = None;
        }
        if state.cooling_down() {
            return Err(ApiError::RateLimited);
        }

        // Keep the lock across the entire read so concurrent refreshes share
        // one answer, but do not let slow token resolution or ten slow pages
        // block later requests indefinitely.
        tokio::time::timeout(LOAD_TIMEOUT, self.fetch(session, context_token, &mut state))
            .await
            .map_err(|_| {
                ApiError::Network("Made for you took too long to load. Try again.".into())
            })?
    }

    /// Resolves both tokens and reads every Home page under the caller's lock.
    async fn fetch<F, Fut>(
        &self,
        session: &Session,
        context_token: F,
        state: &mut State,
    ) -> Result<Vec<Playlist>, ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, ApiError>>,
    {
        let context_token = context_token().await?;
        let client = self.http.client().map_err(ApiError::Network)?;
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
        let timezone = jiff::tz::TimeZone::system();
        let timezone = timezone.iana_name().unwrap_or("UTC");
        let mut playlists = Vec::new();
        let mut seen = HashSet::new();
        for page in 0..MAX_PAGES {
            let offset = page * PAGE_SIZE;
            let response = client
                .post(ENDPOINT)
                .bearer_auth(&token.access_token)
                .header("client-token", &client_token)
                .json(&query(&session.country(), timezone, &context_token, offset))
                .send()
                .await
                .map_err(|error| ApiError::Network(error.without_url().to_string()))?;
            let status = response.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let wait =
                    librespot_core::http_client::HttpClient::get_retry_after(response.headers())
                        .unwrap_or(Duration::from_secs(30));
                state.cooldown = Some((Instant::now(), wait));
                log::warn!(
                    "Spotify rate limit source=session-home wait={}s",
                    wait.as_secs()
                );
                return Err(ApiError::RateLimited);
            }
            if !status.is_success() {
                return Err(ApiError::Status {
                    status: status.as_u16(),
                    message: "Spotify couldn't load Made for you through the playback session. Try again later.".into(),
                });
            }
            let body = response
                .json::<serde_json::Value>()
                .await
                .map_err(|_| response_error())?;
            let page = parse_page(body)?;
            let more = page.more(offset);
            let page_has_playlists = !page.playlists.is_empty();
            let added = page.playlists.into_iter().fold(0, |count, playlist| {
                if seen.insert(playlist.id.clone()) {
                    playlists.push(playlist);
                    count + 1
                } else {
                    count
                }
            });
            if !more {
                state.cached = Some((Instant::now(), playlists.clone()));
                log::debug!("Spotify route operation=MadeForYou source=session");
                return Ok(playlists);
            }
            // A repeated or malformed page must not silently replace a
            // complete shelf with a prefix, or spin forever.
            if added == 0 && page_has_playlists {
                return Err(response_error());
            }
        }
        Err(response_error())
    }
}

fn session_error() -> ApiError {
    ApiError::Network("Couldn't authorize the playback session for Made for you. Try again.".into())
}

fn response_error() -> ApiError {
    ApiError::Decode("Made for you is unavailable in Spotify's response. Try again later.".into())
}

fn query(country: &str, timezone: &str, context_token: &str, offset: usize) -> serde_json::Value {
    serde_json::json!({
        "operationName": "homeSection",
        "variables": {
            "uri": SECTION,
            "country": country,
            "timeZone": timezone,
            "sp_t": context_token,
            "sectionItemsLimit": PAGE_SIZE,
            "sectionItemsOffset": offset
        },
        "extensions": {"persistedQuery": {"version": 1, "sha256Hash": QUERY_HASH}}
    })
}

struct ShelfPage {
    playlists: Vec<Playlist>,
    count: usize,
    total: Option<usize>,
}

impl ShelfPage {
    fn more(&self, offset: usize) -> bool {
        self.count > 0
            && self.total.map_or(self.count == PAGE_SIZE, |total| {
                offset.saturating_add(self.count) < total
            })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SectionItems {
    items: Vec<serde_json::Value>,
    total_count: Option<usize>,
}

/// Preserve Spotify's order and localized titles. Unknown card types are
/// skipped; duplicate IDs are removed, never duplicate display names.
fn parse_page(body: serde_json::Value) -> Result<ShelfPage, ApiError> {
    if body.get("errors").is_some_and(|errors| {
        !errors.is_null() && errors.as_array().is_none_or(|errors| !errors.is_empty())
    }) {
        return Err(response_error());
    }
    let sections = body
        .pointer("/data/homeSections/sections")
        .and_then(|sections| sections.as_array())
        .ok_or_else(response_error)?;
    if sections.is_empty() {
        return Err(response_error());
    }
    let mut playlists = Vec::new();
    let mut count = 0;
    let mut total = Some(0usize);
    for section in sections {
        let items: SectionItems = serde_json::from_value(
            section
                .get("sectionItems")
                .cloned()
                .ok_or_else(response_error)?,
        )
        .map_err(|_| response_error())?;
        count += items.items.len();
        total = total.and_then(|sum| items.total_count.map(|section_count| sum + section_count));
        for item in &items.items {
            let Some(data) = item.pointer("/content/data") else {
                continue;
            };
            if data.get("__typename").and_then(|value| value.as_str()) != Some("Playlist") {
                continue;
            }
            let Some(uri) = data.get("uri").and_then(|value| value.as_str()) else {
                continue;
            };
            if uri == UNSUPPORTED_DJ_URI {
                continue;
            }
            let Some(id) = uri
                .strip_prefix("spotify:playlist:")
                .filter(|id| id.len() == 22 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()))
            else {
                continue;
            };
            let Some(name) = data.get("name").and_then(|value| value.as_str()) else {
                continue;
            };
            let images = data
                .pointer("/images/items")
                .and_then(|items| items.as_array())
                .into_iter()
                .flatten()
                .filter_map(|image| image.get("sources").and_then(|sources| sources.as_array()))
                .flatten()
                .filter_map(|source| serde_json::from_value::<Image>(source.clone()).ok())
                .filter(|image| image.url.starts_with("https://"))
                .collect();
            let owner = data.pointer("/ownerV2/data");
            let owner_uri = owner
                .and_then(|owner| owner.get("uri"))
                .and_then(|uri| uri.as_str());
            playlists.push(Playlist {
                id: id.into(),
                uri: uri.into(),
                name: name.into(),
                description: data
                    .get("description")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                images,
                owner: Owner {
                    id: owner_uri
                        .and_then(|uri| uri.strip_prefix("spotify:user:"))
                        .map(str::to_owned),
                    uri: owner_uri.map(str::to_owned),
                    display_name: owner
                        .and_then(|owner| owner.get("name"))
                        .and_then(|name| name.as_str())
                        .map(str::to_owned),
                },
                ..Default::default()
            });
        }
    }
    Ok(ShelfPage {
        playlists,
        count,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shelf(items: serde_json::Value) -> serde_json::Value {
        json!({"data":{"homeSections":{"sections":[{"sectionItems":{"items":items}}]}}})
    }

    #[test]
    fn reads_personalized_cards_without_english_name_or_owner_filters() {
        let result = parse_page(shelf(json!([
            {"content":{"data":{"__typename":"Playlist","uri":"spotify:playlist:1234567890123456789012","name":"Découvertes de la semaine","ownerV2":{"data":{"uri":"spotify:user:spotify","name":"Spotify"}},"images":{"items":[{"sources":[{"url":"https://i.scdn.co/image/a","width":300}]}]}}}},
            {"content":{"data":{"__typename":"Playlist","uri":"spotify:playlist:2234567890123456789012","name":"Your evening mix"}}},
            {"content":{"data":{"__typename":"NotFound"}}},
            {"content":null}
        ]))).unwrap();
        assert_eq!(result.playlists.len(), 2);
        assert_eq!(result.playlists[0].name, "Découvertes de la semaine");
        assert_eq!(result.playlists[0].images[0].width, Some(300));
        assert_eq!(result.playlists[0].owner.id.as_deref(), Some("spotify"));
        assert_eq!(result.playlists[1].name, "Your evening mix");
        assert_eq!(result.count, 4);
    }

    #[test]
    fn graphql_errors_and_missing_sections_are_not_empty_successes() {
        assert!(parse_page(json!({"errors":[{"message":"refused"}]})).is_err());
        assert!(parse_page(json!({"data":null})).is_err());
        assert!(parse_page(json!({"data":{"homeSections":{"sections":[]}}})).is_err());
        assert!(parse_page(shelf(json!([]))).unwrap().playlists.is_empty());
    }

    #[test]
    fn preserves_playlist_order_across_home_sections() {
        let result = parse_page(json!({"data":{"homeSections":{"sections":[
            {"sectionItems":{"items":[{"content":{"data":{"__typename":"Playlist","uri":"spotify:playlist:1234567890123456789012","name":"First"}}}]}},
            {"sectionItems":{"items":[{"content":{"data":{"__typename":"Playlist","uri":"spotify:playlist:2234567890123456789012","name":"Second"}}}]}}
        ]}}})).unwrap();
        assert_eq!(result.count, 2);
        assert_eq!(
            result
                .playlists
                .iter()
                .map(|playlist| playlist.name.as_str())
                .collect::<Vec<_>>(),
            ["First", "Second"]
        );
    }

    #[test]
    fn invalid_playlist_uris_and_unknown_cards_are_skipped() {
        let result = parse_page(shelf(json!([
            {"content":{"data":{"__typename":"Playlist","uri":"https://example.com","name":"wrong"}}},
            {"content":{"data":{"__typename":"Playlist","uri":"spotify:playlist:bad","name":"wrong"}}},
            {"content":{"data":{"__typename":"FutureCard","uri":"spotify:playlist:1234567890123456789012","name":"unknown"}}}
        ]))).unwrap();
        assert!(result.playlists.is_empty());
    }

    #[test]
    fn unsupported_dj_context_is_omitted_without_hiding_dj_named_playlists() {
        let result = parse_page(shelf(json!([
            {"content":{"data":{"__typename":"Playlist","uri":UNSUPPORTED_DJ_URI,"name":"DJ"}}},
            {"content":{"data":{"__typename":"Playlist","uri":"spotify:playlist:1234567890123456789012","name":"DJ Mix"}}}
        ]))).unwrap();
        assert_eq!(result.count, 2);
        assert_eq!(result.playlists.len(), 1);
        assert_eq!(result.playlists[0].name, "DJ Mix");
    }

    #[test]
    fn paging_uses_raw_card_counts_and_stops_on_empty_pages() {
        let mut page = ShelfPage {
            playlists: vec![],
            count: 20,
            total: None,
        };
        assert!(page.more(0));
        page.total = Some(20);
        assert!(!page.more(0));
        page.total = Some(30);
        assert!(page.more(0));
        page.count = 10;
        assert!(!page.more(20));
        page.count = 0;
        assert!(!page.more(0));
    }

    #[test]
    fn cache_is_scoped_to_account_and_manual_refresh_respects_cooldown() {
        let state = State {
            account: "alice".into(),
            cached: Some((Instant::now(), vec![])),
            cooldown: Some((Instant::now(), Duration::from_secs(120))),
        };
        assert!(state.cached("alice", false).is_some());
        assert!(state.cached("bob", false).is_none());
        assert!(state.cached("alice", true).is_none());
        assert!(state.cooling_down());
    }

    #[test]
    fn query_uses_the_verified_grant_for_spotify_home_context() {
        let query = query("CA", "America/Toronto", "personal-grant", 20);
        assert_eq!(query["variables"]["sectionItemsOffset"], 20);
        assert_eq!(query["variables"]["uri"], SECTION);
        assert_eq!(query["variables"]["sp_t"], "personal-grant");
    }
}
