//! Speakers Spotify will not let the Web API control, Sonos above all.
//!
//! Spotify marks some Connect devices as restricted: the Web API refuses
//! every player command aimed at one ("Restriction violated"), even while it
//! plays from this account, and leaves it out of `/me/player/devices`.
//! Spotify's own apps steer those devices over the Connect state service
//! instead, the one librespot's session already speaks: a transfer, and
//! player commands sent from this device to that one. Those work here too, so
//! commands for a restricted device take that route.
//!
//! A Sonos does not appear in the device list while idle, so it is found by
//! its ZeroConf endpoint (`/spotifyzc` on port 1400) and offered from there.
//! It asks for an OAuth token rather than librespot's login blob
//! (`tokenType: authorization_code`); before a transfer it is handed a
//! streaming token minted for its own client id. It answers OK to any
//! login, so that step cannot be confirmed; the transfer that follows is.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use librespot_core::session::Session;
use serde_json::{Value, json};

use crate::api::client::PlayRequest;
use crate::api::models::Device;
use crate::backend::RemoteAction;

const PORT: u16 = 1400;
const ZEROCONF_PATH: &str = "/spotifyzc";

/// A Sonos player known by its Spotify Connect device id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Speaker {
    pub device_id: String,
    pub name: String,
    pub address: IpAddr,
}

#[derive(Default)]
struct Known {
    speakers: HashMap<String, Speaker>,
    /// Devices Spotify reported as restricted.
    restricted: HashSet<String>,
    /// The device Spotify last reported as playing, when it is restricted.
    active_restricted: Option<String>,
}

fn known() -> std::sync::MutexGuard<'static, Known> {
    static KNOWN: OnceLock<Mutex<Known>> = OnceLock::new();
    KNOWN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Whether a ZeroConf receiver is a Sonos player.
pub fn is_sonos(receiver: &crate::zeroconf::Receiver) -> bool {
    receiver.port == PORT && receiver.path.trim_end_matches('/') == ZEROCONF_PATH
}

/// Remembers the Sonos players among resolved receivers.
pub fn remember(receivers: &[crate::zeroconf::Receiver]) {
    let mut known = known();
    for receiver in receivers.iter().filter(|receiver| is_sonos(receiver)) {
        if let Some(id) = &receiver.device_id {
            known.speakers.insert(
                id.clone(),
                Speaker {
                    device_id: id.clone(),
                    name: receiver.name.clone(),
                    address: receiver.address,
                },
            );
        }
    }
}

/// The Sonos player behind a Connect device id, if it is one.
pub fn speaker(device_id: &str) -> Option<Speaker> {
    known().speakers.get(device_id).cloned()
}

/// A device row for a Sonos receiver, which Spotify does not list while idle.
pub fn device_row(receiver: &crate::zeroconf::Receiver, active: bool) -> Option<Device> {
    if !is_sonos(receiver) {
        return None;
    }
    Some(Device {
        id: receiver.device_id.clone(),
        name: receiver.name.clone(),
        is_active: active,
        is_restricted: false,
        volume_percent: None,
        supports_volume: Some(true),
        kind: "speaker".into(),
    })
}

/// Notes which devices Spotify reports as restricted, and which one plays.
pub fn observe_devices(devices: &[Device]) {
    let mut known = known();
    for device in devices.iter().filter(|device| device.is_restricted) {
        if let Some(id) = &device.id {
            known.restricted.insert(id.clone());
        }
    }
}

pub fn observe_playing(device: Option<&Device>) {
    let mut known = known();
    known.active_restricted = device
        .filter(|device| device.is_restricted)
        .and_then(|device| device.id.clone());
    if let Some(id) = &known.active_restricted {
        let id = id.clone();
        known.restricted.insert(id);
    }
}

/// The restricted device a command is for: the one named, or with none
/// named, the restricted device that is playing.
pub fn restricted_target(device_id: Option<&str>) -> Option<String> {
    let known = known();
    match device_id {
        Some(id) if known.restricted.contains(id) || known.speakers.contains_key(id) => {
            Some(id.to_string())
        }
        Some(_) => None,
        None => known.active_restricted.clone(),
    }
}

/// The Connect state service's player command for a remote action.
pub fn command_body(
    action: RemoteAction,
    play: Option<&PlayRequest>,
    position_ms: u32,
    flag: bool,
    repeat: &str,
) -> Option<Value> {
    let command = match action {
        RemoteAction::Play => match play {
            None => json!({ "endpoint": "resume" }),
            Some(request) => play_command(request),
        },
        RemoteAction::Pause => json!({ "endpoint": "pause" }),
        RemoteAction::Next => json!({ "endpoint": "skip_next" }),
        RemoteAction::Previous => json!({ "endpoint": "skip_prev" }),
        RemoteAction::Seek => json!({ "endpoint": "seek_to", "value": position_ms }),
        RemoteAction::Shuffle => json!({ "endpoint": "set_shuffling_context", "value": flag }),
        RemoteAction::Repeat => json!({
            "endpoint": "set_options",
            "repeating_context": repeat != "off",
            "repeating_track": repeat == "track",
        }),
        // Volume has its own endpoint, see `set_volume`.
        RemoteAction::Volume => return None,
    };
    Some(json!({ "command": command }))
}

fn play_command(request: &PlayRequest) -> Value {
    let single = request
        .context_uri
        .as_deref()
        .filter(|uri| matches!(crate::util::uri_kind(uri), Some("track" | "episode")));
    let context = match (single, &request.context_uri) {
        (None, Some(uri)) => {
            json!({ "uri": uri, "url": format!("context://{uri}"), "metadata": {} })
        }
        _ => {
            let uris: Vec<&str> = match single {
                Some(uri) => vec![request.offset_uri.as_deref().unwrap_or(uri)],
                None => request.uris.iter().map(String::as_str).collect(),
            };
            let tracks: Vec<Value> = uris.iter().map(|uri| json!({ "uri": uri })).collect();
            json!({ "uri": "", "url": "", "pages": [{ "tracks": tracks }], "metadata": {} })
        }
    };
    let mut skip_to = serde_json::Map::new();
    if single.is_none() {
        if let Some(uri) = &request.offset_uri {
            skip_to.insert("track_uri".into(), json!(uri));
        } else if let Some(index) = request.offset_position {
            skip_to.insert("track_index".into(), json!(index));
        }
    }
    let mut options = json!({
        "license": "on-demand",
        "skip_to": Value::Object(skip_to),
        "player_options_override": {},
    });
    if request.position_ms > 0 {
        options["seek_to"] = json!(request.position_ms);
    }
    json!({
        "endpoint": "play",
        "context": context,
        "play_origin": { "feature_identifier": "spotifast" },
        "options": options,
    })
}

fn failure(error: impl std::fmt::Display) -> crate::api::client::ApiError {
    crate::api::client::ApiError::Network(error.to_string())
}

/// Sends a player command to a restricted device over the session.
pub async fn command(
    session: &Session,
    to: &str,
    body: &Value,
) -> Result<(), crate::api::client::ApiError> {
    let path = format!(
        "/connect-state/v1/player/command/from/{}/to/{to}",
        session.device_id()
    );
    session
        .spclient()
        .request(
            &reqwest::Method::POST,
            &path,
            None,
            Some(body.to_string().as_bytes()),
        )
        .await
        .map(drop)
        .map_err(failure)
}

/// Sets a restricted device's volume over the session.
pub async fn set_volume(
    session: &Session,
    to: &str,
    percent: u8,
) -> Result<(), crate::api::client::ApiError> {
    let path = format!(
        "/connect-state/v1/connect/volume/from/{}/to/{to}",
        session.device_id()
    );
    let volume = u32::from(percent.min(100)) * 65535 / 100;
    session
        .spclient()
        .request(
            &reqwest::Method::PUT,
            &path,
            None,
            Some(json!({ "volume": volume }).to_string().as_bytes()),
        )
        .await
        .map(drop)
        .map_err(failure)
}

/// Moves playback to a restricted device. A Sonos is first handed a token,
/// so it can join Connect when it is idle.
pub async fn transfer(session: &Session, to: &str) -> Result<(), crate::api::client::ApiError> {
    if let Some(speaker) = speaker(to)
        && let Err(error) = sign_in(session, &speaker).await
    {
        log::warn!("Sonos sign-in for {} failed: {error}", speaker.name);
    }
    let spclient = session.spclient();
    // `to` as the source too means "from whichever device is active".
    match spclient.transfer(to, to, None).await {
        Ok(_) => Ok(()),
        Err(first) => {
            log::debug!("Connect transfer from the active device failed: {first}");
            spclient
                .transfer(session.device_id(), to, None)
                .await
                .map(drop)
                .map_err(failure)
        }
    }
}

/// Signs a Sonos in before playback is started on it, so an idle speaker
/// can take the command. Repeated at most every ten minutes.
pub async fn wake(session: &Session, to: &str) {
    static WOKEN: OnceLock<Mutex<HashMap<String, std::time::Instant>>> = OnceLock::new();
    let Some(speaker) = speaker(to) else { return };
    {
        let mut woken = WOKEN
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if woken
            .get(to)
            .is_some_and(|at| at.elapsed() < Duration::from_secs(600))
        {
            return;
        }
        woken.insert(to.to_string(), std::time::Instant::now());
    }
    if let Err(error) = sign_in(session, &speaker).await {
        log::warn!("Sonos sign-in for {} failed: {error}", speaker.name);
    }
}

/// Hands a Sonos a streaming token for its own client id. It answers OK to
/// anything, so the result only reports transport failures.
async fn sign_in(session: &Session, speaker: &Speaker) -> anyhow::Result<()> {
    let http = reqwest::Client::builder()
        // Private LAN address: keep it off any configured proxy.
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let host = match speaker.address {
        IpAddr::V6(address) => format!("[{address}]"),
        IpAddr::V4(address) => address.to_string(),
    };
    let base = format!("http://{host}:{PORT}{ZEROCONF_PATH}");
    let info: Value = http
        .get(format!("{base}?action=getInfo"))
        .send()
        .await?
        .json()
        .await?;
    let client_id = info["clientID"].as_str().unwrap_or_default();
    if client_id.is_empty() {
        anyhow::bail!("the speaker named no client id");
    }
    let token = session
        .token_provider()
        .get_token_with_client_id("streaming", client_id)
        .await?;
    let username = session.username();
    let device_id: String = <sha1::Sha1 as sha1::Digest>::digest(b"Spotifast")
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    // Not used for a token login, but the field must hold a valid key.
    let client_key = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        librespot_core::diffie_hellman::DhLocalKeys::random(&mut rand::rng()).public_key(),
    );
    let form = [
        ("action", "addUser"),
        ("version", "2.9.0"),
        ("tokenType", "accesstoken"),
        ("clientKey", client_key.as_str()),
        ("loginId", username.as_str()),
        ("userName", username.as_str()),
        ("blob", token.access_token.as_str()),
        ("deviceName", "Spotifast"),
        ("deviceId", device_id.as_str()),
        ("clientID", client_id),
    ];
    http.post(&base)
        .form(&form)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver(port: u16, path: &str) -> crate::zeroconf::Receiver {
        crate::zeroconf::Receiver {
            name: "Sonos Move".into(),
            device_id: Some("d0c4".into()),
            address: "192.168.2.47".parse().unwrap(),
            port,
            path: path.into(),
        }
    }

    #[test]
    fn recognises_sonos_receivers_by_their_endpoint() {
        assert!(is_sonos(&receiver(1400, "/spotifyzc")));
        assert!(!is_sonos(&receiver(80, "/spotifyzc")));
        assert!(!is_sonos(&receiver(1400, "/zc")));
    }

    #[test]
    fn a_playlist_starts_at_the_named_song() {
        let request = PlayRequest::context("spotify:playlist:abc").starting_at_index(2);
        let body = command_body(RemoteAction::Play, Some(&request), 0, false, "").unwrap();
        let command = &body["command"];
        assert_eq!(command["endpoint"], "play");
        assert_eq!(command["context"]["uri"], "spotify:playlist:abc");
        assert_eq!(command["context"]["url"], "context://spotify:playlist:abc");
        assert_eq!(command["options"]["skip_to"]["track_index"], 2);
    }

    #[test]
    fn a_lone_track_plays_as_a_one_page_context() {
        let request = PlayRequest::context("spotify:track:t1");
        let body = command_body(RemoteAction::Play, Some(&request), 0, false, "").unwrap();
        let tracks = &body["command"]["context"]["pages"][0]["tracks"];
        assert_eq!(tracks[0]["uri"], "spotify:track:t1");
        assert!(
            body["command"]["options"]["skip_to"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn play_without_a_request_resumes() {
        let body = command_body(RemoteAction::Play, None, 0, false, "").unwrap();
        assert_eq!(body["command"]["endpoint"], "resume");
        assert!(command_body(RemoteAction::Volume, None, 0, false, "").is_none());
    }

    #[test]
    fn commands_find_the_restricted_device_that_plays() {
        let device = Device {
            id: Some("restricted-one".into()),
            is_restricted: true,
            ..Device::default()
        };
        observe_playing(Some(&device));
        assert_eq!(restricted_target(None).as_deref(), Some("restricted-one"));
        assert_eq!(
            restricted_target(Some("restricted-one")).as_deref(),
            Some("restricted-one")
        );
        assert_eq!(restricted_target(Some("someone-else")), None);
    }
}
