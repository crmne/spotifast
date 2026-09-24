//! The application: state, event handling, and the actions views ask for.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::Color32;

use crate::api::PlayRequest;
use crate::api::models::{
    Album, Artist, ArtistRef, Device, PlayableItem, PlaybackState, Playlist, PlaylistItem, Queue,
    Show, Track, TrackCount, User, UserRef, pick_image,
};
use crate::backend::{
    ApiRequest, ApiResponse, AuthStatus, Backend, Command, Event, LocalPlayback, LyricsRequest,
    PLAYLIST_PAGE_SIZE, PlaylistCacheRows, RecentsFor, RemoteAction, Waker,
};
use crate::i18n::{Locale, gettext, ngettext};
use crate::media::{MediaCommand, MediaState, MediaTrack};
use crate::media_controls::MediaService;
use crate::model::QueueTab;
use crate::model::*;
use crate::paths::AppDirs;
use crate::player::{EngineConfig, LoadSpec, LocalState, Playback, PlayerCommand, RepeatMode};
use crate::settings::{CachedRootlist, SessionState, Settings, ThemeChoice};
use crate::single_instance::ControlCommand;
use crate::theme::{self, Palette};
use crate::tray::{TrayCommand, TrayService};
use crate::util;

const REMOTE_POLL_ACTIVE: Duration = Duration::from_secs(4);
const REMOTE_POLL_IDLE: Duration = Duration::from_secs(20);
const REMOTE_FRESH: Duration = Duration::from_secs(45);
const DEVICES_FRESH: Duration = Duration::from_secs(12);
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(280);
/// How far into a song Previous restarts it rather than stepping back,
/// matching what librespot does during playback.
const RESTART_BEFORE_PREVIOUS: u32 = 3_000;

const TOAST_LIFETIME: Duration = Duration::from_millis(3200);
/// Match other interface animations. egui subtracts the predicted frame time
/// from delayed repaints, so 33 ms drives roughly one frame per 16 ms.
const TOAST_FRAME: Duration = Duration::from_millis(33);
const OPTIMISTIC_HOLD: Duration = Duration::from_millis(2500);

/// How long a newly started context remains visible while Spotify catches up.
/// During local takeover, Spotify may briefly alternate between old and new
/// context state.
const ASSUMED_CONTEXT_HOLD: Duration = Duration::from_secs(8);
/// How long the interface trusts its own play/pause over a polled state that
/// has not caught up yet. Spotify can take a moment to report a command it
/// has already carried out.
const PLAYBACK_HOLD: Duration = Duration::from_secs(6);
/// Delay before checking playback again after a command.
const REMOTE_RECHECK: Duration = Duration::from_millis(1200);
/// Delay before checking the queue after a local change.
const QUEUE_RECHECK: Duration = Duration::from_millis(700);
/// Number of stale queue responses accepted before trusting Spotify's state.
const QUEUE_STALE_RETRIES: u8 = 6;
/// Duplicate queue requests within this window count as one click.
const QUEUE_ADD_DEBOUNCE: Duration = Duration::from_millis(1500);
/// How many played contexts the sidebar's Recently played order keeps.
const RECENT_CONTEXTS_KEPT: usize = 60;
const CONTAINS_BATCH: usize = 40;

pub struct RemoteSnapshot {
    pub state: PlaybackState,
    pub received_at: Instant,
}

/// A context shown as playing before Spotify confirms it.
struct AssumedContext {
    uri: String,
    /// Shuffle state included in the play request, if any.
    shuffle: Option<bool>,
    at: Instant,
}

/// A track the interface shows before playback has reported its new state.
///
/// Local engine events are ordered, so its next track report settles the
/// intent. Remote playback is polled and may lag; one contradictory poll is
/// ignored and a second one settles on what Spotify actually reports.
struct TrackIntent {
    uri: String,
    position_ms: u32,
    at: Instant,
    confirmation: TrackConfirmation,
}

/// What saving the edit dialog sends: only the details that changed.
struct PlaylistDetailChanges {
    name: Option<String>,
    description: Option<String>,
    public: Option<bool>,
    /// The description was cleared, which Spotify doesn't allow.
    kept_description: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackConfirmation {
    Local,
    Remote { after_poll: u64, mismatches: u8 },
}

/// Upcoming context order before shuffle was toggled, used to detect lagging responses.
#[derive(Clone, Debug)]
struct QueueShufflePending {
    current_uri: Option<String>,
    context_uris: Vec<String>,
    at: Instant,
}

/// The playing item as the interface sees it, whichever device plays it.
#[derive(Clone, Debug, PartialEq)]
pub struct NowPlaying {
    pub local: bool,
    pub device_name: Option<String>,
    pub uri: String,
    pub id: Option<String>,
    pub title: String,
    pub artists: Vec<ArtistRef>,
    pub subtitle: String,
    pub album_name: String,
    pub album_id: Option<String>,
    pub show_id: Option<String>,
    pub art_url: Option<String>,
    pub art_small: Option<String>,
    pub duration_ms: u32,
    pub position_ms: u32,
    pub playing: bool,
    pub loading: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub volume_percent: u8,
    pub can_control: bool,
    pub is_episode: bool,
    /// The remembered song from the last session, shown paused before a
    /// first press. Nothing is playing yet.
    pub resuming: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Local,
    Remote(Option<String>),
}

struct PendingAlbumQueue {
    id: String,
    label: String,
    target: Target,
    offset: u32,
    tracks: Vec<Track>,
}

struct PendingQueueAdd {
    item: PlayableItem,
    at: Instant,
    /// Position in `manual_queue`, adjusted whenever an earlier row leaves.
    manual_index: usize,
    /// Remote write and position within it, until Spotify accepts this copy.
    write: Option<(u64, usize)>,
}

/// Song links pasted into a playlist, in clipboard order, while some of
/// the songs are still being looked up.
struct PendingPaste {
    playlist_id: String,
    playlist_name: String,
    uris: Vec<String>,
    /// The songs known so far, by the pasted URI.
    found: HashMap<String, PlayableItem>,
    /// Links Spotify had no song for, or that name an unknown episode.
    missing: HashSet<String>,
}

impl PendingPaste {
    fn settled(&self) -> bool {
        self.uris
            .iter()
            .all(|uri| self.found.contains_key(uri) || self.missing.contains(uri))
    }
}

/// How the application is being started.
#[derive(Clone, Copy, Debug)]
pub struct AppOptions {
    /// Demo and isolated tests must not read or migrate real Spotify grants.
    pub restore_sign_in: bool,
    /// Register the MPRIS media-control service and follow the desktop's
    /// light or dark preference (Linux).
    pub media_controls: bool,
    /// Register the system-tray item (Linux).
    pub tray: bool,
}

impl Default for AppOptions {
    fn default() -> Self {
        Self {
            restore_sign_in: true,
            media_controls: true,
            tray: true,
        }
    }
}

/// Listening time for the current track.
///
/// `listened` stores completed intervals. `playing_since` starts the current
/// interval and is `None` while paused.
struct Listening {
    uri: String,
    listened: std::time::Duration,
    playing_since: Option<Instant>,
    recorded: bool,
}

pub struct App {
    pub dirs: AppDirs,
    pub settings: Settings,
    /// Proxy policy actually handed to network workers. Manual form edits are
    /// drafts until Apply (or Sign in), so unrelated downloads and engine
    /// restarts must not observe them early.
    applied_proxy: crate::settings::ProxyConfig,
    applied_proxy_preferences: crate::settings::ProxyPreferences,
    pending_proxy_preferences: HashMap<u64, crate::settings::ProxyPreferences>,
    proxy_request: u64,
    last_proxy_applied: u64,
    proxy_form_edited: bool,
    settings_dirty: bool,
    last_settings_save: Instant,
    pub backend: Backend,
    media_controls: Option<MediaService>,
    /// The desktop's light or dark preference, for "Follow system".
    #[cfg(target_os = "linux")]
    system_appearance: Option<crate::appearance::SystemAppearance>,
    /// The artwork the media controls were last given, and the URL it came
    /// from. Finding the file touches the disk and the controls are synced
    /// every frame, so the answer is kept until the artwork changes.
    media_art: Option<(String, PathBuf)>,
    tray: Option<TrayService>,
    pub window_hidden: bool,
    /// The window should close but the process should stay in the tray.
    pub hide_intent: bool,
    /// The outer loop should recreate the hidden window.
    pub wants_show: bool,
    /// The window should close and reopen at once as the other kind: the
    /// big window or the Winamp mini player.
    pub switch_intent: bool,
    /// Commands from control clients (a second `spotifast <verb>` launch,
    /// a Raycast script), on the platforms where they do not arrive through
    /// MPRIS. Drained every frame.
    control_commands: Option<std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>>>,
    /// Now-playing snapshot for the control channel.
    control_now_playing: Option<std::sync::Arc<std::sync::Mutex<String>>>,
    /// The same, for its `devices` verb.
    control_devices: Option<std::sync::Arc<std::sync::Mutex<String>>>,
    /// Whether that device slot still matches [`Self::devices`]. The
    /// now-playing snapshot is rebuilt every frame because its position
    /// moves every frame; a device list changes when Spotify answers, which
    /// is seconds apart, so it is written when it changes instead.
    control_devices_stale: bool,
    /// Sample data is loaded; Spotify requests are disabled.
    pub offline: bool,
    pub palette: Palette,
    /// The language the interface is drawn in: [`Settings::language`]
    /// resolved against the operating system's preferred languages.
    pub locale: crate::i18n::Locale,
    /// Whether this window's native backend can keep it above other windows.
    pub window_level_supported: bool,
    /// Whether this window's native backend can leave the mini player out of
    /// the taskbar: Windows and X11.
    pub taskbar_hiding_supported: bool,
    #[cfg(any(test, feature = "demo"))]
    pub demo_windows_controls: bool,
    applied_dark: Option<bool>,
    pub custom_themes: theme::custom::Catalog,

    pub auth: AuthStatus,
    pub user: Option<User>,
    pub local_device_id: Option<String>,
    /// Ignore the previous local song until a Connect transfer reports its track.
    local_transfer_sequence: Option<u64>,
    /// Local playback is authorized and the engine is connected.
    pub local_ready: bool,
    pub local_playback: LocalPlayback,
    pub local: LocalState,
    pub remote: Option<RemoteSnapshot>,
    remote_polled_at: Instant,
    remote_poll_pending: bool,
    /// Serial of the newest playback poll sent; older answers are stale.
    remote_poll_seq: u64,
    /// The restorable session (sorts, recents, resume point) changed and
    /// should be written shortly, not only at exit.
    pub session_dirty: bool,
    last_session_save: Instant,
    /// The saved zoom has been applied to the context once.
    zoom_applied: bool,
    /// Frames left to re-send the Winamp window's always-on-top level after the
    /// window opens. X11 window managers drop `_NET_WM_STATE_ABOVE` set before
    /// the window is mapped, so the creation-time level does not stick; a level
    /// pushed on the first frames after mapping does.
    winamp_level_reassert: u8,
    pub devices: Vec<Device>,
    /// Receivers seen on the local network. Spotify lists a receiver only
    /// once it has an account, so these are the ones it cannot see yet.
    pub receivers: Vec<crate::zeroconf::Receiver>,
    /// The receiver currently being handed the account, by name.
    pub activating_receiver: Option<String>,
    pub devices_loading: bool,
    devices_fetched_at: Option<Instant>,
    pub selected_device: Option<String>,
    pub queue: Loadable<Queue>,
    queue_fetched_at: Option<Instant>,
    /// Latest queue request sequence. Older responses are discarded.
    queue_seq: u64,
    /// When to retry after a stale queue response.
    queue_recheck_at: Option<Instant>,
    queue_stale_retries: u8,
    /// Rows removed by Clear queue, used to detect stale responses.
    queue_cleared: Option<(std::collections::HashSet<String>, Instant)>,
    /// Upcoming context order before shuffle was toggled, used to detect lagging responses.
    queue_shuffle_pending: Option<QueueShufflePending>,
    /// When a local reorder or positional insert last landed, used to
    /// reject a fetch whose queued rows still show the pre-move order.
    queue_reorder_pending: Option<Instant>,
    /// What the window's title bar says, as last set.
    window_title: String,

    pub library: Library,
    liked_songs: crate::liked::LikedSongs,
    liked_recheck_at: Option<Instant>,
    pub home: HomeData,
    /// Local play history. See [`crate::history`].
    pub plays: crate::history::History,
    /// Current track timing used to decide when a play counts.
    listening: Option<Listening>,
    pub recents: crate::model::CursorList<crate::api::models::PlayHistory>,
    /// The Recent tab's rows: what was played here and what
    /// Spotify knows of the other devices, as one list. Rebuilt when
    /// either side changes rather than every frame.
    pub recents_view: Vec<crate::api::models::PlayHistory>,
    pub recents_generation: u64,
    pub queue_tab: QueueTab,
    pub search: SearchState,
    pub playlist_pages: HashMap<String, PlaylistPage>,
    load_generation: u64,
    pub album_pages: HashMap<String, AlbumPage>,
    pub artist_pages: HashMap<String, ArtistPage>,
    pub show_pages: HashMap<String, ShowPage>,
    /// Radio pages by the seed's URI.
    pub radio_pages: HashMap<String, RadioPage>,
    pub track_cache: HashMap<String, Track>,
    track_requests: HashSet<String>,
    /// Album URIs already resolved or attempted through librespot this session.
    album_types_requested: HashSet<String>,
    /// Saved shows Spotify marks as audiobooks, which librespot cannot play;
    /// the Podcasts shelf leaves them out.
    pub audiobook_shows: HashSet<String>,
    /// Saved shows already asked about.
    audiobooks_requested: HashSet<String>,
    /// Album URIs positively identified as EPs by librespot.
    confirmed_ep_albums: HashSet<String>,
    /// Built table rows, keyed by page. Capped; dropped on reset and eviction.
    pub table_rows: HashMap<Page, TableRowsCache>,
    page_used: HashMap<Page, Instant>,
    track_used: HashMap<String, Instant>,

    pub history: Vec<Page>,
    pub history_index: usize,

    pub saved: HashMap<String, bool>,
    saved_pending: HashSet<String>,
    /// Spotify may expose one recording under several market-specific track
    /// URIs. Map those URIs to the recording identity returned by the API.
    track_recordings: HashMap<String, String>,
    /// Known playlist-track availability for this account. Keep positive
    /// answers too, so an older disk cache cannot make a song unavailable
    /// again after the Web API has confirmed it can play.
    playlist_availability: HashMap<String, bool>,
    /// Recording identities for which at least one known URI is saved.
    saved_recordings: HashSet<String>,
    /// Optimistic library writes that a stale contains response must not undo.
    saved_writes: HashMap<String, bool>,
    pub accents: HashMap<String, Color32>,
    accent_pending: HashSet<String>,

    pub dialog: Option<Dialog>,
    cover_request: u64,
    cover_uploads: HashMap<String, u64>,
    /// Successful uploads stay visible while Spotify propagates the new image.
    uploaded_covers: std::collections::HashMap<String, crate::playlist_cover::PendingCover>,
    pub show_queue_panel: bool,
    pub show_lyrics_panel: bool,
    pub lyrics_fullscreen: Option<bool>,
    pub lyrics_fullscreen_seen: bool,
    lyrics_fullscreen_restoring: Option<bool>,
    lyrics_restore_maximized: bool,
    pub lyrics_backdrop: crate::images::LyricsBackdrop,
    pub softened_covers: crate::images::SoftenedCovers,
    /// The track the lyrics below are for.
    pub lyrics_uri: Option<String>,
    /// `Loaded(None)` when no lyrics are available.
    pub lyrics: Loadable<Option<crate::lyrics::Lyrics>>,
    /// Whether the panel follows the current line. Manual scrolling disables
    /// it until Follow is used or the track changes.
    pub lyrics_following: bool,
    /// The line the panel last positioned itself for (`Some(None)` before
    /// the first line), so it moves once per change; `None` until it has
    /// positioned itself at all for this track.
    pub lyrics_line_shown: Option<Option<usize>>,
    pub show_devices: bool,
    pub toasts: Vec<Toast>,
    pub actions: Vec<Action>,
    volume_before_mute: Option<u8>,
    /// Context and track URIs whose pending play buttons show a spinner.
    pending_play_keys: Vec<String>,
    pending_play_at: Option<Instant>,
    /// A play request made while the local engine was still connecting; it
    /// starts the moment the engine reports ready.
    queued_play: Option<PlayRequest>,
    /// Last list sent to local playback. Used for autoplay because librespot
    /// cannot continue a list without a context URI.
    local_list: Option<Vec<String>>,
    /// Activated receiver waiting to appear in Spotify's device list.
    pending_transfer_to: Option<(String, Instant)>,
    /// When to take a confirming look at remote playback after a command.
    remote_recheck_at: Option<Instant>,
    pub seek_preview: Option<f32>,
    pub volume_preview: Option<f32>,
    /// Window geometry to restore on next attach, from the session file.
    session_window_size: Option<[f32; 2]>,
    /// The mode to return the window to when the app closed in
    /// fullscreen lyrics.
    session_lyrics_fullscreen_from: Option<crate::settings::WindowMode>,
    session_window_pos: Option<[f32; 2]>,
    /// Last observed window geometry, updated each frame for saving.
    last_window_size: Option<[f32; 2]>,
    /// Where the open dialog drew itself, so its own layout can be held
    /// to the window it has to fit inside.
    pub dialog_rect: Option<egui::Rect>,
    last_window_pos: Option<[f32; 2]>,
    /// Where the MilkDrop window last was, as it reported, for restoring it.
    pub milkdrop_pos: Option<[f32; 2]>,
    /// The MilkDrop child process; `None` until it is first opened. Its
    /// `Drop` stops the child when the app does.
    #[cfg(feature = "milkdrop")]
    milkdrop_host: Option<crate::milkdrop::host::Host>,
    last_eviction: Instant,
    /// Playback snapshot for the current frame, built once per redraw.
    frame_now: Option<NowPlaying>,
    pub sign_in_url: Option<String>,
    /// The verified personal Web API application, when acceleration is ready.
    pub web_app: Option<String>,
    pending_remote_position: Option<(u32, Instant)>,
    pending_remote_volume: Option<(u8, Instant)>,
    /// A local volume set here that the engine has not echoed back yet. It
    /// reports `VolumeChanged` asynchronously while position snapshots land
    /// every second, so a snapshot must not undo the change on its way past.
    pending_local_volume: Option<(u16, Instant)>,
    optimistic_playing: Option<(bool, Instant)>,
    /// Track shown immediately after a play or skip, until playback reports.
    intent_track: Option<TrackIntent>,
    /// Requested shuffle mode, applied to every context until changed.
    shuffle_wanted: bool,
    /// Last local shuffle change, used to ignore its echo from the engine.
    shuffle_set_at: Option<Instant>,
    /// When tracks recently came up unavailable, to spot a key-service
    /// cascade and reconnect once instead of skipping through an album.
    unavailable_at: Vec<Instant>,
    last_unavailable_reconnect: Option<Instant>,
    /// The Premium notice has been shown for this sign-in.
    premium_notice_shown: bool,
    /// Context shown immediately after play, until Spotify confirms it. An
    /// empty URI means a plain track list, whose lack of a context must also
    /// hide stale state.
    assumed_context: Option<AssumedContext>,
    last_now_playing_uri: Option<String>,
    last_now_playing_sequence: u64,
    /// A requested start whose effect on the manual queue is already applied.
    /// Context loads keep it; explicit skips consume their rows immediately.
    queue_start_pending: Option<Target>,
    pub playlist_busy: bool,
    pub quit_requested: bool,
    /// The axis a scroll gesture settled on, and when it last moved.
    scroll_lock: Option<(ScrollAxis, Instant)>,
    /// Whether the current scroll gesture comes from a trackpad.
    scroll_from_trackpad: bool,
    /// Recent scroll positions, to read the gesture's speed when it ends.
    scroll_history: egui::util::History<egui::Vec2>,
    /// Where the gesture has scrolled to so far, for the history.
    scroll_accum: egui::Vec2,
    /// The speed still carrying the page after the fingers lifted.
    glide: Option<egui::Vec2>,
    /// Time of the last scroll event, used to detect the end of a gesture.
    scroll_last_event: Option<Instant>,
    /// The platform says when fingers touch and lift (Wayland does, X11
    /// does not), so a pause with fingers resting is not taken for a lift.
    scroll_lift_announced: bool,
    autoscroll: crate::autoscroll::Autoscroll,
    /// How each table is sorted, per page, for as long as the app runs.
    /// The rows picked out in a track table, and the page they belong to.
    /// One table at a time: picking rows on another page replaces it.
    /// The page it belongs to, what that page's list looked like when the
    /// rows were picked, and the rows.
    pub selection: Option<(Page, String, RowSelection)>,
    pub table_sorts: HashMap<Page, TableSort>,
    /// User ids resolved to display names; `None` while unknown, so an id
    /// is asked about only once per run.
    pub user_names: HashMap<String, Option<String>>,
    pub user_names_revision: u64,
    /// Context URIs most recently played, newest first: the sidebar's
    /// order. Kept with the session, so it survives a restart.
    pub recent_contexts: Vec<String>,
    /// What was playing when the app last closed, to resume from cold.
    pub resume_context: Option<String>,
    pub resume_track: Option<String>,
    pub resume_position_ms: u32,
    /// Manually queued songs restored with the remembered track.
    pub resume_queue: Vec<String>,
    /// Manually queued songs from this session, oldest first.
    pub manual_queue: Vec<String>,
    /// Queue additions shown before Spotify confirms them, with request time.
    pending_queue_adds: Vec<PendingQueueAdd>,
    pending_album_queues: HashMap<u64, PendingAlbumQueue>,
    pending_queue_batches: HashMap<u64, Target>,
    album_queue_serial: u64,
    last_album_queue: Option<(String, Instant)>,
    /// The account's playlist tree from Spotify, folders and all; empty
    /// until the session answers.
    pub rootlist: Vec<crate::player::RootlistEntry>,
    /// Last good tree and the account it belongs to, kept across restarts.
    rootlist_cache: Option<CachedRootlist>,
    /// Playlists the account may add songs to by Spotify's own word, by
    /// URI: the ones shared with it by invitation, which the Web API's
    /// collaborative flag does not show. Empty until the session answers.
    pub editable_by_grant: std::collections::BTreeSet<String>,
    /// A Spotify link handed over from outside, waiting for the account
    /// to be signed in and, for a song, for its album to be known.
    pending_link: Option<String>,
    /// The songs behind the links last copied from a track table, so a
    /// paste of those links adds rows with their names straight away.
    copied_songs: Vec<PlayableItem>,
    /// Pasted song links waiting for Spotify to describe the songs this
    /// app has not seen, before they are added to their playlist.
    pending_pastes: Vec<PendingPaste>,
    /// Sidebar folders rolled up, by their rootlist ids.
    pub collapsed_folders: Vec<String>,
    /// A newer release than this build, once GitHub has said so.
    pub update: Option<crate::updates::Release>,
    last_update_check: Option<Instant>,
    pub update_checking: bool,
    pub show_update: bool,
    pub update_download: crate::updates::DownloadState,
    pub update_source: crate::updates::Source,
    pub update_support: Option<Result<crate::updates::install::Installation, String>>,
    pub update_restart_arguments: Vec<String>,
    pub update_receipt: Option<PathBuf>,
    /// Winamp window state and active skin.
    pub winamp: crate::winamp::WinampState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollAxis {
    Horizontal,
    Vertical,
}

/// A trackpad gesture that pauses this long has ended; the next movement
/// picks its axis afresh.
const SCROLL_GESTURE_GAP: Duration = Duration::from_millis(150);

/// How far short Linux trackpad deltas land of what other players scroll.
const TRACKPAD_SCALE: f32 = 1.8;

/// The glide's exponential decay time, in seconds; the speed below which a
/// lift starts no glide; and the speed at which a glide stops, points per
/// second.
/// How many plays the Home shelf asks for: it shows sixteen cards.
const HOME_RECENTS: u32 = 50;
/// How many saved podcasts Home reads new episodes from, most recently
/// saved first. Each costs one request per Home refresh.
const HOME_PODCAST_SHOWS: usize = 8;
/// How many plays the Recents tab asks for at a time. Spotify's own
/// endpoint limit is fifty. A shorter page marks the end.
const RECENTS_PAGE: u32 = 50;

const GLIDE_DECAY: f32 = 0.35;
const GLIDE_START: f32 = 120.0;
const GLIDE_STOP: f32 = 40.0;
/// How long fingers may rest on the pad before lifting and still glide,
/// in seconds: the span the release speed is measured over.
const GLIDE_REST: f64 = 0.1;

impl App {
    pub fn new(waker: &Waker, dirs: AppDirs, mut settings: Settings, options: AppOptions) -> Self {
        // The legacy password file has no endpoint of its own. Keep the old
        // settings beside it until migration binds that password in the store.
        settings.proxy_password_legacy |= dirs.proxy_secret_file().try_exists().unwrap_or(true);
        let plays = crate::history::History::load(&dirs.history_file());
        let tap = crate::vis::AudioTap::new();
        let eq = crate::eq::shared();
        if let Ok(mut shared) = eq.lock() {
            *shared = eq_settings(&settings);
        }
        let applied_proxy_preferences = settings.proxy_preferences();
        let applied_proxy = match settings.proxy_config() {
            Ok(proxy) => proxy,
            Err(error) => crate::settings::ProxyConfig::Invalid(error),
        };
        let engine_config = engine_config(
            &dirs,
            &settings,
            applied_proxy.clone(),
            std::sync::Arc::clone(&tap),
            std::sync::Arc::clone(&eq),
        );
        let backend = Backend::spawn(
            dirs.clone(),
            engine_config,
            settings.web_client_id.clone(),
            waker.clone(),
            options.restore_sign_in,
        );
        let locale = settings.language.resolve();
        let applied_proxy = if options.restore_sign_in {
            crate::settings::ProxyConfig::Invalid(
                gettext(locale, "Restoring proxy settings").into_owned(),
            )
        } else {
            applied_proxy
        };
        let session = SessionState::load(&dirs.session_file());
        let wake = waker.clone();
        let media_controls = options
            .media_controls
            .then(|| MediaService::spawn(move || wake.wake()));
        #[cfg(target_os = "linux")]
        let system_appearance = {
            let wake = waker.clone();
            options
                .media_controls
                .then(|| crate::appearance::SystemAppearance::spawn(move || wake.wake()))
        };
        #[cfg(target_os = "macos")]
        let media_controls = {
            let mut media_controls = media_controls;
            if let (Some(controls), Some(track)) =
                (&mut media_controls, session.last_track.as_deref())
            {
                controls.claim_resume(track, session.last_position_ms);
            }
            media_controls
        };
        let wake = waker.clone();
        let tray = options
            .tray
            .then(|| TrayService::spawn(move || wake.wake()))
            .flatten();

        let first_page = session
            .last_page
            .as_deref()
            .and_then(Page::decode)
            .filter(|page| !matches!(page, Page::Settings | Page::Queue))
            .unwrap_or(Page::Home);

        let palette = settings.cached_palette().unwrap_or_else(Palette::dark);
        let mut app = Self {
            custom_themes: theme::custom::Catalog::default(),
            dirs,
            settings,
            applied_proxy,
            applied_proxy_preferences,
            pending_proxy_preferences: HashMap::new(),
            proxy_request: 0,
            last_proxy_applied: 0,
            proxy_form_edited: false,
            settings_dirty: false,
            last_settings_save: Instant::now(),
            backend,
            media_controls,
            #[cfg(target_os = "linux")]
            system_appearance,
            media_art: None,
            tray,
            window_hidden: false,
            hide_intent: false,
            wants_show: false,
            switch_intent: false,
            control_commands: None,
            control_now_playing: None,
            control_devices: None,
            control_devices_stale: true,
            offline: false,
            palette,
            locale,
            window_level_supported: true,
            taskbar_hiding_supported: cfg!(windows),
            #[cfg(any(test, feature = "demo"))]
            demo_windows_controls: false,
            applied_dark: None,
            auth: AuthStatus::Starting,
            user: None,
            local_device_id: None,
            local_transfer_sequence: None,
            local_ready: false,
            local_playback: LocalPlayback::Unavailable,
            local: LocalState::default(),
            remote: None,
            remote_polled_at: Instant::now() - REMOTE_POLL_IDLE,
            remote_poll_pending: false,
            remote_poll_seq: 0,
            session_dirty: false,
            last_session_save: Instant::now(),
            zoom_applied: false,
            winamp_level_reassert: 0,
            devices: Vec::new(),
            receivers: Vec::new(),
            activating_receiver: None,
            devices_loading: false,
            devices_fetched_at: None,
            selected_device: None,
            queue: if session.last_track.is_some() && !session.last_queue_rows.is_empty() {
                // The queue as it was at close, shown until something
                // plays; then the live queue takes over.
                Loadable::Loaded(Queue {
                    currently_playing: None,
                    queue: session.last_queue_rows.clone(),
                })
            } else {
                Loadable::NotLoaded
            },
            queue_fetched_at: None,
            queue_seq: 0,
            queue_recheck_at: None,
            queue_stale_retries: 0,
            queue_cleared: None,
            queue_shuffle_pending: None,
            queue_reorder_pending: None,
            window_title: String::new(),
            library: Library::default(),
            liked_songs: crate::liked::LikedSongs::default(),
            liked_recheck_at: None,
            home: HomeData::default(),
            plays,
            listening: None,
            recents: crate::model::CursorList::default(),
            recents_view: Vec::new(),
            recents_generation: 0,
            queue_tab: session
                .queue_tab
                .as_deref()
                .and_then(QueueTab::decode)
                .unwrap_or_default(),
            search: SearchState::default(),
            playlist_pages: HashMap::new(),
            load_generation: 0,
            album_pages: HashMap::new(),
            artist_pages: HashMap::new(),
            show_pages: HashMap::new(),
            radio_pages: HashMap::new(),
            track_cache: HashMap::new(),
            track_requests: HashSet::new(),
            album_types_requested: HashSet::new(),
            audiobook_shows: HashSet::new(),
            audiobooks_requested: HashSet::new(),
            confirmed_ep_albums: HashSet::new(),
            table_rows: HashMap::new(),
            page_used: HashMap::new(),
            track_used: HashMap::new(),
            history: vec![first_page],
            history_index: 0,
            saved: HashMap::new(),
            saved_pending: HashSet::new(),
            track_recordings: HashMap::new(),
            playlist_availability: HashMap::new(),
            saved_recordings: HashSet::new(),
            saved_writes: HashMap::new(),
            accents: HashMap::new(),
            accent_pending: HashSet::new(),
            dialog: None,
            cover_request: 0,
            cover_uploads: HashMap::new(),
            uploaded_covers: Default::default(),
            show_queue_panel: session.queue_open.unwrap_or(false),
            show_lyrics_panel: false,
            lyrics_fullscreen: None,
            lyrics_fullscreen_seen: false,
            lyrics_fullscreen_restoring: None,
            lyrics_restore_maximized: false,
            lyrics_backdrop: Default::default(),
            softened_covers: Default::default(),
            lyrics_uri: None,
            lyrics: Loadable::NotLoaded,
            lyrics_following: true,
            lyrics_line_shown: None,
            show_devices: false,
            toasts: Vec::new(),
            actions: Vec::new(),
            volume_before_mute: None,
            pending_play_keys: Vec::new(),
            pending_play_at: None,
            queued_play: None,
            local_list: None,
            pending_transfer_to: None,
            remote_recheck_at: None,
            seek_preview: None,
            volume_preview: None,
            session_window_size: session.window_size,
            session_lyrics_fullscreen_from: session.lyrics_fullscreen_from,
            session_window_pos: session.window_pos,
            last_window_size: None,
            dialog_rect: None,
            last_window_pos: None,
            milkdrop_pos: session.milkdrop_pos,
            #[cfg(feature = "milkdrop")]
            milkdrop_host: None,
            last_eviction: Instant::now(),
            frame_now: None,
            sign_in_url: None,
            web_app: None,
            pending_remote_position: None,
            pending_remote_volume: None,
            pending_local_volume: None,
            optimistic_playing: None,
            intent_track: None,
            shuffle_wanted: session.shuffle_on,
            shuffle_set_at: None,
            unavailable_at: Vec::new(),
            last_unavailable_reconnect: None,
            premium_notice_shown: false,
            assumed_context: None,
            last_now_playing_uri: None,
            last_now_playing_sequence: 0,
            queue_start_pending: None,
            playlist_busy: false,
            quit_requested: false,
            scroll_lock: None,
            scroll_from_trackpad: false,
            scroll_history: egui::util::History::new(2..16, 0.1),
            scroll_accum: egui::Vec2::ZERO,
            glide: None,
            scroll_last_event: None,
            scroll_lift_announced: false,
            autoscroll: crate::autoscroll::Autoscroll::default(),
            selection: None,
            table_sorts: session
                .sorts
                .iter()
                .filter_map(|(page, sort)| Some((Page::decode(page)?, *sort)))
                .collect(),
            user_names: HashMap::new(),
            user_names_revision: 0,
            recent_contexts: session.recent_contexts.clone(),
            resume_context: session.last_context.clone(),
            resume_track: session.last_track.clone(),
            resume_position_ms: session.last_position_ms,
            resume_queue: session.last_added_queue.clone(),
            manual_queue: Vec::new(),
            pending_queue_adds: Vec::new(),
            pending_album_queues: HashMap::new(),
            pending_queue_batches: HashMap::new(),
            album_queue_serial: 0,
            last_album_queue: None,
            rootlist: Vec::new(),
            rootlist_cache: session.rootlist.clone(),
            editable_by_grant: std::collections::BTreeSet::new(),
            pending_link: None,
            copied_songs: Vec::new(),
            pending_pastes: Vec::new(),
            collapsed_folders: session.collapsed_folders.clone(),
            update: None,
            last_update_check: None,
            update_checking: false,
            show_update: false,
            update_download: crate::updates::DownloadState::Idle,
            update_source: crate::updates::Source::default(),
            update_support: None,
            update_restart_arguments: Vec::new(),
            update_receipt: None,
            winamp: crate::winamp::WinampState::new(session.winamp_pos, tap, eq),
        };
        app.local.volume = app.settings.volume;
        // What was played here is on disk and needs nothing from the
        // network, so the tab has rows before Spotify has answered.
        app.rebuild_recents();
        app
    }

    /// Watches the queue control clients fill and keeps the snapshots they
    /// read -- now playing, and the device list -- fresh.
    pub fn set_remote_control(&mut self, guard: &crate::single_instance::Guard) {
        self.control_commands = Some(guard.commands());
        self.control_now_playing = Some(guard.now_playing_slot());
        self.control_devices = Some(guard.devices_slot());
    }

    /// Per-window setup: fonts, icons, loaders, theme. Called every time a
    /// window is (re)created around this long-lived application state.
    pub fn attach(&mut self, ctx: &egui::Context) {
        theme::install(ctx);
        ctx.add_bytes_loader(std::sync::Arc::new(self.backend.art().clone()));
        ctx.set_theme(self.theme_preference());
        self.applied_dark = None;
        self.winamp.forget_textures();
        self.window_hidden = false;
        self.hide_intent = false;
        self.wants_show = false;
        self.switch_intent = false;
        self.winamp_level_reassert = 0;
        // A new window starts titled "Spotifast"; name the playing song
        // again rather than trust what the replaced window was told.
        self.window_title.clear();
        if let Some(tray) = &mut self.tray {
            tray.attach();
        }
        // eframe restores the full screen fullscreen lyrics left behind when
        // the app closed in them, without the lyrics. The mode they came
        // from is known, or on Windows, where only fullscreen lyrics make
        // the frameless main window full screen and nothing else could
        // leave it, it was an ordinary window.
        let lyrics_left = self.session_lyrics_fullscreen_from.take();
        if self.settings.winamp_window {
            // The mini player sizes itself; the big window's geometry
            // waits here for its return. eframe may have restored the big
            // window's fullscreen/maximized state before creating this one.
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
            if let Some(pos) = self.winamp.restore_pos
                && crate::window::can_restore(pos, ctx.pixels_per_point())
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                    pos[0], pos[1],
                )));
            }
            // Re-assert the on-top level over the
            // first frames, once the window is mapped, because the level set
            // at creation does not stick on X11.
            if self.settings.winamp_on_top && self.window_level_supported {
                self.winamp_level_reassert = 3;
            }
            return;
        }
        let restored_full_screen = ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
        let lyrics_left = lyrics_left.or_else(|| {
            (cfg!(windows) && restored_full_screen).then(crate::settings::WindowMode::default)
        });
        if let Some(mode) = lyrics_left
            && restored_full_screen
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(mode.fullscreen));
            if mode.maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
        }
        // The session's geometry describes an ordinary window; applying it to
        // one eframe restored maximized or full screen would restore it down.
        let filling_the_screen = match lyrics_left {
            Some(mode) if restored_full_screen => mode.fullscreen || mode.maximized,
            _ => ctx.input(|input| crate::window::fills_the_screen(input.viewport())),
        };
        if let Some(size) = self.session_window_size.take()
            && !filling_the_screen
            && !self.offline
        {
            // Clamp to a sane range so a stale session never creates an
            // unusable window; the OS will further clamp to the monitor.
            if (400.0..=3000.0).contains(&size[0]) && (300.0..=2000.0).contains(&size[1]) {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    size[0], size[1],
                )));
            }
        }
        // If the saved position is off-screen, leave the window where eframe put it.
        if let Some(pos) = self.session_window_pos.take()
            && !filling_the_screen
            && !self.offline
            && crate::window::can_restore(pos, ctx.pixels_per_point())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                pos[0], pos[1],
            )));
        }
        // egui's consensus wheel speed is 40 points per line, about a third
        // of what every other player scrolls per notch; trackpads report
        // pixels and are unaffected (#32).
        ctx.options_mut(|options| options.input_options.line_scroll_speed = 120.0);
    }

    /// The window is gone but the process stays: audio, the tray, and the
    /// media controls keep running until Show or Quit.
    pub fn window_gone(&mut self) {
        // The Winamp window went with it; it comes back where it was.
        self.winamp.remember_position();
        self.winamp.forget_textures();
        self.window_hidden = true;
        self.hide_intent = false;
        self.wants_show = false;
        if let Some(tray) = &mut self.tray {
            tray.hidden();
        }
    }

    /// Whether closing the window keeps the app in the tray rather than
    /// quitting.
    pub fn hides_to_tray(&self) -> bool {
        self.tray.is_some() && self.settings.keep_playing_in_background
    }

    // ---- derived state -----------------------------------------------------

    pub fn page(&self) -> &Page {
        &self.history[self.history_index]
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.auth, AuthStatus::Connected { .. })
    }

    pub fn user_id(&self) -> Option<&str> {
        self.user.as_ref().map(|user| user.id.as_str())
    }

    /// The library list's entry for a playlist, when it holds one.
    fn library_entry(&self, id: &str) -> Option<&Playlist> {
        self.library
            .playlists
            .get()?
            .iter()
            .find(|playlist| playlist.id == id)
    }

    /// The signed-in account's display name, when `owner` is that account.
    fn own_name(&self, owner: Option<&str>) -> Option<String> {
        self.user
            .as_ref()
            .filter(|user| Some(user.id.as_str()) == owner)?
            .display_name
            .clone()
    }

    pub fn is_saved(&self, uri: &str) -> Option<bool> {
        let exact = self.saved.get(uri).copied();
        if exact == Some(true)
            || self
                .track_recordings
                .get(uri)
                .is_some_and(|key| self.saved_recordings.contains(key))
        {
            Some(true)
        } else {
            exact
        }
    }

    /// Album, Single, EP, Compilation or Appears On, in the interface language.
    pub(crate) fn album_kind_label(&self, album: &Album) -> Cow<'static, str> {
        if album.is_single_release() && self.confirmed_ep_albums.contains(&album.uri) {
            Cow::Borrowed("EP")
        } else {
            album.kind_label(self.locale)
        }
    }

    fn remember_track_recording(&mut self, track: &Track) {
        let Some(key) = track.recording_key() else {
            return;
        };
        self.track_recordings.insert(track.uri.clone(), key.clone());
        if let Some(linked) = &track.linked_from
            && !linked.uri.is_empty()
        {
            self.track_recordings
                .insert(linked.uri.clone(), key.clone());
        }
        if self.saved.iter().any(|(uri, saved)| {
            *saved
                && self
                    .track_recordings
                    .get(uri)
                    .is_some_and(|held| held == &key)
        }) {
            self.saved_recordings.insert(key);
        }
    }

    fn set_saved_state(&mut self, uri: String, saved: bool) {
        self.saved.insert(uri.clone(), saved);
        let Some(key) = self.track_recordings.get(&uri).cloned() else {
            return;
        };
        if saved {
            self.saved_recordings.insert(key);
        } else if !self.saved.iter().any(|(candidate, saved)| {
            *saved
                && self
                    .track_recordings
                    .get(candidate)
                    .is_some_and(|held| held == &key)
        }) {
            self.saved_recordings.remove(&key);
        }
    }

    fn saved_toggle_targets(&self, uri: &str) -> Vec<String> {
        let Some(key) = self.track_recordings.get(uri) else {
            return vec![uri.to_string()];
        };
        let equivalents: Vec<String> = self
            .saved
            .iter()
            .filter(|(candidate, saved)| {
                **saved
                    && self
                        .track_recordings
                        .get(*candidate)
                        .is_some_and(|held| held == key)
            })
            .map(|(candidate, _)| candidate.clone())
            .collect();
        if equivalents.is_empty() {
            vec![uri.to_string()]
        } else {
            equivalents
        }
    }

    fn remote_fresh(&self) -> Option<&RemoteSnapshot> {
        self.remote
            .as_ref()
            .filter(|remote| remote.received_at.elapsed() < REMOTE_FRESH)
    }

    /// Where playback commands go: this computer's player or a remote device.
    pub fn target(&self) -> Target {
        if self.local_ready && self.local.is_active() {
            return Target::Local;
        }
        if let Some(selected) = &self.selected_device
            && Some(selected.as_str()) != self.local_device_id.as_deref()
        {
            return Target::Remote(Some(selected.clone()));
        }
        if let Some(remote) = self.remote_fresh() {
            let device = remote.state.device.as_ref();
            let is_local = device
                .and_then(|device| device.id.as_deref())
                .is_some_and(|id| Some(id) == self.local_device_id.as_deref());
            if !is_local && (remote.state.is_playing || remote.state.item.is_some()) {
                return Target::Remote(device.and_then(|device| device.id.clone()));
            }
        }
        if self.local_ready {
            Target::Local
        } else {
            Target::Remote(None)
        }
    }

    /// Context shown as playing, including pending local play requests.
    pub fn playing_context_uri(&self) -> Option<String> {
        let remote = self
            .remote
            .as_ref()
            .and_then(|remote| remote.state.context.as_ref())
            .map(|context| context.uri.clone());
        if let Some(assumed) = &self.assumed_context {
            let held = assumed.at.elapsed() < ASSUMED_CONTEXT_HOLD;
            // A filtered or sorted context plays as plain tracks and will not
            // report a context URI. Keep the assumed URI unless contradicted
            // by a poll taken since the request: one from before it still
            // tells the story from before, whatever device it describes.
            let contradicted = self.remote.as_ref().is_some_and(|snapshot| {
                snapshot.received_at > assumed.at
                    && snapshot
                        .state
                        .context
                        .as_ref()
                        .is_some_and(|context| context.uri != assumed.uri)
            });
            if held || (!contradicted && self.believed_playing()) {
                return (!assumed.uri.is_empty()).then(|| assumed.uri.clone());
            }
        }
        remote
    }

    /// Track shown as current, including a recent unconfirmed play request.
    pub fn current_track_uri(&self) -> Option<String> {
        if let Some(intent) = &self.intent_track
            && (intent.at.elapsed() < PLAYBACK_HOLD || self.queued_play.is_some())
        {
            return Some(intent.uri.clone());
        }
        self.now_playing().map(|now| now.uri)
    }

    /// Shows a user-requested track until the responsible playback surface
    /// has had a chance to report what really started.
    fn expect_track(&mut self, uri: String, position_ms: u32) {
        let confirmation = match self.target() {
            Target::Local => TrackConfirmation::Local,
            Target::Remote(_) => TrackConfirmation::Remote {
                after_poll: self.remote_poll_seq,
                mismatches: 0,
            },
        };
        self.intent_track = Some(TrackIntent {
            uri,
            position_ms,
            at: Instant::now(),
            confirmation,
        });
    }

    /// The local engine is the playback authority. A real track change wins
    /// even when the queue's optimistic guess named a different track.
    fn reconcile_local_track_intent(&mut self, track_changed: bool, reported: Option<&str>) {
        let settled = self.intent_track.as_ref().is_some_and(|intent| {
            matches!(intent.confirmation, TrackConfirmation::Local)
                && (track_changed || reported == Some(intent.uri.as_str()))
        });
        if settled {
            self.intent_track = None;
        }
    }

    /// A poll issued before the command cannot settle its intent. Spotify's
    /// first later answer may still describe the old track, so a mismatch is
    /// held and checked once more. Agreement, or two later mismatches, settles
    /// on the reported state.
    fn reconcile_remote_track_intent(&mut self, poll: u64, reported: Option<&str>) {
        let mut settled = false;
        let mut recheck = false;
        if let Some(intent) = &mut self.intent_track
            && let TrackConfirmation::Remote {
                after_poll,
                mismatches,
            } = &mut intent.confirmation
            && poll > *after_poll
        {
            if reported == Some(intent.uri.as_str()) || *mismatches > 0 {
                settled = true;
            } else {
                *after_poll = poll;
                *mismatches += 1;
                recheck = true;
            }
        }
        if settled {
            self.intent_track = None;
        } else if recheck {
            self.remote_recheck_at = Some(Instant::now() + REMOTE_RECHECK);
        }
    }

    /// Playback state shown by the UI, including a recent local request.
    pub fn believed_playing(&self) -> bool {
        if let Some((playing, at)) = self.optimistic_playing
            && at.elapsed() < PLAYBACK_HOLD
        {
            return playing;
        }
        self.now_playing().is_some_and(|now| now.playing)
    }

    pub fn playing_context_shuffle(&self) -> bool {
        self.shuffle_wanted
    }

    /// Current item for menus, using cached track details when available.
    pub fn now_playing_item(&self) -> Option<PlayableItem> {
        let now = self.now_playing()?;
        if now.is_episode {
            return None;
        }
        if let Some(track) = now.id.as_deref().and_then(|id| self.track_cache.get(id)) {
            return Some(PlayableItem::Track(track.clone()));
        }
        Some(PlayableItem::Track(Track {
            id: now.id.clone(),
            uri: now.uri.clone(),
            name: now.title.clone(),
            artists: now.artists.clone(),
            duration_ms: now.duration_ms,
            ..Track::default()
        }))
    }

    pub fn now_playing(&self) -> Option<NowPlaying> {
        if let Some(now) = &self.frame_now {
            return Some(now.clone());
        }
        self.requested_track_preview()
            .or_else(|| self.now_playing_live())
            .or_else(|| self.resume_preview())
    }

    fn refresh_frame_now(&mut self) {
        self.frame_now = self
            .requested_track_preview()
            .or_else(|| self.now_playing_live())
            .or_else(|| self.resume_preview());
    }

    /// Keep the player bar on the same requested song as the playlist marker
    /// while the responsible playback surface catches up.
    fn requested_track_preview(&self) -> Option<NowPlaying> {
        let intent = self.intent_track.as_ref()?;
        if intent.at.elapsed() >= PLAYBACK_HOLD && self.queued_play.is_none() {
            return None;
        }
        let mut now = self.cached_track_preview(&intent.uri, intent.position_ms)?;
        let actual = self.now_playing_live();
        now.local = matches!(intent.confirmation, TrackConfirmation::Local);
        now.playing = match self.optimistic_playing {
            Some((playing, at)) if at.elapsed() < PLAYBACK_HOLD => playing,
            _ => self.queued_play.is_some() || actual.as_ref().is_some_and(|now| now.playing),
        };
        now.loading = true;
        now.resuming = false;
        if let Some(actual) = actual {
            now.device_name = actual.device_name;
            now.repeat = actual.repeat;
            now.volume_percent = actual.volume_percent;
            now.can_control = actual.can_control;
        }
        Some(now)
    }

    /// What a device is actually playing, here or elsewhere.
    fn now_playing_live(&self) -> Option<NowPlaying> {
        if self.local.is_active() && self.local_transfer_sequence != Some(self.local.track_sequence)
        {
            let track = self.local.track.as_ref()?;
            let cached = track
                .uri
                .rsplit(':')
                .next()
                .and_then(|id| self.track_cache.get(id));
            // Playback already carries artist IDs. Keep those links available
            // while the Web API is loading (or its cached credits have no IDs).
            let artists = if track.artists.iter().any(|artist| artist.id.is_some()) {
                track.artists.clone()
            } else {
                cached
                    .map(|cached| cached.artists.clone())
                    .unwrap_or_else(|| track.artists.clone())
            };
            let playing = match self.optimistic_playing {
                Some((playing, at)) if at.elapsed() < PLAYBACK_HOLD => playing,
                _ => self.local.playback == Playback::Playing,
            };
            return Some(NowPlaying {
                local: true,
                device_name: None,
                uri: track.uri.clone(),
                id: util::uri_id(&track.uri).map(str::to_string),
                title: track.title.clone(),
                subtitle: track.artist_names(),
                artists,
                album_name: track.album.clone(),
                album_id: cached
                    .and_then(|cached| cached.album.as_ref())
                    .map(|album| album.id.clone()),
                show_id: None,
                art_url: track.art_url.clone(),
                art_small: track
                    .art_small_url
                    .clone()
                    .or_else(|| track.art_url.clone()),
                duration_ms: track.duration_ms,
                position_ms: self.local.position_now(),
                playing,
                loading: self.local.playback == Playback::Loading,
                shuffle: self.shuffle_wanted,
                repeat: self.local.repeat,
                volume_percent: volume_to_percent(self.local.volume),
                can_control: true,
                is_episode: track.is_episode,
                resuming: false,
            });
        }
        let remote = self.remote_fresh()?;
        // Ignore a remote snapshot for this device when the local engine has
        // no track. The snapshot may predate a local stop.
        if self.local_device_id.is_some()
            && remote
                .state
                .device
                .as_ref()
                .and_then(|device| device.id.as_deref())
                == self.local_device_id.as_deref()
        {
            return None;
        }
        let item = remote.state.item.as_ref()?;
        let device = remote.state.device.as_ref();
        let playing = match self.optimistic_playing {
            Some((playing, at)) if at.elapsed() < PLAYBACK_HOLD => playing,
            _ => remote.state.is_playing,
        };
        let position = match self.pending_remote_position {
            Some((position, at)) if at.elapsed() < OPTIMISTIC_HOLD => position,
            _ => {
                let base = remote.state.progress_ms.unwrap_or(0);
                if remote.state.is_playing {
                    (base as u64 + remote.received_at.elapsed().as_millis() as u64)
                        .min(item.duration_ms() as u64) as u32
                } else {
                    base
                }
            }
        };
        let volume = match self.pending_remote_volume {
            Some((volume, at)) if at.elapsed() < OPTIMISTIC_HOLD => volume,
            _ => device
                .and_then(|device| device.volume_percent)
                .unwrap_or(50),
        };
        let (artists, album_name, album_id, show_id, is_episode) = match item {
            PlayableItem::Track(track) => (
                track.artists.clone(),
                track
                    .album
                    .as_ref()
                    .map(|album| album.name.clone())
                    .unwrap_or_default(),
                track.album.as_ref().map(|album| album.id.clone()),
                None,
                false,
            ),
            PlayableItem::Episode(episode) => (
                Vec::new(),
                episode
                    .show
                    .as_ref()
                    .map(|show| show.name.clone())
                    .unwrap_or_default(),
                None,
                episode.show.as_ref().map(|show| show.id.clone()),
                true,
            ),
        };
        Some(NowPlaying {
            local: false,
            device_name: device.map(|device| device.name.clone()),
            uri: item.uri().to_string(),
            id: item.id().map(str::to_string),
            title: item.name().to_string(),
            subtitle: item.subtitle(),
            artists,
            album_name,
            album_id,
            show_id,
            art_url: item.image(640).map(str::to_string),
            art_small: item.image(64).map(str::to_string),
            duration_ms: item.duration_ms(),
            position_ms: position,
            playing,
            loading: false,
            shuffle: self.shuffle_wanted,
            repeat: RepeatMode::from_api(&remote.state.repeat_state),
            volume_percent: volume,
            can_control: device.is_none_or(|device| !device.is_restricted),
            is_episode,
            resuming: false,
        })
    }

    /// Last session's track, shown paused when no device is playing.
    fn resume_preview(&self) -> Option<NowPlaying> {
        let uri = self.resume_track.as_deref()?;
        self.cached_track_preview(uri, self.resume_position_ms)
    }

    fn cached_track_preview(&self, uri: &str, position_ms: u32) -> Option<NowPlaying> {
        let track = self.track_cache.get(util::uri_id(uri)?)?;
        Some(NowPlaying {
            local: true,
            device_name: None,
            uri: uri.to_string(),
            id: track.id.clone(),
            title: track.name.clone(),
            subtitle: track.artist_names(),
            artists: track.artists.clone(),
            album_name: track
                .album
                .as_ref()
                .map(|album| album.name.clone())
                .unwrap_or_default(),
            album_id: track.album.as_ref().map(|album| album.id.clone()),
            show_id: None,
            art_url: track.image(640).map(str::to_string),
            art_small: track.image(64).map(str::to_string),
            duration_ms: track.duration_ms,
            position_ms: position_ms.min(track.duration_ms),
            playing: false,
            loading: false,
            shuffle: self.shuffle_wanted,
            repeat: RepeatMode::Off,
            volume_percent: volume_to_percent(self.local.volume),
            can_control: true,
            is_episode: false,
            resuming: true,
        })
    }

    /// The play request for `key` (a context or track URI) is still waiting
    /// for Spotify to react.
    pub fn play_pending(&self, key: &str) -> bool {
        self.pending_fresh() && self.pending_play_keys.iter().any(|k| k == key)
    }

    pub fn set_user_name(&mut self, id: String, name: Option<String>) {
        if self.user_names.get(&id) != Some(&name) {
            self.user_names.insert(id, name);
            self.user_names_revision = self.user_names_revision.wrapping_add(1);
        }
    }

    pub fn any_play_pending(&self) -> bool {
        self.pending_fresh() && !self.pending_play_keys.is_empty()
    }

    fn pending_fresh(&self) -> bool {
        // A request queued behind a connecting engine stays pending for as
        // long as the engine may take; an ordinary request times out fast.
        self.queued_play.is_some()
            || self
                .pending_play_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(8))
    }

    fn set_play_pending(&mut self, keys: Vec<String>) {
        self.pending_play_keys = keys;
        self.pending_play_at = Some(Instant::now());
    }

    fn clear_play_pending(&mut self) {
        self.pending_play_keys.clear();
        self.pending_play_at = None;
    }

    /// The colour to tint the interface with, from the playing art.
    pub fn now_playing_tint(&self) -> Option<Color32> {
        if !self.settings.accent_from_art {
            return None;
        }
        let now = self.now_playing()?;
        let url = now.art_small.or(now.art_url)?;
        self.accents.get(&url).copied()
    }

    pub fn tint_for(&mut self, url: Option<&str>) -> Option<Color32> {
        let url = url?;
        if let Some(color) = self.accents.get(url) {
            return Some(*color);
        }
        if self.accent_pending.insert(url.to_string()) {
            self.backend.send(Command::Accent {
                url: url.to_string(),
            });
        }
        None
    }

    pub fn known_playlist(&self, id: &str) -> Option<&Playlist> {
        self.library
            .playlists
            .get()
            .and_then(|playlists| playlists.iter().find(|playlist| playlist.id == id))
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.playlists.as_ref())
                    .and_then(|playlists| playlists.items.iter().find(|playlist| playlist.id == id))
            })
            .or_else(|| {
                self.home.discover.values().find_map(|playlists| {
                    playlists
                        .get()
                        .and_then(|playlists| playlists.iter().find(|playlist| playlist.id == id))
                })
            })
    }

    pub fn known_album(&self, id: &str) -> Option<&Album> {
        self.library
            .albums
            .items
            .iter()
            .find(|saved| saved.album.id == id)
            .map(|saved| &saved.album)
            .or_else(|| {
                let results = self.search.results.get()?;
                results
                    .albums
                    .as_ref()
                    .and_then(|albums| albums.items.iter().find(|album| album.id == id))
                    .or_else(|| {
                        results.tracks.as_ref().and_then(|tracks| {
                            tracks
                                .items
                                .iter()
                                .filter_map(|track| track.album.as_ref())
                                .find(|album| album.id == id)
                        })
                    })
            })
            .or_else(|| {
                self.library
                    .liked
                    .items
                    .iter()
                    .filter_map(|saved| saved.track.album.as_ref())
                    .find(|album| album.id == id)
            })
            .or_else(|| {
                self.playlist_pages.values().find_map(|page| {
                    page.items
                        .items
                        .iter()
                        .find_map(|item| match item.playable() {
                            Some(PlayableItem::Track(track)) => {
                                track.album.as_ref().filter(|album| album.id == id)
                            }
                            _ => None,
                        })
                })
            })
            .or_else(|| {
                self.artist_pages.values().find_map(|page| {
                    page.albums
                        .values()
                        .find_map(|albums| albums.items.iter().find(|album| album.id == id))
                })
            })
            .or_else(|| {
                [
                    &self.home.top_tracks,
                    &self.home.top_songs,
                    &self.home.recommendations,
                ]
                .into_iter()
                .find_map(|tracks| {
                    tracks.get().and_then(|tracks| {
                        tracks
                            .iter()
                            .filter_map(|track| track.album.as_ref())
                            .find(|album| album.id == id)
                    })
                })
            })
    }

    pub fn known_artist(&self, id: &str) -> Option<&Artist> {
        self.library
            .artists
            .items
            .iter()
            .find(|artist| artist.id == id)
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.artists.as_ref())
                    .and_then(|artists| artists.items.iter().find(|artist| artist.id == id))
            })
            .or_else(|| {
                self.home
                    .top_artists
                    .get()
                    .and_then(|artists| artists.iter().find(|artist| artist.id == id))
            })
            .or_else(|| {
                self.artist_pages.values().find_map(|page| {
                    page.related
                        .get()
                        .and_then(|artists| artists.iter().find(|artist| artist.id == id))
                })
            })
    }

    pub fn known_show(&self, id: &str) -> Option<&Show> {
        self.library
            .shows
            .items
            .iter()
            .find(|saved| saved.show.id == id)
            .map(|saved| &saved.show)
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.shows.as_ref())
                    .and_then(|shows| shows.items.iter().find(|show| show.id == id))
            })
            .or_else(|| {
                self.library
                    .episodes
                    .items
                    .iter()
                    .filter_map(|saved| saved.episode.show.as_ref())
                    .find(|show| show.id == id)
            })
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.episodes.as_ref())
                    .and_then(|episodes| {
                        episodes
                            .items
                            .iter()
                            .filter_map(|episode| episode.show.as_ref())
                            .find(|show| show.id == id)
                    })
            })
    }

    // ---- frame ---------------------------------------------------------------

    fn handle_events(&mut self) {
        let events = self.backend.poll();
        self.handle_backend_events(events);
    }

    fn handle_backend_events(&mut self, events: Vec<Event>) {
        for event in events {
            if self.offline
                && (matches!(self.update_source, crate::updates::Source::GitHub)
                    || !matches!(
                        &event,
                        Event::UpdateChecked { .. }
                            | Event::UpdateSupport(_)
                            | Event::UpdateProgress { .. }
                            | Event::UpdateDownloaded(_)
                            | Event::UpdateInstalling(_)
                    ))
            {
                continue;
            }
            match event {
                Event::PlaylistCoverChecked {
                    id,
                    request,
                    images,
                    result,
                } => {
                    self.cover_checked(&id, request, images, result);
                }
                Event::PlaylistCoverChosen {
                    id,
                    request,
                    result,
                } => {
                    self.cover_chosen(&id, request, result);
                }
                Event::Auth(status) => self.handle_auth(status),
                Event::Playback(status) => self.handle_playback(status),
                Event::Receivers(receivers) => self.receivers = receivers,
                Event::ReceiverActivated { name, result } => {
                    self.activating_receiver = None;
                    match result {
                        Ok(()) => {
                            self.toast(
                                // Translators: {name} is the name of a speaker or other playback device.
                                gettext(self.locale, "{name} is ready").replace("{name}", &name),
                            );
                            // It takes a moment to appear in the device list.
                            self.pending_transfer_to = Some((name, Instant::now()));
                            self.devices_fetched_at = None;
                            self.refresh_devices();
                        }
                        Err(error) => self.toast_error(format!("{name}: {error}")),
                    }
                }
                Event::Local(state) => self.handle_local(*state),
                Event::Api(response) => self.handle_api(*response),
                Event::Accent { url, color } => {
                    self.accent_pending.remove(&url);
                    let tint = self.palette.tint_from_art(color);
                    self.accents.insert(url, tint);
                }
                Event::ProxyRestored { config, password } => {
                    self.handle_proxy_restored(config, password)
                }
                Event::ProxyPasswordStored => self.handle_proxy_password_stored(),
                Event::ProxyStorageFailed(error) => {
                    let message = if self.settings.proxy_password_legacy {
                        gettext(
                            self.locale,
                            // Translators: {error} is a sentence saying why the proxy password could not be stored.
                            "{error} The original settings file is kept intact; changed preferences are not saved yet.",
                        )
                        .replace("{error}", error.proxy_message())
                    } else {
                        error.proxy_message().to_string()
                    };
                    self.toast_error(message);
                }
                Event::ProxyApplied {
                    request,
                    config,
                    result,
                } => self.handle_proxy_applied(request, config, result),
                Event::Error(message) => self.toast_error(message),
                Event::Rootlist { result } => match result {
                    Ok(rootlist) => {
                        let account_id = self.user_id().map(str::to_owned).or_else(|| {
                            if let AuthStatus::Connected { username } = &self.auth {
                                Some(username.clone())
                            } else {
                                None
                            }
                        });
                        self.rootlist = rootlist.entries;
                        self.editable_by_grant = rootlist.editable;
                        if let Some(account_id) = account_id {
                            self.rootlist_cache = Some(CachedRootlist {
                                account_id,
                                entries: self.rootlist.clone(),
                            });
                            self.session_dirty = true;
                        }
                    }
                    Err(error) => log::warn!("rootlist unavailable: {error}"),
                },
                Event::Lyrics { uri, result } => {
                    if self.lyrics_uri.as_deref() == Some(uri.as_str()) {
                        self.lyrics = match result {
                            Ok(found) => Loadable::Loaded(found),
                            Err(error) => Loadable::Failed(error),
                        };
                    }
                }
                Event::PlaylistCache {
                    account_id,
                    id,
                    generation,
                    cache,
                } => {
                    self.receive_playlist_cache(&account_id, &id, generation, cache);
                }
                Event::PlaylistCacheStored {
                    account_id,
                    id,
                    generation,
                    snapshot,
                    success,
                } => {
                    self.receive_playlist_cache_stored(
                        &account_id,
                        &id,
                        generation,
                        &snapshot,
                        success,
                    );
                }
                Event::LikedSongsCache {
                    account_id,
                    generation,
                    cache,
                } => {
                    self.receive_liked_cache(&account_id, generation, cache);
                }
                Event::UserName { id, name } => {
                    self.set_user_name(id, name);
                }
                Event::AudiobookShows(uris) => {
                    self.audiobook_shows.extend(uris);
                }
                Event::Radio {
                    seed,
                    generation,
                    result,
                } => self.receive_radio(&seed, generation, result),
                Event::AlbumType { uri, result } => match result {
                    Ok(true) => {
                        self.confirmed_ep_albums.insert(uri);
                    }
                    Ok(false) => {}
                    Err(error) => log::debug!("album type unavailable for {uri}: {error}"),
                },
                Event::WebApp { client_id } => self.web_app = client_id,
                Event::UpdateSupport(result) => {
                    if result.is_ok()
                        && self.settings.download_updates_automatically
                        && matches!(self.update_download, crate::updates::DownloadState::Idle)
                    {
                        self.actions.push(Action::DownloadUpdate);
                    }
                    self.update_support = Some(result);
                }
                Event::UpdateProgress { received, total } => {
                    self.update_download =
                        crate::updates::DownloadState::Downloading { received, total };
                }
                Event::UpdateDownloaded(result) => {
                    self.update_download = match result {
                        Ok(prepared) => crate::updates::DownloadState::Ready(prepared),
                        Err(error) => crate::updates::DownloadState::Failed(error),
                    };
                }
                Event::UpdateInstalling(result) => match result {
                    Ok(()) => self.actions.push(Action::Quit),
                    Err(error) => {
                        self.update_download = crate::updates::DownloadState::Failed(error)
                    }
                },
                Event::UpdateChecked { manual, result } => {
                    self.update_checking = false;
                    match result {
                        Ok(Some(notice)) => {
                            if manual || self.update.as_ref() != Some(&notice) {
                                self.toast(
                                    // Translators: {version} is a version number such as 1.4.0.
                                    gettext(self.locale, "Spotifast {version} is available")
                                        .replace("{version}", &notice.version.to_string()),
                                );
                            }
                            self.update = Some(notice);
                            if self.settings.download_updates_automatically
                                && matches!(
                                    self.update_download,
                                    crate::updates::DownloadState::Idle
                                )
                            {
                                self.backend.send(Command::InspectUpdate);
                            }
                        }
                        Ok(None) => {
                            self.update = None;
                            if manual {
                                self.toast(gettext(self.locale, "Spotifast is up to date"));
                            } else {
                                log::debug!("this is the newest release");
                            }
                        }
                        Err(error) if manual => {
                            self.toast_error(
                                // Translators: {error} is an error message.
                                gettext(self.locale, "Couldn't check for updates: {error}")
                                    .replace("{error}", &error.to_string()),
                            );
                        }
                        Err(error) => {
                            log::debug!("could not check for a newer release: {error}");
                        }
                    }
                }
            }
        }
    }

    fn handle_auth(&mut self, status: AuthStatus) {
        match &status {
            AuthStatus::Connected { .. } => {
                self.sign_in_url = None;
                self.reset_data();
                self.load_playlists();
                self.ensure_loaded(self.page().clone());
                self.poll_remote(true);
            }
            AuthStatus::WaitingForBrowser { url } => self.sign_in_url = Some(url.clone()),
            AuthStatus::SignedOut => {
                if matches!(self.dialog, Some(Dialog::PersonalAppIntro)) {
                    self.dialog = None;
                }
                self.uploaded_covers.clear();
                self.sign_in_url = None;
                self.web_app = None;
                self.user = None;
                self.premium_notice_shown = false;
                self.local = LocalState::default();
                self.local_ready = false;
                self.local_device_id = None;
                self.local_playback = LocalPlayback::Unavailable;
                self.remote = None;
                self.rootlist.clear();
                self.rootlist_cache = None;
                self.editable_by_grant.clear();
                self.session_dirty = true;
                self.reset_data();
            }
            AuthStatus::Failed(message) => {
                self.sign_in_url = None;
                self.toast_error(message.clone());
            }
            _ => {}
        }
        self.auth = status;
    }

    fn handle_playback(&mut self, status: LocalPlayback) {
        match &status {
            LocalPlayback::Ready { device_id } => {
                self.local_device_id = Some(device_id.clone());
                self.local_ready = true;
                if let Some(request) = self.queued_play.take() {
                    self.play_request(request, false);
                }
            }
            LocalPlayback::Unavailable => {
                self.local_ready = false;
                self.local_device_id = None;
            }
            LocalPlayback::Failed(message) => {
                self.local_ready = false;
                if self.queued_play.take().is_some() {
                    self.clear_play_pending();
                    self.intent_track = None;
                }
                self.toast_error(
                    // Translators: {error} is an error message from the playback engine.
                    gettext(self.locale, "Local playback: {error}").replace("{error}", message),
                );
            }
            LocalPlayback::Authorizing | LocalPlayback::Connecting => {}
        }
        self.local_playback = status;
    }

    fn reset_data(&mut self) {
        self.local_transfer_sequence = None;
        self.queue_start_pending = None;
        self.pending_album_queues.clear();
        self.pending_queue_batches.clear();
        self.last_album_queue = None;
        self.cover_uploads.clear();
        self.uploaded_covers.clear();
        // Pages still on their way belong to the signed-out account's load.
        self.library = Library {
            playlists_generation: self.library.playlists_generation,
            ..Library::default()
        };
        self.liked_songs = crate::liked::LikedSongs::default();
        self.liked_recheck_at = None;
        self.home = HomeData::default();
        self.playlist_pages.clear();
        self.album_pages.clear();
        self.artist_pages.clear();
        self.show_pages.clear();
        self.album_types_requested.clear();
        self.audiobook_shows.clear();
        self.audiobooks_requested.clear();
        self.confirmed_ep_albums.clear();
        self.saved.clear();
        self.saved_pending.clear();
        self.track_recordings.clear();
        self.playlist_availability.clear();
        self.saved_recordings.clear();
        self.saved_writes.clear();
        self.queue = Loadable::NotLoaded;
        self.queue_shuffle_pending = None;
        self.queue_reorder_pending = None;
        self.devices.clear();
        self.control_devices_stale = true;
        self.devices_fetched_at = None;
        self.search.results = Loadable::NotLoaded;
        self.search.committed.clear();
        self.search.playlists = None;
        self.search.serial += 1;
        self.search.catalogue_pending = false;
        self.search.playlists_pending = false;
        self.search.error = None;
        self.table_rows.clear();
        self.page_used.clear();
        self.track_used.clear();
        self.copied_songs.clear();
        self.pending_pastes.clear();
    }

    /// Drop table-row caches whose pages are gone, and cap what remains.
    pub fn retain_table_rows(&mut self, current: &Page) {
        const MAX_TABLE_ROW_CACHES: usize = 2;
        self.table_rows.retain(|page, _| {
            page == current
                || match page {
                    Page::Playlist(id) => self.playlist_pages.contains_key(id),
                    Page::Album(id) => self.album_pages.contains_key(id),
                    Page::LikedSongs | Page::TopSongs => true,
                    _ => false,
                }
        });
        if self.table_rows.len() <= MAX_TABLE_ROW_CACHES {
            return;
        }
        let mut keep = HashSet::from([current.clone()]);
        if let Some(page) = self.history.get(self.history_index) {
            keep.insert(page.clone());
        }
        if self.history_index > 0
            && let Some(page) = self.history.get(self.history_index - 1)
        {
            keep.insert(page.clone());
        }
        self.table_rows.retain(|page, _| keep.contains(page));
        while self.table_rows.len() > MAX_TABLE_ROW_CACHES {
            let drop = self
                .table_rows
                .keys()
                .find(|page| *page != current)
                .cloned();
            match drop {
                Some(page) => {
                    self.table_rows.remove(&page);
                }
                None => break,
            }
        }
    }

    pub fn table_rows_retained_bytes(&self) -> usize {
        self.table_rows
            .values()
            .map(TableRowsCache::retained_bytes)
            .sum()
    }

    fn handle_local(&mut self, state: LocalState) {
        if self
            .local_transfer_sequence
            .is_some_and(|sequence| sequence != state.track_sequence)
        {
            self.local_transfer_sequence = None;
        }
        if state.track_sequence != self.local.track_sequence
            && matches!(self.target(), Target::Local)
        {
            self.listening = None;
        }
        let track_changed =
            state.track != self.local.track || state.track_sequence != self.local.track_sequence;
        let reconnected = state.connected && !self.local.connected;
        if state.shuffle != self.local.shuffle
            && self
                .shuffle_set_at
                .is_none_or(|at| at.elapsed() > Duration::from_secs(5))
        {
            // Accept shuffle changes made by another client.
            self.shuffle_wanted = state.shuffle;
        }
        if state.playback != self.local.playback {
            self.optimistic_playing = None;
            if matches!(state.playback, Playback::Playing | Playback::Loading) {
                self.clear_play_pending();
            }
        }
        if state.track != self.local.track {
            self.clear_play_pending();
        }
        self.reconcile_local_track_intent(
            track_changed,
            state.track.as_ref().map(|track| track.uri.as_str()),
        );
        let held_volume = self.held_local_volume(state.volume);
        if held_volume.is_none() && state.volume != self.settings.volume {
            self.settings.volume = state.volume;
            self.settings_dirty = true;
        }
        if state.seek_sequence != self.local.seek_sequence
            && let Some(controls) = &self.media_controls
        {
            controls.seeked(state.position_ms);
        }
        if let Some(error) = &state.error
            && self.local.error.as_deref() != Some(error.as_str())
        {
            self.toast_error(error.clone());
            // One unavailable track is Spotify's catalogue; several in a
            // row is the session's audio-key service gone bad, which
            // leaves librespot feeding the decoder encrypted bytes and
            // skipping through the whole album. A fresh session cures it.
            if error.starts_with("This item isn't available") {
                let now = Instant::now();
                self.unavailable_at
                    .retain(|at| now.duration_since(*at) < Duration::from_secs(20));
                self.unavailable_at.push(now);
                if self.unavailable_at.len() >= 3
                    && self
                        .last_unavailable_reconnect
                        .is_none_or(|at| at.elapsed() > Duration::from_secs(60))
                {
                    self.unavailable_at.clear();
                    self.last_unavailable_reconnect = Some(now);
                    self.backend.send(Command::Reconnect);
                    self.toast(gettext(
                        self.locale,
                        "Spotify audio disconnected. Reconnecting local playback",
                    ));
                }
            }
        }
        if let Some(seed) = autoplay_seed(
            self.local_list.as_deref(),
            self.settings.autoplay,
            &self.local,
            &state,
        ) {
            log::info!("the list ended; playing what Spotify follows {seed} with");
            self.local_list = None;
            self.backend.player(PlayerCommand::Load(LoadSpec {
                context_uri: Some(seed),
                play: true,
                autoplay: true,
                ..LoadSpec::default()
            }));
        }
        self.local = state;
        if let Some(volume) = held_volume {
            self.local.volume = volume;
        }
        if track_changed {
            self.on_now_playing_changed();
        }
        if reconnected {
            if let Some(request) = self.queued_play.take() {
                self.play_request(request, false);
            }
            // Retry names requested before the session connected.
            let unresolved: Vec<String> = self
                .user_names
                .iter()
                .filter(|(_, name)| name.is_none())
                .map(|(id, _)| id.clone())
                .collect();
            for id in &unresolved {
                self.user_names.remove(id);
            }
            self.request_user_names(unresolved);
        }
    }

    /// Loads details for the remembered track before playback starts.
    fn request_resume_track(&mut self) {
        if self.now_playing_live().is_some() {
            return;
        }
        let Some(uri) = self.resume_track.clone() else {
            return;
        };
        // Episodes are not in the track endpoint; the preview skips them.
        if !uri.starts_with("spotify:track:") {
            return;
        }
        let Some(id) = util::uri_id(&uri).map(str::to_string) else {
            return;
        };
        if self.track_cache.contains_key(&id) {
            self.track_used.insert(id, Instant::now());
            return;
        }
        if !self.track_requests.insert(id.clone()) {
            return;
        }
        self.backend.api(ApiRequest::Track { id });
    }

    /// Whether the player bar shows the remembered track without live playback.
    fn resume_only(&self) -> bool {
        self.resume_track.is_some() && self.now_playing_live().is_none()
    }

    /// Loads the remembered context so Previous and Next work before playback.
    fn ensure_resume_context_loaded(&mut self) {
        if !self.resume_only() {
            return;
        }
        let Some(context) = self.resume_context.clone() else {
            return;
        };
        if self.context_track_uris(&context).is_some() {
            return;
        }
        if let Some(page) = Page::decode(&Self::context_page(&context)) {
            self.ensure_loaded(page);
        }
    }

    /// Where the playing songs come from, for the queue's header: the
    /// playlist, album, artist, or podcast with its page, Liked Songs, or
    /// a song radio as plain text because a station has no page. A context
    /// whose name has not loaded is still named by kind so it can open.
    pub fn playing_from(&self) -> Option<PlayingFrom> {
        let context = self.playing_context_uri()?;
        if context.ends_with(":collection") {
            return Some(PlayingFrom {
                name: gettext(self.locale, "Liked Songs").into_owned(),
                page: Some(Page::LikedSongs),
            });
        }
        if context.starts_with("spotify:station:") {
            return Some(PlayingFrom {
                name: self
                    .station_name(&context)
                    .unwrap_or_else(|| gettext(self.locale, "Radio").into_owned()),
                page: util::station_seed(&context).map(Page::Radio),
            });
        }
        let kind = util::uri_kind(&context)?;
        let id = util::uri_id(&context)?.to_string();
        let (name, page) = match kind {
            "playlist" => {
                let name = self
                    .library_entry(&id)
                    .map(|playlist| playlist.name.clone())
                    .or_else(|| {
                        self.playlist_pages
                            .get(&id)
                            .and_then(|page| page.playlist.get())
                            .map(|playlist| playlist.name.clone())
                    });
                (
                    name.unwrap_or_else(|| gettext(self.locale, "Playlist").into_owned()),
                    Page::Playlist(id),
                )
            }
            "album" => {
                let name = self
                    .album_pages
                    .get(&id)
                    .and_then(|page| page.album.get())
                    .map(|album| album.name.clone())
                    .or_else(|| {
                        self.now_playing()
                            .filter(|now| now.album_id.as_deref() == Some(id.as_str()))
                            .map(|now| now.album_name)
                    });
                (
                    name.unwrap_or_else(|| gettext(self.locale, "Album").into_owned()),
                    Page::Album(id),
                )
            }
            "artist" => {
                let name = self
                    .artist_pages
                    .get(&id)
                    .and_then(|page| page.artist.get())
                    .map(|artist| artist.name.clone())
                    .or_else(|| {
                        self.now_playing().and_then(|now| {
                            now.artists
                                .into_iter()
                                .find(|artist| artist.id.as_deref() == Some(id.as_str()))
                                .map(|artist| artist.name)
                        })
                    });
                (
                    name.unwrap_or_else(|| gettext(self.locale, "Artist").into_owned()),
                    Page::Artist(id),
                )
            }
            "show" => {
                let name = self
                    .show_pages
                    .get(&id)
                    .and_then(|page| page.show.get())
                    .map(|show| show.name.clone())
                    .or_else(|| {
                        self.library
                            .shows
                            .items
                            .iter()
                            .find(|saved| saved.show.id == id)
                            .map(|saved| saved.show.name.clone())
                    });
                (
                    name.unwrap_or_else(|| gettext(self.locale, "Podcast").into_owned()),
                    Page::Show(id),
                )
            }
            _ => return None,
        };
        Some(PlayingFrom {
            name,
            page: Some(page),
        })
    }

    /// "<Name> Radio" for a station whose seed's name is known.
    fn station_name(&self, context: &str) -> Option<String> {
        self.radio_name(&util::station_seed(context)?)
    }

    /// Encodes a context as a value accepted by `Page::decode`.
    fn context_page(context_uri: &str) -> String {
        if context_uri.ends_with(":collection") {
            return "liked".to_owned();
        }
        match (util::uri_kind(context_uri), util::uri_id(context_uri)) {
            (Some(kind), Some(id)) => format!("{kind}:{id}"),
            _ => String::new(),
        }
    }

    /// Moves the paused remembered track within its context without playing.
    /// Returns `false` when the context is unavailable.
    fn step_resume(&mut self, forward: bool) -> bool {
        let Some(context) = self.resume_context.clone() else {
            return false;
        };
        let Some(uris) = self.context_track_uris(&context) else {
            return false;
        };
        let current = self.resume_track.clone().unwrap_or_default();
        let next = if self.shuffle_wanted && forward {
            // Preserve shuffle behavior before playback resumes.
            let choices: Vec<&String> = uris.iter().filter(|uri| **uri != current).collect();
            if choices.is_empty() {
                return false;
            }
            choices[rand::random_range(0..choices.len())].clone()
        } else {
            let Some(index) = uris.iter().position(|uri| *uri == current) else {
                return false;
            };
            let last = uris.len() - 1;
            let target = match (forward, index) {
                (true, i) if i == last => 0,
                (true, i) => i + 1,
                (false, 0) => last,
                (false, i) => i - 1,
            };
            uris[target].clone()
        };
        self.cache_track_from_context(&context, &next);
        self.resume_track = Some(next);
        self.resume_position_ms = 0;
        self.session_dirty = true;
        true
    }

    /// Caches track details from a loaded context for immediate display.
    fn cache_track_from_context(&mut self, context_uri: &str, uri: &str) {
        let Some(id) = util::uri_id(uri) else {
            return;
        };
        if self.track_cache.contains_key(id) {
            self.track_used.insert(id.to_owned(), Instant::now());
            return;
        }
        let found = if let Some(pid) = context_uri.strip_prefix("spotify:playlist:") {
            self.playlist_pages.get(pid).and_then(|page| {
                page.items
                    .items
                    .iter()
                    .find_map(|item| match item.playable() {
                        Some(PlayableItem::Track(track)) if track.uri == uri => Some(track.clone()),
                        _ => None,
                    })
            })
        } else if let Some(aid) = context_uri.strip_prefix("spotify:album:") {
            self.album_pages.get(aid).and_then(|page| {
                page.tracks
                    .items
                    .iter()
                    .find(|track| track.uri == uri)
                    .cloned()
            })
        } else if context_uri.ends_with(":collection") {
            self.library
                .liked
                .items
                .iter()
                .find(|item| item.track.uri == uri)
                .map(|item| item.track.clone())
        } else {
            None
        };
        if let Some(track) = found {
            self.remember_track_recording(&track);
            self.track_cache.insert(id.to_owned(), track);
            self.track_used.insert(id.to_owned(), Instant::now());
        }
    }

    /// Older playlist caches predate recording identities. Once the current
    /// track supplies an ISRC, refresh only plausible aliases so Spotify can
    /// confirm which market-specific URI is the same recording.
    fn request_recording_candidates(&mut self, track: &Track) {
        if track.recording_key().is_none() {
            return;
        }
        let mut candidates = HashSet::new();
        for page in self.playlist_pages.values() {
            for item in &page.items.items {
                let Some(PlayableItem::Track(candidate)) = item.playable() else {
                    continue;
                };
                if same_recording_hint(track, candidate)
                    && let Some(id) = candidate.id.as_ref()
                    && self
                        .track_cache
                        .get(id)
                        .is_none_or(|known| known.recording_key().is_none())
                {
                    candidates.insert(id.clone());
                }
            }
        }
        for id in candidates {
            if self.track_requests.insert(id.clone()) {
                self.backend.api(ApiRequest::Track { id });
            }
        }
    }

    fn on_now_playing_changed(&mut self) {
        // Queue consumption and saved session state follow confirmed playback,
        // not the song shown optimistically while a command is still pending.
        let Some(now) = self.now_playing_live() else {
            return;
        };
        let same_uri = self.last_now_playing_uri.as_deref() == Some(now.uri.as_str());
        let new_occurrence =
            now.local && self.last_now_playing_sequence != self.local.track_sequence;
        if same_uri && !new_occurrence {
            return;
        }
        let queue_already_updated =
            self.queue_start_pending.take().as_ref() == Some(&self.target());
        let repeating = same_uri && new_occurrence && now.repeat == RepeatMode::Track;
        // Restore the saved queue only when the remembered track resumes.
        if !self.resume_queue.is_empty() {
            let queued = std::mem::take(&mut self.resume_queue);
            self.session_dirty = true;
            if now.local && self.resume_track.as_deref() == Some(now.uri.as_str()) {
                for uri in queued {
                    self.manual_queue.push(uri.clone());
                    self.backend.api(ApiRequest::AddToQueue {
                        uri,
                        device_id: self.local_device_id.clone(),
                        label: String::new(),
                    });
                }
            }
        }
        // A context load can start the same song as the manual queue's head
        // without consuming that queued copy. Explicit skips have already
        // consumed their rows; only an unrequested advance consumes one here.
        if !queue_already_updated && !repeating {
            self.consume_manual_queue_head(&now.uri);
        }
        if !queue_already_updated
            && !repeating
            && let Loadable::Loaded(queue) = &mut self.queue
        {
            let accounted = queue
                .currently_playing
                .as_ref()
                .is_some_and(|item| item.uri() == now.uri)
                && !(same_uri && new_occurrence);
            if !accounted
                && queue
                    .queue
                    .first()
                    .is_some_and(|item| item.uri() == now.uri)
            {
                let item = queue.queue.remove(0);
                queue.currently_playing = Some(item);
            }
        }
        self.last_now_playing_uri = Some(now.uri.clone());
        if now.local {
            self.last_now_playing_sequence = self.local.track_sequence;
        }
        self.resume_context = self.playing_context_uri();
        self.resume_track = Some(now.uri.clone());
        self.resume_position_ms = 0;
        if now.local
            && !now.is_episode
            && let Some(id) = &now.id
            && !self.track_cache.contains_key(id)
            && self.track_requests.insert(id.clone())
        {
            self.backend.api(ApiRequest::Track { id: id.clone() });
        }
        self.request_contains(vec![now.uri.clone()]);
        if let Some(url) = now.art_small.or(now.art_url) {
            self.tint_for(Some(&url));
        }
        if matches!(self.page(), Page::Queue)
            || self.show_queue_panel
            || (self.settings.winamp_window && self.settings.playlist_open)
        {
            self.refresh_queue(true);
        }
        if self.show_lyrics_panel {
            self.request_lyrics();
        }
    }

    /// Asks for the playing track's lyrics unless they are here or on the
    /// way. Podcasts have no lyrics to ask for.
    pub fn request_lyrics(&mut self) {
        let Some(now) = self.now_playing() else {
            return;
        };
        if self.lyrics_uri.as_deref() == Some(now.uri.as_str())
            && !matches!(self.lyrics, Loadable::NotLoaded | Loadable::Failed(_))
        {
            return;
        }
        self.lyrics_uri = Some(now.uri.clone());
        self.lyrics_following = true;
        self.lyrics_line_shown = None;
        if now.is_episode || self.offline {
            self.lyrics = Loadable::Loaded(None);
            return;
        }
        self.lyrics = Loadable::Loading;
        self.backend.send(Command::Lyrics(Box::new(LyricsRequest {
            uri: now.uri,
            query: crate::lyrics::Query {
                artist: now
                    .artists
                    .first()
                    .map(|artist| artist.name.clone())
                    .unwrap_or_default(),
                title: now.title,
                album: now.album_name,
                duration_ms: now.duration_ms,
            },
        })));
    }

    /// Pushes the Winamp window's always-on-top level to the live window.
    fn push_winamp_level(&self, ctx: &egui::Context) {
        if self.window_level_supported
            && let Some(level) =
                winamp_on_top_level(self.settings.winamp_window, self.settings.winamp_on_top)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        self.poll_custom_themes(ctx);
        let now = Instant::now();
        if self.winamp_level_reassert > 0 {
            self.winamp_level_reassert -= 1;
            self.push_winamp_level(ctx);
            // The window can be idle right after opening, so drive the next
            // frame to make sure the re-assert actually runs.
            if self.winamp_level_reassert > 0 {
                ctx.request_repaint();
            }
        }
        if !self.zoom_applied {
            self.zoom_applied = true;
            let zoom = self.settings.zoom.clamp(0.5, 2.5);
            if (zoom - 1.0).abs() > 0.001 {
                ctx.set_zoom_factor(zoom);
            }
        } else {
            let zoom = ctx.zoom_factor();
            if (zoom - self.settings.zoom).abs() > 0.001 {
                // Ctrl+plus/minus zoomed; remembered for the next start.
                self.settings.zoom = zoom;
                self.settings_dirty = true;
            }
        }
        self.toasts
            .retain(|toast| toast.created.elapsed() < TOAST_LIFETIME);
        self.maybe_suggest_personal_app();

        if self.settings.check_for_updates
            && !self.offline
            && self
                .last_update_check
                .is_none_or(|at| at.elapsed() >= crate::updates::CHECK_INTERVAL)
        {
            self.check_for_updates(false);
        }

        if self.is_connected() && !self.offline {
            self.request_resume_track();
            self.ensure_resume_context_loaded();
        }

        if self.is_connected() && !self.offline {
            let interval = match self.target() {
                Target::Local if self.local.is_active() => REMOTE_POLL_IDLE,
                _ => REMOTE_POLL_ACTIVE,
            };
            if !self.remote_poll_pending && self.remote_polled_at.elapsed() >= interval {
                self.poll_remote(false);
            }
            if let Some(due) = self.remote_recheck_at
                && Instant::now() >= due
            {
                self.remote_recheck_at = None;
                self.poll_remote(true);
            }
            if self.show_devices
                && !self.devices_loading
                && self
                    .devices_fetched_at
                    .is_none_or(|at| at.elapsed() > DEVICES_FRESH)
            {
                self.refresh_devices();
            }
            let playlist_open = self.settings.winamp_window && self.settings.playlist_open;
            if (self.show_queue_panel || matches!(self.page(), Page::Queue) || playlist_open)
                && !self.queue.is_loading()
                && self
                    .queue_fetched_at
                    .is_none_or(|at| at.elapsed() > Duration::from_secs(20))
            {
                self.refresh_queue(false);
            }
            if let Some(due) = self.liked_recheck_at {
                if Instant::now() >= due && !self.liked_songs.refreshing() {
                    self.liked_recheck_at = None;
                    self.refresh_liked_songs();
                } else {
                    ctx.request_repaint_after(
                        due.saturating_duration_since(Instant::now())
                            .max(Duration::from_millis(100)),
                    );
                }
            }
            if let Some(due) = self.queue_recheck_at {
                if Instant::now() >= due {
                    self.queue_recheck_at = None;
                    self.refresh_queue(true);
                } else {
                    ctx.request_repaint_after(
                        (due - Instant::now()).max(Duration::from_millis(50)),
                    );
                }
            }
        }

        if let Some(typed) = self.search.typed_at {
            if typed.elapsed() >= SEARCH_DEBOUNCE {
                self.search.typed_at = None;
                let query = self.search.query.trim().to_string();
                self.run_search(query);
            } else {
                ctx.request_repaint_after(SEARCH_DEBOUNCE - typed.elapsed());
            }
        }

        if self.last_eviction.elapsed() > Duration::from_secs(20) {
            self.last_eviction = now;
            self.backend.art().evict(ctx);
            self.evict_stale_pages();
        }
        self.sync_skin(ctx);
        if self.settings_dirty && self.last_settings_save.elapsed() > Duration::from_secs(2) {
            self.save_settings();
        }
        if self.session_dirty && self.last_session_save.elapsed() > Duration::from_secs(2) {
            self.save_session();
        }
    }

    /// Loads and applies the skin selected in settings.
    /// On failure, restores the active skin setting to avoid repeated retries.
    fn sync_skin(&mut self, ctx: &egui::Context) {
        if self.settings.winamp_window
            && !self.winamp.is_loading()
            && self.winamp.worn != self.settings.skin
        {
            match self.settings.skin.clone() {
                None => self.winamp.wear(None, crate::skin::Skin::builtin()),
                Some(name) => self.winamp.load(name, &self.dirs.skins_dir(), ctx),
            }
        }
        if let Some(loaded) = self.winamp.poll() {
            self.skin_loaded(loaded);
        }
        let fetched = self.winamp.presets.poll();
        if let Some(fetched) = fetched {
            match fetched {
                Ok(count) => {
                    self.toast(
                        ngettext(
                            self.locale,
                            // Translators: {count} is the number of visualizer presets added.
                            "Added {count} MilkDrop preset",
                            "Added {count} MilkDrop presets",
                            count as u32,
                        )
                        .replace("{count}", &count.to_string()),
                    );
                    // Restart the child so it loads the new preset list.
                    #[cfg(feature = "milkdrop")]
                    if let Some(host) = self.milkdrop_host.as_mut()
                        && host.is_running()
                    {
                        host.close();
                    }
                }
                Err(error) => self.toast_error(
                    // Translators: {error} is an error message.
                    gettext(self.locale, "Couldn't fetch presets: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            }
        }
    }

    /// Syncs MilkDrop settings and receives window state and commands.
    #[cfg(feature = "milkdrop")]
    fn sync_milkdrop(&mut self, ctx: &egui::Context) {
        let presets = self.dirs.milkdrop_dir();
        let open = self.settings.milkdrop_open;
        let size = self.settings.milkdrop_size;
        let pos = self.milkdrop_pos;
        let fullscreen = self.settings.milkdrop_fullscreen;
        let fps = self.settings.milkdrop_fps;
        let seconds = self.settings.milkdrop_seconds;
        let scale = self.settings.milkdrop_scale.max(1);
        // Track metadata shown when the song changes.
        let song = self.now_playing().filter(|now| !now.resuming).map(|now| {
            // Title, artist, and album.
            vec![
                now.title.clone(),
                now.subtitle.clone(),
                now.album_name.clone(),
            ]
        });
        if self.milkdrop_host.is_none() {
            let tap = std::sync::Arc::clone(&self.winamp.tap);
            self.milkdrop_host = Some(crate::milkdrop::host::Host::new(tap));
        }
        let poll = {
            let host = self.milkdrop_host.as_mut().expect("the host was just made");
            if open {
                if !host.is_running() {
                    host.open(&presets, size, pos, fullscreen, fps, seconds, scale);
                }
                host.update(fps, seconds, scale);
                host.song(song);
            } else if host.is_running() {
                host.close();
            }
            host.poll()
        };
        if poll.closed {
            self.settings.milkdrop_open = false;
            self.mark_settings_dirty();
        }
        if let Some(size) = poll.size
            && self.settings.milkdrop_size != size
        {
            self.settings.milkdrop_size = size;
            self.mark_settings_dirty();
        }
        if let Some(pos) = poll.pos {
            self.milkdrop_pos = Some(pos);
        }
        for command in poll.commands {
            self.milkdrop_command(&command);
        }
        if let Some(hz) = poll.screen_hz {
            self.learn_screen_hz(hz);
        }
        // Poll the child while the main window is otherwise idle.
        if self.settings.milkdrop_open {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }

    /// Records the MilkDrop screen refresh rate and uses it as the initial FPS.
    /// Later screen changes do not override a configured FPS.
    #[cfg(feature = "milkdrop")]
    fn learn_screen_hz(&mut self, hz: u32) {
        if hz == 0 || self.settings.milkdrop_screen_hz == hz {
            return;
        }
        let first = self.settings.milkdrop_screen_hz == 0
            && self.settings.milkdrop_fps == crate::milkdrop::DEFAULT_FPS;
        self.settings.milkdrop_screen_hz = hz;
        if first {
            self.settings.milkdrop_fps = hz;
        }
        self.mark_settings_dirty();
    }

    /// Applies playback commands received from the MilkDrop window.
    #[cfg(feature = "milkdrop")]
    fn milkdrop_command(&mut self, command: &str) {
        match command {
            "previous" => self.actions.push(Action::Previous),
            "next" => self.actions.push(Action::Next),
            "play-pause" => self.actions.push(Action::TogglePlay),
            "mute" => self.actions.push(Action::ToggleMute),
            "save-toggle" => {
                if let Some(now) = self.now_playing().filter(|now| !now.is_episode) {
                    self.actions.push(Action::ToggleSaved(now.uri));
                }
            }
            "shuffle" => self.actions.push(Action::ToggleShuffle),
            "volume-up" => self.actions.push(Action::VolumeBy(5)),
            "volume-down" => self.actions.push(Action::VolumeBy(-5)),
            _ => {}
        }
    }

    /// Creates a config folder if needed and opens it in the file manager.
    fn open_folder(&mut self, folder: std::path::PathBuf) {
        let opened = std::fs::create_dir_all(&folder).and_then(|()| crate::opener::open(&folder));
        if let Err(error) = opened {
            self.toast_error(
                // Translators: {folder} is a folder path and {error} is an error message.
                gettext(self.locale, "Couldn't open {folder}: {error}")
                    .replace("{folder}", &folder.display().to_string())
                    .replace("{error}", &error.to_string()),
            );
        }
    }

    /// Applies a loaded skin. Installed files become the selected skin.
    fn skin_loaded(&mut self, loaded: crate::winamp::Loaded) {
        match loaded.result {
            Ok(skin) => {
                self.winamp
                    .wear(Some(loaded.name.clone()), std::sync::Arc::new(skin));
                if loaded.installed {
                    self.toast(
                        // Translators: {skin} is the name of a Winamp skin.
                        gettext(self.locale, "Added {skin} skin")
                            .replace("{skin}", crate::winamp::label(&loaded.name)),
                    );
                    self.winamp.list_choices(&self.dirs.skins_dir());
                    self.settings.skin = Some(loaded.name);
                    self.settings_dirty = true;
                }
            }
            Err(error) => {
                self.toast_error(format!("{}: {error}", crate::winamp::label(&loaded.name)));
                if !loaded.installed {
                    self.settings.skin = self.winamp.worn.clone();
                    self.settings_dirty = true;
                }
            }
        }
    }

    /// Sends equalizer settings to the player and marks them for saving.
    fn push_eq(&mut self) {
        if let Ok(mut shared) = self.winamp.eq.lock() {
            *shared = eq_settings(&self.settings);
        }
        self.settings_dirty = true;
    }

    /// Note that a setting changed, so the file is saved shortly.
    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty = true;
    }

    /// Note that the restorable session changed, so it is saved shortly.
    pub fn note_session_change(&mut self) {
        self.session_dirty = true;
    }

    fn handle_proxy_restored(
        &mut self,
        config: crate::settings::ProxyConfig,
        password: Option<crate::credentials::ProxyPassword>,
    ) {
        if self.last_proxy_applied == 0 {
            self.applied_proxy = config;
        }
        if !self.proxy_form_edited
            && self.proxy_request == 0
            && let Some(password) = password
        {
            self.settings.restore_proxy_password(&password);
        }
    }

    fn handle_proxy_password_stored(&mut self) {
        if self.settings.proxy_password_legacy {
            self.settings.proxy_password_legacy = false;
            self.save_settings();
        }
    }

    fn request_proxy(&mut self, sign_in: bool) {
        let config = match self.settings.proxy_config() {
            Ok(config) => config,
            Err(error) => {
                self.toast_error(error);
                return;
            }
        };
        self.proxy_request = self.proxy_request.wrapping_add(1);
        let request = self.proxy_request;
        self.pending_proxy_preferences
            .insert(request, self.settings.proxy_preferences());
        self.backend.send(if sign_in {
            Command::SignIn { request, config }
        } else {
            Command::ApplyProxy { request, config }
        });
    }

    fn handle_proxy_applied(
        &mut self,
        request: u64,
        config: crate::settings::ProxyConfig,
        result: Result<bool, String>,
    ) {
        let Some(preferences) = self.pending_proxy_preferences.remove(&request) else {
            return;
        };
        if request < self.last_proxy_applied {
            return;
        }
        match result {
            Ok(restarted) => {
                self.last_proxy_applied = request;
                self.applied_proxy = config;
                self.applied_proxy_preferences = preferences;
                self.save_settings();
                if restarted && self.local_ready {
                    self.toast(gettext(self.locale, "Proxy applied. Restarting local playback."));
                } else {
                    self.toast(gettext(self.locale, "Proxy settings applied"));
                }
            }
            Err(error) => self.toast_error(
                gettext(
                    self.locale,
                    // Translators: {error} is an error message.
                    "Proxy could not be applied: {error}. Previous connection settings are still in use.",
                )
                .replace("{error}", &error.to_string()),
            ),
        }
    }

    fn save_settings(&mut self) {
        if self.settings.proxy_password_legacy {
            self.settings_dirty = false;
            self.last_settings_save = Instant::now();
            // Do not erase the sole surviving legacy password when the native
            // store is locked. Its successful callback owns retrying this save.
            return;
        }
        self.settings_dirty = false;
        self.last_settings_save = Instant::now();
        if self.offline {
            // Demo data must never overwrite the person's real preferences.
            return;
        }
        let mut saved = self.settings.clone();
        self.applied_proxy_preferences.apply_to(&mut saved);
        saved.save(&self.dirs.settings_file());
    }

    /// Called at launch or by the local reload command. Construction and window
    /// attachment never scan theme files.
    pub fn load_custom_themes(&mut self, waker: &Waker) {
        self.custom_themes.start(
            self.dirs.config.join("themes"),
            self.settings.custom_theme.clone(),
            waker,
        );
    }

    fn poll_custom_themes(&mut self, ctx: &egui::Context) {
        if !self.custom_themes.poll() {
            return;
        }
        let mut changed = false;
        if let Some(filename) = &self.settings.custom_theme
            && let Some(theme) = self.custom_themes.find(filename)
            && self.settings.custom_theme_cache.as_ref() != Some(theme)
        {
            // Use the current selection, never the selection captured by the scan.
            self.settings.custom_theme_cache = Some(theme.clone());
            changed = true;
        }
        if self.custom_themes.follows_omarchy() {
            if let Some(theme) = self.custom_themes.system_theme()
                && self.settings.system_theme_cache.as_ref() != Some(theme)
            {
                self.settings.system_theme_cache = Some(theme.clone());
                changed = true;
            }
        } else if self.settings.system_theme_cache.take().is_some() {
            changed = true;
        }
        if changed {
            self.mark_settings_dirty();
            ctx.set_theme(self.theme_preference());
        }
    }

    fn custom_palette(&self) -> Option<Palette> {
        self.settings.cached_palette()
    }

    fn theme_preference(&self) -> egui::ThemePreference {
        if let Some(palette) = self.custom_palette() {
            return if palette.dark {
                egui::ThemePreference::Dark
            } else {
                egui::ThemePreference::Light
            };
        }
        match self.settings.theme {
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::System => egui::ThemePreference::System,
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        // winit reports no system theme on Linux, so "Follow system" falls
        // back to what the desktop portal says.
        #[cfg(target_os = "linux")]
        if let Some(dark) = self
            .system_appearance
            .as_ref()
            .and_then(crate::appearance::SystemAppearance::dark)
        {
            let theme = if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            };
            if ctx.options(|options| options.fallback_theme) != theme {
                ctx.options_mut(|options| options.fallback_theme = theme);
            }
        }
        let dark = ctx.theme() == egui::Theme::Dark;
        let palette = self.custom_palette().unwrap_or_else(|| {
            if dark {
                Palette::dark()
            } else {
                Palette::light()
            }
        });
        if self.applied_dark != Some(dark) || self.palette != palette {
            self.palette = palette;
            theme::apply(ctx, &self.palette);
            self.applied_dark = Some(dark);
            self.accents.clear();
            self.accent_pending.clear();
        }
    }

    fn handle_tray(&mut self) {
        let Some(commands) = self.tray.as_ref().map(TrayService::drain_commands) else {
            return;
        };
        for command in commands {
            match command {
                TrayCommand::Show => self.actions.push(Action::ShowWindow),
                TrayCommand::ShowHide => self.actions.push(if self.window_hidden {
                    Action::ShowWindow
                } else {
                    Action::HideWindow
                }),
                TrayCommand::PlayPause => self.actions.push(Action::TogglePlay),
                TrayCommand::Next => self.actions.push(Action::Next),
                TrayCommand::Previous => self.actions.push(Action::Previous),
                TrayCommand::Quit => self.actions.push(Action::Quit),
            }
        }
    }

    /// The Dock menu's playback items, read with or without a window.
    #[cfg(target_os = "macos")]
    fn handle_dock_menu(&mut self) {
        use crate::mac_menu::MenuCommand;
        for command in crate::mac_menu::drain_dock_commands() {
            match command {
                MenuCommand::PlayPause => self.actions.push(Action::TogglePlay),
                MenuCommand::Next => self.actions.push(Action::Next),
                MenuCommand::Previous => self.actions.push(Action::Previous),
                _ => {}
            }
        }
    }

    fn handle_control_commands(&mut self) {
        let Some(queue) = &self.control_commands else {
            return;
        };
        let commands: Vec<ControlCommand> =
            std::mem::take(&mut *queue.lock().unwrap_or_else(|p| p.into_inner()));
        for command in commands {
            let playing = self.now_playing().is_some_and(|now| now.playing);
            let action = match command {
                ControlCommand::Show => Some(Action::ShowWindow),
                ControlCommand::ReloadThemes => Some(Action::ReloadThemes),
                ControlCommand::PlayPause => Some(Action::TogglePlay),
                ControlCommand::Play => (!playing).then_some(Action::TogglePlay),
                ControlCommand::Pause => playing.then_some(Action::TogglePlay),
                ControlCommand::Next => Some(Action::Next),
                ControlCommand::Previous => Some(Action::Previous),
                ControlCommand::SeekBy(offset) => Some(Action::SeekBy(offset)),
                ControlCommand::VolumeBy(delta) => Some(Action::VolumeBy(delta)),
                ControlCommand::SetVolume(volume) => Some(Action::SetVolume(volume.min(100))),
                ControlCommand::ToggleMute => Some(Action::ToggleMute),
                ControlCommand::ToggleShuffle => Some(Action::ToggleShuffle),
                ControlCommand::CycleRepeat => Some(Action::CycleRepeat),
                ControlCommand::SetShuffle(shuffle) => Some(Action::SetShuffle(shuffle)),
                ControlCommand::SetRepeat(mode) => Some(Action::SetRepeat(mode)),
                ControlCommand::SeekTo(position) => Some(Action::Seek(position)),
                // Nothing playing is nothing to save, so the verb is a
                // no-op rather than an error the client has to handle.
                ControlCommand::ToggleSaved => {
                    self.now_playing().map(|now| Action::ToggleSaved(now.uri))
                }
                ControlCommand::PlayUri(uri) => Some(Action::PlayContext {
                    uri,
                    offset_uri: None,
                    offset_index: None,
                }),
                ControlCommand::OpenLink(uri) => Some(Action::OpenLink(uri)),
                ControlCommand::Transfer(device_id) => Some(Action::Transfer(device_id)),
                ControlCommand::RefreshDevices => Some(Action::RefreshDevices),
            };
            if let Some(action) = action {
                self.actions.push(action);
            }
        }
    }

    fn handle_media_commands(&mut self) {
        let Some(commands) = self
            .media_controls
            .as_ref()
            .map(MediaService::drain_commands)
        else {
            return;
        };
        for command in commands {
            let playing = self.now_playing().is_some_and(|now| now.playing);
            let action = match command {
                MediaCommand::Play => (!playing).then_some(Action::TogglePlay),
                MediaCommand::Pause | MediaCommand::Stop => playing.then_some(Action::TogglePlay),
                MediaCommand::PlayPause => Some(Action::TogglePlay),
                MediaCommand::Next => Some(Action::Next),
                MediaCommand::Previous => Some(Action::Previous),
                MediaCommand::SeekBy(offset) => Some(Action::SeekBy(offset)),
                MediaCommand::SetPosition {
                    track_uri,
                    position_ms,
                } => self
                    .now_playing()
                    .filter(|now| now.uri == track_uri)
                    .map(|_| Action::Seek(position_ms)),
                MediaCommand::SetVolume(volume) => Some(Action::SetVolume(
                    (volume.clamp(0.0, 1.0) * 100.0).round() as u8,
                )),
                MediaCommand::SetShuffle(shuffle) => Some(Action::SetShuffle(shuffle)),
                MediaCommand::SetRepeat(mode) => Some(Action::SetRepeat(mode)),
                MediaCommand::OpenUri(uri) => {
                    if crate::link::search_query(&uri).is_some() {
                        crate::link::parse(&uri).map(Action::OpenLink)
                    } else {
                        Some(Action::PlayContext {
                            uri,
                            offset_uri: None,
                            offset_index: None,
                        })
                    }
                }
                MediaCommand::Raise => Some(Action::ShowWindow),
                MediaCommand::Quit => Some(Action::Quit),
            };
            if let Some(action) = action {
                self.actions.push(action);
            }
        }
    }

    /// The downloaded file for `url`, once the art cache has it.
    ///
    /// Windows and macOS are handed a file rather than the URL, so the disk
    /// is asked until the download lands and the answer remembered after
    /// that; see `media_native::file_url` for why a URL will not do. The
    /// player bar only ever draws the small cover, so on a miss the full-size
    /// artwork is fetched here -- the one request the controls add.
    ///
    /// MPRIS passes `art_url` to the desktop, which fetches whatever it
    /// wants: Linux needs no file and downloads nothing extra.
    fn media_art_file(&mut self, ctx: &egui::Context, url: &str) -> Option<PathBuf> {
        if cfg!(target_os = "linux") {
            return None;
        }
        if let Some((known, file)) = &self.media_art
            && known == url
        {
            return Some(file.clone());
        }
        match self.backend.art().cached_file(url) {
            Some(file) => {
                self.media_art = Some((url.to_owned(), file.clone()));
                Some(file)
            }
            None => {
                self.backend.art().prefetch(ctx, url);
                None
            }
        }
    }

    pub fn thumb_state(&self, dark: bool) -> crate::thumbbar::ThumbState {
        let now = self.now_playing();
        crate::thumbbar::ThumbState {
            has_track: now.is_some(),
            playing: now.as_ref().is_some_and(|now| now.playing),
            can_control: now.as_ref().is_some_and(|now| now.can_control),
            dark,
        }
    }

    pub fn windows_controls_visible(&self) -> bool {
        #[cfg(any(test, feature = "demo"))]
        {
            cfg!(windows) || self.demo_windows_controls
        }
        #[cfg(not(any(test, feature = "demo")))]
        {
            cfg!(windows)
        }
    }

    /// Whether to offer hiding the mini player's taskbar entry.
    pub fn taskbar_setting_visible(&self) -> bool {
        self.taskbar_hiding_supported || self.windows_controls_visible()
    }

    fn sync_media_controls(&mut self, ctx: &egui::Context) {
        let art_file = self
            .now_playing()
            .and_then(|now| now.art_url)
            .and_then(|url| self.media_art_file(ctx, &url));
        let state = match self.now_playing() {
            Some(now) => MediaState {
                playback: if now.playing {
                    Playback::Playing
                } else if now.loading {
                    Playback::Loading
                } else {
                    Playback::Paused
                },
                track: Some(MediaTrack {
                    uri: now.uri.clone(),
                    title: now.title.clone(),
                    artists: now
                        .artists
                        .iter()
                        .map(|artist| artist.name.clone())
                        .collect(),
                    album: now.album_name.clone(),
                    art_url: now.art_url.clone(),
                    art_file,
                    duration_ms: now.duration_ms,
                }),
                position_ms: now.position_ms,
                volume: f64::from(now.volume_percent) / 100.0,
                shuffle: now.shuffle,
                repeat: now.repeat,
                can_control: now.can_control,
            },
            None => MediaState::default(),
        };
        if let Some(controls) = &mut self.media_controls {
            controls.update(state);
        }
        let playing = self.now_playing().is_some_and(|now| now.playing);
        if let Some(tray) = &mut self.tray {
            tray.set_playing(playing);
        }
        #[cfg(target_os = "macos")]
        crate::mac_menu::set_playing(playing);
        if let Some(slot) = &self.control_now_playing {
            let snapshot = self.control_snapshot();
            *slot.lock().unwrap_or_else(|p| p.into_inner()) = snapshot;
        }
        if self.control_devices_stale
            && let Some(slot) = self.control_devices.clone()
        {
            let snapshot = self.control_devices_snapshot();
            *slot.lock().unwrap_or_else(|p| p.into_inner()) = snapshot;
            self.control_devices_stale = false;
        }
    }

    /// One line for the control channel's `nowplaying` verb: tab-separated
    /// `state, title, artists, album, position_ms, duration_ms, volume,
    /// shuffle, repeat, art_url, saved, device`, or
    /// [`crate::single_instance::NOTHING_PLAYING`].
    ///
    /// The last three are what a Stream Deck key needs and a media key does
    /// not: something to draw, whether the heart is filled, and where the
    /// sound is coming out. They are appended rather than woven in, so a
    /// script written against the older nine fields still reads correctly.
    fn control_snapshot(&self) -> String {
        let Some(now) = self.now_playing() else {
            return crate::single_instance::NOTHING_PLAYING.to_owned();
        };
        let state = if now.playing { "playing" } else { "paused" };
        // Not every track has been looked up yet; say so rather than
        // claiming an unsaved track the client would draw as a hollow heart
        // and then watch fill in a moment later.
        let saved = match self.is_saved(&now.uri) {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        // Local playback is this computer, which Spotify has not named in
        // the snapshot because it is not a remote device.
        let device = match (&now.device_name, now.local) {
            (Some(name), _) => name.as_str(),
            (None, true) => self.settings.device_name.as_str(),
            (None, false) => "",
        };
        // Tabs separate the fields, so a tab inside one would shift the rest.
        // This runs every frame, and titles almost never contain one, so the
        // usual answer borrows rather than allocating a copy per field.
        fn clean(text: &str) -> std::borrow::Cow<'_, str> {
            match text.contains('\t') {
                true => std::borrow::Cow::Owned(text.replace('\t', " ")),
                false => std::borrow::Cow::Borrowed(text),
            }
        }
        format!(
            "{state}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{saved}\t{}",
            clean(&now.title),
            clean(&now.subtitle),
            clean(&now.album_name),
            now.position_ms,
            now.duration_ms,
            now.volume_percent,
            if now.shuffle { "on" } else { "off" },
            now.repeat.api_name(),
            clean(now.art_url.as_deref().unwrap_or_default()),
            clean(device),
        )
    }

    /// Last Spotify Connect device list as one JSON line.
    ///
    /// JSON safely carries multiple records and free-text device names.
    fn control_devices_snapshot(&self) -> String {
        let devices: Vec<_> = self
            .devices
            .iter()
            .filter_map(|device| {
                Some(serde_json::json!({
                    "id": device.id.as_deref()?,
                    "name": device.name,
                    "kind": device.kind,
                    "active": device.is_active,
                }))
            })
            .collect();
        serde_json::to_string(&devices)
            .unwrap_or_else(|_| crate::single_instance::NO_DEVICES.to_owned())
    }

    // ---- loading ---------------------------------------------------------------

    fn load_playlists(&mut self) {
        if self.library.playlists.is_loading() {
            return;
        }
        self.library.playlists = Loadable::Loading;
        self.library.playlists_next = None;
        self.library.playlists_asked = None;
        self.library.playlists_generation += 1;
        self.backend.api(ApiRequest::MyPlaylists {
            offset: 0,
            generation: self.library.playlists_generation,
        });
    }

    pub fn ensure_loaded(&mut self, page: Page) {
        if !self.is_connected() {
            return;
        }
        match page {
            Page::Home => self.load_home(false),
            Page::TopSongs => self.load_top_songs(false),
            Page::Search => {}
            Page::LikedSongs => self.ensure_liked_songs(),
            Page::Albums => {
                if !self.library.albums.loaded_once {
                    self.load_more(Page::Albums);
                }
            }
            Page::Artists => {
                if !self.library.artists.loaded_once {
                    self.load_more(Page::Artists);
                }
            }
            Page::Podcasts => {
                if !self.library.shows.loaded_once {
                    self.load_more(Page::Podcasts);
                }
            }
            Page::Episodes => {
                if !self.library.episodes.loaded_once {
                    self.load_more(Page::Episodes);
                }
            }
            Page::Playlist(id) => {
                let needs_generation = self
                    .playlist_pages
                    .get(&id)
                    .is_none_or(|page| page.generation == 0);
                if needs_generation {
                    self.load_generation += 1;
                    self.playlist_pages
                        .entry(id.clone())
                        .or_default()
                        .generation = self.load_generation;
                }
                let page = self.playlist_pages.entry(id.clone()).or_default();
                let generation = page.generation;
                if page.playlist.needs_load() {
                    page.playlist = Loadable::Loading;
                    self.backend.api(ApiRequest::Playlist {
                        id: id.clone(),
                        generation,
                    });
                }
                if !page.items.loaded_once && page.items.can_load_more() {
                    page.items.loading = true;
                    self.backend.api(ApiRequest::PlaylistItems {
                        id: id.clone(),
                        offset: 0,
                        generation,
                    });
                    // Disk progress is adopted only if Spotify's snapshot
                    // still matches.
                    self.backend.send(Command::LoadPlaylistCache {
                        id: id.clone(),
                        generation,
                    });
                }
                self.request_contains(vec![format!("spotify:playlist:{id}")]);
            }
            Page::Album(id) => {
                if !self.album_pages.contains_key(&id) {
                    self.load_generation = self.load_generation.wrapping_add(1);
                    self.album_pages.insert(
                        id.clone(),
                        AlbumPage {
                            generation: self.load_generation,
                            ..Default::default()
                        },
                    );
                }
                let page = self.album_pages.entry(id.clone()).or_default();
                if page.album.needs_load() {
                    page.album = Loadable::Loading;
                    self.backend.api(ApiRequest::Album { id: id.clone() });
                }
                self.request_contains(vec![format!("spotify:album:{id}")]);
            }
            Page::Artist(id) => {
                let page = self.artist_pages.entry(id.clone()).or_default();
                if page.artist.needs_load() {
                    page.artist = Loadable::Loading;
                    self.backend.api(ApiRequest::Artist { id: id.clone() });
                }
                let filter = page.filter;
                self.load_artist_albums(&id, filter);
                if page_related_needs_load(&self.artist_pages, &id) {
                    if let Some(page) = self.artist_pages.get_mut(&id) {
                        page.related = Loadable::Loading;
                    }
                    self.backend
                        .api(ApiRequest::RelatedArtists { id: id.clone() });
                }
                self.request_contains(vec![format!("spotify:artist:{id}")]);
            }
            Page::Show(id) => {
                let page = self.show_pages.entry(id.clone()).or_default();
                if page.show.needs_load() {
                    page.show = Loadable::Loading;
                    self.backend.api(ApiRequest::Show { id: id.clone() });
                }
                self.request_contains(vec![format!("spotify:show:{id}")]);
            }
            Page::Radio(seed) => self.load_radio(&seed),
            Page::Queue => self.refresh_queue(true),
            Page::Settings => {}
        }
    }

    fn load_artist_albums(&mut self, id: &str, filter: DiscographyFilter) {
        let Some(page) = self.artist_pages.get_mut(id) else {
            return;
        };
        let list = page.albums.entry(filter.groups().to_string()).or_default();
        if !list.loaded_once && list.can_load_more() {
            list.loading = true;
            self.backend.api(ApiRequest::ArtistAlbums {
                id: id.to_string(),
                groups: filter.groups().to_string(),
                offset: 0,
            });
        }
    }

    fn load_home(&mut self, force: bool) {
        if self.home.requested
            && !force
            && self
                .home
                .loaded_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(600))
        {
            return;
        }
        self.home.requested = true;
        self.home.loaded_at = Some(Instant::now());
        self.home.generation += 1;
        let generation = self.home.generation;
        if self.home.recently_played.get().is_none() {
            self.home.recently_played = Loadable::Loading;
        }
        if self.home.top_artists.get().is_none() {
            self.home.top_artists = Loadable::Loading;
        }
        if self.home.top_tracks.get().is_none() {
            self.home.top_tracks = Loadable::Loading;
        }
        self.backend.api(ApiRequest::RecentlyPlayed {
            who: RecentsFor::Home,
            generation,
            before: None,
            limit: HOME_RECENTS,
        });
        self.backend.api(ApiRequest::TopArtists { generation });
        self.backend.api(ApiRequest::TopTracks {
            offset: 0,
            full: false,
            generation,
        });
        self.home.discover_pending.clear();
        for term in DISCOVER_TERMS {
            self.home
                .discover_pending
                .insert((*term).to_string(), Loadable::Loading);
            if !self.home.discover.contains_key(*term) {
                self.home
                    .discover
                    .insert((*term).to_string(), Loadable::Loading);
            }
            self.backend.api(ApiRequest::Discover {
                term: (*term).to_string(),
                generation,
            });
        }
        // The podcast shelf reads from the saved shows. The first page of
        // them is the one the Podcasts shelf of the library asks for.
        if self.library.shows.loaded_once {
            self.request_home_episodes();
        } else if !self.library.shows.loading && self.library.shows.error.is_none() {
            self.load_more(Page::Podcasts);
        }
    }

    /// Asks for the newest episodes of the most recently saved podcasts,
    /// once per Home refresh. Known audiobooks are skipped: librespot cannot
    /// play them.
    fn request_home_episodes(&mut self) {
        if self.home.podcasts_generation == self.home.generation {
            return;
        }
        self.home.podcasts_generation = self.home.generation;
        let shows: Vec<Show> = self
            .library
            .shows
            .items
            .iter()
            .map(|saved| &saved.show)
            .filter(|show| !show.id.is_empty() && !self.audiobook_shows.contains(&show.uri))
            .take(HOME_PODCAST_SHOWS)
            .cloned()
            .collect();
        if shows.is_empty() {
            self.home.podcasts.clear();
            return;
        }
        self.backend.api(ApiRequest::HomeEpisodes {
            shows,
            generation: self.home.generation,
        });
    }

    fn load_top_songs(&mut self, force: bool) {
        if self.home.top_songs_loading || (!force && self.home.top_songs_complete) {
            return;
        }
        self.home.top_songs = Loadable::Loading;
        self.home.top_songs_loading = true;
        self.home.top_songs_complete = false;
        self.home.top_songs_generation += 1;
        self.backend.api(ApiRequest::TopTracks {
            offset: 0,
            full: true,
            generation: self.home.top_songs_generation,
        });
    }

    pub fn load_recents(&mut self, force: bool) {
        if self.recents.loading {
            return;
        }
        if self.recents.complete && !force {
            return;
        }
        if force {
            self.recents.reset();
        }
        self.recents.loading = true;
        self.recents.error = None;
        self.recents_generation = self.recents_generation.wrapping_add(1);
        let generation = self.recents_generation;
        // This endpoint paginates backwards, so its continuation cursor is
        // named `before`.
        let before = self.recents.after.clone();
        self.backend.api(ApiRequest::RecentlyPlayed {
            who: RecentsFor::Panel,
            generation,
            before,
            limit: RECENTS_PAGE,
        });
    }

    pub fn load_more_recents(&mut self) {
        if !self.recents.can_load_more() {
            return;
        }
        self.load_recents(false);
    }

    pub fn reload_recents(&mut self) {
        self.load_recents(true);
    }

    pub fn load_more(&mut self, page: Page) {
        match page {
            Page::LikedSongs => {
                if !self.liked_songs.cache_checked {
                    self.ensure_liked_songs();
                } else if let Some(offset) = self.liked_songs.request_offset() {
                    self.backend.api(ApiRequest::SavedTracks {
                        offset,
                        generation: self.liked_songs.generation,
                    });
                    self.library.liked.loading = true;
                }
            }
            Page::Albums => {
                let list = &mut self.library.albums;
                if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                    list.loading = true;
                    self.backend.api(ApiRequest::SavedAlbums { offset });
                }
            }
            Page::Artists => {
                let list = &mut self.library.artists;
                if list.can_load_more() {
                    list.loading = true;
                    self.backend.api(ApiRequest::FollowedArtists {
                        after: list.after.clone(),
                    });
                }
            }
            Page::Podcasts => {
                let list = &mut self.library.shows;
                if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                    list.loading = true;
                    self.backend.api(ApiRequest::SavedShows { offset });
                }
            }
            Page::Episodes => {
                let list = &mut self.library.episodes;
                if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                    list.loading = true;
                    self.backend.api(ApiRequest::SavedEpisodes { offset });
                }
            }
            Page::Playlist(id) => {
                if self.playlist_pages.get(&id).is_some_and(|page| {
                    page.pending_writes > 0
                        || page.optimistic_snapshot.is_some()
                        || page.items.error.is_some()
                }) {
                    return;
                }
                let restart_from_top = self.playlist_pages.get(&id).is_some_and(|page| {
                    page.items.base_offset > 0
                        && (!page.filter.trim().is_empty()
                            || self.table_sorts.contains_key(&Page::Playlist(id.clone())))
                });
                if restart_from_top {
                    self.load_playlist_items_at(&id, 0);
                    return;
                }
                if let Some(page) = self.playlist_pages.get_mut(&id) {
                    let list = &mut page.items;
                    if let Some(offset) = list
                        .window_request
                        .or(list.next_offset)
                        .filter(|_| list.can_load_more())
                    {
                        list.loading = true;
                        self.backend.api(ApiRequest::PlaylistItems {
                            id,
                            offset,
                            generation: page.generation,
                        });
                    }
                }
            }
            Page::Album(id) => {
                if let Some(page) = self.album_pages.get_mut(&id) {
                    let list = &mut page.tracks;
                    if list.base_offset > 0
                        && self.table_sorts.contains_key(&Page::Album(id.clone()))
                    {
                        self.load_generation = self.load_generation.wrapping_add(1);
                        page.generation = self.load_generation;
                        let total = list.total;
                        list.reset();
                        list.total = total;
                    }
                    if let Some(offset) = list
                        .window_request
                        .or(list.next_offset)
                        .filter(|_| list.can_load_more())
                    {
                        list.loading = true;
                        self.backend.api(ApiRequest::AlbumTracks {
                            id,
                            offset,
                            generation: page.generation,
                        });
                    }
                }
            }
            Page::Show(id) => {
                if let Some(page) = self.show_pages.get_mut(&id) {
                    let list = &mut page.episodes;
                    if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                        list.loading = true;
                        self.backend.api(ApiRequest::ShowEpisodes { id, offset });
                    }
                }
            }
            Page::Home => {
                if let Some(offset) = self.library.playlists_next.take() {
                    self.library.playlists_asked = Some(offset);
                    self.backend.api(ApiRequest::MyPlaylists {
                        offset,
                        generation: self.library.playlists_generation,
                    });
                }
            }
            _ => {}
        }
    }

    fn load_window(&mut self, page: Page, position: u32) {
        match page {
            Page::Playlist(id) => {
                let Some(page) = self.playlist_pages.get_mut(&id) else {
                    return;
                };
                if page.pending_writes > 0
                    || page.optimistic_snapshot.is_some()
                    || !page.filter.trim().is_empty()
                    || self.table_sorts.contains_key(&Page::Playlist(id.clone()))
                {
                    return;
                }
                let offset = page.items.window_at(position, PLAYLIST_PAGE_SIZE);
                if let Some(offset) = offset {
                    self.backend.api(ApiRequest::PlaylistItems {
                        id,
                        offset,
                        generation: page.generation,
                    });
                }
            }
            Page::Album(id) => {
                let Some(page) = self.album_pages.get_mut(&id) else {
                    return;
                };
                if self.table_sorts.contains_key(&Page::Album(id.clone())) {
                    return;
                }
                if let Some(offset) = page.tracks.window_at(position, 50) {
                    self.backend.api(ApiRequest::AlbumTracks {
                        id,
                        offset,
                        generation: page.generation,
                    });
                }
            }
            _ => {}
        }
    }

    fn retry_window(&mut self, page: Page) {
        match &page {
            Page::Playlist(id) => {
                let Some(playlist) = self.playlist_pages.get_mut(id) else {
                    return;
                };
                if playlist.pending_writes > 0 || playlist.optimistic_snapshot.is_some() {
                    // A failed snapshot confirmation must retry that confirmation
                    // before any replacement rows can be requested.
                    self.reload(page);
                    return;
                }
                if playlist.items.error.is_none() || !playlist.items.can_load_more() {
                    return;
                }
                playlist.items.error = None;
            }
            Page::Album(id) => {
                let Some(album) = self.album_pages.get_mut(id) else {
                    return;
                };
                if album.tracks.error.is_none() || !album.tracks.can_load_more() {
                    return;
                }
                album.tracks.error = None;
            }
            _ => return,
        }
        self.load_more(page);
    }

    fn load_playlist_items_at(&mut self, id: &str, offset: u32) {
        let generation = {
            let Some(page) = self.playlist_pages.get_mut(id) else {
                return;
            };
            self.load_generation += 1;
            page.generation = self.load_generation;
            let total = page.items.total;
            page.items.reset_at(offset);
            page.items.total = total;
            page.items.loading = true;
            page.tail_checked = false;
            page.cache_restored_through = None;
            page.pending_cache = None;
            page.cache_checked = true;
            page.cache_write_pending = None;
            page.cache_append_valid = false;
            page.local_additions.clear();
            page.optimistic_snapshot = None;
            page.snapshot_rechecks = 0;
            page.refresh_after_write = false;
            page.generation
        };
        self.clear_picked_rows();
        self.backend.api(ApiRequest::PlaylistItems {
            id: id.to_string(),
            offset,
            generation,
        });
    }

    fn reload(&mut self, page: Page) {
        match &page {
            Page::Home => self.load_home(true),
            Page::TopSongs => self.load_top_songs(true),
            Page::LikedSongs => {
                self.refresh_liked_songs();
                return;
            }
            Page::Albums => self.library.albums.reset(),
            Page::Artists => self.library.artists.reset(),
            Page::Podcasts => self.library.shows.reset(),
            Page::Episodes => self.library.episodes.reset(),
            Page::Playlist(id) => {
                if let Some(playlist) = self.playlist_pages.get_mut(id) {
                    playlist.items.loading = true;
                    playlist.items.error = None;
                    if playlist.pending_writes > 0 || playlist.optimistic_snapshot.is_some() {
                        // A refresh must not turn pre-write rows into a new,
                        // apparently current generation. Wait for the write's
                        // metadata confirmation before asking for rows.
                        playlist.refresh_after_write = true;
                        playlist.snapshot_rechecks = 0;
                        if playlist.pending_writes == 0 {
                            self.backend.api(ApiRequest::Playlist {
                                id: id.clone(),
                                generation: playlist.generation,
                            });
                        }
                        return;
                    }
                    playlist.refresh_after_write = false;
                    self.load_generation += 1;
                    playlist.generation = self.load_generation;
                    playlist.items.clear_windows();
                    playlist.cache_checked = true;
                    playlist.cache_restored_through = None;
                    playlist.pending_cache = None;
                    playlist.cache_write_pending = None;
                    playlist.cache_append_valid = false;
                    playlist.local_additions.clear();
                    playlist.optimistic_snapshot = None;
                    playlist.snapshot_rechecks = 0;
                    self.backend.api(ApiRequest::Playlist {
                        id: id.clone(),
                        generation: playlist.generation,
                    });
                    self.backend.api(ApiRequest::PlaylistItems {
                        id: id.clone(),
                        offset: 0,
                        generation: playlist.generation,
                    });
                    return;
                }
            }
            Page::Album(id) => {
                self.album_pages.remove(id);
            }
            Page::Artist(id) => {
                self.artist_pages.remove(id);
            }
            Page::Show(id) => {
                self.show_pages.remove(id);
            }
            Page::Radio(seed) => {
                // A new mix replaces the songs on screen when it arrives.
                if self.refresh_radio(seed) {
                    return;
                }
                self.radio_pages.remove(seed);
            }
            Page::Queue => self.queue = Loadable::NotLoaded,
            _ => {}
        }
        self.ensure_loaded(page);
    }

    fn poll_remote(&mut self, _immediate: bool) {
        if !self.is_connected() {
            return;
        }
        self.remote_poll_pending = true;
        self.remote_polled_at = Instant::now();
        self.remote_poll_seq += 1;
        self.backend.api(ApiRequest::PlaybackState {
            seq: self.remote_poll_seq,
        });
    }

    fn refresh_devices(&mut self) {
        if !self.is_connected() || self.devices_loading {
            return;
        }
        self.devices_loading = true;
        self.backend.api(ApiRequest::Devices);
    }

    fn refresh_queue(&mut self, force: bool) {
        if !self.is_connected() {
            return;
        }
        if self.resume_only() && matches!(self.queue, Loadable::Loaded(_)) {
            // Nothing is playing anywhere and the queue on show is the
            // remembered one; a fetch could only replace it with less.
            return;
        }
        if self.queue.is_loading() && !force {
            return;
        }
        if !matches!(self.queue, Loadable::Loaded(_)) {
            self.queue = Loadable::Loading;
        }
        self.queue_fetched_at = Some(Instant::now());
        self.queue_seq += 1;
        self.backend.api(ApiRequest::Queue {
            seq: self.queue_seq,
        });
    }

    /// A chosen row of Next up plays at once, and the rows above it go
    /// with it: skips consume the queue, so the playing context and the
    /// songs queued after the chosen one stay intact. Loading the queue's
    /// rows as a fresh list instead used to take seconds, threw the
    /// context away, and left Spotify's copy of the queue to reappear.
    fn play_queue_item(&mut self, index: usize, uri: String) {
        if self.resume_only() {
            // Nothing is playing anywhere, so there is no live queue to
            // consume: play the shown rows as a plain list.
            let uris: Vec<String> = self
                .queue
                .get()
                .map(|queue| {
                    queue
                        .queue
                        .iter()
                        .map(|item| item.uri().to_string())
                        .collect()
                })
                .unwrap_or_default();
            if uris.is_empty() {
                return;
            }
            let (uris, index) = cap_uris(&uris, index as u32);
            self.play_request(PlayRequest::tracks(uris).starting_at_index(index), false);
            return;
        }
        let mut skips = index + 1;
        let mut consumed: Vec<String> = Vec::new();
        if let Loadable::Loaded(queue) = &mut self.queue {
            // The click names a song; if the rows shifted under the
            // pointer, the song wins over the row number.
            let position = match queue.queue.get(index) {
                Some(item) if item.uri() == uri => Some(index),
                _ => queue.queue.iter().position(|item| item.uri() == uri),
            };
            let Some(position) = position else {
                self.refresh_queue(true);
                return;
            };
            skips = position + 1;
            let mut items: Vec<_> = queue.queue.drain(..=position).collect();
            let chosen = items.pop().expect("the chosen row was just drained");
            consumed = items.iter().map(|item| item.uri().to_string()).collect();
            consumed.push(chosen.uri().to_string());
            queue.currently_playing = Some(chosen);
        }
        for gone in &consumed {
            if let Some(at) = self.manual_queue.iter().position(|queued| queued == gone) {
                self.remove_manual_queue_row(at);
            }
        }
        self.expect_track(uri.clone(), 0);
        self.queue_start_pending = Some(self.target());
        self.set_play_pending(vec![uri]);
        self.optimistic_playing = Some((true, Instant::now()));
        match self.target() {
            Target::Local => {
                for _ in 0..skips {
                    self.backend.player(PlayerCommand::Next);
                }
            }
            Target::Remote(device_id) => {
                // With nothing to act on, one call earns the "pick
                // something first" toast; a skip per row would repeat it.
                if device_id.is_none() && self.remote_fresh().is_none() {
                    self.remote(RemoteAction::Next, None);
                    return;
                }
                for _ in 0..skips {
                    self.remote(RemoteAction::Next, device_id.clone());
                }
            }
        }
    }

    /// How many leading rows of Next up are songs the user queued here,
    /// so the view can give them their own section.
    pub fn queued_rows_len(&self) -> usize {
        // Before resume, use the saved manual queue to split restored rows.
        let manual = if self.manual_queue.is_empty() && self.resume_only() {
            &self.resume_queue
        } else {
            &self.manual_queue
        };
        match &self.queue {
            Loadable::Loaded(queue) => Self::end_of_queued_rows(&queue.queue, manual),
            _ => 0,
        }
    }

    /// Whether the local player is the active target, so its queue can be
    /// rewritten directly. Neither the Web API nor librespot can reorder or
    /// insert into a live queue; the only way to change one is to clear it
    /// and re-add its songs in the new order, which only reaches the
    /// engine actually playing them.
    pub fn queue_locally_reorderable(&self) -> bool {
        self.local.is_active() && matches!(self.target(), Target::Local)
    }

    /// Whether the active local queue has rows that can be cleared.
    pub fn can_clear_queue(&self) -> bool {
        self.queue_locally_reorderable() && self.queued_rows_len() > 0
    }

    /// Clears manually queued tracks while keeping the context's upcoming rows.
    fn clear_queue(&mut self) {
        if !matches!(self.target(), Target::Local) {
            return;
        }
        self.pending_album_queues.clear();
        self.last_album_queue = None;
        // `queue_one` writes every queued song to both lists, so they hold
        // the same wishes and adding their counts asks for twice the rows
        // Next up was given. The extra row taken is the context's own copy
        // of that song, which stays. Neither list alone is the count
        // either: a pending add outlives its `manual_queue` entry once the
        // song starts.
        // Take as many rows as the longer of the two holds.
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for uri in &self.manual_queue {
            *counts.entry(uri.clone()).or_insert(0) += 1;
        }
        let mut pending: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for addition in &self.pending_queue_adds {
            *pending.entry(addition.item.uri()).or_insert(0) += 1;
        }
        for (uri, count) in pending {
            let held = counts.entry(uri.to_string()).or_insert(0);
            *held = (*held).max(count);
        }
        if let Loadable::Loaded(queue) = &mut self.queue {
            // Remove one row per queued copy, starting at the front.
            queue.queue.retain(|item| match counts.get_mut(item.uri()) {
                Some(left) if *left > 0 => {
                    *left -= 1;
                    false
                }
                _ => true,
            });
        }
        let cleared: std::collections::HashSet<String> = counts.into_keys().collect();
        if !self.manual_queue.is_empty() {
            self.session_dirty = true;
        }
        self.manual_queue.clear();
        self.pending_queue_adds.clear();
        if !cleared.is_empty() {
            self.queue_cleared = Some((cleared, Instant::now()));
        }
        self.backend.player(PlayerCommand::ClearQueue);
        // Refresh to remove queued tracks added by another client.
        self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
        self.toast(gettext(self.locale, "Queue cleared"));
    }

    /// Current and upcoming track URIs, deduplicated in playback order.
    pub fn queue_playlist_uris(&self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut uris = Vec::new();
        let queued = self.queue.get();
        let rows = self.now_playing().map(|now| now.uri).into_iter().chain(
            queued
                .iter()
                .flat_map(|queue| queue.queue.iter().map(|item| item.uri().to_string())),
        );
        for uri in rows {
            if seen.insert(uri.clone()) {
                uris.push(uri);
            }
        }
        uris
    }

    /// Name for a playlist created from the queue.
    pub fn queue_playlist_name(&self) -> String {
        if let Some(context) = self.playing_context_uri()
            && let Some(name) = self.station_name(&context)
        {
            return name;
        }
        let today = jiff::Zoned::now().strftime("%Y-%m-%d").to_string();
        // Translators: Keep {date} unchanged. It is a date in YYYY-MM-DD form.
        gettext(self.locale, "Queue {date}").replace("{date}", &today)
    }

    /// Saves the queue as a new playlist.
    fn save_queue_as_playlist(&mut self) {
        let uris = self.queue_playlist_uris();
        if uris.is_empty() {
            return;
        }
        let name = self.queue_playlist_name();
        self.actions.push(Action::CreatePlaylist {
            name,
            public: false,
            add_uris: uris,
        });
    }

    /// Shows the head of Next up as playing before the engine confirms it.
    fn pop_queue_head(&mut self) {
        // A restored or stale queue has no trustworthy relationship to the
        // current player state. It can stay visible while the live queue is
        // fetched, but must not be used to guess where Next will land.
        let current = self.current_track_uri();
        let Loadable::Loaded(queue) = &mut self.queue else {
            return;
        };
        let anchored =
            current.as_deref() == queue.currently_playing.as_ref().map(PlayableItem::uri);
        if !anchored || queue.queue.is_empty() {
            return;
        }
        let item = queue.queue.remove(0);
        let uri = item.uri().to_string();
        // The queue row already has the song's details, so the player bar
        // can name it now instead of waiting for the engine to load it.
        let track = match &item {
            crate::api::models::PlayableItem::Track(track) => Some(track.clone()),
            crate::api::models::PlayableItem::Episode(_) => None,
        };
        queue.currently_playing = Some(item);
        if let Some(track) = track
            && let Some(id) = util::uri_id(&uri)
        {
            self.track_cache.entry(id.to_owned()).or_insert(track);
        }
        self.consume_manual_queue_head(&uri);
        self.queue_start_pending = Some(self.target());
        self.expect_track(uri, 0);
    }

    /// Consume one queued occurrence, retaining any later copy of this song.
    fn consume_manual_queue_head(&mut self, uri: &str) {
        if self.manual_queue.first().map(String::as_str) == Some(uri) {
            self.remove_manual_queue_row(0);
        }
    }

    fn remove_manual_queue_row(&mut self, index: usize) {
        self.manual_queue.remove(index);
        self.pending_queue_adds.retain_mut(|addition| {
            if addition.manual_index == index {
                return false;
            }
            if addition.manual_index > index {
                addition.manual_index -= 1;
            }
            true
        });
        self.session_dirty = true;
    }

    /// Whether a fetched queue predates the latest local change.
    fn queue_fetch_is_stale(&self, fetched: &Queue) -> bool {
        if self.queue_stale_retries >= QUEUE_STALE_RETRIES {
            return false;
        }
        // A recent play or pop must be reflected in the fetched current row.
        if let Some(intent) = &self.intent_track
            && (intent.at.elapsed() < PLAYBACK_HOLD || self.queued_play.is_some())
            && fetched
                .currently_playing
                .as_ref()
                .is_none_or(|item| item.uri() != intent.uri)
        {
            return true;
        }
        // For local playback, reject a fetched current row that differs from
        // the engine's current track.
        if self.local.is_active()
            && let Some(track) = &self.local.track
            && fetched
                .currently_playing
                .as_ref()
                .is_some_and(|item| item.uri() != track.uri)
        {
            return true;
        }
        // A cleared row still on top means the clear has not landed yet.
        if let Some((cleared, at)) = &self.queue_cleared
            && at.elapsed() < PLAYBACK_HOLD
            && fetched
                .queue
                .first()
                .is_some_and(|item| cleared.contains(item.uri()))
        {
            return true;
        }
        // An unchanged context order right after toggling shuffle means the
        // reordered queue has not landed yet.
        if let Some(pending) = &self.queue_shuffle_pending
            && pending.at.elapsed() < PLAYBACK_HOLD
            && fetched.currently_playing.as_ref().map(|item| item.uri())
                == pending.current_uri.as_deref()
        {
            let at = Self::end_of_queued_rows(&fetched.queue, &self.manual_queue);
            let fetched_context = &fetched.queue[at..];
            if fetched_context.len() == pending.context_uris.len()
                && fetched_context
                    .iter()
                    .map(|item| item.uri())
                    .eq(pending.context_uris.iter().map(String::as_str))
            {
                return true;
            }
        }
        // A local reorder or positional insert is optimistic; reject a
        // fetch whose queued rows have not caught up to it yet.
        if let Some(at) = self.queue_reorder_pending
            && at.elapsed() < PLAYBACK_HOLD
            && !fetched
                .queue
                .iter()
                .take(self.manual_queue.len())
                .map(|item| item.uri())
                .eq(self.manual_queue.iter().map(String::as_str))
        {
            return true;
        }
        false
    }

    fn run_search(&mut self, query: String) {
        if !query.is_empty()
            && query == self.search.committed
            && !self.search.results.needs_load()
            && self.search.error.is_none()
        {
            return;
        }
        self.search.serial += 1;
        self.search.committed = query.clone();
        self.search.playlists = None;
        self.search.error = None;
        self.search.catalogue_pending = !query.is_empty();
        self.search.playlists_pending = false;
        // Results and their query change together. In particular, a failed
        // new query must never inherit the previous query's songs.
        self.search.results = if query.is_empty() {
            Loadable::NotLoaded
        } else {
            Loadable::Loading
        };
        // An empty query also cancels requests waiting on Spotify's quota.
        self.backend.api(ApiRequest::Search {
            query,
            serial: self.search.serial,
        });
    }

    fn show_search_playlists(&mut self) {
        let Some((serial, page)) = self.search.playlists.as_ref() else {
            return;
        };
        if *serial != self.search.serial {
            return;
        }
        if self.search.results_serial != *serial || self.search.results.get().is_none() {
            self.search.results = Loadable::Loaded(Default::default());
            self.search.results_serial = *serial;
        }
        if let Some(results) = self.search.results.get_mut() {
            results.playlists = Some(page.clone());
        }
    }

    fn search_failed(&mut self, part: &str, error: impl std::fmt::Display) {
        let message = format!("{part}: {error}");
        self.search.error = Some(match self.search.error.take() {
            Some(previous) => format!("{previous}\n{message}"),
            None => message,
        });
        if self.search.results_serial != self.search.serial
            && !self.search.catalogue_pending
            && !self.search.playlists_pending
        {
            self.search.results = Loadable::Failed(self.search.error.clone().unwrap());
        }
    }

    /// Resolves user IDs that do not have a cached display name.
    pub fn request_user_names(&mut self, ids: Vec<String>) {
        let unknown: Vec<String> = ids
            .into_iter()
            .filter(|id| !self.user_names.contains_key(id))
            .collect();
        if unknown.is_empty() {
            return;
        }
        for id in &unknown {
            self.user_names.insert(id.clone(), None);
        }
        self.backend.send(Command::UserNames(unknown));
    }

    fn request_album_types<'a>(&mut self, albums: impl IntoIterator<Item = &'a Album>) {
        let mut uris = Vec::new();
        for album in albums {
            if album.is_single_release()
                && !album.uri.is_empty()
                && self.album_types_requested.insert(album.uri.clone())
            {
                uris.push(album.uri.clone());
            }
        }
        if !uris.is_empty() {
            self.backend.album_types(uris);
        }
    }

    pub fn request_contains(&mut self, uris: Vec<String>) {
        let mut batch = Vec::new();
        for uri in uris {
            if uri.is_empty()
                || self.saved.contains_key(&uri)
                || self.saved_pending.contains(&uri)
                || uri.starts_with("spotify:local")
            {
                continue;
            }
            self.saved_pending.insert(uri.clone());
            batch.push(uri);
            if batch.len() == CONTAINS_BATCH {
                self.backend.api(ApiRequest::Contains {
                    uris: std::mem::take(&mut batch),
                });
            }
        }
        if !batch.is_empty() {
            self.backend.api(ApiRequest::Contains { uris: batch });
        }
    }

    // ---- api responses -------------------------------------------------------

    fn cover_chosen(
        &mut self,
        id: &str,
        request: u64,
        result: Result<Option<crate::playlist_cover::Cover>, String>,
    ) {
        if let Some(Dialog::EditPlaylist {
            id: current, cover, ..
        }) = &mut self.dialog
            && current == id
            && cover.request == Some(request)
        {
            cover.request = None;
            match result {
                Ok(Some(selected)) => {
                    cover.selection = Some(selected);
                    cover.error = None;
                }
                Ok(None) => {}
                Err(error) => cover.error = Some(error),
            }
        }
    }

    fn set_playlist_cover(&mut self, id: &str, cover: &crate::playlist_cover::Cover) {
        if let Some(page) = self.playlist_pages.get_mut(id)
            && let Some(playlist) = page.playlist.get_mut()
        {
            playlist.images = cover_images(cover);
        }
        if let Some(playlists) = self.library.playlists.get_mut() {
            for playlist in playlists.iter_mut().filter(|playlist| playlist.id == id) {
                playlist.images = cover_images(cover);
            }
        }
    }

    fn reconcile_playlist_cover(&mut self, id: &str, images: &mut Vec<crate::api::models::Image>) {
        let Some(pending) = self.uploaded_covers.get_mut(id) else {
            return;
        };
        let new_urls = !images.is_empty()
            && images
                .iter()
                .all(|image| !pending.previous_urls.contains(&image.url));
        if new_urls {
            if pending.checking.is_none() {
                pending.checking = Some(images.clone());
                self.backend.send(Command::CheckPlaylistCover {
                    id: id.into(),
                    request: pending.request,
                    cover: pending.cover.clone(),
                    images: images.clone(),
                });
            } else if pending.checking.as_ref() != Some(images) {
                // Keep the latest candidate while one image is being checked.
                // Do not start parallel downloads for every metadata reply.
                pending.next_images = Some(images.clone());
            }
        }
        *images = cover_images(&pending.cover);
        if !new_urls {
            self.recheck_playlist_cover(id);
        }
    }

    fn recheck_playlist_cover(&mut self, id: &str) {
        if let Some(pending) = self.uploaded_covers.get_mut(id)
            && pending.rechecks_left > 0
            && let Some(page) = self.playlist_pages.get(id)
        {
            pending.rechecks_left -= 1;
            self.backend.api(ApiRequest::Playlist {
                id: id.into(),
                generation: page.generation,
            });
        }
    }

    fn cover_checked(
        &mut self,
        id: &str,
        request: u64,
        images: Vec<crate::api::models::Image>,
        result: Result<bool, String>,
    ) {
        let Some(pending) = self.uploaded_covers.get_mut(id) else {
            return;
        };
        if pending.request != request || pending.checking.as_ref() != Some(&images) {
            return;
        }
        pending.checking = None;
        let next_images = pending.next_images.take();
        let should_recheck = matches!(result, Ok(false));
        match result {
            Ok(true) => {
                self.uploaded_covers.remove(id);
                if let Some(page) = self.playlist_pages.get_mut(id)
                    && let Some(playlist) = page.playlist.get_mut()
                {
                    playlist.images = images.clone();
                }
                if let Some(playlists) = self.library.playlists.get_mut() {
                    for playlist in playlists.iter_mut().filter(|playlist| playlist.id == id) {
                        playlist.images = images.clone();
                    }
                }
            }
            Ok(false) => {
                pending
                    .previous_urls
                    .extend(images.into_iter().map(|image| image.url));
            }
            // The successful upload stays visible if its artwork cannot be
            // checked. A later metadata refresh can try that URL again.
            Err(_) => {}
        }
        if self.uploaded_covers.contains_key(id) {
            if let Some(mut next) = next_images {
                self.reconcile_playlist_cover(id, &mut next);
            } else if should_recheck {
                self.recheck_playlist_cover(id);
            }
        }
    }

    fn handle_api(&mut self, mut response: ApiResponse) {
        match &mut response {
            ApiResponse::Playlist {
                id,
                result: Ok(playlist),
                generation,
            } => {
                if self
                    .playlist_pages
                    .get(id)
                    .is_some_and(|page| page.generation == *generation)
                {
                    self.reconcile_playlist_cover(id, &mut playlist.images);
                }
            }
            ApiResponse::MyPlaylists {
                result: Ok(page), ..
            } => {
                for playlist in &mut page.items {
                    self.reconcile_playlist_cover(&playlist.id, &mut playlist.images);
                }
            }
            _ => {}
        }
        match response {
            ApiResponse::Me(result) => match result {
                Ok(user) => {
                    // Spotify only takes playback commands from Premium
                    // accounts, here or on any device, so a Free account
                    // is told once rather than left pressing play.
                    let free = user
                        .product
                        .as_deref()
                        .is_some_and(|product| product != "premium");
                    if free && !self.premium_notice_shown {
                        self.premium_notice_shown = true;
                        self.dialog = Some(Dialog::PremiumNeeded);
                    }
                    if self.user_id() != Some(user.id.as_str()) {
                        self.rootlist = self
                            .rootlist_cache
                            .as_ref()
                            .filter(|cached| cached.account_id == user.id)
                            .map(|cached| cached.entries.clone())
                            .unwrap_or_default();
                        self.editable_by_grant.clear();
                    }
                    self.user = Some(user);
                    let page = self.page().clone();
                    self.ensure_loaded(page);
                    if let Some(now) = self.now_playing() {
                        self.request_contains(vec![now.uri]);
                    }
                }
                Err(error) => {
                    if matches!(error, crate::api::ApiError::SignInExpired { .. }) {
                        self.auth = AuthStatus::Failed(
                            gettext(
                                self.locale,
                                "Your Spotify sign-in expired. Please sign in again.",
                            )
                            .into_owned(),
                        );
                    } else {
                        self.toast_error(
                            // Translators: {error} is an error message.
                            gettext(self.locale, "Couldn't load your profile: {error}")
                                .replace("{error}", &error.to_string()),
                        );
                    }
                }
            },
            ApiResponse::Devices(result) => {
                self.devices_loading = false;
                self.devices_fetched_at = Some(Instant::now());
                match result {
                    Ok(devices) => {
                        self.devices = devices;
                        self.control_devices_stale = true;
                        if let Some((name, since)) = self.pending_transfer_to.clone() {
                            let matching = self
                                .devices
                                .iter()
                                .find(|device| device.name == name)
                                .and_then(|device| device.id.clone());
                            if let Some(id) = matching {
                                self.pending_transfer_to = None;
                                self.transfer(id);
                            } else if since.elapsed() > Duration::from_secs(20) {
                                self.pending_transfer_to = None;
                            } else {
                                self.devices_fetched_at = None;
                            }
                        }
                        if let Some(selected) = &self.selected_device
                            && !self
                                .devices
                                .iter()
                                .any(|device| device.id.as_deref() == Some(selected.as_str()))
                        {
                            self.selected_device = None;
                        }
                    }
                    Err(error) => self.toast_error(
                        // Translators: {error} is an error message.
                        gettext(self.locale, "Couldn't list devices: {error}")
                            .replace("{error}", &error.to_string()),
                    ),
                }
            }
            ApiResponse::PlaybackState { seq, result } => {
                if seq != self.remote_poll_seq {
                    // An older poll finishing late describes the past.
                    return;
                }
                self.remote_poll_pending = false;
                match result {
                    Ok(state) => {
                        let previous_uri = self.remote.as_ref().and_then(|remote| {
                            remote
                                .state
                                .item
                                .as_ref()
                                .map(|item| item.uri().to_string())
                        });
                        let previous_shuffle = self
                            .remote
                            .as_ref()
                            .map(|remote| remote.state.shuffle_state);
                        self.remote = state.map(|state| RemoteSnapshot {
                            state,
                            received_at: Instant::now(),
                        });
                        if let (Some(previous), Some(current)) = (
                            previous_shuffle,
                            self.remote
                                .as_ref()
                                .map(|remote| remote.state.shuffle_state),
                        ) && previous != current
                            && self
                                .shuffle_set_at
                                .is_none_or(|at| at.elapsed() > Duration::from_secs(5))
                        {
                            // Accept shuffle changes from another device.
                            self.shuffle_wanted = current;
                        }
                        if let Some(context) = self
                            .remote
                            .as_ref()
                            .and_then(|remote| remote.state.context.as_ref())
                            .map(|context| context.uri.clone())
                        {
                            // Ignore the old context while a takeover settles.
                            let stale = self.assumed_context.as_ref().is_some_and(|assumed| {
                                assumed.at.elapsed() < ASSUMED_CONTEXT_HOLD
                                    && assumed.uri != context
                            });
                            if !stale {
                                self.note_recent_context(&context);
                            }
                        }
                        let uri = self.remote.as_ref().and_then(|remote| {
                            remote
                                .state
                                .item
                                .as_ref()
                                .map(|item| item.uri().to_string())
                        });
                        self.reconcile_remote_track_intent(seq, uri.as_deref());
                        if let Some(remote) = &self.remote
                            && let Some(device) = &remote.state.device
                            && device.id.is_some()
                            && let Some(known) =
                                self.devices.iter_mut().find(|known| known.id == device.id)
                        {
                            known.is_active = true;
                            known.volume_percent = device.volume_percent;
                        }
                        if let Some((wanted, _)) = self.optimistic_playing
                            && self
                                .remote
                                .as_ref()
                                .is_some_and(|remote| remote.state.is_playing == wanted)
                        {
                            self.optimistic_playing = None;
                        }
                        if uri != previous_uri {
                            self.on_now_playing_changed();
                        }
                    }
                    Err(error) => log::debug!("playback state unavailable: {error}"),
                }
            }
            ApiResponse::Queue { seq, result } => {
                if seq != self.queue_seq {
                    // A newer request supersedes this response.
                    return;
                }
                // Partial reads and read failures must preserve optimistic
                // additions while Spotify is still accepting their writes.
                let writing_queue = self
                    .pending_queue_batches
                    .values()
                    .any(|target| *target == self.target());
                if writing_queue
                    || result
                        .as_ref()
                        .is_ok_and(|fetched| self.queue_fetch_is_stale(fetched))
                {
                    // Keep the optimistic queue and retry after a stale
                    // response. Accept Spotify's state after the retry limit.
                    self.queue_stale_retries = self.queue_stale_retries.saturating_add(1);
                    self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
                    return;
                }
                self.queue_stale_retries = 0;
                if result.is_ok() {
                    self.queue_cleared = None;
                    self.queue_shuffle_pending = None;
                    self.queue_reorder_pending = None;
                }
                self.queue = Loadable::from_result(result);
                self.reconcile_pending_queue();
                if let Some(queue) = self.queue.get() {
                    let uris: Vec<String> = queue
                        .queue
                        .iter()
                        .map(|item| item.uri().to_string())
                        .collect();
                    self.request_contains(uris);
                }
            }
            ApiResponse::RecentlyPlayed {
                who,
                generation,
                limit,
                result,
            } => match who {
                RecentsFor::Home => {
                    if generation != self.home.generation {
                        return;
                    }
                    if let Ok(page) = &result {
                        self.note_recent_contexts(&page.items);
                    }
                    let items = result
                        .as_ref()
                        .map(|page| page.items.clone())
                        .map_err(|error| error.to_string());
                    self.home.recently_played.refresh(items);
                }
                RecentsFor::Panel => {
                    if generation != self.recents_generation {
                        return;
                    }
                    self.recents.loading = false;
                    self.recents.loaded_once = true;
                    match result {
                        Ok(page) => self.absorb_recents(page, limit),
                        Err(error) => self.recents.error = Some(error.to_string()),
                    }
                }
            },
            ApiResponse::TopTracks {
                offset,
                full,
                generation,
                result,
            } => {
                if full {
                    if generation != self.home.top_songs_generation {
                        return;
                    }
                    match result {
                        Ok(page) => {
                            let received = page.items.len() as u32;
                            let tracks = page.items;
                            let uris: Vec<String> =
                                tracks.iter().map(|track| track.uri.clone()).collect();
                            self.request_contains(uris);
                            if offset == 0 {
                                self.home.top_songs = Loadable::Loaded(tracks);
                            } else if let Some(current) = self.home.top_songs.get_mut() {
                                current.extend(tracks);
                            }
                            if page.next.is_some() && received > 0 && offset + received < 100 {
                                self.backend.api(ApiRequest::TopTracks {
                                    offset: offset + received,
                                    full: true,
                                    generation,
                                });
                            } else {
                                self.home.top_songs_loading = false;
                                self.home.top_songs_complete = true;
                            }
                        }
                        Err(error) => {
                            self.home.top_songs.refresh(Err::<Vec<Track>, _>(error));
                            self.home.top_songs_loading = false;
                        }
                    }
                } else if generation == self.home.generation
                    && let Ok(page) = result
                {
                    let tracks = page.items;
                    let seeds: Vec<String> = tracks
                        .iter()
                        .filter_map(|track| track.id.clone())
                        .take(5)
                        .collect();
                    if !seeds.is_empty() {
                        if self.home.recommendations.get().is_none() {
                            self.home.recommendations = Loadable::Loading;
                        }
                        self.backend.api(ApiRequest::Recommendations {
                            seed_tracks: seeds,
                            seed_artists: Vec::new(),
                            generation,
                        });
                    }
                    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();
                    self.request_contains(uris);
                    self.home.top_tracks = Loadable::Loaded(tracks);
                } else if generation == self.home.generation
                    && offset == 0
                    && let Err(error) = result
                    && self.home.top_tracks.get().is_none()
                {
                    self.home.top_tracks = Loadable::Failed(error.to_string());
                }
            }
            ApiResponse::TopArtists { generation, result } => {
                if generation != self.home.generation {
                    return;
                }
                self.home.top_artists.refresh(result);
            }
            ApiResponse::Recommendations { generation, result } => {
                if generation != self.home.generation {
                    return;
                }
                if let Ok(tracks) = &result {
                    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();
                    self.request_contains(uris);
                }
                self.home.recommendations.refresh(result);
            }
            ApiResponse::Discover {
                term,
                generation,
                result,
            } => {
                if generation != self.home.generation {
                    return;
                }
                let filtered = result.map(|playlists| {
                    let mut seen = std::collections::HashSet::new();
                    let mut matching: Vec<Playlist> = playlists
                        .into_iter()
                        .filter(|playlist| {
                            let owner = playlist.owner.id.as_deref().unwrap_or("");
                            is_made_for_you(&playlist.name, &term)
                                && (owner == "spotify" || playlist.owner_name() == "Spotify")
                                && seen.insert(playlist.name.to_lowercase())
                        })
                        .collect();
                    matching.truncate(6);
                    matching
                });
                self.home
                    .discover_pending
                    .insert(term, Loadable::from_result(filtered));
                let complete = DISCOVER_TERMS.iter().all(|term| {
                    self.home
                        .discover_pending
                        .get(*term)
                        .is_some_and(|result| !result.is_loading())
                });
                if complete {
                    self.home.discover = std::mem::take(&mut self.home.discover_pending);
                }
            }
            // A reload reads the playlists from the top again under a new
            // generation, so a page any earlier load asked for no longer
            // continues the list, whatever its offset. Within one load, only
            // the later page on its way is taken, and only once.
            ApiResponse::MyPlaylists {
                offset, generation, ..
            } if generation != self.library.playlists_generation
                || (offset > 0 && self.library.playlists_asked != Some(offset)) => {}
            ApiResponse::MyPlaylists { offset, result, .. } => match result {
                Ok(page) => {
                    self.library.playlists_asked = None;
                    let next_offset = page.next_offset();
                    match &mut self.library.playlists {
                        Loadable::Loaded(existing) if offset > 0 => existing.extend(page.items),
                        slot => *slot = Loadable::Loaded(page.items),
                    }
                    self.library.playlists_next = next_offset;
                    if next_offset.is_some() {
                        self.load_more(Page::Home);
                    } else {
                        // Load folder order after all playlists arrive.
                        self.backend.send(Command::Rootlist);
                    }
                    if let Some(playlists) = self.library.playlists.get() {
                        for listed in playlists {
                            self.saved.insert(listed.uri.clone(), true);
                            // A header read over the streaming session
                            // lacks what the list carries; pages that
                            // arrived before the list take it now.
                            if let Some(playlist) = self
                                .playlist_pages
                                .get_mut(&listed.id)
                                .and_then(|page| page.playlist.get_mut())
                            {
                                playlist.fill_from(listed);
                            }
                        }
                    }
                }
                Err(error) => {
                    self.library.playlists_asked = None;
                    if offset == 0 {
                        self.library.playlists = Loadable::Failed(error.to_string());
                    } else {
                        self.toast_error(
                            // Translators: {error} is an error message.
                            gettext(self.locale, "Couldn't load more playlists: {error}")
                                .replace("{error}", &error.to_string()),
                        );
                    }
                }
            },
            ApiResponse::Playlist {
                id,
                generation,
                mut result,
            } => {
                if self
                    .playlist_pages
                    .get(&id)
                    .is_none_or(|page| page.generation != generation || page.pending_writes > 0)
                {
                    return;
                }
                let stale_write_snapshot = self.playlist_pages.get_mut(&id).is_some_and(|page| {
                    let Some(expected) = page.optimistic_snapshot.as_deref() else {
                        return false;
                    };
                    let reported = result
                        .as_ref()
                        .ok()
                        .and_then(|playlist| playlist.snapshot_id.as_deref());
                    if reported == Some(expected) {
                        page.optimistic_snapshot = None;
                        page.snapshot_rechecks = 0;
                        false
                    } else if result.is_ok() {
                        page.snapshot_rechecks = page.snapshot_rechecks.saturating_add(1);
                        true
                    } else {
                        false
                    }
                });
                if stale_write_snapshot {
                    let page = self.playlist_pages.get_mut(&id).unwrap();
                    if page.snapshot_rechecks <= 3 {
                        self.backend.api(ApiRequest::Playlist { id, generation });
                    } else {
                        page.refresh_after_write = false;
                        page.items.fail(
                            gettext(
                                self.locale,
                                "Spotify hasn't confirmed your playlist changes yet. Try refreshing again.",
                            )
                            .into_owned(),
                        );
                    }
                    return;
                }
                if let Ok(playlist) = &mut result {
                    // A header read over the streaming session lacks what
                    // the Web API gave: the account's own name, and the
                    // library list's public flag, owner name, and cover.
                    if playlist.owner.display_name.is_none() {
                        playlist.owner.display_name = self.own_name(playlist.owner.id.as_deref());
                    }
                    if let Some(listed) = self.library_entry(&id) {
                        playlist.fill_from(listed);
                    }
                    if let Some(image) = pick_image(&playlist.images, 64) {
                        self.tint_for(Some(image));
                    }
                }
                let mut refresh_rows = false;
                if let Some(page) = self.playlist_pages.get_mut(&id) {
                    if page.refresh_after_write || page.optimistic_snapshot.is_some() {
                        match &result {
                            Ok(_) => refresh_rows = page.refresh_after_write,
                            Err(error) => {
                                page.refresh_after_write = false;
                                page.items.fail(friendly_page_error(self.locale, error));
                            }
                        }
                    }
                    let old_snapshot = page
                        .playlist
                        .get()
                        .and_then(|playlist| playlist.snapshot_id.as_deref());
                    let new_snapshot = result
                        .as_ref()
                        .ok()
                        .and_then(|playlist| playlist.snapshot_id.as_deref());
                    if old_snapshot.is_some() && old_snapshot != new_snapshot {
                        page.items.clear_windows();
                        page.cache_saved_through = None;
                        page.cache_saved_rows = 0;
                        page.cache_saved_total = None;
                        page.cache_write_pending = None;
                        page.cache_append_valid = false;
                        page.cache_restored_through = None;
                    }
                    if let Ok(playlist) = &result
                        && page.items.loaded_once
                    {
                        page.items.total = Some(playlist.track_total());
                    }
                    page.playlist.refresh(result);
                }
                self.try_adopt_playlist_cache(&id);
                self.checkpoint_playlist_cache(&id);
                if refresh_rows {
                    self.reload(Page::Playlist(id));
                }
            }
            ApiResponse::PlaylistItems {
                id,
                offset,
                generation,
                result,
            } => {
                if self.playlist_pages.get(&id).is_none_or(|page| {
                    page.generation != generation
                        || page.pending_writes > 0
                        || page.optimistic_snapshot.is_some()
                }) {
                    return;
                }
                let mut uris = Vec::new();
                let mut adders: Vec<String> = Vec::new();
                let mut tracks = Vec::new();
                let cache_key = Page::Playlist(id.clone());
                if let Some(page) = self.playlist_pages.get_mut(&id) {
                    match result {
                        _ if page
                            .cache_restored_through
                            .is_some_and(|cached| offset < cached)
                            && page.items.window_request != Some(offset) =>
                        {
                            // The initial request was already in flight when
                            // a longer cached prefix was restored.
                        }
                        Ok(mut items) => {
                            note_availability(&mut self.playlist_availability, &items.items);
                            fill_availability(&self.playlist_availability, &mut items.items);
                            tracks = items
                                .items
                                .iter()
                                .filter_map(|item| match item.playable() {
                                    Some(PlayableItem::Track(track)) => Some(track.clone()),
                                    _ => None,
                                })
                                .collect();
                            uris = items
                                .items
                                .iter()
                                .filter_map(|item| item.playable())
                                .map(|item| item.uri().to_string())
                                .collect();
                            adders = items
                                .items
                                .iter()
                                .filter_map(|item| item.added_by.as_ref()?.id.clone())
                                .filter(|id| !id.is_empty())
                                .collect();
                            page.contributors.extend(adders.iter().cloned());
                            let extends_checkpoint = offset > 0
                                && page.items.base_offset == 0
                                && page.items.loaded_once
                                && page.items.window_request.is_none()
                                && page.items.windows.is_empty()
                                && page.items.next_offset == Some(offset)
                                && page.items.total == Some(items.total)
                                && page.items.items.len() <= offset as usize
                                && page.items_generation == generation;
                            let extends_cached_rows = offset > 0
                                && page.items.loaded_once
                                && page.items.window_request.is_none()
                                && page.items.windows.is_empty()
                                && page.items.next_offset == Some(offset)
                                && page.items.total == Some(items.total)
                                && u32::try_from(page.items.items.len())
                                    .ok()
                                    .and_then(|len| page.items.base_offset.checked_add(len))
                                    == Some(offset)
                                && self.table_rows.get(&cache_key).is_some_and(|cache| {
                                    cache.generation == generation
                                        && cache.items_revision == page.items.revision
                                        && cache.user_names_revision == self.user_names_revision
                                        && cache.playlist_positions.is_some()
                                        && cache.playlist_raw_count == page.items.items.len()
                                });
                            if !extends_checkpoint {
                                page.cache_append_valid = false;
                            }
                            page.items.absorb(offset, items);
                            page.items_generation = generation;
                            if extends_cached_rows
                                && let Some(cache) = self.table_rows.get_mut(&cache_key)
                            {
                                cache.playlist_append_revision = Some(page.items.revision);
                            }
                        }
                        Err(error) => page.items.fail(friendly_page_error(self.locale, &error)),
                    }
                }
                for track in &tracks {
                    self.remember_track_recording(track);
                }
                self.request_contains(uris);
                self.request_user_names(adders);
                self.sample_playlist_tail(&id);
                self.checkpoint_playlist_cache(&id);
                // A sorted table means the whole list, not the loaded part.
                if self.table_sorts.contains_key(&Page::Playlist(id.clone())) {
                    self.load_more(Page::Playlist(id));
                }
            }
            ApiResponse::PlaylistSample {
                id,
                generation,
                result,
            } => {
                if self
                    .playlist_pages
                    .get(&id)
                    .is_none_or(|page| page.generation != generation)
                {
                    return;
                }
                let mut adders: Vec<String> = Vec::new();
                if let Ok(items) = result
                    && let Some(page) = self.playlist_pages.get_mut(&id)
                {
                    adders = items
                        .items
                        .iter()
                        .filter_map(|item| item.added_by.as_ref()?.id.clone())
                        .filter(|id| !id.is_empty())
                        .collect();
                    page.contributors.extend(adders.iter().cloned());
                }
                self.request_user_names(adders);
            }
            ApiResponse::PlaylistCreated(result) => {
                self.playlist_busy = false;
                match result {
                    Ok(playlist) => {
                        self.toast(
                            // Translators: {name} is a playlist name.
                            gettext(self.locale, "Created {name}")
                                .replace("{name}", &playlist.name),
                        );
                        if let Some(playlists) = self.library.playlists.get_mut() {
                            playlists.insert(0, playlist.clone());
                        }
                        self.saved.insert(playlist.uri.clone(), true);
                        if let Some(Dialog::CreatePlaylist { add_uris, .. }) = self.dialog.take()
                            && !add_uris.is_empty()
                        {
                            self.backend.api(ApiRequest::AddToPlaylist {
                                playlist_id: playlist.id.clone(),
                                playlist_name: playlist.name.clone(),
                                uris: add_uris,
                                position: None,
                            });
                        }
                        self.open(Page::Playlist(playlist.id));
                    }
                    Err(error) => {
                        self.toast_error(
                            // Translators: {error} is an error message.
                            gettext(self.locale, "Couldn't create the playlist: {error}")
                                .replace("{error}", &error.to_string()),
                        )
                    }
                }
            }
            ApiResponse::PlaylistCoverUploaded {
                id,
                request,
                previous_urls,
                cover,
                result,
            } => {
                if self.cover_uploads.get(&id) != Some(&request) {
                    return;
                }
                self.cover_uploads.remove(&id);
                if let Some(Dialog::EditPlaylist {
                    id: current,
                    cover: draft,
                    ..
                }) = &mut self.dialog
                    && *current == id
                    && draft.uploading == Some(request)
                {
                    draft.uploading = None;
                    draft.error = result
                        .as_ref()
                        .err()
                        .map(|error| cover_error(self.locale, error));
                    if result.is_ok() {
                        draft.selection = None;
                    }
                }
                match result {
                    Ok(()) => {
                        self.set_playlist_cover(&id, &cover);
                        self.uploaded_covers.insert(
                            id.clone(),
                            crate::playlist_cover::PendingCover {
                                cover,
                                previous_urls,
                                request,
                                checking: None,
                                next_images: None,
                                rechecks_left: 3,
                            },
                        );
                        if let Some(page) = self.playlist_pages.get(&id) {
                            self.backend.api(ApiRequest::Playlist {
                                id,
                                generation: page.generation,
                            });
                        }
                        self.toast(gettext(self.locale, "Playlist cover updated"));
                    }
                    Err(error) => self.toast_error(cover_error(self.locale, &error)),
                }
            }
            ApiResponse::PlaylistUpdated { id, result } => {
                self.playlist_busy = false;
                match result {
                    Ok(()) => {
                        self.toast(gettext(self.locale, "Playlist updated"));
                        self.playlist_pages.remove(&id);
                        self.load_playlists();
                        if matches!(self.page(), Page::Playlist(current) if *current == id) {
                            self.ensure_loaded(Page::Playlist(id));
                        }
                    }
                    Err(error) => {
                        self.toast_error(
                            // Translators: {error} is an error message.
                            gettext(self.locale, "Couldn't update the playlist: {error}")
                                .replace("{error}", &error.to_string()),
                        )
                    }
                }
            }
            ApiResponse::PlaylistDuplicatesChecked {
                playlist_id,
                playlist_name,
                items,
                position,
                result,
            } => match result {
                Ok(duplicate_uris) if duplicate_uris.is_empty() => {
                    self.add_to_playlist_now(playlist_id, playlist_name, items, position)
                }
                Ok(duplicate_uris) => {
                    self.playlist_busy = false;
                    self.dialog = Some(Dialog::ConfirmPlaylistDuplicates {
                        playlist_id,
                        playlist_name,
                        items,
                        position,
                        duplicate_uris,
                    });
                }
                Err(error) => {
                    // A failed read must not take away an edit the account is
                    // still allowed to make. The write reports its own error.
                    log::debug!("could not check playlist for duplicates: {error}");
                    self.add_to_playlist_now(playlist_id, playlist_name, items, position);
                }
            },
            ApiResponse::PlaylistItemsChanged {
                id,
                message,
                result,
            } => {
                self.playlist_busy = false;
                if let Some(page) = self.playlist_pages.get_mut(&id) {
                    page.pending_writes = page.pending_writes.saturating_sub(1);
                }
                match result {
                    Ok(snapshot) => {
                        if !message.is_empty() {
                            self.toast(message);
                        }
                        let mut generation = None;
                        if let Some(page) = self.playlist_pages.get_mut(&id) {
                            if let Some(playlist) = page.playlist.get_mut() {
                                playlist.snapshot_id = snapshot.clone();
                            }
                            if snapshot.is_some() {
                                page.optimistic_snapshot = snapshot.clone();
                                page.snapshot_rechecks = 0;
                            }
                            generation = Some(page.generation);
                        }
                        if let Some(playlists) = self.library.playlists.get_mut() {
                            for playlist in playlists.iter_mut().filter(|p| p.id == id) {
                                playlist.snapshot_id = snapshot.clone();
                            }
                        }
                        self.checkpoint_playlist_cache(&id);
                        // Reconcile metadata in the background. The mutation
                        // response and local rows are enough to keep the page;
                        // no item page is discarded or downloaded again.
                        if let Some(generation) = generation {
                            self.backend.api(ApiRequest::Playlist { id, generation });
                        }
                    }
                    Err(error) => {
                        self.toast_error(
                            // Translators: {error} is an error message.
                            gettext(self.locale, "Playlist change failed: {error}")
                                .replace("{error}", &error.to_string()),
                        );
                        if let Some(page) = self.playlist_pages.get_mut(&id) {
                            page.optimistic_snapshot = None;
                            page.snapshot_rechecks = 0;
                        }
                        self.load_playlist_items_at(&id, 0);
                        if let Some(page) = self.playlist_pages.get(&id) {
                            self.backend.api(ApiRequest::Playlist {
                                id,
                                generation: page.generation,
                            });
                        }
                    }
                }
            }
            ApiResponse::PlaylistFollowChanged {
                id,
                followed,
                result,
            } => match result {
                Ok(()) => {
                    self.saved
                        .insert(format!("spotify:playlist:{id}"), followed);
                    self.toast(if followed {
                        gettext(self.locale, "Added to Your Library")
                    } else {
                        gettext(self.locale, "Removed from Your Library")
                    });
                    self.load_playlists();
                    if !followed && matches!(self.page(), Page::Playlist(current) if *current == id)
                    {
                        self.open(Page::Home);
                    }
                }
                Err(error) => {
                    self.saved
                        .insert(format!("spotify:playlist:{id}"), !followed);
                    self.toast_error(
                        // Translators: {error} is an error message.
                        gettext(self.locale, "Couldn't update the playlist: {error}")
                            .replace("{error}", &error.to_string()),
                    );
                }
            },
            ApiResponse::SavedTracks {
                offset,
                generation,
                account_id,
                result,
            } => {
                if generation != self.liked_songs.generation
                    || self.user_id() != account_id.as_deref()
                {
                    return;
                }
                let mut more = false;
                let succeeded = result.is_ok();
                match result {
                    Ok(page) => {
                        for item in &page.items {
                            let uri = item.track.uri.clone();
                            self.remember_track_recording(&item.track);
                            if !self.saved_writes.contains_key(&uri)
                                && self.liked_songs.intent(&uri).is_none()
                            {
                                self.set_saved_state(uri, true);
                            }
                        }
                        more = self.liked_songs.absorb(
                            offset,
                            page,
                            jiff::Timestamp::now().as_second(),
                        );
                    }
                    Err(error) => self.liked_songs.fail(error.to_string()),
                }
                if !more {
                    self.sync_liked_songs();
                    self.checkpoint_liked_songs(false);
                }
                if succeeded && (more || self.table_sorts.contains_key(&Page::LikedSongs)) {
                    self.load_more(Page::LikedSongs);
                }
            }
            // A reset shelf is read again from the top, so a page asked for
            // before the reset no longer continues it.
            ApiResponse::SavedAlbums { offset, .. }
                if self.library.albums.next_offset != Some(offset) => {}
            ApiResponse::SavedAlbums { offset, result } => match result {
                Ok(page) => {
                    self.request_album_types(page.items.iter().map(|item| &item.album));
                    for item in &page.items {
                        self.saved.insert(item.album.uri.clone(), true);
                    }
                    self.library.albums.absorb(offset, page);
                }
                Err(error) => self.library.albums.fail(error.to_string()),
            },
            ApiResponse::FollowedArtists { after, result } => {
                let list = &mut self.library.artists;
                // A reset shelf is read again from the top, so a page asked
                // for before the reset no longer continues it.
                if after != list.after {
                    return;
                }
                list.loading = false;
                list.loaded_once = true;
                match result {
                    Ok(page) => {
                        if after.is_none() {
                            list.items.clear();
                        }
                        let received = page.items.len();
                        for artist in &page.items {
                            self.saved.insert(artist.uri.clone(), true);
                        }
                        list.items.extend(page.items);
                        let next = page.cursors.and_then(|cursors| cursors.after);
                        list.complete = next.is_none() || received == 0;
                        list.after = next;
                        list.error = None;
                    }
                    Err(error) => list.error = Some(error.to_string()),
                }
            }
            ApiResponse::SavedShows { offset, .. }
                if self.library.shows.next_offset != Some(offset) => {}
            ApiResponse::SavedShows { offset, result } => match result {
                Ok(page) => {
                    for item in &page.items {
                        self.saved.insert(item.show.uri.clone(), true);
                    }
                    let unknown: Vec<String> = page
                        .items
                        .iter()
                        .map(|item| item.show.uri.clone())
                        .filter(|uri| self.audiobooks_requested.insert(uri.clone()))
                        .collect();
                    if !unknown.is_empty() {
                        self.backend.send(Command::AudiobookShows(unknown));
                    }
                    self.library.shows.absorb(offset, page);
                    if offset == 0 && self.home.requested {
                        self.request_home_episodes();
                    }
                }
                Err(error) => self.library.shows.fail(error.to_string()),
            },
            ApiResponse::HomeEpisodes { generation, .. } if generation != self.home.generation => {}
            ApiResponse::HomeEpisodes { result, .. } => match result {
                Ok(podcasts) => self.home.podcasts = podcasts,
                // The shelf is an extra: without an answer it keeps what it
                // showed, or stays hidden.
                Err(error) => log::debug!("podcast episodes for Home unavailable: {error}"),
            },
            ApiResponse::SavedEpisodes { offset, .. }
                if self.library.episodes.next_offset != Some(offset) => {}
            ApiResponse::SavedEpisodes { offset, result } => match result {
                Ok(page) => {
                    for item in &page.items {
                        self.saved.insert(item.episode.uri.clone(), true);
                    }
                    self.library.episodes.absorb(offset, page);
                }
                Err(error) => self.library.episodes.fail(error.to_string()),
            },
            ApiResponse::SavedChanged {
                uris,
                saved,
                result,
            } => {
                let current_uris: Vec<String> = uris
                    .iter()
                    .filter(|uri| {
                        self.saved_writes
                            .get(*uri)
                            .is_none_or(|wanted| *wanted == saved)
                    })
                    .cloned()
                    .collect();
                for uri in &uris {
                    if self.saved_writes.get(uri) == Some(&saved) {
                        self.saved_writes.remove(uri);
                    }
                }
                match result {
                    Ok(()) => {
                        for uri in &current_uris {
                            self.set_saved_state(uri.clone(), saved);
                            match util::uri_kind(uri) {
                                Some("track") => {
                                    if self.liked_songs.intent(uri).is_some() {
                                        self.liked_songs.confirm(uri, saved, true);
                                    } else if !saved {
                                        self.library.liked.retain(|item| item.track.uri != *uri);
                                        if let Some(total) = self.library.liked.total.as_mut() {
                                            *total = total.saturating_sub(1);
                                        }
                                    }
                                }
                                Some("album") => self.library.albums.reset(),
                                Some("artist") => self.library.artists.reset(),
                                Some("show") => self.library.shows.reset(),
                                Some("episode") => self.library.episodes.reset(),
                                _ => {}
                            }
                        }
                        let message = match (
                            current_uris.first().and_then(|uri| util::uri_kind(uri)),
                            saved,
                        ) {
                            (Some("track"), true) => gettext(self.locale, "Added to Liked Songs"),
                            (Some("track"), false) => {
                                gettext(self.locale, "Removed from Liked Songs")
                            }
                            (Some("artist"), true) => gettext(self.locale, "Following artist"),
                            (Some("artist"), false) => gettext(self.locale, "Unfollowed artist"),
                            (_, true) => gettext(self.locale, "Saved to Your Library"),
                            (_, false) => gettext(self.locale, "Removed from Your Library"),
                        };
                        if !current_uris.is_empty() {
                            self.toast(message);
                        }
                    }
                    Err(error) => {
                        for uri in &current_uris {
                            self.set_saved_state(uri.clone(), !saved);
                            self.liked_songs.confirm(uri, saved, false);
                        }
                        if !current_uris.is_empty() {
                            self.toast_error(
                                // Translators: {error} is an error message.
                                gettext(self.locale, "Couldn't update your library: {error}")
                                    .replace("{error}", &error.to_string()),
                            );
                        }
                    }
                }
                if self.liked_songs.cache_checked && self.liked_songs.has_confirmed_changes() {
                    self.sync_liked_songs();
                    self.checkpoint_liked_songs(true);
                    self.liked_recheck_at = Some(Instant::now() + Duration::from_secs(5));
                }
            }
            ApiResponse::Contains { uris, result } => {
                for uri in &uris {
                    self.saved_pending.remove(uri);
                }
                if let Ok(flags) = result {
                    for (uri, flag) in uris.into_iter().zip(flags) {
                        if !self.saved_writes.contains_key(&uri)
                            && self.liked_songs.intent(&uri).is_none()
                        {
                            self.set_saved_state(uri, flag);
                        }
                    }
                }
            }
            ApiResponse::SearchStarted {
                query,
                serial,
                split,
            } => {
                if serial == self.search.serial && query == self.search.committed {
                    self.search.catalogue_pending = true;
                    self.search.playlists_pending = split;
                }
            }
            ApiResponse::SearchPlaylists {
                query,
                serial,
                result,
            } => {
                if serial != self.search.serial || query != self.search.committed {
                    return;
                }
                self.search.playlists_pending = false;
                match result {
                    Ok(page) => {
                        self.search.playlists = Some((serial, page));
                        self.show_search_playlists();
                        self.settings.remember_search(&query);
                        self.settings_dirty = true;
                    }
                    Err(error) => self.search_failed(&gettext(self.locale, "Playlists"), error),
                }
            }
            ApiResponse::Search {
                query,
                serial,
                result,
            } => {
                if serial != self.search.serial || query != self.search.committed {
                    return;
                }
                self.search.catalogue_pending = false;
                match result {
                    Ok(results) => {
                        let uris = results
                            .tracks
                            .iter()
                            .flat_map(|page| page.items.iter())
                            .map(|track| track.uri.clone())
                            .collect();
                        self.request_contains(uris);
                        self.settings.remember_search(&query);
                        self.settings_dirty = true;
                        self.search.results = Loadable::Loaded(results);
                        self.search.results_serial = serial;
                        self.show_search_playlists();
                    }
                    Err(error) => self.search_failed(&gettext(self.locale, "Search"), error),
                }
            }
            ApiResponse::Artist { id, result } => {
                if let Ok(artist) = &result {
                    if let Some(image) = pick_image(&artist.images, 64) {
                        self.tint_for(Some(image));
                    }
                    if let Some(page) = self.artist_pages.get_mut(&id)
                        && page.top_tracks.needs_load()
                    {
                        page.top_tracks = Loadable::Loading;
                        self.backend
                            .api(ApiRequest::ArtistTopTracks { id: id.clone() });
                    }
                }
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    page.artist = Loadable::from_result(result);
                }
            }
            ApiResponse::ArtistTopTracks { id, result } => {
                if let Ok(tracks) = &result {
                    let uris: Vec<String> = tracks.iter().map(|track| track.uri.clone()).collect();
                    self.request_contains(uris);
                }
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    page.top_tracks = Loadable::from_result(result);
                }
            }
            ApiResponse::ArtistAlbums {
                id,
                groups,
                offset,
                result,
            } => {
                if let Ok(albums) = &result {
                    self.request_album_types(albums.items.iter());
                }
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    let list = page.albums.entry(groups).or_default();
                    match result {
                        Ok(albums) => list.absorb(offset, albums),
                        Err(error) => list.fail(error.to_string()),
                    }
                }
            }
            ApiResponse::RelatedArtists { id, result } => {
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    page.related = Loadable::from_result(result);
                }
            }
            ApiResponse::Album { id, result } => {
                let mut uris = Vec::new();
                if let Ok(album) = &result {
                    self.request_album_types(std::iter::once(album));
                }
                if let Ok(album) = &result
                    && let Some(image) = pick_image(&album.images, 64)
                {
                    self.tint_for(Some(image));
                }
                if let Some(page) = self.album_pages.get_mut(&id) {
                    match result {
                        Ok(mut album) => {
                            if let Some(tracks) = album.tracks.take() {
                                uris = tracks.items.iter().map(|track| track.uri.clone()).collect();
                                page.tracks.absorb(0, tracks);
                            }
                            page.album = Loadable::Loaded(album);
                            if !page.tracks.loaded_once {
                                page.tracks.loading = true;
                                self.backend.api(ApiRequest::AlbumTracks {
                                    id,
                                    offset: 0,
                                    generation: page.generation,
                                });
                            }
                        }
                        Err(error) => page.album = Loadable::Failed(error.to_string()),
                    }
                }
                self.request_contains(uris);
            }
            ApiResponse::AlbumTracks {
                id,
                offset,
                generation,
                result,
            } => {
                if self
                    .album_pages
                    .get(&id)
                    .is_none_or(|page| page.generation != generation)
                {
                    return;
                }
                let mut uris = Vec::new();
                if let Some(page) = self.album_pages.get_mut(&id) {
                    match result {
                        Ok(tracks) => {
                            uris = tracks.items.iter().map(|track| track.uri.clone()).collect();
                            page.tracks.absorb(offset, tracks);
                        }
                        Err(error) => page.tracks.fail(error.to_string()),
                    }
                }
                self.request_contains(uris);
                // A sorted table means the whole list, not the loaded part.
                if self.table_sorts.contains_key(&Page::Album(id.clone())) {
                    self.load_more(Page::Album(id));
                }
            }
            ApiResponse::AlbumQueueTracks {
                request,
                offset,
                result,
            } => {
                self.receive_album_queue(request, offset, result);
            }
            ApiResponse::Show { id, result } => {
                if let Ok(show) = &result
                    && let Some(image) = pick_image(&show.images, 64)
                {
                    self.tint_for(Some(image));
                }
                if let Some(page) = self.show_pages.get_mut(&id) {
                    match result {
                        Ok(mut show) => {
                            if let Some(episodes) = show.episodes.take() {
                                page.episodes.absorb(0, episodes);
                            }
                            page.show = Loadable::Loaded(show);
                            if !page.episodes.loaded_once {
                                page.episodes.loading = true;
                                self.backend.api(ApiRequest::ShowEpisodes { id, offset: 0 });
                            }
                        }
                        Err(error) => page.show = Loadable::Failed(error.to_string()),
                    }
                }
            }
            ApiResponse::ShowEpisodes { id, offset, result } => {
                if let Some(page) = self.show_pages.get_mut(&id) {
                    match result {
                        Ok(episodes) => page.episodes.absorb(offset, episodes),
                        Err(error) => page.episodes.fail(error.to_string()),
                    }
                }
            }
            ApiResponse::Track { id, result } => {
                self.track_requests.remove(&id);
                match result {
                    Ok(track) => {
                        self.remember_track_recording(&track);
                        self.request_recording_candidates(&track);
                        if self.liked_songs.update_track(&track) {
                            self.sync_liked_songs();
                        }
                        self.resolve_pasted_song(
                            &format!("spotify:track:{id}"),
                            Some(PlayableItem::Track(track.clone())),
                        );
                        self.track_cache.insert(id.clone(), track);
                        self.track_used.insert(id, Instant::now());
                    }
                    Err(error) => {
                        self.resolve_pasted_song(&format!("spotify:track:{id}"), None);
                        if self.pending_link.as_deref()
                            == Some(format!("spotify:track:{id}").as_str())
                        {
                            self.pending_link = None;
                            self.toast_error(
                                // Translators: {error} is an error message.
                                gettext(self.locale, "Cannot open this song: {error}")
                                    .replace("{error}", &error.to_string()),
                            );
                        }
                    }
                }
            }
            // Only a link asks for an episode; its podcast's page is the
            // nearest thing to a page for it.
            ApiResponse::Episode { result, .. } => match result {
                Ok(episode) => match episode.show.filter(|show| !show.id.is_empty()) {
                    Some(show) => self.open(Page::Show(show.id)),
                    None => self.toast_error(gettext(
                        self.locale,
                        "This episode's podcast is not on Spotify",
                    )),
                },
                Err(error) => self.toast_error(
                    // Translators: {error} is an error message.
                    gettext(self.locale, "Cannot open this episode: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            ApiResponse::Remote { action, result } => {
                if matches!(action, RemoteAction::Play | RemoteAction::Pause) {
                    self.clear_play_pending();
                }
                match result {
                    Ok(()) => {
                        self.remote_recheck_at = Some(Instant::now() + REMOTE_RECHECK);
                        if action == RemoteAction::Shuffle {
                            self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
                            if let Some(pending) = &mut self.queue_shuffle_pending {
                                pending.at = Instant::now();
                            } else {
                                self.note_shuffle_pending();
                            }
                        }
                    }
                    Err(error) => {
                        self.optimistic_playing = None;
                        self.pending_remote_position = None;
                        self.pending_remote_volume = None;
                        if matches!(
                            action,
                            RemoteAction::Play | RemoteAction::Next | RemoteAction::Previous
                        ) && self.intent_track.as_ref().is_some_and(|intent| {
                            matches!(intent.confirmation, TrackConfirmation::Remote { .. })
                        }) {
                            self.intent_track = None;
                        }
                        let hint = if error.status() == Some(404) {
                            format!(
                                " {}",
                                gettext(
                                    self.locale,
                                    "Choose a device from the devices menu first."
                                )
                            )
                        } else {
                            String::new()
                        };
                        self.toast_error(format!(
                            "{}: {error}.{hint}",
                            remote_action_label(self.locale, action)
                        ));
                    }
                }
                self.poll_remote_soon();
            }
            ApiResponse::Transferred { device_id, result } => match result {
                Ok(()) => {
                    self.selected_device = Some(device_id);
                    self.show_devices = false;
                    self.poll_remote_soon();
                    self.refresh_devices();
                }
                Err(error) => self.toast_error(
                    // Translators: {error} is an error message.
                    gettext(self.locale, "Couldn't switch device: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            ApiResponse::QueueAdded { label: _, result } => match result {
                Ok(()) => {
                    // Refresh the queue after the optimistic update.
                    self.refresh_queue(true);
                }
                Err(error) => self.toast_error(
                    // Translators: {error} is an error message.
                    gettext(self.locale, "Couldn't add to queue: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            ApiResponse::QueueBatchAdded {
                request,
                added,
                result,
            } => {
                if self.pending_queue_batches.remove(&request).as_ref() != Some(&self.target()) {
                    return;
                }
                if result.is_err() {
                    self.remove_failed_queue_adds(request, added);
                }
                for addition in &mut self.pending_queue_adds {
                    if addition.write.is_some_and(|(write, _)| write == request) {
                        addition.write = None;
                        addition.at = Instant::now();
                    }
                }
                if let Err(error) = result {
                    self.toast_error(
                        // Translators: {error} is an error message.
                        gettext(self.locale, "Couldn't add to queue: {error}")
                            .replace("{error}", &error.to_string()),
                    );
                }
                self.refresh_queue(true);
            }
        }
    }

    fn poll_remote_soon(&mut self) {
        self.remote_polled_at = Instant::now() - REMOTE_POLL_IDLE + Duration::from_millis(700);
    }

    // ---- navigation ------------------------------------------------------------

    pub fn open(&mut self, page: Page) {
        self.touch_page(&page);
        if *self.page() == page {
            self.ensure_loaded(page.clone());
            self.retain_table_rows(&page);
            return;
        }
        self.history.truncate(self.history_index + 1);
        self.history.push(page.clone());
        if self.history.len() > 60 {
            self.history.remove(0);
        }
        self.history_index = self.history.len() - 1;
        self.show_devices = false;
        self.ensure_loaded(page.clone());
        self.retain_table_rows(&page);
        self.evict_stale_pages();
    }

    /// Hands the app a Spotify link from outside, a canonical URI as
    /// [`crate::link::parse`] makes it: the window comes forward and the
    /// page opens once the account is signed in.
    pub fn open_link(&mut self, uri: String) {
        self.actions.push(Action::OpenLink(uri));
    }

    /// Opens the page behind the pending link once the account is signed
    /// in. A song and an episode have no page of their own here, so the
    /// album's and the podcast's open, once Spotify has said which.
    fn open_pending_link(&mut self) {
        let Some(uri) = self.pending_link.clone() else {
            return;
        };
        if self.user.is_none() {
            return;
        }
        if let Some(query) = crate::link::search_query(&uri) {
            self.pending_link = None;
            self.actions.push(Action::Search(query));
            self.actions.push(Action::FocusSearch);
            return;
        }
        if let Some(page) = Page::from_uri(&uri) {
            self.pending_link = None;
            self.open(page);
            return;
        }
        let id = util::uri_id(&uri).unwrap_or_default().to_string();
        match util::uri_kind(&uri) {
            Some("track") => {
                if let Some(track) = self.read_cached_track(&id) {
                    self.pending_link = None;
                    let album = track
                        .album
                        .as_ref()
                        .map(|album| album.id.clone())
                        .filter(|id| !id.is_empty());
                    match album {
                        Some(album) => self.open(Page::Album(album)),
                        None => self.toast_error(gettext(
                            self.locale,
                            "This song's album is not on Spotify",
                        )),
                    }
                } else if self.track_requests.insert(id.clone()) {
                    // The answer lands in the cache, and the link waits
                    // for the next frame to find it there.
                    self.backend.api(ApiRequest::Track { id });
                }
            }
            Some("episode") => {
                self.pending_link = None;
                self.backend.api(ApiRequest::Episode { id });
            }
            _ => {
                self.pending_link = None;
                self.toast_error(gettext(
                    self.locale,
                    "Spotifast cannot open this kind of Spotify link",
                ));
            }
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.history_index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.history_index + 1 < self.history.len()
    }

    fn touch_page(&mut self, page: &Page) {
        self.page_used.insert(page.clone(), Instant::now());
    }

    fn read_cached_track(&mut self, id: &str) -> Option<Track> {
        let track = self.track_cache.get(id).cloned()?;
        self.track_used.insert(id.to_owned(), Instant::now());
        Some(track)
    }

    /// Drops page caches that exceed the cap, keeping the open page, the
    /// playing context, and the most recently used pages. History does not
    /// protect a page from the cap. Track metadata is an LRU of 800.
    /// Table-row copies of dropped playlist and album pages go with them.
    fn evict_stale_pages(&mut self) {
        const MAX_PLAYLIST_PAGES: usize = 12;
        const MAX_ALBUM_PAGES: usize = 16;
        const MAX_ARTIST_PAGES: usize = 10;
        const MAX_SHOW_PAGES: usize = 8;
        const MAX_RADIO_PAGES: usize = 6;
        const MAX_TRACK_CACHE: usize = 800;

        let mut protected_playlists = HashSet::new();
        for (id, page) in &self.playlist_pages {
            if page.pending_writes > 0 || page.optimistic_snapshot.is_some() {
                protected_playlists.insert(id.clone());
            }
        }
        let mut protected_albums = HashSet::new();
        let mut protected_artists = HashSet::new();
        let mut protected_shows = HashSet::new();
        let mut protected_radios = HashSet::new();
        if let Some(seed) = self
            .playing_context_uri()
            .and_then(|uri| util::station_seed(&uri))
        {
            protected_radios.insert(seed);
        }
        match self.page() {
            Page::Playlist(id) => {
                protected_playlists.insert(id.clone());
            }
            Page::Album(id) => {
                protected_albums.insert(id.clone());
            }
            Page::Artist(id) => {
                protected_artists.insert(id.clone());
            }
            Page::Show(id) => {
                protected_shows.insert(id.clone());
            }
            Page::Radio(seed) => {
                protected_radios.insert(seed.clone());
            }
            _ => {}
        }
        if let Some(uri) = self.playing_context_uri()
            && let Some(kind) = util::uri_kind(&uri)
            && let Some(id) = util::uri_id(&uri)
        {
            match kind {
                "playlist" => {
                    protected_playlists.insert(id.to_string());
                }
                "album" => {
                    protected_albums.insert(id.to_string());
                }
                "artist" => {
                    protected_artists.insert(id.to_string());
                }
                "show" => {
                    protected_shows.insert(id.to_string());
                }
                _ => {}
            }
        }
        evict_lru_map(
            &mut self.playlist_pages,
            &self.page_used,
            |id| Page::Playlist(id.to_string()),
            &protected_playlists,
            MAX_PLAYLIST_PAGES,
        );
        evict_lru_map(
            &mut self.album_pages,
            &self.page_used,
            |id| Page::Album(id.to_string()),
            &protected_albums,
            MAX_ALBUM_PAGES,
        );
        evict_lru_map(
            &mut self.artist_pages,
            &self.page_used,
            |id| Page::Artist(id.to_string()),
            &protected_artists,
            MAX_ARTIST_PAGES,
        );
        evict_lru_map(
            &mut self.show_pages,
            &self.page_used,
            |id| Page::Show(id.to_string()),
            &protected_shows,
            MAX_SHOW_PAGES,
        );
        evict_lru_map(
            &mut self.radio_pages,
            &self.page_used,
            |seed| Page::Radio(seed.to_string()),
            &protected_radios,
            MAX_RADIO_PAGES,
        );
        self.page_used.retain(|page, _| match page {
            Page::Playlist(id) => self.playlist_pages.contains_key(id),
            Page::Album(id) => self.album_pages.contains_key(id),
            Page::Artist(id) => self.artist_pages.contains_key(id),
            Page::Show(id) => self.show_pages.contains_key(id),
            Page::Radio(seed) => self.radio_pages.contains_key(seed),
            _ => true,
        });
        if self.track_cache.len() > MAX_TRACK_CACHE {
            let playing = self.now_playing().and_then(|now| now.id);
            let overflow = self.track_cache.len() - MAX_TRACK_CACHE;
            let mut victims: Vec<(Option<Instant>, String)> = self
                .track_cache
                .keys()
                .filter(|id| playing.as_ref() != Some(*id))
                .map(|id| {
                    let used = self.track_used.get(id).copied();
                    (used, id.clone())
                })
                .collect();
            victims.sort();
            for (_, id) in victims.into_iter().take(overflow) {
                self.track_cache.remove(&id);
                self.track_used.remove(&id);
            }
        }
        let current = self.page().clone();
        self.retain_table_rows(&current);
    }

    // ---- playback --------------------------------------------------------------

    fn remote(&mut self, action: RemoteAction, device_id: Option<String>) {
        if device_id.is_none() && self.remote_fresh().is_none() {
            // Spotify would only answer "no active device found".
            self.clear_play_pending();
            self.toast(gettext(
                self.locale,
                "Nothing is playing. Pick something first",
            ));
            return;
        }
        self.backend.api(ApiRequest::Remote {
            action,
            device_id,
            play: None,
            position_ms: 0,
            percent: 0,
            flag: false,
            repeat: String::new(),
        });
    }

    /// Remembers `uri` as the most recently played context, for the
    /// sidebar's order.
    fn note_recent_context(&mut self, uri: &str) {
        self.session_dirty = true;
        if !Self::is_sidebar_context(uri) {
            return;
        }
        self.recent_contexts.retain(|held| held != uri);
        self.recent_contexts.insert(0, uri.to_string());
        self.recent_contexts.truncate(RECENT_CONTEXTS_KEPT);
    }

    /// Whether the sidebar lists `uri`, so its order has a place for it.
    fn is_sidebar_context(uri: &str) -> bool {
        uri.contains(":playlist:") || uri.contains(":album:") || uri.contains(":collection")
    }

    /// Notes the contexts of a page of history older than every play the
    /// order already holds. They go after it, newest first, and a context
    /// already in the order keeps the place a newer play gave it.
    fn note_older_contexts(&mut self, history: &[crate::api::models::PlayHistory]) {
        for play in history {
            let Some(uri) = play.context.as_ref().map(|context| context.uri.as_str()) else {
                continue;
            };
            if self.recent_contexts.len() >= RECENT_CONTEXTS_KEPT {
                break;
            }
            if Self::is_sidebar_context(uri) && !self.recent_contexts.iter().any(|held| held == uri)
            {
                self.recent_contexts.push(uri.to_string());
                self.session_dirty = true;
            }
        }
    }

    /// Notes every context in a page of play history, oldest first, so
    /// the newest ends up at the front of the sidebar's order.
    fn note_recent_contexts(&mut self, history: &[crate::api::models::PlayHistory]) {
        let contexts: Vec<String> = history
            .iter()
            .rev()
            .filter_map(|play| play.context.as_ref().map(|context| context.uri.clone()))
            .collect();
        for context in contexts {
            self.note_recent_context(&context);
        }
    }

    /// Merges local and Spotify history for the Recent tab.
    fn rebuild_recents(&mut self) {
        self.recents_view = crate::history::merged(self.plays.plays(), &self.recents.items);
    }

    /// Adds a page of play history to the Recents tab.
    ///
    /// Repeated plays remain separate. Only identical track-and-time entries
    /// are deduplicated across page boundaries.
    ///
    /// Spotify paginates backwards with `before`; a short page ends the list.
    fn absorb_recents(
        &mut self,
        page: crate::api::models::CursorPage<crate::api::models::PlayHistory>,
        limit: u32,
    ) {
        // A page asked for with a cursor is older than the pages before it.
        if self.recents.after.is_some() {
            self.note_older_contexts(&page.items);
        } else {
            self.note_recent_contexts(&page.items);
        }
        self.recents.error = None;
        let short_page = (page.items.len() as u32) < limit;
        let cursor = page.cursors.as_ref().and_then(|c| c.before.clone());
        let mut seen: std::collections::HashSet<(String, Option<String>)> = self
            .recents
            .items
            .iter()
            .map(|play| (play.track.uri.clone(), play.played_at.clone()))
            .collect();
        let fresh: Vec<crate::api::models::PlayHistory> = page
            .items
            .into_iter()
            .filter(|play| seen.insert((play.track.uri.clone(), play.played_at.clone())))
            .collect();
        let uris: Vec<String> = fresh.iter().map(|play| play.track.uri.clone()).collect();
        self.recents.items.extend(fresh);
        match cursor {
            Some(cursor) if !short_page => self.recents.after = Some(cursor),
            _ => {
                self.recents.complete = true;
                self.recents.after = None;
            }
        }
        // Request saved state only for newly added rows.
        if !uris.is_empty() {
            self.request_contains(uris);
        }
        self.rebuild_recents();
    }

    /// Loaded track URIs for a context, in display order.
    fn context_track_uris(&self, context_uri: &str) -> Option<Vec<String>> {
        let uris: Vec<String> = if let Some(id) = context_uri.strip_prefix("spotify:playlist:") {
            self.playlist_pages
                .get(id)?
                .items
                .items
                .iter()
                .filter_map(|item| item.playable())
                .map(|item| item.uri().to_string())
                .collect()
        } else if let Some(id) = context_uri.strip_prefix("spotify:album:") {
            self.album_pages
                .get(id)?
                .tracks
                .items
                .iter()
                .filter(|track| !track.uri.is_empty())
                .map(|track| track.uri.clone())
                .collect()
        } else if context_uri.ends_with(":collection") {
            self.library
                .liked
                .items
                .iter()
                .map(|item| item.track.uri.clone())
                .collect()
        } else {
            return None;
        };
        (!uris.is_empty()).then_some(uris)
    }

    fn random_track_in(&self, context_uri: &str) -> Option<String> {
        let uris = self.context_track_uris(context_uri)?;
        Some(uris[rand::random_range(0..uris.len())].clone())
    }

    /// Last known context length from the library.
    /// Used to choose a random shuffle offset before rows are loaded.
    fn context_len(&self, context_uri: &str) -> Option<u32> {
        if context_uri.ends_with(":collection") {
            return self.library.liked.total;
        }
        match util::uri_kind(context_uri)? {
            "playlist" => self
                .library
                .playlists
                .get()?
                .iter()
                .find(|playlist| playlist.uri == context_uri)?
                .tracks
                .as_ref()
                .map(|tracks| tracks.total),
            "album" => {
                self.library
                    .albums
                    .items
                    .iter()
                    .find(|saved| saved.album.uri == context_uri)?
                    .album
                    .total_tracks
            }
            "show" => {
                self.library
                    .shows
                    .items
                    .iter()
                    .find(|saved| saved.show.uri == context_uri)?
                    .show
                    .total_episodes
            }
            _ => None,
        }
    }

    /// Where a shuffled play of `context_uri` begins, as an offset by
    /// track URI or by position: a random one of the rows the app holds,
    /// or, when it holds none, a random position within the length the
    /// library knows. Neither, and the play carries no offset at all:
    /// librespot then picks its own random track, and only the Web API,
    /// which would start at track one, needs telling where to go.
    fn shuffle_start(&self, context_uri: &str) -> (Option<String>, Option<u32>) {
        if let Some(uri) = self.random_track_in(context_uri) {
            return (Some(uri), None);
        }
        if matches!(self.target(), Target::Remote(Some(_)))
            && let Some(len) = self.context_len(context_uri)
            && len > 0
        {
            return (None, Some(rand::random_range(0..len)));
        }
        (None, None)
    }

    /// Start a playlist at its first available row, not an unspecified place
    /// in the player's resolved context. A loaded range from the middle is
    /// not the playlist's beginning; without its prefix, request position zero.
    fn playlist_start(&self, id: &str) -> (Option<String>, Option<u32>) {
        let first = self
            .playlist_pages
            .get(id)
            .filter(|page| page.items.base_offset == 0)
            .and_then(|page| {
                page.items.items.iter().find_map(|row| {
                    let item = row.playable()?;
                    if row.is_local
                        || item.uri().is_empty()
                        || matches!(item, PlayableItem::Track(track)
                            if track.is_local || track.is_playable == Some(false))
                    {
                        return None;
                    }
                    Some(item.uri().to_string())
                })
            });
        match first {
            Some(uri) => (Some(uri), None),
            None => (None, Some(0)),
        }
    }

    /// With `shuffle_first`, shuffle is turned on before playback starts,
    /// in one ordered exchange: two independent requests race, and shuffle
    /// sometimes lost.
    fn play_request(&mut self, request: PlayRequest, shuffle_first: bool) {
        // Shuffle applies across contexts until disabled. A selected row still
        // starts first; otherwise choose a random starting track.
        let mut request = request;
        self.queue_shuffle_pending = None;
        if shuffle_first {
            self.shuffle_wanted = true;
            self.shuffle_set_at = Some(Instant::now());
            self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
        }
        let shuffle = shuffle_first || self.shuffle_wanted;
        if request.offset_uri.is_none()
            && request.offset_position.is_none()
            && request.uris.is_empty()
            && let Some(context) = request.context_uri.as_deref()
        {
            if shuffle {
                (request.offset_uri, request.offset_position) = self.shuffle_start(context);
            } else if let Some(id) = context.strip_prefix("spotify:playlist:") {
                (request.offset_uri, request.offset_position) = self.playlist_start(id);
            }
        }
        let mut keys: Vec<String> = Vec::new();
        if let Some(context) = &request.context_uri {
            keys.push(context.clone());
        }
        if let Some(offset) = &request.offset_uri {
            keys.push(offset.clone());
        }
        match request.offset_position {
            // The play starts at a chosen row; only that row is starting.
            Some(position) => {
                if let Some(uri) = request.uris.get(position as usize) {
                    keys.push(uri.clone());
                }
            }
            // No chosen row: the list starts at its first song.
            None if request.offset_uri.is_none() => {
                if let Some(first) = request.uris.first() {
                    keys.push(first.clone());
                }
            }
            None => {}
        }
        let expected_track = keys.iter().find(|key| key.contains(":track:")).cloned();
        if let Some(uri) = expected_track {
            if let Some(context) = &request.context_uri {
                self.cache_track_from_context(context, &uri);
            }
            self.expect_track(uri, request.position_ms);
        } else {
            self.intent_track = None;
        }
        self.set_play_pending(keys);
        if let Some(context) = request.context_uri.clone() {
            self.note_recent_context(&context);
        }
        // Show a context as playing at once, or clear the previous context
        // for a plain track list. Spotify's state catches up behind it.
        self.assumed_context = Some(AssumedContext {
            uri: request.context_uri.clone().unwrap_or_default(),
            shuffle: shuffle.then_some(true),
            at: Instant::now(),
        });
        self.queue_start_pending = Some(self.target());
        match self.target() {
            Target::Local if !self.local.connected => {
                // Hold the request while the local engine reconnects.
                self.queued_play = Some(request);
            }
            Target::Local => {
                self.queued_play = None;
                let mut load = local_load(&request, shuffle);
                load.repeat = Some(self.local.repeat);
                self.local_list = load.context_uri.is_none().then(|| load.uris.clone());
                let shuffle_after = shuffle && load.shuffle.is_none() && !load.uris.is_empty();
                self.backend.player(PlayerCommand::Load(load));
                if shuffle_after {
                    self.backend.player(PlayerCommand::Shuffle(true));
                }
                self.optimistic_playing = Some((true, Instant::now()));
            }
            Target::Remote(Some(device_id)) => {
                self.queued_play = None;
                if shuffle {
                    self.backend.api(ApiRequest::ShufflePlay {
                        device_id: Some(device_id),
                        play: request,
                    });
                } else {
                    self.backend.api(ApiRequest::Remote {
                        action: RemoteAction::Play,
                        device_id: Some(device_id),
                        play: Some(request),
                        position_ms: 0,
                        percent: 0,
                        flag: false,
                        repeat: String::new(),
                    });
                }
                self.optimistic_playing = Some((true, Instant::now()));
            }
            Target::Remote(None) => {
                // No remote device is active, and this computer's player is
                // not ready. Never ask Spotify to play "nowhere": either
                // wait for the connecting engine or ask for a device.
                if matches!(
                    self.local_playback,
                    LocalPlayback::Connecting | LocalPlayback::Authorizing
                ) || (self.settings.playback_authorized
                    && matches!(self.auth, AuthStatus::Starting | AuthStatus::Connecting))
                {
                    self.queued_play = Some(request);
                } else {
                    self.queue_start_pending = None;
                    self.clear_play_pending();
                    self.queued_play = None;
                    self.toast(gettext(
                        self.locale,
                        "Choose a device, or enable playback on this computer",
                    ));
                    self.show_devices = true;
                    self.refresh_devices();
                }
            }
        }
    }

    /// A playlist's disk cache has been read. Whether or not the page
    /// still matches it, what the Web API said about each song's
    /// availability holds, for rows already shown and rows to come.
    fn receive_playlist_cache(
        &mut self,
        account_id: &str,
        id: &str,
        generation: u64,
        mut cache: Option<PlaylistCache>,
    ) {
        if self.user_id() != Some(account_id) {
            return;
        }
        if let Some(page) = self.playlist_pages.get_mut(id) {
            if page.generation != generation {
                return;
            }
            page.cache_checked = true;
            if let Some(cache) = &mut cache {
                for (uri, playable) in known_availability(&cache.items) {
                    self.playlist_availability
                        .entry(uri.to_string())
                        .or_insert(playable);
                }
                // A cache may arrive after a fresh answer from another page.
                // Correct its flags before this prefix can be adopted.
                for row in &mut cache.items {
                    if let Some(PlayableItem::Track(track)) = row.item.as_mut()
                        && let Some(playable) = self.playlist_availability.get(&track.uri)
                    {
                        if track.is_playable != Some(*playable) {
                            cache.appendable = false;
                        }
                        track.is_playable = Some(*playable);
                    }
                }
                if fill_availability(&self.playlist_availability, &mut page.items.items) {
                    page.items.revision = page.items.revision.wrapping_add(1);
                    page.cache_append_valid = false;
                }
            }
            page.pending_cache = cache;
        }
        self.try_adopt_playlist_cache(id);
        self.checkpoint_playlist_cache(id);
    }

    fn receive_playlist_cache_stored(
        &mut self,
        account_id: &str,
        id: &str,
        generation: u64,
        snapshot: &str,
        success: bool,
    ) {
        if self.user_id() != Some(account_id) {
            return;
        }
        let should_check_again = self.playlist_pages.get_mut(id).is_some_and(|page| {
            let Some(pending) = page.cache_write_pending.take() else {
                return false;
            };
            if pending.generation != generation || pending.snapshot != snapshot {
                page.cache_write_pending = Some(pending);
                return false;
            }
            let current = page.generation == generation
                && page.items_generation == generation
                && page.pending_writes == 0
                && page
                    .playlist
                    .get()
                    .and_then(|playlist| playlist.snapshot_id.as_deref())
                    == Some(snapshot);
            if !current {
                page.cache_append_valid = false;
                return true;
            }
            if success && page.cache_append_valid {
                page.cache_saved_through = Some(pending.through);
                page.cache_saved_rows = pending.rows;
                page.cache_saved_total = Some(pending.total);
            } else if !success {
                page.cache_append_valid = false;
            }
            success || !pending.replacing
        });
        if should_check_again {
            self.checkpoint_playlist_cache(id);
        }
    }

    /// Adopt a playlist's cached prefix once Spotify confirms its snapshot.
    fn try_adopt_playlist_cache(&mut self, id: &str) {
        let mut uris = Vec::new();
        let mut adders: Vec<String> = Vec::new();
        let mut tracks = Vec::new();
        if let Some(page) = self.playlist_pages.get_mut(id) {
            if page.pending_writes > 0 {
                return;
            }
            let Some(snapshot_now) = page
                .playlist
                .get()
                .and_then(|playlist| playlist.snapshot_id.clone())
            else {
                return;
            };
            let confirmed_total = page.playlist.get().and_then(|playlist| {
                playlist
                    .items_count
                    .as_ref()
                    .or(playlist.tracks.as_ref())
                    .map(|count| count.total)
            });
            match &page.pending_cache {
                Some(cache)
                    if cache.snapshot == snapshot_now
                        && confirmed_total.is_none_or(|total| cache.total == total) => {}
                Some(_) => {
                    // A revision alone cannot validate an inconsistent cache.
                    // Let the live item request establish the rows and order.
                    page.pending_cache = None;
                    return;
                }
                None => return,
            }
            let Some(cache) = page.pending_cache.take() else {
                return;
            };
            let cached_through = cache.next_offset.unwrap_or(cache.total);
            let cached_rows = cache.items.len();
            let cached_total = cache.total;
            let cache_appendable = cache.appendable;
            let loaded_through = if page.items.base_offset == 0 {
                page.items.items.len() as u32
            } else {
                page.items
                    .windows
                    .get(&0)
                    .map_or(0, |items| items.len() as u32)
            };
            if loaded_through >= cached_through {
                page.cache_saved_through = Some(cached_through);
                page.cache_saved_rows = cached_rows;
                page.cache_saved_total = Some(cached_total);
                page.cache_append_valid = false;
                page.pending_cache = None;
                return;
            }
            uris = cache
                .items
                .iter()
                .filter_map(|item| item.playable())
                .map(|item| item.uri().to_string())
                .collect();
            tracks = cache
                .items
                .iter()
                .filter_map(|item| match item.playable() {
                    Some(PlayableItem::Track(track)) => Some(track.clone()),
                    _ => None,
                })
                .collect();
            adders = cache
                .items
                .iter()
                .filter_map(|item| item.added_by.as_ref()?.id.clone())
                .filter(|id| !id.is_empty())
                .collect();
            page.contributors.extend(adders.iter().cloned());
            page.items
                .adopt_cached_prefix(cache.items, cache.total, cache.next_offset);
            page.items_generation = page.generation;
            page.cache_saved_through = Some(cached_through);
            page.cache_saved_rows = cached_rows;
            page.cache_saved_total = Some(cached_total);
            page.cache_append_valid = cache_appendable;
            page.cache_restored_through = Some(cached_through);
        }
        for track in &tracks {
            self.remember_track_recording(track);
        }
        self.request_contains(uris);
        self.request_user_names(adders);
        self.sample_playlist_tail(id);
        if self
            .table_sorts
            .contains_key(&Page::Playlist(id.to_string()))
        {
            self.load_more(Page::Playlist(id.to_string()));
        }
    }

    /// Read the final page once for collaborators when the loaded rows do not
    /// already include it. This also runs after a disk cache wins startup.
    fn sample_playlist_tail(&mut self, id: &str) {
        let request = self.playlist_pages.get_mut(id).and_then(|page| {
            if page.tail_checked || !page.items.loaded_once {
                return None;
            }
            page.tail_checked = true;
            let loaded_end = page
                .items
                .base_offset
                .saturating_add(page.items.items.len().try_into().unwrap_or(u32::MAX));
            let total = page.items.total.filter(|total| *total > loaded_end)?;
            Some(ApiRequest::PlaylistSample {
                id: id.to_string(),
                offset: total.saturating_sub(PLAYLIST_PAGE_SIZE),
                generation: page.generation,
            })
        });
        if let Some(request) = request {
            self.backend.api(request);
        }
    }

    /// Save the first page, every ten pages after that, and the completed list.
    /// Only rows added since the confirmed checkpoint are copied on appends.
    fn checkpoint_playlist_cache(&mut self, id: &str) {
        const CHECKPOINT_ITEMS: u32 = PLAYLIST_PAGE_SIZE * 10;

        let command = self.playlist_pages.get_mut(id).and_then(|page| {
            let snapshot = page
                .playlist
                .get()
                .and_then(|playlist| playlist.snapshot_id.clone())?;
            if !page.items.loaded_once
                || page.items.items.is_empty()
                || page.items.base_offset != 0
                || page.items_generation != page.generation
                || page.pending_writes > 0
                || page.cache_write_pending.is_some()
            {
                return None;
            }
            if !page.cache_checked {
                return None;
            }
            let total = page.items.total?;
            let next_offset = page.items.next_offset;
            let through = next_offset.unwrap_or(total);
            let previous = page.cache_saved_through.unwrap_or(0);
            let complete = next_offset.is_none();
            let row_count = page.items.items.len();
            if page.cache_append_valid
                && page.cache_saved_through == Some(through)
                && page.cache_saved_rows == row_count
                && page.cache_saved_total == Some(total)
            {
                return None;
            }
            if previous > 0 && through.saturating_sub(previous) < CHECKPOINT_ITEMS && !complete {
                return None;
            }
            let replacing = !page.cache_append_valid
                || page.cache_saved_through.is_none()
                || page.cache_saved_total != Some(total)
                || page.cache_saved_rows > row_count;
            let rows = if replacing {
                PlaylistCacheRows::Replace(page.items.items.clone())
            } else {
                PlaylistCacheRows::Append {
                    previous_rows: page.cache_saved_rows,
                    previous_offset: previous,
                    items: page.items.items[page.cache_saved_rows..].to_vec(),
                }
            };
            page.cache_write_pending = Some(PlaylistCachePending {
                generation: page.generation,
                snapshot: snapshot.clone(),
                through,
                rows: row_count,
                total,
                replacing,
            });
            if replacing {
                page.cache_append_valid = true;
            }
            Some(Command::StorePlaylistCache {
                id: id.to_string(),
                generation: page.generation,
                snapshot,
                rows,
                total,
                next_offset,
            })
        });
        if let Some(command) = command {
            self.backend.send(command);
        }
    }

    /// Play what was playing when the app last closed. `false` when
    /// nothing is known to resume.
    fn resume_last(&mut self) -> bool {
        let Some(track) = self.resume_track.clone() else {
            return false;
        };
        let mut request = match self.resume_context.clone() {
            Some(context) => PlayRequest::context(context).starting_at_uri(track),
            None => PlayRequest::tracks(vec![track]),
        };
        request.position_ms = self.resume_position_ms;
        self.play_request(request, false);
        true
    }

    fn toggle_play(&mut self) {
        let playing = self.now_playing().map(|now| now.playing);
        match self.target() {
            Target::Local => {
                if self.local.is_active() {
                    self.backend.player(PlayerCommand::Toggle);
                } else if let Some(remote) = self.remote_fresh() {
                    // Nothing is playing locally: resume on this computer.
                    let uri = remote
                        .state
                        .item
                        .as_ref()
                        .map(|item| item.uri().to_string());
                    let position = remote.state.progress_ms.unwrap_or(0);
                    if let Some(uri) = uri {
                        let mut request = match &remote.state.context {
                            Some(context) if !context.uri.is_empty() => {
                                PlayRequest::context(context.uri.clone()).starting_at_uri(uri)
                            }
                            _ => PlayRequest::tracks(vec![uri]),
                        };
                        request.position_ms = position;
                        self.play_request(request, false);
                        return;
                    }
                    if !self.resume_last() {
                        self.toast(gettext(self.locale, "Pick something to play"));
                    }
                    return;
                } else {
                    if !self.resume_last() {
                        self.toast(gettext(self.locale, "Pick something to play"));
                    }
                    return;
                }
            }
            Target::Remote(device_id) => {
                if device_id.is_none() && self.remote_fresh().is_none() {
                    // Nothing is known to be playing anywhere, which is how
                    // a fresh start looks before the local engine is ready:
                    // pick up where the last run left off, the way the
                    // local branch does. The engine plays it once it is up.
                    if !self.resume_last() {
                        self.toast(gettext(self.locale, "Pick a song, album, or playlist"));
                    }
                    return;
                }
                self.set_play_pending(vec!["::toggle".into()]);
                if playing == Some(true) {
                    self.remote(RemoteAction::Pause, device_id);
                } else {
                    self.remote(RemoteAction::Play, device_id);
                }
            }
        }
        if let Some(playing) = playing {
            self.optimistic_playing = Some((!playing, Instant::now()));
        }
    }

    fn seek(&mut self, position_ms: u32) {
        // Dragging the bar under the remembered song moves the point a press
        // of play will resume from; there is no stream to seek yet.
        if self.now_playing_live().is_none() && self.resume_track.is_some() {
            self.resume_position_ms = position_ms;
            self.session_dirty = true;
            return;
        }
        match self.target() {
            Target::Local => self.backend.player(PlayerCommand::Seek(position_ms)),
            Target::Remote(device_id) => {
                self.pending_remote_position = Some((position_ms, Instant::now()));
                self.backend.api(ApiRequest::Remote {
                    action: RemoteAction::Seek,
                    device_id,
                    play: None,
                    position_ms,
                    percent: 0,
                    flag: false,
                    repeat: String::new(),
                });
            }
        }
    }

    /// The volume this side set that the engine has yet to confirm, if the
    /// hold is still good. Clears itself once the engine agrees or it expires.
    fn held_local_volume(&mut self, reported: u16) -> Option<u16> {
        match self.pending_local_volume {
            Some((volume, at)) if volume != reported && at.elapsed() < OPTIMISTIC_HOLD => {
                Some(volume)
            }
            _ => {
                self.pending_local_volume = None;
                None
            }
        }
    }

    /// `settle` is false while the slider is still moving: the level is heard
    /// at once, and Spotify is told where it ended up on release.
    fn set_volume(&mut self, percent: u8, settle: bool) {
        let percent = percent.min(100);
        match self.target() {
            Target::Local => {
                let volume = percent_to_volume(percent);
                self.local.volume = volume;
                self.pending_local_volume = Some((volume, Instant::now()));
                // The engine echoes `VolumeChanged` only while this device
                // holds the Connect session, so the snapshot that would
                // otherwise persist this may never arrive.
                if self.settings.volume != volume {
                    self.settings.volume = volume;
                    self.settings_dirty = true;
                }
                self.backend.player(if settle {
                    PlayerCommand::Volume(volume)
                } else {
                    PlayerCommand::VolumePreview(volume)
                });
            }
            Target::Remote(_) if !settle => {}
            Target::Remote(device_id) => {
                self.pending_remote_volume = Some((percent, Instant::now()));
                self.backend.api(ApiRequest::Remote {
                    action: RemoteAction::Volume,
                    device_id,
                    play: None,
                    position_ms: 0,
                    percent,
                    flag: false,
                    repeat: String::new(),
                });
            }
        }
    }

    fn note_shuffle_pending(&mut self) {
        if let Loadable::Loaded(queue) = &self.queue {
            let at = Self::end_of_queued_rows(&queue.queue, &self.manual_queue);
            let context_uris: Vec<String> = queue.queue[at..]
                .iter()
                .map(|item| item.uri().to_string())
                .collect();
            if context_uris.len() > 1 && context_uris.iter().any(|uri| uri != &context_uris[0]) {
                self.queue_shuffle_pending = Some(QueueShufflePending {
                    current_uri: self.current_track_uri(),
                    context_uris,
                    at: Instant::now(),
                });
            } else {
                self.queue_shuffle_pending = None;
            }
        }
    }

    fn set_shuffle(&mut self, shuffle: bool) {
        self.shuffle_wanted = shuffle;
        self.shuffle_set_at = Some(Instant::now());
        self.session_dirty = true;
        if let Some(assumed) = &mut self.assumed_context {
            assumed.shuffle = Some(shuffle);
        }
        self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
        self.note_shuffle_pending();
        match self.target() {
            Target::Local => {
                self.local.shuffle = shuffle;
                self.backend.player(PlayerCommand::Shuffle(shuffle));
            }
            Target::Remote(None) if self.remote_fresh().is_none() => {
                // Keep the mode for Play without requesting an absent device.
            }
            Target::Remote(device_id) => {
                if let Some(remote) = self.remote.as_mut() {
                    remote.state.shuffle_state = shuffle;
                }
                self.backend.api(ApiRequest::Remote {
                    action: RemoteAction::Shuffle,
                    device_id,
                    play: None,
                    position_ms: 0,
                    percent: 0,
                    flag: shuffle,
                    repeat: String::new(),
                });
            }
        }
    }

    fn set_repeat(&mut self, mode: RepeatMode) {
        match self.target() {
            Target::Local => {
                self.local.repeat = mode;
                self.backend.player(PlayerCommand::Repeat(mode));
            }
            Target::Remote(device_id) => {
                if let Some(remote) = self.remote.as_mut() {
                    remote.state.repeat_state = mode.api_name().to_string();
                }
                self.backend.api(ApiRequest::Remote {
                    action: RemoteAction::Repeat,
                    device_id,
                    play: None,
                    position_ms: 0,
                    percent: 0,
                    flag: false,
                    repeat: mode.api_name().to_string(),
                });
            }
        }
    }

    fn transfer(&mut self, device_id: String) {
        if Some(device_id.as_str()) == self.local_device_id.as_deref() {
            self.selected_device = None;
            self.show_devices = false;
            if !self.local.is_active() {
                // Connect transfers the active device's full playback state.
                // A Web API snapshot may be stale and cannot recreate its queue.
                self.queue_start_pending = Some(Target::Local);
                self.local_transfer_sequence = Some(self.local.track_sequence);
                self.local_list = None;
                self.resume_queue.clear();
                self.queued_play = None;
                self.intent_track = None;
                self.assumed_context = None;
                self.optimistic_playing = None;
                self.clear_play_pending();
                self.backend.player(PlayerCommand::Transfer);
            }
            self.poll_remote_soon();
            return;
        }
        let play = self.now_playing().is_some_and(|now| now.playing);
        self.selected_device = Some(device_id.clone());
        self.backend.api(ApiRequest::Transfer { device_id, play });
    }

    /// Adds a row to Next up immediately, before the context's upcoming rows.
    fn add_to_queue(&mut self, uri: String, label: String) {
        if util::uri_kind(&uri) == Some("album") {
            self.queue_album(uri, label);
            return;
        }
        if self.queued_moments_ago(&uri) {
            return;
        }
        self.queue_one(uri, label, true);
    }

    /// Whether `uri` was queued so recently that asking again is the same
    /// click arriving twice. Later duplicates are separate asks.
    fn queued_moments_ago(&mut self, uri: &str) -> bool {
        self.expire_pending_queue_adds();
        self.pending_queue_adds
            .iter()
            .any(|pending| pending.item.uri() == uri && pending.at.elapsed() < QUEUE_ADD_DEBOUNCE)
    }

    fn queue_album(&mut self, uri: String, label: String) {
        if self
            .last_album_queue
            .as_ref()
            .is_some_and(|(previous, at)| previous == &uri && at.elapsed() < QUEUE_ADD_DEBOUNCE)
        {
            return;
        }
        let Some(id) = util::uri_id(&uri).map(str::to_string) else {
            return;
        };
        self.last_album_queue = Some((uri, Instant::now()));
        if let Some(page) = self.album_pages.get(&id)
            && page.tracks.is_complete()
        {
            self.queue_album_tracks(page.tracks.items.clone(), label);
            return;
        }
        self.album_queue_serial = self.album_queue_serial.wrapping_add(1);
        let request = self.album_queue_serial;
        self.pending_album_queues.insert(
            request,
            PendingAlbumQueue {
                id: id.clone(),
                label: label.clone(),
                target: self.target(),
                offset: 0,
                tracks: Vec::new(),
            },
        );
        self.backend.api(ApiRequest::AlbumQueueTracks {
            id,
            offset: 0,
            request,
        });
        // Translators: {name} is an album name.
        self.toast(gettext(self.locale, "Loading {name} to queue…").replace("{name}", &label));
    }

    fn receive_album_queue(
        &mut self,
        request: u64,
        offset: u32,
        result: crate::api::client::Result<crate::api::models::Page<Track>>,
    ) {
        let Some(pending) = self.pending_album_queues.get(&request) else {
            return;
        };
        if pending.offset != offset {
            return;
        }
        let mut pending = self.pending_album_queues.remove(&request).unwrap();
        if pending.target != self.target() {
            self.toast_error(gettext(
                self.locale,
                "Playback device changed. Add the album to queue again",
            ));
            return;
        }
        let page = match result {
            Ok(page) if page.offset == offset => page,
            Ok(_) => {
                self.toast_error(gettext(
                    self.locale,
                    "Couldn't load the album's songs in order. Try again",
                ));
                return;
            }
            Err(error) => {
                self.toast_error(
                    // Translators: {name} is an album name and {error} is an error message.
                    gettext(self.locale, "Couldn't add {name} to queue: {error}")
                        .replace("{name}", &pending.label)
                        .replace("{error}", &error.to_string()),
                );
                return;
            }
        };
        let next = page.next_offset();
        if page.next.is_some() && next.is_none() {
            self.toast_error(gettext(
                self.locale,
                "Couldn't load the album's songs in order. Try again",
            ));
            return;
        }
        pending.tracks.extend(page.items);
        if let Some(next) = next {
            if next <= offset {
                self.toast_error(gettext(
                    self.locale,
                    "Couldn't load the album's songs in order. Try again",
                ));
                return;
            }
            pending.offset = next;
            self.backend.api(ApiRequest::AlbumQueueTracks {
                id: pending.id.clone(),
                offset: next,
                request,
            });
            self.pending_album_queues.insert(request, pending);
        } else {
            self.queue_album_tracks(pending.tracks, pending.label);
        }
    }

    fn queue_album_tracks(&mut self, tracks: Vec<Track>, label: String) {
        let pending_start = self.pending_queue_adds.len();
        let mut uris = Vec::new();
        for track in tracks {
            if track.is_local
                || track.is_playable == Some(false)
                || util::uri_kind(&track.uri) != Some("track")
            {
                continue;
            }
            let uri = track.uri.clone();
            let name = track.name.clone();
            if let Some(id) = util::uri_id(&uri) {
                self.remember_track_recording(&track);
                self.track_cache.insert(id.to_string(), track);
            }
            self.show_queued_song(&uri, &name);
            uris.push(uri);
        }
        if uris.is_empty() {
            self.toast_error(gettext(
                self.locale,
                "This album has no playable songs to queue",
            ));
            return;
        }
        let count = uris.len();
        self.toast(
            ngettext(
                self.locale,
                // Translators: {count} is a number of songs and {name} is an album name.
                "{count} song from {name} added to queue",
                "{count} songs from {name} added to queue",
                count as u32,
            )
            .replace("{count}", &count.to_string())
            .replace("{name}", &label),
        );
        if self.local.is_active() && self.target() == Target::Local {
            for uri in uris {
                self.backend.player(PlayerCommand::AddToQueue(uri));
            }
            self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
        } else {
            self.write_queue_adds(uris, pending_start);
        }
    }

    /// Queues several songs after the ones already queued, skipping any
    /// queued again within `QUEUE_ADD_DEBOUNCE`, with one combined toast.
    fn queue_many(&mut self, songs: Vec<(String, String)>) {
        // Each picked row is its own ask, so a song picked twice is queued
        // twice. Only an add from an earlier click can make one of them a
        // repeat, so decide that before adding any.
        let repeats: Vec<bool> = songs
            .iter()
            .map(|(uri, _)| self.queued_moments_ago(uri))
            .collect();
        let mut count = 0;
        for ((uri, label), repeat) in songs.into_iter().zip(repeats) {
            if !repeat {
                self.queue_one(uri, label, false);
                count += 1;
            }
        }
        if count > 0 {
            self.queued_toast(count);
        }
    }

    fn queued_toast(&mut self, count: usize) {
        self.toast(
            ngettext(
                self.locale,
                // Translators: {count} is a number of songs.
                "{count} song added to queue",
                "{count} songs added to queue",
                count as u32,
            )
            .replace("{count}", &count.to_string()),
        );
    }

    /// Replays the manually queued songs on the local engine in their
    /// current order. librespot can only append to or clear a live queue,
    /// so a move or positional insert clears it and re-adds every song.
    fn resync_local_queue(&mut self) {
        self.backend.player(PlayerCommand::ClearQueue);
        for uri in self.manual_queue.clone() {
            if uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:") {
                self.backend.player(PlayerCommand::AddToQueue(uri));
            }
        }
        // Drop any queue fetch already in flight: it was asked for before
        // the move and would otherwise land with the pre-move order.
        self.queue_seq += 1;
        self.queue_reorder_pending = Some(Instant::now());
        self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
    }

    /// Adds one song after existing manual queue entries.
    ///
    /// `announce` is false when a batch should produce one toast.
    fn queue_one(&mut self, uri: String, label: String, announce: bool) {
        let pending_start = self.pending_queue_adds.len();
        self.show_queued_song(&uri, &label);
        if announce {
            // Translators: {name} is a song, episode, album, or playlist name.
            self.toast(gettext(self.locale, "{name} added to queue").replace("{name}", &label));
        }
        // Queue tracks and episodes directly on the active local engine.
        // Other targets and item types use the Web API.
        let track_like = uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:");
        if track_like && self.local.is_active() && matches!(self.target(), Target::Local) {
            self.backend.player(PlayerCommand::AddToQueue(uri));
            self.queue_recheck_at = Some(Instant::now() + QUEUE_RECHECK);
            return;
        }
        self.write_queue_adds(vec![uri], pending_start);
    }

    fn show_queued_song(&mut self, uri: &str, label: &str) {
        let item = self.optimistic_queue_item(uri, label);
        self.pending_queue_adds.push(PendingQueueAdd {
            item: item.clone(),
            at: Instant::now(),
            manual_index: self.manual_queue.len(),
            write: None,
        });
        if let Loadable::Loaded(queue) = &self.queue {
            let at = Self::end_of_queued_rows(&queue.queue, &self.manual_queue);
            if let Loadable::Loaded(queue) = &mut self.queue {
                queue.queue.insert(at, item);
            }
        }
        self.manual_queue.push(uri.to_string());
        self.session_dirty = true;
    }

    fn write_queue_adds(&mut self, uris: Vec<String>, pending_start: usize) {
        let target = self.target();
        let device_id = match &target {
            Target::Local => self.local_device_id.clone(),
            Target::Remote(device_id) => device_id.clone(),
        };
        self.album_queue_serial = self.album_queue_serial.wrapping_add(1);
        let request = self.album_queue_serial;
        for (index, addition) in self.pending_queue_adds[pending_start..]
            .iter_mut()
            .enumerate()
        {
            addition.write = Some((request, index));
        }
        self.pending_queue_batches.insert(request, target);
        self.backend.api(ApiRequest::AddManyToQueue {
            request,
            uris,
            device_id,
        });
    }

    /// Roll back only the rejected occurrences, leaving accepted songs,
    /// later additions and matching songs in the playing context intact.
    fn remove_failed_queue_adds(&mut self, request: u64, added: usize) {
        let mut indexes: Vec<_> = self
            .pending_queue_adds
            .iter()
            .filter(|addition| {
                addition
                    .write
                    .is_some_and(|(write, index)| write == request && index >= added)
            })
            .map(|addition| addition.manual_index)
            .collect();
        indexes.sort_unstable();
        for index in indexes.into_iter().rev() {
            let Some(uri) = self.manual_queue.get(index) else {
                continue;
            };
            let occurrence = self.manual_queue[..index]
                .iter()
                .filter(|other| *other == uri)
                .count();
            if let Loadable::Loaded(queue) = &mut self.queue {
                let queued_len = Self::end_of_queued_rows(&queue.queue, &self.manual_queue);
                if let Some(row) = queue.queue[..queued_len]
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.uri() == uri)
                    .nth(occurrence)
                    .map(|(row, _)| row)
                {
                    queue.queue.remove(row);
                }
            }
            self.remove_manual_queue_row(index);
        }
    }

    /// Index after manual queue rows and before context rows.
    fn end_of_queued_rows(rows: &[PlayableItem], manual: &[String]) -> usize {
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for uri in manual {
            *counts.entry(uri.as_str()).or_insert(0) += 1;
        }
        let mut at = 0;
        for item in rows {
            match counts.get_mut(item.uri()) {
                Some(left) if *left > 0 => {
                    *left -= 1;
                    at += 1;
                }
                _ => break,
            }
        }
        at
    }

    /// Queue row built from cached details or a temporary label.
    fn optimistic_queue_item(&self, uri: &str, label: &str) -> PlayableItem {
        let cached = util::uri_id(uri)
            .and_then(|id| self.track_cache.get(id))
            .cloned();
        if let Some(track) = cached {
            return PlayableItem::Track(track);
        }
        if let Some(item) = self.queue.get().and_then(|queue| {
            queue
                .currently_playing
                .iter()
                .chain(&queue.queue)
                .find(|item| item.uri() == uri)
        }) {
            return item.clone();
        }
        if let Some(item) = self
            .playlist_pages
            .values()
            .flat_map(|page| &page.items.items)
            .filter_map(PlaylistItem::playable)
            .find(|item| item.uri() == uri)
        {
            return item.clone();
        }
        let known = self
            .album_pages
            .values()
            .flat_map(|page| &page.tracks.items)
            .chain(self.library.liked.items.iter().map(|saved| &saved.track))
            .chain(
                self.search
                    .results
                    .get()
                    .into_iter()
                    .flat_map(|results| results.tracks.iter().flat_map(|page| &page.items)),
            )
            .find(|track| track.uri == uri);
        if let Some(track) = known {
            return PlayableItem::Track(track.clone());
        }
        PlayableItem::Track(crate::api::models::Track {
            uri: uri.to_string(),
            name: label.to_string(),
            ..Default::default()
        })
    }

    fn expire_pending_queue_adds(&mut self) {
        if !self.pending_queue_batches.is_empty() {
            return;
        }
        self.pending_queue_adds
            .retain(|addition| addition.at.elapsed() < Duration::from_secs(30));
    }

    /// Restores pending additions missing from a stale fetched queue.
    fn reconcile_pending_queue(&mut self) {
        self.expire_pending_queue_adds();
        if self.pending_queue_adds.is_empty() {
            return;
        }
        let Loadable::Loaded(queue) = &mut self.queue else {
            return;
        };
        let fetched: std::collections::HashSet<String> = queue
            .queue
            .iter()
            .map(|item| item.uri().to_string())
            .collect();
        // A fetched or currently playing addition is no longer pending.
        let current = self.current_track_uri();
        self.pending_queue_adds.retain(|addition| {
            !fetched.contains(addition.item.uri())
                && current.as_deref() != Some(addition.item.uri())
        });
        let missing: Vec<PlayableItem> = self
            .pending_queue_adds
            .iter()
            .map(|addition| addition.item.clone())
            .collect();
        let at = match &self.queue {
            Loadable::Loaded(queue) => Self::end_of_queued_rows(&queue.queue, &self.manual_queue),
            _ => 0,
        };
        if let Loadable::Loaded(queue) = &mut self.queue {
            for item in missing.into_iter().rev() {
                queue.queue.insert(at, item);
            }
        }
    }

    /// Answer from the rows already held locally. A found duplicate is
    /// definitive even for a partial playlist; an empty answer is definitive
    /// only when the whole playlist is present.
    fn local_playlist_duplicates(&self, id: &str, items: &[PlayableItem]) -> Option<Vec<String>> {
        let page = self.playlist_pages.get(id)?;
        let existing: HashSet<&str> = page
            .items
            .items
            .iter()
            .filter_map(PlaylistItem::playable)
            .map(PlayableItem::uri)
            .chain(page.local_additions.iter().map(String::as_str))
            .collect();
        let duplicates: Vec<String> = items
            .iter()
            .map(PlayableItem::uri)
            .filter(|uri| existing.contains(*uri))
            .map(str::to_string)
            .collect();
        if !duplicates.is_empty() || page.items.is_complete() {
            Some(duplicates)
        } else {
            None
        }
    }

    fn prepare_playlist_mutation(&mut self, id: &str) {
        let Some(page) = self.playlist_pages.get_mut(id) else {
            return;
        };
        page.pending_writes += 1;
        self.load_generation += 1;
        page.generation = self.load_generation;
        page.items_generation = page.generation;
        // Reads issued before the edit describe the old snapshot and must not
        // be allowed to replace the optimistic rows when they arrive.
        page.items.loading = page.refresh_after_write;
        page.items.clear_windows();
        page.cache_saved_through = None;
        page.cache_saved_rows = 0;
        page.cache_saved_total = None;
        // Let the old write finish before checkpointing the edited snapshot.
        page.cache_append_valid = false;
        page.cache_restored_through = None;
        page.pending_cache = None;
    }

    fn request_playlist_add(
        &mut self,
        playlist_id: String,
        playlist_name: String,
        items: Vec<PlayableItem>,
        position: Option<u32>,
    ) {
        match self.local_playlist_duplicates(&playlist_id, &items) {
            Some(duplicate_uris) if duplicate_uris.is_empty() => {
                self.add_to_playlist_now(playlist_id, playlist_name, items, position);
            }
            Some(duplicate_uris) => {
                self.dialog = Some(Dialog::ConfirmPlaylistDuplicates {
                    playlist_id,
                    playlist_name,
                    items,
                    position,
                    duplicate_uris,
                });
            }
            None => {
                self.playlist_busy = true;
                self.backend.api(ApiRequest::CheckPlaylistDuplicates {
                    playlist_id,
                    playlist_name,
                    items,
                    position,
                });
            }
        }
    }

    fn add_to_playlist_now(
        &mut self,
        playlist_id: String,
        playlist_name: String,
        items: Vec<PlayableItem>,
        position: Option<u32>,
    ) {
        if items.is_empty() {
            return;
        }
        self.dialog = None;
        self.playlist_busy = true;
        for item in &items {
            if let PlayableItem::Track(track) = item {
                self.remember_track_recording(track);
            }
        }

        self.prepare_playlist_mutation(&playlist_id);
        let added = items.len().try_into().unwrap_or(u32::MAX);
        let added_by = self.user_id().map(|id| UserRef {
            id: Some(id.to_string()),
        });
        let added_at = Some(jiff::Timestamp::now().to_string());
        if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
            let current_total = page.items.total.unwrap_or_else(|| {
                page.items
                    .base_offset
                    .saturating_add(page.items.items.len() as u32)
            });
            let at = position.unwrap_or(current_total);
            let start = page.items.base_offset;
            let end = start.saturating_add(page.items.items.len() as u32);
            for item in &items {
                page.local_additions.insert(item.uri().to_string());
            }
            if page.items.loaded_once && (start..=end).contains(&at) {
                let relative = (at - start) as usize;
                page.items.items.splice(
                    relative..relative,
                    items.iter().cloned().map(|item| PlaylistItem {
                        added_at: added_at.clone(),
                        added_by: added_by.clone(),
                        is_local: false,
                        item: Some(item),
                        track: None,
                    }),
                );
            } else if at < start {
                // A delayed confirmation can arrive after the user opened a
                // different slice. Keep that slice attached to the same rows.
                page.items.base_offset = start.saturating_add(added);
            }
            if page.items.loaded_once
                && at <= end
                && let Some(next) = &mut page.items.next_offset
            {
                *next = next.saturating_add(added);
            }
            page.items.total = Some(current_total.saturating_add(added));
            page.items.revision = page.items.revision.wrapping_add(1);
            if let Some(playlist) = page.playlist.get_mut() {
                increase_playlist_total(playlist, added);
            }
        }
        if let Some(playlists) = self.library.playlists.get_mut()
            && let Some(playlist) = playlists
                .iter_mut()
                .find(|playlist| playlist.id == playlist_id)
        {
            increase_playlist_total(playlist, added);
        }

        self.backend.api(ApiRequest::AddToPlaylist {
            playlist_id,
            playlist_name,
            uris: items.iter().map(|item| item.uri().to_string()).collect(),
            position,
        });
    }

    /// Appends the songs linked in pasted text to an editable playlist.
    /// Songs this app already knows add their rows at once; the rest are
    /// asked of Spotify first, so that every row has its name.
    fn paste_songs(&mut self, playlist_id: String, text: &str) {
        let Some(playlist_name) = self
            .playlist_pages
            .get(&playlist_id)
            .and_then(|page| page.playlist.get())
            .filter(|playlist| self.can_edit_playlist(playlist))
            .map(|playlist| playlist.name.clone())
        else {
            return;
        };
        let uris: Vec<String> = text
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter_map(crate::link::parse)
            .filter(|uri| matches!(util::uri_kind(uri), Some("track" | "episode")))
            .collect();
        if uris.is_empty() {
            self.toast(gettext(
                self.locale,
                "The clipboard has no Spotify song links",
            ));
            return;
        }
        let mut paste = PendingPaste {
            playlist_id,
            playlist_name,
            uris,
            found: HashMap::new(),
            missing: HashSet::new(),
        };
        for uri in paste.uris.clone() {
            if paste.found.contains_key(&uri) || paste.missing.contains(&uri) {
                continue;
            }
            if let Some(item) = self.known_song(&uri) {
                paste.found.insert(uri, item);
            } else if let Some(id) = uri.strip_prefix("spotify:track:").map(str::to_string) {
                if self.track_requests.insert(id.clone()) {
                    self.backend.api(ApiRequest::Track { id });
                }
            } else {
                // Episodes are only added from what this app has shown.
                paste.missing.insert(uri);
            }
        }
        self.pending_pastes.push(paste);
        self.finish_pastes();
    }

    /// A song this app has already shown, by its URI.
    fn known_song(&mut self, uri: &str) -> Option<PlayableItem> {
        if let Some(item) = self.copied_songs.iter().find(|item| item.uri() == uri) {
            return Some(item.clone());
        }
        let id = uri.strip_prefix("spotify:track:")?;
        self.read_cached_track(id).map(PlayableItem::Track)
    }

    /// Records Spotify's answer for a pasted song link: the song, or `None`
    /// when there is no such song.
    fn resolve_pasted_song(&mut self, uri: &str, item: Option<PlayableItem>) {
        if self.pending_pastes.is_empty() {
            return;
        }
        for paste in &mut self.pending_pastes {
            if !paste.uris.iter().any(|pasted| pasted == uri) {
                continue;
            }
            match &item {
                Some(item) => {
                    paste.found.insert(uri.to_string(), item.clone());
                }
                None => {
                    paste.missing.insert(uri.to_string());
                }
            }
        }
        self.finish_pastes();
    }

    /// Adds every paste whose songs are all known, or known to be missing.
    fn finish_pastes(&mut self) {
        let mut index = 0;
        while index < self.pending_pastes.len() {
            if !self.pending_pastes[index].settled() {
                index += 1;
                continue;
            }
            let paste = self.pending_pastes.remove(index);
            let items: Vec<PlayableItem> = paste
                .uris
                .iter()
                .filter_map(|uri| paste.found.get(uri).cloned())
                .collect();
            let skipped = paste.uris.len() - items.len();
            if skipped > 0 {
                self.toast_error(
                    ngettext(
                        self.locale,
                        // Translators: {count} is the number of pasted links that failed.
                        "{count} pasted link could not be added",
                        "{count} pasted links could not be added",
                        skipped as u32,
                    )
                    .replace("{count}", &skipped.to_string()),
                );
            }
            if !items.is_empty() {
                self.request_playlist_add(paste.playlist_id, paste.playlist_name, items, None);
            }
        }
    }

    fn set_saved(&mut self, uri: String, saved: bool) {
        if uri.starts_with("spotify:playlist:") {
            self.saved.insert(uri.clone(), saved);
            let id = util::uri_id(&uri).unwrap_or_default().to_string();
            self.backend
                .api(ApiRequest::FollowPlaylist { id, follow: saved });
            return;
        }
        self.set_saved_state(uri.clone(), saved);
        self.saved_writes.insert(uri.clone(), saved);
        if self.change_liked_song(&uri, saved) {
            self.sync_liked_songs();
            self.ensure_liked_songs();
        }
        self.backend.api(ApiRequest::SetSaved {
            uris: vec![uri],
            saved,
        });
    }

    // ---- actions -----------------------------------------------------------------

    fn apply_actions(&mut self, ctx: &egui::Context) {
        for cover in self.uploaded_covers.values() {
            ctx.include_bytes(cover.cover.uri.clone(), cover.cover.jpeg.clone());
        }
        self.frame_now = None;
        let mut actions = std::mem::take(&mut self.actions);
        while !actions.is_empty() {
            for action in actions.drain(..) {
                self.apply(action, ctx);
            }
            actions = std::mem::take(&mut self.actions);
        }
    }

    fn leave_lyrics_fullscreen(&mut self, ctx: &egui::Context) {
        if let Some(was_fullscreen) = self.lyrics_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(was_fullscreen));
            if self.lyrics_restore_maximized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            self.lyrics_fullscreen_restoring = Some(was_fullscreen);
            self.lyrics_fullscreen_seen = false;
            self.lyrics_line_shown = None;
        }
    }

    pub(crate) fn apply(&mut self, action: Action, ctx: &egui::Context) {
        if matches!(
            &action,
            Action::Open(_)
                | Action::OpenUri(_)
                | Action::OpenLink(_)
                | Action::FocusSearch
                | Action::Back
                | Action::Forward
                | Action::SignOut
                | Action::ToggleWinampWindow
                | Action::ToggleQueuePanel
        ) {
            self.leave_lyrics_fullscreen(ctx);
        }
        match action {
            Action::Open(page) => self.open(page),
            Action::PrepareTint(url) => {
                if self.settings.accent_from_art {
                    self.tint_for(Some(&url));
                }
            }
            Action::OpenUri(uri) => {
                if let Some(page) = Page::from_uri(&uri) {
                    self.open(page);
                }
            }
            Action::OpenLink(uri) => {
                // The window first: a link is the user asking for the
                // app, signed in or not, and the page follows when it can.
                self.pending_link = Some(uri);
                self.open_pending_link();
                self.actions.push(Action::ShowWindow);
            }
            Action::Back => {
                if self.can_go_back() {
                    self.history_index -= 1;
                    let page = self.page().clone();
                    self.touch_page(&page);
                    self.ensure_loaded(page.clone());
                    self.retain_table_rows(&page);
                    self.evict_stale_pages();
                }
            }
            Action::Forward => {
                if self.can_go_forward() {
                    self.history_index += 1;
                    let page = self.page().clone();
                    self.touch_page(&page);
                    self.ensure_loaded(page.clone());
                    self.retain_table_rows(&page);
                    self.evict_stale_pages();
                }
            }
            Action::PlayContext {
                uri,
                offset_uri,
                offset_index,
            } => {
                let mut request = PlayRequest::context(uri);
                request.offset_uri = offset_uri;
                request.offset_position = offset_index;
                self.play_request(request, false);
            }
            Action::PlayUris { uris, index } => {
                if uris.is_empty() {
                    return;
                }
                let (uris, index) = cap_uris(&uris, index);
                let request = PlayRequest::tracks(uris).starting_at_index(index);
                self.play_request(request, false);
            }
            Action::PlayFromRow {
                context,
                uri,
                index,
            } => match context {
                RowContext::Context {
                    uri: context_uri, ..
                } => {
                    let request = PlayRequest::context(context_uri).starting_at_uri(uri);
                    self.play_request(request, false);
                }
                RowContext::Uris(uris) => {
                    // The click names a song. A row that plays a list of its
                    // own, as each Recent row does, still hands over its
                    // place in the list on screen; when that place holds
                    // another song, the song wins, as it does in Next up.
                    let index = if uris.get(index as usize).is_some_and(|held| *held == uri) {
                        index
                    } else {
                        uris.iter()
                            .position(|held| *held == uri)
                            .map_or(index, |position| position as u32)
                    };
                    let (uris, index) = cap_uris(uris.as_ref(), index);
                    let request = PlayRequest::tracks(uris).starting_at_index(index);
                    self.play_request(request, false);
                }
                RowContext::Queue => self.play_queue_item(index as usize, uri),
                RowContext::View {
                    uris, context_uri, ..
                } => {
                    let (uris, index) = cap_uris(uris.as_ref(), index);
                    if let Some(uri) = uris.get(index as usize) {
                        self.cache_track_from_context(&context_uri, uri);
                    }
                    let request = PlayRequest::tracks(uris).starting_at_index(index);
                    self.play_request(request, false);
                    self.note_recent_context(&context_uri);
                    self.assumed_context = Some(AssumedContext {
                        uri: context_uri,
                        shuffle: self.shuffle_wanted.then_some(true),
                        at: Instant::now(),
                    });
                }
            },
            Action::ShufflePlay(uri) => {
                // The random starting song is picked in `play_request`,
                // which every shuffled play goes through.
                self.play_request(PlayRequest::context(uri), true);
            }
            Action::TogglePlay => self.toggle_play(),
            Action::Next if self.resume_only() => {
                self.step_resume(true);
            }
            Action::Next => {
                // Move the queue head to the playing row immediately. Do not
                // pop when there is no active playback target.
                let target = self.target();
                if !matches!(target, Target::Remote(None)) || self.remote_fresh().is_some() {
                    self.pop_queue_head();
                }
                match target {
                    Target::Local => self.backend.player(PlayerCommand::Next),
                    Target::Remote(device_id) => self.remote(RemoteAction::Next, device_id),
                }
            }
            // Previous restarts after three seconds and otherwise steps back,
            // matching librespot.
            Action::Previous if self.resume_only() => {
                if self.resume_position_ms > RESTART_BEFORE_PREVIOUS {
                    self.resume_position_ms = 0;
                    self.session_dirty = true;
                } else {
                    self.step_resume(false);
                }
            }
            Action::Previous => {
                // Next names its expected destination so the row moves before
                // the engine does. Previous has no forward queue row to name;
                // discard that expectation or a quick Next, Previous leaves
                // the row marker stuck on the skipped-to song.
                self.intent_track = None;
                self.queue_start_pending = Some(self.target());
                match self.target() {
                    Target::Local => self.backend.player(PlayerCommand::Previous),
                    Target::Remote(device_id) => self.remote(RemoteAction::Previous, device_id),
                }
            }
            Action::Seek(position_ms) => self.seek(position_ms),
            Action::SeekBy(offset) => {
                if let Some(now) = self.now_playing() {
                    let target = (i64::from(now.position_ms) + offset)
                        .clamp(0, i64::from(now.duration_ms))
                        as u32;
                    self.seek(target);
                }
            }
            Action::SetVolume(percent) => {
                self.volume_before_mute = None;
                self.set_volume(percent, true);
            }
            Action::PreviewVolume(percent) => self.set_volume(percent, false),
            Action::VolumeBy(delta) => {
                if let Some(now) = self.now_playing() {
                    let next =
                        (i16::from(now.volume_percent) + i16::from(delta)).clamp(0, 100) as u8;
                    self.volume_before_mute = None;
                    self.set_volume(next, true);
                } else if self.is_connected() {
                    let current = volume_to_percent(self.local.volume);
                    let next = (i16::from(current) + i16::from(delta)).clamp(0, 100) as u8;
                    self.set_volume(next, true);
                }
            }
            Action::ToggleMute => {
                let current = self
                    .now_playing()
                    .map(|now| now.volume_percent)
                    .unwrap_or_else(|| volume_to_percent(self.local.volume));
                if current == 0 {
                    let restore = self.volume_before_mute.take().unwrap_or(50).max(5);
                    self.set_volume(restore, true);
                } else {
                    self.volume_before_mute = Some(current);
                    self.set_volume(0, true);
                }
            }
            Action::ToggleShuffle => {
                let shuffle = self
                    .now_playing()
                    .map_or(self.shuffle_wanted, |now| now.shuffle);
                self.set_shuffle(!shuffle);
            }
            Action::SetShuffle(shuffle) => self.set_shuffle(shuffle),
            Action::CycleRepeat => {
                let mode = self.now_playing().map(|now| now.repeat).unwrap_or_default();
                self.set_repeat(mode.next());
            }
            Action::SetRepeat(mode) => self.set_repeat(mode),
            Action::AddToQueue { uri, label } => self.add_to_queue(uri, label),
            Action::QueueMany { songs } => self.queue_many(songs),
            Action::MoveInQueue { from, to } => {
                if !self.queue_locally_reorderable() {
                    return;
                }
                let queued_len = self.queued_rows_len();
                if from >= queued_len || to > queued_len || from == to || to == from + 1 {
                    return;
                }
                if let Loadable::Loaded(queue) = &mut self.queue {
                    let item = queue.queue.remove(from);
                    queue
                        .queue
                        .insert(if to > from { to - 1 } else { to }, item);
                }
                if from < self.manual_queue.len() {
                    let uri = self.manual_queue.remove(from);
                    let at = (if to > from { to - 1 } else { to }).min(self.manual_queue.len());
                    self.manual_queue.insert(at, uri);
                    // Pending additions are tracked by manual_queue index;
                    // shift them the same way the move just shifted the row
                    // they point at, or they end up naming a different song.
                    for addition in &mut self.pending_queue_adds {
                        if addition.manual_index == from {
                            addition.manual_index = at;
                        } else if from < addition.manual_index && addition.manual_index <= at {
                            addition.manual_index -= 1;
                        } else if at <= addition.manual_index && addition.manual_index < from {
                            addition.manual_index += 1;
                        }
                    }
                }
                self.session_dirty = true;
                self.resync_local_queue();
            }
            Action::InsertInQueue { items, position } => {
                if !self.queue_locally_reorderable() {
                    let songs = items
                        .into_iter()
                        .map(|item| (item.uri().to_string(), item.name().to_string()))
                        .collect();
                    self.queue_many(songs);
                    return;
                }
                let position = position.min(self.queued_rows_len());
                let repeats: Vec<bool> = items
                    .iter()
                    .map(|item| self.queued_moments_ago(item.uri()))
                    .collect();
                let mut inserted = 0;
                for (item, repeat) in items.into_iter().zip(repeats) {
                    if repeat {
                        continue;
                    }
                    let uri = item.uri().to_string();
                    let at = (position + inserted).min(self.manual_queue.len());
                    // Inserting here shifts every later manual_queue row
                    // right by one; keep pending additions pointing at their
                    // own song rather than the one now sitting in their slot.
                    for addition in &mut self.pending_queue_adds {
                        if addition.manual_index >= at {
                            addition.manual_index += 1;
                        }
                    }
                    self.pending_queue_adds.push(PendingQueueAdd {
                        item: item.clone(),
                        at: Instant::now(),
                        manual_index: at,
                        write: None,
                    });
                    if let Loadable::Loaded(queue) = &mut self.queue {
                        queue.queue.insert(at.min(queue.queue.len()), item);
                    }
                    self.manual_queue.insert(at, uri);
                    inserted += 1;
                }
                if inserted > 0 {
                    self.session_dirty = true;
                    self.queued_toast(inserted);
                    self.resync_local_queue();
                }
            }
            Action::SetSavedMany { uris, saved } => {
                let mut changed_tracks = false;
                for uri in &uris {
                    self.set_saved_state(uri.clone(), saved);
                    self.saved_writes.insert(uri.clone(), saved);
                    changed_tracks |= self.change_liked_song(uri, saved);
                }
                if changed_tracks {
                    self.sync_liked_songs();
                    self.ensure_liked_songs();
                }
                if !uris.is_empty() {
                    self.backend.api(ApiRequest::SetSaved { uris, saved });
                }
            }
            Action::ToggleSaved(uri) => {
                let saved = self.is_saved(&uri).unwrap_or(false);
                let targets = if saved {
                    self.saved_toggle_targets(&uri)
                } else {
                    vec![uri]
                };
                for target in targets {
                    self.set_saved(target, !saved);
                }
            }
            Action::AddToPlaylist {
                playlist_id,
                playlist_name,
                items,
            } => {
                self.request_playlist_add(playlist_id, playlist_name, items, None);
            }
            Action::InsertInPlaylist {
                playlist_id,
                position,
                items,
            } => {
                let name = self
                    .playlist_pages
                    .get(&playlist_id)
                    .and_then(|page| page.playlist.get())
                    .filter(|playlist| self.can_edit_playlist(playlist))
                    .map(|playlist| playlist.name.clone());
                if let Some(name) = name {
                    self.request_playlist_add(playlist_id, name, items, Some(position));
                }
            }
            Action::ConfirmAddToPlaylist {
                playlist_id,
                playlist_name,
                items,
                position,
            } => {
                self.add_to_playlist_now(playlist_id, playlist_name, items, position);
            }
            Action::RemoveFromPlaylist { playlist_id, uris } => {
                let snapshot_id = self
                    .playlist_pages
                    .get(&playlist_id)
                    .and_then(|page| page.playlist.get())
                    .and_then(|playlist| playlist.snapshot_id.clone());
                self.prepare_playlist_mutation(&playlist_id);
                if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
                    page.local_additions
                        .retain(|uri| !uris.iter().any(|removed| removed == uri));
                    let before = page.items.items.len();
                    page.items.retain(|item| {
                        item.playable()
                            .is_none_or(|playable| !uris.iter().any(|uri| uri == playable.uri()))
                    });
                    let removed = (before - page.items.items.len()) as u32;
                    page.items.total = page.items.total.map(|total| total.saturating_sub(removed));
                    page.items.next_offset = page
                        .items
                        .next_offset
                        .map(|offset| offset.saturating_sub(removed));
                    if let Loadable::Loaded(playlist) = &mut page.playlist
                        && let Some(count) =
                            playlist.items_count.as_mut().or(playlist.tracks.as_mut())
                    {
                        count.total = count.total.saturating_sub(removed);
                    }
                }
                self.playlist_busy = true;
                self.backend.api(ApiRequest::RemoveFromPlaylist {
                    playlist_id,
                    uris,
                    snapshot_id,
                });
            }
            Action::MoveInPlaylist {
                playlist_id,
                from,
                to,
            } => {
                let snapshot_id = self
                    .playlist_pages
                    .get(&playlist_id)
                    .and_then(|page| page.playlist.get())
                    .and_then(|playlist| playlist.snapshot_id.clone());
                self.prepare_playlist_mutation(&playlist_id);
                if let Some(page) = self.playlist_pages.get_mut(&playlist_id) {
                    let base = page.items.base_offset;
                    if from >= base && to >= base {
                        page.items
                            .reorder((from - base) as usize, (to - base) as usize);
                    }
                }
                self.playlist_busy = true;
                self.backend.api(ApiRequest::ReorderPlaylist {
                    playlist_id,
                    range_start: from,
                    insert_before: to,
                    snapshot_id,
                });
            }
            Action::ChoosePlaylistCover(id) => {
                if self.cover_uploads.contains_key(&id) {
                    return;
                }
                if let Some(Dialog::EditPlaylist {
                    id: current, cover, ..
                }) = &mut self.dialog
                    && *current == id
                    && cover.request.is_none()
                    && cover.uploading.is_none()
                {
                    self.cover_request = self.cover_request.wrapping_add(1);
                    cover.request = Some(self.cover_request);
                    cover.error = None;
                    self.backend.choose_playlist_cover(id, self.cover_request);
                }
            }
            Action::UploadPlaylistCover(id) => {
                if self.cover_uploads.contains_key(&id) {
                    return;
                }
                let previous_urls = self
                    .uploaded_covers
                    .get(&id)
                    .map(|pending| pending.previous_urls.clone())
                    .unwrap_or_else(|| {
                        self.playlist_pages
                            .get(&id)
                            .and_then(|page| page.playlist.get())
                            .into_iter()
                            .chain(
                                self.library
                                    .playlists
                                    .get()
                                    .into_iter()
                                    .flatten()
                                    .filter(|playlist| playlist.id == id),
                            )
                            .flat_map(|playlist| {
                                playlist.images.iter().map(|image| image.url.clone())
                            })
                            .collect()
                    });
                if let Some(Dialog::EditPlaylist {
                    id: current, cover, ..
                }) = &mut self.dialog
                    && *current == id
                    && cover.uploading.is_none()
                    && cover.request.is_none()
                    && let Some(selected) = cover.selection.clone()
                {
                    self.cover_request = self.cover_request.wrapping_add(1);
                    cover.uploading = Some(self.cover_request);
                    self.cover_uploads.insert(id.clone(), self.cover_request);
                    cover.error = None;
                    self.backend.api(ApiRequest::UploadPlaylistCover {
                        id,
                        request: self.cover_request,
                        previous_urls,
                        cover: selected,
                    });
                }
            }
            Action::ShowDialog(mut dialog) => {
                if let Dialog::EditPlaylist { id, cover, .. } = &mut dialog
                    && let Some(request) = self.cover_uploads.get(id)
                {
                    cover.uploading = Some(*request);
                }
                self.dialog = Some(dialog);
            }
            Action::CloseDialog => {
                if matches!(self.dialog, Some(Dialog::PersonalAppIntro)) {
                    self.settings.personal_app_intro_seen = true;
                    self.settings_dirty = true;
                }
                self.dialog = None;
            }
            Action::CreatePlaylist {
                name,
                public,
                add_uris,
            } => {
                self.playlist_busy = true;
                self.dialog = Some(Dialog::CreatePlaylist {
                    name: name.clone(),
                    public,
                    add_uris,
                });
                self.backend.api(ApiRequest::CreatePlaylist {
                    name,
                    public,
                    description: String::new(),
                });
            }
            Action::UpdatePlaylist {
                id,
                name,
                description,
                public,
            } => {
                self.dialog = None;
                let changes = self.changed_playlist_details(&id, name, description, public);
                if changes.kept_description {
                    self.toast_error(gettext(
                        self.locale,
                        "Spotify doesn't let apps remove a playlist description, so it was kept",
                    ));
                }
                if changes.name.is_some()
                    || changes.description.is_some()
                    || changes.public.is_some()
                {
                    self.playlist_busy = true;
                    self.backend.api(ApiRequest::UpdatePlaylist {
                        id,
                        name: changes.name,
                        description: changes.description,
                        public: changes.public,
                    });
                }
            }
            Action::DeletePlaylist(id) => {
                self.dialog = None;
                self.saved.insert(format!("spotify:playlist:{id}"), false);
                if let Some(playlists) = self.library.playlists.get_mut() {
                    playlists.retain(|playlist| playlist.id != id);
                }
                self.backend
                    .api(ApiRequest::FollowPlaylist { id, follow: false });
            }
            Action::Transfer(device_id) => self.transfer(device_id),
            Action::ActivateReceiver(receiver) => {
                if self.activating_receiver.is_none() {
                    self.activating_receiver = Some(receiver.name.clone());
                    self.backend.send(Command::ActivateReceiver(receiver));
                }
            }
            Action::RefreshDevices => {
                self.devices_fetched_at = None;
                self.refresh_devices();
                self.backend.send(Command::DiscoverReceivers);
            }
            Action::ClearQueue => self.clear_queue(),
            Action::SaveQueueAsPlaylist => self.save_queue_as_playlist(),
            Action::SaveRadio(seed) => self.save_radio(&seed),
            Action::RefreshQueue => self.refresh_queue(true),
            Action::CopyLink(uri) => {
                if let Some(url) = util::open_spotify_url(&uri) {
                    ctx.copy_text(url);
                    self.toast(gettext(self.locale, "Link copied"));
                }
            }
            Action::CopySongs(items) => {
                let links: Vec<String> = items
                    .iter()
                    .filter_map(|item| util::open_spotify_url(item.uri()))
                    .collect();
                if !links.is_empty() {
                    // One link a line, in the platform's own line breaks.
                    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
                    ctx.copy_text(links.join(newline));
                    self.toast(match links.len() {
                        1 => gettext(self.locale, "Link copied").into_owned(),
                        count => ngettext(
                            self.locale,
                            // Translators: {count} is a number of song links, always more than one.
                            "{count} link copied",
                            "{count} links copied",
                            count as u32,
                        )
                        .replace("{count}", &count.to_string()),
                    });
                    self.copied_songs = items;
                }
            }
            Action::PasteSongs { playlist_id, text } => self.paste_songs(playlist_id, &text),
            Action::OpenInSpotify(uri) => {
                if let Some(url) = util::open_spotify_url(&uri) {
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
            Action::Search(query) => {
                self.search.query = query.clone();
                self.search.typed_at = None;
                self.open(Page::Search);
                self.run_search(query.trim().to_string());
            }
            Action::ForgetSearch(query) => {
                self.settings.search_history.retain(|entry| entry != &query);
                self.settings_dirty = true;
            }
            Action::SetSearchFilter(filter) => self.search.filter = filter,
            Action::FocusSearch => {
                self.search.focus_requested = true;
                if !matches!(self.page(), Page::Search) {
                    self.open(Page::Search);
                }
            }
            Action::LoadMore(page) => self.load_more(page),
            Action::LoadWindow { page, position } => self.load_window(page, position),
            Action::RetryWindow(page) => self.retry_window(page),
            Action::LoadMoreRecents => self.load_more_recents(),
            Action::ReloadRecents => self.reload_recents(),
            Action::SetQueueTab(tab) => {
                self.queue_tab = tab;
                self.session_dirty = true;
                if tab == QueueTab::Recents
                    && self.recents.items.is_empty()
                    && !self.recents.loading
                {
                    self.load_recents(false);
                }
            }
            Action::LoadMoreArtistAlbums(id) => {
                let Some(page) = self.artist_pages.get_mut(&id) else {
                    return;
                };
                let groups = page.filter.groups().to_string();
                let list = page.albums.entry(groups.clone()).or_default();
                if let Some(offset) = list.next_offset.filter(|_| list.can_load_more()) {
                    list.loading = true;
                    self.backend
                        .api(ApiRequest::ArtistAlbums { id, groups, offset });
                }
            }
            Action::SetDiscographyFilter { artist_id, filter } => {
                if let Some(page) = self.artist_pages.get_mut(&artist_id) {
                    page.filter = filter;
                }
                self.load_artist_albums(&artist_id, filter);
            }
            Action::ToggleShowAllTop(id) => {
                if let Some(page) = self.artist_pages.get_mut(&id) {
                    page.show_all_top = !page.show_all_top;
                }
            }
            Action::Reload(page) => self.reload(page),
            Action::SignIn => self.request_proxy(true),
            Action::ApplyProxy => self.request_proxy(false),
            Action::ProxyEdited => self.proxy_form_edited = true,
            Action::CancelSignIn => {
                self.backend.send(Command::CancelSignIn);
                self.sign_in_url = None;
                self.auth = AuthStatus::SignedOut;
            }
            Action::ConfigurePersonalWebApp => {
                self.save_settings();
                self.backend.send(Command::ConfigurePersonalWebApp(
                    self.settings.web_client_id.clone(),
                ));
            }
            Action::OpenPersonalAppSetup => {
                self.settings.personal_app_intro_seen = true;
                self.settings_dirty = true;
                self.dialog = None;
                self.open(Page::Settings);
                // A saved search could be hiding the Client ID field the
                // flow is about to focus, so drop it before landing.
                crate::ui::settings::clear_search(ctx);
                ctx.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new(crate::ui::settings::PERSONAL_APP_FOCUS_ID),
                        true,
                    );
                });
            }
            Action::SignOut => {
                self.backend.send(Command::SignOut);
                self.history = vec![Page::Home];
                self.history_index = 0;
            }
            Action::ToggleSidebar => {
                self.settings.sidebar_visible = !self.settings.sidebar_visible;
                self.settings_dirty = true;
            }
            Action::ToggleQueuePanel => {
                self.show_queue_panel = !self.show_queue_panel;
                if self.show_queue_panel {
                    self.show_lyrics_panel = false;
                    self.refresh_queue(true);
                }
            }
            Action::ToggleLyricsPanel => {
                self.leave_lyrics_fullscreen(ctx);
                self.show_lyrics_panel = !self.show_lyrics_panel;
                if self.show_lyrics_panel {
                    self.show_queue_panel = false;
                    self.lyrics_following = true;
                    self.request_lyrics();
                }
            }
            Action::SetLyricsFullscreen(fullscreen) => {
                if fullscreen && self.lyrics_fullscreen.is_none() {
                    self.lyrics_fullscreen =
                        Some(self.lyrics_fullscreen_restoring.take().unwrap_or_else(|| {
                            ctx.input(|input| input.viewport().fullscreen.unwrap_or(false))
                        }));
                    #[cfg(windows)]
                    {
                        // Winit's undecorated maximized Windows client area is
                        // constrained to the work area, even in fullscreen.
                        // Clear maximization before entering, then restore it
                        // together with the original window mode on exit.
                        self.lyrics_restore_maximized |= self.lyrics_fullscreen == Some(false)
                            && ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                        if self.lyrics_restore_maximized {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                        }
                    }
                    self.lyrics_fullscreen_seen = false;
                    self.show_lyrics_panel = true;
                    self.show_queue_panel = false;
                    self.lyrics_following = true;
                    self.lyrics_line_shown = None;
                    self.request_lyrics();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                } else if !fullscreen {
                    self.leave_lyrics_fullscreen(ctx);
                }
            }
            Action::LyricsLineShown(line) => {
                // Escape or navigation may already have left the view earlier
                // in this frame. The returning panel still needs to reposition.
                if self.lyrics_fullscreen.is_some() {
                    self.lyrics_line_shown = Some(line);
                }
            }
            Action::FollowLyrics => {
                self.lyrics_following = true;
                self.lyrics_line_shown = None;
            }
            Action::PauseLyricsFollow => self.lyrics_following = false,
            Action::RetryLyrics => self.request_lyrics(),
            Action::ToggleDevicesPopup => {
                self.show_devices = !self.show_devices;
                if self.show_devices {
                    self.refresh_devices();
                    // Receivers waiting on the network are invisible to the
                    // Web API, so look for them ourselves.
                    self.backend.send(Command::DiscoverReceivers);
                }
            }
            Action::CheckForUpdates => self.check_for_updates(true),
            Action::ShowUpdate => {
                self.show_update = true;
                if self.update_support.is_none() {
                    self.backend.send(Command::InspectUpdate);
                }
            }
            Action::DownloadUpdate => {
                if matches!(
                    self.update_download,
                    crate::updates::DownloadState::Idle | crate::updates::DownloadState::Failed(_)
                ) && let Some(release) = self.update.clone()
                {
                    self.update_download = crate::updates::DownloadState::Downloading {
                        received: 0,
                        total: 0,
                    };
                    self.backend.send(Command::DownloadUpdate {
                        release,
                        source: self.update_source.clone(),
                    });
                }
            }
            Action::InstallUpdate => {
                if let crate::updates::DownloadState::Ready(prepared) = &self.update_download {
                    self.backend.send(Command::InstallUpdate {
                        prepared: prepared.clone(),
                        arguments: self.update_restart_arguments.clone(),
                    });
                    self.update_download = crate::updates::DownloadState::Installing;
                }
            }
            Action::SetLibrarySort { shelf, sort } => {
                if sort.supports(shelf) {
                    self.settings.library_sort.insert(shelf, sort);
                    match shelf {
                        crate::settings::LibraryShelf::Albums => self.library.albums.error = None,
                        crate::settings::LibraryShelf::Artists => self.library.artists.error = None,
                        crate::settings::LibraryShelf::Podcasts => self.library.shows.error = None,
                        crate::settings::LibraryShelf::Playlists => {}
                    }
                    self.mark_settings_dirty();
                }
            }
            Action::SetLibraryGrid(grid) => {
                self.settings.sidebar_grid = grid;
                self.mark_settings_dirty();
            }
            Action::ToggleLibraryFolder(id) => {
                if self.collapsed_folders.contains(&id) {
                    self.collapsed_folders.retain(|held| held != &id);
                } else {
                    self.collapsed_folders.push(id);
                }
                self.session_dirty = true;
            }
            Action::ArrangeLibrary {
                pinned,
                playlist_order,
            } => {
                self.settings.liked_songs_pinned = pinned
                    .iter()
                    .any(|key| key == crate::settings::LIKED_SONGS_KEY);
                self.settings.pinned_contexts = pinned;
                if let Some(order) = playlist_order {
                    // Spotify doesn't let apps change the playlist order it
                    // keeps, so a drag in that order starts a local one.
                    if crate::ui::sidebar::selected_sort(
                        self,
                        crate::settings::LibraryShelf::Playlists,
                    ) == crate::settings::LibrarySort::Spotify
                    {
                        self.toast(gettext(
                            self.locale,
                            "Spotify doesn't let apps reorder your playlists, so this order is saved on this computer",
                        ));
                    }
                    self.settings.sidebar_order = order;
                    self.settings.library_sort.insert(
                        crate::settings::LibraryShelf::Playlists,
                        crate::settings::LibrarySort::Local,
                    );
                }
                self.settings
                    .sidebar_order
                    .retain(|key| !self.settings.pinned_contexts.contains(key));
                self.mark_settings_dirty();
            }
            Action::SetTheme(choice) => {
                self.settings.theme = choice;
                self.settings.custom_theme = None;
                self.settings.custom_theme_cache = None;
                self.mark_settings_dirty();
                ctx.set_theme(self.theme_preference());
                self.apply_theme(ctx);
            }
            Action::SetLanguage(choice) => {
                self.settings.language = choice;
                self.locale = choice.resolve();
                self.mark_settings_dirty();
                ctx.request_repaint();
            }
            Action::SetCustomTheme(filename) => {
                if let Some(theme) = self.custom_themes.find(&filename) {
                    self.settings.custom_theme_cache = Some(theme.clone());
                    self.settings.custom_theme = Some(filename);
                    self.mark_settings_dirty();
                    ctx.set_theme(self.theme_preference());
                    self.apply_theme(ctx);
                }
            }
            Action::ReloadThemes => {
                let waker = Waker::default();
                waker.attach(ctx);
                self.load_custom_themes(&waker);
            }
            Action::OpenThemesFolder => {
                self.backend.send(Command::OpenThemesFolder);
            }
            Action::SettingsChanged => {
                self.settings_dirty = true;
                ctx.set_theme(self.theme_preference());
            }
            Action::RestartEngine => {
                self.save_settings();
                let config = engine_config(
                    &self.dirs,
                    &self.settings,
                    self.applied_proxy.clone(),
                    std::sync::Arc::clone(&self.winamp.tap),
                    std::sync::Arc::clone(&self.winamp.eq),
                );
                self.backend.send(Command::RestartEngine(config));
                if self.local_ready {
                    self.toast(gettext(self.locale, "Restarting local playback"));
                }
            }
            Action::ShowWindow => {
                if self.window_hidden {
                    // No window exists; the outer loop creates one.
                    self.wants_show = true;
                } else {
                    // A minimized window has to be restored before it can be
                    // focused: Focus alone leaves it in the Dock, and the
                    // platform draws no frames while it is down there, so
                    // nothing arrives to ask a second time.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            }
            Action::HideWindow => {
                if self.tray.is_some() {
                    self.hide_intent = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Action::EnablePlayback => {
                let free = self
                    .user
                    .as_ref()
                    .and_then(|user| user.product.as_deref())
                    .is_some_and(|product| product != "premium");
                if free {
                    self.toast_error(gettext(self.locale, "Local playback needs Spotify Premium"));
                } else if !self.local_ready
                    && !matches!(
                        self.local_playback,
                        LocalPlayback::Authorizing | LocalPlayback::Connecting
                    )
                {
                    self.settings.playback_authorized = true;
                    self.settings_dirty = true;
                    self.backend.send(Command::AuthorizePlayback);
                    self.toast(gettext(
                        self.locale,
                        "Opening your browser to set up local playback",
                    ));
                }
            }
            Action::OpenUrl(url) => {
                // Open links off the UI thread with the platform-specific
                // launcher used elsewhere in the app.
                std::thread::spawn(move || {
                    if let Err(error) = crate::opener::open(&url) {
                        log::warn!("unable to open {url}: {error}");
                    }
                });
            }
            Action::ClearPlayHistory => {
                self.plays.clear();
                self.plays.save(&self.dirs.history_file());
                self.rebuild_recents();
                self.toast(gettext(self.locale, "Play history cleared"));
            }
            Action::ClearArtCache => match self.backend.art().clear_disk_cache() {
                Ok(bytes) => {
                    ctx.forget_all_images();
                    // The media controls hold a path into what was just
                    // deleted; forget it, or the next sync hands the system
                    // a file that is no longer there.
                    self.media_art = None;
                    self.toast(
                        // Translators: {size} is a size in megabytes, such as 12.5.
                        gettext(self.locale, "Cleared {size} MB of artwork")
                            .replace("{size}", &format!("{:.1}", bytes as f64 / 1_048_576.0)),
                    );
                }
                Err(error) => self.toast_error(
                    // Translators: {error} is an error message.
                    gettext(self.locale, "Couldn't clear artwork: {error}")
                        .replace("{error}", &error.to_string()),
                ),
            },
            Action::ToggleWinampWindow => {
                // One window at a time: this one closes and the loop in
                // `main` opens the other kind where each was last.
                if self.settings.winamp_window {
                    self.winamp.remember_position();
                }
                self.session_window_size = self.last_window_size.or(self.session_window_size);
                self.session_window_pos = self.last_window_pos.or(self.session_window_pos);
                self.settings.winamp_window = !self.settings.winamp_window;
                self.settings_dirty = true;
                self.switch_intent = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::SetSkin(name) => {
                self.settings.skin = name;
                self.settings_dirty = true;
            }
            Action::InstallSkin(path) => {
                self.winamp.install(path, &self.dirs.skins_dir(), ctx);
            }
            Action::SetSkinScale(scale) => {
                self.settings.skin_scale = Some(scale);
                self.settings_dirty = true;
            }
            Action::ToggleWinampOnTop => {
                if self.window_level_supported {
                    self.settings.winamp_on_top = !self.settings.winamp_on_top;
                    self.settings_dirty = true;
                    self.push_winamp_level(ctx);
                }
            }
            Action::SetCustomTitlebar(custom) => {
                if self.settings.custom_titlebar != custom {
                    self.settings.custom_titlebar = custom;
                    crate::window::set_custom_titlebar(custom);
                    self.mark_settings_dirty();
                    if !self.settings.winamp_window {
                        // Decorations are fixed at creation. Replace only the
                        // native window, keeping the page and playback.
                        self.switch_intent = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            Action::SetWinampTaskbar(visible) => {
                if self.settings.winamp_show_taskbar != visible {
                    self.settings.winamp_show_taskbar = visible;
                    self.mark_settings_dirty();
                    if self.settings.winamp_window {
                        // This window attribute is fixed at creation. Keep
                        // the visible mini player, its position, and playback
                        // while replacing only its native window.
                        self.winamp.remember_position();
                        self.switch_intent = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
            Action::ToggleWinampPlaylist => {
                self.settings.playlist_open = !self.settings.playlist_open;
                self.settings_dirty = true;
                if self.settings.playlist_open {
                    self.refresh_queue(false);
                }
            }
            Action::SetPlaylistHeight(height) => {
                self.settings.playlist_height = height.clamp(
                    crate::skin::layout::PLAYLIST_MIN_HEIGHT,
                    crate::skin::layout::PLAYLIST_MAX_HEIGHT,
                );
                self.settings_dirty = true;
            }
            Action::ToggleWinampEq => {
                self.settings.eq_open = !self.settings.eq_open;
                self.settings_dirty = true;
            }
            Action::ToggleEq => {
                self.settings.eq_on = !self.settings.eq_on;
                self.push_eq();
            }
            Action::SetEqBand(band, gain_db) => {
                if let Some(slot) = self.settings.eq_bands_db.get_mut(band) {
                    *slot = gain_db.clamp(-crate::eq::RANGE_DB, crate::eq::RANGE_DB);
                    self.push_eq();
                }
            }
            Action::SetEqPreamp(gain_db) => {
                self.settings.eq_preamp_db =
                    gain_db.clamp(-crate::eq::RANGE_DB, crate::eq::RANGE_DB);
                self.push_eq();
            }
            Action::ApplyEqPreset(index) => {
                if let Some(preset) = crate::eq::PRESETS.get(index) {
                    self.settings.eq_bands_db = preset.bands_db;
                    self.settings.eq_on = true;
                    self.push_eq();
                }
            }
            Action::SetBalance(balance) => {
                self.settings.balance = balance.clamp(-1.0, 1.0);
                self.push_eq();
            }
            Action::ToggleMono => {
                self.settings.mono = !self.settings.mono;
                self.push_eq();
            }
            Action::ToggleWinampShade => {
                self.settings.winamp_shaded = !self.settings.winamp_shaded;
                self.settings_dirty = true;
            }
            Action::ToggleWinampPlaylistShade => {
                self.settings.playlist_shaded = !self.settings.playlist_shaded;
                self.settings_dirty = true;
            }
            Action::ToggleWinampEqShade => {
                self.settings.eq_shaded = !self.settings.eq_shaded;
                self.settings_dirty = true;
            }
            // The same request the window's own close button makes, so the
            // close-to-tray setting decides what follows.
            Action::CloseWindow => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::CycleVisualiser => {
                self.settings.vis = self.settings.vis.next();
                self.settings_dirty = true;
                self.winamp.analyser.reset();
            }
            Action::SetVisualiser(mode) => {
                if self.settings.vis != mode {
                    self.settings.vis = mode;
                    self.settings_dirty = true;
                    self.winamp.analyser.reset();
                }
            }
            Action::OpenSkinsFolder => self.open_folder(self.dirs.skins_dir()),
            Action::ToggleWinampMilkdrop => {
                self.settings.milkdrop_open = !self.settings.milkdrop_open;
                self.settings_dirty = true;
                #[cfg(feature = "milkdrop")]
                if self.settings.milkdrop_open {
                    // A first open has nothing to draw but the idle preset,
                    // which hardly answers the music; fetch the packs in the
                    // background and the window fills up on its own.
                    let folder = self.dirs.milkdrop_dir();
                    self.winamp.presets.refresh(&folder);
                    if self.winamp.presets.count() == 0
                        && self.winamp.presets.downloading().is_none()
                    {
                        self.winamp.presets.download_missing(
                            folder,
                            ctx.clone(),
                            self.applied_proxy.clone(),
                        );
                        self.toast(gettext(self.locale, "Downloading MilkDrop preset packs"));
                    }
                }
            }
            Action::SetMilkdropSeconds(seconds) => {
                self.settings.milkdrop_seconds = seconds.clamp(1, 3600);
                self.settings_dirty = true;
            }
            Action::SetMilkdropFps(fps) => {
                self.settings.milkdrop_fps = if fps == 0 {
                    0
                } else {
                    fps.clamp(
                        *crate::milkdrop::FPS_RANGE.start(),
                        *crate::milkdrop::FPS_RANGE.end(),
                    )
                };
                self.settings_dirty = true;
            }
            Action::SetMilkdropScale(scale) => {
                self.settings.milkdrop_scale = scale.clamp(1, 4);
                self.settings_dirty = true;
            }
            Action::OpenMilkdropFolder => self.open_folder(self.dirs.milkdrop_dir()),
            Action::DownloadMilkdropPack(index) => {
                if let Some(pack) = crate::milkdrop::PACKS.get(index) {
                    self.winamp.presets.download(
                        pack,
                        self.dirs.milkdrop_dir(),
                        ctx.clone(),
                        self.applied_proxy.clone(),
                    );
                    self.toast(
                        // Translators: {name} is the name of a visualizer preset pack.
                        gettext(self.locale, "Downloading {name} presets")
                            .replace("{name}", pack.name),
                    );
                }
            }
            Action::Quit => {
                self.quit_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// The details the edit dialog actually changed, compared with the
    /// playlist it was opened from. Spotify rejects an empty description
    /// with "Attribute description is empty", so clearing one sends nothing
    /// for it rather than failing the rename saved alongside. The dialog
    /// shows the description without its HTML, so an untouched one is not
    /// sent back stripped either.
    fn changed_playlist_details(
        &self,
        id: &str,
        name: String,
        description: String,
        public: Option<bool>,
    ) -> PlaylistDetailChanges {
        let original = self
            .playlist_pages
            .get(id)
            .and_then(|page| page.playlist.get())
            .or_else(|| {
                self.library
                    .playlists
                    .get()
                    .and_then(|playlists| playlists.iter().find(|playlist| playlist.id == id))
            });
        let original_description = original
            .and_then(|playlist| playlist.description.as_deref())
            .map(util::strip_html)
            .unwrap_or_default();
        let cleared = description.is_empty();
        PlaylistDetailChanges {
            name: original
                .is_none_or(|playlist| playlist.name != name)
                .then_some(name),
            kept_description: cleared && !original_description.is_empty(),
            description: (!cleared && description != original_description).then_some(description),
            public: public
                .filter(|public| original.is_none_or(|playlist| playlist.public != Some(*public))),
        }
    }

    pub fn toast(&mut self, message: impl Into<String>) {
        self.toasts.push(Toast {
            message: message.into(),
            kind: ToastKind::Info,
            created: Instant::now(),
        });
        self.toasts.truncate(4);
    }

    pub fn toast_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        log::warn!("{message}");
        self.toasts.push(Toast {
            message,
            kind: ToastKind::Error,
            created: Instant::now(),
        });
    }

    pub fn report_update_failure(&mut self, error: String) {
        self.toast_error(error);
    }

    fn check_for_updates(&mut self, manual: bool) {
        if self.update_checking
            || (self.offline && matches!(self.update_source, crate::updates::Source::GitHub))
            || !matches!(
                self.update_download,
                crate::updates::DownloadState::Idle | crate::updates::DownloadState::Failed(_)
            )
        {
            return;
        }
        self.update_checking = true;
        self.last_update_check = Some(Instant::now());
        self.backend.send(Command::CheckForUpdates {
            manual,
            source: self.update_source.clone(),
        });
    }

    fn maybe_suggest_personal_app(&mut self) {
        if self.settings.personal_app_intro_seen
            || self
                .settings
                .web_client_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
            || self.web_app.is_some()
            || !self.is_connected()
            || self.offline
            || self.user.as_ref().and_then(|user| user.product.as_deref()) != Some("premium")
            || self.dialog.is_some()
            || self.show_devices
            || self.settings.winamp_window
            || self.page() == &Page::Settings
        {
            return;
        }
        self.dialog = Some(Dialog::PersonalAppIntro);
    }

    /// Selected row indices for `page`.
    pub fn picked_rows(&self, page: &Page) -> Option<&std::collections::BTreeSet<usize>> {
        self.selection
            .as_ref()
            .filter(|(owner, _, _)| owner == page)
            .map(|(_, _, selection)| &selection.rows)
            .filter(|rows| !rows.is_empty())
    }

    /// Clears selection when the page's rows or order change.
    pub fn keep_picked_rows_for(&mut self, page: &Page, view: &str) {
        let stale = self
            .selection
            .as_ref()
            .is_some_and(|(owner, seen, _)| owner == page && seen != view);
        if stale {
            self.selection = None;
        }
    }

    /// Applies a single, toggle, or range row selection.
    ///
    /// `len` bounds ranges if rows changed after the anchor was set.
    pub fn pick_row(&mut self, page: &Page, view: &str, row: usize, pick: RowPick, len: usize) {
        let mut selection = match self.selection.take() {
            Some((owner, seen, selection)) if owner == *page && seen == view => selection,
            _ => RowSelection::default(),
        };
        match pick {
            RowPick::Only => {
                // Clicking the sole selected row clears the selection.
                let only_this = selection.rows.len() == 1 && selection.rows.contains(&row);
                selection.rows.clear();
                if only_this {
                    selection.anchor = None;
                } else {
                    selection.rows.insert(row);
                    selection.anchor = Some(row);
                }
            }
            RowPick::Toggle => {
                if !selection.rows.remove(&row) {
                    selection.rows.insert(row);
                }
                selection.anchor = Some(row);
            }
            RowPick::Range => {
                // Without an anchor, shift-click selects only this row.
                let anchor = selection.anchor.unwrap_or(row);
                let (from, to) = if anchor <= row {
                    (anchor, row)
                } else {
                    (row, anchor)
                };
                selection.rows.clear();
                selection.rows.extend((from..=to).filter(|row| *row < len));
                selection.anchor = Some(anchor);
            }
        }
        if selection.rows.is_empty() {
            self.selection = None;
        } else {
            self.selection = Some((page.clone(), view.to_string(), selection));
        }
    }

    /// Picks exactly `rows` of the page's list as it looks in `view`: every
    /// song it shows, for Select all.
    pub fn pick_rows(&mut self, page: &Page, view: &str, rows: std::collections::BTreeSet<usize>) {
        let Some(&first) = rows.first() else {
            self.selection = None;
            return;
        };
        self.selection = Some((
            page.clone(),
            view.to_string(),
            RowSelection {
                rows,
                anchor: Some(first),
            },
        ));
    }

    /// Clears the current row selection.
    pub fn clear_picked_rows(&mut self) {
        self.selection = None;
    }

    /// Whether songs may be added to `playlist` from here: the account
    /// owns it, Spotify flags it collaborative, or the rootlist says the
    /// account was invited to it.
    pub fn can_edit_playlist(&self, playlist: &Playlist) -> bool {
        let owned = self.user_id().is_some_and(|user| playlist.owned_by(user));
        owned || playlist.collaborative || self.editable_by_grant.contains(&playlist.uri)
    }

    /// The library's playlists that take songs, as id and name pairs.
    pub fn editable_playlists(&self) -> Vec<(String, String)> {
        if self.user_id().is_none() {
            return Vec::new();
        }
        self.library
            .playlists
            .get()
            .map(|playlists| {
                playlists
                    .iter()
                    .filter(|playlist| self.can_edit_playlist(playlist))
                    .map(|playlist| (playlist.id.clone(), playlist.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl App {
    /// Runs background work with or without a main window.
    pub fn background_frame(&mut self, ctx: &egui::Context) {
        if self.autoscroll.cancel_if_unfocused(ctx) {
            self.glide = None;
            self.scroll_lock = None;
        }
        self.handle_control_commands();
        self.handle_events();
        self.open_pending_link();
        self.handle_media_commands();
        self.handle_tray();
        #[cfg(target_os = "macos")]
        self.handle_dock_menu();
        self.tick(ctx);
        self.note_listening();
        // MilkDrop runs in a child process and can outlive the main window.
        // Poll it before applying actions because its keys produce actions.
        #[cfg(feature = "milkdrop")]
        self.sync_milkdrop(ctx);
        self.apply_actions(ctx);
        self.sync_media_controls(ctx);
        self.sync_window_title(ctx);
        self.schedule_next_pass(ctx);
    }

    /// Asks for the next pass that playback, pending plays and polling need.
    ///
    /// This runs with the logic, not the drawing: eframe skips drawing a
    /// hidden, minimised or occluded window but still runs its logic, so
    /// a hidden window keeps polling and keeps media controls current.
    fn schedule_next_pass(&self, ctx: &egui::Context) {
        let playing = self.now_playing().is_some_and(|now| now.playing);
        if playing {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self.any_play_pending() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
        if self.is_connected() {
            ctx.request_repaint_after(self.connected_repaint_interval());
        }
    }

    /// Records a track after enough active listening time.
    ///
    /// Paused time and seeking do not count. Each track is recorded once.
    fn note_listening(&mut self) {
        let Some(now) = self.now_playing_live() else {
            self.listening = None;
            return;
        };
        let listening = match &mut self.listening {
            Some(held) if held.uri == now.uri => held,
            _ => {
                self.listening = Some(Listening {
                    uri: now.uri.clone(),
                    listened: std::time::Duration::ZERO,
                    playing_since: now.playing.then(Instant::now),
                    recorded: false,
                });
                return;
            }
        };
        match (now.playing, listening.playing_since) {
            // Add the completed interval when playback pauses.
            (false, Some(since)) => {
                listening.listened += since.elapsed();
                listening.playing_since = None;
            }
            (true, None) => listening.playing_since = Some(Instant::now()),
            _ => {}
        }
        if listening.recorded {
            return;
        }
        let listened = listening.listened
            + listening
                .playing_since
                .map(|since| since.elapsed())
                .unwrap_or_default();
        if listened < crate::history::counts_after(now.duration_ms) {
            return;
        }
        listening.recorded = true;
        self.plays
            .record(crate::history::played_track(&now), jiff::Timestamp::now());
        self.plays.save(&self.dirs.history_file());
        self.rebuild_recents();
    }

    /// Keeps the current track in the window and taskbar title (#94).
    fn sync_window_title(&mut self, ctx: &egui::Context) {
        let title = match self.now_playing().filter(|now| now.playing) {
            Some(now) if now.subtitle.is_empty() => format!("{} - Spotifast", now.title),
            Some(now) => format!("{} - {}", now.subtitle, now.title),
            None => "Spotifast".to_string(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    pub fn frame_ui(&mut self, ui: &mut egui::Ui) {
        // The native close can take another frame. Settings already describe
        // the replacement window, but drawing it here would resize this one
        // before eframe saves its geometry. attach clears the switch intent
        // only once the replacement exists.
        if self.switch_intent {
            return;
        }
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.refresh_frame_now();
        self.apply_theme(ctx);
        let autoscroll_on = crate::autoscroll::enabled(self.settings.middle_click_autoscroll);
        self.autoscroll.begin(ctx, autoscroll_on);
        if self.autoscroll.active() {
            self.glide = None;
            self.scroll_lock = None;
            ctx.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
        } else {
            self.lock_scroll_axis(ctx);
        }
        if self.lyrics_fullscreen_restoring.is_some()
            && self.lyrics_fullscreen_restoring == ctx.input(|input| input.viewport().fullscreen)
            && (!self.lyrics_restore_maximized
                || ctx.input(|input| input.viewport().maximized == Some(true)))
        {
            self.lyrics_fullscreen_restoring = None;
            self.lyrics_restore_maximized = false;
        }
        if self.lyrics_fullscreen.is_some() {
            if ctx.input(|input| input.viewport().fullscreen.unwrap_or(false)) {
                self.lyrics_fullscreen_seen = true;
            } else if self.lyrics_fullscreen_seen {
                self.leave_lyrics_fullscreen(ctx);
            }
        }
        // Switch to the main window when sign-in is required.
        let needs_sign_in = !(self.is_connected() && self.user.is_some())
            && !matches!(self.auth, AuthStatus::Connecting | AuthStatus::Starting)
            && !(self.is_connected() && self.user.is_none());
        if self.settings.winamp_window && needs_sign_in && !self.switch_intent {
            self.actions.push(Action::ToggleWinampWindow);
        }
        if self.settings.winamp_window {
            crate::ui::winamp::show(self, ui);
        } else {
            crate::ui::show(self, ui);
        }
        self.apply_actions(ctx);
        let autoscroll = self.autoscroll.finish(
            ctx,
            crate::autoscroll::enabled(self.settings.middle_click_autoscroll),
        );
        if autoscroll.scrolling {
            self.glide = None;
            self.scroll_lock = None;
        }
        if autoscroll.stop_following_lyrics {
            self.lyrics_following = false;
        }
        if let Some(offset) = autoscroll.playlist_scroll {
            self.winamp.playlist_scroll = offset;
        }
        self.refresh_frame_now();
        self.sync_media_controls(ctx);

        if !self.settings.winamp_window
            && !self.switch_intent
            && self.lyrics_fullscreen.is_none()
            && self.lyrics_fullscreen_restoring.is_none()
        {
            if let Some(rect) = ctx.input(|input| input.viewport().inner_rect) {
                self.last_window_size = Some([rect.width(), rect.height()]);
            }
            if let Some(rect) = ctx.input(|input| input.viewport().outer_rect) {
                self.last_window_pos = Some([rect.min.x, rect.min.y]);
            }
        }

        if !self.toasts.is_empty() {
            ctx.request_repaint_after(TOAST_FRAME);
        }
        if ctx.input(|input| input.viewport().close_requested())
            && !self.quit_requested
            && !self.switch_intent
            && self.hides_to_tray()
        {
            // Close the window and keep the process running in the tray.
            self.hide_intent = true;
        }
        self.frame_now = None;
    }

    /// How soon the window asks for another frame while signed in.
    ///
    /// API polling already uses 20s during local playback. This only changes
    /// the UI deadline. While a track is playing, the 250ms progress refresh
    /// still wins, so the saving is idle-local frames: 4s -> 20s, 80% fewer
    /// wakeups when paused on this device.
    fn connected_repaint_interval(&self) -> Duration {
        match self.target() {
            Target::Local if self.local.is_active() => REMOTE_POLL_IDLE,
            _ => REMOTE_POLL_ACTIVE,
        }
    }

    /// Locks each scroll gesture to one axis.
    ///
    /// Trackpads report small cross-axis deltas. Choose from the first movement
    /// and hold that axis until the gesture ends.
    fn lock_scroll_axis(&mut self, ctx: &egui::Context) {
        let options = ctx.options(|options| options.input_options);
        let (raw, from_trackpad, ended, announced, forced_axis) = ctx.input(|input| {
            let mut sum = egui::Vec2::ZERO;
            let mut pointish = false;
            let mut ended = false;
            let mut announced = false;
            let mut forced_axis = None;
            for event in &input.events {
                if let egui::Event::MouseWheel {
                    unit,
                    delta,
                    phase,
                    modifiers,
                } = event
                {
                    // egui applies scroll modifiers before producing smooth
                    // deltas. Lock and measure momentum in that same direction.
                    let horizontal = modifiers.matches_any(options.horizontal_scroll_modifier);
                    let vertical = modifiers.matches_any(options.vertical_scroll_modifier);
                    forced_axis = match (horizontal, vertical) {
                        (true, false) => Some(ScrollAxis::Horizontal),
                        (false, true) => Some(ScrollAxis::Vertical),
                        _ => None,
                    };
                    sum += match forced_axis {
                        Some(ScrollAxis::Horizontal) => egui::vec2(delta.x + delta.y, 0.0),
                        Some(ScrollAxis::Vertical) => egui::vec2(0.0, delta.x + delta.y),
                        None => *delta,
                    };
                    pointish |= *unit == egui::MouseWheelUnit::Point;
                    ended |= matches!(phase, egui::TouchPhase::End | egui::TouchPhase::Cancel);
                    announced |= *phase != egui::TouchPhase::Move;
                }
            }
            (sum, pointish, ended, announced, forced_axis)
        });
        self.scroll_lift_announced |= announced;
        let now = Instant::now();
        if raw != egui::Vec2::ZERO {
            self.scroll_from_trackpad = from_trackpad;
        }
        // Linux touchpad point deltas need scaling. Wheel deltas are already
        // scaled, and macOS point deltas need no adjustment.
        let trackpad_here = cfg!(target_os = "linux") && self.scroll_from_trackpad;
        if trackpad_here {
            ctx.input_mut(|input| input.smooth_scroll_delta *= TRACKPAD_SCALE);
        }
        // Add decaying momentum to Linux touchpad scrolling. Track the final
        // 100 ms of movement to estimate release velocity.
        if trackpad_here && raw != egui::Vec2::ZERO {
            self.glide = None;
            self.scroll_accum += raw * TRACKPAD_SCALE;
            self.scroll_history
                .add(ctx.input(|input| input.time), self.scroll_accum);
            self.scroll_last_event = Some(now);
            // Wayland announces the lift; where nothing does, the quiet-gap
            // check below needs a frame to run in.
            ctx.request_repaint_after(Duration::from_millis(60));
        } else if raw != egui::Vec2::ZERO || ctx.input(|input| input.pointer.any_down()) {
            // Wheel input or a press stops touchpad momentum.
            self.glide = None;
            self.scroll_history.clear();
            self.scroll_last_event = None;
        }
        // Where the platform never says when fingers lift, a quiet gap is
        // taken for one. Where it does, fingers resting on the pad are not.
        let quiet = !self.scroll_lift_announced
            && self
                .scroll_last_event
                .is_some_and(|at| now.duration_since(at).as_secs_f32() > 0.15);
        if ended || quiet {
            // Only movement just before the lift carries on: fingers that
            // stopped and rested first leave nothing to glide on.
            let lift_time = ctx.input(|input| input.time);
            let rested = ended
                && self
                    .scroll_history
                    .iter()
                    .last()
                    .is_some_and(|(time, _)| lift_time - time > GLIDE_REST);
            let mut velocity = if rested {
                egui::Vec2::ZERO
            } else {
                self.scroll_history.velocity().unwrap_or(egui::Vec2::ZERO)
            };
            if let Some((axis, _)) = self.scroll_lock {
                match axis {
                    ScrollAxis::Horizontal => velocity.y = 0.0,
                    ScrollAxis::Vertical => velocity.x = 0.0,
                }
            }
            self.glide = (velocity.length() > GLIDE_START).then_some(velocity);
            self.scroll_history.clear();
            self.scroll_accum = egui::Vec2::ZERO;
            self.scroll_last_event = None;
        }
        if let Some(velocity) = self.glide {
            if raw == egui::Vec2::ZERO {
                let dt = ctx.input(|input| input.stable_dt).clamp(0.001, 0.05);
                ctx.input_mut(|input| input.smooth_scroll_delta += velocity * dt);
                let slower = velocity * (-dt / GLIDE_DECAY).exp();
                self.glide = (slower.length() > GLIDE_STOP).then_some(slower);
            }
            ctx.request_repaint();
        }
        let moved = raw != egui::Vec2::ZERO;
        // Separate wheel notches may change direction immediately, including
        // when Shift is pressed or released. Only trackpad gestures hold it.
        let held = forced_axis.or_else(|| {
            self.scroll_lock
                .filter(|(_, at)| {
                    now.duration_since(*at) < SCROLL_GESTURE_GAP && (!moved || from_trackpad)
                })
                .map(|(axis, _)| axis)
        });
        let axis = match held {
            Some(axis) => axis,
            None if moved && raw.x.abs() > raw.y.abs() * 1.2 => ScrollAxis::Horizontal,
            None if moved => ScrollAxis::Vertical,
            None => {
                self.scroll_lock = None;
                return;
            }
        };
        if moved {
            self.scroll_lock = Some((axis, now));
        }
        ctx.input_mut(|input| match axis {
            ScrollAxis::Horizontal => input.smooth_scroll_delta.y = 0.0,
            ScrollAxis::Vertical => input.smooth_scroll_delta.x = 0.0,
        });
    }

    /// Persist state when a window closes (to the tray or for good).
    pub fn save_state(&mut self) {
        self.save_settings();
        self.save_session();
    }

    /// Write the restorable session: page, recents, resume point, sorts.
    fn save_session(&mut self) {
        self.session_dirty = false;
        self.last_session_save = Instant::now();
        if let Some(now) = self.now_playing() {
            self.resume_context = self.playing_context_uri();
            self.resume_track = Some(now.uri.clone());
            self.resume_position_ms = now.position_ms;
        }
        if !self.offline {
            SessionState {
                last_page: Some(self.page().encode()),
                recent_contexts: self.recent_contexts.clone(),
                last_context: self.resume_context.clone(),
                last_track: self.resume_track.clone(),
                last_position_ms: self.resume_position_ms,
                collapsed_folders: self.collapsed_folders.clone(),
                rootlist: self.rootlist_cache.clone(),
                last_added_queue: if self.resume_queue.is_empty() {
                    self.manual_queue.clone()
                } else {
                    // Never resumed this session; the owed queue carries over.
                    self.resume_queue.clone()
                },
                last_queue_rows: self
                    .queue
                    .get()
                    .map(|queue| queue.queue.iter().take(30).cloned().collect())
                    .unwrap_or_default(),
                shuffle_on: self.shuffle_wanted,
                sorts: self
                    .table_sorts
                    .iter()
                    .map(|(page, sort)| (page.encode(), *sort))
                    .collect(),
                window_size: self.last_window_size.or(self.session_window_size),
                window_pos: self.last_window_pos.or(self.session_window_pos),
                queue_open: Some(self.show_queue_panel),
                queue_tab: Some(self.queue_tab.encode().to_string()),
                winamp_pos: self.winamp.last_pos.or(self.winamp.restore_pos),
                milkdrop_pos: self.milkdrop_pos,
                lyrics_fullscreen_from: self.lyrics_fullscreen.map(|fullscreen| {
                    crate::settings::WindowMode {
                        fullscreen,
                        maximized: self.lyrics_restore_maximized,
                    }
                }),
            }
            .save(&self.dirs.session_file());
        }
    }

    /// Final teardown at real quit.
    pub fn shutdown(&mut self) {
        self.save_state();
        self.backend.shutdown();
    }
}

impl App {
    fn ensure_liked_songs(&mut self) {
        if self.liked_songs.cache_loading || self.liked_songs.refreshing() {
            return;
        }
        if !self.liked_songs.cache_checked {
            if self.user_id().is_none() {
                return;
            }
            self.load_generation = self.load_generation.wrapping_add(1);
            self.liked_songs.generation = self.load_generation;
            self.liked_songs.cache_loading = true;
            self.library.liked.loading = true;
            self.backend.send(Command::LoadLikedSongsCache {
                generation: self.load_generation,
            });
        } else if !self.liked_songs.fresh(jiff::Timestamp::now().as_second())
            && self.library.liked.error.is_none()
        {
            self.refresh_liked_songs();
        }
    }

    fn receive_liked_cache(
        &mut self,
        account: &str,
        generation: u64,
        cache: Option<crate::liked::Cache>,
    ) {
        if self.user_id() != Some(account)
            || self.liked_songs.generation != generation
            || self.liked_songs.cache_checked
        {
            return;
        }
        self.liked_songs.cache_loading = false;
        self.liked_songs.cache_checked = true;
        if let Some(cache) = cache.filter(|cache| cache.valid_for(account)) {
            self.liked_songs.restore(cache);
        }
        self.sync_liked_songs();
        for item in &self.library.liked.items {
            let uri = &item.track.uri;
            if !self.saved_writes.contains_key(uri) && self.liked_songs.intent(uri).is_none() {
                self.saved.insert(uri.clone(), true);
                if let Some(key) = item.track.recording_key() {
                    self.track_recordings.insert(uri.clone(), key.clone());
                    self.saved_recordings.insert(key);
                }
            }
        }
        if !self.liked_songs.fresh(jiff::Timestamp::now().as_second())
            || self.liked_songs.has_confirmed_changes()
        {
            self.refresh_liked_songs();
        } else if self.table_sorts.contains_key(&Page::LikedSongs) {
            self.load_more(Page::LikedSongs);
        }
    }

    fn refresh_liked_songs(&mut self) {
        if !self.liked_songs.cache_checked {
            self.ensure_liked_songs();
            return;
        }
        self.load_generation = self.load_generation.wrapping_add(1);
        self.liked_songs.start_refresh(self.load_generation);
        self.library.liked.loading = true;
        self.load_more(Page::LikedSongs);
    }

    fn sync_liked_songs(&mut self) {
        self.liked_songs.sync_view(&mut self.library.liked);
        for (uri, saved) in self.liked_songs.intents() {
            self.set_saved_state(uri, saved);
        }
    }

    fn checkpoint_liked_songs(&mut self, force: bool) {
        if let Some(account) = self.user_id().map(str::to_owned)
            && let Some(cache) = self.liked_songs.checkpoint(account, force)
        {
            self.backend.send(Command::StoreLikedSongsCache(cache));
        }
    }

    fn change_liked_song(&mut self, uri: &str, saved: bool) -> bool {
        let Some(id) = uri.strip_prefix("spotify:track:") else {
            return false;
        };
        self.liked_songs.seed(&self.library.liked);
        let track = self
            .track_cache
            .get(id)
            .cloned()
            .or_else(|| {
                self.library
                    .liked
                    .items
                    .iter()
                    .find(|item| item.track.uri == uri)
                    .map(|item| item.track.clone())
            })
            .or_else(|| {
                self.now_playing_item().and_then(|item| match item {
                    PlayableItem::Track(track) if track.uri == uri => Some(track),
                    _ => None,
                })
            })
            .or_else(|| {
                self.table_rows.values().find_map(|table| {
                    table.items.iter().find_map(|(item, _, _)| match item {
                        PlayableItem::Track(track) if track.uri == uri => Some(track.clone()),
                        _ => None,
                    })
                })
            })
            .or_else(|| {
                self.home
                    .top_tracks
                    .get()
                    .and_then(|tracks| tracks.iter().find(|track| track.uri == uri).cloned())
            })
            .or_else(|| {
                self.search
                    .results
                    .get()
                    .and_then(|results| results.tracks.as_ref())
                    .and_then(|tracks| tracks.items.iter().find(|track| track.uri == uri).cloned())
            });
        let missing = track.is_none();
        let track = track.unwrap_or_else(|| Track {
            uri: uri.to_string(),
            id: Some(id.to_string()),
            name: gettext(self.locale, "Loading…").into_owned(),
            ..Default::default()
        });
        self.liked_songs.change(uri.to_string(), saved, track);
        if saved && missing && self.track_requests.insert(id.to_string()) {
            self.backend.api(ApiRequest::Track { id: id.to_string() });
        }
        true
    }
}

fn same_recording_hint(left: &Track, right: &Track) -> bool {
    left.uri != right.uri
        && !left.name.is_empty()
        && left.name == right.name
        && left.duration_ms > 0
        && left.duration_ms == right.duration_ms
        && !left.artists.is_empty()
        && left.artists.len() == right.artists.len()
        && left
            .artists
            .iter()
            .zip(&right.artists)
            .all(|(left, right)| match (&left.id, &right.id) {
                (Some(left), Some(right)) => left == right,
                _ => left.name == right.name,
            })
}

fn increase_playlist_total(playlist: &mut Playlist, added: u32) {
    if let Some(items) = &mut playlist.items_count {
        items.total = items.total.saturating_add(added);
    } else if let Some(tracks) = &mut playlist.tracks {
        tracks.total = tracks.total.saturating_add(added);
    } else {
        playlist.items_count = Some(TrackCount { total: added });
    }
}

pub fn engine_config(
    dirs: &AppDirs,
    settings: &Settings,
    proxy: crate::settings::ProxyConfig,
    tap: std::sync::Arc<crate::vis::AudioTap>,
    eq: crate::eq::SharedEq,
) -> EngineConfig {
    EngineConfig {
        tap,
        eq,
        device_name: settings.device_name.trim().to_string(),
        bitrate_kbps: settings.bitrate,
        normalisation: settings.normalisation,
        autoplay: settings.autoplay,
        gapless: settings.gapless,
        backend: settings.platform_backend(),
        buffer_ms: settings.audio_buffer_ms,
        audio_device: settings
            .audio_device
            .clone()
            .filter(|device| !device.trim().is_empty()),
        initial_volume: settings.volume,
        volume_dir: dirs.volume_dir(),
        audio_cache_dir: settings.audio_cache.then(|| dirs.audio_cache_dir()),
        audio_cache_limit: Some(settings.audio_cache_mb.max(64) * 1024 * 1024),
        proxy,
    }
}

/// The equalizer as the settings describe it.
pub fn eq_settings(settings: &Settings) -> crate::eq::EqSettings {
    crate::eq::EqSettings {
        on: settings.eq_on,
        preamp_db: settings.eq_preamp_db,
        bands_db: settings.eq_bands_db,
        balance: settings.balance,
        mono: settings.mono,
    }
    .clamped()
}

pub fn volume_to_percent(volume: u16) -> u8 {
    ((u32::from(volume) * 100 + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)) as u8
}

pub fn percent_to_volume(percent: u8) -> u16 {
    ((u32::from(percent.min(100)) * u32::from(u16::MAX)) / 100) as u16
}

/// The window level for the Winamp window's always-on-top setting. Shared by
/// window creation and the live window, so the mapping is owned in one place.
pub fn on_top_window_level(on_top: bool) -> egui::WindowLevel {
    if on_top {
        egui::WindowLevel::AlwaysOnTop
    } else {
        egui::WindowLevel::Normal
    }
}

/// The window level to push to the live window, or `None` when there is no
/// Winamp window to change. The big window (where Settings lives) keeps its
/// normal level, so toggling the setting there only takes effect once the
/// Winamp window opens.
fn winamp_on_top_level(winamp_window: bool, on_top: bool) -> Option<egui::WindowLevel> {
    winamp_window.then_some(on_top_window_level(on_top))
}

fn page_related_needs_load(pages: &HashMap<String, ArtistPage>, id: &str) -> bool {
    pages.get(id).is_some_and(|page| page.related.needs_load())
}

fn remote_action_label(locale: Locale, action: RemoteAction) -> Cow<'static, str> {
    match action {
        RemoteAction::Play => gettext(locale, "Couldn't start playback"),
        RemoteAction::Pause => gettext(locale, "Couldn't pause"),
        RemoteAction::Next => gettext(locale, "Couldn't skip"),
        RemoteAction::Previous => gettext(locale, "Couldn't go back"),
        RemoteAction::Seek => gettext(locale, "Couldn't seek"),
        RemoteAction::Volume => gettext(locale, "Couldn't change the volume"),
        RemoteAction::Shuffle => gettext(locale, "Couldn't change shuffle"),
        RemoteAction::Repeat => gettext(locale, "Couldn't change repeat"),
    }
}

/// Availability explicitly reported for a song; session-only rows are unknown.
fn known_availability(items: &[PlaylistItem]) -> impl Iterator<Item = (&str, bool)> {
    items.iter().filter_map(|item| match item.playable()? {
        PlayableItem::Track(track) => Some((track.uri.as_str(), track.is_playable?)),
        _ => None,
    })
}

/// A fresh Web API answer takes precedence over anything remembered from disk.
fn note_availability(availability: &mut HashMap<String, bool>, items: &[PlaylistItem]) {
    availability
        .extend(known_availability(items).map(|(uri, playable)| (uri.to_string(), playable)));
}

/// Grey out the songs the Web API has said this account cannot play,
/// among rows that do not say. A page read over the streaming session
/// says nothing of the account's market; a song the Web API had greyed
/// out stays so wherever it recurs, and rows it never described stay
/// unknown.
fn fill_availability(availability: &HashMap<String, bool>, items: &mut [PlaylistItem]) -> bool {
    let mut changed = false;
    for item in items {
        if let Some(PlayableItem::Track(track)) = item.item.as_mut()
            && track.is_playable.is_none()
            && availability.get(&track.uri) == Some(&false)
        {
            track.is_playable = Some(false);
            changed = true;
        }
    }
    changed
}

fn friendly_page_error(locale: Locale, error: &crate::api::ApiError) -> String {
    match error.status() {
        Some(403) | Some(404) => gettext(
            locale,
            "Spotify doesn't make this playlist's songs available to third-party apps.",
        )
        .into_owned(),
        _ => error.to_string(),
    }
}

/// Whether a Spotify-owned playlist named `name` is the personal one the
/// Made for you shelf looks for under `term`. The name has to be the term
/// itself, or "Daily Mix" with a number: Spotify also makes "<Artist> Mix",
/// "This Is <Artist>", and "<Artist> Radio" for every artist, and an artist
/// called "Discover Weekly" put those on the shelf (#89).
fn is_made_for_you(name: &str, term: &str) -> bool {
    let name = name.trim().to_lowercase();
    let term = term.to_lowercase();
    if name == term {
        return true;
    }
    term == "daily mix"
        && name
            .strip_prefix("daily mix ")
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// What the engine is told to play. A single song goes as a context of
/// its own rather than a list of one: Spotify resolves a track's URI as a
/// context, and a context with a URI is what librespot's autoplay carries
/// on from when it ends, the way one song from a search does in Spotify.
fn local_load(request: &PlayRequest, shuffle: bool) -> LoadSpec {
    let single_song = request.context_uri.is_none()
        && request.uris.len() == 1
        && request.uris[0].contains(":track:");
    if single_song {
        return LoadSpec {
            context_uri: Some(request.uris[0].clone()),
            position_ms: request.position_ms,
            play: true,
            ..LoadSpec::default()
        };
    }
    // A plain track list must not load shuffled when a row was chosen:
    // librespot shuffles the list first and then cannot find the chosen
    // row in it, falls back to nowhere, and replays what was on. The list
    // loads straight and shuffle is switched on right after the load,
    // which also matches Spotify: the chosen song plays, the rest shuffle.
    let chosen = request.offset_uri.is_some() || request.offset_position.is_some();
    let list = request.context_uri.is_none();
    LoadSpec {
        context_uri: request.context_uri.clone(),
        uris: request.uris.clone(),
        offset_uri: request.offset_uri.clone(),
        offset_index: request.offset_position,
        position_ms: request.position_ms,
        play: true,
        shuffle: (shuffle && !(list && chosen)).then_some(true),
        repeat: None,
        autoplay: false,
    }
}

/// Returns the final track as the autoplay seed when a plain list ends.
fn autoplay_seed(
    list: Option<&[String]>,
    autoplay: bool,
    before: &LocalState,
    after: &LocalState,
) -> Option<String> {
    if !autoplay
        || !after.connected
        || after.playback != Playback::Stopped
        || before.playback != Playback::Playing
    {
        return None;
    }
    let track = before.track.as_ref()?;
    if list?.last() != Some(&track.uri) || track.duration_ms == 0 {
        return None;
    }
    let near_end = before.position_now() + 3_000 >= track.duration_ms;
    near_end.then(|| track.uri.clone())
}

/// Caps large track lists at 500 items starting from the selected row.
fn cap_uris(uris: &[String], index: u32) -> (Vec<String>, u32) {
    const MAX: usize = 500;
    if uris.len() <= MAX {
        return (uris.to_vec(), index);
    }
    let start = (index as usize).min(uris.len().saturating_sub(1));
    let end = (start + MAX).min(uris.len());
    (uris[start..end].to_vec(), 0)
}

fn evict_lru_map<V>(
    map: &mut HashMap<String, V>,
    used: &HashMap<Page, Instant>,
    to_page: impl Fn(&str) -> Page,
    protected: &HashSet<String>,
    max: usize,
) {
    if map.len() <= max {
        return;
    }
    let overflow = map.len() - max;
    let mut victims: Vec<(Option<Instant>, String)> = map
        .keys()
        .filter(|id| !protected.contains(*id))
        .map(|id| {
            let used = used.get(&to_page(id)).copied();
            (used, id.clone())
        })
        .collect();
    victims.sort();
    for (_, id) in victims.into_iter().take(overflow) {
        map.remove(&id);
    }
}

fn cover_images(cover: &crate::playlist_cover::Cover) -> Vec<crate::api::models::Image> {
    vec![crate::api::models::Image {
        url: cover.uri.clone(),
        width: None,
        height: None,
    }]
}

fn cover_error(locale: Locale, error: &crate::api::client::ApiError) -> String {
    match error.status() {
        Some(401) => gettext(
            locale,
            "Spotify sign-in expired. Sign in again, then retry the cover upload.",
        )
        .into_owned(),
        Some(403) => gettext(
            locale,
            "Spotify refused this cover. Check that you own the playlist, then sign in again to grant image upload permission. If using a personal app, reconnect it in Settings too.",
        )
        .into_owned(),
        Some(413) => {
            gettext(locale, "Spotify rejected the image size. Choose a smaller image.").into_owned()
        }
        // Translators: {error} is an error message.
        _ => gettext(locale, "Couldn't upload the cover: {error}. Try again.")
            .replace("{error}", &error.to_string()),
    }
}

mod radio;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::api::models::{
        Episode, Image, Page as ApiPage, SavedEpisode, SavedTrack, SearchResults,
    };

    #[test]
    fn middle_clicking_a_playlist_row_autoscrolls_only_on_windows_by_default() {
        middle_click_a_playlist_row(false);
    }

    /// Linux autoscrolls once the listener turns it on; macOS never does.
    #[test]
    fn middle_clicking_a_playlist_row_autoscrolls_on_linux_when_chosen() {
        middle_click_a_playlist_row(true);
    }

    /// Middle-clicks a real playlist row, moves over the queue, and checks
    /// that the list scrolls exactly where autoscroll is on, and that the
    /// click never plays the row.
    fn middle_click_a_playlist_row(chosen: bool) {
        use egui::accesskit::Role;
        let on = crate::autoscroll::enabled(chosen);
        fn draw(
            ctx: &egui::Context,
            app: &mut App,
            time: u32,
            events: Vec<egui::Event>,
        ) -> egui::accesskit::TreeUpdate {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    time: Some(time as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| app.frame_ui(ui),
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        }
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = test_app(if chosen {
            "autoscroll-playlist-row-chosen"
        } else {
            "autoscroll-playlist-row"
        });
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.settings.middle_click_autoscroll = chosen;
        app.open(Page::Playlist("pl1".into()));
        app.show_queue_panel = true;
        let playing = app.now_playing().unwrap().uri.clone();
        draw(&ctx, &mut app, 0, vec![]);
        let tree = draw(&ctx, &mut app, 1, vec![]);
        let (id, node) = tree
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == Role::Button
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Play ") && label.contains(','))
                    && node.bounds().is_some_and(|rect| {
                        rect.x0 > 250.0
                            && rect.x0 < 700.0
                            && rect.width() > 400.0
                            && rect.y0 > 60.0
                            && rect.y1 < 700.0
                    })
            })
            .expect("a visible playlist row");
        let id = *id;
        let before = node.bounds().unwrap();
        let anchor = egui::pos2(before.x0 as f32 + 120.0, before.y0 as f32 + 12.0);
        draw(
            &ctx,
            &mut app,
            2,
            vec![
                egui::Event::PointerMoved(anchor),
                egui::Event::PointerButton {
                    pos: anchor,
                    button: egui::PointerButton::Middle,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(
            app.autoscroll.active(),
            on,
            "only an enabled autoscroll arms the real row's scroll area"
        );
        draw(
            &ctx,
            &mut app,
            3,
            vec![egui::Event::PointerButton {
                pos: anchor,
                button: egui::PointerButton::Middle,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        for frame in 4..8 {
            draw(
                &ctx,
                &mut app,
                frame,
                vec![egui::Event::PointerMoved(egui::pos2(1100.0, 680.0))],
            );
        }
        let tree = draw(&ctx, &mut app, 8, vec![]);
        let after = tree
            .nodes
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .expect("the same displayed occurrence")
            .1
            .bounds()
            .unwrap();
        if on {
            assert!(
                after.y0 < before.y0,
                "the playlist must scroll while the pointer is over Queue"
            );
        } else {
            assert_eq!(after.y0, before.y0, "middle-click must not move the list");
        }
        assert_eq!(app.now_playing().unwrap().uri, playing);
        assert_eq!(app.autoscroll.active(), on);
    }

    #[test]
    fn autoscroll_updates_the_real_lyrics_and_skinned_playlist_without_changing_playback() {
        for (skinned, chosen) in [(false, false), (true, false), (false, true), (true, true)] {
            let on = crate::autoscroll::enabled(chosen);
            let ctx = egui::Context::default();
            let mut app = test_app(match (skinned, chosen) {
                (false, false) => "autoscroll-lyrics",
                (true, false) => "autoscroll-skin",
                (false, true) => "autoscroll-lyrics-chosen",
                (true, true) => "autoscroll-skin-chosen",
            });
            app.attach(&ctx);
            crate::demo::populate(&mut app);
            app.settings.middle_click_autoscroll = chosen;
            app.show_queue_panel = false;
            app.show_lyrics_panel = !skinned;
            app.settings.winamp_window = skinned;
            app.settings.playlist_open = skinned;
            app.settings.skin_scale = Some(2);
            if let Loadable::Loaded(queue) = &mut app.queue {
                queue.queue = queue.queue.iter().cycle().take(80).cloned().collect();
            }
            app.lyrics = Loadable::Loaded(Some(crate::lyrics::Lyrics {
                lines: (0..80)
                    .map(|i| crate::lyrics::Line {
                        at_ms: Some(i * 5000),
                        text: format!("Autoscroll lyric line {i}"),
                    })
                    .collect(),
                synced: true,
                instrumental: false,
            }));
            if let Some(remote) = &mut app.remote {
                remote.state.is_playing = false;
                remote.state.progress_ms = Some(0);
            }
            let playing = app.now_playing().unwrap().uri.clone();
            let mut frame = 0;
            let mut draw = |app: &mut App, events: Vec<egui::Event>| {
                frame += 1;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            if skinned {
                                egui::vec2(550.0, 580.0)
                            } else {
                                egui::vec2(1280.0, 800.0)
                            },
                        )),
                        time: Some(frame as f64 / 60.0),
                        events,
                        ..Default::default()
                    },
                    |ui| app.frame_ui(ui),
                );
                output.textures_delta.clear();
            };
            draw(&mut app, vec![]);
            draw(&mut app, vec![]);
            let anchor = if skinned {
                ctx.read_response(egui::Id::new(("playlist-row", 0_usize)))
                    .unwrap()
                    .rect
                    .center()
            } else {
                egui::pos2(1120.0, 100.0)
            };
            let press = |button, pressed| egui::Event::PointerButton {
                pos: anchor,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            draw(
                &mut app,
                vec![
                    egui::Event::PointerMoved(anchor),
                    press(egui::PointerButton::Middle, true),
                ],
            );
            assert_eq!(
                app.autoscroll.active(),
                on,
                "real surface, skinned={skinned}"
            );
            if !skinned {
                assert_eq!(app.lyrics_following, !on);
            }
            draw(&mut app, vec![press(egui::PointerButton::Middle, false)]);
            for _ in 0..5 {
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(anchor + egui::vec2(0.0, 120.0))],
                );
            }
            if skinned {
                if on {
                    assert!(app.winamp.playlist_scroll > 0);
                } else {
                    assert_eq!(app.winamp.playlist_scroll, 0);
                }
                assert!(
                    app.winamp.playlist_selection.is_empty(),
                    "middle-click must not select a row"
                );
            } else {
                assert_eq!(app.lyrics_following, !on);
            }
            assert_eq!(app.now_playing().unwrap().uri, playing);
            draw(&mut app, vec![press(egui::PointerButton::Primary, true)]);
            assert!(!app.autoscroll.active());
            draw(&mut app, vec![press(egui::PointerButton::Primary, false)]);
            if skinned {
                // After cancelling, a normal click selects the visible row.
                draw(&mut app, vec![egui::Event::PointerMoved(anchor)]);
                draw(&mut app, vec![press(egui::PointerButton::Primary, true)]);
                draw(&mut app, vec![press(egui::PointerButton::Primary, false)]);
                assert_eq!(
                    app.winamp.playlist_selection.len(),
                    1,
                    "ordinary row selection must still work after cancellation"
                );
            }
            app.backend.shutdown();
        }
    }

    #[test]
    fn shift_wheel_moves_the_shelf_without_scrolling_the_page() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut shelf_left = 0.0;
        let mut initial_left = 0.0;
        let mut page_offset = 0.0;
        for frame in 0..6 {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 400.0),
                )),
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            };
            input
                .events
                .push(egui::Event::PointerMoved(egui::pos2(100.0, 70.0)));
            if frame == 2 {
                input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, -3.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::SHIFT,
                });
            }
            let mut output = ctx.run_ui(input, |ui| {
                app.lock_scroll_axis(ui.ctx());
                let page = egui::ScrollArea::vertical().show(ui, |ui| {
                    crate::ui::widgets::shelf(
                        ui,
                        &app.palette,
                        "wheel-test-shelf",
                        "Shelf",
                        |ui| {
                            shelf_left = ui.allocate_space(egui::vec2(1600.0, 100.0)).1.left();
                        },
                    );
                    ui.allocate_space(egui::vec2(100.0, 1200.0));
                });
                page_offset = page.state.offset.y;
            });
            output.textures_delta.clear();
            if frame == 0 {
                initial_left = shelf_left;
            }
        }
        assert!(shelf_left < initial_left, "Shift+wheel must move the shelf");
        assert_eq!(page_offset, 0.0, "the enclosing page must stay put");
    }

    #[test]
    fn wheel_notches_can_change_direction_without_waiting_for_a_gesture_gap() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        for (frame, (modifiers, delta, horizontal)) in [
            (egui::Modifiers::NONE, egui::vec2(0.0, -3.0), false),
            (egui::Modifiers::SHIFT, egui::vec2(0.0, -3.0), true),
            (egui::Modifiers::NONE, egui::vec2(0.0, -3.0), false),
            (egui::Modifiers::NONE, egui::vec2(-3.0, 0.0), true),
        ]
        .into_iter()
        .enumerate()
        {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(frame as f64 / 60.0),
                    events: vec![egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta,
                        phase: egui::TouchPhase::Move,
                        modifiers,
                    }],
                    ..Default::default()
                },
                |ui| {
                    app.lock_scroll_axis(ui.ctx());
                    let delta = ui.input(|input| input.smooth_scroll_delta);
                    if horizontal {
                        assert!(delta.x < 0.0, "horizontal notch {frame}: {delta:?}");
                        assert_eq!(delta.y, 0.0);
                    } else {
                        assert!(delta.y < 0.0, "vertical notch {frame}: {delta:?}");
                        assert_eq!(delta.x, 0.0);
                    }
                },
            );
            output.textures_delta.clear();
        }
    }

    /// A song started outside a playlist must turn off the playlist's
    /// sidebar light at once, even while Spotify still reports the old
    /// context from before the click.
    #[test]
    fn a_plain_song_clears_the_sidebar_context() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.assumed_context = Some(AssumedContext {
            uri: "spotify:playlist:sidebar".into(),
            shuffle: None,
            at: Instant::now(),
        });
        app.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                context: Some(crate::api::models::Context {
                    uri: "spotify:playlist:sidebar".into(),
                    ..Default::default()
                }),
                is_playing: true,
                ..Default::default()
            },
            received_at: Instant::now(),
        });

        app.apply(
            Action::PlayUris {
                uris: vec!["spotify:track:standalone".into()],
                index: 0,
            },
            &ctx,
        );

        assert_eq!(app.playing_context_uri(), None);
    }

    /// The playlist just started stays lit after the first seconds even
    /// while the last poll, taken before the click, still names what the
    /// phone was playing; only a poll from after the click may say
    /// otherwise.
    #[test]
    fn a_poll_from_before_the_click_does_not_move_the_sidebar_light() {
        // #given a poll from the phone's playlist, then a playlist started here
        let mut app = headless_app();
        app.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                context: Some(crate::api::models::Context {
                    uri: "spotify:playlist:phone".into(),
                    ..Default::default()
                }),
                is_playing: true,
                ..Default::default()
            },
            received_at: Instant::now() - Duration::from_secs(30),
        });
        app.assumed_context = Some(AssumedContext {
            uri: "spotify:playlist:here".into(),
            shuffle: None,
            at: Instant::now() - ASSUMED_CONTEXT_HOLD - Duration::from_secs(1),
        });
        app.optimistic_playing = Some((true, Instant::now()));

        // #then the hold is over, yet the stale poll does not win
        assert_eq!(
            app.playing_context_uri().as_deref(),
            Some("spotify:playlist:here")
        );

        // #when a poll taken after the click still reports the phone's playlist
        app.remote.as_mut().expect("a snapshot").received_at = Instant::now();

        // #then Spotify's word from after the click stands
        assert_eq!(
            app.playing_context_uri().as_deref(),
            Some("spotify:playlist:phone")
        );
    }

    /// A song the Web API had greyed out stays greyed out when the session
    /// reads the rows, at every place it recurs, whether the Web API's word
    /// came from an earlier page or from the disk cache of a visit the
    /// playlist has changed since. Rows it never described stay unknown,
    /// and a song it later calls playable is no longer greyed out.
    #[test]
    fn a_session_read_keeps_a_songs_known_unavailability() {
        let mut app = headless_app();
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        let row = |uri: &str, is_playable: Option<bool>| crate::api::models::PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                uri: uri.into(),
                is_playable,
                ..Default::default()
            })),
            ..Default::default()
        };
        let items =
            |id: &str, rows: Vec<crate::api::models::PlaylistItem>| ApiResponse::PlaylistItems {
                id: id.into(),
                offset: 0,
                generation: 0,
                result: Ok(crate::api::models::Page {
                    total: rows.len() as u32,
                    limit: 50,
                    items: rows,
                    ..Default::default()
                }),
            };
        let rows = |app: &App, id: &str| {
            app.playlist_pages[id]
                .items
                .items
                .iter()
                .map(|item| match item.playable() {
                    Some(PlayableItem::Track(track)) => track.is_playable,
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        // The Web API's word from an earlier page of the same list.
        app.playlist_pages
            .insert("pl1".into(), PlaylistPage::default());
        app.handle_api(items(
            "pl1",
            vec![
                row("spotify:track:gone", Some(false)),
                row("spotify:track:fine", Some(true)),
                row("spotify:track:gone", Some(false)),
            ],
        ));
        app.handle_api(items(
            "pl1",
            vec![
                row("spotify:track:gone", None),
                row("spotify:track:fine", None),
                row("spotify:track:new", None),
                row("spotify:track:gone", None),
            ],
        ));
        assert_eq!(rows(&app, "pl1"), [Some(false), None, None, Some(false)]);

        // The Web API's word from the disk cache of another visit, read
        // after the session page arrived and never adopted, the playlist
        // having changed since.
        app.playlist_pages.insert(
            "pl2".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "pl2".into(),
                    snapshot_id: Some("now".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        app.handle_api(items(
            "pl2",
            vec![
                row("spotify:track:other", None),
                row("spotify:track:fine", None),
            ],
        ));
        app.receive_playlist_cache(
            "alice",
            "pl2",
            0,
            Some(PlaylistCache {
                snapshot: "then".into(),
                items: vec![row("spotify:track:other", Some(false))],
                total: 1,
                next_offset: None,
                appendable: false,
            }),
        );
        assert_eq!(
            rows(&app, "pl2"),
            [Some(false), None],
            "the cache's word reaches the rows already shown"
        );
        assert!(
            app.playlist_pages["pl2"].pending_cache.is_none(),
            "though the stale cache itself is not adopted"
        );

        // A song the Web API later calls playable is no longer greyed out.
        app.handle_api(items("pl2", vec![row("spotify:track:other", Some(true))]));
        app.handle_api(items("pl2", vec![row("spotify:track:other", None)]));
        assert_eq!(rows(&app, "pl2"), [None]);
    }

    fn availability_row(playable: Option<bool>) -> PlaylistItem {
        PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                uri: "spotify:track:availability".into(),
                is_playable: playable,
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    fn receive_availability_rows(app: &mut App, id: &str, playable: Option<bool>) {
        app.playlist_pages
            .entry(id.into())
            .or_insert_with(|| PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: id.into(),
                    snapshot_id: Some("now".into()),
                    ..Default::default()
                }),
                ..Default::default()
            });
        app.handle_api(ApiResponse::PlaylistItems {
            id: id.into(),
            offset: 0,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![availability_row(playable)],
                total: 1,
                limit: 50,
                ..Default::default()
            }),
        });
    }

    fn shown_availability(app: &mut App, id: &str) -> Option<bool> {
        let page = &app.playlist_pages[id];
        let generation = page.generation;
        let revision = page.items.revision;
        let rows = page
            .items
            .items
            .iter()
            .filter_map(|row| row.playable().cloned().map(|item| (item, None, None)))
            .collect();
        let rows = crate::ui::collection::cached_table_items(
            app,
            Page::Playlist(id.into()),
            generation,
            revision,
            app.user_names_revision,
            || rows,
        );
        match &rows[0].0 {
            PlayableItem::Track(track) => track.is_playable,
            _ => panic!("a track row"),
        }
    }

    fn old_availability_cache(playable: bool) -> Option<PlaylistCache> {
        Some(PlaylistCache {
            snapshot: "then".into(),
            items: vec![availability_row(Some(playable))],
            total: 1,
            next_offset: None,
            appendable: false,
        })
    }

    #[test]
    fn playlist_availability_does_not_follow_an_account_switch() {
        let mut app = headless_app();
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        receive_availability_rows(&mut app, "pl1", Some(false));
        app.handle_auth(AuthStatus::SignedOut);
        app.handle_auth(AuthStatus::Connected {
            username: "bob".into(),
        });
        app.user = Some(User {
            id: "bob".into(),
            ..Default::default()
        });
        receive_availability_rows(&mut app, "pl1", None);
        app.receive_playlist_cache("alice", "pl1", 0, old_availability_cache(false));
        assert_eq!(shown_availability(&mut app, "pl1"), None);
    }

    #[test]
    fn playlist_availability_prefers_fresh_answers_to_late_disk_caches() {
        for fresh in [true, false] {
            let mut app = headless_app();
            app.user = Some(User {
                id: "alice".into(),
                ..Default::default()
            });
            receive_availability_rows(&mut app, "pl1", Some(fresh));
            receive_availability_rows(&mut app, "pl2", None);
            app.receive_playlist_cache("alice", "pl2", 0, old_availability_cache(!fresh));
            receive_availability_rows(&mut app, "pl3", None);
            assert_eq!(
                shown_availability(&mut app, "pl3"),
                (!fresh).then_some(false)
            );
        }
    }

    #[test]
    fn playlist_availability_from_disk_reaches_rows_already_drawn() {
        let mut app = headless_app();
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        receive_availability_rows(&mut app, "pl1", None);
        assert_eq!(shown_availability(&mut app, "pl1"), None);
        app.receive_playlist_cache("alice", "pl1", 0, old_availability_cache(false));
        assert_eq!(shown_availability(&mut app, "pl1"), Some(false));
    }

    #[test]
    fn playlist_availability_stays_fresh_when_a_cached_prefix_is_adopted() {
        let mut app = headless_app();
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        receive_availability_rows(&mut app, "pl1", Some(true));
        app.playlist_pages.insert(
            "pl2".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "pl2".into(),
                    snapshot_id: Some("then".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        app.receive_playlist_cache("alice", "pl2", 0, old_availability_cache(false));
        assert_eq!(shown_availability(&mut app, "pl2"), Some(true));
    }

    /// Saving the edit dialog sends the public flag only when its switch
    /// was used; a playlist nothing has described keeps whatever it was.
    #[test]
    fn saving_playlist_details_leaves_an_unknown_public_flag_alone() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        let save = |app: &mut App, public| {
            app.apply(
                Action::UpdatePlaylist {
                    id: "pl1".into(),
                    name: "Renamed".into(),
                    description: String::new(),
                    public,
                },
                &ctx,
            );
            match app.backend.take_playlist_add_requests().as_slice() {
                [ApiRequest::UpdatePlaylist { public, .. }] => *public,
                sent => panic!("{sent:?}"),
            }
        };
        assert_eq!(save(&mut app, None), None);
        assert_eq!(save(&mut app, Some(false)), Some(false));
    }

    /// Saving sends only what the dialog changed. Spotify refuses an empty
    /// description, so clearing one keeps it and says why instead of
    /// failing a rename saved at the same time.
    /// Spotify's playlist order can't be changed by apps, so dragging a
    /// playlist in it switches to the local order and says why, once.
    #[test]
    fn dragging_in_spotify_order_switches_to_a_local_order_and_says_why() {
        use crate::settings::{LibraryShelf, LibrarySort};
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.settings
            .library_sort
            .insert(LibraryShelf::Playlists, LibrarySort::Spotify);
        let arrange = |app: &mut App| {
            app.apply(
                Action::ArrangeLibrary {
                    pinned: Vec::new(),
                    playlist_order: Some(vec![
                        "spotify:playlist:b".into(),
                        "spotify:playlist:a".into(),
                    ]),
                },
                &ctx,
            );
        };
        let toasts = app.toasts.len();
        arrange(&mut app);
        assert_eq!(
            app.settings.library_sort.get(&LibraryShelf::Playlists),
            Some(&LibrarySort::Local)
        );
        assert_eq!(app.toasts.len(), toasts + 1);
        assert!(app.toasts.last().unwrap().message.contains("Spotify"));

        arrange(&mut app);
        assert_eq!(
            app.toasts.len(),
            toasts + 1,
            "already local, nothing to explain"
        );
    }

    /// The title bar choice is saved and, because decorations are fixed when
    /// a window is created, replaces only the main window's native window.
    /// The mini player picks it up the next time the main window opens.
    #[test]
    fn choosing_the_custom_title_bar_recreates_only_the_main_window() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.apply(Action::SetCustomTitlebar(true), &ctx);
        assert!(app.settings.custom_titlebar);
        assert!(app.switch_intent, "the main window is replaced");
        assert_eq!(crate::window::custom_titlebar(), cfg!(windows));

        app.switch_intent = false;
        app.apply(Action::SetCustomTitlebar(true), &ctx);
        assert!(!app.switch_intent, "an unchanged choice keeps the window");

        app.settings.winamp_window = true;
        app.apply(Action::SetCustomTitlebar(false), &ctx);
        assert!(!app.settings.custom_titlebar);
        assert!(!app.switch_intent, "the mini player is not the main window");
        assert!(!crate::window::custom_titlebar());
    }

    #[test]
    fn saving_playlist_details_sends_only_what_changed() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            name: "Mix".into(),
            description: Some("<b>Old</b> notes".into()),
            public: Some(true),
            ..Playlist::default()
        }]);
        let save = |app: &mut App, name: &str, description: &str, public| {
            app.apply(
                Action::UpdatePlaylist {
                    id: "pl1".into(),
                    name: name.into(),
                    description: description.into(),
                    public,
                },
                &ctx,
            );
            match app.backend.take_playlist_add_requests().as_slice() {
                [] => None,
                [
                    ApiRequest::UpdatePlaylist {
                        name,
                        description,
                        public,
                        ..
                    },
                ] => Some((name.clone(), description.clone(), *public)),
                sent => panic!("{sent:?}"),
            }
        };

        assert_eq!(
            save(&mut app, "Renamed", "Old notes", Some(true)),
            Some((Some("Renamed".into()), None, None)),
            "an untouched description is not sent back without its HTML"
        );
        assert_eq!(
            save(&mut app, "Mix", "New notes", Some(true)),
            Some((None, Some("New notes".into()), None))
        );
        assert_eq!(
            save(&mut app, "Mix", "Old notes", Some(false)),
            Some((None, None, Some(false)))
        );

        app.playlist_busy = false;
        let toasts = app.toasts.len();
        assert_eq!(save(&mut app, "Mix", "", Some(true)), None);
        assert!(!app.playlist_busy, "nothing was sent, so nothing waits");
        assert_eq!(app.toasts.len(), toasts + 1);
        assert_eq!(
            save(&mut app, "Renamed", "", Some(true)),
            Some((Some("Renamed".into()), None, None)),
            "clearing the description does not fail the rename"
        );
    }

    /// A header read over the streaming session carries no public flag,
    /// and the edit dialog fills its switch from it, so the library list's
    /// answer stands in.
    #[test]
    fn a_header_without_a_public_flag_takes_the_library_lists() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            public: Some(true),
            ..Playlist::default()
        }]);
        app.playlist_pages.insert(
            "pl1".into(),
            PlaylistPage {
                generation: 1,
                ..Default::default()
            },
        );
        app.handle_api(ApiResponse::Playlist {
            id: "pl1".into(),
            generation: 1,
            result: Ok(Playlist {
                id: "pl1".into(),
                name: "Mine".into(),
                ..Playlist::default()
            }),
        });
        let playlist = app.playlist_pages["pl1"].playlist.get().unwrap();
        assert_eq!(playlist.public, Some(true));
        assert_eq!(playlist.name, "Mine", "the rest is Spotify's answer");

        // Spotify's own answer outranks the list, and a playlist the list
        // does not hold stays unknown rather than guessed.
        app.handle_api(ApiResponse::Playlist {
            id: "pl1".into(),
            generation: 1,
            result: Ok(Playlist {
                id: "pl1".into(),
                public: Some(false),
                ..Playlist::default()
            }),
        });
        assert_eq!(
            app.playlist_pages["pl1"].playlist.get().unwrap().public,
            Some(false)
        );
        app.playlist_pages.insert(
            "pl2".into(),
            PlaylistPage {
                generation: 1,
                ..Default::default()
            },
        );
        app.handle_api(ApiResponse::Playlist {
            id: "pl2".into(),
            generation: 1,
            result: Ok(Playlist {
                id: "pl2".into(),
                ..Playlist::default()
            }),
        });
        assert_eq!(
            app.playlist_pages["pl2"].playlist.get().unwrap().public,
            None
        );

        // The list can arrive after the header: the page takes the flag
        // then, and a flag Spotify already gave stays.
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![
                    Playlist {
                        id: "pl1".into(),
                        public: Some(true),
                        ..Playlist::default()
                    },
                    Playlist {
                        id: "pl2".into(),
                        public: Some(true),
                        ..Playlist::default()
                    },
                ],
                ..Default::default()
            }),
        });
        assert_eq!(
            app.playlist_pages["pl2"].playlist.get().unwrap().public,
            Some(true)
        );
        assert_eq!(
            app.playlist_pages["pl1"].playlist.get().unwrap().public,
            Some(false)
        );
    }

    /// A page of the library's playlists, `limit` 2, that says whether
    /// more follow.
    fn playlist_page(ids: &[&str], offset: u32, total: u32) -> crate::api::models::Page<Playlist> {
        crate::api::models::Page {
            items: ids
                .iter()
                .map(|id| Playlist {
                    id: (*id).into(),
                    uri: format!("spotify:playlist:{id}"),
                    ..Playlist::default()
                })
                .collect(),
            total,
            limit: 2,
            offset,
            next: (offset + (ids.len() as u32) < total).then(|| "more".to_string()),
        }
    }

    fn listed_playlists(app: &App) -> Option<Vec<String>> {
        app.library.playlists.get().map(|playlists| {
            playlists
                .iter()
                .map(|playlist| playlist.id.clone())
                .collect::<Vec<_>>()
        })
    }

    fn playlist_ids(ids: &[&str]) -> Option<Vec<String>> {
        Some(ids.iter().map(|id| (*id).to_string()).collect())
    }

    /// Following, unfollowing or editing a playlist reads the library's
    /// playlists again from the top. A later page asked for before that
    /// belongs to the old list: taking it made that page the whole list,
    /// and the pages it led on to ran beside the new ones and repeated them.
    #[test]
    fn a_playlist_page_asked_for_before_a_reload_is_not_taken() {
        let mut app = headless_app();
        app.backend.set_offline(true);

        app.load_playlists();
        let old = app.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: old,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        // The second page is on its way when following a playlist reloads.
        app.handle_api(ApiResponse::PlaylistFollowChanged {
            id: "new".into(),
            followed: true,
            result: Ok(()),
        });
        assert!(app.library.playlists.is_loading());
        let new = app.library.playlists_generation;
        assert_ne!(new, old, "a reload is a new load");
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: old,
            result: Ok(playlist_page(&["c"], 2, 3)),
        });
        assert!(
            app.library.playlists.is_loading(),
            "the late page is not the reloaded list"
        );
        assert_eq!(app.library.playlists_next, None, "and asks for nothing");

        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: new,
            result: Ok(playlist_page(&["new", "a"], 0, 4)),
        });
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: new,
            result: Ok(playlist_page(&["b", "c"], 2, 4)),
        });
        let whole = playlist_ids(&["new", "a", "b", "c"]);
        assert_eq!(listed_playlists(&app), whole);
        // Another answer for a page already taken adds nothing.
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: new,
            result: Ok(playlist_page(&["b", "c"], 2, 4)),
        });
        assert_eq!(listed_playlists(&app), whole);
    }

    /// An offset does not say which load a page belongs to. Once the
    /// reloaded list has asked for its own second page, the old load's
    /// second page, arriving late at the same offset, is still not taken:
    /// taking it ended the list early and dropped the real second page.
    #[test]
    fn an_old_playlist_page_at_the_offset_the_reload_asked_for_is_not_taken() {
        let mut app = headless_app();
        app.backend.set_offline(true);

        app.load_playlists();
        let old = app.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: old,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        assert_eq!(app.library.playlists_asked, Some(2));
        app.handle_api(ApiResponse::PlaylistFollowChanged {
            id: "new".into(),
            followed: true,
            result: Ok(()),
        });
        let new = app.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: new,
            result: Ok(playlist_page(&["new", "a"], 0, 4)),
        });
        assert_eq!(
            app.library.playlists_asked,
            Some(2),
            "the reloaded list asks for the same offset"
        );

        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: old,
            result: Ok(playlist_page(&["c"], 2, 3)),
        });
        assert_eq!(
            listed_playlists(&app),
            playlist_ids(&["new", "a"]),
            "the old load's page is not taken"
        );
        assert_eq!(
            app.library.playlists_asked,
            Some(2),
            "the reloaded list still waits for its own page"
        );

        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation: new,
            result: Ok(playlist_page(&["b", "c"], 2, 4)),
        });
        assert_eq!(
            listed_playlists(&app),
            playlist_ids(&["new", "a", "b", "c"])
        );
    }

    /// A later page that failed is no longer on its way, so another answer
    /// for it is not taken into the list.
    #[test]
    fn a_failed_playlist_page_is_no_longer_awaited() {
        let mut app = headless_app();
        app.backend.set_offline(true);

        app.load_playlists();
        let generation = app.library.playlists_generation;
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation,
            result: Ok(playlist_page(&["a", "b"], 0, 3)),
        });
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(app.library.playlists_asked, None);
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 2,
            generation,
            result: Ok(playlist_page(&["c"], 2, 3)),
        });
        assert_eq!(listed_playlists(&app), playlist_ids(&["a", "b"]));
    }

    /// Signing out forgets the library, but not which load came last: a
    /// page the old account asked for does not become the next account's
    /// list.
    #[test]
    fn a_playlist_page_asked_for_before_sign_out_is_not_taken() {
        let mut app = headless_app();
        app.backend.set_offline(true);

        app.load_playlists();
        let old = app.library.playlists_generation;
        app.reset_data();
        app.load_playlists();
        assert_ne!(app.library.playlists_generation, old);
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: old,
            result: Ok(playlist_page(&["theirs"], 0, 1)),
        });
        assert!(app.library.playlists.is_loading());
    }

    /// The streaming session does not always name a playlist's owner. The
    /// account's own name stands in for its own lists, the library list's
    /// for the rest it holds, in whichever order the answers arrive, and
    /// a name Spotify gave stays.
    #[test]
    fn a_header_without_an_owner_name_takes_a_known_one() {
        use crate::api::models::{Owner, User};
        let owned_by = |id: &str, name: Option<&str>| Owner {
            id: Some(id.into()),
            display_name: name.map(str::to_string),
            uri: None,
        };
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.user = Some(User {
            id: "me".into(),
            display_name: Some("Mine".into()),
            ..User::default()
        });
        app.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl2".into(),
            owner: owned_by("other", Some("Molly C.")),
            ..Playlist::default()
        }]);
        for id in ["pl1", "pl2", "pl3"] {
            app.playlist_pages.insert(
                id.into(),
                PlaylistPage {
                    generation: 1,
                    ..Default::default()
                },
            );
        }
        let header = |id: &str, owner: Owner| ApiResponse::Playlist {
            id: id.into(),
            generation: 1,
            result: Ok(Playlist {
                id: id.into(),
                owner,
                ..Playlist::default()
            }),
        };
        let shown = |app: &App, id: &str| {
            app.playlist_pages[id]
                .playlist
                .get()
                .unwrap()
                .owner_name()
                .to_string()
        };
        app.handle_api(header("pl1", owned_by("me", None)));
        app.handle_api(header("pl2", owned_by("other", None)));
        app.handle_api(header("pl3", owned_by("nobody", None)));
        assert_eq!(shown(&app, "pl1"), "Mine", "the account's own name");
        assert_eq!(shown(&app, "pl2"), "Molly C.", "the library list's");
        assert_eq!(
            shown(&app, "pl3"),
            "nobody",
            "the id until someone names them"
        );

        // The list can arrive after the header: the page takes the name
        // then, and a name Spotify already gave stays.
        app.handle_api(header("pl2", owned_by("other", Some("Molly"))));
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![
                    Playlist {
                        id: "pl2".into(),
                        owner: owned_by("other", Some("Molly C.")),
                        ..Playlist::default()
                    },
                    Playlist {
                        id: "pl3".into(),
                        owner: owned_by("nobody", Some("Nobody")),
                        ..Playlist::default()
                    },
                ],
                ..Default::default()
            }),
        });
        assert_eq!(shown(&app, "pl3"), "Nobody");
        assert_eq!(shown(&app, "pl2"), "Molly");
    }

    /// The streaming session carries no cover for a playlist without one
    /// of its own, where the Web API composes a mosaic; the library list
    /// holds that mosaic, in whichever order the answers arrive.
    #[test]
    fn a_header_without_a_cover_takes_the_library_lists() {
        let cover = |url: &str| {
            vec![Image {
                url: url.into(),
                width: Some(640),
                height: Some(640),
            }]
        };
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            images: cover("https://mosaic.scdn.co/640/pl1"),
            ..Playlist::default()
        }]);
        for id in ["pl1", "pl2", "pl3"] {
            app.playlist_pages.insert(
                id.into(),
                PlaylistPage {
                    generation: 1,
                    ..Default::default()
                },
            );
        }
        let header = |id: &str, images: Vec<Image>| ApiResponse::Playlist {
            id: id.into(),
            generation: 1,
            result: Ok(Playlist {
                id: id.into(),
                images,
                ..Playlist::default()
            }),
        };
        let shown = |app: &App, id: &str| {
            app.playlist_pages[id]
                .playlist
                .get()
                .unwrap()
                .images
                .iter()
                .map(|image| image.url.clone())
                .collect::<Vec<_>>()
        };
        app.handle_api(header("pl1", Vec::new()));
        app.handle_api(header("pl2", cover("https://i.scdn.co/image/own")));
        app.handle_api(header("pl3", Vec::new()));
        assert_eq!(
            shown(&app, "pl1"),
            ["https://mosaic.scdn.co/640/pl1"],
            "the library list's mosaic"
        );
        assert_eq!(
            shown(&app, "pl2"),
            ["https://i.scdn.co/image/own"],
            "a cover of its own stays"
        );
        assert!(shown(&app, "pl3").is_empty(), "nothing to take it from");

        // The list can arrive after the header: the page takes the cover
        // then, and one the header carried stays.
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![
                    Playlist {
                        id: "pl2".into(),
                        images: cover("https://mosaic.scdn.co/640/pl2"),
                        ..Playlist::default()
                    },
                    Playlist {
                        id: "pl3".into(),
                        images: cover("https://mosaic.scdn.co/640/pl3"),
                        ..Playlist::default()
                    },
                ],
                ..Default::default()
            }),
        });
        assert_eq!(shown(&app, "pl3"), ["https://mosaic.scdn.co/640/pl3"]);
        assert_eq!(shown(&app, "pl2"), ["https://i.scdn.co/image/own"]);
    }

    #[test]
    fn volume_conversions_round_trip() {
        assert_eq!(volume_to_percent(u16::MAX), 100);
        assert_eq!(volume_to_percent(0), 0);
        assert_eq!(volume_to_percent(percent_to_volume(70)), 70);
        assert_eq!(percent_to_volume(200), u16::MAX);
    }

    /// Toggling always-on-top pushes the matching level to the live Winamp
    /// window, so the change takes effect without recreating the window.
    #[test]
    fn the_winamp_window_follows_the_on_top_toggle_live() {
        assert_eq!(
            winamp_on_top_level(true, true),
            Some(egui::WindowLevel::AlwaysOnTop)
        );
        assert_eq!(
            winamp_on_top_level(true, false),
            Some(egui::WindowLevel::Normal)
        );
    }

    /// The setting lives in the big window's Settings page. Toggling it there
    /// must never force the big window on top, so no level command is sent.
    #[test]
    fn the_big_window_never_follows_the_on_top_toggle() {
        assert_eq!(winamp_on_top_level(false, true), None);
        assert_eq!(winamp_on_top_level(false, false), None);
    }

    /// The level set at window creation does not stick on X11, so opening the
    /// Winamp window with always-on-top saved must schedule a re-assert.
    #[test]
    fn opening_the_winamp_window_on_top_schedules_a_reassert() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.settings.winamp_window = true;
        app.settings.winamp_on_top = true;
        app.attach(&ctx);
        assert_eq!(app.winamp_level_reassert, 3);
    }

    /// Opening the Winamp window without always-on-top re-asserts nothing.
    #[test]
    fn opening_the_winamp_window_without_on_top_schedules_no_reassert() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.settings.winamp_window = true;
        app.settings.winamp_on_top = false;
        app.attach(&ctx);
        assert_eq!(app.winamp_level_reassert, 0);
    }

    #[test]
    fn unsupported_on_top_controls_keep_the_saved_preference_without_commands() {
        for saved in [false, true] {
            let ctx = egui::Context::default();
            let mut app = headless_app();
            app.settings.winamp_window = true;
            app.settings.winamp_on_top = saved;
            app.window_level_supported = false;
            app.attach(&ctx);
            assert_eq!(app.winamp_level_reassert, 0);
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.apply(Action::ToggleWinampOnTop, ui.ctx());
                app.push_winamp_level(ui.ctx());
            });
            output.textures_delta.clear();
            assert_eq!(app.settings.winamp_on_top, saved);
            assert!(!app.settings_dirty);
            assert!(
                !output.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .iter()
                    .any(|command| matches!(command, egui::ViewportCommand::WindowLevel(_)))
            );
            app.backend.shutdown();
        }
    }

    #[test]
    fn returning_to_the_mini_player_restores_its_position_and_shade() {
        let mut app = headless_app();
        app.settings.winamp_window = true;
        app.settings.winamp_shaded = true;
        app.winamp.last_pos = Some([300.0, 200.0]);
        app.last_window_size = Some([1024.0, 768.0]);
        app.last_window_pos = Some([100.0, 100.0]);

        let main_ctx = egui::Context::default();
        app.apply(Action::ToggleWinampWindow, &main_ctx);
        app.attach(&main_ctx);
        app.apply(Action::ToggleWinampWindow, &main_ctx);

        let mini_ctx = egui::Context::default();
        let mut output = mini_ctx.run_ui(Default::default(), |_ui| app.attach(&mini_ctx));
        output.textures_delta.clear();
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(app.settings.winamp_window);
        assert!(app.settings.winamp_shaded);
        assert!(commands.contains(&egui::ViewportCommand::Fullscreen(false)));
        assert!(commands.contains(&egui::ViewportCommand::Maximized(false)));
        assert!(
            commands.contains(&egui::ViewportCommand::OuterPosition(egui::pos2(
                300.0, 200.0
            )))
        );
        assert_eq!(app.session_window_size, Some([1024.0, 768.0]));
        assert_eq!(app.session_window_pos, Some([100.0, 100.0]));
    }

    /// A window left maximized or full screen comes back that way, instead of
    /// being restored down by the session's size and position.
    #[test]
    fn a_window_that_fills_the_screen_keeps_its_state_over_the_session_geometry() {
        // On Windows only fullscreen lyrics make the frameless main window
        // full screen, so a restored full screen is left instead; see
        // a_window_closed_in_fullscreen_lyrics_returns_to_its_previous_mode.
        let cases: &[(&str, Option<bool>, Option<bool>)] = if cfg!(windows) {
            &[("maximized", Some(true), None)]
        } else {
            &[
                ("maximized", Some(true), None),
                ("full screen", None, Some(true)),
            ]
        };
        for &(name, maximized, fullscreen) in cases {
            let mut app = headless_app();
            app.session_window_size = Some([1024.0, 768.0]);
            app.session_window_pos = Some([100.0, 150.0]);

            let ctx = egui::Context::default();
            let mut raw_input = egui::RawInput::default();
            let viewport = raw_input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default();
            viewport.maximized = maximized;
            viewport.fullscreen = fullscreen;

            let mut output = ctx.run_ui(raw_input, |_ui| app.attach(&ctx));
            output.textures_delta.clear();
            let commands = &output
                .viewport_output
                .get(&egui::ViewportId::ROOT)
                .expect("the root viewport")
                .commands;
            assert!(
                !commands.iter().any(|command| matches!(
                    command,
                    egui::ViewportCommand::InnerSize(_) | egui::ViewportCommand::OuterPosition(_)
                )),
                "a {name} window is neither resized nor moved: {commands:?}"
            );
            app.backend.shutdown();
        }
    }

    /// Closing the app in fullscreen lyrics leaves eframe to restore the
    /// window full screen without them, and on Windows nothing else could
    /// leave it. The next start returns it to the mode the lyrics came from.
    #[test]
    fn a_window_closed_in_fullscreen_lyrics_returns_to_its_previous_mode() {
        use crate::settings::WindowMode;
        use egui::ViewportCommand::{Fullscreen, InnerSize, Maximized};
        let ordinary = WindowMode::default();
        let maximized = WindowMode {
            maximized: true,
            ..WindowMode::default()
        };
        let full_screen = WindowMode {
            fullscreen: true,
            ..WindowMode::default()
        };
        for (left, expected, resized) in [
            (Some(ordinary), vec![Fullscreen(false)], true),
            (
                Some(maximized),
                vec![Fullscreen(false), Maximized(true)],
                false,
            ),
            (Some(full_screen), vec![Fullscreen(true)], false),
            // A session from before this was remembered: Windows can only
            // have been in fullscreen lyrics, other desktops keep the state.
            (
                None,
                if cfg!(windows) {
                    vec![Fullscreen(false)]
                } else {
                    vec![]
                },
                cfg!(windows),
            ),
        ] {
            let mut app = headless_app();
            app.session_window_size = Some([1024.0, 768.0]);
            app.session_lyrics_fullscreen_from = left;

            let ctx = egui::Context::default();
            let mut raw_input = egui::RawInput::default();
            raw_input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .fullscreen = Some(true);
            let mut output = ctx.run_ui(raw_input, |_ui| app.attach(&ctx));
            output.textures_delta.clear();
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            let modes: Vec<_> = commands
                .iter()
                .filter(|command| matches!(command, Fullscreen(_) | Maximized(_)))
                .cloned()
                .collect();
            assert_eq!(modes, expected, "{left:?}");
            assert_eq!(
                commands
                    .iter()
                    .any(|command| matches!(command, InnerSize(_))),
                resized,
                "{left:?}: an ordinary window takes the session's size back"
            );
            assert_eq!(app.session_lyrics_fullscreen_from, None);
            app.backend.shutdown();
        }
    }

    /// The session remembers fullscreen lyrics and the mode they left, and a
    /// session file from before the field existed still loads.
    #[test]
    fn the_session_remembers_the_mode_fullscreen_lyrics_left() {
        let mut app = test_app("lyrics-fullscreen-session");
        app.lyrics_fullscreen = Some(false);
        app.lyrics_restore_maximized = true;
        app.save_session();
        let session = SessionState::load(&app.dirs.session_file());
        assert_eq!(
            session.lyrics_fullscreen_from,
            Some(crate::settings::WindowMode {
                fullscreen: false,
                maximized: true,
            })
        );

        app.lyrics_fullscreen = None;
        app.save_session();
        let text = std::fs::read_to_string(app.dirs.session_file()).unwrap();
        assert!(!text.contains("lyrics_fullscreen_from"));
        assert_eq!(
            SessionState::load(&app.dirs.session_file()).lyrics_fullscreen_from,
            None
        );
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(app.dirs.config.parent().unwrap());
    }

    /// One frame of touchpad scrolling at `time`, with its wheel events.
    #[cfg(target_os = "linux")]
    fn touchpad_frame(
        app: &mut App,
        ctx: &egui::Context,
        time: f64,
        events: &[(egui::TouchPhase, f32)],
    ) {
        let mut raw_input = egui::RawInput {
            time: Some(time),
            ..Default::default()
        };
        for &(phase, delta) in events {
            raw_input.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, delta),
                phase,
                modifiers: egui::Modifiers::default(),
            });
        }
        let mut output = ctx.run_ui(raw_input, |_ui| app.lock_scroll_axis(ctx));
        output.textures_delta.clear();
    }

    /// Scrolls for a few frames, as a finger moving quickly down the pad.
    #[cfg(target_os = "linux")]
    fn touchpad_swipe(app: &mut App, ctx: &egui::Context, phase: egui::TouchPhase) {
        use egui::TouchPhase::Move;
        touchpad_frame(app, ctx, 0.0, &[(phase, 20.0)]);
        for frame in 1..6 {
            touchpad_frame(app, ctx, f64::from(frame) * 0.016, &[(Move, 20.0)]);
        }
    }

    /// Lifting the fingers mid-swipe carries the page on.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_touchpad_flick_glides_after_the_lift() {
        use egui::TouchPhase::{End, Start};
        let mut app = headless_app();
        let ctx = egui::Context::default();
        touchpad_swipe(&mut app, &ctx, Start);
        touchpad_frame(&mut app, &ctx, 0.1, &[(End, 0.0)]);
        assert!(app.glide.is_some());
    }

    /// Fingers that stop and rest on the pad do not fling the page, while
    /// they rest or when they later lift (#503).
    #[cfg(target_os = "linux")]
    #[test]
    fn resting_fingers_do_not_glide_where_the_lift_is_announced() {
        use egui::TouchPhase::{End, Start};
        let mut app = headless_app();
        let ctx = egui::Context::default();
        touchpad_swipe(&mut app, &ctx, Start);
        std::thread::sleep(Duration::from_millis(200));
        touchpad_frame(&mut app, &ctx, 0.3, &[]);
        assert!(app.glide.is_none(), "resting is not a lift");
        touchpad_frame(&mut app, &ctx, 1.0, &[(End, 0.0)]);
        assert!(app.glide.is_none(), "a lift after resting has no speed");
    }

    /// X11 never says when fingers lift, so a quiet gap still ends the
    /// gesture and glides as before.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_quiet_gap_glides_where_the_lift_is_never_announced() {
        use egui::TouchPhase::Move;
        let mut app = headless_app();
        let ctx = egui::Context::default();
        touchpad_swipe(&mut app, &ctx, Move);
        std::thread::sleep(Duration::from_millis(200));
        touchpad_frame(&mut app, &ctx, 0.1, &[]);
        assert!(app.glide.is_some());
    }

    #[test]
    fn a_demo_window_keeps_its_requested_geometry_over_the_saved_session() {
        let mut app = headless_app();
        crate::demo::populate(&mut app);
        app.session_window_size = Some([1100.0, 700.0]);
        app.session_window_pos = Some([100.0, 150.0]);

        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |_ui| app.attach(&ctx));
        output.textures_delta.clear();
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            !commands.iter().any(|command| matches!(
                command,
                egui::ViewportCommand::InnerSize(_) | egui::ViewportCommand::OuterPosition(_)
            )),
            "the last real session must not move or resize the demo: {commands:?}"
        );
        app.backend.shutdown();
    }

    /// The song the last session ended on is shown, paused, at the position
    /// it stopped at, so a cold start does not look like an empty player.
    #[test]
    fn the_remembered_song_is_shown_paused_at_its_position() {
        use crate::api::models::{Album, ArtistRef, Track};
        let mut app = headless_app();
        app.resume_track = Some("spotify:track:abc".into());
        app.resume_position_ms = 19_566;
        assert!(
            app.now_playing().is_none(),
            "nothing to show until the song's details arrive"
        );
        app.track_cache.insert(
            "abc".into(),
            Track {
                id: Some("abc".into()),
                uri: "spotify:track:abc".into(),
                name: "Karma Police".into(),
                artists: vec![ArtistRef {
                    id: None,
                    name: "Radiohead".into(),
                    uri: None,
                }],
                album: Some(Album {
                    id: "ok".into(),
                    name: "OK Computer".into(),
                    ..Default::default()
                }),
                duration_ms: 264_000,
                ..Default::default()
            },
        );
        let now = app.now_playing().expect("the remembered song is shown");
        assert!(now.resuming);
        assert!(!now.playing, "it is shown paused, not played");
        assert_eq!(now.title, "Karma Police");
        assert_eq!(now.subtitle, "Radiohead");
        assert_eq!(now.album_name, "OK Computer");
        assert_eq!(now.duration_ms, 264_000);
        assert_eq!(
            now.position_ms, 19_566,
            "the bar sits where the listener left it, not at zero"
        );
    }

    /// Drawing the remembered song must not be mistaken for a song that
    /// started: that would rewind the very position it exists to show.
    #[test]
    fn showing_the_remembered_song_keeps_its_position() {
        use crate::api::models::Track;
        let mut app = headless_app();
        app.resume_track = Some("spotify:track:abc".into());
        app.resume_position_ms = 19_566;
        app.track_cache.insert(
            "abc".into(),
            Track {
                id: Some("abc".into()),
                uri: "spotify:track:abc".into(),
                duration_ms: 264_000,
                ..Default::default()
            },
        );
        app.on_now_playing_changed();
        assert_eq!(app.resume_position_ms, 19_566);
        app.save_session();
        assert_eq!(
            app.resume_position_ms, 19_566,
            "closing again must not lose the position"
        );
    }

    /// Dragging the bar before pressing play moves where play will land.
    #[test]
    fn seeking_the_remembered_song_moves_the_resume_point() {
        let mut app = headless_app();
        app.resume_track = Some("spotify:track:abc".into());
        app.resume_position_ms = 19_566;
        app.seek(90_000);
        assert_eq!(app.resume_position_ms, 90_000);
    }

    /// Play resumes the remembered track at its saved position.
    #[test]
    fn pressing_play_on_a_cold_start_does_not_restart_the_song() {
        let mut app = headless_app();
        app.resume_context = Some("spotify:playlist:pl1".into());
        app.resume_track = Some("spotify:track:abc".into());
        app.resume_position_ms = 19_566;
        app.toggle_play();
        let request = app
            .queued_play
            .as_ref()
            .expect("the resumed play is held for the engine");
        assert_eq!(
            request.context_uri.as_deref(),
            Some("spotify:playlist:pl1"),
            "it resumes inside the playlist it was left in"
        );
        assert_eq!(request.offset_uri.as_deref(), Some("spotify:track:abc"));
        assert_eq!(
            request.position_ms, 19_566,
            "the song resumes where it stopped, not at zero"
        );
    }

    /// A media key can arrive before startup has reported that the saved
    /// local player is connecting. Its intent must survive that race.
    #[test]
    fn pressing_play_while_startup_connects_is_held_for_the_player() {
        let mut app = headless_app();
        app.local_ready = false;
        app.local_playback = LocalPlayback::Unavailable;
        app.auth = AuthStatus::Starting;
        app.settings.playback_authorized = true;
        app.resume_track = Some("spotify:track:abc".into());

        app.toggle_play();

        assert!(app.queued_play.is_some());
        assert!(!app.show_devices, "startup is not a missing-device error");
    }

    /// Previous and Next move a restored track without starting playback.
    #[test]
    fn the_transport_works_on_a_restored_song_without_playing_it() {
        use crate::api::models::{PlayableItem, PlaylistItem, Track};
        use crate::model::PagedList;
        let row = |uri: &str| PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                id: Some(uri.rsplit(':').next().unwrap().into()),
                uri: uri.into(),
                name: uri.into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.playlist_pages.insert(
            "pl1".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        row("spotify:track:one"),
                        row("spotify:track:two"),
                        row("spotify:track:three"),
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.resume_context = Some("spotify:playlist:pl1".into());
        app.resume_track = Some("spotify:track:two".into());
        app.resume_position_ms = 19_566;
        assert!(app.resume_only(), "loaded and current, but not playing");

        // Next steps to the following song, at its start, still not playing.
        app.apply(Action::Next, &ctx);
        assert_eq!(app.resume_track.as_deref(), Some("spotify:track:three"));
        assert_eq!(app.resume_position_ms, 0);
        assert!(
            app.queued_play.is_none() && app.local_list.is_none(),
            "skipping must not start the restored song"
        );
        // Use loaded row details for immediate display.
        let now = app.now_playing().expect("the new song is shown");
        assert!(now.resuming && !now.playing);
        assert_eq!(now.uri, "spotify:track:three");

        // Previous steps back from the start of a song.
        app.apply(Action::Previous, &ctx);
        assert_eq!(app.resume_track.as_deref(), Some("spotify:track:two"));

        // Past the threshold, Previous restarts instead, as it does while
        // playing.
        app.resume_position_ms = 19_566;
        app.apply(Action::Previous, &ctx);
        assert_eq!(app.resume_track.as_deref(), Some("spotify:track:two"));
        assert_eq!(app.resume_position_ms, 0, "it restarts the song");

        // The ends of the list wrap rather than dead-ending.
        app.apply(Action::Previous, &ctx);
        assert_eq!(app.resume_track.as_deref(), Some("spotify:track:one"));
        app.apply(Action::Previous, &ctx);
        assert_eq!(app.resume_track.as_deref(), Some("spotify:track:three"));

        // Play starts the selected track in its playlist.
        app.apply(Action::TogglePlay, &ctx);
        let request = app.queued_play.as_ref().expect("play starts it");
        assert_eq!(
            request.context_uri.as_deref(),
            Some("spotify:playlist:pl1"),
            "the playlist it was left in is kept"
        );
        assert_eq!(request.offset_uri.as_deref(), Some("spotify:track:three"));
    }

    /// A restored session keeps shuffle enabled when skipping.
    #[test]
    fn skipping_a_restored_song_keeps_shuffle_on() {
        use crate::api::models::{PlayableItem, PlaylistItem, Track};
        use crate::model::PagedList;
        let row = |uri: &str| PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                uri: uri.into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.playlist_pages.insert(
            "pl1".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![row("spotify:track:one"), row("spotify:track:two")],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.resume_context = Some("spotify:playlist:pl1".into());
        app.resume_track = Some("spotify:track:one".into());
        app.shuffle_wanted = true;
        app.apply(Action::Next, &ctx);
        assert!(app.shuffle_wanted, "shuffle survives the skip");
        assert_eq!(
            app.resume_track.as_deref(),
            Some("spotify:track:two"),
            "a shuffled skip still lands on another song in the context"
        );
    }

    /// The saved queue is restored only with its remembered track.
    #[test]
    fn the_saved_queue_follows_the_resumed_song_only() {
        let mut app = headless_app();
        app.resume_track = Some("spotify:track:abc".into());
        app.resume_queue = vec!["spotify:track:q1".into(), "spotify:track:q2".into()];
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:other".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.on_now_playing_changed();
        assert!(
            app.resume_queue.is_empty(),
            "a fresh start lets the saved queue go"
        );
        assert!(app.session_dirty);
    }

    /// A selected list row loads without shuffle, then enables shuffle.
    #[test]
    fn a_chosen_row_in_a_list_never_loads_shuffled() {
        let request = PlayRequest::tracks(vec!["spotify:track:a".into(), "spotify:track:b".into()])
            .starting_at_index(1);
        let load = local_load(&request, true);
        assert_eq!(load.shuffle, None);
        assert_eq!(load.offset_index, Some(1));
        // Without a chosen row the list may shuffle from the start.
        let request = PlayRequest::tracks(vec!["spotify:track:a".into(), "spotify:track:b".into()]);
        assert_eq!(local_load(&request, true).shuffle, Some(true));
        // A context play keeps its shuffled load; the offset was already
        // picked to match.
        let request = PlayRequest::context("spotify:playlist:x").starting_at_uri("spotify:track:a");
        assert_eq!(local_load(&request, true).shuffle, Some(true));
    }

    fn queued_song(uri: &str) -> crate::api::models::PlayableItem {
        crate::api::models::PlayableItem::Track(crate::api::models::Track {
            uri: uri.into(),
            ..Default::default()
        })
    }

    fn loaded_queue(current: &str, next: &[&str]) -> Loadable<Queue> {
        Loadable::Loaded(Queue {
            currently_playing: Some(queued_song(current)),
            queue: next.iter().map(|uri| queued_song(uri)).collect(),
        })
    }

    fn queue_uris(app: &App) -> (Option<String>, Vec<String>) {
        let queue = app.queue.get().expect("the queue stays loaded");
        (
            queue
                .currently_playing
                .as_ref()
                .map(|item| item.uri().to_string()),
            queue
                .queue
                .iter()
                .map(|item| item.uri().to_string())
                .collect(),
        )
    }

    /// Next moves the queue head to the playing row immediately.
    #[test]
    fn next_pops_the_queue_head_into_now_playing() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:b", "spotify:track:c"]);
        app.apply(Action::Next, &ctx);
        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:b"));
        assert_eq!(next, vec!["spotify:track:c"]);
        assert_eq!(
            app.current_track_uri().as_deref(),
            Some("spotify:track:b"),
            "the popped row is already the one the interface marks as playing"
        );
    }

    /// Next names the next song in the player bar at once, from the queue
    /// row, even when the song was never loaded anywhere else.
    #[test]
    fn next_shows_the_next_songs_title_before_the_engine_loads_it() {
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = Loadable::Loaded(Queue {
            currently_playing: Some(queued_song("spotify:track:a")),
            queue: vec![crate::api::models::PlayableItem::Track(Track {
                id: Some("b".into()),
                uri: "spotify:track:b".into(),
                name: "Second Song".into(),
                ..Default::default()
            })],
        });
        assert!(!app.track_cache.contains_key("b"));
        app.apply(Action::Next, &egui::Context::default());
        let now = app.now_playing().expect("the next song is on show");
        assert_eq!(now.uri, "spotify:track:b");
        assert_eq!(now.title, "Second Song");
    }

    #[test]
    fn a_pending_next_keeps_paused_playback_paused() {
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Paused;
        app.track_cache.insert(
            "b".into(),
            Track {
                uri: "spotify:track:b".into(),
                ..Default::default()
            },
        );
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:b"]);
        app.apply(Action::Next, &egui::Context::default());
        let now = app.now_playing().unwrap();
        assert_eq!(now.uri, "spotify:track:b");
        assert!(
            !now.playing,
            "Next preserves pause while the engine catches up"
        );
    }

    /// A queue restored at startup is useful to show, but it is not evidence
    /// of what follows the player now and must not drive an optimistic skip.
    #[test]
    fn next_does_not_guess_from_an_unanchored_queue() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:honey".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = Loadable::Loaded(Queue {
            currently_playing: None,
            queue: vec![queued_song("spotify:track:stale")],
        });

        app.apply(Action::Next, &ctx);

        assert_eq!(
            app.current_track_uri().as_deref(),
            Some("spotify:track:honey"),
            "an unanchored row is not presented as Next's destination"
        );
        assert!(app.intent_track.is_none());
        assert_eq!(
            queue_uris(&app).1,
            vec!["spotify:track:stale"],
            "the restored queue stays visible until the live queue arrives"
        );
    }

    /// The queue can still disagree with librespot after an anchored skip.
    /// Once the engine reports the track that started, its report is final.
    #[test]
    fn an_engine_track_report_overrules_the_optimistic_queue_marker() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:honey".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue("spotify:track:honey", &["spotify:track:guessed"]);

        app.apply(Action::Next, &ctx);
        assert_eq!(
            app.current_track_uri().as_deref(),
            Some("spotify:track:guessed")
        );

        let mut reported = app.local.clone();
        reported.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:black-dove".into(),
            ..Default::default()
        });
        app.handle_local(reported);

        assert_eq!(
            app.current_track_uri().as_deref(),
            Some("spotify:track:black-dove"),
            "the real engine track wins without waiting for a timeout"
        );
        assert!(app.intent_track.is_none());
    }

    /// Remote state is polled rather than pushed. Ignore a poll already in
    /// flight and one lagging answer, then accept a second report.
    #[test]
    fn remote_track_intent_waits_for_a_fresh_confirming_poll() {
        let mut app = headless_app();
        app.local_ready = false;
        app.selected_device = Some("phone".into());
        app.remote_poll_seq = 7;
        app.expect_track("spotify:track:wanted".into(), 0);

        app.reconcile_remote_track_intent(7, Some("spotify:track:old"));
        assert!(
            app.intent_track.is_some(),
            "the in-flight poll predates the command"
        );

        app.reconcile_remote_track_intent(8, Some("spotify:track:old"));
        assert!(
            app.intent_track.is_some(),
            "one stale Spotify answer is retried"
        );

        app.reconcile_remote_track_intent(9, Some("spotify:track:actual"));
        assert!(
            app.intent_track.is_none(),
            "the second fresh report settles playback"
        );
    }

    /// Going back cancels Next's speculative destination, even when both
    /// commands arrive before the engine has reported either track change.
    #[test]
    fn previous_discards_an_unconfirmed_next_marker() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:b"]);

        app.apply(Action::Next, &ctx);
        assert_eq!(
            app.current_track_uri().as_deref(),
            Some("spotify:track:b"),
            "Next marks its queue head immediately"
        );

        app.apply(Action::Previous, &ctx);
        assert_eq!(
            app.current_track_uri().as_deref(),
            Some("spotify:track:a"),
            "Previous restores the engine's current row instead of holding Next's marker"
        );
    }

    /// A track is removed from Next up as soon as it starts.
    #[test]
    fn a_song_starting_consumes_its_queue_row() {
        let mut app = headless_app();
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:b", "spotify:track:c"]);
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:b".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.on_now_playing_changed();
        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:b"));
        assert_eq!(next, vec!["spotify:track:c"]);
    }

    /// A stale queue response does not undo an optimistic skip.
    #[test]
    fn a_stale_queue_answer_does_not_undo_a_skip() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:b", "spotify:track:c"]);
        app.apply(Action::Next, &ctx);

        let stale = Queue {
            currently_playing: Some(queued_song("spotify:track:a")),
            queue: vec![
                queued_song("spotify:track:b"),
                queued_song("spotify:track:c"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: app.queue_seq,
            result: Ok(stale.clone()),
        });
        let (current, next) = queue_uris(&app);
        assert_eq!(
            current.as_deref(),
            Some("spotify:track:b"),
            "the pop stands"
        );
        assert_eq!(next, vec!["spotify:track:c"]);
        assert!(
            app.queue_recheck_at.is_some(),
            "the stale answer is asked again rather than believed"
        );

        // Accept Spotify's state after the retry limit.
        for _ in 0..QUEUE_STALE_RETRIES {
            app.handle_api(ApiResponse::Queue {
                seq: app.queue_seq,
                result: Ok(stale.clone()),
            });
        }
        let (current, _) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:a"));
    }

    /// A pending addition is not restored after its track starts.
    #[test]
    fn a_played_pending_add_is_not_put_back() {
        let mut app = headless_app();
        app.pending_queue_adds = vec![PendingQueueAdd {
            item: queued_song("spotify:track:b"),
            at: Instant::now(),
            manual_index: 0,
            write: None,
        }];
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:b".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue("spotify:track:b", &["spotify:track:c"]);
        app.reconcile_pending_queue();
        let (_, next) = queue_uris(&app);
        assert_eq!(next, vec!["spotify:track:c"], "no resurrected row on top");
        assert!(
            app.pending_queue_adds.is_empty(),
            "the add has been consumed"
        );
    }

    /// Playing a queue row consumes it and every row before it.
    #[test]
    fn a_chosen_queue_row_plays_at_once_and_takes_the_rows_above() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.manual_queue = vec!["spotify:track:b".into(), "spotify:track:c".into()];
        app.queue = loaded_queue(
            "spotify:track:a",
            &["spotify:track:b", "spotify:track:c", "spotify:track:d"],
        );
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Queue,
                uri: "spotify:track:c".into(),
                index: 1,
            },
            &ctx,
        );
        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:c"));
        assert_eq!(
            next,
            vec!["spotify:track:d"],
            "the rows after the chosen one stay"
        );
        assert_eq!(
            app.current_track_uri().as_deref(),
            Some("spotify:track:c"),
            "the chosen row is marked as playing at once"
        );
        assert!(
            app.manual_queue.is_empty(),
            "hand-queued songs consumed by the jump are let go"
        );
        assert!(app.play_pending("spotify:track:c"));
    }

    /// The click names a song: when the rows have shifted under the
    /// pointer, the song wins over the row number.
    #[test]
    fn a_clicked_queue_row_is_found_by_its_song_when_rows_shifted() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:b", "spotify:track:c"]);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Queue,
                uri: "spotify:track:c".into(),
                index: 0,
            },
            &ctx,
        );
        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:c"));
        assert!(
            next.is_empty(),
            "the row above the chosen song went with it"
        );
    }

    /// Clear queue removes manual rows and preserves matching context rows.
    #[test]
    fn clear_queue_takes_the_hand_queued_rows_and_keeps_the_context() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.manual_queue = vec!["spotify:track:b".into(), "spotify:track:c".into()];
        app.queue = loaded_queue(
            "spotify:track:a",
            &[
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:c",
                "spotify:track:d",
            ],
        );
        assert!(app.can_clear_queue());
        app.apply(Action::ClearQueue, &ctx);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:c", "spotify:track:d"],
            "one queued c goes, the context's own c stays"
        );
        assert!(app.manual_queue.is_empty());
        assert!(
            app.queue_recheck_at.is_some(),
            "a fetch follows to sweep rows queued from other devices"
        );
    }

    /// Rule: clearing takes back the rows Add to queue added, and the
    /// context's own copy of the same song is not one of them, however
    /// recently the song was queued.
    #[test]
    fn clearing_a_just_queued_song_leaves_the_contexts_copy_of_it() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        // What is playing comes to b on its own later on.
        app.queue = loaded_queue(
            "spotify:track:a",
            &[
                "spotify:track:ctx1",
                "spotify:track:b",
                "spotify:track:ctx2",
            ],
        );
        app.apply(
            Action::AddToQueue {
                uri: "spotify:track:b".into(),
                label: "b".into(),
            },
            &ctx,
        );
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:b",
                "spotify:track:ctx1",
                "spotify:track:b",
                "spotify:track:ctx2",
            ],
            "one row queued on top, the context's own b still below"
        );
        assert!(app.can_clear_queue());
        app.apply(Action::ClearQueue, &ctx);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:ctx1",
                "spotify:track:b",
                "spotify:track:ctx2",
            ],
            "the queued b goes and the context keeps the b it was going to play"
        );
    }

    /// Add to queue inserts after manual queue rows and before context rows.
    #[test]
    fn play_next_queues_after_the_songs_already_queued() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue(
            "spotify:track:a",
            &["spotify:track:ctx1", "spotify:track:ctx2"],
        );
        app.apply(
            Action::AddToQueue {
                uri: "spotify:track:b".into(),
                label: "b".into(),
            },
            &ctx,
        );
        app.apply(
            Action::AddToQueue {
                uri: "spotify:track:c".into(),
                label: "c".into(),
            },
            &ctx,
        );
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ],
            "queued songs keep their order and stay ahead of the context"
        );
        assert_eq!(
            app.toasts.last().map(|toast| toast.message.as_str()),
            Some("c added to queue")
        );
    }

    fn album_queue_track(id: &str) -> Track {
        Track {
            uri: format!("spotify:track:{id}"),
            name: id.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn starting_a_queued_album_preserves_its_manual_copies_and_clear_button() {
        let ctx = egui::Context::default();
        for advance in ["clear", "next", "natural"] {
            let mut app = headless_app();
            app.local.connected = true;
            app.local.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:old".into(),
                ..Default::default()
            });
            app.local.playback = Playback::Playing;
            app.queue = loaded_queue("spotify:track:old", &["spotify:track:old-next"]);
            app.on_now_playing_changed();
            app.album_pages.insert(
                "album".into(),
                AlbumPage {
                    tracks: PagedList {
                        items: ["intro", "second", "third"]
                            .into_iter()
                            .map(album_queue_track)
                            .collect(),
                        loaded_once: true,
                        next_offset: None,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            app.apply(
                Action::AddToQueue {
                    uri: "spotify:album:album".into(),
                    label: "Album".into(),
                },
                &ctx,
            );
            app.add_to_queue("spotify:track:extra".into(), "Extra".into());
            let manual = app.manual_queue.clone();
            assert_eq!(manual.len(), 4);
            app.apply(
                Action::PlayContext {
                    uri: "spotify:album:album".into(),
                    offset_uri: None,
                    offset_index: None,
                },
                &ctx,
            );
            let mut started = app.local.clone();
            started.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:intro".into(),
                ..Default::default()
            });
            started.track_sequence += 1;
            app.handle_local(started);
            assert_eq!(
                app.manual_queue, manual,
                "the album start is not a queued play"
            );

            let next = [
                "spotify:track:intro",
                "spotify:track:second",
                "spotify:track:third",
                "spotify:track:extra",
                "spotify:track:second",
                "spotify:track:third",
            ];
            let Loadable::Loaded(fetched) = loaded_queue("spotify:track:intro", &next) else {
                unreachable!();
            };
            app.handle_api(ApiResponse::Queue {
                seq: app.queue_seq,
                result: Ok(fetched),
            });
            assert_eq!(queue_uris(&app).1, next);
            assert_eq!(app.queued_rows_len(), 4);
            assert!(app.can_clear_queue());

            // Restarting the same album also keeps its separate queued copy.
            app.apply(
                Action::PlayContext {
                    uri: "spotify:album:album".into(),
                    offset_uri: None,
                    offset_index: None,
                },
                &ctx,
            );
            let mut restarted = app.local.clone();
            restarted.track_sequence += 1;
            app.handle_local(restarted);
            assert_eq!(app.manual_queue, manual);
            assert_eq!(queue_uris(&app).1, next);

            if advance != "clear" {
                if advance == "next" {
                    app.apply(Action::Next, &ctx);
                    assert_eq!(app.queued_rows_len(), 3);
                }
                // The queued intro has the same URI and metadata as the album's
                // intro. Both Next and natural completion consume it once.
                let mut queued_intro = app.local.clone();
                queued_intro.track_sequence += 1;
                app.handle_local(queued_intro);
                assert_eq!(app.manual_queue, manual[1..]);
                assert_eq!(queue_uris(&app).1, next[1..]);
                assert_eq!(app.queued_rows_len(), 3);
                assert!(app.can_clear_queue());
            }

            app.apply(Action::ClearQueue, &ctx);
            assert_eq!(queue_uris(&app).1, next[4..]);
            assert!(!app.can_clear_queue());
            app.backend.shutdown();
        }
    }

    #[test]
    fn replaying_a_song_only_consumes_a_queue_copy_when_the_queue_advances() {
        let ctx = egui::Context::default();
        for repeat in [RepeatMode::Off, RepeatMode::Track] {
            let mut app = headless_app();
            app.local = LocalState {
                connected: true,
                playback: Playback::Playing,
                repeat,
                track: Some(crate::player::LocalTrack {
                    uri: "spotify:track:a".into(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            app.on_now_playing_changed();
            app.manual_queue = vec!["spotify:track:b".into(), "spotify:track:b".into()];
            app.queue = loaded_queue(
                "spotify:track:a",
                &[
                    "spotify:track:b",
                    "spotify:track:b",
                    "spotify:track:context",
                ],
            );
            app.apply(Action::Next, &ctx);
            let mut started = app.local.clone();
            started.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:b".into(),
                ..Default::default()
            });
            started.track_sequence += 1;
            app.handle_local(started);
            assert_eq!(app.manual_queue, ["spotify:track:b"]);
            assert_eq!(app.queued_rows_len(), 1);

            let mut again = app.local.clone();
            again.track_sequence += 1;
            app.handle_local(again);
            if repeat == RepeatMode::Track {
                assert_eq!(app.manual_queue, ["spotify:track:b"]);
                assert_eq!(
                    app.queued_rows_len(),
                    1,
                    "repeat does not advance the queue"
                );
                app.apply(Action::Next, &ctx);
                let mut skipped = app.local.clone();
                skipped.track_sequence += 1;
                app.handle_local(skipped);
            }
            assert!(app.manual_queue.is_empty());
            assert_eq!(queue_uris(&app).1, ["spotify:track:context"]);
            app.backend.shutdown();
        }
    }

    #[test]
    fn queue_album_uses_every_cached_song_in_order_and_preserves_duplicates() {
        for local in [false, true] {
            let mut app = test_app(if local {
                "album-queue-local"
            } else {
                "album-queue-remote"
            });
            app.local_ready = local;
            app.local.playback = Playback::Playing;
            app.local.connected = local;
            app.local.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:current".into(),
                ..Default::default()
            });
            app.queue = loaded_queue(
                "spotify:track:current",
                &["spotify:track:manual", "spotify:track:context"],
            );
            app.manual_queue.push("spotify:track:manual".into());
            let mut tracks: Vec<_> = (0..120)
                .map(|n| album_queue_track(&format!("song{n}")))
                .collect();
            tracks.push(album_queue_track("song0"));
            let expected: Vec<_> = tracks.iter().map(|track| track.uri.clone()).collect();
            tracks.push(Track {
                is_playable: Some(false),
                ..album_queue_track("unavailable")
            });
            tracks.push(Track::default());
            tracks.push(Track {
                is_local: true,
                ..album_queue_track("local")
            });
            app.album_pages.insert(
                "album".into(),
                AlbumPage {
                    tracks: PagedList {
                        items: tracks,
                        loaded_once: true,
                        next_offset: None,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            let action = Action::AddToQueue {
                uri: "spotify:album:album".into(),
                label: "Album".into(),
            };
            app.apply(action.clone(), &egui::Context::default());
            app.apply(action, &egui::Context::default());
            let (_, queued) = queue_uris(&app);
            assert_eq!(queued[0], "spotify:track:manual");
            assert_eq!(queued[1..queued.len() - 1], expected);
            assert_eq!(queued.last().unwrap(), "spotify:track:context");
            if local {
                assert_eq!(app.backend.take_queued_tracks(), expected);
                assert!(app.backend.take_queue_requests().is_empty());
            } else {
                let requests = app.backend.take_queue_requests();
                assert_eq!(requests.len(), 1);
                assert!(
                    matches!(&requests[0], ApiRequest::AddManyToQueue { uris, .. } if uris == &expected)
                );
            }
            assert!(app.pending_album_queues.is_empty());
            app.add_to_queue("spotify:track:after".into(), "After".into());
            let (_, queued) = queue_uris(&app);
            assert_eq!(
                queued[queued.len() - 2],
                "spotify:track:after",
                "a later song stays after an album longer than 100 tracks"
            );
            app.backend.shutdown();
        }
    }

    #[test]
    fn queue_album_fetches_all_pages_before_appending_and_ignores_old_pages() {
        let mut app = test_app("album-queue-pages");
        app.queue = loaded_queue("spotify:track:current", &["spotify:track:context"]);
        app.add_to_queue("spotify:album:album".into(), "Album".into());
        let request = app.album_queue_serial;
        let requests = app.backend.take_queue_requests();
        assert!(matches!(
            &requests[..],
            [ApiRequest::AlbumQueueTracks { offset: 0, .. }]
        ));
        let page = crate::api::models::Page {
            items: vec![album_queue_track("one"), album_queue_track("two")],
            offset: 0,
            limit: 2,
            total: 3,
            next: Some("next".into()),
        };
        app.handle_api(ApiResponse::AlbumQueueTracks {
            request,
            offset: 0,
            result: Ok(page.clone()),
        });
        assert_eq!(queue_uris(&app).1, ["spotify:track:context"]);
        let requests = app.backend.take_queue_requests();
        assert!(matches!(
            &requests[..],
            [ApiRequest::AlbumQueueTracks { offset: 2, .. }]
        ));
        app.handle_api(ApiResponse::AlbumQueueTracks {
            request,
            offset: 0,
            result: Ok(page),
        });
        assert!(app.backend.take_queue_requests().is_empty());
        app.handle_api(ApiResponse::AlbumQueueTracks {
            request,
            offset: 2,
            result: Ok(crate::api::models::Page {
                items: vec![album_queue_track("one")],
                offset: 2,
                limit: 2,
                total: 3,
                next: None,
            }),
        });
        assert_eq!(
            queue_uris(&app).1,
            [
                "spotify:track:one",
                "spotify:track:two",
                "spotify:track:one",
                "spotify:track:context"
            ]
        );
        assert!(app.pending_album_queues.is_empty());
        app.backend.shutdown();
    }

    #[test]
    fn remote_album_queue_keeps_its_rows_while_rate_limited_writes_are_pending() {
        let mut app = test_app("album-queue-stale");
        app.queue = loaded_queue("spotify:track:current", &["spotify:track:context"]);
        let old = app.queue.get().unwrap().clone();
        app.queue_album_tracks(
            vec![
                album_queue_track("a"),
                album_queue_track("b"),
                album_queue_track("a"),
            ],
            "Album".into(),
        );
        let request = app.album_queue_serial;
        for addition in &mut app.pending_queue_adds {
            addition.at = Instant::now() - Duration::from_secs(60);
        }
        app.expire_pending_queue_adds();
        assert_eq!(app.pending_queue_adds.len(), 3);
        for _ in 0..u16::from(u8::MAX) + 2 {
            app.handle_api(ApiResponse::Queue {
                seq: app.queue_seq,
                result: Ok(old.clone()),
            });
            assert_eq!(
                queue_uris(&app).1,
                [
                    "spotify:track:a",
                    "spotify:track:b",
                    "spotify:track:a",
                    "spotify:track:context"
                ]
            );
        }
        app.handle_api(ApiResponse::Queue {
            seq: app.queue_seq,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(
            queue_uris(&app).1,
            [
                "spotify:track:a",
                "spotify:track:b",
                "spotify:track:a",
                "spotify:track:context"
            ]
        );
        app.handle_api(ApiResponse::QueueBatchAdded {
            request,
            added: 3,
            result: Ok(()),
        });
        assert!(app.pending_queue_batches.is_empty());
        assert!(
            app.pending_queue_adds
                .iter()
                .all(|addition| addition.at.elapsed() < Duration::from_secs(1))
        );
        app.backend.shutdown();
    }

    #[test]
    fn rejected_queue_copies_leave_accepted_later_and_context_copies_intact() {
        let mut app = test_app("queue-partial-failure");
        app.queue = loaded_queue("spotify:track:current", &["spotify:track:a"]);
        app.queue_album_tracks(
            vec![
                album_queue_track("a"),
                album_queue_track("a"),
                album_queue_track("b"),
            ],
            "Album".into(),
        );
        let album = app.album_queue_serial;
        // Bypass click debounce: this is a separately selected occurrence.
        app.queue_one("spotify:track:a".into(), "Later A".into(), false);
        let later = app.album_queue_serial;
        app.handle_api(ApiResponse::QueueBatchAdded {
            request: album,
            added: 1,
            result: Err(crate::api::ApiError::Status {
                status: 403,
                message: "Rejected".into(),
            }),
        });
        assert_eq!(app.manual_queue, ["spotify:track:a", "spotify:track:a"]);
        assert_eq!(
            queue_uris(&app).1,
            ["spotify:track:a", "spotify:track:a", "spotify:track:a"]
        );
        assert_eq!(app.pending_queue_adds.len(), 2);
        assert_eq!(app.pending_queue_adds[1].manual_index, 1);
        assert_eq!(app.pending_queue_adds[1].write, Some((later, 0)));
        app.handle_api(ApiResponse::QueueBatchAdded {
            request: later,
            added: 0,
            result: Err(crate::api::ApiError::QuotaExhausted),
        });
        assert_eq!(app.manual_queue, ["spotify:track:a"]);
        assert_eq!(queue_uris(&app).1, ["spotify:track:a", "spotify:track:a"]);
        app.backend.shutdown();
    }

    #[test]
    fn pending_single_keeps_its_name_and_duration_during_and_after_a_cooldown() {
        let mut app = test_app("queue-single-cooldown");
        app.queue = loaded_queue("spotify:track:current", &["spotify:track:context"]);
        let old = app.queue.get().unwrap().clone();
        let track = Track {
            duration_ms: 215_000,
            ..album_queue_track("single")
        };
        app.album_pages.insert(
            "album".into(),
            AlbumPage {
                tracks: PagedList {
                    items: vec![track.clone()],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.add_to_queue(track.uri.clone(), track.name.clone());
        let request = app.album_queue_serial;
        app.album_pages.clear();
        app.track_cache.clear();
        app.pending_queue_adds[0].at = Instant::now() - Duration::from_secs(90);
        app.handle_api(ApiResponse::Queue {
            seq: app.queue_seq,
            result: Ok(old.clone()),
        });
        let row = &app.queue.get().unwrap().queue[0];
        assert_eq!(row.name(), track.name);
        assert_eq!(row.duration_ms(), track.duration_ms);
        app.handle_api(ApiResponse::QueueBatchAdded {
            request,
            added: 1,
            result: Ok(()),
        });
        app.handle_api(ApiResponse::Queue {
            seq: app.queue_seq,
            result: Ok(old),
        });
        let row = &app.queue.get().unwrap().queue[0];
        assert_eq!(row.uri(), track.uri);
        assert_eq!(row.name(), track.name);
        assert_eq!(row.duration_ms(), track.duration_ms);
        app.backend.shutdown();
    }

    #[test]
    fn pending_album_queue_cannot_survive_clear_sign_out_or_device_change() {
        for cancel in ["clear", "sign-out", "device", "failure"] {
            let mut app = test_app(&format!("album-queue-cancel-{cancel}"));
            app.local_ready = true;
            app.queue = loaded_queue("spotify:track:current", &["spotify:track:context"]);
            app.add_to_queue("spotify:album:album".into(), "Album".into());
            let request = app.album_queue_serial;
            app.backend.take_queue_requests();
            match cancel {
                "clear" => app.clear_queue(),
                "sign-out" => app.reset_data(),
                "device" => app.selected_device = Some("phone".into()),
                "failure" => app.handle_api(ApiResponse::AlbumQueueTracks {
                    request,
                    offset: 0,
                    result: Err(crate::api::ApiError::RateLimited),
                }),
                _ => unreachable!(),
            }
            app.handle_api(ApiResponse::AlbumQueueTracks {
                request,
                offset: 0,
                result: Ok(crate::api::models::Page {
                    items: vec![album_queue_track("late")],
                    ..Default::default()
                }),
            });
            assert!(app.manual_queue.is_empty());
            assert!(app.pending_album_queues.is_empty());
            assert!(app.backend.take_queue_requests().is_empty());
            assert!(app.backend.take_queued_tracks().is_empty());
            app.backend.shutdown();
        }
    }

    /// Separate requests may queue duplicates; duplicate click events do not.
    #[test]
    fn asking_play_next_twice_queues_two_rows() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:ctx1"]);
        let add = Action::AddToQueue {
            uri: "spotify:track:b".into(),
            label: "b".into(),
        };
        app.apply(add.clone(), &ctx);
        app.apply(add.clone(), &ctx);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b", "spotify:track:ctx1"],
            "the double-click's second click is not a second wish"
        );
        // Simulate a later request.
        for addition in &mut app.pending_queue_adds {
            addition.at = Instant::now() - QUEUE_ADD_DEBOUNCE;
        }
        app.apply(add, &ctx);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b", "spotify:track:b", "spotify:track:ctx1"],
            "two asks are two rows, one after the other"
        );
    }

    /// A playlist can hold the same song twice. Picking both rows and
    /// choosing Add to queue is one ask for each row, so the song is queued
    /// twice, as the notification says.
    #[test]
    fn a_song_picked_twice_is_queued_twice() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:ctx1"]);
        let add = Action::QueueMany {
            songs: vec![
                ("spotify:track:b".into(), "b".into()),
                ("spotify:track:c".into(), "c".into()),
                ("spotify:track:b".into(), "b".into()),
            ],
        };
        app.apply(add.clone(), &ctx);
        // The same click arriving twice is still one ask.
        let toasts = app.toasts.len();
        app.apply(add.clone(), &ctx);
        assert_eq!(app.toasts.len(), toasts, "no second addition to announce");
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:b",
                "spotify:track:ctx1",
            ],
            "every picked row is queued, in order"
        );
        assert_eq!(
            app.manual_queue,
            vec!["spotify:track:b", "spotify:track:c", "spotify:track:b"]
        );
        assert_eq!(
            app.toasts.last().map(|toast| toast.message.as_str()),
            Some("3 songs added to queue")
        );
        // A later request is separate and must preserve its duplicates too.
        for addition in &mut app.pending_queue_adds {
            addition.at = Instant::now() - QUEUE_ADD_DEBOUNCE;
        }
        app.apply(add, &ctx);
        assert_eq!(
            queue_uris(&app).1,
            [
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:b",
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:b",
                "spotify:track:ctx1"
            ]
        );
    }

    #[test]
    fn a_queue_batch_reports_only_rows_it_added() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:ctx1"]);
        app.apply(
            Action::AddToQueue {
                uri: "spotify:track:b".into(),
                label: "b".into(),
            },
            &ctx,
        );
        app.apply(
            Action::QueueMany {
                songs: vec![
                    ("spotify:track:b".into(), "b".into()),
                    ("spotify:track:c".into(), "c".into()),
                    ("spotify:track:c".into(), "c".into()),
                ],
            },
            &ctx,
        );
        assert_eq!(
            queue_uris(&app).1,
            [
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:c",
                "spotify:track:ctx1"
            ]
        );
        assert_eq!(app.toasts.last().unwrap().message, "2 songs added to queue");
        let toasts = app.toasts.len();
        app.apply(Action::QueueMany { songs: vec![] }, &ctx);
        assert_eq!(app.toasts.len(), toasts, "an empty batch adds nothing");
    }

    /// A response superseded by a newer request is ignored.
    #[test]
    fn an_overtaken_queue_answer_is_dropped_unread() {
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:b"]);
        app.queue_seq = 2;
        let old_story = Queue {
            currently_playing: Some(queued_song("spotify:track:a")),
            queue: Vec::new(),
        };
        app.handle_api(ApiResponse::Queue {
            seq: 1,
            result: Ok(old_story),
        });
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b"],
            "the overtaken answer changed nothing"
        );
        let current_story = Queue {
            currently_playing: Some(queued_song("spotify:track:a")),
            queue: vec![
                queued_song("spotify:track:b"),
                queued_song("spotify:track:c"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: 2,
            result: Ok(current_story),
        });
        let (_, next) = queue_uris(&app);
        assert_eq!(next, vec!["spotify:track:b", "spotify:track:c"]);
    }

    /// Rule: a row you queued is put back until Spotify confirms it, in
    /// its place after the queued section, not on top of it.
    #[test]
    fn a_missing_queued_row_returns_to_its_place() {
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.manual_queue = vec!["spotify:track:b".into(), "spotify:track:c".into()];
        app.pending_queue_adds = vec![PendingQueueAdd {
            item: queued_song("spotify:track:c"),
            at: Instant::now(),
            manual_index: 1,
            write: None,
        }];
        // Spotify's answer knows b already but not c yet.
        app.queue = loaded_queue(
            "spotify:track:a",
            &["spotify:track:b", "spotify:track:ctx1"],
        );
        app.reconcile_pending_queue();
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b", "spotify:track:c", "spotify:track:ctx1"],
            "the missing row comes back after the queued section"
        );
    }

    /// The view splits the queue where the user's own songs end; rows
    /// queued elsewhere or belonging to the context stay below the line.
    #[test]
    fn the_queued_section_covers_only_the_users_own_rows() {
        let mut app = headless_app();
        app.manual_queue = vec!["spotify:track:b".into(), "spotify:track:c".into()];
        app.queue = loaded_queue(
            "spotify:track:a",
            &[
                "spotify:track:b",
                "spotify:track:c",
                "spotify:track:ctx1",
                "spotify:track:c",
            ],
        );
        assert_eq!(
            app.queued_rows_len(),
            2,
            "the context's own copy of c does not count as queued"
        );
        app.manual_queue.clear();
        assert_eq!(app.queued_rows_len(), 0);
    }

    /// Changing shuffle rechecks the queue to reflect the new playback order.
    #[test]
    fn changing_shuffle_schedules_a_queue_recheck() {
        let mut app = headless_app();
        app.queue = loaded_queue("spotify:track:a", &["spotify:track:b"]);
        assert!(app.queue_recheck_at.is_none());
        app.set_shuffle(true);
        assert!(
            app.queue_recheck_at.is_some(),
            "toggling shuffle asks Spotify for the reordered queue"
        );
    }

    /// Toggling shuffle for local playback rechecks the queue, reveals the
    /// new playback order without losing hand-queued songs, and ignores an
    /// old response from before the toggle.
    #[test]
    fn shuffling_local_playback_updates_queue_and_keeps_user_songs() {
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.manual_queue = vec!["spotify:track:manual1".into()];
        app.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
                "spotify:track:ctx3",
            ],
        );
        assert_eq!(app.queued_rows_len(), 1);

        // Toggle shuffle on local player.
        app.set_shuffle(true);
        assert!(
            app.queue_recheck_at.is_some(),
            "toggling shuffle schedules a queue recheck"
        );

        // Start the queue refresh with a new sequence number.
        app.refresh_queue(true);
        let seq = app.queue_seq;

        // A response from before the shuffle command is dropped unread.
        let old_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
                queued_song("spotify:track:ctx3"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: seq - 1,
            result: Ok(old_response),
        });
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
                "spotify:track:ctx3",
            ],
            "the superseded pre-shuffle response is dropped unread"
        );

        // When the fresh response arrives, the context rows reflect the new shuffle
        // order while manually queued rows stay on top.
        let shuffled_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx3"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(shuffled_response),
        });

        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:playing"));
        assert_eq!(
            next,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx3",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ],
            "the user's song stays on top and the shuffled context order is shown"
        );
        assert_eq!(
            app.queued_rows_len(),
            1,
            "the user's queued section is preserved"
        );
    }

    /// Toggling shuffle for remote playback rechecks the queue on confirmation,
    /// adopts the new shuffle order while keeping hand-queued songs, and drops
    /// an overtaken response.
    #[test]
    fn shuffling_remote_playback_updates_queue_and_keeps_user_songs() {
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.local_ready = false;
        app.selected_device = Some("speaker".into());
        app.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                device: Some(crate::api::models::Device {
                    id: Some("speaker".into()),
                    name: "Speaker".into(),
                    is_active: true,
                    ..Default::default()
                }),
                item: Some(crate::api::models::PlayableItem::Track(
                    crate::api::models::Track {
                        uri: "spotify:track:playing".into(),
                        ..Default::default()
                    },
                )),
                is_playing: true,
                shuffle_state: false,
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        app.manual_queue = vec!["spotify:track:manual1".into()];
        app.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ],
        );
        assert_eq!(app.queued_rows_len(), 1);

        // Turn on shuffle on the remote target.
        app.set_shuffle(true);
        assert!(app.queue_recheck_at.is_some());

        // Spotify confirms the remote shuffle action.
        app.handle_api(ApiResponse::Remote {
            action: RemoteAction::Shuffle,
            result: Ok(()),
        });
        assert!(
            app.queue_recheck_at.is_some(),
            "confirmed remote shuffle rechecks the queue"
        );

        // Fetch the queue with a new sequence number.
        app.refresh_queue(true);
        let seq = app.queue_seq;

        // An older response is ignored.
        let stale = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: seq - 1,
            result: Ok(stale),
        });
        assert_eq!(
            queue_uris(&app).1,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ]
        );

        // The confirming response shows the new shuffle order.
        let shuffled = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx2"),
                queued_song("spotify:track:ctx1"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(shuffled),
        });

        let (current, next) = queue_uris(&app);
        assert_eq!(current.as_deref(), Some("spotify:track:playing"));
        assert_eq!(
            next,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx2",
                "spotify:track:ctx1",
            ],
            "remote shuffle preserves hand-queued songs and updates context rows"
        );
        assert_eq!(app.queued_rows_len(), 1);
    }

    /// A queue fetch already in flight before a local reorder must not be
    /// allowed to land afterwards and undo it. Once resynced with the local
    /// engine, a fresh fetch still reporting the pre-drag order is likewise
    /// rejected as stale until it catches up.
    #[test]
    fn stale_queue_response_after_local_reorder_does_not_undo_it() {
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.local_ready = true;
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.manual_queue = vec![
            "spotify:track:manual1".into(),
            "spotify:track:manual2".into(),
        ];
        app.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:manual1",
                "spotify:track:manual2",
                "spotify:track:ctx1",
            ],
        );

        // A periodic refresh is already in flight before the drag lands.
        app.refresh_queue(true);
        let outstanding_seq = app.queue_seq;

        let ctx = egui::Context::default();
        app.apply(Action::MoveInQueue { from: 0, to: 2 }, &ctx);

        let reordered = vec![
            "spotify:track:manual2".to_string(),
            "spotify:track:manual1".to_string(),
            "spotify:track:ctx1".to_string(),
        ];
        assert_eq!(
            queue_uris(&app).1,
            reordered,
            "the reorder is applied optimistically"
        );

        // The request that was already outstanding lands after the move,
        // still reporting the pre-drag order. It must be superseded.
        let pre_drag_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:manual2"),
                queued_song("spotify:track:ctx1"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq: outstanding_seq,
            result: Ok(pre_drag_response.clone()),
        });
        assert_eq!(
            queue_uris(&app).1,
            reordered,
            "a late pre-drag response must not undo the move"
        );

        // A fresh fetch, issued after the move, still reports the pre-drag
        // order because the local engine has not caught up to the resync
        // yet. It is rejected as stale rather than accepted.
        app.queue_recheck_at = None;
        app.refresh_queue(true);
        let seq = app.queue_seq;
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(pre_drag_response),
        });
        assert_eq!(
            queue_uris(&app).1,
            reordered,
            "a lagging fresh response must not undo the move either"
        );
        assert_eq!(app.queue_stale_retries, 1);
        assert!(app.queue_recheck_at.is_some());

        // Once the resync has landed, the confirmed order is accepted.
        let resynced_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual2"),
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx1"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(resynced_response),
        });
        assert_eq!(app.queue_stale_retries, 0);
        assert!(app.queue_reorder_pending.is_none());
        assert_eq!(queue_uris(&app).1, reordered);
    }

    /// Moving a queued row must keep every pending addition pointing at its
    /// own song, not whichever row now sits in its old `manual_queue` slot.
    #[test]
    fn moving_a_queued_row_keeps_pending_additions_on_their_own_song() {
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.local_ready = true;
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.manual_queue = vec![
            "spotify:track:m0".into(),
            "spotify:track:m1".into(),
            "spotify:track:m2".into(),
        ];
        app.queue = loaded_queue(
            "spotify:track:playing",
            &["spotify:track:m0", "spotify:track:m1", "spotify:track:m2"],
        );
        app.pending_queue_adds = vec![
            PendingQueueAdd {
                item: queued_song("spotify:track:m0"),
                at: Instant::now(),
                manual_index: 0,
                write: None,
            },
            PendingQueueAdd {
                item: queued_song("spotify:track:m1"),
                at: Instant::now(),
                manual_index: 1,
                write: None,
            },
            PendingQueueAdd {
                item: queued_song("spotify:track:m2"),
                at: Instant::now(),
                manual_index: 2,
                write: None,
            },
        ];

        let ctx = egui::Context::default();
        // Drag "m0" past the end: it lands last, "m1" and "m2" each shift up one.
        app.apply(Action::MoveInQueue { from: 0, to: 3 }, &ctx);

        assert_eq!(
            app.manual_queue,
            ["spotify:track:m1", "spotify:track:m2", "spotify:track:m0"]
        );
        assert_eq!(app.pending_queue_adds.len(), 3);
        for addition in &app.pending_queue_adds {
            assert_eq!(
                app.manual_queue[addition.manual_index],
                addition.item.uri(),
                "pending addition must still name the song at its own index"
            );
        }
    }

    /// Inserting a dropped song into "Playing next" must keep every earlier
    /// pending addition pointing at its own song, not the newly inserted row.
    #[test]
    fn inserting_in_queue_keeps_pending_additions_on_their_own_song() {
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.local_ready = true;
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.manual_queue = vec!["spotify:track:m0".into(), "spotify:track:m1".into()];
        app.queue = loaded_queue(
            "spotify:track:playing",
            &["spotify:track:m0", "spotify:track:m1"],
        );
        app.pending_queue_adds = vec![
            PendingQueueAdd {
                item: queued_song("spotify:track:m0"),
                at: Instant::now(),
                manual_index: 0,
                write: None,
            },
            PendingQueueAdd {
                item: queued_song("spotify:track:m1"),
                at: Instant::now(),
                manual_index: 1,
                write: None,
            },
        ];

        let ctx = egui::Context::default();
        // Drop "new" between "m0" and "m1": "m1"'s pending entry must shift.
        app.apply(
            Action::InsertInQueue {
                items: vec![queued_song("spotify:track:new")],
                position: 1,
            },
            &ctx,
        );

        assert_eq!(
            app.manual_queue,
            ["spotify:track:m0", "spotify:track:new", "spotify:track:m1"]
        );
        assert_eq!(app.pending_queue_adds.len(), 3);
        for addition in &app.pending_queue_adds {
            assert_eq!(
                app.manual_queue[addition.manual_index],
                addition.item.uri(),
                "pending addition must still name the song at its own index"
            );
        }
    }

    /// A stale queue answer whose current track does not match the active
    /// local player is rejected rather than accepted after a shuffle toggle.
    #[test]
    fn stale_queue_after_shuffle_is_retried_and_not_accepted() {
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue(
            "spotify:track:playing",
            &["spotify:track:ctx1", "spotify:track:ctx2"],
        );

        app.set_shuffle(true);
        app.refresh_queue(true);
        let seq = app.queue_seq;

        // A stale response that reports an old playing track is rejected.
        let stale_track_response = Queue {
            currently_playing: Some(queued_song("spotify:track:old_song")),
            queue: vec![
                queued_song("spotify:track:ctx2"),
                queued_song("spotify:track:ctx1"),
            ],
        };
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(stale_track_response),
        });

        assert_eq!(
            queue_uris(&app).1,
            vec!["spotify:track:ctx1", "spotify:track:ctx2"],
            "the queue remains uncorrupted by the stale response"
        );
        assert_eq!(app.queue_stale_retries, 1);
        assert!(app.queue_recheck_at.is_some());
    }

    /// A lagging queue answer after a shuffle toggle with the same current song
    /// and sequence is rejected and retried until the new order arrives.
    #[test]
    fn lagging_queue_after_shuffle_is_retried_until_new_order_arrives() {
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.manual_queue = vec!["spotify:track:manual1".into()];
        app.queue = loaded_queue(
            "spotify:track:playing",
            &[
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
                "spotify:track:ctx3",
            ],
        );

        app.set_shuffle(true);
        app.refresh_queue(true);
        let seq = app.queue_seq;

        // A lagging response arrives with the same currently playing track and
        // the current sequence, but the context rows are still in the old order.
        let lagging_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
                queued_song("spotify:track:ctx3"),
            ],
        };

        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(lagging_response.clone()),
        });

        // The stale pre-shuffle order is rejected, preserving the existing queue,
        // incrementing the retry counter, and scheduling a prompt recheck.
        assert_eq!(app.queue_stale_retries, 1);
        assert!(app.queue_recheck_at.is_some());
        assert_eq!(
            queue_uris(&app).1,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
                "spotify:track:ctx3",
            ]
        );

        // A second lagging response also retries.
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(lagging_response),
        });
        assert_eq!(app.queue_stale_retries, 2);
        assert!(app.queue_recheck_at.is_some());

        // Once the reordered response arrives, it is accepted and pending state clears.
        let new_order_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:manual1"),
                queued_song("spotify:track:ctx3"),
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
            ],
        };

        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(new_order_response),
        });

        assert_eq!(app.queue_stale_retries, 0);
        assert!(app.queue_shuffle_pending.is_none());
        assert_eq!(
            queue_uris(&app).1,
            vec![
                "spotify:track:manual1",
                "spotify:track:ctx3",
                "spotify:track:ctx1",
                "spotify:track:ctx2",
            ]
        );
    }

    /// If Spotify returns an unchanged queue order after shuffle, Spotifast
    /// retries up to the limit and then accepts the result as a bounded fallback.
    #[test]
    fn unchanged_shuffle_result_has_bounded_fallback() {
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:playing".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue(
            "spotify:track:playing",
            &["spotify:track:ctx1", "spotify:track:ctx2"],
        );

        app.set_shuffle(true);
        app.refresh_queue(true);
        let seq = app.queue_seq;

        let unchanged_response = Queue {
            currently_playing: Some(queued_song("spotify:track:playing")),
            queue: vec![
                queued_song("spotify:track:ctx1"),
                queued_song("spotify:track:ctx2"),
            ],
        };

        // Responses with the pre-shuffle order are rejected as stale and scheduled for retry.
        for expected_retries in 1..=QUEUE_STALE_RETRIES {
            app.handle_api(ApiResponse::Queue {
                seq,
                result: Ok(unchanged_response.clone()),
            });
            assert_eq!(app.queue_stale_retries, expected_retries);
            assert!(app.queue_recheck_at.is_some());
        }

        // The next response exceeds the retry limit, so Spotifast accepts it.
        app.handle_api(ApiResponse::Queue {
            seq,
            result: Ok(unchanged_response),
        });
        assert_eq!(app.queue_stale_retries, 0);
        assert!(app.queue_shuffle_pending.is_none());
        assert_eq!(
            queue_uris(&app).1,
            vec!["spotify:track:ctx1", "spotify:track:ctx2"]
        );
    }

    fn picked(app: &App, page: &Page) -> Vec<usize> {
        app.picked_rows(page)
            .map(|rows| rows.iter().copied().collect())
            .unwrap_or_default()
    }

    /// A plain click selects one row; clicking it again clears selection.
    #[test]
    fn a_plain_click_picks_one_row_and_a_second_lets_it_go() {
        let mut app = test_app("pick-one");
        let page = Page::LikedSongs;
        app.pick_row(&page, "v", 3, RowPick::Only, 10);
        assert_eq!(picked(&app, &page), vec![3]);
        app.pick_row(&page, "v", 5, RowPick::Only, 10);
        assert_eq!(picked(&app, &page), vec![5], "the first one is dropped");
        app.pick_row(&page, "v", 5, RowPick::Only, 10);
        assert!(picked(&app, &page).is_empty(), "clicking it again lets go");
    }

    /// Ctrl-click toggles one row.
    #[test]
    fn ctrl_click_adds_and_removes_one_row() {
        let mut app = test_app("pick-toggle");
        let page = Page::LikedSongs;
        app.pick_row(&page, "v", 1, RowPick::Only, 10);
        app.pick_row(&page, "v", 4, RowPick::Toggle, 10);
        app.pick_row(&page, "v", 7, RowPick::Toggle, 10);
        assert_eq!(picked(&app, &page), vec![1, 4, 7]);
        app.pick_row(&page, "v", 4, RowPick::Toggle, 10);
        assert_eq!(
            picked(&app, &page),
            vec![1, 7],
            "the same click takes it out"
        );
    }

    /// Shift-click selects from the anchor without exceeding the list.
    #[test]
    fn shift_click_takes_the_run_back_to_the_anchor() {
        let mut app = test_app("pick-range");
        let page = Page::LikedSongs;
        app.pick_row(&page, "v", 2, RowPick::Only, 10);
        app.pick_row(&page, "v", 5, RowPick::Range, 10);
        assert_eq!(picked(&app, &page), vec![2, 3, 4, 5]);
        // Back the other way, from the same anchor.
        app.pick_row(&page, "v", 0, RowPick::Range, 10);
        assert_eq!(picked(&app, &page), vec![0, 1, 2]);
        // A list that has since shrunk cannot be reached past its end.
        app.pick_row(&page, "v", 9, RowPick::Range, 4);
        assert_eq!(picked(&app, &page), vec![2, 3]);
    }

    /// Shift-click without an anchor selects one row.
    #[test]
    fn shift_click_with_no_anchor_picks_one_row() {
        let mut app = test_app("pick-no-anchor");
        let page = Page::LikedSongs;
        app.pick_row(&page, "v", 6, RowPick::Range, 10);
        assert_eq!(picked(&app, &page), vec![6]);
    }

    /// Selection clears when sorting, filtering, or paging changes the rows.
    #[test]
    fn the_rows_let_go_when_the_list_moves_underneath() {
        let mut app = test_app("pick-stale");
        let page = Page::LikedSongs;
        app.pick_row(&page, "by-name|", 3, RowPick::Only, 10);
        app.keep_picked_rows_for(&page, "by-name|");
        assert_eq!(picked(&app, &page), vec![3], "the same list keeps them");
        app.keep_picked_rows_for(&page, "by-date|");
        assert!(picked(&app, &page).is_empty(), "a re-sort lets them go");
    }

    /// Selecting rows in another table replaces the current selection.
    #[test]
    fn picking_rows_on_another_page_replaces_the_first() {
        let mut app = test_app("pick-other-page");
        let liked = Page::LikedSongs;
        let album = Page::Album("a".to_string());
        app.pick_row(&liked, "v", 1, RowPick::Only, 10);
        app.pick_row(&album, "v", 2, RowPick::Only, 10);
        assert_eq!(picked(&app, &album), vec![2]);
        assert!(picked(&app, &liked).is_empty());
    }

    fn search_page(id: &str) -> crate::api::models::Page<Playlist> {
        crate::api::models::Page {
            items: vec![Playlist {
                id: id.into(),
                ..Playlist::default()
            }],
            ..Default::default()
        }
    }

    fn catalogue_answer() -> crate::api::models::SearchResults {
        crate::api::models::SearchResults {
            tracks: Some(crate::api::models::Page {
                items: vec![Track::default()],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn searching(app: &mut App, query: &str) -> u64 {
        app.search.query = query.into();
        app.run_search(query.into());
        let serial = app.search.serial;
        app.handle_api(ApiResponse::SearchStarted {
            query: query.into(),
            serial,
            split: true,
        });
        serial
    }

    #[test]
    fn each_half_of_a_search_shows_as_it_arrives() {
        for playlists_first in [false, true] {
            let mut app = test_app("search-halves");
            let serial = searching(&mut app, "radiohead");
            let catalogue = ApiResponse::Search {
                query: "radiohead".into(),
                serial,
                result: Ok(catalogue_answer()),
            };
            let playlists = ApiResponse::SearchPlaylists {
                query: "radiohead".into(),
                serial,
                result: Ok(search_page("p")),
            };
            if playlists_first {
                app.handle_api(playlists);
                let shown = app
                    .search
                    .results
                    .get()
                    .expect("playlists appear before catalogue");
                assert_eq!(shown.playlists.as_ref().unwrap().items[0].id, "p");
                assert!(shown.tracks.is_none());
                assert!(app.search.catalogue_pending);
                app.handle_api(catalogue);
            } else {
                app.handle_api(catalogue);
                let shown = app.search.results.get().expect("results");
                assert_eq!(shown.tracks.as_ref().unwrap().items.len(), 1);
                assert!(shown.playlists.is_none());
                app.handle_api(playlists);
            }
            let shown = app.search.results.get().expect("results");
            assert_eq!(shown.tracks.as_ref().unwrap().items.len(), 1);
            assert_eq!(shown.playlists.as_ref().unwrap().items[0].id, "p");
        }
    }

    #[test]
    fn playlists_from_an_older_search_never_join_a_newer_one() {
        let mut app = test_app("search-stale-playlists");
        let stale = searching(&mut app, "radiohead");
        app.handle_api(ApiResponse::SearchPlaylists {
            query: "radiohead".into(),
            serial: stale,
            result: Ok(search_page("stale")),
        });
        let serial = searching(&mut app, "portishead");
        app.handle_api(ApiResponse::Search {
            query: "portishead".into(),
            serial,
            result: Ok(catalogue_answer()),
        });
        let shown = app.search.results.get().expect("results");
        assert!(shown.playlists.is_none());
    }

    #[test]
    fn failed_search_halves_never_mix_queries_or_discard_the_successful_half() {
        for playlists_fail in [false, true] {
            for failure_first in [false, true] {
                let mut app = test_app("search-half-failure");
                let old = searching(&mut app, "old");
                app.handle_api(ApiResponse::Search {
                    query: "old".into(),
                    serial: old,
                    result: Ok(catalogue_answer()),
                });
                let serial = searching(&mut app, "new");
                assert!(
                    app.search.results.get().is_none(),
                    "old songs cannot label a new query"
                );
                let failure = if playlists_fail {
                    ApiResponse::SearchPlaylists {
                        query: "new".into(),
                        serial,
                        result: Err(crate::api::ApiError::RateLimited),
                    }
                } else {
                    ApiResponse::Search {
                        query: "new".into(),
                        serial,
                        result: Err(crate::api::ApiError::RateLimited),
                    }
                };
                let success = if playlists_fail {
                    ApiResponse::Search {
                        query: "new".into(),
                        serial,
                        result: Ok(catalogue_answer()),
                    }
                } else {
                    ApiResponse::SearchPlaylists {
                        query: "new".into(),
                        serial,
                        result: Ok(search_page("new")),
                    }
                };
                if failure_first {
                    app.handle_api(failure);
                    assert!(matches!(app.search.results, Loadable::Loading));
                    app.handle_api(success);
                } else {
                    app.handle_api(success);
                    assert!(app.search.results.get().is_some());
                    app.handle_api(failure);
                }
                let shown = app.search.results.get().expect("successful half survives");
                assert_eq!(shown.tracks.is_some(), playlists_fail);
                assert_eq!(shown.playlists.is_some(), !playlists_fail);
                assert_eq!(app.search.results_serial, serial);
                assert!(app.search.error.is_some());
                assert!(!app.search.catalogue_pending && !app.search.playlists_pending);
                app.backend.shutdown();
            }
        }
    }

    #[test]
    fn both_search_failures_finish_loading_and_the_same_query_can_retry() {
        let mut app = test_app("search-both-fail");
        let serial = searching(&mut app, "new");
        app.handle_api(ApiResponse::Search {
            query: "new".into(),
            serial,
            result: Err(crate::api::ApiError::RateLimited),
        });
        app.handle_api(ApiResponse::SearchPlaylists {
            query: "new".into(),
            serial,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert!(matches!(app.search.results, Loadable::Failed(_)));
        assert!(!app.search.catalogue_pending && !app.search.playlists_pending);
        app.run_search("new".into());
        assert!(app.search.serial > serial);
        assert!(app.search.error.is_none());
        assert!(matches!(app.search.results, Loadable::Loading));
        app.backend.shutdown();
    }

    #[test]
    fn clearing_or_signing_out_rejects_both_late_search_halves() {
        for sign_out in [false, true] {
            let mut app = test_app("search-cancel");
            let serial = searching(&mut app, "old");
            if sign_out {
                app.reset_data();
            } else {
                app.run_search(String::new());
            }
            app.handle_api(ApiResponse::Search {
                query: "old".into(),
                serial,
                result: Ok(catalogue_answer()),
            });
            app.handle_api(ApiResponse::SearchPlaylists {
                query: "old".into(),
                serial,
                result: Ok(search_page("old")),
            });
            assert!(matches!(app.search.results, Loadable::NotLoaded));
            assert!(app.search.playlists.is_none());
            assert!(!app.search.catalogue_pending && !app.search.playlists_pending);
            app.backend.shutdown();
        }
    }

    fn cover_dialog(request: Option<u64>) -> Dialog {
        Dialog::EditPlaylist {
            id: "pl1".into(),
            name: "Test".into(),
            description: String::new(),
            public: Some(false),
            cover: crate::playlist_cover::Draft {
                request,
                ..Default::default()
            },
        }
    }

    fn test_cover() -> crate::playlist_cover::Cover {
        test_cover_color([220, 20, 40])
    }

    fn test_cover_color(color: [u8; 3]) -> crate::playlist_cover::Cover {
        let pixels = image::RgbImage::from_pixel(16, 16, image::Rgb(color));
        let mut bytes = std::io::Cursor::new(Vec::new());
        pixels
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        crate::playlist_cover::prepare(bytes.get_ref()).unwrap()
    }

    #[test]
    fn reopening_cover_details_does_not_start_a_second_upload() {
        let mut app = test_app("cover-reopened-in-flight");
        app.backend.set_offline(true);
        let ctx = egui::Context::default();
        app.dialog = Some(cover_dialog(None));
        if let Some(Dialog::EditPlaylist { cover, .. }) = &mut app.dialog {
            cover.selection = Some(test_cover());
        }
        app.apply(Action::UploadPlaylistCover("pl1".into()), &ctx);
        let request = app.cover_request;
        app.apply(Action::CloseDialog, &ctx);
        app.apply(Action::ShowDialog(cover_dialog(None)), &ctx);
        // A stale action or another UI entry point cannot overlap that PUT.
        if let Some(Dialog::EditPlaylist { cover, .. }) = &mut app.dialog {
            cover.selection = Some(test_cover());
        }
        app.apply(Action::UploadPlaylistCover("pl1".into()), &ctx);
        assert_eq!(
            app.cover_request, request,
            "the first upload still owns the playlist"
        );
        let Some(Dialog::EditPlaylist { cover, .. }) = &app.dialog else {
            panic!()
        };
        assert_eq!(cover.uploading, Some(request));
        app.backend.shutdown();
    }

    #[test]
    fn cover_picker_cancellation_and_stale_completion_preserve_the_dialog() {
        let mut app = test_app("cover-picker");
        app.dialog = Some(cover_dialog(Some(2)));
        app.cover_chosen("pl1", 1, Ok(Some(test_cover())));
        let Some(Dialog::EditPlaylist { cover, .. }) = &app.dialog else {
            panic!()
        };
        assert!(cover.selection.is_none());
        assert_eq!(cover.request, Some(2));
        app.cover_chosen("pl1", 2, Ok(None));
        let Some(Dialog::EditPlaylist { cover, .. }) = &app.dialog else {
            panic!()
        };
        assert!(cover.request.is_none());
        assert!(cover.error.is_none());
        app.dialog = None;
        app.cover_chosen("pl1", 2, Ok(Some(test_cover())));
        assert!(app.dialog.is_none());
        app.backend.shutdown();
    }

    #[test]
    fn failed_cover_upload_preserves_preview_and_success_refreshes_library_art() {
        let mut app = test_app("cover-upload");
        app.dialog = Some(cover_dialog(None));
        let cover = test_cover();
        if let Some(Dialog::EditPlaylist { cover: draft, .. }) = &mut app.dialog {
            draft.selection = Some(cover.clone());
            draft.uploading = Some(1);
        }
        app.cover_uploads.insert("pl1".into(), 1);
        app.handle_api(ApiResponse::PlaylistCoverUploaded {
            id: "pl1".into(),
            request: 1,
            previous_urls: vec![],
            cover: cover.clone(),
            result: Err(crate::api::ApiError::Status {
                status: 403,
                message: "Forbidden".into(),
            }),
        });
        let Some(Dialog::EditPlaylist { cover: draft, .. }) = &app.dialog else {
            panic!()
        };
        assert!(draft.uploading.is_none());
        assert!(draft.selection.is_some());
        assert!(draft.error.as_ref().unwrap().contains("sign in again"));
        app.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            ..Default::default()
        }]);
        if let Some(Dialog::EditPlaylist { cover: draft, .. }) = &mut app.dialog {
            draft.uploading = Some(2);
        }
        app.cover_uploads.insert("pl1".into(), 2);
        app.handle_api(ApiResponse::PlaylistCoverUploaded {
            id: "pl1".into(),
            request: 2,
            previous_urls: vec![],
            cover: cover.clone(),
            result: Ok(()),
        });
        assert_eq!(
            app.library.playlists.get().unwrap()[0].images[0].url,
            cover.uri
        );
        assert!(app.uploaded_covers.contains_key("pl1"));
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![Playlist {
                    id: "pl1".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        });
        assert_eq!(
            app.library.playlists.get().unwrap()[0].images[0].url,
            cover.uri,
            "lagging metadata must not undo the uploaded artwork"
        );
        let Some(Dialog::EditPlaylist { cover: draft, .. }) = &app.dialog else {
            panic!()
        };
        assert!(draft.selection.is_none());
        assert!(draft.error.is_none());
        app.backend.shutdown();
    }

    #[test]
    fn old_upload_completion_does_not_change_a_reopened_dialog() {
        for succeeds in [false, true] {
            for newer_upload in [None, Some(2)] {
                let mut app = test_app("reopened-cover-upload");
                app.dialog = Some(cover_dialog(None));
                let selected = test_cover();
                if let Some(Dialog::EditPlaylist { cover, .. }) = &mut app.dialog {
                    cover.selection = Some(selected.clone());
                    cover.uploading = Some(1);
                }
                app.dialog = None;
                app.dialog = Some(cover_dialog(Some(3)));
                if let Some(Dialog::EditPlaylist { cover, .. }) = &mut app.dialog {
                    cover.selection = Some(selected.clone());
                    cover.uploading = newer_upload;
                    cover.error = Some("Newer selection error".into());
                }
                app.cover_uploads
                    .insert("pl1".into(), newer_upload.unwrap_or(1));
                app.handle_api(ApiResponse::PlaylistCoverUploaded {
                    id: "pl1".into(),
                    request: 1,
                    previous_urls: vec![],
                    cover: selected.clone(),
                    result: if succeeds {
                        Ok(())
                    } else {
                        Err(crate::api::ApiError::Status {
                            status: 403,
                            message: "Forbidden".into(),
                        })
                    },
                });
                let Some(Dialog::EditPlaylist { cover, .. }) = &app.dialog else {
                    panic!()
                };
                assert_eq!(cover.selection.as_ref().unwrap().uri, selected.uri);
                assert_eq!(cover.uploading, newer_upload);
                assert_eq!(cover.request, Some(3));
                assert_eq!(cover.error.as_deref(), Some("Newer selection error"));
                app.backend.shutdown();
            }
        }
    }

    #[test]
    fn a_late_url_from_an_earlier_upload_cannot_replace_the_new_cover() {
        let mut app = test_app("cover-lagging-earlier-upload");
        app.backend.set_offline(true);
        app.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            images: vec![Image {
                url: "original".into(),
                ..Default::default()
            }],
            ..Default::default()
        }]);
        let current = test_cover_color([20, 40, 220]);
        for request in [1, 2] {
            app.cover_uploads.insert("pl1".into(), request);
            app.handle_api(ApiResponse::PlaylistCoverUploaded {
                id: "pl1".into(),
                request,
                previous_urls: vec!["original".into()],
                cover: if request == 1 {
                    test_cover()
                } else {
                    current.clone()
                },
                result: Ok(()),
            });
        }
        app.handle_api(ApiResponse::MyPlaylists {
            offset: 0,
            generation: app.library.playlists_generation,
            result: Ok(crate::api::models::Page {
                items: vec![Playlist {
                    id: "pl1".into(),
                    images: vec![Image {
                        url: "https://art.example/earlier-upload.jpg".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }),
        });
        assert_eq!(
            app.library.playlists.get().unwrap()[0].images[0].url,
            current.uri,
            "a new URL does not establish which upload it contains"
        );
        let earlier = vec![Image {
            url: "https://art.example/earlier-upload.jpg".into(),
            ..Default::default()
        }];
        // A check started for the superseded upload cannot confirm request 2.
        app.cover_checked("pl1", 1, earlier.clone(), Ok(true));
        assert_eq!(app.uploaded_covers["pl1"].request, 2);
        let latest = vec![Image {
            url: "https://art.example/current-upload.jpg".into(),
            ..Default::default()
        }];
        let mut latest_reply = latest.clone();
        app.reconcile_playlist_cover("pl1", &mut latest_reply);
        assert_eq!(latest_reply[0].url, current.uri);
        assert_eq!(app.uploaded_covers["pl1"].checking, Some(earlier.clone()));
        assert_eq!(app.uploaded_covers["pl1"].next_images, Some(latest.clone()));
        app.cover_checked("pl1", 2, earlier, Ok(false));
        assert_eq!(app.uploaded_covers["pl1"].checking, Some(latest.clone()));
        assert_eq!(
            app.library.playlists.get().unwrap()[0].images[0].url,
            current.uri
        );
        // A failed download also preserves the preview and remains retryable.
        app.cover_checked("pl1", 2, latest.clone(), Err("offline".into()));
        assert!(app.uploaded_covers["pl1"].checking.is_none());
        app.reconcile_playlist_cover("pl1", &mut latest.clone());
        app.cover_checked("pl1", 2, latest.clone(), Ok(true));
        assert!(!app.uploaded_covers.contains_key("pl1"));
        assert_eq!(app.library.playlists.get().unwrap()[0].images, latest);
        app.backend.shutdown();
    }

    #[test]
    fn confirmed_spotify_art_retires_the_override_and_accepts_later_changes() {
        for confirm_from_library in [false, true] {
            let mut app = test_app("confirmed-cover");
            let remote = |url: &str| {
                vec![crate::api::models::Image {
                    url: url.into(),
                    width: None,
                    height: None,
                }]
            };
            let playlist = |url: &str| Playlist {
                id: "pl1".into(),
                images: remote(url),
                ..Default::default()
            };
            app.library.playlists = Loadable::Loaded(vec![playlist("old")]);
            let page = app.playlist_pages.entry("pl1".into()).or_default();
            page.playlist = Loadable::Loaded(playlist("old"));
            let generation = page.generation;
            let cover = test_cover();
            app.cover_uploads.insert("pl1".into(), 1);
            app.handle_api(ApiResponse::PlaylistCoverUploaded {
                id: "pl1".into(),
                request: 1,
                previous_urls: vec!["old".into()],
                cover: cover.clone(),
                result: Ok(()),
            });
            app.handle_api(ApiResponse::Playlist {
                id: "pl1".into(),
                generation: generation + 1,
                result: Ok(playlist("stale-generation")),
            });
            assert!(app.uploaded_covers.contains_key("pl1"));
            for url in ["old", "confirmed", "changed-elsewhere"] {
                if confirm_from_library {
                    app.handle_api(ApiResponse::MyPlaylists {
                        offset: 0,
                        generation: app.library.playlists_generation,
                        result: Ok(crate::api::models::Page {
                            items: vec![playlist(url)],
                            ..Default::default()
                        }),
                    });
                } else {
                    app.handle_api(ApiResponse::Playlist {
                        id: "pl1".into(),
                        generation,
                        result: Ok(playlist(url)),
                    });
                }
                if url == "confirmed" {
                    assert_eq!(
                        app.library.playlists.get().unwrap()[0].images[0].url,
                        cover.uri
                    );
                    app.cover_checked("pl1", 1, remote(url), Ok(true));
                }
                let expected = if url == "old" { &cover.uri } else { url };
                if confirm_from_library {
                    assert_eq!(
                        app.library.playlists.get().unwrap()[0].images[0].url,
                        expected
                    );
                } else {
                    assert_eq!(
                        app.playlist_pages["pl1"].playlist.get().unwrap().images[0].url,
                        expected
                    );
                }
                assert_eq!(app.uploaded_covers.contains_key("pl1"), url == "old");
                if url == "confirmed" {
                    assert_eq!(app.library.playlists.get().unwrap()[0].images[0].url, url);
                    assert_eq!(
                        app.playlist_pages["pl1"].playlist.get().unwrap().images[0].url,
                        url
                    );
                }
            }
            app.backend.shutdown();
        }
    }

    fn test_app(name: &str) -> App {
        let root =
            std::env::temp_dir().join(format!("spotifast-{name}-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        App::new(
            &Waker::default(),
            AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        )
    }

    #[test]
    fn known_albums_include_liked_and_playlist_tracks() {
        let mut app = test_app("known-track-albums");
        let album = |id: &str| Album {
            id: id.to_string(),
            ..Default::default()
        };
        app.library.liked.items.push(SavedTrack {
            track: Track {
                album: Some(album("liked-album")),
                ..Default::default()
            },
            ..Default::default()
        });
        app.playlist_pages.insert(
            "playlist".to_string(),
            PlaylistPage {
                items: PagedList {
                    items: vec![PlaylistItem {
                        item: Some(PlayableItem::Track(Track {
                            album: Some(album("playlist-album")),
                            ..Default::default()
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        assert_eq!(app.known_album("liked-album").unwrap().id, "liked-album");
        assert_eq!(
            app.known_album("playlist-album").unwrap().id,
            "playlist-album"
        );
        app.backend.shutdown();
    }

    #[test]
    fn known_shows_include_saved_and_search_episodes() {
        let mut app = test_app("known-episode-shows");
        let show = |id: &str| Show {
            id: id.to_string(),
            ..Default::default()
        };
        app.library.episodes.items.push(SavedEpisode {
            episode: Episode {
                show: Some(show("saved-show")),
                ..Default::default()
            },
            ..Default::default()
        });
        app.search.results = Loadable::Loaded(SearchResults {
            episodes: Some(ApiPage {
                items: vec![Episode {
                    show: Some(show("search-show")),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(app.known_show("saved-show").unwrap().id, "saved-show");
        assert_eq!(app.known_show("search-show").unwrap().id, "search-show");
        app.backend.shutdown();
    }

    #[test]
    fn custom_theme_cache_preserves_first_frame_and_settings_after_file_removal_or_corruption() {
        let mut app = test_app("custom-theme-restart");
        app.backend.shutdown();
        let ctx = egui::Context::default();
        let directory = app.dirs.config.join("themes");
        let file = directory.join("local.json");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            &file,
            br##"{"base":"light","colors":{"accent":"#8c3fa5"}}"##,
        )
        .unwrap();
        app.settings.theme = ThemeChoice::Dark;
        app.settings.audio_cache_mb = 777;
        app.settings.custom_theme = Some("local.json".into());
        assert!(
            !app.custom_themes.loading(),
            "construction never starts theme discovery"
        );
        app.load_custom_themes(&Waker::default());
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        let mut expected = Palette::light();
        expected.accent = egui::Color32::from_rgb(140, 63, 165);
        assert_eq!(app.palette, expected);
        app.save_settings();
        let accepted = Settings::load(&app.dirs.settings_file());
        assert!(accepted.custom_theme_cache.is_some());

        for contents in [
            None,
            Some("{broken"),
            Some(r##"{"colors":{"accent":"#bad"}}"##),
        ] {
            if let Some(contents) = contents {
                std::fs::write(&file, contents).unwrap();
            } else {
                std::fs::remove_file(&file).unwrap();
            }
            let mut restored = App::new(
                &Waker::default(),
                app.dirs.clone(),
                Settings::load(&app.dirs.settings_file()),
                AppOptions {
                    media_controls: false,
                    restore_sign_in: false,
                    tray: false,
                },
            );
            restored.backend.shutdown();
            assert_eq!(
                restored.palette, expected,
                "even the first window clear uses the accepted palette"
            );
            let ctx = egui::Context::default();
            restored.attach(&ctx);
            restored.apply_theme(&ctx);
            assert_eq!(ctx.theme(), egui::Theme::Light);
            assert_eq!(restored.palette, expected);
            restored.load_custom_themes(&Waker::default());
            wait_for_custom_themes(&mut restored, &ctx);
            restored.apply_theme(&ctx);
            assert_eq!(restored.palette, expected);
            assert!(
                restored
                    .custom_themes
                    .detail_in(crate::i18n::Locale::English, Some("local.json"))
                    .contains("last usable")
            );
            restored.save_settings();
            assert_eq!(Settings::load(&restored.dirs.settings_file()), accepted);
        }
        std::fs::remove_dir_all(app.dirs.config.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_chosen_language_applies_at_once_and_survives_a_restart() {
        use crate::i18n::Locale;
        use crate::settings::LanguageChoice;
        let mut app = test_app("language-choice");
        app.backend.shutdown();
        let ctx = egui::Context::default();
        assert_eq!(app.locale, Locale::English);
        let choice = LanguageChoice::Locale(Locale::PortugueseBrazil);
        app.apply(Action::SetLanguage(choice), &ctx);
        assert_eq!(app.locale, Locale::PortugueseBrazil);
        assert!(app.settings_dirty);
        app.save_settings();
        let saved = Settings::load(&app.dirs.settings_file());
        assert_eq!(saved.language, choice);
        let mut restarted = App::new(
            &Waker::default(),
            app.dirs.clone(),
            saved,
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        restarted.backend.shutdown();
        assert_eq!(restarted.locale, Locale::PortugueseBrazil);
        std::fs::remove_dir_all(app.dirs.config.parent().unwrap()).unwrap();
    }

    fn wait_for_custom_themes(app: &mut App, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.custom_themes.loading() {
            app.poll_custom_themes(ctx);
            assert!(Instant::now() < deadline, "theme worker did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn following_omarchy_is_automatic_live_and_never_overrides_an_explicit_choice() {
        let mut app = test_app("system-omarchy");
        app.backend.shutdown();
        let ctx = egui::Context::default();
        app.settings = Settings::default();
        app.window_hidden = true;
        app.resume_track = Some("spotify:track:playing".into());
        app.resume_position_ms = 123_000;
        let theme = theme::custom::CustomTheme {
            filename: "omarchy.json".into(),
            palette: Palette::light(),
        };
        app.custom_themes
            .load_system_test(Some(theme.clone()), true);
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        assert_eq!(app.palette, theme.palette);
        assert_eq!(app.settings.theme, ThemeChoice::System);
        assert!(app.settings.custom_theme.is_none());
        app.save_settings();
        let saved = Settings::load(&app.dirs.settings_file());
        assert_eq!(saved.cached_palette(), Some(theme.palette));

        app.custom_themes.load_system_test(None, true);
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        assert_eq!(
            app.palette, theme.palette,
            "missing colours retain the last palette"
        );
        for (choice, expected) in [
            (ThemeChoice::Dark, Palette::dark()),
            (ThemeChoice::Light, Palette::light()),
        ] {
            app.apply(Action::SetTheme(choice), &ctx);
            let mut updated = theme.clone();
            updated.palette.accent = egui::Color32::RED;
            app.custom_themes.load_system_test(Some(updated), true);
            wait_for_custom_themes(&mut app, &ctx);
            app.apply_theme(&ctx);
            assert_eq!(app.palette, expected);
        }
        app.apply(Action::SetTheme(ThemeChoice::System), &ctx);
        assert_eq!(app.palette.accent, egui::Color32::RED);
        let custom = theme::custom::CustomTheme {
            filename: "mine.json".into(),
            palette: Palette::dark(),
        };
        app.custom_themes = theme::custom::Catalog::from_themes(vec![custom.clone()]);
        app.apply(Action::SetCustomTheme(custom.filename.clone()), &ctx);
        app.custom_themes.load_system_test(Some(theme), true);
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        assert_eq!(app.palette, custom.palette);
        app.apply(Action::SetTheme(ThemeChoice::System), &ctx);
        app.custom_themes.load_system_test(None, false);
        wait_for_custom_themes(&mut app, &ctx);
        assert!(app.settings.system_theme_cache.is_none());
        assert_eq!(app.theme_preference(), egui::ThemePreference::System);
        assert!(app.window_hidden);
        assert_eq!(app.resume_track.as_deref(), Some("spotify:track:playing"));
        assert_eq!(app.resume_position_ms, 123_000);
        std::fs::remove_dir_all(app.dirs.config.parent().unwrap()).unwrap();
    }

    /// winit reports no system theme on Linux, so "Follow system" takes
    /// the desktop portal's preference, and a fixed choice ignores it.
    #[cfg(target_os = "linux")]
    #[test]
    fn follow_system_uses_the_desktops_preference_on_linux() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.settings.theme = ThemeChoice::System;
        ctx.set_theme(app.theme_preference());
        app.system_appearance = Some(crate::appearance::SystemAppearance::fixed(false));
        app.apply_theme(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Light);
        assert_eq!(app.palette, Palette::light());

        app.system_appearance = Some(crate::appearance::SystemAppearance::fixed(true));
        app.apply_theme(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Dark);

        app.settings.theme = ThemeChoice::Dark;
        ctx.set_theme(app.theme_preference());
        app.system_appearance = Some(crate::appearance::SystemAppearance::fixed(false));
        app.apply_theme(&ctx);
        assert_eq!(ctx.theme(), egui::Theme::Dark, "a chosen theme wins");
    }

    #[test]
    fn custom_theme_reload_updates_the_selected_palette_without_showing_the_window() {
        let mut app = test_app("custom-theme-reload");
        app.backend.shutdown();
        let ctx = egui::Context::default();
        let directory = app.dirs.config.join("themes");
        let file = directory.join("omarchy.json");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&file, br#"{"base":"dark"}"#).unwrap();
        app.settings.custom_theme = Some("omarchy.json".into());
        app.load_custom_themes(&Waker::default());
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        assert_eq!(app.palette, Palette::dark());

        app.window_hidden = true;
        app.settings.volume = 37;
        app.resume_track = Some("spotify:track:playing".into());
        app.resume_position_ms = 123_000;
        let queue: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> = Default::default();
        app.control_commands = Some(queue.clone());
        for (contents, expected) in [
            (r#"{"base":"light"}"#, Palette::light()),
            ("broken", Palette::light()),
            (r#"{"base":"dark"}"#, Palette::dark()),
        ] {
            std::fs::write(&file, contents).unwrap();
            queue.lock().unwrap().push(ControlCommand::ReloadThemes);
            app.handle_control_commands();
            assert!(matches!(app.actions.as_slice(), [Action::ReloadThemes]));
            let action = app.actions.pop().unwrap();
            app.apply(action, &ctx);
            wait_for_custom_themes(&mut app, &ctx);
            app.apply_theme(&ctx);
            assert_eq!(app.palette, expected);
            assert!(app.window_hidden);
            assert_eq!(app.settings.volume, 37);
            assert_eq!(app.resume_track.as_deref(), Some("spotify:track:playing"));
            assert_eq!(app.resume_position_ms, 123_000);
            assert_eq!(app.settings.custom_theme.as_deref(), Some("omarchy.json"));
        }

        app.apply(Action::SetTheme(ThemeChoice::Light), &ctx);
        app.apply(Action::ReloadThemes, &ctx);
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        assert_eq!(
            app.palette,
            Palette::light(),
            "a hook never selects a custom theme"
        );
        assert!(app.settings.custom_theme.is_none());
        std::fs::remove_dir_all(app.dirs.config.parent().unwrap()).unwrap();
    }

    #[test]
    fn custom_theme_worker_does_not_block_selection_or_restore_a_stale_choice() {
        let mut app = test_app("custom-theme-worker");
        app.backend.shutdown();
        let ctx = egui::Context::default();
        app.settings.custom_theme = Some("old.json".into());
        let (started, worker) = std::sync::mpsc::channel();
        let (finish, continue_load) = std::sync::mpsc::channel();
        app.custom_themes.load_test(move || {
            started.send(std::thread::current().id()).unwrap();
            continue_load.recv().unwrap();
            vec![theme::custom::CustomTheme {
                filename: "old.json".into(),
                palette: Palette::dark(),
            }]
        });
        assert_ne!(
            worker.recv_timeout(Duration::from_secs(3)).unwrap(),
            std::thread::current().id()
        );
        app.apply(Action::SetTheme(ThemeChoice::Light), &ctx);
        assert_eq!(app.palette, Palette::light());
        assert!(app.custom_themes.loading());
        finish.send(()).unwrap();
        wait_for_custom_themes(&mut app, &ctx);
        app.apply_theme(&ctx);
        assert_eq!(app.palette, Palette::light());
        assert!(app.custom_themes.find("old.json").is_some());
        assert!(app.settings.custom_theme.is_none());
        assert!(app.settings.custom_theme_cache.is_none());
    }

    #[test]
    fn custom_theme_changes_egui_base_and_colors_together_and_builtin_choices_remain_available() {
        let mut app = test_app("custom-theme");
        let ctx = egui::Context::default();
        app.settings.theme = ThemeChoice::System;
        let mut palette = Palette::light();
        palette.accent = egui::Color32::RED;
        app.custom_themes = theme::custom::Catalog::from_themes(vec![theme::custom::CustomTheme {
            filename: "local.json".into(),
            palette,
        }]);
        app.apply(Action::SetCustomTheme("local.json".into()), &ctx);
        assert_eq!(app.palette, palette);
        assert_eq!(
            app.settings.theme,
            ThemeChoice::System,
            "custom selection preserves the built-in choice"
        );
        assert_eq!(ctx.theme(), egui::Theme::Light);
        assert!(!ctx.global_style().visuals.dark_mode);
        assert_eq!(ctx.global_style().visuals.panel_fill, palette.panel);
        let accepted = app.settings.clone();
        app.apply(Action::SetCustomTheme("missing.json".into()), &ctx);
        assert_eq!(
            app.settings, accepted,
            "an unavailable selection cannot erase usable settings"
        );
        app.apply(Action::SettingsChanged, &ctx);
        assert_eq!(
            ctx.theme(),
            egui::Theme::Light,
            "unrelated settings must keep the custom base"
        );
        for choice in ThemeChoice::ALL {
            app.apply(Action::SetTheme(choice), &ctx);
            assert_eq!(app.settings.theme, choice);
            assert!(app.settings.custom_theme.is_none());
            assert!(app.settings.custom_theme_cache.is_none());
            if choice != ThemeChoice::System {
                assert_eq!(app.palette.dark, choice == ThemeChoice::Dark);
            }
        }
    }

    #[test]
    fn premium_listeners_discover_personal_apps_before_requests_slow_down() {
        let mut app = test_app("personal-app-intro");
        app.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.user = Some(User {
            product: Some("premium".into()),
            ..User::default()
        });
        // Having seen an old transient toast does not count as seeing the intro.
        app.settings.personal_app_nudge_at = Some("2026-09-09T10:00:00Z".into());
        app.maybe_suggest_personal_app();
        assert!(matches!(app.dialog, Some(Dialog::PersonalAppIntro)));
        assert!(!app.settings.personal_app_intro_seen);
        app.actions.push(Action::CloseDialog);
        app.apply_actions(&egui::Context::default());
        assert!(app.settings.personal_app_intro_seen);
        assert!(app.dialog.is_none());
        let saved = serde_json::to_string(&app.settings).unwrap();
        let mut restarted = test_app("personal-app-intro-restart");
        restarted.settings = serde_json::from_str(&saved).unwrap();
        restarted.auth = app.auth.clone();
        restarted.user = app.user.clone();
        restarted.maybe_suggest_personal_app();
        assert!(restarted.dialog.is_none(), "dismissal survives a restart");
        assert!(app.toasts.is_empty(), "the old daily reminder is replaced");
    }

    #[test]
    fn personal_app_intro_waits_for_a_premium_account_using_shared_access() {
        let mut app = test_app("personal-app-intro-eligibility");
        app.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        for product in [None, Some("free"), Some("open")] {
            app.user = Some(User {
                product: product.map(str::to_string),
                ..User::default()
            });
            app.maybe_suggest_personal_app();
            assert!(app.dialog.is_none());
        }
        app.user = Some(User {
            product: Some("premium".into()),
            ..User::default()
        });
        app.settings.web_client_id = Some("personal-client".into());
        app.maybe_suggest_personal_app();
        assert!(app.dialog.is_none());
        app.settings.web_client_id = None;
        app.web_app = Some("personal-client".into());
        app.maybe_suggest_personal_app();
        assert!(app.dialog.is_none());
        app.web_app = None;
        app.auth = AuthStatus::SignedOut;
        app.maybe_suggest_personal_app();
        assert!(app.dialog.is_none());
        app.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.offline = true;
        app.maybe_suggest_personal_app();
        assert!(app.dialog.is_none());
    }

    #[test]
    fn personal_app_intro_defers_while_another_surface_is_in_use() {
        let mut app = test_app("personal-app-intro-defer");
        app.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.user = Some(User {
            product: Some("premium".into()),
            ..User::default()
        });
        app.dialog = Some(Dialog::Shortcuts);
        app.maybe_suggest_personal_app();
        assert!(matches!(app.dialog, Some(Dialog::Shortcuts)));
        app.dialog = None;
        app.show_devices = true;
        app.maybe_suggest_personal_app();
        assert!(app.dialog.is_none());
        app.show_devices = false;
        app.settings.winamp_window = true;
        app.maybe_suggest_personal_app();
        assert!(app.dialog.is_none());
        app.settings.winamp_window = false;
        app.open(Page::Settings);
        app.maybe_suggest_personal_app();
        assert!(app.dialog.is_none());
        app.open(Page::Home);
        app.maybe_suggest_personal_app();
        assert!(matches!(app.dialog, Some(Dialog::PersonalAppIntro)));
        app.handle_auth(AuthStatus::SignedOut);
        assert!(app.dialog.is_none());
        assert!(!app.settings.personal_app_intro_seen);
    }

    fn play(uri: &str, at: &str) -> crate::api::models::PlayHistory {
        crate::api::models::PlayHistory {
            track: crate::api::models::Track {
                uri: uri.to_string(),
                ..Default::default()
            },
            played_at: Some(at.to_string()),
            context: None,
        }
    }

    fn history(
        items: Vec<crate::api::models::PlayHistory>,
        before: Option<&str>,
    ) -> crate::api::models::CursorPage<crate::api::models::PlayHistory> {
        crate::api::models::CursorPage {
            items,
            cursors: Some(crate::api::models::Cursors {
                before: before.map(str::to_string),
                after: None,
            }),
            ..Default::default()
        }
    }

    /// Rule: the Recents tab is a history, so the same song played twice
    /// is two rows. Collapsing repeats would lose what the list is for.
    #[test]
    fn recents_keep_a_song_played_twice() {
        let mut app = test_app("recents-repeat");
        let page = history(
            vec![
                play("spotify:track:a", "2026-09-01T10:00:00Z"),
                play("spotify:track:a", "2026-09-01T09:00:00Z"),
                play("spotify:track:b", "2026-09-01T08:00:00Z"),
            ],
            Some("cursor-1"),
        );
        app.absorb_recents(page, 3);
        assert_eq!(app.recents.items.len(), 3, "both plays of a are kept");
    }

    /// Duplicate play records across page boundaries are removed.
    #[test]
    fn recents_drop_a_play_that_arrives_twice() {
        let mut app = test_app("recents-dedup");
        app.absorb_recents(
            history(
                vec![
                    play("spotify:track:a", "2026-09-01T10:00:00Z"),
                    play("spotify:track:b", "2026-09-01T09:00:00Z"),
                ],
                Some("cursor-1"),
            ),
            2,
        );
        app.absorb_recents(
            history(
                vec![
                    play("spotify:track:b", "2026-09-01T09:00:00Z"),
                    play("spotify:track:c", "2026-09-01T08:00:00Z"),
                ],
                Some("cursor-2"),
            ),
            2,
        );
        let uris: Vec<&str> = app
            .recents
            .items
            .iter()
            .map(|play| play.track.uri.as_str())
            .collect();
        assert_eq!(
            uris,
            vec!["spotify:track:a", "spotify:track:b", "spotify:track:c"]
        );
    }

    /// A short page ends history pagination, even with a cursor.
    #[test]
    fn a_short_page_ends_the_recents_list() {
        let mut app = test_app("recents-short");
        app.absorb_recents(
            history(
                vec![play("spotify:track:a", "2026-09-01T10:00:00Z")],
                Some("more"),
            ),
            50,
        );
        assert!(
            app.recents.complete,
            "a page of one against fifty is the end"
        );
        assert!(
            app.recents.after.is_none(),
            "and there is nothing to ask for"
        );
    }

    /// A full page with a cursor allows another history request.
    #[test]
    fn a_full_page_leaves_the_recents_list_open() {
        let mut app = test_app("recents-full");
        app.absorb_recents(
            history(
                vec![
                    play("spotify:track:a", "2026-09-01T10:00:00Z"),
                    play("spotify:track:b", "2026-09-01T09:00:00Z"),
                ],
                Some("cursor-1"),
            ),
            2,
        );
        assert!(!app.recents.complete);
        assert_eq!(app.recents.after.as_deref(), Some("cursor-1"));
    }

    /// The sidebar's Recently played order is newest first. Paging back
    /// through Recent reaches plays older than every context already in
    /// that order, so they go after it rather than ahead of it, and a
    /// context played more recently keeps its place.
    #[test]
    fn older_history_pages_do_not_jump_ahead_in_the_sidebar_order() {
        let ctx = egui::Context::default();
        let mut app = test_app("recents-older-contexts");
        let from = |uri: &str, context: &str, at: &str| crate::api::models::PlayHistory {
            context: Some(crate::api::models::Context {
                uri: context.to_string(),
                kind: "playlist".into(),
            }),
            ..play(uri, at)
        };
        let answer = |app: &mut App, page| {
            app.handle_api(ApiResponse::RecentlyPlayed {
                who: RecentsFor::Panel,
                generation: app.recents_generation,
                limit: 2,
                result: Ok(page),
            });
        };
        app.apply(Action::ReloadRecents, &ctx);
        answer(
            &mut app,
            history(
                vec![
                    from(
                        "spotify:track:a",
                        "spotify:playlist:p1",
                        "2026-09-01T10:00:00Z",
                    ),
                    from(
                        "spotify:track:b",
                        "spotify:playlist:p2",
                        "2026-09-01T09:00:00Z",
                    ),
                ],
                Some("cursor-1"),
            ),
        );
        assert_eq!(
            app.recent_contexts,
            ["spotify:playlist:p1", "spotify:playlist:p2"]
        );
        app.apply(Action::LoadMoreRecents, &ctx);
        app.note_recent_context("spotify:album:just-played");
        answer(
            &mut app,
            history(
                vec![
                    from(
                        "spotify:track:c",
                        "spotify:playlist:p3",
                        "2026-09-01T08:00:00Z",
                    ),
                    from(
                        "spotify:track:d",
                        "spotify:artist:x",
                        "2026-09-01T07:30:00Z",
                    ),
                    from(
                        "spotify:track:e",
                        "spotify:playlist:p2",
                        "2026-09-01T07:00:00Z",
                    ),
                ],
                Some("cursor-2"),
            ),
        );
        assert_eq!(app.recents.items.len(), 5, "the older page was taken");
        assert_eq!(
            app.recent_contexts,
            [
                "spotify:album:just-played",
                "spotify:playlist:p1",
                "spotify:playlist:p2",
                "spotify:playlist:p3"
            ],
            "older plays follow the newer ones"
        );

        // A full order has no room left for plays older than all of it.
        app.recent_contexts = (0..RECENT_CONTEXTS_KEPT)
            .map(|index| format!("spotify:playlist:full{index}"))
            .collect();
        let full = app.recent_contexts.clone();
        app.apply(Action::LoadMoreRecents, &ctx);
        answer(
            &mut app,
            history(
                vec![from(
                    "spotify:track:f",
                    "spotify:playlist:p4",
                    "2026-09-01T06:00:00Z",
                )],
                Some("cursor-3"),
            ),
        );
        assert_eq!(app.recents.items.len(), 6, "the oldest page was taken");
        assert_eq!(app.recent_contexts, full);
        app.apply(Action::ReloadRecents, &ctx);
        answer(
            &mut app,
            history(
                vec![from(
                    "spotify:track:g",
                    "spotify:playlist:p4",
                    "2026-09-01T11:00:00Z",
                )],
                None,
            ),
        );
        assert_eq!(app.recent_contexts[0], "spotify:playlist:p4");
        assert_eq!(app.recent_contexts.len(), RECENT_CONTEXTS_KEPT);
    }

    /// Saving to or removing from the library resets that shelf so it is
    /// read again from the top. A page asked for before the reset belongs
    /// to the old list: taking it starts the shelf part way through and
    /// marks its rows saved again, even one that was just removed.
    #[test]
    fn a_page_asked_for_before_a_shelf_reset_is_not_taken() {
        use crate::api::models::{
            Artist, CursorPage, Cursors, Page as ApiPage, SavedAlbum, SavedEpisode, SavedShow,
        };
        fn page<T>(items: Vec<T>, offset: u32) -> ApiPage<T> {
            let end = offset + items.len() as u32;
            ApiPage {
                items,
                total: 3,
                limit: 2,
                offset,
                next: (end < 3).then(|| "more".to_string()),
            }
        }
        let ctx = egui::Context::default();
        let mut app = test_app("library-late-pages");
        let changed = |app: &mut App, uri: &str, saved: bool| {
            app.handle_api(ApiResponse::SavedChanged {
                uris: vec![uri.to_string()],
                saved,
                result: Ok(()),
            });
        };

        // Followed artists continue from a cursor.
        let artists = |names: &[&str], after: Option<&str>| CursorPage {
            items: names
                .iter()
                .map(|name| Artist {
                    uri: format!("spotify:artist:{name}"),
                    ..Artist::default()
                })
                .collect(),
            cursors: Some(Cursors {
                after: after.map(str::to_string),
                before: None,
            }),
            ..CursorPage::default()
        };
        app.apply(Action::LoadMore(Page::Artists), &ctx);
        app.handle_api(ApiResponse::FollowedArtists {
            after: None,
            result: Ok(artists(&["a", "b"], Some("page-2"))),
        });
        app.apply(Action::LoadMore(Page::Artists), &ctx);
        changed(&mut app, "spotify:artist:new", true);
        app.apply(Action::LoadMore(Page::Artists), &ctx);
        app.handle_api(ApiResponse::FollowedArtists {
            after: Some("page-2".into()),
            result: Ok(artists(&["c"], None)),
        });
        assert!(
            app.library.artists.items.is_empty() && app.library.artists.loading,
            "the reset shelf waits for its own first page"
        );
        app.handle_api(ApiResponse::FollowedArtists {
            after: None,
            result: Ok(artists(&["new", "a"], Some("page-2"))),
        });
        let followed: Vec<&str> = app
            .library
            .artists
            .items
            .iter()
            .map(|artist| artist.uri.as_str())
            .collect();
        assert_eq!(followed, ["spotify:artist:new", "spotify:artist:a"]);
        assert_eq!(app.library.artists.after.as_deref(), Some("page-2"));

        // Albums, podcasts and episodes continue from an offset.
        let album = |id: &str| SavedAlbum {
            album: web_album(&format!("spotify:album:{id}"), "album", Some("album")),
            ..SavedAlbum::default()
        };
        app.apply(Action::LoadMore(Page::Albums), &ctx);
        app.handle_api(ApiResponse::SavedAlbums {
            offset: 0,
            result: Ok(page(vec![album("a"), album("b")], 0)),
        });
        app.apply(Action::LoadMore(Page::Albums), &ctx);
        changed(&mut app, "spotify:album:c", false);
        app.handle_api(ApiResponse::SavedAlbums {
            offset: 2,
            result: Ok(page(vec![album("c")], 2)),
        });
        assert!(app.library.albums.items.is_empty());
        assert!(
            !app.library.albums.loaded_once,
            "the shelf still starts from the top"
        );
        assert_eq!(
            app.is_saved("spotify:album:c"),
            Some(false),
            "a removed album stays removed"
        );

        let show = |id: &str| SavedShow {
            show: crate::api::models::Show {
                uri: format!("spotify:show:{id}"),
                ..Default::default()
            },
            ..SavedShow::default()
        };
        app.apply(Action::LoadMore(Page::Podcasts), &ctx);
        app.handle_api(ApiResponse::SavedShows {
            offset: 0,
            result: Ok(page(vec![show("a"), show("b")], 0)),
        });
        app.apply(Action::LoadMore(Page::Podcasts), &ctx);
        changed(&mut app, "spotify:show:new", true);
        app.handle_api(ApiResponse::SavedShows {
            offset: 2,
            result: Ok(page(vec![show("c")], 2)),
        });
        assert!(app.library.shows.items.is_empty() && !app.library.shows.loaded_once);

        let episode = |id: &str| SavedEpisode {
            episode: crate::api::models::Episode {
                uri: format!("spotify:episode:{id}"),
                ..Default::default()
            },
            ..SavedEpisode::default()
        };
        app.apply(Action::LoadMore(Page::Episodes), &ctx);
        app.handle_api(ApiResponse::SavedEpisodes {
            offset: 0,
            result: Ok(page(vec![episode("a"), episode("b")], 0)),
        });
        app.apply(Action::LoadMore(Page::Episodes), &ctx);
        changed(&mut app, "spotify:episode:new", true);
        app.handle_api(ApiResponse::SavedEpisodes {
            offset: 2,
            result: Ok(page(vec![episode("c")], 2)),
        });
        assert!(app.library.episodes.items.is_empty() && !app.library.episodes.loaded_once);
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(app.dirs.config.parent().unwrap());
    }

    /// Home's podcast shelf reads a bounded number of saved shows once per
    /// Home refresh, skips known audiobooks, and keeps what it shows until
    /// the current refresh answers.
    #[test]
    fn home_reads_a_few_saved_podcasts_once_per_refresh() {
        use crate::api::models::{Episode, Page as ApiPage, SavedShow};
        let mut app = test_app("home-podcasts");
        let show = |index: usize| Show {
            id: format!("s{index}"),
            uri: format!("spotify:show:s{index}"),
            ..Show::default()
        };
        app.load_home(false);
        assert!(app.library.shows.loading, "Home asks for the saved shows");
        assert!(app.backend.take_home_episode_requests().is_empty());

        app.audiobook_shows.insert("spotify:show:s1".into());
        app.handle_api(ApiResponse::SavedShows {
            offset: 0,
            result: Ok(ApiPage {
                items: (0..12)
                    .map(|index| SavedShow {
                        show: show(index),
                        ..SavedShow::default()
                    })
                    .collect(),
                total: 12,
                limit: 50,
                offset: 0,
                next: None,
            }),
        });
        let generation = app.home.generation;
        let expected: Vec<String> = [0, 2, 3, 4, 5, 6, 7, 8]
            .iter()
            .map(|index| format!("s{index}"))
            .collect();
        assert_eq!(
            app.backend.take_home_episode_requests(),
            vec![(expected, generation)]
        );

        // Visiting Home again soon, or another first page of the shows,
        // asks for nothing more.
        app.load_home(false);
        app.library.shows.next_offset = Some(0);
        app.handle_api(ApiResponse::SavedShows {
            offset: 0,
            result: Ok(ApiPage {
                items: vec![SavedShow {
                    show: show(0),
                    ..SavedShow::default()
                }],
                total: 1,
                limit: 50,
                offset: 0,
                next: None,
            }),
        });
        assert!(app.backend.take_home_episode_requests().is_empty());

        let episodes = |show_index: usize| {
            vec![(
                show(show_index),
                vec![Episode {
                    uri: format!("spotify:episode:e{show_index}"),
                    ..Episode::default()
                }],
            )]
        };
        app.handle_api(ApiResponse::HomeEpisodes {
            generation,
            result: Ok(episodes(0)),
        });
        assert_eq!(app.home.podcasts, episodes(0));

        app.load_home(true);
        assert_eq!(app.backend.take_home_episode_requests().len(), 1);
        app.handle_api(ApiResponse::HomeEpisodes {
            generation,
            result: Ok(episodes(2)),
        });
        assert_eq!(app.home.podcasts, episodes(0), "an older answer is ignored");
        app.handle_api(ApiResponse::HomeEpisodes {
            generation: app.home.generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(app.home.podcasts, episodes(0), "a failure keeps the shelf");
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(app.dirs.config.parent().unwrap());
    }

    /// Closing and reopening restores queue rows and their manual split.
    #[test]
    fn the_queue_comes_back_after_a_restart() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-queue-restart-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let options = AppOptions {
            media_controls: false,
            restore_sign_in: false,
            tray: false,
        };
        let mut app = App::new(
            &Waker::default(),
            dirs.clone(),
            Settings::default(),
            options,
        );
        app.local_ready = true;
        app.resume_track = Some("spotify:track:a".into());
        app.manual_queue = vec!["spotify:track:b".into()];
        app.queue = loaded_queue(
            "spotify:track:a",
            &["spotify:track:b", "spotify:track:ctx1"],
        );
        app.save_session();

        let options = AppOptions {
            media_controls: false,
            restore_sign_in: false,
            tray: false,
        };
        let app = App::new(&Waker::default(), dirs, Settings::default(), options);
        let (_, next) = queue_uris(&app);
        assert_eq!(
            next,
            vec!["spotify:track:b", "spotify:track:ctx1"],
            "the queue is shown as it was left"
        );
        assert_eq!(
            app.queued_rows_len(),
            1,
            "the remembered hand-queued song keeps its own section"
        );
    }

    /// Saving the queue writes the playing song and every row after it,
    /// each song once, in playing order.
    #[test]
    fn saving_the_queue_writes_each_song_once_in_order() {
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = loaded_queue(
            "spotify:track:a",
            &[
                "spotify:track:b",
                "spotify:track:a",
                "spotify:track:b",
                "spotify:track:c",
            ],
        );
        assert_eq!(
            app.queue_playlist_uris(),
            vec!["spotify:track:a", "spotify:track:b", "spotify:track:c"],
            "the playing song leads and a repeat wrap adds nothing"
        );
    }

    /// A saved song radio is named after its song; any other queue is
    /// named after the day.
    #[test]
    fn a_saved_radio_is_named_after_its_song() {
        let mut app = headless_app();
        app.track_cache.insert(
            "xyz".into(),
            crate::api::models::Track {
                id: Some("xyz".into()),
                uri: "spotify:track:xyz".into(),
                name: "Wish You Were Here".into(),
                ..Default::default()
            },
        );
        app.assumed_context = Some(AssumedContext {
            uri: "spotify:station:track:xyz".into(),
            shuffle: None,
            at: Instant::now(),
        });
        assert_eq!(app.queue_playlist_name(), "Wish You Were Here Radio");
        app.assumed_context = None;
        assert!(app.queue_playlist_name().starts_with("Queue "));
    }

    #[test]
    fn translated_context_labels_preserve_spotify_names() {
        use crate::i18n::Locale;
        use clap::ValueEnum;
        let mut app = headless_app();
        let today = jiff::Zoned::now().strftime("%Y-%m-%d").to_string();
        let title = "Home {date} 夜";
        app.track_cache.insert(
            "xyz".into(),
            crate::api::models::Track {
                id: Some("xyz".into()),
                uri: "spotify:track:xyz".into(),
                name: title.into(),
                ..Default::default()
            },
        );
        app.library.playlists = Loadable::Loaded(vec![crate::api::models::Playlist {
            id: "pl9".into(),
            uri: "spotify:playlist:pl9".into(),
            name: title.into(),
            ..Default::default()
        }]);
        for &locale in Locale::value_variants() {
            app.locale = locale;
            app.assumed_context = None;
            assert_eq!(
                app.queue_playlist_name(),
                gettext(locale, "Queue {date}").replace("{date}", &today)
            );
            for (uri, expected, page) in [
                (
                    "spotify:playlist:pl9",
                    title.into(),
                    Some(Page::Playlist("pl9".into())),
                ),
                (
                    "spotify:playlist:unloaded",
                    gettext(locale, "Playlist").into_owned(),
                    Some(Page::Playlist("unloaded".into())),
                ),
                (
                    "spotify:user:me:collection",
                    gettext(locale, "Liked Songs").into_owned(),
                    Some(Page::LikedSongs),
                ),
                (
                    "spotify:station:track:xyz",
                    gettext(locale, "{track} Radio").replace("{track}", title),
                    Some(Page::Radio("spotify:track:xyz".into())),
                ),
            ] {
                app.assumed_context = Some(AssumedContext {
                    uri: uri.into(),
                    shuffle: None,
                    at: Instant::now(),
                });
                let from = app.playing_from().unwrap();
                assert_eq!(from.name, expected);
                assert_eq!(from.page, page);
                if uri.starts_with("spotify:station:") {
                    assert_eq!(app.queue_playlist_name(), expected);
                }
            }
        }
    }

    /// The queue names where the playing song comes from and opens it.
    /// A radio has no page; a context whose name has not loaded is still
    /// named by kind so it can open.
    #[test]
    fn playing_from_names_the_context_and_its_page() {
        let mut app = headless_app();
        assert_eq!(app.playing_from(), None, "no context, no line");
        let assume = |app: &mut App, uri: &str| {
            app.assumed_context = Some(AssumedContext {
                uri: uri.into(),
                shuffle: None,
                at: Instant::now(),
            });
        };
        let named = |app: &App| {
            let from = app.playing_from().expect("a context is playing");
            (from.name, from.page)
        };

        app.library.playlists = Loadable::Loaded(vec![crate::api::models::Playlist {
            id: "pl9".into(),
            uri: "spotify:playlist:pl9".into(),
            name: "Long Way Home".into(),
            ..Default::default()
        }]);
        assume(&mut app, "spotify:playlist:pl9");
        assert_eq!(
            named(&app),
            ("Long Way Home".into(), Some(Page::Playlist("pl9".into())))
        );
        assume(&mut app, "spotify:playlist:unloaded");
        assert_eq!(
            named(&app),
            ("Playlist".into(), Some(Page::Playlist("unloaded".into())))
        );

        app.album_pages.insert(
            "alb1".into(),
            AlbumPage {
                album: Loadable::Loaded(crate::api::models::Album {
                    id: "alb1".into(),
                    uri: "spotify:album:alb1".into(),
                    name: "Black Sands".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        assume(&mut app, "spotify:album:alb1");
        assert_eq!(
            named(&app),
            ("Black Sands".into(), Some(Page::Album("alb1".into())))
        );

        assume(&mut app, "spotify:user:me:collection");
        assert_eq!(named(&app), ("Liked Songs".into(), Some(Page::LikedSongs)));

        app.track_cache.insert(
            "xyz".into(),
            crate::api::models::Track {
                id: Some("xyz".into()),
                uri: "spotify:track:xyz".into(),
                name: "Wish You Were Here".into(),
                ..Default::default()
            },
        );
        assume(&mut app, "spotify:station:track:xyz");
        assert_eq!(
            named(&app),
            (
                "Wish You Were Here Radio".into(),
                Some(Page::Radio("spotify:track:xyz".into()))
            )
        );
        assume(&mut app, "spotify:station:track:uncached");
        assert_eq!(
            named(&app),
            (
                "Radio".into(),
                Some(Page::Radio("spotify:track:uncached".into()))
            )
        );
        assume(&mut app, "spotify:station:playlist:pl9");
        assert_eq!(
            named(&app),
            (
                "Long Way Home Radio".into(),
                Some(Page::Radio("spotify:playlist:pl9".into()))
            )
        );
    }

    /// A replaced window, as when the mini player's taskbar setting
    /// changes, is titled with the playing song again, not left as
    /// "Spotifast".
    #[test]
    fn a_new_window_is_titled_with_the_playing_song() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.attach(&ctx);
        app.window_title = "Bonobo - Rosewood".into();
        app.attach(&ctx);
        assert!(
            app.window_title.is_empty(),
            "the next frame sends the title again"
        );
    }

    fn radio_song(id: &str, artist: &str) -> Track {
        Track {
            id: Some(id.into()),
            uri: format!("spotify:track:{id}"),
            name: format!("Song {id}"),
            duration_ms: 200_000,
            artists: vec![crate::api::models::ArtistRef {
                name: artist.into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn draw_radio(
        ctx: &egui::Context,
        app: &mut App,
        seed: &str,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1240.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| crate::ui::radio::radio(app, ui, seed),
        );
        output.textures_delta.clear();
        app.apply_actions(ctx);
        output.platform_output.accesskit_update.unwrap()
    }

    fn click_labelled(
        ctx: &egui::Context,
        app: &mut App,
        seed: &str,
        label: &str,
    ) -> egui::accesskit::TreeUpdate {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, TreeId};
        let tree = draw_radio(ctx, app, seed, Vec::new());
        let id = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .unwrap_or_else(|| panic!("a {label} control"))
            .0;
        draw_radio(
            ctx,
            app,
            seed,
            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                target_tree: TreeId::ROOT,
                target_node: id,
                action: AccessibleAction::Click,
                data: None,
            })],
        )
    }

    /// #369: Go to song radio opens the radio's page, as in Spotify's app,
    /// and plays nothing until asked.
    #[test]
    fn song_radio_opens_its_page_without_playing() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "test".into(),
        };
        app.apply(Action::Open(Page::Radio("spotify:track:xyz".into())), &ctx);
        assert_eq!(app.page(), &Page::Radio("spotify:track:xyz".into()));
        assert!(matches!(
            app.radio_pages["spotify:track:xyz"].songs,
            Loadable::Loading
        ));
        assert!(app.queued_play.is_none());
        assert!(app.optimistic_playing.is_none());
        assert!(!app.show_queue_panel);
        assert_eq!(
            Page::decode(&Page::Radio("spotify:playlist:pl9".into()).encode()),
            Some(Page::Radio("spotify:playlist:pl9".into()))
        );
        assert_eq!(Page::decode("radio:spotify:show:abc"), None);
    }

    /// Spotify mixes a station afresh each time it is asked, so the page's
    /// Play plays the songs on screen, shuffled or not, as the radio.
    #[test]
    fn a_radio_plays_the_songs_it_shows() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::theme::install(&ctx);
        let seed = "spotify:playlist:pl9";
        for shuffle in [false, true] {
            let mut app = headless_app();
            app.auth = AuthStatus::Connected {
                username: "test".into(),
            };
            app.backend.set_offline(true);
            app.shuffle_wanted = shuffle;
            app.apply(Action::Open(Page::Radio(seed.into())), &ctx);
            let generation = app.radio_pages[seed].generation;
            let songs = vec![radio_song("a", "Björk"), radio_song("b", "Arca")];
            app.receive_radio(seed, generation, Ok(songs));
            click_labelled(&ctx, &mut app, seed, "Play");
            assert_eq!(
                app.queued_play.as_ref().expect("a play request").uris,
                vec!["spotify:track:a".to_string(), "spotify:track:b".into()],
                "shuffle {shuffle}: the shown songs play"
            );
            assert_eq!(
                app.playing_context_uri().as_deref(),
                Some("spotify:station:playlist:pl9"),
                "the queue names the radio"
            );
            app.backend.shutdown();
        }
    }

    /// An answer for an earlier request does not replace the page, a
    /// failure can be retried, and Refresh asks for a new mix.
    #[test]
    fn a_radio_takes_only_its_latest_answer() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "test".into(),
        };
        let seed = "spotify:album:alb1";
        app.apply(Action::Open(Page::Radio(seed.into())), &ctx);
        let first = app.radio_pages[seed].generation;
        app.receive_radio(
            seed,
            first,
            Err("Couldn't load this radio. Try again.".into()),
        );
        assert!(matches!(app.radio_pages[seed].songs, Loadable::Failed(_)));
        app.apply(Action::Reload(Page::Radio(seed.into())), &ctx);
        let second = app.radio_pages[seed].generation;
        assert_ne!(first, second);
        app.receive_radio(seed, first, Ok(vec![radio_song("old", "Old")]));
        assert!(matches!(app.radio_pages[seed].songs, Loadable::Loading));
        app.receive_radio(seed, second, Ok(vec![radio_song("new", "New")]));
        let songs = app.radio_pages[seed].songs.get().expect("the latest mix");
        assert_eq!(songs[0].uri, "spotify:track:new");
        assert!(app.track_cache.contains_key("new"));
    }

    /// Refresh keeps the mix on screen until the new one arrives, shows the
    /// new songs once it does, and keeps the old ones if it fails.
    #[test]
    fn refreshing_a_radio_keeps_its_songs_until_the_new_mix_arrives() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::theme::install(&ctx);
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "test".into(),
        };
        let seed = "spotify:artist:art1";
        app.apply(Action::Open(Page::Radio(seed.into())), &ctx);
        let generation = app.radio_pages[seed].generation;
        app.receive_radio(seed, generation, Ok(vec![radio_song("old", "Old")]));
        let rows = |app: &mut App| {
            draw_radio(&ctx, app, seed, Vec::new());
            app.table_rows[&Page::Radio(seed.into())].items[0]
                .0
                .uri()
                .to_string()
        };
        assert_eq!(rows(&mut app), "spotify:track:old");

        app.apply(Action::Reload(Page::Radio(seed.into())), &ctx);
        assert!(app.radio_pages[seed].refreshing);
        assert_eq!(rows(&mut app), "spotify:track:old", "the old mix stays");
        let asked = app.radio_pages[seed].generation;
        app.receive_radio(seed, asked, Ok(vec![radio_song("new", "New")]));
        assert!(!app.radio_pages[seed].refreshing);
        assert_eq!(
            rows(&mut app),
            "spotify:track:new",
            "the table shows the new mix"
        );

        app.apply(Action::Reload(Page::Radio(seed.into())), &ctx);
        let asked = app.radio_pages[seed].generation;
        app.receive_radio(
            seed,
            asked,
            Err("Couldn't load this radio. Try again.".into()),
        );
        assert_eq!(
            rows(&mut app),
            "spotify:track:new",
            "a failed refresh keeps the songs"
        );
        app.backend.shutdown();
    }

    /// Save as playlist keeps the mix on screen, in order, under the
    /// radio's name.
    #[test]
    fn saving_a_radio_makes_a_playlist_of_its_songs() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::theme::install(&ctx);
        let mut app = headless_app();
        app.auth = AuthStatus::Connected {
            username: "test".into(),
        };
        app.backend.set_offline(true);
        let seed = "spotify:track:xyz";
        app.track_cache.insert("xyz".into(), {
            let mut seed_song = radio_song("xyz", "Pink Floyd");
            seed_song.name = "Wish You Were Here".into();
            seed_song
        });
        app.apply(Action::Open(Page::Radio(seed.into())), &ctx);
        let generation = app.radio_pages[seed].generation;
        app.receive_radio(
            seed,
            generation,
            Ok(vec![radio_song("b", "Camel"), radio_song("a", "Yes")]),
        );
        let tree = draw_radio(&ctx, &mut app, seed, Vec::new());
        assert_eq!(
            app.radio_name(seed).as_deref(),
            Some("Wish You Were Here Radio"),
            "the page is named after its song"
        );
        let id = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Save as playlist"))
            .expect("a Save as playlist button")
            .0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1240.0, 800.0),
                )),
                events: vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        target_tree: egui::accesskit::TreeId::ROOT,
                        target_node: id,
                        action: egui::accesskit::Action::Click,
                        data: None,
                    },
                )],
                ..Default::default()
            },
            |ui| crate::ui::radio::radio(&mut app, ui, seed),
        );
        output.textures_delta.clear();
        let saved = std::mem::take(&mut app.actions);
        assert!(matches!(saved.as_slice(), [Action::SaveRadio(uri)] if uri == seed));
        app.apply(Action::SaveRadio(seed.into()), &ctx);
        assert!(
            app.actions.iter().any(|action| matches!(
                action,
                Action::CreatePlaylist { name, public: false, add_uris }
                    if name == "Wish You Were Here Radio"
                        && add_uris == &["spotify:track:b".to_string(), "spotify:track:a".into()]
            )),
            "{:?}",
            app.actions
        );
        app.backend.shutdown();
    }

    /// MilkDrop playback keys produce the same actions as the main window.
    #[cfg(feature = "milkdrop")]
    #[test]
    fn the_milkdrop_window_drives_playback() {
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;

        for command in [
            "play-pause",
            "next",
            "previous",
            "mute",
            "save-toggle",
            "shuffle",
            "volume-up",
            "volume-down",
        ] {
            app.actions.clear();
            app.milkdrop_command(command);
            assert_eq!(
                app.actions.len(),
                1,
                "{command} asks the player for one thing"
            );
        }

        app.actions.clear();
        app.milkdrop_command("save-toggle");
        assert!(matches!(
            app.actions.first(),
            Some(Action::ToggleSaved(uri)) if uri == "spotify:track:a"
        ));
        app.actions.clear();
        app.milkdrop_command("next");
        assert!(matches!(app.actions.first(), Some(Action::Next)));
        app.actions.clear();
        app.milkdrop_command("volume-down");
        assert!(matches!(app.actions.first(), Some(Action::VolumeBy(-5))));

        // Ignore unknown commands.
        app.actions.clear();
        app.milkdrop_command("teleport");
        assert!(app.actions.is_empty());
    }

    /// The first reported screen rate sets the default FPS, but later reports
    /// do not override a configured value.
    #[cfg(feature = "milkdrop")]
    #[test]
    fn the_frame_rate_matches_the_screen_the_first_time_it_is_known() {
        let mut app = headless_app();
        assert_eq!(app.settings.milkdrop_screen_hz, 0, "no screen has spoken");
        assert_eq!(app.settings.milkdrop_fps, crate::milkdrop::DEFAULT_FPS);

        app.learn_screen_hz(144);
        assert_eq!(app.settings.milkdrop_screen_hz, 144);
        assert_eq!(app.settings.milkdrop_fps, 144, "smooth without being asked");

        // Keep the configured FPS when the screen changes.
        app.settings.milkdrop_fps = 30;
        app.learn_screen_hz(60);
        assert_eq!(
            app.settings.milkdrop_screen_hz, 60,
            "the new screen is noted"
        );
        assert_eq!(app.settings.milkdrop_fps, 30, "their number stands");
    }

    /// A window in the Dock is drawn no frames, so the one frame a Show
    /// request buys has to be the frame that brings it back. Focus alone
    /// leaves it down there, and nothing asks again.
    #[test]
    fn showing_a_minimized_window_restores_it_before_focusing_it() {
        // #given a window exists, minimized rather than closed
        let mut app = headless_app();
        let ctx = egui::Context::default();
        assert!(!app.window_hidden, "a window this app still owns");

        // #when something asks for the window: the Dock, the tray, or
        // `spotifast show`
        let mut output = ctx.run_ui(Default::default(), |ui| {
            app.apply(Action::ShowWindow, ui.ctx());
        });
        output.textures_delta.clear();

        // #then
        let commands = &output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("the root viewport")
            .commands;
        assert!(
            commands.contains(&egui::ViewportCommand::Minimized(false)),
            "asked to show, but never asked to come out of the Dock: {commands:?}"
        );
        assert!(
            commands.contains(&egui::ViewportCommand::Focus),
            "restored but left behind whatever is in front of it: {commands:?}"
        );
    }

    #[test]
    fn repeated_show_requests_never_close_or_hide_the_window() {
        for hidden in [false, true] {
            let mut app = headless_app();
            app.window_hidden = hidden;
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(Default::default(), |ui| {
                app.apply(Action::ShowWindow, ui.ctx());
                app.apply(Action::ShowWindow, ui.ctx());
            });
            output.textures_delta.clear();
            assert!(!app.hide_intent);
            assert_eq!(app.wants_show, hidden);
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            assert!(!commands.contains(&egui::ViewportCommand::Close));
            if !hidden {
                assert!(commands.contains(&egui::ViewportCommand::Focus));
            }
            app.backend.shutdown();
        }
    }

    /// The media controls are handed a downloaded file, never a URL: macOS
    /// loads cover art itself and dereferences a failed load without
    /// checking it, so a URL that does not answer aborts the process. The
    /// sync runs every frame, so the answer is remembered -- which means
    /// emptying the cache has to forget it, or the path outlives the file.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn the_media_controls_only_hear_about_artwork_that_exists() {
        // #given
        let mut app = headless_app();
        let ctx = egui::Context::default();
        let url = "https://i.scdn.co/image/abc";
        let file = app.dirs.cache.join("art").join("0badc0de");
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("the art cache");
        std::fs::write(&file, b"jpeg-ish").expect("a cached file");

        // #then nothing has been downloaded for this song yet
        assert_eq!(app.media_art_file(&ctx, url), None);
        assert!(
            !app.backend.art().prefetch(&ctx, url),
            "the miss should have started the download"
        );

        // #when the cache holds it, that file is what the controls are told
        app.media_art = Some((url.to_owned(), file.clone()));
        assert_eq!(app.media_art_file(&ctx, url), Some(file.clone()));

        // #then another song is not covered by what is remembered
        assert_eq!(
            app.media_art_file(&ctx, "https://i.scdn.co/image/def"),
            None
        );

        // #when the artwork cache is emptied, the remembered path goes too
        app.actions.push(Action::ClearArtCache);
        app.apply_actions(&ctx);
        assert_eq!(app.media_art, None, "a path into a deleted cache");

        let _ = std::fs::remove_file(&file);
    }

    /// MPRIS hands the desktop the artwork URL and reads no file, so the
    /// controls have nothing to download for it: the full-size cover is
    /// fetched on the platforms that load the image themselves.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_media_controls_are_told_no_file_and_download_nothing() {
        // #given
        let mut app = headless_app();
        let ctx = egui::Context::default();
        let url = "https://i.scdn.co/image/abc";

        // #then MPRIS is left to the URL alone
        assert_eq!(app.media_art_file(&ctx, url), None);
        assert!(
            app.backend.art().prefetch(&ctx, url),
            "a download was started for artwork nothing here reads"
        );
    }

    /// Emptying the artwork cache has to let the cover come back. The files
    /// are gone, so what the loader remembers of them goes too: an entry
    /// left behind answers the next request with a file that is no longer
    /// there, and the playing song stays coverless until the track changes.
    #[test]
    fn clearing_the_artwork_cache_lets_the_cover_come_back() {
        // #given artwork that has already been asked for
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.attach(&ctx);
        let url = "https://i.scdn.co/image/abc";
        assert!(app.backend.art().prefetch(&ctx, url), "the first request");

        // #when the artwork cache is emptied
        app.actions.push(Action::ClearArtCache);
        app.apply_actions(&ctx);

        // #then the next request downloads it again
        assert!(
            app.backend.art().prefetch(&ctx, url),
            "the loader still remembers artwork that has been deleted"
        );
    }

    #[test]
    fn thumbnail_transport_tracks_optimistic_pause_and_window_recreation() {
        use crate::thumbbar::{Icon, ThumbCommand, buttons};
        let mut app = headless_app();
        app.backend.set_offline(true);
        let ctx = egui::Context::default();
        assert!(!app.thumb_state(true).has_track);
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:thumbnail".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        let state = app.thumb_state(true);
        assert!(state.has_track && state.playing && state.can_control);
        assert_eq!(buttons(&state)[1].icon, Icon::Pause);
        app.apply(ThumbCommand::PlayPause.action(&state).unwrap(), &ctx);
        assert_eq!(
            buttons(&app.thumb_state(true))[1].icon,
            Icon::Play,
            "pause updates before a backend reply"
        );
        app.window_gone();
        app.attach(&ctx);
        assert_eq!(buttons(&app.thumb_state(false))[1].icon, Icon::Play);
        assert!(!app.thumb_state(false).dark);
        app.backend.shutdown();
    }

    #[test]
    fn changing_mini_taskbar_visibility_recreates_only_an_open_mini_window() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let ctx = egui::Context::default();
        app.apply(Action::SetWinampTaskbar(false), &ctx);
        assert!(!app.settings.winamp_show_taskbar);
        assert!(!app.switch_intent, "settings do not close the main window");
        app.settings.winamp_window = true;
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:continues".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        let mut output = ctx.run_ui(egui::RawInput::default(), |_ui| {
            app.apply(Action::SetWinampTaskbar(true), &ctx);
        });
        output.textures_delta.clear();
        assert!(app.settings.winamp_window && app.switch_intent);
        assert!(!app.hide_intent && !app.quit_requested);
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );
        assert_eq!(app.local.playback, Playback::Playing);
        assert_eq!(
            app.local.track.as_ref().unwrap().uri,
            "spotify:track:continues"
        );
        app.switch_intent = false;
        let mut output = ctx.run_ui(egui::RawInput::default(), |_ui| {
            app.apply(Action::SetWinampTaskbar(true), &ctx);
        });
        output.textures_delta.clear();
        assert!(!app.switch_intent);
        assert!(
            !output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close))
        );
        app.apply(Action::ToggleWinampWindow, &ctx);
        assert!(
            !app.settings.winamp_window,
            "returning to the main interface remains available"
        );
        app.backend.shutdown();
    }

    #[test]
    fn lyrics_fullscreen_restores_the_previous_window_mode() {
        for was_fullscreen in [false, true] {
            let mut app = headless_app();
            let ctx = egui::Context::default();
            let mut input = egui::RawInput::default();
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .fullscreen = Some(was_fullscreen);
            let mut output = ctx.run_ui(input, |ui| {
                app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
                assert!(app.show_lyrics_panel);
                assert_eq!(app.lyrics_fullscreen, Some(was_fullscreen));
                app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
                assert_eq!(app.lyrics_fullscreen, Some(was_fullscreen));
                app.apply(Action::SetLyricsFullscreen(false), ui.ctx());
                assert!(app.show_lyrics_panel);
                assert_eq!(app.lyrics_fullscreen, None);
            });
            output.textures_delta.clear();
            let commands: Vec<_> = output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .filter_map(|command| {
                    if let egui::ViewportCommand::Fullscreen(value) = command {
                        Some(*value)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(commands, vec![true, was_fullscreen]);
        }
    }

    #[test]
    fn native_fullscreen_exit_restores_the_previous_window_mode() {
        for was_fullscreen in [false, true] {
            let mut app = headless_app();
            app.lyrics_fullscreen = Some(was_fullscreen);
            app.lyrics_fullscreen_seen = true;
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            let mut input = egui::RawInput::default();
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .fullscreen = Some(false);
            let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
            output.textures_delta.clear();
            assert_eq!(app.lyrics_fullscreen, None);
            assert!(!app.lyrics_fullscreen_seen);
            let commands: Vec<_> = output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .filter_map(|command| {
                    if let egui::ViewportCommand::Fullscreen(value) = command {
                        Some(*value)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(commands, vec![was_fullscreen]);
        }
    }

    #[test]
    fn leaving_lyrics_fullscreen_keeps_window_bounds_until_native_restore() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        app.lyrics_fullscreen = Some(false);
        app.lyrics_fullscreen_seen = true;
        app.last_window_size = Some([900.0, 650.0]);
        app.last_window_pos = Some([100.0, 80.0]);
        // Escape is applied while the viewport still reports the old screen.
        app.actions.push(Action::SetLyricsFullscreen(false));
        for _ in 0..2 {
            let mut input = egui::RawInput::default();
            let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            viewport.fullscreen = Some(true);
            viewport.inner_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1920.0, 1080.0),
            ));
            viewport.outer_rect = viewport.inner_rect;
            let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
            output.textures_delta.clear();
            assert_eq!(app.lyrics_fullscreen, None);
            assert_eq!(app.last_window_size, Some([900.0, 650.0]));
            assert_eq!(app.last_window_pos, Some([100.0, 80.0]));
        }
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.fullscreen = Some(false);
        viewport.inner_rect = Some(egui::Rect::from_min_size(
            egui::pos2(120.0, 100.0),
            egui::vec2(940.0, 680.0),
        ));
        viewport.outer_rect = viewport.inner_rect;
        let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
        output.textures_delta.clear();
        assert_eq!(app.last_window_size, Some([940.0, 680.0]));
        assert_eq!(app.last_window_pos, Some([120.0, 100.0]));
        app.backend.shutdown();
    }

    #[test]
    fn reopening_lyrics_during_native_exit_keeps_the_original_window_mode() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.lyrics_fullscreen = Some(false);
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .fullscreen = Some(true);
        let mut output = ctx.run_ui(input, |ui| {
            app.apply(Action::SetLyricsFullscreen(false), ui.ctx());
            app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
        });
        output.textures_delta.clear();
        assert_eq!(app.lyrics_fullscreen, Some(false));
        assert_eq!(app.lyrics_fullscreen_restoring, None);
        app.backend.shutdown();
    }

    #[cfg(windows)]
    #[test]
    fn lyrics_fullscreen_clears_and_restores_windows_maximization() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
        viewport.fullscreen = Some(false);
        viewport.maximized = Some(true);
        let mut output = ctx.run_ui(input, |ui| {
            app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
            app.apply(Action::SetLyricsFullscreen(false), ui.ctx());
        });
        output.textures_delta.clear();
        assert_eq!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .filter(|command| {
                    matches!(
                        command,
                        egui::ViewportCommand::Maximized(_) | egui::ViewportCommand::Fullscreen(_)
                    )
                })
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                egui::ViewportCommand::Maximized(false),
                egui::ViewportCommand::Fullscreen(true),
                egui::ViewportCommand::Fullscreen(false),
                egui::ViewportCommand::Maximized(true),
            ]
        );
        app.last_window_size = Some([900.0, 650.0]);
        // A native exit can report windowed before maximization catches up.
        // Do not save that intermediate size over the restored geometry.
        for maximized in [false, true] {
            let mut input = egui::RawInput::default();
            let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
            viewport.fullscreen = Some(false);
            viewport.maximized = Some(maximized);
            viewport.inner_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 852.0),
            ));
            let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
            output.textures_delta.clear();
            assert_eq!(app.lyrics_fullscreen_restoring.is_none(), maximized);
            assert_eq!(
                app.last_window_size,
                Some(if maximized {
                    [1440.0, 852.0]
                } else {
                    [900.0, 650.0]
                })
            );
        }
        app.backend.shutdown();
    }

    #[cfg(feature = "demo")]
    #[test]
    fn escape_from_fullscreen_repositions_the_returning_lyrics_panel() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        crate::demo::apply_flags(&mut app, None, Some("lyrics"));
        app.lyrics_fullscreen = Some(false);
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(760.0, 520.0),
            )),
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .fullscreen = Some(true);
        let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
        output.textures_delta.clear();
        assert_eq!(app.lyrics_fullscreen, None);
        assert_eq!(
            app.lyrics_line_shown, None,
            "the fullscreen line cannot stop the returning panel from positioning itself"
        );
        app.backend.shutdown();
    }

    #[test]
    fn closing_lyrics_leaves_fullscreen() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            app.apply(Action::SetLyricsFullscreen(true), ui.ctx());
            app.apply(Action::ToggleLyricsPanel, ui.ctx());
        });
        output.textures_delta.clear();
        assert!(!app.show_lyrics_panel);
        assert_eq!(app.lyrics_fullscreen, None);
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Fullscreen(false))
        );
    }

    fn headless_app() -> App {
        let root =
            std::env::temp_dir().join(format!("spotifast-volume-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let mut app = App::new(
            &Waker::default(),
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.local_ready = true;
        app
    }

    /// Dock menu picks wait in their own queue, which the application reads
    /// with or without a window, and never in the menu bar's, which only a
    /// window reads. A pick in the tray would otherwise wait for a window.
    #[cfg(target_os = "macos")]
    #[test]
    fn dock_menu_picks_become_playback_actions_without_a_window() {
        use crate::mac_menu::{self, MenuCommand};
        let mut app = headless_app();
        let _ = mac_menu::drain_dock_commands();
        mac_menu::push_dock_command(MenuCommand::PlayPause);
        mac_menu::push_dock_command(MenuCommand::Next);
        mac_menu::push_dock_command(MenuCommand::Previous);
        assert!(mac_menu::drain_commands().is_empty());
        app.actions.clear();
        app.handle_dock_menu();
        assert!(matches!(
            app.actions.as_slice(),
            [Action::TogglePlay, Action::Next, Action::Previous]
        ));
        assert!(mac_menu::drain_dock_commands().is_empty());
    }

    fn seed_playlist(app: &mut App, id: &str) {
        app.playlist_pages
            .insert(id.to_string(), PlaylistPage::default());
        app.open(Page::Playlist(id.to_string()));
        std::thread::sleep(Duration::from_millis(1));
    }

    fn store_track(app: &mut App, id: &str) {
        app.track_cache.insert(
            id.to_string(),
            Track {
                id: Some(id.to_string()),
                uri: format!("spotify:track:{id}"),
                ..Track::default()
            },
        );
    }

    fn web_album(uri: &str, album_type: &str, album_group: Option<&str>) -> Album {
        Album {
            id: uri.rsplit(':').next().unwrap_or_default().into(),
            uri: uri.into(),
            album_type: Some(album_type.into()),
            album_group: album_group.map(str::to_string),
            ..Album::default()
        }
    }

    #[test]
    fn precise_album_types_are_requested_only_for_web_singles_and_once_per_uri() {
        use crate::api::models::{Page as ApiPage, SavedAlbum};

        let mut app = headless_app();
        app.backend.set_offline(true);
        let first = web_album("spotify:album:first", "single", Some("single"));
        let second = web_album("spotify:album:second", "single", Some("single"));
        let detail = web_album("spotify:album:detail", "single", None);
        let album = web_album("spotify:album:album", "album", Some("album"));
        let compilation = web_album(
            "spotify:album:compilation",
            "compilation",
            Some("compilation"),
        );
        let appears_on = web_album("spotify:album:appears", "single", Some("appears_on"));

        app.handle_api(ApiResponse::SavedAlbums {
            offset: 0,
            result: Ok(ApiPage {
                items: vec![
                    SavedAlbum {
                        album: first.clone(),
                        ..SavedAlbum::default()
                    },
                    SavedAlbum {
                        album: compilation,
                        ..SavedAlbum::default()
                    },
                    SavedAlbum {
                        album,
                        ..SavedAlbum::default()
                    },
                ],
                ..ApiPage::default()
            }),
        });
        assert_eq!(
            app.backend.take_album_type_requests(),
            vec![vec![first.uri.clone()]]
        );

        app.artist_pages
            .insert("artist".into(), ArtistPage::default());
        app.handle_api(ApiResponse::ArtistAlbums {
            id: "artist".into(),
            groups: "album,single,compilation,appears_on".into(),
            offset: 0,
            result: Ok(ApiPage {
                items: vec![first, second.clone(), appears_on],
                ..ApiPage::default()
            }),
        });
        assert_eq!(
            app.backend.take_album_type_requests(),
            vec![vec![second.uri.clone()]]
        );

        app.album_pages
            .insert(detail.id.clone(), AlbumPage::default());
        app.handle_api(ApiResponse::Album {
            id: detail.id.clone(),
            result: Ok(detail.clone()),
        });
        assert_eq!(
            app.backend.take_album_type_requests(),
            vec![vec![detail.uri.clone()]]
        );

        app.handle_api(ApiResponse::Album {
            id: detail.id.clone(),
            result: Ok(detail),
        });
        assert!(app.backend.take_album_type_requests().is_empty());
    }

    #[test]
    fn precise_ep_confirmation_preserves_fallback_and_web_kind_precedence() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let ep = web_album("spotify:album:ep", "single", Some("single"));
        let failed = web_album("spotify:album:failed", "single", Some("single"));
        let timed_out = web_album("spotify:album:timeout", "single", Some("single"));
        let regular = web_album("spotify:album:regular", "single", Some("single"));

        app.request_album_types([&ep, &failed, &timed_out, &regular]);
        let _ = app.backend.take_album_type_requests();
        app.handle_backend_events(vec![
            Event::AlbumType {
                uri: ep.uri.clone(),
                result: Ok(true),
            },
            Event::AlbumType {
                uri: failed.uri.clone(),
                result: Err("unavailable".into()),
            },
            Event::AlbumType {
                uri: timed_out.uri.clone(),
                result: Err("album metadata timed out".into()),
            },
            Event::AlbumType {
                uri: regular.uri.clone(),
                result: Ok(false),
            },
        ]);

        assert_eq!(app.album_kind_label(&ep), "EP");
        assert_eq!(app.album_kind_label(&failed), "Single");
        assert_eq!(app.album_kind_label(&timed_out), "Single");
        assert_eq!(app.album_kind_label(&regular), "Single");
        let appears_on = web_album(&ep.uri, "single", Some("appears_on"));
        let compilation = web_album(&ep.uri, "single", Some("compilation"));
        let album = web_album(&ep.uri, "single", Some("album"));
        assert_eq!(app.album_kind_label(&appears_on), "Appears On");
        assert_eq!(app.album_kind_label(&compilation), "Compilation");
        assert_eq!(app.album_kind_label(&album), "Album");

        app.request_album_types([&failed, &timed_out]);
        assert!(
            app.backend.take_album_type_requests().is_empty(),
            "failed lookups are terminal for this session"
        );

        app.handle_auth(AuthStatus::SignedOut);
        assert_eq!(app.album_kind_label(&ep), "Single");
        app.handle_auth(AuthStatus::Connected {
            username: "next-session".into(),
        });
        app.request_album_types([&ep]);
        assert_eq!(app.backend.take_album_type_requests(), vec![vec![ep.uri]]);
    }

    #[test]
    fn two_toggle_play_actions_in_one_batch_return_to_playing() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            title: "A".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.refresh_frame_now();
        assert!(app.now_playing().expect("playing").playing);
        app.actions.push(Action::TogglePlay);
        app.actions.push(Action::TogglePlay);
        app.apply_actions(&ctx);
        assert!(
            app.now_playing().expect("playing").playing,
            "the second toggle must see the first pause, not the drawing snapshot"
        );
    }

    #[test]
    fn page_cache_stays_bounded_when_history_is_long() {
        let mut app = headless_app();
        for i in 0..20 {
            seed_playlist(&mut app, &format!("pl{i}"));
        }
        assert!(
            app.playlist_pages.len() <= 12,
            "history must not protect more pages than the cap: {}",
            app.playlist_pages.len()
        );
        assert!(
            app.playlist_pages.contains_key("pl19"),
            "the open page stays"
        );
        assert!(
            !app.playlist_pages.contains_key("pl0"),
            "the oldest page is dropped"
        );
    }

    #[test]
    fn navigating_past_the_cache_limit_keeps_edits_until_every_write_and_snapshot_is_confirmed() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "edited".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "edited".into(),
                    snapshot_id: Some("old".into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                ..Default::default()
            },
        );
        app.open(Page::Playlist("edited".into()));
        for uri in ["spotify:track:second", "spotify:track:third"] {
            app.apply(
                Action::ConfirmAddToPlaylist {
                    position: None,
                    playlist_id: "edited".into(),
                    playlist_name: "Edited".into(),
                    items: vec![cached_playlist_row(uri).playable().unwrap().clone()],
                },
                &egui::Context::default(),
            );
        }
        assert_eq!(app.playlist_pages["edited"].pending_writes, 2);
        for i in 0..25 {
            seed_playlist(&mut app, &format!("other-{i}"));
        }
        app.open(Page::Playlist("edited".into()));
        let rows = |app: &App| {
            app.playlist_pages["edited"]
                .items
                .items
                .iter()
                .filter_map(|item| item.playable().map(|item| item.uri().to_string()))
                .collect::<Vec<_>>()
        };
        let expected = [
            "spotify:track:first",
            "spotify:track:second",
            "spotify:track:third",
        ];
        assert_eq!(rows(&app), expected);
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Ok(Some("first-write".into())),
        });
        let generation = app.playlist_pages["edited"].generation;
        let metadata = |snapshot: &str| ApiResponse::Playlist {
            id: "edited".into(),
            generation,
            result: Ok(Playlist {
                id: "edited".into(),
                snapshot_id: Some(snapshot.into()),
                items_count: Some(TrackCount { total: 3 }),
                ..Default::default()
            }),
        };
        app.handle_api(metadata("first-write"));
        assert_eq!(app.playlist_pages["edited"].pending_writes, 1);
        assert_eq!(
            app.playlist_pages["edited"].optimistic_snapshot.as_deref(),
            Some("first-write"),
            "metadata cannot confirm the page while another write is pending"
        );
        for i in 25..50 {
            seed_playlist(&mut app, &format!("other-{i}"));
        }
        assert_eq!(rows(&app), expected, "one write is still pending");
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Ok(Some("second-write".into())),
        });
        app.handle_api(metadata("first-write"));
        for i in 50..75 {
            seed_playlist(&mut app, &format!("other-{i}"));
        }
        app.open(Page::Playlist("edited".into()));
        assert_eq!(
            rows(&app),
            expected,
            "lagging metadata cannot lose the edit"
        );
        app.handle_api(metadata("second-write"));
        for i in 75..100 {
            seed_playlist(&mut app, &format!("other-{i}"));
        }
        assert!(
            !app.playlist_pages.contains_key("edited"),
            "confirmed edits stop protecting an old page"
        );
        assert!(app.playlist_pages.len() <= 12);
    }

    #[test]
    fn transferring_back_takes_the_connect_session_without_replaying_a_snapshot() {
        for playing in [false, true] {
            for snapshot in ["fresh", "stale", "missing"] {
                let mut app = test_app(&format!("transfer-back-{playing}-{snapshot}"));
                app.local_ready = true;
                app.local_device_id = Some("this-computer".into());
                app.local = LocalState {
                    connected: true,
                    playback: Playback::Stopped,
                    track: Some(crate::player::LocalTrack {
                        uri: "spotify:track:bellaire".into(),
                        ..Default::default()
                    }),
                    track_sequence: 3,
                    ..Default::default()
                };
                app.selected_device = Some("phone".into());
                app.remote = Some(RemoteSnapshot {
                    state: PlaybackState {
                        is_playing: playing,
                        progress_ms: Some(65_000),
                        item: Some(queued_song("spotify:track:metallica")),
                        device: Some(crate::api::models::Device {
                            id: Some("phone".into()),
                            is_active: true,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    received_at: Instant::now(),
                });
                app.on_now_playing_changed();
                let upcoming = [
                    "spotify:track:metallica",
                    "spotify:track:extra",
                    "spotify:track:context",
                ];
                app.queue = loaded_queue("spotify:track:metallica", &upcoming);
                app.manual_queue = upcoming[..2].iter().map(|uri| uri.to_string()).collect();
                // Old local restoration data must not be appended to the transfer.
                app.resume_track = Some("spotify:track:metallica".into());
                app.resume_queue = vec!["spotify:track:old-queue".into()];
                app.local_list = Some(vec!["spotify:track:bellaire".into()]);
                match snapshot {
                    "stale" => {
                        app.remote.as_mut().unwrap().received_at = Instant::now() - REMOTE_FRESH
                    }
                    "missing" => app.remote = None,
                    _ => {}
                }
                app.backend.take_player_commands();
                app.transfer("this-computer".into());
                assert_eq!(
                    app.backend.take_player_commands(),
                    [PlayerCommand::Transfer]
                );
                assert!(app.local_list.is_none());
                assert!(app.resume_queue.is_empty());
                assert_eq!(queue_uris(&app).1, upcoming);

                // Loading arrives before TrackChanged and still carries the
                // old local song. Keep showing the remote song through it.
                let loading = LocalState {
                    playback: Playback::Loading,
                    ..app.local.clone()
                };
                app.handle_local(loading);
                if snapshot == "fresh" {
                    assert_eq!(app.now_playing().unwrap().uri, "spotify:track:metallica");
                }
                assert_eq!(queue_uris(&app).1, upcoming);

                let transferred = LocalState {
                    connected: true,
                    playback: if playing {
                        Playback::Playing
                    } else {
                        Playback::Paused
                    },
                    track: Some(crate::player::LocalTrack {
                        uri: "spotify:track:metallica".into(),
                        duration_ms: 300_000,
                        ..Default::default()
                    }),
                    position_ms: 65_000,
                    track_sequence: 4,
                    ..Default::default()
                };
                app.handle_local(transferred.clone());
                let now = app.now_playing().unwrap();
                assert_eq!(now.uri, "spotify:track:metallica");
                assert_eq!(now.position_ms, 65_000);
                assert_eq!(now.playing, playing);
                assert_eq!(app.target(), Target::Local);
                assert_eq!(queue_uris(&app).1, upcoming);
                assert_eq!(
                    app.manual_queue.len(),
                    2,
                    "handoff does not consume a queued copy"
                );
                assert!(app.backend.take_queue_requests().is_empty());
                // Selecting this computer again must not suppress the next advance.
                app.transfer("this-computer".into());
                assert!(app.backend.take_player_commands().is_empty());
                app.handle_local(LocalState {
                    track_sequence: 5,
                    ..transferred
                });
                assert_eq!(queue_uris(&app).1, upcoming[1..]);
                assert_eq!(app.manual_queue, ["spotify:track:extra"]);
                app.backend.shutdown();
            }
        }
    }

    #[test]
    fn a_frame_snapshot_does_not_freeze_local_after_remote_handoff() {
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:local".into(),
            title: "Local".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.refresh_frame_now();
        assert_eq!(app.now_playing().expect("local").uri, "spotify:track:local");
        app.local.track = None;
        app.local.playback = Playback::Stopped;
        app.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                is_playing: true,
                item: Some(PlayableItem::Track(Track {
                    id: Some("remote".into()),
                    uri: "spotify:track:remote".into(),
                    name: "Remote".into(),
                    ..Track::default()
                })),
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        assert_eq!(
            app.now_playing().expect("stale snapshot").uri,
            "spotify:track:local"
        );
        app.refresh_frame_now();
        assert_eq!(
            app.now_playing().expect("handoff").uri,
            "spotify:track:remote"
        );
    }

    #[test]
    fn going_back_refreshes_page_recency() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        for i in 0..12 {
            seed_playlist(&mut app, &format!("pl{i}"));
        }
        for _ in 0..11 {
            app.apply(Action::Back, &ctx);
        }
        assert_eq!(app.page(), &Page::Playlist("pl0".into()));
        seed_playlist(&mut app, "pl12");
        assert!(
            app.playlist_pages.contains_key("pl0"),
            "a playlist revisited through Back must not be the oldest"
        );
        assert!(
            !app.playlist_pages.contains_key("pl11"),
            "the page left behind by history is the one that goes"
        );
    }

    #[test]
    fn page_cache_trims_data_that_arrives_after_navigation() {
        let mut app = headless_app();
        for i in 0..12 {
            seed_playlist(&mut app, &format!("pl{i}"));
        }
        app.open(Page::Playlist("current".into()));
        app.playlist_pages
            .insert("current".into(), PlaylistPage::default());
        app.playlist_pages
            .insert("late".into(), PlaylistPage::default());
        app.evict_stale_pages();
        assert!(app.playlist_pages.len() <= 12);
        assert!(app.playlist_pages.contains_key("current"));
        assert!(
            !app.playlist_pages.contains_key("late"),
            "a page that arrived after browsing still counts toward the cap"
        );
    }

    /// eframe runs only the logic of a hidden, minimised or occluded window,
    /// so the logic pass alone must ask for the next one. Otherwise a window
    /// on another workspace sleeps until something else wakes it, and stops
    /// polling the playing device and updating the media controls.
    #[test]
    fn logic_pass_alone_schedules_the_next_one_while_playing() {
        let mut app = headless_app();
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        assert!(app.now_playing().is_some_and(|now| now.playing));

        let ctx = egui::Context::default();
        let delays = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&delays);
        ctx.set_request_repaint_callback(move |info| {
            seen.lock().unwrap().push(info.delay);
        });
        // The first pass settles start-up work (zoom, fonts) that asks for an
        // immediate pass of its own; the steady state is what matters.
        for _ in 0..3 {
            delays.lock().unwrap().clear();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.background_frame(ui.ctx());
            });
            output.textures_delta.clear();
        }

        let soonest = delays.lock().unwrap().iter().copied().min();
        assert!(
            soonest
                .is_some_and(|delay| delay > Duration::ZERO && delay <= Duration::from_millis(250)),
            "a hidden window's logic must wake again within 250 ms while playing, \
             without spinning; asked for {soonest:?}"
        );
    }

    #[test]
    fn local_idle_repaint_is_twenty_seconds_not_four() {
        let mut app = headless_app();
        assert_eq!(app.connected_repaint_interval(), REMOTE_POLL_ACTIVE);
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:a".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Paused;
        assert_eq!(
            app.connected_repaint_interval(),
            REMOTE_POLL_IDLE,
            "paused local UI wait is 20s (already the API interval); 4s was only a tighter wake-up"
        );
    }

    #[test]
    fn reset_data_drops_table_row_caches() {
        let mut app = headless_app();
        crate::ui::collection::cached_table_items(&mut app, Page::LikedSongs, 0, 0, 0, || {
            vec![(
                PlayableItem::Track(Track {
                    name: "Hold".into(),
                    uri: "spotify:track:hold".into(),
                    ..Track::default()
                }),
                None,
                None,
            )]
        });
        assert!(!app.table_rows.is_empty());
        app.reset_data();
        assert!(app.table_rows.is_empty());
        crate::ui::collection::cached_table_items(&mut app, Page::LikedSongs, 0, 0, 0, || {
            vec![(
                PlayableItem::Track(Track {
                    name: "Fresh".into(),
                    uri: "spotify:track:fresh".into(),
                    ..Track::default()
                }),
                None,
                None,
            )]
        });
        let name = app.table_rows[&Page::LikedSongs].items[0].0.name();
        assert_eq!(
            name, "Fresh",
            "reused revision 0 must not keep the old rows"
        );
    }

    #[test]
    fn track_cache_is_an_lru_of_eight_hundred() {
        let mut app = headless_app();
        for i in 0..900 {
            store_track(&mut app, &format!("t{i}"));
        }
        for i in 100..900 {
            let _ = app.read_cached_track(&format!("t{i}"));
        }
        app.evict_stale_pages();
        assert_eq!(app.track_cache.len(), 800);
        assert!(
            app.track_cache.contains_key("t899"),
            "a cache hit must keep that track"
        );
        assert!(!app.track_cache.contains_key("t0"));
    }

    #[test]
    fn reset_data_clears_cache_recency() {
        let mut app = headless_app();
        seed_playlist(&mut app, "pl0");
        store_track(&mut app, "t0");
        let _ = app.read_cached_track("t0");
        assert!(!app.page_used.is_empty());
        assert!(!app.track_used.is_empty());
        app.reset_data();
        assert!(app.page_used.is_empty());
        assert!(app.track_used.is_empty());
    }

    #[test]
    fn stepping_resume_keeps_a_cached_preview() {
        use crate::api::models::{PlayableItem, PlaylistItem, Track};
        use crate::model::PagedList;
        let row = |uri: &str| PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                id: Some(uri.rsplit(':').next().unwrap().into()),
                uri: uri.into(),
                name: uri.into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let mut app = headless_app();
        let ctx = egui::Context::default();
        store_track(&mut app, "one");
        store_track(&mut app, "two");
        for i in 0..798 {
            store_track(&mut app, &format!("old{i}"));
            let _ = app.read_cached_track(&format!("old{i}"));
        }
        app.playlist_pages.insert(
            "pl1".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![row("spotify:track:one"), row("spotify:track:two")],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.resume_context = Some("spotify:playlist:pl1".into());
        app.resume_track = Some("spotify:track:one".into());
        app.apply(Action::Next, &ctx);
        assert_eq!(app.resume_track.as_deref(), Some("spotify:track:two"));
        store_track(&mut app, "overflow");
        let _ = app.read_cached_track("overflow");
        app.evict_stale_pages();
        assert!(
            app.track_cache.contains_key("two"),
            "a cache hit while stepping resume must keep the song just previewed"
        );
        assert!(
            !app.track_cache.contains_key("one"),
            "the song left behind can go"
        );
    }

    #[test]
    fn requesting_a_cached_resume_track_keeps_it() {
        let mut app = headless_app();
        store_track(&mut app, "resume");
        store_track(&mut app, "decoy");
        for i in 0..798 {
            store_track(&mut app, &format!("old{i}"));
            let _ = app.read_cached_track(&format!("old{i}"));
        }
        app.resume_track = Some("spotify:track:resume".into());
        app.request_resume_track();
        store_track(&mut app, "overflow");
        let _ = app.read_cached_track("overflow");
        app.evict_stale_pages();
        assert!(
            app.track_cache.contains_key("resume"),
            "a cache hit on the resume URI must keep that track"
        );
        assert!(
            !app.track_cache.contains_key("decoy"),
            "an untouched cached track can go"
        );
    }

    #[test]
    fn evict_stale_pages_drops_table_row_copies() {
        let mut app = headless_app();
        for i in 0..12 {
            seed_playlist(&mut app, &format!("pl{i}"));
        }
        crate::ui::collection::cached_table_items(
            &mut app,
            Page::Playlist("pl0".into()),
            0,
            0,
            0,
            || {
                vec![(
                    PlayableItem::Track(Track {
                        name: "Old".into(),
                        uri: "spotify:track:old".into(),
                        ..Track::default()
                    }),
                    None,
                    None,
                )]
            },
        );
        assert!(app.table_rows.contains_key(&Page::Playlist("pl0".into())));
        seed_playlist(&mut app, "pl12");
        assert!(
            !app.playlist_pages.contains_key("pl0"),
            "the oldest playlist page is dropped"
        );
        assert!(
            !app.table_rows.contains_key(&Page::Playlist("pl0".into())),
            "its table-row copy must go with it"
        );
    }

    #[test]
    fn update_checks_leave_the_popup_closed_until_requested() {
        for manual in [false, true] {
            let mut app = headless_app();
            app.handle_backend_events(vec![Event::UpdateChecked {
                manual,
                result: Ok(Some(crate::updates::Release {
                    version: "1.2.3".into(),
                    url: "https://github.com/crmne/spotifast/releases/tag/v1.2.3".into(),
                })),
            }]);
            let ctx = egui::Context::default();
            app.apply_actions(&ctx);
            assert!(app.update.is_some());
            assert!(!app.show_update);
            app.apply(Action::ShowUpdate, &ctx);
            assert!(app.show_update);
        }
    }

    #[test]
    fn a_manual_update_check_reports_its_result() {
        let mut app = headless_app();
        app.update = Some(crate::updates::Release {
            version: "1.2.3".into(),
            url: "https://github.com/crmne/spotifast/releases/tag/v1.2.3".into(),
        });
        app.update_checking = true;
        app.handle_backend_events(vec![Event::UpdateChecked {
            manual: true,
            result: Ok(None),
        }]);

        assert!(!app.update_checking);
        assert_eq!(app.update, None);
        assert_eq!(
            app.toasts.last().map(|toast| toast.message.as_str()),
            Some("Spotifast is up to date")
        );

        app.toasts.clear();
        app.update_checking = true;
        app.handle_backend_events(vec![Event::UpdateChecked {
            manual: true,
            result: Err("GitHub is unavailable".into()),
        }]);

        assert!(!app.update_checking);
        assert_eq!(
            app.toasts.last().map(|toast| toast.message.as_str()),
            Some("Couldn't check for updates: GitHub is unavailable")
        );
        assert_eq!(
            app.toasts.last().map(|toast| &toast.kind),
            Some(&ToastKind::Error)
        );
    }

    #[test]
    fn the_daily_update_check_only_announces_a_new_release() {
        let mut app = headless_app();
        app.update_checking = true;
        app.handle_backend_events(vec![Event::UpdateChecked {
            manual: false,
            result: Ok(None),
        }]);
        assert!(
            app.toasts.is_empty(),
            "the current release needs no daily toast"
        );

        app.update_checking = true;
        app.handle_backend_events(vec![Event::UpdateChecked {
            manual: false,
            result: Ok(Some(crate::updates::Release {
                version: "1.2.3".into(),
                url: "https://github.com/crmne/spotifast/releases/tag/v1.2.3".into(),
            })),
        }]);

        assert_eq!(
            app.update.as_ref().map(|release| release.version.as_str()),
            Some("1.2.3")
        );
        assert_eq!(
            app.toasts.last().map(|toast| toast.message.as_str()),
            Some("Spotifast 1.2.3 is available")
        );
    }

    fn cached_playlist_row(uri: &str) -> crate::api::models::PlaylistItem {
        crate::api::models::PlaylistItem {
            item: Some(PlayableItem::Track(Track {
                uri: uri.to_string(),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    #[test]
    fn playlist_row_cache_extends_without_cloning_old_rows_or_losing_null_slots() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "rows".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:first"),
                        Default::default(),
                    ],
                    total: Some(100),
                    next_offset: Some(2),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let cached = |app: &mut App| {
            let page = app.playlist_pages.remove("rows").unwrap();
            let result = crate::ui::collection::playlist_cached_table_items(
                app,
                "rows",
                page.generation,
                &page.items,
                None,
                "",
            );
            app.playlist_pages.insert("rows".into(), page);
            result
        };
        let (rows, positions, _) = cached(&mut app);
        assert_eq!(positions.as_slice(), [0]);
        let old_rows = Arc::as_ptr(&rows);
        let old_positions = Arc::as_ptr(&positions);
        drop(rows);
        drop(positions);

        app.handle_api(ApiResponse::PlaylistItems {
            id: "rows".into(),
            offset: 2,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![
                    cached_playlist_row("spotify:track:second"),
                    Default::default(),
                ],
                total: 100,
                limit: 2,
                offset: 2,
                next: Some("next".into()),
            }),
        });
        let (rows, positions, _) = cached(&mut app);
        assert_eq!(Arc::as_ptr(&rows), old_rows, "old rows were rebuilt");
        assert_eq!(Arc::as_ptr(&positions), old_positions);
        assert_eq!(positions.as_slice(), [0, 2]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].0.uri(), "spotify:track:second");
    }

    #[test]
    fn playlist_row_cache_rebuilds_for_edits_refreshes_and_window_jumps() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "rows".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(100),
                    next_offset: Some(1),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let cached = |app: &mut App, owner_name: &str| {
            let page = app.playlist_pages.remove("rows").unwrap();
            let result = crate::ui::collection::playlist_cached_table_items(
                app,
                "rows",
                page.generation,
                &page.items,
                None,
                owner_name,
            );
            app.playlist_pages.insert("rows".into(), page);
            result
        };
        let (rows, _, _) = cached(&mut app, "");
        let first = Arc::as_ptr(&rows);
        drop(rows);

        let (rows, _, _) = cached(&mut app, "renamed owner");
        assert_ne!(Arc::as_ptr(&rows), first);
        let first = Arc::as_ptr(&rows);
        drop(rows);

        let page = app.playlist_pages.get_mut("rows").unwrap();
        page.items
            .items
            .insert(0, cached_playlist_row("spotify:track:added"));
        page.items.revision += 1;
        let (rows, positions, _) = cached(&mut app, "renamed owner");
        assert_ne!(Arc::as_ptr(&rows), first);
        assert_eq!(rows[0].0.uri(), "spotify:track:added");
        assert_eq!(positions.as_slice(), [0, 1]);
        drop(rows);
        drop(positions);

        app.handle_api(ApiResponse::PlaylistItems {
            id: "rows".into(),
            offset: 0,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:refreshed")],
                total: 100,
                limit: 1,
                offset: 0,
                next: Some("next".into()),
            }),
        });
        let (rows, positions, _) = cached(&mut app, "renamed owner");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.uri(), "spotify:track:refreshed");
        assert_eq!(positions.as_slice(), [0]);
        drop(rows);
        drop(positions);

        app.playlist_pages
            .get_mut("rows")
            .unwrap()
            .items
            .window_request = Some(50);
        app.handle_api(ApiResponse::PlaylistItems {
            id: "rows".into(),
            offset: 50,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:window")],
                total: 100,
                limit: 1,
                offset: 50,
                next: Some("next".into()),
            }),
        });
        let (rows, positions, _) = cached(&mut app, "renamed owner");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.uri(), "spotify:track:window");
        assert_eq!(positions.as_slice(), [0]);
    }

    #[test]
    fn changing_playlist_items_keeps_the_library_visible() {
        let mut app = headless_app();
        app.library.playlists = Loadable::Loaded(vec![
            Playlist {
                id: "changed".into(),
                snapshot_id: Some("old".into()),
                ..Default::default()
            },
            Playlist {
                id: "untouched".into(),
                snapshot_id: Some("same".into()),
                ..Default::default()
            },
        ]);
        app.playlist_pages.insert(
            "changed".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "changed".into(),
                    snapshot_id: Some("old".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:held")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "changed".into(),
            message: "Added to Playlist".into(),
            result: Ok(Some("new".into())),
        });

        let playlists = app
            .library
            .playlists
            .get()
            .expect("the loaded library stays on screen");
        assert_eq!(playlists.len(), 2);
        assert_eq!(playlists[0].snapshot_id.as_deref(), Some("new"));
        assert_eq!(playlists[1].snapshot_id.as_deref(), Some("same"));
        let open = app.playlist_pages["changed"]
            .playlist
            .get()
            .expect("the playlist page");
        assert_eq!(open.snapshot_id.as_deref(), Some("new"));
        assert_eq!(
            app.playlist_pages["changed"].items.items.len(),
            1,
            "a successful write does not throw away the loaded playlist"
        );
    }

    #[test]
    fn a_known_duplicate_opens_the_dialog_without_a_spotify_scan() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let honey = cached_playlist_row("spotify:track:honey");
        app.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![honey.clone()],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        app.apply(
            Action::AddToPlaylist {
                playlist_id: "best".into(),
                playlist_name: "The best music ever".into(),
                items: vec![honey.playable().unwrap().clone()],
            },
            &egui::Context::default(),
        );

        assert!(!app.playlist_busy, "there is no duplicate scan to wait for");
        assert!(matches!(
            app.dialog,
            Some(Dialog::ConfirmPlaylistDuplicates {
                ref duplicate_uris,
                ..
            }) if duplicate_uris == &["spotify:track:honey"]
        ));
    }

    #[test]
    fn adding_to_a_loaded_playlist_is_immediate_and_keeps_its_cache() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.library.playlists = Loadable::Loaded(vec![Playlist {
            id: "best".into(),
            snapshot_id: Some("old".into()),
            items_count: Some(TrackCount { total: 1 }),
            ..Default::default()
        }]);
        app.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                generation: 7,
                items_generation: 7,
                playlist: Loadable::Loaded(Playlist {
                    id: "best".into(),
                    snapshot_id: Some("old".into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:one")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                cache_saved_through: Some(1),
                ..Default::default()
            },
        );
        let added = cached_playlist_row("spotify:track:honey")
            .playable()
            .unwrap()
            .clone();

        app.apply(
            Action::ConfirmAddToPlaylist {
                position: None,
                playlist_id: "best".into(),
                playlist_name: "The best music ever".into(),
                items: vec![added],
            },
            &egui::Context::default(),
        );

        let page = &app.playlist_pages["best"];
        assert_eq!(page.items.items.len(), 2, "the new row is shown at once");
        assert_eq!(page.items.total, Some(2));
        assert_eq!(page.items.next_offset, None);
        assert_eq!(
            page.items.items[0].playable().map(PlayableItem::uri),
            Some("spotify:track:one"),
            "the held rows are not discarded"
        );
        assert!(page.local_additions.contains("spotify:track:honey"));
        assert_eq!(page.cache_saved_through, None);

        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "best".into(),
            message: "Added to The best music ever".into(),
            result: Ok(Some("new".into())),
        });

        let page = &app.playlist_pages["best"];
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(
            page.playlist
                .get()
                .and_then(|playlist| playlist.snapshot_id.as_deref()),
            Some("new")
        );
        assert_eq!(page.cache_saved_through, None);
        assert!(page.cache_write_pending.is_some());
        let generation = page.generation;
        app.receive_playlist_cache_stored("alice", "best", generation, "new", true);
        assert_eq!(app.playlist_pages["best"].cache_saved_through, Some(2));
        assert_eq!(app.library.playlists.get().unwrap()[0].track_total(), 2);
    }

    #[test]
    fn confirmed_playlist_edit_waits_for_old_checkpoint_then_saves_new_snapshot() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                generation: 7,
                items_generation: 7,
                playlist: Loadable::Loaded(Playlist {
                    id: "best".into(),
                    snapshot_id: Some("old".into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:one")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                ..Default::default()
            },
        );
        app.checkpoint_playlist_cache("best");
        let old_generation = app.playlist_pages["best"].generation;
        assert_eq!(
            app.playlist_pages["best"]
                .cache_write_pending
                .as_ref()
                .map(|write| write.snapshot.as_str()),
            Some("old")
        );

        let added = cached_playlist_row("spotify:track:honey")
            .playable()
            .unwrap()
            .clone();
        app.apply(
            Action::ConfirmAddToPlaylist {
                position: None,
                playlist_id: "best".into(),
                playlist_name: "The best music ever".into(),
                items: vec![added],
            },
            &egui::Context::default(),
        );
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "best".into(),
            message: String::new(),
            result: Ok(Some("new".into())),
        });
        assert_eq!(
            app.playlist_pages["best"]
                .cache_write_pending
                .as_ref()
                .map(|write| write.snapshot.as_str()),
            Some("old"),
            "the confirmed edit must wait for the older disk write"
        );

        app.receive_playlist_cache_stored("alice", "best", old_generation, "old", true);
        let page = &app.playlist_pages["best"];
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(page.cache_saved_through, None);
        assert_eq!(
            page.cache_write_pending
                .as_ref()
                .map(|write| write.snapshot.as_str()),
            Some("new"),
            "the stale completion must request the edited snapshot"
        );
        let new_generation = page.generation;
        app.receive_playlist_cache_stored("alice", "best", new_generation, "new", true);
        assert_eq!(app.playlist_pages["best"].cache_saved_through, Some(2));
    }

    #[test]
    fn positioned_playlist_additions_keep_partial_pages_and_ignore_old_reads() {
        for (base, position, scan_fails) in [
            (0, 1, false),
            (100, 101, false),
            (100, 102, true),
            (100, 50, false),
        ] {
            let mut app = headless_app();
            app.backend.set_offline(true);
            app.playlist_pages.insert(
                "target".into(),
                PlaylistPage {
                    generation: 9,
                    items_generation: 9,
                    items: PagedList {
                        base_offset: base,
                        items: vec![
                            cached_playlist_row("spotify:track:a"),
                            cached_playlist_row("spotify:track:b"),
                        ],
                        total: Some(200),
                        next_offset: Some(base + 2),
                        loaded_once: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            let item = cached_playlist_row("spotify:track:new")
                .playable()
                .unwrap()
                .clone();
            app.request_playlist_add("target".into(), "Target".into(), vec![item], Some(position));
            let mut sent = app.backend.take_playlist_add_requests();
            assert_eq!(sent.len(), 1);
            let ApiRequest::CheckPlaylistDuplicates {
                playlist_id,
                playlist_name,
                items,
                position: requested,
            } = sent.pop().unwrap()
            else {
                panic!("expected duplicate check");
            };
            assert_eq!(requested, Some(position));
            app.handle_api(ApiResponse::PlaylistDuplicatesChecked {
                playlist_id,
                playlist_name,
                items,
                position: requested,
                result: if scan_fails {
                    Err(crate::api::ApiError::Network("offline".into()))
                } else {
                    Ok(vec![])
                },
            });
            let page = &app.playlist_pages["target"];
            assert_eq!(page.items.total, Some(201));
            assert_eq!(page.items.next_offset, Some(base + 3));
            assert_eq!(
                page.items.base_offset,
                if position < base { base + 1 } else { base }
            );
            if position >= base {
                assert_eq!(page.items.items.len(), 3);
                assert_eq!(
                    page.items.items[(position - base) as usize]
                        .playable()
                        .unwrap()
                        .uri(),
                    "spotify:track:new"
                );
            } else {
                assert_eq!(page.items.items.len(), 2);
            }
            let held: Vec<_> = page
                .items
                .items
                .iter()
                .map(|row| row.playable().unwrap().uri().to_string())
                .collect();
            let sent = app.backend.take_playlist_add_requests();
            assert!(
                matches!(sent.as_slice(), [ApiRequest::AddToPlaylist { position: Some(at), .. }] if *at == position)
            );
            app.handle_api(ApiResponse::PlaylistItems {
                id: "target".into(),
                offset: base,
                generation: 9,
                result: Ok(crate::api::models::Page {
                    items: vec![cached_playlist_row("spotify:track:old")],
                    total: 200,
                    ..Default::default()
                }),
            });
            assert_eq!(
                app.playlist_pages["target"]
                    .items
                    .items
                    .iter()
                    .map(|row| row.playable().unwrap().uri().to_string())
                    .collect::<Vec<_>>(),
                held
            );
            app.handle_api(ApiResponse::PlaylistItemsChanged {
                id: "target".into(),
                message: String::new(),
                result: Ok(Some("new-snapshot".into())),
            });
            assert_eq!(
                app.playlist_pages["target"]
                    .items
                    .items
                    .iter()
                    .map(|row| row.playable().unwrap().uri().to_string())
                    .collect::<Vec<_>>(),
                held
            );
        }
    }

    #[test]
    fn duplicate_confirmation_keeps_the_requested_insertion_position() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "target".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:a"),
                        cached_playlist_row("spotify:track:b"),
                    ],
                    total: Some(2),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let item = cached_playlist_row("spotify:track:b")
            .playable()
            .unwrap()
            .clone();
        app.request_playlist_add(
            "target".into(),
            "Target".into(),
            vec![item.clone()],
            Some(0),
        );
        assert!(app.backend.take_playlist_add_requests().is_empty());
        app.apply(Action::CloseDialog, &egui::Context::default());
        assert_eq!(app.playlist_pages["target"].items.items.len(), 2);
        app.request_playlist_add("target".into(), "Target".into(), vec![item], Some(0));
        let Some(Dialog::ConfirmPlaylistDuplicates {
            playlist_id,
            playlist_name,
            items,
            position,
            ..
        }) = app.dialog.take()
        else {
            panic!("duplicate confirmation");
        };
        assert_eq!(position, Some(0));
        app.apply(
            Action::ConfirmAddToPlaylist {
                playlist_id,
                playlist_name,
                items,
                position,
            },
            &egui::Context::default(),
        );
        assert_eq!(
            app.playlist_pages["target"]
                .items
                .items
                .iter()
                .map(|row| row.playable().unwrap().uri())
                .collect::<Vec<_>>(),
            ["spotify:track:b", "spotify:track:a", "spotify:track:b"]
        );
        assert!(matches!(
            app.backend.take_playlist_add_requests().as_slice(),
            [ApiRequest::AddToPlaylist {
                position: Some(0),
                ..
            }]
        ));
    }

    /// Copying picked songs puts their links on the clipboard, one per
    /// line. Pasting links into an editable playlist appends the songs:
    /// ones this app has shown get their rows at once, and the rest wait
    /// only for Spotify to name them. Links that are not songs are left out.
    #[test]
    fn copied_and_pasted_song_links_append_to_an_editable_playlist() {
        // #given an editable playlist with one song
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "target".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "target".into(),
                    name: "Target".into(),
                    collaborative: true,
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:old")],
                    total: Some(1),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let track = |id: &str| Track {
            id: Some(id.into()),
            uri: format!("spotify:track:{id}"),
            name: format!("Song {id}"),
            ..Default::default()
        };
        let song = |id: &str| PlayableItem::Track(track(id));
        let rows = |app: &App| {
            app.playlist_pages["target"]
                .items
                .items
                .iter()
                .map(|row| row.playable().unwrap().name().to_string())
                .collect::<Vec<_>>()
        };
        let ctx = egui::Context::default();

        // #when two songs are copied
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.apply(Action::CopySongs(vec![song("aaa"), song("bbb")]), ui.ctx());
        });
        output.textures_delta.clear();

        // #then their links are on the clipboard, one per line
        assert!(
            output
                .platform_output
                .commands
                .contains(&egui::OutputCommand::CopyText(
                    [
                        "https://open.spotify.com/track/aaa",
                        "https://open.spotify.com/track/bbb"
                    ]
                    .join(if cfg!(windows) { "\r\n" } else { "\n" })
                ))
        );

        // #when those links are pasted into the playlist
        app.apply(
            Action::PasteSongs {
                playlist_id: "target".into(),
                text: "https://open.spotify.com/track/aaa?si=x\r\nspotify:track:bbb\n".into(),
            },
            &ctx,
        );

        // #then their rows appear at once, named, and Spotify is asked to add them
        assert_eq!(rows(&app), ["", "Song aaa", "Song bbb"]);
        assert!(matches!(
            app.backend.take_playlist_add_requests().as_slice(),
            [ApiRequest::AddToPlaylist { uris, position: None, .. }]
                if uris == &["spotify:track:aaa", "spotify:track:bbb"]
        ));
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "target".into(),
            message: String::new(),
            result: Ok(Some("after-paste".into())),
        });

        // #when links to songs this app has not seen are pasted, with an album
        app.apply(
            Action::PasteSongs {
                playlist_id: "target".into(),
                text: "spotify:track:ccc https://open.spotify.com/album/xyz, spotify:track:ddd"
                    .into(),
            },
            &ctx,
        );

        // #then Spotify is asked about the songs, and nothing is added yet
        assert!(app.track_requests.contains("ccc"));
        assert!(app.track_requests.contains("ddd"));
        assert_eq!(rows(&app).len(), 3);
        assert!(app.backend.take_playlist_add_requests().is_empty());

        // #when Spotify names one song and has no other
        app.handle_api(ApiResponse::Track {
            id: "ccc".into(),
            result: Ok(track("ccc")),
        });
        assert!(app.backend.take_playlist_add_requests().is_empty());
        app.handle_api(ApiResponse::Track {
            id: "ddd".into(),
            result: Err(crate::api::client::ApiError::Status {
                status: 404,
                message: "not found".into(),
            }),
        });

        // #then the named song is appended and the other is reported
        assert_eq!(rows(&app), ["", "Song aaa", "Song bbb", "Song ccc"]);
        assert!(matches!(
            app.backend.take_playlist_add_requests().as_slice(),
            [ApiRequest::AddToPlaylist { uris, .. }] if uris == &["spotify:track:ccc"]
        ));
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message == "1 pasted link could not be added")
        );
        assert!(app.pending_pastes.is_empty());

        // #when the clipboard holds no song links
        app.apply(
            Action::PasteSongs {
                playlist_id: "target".into(),
                text: "just some words".into(),
            },
            &ctx,
        );

        // #then nothing is added, and the user is told why
        assert!(app.backend.take_playlist_add_requests().is_empty());
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message == "The clipboard has no Spotify song links")
        );

        // #when the playlist cannot be edited
        if let Loadable::Loaded(playlist) =
            &mut app.playlist_pages.get_mut("target").unwrap().playlist
        {
            playlist.collaborative = false;
        }
        app.apply(
            Action::PasteSongs {
                playlist_id: "target".into(),
                text: "spotify:track:aaa".into(),
            },
            &ctx,
        );

        // #then nothing is added to it
        assert_eq!(rows(&app).len(), 4);
        assert!(app.backend.take_playlist_add_requests().is_empty());
    }

    #[test]
    fn appending_before_any_playlist_page_is_loaded_keeps_the_first_page_offset() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages
            .insert("target".into(), PlaylistPage::default());
        app.add_to_playlist_now(
            "target".into(),
            "Target".into(),
            vec![
                cached_playlist_row("spotify:track:new")
                    .playable()
                    .unwrap()
                    .clone(),
            ],
            None,
        );
        assert_eq!(app.playlist_pages["target"].items.next_offset, Some(0));
        assert!(matches!(
            app.backend.take_playlist_add_requests().as_slice(),
            [ApiRequest::AddToPlaylist { position: None, .. }]
        ));
    }

    #[test]
    fn playlist_refresh_preserves_the_view_and_failed_reads_keep_its_rows() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let page = Page::Playlist("best".into());
        app.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(1),
                    loaded_once: true,
                    ..Default::default()
                },
                filter: "first".into(),
                ..Default::default()
            },
        );
        let sort = TableSort {
            column: SortColumn::Title,
            ascending: false,
        };
        app.table_sorts.insert(page.clone(), sort);
        app.pick_row(&page, "best", 0, RowPick::Only, 1);
        app.reload(page.clone());
        let generation = app.playlist_pages["best"].generation;
        assert_eq!(
            app.backend.take_playlist_item_requests(),
            [("best".into(), 0, generation)]
        );
        assert_eq!(app.table_sorts[&page], sort);
        assert_eq!(picked(&app, &page), [0]);
        assert_eq!(app.playlist_pages["best"].filter, "first");
        app.handle_api(ApiResponse::PlaylistItems {
            id: "best".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::Network("offline".into())),
        });
        let playlist = &app.playlist_pages["best"];
        assert_eq!(playlist.items.items.len(), 1);
        assert!(!playlist.items.loading);
        assert!(playlist.items.error.is_some());
        app.reload(page);
        assert!(app.playlist_pages["best"].items.loading);
        assert!(app.playlist_pages["best"].items.error.is_none());
        assert_eq!(app.backend.take_playlist_item_requests().len(), 1);
    }

    #[test]
    fn failed_playlist_write_finishes_a_waiting_refresh_with_the_recovery_read() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "edited".into(),
            PlaylistPage {
                pending_writes: 1,
                ..Default::default()
            },
        );
        app.reload(Page::Playlist("edited".into()));
        assert!(app.backend.take_playlist_item_requests().is_empty());
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Err(crate::api::ApiError::Network("offline".into())),
        });
        let generation = app.playlist_pages["edited"].generation;
        assert_eq!(
            app.backend.take_playlist_item_requests(),
            [("edited".into(), 0, generation)]
        );
        app.handle_api(ApiResponse::Playlist {
            id: "edited".into(),
            generation,
            result: Ok(Playlist::default()),
        });
        assert!(
            app.backend.take_playlist_item_requests().is_empty(),
            "the recovery read already handles the waiting refresh"
        );
        app.handle_api(ApiResponse::PlaylistItems {
            id: "edited".into(),
            offset: 0,
            generation,
            result: Ok(crate::api::models::Page::default()),
        });
        assert!(!app.playlist_pages["edited"].items.loading);
    }

    #[test]
    fn playlist_refresh_waits_for_all_writes_and_their_confirmed_snapshot() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "edited".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "edited".into(),
                    snapshot_id: Some("old".into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(1),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        for uri in ["spotify:track:second", "spotify:track:third"] {
            app.add_to_playlist_now(
                "edited".into(),
                "Edited".into(),
                vec![cached_playlist_row(uri).playable().unwrap().clone()],
                None,
            );
        }
        let generation = app.playlist_pages["edited"].generation;
        app.reload(Page::Playlist("edited".into()));
        assert!(
            app.backend.take_playlist_item_requests().is_empty(),
            "refresh must not read the rows before the pending writes finish"
        );
        let metadata = |snapshot: &str, total| ApiResponse::Playlist {
            id: "edited".into(),
            generation,
            result: Ok(Playlist {
                id: "edited".into(),
                snapshot_id: Some(snapshot.into()),
                items_count: Some(TrackCount { total }),
                ..Default::default()
            }),
        };
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Ok(Some("first-write".into())),
        });
        app.handle_api(metadata("first-write", 2));
        assert_eq!(app.playlist_pages["edited"].items.total, Some(3));
        assert!(app.backend.take_playlist_item_requests().is_empty());
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "edited".into(),
            message: String::new(),
            result: Ok(Some("second-write".into())),
        });
        app.handle_api(metadata("first-write", 2));
        assert_eq!(app.playlist_pages["edited"].items.total, Some(3));
        assert!(app.backend.take_playlist_item_requests().is_empty());
        assert!(app.playlist_pages["edited"].items.loading);

        app.handle_api(metadata("second-write", 3));
        let refreshed_generation = app.playlist_pages["edited"].generation;
        assert!(refreshed_generation > generation);
        assert_eq!(
            app.backend.take_playlist_item_requests(),
            [("edited".into(), 0, refreshed_generation)]
        );
        let rows = |generation, uris: &[&str]| ApiResponse::PlaylistItems {
            id: "edited".into(),
            offset: 0,
            generation,
            result: Ok(crate::api::models::Page {
                total: uris.len() as u32,
                items: uris.iter().map(|uri| cached_playlist_row(uri)).collect(),
                ..Default::default()
            }),
        };
        app.handle_api(rows(generation, &["spotify:track:first"]));
        assert_eq!(app.playlist_pages["edited"].items.items.len(), 3);
        assert!(app.playlist_pages["edited"].items.loading);
        let expected = [
            "spotify:track:first",
            "spotify:track:second",
            "spotify:track:third",
            "spotify:track:added-elsewhere",
        ];
        app.handle_api(rows(refreshed_generation, &expected));
        let page = &app.playlist_pages["edited"];
        assert!(!page.items.loading);
        assert_eq!(
            page.items
                .items
                .iter()
                .map(|row| row.playable().unwrap().uri())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn playlist_window_reads_wait_for_an_optimistic_snapshot_to_be_confirmed() {
        for unconfirmed in [false, true] {
            let mut app = headless_app();
            app.backend.set_offline(true);
            app.playlist_pages.insert(
                "edited".into(),
                PlaylistPage {
                    generation: 7,
                    items: PagedList {
                        items: (0..50)
                            .map(|index| {
                                cached_playlist_row(&format!("spotify:track:edited_{index}"))
                            })
                            .collect(),
                        total: Some(1_000),
                        next_offset: Some(50),
                        loaded_once: true,
                        ..Default::default()
                    },
                    optimistic_snapshot: unconfirmed.then(|| "written".into()),
                    ..Default::default()
                },
            );
            app.load_window(Page::Playlist("edited".into()), 750);
            if unconfirmed {
                assert!(
                    app.backend.take_playlist_item_requests().is_empty(),
                    "a distant read would describe the old playlist snapshot"
                );
                let rows = &app.playlist_pages["edited"].items;
                assert_eq!(rows.base_offset, 0);
                assert_eq!(rows.items.len(), 50);
                assert_eq!(
                    rows.items[0].playable().unwrap().uri(),
                    "spotify:track:edited_0"
                );
                assert!(!rows.loading);
            } else {
                assert_eq!(
                    app.backend.take_playlist_item_requests(),
                    [("edited".into(), 750, 7)]
                );
            }
            app.backend.shutdown();
        }
    }

    #[test]
    fn playlist_refresh_can_retry_unconfirmed_edits_without_losing_them() {
        for (network_failure, manual_refresh, window_retry) in [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, false),
            (false, false, true),
            (false, true, true),
            (true, false, true),
            (true, true, true),
        ] {
            let mut app = headless_app();
            app.backend.set_offline(true);
            app.playlist_pages.insert(
                "edited".into(),
                PlaylistPage {
                    playlist: Loadable::Loaded(Playlist {
                        id: "edited".into(),
                        snapshot_id: Some("written".into()),
                        items_count: Some(TrackCount { total: 1 }),
                        ..Default::default()
                    }),
                    items: PagedList {
                        items: vec![cached_playlist_row("spotify:track:new")],
                        total: Some(1),
                        loaded_once: true,
                        ..Default::default()
                    },
                    optimistic_snapshot: Some("written".into()),
                    local_additions: ["spotify:track:new".into()].into(),
                    ..Default::default()
                },
            );
            if manual_refresh {
                app.reload(Page::Playlist("edited".into()));
            }
            assert!(app.backend.take_playlist_item_requests().is_empty());
            let generation = app.playlist_pages["edited"].generation;
            let metadata = |snapshot: &str| ApiResponse::Playlist {
                id: "edited".into(),
                generation,
                result: Ok(Playlist {
                    snapshot_id: Some(snapshot.into()),
                    items_count: Some(TrackCount { total: 1 }),
                    ..Default::default()
                }),
            };
            if network_failure {
                app.handle_api(ApiResponse::Playlist {
                    id: "edited".into(),
                    generation,
                    result: Err(crate::api::ApiError::Network("offline".into())),
                });
            } else {
                for _ in 0..4 {
                    app.handle_api(metadata("old"));
                }
            }
            let page = &app.playlist_pages["edited"];
            assert!(!page.items.loading, "failed refresh must stop spinning");
            assert!(page.items.error.is_some());
            assert_eq!(page.items.items.len(), 1);
            assert!(page.local_additions.contains("spotify:track:new"));
            assert_eq!(page.optimistic_snapshot.as_deref(), Some("written"));
            app.load_more(Page::Playlist("edited".into()));
            assert!(app.backend.take_playlist_item_requests().is_empty());

            let page = Page::Playlist("edited".into());
            app.apply(
                if window_retry {
                    Action::RetryWindow(page)
                } else {
                    Action::Reload(page)
                },
                &egui::Context::default(),
            );
            assert!(app.backend.take_playlist_item_requests().is_empty());
            assert!(app.playlist_pages["edited"].items.loading);
            assert!(app.playlist_pages["edited"].items.error.is_none());
            app.handle_api(metadata("written"));
            assert_eq!(app.backend.take_playlist_item_requests().len(), 1);
        }
    }

    #[test]
    fn stale_playlist_metadata_cannot_undo_a_successful_write() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "best".into(),
            PlaylistPage {
                generation: 4,
                playlist: Loadable::Loaded(Playlist {
                    id: "best".into(),
                    snapshot_id: Some("before".into()),
                    items_count: Some(TrackCount { total: 2 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:one"),
                        cached_playlist_row("spotify:track:honey"),
                    ],
                    total: Some(2),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.handle_api(ApiResponse::PlaylistItemsChanged {
            id: "best".into(),
            message: String::new(),
            result: Ok(Some("after".into())),
        });

        app.handle_api(ApiResponse::Playlist {
            id: "best".into(),
            generation: 4,
            result: Ok(Playlist {
                id: "best".into(),
                snapshot_id: Some("before".into()),
                items_count: Some(TrackCount { total: 1 }),
                ..Default::default()
            }),
        });

        let page = &app.playlist_pages["best"];
        assert_eq!(
            page.playlist
                .get()
                .and_then(|playlist| playlist.snapshot_id.as_deref()),
            Some("after")
        );
        assert_eq!(page.items.total, Some(2));
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(page.snapshot_rechecks, 1);

        app.handle_api(ApiResponse::Playlist {
            id: "best".into(),
            generation: 4,
            result: Ok(Playlist {
                id: "best".into(),
                snapshot_id: Some("after".into()),
                items_count: Some(TrackCount { total: 2 }),
                ..Default::default()
            }),
        });

        let page = &app.playlist_pages["best"];
        assert_eq!(page.optimistic_snapshot, None);
        assert_eq!(page.snapshot_rechecks, 0);
        assert_eq!(page.items.total, Some(2));
    }

    #[test]
    fn market_specific_track_uris_share_the_liked_state() {
        let mut app = headless_app();
        let recording = |uri: &str| Track {
            uri: uri.into(),
            external_ids: crate::api::models::ExternalIds {
                isrc: Some("GBUM71029604".into()),
            },
            ..Default::default()
        };
        let saved_uri = "spotify:track:original";
        let playing_uri = "spotify:track:playable";

        app.remember_track_recording(&recording(saved_uri));
        app.set_saved_state(saved_uri.into(), true);
        app.remember_track_recording(&recording(playing_uri));

        assert_eq!(app.is_saved(playing_uri), Some(true));
        assert_eq!(app.saved_toggle_targets(playing_uri), vec![saved_uri]);
    }

    #[test]
    fn a_stale_contains_answer_does_not_undo_a_liked_song() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let uri = "spotify:track:honey";
        app.apply(Action::ToggleSaved(uri.into()), &egui::Context::default());
        assert_eq!(app.is_saved(uri), Some(true));

        app.handle_api(ApiResponse::Contains {
            uris: vec![uri.into()],
            result: Ok(vec![false]),
        });

        assert_eq!(
            app.is_saved(uri),
            Some(true),
            "the answer from before the click cannot make the heart flicker"
        );
    }

    #[test]
    fn adding_an_existing_song_asks_before_writing_it_again() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_busy = true;

        app.handle_api(ApiResponse::PlaylistDuplicatesChecked {
            position: None,
            playlist_id: "mix".into(),
            playlist_name: "Night mix".into(),
            items: vec![
                cached_playlist_row("spotify:track:again")
                    .playable()
                    .unwrap()
                    .clone(),
            ],
            result: Ok(vec!["spotify:track:again".into()]),
        });

        assert!(!app.playlist_busy, "the confirmation is interactive");
        assert!(matches!(
            app.dialog,
            Some(Dialog::ConfirmPlaylistDuplicates {
                ref playlist_id,
                ref duplicate_uris,
                ..
            }) if playlist_id == "mix" && duplicate_uris.len() == 1
        ));

        app.apply(
            Action::ConfirmAddToPlaylist {
                position: None,
                playlist_id: "mix".into(),
                playlist_name: "Night mix".into(),
                items: vec![
                    cached_playlist_row("spotify:track:again")
                        .playable()
                        .unwrap()
                        .clone(),
                ],
            },
            &egui::Context::default(),
        );

        assert!(app.dialog.is_none());
        assert!(app.playlist_busy, "the confirmed write is in flight");
    }

    #[test]
    fn a_song_not_in_the_playlist_needs_no_confirmation() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_busy = true;

        app.handle_api(ApiResponse::PlaylistDuplicatesChecked {
            position: None,
            playlist_id: "mix".into(),
            playlist_name: "Night mix".into(),
            items: vec![
                cached_playlist_row("spotify:track:new")
                    .playable()
                    .unwrap()
                    .clone(),
            ],
            result: Ok(Vec::new()),
        });

        assert!(app.dialog.is_none());
        assert!(app.playlist_busy, "the playlist write follows the check");
    }

    #[test]
    fn playlist_cache_count_must_match_spotify_before_it_can_choose_the_first_song() {
        for cache_first in [true, false] {
            let mut app = headless_app();
            app.backend.set_offline(true);
            app.user = Some(User {
                id: "alice".into(),
                ..Default::default()
            });
            app.playlist_pages
                .insert("mix".into(), PlaylistPage::default());
            let header = ApiResponse::Playlist {
                id: "mix".into(),
                generation: 0,
                result: Ok(Playlist {
                    id: "mix".into(),
                    snapshot_id: Some("same-revision".into()),
                    items_count: Some(TrackCount { total: 3 }),
                    ..Default::default()
                }),
            };
            let cache = Some(PlaylistCache {
                snapshot: "same-revision".into(),
                items: vec![cached_playlist_row("spotify:track:wrong"); 4],
                total: 4,
                next_offset: None,
                appendable: false,
            });
            if cache_first {
                app.receive_playlist_cache("alice", "mix", 0, cache);
                app.handle_api(header);
            } else {
                app.handle_api(header);
                app.receive_playlist_cache("alice", "mix", 0, cache);
            }
            assert_eq!(
                app.playlist_start("mix"),
                (None, Some(0)),
                "a matching revision cannot make a cache with the wrong count authoritative"
            );
            app.handle_api(ApiResponse::PlaylistItems {
                id: "mix".into(),
                offset: 0,
                generation: 0,
                result: Ok(crate::api::models::Page {
                    items: vec![cached_playlist_row("spotify:track:right"); 3],
                    total: 3,
                    limit: 50,
                    ..Default::default()
                }),
            });
            assert_eq!(
                app.playlist_start("mix"),
                (Some("spotify:track:right".into()), None)
            );
            assert_eq!(app.playlist_pages["mix"].items.items.len(), 3);
        }
    }

    #[test]
    fn playlist_cache_waits_until_all_optimistic_writes_are_confirmed() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.playlist_pages.insert(
            "mix".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "mix".into(),
                    snapshot_id: Some("before".into()),
                    items_count: Some(TrackCount { total: 2 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:shown"); 2],
                    total: Some(2),
                    next_offset: None,
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                pending_writes: 2,
                ..Default::default()
            },
        );
        app.checkpoint_playlist_cache("mix");
        assert_eq!(app.playlist_pages["mix"].cache_saved_through, None);
        for (snapshot, saved) in [("first-write", None), ("both-writes", None)] {
            app.handle_api(ApiResponse::PlaylistItemsChanged {
                id: "mix".into(),
                message: String::new(),
                result: Ok(Some(snapshot.into())),
            });
            assert_eq!(app.playlist_pages["mix"].cache_saved_through, saved);
            assert_eq!(
                app.playlist_pages["mix"].items.items.len(),
                2,
                "pending edits stay visible while disk persistence waits"
            );
        }
        assert!(app.playlist_pages["mix"].cache_write_pending.is_some());
        app.receive_playlist_cache_stored("alice", "mix", 0, "both-writes", true);
        assert_eq!(app.playlist_pages["mix"].cache_saved_through, Some(2));
    }

    #[test]
    fn a_matching_partial_playlist_cache_resumes_at_its_next_page() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "large".into(),
            PlaylistPage {
                generation: 7,
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("current".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    loading: true,
                    ..Default::default()
                },
                pending_cache: Some(PlaylistCache {
                    snapshot: "current".into(),
                    items: vec![
                        cached_playlist_row("spotify:track:one"),
                        cached_playlist_row("spotify:track:two"),
                    ],
                    total: 10_000,
                    next_offset: Some(500),
                    appendable: true,
                }),
                cache_checked: true,
                ..Default::default()
            },
        );

        app.try_adopt_playlist_cache("large");

        let page = &app.playlist_pages["large"];
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(page.items.total, Some(10_000));
        assert_eq!(page.items.next_offset, Some(500));
        assert_eq!(page.cache_saved_through, Some(500));
        assert_eq!(page.cache_restored_through, Some(500));
        assert!(page.cache_append_valid);
        assert!(page.items.can_load_more());
        assert_eq!(
            app.backend.take_playlist_sample_requests(),
            vec![("large".into(), 9_950, 7)],
            "restoring a prefix still samples the final page once"
        );

        // The offset-zero request starts alongside the cache read. Its late
        // answer must not replace the longer prefix that was restored.
        app.handle_api(ApiResponse::PlaylistItems {
            id: "large".into(),
            offset: 0,
            generation: 7,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:stale")],
                total: 10_000,
                limit: PLAYLIST_PAGE_SIZE,
                offset: 0,
                next: Some("next".into()),
            }),
        });
        let page = &app.playlist_pages["large"];
        assert_eq!(page.items.items.len(), 2);
        assert_eq!(
            page.items.items[0].playable().map(PlayableItem::uri),
            Some("spotify:track:one")
        );
        assert_eq!(page.items.next_offset, Some(500));

        // Reopening a valid incremental cache must not rewrite its prefix.
        app.checkpoint_playlist_cache("large");
        assert!(app.playlist_pages["large"].cache_write_pending.is_none());
        let page = app.playlist_pages.get_mut("large").unwrap();
        page.items
            .items
            .push(cached_playlist_row("spotify:track:three"));
        page.items.next_offset = Some(1_000);
        app.checkpoint_playlist_cache("large");
        assert!(
            app.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| !pending.replacing)
        );
    }

    #[test]
    fn a_partial_playlist_cache_from_an_old_snapshot_is_not_shown() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "changed".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("new".into()),
                    ..Default::default()
                }),
                pending_cache: Some(PlaylistCache {
                    snapshot: "old".into(),
                    items: vec![cached_playlist_row("spotify:track:old")],
                    total: 10_000,
                    next_offset: Some(500),
                    appendable: false,
                }),
                cache_checked: true,
                ..Default::default()
            },
        );

        app.try_adopt_playlist_cache("changed");

        let page = &app.playlist_pages["changed"];
        assert!(page.items.items.is_empty());
        assert!(page.pending_cache.is_none());
        assert_eq!(page.cache_saved_through, None);
        assert_eq!(page.cache_restored_through, None);
    }

    #[test]
    fn playlist_cache_checkpoints_are_periodic_and_include_completion() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.playlist_pages.insert(
            "large".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("current".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:one")],
                    total: Some(10_000),
                    next_offset: Some(50),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        app.checkpoint_playlist_cache("large");
        assert_eq!(
            app.playlist_pages["large"].cache_saved_through, None,
            "a short new prefix must not overwrite a longer cache before it is read"
        );
        app.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .cache_checked = true;
        app.checkpoint_playlist_cache("large");
        assert_eq!(app.playlist_pages["large"].cache_saved_through, None);
        assert!(
            app.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| pending.replacing)
        );
        app.receive_playlist_cache_stored("alice", "large", 0, "current", true);
        assert_eq!(app.playlist_pages["large"].cache_saved_through, Some(50));

        app.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .items
            .next_offset = Some(100);
        app.checkpoint_playlist_cache("large");
        assert_eq!(
            app.playlist_pages["large"].cache_saved_through,
            Some(50),
            "one more page is too small to rewrite the growing cache"
        );

        app.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .items
            .items
            .push(cached_playlist_row("spotify:track:two"));
        app.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .items
            .next_offset = Some(550);
        app.checkpoint_playlist_cache("large");
        assert_eq!(app.playlist_pages["large"].cache_saved_through, Some(50));
        assert!(
            app.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| !pending.replacing)
        );
        app.receive_playlist_cache_stored("alice", "large", 0, "current", true);
        assert_eq!(app.playlist_pages["large"].cache_saved_through, Some(550));
        assert_eq!(app.playlist_pages["large"].cache_saved_rows, 2);

        let page = app.playlist_pages.get_mut("large").expect("the playlist");
        page.items.total = Some(575);
        page.items.next_offset = None;
        app.checkpoint_playlist_cache("large");
        assert_eq!(app.playlist_pages["large"].cache_saved_through, Some(550));
        assert!(
            app.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| pending.replacing)
        );
        app.receive_playlist_cache_stored("alice", "large", 0, "current", true);
        assert_eq!(
            app.playlist_pages["large"].cache_saved_through,
            Some(575),
            "the final short interval is still saved"
        );
    }

    #[test]
    fn failed_playlist_cache_append_rebuilds_from_the_last_confirmed_checkpoint() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.playlist_pages.insert(
            "large".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("current".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:one"),
                        cached_playlist_row("spotify:track:two"),
                    ],
                    total: Some(1_000),
                    next_offset: Some(550),
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                cache_saved_through: Some(50),
                cache_saved_rows: 1,
                cache_saved_total: Some(1_000),
                cache_append_valid: true,
                ..Default::default()
            },
        );

        app.checkpoint_playlist_cache("large");
        assert!(
            app.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| !pending.replacing)
        );
        app.receive_playlist_cache_stored("alice", "large", 0, "current", false);
        assert_eq!(app.playlist_pages["large"].cache_saved_through, Some(50));
        assert!(
            app.playlist_pages["large"]
                .cache_write_pending
                .as_ref()
                .is_some_and(|pending| pending.replacing)
        );
        app.receive_playlist_cache_stored("alice", "large", 0, "current", true);
        assert_eq!(app.playlist_pages["large"].cache_saved_through, Some(550));
        assert_eq!(app.playlist_pages["large"].cache_saved_rows, 2);
    }

    #[test]
    fn removing_a_song_updates_the_finite_extent_immediately() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let mut items = PagedList::default();
        items.restore_cached(
            vec![
                cached_playlist_row("spotify:track:remove"),
                cached_playlist_row("spotify:track:keep"),
            ],
            2,
            None,
        );
        app.playlist_pages.insert(
            "edit".into(),
            PlaylistPage {
                items,
                playlist: Loadable::Loaded(Playlist {
                    items_count: Some(TrackCount { total: 2 }),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        app.apply(
            Action::RemoveFromPlaylist {
                playlist_id: "edit".into(),
                uris: vec!["spotify:track:remove".into()],
            },
            &egui::Context::default(),
        );
        let page = &app.playlist_pages["edit"];
        assert_eq!(page.items.total, Some(1));
        assert_eq!(page.items.items.len(), 1);
        assert_eq!(page.items.next_offset, None);
        assert_eq!(
            page.playlist
                .get()
                .unwrap()
                .items_count
                .as_ref()
                .unwrap()
                .total,
            1
        );
    }

    #[test]
    fn an_overlapping_window_can_extend_a_restored_partial_prefix() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let mut items = PagedList::default();
        items.restore_cached(
            vec![cached_playlist_row("spotify:track:cached"); 499],
            1000,
            Some(499),
        );
        app.playlist_pages.insert(
            "overlap".into(),
            PlaylistPage {
                items,
                cache_restored_through: Some(499),
                tail_checked: true,
                ..Default::default()
            },
        );
        app.load_window(Page::Playlist("overlap".into()), 499);
        app.handle_api(ApiResponse::PlaylistItems {
            id: "overlap".into(),
            offset: 450,
            generation: 0,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:fresh"); 50],
                total: 1000,
                offset: 450,
                limit: 50,
                next: Some("next".into()),
            }),
        });
        let items = &app.playlist_pages["overlap"].items;
        assert!(!items.loading);
        assert_eq!(items.items.len(), 500);
        assert_eq!(items.next_offset, Some(500));
    }

    #[test]
    fn late_disk_prefix_does_not_replace_a_pending_distant_window() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let mut items = PagedList::default();
        items.restore_cached(
            vec![cached_playlist_row("spotify:track:first"); 50],
            1000,
            Some(50),
        );
        items.window_at(720, 50);
        app.playlist_pages.insert(
            "late".into(),
            PlaylistPage {
                generation: 7,
                items_generation: 7,
                tail_checked: true,
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("same".into()),
                    ..Default::default()
                }),
                items,
                pending_cache: Some(PlaylistCache {
                    snapshot: "same".into(),
                    items: vec![cached_playlist_row("spotify:track:cached"); 500],
                    total: 1000,
                    next_offset: Some(500),
                    appendable: false,
                }),
                ..Default::default()
            },
        );
        app.try_adopt_playlist_cache("late");
        let page = &app.playlist_pages["late"];
        assert_eq!(page.items.base_offset, 700);
        assert!(page.items.loading);
        assert_eq!(page.items.windows.get(&0).map(Vec::len), Some(500));
    }

    #[test]
    fn scrollbar_windows_share_the_cache_and_deduplicate_pending_requests() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let first = cached_playlist_row("spotify:track:first");
        app.playlist_pages.insert(
            "scroll".into(),
            PlaylistPage {
                generation: 7,
                items_generation: 7,
                tail_checked: true,
                cache_checked: true,
                items: PagedList {
                    items: vec![first; 50],
                    total: Some(1000),
                    next_offset: Some(50),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.load_window(Page::Playlist("scroll".into()), 720);
        app.load_window(Page::Playlist("scroll".into()), 730);
        assert_eq!(
            app.backend.take_playlist_item_requests(),
            vec![("scroll".into(), 700, 7)]
        );
        app.handle_api(ApiResponse::PlaylistItems {
            id: "scroll".into(),
            offset: 700,
            generation: 7,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:distant"); 50],
                total: 1000,
                offset: 700,
                limit: 50,
                next: Some("next".into()),
            }),
        });
        app.load_window(Page::Playlist("scroll".into()), 10);
        assert!(app.backend.take_playlist_item_requests().is_empty());
        assert_eq!(app.playlist_pages["scroll"].items.base_offset, 0);
        assert_eq!(app.playlist_pages["scroll"].items.total, Some(1000));
    }

    #[test]
    fn sorting_an_album_invalidates_the_in_flight_window() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.load_generation = 7;
        app.album_pages.insert(
            "album".into(),
            AlbumPage {
                generation: 7,
                tracks: PagedList {
                    base_offset: 150,
                    total: Some(200),
                    loading: true,
                    window_request: Some(150),
                    next_offset: Some(150),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.table_sorts.insert(
            Page::Album("album".into()),
            TableSort {
                column: SortColumn::Title,
                ascending: true,
            },
        );
        app.load_more(Page::Album("album".into()));
        let generation = app.album_pages["album"].generation;
        app.handle_api(ApiResponse::AlbumTracks {
            id: "album".into(),
            offset: 0,
            generation,
            result: Ok(crate::api::models::Page {
                items: vec![Track::default(); 50],
                total: 200,
                offset: 0,
                limit: 50,
                next: Some("next".into()),
            }),
        });
        app.handle_api(ApiResponse::AlbumTracks {
            id: "album".into(),
            offset: 150,
            generation: 7,
            result: Ok(crate::api::models::Page {
                items: vec![Track::default(); 50],
                total: 200,
                offset: 150,
                limit: 50,
                next: None,
            }),
        });
        assert_eq!(app.album_pages["album"].tracks.items.len(), 50);
        assert_eq!(app.album_pages["album"].tracks.next_offset, Some(50));
    }

    #[test]
    fn null_album_slots_are_not_shuffle_candidates() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let album: crate::api::models::Album = serde_json::from_str(r#"{"tracks":{"items":[{"uri":"spotify:track:a"},null,{"uri":"spotify:track:c"}],"total":3}}"#).unwrap();
        let mut page = AlbumPage::default();
        page.tracks.absorb(0, album.tracks.unwrap());
        app.album_pages.insert("album".into(), page);
        assert_eq!(
            app.context_track_uris("spotify:album:album"),
            Some(vec!["spotify:track:a".into(), "spotify:track:c".into()])
        );
    }

    #[test]
    fn album_window_from_an_evicted_generation_cannot_replace_current_rows() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.album_pages.insert(
            "album".into(),
            AlbumPage {
                generation: 8,
                ..Default::default()
            },
        );
        app.handle_api(ApiResponse::AlbumTracks {
            id: "album".into(),
            offset: 150,
            generation: 7,
            result: Ok(crate::api::models::Page {
                items: vec![Track::default()],
                total: 200,
                ..Default::default()
            }),
        });
        assert!(app.album_pages["album"].tracks.items.is_empty());
    }

    #[test]
    fn a_distant_playlist_position_needs_one_direct_request() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "large".into(),
            PlaylistPage {
                generation: 3,
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("current".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    total: Some(10_000),
                    next_offset: Some(50),
                    loaded_once: true,
                    ..Default::default()
                },
                items_generation: 3,
                cache_checked: true,
                tail_checked: true,
                ..Default::default()
            },
        );
        let _ = app.backend.take_playlist_item_requests();

        // Dragging the scrollbar far down asks for that window directly.
        app.apply(
            Action::LoadWindow {
                page: Page::Playlist("large".into()),
                position: 6_906,
            },
            &egui::Context::default(),
        );

        let requests = app.backend.take_playlist_item_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0, "large");
        assert_eq!(requests[0].1, 6_900);
        let generation = requests[0].2;
        let page = &app.playlist_pages["large"];
        assert_eq!(page.generation, generation);
        assert_eq!(page.items.base_offset, 6_900);
        assert_eq!(page.items.next_offset, Some(6_900));
        assert!(page.items.items.is_empty());

        app.handle_api(ApiResponse::PlaylistItems {
            id: "large".into(),
            offset: 6_900,
            generation,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:6901")],
                total: 10_000,
                limit: PLAYLIST_PAGE_SIZE,
                offset: 6_900,
                next: Some("next".into()),
            }),
        });
        let page = &app.playlist_pages["large"];
        assert_eq!(page.items.base_offset, 6_900);
        assert_eq!(page.items.items.len(), 1);
        assert_eq!(page.items_generation, generation);

        // Filtering keeps its existing whole-list meaning. It restarts from
        // the beginning instead of pretending the distant window is all.
        app.playlist_pages
            .get_mut("large")
            .expect("the playlist")
            .filter = "track".into();
        let _ = app.backend.take_playlist_item_requests();
        app.load_more(Page::Playlist("large".into()));
        let requests = app.backend.take_playlist_item_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1, 0);
        assert_eq!(app.playlist_pages["large"].items.base_offset, 0);
    }

    #[test]
    fn refresh_metadata_cannot_cache_rows_from_the_previous_generation() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.playlist_pages.insert(
            "changed".into(),
            PlaylistPage {
                generation: 9,
                items_generation: 8,
                playlist: Loadable::Loaded(Playlist {
                    snapshot_id: Some("old".into()),
                    ..Default::default()
                }),
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:old")],
                    total: Some(500),
                    next_offset: Some(50),
                    loaded_once: true,
                    ..Default::default()
                },
                cache_checked: true,
                tail_checked: true,
                ..Default::default()
            },
        );

        app.handle_api(ApiResponse::Playlist {
            id: "changed".into(),
            generation: 9,
            result: Ok(Playlist {
                snapshot_id: Some("new".into()),
                ..Default::default()
            }),
        });

        let page = &app.playlist_pages["changed"];
        assert_eq!(page.cache_saved_through, None);
        assert_eq!(page.items_generation, 8);
        assert_eq!(
            page.items.items[0].playable().map(PlayableItem::uri),
            Some("spotify:track:old")
        );

        app.handle_api(ApiResponse::PlaylistItems {
            id: "changed".into(),
            offset: 0,
            generation: 9,
            result: Ok(crate::api::models::Page {
                items: vec![cached_playlist_row("spotify:track:new")],
                total: 500,
                limit: PLAYLIST_PAGE_SIZE,
                offset: 0,
                next: Some("next".into()),
            }),
        });
        let page = &app.playlist_pages["changed"];
        assert_eq!(page.items_generation, 9);
        assert_eq!(page.cache_saved_through, None);
        assert!(page.cache_write_pending.is_some());
        app.receive_playlist_cache_stored("alice", "changed", 9, "new", true);
        let page = &app.playlist_pages["changed"];
        assert_eq!(page.cache_saved_through, Some(50));
        assert_eq!(
            page.items.items[0].playable().map(PlayableItem::uri),
            Some("spotify:track:new")
        );
    }

    #[test]
    fn playlist_play_button_starts_at_the_first_row_instead_of_the_resume_track() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, Role, TreeId};

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.remote = None;
        app.selected_device = None;
        app.local.connected = false;
        app.shuffle_wanted = false;
        let context = "spotify:playlist:pl1";
        let rows = app.context_track_uris(context).unwrap();
        app.resume_context = Some(context.into());
        app.resume_track = Some(rows[7].clone());
        app.resume_position_ms = 23_000;
        app.open(Page::Playlist("pl1".into()));
        assert!(matches!(app.target(), Target::Local));

        let mut draw = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| crate::ui::collection::playlist(&mut app, ui, "pl1"),
            );
            output.textures_delta.clear();
            app.apply_actions(&ctx);
            output.platform_output.accesskit_update.unwrap()
        };
        draw(Vec::new());
        let tree = draw(Vec::new());
        let button = tree
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::Button && node.label() == Some("Play"))
            .expect("the playlist Play button")
            .0;
        draw(vec![egui::Event::AccessKitActionRequest(ActionRequest {
            target_tree: TreeId::ROOT,
            target_node: button,
            action: AccessibleAction::Click,
            data: None,
        })]);

        let request = app
            .queued_play
            .as_ref()
            .expect("waiting for the local engine");
        assert_eq!(request.context_uri.as_deref(), Some(context));
        assert_eq!(request.offset_uri.as_deref(), Some(rows[0].as_str()));
        assert_eq!(request.position_ms, 0);
        assert!(
            request.uris.is_empty(),
            "retain the full Spotify playlist context"
        );
        let load = local_load(request, false);
        assert_eq!(load.offset_uri, Some(rows[0].clone()));
        assert_eq!(load.context_uri.as_deref(), Some(context));
        assert_eq!(app.now_playing().unwrap().uri, rows[0]);
        assert_eq!(app.now_playing().unwrap().position_ms, 0);
        app.intent_track.as_mut().unwrap().at =
            Instant::now() - PLAYBACK_HOLD - Duration::from_secs(1);
        assert_eq!(app.now_playing().unwrap().uri, rows[0]);
        assert_eq!(app.current_track_uri().as_deref(), Some(rows[0].as_str()));

        // A ready notification may replay the pending request before the
        // engine connects. Keep the song chosen at the click even if the
        // playlist has refreshed in the meantime.
        app.playlist_pages
            .get_mut("pl1")
            .unwrap()
            .items
            .items
            .swap(0, 7);
        app.handle_playback(LocalPlayback::Ready {
            device_id: "demo-local".into(),
        });
        assert_eq!(
            app.queued_play.as_ref().unwrap().offset_uri.as_deref(),
            Some(rows[0].as_str())
        );
        app.handle_playback(LocalPlayback::Failed("test connection failure".into()));
        assert!(app.requested_track_preview().is_none());
        app.backend.shutdown();
    }

    /// A row in the Recent tab plays its own song. Each row plays a list
    /// holding only that song, but it hands over its place in the whole
    /// tab, which is past the end of that list for every row but the top.
    #[test]
    fn a_recent_row_plays_its_own_song() {
        use crate::api::models::{ArtistRef, PlayHistory, Track};
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, Role, TreeId};

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.remote = None;
        app.selected_device = None;
        app.local.connected = false;
        app.shuffle_wanted = false;
        assert!(matches!(app.target(), Target::Local));
        app.recents.items = ["newest", "middle", "oldest"]
            .iter()
            .enumerate()
            .map(|(index, id)| PlayHistory {
                track: Track {
                    id: Some((*id).into()),
                    uri: format!("spotify:track:{id}"),
                    name: format!("Recent {id}"),
                    artists: vec![ArtistRef {
                        name: "Recent Artist".into(),
                        ..Default::default()
                    }],
                    duration_ms: 200_000,
                    ..Default::default()
                },
                played_at: Some(format!("2026-09-01T1{}:00:00Z", 5 - index)),
                context: None,
            })
            .collect();
        app.recents.loaded_once = true;
        app.recents.complete = true;
        app.rebuild_recents();
        app.queue_tab = QueueTab::Recents;
        app.show_queue_panel = true;

        let mut draw = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| crate::ui::queue::side_panel(&mut app, ui),
            );
            output.textures_delta.clear();
            app.apply_actions(&ctx);
            output.platform_output.accesskit_update.unwrap()
        };
        draw(Vec::new());
        let tree = draw(Vec::new());
        let row = tree
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == Role::Button
                    && node.label() == Some("Play Recent middle, Recent Artist")
            })
            .expect("the second row of the Recent tab")
            .0;
        draw(vec![egui::Event::AccessKitActionRequest(ActionRequest {
            target_tree: TreeId::ROOT,
            target_node: row,
            action: AccessibleAction::Click,
            data: None,
        })]);

        let request = app
            .queued_play
            .as_ref()
            .expect("waiting for the local engine");
        assert_eq!(request.uris, ["spotify:track:middle"]);
        assert_eq!(
            request.offset_position,
            Some(0),
            "the song's place in the list it plays from"
        );
        assert_eq!(
            app.intent_track.as_ref().map(|intent| intent.uri.as_str()),
            Some("spotify:track:middle"),
            "the chosen song is the one shown as starting"
        );

        // A row whose place holds its own song keeps that place, even when
        // the song is in the list earlier as well.
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Uris(
                    vec![
                        "spotify:track:middle".to_string(),
                        "spotify:track:newest".to_string(),
                        "spotify:track:middle".to_string(),
                    ]
                    .into(),
                ),
                uri: "spotify:track:middle".into(),
                index: 2,
            },
            &ctx,
        );
        assert_eq!(
            app.queued_play
                .as_ref()
                .and_then(|request| request.offset_position),
            Some(2),
            "the second copy of a repeated song plays from its own place"
        );
        // The same row sends a valid one-song request to a Connect device,
        // with either shuffle setting, while its optimistic title stays visible.
        app.selected_device = Some("speaker".into());
        for shuffle in [false, true] {
            app.shuffle_wanted = shuffle;
            app.apply(
                Action::PlayFromRow {
                    context: RowContext::Uris(vec!["spotify:track:middle".into()].into()),
                    uri: "spotify:track:middle".into(),
                    index: 73,
                },
                &ctx,
            );
            let requests = app.backend.take_remote_play_requests();
            assert_eq!(requests.len(), 1);
            let (device, play) = match &requests[0] {
                ApiRequest::Remote {
                    action: RemoteAction::Play,
                    device_id,
                    play: Some(play),
                    ..
                } if !shuffle => (device_id, play),
                ApiRequest::ShufflePlay { device_id, play } if shuffle => (device_id, play),
                other => panic!("unexpected playback request: {other:?}"),
            };
            assert_eq!(device.as_deref(), Some("speaker"));
            assert_eq!(play.uris, ["spotify:track:middle"]);
            assert_eq!(play.offset_position, Some(0));
            assert_eq!(
                app.intent_track.as_ref().map(|intent| intent.uri.as_str()),
                Some("spotify:track:middle")
            );
        }
        app.backend.shutdown();
    }

    fn draw_collection_actions(
        ctx: &egui::Context,
        app: &mut App,
        uri: &str,
        events: Vec<egui::Event>,
    ) {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                crate::ui::collection::actions_row(
                    app,
                    ui,
                    crate::ui::collection::Actions {
                        play_uri: Some(uri.to_owned()),
                        view: None,
                        saved: None,
                        saved_icons: (
                            crate::theme::Icon::CirclePlus,
                            crate::theme::Icon::CircleCheck,
                        ),
                        saved_tooltips: ("".into(), "".into()),
                        owned_playlist: None,
                        reload: None,
                        name: "Test",
                        save_radio: None,
                    },
                    None,
                );
            },
        );
        output.textures_delta.clear();
        app.apply_actions(ctx);
    }

    fn click_collection_action(
        ctx: &egui::Context,
        app: &mut App,
        uri: &str,
        position: egui::Pos2,
    ) {
        let click = vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        draw_collection_actions(ctx, app, uri, vec![]);
        draw_collection_actions(ctx, app, uri, click);
    }

    #[test]
    fn shuffle_selected_without_a_device_applies_when_collection_play_starts() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        let remote = app.remote.take().expect("demo remote playback");
        app.local_ready = false;
        app.local_device_id = None;
        app.local_playback = LocalPlayback::Unavailable;
        app.local.connected = false;
        app.selected_device = None;
        app.shuffle_wanted = false;
        assert!(matches!(app.target(), Target::Remote(None)));

        click_collection_action(
            &ctx,
            &mut app,
            "spotify:playlist:pl0",
            egui::pos2(87.0, 28.0),
        );
        assert!(app.playing_context_shuffle());
        assert!(app.remote.is_none(), "Shuffle must not start playback");
        assert!(app.backend.take_remote_play_requests().is_empty());
        assert!(
            app.backend.take_remote_shuffle_requests().is_empty(),
            "without an active device, Shuffle stays pending instead of calling Spotify"
        );
        assert!(
            app.toasts.is_empty(),
            "selecting a pending mode shows no error"
        );

        let playing_context = remote
            .state
            .context
            .as_ref()
            .expect("demo playing context")
            .uri
            .clone();
        assert_ne!(playing_context, "spotify:playlist:pl0");
        app.remote = Some(remote);
        click_collection_action(
            &ctx,
            &mut app,
            "spotify:playlist:pl0",
            egui::pos2(28.0, 28.0),
        );

        let requests = app.backend.take_remote_play_requests();
        assert!(matches!(
            requests.as_slice(),
            [ApiRequest::ShufflePlay { device_id: Some(device), play }]
                if device == "remote1" && play.context_uri.as_deref() == Some("spotify:playlist:pl0")
        ));
        app.backend.shutdown();
    }

    #[test]
    fn player_bar_uses_pending_shuffle_without_a_device() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, Toggled, TreeId};

        fn draw_player_bar(
            ctx: &egui::Context,
            app: &mut App,
            events: Vec<egui::Event>,
        ) -> egui::accesskit::TreeUpdate {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| crate::ui::player_bar::show(app, ui),
            );
            output.textures_delta.clear();
            app.apply_actions(ctx);
            output.platform_output.accesskit_update.unwrap()
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.remote.take().expect("demo remote playback");
        app.local_ready = false;
        app.local_device_id = None;
        app.local_playback = LocalPlayback::Unavailable;
        app.local.connected = false;
        app.selected_device = None;
        app.shuffle_wanted = false;
        assert!(app.now_playing().is_none());

        click_collection_action(
            &ctx,
            &mut app,
            "spotify:playlist:pl0",
            egui::pos2(87.0, 28.0),
        );
        assert!(app.playing_context_shuffle());

        draw_player_bar(&ctx, &mut app, Vec::new());
        let enabled = draw_player_bar(&ctx, &mut app, Vec::new());
        let (shuffle_id, shuffle_node) = enabled
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Shuffle"))
            .expect("player-bar Shuffle button");
        assert_eq!(shuffle_node.toggled(), Some(Toggled::True));

        draw_player_bar(
            &ctx,
            &mut app,
            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                target_tree: TreeId::ROOT,
                target_node: *shuffle_id,
                action: AccessibleAction::Click,
                data: None,
            })],
        );
        assert!(!app.playing_context_shuffle());

        let disabled = draw_player_bar(&ctx, &mut app, Vec::new());
        let shuffle_node = &disabled
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Shuffle"))
            .expect("player-bar Shuffle button")
            .1;
        assert_eq!(shuffle_node.toggled(), Some(Toggled::False));
        assert!(app.backend.take_remote_shuffle_requests().is_empty());
        app.backend.shutdown();
    }

    #[test]
    fn shuffle_targets_active_remote_playback_without_a_device_id() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.local_ready = false;
        app.selected_device = None;
        app.shuffle_wanted = false;
        app.remote
            .as_mut()
            .unwrap()
            .state
            .device
            .as_mut()
            .unwrap()
            .id = None;
        assert!(matches!(app.target(), Target::Remote(None)));

        click_collection_action(
            &ctx,
            &mut app,
            "spotify:playlist:pl0",
            egui::pos2(87.0, 28.0),
        );
        assert!(matches!(
            app.backend.take_remote_shuffle_requests().as_slice(),
            [ApiRequest::Remote {
                action: RemoteAction::Shuffle,
                device_id: None,
                flag: true,
                ..
            }]
        ));
        app.backend.shutdown();
    }

    #[test]
    fn shuffle_toggle_on_another_collection_keeps_playing_context_until_play() {
        let ctx = egui::Context::default();
        let mut app = headless_app();
        app.attach(&ctx);
        crate::demo::populate(&mut app);
        app.shuffle_wanted = true;
        let playing_context = app
            .playing_context_uri()
            .expect("demo is playing a collection");
        let other_collection = "spotify:playlist:pl0";
        assert_ne!(playing_context, other_collection);
        app.open(Page::Playlist("pl0".into()));

        click_collection_action(&ctx, &mut app, other_collection, egui::pos2(87.0, 28.0));
        assert!(!app.playing_context_shuffle());
        assert_eq!(
            app.playing_context_uri().as_deref(),
            Some(playing_context.as_str())
        );
        assert!(
            app.backend.take_remote_play_requests().is_empty(),
            "changing the global mode must not start the viewed collection"
        );

        click_collection_action(&ctx, &mut app, other_collection, egui::pos2(28.0, 28.0));
        let requests = app.backend.take_remote_play_requests();
        assert!(matches!(
            requests.as_slice(),
            [ApiRequest::Remote {
                action: RemoteAction::Play,
                device_id: Some(device),
                play: Some(play),
                ..
            }] if device == "remote1" && play.context_uri.as_deref() == Some(other_collection)
        ));
        app.backend.shutdown();
    }

    #[test]
    fn collection_play_starts_at_the_first_available_row_in_the_shown_view() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, Role, TreeId};
        for liked in [false, true] {
            for filter in [None, Some("First"), Some("Unavailable"), Some("Missing")] {
                let filtered = filter.is_some();
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                let mut app = headless_app();
                app.attach(&ctx);
                crate::demo::populate(&mut app);
                app.remote = None;
                app.selected_device = None;
                app.local.connected = false;
                app.shuffle_wanted = false;
                let track = |id: &str, name: &str, available, local| Track {
                    id: Some(id.into()),
                    uri: if local {
                        "spotify:local:Artist:Album:Song:180".into()
                    } else {
                        format!("spotify:track:{id}")
                    },
                    name: name.into(),
                    is_playable: Some(available),
                    is_local: local,
                    duration_ms: 180_000,
                    ..Default::default()
                };
                let tracks = [
                    track("other", "03 Other", true, false),
                    track("unavailable", "00 Unavailable", false, false),
                    track("local", "01 Local", true, true),
                    track("first", "02 First", true, false),
                    track("first", "02 First", true, false),
                ];
                let page = if liked {
                    Page::LikedSongs
                } else {
                    Page::Playlist("pl1".into())
                };
                if liked {
                    app.library.liked.items = tracks
                        .iter()
                        .map(|track| crate::api::models::SavedTrack {
                            track: track.clone(),
                            added_at: None,
                        })
                        .collect();
                    app.library.liked.total = Some(5);
                    app.library.liked.next_offset = None;
                    app.library.liked.revision += 1;
                    if let Some(filter) = filter {
                        ctx.data_mut(|data| {
                            data.insert_temp(egui::Id::new("liked-filter"), filter.to_string())
                        });
                    }
                } else {
                    let list = app.playlist_pages.get_mut("pl1").unwrap();
                    list.items.items = tracks
                        .iter()
                        .map(|track| PlaylistItem {
                            // Spotify can mark only the enclosing playlist row local.
                            is_local: track.is_local,
                            item: Some(PlayableItem::Track(Track {
                                is_local: false,
                                ..track.clone()
                            })),
                            ..Default::default()
                        })
                        .collect();
                    list.items.total = Some(5);
                    list.items.next_offset = None;
                    list.items.revision += 1;
                    if let Some(filter) = filter {
                        list.filter = filter.into();
                    }
                }
                if !filtered {
                    app.table_sorts.insert(
                        page.clone(),
                        TableSort {
                            column: SortColumn::Title,
                            ascending: true,
                        },
                    );
                }
                app.open(page);
                let mut draw = |events| {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(1280.0, 800.0),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            if liked {
                                crate::ui::collection::liked(&mut app, ui);
                            } else {
                                crate::ui::collection::playlist(&mut app, ui, "pl1");
                            }
                        },
                    );
                    output.textures_delta.clear();
                    app.apply_actions(&ctx);
                    output.platform_output.accesskit_update.unwrap()
                };
                draw(vec![]);
                let tree = draw(vec![]);
                let button = tree
                    .nodes
                    .iter()
                    .find(|(_, node)| node.role() == Role::Button && node.label() == Some("Play"))
                    .expect("collection Play")
                    .0;
                draw(vec![egui::Event::AccessKitActionRequest(ActionRequest {
                    target_tree: TreeId::ROOT,
                    target_node: button,
                    action: AccessibleAction::Click,
                    data: None,
                })]);
                if matches!(filter, Some("Unavailable" | "Missing")) {
                    let play = tree.nodes.iter().find(|(id, _)| *id == button).unwrap();
                    assert!(
                        play.1.is_disabled(),
                        "a view without playable songs disables Play"
                    );
                    assert!(
                        app.queued_play.is_none(),
                        "do not start the unfiltered context when no shown song can play"
                    );
                    app.backend.shutdown();
                    continue;
                }
                let request = app
                    .queued_play
                    .as_ref()
                    .expect("waiting for local playback");
                if filtered {
                    assert_eq!(
                        request.uris,
                        vec!["spotify:track:first", "spotify:track:first"],
                        "a filtered view keeps only its shown songs and preserves duplicates"
                    );
                    assert_eq!(request.offset_position, Some(0));
                } else {
                    assert_eq!(
                        request.uris,
                        vec![
                            "spotify:track:first",
                            "spotify:track:first",
                            "spotify:track:other"
                        ]
                    );
                    assert_eq!(
                        request.offset_position,
                        Some(0),
                        "playback starts at the first playable row"
                    );
                }
                assert_eq!(app.now_playing().unwrap().uri, "spotify:track:first");
                let mut settled = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280.0, 800.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        if liked {
                            crate::ui::collection::liked(&mut app, ui);
                        } else {
                            crate::ui::collection::playlist(&mut app, ui, "pl1");
                        }
                    },
                );
                settled.textures_delta.clear();
                app.apply_actions(&ctx);
                let tree = settled.platform_output.accesskit_update.unwrap();
                let second = tree
                    .nodes
                    .iter()
                    .filter(|(_, node)| {
                        node.role() == Role::Button
                            && node
                                .label()
                                .is_some_and(|label| label.starts_with("Play 02 First,"))
                    })
                    .max_by(|a, b| {
                        a.1.bounds()
                            .unwrap()
                            .y0
                            .total_cmp(&b.1.bounds().unwrap().y0)
                    })
                    .expect("second duplicate row")
                    .0;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280.0, 800.0),
                        )),
                        events: vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            target_tree: TreeId::ROOT,
                            target_node: second,
                            action: AccessibleAction::Click,
                            data: None,
                        })],
                        ..Default::default()
                    },
                    |ui| {
                        if liked {
                            crate::ui::collection::liked(&mut app, ui);
                        } else {
                            crate::ui::collection::playlist(&mut app, ui, "pl1");
                        }
                    },
                );
                output.textures_delta.clear();
                assert!(
                    app.actions
                        .iter()
                        .any(|action| matches!(action, Action::PlayFromRow { index: 1, .. })),
                    "second occurrence action: {:?}",
                    app.actions
                );
                app.apply_actions(&ctx);
                assert_eq!(
                    app.queued_play.as_ref().unwrap().offset_position,
                    Some(1),
                    "selecting the second duplicate keeps its occurrence in the playable list"
                );
                app.backend.shutdown();
            }
        }
    }

    #[test]
    fn sorted_view_play_shows_an_uncached_song_while_local_playback_connects() {
        let mut app = headless_app();
        app.backend.set_offline(true);
        let ctx = egui::Context::default();
        let first = Track {
            id: Some("first".into()),
            uri: "spotify:track:first".into(),
            name: "First in the sorted view".into(),
            duration_ms: 180_000,
            ..Default::default()
        };
        app.playlist_pages.insert(
            "mix".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        cached_playlist_row("spotify:track:other"),
                        PlaylistItem {
                            item: Some(PlayableItem::Track(first.clone())),
                            ..Default::default()
                        },
                    ],
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(!app.track_cache.contains_key("first"));
        app.apply(
            Action::PlayFromRow {
                context: RowContext::View {
                    context_uri: "spotify:playlist:mix".into(),
                    uris: vec![first.uri.clone(), "spotify:track:other".into()].into(),
                    editable_playlist: None,
                },
                uri: String::new(),
                index: 0,
            },
            &ctx,
        );
        let now = app
            .now_playing()
            .expect("the requested song appears before the engine connects");
        assert_eq!(now.uri, first.uri);
        assert_eq!(now.title, first.name);
        assert_eq!(now.position_ms, 0);
        assert!(now.loading);
        assert_eq!(
            app.queued_play.as_ref().unwrap().uris,
            vec![first.uri.clone(), "spotify:track:other".into()]
        );
        assert_eq!(
            app.playlist_pages["mix"].items.items[0]
                .playable()
                .unwrap()
                .uri(),
            "spotify:track:other",
            "previewing must not change the playlist order"
        );
        app.intent_track.as_mut().unwrap().at =
            Instant::now() - PLAYBACK_HOLD - Duration::from_secs(1);
        assert_eq!(
            app.now_playing().unwrap().uri,
            first.uri,
            "the preview stays while the play request is pending"
        );
    }

    #[test]
    fn each_repeat_of_a_local_short_song_must_earn_its_own_history_entry() {
        let mut app = headless_app();
        app.remote = None;
        app.selected_device = None;
        app.plays = crate::history::History::default();
        let state_dir =
            std::env::temp_dir().join(format!("spotifast-repeat-history-{}", std::process::id()));
        app.dirs.state = state_dir.clone();
        for sequence in [1, 2] {
            app.handle_local(LocalState {
                connected: true,
                playback: Playback::Playing,
                track_sequence: sequence,
                track: Some(crate::player::LocalTrack {
                    uri: "spotify:track:short".into(),
                    title: "Short interlude".into(),
                    duration_ms: 40_000,
                    ..Default::default()
                }),
                ..Default::default()
            });
            app.note_listening();
            assert_eq!(app.plays.plays().len(), sequence as usize - 1);
            let listening = app.listening.as_mut().unwrap();
            assert!(
                !listening.recorded,
                "the new play must be counted separately"
            );
            listening.playing_since = Some(Instant::now() - Duration::from_secs(21));
            app.note_listening();
            assert_eq!(app.plays.plays().len(), sequence as usize);
            assert_eq!(app.recents_view.len(), sequence as usize);

            // Seeking or pausing the same play cannot count it again.
            let mut paused = app.local.clone();
            paused.playback = Playback::Paused;
            paused.seek_sequence += 1;
            app.handle_local(paused);
            app.note_listening();
            let mut resumed = app.local.clone();
            resumed.playback = Playback::Playing;
            app.handle_local(resumed);
            app.note_listening();
            assert_eq!(app.plays.plays().len(), sequence as usize);
        }
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(state_dir);
    }

    #[test]
    fn playlist_start_preview_waits_for_playback_before_consuming_queue_or_history() {
        let mut app = headless_app();
        crate::demo::populate(&mut app);
        app.remote = None;
        app.selected_device = None;
        app.shuffle_wanted = false;
        let rows = app.context_track_uris("spotify:playlist:pl1").unwrap();
        let old = crate::player::LocalTrack {
            uri: rows[7].clone(),
            duration_ms: 200_000,
            ..Default::default()
        };
        app.local = LocalState {
            connected: true,
            track: Some(old),
            playback: Playback::Playing,
            position_ms: 23_000,
            ..Default::default()
        };
        app.last_now_playing_uri = Some(rows[7].clone());
        app.manual_queue = vec![rows[0].clone()];
        app.play_request(PlayRequest::context("spotify:playlist:pl1"), false);
        assert!(
            app.queued_play.is_none(),
            "the connected engine received it"
        );
        assert_eq!(app.now_playing().unwrap().uri, rows[0]);
        assert!(app.now_playing().unwrap().loading);

        // A progress report for the old song cannot undo the user's start.
        let mut stale = app.local.clone();
        stale.position_ms += 1;
        app.handle_local(stale);
        app.refresh_frame_now();
        assert_eq!(app.now_playing().unwrap().uri, rows[0]);
        app.on_now_playing_changed();
        assert_eq!(app.manual_queue, vec![rows[0].clone()]);
        app.note_listening();
        assert_eq!(app.listening.as_ref().unwrap().uri, rows[7]);

        let mut started = app.local.clone();
        started.track = Some(crate::player::LocalTrack {
            uri: rows[0].clone(),
            duration_ms: 200_000,
            ..Default::default()
        });
        started.position_ms = 0;
        app.handle_local(started);
        app.refresh_frame_now();
        assert_eq!(app.now_playing().unwrap().uri, rows[0]);
        assert!(!app.now_playing().unwrap().loading);
        assert!(app.intent_track.is_none());
        assert_eq!(
            app.manual_queue,
            vec![rows[0].clone()],
            "starting the playlist preserves the separately queued copy"
        );
        app.note_listening();
        assert_eq!(app.listening.as_ref().unwrap().uri, rows[0]);
        app.backend.shutdown();
    }

    #[test]
    fn playlist_play_without_its_prefix_requests_position_zero() {
        for base_offset in [None, Some(0), Some(6_900)] {
            let mut app = headless_app();
            if let Some(base_offset) = base_offset {
                app.playlist_pages.insert(
                    "large".into(),
                    PlaylistPage {
                        items: PagedList {
                            base_offset,
                            items: if base_offset == 0 {
                                Vec::new()
                            } else {
                                vec![cached_playlist_row("spotify:track:middle")]
                            },
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                );
            }
            app.play_request(PlayRequest::context("spotify:playlist:large"), false);
            let request = app.queued_play.as_ref().unwrap();
            assert_eq!(request.offset_uri, None, "base offset {base_offset:?}");
            assert_eq!(request.offset_position, Some(0));
            let load = local_load(request, false);
            assert_eq!(load.context_uri.as_deref(), Some("spotify:playlist:large"));
            assert_eq!(load.offset_index, Some(0));
            assert!(load.uris.is_empty());
        }
    }

    #[test]
    fn playlist_play_skips_missing_local_and_unavailable_prefix_rows() {
        let mut app = headless_app();
        let mut local = cached_playlist_row("spotify:local:artist:album:track");
        local.is_local = true;
        let mut unavailable = cached_playlist_row("spotify:track:unavailable");
        let Some(PlayableItem::Track(track)) = unavailable.item.as_mut() else {
            panic!("a track fixture");
        };
        track.is_playable = Some(false);
        app.playlist_pages.insert(
            "playlist".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        Default::default(),
                        local,
                        unavailable,
                        cached_playlist_row("spotify:track:first"),
                        cached_playlist_row("spotify:track:second"),
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        app.play_request(PlayRequest::context("spotify:playlist:playlist"), false);
        assert_eq!(
            app.queued_play.as_ref().unwrap().offset_uri.as_deref(),
            Some("spotify:track:first")
        );
    }

    #[test]
    fn playlist_start_keeps_explicit_rows_and_shuffle_choices() {
        let mut app = headless_app();
        app.playlist_pages.insert(
            "playlist".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![cached_playlist_row("spotify:track:first")],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let context = "spotify:playlist:playlist";
        for shuffle in [false, true] {
            app.shuffle_wanted = shuffle;
            for request in [
                PlayRequest::context(context).starting_at_uri("spotify:track:chosen"),
                PlayRequest::context(context).starting_at_index(7),
                PlayRequest::tracks(vec!["spotify:track:sorted-first".into()]).starting_at_index(0),
            ] {
                app.play_request(request.clone(), false);
                let queued = app.queued_play.as_ref().unwrap();
                assert_eq!(queued.offset_uri, request.offset_uri);
                assert_eq!(queued.offset_position, request.offset_position);
                assert_eq!(queued.uris, request.uris);
            }
        }
        // With no loaded prefix, local shuffle must still choose its own
        // random start, rather than receiving the unshuffled position zero.
        app.playlist_pages.clear();
        app.play_request(PlayRequest::context(context), false);
        let request = app.queued_play.as_ref().unwrap();
        assert_eq!(request.offset_uri, None);
        assert_eq!(request.offset_position, None);
    }

    #[test]
    fn failed_proxy_migration_preserves_the_original_until_storage_is_confirmed() {
        let mut app = test_app("proxy-migration-settings");
        app.offline = false;
        std::fs::create_dir_all(&app.dirs.config).unwrap();
        let original = r#"{"proxy_mode":"http","proxy_host":"127.0.0.1","proxy_port":"8080","proxy_password":"dummy-legacy-secret"}"#;
        std::fs::write(app.dirs.settings_file(), original).unwrap();
        app.settings = Settings::load(&app.dirs.settings_file());
        app.applied_proxy_preferences = app.settings.proxy_preferences();
        app.settings.theme = crate::settings::ThemeChoice::Light;
        app.settings.proxy_host = "other.example".into();
        app.save_settings();
        assert_eq!(
            std::fs::read_to_string(app.dirs.settings_file()).unwrap(),
            original
        );
        assert!(
            !app.settings_dirty,
            "do not retry every frame while storage is pending"
        );
        app.handle_proxy_password_stored();
        let saved = Settings::load(&app.dirs.settings_file());
        assert_eq!(saved.theme, crate::settings::ThemeChoice::Light);
        assert_eq!(
            saved.proxy_host, "127.0.0.1",
            "unapplied draft is not saved"
        );
        assert!(saved.proxy_password.is_empty());
        assert!(!saved.proxy_password_legacy);
        assert!(
            !std::fs::read_to_string(app.dirs.settings_file())
                .unwrap()
                .contains("dummy-legacy-secret")
        );
    }

    #[test]
    fn a_separate_legacy_password_cannot_be_rebound_by_saving_a_new_address() {
        let seed = test_app("proxy-migration-separate");
        let dirs = seed.dirs.clone();
        drop(seed);
        std::fs::create_dir_all(&dirs.state).unwrap();
        let settings = Settings {
            proxy_mode: crate::settings::ProxyMode::Http,
            proxy_host: "127.0.0.1".into(),
            proxy_port: "8080".into(),
            ..Default::default()
        };
        settings.save(&dirs.settings_file());
        std::fs::write(dirs.proxy_secret_file(), "dummy-legacy-secret").unwrap();
        let original = std::fs::read(dirs.settings_file()).unwrap();
        let mut app = App::new(
            &Waker::default(),
            dirs,
            settings,
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        assert!(app.settings.proxy_password_legacy);
        app.settings.proxy_host = "other.example".into();
        app.request_proxy(false);
        app.handle_proxy_applied(
            app.proxy_request,
            app.settings.proxy_config().unwrap(),
            Ok(false),
        );
        assert_eq!(std::fs::read(app.dirs.settings_file()).unwrap(), original);
        // An unlocked native store can finish the launch-time migration while
        // this test runs, in which case deleting the plaintext file is right.
        // If migration is still pending, changing the address must leave the
        // only surviving copy untouched.
        match std::fs::read_to_string(app.dirs.proxy_secret_file()) {
            Ok(password) => assert_eq!(password, "dummy-legacy-secret"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("could not read the legacy proxy password: {error}"),
        }
    }

    #[test]
    fn late_proxy_password_restore_respects_form_edits_and_applied_settings() {
        for edited in [false, true] {
            let mut app = test_app(if edited {
                "proxy-restored-edit"
            } else {
                "proxy-restored-initial"
            });
            app.settings.proxy_mode = crate::settings::ProxyMode::Http;
            app.settings.proxy_host = "127.0.0.1".into();
            app.settings.proxy_port = "8080".into();
            let mut stored = app.settings.clone();
            stored.proxy_password = "dummy-saved-password".into();
            app.proxy_form_edited = edited;
            app.handle_proxy_restored(
                stored.proxy_config().unwrap(),
                stored.proxy_password_record().unwrap(),
            );
            assert_eq!(app.settings.proxy_password.is_empty(), edited);
            app.settings.proxy_mode = crate::settings::ProxyMode::Off;
            app.request_proxy(false);
            app.handle_proxy_applied(
                app.proxy_request,
                crate::settings::ProxyConfig::Off,
                Ok(false),
            );
            app.settings.proxy_password.clear();
            app.handle_proxy_restored(
                stored.proxy_config().unwrap(),
                stored.proxy_password_record().unwrap(),
            );
            assert_eq!(app.applied_proxy, crate::settings::ProxyConfig::Off);
            assert!(app.settings.proxy_password.is_empty());
        }
    }

    #[test]
    fn a_rejected_proxy_change_does_not_announce_success() {
        let mut app = test_app("proxy-rejected");
        app.backend.set_offline(true);
        let original = app.applied_proxy.clone();
        let failed = crate::settings::ProxyConfig::Invalid("Proxy port must be a number".into());
        app.request_proxy(false);
        app.handle_proxy_applied(
            app.proxy_request,
            failed,
            Err("Unable to build the configured client".into()),
        );
        assert_eq!(app.applied_proxy, original);
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("could not be applied"))
        );
        app.backend.shutdown();
    }

    #[test]
    fn saving_other_settings_keeps_proxy_drafts_out_of_the_next_launch() {
        let mut app = test_app("proxy-persisted-draft");
        app.backend.set_offline(true);
        app.settings.proxy_mode = crate::settings::ProxyMode::Http;
        app.settings.proxy_host = "127.0.0.1".into();
        app.settings.proxy_port = "8080".into();
        app.settings.proxy_password = "dummy-private-password".into();
        app.settings.theme = ThemeChoice::Light;
        app.save_settings();
        let saved = Settings::load(&app.dirs.settings_file());
        assert_eq!(saved.proxy_mode, crate::settings::ProxyMode::System);
        assert_eq!(saved.theme, ThemeChoice::Light);
        assert!(
            !std::fs::read_to_string(app.dirs.settings_file())
                .unwrap()
                .contains("dummy-private-password")
        );
        assert!(
            !app.dirs.proxy_secret_file().exists(),
            "never write a plaintext password beside settings"
        );
        app.request_proxy(false);
        let request = app.proxy_request;
        let accepted = app.settings.proxy_config().unwrap();
        // An acknowledgement for port 8080 arrives after another form edit.
        app.settings.proxy_port = "8090".into();
        app.handle_proxy_applied(request, accepted, Ok(false));
        assert_eq!(
            app.settings.proxy_port, "8090",
            "keep the draft being edited"
        );
        assert_eq!(Settings::load(&app.dirs.settings_file()).proxy_port, "8080");
        app.backend.shutdown();
    }

    #[test]
    fn an_old_proxy_acknowledgement_cannot_replace_a_newer_applied_policy() {
        let mut app = test_app("proxy-stale-ack");
        app.backend.set_offline(true);
        app.settings.proxy_mode = crate::settings::ProxyMode::Http;
        app.settings.proxy_host = "127.0.0.1".into();
        app.settings.proxy_port = "8080".into();
        app.request_proxy(false);
        let first = app.proxy_request;
        let old = app.settings.proxy_config().unwrap();
        app.settings.proxy_port = "8090".into();
        app.request_proxy(false);
        let newest = app.settings.proxy_config().unwrap();
        app.handle_proxy_applied(app.proxy_request, newest.clone(), Ok(false));
        app.handle_proxy_applied(first, old, Ok(false));
        assert_eq!(app.applied_proxy, newest);
        assert_eq!(Settings::load(&app.dirs.settings_file()).proxy_port, "8090");
        app.backend.shutdown();
    }

    #[test]
    fn manual_proxy_edits_remain_drafts_until_apply() {
        let mut app = test_app("proxy-draft");
        app.backend.set_offline(true);
        let original = app.applied_proxy.clone();
        app.settings.proxy_mode = crate::settings::ProxyMode::Http;
        app.settings.proxy_host = "127.0.0.1".into();
        app.settings.proxy_port = "8080".into();

        app.actions.push(Action::RestartEngine);
        app.apply_actions(&egui::Context::default());
        assert_eq!(app.applied_proxy, original);

        app.actions.push(Action::ApplyProxy);
        app.apply_actions(&egui::Context::default());
        assert_eq!(
            app.applied_proxy, original,
            "wait for the transport to build"
        );
        app.handle_proxy_applied(
            app.proxy_request,
            app.settings.proxy_config().unwrap(),
            Ok(true),
        );
        assert!(matches!(
            app.applied_proxy,
            crate::settings::ProxyConfig::Http(_)
        ));
    }

    /// Shuffle picks a random loaded track or Web API offset. Local librespot
    /// playback chooses its own starting track.
    #[test]
    fn a_shuffled_play_does_not_start_at_track_one() {
        use crate::api::models::{
            Album, PlayableItem, Playlist, PlaylistItem, SavedAlbum, Track, TrackCount,
        };
        let track = |uri: &str| {
            Some(PlayableItem::Track(Track {
                uri: uri.into(),
                ..Default::default()
            }))
        };
        let mut app = headless_app();
        app.library.playlists = Loadable::Loaded(vec![
            Playlist {
                uri: "spotify:playlist:open".into(),
                tracks: Some(TrackCount { total: 3 }),
                ..Default::default()
            },
            Playlist {
                uri: "spotify:playlist:unopened".into(),
                tracks: Some(TrackCount { total: 57 }),
                ..Default::default()
            },
        ]);
        app.library.albums.items = vec![SavedAlbum {
            album: Album {
                uri: "spotify:album:saved".into(),
                total_tracks: Some(12),
                ..Default::default()
            },
            ..Default::default()
        }];
        app.library.liked.total = Some(9);
        app.playlist_pages.insert(
            "open".into(),
            PlaylistPage {
                items: PagedList {
                    items: vec![
                        PlaylistItem {
                            item: track("spotify:track:one"),
                            ..Default::default()
                        },
                        PlaylistItem {
                            item: track("spotify:track:two"),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        // Playing on a phone: the Web API needs the offset.
        app.selected_device = Some("phone".into());
        assert!(matches!(app.target(), Target::Remote(Some(_))));

        // The playlist on screen: the start is one of its own rows.
        let (uri, position) = app.shuffle_start("spotify:playlist:open");
        assert!(
            matches!(
                uri.as_deref(),
                Some("spotify:track:one" | "spotify:track:two")
            ),
            "the start comes from the rows, got {uri:?}"
        );
        assert_eq!(position, None);

        // Begun from the sidebar or a menu, with no rows loaded: a
        // position inside the length the library reported.
        for (context, len) in [
            ("spotify:playlist:unopened", 57),
            ("spotify:album:saved", 12),
            ("spotify:user:someone:collection", 9),
        ] {
            for _ in 0..50 {
                let (uri, position) = app.shuffle_start(context);
                assert_eq!(uri, None, "{context} has no rows to name");
                let position = position.unwrap_or_else(|| panic!("{context} got no offset"));
                assert!(
                    position < len,
                    "{context} offset {position} is outside {len}"
                );
            }
        }
        // Over 50 draws a 57-song playlist should not have sat still on
        // one song, let alone on the first.
        let drawn: std::collections::HashSet<Option<u32>> = (0..50)
            .map(|_| app.shuffle_start("spotify:playlist:unopened").1)
            .collect();
        assert!(drawn.len() > 1, "the starting position never moved");

        // Nothing saved, nothing loaded: no offset to give.
        assert_eq!(app.shuffle_start("spotify:playlist:unknown"), (None, None));

        // Local playback: librespot picks the starting track itself.
        app.selected_device = None;
        assert!(matches!(app.target(), Target::Local));
        assert_eq!(
            app.shuffle_start("spotify:playlist:unopened"),
            (None, None),
            "librespot is left to draw its own"
        );
    }

    /// A Free account is told once per sign-in that nothing will play;
    /// a Premium one is not bothered.
    #[test]
    fn a_free_account_is_told_once_that_it_cannot_play() {
        let me = |product: &str| {
            ApiResponse::Me(Ok(crate::api::models::User {
                id: "someone".into(),
                product: Some(product.into()),
                ..Default::default()
            }))
        };
        let mut app = headless_app();
        app.handle_api(me("free"));
        assert!(matches!(app.dialog, Some(Dialog::PremiumNeeded)));
        app.dialog = None;
        app.handle_api(me("free"));
        assert!(app.dialog.is_none(), "the notice is shown once");

        let mut app = headless_app();
        app.handle_api(me("premium"));
        assert!(app.dialog.is_none());
    }

    /// Only the personal playlists themselves belong on the shelf, not
    /// what Spotify generates for an artist who took one of their names.
    #[test]
    fn the_shelf_takes_the_playlist_and_not_an_artist_named_after_it() {
        assert!(is_made_for_you("Discover Weekly", "Discover Weekly"));
        assert!(is_made_for_you("release radar", "Release Radar"));
        assert!(is_made_for_you("daylist", "daylist"));
        assert!(is_made_for_you("Daily Mix 3", "Daily Mix"));
        assert!(is_made_for_you("Daily Mix", "Daily Mix"));
        assert!(!is_made_for_you("Discover Weekly Mix", "Discover Weekly"));
        assert!(!is_made_for_you(
            "This Is Discover Weekly",
            "Discover Weekly"
        ));
        assert!(!is_made_for_you("Release Radar Radio", "Release Radar"));
        assert!(!is_made_for_you("Daily Mix Radio", "Daily Mix"));
        assert!(!is_made_for_you("Daily Mix 3", "Discover Weekly"));
    }

    /// One song plays as a context of its own, so librespot's autoplay
    /// follows it; a list stays a list.
    #[test]
    fn one_song_is_loaded_as_a_context() {
        let one = local_load(&PlayRequest::tracks(vec!["spotify:track:a".into()]), false);
        assert_eq!(one.context_uri.as_deref(), Some("spotify:track:a"));
        assert!(one.uris.is_empty() && !one.autoplay);
        let two = local_load(
            &PlayRequest::tracks(vec!["spotify:track:a".into(), "spotify:track:b".into()])
                .starting_at_index(1),
            true,
        );
        assert_eq!(two.context_uri, None);
        assert_eq!(two.uris.len(), 2);
        assert_eq!(two.offset_index, Some(1));
        // A chosen row keeps the list load straight; shuffle follows as a
        // command (see a_chosen_row_in_a_list_never_loads_shuffled).
        assert_eq!(two.shuffle, None);
        let episode = local_load(
            &PlayRequest::tracks(vec!["spotify:episode:e".into()]),
            false,
        );
        assert_eq!(episode.context_uri, None);
        assert_eq!(episode.uris.len(), 1);
    }

    /// A plain list that plays out seeds autoplay with its last song; a
    /// stop anywhere else, a dropped session, or autoplay off does not.
    #[test]
    fn a_list_that_ends_seeds_autoplay_with_its_last_song() {
        let track = |uri: &str| {
            Some(crate::player::LocalTrack {
                uri: uri.into(),
                duration_ms: 200_000,
                ..Default::default()
            })
        };
        let playing = LocalState {
            playback: Playback::Playing,
            track: track("spotify:track:last"),
            position_ms: 198_500,
            connected: true,
            ..LocalState::default()
        };
        let stopped = LocalState {
            playback: Playback::Stopped,
            track: track("spotify:track:last"),
            connected: true,
            ..LocalState::default()
        };
        let list: Vec<String> = vec!["spotify:track:first".into(), "spotify:track:last".into()];
        assert_eq!(
            autoplay_seed(Some(&list), true, &playing, &stopped).as_deref(),
            Some("spotify:track:last")
        );
        assert_eq!(autoplay_seed(Some(&list), false, &playing, &stopped), None);
        assert_eq!(autoplay_seed(None, true, &playing, &stopped), None);
        let mid_song = LocalState {
            position_ms: 60_000,
            ..playing.clone()
        };
        assert_eq!(autoplay_seed(Some(&list), true, &mid_song, &stopped), None);
        let not_last = LocalState {
            track: track("spotify:track:first"),
            ..playing.clone()
        };
        assert_eq!(autoplay_seed(Some(&list), true, &not_last, &stopped), None);
        let dropped = LocalState {
            connected: false,
            ..stopped.clone()
        };
        assert_eq!(autoplay_seed(Some(&list), true, &playing, &dropped), None);
    }

    fn snapshot_at(percent: u8) -> LocalState {
        LocalState {
            volume: percent_to_volume(percent),
            ..LocalState::default()
        }
    }

    #[test]
    fn a_volume_set_here_is_saved_immediately() {
        let mut app = headless_app();
        app.set_volume(80, true);
        assert_eq!(volume_to_percent(app.settings.volume), 80);
        assert!(app.settings_dirty);
    }

    #[test]
    fn a_stale_engine_snapshot_does_not_pull_the_volume_back() {
        let mut app = headless_app();
        app.set_volume(80, true);

        // The engine reports `VolumeChanged` asynchronously, so its next
        // snapshot still carries the volume from before the change.
        app.handle_local(snapshot_at(20));
        assert_eq!(volume_to_percent(app.local.volume), 80);
        assert_eq!(volume_to_percent(app.settings.volume), 80);

        // Once it has caught up, its snapshots are trusted again.
        app.handle_local(snapshot_at(80));
        assert_eq!(volume_to_percent(app.local.volume), 80);
    }

    #[test]
    fn a_volume_changed_outside_the_app_is_adopted() {
        let mut app = headless_app();
        app.handle_local(snapshot_at(35));
        assert_eq!(volume_to_percent(app.local.volume), 35);
        assert_eq!(volume_to_percent(app.settings.volume), 35);
    }

    /// What a Raycast script sends becomes the same action a menu pick or a
    /// media key would produce.
    #[test]
    fn a_control_command_becomes_the_action_it_names() {
        // #given
        let mut app = headless_app();
        let queue: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> = Default::default();
        app.control_commands = Some(std::sync::Arc::clone(&queue));

        // #when
        queue.lock().expect("the queue").extend([
            ControlCommand::Next,
            ControlCommand::Previous,
            ControlCommand::SeekBy(-15_000),
            ControlCommand::VolumeBy(10),
            ControlCommand::SetVolume(240),
            ControlCommand::ToggleShuffle,
            ControlCommand::Show,
        ]);
        app.handle_control_commands();

        // #then
        assert!(
            matches!(
                app.actions.as_slice(),
                [
                    Action::Next,
                    Action::Previous,
                    Action::SeekBy(-15_000),
                    Action::VolumeBy(10),
                    // A percentage above the scale is clamped, not wrapped.
                    Action::SetVolume(100),
                    Action::ToggleShuffle,
                    Action::ShowWindow,
                ]
            ),
            "{:?}",
            app.actions
        );
        assert!(queue.lock().expect("the queue").is_empty());
    }

    /// Control clients can set state, seek, play a URI, and transfer playback.
    #[test]
    fn a_key_can_ask_for_a_state_rather_than_a_toggle() {
        // #given
        let mut app = headless_app();
        let queue: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> = Default::default();
        app.control_commands = Some(std::sync::Arc::clone(&queue));

        // #when
        queue.lock().expect("the queue").extend([
            ControlCommand::SetShuffle(true),
            ControlCommand::SetRepeat(RepeatMode::Track),
            ControlCommand::SeekTo(90_000),
            ControlCommand::PlayUri("spotify:playlist:pl1".to_owned()),
            ControlCommand::Transfer("abc123".to_owned()),
            ControlCommand::RefreshDevices,
            // Nothing is playing in a headless app, so there is no track to
            // save and this one falls away rather than erroring.
            ControlCommand::ToggleSaved,
        ]);
        app.handle_control_commands();

        // #then
        assert!(
            matches!(
                app.actions.as_slice(),
                [
                    Action::SetShuffle(true),
                    Action::SetRepeat(RepeatMode::Track),
                    Action::Seek(90_000),
                    Action::PlayContext {
                        offset_uri: None,
                        offset_index: None,
                        ..
                    },
                    Action::Transfer(_),
                    Action::RefreshDevices,
                ]
            ),
            "{:?}",
            app.actions
        );
        assert!(queue.lock().expect("the queue").is_empty());
    }

    /// New snapshot fields are appended so older clients keep working.
    #[test]
    fn the_snapshot_appends_what_a_key_needs_without_moving_what_was_there() {
        // #given
        let mut app = headless_app();
        app.handle_local(LocalState {
            playback: Playback::Playing,
            track: Some(crate::player::LocalTrack {
                uri: "spotify:track:t1".to_owned(),
                title: "Go".to_owned(),
                artists: vec![ArtistRef {
                    name: "The Band".to_owned(),
                    ..ArtistRef::default()
                }],
                album: "First".to_owned(),
                art_url: Some("https://i.scdn.co/image/abc".to_owned()),
                duration_ms: 200_000,
                ..Default::default()
            }),
            position_ms: 20_000,
            volume: percent_to_volume(35),
            shuffle: true,
            repeat: RepeatMode::Track,
            ..LocalState::default()
        });

        // #when
        let snapshot = app.control_snapshot();
        let fields: Vec<&str> = snapshot.split('\t').collect();

        // #then
        assert_eq!(
            fields,
            [
                // The nine a media key or a Raycast script already read.
                "playing",
                "Go",
                "The Band",
                "First",
                "20000",
                "200000",
                "35",
                "on",
                "track",
                // The three a Stream Deck key needs, appended.
                "https://i.scdn.co/image/abc",
                // Saved state is unknown before sign-in.
                "unknown",
                // Local playback is this computer, which Spotify has not
                // named because it is not a remote device.
                "Spotifast",
            ]
        );
        // No devices seen yet is an empty array, not an empty string, so a
        // client never special-cases the answer.
        assert_eq!(
            app.control_devices_snapshot(),
            crate::single_instance::NO_DEVICES
        );
    }

    /// Spotify device responses update the control snapshot.
    #[test]
    fn a_device_list_reaches_the_slot_when_spotify_answers() {
        // #given
        let mut app = headless_app();
        let slot = std::sync::Arc::new(std::sync::Mutex::new(
            crate::single_instance::NO_DEVICES.to_owned(),
        ));
        app.control_devices = Some(std::sync::Arc::clone(&slot));
        app.control_devices_stale = false;

        // #when
        app.handle_api(ApiResponse::Devices(Ok(vec![Device {
            id: Some("abc123".to_owned()),
            name: "Kitchen\tspeaker".to_owned(),
            kind: "Speaker".to_owned(),
            is_active: true,
            ..Device::default()
        }])));
        app.sync_media_controls(&egui::Context::default());

        // #then
        let written = slot.lock().expect("the slot").clone();
        assert_eq!(
            written,
            r#"[{"active":true,"id":"abc123","kind":"Speaker","name":"Kitchen\tspeaker"}]"#,
            "a name is carried whole, tab and all, because JSON escapes it \
             where the tab-separated snapshot could not"
        );
        // Written once and not again until the next answer.
        assert!(!app.control_devices_stale);
    }

    /// `play` and `pause` say what state to end in, so the one that would
    /// undo the current state does nothing.
    #[test]
    fn play_and_pause_do_not_toggle_the_wrong_way() {
        let mut app = headless_app();
        let queue: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> = Default::default();
        app.control_commands = Some(std::sync::Arc::clone(&queue));

        // Nothing is playing in a headless app, so `pause` has nothing to do
        // and `play` asks for the toggle.
        queue
            .lock()
            .expect("the queue")
            .extend([ControlCommand::Pause, ControlCommand::Play]);
        app.handle_control_commands();

        assert!(
            matches!(app.actions.as_slice(), [Action::TogglePlay]),
            "{:?}",
            app.actions
        );
    }

    /// A skin with a bitmap in it, for pretending one was read.
    fn some_skin(name: &str) -> crate::skin::Skin {
        let image = image::RgbImage::from_pixel(275, 116, image::Rgb([9, 9, 9]));
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let archive = crate::skin::zip::write(&[("main.bmp", png.get_ref(), false)]);
        crate::skin::Skin::from_archive(name, &archive).unwrap()
    }

    #[test]
    fn a_skin_read_late_does_not_override_a_newer_choice() {
        let mut app = headless_app();
        app.settings.winamp_window = true;
        app.settings.skin = Some("B.wsz".into());
        app.skin_loaded(crate::winamp::Loaded {
            name: "A.wsz".into(),
            result: Ok(some_skin("A")),
            installed: false,
        });
        assert_eq!(app.winamp.worn.as_deref(), Some("A.wsz"));
        assert_eq!(app.settings.skin.as_deref(), Some("B.wsz"));
    }

    #[test]
    fn a_dropped_skin_becomes_the_choice_and_a_failed_one_is_forgotten() {
        let mut app = headless_app();
        app.settings.winamp_window = true;
        app.skin_loaded(crate::winamp::Loaded {
            name: "Dropped.wsz".into(),
            result: Ok(some_skin("Dropped")),
            installed: true,
        });
        assert_eq!(app.settings.skin.as_deref(), Some("Dropped.wsz"));
        assert_eq!(app.winamp.worn.as_deref(), Some("Dropped.wsz"));
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message == "Added Dropped skin")
        );

        app.settings.skin = Some("Gone.wsz".into());
        app.skin_loaded(crate::winamp::Loaded {
            name: "Gone.wsz".into(),
            result: Err(crate::skin::SkinError::Empty),
            installed: false,
        });
        assert_eq!(app.settings.skin.as_deref(), Some("Dropped.wsz"));
        assert_eq!(app.winamp.worn.as_deref(), Some("Dropped.wsz"));
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.starts_with("Gone: "))
        );
    }

    /// A link from outside waits for the account and then opens its page;
    /// a song's link opens the album the song is on.
    #[test]
    fn a_link_waits_for_the_account_and_then_opens_its_page() {
        use crate::api::models::{Album, Track};

        // #given a signed-out app handed a playlist link
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.actions
            .push(Action::OpenLink("spotify:playlist:pl1".into()));
        app.apply_actions(&ctx);

        // #then the window is asked for but the page waits
        assert_eq!(*app.page(), Page::Home);
        assert_eq!(app.pending_link.as_deref(), Some("spotify:playlist:pl1"));

        // #when the account arrives
        app.user = Some(User {
            id: "me".into(),
            ..User::default()
        });
        app.open_pending_link();

        // #then the playlist opens, once
        assert_eq!(*app.page(), Page::Playlist("pl1".into()));
        assert_eq!(app.pending_link, None);

        // #when a song whose album is known is linked
        app.track_cache.insert(
            "t1".into(),
            Track {
                id: Some("t1".into()),
                uri: "spotify:track:t1".into(),
                album: Some(Album {
                    id: "al1".into(),
                    ..Album::default()
                }),
                ..Track::default()
            },
        );
        app.actions
            .push(Action::OpenLink("spotify:track:t1".into()));
        app.apply_actions(&ctx);

        // #then its album's page opens
        assert_eq!(*app.page(), Page::Album("al1".into()));
        assert_eq!(app.pending_link, None);

        // #when a song still unknown is linked
        app.actions
            .push(Action::OpenLink("spotify:track:t2".into()));
        app.apply_actions(&ctx);

        // #then the link waits for Spotify's answer rather than guessing
        assert_eq!(*app.page(), Page::Album("al1".into()));
        assert_eq!(app.pending_link.as_deref(), Some("spotify:track:t2"));
        assert!(app.track_requests.contains("t2"));

        // #when Spotify has no such song
        app.handle_api(ApiResponse::Track {
            id: "t2".into(),
            result: Err(crate::api::client::ApiError::Status {
                status: 404,
                message: "not found".into(),
            }),
        });

        // #then the link is given up on and the user told
        assert_eq!(app.pending_link, None);
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("Cannot open"))
        );
    }

    /// A link to something the app has no page for is refused with a word,
    /// not held forever.
    #[test]
    fn a_link_to_nothing_the_app_shows_says_so() {
        // #given
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.user = Some(User {
            id: "me".into(),
            ..User::default()
        });

        // #when
        app.actions
            .push(Action::OpenLink("spotify:station:track:t1".into()));
        app.apply_actions(&ctx);

        // #then
        assert_eq!(app.pending_link, None);
        assert_eq!(*app.page(), Page::Home);
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("cannot open"))
        );
    }

    #[test]
    fn search_links_wait_for_sign_in_and_open_the_latest_query_once() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        for link in [
            "https://open.spotify.com/search/old",
            "https://open.spotify.com/search/artist%3ABj%C3%B6rk",
        ] {
            app.open_link(crate::link::parse(link).unwrap());
            app.apply_actions(&ctx);
        }
        assert_eq!(*app.page(), Page::Home);
        assert!(app.search.query.is_empty());
        assert!(app.pending_link.is_some());

        app.user = Some(User {
            id: "me".into(),
            ..User::default()
        });
        app.open_pending_link();
        app.apply_actions(&ctx);
        assert_eq!(*app.page(), Page::Search);
        assert_eq!(app.search.query, "artist:Björk");
        assert_eq!(app.search.committed, "artist:Björk");
        assert!(app.search.focus_requested);
        assert!(app.pending_link.is_none());
        assert!(app.now_playing().is_none());
        let serial = app.search.serial;
        app.open_pending_link();
        app.apply_actions(&ctx);
        assert_eq!(app.search.serial, serial);

        app.open_link(crate::link::parse("https://open.spotify.com/search").unwrap());
        app.apply_actions(&ctx);
        assert_eq!(*app.page(), Page::Search);
        assert!(app.search.query.is_empty());
        assert!(app.search.committed.is_empty());
        assert!(matches!(app.search.results, Loadable::NotLoaded));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mpris_search_links_open_search_on_a_private_bus() {
        use std::time::{Duration, Instant};
        const CHILD: &str = "SPOTIFAST_SEARCH_PRIVATE_BUS";
        if std::env::var_os(CHILD).is_none() {
            let root = std::env::temp_dir().join(format!(
                "spotifast-search-bus-{:016x}",
                rand::random::<u64>()
            ));
            std::fs::create_dir(&root).unwrap();
            let config = root.join("session.conf");
            std::fs::write(&config, r#"<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth><policy context="default"><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy></busconfig>"#).unwrap();
            let result = std::process::Command::new("dbus-run-session")
                .arg("--config-file")
                .arg(config)
                .arg("--")
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::tests::mpris_search_links_open_search_on_a_private_bus",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            std::fs::remove_dir_all(root).unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let mut app = headless_app();
        app.user = Some(User {
            id: "me".into(),
            ..User::default()
        });
        app.media_controls = Some(MediaService::spawn(|| {}));
        let client = zbus::blocking::connection::Builder::session()
            .unwrap()
            .method_timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let call = |uri: &str| {
            client.call_method(
                Some("org.mpris.MediaPlayer2.spotifast"),
                "/org/mpris/MediaPlayer2",
                Some("org.mpris.MediaPlayer2.Player"),
                "OpenUri",
                &(uri,),
            )
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Err(error) = call("https://open.spotify.com/search/here%20comes%20the%20sun") {
            assert!(Instant::now() < deadline, "MPRIS did not start: {error}");
            std::thread::sleep(Duration::from_millis(10));
        }
        let wait_for_command = |app: &mut App| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.actions.is_empty() {
                app.handle_media_commands();
                assert!(
                    Instant::now() < deadline,
                    "MPRIS command did not reach the app"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        wait_for_command(&mut app);
        assert!(matches!(app.actions.as_slice(), [Action::OpenLink(_)]));
        let schemes: Vec<String> = zbus::blocking::Proxy::new(
            &client,
            "org.mpris.MediaPlayer2.spotifast",
            "/org/mpris/MediaPlayer2",
            "org.mpris.MediaPlayer2",
        )
        .unwrap()
        .get_property("SupportedUriSchemes")
        .unwrap();
        assert!(schemes.iter().any(|scheme| scheme == "https"));
        app.apply_actions(&egui::Context::default());
        assert_eq!(*app.page(), Page::Search);
        assert_eq!(app.search.query, "here comes the sun");
        assert_eq!(app.search.committed, "here comes the sun");
        assert!(app.now_playing().is_none());

        call("spotify:playlist:unchanged").unwrap();
        wait_for_command(&mut app);
        assert!(
            matches!(app.actions.as_slice(), [Action::PlayContext { uri, offset_uri: None, offset_index: None }] if uri == "spotify:playlist:unchanged")
        );
    }

    /// A playlist shared by invitation takes songs once Spotify's rootlist
    /// says so, though the Web API calls it neither owned nor collaborative.
    #[test]
    fn a_playlist_shared_by_invitation_takes_songs() {
        // #given a friend's playlist in the library
        let mut app = headless_app();
        app.user = Some(User {
            id: "me".into(),
            ..User::default()
        });
        let theirs = Playlist {
            id: "shared".into(),
            name: "the Best Music Ever".into(),
            uri: "spotify:playlist:shared".into(),
            owner: crate::api::models::Owner {
                id: Some("friend".into()),
                ..Default::default()
            },
            ..Playlist::default()
        };
        let mut mine = theirs.clone();
        mine.id = "mine".into();
        mine.uri = "spotify:playlist:mine".into();
        mine.owner.id = Some("me".into());
        let mut public = theirs.clone();
        public.id = "public".into();
        public.uri = "spotify:playlist:public".into();
        app.library.playlists = Loadable::Loaded(vec![theirs.clone(), mine.clone(), public]);

        // #then only the account's own takes songs before Spotify's word
        assert!(app.can_edit_playlist(&mine));
        assert!(!app.can_edit_playlist(&theirs));
        assert_eq!(
            app.editable_playlists(),
            vec![("mine".to_string(), "the Best Music Ever".to_string())]
        );

        // #when the rootlist names the friend's playlist among the editable
        app.handle_backend_events(vec![Event::Rootlist {
            result: Ok(crate::player::Rootlist {
                entries: vec![crate::player::RootlistEntry::Playlist(
                    "spotify:playlist:shared".into(),
                )],
                editable: ["spotify:playlist:shared".to_string()]
                    .into_iter()
                    .collect(),
            }),
        }]);

        // #then it takes songs, and the followed public one still does not
        assert!(app.can_edit_playlist(&theirs));
        let editable: Vec<String> = app
            .editable_playlists()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(editable, ["shared", "mine"]);
    }

    /// Folder order survives a restart, but only for the account that
    /// supplied it; edit grants wait for a fresh session answer.
    #[test]
    fn the_last_playlist_tree_stays_visible_for_its_account() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-rootlist-restart-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let options = || AppOptions {
            media_controls: false,
            restore_sign_in: false,
            tray: false,
        };
        let entries = vec![
            crate::player::RootlistEntry::FolderStart {
                id: "folder".into(),
                name: "Favorites".into(),
            },
            crate::player::RootlistEntry::Playlist("spotify:playlist:one".into()),
            crate::player::RootlistEntry::FolderEnd,
        ];
        let mut app = App::new(
            &Waker::default(),
            dirs.clone(),
            Settings::default(),
            options(),
        );
        app.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        app.user = Some(User {
            id: "listener".into(),
            ..User::default()
        });
        app.handle_backend_events(vec![Event::Rootlist {
            result: Ok(crate::player::Rootlist {
                entries: entries.clone(),
                editable: ["spotify:playlist:one".to_string()].into_iter().collect(),
            }),
        }]);
        assert!(app.session_dirty);
        app.save_session();
        drop(app);

        let mut restored = App::new(
            &Waker::default(),
            dirs.clone(),
            Settings::default(),
            options(),
        );
        restored.handle_api(ApiResponse::Me(Ok(User {
            id: "someone-else".into(),
            ..User::default()
        })));
        assert!(
            restored.rootlist.is_empty(),
            "another account sees no cached tree"
        );
        restored.handle_api(ApiResponse::Me(Ok(User {
            id: "listener".into(),
            ..User::default()
        })));
        assert_eq!(restored.rootlist, entries);
        assert!(
            restored.editable_by_grant.is_empty(),
            "cached order must not cache a stale edit grant"
        );
        restored.handle_auth(AuthStatus::SignedOut);
        assert!(restored.rootlist.is_empty());
        assert_eq!(
            restored.rootlist_cache, None,
            "signing out removes the account's cached tree"
        );
        drop(restored);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_closing_main_window_never_takes_the_mini_players_size() {
        let mut app = headless_app();
        let ctx = egui::Context::default();
        app.attach(&ctx);
        app.actions.push(Action::ToggleWinampWindow);
        app.apply_actions(&ctx);
        assert!(app.switch_intent && app.settings.winamp_window);

        // Native close events may leave another UI frame to draw. It still
        // belongs to the main window, whose geometry eframe will save.
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.frame_ui(ui));
        output.textures_delta.clear();
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            !commands.iter().any(|command| matches!(
                command,
                egui::ViewportCommand::InnerSize(_)
                    | egui::ViewportCommand::MinInnerSize(_)
                    | egui::ViewportCommand::MaxInnerSize(_)
                    | egui::ViewportCommand::Maximized(_)
            )),
            "the retiring main window must keep its geometry: {commands:?}"
        );
    }

    /// Switching from Winamp back to the main window preserves the main
    /// window's size and position across the closing mini-window frame.
    #[test]
    fn closing_winamp_frame_does_not_overwrite_main_window_geometry() {
        let mut app = headless_app();
        app.last_window_size = Some([1024.0, 768.0]);
        app.last_window_pos = Some([100.0, 150.0]);

        // Toggle from main window to Winamp window
        let ctx = egui::Context::default();
        app.actions.push(Action::ToggleWinampWindow);
        app.apply_actions(&ctx);

        assert!(app.settings.winamp_window);
        assert!(app.switch_intent);
        assert_eq!(app.session_window_size, Some([1024.0, 768.0]));
        assert_eq!(app.session_window_pos, Some([100.0, 150.0]));

        // Attach the Winamp window (clears switch_intent, keeps session geometry)
        app.attach(&ctx);
        assert!(!app.switch_intent);
        assert_eq!(app.session_window_size, Some([1024.0, 768.0]));

        // Trigger switch back to the main window
        app.actions.push(Action::ToggleWinampWindow);

        // Run the closing frame of the mini-window with its tiny viewport geometry
        let mut raw_input = egui::RawInput::default();
        let mini_rect = egui::Rect::from_min_size(egui::pos2(50.0, 50.0), egui::vec2(275.0, 116.0));
        let viewport = raw_input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default();
        viewport.inner_rect = Some(mini_rect);
        viewport.outer_rect = Some(mini_rect);

        let mut closing_output = ctx.run_ui(raw_input, |ui| {
            app.frame_ui(ui);
        });
        closing_output.textures_delta.clear();

        // The closing frame switched window mode and armed switch_intent...
        assert!(!app.settings.winamp_window);
        assert!(app.switch_intent);

        // ...but switch_intent prevented the closing mini-window rect from
        // overwriting the saved main window size and position.
        assert_eq!(app.last_window_size, Some([1024.0, 768.0]));
        assert_eq!(app.last_window_pos, Some([100.0, 150.0]));
        assert_eq!(app.session_window_size, Some([1024.0, 768.0]));
        assert_eq!(app.session_window_pos, Some([100.0, 150.0]));

        // Attaching the new main window restores the saved geometry via viewport commands
        let main_ctx = egui::Context::default();
        let mut output = main_ctx.run_ui(Default::default(), |_ui| {
            app.attach(&main_ctx);
        });
        output.textures_delta.clear();

        let commands = &output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("the root viewport")
            .commands;
        assert!(
            commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(1024.0, 768.0))),
            "attach restored the main window size: {commands:?}"
        );
        assert!(
            commands.contains(&egui::ViewportCommand::OuterPosition(egui::pos2(
                100.0, 150.0
            ))),
            "attach restored the main window position: {commands:?}"
        );
    }

    fn cached_liked_app() -> App {
        use crate::api::models::SavedTrack;
        let mut app = headless_app();
        app.backend.set_offline(true);
        app.user = Some(User {
            id: "alice".into(),
            ..Default::default()
        });
        app.auth = AuthStatus::Connected {
            username: "alice".into(),
        };
        app.liked_songs.cache_checked = true;
        app.load_generation = 1;
        app.liked_songs.start_refresh(1);
        app.liked_songs.absorb(
            0,
            crate::api::models::Page {
                items: (0..100)
                    .map(|n| SavedTrack {
                        track: Track {
                            uri: format!("spotify:track:{n}"),
                            id: Some(n.to_string()),
                            name: format!("Song {n}"),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .collect(),
                total: 100,
                limit: 100,
                ..Default::default()
            },
            jiff::Timestamp::now().as_second(),
        );
        app.sync_liked_songs();
        app
    }

    #[test]
    fn liked_cache_ignores_another_account_and_late_responses_after_sign_out() {
        let mut app = cached_liked_app();
        let cache = app.liked_songs.checkpoint("alice".into(), true).unwrap();
        app.reset_data();
        app.user = Some(User {
            id: "bob".into(),
            ..Default::default()
        });
        app.ensure_liked_songs();
        let generation = app.liked_songs.generation;
        app.receive_liked_cache("alice", generation, Some(cache.clone()));
        assert!(app.library.liked.items.is_empty());
        app.handle_api(ApiResponse::SavedTracks {
            offset: 0,
            generation,
            account_id: Some("alice".into()),
            result: Ok(crate::api::models::Page {
                items: vec![crate::api::models::SavedTrack::default()],
                ..Default::default()
            }),
        });
        assert!(app.library.liked.items.is_empty());
        app.reset_data();
        app.user = None;
        app.receive_liked_cache("bob", generation, Some(cache));
        assert!(!app.liked_songs.cache_checked);
        assert!(app.library.liked.items.is_empty());
    }

    #[test]
    fn liking_and_unliking_update_rows_before_the_network_answers() {
        let mut app = cached_liked_app();
        app.set_saved("spotify:track:0".into(), false);
        assert_eq!(app.library.liked.items.len(), 99);
        assert!(
            !app.library
                .liked
                .items
                .iter()
                .any(|item| item.track.uri == "spotify:track:0")
        );
        app.set_saved("spotify:track:new".into(), true);
        assert_eq!(app.library.liked.items.len(), 100);
        assert_eq!(app.library.liked.items[0].track.uri, "spotify:track:new");
        app.handle_api(ApiResponse::SavedChanged {
            uris: vec!["spotify:track:new".into()],
            saved: true,
            result: Ok(()),
        });
        assert_eq!(
            app.library.liked.items.len(),
            100,
            "acknowledging a like does not clear the list"
        );
        assert_eq!(app.library.liked.items[0].track.uri, "spotify:track:new");
        app.handle_api(ApiResponse::Contains {
            uris: vec!["spotify:track:new".into()],
            result: Ok(vec![false]),
        });
        assert_eq!(app.is_saved("spotify:track:new"), Some(true));
    }

    #[test]
    fn manual_liked_refresh_keeps_rows_sort_and_selection_while_loading() {
        let mut app = cached_liked_app();
        let sort = TableSort {
            column: SortColumn::Title,
            ascending: false,
        };
        app.table_sorts.insert(Page::LikedSongs, sort);
        app.pick_row(&Page::LikedSongs, "liked", 7, RowPick::Only, 100);
        let revision = app.library.liked.revision;
        app.refresh_liked_songs();
        assert_eq!(app.library.liked.items.len(), 100);
        assert_eq!(app.library.liked.revision, revision);
        assert_eq!(app.table_sorts[&Page::LikedSongs], sort);
        assert_eq!(picked(&app, &Page::LikedSongs), vec![7]);
        assert!(app.library.liked.loading);
        app.handle_api(ApiResponse::SavedTracks {
            offset: 0,
            generation: 1,
            account_id: Some("alice".into()),
            result: Ok(crate::api::models::Page::default()),
        });
        assert_eq!(
            app.library.liked.items.len(),
            100,
            "a previous load cannot replace current rows"
        );
    }

    /// Unliking a song shortens Liked Songs. The table caches its row order
    /// against the list's revision, so a silent removal leaves the cache
    /// pointing past the end of the rows and the next frame panics.
    #[test]
    fn unliking_a_song_moves_the_liked_revision() {
        use crate::api::models::SavedTrack;

        let saved = |uri: &str| SavedTrack {
            added_at: None,
            track: Track {
                uri: uri.into(),
                ..Default::default()
            },
        };
        let mut app = headless_app();
        app.library.liked.items = vec![saved("spotify:track:stays"), saved("spotify:track:goes")];
        app.library.liked.total = Some(2);
        app.library.liked.loaded_once = true;
        let before = app.library.liked.revision;

        app.handle_api(ApiResponse::SavedChanged {
            uris: vec!["spotify:track:goes".into()],
            saved: false,
            result: Ok(()),
        });

        let uris: Vec<&str> = app
            .library
            .liked
            .items
            .iter()
            .map(|item| item.track.uri.as_str())
            .collect();
        assert_eq!(uris, vec!["spotify:track:stays"], "the song is gone");
        assert_eq!(app.library.liked.total, Some(1));
        assert_ne!(
            app.library.liked.revision, before,
            "the shorter list must invalidate the table's cached row order"
        );
    }
}
