//! Linux desktop media controls (MPRIS) for Spotifast.
//!
//! D-Bus runs on its own thread with a local executor and exchanges bounded
//! messages with the interface, which stays the only owner of playback
//! decisions. A slow or absent session bus therefore cannot stall audio or
//! the window.

use std::cell::Cell;
use std::future::Future;
use std::sync::mpsc::{Receiver, Sender};
use std::task::{Context, Waker};
use std::thread;
use std::time::{Duration, Instant};

use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player, Time, TrackId};
use tokio::sync::mpsc as tokio_mpsc;

use crate::media::{MediaCommand, MediaState, MediaTrack};
use crate::player::{Playback, RepeatMode};

const PLAYING_POSITION_INTERVAL: Duration = Duration::from_millis(1000);
const TRACK_OBJECT_PATH_PREFIX: &str = "/rocks/spotifast/Track/";

enum Update {
    State(MediaState, bool),
    Seeked(u32),
}

pub struct MediaService {
    updates: tokio_mpsc::UnboundedSender<Update>,
    commands: Receiver<MediaCommand>,
    published: Option<MediaState>,
    /// A client set the volume, and the bus now holds its level rather than
    /// the interface's. The next update republishes whatever the interface
    /// settled on, even when that is the level it already had.
    volume_requested: Cell<bool>,
    last_position_update: Instant,
}

impl MediaService {
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (updates, update_rx) = tokio_mpsc::unbounded_channel();
        let (command_tx, commands) = std::sync::mpsc::channel();
        let wake: std::sync::Arc<dyn Fn() + Send + Sync> = std::sync::Arc::new(wake);
        let spawned = thread::Builder::new()
            .name("spotifast-mpris".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        log::warn!("MPRIS runtime unavailable: {error}");
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                let outcome = local.block_on(&runtime, run(update_rx, command_tx, wake));
                if let Err(error) = outcome {
                    log::warn!("MPRIS is unavailable: {error}");
                }
            });
        if let Err(error) = spawned {
            log::warn!("unable to start the MPRIS thread: {error}");
        }
        Self {
            updates,
            commands,
            published: None,
            volume_requested: Cell::new(false),
            last_position_update: Instant::now() - PLAYING_POSITION_INTERVAL,
        }
    }

    pub fn drain_commands(&self) -> Vec<MediaCommand> {
        let commands: Vec<MediaCommand> = self.commands.try_iter().collect();
        if commands
            .iter()
            .any(|command| matches!(command, MediaCommand::SetVolume(_)))
        {
            self.volume_requested.set(true);
        }
        commands
    }

    /// Publishes structural changes immediately; the position once a second
    /// while playing, since clients interpolate between updates.
    pub fn update(&mut self, state: MediaState) {
        let volume_requested = self.volume_requested.take();
        let structural = volume_requested
            || self
                .published
                .as_ref()
                .is_none_or(|published| !same_except_position(published, &state));
        let position_due = state.playback != Playback::Playing
            || self.last_position_update.elapsed() >= PLAYING_POSITION_INTERVAL;
        let position_changed = self
            .published
            .as_ref()
            .is_none_or(|published| published.position_ms != state.position_ms);
        if !structural && (!position_changed || !position_due) {
            return;
        }
        if self
            .updates
            .send(Update::State(state.clone(), volume_requested))
            .is_ok()
        {
            self.published = Some(state);
            self.last_position_update = Instant::now();
        }
    }

    pub fn seeked(&self, position_ms: u32) {
        let _ = self.updates.send(Update::Seeked(position_ms));
    }
}

fn same_except_position(left: &MediaState, right: &MediaState) -> bool {
    left.playback == right.playback
        && left.track == right.track
        && (left.volume - right.volume).abs() < 0.005
        && left.shuffle == right.shuffle
        && left.repeat == right.repeat
        && left.can_control == right.can_control
}

async fn run(
    mut updates: tokio_mpsc::UnboundedReceiver<Update>,
    commands: Sender<MediaCommand>,
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
) -> mpris_server::zbus::Result<()> {
    let player = Player::builder("spotifast")
        .identity("Spotifast")
        .desktop_entry(desktop_entry())
        .can_raise(true)
        .can_quit(true)
        .can_control(true)
        .can_play(true)
        .can_pause(true)
        .can_go_next(true)
        .can_go_previous(true)
        .can_seek(true)
        .supported_uri_schemes(vec![
            "spotify".to_string(),
            "https".to_string(),
            "http".to_string(),
        ])
        .build()
        .await?;

    let send = {
        let commands = commands.clone();
        let wake = wake.clone();
        move |command: MediaCommand| {
            if commands.send(command).is_ok() {
                wake();
            }
        }
    };
    {
        let send = send.clone();
        player.connect_play(move |_| send(MediaCommand::Play));
    }
    {
        let send = send.clone();
        player.connect_pause(move |_| send(MediaCommand::Pause));
    }
    {
        let send = send.clone();
        player.connect_play_pause(move |_| send(MediaCommand::PlayPause));
    }
    {
        let send = send.clone();
        player.connect_stop(move |_| send(MediaCommand::Stop));
    }
    {
        let send = send.clone();
        player.connect_next(move |_| send(MediaCommand::Next));
    }
    {
        let send = send.clone();
        player.connect_previous(move |_| send(MediaCommand::Previous));
    }
    {
        let send = send.clone();
        player.connect_seek(move |_, offset| send(MediaCommand::SeekBy(offset.as_millis())));
    }
    {
        let send = send.clone();
        player.connect_set_position(move |_, track_id, position| {
            if let Some(uri) = uri_from_object_path(track_id.as_str()) {
                send(MediaCommand::SetPosition {
                    track_uri: uri,
                    position_ms: position.as_millis().max(0) as u32,
                });
            }
        });
    }
    {
        let send = send.clone();
        player.connect_set_volume(move |player, volume| {
            let volume = volume.clamp(0.0, 1.0);
            hold_volume(player, volume);
            send(MediaCommand::SetVolume(volume));
        });
    }
    {
        let send = send.clone();
        player.connect_set_shuffle(move |_, shuffle| send(MediaCommand::SetShuffle(shuffle)));
    }
    {
        let send = send.clone();
        player.connect_set_loop_status(move |_, status| {
            send(MediaCommand::SetRepeat(match status {
                LoopStatus::None => RepeatMode::Off,
                LoopStatus::Track => RepeatMode::Track,
                LoopStatus::Playlist => RepeatMode::Context,
            }))
        });
    }
    {
        let send = send.clone();
        player.connect_open_uri(move |_, uri| send(MediaCommand::OpenUri(uri.to_string())));
    }
    {
        let send = send.clone();
        player.connect_raise(move |_| send(MediaCommand::Raise));
    }
    {
        let send = send.clone();
        player.connect_quit(move |_| send(MediaCommand::Quit));
    }

    let server = player.run();
    let apply = async {
        let mut published: Option<MediaState> = None;
        while let Some(update) = updates.recv().await {
            match update {
                Update::Seeked(position_ms) => {
                    let _ = player.seeked(Time::from_millis(position_ms as i64)).await;
                }
                Update::State(state, volume_requested) => {
                    let previous = published.as_ref();
                    if previous.is_none_or(|p| p.playback != state.playback) {
                        let _ = player
                            .set_playback_status(playback_status(state.playback))
                            .await;
                    }
                    if previous.is_none_or(|p| p.track != state.track) {
                        let _ = player.set_metadata(metadata(state.track.as_ref())).await;
                        let _ = player.set_can_seek(state.track.is_some()).await;
                    }
                    if volume_requested
                        || previous.is_none_or(|p| (p.volume - state.volume).abs() >= 0.005)
                    {
                        let _ = player.set_volume(state.volume).await;
                    }
                    if previous.is_none_or(|p| p.shuffle != state.shuffle) {
                        let _ = player.set_shuffle(state.shuffle).await;
                    }
                    if previous.is_none_or(|p| p.repeat != state.repeat) {
                        let _ = player.set_loop_status(loop_status(state.repeat)).await;
                    }
                    player.set_position(Time::from_millis(state.position_ms as i64));
                    published = Some(state);
                }
            }
        }
    };
    tokio::select! {
        _ = server => {}
        _ = apply => {}
    }
    Ok(())
}

fn hold_volume(player: &Player, volume: f64) {
    let update = std::pin::pin!(player.set_volume(volume));
    let _ = update.poll(&mut Context::from_waker(Waker::noop()));
}

fn playback_status(playback: Playback) -> PlaybackStatus {
    match playback {
        Playback::Playing => PlaybackStatus::Playing,
        Playback::Paused | Playback::Loading => PlaybackStatus::Paused,
        Playback::Stopped => PlaybackStatus::Stopped,
    }
}

fn loop_status(mode: RepeatMode) -> LoopStatus {
    match mode {
        RepeatMode::Off => LoopStatus::None,
        RepeatMode::Context => LoopStatus::Playlist,
        RepeatMode::Track => LoopStatus::Track,
    }
}

fn metadata(track: Option<&MediaTrack>) -> Metadata {
    let Some(track) = track else {
        return Metadata::new();
    };
    let mut builder = Metadata::builder()
        .title(track.title.clone())
        .length(Time::from_millis(track.duration_ms as i64))
        .url(track.uri.clone());
    if let Some(track_id) = object_path_for(&track.uri) {
        builder = builder.trackid(track_id);
    }
    if !track.artists.is_empty() {
        builder = builder.artist(track.artists.clone());
    }
    if !track.album.is_empty() {
        builder = builder.album(track.album.clone());
    }
    if let Some(art) = &track.art_url {
        builder = builder.art_url(art.clone());
    }
    builder.build()
}

fn object_path_for(uri: &str) -> Option<TrackId> {
    let id: String = uri
        .rsplit(':')
        .next()
        .unwrap_or("track")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let id = if id.is_empty() {
        "track".to_string()
    } else {
        id
    };
    let kind = crate::util::uri_kind(uri).unwrap_or("track");
    TrackId::try_from(format!("{TRACK_OBJECT_PATH_PREFIX}{kind}_{id}")).ok()
}

fn uri_from_object_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix(TRACK_OBJECT_PATH_PREFIX)?;
    let (kind, id) = rest.split_once('_')?;
    Some(format!("spotify:{kind}:{id}"))
}

/// The desktop entry's name: inside a Flatpak the entry is exported under
/// the app id, and a desktop looking it up by the plain name finds nothing.
pub fn desktop_entry() -> String {
    let app_id = std::env::var("FLATPAK_ID").ok();
    desktop_entry_for(
        app_id.as_deref(),
        std::path::Path::new("/.flatpak-info").exists(),
    )
    .to_owned()
}

fn desktop_entry_for(app_id: Option<&str>, in_flatpak: bool) -> &str {
    if let Some(app_id) = app_id.filter(|id| !id.is_empty()) {
        app_id
    } else if in_flatpak {
        "rocks.spotifast.Spotifast"
    } else {
        "spotifast"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_matches_the_installed_flatpak_id() {
        for id in ["rocks.spotifast.Spotifast", "rocks.fastpotify.Fastpotify"] {
            assert_eq!(desktop_entry_for(Some(id), true), id);
        }
        assert_eq!(desktop_entry_for(None, false), "spotifast");
        assert_eq!(desktop_entry_for(None, true), "rocks.spotifast.Spotifast");
        assert_eq!(
            desktop_entry_for(Some(""), true),
            "rocks.spotifast.Spotifast"
        );
    }

    #[test]
    fn a_volume_request_republishes_the_level_even_when_unchanged() {
        let (updates, mut update_rx) = tokio_mpsc::unbounded_channel();
        let (command_tx, commands) = std::sync::mpsc::channel();
        let mut service = MediaService {
            updates,
            commands,
            published: None,
            volume_requested: Cell::new(false),
            last_position_update: Instant::now(),
        };
        let state = MediaState {
            volume: 0.75,
            ..MediaState::default()
        };
        service.update(state.clone());
        assert!(matches!(update_rx.try_recv(), Ok(Update::State(_, false))));
        service.update(state.clone());
        assert!(update_rx.try_recv().is_err());

        // The client asked for a level the interface already had (or
        // rounded to it): the bus holds the client's copy until told again.
        command_tx.send(MediaCommand::SetVolume(0.753)).unwrap();
        assert_eq!(
            service.drain_commands(),
            vec![MediaCommand::SetVolume(0.753)]
        );
        service.update(state.clone());
        assert!(matches!(update_rx.try_recv(), Ok(Update::State(_, true))));
        service.update(state);
        assert!(update_rx.try_recv().is_err());
    }

    /// Plasma steps its wheel from the last `PropertiesChanged` it heard, so
    /// the one zbus sends for a volume write must carry the new level, not
    /// the level from before the interface got around to it.
    #[test]
    fn a_volume_write_is_answered_with_the_new_level_on_a_private_bus() {
        use std::time::Duration;
        use zbus::zvariant::{OwnedValue, Value};
        const CHILD: &str = "SPOTIFAST_VOLUME_PRIVATE_BUS";
        const NAME: &str =
            "media_controls::tests::a_volume_write_is_answered_with_the_new_level_on_a_private_bus";
        if std::env::var_os(CHILD).is_none() {
            let root = std::env::temp_dir().join(format!(
                "spotifast-volume-bus-{:016x}",
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
                .args(["--exact", NAME, "--nocapture"])
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
        let mut service = MediaService::spawn(|| {});
        service.update(MediaState {
            volume: 0.65,
            ..MediaState::default()
        });
        let client = zbus::blocking::connection::Builder::session()
            .unwrap()
            .method_timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let properties = zbus::blocking::fdo::PropertiesProxy::builder(&client)
            .destination("org.mpris.MediaPlayer2.spotifast")
            .unwrap()
            .path("/org/mpris/MediaPlayer2")
            .unwrap()
            .build()
            .unwrap();
        let player = zbus::names::InterfaceName::try_from("org.mpris.MediaPlayer2.Player").unwrap();
        let volume = || -> Option<f64> {
            let value: OwnedValue = properties.get(player.clone(), "Volume").ok()?;
            f64::try_from(value).ok()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while volume() != Some(0.65) {
            assert!(
                Instant::now() < deadline,
                "MPRIS did not publish the volume"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut changes = properties.receive_properties_changed().unwrap();

        // The interface never drains the request here, as if it had not
        // drawn its next frame yet.
        properties
            .set(player.clone(), "Volume", Value::from(0.7))
            .unwrap();
        let change = changes.next().unwrap();
        let args = change.args().unwrap();
        let heard = args
            .changed_properties()
            .get("Volume")
            .map(|value| f64::try_from(value).unwrap());
        assert_eq!(heard, Some(0.7));
        assert_eq!(volume(), Some(0.7));
        assert_eq!(service.drain_commands(), vec![MediaCommand::SetVolume(0.7)]);
    }

    #[test]
    fn object_paths_round_trip() {
        let path = object_path_for("spotify:track:14XWXWv5FoCbFzLksawpEe").unwrap();
        assert_eq!(
            uri_from_object_path(path.as_str()).as_deref(),
            Some("spotify:track:14XWXWv5FoCbFzLksawpEe")
        );
    }
}
