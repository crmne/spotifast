//! The MilkDrop child with no window: it draws into a framebuffer of its own
//! and publishes each finished frame to shared memory, for the app to show in
//! a skin's pane.
//!
//! On Linux the context comes straight from a graphics device through EGL, so
//! no display server is involved and nothing can appear on screen. macOS and
//! Windows have no such thing in glutin; there the context lives on a window
//! that is never shown, on an event loop of its own.
//!
//! Nobody has to stop it: it leaves when its stdin closes (the app is gone or
//! done with it) and when the picture has not been shown for a while.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::Presets;
use super::child::{Args, prepare_frame};
use super::engine::Engine;
use super::frames::Frames;
use super::shm::Ring;

/// How long the app may go without showing the picture before the child
/// leaves by itself. Longer than the app's own wait before it stops the child
/// ([`super::pane`]), so this only matters when the app is stuck or minimised.
const UNWATCHED: Duration = Duration::from_secs(3);

/// How often a held picture looks to see if it may go on.
const FROZEN_LOOK: Duration = Duration::from_millis(50);

/// Exit code for a machine this cannot draw on, so the app knows to stay
/// with the bars.
pub(super) const NO_GRAPHICS: i32 = 3;

/// The work of one frame, apart from whatever holds the context.
struct Pump {
    ring: Ring,
    cursor: u64,
    frames: Frames,
    folder: PathBuf,
    presets: Presets,
    closing: Arc<AtomicBool>,
    beats: u64,
    watched: Instant,
    due: Instant,
    /// Rate and preset time from the command line, until the app sets them.
    fps: u32,
    seconds: u32,
}

impl Pump {
    fn new(args: &Args, ring: Ring, frames: Frames, closing: Arc<AtomicBool>) -> Self {
        Self {
            ring,
            cursor: 0,
            frames,
            folder: args.presets.clone(),
            presets: Presets::new(),
            closing,
            beats: 0,
            watched: Instant::now(),
            due: Instant::now(),
            fps: args.fps,
            seconds: args.seconds,
        }
    }

    /// Whether to go on: the app is there and someone is looking.
    fn going(&mut self, now: Instant) -> bool {
        let beats = self.frames.beats();
        // A held picture needs no painting, and the app may sleep over it.
        if beats != self.beats || self.frames.frozen() {
            self.beats = beats;
            self.watched = now;
        }
        !self.closing.load(Ordering::Relaxed)
            && now.saturating_duration_since(self.watched) < UNWATCHED
    }

    fn frame(&mut self, engine: &mut Engine) {
        if self.frames.frozen() {
            self.due = Instant::now() + FROZEN_LOOK;
            return;
        }
        let (fps, seconds) = self.frames.paced();
        if fps != 0 {
            self.fps = fps;
        }
        if seconds != 0 {
            self.seconds = seconds;
        }
        let steps = self.frames.take_steps();
        for _ in 0..steps.abs().min(16) {
            if steps > 0 {
                self.presets.next(false);
            } else {
                self.presets.previous();
            }
        }
        // The first preset cuts in: the pane has the bars until it does, so
        // there is no idle picture to fade from.
        self.presets.refresh(&self.folder, None);
        if self.presets.current().is_none() && self.presets.count() > 0 {
            self.presets.next(true);
        }
        prepare_frame(
            engine,
            &mut self.presets,
            &self.folder,
            self.seconds,
            &self.ring,
            &mut self.cursor,
        );
        // Nothing to draw for until the app has measured its pane, and
        // nothing worth showing before a preset plays.
        if let Some(size) = self.frames.wanted()
            && self.presets.current().is_some()
        {
            self.frames
                .publish(size, |pixels| engine.render_into(size[0], size[1], pixels));
        }
        let interval = Duration::from_secs_f32(1.0 / self.fps.max(1) as f32);
        let now = Instant::now();
        self.due = (self.due + interval).max(now);
    }
}

/// Runs the child to the end; returns its exit code.
pub fn run(args: Args, ring: Ring) -> i32 {
    let Some(path) = &args.frames else {
        return 1;
    };
    let frames = match Frames::open(path) {
        Ok(frames) => frames,
        Err(error) => {
            eprintln!("MilkDrop: no frame buffer: {error}");
            return 1;
        }
    };
    // End of input means the app has let go.
    let closing = Arc::new(AtomicBool::new(false));
    {
        let closing = Arc::clone(&closing);
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut sink);
            closing.store(true, Ordering::Relaxed);
        });
    }
    let pump = Pump::new(&args, ring, frames, closing);
    drive(args, pump)
}

/// A context on a graphics device, with nothing to present to, and projectM
/// in it.
#[cfg(target_os = "linux")]
fn drive(args: Args, mut pump: Pump) -> i32 {
    let Some(context) = device_context() else {
        eprintln!("MilkDrop: no graphics device to draw on without a window");
        return NO_GRAPHICS;
    };
    let textures = args.presets.join("textures");
    let dirs = [args.presets.as_path(), textures.as_path()];
    let gl = Arc::clone(&context.gl);
    let Some(mut engine) = Engine::new(gl, &dirs, args.seconds) else {
        return NO_GRAPHICS;
    };
    while pump.going(Instant::now()) {
        pump.frame(&mut engine);
        let wait = pump.due.saturating_duration_since(Instant::now());
        std::thread::sleep(wait);
    }
    // The engine frees projectM with GL calls while the context is current.
    drop(engine);
    drop(context);
    0
}

/// The OpenGL context and what keeps it alive.
#[cfg(target_os = "linux")]
struct DeviceContext {
    gl: Arc<eframe::glow::Context>,
    _context: glutin::api::egl::context::PossiblyCurrentContext,
}

/// Tries every graphics device, real ones first, for an OpenGL context of
/// the version projectM needs.
#[cfg(target_os = "linux")]
fn device_context() -> Option<DeviceContext> {
    use glutin::api::egl::device::Device;
    let mut devices: Vec<Device> = Device::query_devices().ok()?.collect();
    devices.sort_by_key(|device| device.extensions().contains("EGL_MESA_device_software"));
    devices.iter().find_map(|device| {
        on_device(device)
            .map_err(|error| eprintln!("MilkDrop: a device refused: {error}"))
            .ok()
    })
}

#[cfg(target_os = "linux")]
fn on_device(device: &glutin::api::egl::device::Device) -> Result<DeviceContext, String> {
    use glutin::config::{Api, ConfigSurfaceTypes, ConfigTemplateBuilder};
    use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
    use glutin::display::GlDisplay;

    // SAFETY: no display handle is passed, which is what makes this a device
    // display and needs no display server.
    let display = unsafe { glutin::api::egl::display::Display::with_device(device, None) }
        .map_err(|error| error.to_string())?;
    let template = ConfigTemplateBuilder::new()
        .with_api(Api::OPENGL)
        .with_surface_type(ConfigSurfaceTypes::empty())
        .build();
    // SAFETY: the template holds no window to outlive.
    let config = unsafe { display.find_configs(template) }
        .map_err(|error| error.to_string())?
        .next()
        .ok_or("no OpenGL configuration")?;
    let attributes = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
        .build(None);
    // SAFETY: nothing is shared and no window is named.
    let context = unsafe { display.create_context(&config, &attributes) }
        .map_err(|error| error.to_string())?
        .make_current_surfaceless()
        .map_err(|error| error.to_string())?;
    // SAFETY: the context is current on this thread.
    let gl = Arc::new(unsafe {
        eframe::glow::Context::from_loader_function_cstr(|symbol| display.get_proc_address(symbol))
    });
    Ok(DeviceContext {
        gl,
        _context: context,
    })
}

/// A window that never shows holds the context, on an event loop that only
/// keeps the frames' time.
#[cfg(not(target_os = "linux"))]
fn drive(args: Args, pump: Pump) -> i32 {
    use winit::event_loop::EventLoop;

    let event_loop = {
        let mut builder = EventLoop::builder();
        // No icon in the Dock for a window nobody sees.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder.with_activation_policy(ActivationPolicy::Prohibited);
        }
        builder.build()
    };
    let event_loop = match event_loop {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("MilkDrop: no event loop: {error}");
            return NO_GRAPHICS;
        }
    };
    let mut hidden = Hidden {
        args,
        pump,
        live: None,
        code: 0,
    };
    let _ = event_loop.run_app(&mut hidden);
    hidden.code
}

#[cfg(not(target_os = "linux"))]
struct Hidden {
    args: Args,
    pump: Pump,
    live: Option<super::child::Live>,
    code: i32,
}

#[cfg(not(target_os = "linux"))]
impl winit::application::ApplicationHandler for Hidden {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        match super::child::build(event_loop, &self.args, self.args.seconds) {
            Ok(live) => self.live = Some(live),
            Err(error) => {
                eprintln!("MilkDrop: {error}");
                self.code = NO_GRAPHICS;
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &winit::event_loop::ActiveEventLoop,
        _id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
    }

    fn about_to_wait(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        use winit::event_loop::ControlFlow;
        let Some(live) = &mut self.live else {
            return;
        };
        let now = Instant::now();
        if !self.pump.going(now) {
            event_loop.exit();
            return;
        }
        if now >= self.pump.due {
            self.pump.frame(&mut live.engine);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.pump.due));
    }

    fn exiting(&mut self, _event_loop: &winit::event_loop::ActiveEventLoop) {
        self.live = None;
    }
}
