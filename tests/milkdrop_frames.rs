//! The windowless MilkDrop child, end to end: it draws on a graphics device
//! with no display server and publishes frames the app can take. The child is
//! this package's binary, as the app starts it.
#![cfg(all(feature = "milkdrop", target_os = "linux"))]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use spotifast::milkdrop::frames::Frames;
use spotifast::milkdrop::shm::Ring;

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spotifast-milkdrop-frames-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Starts the child with no display to reach, as under a service or over
/// SSH: it must not need one.
fn child(dir: &Path, stdin: Stdio) -> Child {
    Command::new(env!("CARGO_BIN_EXE_spotifast"))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .args(["--milkdrop-child", "--milkdrop-shm"])
        .arg(dir.join("pcm"))
        .arg("--milkdrop-frames")
        .arg(dir.join("frames"))
        .arg("--milkdrop-presets")
        .arg(dir)
        .stdin(stdin)
        .stdout(Stdio::null())
        .spawn()
        .unwrap()
}

/// Waits for `check`, keeping the child watched; `None` if the child left
/// first with the code for a machine it cannot draw on.
fn watch<T>(
    process: &mut Child,
    frames: &Frames,
    mut check: impl FnMut() -> Option<T>,
) -> Option<T> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        frames.beat();
        if let Some(found) = check() {
            return Some(found);
        }
        if let Some(status) = process.try_wait().unwrap() {
            assert_eq!(status.code(), Some(3), "only no graphics may end it");
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no frame in thirty seconds");
}

#[test]
fn the_child_draws_with_no_window_and_stops_with_its_input() {
    let dir = folder("draws");
    std::fs::write(dir.join("empty.milk"), b"[preset00]\n").unwrap();
    let _ring = Ring::create(&dir.join("pcm")).unwrap();
    let frames = Frames::create(&dir.join("frames")).unwrap();
    frames.ask([160, 120]);
    let mut process = child(&dir, Stdio::piped());
    let mut seen = 0;
    let Some((size, _)) = watch(&mut process, &frames, || frames.take(&mut seen)) else {
        eprintln!("no graphics device here: nothing to test");
        return;
    };
    assert_eq!(size, [160, 120]);

    // It follows the pane.
    frames.ask([320, 180]);
    let resized = watch(&mut process, &frames, || {
        frames
            .take(&mut seen)
            .filter(|(size, _)| *size == [320, 180])
    });
    let (_, pixels) = resized.expect("it was drawing a moment ago");
    assert!(
        pixels.iter().all(|pixel| pixel.a() == 255),
        "the picture is opaque"
    );

    drop(process.stdin.take());
    let started = Instant::now();
    while process.try_wait().unwrap().is_none() {
        assert!(started.elapsed() < Duration::from_secs(10), "it did not go");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A stuck or minimised app stops asking; the child does not draw for it
/// for ever.
#[test]
fn the_child_leaves_when_nobody_watches() {
    let dir = folder("leaves");
    let _ring = Ring::create(&dir.join("pcm")).unwrap();
    let frames = Frames::create(&dir.join("frames")).unwrap();
    frames.ask([64, 64]);
    let mut process = child(&dir, Stdio::piped());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = process.try_wait().unwrap() {
            break status;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "it stayed");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success() || status.code() == Some(3));
    std::fs::remove_dir_all(&dir).unwrap();
}
