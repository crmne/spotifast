//! A polite request to end the process quits Spotifast the way Quit does,
//! so it stops Soloist and saves its state instead of dying mid-write.
//!
//! The requests are SIGTERM (`kill`, a logout, systemd stopping the
//! session), SIGINT (Ctrl+C in a terminal) and SIGHUP (the terminal
//! closing). One that the launch set to be ignored, as `nohup` does SIGHUP,
//! stays ignored. A second request ends the process at once, for a quit that
//! does not finish; the Soloist child still goes with it (`soloist::owned`).
//!
//! Windows has no such signals for a program without a console: Task
//! Manager and a logout end the process, and the job object ends Soloist.
use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Whether the process has been asked to end.
pub fn requested() -> bool {
    REQUESTED.load(Ordering::Acquire)
}

/// Listens for the requests on a thread of its own, and wakes the app for
/// each so it sees [`requested`] without waiting for its next tick.
#[cfg(unix)]
pub fn listen(waker: crate::backend::Waker) {
    let started = std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::warn!("Not listening for SIGTERM: {error}");
                    return;
                }
            };
            runtime.block_on(watch(waker));
        });
    if let Err(error) = started {
        log::warn!("Not listening for SIGTERM: {error}");
    }
}

#[cfg(not(unix))]
pub fn listen(_waker: crate::backend::Waker) {}

#[cfg(unix)]
async fn watch(waker: crate::backend::Waker) {
    use tokio::signal::unix::{SignalKind, signal};
    let mut streams = Vec::new();
    for (number, name) in [
        (libc::SIGTERM, "SIGTERM"),
        (libc::SIGINT, "SIGINT"),
        (libc::SIGHUP, "SIGHUP"),
    ] {
        if ignored(number) {
            continue;
        }
        match signal(SignalKind::from_raw(number)) {
            Ok(stream) => streams.push((number, name, stream)),
            Err(error) => log::warn!("Not listening for {name}: {error}"),
        }
    }
    if streams.is_empty() {
        return;
    }
    loop {
        let arrivals = streams.iter_mut().map(|(number, name, stream)| {
            Box::pin(async move {
                stream.recv().await;
                (*number, *name)
            })
        });
        let ((number, name), _, _) = futures_util::future::select_all(arrivals).await;
        if REQUESTED.swap(true, Ordering::AcqRel) {
            log::warn!("{name} again before Spotifast quit: ending it now");
            std::process::exit(128 + number);
        }
        log::info!("{name}: quitting");
        waker.wake();
    }
}

/// Whether the launch set `number` to be ignored.
#[cfg(unix)]
fn ignored(number: libc::c_int) -> bool {
    // SAFETY: a null new action only reads the current one into `current`.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        libc::sigaction(number, std::ptr::null(), &mut current) == 0
            && current.sa_sigaction == libc::SIG_IGN
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    const STAND_IN: &str = "terminate::tests::stand_in";
    const ROLE: &str = "SPOTIFAST_TERMINATE_TEST_ROLE";

    /// The listener is process-wide, so a re-run of this test binary plays
    /// the app: the first SIGTERM asks it to quit and leaves it running, a
    /// second ends it, and an ignored SIGHUP stays ignored.
    #[test]
    fn a_first_sigterm_asks_to_quit_and_a_second_ends_the_process() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([STAND_IN, "--exact", "--ignored", "--test-threads=1"])
            .env(ROLE, "app")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(
            status.code(),
            Some(128 + libc::SIGTERM),
            "the stand-in ended with {status}"
        );
    }

    #[test]
    #[ignore = "a stand-in process for a_first_sigterm_asks_to_quit_and_a_second_ends_the_process"]
    fn stand_in() {
        if std::env::var(ROLE).as_deref() != Ok("app") {
            return;
        }
        // SAFETY: sets SIGHUP to be ignored, as nohup does, before listening.
        unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
        super::listen(crate::backend::Waker::default());
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut asked = false;
        while !asked && Instant::now() < deadline {
            // SAFETY: signals this process, which handles or ignores both.
            unsafe { libc::raise(libc::SIGHUP) };
            std::thread::sleep(Duration::from_millis(50));
            assert!(!super::requested(), "an ignored SIGHUP asked to quit");
            // The listener may not have registered yet; until it has, a
            // SIGTERM would end the process with the default action, so it
            // is sent only once SIGTERM is caught.
            if caught(libc::SIGTERM) {
                // SAFETY: as above.
                unsafe { libc::raise(libc::SIGTERM) };
                let asked_by = Instant::now() + Duration::from_secs(2);
                while !super::requested() && Instant::now() < asked_by {
                    std::thread::sleep(Duration::from_millis(10));
                }
                asked = super::requested();
                assert!(asked, "a SIGTERM did not ask to quit");
            }
        }
        assert!(asked, "the listener never took SIGTERM");
        // SAFETY: as above. The second SIGTERM ends the process.
        unsafe { libc::raise(libc::SIGTERM) };
        std::thread::sleep(Duration::from_secs(5));
        panic!("a second SIGTERM left the process running");
    }

    fn caught(number: libc::c_int) -> bool {
        // SAFETY: a null new action only reads the current one.
        unsafe {
            let mut current: libc::sigaction = std::mem::zeroed();
            libc::sigaction(number, std::ptr::null(), &mut current) == 0
                && current.sa_sigaction != libc::SIG_DFL
                && current.sa_sigaction != libc::SIG_IGN
        }
    }
}
