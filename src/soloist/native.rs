//! Native Soloist audio adapters. The Mac controller holds source volume at
//! unity after redirecting; Windows observation follows the source volume.
//! The OS callbacks never run Rust DSP: a dedicated worker owns this state.
#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::{Bridge, State};
use std::sync::Arc;

struct Audio {
    shared: crate::eq::SharedEq,
    eq: crate::eq::Processor,
    tap: Arc<crate::vis::AudioTap>,
    limiter: crate::limiter::Limiter,
    resampler: Option<crate::resample::Resampler>,
    rate: u32,
    process_output: bool,
}
impl Audio {
    fn new(
        shared: crate::eq::SharedEq,
        tap: Arc<crate::vis::AudioTap>,
        process_output: bool,
    ) -> Self {
        Self {
            eq: crate::eq::Processor::new(shared.clone()),
            shared,
            tap,
            limiter: crate::limiter::Limiter::new(44_100.0),
            resampler: None,
            rate: 44_100,
            process_output,
        }
    }
    fn reset(&mut self, rate: u32) {
        self.rate = rate;
        self.eq = crate::eq::Processor::at_rate(self.shared.clone(), rate);
        self.limiter = crate::limiter::Limiter::new(f64::from(rate));
        self.resampler = crate::resample::Resampler::new(rate, 44_100, 2);
        self.tap.clear();
    }
    fn process(&mut self, samples: &mut [f32], rate: u32, volume: u8) {
        if !(8_000..=192_000).contains(&rate) || !samples.len().is_multiple_of(2) {
            samples.fill(0.0);
            return;
        }
        if self.rate != rate {
            self.reset(rate);
        }
        let mut output: Vec<f64> = samples
            .iter()
            .map(|v| if v.is_finite() { f64::from(*v) } else { 0.0 })
            .collect();
        if self.process_output {
            self.eq.process(&mut output);
        }
        // Only the visualization copy is resampled. The Mac output stays at
        // the tap's native rate. Windows never modifies the audible signal.
        if let Some(resampler) = &mut self.resampler {
            let copy: Vec<f32> = output.iter().map(|v| *v as f32).collect();
            let copy: Vec<f64> = resampler
                .process(&copy)
                .into_iter()
                .map(f64::from)
                .collect();
            self.tap.push(&copy, 1.0);
        } else {
            self.tap.push(&output, 1.0);
        }
        if self.process_output {
            self.limiter.process(&mut output, 1.0);
            // Apply volume after the tap and limiter, including held audio.
            // Cubic gain matches the built-in player and PipeWire route.
            let gain = (f64::from(volume.min(100)) / 100.0).powi(3);
            for (sample, output) in samples.iter_mut().zip(output) {
                *sample = (output * gain) as f32;
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod adapter {
    use super::*;
    use std::{cell::UnsafeCell, ffi::c_void, sync::atomic::Ordering, time::Duration};
    struct Context {
        state: Arc<State>,
        audio: UnsafeCell<Audio>,
    }
    // The native function borrows Context until it returns. Only its worker
    // calls process/event, sequentially; real-time callbacks only call control.
    unsafe extern "C" {
        fn spotifast_soloist_audio(
            pid: u32,
            context: *mut c_void,
            control: extern "C" fn(*mut c_void) -> u32,
            process: extern "C" fn(*mut c_void, *mut f32, usize, u32),
            event: extern "C" fn(*mut c_void, u32),
        ) -> i32;
    }
    extern "C" fn control(context: *mut c_void) -> u32 {
        // SAFETY: the native worker joins its callbacks before returning.
        let state = unsafe { &(*context.cast::<Context>()).state };
        u32::from(state.stop.load(Ordering::Acquire))
            | (u32::from(state.playing.load(Ordering::Acquire)) << 1)
    }
    extern "C" fn process(context: *mut c_void, data: *mut f32, count: usize, rate: u32) {
        // SAFETY: exclusive buffer for this call; Audio has one worker owner.
        let context = unsafe { &*context.cast::<Context>() };
        let samples = unsafe { std::slice::from_raw_parts_mut(data, count) };
        let audio = unsafe { &mut *context.audio.get() };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(target_os = "macos")]
            let volume = context.state.output_volume.percent();
            #[cfg(not(target_os = "macos"))]
            let volume = 100;
            audio.process(samples, rate, volume)
        }))
        .is_err()
        {
            samples.fill(0.0);
            context.state.failed.store(true, Ordering::Release);
        }
    }
    extern "C" fn event(context: *mut c_void, event: u32) {
        // SAFETY: events and processing run serially on the native worker.
        let context = unsafe { &*context.cast::<Context>() };
        match event {
            1 => {
                context.state.routed.store(true, Ordering::Release);
                log::info!("Soloist native audio capture is ready");
            }
            2 => {
                let audio = unsafe { &mut *context.audio.get() };
                audio.reset(audio.rate);
            }
            3 => {
                context.state.failed.store(true, Ordering::Release);
            }
            _ => {}
        }
    }
    pub fn open(
        config: crate::player::EngineConfig,
        output_volume: super::super::OutputVolume,
    ) -> Option<Bridge> {
        let state = State::new();
        #[cfg(target_os = "macos")]
        let state = State {
            output_volume,
            ..state
        };
        #[cfg(not(target_os = "macos"))]
        let _ = output_volume;
        let state = Arc::new(state);
        let worker = state.clone();
        std::thread::Builder::new().name("soloist-native-audio".into()).spawn(move || {
            while worker.child.load(Ordering::Acquire) == 0 && !worker.stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(20));
            }
            if worker.stop.load(Ordering::Acquire) { return; }
            let mut context = Context {
                audio: UnsafeCell::new(Audio::new(config.eq, config.tap, cfg!(target_os = "macos"))),
                state: worker.clone(),
            };
            // SAFETY: Context stays alive until every native callback ends.
            let status = unsafe { spotifast_soloist_audio(worker.child.load(Ordering::Acquire),
                (&mut context as *mut Context).cast(), control, process, event) };
            worker.routed.store(false, Ordering::Release);
            // Errors before the Mac source is muted, and all Windows capture
            // errors, leave ordinary Soloist playback alone. The muted Mac
            // failure path sends event 3 and waits for child shutdown instead.
            if status != 0 && !worker.stop.load(Ordering::Acquire) {
                log::warn!("Soloist native audio capture unavailable (OS code {status}); using its ordinary output");
            }
            // No visual samples from the old child survive a reconnect.
            unsafe { &*context.audio.get() }.tap.clear();
        }).ok()?;
        Some(Bridge { state })
    }
}
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) use adapter::open;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_eq_shapes_the_tap_and_output_but_windows_only_observes() {
        for processing in [false, true] {
            let eq = crate::eq::shared();
            {
                let mut settings = eq.lock().unwrap();
                settings.on = true;
                settings.preamp_db = 6.0;
            }
            let tap = crate::vis::AudioTap::new();
            let mut audio = Audio::new(eq, tap.clone(), processing);
            let mut samples = vec![0.1; 4410];
            audio.process(&mut samples, 44_100, 100);
            let expected = if processing {
                0.1 * 10_f32.powf(6.0 / 20.0)
            } else {
                0.1
            };
            assert!((tap.window(32, 0)[31] - expected).abs() < 0.0001);
            assert!((samples[samples.len() - 1] - expected).abs() < 0.0001);
        }
    }
    #[test]
    fn mac_output_fader_leaves_eq_visuals_unchanged_even_at_zero() {
        for rate in [44_100, 48_000] {
            let eq = crate::eq::shared();
            {
                let mut settings = eq.lock().unwrap();
                settings.on = true;
                settings.preamp_db = 6.0;
            }
            let tap = crate::vis::AudioTap::new();
            let mut audio = Audio::new(eq, tap.clone(), true);
            let expected = 0.1 * 10_f32.powf(6.0 / 20.0);
            // Changes use the same live processor, including its held samples.
            for volume in [100, 50, 0, 25, 100] {
                let mut samples = vec![0.1; rate as usize / 5];
                audio.process(&mut samples, rate, volume);
                assert!(
                    tap.window(256, 0)
                        .iter()
                        .all(|v| (*v - expected).abs() < 0.0001)
                );
                let gain = (f32::from(volume) / 100.0).powi(3);
                assert!((samples[samples.len() - 1] - expected * gain).abs() < 0.0001);
                if volume == 0 {
                    assert!(samples.iter().all(|v| *v == 0.0));
                }
            }
        }
    }
    #[test]
    fn windows_observer_does_not_pretend_to_have_an_output_fader() {
        let tap = crate::vis::AudioTap::new();
        let mut audio = Audio::new(crate::eq::shared(), tap.clone(), false);
        let mut samples = vec![0.1; 4410];
        audio.process(&mut samples, 44_100, 0);
        assert!(samples.iter().all(|v| *v == 0.1));
        assert!(tap.window(100, 0).iter().all(|v| *v == 0.1));
    }
    #[test]
    fn pause_and_format_changes_clear_old_audio() {
        let tap = crate::vis::AudioTap::new();
        let mut audio = Audio::new(crate::eq::shared(), tap.clone(), true);
        audio.process(&mut vec![0.25; 4800], 48_000, 100);
        assert!(tap.window(100, 0).iter().any(|v| *v > 0.1));
        audio.reset(48_000);
        audio.process(&mut vec![0.0; 4800], 48_000, 100);
        assert!(tap.window(100, 0).iter().all(|v| *v == 0.0));
        let mut silence = vec![0.0; 4410];
        audio.process(&mut silence, 44_100, 100);
        assert!(silence.iter().all(|v| *v == 0.0));
        assert!(tap.window(100, 0).iter().all(|v| *v == 0.0));
    }
    #[test]
    fn native_capture_never_invents_audio_when_the_source_is_muted() {
        let tap = crate::vis::AudioTap::new();
        let mut audio = Audio::new(crate::eq::shared(), tap.clone(), false);
        audio.process(&mut vec![0.0; 4800], 48_000, 100);
        assert!(tap.window(100, 0).iter().all(|v| *v == 0.0));
        let mut invalid = vec![f32::NAN; 200];
        audio.process(&mut invalid, 1, 100);
        assert!(invalid.iter().all(|v| *v == 0.0));
    }
}
