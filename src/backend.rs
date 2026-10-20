//! Bridge between the UI thread and asynchronous work.
//!
//! egui runs on the main thread and must never block. A dedicated tokio
//! runtime hosts the librespot engine, the Web API client, sign-in, and
//! artwork fetches; the two sides talk through channels. Every event wakes
//! the interface with `request_repaint`, so the app stays event-driven and
//! idle when nothing is happening.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_core::authentication::Credentials;
use tokio::sync::{mpsc, watch};

use crate::api::models::*;
use crate::api::{
    AccountId, ApiError, ApiGateway, ApiSource, NetActivity, Operation, PlayRequest, PlaylistId,
    SessionState, TokenProvider, WebTokens,
};
use crate::credentials::{
    Grant as StoredGrant, Lease as CredentialLease, Slot as CredentialSlot,
    Store as CredentialStore,
};
use crate::http::Http;
use crate::images::{ArtLoader, art_palette};
use crate::model::PlaylistCache;
use crate::paths::AppDirs;
use crate::player::{
    Engine, EngineConfig, EngineEvent, Heard, KeptPlayer, LocalState, PlaybackResume, PlayerCommand,
};
use crate::session_reads;
use crate::settings::ProxyConfig;

pub type ApiResult<T> = Result<T, ApiError>;

const PREMIUM_NEEDED: &str = "Local playback needs Spotify Premium.";
const ALBUM_TYPE_TIMEOUT: Duration = Duration::from_secs(30);
// Leave time for access-point retries plus resolver and authentication work.
const ENGINE_CONNECT_TIMEOUT: Duration = Duration::from_secs(75);
// Keep at most one full Web API album page outstanding for a playback engine.
const MAX_PENDING_ALBUM_TYPES: usize = 50;
// Saved shows asked about in one extended-metadata request.
const AUDIOBOOK_BATCH: usize = 50;
/// How long resolving a radio station and its songs may take.
const RADIO_TIMEOUT: Duration = Duration::from_secs(20);

/// The `spotify:local:` URIs of the songs in `folder` and its subfolders,
/// found exactly as the player finds them, so the songs a playlist lists
/// that will play are the ones shown as playable.
fn local_file_uris(folder: &std::path::Path) -> HashSet<String> {
    librespot_playback::local_file::create_local_file_lookup(&[folder.to_path_buf()])
        .iter()
        .map(|(uri, _)| uri.to_uri())
        .collect()
}
/// Newest episodes read from each saved podcast for Home's podcast shelf.
const HOME_EPISODES_PER_SHOW: u32 = 5;
pub const PLAYLIST_PAGE_SIZE: u32 = 50;
const RECONNECT_WINDOW: Duration = Duration::from_secs(600);
const RECONNECT_LIMIT: usize = 6;
/// Each followed artist's newest albums and singles the New releases
/// fallback asks for.
const NEW_RELEASES_PER_ARTIST: u32 = 10;
/// Seconds between background attempts to bring the engine back with the
/// saved playback grant after a connection failure that was not a rejected
/// grant (#818). The last delay repeats, so a session lost overnight comes
/// back on its own at the cost of one connection attempt every ten minutes.
const ENGINE_RETRY_DELAYS: [u64; 6] = [5, 15, 60, 120, 300, 600];

async fn connect_engine_with_deadline<F: std::future::Future>(
    connect: F,
) -> Result<F::Output, tokio::time::error::Elapsed> {
    tokio::time::timeout(ENGINE_CONNECT_TIMEOUT, connect).await
}

/// True when the session has already dropped this many times in the window,
/// so another reconnect would only flap. Callers that still reconnect must
/// push `now` themselves.
fn session_drops_exhausted(reconnects: &mut Vec<Instant>, now: Instant) -> bool {
    reconnects.retain(|attempt| now.duration_since(*attempt) < RECONNECT_WINDOW);
    reconnects.len() >= RECONNECT_LIMIT
}

/// Record a replacement requested while a connection attempt is still in
/// flight. The finished attempt must be discarded and immediately retried
/// with the newest config instead of installing stale state.
fn defer_engine_replace(engine_busy: bool, restart_pending: &mut bool) -> bool {
    if engine_busy {
        *restart_pending = true;
    }
    engine_busy
}

#[derive(Clone, Debug, PartialEq)]
pub enum AuthStatus {
    Starting,
    SignedOut,
    WaitingForBrowser { url: String },
    Connecting,
    Connected { username: String },
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteAction {
    Play,
    Pause,
    Next,
    Previous,
    Seek,
    Volume,
    Shuffle,
    Repeat,
}

/// Which of the two readers of the recently-played endpoint an answer
/// belongs to: the shelf on Home, or the Recents tab in the queue panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecentsFor {
    Home,
    Panel,
}

#[derive(Clone, Debug)]
pub enum ApiRequest {
    Me,
    Devices,
    PlaybackState {
        seq: u64,
    },
    Queue {
        seq: u64,
    },
    RecentlyPlayed {
        /// Request owner. Home and Recents use separate generation counters,
        /// so generation alone cannot route the response.
        who: RecentsFor,
        generation: u64,
        before: Option<String>,
        limit: u32,
    },
    TopTracks {
        offset: u32,
        full: bool,
        generation: u64,
    },
    TopArtists {
        generation: u64,
    },
    Recommendations {
        seed_tracks: Vec<String>,
        seed_artists: Vec<String>,
        generation: u64,
    },
    MadeForYou {
        generation: u64,
        force: bool,
    },
    /// The latest releases from the artists the account follows.
    NewReleases {
        generation: u64,
    },
    /// Every song of these releases, for the New releases page.
    NewReleaseTracks {
        albums: Vec<Album>,
        generation: u64,
    },
    /// The songs hidden in Spotify's playlists, over the playback session.
    HiddenSongs {
        generation: u64,
    },
    /// Hide a song in one of Spotify's playlists, or show it again.
    SetHiddenInPlaylist {
        playlist_uri: String,
        track_uri: String,
        hidden: bool,
    },
    MyPlaylists {
        offset: u32,
        generation: u64,
    },
    Playlist {
        id: String,
        generation: u64,
    },
    PlaylistItems {
        id: String,
        offset: u32,
        generation: u64,
    },
    /// A slice of a playlist read only for who added its songs; the rows
    /// on screen stay untouched.
    PlaylistSample {
        id: String,
        offset: u32,
        generation: u64,
    },
    CreatePlaylist {
        name: String,
        public: bool,
        description: String,
    },
    UploadPlaylistCover {
        id: String,
        request: u64,
        previous_urls: Vec<String>,
        cover: crate::playlist_cover::Cover,
    },
    UpdatePlaylist {
        id: String,
        name: Option<String>,
        description: Option<String>,
        public: Option<bool>,
    },
    CheckPlaylistDuplicates {
        playlist_id: String,
        playlist_name: String,
        items: Vec<PlayableItem>,
        position: Option<u32>,
    },
    AddToPlaylist {
        playlist_id: String,
        playlist_name: String,
        uris: Vec<String>,
        position: Option<u32>,
    },
    RemoveFromPlaylist {
        playlist_id: String,
        uris: Vec<String>,
        snapshot_id: Option<String>,
    },
    ReorderPlaylist {
        playlist_id: String,
        range_start: u32,
        insert_before: u32,
        snapshot_id: Option<String>,
    },
    FollowPlaylist {
        id: String,
        follow: bool,
    },
    SavedTracks {
        offset: u32,
        generation: u64,
    },
    SavedAlbums {
        offset: u32,
    },
    FollowedArtists {
        after: Option<String>,
    },
    SavedShows {
        offset: u32,
    },
    SavedEpisodes {
        offset: u32,
    },
    SetSaved {
        uris: Vec<String>,
        saved: bool,
    },
    Contains {
        uris: Vec<String>,
    },
    Search {
        query: String,
        serial: u64,
    },
    SearchCatalogue {
        query: String,
        serial: u64,
    },
    SearchPlaylists {
        query: String,
        serial: u64,
    },
    /// Search for one kind of thing and report the best match, to play it:
    /// how `spotifast play-search` plays a song by its name.
    PlaySearch {
        kind: String,
        query: String,
        serial: u64,
    },
    Artist {
        id: String,
    },
    ArtistTopTracks {
        id: String,
    },
    ArtistAlbums {
        id: String,
        groups: String,
        offset: u32,
    },
    RelatedArtists {
        id: String,
    },
    Album {
        id: String,
    },
    AlbumTracks {
        id: String,
        offset: u32,
        generation: u64,
    },
    AlbumQueueTracks {
        id: String,
        offset: u32,
        request: u64,
    },
    Show {
        id: String,
    },
    ShowEpisodes {
        id: String,
        offset: u32,
    },
    /// The newest episodes of a few saved podcasts, for Home. The shows
    /// are read together, as many at once as the client lets requests
    /// run; a rate limit, quota or lost sign-in stops the rest.
    HomeEpisodes {
        shows: Vec<Show>,
        generation: u64,
    },
    Track {
        id: String,
    },
    /// One episode, asked for by a link to it: the podcast it belongs to
    /// is the page that opens.
    Episode {
        id: String,
    },
    Remote {
        action: RemoteAction,
        device_id: Option<String>,
        play: Option<PlayRequest>,
        position_ms: u32,
        percent: u8,
        flag: bool,
        repeat: String,
    },
    Transfer {
        device_id: String,
        play: bool,
    },
    /// Shuffle on, then start the context, one after the other: sent as two
    /// independent requests they race, and shuffle sometimes lost.
    ShufflePlay {
        device_id: Option<String>,
        play: PlayRequest,
    },
    AddToQueue {
        uri: String,
        device_id: Option<String>,
        label: String,
    },
    AddManyToQueue {
        request: u64,
        uris: Vec<String>,
        device_id: Option<String>,
    },
}

impl ApiRequest {
    /// Identifies reads that share the background request concurrency limit.
    fn background(&self) -> bool {
        matches!(
            self,
            Self::PlaybackState { .. }
                | Self::RecentlyPlayed { .. }
                | Self::TopTracks { .. }
                | Self::TopArtists { .. }
                | Self::Recommendations { .. }
                | Self::MadeForYou { .. }
                | Self::NewReleases { .. }
                | Self::NewReleaseTracks { .. }
                | Self::HiddenSongs { .. }
                | Self::MyPlaylists { .. }
                | Self::PlaylistSample { .. }
                | Self::Contains { .. }
        )
    }
}

#[derive(Debug)]
pub enum ApiResponse {
    Me(ApiResult<User>),
    Devices(ApiResult<Vec<Device>>),
    PlaybackState {
        seq: u64,
        result: ApiResult<Option<PlaybackState>>,
    },
    Queue {
        seq: u64,
        result: ApiResult<Queue>,
    },
    RecentlyPlayed {
        who: RecentsFor,
        generation: u64,
        limit: u32,
        result: ApiResult<CursorPage<PlayHistory>>,
    },
    TopTracks {
        offset: u32,
        full: bool,
        generation: u64,
        result: ApiResult<Page<Track>>,
    },
    TopArtists {
        generation: u64,
        result: ApiResult<Vec<Artist>>,
    },
    Recommendations {
        generation: u64,
        result: ApiResult<Vec<Track>>,
    },
    MadeForYou {
        generation: u64,
        personalized: bool,
        result: ApiResult<Vec<Playlist>>,
        top_mixes: ApiResult<Vec<Playlist>>,
    },
    NewReleases {
        generation: u64,
        result: ApiResult<Vec<Album>>,
    },
    NewReleaseTracks {
        generation: u64,
        result: ApiResult<Vec<Track>>,
    },
    HiddenSongs {
        generation: u64,
        result: ApiResult<Vec<(String, String)>>,
    },
    HiddenInPlaylistSet {
        playlist_uri: String,
        track_uri: String,
        hidden: bool,
        result: ApiResult<()>,
    },
    MyPlaylists {
        offset: u32,
        generation: u64,
        result: ApiResult<Page<Playlist>>,
    },
    Playlist {
        id: String,
        generation: u64,
        result: ApiResult<Playlist>,
    },
    PlaylistItems {
        id: String,
        offset: u32,
        generation: u64,
        result: ApiResult<Page<PlaylistItem>>,
    },
    PlaylistSample {
        id: String,
        generation: u64,
        result: ApiResult<Page<PlaylistItem>>,
    },
    PlaylistCreated(ApiResult<Playlist>),
    PlaylistCoverUploaded {
        id: String,
        request: u64,
        previous_urls: Vec<String>,
        cover: crate::playlist_cover::Cover,
        result: ApiResult<()>,
    },
    PlaylistUpdated {
        id: String,
        result: ApiResult<()>,
    },
    PlaylistDuplicatesChecked {
        playlist_id: String,
        playlist_name: String,
        items: Vec<PlayableItem>,
        position: Option<u32>,
        result: ApiResult<Vec<String>>,
    },
    PlaylistItemsChanged {
        id: String,
        message: String,
        result: ApiResult<Option<String>>,
    },
    PlaylistFollowChanged {
        id: String,
        followed: bool,
        result: ApiResult<()>,
    },
    SavedTracks {
        offset: u32,
        generation: u64,
        account_id: Option<String>,
        result: ApiResult<Page<SavedTrack>>,
    },
    SavedAlbums {
        offset: u32,
        result: ApiResult<Page<SavedAlbum>>,
    },
    FollowedArtists {
        after: Option<String>,
        result: ApiResult<CursorPage<Artist>>,
    },
    SavedShows {
        offset: u32,
        result: ApiResult<Page<SavedShow>>,
    },
    SavedEpisodes {
        offset: u32,
        result: ApiResult<Page<SavedEpisode>>,
    },
    SavedChanged {
        uris: Vec<String>,
        saved: bool,
        result: ApiResult<()>,
    },
    Contains {
        uris: Vec<String>,
        result: ApiResult<Vec<bool>>,
    },
    SearchStarted {
        query: String,
        serial: u64,
        split: bool,
    },
    Search {
        query: String,
        serial: u64,
        result: ApiResult<SearchResults>,
    },
    SearchPlaylists {
        query: String,
        serial: u64,
        result: ApiResult<Page<Playlist>>,
    },
    /// The URI of the best match for a [`ApiRequest::PlaySearch`], or `None`
    /// when nothing matched.
    PlaySearch {
        query: String,
        serial: u64,
        result: ApiResult<Option<String>>,
    },
    Artist {
        id: String,
        result: ApiResult<Artist>,
    },
    ArtistTopTracks {
        id: String,
        result: ApiResult<Vec<Track>>,
    },
    ArtistAlbums {
        id: String,
        groups: String,
        offset: u32,
        result: ApiResult<Page<Album>>,
    },
    RelatedArtists {
        id: String,
        result: ApiResult<Vec<Artist>>,
    },
    Album {
        id: String,
        result: ApiResult<Album>,
    },
    AlbumTracks {
        id: String,
        offset: u32,
        generation: u64,
        result: ApiResult<Page<Track>>,
    },
    AlbumQueueTracks {
        offset: u32,
        request: u64,
        result: ApiResult<Page<Track>>,
    },
    Show {
        id: String,
        result: ApiResult<Show>,
    },
    ShowEpisodes {
        id: String,
        offset: u32,
        result: ApiResult<Page<Episode>>,
    },
    /// Each show with its newest episodes, in the order asked for.
    HomeEpisodes {
        generation: u64,
        result: ApiResult<Vec<(Show, Vec<Episode>)>>,
    },
    Track {
        id: String,
        result: ApiResult<Track>,
    },
    Episode {
        id: String,
        result: ApiResult<Episode>,
    },
    Remote {
        action: RemoteAction,
        result: ApiResult<()>,
    },
    Transferred {
        device_id: String,
        result: ApiResult<()>,
    },
    QueueAdded {
        label: String,
        result: ApiResult<()>,
    },
    QueueBatchAdded {
        request: u64,
        added: usize,
        result: ApiResult<()>,
    },
}

pub enum PlaylistCacheRows {
    Replace(Vec<PlaylistItem>),
    Append {
        previous_rows: usize,
        previous_offset: u32,
        items: Vec<PlaylistItem>,
    },
}

/// Work for the playlist cache writer, done in the order it was queued.
enum PlaylistCacheJob {
    Write(PlaylistCacheWrite),
    /// Remove every account's cached playlist pages, after the writes
    /// before it and before the ones after it.
    Purge(std::path::PathBuf),
}

impl PlaylistCacheJob {
    fn into_write(self) -> Option<PlaylistCacheWrite> {
        match self {
            Self::Write(write) => Some(write),
            Self::Purge(_) => None,
        }
    }
}

struct PlaylistCacheWrite {
    path: std::path::PathBuf,
    account_id: String,
    id: String,
    generation: u64,
    snapshot: String,
    rows: PlaylistCacheRows,
    total: u32,
    next_offset: Option<u32>,
}

/// What the app asks of its login registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutostartRequest {
    /// Read what the system has registered.
    Read,
    /// Register this way, or remove the registration with `None`.
    Set(Option<fastframe_autostart::Start>),
}

/// Spotifast's registration to start at login. The hidden start passes
/// `--start-hidden`, which the command line takes.
pub fn autostart() -> fastframe_autostart::Autostart {
    fastframe_autostart::Autostart::new("spotifast", "Spotifast")
}

pub enum Command {
    OpenThemesFolder,
    /// Read or change the login registration, off the UI thread: it reads
    /// and writes a file, the registry or D-Bus, and the Flatpak portal may
    /// ask the person first.
    Autostart(AutostartRequest),
    /// Read the names of this computer's audio outputs, off the UI thread.
    ListOutputDevices,
    ProxyRestored {
        lease: CredentialLease,
        result: Result<crate::credentials::Loaded, crate::credentials::Error>,
    },
    CredentialsRestored {
        slot: CredentialSlot,
        lease: CredentialLease,
        result: Result<crate::credentials::Loaded, crate::credentials::Error>,
    },
    CheckPlaylistCover {
        id: String,
        request: u64,
        cover: crate::playlist_cover::Cover,
        images: Vec<crate::api::models::Image>,
    },
    ChoosePlaylistCover {
        id: String,
        request: u64,
        selected:
            std::pin::Pin<Box<dyn std::future::Future<Output = Option<rfd::FileHandle>> + Send>>,
    },
    ChooseLocalFilesFolder {
        selected:
            std::pin::Pin<Box<dyn std::future::Future<Output = Option<rfd::FileHandle>> + Send>>,
    },
    /// Find the songs in the local files folder, off the UI thread, as the
    /// player does when it starts.
    ScanLocalFiles(std::path::PathBuf),
    /// Start (or restart) the Web API sign-in in the browser.
    SignIn {
        request: u64,
        config: ProxyConfig,
    },
    CancelSignIn,
    SignOut,
    /// Authorize local playback on this computer (a separate browser grant).
    AuthorizePlayback,
    /// Reload the engine config (audio settings changed).
    RestartEngine(EngineConfig),
    /// Rebuild the HTTP client. Restart local playback only when its HTTP
    /// proxy changed; Off, System, and SOCKS5 share a direct engine connection.
    ApplyProxy {
        request: u64,
        config: ProxyConfig,
    },
    Player(PlayerCommand),
    Api(ApiRequest),
    ApiFinished {
        generation: u64,
        home_account: Option<AccountId>,
        response: Box<ApiResponse>,
        expired: Vec<ApiSource>,
        shared_lease: CredentialLease,
        personal_lease: CredentialLease,
    },
    Accent {
        url: String,
    },
    Shutdown,
    /// Internal: the Web API browser flow produced a grant.
    WebSignedIn {
        source: ApiSource,
        token: Box<crate::auth::StoredToken>,
        lease: CredentialLease,
        attempt: u64,
    },
    WebVerified {
        source: ApiSource,
        token: Box<crate::auth::StoredToken>,
        user: Box<User>,
        lease: CredentialLease,
        attempt: u64,
    },
    WebVerificationFailed {
        source: ApiSource,
        lease: CredentialLease,
        attempt: u64,
        error: ApiError,
    },
    /// Internal: a Web API browser flow or verification ended (success or not).
    SignInEnded {
        source: ApiSource,
        attempt: u64,
    },
    /// Internal: the playback browser flow ended without a credential.
    PlaybackAuthEnded {
        attempt: u64,
    },
    /// Internal: the playback grant produced a streaming access token.
    PlaybackAuthorized {
        access_token: String,
        lease: CredentialLease,
        attempt: u64,
    },
    /// Internal: an engine connection attempt finished.
    EngineConnected {
        session_generation: u64,
        engine: Box<Option<Engine>>,
        error: Option<String>,
        lease: CredentialLease,
        attempt: u64,
    },
    /// OS reports network reachability changed.
    NetworkStatus(bool),
    /// Internal: librespot's session ended on its own.
    Reconnect,
    /// Internal: the backoff after a failed engine connection has passed.
    RetryEngine {
        generation: u64,
    },
    /// Look for Spotify Connect receivers on the local network.
    DiscoverReceivers,
    /// Send the account to a receiver so it joins Spotify Connect.
    ActivateReceiver(Box<crate::zeroconf::Receiver>),
    /// Ask GitHub whether a newer release exists. Manual checks report every
    /// outcome; the daily check only announces a new release.
    CheckForUpdates {
        manual: bool,
        source: crate::updates::Source,
    },
    InspectUpdate,
    DownloadUpdate {
        release: crate::updates::Release,
        source: crate::updates::Source,
    },
    InstallUpdate {
        prepared: Box<crate::updates::Prepared>,
        arguments: Vec<String>,
    },
    /// The words of a track, from LRCLIB.
    Lyrics(Box<LyricsRequest>),
    /// The account's playlist tree, folders and all, from the session.
    Rootlist,
    /// Internal: a rootlist read completed for this signed-in session.
    RootlistFinished {
        generation: u64,
        result: Result<crate::player::Rootlist, String>,
    },
    /// Check that a reconnect's pickup really started, and try again if not.
    VerifyResume,
    /// Add, replace, or remove the optional personal Web API application.
    ConfigurePersonalWebApp(Option<String>),
    /// Read a playlist's cached items from disk.
    LoadPlaylistCache {
        id: String,
        generation: u64,
    },
    /// Remember a playlist prefix on disk under its snapshot.
    StorePlaylistCache {
        id: String,
        generation: u64,
        snapshot: String,
        rows: PlaylistCacheRows,
        total: u32,
        next_offset: Option<u32>,
    },
    /// Resolve user ids to display names through the streaming session.
    UserNames(Vec<String>),
    /// Read Spotify's play counts for an album's songs or an artist's
    /// popular songs, by the album's or the artist's URI.
    PlayCounts(String),
    /// Read what friends are listening to through the streaming session,
    /// for the app's request generation.
    FriendActivity(u64),
    LoadLikedSongsCache {
        generation: u64,
    },
    StoreLikedSongsCache(crate::liked::Cache),
    /// Read the verified account's last playlist list and Home shelves.
    LoadAccountCaches {
        generation: u64,
    },
    StoreLibraryCache(crate::account_cache::Cache<Vec<Playlist>>),
    StoreHomeCache(crate::account_cache::Cache<crate::model::HomeSnapshot>),
    /// Resolve the precise type of Web API singles through the streaming session.
    AlbumTypes(Vec<String>),
    /// Ask the streaming session which saved shows are audiobooks.
    AudiobookShows(Vec<String>),
    /// Read the headers of library playlists the Web API listed blank
    /// through the streaming session.
    PlaylistHeaders(Vec<String>),
    /// Resolve Spotify's radio seeded by `seed` through the streaming session.
    Radio {
        seed: String,
        generation: u64,
    },
    SetPrereleaseSaved {
        uri: String,
        saved: bool,
        generation: u64,
    },
    /// Remove exactly these rows of a playlist through the streaming
    /// session, leaving the song's other copies (#937). Answers as
    /// `ApiResponse::PlaylistItemsChanged`.
    RemovePlaylistRows {
        playlist_id: String,
        rows: Vec<session_reads::RemovedRow>,
    },
    PrereleaseSaved {
        session_generation: u64,
        uri: String,
        saved: bool,
        generation: u64,
        result: Result<(), String>,
    },
    /// Resolve a countdown page once local playback is ready.
    Prerelease {
        uri: String,
        generation: u64,
    },
    PrereleaseResolved {
        session_generation: u64,
        uri: String,
        generation: u64,
        result: Result<Box<crate::prerelease::Prerelease>, String>,
    },
    /// Internal: a radio finished resolving for the session it started in.
    RadioResolved {
        session_generation: u64,
        seed: String,
        generation: u64,
        result: Result<Vec<crate::api::models::Track>, String>,
    },
    /// Internal: an audiobook lookup finished for the session it started in.
    AudiobookShowsResolved {
        session_generation: u64,
        audiobooks: Vec<String>,
    },
    /// Internal: one playlist header read finished for the session and
    /// engine it started on. `None` when the session could not read it.
    PlaylistHeaderResolved {
        id: String,
        session_generation: u64,
        engine_generation: u64,
        header: Option<crate::api::models::Playlist>,
    },
    /// Internal: one precise album type lookup finished.
    AlbumTypeResolved {
        uri: String,
        session_generation: u64,
        engine_generation: u64,
        result: Result<bool, String>,
    },
}

pub struct LyricsRequest {
    /// The track the answer is for, so a stale one is ignored.
    pub uri: String,
    pub query: crate::lyrics::Query,
}

pub enum Event {
    /// The folder picked in the local files dialog; `None` when cancelled.
    LocalFilesFolderChosen(Option<std::path::PathBuf>),
    /// The `spotify:local:` URIs the songs in `folder` play for.
    LocalFiles {
        folder: std::path::PathBuf,
        uris: Arc<HashSet<String>>,
    },
    ProxyRestored {
        config: ProxyConfig,
        password: Option<crate::credentials::ProxyPassword>,
    },
    ProxyPasswordStored,
    ProxyStorageFailed(crate::credentials::Error),
    ProxyApplied {
        request: u64,
        config: ProxyConfig,
        result: Result<bool, String>,
    },
    UpdateSupport(Result<crate::updates::Installation, String>),
    UpdateProgress {
        received: u64,
        total: u64,
    },
    UpdateDownloaded(Result<Box<crate::updates::Prepared>, String>),
    UpdateInstalling(Result<(), String>),
    PlaylistCoverChecked {
        id: String,
        request: u64,
        images: Vec<crate::api::models::Image>,
        result: Result<bool, String>,
    },
    PlaylistCoverChosen {
        id: String,
        request: u64,
        result: Result<Option<crate::playlist_cover::Cover>, String>,
    },
    Auth(AuthStatus),
    Playback(LocalPlayback),
    /// Receivers seen on the local network that Spotify has not listed.
    Receivers(Vec<crate::zeroconf::Receiver>),
    /// This computer's audio outputs, for the device picker.
    OutputDevices(Result<Vec<fastframe_audio::DeviceInfo>, String>),
    /// Local playback opened on the device an older build saved by `name`,
    /// whose `id` the setting holds from now on.
    AudioDeviceFound {
        name: String,
        id: String,
    },
    /// Cast advertisements ready before the final browse/probe reconciliation.
    ReceiversProgress(crate::zeroconf::ReceiverUpdates),
    /// The login registration after `request`: what the system has now, or
    /// why the change failed.
    Autostart {
        request: AutostartRequest,
        result: Result<fastframe_autostart::State, String>,
    },
    ReceiverActivated {
        name: String,
        result: Result<(), String>,
    },
    Local(Box<LocalState>),
    Api(Box<ApiResponse>),
    Accent {
        url: String,
        color: [u8; 3],
        /// The cover's colours, the accent first, for the visualizer.
        palette: Vec<[u8; 3]>,
    },
    Error(String),
    /// GitHub answered an update check, or the request failed.
    UpdateChecked {
        manual: bool,
        result: Result<Option<crate::updates::Release>, String>,
    },
    /// Track lyrics, or `None` when unavailable.
    Lyrics {
        uri: String,
        result: Result<Option<crate::lyrics::Lyrics>, String>,
    },
    /// The account's playlist tree, folders and all, and which of its
    /// playlists take songs from this account.
    Rootlist {
        result: Result<crate::player::Rootlist, String>,
    },
    /// The result of reading a playlist cache for this load generation.
    PlaylistCache {
        account_id: String,
        id: String,
        generation: u64,
        cache: Option<PlaylistCache>,
    },
    PlaylistCacheStored {
        account_id: String,
        id: String,
        generation: u64,
        snapshot: String,
        success: bool,
    },
    /// What friends are listening to, newest first, or why it could not be
    /// read, answering the request of `generation`.
    FriendActivity {
        generation: u64,
        result: Result<Vec<session_reads::FriendActivity>, String>,
    },
    /// A user id resolved to a display name and avatar (each `None` when
    /// nothing answers).
    UserName {
        id: String,
        name: Option<String>,
        image: Option<String>,
    },
    /// Play counts by track URI for the album or artist `uri`, or why they
    /// could not be read.
    PlayCounts {
        uri: String,
        result: Result<std::collections::HashMap<String, u64>, String>,
    },
    /// Saved shows that Spotify's metadata marks as audiobooks. librespot
    /// cannot play them, so the Podcasts shelf leaves them out.
    AudiobookShows(Vec<String>),
    /// The header the streaming session read for a library playlist the
    /// Web API listed blank.
    PlaylistHeader(Box<crate::api::models::Playlist>),
    /// The songs of the radio seeded by `seed`, for the request `generation`.
    Radio {
        seed: String,
        generation: u64,
        result: Result<Vec<crate::api::models::Track>, String>,
    },
    Prerelease {
        uri: String,
        generation: u64,
        result: Result<Box<crate::prerelease::Prerelease>, String>,
    },
    PrereleaseSaved {
        uri: String,
        saved: bool,
        generation: u64,
        result: Result<(), String>,
    },
    /// Whether Spotify's internal metadata positively identifies an album as an EP.
    AlbumType {
        uri: String,
        result: Result<bool, String>,
    },
    /// The verified personal Web API app, or `None` when it is disabled.
    WebApp {
        client_id: Option<String>,
    },
    LikedSongsCache {
        account_id: String,
        generation: u64,
        cache: Option<crate::liked::Cache>,
    },
    /// OS reports network reachability changed.
    NetworkStatus(bool),
    AccountCaches {
        account_id: String,
        generation: u64,
        playlists: Option<Vec<Playlist>>,
        home: Option<crate::model::HomeSnapshot>,
    },
}

/// The state of playback on this computer, independent of Web API sign-in.
#[derive(Clone, Debug, PartialEq)]
pub enum LocalPlayback {
    /// Not authorized; local playback is unavailable but the app still works.
    Unavailable,
    /// The browser is open for the playback grant.
    Authorizing,
    /// Connecting the librespot engine.
    Connecting,
    /// Network path is unavailable; reconnection suspended pending reachability.
    WaitingForNetwork,
    /// This computer is a ready Spotify Connect device.
    Ready {
        device_id: String,
    },
    Failed(String),
}

/// Background services (the runtime, MPRIS, the tray) outlive individual
/// windows: the window is destroyed when it closes to the tray and created
/// again on demand. They therefore hold this handle, which repaints
/// whichever window exists, instead of an `egui::Context`.
pub use fastframe_shell::Waker;

/// The interface's handle to the runtime.
pub struct Backend {
    commands: mpsc::UnboundedSender<Command>,
    events: std::sync::mpsc::Receiver<Event>,
    art: ArtLoader,
    activity: Arc<NetActivity>,
    thread: Option<std::thread::JoinHandle<()>>,
    offline: bool,
    #[cfg(test)]
    network_statuses: std::sync::Mutex<Vec<bool>>,
    #[cfg(test)]
    playlist_item_requests: std::sync::Mutex<Vec<(String, u32, u64)>>,
    #[cfg(test)]
    playlist_sample_requests: std::sync::Mutex<Vec<(String, u32, u64)>>,
    #[cfg(test)]
    playlist_add_requests: std::sync::Mutex<Vec<ApiRequest>>,
    #[cfg(test)]
    remote_play_requests: std::sync::Mutex<Vec<ApiRequest>>,
    #[cfg(test)]
    remote_shuffle_requests: std::sync::Mutex<Vec<ApiRequest>>,
    #[cfg(test)]
    transfer_requests: std::sync::Mutex<Vec<ApiRequest>>,
    #[cfg(test)]
    queue_requests: std::sync::Mutex<Vec<ApiRequest>>,
    #[cfg(test)]
    queued_tracks: std::sync::Mutex<Vec<String>>,
    #[cfg(test)]
    player_commands: std::sync::Mutex<Vec<PlayerCommand>>,
    #[cfg(test)]
    album_type_requests: std::sync::Mutex<Vec<Vec<String>>>,
    #[cfg(test)]
    home_episode_requests: std::sync::Mutex<Vec<(Vec<String>, u64)>>,
    #[cfg(test)]
    library_cache_stores: std::sync::Mutex<Vec<crate::account_cache::Cache<Vec<Playlist>>>>,
    #[cfg(test)]
    playlist_row_removals: std::sync::Mutex<Vec<(String, Vec<session_reads::RemovedRow>)>>,
    #[cfg(test)]
    home_cache_stores:
        std::sync::Mutex<Vec<crate::account_cache::Cache<crate::model::HomeSnapshot>>>,
}

impl Backend {
    pub fn spawn(
        dirs: AppDirs,
        engine_config: EngineConfig,
        web_client_id: Option<String>,
        waker: Waker,
        restore_sign_in: bool,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("spotifast-runtime")
            .enable_all()
            .build()
            .expect("unable to start the async runtime");
        let http = if restore_sign_in {
            Http::unavailable("Restoring proxy settings".into())
        } else {
            Http::from_proxy(&engine_config.proxy).unwrap_or_else(|error| {
                let _ = event_tx.send(Event::Error(format!(
                    "Network configuration failed: {error}"
                )));
                Http::unavailable(error)
            })
        };
        let art = ArtLoader::new(http.clone(), runtime.handle().clone(), dirs.art_cache_dir());
        let activity = Arc::new(NetActivity::default());

        let worker_activity = Arc::clone(&activity);
        let worker_art = art.clone();
        let worker_commands = command_tx.clone();
        let thread = std::thread::Builder::new()
            .name("spotifast-backend".to_string())
            .spawn(move || {
                runtime.block_on(async move {
                    let mut worker = Worker::new(
                        dirs,
                        engine_config,
                        web_client_id,
                        http,
                        worker_art,
                        worker_activity,
                        event_tx,
                        worker_commands,
                        waker,
                    );
                    if restore_sign_in {
                        worker.restore_session();
                    }
                    worker.run(command_rx).await;
                });
                // Give librespot's own threads a moment to release the audio device.
                runtime.shutdown_timeout(Duration::from_secs(2));
            })
            .expect("unable to start the backend thread");

        Self {
            commands: command_tx,
            events: event_rx,
            art,
            activity,
            thread: Some(thread),
            offline: false,
            #[cfg(test)]
            network_statuses: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            playlist_item_requests: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            playlist_sample_requests: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            playlist_add_requests: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            remote_play_requests: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            remote_shuffle_requests: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            transfer_requests: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            queue_requests: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            queued_tracks: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            player_commands: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            album_type_requests: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            home_episode_requests: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            library_cache_stores: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            playlist_row_removals: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            home_cache_stores: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Live network activity, for the interface's busy indicator.
    pub fn activity(&self) -> &NetActivity {
        &self.activity
    }

    /// Stops Spotify-bound commands from leaving the process; artwork and
    /// shutdown still work. Used by the demo mode and by headless tests.
    #[cfg_attr(not(any(test, feature = "demo")), allow(dead_code))]
    pub fn set_offline(&mut self, offline: bool) {
        self.offline = offline;
    }

    pub fn send(&self, command: Command) {
        #[cfg(test)]
        if let Command::StoreLibraryCache(cache) = &command {
            self.library_cache_stores
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(cache.clone());
        }
        #[cfg(test)]
        if let Command::RemovePlaylistRows { playlist_id, rows } = &command {
            self.playlist_row_removals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((playlist_id.clone(), rows.clone()));
        }
        #[cfg(test)]
        if let Command::StoreHomeCache(cache) = &command {
            self.home_cache_stores
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(cache.clone());
        }
        if self.offline
            && !matches!(
                command,
                Command::Accent { .. }
                    | Command::Shutdown
                    | Command::CheckForUpdates { .. }
                    | Command::InspectUpdate
                    | Command::DownloadUpdate { .. }
                    | Command::InstallUpdate { .. }
            )
        {
            return;
        }
        let _ = self.commands.send(command);
    }

    /// Construct the native dialog on the UI thread, then await and read it
    /// on the runtime. AppKit requires its window lookup on the main thread.
    pub fn choose_playlist_cover(&self, id: String, request: u64) {
        if self.offline {
            return;
        }
        let selected = rfd::AsyncFileDialog::new()
            .set_title("Choose playlist cover")
            .add_filter("JPEG or PNG image", &["jpg", "jpeg", "png"])
            .pick_file();
        self.send(Command::ChoosePlaylistCover {
            id,
            request,
            selected: Box::pin(selected),
        });
    }

    /// Construct the folder dialog on the UI thread, like the cover's.
    pub fn choose_local_files_folder(&self) {
        if self.offline {
            return;
        }
        let selected = rfd::AsyncFileDialog::new()
            .set_title("Choose the local files folder")
            .pick_folder();
        self.send(Command::ChooseLocalFilesFolder {
            selected: Box::pin(selected),
        });
    }

    pub fn api(&self, request: ApiRequest) {
        #[cfg(test)]
        if matches!(
            request,
            ApiRequest::AlbumQueueTracks { .. }
                | ApiRequest::AddToQueue { .. }
                | ApiRequest::AddManyToQueue { .. }
        ) {
            self.queue_requests.lock().unwrap().push(request.clone());
        }
        #[cfg(test)]
        if matches!(
            request,
            ApiRequest::Remote {
                action: RemoteAction::Play,
                ..
            } | ApiRequest::ShufflePlay { .. }
        ) {
            self.remote_play_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
        }
        #[cfg(test)]
        if matches!(
            request,
            ApiRequest::Remote {
                action: RemoteAction::Shuffle,
                ..
            }
        ) {
            self.remote_shuffle_requests
                .lock()
                .unwrap()
                .push(request.clone());
        }
        #[cfg(test)]
        if matches!(request, ApiRequest::Transfer { .. }) {
            self.transfer_requests.lock().unwrap().push(request.clone());
        }
        #[cfg(test)]
        if matches!(
            request,
            ApiRequest::AddToPlaylist { .. }
                | ApiRequest::CheckPlaylistDuplicates { .. }
                | ApiRequest::UpdatePlaylist { .. }
        ) {
            self.playlist_add_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
        }
        #[cfg(test)]
        if let ApiRequest::PlaylistItems {
            id,
            offset,
            generation,
        } = &request
        {
            self.playlist_item_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((id.clone(), *offset, *generation));
        }
        #[cfg(test)]
        if let ApiRequest::PlaylistSample {
            id,
            offset,
            generation,
        } = &request
        {
            self.playlist_sample_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((id.clone(), *offset, *generation));
        }
        #[cfg(test)]
        if let ApiRequest::HomeEpisodes { shows, generation } = &request {
            self.home_episode_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((
                    shows.iter().map(|show| show.id.clone()).collect(),
                    *generation,
                ));
        }
        self.send(Command::Api(request));
    }

    #[cfg(test)]
    pub(crate) fn take_playlist_row_removals(
        &self,
    ) -> Vec<(String, Vec<session_reads::RemovedRow>)> {
        std::mem::take(
            &mut *self
                .playlist_row_removals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    #[cfg(test)]
    pub(crate) fn take_library_cache_stores(
        &self,
    ) -> Vec<crate::account_cache::Cache<Vec<Playlist>>> {
        std::mem::take(
            &mut *self
                .library_cache_stores
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    #[cfg(test)]
    pub(crate) fn take_home_cache_stores(
        &self,
    ) -> Vec<crate::account_cache::Cache<crate::model::HomeSnapshot>> {
        std::mem::take(
            &mut *self
                .home_cache_stores
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    #[cfg(test)]
    pub(crate) fn take_home_episode_requests(&self) -> Vec<(Vec<String>, u64)> {
        std::mem::take(
            &mut *self
                .home_episode_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    #[cfg(test)]
    pub fn take_playlist_item_requests(&self) -> Vec<(String, u32, u64)> {
        std::mem::take(
            &mut *self
                .playlist_item_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    #[cfg(test)]
    pub fn take_playlist_sample_requests(&self) -> Vec<(String, u32, u64)> {
        std::mem::take(
            &mut *self
                .playlist_sample_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    #[cfg(test)]
    pub fn take_playlist_add_requests(&self) -> Vec<ApiRequest> {
        std::mem::take(
            &mut *self
                .playlist_add_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    #[cfg(test)]
    pub fn take_remote_play_requests(&self) -> Vec<ApiRequest> {
        std::mem::take(
            &mut *self
                .remote_play_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    #[cfg(test)]
    pub fn take_transfer_requests(&self) -> Vec<ApiRequest> {
        std::mem::take(&mut *self.transfer_requests.lock().unwrap())
    }

    #[cfg(test)]
    pub fn take_remote_shuffle_requests(&self) -> Vec<ApiRequest> {
        std::mem::take(&mut *self.remote_shuffle_requests.lock().unwrap())
    }

    #[cfg(test)]
    pub(crate) fn take_queue_requests(&self) -> Vec<ApiRequest> {
        std::mem::take(&mut *self.queue_requests.lock().unwrap())
    }

    #[cfg(test)]
    pub(crate) fn take_queued_tracks(&self) -> Vec<String> {
        std::mem::take(&mut *self.queued_tracks.lock().unwrap())
    }

    #[cfg(test)]
    pub(crate) fn take_player_commands(&self) -> Vec<PlayerCommand> {
        std::mem::take(&mut *self.player_commands.lock().unwrap())
    }

    pub fn player(&self, command: PlayerCommand) {
        #[cfg(test)]
        self.player_commands.lock().unwrap().push(command.clone());
        #[cfg(test)]
        if let PlayerCommand::AddToQueue(uri) = &command {
            self.queued_tracks.lock().unwrap().push(uri.clone());
        }
        self.send(Command::Player(command));
    }

    /// Whether the system says the internet can be reached. Without it the
    /// engine waits for it to come back instead of retrying.
    pub fn network_status(&self, reachable: bool) {
        #[cfg(test)]
        self.network_statuses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(reachable);
        self.send(Command::NetworkStatus(reachable));
    }

    #[cfg(test)]
    pub fn take_network_statuses(&self) -> Vec<bool> {
        std::mem::take(
            &mut *self
                .network_statuses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    pub(crate) fn album_types(&self, uris: Vec<String>) {
        #[cfg(test)]
        self.album_type_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(uris.clone());
        self.send(Command::AlbumTypes(uris));
    }

    #[cfg(test)]
    pub(crate) fn take_album_type_requests(&self) -> Vec<Vec<String>> {
        std::mem::take(
            &mut *self
                .album_type_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    pub fn art(&self) -> &ArtLoader {
        &self.art
    }

    pub fn shutdown(&mut self) {
        self.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AlbumTypeRequest {
    uri: String,
    session_generation: u64,
    engine_generation: u64,
}

#[derive(Default)]
struct AlbumTypeLookup {
    session_generation: u64,
    engine_generation: u64,
    pending: VecDeque<String>,
    seen: HashSet<String>,
    active: Option<AlbumTypeRequest>,
}

impl AlbumTypeLookup {
    fn enqueue(&mut self, signed_in: bool, premium: Option<bool>, uris: Vec<String>) {
        if !signed_in || premium == Some(false) {
            return;
        }
        for uri in uris {
            if self.pending.len() + usize::from(self.active.is_some()) >= MAX_PENDING_ALBUM_TYPES {
                break;
            }
            if self.seen.insert(uri.clone()) {
                self.pending.push_back(uri);
            }
        }
    }

    fn next(&mut self) -> Option<AlbumTypeRequest> {
        if self.active.is_some() {
            return None;
        }
        let request = AlbumTypeRequest {
            uri: self.pending.pop_front()?,
            session_generation: self.session_generation,
            engine_generation: self.engine_generation,
        };
        self.active = Some(request.clone());
        Some(request)
    }

    fn finish(&mut self, request: &AlbumTypeRequest) -> bool {
        if self.active.as_ref() != Some(request) {
            return false;
        }
        self.active = None;
        true
    }

    fn retire_engine(&mut self) {
        self.engine_generation = self.engine_generation.wrapping_add(1);
        self.active = None;
    }

    fn requeue_active_for_new_engine(&mut self) {
        self.engine_generation = self.engine_generation.wrapping_add(1);
        if let Some(request) = self.active.take() {
            self.pending.push_front(request.uri);
        }
    }

    fn clear_engine_work(&mut self) {
        self.retire_engine();
        self.pending.clear();
        self.seen.clear();
    }

    fn reset_session(&mut self) {
        self.session_generation = self.session_generation.wrapping_add(1);
        self.clear_engine_work();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlaylistHeaderRequest {
    id: String,
    session_generation: u64,
    engine_generation: u64,
}

/// Library playlists waiting for the streaming session to read their
/// headers, read one at a time. A read still running when its engine is
/// replaced goes back to the front of the queue for the new engine, and its
/// answer from the retired one is ignored.
#[derive(Default)]
struct PlaylistHeaderLookup {
    engine_generation: u64,
    pending: VecDeque<String>,
    active: Option<PlaylistHeaderRequest>,
}

impl PlaylistHeaderLookup {
    fn enqueue(&mut self, ids: Vec<String>) {
        for id in ids {
            let active = self.active.as_ref().is_some_and(|request| request.id == id);
            if !active && !self.pending.contains(&id) {
                self.pending.push_back(id);
            }
        }
    }

    fn next(&mut self, session_generation: u64) -> Option<PlaylistHeaderRequest> {
        if self.active.is_some() {
            return None;
        }
        let request = PlaylistHeaderRequest {
            id: self.pending.pop_front()?,
            session_generation,
            engine_generation: self.engine_generation,
        };
        self.active = Some(request.clone());
        Some(request)
    }

    fn finish(&mut self, request: &PlaylistHeaderRequest) -> bool {
        if self.active.as_ref() != Some(request) {
            return false;
        }
        self.active = None;
        true
    }

    fn requeue(&mut self, id: String) {
        if !self.pending.contains(&id) {
            self.pending.push_front(id);
        }
    }

    fn requeue_active_for_new_engine(&mut self) {
        self.engine_generation = self.engine_generation.wrapping_add(1);
        if let Some(request) = self.active.take() {
            self.requeue(request.id);
        }
    }

    fn clear_engine_work(&mut self) {
        self.engine_generation = self.engine_generation.wrapping_add(1);
        self.active = None;
        self.pending.clear();
    }
}

struct Worker {
    dirs: AppDirs,
    credentials: CredentialStore,
    web_tokens: [Option<Arc<WebTokens>>; 2],
    playback_grant: Option<Credentials>,
    /// Credentials retained from a connection attempt while offline before
    /// a durable grant has been stored, so recovery can reconnect when the
    /// network returns.
    pending_playback_grant: Option<Credentials>,
    restore_pending: [bool; 3],
    restoring_proxy: bool,
    waiting_for_proxy: VecDeque<Command>,
    spotify_restore_started: bool,
    proxy_revision: u64,
    authorization_attempt: u64,
    session: watch::Sender<u64>,
    engine_config: EngineConfig,
    /// The resolved proxy handed to the current or in-flight engine connection.
    engine_proxy: Option<reqwest::Url>,
    web_client_id: Option<String>,
    http: Http,
    /// Friends' names and avatars and their playlists' names, kept between
    /// Friend activity polls.
    friend_names: Arc<tokio::sync::Mutex<session_reads::FriendNames>>,
    api: Arc<ApiGateway>,
    made_for_you: Arc<crate::made_for_you::MadeForYou>,
    new_releases: Arc<crate::new_releases::NewReleases>,
    connect: Arc<crate::sonos::Controller>,
    /// Reopening or refreshing the picker reuses an in-flight browse.
    receiver_discovery: Option<tokio::task::JoinHandle<()>>,
    background_session: Arc<tokio::sync::Semaphore>,
    art: ArtLoader,
    events: std::sync::mpsc::Sender<Event>,
    commands: mpsc::UnboundedSender<Command>,
    waker: Waker,
    engine: Option<Arc<Engine>>,
    /// What the engine is heard at, so the engine that replaces it starts
    /// there. `engine_config` alone knows only the level the app launched
    /// with.
    heard: Option<Heard>,
    /// A rootlist fetch asked for before the engine existed, to run once it
    /// does. The rootlist carries invitation edit permissions, which no
    /// other request reports.
    rootlist_pending: bool,
    album_type_lookup: AlbumTypeLookup,
    /// Saved shows waiting for the streaming session to say which are audiobooks.
    audiobook_lookup: BTreeSet<String>,
    /// Library playlists the Web API listed blank, waiting for the streaming
    /// session to read their headers.
    playlist_header_lookup: PlaylistHeaderLookup,
    /// Radios asked for before the streaming session was ready, by seed.
    radio_waiting: BTreeMap<String, u64>,
    prerelease_waiting: Option<(String, u64)>,
    /// True while a playback grant or engine connection is in flight, so a
    /// second attempt does not pile up.
    engine_busy: bool,
    search_tasks: Vec<tokio::task::AbortHandle>,
    /// A user changed engine-affecting settings while the current connection
    /// attempt was in flight. Its result is stale and must not be installed.
    engine_restart_pending: bool,
    signed_in: bool,
    /// The plan, once the Web API has answered.
    premium: Option<bool>,
    cancel_signin: Option<watch::Sender<bool>>,
    authorizing_source: Option<ApiSource>,
    pending_authorization: Option<ApiSource>,
    reconnects: Vec<Instant>,
    /// What the engine was playing when it went down, to load again once
    /// the next one is up.
    resume: Option<PlaybackResume>,
    /// A pickup in flight: the load to repeat and how often it was tried.
    resume_verify: Option<(PlaybackResume, u8)>,
    /// The player of an engine whose session dropped, still playing what it
    /// had fetched, for the next engine to carry on with (#268).
    kept_player: Option<KeptPlayer>,
    network_reachable: bool,
    waiting_for_network: bool,
    engine_attempt: u64,
    /// Failed engine connections in a row that the saved grant is retrying.
    engine_retries: usize,
    /// Bumped to drop a scheduled retry that is no longer wanted.
    engine_retry_generation: u64,
    /// The engine connection in flight is a background retry: it does not
    /// announce itself, and its failure keeps the error already shown.
    engine_retry_quiet: bool,
    /// The user asked to set up playback and the saved grant is being tried
    /// first. Only Spotify rejecting it opens the browser.
    authorize_if_rejected: bool,
}

/// A failed Connect probe setup must still reconcile departed provisional rows.
fn receiver_snapshot(
    receivers: Vec<crate::zeroconf::Receiver>,
    resolve: impl FnOnce(
        Vec<crate::zeroconf::Receiver>,
    ) -> anyhow::Result<Vec<crate::zeroconf::Receiver>>,
) -> Vec<crate::zeroconf::Receiver> {
    let cast = receivers
        .iter()
        .filter(|receiver| receiver.cast)
        .cloned()
        .collect();
    match resolve(receivers) {
        Ok(receivers) => receivers,
        Err(error) => {
            log::debug!("no receivers found on the network: {error}");
            cast
        }
    }
}

impl Worker {
    /// Initialize account-scoped dispatch state, Home reader and session background budget.
    #[allow(clippy::too_many_arguments)]
    fn new(
        dirs: AppDirs,
        engine_config: EngineConfig,
        web_client_id: Option<String>,
        http: Http,
        art: ArtLoader,
        activity: Arc<NetActivity>,
        events: std::sync::mpsc::Sender<Event>,
        commands: mpsc::UnboundedSender<Command>,
        waker: Waker,
    ) -> Self {
        Self {
            #[cfg(not(test))]
            credentials: CredentialStore::new(dirs.clone()),
            #[cfg(test)]
            credentials: CredentialStore::in_memory(dirs.clone()),
            web_tokens: [None, None],
            playback_grant: None,
            pending_playback_grant: None,
            restore_pending: [false; 3],
            restoring_proxy: false,
            waiting_for_proxy: VecDeque::new(),
            spotify_restore_started: false,
            proxy_revision: 0,
            authorization_attempt: 0,
            session: watch::channel(0).0,
            dirs,
            engine_config,
            engine_proxy: None,
            web_client_id,
            api: Arc::new(ApiGateway::new(http.clone(), activity)),
            made_for_you: Arc::new(crate::made_for_you::MadeForYou::new(http.clone())),
            new_releases: Arc::new(crate::new_releases::NewReleases::new(http.clone())),
            connect: Arc::default(),
            receiver_discovery: None,
            background_session: Arc::new(tokio::sync::Semaphore::new(4)),
            http,
            friend_names: Arc::default(),
            art,
            events,
            commands,
            waker,
            engine: None,
            heard: None,
            rootlist_pending: false,
            album_type_lookup: AlbumTypeLookup::default(),
            audiobook_lookup: BTreeSet::new(),
            playlist_header_lookup: PlaylistHeaderLookup::default(),
            radio_waiting: BTreeMap::new(),
            prerelease_waiting: None,
            engine_busy: false,
            search_tasks: Vec::new(),
            engine_restart_pending: false,
            signed_in: false,
            premium: None,
            cancel_signin: None,
            authorizing_source: None,
            pending_authorization: None,
            reconnects: Vec::new(),
            resume: None,
            resume_verify: None,
            kept_player: None,
            network_reachable: true,
            waiting_for_network: false,
            engine_attempt: 0,
            engine_retries: 0,
            engine_retry_generation: 0,
            engine_retry_quiet: false,
            authorize_if_rejected: false,
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
        self.waker.wake();
    }

    /// Build before replacing either transport configuration. A rejected
    /// change leaves the existing connection in place and is reported to the UI.
    fn apply_proxy(&mut self, proxy: ProxyConfig) -> Result<bool, String> {
        let client = crate::http::build_client(&proxy)?;
        let restart = self.engine_proxy != proxy.librespot_url();
        self.http.replace(client);
        self.engine_config.proxy = proxy;
        Ok(restart)
    }

    fn change_proxy(&mut self, request: u64, proxy: ProxyConfig, sign_in: bool) {
        let result = self.apply_proxy(proxy.clone());
        if result.as_ref().is_ok_and(|restart| *restart) {
            self.replace_engine();
        }
        let applied = result.is_ok();
        if applied {
            self.proxy_revision = request;
        }
        self.emit(Event::ProxyApplied {
            request,
            config: proxy.clone(),
            result,
        });
        if applied {
            self.persist_proxy_password(&proxy);
            self.finish_proxy_restore();
            if sign_in {
                self.sign_in();
            }
        }
    }

    fn persist_proxy_password(&mut self, proxy: &ProxyConfig) {
        if !matches!(proxy, ProxyConfig::Http(_) | ProxyConfig::Socks(_)) {
            // Off/System retain the saved manual password for later use.
            return;
        }
        let events = self.events.clone();
        let waker = self.waker.clone();
        let pending = if let Some(password) = proxy.password_record() {
            self.credentials.invalidate(CredentialSlot::Proxy);
            let lease = self.credentials.lease(CredentialSlot::Proxy);
            let saving = lease.save(StoredGrant::Proxy(password));
            (lease, saving, true)
        } else {
            if let Err(error) = self.credentials.revoke(CredentialSlot::Proxy) {
                self.emit(Event::ProxyStorageFailed(error));
                return;
            }
            // The durable revocation marker already prevents a failed delete
            // from restoring an old password. The UI can now scrub legacy JSON.
            self.emit(Event::ProxyPasswordStored);
            let lease = self.credentials.lease(CredentialSlot::Proxy);
            let deleting = lease.delete();
            (lease, deleting, false)
        };
        tokio::spawn(async move {
            let (lease, operation, saving) = pending;
            let result = operation.await;
            if !lease.current() {
                return;
            }
            match result {
                Ok(()) if saving => {
                    let _ = events.send(Event::ProxyPasswordStored);
                }
                Ok(()) => {}
                Err(error) if error != crate::credentials::Error::Stale => {
                    let _ = events.send(Event::ProxyStorageFailed(error));
                }
                Err(_) => {}
            }
            waker.wake();
        });
    }

    fn on_proxy_restored(
        &mut self,
        lease: CredentialLease,
        result: Result<crate::credentials::Loaded, crate::credentials::Error>,
    ) {
        if !lease.current() {
            return;
        }
        let (password, protected) = match result {
            Ok(loaded) => {
                if let Some(error) = loaded.warning {
                    self.emit(Event::ProxyStorageFailed(error));
                }
                let password = match loaded.grant {
                    Some(StoredGrant::Proxy(password)) => Some(password),
                    _ => None,
                };
                (password, loaded.warning.is_none())
            }
            Err(error) => {
                self.emit(Event::ProxyStorageFailed(error));
                (None, false)
            }
        };
        if self.proxy_revision == 0 {
            let mut config = self.engine_config.proxy.clone();
            if let Some(password) = &password {
                config.restore_password(password);
            }
            if let Err(error) = self.apply_proxy(config.clone()) {
                self.http.block(error.clone());
                config = ProxyConfig::Invalid(error.clone());
                self.engine_config.proxy = config.clone();
                self.emit(Event::Error(format!(
                    "Network configuration failed: {error}"
                )));
            }
            self.emit(Event::ProxyRestored { config, password });
        }
        if protected {
            self.emit(Event::ProxyPasswordStored);
        }
        self.finish_proxy_restore();
    }

    fn finish_proxy_restore(&mut self) {
        if !std::mem::take(&mut self.restoring_proxy) {
            return;
        }
        self.restore_spotify_grants();
        for command in self.waiting_for_proxy.drain(..) {
            let _ = self.commands.send(command);
        }
    }

    async fn run(&mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        let (cache_writes, cache_write_receiver) = mpsc::channel(1);
        let cache_writer = tokio::spawn(store_playlist_caches(
            cache_write_receiver,
            self.events.clone(),
            self.waker.clone(),
        ));
        while let Some(command) = commands.recv().await {
            if self.restoring_proxy
                && !matches!(
                    &command,
                    Command::ProxyRestored { .. }
                        | Command::ApplyProxy { .. }
                        | Command::SignIn { .. }
                        | Command::SignOut
                        | Command::CancelSignIn
                        | Command::Shutdown
                )
            {
                self.waiting_for_proxy.push_back(command);
                continue;
            }
            match command {
                Command::ListOutputDevices => {
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    // Listing asks the sound system about every device,
                    // which can take a moment; nothing is opened.
                    tokio::task::spawn_blocking(move || {
                        let devices =
                            fastframe_audio::output_devices().map_err(|error| error.to_string());
                        let _ = events.send(Event::OutputDevices(devices));
                        waker.wake();
                    });
                }
                Command::Autostart(request) => {
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    tokio::task::spawn_blocking(move || {
                        let autostart = autostart();
                        let result = match request {
                            AutostartRequest::Read => Ok(autostart.state()),
                            AutostartRequest::Set(start) => {
                                autostart.set(start).map_err(|error| error.to_string())
                            }
                        };
                        let _ = events.send(Event::Autostart { request, result });
                        waker.wake();
                    });
                }
                Command::OpenThemesFolder => {
                    let directory = self.dirs.config.join("themes");
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(error) = std::fs::create_dir_all(&directory)
                            .and_then(|()| crate::opener::open(&directory))
                        {
                            let _ = events.send(Event::Error(format!(
                                "Couldn't open the themes folder: {error}"
                            )));
                            waker.wake();
                        }
                    });
                }
                Command::ProxyRestored { lease, result } => self.on_proxy_restored(lease, result),
                Command::CredentialsRestored {
                    slot,
                    lease,
                    result,
                } => self.on_credentials_restored(slot, lease, result),
                Command::CheckPlaylistCover {
                    id,
                    request,
                    cover,
                    images,
                } => {
                    let art = self.art.clone();
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    let mut session = self.session.subscribe();
                    tokio::spawn(async move {
                        let result = tokio::select! {
                            _ = session.changed() => return,
                            result = async {
                                let url = crate::api::models::pick_image(&images, u32::MAX)
                                    .ok_or_else(|| "No playlist artwork yet.".to_string())?;
                                let bytes = art.fetch(url).await?;
                                tokio::task::spawn_blocking(move || cover.matches_remote(&bytes))
                                    .await.map_err(|_| "Couldn't check playlist artwork.".to_string())?
                            } => result,
                        };
                        let _ = events.send(Event::PlaylistCoverChecked {
                            id,
                            request,
                            images,
                            result,
                        });
                        waker.wake();
                    });
                }
                Command::ChooseLocalFilesFolder { selected } => {
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    tokio::spawn(async move {
                        let folder = selected.await.map(|folder| folder.path().to_path_buf());
                        let _ = events.send(Event::LocalFilesFolderChosen(folder));
                        waker.wake();
                    });
                }
                Command::ScanLocalFiles(folder) => {
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    tokio::spawn(async move {
                        let scanned = folder.clone();
                        let Ok(uris) =
                            tokio::task::spawn_blocking(move || local_file_uris(&scanned)).await
                        else {
                            return;
                        };
                        let _ = events.send(Event::LocalFiles {
                            folder,
                            uris: Arc::new(uris),
                        });
                        waker.wake();
                    });
                }
                Command::ChoosePlaylistCover {
                    id,
                    request,
                    selected,
                } => {
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    tokio::spawn(async move {
                        let selected = selected.await;
                        let result = match selected {
                            None => Ok(None),
                            Some(file) => tokio::task::spawn_blocking(move || {
                                crate::playlist_cover::read(file.path()).map(Some)
                            })
                            .await
                            .unwrap_or_else(|_| {
                                Err("Couldn't prepare that image. Try another file.".into())
                            }),
                        };
                        let _ = events.send(Event::PlaylistCoverChosen {
                            id,
                            request,
                            result,
                        });
                        waker.wake();
                    });
                }
                Command::Shutdown => break,
                Command::SignIn { request, config } => self.change_proxy(request, config, true),
                Command::CancelSignIn => {
                    self.authorization_attempt += 1;
                    if let Some(cancel) = self.cancel_signin.take() {
                        self.credentials.invalidate(
                            self.authorizing_source
                                .map_or(CredentialSlot::Playback, web_slot),
                        );
                        let _ = cancel.send(true);
                    }
                    if let Some(source) = self.authorizing_source.take()
                        && matches!(self.api.state(source), SessionState::Authorizing)
                    {
                        self.api.clear(source);
                    }
                    self.pending_authorization = None;
                }
                Command::SignOut => {
                    self.sign_out();
                    // The account caches go in command order, so nothing a
                    // sign-in after this stores goes with them. This loop
                    // writes the snapshots, so they go now. The playlist
                    // pages queue behind a page still being written and
                    // ahead of the next account's.
                    for dir in self.dirs.account_snapshot_dirs() {
                        log_cache_removal(&dir, tokio::fs::remove_dir_all(&dir).await);
                    }
                    let purge = PlaylistCacheJob::Purge(self.dirs.playlist_cache_dir());
                    if cache_writes.send(purge).await.is_err() {
                        log::warn!("unable to remove the playlist caches: writer unavailable");
                    }
                }
                Command::AuthorizePlayback => self.authorize_playback(),
                Command::RestartEngine(mut config) => {
                    // Audio settings must not revert a proxy change whose UI
                    // acknowledgement was still in flight when this was clicked.
                    config.proxy = self.engine_config.proxy.clone();
                    self.engine_config = config;
                    self.replace_engine();
                }
                Command::ApplyProxy { request, config } => {
                    self.change_proxy(request, config, false)
                }
                Command::Player(command) => {
                    if matches!(&command, PlayerCommand::Load(_)) {
                        self.resume = None;
                    }
                    match &self.engine {
                        // Until the new session picks the song up, it has
                        // nothing to pause: the player does it (#268).
                        Some(engine)
                            if command == PlayerCommand::Toggle
                                && engine.carrying()
                                && engine.keep_player().toggle() => {}
                        Some(engine) => {
                            if let Err(error) = engine.command(command) {
                                self.emit(Event::Error(format!("Playback error: {error}")));
                            }
                        }
                        // While the session reconnects, the song it left
                        // pauses and plays on its own player (#268).
                        None if command == PlayerCommand::Toggle
                            && self.kept_player.as_ref().is_some_and(KeptPlayer::toggle) => {}
                        None => {
                            if matches!(&command, PlayerCommand::Load(_)) {
                                // Something else chosen: the old song stops.
                                self.stop_kept_player();
                            }
                            self.emit(Event::Error(
                                "Local playback isn't set up on this computer yet".into(),
                            ))
                        }
                    }
                }
                Command::Api(ApiRequest::Search { query, serial }) => self.search(query, serial),
                Command::Api(request) => {
                    self.dispatch(request);
                }
                Command::ApiFinished {
                    generation,
                    home_account,
                    response,
                    expired,
                    shared_lease,
                    personal_lease,
                } => {
                    if generation != *self.session.borrow() {
                        continue;
                    }
                    // Home can finish after a browser reauthorization without
                    // an explicit sign-out. Bind successful cards to both the
                    // account and the grants that existed at dispatch.
                    if let ApiResponse::MadeForYou { generation, .. } = response.as_ref() {
                        if home_account.is_none() || home_account != self.api.account() {
                            continue;
                        }
                        if !shared_lease.current() || !personal_lease.current() {
                            // The same account renewed a grant. Retry under its
                            // current leases so this shelf does not stay loading.
                            self.dispatch(ApiRequest::MadeForYou {
                                generation: *generation,
                                force: false,
                            });
                            continue;
                        }
                    }
                    if expired.iter().any(|source| {
                        if *source == ApiSource::Shared {
                            !shared_lease.current()
                        } else {
                            !personal_lease.current()
                        }
                    }) {
                        continue;
                    }
                    for source in expired {
                        self.forget_web_grant(source);
                        if source == ApiSource::Personal {
                            self.emit(Event::WebApp { client_id: None });
                        } else {
                            self.signed_in = false;
                            self.emit(Event::Auth(AuthStatus::Failed(
                                "Your Spotify sign-in expired. Please sign in again.".into(),
                            )));
                        }
                    }
                    if let ApiResponse::Me(Ok(user)) = response.as_ref()
                        && self.signed_in
                        && self.api.account().as_ref().map(|account| account.as_str())
                            == Some(user.id.as_str())
                    {
                        self.on_account_checked(
                            user.product.as_deref().map(|product| product == "premium"),
                        );
                    }
                    match response.as_ref() {
                        ApiResponse::Devices(Ok(devices)) => self.connect.observe_devices(devices),
                        ApiResponse::PlaybackState {
                            seq,
                            result: Ok(state),
                        } => {
                            self.connect.observe_playing(
                                *seq,
                                state.as_ref().and_then(|state| state.device.as_ref()),
                            );
                        }
                        _ => {}
                    }
                    if let ApiResponse::MadeForYou {
                        result, top_mixes, ..
                    } = response.as_ref()
                    {
                        for playlists in [result, top_mixes].into_iter().flatten() {
                            self.api.observe_playlists(playlists);
                        }
                    }
                    self.emit(Event::Api(response));
                }
                Command::Accent { url } => self.accent(url),
                Command::WebSignedIn {
                    source,
                    token,
                    lease,
                    attempt,
                } => {
                    if lease.current()
                        && self.authorizing_source == Some(source)
                        && self.authorization_attempt == attempt
                    {
                        self.on_web_signed_in(source, *token);
                    } else if self.authorization_attempt == attempt {
                        self.finish_authorization(source);
                    }
                }
                Command::WebVerified {
                    source,
                    token,
                    user,
                    lease,
                    attempt,
                } => {
                    if lease.current() {
                        self.on_web_verified(source, *token, *user);
                    } else if self.authorization_attempt == attempt {
                        self.finish_authorization(source);
                    }
                }
                Command::WebVerificationFailed {
                    source,
                    lease,
                    attempt,
                    error,
                } => {
                    if lease.current() {
                        self.on_web_verification_failed(source, error);
                    }
                    if self.authorization_attempt == attempt {
                        self.finish_authorization(source);
                    }
                }
                Command::PlaybackAuthorized {
                    access_token,
                    lease,
                    attempt,
                } => {
                    if lease.current() {
                        self.on_playback_authorized(access_token);
                    } else {
                        self.finish_playback_authorization(attempt);
                    }
                }
                Command::EngineConnected {
                    session_generation,
                    engine,
                    error,
                    lease,
                    attempt,
                } => {
                    if lease.current() && self.signed_in {
                        self.on_engine_connected(session_generation, *engine, error, attempt);
                    } else if let Some(engine) = *engine {
                        engine.shutdown();
                    }
                }
                Command::NetworkStatus(reachable) => {
                    self.network_reachable = reachable;
                    self.emit(Event::NetworkStatus(reachable));
                    if !reachable {
                        self.waiting_for_network = true;
                    } else if self.engine.is_some() {
                        self.waiting_for_network = false;
                    } else if self.waiting_for_network && !self.engine_busy {
                        self.waiting_for_network = false;
                        self.resume_engine();
                    }
                }
                Command::SignInEnded { source, attempt } => {
                    if self.authorizing_source == Some(source)
                        && self.authorization_attempt == attempt
                    {
                        self.cancel_signin = None;
                        self.authorizing_source = None;
                        if matches!(self.api.state(source), SessionState::Authorizing) {
                            self.api.clear(source);
                        }
                        if let Some(pending) = self.pending_authorization.take() {
                            self.sign_in_source(pending);
                        }
                    }
                }
                Command::PlaybackAuthEnded { attempt } => {
                    self.finish_playback_authorization(attempt)
                }
                Command::Reconnect => self.reconnect_engine(),
                Command::RetryEngine { generation } => self.retry_engine(generation),
                Command::DiscoverReceivers => self.discover_receivers(),
                Command::ActivateReceiver(receiver) => self.activate_receiver(*receiver),
                Command::CheckForUpdates { manual, source } => {
                    self.check_for_updates(manual, source)
                }
                Command::InspectUpdate => {
                    let proxy = self.engine_config.proxy.clone();
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = crate::updates::updater(&proxy)
                            .map_err(|error| format!("{error:#}"))
                            .and_then(|updater| {
                                updater.installation().map_err(|error| error.to_string())
                            });
                        let _ = events.send(Event::UpdateSupport(result));
                        waker.wake();
                    });
                }
                Command::DownloadUpdate { release, source } => {
                    let proxy = self.engine_config.proxy.clone();
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = crate::updates::updater(&proxy)
                            .and_then(|updater| {
                                updater
                                    .with_source(source)
                                    .download(&release, |received, total| {
                                        let _ =
                                            events.send(Event::UpdateProgress { received, total });
                                        waker.wake();
                                    })
                            })
                            .map(Box::new)
                            .map_err(|error| format!("{error:#}"));
                        let _ = events.send(Event::UpdateDownloaded(result));
                        waker.wake();
                    });
                }
                Command::InstallUpdate {
                    prepared,
                    arguments,
                } => {
                    let proxy = self.engine_config.proxy.clone();
                    let events = self.events.clone();
                    let waker = self.waker.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = crate::updates::updater(&proxy)
                            .and_then(|updater| updater.handoff(*prepared, arguments))
                            .map_err(|error| format!("{error:#}"));
                        let _ = events.send(Event::UpdateInstalling(result));
                        waker.wake();
                    });
                }
                Command::Lyrics(request) => self.fetch_lyrics(*request),
                Command::Rootlist => self.fetch_rootlist(),
                Command::RootlistFinished { generation, result } => {
                    self.on_rootlist_finished(generation, result);
                }
                Command::VerifyResume => self.verify_resume(),
                Command::LoadPlaylistCache { id, generation } => {
                    self.load_playlist_cache(id, generation)
                }
                Command::StorePlaylistCache {
                    id,
                    generation,
                    snapshot,
                    rows,
                    total,
                    next_offset,
                } => {
                    if let Some(account) = self.api.account() {
                        let account_id = account.as_str().to_string();
                        let path = self
                            .dirs
                            .account_playlist_cache_dir(&account_id)
                            .join(format!("{id}.json"));
                        if let Err(error) =
                            cache_writes.try_send(PlaylistCacheJob::Write(PlaylistCacheWrite {
                                path,
                                account_id,
                                id,
                                generation,
                                snapshot,
                                rows,
                                total,
                                next_offset,
                            }))
                            && let Some(write) = error.into_inner().into_write()
                        {
                            log::warn!(
                                "unable to queue playlist cache {}: writer unavailable",
                                write.path.display()
                            );
                            self.emit(Event::PlaylistCacheStored {
                                account_id: write.account_id,
                                id: write.id,
                                generation: write.generation,
                                snapshot: write.snapshot,
                                success: false,
                            });
                        }
                    } else {
                        self.emit(Event::PlaylistCacheStored {
                            account_id: String::new(),
                            id,
                            generation,
                            snapshot,
                            success: false,
                        });
                    }
                }
                Command::UserNames(ids) => self.fetch_user_names(ids),
                Command::PlayCounts(uri) => self.fetch_play_counts(uri),
                Command::FriendActivity(generation) => self.fetch_friend_activity(generation),
                Command::LoadLikedSongsCache { generation } => {
                    if let Some(account) = self.api.account() {
                        let account_id = account.as_str().to_string();
                        let path = self.dirs.liked_songs_cache_file(&account_id);
                        let events = self.events.clone();
                        let waker = self.waker.clone();
                        tokio::spawn(async move {
                            let cache = crate::liked::read(&path, &account_id).await;
                            let _ = events.send(Event::LikedSongsCache {
                                account_id,
                                generation,
                                cache,
                            });
                            waker.wake();
                        });
                    }
                }
                Command::StoreLikedSongsCache(cache) => {
                    if self
                        .api
                        .account()
                        .is_some_and(|account| account.as_str() == cache.account_id)
                    {
                        let path = self.dirs.liked_songs_cache_file(&cache.account_id);
                        if let Err(error) = crate::liked::write(&path, &cache).await {
                            log::warn!("unable to store Liked Songs cache: {error}");
                        }
                    }
                }
                Command::LoadAccountCaches { generation } => {
                    if let Some(account) = self.api.account() {
                        let account_id = account.as_str().to_string();
                        let library = self.dirs.library_cache_file(&account_id);
                        let home = self.dirs.home_cache_file(&account_id);
                        let events = self.events.clone();
                        let waker = self.waker.clone();
                        tokio::spawn(async move {
                            let playlists = crate::account_cache::read(&library, &account_id).await;
                            let home = crate::account_cache::read(&home, &account_id).await;
                            let _ = events.send(Event::AccountCaches {
                                account_id,
                                generation,
                                playlists,
                                home,
                            });
                            waker.wake();
                        });
                    }
                }
                Command::StoreLibraryCache(cache) => {
                    if self
                        .api
                        .account()
                        .is_some_and(|account| account.as_str() == cache.account_id)
                    {
                        let path = self.dirs.library_cache_file(&cache.account_id);
                        if let Err(error) = crate::account_cache::write(&path, &cache).await {
                            log::warn!("unable to store the playlist list cache: {error}");
                        }
                    }
                }
                Command::StoreHomeCache(cache) => {
                    if self
                        .api
                        .account()
                        .is_some_and(|account| account.as_str() == cache.account_id)
                    {
                        let path = self.dirs.home_cache_file(&cache.account_id);
                        if let Err(error) = crate::account_cache::write(&path, &cache).await {
                            log::warn!("unable to store the Home cache: {error}");
                        }
                    }
                }
                Command::AlbumTypes(uris) => self.fetch_album_types(uris),
                Command::AudiobookShows(uris) => {
                    self.audiobook_lookup.extend(uris);
                    self.start_audiobook_lookup();
                }
                Command::PlaylistHeaders(ids) => {
                    self.playlist_header_lookup.enqueue(ids);
                    self.start_playlist_header_lookup();
                }
                Command::SetPrereleaseSaved {
                    uri,
                    saved,
                    generation,
                } => self.set_prerelease_saved(uri, saved, generation),
                Command::RemovePlaylistRows { playlist_id, rows } => {
                    self.remove_playlist_rows(playlist_id, rows)
                }
                Command::PrereleaseSaved {
                    session_generation,
                    uri,
                    saved,
                    generation,
                    result,
                } => {
                    if self.signed_in && session_generation == *self.session.borrow() {
                        self.emit(Event::PrereleaseSaved {
                            uri,
                            saved,
                            generation,
                            result,
                        });
                    }
                }
                Command::Prerelease { uri, generation } => {
                    self.prerelease_waiting = Some((uri, generation));
                    self.start_prerelease();
                }
                Command::PrereleaseResolved {
                    session_generation,
                    uri,
                    generation,
                    result,
                } => {
                    if self.signed_in && session_generation == *self.session.borrow() {
                        self.emit(Event::Prerelease {
                            uri,
                            generation,
                            result,
                        });
                    }
                }
                Command::Radio { seed, generation } => {
                    self.radio_waiting.insert(seed, generation);
                    self.start_radio();
                }
                Command::RadioResolved {
                    session_generation,
                    seed,
                    generation,
                    result,
                } => {
                    if self.signed_in && session_generation == *self.session.borrow() {
                        self.emit(Event::Radio {
                            seed,
                            generation,
                            result,
                        });
                    }
                }
                Command::AudiobookShowsResolved {
                    session_generation,
                    audiobooks,
                } => {
                    if self.signed_in && session_generation == *self.session.borrow() {
                        self.emit(Event::AudiobookShows(audiobooks));
                    }
                }
                Command::PlaylistHeaderResolved {
                    id,
                    session_generation,
                    engine_generation,
                    header,
                } => self.on_playlist_header_resolved(
                    PlaylistHeaderRequest {
                        id,
                        session_generation,
                        engine_generation,
                    },
                    header,
                ),
                Command::AlbumTypeResolved {
                    uri,
                    session_generation,
                    engine_generation,
                    result,
                } => self.on_album_type_resolved(
                    AlbumTypeRequest {
                        uri,
                        session_generation,
                        engine_generation,
                    },
                    result,
                ),
                Command::ConfigurePersonalWebApp(client_id) => {
                    self.configure_personal_web_app(client_id)
                }
            }
        }
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
        self.stop_kept_player();
        drop(cache_writes);
        let _ = cache_writer.await;
    }

    // ---- Web API sign-in --------------------------------------------------

    fn restore_session(&mut self) {
        self.restoring_proxy = true;
        let lease = self.credentials.lease(CredentialSlot::Proxy);
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let result = lease.load().await;
            let _ = commands.send(Command::ProxyRestored { lease, result });
        });
    }

    fn restore_spotify_grants(&mut self) {
        if std::mem::replace(&mut self.spotify_restore_started, true) {
            return;
        }
        self.restore_pending = [true; 3];
        self.api
            .set_state(ApiSource::Shared, SessionState::Authorizing);
        if self.web_client_id.is_some() {
            self.api
                .set_state(ApiSource::Personal, SessionState::Authorizing);
        }
        for slot in CredentialSlot::SPOTIFY {
            let lease = self.credentials.lease(slot);
            let commands = self.commands.clone();
            tokio::spawn(async move {
                let result = lease.load().await;
                let _ = commands.send(Command::CredentialsRestored {
                    slot,
                    lease,
                    result,
                });
            });
        }
    }

    fn on_credentials_restored(
        &mut self,
        slot: CredentialSlot,
        lease: CredentialLease,
        result: Result<crate::credentials::Loaded, crate::credentials::Error>,
    ) {
        if slot == CredentialSlot::Proxy {
            return;
        }
        if !lease.current() {
            return;
        }
        self.restore_pending[slot.index()] = false;
        let grant = match result {
            Ok(loaded) => {
                if let Some(error) = loaded.warning {
                    self.emit(Event::Error(error.to_string()));
                }
                loaded.grant
            }
            Err(error) => {
                self.emit(Event::Error(error.to_string()));
                None
            }
        };
        match grant {
            Some(StoredGrant::Playback(grant)) => {
                self.playback_grant = Some(grant);
                self.resume_engine();
            }
            Some(StoredGrant::Web(token)) => {
                let source = if slot == CredentialSlot::Shared {
                    ApiSource::Shared
                } else {
                    ApiSource::Personal
                };
                if source == ApiSource::Shared
                    || self.web_client_id.as_deref() == Some(token.client_id.as_str())
                {
                    if token.has_scopes(crate::auth::WEB_SCOPES) {
                        if !self.signed_in {
                            self.emit(Event::Auth(AuthStatus::Connecting));
                        }
                        self.on_web_signed_in(source, token);
                    } else {
                        // Signing in renews only the shared app's grant, so a
                        // personal app has to be authorized again where it was
                        // set up (#634).
                        let message = if source == ApiSource::Shared {
                            "Spotify permissions changed. Sign in again."
                        } else {
                            "Spotify permissions changed for your personal app. \
                             Authorize it again in Settings, under Account."
                        };
                        self.emit(Event::Error(message.into()));
                    }
                }
            }
            None | Some(StoredGrant::Proxy(_)) => {}
        }
        if slot == CredentialSlot::Playback {
            self.start_prerelease();
        }
        if slot != CredentialSlot::Playback && self.web_tokens[slot.index()].is_none() {
            self.api.clear(if slot == CredentialSlot::Shared {
                ApiSource::Shared
            } else {
                ApiSource::Personal
            });
        }
        if !self.restore_pending.iter().any(|pending| *pending)
            && !self.signed_in
            && self.web_tokens.iter().all(Option::is_none)
        {
            self.emit(Event::Auth(AuthStatus::SignedOut));
        }
    }

    fn storage_notice(
        &self,
        lease: CredentialLease,
    ) -> Arc<dyn Fn(crate::credentials::Error) + Send + Sync> {
        let events = self.events.clone();
        let waker = self.waker.clone();
        Arc::new(move |error| {
            if lease.current() && error != crate::credentials::Error::Stale {
                let _ = events.send(Event::Error(error.to_string()));
                waker.wake();
            }
        })
    }

    fn on_web_signed_in(&mut self, source: ApiSource, token: crate::auth::StoredToken) {
        let lease = self.credentials.lease(web_slot(source));
        let tokens = WebTokens::new(
            self.http.clone(),
            token.clone(),
            lease.clone(),
            source,
            self.storage_notice(lease.clone()),
        );
        self.web_tokens[web_slot(source).index()] = Some(tokens.clone());
        self.api
            .begin_verification(source, TokenProvider::Web(tokens));
        let client = self.api.verification_client(source);
        let gateway = Arc::clone(&self.api);
        let commands = self.commands.clone();
        let attempt = self.authorization_attempt;
        tokio::spawn(async move {
            let mut wait = Duration::from_secs(2);
            let error = loop {
                if !lease.current() {
                    let _ = commands.send(Command::SignInEnded { source, attempt });
                    return;
                }
                match crate::api::client::unseen(client.me()).await {
                    Ok(user) => {
                        let _ = commands.send(Command::WebVerified {
                            source,
                            token: Box::new(token),
                            user: Box::new(user),
                            lease: lease.clone(),
                            attempt,
                        });
                        return;
                    }
                    Err(error @ ApiError::SignInExpired { .. }) => break error,
                    Err(error) if error.status().is_some_and(|status| status < 500) => break error,
                    Err(error) => {
                        log::warn!("Spotify sign-in verification will retry: {error}");
                        tokio::time::sleep(wait).await;
                        wait = (wait * 2).min(Duration::from_secs(60));
                        if !matches!(gateway.state(source), SessionState::Authorizing) {
                            let _ = commands.send(Command::SignInEnded { source, attempt });
                            return;
                        }
                    }
                }
            };
            if !lease.current() {
                let _ = commands.send(Command::SignInEnded { source, attempt });
                return;
            }
            let _ = commands.send(Command::WebVerificationFailed {
                source,
                lease,
                attempt,
                error,
            });
        });
    }

    fn forget_web_grant(&mut self, source: ApiSource) {
        let slot = web_slot(source);
        if let Err(error) = self.credentials.revoke(slot) {
            self.emit(Event::Error(error.to_string()));
        }
        self.delete_stored_grant(slot);
        self.web_tokens[slot.index()] = None;
        self.api.clear(source);
    }

    fn on_web_verification_failed(&mut self, source: ApiSource, error: ApiError) {
        if matches!(error, ApiError::SignInExpired { .. }) {
            // A rejected refresh grant cannot restore a session next time.
            // Forget only this grant and ask for a fresh browser approval.
            self.forget_web_grant(source);
        } else {
            self.api.clear(source);
        }
        let message = match source {
            ApiSource::Shared => format!("Shared Spotify sign-in failed: {error}"),
            ApiSource::Personal => format!("Personal app authorization failed: {error}"),
        };
        let other_ready = match source {
            ApiSource::Shared => self.api.personal_ready(),
            ApiSource::Personal => matches!(
                self.api.state(ApiSource::Shared),
                SessionState::Ready { .. }
            ),
        };
        if source == ApiSource::Shared || !other_ready {
            self.signed_in = false;
            self.emit(Event::Auth(AuthStatus::Failed(message.clone())));
        }
        self.emit(Event::Error(message));
    }

    fn on_web_verified(&mut self, source: ApiSource, token: crate::auth::StoredToken, user: User) {
        if !matches!(self.api.state(source), SessionState::Authorizing)
            || source == ApiSource::Personal
                && self.web_client_id.as_deref() != Some(token.client_id.as_str())
        {
            return;
        }
        if let Err(error) = self.api.install(source, AccountId::new(user.id.clone())) {
            self.api.clear(source);
            if source == ApiSource::Shared {
                self.signed_in = false;
                self.emit(Event::Auth(AuthStatus::Failed(error.to_string())));
            }
            self.emit(Event::Error(error.to_string()));
            self.finish_authorization(source);
            return;
        }
        if let Some(tokens) = self.web_tokens[web_slot(source).index()].clone() {
            let notice = self.storage_notice(self.credentials.lease(web_slot(source)));
            tokio::spawn(async move {
                if let Err(error) = tokens.remember().await {
                    notice(error);
                }
            });
        }
        match source {
            ApiSource::Shared => {
                if !self.signed_in {
                    self.signed_in = true;
                    self.emit(Event::Auth(AuthStatus::Connected {
                        username: user.name().to_string(),
                    }));
                }
                self.emit(Event::Api(Box::new(ApiResponse::Me(Ok(user.clone())))));
                let premium = user.product.as_deref().map(|product| product == "premium");
                self.on_account_checked(premium);
            }
            ApiSource::Personal => {
                self.emit(Event::WebApp {
                    client_id: Some(token.client_id),
                });
                if !self.signed_in {
                    self.signed_in = true;
                    self.emit(Event::Auth(AuthStatus::Connected {
                        username: user.name().to_string(),
                    }));
                    self.emit(Event::Api(Box::new(ApiResponse::Me(Ok(user.clone())))));
                    let premium = user.product.as_deref().map(|product| product == "premium");
                    self.on_account_checked(premium);
                }
            }
        }
        self.finish_authorization(source);
    }

    fn finish_authorization(&mut self, source: ApiSource) {
        if self.authorizing_source != Some(source) {
            return;
        }
        self.cancel_signin = None;
        self.authorizing_source = None;
        if let Some(pending) = self.pending_authorization.take() {
            self.sign_in_source(pending);
        }
    }

    fn finish_playback_authorization(&mut self, attempt: u64) {
        if self.authorization_attempt == attempt {
            self.cancel_signin = None;
            if let Some(pending) = self.pending_authorization.take() {
                self.sign_in_source(pending);
            }
        }
    }

    fn sign_in(&mut self) {
        self.sign_in_source(ApiSource::Shared);
    }

    fn sign_in_source(&mut self, source: ApiSource) {
        let http = match self.http.client() {
            Ok(http) => http,
            Err(error) => {
                self.emit(Event::Error(error));
                return;
            }
        };
        if self.cancel_signin.is_some() {
            return;
        }
        let grant = match source {
            ApiSource::Shared => crate::auth::Grant::shared_web_api(),
            ApiSource::Personal => {
                let Some(client_id) = self.web_client_id.as_deref() else {
                    return;
                };
                match crate::auth::Grant::personal_web_api(client_id) {
                    Ok(grant) => grant,
                    Err(error) => {
                        self.emit(Event::Error(error.to_string()));
                        return;
                    }
                }
            }
        };
        let listener = match crate::auth::listen_for_redirect(grant.redirect_port) {
            Ok(listener) => listener,
            Err(error) => {
                if source == ApiSource::Shared {
                    self.emit(Event::Auth(AuthStatus::Failed(error.to_string())));
                } else {
                    self.emit(Event::Error(error.to_string()));
                }
                return;
            }
        };
        self.credentials.invalidate(web_slot(source));
        self.restore_pending[web_slot(source).index()] = false;
        let lease = self.credentials.lease(web_slot(source));
        self.authorization_attempt += 1;
        let attempt = self.authorization_attempt;
        let flow = crate::auth::begin(grant.clone());
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.cancel_signin = Some(cancel_tx);
        self.authorizing_source = Some(source);
        self.api.set_state(source, SessionState::Authorizing);
        if source == ApiSource::Shared {
            self.emit(Event::Auth(AuthStatus::WaitingForBrowser {
                url: flow.url.clone(),
            }));
        }
        let browser_url = flow.url.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = crate::opener::open(&browser_url) {
                log::warn!("unable to open a browser: {error}");
            }
        });
        let events = self.events.clone();
        let waker = self.waker.clone();
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let result = async {
                let code = crate::auth::wait_for_code_on_listener(listener, &flow.state, cancel_rx)
                    .await?;
                let response =
                    crate::auth::exchange_code(&http, &grant, &code, &flow.verifier).await?;
                crate::auth::StoredToken::from_response(&grant.client_id, response, None)
            }
            .await;
            match result {
                Ok(token) => {
                    let _ = commands.send(Command::WebSignedIn {
                        source,
                        token: Box::new(token),
                        lease: lease.clone(),
                        attempt,
                    });
                }
                Err(error) => {
                    if lease.current() && source == ApiSource::Shared {
                        let _ = events.send(Event::Auth(AuthStatus::SignedOut));
                    }
                    let message = error.to_string();
                    if lease.current() && !message.contains("cancelled") {
                        let _ = events.send(Event::Error(format!("Sign-in failed: {message}")));
                    }
                    waker.wake();
                    let _ = commands.send(Command::SignInEnded { source, attempt });
                }
            }
        });
    }

    fn configure_personal_web_app(&mut self, client_id: Option<String>) {
        let authorization_in_flight = if let Some(cancel) = self.cancel_signin.as_ref() {
            let _ = cancel.send(true);
            self.credentials.invalidate(
                self.authorizing_source
                    .map_or(CredentialSlot::Playback, web_slot),
            );
            true
        } else {
            false
        };
        if let Err(error) = self.credentials.revoke(CredentialSlot::Personal) {
            self.emit(Event::Error(error.to_string()));
        }
        self.delete_stored_grant(CredentialSlot::Personal);
        self.web_tokens[CredentialSlot::Personal.index()] = None;
        self.web_client_id = client_id;
        self.api.clear(ApiSource::Personal);
        self.emit(Event::WebApp { client_id: None });
        if self.web_client_id.is_some() {
            if authorization_in_flight {
                self.pending_authorization = Some(ApiSource::Personal);
            } else {
                self.sign_in_source(ApiSource::Personal);
            }
        } else {
            self.pending_authorization = None;
        }
    }

    fn delete_stored_grant(&self, slot: CredentialSlot) {
        let lease = self.credentials.lease(slot);
        let notice = self.storage_notice(lease.clone());
        // Queue deletion before a new browser flow can enqueue a replacement.
        let pending = lease.delete();
        tokio::spawn(async move {
            if let Err(error) = pending.await {
                notice(error);
            }
        });
    }

    /// Cancels in-flight account work and drops cached Home data on sign-out.
    fn sign_out(&mut self) {
        self.spotify_restore_started = true;
        self.cancel_search();
        self.signed_in = false;
        self.rootlist_pending = false;
        self.made_for_you = Arc::new(crate::made_for_you::MadeForYou::new(self.http.clone()));
        self.new_releases = Arc::new(crate::new_releases::NewReleases::new(self.http.clone()));
        self.session.send_modify(|generation| *generation += 1);
        // Friends' names belong to the account that read them.
        self.friend_names = Arc::default();
        self.connect = Arc::default();
        self.authorization_attempt += 1;
        if let Err(error) = self.credentials.revoke_spotify() {
            self.emit(Event::Error(error.to_string()));
        }
        self.restore_pending = [false; 3];
        self.web_tokens = [None, None];
        self.playback_grant = None;
        self.pending_playback_grant = None;
        self.engine_busy = false;
        self.premium = None;
        self.resume = None;
        self.resume_verify = None;
        self.waiting_for_network = false;
        self.cancel_engine_retry();
        self.authorize_if_rejected = false;
        self.album_type_lookup.reset_session();
        self.audiobook_lookup.clear();
        self.playlist_header_lookup.clear_engine_work();
        self.radio_waiting.clear();
        self.prerelease_waiting = None;
        self.carry_volume();
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
        self.stop_kept_player();
        if let Some(cancel) = self.cancel_signin.take() {
            let _ = cancel.send(true);
        }
        self.authorizing_source = None;
        self.pending_authorization = None;
        self.api.clear_all();
        for slot in CredentialSlot::SPOTIFY {
            self.delete_stored_grant(slot);
        }
        self.emit(Event::Playback(LocalPlayback::Unavailable));
        self.emit(Event::Auth(AuthStatus::SignedOut));
    }

    // ---- local playback engine -------------------------------------------

    fn on_playback_authorized(&mut self, access_token: String) {
        let Some(credentials) = playback_credentials(self.api.account(), access_token) else {
            self.engine_busy = false;
            self.emit(Event::Playback(LocalPlayback::Failed(
                "Finish signing in to Spotify before enabling playback.".into(),
            )));
            return;
        };
        self.connect_engine(credentials);
    }

    fn engine_notify(&self) -> crate::player::Notify {
        let events = self.events.clone();
        let commands = self.commands.clone();
        let waker = self.waker.clone();
        let lease = self.credentials.lease(CredentialSlot::Playback);
        Arc::new(move |event| {
            if !lease.current() {
                return;
            }
            match event {
                EngineEvent::State(state) => {
                    let _ = events.send(Event::Local(Box::new(state)));
                    waker.wake();
                }
                EngineEvent::SessionEnded => {
                    let _ = commands.send(Command::Reconnect);
                }
                EngineEvent::AudioDevice { name, id } => {
                    let _ = events.send(Event::AudioDeviceFound { name, id });
                    waker.wake();
                }
            }
        })
    }

    /// Bring the engine up from a credential stored by a previous playback
    /// authorization, if there is one. Silent when there is nothing to resume.
    fn resume_engine(&mut self) {
        if !self.signed_in
            || self.engine.is_some()
            || self.engine_busy
            || self.premium == Some(false)
        {
            return;
        }
        let credentials = self
            .pending_playback_grant
            .clone()
            .or_else(|| self.playback_grant.clone());
        if let Some(credentials) = credentials {
            if !self.network_reachable {
                self.waiting_for_network = true;
                self.emit(Event::Playback(LocalPlayback::WaitingForNetwork));
                return;
            }
            if credentials.username.as_deref()
                != self.api.account().as_ref().map(|account| account.as_str())
            {
                self.pending_playback_grant = None;
                self.emit(Event::Playback(LocalPlayback::Failed(
                    "Stored playback belongs to another Spotify account. Enable playback again."
                        .into(),
                )));
                return;
            }
            self.connect_engine(credentials);
        }
    }

    /// Replace the engine after a user-initiated change (audio settings or
    /// an HTTP proxy). Does not count toward the drop limiter: flipping a
    /// setting is not the session falling over.
    fn replace_engine(&mut self) {
        if !self.signed_in {
            return;
        }
        // New audio settings take effect now, even when the replacement
        // waits for a connection in flight.
        self.stop_kept_player();
        if defer_engine_replace(self.engine_busy, &mut self.engine_restart_pending) {
            return;
        }
        self.take_engine_for_resume();
        self.resume_engine();
    }

    /// Reconnect the engine after its session dropped on its own. Whatever
    /// was playing comes back at the same spot on the new session, so a
    /// dropped connection is a pause of a few seconds rather than silence.
    /// Six drops in ten minutes stop the loop so a flapping session cannot
    /// sit there reconnecting forever.
    fn reconnect_engine(&mut self) {
        if !self.signed_in {
            return;
        }
        if self.engine_busy {
            return;
        }
        if !self.network_reachable {
            self.take_engine_keeping_player();
            self.waiting_for_network = true;
            self.emit(Event::Playback(LocalPlayback::WaitingForNetwork));
            return;
        }
        let now = Instant::now();
        if session_drops_exhausted(&mut self.reconnects, now) {
            self.take_engine_for_resume();
            self.resume = None;
            self.emit(Event::Playback(LocalPlayback::Failed(
                "Local playback keeps dropping. Re-enable it from Settings.".into(),
            )));
            return;
        }
        self.reconnects.push(now);
        log::info!(
            "local playback session ended; reconnecting ({} of {RECONNECT_LIMIT} in ten minutes)",
            self.reconnects.len()
        );
        self.take_engine_keeping_player();
        self.resume_engine();
    }

    /// Try the saved grant again after a backoff. Each failure in a row
    /// waits longer, up to the last of `ENGINE_RETRY_DELAYS`.
    fn schedule_engine_retry(&mut self) {
        let delay = ENGINE_RETRY_DELAYS[self.engine_retries.min(ENGINE_RETRY_DELAYS.len() - 1)];
        self.engine_retries += 1;
        self.engine_retry_generation += 1;
        let generation = self.engine_retry_generation;
        log::info!("local playback connection failed; trying again in {delay}s");
        let commands = self.commands.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            let _ = commands.send(Command::RetryEngine { generation });
        });
    }

    fn cancel_engine_retry(&mut self) {
        self.engine_retries = 0;
        self.engine_retry_generation += 1;
    }

    /// A scheduled retry is due. It runs quietly: the error already shown
    /// stays until the engine is back, and network reachability still
    /// decides whether to connect at all.
    fn retry_engine(&mut self, generation: u64) {
        if generation != self.engine_retry_generation
            || !self.signed_in
            || self.engine.is_some()
            || self.engine_busy
            || self.cancel_signin.is_some()
            || self.playback_grant.is_none()
        {
            return;
        }
        if self.network_reachable {
            self.engine_retry_quiet = true;
        }
        self.resume_engine();
        if !self.engine_busy {
            self.engine_retry_quiet = false;
        }
    }

    /// Keeps the level being heard for the next engine, and lets what the
    /// engine was heard at go with it, so a level from an engine that is
    /// gone never overrides a setting that arrives after it.
    fn carry_volume(&mut self) {
        if let Some(heard) = self.heard.take() {
            self.engine_config.initial_volume = heard.level();
        }
    }

    /// Takes the engine down and keeps what its replacement picks up: the
    /// playback, and the level being heard. A session that drops on its own
    /// comes back at the level it was heard at, not at the level the app
    /// launched with.
    fn take_engine_for_resume(&mut self) {
        self.stop_kept_player();
        self.resume_verify = None;
        self.album_type_lookup.requeue_active_for_new_engine();
        self.playlist_header_lookup.requeue_active_for_new_engine();
        self.carry_volume();
        if let Some(engine) = self.engine.take() {
            self.resume = engine.resume_point();
            engine.shutdown();
        }
    }

    /// Takes down an engine whose session dropped, keeping its player: the
    /// song plays on from what it has fetched while the next engine
    /// connects, and that engine takes the player over (#268).
    fn take_engine_keeping_player(&mut self) {
        self.resume_verify = None;
        self.album_type_lookup.requeue_active_for_new_engine();
        self.playlist_header_lookup.requeue_active_for_new_engine();
        self.carry_volume();
        if let Some(engine) = self.engine.take() {
            // A replacement that dropped before picking the song up has no
            // playback of its own to give: the first pickup stands.
            if !engine.carrying() || self.resume.is_none() {
                self.resume = engine.resume_point();
            }
            self.kept_player = Some(engine.keep_player());
        }
    }

    /// Stops the song a dropped session left playing, when no engine is
    /// going to pick it up.
    fn stop_kept_player(&mut self) {
        if let Some(player) = self.kept_player.take() {
            player.stop();
        }
    }

    /// Set up playback on this computer. A saved playback grant for this
    /// account is tried first; the browser opens only when there is none or
    /// Spotify rejects it (#818, #824).
    fn authorize_playback(&mut self) {
        if self.cancel_signin.is_some() {
            return;
        }
        if self.engine_busy {
            if std::mem::take(&mut self.engine_retry_quiet) {
                // A background retry is already connecting: show it.
                self.authorize_if_rejected = true;
                self.emit(Event::Playback(LocalPlayback::Connecting));
            }
            return;
        }
        if self.premium == Some(false) {
            self.emit(Event::Playback(LocalPlayback::Failed(
                PREMIUM_NEEDED.into(),
            )));
            return;
        }
        let saved = self
            .playback_grant
            .as_ref()
            .is_some_and(|grant| playback_account_matches(grant, self.api.account()));
        if saved && self.signed_in && self.engine.is_none() {
            self.cancel_engine_retry();
            self.pending_playback_grant = None;
            self.authorize_if_rejected = true;
            self.resume_engine();
            if self.engine_busy || self.waiting_for_network {
                return;
            }
            self.authorize_if_rejected = false;
        }
        self.authorize_playback_in_browser();
    }

    /// Start (or re-enter) the playback authorization in the browser. This is
    /// a distinct grant from the Web API sign-in: it uses Spotify's streaming
    /// client identity, the one librespot can play with.
    fn authorize_playback_in_browser(&mut self) {
        let http = match self.http.client() {
            Ok(http) => http,
            Err(error) => {
                self.emit(Event::Error(error));
                return;
            }
        };
        if self.engine_busy || self.cancel_signin.is_some() {
            return;
        }
        let grant = crate::auth::Grant::playback();
        // Reserve the callback port before touching the saved grant or
        // opening the browser (#899, as #754 did for the Web API). A busy
        // port must keep the saved grant and show the conflict, not send the
        // approval to whichever app holds the port.
        let listener = match crate::auth::listen_for_redirect(grant.redirect_port) {
            Ok(listener) => listener,
            Err(error) => {
                self.emit(Event::Playback(LocalPlayback::Failed(error.to_string())));
                return;
            }
        };
        self.cancel_engine_retry();
        self.credentials.invalidate(CredentialSlot::Playback);
        self.authorization_attempt += 1;
        let attempt = self.authorization_attempt;
        let lease = self.credentials.lease(CredentialSlot::Playback);
        let flow = crate::auth::begin(grant.clone());
        let (cancel_tx, cancel_rx) = watch::channel(false);
        self.cancel_signin = Some(cancel_tx);
        self.emit(Event::Playback(LocalPlayback::Authorizing));
        let browser_url = flow.url.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(error) = crate::opener::open(&browser_url) {
                log::warn!("unable to open a browser: {error}");
            }
        });

        let events = self.events.clone();
        let waker = self.waker.clone();
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let result = async {
                let code = crate::auth::wait_for_code_on_listener(listener, &flow.state, cancel_rx)
                    .await?;
                crate::auth::exchange_code(&http, &grant, &code, &flow.verifier).await
            }
            .await;
            match result {
                Ok(token) => {
                    let _ = commands.send(Command::PlaybackAuthorized {
                        access_token: token.access_token,
                        lease: lease.clone(),
                        attempt,
                    });
                }
                Err(error) => {
                    let message = error.to_string();
                    if !lease.current() {
                        let _ = commands.send(Command::PlaybackAuthEnded { attempt });
                        return;
                    }
                    if message.contains("cancelled") {
                        let _ = events.send(Event::Playback(LocalPlayback::Unavailable));
                    } else {
                        let _ = events.send(Event::Playback(LocalPlayback::Failed(message)));
                    }
                    waker.wake();
                    let _ = commands.send(Command::PlaybackAuthEnded { attempt });
                }
            }
        });
    }

    /// Spawn an engine connection so a slow or hung librespot handshake can
    /// never block the command loop (this was the cause of the app freezing
    /// on "Connecting to Spotify"). Reusable credentials stay in memory until
    /// this worker receives the connected engine and persists them securely.
    fn connect_engine(&mut self, credentials: Credentials) {
        if let Err(error) = self.http.client() {
            self.pending_playback_grant = None;
            self.emit(Event::Playback(LocalPlayback::Failed(error)));
            return;
        }
        if self.engine_busy {
            return;
        }
        if self.premium == Some(false) {
            self.pending_playback_grant = None;
            self.emit(Event::Playback(LocalPlayback::Failed(
                PREMIUM_NEEDED.into(),
            )));
            return;
        }
        self.pending_playback_grant = Some(credentials.clone());
        self.cancel_signin = None;
        self.engine_busy = true;
        self.engine_attempt = self.engine_attempt.wrapping_add(1);
        let attempt = self.engine_attempt;
        if !self.engine_retry_quiet {
            self.emit(Event::Playback(LocalPlayback::Connecting));
        }
        let lease = self.credentials.lease(CredentialSlot::Playback);
        let config = self.engine_config.clone();
        let proxy = config.proxy.librespot_url();
        self.engine_proxy = proxy.clone();
        let notify = self.engine_notify();
        let kept = self.kept_player.clone();
        let events = self.events.clone();
        let commands = self.commands.clone();
        let waker = self.waker.clone();
        let session_generation = self.album_type_lookup.session_generation;
        tokio::spawn(async move {
            let cache = match config.open_cache() {
                Ok(cache) => cache,
                Err(error) => {
                    let _ = commands.send(Command::EngineConnected {
                        lease: lease.clone(),
                        session_generation,
                        engine: Box::new(None),
                        error: Some(error.to_string()),
                        attempt,
                    });
                    return;
                }
            };
            let connect_outcome = connect_engine_with_deadline(Engine::connect(
                &config,
                proxy,
                credentials,
                cache,
                notify,
                kept,
            ))
            .await;
            let outcome = match connect_outcome {
                Ok(Ok(engine)) => Command::EngineConnected {
                    lease: lease.clone(),
                    session_generation,
                    engine: Box::new(Some(engine)),
                    error: None,
                    attempt,
                },
                Ok(Err(error)) => {
                    log::error!("engine connect failed: {error:#}");
                    Command::EngineConnected {
                        lease: lease.clone(),
                        session_generation,
                        engine: Box::new(None),
                        error: Some(friendly_connect_error(&error)),
                        attempt,
                    }
                }
                Err(_) => Command::EngineConnected {
                    lease: lease.clone(),
                    session_generation,
                    engine: Box::new(None),
                    error: Some("Connecting to Spotify timed out".into()),
                    attempt,
                },
            };
            let _ = commands.send(outcome);
            let _ = events;
            waker.wake();
        });
    }

    fn on_engine_connected(
        &mut self,
        session_generation: u64,
        engine: Option<Engine>,
        error: Option<String>,
        attempt: u64,
    ) {
        if !self.signed_in || session_generation != self.album_type_lookup.session_generation {
            if let Some(engine) = engine {
                engine.shutdown();
            }
            return;
        }
        if attempt != self.engine_attempt {
            if let Some(engine) = engine {
                // A newer attempt carries the same song on.
                if engine.carrying() && self.kept_player.is_some() {
                    engine.close_session();
                } else {
                    engine.shutdown();
                }
            }
            return;
        }
        self.engine_busy = false;
        if std::mem::take(&mut self.engine_restart_pending) {
            if let Some(engine) = engine {
                engine.shutdown();
            }
            // Keep `resume`: it belongs to the engine which was replaced,
            // not to this stale attempt. The newest config is already stored.
            self.resume_engine();
            self.start_prerelease();
            return;
        }
        let quiet = std::mem::take(&mut self.engine_retry_quiet);
        let authorize_if_rejected = std::mem::take(&mut self.authorize_if_rejected);
        match engine {
            Some(engine) => {
                self.waiting_for_network = false;
                self.pending_playback_grant = None;
                self.cancel_engine_retry();
                if let Some(grant) = engine.credentials() {
                    if !playback_account_matches(&grant, self.api.account()) {
                        engine.shutdown();
                        self.fail_waiting_prerelease("Playback was authorized for another Spotify account. Enable playback again with the signed-in account.");
                        self.emit(Event::Playback(LocalPlayback::Failed("Playback was authorized for another Spotify account. Enable playback again with the signed-in account.".into())));
                        return;
                    }
                    self.playback_grant = Some(grant.clone());
                    let lease = self.credentials.lease(CredentialSlot::Playback);
                    let notice = self.storage_notice(lease.clone());
                    let pending = lease.save(StoredGrant::Playback(grant));
                    tokio::spawn(async move {
                        if let Err(error) = pending.await {
                            notice(error);
                        }
                    });
                }
                let device_id = engine.device_id().to_string();
                // The engine plays the kept song on now.
                self.kept_player = None;
                let engine = Arc::new(engine);
                self.heard = Some(engine.heard());
                if let Some(spec) = self.resume.take() {
                    // Delay resume until Spirc finishes registering. An early
                    // load can return 400 and leave playback stopped. Verify
                    // the load and retry if needed.
                    self.resume_verify = Some((spec, 0));
                    self.schedule_resume_check(1_500);
                }
                self.engine = Some(engine);
                self.start_rootlist();
                self.reconnects.clear();
                self.emit(Event::Playback(LocalPlayback::Ready { device_id }));
                self.start_album_type_lookup();
                self.start_audiobook_lookup();
                self.start_playlist_header_lookup();
                self.start_radio();
                self.start_prerelease();
            }
            None => {
                if !self.network_reachable {
                    self.waiting_for_network = true;
                    self.emit(Event::Playback(LocalPlayback::WaitingForNetwork));
                } else if self.waiting_for_network
                    && !is_terminal_credential_error(error.as_deref())
                {
                    self.waiting_for_network = false;
                    let now = Instant::now();
                    if session_drops_exhausted(&mut self.reconnects, now) {
                        self.pending_playback_grant = None;
                        self.resume = None;
                        self.stop_kept_player();
                        let message = "Local playback keeps dropping. Re-enable it from Settings.";
                        self.fail_waiting_prerelease(message);
                        self.emit(Event::Playback(LocalPlayback::Failed(message.into())));
                    } else {
                        self.reconnects.push(now);
                        self.resume_engine();
                    }
                } else if authorize_if_rejected && is_rejected_grant(error.as_deref()) {
                    // The user asked for playback and the saved grant no
                    // longer works: ask Spotify for a new one.
                    self.waiting_for_network = false;
                    self.pending_playback_grant = None;
                    self.resume = None;
                    self.stop_kept_player();
                    self.authorize_playback_in_browser();
                } else {
                    self.waiting_for_network = false;
                    self.pending_playback_grant = None;
                    let message = error.unwrap_or_else(|| "Local playback is unavailable".into());
                    self.fail_waiting_prerelease(&message);
                    // A dropped or reset connection is not a rejected grant:
                    // keep trying the saved one in the background (#818).
                    let retrying = self.playback_grant.is_some()
                        && !is_terminal_credential_error(Some(&message));
                    if retrying {
                        self.schedule_engine_retry();
                    }
                    // A song still playing out a dropped session carries on
                    // through the retries, and the engine that connects
                    // picks it up (#268). Anything else is forgotten.
                    if !(retrying && self.kept_player.as_ref().is_some_and(KeptPlayer::has_song)) {
                        self.resume = None;
                        self.stop_kept_player();
                    }
                    if !quiet {
                        self.emit(Event::Playback(LocalPlayback::Failed(message)));
                    }
                }
            }
        }
    }

    /// Starts the engine only for Premium accounts. librespot 0.8 calls
    /// `exit(1)` for Free accounts, which cannot be caught. If the plan is
    /// unknown, preserve the previous behavior and start the engine.
    fn on_account_checked(&mut self, premium: Option<bool>) {
        self.premium = premium;
        if premium == Some(false) {
            self.album_type_lookup.clear_engine_work();
            self.playlist_header_lookup.clear_engine_work();
            self.carry_volume();
            if let Some(engine) = self.engine.take() {
                engine.shutdown();
            }
            self.stop_kept_player();
            self.pending_playback_grant = None;
            let credential_stored = self.playback_grant.is_some();
            if credential_stored {
                self.emit(Event::Playback(LocalPlayback::Failed(
                    PREMIUM_NEEDED.into(),
                )));
            }
            return;
        }
        self.resume_engine();
        // A sign-in restored over a running engine picks up the headers the
        // lapse held back.
        self.start_playlist_header_lookup();
    }

    // ---- receivers on the local network -----------------------------------

    /// Browses for receivers Spotify's device list does not know about. The
    /// browse blocks, so it runs off the runtime's worker threads.
    fn discover_receivers(&mut self) {
        if self
            .receiver_discovery
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        let events = self.events.clone();
        let waker = self.waker.clone();
        let connect = Arc::clone(&self.connect);
        let updates = crate::zeroconf::ReceiverUpdates::default();
        self.receiver_discovery =
            Some(tokio::task::spawn_blocking(
                move || match crate::zeroconf::discover_with_updates(
                    std::time::Duration::from_secs(3),
                    |receivers| {
                        if updates.publish(receivers) {
                            let _ = events.send(Event::ReceiversProgress(updates.clone()));
                            waker.wake();
                        }
                    },
                )
                .map(|receivers| receiver_snapshot(receivers, crate::zeroconf::resolve_receivers))
                {
                    Ok(receivers) => {
                        connect.remember(&receivers);
                        let _ = events.send(Event::Receivers(receivers));
                        waker.wake();
                    }
                    Err(error) => log::debug!("no receivers found on the network: {error}"),
                },
            ));
    }

    /// Sends the stored playback credential to a receiver so it can sign in.
    fn activate_receiver(&self, receiver: crate::zeroconf::Receiver) {
        if receiver.cast {
            return self.activate_cast(receiver);
        }
        let events = self.events.clone();
        let waker = self.waker.clone();
        let credentials = self
            .playback_grant
            .as_ref()
            .filter(|credentials| {
                credentials.username.as_deref()
                    == self.api.account().as_ref().map(|account| account.as_str())
            })
            .and_then(|credentials| crate::zeroconf::Credentials::from_playback(credentials).ok());
        let lease = self.credentials.lease(CredentialSlot::Playback);
        tokio::task::spawn_blocking(move || {
            let name = receiver.name.clone();
            let result = (|| -> Result<(), String> {
                if !lease.current() {
                    return Err("Sign-in changed before receiver activation.".into());
                }
                let credentials = credentials.ok_or_else(|| "Enable playback on this computer first, so there is an account to hand over".to_string())?;
                let http = reqwest::blocking::Client::builder()
                    // Receiver endpoints are private LAN addresses. Keep
                    // them off environment and OS proxies even when System
                    // mode is active.
                    .no_proxy()
                    .timeout(std::time::Duration::from_secs(8))
                    .build()
                    .map_err(|error| error.to_string())?;
                let info = crate::zeroconf::get_info(&http, &receiver)
                    .map_err(|error| error.to_string())?;
                if !lease.current() {
                    return Err("Sign-in changed before receiver activation.".into());
                }
                crate::zeroconf::add_user(&http, &receiver, &info, &credentials, "Spotifast")
                    .map_err(|error| error.to_string())
            })();
            let _ = events.send(Event::ReceiverActivated { name, result });
            waker.wake();
        });
    }

    /// Starts Spotify on a Google Cast device and signs it in with a token
    /// that this session obtains for the device's Spotify client.
    fn activate_cast(&self, receiver: crate::zeroconf::Receiver) {
        let events = self.events.clone();
        let waker = self.waker.clone();
        let session = self
            .engine
            .as_ref()
            .map(|engine| engine.session().clone())
            .filter(|session| same_account(&session.username(), self.api.account().as_ref()));
        let lease = self.credentials.lease(CredentialSlot::Playback);
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let name = receiver.name.clone();
            let result = (|| -> Result<(), String> {
                let session = session.ok_or_else(|| "Enable playback on this computer first, so there is an account to hand over".to_string())?;
                crate::cast::start(receiver.address, receiver.port, &name, |client, device| {
                    let token =
                        runtime.block_on(crate::cast::device_token(&session, client, device))?;
                    if !lease.current() {
                        anyhow::bail!("Sign-in changed before receiver activation.");
                    }
                    Ok(token)
                })
                .map_err(|error| format!("{error:#}"))
            })();
            let _ = events.send(Event::ReceiverActivated { name, result });
            waker.wake();
        });
    }

    fn check_for_updates(&self, manual: bool, source: crate::updates::Source) {
        // A proxy still being restored or refused blocks the check, as it
        // blocks every other request.
        let usable = self.http.client().map(|_| ());
        let proxy = self.engine_config.proxy.clone();
        let events = self.events.clone();
        let waker = self.waker.clone();
        tokio::task::spawn_blocking(move || {
            let result = usable.and_then(|()| {
                crate::updates::updater(&proxy)
                    .and_then(|updater| updater.with_source(source).check())
                    .map_err(|error| format!("{error:#}"))
            });
            let _ = events.send(Event::UpdateChecked { manual, result });
            waker.wake();
        });
    }

    /// Verifies playback after reconnect and retries loads rejected while
    /// Spirc is still registering. Runs on the backend timer.
    fn verify_resume(&mut self) {
        let Some((spec, attempts)) = self.resume_verify.take() else {
            return;
        };
        let Some(engine) = &self.engine else {
            return;
        };
        if engine.interrupted().is_some() && !engine.carrying() {
            // Playback resumed or another track started. A song the player
            // carried through the reconnect still needs picking up.
            return;
        }
        if attempts >= 3 {
            log::warn!("gave up picking playback up again after {attempts} tries");
            return;
        }
        log::info!(
            "restoring playback on the new session (try {})",
            attempts + 1
        );
        if let Err(error) = engine.resume(spec.clone()) {
            log::warn!("unable to pick playback up again: {error}");
        }
        self.resume_verify = Some((spec, attempts + 1));
        self.schedule_resume_check(4_000);
    }

    fn schedule_resume_check(&self, delay_ms: u64) {
        let commands = self.commands.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let _ = commands.send(Command::VerifyResume);
        });
    }

    fn fetch_rootlist(&mut self) {
        if !self.signed_in {
            return;
        }
        self.rootlist_pending = true;
        self.start_rootlist();
    }

    fn start_rootlist(&mut self) {
        let fetch = self.engine.clone().map(|engine| async move {
            engine
                .rootlist()
                .await
                .map_err(|error| format!("{error:#}"))
        });
        self.start_pending_rootlist(fetch);
    }

    // Keep the read injectable so startup ordering can be tested without
    // authenticating a real playback engine or using an account's folders.
    fn start_pending_rootlist(
        &mut self,
        fetch: Option<
            impl std::future::Future<Output = Result<crate::player::Rootlist, String>> + Send + 'static,
        >,
    ) {
        if !self.signed_in || !self.rootlist_pending {
            return;
        }
        let Some(fetch) = fetch else {
            return;
        };
        self.rootlist_pending = false;
        let commands = self.commands.clone();
        let mut session = self.session.subscribe();
        let generation = *session.borrow_and_update();
        tokio::spawn(async move {
            let result = tokio::select! {
                _ = session.changed() => return,
                result = fetch => result,
            };
            let _ = commands.send(Command::RootlistFinished { generation, result });
        });
    }

    fn on_rootlist_finished(
        &self,
        generation: u64,
        result: Result<crate::player::Rootlist, String>,
    ) {
        if self.signed_in && generation == *self.session.borrow() {
            self.emit(Event::Rootlist { result });
        }
    }

    fn fetch_album_types(&mut self, uris: Vec<String>) {
        self.album_type_lookup
            .enqueue(self.signed_in, self.premium, uris);
        self.start_album_type_lookup();
    }

    /// Asks the streaming session about the waiting shows. Without a
    /// session they wait; a failed answer leaves them shown.
    fn start_audiobook_lookup(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        if self.audiobook_lookup.is_empty() {
            return;
        }
        let uris: Vec<String> = std::mem::take(&mut self.audiobook_lookup)
            .into_iter()
            .collect();
        let session_generation = *self.session.borrow();
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let mut audiobooks = Vec::new();
            for chunk in uris.chunks(AUDIOBOOK_BATCH) {
                match session_reads::audiobook_shows(engine.session(), chunk).await {
                    Ok(found) => audiobooks.extend(found),
                    Err(error) => log::debug!("audiobook lookup failed: {error:#}"),
                }
            }
            let _ = commands.send(Command::AudiobookShowsResolved {
                session_generation,
                audiobooks,
            });
        });
    }

    /// Reads the next waiting playlist header over the streaming session,
    /// never the Web API, which already listed it blank. One read runs at a
    /// time, so a long list stays gentle on Spotify, and each header is sent
    /// as it arrives so the rows fill in turn. Without a session they wait; a
    /// header that cannot be read leaves its row as listed.
    fn start_playlist_header_lookup(&mut self) {
        if !self.signed_in {
            return;
        }
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let Some(request) = self.playlist_header_lookup.next(*self.session.borrow()) else {
            return;
        };
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let read = session_reads::playlist(engine.session(), &request.id);
            let header = match tokio::time::timeout(SESSION_READ_TIMEOUT, read).await {
                Ok(Ok(header)) => Some(header),
                Ok(Err(error)) => {
                    log::debug!("playlist header lookup failed: {error:?}");
                    None
                }
                Err(_) => {
                    log::debug!("playlist header lookup timed out");
                    None
                }
            };
            let _ = commands.send(Command::PlaylistHeaderResolved {
                id: request.id,
                session_generation: request.session_generation,
                engine_generation: request.engine_generation,
                header,
            });
        });
    }

    fn on_playlist_header_resolved(
        &mut self,
        request: PlaylistHeaderRequest,
        header: Option<crate::api::models::Playlist>,
    ) {
        if !self.playlist_header_lookup.finish(&request) {
            return;
        }
        // A read that finishes while the sign-in has lapsed is not shown. It
        // goes back to the front of the queue and is read again, with the
        // rest, once the sign-in comes back. Signing out clears the queue
        // instead, so its answer never arrives here.
        if !self.signed_in {
            self.playlist_header_lookup.requeue(request.id);
            return;
        }
        if let Some(header) = header {
            self.emit(Event::PlaylistHeader(Box::new(header)));
        }
        self.start_playlist_header_lookup();
    }

    /// Resolves the waiting radios once the streaming session is ready; the
    /// Web API has no stations.
    fn start_radio(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let session_generation = *self.session.borrow();
        for (seed, generation) in std::mem::take(&mut self.radio_waiting) {
            let engine = Arc::clone(&engine);
            let commands = self.commands.clone();
            tokio::spawn(async move {
                let result = match crate::util::station_uri(&seed) {
                    Some(station) => {
                        match tokio::time::timeout(
                            RADIO_TIMEOUT,
                            session_reads::station(engine.session(), &station),
                        )
                        .await
                        {
                            Ok(Ok(tracks)) => Ok(tracks),
                            Ok(Err(error)) => {
                                log::warn!("radio {station} failed: {error:#}");
                                Err("Couldn't load this radio. Try again.".to_string())
                            }
                            Err(_) => Err("Spotify took too long to answer. Try again.".into()),
                        }
                    }
                    None => Err("There is no radio for this item.".into()),
                };
                let _ = commands.send(Command::RadioResolved {
                    session_generation,
                    seed,
                    generation,
                    result,
                });
            });
        }
    }

    fn remove_playlist_rows(&self, playlist_id: String, rows: Vec<session_reads::RemovedRow>) {
        let answer = |id: String, result: Result<Option<String>, String>| {
            Event::Api(Box::new(ApiResponse::PlaylistItemsChanged {
                id,
                message: "Removed from playlist".to_string(),
                result: result.map_err(|message| ApiError::Status { status: 0, message }),
            }))
        };
        // The Web API would remove every copy, so without the session the
        // change is refused rather than made bigger than asked.
        let Some(engine) = self.engine.clone().filter(|_| self.signed_in) else {
            self.emit(answer(
                playlist_id,
                Err(
                    "Removing one copy of a repeated song needs local playback. Try again when connected."
                        .into(),
                ),
            ));
            return;
        };
        let events = self.events.clone();
        let waker = self.waker.clone();
        let mut cancelled = self.session.subscribe();
        tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                _ = cancelled.changed() => return,
                result = tokio::time::timeout(
                    RADIO_TIMEOUT,
                    session_reads::remove_playlist_rows(engine.session(), &playlist_id, &rows),
                ) => match result {
                    Ok(result) => result.map_err(|error| format!("{error:#}")),
                    Err(_) => Err("Spotify took too long to answer. Check the playlist and try again.".into()),
                },
            };
            let _ = events.send(answer(playlist_id, result));
            waker.wake();
        });
    }

    fn set_prerelease_saved(&self, uri: String, saved: bool, generation: u64) {
        let session_generation = *self.session.borrow();
        let Some(engine) = self.engine.clone().filter(|_| self.signed_in) else {
            self.emit(Event::PrereleaseSaved {
                uri,
                saved,
                generation,
                result: Err("Local playback is unavailable. Try again when connected.".into()),
            });
            return;
        };
        let http = crate::http::build_collection_client(&self.engine_config.proxy);
        let commands = self.commands.clone();
        let mut cancelled = self.session.subscribe();
        tokio::spawn(async move {
            let result = match http {
                Err(error) => Err(error),
                Ok(http) => tokio::select! {
                biased;
                _ = cancelled.changed() => return,
                result = tokio::time::timeout(RADIO_TIMEOUT, session_reads::set_prerelease_saved(engine.session(), &http, &uri, saved)) => match result {
                    Ok(result) => result.map_err(|error| format!("{error:#}")),
                    Err(_) => Err("Spotify took too long to confirm pre-save. Open the link again to check.".into()),
                }
                },
            };
            let _ = commands.send(Command::PrereleaseSaved {
                session_generation,
                uri,
                saved,
                generation,
                result,
            });
        });
    }

    fn fail_waiting_prerelease(&mut self, message: &str) {
        if let Some((uri, generation)) = self.prerelease_waiting.take() {
            self.emit(Event::Prerelease {
                uri,
                generation,
                result: Err(message.into()),
            });
        }
    }

    fn start_prerelease(&mut self) {
        if self.prerelease_waiting.is_none() {
            return;
        }
        let Some(engine) = self.engine.clone() else {
            if self.restore_pending[CredentialSlot::Playback.index()] || self.engine_busy {
                return;
            }
            let has_credentials =
                self.playback_grant.is_some() || self.pending_playback_grant.is_some();
            if has_credentials {
                self.resume_engine();
            }
            if !self.engine_busy && (!self.waiting_for_network || !has_credentials) {
                self.fail_waiting_prerelease(
                    "Upcoming albums need playback authorization for this account in Settings.",
                );
            }
            return;
        };
        let Some((uri, generation)) = self.prerelease_waiting.take() else {
            return;
        };
        let session_generation = *self.session.borrow();
        let mut cancelled = self.session.subscribe();
        let commands = self.commands.clone();
        let web = self.api.prerelease_http_client();
        tokio::spawn(async move {
            let Some(web) = web else {
                let _ = commands.send(Command::PrereleaseResolved {
                    session_generation,
                    uri,
                    generation,
                    result: Err("Sign in before opening an upcoming album.".into()),
                });
                return;
            };
            let started = Instant::now();
            let result = tokio::select! {
                biased;
                _ = cancelled.changed() => return,
                result = tokio::time::timeout(RADIO_TIMEOUT, session_reads::prerelease(engine.session(), &web, &uri)) => match result {
                    Ok(result) => result.map_err(|error| format!("{error:#}")),
                    Err(_) => Err("Spotify took too long to load this upcoming album. Try the link again.".into()),
                }
            };
            log::info!(
                "Prerelease source=session success={} elapsed_ms={}",
                result.is_ok(),
                started.elapsed().as_millis()
            );
            let _ = commands.send(Command::PrereleaseResolved {
                session_generation,
                uri,
                generation,
                result: result.map(Box::new),
            });
        });
    }

    fn start_album_type_lookup(&mut self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let Some(request) = self.album_type_lookup.next() else {
            return;
        };
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let result =
                match tokio::time::timeout(ALBUM_TYPE_TIMEOUT, engine.album_is_ep(&request.uri))
                    .await
                {
                    Ok(result) => result.map_err(|error| format!("{error:#}")),
                    Err(_) => Err("album metadata timed out".into()),
                };
            let _ = commands.send(Command::AlbumTypeResolved {
                uri: request.uri,
                session_generation: request.session_generation,
                engine_generation: request.engine_generation,
                result,
            });
        });
    }

    fn on_album_type_resolved(&mut self, request: AlbumTypeRequest, result: Result<bool, String>) {
        if !self.signed_in || !self.album_type_lookup.finish(&request) {
            return;
        }
        self.emit(Event::AlbumType {
            uri: request.uri,
            result,
        });
        self.start_album_type_lookup();
    }

    fn fetch_lyrics(&self, request: LyricsRequest) {
        let http = self.http.client();
        let events = self.events.clone();
        let waker = self.waker.clone();
        let cache_dir = self.dirs.lyrics_cache_dir();
        let engine = self.engine.clone();
        tokio::spawn(async move {
            // Spotify's own words go first: they follow the recording
            // exactly. Everything else, a signed-out session included,
            // falls back to LRCLIB.
            let result = match spotify_lyrics(engine, &request.uri, &cache_dir).await {
                Some(found) => Ok(Some(found)),
                None => match http {
                    Ok(http) => crate::lyrics::fetch(&http, &cache_dir, &request.query)
                        .await
                        .map_err(|error| format!("{error:#}")),
                    Err(error) => Err(error),
                },
            };
            let _ = events.send(Event::Lyrics {
                uri: request.uri,
                result,
            });
            waker.wake();
        });
    }

    /// Loads cached playlist items. The UI compares the cached snapshot with
    /// the live playlist before using them.
    fn load_playlist_cache(&self, id: String, generation: u64) {
        let Some(account) = self.api.account() else {
            return;
        };
        let events = self.events.clone();
        let waker = self.waker.clone();
        let path = self
            .dirs
            .account_playlist_cache_dir(account.as_str())
            .join(format!("{id}.json"));
        let account_id = account.as_str().to_string();
        tokio::spawn(async move {
            let cache = read_playlist_cache(path)
                .await
                .ok()
                .and_then(|(cached, appendable)| {
                    let total = cached
                        .total
                        .unwrap_or_else(|| cached.items.len().try_into().unwrap_or(u32::MAX));
                    if cached.items.len() > total as usize
                        || cached.next_offset.is_some_and(|offset| offset > total)
                    {
                        return None;
                    }
                    Some(PlaylistCache {
                        snapshot: cached.snapshot,
                        items: cached.items,
                        total,
                        next_offset: cached.next_offset,
                        appendable,
                    })
                });
            let _ = events.send(Event::PlaylistCache {
                account_id,
                id,
                generation,
                cache,
            });
            waker.wake();
        });
    }

    /// Ask Spotify who is behind each user id. Only the streaming session
    /// can ask; without one the interface shows the bare ids.
    fn fetch_user_names(&self, ids: Vec<String>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let events = self.events.clone();
        let waker = self.waker.clone();
        tokio::spawn(async move {
            for id in ids {
                let profile = session_reads::user_profile(engine.session(), &id).await;
                let (name, image) = match profile {
                    Some(profile) => (profile.name, profile.image_url),
                    None => (None, None),
                };
                let _ = events.send(Event::UserName { id, name, image });
                waker.wake();
            }
        });
    }

    /// Ask Spotify how often each song of an album or each of an artist's
    /// popular songs was played. It needs the playback session and a
    /// verified Web API grant, as upcoming album pages do; without them the
    /// lists simply show no counts.
    fn fetch_play_counts(&self, uri: String) {
        let events = self.events.clone();
        let waker = self.waker.clone();
        let send = move |uri, result| {
            let _ = events.send(Event::PlayCounts { uri, result });
            waker.wake();
        };
        let Some(engine) = self.engine.clone() else {
            return send(uri, Err("Play counts need playback enabled.".into()));
        };
        let Some(web) = self.api.prerelease_http_client() else {
            return send(uri, Err("Play counts need a signed-in account.".into()));
        };
        let mut cancelled = self.session.subscribe();
        tokio::spawn(async move {
            let read = tokio::time::timeout(
                Duration::from_secs(15),
                session_reads::play_counts(engine.session(), &web, &uri),
            );
            let result = tokio::select! {
                biased;
                // A later visit asks again.
                _ = cancelled.changed() => Err("The playback session ended.".to_string()),
                result = read => result
                    .map_err(|_| "Spotify took too long to answer.".to_string())
                    .and_then(|result| result.map_err(|error| format!("{error:#}"))),
            };
            if let Err(error) = &result {
                log::debug!("Play counts unavailable: {error}");
            }
            send(uri, result);
        });
    }

    /// Ask the streaming session what friends are listening to. Without one
    /// there is nobody to ask, which the panel says.
    /// A read still running when the session ends is dropped, so nothing
    /// from the last account reaches the next.
    fn fetch_friend_activity(&self, generation: u64) {
        let events = self.events.clone();
        let waker = self.waker.clone();
        let send = move |result| {
            let _ = events.send(Event::FriendActivity { generation, result });
            waker.wake();
        };
        let Some(engine) = self.engine.clone() else {
            send(Err("Listening activity needs playback enabled.".into()));
            return;
        };
        let names = self.friend_names.clone();
        let mut cancelled = self.session.subscribe();
        tokio::spawn(async move {
            let read = tokio::time::timeout(
                Duration::from_secs(15),
                session_reads::friend_activity(engine.session(), &names),
            );
            let result = tokio::select! {
                biased;
                _ = cancelled.changed() => return,
                result = read => result,
            }
            .map_err(|_| "Spotify took too long to answer.".to_string())
            .and_then(|result| result.map_err(|error| error.to_string()));
            send(result);
        });
    }

    // ---- api ----------------------------------------------------------------

    fn cancel_search(&mut self) {
        for task in self.search_tasks.drain(..) {
            task.abort();
        }
    }

    fn search(&mut self, query: String, serial: u64) {
        self.cancel_search();
        if query.is_empty() {
            return;
        }
        let split = self.api.personal_ready();
        self.emit(Event::Api(Box::new(ApiResponse::SearchStarted {
            query: query.clone(),
            serial,
            split,
        })));
        if split {
            self.search_tasks
                .push(self.dispatch(ApiRequest::SearchPlaylists {
                    query: query.clone(),
                    serial,
                }));
        }
        let request = if split {
            ApiRequest::SearchCatalogue { query, serial }
        } else {
            ApiRequest::Search { query, serial }
        };
        self.search_tasks.push(self.dispatch(request));
    }

    /// Cancel requests on sign-out and report completion under their original generation.
    fn dispatch(&self, request: ApiRequest) -> tokio::task::AbortHandle {
        let api = Arc::clone(&self.api);
        let made_for_you = Arc::clone(&self.made_for_you);
        let new_releases = Arc::clone(&self.new_releases);
        let home_account = if matches!(request, ApiRequest::MadeForYou { .. }) {
            api.account()
        } else {
            None
        };
        let connect = Arc::clone(&self.connect);
        let shared_lease = self.credentials.lease(CredentialSlot::Shared);
        let personal_lease = self.credentials.lease(CredentialSlot::Personal);
        let background_session = Arc::clone(&self.background_session);
        let engine = self.engine.clone();
        let commands = self.commands.clone();
        let mut session = self.session.subscribe();
        let generation = *session.borrow_and_update();
        tokio::spawn(async move {
            let (response, expired) = tokio::select! {
                _ = session.changed() => return,
                result = async {
                    handle(&api, &made_for_you, &new_releases, &connect, engine.as_deref(), &background_session, request).await
                } => result,
            };
            // Apply completion on the command loop. A late response cannot
            // clear or repopulate a session created after sign-out.
            let _ = commands.send(Command::ApiFinished {
                generation,
                home_account,
                response: Box::new(response),
                expired,
                shared_lease,
                personal_lease,
            });
        })
        .abort_handle()
    }

    fn accent(&self, url: String) {
        let art = self.art.clone();
        let events = self.events.clone();
        let waker = self.waker.clone();
        tokio::spawn(async move {
            if let Ok(bytes) = art.fetch(&url).await {
                let palette = tokio::task::spawn_blocking(move || art_palette(&bytes))
                    .await
                    .unwrap_or_default();
                if let Some(&color) = palette.first() {
                    let _ = events.send(Event::Accent {
                        url,
                        color,
                        palette,
                    });
                    waker.wake();
                }
            }
        });
    }
}

fn friendly_connect_error(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    let lower = text.to_lowercase();
    if lower.contains("badcredentials") || lower.contains("bad credentials") {
        "Spotify rejected the saved sign-in. Please sign in again.".to_string()
    } else if lower.contains("premium") {
        PREMIUM_NEEDED.to_string()
    } else if lower.contains("dns") || lower.contains("connect") || lower.contains("resolve") {
        format!("Couldn't reach Spotify: {text}")
    } else {
        text
    }
}

/// Spotify refused the playback grant itself, as opposed to the connection
/// failing on the way.
fn is_rejected_grant(error: Option<&str>) -> bool {
    error.is_some_and(|msg| {
        let lower = msg.to_lowercase();
        lower.contains("badcredentials")
            || lower.contains("bad credentials")
            || lower.contains("rejected the saved sign-in")
    })
}

fn is_terminal_credential_error(error: Option<&str>) -> bool {
    error.is_some_and(|msg| {
        let lower = msg.to_lowercase();
        lower.contains("badcredentials")
            || lower.contains("bad credentials")
            || lower.contains("rejected the saved sign-in")
            || lower.contains("premium")
    })
}

/// Classify a request by the coverage and playlist permissions its source must provide.
fn operation_for(api: &ApiGateway, request: &ApiRequest) -> Operation {
    match request {
        ApiRequest::Me => Operation::CanonicalAccount,
        ApiRequest::Devices
        | ApiRequest::PlaybackState { .. }
        | ApiRequest::Queue { .. }
        | ApiRequest::Remote { .. }
        | ApiRequest::Transfer { .. }
        | ApiRequest::ShufflePlay { .. }
        | ApiRequest::AddToQueue { .. }
        | ApiRequest::AddManyToQueue { .. } => Operation::Playback,
        ApiRequest::RecentlyPlayed { .. }
        | ApiRequest::TopTracks { .. }
        | ApiRequest::TopArtists { .. }
        | ApiRequest::SavedTracks { .. }
        | ApiRequest::SavedAlbums { .. }
        | ApiRequest::FollowedArtists { .. }
        | ApiRequest::SavedShows { .. }
        | ApiRequest::SavedEpisodes { .. }
        | ApiRequest::SetSaved { .. } => Operation::UserData,
        // Development Mode cannot answer membership for playlists it omits.
        ApiRequest::Contains { uris } => {
            if uris.iter().any(|uri| uri.starts_with("spotify:playlist:")) {
                Operation::UnsupportedDevelopmentMode
            } else {
                Operation::UserData
            }
        }
        ApiRequest::MyPlaylists { .. } => Operation::PlaylistLibrary,
        ApiRequest::CreatePlaylist { .. } => Operation::PlaylistCreation,
        ApiRequest::MadeForYou { .. } | ApiRequest::SearchPlaylists { .. } => {
            Operation::PlaylistSearch
        }
        ApiRequest::NewReleases { .. } => Operation::UserData,
        ApiRequest::NewReleaseTracks { .. } => Operation::Catalog,
        ApiRequest::HiddenSongs { .. } | ApiRequest::SetHiddenInPlaylist { .. } => {
            Operation::UserData
        }
        ApiRequest::SearchCatalogue { .. } => Operation::CatalogSearch,
        ApiRequest::Search { .. } => Operation::PlaylistSearch,
        ApiRequest::PlaySearch { kind, .. } if kind == "playlist" => Operation::PlaylistSearch,
        ApiRequest::PlaySearch { .. } => Operation::CatalogSearch,
        ApiRequest::Playlist { id, .. } => Operation::PlaylistMetadata(api.playlist_access(id)),
        ApiRequest::PlaylistItems { id, .. }
        | ApiRequest::PlaylistSample { id, .. }
        | ApiRequest::CheckPlaylistDuplicates {
            playlist_id: id, ..
        } => Operation::PlaylistItems(api.playlist_access(id)),
        ApiRequest::UploadPlaylistCover { id, .. }
        | ApiRequest::UpdatePlaylist { id, .. }
        | ApiRequest::FollowPlaylist { id, .. } => {
            Operation::PlaylistMutation(api.playlist_access(id))
        }
        ApiRequest::AddToPlaylist { playlist_id, .. }
        | ApiRequest::RemoveFromPlaylist { playlist_id, .. }
        | ApiRequest::ReorderPlaylist { playlist_id, .. } => {
            Operation::PlaylistMutation(api.playlist_access(playlist_id))
        }
        ApiRequest::ArtistTopTracks { .. } => Operation::ArtistTopTracks,
        ApiRequest::Recommendations { .. } | ApiRequest::RelatedArtists { .. } => {
            Operation::UnsupportedDevelopmentMode
        }
        ApiRequest::Artist { .. }
        | ApiRequest::ArtistAlbums { .. }
        | ApiRequest::Album { .. }
        | ApiRequest::AlbumTracks { .. }
        | ApiRequest::AlbumQueueTracks { .. }
        | ApiRequest::Show { .. }
        | ApiRequest::ShowEpisodes { .. }
        | ApiRequest::HomeEpisodes { .. }
        | ApiRequest::Track { .. }
        | ApiRequest::Episode { .. } => Operation::Catalog,
    }
}

/// Records returned playlist ownership for later metadata and item routing.
fn observe_playlists(api: &ApiGateway, response: &ApiResponse) {
    match response {
        ApiResponse::MadeForYou {
            result, top_mixes, ..
        } => {
            for playlists in [result, top_mixes].into_iter().flatten() {
                api.observe_playlists(playlists);
            }
        }
        ApiResponse::MyPlaylists {
            result: Ok(page), ..
        } => api.observe_playlists(&page.items),
        ApiResponse::Playlist {
            result: Ok(playlist),
            ..
        }
        | ApiResponse::PlaylistCreated(Ok(playlist)) => api.observe_playlist(playlist),
        ApiResponse::Search {
            result: Ok(results),
            ..
        } => {
            if let Some(playlists) = &results.playlists {
                api.observe_playlists(&playlists.items);
            }
        }
        ApiResponse::SearchPlaylists {
            result: Ok(page), ..
        } => api.observe_playlists(&page.items),
        ApiResponse::PlaylistUpdated {
            id,
            result: Err(error),
        }
        | ApiResponse::PlaylistItemsChanged {
            id,
            result: Err(error),
            ..
        }
        | ApiResponse::PlaylistItems {
            id,
            result: Err(error),
            ..
        }
        | ApiResponse::PlaylistSample {
            id,
            result: Err(error),
            ..
        }
        | ApiResponse::PlaylistDuplicatesChecked {
            playlist_id: id,
            result: Err(error),
            ..
        }
        | ApiResponse::PlaylistFollowChanged {
            id,
            result: Err(error),
            ..
        } if error.status() == Some(403) => {
            api.invalidate_playlist_access(&PlaylistId::new(id.clone()));
        }
        _ => {}
    }
}

const SHARED_MADE_FOR_YOU_TERMS: [&str; 4] =
    ["Discover Weekly", "Release Radar", "Daily Mix", "daylist"];
const UNSUPPORTED_DJ_URI: &str = "spotify:playlist:37i9dQZF1EYkqdzj48dyYq";

/// Search is an approximation, used only when Spotify's personalized Home
/// section cannot be read.
/// Releases from Spotify's own new-releases feed over the playback session,
/// or, when that cannot answer, from the Web API's followed artists.
async fn read_new_releases(
    api: &ApiGateway,
    new_releases: &crate::new_releases::NewReleases,
    engine: Option<&Engine>,
    background_session: &tokio::sync::Semaphore,
) -> ApiResult<Vec<Album>> {
    if let Some(engine) = engine {
        let _permit = background_session.acquire().await.ok();
        match new_releases.feed(engine.session()).await {
            Ok(albums) => {
                log::debug!("Spotify route operation=NewReleases source=session");
                return Ok(albums);
            }
            Err(error) => {
                log::warn!("New releases session read failed; asking the Web API: {error}");
            }
        }
    }
    let account = api.account().map(|account| account.as_str().to_owned());
    if let Some(account) = &account
        && let Some(albums) = new_releases.cached_fallback(account).await
    {
        return Ok(albums);
    }
    let albums = followed_artist_releases(api).await?;
    if let Some(account) = &account {
        new_releases.keep_fallback(account, &albums).await;
    }
    log::debug!("Spotify route operation=NewReleases source=web-api");
    Ok(albums)
}

/// The latest albums and singles of the first followed artists, merged.
async fn followed_artist_releases(api: &ApiGateway) -> ApiResult<Vec<Album>> {
    let client = api.client_for(Operation::UserData).await?;
    let mut artists = Vec::new();
    let mut after: Option<String> = None;
    while artists.len() < crate::new_releases::FALLBACK_ARTISTS {
        let page = client.followed_artists(after.as_deref(), 50).await?;
        let next = page.cursors.and_then(|cursors| cursors.after);
        let empty = page.items.is_empty();
        artists.extend(page.items);
        match next {
            Some(next) if !empty => after = Some(next),
            _ => break,
        }
    }
    artists.truncate(crate::new_releases::FALLBACK_ARTISTS);
    let catalog = api.client_for(Operation::Catalog).await?;
    let _permit = catalog.background_permit().await;
    let mut reads = tokio::task::JoinSet::new();
    for artist in artists {
        let catalog = Arc::clone(&catalog);
        reads.spawn(async move {
            catalog
                .artist_albums(&artist.id, "album,single", 0, NEW_RELEASES_PER_ARTIST)
                .await
        });
    }
    let mut pages = Vec::new();
    let mut failure = None;
    while let Some(joined) = reads.join_next().await {
        let Ok(result) = joined else {
            continue;
        };
        match result {
            Ok(page) => pages.push(page.items),
            // One artist gone answers alone; a rate limit, quota or lost
            // sign-in would answer the same for every artist left.
            Err(error) => {
                let stop = !matches!(error, ApiError::Status { .. } | ApiError::Decode(_));
                failure.get_or_insert(error);
                if stop {
                    reads.abort_all();
                    break;
                }
            }
        }
    }
    match failure {
        Some(error) if pages.is_empty() => Err(error),
        _ => Ok(crate::new_releases::merge_artist_releases(pages)),
    }
}

/// The songs of the releases on the New releases page, over the playback
/// session when it can answer, otherwise album by album from the Web API.
async fn read_new_release_tracks(
    api: &ApiGateway,
    engine: Option<&Engine>,
    background_session: &tokio::sync::Semaphore,
    albums: &[Album],
) -> ApiResult<Vec<Track>> {
    if let Some(engine) = engine {
        let _permit = background_session.acquire().await.ok();
        match session_reads::album_tracks(engine.session(), albums).await {
            Ok(tracks) => return Ok(tracks),
            Err(session_reads::Failure::Definitive(error)) => return Err(error),
            Err(session_reads::Failure::Retry(error)) => {
                log::debug!("new release songs session read failed: {error}; asking the Web API");
            }
        }
    }
    let catalog = api.client_for(Operation::Catalog).await?;
    let _permit = catalog.background_permit().await;
    let mut reads = tokio::task::JoinSet::new();
    for (index, album) in albums.iter().cloned().enumerate() {
        let catalog = Arc::clone(&catalog);
        reads.spawn(async move {
            let result = catalog.album_tracks(&album.id, 0, 50).await;
            (index, album, result)
        });
    }
    let mut answers = Vec::new();
    let mut failure = None;
    while let Some(joined) = reads.join_next().await {
        let Ok((index, album, result)) = joined else {
            continue;
        };
        match result {
            Ok(page) => answers.push((index, album, page.items)),
            Err(error) => {
                let stop = !matches!(error, ApiError::Status { .. } | ApiError::Decode(_));
                failure.get_or_insert(error);
                if stop {
                    reads.abort_all();
                    break;
                }
            }
        }
    }
    answers.sort_by_key(|(index, ..)| *index);
    let tracks: Vec<Track> = answers
        .into_iter()
        .flat_map(|(_, album, tracks)| {
            tracks.into_iter().map(move |mut track| {
                track.album = Some(album.clone());
                track
            })
        })
        .collect();
    match failure {
        Some(error) if tracks.is_empty() => Err(error),
        _ => Ok(tracks),
    }
}

/// The playback session of the signed-in account, for what only it can do.
fn playback_session<'a>(
    api: &ApiGateway,
    engine: Option<&'a Engine>,
) -> ApiResult<&'a librespot_core::session::Session> {
    engine
        .filter(|engine| same_account(&engine.session().username(), api.account().as_ref()))
        .map(Engine::session)
        .ok_or_else(|| ApiError::Status {
            status: 503,
            message: "Hiding songs needs playback on this computer.".into(),
        })
}

async fn shared_made_for_you(api: &ApiGateway) -> ApiResult<Vec<Playlist>> {
    let client = api.client_for(Operation::PlaylistSearch).await?;
    let _background_permit = client.background_permit().await;
    let mut playlists = Vec::new();
    let mut seen = HashSet::new();
    for term in SHARED_MADE_FOR_YOU_TERMS {
        let results = client.search(term, &["playlist"]).await?;
        let mut matching: Vec<_> = results
            .playlists
            .into_iter()
            .flat_map(|page| page.items)
            .filter(|playlist| shared_made_for_you_match(playlist, term))
            .filter(|playlist| seen.insert(playlist.name.to_ascii_lowercase()))
            .take(6)
            .collect();
        playlists.append(&mut matching);
    }
    log::debug!("Spotify route operation=MadeForYou source=shared-fallback");
    Ok(playlists)
}

/// Accepts only Spotify-owned fallback matches with supported playlist URIs.
fn shared_made_for_you_match(playlist: &Playlist, term: &str) -> bool {
    let name = playlist.name.trim();
    let matches_name = name.eq_ignore_ascii_case(term)
        || (term == "Daily Mix"
            && name.strip_prefix("Daily Mix ").is_some_and(|number| {
                !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit())
            }));
    matches_name
        && playlist.uri != UNSUPPORTED_DJ_URI
        && playlist.owner.id.as_deref() == Some("spotify")
}

/// Searches through the shared app only when the playback-session read fails.
async fn with_shared_made_for_you_fallback<F, Fut>(
    session_result: ApiResult<Vec<Playlist>>,
    fallback: F,
) -> (ApiResult<Vec<Playlist>>, bool, Vec<ApiSource>)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ApiResult<Vec<Playlist>>>,
{
    match session_result {
        Ok(playlists) => (Ok(playlists), true, Vec::new()),
        Err(error) => {
            let mut expired = Vec::new();
            if let ApiError::SignInExpired { api_source } = &error {
                expired.push(*api_source);
            }
            log::warn!("Made for you session read failed; trying shared search fallback: {error}");
            let result = fallback().await;
            if let Err(ApiError::SignInExpired { api_source }) = &result
                && !expired.contains(api_source)
            {
                expired.push(*api_source);
            }
            (result, false, expired)
        }
    }
}

/// Tries the session Home route before the shared fallback and routes other API work.
async fn handle(
    api: &ApiGateway,
    made_for_you: &crate::made_for_you::MadeForYou,
    new_releases: &crate::new_releases::NewReleases,
    connect: &crate::sonos::Controller,
    engine: Option<&Engine>,
    background_session: &tokio::sync::Semaphore,
    request: ApiRequest,
) -> (ApiResponse, Vec<ApiSource>) {
    if let ApiRequest::MadeForYou { generation, force } = request {
        let shelves = match engine
            .filter(|engine| same_account(&engine.session().username(), api.account().as_ref()))
        {
            Some(engine) => {
                let _background_permit = background_session.acquire().await.ok();
                let playback_account = engine.session().username();
                made_for_you
                    .load(engine.session(), force, || {
                        api.home_context_token(&playback_account)
                    })
                    .await
            }
            None => Err(ApiError::Status {
                status: 503,
                message: "The playback session is unavailable for Made for you.".into(),
            }),
        };
        let (session_result, top_mixes) = match shelves {
            Ok(shelves) => {
                if let Err(error) = &shelves.top_mixes {
                    log::warn!("Your top mixes session read failed: {error}");
                }
                (Ok(shelves.made_for_you), shelves.top_mixes)
            }
            Err(error) => (Err(error.clone()), Err(error)),
        };
        let (result, personalized, expired) =
            with_shared_made_for_you_fallback(session_result, || async {
                shared_made_for_you(api).await
            })
            .await;
        return (
            ApiResponse::MadeForYou {
                generation,
                personalized,
                result,
                top_mixes,
            },
            expired,
        );
    }
    let session_engine =
        engine.filter(|engine| same_account(&engine.session().username(), api.account().as_ref()));
    if let ApiRequest::NewReleases { generation } = request {
        let mut expired = Vec::new();
        let result =
            match read_new_releases(api, new_releases, session_engine, background_session).await {
                Err(ApiError::SignInExpired { api_source }) => {
                    expired.push(api_source);
                    Err(ApiError::SignInExpired { api_source })
                }
                result => result,
            };
        return (ApiResponse::NewReleases { generation, result }, expired);
    }
    if let ApiRequest::NewReleaseTracks { albums, generation } = request {
        let mut expired = Vec::new();
        let result =
            match read_new_release_tracks(api, session_engine, background_session, &albums).await {
                Err(ApiError::SignInExpired { api_source }) => {
                    expired.push(api_source);
                    Err(ApiError::SignInExpired { api_source })
                }
                result => result,
            };
        return (
            ApiResponse::NewReleaseTracks { generation, result },
            expired,
        );
    }
    if let ApiRequest::HiddenSongs { generation } = request {
        let result = match playback_session(api, engine) {
            Ok(session) => {
                let _permit = background_session.acquire().await.ok();
                session_reads::hidden_songs(session)
                    .await
                    .map_err(|error| ApiError::Network(format!("{error:#}")))
            }
            Err(error) => Err(error),
        };
        return (ApiResponse::HiddenSongs { generation, result }, Vec::new());
    }
    if let ApiRequest::SetHiddenInPlaylist {
        playlist_uri,
        track_uri,
        hidden,
    } = request
    {
        let result = match playback_session(api, engine) {
            Ok(session) => session_reads::set_hidden(
                session,
                &playlist_uri,
                &track_uri,
                hidden,
                jiff::Timestamp::now().as_second(),
            )
            .await
            .map_err(|error| ApiError::Network(format!("{error:#}"))),
            Err(error) => Err(error),
        };
        return (
            ApiResponse::HiddenInPlaylistSet {
                playlist_uri,
                track_uri,
                hidden,
                result,
            },
            Vec::new(),
        );
    }
    let operation = operation_for(api, &request);
    // A session whose long-lived connection has dropped still answers over
    // its HTTP client, so the engine's presence is the only liveness test;
    // a read the session truly cannot make falls back to the Web API below.
    if (api.session_serves(operation) || library_over_session(api, operation).await)
        && let Some(engine) = engine
            .filter(|engine| same_account(&engine.session().username(), api.account().as_ref()))
    {
        let response = {
            let _background_permit = if request.background() {
                background_session.acquire().await.ok()
            } else {
                None
            };
            over_session(engine, &request).await
        };
        if let Some(response) = response {
            log::debug!("Spotify route operation={operation:?} source=session");
            observe_playlists(api, &response);
            return (response, Vec::new());
        }
    }
    if let Some(engine) =
        engine.filter(|engine| same_account(&engine.session().username(), api.account().as_ref()))
        && let Some(response) = over_connect(engine, connect, &request).await
    {
        log::debug!("Spotify route operation={operation:?} source=connect");
        return (response, Vec::new());
    }
    let selected = api.client_for(operation).await;
    let _background_permit = if request.background() {
        match &selected {
            Ok(client) => client.background_permit().await,
            Err(_) => None,
        }
    } else {
        None
    };
    let expired = std::cell::Cell::new(None);
    macro_rules! routed {
        ($method:ident($($argument:expr),* $(,)?)) => {{
            let result = match &selected {
                Ok(client) => client.$method($($argument),*).await,
                Err(error) => Err(error.clone()),
            };
            if let Err(ApiError::SignInExpired { api_source }) = &result {
                expired.set(Some(*api_source));
            }
            result
        }};
    }

    let response = match request {
        ApiRequest::Me => ApiResponse::Me(routed!(me())),
        ApiRequest::Devices => ApiResponse::Devices(routed!(devices())),
        ApiRequest::PlaybackState { seq } => ApiResponse::PlaybackState {
            seq,
            result: routed!(playback_state()),
        },
        ApiRequest::Queue { seq } => ApiResponse::Queue {
            seq,
            result: routed!(queue()),
        },
        ApiRequest::RecentlyPlayed {
            who,
            generation,
            before,
            limit,
        } => ApiResponse::RecentlyPlayed {
            who,
            generation,
            limit,
            result: routed!(recently_played(limit, None, before.as_deref())),
        },
        ApiRequest::TopTracks {
            offset,
            full,
            generation,
        } => ApiResponse::TopTracks {
            result: routed!(top_tracks("short_term", if full { 50 } else { 20 }, offset)),
            offset,
            full,
            generation,
        },
        ApiRequest::TopArtists { generation } => ApiResponse::TopArtists {
            generation,
            result: routed!(top_artists("medium_term", 20)).map(|page| page.items),
        },
        ApiRequest::Recommendations {
            seed_tracks,
            seed_artists,
            generation,
        } => ApiResponse::Recommendations {
            generation,
            result: routed!(recommendations(&seed_tracks, &seed_artists, 20)),
        },
        ApiRequest::MadeForYou { .. }
        | ApiRequest::NewReleases { .. }
        | ApiRequest::NewReleaseTracks { .. }
        | ApiRequest::HiddenSongs { .. }
        | ApiRequest::SetHiddenInPlaylist { .. } => unreachable!("handled before Web API routing"),
        ApiRequest::MyPlaylists { offset, generation } => ApiResponse::MyPlaylists {
            offset,
            generation,
            result: routed!(my_playlists(offset, 50)),
        },
        ApiRequest::Playlist { id, generation } => ApiResponse::Playlist {
            result: routed!(playlist(&id)),
            id,
            generation,
        },
        ApiRequest::PlaylistItems {
            id,
            offset,
            generation,
        } => ApiResponse::PlaylistItems {
            result: routed!(playlist_items(&id, offset, PLAYLIST_PAGE_SIZE)),
            id,
            offset,
            generation,
        },
        ApiRequest::PlaylistSample {
            id,
            offset,
            generation,
        } => ApiResponse::PlaylistSample {
            result: routed!(playlist_items(&id, offset, PLAYLIST_PAGE_SIZE)),
            id,
            generation,
        },
        ApiRequest::CreatePlaylist {
            name,
            public,
            description,
        } => ApiResponse::PlaylistCreated(routed!(create_playlist(&name, public, &description))),
        ApiRequest::UploadPlaylistCover {
            id,
            request,
            previous_urls,
            cover,
        } => ApiResponse::PlaylistCoverUploaded {
            request,
            previous_urls,
            result: routed!(upload_playlist_cover(&id, &cover.encoded)),
            id,
            cover,
        },
        ApiRequest::UpdatePlaylist {
            id,
            name,
            description,
            public,
        } => ApiResponse::PlaylistUpdated {
            result: routed!(update_playlist(
                &id,
                name.as_deref(),
                description.as_deref(),
                public
            )),
            id,
        },
        ApiRequest::CheckPlaylistDuplicates {
            playlist_id,
            playlist_name,
            items,
            position,
        } => {
            let uris: Vec<String> = items.iter().map(|item| item.uri().to_string()).collect();
            ApiResponse::PlaylistDuplicatesChecked {
                result: routed!(playlist_duplicates(&playlist_id, &uris)),
                playlist_id,
                playlist_name,
                items,
                position,
            }
        }
        ApiRequest::AddToPlaylist {
            playlist_id,
            playlist_name,
            uris,
            position,
        } => ApiResponse::PlaylistItemsChanged {
            result: routed!(add_playlist_items(&playlist_id, &uris, position)),
            id: playlist_id,
            message: format!("Added to {playlist_name}"),
        },
        ApiRequest::RemoveFromPlaylist {
            playlist_id,
            uris,
            snapshot_id,
        } => ApiResponse::PlaylistItemsChanged {
            result: routed!(remove_playlist_items(
                &playlist_id,
                &uris,
                snapshot_id.as_deref()
            )),
            id: playlist_id,
            message: "Removed from playlist".to_string(),
        },
        ApiRequest::ReorderPlaylist {
            playlist_id,
            range_start,
            insert_before,
            snapshot_id,
        } => ApiResponse::PlaylistItemsChanged {
            result: routed!(reorder_playlist(
                &playlist_id,
                range_start,
                insert_before,
                snapshot_id.as_deref()
            )),
            id: playlist_id,
            message: String::new(),
        },
        ApiRequest::FollowPlaylist { id, follow } => ApiResponse::PlaylistFollowChanged {
            result: if follow {
                routed!(follow_playlist(&id))
            } else {
                routed!(unfollow_playlist(&id))
            },
            id,
            followed: follow,
        },
        ApiRequest::SavedTracks { offset, generation } => ApiResponse::SavedTracks {
            offset,
            generation,
            account_id: api.account().map(|account| account.as_str().to_string()),
            result: routed!(saved_tracks(offset, 50)),
        },
        ApiRequest::SavedAlbums { offset } => ApiResponse::SavedAlbums {
            offset,
            result: routed!(saved_albums(offset, 50)),
        },
        ApiRequest::FollowedArtists { after } => ApiResponse::FollowedArtists {
            result: routed!(followed_artists(after.as_deref(), 50)),
            after,
        },
        ApiRequest::SavedShows { offset } => ApiResponse::SavedShows {
            offset,
            result: routed!(saved_shows(offset, 50)),
        },
        ApiRequest::SavedEpisodes { offset } => ApiResponse::SavedEpisodes {
            offset,
            result: routed!(saved_episodes(offset, 50)),
        },
        ApiRequest::SetSaved { uris, saved } => ApiResponse::SavedChanged {
            result: if saved {
                routed!(save(&uris))
            } else {
                routed!(unsave(&uris))
            },
            uris,
            saved,
        },
        ApiRequest::Contains { uris } => ApiResponse::Contains {
            result: routed!(contains(&uris)),
            uris,
        },
        ApiRequest::Search { query, serial } => ApiResponse::Search {
            result: routed!(search(
                &query,
                &["track", "artist", "album", "playlist", "show", "episode"]
            )),
            query,
            serial,
        },
        ApiRequest::SearchCatalogue { query, serial } => ApiResponse::Search {
            result: routed!(search(
                &query,
                &["track", "artist", "album", "show", "episode"]
            )),
            query,
            serial,
        },
        ApiRequest::PlaySearch {
            kind,
            query,
            serial,
        } => ApiResponse::PlaySearch {
            result: routed!(search(&query, &[kind.as_str()])).map(|results| {
                // Spotify ranks each kind by relevance; the first is the match.
                match kind.as_str() {
                    "album" => results
                        .albums
                        .and_then(|p| p.items.into_iter().next())
                        .map(|a| a.uri),
                    "artist" => results
                        .artists
                        .and_then(|p| p.items.into_iter().next())
                        .map(|a| a.uri),
                    "playlist" => results
                        .playlists
                        .and_then(|p| p.items.into_iter().next())
                        .map(|p| p.uri),
                    _ => results
                        .tracks
                        .and_then(|p| p.items.into_iter().next())
                        .map(|t| t.uri),
                }
            }),
            query,
            serial,
        },
        ApiRequest::SearchPlaylists { query, serial } => ApiResponse::SearchPlaylists {
            result: routed!(search(&query, &["playlist"]))
                .map(|results| results.playlists.unwrap_or_default()),
            query,
            serial,
        },
        ApiRequest::Artist { id } => ApiResponse::Artist {
            result: routed!(artist(&id)),
            id,
        },
        ApiRequest::ArtistTopTracks { id } => ApiResponse::ArtistTopTracks {
            result: routed!(artist_top_tracks(&id)),
            id,
        },
        ApiRequest::ArtistAlbums { id, groups, offset } => ApiResponse::ArtistAlbums {
            result: routed!(artist_albums(&id, &groups, offset, 50)),
            id,
            groups,
            offset,
        },
        ApiRequest::RelatedArtists { id } => ApiResponse::RelatedArtists {
            result: routed!(related_artists(&id)),
            id,
        },
        ApiRequest::Album { id } => ApiResponse::Album {
            result: routed!(album(&id)),
            id,
        },
        ApiRequest::AlbumTracks {
            id,
            offset,
            generation,
        } => ApiResponse::AlbumTracks {
            generation,
            result: routed!(album_tracks(&id, offset, 50)),
            id,
            offset,
        },
        ApiRequest::AlbumQueueTracks {
            id,
            offset,
            request,
        } => ApiResponse::AlbumQueueTracks {
            result: routed!(album_tracks(&id, offset, 50)),
            offset,
            request,
        },
        ApiRequest::Show { id } => ApiResponse::Show {
            result: routed!(show(&id)),
            id,
        },
        ApiRequest::ShowEpisodes { id, offset } => ApiResponse::ShowEpisodes {
            result: routed!(show_episodes(&id, offset, 50)),
            id,
            offset,
        },
        ApiRequest::HomeEpisodes {
            shows: asked,
            generation,
        } => {
            let mut answers = Vec::new();
            let mut failure = selected.as_ref().err().cloned();
            let mut reads = tokio::task::JoinSet::new();
            if let Ok(client) = &selected {
                for (index, show) in asked.into_iter().enumerate() {
                    let client = Arc::clone(client);
                    reads.spawn(async move {
                        let result = client
                            .show_episodes(&show.id, 0, HOME_EPISODES_PER_SHOW)
                            .await;
                        (index, show, result)
                    });
                }
            }
            while let Some(joined) = reads.join_next().await {
                // A read that panicked has already reported it; the others
                // still answer for their own shows.
                let Ok((index, show, result)) = joined else {
                    continue;
                };
                match result {
                    Ok(page) => answers.push((index, show, page.items)),
                    // A show that is gone answers on its own; a rate limit,
                    // an exhausted quota or a lost sign-in would answer the
                    // same for every show still to come, so stop the rest.
                    Err(error) => {
                        if let ApiError::SignInExpired { api_source } = &error {
                            expired.set(Some(*api_source));
                        }
                        let stop = !matches!(error, ApiError::Status { .. } | ApiError::Decode(_));
                        failure.get_or_insert(error);
                        if stop {
                            reads.abort_all();
                            break;
                        }
                    }
                }
            }
            answers.sort_by_key(|(index, ..)| *index);
            let shows: Vec<_> = answers
                .into_iter()
                .map(|(_, show, items)| (show, items))
                .collect();
            ApiResponse::HomeEpisodes {
                generation,
                result: match failure {
                    Some(error) if shows.is_empty() => Err(error),
                    _ => Ok(shows),
                },
            }
        }
        ApiRequest::Track { id } => ApiResponse::Track {
            result: routed!(track(&id)),
            id,
        },
        ApiRequest::Episode { id } => ApiResponse::Episode {
            result: routed!(episode(&id)),
            id,
        },
        ApiRequest::Remote {
            action,
            device_id,
            play,
            position_ms,
            percent,
            flag,
            repeat,
        } => {
            let device = device_id.as_deref();
            let result = match action {
                RemoteAction::Play => routed!(play(device, play.as_ref())),
                RemoteAction::Pause => routed!(pause(device)),
                RemoteAction::Next => routed!(next(device)),
                RemoteAction::Previous => routed!(previous(device)),
                RemoteAction::Seek => routed!(seek(position_ms, device)),
                RemoteAction::Volume => routed!(set_volume(percent, device)),
                RemoteAction::Shuffle => routed!(set_shuffle(flag, device)),
                RemoteAction::Repeat => routed!(set_repeat(&repeat, device)),
            };
            ApiResponse::Remote { action, result }
        }
        ApiRequest::ShufflePlay { device_id, play } => {
            let device = device_id.as_deref();
            let result = match routed!(set_shuffle(true, device)) {
                Ok(()) => routed!(play(device, Some(&play))),
                Err(error) => Err(error),
            };
            ApiResponse::Remote {
                action: RemoteAction::Play,
                result,
            }
        }
        ApiRequest::Transfer { device_id, play } => ApiResponse::Transferred {
            result: routed!(transfer(&device_id, play)),
            device_id,
        },
        ApiRequest::AddToQueue {
            uri,
            device_id,
            label,
        } => ApiResponse::QueueAdded {
            result: routed!(add_to_queue(&uri, device_id.as_deref())),
            label,
        },
        ApiRequest::AddManyToQueue {
            request,
            uris,
            device_id,
        } => {
            let (added, result) = match &selected {
                Ok(client) => client.add_many_to_queue(&uris, device_id.as_deref()).await,
                Err(error) => (0, Err(error.clone())),
            };
            if let Err(ApiError::SignInExpired { api_source }) = &result {
                expired.set(Some(*api_source));
            }
            ApiResponse::QueueBatchAdded {
                request,
                added,
                result,
            }
        }
    };
    observe_playlists(api, &response);
    (response, expired.get().into_iter().collect())
}

/// Player commands for a device the Web API refuses (a Sonos, say) go over
/// the session's Connect state service instead. `None` leaves the request
/// to the Web API. See [`crate::sonos`].
async fn over_connect(
    engine: &Engine,
    connect: &crate::sonos::Controller,
    request: &ApiRequest,
) -> Option<ApiResponse> {
    let session = engine.session();
    Some(match request {
        ApiRequest::Remote {
            action,
            device_id,
            play,
            position_ms,
            percent,
            flag,
            repeat,
        } => {
            let to = connect.restricted_target(device_id.as_deref())?;
            let result = match crate::sonos::command_body(
                *action,
                play.as_ref(),
                *position_ms,
                *flag,
                repeat,
            ) {
                Some(body) => crate::sonos::command(session, &to, &body).await,
                None => crate::sonos::set_volume(session, &to, *percent).await,
            };
            ApiResponse::Remote {
                action: *action,
                result,
            }
        }
        ApiRequest::ShufflePlay { device_id, play } => {
            let to = connect.restricted_target(device_id.as_deref())?;
            let shuffle = crate::sonos::command_body(RemoteAction::Shuffle, None, 0, true, "")?;
            let start = crate::sonos::command_body(RemoteAction::Play, Some(play), 0, false, "")?;
            let result = shuffle_then_play(shuffle, start, |body| {
                let to = &to;
                async move { crate::sonos::command(session, to, &body).await }
            })
            .await;
            ApiResponse::Remote {
                action: RemoteAction::Play,
                result,
            }
        }
        ApiRequest::Transfer { device_id, .. } => {
            connect.restricted_target(Some(device_id))?;
            ApiResponse::Transferred {
                result: connect.transfer(session, device_id).await,
                device_id: device_id.clone(),
            }
        }
        ApiRequest::AddToQueue {
            uri,
            device_id,
            label,
        } => {
            let to = connect.restricted_target(device_id.as_deref())?;
            let (_, result) = connect
                .add_to_queue(session, &to, std::slice::from_ref(uri))
                .await;
            ApiResponse::QueueAdded {
                result,
                label: label.clone(),
            }
        }
        ApiRequest::AddManyToQueue {
            request,
            uris,
            device_id,
        } => {
            let to = connect.restricted_target(device_id.as_deref())?;
            let (added, result) = connect.add_to_queue(session, &to, uris).await;
            ApiResponse::QueueBatchAdded {
                request: *request,
                added,
                result,
            }
        }
        _ => return None,
    })
}

/// Waits for shuffle to succeed before starting playback, preserving either error.
async fn shuffle_then_play<F, Fut>(
    shuffle: serde_json::Value,
    play: serde_json::Value,
    mut send: F,
) -> ApiResult<()>
where
    F: FnMut(serde_json::Value) -> Fut,
    Fut: std::future::Future<Output = ApiResult<()>>,
{
    send(shuffle).await?;
    send(play).await
}

/// Whether a session signed in as `username` answers for the Web API's
/// account. Local playback is approved separately, and another account's
/// view of a playlist is not this one's.
fn same_account(username: &str, account: Option<&AccountId>) -> bool {
    account.is_some_and(|account| account.as_str() == username)
}

/// How long a session read may take before the Web API is asked instead:
/// what the Web API's own requests get. A stalled line otherwise waits on
/// the operating system, which is far longer than a page should spin.
const SESSION_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether the playlist library is read from the playback session's
/// rootlist: when the shared app, which lists it with every field, cannot
/// answer now, because it is still verifying sign-in or waiting out a rate
/// limit (#698, #816). A personal app lists only part of it, leaving out
/// Spotify's own playlists, so it is no substitute.
async fn library_over_session(api: &ApiGateway, operation: Operation) -> bool {
    operation == Operation::PlaylistLibrary && !api.shared_available().await
}

/// Answers a playlist or popular-track read over the streaming session. `None` when the
/// session could not, leaving the request to the Web API.
async fn over_session(engine: &Engine, request: &ApiRequest) -> Option<ApiResponse> {
    let session = engine.session();
    let read = async {
        Some(match session_read(request)? {
            SessionRead::Header { id } => SessionAnswer::Header(Box::new(settle(
                session_reads::playlist(session, id).await,
            )?)),
            SessionRead::Rows { id, offset } => SessionAnswer::Rows(settle(
                session_reads::items(session, id, offset, PLAYLIST_PAGE_SIZE).await,
            )?),
            SessionRead::Sample { id, offset } => SessionAnswer::Rows(settle(
                session_reads::sample(session, id, offset, PLAYLIST_PAGE_SIZE).await,
            )?),
            SessionRead::ArtistTop { id } => SessionAnswer::ArtistTop(settle(
                session_reads::artist_top_tracks(session, id).await,
            )?),
            SessionRead::Library => {
                SessionAnswer::Library(settle(session_reads::library(session).await)?)
            }
        })
    };
    let Ok(answer) = tokio::time::timeout(SESSION_READ_TIMEOUT, read).await else {
        log::debug!("session read timed out; asking the Web API");
        return None;
    };
    session_response(request, answer?)
}

/// The read a request asks of the session: a playlist's header, a page of
/// its rows, or a sample of who added them, which needs no song details.
/// Anything else, a duplicate check among them, is the Web API's even
/// where the session serves the operation.
#[derive(Debug, PartialEq)]
enum SessionRead<'a> {
    ArtistTop {
        id: &'a str,
    },
    Header {
        id: &'a str,
    },
    Rows {
        id: &'a str,
        offset: u32,
    },
    Sample {
        id: &'a str,
        offset: u32,
    },
    /// The whole playlist library, whichever page was asked for.
    Library,
}

fn session_read(request: &ApiRequest) -> Option<SessionRead<'_>> {
    Some(match request {
        ApiRequest::ArtistTopTracks { id } => SessionRead::ArtistTop { id },
        ApiRequest::Playlist { id, .. } => SessionRead::Header { id },
        ApiRequest::PlaylistItems { id, offset, .. } => SessionRead::Rows {
            id,
            offset: *offset,
        },
        ApiRequest::PlaylistSample { id, offset, .. } => SessionRead::Sample {
            id,
            offset: *offset,
        },
        ApiRequest::MyPlaylists { .. } => SessionRead::Library,
        _ => return None,
    })
}

/// What the session read: a playlist's header, a page of its rows, or the
/// whole playlist library.
enum SessionAnswer {
    ArtistTop(ApiResult<Vec<Track>>),
    Header(Box<ApiResult<Playlist>>),
    Rows(ApiResult<Page<PlaylistItem>>),
    Library(ApiResult<Vec<Playlist>>),
}

/// The response a session answer becomes, carrying the request's own id,
/// offset, and generation so the app matches it to the page that asked.
fn session_response(request: &ApiRequest, answer: SessionAnswer) -> Option<ApiResponse> {
    Some(match (request, answer) {
        (ApiRequest::ArtistTopTracks { id }, SessionAnswer::ArtistTop(result)) => {
            ApiResponse::ArtistTopTracks {
                id: id.clone(),
                result,
            }
        }
        (ApiRequest::Playlist { id, generation }, SessionAnswer::Header(result)) => {
            ApiResponse::Playlist {
                id: id.clone(),
                generation: *generation,
                result: *result,
            }
        }
        (
            ApiRequest::PlaylistItems {
                id,
                offset,
                generation,
            },
            SessionAnswer::Rows(result),
        ) => ApiResponse::PlaylistItems {
            id: id.clone(),
            offset: *offset,
            generation: *generation,
            result,
        },
        (ApiRequest::PlaylistSample { id, generation, .. }, SessionAnswer::Rows(result)) => {
            ApiResponse::PlaylistSample {
                id: id.clone(),
                generation: *generation,
                result,
            }
        }
        // The session reads the library whole, so its answer starts the
        // list over at the top, whichever page of a Web API load asked: it
        // holds every row that load had shown, and the rest.
        (ApiRequest::MyPlaylists { generation, .. }, SessionAnswer::Library(result)) => {
            ApiResponse::MyPlaylists {
                offset: 0,
                generation: *generation,
                result: result.map(Page::whole),
            }
        }
        _ => return None,
    })
}

/// A session answer as the Web API would have given it. A final refusal is
/// shown as such; a dropped line leaves the Web API to try.
fn settle<T>(result: Result<T, session_reads::Failure>) -> Option<ApiResult<T>> {
    match result {
        Ok(value) => Some(Ok(value)),
        Err(session_reads::Failure::Definitive(error)) => Some(Err(error)),
        Err(session_reads::Failure::Retry(error)) => {
            log::debug!("session read failed: {error}; asking the Web API");
            None
        }
    }
}

/// Spotify's transcription of the track, when the local session can ask for
/// one. Answers are cached like LRCLIB's, "none" included; `None` falls
/// back to LRCLIB.
async fn spotify_lyrics(
    engine: Option<Arc<Engine>>,
    uri: &str,
    cache_dir: &std::path::Path,
) -> Option<crate::lyrics::Lyrics> {
    let id = uri.strip_prefix("spotify:track:")?;
    let path = cache_dir.join(format!("spotify-{id}.json"));
    if let Some(cached) = crate::lyrics::cached(&path) {
        return cached;
    }
    match engine?.lyrics_json(uri).await {
        Ok(json) => {
            let found = json.as_ref().and_then(crate::lyrics::from_spotify);
            crate::lyrics::store(&path, &found);
            found
        }
        Err(error) => {
            log::debug!("spotify lyrics unavailable: {error:#}");
            None
        }
    }
}

/// A playlist's items on disk, valid for exactly one snapshot.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedPlaylist {
    snapshot: String,
    items: Vec<PlaylistItem>,
    /// Absent in the original whole-playlist cache format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total: Option<u32>,
    /// A value means this is a prefix. Absent means the cache is complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next_offset: Option<u32>,
}

#[cfg(test)]
async fn read_cached_playlist(path: std::path::PathBuf) -> std::io::Result<CachedPlaylist> {
    tokio::task::spawn_blocking(move || read_cached_playlist_file(&path))
        .await
        .map_err(std::io::Error::other)?
}

fn read_cached_playlist_file(path: &std::path::Path) -> std::io::Result<CachedPlaylist> {
    let file = std::fs::File::open(path)?;
    // Parse on the file worker without keeping the entire JSON alongside
    // the deserialized playlist. Snapshot/count validation still follows.
    serde_json::from_reader(std::io::BufReader::new(file)).map_err(std::io::Error::other)
}

async fn read_playlist_cache(path: std::path::PathBuf) -> std::io::Result<(CachedPlaylist, bool)> {
    tokio::task::spawn_blocking(move || {
        // A writer may have created a row file that its manifest does not yet
        // reference. Hold the account lock through both reading and recovery.
        let lock = playlist_cache_lock(&path);
        if let Err(error) = &lock {
            log::warn!("unable to lock playlist cache {}: {error}", path.display());
        }
        let cache = read_incremental_playlist_cache_file(&path)
            .map(|cache| (cache, true))
            // A missing or damaged new cache must not hide an older JSON cache.
            .or_else(|_| read_cached_playlist_file(&path).map(|cache| (cache, false)));
        if lock.is_ok()
            && let Err(error) = cleanup_unreferenced_playlist_rows(&path)
        {
            log::warn!("unable to clean playlist cache {}: {error}", path.display());
        }
        cache
    })
    .await
    .map_err(std::io::Error::other)?
}

#[cfg(test)]
async fn write_cached_playlist(
    path: std::path::PathBuf,
    cached: CachedPlaylist,
) -> std::io::Result<()> {
    // Move the checkpoint into the file worker without another model clone.
    // Awaiting it preserves checkpoint order, including after playlist edits.
    tokio::task::spawn_blocking(move || write_cached_playlist_file(&path, &cached))
        .await
        .map_err(std::io::Error::other)?
}

#[cfg(test)]
fn write_cached_playlist_file(
    path: &std::path::Path,
    cached: &CachedPlaylist,
) -> std::io::Result<()> {
    use std::io::Write;

    fastframe_file::write_with(path, fastframe_file::Access::Default, |file| {
        // Buffer small serializer writes without retaining the whole JSON file.
        let mut writer = std::io::BufWriter::new(file);
        serde_json::to_writer(&mut writer, cached).map_err(std::io::Error::other)?;
        writer.flush()
    })
}

/// The manifest stays constant-sized as the row file grows. A reader only
/// consumes `bytes`, so an interrupted append cannot become visible.
#[derive(serde::Serialize, serde::Deserialize)]
struct PlaylistCacheManifest {
    version: u8,
    snapshot: String,
    data_file: u64,
    bytes: u64,
    rows: u32,
    total: u32,
    next_offset: Option<u32>,
}

fn playlist_manifest_path(path: &std::path::Path) -> std::path::PathBuf {
    path.with_extension("manifest.json")
}

fn playlist_data_path(path: &std::path::Path, data_file: u64) -> std::path::PathBuf {
    path.with_extension(format!("rows.{data_file:016x}"))
}

fn log_cache_removal(path: &std::path::Path, result: std::io::Result<()>) {
    if let Err(error) = result
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("unable to remove {}: {error}", path.display());
    }
}

/// Removes every account's cached playlist pages. Each account's pages are
/// emptied under its lock, so a reader never sees half of them; the
/// unlocked lock file and the directories go after.
fn purge_playlist_caches(root: &std::path::Path) {
    if let Ok(accounts) = std::fs::read_dir(root) {
        for account in accounts.flatten() {
            let dir = account.path();
            if !dir.is_dir() {
                continue;
            }
            let lock = match playlist_cache_lock(&dir.join(".purge")) {
                Ok(lock) => lock,
                Err(error) => {
                    log::warn!("unable to lock {}: {error}", dir.display());
                    continue;
                }
            };
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if entry.file_name() == ".playlist-cache.lock" {
                    continue;
                }
                let removed = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                log_cache_removal(&path, removed);
            }
            drop(lock);
        }
    }
    log_cache_removal(root, std::fs::remove_dir_all(root));
}

fn playlist_cache_lock(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "playlist cache has no parent",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(".playlist-cache.lock"))?;
    file.lock()?;
    Ok(file)
}

/// Recover row files from interrupted replacements. Run under the account
/// lock so a new, unpublished row file cannot be mistaken for an orphan.
fn cleanup_unreferenced_playlist_rows(path: &std::path::Path) -> std::io::Result<()> {
    let current = match read_playlist_manifest(path) {
        Ok(manifest) => Some(manifest.data_file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "playlist cache has no parent",
        )
    })?;
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid playlist cache name",
            )
        })?;
    let prefix = format!("{stem}.rows.");
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        if suffix.len() != 16 || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let data_file = u64::from_str_radix(suffix, 16).map_err(std::io::Error::other)?;
        if Some(data_file) == current {
            continue;
        }
        if let Err(error) = std::fs::remove_file(entry.path())
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!(
                "unable to remove orphan playlist rows {}: {error}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn read_playlist_manifest(path: &std::path::Path) -> std::io::Result<PlaylistCacheManifest> {
    let file = std::fs::File::open(playlist_manifest_path(path))?;
    let manifest: PlaylistCacheManifest =
        serde_json::from_reader(std::io::BufReader::new(file)).map_err(std::io::Error::other)?;
    if manifest.version != 2
        || manifest.rows > manifest.total
        || manifest
            .next_offset
            .is_some_and(|offset| offset > manifest.total)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid playlist cache manifest",
        ));
    }
    Ok(manifest)
}

fn read_incremental_playlist_cache_file(path: &std::path::Path) -> std::io::Result<CachedPlaylist> {
    use std::io::Read;

    let manifest = read_playlist_manifest(path)?;
    let file = std::fs::File::open(playlist_data_path(path, manifest.data_file))?;
    if file.metadata()?.len() < manifest.bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "truncated playlist cache data",
        ));
    }
    let reader = std::io::BufReader::new(file.take(manifest.bytes));
    let mut items = Vec::new();
    for block in serde_json::Deserializer::from_reader(reader).into_iter::<Vec<PlaylistItem>>() {
        let block = block.map_err(std::io::Error::other)?;
        if items.is_empty() {
            items = block;
        } else {
            items.extend(block);
        }
        if items.len() > manifest.rows as usize {
            break;
        }
    }
    if items.len() != manifest.rows as usize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "playlist cache row count does not match its manifest",
        ));
    }
    Ok(CachedPlaylist {
        snapshot: manifest.snapshot,
        items,
        total: Some(manifest.total),
        next_offset: manifest.next_offset,
    })
}

/// Keep writes in command order without making the command loop wait for disk.
async fn store_playlist_caches(
    mut writes: mpsc::Receiver<PlaylistCacheJob>,
    events: std::sync::mpsc::Sender<Event>,
    waker: Waker,
) {
    while let Some(job) = writes.recv().await {
        let write = match job {
            PlaylistCacheJob::Write(write) => write,
            PlaylistCacheJob::Purge(root) => {
                let purged = tokio::task::spawn_blocking(move || purge_playlist_caches(&root));
                if let Err(error) = purged.await {
                    log::warn!("unable to remove the playlist caches: {error}");
                }
                continue;
            }
        };
        let PlaylistCacheWrite {
            path,
            account_id,
            id,
            generation,
            snapshot,
            rows,
            total,
            next_offset,
        } = write;
        let result = write_incremental_playlist_cache(
            path.clone(),
            snapshot.clone(),
            rows,
            total,
            next_offset,
        )
        .await;
        if let Err(error) = &result {
            log::warn!("unable to store playlist cache {}: {error}", path.display());
        }
        let _ = events.send(Event::PlaylistCacheStored {
            account_id,
            id,
            generation,
            snapshot,
            success: result.is_ok(),
        });
        waker.wake();
    }
}

async fn write_incremental_playlist_cache(
    path: std::path::PathBuf,
    snapshot: String,
    rows: PlaylistCacheRows,
    total: u32,
    next_offset: Option<u32>,
) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        write_incremental_playlist_cache_file(&path, snapshot, rows, total, next_offset)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn write_incremental_playlist_cache_file(
    path: &std::path::Path,
    snapshot: String,
    rows: PlaylistCacheRows,
    total: u32,
    next_offset: Option<u32>,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    if next_offset.is_some_and(|offset| offset > total) {
        return Err(Error::new(ErrorKind::InvalidInput, "offset exceeds total"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = playlist_cache_lock(path)?;
    match rows {
        PlaylistCacheRows::Replace(items) => {
            let count = u32::try_from(items.len())
                .map_err(|_| Error::new(ErrorKind::InvalidInput, "too many playlist rows"))?;
            if count > total {
                return Err(Error::new(ErrorKind::InvalidInput, "rows exceed total"));
            }
            let (file, data_file) = create_playlist_data_file(path)?;
            let data_path = playlist_data_path(path, data_file);
            let result = (|| {
                let bytes = write_playlist_block(file, &items)?;
                write_playlist_manifest(
                    path,
                    &PlaylistCacheManifest {
                        version: 2,
                        snapshot,
                        data_file,
                        bytes,
                        rows: count,
                        total,
                        next_offset,
                    },
                )
            })();
            if result.is_err() {
                if let Err(error) = std::fs::remove_file(&data_path) {
                    log::warn!(
                        "unable to remove incomplete playlist rows {}: {error}",
                        data_path.display()
                    );
                }
            } else if let Err(error) = cleanup_unreferenced_playlist_rows(path) {
                log::warn!("unable to clean playlist cache {}: {error}", path.display());
            }
            result
        }
        PlaylistCacheRows::Append {
            previous_rows,
            previous_offset,
            items,
        } => {
            let mut manifest = read_playlist_manifest(path)?;
            if manifest.snapshot != snapshot
                || manifest.total != total
                || manifest.rows as usize != previous_rows
                || manifest.next_offset.unwrap_or(manifest.total) != previous_offset
            {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "playlist cache changed before append",
                ));
            }
            let added = u32::try_from(items.len())
                .map_err(|_| Error::new(ErrorKind::InvalidInput, "too many playlist rows"))?;
            let count = manifest
                .rows
                .checked_add(added)
                .filter(|count| *count <= total)
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "rows exceed total"))?;
            let data_path = playlist_data_path(path, manifest.data_file);
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(data_path)?;
            if file.metadata()?.len() < manifest.bytes {
                return Err(Error::new(ErrorKind::UnexpectedEof, "truncated cache data"));
            }
            // Drop bytes from a write whose manifest was never published.
            file.set_len(manifest.bytes)?;
            use std::io::{Seek, SeekFrom};
            file.seek(SeekFrom::Start(manifest.bytes))?;
            let bytes = write_playlist_block(file, &items)?;
            manifest.bytes = bytes;
            manifest.rows = count;
            manifest.next_offset = next_offset;
            write_playlist_manifest(path, &manifest)
        }
    }
}

fn create_playlist_data_file(path: &std::path::Path) -> std::io::Result<(std::fs::File, u64)> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    for attempt in 0..1024 {
        let data_file = seed.wrapping_add(attempt);
        let candidate = playlist_data_path(path, data_file);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(candidate)
        {
            Ok(file) => return Ok((file, data_file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "playlist cache data names exhausted",
    ))
}

fn write_playlist_block(file: std::fs::File, items: &[PlaylistItem]) -> std::io::Result<u64> {
    use std::io::Write;

    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer(&mut writer, items).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    let file = writer.into_inner().map_err(|error| error.into_error())?;
    // Publish the manifest only after its referenced bytes are durable.
    file.sync_all()?;
    Ok(file.metadata()?.len())
}

fn write_playlist_manifest(
    path: &std::path::Path,
    manifest: &PlaylistCacheManifest,
) -> std::io::Result<()> {
    use std::io::Write;

    // The new manifest reaches the disk before it replaces the old one.
    fastframe_file::write_with(
        &playlist_manifest_path(path),
        fastframe_file::Access::Default,
        |file| {
            let mut writer = std::io::BufWriter::new(file);
            serde_json::to_writer(&mut writer, manifest).map_err(std::io::Error::other)?;
            writer.flush()
        },
    )
}

#[cfg(test)]
mod album_type_lookup_tests {
    use super::AlbumTypeLookup;

    #[test]
    fn signed_out_requests_are_dropped_and_valid_pending_work_is_deduplicated() {
        let mut lookup = AlbumTypeLookup::default();
        lookup.enqueue(false, None, vec!["signed-out".into()]);
        assert!(lookup.pending.is_empty());

        lookup.enqueue(
            true,
            None,
            vec!["first".into(), "first".into(), "second".into()],
        );
        assert_eq!(lookup.pending.len(), 2);

        let first = lookup.next().expect("first request when an engine appears");
        assert_eq!(first.uri, "first");
        assert!(lookup.next().is_none(), "only one lookup may be active");
        assert!(lookup.finish(&first));
        assert_eq!(lookup.next().expect("remaining request").uri, "second");
    }

    #[test]
    fn results_from_a_signed_out_session_are_rejected_after_a_new_session_starts() {
        let mut lookup = AlbumTypeLookup::default();
        lookup.enqueue(true, None, vec!["old".into()]);
        let old = lookup.next().expect("old session request");

        lookup.reset_session();
        assert!(!lookup.finish(&old));
        lookup.enqueue(false, None, vec!["after-logout".into()]);
        assert!(lookup.pending.is_empty());

        lookup.enqueue(true, None, vec!["new".into()]);
        let new = lookup.next().expect("new session request");
        assert_ne!(old.session_generation, new.session_generation);
        assert!(!lookup.finish(&old));
        assert!(lookup.finish(&new));
    }

    #[test]
    fn reconnect_requeues_active_work_for_the_new_engine() {
        let mut lookup = AlbumTypeLookup::default();
        lookup.enqueue(true, None, vec!["active".into(), "waiting".into()]);
        let retired = lookup.next().expect("retired engine request");

        lookup.requeue_active_for_new_engine();
        assert_eq!(lookup.pending.front().map(String::as_str), Some("active"));

        lookup.enqueue(true, None, vec!["active".into()]);
        assert_eq!(lookup.pending.len(), 2, "external duplicates stay ignored");

        let replacement = lookup.next().expect("new engine request");
        assert_eq!(replacement.uri, "active");
        assert_eq!(retired.session_generation, replacement.session_generation);
        assert_ne!(retired.engine_generation, replacement.engine_generation);
        assert!(!lookup.finish(&retired));
        assert_eq!(lookup.active.as_ref(), Some(&replacement));
        assert!(lookup.finish(&replacement));
        assert_eq!(lookup.next().expect("remaining request").uri, "waiting");
    }

    #[test]
    fn completed_failures_remain_terminal_for_the_session() {
        let mut lookup = AlbumTypeLookup::default();

        for uri in ["error", "timeout"] {
            lookup.enqueue(true, None, vec![uri.into()]);
            let request = lookup.next().expect("request before failure");
            assert!(lookup.finish(&request));

            lookup.enqueue(true, None, vec![uri.into()]);
            assert!(lookup.pending.is_empty(), "failed request must not retry");
        }
    }

    #[test]
    fn non_premium_status_clears_and_invalidates_all_engine_work() {
        let mut lookup = AlbumTypeLookup::default();
        lookup.enqueue(true, None, vec!["active".into(), "pending".into()]);
        let active = lookup.next().expect("request before account check");
        let session_generation = lookup.session_generation;

        lookup.clear_engine_work();

        assert_eq!(lookup.session_generation, session_generation);
        assert_ne!(lookup.engine_generation, active.engine_generation);
        assert!(lookup.active.is_none());
        assert!(lookup.pending.is_empty());
        assert!(lookup.seen.is_empty());
        assert!(!lookup.finish(&active));

        lookup.enqueue(true, Some(false), vec!["after-check".into()]);
        assert!(lookup.pending.is_empty());
        assert!(lookup.seen.is_empty());
    }

    #[test]
    fn unknown_and_premium_accounts_accept_album_type_work() {
        let mut lookup = AlbumTypeLookup::default();

        lookup.enqueue(true, None, vec!["unknown".into()]);
        let unknown = lookup.next().expect("unknown account request");
        assert_eq!(unknown.uri, "unknown");
        assert!(lookup.finish(&unknown));

        lookup.enqueue(true, Some(true), vec!["premium".into()]);
        let premium = lookup.next().expect("premium account request");
        assert_eq!(premium.uri, "premium");
        assert!(lookup.finish(&premium));
    }
}

#[cfg(test)]
mod playlist_header_lookup_tests {
    use super::PlaylistHeaderLookup;

    /// Blank library playlists are read one at a time: a batch that arrives
    /// while a read runs waits behind it rather than starting a second read
    /// beside it, and a playlist already waiting or being read is not queued
    /// twice.
    #[test]
    fn playlist_headers_are_read_one_at_a_time() {
        let mut lookup = PlaylistHeaderLookup::default();
        lookup.enqueue(vec!["first".into(), "second".into()]);
        let first = lookup.next(1).expect("first read");
        assert_eq!(first.id, "first");
        assert_eq!(lookup.next(1), None, "one read at a time");

        lookup.enqueue(vec!["first".into(), "second".into(), "third".into()]);
        assert_eq!(lookup.pending, ["second", "third"]);
        assert_eq!(lookup.next(1), None, "a later batch waits too");

        assert!(lookup.finish(&first));
        assert_eq!(
            lookup.next(1).map(|request| request.id).as_deref(),
            Some("second")
        );
    }

    /// A read still running when the engine is replaced goes back to the
    /// front of the queue for the new engine, and the retired engine's
    /// answer is ignored rather than finishing the new read.
    #[test]
    fn a_playlist_header_read_on_a_retired_engine_is_read_again() {
        let mut lookup = PlaylistHeaderLookup::default();
        lookup.enqueue(vec!["active".into(), "waiting".into()]);
        let retired = lookup.next(1).expect("read on the old engine");

        lookup.requeue_active_for_new_engine();
        assert_eq!(lookup.pending, ["active", "waiting"]);

        let replacement = lookup.next(1).expect("read on the new engine");
        assert_eq!(replacement.id, "active");
        assert_ne!(retired.engine_generation, replacement.engine_generation);
        assert!(!lookup.finish(&retired), "the retired answer is ignored");
        assert!(lookup.finish(&replacement));
    }

    #[test]
    fn clearing_engine_work_drops_waiting_playlist_headers() {
        let mut lookup = PlaylistHeaderLookup::default();
        lookup.enqueue(vec!["active".into(), "waiting".into()]);
        let active = lookup.next(1).expect("read before sign-out");

        lookup.clear_engine_work();

        assert!(lookup.active.is_none());
        assert!(lookup.pending.is_empty());
        assert!(!lookup.finish(&active));
    }
}

#[cfg(test)]
mod playlist_cache_tests {
    use super::{
        CachedPlaylist, PlaylistCacheRows, playlist_data_path, read_cached_playlist,
        read_incremental_playlist_cache_file, read_playlist_cache, read_playlist_manifest,
        write_cached_playlist, write_incremental_playlist_cache_file,
    };
    use crate::api::models::{PlayableItem, PlaylistItem, Track};

    #[test]
    fn the_original_complete_cache_format_remains_readable() {
        let cached: CachedPlaylist =
            serde_json::from_str(r#"{"snapshot":"old","items":[]}"#).unwrap();

        assert_eq!(cached.snapshot, "old");
        assert_eq!(cached.total, None);
        assert_eq!(cached.next_offset, None);
    }

    #[test]
    fn incremental_checkpoints_append_only_new_rows_and_recover_from_failed_publication() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-playlist-cache-incremental-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("playlist.json");
        let row = |uri: &str| PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                uri: uri.into(),
                ..Track::default()
            })),
            ..PlaylistItem::default()
        };
        let first = vec![row("spotify:track:one"), PlaylistItem::default()];
        let added = row("spotify:track:three");
        write_incremental_playlist_cache_file(
            &path,
            "same".into(),
            PlaylistCacheRows::Replace(first.clone()),
            100,
            Some(50),
        )
        .unwrap();
        let initial = read_playlist_manifest(&path).unwrap();
        assert_eq!(initial.rows, 2);
        assert_eq!(
            initial.bytes as usize,
            serde_json::to_vec(&first).unwrap().len() + 1
        );

        let append = || PlaylistCacheRows::Append {
            previous_rows: 2,
            previous_offset: 50,
            items: vec![added.clone()],
        };
        // An append whose manifest was never published (the app stopped
        // between the rows and the manifest) leaves its rows behind.
        {
            use std::io::Write;
            let mut data = std::fs::OpenOptions::new()
                .append(true)
                .open(playlist_data_path(&path, initial.data_file))
                .unwrap();
            serde_json::to_writer(&mut data, std::slice::from_ref(&added)).unwrap();
            data.write_all(b"\n").unwrap();
        }
        assert_eq!(
            read_incremental_playlist_cache_file(&path).unwrap().items,
            first
        );
        assert_eq!(read_playlist_manifest(&path).unwrap().bytes, initial.bytes);

        write_incremental_playlist_cache_file(&path, "same".into(), append(), 100, Some(75))
            .unwrap();
        let committed = read_playlist_manifest(&path).unwrap();
        assert_eq!(committed.rows, 3);
        assert_eq!(
            committed.bytes - initial.bytes,
            (serde_json::to_vec(std::slice::from_ref(&added))
                .unwrap()
                .len()
                + 1) as u64,
            "an append writes only its new block, including after a failed publication"
        );
        assert_eq!(
            std::fs::metadata(playlist_data_path(&path, committed.data_file))
                .unwrap()
                .len(),
            committed.bytes
        );
        let restored = read_incremental_playlist_cache_file(&path).unwrap();
        assert_eq!(restored.items, [first, vec![added]].concat());
        assert_eq!(restored.next_offset, Some(75));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn incremental_cache_replaces_changed_snapshot_and_keeps_legacy_reader() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-playlist-cache-migration-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("playlist.json");
        write_cached_playlist(
            path.clone(),
            CachedPlaylist {
                snapshot: "legacy".into(),
                items: vec![PlaylistItem::default()],
                total: Some(10),
                next_offset: Some(5),
            },
        )
        .await
        .unwrap();
        let (cached, appendable) = read_playlist_cache(path.clone()).await.unwrap();
        assert_eq!(cached.snapshot, "legacy");
        assert!(!appendable);

        write_incremental_playlist_cache_file(
            &path,
            "new".into(),
            PlaylistCacheRows::Replace(vec![PlaylistItem::default(); 2]),
            10,
            Some(5),
        )
        .unwrap();
        let old_data = read_playlist_manifest(&path).unwrap().data_file;
        let (cached, appendable) = read_playlist_cache(path.clone()).await.unwrap();
        assert_eq!(cached.snapshot, "new");
        assert!(appendable);
        assert_eq!(
            read_playlist_cache(path.clone())
                .await
                .unwrap()
                .0
                .items
                .len(),
            2
        );

        std::fs::OpenOptions::new()
            .write(true)
            .open(playlist_data_path(&path, old_data))
            .unwrap()
            .set_len(0)
            .unwrap();
        let (cached, appendable) = read_playlist_cache(path.clone()).await.unwrap();
        assert_eq!(cached.snapshot, "legacy");
        assert!(!appendable);

        write_incremental_playlist_cache_file(
            &path,
            "newer".into(),
            PlaylistCacheRows::Replace(vec![PlaylistItem::default()]),
            1,
            None,
        )
        .unwrap();
        assert_eq!(
            read_playlist_cache(path.clone()).await.unwrap().0.snapshot,
            "newer"
        );
        assert!(!playlist_data_path(&path, old_data).exists());
        assert_eq!(read_cached_playlist(path).await.unwrap().snapshot, "legacy");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn reopening_a_cache_removes_rows_left_by_an_interrupted_replacement() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-playlist-cache-orphan-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("playlist.json");
        write_incremental_playlist_cache_file(
            &path,
            "current".into(),
            PlaylistCacheRows::Replace(vec![PlaylistItem::default()]),
            1,
            None,
        )
        .unwrap();
        let current = read_playlist_manifest(&path).unwrap().data_file;
        let orphan = playlist_data_path(&path, current.wrapping_add(1));
        let blocked = playlist_data_path(&path, current.wrapping_add(2));
        let recoverable = playlist_data_path(&path, current.wrapping_add(3));
        let other_playlist = playlist_data_path(&root.join("other.json"), 1);
        std::fs::write(&orphan, b"unfinished rows").unwrap();
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(&recoverable, b"unfinished rows").unwrap();
        std::fs::write(&other_playlist, b"unrelated rows").unwrap();

        let (cached, appendable) = read_playlist_cache(path.clone()).await.unwrap();
        assert_eq!(cached.snapshot, "current");
        assert!(appendable);
        assert!(
            !orphan.exists(),
            "recovery must remove unreferenced row files"
        );
        assert!(playlist_data_path(&path, current).exists());
        assert!(
            !recoverable.exists(),
            "one failed removal must not stop cleanup"
        );
        assert!(blocked.is_dir());
        assert!(other_playlist.exists());
        std::fs::remove_dir(&blocked).unwrap();

        let before_manifest = root.join("cold.json");
        let orphan = playlist_data_path(&before_manifest, 1);
        std::fs::write(&orphan, b"unfinished first checkpoint").unwrap();
        assert!(read_playlist_cache(before_manifest).await.is_err());
        assert!(
            !orphan.exists(),
            "a crash before the first manifest is recoverable"
        );

        let orphan = playlist_data_path(&path, current.wrapping_add(1));
        std::fs::write(&orphan, b"unfinished rows").unwrap();
        write_incremental_playlist_cache_file(
            &path,
            "next".into(),
            PlaylistCacheRows::Replace(vec![PlaylistItem::default()]),
            1,
            None,
        )
        .unwrap();
        assert!(!orphan.exists());
        assert!(!playlist_data_path(&path, current).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn the_file_reader_accepts_legacy_caches_and_ignores_unknown_fields() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-playlist-cache-legacy-read-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("playlist.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &path,
            b"{\"snapshot\":\"old\",\"items\":[],\"future_field\":true} \n\t",
        )
        .unwrap();

        let cached = read_cached_playlist(path).await.unwrap();

        assert_eq!(cached.snapshot, "old");
        assert!(cached.items.is_empty());
        assert_eq!(cached.total, None);
        assert_eq!(cached.next_offset, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn the_file_reader_rejects_missing_corrupt_and_trailing_data() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-playlist-cache-invalid-read-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("playlist.json");
        std::fs::create_dir_all(&root).unwrap();
        assert!(read_cached_playlist(path.clone()).await.is_err());
        for bytes in [
            b"".as_slice(),
            b"{\"snapshot\":\"partial\",\"items\":[",
            b"{\"snapshot\":\"bad utf8: \xff\",\"items\":[]}",
            b"{\"snapshot\":\"missing items\"}",
            b"{\"snapshot\":\"ok\",\"items\":[]}{}",
            b"{\"snapshot\":\"ok\",\"items\":[]}trailing",
        ] {
            std::fs::write(&path, bytes).unwrap();

            assert!(read_cached_playlist(path.clone()).await.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_new_checkpoint_atomically_replaces_the_previous_one() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-playlist-cache-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("playlist.json");
        let cached = |snapshot: &str| CachedPlaylist {
            snapshot: snapshot.into(),
            items: Vec::new(),
            total: Some(10_000),
            next_offset: Some(500),
        };

        write_cached_playlist(path.clone(), cached("first"))
            .await
            .unwrap();
        write_cached_playlist(path.clone(), cached("second"))
            .await
            .unwrap();

        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let stored: CachedPlaylist = serde_json::from_str(&text).unwrap();
        assert_eq!(stored.snapshot, "second");
        assert!(crate::util::temporary_files(path.parent().unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn streaming_preserves_the_cache_bytes_and_duplicate_unavailable_rows() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-playlist-cache-stream-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("playlist.json");
        let song = PlayableItem::Track(Track {
            uri: "spotify:track:duplicate".into(),
            name: "Song with \"quotes\", newlines\nand 日本語".into(),
            is_playable: Some(false),
            ..Track::default()
        });
        let rows = [
            PlaylistItem {
                item: Some(song.clone()),
                ..PlaylistItem::default()
            },
            PlaylistItem {
                track: Some(song),
                ..PlaylistItem::default()
            },
            PlaylistItem::default(),
        ];
        let cached = CachedPlaylist {
            snapshot: "unchanged-snapshot".into(),
            items: (0..500).flat_map(|_| rows.clone()).collect(),
            total: Some(2_000),
            next_offset: Some(1_500),
        };
        let expected = serde_json::to_vec(&cached).unwrap();
        assert!(
            expected.len() > 64 * 1024,
            "exercise multiple buffer flushes"
        );

        write_cached_playlist(path.clone(), cached).await.unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, expected, "existing readers see the identical format");
        let restored = read_cached_playlist(path.clone()).await.unwrap();
        assert_eq!(restored.snapshot, "unchanged-snapshot");
        assert_eq!(restored.items.len(), 1_500);
        for chunk in restored.items.chunks(3) {
            assert_eq!(chunk, rows);
        }
        assert_eq!(restored.total, Some(2_000));
        assert_eq!(restored.next_offset, Some(1_500));
        assert!(crate::util::temporary_files(path.parent().unwrap()).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_checkpoint_keeps_existing_data_and_cleans_only_its_temporary_file() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-playlist-cache-failure-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = root.join("playlist.json");
        let cached = || CachedPlaylist {
            snapshot: "new".into(),
            items: Vec::new(),
            total: Some(0),
            next_offset: None,
        };
        let previous = br#"{"snapshot":"old","items":[]}"#;
        std::fs::create_dir_all(&path).unwrap();
        // A file an older version's interrupted write left is not this
        // write's to remove.
        let stale = path.with_extension("json.tmp");
        std::fs::write(&stale, previous).unwrap();

        // Force final replacement to fail after serialization and flushing.
        std::fs::write(path.join("preserved"), previous).unwrap();
        assert!(write_cached_playlist(path.clone(), cached()).await.is_err());
        assert_eq!(std::fs::read(path.join("preserved")).unwrap(), previous);
        assert_eq!(
            crate::util::temporary_files(&root),
            ["playlist.json.tmp"],
            "discard the failed checkpoint"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Bind a streaming token to an account verified by either Web API grant.
/// A late browser result after sign-out must not start an anonymous session.
fn playback_credentials(account: Option<AccountId>, access_token: String) -> Option<Credentials> {
    let account = account.filter(|account| !account.as_str().is_empty())?;
    Some(Credentials {
        username: Some(account.as_str().to_string()),
        auth_type:
            librespot_protocol::authentication::AuthenticationType::AUTHENTICATION_SPOTIFY_TOKEN,
        auth_data: access_token.into_bytes(),
    })
}

#[cfg(test)]
mod authorization_tests {
    #[test]
    fn failed_connect_resolution_keeps_only_current_cast_results() {
        let cast = crate::zeroconf::Receiver {
            name: "Current Cast".into(),
            device_id: Some("cast".into()),
            address: "192.0.2.1".parse().unwrap(),
            port: 8009,
            path: "/".into(),
            cast: true,
        };
        let connect = crate::zeroconf::Receiver {
            cast: false,
            ..cast.clone()
        };
        let final_rows = receiver_snapshot(vec![connect, cast.clone()], |_| {
            Err(anyhow::anyhow!("HTTP client initialization failed"))
        });
        assert_eq!(final_rows, [cast]);
        // A service removed during the browse must not survive through progress.
        assert!(receiver_snapshot(Vec::new(), |_| Err(anyhow::anyhow!("failed"))).is_empty());
        assert!(receiver_snapshot(final_rows, |_| Ok(Vec::new())).is_empty());
    }

    #[test]
    fn opening_the_picker_again_does_not_start_a_second_receiver_scan() {
        let (runtime, mut worker, _) = worker("coalesce-receiver-scan");
        let _entered = runtime.enter();
        let pending = runtime.spawn(std::future::pending::<()>());
        let id = pending.id();
        worker.receiver_discovery = Some(pending);
        worker.discover_receivers();
        assert_eq!(worker.receiver_discovery.as_ref().unwrap().id(), id);
        worker.receiver_discovery.take().unwrap().abort();
    }
    use super::*;

    #[test]
    fn playlist_cache_store_does_not_hold_up_the_command_loop() {
        let (runtime, mut worker, events) = worker("playlist-cache-command-loop");
        worker
            .api
            .install(ApiSource::Shared, AccountId::new("alice"))
            .unwrap();
        let path = worker
            .dirs
            .account_playlist_cache_dir("alice")
            .join("mix.json");
        let root = worker.dirs.cache.parent().unwrap().to_path_buf();
        let lock = playlist_cache_lock(&path).unwrap();
        let (commands, receiver) = mpsc::unbounded_channel();
        commands
            .send(Command::StorePlaylistCache {
                id: "mix".into(),
                generation: 1,
                snapshot: "old".into(),
                rows: PlaylistCacheRows::Replace(vec![PlaylistItem::default()]),
                total: 1,
                next_offset: None,
            })
            .unwrap();
        commands
            .send(Command::ConfigurePersonalWebApp(None))
            .unwrap();
        commands.send(Command::Shutdown).unwrap();
        let thread = std::thread::spawn(move || runtime.block_on(worker.run(receiver)));

        let deadline = Instant::now() + Duration::from_secs(2);
        let handled_next_command = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break false;
            }
            match events.recv_timeout(remaining) {
                Ok(Event::WebApp { client_id: None }) => break true,
                Ok(_) => continue,
                Err(_) => break false,
            }
        };
        drop(lock);
        thread.join().unwrap();
        assert!(
            handled_next_command,
            "a pending disk write blocked the command loop"
        );
        assert_eq!(read_playlist_manifest(&path).unwrap().snapshot, "old");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Signing out removes every account's cached library: playlist pages,
    /// the playlist list, Home and Liked Songs. A playlist page still being
    /// written when it happens is removed with the rest, not left behind.
    #[test]
    fn signing_out_removes_the_account_caches_after_pending_writes() {
        let (runtime, mut worker, _events) = worker("sign-out-purges-caches");
        worker
            .api
            .install(ApiSource::Shared, AccountId::new("alice"))
            .unwrap();
        let root = worker.dirs.cache.parent().unwrap().to_path_buf();
        let others = [
            worker.dirs.library_cache_file("alice"),
            worker.dirs.home_cache_file("alice"),
            worker.dirs.liked_songs_cache_file("alice"),
            worker
                .dirs
                .account_playlist_cache_dir("bob")
                .join("theirs.json"),
        ];
        for file in &others {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"{}").unwrap();
        }
        let path = worker
            .dirs
            .account_playlist_cache_dir("alice")
            .join("mix.json");
        let dirs = worker.dirs.account_cache_dirs();
        // The page write waits on this lock while the sign-out arrives.
        let lock = playlist_cache_lock(&path).unwrap();
        let (commands, receiver) = mpsc::unbounded_channel();
        commands
            .send(Command::StorePlaylistCache {
                id: "mix".into(),
                generation: 1,
                snapshot: "old".into(),
                rows: PlaylistCacheRows::Replace(vec![PlaylistItem::default()]),
                total: 1,
                next_offset: None,
            })
            .unwrap();
        commands.send(Command::SignOut).unwrap();
        commands.send(Command::Shutdown).unwrap();
        let thread = std::thread::spawn(move || runtime.block_on(worker.run(receiver)));
        std::thread::sleep(Duration::from_millis(200));
        drop(lock);
        thread.join().unwrap();

        for dir in &dirs {
            assert!(!dir.exists(), "{} was left behind", dir.display());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The account caches go in command order: an account that signs in
    /// right after a sign-out keeps what it stores, even while a page of
    /// the previous account still holds the removal of the pages back.
    #[test]
    fn a_sign_in_right_after_sign_out_keeps_its_caches() {
        let (runtime, mut worker, events) = worker("sign-out-then-sign-in");
        worker
            .api
            .install(ApiSource::Shared, AccountId::new("alice"))
            .unwrap();
        let api = worker.api.clone();
        let root = worker.dirs.cache.parent().unwrap().to_path_buf();
        let theirs = [
            worker.dirs.library_cache_file("alice"),
            worker.dirs.home_cache_file("alice"),
        ];
        for file in &theirs {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, b"{}").unwrap();
        }
        let mine = [
            worker.dirs.library_cache_file("carol"),
            worker.dirs.home_cache_file("carol"),
        ];
        let path = worker
            .dirs
            .account_playlist_cache_dir("alice")
            .join("mix.json");
        let lock = playlist_cache_lock(&path).unwrap();
        let (commands, receiver) = mpsc::unbounded_channel();
        commands
            .send(Command::StorePlaylistCache {
                id: "mix".into(),
                generation: 1,
                snapshot: "old".into(),
                rows: PlaylistCacheRows::Replace(vec![PlaylistItem::default()]),
                total: 1,
                next_offset: None,
            })
            .unwrap();
        commands.send(Command::SignOut).unwrap();
        let thread = std::thread::spawn(move || runtime.block_on(worker.run(receiver)));
        let deadline = Instant::now() + Duration::from_secs(5);
        let signed_out = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match events.recv_timeout(remaining) {
                Ok(Event::Auth(AuthStatus::SignedOut)) => break true,
                Ok(_) => continue,
                Err(_) => break false,
            }
        };
        api.install(ApiSource::Shared, AccountId::new("carol"))
            .unwrap();
        commands
            .send(Command::StoreLibraryCache(
                crate::account_cache::Cache::new("carol".into(), Vec::new()),
            ))
            .unwrap();
        commands
            .send(Command::StoreHomeCache(crate::account_cache::Cache::new(
                "carol".into(),
                crate::model::HomeSnapshot::default(),
            )))
            .unwrap();
        commands.send(Command::Shutdown).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        drop(lock);
        thread.join().unwrap();

        assert!(signed_out);
        for file in &theirs {
            assert!(!file.exists(), "{} was left behind", file.display());
        }
        assert!(
            !path.exists(),
            "the previous account's page was left behind"
        );
        for file in &mine {
            assert!(file.exists(), "{} was removed", file.display());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn playlist_cache_writer_rejects_excess_snapshots_without_blocking_commands() {
        let (runtime, mut worker, events) = worker("playlist-cache-bounded-writes");
        worker
            .api
            .install(ApiSource::Shared, AccountId::new("alice"))
            .unwrap();
        let path = worker
            .dirs
            .account_playlist_cache_dir("alice")
            .join("mix.json");
        let root = worker.dirs.cache.parent().unwrap().to_path_buf();
        let lock = playlist_cache_lock(&path).unwrap();
        let (commands, receiver) = mpsc::unbounded_channel();
        for generation in 0..4 {
            commands
                .send(Command::StorePlaylistCache {
                    id: "mix".into(),
                    generation,
                    snapshot: format!("snapshot-{generation}"),
                    rows: PlaylistCacheRows::Replace(vec![PlaylistItem::default()]),
                    total: 1,
                    next_offset: None,
                })
                .unwrap();
        }
        commands
            .send(Command::ConfigurePersonalWebApp(None))
            .unwrap();
        commands.send(Command::Shutdown).unwrap();
        let thread = std::thread::spawn(move || runtime.block_on(worker.run(receiver)));

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut rejected = 0;
        let handled_next_command = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break false;
            }
            match events.recv_timeout(remaining) {
                Ok(Event::PlaylistCacheStored { success: false, .. }) => rejected += 1,
                Ok(Event::WebApp { client_id: None }) => break true,
                Ok(_) => {}
                Err(_) => break false,
            }
        };
        drop(lock);
        thread.join().unwrap();
        assert!(
            handled_next_command,
            "cache writes blocked the command loop"
        );
        assert!(rejected >= 2, "only one write may wait behind the writer");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn engine_deadline_reaches_final_access_point_after_stalled_retries() {
        let started = tokio::time::Instant::now();
        let mut attempts = Vec::new();
        let result = connect_engine_with_deadline(async {
            // Resolver and other setup work also spend the outer budget.
            tokio::time::sleep(Duration::from_secs(10)).await;
            // Model the pinned librespot cap: six APs, two five-second
            // connection attempts per AP. The final retry succeeds.
            for ap in 0..6 {
                for retry in 0..2 {
                    attempts.push((ap, retry));
                    if (ap, retry) == (5, 1) {
                        return ap;
                    }
                    assert!(
                        tokio::time::timeout(Duration::from_secs(5), std::future::pending::<()>())
                            .await
                            .is_err()
                    );
                }
            }
            unreachable!("the final fallback should connect");
        })
        .await;

        assert_eq!(result.unwrap(), 5);
        assert_eq!(
            attempts,
            (0..6).flat_map(|ap| [(ap, 0), (ap, 1)]).collect::<Vec<_>>()
        );
        assert_eq!(started.elapsed(), Duration::from_secs(65));
    }

    #[test]
    fn proxy_changes_compare_with_the_running_engine() {
        const CHILD: &str = "SPOTIFAST_PROXY_SNAPSHOT_TEST";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "backend::authorization_tests::proxy_changes_compare_with_the_running_engine",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("NO_PROXY", "*")
                .env("no_proxy", "*")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        // The system proxy has been removed since the engine connected.
        // Resolve it in a child process so the test never alters desktop settings
        // or the environment used by concurrently running tests.
        assert_eq!(ProxyConfig::System.librespot_url(), None);
        let old_url = reqwest::Url::parse("http://127.0.0.1:7890/").unwrap();
        let (runtime, mut worker, _) = worker("proxy-snapshot");
        let _entered = runtime.enter();
        worker.engine_config.proxy = ProxyConfig::System;
        worker.engine_proxy = Some(old_url.clone());
        worker.signed_in = true;
        worker.engine_busy = true;
        worker.change_proxy(1, ProxyConfig::Off, false);
        assert!(
            worker.engine_restart_pending,
            "Off must discard an in-flight connection using the former system proxy"
        );
        assert_eq!(worker.engine_proxy, Some(old_url));
        assert_eq!(worker.engine_config.proxy, ProxyConfig::Off);

        let http = crate::settings::Settings {
            proxy_mode: crate::settings::ProxyMode::Http,
            proxy_host: "127.0.0.1".into(),
            proxy_port: "7890".into(),
            ..Default::default()
        };
        let socks = crate::settings::Settings {
            proxy_mode: crate::settings::ProxyMode::Socks,
            ..http.clone()
        };
        let other = crate::settings::Settings {
            proxy_port: "7891".into(),
            ..http.clone()
        };
        // Every choice is compared with the old connection, even after a
        // previous Apply has updated the saved policy while reconnecting.
        for (next, restart) in [
            (ProxyConfig::System, true),
            (socks.proxy_config().unwrap(), true),
            (other.proxy_config().unwrap(), true),
            (http.proxy_config().unwrap(), false),
        ] {
            assert_eq!(worker.apply_proxy(next).unwrap(), restart);
        }
        worker.engine_proxy = None;
        for (next, restart) in [
            (ProxyConfig::Off, false),
            (ProxyConfig::System, false),
            (socks.proxy_config().unwrap(), false),
            (http.proxy_config().unwrap(), true),
        ] {
            assert_eq!(worker.apply_proxy(next).unwrap(), restart);
        }
    }

    #[test]
    fn proxy_restoration_defers_work_without_blocking_shutdown() {
        let (runtime, mut worker, _) = worker("proxy-restore-barrier");
        worker.restoring_proxy = true;
        let mut audio = worker.engine_config.clone();
        audio.proxy = ProxyConfig::System;
        let (commands, receiver) = mpsc::unbounded_channel();
        commands.send(Command::RestartEngine(audio)).unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(1), worker.run(receiver))
                .await
                .unwrap();
        });
        assert_eq!(worker.waiting_for_proxy.len(), 1);
        assert_eq!(worker.engine_config.proxy, ProxyConfig::Off);
    }

    /// A session that drops on its own comes back at the level being heard,
    /// exactly, not at the level the app launched with. Music turned down
    /// to 5% after launch used to come back at the previous session's 80%.
    #[test]
    fn a_dropped_session_comes_back_at_the_level_being_heard() {
        let (runtime, mut worker, _) = worker("reconnect-volume");
        worker.signed_in = true;
        worker.engine_config.initial_volume = crate::app::percent_to_volume(80);
        worker.heard = Some(Heard::at(crate::app::percent_to_volume(5)));
        let (commands, receiver) = mpsc::unbounded_channel();
        commands.send(Command::Reconnect).unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(1), worker.run(receiver))
                .await
                .unwrap();
        });
        assert_eq!(
            worker.engine_config.initial_volume,
            crate::app::percent_to_volume(5),
            "the next engine must start where the last one was heard"
        );
        assert!(
            worker.heard.is_none(),
            "what was heard goes with its engine"
        );
    }

    /// Signing out takes the engine down outside the reconnect path. The
    /// engine that playback is enabled with afterwards starts where the last
    /// one was heard as well.
    #[test]
    fn signing_out_keeps_the_level_being_heard_for_the_next_engine() {
        let (runtime, mut worker, _) = worker("sign-out-volume");
        let _entered = runtime.enter();
        worker.signed_in = true;
        worker.engine_config.initial_volume = crate::app::percent_to_volume(80);
        worker.heard = Some(Heard::at(crate::app::percent_to_volume(5)));
        worker.sign_out();
        assert_eq!(
            worker.engine_config.initial_volume,
            crate::app::percent_to_volume(5)
        );
        assert!(worker.heard.is_none());
    }

    #[test]
    fn signing_out_discards_restricted_devices_and_late_discovery_results() {
        let (runtime, mut worker, _) = worker("sign-out-connect");
        let _entered = runtime.enter();
        let previous = Arc::clone(&worker.connect);
        let device = Device {
            id: Some("sonos".into()),
            is_restricted: true,
            ..Default::default()
        };
        previous.observe_playing(1, Some(&device));
        assert_eq!(
            worker.connect.restricted_target(None).as_deref(),
            Some("sonos")
        );
        worker.sign_out();
        previous.remember(&[crate::zeroconf::Receiver {
            name: "Old discovery".into(),
            device_id: Some("late-speaker".into()),
            address: "127.0.0.1".parse().unwrap(),
            port: 1400,
            path: "/spotifyzc".into(),
            cast: false,
        }]);
        assert_eq!(worker.connect.restricted_target(None), None);
        assert_eq!(worker.connect.restricted_target(Some("sonos")), None);
        assert_eq!(worker.connect.restricted_target(Some("late-speaker")), None);
    }

    #[test]
    fn an_invalid_saved_proxy_keeps_restored_network_work_blocked() {
        let (runtime, mut worker, events) = worker("proxy-restore-invalid");
        let _entered = runtime.enter();
        let invalid = ProxyConfig::Invalid("Proxy port must be a number".into());
        worker.engine_config.proxy = invalid.clone();
        worker.restoring_proxy = true;
        worker.on_proxy_restored(
            worker.credentials.lease(CredentialSlot::Proxy),
            Ok(crate::credentials::Loaded {
                grant: None,
                warning: None,
            }),
        );
        assert!(worker.http.client().is_err());
        assert!(!worker.restoring_proxy);
        assert!(events.try_iter().any(
            |event| matches!(event, Event::ProxyRestored { config, .. } if config == invalid)
        ));
    }

    #[test]
    fn explicit_proxy_apply_can_complete_before_native_restoration() {
        let (runtime, mut worker, events) = worker("proxy-restore-apply");
        let _entered = runtime.enter();
        worker.restoring_proxy = true;
        let old = worker.credentials.lease(CredentialSlot::Proxy);
        worker.change_proxy(1, ProxyConfig::System, false);
        assert!(!worker.restoring_proxy);
        assert!(worker.http.client().is_ok());
        worker.on_proxy_restored(
            old,
            Ok(crate::credentials::Loaded {
                grant: None,
                warning: None,
            }),
        );
        assert_eq!(worker.engine_config.proxy, ProxyConfig::System);
        assert!(
            events
                .try_iter()
                .all(|event| !matches!(event, Event::ProxyRestored { .. }))
        );
    }

    #[test]
    fn audio_settings_cannot_revert_a_proxy_waiting_for_its_ui_acknowledgement() {
        let (runtime, mut worker, _) = worker("proxy-audio-restart");
        let old_audio = worker.engine_config.clone();
        let settings = crate::settings::Settings {
            proxy_mode: crate::settings::ProxyMode::Http,
            proxy_host: "127.0.0.1".into(),
            proxy_port: "8080".into(),
            ..Default::default()
        };
        let proxy = settings.proxy_config().unwrap();
        let (commands, receiver) = mpsc::unbounded_channel();
        commands
            .send(Command::ApplyProxy {
                request: 1,
                config: proxy.clone(),
            })
            .unwrap();
        commands.send(Command::RestartEngine(old_audio)).unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));
        assert_eq!(worker.engine_config.proxy, proxy);
    }

    #[test]
    fn a_rejected_proxy_change_does_not_replace_the_live_configuration() {
        let (runtime, mut worker, events) = worker("rejected-proxy");
        let _entered = runtime.enter();
        let original = worker.engine_config.proxy.clone();
        let bad = ProxyConfig::Invalid("Proxy port must be a number".into());
        worker.change_proxy(1, bad.clone(), false);
        assert_eq!(worker.engine_config.proxy, original);
        assert!(!worker.engine_restart_pending);
        assert!(worker.http.client().is_ok());
        let answers: Vec<_> = events.try_iter().collect();
        assert_eq!(answers.len(), 1);
        assert!(
            matches!(&answers[0], Event::ProxyApplied { config, result: Err(_), .. } if config == &bad)
        );
    }

    #[test]
    fn expired_grants_are_forgotten_and_a_new_sign_in_completes_without_restart() {
        let (runtime, mut worker, events) = worker("expired-then-sign-in");
        runtime.block_on(async {
            verify(&mut worker, ApiSource::Shared, "alice");
            let old = worker.credentials.lease(CredentialSlot::Shared);
            let grant = StoredGrant::Web(crate::auth::StoredToken {
                client_id: crate::auth::DEFAULT_WEB_CLIENT_ID.into(),
                access_token: "dummy-expired-access".into(),
                refresh_token: "dummy-rejected-refresh".into(),
                ..Default::default()
            });
            old.save(grant).await.unwrap();
            worker.on_web_verification_failed(
                ApiSource::Shared,
                ApiError::SignInExpired {
                    api_source: ApiSource::Shared,
                },
            );
            assert!(!old.current());
            assert!(!worker.signed_in);
            assert!(
                worker
                    .credentials
                    .lease(CredentialSlot::Shared)
                    .load()
                    .await
                    .unwrap()
                    .grant
                    .is_none()
            );
            let _ = events.try_iter().collect::<Vec<_>>();
            verify(&mut worker, ApiSource::Shared, "alice");
            assert!(worker.signed_in);
            assert!(
                events
                    .try_iter()
                    .any(|event| matches!(event, Event::Auth(AuthStatus::Connected { .. })))
            );
        });
    }

    #[test]
    fn old_api_and_verification_errors_cannot_sign_out_a_new_session() {
        let (runtime, mut worker, events) = worker("old-api-after-sign-in");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        let generation = *worker.session.borrow();
        let shared = worker.credentials.lease(CredentialSlot::Shared);
        let personal = worker.credentials.lease(CredentialSlot::Personal);
        worker.sign_out();
        verify(&mut worker, ApiSource::Shared, "bob");
        let _ = events.try_iter().collect::<Vec<_>>();
        let (commands, receiver) = mpsc::unbounded_channel();
        commands
            .send(Command::ApiFinished {
                generation,
                home_account: None,
                response: Box::new(ApiResponse::Me(Err(ApiError::SignInExpired {
                    api_source: ApiSource::Shared,
                }))),
                expired: vec![ApiSource::Shared],
                shared_lease: shared.clone(),
                personal_lease: personal,
            })
            .unwrap();
        commands
            .send(Command::WebVerificationFailed {
                source: ApiSource::Shared,
                lease: shared,
                attempt: 0,
                error: ApiError::SignInExpired {
                    api_source: ApiSource::Shared,
                },
            })
            .unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));
        assert!(worker.signed_in);
        assert_eq!(worker.api.account(), Some(AccountId::new("bob")));
        assert!(events.try_iter().next().is_none());
    }

    #[test]
    fn prerelease_waits_for_restore_and_reports_a_missing_playback_grant() {
        let (_runtime, mut worker, events) = worker("prerelease-cold-start");
        worker.signed_in = true;
        worker.restore_pending[CredentialSlot::Playback.index()] = true;
        worker.prerelease_waiting = Some(("spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL".into(), 4));
        worker.start_prerelease();
        assert!(worker.prerelease_waiting.is_some());
        assert!(events.try_recv().is_err());
        worker.on_credentials_restored(
            CredentialSlot::Playback,
            worker.credentials.lease(CredentialSlot::Playback),
            Ok(crate::credentials::Loaded {
                grant: None,
                warning: None,
            }),
        );
        assert!(worker.prerelease_waiting.is_none());
        assert!(events.try_iter().any(|event| matches!(
            event,
            Event::Prerelease {
                generation: 4,
                result: Err(_),
                ..
            }
        )));
    }

    #[test]
    fn offline_prerelease_without_playback_credentials_reports_authorization_before_reconnect() {
        let (runtime, mut worker, events) = worker("prerelease-offline-no-grant");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        let uri = "spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL";
        let (commands, receiver) = mpsc::unbounded_channel();
        commands.send(Command::NetworkStatus(false)).unwrap();
        commands
            .send(Command::Prerelease {
                uri: uri.into(),
                generation: 8,
            })
            .unwrap();
        commands.send(Command::NetworkStatus(true)).unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));

        assert!(worker.prerelease_waiting.is_none());
        assert!(!worker.waiting_for_network);
        assert!(!worker.engine_busy);
        let received = events.try_iter().collect::<Vec<_>>();
        let failure = received.iter().position(|event| {
            matches!(
                event,
                Event::Prerelease {
                    uri: returned_uri,
                    generation: 8,
                    result: Err(error),
                } if returned_uri == uri && error.contains("playback authorization")
            )
        });
        let reconnected = received
            .iter()
            .position(|event| matches!(event, Event::NetworkStatus(true)));
        assert!(failure.unwrap() < reconnected.unwrap());
    }

    #[test]
    fn prerelease_waits_for_network_without_losing_the_pending_page() {
        let (runtime, mut worker, events) = worker("prerelease-offline");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        worker.playback_grant = Some(Credentials::with_password("alice", "grant"));
        worker.network_reachable = false;
        worker.prerelease_waiting = Some(("spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL".into(), 5));
        worker.start_prerelease();
        assert!(worker.prerelease_waiting.is_some());
        assert!(worker.waiting_for_network);
        assert!(!worker.engine_busy);
        assert!(
            !events
                .try_iter()
                .any(|event| matches!(event, Event::Prerelease { .. }))
        );
    }

    #[test]
    fn prerelease_waits_with_fresh_pending_credentials_while_offline() {
        let (runtime, mut worker, events) = worker("prerelease-pending-offline");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        worker.pending_playback_grant = Some(Credentials::with_password("alice", "fresh-grant"));
        worker.network_reachable = false;
        worker.prerelease_waiting = Some(("spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL".into(), 7));
        worker.start_prerelease();
        assert!(worker.prerelease_waiting.is_some());
        assert!(worker.waiting_for_network);
        assert!(worker.pending_playback_grant.is_some());
        assert!(
            !events
                .try_iter()
                .any(|event| matches!(event, Event::Prerelease { .. }))
        );
    }

    #[test]
    fn terminal_playback_failure_finishes_a_queued_prerelease() {
        let (runtime, mut worker, events) = worker("prerelease-terminal-error");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        worker.prerelease_waiting = Some(("spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL".into(), 6));
        worker.on_engine_connected(
            worker.album_type_lookup.session_generation,
            None,
            Some("bad credentials".into()),
            worker.engine_attempt,
        );
        assert!(worker.prerelease_waiting.is_none());
        assert!(events.try_iter().any(|event| matches!(
            event,
            Event::Prerelease {
                generation: 6,
                result: Err(_),
                ..
            }
        )));
    }

    /// Successful fallback cards still revoke the expired personal grant on the command loop.
    #[test]
    fn successful_home_fallback_clears_only_the_expired_personal_grant() {
        let (runtime, mut worker, events) = worker("home-fallback-expiration");
        let _entered = runtime.enter();
        for source in [ApiSource::Shared, ApiSource::Personal] {
            worker.api.install(source, AccountId::new("alice")).unwrap();
        }
        let personal_lease = worker.credentials.lease(CredentialSlot::Personal);
        let (commands, receiver) = mpsc::unbounded_channel();
        commands
            .send(Command::ApiFinished {
                generation: *worker.session.borrow(),
                home_account: Some(AccountId::new("alice")),
                response: Box::new(ApiResponse::MadeForYou {
                    generation: 1,
                    personalized: false,
                    result: Ok(Vec::new()),
                    top_mixes: Err(ApiError::RateLimited),
                }),
                expired: vec![ApiSource::Personal],
                shared_lease: worker.credentials.lease(CredentialSlot::Shared),
                personal_lease: personal_lease.clone(),
            })
            .unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));
        assert!(!personal_lease.current());
        assert!(!worker.api.personal_ready());
        assert_eq!(worker.api.account(), Some(AccountId::new("alice")));
        assert!(events.try_iter().any(|event| matches!(
            event, Event::Api(response) if matches!(response.as_ref(),
                ApiResponse::MadeForYou { personalized: false, result: Ok(_), .. })
        )));
    }

    /// A Home answer cannot cross account replacement or grant renewal without sign-out.
    #[test]
    fn late_home_answers_are_discarded_after_reauthorization() {
        for replace_account in [false, true] {
            let (runtime, mut worker, events) = worker("late-home-reauthorization");
            let _entered = runtime.enter();
            worker
                .api
                .install(ApiSource::Shared, AccountId::new("alice"))
                .unwrap();
            let shared_lease = worker.credentials.lease(CredentialSlot::Shared);
            let personal_lease = worker.credentials.lease(CredentialSlot::Personal);
            if replace_account {
                worker.api.clear(ApiSource::Shared);
                worker
                    .api
                    .install(ApiSource::Shared, AccountId::new("bob"))
                    .unwrap();
            } else {
                worker.credentials.invalidate(CredentialSlot::Shared);
            }
            let (commands, receiver) = mpsc::unbounded_channel();
            commands
                .send(Command::ApiFinished {
                    generation: *worker.session.borrow(),
                    home_account: Some(AccountId::new("alice")),
                    response: Box::new(ApiResponse::MadeForYou {
                        generation: 1,
                        personalized: true,
                        result: Ok(vec![Playlist {
                            id: "old-account".into(),
                            ..Default::default()
                        }]),
                        top_mixes: Ok(Vec::new()),
                    }),
                    expired: Vec::new(),
                    shared_lease,
                    personal_lease,
                })
                .unwrap();
            commands.send(Command::Shutdown).unwrap();
            runtime.block_on(worker.run(receiver));
            assert!(
                events
                    .try_iter()
                    .all(|event| !matches!(event, Event::Api(_)))
            );
        }
    }

    /// Top mixes open like Made for you cards once a Home answer names their owner.
    #[test]
    fn home_answers_record_top_mix_ownership() {
        let (runtime, mut worker, events) = worker("home-top-mixes-access");
        let _entered = runtime.enter();
        worker
            .api
            .install(ApiSource::Shared, AccountId::new("alice"))
            .unwrap();
        let mix = Playlist {
            id: "mix".into(),
            owner: Owner {
                id: Some("spotify".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let (commands, receiver) = mpsc::unbounded_channel();
        commands
            .send(Command::ApiFinished {
                generation: *worker.session.borrow(),
                home_account: Some(AccountId::new("alice")),
                response: Box::new(ApiResponse::MadeForYou {
                    generation: 1,
                    personalized: true,
                    result: Ok(Vec::new()),
                    top_mixes: Ok(vec![mix]),
                }),
                expired: Vec::new(),
                shared_lease: worker.credentials.lease(CredentialSlot::Shared),
                personal_lease: worker.credentials.lease(CredentialSlot::Personal),
            })
            .unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));
        assert_eq!(
            worker.api.playlist_access("mix"),
            crate::api::PlaylistAccess::External
        );
        assert!(events.try_iter().any(|event| matches!(
            event, Event::Api(response) if matches!(response.as_ref(),
                ApiResponse::MadeForYou { top_mixes: Ok(mixes), .. } if mixes.len() == 1)
        )));
    }

    pub(super) fn worker(
        name: &str,
    ) -> (
        tokio::runtime::Runtime,
        Worker,
        std::sync::mpsc::Receiver<Event>,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let root =
            std::env::temp_dir().join(format!("spotifast-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let settings = crate::settings::Settings::default();
        let config = crate::app::engine_config(
            &dirs,
            &settings,
            ProxyConfig::Off,
            crate::vis::AudioTap::new(),
            crate::eq::shared(),
        );
        let http = Http::new(reqwest::Client::new());
        let art = ArtLoader::new(http.clone(), runtime.handle().clone(), dirs.art_cache_dir());
        let (sender, events) = std::sync::mpsc::channel();
        let (commands, _) = mpsc::unbounded_channel();
        let worker = Worker::new(
            dirs,
            config,
            Some("personal".into()),
            http,
            art,
            Arc::new(NetActivity::default()),
            sender,
            commands,
            Waker::default(),
        );
        (runtime, worker, events)
    }

    #[test]
    fn occupied_login_port_reports_persistent_failure_before_browser_sign_in() {
        let (runtime, mut worker, events) = worker("occupied-login-port");
        let _entered = runtime.enter();
        let _occupied =
            std::net::TcpListener::bind(("127.0.0.1", crate::auth::WEB_REDIRECT_PORT)).unwrap();
        for _ in 0..2 {
            worker.sign_in_source(ApiSource::Shared);
            let received: Vec<_> = events.try_iter().collect();
            assert!(received.iter().any(|event| matches!(event,
                Event::Auth(AuthStatus::Failed(message)) if message.contains("8989")
            )));
            assert!(
                !received.iter().any(|event| matches!(
                    event,
                    Event::Auth(AuthStatus::WaitingForBrowser { .. })
                ))
            );
            assert!(worker.cancel_signin.is_none());
            assert!(worker.authorizing_source.is_none());
            assert_eq!(worker.authorization_attempt, 0);
        }
    }

    /// Tests that bind the playback callback port take turns.
    static PLAYBACK_PORT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn failed(events: &std::sync::mpsc::Receiver<Event>) -> Vec<String> {
        events
            .try_iter()
            .filter_map(|event| match event {
                Event::Playback(LocalPlayback::Failed(message)) => Some(message),
                _ => None,
            })
            .collect()
    }

    /// A connection reset with the network up is retried with the saved
    /// grant, quietly and with a growing backoff (#818).
    #[test]
    fn failed_engine_connection_retries_with_the_saved_grant() {
        let (runtime, mut worker, events) = worker("engine-retry");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        worker.playback_grant = Some(Credentials::with_password("alice", "dummy-grant"));
        let session = worker.album_type_lookup.session_generation;
        let reset = "Couldn't reach Spotify: connection reset";

        worker.connect_engine(Credentials::with_password("alice", "dummy-grant"));
        worker.on_engine_connected(session, None, Some(reset.into()), worker.engine_attempt);
        assert_eq!(failed(&events), [reset]);
        assert_eq!(worker.engine_retries, 1);
        assert!(worker.playback_grant.is_some());

        // A retry superseded by something newer does nothing.
        worker.retry_engine(worker.engine_retry_generation - 1);
        assert!(!worker.engine_busy);

        worker.retry_engine(worker.engine_retry_generation);
        assert!(worker.engine_busy);
        assert!(worker.engine_retry_quiet);
        let received: Vec<_> = events.try_iter().collect();
        assert!(
            !received
                .iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Connecting))),
            "a background retry flickered the status"
        );
        worker.on_engine_connected(session, None, Some(reset.into()), worker.engine_attempt);
        assert!(
            failed(&events).is_empty(),
            "a quiet retry repeated the error"
        );
        assert_eq!(worker.engine_retries, 2);
        assert!(!worker.engine_retry_quiet);
    }

    /// A rejected grant is not retried in the background, and nothing is
    /// retried while the network is down.
    #[test]
    fn rejected_grant_and_offline_failures_are_not_retried() {
        let (runtime, mut worker, events) = worker("engine-retry-rejected");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        worker.playback_grant = Some(Credentials::with_password("alice", "dummy-grant"));
        let session = worker.album_type_lookup.session_generation;

        worker.connect_engine(Credentials::with_password("alice", "dummy-grant"));
        worker.on_engine_connected(
            session,
            None,
            Some("Spotify rejected the saved sign-in. Please sign in again.".into()),
            worker.engine_attempt,
        );
        assert_eq!(failed(&events).len(), 1);
        assert_eq!(worker.engine_retries, 0);

        worker.network_reachable = false;
        worker.connect_engine(Credentials::with_password("alice", "dummy-grant"));
        worker.on_engine_connected(
            session,
            None,
            Some("timed out".into()),
            worker.engine_attempt,
        );
        assert!(worker.waiting_for_network);
        assert_eq!(worker.engine_retries, 0);
    }

    /// "Set up playback here" reconnects with a saved grant instead of
    /// clearing it and opening the browser (#818, #824).
    #[test]
    fn set_up_playback_reconnects_with_the_saved_grant_first() {
        let (runtime, mut worker, events) = worker("set-up-with-saved-grant");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        worker.playback_grant = Some(Credentials::with_password("alice", "dummy-grant"));
        let lease = worker.credentials.lease(CredentialSlot::Playback);
        let attempt = worker.authorization_attempt;
        let _ = events.try_iter().count();

        worker.authorize_playback();

        assert!(worker.engine_busy);
        assert!(worker.authorize_if_rejected);
        assert!(worker.cancel_signin.is_none());
        assert_eq!(worker.authorization_attempt, attempt);
        assert!(lease.current(), "the saved grant was invalidated");
        let received: Vec<_> = events.try_iter().collect();
        assert!(
            received
                .iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Connecting)))
        );
        assert!(
            !received
                .iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Authorizing)))
        );

        // Only Spotify rejecting the grant goes to the browser. The port is
        // held so the browser flow stops at its port check and opens nothing.
        let _port = PLAYBACK_PORT.lock().unwrap_or_else(|p| p.into_inner());
        let _occupied =
            std::net::TcpListener::bind(("127.0.0.1", crate::auth::PLAYBACK_REDIRECT_PORT))
                .unwrap();
        let session = worker.album_type_lookup.session_generation;
        worker.on_engine_connected(
            session,
            None,
            Some("Spotify rejected the saved sign-in. Please sign in again.".into()),
            worker.engine_attempt,
        );
        let messages = failed(&events);
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("8898"), "{messages:?}");
        assert!(!worker.authorize_if_rejected);
    }

    /// Clicking while a background retry is connecting shows that attempt
    /// rather than starting another one.
    #[test]
    fn set_up_playback_during_a_quiet_retry_shows_it() {
        let (runtime, mut worker, events) = worker("set-up-during-retry");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        worker.playback_grant = Some(Credentials::with_password("alice", "dummy-grant"));
        worker.engine_retry_quiet = true;
        worker.connect_engine(Credentials::with_password("alice", "dummy-grant"));
        let engine_attempt = worker.engine_attempt;
        let _ = events.try_iter().count();

        worker.authorize_playback();

        assert_eq!(worker.engine_attempt, engine_attempt);
        assert!(!worker.engine_retry_quiet);
        assert!(worker.authorize_if_rejected);
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Connecting)))
        );
    }

    /// A busy playback callback port reports the conflict and leaves the
    /// saved playback grant alone, without opening the browser (#899).
    #[test]
    fn occupied_playback_port_keeps_the_saved_grant() {
        let _port = PLAYBACK_PORT.lock().unwrap_or_else(|p| p.into_inner());
        let (runtime, mut worker, events) = worker("occupied-playback-port");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        let saved = worker.credentials.lease(CredentialSlot::Playback);
        runtime
            .block_on(saved.save(StoredGrant::Playback(Credentials::with_password(
                "alice",
                "dummy-saved-grant",
            ))))
            .unwrap();
        let _ = events.try_iter().count();
        let _occupied =
            std::net::TcpListener::bind(("127.0.0.1", crate::auth::PLAYBACK_REDIRECT_PORT))
                .unwrap();
        let attempt = worker.authorization_attempt;

        worker.authorize_playback();

        let received: Vec<_> = events.try_iter().collect();
        assert!(received.iter().any(|event| matches!(event,
            Event::Playback(LocalPlayback::Failed(message)) if message.contains("8898")
        )));
        assert!(
            !received
                .iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Authorizing)))
        );
        assert!(worker.cancel_signin.is_none());
        assert_eq!(worker.authorization_attempt, attempt);
        assert!(saved.current(), "the saved grant was invalidated");
        assert!(matches!(
            runtime.block_on(saved.load()).unwrap().grant,
            Some(StoredGrant::Playback(grant)) if grant.username.as_deref() == Some("alice")
        ));
    }

    #[test]
    fn cover_confirmation_checks_the_largest_image_without_spotify_credentials() {
        use std::io::{Read, Write};
        for matches in [false, true] {
            let (runtime, mut worker, events) = worker("cover-check");
            let pixels = image::RgbImage::from_pixel(24, 24, image::Rgb([20, 40, 200]));
            let mut bytes = std::io::Cursor::new(Vec::new());
            pixels
                .write_to(&mut bytes, image::ImageFormat::Png)
                .unwrap();
            let cover = crate::playlist_cover::prepare(bytes.get_ref()).unwrap();
            let returned = if matches {
                cover.jpeg.clone()
            } else {
                let earlier = image::RgbImage::from_pixel(24, 24, image::Rgb([200, 20, 40]));
                let mut bytes = std::io::Cursor::new(Vec::new());
                earlier
                    .write_to(&mut bytes, image::ImageFormat::Png)
                    .unwrap();
                crate::playlist_cover::prepare(bytes.get_ref())
                    .unwrap()
                    .jpeg
            };
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0 && request.len() < 8192);
                    request.extend_from_slice(&buffer[..count]);
                }
                let request = String::from_utf8(request).unwrap().to_lowercase();
                assert!(request.starts_with("get /full "));
                assert!(!request.contains("authorization:"));
                write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",returned.len()).unwrap();
                socket.write_all(&returned).unwrap();
            });
            let images = vec![
                crate::api::models::Image {
                    url: format!("http://{address}/thumbnail"),
                    width: Some(64),
                    height: Some(64),
                },
                crate::api::models::Image {
                    url: format!("http://{address}/full"),
                    width: Some(640),
                    height: Some(640),
                },
            ];
            let (commands, receiver) = mpsc::unbounded_channel();
            commands
                .send(Command::CheckPlaylistCover {
                    id: "pl1".into(),
                    request: 7,
                    cover,
                    images: images.clone(),
                })
                .unwrap();
            runtime.block_on(async {
                let controller = async {
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        if let Ok(event) = events.try_recv() {
                            match event {
                                Event::PlaylistCoverChecked {
                                    id,
                                    request,
                                    images: checked,
                                    result,
                                } => {
                                    assert_eq!(id, "pl1");
                                    assert_eq!(request, 7);
                                    assert_eq!(checked, images);
                                    assert_eq!(result, Ok(matches));
                                    commands.send(Command::Shutdown).unwrap();
                                    break;
                                }
                                _ => panic!("unexpected event during isolated cover check"),
                            }
                        }
                        assert!(
                            tokio::time::Instant::now() < deadline,
                            "cover check timed out"
                        );
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                };
                tokio::join!(worker.run(receiver), controller);
            });
            server.join().unwrap();
        }
    }

    #[test]
    fn old_protected_web_grants_wait_for_cover_consent_without_erasing_credentials() {
        for slot in [CredentialSlot::Shared, CredentialSlot::Personal] {
            let (runtime, mut worker, events) = worker("cover-consent");
            let lease = worker.credentials.lease(slot);
            let token = crate::auth::StoredToken {
                client_id: if slot == CredentialSlot::Shared {
                    crate::auth::DEFAULT_WEB_CLIENT_ID.into()
                } else {
                    "personal".into()
                },
                access_token: "dummy-access".into(),
                refresh_token: "dummy-refresh".into(),
                expires_at: u64::MAX,
                scope: crate::auth::WEB_SCOPES
                    .iter()
                    .filter(|scope| **scope != "ugc-image-upload")
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            runtime
                .block_on(lease.save(StoredGrant::Web(token)))
                .unwrap();
            worker.restore_pending[slot.index()] = true;
            let loaded = runtime.block_on(lease.load());
            worker.on_credentials_restored(slot, lease.clone(), loaded);
            assert!(!worker.restore_pending[slot.index()]);
            assert!(worker.web_tokens[slot.index()].is_none());
            assert!(!worker.signed_in);
            let emitted: Vec<_> = events.try_iter().collect();
            // Signing in renews the shared app; a personal app is renewed only
            // by authorizing it again in Settings (#634).
            let advice = if slot == CredentialSlot::Shared {
                "Sign in again."
            } else {
                "Authorize it again in Settings, under Account."
            };
            assert!(emitted.iter().any(|event| matches!(event,
                Event::Error(message)
                    if message.contains("permissions changed") && message.ends_with(advice))));
            assert!(
                emitted
                    .iter()
                    .any(|event| matches!(event, Event::Auth(AuthStatus::SignedOut)))
            );
            assert!(lease.current());
            assert!(matches!(runtime.block_on(lease.load()).unwrap().grant,
                Some(StoredGrant::Web(saved)) if saved.refresh_token == "dummy-refresh"));
        }
    }

    #[test]
    fn signout_rejects_late_restore_browser_verification_and_engine_results() {
        let (runtime, mut worker, events) = worker("late-authorization-results");
        let shared = worker.credentials.lease(CredentialSlot::Shared);
        let playback = worker.credentials.lease(CredentialSlot::Playback);
        let attempt = worker.authorization_attempt;
        let album_type_session = worker.album_type_lookup.session_generation;
        let token = crate::auth::StoredToken {
            client_id: crate::auth::DEFAULT_WEB_CLIENT_ID.into(),
            access_token: "dummy-access".into(),
            refresh_token: "dummy-refresh".into(),
            ..Default::default()
        };
        let (commands, receiver) = mpsc::unbounded_channel();
        commands.send(Command::SignOut).unwrap();
        commands
            .send(Command::CredentialsRestored {
                slot: CredentialSlot::Playback,
                lease: playback.clone(),
                result: Ok(crate::credentials::Loaded {
                    grant: Some(StoredGrant::Playback(Credentials::with_password(
                        "dummy-account",
                        "dummy-grant",
                    ))),
                    warning: None,
                }),
            })
            .unwrap();
        commands
            .send(Command::WebSignedIn {
                source: ApiSource::Shared,
                token: Box::new(token.clone()),
                lease: shared.clone(),
                attempt,
            })
            .unwrap();
        commands
            .send(Command::WebVerified {
                source: ApiSource::Shared,
                token: Box::new(token),
                user: Box::new(User {
                    id: "dummy-account".into(),
                    product: Some("premium".into()),
                    ..Default::default()
                }),
                lease: shared,
                attempt,
            })
            .unwrap();
        commands
            .send(Command::PlaybackAuthorized {
                access_token: "dummy-streaming-token".into(),
                lease: playback.clone(),
                attempt,
            })
            .unwrap();
        commands
            .send(Command::EngineConnected {
                session_generation: album_type_session,
                engine: Box::new(None),
                error: Some("late engine error".into()),
                lease: playback,
                attempt,
            })
            .unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));
        assert!(!worker.signed_in);
        assert!(!worker.engine_busy);
        assert!(worker.playback_grant.is_none());
        assert!(worker.web_tokens.iter().all(Option::is_none));
        assert!(worker.api.account().is_none());
        assert!(events.try_iter().all(|event| !matches!(
            event,
            Event::Auth(AuthStatus::Connected { .. })
                | Event::Playback(
                    LocalPlayback::Connecting
                        | LocalPlayback::WaitingForNetwork
                        | LocalPlayback::Ready { .. }
                        | LocalPlayback::Failed(_)
                )
        )));
        let _ = std::fs::remove_dir_all(worker.dirs.state.parent().unwrap());
    }

    #[test]
    fn restored_playback_requires_the_verified_account() {
        let (runtime, mut worker, events) = worker("playback-account-mismatch");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        worker.playback_grant = Some(Credentials::with_password("bob", "dummy-reusable-grant"));
        worker.resume_engine();
        assert!(!worker.engine_busy);
        assert!(worker.engine.is_none());
        assert!(!playback_account_matches(
            worker.playback_grant.as_ref().unwrap(),
            worker.api.account()
        ));
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Failed(_))))
        );
    }

    #[test]
    fn engine_cache_never_writes_a_playback_grant_file() {
        let (_runtime, worker, _) = worker("memory-playback-cache");
        let cache = worker.engine_config.open_cache().unwrap();
        let cloned = cache.clone();
        cache.save_credentials(&Credentials::with_password("dummy-account", "dummy-grant"));
        assert!(cloned.credentials().is_some());
        assert!(
            !worker
                .dirs
                .credentials_dir()
                .join("credentials.json")
                .exists()
        );
        let _ = std::fs::remove_dir_all(worker.dirs.state.parent().unwrap());
    }

    #[test]
    fn search_requests_keep_their_scope_when_personal_readiness_changes() {
        let (_runtime, worker, _) = worker("search-routing");
        for ready in [false, true] {
            worker.api.set_state(
                ApiSource::Personal,
                if ready {
                    SessionState::Ready {
                        account: AccountId::new("alice"),
                    }
                } else {
                    SessionState::Unavailable
                },
            );
            assert_eq!(
                operation_for(
                    &worker.api,
                    &ApiRequest::Search {
                        query: "q".into(),
                        serial: 1,
                    }
                ),
                Operation::PlaylistSearch
            );
            assert_eq!(
                operation_for(
                    &worker.api,
                    &ApiRequest::SearchCatalogue {
                        query: "q".into(),
                        serial: 1,
                    }
                ),
                Operation::CatalogSearch
            );
            assert_eq!(
                operation_for(
                    &worker.api,
                    &ApiRequest::SearchPlaylists {
                        query: "q".into(),
                        serial: 1,
                    }
                ),
                Operation::PlaylistSearch
            );
        }
    }

    #[test]
    fn new_empty_queries_and_signout_cancel_searches_waiting_for_shared_access() {
        let (runtime, mut worker, _) = worker("search-cancellation");
        runtime.block_on(async {
            for cancel in 0..3 {
                worker
                    .api
                    .set_state(ApiSource::Shared, SessionState::Authorizing);
                worker.search("old".into(), 1);
                tokio::task::yield_now().await;
                let old = worker.search_tasks[0].clone();
                assert!(!old.is_finished(), "old search waits for shared access");
                match cancel {
                    0 => worker.search("new".into(), 2),
                    1 => worker.search(String::new(), 2),
                    _ => worker.sign_out(),
                }
                tokio::task::yield_now().await;
                assert!(old.is_finished(), "abandoned search was cancelled");
                worker.cancel_search();
            }
        });
    }

    pub(super) fn verify(worker: &mut Worker, source: ApiSource, account: &str) {
        worker.api.set_state(source, SessionState::Authorizing);
        worker.on_web_verified(
            source,
            crate::auth::StoredToken {
                client_id: if source == ApiSource::Personal {
                    "personal"
                } else {
                    crate::auth::DEFAULT_WEB_CLIENT_ID
                }
                .into(),
                ..Default::default()
            },
            User {
                id: account.into(),
                display_name: Some("Listener".into()),
                product: Some("premium".into()),
                ..Default::default()
            },
        );
    }

    #[test]
    fn personal_verification_unblocks_sign_in_while_shared_verification_waits() {
        let (runtime, mut worker, events) = worker("personal-first");
        let _entered = runtime.enter();
        worker
            .api
            .set_state(ApiSource::Shared, SessionState::Authorizing);
        verify(&mut worker, ApiSource::Personal, "alice");
        assert!(worker.signed_in);
        assert_eq!(worker.premium, Some(true));
        assert_eq!(
            worker.api.state(ApiSource::Shared),
            SessionState::Authorizing
        );
        let emitted: Vec<_> = events.try_iter().collect();
        assert!(
            emitted
                .iter()
                .any(|event| matches!(event, Event::Auth(AuthStatus::Connected { .. })))
        );
        assert!(emitted.iter().any(|event| matches!(event, Event::Api(response) if matches!(response.as_ref(), ApiResponse::Me(Ok(user)) if user.id == "alice"))));
        let credentials =
            playback_credentials(worker.api.account(), "dummy-streaming-token".into()).unwrap();
        assert_eq!(credentials.username.as_deref(), Some("alice"));
        assert_eq!(credentials.auth_data, b"dummy-streaming-token");
        verify(&mut worker, ApiSource::Shared, "alice");
        assert!(worker.signed_in);
        assert_eq!(worker.api.account(), Some(AccountId::new("alice")));
        assert!(
            !events
                .try_iter()
                .any(|event| matches!(event, Event::Auth(AuthStatus::Connected { .. })))
        );
    }

    /// The rootlist carries the edit permission for a playlist shared by
    /// invitation, and its request is sent once, when the playlist library
    /// finishes. A cached web token can finish that before the engine
    /// connects, so the request has to wait rather than be dropped.
    /// A header read that finishes while the sign-in has lapsed is not
    /// shown. It frees the queue rather than holding it forever, and goes
    /// back to the front so it is read again first once the sign-in returns.
    #[test]
    fn a_playlist_header_finishing_while_signed_out_is_read_again() {
        let (runtime, mut worker, events) = worker("playlist-header-lapse");
        let _entered = runtime.enter();
        worker
            .playlist_header_lookup
            .enqueue(vec!["read".into(), "next".into()]);
        let request = worker
            .playlist_header_lookup
            .next(*worker.session.borrow())
            .expect("read before the lapse");
        worker.signed_in = false;

        worker.on_playlist_header_resolved(
            request,
            Some(crate::api::models::Playlist {
                id: "read".into(),
                name: "This Is TUYU".into(),
                ..Default::default()
            }),
        );

        assert!(worker.playlist_header_lookup.active.is_none());
        assert_eq!(worker.playlist_header_lookup.pending, ["read", "next"]);
        assert!(
            !events
                .try_iter()
                .any(|event| matches!(event, Event::PlaylistHeader(_))),
            "nothing is shown while signed out"
        );

        worker.signed_in = true;
        let retry = worker
            .playlist_header_lookup
            .next(*worker.session.borrow())
            .expect("read again after the sign-in returns");
        assert_eq!(retry.id, "read");
    }

    #[test]
    fn a_rootlist_request_before_the_engine_waits_for_it() {
        let (runtime, mut worker, events) = worker("rootlist-before-engine");
        let _entered = runtime.enter();
        assert!(worker.engine.is_none());

        worker.signed_in = true;
        worker.fetch_rootlist();
        worker.fetch_rootlist();
        assert!(worker.rootlist_pending);

        let (commands, mut results) = mpsc::unbounded_channel();
        worker.commands = commands;
        runtime.block_on(async {
            let fetched = || async {
                Ok(crate::player::Rootlist {
                    entries: vec![crate::player::RootlistEntry::Playlist(
                        "spotify:playlist:shared".into(),
                    )],
                    editable: ["spotify:playlist:shared".into()].into(),
                })
            };
            worker.start_pending_rootlist(Some(fetched()));
            worker.start_pending_rootlist(Some(fetched()));
            let Command::RootlistFinished { generation, result } = results.recv().await.unwrap()
            else {
                panic!("expected the deferred rootlist");
            };
            assert!(
                result
                    .as_ref()
                    .unwrap()
                    .editable
                    .contains("spotify:playlist:shared")
            );
            worker.on_rootlist_finished(generation, result);
            tokio::task::yield_now().await;
            assert!(
                results.try_recv().is_err(),
                "engine readiness fetches only once"
            );
            assert!(!worker.rootlist_pending);
        });
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Rootlist { result: Ok(_) })
        ));
    }

    #[test]
    fn signout_cancels_pending_and_completed_rootlist_work() {
        let (runtime, mut worker, events) = worker("rootlist-signout");
        let _entered = runtime.enter();
        worker.signed_in = true;
        worker.fetch_rootlist();
        let generation = *worker.session.borrow();
        worker.sign_out();
        assert!(!worker.rootlist_pending);
        worker.fetch_rootlist();
        assert!(
            !worker.rootlist_pending,
            "signed-out work must not be deferred"
        );
        events.try_iter().for_each(drop);
        worker.signed_in = true;
        worker.on_rootlist_finished(generation, Err("previous account".into()));
        assert!(
            events.try_recv().is_err(),
            "late results cannot reach a new account"
        );
        assert!(!worker.rootlist_pending);
    }

    #[test]
    fn premium_without_local_playback_keeps_album_type_work_bounded() {
        let (runtime, mut worker, _) = worker("album-types-without-playback");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        assert_eq!(worker.premium, Some(true));
        assert!(worker.playback_grant.is_none());
        assert!(worker.engine.is_none());

        worker.fetch_album_types(
            (0..MAX_PENDING_ALBUM_TYPES + 10)
                .map(|index| format!("spotify:album:{index}"))
                .collect(),
        );

        assert!(worker.album_type_lookup.active.is_none());
        assert_eq!(
            worker.album_type_lookup.pending.len(),
            MAX_PENDING_ALBUM_TYPES
        );
        assert_eq!(worker.album_type_lookup.seen.len(), MAX_PENDING_ALBUM_TYPES);
    }

    #[test]
    fn a_mismatched_grant_cannot_replace_the_verified_playback_account() {
        let (runtime, mut worker, events) = worker("mismatch");
        let _entered = runtime.enter();
        verify(&mut worker, ApiSource::Shared, "alice");
        let _ = events.try_iter().collect::<Vec<_>>();
        verify(&mut worker, ApiSource::Personal, "bob");
        assert_eq!(worker.api.account(), Some(AccountId::new("alice")));
        assert_eq!(
            worker.api.state(ApiSource::Personal),
            SessionState::Unavailable
        );
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Error(_)))
        );
    }

    #[test]
    fn a_playback_browser_result_after_sign_out_cannot_start_an_engine() {
        let (_runtime, mut worker, events) = worker("signed-out");
        worker.engine_busy = true;
        worker.on_playback_authorized("dummy-streaming-token".into());
        assert!(!worker.engine_busy);
        assert!(worker.engine.is_none());
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Failed(_))))
        );
        assert!(playback_credentials(Some(AccountId::new("")), "dummy".into()).is_none());
    }
}

fn web_slot(source: ApiSource) -> CredentialSlot {
    match source {
        ApiSource::Shared => CredentialSlot::Shared,
        ApiSource::Personal => CredentialSlot::Personal,
    }
}

fn playback_account_matches(credentials: &Credentials, account: Option<AccountId>) -> bool {
    credentials
        .username
        .as_deref()
        .filter(|name| !name.is_empty())
        .is_some_and(|name| same_account(name, account.as_ref()))
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::session_reads::Failure;

    /// Queued Shared and session work must not delay a Personal background read.
    #[tokio::test]
    async fn shared_and_session_saturation_do_not_block_personal_background_reads() {
        let api = ApiGateway::new(reqwest::Client::new(), Arc::new(NetActivity::default()));
        for source in [ApiSource::Shared, ApiSource::Personal] {
            api.set_state(
                source,
                SessionState::Ready {
                    account: AccountId::new("test-account"),
                },
            );
        }
        let shared = api.client_for(Operation::PlaylistLibrary).await.unwrap();
        let mut held_shared = Vec::new();
        for _ in 0..4 {
            held_shared.push(shared.background_permit().await.unwrap());
        }
        let background_session = tokio::sync::Semaphore::new(4);
        let _held_session = background_session.acquire_many(4).await.unwrap();
        let (response, expired) = tokio::time::timeout(
            Duration::from_secs(1),
            handle(
                &api,
                &crate::made_for_you::MadeForYou::new(crate::http::Http::default()),
                &crate::new_releases::NewReleases::new(crate::http::Http::default()),
                &crate::sonos::Controller::default(),
                None,
                &background_session,
                ApiRequest::TopArtists { generation: 7 },
            ),
        )
        .await
        .expect("a personal background read must reach its own client");
        assert!(matches!(
            response,
            ApiResponse::TopArtists {
                generation: 7,
                result: Err(ApiError::NotSignedIn)
            }
        ));
        assert!(expired.is_empty());
    }

    /// Popular-track responses retain artist identity and the existing fallback/error policy.
    #[test]
    fn popular_tracks_keep_the_requested_artist_and_existing_failure_policy() {
        let api = ApiGateway::new(reqwest::Client::new(), Arc::new(NetActivity::default()));
        let request = ApiRequest::ArtistTopTracks {
            id: "artist1".into(),
        };
        assert_eq!(operation_for(&api, &request), Operation::ArtistTopTracks);
        assert_eq!(
            session_read(&request),
            Some(SessionRead::ArtistTop { id: "artist1" })
        );
        assert!(
            matches!(session_response(&request, SessionAnswer::ArtistTop(Ok(Vec::new()))),
            Some(ApiResponse::ArtistTopTracks { id, result: Ok(tracks) }) if id == "artist1" && tracks.is_empty())
        );
        let transient = settle(Err::<Vec<Track>, Failure>(Failure::Retry(anyhow::anyhow!(
            "connection lost"
        ))));
        assert!(
            transient.is_none(),
            "transient failures retain Web API fallback"
        );
        let refused = settle(Err::<Vec<Track>, Failure>(Failure::Definitive(
            ApiError::Status {
                status: 403,
                message: "Forbidden".into(),
            },
        )))
        .unwrap();
        assert!(matches!(
            session_response(&request, SessionAnswer::ArtistTop(refused)),
            Some(ApiResponse::ArtistTopTracks {
                result: Err(ApiError::Status { status: 403, .. }),
                ..
            })
        ));
    }

    /// The session reads the library whole, so its answer to any page of a
    /// load is the whole list from the top, with nothing after it (#698).
    #[test]
    fn the_session_answers_any_library_page_with_the_whole_list() {
        let listed = |id: &str| Playlist {
            id: id.into(),
            ..Playlist::default()
        };
        for offset in [0, 50] {
            let request = ApiRequest::MyPlaylists {
                offset,
                generation: 7,
            };
            assert_eq!(session_read(&request), Some(SessionRead::Library));
            let answer = SessionAnswer::Library(Ok(vec![listed("a"), listed("b")]));
            let Some(ApiResponse::MyPlaylists {
                offset: 0,
                generation: 7,
                result: Ok(page),
            }) = session_response(&request, answer)
            else {
                panic!("expected the whole library from the top");
            };
            assert_eq!(page.items.len(), 2);
            assert_eq!((page.total, page.next_offset()), (2, None));
        }
    }

    /// The session answers only for the account the Web API verified.
    #[test]
    fn the_session_answers_only_for_the_web_apis_account() {
        let alice = AccountId::new("alice");
        assert!(same_account("alice", Some(&alice)));
        assert!(!same_account("bob", Some(&alice)));
        assert!(!same_account("alice", None), "nothing verified yet");
    }

    /// A header read, a page of rows or a sample at the request's offset,
    /// and nothing else: a duplicate check shares the rows' operation but
    /// stays with the Web API.
    #[test]
    fn a_request_asks_the_session_for_a_header_or_a_page_or_nothing() {
        let playlist = ApiRequest::Playlist {
            id: "pl1".into(),
            generation: 1,
        };
        assert_eq!(
            session_read(&playlist),
            Some(SessionRead::Header { id: "pl1" })
        );
        let items = ApiRequest::PlaylistItems {
            id: "pl1".into(),
            offset: 150,
            generation: 1,
        };
        assert_eq!(
            session_read(&items),
            Some(SessionRead::Rows {
                id: "pl1",
                offset: 150
            })
        );
        let sample = ApiRequest::PlaylistSample {
            id: "pl1".into(),
            offset: 150,
            generation: 1,
        };
        assert_eq!(
            session_read(&sample),
            Some(SessionRead::Sample {
                id: "pl1",
                offset: 150
            })
        );
        let duplicates = ApiRequest::CheckPlaylistDuplicates {
            playlist_id: "pl1".into(),
            playlist_name: "Mine".into(),
            items: Vec::new(),
            position: None,
        };
        assert_eq!(session_read(&duplicates), None);
        assert_eq!(session_read(&ApiRequest::Me), None);
    }

    /// A session answer reaches the app as the Web API's would: a value, a
    /// final refusal, or nothing, so the Web API is asked instead.
    #[test]
    fn a_session_answer_settles_like_a_web_api_one() {
        assert!(matches!(settle(Ok::<u8, Failure>(7)), Some(Ok(7))));
        let refused = settle(Err::<u8, Failure>(Failure::Definitive(ApiError::Status {
            status: 403,
            message: "Forbidden".into(),
        })));
        assert!(matches!(
            refused,
            Some(Err(ApiError::Status { status: 403, .. }))
        ));
        let dropped = settle(Err::<u8, Failure>(Failure::Retry(anyhow::anyhow!("gone"))));
        assert!(dropped.is_none(), "the Web API gets its turn");
    }

    /// The response carries the request's own id, offset, and generation;
    /// the app drops an answer whose generation is not the page's.
    #[test]
    fn a_session_answer_carries_the_requests_identity() {
        let rows = || SessionAnswer::Rows(Ok(Page::default()));
        let header = || SessionAnswer::Header(Box::new(Ok(Playlist::default())));
        let items = ApiRequest::PlaylistItems {
            id: "pl1".into(),
            offset: 150,
            generation: 7,
        };
        assert!(matches!(
            session_response(&items, rows()),
            Some(ApiResponse::PlaylistItems { id, offset: 150, generation: 7, result: Ok(_) }) if id == "pl1"
        ));
        let sample = ApiRequest::PlaylistSample {
            id: "pl1".into(),
            offset: 150,
            generation: 7,
        };
        assert!(matches!(
            session_response(&sample, rows()),
            Some(ApiResponse::PlaylistSample { id, generation: 7, result: Ok(_) }) if id == "pl1"
        ));
        let playlist = ApiRequest::Playlist {
            id: "pl1".into(),
            generation: 7,
        };
        assert!(matches!(
            session_response(&playlist, header()),
            Some(ApiResponse::Playlist { id, generation: 7, result: Ok(_) }) if id == "pl1"
        ));
        assert!(
            session_response(&playlist, rows()).is_none(),
            "rows are no header"
        );
        assert!(
            session_response(&ApiRequest::Me, header()).is_none(),
            "nothing else is served here"
        );
    }
}

#[cfg(test)]
mod cover_routing_tests {
    use super::*;
    use crate::api::gateway::{AccountId, PlaylistAccess};

    #[test]
    fn cover_upload_uses_the_existing_playlist_mutation_route() {
        let api = ApiGateway::new(
            reqwest::Client::new(),
            std::sync::Arc::new(NetActivity::default()),
        );
        api.install(ApiSource::Shared, AccountId::new("me"))
            .unwrap();
        let request = ApiRequest::UploadPlaylistCover {
            id: "test".into(),
            request: 1,
            previous_urls: vec![],
            cover: crate::playlist_cover::Cover {
                jpeg: Vec::new().into(),
                encoded: "".into(),
                uri: String::new(),
            },
        };
        assert_eq!(
            operation_for(&api, &request),
            Operation::PlaylistMutation(PlaylistAccess::Unknown)
        );
        let mut playlist = Playlist {
            id: "test".into(),
            ..Default::default()
        };
        playlist.owner.id = Some("me".into());
        api.observe_playlist(&playlist);
        assert_eq!(
            operation_for(&api, &request),
            Operation::PlaylistMutation(PlaylistAccess::Owned)
        );
        playlist.owner.id = Some("other".into());
        playlist.collaborative = true;
        api.observe_playlist(&playlist);
        assert_eq!(
            operation_for(&api, &request),
            Operation::PlaylistMutation(PlaylistAccess::Collaborative)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Avoids shared playlist search after a successful session read.
    #[tokio::test]
    async fn made_for_you_uses_shared_search_only_after_session_failure() {
        let session_playlist = Playlist {
            id: "session".into(),
            ..Default::default()
        };
        let (result, personalized, expired) =
            with_shared_made_for_you_fallback(Ok(vec![session_playlist.clone()]), || async {
                panic!("the successful session must not search")
            })
            .await;
        assert_eq!(result.unwrap(), vec![session_playlist]);
        assert!(personalized);
        assert!(expired.is_empty());

        let (result, personalized, expired) =
            with_shared_made_for_you_fallback(Err(ApiError::RateLimited), || async {
                Ok(vec![Playlist {
                    id: "shared".into(),
                    ..Default::default()
                }])
            })
            .await;
        assert_eq!(result.unwrap()[0].id, "shared");
        assert!(!personalized);
        assert!(expired.is_empty());
    }

    /// A successful shared fallback must not conceal a rejected personal grant.
    #[tokio::test]
    async fn made_for_you_preserves_expiration_across_fallback_results() {
        for fallback_error in [None, Some(ApiSource::Shared), Some(ApiSource::Personal)] {
            let (result, personalized, expired) = with_shared_made_for_you_fallback(
                Err(ApiError::SignInExpired {
                    api_source: ApiSource::Personal,
                }),
                || async {
                    match fallback_error {
                        Some(api_source) => Err(ApiError::SignInExpired { api_source }),
                        None => Ok(Vec::new()),
                    }
                },
            )
            .await;
            assert_eq!(result.is_ok(), fallback_error.is_none());
            assert!(!personalized);
            assert_eq!(
                expired,
                if fallback_error == Some(ApiSource::Shared) {
                    vec![ApiSource::Personal, ApiSource::Shared]
                } else {
                    vec![ApiSource::Personal]
                }
            );
        }
    }

    /// Limits fallback results to Spotify-owned lists with playable contexts.
    #[test]
    fn shared_suggestions_are_spotify_owned_and_exclude_dj() {
        let mut playlist = Playlist {
            name: "Daily Mix 3".into(),
            uri: "spotify:playlist:mix".into(),
            ..Default::default()
        };
        playlist.owner.id = Some("spotify".into());
        assert!(shared_made_for_you_match(&playlist, "Daily Mix"));
        playlist.name = "Daily Mix Party".into();
        assert!(!shared_made_for_you_match(&playlist, "Daily Mix"));
        playlist.name = "Daily Mix 3".into();
        playlist.owner.id = Some("other".into());
        playlist.owner.display_name = Some("Spotify".into());
        assert!(!shared_made_for_you_match(&playlist, "Daily Mix"));
        playlist.owner.id = None;
        assert!(!shared_made_for_you_match(&playlist, "Daily Mix"));
        playlist.owner.id = Some("spotify".into());
        playlist.uri = UNSUPPORTED_DJ_URI.into();
        assert!(!shared_made_for_you_match(&playlist, "Daily Mix"));
    }

    #[tokio::test]
    async fn connect_shuffle_play_waits_for_shuffle_before_starting_playback() {
        let request = PlayRequest::context("spotify:playlist:abc");
        let shuffle = crate::sonos::command_body(RemoteAction::Shuffle, None, 0, true, "").unwrap();
        let play =
            crate::sonos::command_body(RemoteAction::Play, Some(&request), 0, false, "").unwrap();
        let sent = std::sync::Mutex::new(Vec::new());
        let shuffled = tokio::sync::Notify::new();
        let sending = shuffle_then_play(shuffle.clone(), play.clone(), |body| {
            let is_shuffle = body["command"]["endpoint"] == "set_shuffling_context";
            sent.lock().unwrap().push(body);
            let shuffled = &shuffled;
            async move {
                if is_shuffle {
                    shuffled.notified().await;
                }
                Ok(())
            }
        });
        tokio::pin!(sending);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(sending.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(
            sent.lock().unwrap().as_slice(),
            std::slice::from_ref(&shuffle)
        );
        assert_eq!(shuffle["command"]["value"], true);
        shuffled.notify_one();
        sending.await.unwrap();
        assert_eq!(*sent.lock().unwrap(), [shuffle, play]);
    }

    #[tokio::test]
    async fn connect_shuffle_play_preserves_errors_and_skips_play_when_shuffle_fails() {
        let request = PlayRequest::context("spotify:playlist:abc");
        let shuffle = crate::sonos::command_body(RemoteAction::Shuffle, None, 0, true, "").unwrap();
        let play =
            crate::sonos::command_body(RemoteAction::Play, Some(&request), 0, false, "").unwrap();
        for failed_endpoint in ["set_shuffling_context", "play"] {
            let mut sent = Vec::new();
            let result = shuffle_then_play(shuffle.clone(), play.clone(), |body| {
                let failed = body["command"]["endpoint"] == failed_endpoint;
                sent.push(body);
                std::future::ready(if failed {
                    Err(ApiError::Network(failed_endpoint.into()))
                } else {
                    Ok(())
                })
            })
            .await;
            assert!(matches!(result, Err(ApiError::Network(error)) if error == failed_endpoint));
            if failed_endpoint == "set_shuffling_context" {
                assert_eq!(sent.as_slice(), std::slice::from_ref(&shuffle));
            } else {
                assert_eq!(sent, [shuffle.clone(), play.clone()]);
            }
        }
    }

    #[test]
    fn six_session_drops_in_ten_minutes_give_up() {
        let start = Instant::now();
        let mut reconnects = Vec::new();
        for i in 0..RECONNECT_LIMIT {
            let at = start + Duration::from_secs(i as u64);
            assert!(
                !session_drops_exhausted(&mut reconnects, at),
                "attempt {i} should still reconnect"
            );
            reconnects.push(at);
        }
        assert!(session_drops_exhausted(
            &mut reconnects,
            start + Duration::from_secs(30)
        ));
    }

    #[test]
    fn session_drops_outside_the_window_do_not_count() {
        let start = Instant::now();
        let mut reconnects = vec![start; RECONNECT_LIMIT];
        assert!(!session_drops_exhausted(
            &mut reconnects,
            start + RECONNECT_WINDOW + Duration::from_secs(1)
        ));
        assert!(reconnects.is_empty());
    }

    #[test]
    fn an_engine_change_during_connect_is_deferred() {
        let mut pending = false;
        assert!(defer_engine_replace(true, &mut pending));
        assert!(pending);
        assert!(!defer_engine_replace(false, &mut pending));
        assert!(pending, "finishing the attempt owns clearing the request");
    }

    #[test]
    fn test_session_drop_when_unreachable_preserves_resume_and_skips_drop_counter() {
        let (runtime, mut worker, events) = super::authorization_tests::worker("drop-unreachable");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");
        worker.network_reachable = false;
        worker.resume = Some(PlaybackResume::Track(crate::player::LoadSpec::default()));
        worker.reconnect_engine();
        assert!(worker.resume.is_some());
        assert!(worker.waiting_for_network);
        assert!(worker.reconnects.is_empty());
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::WaitingForNetwork)))
        );
    }

    #[test]
    fn test_network_restored_triggers_resume_and_reconnection() {
        let (runtime, mut worker, events) = super::authorization_tests::worker("network-restored");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");
        worker.playback_grant = Some(Credentials::with_password("alice", "grant"));
        worker.network_reachable = false;
        worker.waiting_for_network = true;
        worker.resume = Some(PlaybackResume::Track(crate::player::LoadSpec::default()));

        let (commands, receiver) = mpsc::unbounded_channel();
        commands.send(Command::NetworkStatus(true)).unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));

        assert!(!worker.waiting_for_network);
        assert!(worker.network_reachable);
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Connecting)))
        );
    }

    #[test]
    fn test_stale_connection_outcome_ignored() {
        let (runtime, mut worker, events) = super::authorization_tests::worker("stale-outcome");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");
        worker.engine_attempt = 2;
        worker.resume = Some(PlaybackResume::Track(crate::player::LoadSpec::default()));
        let session_generation = worker.album_type_lookup.session_generation;

        worker.on_engine_connected(session_generation, None, Some("stale failure".into()), 1);

        assert!(worker.resume.is_some());
        assert!(
            events
                .try_iter()
                .all(|event| !matches!(event, Event::Playback(LocalPlayback::Failed(_))))
        );
    }

    #[test]
    fn test_user_stop_clears_waiting_resume() {
        let (runtime, mut worker, _events) = super::authorization_tests::worker("user-load-clears");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");
        worker.waiting_for_network = true;
        worker.resume = Some(PlaybackResume::Track(crate::player::LoadSpec::default()));

        let (commands, receiver) = mpsc::unbounded_channel();
        commands
            .send(Command::Player(PlayerCommand::Load(
                crate::player::LoadSpec::default(),
            )))
            .unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));

        assert!(worker.resume.is_none());
        assert!(worker.waiting_for_network);
    }

    #[test]
    fn test_connection_failure_while_offline_does_not_clear_resume() {
        let (runtime, mut worker, events) = super::authorization_tests::worker("offline-failure");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");
        worker.network_reachable = false;
        worker.engine_attempt = 1;
        worker.resume = Some(PlaybackResume::Track(crate::player::LoadSpec::default()));
        let session_generation = worker.album_type_lookup.session_generation;

        worker.on_engine_connected(session_generation, None, Some("timed out".into()), 1);

        assert!(worker.resume.is_some());
        assert!(worker.waiting_for_network);
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::WaitingForNetwork)))
        );
    }

    #[test]
    fn test_offline_first_connection_failure_retains_credentials_for_recovery() {
        let (runtime, mut worker, events) =
            super::authorization_tests::worker("offline-first-connect");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");
        assert!(worker.playback_grant.is_none());
        assert!(worker.pending_playback_grant.is_none());

        worker.network_reachable = false;
        let creds = Credentials::with_password("alice", "streaming-token");
        worker.connect_engine(creds);
        assert!(worker.pending_playback_grant.is_some());

        let session_generation = worker.album_type_lookup.session_generation;
        let attempt = worker.engine_attempt;
        worker.on_engine_connected(session_generation, None, Some("timed out".into()), attempt);

        assert!(worker.waiting_for_network);
        assert!(worker.pending_playback_grant.is_some());
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::WaitingForNetwork)))
        );

        let (commands, receiver) = mpsc::unbounded_channel();
        commands.send(Command::NetworkStatus(true)).unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));

        assert!(!worker.waiting_for_network);
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Connecting)))
        );
    }

    #[test]
    fn test_online_connection_failure_clears_pending_credentials() {
        let (runtime, mut worker, events) =
            super::authorization_tests::worker("online-connect-failure");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");
        worker.network_reachable = true;
        let creds = Credentials::with_password("alice", "bad-token");
        worker.connect_engine(creds);
        assert!(worker.pending_playback_grant.is_some());

        let session_generation = worker.album_type_lookup.session_generation;
        let attempt = worker.engine_attempt;
        worker.on_engine_connected(
            session_generation,
            None,
            Some("bad credentials".into()),
            attempt,
        );

        assert!(!worker.waiting_for_network);
        assert!(worker.pending_playback_grant.is_none());
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Failed(_))))
        );
    }

    #[test]
    fn test_reauthorization_during_outage_prefers_new_credentials_over_older_grant() {
        let (runtime, mut worker, events) =
            super::authorization_tests::worker("reauth-during-outage");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");

        // Existing older playback grant
        worker.playback_grant = Some(Credentials::with_password("alice", "stale-grant"));

        // User reauthorizes with new credentials while offline
        worker.network_reachable = false;
        let new_creds = Credentials::with_password("alice", "fresh-token");
        worker.connect_engine(new_creds.clone());

        // Attempt fails while offline
        let session_generation = worker.album_type_lookup.session_generation;
        let attempt = worker.engine_attempt;
        worker.on_engine_connected(session_generation, None, Some("timed out".into()), attempt);

        assert!(worker.waiting_for_network);
        assert_eq!(worker.pending_playback_grant, Some(new_creds.clone()));

        // When network returns, resume_engine must prefer the new pending credentials over stale grant
        let (commands, receiver) = mpsc::unbounded_channel();
        commands.send(Command::NetworkStatus(true)).unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));

        assert!(!worker.waiting_for_network);
        assert_eq!(worker.pending_playback_grant, Some(new_creds));
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Connecting)))
        );
    }

    #[test]
    fn test_resume_engine_while_offline_suspends_and_does_not_connect() {
        let (runtime, mut worker, events) =
            super::authorization_tests::worker("offline-resume-guard");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");

        worker.playback_grant = Some(Credentials::with_password("alice", "grant"));
        worker.network_reachable = false;
        worker.resume = Some(PlaybackResume::Track(crate::player::LoadSpec::default()));

        worker.resume_engine();

        assert!(worker.waiting_for_network);
        assert!(!worker.engine_busy);
        assert!(worker.resume.is_some());
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::WaitingForNetwork)))
        );
        assert!(
            events
                .try_iter()
                .all(|event| !matches!(event, Event::Playback(LocalPlayback::Connecting)))
        );
    }

    #[test]
    fn test_resume_engine_without_credentials_does_not_emit_waiting_for_network() {
        let (runtime, mut worker, events) = super::authorization_tests::worker("offline-no-grant");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");

        assert!(worker.playback_grant.is_none());
        assert!(worker.pending_playback_grant.is_none());
        worker.network_reachable = false;

        worker.resume_engine();

        assert!(!worker.waiting_for_network);
        assert!(
            events
                .try_iter()
                .all(|event| !matches!(event, Event::Playback(LocalPlayback::WaitingForNetwork)))
        );
    }

    #[test]
    fn test_outage_ending_before_failed_attempt_reports_retries_and_preserves_resume() {
        let (runtime, mut worker, events) =
            super::authorization_tests::worker("outage-ends-before-failure");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");

        worker.playback_grant = Some(Credentials::with_password("alice", "grant"));
        worker.engine_attempt = 1;
        worker.resume = Some(PlaybackResume::Track(crate::player::LoadSpec::default()));
        let session_generation = worker.album_type_lookup.session_generation;

        // Connection was in flight when outage occurred
        worker.engine_busy = true;
        let (commands, receiver) = mpsc::unbounded_channel();
        // Network went down
        commands.send(Command::NetworkStatus(false)).unwrap();
        // Network returned before engine attempt finished
        commands.send(Command::NetworkStatus(true)).unwrap();
        commands.send(Command::Shutdown).unwrap();
        runtime.block_on(worker.run(receiver));

        assert!(worker.network_reachable);
        assert!(worker.waiting_for_network);

        // Now the failed engine connection from during the outage reports
        worker.on_engine_connected(
            session_generation,
            None,
            Some("Connecting to Spotify timed out".into()),
            1,
        );

        // It must preserve resume, not emit Failed, and schedule a retry
        assert!(worker.resume.is_some());
        let recorded = events.try_iter().collect::<Vec<_>>();
        assert!(
            recorded
                .iter()
                .all(|event| !matches!(event, Event::Playback(LocalPlayback::Failed(_))))
        );
        assert!(
            recorded
                .iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Connecting)))
        );
    }

    #[test]
    fn test_terminal_credential_failure_after_outage_is_not_retried() {
        let (runtime, mut worker, events) =
            super::authorization_tests::worker("terminal-failure-after-outage");
        let _entered = runtime.enter();
        super::authorization_tests::verify(&mut worker, ApiSource::Shared, "alice");

        worker.playback_grant = Some(Credentials::with_password("alice", "grant"));
        worker.engine_attempt = 1;
        worker.resume = Some(PlaybackResume::Track(crate::player::LoadSpec::default()));
        let session_generation = worker.album_type_lookup.session_generation;

        // Waiting for network flagged from an outage
        worker.waiting_for_network = true;
        worker.network_reachable = true;

        // Terminal credential error reported
        worker.on_engine_connected(
            session_generation,
            None,
            Some("Spotify rejected the saved sign-in. Please sign in again.".into()),
            1,
        );

        assert!(!worker.waiting_for_network);
        assert!(worker.resume.is_none());
        assert!(worker.pending_playback_grant.is_none());
        assert!(
            events
                .try_iter()
                .any(|event| matches!(event, Event::Playback(LocalPlayback::Failed(_))))
        );
    }
}
