//! Only commands documented by Spotify's Soloist WebSocket API. Context
//! loads retain their offsets through Spotifast's existing Spotify API path.
use crate::{
    api::{client::ApiError, models::*},
    backend::{ApiRequest, ApiResponse, RemoteAction},
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
pub async fn connect(port: u16) -> Result<Socket> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(4 * 1024 * 1024))
        .max_frame_size(Some(4 * 1024 * 1024));
    let (socket, _) = tokio_tungstenite::connect_async_with_config(
        format!("ws://127.0.0.1:{port}"),
        Some(config),
        false,
    )
    .await?;
    Ok(socket)
}
pub async fn send(socket: &mut Socket, mut body: Value) -> Result<()> {
    body["type"] = json!("command");
    socket.send(Message::Text(body.to_string().into())).await?;
    Ok(())
}
pub async fn send_reconnecting(socket: &mut Socket, port: u16, body: Value) -> Result<bool> {
    if send(socket, body.clone()).await.is_ok() {
        return Ok(false);
    }
    *socket = tokio::time::timeout(Duration::from_secs(5), connect(port))
        .await
        .context("Soloist's local API did not reconnect in time")??;
    send(socket, body).await?;
    Ok(true)
}
pub async fn receive(socket: &mut Socket) -> Result<Value> {
    loop {
        match socket.next().await.context("Soloist disconnected")?? {
            Message::Text(text) => return Ok(serde_json::from_str(&text)?),
            Message::Close(_) => bail!("Soloist disconnected"),
            Message::Ping(_) => socket.flush().await?,
            _ => {}
        }
    }
}

pub async fn receive_reconnecting(socket: &mut Socket, port: u16) -> Result<(Option<Value>, bool)> {
    match receive(socket).await {
        Ok(message) => Ok((Some(message), false)),
        Err(_) => {
            *socket = tokio::time::timeout(Duration::from_secs(5), connect(port))
                .await
                .context("Soloist's local API did not reconnect in time")??;
            Ok((None, true))
        }
    }
}

#[derive(Default)]
struct Endpoint {
    port: Option<u16>,
    device: Option<Device>,
    active: bool,
}
#[derive(Clone, Default)]
pub struct Client {
    endpoint: Arc<RwLock<Endpoint>>,
    /// Soloist acknowledgements have command names, not request IDs.
    serial: Arc<tokio::sync::Mutex<()>>,
}
impl Client {
    pub fn bind(&self, device: Option<Device>) {
        self.endpoint
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .device = device;
    }
    pub fn listening(&self, port: Option<u16>) {
        let mut state = self.endpoint.write().unwrap_or_else(|e| e.into_inner());
        state.port = port;
        state.active = false;
    }
    pub fn active(&self, active: bool) {
        self.endpoint
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .active = active;
    }
    pub fn clear(&self) {
        *self.endpoint.write().unwrap_or_else(|e| e.into_inner()) = Endpoint::default();
    }

    pub async fn dispatch(&self, request: &ApiRequest) -> Option<ApiResponse> {
        let (port, device, active) = {
            let state = self.endpoint.read().unwrap_or_else(|e| e.into_inner());
            (state.port?, state.device.clone()?, state.active)
        };
        match request {
            ApiRequest::PlaybackState { seq } if active => {
                let state = self
                    .rpc(port, json!({"command":"get_state"}), Some("playback_state"))
                    .await;
                if state
                    .as_ref()
                    .is_ok_and(|state| state["is_active"] == false)
                {
                    self.active(false);
                    return None;
                }
                let result = state
                    .and_then(|state| playback(&state, device))
                    .map(Some)
                    .map_err(failure);
                Some(ApiResponse::PlaybackState { seq: *seq, result })
            }
            ApiRequest::Queue { seq } if active => {
                let state = self
                    .rpc(port, json!({"command":"get_state"}), Some("playback_state"))
                    .await;
                if state
                    .as_ref()
                    .is_ok_and(|state| state["is_active"] == false)
                {
                    self.active(false);
                    return None;
                }
                let result = async {
                    let state = state?;
                    let queue = self
                        .rpc(
                            port,
                            json!({"command":"get_queue", "limit":0}),
                            Some("queue_changed"),
                        )
                        .await?;
                    Ok(Queue {
                        currently_playing: item(&state["item"]),
                        queue: queue["upcoming"]
                            .as_array()
                            .context("Missing Soloist queue")?
                            .iter()
                            .filter_map(|entry| item(&entry["item"]))
                            .collect(),
                    })
                }
                .await
                .map_err(failure);
                Some(ApiResponse::Queue { seq: *seq, result })
            }
            ApiRequest::Remote {
                action, device_id, ..
            } if device_id.is_some() && device_id == &device.id => {
                let commands = commands(request)?;
                let result = async {
                    let _guard = self.serial.lock().await;
                    for command in commands {
                        self.rpc_unlocked(port, command, None).await?;
                    }
                    Ok(())
                }
                .await
                .map_err(failure);
                Some(ApiResponse::Remote {
                    action: *action,
                    result,
                })
            }
            // Adding remains on the existing serialized queue writer, so an
            // album and a single addition cannot overtake one another.
            _ => None,
        }
    }
    async fn rpc(&self, port: u16, body: Value, response_type: Option<&str>) -> Result<Value> {
        let _guard = self.serial.lock().await;
        self.rpc_unlocked(port, body, response_type).await
    }
    async fn rpc_unlocked(
        &self,
        port: u16,
        body: Value,
        response_type: Option<&str>,
    ) -> Result<Value> {
        if self.endpoint.read().unwrap_or_else(|e| e.into_inner()).port != Some(port) {
            bail!("Soloist restarted before this command could be sent");
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut socket = connect(port).await?;
            send(&mut socket, body.clone()).await?;
            loop {
                let response = receive(&mut socket).await?;
                let kind = response["type"].as_str().unwrap_or_default();
                if kind == "error" {
                    bail!("Soloist refused this command. Check its connection and try again.");
                }
                if response_type == Some(kind)
                    || (response_type.is_none()
                        && kind == "command_result"
                        && response["command"] == body["command"])
                {
                    return Ok(response);
                }
            }
        })
        .await
        .context("Soloist did not answer in time")?
    }
}
fn failure(error: anyhow::Error) -> ApiError {
    ApiError::Network(error.to_string())
}

fn commands(request: &ApiRequest) -> Option<Vec<Value>> {
    let ApiRequest::Remote {
        action,
        play,
        position_ms,
        percent,
        flag,
        repeat,
        ..
    } = request
    else {
        return None;
    };
    let command = match action {
        RemoteAction::Play if play.is_none() => json!({"command":"play"}),
        RemoteAction::Play => return None, // Preserve context, ordering and selected occurrence.
        RemoteAction::Pause => json!({"command":"pause"}),
        RemoteAction::Next => json!({"command":"skip_next"}),
        RemoteAction::Previous => json!({"command":"skip_prev"}),
        RemoteAction::Seek => json!({"command":"seek", "position_ms":position_ms}),
        RemoteAction::Volume { .. } => json!({"command":"set_volume", "volume":percent.min(&100)}),
        RemoteAction::Shuffle => json!({"command":"set_shuffle", "enabled":flag}),
        RemoteAction::Repeat if repeat == "track" => {
            return Some(vec![
                json!({"command":"set_repeat_context", "enabled":false}),
                json!({"command":"set_repeat_track", "enabled":true}),
            ]);
        }
        RemoteAction::Repeat => {
            return Some(vec![
                json!({"command":"set_repeat_track", "enabled":repeat == "track"}),
                json!({"command":"set_repeat_context", "enabled":repeat == "context"}),
            ]);
        }
    };
    Some(vec![command])
}
fn name(entity: &Value) -> String {
    entity["decorations"]["identity"]["name"]
        .as_str()
        .unwrap_or_default()
        .into()
}
fn uri(entity: &Value) -> String {
    entity["uri"].as_str().unwrap_or_default().into()
}
fn id(entity: &Value) -> Option<String> {
    uri(entity)
        .rsplit(':')
        .next()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}
fn images(entity: &Value) -> Vec<Image> {
    entity["decorations"]["visual_identity"]["cover"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|image| {
            let url = image["url"].as_str()?;
            let width = match image["size"].as_str() {
                Some("small") => 64,
                Some("default") => 300,
                Some("large") => 640,
                _ => 1280,
            };
            Some(Image {
                url: url.into(),
                width: Some(width),
                height: Some(width),
            })
        })
        .collect()
}
fn item(entity: &Value) -> Option<PlayableItem> {
    let decorations = &entity["decorations"];
    let duration_ms = decorations["playback"]["duration_ms"]
        .as_u64()
        .and_then(|ms| u32::try_from(ms).ok())
        .unwrap_or(0);
    let explicit = decorations["playback"]["content_ratings"]
        .as_array()
        .is_some_and(|ratings| ratings.iter().any(|rating| rating == "explicit"));
    match entity["entity_type"].as_str()? {
        "track" => {
            let parent = &decorations["parent"]["entity"];
            let artists = decorations["creators"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|creator| {
                    let entity = &creator["entity"];
                    ArtistRef {
                        id: id(entity),
                        name: name(entity),
                        uri: Some(uri(entity)),
                    }
                })
                .collect();
            Some(PlayableItem::Track(Track {
                id: id(entity),
                uri: uri(entity),
                name: name(entity),
                duration_ms,
                explicit,
                artists,
                album: Some(Album {
                    id: id(parent).unwrap_or_default(),
                    uri: uri(parent),
                    name: name(parent),
                    images: images(entity),
                    ..Default::default()
                }),
                ..Default::default()
            }))
        }
        "episode" => Some(PlayableItem::Episode(Episode {
            id: id(entity).unwrap_or_default(),
            uri: uri(entity),
            name: name(entity),
            duration_ms,
            explicit,
            images: images(entity),
            ..Default::default()
        })),
        _ => None,
    }
}
fn playback(state: &Value, mut device: Device) -> Result<PlaybackState> {
    let active = state["is_active"]
        .as_bool()
        .context("Missing Soloist activity state")?;
    device.is_active = active;
    device.volume_percent = state["volume"].as_u64().map(|v| v.min(100) as u8);
    let context = &state["context"];
    let position = &state["position"];
    let position_ms = position["position_ms"].as_u64().unwrap_or(0);
    let at = position["timestamp_ms"].as_u64().unwrap_or(0);
    let speed = position["speed"].as_f64().unwrap_or(0.0).clamp(0.0, 4.0);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64;
    let elapsed = if at == 0 {
        0
    } else {
        now_ms.saturating_sub(at)
    };
    Ok(PlaybackState {
        device: Some(device),
        item: item(&state["item"]),
        is_playing: active && state["status"] == "playing",
        progress_ms: Some(
            position_ms
                .saturating_add((elapsed as f64 * speed) as u64)
                .min(u32::MAX as u64) as u32,
        ),
        context: context["uri"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|uri| crate::api::models::Context {
                uri: uri.into(),
                kind: context["entity_type"].as_str().unwrap_or_default().into(),
            }),
        repeat_state: state["options"]["repeat"].as_str().unwrap_or("off").into(),
        shuffle_state: state["options"]["shuffle"].as_bool().unwrap_or(false),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn documented_state_keeps_context_and_metadata() {
        let state = json!({"is_active":true,"status":"playing","volume":52,"position":{"position_ms":2300,"speed":0},"context":{"uri":"spotify:playlist:p","entity_type":"playlist"},"options":{"shuffle":true,"repeat":"track"},"item":{"uri":"spotify:track:t","entity_type":"track","decorations":{"identity":{"name":"Song"},"playback":{"duration_ms":123000},"creators":[{"entity":{"uri":"spotify:artist:a","decorations":{"identity":{"name":"Artist"}}}}]}}});
        let got = playback(
            &state,
            Device {
                id: Some("own".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(got.progress_ms, Some(2300));
        assert_eq!(got.context.unwrap().uri, "spotify:playlist:p");
        assert_eq!(got.item.unwrap().name(), "Song");
        assert_eq!(got.repeat_state, "track");
        assert!(got.shuffle_state);
    }
    #[tokio::test]
    async fn unbound_or_inactive_player_does_not_replace_remote_state() {
        let client = Client::default();
        client.listening(Some(12345));
        client.active(true);
        assert!(
            client
                .dispatch(&ApiRequest::PlaybackState { seq: 1 })
                .await
                .is_none()
        );
        client.bind(Some(Device::default()));
        client.active(false);
        assert!(
            client
                .dispatch(&ApiRequest::PlaybackState { seq: 1 })
                .await
                .is_none()
        );
    }
    fn remote(action: RemoteAction) -> ApiRequest {
        ApiRequest::Remote {
            action,
            device_id: Some("soloist".into()),
            play: None,
            position_ms: 0,
            percent: 50,
            flag: false,
            repeat: "off".into(),
        }
    }
    #[tokio::test]
    async fn acknowledgements_are_correlated_and_remote_errors_are_redacted() {
        for reject in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let message = socket.next().await.unwrap().unwrap();
                let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                assert_eq!(command["type"], "command");
                assert_eq!(command["command"], "pause");
                socket
                    .send(Message::Text(
                        json!({"type":"command_result", "command":"play"})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                let result = if reject {
                    json!({"type":"error", "message":"private-server-detail"})
                } else {
                    json!({"type":"command_result", "command":"pause"})
                };
                socket
                    .send(Message::Text(result.to_string().into()))
                    .await
                    .unwrap();
            });
            let client = Client::default();
            client.listening(Some(port));
            client.bind(Some(Device {
                id: Some("soloist".into()),
                ..Default::default()
            }));
            let Some(ApiResponse::Remote { result, .. }) =
                client.dispatch(&remote(RemoteAction::Pause)).await
            else {
                panic!("command did not reach Soloist");
            };
            assert_eq!(result.is_err(), reject);
            if let Err(error) = result {
                assert!(!error.to_string().contains("private-server-detail"));
            }
            server.await.unwrap();
        }
    }
    #[tokio::test]
    async fn event_connection_recovers_after_soloist_drops_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream)
                .await
                .unwrap()
                .close(None)
                .await
                .unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    json!({"type":"auth_state","logged_in":false})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let mut socket = connect(port).await.unwrap();
        let (message, reconnected) = receive_reconnecting(&mut socket, port).await.unwrap();
        assert!(message.is_none());
        assert!(reconnected);
        let (message, reconnected) = receive_reconnecting(&mut socket, port).await.unwrap();
        let message = message.unwrap();
        assert_eq!(message["type"], "auth_state");
        assert!(!reconnected);
        server.await.unwrap();
    }
    #[tokio::test]
    async fn command_connection_recovers_after_soloist_drops_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await.unwrap().unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            let command: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            assert_eq!(command["command"], "get_state");
        });
        let mut socket = connect(port).await.unwrap();
        socket.close(None).await.unwrap();
        assert!(
            send_reconnecting(&mut socket, port, json!({"command":"get_state"}))
                .await
                .unwrap()
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn playlist_offsets_and_other_devices_keep_the_existing_route() {
        let client = Client::default();
        client.listening(Some(12345));
        client.bind(Some(Device {
            id: Some("soloist".into()),
            ..Default::default()
        }));
        let mut request = remote(RemoteAction::Play);
        if let ApiRequest::Remote { play, .. } = &mut request {
            *play = Some(crate::api::PlayRequest {
                context_uri: Some("spotify:playlist:original".into()),
                offset_position: Some(17),
                ..Default::default()
            });
        }
        assert!(client.dispatch(&request).await.is_none());
        let mut other = remote(RemoteAction::Pause);
        if let ApiRequest::Remote { device_id, .. } = &mut other {
            *device_id = Some("speaker".into());
        }
        assert!(client.dispatch(&other).await.is_none());
    }
    #[test]
    fn repeat_track_does_not_also_enable_context_repeat() {
        let mut request = remote(RemoteAction::Repeat);
        if let ApiRequest::Remote { repeat, .. } = &mut request {
            *repeat = "track".into();
        }
        assert_eq!(
            commands(&request).unwrap(),
            vec![
                json!({"command":"set_repeat_context","enabled":false}),
                json!({"command":"set_repeat_track","enabled":true})
            ]
        );
    }
    #[tokio::test]
    async fn a_transfer_during_a_queue_read_leaves_the_new_devices_queue_alone() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let command = socket.next().await.unwrap().unwrap();
            let command: Value = serde_json::from_str(command.to_text().unwrap()).unwrap();
            assert_eq!(command["command"], "get_state");
            socket
                .send(Message::Text(
                    json!({"type":"playback_state","is_active":false})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        });
        let client = Client::default();
        client.listening(Some(port));
        client.bind(Some(Device {
            id: Some("soloist".into()),
            ..Default::default()
        }));
        client.active(true);
        assert!(
            client
                .dispatch(&ApiRequest::Queue { seq: 9 })
                .await
                .is_none()
        );
        server.await.unwrap();
    }
}
