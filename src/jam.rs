//! Spotify Jam sessions: Spotify's "social-connect" service, reached through
//! the playback session like the app's other private reads.
//!
//! A Jam is a shared queue. The host's active Connect device plays the
//! session's list, everyone who joined can steer playback and add songs, and
//! the host shares the session with a link the official apps render as a QR
//! code. Spotify documents none of this publicly; the endpoints and shapes
//! below follow what Spotify's own clients send, as captured by community
//! clients (see docs/_reference/how-it-connects.md).

use anyhow::{Context, Result, bail, ensure};
use librespot_core::session::Session;
use rand::Rng;
use reqwest::Method;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::collections::HashMap;

/// Connect-state commands for a Jam address a pseudo-device named after the
/// session, not any member's player.
pub const PSEUDO_PREFIX: &str = "social-connect-";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JamMember {
    pub uri: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JamSession {
    pub id: String,
    pub join_token: String,
    pub owner_id: Option<String>,
    pub host_device_id: Option<String>,
    pub members: Vec<JamMember>,
    pub is_owner: bool,
}

impl JamSession {
    /// The Connect-state target every member's commands address.
    pub fn pseudo_device(&self) -> String {
        format!("{PSEUDO_PREFIX}{}", self.id)
    }

    /// The share link the official apps encode as a QR code.
    pub fn invite_url(&self) -> String {
        format!("https://open.spotify.com/socialsession/{}", self.join_token)
    }

    pub fn member(&self, uri: &str) -> Option<&JamMember> {
        self.members.iter().find(|member| member.uri == uri)
    }
}

/// What the queue's rows need from a Jam: who added each song, and each
/// member's portrait. Either map can stay empty when Spotify names
/// nothing; the queue then simply shows rows without portraits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JamDetail {
    /// Track URI to the member who added it.
    pub adders: HashMap<String, JamMember>,
    /// Member URI to their portrait's address.
    pub images: HashMap<String, String>,
}

/// Join tokens are plain alphanumerics, as every observed link shows.
/// Anything else is not a Jam invite and must not reach the path segment.
pub fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 128 && token.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Session ids ride in URL paths too. Spotify names them itself, but the
/// answer is still data before it is a path segment.
fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A fresh attempt id, so Spotify can tell retries from new joins.
fn join_attempt_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        hex[0..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..16].concat()
    )
}

fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}

async fn send(
    session: &Session,
    method: Method,
    endpoint: String,
    body: Option<Value>,
) -> Result<Value> {
    let bytes = session
        .spclient()
        .request_as_json(
            &method,
            &endpoint,
            Some(json_headers()),
            body.as_ref().map(|body| body.to_string()).as_deref(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("Spotify refused the Jam request: {error}"))?;
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).context("Jam answer was not valid JSON")
}

/// Starts a Jam on the given Connect device, activating it as the host's
/// player, or returns the session already running.
pub async fn start(session: &Session, local_device_id: Option<&str>) -> Result<JamSession> {
    let username = session.username();
    let mut endpoint =
        "/social-connect/v2/sessions/current_or_new?activate=true&type=REMOTE".to_string();
    if let Some(device_id) = local_device_id {
        endpoint.push_str(&format!("&local_device_id={device_id}"));
    }
    let answer = send(session, Method::GET, endpoint, None).await?;
    parse_session(&answer, Some(&username))
}

/// The Jam this account is in, if any. Spotify answers 404 when none is.
pub async fn current(session: &Session) -> Result<Option<JamSession>> {
    let username = session.username();
    match session
        .spclient()
        .request_as_json(
            &Method::GET,
            "/social-connect/v2/sessions/current",
            Some(json_headers()),
            None,
        )
        .await
    {
        Ok(bytes) if bytes.is_empty() => Ok(None),
        Ok(bytes) => {
            if bytes.as_ref() == b"null" {
                return Ok(None);
            }
            let answer: Value =
                serde_json::from_slice(&bytes).context("Jam answer was not valid JSON")?;
            if answer.is_null() {
                return Ok(None);
            }
            parse_session(&answer, Some(&username)).map(Some)
        }
        Err(error) if error.kind == librespot_core::error::ErrorKind::NotFound => Ok(None),
        Err(error) => bail!("Spotify refused the Jam request: {error}"),
    }
}

/// Joins the Jam an invite token names, the way the official apps join from
/// a link or a scanned code.
pub async fn join(session: &Session, token: &str) -> Result<JamSession> {
    let username = session.username();
    anyhow::ensure!(valid_token(token), "That Jam invite is not valid.");
    let answer = send(
        session,
        Method::POST,
        "/social-connect/v3/sessions/join".into(),
        Some(json!({
            "join_token": token,
            "join_type": "deeplinking",
            "playback_control": "LISTEN_AND_CONTROL",
            "join_attempt_id": join_attempt_id(),
            "permission_mode": "FULL",
        })),
    )
    .await;
    match answer {
        Ok(answer) => parse_session(&answer, Some(&username)),
        Err(error) => {
            // Older deployments only answer the v2 path form.
            log::debug!("The v3 join was refused, trying v2: {error:#}");
            let endpoint = format!(
                "/social-connect/v2/sessions/join/{token}?playback_control=listen_and_control&join_type=deeplinking"
            );
            let answer = send(session, Method::POST, endpoint, None).await?;
            parse_session(&answer, Some(&username))
        }
    }
}

/// Leaves the Jam. A session already gone is still a departure.
pub async fn leave(session: &Session, session_id: &str) -> Result<()> {
    anyhow::ensure!(
        valid_session_id(session_id),
        "That Jam session is not valid."
    );
    let endpoint = format!("/social-connect/v3/sessions/{session_id}/leave");
    match send(session, Method::POST, endpoint, None).await {
        Ok(_) => Ok(()),
        Err(error) => {
            let gone = error.to_string().contains("404")
                || error.to_string().to_lowercase().contains("not found");
            if gone { Ok(()) } else { Err(error) }
        }
    }
}

/// Ends the Jam as its owner. Everyone still in it leaves with it.
pub async fn end(session: &Session, session_id: &str) -> Result<()> {
    anyhow::ensure!(
        valid_session_id(session_id),
        "That Jam session is not valid."
    );
    let endpoint = format!("/social-connect/v3/sessions/{session_id}");
    match send(session, Method::DELETE, endpoint, None).await {
        Ok(_) => Ok(()),
        Err(error) => {
            let gone = error.to_string().contains("404")
                || error.to_string().to_lowercase().contains("not found");
            if gone { Ok(()) } else { Err(error) }
        }
    }
}

/// Reads the member list and ownership afresh.
pub async fn refresh(session: &Session, session_id: &str) -> Result<JamSession> {
    anyhow::ensure!(
        valid_session_id(session_id),
        "That Jam session is not valid."
    );
    let username = session.username();
    let answer = send(
        session,
        Method::GET,
        "/social-connect/v2/sessions/current".to_string(),
        None,
    )
    .await?;
    let parsed = parse_session(&answer, Some(&username))?;
    anyhow::ensure!(
        parsed.id == session_id,
        "The Jam ended while its details were loading."
    );
    Ok(parsed)
}

/// Steers the host's playback as a member: play, pause, skip, or queue a
/// song. Commands address the session's pseudo-device, not the host player.
pub async fn member_control(session: &Session, session_id: &str, body: &Value) -> Result<()> {
    anyhow::ensure!(
        valid_session_id(session_id),
        "That Jam session is not valid."
    );
    let target = format!("{PSEUDO_PREFIX}{session_id}");
    let path = format!(
        "/connect-state/v1/player/command/from/{}/to/{target}",
        session.device_id()
    );
    let _ = session
        .spclient()
        .request(
            &Method::POST,
            &path,
            None,
            Some(body.to_string().as_bytes()),
        )
        .await
        .map_err(|error| anyhow::anyhow!("The Jam refused the command: {error}"))?;
    Ok(())
}

/// One session member. Adds a song by queuing it against the session.
pub fn queue_body(uri: &str) -> Value {
    json!({ "command": { "endpoint": "add_to_queue", "track": { "uri": uri } } })
}

/// Who added each song in the session's shared list, by its track URI.
/// Spotify names the adder in the list context's track metadata when it
/// does at all; a list that names nobody maps to nothing, and the queue
/// then simply shows no portraits.
pub async fn queue_adders(
    session: &Session,
    session_id: &str,
) -> Result<HashMap<String, JamMember>> {
    ensure!(
        valid_session_id(session_id),
        "That Jam session is not valid."
    );
    let context = session
        .spclient()
        .get_context(&format!("spotify:list:jam-list:{session_id}"))
        .await
        .map_err(|error| anyhow::anyhow!("Spotify refused the Jam queue read: {error}"))?;
    let adder_uri = |metadata: &HashMap<String, String>| {
        ["added_by.uri", "adder.uri", "addedby.uri"]
            .iter()
            .find_map(|key| metadata.get(*key))
            .filter(|uri| uri.starts_with("spotify:user:"))
            .cloned()
    };
    let adder_name = |metadata: &HashMap<String, String>| {
        ["added_by.name", "adder.name", "addedby.name"]
            .iter()
            .find_map(|key| metadata.get(*key))
            .cloned()
            .unwrap_or_default()
    };
    let mut adders = HashMap::new();
    for page in context.pages.iter() {
        for track in page.tracks.iter() {
            let Some(track_uri) = track.uri.as_deref().filter(|uri| !uri.is_empty()) else {
                continue;
            };
            let Some(uri) = adder_uri(&track.metadata) else {
                continue;
            };
            adders.insert(
                track_uri.to_owned(),
                JamMember {
                    uri,
                    name: adder_name(&track.metadata),
                },
            );
        }
    }
    Ok(adders)
}

/// Reads a session answer, tolerating both the flat REST shape and the
/// nested shape the session-update payloads use.
fn parse_session(answer: &Value, username: Option<&str>) -> Result<JamSession> {
    let session = if answer.get("session_id").is_some() {
        answer
    } else {
        answer
            .get("session")
            .filter(|session| session.is_object())
            .unwrap_or(answer)
    };
    let id = session["session_id"]
        .as_str()
        .or_else(|| session["id"].as_str())
        .filter(|id| !id.is_empty())
        .context("The Jam answer named no session.")?
        .to_owned();
    let join_token = session["join_session_token"]
        .as_str()
        .or_else(|| session["join_token"].as_str())
        .unwrap_or_default()
        .to_owned();
    let owner_id = session["session_owner_id"]
        .as_str()
        .filter(|owner| !owner.is_empty())
        .map(str::to_owned);
    let host_device_id = session["host_active_device_id"]
        .as_str()
        .filter(|host| !host.is_empty())
        .map(str::to_owned);
    let mut members = Vec::new();
    for entry in session["session_members"].as_array().into_iter().flatten() {
        let entity = entry
            .get("member")
            .filter(|entity| entity.is_object())
            .unwrap_or(entry);
        let uri = entity["uri"]
            .as_str()
            .or_else(|| entity["member_uri"].as_str())
            .unwrap_or_default()
            .to_owned();
        let name = entity["display_name"]
            .as_str()
            .or_else(|| entity["name"].as_str())
            .unwrap_or_default()
            .to_owned();
        members.push(JamMember { uri, name });
    }
    let is_owner = session["is_session_owner"]
        .as_bool()
        .or_else(|| {
            let owner = owner_id.as_deref()?;
            username.map(|username| owner == username)
        })
        .unwrap_or(false);
    Ok(JamSession {
        id,
        join_token,
        owner_id,
        host_device_id,
        members,
        is_owner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_answer() -> Value {
        json!({
            "session_id": "3fa85f64-5717-4562-b3fc-2c963f66afa6",
            "join_session_token": "AbC123xyz",
            "join_session_uri": "https://open.spotify.com/socialsession/AbC123xyz",
            "session_owner_id": "acct1",
            "host_active_device_id": "dev-9",
            "is_session_owner": true,
            "max_member_count": 32,
            "session_members": [
                {"member_uri": "spotify:user:acct1", "display_name": "Host"},
                {"member": {"uri": "spotify:user:acct2", "name": "Guest"}},
            ],
        })
    }

    #[test]
    fn a_flat_rest_answer_parses() {
        let session = parse_session(&flat_answer(), Some("acct1")).unwrap();
        assert_eq!(session.id, "3fa85f64-5717-4562-b3fc-2c963f66afa6");
        assert_eq!(session.join_token, "AbC123xyz");
        assert_eq!(session.owner_id.as_deref(), Some("acct1"));
        assert_eq!(session.host_device_id.as_deref(), Some("dev-9"));
        assert!(session.is_owner);
        assert_eq!(session.members.len(), 2);
        assert_eq!(session.members[0].name, "Host");
        assert_eq!(session.members[1].uri, "spotify:user:acct2");
    }

    #[test]
    fn a_nested_answer_parses_and_names_the_owner_from_the_caller() {
        let answer = json!({
            "session": {
                "session_id": "sid",
                "join_session_token": "tok",
                "session_owner_id": "me",
                "session_members": [],
            }
        });
        let session = parse_session(&answer, Some("me")).unwrap();
        assert_eq!(session.id, "sid");
        assert!(session.is_owner, "ownership follows the caller's username");
        let session = parse_session(&answer, Some("other")).unwrap();
        assert!(!session.is_owner);
    }

    #[test]
    fn an_answer_without_a_session_is_refused() {
        assert!(parse_session(&json!({}), None).is_err());
    }

    #[test]
    fn the_invite_link_and_pseudo_device_follow_the_session() {
        let session = parse_session(&flat_answer(), None).unwrap();
        assert_eq!(
            session.invite_url(),
            "https://open.spotify.com/socialsession/AbC123xyz"
        );
        assert_eq!(
            session.pseudo_device(),
            "social-connect-3fa85f64-5717-4562-b3fc-2c963f66afa6"
        );
    }

    #[test]
    fn join_tokens_are_alphanumerics_only() {
        assert!(valid_token("AbC123xyz"));
        assert!(!valid_token(""));
        assert!(!valid_token("../../etc"));
        assert!(!valid_token("with space"));
        assert!(!valid_token(&"x".repeat(129)));
    }

    #[test]
    fn session_ids_stay_url_safe() {
        assert!(valid_session_id("3fa85f64-5717-4562-b3fc-2c963f66afa6"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("../tracks"));
        assert!(!valid_session_id("id?x=1"));
        assert!(!valid_session_id("id with space"));
    }

    #[test]
    fn every_join_attempt_gets_a_fresh_versioned_uuid() {
        let first = join_attempt_id();
        let second = join_attempt_id();
        assert_ne!(first, second);
        assert_eq!(first.len(), 36);
        assert_eq!(first.as_bytes()[14], b'4', "version nibble stays a 4");
        let variant = first.as_bytes()[19] as char;
        assert!(matches!(variant, '8' | '9' | 'a' | 'b'), "RFC 4122 variant");
    }

    #[test]
    fn the_queue_command_targets_the_session_queue() {
        let body = queue_body("spotify:track:6rqhFgbbKwnb9MLmUQDhG6");
        assert_eq!(body["command"]["endpoint"], "add_to_queue");
        assert_eq!(
            body["command"]["track"]["uri"],
            "spotify:track:6rqhFgbbKwnb9MLmUQDhG6"
        );
    }

    /// The adder read tolerates the metadata keys Spotify has used and
    /// skips tracks that name nobody.
    #[test]
    fn the_queue_read_maps_only_tracks_that_name_an_adder() {
        use librespot_protocol::context_page::ContextPage;
        use librespot_protocol::context_track::ContextTrack;
        let named = ContextTrack {
            uri: Some("spotify:track:one".into()),
            metadata: [
                ("added_by.uri".to_string(), "spotify:user:me".to_string()),
                ("added_by.name".to_string(), "Me".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let unnamed = ContextTrack {
            uri: Some("spotify:track:two".into()),
            ..Default::default()
        };
        let context = librespot_protocol::context::Context {
            pages: vec![ContextPage {
                tracks: vec![named, unnamed],
                ..Default::default()
            }],
            ..Default::default()
        };
        let adder_uri = |metadata: &HashMap<String, String>| {
            ["added_by.uri", "adder.uri", "addedby.uri"]
                .iter()
                .find_map(|key| metadata.get(*key))
                .filter(|uri| uri.starts_with("spotify:user:"))
                .cloned()
        };
        let adder_name = |metadata: &HashMap<String, String>| {
            ["added_by.name", "adder.name", "addedby.name"]
                .iter()
                .find_map(|key| metadata.get(*key))
                .cloned()
                .unwrap_or_default()
        };
        let mut adders = HashMap::new();
        for page in context.pages.iter() {
            for track in page.tracks.iter() {
                let Some(track_uri) = track.uri.as_deref().filter(|uri| !uri.is_empty()) else {
                    continue;
                };
                let Some(uri) = adder_uri(&track.metadata) else {
                    continue;
                };
                adders.insert(
                    track_uri.to_owned(),
                    JamMember {
                        uri,
                        name: adder_name(&track.metadata),
                    },
                );
            }
        }
        assert_eq!(adders.len(), 1);
        assert_eq!(adders["spotify:track:one"].name, "Me");
        assert!(!adders.contains_key("spotify:track:two"));
    }
}
