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
//! blob. If Spotify cannot find an idle player, the existing session exchanges
//! its token for a Sonos authorization code before retrying the transfer. Only
//! that code goes to the receiver; the access token stays with Spotify.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use librespot_core::http_client::HttpClientError;
use librespot_core::session::Session;
use serde_json::{Value, json};

use crate::api::client::PlayRequest;
use crate::api::models::Device;
use crate::backend::RemoteAction;

const PORT: u16 = 1400;
const ZEROCONF_PATH: &str = "/spotifyzc";
/// The audience used by Sonos's Spotify integration, never supplied by the LAN.
const SONOS_CLIENT_ID: &str = "9b377073ea334637b1406f329ce005de";
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(20);

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

/// librespot also calls redirects and 410 `NotFound`. Only 404 activates Sonos.
fn missing_device(error: &librespot_core::Error) -> bool {
    matches!(
        error.error.downcast_ref::<HttpClientError>(),
        Some(HttpClientError::StatusCode(http::StatusCode::NOT_FOUND))
    )
}

/// Validate the fresh probe before obtaining any account-bound code.
fn validate_activation(info: &crate::zeroconf::Info, device_id: &str) -> Result<()> {
    if info.device_id != device_id || info.token_type != "authorization_code" {
        bail!(
            "The Sonos receiver changed or does not accept authorization codes. Refresh the device list and try again."
        );
    }
    Ok(())
}

/// Exchange only with Spotify. The receiver cannot choose the audience or URL.
fn exchange_form(client_id: &str, access_token: &str) -> String {
    [
        ("audience", SONOS_CLIENT_ID),
        ("client_id", client_id),
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:token-exchange",
        ),
        (
            "requested_token_type",
            "urn:spotify:params:oauth:authorization_code",
        ),
        ("resource", "urn:spotify:resources:connect"),
        ("scope", "streaming"),
        (
            "subject_token_type",
            "urn:ietf:params:oauth:token-type:access_token",
        ),
        ("subject_token", access_token),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}={}", urlencoding::encode(value)))
    .collect::<Vec<_>>()
    .join("&")
}

/// This reply contains a credential. Never include its body in errors or logs.
fn authorization_code(body: &[u8]) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Reply {
        access_token: String,
    }
    let reply: Reply = serde_json::from_slice(body)
        .map_err(|_| anyhow::anyhow!("Spotify did not return a Sonos authorization code"))?;
    if reply.access_token.is_empty() {
        bail!("Spotify returned an empty Sonos authorization code");
    }
    Ok(reply.access_token)
}

/// LAN replies are untrusted. Bound them and keep their contents out of errors.
async fn receiver_reply<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T> {
    if !response.status().is_success() {
        bail!("Sonos refused the request (HTTP {})", response.status());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("Sonos reply was interrupted")?
    {
        if body.len() + chunk.len() > 64 * 1024 {
            bail!("Sonos returned an oversized reply");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| anyhow::anyhow!("Sonos returned an unreadable reply"))
}

/// Sign in an idle Sonos without handing it the session's reusable credential.
async fn activate(session: &Session, speaker: &Speaker) -> Result<()> {
    let http = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(6))
        .build()?;
    let endpoint = format!(
        "http://{}{ZEROCONF_PATH}",
        std::net::SocketAddr::new(speaker.address, PORT)
    );
    activate_with(
        &http,
        &endpoint,
        &speaker.device_id,
        &session.username(),
        session.device_id(),
        || exchange_code(session),
    )
    .await
}

async fn exchange_code(session: &Session) -> Result<String> {
    let token = session
        .login5()
        .auth_token()
        .await
        .context("Spotify could not authorise Sonos activation")?;
    let request = http::Request::builder()
        .method(http::Method::POST)
        .uri("https://accounts.spotify.com/api/token")
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(exchange_form(&session.client_id(), &token.access_token).into())?;
    let body = session.http_client().request_body(request).await
        .context("Spotify refused Sonos activation. Start the speaker in Spotify once, then select it again in Spotifast.")?;
    authorization_code(&body)
}

/// The exchange runs only after checking the probe. Tests supply a dummy code.
async fn activate_with<F, Fut>(
    http: &reqwest::Client,
    endpoint: &str,
    device_id: &str,
    username: &str,
    origin: &str,
    exchange: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let info: crate::zeroconf::Info = receiver_reply(
        http.get(endpoint)
            .query(&[("action", "getInfo")])
            .send()
            .await
            .context("Sonos did not answer the activation probe")?,
    )
    .await?;
    validate_activation(&info, device_id)?;
    let code = exchange().await?;
    let form = [
        ("action", "addUser"),
        (
            "version",
            if info.version.is_empty() {
                "2.9.0"
            } else {
                &info.version
            },
        ),
        ("tokenType", "authorization_code"),
        ("clientKey", ""),
        ("loginId", username),
        ("userName", username),
        ("blob", code.as_str()),
        ("deviceName", "Spotifast"),
        ("deviceId", origin),
    ];
    #[derive(serde::Deserialize)]
    struct Reply {
        status: i64,
    }
    let reply: Reply = receiver_reply(
        http.post(endpoint)
            .form(&form)
            .send()
            .await
            .context("Sonos did not accept the activation request")?,
    )
    .await?;
    if reply.status != 101 {
        bail!(
            "Sonos refused activation (status {}). Start the speaker in Spotify once, then select it again in Spotifast.",
            reply.status
        );
    }
    Ok(())
}

/// Retain both transfer routes; only a missing known Sonos needs activation.
async fn transfer_with_activation<T, TF, A, AF>(
    mut transfer: T,
    activation: Option<A>,
) -> Result<(), crate::api::client::ApiError>
where
    T: FnMut(bool) -> TF,
    TF: std::future::Future<Output = Result<(), librespot_core::Error>>,
    A: FnOnce() -> AF,
    AF: std::future::Future<Output = Result<()>>,
{
    let first = match transfer(false).await {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    match transfer(true).await {
        Ok(()) => return Ok(()),
        Err(error)
            if !missing_device(&first) || !missing_device(&error) || activation.is_none() =>
        {
            return Err(failure(error));
        }
        Err(_) => {}
    }
    tokio::time::timeout(ACTIVATION_TIMEOUT, activation.unwrap()())
        .await
        .map_err(|_| {
            failure(
                "Sonos activation timed out. Start the speaker in Spotify once, then try again.",
            )
        })?
        .map_err(|error| failure(format!("{error:#}")))?;
    // addUser can finish before Spotify publishes the newly signed-in device.
    // Retain the session source too when there is no active source to transfer.
    for attempt in 0..5 {
        let result = match transfer(false).await {
            Err(error) if missing_device(&error) => transfer(true).await,
            result => result,
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) if missing_device(&error) && attempt < 4 => {
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
            Err(error) => return Err(failure(error)),
        }
    }
    unreachable!()
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
        let speaker = self.speaker(to);
        transfer_with_activation(
            |from_session| async move {
                // `to` as the source too means "from whichever device is active".
                session
                    .spclient()
                    .transfer(
                        if from_session {
                            session.device_id()
                        } else {
                            to
                        },
                        to,
                        None,
                    )
                    .await
                    .map(drop)
            },
            speaker
                .as_ref()
                .map(|speaker| || activate(session, speaker)),
        )
        .await
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
    use librespot_core::error::ErrorKind;

    fn not_found() -> librespot_core::Error {
        HttpClientError::StatusCode(http::StatusCode::NOT_FOUND).into()
    }

    #[test]
    fn only_an_http_404_counts_as_a_missing_device() {
        for status in [404, 410, 301, 307, 308, 403, 429, 500, 503] {
            let error =
                HttpClientError::StatusCode(http::StatusCode::from_u16(status).unwrap()).into();
            assert_eq!(missing_device(&error), status == 404);
        }
        assert!(!missing_device(&librespot_core::Error::not_found(
            "non-HTTP error"
        )));
    }

    #[test]
    fn activation_requires_the_selected_identity_and_authorization_code_protocol() {
        for (id, token_type, valid) in [
            ("selected", "authorization_code", true),
            ("other", "authorization_code", false),
            ("selected", "default", false),
            ("selected", "accesstoken", false),
            ("selected", "", false),
        ] {
            let info =
                serde_json::from_value(json!({ "deviceID": id, "tokenType": token_type })).unwrap();
            assert_eq!(validate_activation(&info, "selected").is_ok(), valid);
        }
    }

    #[test]
    fn exchange_is_limited_to_sonos_streaming_and_encodes_the_subject() {
        let body = exchange_form("our-client", "dummy&token=1");
        let pairs: HashMap<String, String> = body
            .split('&')
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap();
                (key.into(), urlencoding::decode(value).unwrap().into_owned())
            })
            .collect();
        assert_eq!(pairs.len(), 8);
        assert_eq!(pairs["audience"], SONOS_CLIENT_ID);
        assert_eq!(pairs["client_id"], "our-client");
        assert_eq!(pairs["subject_token"], "dummy&token=1");
        assert_eq!(pairs["scope"], "streaming");
        assert_eq!(pairs["resource"], "urn:spotify:resources:connect");
        assert_eq!(
            pairs["requested_token_type"],
            "urn:spotify:params:oauth:authorization_code"
        );
    }

    #[test]
    fn exchange_errors_do_not_disclose_response_contents() {
        assert_eq!(
            authorization_code(br#"{"access_token":"dummy-code"}"#).unwrap(),
            "dummy-code"
        );
        for body in [
            br#"{"access_token":""}"#.as_slice(),
            br#"{"error":"secret-body"}"#,
            br#"{"access_token":123,"secret-body":true}"#,
        ] {
            let error = authorization_code(body).unwrap_err().to_string();
            assert!(!error.contains("secret-body"));
            assert!(!error.contains("123"));
        }
    }

    #[tokio::test]
    async fn transfers_activate_only_a_missing_known_sonos_and_preserve_fallback_order() {
        use std::cell::RefCell;
        // active route, session route, activation allowed, expected calls
        for (first, second, allowed, expected) in [
            (None, None, true, vec!["active"]),
            (
                Some(ErrorKind::NotFound),
                None,
                true,
                vec!["active", "session"],
            ),
            (
                Some(ErrorKind::NotFound),
                Some(ErrorKind::NotFound),
                true,
                vec!["active", "session", "activate", "active"],
            ),
            (
                Some(ErrorKind::NotFound),
                Some(ErrorKind::NotFound),
                false,
                vec!["active", "session"],
            ),
            (
                Some(ErrorKind::NotFound),
                Some(ErrorKind::PermissionDenied),
                true,
                vec!["active", "session"],
            ),
            (
                Some(ErrorKind::Unavailable),
                Some(ErrorKind::Unavailable),
                true,
                vec!["active", "session"],
            ),
        ] {
            let calls = RefCell::new(Vec::new());
            let mut failures = [first, second].into_iter();
            let result = transfer_with_activation(
                |from_session| {
                    calls
                        .borrow_mut()
                        .push(if from_session { "session" } else { "active" });
                    std::future::ready(match failures.next().flatten() {
                        None => Ok(()),
                        Some(ErrorKind::NotFound) => Err(not_found()),
                        Some(kind) => {
                            Err(librespot_core::Error::new(kind, "test transfer failure"))
                        }
                    })
                },
                allowed.then_some(|| {
                    calls.borrow_mut().push("activate");
                    std::future::ready(Ok(()))
                }),
            )
            .await;
            assert_eq!(*calls.borrow(), expected);
            assert_eq!(
                result.is_ok(),
                second.is_none()
                    || first.is_none()
                    || (allowed && second == Some(ErrorKind::NotFound))
            );
        }
    }

    #[tokio::test]
    async fn activation_failure_stops_transfers_and_keeps_the_actionable_error() {
        let mut transfers = 0;
        let result = transfer_with_activation(
            |_| {
                transfers += 1;
                std::future::ready(Err(not_found()))
            },
            Some(|| std::future::ready(Err(anyhow::anyhow!("activation refused")))),
        )
        .await;
        assert_eq!(transfers, 2);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("activation refused")
        );
    }

    #[tokio::test]
    async fn after_activation_a_non_missing_transfer_failure_is_not_retried() {
        let mut transfers = 0;
        let result = transfer_with_activation(
            |_| {
                transfers += 1;
                std::future::ready(Err(if transfers <= 2 {
                    not_found()
                } else {
                    librespot_core::Error::permission_denied("retry refused")
                }))
            },
            Some(|| std::future::ready(Ok(()))),
        )
        .await;
        assert_eq!(transfers, 3);
        assert!(result.unwrap_err().to_string().contains("retry refused"));
    }

    #[tokio::test]
    async fn registration_retries_are_bounded_and_keep_the_final_failure() {
        let mut transfers = 0;
        let mut activations = 0;
        let result = transfer_with_activation(
            |_| {
                transfers += 1;
                std::future::ready(Err(not_found()))
            },
            Some(|| {
                activations += 1;
                std::future::ready(Ok(()))
            }),
        )
        .await;
        assert_eq!(transfers, 12);
        assert_eq!(activations, 1);
        assert!(result.unwrap_err().to_string().contains("404"));
    }

    #[tokio::test]
    async fn activation_retries_the_session_source_when_no_device_is_active() {
        let mut sources = Vec::new();
        transfer_with_activation(
            |from_session| {
                sources.push(from_session);
                std::future::ready(if sources.len() == 4 && from_session {
                    Ok(())
                } else {
                    Err(not_found())
                })
            },
            Some(|| std::future::ready(Ok(()))),
        )
        .await
        .unwrap();
        assert_eq!(sources, [false, true, false, true]);
    }

    async fn mock_receiver(
        responses: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/spotifyzc", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(3), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0; 1024];
                    let length = stream.read(&mut chunk).await.unwrap();
                    assert!(length > 0);
                    request.extend_from_slice(&chunk[..length]);
                    if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        let length: usize = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length: "))
                            .unwrap_or("0")
                            .parse()
                            .unwrap();
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(request).unwrap());
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (url, server)
    }

    fn response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test]
    async fn receiver_activation_probes_before_exchange_and_sends_only_the_code() {
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for status in [101, 102] {
            let (endpoint, server) = mock_receiver(vec![
                response(r#"{"deviceID":"selected","tokenType":"authorization_code","version":"2.10.0"}"#),
                response(&format!(r#"{{"status":{status},"statusString":"secret-receiver-body"}}"#)),
            ]).await;
            let exchanges = std::cell::Cell::new(0);
            let result = activate_with(
                &http,
                &endpoint,
                "selected",
                "dummy-user",
                "dummy-origin",
                || {
                    exchanges.set(exchanges.get() + 1);
                    std::future::ready(Ok("dummy-code&with=symbols".into()))
                },
            )
            .await;
            assert_eq!(result.is_ok(), status == 101);
            if let Err(error) = result {
                assert!(!error.to_string().contains("secret-receiver-body"));
            }
            assert_eq!(exchanges.get(), 1);
            let requests = server.await.unwrap();
            assert!(requests[0].starts_with("GET /spotifyzc?action=getInfo "));
            assert!(requests[1].starts_with("POST /spotifyzc "));
            let form = requests[1].split_once("\r\n\r\n").unwrap().1;
            assert!(form.contains("blob=dummy-code%26with%3Dsymbols"));
            assert!(form.contains("tokenType=authorization_code"));
            assert!(form.contains("deviceId=dummy-origin"));
            assert!(form.contains("loginId=dummy-user"));
            assert!(!form.contains("subject_token"));
        }
    }

    #[tokio::test]
    async fn a_changed_unsupported_redirected_or_oversized_probe_never_exchanges_a_token() {
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for reply in [
            response(r#"{"deviceID":"other","tokenType":"authorization_code"}"#),
            response(r#"{"deviceID":"selected","tokenType":"accesstoken"}"#),
            response(r#"{"deviceID":"selected"}"#),
            response(&"x".repeat(65 * 1024)),
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/steal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        ] {
            let (endpoint, server) = mock_receiver(vec![reply]).await;
            let result = activate_with(&http, &endpoint, "selected", "dummy-user", "dummy-origin", || async { panic!("must validate before exchange") }).await;
            assert!(result.is_err());
            assert_eq!(server.await.unwrap().len(), 1);
        }
    }

    fn receiver(port: u16, path: &str) -> crate::zeroconf::Receiver {
        crate::zeroconf::Receiver {
            name: "Sonos Move".into(),
            device_id: Some("d0c4".into()),
            address: "192.168.2.47".parse().unwrap(),
            port,
            path: path.into(),
            cast: false,
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
