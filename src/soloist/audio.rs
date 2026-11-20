//! Private OS audio routing for the managed Soloist process. No system mix is
//! recorded. Linux uses PipeWire; the default native-audio feature adds
//! process capture on macOS and Windows.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

/// The waits before each rebuild of a failed route. Soloist keeps running
/// meanwhile, so the first follows at once.
const REPAIR_WAITS: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_secs(2),
    Duration::from_secs(5),
];
/// A route that lasted this long has spent none of its rebuilds.
const REPAIR_STABLE: Duration = Duration::from_secs(60);

/// What to do about a failed route.
#[derive(Debug, PartialEq, Eq)]
pub enum Repair {
    Wait,
    Rebuild,
    GiveUp,
}

/// How many times a failed route may be rebuilt before the player itself is
/// restarted.
#[derive(Default)]
pub struct Repairs {
    used: usize,
    last: Option<Instant>,
}
impl Repairs {
    pub fn next(&mut self, now: Instant) -> Repair {
        let since = self.last.map(|last| now.duration_since(last));
        if since.is_some_and(|since| since >= REPAIR_STABLE) {
            self.used = 0;
        }
        let Some(wait) = REPAIR_WAITS.get(self.used) else {
            return Repair::GiveUp;
        };
        if since.is_some_and(|since| since < *wait) {
            return Repair::Wait;
        }
        self.used += 1;
        self.last = Some(now);
        Repair::Rebuild
    }
}

/// How long a level set here outranks what Soloist reports. Longer than its
/// round trip, short enough for a change made elsewhere to get through.
const LEAD_HOLD: Duration = Duration::from_millis(1500);
/// The route's level before Soloist has reported one.
const NO_LEVEL: u8 = u8::MAX;

/// A separate output fader. Spotify's source volume never writes this value.
/// Clones share one control across the UI, child restarts and the audio worker.
#[derive(Clone)]
pub struct OutputVolume(Arc<Levels>);
struct Levels {
    fader: AtomicU8,
    /// What Linux's route plays at: Soloist's volume, ahead of it when the
    /// user just moved a slider here. The realtime thread reads this.
    route: AtomicU8,
    /// The level set here that a report may not undo, and until when.
    lead: Mutex<Option<(u8, Instant)>>,
    /// The listening level to bring back when Soloist's player restarts.
    remembered: AtomicU8,
}
impl Default for OutputVolume {
    fn default() -> Self {
        Self(Arc::new(Levels {
            fader: AtomicU8::new(100),
            route: AtomicU8::new(NO_LEVEL),
            lead: Mutex::new(None),
            remembered: AtomicU8::new(NO_LEVEL),
        }))
    }
}
impl OutputVolume {
    pub fn percent(&self) -> u8 {
        self.0.fader.load(Ordering::Relaxed)
    }
    pub fn set(&self, percent: u8) {
        self.0.fader.store(percent.min(100), Ordering::Relaxed);
    }
    /// What Soloist's player should come back at, once it has played at all.
    pub fn remembered(&self) -> Option<u8> {
        let percent = self.0.remembered.load(Ordering::Relaxed);
        (percent != NO_LEVEL).then_some(percent)
    }
    /// Returns whether that changed, so the session can be saved.
    pub fn remember(&self, percent: u8) -> bool {
        let percent = percent.min(100);
        self.0.remembered.swap(percent, Ordering::Relaxed) != percent
    }
    /// The level the route plays at, once there is one.
    pub fn route_percent(&self) -> Option<u8> {
        let percent = self.0.route.load(Ordering::Relaxed);
        (percent != NO_LEVEL).then_some(percent)
    }
    /// Makes `percent` heard now, ahead of Soloist. It is still told, and a
    /// report from before it knew can't take the level back.
    pub fn lead(&self, percent: u8) {
        let percent = percent.min(100);
        self.0.route.store(percent, Ordering::Relaxed);
        let mut lead = self.0.lead.lock().unwrap_or_else(|p| p.into_inner());
        *lead = Some((percent, Instant::now() + LEAD_HOLD));
    }
    /// Takes Soloist's level, unless it is behind what was set here.
    #[cfg(target_os = "linux")]
    fn report(&self, percent: u8, now: Instant) {
        let percent = percent.min(100);
        let mut lead = self.0.lead.lock().unwrap_or_else(|p| p.into_inner());
        if lead.is_some_and(|(wanted, until)| wanted != percent && now < until) {
            return;
        }
        *lead = None;
        self.0.route.store(percent, Ordering::Relaxed);
    }
}

struct State {
    child: AtomicU32,
    routed: AtomicBool,
    stop: AtomicBool,
    failed: AtomicBool,
    /// Why the route failed, for the log and for nothing else.
    fault: Mutex<Option<String>>,
    playing: AtomicBool,
    volume: AtomicU8,
    /// How far our queue puts the sound behind Soloist's clock.
    delay_ms: AtomicU32,
    #[cfg(all(feature = "soloist-native-audio", target_os = "macos"))]
    output_volume: OutputVolume,
    #[cfg(target_os = "linux")]
    route_volume: OutputVolume,
}

#[cfg(any(
    target_os = "linux",
    all(
        feature = "soloist-native-audio",
        any(target_os = "macos", target_os = "windows")
    )
))]
impl State {
    fn new() -> Self {
        Self {
            child: AtomicU32::new(0),
            routed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            fault: Mutex::new(None),
            playing: AtomicBool::new(false),
            volume: AtomicU8::new(0),
            delay_ms: AtomicU32::new(0),
            #[cfg(all(feature = "soloist-native-audio", target_os = "macos"))]
            output_volume: OutputVolume::default(),
            #[cfg(target_os = "linux")]
            route_volume: OutputVolume::default(),
        }
    }

    /// Records why the route can't go on. Not for the audio callbacks.
    fn fail(&self, reason: impl Into<String>) {
        let reason = reason.into();
        log::warn!("Soloist audio route failed: {reason}");
        if let Ok(mut fault) = self.fault.lock() {
            *fault = Some(reason);
        }
        self.failed.store(true, Ordering::Release);
    }
}

#[cfg(all(
    feature = "soloist-native-audio",
    any(test, target_os = "macos", target_os = "windows")
))]
#[path = "native.rs"]
mod native;

pub struct Bridge {
    #[cfg(target_os = "linux")]
    pub name: String,
    state: Arc<State>,
    #[cfg(target_os = "linux")]
    config: crate::player::EngineConfig,
    #[cfg(target_os = "linux")]
    output: Option<String>,
    #[cfg(target_os = "linux")]
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
    }
}
impl Bridge {
    pub async fn open(
        config: crate::player::EngineConfig,
        output_volume: OutputVolume,
    ) -> Option<Self> {
        #[cfg(not(all(
            feature = "soloist-native-audio",
            any(target_os = "macos", target_os = "windows")
        )))]
        let _ = output_volume;
        #[cfg(all(
            feature = "soloist-native-audio",
            any(target_os = "macos", target_os = "windows")
        ))]
        {
            native::open(config, output_volume)
        }
        #[cfg(not(any(
            target_os = "linux",
            all(
                feature = "soloist-native-audio",
                any(target_os = "macos", target_os = "windows")
            )
        )))]
        {
            let _ = config;
            None
        }
        #[cfg(target_os = "linux")]
        {
            Self::open_on(config, None, output_volume).await
        }
    }

    #[cfg(target_os = "linux")]
    async fn open_on(
        config: crate::player::EngineConfig,
        output: Option<String>,
        route_volume: OutputVolume,
    ) -> Option<Self> {
        let bridge = Self {
            name: format!("spotifast.soloist.{:016x}", rand::random::<u64>()),
            state: Arc::new(State {
                route_volume,
                ..State::new()
            }),
            config,
            output,
            worker: Mutex::new(None),
        };
        if !bridge.start().await {
            return None;
        }
        let state = bridge.state.clone();
        let raising = std::thread::Builder::new()
            .name("soloist-priority".into())
            .spawn(move || {
                super::priority::watch(
                    || state.child.load(Ordering::Acquire),
                    || state.playing.load(Ordering::Acquire),
                    || state.stop.load(Ordering::Acquire),
                );
            });
        if let Err(error) = raising {
            log::debug!("could not start the thread that raises Soloist's: {error}");
        }
        Some(bridge)
    }

    #[cfg(target_os = "linux")]
    async fn start(&self) -> bool {
        let state = self.state.clone();
        let name = self.name.clone();
        let config = self.config.clone();
        let output = self.output.clone();
        let (ready, receive) = tokio::sync::oneshot::channel();
        let worker = std::thread::Builder::new()
            .name("soloist-audio".into())
            .spawn(move || {
                if let Err(error) =
                    pipewire::run(&name, config, state.clone(), ready, output.as_deref())
                {
                    state.fail(format!("could not set up PipeWire: {error:#}"));
                }
            });
        let Ok(worker) = worker else {
            self.state.fail("could not start the audio thread");
            return false;
        };
        if let Ok(mut slot) = self.worker.lock() {
            *slot = Some(worker);
        }
        match tokio::time::timeout(Duration::from_secs(5), receive).await {
            Ok(Ok(())) if !self.failed() => true,
            _ => {
                self.state.fail("PipeWire did not bring the route up");
                false
            }
        }
    }

    /// Builds the failed route again. The same name lets the running Soloist,
    /// which was told it once at launch, find its output again.
    #[cfg(target_os = "linux")]
    pub async fn rebuild(&self) -> bool {
        let worker = self.worker.lock().ok().and_then(|mut slot| slot.take());
        if let Some(worker) = worker {
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        }
        self.state.routed.store(false, Ordering::Release);
        self.state.failed.store(false, Ordering::Release);
        self.start().await
    }

    /// Other platforms' capture has nothing to build again.
    #[cfg(not(target_os = "linux"))]
    pub async fn rebuild(&self) -> bool {
        false
    }

    pub fn fault(&self) -> Option<String> {
        self.state.fault.lock().ok().and_then(|fault| fault.clone())
    }

    pub fn playing(&self) -> bool {
        self.state.playing.load(Ordering::Acquire)
    }
    /// How far behind Soloist's reported position the sound is.
    pub fn delay_ms(&self) -> u32 {
        self.state.delay_ms.load(Ordering::Relaxed)
    }
    pub fn child(&self, pid: u32) {
        self.state.child.store(pid, Ordering::Release);
    }
    pub fn routed(&self) -> bool {
        self.state.routed.load(Ordering::Acquire)
    }
    /// A confirmed Mac route can raise the source without exposing dry audio.
    /// Windows observes only; Linux already locks source gain in PipeWire.
    pub fn owns_output_volume(&self) -> bool {
        cfg!(all(target_os = "macos", feature = "soloist-native-audio"))
            && self.routed()
            && !self.failed()
    }
    pub fn failed(&self) -> bool {
        self.state.failed.load(Ordering::Acquire)
    }
    pub fn update(&self, message: &serde_json::Value) {
        if let Some(volume) = message["volume"].as_u64() {
            self.state
                .volume
                .store(volume.min(100) as u8, Ordering::Relaxed);
            #[cfg(target_os = "linux")]
            self.state
                .route_volume
                .report(volume.min(100) as u8, Instant::now());
        }
        if let Some(status) = message["status"].as_str() {
            match status {
                "playing" | "buffering" => self.state.playing.store(true, Ordering::Release),
                "idle" | "paused" => self.state.playing.store(false, Ordering::Release),
                _ => {}
            }
        }
        if message["is_active"] == false {
            self.state.playing.store(false, Ordering::Release);
        }
    }
}

/// The stream properties Soloist runs with, added to any the user set.
///
/// Only this child's stream stays at unity: its documented volume events
/// drive our final gain, after the equalizer and visualizers.
///
/// `node.dont-fallback` keeps it from ever reaching a speaker unprocessed,
/// but alone it makes WirePlumber destroy the stream the moment the private
/// sink goes (linking/find-defined-target.lua), and Soloist then has nothing
/// left to link to a rebuilt route. `node.linger` makes WirePlumber wait for
/// the sink's name to come back instead.
#[cfg(target_os = "linux")]
pub fn child_props(existing: &str) -> String {
    let existing = existing.trim();
    let existing = existing
        .strip_prefix('{')
        .and_then(|props| props.strip_suffix('}'))
        .unwrap_or(existing);
    format!(
        "{{ {existing} channelmix.lock-volumes = true node.dont-fallback = true node.dont-move = true node.linger = true }}"
    )
}

/// The same stream properties in libpulse's `PULSE_PROP` form, for a Soloist
/// build that plays through `pipewire-pulse` instead of native PipeWire.
#[cfg(target_os = "linux")]
pub fn pulse_props(existing: &str) -> String {
    let existing = existing.trim();
    const PROPS: &str =
        "channelmix.lock-volumes=true node.dont-fallback=true node.dont-move=true node.linger=true";
    if existing.is_empty() {
        PROPS.into()
    } else {
        format!("{existing} {PROPS}")
    }
}

/// True when libpulse's configured local UNIX server exposes `sink`, proving
/// that a libpulse Soloist child reaches the same PipeWire graph as `Bridge`.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PulseReachability {
    Found,
    Retry,
    Unavailable,
}

#[cfg(target_os = "linux")]
fn pulse_reaches_sink_with(
    mut probe: impl FnMut() -> PulseReachability,
    timeout: Duration,
    retry_interval: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match probe() {
            PulseReachability::Found => return true,
            PulseReachability::Unavailable => return false,
            PulseReachability::Retry => {}
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        std::thread::sleep(remaining.min(retry_interval));
    }
}

#[cfg(target_os = "linux")]
pub fn pulse_reaches_sink(sink: &str) -> bool {
    pulse_reaches_sink_with(
        || pulse::reaches_sink(sink),
        Duration::from_secs(5),
        Duration::from_millis(50),
    )
}

#[cfg(target_os = "linux")]
mod pulse {
    use super::PulseReachability;
    use std::{
        ffi::OsStr,
        io::{self, Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::{ffi::OsStrExt, net::UnixStream},
        },
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    const PROTOCOL_VERSION: u32 = 35;
    const COOKIE_LEN: usize = 256;
    const DESCRIPTOR_LEN: usize = 20;
    const CONTROL_CHANNEL: u32 = u32::MAX;
    const MAX_FRAME_LEN: usize = 4096;
    const MAX_INCLUDE_DEPTH: usize = 16;
    const CMD_ERROR: u32 = 0;
    const CMD_REPLY: u32 = 2;
    const CMD_AUTH: u32 = 8;
    const CMD_SET_CLIENT_NAME: u32 = 9;
    const CMD_LOOKUP_SINK: u32 = 10;
    const PA_ERR_NOENTITY: u32 = 5;
    const TAG_STRING: u8 = b't';
    const TAG_STRING_NULL: u8 = b'N';
    const TAG_U32: u8 = b'L';
    const TAG_ARBITRARY: u8 = b'x';
    const TAG_PROPLIST: u8 = b'P';
    const SOCKET_TIMEOUT: Duration = Duration::from_millis(200);

    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Endpoint {
        Unix(PathBuf),
        NonUnix,
    }

    #[derive(Debug, Default, PartialEq, Eq)]
    pub(super) struct ClientConf {
        pub(super) default_server: Option<String>,
        pub(super) cookie_file: Option<PathBuf>,
    }

    pub(super) fn parse_server_list(list: &str, local_machine_id: Option<&str>) -> Vec<Endpoint> {
        let mut out = Vec::new();
        for entry in list.split_ascii_whitespace() {
            let rest = if let Some(after_open) = entry.strip_prefix('{') {
                let Some(local_id) = local_machine_id.filter(|id| !id.is_empty()) else {
                    continue;
                };
                let Some((id, after_close)) = after_open.split_once('}') else {
                    continue;
                };
                if id != local_id {
                    continue;
                }
                after_close
            } else {
                entry
            };
            if rest.starts_with('/') {
                out.push(Endpoint::Unix(PathBuf::from(rest)));
            } else if let Some(path) = rest.strip_prefix("unix:") {
                if !path.is_empty() {
                    out.push(Endpoint::Unix(PathBuf::from(path)));
                }
            } else if !rest.is_empty() {
                out.push(Endpoint::NonUnix);
                break;
            }
        }
        out
    }

    fn open_conf_file(path: &Path) -> Result<(), std::io::ErrorKind> {
        let file = std::fs::File::open(path).map_err(|err| err.kind())?;
        let meta = file.metadata().map_err(|err| err.kind())?;
        if meta.is_dir() {
            return Err(std::io::ErrorKind::InvalidInput);
        }
        Ok(())
    }

    pub(super) fn select_client_conf_file(
        pulse_clientconfig: Option<&OsStr>,
        pulse_config_path: Option<&OsStr>,
        home: Option<&Path>,
        system_conf: &Path,
    ) -> Option<PathBuf> {
        if let Some(custom) = pulse_clientconfig {
            let path = PathBuf::from(custom);
            return open_conf_file(&path).ok().map(|()| path);
        }
        if let Some(config_dir) = pulse_config_path {
            let path = PathBuf::from(config_dir).join("client.conf");
            match open_conf_file(&path) {
                Ok(()) => return Some(path),
                Err(std::io::ErrorKind::NotFound) => {}
                Err(_) => return None,
            }
        } else {
            let home = home.filter(|dir| !dir.as_os_str().is_empty())?;
            let dot_pulse = home.join(".pulse/client.conf");
            if open_conf_file(&dot_pulse).is_ok() {
                return Some(dot_pulse);
            }
            let dot_config = home.join(".config/pulse/client.conf");
            match open_conf_file(&dot_config) {
                Ok(()) => return Some(dot_config),
                Err(std::io::ErrorKind::NotFound) => {}
                Err(_) => return None,
            }
        }
        open_conf_file(system_conf)
            .ok()
            .map(|()| system_conf.to_path_buf())
    }

    pub(super) fn load_client_conf(path: &Path) -> ClientConf {
        let mut conf = ClientConf::default();
        let _ = parse_conf_file(path, &mut conf, true, 0);
        conf
    }

    fn parse_conf_file(
        path: &Path,
        conf: &mut ClientConf,
        use_dot_d: bool,
        depth: usize,
    ) -> Result<(), ()> {
        if depth > MAX_INCLUDE_DEPTH {
            return Err(());
        }
        let result = match std::fs::read_to_string(path) {
            Ok(contents) => parse_conf_contents(path, &contents, conf, depth),
            Err(err) if !use_dot_d && err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(()),
        };
        if use_dot_d {
            let mut dot_d = path.as_os_str().to_os_string();
            dot_d.push(".d");
            if let Ok(entries) = std::fs::read_dir(PathBuf::from(dot_d)) {
                let mut drop_ins: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|entry_path| {
                        entry_path
                            .file_name()
                            .is_some_and(|name| name.as_encoded_bytes().ends_with(b".conf"))
                    })
                    .collect();
                drop_ins.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
                for drop_in in drop_ins {
                    let _ = parse_conf_file(&drop_in, conf, false, depth + 1);
                }
            }
        }
        result
    }

    fn parse_conf_contents(
        filename: &Path,
        contents: &str,
        conf: &mut ClientConf,
        depth: usize,
    ) -> Result<(), ()> {
        const TRIM: [char; 3] = [' ', '\t', '\r'];
        for raw_line in contents.lines() {
            let line = raw_line.trim_start_matches(TRIM);
            let line = line.split(['#', ';']).next().unwrap_or("");
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix(".include ") {
                let target = rest.trim_matches(TRIM);
                let include_path = if Path::new(target).is_absolute() {
                    PathBuf::from(target)
                } else if let Some(parent) =
                    filename.parent().filter(|dir| !dir.as_os_str().is_empty())
                {
                    parent.join(target)
                } else {
                    PathBuf::from(target)
                };
                parse_conf_file(&include_path, conf, false, depth + 1)?;
                continue;
            }
            if line.starts_with('[') {
                let section = line.trim_end_matches(TRIM);
                if !section.ends_with(']') || section == "[Properties]" {
                    return Err(());
                }
                continue;
            }
            let Some((raw_key, raw_value)) = line.split_once('=') else {
                return Err(());
            };
            let key = raw_key.trim_matches(TRIM);
            let value = raw_value.trim_matches(TRIM);
            match key {
                "default-server" => {
                    conf.default_server = (!value.is_empty()).then(|| value.to_owned());
                }
                "cookie-file" => {
                    conf.cookie_file = (!value.is_empty()).then(|| PathBuf::from(value));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn local_machine_id() -> Option<String> {
        [
            "/etc/machine-id",
            "/var/lib/dbus/machine-id",
            "/proc/sys/kernel/hostname",
        ]
        .into_iter()
        .find_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            let first = text.lines().next().unwrap_or("").trim();
            (!first.is_empty()).then(|| first.to_owned())
        })
    }

    fn cookie_config_dir(home: Option<&Path>) -> Option<PathBuf> {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(xdg).join("pulse"));
        }
        home.map(|dir| dir.join(".config/pulse"))
    }

    fn configured_endpoints_and_cookie() -> (Vec<Endpoint>, [u8; COOKIE_LEN]) {
        let runtime_dir = std::env::var_os("PULSE_RUNTIME_PATH")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_RUNTIME_DIR")
                    .filter(|v| !v.is_empty())
                    .map(|dir| PathBuf::from(dir).join("pulse"))
            });
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        let conf = select_client_conf_file(
            std::env::var_os("PULSE_CLIENTCONFIG").as_deref(),
            std::env::var_os("PULSE_CONFIG_PATH").as_deref(),
            home.as_deref(),
            Path::new("/etc/pulse/client.conf"),
        )
        .as_deref()
        .map(load_client_conf)
        .unwrap_or_default();

        let local_id = local_machine_id();
        let endpoints = if let Some(env_server) = std::env::var_os("PULSE_SERVER") {
            match env_server.to_str() {
                Some(server) if !server.trim().is_empty() => {
                    parse_server_list(server, local_id.as_deref())
                }
                _ => Vec::new(),
            }
        } else if let Some(server) = conf.default_server.as_deref() {
            parse_server_list(server, local_id.as_deref())
        } else {
            let mut defaults = Vec::new();
            if let Some(dir) = runtime_dir {
                defaults.push(Endpoint::Unix(dir.join("native")));
            }
            defaults.push(Endpoint::Unix(PathBuf::from("/var/run/pulse/native")));
            defaults
        };

        let cookie_dir = cookie_config_dir(home.as_deref());
        let resolve_cookie = |path: PathBuf| -> Option<PathBuf> {
            if path.is_absolute() {
                Some(path)
            } else {
                cookie_dir.as_ref().map(|dir| dir.join(path))
            }
        };
        let mut cookie = [0u8; COOKIE_LEN];
        let cookie_candidates = [
            std::env::var_os("PULSE_COOKIE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .and_then(&resolve_cookie),
            conf.cookie_file.and_then(&resolve_cookie),
            cookie_dir.map(|dir| dir.join("cookie")),
            home.map(|dir| dir.join(".pulse-cookie")),
        ];
        for candidate in cookie_candidates.into_iter().flatten() {
            if let Ok(bytes) = std::fs::read(candidate)
                && bytes.len() == COOKIE_LEN
            {
                cookie.copy_from_slice(&bytes);
                break;
            }
        }

        (endpoints, cookie)
    }

    fn push_u32_tag(buf: &mut Vec<u8>, value: u32) {
        buf.push(TAG_U32);
        buf.extend_from_slice(&value.to_be_bytes());
    }

    fn send_packet(stream: &mut UnixStream, payload: &[u8]) -> std::io::Result<()> {
        let len = u32::try_from(payload.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "packet too large")
        })?;
        let mut header = [0u8; DESCRIPTOR_LEN];
        header[0..4].copy_from_slice(&len.to_be_bytes());
        header[4..8].copy_from_slice(&CONTROL_CHANNEL.to_be_bytes());
        stream.write_all(&header)?;
        stream.write_all(payload)
    }

    fn recv_packet(stream: &mut UnixStream) -> Option<(u32, u32, Vec<u8>)> {
        let mut header = [0u8; DESCRIPTOR_LEN];
        stream.read_exact(&mut header).ok()?;
        let len = u32::from_be_bytes(header[0..4].try_into().ok()?) as usize;
        let channel = u32::from_be_bytes(header[4..8].try_into().ok()?);
        if channel != CONTROL_CHANNEL || len > MAX_FRAME_LEN {
            return None;
        }
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).ok()?;
        let cmd = read_u32_tag(body.get(0..5)?)?;
        let seq = read_u32_tag(body.get(5..10)?)?;
        Some((cmd, seq, body.split_off(10)))
    }

    fn read_u32_tag(bytes: &[u8]) -> Option<u32> {
        if *bytes.first()? != TAG_U32 {
            return None;
        }
        Some(u32::from_be_bytes(bytes.get(1..5)?.try_into().ok()?))
    }

    fn connect_unix_with_timeout(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
        let path_bytes = path.as_os_str().as_bytes();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        if path_bytes.contains(&0) || path_bytes.len() >= address.sun_path.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Pulse socket path is invalid or too long",
            ));
        }
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (destination, source) in address.sun_path.iter_mut().zip(path_bytes) {
            *destination = *source as libc::c_char;
        }
        let address_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1)
            as libc::socklen_t;

        let raw_fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if raw_fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let deadline = Instant::now() + timeout;
        loop {
            let connected = unsafe {
                libc::connect(
                    fd.as_raw_fd(),
                    std::ptr::from_ref(&address).cast(),
                    address_len,
                )
            };
            if connected == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EISCONN) => break,
                Some(libc::EINTR) => continue,
                Some(libc::EAGAIN) | Some(libc::EINPROGRESS) | Some(libc::EALREADY) => {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "Pulse socket connection timed out",
                        ));
                    };
                    std::thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                _ => return Err(error),
            }
        }

        let stream: UnixStream = fd.into();
        stream.set_nonblocking(false)?;
        Ok(stream)
    }

    pub(super) fn socket_has_sink(
        path: &Path,
        sink: &str,
        cookie: &[u8; COOKIE_LEN],
    ) -> io::Result<bool> {
        let mut stream = connect_unix_with_timeout(path, SOCKET_TIMEOUT)?;
        let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
        let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
        Ok(query_connected_sink(&mut stream, sink, cookie).unwrap_or(false))
    }

    fn query_connected_sink(
        stream: &mut UnixStream,
        sink: &str,
        cookie: &[u8; COOKIE_LEN],
    ) -> Option<bool> {
        let mut auth = Vec::with_capacity(20 + COOKIE_LEN);
        push_u32_tag(&mut auth, CMD_AUTH);
        push_u32_tag(&mut auth, 0);
        push_u32_tag(&mut auth, PROTOCOL_VERSION);
        auth.push(TAG_ARBITRARY);
        auth.extend_from_slice(&(COOKIE_LEN as u32).to_be_bytes());
        auth.extend_from_slice(cookie);
        send_packet(stream, &auth).ok()?;
        let (cmd, seq, _) = recv_packet(stream)?;
        if cmd != CMD_REPLY || seq != 0 {
            return None;
        }

        let mut set_name = Vec::with_capacity(12);
        push_u32_tag(&mut set_name, CMD_SET_CLIENT_NAME);
        push_u32_tag(&mut set_name, 1);
        set_name.push(TAG_PROPLIST);
        set_name.push(TAG_STRING_NULL);
        send_packet(stream, &set_name).ok()?;
        let (cmd, seq, _) = recv_packet(stream)?;
        if cmd != CMD_REPLY || seq != 1 {
            return None;
        }

        for attempt in 0..4u32 {
            let req_seq = 2 + attempt;
            let mut lookup = Vec::with_capacity(12 + sink.len());
            push_u32_tag(&mut lookup, CMD_LOOKUP_SINK);
            push_u32_tag(&mut lookup, req_seq);
            lookup.push(TAG_STRING);
            lookup.extend_from_slice(sink.as_bytes());
            lookup.push(0);
            send_packet(stream, &lookup).ok()?;
            let (cmd, seq, rest) = recv_packet(stream)?;
            if seq != req_seq {
                return None;
            }
            match cmd {
                CMD_REPLY => {
                    let index = read_u32_tag(rest.get(0..5)?)?;
                    return Some(index != u32::MAX);
                }
                CMD_ERROR => {
                    let code = read_u32_tag(rest.get(0..5)?)?;
                    if code == PA_ERR_NOENTITY && attempt < 3 {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    return Some(false);
                }
                _ => return Some(false),
            }
        }
        Some(false)
    }

    pub(super) fn reaches_sink_on(
        endpoints: Vec<Endpoint>,
        cookie: &[u8; COOKIE_LEN],
        sink: &str,
    ) -> PulseReachability {
        for endpoint in endpoints {
            match endpoint {
                Endpoint::NonUnix => return PulseReachability::Unavailable,
                Endpoint::Unix(path) => match socket_has_sink(&path, sink, cookie) {
                    Ok(true) => return PulseReachability::Found,
                    Ok(false) => return PulseReachability::Retry,
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                        return PulseReachability::Unavailable;
                    }
                    Err(_) => continue,
                },
            }
        }
        PulseReachability::Retry
    }

    pub(super) fn reaches_sink(sink: &str) -> PulseReachability {
        if sink.is_empty() || sink.as_bytes().contains(&0) {
            return PulseReachability::Unavailable;
        }
        let (endpoints, cookie) = configured_endpoints_and_cookie();
        reaches_sink_on(endpoints, &cookie, sink)
    }
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        all(
            feature = "soloist-native-audio",
            any(target_os = "macos", target_os = "windows")
        )
    )
))]
mod tests {
    use super::*;

    /// A slider is heard at once, and Soloist's report of an older level, on
    /// its way back from before it was told, doesn't move it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_level_set_here_outlasts_stale_reports_until_soloist_agrees() {
        let volume = OutputVolume::default();
        let now = Instant::now();
        assert_eq!(volume.route_percent(), None, "nothing to play at yet");
        volume.report(40, now);
        assert_eq!(volume.route_percent(), Some(40));
        volume.lead(70);
        assert_eq!(volume.route_percent(), Some(70), "with no round trip");
        volume.report(40, now);
        volume.report(55, now);
        assert_eq!(volume.route_percent(), Some(70));
        volume.report(70, now);
        volume.report(30, now);
        assert_eq!(
            volume.route_percent(),
            Some(30),
            "agreed: anyone may move it"
        );
    }

    /// If Soloist never agrees, it stays the authority on its own volume.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_level_set_here_gives_way_when_soloist_never_agrees() {
        let volume = OutputVolume::default();
        volume.lead(70);
        let later = Instant::now() + LEAD_HOLD + Duration::from_millis(1);
        volume.report(40, later);
        assert_eq!(volume.route_percent(), Some(40));
    }

    /// The route is rebuilt under Soloist's running stream, which must wait
    /// for it rather than be destroyed or fall back to a speaker.
    #[cfg(target_os = "linux")]
    #[test]
    fn soloist_s_stream_waits_for_a_rebuilt_route_and_keeps_the_user_s_props() {
        let props = child_props(" { node.latency = 1024/48000 } ");
        assert!(props.contains("node.latency = 1024/48000"), "{props}");
        assert!(props.starts_with('{') && props.ends_with('}'), "{props}");
        for wanted in [
            "node.linger = true",
            "node.dont-fallback = true",
            "node.dont-move = true",
            "channelmix.lock-volumes = true",
        ] {
            assert!(props.contains(wanted), "{props}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_props_locks_volume_and_keeps_the_user_s_props() {
        let empty = pulse_props("   ");
        assert!(!empty.starts_with(' '), "{empty}");
        let props = pulse_props(" media.role=music ");
        assert!(props.starts_with("media.role=music "), "{props}");
        for wanted in [
            "channelmix.lock-volumes=true",
            "node.dont-fallback=true",
            "node.dont-move=true",
            "node.linger=true",
        ] {
            assert!(empty.contains(wanted), "{empty}");
            assert!(props.contains(wanted), "{props}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_server_list_distinguishes_local_sockets_from_remote_servers() {
        use pulse::{Endpoint, parse_server_list};
        assert_eq!(
            parse_server_list("unix:/tmp/pulse/native", None),
            vec![Endpoint::Unix("/tmp/pulse/native".into())]
        );
        assert_eq!(
            parse_server_list("unix:native", None),
            vec![Endpoint::Unix("native".into())]
        );
        assert_eq!(
            parse_server_list(
                "{other-host}unix:/wrong/socket {my-host}/tmp/right/socket",
                Some("my-host"),
            ),
            vec![Endpoint::Unix("/tmp/right/socket".into())]
        );
        assert_eq!(
            parse_server_list("tcp:192.168.1.20:4713", Some("my-host")),
            vec![Endpoint::NonUnix]
        );
        assert_eq!(
            parse_server_list("remote-pulse-host", Some("my-host")),
            vec![Endpoint::NonUnix]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_client_conf_selection_does_not_merge_user_and_system_files() {
        use pulse::{load_client_conf, select_client_conf_file};
        let dir = std::env::temp_dir().join(format!(
            "spotifast-pulse-select-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = dir.join("home");
        let user_conf = home.join(".config/pulse/client.conf");
        let system_conf = dir.join("etc/pulse/client.conf");
        std::fs::create_dir_all(user_conf.parent().unwrap()).unwrap();
        std::fs::create_dir_all(system_conf.parent().unwrap()).unwrap();
        std::fs::write(&system_conf, "default-server = /tmp/system/native\n").unwrap();

        assert_eq!(
            select_client_conf_file(None, None, Some(&home), &system_conf),
            Some(system_conf.clone())
        );

        std::fs::write(&user_conf, "cookie-file = /custom/cookie\n").unwrap();
        let chosen = select_client_conf_file(None, None, Some(&home), &system_conf).unwrap();
        assert_eq!(chosen, user_conf);
        let parsed = load_client_conf(&chosen);
        assert_eq!(parsed.default_server, None);
        assert_eq!(parsed.cookie_file, Some("/custom/cookie".into()));

        let dot_pulse = home.join(".pulse/client.conf");
        std::fs::create_dir_all(dot_pulse.parent().unwrap()).unwrap();
        std::fs::write(&dot_pulse, "default-server = /tmp/dot-pulse/native\n").unwrap();
        assert_eq!(
            select_client_conf_file(None, None, Some(&home), &system_conf),
            Some(dot_pulse)
        );

        assert_eq!(
            select_client_conf_file(
                Some(dir.join("missing.conf").as_os_str()),
                None,
                Some(&home),
                &system_conf,
            ),
            None
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_client_conf_applies_includes_and_ordered_drop_ins() {
        use pulse::load_client_conf;
        let dir = std::env::temp_dir().join(format!(
            "spotifast-pulse-conf-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let conf_dir = dir.join("pulse");
        let drop_in_dir = conf_dir.join("client.conf.d");
        std::fs::create_dir_all(&drop_in_dir).unwrap();

        let main_conf = conf_dir.join("client.conf");
        std::fs::write(
            conf_dir.join("included.conf"),
            "default-server = unix:/included/native ; inline comment\ncookie-file = custom-cookie\n",
        )
        .unwrap();
        std::fs::write(
            &main_conf,
            "# header comment\ndefault-server = unix:/main/native\n.include missing.conf\n.include included.conf\n",
        )
        .unwrap();
        std::fs::write(
            drop_in_dir.join("10-first.conf"),
            "default-server = unix:/dropin-10/native # comment\n",
        )
        .unwrap();
        std::fs::write(
            drop_in_dir.join("20-second.conf"),
            "default-server = unix:/dropin-20/native\n",
        )
        .unwrap();
        std::fs::write(
            drop_in_dir.join("99-ignored.txt"),
            "default-server = unix:/ignored/native\n",
        )
        .unwrap();

        let parsed = load_client_conf(&main_conf);
        assert_eq!(
            parsed.default_server.as_deref(),
            Some("unix:/dropin-20/native")
        );
        assert_eq!(parsed.cookie_file, Some("custom-cookie".into()));

        std::fs::write(drop_in_dir.join("30-clear.conf"), "default-server =\n").unwrap();
        assert_eq!(load_client_conf(&main_conf).default_server, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_socket_lookup_matches_only_the_bridge_s_sink() {
        use std::{
            io::{Read, Write},
            os::unix::net::UnixListener,
        };
        let dir = std::env::temp_dir().join(format!(
            "spotifast-pulse-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("native");
        let listener = UnixListener::bind(&sock_path).unwrap();

        let reply_frame = |cmd: u32, seq: u32, val: u32| -> Vec<u8> {
            let mut body = Vec::with_capacity(15);
            for n in [cmd, seq, val] {
                body.push(b'L');
                body.extend_from_slice(&n.to_be_bytes());
            }
            let mut frame = Vec::with_capacity(20 + body.len());
            frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
            frame.extend_from_slice(&u32::MAX.to_be_bytes());
            frame.extend_from_slice(&0u64.to_be_bytes());
            frame.extend_from_slice(&0u32.to_be_bytes());
            frame.extend_from_slice(&body);
            frame
        };

        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut conn, _) = listener.accept().unwrap();
                let read_pkt = |c: &mut std::os::unix::net::UnixStream| -> Vec<u8> {
                    let mut hdr = [0u8; 20];
                    c.read_exact(&mut hdr).unwrap();
                    let len = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as usize;
                    let mut buf = vec![0u8; len];
                    c.read_exact(&mut buf).unwrap();
                    buf
                };
                // AUTH
                let _ = read_pkt(&mut conn);
                conn.write_all(&reply_frame(2, 0, 35)).unwrap();
                // SET_CLIENT_NAME
                let _ = read_pkt(&mut conn);
                conn.write_all(&reply_frame(2, 1, 1)).unwrap();
                // LOOKUP_SINK (up to 4 retries when missing)
                loop {
                    let mut hdr = [0u8; 20];
                    if conn.read_exact(&mut hdr).is_err() {
                        break;
                    }
                    let len = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as usize;
                    let mut pkt = vec![0u8; len];
                    conn.read_exact(&mut pkt).unwrap();
                    let seq = u32::from_be_bytes(pkt[6..10].try_into().unwrap());
                    let name_bytes = &pkt[11..pkt.len() - 1];
                    if name_bytes == b"spotifast.soloist.owned" {
                        conn.write_all(&reply_frame(2, seq, 42)).unwrap();
                        break;
                    } else {
                        conn.write_all(&reply_frame(0, seq, 5)).unwrap();
                    }
                }
            }
        });

        let cookie = [0u8; 256];
        assert!(pulse::socket_has_sink(&sock_path, "spotifast.soloist.owned", &cookie).unwrap());
        assert!(!pulse::socket_has_sink(&sock_path, "spotifast.soloist.foreign", &cookie).unwrap());
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    fn pulse_sink_server(
        listener: std::os::unix::net::UnixListener,
    ) -> std::thread::JoinHandle<bool> {
        use std::io::{Read, Write};
        listener.set_nonblocking(true).unwrap();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut conn = loop {
                match listener.accept() {
                    Ok((conn, _)) => break Some(conn),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            break None;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fallback accept failed: {error}"),
                }
            };
            let Some(ref mut conn) = conn else {
                return false;
            };
            conn.set_nonblocking(false).unwrap();
            let read_packet = |stream: &mut std::os::unix::net::UnixStream| {
                let mut header = [0u8; 20];
                stream.read_exact(&mut header).unwrap();
                let len = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
                let mut body = vec![0u8; len];
                stream.read_exact(&mut body).unwrap();
                body
            };
            let reply = |cmd: u32, seq: u32, value: u32| {
                let mut body = Vec::with_capacity(15);
                for n in [cmd, seq, value] {
                    body.push(b'L');
                    body.extend_from_slice(&n.to_be_bytes());
                }
                let mut frame = Vec::with_capacity(20 + body.len());
                frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
                frame.extend_from_slice(&u32::MAX.to_be_bytes());
                frame.extend_from_slice(&0u64.to_be_bytes());
                frame.extend_from_slice(&0u32.to_be_bytes());
                frame.extend_from_slice(&body);
                frame
            };
            let _ = read_packet(conn);
            conn.write_all(&reply(2, 0, 35)).unwrap();
            let _ = read_packet(conn);
            conn.write_all(&reply(2, 1, 1)).unwrap();
            let lookup = read_packet(conn);
            let seq = u32::from_be_bytes(lookup[6..10].try_into().unwrap());
            conn.write_all(&reply(2, seq, 42)).unwrap();
            true
        })
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_review_regression_retries_readiness_until_sink_appears() {
        let attempts = std::cell::Cell::new(0);
        assert!(pulse_reaches_sink_with(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() >= 3 {
                    PulseReachability::Found
                } else {
                    PulseReachability::Retry
                }
            },
            Duration::from_secs(1),
            Duration::ZERO,
        ));
        assert_eq!(attempts.get(), 3);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_review_regression_nonlocal_selection_returns_without_retry() {
        let previous = std::env::var_os("PULSE_SERVER");
        // SAFETY: this test runs with --test-threads=1 while it temporarily
        // exercises libpulse's process-wide server selection.
        unsafe { std::env::set_var("PULSE_SERVER", "tcp:192.0.2.1:4713") };
        let started = Instant::now();
        let reaches = pulse_reaches_sink("spotifast.soloist.owned");
        let elapsed = started.elapsed();
        // SAFETY: restore the process environment after the single-threaded call.
        unsafe {
            match previous {
                Some(value) => std::env::set_var("PULSE_SERVER", value),
                None => std::env::remove_var("PULSE_SERVER"),
            }
        }
        assert!(!reaches);
        assert!(
            elapsed < Duration::from_secs(1),
            "a definitively nonlocal selection must not retry for five seconds: {elapsed:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_review_regression_stops_after_selected_endpoint_times_out() {
        use std::os::{
            fd::AsRawFd,
            unix::net::{UnixListener, UnixStream},
        };
        let dir = std::env::temp_dir().join(format!(
            "spotifast-pulse-endpoints-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let stalled_path = dir.join("stalled");
        let stalled = UnixListener::bind(&stalled_path).unwrap();
        assert_eq!(unsafe { libc::listen(stalled.as_raw_fd(), 0) }, 0);
        let queued = UnixStream::connect(&stalled_path).unwrap();

        let fallback_path = dir.join("fallback");
        let server = pulse_sink_server(UnixListener::bind(&fallback_path).unwrap());
        let found = pulse::reaches_sink_on(
            vec![
                pulse::Endpoint::Unix(stalled_path),
                pulse::Endpoint::Unix(fallback_path),
            ],
            &[0; 256],
            "spotifast.soloist.owned",
        );
        assert!(
            !server.join().unwrap(),
            "a timed-out selected server must not probe a different Pulse server"
        );
        assert_eq!(found, PulseReachability::Unavailable);
        drop(queued);
        drop(stalled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_review_regression_missing_endpoint_uses_the_next_server() {
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!(
            "spotifast-pulse-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fallback_path = dir.join("fallback");
        let server = pulse_sink_server(UnixListener::bind(&fallback_path).unwrap());
        assert_eq!(
            pulse::reaches_sink_on(
                vec![
                    pulse::Endpoint::Unix(dir.join("missing")),
                    pulse::Endpoint::Unix(fallback_path),
                ],
                &[0; 256],
                "spotifast.soloist.owned",
            ),
            PulseReachability::Found
        );
        assert!(
            server.join().unwrap(),
            "a missing selected server must try the next Pulse server"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pulse_socket_lookup_does_not_block_on_a_full_accept_queue() {
        use std::{
            os::{
                fd::AsRawFd,
                unix::net::{UnixListener, UnixStream},
            },
            time::{Duration, Instant},
        };
        let dir = std::env::temp_dir().join(format!(
            "spotifast-pulse-full-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("native");
        let listener = UnixListener::bind(&sock_path).unwrap();
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let queued = UnixStream::connect(&sock_path).unwrap();

        let started = Instant::now();
        let result = pulse::socket_has_sink(&sock_path, "spotifast.soloist.owned", &[0; 256]);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "full Pulse accept queue blocked for {:?}",
            started.elapsed()
        );
        assert!(result.is_err() || !result.unwrap());

        drop(queued);
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One bad moment rebuilds at once, a few more wait, and a route that
    /// keeps failing stops using up the audio thread.
    #[test]
    fn a_failing_route_is_rebuilt_a_few_times_then_given_up() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let mut repairs = Repairs::default();
        assert_eq!(repairs.next(at(0)), Repair::Rebuild);
        assert_eq!(repairs.next(at(1)), Repair::Wait);
        assert_eq!(repairs.next(at(2)), Repair::Rebuild);
        assert_eq!(repairs.next(at(3)), Repair::Wait);
        assert_eq!(repairs.next(at(7)), Repair::Rebuild);
        assert_eq!(repairs.next(at(8)), Repair::GiveUp);
    }

    /// Weeks of playback must not be one fault from a restarted player.
    #[test]
    fn a_route_that_lasted_has_all_its_rebuilds_again() {
        let start = Instant::now();
        let mut repairs = Repairs::default();
        for seconds in [0, 2, 7] {
            assert_eq!(
                repairs.next(start + Duration::from_secs(seconds)),
                Repair::Rebuild
            );
        }
        assert_eq!(
            repairs.next(start + Duration::from_secs(67)),
            Repair::Rebuild
        );
    }

    #[test]
    fn a_failed_route_says_why() {
        let state = State::new();
        state.fail("the output stream reported an error");
        assert!(state.failed.load(Ordering::Acquire));
        assert_eq!(
            state.fault.lock().unwrap().as_deref(),
            Some("the output stream reported an error")
        );
    }

    #[test]
    fn only_a_confirmed_healthy_mac_output_allows_full_source_volume() {
        #[cfg(target_os = "linux")]
        let config = {
            let dirs = crate::paths::AppDirs {
                config: std::path::PathBuf::new(),
                state: std::path::PathBuf::new(),
                cache: std::path::PathBuf::new(),
            };
            crate::app::engine_config(
                &dirs,
                &crate::settings::Settings::default(),
                crate::settings::ProxyConfig::Off,
                crate::vis::AudioTap::new(),
                crate::eq::shared(),
            )
        };
        let bridge = Bridge {
            #[cfg(target_os = "linux")]
            name: "test".into(),
            state: Arc::new(State::new()),
            #[cfg(target_os = "linux")]
            config,
            #[cfg(target_os = "linux")]
            output: None,
            #[cfg(target_os = "linux")]
            worker: Mutex::new(None),
        };
        assert!(!bridge.owns_output_volume());
        bridge.state.routed.store(true, Ordering::Release);
        assert_eq!(
            bridge.owns_output_volume(),
            cfg!(all(target_os = "macos", feature = "soloist-native-audio"))
        );
        bridge.state.failed.store(true, Ordering::Release);
        assert!(!bridge.owns_output_volume());
    }
}

#[cfg(target_os = "linux")]
mod pipewire {
    use super::*;
    use ::pipewire as pw;
    use pw::{properties::properties, proxy::ProxyT, spa};
    use std::{
        cell::{Cell, RefCell},
        collections::{HashMap, HashSet, VecDeque},
        rc::Rc,
        sync::atomic::AtomicUsize,
    };
    /// How often the main loop reports what went wrong in the audio path.
    const REPORT_EVERY: Duration = Duration::from_secs(10);
    /// A shorter run of digital silence between sounds is a piece missing
    /// from what Soloist sent, not a quiet moment in a song.
    const GAP_LIMIT: Duration = Duration::from_millis(250);
    const EPIPE: i32 = 32;
    const RATE: usize = 44_100;
    /// The fastest rate the streams accept. It sizes the queues once and for
    /// all, at `MAX_HELD_SECONDS` of stereo.
    const MAX_RATE: usize = 384_000;
    /// Samples per second of stereo held at most before queued audio counts
    /// as abandoned: a target at PipeWire's largest quantum, and a cycle more.
    const MAX_HELD: f64 = 1.5;
    /// The least audio the output keeps queued. Soloist's stream is async, so
    /// the graph doesn't wait for it, and the two streams' order inside a
    /// cycle isn't fixed either. This rides out both: one flipped order costs
    /// a quantum, one late cycle another, at the usual 21 ms quantum. Measured
    /// against 100 ms, more margin only made each re-prime longer.
    const MIN_TARGET: Duration = Duration::from_millis(60);
    /// How much the output's read may run off the true rate to put a margin
    /// back or take one off: about 9 cents, under what anyone can hear, and
    /// enough to rebuild the margin in 12 s with no silence at all.
    const DRIFT: f64 = 0.005;
    /// The most the target grows to on a machine that keeps running dry.
    const MAX_TARGET: Duration = Duration::from_millis(250);
    /// What one dropout adds to the target.
    const TARGET_STEP: Duration = Duration::from_millis(40);
    /// What each `CLEAN_SPELL` without a dropout takes off it again.
    const TARGET_EASE: Duration = Duration::from_millis(10);
    const CLEAN_SPELL: Duration = Duration::from_secs(30);
    /// How far over the target the queue may be before it is cut, not eased.
    const CUT_OVER: Duration = Duration::from_millis(50);
    /// The longest fade at either end of a hole or a splice.
    const FADE: Duration = Duration::from_millis(5);
    /// How much output passes between looks at whether the delay has grown.
    const TRIM_EVERY: Duration = Duration::from_secs(1);
    /// The samples processed in one go, a whole number of stereo frames.
    /// A longer quantum than this is worked through in pieces.
    const CHUNK: usize = 4096;
    /// How long the input may stay silent behind a link that won't go away.
    const FOREIGN_LIMIT: Duration = Duration::from_secs(10);
    /// How many timer ticks the streams may refuse to change state.
    const ACTIVATE_TRIES: u32 = 20;

    #[derive(Default)]
    struct Graph {
        clients: HashMap<u32, u32>,
        nodes: HashMap<u32, u32>,
        names: HashMap<u32, String>,
        links: HashMap<u32, (u32, u32)>,
        /// Other processes running the child's own program: Soloist starts
        /// one, in a session of its own, so it is not the child by PID.
        helpers: HashSet<u32>,
        checked: HashSet<u32>,
    }
    #[derive(Debug, PartialEq, Eq)]
    enum Route {
        Waiting,
        Owned,
        Foreign,
    }
    impl Graph {
        fn route(&self, input: u32, pid: u32) -> Route {
            if pid == 0 {
                return Route::Waiting;
            }
            let mut any = false;
            for (from, _) in self.links.values().filter(|(_, to)| *to == input) {
                let Some(owner) = self
                    .nodes
                    .get(from)
                    .and_then(|client| self.clients.get(client))
                else {
                    return Route::Waiting;
                };
                if *owner != pid && !self.helpers.contains(owner) {
                    return Route::Foreign;
                }
                any = true;
            }
            if any { Route::Owned } else { Route::Waiting }
        }

        /// The links into `input` from anything but the child.
        fn foreign_links(&self, input: u32, pid: u32) -> Vec<u32> {
            self.links
                .iter()
                .filter(|(_, (from, to))| {
                    *to == input
                        && self
                            .nodes
                            .get(from)
                            .and_then(|client| self.clients.get(client))
                            .is_some_and(|owner| *owner != pid && !self.helpers.contains(owner))
                })
                .map(|(link, _)| *link)
                .collect()
        }

        fn link_source(&self, link: u32) -> &str {
            self.links
                .get(&link)
                .and_then(|(from, _)| self.names.get(from))
                .map_or("an unnamed node", String::as_str)
        }

        /// Sorts out which clients run the child's program, by its executable
        /// rather than a name any client could claim. Each process once.
        fn note_helpers(&mut self, child: u32) {
            let program = |pid: u32| std::fs::read_link(format!("/proc/{pid}/exe")).ok();
            let Some(own) = program(child) else { return };
            let pids: Vec<u32> = self.clients.values().copied().collect();
            for pid in pids {
                if pid != child && self.checked.insert(pid) && program(pid).as_ref() == Some(&own) {
                    self.helpers.insert(pid);
                }
            }
        }
    }

    /// Whether a core error means the connection is gone. Everything else,
    /// even on the core object, is one request PipeWire turned down, with the
    /// connection fine: pw_stream, on the same event, stops only for this.
    /// A hangup is reported as -EPIPE on PW_ID_CORE (module-protocol-native.c).
    fn connection_lost(id: u32, res: i32) -> bool {
        id == pw::core::PW_ID_CORE && res == -EPIPE
    }

    /// Proxies of objects that have left, held until the server has said so.
    ///
    /// The registry reports a global's removal before the server has told us
    /// its resource went. Dropping a proxy in between asks the server to
    /// destroy what it no longer has, and it answers "unknown resource N
    /// op:7" (PW_CORE_METHOD_DESTROY). A proxy dropped after it was removed
    /// sends nothing.
    struct Leaving<T>(Vec<(Rc<Cell<bool>>, T)>);
    impl<T> Default for Leaving<T> {
        fn default() -> Self {
            Self(Vec::new())
        }
    }
    impl<T> Leaving<T> {
        fn add(&mut self, gone: Rc<Cell<bool>>, proxy: T) {
            self.0.push((gone, proxy));
        }
        /// Drops the ones the server has removed.
        fn settle(&mut self) {
            self.0.retain(|(gone, _)| !gone.get());
        }
    }

    /// Samples between the two streams' callbacks: the input writes, the
    /// output reads. Their threads may differ, and neither can wait, so this
    /// is lock-free. Each sample is an atomic, which needs no unsafe code and
    /// is an ordinary load or store on anything this runs on.
    struct Ring {
        samples: Box<[AtomicU32]>,
        /// Counts of samples ever written and read, wrapping.
        written: AtomicUsize,
        read: AtomicUsize,
    }
    impl Ring {
        fn new(capacity: usize) -> Self {
            Self {
                samples: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
                written: AtomicUsize::new(0),
                read: AtomicUsize::new(0),
            }
        }
        fn len(&self) -> usize {
            self.written
                .load(Ordering::Acquire)
                .wrapping_sub(self.read.load(Ordering::Acquire))
        }
        /// For the writer. False, with nothing written, if they won't all fit.
        fn write(&self, samples: &[[u8; 4]]) -> bool {
            if self.len() + samples.len() > self.samples.len() {
                return false;
            }
            let mut at = self.written.load(Ordering::Relaxed);
            for bytes in samples {
                let sample = f32::from_le_bytes(*bytes);
                let sample = if sample.is_finite() { sample } else { 0.0 };
                self.samples[at % self.samples.len()].store(sample.to_bits(), Ordering::Relaxed);
                at = at.wrapping_add(1);
            }
            self.written.store(at, Ordering::Release);
            true
        }
        /// For the reader. How many samples it took into `out`.
        fn read(&self, out: &mut [f32]) -> usize {
            let count = self.len().min(out.len());
            let mut at = self.read.load(Ordering::Relaxed);
            for slot in &mut out[..count] {
                let bits = self.samples[at % self.samples.len()].load(Ordering::Relaxed);
                *slot = f32::from_bits(bits);
                at = at.wrapping_add(1);
            }
            self.read.store(at, Ordering::Release);
            count
        }
        /// For the reader. Leaves whatever has been written unread.
        fn skip(&self) {
            self.read
                .store(self.written.load(Ordering::Acquire), Ordering::Release);
        }
    }

    /// What happened in the realtime callbacks, kept in atomics so they can
    /// count without waiting. The main loop empties these into the log.
    struct Stats {
        /// Output cycles the queue couldn't fill: an audible dropout here.
        short_reads: AtomicU32,
        short_frames: AtomicU32,
        /// Output cycles rendered silent because the chain was being swapped.
        chain_misses: AtomicU32,
        /// Input blocks that didn't fit in the ring.
        overflows: AtomicU32,
        /// Frames thrown away to keep the delay from growing.
        skipped_frames: AtomicU32,
        /// Input blocks dropped while the input's owner was in doubt.
        unowned: AtomicU32,
        /// Cycles when the input's callback got no audio at all.
        input_empty: AtomicU32,
        /// Input blocks smaller than the output's cycle, and the smallest.
        /// Nothing else sees these: such a cycle has input, and it isn't zero.
        short_inputs: AtomicU32,
        smallest_input: AtomicUsize,
        /// Frames in at the input's rate, and out at the output's: what the
        /// two streams really moved, which a steady deficit shows up in.
        frames_in: AtomicU32,
        frames_out: AtomicU32,
        /// Frames of silence rendered while holding for the margin.
        held_frames: AtomicU32,
        /// Runs of silence inside the sound: missing before our ring.
        input_gaps: AtomicU32,
        input_gap_frames: AtomicU32,
        /// Output cycles no input had arrived since the one before. Harmless
        /// as long as the queue has a margin, so these log only alongside.
        cycles_without_input: AtomicU32,
        /// The fewest frames left in the queue after a read.
        low_water: AtomicUsize,
        /// The input's run of digital silence, and whether sound came before.
        quiet_frames: AtomicUsize,
        sounding: AtomicBool,
        /// The queue ran out inside that run: unheard if the silence was the
        /// song's, a dropout if the sound comes back as from a hole.
        dry_in_quiet: AtomicBool,
        /// The output's latest cycle, for the log.
        quantum: AtomicUsize,
        rate: AtomicU32,
        /// The frames the queue is being steered towards.
        target: AtomicUsize,
        /// The queue's smoothed fill: how far behind Soloist's clock we are.
        delay_frames: AtomicUsize,
        /// Dropouts ever, which the chain's target follows. Never emptied.
        faults: AtomicU32,
        /// Output frames read off the true rate.
        drift_frames: AtomicU32,
    }
    impl Default for Stats {
        fn default() -> Self {
            Self {
                short_reads: AtomicU32::new(0),
                short_frames: AtomicU32::new(0),
                chain_misses: AtomicU32::new(0),
                overflows: AtomicU32::new(0),
                skipped_frames: AtomicU32::new(0),
                unowned: AtomicU32::new(0),
                input_empty: AtomicU32::new(0),
                short_inputs: AtomicU32::new(0),
                smallest_input: AtomicUsize::new(usize::MAX),
                frames_in: AtomicU32::new(0),
                frames_out: AtomicU32::new(0),
                held_frames: AtomicU32::new(0),
                input_gaps: AtomicU32::new(0),
                input_gap_frames: AtomicU32::new(0),
                cycles_without_input: AtomicU32::new(0),
                low_water: AtomicUsize::new(usize::MAX),
                quiet_frames: AtomicUsize::new(0),
                sounding: AtomicBool::new(false),
                dry_in_quiet: AtomicBool::new(false),
                quantum: AtomicUsize::new(0),
                rate: AtomicU32::new(RATE as u32),
                target: AtomicUsize::new(0),
                delay_frames: AtomicUsize::new(0),
                faults: AtomicU32::new(0),
                drift_frames: AtomicU32::new(0),
            }
        }
    }
    impl Stats {
        fn add(counter: &AtomicU32, by: usize) {
            counter.fetch_add(by as u32, Ordering::Relaxed);
        }
        /// For the input's callback: what a block says about what Soloist
        /// sent. A late Soloist isn't an xrun, its stream being async, and
        /// PipeWire fills the cycle it missed with zeros. True for those: the
        /// music comes after them, late rather than lost, so they are no part
        /// of it, and the output's margin plays across the hole. If it was a
        /// silence in the song after all, the queue runs out in that silence,
        /// where nobody can hear it.
        fn is_hole(&self, samples: &[[u8; 4]], input_rate: u32) -> bool {
            let frames = samples.len() / 2;
            if frames == 0 {
                return false;
            }
            let limit = (f64::from(input_rate) * GAP_LIMIT.as_secs_f64()) as usize;
            if samples
                .iter()
                .all(|bytes| f32::from_le_bytes(*bytes) == 0.0)
            {
                let quiet = self.quiet_frames.fetch_add(frames, Ordering::Relaxed) + frames;
                return quiet <= limit && self.sounding.load(Ordering::Relaxed);
            }
            let quiet = self.quiet_frames.swap(0, Ordering::Relaxed);
            let dry = self.dry_in_quiet.swap(false, Ordering::Relaxed);
            if quiet > 0 && quiet <= limit && self.sounding.load(Ordering::Relaxed) {
                Self::add(&self.input_gaps, 1);
                Self::add(&self.input_gap_frames, quiet);
                if dry {
                    Self::add(&self.short_reads, 1);
                    Self::add(&self.short_frames, quiet);
                    Self::add(&self.faults, 1);
                }
            }
            self.sounding.store(true, Ordering::Relaxed);
            false
        }
        /// Sound from before a pause or a flush says nothing of what follows.
        fn restart(&self) {
            self.sounding.store(false, Ordering::Relaxed);
            self.quiet_frames.store(0, Ordering::Relaxed);
            self.dry_in_quiet.store(false, Ordering::Relaxed);
        }
        fn delay_ms(&self) -> u32 {
            let frames = self.delay_frames.load(Ordering::Relaxed) as u64;
            (frames * 1000 / u64::from(self.rate.load(Ordering::Relaxed).max(1))) as u32
        }
        /// Empties the counters.
        fn take(&self) -> Report {
            let take = |counter: &AtomicU32| counter.swap(0, Ordering::Relaxed);
            Report {
                short_reads: take(&self.short_reads),
                short_frames: take(&self.short_frames),
                chain_misses: take(&self.chain_misses),
                overflows: take(&self.overflows),
                skipped_frames: take(&self.skipped_frames),
                unowned: take(&self.unowned),
                input_empty: take(&self.input_empty),
                short_inputs: take(&self.short_inputs),
                smallest_input: self.smallest_input.swap(usize::MAX, Ordering::Relaxed),
                frames_in: take(&self.frames_in),
                frames_out: take(&self.frames_out),
                held_frames: take(&self.held_frames),
                drift_frames: take(&self.drift_frames),
                target: self.target.load(Ordering::Relaxed),
                input_gaps: take(&self.input_gaps),
                input_gap_frames: take(&self.input_gap_frames),
                cycles_without_input: take(&self.cycles_without_input),
                low_water: self.low_water.swap(usize::MAX, Ordering::Relaxed),
                quantum: self.quantum.load(Ordering::Relaxed),
                rate: self.rate.load(Ordering::Relaxed),
            }
        }
    }

    /// One report's worth of `Stats`.
    #[derive(Debug, Default, PartialEq, Eq)]
    struct Report {
        short_reads: u32,
        short_frames: u32,
        chain_misses: u32,
        overflows: u32,
        skipped_frames: u32,
        unowned: u32,
        input_empty: u32,
        short_inputs: u32,
        smallest_input: usize,
        frames_in: u32,
        frames_out: u32,
        held_frames: u32,
        drift_frames: u32,
        target: usize,
        input_gaps: u32,
        input_gap_frames: u32,
        cycles_without_input: u32,
        low_water: usize,
        quantum: usize,
        rate: u32,
    }
    impl Report {
        /// The log line, if anything went wrong. Which number moved says where
        /// the fault is: `input gaps` and `empty inputs` are before our ring
        /// (Soloist was late), the rest are in it.
        fn line(&self) -> Option<String> {
            if self.short_reads == 0
                && self.chain_misses == 0
                && self.overflows == 0
                && self.skipped_frames == 0
                && self.unowned == 0
                && self.input_empty == 0
                && self.input_gaps == 0
                && self.short_inputs == 0
            {
                return None;
            }
            let ms = |frames: u64| frames * 1000 / u64::from(self.rate.max(1));
            let low = match self.low_water {
                usize::MAX => "no reads".to_owned(),
                low => format!("{} ms", ms(low as u64)),
            };
            let smallest = match self.smallest_input {
                usize::MAX => 0,
                smallest => smallest,
            };
            Some(format!(
                "Soloist audio, last {} s: {} short reads ({} ms silent), {} chain lock misses, \
                 {} ring overflows, {} ms skipped, {} unowned blocks dropped, {} input gaps \
                 ({} ms), {} empty inputs, {} short inputs (smallest {smallest}), \
                 {} cycles without input, lowest margin {low}, quantum {}, \
                 {} frames in, {} out, {} ms held, {} ms read off rate, target {} ms",
                REPORT_EVERY.as_secs(),
                self.short_reads,
                ms(u64::from(self.short_frames)),
                self.chain_misses,
                self.overflows,
                ms(u64::from(self.skipped_frames)),
                self.unowned,
                self.input_gaps,
                ms(u64::from(self.input_gap_frames)),
                self.input_empty,
                self.short_inputs,
                self.cycles_without_input,
                self.quantum,
                self.frames_in,
                self.frames_out,
                ms(u64::from(self.held_frames)),
                ms(u64::from(self.drift_frames)),
                ms(self.target as u64),
            ))
        }
    }

    /// Everything the output's callback works on. It owns this for as long
    /// as it holds the lock, and only ever tries for it. Allocating is done
    /// beforehand, here in `new`, for the sizes `pull` and `block` work in.
    struct Chain {
        eq: crate::eq::Processor,
        input_rate: u32,
        output_rate: u32,
        input_resampler: Option<crate::resample::Resampler>,
        tap_resampler: Option<crate::resample::Resampler>,
        tap: Arc<crate::vis::AudioTap>,
        limiter: crate::limiter::Limiter,
        samples: VecDeque<f64>,
        gain: f64,
        /// One `CHUNK` from the ring, and what the rate conversion makes of it.
        pulled: Vec<f32>,
        converted: Vec<f32>,
        /// One `CHUNK` on its way out, and its copies for the tap.
        block: Vec<f64>,
        narrow: Vec<f32>,
        tapped_narrow: Vec<f32>,
        tapped: Vec<f64>,
        /// The ring's write count at the output's last cycle.
        seen: usize,
        /// Whether the output is playing the queue, or holding for it to fill.
        primed: bool,
        /// The fullest the queue has stayed before a read, since `watched`
        /// frames ago: what could go without ever running short.
        lowest: usize,
        watched: usize,
        /// What repeated dropouts have added to the target, in frames, and
        /// the frames played since the last one.
        extra: usize,
        clean: usize,
        /// The dropouts `Stats` had counted when this last looked.
        faults: u32,
        /// How far the read moves per output frame, what it is meant to be,
        /// and how far it stands between two queued frames.
        step: f64,
        wanted_step: f64,
        phase: f64,
        /// The queue's fill, smoothed: how far behind Soloist the sound is.
        delay: f64,
    }
    impl Chain {
        fn new(
            eq: crate::eq::Processor,
            tap: Arc<crate::vis::AudioTap>,
            input_rate: u32,
            output_rate: u32,
        ) -> Self {
            let resampler = |from, to| {
                let mut resampler = crate::resample::Resampler::new(from, to, 2);
                if let Some(resampler) = &mut resampler {
                    resampler.reserve(CHUNK);
                }
                resampler
            };
            // A chunk converted, and the frames rounding can add to it.
            let room = |from: u32, to: u32| (CHUNK * to as usize).div_ceil(from as usize) + 64;
            Self {
                eq,
                input_rate,
                output_rate,
                input_resampler: resampler(input_rate, output_rate),
                tap_resampler: resampler(output_rate, RATE as u32),
                tap,
                limiter: crate::limiter::Limiter::new(f64::from(output_rate)),
                samples: VecDeque::with_capacity((MAX_RATE as f64 * MAX_HELD) as usize),
                gain: 0.0,
                pulled: vec![0.0; CHUNK],
                converted: Vec::with_capacity(room(input_rate, output_rate)),
                block: vec![0.0; CHUNK],
                narrow: vec![0.0; CHUNK],
                tapped_narrow: Vec::with_capacity(room(output_rate, RATE as u32)),
                tapped: vec![0.0; room(output_rate, RATE as u32)],
                seen: 0,
                primed: false,
                lowest: usize::MAX,
                watched: 0,
                extra: 0,
                clean: 0,
                faults: 0,
                step: 1.0,
                wanted_step: 1.0,
                phase: 0.0,
                delay: 0.0,
            }
        }
        fn frames_in(&self, time: Duration) -> usize {
            (f64::from(self.output_rate) * time.as_secs_f64()) as usize
        }
        /// The frames to hold before playing after running dry: two quanta,
        /// and never less than `MIN_TARGET`. A hold is silence, so it stays
        /// this short however large the target has grown.
        fn floor(&self, frames: usize) -> usize {
            self.frames_in(MIN_TARGET).max(frames * 2)
        }
        /// The frames the queue is steered towards.
        fn target(&self, frames: usize) -> usize {
            let floor = self.floor(frames);
            (floor + self.extra).min(self.frames_in(MAX_TARGET).max(floor))
        }
        /// Grows the target on a new dropout, and eases it back after a clean
        /// spell. Not during a hold: nothing plays there to be clean.
        fn adapt_target(&mut self, frames: usize, stats: &Stats) {
            let faults = stats.faults.load(Ordering::Relaxed);
            if std::mem::replace(&mut self.faults, faults) != faults {
                let most = self
                    .frames_in(MAX_TARGET)
                    .saturating_sub(self.floor(frames));
                self.extra = (self.extra + self.frames_in(TARGET_STEP)).min(most);
                self.clean = 0;
                return;
            }
            self.clean += frames;
            if self.clean >= self.frames_in(CLEAN_SPELL) {
                self.clean = 0;
                self.extra = self.extra.saturating_sub(self.frames_in(TARGET_EASE));
            }
        }
        /// Decides what this cycle of `frames` does: true plays the queue,
        /// false holds it and renders silence.
        fn pace(&mut self, frames: usize, stats: &Stats) -> bool {
            if frames == 0 {
                return false;
            }
            let fill = self.samples.len() / 2;
            self.delay += (fill as f64 - self.delay) * 0.02;
            stats
                .delay_frames
                .store(self.delay as usize, Ordering::Relaxed);
            if !self.primed {
                self.wanted_step = 1.0;
                self.step = 1.0;
                self.phase = 0.0;
                if fill < self.floor(frames) {
                    return false;
                }
                self.primed = true;
                // The fader's ramp brings the sound back without a click.
                self.gain = 0.0;
                self.lowest = usize::MAX;
                self.watched = 0;
            }
            stats
                .low_water
                .fetch_min(fill.saturating_sub(frames), Ordering::Relaxed);
            if fill < frames {
                // Running out in the song's own silence is no dropout, and the
                // hold after it puts back a margin spent on late cycles. The
                // input can only tell which it was when the sound returns.
                if stats.quiet_frames.load(Ordering::Relaxed) == 0 {
                    Stats::add(&stats.short_reads, 1);
                    Stats::add(&stats.short_frames, frames - fill);
                    Stats::add(&stats.faults, 1);
                } else {
                    stats.dry_in_quiet.store(true, Ordering::Relaxed);
                }
                self.fade_out(fill);
                self.primed = false;
                self.adapt_target(frames, stats);
                stats.target.store(self.target(frames), Ordering::Relaxed);
                return true;
            }
            self.adapt_target(frames, stats);
            let target = self.target(frames);
            stats.target.store(target, Ordering::Relaxed);
            // A margin comes back by reading a little slow, never by silence.
            // Between the two is the steady place: a quantum's worth of play
            // that is only where the cycles happen to fall.
            self.wanted_step = if fill < target {
                1.0 - DRIFT
            } else if fill > target + frames {
                1.0 + DRIFT
            } else {
                1.0
            };
            if self.wanted_step != 1.0 {
                self.step = self.wanted_step;
                Stats::add(&stats.drift_frames, frames);
            }
            self.lowest = self.lowest.min(fill);
            self.watched += frames;
            if self.watched >= (f64::from(self.output_rate) * TRIM_EVERY.as_secs_f64()) as usize {
                // A cycle or two over is the streams' order, or a chain lock
                // miss, and easing deals with it. A long way over is a change
                // of quantum, and too much to ease.
                let slack = frames + self.frames_in(CUT_OVER);
                if self.lowest > target + slack {
                    let excess = self.lowest - target;
                    self.splice(excess);
                    Stats::add(&stats.skipped_frames, excess);
                }
                self.lowest = usize::MAX;
                self.watched = 0;
            }
            true
        }
        fn fade_frames(&self) -> usize {
            (f64::from(self.output_rate) * FADE.as_secs_f64()) as usize
        }
        /// Fades what is left of the queue's `fill` frames into silence.
        fn fade_out(&mut self, fill: usize) {
            let fade = fill.min(self.fade_frames());
            for back in 0..fade {
                let gain = (back as f64 + 0.5) / fade as f64;
                let frame = fill - 1 - back;
                self.samples[frame * 2] *= gain;
                self.samples[frame * 2 + 1] *= gain;
            }
        }
        /// Takes `excess` frames off the front of the queue, crossfading what
        /// was next into what is now next so the jump doesn't click.
        fn splice(&mut self, excess: usize) {
            let fill = self.samples.len() / 2;
            let fade = self.fade_frames().min(fill.saturating_sub(excess));
            for frame in 0..fade {
                let share = (frame as f64 + 0.5) / fade as f64;
                for channel in 0..2 {
                    let old = self.samples[frame * 2 + channel];
                    let new = &mut self.samples[(frame + excess) * 2 + channel];
                    *new = old * (1.0 - share) + *new * share;
                }
            }
            self.samples.drain(..excess * 2);
        }
        /// Forgets the signal.
        fn reset(&mut self) {
            self.samples.clear();
            self.eq.reset();
            self.tap.try_clear();
            self.gain = 0.0;
            self.primed = false;
            self.step = 1.0;
            self.wanted_step = 1.0;
            self.phase = 0.0;
            if let Some(resampler) = &mut self.input_resampler {
                resampler.reset();
            }
            if let Some(resampler) = &mut self.tap_resampler {
                resampler.reset();
            }
            self.limiter.reset();
        }
        /// Moves what the input wrote into `samples`, at the output's rate.
        fn pull(&mut self, ring: &Ring, stats: &Stats) {
            let written = ring.written.load(Ordering::Acquire);
            if std::mem::replace(&mut self.seen, written) == written {
                Stats::add(&stats.cycles_without_input, 1);
            }
            // A missing output must never accumulate a song to play later.
            if ring.len() as f64 > f64::from(self.input_rate) * MAX_HELD {
                Stats::add(&stats.skipped_frames, ring.len() / 2);
                ring.skip();
                self.reset();
                return;
            }
            loop {
                let count = ring.read(&mut self.pulled);
                if count == 0 {
                    return;
                }
                self.converted.clear();
                match &mut self.input_resampler {
                    Some(resampler) => {
                        resampler.process_into(&self.pulled[..count], &mut self.converted);
                    }
                    None => self.converted.extend_from_slice(&self.pulled[..count]),
                }
                if (self.samples.len() + self.converted.len()) as f64
                    > f64::from(self.output_rate) * MAX_HELD
                {
                    Stats::add(&stats.skipped_frames, self.samples.len() / 2);
                    self.reset();
                    continue;
                }
                self.samples
                    .extend(self.converted.iter().copied().map(f64::from));
            }
        }
        /// Fills at most one `CHUNK` of interleaved float frames.
        /// The next frame of the queue, at the read's `step`. At a step of one
        /// on a whole frame this is the queue as it stands, sample for sample.
        fn next_frame(&mut self) -> [f64; 2] {
            let pop = |samples: &mut VecDeque<f64>| {
                [
                    samples.pop_front().unwrap_or(0.0),
                    samples.pop_front().unwrap_or(0.0),
                ]
            };
            if self.step == 1.0 && self.phase == 0.0 {
                return pop(&mut self.samples);
            }
            if self.samples.len() < 4 {
                self.phase = 0.0;
                return pop(&mut self.samples);
            }
            let share = self.phase;
            let frame = [0, 1].map(|channel| {
                self.samples[channel] * (1.0 - share) + self.samples[channel + 2] * share
            });
            self.phase += self.step;
            while self.phase >= 1.0 {
                self.phase -= 1.0;
                pop(&mut self.samples);
            }
            // Back on a whole frame the read returns to the true rate, so it
            // doesn't go on interpolating for nothing.
            if self.wanted_step == 1.0 && self.phase < DRIFT * 2.0 {
                self.step = 1.0;
                self.phase = 0.0;
            }
            frame
        }
        /// Silent, leaving the queue alone, unless it is to `play`.
        fn block(&mut self, bytes: &mut [u8], percent: u8, play: bool) {
            let count = bytes.len() / 8 * 2;
            self.block[..count].fill(0.0);
            if play {
                for at in (0..count).step_by(2) {
                    let frame = self.next_frame();
                    self.block[at..at + 2].copy_from_slice(&frame);
                }
            }
            let samples = &mut self.block[..count];
            self.eq.apply(samples);
            if let Some(resampler) = &mut self.tap_resampler {
                for (narrow, sample) in self.narrow.iter_mut().zip(samples.iter()) {
                    *narrow = *sample as f32;
                }
                self.tapped_narrow.clear();
                resampler.process_into(&self.narrow[..count], &mut self.tapped_narrow);
                for (wide, sample) in self.tapped.iter_mut().zip(&self.tapped_narrow) {
                    *wide = f64::from(*sample);
                }
                self.tap
                    .try_push(&self.tapped[..self.tapped_narrow.len()], 1.0);
            } else {
                self.tap.try_push(samples, 1.0);
            }
            // Matches Soloist's native PipeWire curve: 50% is 0.125 gain.
            let wanted = (f64::from(percent.min(100)) / 100.0).powi(3);
            for frame in samples.as_chunks_mut::<2>().0 {
                self.gain += (wanted - self.gain).clamp(
                    -1.0 / (f64::from(self.output_rate) * 0.03),
                    1.0 / (f64::from(self.output_rate) * 0.03),
                );
                for sample in frame {
                    *sample *= self.gain;
                }
            }
            self.limiter.process(samples, 1.0);
            for (out, sample) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(samples.iter()) {
                out.copy_from_slice(&(*sample as f32).to_le_bytes());
            }
        }
    }

    /// The audio path, shared by the main loop and PipeWire's realtime data
    /// thread. `push` and `render` run on the latter and must never wait,
    /// allocate or log. The rest runs on the main loop: it builds what the
    /// callbacks will use, and takes the chain's lock only to swap it in.
    struct Audio {
        ring: Ring,
        chain: Mutex<Chain>,
        /// The chain is to forget the signal before it next renders.
        flush: AtomicBool,
        /// The input's rate, for what `push` counts.
        input_rate: AtomicU32,
        /// Only the owned child feeds the input, as the main loop last saw.
        owned: AtomicBool,
        stats: Stats,
        tap: Arc<crate::vis::AudioTap>,
        shared_eq: crate::eq::SharedEq,
        /// The main loop's: what the chain was last built from.
        settings: Mutex<(u32, u32, crate::eq::EqSettings)>,
    }
    impl Audio {
        fn new(eq: crate::eq::SharedEq, tap: Arc<crate::vis::AudioTap>) -> Self {
            let rate = RATE as u32;
            let settings = Self::eq_settings(&eq);
            let processor = crate::eq::Processor::prepared(eq.clone(), rate);
            Self {
                ring: Ring::new((MAX_RATE as f64 * MAX_HELD) as usize),
                chain: Mutex::new(Chain::new(processor, tap.clone(), rate, rate)),
                flush: AtomicBool::new(false),
                input_rate: AtomicU32::new(rate),
                owned: AtomicBool::new(false),
                stats: Stats::default(),
                tap,
                shared_eq: eq,
                settings: Mutex::new((rate, rate, settings)),
            }
        }
        fn eq_settings(eq: &crate::eq::SharedEq) -> crate::eq::EqSettings {
            eq.lock()
                .map(|settings| settings.clamped())
                .unwrap_or_else(|poisoned| poisoned.into_inner().clamped())
        }
        /// Swaps in `change`, keeping the lock for the swap alone. What it
        /// replaces is freed here, away from the realtime thread.
        fn swap(&self, mut change: impl FnMut(&mut Chain)) {
            let mut chain = self.chain.lock().unwrap_or_else(|p| p.into_inner());
            change(&mut chain);
        }
        fn rates(&self, input: Option<u32>, output: Option<u32>) {
            let mut settings = self.settings.lock().unwrap_or_else(|p| p.into_inner());
            let input = input.unwrap_or(settings.0);
            let output = output.unwrap_or(settings.1);
            let wanted = Self::eq_settings(&self.shared_eq);
            *settings = (input, output, wanted);
            self.input_rate.store(input, Ordering::Relaxed);
            let eq = crate::eq::Processor::prepared(self.shared_eq.clone(), output);
            let mut fresh = Chain::new(eq, self.tap.clone(), input, output);
            self.swap(|chain| std::mem::swap(chain, &mut fresh));
            drop(fresh);
            self.clear();
        }
        /// Takes up an equalizer change, if there is one. The chain would
        /// have to allocate to do this itself.
        fn refresh_eq(&self) {
            let mut settings = self.settings.lock().unwrap_or_else(|p| p.into_inner());
            let wanted = Self::eq_settings(&self.shared_eq);
            if settings.2 == wanted {
                return;
            }
            settings.2 = wanted;
            let mut eq = crate::eq::Processor::prepared(self.shared_eq.clone(), settings.1);
            self.swap(|chain| std::mem::swap(&mut chain.eq, &mut eq));
        }
        /// Forgets queued audio, and at once what the tap shows.
        fn clear(&self) {
            self.flush.store(true, Ordering::Release);
            self.stats.restart();
            self.tap.clear();
        }
        /// The input's callback.
        fn push(&self, bytes: &[u8]) {
            // A mix is never processed: while anything else is linked in, the
            // input is dropped and the main loop deals with it.
            let samples = bytes.as_chunks::<4>().0;
            let stereo = samples.len() / 2 * 2;
            if !self.owned.load(Ordering::Acquire) {
                Stats::add(&self.stats.unowned, usize::from(stereo > 0));
                self.flush.store(true, Ordering::Release);
                return;
            }
            let rate = self.input_rate.load(Ordering::Relaxed);
            let frames = stereo / 2;
            let cycle = self.stats.quantum.load(Ordering::Relaxed) as u64 * u64::from(rate)
                / u64::from(self.stats.rate.load(Ordering::Relaxed).max(1));
            if frames > 0 && (frames as u64) < cycle {
                Stats::add(&self.stats.short_inputs, 1);
                self.stats
                    .smallest_input
                    .fetch_min(frames, Ordering::Relaxed);
            }
            if self.stats.is_hole(&samples[..stereo], rate) {
                return;
            }
            Stats::add(&self.stats.frames_in, frames);
            if !self.ring.write(&samples[..stereo]) {
                Stats::add(&self.stats.overflows, 1);
                self.flush.store(true, Ordering::Release);
            }
        }
        /// The output's callback.
        fn render(&self, bytes: &mut [u8], percent: u8) {
            let Ok(mut chain) = self.chain.try_lock() else {
                // The main loop is swapping the chain in: a format change, or
                // an equalizer change. The ring keeps the sound for next time.
                Stats::add(&self.stats.chain_misses, usize::from(!bytes.is_empty()));
                bytes.fill(0);
                return;
            };
            if self.flush.swap(false, Ordering::AcqRel) {
                self.ring.skip();
                chain.reset();
            }
            chain.pull(&self.ring, &self.stats);
            let frames = bytes.len() / 8;
            self.stats.quantum.store(frames, Ordering::Relaxed);
            self.stats.rate.store(chain.output_rate, Ordering::Relaxed);
            let play = chain.pace(frames, &self.stats);
            let moved = if play {
                &self.stats.frames_out
            } else {
                &self.stats.held_frames
            };
            Stats::add(moved, frames);
            for block in bytes.chunks_mut(CHUNK * 4) {
                chain.block(block, percent, play);
            }
        }
    }

    /// Marks a `process` callback. pipewire-rs takes these on a "local"
    /// listener, with no `Send` bound, though RT_PROCESS calls them on another
    /// thread. This bound is that check: no `Rc` of the main loop gets in.
    fn realtime<F>(callback: F) -> F
    where
        F: FnMut(&pw::stream::Stream, &mut ()) + Send + 'static,
    {
        callback
    }

    pub(super) fn run(
        name: &str,
        config: crate::player::EngineConfig,
        state: Arc<State>,
        ready: tokio::sync::oneshot::Sender<()>,
        output_target: Option<&str>,
    ) -> anyhow::Result<()> {
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        let core = context.connect_rc(None)?;
        let core_state = state.clone();
        let core_loop = mainloop.clone();
        let _core_listener = core
            .add_listener_local()
            .error(move |id, seq, res, message| {
                if connection_lost(id, res) {
                    core_state.fail(format!("lost the PipeWire connection: {message}"));
                    core_loop.quit();
                } else {
                    log::debug!("PipeWire refused a request ({id}, {seq}, {res}): {message}");
                }
            })
            .register();
        let graph = Rc::new(RefCell::new(Graph::default()));
        let audio = Arc::new(Audio::new(config.eq, config.tap));
        // The input's node id, once it has one: the timer keeps this.
        let input_id = Rc::new(Cell::new(pw::constants::ID_ANY));
        // The realtime callback can't see the graph, so what it needs of it
        // is published, as soon as a link or a client changes the answer.
        let gate = {
            let (audio, state, input_id) = (audio.clone(), state.clone(), input_id.clone());
            Rc::new(move |graph: &Graph| {
                let route = graph.route(input_id.get(), state.child.load(Ordering::Acquire));
                audio.owned.store(route == Route::Owned, Ordering::Release);
            })
        };
        let registry = core.get_registry_rc()?;
        let added = graph.clone();
        let removed = graph.clone();
        let added_gate = gate.clone();
        let removed_gate = gate.clone();
        let weak_registry = registry.downgrade();
        // Client PIDs arrive with ClientInfo, not the registry's minimal props.
        let clients = Rc::new(RefCell::new(HashMap::new()));
        let added_clients = clients.clone();
        let removed_clients = clients;
        let leaving = Rc::new(RefCell::new(Leaving::default()));
        let left = leaving.clone();

        let _registry_listener = registry
            .add_listener_local()
            .global(move |global| {
                let Some(props) = global.props else { return };
                let number = |key: &str| props.get(key).and_then(|s| s.parse::<u32>().ok());
                let mut graph = added.borrow_mut();
                match global.type_ {
                    pw::types::ObjectType::Client => {
                        if let Some(registry) = weak_registry.upgrade()
                            && let Ok(client) = registry.bind::<pw::client::Client, _>(global)
                        {
                            let clients_graph = added.clone();
                            let client_gate = added_gate.clone();
                            let id = global.id;
                            let listener = client
                                .add_listener_local()
                                .info(move |info| {
                                    if let Some(pid) = info
                                        .props()
                                        .and_then(|p| p.get("application.process.id"))
                                        .and_then(|s| s.parse::<u32>().ok())
                                    {
                                        let mut graph = clients_graph.borrow_mut();
                                        graph.clients.insert(id, pid);
                                        client_gate(&graph);
                                    }
                                })
                                .register();
                            let gone = Rc::new(Cell::new(false));
                            let removed = gone.clone();
                            let proxy = client
                                .upcast_ref()
                                .add_listener_local()
                                .removed(move || removed.set(true))
                                .register();
                            added_clients
                                .borrow_mut()
                                .insert(id, (gone, (client, listener, proxy)));
                        }
                    }
                    pw::types::ObjectType::Node => {
                        if let Some(client) = number("client.id") {
                            graph.nodes.insert(global.id, client);
                        }
                        if let Some(name) = props.get("node.name") {
                            graph.names.insert(global.id, name.to_owned());
                        }
                    }
                    pw::types::ObjectType::Link => {
                        if let (Some(from), Some(to)) =
                            (number("link.output.node"), number("link.input.node"))
                        {
                            graph.links.insert(global.id, (from, to));
                            added_gate(&graph);
                        }
                    }
                    _ => {}
                }
            })
            .global_remove(move |id| {
                let mut graph = removed.borrow_mut();
                graph.clients.remove(&id);
                graph.nodes.remove(&id);
                graph.names.remove(&id);
                graph.links.remove(&id);
                removed_gate(&graph);
                if let Some((gone, client)) = removed_clients.borrow_mut().remove(&id) {
                    left.borrow_mut().add(gone, client);
                }
            })
            .register();
        let input = pw::stream::StreamRc::new(
            core.clone(),
            "Soloist processing",
            properties! {
                "media.type" => "Audio", "media.category" => "Capture", "media.role" => "Music",
                // Soloist only selects sink devices, not capture streams.
                "media.class" => "Audio/Sink", "node.name" => name,
                "node.description" => "Spotifast Soloist processing", "node.virtual" => "true",
                "priority.session" => "0", "node.link-group" => name, "node.group" => name,
                // Exclude this sink from automatic/default device selection.
                // Its unique target is our output stream, not a speaker or
                // an absent target shared with another application's filter.
                "filter.smart" => "true", "filter.smart.targetable" => "true",
                "filter.smart.target" => format!(r#"{{"node.name":"{name}.output"}}"#),
                "node.hidden" => "true",
                "audio.position" => "[ FL FR ]",
            },
        )?;
        let mut output_properties = properties! {
            "media.type" => "Audio", "media.category" => "Playback", "media.role" => "Music",
            // This is ordinary playback, not the stream of a smart filter:
            // WirePlumber must not chain it through other private inputs.
            "node.name" => format!("{name}.output"), "node.group" => name,
            "audio.position" => "[ FL FR ]",
        };
        if let Some(target) = output_target {
            output_properties.insert("target.object", target);
            output_properties.insert("node.dont-fallback", "true");
            // An explicit target must not be replaced by follow-default metadata.
            output_properties.insert("node.dont-move", "true");
        }
        let output = pw::stream::StreamRc::new(core, "Soloist output", output_properties)?;
        let readiness = Rc::new((Cell::new(0u8), RefCell::new(Some(ready))));
        let ready_input = readiness.clone();
        let input_state = state.clone();
        let in_audio = audio.clone();
        let in_format = audio.clone();
        let in_fail = state.clone();
        let _input_listener = input
            .add_local_listener::<()>()
            .state_changed(move |_, _, _, new| {
                if let pw::stream::StreamState::Error(error) = &new {
                    input_state.fail(format!("input stream error: {error}"));
                }
                if matches!(
                    new,
                    pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming
                ) {
                    ready_input.0.set(ready_input.0.get() | 1);
                    if ready_input.0.get() == 3
                        && let Some(ready) = ready_input.1.borrow_mut().take()
                    {
                        let _ = ready.send(());
                    }
                }
            })
            .param_changed(move |_, _, id, param| {
                if id == spa::param::ParamType::Format.as_raw()
                    && let Some(param) = param
                {
                    let mut info = spa::param::audio::AudioInfoRaw::new();
                    if info.parse(param).is_ok()
                        && info.channels() == 2
                        && (44100..=384000).contains(&info.rate())
                    {
                        in_format.rates(Some(info.rate()), None);
                    } else {
                        in_fail.fail(format!(
                            "unusable input format: {} channels at {} Hz",
                            info.channels(),
                            info.rate()
                        ));
                    }
                }
            })
            .register()?;
        // On the realtime thread. A listener of its own, so the callback
        // shares no state with the ones the main loop calls.
        let _input_process = input
            .add_local_listener::<()>()
            .process(realtime(move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    Stats::add(&in_audio.stats.input_empty, 1);
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else {
                    return;
                };
                let offset = data.chunk().offset() as usize;
                let size = data.chunk().size() as usize;
                Stats::add(&in_audio.stats.input_empty, usize::from(size == 0));
                let Some(bytes) = data.data() else { return };
                if let Some(bytes) = bytes.get(offset..offset.saturating_add(size)) {
                    in_audio.push(bytes);
                }
            }))
            .register()?;
        let ready_output = readiness;
        let output_state = state.clone();
        let out_audio = audio.clone();
        let volume = state.clone();
        let out_format = audio.clone();
        let out_fail = state.clone();
        let _output_listener = output
            .add_local_listener::<()>()
            .state_changed(move |_, _, _, new| {
                if let pw::stream::StreamState::Error(error) = &new {
                    output_state.fail(format!("output stream error: {error}"));
                }
                if matches!(
                    new,
                    pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming
                ) {
                    ready_output.0.set(ready_output.0.get() | 2);
                    if ready_output.0.get() == 3
                        && let Some(ready) = ready_output.1.borrow_mut().take()
                    {
                        let _ = ready.send(());
                    }
                }
            })
            .param_changed(move |_, _, id, param| {
                if id == spa::param::ParamType::Format.as_raw()
                    && let Some(param) = param
                {
                    let mut info = spa::param::audio::AudioInfoRaw::new();
                    if info.parse(param).is_ok()
                        && info.channels() == 2
                        && (44100..=384000).contains(&info.rate())
                    {
                        out_format.rates(None, Some(info.rate()));
                    } else {
                        out_fail.fail(format!(
                            "unusable output format: {} channels at {} Hz",
                            info.channels(),
                            info.rate()
                        ));
                    }
                }
            })
            .register()?;
        let _output_process = output
            .add_local_listener::<()>()
            .process(realtime(move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let requested = buffer.requested() as usize;
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else {
                    return;
                };
                let mut size = 0;
                if let Some(bytes) = data.data() {
                    size = if requested == 0 {
                        bytes.len() / 8 * 8
                    } else {
                        requested.saturating_mul(8).min(bytes.len()) / 8 * 8
                    };
                    let percent = volume.route_volume.route_percent().unwrap_or(0);
                    out_audio.render(&mut bytes[..size], percent);
                }
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = 8;
                *chunk.size_mut() = size as u32;
            }))
            .register()?;
        let mut format = spa::param::audio::AudioInfoRaw::new();
        format.set_format(spa::param::audio::AudioFormat::F32LE);
        format.set_channels(2);
        let mut format_properties: Vec<spa::pod::Property> = format.into();
        // Keep all EQ bands below Nyquist. PipeWire can still convert this
        // stream for a low-rate device such as a hands-free Bluetooth output.
        format_properties.push(spa::pod::Property::new(
            spa::param::format::FormatProperties::AudioRate.as_raw(),
            spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                spa::utils::ChoiceFlags::empty(),
                spa::utils::ChoiceEnum::Range {
                    default: 48_000,
                    min: 44_100,
                    max: 384_000,
                },
            ))),
        ));
        let value = spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: format_properties,
        });
        let bytes = spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &value,
        )?
        .0
        .into_inner();
        let mut params = [spa::pod::Pod::from_bytes(&bytes)
            .ok_or_else(|| anyhow::anyhow!("Invalid audio format"))?];
        // RT_PROCESS runs both `process` callbacks on PipeWire's realtime data
        // thread, in the graph's own time. No node.latency is set: it could
        // only slow the whole graph for everyone, the lowest request wins,
        // and it adds to the delay that lyrics and the clock must follow.
        let flags = pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::INACTIVE
            | pw::stream::StreamFlags::RT_PROCESS;
        input.connect(spa::utils::Direction::Input, None, flags, &mut params)?;
        output.connect(
            spa::utils::Direction::Output,
            None,
            flags | pw::stream::StreamFlags::AUTOCONNECT,
            &mut params,
        )?;
        let done = mainloop.clone();
        let was_playing = Cell::new(false);
        let active_input = input.clone();
        let active_output = output.clone();
        let timer_audio = audio.clone();
        let timer_input = input_id;
        let timer_gate = gate;
        let foreign_since = Cell::new(None);
        let unlinked = RefCell::new(HashSet::new());
        let refused = Cell::new(0u32);
        let reported = Cell::new(Instant::now());
        let timer = mainloop.loop_().add_timer(move |_| {
            if state.stop.load(Ordering::Acquire) || state.failed.load(Ordering::Acquire) {
                done.quit();
                return;
            }
            leaving.borrow_mut().settle();
            state
                .delay_ms
                .store(timer_audio.stats.delay_ms(), Ordering::Relaxed);
            if reported.get().elapsed() >= REPORT_EVERY {
                reported.set(Instant::now());
                if let Some(line) = timer_audio.stats.take().line() {
                    log::warn!("{line}");
                    // The soak has no logger, and reads this beside its breaks.
                    #[cfg(test)]
                    println!("{line}");
                }
            }
            // Linking does not require playback. Show EQ and visualizers as
            // soon as the owned child is routed, including while paused.
            timer_audio.refresh_eq();
            let input_node = active_input.node_id();
            timer_input.set(input_node);
            let child = state.child.load(Ordering::Acquire);
            let mut route = graph.borrow().route(input_node, child);
            if route == Route::Foreign {
                graph.borrow_mut().note_helpers(child);
                route = graph.borrow().route(input_node, child);
            }
            timer_gate(&graph.borrow());
            match route {
                Route::Owned => state.routed.store(true, Ordering::Release),
                Route::Foreign => {
                    // Silent until it goes: only Soloist reaches the tap and
                    // the output. Soloist's own link is left alone.
                    timer_audio.clear();
                    let graph = graph.borrow();
                    for link in graph.foreign_links(input_node, child) {
                        if unlinked.borrow_mut().insert(link) {
                            log::warn!(
                                "Removing a link into Soloist's private input from {}",
                                graph.link_source(link)
                            );
                            if let Err(error) = registry.destroy_global(link).into_result() {
                                log::warn!("PipeWire refused to remove the link: {error}");
                            }
                        }
                    }
                    let since = foreign_since.get().unwrap_or_else(std::time::Instant::now);
                    foreign_since.set(Some(since));
                    if since.elapsed() >= FOREIGN_LIMIT {
                        state.fail(format!(
                            "a link from {} would not go away",
                            graph
                                .foreign_links(input_node, child)
                                .first()
                                .map_or("another node", |link| graph.link_source(*link))
                        ));
                        done.quit();
                        return;
                    }
                }
                Route::Waiting => {}
            }
            if route != Route::Foreign {
                foreign_since.set(None);
            }
            let playing = state.playing.load(Ordering::Acquire);
            if was_playing.get() != playing {
                if !playing {
                    timer_audio.clear();
                }
                if active_input.set_active(playing).is_ok()
                    && active_output.set_active(playing).is_ok()
                {
                    was_playing.set(playing);
                    refused.set(0);
                } else if refused.replace(refused.get() + 1) >= ACTIVATE_TRIES {
                    state.fail("the streams would not change state");
                    done.quit();
                }
            }
        });
        timer
            .update_timer(
                Some(Duration::from_millis(50)),
                Some(Duration::from_millis(50)),
            )
            .into_result()?;
        mainloop.run();
        // Drop listeners before their proxies. PipeWire takes a process
        // listener off the data thread before its drop frees the callback
        // (hook_removed in stream.c). The graph stores no audio.
        audio.clear();
        Ok(())
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        fn tone(rate: u32, frames: usize) -> Vec<u8> {
            (0..frames)
                .flat_map(|i| {
                    let sample = (0.1
                        * (std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(rate)).sin())
                        as f32;
                    [sample, sample].into_iter().flat_map(f32::to_le_bytes)
                })
                .collect()
        }
        fn peak(bytes: &[u8]) -> f32 {
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b).abs())
                .fold(0.0, f32::max)
        }

        /// Renders `output` as a graph would, a short quantum at a time.
        fn render_all(audio: &Audio, output: &mut [u8], percent: u8) {
            for cycle in output.chunks_mut(256 * 8) {
                audio.render(cycle, percent);
            }
        }

        fn owned(audio: Audio) -> Audio {
            audio.owned.store(true, Ordering::Release);
            audio
        }

        #[test]
        fn only_a_hangup_loses_the_connection() {
            assert!(connection_lost(0, -32));
            // "unknown resource 28 op:7": sent on the core, with the
            // connection perfectly healthy.
            assert!(!connection_lost(0, -2));
            assert!(!connection_lost(28, -32));
            assert!(!connection_lost(28, -2));
        }

        #[test]
        fn a_leaving_proxy_is_kept_until_the_server_has_removed_it() {
            let proxy = Rc::new(());
            let gone = Rc::new(Cell::new(false));
            let mut leaving = Leaving::default();
            leaving.add(gone.clone(), proxy.clone());
            leaving.settle();
            assert_eq!(Rc::strong_count(&proxy), 2, "the server still has it");
            gone.set(true);
            leaving.settle();
            assert_eq!(Rc::strong_count(&proxy), 1);
        }

        #[test]
        fn only_the_owned_child_may_feed_the_processing_input() {
            let mut graph = Graph::default();
            graph.clients.insert(1, 100);
            graph.clients.insert(2, 200);
            graph.nodes.insert(3, 1);
            graph.nodes.insert(4, 2);
            assert_eq!(graph.route(10, 100), Route::Waiting);
            graph.links.insert(5, (3, 10));
            assert_eq!(graph.route(10, 100), Route::Owned);
            assert_eq!(graph.route(10, 200), Route::Foreign);
            graph.links.insert(6, (4, 10));
            assert_eq!(
                graph.route(10, 100),
                Route::Foreign,
                "a mixed input cannot be processed"
            );
            assert_eq!(graph.foreign_links(10, 100), vec![6]);
            graph.links.remove(&6);
            graph.clients.remove(&1);
            assert_eq!(
                graph.route(10, 100),
                Route::Waiting,
                "client removal revokes ownership"
            );
        }

        /// Soloist starts a second process of its own program, in another
        /// session. By PID alone it looked like someone else's audio.
        #[test]
        fn soloist_s_own_helper_process_is_not_foreign() {
            let mut graph = Graph::default();
            graph.clients.insert(1, 100);
            graph.clients.insert(2, 101);
            graph.nodes.insert(3, 1);
            graph.nodes.insert(4, 2);
            graph.links.insert(5, (3, 10));
            graph.links.insert(6, (4, 10));
            assert_eq!(graph.route(10, 100), Route::Foreign);
            graph.helpers.insert(101);
            assert_eq!(graph.route(10, 100), Route::Owned);
            assert!(graph.foreign_links(10, 100).is_empty());
        }

        /// This test's own process runs one program, so another process of
        /// it counts and an unrelated one does not.
        #[test]
        fn helpers_are_told_apart_by_their_executable() {
            let start = |program: &str| {
                let child = std::process::Command::new(program)
                    .arg("30")
                    .spawn()
                    .unwrap();
                // A new process runs this test's executable until its exec.
                let ours = std::env::current_exe().unwrap();
                let exe = format!("/proc/{}/exe", child.id());
                while std::fs::read_link(&exe).is_ok_and(|exe| exe == ours) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                child
            };
            let mut child = start("sleep");
            let mut same = start("sleep");
            let mut graph = Graph::default();
            graph.clients.insert(1, same.id());
            // Nix's sleep and tail are one program, so compare with this one.
            graph.clients.insert(2, std::process::id());
            graph.note_helpers(child.id());
            let found = (
                graph.helpers.contains(&same.id()),
                graph.helpers.contains(&std::process::id()),
            );
            for process in [&mut child, &mut same] {
                let _ = process.kill();
                let _ = process.wait();
            }
            assert_eq!(found, (true, false));
        }

        #[test]
        fn a_foreign_source_is_named_for_the_log() {
            let mut graph = Graph::default();
            graph.clients.insert(2, 200);
            graph.nodes.insert(4, 2);
            graph.names.insert(4, "other-app".into());
            graph.links.insert(6, (4, 10));
            assert_eq!(graph.link_source(6), "other-app");
            assert_eq!(graph.link_source(7), "an unnamed node");
        }

        #[test]
        fn equalizer_shapes_the_tap_before_volume_at_native_rates() {
            for rate in [44_100, 48_000, 96_000, 192_000] {
                let eq = crate::eq::shared();
                {
                    let mut eq = eq.lock().unwrap();
                    eq.on = true;
                    eq.bands_db[4] = 6.0;
                }
                let tap = crate::vis::AudioTap::new();
                let audio = owned(Audio::new(eq, tap.clone()));
                audio.rates(Some(rate), Some(rate));
                // Takes up the flush that drops audio of the old format.
                audio.render(&mut [], 0);
                let input = tone(rate, rate as usize / 10);
                audio.push(&input);
                let mut output = vec![0; input.len()];
                render_all(&audio, &mut output, 0);
                assert_eq!(peak(&output), 0.0, "zero volume must be silent");
                let before = tap.window(512, 0);
                let level = before.iter().copied().map(f32::abs).fold(0.0, f32::max);
                assert!(
                    (level - 0.2).abs() < 0.005,
                    "post-EQ level at {rate} Hz: {level}"
                );
                audio.push(&input);
                render_all(&audio, &mut output, 50);
                let level = peak(&output[output.len() / 2..]);
                assert!(
                    (level - 0.025).abs() < 0.002,
                    "volume curve at {rate} Hz: {level}"
                );
                let after = tap.window(512, 0);
                let level = after.iter().copied().map(f32::abs).fold(0.0, f32::max);
                assert!(
                    (level - 0.2).abs() < 0.005,
                    "volume changed the visualization"
                );
            }
        }

        #[test]
        fn pause_clears_queued_audio_and_nonfinite_input_is_silent() {
            let tap = crate::vis::AudioTap::new();
            let audio = owned(Audio::new(crate::eq::shared(), tap.clone()));
            audio.push(&tone(44_100, 4096));
            audio.clear();
            let mut output = vec![0; 4096 * 8];
            audio.render(&mut output, 100);
            assert_eq!(peak(&output), 0.0);
            assert!(tap.window(512, 0).iter().all(|v| *v == 0.0));
            audio.push(
                &[f32::NAN, f32::INFINITY]
                    .into_iter()
                    .flat_map(f32::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
            audio.render(&mut output, 100);
            assert_eq!(peak(&output), 0.0);
            audio.push(&tone(44_100, 44_100));
            audio.render(&mut output, 100);
            assert_eq!(
                peak(&output),
                0.0,
                "oversized input cannot queue stale music"
            );
            assert_eq!(audio.ring.len(), 0);
        }

        /// A mix is never processed: not a sample, even before the main loop
        /// has reacted to the link.
        #[test]
        fn input_is_dropped_unless_it_is_the_owned_child_alone() {
            let tap = crate::vis::AudioTap::new();
            let audio = Audio::new(crate::eq::shared(), tap.clone());
            audio.push(&tone(44_100, 4096));
            assert_eq!(audio.ring.len(), 0);
            let mut output = vec![0; 4096 * 8];
            audio.render(&mut output, 100);
            assert_eq!(peak(&output), 0.0);
            audio.owned.store(true, Ordering::Release);
            audio.push(&tone(44_100, 4096));
            audio.owned.store(false, Ordering::Release);
            audio.push(&tone(44_100, 16));
            audio.render(&mut output, 100);
            assert_eq!(peak(&output), 0.0, "what came before goes with it");
        }

        /// Each fault the log can name is counted, and a clean run says nothing.
        #[test]
        fn the_report_names_each_fault_and_stays_quiet_without_one() {
            let audio = owned(Audio::new(crate::eq::shared(), crate::vis::AudioTap::new()));
            assert_eq!(audio.stats.take().line(), None);
            audio.push(&tone(44_100, 4096));
            let mut output = vec![0; 4096 * 8];
            render_all(&audio, &mut output, 100);
            assert_eq!(
                audio.stats.take().line(),
                None,
                "no fault, and a margin alone is not one"
            );

            render_all(&audio, &mut output[..256 * 8], 100);
            let report = audio.stats.take();
            assert_eq!(report.short_reads, 1);
            assert!(report.line().unwrap().contains("1 short reads"));
            assert_eq!(report.low_water, 0);
            assert!(report.cycles_without_input >= 1);
            assert_eq!(audio.stats.take().short_reads, 0, "emptied by the report");

            let chain = audio.chain.lock().unwrap();
            audio.render(&mut output, 100);
            drop(chain);
            assert_eq!(audio.stats.take().chain_misses, 1);

            for _ in 0..4 {
                audio.push(&tone(44_100, MAX_RATE / 4));
            }
            assert_eq!(audio.stats.take().overflows, 1);
            audio.render(&mut [], 100);

            audio.owned.store(false, Ordering::Release);
            audio.push(&tone(44_100, 16));
            assert_eq!(audio.stats.take().unowned, 1);
            audio.owned.store(true, Ordering::Release);
            audio.render(&mut [], 100);
            audio.clear();

            audio.push(&tone(44_100, 64));
            audio.render(&mut [], 100);
            audio.push(&tone(44_100, 8820));
            audio.push(&tone(44_100, 8820));
            audio.push(&tone(44_100, 8820));
            audio.push(&tone(44_100, 8820));
            audio.push(&tone(44_100, 8820));
            audio.render(&mut output, 100);
            let report = audio.stats.take();
            assert!(report.skipped_frames >= 8820 * 5, "{report:?}");
        }

        /// Silence between sounds is a piece Soloist didn't deliver. Silence
        /// before the first sound, or a long one, is music.
        #[test]
        fn a_short_silence_inside_the_sound_is_a_gap_in_the_input() {
            let audio = owned(Audio::new(crate::eq::shared(), crate::vis::AudioTap::new()));
            let silence = |frames: usize| vec![0; frames * 8];
            audio.push(&silence(1024));
            audio.push(&tone(44_100, 1024));
            assert_eq!(audio.stats.take().input_gaps, 0, "nothing before it");
            audio.push(&silence(1024));
            audio.push(&tone(44_100, 1024));
            let report = audio.stats.take();
            assert_eq!((report.input_gaps, report.input_gap_frames), (1, 1024));
            assert!(report.line().unwrap().contains("1 input gaps (23 ms)"));
            audio.push(&silence(44_100));
            audio.push(&tone(44_100, 1024));
            assert_eq!(audio.stats.take().input_gaps, 0, "a pause in the song");
        }

        /// A volume change sits after the queue, so none of the margin
        /// delays it: the cycle in progress, then the fader's ramp.
        #[test]
        fn a_volume_change_lands_within_two_cycles_of_a_common_quantum() {
            let audio = fresh();
            let quantum = 1024;
            let mut output = vec![0; quantum * 8];
            for _ in 0..12 {
                audio.push(&tone(44_100, quantum));
                audio.render(&mut output, 100);
            }
            let full = peak(&output);
            audio.push(&tone(44_100, quantum));
            audio.render(&mut output, 50);
            audio.push(&tone(44_100, quantum));
            audio.render(&mut output, 50);
            let level = peak(&output[output.len() / 2..]);
            assert!((level - full * 0.125).abs() < 0.002, "{full} to {level}");
        }

        /// Takes a cycle of `frames` of a steady tone through the input and
        /// the output, in the order given. The output's first sample of each
        /// cycle is what the tests below look at, so its peak shows gaps.
        fn cycle(audio: &Audio, frames: usize, input_first: bool) -> Vec<u8> {
            let mut output = vec![0; frames * 8];
            if input_first {
                audio.push(&tone(44_100, frames));
            }
            audio.render(&mut output, 100);
            if !input_first {
                audio.push(&tone(44_100, frames));
            }
            output
        }
        fn fresh() -> Audio {
            owned(Audio::new(crate::eq::shared(), crate::vis::AudioTap::new()))
        }
        /// The frames of `MIN_TARGET` at the tests' 44.1 kHz.
        const TARGET: usize = 2646;

        /// Nothing plays until the margin is there, and then it stays.
        #[test]
        fn the_output_holds_until_the_target_fill_then_plays_on_unbroken() {
            let audio = fresh();
            let quantum = 256;
            let mut held = 0;
            for _ in 0..TARGET / quantum + 4 {
                if peak(&cycle(&audio, quantum, true)) > 0.0 {
                    break;
                }
                held += 1;
            }
            // And the limiter's look-ahead, with the ramp's first frames.
            assert!(
                (TARGET / quantum..TARGET / quantum + 3).contains(&held),
                "held for the target and no longer: {held} cycles"
            );
            for _ in 0..500 {
                cycle(&audio, quantum, true);
            }
            assert_eq!(audio.stats.take().short_reads, 0);
        }

        /// A large quantum holds for two of them instead.
        #[test]
        fn the_target_is_two_quanta_when_those_are_more_than_the_minimum() {
            let chain = fresh();
            let chain = chain.chain.lock().unwrap();
            assert_eq!(chain.target(256), TARGET);
            assert_eq!(chain.target(1024), TARGET);
            assert_eq!(chain.target(2048), 4096);
            assert_eq!(chain.target(8192), 16_384);
        }

        /// Plays `seconds` with `in_per_cycle` frames arriving for every 1024
        /// that leave, a steady deficit, and says how many times it ran dry.
        fn run_short(audio: &Audio, in_per_cycle: usize, seconds: usize) -> u32 {
            let mut output = vec![0; 1024 * 8];
            let (mut phase, mut dry) = (0u64, 0);
            for _ in 0..seconds * 44_100 / 1024 {
                let block: Vec<u8> = (0..in_per_cycle)
                    .flat_map(|_| {
                        phase += 1;
                        let sample = (0.1
                            * (std::f64::consts::TAU * 1000.0 * phase as f64 / 44_100.0).sin())
                            as f32;
                        [sample, sample].into_iter().flat_map(f32::to_le_bytes)
                    })
                    .collect();
                audio.push(&block);
                audio.render(&mut output, 100);
                dry += audio.stats.short_reads.swap(0, Ordering::Relaxed);
            }
            dry
        }

        /// A deficit within what the read can drift never reaches silence.
        #[test]
        fn a_deficit_within_the_drift_never_runs_dry() {
            let audio = fresh();
            let quantum_fraction = (1024.0 * (1.0 - DRIFT * 0.8)) as usize;
            assert_eq!(run_short(&audio, quantum_fraction, 60), 0);
        }

        /// Frames lost for good can only be silence: what drifting buys is
        /// the drift. 18 frames short in 1024 is 1.8%, past what it can cover,
        /// so about 1.3% of the time is held, not the whole 1.8%.
        #[test]
        fn a_deficit_past_the_drift_costs_only_the_excess_in_silence() {
            let audio = fresh();
            let seconds = 120;
            run_short(&audio, 1006, seconds);
            let held = audio.stats.take().held_frames as f64 / (seconds * 44_100) as f64;
            assert!((0.010..0.016).contains(&held), "{held}");
        }

        /// A burst past the margin is heard once. The larger target it leaves
        /// behind, filled by drifting, rides out the next one like it.
        #[test]
        fn a_burst_that_ran_dry_leaves_a_margin_for_the_next() {
            let audio = fresh();
            let quantum = 1024;
            let mut output = vec![0; quantum * 8];
            let burst = |audio: &Audio, output: &mut [u8]| {
                for _ in 0..3 {
                    audio.push(&vec![0; quantum * 8]);
                    audio.render(output, 100);
                }
                cycle(audio, quantum, true);
                audio.stats.short_reads.swap(0, Ordering::Relaxed)
            };
            run_short(&audio, quantum, 2);
            assert_eq!(burst(&audio, &mut output), 1, "past the 60 ms margin");
            run_short(&audio, quantum, 20);
            assert_eq!(burst(&audio, &mut output), 0, "and within the grown one");
        }

        /// A hole's cost comes back at no more than the drift, in no silence.
        #[test]
        fn a_spent_margin_comes_back_by_reading_slow() {
            let audio = fresh();
            let quantum = 1024;
            for _ in 0..6 {
                cycle(&audio, quantum, true);
            }
            audio.push(&vec![0; quantum * 8]);
            audio.render(&mut vec![0; quantum * 8], 100);
            let spent = audio.chain.lock().unwrap().samples.len() / 2;
            assert!(spent < TARGET, "{spent}");
            let cycles = (TARGET - spent) as f64 / (DRIFT * quantum as f64) * 1.2;
            for _ in 0..cycles as usize {
                cycle(&audio, quantum, true);
            }
            // Steered before each read, so a quantum of the fill has just gone.
            let chain = audio.chain.lock().unwrap();
            assert!(chain.samples.len() / 2 + quantum >= TARGET);
            assert_eq!(audio.stats.short_reads.load(Ordering::Relaxed), 0);
        }

        /// The read is continuous however it is drifting, or a tone would
        /// tick at the drift's rate: no step beyond the tone's own.
        #[test]
        fn reading_off_rate_keeps_a_tone_smooth_and_true() {
            let mut audio = fresh();
            let chain = audio.chain.get_mut().unwrap();
            let tone = tone(44_100, 8192);
            chain.samples = tone
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f64::from(f32::from_le_bytes(*bytes)))
                .collect();
            let step = 0.1 * std::f64::consts::TAU * 1000.0 / 44_100.0;
            for wanted in [1.0 - DRIFT, 1.0 + DRIFT] {
                chain.step = wanted;
                chain.wanted_step = wanted;
                let mut last = chain.next_frame()[0];
                for _ in 0..2000 {
                    let next = chain.next_frame()[0];
                    assert!((next - last).abs() < step * 1.05);
                    last = next;
                }
            }
            chain.wanted_step = 1.0;
            for _ in 0..1000 {
                chain.next_frame();
            }
            assert_eq!(
                (chain.step, chain.phase),
                (1.0, 0.0),
                "back to the true rate"
            );
        }

        /// One dropout adds a step, within the bound, and clean playing
        /// eases it back, a little at a time.
        #[test]
        fn the_target_grows_on_dropouts_and_shrinks_slowly_when_clean() {
            let mut audio = fresh();
            let stats = Stats::default();
            let chain = audio.chain.get_mut().unwrap();
            let step = chain.frames_in(TARGET_STEP);
            stats.faults.store(1, Ordering::Relaxed);
            chain.adapt_target(1024, &stats);
            assert_eq!(chain.target(1024), TARGET + step);
            for fault in 2..20 {
                stats.faults.store(fault, Ordering::Relaxed);
                chain.adapt_target(1024, &stats);
            }
            assert_eq!(chain.target(1024), chain.frames_in(MAX_TARGET), "bounded");
            let top = chain.target(1024);
            chain.adapt_target(chain.frames_in(CLEAN_SPELL), &stats);
            assert_eq!(chain.target(1024), top - chain.frames_in(TARGET_EASE));
        }

        /// However large the target, a hold stays short: it is silence.
        #[test]
        fn a_grown_target_does_not_lengthen_the_hold() {
            let audio = fresh();
            audio.chain.lock().unwrap().extra = 8000;
            let quantum = 1024;
            let mut held = 0;
            while peak(&cycle(&audio, quantum, true)) == 0.0 {
                held += 1;
                assert!(held < 8, "held for the floor, not the target");
            }
        }

        /// The position follows what is really queued, target and all.
        #[test]
        fn the_reported_delay_follows_the_queue() {
            let audio = fresh();
            for _ in 0..400 {
                cycle(&audio, 1024, true);
            }
            let settled = audio.stats.delay_ms();
            assert!((60..=90).contains(&settled), "{settled}");
        }

        /// The streams' order inside a cycle isn't fixed: the old queue, with
        /// no margin, lost a quantum whenever it flipped.
        #[test]
        fn input_and_output_swapping_order_within_a_cycle_costs_nothing() {
            let audio = fresh();
            let (mut random, quantum) = (0x9E37_79B9u32, 1024);
            for _ in 0..4 {
                cycle(&audio, quantum, true);
            }
            for _ in 0..2000 {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                cycle(&audio, quantum, random & 1 == 0);
            }
            let report = audio.stats.take();
            assert_eq!(report.short_reads, 0);
            assert_eq!(report.skipped_frames, 0, "and it isn't mistaken for delay");
        }

        /// After running dry it holds for the margin again, and the sound
        /// comes back on the fader's ramp rather than at full level.
        #[test]
        fn running_dry_re_primes_and_ramps_back_in() {
            let audio = fresh();
            let quantum = 1024;
            for _ in 0..8 {
                cycle(&audio, quantum, true);
            }
            let mut output = vec![0; quantum * 8];
            audio.render(&mut output, 100);
            audio.render(&mut output, 100);
            audio.render(&mut output, 100);
            audio.render(&mut output, 100);
            assert_eq!(audio.stats.take().short_reads, 1, "counted once");
            assert_eq!(peak(&output), 0.0, "holding");
            let mut held = 0;
            let output = loop {
                let output = cycle(&audio, quantum, true);
                if peak(&output) > 0.0 {
                    break output;
                }
                held += 1;
            };
            assert_eq!(held, 2);
            let first = f32::from_le_bytes(output[..4].try_into().unwrap());
            assert!(first.abs() < 0.001, "not back at full level: {first}");
        }

        /// The last of the queue before a hole fades rather than stops dead.
        #[test]
        fn the_last_frames_before_running_dry_fade_out() {
            let mut audio = fresh();
            let chain = audio.chain.get_mut().unwrap();
            chain.samples = vec![1.0; 2000].into();
            chain.fade_out(1000);
            let left = |frame: usize| chain.samples[frame * 2];
            assert_eq!(left(0), 1.0, "only the end");
            assert!(left(999) < 0.01, "{}", left(999));
            assert!((800..999).all(|frame| left(frame) >= left(frame + 1)));
        }

        /// Delay left over from a large quantum, or a missed render, goes.
        /// It would otherwise stay for as long as the song does.
        #[test]
        fn delay_beyond_the_target_is_cut_back() {
            let audio = fresh();
            for _ in 0..4 {
                cycle(&audio, 8192, true);
            }
            let held = |audio: &Audio| audio.chain.lock().unwrap().samples.len() / 2;
            assert!(held(&audio) >= 8192, "{}", held(&audio));
            for _ in 0..(2 * 44_100 / 256) {
                cycle(&audio, 256, true);
            }
            let over = 256 + audio.chain.lock().unwrap().frames_in(CUT_OVER);
            assert!(held(&audio) <= TARGET + over, "{}", held(&audio));
            let report = audio.stats.take();
            assert!(report.skipped_frames > 0);
            assert_eq!(report.short_reads, 0);
        }

        /// Cutting the delay doesn't click: a tone steps by no more than it
        /// does anyway, or little over it.
        #[test]
        fn cutting_the_delay_crossfades() {
            let mut chain = fresh();
            let chain = chain.chain.get_mut().unwrap();
            let tone = tone(44_100, 8192);
            chain.samples = tone
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f64::from(f32::from_le_bytes(*bytes)))
                .collect();
            chain.splice(1000);
            assert_eq!(chain.samples.len() / 2, 8192 - 1000);
            let step = 0.1 * std::f64::consts::TAU * 1000.0 / 44_100.0;
            for at in 1..512 {
                let jump = (chain.samples[at * 2] - chain.samples[(at - 1) * 2]).abs();
                assert!(jump < step * 2.0, "at {at}: {jump}");
            }
        }

        /// The cycle a late Soloist missed comes as zeros, with the music
        /// after it. They aren't queued, so the margin plays straight across.
        #[test]
        fn a_cycle_of_zeros_inside_the_sound_is_played_across() {
            let audio = fresh();
            let quantum = 1024;
            let frames = |bytes: &[u8]| bytes.len() / 8;
            for _ in 0..6 {
                cycle(&audio, quantum, true);
            }
            audio.push(&vec![0; quantum * 8]);
            let mut output = vec![0; quantum * 8];
            audio.render(&mut output, 100);
            assert!(
                output
                    .as_chunks::<8>()
                    .0
                    .chunks(45)
                    .all(|period| peak(period.as_flattened()) > 0.05),
                "{} frames with a hole in them",
                frames(&output)
            );
            for _ in 0..20 {
                cycle(&audio, quantum, true);
            }
            let report = audio.stats.take();
            assert_eq!(report.short_reads, 0);
            assert_eq!(report.input_gaps, 1, "and the log still hears of it");
        }

        /// Holes that outlast the margin are heard, and the log must say so.
        #[test]
        fn a_hole_longer_than_the_margin_counts_as_a_short_read() {
            let audio = fresh();
            let quantum = 1024;
            for _ in 0..6 {
                cycle(&audio, quantum, true);
            }
            let mut output = vec![0; quantum * 8];
            for _ in 0..6 {
                audio.push(&vec![0; quantum * 8]);
                audio.render(&mut output, 100);
            }
            cycle(&audio, quantum, true);
            let report = audio.stats.take();
            assert_eq!((report.short_reads, report.input_gaps), (1, 1));
        }

        /// Silence that was the song's after all goes unreported, and puts
        /// the margin back. A dropout here would be a warning per track.
        #[test]
        fn a_long_silence_runs_the_queue_out_quietly_and_restores_the_margin() {
            let audio = fresh();
            let quantum = 1024;
            for _ in 0..6 {
                cycle(&audio, quantum, true);
            }
            let mut output = vec![0; quantum * 8];
            for _ in 0..40 {
                audio.push(&vec![0; quantum * 8]);
                audio.render(&mut output, 100);
            }
            assert_eq!(peak(&output), 0.0);
            for _ in 0..6 {
                cycle(&audio, quantum, true);
            }
            let report = audio.stats.take();
            assert_eq!(report.short_reads, 0);
            assert_eq!(report.input_gaps, 0);
            let chain = audio.chain.lock().unwrap();
            assert!(chain.primed);
            assert!(chain.samples.len() / 2 >= TARGET - quantum);
        }

        /// A chain taken by the main loop costs a block of silence, never a
        /// wait, and the sound is still there afterwards.
        #[test]
        fn a_busy_chain_renders_silence_and_loses_nothing() {
            let audio = owned(Audio::new(crate::eq::shared(), crate::vis::AudioTap::new()));
            audio.push(&tone(44_100, 4096));
            let mut output = vec![1; 1024 * 8];
            let chain = audio.chain.lock().unwrap();
            audio.render(&mut output, 100);
            drop(chain);
            assert!(output.iter().all(|byte| *byte == 0));
            assert_eq!(audio.ring.len(), 4096 * 2);
            audio.render(&mut output, 100);
            assert_eq!(audio.ring.len(), 0);
        }

        /// The main loop rebuilds the equalizer; the callback only swaps it.
        #[test]
        fn an_equalizer_change_arrives_from_the_main_loop() {
            let eq = crate::eq::shared();
            let tap = crate::vis::AudioTap::new();
            let audio = owned(Audio::new(eq.clone(), tap.clone()));
            let level = |audio: &Audio| {
                let input = tone(44_100, 4410);
                let mut output = vec![0; input.len()];
                audio.push(&input);
                render_all(audio, &mut output, 100);
                peak(&output[output.len() / 2..])
            };
            let before = level(&audio);
            {
                let mut eq = eq.lock().unwrap();
                eq.on = true;
                eq.preamp_db = -6.0;
            }
            assert!(
                (level(&audio) - before).abs() < 0.001,
                "not on the audio thread"
            );
            audio.refresh_eq();
            assert!(level(&audio) < before * 0.6);
        }

        #[test]
        fn the_ring_keeps_order_across_its_end_and_refuses_what_won_t_fit() {
            let ring = Ring::new(4);
            let bytes = |values: &[f32]| -> Vec<[u8; 4]> {
                values.iter().map(|value| value.to_le_bytes()).collect()
            };
            let mut out = [0.0; 3];
            assert!(ring.write(&bytes(&[1.0, 2.0, 3.0])));
            assert_eq!(ring.read(&mut out), 3);
            assert!(ring.write(&bytes(&[4.0, f32::NAN, 6.0, 7.0])));
            assert!(!ring.write(&bytes(&[8.0])), "full");
            assert_eq!(ring.read(&mut out), 3);
            assert_eq!(out, [4.0, 0.0, 6.0]);
            ring.skip();
            assert_eq!(ring.read(&mut out), 0);
        }

        /// One writer and one reader on their own threads, as the streams'
        /// callbacks may be.
        #[test]
        fn the_ring_carries_every_sample_between_threads() {
            let ring = Arc::new(Ring::new(64));
            let writer = ring.clone();
            let thread = std::thread::spawn(move || {
                let mut next = 0u32;
                while next < 100_000 {
                    let block: Vec<[u8; 4]> = (next..next + 8)
                        .map(|value| (value as f32).to_le_bytes())
                        .collect();
                    if writer.write(&block) {
                        next += 8;
                    }
                }
            });
            let mut next = 0u32;
            let mut out = [0.0; 16];
            while next < 100_000 {
                let count = ring.read(&mut out);
                for value in &out[..count] {
                    assert_eq!(*value, next as f32);
                    next += 1;
                }
            }
            thread.join().unwrap();
        }

        /// Counts this thread's allocations, while told to.
        struct Counting;
        thread_local! {
            static COUNTING: Cell<bool> = const { Cell::new(false) };
            static COUNT: Cell<usize> = const { Cell::new(0) };
        }
        fn note() {
            let _ = COUNTING.try_with(|counting| {
                if counting.get() {
                    let _ = COUNT.try_with(|count| count.set(count.get() + 1));
                }
            });
        }
        // SAFETY: this forwards every call to the system allocator as it
        // stands, and only adds to a thread-local beside it.
        unsafe impl std::alloc::GlobalAlloc for Counting {
            unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
                note();
                unsafe { std::alloc::System.alloc(layout) }
            }
            unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
                note();
                unsafe { std::alloc::System.alloc_zeroed(layout) }
            }
            unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
                note();
                unsafe { std::alloc::System.dealloc(pointer, layout) }
            }
            unsafe fn realloc(
                &self,
                pointer: *mut u8,
                layout: std::alloc::Layout,
                size: usize,
            ) -> *mut u8 {
                note();
                unsafe { std::alloc::System.realloc(pointer, layout, size) }
            }
        }
        #[global_allocator]
        static ALLOCATOR: Counting = Counting;

        /// The callbacks on an equalized, resampled, flushed stream, at
        /// quanta from a small one to one larger than `CHUNK`.
        #[test]
        fn the_realtime_callbacks_never_allocate() {
            for (input, output) in [
                (44_100, 44_100),
                (48_000, 48_000),
                (44_100, 96_000),
                (192_000, 48_000),
            ] {
                let eq = crate::eq::shared();
                {
                    let mut eq = eq.lock().unwrap();
                    eq.on = true;
                    eq.bands_db[4] = 6.0;
                }
                let audio = owned(Audio::new(eq, crate::vis::AudioTap::new()));
                audio.rates(Some(input), Some(output));
                for quantum in [256, 1024, 8192] {
                    let incoming = tone(input, quantum * input as usize / output as usize);
                    let mut outgoing = vec![0; quantum * 8];
                    COUNT.with(|count| count.set(0));
                    COUNTING.with(|counting| counting.set(true));
                    for cycle in 0..64 {
                        audio.push(&incoming);
                        audio.render(&mut outgoing, 50);
                        if cycle == 32 {
                            audio.flush.store(true, Ordering::Release);
                        }
                    }
                    COUNTING.with(|counting| counting.set(false));
                    assert_eq!(
                        COUNT.with(Cell::get),
                        0,
                        "{input} to {output} Hz at a quantum of {quantum}"
                    );
                }
            }
        }

        /// What the private tests below need: a bridge to the private null
        /// sink, and a stand-in for Soloist playing a tone at it.
        struct Private {
            bridge: Bridge,
            source: tokio::process::Child,
            feed: tokio::task::JoinHandle<()>,
        }
        impl Private {
            async fn open() -> Self {
                use std::process::Stdio;
                use tokio::io::AsyncWriteExt;
                let output =
                    std::env::var("SOLOIST_AUDIO_TEST_OUTPUT").expect("private null sink required");
                assert!(output.starts_with("spotifast_test_"));
                assert!(
                    std::env::var("PIPEWIRE_RUNTIME_DIR").is_ok(),
                    "these tests start and stop clients: private instance only"
                );
                let dirs = crate::paths::AppDirs {
                    config: std::path::PathBuf::new(),
                    state: std::path::PathBuf::new(),
                    cache: std::path::PathBuf::new(),
                };
                let config = crate::app::engine_config(
                    &dirs,
                    &crate::settings::Settings::default(),
                    crate::settings::ProxyConfig::Off,
                    crate::vis::AudioTap::new(),
                    crate::eq::shared(),
                );
                // As in the app, the output finds its own way to the default
                // sink, which here is the only one there is.
                let bridge = Bridge::open_on(config, None, OutputVolume::default())
                    .await
                    .expect("private bridge");
                if std::env::var("SOLOIST_SOAK_SOURCE").is_ok_and(|source| source == "irregular") {
                    return Self::irregular(bridge).await;
                }
                let mut source = tokio::process::Command::new("pw-play")
                    .args([
                        "--target",
                        &bridge.name,
                        "--raw",
                        "--format",
                        "f32",
                        "--rate",
                        "48000",
                        "--channels",
                        "2",
                        // The quantum of the graph being measured.
                        "--latency",
                        "1024",
                        "-",
                    ])
                    .env("PIPEWIRE_PROPS", child_props(""))
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                bridge.child(source.id().unwrap());
                let mut stdin = source.stdin.take().unwrap();
                let feed = tokio::spawn(async move {
                    let tone = tone(48_000, 48_000);
                    while stdin.write_all(&tone).await.is_ok() {}
                });
                bridge.update(&json!({"status":"playing", "volume":100, "is_active":true}));
                let private = Self {
                    bridge,
                    source,
                    feed,
                };
                private.wait_routed().await;
                private
            }
            /// Soloist as `pw-dump` shows it: an async stream at 44.1 kHz on a
            /// main-class loop, which the graph never waits for. It runs in a
            /// process of its own, as this one's module-rt would raise it.
            async fn irregular(bridge: Bridge) -> Self {
                let mut source = tokio::process::Command::new(std::env::current_exe().unwrap());
                source
                    .args([
                        "--ignored",
                        "--exact",
                        "--nocapture",
                        "soloist::audio::pipewire::tests::irregular_source",
                    ])
                    .env("SOLOIST_STAND_IN_TARGET", &bridge.name)
                    .env("PIPEWIRE_PROPS", child_props(""))
                    .env("PIPEWIRE_CONFIG_NAME", "client-nort.conf")
                    .stdout(std::process::Stdio::null())
                    .kill_on_drop(true);
                // As Soloist runs, so the bridge's watcher may make it realtime.
                crate::soloist::priority::bound_realtime(&mut source);
                let source = source.spawn().unwrap();
                bridge.child(source.id().unwrap());
                bridge.update(&json!({"status":"playing", "volume":100, "is_active":true}));
                let private = Self {
                    bridge,
                    source,
                    feed: tokio::spawn(async {}),
                };
                private.wait_routed().await;
                private
            }
            async fn wait_routed(&self) {
                for _ in 0..100 {
                    if self.bridge.routed() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                panic!(
                    "the stand-in never reached the route: {:?}",
                    self.bridge.fault()
                );
            }
            async fn finish(mut self) {
                let _ = self.source.kill().await;
                let _ = self.source.wait().await;
                let _ = self.feed.await;
            }
        }

        /// Every client that left used to risk the route, by way of "unknown
        /// resource N op:7": any pw-cli, any app closing its connection.
        #[tokio::test]
        #[ignore = "requires a private PipeWire with WirePlumber, see SOLOIST_AUDIO_TEST_OUTPUT"]
        async fn clients_leaving_leave_the_route_alone() {
            let private = Private::open().await;
            for _ in 0..50 {
                let status = tokio::process::Command::new("pw-cli")
                    .args(["info", "0"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await
                    .unwrap();
                assert!(status.success());
                tokio::time::sleep(Duration::from_millis(100)).await;
                assert_eq!(private.bridge.fault(), None);
            }
            private.finish().await;
        }

        /// The stream of a running Soloist, whose target name is fixed at
        /// launch, has to come back to a route built under it.
        #[tokio::test]
        #[ignore = "requires a private PipeWire with WirePlumber, see SOLOIST_AUDIO_TEST_OUTPUT"]
        async fn a_running_stream_returns_to_a_rebuilt_route() {
            let private = Private::open().await;
            private.bridge.state.fail("a test wanted a rebuild");
            assert!(private.bridge.rebuild().await, "the route came back up");
            assert!(!private.bridge.routed());
            private.bridge.update(&json!({"status":"playing"}));
            private.wait_routed().await;
            private.finish().await;
        }

        /// Records the private sink for `SOLOIST_SOAK_SECONDS` and reports
        /// every break in the tone. Run it with and without CPU load.
        #[tokio::test]
        #[ignore = "a measurement, requires a private PipeWire with WirePlumber"]
        async fn soak() {
            let seconds: usize =
                std::env::var("SOLOIST_SOAK_SECONDS").map_or(20, |value| value.parse().unwrap());
            let private = Private::open().await;
            tokio::time::sleep(Duration::from_secs(1)).await;
            let frames = seconds * 48_000;
            let recording = tokio::process::Command::new("pw-record")
                .args([
                    "--properties",
                    "{ stream.capture.sink=true }",
                    "--raw",
                    "--format",
                    "f32",
                    "--rate",
                    "48000",
                    "--channels",
                    "2",
                    "--sample-count",
                    &frames.to_string(),
                    "-",
                ])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
                .unwrap()
                .stdout;
            let samples: Vec<f32> = recording
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect();
            let breaks = tone_breaks(&samples);
            println!(
                "soak: {} frames, {} breaks, {} frames broken, longest {}, failed={}",
                samples.len() / 2,
                breaks.len(),
                breaks.iter().sum::<usize>(),
                breaks.iter().max().unwrap_or(&0),
                private.bridge.failed()
            );
            private.finish().await;
        }

        /// The stand-in for Soloist's thread: not a test of its own, the
        /// `Private` above runs it. In one wake in `SOLOIST_STAND_IN_ODDS` it
        /// oversleeps by up to `SOLOIST_STAND_IN_MS`, as a thread of normal
        /// priority does on a busy machine, then carries on where it was.
        #[test]
        #[ignore = "a stand-in for Soloist, started by the soak"]
        fn irregular_source() {
            let Ok(target) = std::env::var("SOLOIST_STAND_IN_TARGET") else {
                return;
            };
            let number = |name: &str, default: u64| {
                std::env::var(name).map_or(default, |value| value.parse().unwrap())
            };
            let odds = number("SOLOIST_STAND_IN_ODDS", 8);
            let most = number("SOLOIST_STAND_IN_MS", 40);
            // One buffer in this many carries half what was asked for.
            let partial = number("SOLOIST_STAND_IN_PARTIAL", 0);
            // With CPU work per 1024 frames, a thread of its own makes the
            // audio, as Soloist's mixer does, and the loop hands PipeWire what
            // it has: half a cycle, when the mixer was starved of CPU.
            let work = Duration::from_micros(number("SOLOIST_STAND_IN_WORK_US", 0));
            // `audio` takes Soloist's thread names, which Spotifast raises;
            // `nice` raises other names to nice -11 itself, as before; any
            // other value leaves them at normal priority.
            let names = std::env::var("SOLOIST_STAND_IN_NAMES").unwrap_or_default();
            let (mixer_name, loop_name) = match names.as_str() {
                "audio" => ("Media Mixer Ren", "pipewire-loop"),
                _ => ("stand-in mixer", "stand-in loop"),
            };
            let nice = names == "nice";
            let raise = move || {
                if nice {
                    // SAFETY: gettid takes nothing and cannot fail.
                    let tid = unsafe { libc::gettid() } as u32;
                    let mut bus = None;
                    crate::soloist::priority::high_priority(&mut bus, std::process::id(), tid)
                        .unwrap();
                }
            };
            let made: Option<Arc<Mutex<VecDeque<f32>>>> = (!work.is_zero()).then(Default::default);
            if let Some(made) = made.clone() {
                std::thread::Builder::new()
                    .name(mixer_name.into())
                    .spawn(move || {
                        raise();
                        let cpu = || {
                            let mut time = libc::timespec {
                                tv_sec: 0,
                                tv_nsec: 0,
                            };
                            // SAFETY: writes the struct above and nothing else.
                            unsafe {
                                libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time)
                            };
                            Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
                        };
                        let mut frame = 0u64;
                        loop {
                            if made.lock().unwrap().len() >= 4096 {
                                std::thread::sleep(Duration::from_millis(2));
                                continue;
                            }
                            let start = cpu();
                            while cpu() - start < work {}
                            let block: Vec<f32> = (0..1024)
                                .map(|_| {
                                    frame += 1;
                                    (0.1 * (std::f64::consts::TAU * 1000.0 * frame as f64
                                        / 44_100.0)
                                        .sin()) as f32
                                })
                                .collect();
                            made.lock().unwrap().extend(block);
                        }
                    })
                    .unwrap();
            }
            std::thread::Builder::new()
                .name(loop_name.into())
                .spawn(move || {
                    raise();
                    run_stand_in(&target, odds, most, partial, made);
                })
                .unwrap()
                .join()
                .unwrap();
        }

        fn run_stand_in(
            target: &str,
            odds: u64,
            most: u64,
            partial: u64,
            made: Option<Arc<Mutex<VecDeque<f32>>>>,
        ) {
            pw::init();
            let mainloop = pw::main_loop::MainLoopRc::new(None).unwrap();
            let context = pw::context::ContextRc::new(&mainloop, None).unwrap();
            let core = context.connect_rc(None).unwrap();
            let stream = pw::stream::StreamRc::new(
                core,
                "stand-in",
                properties! {
                    "media.type" => "Audio", "media.category" => "Playback",
                    "media.role" => "Music", "node.name" => "spotify",
                    "target.object" => target, "node.loop.class" => "main",
                },
            )
            .unwrap();
            let (mut frame, mut random) = (0u64, 0x9E37_79B9_7F4A_7C15u64);
            let _listener = stream
                .add_local_listener::<()>()
                .process(move |stream, _| {
                    random ^= random << 13;
                    random ^= random >> 7;
                    random ^= random << 17;
                    if most > 0 && random % odds == 0 {
                        std::thread::sleep(Duration::from_micros(
                            (random >> 8) % (most * 1000).max(1),
                        ));
                    }
                    let Some(mut buffer) = stream.dequeue_buffer() else {
                        return;
                    };
                    let requested = buffer.requested() as usize;
                    let datas = buffer.datas_mut();
                    let Some(data) = datas.first_mut() else {
                        return;
                    };
                    let mut size = 0;
                    if let Some(bytes) = data.data() {
                        let frames = if requested == 0 {
                            (bytes.len() / 8).min(1024)
                        } else {
                            requested.min(bytes.len() / 8)
                        };
                        let frames = if partial > 0 && (random >> 32) % partial == 0 {
                            frames / 2
                        } else {
                            frames
                        };
                        let mut made = made.as_ref().map(|made| made.lock().unwrap());
                        let frames = made.as_ref().map_or(frames, |made| frames.min(made.len()));
                        size = frames * 8;
                        for out in bytes[..size].as_chunks_mut::<8>().0 {
                            let sample = match &mut made {
                                Some(made) => made.pop_front().unwrap_or(0.0),
                                None => {
                                    (0.1 * (std::f64::consts::TAU * 1000.0 * frame as f64
                                        / 44_100.0)
                                        .sin()) as f32
                                }
                            };
                            frame += 1;
                            out[..4].copy_from_slice(&sample.to_le_bytes());
                            out[4..].copy_from_slice(&sample.to_le_bytes());
                        }
                    }
                    let chunk = data.chunk_mut();
                    *chunk.offset_mut() = 0;
                    *chunk.stride_mut() = 8;
                    *chunk.size_mut() = size as u32;
                })
                .register()
                .unwrap();
            let mut format = spa::param::audio::AudioInfoRaw::new();
            format.set_format(spa::param::audio::AudioFormat::F32LE);
            format.set_channels(2);
            format.set_rate(44_100);
            let value = spa::pod::Value::Object(spa::pod::Object {
                type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
                id: spa::param::ParamType::EnumFormat.as_raw(),
                properties: format.into(),
            });
            let bytes = spa::pod::serialize::PodSerializer::serialize(
                std::io::Cursor::new(Vec::new()),
                &value,
            )
            .unwrap()
            .0
            .into_inner();
            let mut params = [spa::pod::Pod::from_bytes(&bytes).unwrap()];
            stream
                .connect(
                    spa::utils::Direction::Output,
                    None,
                    pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
                    &mut params,
                )
                .unwrap();
            mainloop.run();
        }

        /// Frames in one period of the soak's 1 kHz tone at 48 kHz.
        const PERIOD: usize = 48;

        /// Differences closer than this are one break: a gap of silence
        /// shows at both its edges, a period apart from what is around it.
        const MERGED: usize = 4096;

        /// How long each break in a steady tone lasts, in frames, on the left
        /// channel from where the tone starts. Silence and a jump alike make
        /// a frame differ from the one a period before it.
        fn tone_breaks(interleaved: &[f32]) -> Vec<usize> {
            let left: Vec<f32> = interleaved.iter().step_by(2).copied().collect();
            let Some(start) = left.iter().position(|sample| sample.abs() > 0.05) else {
                return Vec::new();
            };
            // The gain's fade-in from silence is not a break.
            let left = &left[(start + 48_000 / 10).min(left.len())..];
            let mut breaks = Vec::new();
            let mut last: Option<(usize, usize)> = None;
            for frame in PERIOD..left.len() {
                if (left[frame] - left[frame - PERIOD]).abs() < 0.01 {
                    continue;
                }
                last = match last {
                    Some((first, end)) if frame - end <= MERGED => Some((first, frame)),
                    Some((first, end)) => {
                        breaks.push(end + 1 - first);
                        Some((frame, frame))
                    }
                    None => Some((frame, frame)),
                };
            }
            breaks.extend(last.map(|(first, end)| end + 1 - first));
            breaks
        }

        #[test]
        fn a_gap_in_a_tone_counts_once() {
            let mut stereo: Vec<f32> = tone(48_000, 9600)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect();
            assert!(tone_breaks(&stereo).is_empty());
            stereo[12_000..12_256].fill(0.0);
            assert_eq!(tone_breaks(&stereo).len(), 1);
        }

        /// Run against a temporary null sink created by the caller. Captures
        /// only a synthetic tone and leaves the desktop's default unchanged.
        #[tokio::test]
        #[ignore = "requires PipeWire, pw-play, pw-record, pw-dump and SOLOIST_AUDIO_TEST_OUTPUT naming a private null sink"]
        async fn native_route_volume_and_pause() {
            use std::process::Stdio;
            use tokio::io::AsyncWriteExt;
            let output =
                std::env::var("SOLOIST_AUDIO_TEST_OUTPUT").expect("private null sink required");
            assert!(output.starts_with("spotifast_test_"));
            let tap = crate::vis::AudioTap::new();
            let eq = crate::eq::shared();
            {
                let mut eq = eq.lock().unwrap();
                eq.on = true;
                eq.preamp_db = 6.0;
            }
            let dirs = crate::paths::AppDirs {
                config: std::path::PathBuf::new(),
                state: std::path::PathBuf::new(),
                cache: std::path::PathBuf::new(),
            };
            let config = crate::app::engine_config(
                &dirs,
                &crate::settings::Settings::default(),
                crate::settings::ProxyConfig::Off,
                tap.clone(),
                eq,
            );
            // This inactive bridge also exercises ordinary default-output
            // selection, without sending a tone to the desktop's speakers.
            let other = Bridge::open(config.clone(), OutputVolume::default())
                .await
                .expect("second private bridge");
            let bridge = Bridge::open_on(config, Some(output.clone()), OutputVolume::default())
                .await
                .expect("private bridge");
            let graph = tokio::process::Command::new("pw-dump")
                .output()
                .await
                .unwrap();
            let graph: serde_json::Value = serde_json::from_slice(&graph.stdout).unwrap();
            let input = graph
                .as_array()
                .unwrap()
                .iter()
                .find(|node| node["info"]["props"]["node.name"] == bridge.name)
                .expect("private input registered");
            assert_eq!(
                input["info"]["props"]["media.class"], "Audio/Sink",
                "Soloist only selects sink devices, even though pw-play also accepts streams"
            );
            let mut source = tokio::process::Command::new("pw-play")
                .args([
                    "--target",
                    &bridge.name,
                    "--volume",
                    "0",
                    "--raw",
                    "--format",
                    "f32",
                    "--rate",
                    "48000",
                    "--channels",
                    "2",
                    "-",
                ])
                .env("PIPEWIRE_PROPS", "{ channelmix.lock-volumes = true }")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            bridge.child(source.id().unwrap());
            let mut stdin = source.stdin.take().unwrap();
            let feed = tokio::spawn(async move {
                let bytes = tone(48_000, 4800);
                while stdin.write_all(&bytes).await.is_ok() {}
            });
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert!(bridge.routed(), "a paused child must establish its route");
            let graph = tokio::process::Command::new("pw-dump")
                .output()
                .await
                .unwrap();
            let graph: serde_json::Value = serde_json::from_slice(&graph.stdout).unwrap();
            let nodes = graph.as_array().unwrap();
            let other_output = nodes
                .iter()
                .find(|node| node["info"]["props"]["node.name"] == format!("{}.output", other.name))
                .expect("second output registered")["id"]
                .clone();
            let links: Vec<_> = nodes
                .iter()
                .filter(|node| node["info"]["props"]["link.output.node"] == other_output)
                .collect();
            assert!(!links.is_empty(), "default output must connect");
            for link in links {
                let target = nodes
                    .iter()
                    .find(|node| node["id"] == link["info"]["props"]["link.input.node"])
                    .unwrap();
                assert!(
                    !target["info"]["props"]["node.name"]
                        .as_str()
                        .unwrap()
                        .starts_with("spotifast.soloist."),
                    "automatic output selection must never choose a private route"
                );
            }
            bridge.update(&json!({"status":"playing", "volume":0, "is_active":true}));
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert!(!bridge.failed());
            assert!(bridge.routed(), "route must match the owned child's PID");
            assert!(!other.routed());
            assert!(!other.failed(), "private routes must not chain together");
            let tap_peak = tap
                .window(512, 0)
                .iter()
                .copied()
                .map(f32::abs)
                .fold(0.0, f32::max);
            assert!(
                (tap_peak - 0.2).abs() < 0.01,
                "zero-volume post-EQ tap: {tap_peak}"
            );
            async fn capture(output: &str) -> f32 {
                let child = tokio::process::Command::new("pw-record")
                    .args([
                        "--target",
                        output,
                        "--properties",
                        "{ stream.capture.sink=true node.dont-fallback=true }",
                        "--raw",
                        "--format",
                        "f32",
                        "--rate",
                        "44100",
                        "--channels",
                        "2",
                        "--sample-count",
                        "22050",
                        "-",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
                let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
                    .await
                    .unwrap()
                    .unwrap();
                // pw-record exits 1 with empty stderr on reaching its sample
                // limit. Require the entire requested capture, not that code.
                assert_eq!(
                    output.stdout.len(),
                    22_050 * 8,
                    "capture exited {:?}: {}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.stderr.is_empty());
                peak(&output.stdout)
            }
            let silent = capture(&output).await;
            assert_eq!(silent, 0.0, "zero volume must silence the native output");
            bridge.update(&json!({"volume":50}));
            tokio::time::sleep(Duration::from_millis(150)).await;
            let half = capture(&output).await;
            assert!(
                (half - 0.025).abs() < 0.005,
                "processed output at 50%: {half}"
            );
            bridge.update(&json!({"status":"paused"}));
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert!(tap.window(512, 0).iter().all(|v| *v == 0.0));
            bridge.update(&json!({"status":"playing", "volume":100}));
            tokio::time::sleep(Duration::from_millis(250)).await;
            let full = capture(&output).await;
            assert!((full - 0.2).abs() < 0.01, "resumed output: {full}");
            println!(
                "Native private route: zero={silent}, half={half}, full={full}, pre-volume tap={tap_peak}"
            );
            let mut foreign = tokio::process::Command::new("pw-play")
                .args([
                    "--target",
                    &bridge.name,
                    "--raw",
                    "--format",
                    "f32",
                    "--rate",
                    "48000",
                    "--channels",
                    "2",
                    "-",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let mut foreign_input = foreign.stdin.take().unwrap();
            foreign_input.write_all(&tone(48_000, 512)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(
                !bridge.failed(),
                "a foreign source is removed, and costs the route nothing"
            );
            let peak_after = tap
                .window(512, 0)
                .iter()
                .copied()
                .map(f32::abs)
                .fold(0.0, f32::max);
            assert!(peak_after <= 0.25, "nothing foreign was mixed in");
            let _ = foreign.kill().await;
            foreign.wait().await.unwrap();
            source.kill().await.unwrap();
            source.wait().await.unwrap();
            feed.await.unwrap();
            let state = bridge.state.clone();
            drop(bridge);
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert!(state.stop.load(Ordering::Acquire));
            assert!(tap.window(512, 0).iter().all(|v| *v == 0.0));
        }

        /// A Soloist binary linked against libpulse (`pipewire-pulse`) ignores
        /// `--pipewire-device` and `PIPEWIRE_PROPS`, so it needs `PULSE_SINK`
        /// and `PULSE_PROP` to reach the private processing sink at unity gain.
        #[tokio::test]
        #[ignore = "requires PipeWire, pipewire-pulse, WirePlumber, pacat, pw-dump and SOLOIST_AUDIO_TEST_OUTPUT naming a private null sink"]
        async fn pulse_client_routes_and_locks_volume() {
            use std::process::Stdio;
            use tokio::io::AsyncWriteExt;
            let output =
                std::env::var("SOLOIST_AUDIO_TEST_OUTPUT").expect("private null sink required");
            assert!(output.starts_with("spotifast_test_"));
            assert!(
                std::env::var("PIPEWIRE_RUNTIME_DIR").is_ok(),
                "these tests start and stop clients: private instance only"
            );
            let tap = crate::vis::AudioTap::new();
            let eq = crate::eq::shared();
            {
                let mut eq = eq.lock().unwrap();
                eq.on = true;
                eq.preamp_db = 6.0;
            }
            let dirs = crate::paths::AppDirs {
                config: std::path::PathBuf::new(),
                state: std::path::PathBuf::new(),
                cache: std::path::PathBuf::new(),
            };
            let config = crate::app::engine_config(
                &dirs,
                &crate::settings::Settings::default(),
                crate::settings::ProxyConfig::Off,
                tap.clone(),
                eq,
            );
            let other = Bridge::open(config.clone(), OutputVolume::default())
                .await
                .expect("second private bridge");
            let bridge = Bridge::open_on(config, Some(output), OutputVolume::default())
                .await
                .expect("private bridge");
            // Production waits for the separate Pulse service's registry to catch up.
            assert!(
                pulse_reaches_sink(&bridge.name),
                "pipewire-pulse must expose the bridge sink: {:?}",
                bridge.fault()
            );
            assert_eq!(
                pulse::reaches_sink("spotifast.soloist.absent"),
                PulseReachability::Retry,
                "a reachable server without the sink must remain retryable"
            );

            let mut source = tokio::process::Command::new("pacat")
                .args([
                    "--volume=0",
                    "--raw",
                    "--format=float32le",
                    "--rate=48000",
                    "--channels=2",
                ])
                .env("PIPEWIRE_PROPS", child_props(""))
                .env("PULSE_SINK", &bridge.name)
                .env("PULSE_PROP", pulse_props("media.role=Music"))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let child_pid = source.id().unwrap();
            bridge.child(child_pid);
            let mut stdin = source.stdin.take().unwrap();
            let feed = tokio::spawn(async move {
                let bytes = tone(48_000, 4800);
                while stdin.write_all(&bytes).await.is_ok() {}
            });
            for _ in 0..40 {
                if bridge.routed() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(
                bridge.routed(),
                "PULSE_SINK must route the owned libpulse child"
            );
            assert!(
                !other.routed(),
                "PULSE_SINK must target only its own bridge"
            );

            let dump = tokio::process::Command::new("pw-dump")
                .output()
                .await
                .unwrap();
            let graph: serde_json::Value = serde_json::from_slice(&dump.stdout).unwrap();
            let stream = graph
                .as_array()
                .unwrap()
                .iter()
                .find(|node| {
                    node["info"]["props"]["application.process.id"].as_u64()
                        == Some(u64::from(child_pid))
                        && node["info"]["props"]["media.class"] == "Stream/Output/Audio"
                })
                .expect("pipewire-pulse stream node registered");
            let props = &stream["info"]["props"];
            assert_eq!(props["client.api"], "pipewire-pulse");
            assert_eq!(props["target.object"], bridge.name);
            assert_eq!(props["media.role"], "Music");
            for key in [
                "channelmix.lock-volumes",
                "node.dont-fallback",
                "node.dont-move",
                "node.linger",
            ] {
                assert_eq!(props[key], true, "{key} missing on {props}");
            }

            bridge.update(&json!({"status":"playing", "volume":0, "is_active":true}));
            tokio::time::sleep(Duration::from_millis(800)).await;
            assert!(!bridge.failed());
            assert!(!other.failed());
            let tap_peak = tap
                .window(512, 0)
                .iter()
                .copied()
                .map(f32::abs)
                .fold(0.0, f32::max);
            assert!(
                (tap_peak - 0.2).abs() < 0.02,
                "zero-volume post-EQ tap through pipewire-pulse: {tap_peak}"
            );
            bridge.update(&json!({"status":"paused"}));
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert!(tap.window(512, 0).iter().all(|v| *v == 0.0));

            bridge.state.fail("a test wanted a rebuild");
            assert!(bridge.rebuild().await, "the route came back up");
            assert!(!bridge.routed());
            bridge.update(&json!({"status":"playing", "volume":0, "is_active":true}));
            for _ in 0..100 {
                if bridge.routed() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(
                bridge.routed(),
                "running libpulse child must reconnect to rebuilt route"
            );
            tokio::time::sleep(Duration::from_millis(800)).await;
            assert!(!bridge.failed());
            let rebuilt_peak = tap
                .window(512, 0)
                .iter()
                .copied()
                .map(f32::abs)
                .fold(0.0, f32::max);
            assert!(
                (rebuilt_peak - 0.2).abs() < 0.02,
                "post-EQ tap after rebuilding route under running libpulse child: {rebuilt_peak}"
            );

            let _ = source.kill().await;
            let _ = source.wait().await;
            let _ = feed.await;
        }
    }
}
