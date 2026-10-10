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
//! It asks for an OAuth authorization code rather than librespot's login
//! blob. No credential is sent to its HTTP receiver: the existing session
//! transfers and controls playback through Spotify instead.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Mutex;

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
    playback_seq: Option<u64>,
}

/// Receiver identities and queue writes belong to one signed-in backend.
#[derive(Default)]
pub struct Controller {
    known: Mutex<Known>,
    queue_writes: tokio::sync::Mutex<()>,
}

impl Controller {
    fn known(&self) -> std::sync::MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Remembers the Sonos players among resolved receivers.
    pub fn remember(&self, receivers: &[crate::zeroconf::Receiver]) {
        let mut known = self.known();
        known.speakers = receivers
            .iter()
            .filter(|receiver| is_sonos(receiver))
            .filter_map(|receiver| {
                let id = receiver.device_id.as_ref()?;
                Some((
                    id.clone(),
                    Speaker {
                        device_id: id.clone(),
                        name: receiver.name.clone(),
                        address: receiver.address,
                    },
                ))
            })
            .collect();
    }

    /// The Sonos player behind a Connect device id, if it is one.
    pub fn speaker(&self, device_id: &str) -> Option<Speaker> {
        self.known().speakers.get(device_id).cloned()
    }

    /// Notes which devices Spotify reports as restricted.
    pub fn observe_devices(&self, devices: &[Device]) {
        let mut known = self.known();
        for device in devices.iter().filter(|device| device.is_restricted) {
            if let Some(id) = &device.id {
                known.restricted.insert(id.clone());
            }
        }
    }

    /// Updates the active restricted device, ignoring older playback polls.
    pub fn observe_playing(&self, seq: u64, device: Option<&Device>) {
        let mut known = self.known();
        if known.playback_seq.is_some_and(|latest| seq < latest) {
            return;
        }
        known.playback_seq = Some(seq);
        known.active_restricted = device
            .filter(|device| device.is_restricted)
            .and_then(|device| device.id.clone());
        if let Some(id) = known.active_restricted.clone() {
            known.restricted.insert(id);
        }
    }

    /// An explicit target takes precedence over the last active device.
    pub fn restricted_target(&self, device_id: Option<&str>) -> Option<String> {
        let known = self.known();
        match device_id {
            Some(id) if known.restricted.contains(id) || known.speakers.contains_key(id) => {
                Some(id.to_string())
            }
            Some(_) => None,
            None => known.active_restricted.clone(),
        }
    }
}

/// Whether a ZeroConf receiver is a Sonos player.
pub fn is_sonos(receiver: &crate::zeroconf::Receiver) -> bool {
    receiver.port == PORT
        && receiver.path.trim_matches('/') == ZEROCONF_PATH.trim_start_matches('/')
}

/// A device row for a Sonos receiver, which Spotify does not list while idle.
pub fn device_row(receiver: &crate::zeroconf::Receiver, active: bool) -> Option<Device> {
    if !is_sonos(receiver) {
        return None;
    }
    Some(Device {
        id: Some(receiver.device_id.clone().filter(|id| !id.is_empty())?),
        name: receiver.name.clone(),
        is_active: active,
        is_restricted: false,
        volume_percent: None,
        supports_volume: Some(true),
        kind: "speaker".into(),
    })
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

/// Keeps a Spotify context intact or builds an ordered page for explicit tracks.
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

impl Controller {
    /// Moves playback to a restricted device through Spotify Connect.
    pub async fn transfer(
        &self,
        session: &Session,
        to: &str,
    ) -> Result<(), crate::api::client::ApiError> {
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

    /// Serializes complete batches with individual additions, as the Web API does.
    pub async fn add_to_queue(
        &self,
        session: &Session,
        to: &str,
        uris: &[String],
    ) -> (usize, Result<(), crate::api::client::ApiError>) {
        self.append_many(uris, |uri| async move {
            command(
                session,
                to,
                &json!({
                    "command": { "endpoint": "add_to_queue", "track": { "uri": uri } }
                }),
            )
            .await
        })
        .await
    }

    /// Locks the whole batch and reports only the prefix accepted before a failure.
    async fn append_many<F, Fut>(
        &self,
        uris: &[String],
        mut append: F,
    ) -> (usize, Result<(), crate::api::client::ApiError>)
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), crate::api::client::ApiError>>,
    {
        let _write = self.queue_writes.lock().await;
        for (added, uri) in uris.iter().enumerate() {
            if let Err(error) = append(uri.clone()).await {
                return (added, Err(error));
            }
        }
        (uris.len(), Ok(()))
    }
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
        assert!(is_sonos(&receiver(1400, "spotifyzc/")));
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
        let controller = Controller::default();
        let device = Device {
            id: Some("restricted-one".into()),
            is_restricted: true,
            ..Device::default()
        };
        controller.observe_playing(2, Some(&device));
        assert_eq!(
            controller.restricted_target(None).as_deref(),
            Some("restricted-one")
        );
        assert_eq!(
            controller
                .restricted_target(Some("restricted-one"))
                .as_deref(),
            Some("restricted-one")
        );
        assert_eq!(controller.restricted_target(Some("someone-else")), None);
        controller.observe_playing(1, None);
        assert_eq!(
            controller.restricted_target(None).as_deref(),
            Some("restricted-one")
        );
        controller.observe_playing(3, None);
        assert_eq!(controller.restricted_target(None), None);
    }

    #[test]
    fn discovery_updates_addresses_and_only_offers_resolved_identities() {
        let controller = Controller::default();
        let mut found = receiver(1400, "/spotifyzc");
        controller.remember(std::slice::from_ref(&found));
        found.address = "192.168.2.48".parse().unwrap();
        controller.remember(std::slice::from_ref(&found));
        assert_eq!(controller.speaker("d0c4").unwrap().address, found.address);
        assert_eq!(device_row(&found, false).unwrap().id, found.device_id);
        found.device_id = None;
        assert!(device_row(&found, false).is_none());
        controller.remember(&[]);
        assert_eq!(controller.restricted_target(Some("d0c4")), None);
    }

    #[test]
    fn filtered_tracks_preserve_duplicates_offsets_and_resume_position() {
        let mut request = PlayRequest::tracks(vec![
            "spotify:track:a".into(),
            "spotify:track:b".into(),
            "spotify:track:a".into(),
        ]);
        request.offset_position = Some(2);
        request.position_ms = 12_345;
        let body = command_body(RemoteAction::Play, Some(&request), 0, false, "").unwrap();
        let command = &body["command"];
        assert_eq!(
            command["context"]["pages"][0]["tracks"],
            json!([
                {"uri": "spotify:track:a"}, {"uri": "spotify:track:b"}, {"uri": "spotify:track:a"}
            ])
        );
        assert_eq!(command["options"]["skip_to"]["track_index"], 2);
        assert_eq!(command["options"]["seek_to"], 12_345);
    }

    #[tokio::test]
    async fn queue_batches_keep_order_and_duplicates_and_stop_at_a_failed_write() {
        let controller = Controller::default();
        let uris = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        for fail in [false, true] {
            let mut written = Vec::new();
            let (added, result) = controller
                .append_many(&uris, |uri| {
                    written.push(uri);
                    std::future::ready(if fail && written.len() == 2 {
                        Err(crate::api::client::ApiError::Network(
                            "test-only refusal".into(),
                        ))
                    } else {
                        Ok(())
                    })
                })
                .await;
            assert_eq!(added, if fail { 1 } else { 3 });
            assert_eq!(result.is_err(), fail);
            assert_eq!(
                written,
                if fail {
                    uris[..2].to_vec()
                } else {
                    uris.clone()
                }
            );
        }
    }

    #[tokio::test]
    async fn an_individual_queue_add_waits_for_the_whole_album() {
        let controller = Controller::default();
        let written = Mutex::new(Vec::new());
        let first = tokio::sync::Notify::new();
        let album = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        let later = vec!["later".to_string()];
        let (batch, single) = tokio::join!(
            controller.append_many(&album, |uri| {
                written.lock().unwrap().push(uri);
                first.notify_one();
                async {
                    tokio::task::yield_now().await;
                    Ok(())
                }
            }),
            async {
                first.notified().await;
                controller
                    .append_many(&later, |uri| {
                        written.lock().unwrap().push(uri);
                        std::future::ready(Ok(()))
                    })
                    .await
            }
        );
        assert_eq!(batch.0, 3);
        batch.1.unwrap();
        assert_eq!(single.0, 1);
        single.1.unwrap();
        assert_eq!(*written.lock().unwrap(), ["a", "b", "a", "later"]);
    }
}
