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

/// A separate output fader. Spotify's source volume never writes this value.
/// Clones share one control across the UI, child restarts and the audio worker.
#[derive(Clone)]
pub struct OutputVolume(Arc<AtomicU8>);
impl Default for OutputVolume {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(100)))
    }
}
impl OutputVolume {
    pub fn percent(&self) -> u8 {
        self.0.load(Ordering::Relaxed)
    }
    pub fn set(&self, percent: u8) {
        self.0.store(percent.min(100), Ordering::Relaxed);
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
    #[cfg(all(feature = "soloist-native-audio", target_os = "macos"))]
    output_volume: OutputVolume,
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
            #[cfg(all(feature = "soloist-native-audio", target_os = "macos"))]
            output_volume: OutputVolume::default(),
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
            Self::open_on(config, None).await
        }
    }

    #[cfg(target_os = "linux")]
    async fn open_on(config: crate::player::EngineConfig, output: Option<String>) -> Option<Self> {
        let bridge = Self {
            name: format!("spotifast.soloist.{:016x}", rand::random::<u64>()),
            state: Arc::new(State::new()),
            config,
            output,
            worker: Mutex::new(None),
        };
        bridge.start().await.then_some(bridge)
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
    const EPIPE: i32 = 32;
    const RATE: usize = 44_100;
    /// The fastest rate the streams accept. At a quarter-second of stereo
    /// this bounds the queue, and sizes it once and for all.
    const MAX_RATE: usize = 384_000;
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
                samples: VecDeque::with_capacity(MAX_RATE / 2),
                gain: 0.0,
                pulled: vec![0.0; CHUNK],
                converted: Vec::with_capacity(room(input_rate, output_rate)),
                block: vec![0.0; CHUNK],
                narrow: vec![0.0; CHUNK],
                tapped_narrow: Vec::with_capacity(room(output_rate, RATE as u32)),
                tapped: vec![0.0; room(output_rate, RATE as u32)],
            }
        }
        /// Forgets the signal.
        fn reset(&mut self) {
            self.samples.clear();
            self.eq.reset();
            self.tap.try_clear();
            self.gain = 0.0;
            if let Some(resampler) = &mut self.input_resampler {
                resampler.reset();
            }
            if let Some(resampler) = &mut self.tap_resampler {
                resampler.reset();
            }
            self.limiter.reset();
        }
        /// Moves what the input wrote into `samples`, at the output's rate.
        fn pull(&mut self, ring: &Ring) {
            // A missing output must never accumulate a song to play later.
            if ring.len() > self.input_rate as usize / 2 {
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
                if self.samples.len() + self.converted.len() > self.output_rate as usize / 2 {
                    self.reset();
                    continue;
                }
                self.samples
                    .extend(self.converted.iter().copied().map(f64::from));
            }
        }
        /// Fills at most one `CHUNK` of interleaved float frames.
        fn block(&mut self, bytes: &mut [u8], percent: u8) {
            let count = bytes.len() / 8 * 2;
            let samples = &mut self.block[..count];
            for sample in samples.iter_mut() {
                *sample = self.samples.pop_front().unwrap_or(0.0);
            }
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
        /// Only the owned child feeds the input, as the main loop last saw.
        owned: AtomicBool,
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
                ring: Ring::new(MAX_RATE / 2),
                chain: Mutex::new(Chain::new(processor, tap.clone(), rate, rate)),
                flush: AtomicBool::new(false),
                owned: AtomicBool::new(false),
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
            self.tap.clear();
        }
        /// The input's callback.
        fn push(&self, bytes: &[u8]) {
            // A mix is never processed: while anything else is linked in, the
            // input is dropped and the main loop deals with it.
            let samples = bytes.as_chunks::<4>().0;
            let stereo = samples.len() / 2 * 2;
            if !self.owned.load(Ordering::Acquire) || !self.ring.write(&samples[..stereo]) {
                self.flush.store(true, Ordering::Release);
            }
        }
        /// The output's callback.
        fn render(&self, bytes: &mut [u8], percent: u8) {
            let Ok(mut chain) = self.chain.try_lock() else {
                // The main loop is swapping the chain in: a format change, or
                // an equalizer change. The ring keeps the sound for next time.
                bytes.fill(0);
                return;
            };
            if self.flush.swap(false, Ordering::AcqRel) {
                self.ring.skip();
                chain.reset();
            }
            chain.pull(&self.ring);
            for block in bytes.chunks_mut(CHUNK * 4) {
                chain.block(block, percent);
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
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else {
                    return;
                };
                let offset = data.chunk().offset() as usize;
                let size = data.chunk().size() as usize;
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
                    out_audio.render(&mut bytes[..size], volume.volume.load(Ordering::Relaxed));
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
        let timer = mainloop.loop_().add_timer(move |_| {
            if state.stop.load(Ordering::Acquire) || state.failed.load(Ordering::Acquire) {
                done.quit();
                return;
            }
            leaving.borrow_mut().settle();
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
                audio.render(&mut output, 0);
                assert_eq!(peak(&output), 0.0, "zero volume must be silent");
                let before = tap.window(512, 0);
                let level = before.iter().copied().map(f32::abs).fold(0.0, f32::max);
                assert!(
                    (level - 0.2).abs() < 0.005,
                    "post-EQ level at {rate} Hz: {level}"
                );
                audio.push(&input);
                audio.render(&mut output, 50);
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
                audio.render(&mut output, 100);
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
                let bridge = Bridge::open_on(config, None).await.expect("private bridge");
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
                let source = tokio::process::Command::new(std::env::current_exe().unwrap())
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
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
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
                        size = frames * 8;
                        for out in bytes[..size].as_chunks_mut::<8>().0 {
                            let sample = (0.1
                                * (std::f64::consts::TAU * 1000.0 * frame as f64 / 44_100.0).sin())
                                as f32;
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
            let bridge = Bridge::open_on(config, Some(output.clone()))
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
    }
}
