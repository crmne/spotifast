// This exercises real process loopback with silence, not the identity of stale PCM.
use std::{
    ffi::c_void,
    mem::size_of,
    ptr,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[repr(C, packed(2))]
struct WaveFormat {
    tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    extra_size: u16,
}

#[repr(C)]
struct WaveHeader {
    data: *mut u8,
    buffer_length: u32,
    bytes_recorded: u32,
    user: usize,
    flags: u32,
    loops: u32,
    next: *mut WaveHeader,
    reserved: usize,
}

#[link(name = "winmm")]
unsafe extern "system" {
    fn waveOutOpen(
        handle: *mut *mut c_void,
        device: u32,
        format: *const WaveFormat,
        callback: usize,
        instance: usize,
        flags: u32,
    ) -> u32;
    fn waveOutPrepareHeader(handle: *mut c_void, header: *mut WaveHeader, size: u32) -> u32;
    fn waveOutWrite(handle: *mut c_void, header: *mut WaveHeader, size: u32) -> u32;
    fn waveOutReset(handle: *mut c_void) -> u32;
    fn waveOutUnprepareHeader(handle: *mut c_void, header: *mut WaveHeader, size: u32) -> u32;
    fn waveOutClose(handle: *mut c_void) -> u32;
}

// WinMM owns neither allocation, so both remain at stable addresses until reset.
struct SilentOutput {
    handle: *mut c_void,
    header: Box<WaveHeader>,
    _samples: Box<[u8]>,
    prepared: bool,
}

impl SilentOutput {
    fn start() -> Self {
        let mut samples = vec![0; 48_000 * 2 * 2].into_boxed_slice();
        let header = Box::new(WaveHeader {
            data: samples.as_mut_ptr(),
            buffer_length: samples.len() as u32,
            bytes_recorded: 0,
            user: 0,
            flags: 0x0000_0004 | 0x0000_0008, // WHDR_BEGINLOOP | WHDR_ENDLOOP
            loops: u32::MAX,
            next: ptr::null_mut(),
            reserved: 0,
        });
        let mut output = Self {
            handle: ptr::null_mut(),
            header,
            _samples: samples,
            prepared: false,
        };
        let format = WaveFormat {
            tag: 1,
            channels: 2,
            samples_per_sec: 48_000,
            avg_bytes_per_sec: 192_000,
            block_align: 4,
            bits_per_sample: 16,
            extra_size: 0,
        };
        // SAFETY: format and output handle pointers are valid for the call.
        let status = unsafe { waveOutOpen(&mut output.handle, u32::MAX, &format, 0, 0, 0) };
        assert_eq!(status, 0, "waveOutOpen failed: {status}");
        let size = size_of::<WaveHeader>() as u32;
        // SAFETY: the boxed header and its buffer remain live until Drop.
        let status = unsafe { waveOutPrepareHeader(output.handle, &mut *output.header, size) };
        assert_eq!(status, 0, "waveOutPrepareHeader failed: {status}");
        output.prepared = true;
        // SAFETY: this header was prepared for this open device.
        let status = unsafe { waveOutWrite(output.handle, &mut *output.header, size) };
        assert_eq!(status, 0, "waveOutWrite failed: {status}");
        output
    }
}

impl Drop for SilentOutput {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: reset returns queued buffers before they are unprepared and freed.
            unsafe {
                waveOutReset(self.handle);
                if self.prepared {
                    waveOutUnprepareHeader(
                        self.handle,
                        &mut *self.header,
                        size_of::<WaveHeader>() as u32,
                    );
                }
                waveOutClose(self.handle);
            }
        }
    }
}

#[derive(Default)]
struct State {
    stop: bool,
    playing: bool,
    gate: bool,
    gated: bool,
    callbacks: usize,
    samples: usize,
    resets: usize,
    bad_format: bool,
    result: Option<i32>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn wait_for(&self, description: &str, predicate: impl Fn(&State) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut state = self.lock();
        while !predicate(&state) {
            assert!(
                state.result.is_none(),
                "capture exited while waiting for {description}: {:?}",
                state.result
            );
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for {description}");
            let (next, _) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next;
        }
    }
}

extern "C" fn control(context: *mut c_void) -> u32 {
    // SAFETY: the worker holds the Arc until the native function returns.
    let shared = unsafe { &*context.cast::<Shared>() };
    let state = shared.lock();
    u32::from(state.stop) | (u32::from(state.playing) << 1)
}

extern "C" fn process(context: *mut c_void, _data: *mut f32, count: usize, rate: u32) {
    // SAFETY: context is borrowed from the worker's live Arc.
    let shared = unsafe { &*context.cast::<Shared>() };
    let mut state = shared.lock();
    state.callbacks += 1;
    state.samples += count;
    state.bad_format |= count == 0 || rate != 48_000;
    if state.gate {
        state.gated = true;
        shared.changed.notify_all();
        // Bound the callback even if the test thread fails before releasing it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.gate && !state.stop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next, _) = shared
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next;
        }
        state.gated = false;
    }
    shared.changed.notify_all();
}

extern "C" fn event(context: *mut c_void, event: u32) {
    // SAFETY: context is borrowed from the worker's live Arc.
    let shared = unsafe { &*context.cast::<Shared>() };
    if event == 2 {
        shared.lock().resets += 1;
        shared.changed.notify_all();
    }
}

struct Capture {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Capture {
    fn start() -> Self {
        let shared = Arc::new(Shared::default());
        shared.lock().playing = true;
        let worker_shared = Arc::clone(&shared);
        let worker = thread::spawn(move || {
            // SAFETY: the Arc and callbacks remain valid until the native call returns.
            let result = unsafe {
                super::spotifast_soloist_audio(
                    std::process::id(),
                    Arc::as_ptr(&worker_shared).cast_mut().cast(),
                    control,
                    process,
                    event,
                )
            };
            worker_shared.lock().result = Some(result);
            worker_shared.changed.notify_all();
        });
        Self {
            shared,
            worker: Some(worker),
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        {
            let mut state = self.shared.lock();
            state.stop = true;
            state.gate = false;
            self.shared.changed.notify_all();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Run with a Windows playback device:
/// `cargo test --locked --no-default-features --features soloist-native-audio,demo --lib soloist::audio::native::adapter::windows_test::pause_resume_process_loopback -- --ignored --exact`
#[test]
#[ignore = "requires Windows process loopback and an available playback device"]
fn pause_resume_process_loopback() {
    let _output = SilentOutput::start();
    let capture = Capture::start();
    let shared = &capture.shared;
    shared.wait_for("initial audio samples", |state| state.samples > 0);

    for cycle in 0..5 {
        // Hold a real process callback briefly while the source continues. This
        // gives WASAPI an opportunity to queue packets before the pause.
        {
            let mut state = shared.lock();
            state.gate = true;
        }
        shared.wait_for("a gated capture callback", |state| state.gated);
        thread::sleep(Duration::from_millis(150));
        let (callbacks, resets) = {
            let mut state = shared.lock();
            assert!(state.gated, "callback gate timed out in cycle {cycle}");
            state.playing = false;
            let baseline = (state.callbacks, state.resets);
            state.gate = false;
            shared.changed.notify_all();
            baseline
        };
        shared.wait_for("pause reset event", |state| state.resets > resets);
        thread::sleep(Duration::from_millis(400));
        let samples = {
            let mut state = shared.lock();
            assert_eq!(
                state.callbacks, callbacks,
                "callback during pause in cycle {cycle}"
            );
            assert!(state.resets > resets, "reset must precede resumed samples");
            assert!(
                !state.bad_format,
                "capture returned an empty or incorrectly formatted buffer"
            );
            let samples = state.samples;
            state.playing = true;
            samples
        };
        shared.wait_for("resumed audio samples", |state| state.samples > samples);
    }
}
