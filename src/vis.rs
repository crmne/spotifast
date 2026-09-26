//! Audio tap and Winamp-style spectrum and oscilloscope data.
//!
//! The tap wraps the active sink and stores half a second of post-EQ,
//! pre-volume audio. The analyser uses Winamp's constants and behavior from
//! `classic_vis.cpp`, with FFT details cross-checked against Webamp's
//! `VisPainter.ts` and `FFTNullsoft.ts`. MilkDrop receives stereo samples;
//! the spectrum and scope use mono samples.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::VolumeGetter;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};

/// Half a second of audio.
const KEPT: usize = SAMPLE_RATE as usize / 2;
/// How far behind the newest sample the visualiser looks, so that it shows
/// what the speaker is playing rather than what the sink has queued.
pub const LAG: usize = SAMPLE_RATE as usize * 3 / 20;
/// Samples that go into one spectrum.
pub const FFT_SAMPLES: usize = 512;
const SPECTRUM_BINS: usize = FFT_SAMPLES / 2;
/// Samples the scope reads, one column every seventh.
pub const SCOPE_SAMPLES: usize = 576;
/// The visualiser's width and height in skin pixels.
pub const COLUMNS: usize = 75;
pub const ROWS: u8 = 16;
/// The bars, each three columns wide with one between.
pub const BARS: usize = 19;
/// The tallest a bar gets.
const MAX_HEIGHT: f32 = 15.0;
/// How far a bar falls each step, and how peaks pick up speed.
const FALLOFF: f32 = 12.0 / 16.0;
const PEAK_FALLOFF: f32 = 1.1;
/// How often the bars move. Winamp drew its analyser sixty times a
/// second on a timer of its own, so a fast or slow frame rate never
/// changed how quickly the bars fell; a frame that comes sooner than
/// this shows the bars where they were.
pub const STEP: Duration = Duration::from_micros(16_667);
/// Converts the tap's channel mean to Winamp's channel sum.
const CHANNEL_SUM: f32 = 2.0;
/// Winamp's own scale on every magnitude.
const SPEC_SCALE: f32 = 0.5;

/// The last half second of sound, shared between the player's thread and
/// the visualiser. The mono mix stays in this process for the skin's
/// analyser; the stereo sound goes to a shared-memory ring for the MilkDrop
/// child process, when one is running.
pub struct AudioTap {
    samples: Mutex<VecDeque<f32>>,
    /// The ring the MilkDrop child reads, attached while its window is open.
    #[cfg(feature = "milkdrop")]
    shm: Mutex<Option<std::sync::Arc<crate::milkdrop::shm::Ring>>>,
}

impl std::fmt::Debug for AudioTap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudioTap")
    }
}

impl Default for AudioTap {
    fn default() -> Self {
        Self {
            samples: Mutex::new(VecDeque::with_capacity(KEPT)),
            #[cfg(feature = "milkdrop")]
            shm: Mutex::new(None),
        }
    }
}

impl AudioTap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Connects the tap to MilkDrop's shared-memory ring. `None` detaches it.
    #[cfg(feature = "milkdrop")]
    pub fn set_shm(&self, ring: Option<std::sync::Arc<crate::milkdrop::shm::Ring>>) {
        *self.shm.lock().unwrap_or_else(|p| p.into_inner()) = ring;
    }

    /// Adds scaled stereo samples to the mono analyser buffer and, when
    /// attached, MilkDrop's stereo shared-memory ring.
    pub fn push(&self, interleaved: &[f64], gain: f32) {
        let mut samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        let (frames, _) = interleaved.as_chunks::<{ NUM_CHANNELS as usize }>();
        #[cfg(feature = "milkdrop")]
        let shm = self.shm.lock().unwrap_or_else(|p| p.into_inner()).clone();
        #[cfg(feature = "milkdrop")]
        let mut stereo: Vec<f32> = if shm.is_some() {
            Vec::with_capacity(frames.len() * 2)
        } else {
            Vec::new()
        };
        for frame in frames {
            let mono = frame.iter().sum::<f64>() as f32 / frame.len() as f32 * gain;
            if samples.len() == KEPT {
                samples.pop_front();
            }
            samples.push_back(mono);
            #[cfg(feature = "milkdrop")]
            if shm.is_some() {
                stereo.push(frame[0] as f32 * gain);
                stereo.push(frame[1] as f32 * gain);
            }
        }
        #[cfg(feature = "milkdrop")]
        if let Some(ring) = &shm {
            ring.push(&stereo);
        }
    }

    /// The `count` samples ending `lag` samples before the newest, with
    /// silence where there is less than that.
    pub fn window(&self, count: usize, lag: usize) -> Vec<f32> {
        let samples = self.samples.lock().unwrap_or_else(|p| p.into_inner());
        let end = samples.len().saturating_sub(lag);
        let start = end.saturating_sub(count);
        let mut out = vec![0.0; count];
        let taken = end - start;
        for (slot, sample) in out[count - taken..]
            .iter_mut()
            .zip(samples.range(start..end))
        {
            *slot = *sample;
        }
        out
    }

    pub fn clear(&self) {
        self.samples
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }
}

/// Runs the equalizer and taps the signal before passing it to the real sink.
pub struct Tapped {
    inner: Box<dyn Sink>,
    tap: Arc<AudioTap>,
    eq: crate::eq::Processor,
    /// Player volume, used to calculate the limiter ceiling.
    volume: Box<dyn VolumeGetter + Send>,
    /// Whether this wrapper applies volume after the tap. Otherwise the inner
    /// sink applies it, still after the tap and to already queued audio.
    applies_volume: bool,
    /// Final limiter, placed here because this stage knows the output volume.
    limiter: crate::limiter::Limiter,
    /// Track normalization factor. The tap removes it so visualizers show the
    /// source dynamics, as Winamp's analyser did.
    normalisation: Arc<std::sync::atomic::AtomicU64>,
}

impl Tapped {
    pub fn new(
        inner: Box<dyn Sink>,
        tap: Arc<AudioTap>,
        volume: Box<dyn VolumeGetter + Send>,
        applies_volume: bool,
        eq: crate::eq::SharedEq,
        normalisation: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            inner,
            tap,
            eq: crate::eq::Processor::new(eq),
            volume,
            applies_volume,
            limiter: crate::limiter::Limiter::new(f64::from(SAMPLE_RATE)),
            normalisation,
        }
    }
}

/// Full-scale level for samples leaving `Tapped`.
///
/// This is 1.0 after volume is applied. Before volume, it is the level that
/// becomes 1.0 after the inner sink applies volume.
fn full_scale(volume: f64, applied: bool) -> Option<f64> {
    if applied {
        Some(1.0)
    } else if volume > f64::EPSILON {
        Some(1.0 / volume)
    } else {
        // At zero volume, no finite pre-volume ceiling is needed.
        None
    }
}

impl Sink for Tapped {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.tap.clear();
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let packet = match packet {
            AudioPacket::Samples(mut samples) => {
                self.eq.process(&mut samples);
                // Post-EQ, pre-volume, pre-normalisation: the equalizer
                // shapes what the bars show; the volume knob and the
                // loudness housekeeping never move them.
                let factor = f64::from_bits(
                    self.normalisation
                        .load(std::sync::atomic::Ordering::Relaxed),
                );
                let restore = if factor > 0.05 && factor < 20.0 {
                    (1.0 / factor).clamp(0.125, 8.0) as f32
                } else {
                    1.0
                };
                self.tap.push(&samples, restore);
                let attenuation = self.volume.attenuation_factor();
                if self.applies_volume {
                    for sample in &mut samples {
                        *sample *= attenuation;
                    }
                }
                if let Some(full_scale) = full_scale(attenuation, self.applies_volume) {
                    self.limiter.process(&mut samples, full_scale);
                }
                AudioPacket::Samples(samples)
            }
            raw => raw,
        };
        self.inner.write(packet, converter)
    }
}

/// The classic analyser's FFT, as Winamp's own source has it: 512
/// samples under a Hann window, 256 magnitudes out, each halved. The
/// player bar's analyser runs the same transform over more samples.
struct Fft {
    bit_reversed: Vec<usize>,
    envelope: Vec<f32>,
    twiddles: Vec<(f32, f32)>,
    real: Vec<f32>,
    imaginary: Vec<f32>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let mut bit_reversed: Vec<usize> = (0..n).collect();
        let mut j = 0;
        for i in 0..n {
            if j > i {
                bit_reversed.swap(i, j);
            }
            let mut m = n >> 1;
            while m >= 1 && j >= m {
                j -= m;
                m >>= 1;
            }
            j += m;
        }
        let envelope = (0..n)
            .map(|i| {
                let phase = i as f32 / n as f32 * std::f32::consts::TAU;
                0.5 + 0.5 * (phase - std::f32::consts::FRAC_PI_2).sin()
            })
            .collect();
        let mut twiddles = Vec::new();
        let mut size = 2;
        while size <= n {
            let theta = -std::f32::consts::TAU / size as f32;
            twiddles.push((theta.cos(), theta.sin()));
            size <<= 1;
        }
        Self {
            bit_reversed,
            envelope,
            twiddles,
            real: vec![0.0; n],
            imaginary: vec![0.0; n],
        }
    }

    fn spectrum(&mut self, wave: &[f32], out: &mut [f32]) {
        let n = self.real.len();
        for i in 0..n {
            let from = self.bit_reversed[i];
            self.real[i] = wave.get(from).copied().unwrap_or(0.0) * self.envelope[from];
            self.imaginary[i] = 0.0;
        }
        let mut size = 2;
        let mut stage = 0;
        while size <= n {
            let (wpr, wpi) = self.twiddles[stage];
            let (mut wr, mut wi) = (1.0f32, 0.0f32);
            let half = size >> 1;
            for m in 0..half {
                let mut i = m;
                while i < n {
                    let j = i + half;
                    let tr = wr * self.real[j] - wi * self.imaginary[j];
                    let ti = wr * self.imaginary[j] + wi * self.real[j];
                    self.real[j] = self.real[i] - tr;
                    self.imaginary[j] = self.imaginary[i] - ti;
                    self.real[i] += tr;
                    self.imaginary[i] += ti;
                    i += size;
                }
                let previous = wr;
                wr = wr * wpr - wi * wpi;
                wi = wi * wpr + previous * wpi;
            }
            size <<= 1;
            stage += 1;
        }
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = (self.real[i] * self.real[i] + self.imaginary[i] * self.imaginary[i]).sqrt()
                * SPEC_SCALE;
        }
    }
}

/// One bar of the analyser, in rows from the bottom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bar {
    /// How tall the bar is, 0 to 15.
    pub height: u8,
    /// Where the peak mark sits, as the height of a bar whose top row it
    /// would be, 1 to 16; `None` while it is out of sight.
    pub peak: Option<u8>,
}

/// The spectrum analyser's memory: where each bar and its peak are.
pub struct Analyser {
    fft: Fft,
    spectrum: [f32; SPECTRUM_BINS],
    wave: [f32; FFT_SAMPLES],
    /// Where each bar is, falling at a fixed rate towards the sound.
    falloff: [f32; BARS],
    /// Each peak, in 256ths of a row, and how fast it is dropping.
    peaks: [i32; BARS],
    peak_speed: [f32; BARS],
    /// When the bars last moved, and where they are.
    last_step: Option<Instant>,
    bars: [Bar; BARS],
}

impl Default for Analyser {
    fn default() -> Self {
        Self {
            fft: Fft::new(FFT_SAMPLES),
            spectrum: [0.0; SPECTRUM_BINS],
            wave: [0.0; FFT_SAMPLES],
            falloff: [0.0; BARS],
            peaks: [0; BARS],
            peak_speed: [0.0; BARS],
            last_step: None,
            bars: [Bar::default(); BARS],
        }
    }
}

impl Analyser {
    /// One frame: the spectrum of `samples` (512 of them, mono, -1 to 1)
    /// moves the bars, and the bars are returned.
    pub fn step(&mut self, samples: &[f32], now: Instant) -> [Bar; BARS] {
        // Keep the step's own beat when frames come a little early or late,
        // and never owe more than one step after a long gap.
        let due = self.last_step.map_or(now, |last| last + STEP);
        if now + Duration::from_millis(1) < due {
            return self.bars;
        }
        self.last_step = Some(due.max(now - STEP));
        self.wave.fill(0.0);
        for (slot, sample) in self.wave.iter_mut().zip(samples.iter()) {
            *slot = sample * CHANNEL_SUM;
        }
        self.fft.spectrum(&self.wave, &mut self.spectrum);
        let columns = self.bands();
        let mut bars = [Bar::default(); BARS];
        for (bar, slot) in bars.iter_mut().enumerate() {
            let chunk = 4 * bar;
            let sound =
                (columns[chunk] + columns[chunk + 1] + columns[chunk + 2] + columns[chunk + 3])
                    / 4.0;
            // Winamp kept the target as a whole number of rows.
            let target = sound.min(MAX_HEIGHT).trunc();
            let falloff = &mut self.falloff[bar];
            *falloff -= FALLOFF;
            if *falloff <= target {
                *falloff = target;
            }
            let peak = &mut self.peaks[bar];
            if *peak <= (*falloff * 256.0).round() as i32 {
                *peak = (*falloff * 256.0) as i32;
                self.peak_speed[bar] = 3.0;
            }
            let peak_row = *peak / 256;
            *peak -= self.peak_speed[bar].round() as i32;
            self.peak_speed[bar] *= PEAK_FALLOFF;
            if *peak <= 0 {
                *peak = 0;
            }
            slot.height = falloff.round() as u8;
            slot.peak = (peak_row >= 1).then_some((peak_row + 1) as u8);
        }
        self.bars = bars;
        bars
    }

    /// Whether every bar and peak has come to rest, so nothing moves until
    /// there is sound again.
    pub fn settled(&self) -> bool {
        self.falloff.iter().all(|f| *f <= 0.0) && self.peaks.iter().all(|p| *p == 0)
    }

    pub fn reset(&mut self) {
        self.falloff = [0.0; BARS];
        self.peaks = [0; BARS];
        self.peak_speed = [0.0; BARS];
        self.last_step = None;
        self.bars = [Bar::default(); BARS];
    }

    /// Winamp's own bands, from its published source: seventy-five spans
    /// a semitone apart (`2^(x/12)`), each summing its share of the 256
    /// bins, the fractional edges read through a Hermite curve, the sum
    /// clipped at 255. Fifteen of those 255 fill the display, which is why
    /// the classic analyser always looked alive. One extra silent column
    /// pads the last bar's group of four.
    fn bands(&self) -> [f32; COLUMNS + 1] {
        bands(&self.spectrum)
    }
}

/// Winamp's bands over a 256-bin spectrum; see [`Analyser::bands`].
fn bands(spectrum: &[f32; SPECTRUM_BINS]) -> [f32; COLUMNS + 1] {
    let bla = 255.0 / 2f32.powf(75.0 / 12.0);
    let warp = |x: f32| (2f32.powf(x / 12.0) - 1.0) * bla;
    let sample = |index: usize| spectrum.get(index).copied().unwrap_or(0.0);
    let hermite = |x: f32, y0: f32, y1: f32, y2: f32, y3: f32| {
        let c1 = 0.5 * (y2 - y0);
        let c3 = 1.5 * (y1 - y2) + 0.5 * (y3 - y0);
        let c2 = y0 - y1 + c1 - c3;
        ((c3 * x + c2) * x + c1) * x + y1
    };
    let mut columns = [0.0; COLUMNS + 1];
    let mut next = warp(0.0) + 1.0;
    for (x, column) in columns.iter_mut().take(COLUMNS).enumerate() {
        let low = next;
        next = warp(x as f32 + 1.0) + 1.0;
        let mut value = 0.0f32;
        let mut bin = low.floor() as usize;
        let end = (next.floor() as usize).min(SPECTRUM_BINS - 1);
        let mut fraction = low;
        let mut mult = (bin as f32 + 1.0) - low;
        let mut herm = true;
        loop {
            if bin == end {
                mult = next - fraction;
                herm = true;
            }
            if herm {
                value += hermite(
                    fraction - bin as f32,
                    sample(bin.saturating_sub(1)),
                    sample(bin),
                    sample(bin + 1),
                    sample(bin + 2),
                ) * mult;
            } else {
                value += sample(bin);
            }
            herm = false;
            bin += 1;
            if bin > end {
                break;
            }
            fraction = bin as f32;
        }
        *column = value.min(255.0);
    }
    columns
}

/// The oscilloscope's trace: a row (0 at the top) for each column, from
/// every seventh of the samples, the wave's centre at row 7.
pub fn scope(samples: &[f32]) -> [u8; COLUMNS] {
    let mut rows = [7u8; COLUMNS];
    for (column, row) in rows.iter_mut().enumerate() {
        let sample = samples.get(column * 7).copied().unwrap_or(0.0);
        let byte = ((sample * 128.0 + 128.0).round()).clamp(0.0, 255.0);
        let y = (byte / 16.0 * 2.0).round() - 9.0;
        *row = y.clamp(0.0, f32::from(ROWS) - 1.0) as u8;
    }
    rows
}

/// Which of the scope's five colours a row is drawn in: brightest at the
/// centre, darker towards the edges.
pub fn scope_shade(row: u8) -> usize {
    match row {
        14.. => 4,
        12..=13 => 3,
        10..=11 => 2,
        8..=9 => 1,
        6..=7 => 0,
        4..=5 => 1,
        2..=3 => 2,
        _ => 3,
    }
}

/// Samples in one spectrum of the player bar's analyser: Winamp's own.
pub const WIDE_SAMPLES: usize = FFT_SAMPLES;
/// The player bar's bands: every one of Winamp's semitone columns as a bar
/// of its own, where the skin groups them four to a bar.
pub const WIDE_BANDS: usize = COLUMNS;
/// How fast a bar falls, in heights per second: Winamp's twelve
/// sixteenths of a row each sixtieth of a second, over fifteen rows.
const WIDE_FALL: f32 = FALLOFF / MAX_HEIGHT / 0.016_667;
/// How long a peak cap hangs above its band before it drops, and how fast
/// it then falls, in heights per second squared.
const PEAK_HOLD: f32 = 0.35;
const PEAK_GRAVITY: f32 = 2.4;

/// The player bar's spectrum analyser: Winamp's classic analyser, with
/// each of its seventy-five columns as a bar, heights from 0 to 1 instead
/// of whole rows, and falls timed by the clock, so they move smoothly at
/// any frame rate.
pub struct WideAnalyser {
    fft: Fft,
    wave: [f32; FFT_SAMPLES],
    spectrum: [f32; SPECTRUM_BINS],
    levels: [f32; WIDE_BANDS],
    /// Each band's peak cap, how long it has hung there, and how fast it
    /// is falling.
    peaks: [f32; WIDE_BANDS],
    held: [f32; WIDE_BANDS],
    speed: [f32; WIDE_BANDS],
    last: Option<Instant>,
}

impl Default for WideAnalyser {
    fn default() -> Self {
        Self {
            fft: Fft::new(FFT_SAMPLES),
            wave: [0.0; FFT_SAMPLES],
            spectrum: [0.0; SPECTRUM_BINS],
            levels: [0.0; WIDE_BANDS],
            peaks: [0.0; WIDE_BANDS],
            held: [0.0; WIDE_BANDS],
            speed: [0.0; WIDE_BANDS],
            last: None,
        }
    }
}

impl WideAnalyser {
    /// Moves the bars towards the spectrum of `samples` (mono, -1 to 1,
    /// `WIDE_SAMPLES` of them): up at once, as Winamp's, and down at its
    /// rate for the time since the last step.
    pub fn step(&mut self, samples: &[f32], now: Instant) -> [f32; WIDE_BANDS] {
        let elapsed = self
            .last
            .map_or(STEP, |last| now.saturating_duration_since(last))
            .min(Duration::from_millis(250))
            .as_secs_f32();
        self.last = Some(now);
        self.wave.fill(0.0);
        for (slot, sample) in self.wave.iter_mut().zip(samples) {
            *slot = sample * CHANNEL_SUM;
        }
        self.fft.spectrum(&self.wave, &mut self.spectrum);
        let columns = bands(&self.spectrum);
        for (level, column) in self.levels.iter_mut().zip(columns) {
            let target = column.min(MAX_HEIGHT) / MAX_HEIGHT;
            *level = (*level - WIDE_FALL * elapsed).max(target);
            // Too low to see: at rest.
            if *level < 0.01 {
                *level = 0.0;
            }
        }
        for band in 0..WIDE_BANDS {
            let level = self.levels[band];
            if level >= self.peaks[band] {
                self.peaks[band] = level;
                self.held[band] = 0.0;
                self.speed[band] = 0.0;
            } else if self.held[band] < PEAK_HOLD {
                self.held[band] += elapsed;
            } else {
                self.speed[band] += PEAK_GRAVITY * elapsed;
                self.peaks[band] = (self.peaks[band] - self.speed[band] * elapsed).max(level);
            }
            if self.peaks[band] < 0.01 {
                self.peaks[band] = 0.0;
            }
        }
        self.levels
    }

    /// Where each band's peak cap hangs, from 0 to 1.
    pub fn peaks(&self) -> [f32; WIDE_BANDS] {
        self.peaks
    }

    /// Whether every band and peak has fallen to rest.
    pub fn settled(&self) -> bool {
        self.levels
            .iter()
            .chain(&self.peaks)
            .all(|level| *level == 0.0)
    }
}

/// Samples the player bar's waveform reads.
pub const TRACE_SAMPLES: usize = 2048;

/// The player bar's waveform: `points` values from -1 to 1 across a
/// stretch of `samples` that starts where the wave rises through zero, so
/// a steady tone stands still instead of sliding sideways.
pub fn trace(samples: &[f32], points: usize) -> Vec<f32> {
    if points == 0 {
        return Vec::new();
    }
    let span = samples.len() / 2;
    let start = (1..span)
        .find(|&index| samples[index - 1] < 0.0 && samples[index] >= 0.0)
        .unwrap_or(0);
    let stretch = &samples[start..(start + span).min(samples.len())];
    (0..points)
        .map(|point| {
            let from = point * stretch.len() / points;
            let to = ((point + 1) * stretch.len() / points).max(from + 1);
            let chunk = &stretch[from.min(stretch.len())..to.min(stretch.len())];
            if chunk.is_empty() {
                0.0
            } else {
                (chunk.iter().sum::<f32>() / chunk.len() as f32).clamp(-1.0, 1.0)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, amplitude: f32, count: usize) -> Vec<f32> {
        (0..count)
            .map(|i| amplitude * (i as f32 / SAMPLE_RATE as f32 * hz * std::f32::consts::TAU).sin())
            .collect()
    }

    #[test]
    fn the_tap_mixes_stereo_down_and_pads_with_silence() {
        let tap = AudioTap::new();
        let interleaved: Vec<f64> = vec![0.5, -0.5, 1.0, 0.0, 0.2, 0.2];
        tap.push(&interleaved, 2.0);
        assert_eq!(tap.window(5, 0), [0.0, 0.0, 0.0, 1.0, 0.4]);
        assert_eq!(tap.window(2, 1), [0.0, 1.0]);
        tap.clear();
        assert_eq!(tap.window(2, 0), [0.0, 0.0]);
    }

    #[test]
    fn the_tap_keeps_half_a_second_at_most() {
        let tap = AudioTap::new();
        let interleaved = vec![0.25f64; 2 * (KEPT + 100)];
        tap.push(&interleaved, 1.0);
        let samples = tap.samples.lock().unwrap();
        assert_eq!(samples.len(), KEPT);
    }

    #[test]
    fn the_spectrum_peaks_where_the_tone_is() {
        let mut fft = Fft::new(FFT_SAMPLES);
        let mut out = [0.0; SPECTRUM_BINS];
        let wave: Vec<f32> = sine(1000.0, 0.5, FFT_SAMPLES)
            .into_iter()
            .map(|s| s * CHANNEL_SUM)
            .collect();
        fft.spectrum(&wave, &mut out);
        let loudest = (0..SPECTRUM_BINS)
            .max_by(|a, b| out[*a].total_cmp(&out[*b]))
            .unwrap();
        let expected = (1000.0 / SAMPLE_RATE as f32 * FFT_SAMPLES as f32).round() as usize;
        assert!(
            loudest.abs_diff(expected) <= 1,
            "peak at bin {loudest}, tone at {expected}"
        );
    }

    /// Frames one step apart, the way a 60 Hz loop delivers them.
    fn clock() -> impl FnMut() -> Instant {
        let mut at = Instant::now();
        move || {
            at += STEP;
            at
        }
    }

    #[test]
    fn a_fast_frame_rate_leaves_the_bars_alone() {
        let mut analyser = Analyser::default();
        let loud = sine(1000.0, 0.5, FFT_SAMPLES);
        let silence = vec![0.0; FFT_SAMPLES];
        let start = Instant::now();
        let bars = analyser.step(&loud, start);
        // Frames a millisecond apart do not move the bars: they are shown
        // where they were, however often the window paints.
        for i in 1..12 {
            let again = analyser.step(&silence, start + Duration::from_millis(i));
            assert_eq!(
                again.iter().map(|b| b.height).collect::<Vec<_>>(),
                bars.iter().map(|b| b.height).collect::<Vec<_>>()
            );
        }
        let moved = analyser.step(&silence, start + STEP);
        assert!(
            moved
                .iter()
                .zip(bars.iter())
                .any(|(after, before)| after.height < before.height)
        );
    }

    #[test]
    fn bars_rise_with_sound_and_fall_without() {
        let mut analyser = Analyser::default();
        let mut tick = clock();
        let loud: Vec<f32> = (0..FFT_SAMPLES)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32 * std::f32::consts::TAU;
                0.3 * ((t * 100.0).sin() + (t * 800.0).sin() + (t * 5000.0).sin())
            })
            .collect();
        let bars = analyser.step(&loud, tick());
        let tallest = bars.iter().map(|bar| bar.height).max().unwrap();
        assert!(tallest > 0, "no bar rose to the sound");
        assert!(tallest <= 15);
        assert!(!analyser.settled());

        let silence = vec![0.0; FFT_SAMPLES];
        let after = analyser.step(&silence, tick());
        let lower = bars
            .iter()
            .zip(after.iter())
            .all(|(before, after)| after.height <= before.height);
        assert!(lower, "bars rose in silence");
        // The peak hangs above the bar it came from.
        let with_peak = after.iter().find(|bar| bar.peak.is_some()).unwrap();
        assert!(with_peak.peak.unwrap() > with_peak.height);
        for _ in 0..400 {
            analyser.step(&silence, tick());
        }
        assert!(analyser.settled());
        assert!(
            analyser
                .step(&silence, tick())
                .iter()
                .all(|bar| bar.height == 0 && bar.peak.is_none())
        );
    }

    #[test]
    fn the_scope_rests_at_the_middle_and_stays_inside() {
        assert!(scope(&[0.0; SCOPE_SAMPLES]).iter().all(|row| *row == 7));
        let loud = scope(&sine(440.0, 1.0, SCOPE_SAMPLES));
        assert!(loud.iter().all(|row| *row < ROWS));
        assert!(loud.iter().any(|row| *row != 7));
        assert_eq!(scope_shade(7), 0);
        assert_eq!(scope_shade(0), 3);
        assert_eq!(scope_shade(15), 4);
    }

    /// A reused analyser that just saw a full frame must match a fresh one
    /// when the next input is short or empty. Zero-fill stops old samples
    /// leaking into the spectrum.
    #[test]
    fn a_short_or_empty_input_does_not_keep_the_previous_spectrum() {
        let mut reused = Analyser::default();
        let mut fresh = Analyser::default();
        let loud = sine(1000.0, 0.5, FFT_SAMPLES);
        let t0 = Instant::now();
        reused.step(&loud, t0);
        let t1 = t0 + STEP;
        let short = &loud[..32];
        reused.step(short, t1);
        fresh.step(short, t1);
        assert_eq!(
            reused.spectrum, fresh.spectrum,
            "a short frame left the previous spectrum in the scratch buffer"
        );
        let mut reused_empty = Analyser::default();
        let mut fresh_empty = Analyser::default();
        reused_empty.step(&loud, t0);
        reused_empty.step(&[], t1);
        fresh_empty.step(&[], t1);
        assert_eq!(
            reused_empty.spectrum, fresh_empty.spectrum,
            "an empty frame left the previous spectrum in the scratch buffer"
        );
    }
    /// A loud tone lifts the bands around its pitch to the top and leaves
    /// distant ones low; silence lets every band fall to rest.
    #[test]
    fn the_wide_analyser_follows_a_tone_and_settles_in_silence() {
        let mut analyser = WideAnalyser::default();
        let start = Instant::now();
        let tone = sine(1000.0, 0.8, WIDE_SAMPLES);
        let levels = analyser.step(&tone, start);
        let loudest = levels.iter().copied().fold(0.0, f32::max);
        assert!(loudest > 0.99, "{loudest}");
        assert!(levels[0] < 0.3, "the bass stays low: {}", levels[0]);
        assert!(levels[WIDE_BANDS - 1] < 0.3, "{}", levels[WIDE_BANDS - 1]);
        let silence = vec![0.0; WIDE_SAMPLES];
        for frame in 1..120 {
            analyser.step(&silence, start + STEP * frame);
        }
        assert!(analyser.settled());
    }

    /// The bars fall at Winamp's rate by the clock, not by how often they
    /// are drawn.
    #[test]
    fn the_wide_analyser_falls_the_same_at_any_frame_rate() {
        let tone = sine(440.0, 0.8, WIDE_SAMPLES);
        let silence = vec![0.0; WIDE_SAMPLES];
        let run = |step: Duration, steps: u32| {
            let mut analyser = WideAnalyser::default();
            let start = Instant::now();
            analyser.step(&tone, start);
            let mut levels = [0.0; WIDE_BANDS];
            for frame in 1..=steps {
                levels = analyser.step(&silence, start + step * frame);
            }
            levels
        };
        let slow = run(Duration::from_millis(40), 3);
        let fast = run(Duration::from_millis(10), 12);
        for (slow, fast) in slow.iter().zip(fast) {
            assert!((slow - fast).abs() < 0.01, "{slow} against {fast}");
        }
        // Winamp's bars cross the whole height in a third of a second.
        assert!(slow.iter().all(|level| *level < 0.7));
    }
    #[test]
    fn full_scale_follows_the_volume_still_to_come() {
        assert_eq!(full_scale(0.5, true), Some(1.0), "already applied: one");
        assert_eq!(full_scale(0.25, false), Some(4.0), "a quarter to come");
        assert_eq!(full_scale(1.0, false), Some(1.0), "full volume to come");
        assert_eq!(full_scale(0.0, false), None, "silence has no ceiling");
    }

    #[test]
    fn the_trace_starts_on_a_rising_zero_crossing() {
        // A quarter period in, the wave is at its crest.
        let wave: Vec<f32> = sine(441.0, 0.5, TRACE_SAMPLES).split_off(25);
        let points = trace(&wave, 64);
        assert_eq!(points.len(), 64);
        // Each point averages its share of the samples, so the first sits
        // just above the crossing.
        assert!((0.0..0.25).contains(&points[0]), "{}", points[0]);
        assert!(points[1] > points[0], "rising");
        assert!(points.iter().all(|point| point.abs() <= 0.5));
        assert_eq!(trace(&[0.0; 100], 8), vec![0.0; 8]);
        assert!(trace(&wave, 0).is_empty());
    }

    /// A peak cap hangs where the band last reached, then falls after it.
    #[test]
    fn a_peak_hangs_then_falls_behind_its_band() {
        let mut analyser = WideAnalyser::default();
        let start = Instant::now();
        let tone = sine(1000.0, 0.8, WIDE_SAMPLES);
        analyser.step(&tone, start);
        let band = (0..WIDE_BANDS)
            .max_by(|a, b| analyser.peaks()[*a].total_cmp(&analyser.peaks()[*b]))
            .unwrap();
        let top = analyser.peaks()[band];
        let silence = vec![0.0; WIDE_SAMPLES];
        // Within the hold the band drops and the cap stays.
        let mut frame = 1;
        for _ in 0..12 {
            analyser.step(&silence, start + STEP * frame);
            frame += 1;
        }
        assert!(analyser.peaks()[band] >= top - 1e-6);
        assert!(analyser.levels[band] < top, "the band itself fell");
        // Then it falls, never below the band itself.
        for _ in 0..60 {
            let levels = analyser.step(&silence, start + STEP * frame);
            frame += 1;
            assert!(analyser.peaks()[band] >= levels[band]);
        }
        assert!(analyser.peaks()[band] < top - 0.2);
    }
}
