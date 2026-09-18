//! Audio output for local playback.
//!
//! librespot's rodio sink panics if no output device is available. Release
//! builds abort on that panic. This sink opens the device when playback starts
//! and reports failures through the UI. Spotifast can then remain available
//! as a Connect remote until an output appears.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::VolumeGetter;
use librespot_playback::player::PlayerEvent;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use rodio::Source;

use crate::resample::Resampler;

/// The backend name Settings uses for this sink.
pub const NAME: &str = "rodio";

/// Told about output failures, with a message fit for the interface.
pub type ErrorHook = Arc<dyn Fn(String) + Send + Sync>;

/// Maximum queued rodio chunks before `write` blocks, about 200 ms of audio.
const QUEUE_LIMIT: usize = 12;

/// Maximum time `stop` waits for the queue to drain.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Length of each side of an interrupted-track fade.
const INTERRUPT_FADE: Duration = Duration::from_millis(10);

/// How long Play takes to come up, and Pause to go down.
const TRANSPORT_FADE: Duration = Duration::from_millis(250);

/// How often playback looks at which output the system calls its default.
const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// Default Windows device buffer length in milliseconds.
///
/// Small platform defaults can click under load (#88). A 100 ms buffer avoids
/// these underruns while keeping controls responsive.
pub const DEFAULT_BUFFER_MS: u32 = 100;

/// Allowed Windows device buffer range. Lower values can click; higher values
/// delay playback controls.
pub const BUFFER_MS_RANGE: std::ops::RangeInclusive<u32> = 20..=500;

/// Coordinates an explicit track replacement with the audio thread.
///
/// librespot deliberately leaves a gapless sink running between tracks. That
/// is right when one track reaches its end, but an explicit skip otherwise
/// leaves the old queued audio in front of the replacement. The old signal is
/// faded on rodio's output thread before its queue is discarded; writes stay
/// gated until librespot reports that the replacement track is loaded.
/// A confirmed seek also discards queued audio, without gating packets from
/// the decoder that has already moved to the requested position.
pub struct AudioControl {
    target: Mutex<AudioTarget>,
    waiting_for_track: AtomicBool,
    reset_output: AtomicBool,
    buffer_ms: u32,
}

#[derive(Default, PartialEq, Eq, Debug)]
enum NarrationPhase {
    #[default]
    Idle,
    Fetching,
    Playing,
}

#[derive(Default)]
struct AudioTarget {
    sink: Weak<rodio::Sink>,
    narration_sink: Option<rodio::Sink>,
    envelope: Option<Arc<Envelope>>,
    pending_narration: Option<Vec<u8>>,
    volume: Option<f32>,
    narration_phase: NarrationPhase,
    narration_phase_start: Option<Instant>,
}

impl AudioControl {
    pub fn new(buffer_ms: u32) -> Arc<Self> {
        Arc::new(Self {
            target: Mutex::new(AudioTarget::default()),
            waiting_for_track: AtomicBool::new(false),
            reset_output: AtomicBool::new(false),
            buffer_ms: buffer_ms.clamp(*BUFFER_MS_RANGE.start(), *BUFFER_MS_RANGE.end()),
        })
    }

    /// Follows confirmed decoder transitions, including seeks requested by
    /// another Spotify client. Natural track changes retain gapless audio.
    pub(crate) fn handle_player_event(&self, event: &PlayerEvent) {
        match event {
            PlayerEvent::TrackChanged { .. } => self.track_changed(),
            PlayerEvent::Seeked { .. } => {
                let target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(sink) = target.sink.upgrade() {
                    sink.stop();
                }
                self.reset_output.store(true, Ordering::SeqCst);
                // Previous can rewind the current track after interrupting
                // it. Release that gate, but never close it for a seek:
                // the decoder is already sending audio from the new position.
                self.track_changed();
            }
            PlayerEvent::Stopped { .. } => self.stopped(),
            _ => {}
        }
    }

    /// Fades and discards the current output before a user-requested track
    /// change. Repeated skips share the same handoff.
    pub fn interrupt(&self) {
        if self.waiting_for_track.swap(true, Ordering::SeqCst) {
            return;
        }
        let (sink, envelope) = {
            let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(ref ns) = target.narration_sink {
                ns.stop();
            }
            target.pending_narration = None;
            target.narration_phase = NarrationPhase::Idle;
            target.narration_phase_start = None;
            (target.sink.upgrade(), target.envelope.clone())
        };
        if let (Some(sink), Some(envelope)) = (&sink, &envelope) {
            envelope.fade_out();
            let wait =
                Duration::from_millis(u64::from(self.buffer_ms)).saturating_add(INTERRUPT_FADE * 2);
            let deadline = Instant::now() + wait;
            while !envelope.silent() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            // Unlike `clear`, this does not wait for every queued source.
            // The replacement gets a fresh rodio sink on its first write.
            sink.stop();
        }
        self.reset_output.store(true, Ordering::SeqCst);
    }

    /// Signals that a DJ narration clip is being fetched, so music stream can wait.
    pub fn begin_narration(&self) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        target.narration_phase = NarrationPhase::Fetching;
        target.narration_phase_start = Some(Instant::now());
        if let Some(sink) = target.sink.upgrade() {
            sink.pause();
        }
        log::info!(
            "AudioControl: DJ narration requested; pausing music sink and waiting for narration to complete"
        );
    }

    /// Signals that DJ narration finished or failed.
    pub fn finish_narration(&self) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        target.narration_phase = NarrationPhase::Idle;
        target.narration_phase_start = None;
        if let Some(sink) = target.sink.upgrade() {
            sink.play();
        }
    }

    /// Checks if DJ narration is currently being fetched or actively playing.
    pub fn is_narration_active(&self) -> bool {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        match target.narration_phase {
            NarrationPhase::Idle => false,
            NarrationPhase::Fetching => {
                if target
                    .narration_phase_start
                    .is_some_and(|t| t.elapsed() > Duration::from_secs(5))
                {
                    log::warn!(
                        "AudioControl: DJ narration fetch timed out; releasing music stream"
                    );
                    target.narration_phase = NarrationPhase::Idle;
                    target.narration_phase_start = None;
                    if let Some(sink) = target.sink.upgrade() {
                        sink.play();
                    }
                    false
                } else {
                    true
                }
            }
            NarrationPhase::Playing => {
                let start_elapsed = target
                    .narration_phase_start
                    .map(|t| t.elapsed())
                    .unwrap_or_default();
                if target.pending_narration.is_some() {
                    true
                } else if let Some(ref sink) = target.narration_sink {
                    if start_elapsed > Duration::from_millis(100) && sink.empty() {
                        log::info!("AudioControl: DJ narration completed; starting music playback");
                        target.narration_phase = NarrationPhase::Idle;
                        target.narration_phase_start = None;
                        if let Some(sink) = target.sink.upgrade() {
                            sink.play();
                        }
                        false
                    } else if start_elapsed > Duration::from_secs(45) {
                        log::warn!("AudioControl: DJ narration playback timed out");
                        target.narration_phase = NarrationPhase::Idle;
                        target.narration_phase_start = None;
                        if let Some(sink) = target.sink.upgrade() {
                            sink.play();
                        }
                        false
                    } else {
                        true
                    }
                } else {
                    false
                }
            }
        }
    }

    /// Plays a synthesized DJ voice clip directly through rodio's sink.
    pub fn play_narration(&self, audio_bytes: Vec<u8>) -> Result<(), anyhow::Error> {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        target.narration_phase = NarrationPhase::Playing;
        target.narration_phase_start = Some(Instant::now());
        if let Some(ref sink) = target.narration_sink {
            let cursor = std::io::Cursor::new(audio_bytes);
            let source = rodio::Decoder::new(cursor)?;
            sink.append(source);
            log::info!("Queued DJ narration into dedicated audio sink");
        } else {
            log::info!("Audio sink not yet active; buffering DJ narration");
            target.pending_narration = Some(audio_bytes);
        }
        Ok(())
    }

    pub fn set_narration_volume(&self, volume: f32) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        target.volume = Some(volume);
        if let Some(ref sink) = target.narration_sink {
            sink.set_volume(volume);
        }
    }

    pub fn pause_narration(&self) {
        let target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(ref sink) = target.narration_sink {
            sink.pause();
        }
    }

    pub fn resume_narration(&self) {
        let target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(ref sink) = target.narration_sink {
            sink.play();
        }
    }

    pub fn stop_narration(&self) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(ref sink) = target.narration_sink {
            sink.stop();
        }
        target.pending_narration = None;
        target.narration_phase = NarrationPhase::Idle;
        target.narration_phase_start = None;
    }

    /// Opens the write gate once librespot has left the old decoder behind.
    pub fn track_changed(&self) {
        self.waiting_for_track.store(false, Ordering::SeqCst);
    }

    /// Releases the gate if the requested replacement stopped instead.
    pub fn stopped(&self) {
        self.waiting_for_track.store(false, Ordering::SeqCst);
    }

    fn waiting_for_track(&self) -> bool {
        self.waiting_for_track.load(Ordering::SeqCst)
    }

    fn take_reset(&self) -> bool {
        self.reset_output.swap(false, Ordering::SeqCst)
    }

    fn register(
        &self,
        sink: &Arc<rodio::Sink>,
        narration_sink: rodio::Sink,
        envelope: Arc<Envelope>,
    ) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        target.sink = Arc::downgrade(sink);
        if let Some(volume) = target.volume {
            narration_sink.set_volume(volume);
        }
        if let Some(pending) = target.pending_narration.take() {
            let cursor = std::io::Cursor::new(pending);
            match rodio::Decoder::new(cursor) {
                Ok(source) => {
                    narration_sink.append(source);
                    log::info!("Flushed buffered DJ narration into newly registered sink");
                }
                Err(e) => {
                    log::warn!("Failed to decode buffered DJ narration: {e}");
                }
            }
        }
        target.narration_sink = Some(narration_sink);
        target.envelope = Some(envelope);
    }
}

/// Frames handed to rodio, and frames it has finished with.
/// The difference is what is still queued.
struct Queued {
    appended: AtomicU64,
    consumed: AtomicU64,
}

impl Queued {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            appended: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
        })
    }

    /// Frames handed over and not yet played.
    fn frames(&self) -> u64 {
        self.appended
            .load(Ordering::Relaxed)
            .saturating_sub(self.consumed.load(Ordering::Relaxed))
    }
}

/// The range a level moves over, as a fixed point fraction of full gain.
const SCALE: u32 = 1 << 24;

/// A sample-clocked gain shared by every chunk in one rodio queue.
struct Envelope {
    level: AtomicU32,
    target: AtomicU32,
    /// How far `level` moves each frame. Set per fade, so a ramp can be cut
    /// to fit the sound that is left to carry it.
    step: AtomicU32,
    /// The step for this envelope's nominal length, and its slowest.
    full_step: u32,
}

impl Envelope {
    /// Resting fully open, for a signal that is already sounding.
    fn open(sample_rate: u32, length: Duration) -> Arc<Self> {
        Self::at(sample_rate, length, SCALE)
    }

    /// Resting closed, and staying there until something raises it.
    fn closed(sample_rate: u32, length: Duration) -> Arc<Self> {
        Self::at(sample_rate, length, 0)
    }

    /// Closed, and already on its way up.
    fn rising(sample_rate: u32, length: Duration) -> Arc<Self> {
        let envelope = Self::closed(sample_rate, length);
        envelope.fade_in();
        envelope
    }

    fn at(sample_rate: u32, length: Duration, level: u32) -> Arc<Self> {
        let full_step = step_over(fade_frames(sample_rate, length));
        Arc::new(Self {
            level: AtomicU32::new(level),
            target: AtomicU32::new(level),
            step: AtomicU32::new(full_step),
            full_step,
        })
    }

    fn fade_in(&self) {
        self.step.store(self.full_step, Ordering::Relaxed);
        self.target.store(SCALE, Ordering::Relaxed);
    }

    fn fade_out(&self) {
        self.step.store(self.full_step, Ordering::Relaxed);
        self.target.store(0, Ordering::Relaxed);
    }

    /// Fades out over `frames` of sound, or the nominal length if that is
    /// shorter.
    fn fade_out_over(&self, frames: u64) {
        let frames = frames.clamp(1, u64::from(u32::MAX)) as u32;
        self.step
            .store(step_over(frames).max(self.full_step), Ordering::Relaxed);
        self.target.store(0, Ordering::Relaxed);
    }

    /// Puts the envelope at silence at once, wherever its ramp had reached.
    /// Callers use this once the sound has stopped and there is no longer
    /// anything for a ramp to ride.
    fn close(&self) {
        self.step.store(self.full_step, Ordering::Relaxed);
        self.target.store(0, Ordering::Relaxed);
        self.level.store(0, Ordering::Relaxed);
    }

    fn silent(&self) -> bool {
        self.level.load(Ordering::Relaxed) == 0
    }

    /// Returns this frame's gain, then moves one frame toward the target.
    fn next_gain(&self) -> f32 {
        let level = self.level.load(Ordering::Relaxed);
        let target = self.target.load(Ordering::Relaxed);
        let step = self.step.load(Ordering::Relaxed);
        let next = match level.cmp(&target) {
            std::cmp::Ordering::Less => level.saturating_add(step).min(target),
            std::cmp::Ordering::Greater => level.saturating_sub(step).max(target),
            std::cmp::Ordering::Equal => level,
        };
        self.level.store(next, Ordering::Relaxed);
        level as f32 / SCALE as f32
    }
}

/// The per-frame movement that crosses the whole range in `frames`.
fn step_over(frames: u32) -> u32 {
    SCALE.div_ceil(frames.max(1)).max(1)
}

fn fade_frames(sample_rate: u32, length: Duration) -> u32 {
    (u64::from(sample_rate) * length.as_millis() as u64 / 1_000).max(1) as u32
}

/// Applies the shared interruption envelope on rodio's output thread, so it
/// can smooth audio that was already queued when the user changes track.
struct TransitionSource {
    inner: rodio::buffer::SamplesBuffer,
    /// Smooths a track the listener replaced part way through.
    interrupt: Arc<Envelope>,
    /// Carries Play and Pause.
    transport: Arc<Envelope>,
    /// The count this chunk's frames belong to.
    queued: Arc<Queued>,
    /// Frames of this chunk not yet handed on.
    remaining: u32,
    channel: usize,
    gain: f32,
}

impl TransitionSource {
    fn new(
        inner: rodio::buffer::SamplesBuffer,
        interrupt: Arc<Envelope>,
        transport: Arc<Envelope>,
        queued: Arc<Queued>,
        frames: u32,
    ) -> Self {
        Self {
            inner,
            interrupt,
            transport,
            queued,
            remaining: frames,
            channel: 0,
            gain: 1.0,
        }
    }
}

impl Drop for TransitionSource {
    /// rodio drops whole sources on `stop`, which every track change does, so
    /// a chunk can end without being played. Settling up here is what stops
    /// the count drifting away from the queue it is meant to describe.
    fn drop(&mut self) {
        self.queued
            .consumed
            .fetch_add(u64::from(self.remaining), Ordering::Relaxed);
    }
}

impl Iterator for TransitionSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        if self.channel == 0 {
            // Both step every frame. They are independent ramps that happen
            // to share a signal, so a skip during a pause rides them at once.
            self.gain = self.interrupt.next_gain() * self.transport.next_gain();
            self.remaining = self.remaining.saturating_sub(1);
            self.queued.consumed.fetch_add(1, Ordering::Relaxed);
        }
        self.channel = (self.channel + 1) % NUM_CHANNELS as usize;
        Some(sample * self.gain)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl Source for TransitionSource {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

/// The buffer to ask the device for, in frames.
///
/// Clamp to the reported range because CoreAudio rejects unsupported sizes.
/// If a device reports no range, request the configured size; `open_stream`
/// can retry without a fixed size.
fn engine_buffer(
    sample_rate: u32,
    ms: u32,
    supported: cpal::SupportedBufferSize,
) -> cpal::BufferSize {
    let ms = ms.clamp(*BUFFER_MS_RANGE.start(), *BUFFER_MS_RANGE.end());
    let frames = (u64::from(sample_rate) * u64::from(ms) / 1000).max(1) as u32;
    match supported {
        cpal::SupportedBufferSize::Range { min, max } if min <= max && max > 0 => {
            cpal::BufferSize::Fixed(frames.clamp(min.max(1), max))
        }
        _ => cpal::BufferSize::Fixed(frames),
    }
}

pub struct RodioSink {
    /// The output device name from Settings; `None` means the default.
    device: Option<String>,
    output: Option<Output>,
    on_error: ErrorHook,
    /// Player volume, applied at output so changes affect queued audio.
    volume: Box<dyn VolumeGetter + Send>,
    applied_volume: f32,
    /// Watches for changes to the default output.
    watch: Option<DefaultWatch>,
    /// How much sound to ask the device to hold, in milliseconds. Taken
    /// when the stream opens, so a change lands with the next restart.
    buffer_ms: u32,
    control: Arc<AudioControl>,
}

struct Output {
    sink: Arc<rodio::Sink>,
    _stream: rodio::OutputStream,
    /// The name of the device the stream was opened on.
    device_name: Option<String>,
    /// Set from the audio thread when the stream dies (device unplugged).
    failed: Arc<AtomicBool>,
    /// The rate the stream runs at, and the converter to it when that is
    /// not Spotify's.
    sample_rate: u32,
    resampler: Option<Resampler>,
    envelope: Arc<Envelope>,
    /// The Play and Pause ramp, kept across track changes so a skip during a
    /// fade does not snap the level back.
    transport: Arc<Envelope>,
    /// How much sound is queued, so Pause can cut its ramp to fit.
    queued: Arc<Queued>,
    /// Whether this track has supplied audio since its last stop.
    fed: bool,
    last_write: Option<Instant>,
}

impl Output {
    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

impl RodioSink {
    pub fn new(
        device: Option<String>,
        on_error: ErrorHook,
        volume: Box<dyn VolumeGetter + Send>,
        buffer_ms: u32,
        control: Arc<AudioControl>,
    ) -> Self {
        Self {
            device,
            output: None,
            on_error,
            volume,
            applied_volume: -1.0,
            watch: None,
            buffer_ms,
            control,
        }
    }

    /// Follows the system default output when no device is selected.
    ///
    /// Windows and macOS need explicit polling. PipeWire and PulseAudio move
    /// streams themselves, while ALSA's answer does not change. Polling runs
    /// off the player thread. `at_once` requests a fresh value at playback
    /// start.
    fn follow_default(&mut self, at_once: bool) {
        if cfg!(target_os = "linux") || self.device.is_some() {
            return;
        }
        let Some(output) = &self.output else {
            return;
        };
        let watch = self.watch.get_or_insert_with(DefaultWatch::start);
        let current = if at_once { watch.ask() } else { watch.name() };
        if current.is_some() && current != output.device_name {
            log::info!(
                "the default audio output is now {}; moving playback to it",
                current.as_deref().unwrap_or("[unknown device]")
            );
            self.output = None;
        }
    }

    fn apply_volume(&mut self) {
        let factor = self.volume.attenuation_factor() as f32;
        if let Some(output) = &self.output
            && factor != self.applied_volume
        {
            output.sink.set_volume(factor);
            self.control.set_narration_volume(factor);
            self.applied_volume = factor;
        }
    }

    /// Opens the output if it is not open, or if it died since.
    fn ensure_open(&mut self) -> SinkResult<()> {
        if self.output.as_ref().is_some_and(Output::failed) {
            log::warn!("the audio output stopped working; reopening it");
            self.output = None;
        }
        if self.output.is_some() {
            return Ok(());
        }
        match open_output(self.device.as_deref(), self.buffer_ms, &self.control) {
            Ok(output) => {
                self.output = Some(output);
                self.applied_volume = -1.0;
                Ok(())
            }
            Err(error) => {
                let message = error.to_string();
                log::error!("{message}");
                (self.on_error)(message.clone());
                Err(SinkError::ConnectionRefused(message))
            }
        }
    }
}

impl Sink for RodioSink {
    fn start(&mut self) -> SinkResult<()> {
        take_precedence();
        self.follow_default(true);
        self.ensure_open()?;
        self.apply_volume();
        if let Some(output) = &mut self.output {
            output.transport.fade_in();
            output.sink.play();
        }
        Ok(())
    }

    /// Never fails: librespot exits the process when a sink cannot stop.
    fn stop(&mut self) -> SinkResult<()> {
        if let Some(output) = &mut self.output {
            // The drain below plays the queue out, so the ramp is cut to
            // what is in it. During steady playback that is the whole
            // 250 ms; just after a seek or a track change it is whatever has
            // been decoded since.
            output.transport.fade_out_over(output.queued.frames());
            let deadline = Instant::now() + DRAIN_TIMEOUT;
            while !output.sink.empty() && !output.failed() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            output.sink.pause();
            output.transport.close();
            output.fed = false;
            output.last_write = None;
        }
        self.control.pause_narration();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|error| SinkError::OnWrite(error.to_string()))?;
        let samples = converter.f64_to_f32(samples);
        if self.control.waiting_for_track() {
            return Ok(());
        }
        self.follow_default(false);
        self.ensure_open()?;
        self.control.resume_narration();
        while self.control.is_narration_active() {
            if self.control.waiting_for_track() {
                return Ok(());
            }
            if self.output.as_ref().is_some_and(|output| output.failed()) {
                let message = "The audio output stopped working".to_string();
                (self.on_error)(message.clone());
                return Err(SinkError::OnWrite(message));
            }
            thread::sleep(Duration::from_millis(15));
        }
        if self.control.take_reset()
            && let Some(output) = &mut self.output
        {
            let sink = Arc::new(rodio::Sink::connect_new(output._stream.mixer()));
            let narration_sink = rodio::Sink::connect_new(output._stream.mixer());
            let envelope = Envelope::rising(output.sample_rate, INTERRUPT_FADE);
            self.control
                .register(&sink, narration_sink, Arc::clone(&envelope));
            output.sink = sink;
            output.envelope = envelope;
            output.queued = Queued::new();
            output.resampler =
                Resampler::new(SAMPLE_RATE, output.sample_rate, NUM_CHANNELS as usize);
            output.fed = false;
            output.last_write = None;
            self.applied_volume = -1.0;
        }
        self.apply_volume();
        let Some(output) = &mut self.output else {
            return Err(SinkError::NotConnected(
                "the audio output is not open".into(),
            ));
        };
        let samples = match &mut output.resampler {
            Some(resampler) => resampler.process(&samples),
            None => samples,
        };
        let now = Instant::now();
        if output.fed && output.sink.empty() && !output.sink.is_paused() {
            let late_ms = output
                .last_write
                .map(|last| now.duration_since(last).as_millis())
                .unwrap_or(0);
            log::warn!("audio queue ran dry; next packet arrived after {late_ms} ms");
        }
        output.transport.fade_in();
        let frames = (samples.len() / NUM_CHANNELS as usize) as u32;
        let source = rodio::buffer::SamplesBuffer::new(
            NUM_CHANNELS as rodio::ChannelCount,
            output.sample_rate as rodio::SampleRate,
            samples,
        );
        output
            .queued
            .appended
            .fetch_add(u64::from(frames), Ordering::Relaxed);
        output.sink.append(TransitionSource::new(
            source,
            Arc::clone(&output.envelope),
            Arc::clone(&output.transport),
            Arc::clone(&output.queued),
            frames,
        ));
        output.fed = true;
        output.last_write = Some(now);
        // Let rodio drain a little; without this the whole track would be
        // decoded into memory at once.
        while output.sink.len() > QUEUE_LIMIT {
            if output.failed() {
                let message = "The audio output stopped working".to_string();
                (self.on_error)(message.clone());
                return Err(SinkError::OnWrite(message));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

/// Opens the stream at Spotify's stereo 44.1 kHz, so nothing is converted,
/// else at the device's own rate, which Windows insists on for a shared
/// device, else at whatever rodio can find.
///
/// The first two attempts request the configured buffer. The fallback lets
/// the driver choose its buffer size.
fn open_stream(
    device: &cpal::Device,
    on_error: impl FnMut(cpal::StreamError) + Send + Clone + 'static,
    buffer_ms: u32,
) -> Result<rodio::OutputStream, rodio::StreamError> {
    let supported = device
        .default_output_config()
        .map(|config| *config.buffer_size())
        .unwrap_or(cpal::SupportedBufferSize::Unknown);
    let builder = |sample_rate: u32, buffer: bool| -> Result<_, rodio::StreamError> {
        let builder = rodio::OutputStreamBuilder::from_device(device.clone())?
            .with_channels(NUM_CHANNELS as rodio::ChannelCount)
            .with_sample_rate(sample_rate as rodio::SampleRate)
            .with_error_callback(on_error.clone());
        Ok(if buffer {
            builder.with_buffer_size(engine_buffer(sample_rate, buffer_ms, supported))
        } else {
            builder
        })
    };
    // The fixed engine buffer addresses Windows shared-mode underruns (#88).
    // CoreAudio, ALSA, PulseAudio, and PipeWire keep their proven
    // driver-selected callback periods.
    let fixed_buffer = cfg!(windows);
    if let Ok(stream) = builder(SAMPLE_RATE, fixed_buffer)?.open_stream() {
        return Ok(stream);
    }
    if let Ok(config) = device.default_output_config()
        && let Ok(stream) = builder(config.sample_rate().0, fixed_buffer)?.open_stream()
    {
        return Ok(stream);
    }
    builder(SAMPLE_RATE, false)?.open_stream_or_fallback()
}

/// Raises the Windows decoder thread one step above normal to prevent queued
/// audio from running out under load (#88).
///
/// Linux requires rtkit; CoreAudio owns its real-time callback on macOS.
#[cfg(windows)]
fn take_precedence() {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    };
    // SAFETY: the current thread's pseudo-handle needs no closing, and the
    // call takes nothing else.
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
}

#[cfg(not(windows))]
fn take_precedence() {}

/// Last default-output name, polled on a worker thread because Windows device
/// enumeration can block. The thread ends when the sink is dropped.
struct DefaultWatch(Arc<Mutex<Option<String>>>);

impl DefaultWatch {
    fn start() -> Self {
        let shared = Arc::new(Mutex::new(None));
        let weak = Arc::downgrade(&shared);
        let watching = thread::Builder::new()
            .name("audio-default-watch".into())
            .spawn(move || {
                while let Some(shared) = weak.upgrade() {
                    let name = default_output_name();
                    *shared.lock().unwrap_or_else(PoisonError::into_inner) = name;
                    drop(shared);
                    thread::sleep(DEFAULT_CHECK_INTERVAL);
                }
            });
        if let Err(error) = watching {
            log::warn!("cannot watch the default audio output: {error}");
        }
        Self(shared)
    }

    /// Last polled name, or `None` before the first poll.
    fn name(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Asks right now, on this thread.
    fn ask(&self) -> Option<String> {
        let name = default_output_name();
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = name.clone();
        name
    }
}

fn default_output_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|device| device.name().ok())
}

#[derive(Debug, thiserror::Error)]
enum OpenError {
    #[error("No audio output device was found. Connect or enable one, then press play again.")]
    NoDevice,
    #[error("Cannot list the audio devices: {0}")]
    Devices(#[from] cpal::DevicesError),
    #[error("Cannot open the audio output: {0}")]
    Stream(#[from] rodio::StreamError),
}

fn open_output(
    preferred: Option<&str>,
    buffer_ms: u32,
    control: &AudioControl,
) -> Result<Output, OpenError> {
    let host = cpal::default_host();
    let device = match preferred.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => {
            let chosen = host
                .output_devices()?
                .find(|device| device.name().is_ok_and(|found| found == name));
            match chosen {
                Some(device) => device,
                None => {
                    log::warn!("audio device {name:?} is not available; using the default");
                    host.default_output_device().ok_or(OpenError::NoDevice)?
                }
            }
        }
        None => host.default_output_device().ok_or(OpenError::NoDevice)?,
    };
    let device_name = device.name().ok();
    log::info!(
        "audio output: {}",
        device_name.as_deref().unwrap_or("[unknown device]")
    );

    let failed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&failed);
    let on_error = move |error: cpal::StreamError| {
        log::error!("audio stream error: {error}");
        flag.store(true, Ordering::Relaxed);
    };
    let mut stream = open_stream(&device, on_error, buffer_ms)?;
    stream.log_on_drop(false);
    let sample_rate = stream.config().sample_rate();
    let resampler = Resampler::new(SAMPLE_RATE, sample_rate, NUM_CHANNELS as usize);
    if resampler.is_some() {
        log::info!(
            "the output runs at {sample_rate} Hz; the music is converted from {SAMPLE_RATE} Hz"
        );
    }
    let sink = Arc::new(rodio::Sink::connect_new(stream.mixer()));
    let narration_sink = rodio::Sink::connect_new(stream.mixer());
    let envelope = Envelope::open(sample_rate, INTERRUPT_FADE);
    // The first Play has silence to come up from instead of a hard edge.
    let transport = Envelope::closed(sample_rate, TRANSPORT_FADE);
    control.register(&sink, narration_sink, Arc::clone(&envelope));
    Ok(Output {
        sink,
        _stream: stream,
        device_name,
        failed,
        sample_rate,
        resampler,
        envelope,
        transport,
        queued: Queued::new(),
        fed: false,
        last_write: None,
    })
}

#[cfg(test)]
mod tests {

    /// Converts the configured buffer duration to device frames.
    #[test]
    fn the_buffer_follows_the_setting_and_the_rate() {
        let unknown = cpal::SupportedBufferSize::Unknown;
        assert_eq!(
            engine_buffer(44_100, 100, unknown),
            cpal::BufferSize::Fixed(4410),
            "a tenth of a second at 44.1 kHz"
        );
        assert_eq!(
            engine_buffer(48_000, 100, unknown),
            cpal::BufferSize::Fixed(4800),
            "the same tenth of a second at 48 kHz"
        );
        assert_eq!(
            engine_buffer(44_100, 20, unknown),
            cpal::BufferSize::Fixed(882)
        );
    }

    /// Clamps the buffer to the device range required by CoreAudio.
    #[test]
    fn a_device_that_states_its_range_is_kept_inside_it() {
        let range = cpal::SupportedBufferSize::Range { min: 64, max: 2048 };
        assert_eq!(
            engine_buffer(44_100, 100, range),
            cpal::BufferSize::Fixed(2048),
            "held down to what the device can take"
        );
        assert_eq!(
            engine_buffer(44_100, 20, range),
            cpal::BufferSize::Fixed(882),
            "and left alone when it fits"
        );
        let tiny = cpal::SupportedBufferSize::Range {
            min: 4096,
            max: 8192,
        };
        assert_eq!(
            engine_buffer(44_100, 20, tiny),
            cpal::BufferSize::Fixed(4096),
            "and brought up to a device that will not go smaller"
        );
    }

    /// Rule: a settings file with a wild number in it still opens a
    /// stream. The range is the range whoever wrote the file thought of.
    #[test]
    fn a_number_from_outside_the_range_is_brought_back_in() {
        let unknown = cpal::SupportedBufferSize::Unknown;
        assert_eq!(
            engine_buffer(44_100, 0, unknown),
            engine_buffer(44_100, *BUFFER_MS_RANGE.start(), unknown)
        );
        assert_eq!(
            engine_buffer(44_100, 100_000, unknown),
            engine_buffer(44_100, *BUFFER_MS_RANGE.end(), unknown)
        );
    }
    use super::*;
    use std::sync::Mutex;

    /// A machine without audio (CI, a PC with nothing plugged in) must get
    /// an error and a message for the interface, never a panic. A machine
    /// with audio opens its default device.
    #[test]
    fn starting_without_a_device_is_an_error_not_a_panic() {
        let reported: Arc<Mutex<Option<String>>> = Arc::default();
        let store = Arc::clone(&reported);
        let mut sink = RodioSink::new(
            Some("no such device".into()),
            Arc::new(move |message| *store.lock().unwrap() = Some(message)),
            Box::new(librespot_playback::mixer::NoOpVolume),
            DEFAULT_BUFFER_MS,
            AudioControl::new(DEFAULT_BUFFER_MS),
        );
        match sink.start() {
            Ok(()) => assert!(reported.lock().unwrap().is_none()),
            Err(SinkError::ConnectionRefused(message)) => {
                assert_eq!(reported.lock().unwrap().as_deref(), Some(message.as_str()));
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
        assert!(sink.stop().is_ok());
    }

    /// A rate that keeps a ramp short enough to step through in a test.
    const RATE: u32 = 1_000;

    /// Ramps that stay out of the way, for tests about something else.
    fn wide_open() -> (Arc<Envelope>, Arc<Envelope>) {
        (
            Envelope::open(RATE, INTERRUPT_FADE),
            Envelope::open(RATE, TRANSPORT_FADE),
        )
    }

    /// A chunk of full scale sound, counted into `queued` the way `write`
    /// counts one, and shaped by the ramps it is handed.
    fn chunk(
        frames: u32,
        interrupt: &Arc<Envelope>,
        transport: &Arc<Envelope>,
        queued: &Arc<Queued>,
    ) -> TransitionSource {
        queued
            .appended
            .fetch_add(u64::from(frames), Ordering::Relaxed);
        TransitionSource::new(
            rodio::buffer::SamplesBuffer::new(
                NUM_CHANNELS.into(),
                RATE,
                vec![1.0; frames as usize * NUM_CHANNELS as usize],
            ),
            Arc::clone(interrupt),
            Arc::clone(transport),
            Arc::clone(queued),
            frames,
        )
    }

    /// The gain each frame comes out at. The sound is full scale, so every
    /// sample is the gain that shaped it.
    fn gains(frames: u32, interrupt: &Arc<Envelope>, transport: &Arc<Envelope>) -> Vec<f32> {
        chunk(frames, interrupt, transport, &Queued::new())
            .step_by(NUM_CHANNELS as usize)
            .collect()
    }

    /// Asserts a ramp still sounds for every one of `frames`, and is silent
    /// on the frame after.
    fn falls_silent_after(envelope: &Envelope, frames: u32) {
        for step in 0..frames {
            assert!(envelope.next_gain() > 0.0, "silent {step} frames early");
        }
        assert_eq!(envelope.next_gain(), 0.0);
        assert!(envelope.silent());
    }

    #[test]
    fn confirmed_seek_discards_the_old_position_without_gating_new_packets() {
        let control = AudioControl::new(DEFAULT_BUFFER_MS);
        let (sink, mut output) = rodio::Sink::new();
        let sink = Arc::new(sink);
        let (interrupt, transport) = wide_open();
        let queued = Queued::new();
        let (narration_sink, _) = rodio::Sink::new();
        control.register(&sink, narration_sink, Arc::clone(&interrupt));
        sink.append(chunk(500, &interrupt, &transport, &queued));
        assert_eq!(output.next(), Some(1.0));

        control.handle_player_event(&PlayerEvent::Seeked {
            play_request_id: 1,
            track_id: librespot_core::SpotifyUri::from_uri("spotify:track:14XWXWv5FoCbFzLksawpEe")
                .unwrap(),
            position_ms: 90_000,
        });

        // Rodio checks stop every 5 ms of output. After that, none of the
        // half-second of sound from before the seek may still play.
        output
            .by_ref()
            .take(20 * NUM_CHANNELS as usize)
            .for_each(drop);
        assert!(output.take(50).all(|sample| sample == 0.0));
        assert_eq!(queued.frames(), 0);
        assert!(!control.waiting_for_track());
        assert!(control.take_reset(), "the next packet gets a fresh queue");
    }

    #[test]
    fn track_changes_and_position_updates_preserve_gapless_queued_audio() {
        use librespot_metadata::audio::item::{AudioItem, UniqueFields};

        let control = AudioControl::new(DEFAULT_BUFFER_MS);
        let (sink, mut output) = rodio::Sink::new();
        let sink = Arc::new(sink);
        let (interrupt, transport) = wide_open();
        let (narration_sink, _) = rodio::Sink::new();
        control.register(&sink, narration_sink, Arc::clone(&interrupt));
        sink.append(chunk(500, &interrupt, &transport, &Queued::new()));
        assert_eq!(output.next(), Some(1.0));
        let track_id =
            librespot_core::SpotifyUri::from_uri("spotify:track:14XWXWv5FoCbFzLksawpEe").unwrap();
        let item = AudioItem {
            track_id: track_id.clone(),
            uri: track_id.to_uri().unwrap(),
            files: Default::default(),
            name: "Next song".into(),
            covers: vec![],
            language: vec![],
            duration_ms: 200_000,
            is_explicit: false,
            availability: Ok(()),
            alternatives: None,
            unique_fields: UniqueFields::Track {
                artists: Default::default(),
                album: "Album".into(),
                album_artists: vec![],
                popularity: 0,
                number: 1,
                disc_number: 1,
            },
        };
        for event in [
            PlayerEvent::TrackChanged {
                audio_item: Box::new(item),
            },
            PlayerEvent::PositionCorrection {
                play_request_id: 1,
                track_id: track_id.clone(),
                position_ms: 100,
            },
            PlayerEvent::PositionChanged {
                play_request_id: 1,
                track_id,
                position_ms: 200,
            },
        ] {
            control.handle_player_event(&event);
            assert!(output.by_ref().take(100).all(|sample| sample == 1.0));
            assert!(!control.take_reset());
        }
    }

    #[test]
    fn a_previous_that_rewinds_releases_the_interrupted_track_gate() {
        let control = AudioControl::new(DEFAULT_BUFFER_MS);
        control.interrupt();
        assert!(control.waiting_for_track());

        control.handle_player_event(&PlayerEvent::Seeked {
            play_request_id: 1,
            track_id: librespot_core::SpotifyUri::from_uri("spotify:track:14XWXWv5FoCbFzLksawpEe")
                .unwrap(),
            position_ms: 0,
        });
        assert!(!control.waiting_for_track());
        assert!(control.take_reset());
    }

    #[test]
    fn an_interrupted_signal_fades_out_and_a_replacement_fades_in() {
        let frames = fade_frames(RATE, INTERRUPT_FADE);
        let (interrupt, transport) = wide_open();
        interrupt.fade_out();

        let faded = gains(frames + 2, &interrupt, &transport);
        assert_eq!(faded[0], 1.0);
        assert_eq!(faded[frames as usize], 0.0);

        let incoming = Envelope::rising(RATE, INTERRUPT_FADE);
        let risen = gains(frames + 2, &incoming, &transport);
        assert_eq!(risen[0], 0.0);
        assert_eq!(risen[frames as usize], 1.0);
    }

    /// One gain per frame rather than per sample, so the two channels of a
    /// frame stay level with each other.
    #[test]
    fn both_channels_of_a_frame_share_a_gain() {
        let (interrupt, transport) = wide_open();
        interrupt.fade_out();

        let played: Vec<_> = chunk(8, &interrupt, &transport, &Queued::new()).collect();
        for pair in played.chunks(NUM_CHANNELS as usize) {
            assert_eq!(pair[0], pair[1]);
        }
    }

    /// A fresh output has played nothing, so the first Play must have silence
    /// to come up from rather than starting already open.
    #[test]
    fn the_first_play_ramps_up_instead_of_starting_open() {
        let transport = Envelope::closed(RATE, TRANSPORT_FADE);
        assert!(transport.silent());
        for _ in 0..fade_frames(RATE, TRANSPORT_FADE) {
            assert_eq!(transport.next_gain(), 0.0);
        }

        transport.fade_in();
        assert_eq!(transport.next_gain(), 0.0);
        assert!(transport.next_gain() > 0.0);
    }

    #[test]
    fn a_pause_reaches_silence_only_after_the_whole_ramp() {
        let transport = Envelope::open(RATE, TRANSPORT_FADE);
        transport.fade_out();
        falls_silent_after(&transport, fade_frames(RATE, TRANSPORT_FADE));
    }

    /// An underrun leaves the pause with no frames to fade through,
    /// so the ramp never moves and the level is still up.
    /// Settling it at the stop is what keeps the next Play coming up
    /// from silence rather than resuming at full gain.
    #[test]
    fn a_pause_with_nothing_queued_still_resumes_from_silence() {
        let transport = Envelope::open(RATE, TRANSPORT_FADE);
        transport.fade_out_over(0);
        // No frame is pulled here, because there is none to pull. That is
        // the underrun, and it leaves the ramp exactly where it started.
        assert!(!transport.silent());

        transport.close();
        assert!(transport.silent());

        transport.fade_in();
        assert_eq!(transport.next_gain(), 0.0);
        assert!(transport.next_gain() > 0.0);
    }

    /// The ramp is clocked by the sound, not by the wall, so it cannot run
    /// past the audio it is shaping however long it is left waiting.
    #[test]
    fn the_fade_advances_with_the_music_not_the_clock() {
        let transport = Envelope::open(RATE, TRANSPORT_FADE);
        transport.fade_out();
        thread::sleep(TRANSPORT_FADE * 2);
        assert_eq!(transport.next_gain(), 1.0);
    }

    /// Skipping during a pause rides both ramps at once, and the shorter one
    /// decides when silence arrives.
    #[test]
    fn a_skip_during_a_pause_carries_both_ramps() {
        let frames = fade_frames(RATE, INTERRUPT_FADE);
        let (interrupt, transport) = wide_open();
        interrupt.fade_out();
        transport.fade_out();

        let faded = gains(frames + 2, &interrupt, &transport);
        assert_eq!(faded[0], 1.0);
        assert_eq!(faded[frames as usize], 0.0);
        assert!(faded.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn a_short_queue_still_gets_a_whole_ramp() {
        let left = 60;
        assert!(left < fade_frames(RATE, TRANSPORT_FADE));

        let transport = Envelope::open(RATE, TRANSPORT_FADE);
        transport.fade_out_over(u64::from(left));
        falls_silent_after(&transport, left);
    }

    #[test]
    fn a_deep_queue_does_not_stretch_the_ramp() {
        let nominal = fade_frames(RATE, TRANSPORT_FADE);
        let transport = Envelope::open(RATE, TRANSPORT_FADE);
        transport.fade_out_over(u64::from(nominal) * 10);
        falls_silent_after(&transport, nominal);
    }

    #[test]
    fn playing_a_chunk_takes_it_out_of_the_count() {
        let queued = Queued::new();
        let (interrupt, transport) = wide_open();

        let played = chunk(40, &interrupt, &transport, &queued);
        assert_eq!(queued.frames(), 40);
        assert_eq!(played.count(), 40 * NUM_CHANNELS as usize);
        assert_eq!(queued.frames(), 0);
    }

    /// rodio discards whole sources on a track change. Their frames never
    /// play, so without settling up on drop the count would keep claiming
    /// sound that no longer exists.
    #[test]
    fn a_discarded_chunk_stops_counting_as_queued() {
        let queued = Queued::new();
        let (interrupt, transport) = wide_open();

        drop(chunk(40, &interrupt, &transport, &queued));
        assert_eq!(queued.frames(), 0);
    }
}
