//! Optional, authenticated MCP access to the running application's Spotify session.

use std::fmt::{self, Debug, Formatter};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rand::RngCore;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::schemars::{self, JsonSchema};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::api::{PlayRequest, PlaylistId};
use crate::backend::{ApiRequest, ApiResponse, Command, RemoteAction};
use crate::player::RepeatMode;

const REQUEST_LIMIT: usize = 64 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(90);

#[doc(hidden)]
pub struct Completion {
    pub(crate) reply: oneshot::Sender<Result<Value, String>>,
    pub(crate) result: Result<Value, String>,
}

/// A usable credential that diagnostics must never print.
#[derive(Clone)]
pub struct Connection {
    pub address: SocketAddr,
    token: Arc<str>,
}

impl Debug for Connection {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Connection {
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.address)
    }

    /// Copy only on an explicit user action; never log or persist this value.
    pub fn client_config(&self) -> String {
        json!({"mcpServers": {"spotifast": {
            "url": self.url(), "headers": {"Authorization": format!("Bearer {}", self.token)}
        }}})
        .to_string()
    }
}

#[derive(Clone, Debug, Default)]
pub enum Status {
    #[default]
    Disabled,
    Running(Connection),
    Failed(String),
}

pub struct Server {
    pub connection: Connection,
    cancel: CancellationToken,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Server {
    pub async fn start(commands: mpsc::UnboundedSender<Command>, port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let connection = Connection {
            address: listener.local_addr()?,
            token: token.into(),
        };
        let cancel = CancellationToken::new();
        let handler = SpotifyServer {
            commands,
            cancel: cancel.clone(),
            calls: Arc::new(Semaphore::new(4)),
            edits: Arc::new(Semaphore::new(1)),
        };
        let service: StreamableHttpService<SpotifyServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(handler.clone()),
                Default::default(),
                StreamableHttpServerConfig::default()
                    .with_json_response(true)
                    .with_max_request_body_bytes(REQUEST_LIMIT)
                    .with_cancellation_token(cancel.child_token()),
            );
        let access = Access {
            connection: connection.clone(),
            cancel: cancel.clone(),
        };
        let router = axum::Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn(move |request, next| {
                authorize(access.clone(), request, next)
            }));
        let shutdown = cancel.clone();
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
            {
                log::error!("MCP server stopped: {error}");
            }
        });
        Ok(Self { connection, cancel })
    }
}

#[derive(Clone)]
struct Access {
    connection: Connection,
    cancel: CancellationToken,
}

fn allowed(headers: &HeaderMap, access: &Access) -> bool {
    if access.cancel.is_cancelled()
        || headers.contains_key(header::ORIGIN)
        || headers.get_all(header::AUTHORIZATION).iter().count() != 1
        || headers.get_all(header::HOST).iter().count() != 1
    {
        return false;
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    let address = access.connection.address;
    if host != Some(address.to_string().as_str())
        && host != Some(format!("localhost:{}", address.port()).as_str())
    {
        return false;
    }
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(supplied) = supplied else {
        return false;
    };
    // Compare fixed-length digests without an early exit on matching bytes.
    Sha256::digest(supplied.as_bytes())
        .iter()
        .zip(Sha256::digest(access.connection.token.as_bytes()))
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

async fn authorize(access: Access, request: Request, next: Next) -> Response {
    if !allowed(request.headers(), &access) {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

#[derive(Clone)]
struct SpotifyServer {
    commands: mpsc::UnboundedSender<Command>,
    cancel: CancellationToken,
    calls: Arc<Semaphore>,
    edits: Arc<Semaphore>,
}

impl ServerHandler for SpotifyServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("spotifast", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request = parse_tool(
            &request.name,
            Value::Object(request.arguments.unwrap_or_default()),
        )?;
        let _permit = self.calls.try_acquire().map_err(|_| {
            ErrorData::internal_error(
                "Too many active Spotify requests. Try again after one finishes.",
                None,
            )
        })?;
        let _edit = if matches!(
            request,
            ApiRequest::CreatePlaylist { .. }
                | ApiRequest::UpdatePlaylist { .. }
                | ApiRequest::AddToPlaylist { .. }
                | ApiRequest::RemoveFromPlaylist { .. }
                | ApiRequest::ReorderPlaylist { .. }
        ) {
            Some(self.edits.try_acquire().map_err(|_| ErrorData::internal_error("Another playlist edit is still running. Wait for its result before editing again.", None))?)
        } else {
            None
        };
        let cancel = self.cancel.child_token();
        let (reply, result) = oneshot::channel();
        self.commands
            .send(Command::Mcp {
                request,
                reply,
                cancel: cancel.clone(),
            })
            .map_err(|_| ErrorData::internal_error("Spotifast is shutting down", None))?;
        let outcome = tokio::select! {
            _ = self.cancel.cancelled() => Err("MCP was disabled".to_owned()),
            _ = context.ct.cancelled() => Err("Request cancelled. A write already sent to Spotify may have completed; read its state before retrying.".to_owned()),
            result = tokio::time::timeout(CALL_TIMEOUT, result) => match result {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("Spotify session changed or Spotifast stopped".to_owned()),
                Err(_) => Err("Spotify request timed out. A write may have completed; read its state before retrying.".to_owned()),
            },
        };
        cancel.cancel();
        let response = match outcome {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        };
        Ok(response.into())
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Search {
    query: String,
    #[serde(default)]
    scope: SearchScope,
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SearchScope {
    #[default]
    Catalogue,
    Playlists,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Page {
    #[serde(default)]
    offset: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PlaylistPage {
    playlist_id: PlaylistId,
    #[serde(default)]
    offset: u32,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreatePlaylist {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    public: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdatePlaylist {
    playlist_id: PlaylistId,
    name: Option<String>,
    description: Option<String>,
    public: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum EditPlaylist {
    Add {
        playlist_id: PlaylistId,
        uris: Vec<String>,
        position: Option<u32>,
    },
    Remove {
        playlist_id: PlaylistId,
        uris: Vec<String>,
        snapshot_id: String,
    },
    Move {
        playlist_id: PlaylistId,
        range_start: u32,
        insert_before: u32,
        snapshot_id: String,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Playback {
    Play {
        uri: Option<String>,
        device_id: Option<String>,
    },
    Pause {
        device_id: Option<String>,
    },
    Next {
        device_id: Option<String>,
    },
    Previous {
        device_id: Option<String>,
    },
    Seek {
        position_ms: u32,
        device_id: Option<String>,
    },
    Volume {
        percent: u8,
        device_id: Option<String>,
    },
    Shuffle {
        enabled: bool,
        device_id: Option<String>,
    },
    Repeat {
        mode: RepeatMode,
        device_id: Option<String>,
    },
    Transfer {
        device_id: String,
        #[serde(default)]
        play: bool,
    },
    Queue {
        uri: String,
        device_id: Option<String>,
    },
}

fn tool<T: JsonSchema>(name: &'static str, description: &'static str, read_only: bool) -> Tool {
    let schema = schemars::schema_for!(T);
    let mut tool = Tool::new(
        name,
        description,
        schema.as_object().cloned().unwrap_or_default(),
    );
    tool.annotations = Some(ToolAnnotations::new().read_only(read_only).open_world(true));
    tool
}

fn tools() -> Vec<Tool> {
    vec![
        tool::<Search>(
            "search",
            "Search Spotify. The catalogue scope returns tracks, albums, artists, shows and episodes; the playlists scope searches playlists. Results are bounded by Spotify's page limit.",
            true,
        ),
        tool::<Empty>(
            "playback_state",
            "Read Spotify's current playback state. Null means no active playback.",
            true,
        ),
        tool::<Empty>("devices", "List available Spotify Connect devices.", true),
        tool::<Empty>("queue", "Read the current Spotify queue.", true),
        tool::<Playback>(
            "playback",
            "Control Spotify playback, transfer to a device, or queue one track or episode. Spotify accepting a command does not mean audio has started; read playback_state to verify.",
            false,
        ),
        tool::<Page>(
            "playlists",
            "List one page of your playlists. Use next_offset from the response to continue.",
            true,
        ),
        tool::<PlaylistPage>(
            "playlist_tracks",
            "Read one page of playlist items and its snapshot_id. Keep positions and snapshots when planning edits.",
            true,
        ),
        tool::<CreatePlaylist>(
            "create_playlist",
            "Create a playlist, private by default. Returns the created playlist.",
            false,
        ),
        tool::<UpdatePlaylist>(
            "update_playlist",
            "Change a playlist's name, description or visibility. Requires edit permission.",
            false,
        ),
        tool::<EditPlaylist>(
            "edit_playlist",
            "Add up to 100 tracks/episodes, remove all occurrences of the supplied URIs, or move one item. Positions are zero-based. Remove and move require snapshot_id from playlist_tracks. Requires edit permission; Spotify confirms the write. Adding allows duplicates.",
            false,
        ),
    ]
}

fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, ErrorData> {
    serde_json::from_value(value)
        .map_err(|error| ErrorData::invalid_params(error.to_string(), None))
}

fn invalid(message: &'static str) -> ErrorData {
    ErrorData::invalid_params(message, None)
}

fn identifier(value: &str) -> Result<(), ErrorData> {
    if !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        Ok(())
    } else {
        Err(invalid("Invalid Spotify identifier"))
    }
}

fn text(value: &str, maximum: usize, empty: bool) -> Result<(), ErrorData> {
    if (empty || !value.trim().is_empty())
        && value.len() <= maximum
        && !value.chars().any(char::is_control)
    {
        Ok(())
    } else {
        Err(invalid(
            "Text is empty, too long, or contains control characters",
        ))
    }
}

fn playable(uri: &str, context: bool) -> Result<(), ErrorData> {
    let Some((kind, id)) = uri
        .strip_prefix("spotify:")
        .and_then(|value| value.split_once(':'))
    else {
        return Err(invalid("Expected a Spotify URI"));
    };
    identifier(id)?;
    if matches!(kind, "track" | "episode")
        || (context && matches!(kind, "album" | "artist" | "playlist" | "show"))
    {
        Ok(())
    } else {
        Err(invalid("Unsupported Spotify URI type"))
    }
}

fn uris(values: &[String]) -> Result<(), ErrorData> {
    if values.is_empty() || values.len() > 100 {
        return Err(invalid("Supply between 1 and 100 track or episode URIs"));
    }
    for value in values {
        playable(value, false)?;
    }
    Ok(())
}

fn parse_tool(name: &str, args: Value) -> Result<ApiRequest, ErrorData> {
    Ok(match name {
        "search" => {
            let args: Search = decode(args)?;
            text(&args.query, 200, false)?;
            match args.scope {
                SearchScope::Catalogue => ApiRequest::SearchCatalogue {
                    query: args.query,
                    serial: 0,
                },
                SearchScope::Playlists => ApiRequest::SearchPlaylists {
                    query: args.query,
                    serial: 0,
                },
            }
        }
        "playback_state" | "devices" | "queue" => {
            let _: Empty = decode(args)?;
            match name {
                "playback_state" => ApiRequest::PlaybackState { seq: 0 },
                "devices" => ApiRequest::Devices,
                _ => ApiRequest::Queue { seq: 0 },
            }
        }
        "playback" => playback(decode(args)?)?,
        "playlists" => {
            let args: Page = decode(args)?;
            ApiRequest::MyPlaylists {
                offset: args.offset,
                generation: 0,
            }
        }
        "playlist_tracks" => {
            let args: PlaylistPage = decode(args)?;
            identifier(args.playlist_id.as_str())?;
            ApiRequest::PlaylistItems {
                id: args.playlist_id.as_str().to_owned(),
                offset: args.offset,
                generation: 0,
            }
        }
        "create_playlist" => {
            let args: CreatePlaylist = decode(args)?;
            text(&args.name, 100, false)?;
            text(&args.description, 300, true)?;
            ApiRequest::CreatePlaylist {
                name: args.name,
                description: args.description,
                public: args.public,
            }
        }
        "update_playlist" => {
            let args: UpdatePlaylist = decode(args)?;
            identifier(args.playlist_id.as_str())?;
            if let Some(name) = &args.name {
                text(name, 100, false)?;
            }
            if let Some(description) = &args.description {
                text(description, 300, true)?;
            }
            if args.name.is_none() && args.description.is_none() && args.public.is_none() {
                return Err(invalid("Supply a field to update"));
            }
            ApiRequest::UpdatePlaylist {
                id: args.playlist_id.as_str().to_owned(),
                name: args.name,
                description: args.description,
                public: args.public,
            }
        }
        "edit_playlist" => match decode(args)? {
            EditPlaylist::Add {
                playlist_id,
                uris: items,
                position,
            } => {
                identifier(playlist_id.as_str())?;
                uris(&items)?;
                ApiRequest::AddToPlaylist {
                    playlist_id: playlist_id.as_str().to_owned(),
                    playlist_name: String::new(),
                    uris: items,
                    position,
                }
            }
            EditPlaylist::Remove {
                playlist_id,
                uris: items,
                snapshot_id,
            } => {
                identifier(playlist_id.as_str())?;
                uris(&items)?;
                text(&snapshot_id, 512, false)?;
                ApiRequest::RemoveFromPlaylist {
                    playlist_id: playlist_id.as_str().to_owned(),
                    uris: items,
                    snapshot_id: Some(snapshot_id),
                }
            }
            EditPlaylist::Move {
                playlist_id,
                range_start,
                insert_before,
                snapshot_id,
            } => {
                identifier(playlist_id.as_str())?;
                text(&snapshot_id, 512, false)?;
                ApiRequest::ReorderPlaylist {
                    playlist_id: playlist_id.as_str().to_owned(),
                    range_start,
                    insert_before,
                    snapshot_id: Some(snapshot_id),
                }
            }
        },
        _ => return Err(invalid("Unknown tool")),
    })
}

fn playback(args: Playback) -> Result<ApiRequest, ErrorData> {
    let mut request = ApiRequest::Remote {
        action: RemoteAction::Play,
        device_id: None,
        play: None,
        position_ms: 0,
        percent: 0,
        flag: false,
        repeat: String::new(),
    };
    let ApiRequest::Remote {
        action,
        device_id,
        play,
        position_ms,
        percent,
        flag,
        repeat,
    } = &mut request
    else {
        return Err(invalid("Invalid playback request"));
    };
    match args {
        Playback::Play {
            uri,
            device_id: device,
        } => {
            *device_id = device;
            if let Some(uri) = uri {
                playable(&uri, true)?;
                *play = Some(
                    if uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:") {
                        PlayRequest {
                            uris: vec![uri],
                            ..Default::default()
                        }
                    } else {
                        PlayRequest::context(uri)
                    },
                );
            }
        }
        Playback::Pause { device_id: device } => {
            *action = RemoteAction::Pause;
            *device_id = device;
        }
        Playback::Next { device_id: device } => {
            *action = RemoteAction::Next;
            *device_id = device;
        }
        Playback::Previous { device_id: device } => {
            *action = RemoteAction::Previous;
            *device_id = device;
        }
        Playback::Seek {
            position_ms: position,
            device_id: device,
        } => {
            *action = RemoteAction::Seek;
            *position_ms = position;
            *device_id = device;
        }
        Playback::Volume {
            percent: volume,
            device_id: device,
        } => {
            if volume > 100 {
                return Err(invalid("Volume must be between 0 and 100"));
            }
            *action = RemoteAction::Volume;
            *percent = volume;
            *device_id = device;
        }
        Playback::Shuffle {
            enabled,
            device_id: device,
        } => {
            *action = RemoteAction::Shuffle;
            *flag = enabled;
            *device_id = device;
        }
        Playback::Repeat {
            mode,
            device_id: device,
        } => {
            *action = RemoteAction::Repeat;
            *device_id = device;
            *repeat = mode.api_name().into();
        }
        Playback::Transfer { device_id, play } => {
            identifier(&device_id)?;
            return Ok(ApiRequest::Transfer { device_id, play });
        }
        Playback::Queue { uri, device_id } => {
            playable(&uri, false)?;
            if let Some(device) = &device_id {
                identifier(device)?;
            }
            return Ok(ApiRequest::AddToQueue {
                uri,
                device_id,
                label: String::new(),
            });
        }
    }
    if let Some(device) = device_id {
        identifier(device)?;
    }
    Ok(request)
}

fn value<T: Serialize>(result: &Result<T, crate::api::ApiError>) -> Result<Value, String> {
    match result {
        Ok(value) => serde_json::to_value(value).map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

fn page<T: Serialize>(
    result: &Result<crate::api::models::Page<T>, crate::api::ApiError>,
) -> Result<Value, String> {
    let mut data = value(result)?;
    if let Ok(page) = result {
        data["next_offset"] = json!(page.next_offset());
    }
    Ok(data)
}

pub(crate) fn response(response: &ApiResponse) -> Result<Value, String> {
    match response {
        ApiResponse::Devices(result) => value(result).map(|devices| json!({"devices": devices})),
        ApiResponse::PlaybackState { result, .. } => {
            value(result).map(|playback| json!({"playback": playback}))
        }
        ApiResponse::Queue { result, .. } => value(result),
        ApiResponse::Search { result, .. } => value(result),
        ApiResponse::SearchPlaylists { result, .. } => page(result),
        ApiResponse::MyPlaylists { result, .. } => page(result),
        ApiResponse::PlaylistItems { result, .. } => page(result),
        ApiResponse::PlaylistCreated(result) => value(result),
        ApiResponse::PlaylistUpdated { result, .. } => {
            value(result).map(|_| json!({"status": "confirmed"}))
        }
        ApiResponse::PlaylistItemsChanged { result, .. } => {
            value(result).map(|snapshot| json!({"status": "confirmed", "snapshot_id": snapshot}))
        }
        ApiResponse::Remote { result, .. }
        | ApiResponse::Transferred { result, .. }
        | ApiResponse::QueueAdded { result, .. } => {
            value(result).map(|_| json!({"status": "accepted"}))
        }
        _ => Err("Unsupported MCP result".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ApiError, models};

    fn args(name: &str, arguments: Value) -> ApiRequest {
        parse_tool(name, arguments).expect("test arguments must be accepted")
    }

    #[test]
    fn refuses_unsafe_or_ambiguous_edits() {
        for (name, arguments) in [
            ("create_playlist", json!({"name": ""})),
            ("update_playlist", json!({"playlist_id": "playlist123"})),
            ("playlist_tracks", json!({"playlist_id": "../../other"})),
            (
                "edit_playlist",
                json!({"action":"add", "playlist_id":"playlist123", "uris":[]}),
            ),
            (
                "edit_playlist",
                json!({"action":"add", "playlist_id":"playlist123", "uris":["spotify:album:album123"]}),
            ),
            (
                "edit_playlist",
                json!({"action":"add", "playlist_id":"playlist123", "uris":vec!["spotify:track:track123"; 101]}),
            ),
            ("playback", json!({"action":"volume", "percent":101})),
            (
                "playback",
                json!({"action":"play", "uri":"https://example.com/music"}),
            ),
            ("playback", json!({"action":"repeat", "mode":"unknown"})),
            ("search", json!({"query":"music\nnext"})),
            ("devices", json!({"unexpected":true})),
        ] {
            assert!(parse_tool(name, arguments).is_err(), "{name}");
        }
        assert!(matches!(
            args("create_playlist", json!({"name":"New playlist"})),
            ApiRequest::CreatePlaylist { public: false, .. }
        ));
        assert!(
            matches!(args("playback", json!({"action":"play", "uri":"spotify:track:track123"})), ApiRequest::Remote { play: Some(PlayRequest { context_uri: None, uris, .. }), .. } if uris == ["spotify:track:track123"])
        );
        assert!(
            matches!(args("edit_playlist", json!({"action":"move", "playlist_id":"playlist123", "range_start":3, "insert_before":0, "snapshot_id":"revision123"})), ApiRequest::ReorderPlaylist { range_start: 3, insert_before: 0, snapshot_id: Some(snapshot), .. } if snapshot == "revision123")
        );
    }

    #[test]
    fn results_preserve_errors_snapshots_and_pagination() {
        assert_eq!(
            response(&ApiResponse::Devices(Ok(vec![]))).unwrap(),
            json!({"devices": []})
        );
        assert_eq!(
            response(&ApiResponse::PlaybackState {
                seq: 0,
                result: Ok(None)
            })
            .unwrap(),
            json!({"playback": null})
        );
        let result = response(&ApiResponse::PlaylistItemsChanged {
            id: "playlist123".into(),
            message: String::new(),
            result: Ok(Some("revision123".into())),
        })
        .unwrap();
        assert_eq!(
            result,
            json!({"status":"confirmed", "snapshot_id":"revision123"})
        );
        assert!(
            response(&ApiResponse::PlaylistUpdated {
                id: "playlist123".into(),
                result: Err(ApiError::Decode("test failure".into()))
            })
            .is_err()
        );
        let result = response(&ApiResponse::MyPlaylists {
            offset: 0,
            generation: 0,
            result: Ok(models::Page {
                items: vec![],
                offset: 0,
                limit: 50,
                total: 100,
                next: Some("next-page".into()),
            }),
        })
        .unwrap();
        assert_eq!(result["next_offset"], 50);
    }

    async fn read_response(response: reqwest::Response) -> Value {
        let body = response.text().await.unwrap();
        if let Ok(value) = serde_json::from_str(&body) {
            return value;
        }
        body.lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim))
            .find_map(|data| serde_json::from_str(data).ok())
            .expect("MCP response must contain a JSON data frame")
    }

    struct Client {
        http: reqwest::Client,
        connection: Connection,
        session: Option<String>,
    }

    impl Client {
        fn new(connection: Connection) -> Self {
            Self {
                http: reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .unwrap(),
                connection,
                session: None,
            }
        }

        fn post(&self, message: Value) -> reqwest::RequestBuilder {
            let mut request = self
                .http
                .post(self.connection.url())
                .bearer_auth(self.connection.token.as_ref())
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", "2025-03-26")
                .json(&message);
            if let Some(session) = &self.session {
                request = request.header("Mcp-Session-Id", session);
            }
            request
        }

        async fn initialize(&mut self) {
            let response = self.post(json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params": {
                "protocolVersion":"2025-03-26", "capabilities":{}, "clientInfo":{"name":"test-client", "version":"1"}
            }})).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            self.session = response
                .headers()
                .get("Mcp-Session-Id")
                .map(|value| value.to_str().unwrap().to_owned());
            let body = read_response(response).await;
            assert_eq!(body["result"]["serverInfo"]["name"], "spotifast");
            let response = self
                .post(json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }
    }

    #[tokio::test]
    async fn http_requires_token_local_host_and_no_browser_origin() {
        let (commands, mut receiver) = mpsc::unbounded_channel();
        let server = Server::start(commands, 0).await.unwrap();
        assert!(server.connection.address.ip().is_loopback());
        assert!(!format!("{:?}", server.connection).contains(server.connection.token.as_ref()));
        let mut client = Client::new(server.connection.clone());
        assert_eq!(
            client
                .http
                .post(client.connection.url())
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        for request in [
            client.post(json!({})).bearer_auth("wrong-token"),
            client
                .post(json!({}))
                .header("Origin", "https://example.com"),
            client.post(json!({})).header("Host", "example.com"),
        ] {
            assert_eq!(
                request.send().await.unwrap().status(),
                StatusCode::FORBIDDEN
            );
        }
        client.initialize().await;
        let response = client
            .post(json!({"jsonrpc":"2.0", "id":2, "method":"tools/list"}))
            .send()
            .await
            .unwrap();
        let response = read_response(response).await;
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 10);
        assert!(
            receiver.try_recv().is_err(),
            "handshake and tool listing must not contact Spotify"
        );
        let oversized = client
            .post(json!({}))
            .body(" ".repeat(REQUEST_LIMIT + 1))
            .send()
            .await
            .unwrap();
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn tool_call_waits_for_the_backend_result_and_reports_failures() {
        let (commands, mut receiver) = mpsc::unbounded_channel();
        let server = Server::start(commands, 0).await.unwrap();
        let mut client = Client::new(server.connection.clone());
        client.initialize().await;
        let backend = tokio::spawn(async move {
            let Some(Command::Mcp { request, reply, .. }) = receiver.recv().await else {
                panic!("expected backend request")
            };
            assert!(matches!(
                request,
                ApiRequest::CreatePlaylist { public: false, .. }
            ));
            reply.send(Err("Spotify refused the edit".into())).unwrap();
        });
        let response = client
            .post(
                json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params": {
                    "name":"create_playlist", "arguments":{"name":"Test playlist"}
                }}),
            )
            .send()
            .await
            .unwrap();
        let response = read_response(response).await;
        backend.await.unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["content"][0]["text"],
            "Spotify refused the edit"
        );
    }

    #[tokio::test]
    async fn disabling_cancels_pending_calls_and_rotates_the_credential() {
        let (commands, mut receiver) = mpsc::unbounded_channel();
        let server = Server::start(commands.clone(), 0).await.unwrap();
        let mut client = Client::new(server.connection.clone());
        client.initialize().await;
        let call = client.post(json!({"jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":"devices", "arguments":{}}}));
        let pending = tokio::spawn(async move { call.send().await });
        let Some(Command::Mcp { cancel, reply, .. }) = receiver.recv().await else {
            panic!("expected request")
        };
        drop(server);
        tokio::time::timeout(Duration::from_secs(2), cancel.cancelled())
            .await
            .unwrap();
        drop(reply);
        let _ = tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap();
        let replacement = Server::start(commands, 0).await.unwrap();
        assert_ne!(replacement.connection.token, client.connection.token);
        let response = client
            .http
            .post(replacement.connection.url())
            .bearer_auth(client.connection.token.as_ref())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
