//! MilkDrop inside the app's own windows: a skin's visualisation pane, or the
//! Winamp frame around it.
//!
//! `Stream` keeps a windowless child running while something shows its
//! picture, and turns the newest frame it published into a texture. The UI
//! thread only ever looks at shared memory and asks nothing of the child, so
//! a child that hangs or dies costs the bars their pane back and nothing
//! else. Starting one (which writes files and forks) and reaping one happen
//! on threads of their own.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use egui::{ColorImage, TextureHandle, TextureOptions};

use super::frames::{self, Frames};
use super::offscreen::NO_GRAPHICS;
use super::shm::Ring;
use crate::vis::AudioTap;

/// How long after the picture was last shown the child stops.
const LINGER: Duration = Duration::from_secs(1);
/// How long a child may go without a frame before it counts as hung. Long,
/// because a preset's shaders can take seconds to compile.
const HUNG: Duration = Duration::from_secs(15);
/// The first wait after a failure, doubling up to [`LONGEST_WAIT`].
const FIRST_WAIT: Duration = Duration::from_secs(5);
const LONGEST_WAIT: Duration = Duration::from_secs(300);
/// A child that ran this long has shown it can run; its end starts the count
/// of failures anew.
const PROVEN: Duration = Duration::from_secs(60);
/// How soon to look again in a folder that had no presets.
const NO_PRESETS_WAIT: Duration = Duration::from_secs(10);
/// Scaled up smoothly when a big window outgrows the frame limit.
const OPTIONS: TextureOptions = TextureOptions::LINEAR;

/// What a pane can show at this moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picture {
    /// The newest frame. It is stored as OpenGL reads it, bottom row first.
    Frame(egui::TextureId),
    /// A child is on its way: draw the bars, and keep painting.
    Waiting,
    /// No MilkDrop here for now: the bars.
    None,
}

/// The texture coordinates that stand a frame the right way up.
pub const UV: egui::Rect = egui::Rect {
    min: egui::pos2(0.0, 1.0),
    max: egui::pos2(1.0, 0.0),
};

struct Running {
    process: Child,
    /// Held so that the child sees end of input when it is dropped.
    _stdin: ChildStdin,
    ring: Arc<Ring>,
    frames: Frames,
    seen: u64,
    started: Instant,
    framed: Option<Instant>,
}

enum Refused {
    NoPresets,
    Failed(String),
}

type Started = Result<Running, Refused>;

enum Phase {
    Off,
    Starting(Receiver<Started>),
    Running(Box<Running>),
}

pub struct Stream {
    tap: Arc<AudioTap>,
    ring_path: PathBuf,
    frames_path: PathBuf,
    /// What to run as the child: this program, unless a test says otherwise.
    program: Option<PathBuf>,
    phase: Phase,
    texture: Option<TextureHandle>,
    /// When the picture was last asked for.
    shown: Option<Instant>,
    /// The frame size asked for, and in which pass, so that two panes drawn
    /// in one pass share the bigger.
    size: [u32; 2],
    pass: u64,
    failures: u32,
    /// Not to start again before this.
    retry: Option<Instant>,
    /// How bright the newest frame is; see [`frames::glow`].
    glow: f32,
    /// What to answer in a test of what is drawn over the picture.
    #[cfg(test)]
    pub(crate) answer: Option<Picture>,
}

impl Stream {
    pub fn new(tap: Arc<AudioTap>) -> Self {
        let name = |extension: &str| {
            std::env::temp_dir().join(format!(
                "spotifast-milkdrop-pane-{}.{extension}",
                std::process::id()
            ))
        };
        Self {
            tap,
            ring_path: name("pcm"),
            frames_path: name("frames"),
            program: None,
            phase: Phase::Off,
            texture: None,
            shown: None,
            size: [0, 0],
            pass: 0,
            failures: 0,
            retry: None,
            glow: 0.0,
            #[cfg(test)]
            answer: None,
        }
    }

    /// Asks for the picture at this many physical pixels, and says what
    /// there is to draw. Called for each pane each time it paints; it copies
    /// a frame only if a new one has been published, and never waits.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pixels: [f32; 2],
        scale: u32,
        side: u32,
    ) -> Picture {
        let now = Instant::now();
        self.shown = Some(now);
        #[cfg(test)]
        if let Some(answer) = self.answer {
            return answer;
        }
        let Phase::Running(running) = &mut self.phase else {
            let waiting = matches!(self.phase, Phase::Starting(_)) || self.retry.is_none();
            return if waiting {
                Picture::Waiting
            } else {
                Picture::None
            };
        };
        let [width, height] = frames::fit(pixels, scale, side);
        let pass = ctx.cumulative_pass_nr();
        self.size = if pass == self.pass {
            [self.size[0].max(width), self.size[1].max(height)]
        } else {
            [width, height]
        };
        self.pass = pass;
        running.frames.ask(self.size);
        running.frames.beat();
        if let Some((size, pixels)) = running.frames.take(&mut running.seen) {
            self.glow = frames::glow(&pixels);
            let image = ColorImage::new(size, pixels);
            match &mut self.texture {
                Some(texture) => texture.set(image, OPTIONS),
                None => self.texture = Some(ctx.load_texture("milkdrop-pane", image, OPTIONS)),
            }
            if running.framed.is_none() {
                // Both sides have the files mapped; on Windows, where a
                // mapped file cannot go yet, `stop` gets them.
                let _ = std::fs::remove_file(&self.ring_path);
                let _ = std::fs::remove_file(&self.frames_path);
            }
            running.framed = Some(now);
        }
        match &self.texture {
            Some(texture) => Picture::Frame(texture.id()),
            None => Picture::Waiting,
        }
    }

    /// How bright the picture is, for what is drawn over it.
    pub fn glow(&self) -> f32 {
        self.glow
    }

    /// Moves the preset on or back, if a frame is on show; `false` if not,
    /// which leaves the skin's command to the bars.
    pub fn step(&self, by: i32) -> bool {
        match &self.phase {
            Phase::Running(running) if running.framed.is_some() => {
                running.frames.step(by);
                true
            }
            _ => false,
        }
    }

    /// The window is gone, and the texture with its context. The next
    /// paint uploads the newest frame afresh.
    pub fn forget_texture(&mut self) {
        self.texture = None;
        if let Phase::Running(running) = &mut self.phase {
            running.seen = 0;
        }
    }

    /// Starts and stops the child to match what has been shown, and hands
    /// it the settings. Runs each logic pass, with or without a window. While
    /// `frozen` the child keeps its picture and draws nothing, even if no
    /// paint comes along to ask for it.
    pub fn tick(&mut self, presets: &Path, fps: u32, seconds: u32, frozen: bool) {
        let now = Instant::now();
        if frozen && let Phase::Running(running) = &mut self.phase {
            // Quiet because it was told to be, not hung.
            running.framed = running.framed.map(|_| now);
        }
        self.settle(now);
        let wanted = frozen
            || self
                .shown
                .is_some_and(|at| now.saturating_duration_since(at) < LINGER);
        match &self.phase {
            Phase::Off => {
                if wanted && self.retry.is_none_or(|at| now >= at) {
                    self.retry = None;
                    self.start(presets);
                }
            }
            Phase::Starting(_) => {}
            Phase::Running(running) => {
                if wanted {
                    running.frames.pace(fps, seconds);
                    running.frames.freeze(frozen);
                } else {
                    self.stop();
                }
            }
        }
    }

    /// Takes a child that has finished starting, and notices one that has
    /// ended or stopped drawing.
    fn settle(&mut self, now: Instant) {
        match &mut self.phase {
            Phase::Off => {}
            Phase::Starting(receiver) => match receiver.try_recv() {
                Ok(Ok(running)) => {
                    self.tap.set_pane_shm(Some(Arc::clone(&running.ring)));
                    self.phase = Phase::Running(Box::new(running));
                }
                Ok(Err(Refused::NoPresets)) => {
                    self.phase = Phase::Off;
                    self.retry = Some(now + NO_PRESETS_WAIT);
                }
                Ok(Err(Refused::Failed(error))) => {
                    log::warn!("MilkDrop for the skin could not start: {error}");
                    self.phase = Phase::Off;
                    self.failed(now, false);
                }
                Err(TryRecvError::Disconnected) => {
                    self.phase = Phase::Off;
                    self.failed(now, false);
                }
                Err(TryRecvError::Empty) => {}
            },
            Phase::Running(running) => {
                let last_frame = running.framed.unwrap_or(running.started);
                let hung = now.saturating_duration_since(last_frame) >= HUNG;
                let ended = running.process.try_wait().ok().flatten();
                let proven = now.saturating_duration_since(running.started) >= PROVEN;
                if ended.is_some() || hung {
                    if proven {
                        self.failures = 0;
                    }
                    self.stop();
                    match ended {
                        // Left by itself, unwatched: nothing is wrong.
                        Some(status) if status.success() => {}
                        Some(status) => self.failed(now, no_graphics(status)),
                        None => {
                            log::warn!("MilkDrop for the skin stopped drawing");
                            self.failed(now, false);
                        }
                    }
                }
            }
        }
    }

    /// Leaves the pane to the bars for a while.
    fn failed(&mut self, now: Instant, for_good: bool) {
        let wait = if for_good {
            LONGEST_WAIT
        } else {
            FIRST_WAIT
                .saturating_mul(1 << self.failures.min(10))
                .min(LONGEST_WAIT)
        };
        self.failures += 1;
        self.retry = Some(now + wait);
    }

    fn start(&mut self, presets: &Path) {
        let (sender, receiver) = mpsc::channel();
        let paths = (self.ring_path.clone(), self.frames_path.clone());
        let program = self.program.clone();
        let presets = presets.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("milkdrop-start".into())
            .spawn(move || {
                let _ = sender.send(launch(program, &paths.0, &paths.1, &presets));
            });
        match spawned {
            Ok(_) => self.phase = Phase::Starting(receiver),
            Err(error) => log::warn!("could not start MilkDrop for the skin: {error}"),
        }
    }

    /// Lets go of the child, off this thread: a driver can hold a dying
    /// process for as long as it likes.
    fn stop(&mut self) {
        self.tap.set_pane_shm(None);
        self.texture = None;
        self.size = [0, 0];
        if let Phase::Running(running) = std::mem::replace(&mut self.phase, Phase::Off) {
            let Running { mut process, .. } = *running;
            let reaped = std::thread::Builder::new()
                .name("milkdrop-reap".into())
                .spawn(move || {
                    let _ = process.kill();
                    let _ = process.wait();
                });
            if let Err(error) = reaped {
                log::warn!("could not reap MilkDrop: {error}");
            }
            let _ = std::fs::remove_file(&self.ring_path);
            let _ = std::fs::remove_file(&self.frames_path);
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop();
    }
}

fn no_graphics(status: ExitStatus) -> bool {
    status.code() == Some(NO_GRAPHICS)
}

/// Makes the shared files and starts the child; the slow part of starting.
fn launch(
    program: Option<PathBuf>,
    ring_path: &Path,
    frames_path: &Path,
    presets: &Path,
) -> Started {
    if !super::has_presets(presets) {
        return Err(Refused::NoPresets);
    }
    let failed = |error: io::Error| Refused::Failed(error.to_string());
    let ring = Arc::new(Ring::create(ring_path).map_err(failed)?);
    let frames = Frames::create(frames_path).map_err(failed)?;
    let program = match program {
        Some(program) => program,
        None => super::this_program().map_err(failed)?,
    };
    let mut process = Command::new(program)
        .arg("--milkdrop-child")
        .arg("--milkdrop-shm")
        .arg(ring_path)
        .arg("--milkdrop-frames")
        .arg(frames_path)
        .arg("--milkdrop-presets")
        .arg(presets)
        .stdin(Stdio::piped())
        // projectM talks on stdout, and nobody here wants to hear it.
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(failed)?;
    let stdin = process
        .stdin
        .take()
        .ok_or_else(|| Refused::Failed("no way to reach the child".into()))?;
    Ok(Running {
        process,
        _stdin: stdin,
        ring,
        frames,
        seen: 0,
        started: Instant::now(),
        framed: None,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A folder with a preset in it and, as the child, a shell script.
    fn stream(name: &str, script: &str) -> (Stream, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "spotifast-milkdrop-pane-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.milk"), b"").unwrap();
        let program = dir.join("child.sh");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut stream = Stream::new(AudioTap::new());
        stream.program = Some(program);
        stream.ring_path = dir.join("pcm");
        stream.frames_path = dir.join("frames");
        (stream, dir)
    }

    /// Ticks until `done`, the way the app's logic pass would, looking at
    /// the pane every time.
    fn run_until(
        stream: &mut Stream,
        ctx: &egui::Context,
        dir: &Path,
        done: impl Fn(&Stream) -> bool,
    ) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            stream.show(ctx, [320.0, 240.0], 1, frames::PANE_SIDE);
            stream.tick(dir, 60, 10, false);
            if done(stream) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_child_that_dies_leaves_the_bars_and_a_wait() {
        let (mut stream, dir) = stream("dies", "exit 9");
        let ctx = egui::Context::default();
        assert_eq!(
            stream.show(&ctx, [320.0, 240.0], 1, frames::PANE_SIDE),
            Picture::Waiting
        );
        assert!(run_until(&mut stream, &ctx, &dir, |stream| stream
            .retry
            .is_some()));
        assert_eq!(stream.failures, 1);
        assert_eq!(
            stream.show(&ctx, [320.0, 240.0], 1, frames::PANE_SIDE),
            Picture::None,
            "bars, with no repaint asked on its account"
        );
        let retry = stream.retry;
        stream.tick(&dir, 60, 10, false);
        assert_eq!(stream.retry, retry, "and it does not start again at once");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_failure_waits_longer_up_to_a_limit() {
        let (mut stream, dir) = stream("waits", "exit 1");
        let now = Instant::now();
        for seconds in [5, 10, 20, 40, 80, 160, 300, 300] {
            stream.failed(now, false);
            assert_eq!(stream.retry, Some(now + Duration::from_secs(seconds)));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_program_that_cannot_run_is_only_a_failure() {
        let (mut stream, dir) = stream("missing", "exit 0");
        stream.program = Some(dir.join("no-such-program"));
        let ctx = egui::Context::default();
        assert!(run_until(&mut stream, &ctx, &dir, |stream| stream
            .retry
            .is_some()));
        assert_eq!(
            stream.show(&ctx, [320.0, 240.0], 1, frames::PANE_SIDE),
            Picture::None
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Nothing to draw is not a failure; the bars stay and the folder is
    /// looked in again.
    #[test]
    fn no_presets_is_not_a_failure() {
        let (mut stream, dir) = stream("empty", "exit 0");
        std::fs::remove_file(dir.join("a.milk")).unwrap();
        let ctx = egui::Context::default();
        assert!(run_until(&mut stream, &ctx, &dir, |stream| stream
            .retry
            .is_some()));
        assert_eq!(stream.failures, 0);
        assert_eq!(
            stream.show(&ctx, [320.0, 240.0], 1, frames::PANE_SIDE),
            Picture::None
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Once nothing shows the picture the child stops, and the tap lets go.
    #[test]
    fn the_child_runs_only_while_the_picture_is_shown() {
        let (mut stream, dir) = stream("stops", "exec cat >/dev/null");
        let ctx = egui::Context::default();
        assert!(run_until(&mut stream, &ctx, &dir, |stream| matches!(
            stream.phase,
            Phase::Running(_)
        )));
        stream.shown = Some(Instant::now() - LINGER);
        stream.tick(&dir, 60, 10, false);
        assert!(matches!(stream.phase, Phase::Off));
        assert!(stream.retry.is_none(), "stopping is no failure");
        stream.tick(&dir, 60, 10, false);
        assert!(matches!(stream.phase, Phase::Off), "and it stays stopped");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A child alive but silent cannot keep the pane waiting for ever.
    #[test]
    fn a_child_that_stops_drawing_counts_as_failed() {
        let (mut stream, dir) = stream("hangs", "exec cat >/dev/null");
        let ctx = egui::Context::default();
        assert!(run_until(&mut stream, &ctx, &dir, |stream| matches!(
            stream.phase,
            Phase::Running(_)
        )));
        if let Phase::Running(running) = &mut stream.phase {
            running.started = Instant::now() - HUNG;
        }
        stream.show(&ctx, [320.0, 240.0], 1, frames::PANE_SIDE);
        stream.tick(&dir, 60, 10, false);
        assert!(matches!(stream.phase, Phase::Off));
        assert_eq!(stream.failures, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// With a frame published the pane has a picture, and the commands of a
    /// skin reach the child; without one they do not claim to.
    #[test]
    fn a_published_frame_becomes_the_picture() {
        let (mut stream, dir) = stream("frame", "exec cat >/dev/null");
        let ctx = egui::Context::default();
        assert!(run_until(&mut stream, &ctx, &dir, |stream| matches!(
            stream.phase,
            Phase::Running(_)
        )));
        assert!(!stream.step(1), "nothing on show to change yet");
        assert_eq!(
            stream.show(&ctx, [320.0, 240.0], 1, frames::PANE_SIDE),
            Picture::Waiting
        );
        let child = Frames::open(&stream.frames_path).unwrap();
        assert_eq!(child.wanted(), Some([320, 240]), "asked at the pane's size");
        child.publish([320, 240], |pixels| pixels.fill(0x80));
        assert!(matches!(
            stream.show(&ctx, [320.0, 240.0], 1, frames::PANE_SIDE),
            Picture::Frame(_)
        ));
        assert!(stream.step(1));
        assert_eq!(child.take_steps(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Paused behind the lyrics, the child stays, and no paint need come.
    #[test]
    fn a_frozen_picture_keeps_its_child_with_nothing_painting() {
        let (mut stream, dir) = stream("frozen", "exec cat >/dev/null");
        let ctx = egui::Context::default();
        assert!(run_until(&mut stream, &ctx, &dir, |stream| matches!(
            stream.phase,
            Phase::Running(_)
        )));
        let child = Frames::open(&stream.frames_path).unwrap();
        child.publish([320, 240], |pixels| pixels.fill(0x80));
        stream.show(&ctx, [320.0, 240.0], 1, frames::PANE_SIDE);
        // No paint for longer than the child would otherwise be kept.
        stream.shown = Some(Instant::now() - LINGER);
        if let Phase::Running(running) = &mut stream.phase {
            running.framed = Some(Instant::now() - HUNG);
        }
        stream.tick(&dir, 60, 10, true);
        assert!(matches!(stream.phase, Phase::Running(_)));
        assert!(child.frozen());
        stream.show(&ctx, [320.0, 240.0], 1, frames::PANE_SIDE);
        stream.tick(&dir, 60, 10, false);
        assert!(!child.frozen(), "and playing again lets it go on");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Full screen asks for more than a pane may have.
    #[test]
    fn a_full_screen_picture_may_be_bigger_than_a_pane() {
        assert_eq!(
            frames::fit([3840.0, 2160.0], 1, frames::PANE_SIDE),
            [1280, 720]
        );
        assert_eq!(
            frames::fit([3840.0, 2160.0], 1, frames::FULL_SCREEN_SIDE),
            [1920, 1080]
        );
    }

    #[test]
    fn glow_reads_the_bright_parts_and_not_a_stray_spark() {
        let white = vec![egui::Color32::WHITE; 4096];
        assert!(frames::glow(&white) > 0.99);
        let mut dark = vec![egui::Color32::from_gray(20); 4096];
        dark[1000] = egui::Color32::WHITE;
        assert!(frames::glow(&dark) < 0.1);
        assert_eq!(frames::glow(&[]), 0.0);
    }
}
