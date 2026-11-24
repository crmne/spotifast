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
    FileId, SpotifyUri,
    authentication::Credentials,
    cache::Cache,
    config::{DeviceType, SessionConfig},
    dealer::{manager::BoxedStreamResult, protocol::Message},
    error::ErrorKind,
    session::Session,
    spotify_id::SpotifyId,
};
use librespot_metadata::{
    Album as MetadataAlbum, Metadata, Track as MetadataTrack,
    album::AlbumType,
    audio::{AudioItem, UniqueFields},
};
use librespot_playback::{
    audio_backend::Sink,
    config::{Bitrate, NormalisationType, PlayerConfig, VolumeCtrl},
    mixer::{self, Mixer, MixerConfig, NoOpVolume, VolumeGetter},
    player::{Player, PlayerEvent},
};
use librespot_protocol::connect::{Cluster, ClusterUpdate};
use librespot_protocol::player::{PlayerState, ProvidedTrack};
use sha1::{Digest, Sha1};

use crate::api::models::ArtistRef;
use crate::sink::{AudioControl, DeviceHook, ErrorHook, RodioSink};
use crate::vis::{AudioTap, Tapped};

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub device_name: String,
    pub bitrate_kbps: u16,
    pub normalisation: bool,
    pub autoplay: bool,
    pub gapless: bool,
    /// The saved output, an id or an older build's name (`crate::sink::Saved`).
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
    /// The folder a playlist's `spotify:local:` songs play from.
    pub local_files: Option<PathBuf>,
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

/// The engine error for a song whose decryption key Spotify refused. The app
/// answers it with a banner explaining what still works, rather than the
/// text itself.
pub const AUDIO_KEY_REFUSED: &str = "Spotify refused the audio key. Try again later";

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
    /// The engine is fetching a track right now, even when the previous
    /// track's `playback` still reads `Playing` so its controls stay visible
    /// through the swap.
    pub loading: bool,
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
    /// Queue a track, episode or local file after the ones already queued.
    AddToQueue(String),
    /// Take one upcoming row out of the engine's own queue without playing
    /// it. `index` counts the rows shown under Playing next and Next up;
    /// the engine ignores the command when the row there is another song.
    RemoveNextTrack {
        index: usize,
        uri: String,
    },
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
    /// The output opened on the device an older build saved by `name`;
    /// the setting holds its `id` from now on.
    AudioDevice {
        name: String,
        id: String,
    },
    /// Spotify pushed a change to playback on another device: a switch,
    /// play or pause, another song or collection, shuffle or repeat.
    RemoteChanged,
    /// Spotify pushed a new volume for the device playing elsewhere, and
    /// nothing else changed. It carries the value, so no poll is needed.
    RemoteVolume {
        device_id: String,
        volume: u16,
    },
    /// What the playing device has queued by hand changed, or which queue
    /// edits it takes.
    RemoteQueue(RemoteQueueView),
    LocalQueue(RemoteQueueView),
}

/// The parts of a Connect cluster push that the interface shows for
/// another device. Spotify pushes the whole cluster on every change
/// anywhere, including position heartbeats and this device's own updates,
/// so only a difference here is worth asking the Web API about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ClusterView {
    active_device: String,
    context: String,
    track: String,
    playing: bool,
    shuffle: bool,
    repeat_context: bool,
    repeat_track: bool,
    volume: Option<u32>,
}

impl ClusterView {
    fn of(cluster: &Cluster) -> Self {
        let state = &cluster.player_state;
        Self {
            active_device: cluster.active_device_id.clone(),
            context: state.context_uri.clone(),
            track: state.track.uri.clone(),
            playing: state.is_playing && !state.is_paused,
            shuffle: state.options.shuffling_context,
            repeat_context: state.options.repeating_context,
            repeat_track: state.options.repeating_track,
            volume: cluster
                .device
                .get(&cluster.active_device_id)
                .map(|device| device.volume),
        }
    }

    /// Whether going from `self` to `next` changed what the interface shows
    /// for another device. While this device plays before and after, the
    /// engine's own events already carry every change.
    fn changed_elsewhere(&self, next: &Self, own_device: &str) -> bool {
        let ours = self.active_device == own_device && next.active_device == own_device;
        self != next && !ours
    }

    /// The new volume, when it is all that changed on the same device.
    fn volume_only(&self, next: &Self) -> Option<u16> {
        let volume = next.volume?;
        let same_otherwise = Self {
            volume: next.volume,
            ..self.clone()
        } == *next;
        same_otherwise.then(|| volume.min(u32::from(u16::MAX)) as u16)
    }
}

/// The provider Connect marks a next track queued by hand with.
const QUEUE_PROVIDER: &str = "queue";

/// What the playing device has queued by hand, as a cluster push lists
/// it, and which queue edits the device takes. The Web API's queue does
/// not tell queued rows from the context's; the cluster does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteQueueView {
    /// The device's Connect id.
    pub device: String,
    /// When the engine received this snapshot, shared with edit acknowledgements.
    pub published_at: Option<Instant>,
    /// The song it plays.
    pub track: String,
    /// Its next tracks queued by hand, in play order.
    pub queued: Vec<String>,
    /// Their titles, empty where the push names none.
    pub titles: Vec<String>,
    /// Upcoming rows in play order, including the manual queue at the front.
    pub rows: Vec<(String, String)>,
    /// Whether it takes a `set_queue` that reorders its next tracks.
    pub can_reorder: bool,
    pub can_insert: bool,
    /// Whether it takes a `set_queue` that removes some of them.
    pub can_remove: bool,
}

impl RemoteQueueView {
    fn of(cluster: &Cluster) -> Self {
        Self::from_state(cluster.active_device_id.clone(), &cluster.player_state)
    }

    pub(crate) fn from_state(device: String, state: &PlayerState) -> Self {
        let restrictions = &state.restrictions;
        let set_queue = restrictions.disallow_set_queue_reasons.is_empty();
        let mut order = visible_queue(&state.next_tracks);
        order.sort_by_key(|&index| state.next_tracks[index].provider != QUEUE_PROVIDER);
        let rows: Vec<_> = order
            .iter()
            .map(|&index| {
                let track = &state.next_tracks[index];
                (
                    track.uri.clone(),
                    track.metadata.get("title").cloned().unwrap_or_default(),
                )
            })
            .collect();
        let queued = order
            .iter()
            .take_while(|&&index| state.next_tracks[index].provider == QUEUE_PROVIDER)
            .count();
        Self {
            device,
            published_at: None,
            track: state.track.uri.clone(),
            queued: rows[..queued].iter().map(|(uri, _)| uri.clone()).collect(),
            titles: rows[..queued]
                .iter()
                .map(|(_, title)| title.clone())
                .collect(),
            rows,
            can_reorder: set_queue
                && restrictions
                    .disallow_reordering_in_next_tracks_reasons
                    .is_empty(),
            can_insert: set_queue
                && restrictions
                    .disallow_inserting_into_next_tracks_reasons
                    .is_empty(),
            can_remove: set_queue
                && restrictions
                    .disallow_removing_from_next_tracks_reasons
                    .is_empty(),
        }
    }
}

fn visible_queue(next: &[ProvidedTrack]) -> Vec<usize> {
    next.iter()
        .enumerate()
        .filter_map(|(index, row)| {
            (next_stops_on(row)
                && !row.uri.is_empty()
                && row.uri != "spotify:delimiter"
                && row
                    .metadata
                    .get("hidden")
                    .is_none_or(|hidden| hidden != "true"))
            .then_some(index)
        })
        .collect()
}

/// A next or previous track as a Connect command names it. A queued one
/// is marked queued in its metadata too, which is how a receiver keeps it
/// queued (librespot's `set_next_tracks` reads `is_queued`).
fn track_json(track: &ProvidedTrack) -> serde_json::Value {
    let mut metadata = track.metadata.clone();
    if track.provider == QUEUE_PROVIDER {
        metadata.insert("is_queued".to_string(), "true".to_string());
    }
    serde_json::json!({
        "uri": track.uri,
        "uid": track.uid,
        "provider": track.provider,
        "metadata": metadata,
        "album_uri": track.album_uri,
        "artist_uri": track.artist_uri,
    })
}

fn queue_command(state: &PlayerState, next: Vec<serde_json::Value>) -> serde_json::Value {
    let prev: Vec<_> = state.prev_tracks.iter().map(track_json).collect();
    serde_json::json!({
        "command": {
            "endpoint": "set_queue",
            "next_tracks": next,
            "prev_tracks": prev,
            "queue_revision": state.queue_revision,
        }
    })
}

/// A single edit against the visible part of the active player's queue.
#[derive(Clone, Debug)]
pub struct QueueEdit {
    pub playing: String,
    pub before: Vec<String>,
    pub manual: usize,
    pub operation: QueueEditOperation,
}

#[derive(Clone, Debug)]
pub enum QueueEditOperation {
    Move {
        from: usize,
        to: usize,
        into_manual: bool,
    },
    Insert {
        to: usize,
        into_manual: bool,
        added: Vec<String>,
    },
    Remove {
        indexes: Vec<usize>,
    },
}

impl QueueEdit {
    pub(crate) fn command(&self, state: &PlayerState) -> Option<serde_json::Value> {
        self.tracks(state)
            .map(|next| queue_command(state, next.iter().map(track_json).collect()))
    }

    fn tracks(&self, state: &PlayerState) -> Option<Vec<ProvidedTrack>> {
        let restrictions = &state.restrictions;
        let blocked = match &self.operation {
            QueueEditOperation::Move { .. } => {
                &restrictions.disallow_reordering_in_next_tracks_reasons
            }
            QueueEditOperation::Insert { .. } => {
                &restrictions.disallow_inserting_into_next_tracks_reasons
            }
            QueueEditOperation::Remove { .. } => {
                &restrictions.disallow_removing_from_next_tracks_reasons
            }
        };
        if state.track.uri != self.playing
            || !restrictions.disallow_set_queue_reasons.is_empty()
            || !blocked.is_empty()
        {
            return None;
        }
        // Keep delimiters, hidden rows and the unshown tail exactly as received.
        let mut next = state.next_tracks.clone();
        // Some receivers interleave their manual and context rows in a push.
        next.sort_by_key(|row| row.provider != QUEUE_PROVIDER);
        let visible = visible_queue(&next);
        if self.before.len() > visible.len()
            || self.before.iter().enumerate().any(|(i, uri)| {
                let row = &next[visible[i]];
                row.uri != *uri || (row.provider == QUEUE_PROVIDER) != (i < self.manual)
            })
        {
            return None;
        }
        let (from, to, into_manual, added) = match &self.operation {
            QueueEditOperation::Move {
                from,
                to,
                into_manual,
            } => (Some(*from), *to, *into_manual, &[][..]),
            QueueEditOperation::Insert {
                to,
                into_manual,
                added,
            } => (None, *to, *into_manual, added.as_slice()),
            QueueEditOperation::Remove { indexes } => {
                let mut removed = indexes
                    .iter()
                    .map(|&i| visible[..self.before.len()].get(i).copied())
                    .collect::<Option<Vec<_>>>()?;
                removed.sort_unstable();
                removed.dedup();
                for index in removed.into_iter().rev() {
                    next.remove(index);
                }
                return Some(next);
            }
        };
        if to > self.before.len()
            || (into_manual && to > self.manual)
            || (!into_manual && to < self.manual)
        {
            return None;
        }
        let mut at = visible
            .get(to)
            .copied()
            .unwrap_or_else(|| visible.last().map_or(0, |last| last + 1));
        let mut insert = if let Some(from) = from {
            let index = *visible[..self.before.len()].get(from)?;
            at -= usize::from(index < at);
            vec![next.remove(index)]
        } else {
            added
                .iter()
                .map(|uri| ProvidedTrack {
                    uri: uri.clone(),
                    ..Default::default()
                })
                .collect()
        };
        for row in &mut insert {
            if into_manual {
                row.provider = QUEUE_PROVIDER.into();
                row.metadata.insert("is_queued".into(), "true".into());
            } else if row.provider.is_empty() || row.provider == QUEUE_PROVIDER {
                row.provider = "context".into();
                row.metadata.remove("is_queued");
            }
        }
        next.splice(at..at, insert);
        Some(next)
    }

    /// Confirm the complete rewrite, including duplicate identities and the tail.
    pub(crate) fn confirmed_state(&self, before: &PlayerState, current: &PlayerState) -> bool {
        if current.queue_revision.is_empty() {
            return false;
        }
        let Some(wanted) = self.tracks(before) else {
            return false;
        };
        if current.queue_revision == before.queue_revision {
            // librespot hashes the URIs alone, so a move between sections
            // keeps its revision. Rows carrying the UIDs sent, which the
            // old rows do not, still prove the rewrite.
            return !rows_match(&wanted, before, before, true)
                && rows_match(&wanted, before, current, true);
        }
        rows_match(&wanted, before, current, false)
    }

    pub fn confirmed(&self, view: &RemoteQueueView, expected: &[String], manual: usize) -> bool {
        let prefix = manual.min(expected.len());
        view.track == self.playing
            && view.queued.len() >= manual
            && view
                .queued
                .iter()
                .take(prefix)
                .eq(expected[..prefix].iter())
            && view
                .rows
                .iter()
                .skip(view.queued.len())
                .take(expected.len().saturating_sub(manual))
                .map(|(uri, _)| uri)
                .eq(expected[prefix..].iter())
    }
}

/// Whether `current`'s rows are `wanted`. A row keeping one of `before`'s
/// UIDs must keep the one sent; `exact_uids` asks that of every row.
fn rows_match(
    wanted: &[ProvidedTrack],
    before: &PlayerState,
    current: &PlayerState,
    exact_uids: bool,
) -> bool {
    let mut actual = current.next_tracks.clone();
    actual.sort_by_key(|row| row.provider != QUEUE_PROVIDER);
    let manual = wanted
        .iter()
        .take_while(|row| row.provider == QUEUE_PROVIDER)
        .count();
    let actual_manual = actual
        .iter()
        .take_while(|row| row.provider == QUEUE_PROVIDER)
        .count();
    if actual_manual < manual || actual.len() - actual_manual != wanted.len() - manual {
        return false;
    }
    let original_uids: std::collections::HashSet<_> = before
        .next_tracks
        .iter()
        .filter(|row| !row.uid.is_empty())
        .map(|row| row.uid.as_str())
        .collect();
    // Manual rows past the wanted ones were queued since. One that was
    // already queued is a row the edit should have moved or removed.
    let kept: std::collections::HashSet<_> = wanted.iter().map(|row| row.uid.as_str()).collect();
    if actual[manual..actual_manual].iter().any(|row| {
        !row.uid.is_empty()
            && original_uids.contains(row.uid.as_str())
            && !kept.contains(row.uid.as_str())
    }) {
        return false;
    }
    wanted
        .iter()
        .zip(actual[..manual].iter().chain(&actual[actual_manual..]))
        .all(|(wanted, actual)| {
            wanted.uri == actual.uri
                && wanted.provider == actual.provider
                && wanted.metadata.get("hidden") == actual.metadata.get("hidden")
                && (wanted.uid == actual.uid
                    || (!exact_uids && !original_uids.contains(actual.uid.as_str())))
        })
}

/// What the device that plays has coming up, as the latest cluster push
/// told it, and the row the last skip sent to another device goes to.
#[derive(Default)]
struct RemoteQueue {
    device: String,
    state: Option<PlayerState>,
    published_at: Option<Instant>,
    target: Option<SkipTarget>,
    /// The row before the last command moved it, for one that fails.
    before: Option<SkipTarget>,
    /// The list the last skip was counted on, and the row it counted
    /// from, for one stopped partway.
    planned: Option<PlayerState>,
    planned_from: Option<SkipTarget>,
}

impl RemoteQueue {
    /// Takes what a cluster push tells of the device that plays.
    fn show(&mut self, cluster: Cluster) {
        self.published_at = Some(Instant::now());
        self.device = cluster.active_device_id;
        self.state = cluster.player_state.into_option();
        let playing = self.state.as_ref().and_then(playing_key);
        retire_landed(&mut self.target, playing.as_deref());
        retire_landed(&mut self.before, playing.as_deref());
    }

    /// The uid of the Next up row of `device` holding the `occurrence`th
    /// copy of `uri`, counted after the row a skip still on its way there
    /// goes to. `None` when no push has told what `device` has coming up,
    /// the song is not in it, or its row has no uid: that skip names the
    /// song alone.
    fn row(&mut self, device: &str, uri: &str, occurrence: usize) -> Option<String> {
        let earlier = self.pending();
        let pending = earlier.as_ref().map(|target| target.key.as_str());
        // A copy above the row that skip goes to is behind the device by
        // now, so it is not the one meant.
        let key = self
            .state
            .as_ref()
            .filter(|_| self.device == device)
            .and_then(|state| queue_skip(state, pending, uri, occurrence))
            .filter(|skip| skip.skips > 0)
            .map(|skip| skip.key);
        self.planned = self.state.clone();
        self.planned_from = earlier.clone();
        self.before = earlier;
        self.target = key.clone().map(|key| SkipTarget {
            key,
            at: Instant::now(),
        });
        key.filter(|key| is_uid(key, uri))
    }

    /// Moves the row a later skip counts from on to where a Next about to
    /// be sent to `device` lands. The cluster push that shows it comes
    /// after the command's answer, and a click before it would otherwise
    /// name a copy the device has already passed.
    fn note_next(&mut self, device: &str) {
        let earlier = self.pending();
        let pending = earlier.as_ref().map(|target| target.key.as_str());
        let lands = self
            .state
            .as_ref()
            .filter(|_| self.device == device)
            .and_then(|state| next_lands_on(state, pending));
        self.before = earlier;
        self.target = lands.map(|key| SkipTarget {
            key,
            at: Instant::now(),
        });
    }

    /// Previous moves the list differently from the row a skip counts
    /// from, so the next click counts from the top.
    fn note_previous(&mut self) {
        self.before = self.pending();
    }

    /// Puts back the row a later skip counts from, as it was before a
    /// command the device refused or never got.
    fn refused(&mut self) {
        self.target = self.before.take();
    }

    /// Moves the row a later skip counts from on by the `nexts` of a skip
    /// sent as a Next per row that went through before `device` refused
    /// one. They count on the list the skip was counted on, from the row
    /// it counted from: a push since may already show some of them, or
    /// have retired that row.
    fn stopped(&mut self, device: &str, nexts: usize) {
        self.before = None;
        // The skip counted from that row, however old it is by now.
        self.target = self.planned_from.take().map(|from| SkipTarget {
            at: Instant::now(),
            ..from
        });
        let shown = std::mem::replace(&mut self.state, self.planned.take());
        for _ in 0..nexts {
            self.note_next(device);
        }
        self.state = shown;
        let playing = self.state.as_ref().and_then(playing_key);
        retire_landed(&mut self.target, playing.as_deref());
    }

    /// Takes the row a skip still on its way goes to, unless it is too old
    /// to be on its way.
    fn pending(&mut self) -> Option<SkipTarget> {
        self.target
            .take()
            .filter(|target| target.at.elapsed() < SKIP_TARGET_LIFE)
    }
}

/// Forwards cluster pushes that change another device's playback, so the
/// interface shows them at once instead of on its next poll: a new volume
/// as it is, anything else as a reason to poll. Spotify pushes no cluster
/// back for this device's own announcement, so the first push often is the
/// next change on another device, and it counts as one: at most one extra
/// poll per connect. Each push also leaves what the playing device has
/// coming up in `queue`, for a skip to one of those rows to name.
async fn watch_cluster(
    mut updates: BoxedStreamResult<ClusterUpdate>,
    own_device: String,
    queue: Arc<Mutex<RemoteQueue>>,
    notify: Notify,
) {
    use futures_util::StreamExt as _;
    let mut shown = ClusterView::default();
    let mut shown_queue = RemoteQueueView::default();
    while let Some(update) = updates.next().await {
        let Some(cluster) = update.ok().and_then(|update| update.cluster.into_option()) else {
            continue;
        };
        let mut queued = RemoteQueueView::of(&cluster);
        if (queued.can_reorder, queued.can_remove)
            != (shown_queue.can_reorder, shown_queue.can_remove)
        {
            // Why a device refuses queue edits is otherwise invisible.
            let restrictions = &cluster.player_state.restrictions;
            log::info!(
                "queue edits on another device: reorder {}, remove {} \
                 (set_queue {:?}, reordering {:?}, removing {:?})",
                queued.can_reorder,
                queued.can_remove,
                restrictions.disallow_set_queue_reasons,
                restrictions.disallow_reordering_in_next_tracks_reasons,
                restrictions.disallow_removing_from_next_tracks_reasons,
            );
        }
        let view = ClusterView::of(&cluster);
        let published_at = {
            let mut kept = queue.lock().unwrap_or_else(|p| p.into_inner());
            kept.show(cluster);
            kept.published_at
        };
        if queued != shown_queue {
            shown_queue = queued.clone();
            queued.published_at = published_at;
            notify(EngineEvent::RemoteQueue(queued));
        }
        if shown.changed_elsewhere(&view, &own_device) {
            notify(match shown.volume_only(&view) {
                Some(volume) => EngineEvent::RemoteVolume {
                    device_id: view.active_device.clone(),
                    volume,
                },
                None => EngineEvent::RemoteChanged,
            });
        }
        shown = view;
    }
}

pub type Notify = Arc<dyn Fn(EngineEvent) + Send + Sync>;

/// The parts of an engine that outlive its session (#268). librespot's
/// player keeps playing what it has already fetched when the session drops;
/// handed to the next engine, the song carries on through the reconnect
/// instead of stopping until the new session has loaded it again.
#[derive(Clone)]
pub struct KeptPlayer {
    player: Arc<Player>,
    mixer: Arc<dyn Mixer>,
    state: Arc<Mutex<LocalState>>,
    audio: Arc<AudioControl>,
}

impl KeptPlayer {
    /// Stops the song, when nothing is going to pick it up.
    pub fn stop(&self) {
        self.player.stop();
    }

    /// Pauses the song or plays it again, while there is no session to ask.
    /// False when it has no song left to pause or play.
    pub fn toggle(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.track.is_none() {
            return false;
        }
        // Shown at once, and a second toggle before the player answers
        // undoes the first instead of repeating it.
        match state.playback {
            Playback::Playing => {
                state.position_ms = state.position_now();
                state.position_at = None;
                state.playback = Playback::Paused;
                self.player.pause();
            }
            Playback::Paused => {
                state.position_at = Some(Instant::now());
                state.playback = Playback::Playing;
                self.player.play();
            }
            Playback::Stopped | Playback::Loading => return false,
        }
        true
    }

    /// Whether the player still has a song, playing or paused.
    pub fn has_song(&self) -> bool {
        self.now().is_some()
    }

    /// Where the song is now and whether it plays, or `None` once the
    /// player has nothing left.
    fn now(&self) -> Option<Interrupted> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .interrupted()
    }
}

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
    /// The session is being ended while its player plays on.
    closing: Arc<std::sync::atomic::AtomicBool>,
    audio: Arc<AudioControl>,
    /// The player came from a dropped session and still plays what that
    /// session left it, until something is loaded through this one.
    carrying: std::sync::atomic::AtomicBool,
    /// What another device has coming up, for a queue skip sent there.
    remote_queue: Arc<Mutex<RemoteQueue>>,
    local_queue_state: Arc<Mutex<Option<QueueSnapshot>>>,
}

pub(crate) type QueueSnapshot = (PlayerState, Instant);

/// Where a queue skip lands, named as `row_key` names the row.
#[derive(Clone)]
struct SkipTarget {
    key: String,
    at: Instant,
}

/// Each of a skip's Nexts waits for a Connect state update, a fraction of
/// a second; by this age the skip has landed or its list is gone.
const SKIP_TARGET_LIFE: Duration = Duration::from_secs(20);

/// Forgets the row a skip or Next goes to once it is `playing`. Counted
/// after that row, a later click would leave out the Nexts already taken
/// if another app stepped back to the song before it.
fn retire_landed(target: &mut Option<SkipTarget>, playing: Option<&str>) {
    if target
        .as_ref()
        .is_some_and(|target| playing == Some(target.key.as_str()))
    {
        *target = None;
    }
}

/// Tells the app what this computer's own Connect state has coming up
/// each time it changes, in the order the player will play it.
async fn watch_local_queue(
    mut states: tokio::sync::watch::Receiver<Option<PlayerState>>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<PlayerEvent>,
    player: Arc<Player>,
    device: String,
    snapshot: Arc<Mutex<Option<QueueSnapshot>>>,
    notify: Notify,
) {
    let mut shown = RemoteQueueView::default();
    states.mark_changed();
    loop {
        tokio::select! {
            changed = states.changed() => {
                if changed.is_err() {
                    return;
                }
                let queue = {
                    let state = states.borrow_and_update();
                    let state = state.as_ref();
                    *snapshot.lock().unwrap_or_else(|p| p.into_inner()) =
                        state.map(|state| (state.clone(), Instant::now()));
                    state.map(|state| RemoteQueueView::from_state(device.clone(), state))
                };
                if let Some(mut queue) = queue
                    && queue != shown
                {
                    shown = queue.clone();
                    queue.published_at = snapshot.lock().unwrap_or_else(|p| p.into_inner())
                        .as_ref().map(|(_, at)| *at);
                    notify(EngineEvent::LocalQueue(queue));
                }
            }
            event = events.recv() => match event {
                Some(PlayerEvent::Playing { .. }) => {
                    // Spirc can skip publishing Playing when the position agrees.
                    // SetQueue is a notification: Spirc republishes its own state
                    // and doesn't read the payload or change the queue.
                    player.emit_set_queue_event(String::new(), None, Vec::new(), Vec::new());
                }
                Some(_) => {}
                None => return,
            },
        }
    }
}

impl Engine {
    pub(crate) fn credentials(&self) -> Option<Credentials> {
        self.session.cache().and_then(|cache| cache.credentials())
    }
    /// Connects to Spotify and announces this device on Spotify Connect.
    /// A player `kept` from an engine whose session dropped plays on, and
    /// this engine takes it over, instead of starting a player of its own.
    pub async fn connect(
        config: &EngineConfig,
        proxy: Option<reqwest::Url>,
        credentials: Credentials,
        cache: Cache,
        notify: Notify,
        kept: Option<KeptPlayer>,
    ) -> Result<Self> {
        let session_config = SessionConfig {
            device_id: config.device_id(),
            autoplay: Some(config.autoplay),
            proxy,
            ..SessionConfig::default()
        };
        let session = Session::new(session_config, Some(cache));
        let carrying = kept.is_some();
        let (player, mixer, state, audio) = match kept {
            Some(kept) => (kept.player, kept.mixer, kept.state, kept.audio),
            None => Self::start_player(config, &session, &notify)?,
        };

        let connect_config = ConnectConfig {
            name: config.device_name.clone(),
            device_type: DeviceType::Computer,
            initial_volume: config.initial_volume,
            disable_volume: false,
            volume_steps: 64,
            emit_set_queue_events: true,
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
        if carrying {
            // Before Spirc's task runs, so everything it loads comes through
            // the new session. The song already playing keeps fetching the
            // rest of its file as it did.
            player.set_session(session.clone());
        }

        // Subscribed before the Spirc task starts the dealer, while its
        // builder still takes subscriptions. Without it the interface still
        // polls; it only learns of other devices' changes later.
        let remote_queue: Arc<Mutex<RemoteQueue>> = Arc::default();
        match session
            .dealer()
            .listen_for("hm://connect-state/v1/cluster", Message::from_raw)
        {
            Ok(updates) => {
                tokio::spawn(watch_cluster(
                    updates,
                    config.device_id(),
                    Arc::clone(&remote_queue),
                    Arc::clone(&notify),
                ));
            }
            Err(error) => log::warn!("cluster updates unavailable: {error}"),
        }

        {
            let mut current = state.lock().unwrap_or_else(|p| p.into_inner());
            current.connected = true;
            current.username = session.username();
            notify(EngineEvent::State(current.clone()));
        }

        let local_queue_state = Arc::default();
        tokio::spawn(watch_local_queue(
            spirc.watch_player_state(),
            player.get_player_event_channel(),
            Arc::clone(&player),
            config.device_id(),
            Arc::clone(&local_queue_state),
            Arc::clone(&notify),
        ));

        let shutting_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let interrupted: Arc<Mutex<Option<Interrupted>>> = Arc::default();
        let ended_flag = Arc::clone(&shutting_down);
        let ended_closing = Arc::clone(&closing);
        let ended_notify = Arc::clone(&notify);
        let ended_state = Arc::clone(&state);
        let ended_interrupted = Arc::clone(&interrupted);
        tokio::spawn(async move {
            spirc_task.await;
            let shut_down = ended_flag.load(std::sync::atomic::Ordering::SeqCst);
            {
                let mut current = ended_state.lock().unwrap_or_else(|p| p.into_inner());
                // Kept before the state is marked stopped, so a reconnect
                // knows what to pick up.
                *ended_interrupted.lock().unwrap_or_else(|p| p.into_inner()) =
                    current.interrupted();
                session_ended(&mut current, shut_down);
                ended_notify(EngineEvent::State(current.clone()));
            }
            if !shut_down && !ended_closing.load(std::sync::atomic::Ordering::SeqCst) {
                ended_notify(EngineEvent::SessionEnded);
            }
        });

        let device_id = config.device_id();
        Ok(Self {
            player,
            spirc: Arc::new(spirc),
            session,
            mixer,
            device_id,
            state,
            interrupted,
            shutting_down,
            closing,
            audio,
            carrying: std::sync::atomic::AtomicBool::new(carrying),
            remote_queue,
            local_queue_state,
        })
    }

    /// A new player, with the mixer, output and state it plays through.
    #[allow(clippy::type_complexity)]
    fn start_player(
        config: &EngineConfig,
        session: &Session,
        notify: &Notify,
    ) -> Result<(
        Arc<Player>,
        Arc<dyn Mixer>,
        Arc<Mutex<LocalState>>,
        Arc<AudioControl>,
    )> {
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
            // librespot reads this folder's tags on its own player thread
            // when the engine starts, never on the UI thread.
            local_file_directories: config.local_files.iter().cloned().collect(),
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
        let audio = AudioControl::new(config.buffer_ms);
        let (sink_builder, volume) = sink_builder(
            config,
            Arc::clone(&state),
            Arc::clone(notify),
            &mixer,
            Arc::clone(&normalisation_factor),
            Arc::clone(&audio),
        );
        let player = Player::new(player_config, session.clone(), volume, sink_builder);
        let events = player.get_player_event_channel();
        tokio::spawn(run_events(
            events,
            Arc::clone(&state),
            Arc::clone(notify),
            Arc::clone(&audio),
        ));
        Ok((player, mixer, state, audio))
    }

    /// The player and what it plays through, for the engine that replaces
    /// this one after its session dropped. Nothing stops it: the song plays
    /// on until the next engine takes it over or [`KeptPlayer::stop`].
    pub fn keep_player(&self) -> KeptPlayer {
        KeptPlayer {
            player: Arc::clone(&self.player),
            mixer: Arc::clone(&self.mixer),
            state: Arc::clone(&self.state),
            audio: Arc::clone(&self.audio),
        }
    }

    /// Whether the player still plays only what a dropped session left it:
    /// nothing has been loaded or picked up through this engine yet.
    pub fn carrying(&self) -> bool {
        self.carrying.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn stop_carrying(&self) {
        self.carrying
            .store(false, std::sync::atomic::Ordering::SeqCst);
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

    /// What this engine is heard at, kept past the engine itself: the next
    /// engine starts there.
    pub(crate) fn heard(&self) -> Heard {
        Heard {
            state: Arc::clone(&self.state),
        }
    }

    /// Applies a level set here to the mixer and the state at once, so the
    /// engine is heard at it from now on. Connect is told on release, not
    /// while the slider is still moving.
    fn note_volume(&self, volume: u16, preview: bool) -> Result<()> {
        self.mixer.set_volume(volume);
        self.state.lock().unwrap_or_else(|p| p.into_inner()).volume = volume;
        if !preview {
            self.spirc.set_volume(volume)?;
        }
        Ok(())
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
        let cover = self.album_cover(&id).await;
        let endpoint = lyrics_endpoint(&id, cover.as_ref());
        let answer = self
            .session
            .spclient()
            .request_as_json(&reqwest::Method::GET, &endpoint, None, None)
            .await;
        match answer {
            Ok(bytes) => {
                let (json, outcome) = read_lyrics(&bytes);
                log::info!("spotify lyrics {track_uri}: {outcome}");
                Ok(json)
            }
            // A redirect lands here too: librespot files it under not found,
            // and only the status in the error tells them apart.
            Err(error) if error.kind == ErrorKind::NotFound => {
                log::info!("spotify lyrics {track_uri}: not found ({error})");
                Ok(None)
            }
            Err(error) => {
                log::info!("spotify lyrics {track_uri}: failed ({error})");
                Err(anyhow!("spotify lyrics: {error}"))
            }
        }
    }

    /// The track's album cover, which the lyrics request names.
    async fn album_cover(&self, id: &SpotifyId) -> Option<FileId> {
        let uri = SpotifyUri::Track { id: *id };
        match MetadataTrack::get(&self.session, &uri).await {
            Ok(track) => track.album.covers.first().map(|cover| cover.id),
            Err(error) => {
                log::debug!("no cover to ask for lyrics with: {error}");
                None
            }
        }
    }

    /// Account playlist tree in Spotify order, including folder markers,
    /// and which of its playlists the account may add songs to.
    pub async fn rootlist(&self) -> Result<Rootlist> {
        let mut uris = Vec::new();
        let mut editable = std::collections::BTreeSet::new();
        for contents in crate::session_reads::rootlist(&self.session).await? {
            editable.extend(editable_uris(&contents));
            uris.extend(contents.items.into_iter().filter_map(|item| item.uri));
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

    /// The device Spotify's Connect cluster names as playing. Known from
    /// the moment the session connects, and by its Connect id even for a
    /// Sonos, which the Web API reports without one.
    pub fn active_device(&self) -> Option<String> {
        self.spirc
            .watch_cluster_state()
            .borrow()
            .active_device_id
            .clone()
    }

    /// The uid of the row that a skip to the `occurrence`th copy of `uri`
    /// in the Next up of `device`, playing elsewhere, names. `None` leaves
    /// the skip to name the song alone.
    pub fn remote_row(&self, device: &str, uri: &str, occurrence: usize) -> Option<String> {
        self.remote_queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .row(device, uri, occurrence)
    }

    pub(crate) fn queue_state(&self, device: &str) -> Option<QueueSnapshot> {
        if device == self.device_id() {
            if !self
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_active()
            {
                return None;
            }
            return self
                .local_queue_state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
        }
        let queue = self.remote_queue.lock().unwrap_or_else(|p| p.into_inner());
        if queue.device != device {
            return None;
        }
        Some((queue.state.clone()?, queue.published_at?))
    }

    /// Notes a Next about to be sent to `device`, playing elsewhere, for a
    /// skip after it to count its rows from.
    pub fn note_remote_next(&self, device: &str) {
        self.remote_queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .note_next(device);
    }

    /// Notes a Previous about to be sent to another device.
    pub fn note_remote_previous(&self) {
        self.remote_queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .note_previous();
    }

    /// The last skip, Next or Previous sent to another device failed: a
    /// later skip counts from where the device was before it.
    pub fn remote_step_failed(&self) {
        self.remote_queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .refused();
    }

    /// A skip to a row on `device`, sent as a Next per row, that the
    /// device refused after `nexts` of them went through.
    pub fn remote_skip_stopped(&self, device: &str, nexts: usize) {
        self.remote_queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .stopped(device, nexts);
    }

    pub fn shutdown(&self) {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = self.spirc.shutdown();
        self.player.stop();
    }

    /// Ends the session but leaves the player playing, for an engine that
    /// is not kept while the song it carries is (#268).
    pub fn close_session(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = self.spirc.shutdown();
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
        // A carried song is picked up where it has got to, playing or
        // paused as it is now, not where the session dropped: the player
        // already has it, so the restore only lines its decoder up.
        let now = self.carrying().then(|| self.keep_player().now()).flatten();
        self.stop_carrying();
        match resume_from(resume, now.as_ref()) {
            PlaybackResume::Session(snapshot) => {
                self.spirc.restore_playback(snapshot).map_err(Into::into)
            }
            PlaybackResume::Track(spec) => self.send_command(PlayerCommand::Load(spec)),
        }
    }

    pub fn command(&self, command: PlayerCommand) -> Result<()> {
        if matches!(command, PlayerCommand::Load(_) | PlayerCommand::Transfer) {
            // Something else chosen replaces the song a dropped session left.
            self.stop_carrying();
        }
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
            PlayerCommand::AddToQueue(uri) => spirc.add_to_queue(SpotifyUri::from_uri(&uri)?)?,
            PlayerCommand::RemoveNextTrack { index, uri } => {
                spirc.remove_next_track(index, SpotifyUri::from_uri(&uri)?)?
            }
            PlayerCommand::Seek(position_ms) => spirc.set_position_ms(position_ms)?,
            PlayerCommand::Volume(volume) => self.note_volume(volume, false)?,
            PlayerCommand::VolumePreview(volume) => self.note_volume(volume, true)?,
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

/// Next passes queue delimiters, and songs found unavailable, without
/// stopping on them, as librespot's does.
fn next_stops_on(track: &ProvidedTrack) -> bool {
    !track.uid.starts_with("delimiter") && track.provider != "unavailable"
}

/// What tells a row apart from the rows `after` it: its uid, or for a row
/// without one its URI and the copies of the song without a uid after it,
/// which the Nexts that pass the rows above it leave as they are.
fn row_key(track: &ProvidedTrack, after: &[ProvidedTrack]) -> String {
    if !track.uid.is_empty() {
        return track.uid.clone();
    }
    let later = after
        .iter()
        .filter(|row| row.uid.is_empty() && row.uri == track.uri)
        .count();
    format!("{}#{later}", track.uri)
}

/// The key of the `at`th of the `next` tracks.
fn next_key(next: &[ProvidedTrack], at: usize) -> String {
    row_key(&next[at], &next[at + 1..])
}

/// The key of the row `state` plays.
fn playing_key(state: &PlayerState) -> Option<String> {
    let playing = state.track.as_ref()?;
    Some(row_key(playing, &state.next_tracks))
}

/// Whether `key` is the uid of a row of `uri`, not the song of a row
/// without one.
fn is_uid(key: &str, uri: &str) -> bool {
    let rest = key.strip_prefix(uri);
    rest.is_none_or(|rest| !rest.starts_with('#'))
}

#[derive(Debug, PartialEq)]
struct QueueSkip {
    skips: usize,
    key: String,
}

/// Where the next tracks of `state` still ahead start: after the row an
/// earlier skip to `pending` lands on, or at the top once it has landed.
fn skip_from(state: &PlayerState, pending: Option<&str>) -> usize {
    let playing = playing_key(state);
    let next = &state.next_tracks;
    // That skip's Nexts are still on their way: these follow them.
    match pending {
        Some(key) if playing.as_deref() != Some(key) => (0..next.len())
            .position(|at| next_key(next, at) == key)
            .map_or(0, |at| at + 1),
        _ => 0,
    }
}

/// The row one more Next takes `state` to, after an earlier skip to
/// `pending` lands.
fn next_lands_on(state: &PlayerState, pending: Option<&str>) -> Option<String> {
    let next = &state.next_tracks;
    (skip_from(state, pending)..next.len())
        .find(|&at| next_stops_on(&next[at]))
        .map(|at| next_key(next, at))
}

/// The Nexts that take `state` to the `occurrence`th copy of `uri` in its
/// next tracks, after the row an earlier skip to `pending` lands on.
fn queue_skip(
    state: &PlayerState,
    pending: Option<&str>,
    uri: &str,
    occurrence: usize,
) -> Option<QueueSkip> {
    let next = &state.next_tracks;
    let from = skip_from(state, pending);
    let Some((at, _)) = next
        .iter()
        .enumerate()
        .skip(from)
        .filter(|(_, track)| track.uri == uri)
        .nth(occurrence)
    else {
        // A row above the one being skipped to: Next cannot go back to
        // it, so that skip stands.
        return pending
            .filter(|_| from > 0 && next[..from].iter().any(|track| track.uri == uri))
            .map(|key| QueueSkip {
                skips: 0,
                key: key.to_string(),
            });
    };
    // Next passes an unavailable row too, onto the song after it.
    let skips = next[from..at]
        .iter()
        .filter(|track| next_stops_on(track))
        .count()
        + 1;
    Some(QueueSkip {
        skips,
        key: next_key(next, at),
    })
}

/// What to pick up: `resume` as it was when the session dropped, or where a
/// song the player carried through the reconnect is `now`.
fn resume_from(resume: PlaybackResume, now: Option<&Interrupted>) -> PlaybackResume {
    let Some(now) = now else {
        return resume;
    };
    match resume {
        PlaybackResume::Session(snapshot) => {
            PlaybackResume::Session(Arc::new(snapshot.at_position(now.position_ms, now.playing)))
        }
        PlaybackResume::Track(mut spec) => {
            if spec.uris.first() == Some(&now.uri) {
                spec.position_ms = now.position_ms;
                spec.play = now.playing;
            }
            PlaybackResume::Track(spec)
        }
    }
}

/// The session is gone. librespot's player does not stop with it, so a
/// song playing or paused stays as it is until the next session takes it
/// over or the player is stopped (#268); a shutdown asked for stops it.
fn session_ended(state: &mut LocalState, shut_down: bool) {
    state.connected = false;
    if shut_down || !matches!(state.playback, Playback::Playing | Playback::Paused) {
        state.playback = Playback::Stopped;
        state.loading = false;
        state.position_at = None;
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
/// It is the only sink: librespot's own backends are not built in.
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
    let notify_device = Arc::clone(&notify);
    let report: ErrorHook = Arc::new(move |message: String| {
        let snapshot = {
            let mut current = state.lock().unwrap_or_else(|p| p.into_inner());
            current.error = Some(message);
            current.clone()
        };
        notify(EngineEvent::State(snapshot));
    });
    let saved: DeviceHook = Arc::new(move |name, id| {
        notify_device(EngineEvent::AudioDevice {
            name,
            id: id.as_str().to_string(),
        });
    });
    let volume = mixer.get_soft_volume();
    // The output applies volume to queued audio. The wrapper reads the same
    // value to calculate the pre-volume limiter ceiling.
    let ceiling = mixer.get_soft_volume();
    (
        Box::new(move || {
            let sink = Box::new(
                RodioSink::new(device, report, volume, buffer_ms, Arc::clone(&audio))
                    .on_device(saved),
            );
            Box::new(Tapped::new(
                sink,
                audio,
                tap,
                ceiling,
                false,
                eq,
                normalisation,
            )) as Box<dyn Sink>
        }),
        Box::new(NoOpVolume),
    )
}

/// What an engine is heard at, for the engine that replaces it.
#[derive(Clone)]
pub(crate) struct Heard {
    state: Arc<Mutex<LocalState>>,
}

impl Heard {
    /// The level set last, which the state holds exactly: a level set here
    /// goes into the state with the mixer, and Connect reports every other
    /// change of the level.
    pub(crate) fn level(&self) -> u16 {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).volume
    }

    /// An engine heard at `volume`, for tests that have no engine.
    #[cfg(test)]
    pub(crate) fn at(volume: u16) -> Self {
        Self {
            state: Arc::new(Mutex::new(LocalState {
                volume,
                ..LocalState::default()
            })),
        }
    }
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
            changed |= set(&mut state.loading, false);
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
            changed |= set(&mut state.loading, true);
            changed |= set(&mut state.position_ms, position_ms);
            changed |= set(&mut state.position_at, None);
            changed |= set(&mut state.error, None);
            changed
        }
        PlayerEvent::Playing { position_ms, .. } => {
            set(&mut state.playback, Playback::Playing);
            set(&mut state.position_ms, position_ms);
            state.position_at = Some(Instant::now());
            state.loading = false;
            start_replay(state);
            true
        }
        PlayerEvent::Paused { position_ms, .. } => {
            let mut changed = set(&mut state.playback, Playback::Paused);
            changed |= set(&mut state.loading, false);
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
        PlayerEvent::Unavailable { track_id, .. } => {
            // A failed load never reaches Playing, so nothing else would
            // turn the spinner off.
            let mut changed = set(
                &mut state.error,
                Some(format!(
                    "This item isn't available: {}",
                    track_id.to_uri()
                )),
            );
            changed |= set(&mut state.loading, false);
            changed
        }
        PlayerEvent::AudioKeyUnavailable { .. } => {
            let mut changed = set(&mut state.error, Some(AUDIO_KEY_REFUSED.into()));
            changed |= set(&mut state.loading, false);
            changed
        }
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
        | PlayerEvent::FilterExplicitContentChanged { .. }
        // Only sent when `ConnectConfig::emit_set_queue_events` asks for it.
        | PlayerEvent::SetQueue { .. } => false,
    }
}

fn local_track(item: &AudioItem) -> LocalTrack {
    let (artists, album, is_episode) = match &item.unique_fields {
        UniqueFields::Track { artists, album, .. } => (
            artists
                .iter()
                .map(|artist| {
                    let uri = Some(artist.id.to_uri());
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

/// The request Spotify's own client makes for a track's lyrics, which names
/// the album cover. Every lookup without the cover came back 404, so it stays
/// only for a track whose cover could not be read.
fn lyrics_endpoint(id: &SpotifyId, cover: Option<&FileId>) -> String {
    let track = id.to_base62();
    let query = "format=json&vocalRemoval=false&market=from_token";
    match cover {
        Some(cover) => {
            format!("/color-lyrics/v2/track/{track}/image/spotify:image:{cover}?{query}")
        }
        None => format!("/color-lyrics/v2/track/{track}?{query}"),
    }
}

/// What came back for a lyrics request, and what to log of it: its shape and
/// size, never its words.
fn read_lyrics(bytes: &[u8]) -> (Option<serde_json::Value>, String) {
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return (None, format!("unparseable, {} bytes", bytes.len()));
    };
    let lines = json
        .get("lyrics")
        .and_then(|lyrics| lyrics.get("lines"))
        .and_then(|lines| lines.as_array())
        .map(Vec::len);
    let outcome = match lines {
        Some(lines) => format!("found, {lines} lines"),
        None => format!("no lines in the answer, {} bytes", bytes.len()),
    };
    (Some(json), outcome)
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
            uri: uri().to_uri(),
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

    /// #268: a dropped session leaves the song playing or paused where it
    /// is, for the next session to pick up; a shutdown stops it.
    #[test]
    fn a_dropped_session_keeps_the_song_and_a_shutdown_stops_it() {
        let playing = LocalState {
            connected: true,
            playback: Playback::Playing,
            track: Some(LocalTrack {
                uri: "spotify:track:left".into(),
                ..LocalTrack::default()
            }),
            position_ms: 61_000,
            position_at: Some(Instant::now()),
            ..LocalState::default()
        };
        for playback in [Playback::Playing, Playback::Paused] {
            let mut dropped = LocalState {
                playback,
                ..playing.clone()
            };
            session_ended(&mut dropped, false);
            assert!(!dropped.connected);
            assert_eq!(dropped.playback, playback);
            assert!(dropped.interrupted().is_some());
        }

        let mut shut = playing.clone();
        session_ended(&mut shut, true);
        assert_eq!(shut.playback, Playback::Stopped);
        assert_eq!(shut.position_at, None);

        let mut loading = LocalState {
            playback: Playback::Loading,
            loading: true,
            ..playing
        };
        session_ended(&mut loading, false);
        assert_eq!(loading.playback, Playback::Stopped);
        assert!(!loading.loading);
    }

    /// #268: the pickup of a song the player carried through the reconnect
    /// starts where the song has got to, as it plays now; another song's
    /// pickup is left as it was.
    #[test]
    fn a_carried_song_is_picked_up_where_it_has_got_to() {
        let spec = LoadSpec {
            uris: vec!["spotify:track:left".into()],
            position_ms: 42_000,
            play: true,
            ..LoadSpec::default()
        };
        let now = Interrupted {
            uri: "spotify:track:left".into(),
            position_ms: 47_500,
            playing: false,
        };
        let PlaybackResume::Track(picked) =
            resume_from(PlaybackResume::Track(spec.clone()), Some(&now))
        else {
            panic!("a track pickup stays one");
        };
        assert_eq!((picked.position_ms, picked.play), (47_500, false));

        let elsewhere = Interrupted {
            uri: "spotify:track:other".into(),
            ..now
        };
        let PlaybackResume::Track(kept) =
            resume_from(PlaybackResume::Track(spec.clone()), Some(&elsewhere))
        else {
            panic!("a track pickup stays one");
        };
        assert_eq!(kept, spec);

        let PlaybackResume::Track(untouched) =
            resume_from(PlaybackResume::Track(spec.clone()), None)
        else {
            panic!("a track pickup stays one");
        };
        assert_eq!(untouched, spec);
    }

    fn uri() -> SpotifyUri {
        SpotifyUri::from_uri("spotify:track:14XWXWv5FoCbFzLksawpEe").unwrap()
    }

    fn interlude() -> AudioItem {
        AudioItem {
            track_id: uri(),
            uri: uri().to_uri(),
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

    /// Every skip and end-of-track advance loads while the previous track
    /// still shows as playing. The load itself is carried by `loading`, so
    /// the button spinner follows the engine without flipping the transport.
    #[test]
    fn a_mid_song_load_marks_the_state_loading_until_it_starts() {
        let mut state = LocalState {
            playback: Playback::Playing,
            ..LocalState::default()
        };
        assert!(apply_event(
            &mut state,
            PlayerEvent::Loading {
                play_request_id: 2,
                track_id: uri(),
                position_ms: 30_000,
            },
        ));
        assert_eq!(state.playback, Playback::Playing);
        assert!(state.loading);

        apply_event(
            &mut state,
            PlayerEvent::Playing {
                play_request_id: 2,
                track_id: uri(),
                position_ms: 0,
            },
        );
        assert!(!state.loading);

        apply_event(
            &mut state,
            PlayerEvent::Loading {
                play_request_id: 3,
                track_id: uri(),
                position_ms: 0,
            },
        );
        assert!(state.loading);
        apply_event(
            &mut state,
            PlayerEvent::Stopped {
                play_request_id: 3,
                track_id: uri(),
            },
        );
        assert!(!state.loading);

        // A load that fails has no Playing to clear the spinner, so the
        // failure events must do it themselves.
        for failure in 4..=5 {
            apply_event(
                &mut state,
                PlayerEvent::Loading {
                    play_request_id: failure,
                    track_id: uri(),
                    position_ms: 0,
                },
            );
            assert!(state.loading);
            let failed = if failure == 4 {
                PlayerEvent::Unavailable {
                    play_request_id: failure,
                    track_id: uri(),
                }
            } else {
                PlayerEvent::AudioKeyUnavailable {
                    play_request_id: failure,
                    track_id: uri(),
                }
            };
            apply_event(&mut state, failed);
            assert!(!state.loading, "failure {failure} stops the spinner");
            assert!(state.error.is_some(), "failure {failure} reports why");
        }
    }

    fn provided(uri: &str, uid: &str, provider: &str) -> ProvidedTrack {
        ProvidedTrack {
            uri: uri.into(),
            uid: uid.into(),
            provider: provider.into(),
            ..ProvidedTrack::default()
        }
    }

    fn player_state(playing: ProvidedTrack, next: Vec<ProvidedTrack>) -> PlayerState {
        PlayerState {
            track: protobuf::MessageField::some(playing),
            next_tracks: next,
            ..PlayerState::default()
        }
    }

    #[test]
    fn queue_edit_moves_the_chosen_copy_and_keeps_hidden_and_unshown_tracks() {
        let state = player_state(
            provided("now", "n", "context"),
            vec![
                provided("a", "q1", "queue"),
                provided("a", "first", "context"),
                provided("spotify:delimiter", "delimiter1", "context"),
                provided("a", "second", "context"),
                provided("tail", "t", "autoplay"),
            ],
        );
        let mut edit = super::QueueEdit {
            playing: "now".into(),
            before: vec!["a".into(); 3],
            manual: 1,
            operation: super::QueueEditOperation::Move {
                from: 2,
                to: 0,
                into_manual: true,
            },
        };
        let body = edit.command(&state).unwrap();
        let next = body["command"]["next_tracks"].as_array().unwrap();
        assert_eq!(
            next.iter()
                .map(|row| row["uid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["second", "q1", "first", "delimiter1", "t"]
        );
        assert_eq!(next[0]["metadata"]["is_queued"], "true");
        assert_eq!(next[0]["provider"], "queue");
        edit.playing = "advanced".into();
        assert!(edit.command(&state).is_none());
        edit.playing = "now".into();
        edit.before[1] = "changed".into();
        assert!(edit.command(&state).is_none());
    }

    #[test]
    fn queue_edits_use_the_same_order_as_the_player_view() {
        let mut state = player_state(
            provided("now", "n", "context"),
            vec![
                provided("a", "first", "context"),
                provided("b", "manual", "queue"),
                provided("hidden", "x", "unavailable"),
                provided("a", "second", "context"),
            ],
        );
        state.queue_revision = "revision-from-receiver".into();
        let before = super::RemoteQueueView::from_state("device".into(), &state);
        let edit = super::QueueEdit {
            playing: before.track,
            before: before.rows.into_iter().map(|(uri, _)| uri).collect(),
            manual: 1,
            operation: super::QueueEditOperation::Move {
                from: 0,
                to: 2,
                into_manual: false,
            },
        };
        let body = edit.command(&state).unwrap();
        assert_eq!(body["command"]["queue_revision"], "revision-from-receiver");
        let rows = body["command"]["next_tracks"].as_array().unwrap();
        assert_eq!(rows[2]["uid"], "manual");
        assert_eq!(rows[2]["provider"], "context");
        assert!(!edit.confirmed(
            &super::RemoteQueueView::from_state("device".into(), &state),
            &["a".into(), "b".into(), "a".into()],
            0,
        ));
        let mut moved = state.next_tracks.remove(1);
        moved.provider = "context".into();
        state.next_tracks.insert(2, moved);
        assert!(edit.confirmed(
            &super::RemoteQueueView::from_state("device".into(), &state),
            &["a".into(), "b".into(), "a".into()],
            0,
        ));
    }

    #[test]
    fn duplicate_edits_need_a_new_revision_and_the_affected_occurrences() {
        let mut state = player_state(
            provided("now", "n", "context"),
            vec![
                provided("a", "first", "queue"),
                provided("a", "second", "queue"),
                provided("tail", "t", "context"),
            ],
        );
        state.queue_revision = "r0".into();
        let edit = super::QueueEdit {
            playing: "now".into(),
            before: vec!["a".into(), "a".into()],
            manual: 2,
            operation: super::QueueEditOperation::Move {
                from: 0,
                to: 2,
                into_manual: true,
            },
        };
        assert!(!edit.confirmed_state(&state, &state));
        let mut confirmed = state.clone();
        confirmed.queue_revision = "r1".into();
        assert!(
            !edit.confirmed_state(&state, &confirmed),
            "a revision alone cannot swap retained UIDs"
        );
        confirmed.next_tracks.swap(0, 1);
        assert!(edit.confirmed_state(&state, &confirmed));
        for (index, row) in confirmed.next_tracks.iter_mut().enumerate() {
            row.uid = format!("fresh-{index}");
        }
        assert!(
            edit.confirmed_state(&state, &confirmed),
            "some receivers replace all UIDs"
        );
        confirmed.next_tracks[2].uri = "unrelated".into();
        assert!(
            !edit.confirmed_state(&state, &confirmed),
            "the untouched tail still matters"
        );
    }

    #[test]
    fn a_move_into_next_in_queue_confirms_under_librespots_unchanged_revision() {
        // #given the first Next from row dragged to the end of Next in queue,
        // which leaves the URIs, and so librespot's revision, as they were
        let mut state = player_state(
            provided("now", "n", "context"),
            vec![
                provided("a", "manual", "queue"),
                provided("b", "first", "context"),
                provided("c", "second", "context"),
            ],
        );
        state.queue_revision = "r0".into();
        let edit = super::QueueEdit {
            playing: "now".into(),
            before: vec!["a".into(), "b".into()],
            manual: 1,
            operation: super::QueueEditOperation::Move {
                from: 1,
                to: 1,
                into_manual: true,
            },
        };
        assert!(!edit.confirmed_state(&state, &state));
        // #when the receiver publishes the rows sent #then they confirm it
        let mut applied = state.clone();
        applied.next_tracks[1].provider = "queue".into();
        assert!(edit.confirmed_state(&state, &applied));
        // #when the rows lack the UIDs sent #then only a new revision can
        for row in &mut applied.next_tracks {
            row.uid = format!("fresh-{}", row.uid);
        }
        assert!(!edit.confirmed_state(&state, &applied));
        applied.queue_revision = "r1".into();
        assert!(edit.confirmed_state(&state, &applied));
    }

    #[test]
    fn an_empty_remove_prefix_cannot_confirm_an_unrelated_rewrite() {
        let mut state = player_state(
            provided("now", "n", "context"),
            vec![provided("a", "q", "queue"), provided("b", "b", "context")],
        );
        state.queue_revision = "r0".into();
        let edit = super::QueueEdit {
            playing: "now".into(),
            before: vec!["a".into()],
            manual: 1,
            operation: super::QueueEditOperation::Remove { indexes: vec![0] },
        };
        let mut confirmed = state.clone();
        confirmed.queue_revision = "r1".into();
        confirmed.next_tracks.remove(0);
        assert!(edit.confirmed_state(&state, &confirmed));
        confirmed.next_tracks[0].uri = "unrelated".into();
        assert!(!edit.confirmed_state(&state, &confirmed));
    }

    #[test]
    fn a_remove_is_not_confirmed_while_the_removed_row_stays_queued() {
        let mut state = player_state(
            provided("now", "n", "context"),
            vec![provided("a", "qa", "queue"), provided("c", "cc", "context")],
        );
        state.queue_revision = "r0".into();
        let edit = super::QueueEdit {
            playing: "now".into(),
            before: vec!["a".into()],
            manual: 1,
            operation: super::QueueEditOperation::Remove { indexes: vec![0] },
        };
        let mut appended = state.clone();
        appended.queue_revision = "r1".into();
        appended.next_tracks.insert(1, provided("d", "d", "queue"));
        assert!(
            !edit.confirmed_state(&state, &appended),
            "the removed row is still there"
        );
        appended.next_tracks.remove(0);
        assert!(
            edit.confirmed_state(&state, &appended),
            "a song queued since is not the removed row"
        );
    }

    #[test]
    fn an_appended_manual_row_does_not_hide_a_confirmed_edit() {
        let edit = super::QueueEdit {
            playing: "now".into(),
            before: vec!["a".into(), "b".into(), "c".into()],
            manual: 1,
            operation: super::QueueEditOperation::Move {
                from: 1,
                to: 1,
                into_manual: true,
            },
        };
        let view = super::RemoteQueueView {
            track: "now".into(),
            queued: vec!["a".into(), "b".into(), "d".into()],
            rows: ["a", "b", "d", "c"]
                .into_iter()
                .map(|uri| (uri.into(), String::new()))
                .collect(),
            ..Default::default()
        };
        assert!(edit.confirmed(&view, &["a".into(), "b".into(), "c".into()], 2));
        let mut wrong = view;
        wrong.queued.swap(0, 1);
        assert!(!edit.confirmed(&wrong, &["a".into(), "b".into(), "c".into()], 2));
    }

    #[test]
    fn removing_a_context_copy_keeps_other_copies_and_requires_remove_permission() {
        let mut state = player_state(
            provided("now", "n", "context"),
            vec![
                provided("a", "manual", "queue"),
                provided("a", "first", "context"),
                provided("spotify:delimiter", "delimiter1", "context"),
                provided("a", "second", "context"),
                provided("tail", "t", "autoplay"),
            ],
        );
        let edit = super::QueueEdit {
            playing: "now".into(),
            before: vec!["a".into(); 3],
            manual: 1,
            operation: super::QueueEditOperation::Remove { indexes: vec![2] },
        };
        let body = edit.command(&state).unwrap();
        let next = body["command"]["next_tracks"].as_array().unwrap();
        assert_eq!(
            next.iter()
                .map(|row| row["uid"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["manual", "first", "delimiter1", "t"],
        );
        state
            .restrictions
            .mut_or_insert_default()
            .disallow_removing_from_next_tracks_reasons
            .push("restricted".into());
        assert!(edit.command(&state).is_none());
    }

    /// A Next up click counts the rows Next stops on, which the rows on
    /// screen do not tell: Spotify still lists a song found unavailable,
    /// and never shows a queue delimiter.
    #[test]
    fn a_queue_skip_counts_only_the_rows_next_stops_on() {
        let state = player_state(
            provided("spotify:track:now", "u0", "context"),
            vec![
                provided("spotify:track:a", "u1", "autoplay"),
                provided("spotify:track:b", "u2", "unavailable"),
                provided("spotify:delimiter", "delimiter0", "context"),
                provided("spotify:track:c", "u3", "autoplay"),
                provided("spotify:track:a", "u4", "autoplay"),
            ],
        );
        assert_eq!(
            queue_skip(&state, None, "spotify:track:c", 0),
            Some(QueueSkip {
                skips: 2,
                key: "u3".into(),
            }),
            "the screen's row count would say four"
        );
        let skips = |uri: &str, n| queue_skip(&state, None, uri, n).map(|skip| skip.skips);
        assert_eq!(skips("spotify:track:a", 1), Some(3), "the second copy");
        assert_eq!(
            skips("spotify:track:b", 0),
            Some(2),
            "Next passes an unavailable row onto the song after it"
        );
        assert_eq!(skips("spotify:track:gone", 0), None);
    }

    /// A second click while the first skip's Nexts are still on their way
    /// counts from where that skip lands, however far the player has got.
    #[test]
    fn a_second_queue_skip_counts_from_where_the_first_lands() {
        let next = vec![
            provided("spotify:track:a", "u1", "autoplay"),
            provided("spotify:track:b", "u2", "autoplay"),
            provided("spotify:track:c", "u3", "autoplay"),
            provided("spotify:track:d", "u4", "autoplay"),
        ];
        let now = provided("spotify:track:now", "u0", "context");
        let before = player_state(now, next.clone());
        let halfway = player_state(next[0].clone(), next[1..].to_vec());
        let landed = player_state(next[2].clone(), next[3..].to_vec());
        for state in [&before, &halfway, &landed] {
            assert_eq!(
                queue_skip(state, Some("u3"), "spotify:track:d", 0),
                Some(QueueSkip {
                    skips: 1,
                    key: "u4".into(),
                })
            );
        }
        assert_eq!(
            queue_skip(&before, Some("u3"), "spotify:track:b", 0),
            Some(QueueSkip {
                skips: 0,
                key: "u3".into(),
            }),
            "a row above the target leaves the first skip standing"
        );
        assert_eq!(
            queue_skip(&before, Some("gone"), "spotify:track:b", 0).map(|skip| skip.skips),
            Some(2),
            "a target the list no longer holds counts from the playing song"
        );
    }

    /// A click right after Next, while that Next still waits for its
    /// Connect state update, counts from the song the Next lands on: from
    /// a, Next and then d is two more Nexts, not three on to e.
    #[test]
    fn a_queue_skip_counts_from_where_a_pending_next_lands() {
        let next = vec![
            provided("spotify:track:b", "u1", "autoplay"),
            provided("spotify:track:c", "u2", "autoplay"),
            provided("spotify:track:d", "u3", "autoplay"),
            provided("spotify:track:e", "u4", "autoplay"),
        ];
        let before = player_state(provided("spotify:track:a", "u0", "context"), next.clone());
        let landed = player_state(next[0].clone(), next[1..].to_vec());
        let lands = next_lands_on(&before, None);
        assert_eq!(lands.as_deref(), Some("u1"));
        let to_d = |state: &PlayerState| queue_skip(state, lands.as_deref(), "spotify:track:d", 0);
        let two_more = Some(QueueSkip {
            skips: 2,
            key: "u3".into(),
        });
        assert_eq!(to_d(&before), two_more, "Next still on its way to b");
        assert_eq!(to_d(&landed), two_more, "Next already at b");
        assert_eq!(
            next_lands_on(&before, Some("u2")).as_deref(),
            Some("u3"),
            "a Next after a skip to c still on its way"
        );
        let unavailable = player_state(
            provided("spotify:track:a", "u0", "context"),
            vec![
                provided("spotify:track:b", "u1", "unavailable"),
                provided("spotify:track:c", "u2", "autoplay"),
            ],
        );
        assert_eq!(
            next_lands_on(&unavailable, None).as_deref(),
            Some("u2"),
            "Next passes an unavailable song"
        );
    }

    /// A skip sent to another device names the row by the uid that device
    /// lists it under, and a second click's skip counts from the row the
    /// first one goes to, wherever the device has got to.
    #[test]
    fn a_skip_on_another_device_names_the_row_by_its_uid() {
        let next = vec![
            provided("spotify:track:a", "u1", "context"),
            provided("spotify:track:b", "u2", "context"),
            provided("spotify:track:a", "u3", "context"),
            provided("spotify:track:c", "", "context"),
        ];
        let listed = || {
            let mut pushed = cluster("sonos", "spotify:track:now", true, 30_000);
            pushed.player_state.mut_or_insert_default().next_tracks = next.clone();
            let mut queue = RemoteQueue::default();
            queue.show(pushed);
            queue
        };
        let uid = |uri: &str, n| listed().row("sonos", uri, n);
        assert_eq!(uid("spotify:track:b", 0).as_deref(), Some("u2"));
        assert_eq!(uid("spotify:track:a", 1).as_deref(), Some("u3"));
        assert_eq!(uid("spotify:track:c", 0), None, "a row without a uid");
        assert_eq!(uid("spotify:track:gone", 0), None);
        assert_eq!(listed().row("phone", "spotify:track:b", 0), None);

        let mut queue = listed();
        let first = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(first.as_deref(), Some("u2"));
        let second = queue.row("sonos", "spotify:track:a", 0);
        assert_eq!(
            second.as_deref(),
            Some("u3"),
            "the first copy is above the row the first skip goes to"
        );
    }

    /// Once the row a skip went to plays, the next click counts from the
    /// top again, even after another app steps back to the song before it.
    #[test]
    fn a_skip_target_is_forgotten_once_its_row_plays() {
        let push = |playing: ProvidedTrack, next: &[ProvidedTrack]| {
            let mut pushed = Cluster::new();
            pushed.active_device_id = "sonos".into();
            let state = pushed.player_state.mut_or_insert_default();
            *state.track.mut_or_insert_default() = playing;
            state.next_tracks = next.to_vec();
            pushed
        };
        let a = provided("spotify:track:a", "u0", "context");
        let next = [
            provided("spotify:track:b", "u1", "context"),
            provided("spotify:track:c", "u2", "context"),
            provided("spotify:track:b", "u3", "context"),
        ];
        let mut queue = RemoteQueue::default();
        queue.show(push(a.clone(), &next));
        let first = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(first.as_deref(), Some("u1"));

        queue.show(push(next[0].clone(), &next[1..]));
        queue.show(push(a, &next));
        let again = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(again.as_deref(), Some("u1"), "not the copy after it");

        let mut target = Some(SkipTarget {
            key: "u1".into(),
            at: Instant::now(),
        });
        retire_landed(&mut target, Some("u0"));
        assert!(target.is_some(), "still on its way");
        retire_landed(&mut target, Some("u1"));
        assert!(target.is_none());
    }

    /// A Next sent to another device moves the row a click right after it
    /// counts from, before any cluster push shows it, and Previous sends
    /// the count back to the top. A command that fails gives back the row
    /// from before it.
    #[test]
    fn a_skip_on_another_device_follows_its_next_and_failures() {
        let mut pushed = cluster("sonos", "spotify:track:now", true, 30_000);
        pushed.player_state.mut_or_insert_default().next_tracks = vec![
            provided("spotify:track:b", "u1", "context"),
            provided("spotify:track:c", "u2", "context"),
            provided("spotify:track:b", "u3", "context"),
            provided("spotify:track:d", "u4", "context"),
        ];
        let mut queue = RemoteQueue::default();
        queue.show(pushed);

        queue.note_next("sonos");
        let after_next = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(after_next.as_deref(), Some("u3"), "not the copy passed");

        queue.note_previous();
        let after_previous = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(after_previous.as_deref(), Some("u1"));

        let refused = queue.row("sonos", "spotify:track:c", 0);
        assert_eq!(refused.as_deref(), Some("u2"));
        queue.refused();
        let again = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(again.as_deref(), Some("u3"), "after the first skip");
        queue.refused();
        assert!(queue.target.is_some(), "the first skip stands");
    }

    /// Copies of a song without a uid keep their places: a Next after a
    /// skip to the second one lands after it, and a Next per row that the
    /// device stops partway counts on from the rows it passed.
    #[test]
    fn copies_without_a_uid_keep_their_places() {
        let listed = || {
            let mut pushed = cluster("sonos", "spotify:track:now", true, 30_000);
            pushed.player_state.mut_or_insert_default().next_tracks = vec![
                provided("spotify:track:a", "", "context"),
                provided("spotify:track:b", "u2", "context"),
                provided("spotify:track:a", "", "context"),
                provided("spotify:track:b", "u4", "context"),
                provided("spotify:track:c", "u5", "context"),
                provided("spotify:track:b", "u6", "context"),
            ];
            let mut queue = RemoteQueue::default();
            queue.show(pushed);
            queue
        };

        let mut queue = listed();
        assert_eq!(queue.row("sonos", "spotify:track:a", 1), None, "no uid");
        queue.note_next("sonos");
        let after_next = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(after_next.as_deref(), Some("u6"), "not the first a's b");

        let mut queue = listed();
        assert_eq!(queue.row("sonos", "spotify:track:a", 1), None);
        queue.stopped("sonos", 2);
        let after_two = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(after_two.as_deref(), Some("u4"), "two Nexts went through");

        // A push that shows the second Next landed before the third failed
        // leaves the two that went through counted once.
        let mut queue = listed();
        assert_eq!(queue.row("sonos", "spotify:track:a", 1), None);
        let mut pushed = cluster("sonos", "spotify:track:b", true, 30_000);
        let state = pushed.player_state.mut_or_insert_default();
        state.track.mut_or_insert_default().uid = "u2".into();
        state.next_tracks = vec![
            provided("spotify:track:a", "", "context"),
            provided("spotify:track:b", "u4", "context"),
            provided("spotify:track:c", "u5", "context"),
            provided("spotify:track:b", "u6", "context"),
        ];
        queue.show(pushed);
        queue.stopped("sonos", 2);
        let after_push = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(after_push.as_deref(), Some("u4"), "counted once");
    }

    /// A Next per row stopped partway after an earlier skip counts on from
    /// that skip's row, even once a push has shown it playing.
    #[test]
    fn a_stopped_skip_counts_on_from_the_skip_before_it() {
        let mut pushed = cluster("sonos", "spotify:track:now", true, 30_000);
        pushed.player_state.mut_or_insert_default().next_tracks = vec![
            provided("spotify:track:x", "u0", "context"),
            provided("spotify:track:a", "", "context"),
            provided("spotify:track:b", "u2", "context"),
            provided("spotify:track:a", "", "context"),
            provided("spotify:track:b", "u4", "context"),
            provided("spotify:track:a", "", "context"),
            provided("spotify:track:b", "u6", "context"),
        ];
        let mut queue = RemoteQueue::default();
        queue.show(pushed.clone());
        // #given a skip to the first b, then one to the second a after it
        let first = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(first.as_deref(), Some("u2"));
        assert_eq!(queue.row("sonos", "spotify:track:a", 1), None);
        // #when the push for the first skip comes in, and the second stops
        // after two of its Nexts
        let mut landed = cluster("sonos", "spotify:track:b", true, 30_000);
        let state = landed.player_state.mut_or_insert_default();
        state.track.mut_or_insert_default().uid = "u2".into();
        state.next_tracks = vec![
            provided("spotify:track:a", "", "context"),
            provided("spotify:track:b", "u4", "context"),
            provided("spotify:track:a", "", "context"),
            provided("spotify:track:b", "u6", "context"),
        ];
        queue.show(landed);
        queue.stopped("sonos", 2);
        // #then the next b is the one after the b those Nexts reached
        let after = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(after.as_deref(), Some("u6"));

        // #and so when no push came, but the first skip's row has outlived
        // a skip by the time the second stops
        let mut queue = RemoteQueue::default();
        queue.show(pushed);
        let first = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(first.as_deref(), Some("u2"));
        assert_eq!(queue.row("sonos", "spotify:track:a", 1), None);
        let from = queue.planned_from.as_mut().expect("the first skip's row");
        from.at -= SKIP_TARGET_LIFE;
        queue.stopped("sonos", 2);
        let after = queue.row("sonos", "spotify:track:b", 0);
        assert_eq!(after.as_deref(), Some("u6"), "not the b passed");
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
        assert_eq!(state.error.as_deref(), Some(AUDIO_KEY_REFUSED));
        assert!(!state.loading);
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
            audio_device: None,
            initial_volume: 1,
            volume_dir: PathBuf::new(),
            audio_cache_dir: None,
            audio_cache_limit: None,
            proxy: crate::settings::ProxyConfig::Off,
            local_files: None,
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

    /// The next engine starts at the level set last, exactly.
    #[test]
    fn an_engine_is_heard_at_the_level_set_last() {
        let set_here = 3276;
        assert_eq!(Heard::at(set_here).level(), set_here);
    }
    fn cluster(active: &str, track: &str, playing: bool, volume: u32) -> Cluster {
        let mut cluster = Cluster::new();
        cluster.active_device_id = active.into();
        let state = cluster.player_state.mut_or_insert_default();
        state.track.mut_or_insert_default().uri = track.into();
        state.is_playing = playing;
        let mut device = librespot_protocol::connect::DeviceInfo::new();
        device.volume = volume;
        cluster.device.insert(active.into(), device);
        cluster
    }

    /// A knob turned on the speaker that plays is news; the position
    /// heartbeats Spotify pushes in between are not.
    #[test]
    fn another_devices_volume_is_news_and_its_heartbeat_is_not() {
        let before = cluster("sonos", "spotify:track:a", true, 30_000);
        let shown = ClusterView::of(&before);

        let mut heartbeat = before.clone();
        let state = heartbeat.player_state.mut_or_insert_default();
        state.timestamp = 1_000;
        state.position_as_of_timestamp = 61_000;
        assert!(!shown.changed_elsewhere(&ClusterView::of(&heartbeat), "this"));

        let louder = cluster("sonos", "spotify:track:a", true, 32_000);
        assert!(shown.changed_elsewhere(&ClusterView::of(&louder), "this"));
        assert_eq!(shown.volume_only(&ClusterView::of(&louder)), Some(32_000));
    }

    /// A new volume alone is shown as pushed; with anything else it waits
    /// for the poll, which brings both.
    #[test]
    fn a_volume_alone_is_carried_and_with_more_it_is_polled() {
        let shown = ClusterView::of(&cluster("sonos", "spotify:track:a", true, 30_000));
        for next in [
            cluster("phone", "spotify:track:a", true, 32_000),
            cluster("sonos", "spotify:track:b", true, 32_000),
            cluster("sonos", "spotify:track:a", false, 32_000),
        ] {
            assert_eq!(shown.volume_only(&ClusterView::of(&next)), None);
        }
        let mut gone = cluster("sonos", "spotify:track:a", true, 30_000);
        gone.device.clear();
        assert_eq!(shown.volume_only(&ClusterView::of(&gone)), None);
    }

    #[test]
    fn a_switch_pause_or_new_song_elsewhere_is_news() {
        let shown = ClusterView::of(&cluster("sonos", "spotify:track:a", true, 30_000));
        for next in [
            cluster("phone", "spotify:track:a", true, 30_000),
            cluster("sonos", "spotify:track:a", false, 30_000),
            cluster("sonos", "spotify:track:b", true, 30_000),
        ] {
            assert!(shown.changed_elsewhere(&ClusterView::of(&next), "this"));
        }
        let mut shuffled = cluster("sonos", "spotify:track:a", true, 30_000);
        shuffled
            .player_state
            .mut_or_insert_default()
            .options
            .mut_or_insert_default()
            .shuffling_context = true;
        assert!(shown.changed_elsewhere(&ClusterView::of(&shuffled), "this"));
        // Another collection started at the same song changes Playing from.
        let mut playlist = cluster("sonos", "spotify:track:a", true, 30_000);
        playlist.player_state.mut_or_insert_default().context_uri = "spotify:playlist:b".into();
        assert!(shown.changed_elsewhere(&ClusterView::of(&playlist), "this"));
    }

    /// The first push after connecting is often already a change on another
    /// device, such as a pause on the phone, so it is news and polls.
    #[test]
    fn the_first_push_after_connecting_is_news() {
        let first = ClusterView::of(&cluster("sonos", "spotify:track:a", false, 30_000));
        assert!(ClusterView::default().changed_elsewhere(&first, "this"));
        assert_eq!(ClusterView::default().volume_only(&first), None);
    }

    /// While this device plays, its own events carry every change, so its
    /// pushes ask for no poll. Playback leaving it, or arriving, does.
    #[test]
    fn this_devices_own_playback_is_not_news_but_leaving_it_is() {
        let ours = ClusterView::of(&cluster("this", "spotify:track:a", true, 30_000));
        let next_song = ClusterView::of(&cluster("this", "spotify:track:b", true, 20_000));
        assert!(!ours.changed_elsewhere(&next_song, "this"));

        let elsewhere = ClusterView::of(&cluster("sonos", "spotify:track:b", true, 20_000));
        assert!(ours.changed_elsewhere(&elsewhere, "this"));
        assert!(elsewhere.changed_elsewhere(&ours, "this"));
    }

    /// The log says which of the ways to no lyrics it was, without the words.
    #[test]
    fn a_lyrics_answer_is_described_by_its_shape_and_never_its_words() {
        let (json, outcome) = read_lyrics(b"<html>secret</html>");
        assert!(json.is_none());
        assert_eq!(outcome, "unparseable, 19 bytes");
        assert_eq!(read_lyrics(b"").1, "unparseable, 0 bytes");

        let body = br#"{"lyrics":{"lines":[{"words":"secret"},{"words":"words"}]}}"#;
        let (json, outcome) = read_lyrics(body);
        assert!(json.is_some());
        assert_eq!(outcome, "found, 2 lines");

        let (json, outcome) = read_lyrics(b"{\"message\":\"secret\"}");
        assert!(json.is_some(), "a body the caller still gets to read");
        assert_eq!(outcome, "no lines in the answer, 20 bytes");
    }

    /// The official client's request, cover included; librespot adds its
    /// `product`, `country` and `salt` after an `&`.
    #[test]
    fn the_lyrics_request_names_the_album_cover() {
        let id = SpotifyId::from_base62("4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let cover = FileId::from_raw(&[0xab; 20]);
        assert_eq!(
            lyrics_endpoint(&id, Some(&cover)),
            format!(
                "/color-lyrics/v2/track/4uLU6hMCjMI75M1A2tKUQC/image/spotify:image:{}\
                 ?format=json&vocalRemoval=false&market=from_token",
                "ab".repeat(20)
            )
        );
        assert_eq!(
            lyrics_endpoint(&id, None),
            "/color-lyrics/v2/track/4uLU6hMCjMI75M1A2tKUQC\
             ?format=json&vocalRemoval=false&market=from_token"
        );
    }
}
