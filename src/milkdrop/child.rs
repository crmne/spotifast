//! MilkDrop child process with its own window, OpenGL context, and event loop.
//!
//! The app starts this binary with `--milkdrop-child` because winit permits one
//! event loop per process. The child reads audio from shared memory, receives
//! JSON-line commands on stdin, and reports window state on stdout. projectM
//! resources stay isolated from the app's windows.

use std::io::{BufRead, Write};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::glow;
use glutin::config::ConfigTemplateBuilder;
use glutin::context::{ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use glutin_winit::DisplayBuilder;
use raw_window_handle::HasWindowHandle;
use serde::{Deserialize, Serialize};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, ResizeDirection, Window, WindowId};

use super::engine::Engine;
use super::overlay::{Backing, Overlay, Place, Row, Span};
use super::shm::Ring;
use super::{DEFAULT_FPS, DEFAULT_SECONDS, LAG, MIN_SIZE, Presets, Request};

/// What the child is told to do, on stdin, as one JSON object per line.
#[derive(Debug, Default, Deserialize)]
struct Control {
    fps: Option<u32>,
    seconds: Option<u32>,
    scale: Option<u32>,
    /// The playing song, as lines to overlay when it changes.
    song: Option<Vec<String>>,
    close: Option<bool>,
}

/// What the child reports back, on stdout, as one JSON object per line.
#[derive(Debug, Default, Serialize)]
struct Event {
    #[serde(skip_serializing_if = "Option::is_none")]
    closed: Option<bool>,
    /// What the window asks the app to do with the player.
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    /// How often the screen it opened on refreshes.
    #[serde(skip_serializing_if = "Option::is_none")]
    screen_hz: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pos: Option<[f32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<[f32; 2]>,
}

/// How the child was started, from the command line.
pub struct Args {
    pub shm: PathBuf,
    pub presets: PathBuf,
    pub size: [f32; 2],
    pub pos: Option<[f32; 2]>,
    pub fullscreen: bool,
    pub fps: u32,
    pub seconds: u32,
    pub scale: u32,
}

impl Args {
    /// Reads the child's arguments from the command line; `None` when this is
    /// not a child process.
    pub fn parse() -> Option<Self> {
        let mut all = std::env::args().skip(1);
        if !std::env::args().any(|arg| arg == "--milkdrop-child") {
            return None;
        }
        let mut shm = None;
        let mut presets = None;
        let mut size = super::DEFAULT_SIZE;
        let mut pos = None;
        let mut fullscreen = false;
        let mut fps = DEFAULT_FPS;
        let mut seconds = DEFAULT_SECONDS;
        let mut scale = 1u32;
        while let Some(arg) = all.next() {
            match arg.as_str() {
                "--milkdrop-shm" => shm = all.next().map(PathBuf::from),
                "--milkdrop-presets" => presets = all.next().map(PathBuf::from),
                "--milkdrop-size" => {
                    if let Some(pair) = all.next().and_then(|value| parse_pair(&value)) {
                        size = pair;
                    }
                }
                "--milkdrop-pos" => pos = all.next().and_then(|value| parse_pair(&value)),
                "--milkdrop-fullscreen" => fullscreen = true,
                "--milkdrop-fps" => fps = all.next().and_then(|v| v.parse().ok()).unwrap_or(fps),
                "--milkdrop-seconds" => {
                    seconds = all.next().and_then(|v| v.parse().ok()).unwrap_or(seconds);
                }
                "--milkdrop-scale" => {
                    scale = all.next().and_then(|v| v.parse().ok()).unwrap_or(scale);
                }
                _ => {}
            }
        }
        Some(Self {
            shm: shm?,
            presets: presets?,
            size,
            pos,
            fullscreen,
            fps,
            seconds,
            scale,
        })
    }
}

fn parse_pair(value: &str) -> Option<[f32; 2]> {
    let (a, b) = value.split_once([',', 'x'])?;
    Some([a.trim().parse().ok()?, b.trim().parse().ok()?])
}

/// Runs the child process to the end; returns its exit code.
pub fn run(args: Args) -> i32 {
    let ring = match Ring::open(&args.shm) {
        Ok(ring) => ring,
        Err(error) => {
            eprintln!("MilkDrop: no audio ring: {error}");
            return 1;
        }
    };
    let event_loop = match EventLoop::<Control>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("MilkDrop: no event loop: {error}");
            return 1;
        }
    };
    // A thread reads control lines from the app and wakes the loop with them;
    // end of input (the app is gone) closes the window.
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let control: Control = serde_json::from_str(&line).unwrap_or_default();
            if proxy.send_event(control).is_err() {
                break;
            }
        }
        let _ = proxy.send_event(Control {
            close: Some(true),
            ..Default::default()
        });
    });

    let mut app = Child::new(args, ring);
    let _ = event_loop.run_app(&mut app);
    0
}

/// The live window and everything on its graphics context. Fields drop in
/// order and the engine frees projectM with GL calls, so it comes first.
struct Live {
    engine: Engine,
    gl: Arc<glow::Context>,
    overlay: Option<Overlay>,
    /// What is switched on to stay, each in a corner of its own so that
    /// none of them can cover another: the frame rate, the song, the
    /// preset's name. None of these fade.
    corners: [Option<Overlay>; 3],
    context: PossiblyCurrentContext,
    surface: Surface<WindowSurface>,
    window: Window,
    fullscreen: bool,
}

struct Child {
    args: Args,
    ring: Ring,
    cursor: u64,
    presets: Presets,
    live: Option<Live>,
    song: Option<Vec<String>>,
    /// When the last frames were drawn, for the count D shows.
    drawn: Vec<Instant>,
    /// Whether the keys are what is on show, so the same key hides them.
    showing_keys: bool,
    /// Song-title display mode.
    song_shown: SongShown,
    preset_on: bool,
    fps_on: bool,
    /// Cached corner text and its last render time.
    corner_lines: [Vec<String>; 3],
    corner_written: [Option<Instant>; 3],
    modifiers: winit::keyboard::ModifiersState,
    fps: u32,
    /// Current screen refresh rate, when reported.
    screen_hz: Option<f32>,
    scale: u32,
    seconds: u32,
    pointer: PhysicalPosition<f64>,
    last_click: Option<Instant>,
    next_frame: Instant,
    reported: Option<([f32; 2], [f32; 2])>,
    /// The screen the window was last on: on a Mac that switches graphics
    /// by itself, another screen can mean another GPU.
    #[cfg(target_os = "macos")]
    screen: Option<winit::monitor::MonitorHandle>,
}

impl Child {
    fn new(args: Args, ring: Ring) -> Self {
        let fps = args.fps;
        let scale = args.scale;
        let seconds = args.seconds;
        Self {
            args,
            ring,
            cursor: 0,
            presets: Presets::new(),
            live: None,
            song: None,
            drawn: Vec::new(),
            showing_keys: false,
            song_shown: SongShown::OnChange,
            preset_on: false,
            fps_on: false,
            corner_lines: [Vec::new(), Vec::new(), Vec::new()],
            corner_written: [None; 3],
            screen_hz: None,
            modifiers: winit::keyboard::ModifiersState::empty(),
            fps,
            scale,
            seconds,
            pointer: PhysicalPosition::new(0.0, 0.0),
            last_click: None,
            next_frame: Instant::now(),
            reported: None,
            #[cfg(target_os = "macos")]
            screen: None,
        }
    }

    /// Schedules from the previous deadline so render time does not reduce FPS.
    fn schedule_next_frame(&mut self) {
        self.schedule_next_frame_at(Instant::now());
    }

    /// The same, told what the time is.
    ///
    /// Accepts `now` so tests can advance time without sleeping.
    fn schedule_next_frame_at(&mut self, now: Instant) {
        let Some(interval) = self.frame_interval() else {
            self.next_frame = now;
            return;
        };
        self.next_frame += interval;
        if self.next_frame <= now {
            // Slower than the limit asks for, or back from a pause: take
            // the rhythm up from here rather than chasing frames whose
            // moment has gone.
            self.next_frame = now + interval;
        }
    }

    /// How long a frame is meant to take: the screen's own rhythm, a
    /// number of frames a second, or nothing at all when uncapped.
    fn frame_interval(&self) -> Option<Duration> {
        (self.fps != 0).then(|| Duration::from_secs_f32(1.0 / self.fps as f32))
    }

    /// Asks the screen the window is on how often it refreshes.
    fn read_screen_hz(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        let hz = live
            .window
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .map(|millihertz| millihertz as f32 / 1000.0);
        if hz != self.screen_hz {
            self.screen_hz = hz;
            if let Some(hz) = hz {
                report(&Event {
                    screen_hz: Some(hz.round() as u32),
                    ..Default::default()
                });
            }
        }
    }

    fn create(&mut self, event_loop: &ActiveEventLoop) {
        match build(event_loop, &self.args, self.seconds) {
            Ok(live) => {
                live.window.request_redraw();
                self.live = Some(live);
                self.read_screen_hz();
            }
            Err(error) => {
                eprintln!("MilkDrop: {error}");
                report(&Event {
                    closed: Some(true),
                    ..Default::default()
                });
                event_loop.exit();
            }
        }
    }

    fn set_fullscreen(&mut self, on: bool) {
        if let Some(live) = &mut self.live {
            live.window
                .set_fullscreen(on.then_some(Fullscreen::Borderless(None)));
            live.fullscreen = on;
        }
    }

    fn close(&mut self, event_loop: &ActiveEventLoop) {
        report(&Event {
            closed: Some(true),
            ..Default::default()
        });
        self.live = None;
        event_loop.exit();
    }

    fn render(&mut self) {
        let Some(live) = &mut self.live else {
            return;
        };
        let folder = self.args.presets.clone();
        self.presets.refresh(&folder);
        if self.presets.current().is_none() && self.presets.count() > 0 {
            self.presets.next(false);
        }
        if let Some(hard) = live.engine.switch_wanted()
            && !self.presets.locked
        {
            self.presets.next(hard);
        }
        live.engine.set_seconds(self.seconds);
        live.engine.set_locked(self.presets.locked);
        if let Some(Request::Load { path, smooth }) = self.presets.take_request() {
            live.engine.load(&path, smooth);
        }
        let frames = self.ring.since(&mut self.cursor, LAG);
        live.engine.feed_frames(&frames);
        let size = live.window.inner_size();
        live.engine.render(
            0,
            0,
            size.width.max(1),
            size.height.max(1),
            self.scale.max(1),
        );
        if let Some(overlay) = &mut live.overlay {
            overlay.draw(&live.gl, (size.width, size.height));
        }
        for corner in live.corners.iter_mut().flatten() {
            corner.draw(&live.gl, (size.width, size.height));
        }
        if self.drawn.len() >= 60 {
            self.drawn.remove(0);
        }
        self.drawn.push(Instant::now());
        if let Err(error) = live.surface.swap_buffers(&live.context) {
            eprintln!("MilkDrop: present failed: {error}");
        }
        self.update_corners();
        self.report_geometry();
    }

    /// Tells the app where the window is, when it has moved or resized.
    fn report_geometry(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        let scale = live.window.scale_factor();
        let size = live.window.inner_size().to_logical::<f32>(scale);
        let size = [size.width, size.height];
        let pos = live
            .window
            .outer_position()
            .map(|pos| {
                let pos = pos.to_logical::<f32>(scale);
                [pos.x, pos.y]
            })
            .unwrap_or([0.0, 0.0]);
        if self.reported != Some((pos, size)) {
            self.reported = Some((pos, size));
            // Moved or resized: it may be on another screen now, and
            // screens do not all refresh at the same rate.
            self.read_screen_hz();
            report(&Event {
                pos: Some(pos),
                size: Some(size),
                ..Default::default()
            });
        }
    }

    fn over_grip(&self) -> bool {
        let Some(live) = &self.live else {
            return false;
        };
        let size = live.window.inner_size();
        self.pointer.x >= size.width as f64 - 16.0 && self.pointer.y >= size.height as f64 - 16.0
    }

    /// With automatic graphics switching, a window moved to another screen
    /// can land on another GPU, and Apple asks that a context managed by
    /// hand be updated before it draws there; glutin's resize is that
    /// update. Only a change of screen asks for it, not every step of a drag.
    #[cfg(target_os = "macos")]
    fn follow_screen(&mut self) {
        let Some(live) = &self.live else {
            return;
        };
        let screen = live.window.current_monitor();
        if screen == self.screen {
            return;
        }
        self.screen = screen;
        let size = live.window.inner_size();
        if let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        {
            live.surface.resize(&live.context, width, height);
            live.window.request_redraw();
        }
    }
}

impl ApplicationHandler<Control> for Child {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_none() {
            self.create(event_loop);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, control: Control) {
        if let Some(scale) = control.scale {
            self.scale = scale.clamp(1, 4);
        }
        if let Some(song) = control.song
            && self.song.as_ref() != Some(&song)
        {
            self.song = Some(song);
            // The card in the middle is the announcement; the corner is
            // the reminder. One or the other, never both.
            if self.song_shown == SongShown::OnChange {
                self.show_song();
            }
        }
        if let Some(fps) = control.fps {
            self.fps = fps;
        }
        if let Some(seconds) = control.seconds {
            self.seconds = seconds;
        }
        if control.close == Some(true) {
            self.close(event_loop);
        } else if let Some(live) = &self.live {
            live.window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.close(event_loop),
            WindowEvent::Resized(size) => {
                if let Some(live) = &self.live
                    && let (Some(width), Some(height)) =
                        (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    live.surface.resize(&live.context, width, height);
                    live.window.request_redraw();
                }
            }
            #[cfg(target_os = "macos")]
            WindowEvent::Moved(_) => self.follow_screen(),
            WindowEvent::CursorMoved { position, .. } => self.pointer = position,
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => self.on_press(button),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.on_key(event.logical_key, event_loop);
            }
            WindowEvent::RedrawRequested => {
                self.render();
                self.schedule_next_frame();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(live) = &self.live else {
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
            return;
        };
        match self.frame_interval() {
            None => {
                live.window.request_redraw();
                event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
            }
            Some(_) => {
                if Instant::now() >= self.next_frame {
                    live.window.request_redraw();
                } else {
                    event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                        self.next_frame,
                    ));
                }
            }
        }
    }
}

impl Child {
    fn on_press(&mut self, button: MouseButton) {
        let Some(live) = &self.live else {
            return;
        };
        match button {
            MouseButton::Left => {
                if self.over_grip() {
                    let _ = live.window.drag_resize_window(ResizeDirection::SouthEast);
                    return;
                }
                let now = Instant::now();
                let double = self
                    .last_click
                    .is_some_and(|last| now.duration_since(last) < Duration::from_millis(350));
                if double {
                    self.last_click = None;
                    let on = !live.fullscreen;
                    self.set_fullscreen(on);
                } else {
                    self.last_click = Some(now);
                    let _ = live.window.drag_window();
                }
            }
            MouseButton::Right => self.presets.next(false),
            _ => {}
        }
    }

    fn on_key(&mut self, key: Key, event_loop: &ActiveEventLoop) {
        let fullscreen = self.live.as_ref().is_some_and(|live| live.fullscreen);
        // Control (Command on the Mac) is what the app's own playback
        // shortcuts are held with; a plain key is the window's own.
        let control = self.modifiers.control_key() || self.modifiers.super_key();
        let plain = !control && !self.modifiers.alt_key();
        match key.as_ref() {
            Key::Named(NamedKey::Escape) => {
                if fullscreen {
                    self.set_fullscreen(false);
                } else {
                    self.close(event_loop);
                }
            }
            // Plain keys control presets; modified keys keep the app's playback
            // bindings.
            Key::Named(NamedKey::ArrowRight) if plain => self.presets.next(false),
            Key::Named(NamedKey::ArrowLeft) if plain => self.presets.previous(),
            Key::Character("n") | Key::Character("N") if plain => self.presets.next(false),
            Key::Character("p") | Key::Character("P") if plain => self.presets.previous(),
            Key::Character("h") | Key::Character("H") if plain => self.presets.next(true),
            Key::Character("l") | Key::Character("L") if plain => {
                self.presets.locked = !self.presets.locked;
                let note = if self.presets.locked {
                    "Preset kept"
                } else {
                    "Preset free again"
                };
                self.show_note(note.into());
            }
            Key::Character("r") | Key::Character("R") if plain => {
                let note = if self.presets.toggle_order() {
                    "Random order"
                } else {
                    "Folder order"
                };
                self.show_note(note.into());
            }
            // Playback, in the app's own bindings.
            Key::Named(NamedKey::Space) if plain => command("play-pause"),
            Key::Named(NamedKey::ArrowLeft) if control => command("previous"),
            Key::Named(NamedKey::ArrowRight) if control => command("next"),
            Key::Named(NamedKey::ArrowUp) if control => command("volume-up"),
            Key::Named(NamedKey::ArrowDown) if control => command("volume-down"),
            Key::Character("m") | Key::Character("M") if plain => command("mute"),
            Key::Character("b") | Key::Character("B") if plain => command("save-toggle"),
            Key::Character("s") | Key::Character("S") if plain => command("shuffle"),
            // The window itself.
            Key::Named(NamedKey::Enter) if self.modifiers.alt_key() => {
                self.set_fullscreen(!fullscreen)
            }
            Key::Character("f") | Key::Character("F") if plain => self.set_fullscreen(!fullscreen),
            // What it can tell you.
            Key::Character("?") | Key::Named(NamedKey::F1) => self.show_keys(),
            Key::Character("i") | Key::Character("I") if plain => self.cycle_song(),
            Key::Character("t") | Key::Character("T") if plain => {
                self.toggle_status(Status::Preset)
            }
            Key::Character("d") | Key::Character("D") if plain => self.toggle_status(Status::Fps),
            _ => {}
        }
    }

    /// Every key this window answers, in two columns over the picture.
    /// The list is the window's own bindings: what is here is what works.
    fn show_keys(&mut self) {
        let Some(live) = &mut self.live else {
            return;
        };
        let Some(overlay) = &mut live.overlay else {
            return;
        };
        if overlay.showing() && self.showing_keys {
            // The same key that opened it puts it away again.
            overlay.hide();
            self.showing_keys = false;
            return;
        }
        let heading = |text: &str| Row::Heading(Span::new(text, 12.0).weight(700.0).tint(0.62));
        let keys = |key: &str, does: &str| Row::Keys {
            key: Span::new(key, 14.0).weight(600.0),
            does: Span::new(does, 14.0).tint(0.86),
        };
        let rows = [
            Row::Line(Span::new("MilkDrop", 22.0).weight(700.0)),
            Row::Gap(10.0),
            heading("PRESETS"),
            Row::Gap(3.0),
            keys("\u{2192}  or  N", "Next preset"),
            keys("\u{2190}  or  P", "Previous preset"),
            keys("H", "Next preset, cut on the beat"),
            keys("L", "Keep this preset"),
            keys("R", "Random or folder order"),
            keys("Right-click", "Next preset"),
            Row::Gap(9.0),
            heading("PLAYBACK"),
            Row::Gap(3.0),
            keys("Space", "Play or pause"),
            keys("Ctrl+\u{2190}  /  Ctrl+\u{2192}", "Previous or next song"),
            keys("Ctrl+\u{2191}  /  Ctrl+\u{2193}", "Volume up or down"),
            keys("M", "Mute or unmute"),
            keys("B", "Like or unlike the playing song"),
            keys("S", "Shuffle"),
            Row::Gap(9.0),
            heading("WINDOW"),
            Row::Gap(3.0),
            keys("F, Alt+Enter, double-click", "Full screen"),
            keys("Esc", "Leave full screen, or close"),
            keys("Drag", "Move it; drag a corner to resize"),
            Row::Gap(9.0),
            heading("SHOW"),
            Row::Gap(3.0),
            keys("?  or  F1", "These keys"),
            keys("I", "Song title: on a change, always, off"),
            keys("T", "This preset's name, on or off"),
            keys("D", "FPS, on or off"),
        ];
        overlay.show(
            &live.gl,
            &rows,
            Place::Center,
            Backing::Box,
            Duration::from_secs(12),
            window_size(&live.window),
        );
        self.showing_keys = true;
        live.window.request_redraw();
    }

    /// A line of its own, low in the picture: a short answer to a key.
    fn show_note(&mut self, text: String) {
        let Some(live) = &mut self.live else {
            return;
        };
        let Some(overlay) = &mut live.overlay else {
            return;
        };
        overlay.show(
            &live.gl,
            &[Row::Line(Span::new(text, 15.0).weight(600.0))],
            Place::BottomLeft,
            Backing::Shadow,
            Duration::from_secs(3),
            window_size(&live.window),
        );
        self.showing_keys = false;
        live.window.request_redraw();
    }

    /// Moves the song title on to its next way of being shown: the card
    /// in the middle when it changes, the corner at all times, neither.
    fn cycle_song(&mut self) {
        self.song_shown = match self.song_shown {
            SongShown::OnChange => SongShown::Always,
            SongShown::Always => SongShown::Off,
            SongShown::Off => SongShown::OnChange,
        };
        let note = match self.song_shown {
            SongShown::OnChange => "Song title: when it changes",
            SongShown::Always => "Song title: always",
            SongShown::Off => "Song title: off",
        };
        self.corner_written[Status::Song.index()] = None;
        self.update_corners();
        self.show_note(note.into());
    }

    /// Toggles a persistent corner status.
    fn toggle_status(&mut self, what: Status) {
        match what {
            Status::Song => return,
            Status::Preset => self.preset_on = !self.preset_on,
            Status::Fps => self.fps_on = !self.fps_on,
        }
        // A key press answers at once, whatever the pace below.
        self.corner_written[what.index()] = None;
        self.update_corners();
        if let Some(live) = &self.live {
            live.window.request_redraw();
        }
    }

    /// Current lines for a corner status, or none when disabled.
    fn corner_lines(&self, what: Status) -> Vec<String> {
        match what {
            Status::Fps if self.fps_on => {
                vec![format!("{:.0} FPS", frames_per_second(&self.drawn))]
            }
            Status::Song if self.song_shown == SongShown::Always => match &self.song {
                Some(song) => song
                    .iter()
                    .take(2)
                    .filter(|line| !line.is_empty())
                    .cloned()
                    .collect(),
                None => vec!["Nothing playing".into()],
            },
            Status::Preset if self.preset_on => vec![
                self.presets
                    .current()
                    .and_then(|path| path.file_stem())
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_else(|| "No preset".into()),
            ],
            _ => Vec::new(),
        }
    }

    /// Writes each corner again when it would say something else, and no
    /// more often than a few times a second while a count is moving.
    fn update_corners(&mut self) {
        for what in [Status::Fps, Status::Song, Status::Preset] {
            let at = what.index();
            let lines = self.corner_lines(what);
            if lines.is_empty() {
                if !self.corner_lines[at].is_empty() {
                    self.corner_lines[at].clear();
                    if let Some(live) = &mut self.live
                        && let Some(corner) = &mut live.corners[at]
                    {
                        corner.hide();
                    }
                }
                continue;
            }
            // Saying the same thing again is not worth a new bitmap, and
            // a frame rate that flickers between two numbers is not worth
            // one several times a second either.
            if lines == self.corner_lines[at] {
                continue;
            }
            let waited = self.corner_written[at]
                .is_none_or(|written| written.elapsed() >= Duration::from_millis(400));
            if !waited {
                continue;
            }
            let rows: Vec<Row> = lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    // The song leads with its title and names who plays
                    // it underneath; everything else is one plain line.
                    let span = match (what, index) {
                        (Status::Song, 0) => Span::new(line.clone(), 16.0).weight(700.0),
                        (Status::Song, _) => Span::new(line.clone(), 13.0).tint(0.82),
                        _ => Span::new(line.clone(), 13.0).weight(600.0),
                    };
                    Row::Line(span)
                })
                .collect();
            self.corner_lines[at] = lines;
            self.corner_written[at] = Some(Instant::now());
            let Some(live) = &mut self.live else {
                return;
            };
            let gl = Arc::clone(&live.gl);
            let window = window_size(&live.window);
            let Some(corner) = &mut live.corners[at] else {
                continue;
            };
            corner.show(
                &gl,
                &rows,
                what.place(),
                Backing::Shadow,
                // Long enough that it never fades on its own; the key
                // that turned it on is what turns it off.
                Duration::from_secs(60 * 60),
                window,
            );
        }
    }

    /// What is playing, big in the middle of the picture: the title, then
    /// the artist, then the album, fading out into the visuals again.
    fn show_song(&mut self) {
        let Some(live) = &mut self.live else {
            return;
        };
        let (Some(overlay), Some(song)) = (&mut live.overlay, &self.song) else {
            return;
        };
        let mut rows: Vec<Row> = Vec::new();
        for (index, text) in song.iter().take(3).enumerate() {
            if text.is_empty() {
                continue;
            }
            let span = match index {
                0 => Span::new(text.clone(), 27.0).weight(700.0),
                1 => Span::new(text.clone(), 19.0).weight(500.0).tint(0.92),
                _ => Span::new(text.clone(), 15.0).tint(0.68),
            };
            if index > 0 {
                rows.push(Row::Gap(3.0));
            }
            rows.push(Row::Line(span));
        }
        if rows.is_empty() {
            return;
        }
        overlay.show(
            &live.gl,
            &rows,
            Place::Center,
            Backing::Shadow,
            Duration::from_secs(4),
            window_size(&live.window),
        );
        self.showing_keys = false;
        live.window.request_redraw();
    }
}

/// How the song's title is shown, which one key moves through.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SongShown {
    /// A card in the middle of the picture when the song turns over.
    OnChange,
    /// In its corner, all the time.
    Always,
    Off,
}

/// What a corner can be asked to keep showing, and which corner it is.
#[derive(Clone, Copy)]
enum Status {
    Fps,
    Song,
    Preset,
}

impl Status {
    fn index(self) -> usize {
        match self {
            Status::Fps => 0,
            Status::Song => 1,
            Status::Preset => 2,
        }
    }

    /// The frame rate out of the way in the top left, the song where the
    /// eye goes first, the preset's name furthest from both.
    fn place(self) -> Place {
        match self {
            Status::Fps => Place::TopLeft,
            Status::Song => Place::TopRight,
            Status::Preset => Place::BottomRight,
        }
    }
}

/// Frames a second, from when the last frames were drawn.
fn frames_per_second(drawn: &[Instant]) -> f32 {
    let (Some(first), Some(last)) = (drawn.first(), drawn.last()) else {
        return 0.0;
    };
    let span = last.duration_since(*first);
    if drawn.len() < 2 || span.is_zero() {
        return 0.0;
    }
    (drawn.len() - 1) as f32 / span.as_secs_f32()
}

/// The window's size in pixels, which the overlay lays itself out for.
fn window_size(window: &Window) -> (u32, u32) {
    let size = window.inner_size();
    (size.width.max(1), size.height.max(1))
}

/// Asks the app for something only it can do: the player is over there.
fn command(what: &str) {
    report(&Event {
        command: Some(what.to_string()),
        ..Default::default()
    });
}

/// Writes one event line to the app and flushes it. The line is tagged so
/// the app can tell it from libprojectM's own chatter on the same stream.
fn report(event: &Event) {
    if let Ok(line) = serde_json::to_string(event) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{}{line}", super::host::EVENT_PREFIX);
        let _ = stdout.flush();
    }
}

/// Builds the window, a GL context and surface for it, and projectM.
fn build(event_loop: &ActiveEventLoop, args: &Args, seconds: u32) -> Result<Live, String> {
    let mut attributes = Window::default_attributes()
        .with_title("MilkDrop")
        .with_decorations(false)
        .with_resizable(true)
        .with_min_inner_size(LogicalSize::new(MIN_SIZE[0], MIN_SIZE[1]))
        .with_inner_size(LogicalSize::new(args.size[0], args.size[1]));
    if let Some(pos) = args.pos {
        attributes = attributes.with_position(LogicalPosition::new(pos[0], pos[1]));
    }

    // Opaque: no alpha, so the window is never see-through.
    let template = ConfigTemplateBuilder::new().with_alpha_size(0);
    let (window, config) = DisplayBuilder::new()
        .with_window_attributes(Some(attributes))
        .build(event_loop, template, |mut configs| {
            configs.next().expect("a GL config for MilkDrop")
        })
        .map_err(|error| format!("no GL config: {error}"))?;
    let window = window.ok_or_else(|| "the window was not created".to_string())?;

    let raw = window
        .window_handle()
        .map_err(|error| format!("no window handle: {error}"))?
        .as_raw();
    let context_attributes = ContextAttributesBuilder::new().build(Some(raw));
    let not_current = unsafe {
        config
            .display()
            .create_context(&config, &context_attributes)
    }
    .map_err(|error| format!("no GL context: {error}"))?;

    let inner = window.inner_size();
    let surface_attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(
        raw,
        NonZeroU32::new(inner.width).unwrap_or(NonZeroU32::MIN),
        NonZeroU32::new(inner.height).unwrap_or(NonZeroU32::MIN),
    );
    let surface = unsafe {
        config
            .display()
            .create_window_surface(&config, &surface_attributes)
    }
    .map_err(|error| format!("no GL surface: {error}"))?;
    let context = not_current
        .make_current(&surface)
        .map_err(|error| format!("could not make the context current: {error}"))?;
    let _ = surface.set_swap_interval(&context, SwapInterval::DontWait);

    let gl = Arc::new(unsafe {
        glow::Context::from_loader_function_cstr(|symbol| config.display().get_proc_address(symbol))
    });
    let texture_dirs = [args.presets.clone(), args.presets.join("textures")];
    let dirs: Vec<&std::path::Path> = texture_dirs.iter().map(|dir| dir.as_path()).collect();
    let engine = Engine::new(Arc::clone(&gl), &dirs, seconds)
        .ok_or_else(|| "needs OpenGL 3.3".to_string())?;
    // Text over the picture; the picture goes on without it if the
    // shaders will not take.
    let overlay = Overlay::new(&gl);
    let corners = [Overlay::new(&gl), Overlay::new(&gl), Overlay::new(&gl)];

    let mut live = Live {
        engine,
        gl,
        overlay,
        corners,
        context,
        surface,
        window,
        fullscreen: false,
    };
    if args.fullscreen {
        live.window
            .set_fullscreen(Some(Fullscreen::Borderless(None)));
        live.fullscreen = true;
    }
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_RING: AtomicU64 = AtomicU64::new(0);

    /// A child with no window, for the parts that need none.
    fn headless_child() -> Child {
        let dir = std::env::temp_dir().join(format!(
            "spotifast-milkdrop-child-{}-{}",
            std::process::id(),
            NEXT_RING.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a place for the test's ring");
        let shm = dir.join("ring");
        let ring = Ring::create(&shm).expect("a ring to read");
        Child::new(
            Args {
                shm,
                presets: dir,
                size: [640.0, 480.0],
                pos: None,
                fullscreen: false,
                fps: 30,
                seconds: 30,
                scale: 1,
            },
            ring,
        )
    }

    /// The limit is the number it was given, and nothing at zero.
    #[test]
    fn the_frame_limit_is_the_number_it_was_given() {
        let mut child = headless_child();
        let hz = |interval: Option<Duration>| {
            interval.map(|gap| (1.0 / gap.as_secs_f32()).round() as u32)
        };
        for rate in [24, 60, 144, 240] {
            child.fps = rate;
            assert_eq!(hz(child.frame_interval()), Some(rate));
        }
        child.fps = 0;
        assert!(child.frame_interval().is_none(), "zero is uncapped");
    }

    /// Frame deadlines do not drift with rendering time.
    #[test]
    fn the_frame_limit_does_not_drift_with_the_drawing() {
        let mut child = headless_child();
        child.fps = 60;
        let interval = Duration::from_secs_f32(1.0 / 60.0);
        let start = Instant::now();
        child.next_frame = start;

        // Simulate ten frames that each use one third of the interval.
        for frame in 1..=10 {
            child.schedule_next_frame_at(start + interval * (frame - 1) / 3);
            assert_eq!(
                child.next_frame,
                start + interval * frame,
                "frame {frame} is due on the beat"
            );
        }

        // Falling behind for good does not pile up a debt of frames to
        // catch up on: the rhythm picks up from now.
        let late = start + Duration::from_secs(60);
        child.next_frame = late - Duration::from_secs(5);
        child.schedule_next_frame_at(late);
        assert_eq!(
            child.next_frame,
            late + interval,
            "a missed stretch is let go, not chased"
        );
    }

    /// Each corner status renders independently and clears when disabled.
    #[test]
    fn each_corner_carries_what_was_switched_on() {
        let mut child = headless_child();
        for what in [Status::Fps, Status::Song, Status::Preset] {
            assert!(
                child.corner_lines(what).is_empty(),
                "nothing on, nothing shown"
            );
        }

        child.fps_on = true;
        let fps = child.corner_lines(Status::Fps);
        assert_eq!(fps.len(), 1);
        assert!(fps[0].ends_with("FPS"), "the count says FPS: {}", fps[0]);
        assert!(
            child.corner_lines(Status::Song).is_empty(),
            "one key, one corner"
        );

        child.song = Some(vec![
            "Wish You Were Here".into(),
            "Incubus".into(),
            "Morning View".into(),
        ]);
        // One key moves the title through its three ways of being shown.
        assert_eq!(
            child.song_shown,
            SongShown::OnChange,
            "the card, by default"
        );
        assert!(
            child.corner_lines(Status::Song).is_empty(),
            "announced on a change, it is not also kept in the corner"
        );
        child.cycle_song();
        assert_eq!(child.song_shown, SongShown::Always);
        assert_eq!(
            child.corner_lines(Status::Song),
            vec!["Wish You Were Here", "Incubus"],
            "the song names itself, then who plays it"
        );
        child.cycle_song();
        assert_eq!(child.song_shown, SongShown::Off);
        assert!(child.corner_lines(Status::Song).is_empty());
        child.cycle_song();
        assert_eq!(child.song_shown, SongShown::OnChange, "round it goes");
        child.cycle_song();

        child.presets.files = vec![PathBuf::from("/presets/Geiss - Spiral Artifact.milk")];
        child.presets.next(true);
        child.preset_on = true;
        assert_eq!(
            child.corner_lines(Status::Preset),
            vec!["Geiss - Spiral Artifact"]
        );

        // Three corners, three places, no two the same.
        let places: Vec<Place> = [Status::Fps, Status::Song, Status::Preset]
            .iter()
            .map(|what| what.place())
            .collect();
        assert!(
            places[0] != places[1] && places[1] != places[2] && places[0] != places[2],
            "each corner is its own"
        );

        child.cycle_song();
        child.preset_on = false;
        child.fps_on = false;
        for what in [Status::Fps, Status::Song, Status::Preset] {
            assert!(child.corner_lines(what).is_empty(), "and off again");
        }
    }

    /// FPS uses elapsed time and requires at least two frames.
    #[test]
    fn frames_a_second_are_counted_over_the_time_they_took() {
        assert_eq!(frames_per_second(&[]), 0.0);
        let now = Instant::now();
        assert_eq!(frames_per_second(&[now]), 0.0, "one frame measures nothing");

        // Thirty frames, a fiftieth of a second apart: fifty a second.
        let drawn: Vec<Instant> = (0..30)
            .map(|index| now + Duration::from_micros(20_000 * index))
            .collect();
        let fps = frames_per_second(&drawn);
        assert!(
            (fps - 50.0).abs() < 0.001,
            "twenty milliseconds a frame is fifty a second, not {fps}"
        );
    }
}
