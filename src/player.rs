//! Local Spotify Connect playback through librespot.
//!
//! The engine owns one librespot session, player, mixer, and Spirc (the
//! Connect state machine). Player events are folded into a [`LocalState`]
//! snapshot that is pushed to the interface whenever something changed;
//! commands from the interface go straight to Spirc, which keeps Spotify's
//! cluster state in sync so phones and other clients see what this device
//! is doing.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use librespot_connect::{
    ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options, PlayingTrack,
    Spirc,
};
use librespot_core::{
    SpotifyUri,
    authentication::Credentials,
    cache::Cache,
    config::{DeviceType, SessionConfig},
    error::ErrorKind,
    session::Session,
    spotify_id::SpotifyId,
};
use librespot_metadata::{
    Album as MetadataAlbum, Metadata,
    album::AlbumType,
    audio::{AudioItem, UniqueFields},
};
use librespot_playback::{
    audio_backend::{self, Sink},
    config::{AudioFormat, Bitrate, NormalisationType, PlayerConfig, VolumeCtrl},
    mixer::{self, Mixer, MixerConfig, NoOpVolume, VolumeGetter},
    player::{Player, PlayerEvent},
};
use sha1::{Digest, Sha1};

use crate::api::models::ArtistRef;
use crate::sink::{AudioControl, ErrorHook, RodioSink};
use crate::vis::{AudioTap, Tapped};

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub device_name: String,
    pub bitrate_kbps: u16,
    pub normalisation: bool,
    pub autoplay: bool,
    pub gapless: bool,
    pub backend: Option<String>,
    pub audio_device: Option<String>,
    pub initial_volume: u16,
    pub volume_dir: PathBuf,
    pub audio_cache_dir: Option<PathBuf>,
    pub audio_cache_limit: Option<u64>,
    /// Output buffer length in milliseconds.
    pub buffer_ms: u32,
    pub tap: Arc<AudioTap>,
    /// The equalizer's settings, shared with the window that sets them.
    pub eq: crate::eq::SharedEq,
    /// Proxy used by the Web API client. Librespot only uses the HTTP form.
    pub proxy: crate::settings::ProxyConfig,
}

impl EngineConfig {
    /// A stable Connect device id derived from the name, so Spotify keeps
    /// recognising this computer across restarts.
    pub fn device_id(&self) -> String {
        hex(&Sha1::digest(self.device_name.as_bytes()))
    }

    pub fn open_cache(&self) -> Result<Cache> {
        Cache::new(
            None,
            Some(self.volume_dir.as_path()),
            self.audio_cache_dir.as_deref(),
            self.audio_cache_limit,
        )
        .map(Cache::with_memory_credentials)
        .context("unable to open the playback cache")
    }

    fn bitrate(&self) -> Bitrate {
        match self.bitrate_kbps {
            96 => Bitrate::Bitrate96,
            160 => Bitrate::Bitrate160,
            _ => Bitrate::Bitrate320,
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Playback {
    #[default]
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepeatMode {
    #[default]
    Off,
    Context,
    Track,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Context,
            Self::Context => Self::Track,
            Self::Track => Self::Off,
        }
    }

    pub fn api_name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Context => "context",
            Self::Track => "track",
        }
    }

    pub fn from_api(name: &str) -> Self {
        match name {
            "context" => Self::Context,
            "track" => Self::Track,
            _ => Self::Off,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalTrack {
    pub uri: String,
    pub title: String,
    pub artists: Vec<ArtistRef>,
    pub album: String,
    pub art_url: Option<String>,
    pub art_small_url: Option<String>,
    pub duration_ms: u32,
    pub is_episode: bool,
}

impl LocalTrack {
    pub fn artist_names(&self) -> String {
        crate::api::models::join_names(self.artists.iter().map(|artist| artist.name.as_str()))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalState {
    pub playback: Playback,
    pub track: Option<LocalTrack>,
    pub position_ms: u32,
    /// When `position_ms` was observed; `None` while not advancing.
    pub position_at: Option<Instant>,
    pub volume: u16,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// The librespot engine's Spotify session is alive. Connect device
    /// activity is separate: Spotify may make this device inactive while the
    /// session remains ready to be activated by the next load.
    pub connected: bool,
    pub username: String,
    pub active_client: String,
    pub error: Option<String>,
    pub seek_sequence: u64,
    /// A newly loaded track, including another play of the same URI.
    pub track_sequence: u64,
    /// Another play of the same track started, and its start position has
    /// not arrived yet. The track looks unchanged, so the `Playing` or
    /// `Paused` that brings the position counts as a seek for media
    /// controls, which would otherwise count on past the end (#587).
    pub replay_pending: bool,
}

/// What local playback was doing when its session ended, so the engine
/// can pick it up again after reconnecting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interrupted {
    pub uri: String,
    pub position_ms: u32,
    /// Playing or loading, as opposed to paused.
    pub playing: bool,
}

impl LocalState {
    /// The track and position to come back to, if something was on.
    pub fn interrupted(&self) -> Option<Interrupted> {
        let track = self.track.as_ref()?;
        if self.playback == Playback::Stopped {
            return None;
        }
        Some(Interrupted {
            uri: track.uri.clone(),
            position_ms: self.position_now(),
            playing: matches!(self.playback, Playback::Playing | Playback::Loading),
        })
    }

    /// The position now, interpolated from the last report while playing.
    pub fn position_now(&self) -> u32 {
        match (self.playback, self.position_at) {
            (Playback::Playing, Some(at)) => {
                let elapsed = at.elapsed().as_millis() as u32;
                let limit = self
                    .track
                    .as_ref()
                    .map_or(u32::MAX, |track| track.duration_ms.max(self.position_ms));
                self.position_ms.saturating_add(elapsed).min(limit)
            }
            _ => self.position_ms,
        }
    }

    pub fn is_active(&self) -> bool {
        self.track.is_some() && self.playback != Playback::Stopped
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadSpec {
    pub context_uri: Option<String>,
    pub uris: Vec<String>,
    pub offset_uri: Option<String>,
    pub offset_index: Option<u32>,
    pub position_ms: u32,
    pub play: bool,
    pub shuffle: Option<bool>,
    /// Explicit repeat preference for a new load. Otherwise retain the
    /// engine's current preference instead of librespot's default (off).
    pub repeat: Option<RepeatMode>,
    /// Play what Spotify would follow `context_uri` with, its autoplay
    /// station, rather than the context itself.
    pub autoplay: bool,
}

/// A dropped Connect session retains its complete playback state. Live
/// replacement for an audio-settings change still uses a track pickup.
#[derive(Clone, Debug)]
pub enum PlaybackResume {
    Session(Arc<librespot_connect::PlaybackSnapshot>),
    Track(LoadSpec),
}

impl LoadSpec {
    fn context_options(&self, current_repeat: RepeatMode) -> LoadContextOptions {
        if self.autoplay {
            LoadContextOptions::Autoplay
        } else {
            let repeat = self.repeat.unwrap_or(current_repeat);
            LoadContextOptions::Options(Options {
                shuffle: self.shuffle.unwrap_or(false),
                repeat: repeat == RepeatMode::Context,
                repeat_track: repeat == RepeatMode::Track,
            })
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerCommand {
    Toggle,
    Next,
    Previous,
    /// Remove manually queued tracks and keep context tracks.
    ClearQueue,
    /// Queue a track or episode after the ones already queued.
    AddToQueue(String),
    Seek(u32),
    /// The volume to keep: applied at once and told to Spotify Connect.
    Volume(u16),
    /// The slider mid-drag: applied at once, nothing sent. Every Connect
    /// update costs a round trip to Spotify, and librespot makes them one
    /// after another, so dragging through fifty values lagged by seconds.
    VolumePreview(u16),
    Shuffle(bool),
    Repeat(RepeatMode),
    Load(LoadSpec),
    /// Take over the active Connect session, including its queue and position.
    Transfer,
}

#[allow(clippy::large_enum_variant)]
pub enum EngineEvent {
    State(LocalState),
    SessionEnded,
}

pub type Notify = Arc<dyn Fn(EngineEvent) + Send + Sync>;

pub struct Engine {
    player: Arc<Player>,
    spirc: Arc<Spirc>,
    session: Session,
    mixer: Arc<dyn Mixer>,
    device_id: String,
    state: Arc<Mutex<LocalState>>,
    /// What was playing when the session ended on its own.
    interrupted: Arc<Mutex<Option<Interrupted>>>,
    shutting_down: Arc<std::sync::atomic::AtomicBool>,
    audio: Arc<AudioControl>,
}

impl Engine {
    pub(crate) fn credentials(&self) -> Option<Credentials> {
        self.session.cache().and_then(|cache| cache.credentials())
    }
    /// Connects to Spotify and announces this device on Spotify Connect.
    pub async fn connect(
        config: &EngineConfig,
        proxy: Option<reqwest::Url>,
        credentials: Credentials,
        cache: Cache,
        notify: Notify,
    ) -> Result<Self> {
        let device_id = config.device_id();
        let session_config = SessionConfig {
            device_id: device_id.clone(),
            autoplay: Some(config.autoplay),
            proxy,
            ..SessionConfig::default()
        };
        let normalisation_factor = Arc::new(std::sync::atomic::AtomicU64::new(1.0f64.to_bits()));
        let player_config = PlayerConfig {
            bitrate: config.bitrate(),
            gapless: config.gapless,
            normalisation: config.normalisation,
            normalisation_type: NormalisationType::Auto,
            position_update_interval: Some(Duration::from_secs(1)),
            // The fork reports each track's normalisation factor here, so
            // the tap can undo it for the visualisers: they show the music,
            // not the loudness housekeeping.
            normalisation_report: Some(Arc::clone(&normalisation_factor)),
            ..PlayerConfig::default()
        };

        let mixer_builder =
            mixer::find(Some("softvol")).ok_or_else(|| anyhow!("soft volume mixer missing"))?;
        // librespot's default curve spans 60 dB logarithmically, which puts
        // half the slider below -30 dB and every level anyone wants in its
        // top quarter. The cubic curve reaches -16 dB at the middle and -7 dB
        // at three quarters, spreading the useful range across the slider.
        let mixer = mixer_builder(MixerConfig {
            volume_ctrl: VolumeCtrl::Cubic(VolumeCtrl::DEFAULT_DB_RANGE),
            ..MixerConfig::default()
        })
        .context("unable to create the mixer")?;

        let state = Arc::new(Mutex::new(LocalState {
            volume: config.initial_volume,
            ..LocalState::default()
        }));
        let session = Session::new(session_config, Some(cache));
        let audio = AudioControl::new(config.buffer_ms);
        let (sink_builder, volume) = sink_builder(
            config,
            Arc::clone(&state),
            Arc::clone(&notify),
            &mixer,
            Arc::clone(&normalisation_factor),
            Arc::clone(&audio),
        );
        let player = Player::new(player_config, session.clone(), volume, sink_builder);
        let events = player.get_player_event_channel();
        tokio::spawn(run_events(
            events,
            Arc::clone(&state),
            Arc::clone(&notify),
            Arc::clone(&audio),
        ));

        let connect_config = ConnectConfig {
            name: config.device_name.clone(),
            device_type: DeviceType::Computer,
            initial_volume: config.initial_volume,
            disable_volume: false,
            volume_steps: 64,
            ..ConnectConfig::default()
        };
        let (spirc, spirc_task) = Spirc::new(
            connect_config,
            session.clone(),
            credentials,
            Arc::clone(&player),
            Arc::clone(&mixer),
        )
        .await
        .context("unable to connect to Spotify")?;

        {
            let mut current = state.lock().unwrap_or_else(|p| p.into_inner());
            current.connected = true;
            current.username = session.username();
            notify(EngineEvent::State(current.clone()));
        }

        let shutting_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let interrupted: Arc<Mutex<Option<Interrupted>>> = Arc::default();
        let ended_flag = Arc::clone(&shutting_down);
        let ended_notify = Arc::clone(&notify);
        let ended_state = Arc::clone(&state);
        let ended_interrupted = Arc::clone(&interrupted);
        tokio::spawn(async move {
            spirc_task.await;
            {
                let mut current = ended_state.lock().unwrap_or_else(|p| p.into_inner());
                // Kept before the state is marked stopped, so a reconnect
                // knows what to pick up.
                *ended_interrupted.lock().unwrap_or_else(|p| p.into_inner()) =
                    current.interrupted();
                current.connected = false;
                current.playback = Playback::Stopped;
                current.position_at = None;
                ended_notify(EngineEvent::State(current.clone()));
            }
            if !ended_flag.load(std::sync::atomic::Ordering::SeqCst) {
                ended_notify(EngineEvent::SessionEnded);
            }
        });

        Ok(Self {
            player,
            spirc: Arc::new(spirc),
            session,
            mixer,
            device_id,
            state,
            interrupted,
            shutting_down,
            audio,
        })
    }

    /// Playback state to resume after replacing this engine.
    pub fn interrupted(&self) -> Option<Interrupted> {
        let ended = self
            .interrupted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        ended.or_else(|| {
            self.state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .interrupted()
        })
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Whether Spotify classifies this album as an EP in its internal metadata.
    pub(crate) async fn album_is_ep(&self, album_uri: &str) -> Result<bool> {
        let uri = SpotifyUri::from_uri(album_uri).context("invalid album URI")?;
        let album = MetadataAlbum::get(&self.session, &uri)
            .await
            .context("album metadata")?;
        Ok(album.album_type == AlbumType::EP)
    }

    /// Spotify's own transcription of a track, as the raw JSON its clients
    /// read; `Ok(None)` when Spotify has none, an error when asking failed.
    pub async fn lyrics_json(&self, track_uri: &str) -> Result<Option<serde_json::Value>> {
        let Some(id) = track_uri
            .rsplit(':')
            .next()
            .and_then(|id| SpotifyId::from_base62(id).ok())
        else {
            return Ok(None);
        };
        match self.session.spclient().get_lyrics(&id).await {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
            Err(error) if error.kind == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(anyhow!("spotify lyrics: {error}")),
        }
    }

    /// Account playlist tree in Spotify order, including folder markers,
    /// and which of its playlists the account may add songs to.
    pub async fn rootlist(&self) -> Result<Rootlist> {
        use protobuf::Message as _;
        let mut uris = Vec::new();
        let mut editable = std::collections::BTreeSet::new();
        let mut from = 0usize;
        loop {
            let bytes = self
                .session
                .spclient()
                .get_rootlist(from, Some(500))
                .await
                .map_err(|error| anyhow!("rootlist: {error}"))?;
            let content =
                librespot_protocol::playlist4_external::SelectedListContent::parse_from_bytes(
                    &bytes,
                )?;
            let Some(contents) = content.contents.into_option() else {
                break;
            };
            let count = contents.items.len();
            let truncated = contents.truncated();
            editable.extend(editable_uris(&contents));
            uris.extend(contents.items.into_iter().filter_map(|item| item.uri));
            if !truncated || count == 0 {
                break;
            }
            from += count;
        }
        Ok(Rootlist {
            entries: parse_rootlist(&uris),
            editable,
        })
    }

    /// The streaming session, for reads that need no Web API quota.
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn shutdown(&self) {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = self.spirc.shutdown();
        self.player.stop();
    }

    pub fn resume_point(&self) -> Option<PlaybackResume> {
        if let Some(snapshot) = self.spirc.disconnected_playback() {
            return Some(PlaybackResume::Session(snapshot));
        }
        self.interrupted().map(|interrupted| {
            PlaybackResume::Track(LoadSpec {
                uris: vec![interrupted.uri],
                position_ms: interrupted.position_ms,
                play: interrupted.playing,
                ..LoadSpec::default()
            })
        })
    }

    pub fn resume(&self, resume: PlaybackResume) -> Result<()> {
        match resume {
            PlaybackResume::Session(snapshot) => {
                self.spirc.restore_playback(snapshot).map_err(Into::into)
            }
            PlaybackResume::Track(spec) => self.command(PlayerCommand::Load(spec)),
        }
    }

    pub fn command(&self, command: PlayerCommand) -> Result<()> {
        let interrupts_audio = command_interrupts_audio(
            &self.state.lock().unwrap_or_else(|p| p.into_inner()),
            &command,
        );
        if interrupts_audio {
            self.audio.interrupt();
        }
        let result = self.send_command(command);
        if interrupts_audio && result.is_err() {
            self.audio.stopped();
        }
        result
    }

    fn send_command(&self, command: PlayerCommand) -> Result<()> {
        let spirc = &self.spirc;
        match command {
            PlayerCommand::Toggle => spirc.play_pause()?,
            PlayerCommand::Next => spirc.next()?,
            PlayerCommand::Previous => spirc.prev()?,
            PlayerCommand::ClearQueue => spirc.clear_queue()?,
            PlayerCommand::AddToQueue(uri) => spirc.add_to_queue(uri)?,
            PlayerCommand::Seek(position_ms) => spirc.set_position_ms(position_ms)?,
            PlayerCommand::Volume(volume) => {
                self.mixer.set_volume(volume);
                spirc.set_volume(volume)?;
            }
            PlayerCommand::VolumePreview(volume) => self.mixer.set_volume(volume),
            PlayerCommand::Shuffle(enabled) => spirc.shuffle(enabled)?,
            PlayerCommand::Repeat(mode) => match mode {
                RepeatMode::Off => {
                    spirc.repeat_track(false)?;
                    spirc.repeat(false)?;
                }
                RepeatMode::Context => {
                    spirc.repeat_track(false)?;
                    spirc.repeat(true)?;
                }
                RepeatMode::Track => {
                    spirc.repeat(false)?;
                    spirc.repeat_track(true)?;
                }
            },
            PlayerCommand::Transfer => spirc.transfer(None)?,
            PlayerCommand::Load(spec) => {
                let playing_track = spec
                    .offset_uri
                    .clone()
                    .map(PlayingTrack::Uri)
                    .or_else(|| spec.offset_index.map(PlayingTrack::Index));
                // A load resets librespot's options, including repeat. Pass
                // the user's preference even when shuffle is off.
                let repeat = self.state.lock().unwrap_or_else(|p| p.into_inner()).repeat;
                let context_options = Some(spec.context_options(repeat));
                let options = LoadRequestOptions {
                    start_playing: spec.play,
                    seek_to: spec.position_ms,
                    playing_track,
                    context_options,
                };
                let request = if let Some(context) = spec.context_uri {
                    LoadRequest::from_context_uri(context, options)
                } else if !spec.uris.is_empty() {
                    LoadRequest::from_tracks(spec.uris, options)
                } else {
                    anyhow::bail!("nothing to play");
                };
                spirc.activate()?;
                spirc.load(request)?;
            }
        }
        Ok(())
    }
}

fn command_interrupts_audio(state: &LocalState, command: &PlayerCommand) -> bool {
    state.playback == Playback::Playing
        && matches!(
            command,
            PlayerCommand::Next | PlayerCommand::Previous | PlayerCommand::Load(_)
        )
}

/// Builds the audio sink and chooses where volume is applied.
///
/// The default sink opens the device on playback and reports errors instead
/// of panicking. It applies volume at output so changes affect queued audio.
/// Other librespot backends remain available through Settings.
type SinkAndVolume = (
    Box<dyn FnOnce() -> Box<dyn Sink> + Send>,
    Box<dyn VolumeGetter + Send>,
);

fn sink_builder(
    config: &EngineConfig,
    state: Arc<Mutex<LocalState>>,
    notify: Notify,
    mixer: &Arc<dyn Mixer>,
    normalisation: Arc<std::sync::atomic::AtomicU64>,
    audio: Arc<AudioControl>,
) -> SinkAndVolume {
    let device = config.audio_device.clone();
    let buffer_ms = config.buffer_ms;
    let tap = Arc::clone(&config.tap);
    let eq = Arc::clone(&config.eq);
    let report: ErrorHook = Arc::new(move |message: String| {
        let snapshot = {
            let mut current = state.lock().unwrap_or_else(|p| p.into_inner());
            current.error = Some(message);
            current.clone()
        };
        notify(EngineEvent::State(snapshot));
    });
    if let Some(name) = config
        .backend
        .as_deref()
        .filter(|name| *name != crate::sink::NAME)
    {
        match audio_backend::find(Some(name.to_string())) {
            Some(builder) => {
                // Apply volume after the tap so visualizers are independent of
                // volume, including at zero.
                let applied = mixer.get_soft_volume();
                let normalisation = Arc::clone(&normalisation);
                return (
                    Box::new(move || {
                        let sink = builder(device, AudioFormat::S16);
                        Box::new(Tapped::new(sink, tap, applied, true, eq, normalisation))
                            as Box<dyn Sink>
                    }),
                    Box::new(NoOpVolume),
                );
            }
            None => log::warn!("audio backend {name:?} is unavailable; using the default"),
        }
    }
    let volume = mixer.get_soft_volume();
    // The output applies volume to queued audio. The wrapper reads the same
    // value to calculate the pre-volume limiter ceiling.
    let ceiling = mixer.get_soft_volume();
    (
        Box::new(move || {
            let sink = Box::new(RodioSink::new(device, report, volume, buffer_ms, audio));
            Box::new(Tapped::new(sink, tap, ceiling, false, eq, normalisation)) as Box<dyn Sink>
        }),
        Box::new(NoOpVolume),
    )
}

async fn run_events(
    mut events: tokio::sync::mpsc::UnboundedReceiver<PlayerEvent>,
    state: Arc<Mutex<LocalState>>,
    notify: Notify,
    audio: Arc<AudioControl>,
) {
    let mut play_request_id = None;
    while let Some(event) = events.recv().await {
        if let PlayerEvent::PlayRequestIdChanged {
            play_request_id: next,
        } = &event
        {
            play_request_id = Some(*next);
            continue;
        }
        if let (Some(current), Some(incoming)) = (play_request_id, event.get_play_request_id())
            && current != incoming
        {
            continue;
        }
        audio.handle_player_event(&event);
        let snapshot = {
            let mut current = state.lock().unwrap_or_else(|p| p.into_inner());
            if apply_event(&mut current, event) {
                Some(current.clone())
            } else {
                None
            }
        };
        if let Some(snapshot) = snapshot {
            notify(EngineEvent::State(snapshot));
        }
    }
}

fn set<T: PartialEq>(target: &mut T, value: T) -> bool {
    if *target == value {
        false
    } else {
        *target = value;
        true
    }
}

/// Reports a replay's start position as a seek, once.
fn start_replay(state: &mut LocalState) -> bool {
    if std::mem::take(&mut state.replay_pending) {
        state.seek_sequence = state.seek_sequence.wrapping_add(1);
        true
    } else {
        false
    }
}

fn apply_event(state: &mut LocalState, event: PlayerEvent) -> bool {
    match event {
        PlayerEvent::Stopped { .. } => {
            let mut changed = set(&mut state.playback, Playback::Stopped);
            changed |= set(&mut state.position_ms, 0);
            changed |= set(&mut state.position_at, None);
            changed
        }
        PlayerEvent::Loading { position_ms, .. } => {
            let mut changed = if state.playback == Playback::Stopped {
                set(&mut state.playback, Playback::Loading)
            } else {
                false
            };
            changed |= set(&mut state.position_ms, position_ms);
            changed |= set(&mut state.position_at, None);
            changed |= set(&mut state.error, None);
            changed
        }
        PlayerEvent::Playing { position_ms, .. } => {
            set(&mut state.playback, Playback::Playing);
            set(&mut state.position_ms, position_ms);
            state.position_at = Some(Instant::now());
            start_replay(state);
            true
        }
        PlayerEvent::Paused { position_ms, .. } => {
            let mut changed = set(&mut state.playback, Playback::Paused);
            changed |= set(&mut state.position_ms, position_ms);
            changed |= set(&mut state.position_at, None);
            changed | start_replay(state)
        }
        PlayerEvent::PositionCorrection { position_ms, .. }
        | PlayerEvent::PositionChanged { position_ms, .. } => {
            state.position_ms = position_ms;
            if state.playback == Playback::Playing {
                state.position_at = Some(Instant::now());
            }
            true
        }
        PlayerEvent::Seeked { position_ms, .. } => {
            state.position_ms = position_ms;
            if state.playback == Playback::Playing {
                state.position_at = Some(Instant::now());
            }
            state.seek_sequence = state.seek_sequence.wrapping_add(1);
            true
        }
        PlayerEvent::TrackChanged { audio_item } => {
            let track = local_track(&audio_item);
            state.replay_pending = state
                .track
                .as_ref()
                .is_some_and(|previous| previous.uri == track.uri);
            state.track = Some(track);
            state.error = None;
            // librespot emits this when a loaded track starts, including a
            // repeat whose URI and metadata are identical to the previous play.
            state.track_sequence = state.track_sequence.wrapping_add(1);
            true
        }
        PlayerEvent::Unavailable { track_id, .. } => set(
            &mut state.error,
            Some(format!(
                "This item isn't available: {}",
                track_id.to_uri().unwrap_or_default()
            )),
        ),
        PlayerEvent::AudioKeyUnavailable { .. } => set(
            &mut state.error,
            Some("Spotify refused the audio key. Try again later".into()),
        ),
        PlayerEvent::VolumeChanged { volume } => set(&mut state.volume, volume),
        PlayerEvent::SessionConnected { user_name, .. } => {
            let mut changed = set(&mut state.connected, true);
            changed |= set(&mut state.username, user_name);
            changed
        }
        // In librespot this event means the Connect device became inactive,
        // usually because another device took over. The engine session is
        // still alive, and `Load` activates it again before starting a track.
        PlayerEvent::SessionDisconnected { .. } => set(&mut state.active_client, String::new()),
        PlayerEvent::SessionClientChanged { client_name, .. } => {
            set(&mut state.active_client, client_name)
        }
        PlayerEvent::ShuffleChanged { shuffle } => set(&mut state.shuffle, shuffle),
        PlayerEvent::RepeatChanged { context, track } => {
            let mode = if track {
                RepeatMode::Track
            } else if context {
                RepeatMode::Context
            } else {
                RepeatMode::Off
            };
            set(&mut state.repeat, mode)
        }
        PlayerEvent::Preloading { .. }
        | PlayerEvent::TimeToPreloadNextTrack { .. }
        | PlayerEvent::EndOfTrack { .. }
        | PlayerEvent::PlayRequestIdChanged { .. }
        | PlayerEvent::AutoPlayChanged { .. }
        | PlayerEvent::FilterExplicitContentChanged { .. } => false,
    }
}

fn local_track(item: &AudioItem) -> LocalTrack {
    let (artists, album, is_episode) = match &item.unique_fields {
        UniqueFields::Track { artists, album, .. } => (
            artists
                .iter()
                .map(|artist| {
                    let uri = artist.id.to_uri().ok();
                    ArtistRef {
                        id: uri
                            .as_deref()
                            .and_then(crate::util::uri_id)
                            .map(str::to_string),
                        name: artist.name.clone(),
                        uri,
                    }
                })
                .collect(),
            album.clone(),
            false,
        ),
        UniqueFields::Episode { show_name, .. } => (
            vec![ArtistRef {
                name: show_name.clone(),
                ..ArtistRef::default()
            }],
            show_name.clone(),
            true,
        ),
        UniqueFields::Local { artists, album, .. } => (
            artists
                .iter()
                .map(|name| ArtistRef {
                    name: name.clone(),
                    ..ArtistRef::default()
                })
                .collect(),
            album.clone().unwrap_or_default(),
            false,
        ),
    };
    let mut covers: Vec<_> = item.covers.iter().collect();
    covers.sort_by_key(|cover| std::cmp::Reverse(cover.width));
    let art_url = covers.first().map(|cover| cover.url.clone());
    let art_small_url = covers
        .iter()
        .rev()
        .find(|cover| cover.width >= 64)
        .or(covers.last())
        .map(|cover| cover.url.clone());
    LocalTrack {
        uri: item.uri.clone(),
        title: item.name.clone(),
        artists,
        album,
        art_url,
        art_small_url,
        duration_ms: item.duration_ms,
        is_episode,
    }
}

/// The account's playlist tree, and what Spotify lets the account do to
/// the playlists in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rootlist {
    /// The rows in Spotify's order, folder markers included.
    pub entries: Vec<RootlistEntry>,
    /// Playlists the account may add songs to, by URI, as Spotify's own
    /// permission service decorates the rootlist. The Web API's
    /// `collaborative` flag stays false for a playlist shared by
    /// invitation, so this is the only word on those.
    pub editable: std::collections::BTreeSet<String>,
}

/// The playlists in one rootlist page the account may add songs to, read
/// from the `capabilities` Spotify puts beside each row.
pub fn editable_uris(
    contents: &librespot_protocol::playlist4_external::ListItems,
) -> impl Iterator<Item = String> + '_ {
    contents
        .items
        .iter()
        .zip(&contents.meta_items)
        .filter(|(_, meta)| meta.capabilities.can_edit_items())
        .filter_map(|(item, _)| item.uri.clone())
}

/// One row of the account's playlist tree.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RootlistEntry {
    /// A playlist, by its URI.
    Playlist(String),
    /// A folder opens; everything until its end sits inside it.
    FolderStart {
        id: String,
        name: String,
    },
    FolderEnd,
}

/// The rootlist's rows from its URIs: playlists pass through, and the
/// `start-group`/`end-group` markers Spotify brackets folders with become
/// folder rows, their names percent-decoded.
pub fn parse_rootlist(uris: &[String]) -> Vec<RootlistEntry> {
    let mut entries = Vec::new();
    let mut depth = 0usize;
    for uri in uris {
        if let Some(rest) = uri.strip_prefix("spotify:start-group:") {
            let (id, name) = match rest.split_once(':') {
                Some((id, name)) => (id.to_string(), decode_folder_name(name)),
                None => (rest.to_string(), String::new()),
            };
            entries.push(RootlistEntry::FolderStart { id, name });
            depth += 1;
        } else if uri.starts_with("spotify:end-group:") {
            if depth > 0 {
                entries.push(RootlistEntry::FolderEnd);
                depth -= 1;
            }
        } else if uri.starts_with("spotify:playlist:") {
            entries.push(RootlistEntry::Playlist(uri.clone()));
        }
    }
    // A folder Spotify never closed still closes here.
    entries.extend(std::iter::repeat_n(RootlistEntry::FolderEnd, depth));
    entries
}

/// Folder names arrive percent-encoded, with `+` for a space.
fn decode_folder_name(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            // The digits are read from the bytes this loop is already
            // walking. Taking them by slicing the text instead put the end
            // of the slice two bytes past a `%`, which is inside a character
            // whenever the next one is not ASCII: a panic rather than the
            // parse error the arm below is written for, on exactly the names
            // that arm exists for.
            b'%' if i + 2 < bytes.len() => {
                let digit = |byte: u8| (byte as char).to_digit(16);
                match (digit(bytes[i + 1]), digit(bytes[i + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high << 4 | low) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn loading_another_song_keeps_repeat_with_or_without_shuffle() {
        for shuffle in [None, Some(false), Some(true)] {
            for mode in [RepeatMode::Off, RepeatMode::Context, RepeatMode::Track] {
                let mut spec = LoadSpec {
                    shuffle,
                    ..LoadSpec::default()
                };
                let LoadContextOptions::Options(options) = spec.context_options(mode) else {
                    panic!("ordinary playback must supply repeat options");
                };
                assert_eq!(options.shuffle, shuffle.unwrap_or(false));
                assert_eq!(options.repeat, mode == RepeatMode::Context);
                assert_eq!(options.repeat_track, mode == RepeatMode::Track);

                // A user's new preference wins over an older player event,
                // including turning repeat off immediately before a load.
                spec.repeat = Some(mode);
                let LoadContextOptions::Options(options) = spec.context_options(mode.next()) else {
                    panic!("ordinary playback must supply repeat options");
                };
                assert_eq!(options.repeat, mode == RepeatMode::Context);
                assert_eq!(options.repeat_track, mode == RepeatMode::Track);
            }
        }
        assert!(matches!(
            LoadSpec {
                autoplay: true,
                ..LoadSpec::default()
            }
            .context_options(RepeatMode::Context),
            LoadContextOptions::Autoplay
        ));
    }

    #[test]
    fn playback_metadata_preserves_each_artist_id_and_name() {
        use librespot_metadata::artist::{ArtistWithRole, ArtistsWithRole};

        let credits = [
            (
                "spotify:artist:0000000000000000000001",
                "Tyler, the Creator",
            ),
            ("spotify:artist:0000000000000000000002", "Guest"),
        ];
        let item = AudioItem {
            track_id: uri(),
            uri: uri().to_uri().unwrap(),
            files: Default::default(),
            name: "Song".into(),
            covers: vec![],
            language: vec![],
            duration_ms: 200_000,
            is_explicit: false,
            availability: Ok(()),
            alternatives: None,
            unique_fields: UniqueFields::Track {
                artists: ArtistsWithRole(
                    credits
                        .iter()
                        .map(|(uri, name)| ArtistWithRole {
                            id: librespot_core::SpotifyUri::from_uri(uri).unwrap(),
                            name: (*name).into(),
                            role: Default::default(),
                        })
                        .collect(),
                ),
                album: "Album".into(),
                album_artists: vec![],
                popularity: 0,
                number: 1,
                disc_number: 1,
            },
        };

        let track = local_track(&item);
        assert_eq!(track.artist_names(), "Tyler, the Creator, Guest");
        assert_eq!(track.artists.len(), 2);
        for (artist, (uri, name)) in track.artists.iter().zip(credits) {
            assert_eq!(artist.id.as_deref(), crate::util::uri_id(uri));
            assert_eq!(artist.uri.as_deref(), Some(uri));
            assert_eq!(artist.name, name);
        }
    }

    #[test]
    fn the_rootlist_markers_become_folders() {
        let uris: Vec<String> = [
            "spotify:playlist:aaa",
            "spotify:start-group:f1:Late%20Night+Mix",
            "spotify:playlist:bbb",
            "spotify:playlist:ccc",
            "spotify:end-group:f1",
            "spotify:playlist:ddd",
            "spotify:start-group:f2:Open",
            "spotify:playlist:eee",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let rows = parse_rootlist(&uris);
        assert_eq!(
            rows[0],
            RootlistEntry::Playlist("spotify:playlist:aaa".into())
        );
        assert_eq!(
            rows[1],
            RootlistEntry::FolderStart {
                id: "f1".into(),
                name: "Late Night Mix".into()
            }
        );
        assert_eq!(rows[4], RootlistEntry::FolderEnd);
        // The unclosed folder still closes.
        assert_eq!(rows.last(), Some(&RootlistEntry::FolderEnd));
        assert_eq!(rows.len(), 9);
    }

    #[test]
    fn a_folder_name_with_a_bare_percent_keeps_its_percent() {
        // The decoder already has an answer for a `%` that begins no escape:
        // it keeps the `%` and moves on. That answer could not be reached
        // when the next character was multi-byte, because the two digits
        // were taken by slicing the `&str` and the second byte of a slice
        // that lands inside a character is a panic, not a parse error.
        let uris: Vec<String> = ["spotify:start-group:f1:100%25 \u{c548}\u{b155}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            parse_rootlist(&uris)[0],
            RootlistEntry::FolderStart {
                id: "f1".into(),
                name: "100% \u{c548}\u{b155}".into()
            }
        );

        // The same shape with nothing to decode at all.
        let raw: Vec<String> = ["spotify:start-group:f2:100% \u{c548}\u{b155}"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            parse_rootlist(&raw)[0],
            RootlistEntry::FolderStart {
                id: "f2".into(),
                name: "100% \u{c548}\u{b155}".into()
            }
        );
    }

    /// A playlist shared by invitation is editable by Spotify's word in the
    /// rootlist, never by the Web API's collaborative flag.
    #[test]
    fn the_rootlist_says_which_playlists_take_songs() {
        use librespot_protocol::playlist_permission::Capabilities;
        use librespot_protocol::playlist4_external::{Item, ListItems, MetaItem};

        // #given
        let mut contents = ListItems::new();
        for (uri, can_edit) in [
            ("spotify:playlist:mine", Some(true)),
            ("spotify:playlist:theirs", Some(false)),
            ("spotify:playlist:shared", Some(true)),
            ("spotify:playlist:undecorated", None),
        ] {
            let mut item = Item::new();
            item.set_uri(uri.to_string());
            contents.items.push(item);
            let mut meta = MetaItem::new();
            if let Some(can_edit) = can_edit {
                let mut capabilities = Capabilities::new();
                capabilities.set_can_edit_items(can_edit);
                meta.capabilities = protobuf::MessageField::some(capabilities);
            }
            contents.meta_items.push(meta);
        }

        // #when
        let editable: Vec<String> = editable_uris(&contents).collect();

        // #then
        assert_eq!(
            editable,
            ["spotify:playlist:mine", "spotify:playlist:shared"]
        );
    }

    use super::*;
    use librespot_core::SpotifyUri;

    fn uri() -> SpotifyUri {
        SpotifyUri::from_uri("spotify:track:14XWXWv5FoCbFzLksawpEe").unwrap()
    }

    fn interlude() -> AudioItem {
        AudioItem {
            track_id: uri(),
            uri: uri().to_uri().unwrap(),
            files: Default::default(),
            name: "Short interlude".into(),
            covers: vec![],
            language: vec![],
            duration_ms: 40_000,
            is_explicit: false,
            availability: Ok(()),
            alternatives: None,
            unique_fields: UniqueFields::Track {
                artists: librespot_metadata::artist::ArtistsWithRole(vec![]),
                album: "Album".into(),
                album_artists: vec![],
                popularity: 0,
                number: 1,
                disc_number: 1,
            },
        }
    }

    #[test]
    fn each_loaded_track_has_a_new_history_sequence_but_seek_and_pause_do_not() {
        let item = interlude();
        let mut state = LocalState::default();
        for sequence in [1, 2] {
            assert!(apply_event(
                &mut state,
                PlayerEvent::TrackChanged {
                    audio_item: Box::new(item.clone())
                }
            ));
            assert_eq!(state.track_sequence, sequence);
            for event in [
                PlayerEvent::Playing {
                    play_request_id: sequence,
                    track_id: uri(),
                    position_ms: 0,
                },
                PlayerEvent::Paused {
                    play_request_id: sequence,
                    track_id: uri(),
                    position_ms: 20_000,
                },
                PlayerEvent::Seeked {
                    play_request_id: sequence,
                    track_id: uri(),
                    position_ms: 0,
                },
                PlayerEvent::Playing {
                    play_request_id: sequence,
                    track_id: uri(),
                    position_ms: 0,
                },
            ] {
                apply_event(&mut state, event);
                assert_eq!(state.track_sequence, sequence);
            }
        }
    }

    /// A track on repeat plays again with the same metadata, so only a
    /// seek tells media controls that the position went back to the start
    /// (#587). The position arrives with `Playing`, after `TrackChanged`.
    #[test]
    fn a_replay_of_the_same_track_reports_its_start_as_a_seek() {
        let mut state = LocalState::default();
        apply_event(
            &mut state,
            PlayerEvent::TrackChanged {
                audio_item: Box::new(interlude()),
            },
        );
        apply_event(
            &mut state,
            PlayerEvent::Playing {
                play_request_id: 1,
                track_id: uri(),
                position_ms: 0,
            },
        );
        let first_play = state.seek_sequence;
        apply_event(
            &mut state,
            PlayerEvent::PositionChanged {
                play_request_id: 1,
                track_id: uri(),
                position_ms: 39_900,
            },
        );

        apply_event(
            &mut state,
            PlayerEvent::TrackChanged {
                audio_item: Box::new(interlude()),
            },
        );
        assert_eq!(
            state.seek_sequence, first_play,
            "the start position is not known yet"
        );
        apply_event(
            &mut state,
            PlayerEvent::Playing {
                play_request_id: 2,
                track_id: uri(),
                position_ms: 0,
            },
        );
        assert_eq!(state.seek_sequence, first_play + 1);
        assert_eq!(state.position_ms, 0);

        apply_event(
            &mut state,
            PlayerEvent::Paused {
                play_request_id: 2,
                track_id: uri(),
                position_ms: 1_000,
            },
        );
        assert_eq!(state.seek_sequence, first_play + 1, "reported once");
    }
    #[test]
    fn position_interpolates_only_while_playing() {
        let mut state = LocalState {
            playback: Playback::Paused,
            position_ms: 5_000,
            position_at: Some(Instant::now() - Duration::from_secs(2)),
            ..LocalState::default()
        };
        assert_eq!(state.position_now(), 5_000);
        state.playback = Playback::Playing;
        assert!(state.position_now() >= 7_000);
    }

    #[test]
    fn loading_keeps_a_playing_state_visible() {
        let mut state = LocalState {
            playback: Playback::Playing,
            ..LocalState::default()
        };
        apply_event(
            &mut state,
            PlayerEvent::Loading {
                play_request_id: 1,
                track_id: uri(),
                position_ms: 0,
            },
        );
        assert_eq!(state.playback, Playback::Playing);
    }

    #[test]
    fn replacing_a_playing_track_interrupts_queued_audio() {
        let playing = LocalState {
            playback: Playback::Playing,
            ..LocalState::default()
        };
        let stopped = LocalState::default();
        let load = PlayerCommand::Load(LoadSpec::default());

        assert!(command_interrupts_audio(&playing, &PlayerCommand::Next));
        assert!(command_interrupts_audio(&playing, &PlayerCommand::Previous));
        assert!(command_interrupts_audio(&playing, &load));
        assert!(!command_interrupts_audio(&stopped, &PlayerCommand::Next));
        assert!(!command_interrupts_audio(
            &playing,
            &PlayerCommand::Seek(10)
        ));
    }

    /// Spotify making this Connect device inactive must not be mistaken for
    /// the engine session ending. A later playlist load can activate the same
    /// Spirc instance; marking it disconnected makes the UI hold that load
    /// forever while waiting for a reconnect that will never happen.
    #[test]
    fn an_inactive_connect_device_keeps_its_engine_session() {
        let mut state = LocalState {
            connected: true,
            active_client: "Spotifast".into(),
            ..LocalState::default()
        };

        assert!(apply_event(
            &mut state,
            PlayerEvent::SessionDisconnected {
                connection_id: "connection".into(),
                user_name: "listener".into(),
            },
        ));

        assert!(state.connected, "the Spotify session is still usable");
        assert!(state.active_client.is_empty());
    }

    #[test]
    fn a_rejected_audio_key_has_its_own_error() {
        let mut state = LocalState::default();

        assert!(apply_event(
            &mut state,
            PlayerEvent::AudioKeyUnavailable {
                play_request_id: 1,
                track_id: uri(),
            },
        ));
        assert_eq!(
            state.error.as_deref(),
            Some("Spotify refused the audio key. Try again later")
        );
    }

    #[test]
    fn repeat_cycles_and_maps() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::Context);
        assert_eq!(RepeatMode::Track.next(), RepeatMode::Off);
        assert_eq!(RepeatMode::from_api("track"), RepeatMode::Track);
        assert_eq!(RepeatMode::Context.api_name(), "context");
    }

    #[test]
    fn device_id_is_stable_hex() {
        let config = EngineConfig {
            buffer_ms: crate::sink::DEFAULT_BUFFER_MS,
            tap: AudioTap::new(),
            eq: crate::eq::shared(),
            device_name: "Spotifast".into(),
            bitrate_kbps: 320,
            normalisation: false,
            autoplay: true,
            gapless: true,
            backend: None,
            audio_device: None,
            initial_volume: 1,
            volume_dir: PathBuf::new(),
            audio_cache_dir: None,
            audio_cache_limit: None,
            proxy: crate::settings::ProxyConfig::Off,
        };
        let id = config.device_id();
        assert_eq!(id.len(), 40);
        assert_eq!(id, config.device_id());
    }

    /// A track that was playing or paused is remembered with its position;
    /// nothing is once playback has stopped.
    #[test]
    fn an_interrupted_track_is_remembered_with_its_position() {
        let mut state = LocalState {
            track: Some(LocalTrack {
                uri: "spotify:track:x".into(),
                duration_ms: 200_000,
                ..LocalTrack::default()
            }),
            playback: Playback::Playing,
            position_ms: 10_000,
            position_at: Some(Instant::now()),
            ..LocalState::default()
        };
        let resume = state.interrupted().expect("playing");
        assert_eq!(resume.uri, "spotify:track:x");
        assert!(resume.playing);
        assert!(resume.position_ms >= 10_000);
        state.playback = Playback::Paused;
        assert!(!state.interrupted().expect("paused").playing);
        state.playback = Playback::Stopped;
        assert!(state.interrupted().is_none());
        state.playback = Playback::Playing;
        state.track = None;
        assert!(state.interrupted().is_none());
    }
}
