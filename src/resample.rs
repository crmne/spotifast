//! Sample rate conversion for an output that will not take 44.1 kHz.
//!
//! Spotify audio is stereo 44.1 kHz, but many shared Windows devices run at
//! 48 kHz. rodio resets its resampler at each chunk boundary, which can cause
//! crackle. This converter preserves state across chunks.
//!
//! It uses a polyphase windowed-sinc filter and computes only the taps needed
//! for each output sample.

use std::f64::consts::PI;

/// Input samples each output sample is made from. Sixty-four give a
/// passband flat to about 19 kHz and images under the window's floor.
const TAPS: usize = 64;
/// Frames each step of the stereo dot product takes, each with its own
/// running sum per channel, so the sums do not wait on one another and
/// the compiler can keep them in vector registers on any target.
const LANES: usize = 8;

pub struct Resampler {
    up: usize,
    down: usize,
    channels: usize,
    /// `up` phases of `TAPS` coefficients each, normalised to unit gain.
    taps: Vec<f32>,
    /// Interleaved input frames still in reach: the tail of what came
    /// before, then whatever has not produced its outputs yet.
    input: Vec<f32>,
    /// The frame in `input` the next output sample sits on or just after.
    next: usize,
    /// How far past `next` the output sits, in steps of `1 / up`.
    phase: usize,
}

impl Resampler {
    /// `None` when the rates agree and nothing needs doing.
    pub fn new(from_hz: u32, to_hz: u32, channels: usize) -> Option<Self> {
        if from_hz == to_hz || from_hz == 0 || to_hz == 0 || channels == 0 {
            return None;
        }
        let divisor = gcd(from_hz, to_hz);
        let up = (to_hz / divisor) as usize;
        let down = (from_hz / divisor) as usize;
        let half = TAPS / 2;
        Some(Self {
            up,
            down,
            channels,
            taps: kernel(up, down),
            input: vec![0.0; (half - 1) * channels],
            next: half - 1,
            phase: 0,
        })
    }

    /// Converts a chunk of interleaved frames. The output is what the
    /// input so far allows; the last few frames wait for the next chunk.
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        self.input.extend_from_slice(samples);
        let half = TAPS / 2;
        let frames = self.input.len() / self.channels;
        let expected = samples.len() * self.up / self.down + self.channels;
        let mut out = Vec::with_capacity(expected);
        while self.next + half < frames {
            let taps = &self.taps[self.phase * TAPS..(self.phase + 1) * TAPS];
            let start = (self.next + 1 - half) * self.channels;
            let reach = &self.input[start..start + TAPS * self.channels];
            if self.channels == 2 {
                out.extend_from_slice(&stereo_dot(reach, taps));
            } else {
                for channel in 0..self.channels {
                    let sum: f32 = taps
                        .iter()
                        .enumerate()
                        .map(|(k, tap)| reach[k * self.channels + channel] * tap)
                        .sum();
                    out.push(sum);
                }
            }
            let position = self.phase + self.down;
            self.next += position / self.up;
            self.phase = position % self.up;
        }
        // Keep only the frames the next output still reaches back to.
        let keep_from = (self.next + 1 - half).min(frames);
        self.input.drain(..keep_from * self.channels);
        self.next -= keep_from;
        out
    }
}

/// Both channels of one output frame: `TAPS` interleaved stereo frames
/// against one phase's taps. The sums run in `LANES` parts per channel and
/// are added at the end, which is a rounding's difference (about 1e-7 of
/// full scale, under -130 dB) from adding the taps one after another.
#[inline]
fn stereo_dot(reach: &[f32], taps: &[f32]) -> [f32; 2] {
    let mut left = [0.0f32; LANES];
    let mut right = [0.0f32; LANES];
    let (frames, _) = reach.as_chunks::<2>();
    let (frames, _) = frames.as_chunks::<LANES>();
    let (taps, _) = taps.as_chunks::<LANES>();
    for (frames, taps) in frames.iter().zip(taps) {
        for (((to_left, to_right), frame), tap) in
            left.iter_mut().zip(&mut right).zip(frames).zip(taps)
        {
            *to_left += frame[0] * tap;
            *to_right += frame[1] * tap;
        }
    }
    [sum_lanes(left), sum_lanes(right)]
}

/// A pairwise sum, the same on every target.
#[inline]
fn sum_lanes(lanes: [f32; LANES]) -> f32 {
    ((lanes[0] + lanes[4]) + (lanes[2] + lanes[6]))
        + ((lanes[1] + lanes[5]) + (lanes[3] + lanes[7]))
}

/// The taps for every phase: a sinc cut just under the lower of the two
/// Nyquist limits, under a Blackman window, each phase scaled to unit
/// gain so a steady level comes out at the level it went in.
fn kernel(up: usize, down: usize) -> Vec<f32> {
    let half = (TAPS / 2) as f64;
    let cutoff = 0.475 * (up as f64 / down as f64).min(1.0);
    let mut taps = Vec::with_capacity(up * TAPS);
    for phase in 0..up {
        let offset = phase as f64 / up as f64;
        let start = taps.len();
        for k in 0..TAPS {
            let u = offset + half - 1.0 - k as f64;
            let x = u / half;
            let window = 0.42 + 0.5 * (PI * x).cos() + 0.08 * (2.0 * PI * x).cos();
            let angle = 2.0 * PI * cutoff * u;
            let sinc = if u == 0.0 { 1.0 } else { angle.sin() / angle };
            taps.push((2.0 * cutoff * sinc * window) as f32);
        }
        let sum: f32 = taps[start..].iter().sum();
        for tap in &mut taps[start..] {
            *tap /= sum;
        }
    }
    taps
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stereo tone at half scale.
    fn tone(hz: f64, rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let sample = 0.5 * (2.0 * PI * hz * i as f64 / f64::from(rate)).sin();
                [sample as f32, sample as f32]
            })
            .collect()
    }

    /// Where output frame `n` sits on the input's clock, in input frames:
    /// the filter is symmetric about its output, so there is no delay.
    fn input_time(resampler: &Resampler, n: usize) -> f64 {
        n as f64 * resampler.down as f64 / resampler.up as f64
    }

    fn check(from: u32, to: u32, tolerance: f32) {
        let mut resampler = Resampler::new(from, to, 2).unwrap();
        let out = resampler.process(&tone(1000.0, from, from as usize));
        let frames = out.len() / 2;
        assert!(
            (frames as i64 - i64::from(to)).abs() < 100,
            "{frames} frames"
        );
        let mut worst = 0.0f32;
        for n in 200..frames - 200 {
            let t = input_time(&resampler, n) / f64::from(from);
            let ideal = (0.5 * (2.0 * PI * 1000.0 * t).sin()) as f32;
            worst = worst.max((out[2 * n] - ideal).abs());
            assert_eq!(out[2 * n], out[2 * n + 1]);
        }
        assert!(worst < tolerance, "{from} to {to}: off by {worst}");
    }

    #[test]
    fn the_same_rate_needs_nothing() {
        assert!(Resampler::new(44100, 44100, 2).is_none());
    }

    #[test]
    fn a_tone_keeps_its_pitch_and_level_going_up() {
        check(44100, 48000, 0.005);
    }

    #[test]
    fn a_tone_keeps_its_pitch_and_level_going_down() {
        check(44100, 22050, 0.02);
    }

    #[test]
    fn chunking_makes_no_difference() {
        let input = tone(440.0, 44100, 20_000);
        let whole = Resampler::new(44100, 48000, 2).unwrap().process(&input);
        let mut chunked = Resampler::new(44100, 48000, 2).unwrap();
        let mut out = Vec::new();
        let mut at = 0;
        for size in [2, 14, 200, 3256, 6, 1000, 2, 8000].iter().cycle() {
            if at >= input.len() {
                break;
            }
            let end = (at + size).min(input.len());
            out.extend(chunked.process(&input[at..end]));
            at = end;
        }
        assert_eq!(out, whole);
    }

    /// The stereo path's split sums land within a rounding of the plain
    /// one-tap-after-another sum the other channel counts use.
    #[test]
    fn stereo_matches_the_plain_sum_within_a_rounding() {
        let stereo: Vec<f32> = tone(997.0, 44100, 20_000)
            .chunks(2)
            .enumerate()
            .flat_map(|(i, frame)| [frame[0], frame[1] * if i % 3 == 0 { -0.7 } else { 0.4 }])
            .collect();
        let left: Vec<f32> = stereo.iter().step_by(2).copied().collect();
        let right: Vec<f32> = stereo.iter().skip(1).step_by(2).copied().collect();
        let both = Resampler::new(44100, 48000, 2).unwrap().process(&stereo);
        let plain_left = Resampler::new(44100, 48000, 1).unwrap().process(&left);
        let plain_right = Resampler::new(44100, 48000, 1).unwrap().process(&right);
        assert_eq!(both.len(), plain_left.len() * 2);
        for (n, frame) in both.chunks(2).enumerate() {
            assert!((frame[0] - plain_left[n]).abs() < 1e-6, "left {n}");
            assert!((frame[1] - plain_right[n]).abs() < 1e-6, "right {n}");
        }
    }
}
