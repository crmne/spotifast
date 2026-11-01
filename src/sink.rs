//! Audio output for local playback.
//!
//! librespot's rodio sink panics if no output device is available. Release
//! builds abort on that panic. This sink opens the device when playback starts
//! and reports a device it cannot open through the UI, from the first write
//! (see `start`). Spotifast can then remain available as a Connect remote
//! until an output appears.
//!
//! fastframe-audio owns the device stream: it pauses with playback, so a
//! paused player costs no audio work (#636), follows the system's default
//! output and reopens after a failure. rodio's mixer and queue fill it. It
//! is the only output on every platform; on Linux it plays through cpal's
//! default host, PipeWire, then PulseAudio, then ALSA.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use fastframe_audio::{
    Buffer, BufferSize, DeviceId, DeviceInfo, Maintained, OutputOptions, Reason, Render,
};
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::VolumeGetter;
use librespot_playback::player::PlayerEvent;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use rodio::Source;

use crate::resample::Resampler;

/// Told about output failures, with a message fit for the interface.
pub type ErrorHook = Arc<dyn Fn(String) + Send + Sync>;

/// Told, with the saved name, the id of the device an older Spotifast saved
/// by its name, once the output has opened on it, so the setting can hold
/// the id from then on.
pub type DeviceHook = Arc<dyn Fn(String, DeviceId) + Send + Sync>;

/// Reported when the system has no audio output at all. The interface
/// recognises it and shows it in the user's language.
pub const NO_DEVICE: &str =
    "No audio output device was found. Connect or enable one, then press play again.";

/// Opens the output: the saved device, else the default.
type Opener = fn(Option<&str>, u32, &AudioControl) -> Result<Output, OpenError>;

/// How much sound `write` queues before it blocks. Measured in time, not in
/// chunks: librespot's packets hold as little as 4 ms, so a dozen of them
/// could be less than one PipeWire quantum (2048 frames, 43 ms), and every
/// callback would drain the queue.
const QUEUE_AHEAD: Duration = Duration::from_millis(200);

/// How much of a full queue rodio plays before `write` is woken to top it
/// up. librespot's packets hold 4 to 13 ms of sound, so polling every 10 ms
/// (or waking for every packet) kept the decoder thread awake about 100
/// times a second; this wakes it about 30 times.
const REFILL: Duration = Duration::from_millis(20);

/// Longest `write` waits for rodio before it looks at the device again: a
/// stream that has failed or stopped playing finishes no chunk to wake it.
const QUEUE_WAIT: Duration = Duration::from_millis(50);

/// Maximum time `stop` waits for the queue to drain.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Length of each side of an interrupted-track fade.
const INTERRUPT_FADE: Duration = Duration::from_millis(50);

/// How long Play takes to come up, and Pause and Stop to go down.
const TRANSPORT_FADE: Duration = Duration::from_millis(50);

/// Smooths slider and mute changes at the device's sample rate.
const VOLUME_RAMP: Duration = Duration::from_millis(30);

/// The least time between two logs of the device's underruns, so a stretch
/// of them under load leaves a line now and then, not one per gap.
const UNDERRUN_LOG_EVERY: Duration = Duration::from_secs(10);

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
    reset_processing: AtomicBool,
    buffer_ms: u32,
}

#[derive(Default)]
struct AudioTarget {
    sink: Weak<rodio::Sink>,
    envelope: Option<Arc<Envelope>>,
}

impl AudioControl {
    pub fn new(buffer_ms: u32) -> Arc<Self> {
        Arc::new(Self {
            target: Mutex::new(AudioTarget::default()),
            waiting_for_track: AtomicBool::new(false),
            reset_output: AtomicBool::new(false),
            reset_processing: AtomicBool::new(false),
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
                self.reset_processing.store(true, Ordering::SeqCst);
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
            let target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
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
        self.reset_processing.store(true, Ordering::SeqCst);
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

    /// The processing wrapper owns a separate reset from the output queue.
    /// Keep it pending while old decoder packets are still being discarded.
    pub(crate) fn take_processing_reset(&self) -> bool {
        !self.waiting_for_track() && self.reset_processing.swap(false, Ordering::SeqCst)
    }

    fn take_reset(&self) -> bool {
        self.reset_output.swap(false, Ordering::SeqCst)
    }

    fn register(&self, sink: &Arc<rodio::Sink>, envelope: Arc<Envelope>) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        target.sink = Arc::downgrade(sink);
        target.envelope = Some(envelope);
    }
}

/// Frames handed to rodio, and frames it has finished with.
/// The difference is what is still queued.
struct Queued {
    appended: AtomicU64,
    consumed: AtomicU64,
    /// While `write` waits for room, the queued level at or below which it
    /// is woken; zero while nothing waits.
    wake_at: AtomicU64,
}

impl Queued {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            appended: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            wake_at: AtomicU64::new(0),
        })
    }

    /// Frames handed over and not yet played.
    fn frames(&self) -> u64 {
        self.appended
            .load(Ordering::Relaxed)
            .saturating_sub(self.consumed.load(Ordering::Relaxed))
    }

    /// Sleeps the writer until rodio has played `refill` frames of what is
    /// queued, or `QUEUE_WAIT` has passed.
    fn wait_for_room(&self, refill: u64) {
        let wake_at = self.frames().saturating_sub(refill).max(1);
        self.wake_at.store(wake_at, Ordering::Relaxed);
        // Pairs with the fence in `drained`: either the chunk that reaches
        // the level sees it, or this sees that chunk already gone.
        fence(Ordering::SeqCst);
        if self.frames() > wake_at {
            thread::park_timeout(QUEUE_WAIT);
        }
        self.wake_at.store(0, Ordering::Relaxed);
    }

    /// rodio has finished with a chunk: wakes `writer` once the queue has
    /// drained to the level it waits for, and only then.
    fn drained(&self, writer: &thread::Thread) {
        fence(Ordering::SeqCst);
        let wake_at = self.wake_at.load(Ordering::Relaxed);
        if wake_at != 0
            && self.frames() <= wake_at
            && self
                .wake_at
                .compare_exchange(wake_at, 0, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            writer.unpark();
        }
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
    /// The thread that queued this chunk, which may be waiting in `write`
    /// for room.
    writer: thread::Thread,
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
            writer: thread::current(),
        }
    }
}

impl Drop for TransitionSource {
    /// rodio drops whole sources on `stop`, which every track change does, so
    /// a chunk can end without being played. Settling up here is what stops
    /// the count drifting away from the queue it is meant to describe.
    ///
    /// rodio drops a chunk once it has taken it out of the sink's count, so
    /// this is also where a writer waiting for room learns of it.
    fn drop(&mut self) {
        self.queued
            .consumed
            .fetch_add(u64::from(self.remaining), Ordering::Relaxed);
        self.queued.drained(&self.writer);
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

pub struct RodioSink {
    /// The output device from Settings, an id or an older build's name
    /// (see [`Saved`]); `None` means the default.
    device: Option<String>,
    output: Option<Output>,
    on_error: ErrorHook,
    on_device: DeviceHook,
    /// Player volume, applied at output so changes affect queued audio.
    volume: Box<dyn VolumeGetter + Send>,
    applied_volume: f32,
    /// How much sound to ask the device to hold, in milliseconds. Taken
    /// when the stream opens, so a change lands with the next restart.
    buffer_ms: u32,
    control: Arc<AudioControl>,
    open: Opener,
}

struct Output {
    device: fastframe_audio::Output<MixerRender>,
    volume: Arc<AtomicU32>,
    /// Where a mixer made for a new stream format waits for this thread.
    made: MixerSlot,
    mixer: rodio::mixer::Mixer,
    sink: Arc<rodio::Sink>,
    /// The rate the mixer runs at, and the converter to it when that is
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
    /// The gaps the device played, as last logged.
    underruns: Underruns,
    /// The name this output was opened by and the id of the device it found
    /// there, until they are handed to the setting.
    saved_as: Option<(String, DeviceId)>,
}

/// Which of the device's underruns have been logged. "audio queue ran dry"
/// is this app being late to fill the queue; an underrun is the device
/// playing silence because its callback came too late, so a log shows both.
#[derive(Default)]
struct Underruns {
    logged: u64,
    at: Option<Instant>,
}

impl Underruns {
    /// The gaps to log now, out of `count` so far: only when it has grown,
    /// and at most once every `UNDERRUN_LOG_EVERY`, holding the rest for the
    /// next line.
    fn take_new(&mut self, count: u64, now: Instant) -> Option<u64> {
        if count <= self.logged
            || self
                .at
                .is_some_and(|at| now.duration_since(at) < UNDERRUN_LOG_EVERY)
        {
            return None;
        }
        let new = count - self.logged;
        self.logged = count;
        self.at = Some(now);
        Some(new)
    }
}

impl Output {
    fn failed(&self) -> bool {
        self.device.failed()
    }

    /// Plays from `mixer`, made for the format the device now runs at, with
    /// a fresh queue and ramps measured at its rate.
    fn attach(&mut self, (mixer, sample_rate): MadeMixer, control: &AudioControl) {
        let sink = Arc::new(rodio::Sink::connect_new(&mixer));
        let envelope = Envelope::open(sample_rate, INTERRUPT_FADE);
        control.register(&sink, Arc::clone(&envelope));
        self.resampler = converter_to(sample_rate);
        self.mixer = mixer;
        self.sink = sink;
        self.sample_rate = sample_rate;
        self.envelope = envelope;
        // The first sound has silence to come up from instead of a hard edge.
        self.transport = Envelope::closed(sample_rate, TRANSPORT_FADE);
        self.queued = Queued::new();
        self.fed = false;
        self.last_write = None;
    }

    /// Has the device ask for sound, reopening it if it failed, moved to a
    /// new default output, stopped playing, or was let go after a long
    /// pause. Returns whether the queue was replaced, which needs the volume
    /// set again.
    fn run(&mut self, control: &AudioControl) -> Result<bool, OpenError> {
        self.device.resume();
        for error in self.device.take_errors() {
            if error.is_fatal() {
                log::error!("audio stream error: {error}");
            } else {
                log::warn!("audio stream error: {error}");
            }
        }
        match self.device.maintain() {
            Maintained::Reopened {
                device,
                sample_rate,
                reason,
                ..
            } => match reason {
                // A stream can stop asking for sound without reporting an
                // error, and then nothing would ever make room (#731).
                Reason::Stalled => log::warn!(
                    "the audio output stopped playing; opened it again: {device} at {sample_rate} Hz"
                ),
                Reason::Failed | Reason::DefaultChanged | Reason::Resumed => {
                    log::info!("audio output reopened ({reason:?}): {device} at {sample_rate} Hz")
                }
            },
            Maintained::Failed(error) => return Err(error.into()),
            Maintained::Unchanged | Maintained::Released => {}
        }
        let count = self.device.clock().underruns();
        if let Some(new) = self.underruns.take_new(count, Instant::now()) {
            log::warn!("audio device underran {new} times; {count} since the output opened");
        }
        let made = self
            .made
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(mixer) = made else {
            return Ok(false);
        };
        self.attach(mixer, control);
        Ok(true)
    }
}

/// The converter from Spotify's rate to `sample_rate`, when they differ.
fn converter_to(sample_rate: u32) -> Option<Resampler> {
    let resampler = Resampler::new(SAMPLE_RATE, sample_rate, NUM_CHANNELS as usize);
    if resampler.is_some() {
        log::info!(
            "the output runs at {sample_rate} Hz; the music is converted from {SAMPLE_RATE} Hz"
        );
    }
    resampler
}

/// A mixer and the rate it runs at.
type MadeMixer = (rodio::mixer::Mixer, u32);

type MixerSlot = Arc<Mutex<Option<MadeMixer>>>;

/// A linear gain ramp, shared by every channel of an output frame.
#[derive(Default)]
struct VolumeRamp {
    current: f32,
    target: f32,
    remaining: u32,
    sample_rate: u32,
}

impl VolumeRamp {
    fn next_gain(&mut self, target: f32, sample_rate: u32) -> f32 {
        if target != self.target || sample_rate != self.sample_rate {
            self.target = target;
            self.sample_rate = sample_rate;
            self.remaining = fade_frames(sample_rate, VOLUME_RAMP);
        }
        let gain = self.current;
        if self.remaining > 0 {
            self.current += (self.target - self.current) / self.remaining as f32;
            self.remaining -= 1;
            if self.remaining == 0 {
                self.current = self.target;
            }
        }
        gain
    }
}

/// Fills the device from rodio's mixer, or with silence before there is one.
///
/// fastframe-audio configures it on the sink's thread whenever it opens a
/// stream. A stream in the format the mixer already has keeps it, so a
/// reopen on another device carries on from the same sample; another rate or
/// channel count gets a new mixer, which waits in `made` for the sink.
struct MixerRender {
    volume: Arc<AtomicU32>,
    ramp: VolumeRamp,
    source: Option<rodio::mixer::MixerSource>,
    format: (u32, u16),
    made: MixerSlot,
}

impl Render for MixerRender {
    fn configure(&mut self, sample_rate: u32, channels: u16) {
        if self.source.is_some() && self.format == (sample_rate, channels) {
            return;
        }
        let (mixer, source) = rodio::mixer::mixer(
            channels as rodio::ChannelCount,
            sample_rate as rodio::SampleRate,
        );
        self.source = Some(source);
        self.format = (sample_rate, channels);
        *self.made.lock().unwrap_or_else(PoisonError::into_inner) = Some((mixer, sample_rate));
    }

    fn render(&mut self, out: &mut [f32]) {
        match &mut self.source {
            Some(source) => {
                let target = f32::from_bits(self.volume.load(Ordering::Relaxed));
                for frame in out.chunks_mut(usize::from(self.format.1).max(1)) {
                    let gain = self.ramp.next_gain(target, self.format.0);
                    for sample in frame {
                        *sample = source.next().unwrap_or(0.0) * gain;
                    }
                }
            }
            None => out.fill(0.0),
        }
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
            on_device: Arc::new(|_, _| {}),
            volume,
            applied_volume: -1.0,
            buffer_ms,
            control,
            open: open_output,
        }
    }

    /// Tells `hook` the id of a device opened by an old saved name.
    pub fn on_device(mut self, hook: DeviceHook) -> Self {
        self.on_device = hook;
        self
    }

    fn apply_volume(&mut self) {
        let factor = self.volume.attenuation_factor() as f32;
        if let Some(output) = &self.output
            && factor != self.applied_volume
        {
            output.volume.store(factor.to_bits(), Ordering::Relaxed);
            self.applied_volume = factor;
        }
    }

    /// Opens the output if it is not open, and has it ask for sound.
    fn open_if_needed(&mut self) -> Result<(), OpenError> {
        // librespot's player thread decodes and queues every packet; ahead
        // of ordinary work, a busy or swapping machine keeps it waiting less
        // than the queue lasts (#88, #785). Once per thread, then free.
        fastframe_audio::raise_feeder_priority();
        match &mut self.output {
            Some(output) => {
                if output.run(&self.control)? {
                    self.applied_volume = -1.0;
                }
            }
            None => {
                let mut output =
                    (self.open)(self.device.as_deref(), self.buffer_ms, &self.control)?;
                if let Some((name, id)) = output.saved_as.take() {
                    log::info!("the saved audio output {name:?} is now saved by its id");
                    self.device = Some(id.as_str().to_string());
                    (self.on_device)(name, id);
                }
                self.output = Some(output);
                self.applied_volume = -1.0;
            }
        }
        Ok(())
    }

    /// As `open_if_needed`, reporting a failure to the interface.
    fn ensure_open(&mut self) -> SinkResult<()> {
        self.open_if_needed().map_err(|error| {
            let message = error.to_string();
            log::error!("{message}");
            (self.on_error)(message.clone());
            SinkError::ConnectionRefused(message)
        })
    }
}

impl Sink for RodioSink {
    /// Never fails: an output that cannot open is reported by the first
    /// `write` instead (#623).
    ///
    /// librespot starts the sink from inside its playing loop and, when
    /// `start` fails, pauses and then carries on as if it were still
    /// playing. It finds itself paused, calls that an invalid state and
    /// exits the process. A failed `write` pauses too, but at a point
    /// where librespot expects it, so playback stops with a message and
    /// the app stays up as a Connect remote.
    fn start(&mut self) -> SinkResult<()> {
        if let Err(error) = self.open_if_needed() {
            log::debug!("audio output not open at start: {error}");
            return Ok(());
        }
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
            // 50 ms; just after a seek or a track change it is whatever has
            // been decoded since.
            output.transport.fade_out_over(output.queued.frames());
            let deadline = Instant::now() + DRAIN_TIMEOUT;
            while !output.sink.empty() && !output.failed() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            output.sink.pause();
            // With the queue played out, the device can stop asking for
            // sound until Play: a paused app costs no audio work (#636).
            output.device.pause();
            output.transport.close();
            output.fed = false;
            output.last_write = None;
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|error| SinkError::OnWrite(error.to_string()))?;
        if self.control.waiting_for_track() {
            // Muting must not remove decoder backpressure. Otherwise cached
            // audio races to EndOfTrack while Connect is still handling the
            // replacement load, and that old event can skip the chosen song.
            // Pace one discarded packet, then let librespot process commands.
            let frames = samples.len() / NUM_CHANNELS as usize;
            thread::sleep(Duration::from_secs_f64(frames as f64 / SAMPLE_RATE as f64));
            return Ok(());
        }
        let samples = converter.f64_to_f32(samples);
        // Sound arriving without a Play first still has a device to go to.
        self.ensure_open()?;
        if self.control.take_reset()
            && let Some(output) = &mut self.output
        {
            let sink = Arc::new(rodio::Sink::connect_new(&output.mixer));
            let envelope = Envelope::rising(output.sample_rate, INTERRUPT_FADE);
            self.control.register(&sink, Arc::clone(&envelope));
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
        // decoded into memory at once. A full queue sleeps until rodio has
        // played `REFILL` of it, then is topped up in one go.
        let refill = u64::from(output.sample_rate) * REFILL.as_millis() as u64 / 1_000;
        while queue_is_full(output.queued.frames(), output.sample_rate) {
            // Headphones that connect, a new default output, a device that
            // went away or a stream that stopped asking for sound: the
            // output moves to the one to use now and the music carries on.
            // Only an output that will not open stops it.
            match output.run(&self.control) {
                Ok(replaced) if !output.failed() => {
                    if replaced {
                        self.applied_volume = -1.0;
                    }
                }
                Ok(_) | Err(_) => {
                    let message = "The audio output stopped working".to_string();
                    (self.on_error)(message.clone());
                    return Err(SinkError::OnWrite(message));
                }
            }
            if !queue_is_full(output.queued.frames(), output.sample_rate) {
                break;
            }
            output.queued.wait_for_room(refill);
        }
        self.apply_volume();
        Ok(())
    }
}

/// Whether `write` has queued `QUEUE_AHEAD` of sound at `sample_rate` and
/// waits for rodio to play some before taking the next packet.
fn queue_is_full(queued_frames: u64, sample_rate: u32) -> bool {
    queued_frames > u64::from(sample_rate) * QUEUE_AHEAD.as_millis() as u64 / 1_000
}

/// What a saved `audio_device` opens, among the outputs listed now.
///
/// Spotifast saves the id `fastframe_audio::output_devices` gives (on Linux
/// PipeWire's or PulseAudio's node name, the WASAPI endpoint id on Windows,
/// the CoreAudio UID on macOS). Up to 0.13.3 it saved a name from
/// `output_device_names` instead, which on Linux was usually one of ALSA's
/// (`PipeWire Sound Server`, `Default ALSA Output (currently PipeWire Media
/// Server)`, a card's name), or a PulseAudio sink with that backend.
///
/// An id a listed device has opens that device. Otherwise a name one still
/// has opens it by that name, and its id is saved once it has opened there.
/// Anything else plays through the system default, as if nothing had been
/// chosen: no error, and the setting stays as it is until another choice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Saved {
    Default,
    Id(DeviceId),
    Named { name: String, id: DeviceId },
}

impl Saved {
    pub(crate) fn among(saved: Option<&str>, devices: &[DeviceInfo]) -> Self {
        let Some(saved) = saved.map(str::trim).filter(|saved| !saved.is_empty()) else {
            return Self::Default;
        };
        if let Some(device) = devices.iter().find(|device| device.id.as_str() == saved) {
            return Self::Id(device.id.clone());
        }
        match devices.iter().find(|device| device.name == saved) {
            Some(device) => Self::Named {
                name: device.name.clone(),
                id: device.id.clone(),
            },
            None => Self::Default,
        }
    }

    /// Reads the saved device against the outputs there are now. Listing
    /// asks the sound system, so this runs where opening the device does,
    /// never on the UI thread; nothing is listed for the default.
    fn now(saved: Option<&str>) -> Self {
        let Some(saved) = saved.map(str::trim).filter(|saved| !saved.is_empty()) else {
            return Self::Default;
        };
        match fastframe_audio::output_devices() {
            Ok(devices) => {
                let resolved = Self::among(Some(saved), &devices);
                if resolved == Self::Default {
                    log::info!(
                        "the saved audio output {saved:?} is not here; playing through the default"
                    );
                }
                resolved
            }
            // The output falls back to the default itself, with a warning,
            // when the id matches nothing.
            Err(error) => {
                log::warn!("couldn't list audio outputs: {error}");
                Self::Id(DeviceId::new(saved))
            }
        }
    }

    fn device(&self) -> fastframe_audio::Device {
        match self {
            Self::Default => fastframe_audio::Device::Default,
            Self::Id(id) => fastframe_audio::Device::Id(id.clone()),
            Self::Named { name, .. } => fastframe_audio::Device::Named(name.clone()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum OpenError {
    #[error("{NO_DEVICE}")]
    NoDevice,
    #[error("{0}")]
    Device(fastframe_audio::OpenError),
}

impl From<fastframe_audio::OpenError> for OpenError {
    fn from(error: fastframe_audio::OpenError) -> Self {
        match error {
            fastframe_audio::OpenError::NoDevice => Self::NoDevice,
            other => Self::Device(other),
        }
    }
}

/// The stream's name in the system's volume mixer, under the app's.
const MEDIA_NAME: &str = "Spotify playback";

/// Names Spotifast, with the desktop entry's icon, in the system's volume
/// mixer (PipeWire and PulseAudio) for every output opened afterwards. Call
/// once at startup, before any output is listed or opened.
pub fn name_in_mixers() {
    let _ = fastframe_audio::set_app_info(fastframe_audio::AppInfo {
        name: "Spotifast".to_owned(),
        icon: Some("spotifast".to_owned()),
    });
}

/// What the output asks of the device: Spotify's stereo 44.1 kHz first, so
/// nothing is converted, then whatever the device takes. A device that has
/// gone falls back to the default. The fixed buffer addresses Windows
/// shared-mode underruns (#88); CoreAudio, PipeWire and ALSA keep their
/// driver-selected periods, and PulseAudio gets fastframe-audio's 20 ms
/// instead of its own two seconds.
fn output_options(device: fastframe_audio::Device, buffer_ms: u32) -> OutputOptions {
    let buffer_ms = buffer_ms.clamp(*BUFFER_MS_RANGE.start(), *BUFFER_MS_RANGE.end());
    OutputOptions {
        device,
        channels: NUM_CHANNELS as u16,
        sample_rate: Some(SAMPLE_RATE),
        buffer: Buffer::FixedOnWindows(BufferSize::Duration(Duration::from_millis(u64::from(
            buffer_ms,
        )))),
        follow_default: true,
        // Fixed while the stream is open, so not the song title. No role:
        // the sound server would share a role's remembered volume with
        // every other app playing music.
        media_name: Some(MEDIA_NAME.to_owned()),
        ..OutputOptions::default()
    }
}

fn open_output(
    preferred: Option<&str>,
    buffer_ms: u32,
    control: &AudioControl,
) -> Result<Output, OpenError> {
    let made = MixerSlot::default();
    let volume = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let render = MixerRender {
        volume: Arc::clone(&volume),
        ramp: VolumeRamp::default(),
        source: None,
        format: (0, 0),
        made: Arc::clone(&made),
    };
    let saved = Saved::now(preferred);
    let device = fastframe_audio::Output::open(output_options(saved.device(), buffer_ms), render)?;
    log::info!(
        "audio output: {} through {}",
        device.device_name(),
        fastframe_audio::host()
    );
    // A device found by an old saved name, and not the default standing in
    // for it, has its id saved from now on.
    let saved_as = match saved {
        Saved::Named { name, id } if *device.device_id() == id => Some((name, id)),
        _ => None,
    };
    // The open configured the renderer, which made the mixer.
    let (mixer, sample_rate) = made
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .ok_or(OpenError::NoDevice)?;
    let mut output = Output {
        device,
        volume,
        made,
        sink: Arc::new(rodio::Sink::connect_new(&mixer)),
        mixer: mixer.clone(),
        sample_rate,
        resampler: None,
        envelope: Envelope::open(sample_rate, INTERRUPT_FADE),
        transport: Envelope::closed(sample_rate, TRANSPORT_FADE),
        queued: Queued::new(),
        fed: false,
        last_write: None,
        underruns: Underruns::default(),
        saved_as,
    };
    output.attach((mixer, sample_rate), control);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn underruns_are_logged_when_they_grow_at_most_every_ten_seconds() {
        let start = Instant::now();
        let mut underruns = Underruns::default();
        assert_eq!(underruns.take_new(0, start), None);
        assert_eq!(underruns.take_new(2, start), Some(2));
        assert_eq!(underruns.take_new(2, start + Duration::from_secs(30)), None);
        // More within ten seconds of the last line wait for the next one.
        let soon = start + Duration::from_secs(31);
        assert_eq!(underruns.take_new(3, soon), Some(1));
        assert_eq!(underruns.take_new(5, soon + Duration::from_secs(9)), None);
        assert_eq!(
            underruns.take_new(6, soon + UNDERRUN_LOG_EVERY),
            Some(3),
            "the held gaps are counted in the next line"
        );
    }

    #[test]
    fn volume_changes_take_thirty_ms_at_each_output_rate() {
        for rate in [44_100, 48_000, 96_000] {
            let frames = rate * 30 / 1_000;
            let mut ramp = VolumeRamp::default();
            for target in [1.0, 0.25, 0.0, 0.8] {
                let start = ramp.current;
                for frame in 0..frames {
                    let gain = ramp.next_gain(target, rate);
                    let expected = start + (target - start) * frame as f32 / frames as f32;
                    assert!((gain - expected).abs() < 0.0001);
                }
                assert_eq!(ramp.next_gain(target, rate), target);
                assert_eq!(ramp.remaining, 0);
            }
        }
    }

    #[test]
    fn retargeting_volume_continues_from_the_current_gain() {
        let mut ramp = VolumeRamp::default();
        for _ in 0..480 {
            ramp.next_gain(1.0, 48_000);
        }
        let current = ramp.current;
        assert!(current > 0.0 && current < 1.0);
        assert_eq!(ramp.next_gain(0.0, 48_000), current);
        for _ in 1..1440 {
            ramp.next_gain(0.0, 48_000);
        }
        assert_eq!(ramp.next_gain(0.0, 48_000), 0.0);
    }

    #[test]
    fn rendered_volume_is_stereo_linked_and_spans_callbacks() {
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let made = MixerSlot::default();
        let mut render = MixerRender {
            volume: Arc::clone(&volume),
            ramp: VolumeRamp::default(),
            source: None,
            format: (0, 0),
            made: Arc::clone(&made),
        };
        render.configure(48_000, 2);
        let (mixer, _) = made.lock().unwrap().take().unwrap();
        mixer.add(rodio::buffer::SamplesBuffer::new(
            2,
            48_000,
            vec![1.0; 10_000],
        ));
        let mut rising = Vec::new();
        for _ in 0..6 {
            let mut block = [0.0; 480];
            render.render(&mut block);
            rising.extend(block);
        }
        assert_eq!(rising[0], 0.0);
        assert!(rising.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            rising
                .as_chunks::<2>()
                .0
                .iter()
                .all(|pair| pair[0] == pair[1])
        );
        let mut settled = [0.0; 2];
        render.render(&mut settled);
        assert_eq!(settled, [1.0; 2]);
        volume.store(0.0f32.to_bits(), Ordering::Relaxed);
        let mut falling = vec![0.0; 2882];
        render.render(&mut falling);
        assert_eq!(falling[0], 1.0);
        assert_eq!(&falling[2880..], &[0.0; 2]);
        assert!(falling.windows(2).all(|pair| pair[0] >= pair[1]));
        assert!(
            falling
                .as_chunks::<2>()
                .0
                .iter()
                .all(|pair| pair[0] == pair[1])
        );
    }
    /// The buffer setting reaches the device on Windows only (#88), and a
    /// settings file with a wild number in it still opens a stream: the
    /// range is the range whoever wrote the file thought of.
    #[test]
    fn the_buffer_follows_the_setting_within_its_range() {
        let buffer = |ms| output_options(fastframe_audio::Device::Default, ms).buffer;
        let fixed = |ms| Buffer::FixedOnWindows(BufferSize::Duration(Duration::from_millis(ms)));
        assert_eq!(buffer(100), fixed(100));
        assert_eq!(buffer(0), fixed(u64::from(*BUFFER_MS_RANGE.start())));
        assert_eq!(buffer(100_000), fixed(u64::from(*BUFFER_MS_RANGE.end())));
    }

    /// Spotify's own format is asked for first, so nothing is converted.
    #[test]
    fn the_output_asks_for_spotifys_format_on_the_chosen_device() {
        let id = fastframe_audio::Device::Id(DeviceId::new("usb-dac"));
        let options = output_options(id.clone(), DEFAULT_BUFFER_MS);
        assert_eq!(options.device, id);
        assert_eq!(options.sample_rate, Some(SAMPLE_RATE));
        assert_eq!(options.channels, NUM_CHANNELS as u16);
        assert!(options.follow_default);
    }

    fn listed(id: &str, name: &str, is_default: bool) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new(id),
            name: name.into(),
            is_default,
        }
    }

    /// The outputs PipeWire lists on a desktop with a USB DAC beside the
    /// built-in card.
    fn pipewire_outputs() -> Vec<DeviceInfo> {
        vec![
            listed(
                "alsa_output.pci-0000_00_1f.3.analog-stereo",
                "Built-in Audio Analog Stereo",
                true,
            ),
            listed(
                "alsa_output.usb-Topping_DX3_Pro-00.analog-stereo",
                "DX3 Pro Analog Stereo",
                false,
            ),
        ]
    }

    /// A saved id opens that device by its id.
    #[test]
    fn a_saved_id_opens_its_device() {
        let id = "alsa_output.usb-Topping_DX3_Pro-00.analog-stereo";
        let saved = Saved::among(Some(id), &pipewire_outputs());
        assert_eq!(saved, Saved::Id(DeviceId::new(id)));
        assert_eq!(
            saved.device(),
            fastframe_audio::Device::Id(DeviceId::new(id))
        );
        // Windows and macOS save theirs the same way.
        let wasapi = "{0.0.0.00000000}.{6a1b0c4e-2f8d-4b8e-9c55-1d2e3f4a5b6c}";
        let outputs = [listed(wasapi, "Speakers (Realtek(R) Audio)", true)];
        assert_eq!(
            Saved::among(Some(wasapi), &outputs),
            Saved::Id(DeviceId::new(wasapi))
        );
    }

    /// A name an older build saved opens the device that still has it, and
    /// says which id to save.
    #[test]
    fn a_saved_name_a_device_still_has_opens_it_by_that_name() {
        let saved = Saved::among(Some("DX3 Pro Analog Stereo"), &pipewire_outputs());
        assert_eq!(
            saved,
            Saved::Named {
                name: "DX3 Pro Analog Stereo".into(),
                id: DeviceId::new("alsa_output.usb-Topping_DX3_Pro-00.analog-stereo"),
            }
        );
        assert_eq!(
            saved.device(),
            fastframe_audio::Device::Named("DX3 Pro Analog Stereo".into())
        );
        // A CoreAudio name that became a UID.
        let outputs = [listed("BuiltInSpeakerDevice", "MacBook Pro Speakers", true)];
        assert!(matches!(
            Saved::among(Some("MacBook Pro Speakers"), &outputs),
            Saved::Named { .. }
        ));
    }

    /// The names the ALSA host listed on Linux, which PipeWire's outputs do
    /// not have, play through the default, as do blank and unplugged ones.
    #[test]
    fn an_old_alsa_name_or_a_gone_device_plays_the_default() {
        for saved in [
            "PipeWire Sound Server",
            "Default ALSA Output (currently PipeWire Media Server)",
            "PulseAudio Sound Server",
            "HDA Intel PCH, ALC892 Analog",
            "Discard all samples (playback) or generate zero samples (capture)",
            "alsa_output.usb-unplugged.analog-stereo",
            "",
            "  ",
        ] {
            let resolved = Saved::among(Some(saved), &pipewire_outputs());
            assert_eq!(resolved, Saved::Default, "{saved:?}");
            assert_eq!(resolved.device(), fastframe_audio::Device::Default);
        }
        assert_eq!(Saved::among(None, &pipewire_outputs()), Saved::Default);
        assert_eq!(
            Saved::among(Some("DX3 Pro Analog Stereo"), &[]),
            Saved::Default
        );
    }

    /// A stream reopened in the format the mixer already plays keeps it, so
    /// what is queued carries on; another rate gets a new mixer for the sink
    /// to pick up.
    #[test]
    fn only_a_new_format_makes_a_new_mixer() {
        let made = MixerSlot::default();
        let mut render = MixerRender {
            volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            ramp: VolumeRamp::default(),
            source: None,
            format: (0, 0),
            made: Arc::clone(&made),
        };
        let mut out = [1.0; 8];
        render.render(&mut out);
        assert_eq!(out, [0.0; 8], "silence before there is a mixer");

        render.configure(44_100, 2);
        let (mixer, rate) = made.lock().unwrap().take().expect("a first mixer");
        assert_eq!(rate, 44_100);
        render.configure(44_100, 2);
        assert!(made.lock().unwrap().is_none(), "the same format keeps it");
        render.configure(48_000, 2);
        assert_eq!(
            made.lock().unwrap().as_ref().map(|made| made.1),
            Some(48_000)
        );
        drop(mixer);
    }

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

    /// #636: a paused player stops the device asking for sound, and Play,
    /// or sound arriving without one, starts it again. Needs an output, so
    /// a machine without audio has nothing to check.
    #[test]
    fn pause_stops_the_device_and_play_starts_it_again() {
        let mut sink = RodioSink::new(
            None,
            Arc::new(|_| {}),
            Box::new(librespot_playback::mixer::NoOpVolume),
            DEFAULT_BUFFER_MS,
            AudioControl::new(DEFAULT_BUFFER_MS),
        );
        assert!(sink.start().is_ok());
        let running = |sink: &RodioSink| {
            sink.output
                .as_ref()
                .map(|output| !output.device.is_paused())
        };
        if running(&sink).is_none() {
            return;
        }
        let mut converter = Converter::new(None);
        // Silence, so a test run plays nothing on the speakers.
        let packet = || AudioPacket::Samples(vec![0.0; 441 * NUM_CHANNELS as usize]);
        sink.write(packet(), &mut converter).unwrap();
        assert_eq!(running(&sink), Some(true));

        assert!(sink.stop().is_ok());
        assert_eq!(running(&sink), Some(false), "paused, the device is quiet");
        assert!(sink.stop().is_ok(), "stopping twice is harmless");

        assert!(sink.start().is_ok());
        assert_eq!(running(&sink), Some(true), "Play starts it again");

        assert!(sink.stop().is_ok());
        sink.write(packet(), &mut converter).unwrap();
        assert_eq!(running(&sink), Some(true), "and so does sound");
        assert!(sink.stop().is_ok());
    }

    fn no_device(_: Option<&str>, _: u32, _: &AudioControl) -> Result<Output, OpenError> {
        Err(OpenError::NoDevice)
    }

    /// #623: a PC with no output at all. librespot exits the process when
    /// `start` fails from its playing loop, but pauses cleanly when `write`
    /// fails, so the failure has to surface from `write`, reported to the
    /// interface once per attempt to play.
    #[test]
    fn with_no_output_at_all_playing_fails_at_the_first_packet_not_at_start() {
        let reported: Arc<Mutex<Vec<String>>> = Arc::default();
        let store = Arc::clone(&reported);
        let mut sink = RodioSink {
            open: no_device,
            ..RodioSink::new(
                None,
                Arc::new(move |message| store.lock().unwrap().push(message)),
                Box::new(librespot_playback::mixer::NoOpVolume),
                DEFAULT_BUFFER_MS,
                AudioControl::new(DEFAULT_BUFFER_MS),
            )
        };
        let mut converter = Converter::new(None);
        let packet = || AudioPacket::Samples(vec![0.0; 441 * NUM_CHANNELS as usize]);

        for attempt in 1..=2 {
            assert!(sink.start().is_ok(), "librespot exits when start fails");
            assert_eq!(reported.lock().unwrap().len(), attempt - 1);

            let Err(SinkError::ConnectionRefused(message)) = sink.write(packet(), &mut converter)
            else {
                panic!("the first packet must report the missing output");
            };
            assert_eq!(message, NO_DEVICE);
            assert_eq!(reported.lock().unwrap().len(), attempt);
            assert_eq!(reported.lock().unwrap().last().unwrap(), NO_DEVICE);

            // librespot pauses on the failed write, which stops the sink.
            assert!(sink.stop().is_ok());
        }
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
    fn an_interrupted_decoder_cannot_race_to_the_end_while_a_new_track_loads() {
        let control = AudioControl::new(DEFAULT_BUFFER_MS);
        control.interrupt();
        let mut sink = RodioSink::new(
            None,
            Arc::new(|error| panic!("no audio device should be opened: {error}")),
            Box::new(librespot_playback::mixer::NoOpVolume),
            DEFAULT_BUFFER_MS,
            control,
        );
        let mut converter = Converter::new(None);
        let frames = SAMPLE_RATE as usize / 100;
        let started = Instant::now();
        for _ in 0..4 {
            sink.write(
                AudioPacket::Samples(vec![0.0; frames * NUM_CHANNELS as usize]),
                &mut converter,
            )
            .unwrap();
        }
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "discarded audio must retain backpressure until the new load reaches the decoder"
        );
        assert!(sink.output.is_none());
    }

    #[test]
    fn confirmed_seek_discards_the_old_position_without_gating_new_packets() {
        let control = AudioControl::new(DEFAULT_BUFFER_MS);
        let (sink, mut output) = rodio::Sink::new();
        let sink = Arc::new(sink);
        let (interrupt, transport) = wide_open();
        let queued = Queued::new();
        control.register(&sink, Arc::clone(&interrupt));
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
        control.register(&sink, Arc::clone(&interrupt));
        sink.append(chunk(500, &interrupt, &transport, &Queued::new()));
        assert_eq!(output.next(), Some(1.0));
        let track_id =
            librespot_core::SpotifyUri::from_uri("spotify:track:14XWXWv5FoCbFzLksawpEe").unwrap();
        let item = AudioItem {
            track_id: track_id.clone(),
            uri: track_id.to_uri(),
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
        let left = 20;
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

    /// A dozen of librespot's shortest packets (4 ms each) are less than one
    /// 43 ms PipeWire quantum, so a queue limited by count ran dry on every
    /// callback; the limit is time.
    #[test]
    fn the_queue_holds_time_not_a_count_of_packets() {
        let short_packet = 176; // 4 ms at 44.1 kHz
        assert!(!queue_is_full(12 * short_packet, 44_100));
        assert!(!queue_is_full(8_820, 44_100), "200 ms is the most it holds");
        assert!(queue_is_full(8_821, 44_100));
        assert!(!queue_is_full(9_600, 48_000));
        assert!(queue_is_full(9_601, 48_000));
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

    /// A writer waiting for room sleeps through the chunks that leave the
    /// queue above its level, and the one that reaches it wakes the writer,
    /// once. Polling instead woke the decoder thread every 10 ms.
    #[test]
    fn a_waiting_writer_wakes_once_the_queue_has_drained_enough() {
        let queued = Queued::new();
        let (interrupt, transport) = wide_open();
        let mut chunks: Vec<_> = (0..4)
            .map(|_| chunk(10, &interrupt, &transport, &queued))
            .collect();
        queued.wake_at.store(20, Ordering::SeqCst);

        drop(chunks.remove(0));
        assert_eq!(queued.frames(), 30);
        assert_eq!(queued.wake_at.load(Ordering::SeqCst), 20, "still asleep");

        drop(chunks.remove(0));
        assert_eq!(queued.wake_at.load(Ordering::SeqCst), 0, "woken, once");
        // The chunks were queued from this thread, so the wake is its own:
        // the token is waiting, and parking returns at once.
        let started = Instant::now();
        thread::park_timeout(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));

        drop(chunks);
        assert_eq!(queued.frames(), 0);
    }

    /// A stream that has stopped consuming finishes no chunk, so the wait
    /// gives up by itself and `write` gets to have the device maintained,
    /// which reopens a stream that stopped playing (Reason::Stalled).
    #[test]
    fn waiting_for_room_gives_up_when_nothing_plays() {
        let queued = Queued::new();
        queued.appended.store(1_000, Ordering::SeqCst);
        let started = Instant::now();
        queued.wait_for_room(100);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(queued.wake_at.load(Ordering::SeqCst), 0);
    }
}
