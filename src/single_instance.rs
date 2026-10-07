//! Single-instance guard and remote-control channel.
//!
//! A second instance would duplicate the Spotify Connect device, MPRIS player,
//! and tray icon. A second launch hands its request to the running instance
//! and exits.
//!
//! The guard is fastframe-instance's slot: an exclusive lock on a file in a
//! private per-user directory, which the system releases when the process
//! ends, even after a crash. Requests travel over a socket in that directory
//! that only the user can open (on Linux and macOS), or on Windows a loopback
//! port that answers only requests carrying the random token the running
//! instance writes beside the lock.
//!
//! Clients send one `spotizgeg:<verb>` line and receive one reply. Commands
//! enter the same action queue as tray and media-key events. Read commands use
//! snapshots, so the listener thread never accesses app state. The
//! `spotizgeg` command-line subcommands are clients of this channel; MPRIS
//! remains for media keys and desktop players on Linux.
//!
//! Clients poll the current snapshot; the app does not push updates.
//! `play-uri`, `open-link`, and `transfer` validate their free-text arguments
//! here before anything reaches the app.
//!
//! A Spotify link the desktop hands to a second launch reaches the running
//! instance the same way, as `open-link`.

/// The name every request and reply starts with, so a copy of another app
/// never obeys Spotizgeg's requests.
const NAME: &str = "spotizgeg";

/// The reply to an accepted command.
const OK_REPLY: &str = "ok";
/// The reply to `nowplaying`, before the snapshot.
const NOW_REPLY: &str = "now ";
/// The reply to `devices`, before the snapshot.
const DEVICES_REPLY: &str = "devices ";

pub enum Outcome {
    /// This process is the only instance. Hold the guard until it exits.
    Only(Guard),
    /// Another instance is running and took the request to show its window
    /// or open a link.
    Surfaced,
}

/// What a control client asked the running instance to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlCommand {
    /// Bring the window forward, creating it if needed.
    Show,
    /// Re-read local palette files without showing the window or restarting audio.
    ReloadThemes,
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    /// Milliseconds; negative seeks backwards.
    SeekBy(i64),
    /// Percentage points; negative lowers the volume.
    VolumeBy(i8),
    /// Absolute percentage.
    SetVolume(u8),
    ToggleMute,
    ToggleShuffle,
    CycleRepeat,
    /// Set shuffle explicitly, avoiding missed toggle updates.
    SetShuffle(bool),
    /// Repeat set outright, for the same reason.
    SetRepeat(crate::player::RepeatMode),
    /// Absolute position, in milliseconds.
    SeekTo(u32),
    /// Save the playing track to the library, or take it back out.
    ToggleSaved,
    /// Play a `spotify:` URI: a track, album, playlist, artist, or show.
    PlayUri(String),
    /// Open the page for a Spotify link the desktop or another launch
    /// handed over, and bring the window forward.
    OpenLink(String),
    /// Move playback to a Spotify Connect device, by id.
    Transfer(String),
    /// Refresh the device list. Sent by the `devices` read, which answers
    /// from a snapshot that is only as fresh as the app's last look.
    RefreshDevices,
}

/// Marks this process as the running instance until dropped.
pub struct Guard {
    /// The slot's lock; `None` when running unguarded.
    _slot: Option<fastframe_instance::Guard>,
    /// Filled by control clients, drained by the app every frame.
    commands: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>>,
    /// Current-track snapshot for `nowplaying` requests.
    now_playing: std::sync::Arc<std::sync::Mutex<String>>,
    /// Last Spotify Connect device snapshot, as one line of JSON.
    devices: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Guard {
    fn unguarded() -> Self {
        Self {
            _slot: None,
            commands: Default::default(),
            now_playing: std::sync::Arc::new(std::sync::Mutex::new(NOTHING_PLAYING.to_owned())),
            devices: std::sync::Arc::new(std::sync::Mutex::new(NO_DEVICES.to_owned())),
        }
    }

    /// The queue a control client's commands land in. The app drains it.
    pub fn commands(&self) -> std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>> {
        std::sync::Arc::clone(&self.commands)
    }

    /// The slot the app writes the now-playing snapshot into.
    pub fn now_playing_slot(&self) -> std::sync::Arc<std::sync::Mutex<String>> {
        std::sync::Arc::clone(&self.now_playing)
    }

    /// The slot the app writes the device list into.
    pub fn devices_slot(&self) -> std::sync::Arc<std::sync::Mutex<String>> {
        std::sync::Arc::clone(&self.devices)
    }
}

/// Snapshot value reported when nothing is playing.
pub const NOTHING_PLAYING: &str = "stopped";

/// Device snapshot used before loading and when Spotify reports no devices.
pub const NO_DEVICES: &str = "[]";

/// Where the running instance's lock and channel live: the per-user
/// runtime directory on Linux (the app's own inside Flatpak), the user's
/// private temporary directory on macOS, where a socket path under
/// Application Support can outgrow the 104 bytes macOS allows with a long
/// user name, and beside Spotizgeg's state on Windows.
fn slot() -> fastframe_instance::Slot {
    #[cfg(not(windows))]
    {
        fastframe_instance::Slot::new(NAME)
    }
    #[cfg(windows)]
    {
        fastframe_instance::Slot::at(
            crate::paths::AppDirs::discover().state.join("instance"),
            NAME,
        )
    }
}

/// What the running instance said back.
pub enum Reply {
    /// The command was accepted.
    Ok,
    /// The `nowplaying` snapshot: [`NOTHING_PLAYING`], or tab-separated
    /// `state, title, artists, album, position_ms, duration_ms, volume,
    /// shuffle, repeat, art_url, saved, device`.
    NowPlaying(String),
    /// The `devices` snapshot: a JSON array of objects with `id`, `name`,
    /// `kind`, and `active`, or [`NO_DEVICES`]. JSON safely carries free-text
    /// device names.
    Devices(String),
}

/// Sends one verb to the running instance and reads its reply.
pub fn send(verb: &str) -> std::io::Result<Reply> {
    reply(&slot().send(verb)?)
}

/// Reads the running instance's reply, without the `spotizgeg:` prefix the
/// channel already checked.
fn reply(line: &str) -> std::io::Result<Reply> {
    if line == OK_REPLY {
        Ok(Reply::Ok)
    } else if let Some(snapshot) = line.strip_prefix(NOW_REPLY) {
        Ok(Reply::NowPlaying(snapshot.to_owned()))
    } else if let Some(snapshot) = line.strip_prefix(DEVICES_REPLY) {
        Ok(Reply::Devices(snapshot.to_owned()))
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the running Spotizgeg answered something unexpected",
        ))
    }
}

/// Claims the running-instance role, or hands `link` (a canonical
/// `spotify:` URI, see [`crate::link::parse`]) to the instance that has it
/// and asks that one to come forward.
pub fn acquire(waker: &crate::backend::Waker, link: Option<&str>) -> Outcome {
    claim(&slot(), waker, link)
}

fn claim(
    slot: &fastframe_instance::Slot,
    waker: &crate::backend::Waker,
    link: Option<&str>,
) -> Outcome {
    let mut guard = Guard::unguarded();
    let request = match link {
        Some(uri) => format!("open-link {uri}"),
        None => "show".to_owned(),
    };
    let handler = handler(
        std::sync::Arc::clone(&guard.commands),
        std::sync::Arc::clone(&guard.now_playing),
        std::sync::Arc::clone(&guard.devices),
        waker.clone(),
    );
    match slot.claim(&request, handler) {
        fastframe_instance::Claim::First(slot) => {
            guard._slot = Some(slot);
            Outcome::Only(guard)
        }
        fastframe_instance::Claim::Running(_) => Outcome::Surfaced,
        fastframe_instance::Claim::Declined => {
            log::warn!("Spotizgeg is already running and declined this launch's request");
            Outcome::Surfaced
        }
        fastframe_instance::Claim::Unanswered => {
            log::warn!(
                "Spotizgeg is already running but did not answer; not starting a second copy"
            );
            Outcome::Surfaced
        }
    }
}

/// Answers one control request on the channel's thread: queues commands for
/// the app and wakes it, and answers reads from the published snapshots.
/// `None` refuses a request that is not ours, so the client gets no reply.
fn handler(
    commands: std::sync::Arc<std::sync::Mutex<Vec<ControlCommand>>>,
    now_playing: std::sync::Arc<std::sync::Mutex<String>>,
    devices: std::sync::Arc<std::sync::Mutex<String>>,
    waker: crate::backend::Waker,
) -> impl FnMut(&str) -> Option<String> + Send + 'static {
    move |request| {
        let queue = |command| {
            commands
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(command);
            waker.wake();
        };
        match parse(request)? {
            Request::Command(command) => {
                queue(command);
                Some(OK_REPLY.to_owned())
            }
            Request::NowPlaying => {
                let snapshot = now_playing
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                Some(format!("{NOW_REPLY}{snapshot}"))
            }
            Request::Devices => {
                let snapshot = devices.lock().unwrap_or_else(|p| p.into_inner()).clone();
                // Return the current snapshot, then request a refresh for the
                // next read. The app otherwise refreshes only while its picker
                // is open.
                queue(ControlCommand::RefreshDevices);
                Some(format!("{DEVICES_REPLY}{snapshot}"))
            }
        }
    }
}

/// A parsed request line: a command for the app, or a read the listener
/// answers itself.
enum Request {
    Command(ControlCommand),
    NowPlaying,
    Devices,
}

/// Reads one request line, the channel having already checked and removed
/// its `spotizgeg:` prefix.
fn parse(line: &str) -> Option<Request> {
    let verb = line.trim_end();
    let (verb, argument) = match verb.split_once(' ') {
        Some((verb, argument)) => (verb, Some(argument.trim())),
        None => (verb, None),
    };
    let command = match (verb, argument) {
        ("show", None) => ControlCommand::Show,
        ("reload-themes", None) => ControlCommand::ReloadThemes,
        ("playpause", None) => ControlCommand::PlayPause,
        ("play", None) => ControlCommand::Play,
        ("pause", None) => ControlCommand::Pause,
        ("next", None) => ControlCommand::Next,
        ("previous", None) => ControlCommand::Previous,
        ("seek-by", Some(ms)) => ControlCommand::SeekBy(ms.parse().ok()?),
        ("seek-to", Some(ms)) => ControlCommand::SeekTo(ms.parse().ok()?),
        ("volume-by", Some(delta)) => ControlCommand::VolumeBy(delta.parse().ok()?),
        ("volume-set", Some(volume)) => ControlCommand::SetVolume(volume.parse().ok()?),
        ("mute", None) => ControlCommand::ToggleMute,
        ("shuffle", None) => ControlCommand::ToggleShuffle,
        ("shuffle-set", Some("on")) => ControlCommand::SetShuffle(true),
        ("shuffle-set", Some("off")) => ControlCommand::SetShuffle(false),
        ("repeat", None) => ControlCommand::CycleRepeat,
        // Match explicitly because `RepeatMode::from_api` maps unknown values
        // to `off`; control clients should reject them.
        ("repeat-set", Some("off")) => ControlCommand::SetRepeat(crate::player::RepeatMode::Off),
        ("repeat-set", Some("context")) => {
            ControlCommand::SetRepeat(crate::player::RepeatMode::Context)
        }
        ("repeat-set", Some("track")) => {
            ControlCommand::SetRepeat(crate::player::RepeatMode::Track)
        }
        ("save-toggle", None) => ControlCommand::ToggleSaved,
        ("play-uri", Some(uri)) => ControlCommand::PlayUri(spotify_uri(uri)?),
        ("open-link", Some(link)) => ControlCommand::OpenLink(crate::link::parse(link)?),
        ("transfer", Some(id)) => ControlCommand::Transfer(device_id(id)?),
        ("nowplaying", None) => return Some(Request::NowPlaying),
        ("devices", None) => return Some(Request::Devices),
        _ => return None,
    };
    Some(Request::Command(command))
}

/// Validates the scheme, length, and characters of a Spotify URI received over
/// the control channel.
fn spotify_uri(text: &str) -> Option<String> {
    let shaped = text.starts_with("spotify:")
        && text.len() <= 128
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.' | '%' | '+'));
    shaped.then(|| text.to_owned())
}

/// A Spotify Connect device id: the opaque hex-ish string the Web API hands
/// out. Checked for the same reason as [`spotify_uri`].
fn device_id(text: &str) -> Option<String> {
    let shaped = !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    shaped.then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::RepeatMode;

    fn command(line: &str) -> Option<ControlCommand> {
        match parse(line) {
            Some(Request::Command(command)) => Some(command),
            _ => None,
        }
    }

    #[test]
    fn parses_every_control_verb() {
        // #given / #when / #then
        assert_eq!(command("show\n"), Some(ControlCommand::Show));
        assert_eq!(command("reload-themes"), Some(ControlCommand::ReloadThemes));
        assert_eq!(command("reload-themes extra"), None);
        assert_eq!(command("playpause"), Some(ControlCommand::PlayPause));
        assert_eq!(command("play"), Some(ControlCommand::Play));
        assert_eq!(command("pause"), Some(ControlCommand::Pause));
        assert_eq!(command("next"), Some(ControlCommand::Next));
        assert_eq!(command("previous"), Some(ControlCommand::Previous));
        assert_eq!(
            command("seek-by -10000"),
            Some(ControlCommand::SeekBy(-10_000))
        );
        assert_eq!(command("volume-by +5"), Some(ControlCommand::VolumeBy(5)));
        assert_eq!(
            command("volume-set 40"),
            Some(ControlCommand::SetVolume(40))
        );
        assert_eq!(command("mute"), Some(ControlCommand::ToggleMute));
        assert_eq!(command("shuffle"), Some(ControlCommand::ToggleShuffle));
        assert_eq!(command("repeat"), Some(ControlCommand::CycleRepeat));
        assert_eq!(
            command("shuffle-set on"),
            Some(ControlCommand::SetShuffle(true))
        );
        assert_eq!(
            command("shuffle-set off"),
            Some(ControlCommand::SetShuffle(false))
        );
        assert_eq!(
            command("repeat-set track"),
            Some(ControlCommand::SetRepeat(RepeatMode::Track))
        );
        assert_eq!(
            command("repeat-set context"),
            Some(ControlCommand::SetRepeat(RepeatMode::Context))
        );
        assert_eq!(
            command("repeat-set off"),
            Some(ControlCommand::SetRepeat(RepeatMode::Off))
        );
        assert_eq!(
            command("seek-to 90000"),
            Some(ControlCommand::SeekTo(90_000))
        );
        assert_eq!(command("save-toggle"), Some(ControlCommand::ToggleSaved));
        assert_eq!(
            command("play-uri spotify:playlist:37i9dQZF1DXcBWIGoYBM5M"),
            Some(ControlCommand::PlayUri(
                "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M".to_owned()
            ))
        );
        assert_eq!(
            command("transfer a1b2c3d4e5"),
            Some(ControlCommand::Transfer("a1b2c3d4e5".to_owned()))
        );
        // A link arrives in whatever shape the desktop had it and leaves
        // as the one URI the app navigates by.
        assert_eq!(
            command("open-link https://open.spotify.com/album/1DFixLWuPkv3KT3TnV35m3?si=x"),
            Some(ControlCommand::OpenLink(
                "spotify:album:1DFixLWuPkv3KT3TnV35m3".to_owned()
            ))
        );
        assert!(matches!(parse("nowplaying"), Some(Request::NowPlaying)));
        assert!(matches!(parse("devices"), Some(Request::Devices)));
    }

    #[test]
    fn rejects_lines_that_are_not_ours() {
        assert!(parse("GET / HTTP/1.1").is_none());
        assert!(parse("frobnicate").is_none());
        assert!(parse("seek-by soon").is_none());
        assert!(parse("volume-set 999").is_none());
        assert!(parse("next please").is_none());
        assert!(parse("").is_none());
    }

    /// Free-text control arguments are validated before reaching the app.
    #[test]
    fn refuses_arguments_that_are_not_shaped_like_spotifys_own() {
        // #given / #when / #then
        assert!(command("play-uri http://example.com/pwn").is_none());
        assert!(command("play-uri spotify:track:a b").is_none());
        assert!(command("play-uri ../../etc/passwd").is_none());
        assert!(command("play-uri").is_none());
        assert!(command(&format!("play-uri spotify:{}", "x".repeat(200))).is_none());
        assert!(command("transfer ../secrets").is_none());
        assert!(command("transfer").is_none());
        assert!(command("open-link https://example.com/track/x").is_none());
        assert!(command("open-link spotify:user:someone").is_none());
        assert!(command("open-link").is_none());
        // A word that is not one of the three is refused rather than read
        // as `off`, which is what `RepeatMode::from_api` would have done.
        assert!(command("repeat-set sometimes").is_none());
        assert!(command("shuffle-set maybe").is_none());
        assert!(command("seek-to -1").is_none());
    }

    /// A slot of its own in a throwaway directory, so a Spotizgeg already
    /// running on this machine is left alone.
    fn test_slot(name: &str) -> (fastframe_instance::Slot, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("spotizgeg-instance-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (fastframe_instance::Slot::at(&dir, NAME), dir)
    }

    /// The first launch holds the slot; a second one hands its link over
    /// and does not start.
    #[test]
    fn a_second_launch_hands_its_link_to_the_first() {
        // #given
        let (slot, dir) = test_slot("second-launch");
        let waker = crate::backend::Waker::default();
        let Outcome::Only(first) = claim(&slot, &waker, None) else {
            panic!("the first launch runs");
        };

        // #when
        let second = claim(&slot, &waker, Some("spotify:album:1DFixLWuPkv3KT3TnV35m3"));

        // #then
        assert!(matches!(second, Outcome::Surfaced));
        assert_eq!(
            *first.commands.lock().expect("the queue"),
            vec![ControlCommand::OpenLink(
                "spotify:album:1DFixLWuPkv3KT3TnV35m3".to_owned()
            )]
        );
        drop(first);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Commands reach the queue and reads return the published snapshots.
    #[test]
    fn a_client_reaches_the_command_queue_and_the_snapshot() {
        // #given
        let (slot, dir) = test_slot("round-trip");
        let Outcome::Only(guard) = claim(&slot, &crate::backend::Waker::default(), None) else {
            panic!("the slot is free");
        };
        *guard.now_playing.lock().expect("the snapshot") = "playing\tGo\tThe Band".to_owned();
        *guard.devices.lock().expect("the snapshot") =
            r#"[{"id":"abc","name":"Kitchen","kind":"Speaker","active":true}]"#.to_owned();
        let send = |verb: &str| slot.send(verb).and_then(|line| reply(&line));

        // #when
        let accepted = send("next").expect("a reply");
        let volume = send("volume-by -5").expect("a reply");
        let liked = send("save-toggle").expect("a reply");
        let snapshot = send("nowplaying").expect("a reply");
        let listed = send("devices").expect("a reply");
        let search =
            crate::link::parse(&format!("spotify:search:{}", "東京の音楽 ".repeat(20))).unwrap();
        assert!(search.len() > 256, "exercise a long search link");
        let searched = send(&format!("open-link {search}")).expect("a search reply");
        let oversized = send(&format!(
            "open-link spotify:search:{}",
            "x".repeat(16 * 1024)
        ));
        let refused = send("frobnicate");

        // #then
        assert!(matches!(accepted, Reply::Ok));
        assert!(matches!(volume, Reply::Ok));
        assert!(matches!(liked, Reply::Ok));
        assert!(matches!(searched, Reply::Ok));
        assert!(
            oversized.is_err(),
            "requests beyond the size bound are refused"
        );
        match snapshot {
            Reply::NowPlaying(line) => assert_eq!(line, "playing\tGo\tThe Band"),
            _ => panic!("nowplaying answered with something else"),
        }
        match listed {
            Reply::Devices(json) => assert!(json.contains("Kitchen")),
            _ => panic!("devices answered with something else"),
        }
        // An unknown verb gets no reply at all, so the client sees a closed
        // connection rather than a command it never sent being obeyed.
        assert!(refused.is_err());
        // Reading the devices also asks the app to look again, so the next
        // read is fresh.
        assert_eq!(
            *guard.commands.lock().expect("the queue"),
            vec![
                ControlCommand::Next,
                ControlCommand::VolumeBy(-5),
                ControlCommand::ToggleSaved,
                ControlCommand::RefreshDevices,
                ControlCommand::OpenLink(search),
            ]
        );
        drop(guard);
        let _ = std::fs::remove_dir_all(dir);
    }
}
