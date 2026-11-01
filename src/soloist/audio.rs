//! Private OS audio routing for the managed Soloist process. No system mix is
//! recorded. Linux uses PipeWire; the default native-audio feature adds
//! process capture on macOS and Windows.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
};

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
            playing: AtomicBool::new(false),
            volume: AtomicU8::new(0),
            #[cfg(all(feature = "soloist-native-audio", target_os = "macos"))]
            output_volume: OutputVolume::default(),
        }
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
        };
        let state = bridge.state.clone();
        let name = bridge.name.clone();
        let (ready, receive) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("soloist-audio".into())
            .spawn(move || {
                if pipewire::run(&name, config, state.clone(), ready, output.as_deref()).is_err() {
                    state.failed.store(true, Ordering::Release);
                }
            })
            .ok()?;
        match tokio::time::timeout(std::time::Duration::from_secs(5), receive).await {
            Ok(Ok(())) if !bridge.failed() => Some(bridge),
            _ => None,
        }
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
    #[test]
    fn only_a_confirmed_healthy_mac_output_allows_full_source_volume() {
        let bridge = Bridge {
            #[cfg(target_os = "linux")]
            name: "test".into(),
            state: Arc::new(State::new()),
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
    use pw::{properties::properties, spa};
    use std::{
        cell::{Cell, RefCell},
        collections::{HashMap, VecDeque},
        rc::Rc,
        time::Duration,
    };
    const RATE: usize = 44_100;
    const MAX_SAMPLES: usize = RATE / 2; // A quarter-second of stereo, always bounded.

    #[derive(Default)]
    struct Graph {
        clients: HashMap<u32, u32>,
        nodes: HashMap<u32, u32>,
        links: HashMap<u32, (u32, u32)>,
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
                if *owner != pid {
                    return Route::Foreign;
                }
                any = true;
            }
            if any { Route::Owned } else { Route::Waiting }
        }
    }

    struct Audio {
        eq: crate::eq::Processor,
        shared_eq: crate::eq::SharedEq,
        input_rate: u32,
        output_rate: u32,
        input_resampler: Option<crate::resample::Resampler>,
        tap_resampler: Option<crate::resample::Resampler>,
        tap: Arc<crate::vis::AudioTap>,
        limiter: crate::limiter::Limiter,
        samples: VecDeque<f64>,
        gain: f64,
    }
    impl Audio {
        fn new(eq: crate::eq::SharedEq, tap: Arc<crate::vis::AudioTap>) -> Self {
            Self {
                eq: crate::eq::Processor::new(eq.clone()),
                shared_eq: eq,
                tap,
                input_rate: RATE as u32,
                output_rate: RATE as u32,
                input_resampler: None,
                tap_resampler: None,
                limiter: crate::limiter::Limiter::new(RATE as f64),
                samples: VecDeque::with_capacity(MAX_SAMPLES),
                gain: 0.0,
            }
        }
        fn rates(&mut self, input: Option<u32>, output: Option<u32>) {
            if let Some(rate) = input {
                self.input_rate = rate;
            }
            if let Some(rate) = output {
                self.output_rate = rate;
            }
            self.clear();
            self.eq = crate::eq::Processor::at_rate(self.shared_eq.clone(), self.output_rate);
            self.input_resampler =
                crate::resample::Resampler::new(self.input_rate, self.output_rate, 2);
            self.tap_resampler = crate::resample::Resampler::new(self.output_rate, RATE as u32, 2);
        }
        fn clear(&mut self) {
            self.samples.clear();
            self.eq.reset();
            self.tap.clear();
            self.gain = 0.0;
            self.input_resampler =
                crate::resample::Resampler::new(self.input_rate, self.output_rate, 2);
            self.tap_resampler = crate::resample::Resampler::new(self.output_rate, RATE as u32, 2);
            self.limiter = crate::limiter::Limiter::new(f64::from(self.output_rate));
        }
        fn push(&mut self, bytes: &[u8]) {
            // A missing output must never accumulate a song to play later.
            let count = bytes.len() / 8 * 2;
            if count > self.input_rate as usize / 2 {
                self.clear();
                return;
            }
            let mut samples: Vec<f32> = bytes[..count * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| {
                    let sample = f32::from_le_bytes(*b);
                    if sample.is_finite() { sample } else { 0.0 }
                })
                .collect();
            if let Some(resampler) = &mut self.input_resampler {
                samples = resampler.process(&samples);
            }
            let max = self.output_rate as usize / 2;
            if samples.len() > max {
                self.clear();
                return;
            }
            if self.samples.len() + samples.len() > max {
                self.clear();
            }
            self.samples.extend(samples.into_iter().map(f64::from));
        }
        fn render(&mut self, bytes: &mut [u8], percent: u8) {
            let mut samples: Vec<f64> = (0..bytes.len() / 8 * 2)
                .map(|_| self.samples.pop_front().unwrap_or(0.0))
                .collect();
            self.eq.process(&mut samples);
            if let Some(resampler) = &mut self.tap_resampler {
                let mono_rate =
                    resampler.process(&samples.iter().map(|v| *v as f32).collect::<Vec<_>>());
                self.tap.push(
                    &mono_rate.into_iter().map(f64::from).collect::<Vec<_>>(),
                    1.0,
                );
            } else {
                self.tap.push(&samples, 1.0);
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
            self.limiter.process(&mut samples, 1.0);
            for (out, sample) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(samples) {
                out.copy_from_slice(&(sample as f32).to_le_bytes());
            }
        }
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
            .error(move |id, _, _, _| {
                if id == 0 {
                    core_state.failed.store(true, Ordering::Release);
                    core_loop.quit();
                }
            })
            .register();
        let graph = Rc::new(RefCell::new(Graph::default()));
        let registry = core.get_registry_rc()?;
        let added = graph.clone();
        let removed = graph.clone();
        let weak_registry = registry.downgrade();
        // Client PIDs arrive with ClientInfo, not the registry's minimal props.
        let clients = Rc::new(RefCell::new(HashMap::new()));
        let added_clients = clients.clone();
        let removed_clients = clients;

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
                            let id = global.id;
                            let listener = client
                                .add_listener_local()
                                .info(move |info| {
                                    if let Some(pid) = info
                                        .props()
                                        .and_then(|p| p.get("application.process.id"))
                                        .and_then(|s| s.parse::<u32>().ok())
                                    {
                                        clients_graph.borrow_mut().clients.insert(id, pid);
                                    }
                                })
                                .register();
                            added_clients.borrow_mut().insert(id, (client, listener));
                        }
                    }
                    pw::types::ObjectType::Node => {
                        if let Some(client) = number("client.id") {
                            graph.nodes.insert(global.id, client);
                        }
                    }
                    pw::types::ObjectType::Link => {
                        if let (Some(from), Some(to)) =
                            (number("link.output.node"), number("link.input.node"))
                        {
                            graph.links.insert(global.id, (from, to));
                        }
                    }
                    _ => {}
                }
            })
            .global_remove(move |id| {
                let mut graph = removed.borrow_mut();
                graph.clients.remove(&id);
                graph.nodes.remove(&id);
                graph.links.remove(&id);
                removed_clients.borrow_mut().remove(&id);
            })
            .register();
        let audio = Rc::new(RefCell::new(Audio::new(config.eq, config.tap)));
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
        let in_state = state.clone();
        let in_graph = graph.clone();
        let _input_listener = input
            .add_local_listener::<()>()
            .state_changed(move |_, _, _, new| {
                if matches!(new, pw::stream::StreamState::Error(_)) {
                    input_state.failed.store(true, Ordering::Release);
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
                        in_format.borrow_mut().rates(Some(info.rate()), None);
                    } else {
                        in_fail.failed.store(true, Ordering::Release);
                    }
                }
            })
            .process(move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                match in_graph
                    .borrow()
                    .route(stream.node_id(), in_state.child.load(Ordering::Acquire))
                {
                    Route::Owned => in_state.routed.store(true, Ordering::Release),
                    route => {
                        in_audio.borrow_mut().clear();
                        if route == Route::Foreign {
                            in_state.failed.store(true, Ordering::Release);
                        }
                        return;
                    }
                }
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else {
                    return;
                };
                let offset = data.chunk().offset() as usize;
                let size = data.chunk().size() as usize;
                let Some(bytes) = data.data() else { return };
                if let Some(bytes) = bytes.get(offset..offset.saturating_add(size)) {
                    in_audio.borrow_mut().push(bytes);
                }
            })
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
                if matches!(new, pw::stream::StreamState::Error(_)) {
                    output_state.failed.store(true, Ordering::Release);
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
                        out_format.borrow_mut().rates(None, Some(info.rate()));
                    } else {
                        out_fail.failed.store(true, Ordering::Release);
                    }
                }
            })
            .process(move |stream, _| {
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
                    out_audio
                        .borrow_mut()
                        .render(&mut bytes[..size], volume.volume.load(Ordering::Relaxed));
                }
                let chunk = data.chunk_mut();
                *chunk.offset_mut() = 0;
                *chunk.stride_mut() = 8;
                *chunk.size_mut() = size as u32;
            })
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
        let flags = pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::INACTIVE;
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
        let timer = mainloop.loop_().add_timer(move |_| {
            if state.stop.load(Ordering::Acquire) || state.failed.load(Ordering::Acquire) {
                done.quit();
                return;
            }
            // Linking does not require playback. Show EQ and visualizers as
            // soon as the owned child is routed, including while paused.
            match graph
                .borrow()
                .route(active_input.node_id(), state.child.load(Ordering::Acquire))
            {
                Route::Owned => state.routed.store(true, Ordering::Release),
                Route::Foreign => {
                    timer_audio.borrow_mut().clear();
                    state.failed.store(true, Ordering::Release);
                    done.quit();
                    return;
                }
                Route::Waiting => {}
            }
            let playing = state.playing.load(Ordering::Acquire);
            if was_playing.replace(playing) != playing {
                if !playing {
                    timer_audio.borrow_mut().clear();
                }
                if active_input.set_active(playing).is_err()
                    || active_output.set_active(playing).is_err()
                {
                    state.failed.store(true, Ordering::Release);
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
        // Drop listeners before their proxies. The graph stores no audio.
        audio.borrow_mut().clear();
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
            graph.links.remove(&6);
            graph.clients.remove(&1);
            assert_eq!(
                graph.route(10, 100),
                Route::Waiting,
                "client removal revokes ownership"
            );
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
                let mut audio = Audio::new(eq, tap.clone());
                audio.rates(Some(rate), Some(rate));
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
            let mut audio = Audio::new(crate::eq::shared(), tap.clone());
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
            assert!(
                audio.samples.is_empty(),
                "oversized input cannot queue stale music"
            );
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
                bridge.failed(),
                "a foreign source must stop the route instead of being mixed in"
            );
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
