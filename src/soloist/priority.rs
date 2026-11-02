//! Raises the threads that carry Soloist's audio. They run at normal priority
//! and its stream is async, so on a busy machine a thread that wakes late
//! leaves a hole in the music, which PipeWire doesn't count as an xrun.
//!
//! A nice level, not SCHED_RR: Soloist is closed source, realtime would need
//! RLIMIT_RTTIME on it, and one busy thread could then stall the desktop.
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

/// Their names in `/proc`, cut to the kernel's 15 bytes.
const AUDIO_THREADS: [&str; 3] = ["pipewire-loop", "Audio Driver Th", "Media Mixer Ren"];
/// What PipeWire's own module-rt asks for a client that can't have realtime.
const NICE: i32 = -11;
/// Threads appear with the first playback, and may come and go with it.
const LOOK_EVERY: Duration = Duration::from_secs(2);
const STOP_CHECK: Duration = Duration::from_millis(250);

const RTKIT: &str = "org.freedesktop.RealtimeKit1";
const RTKIT_PATH: &str = "/org/freedesktop/RealtimeKit1";

/// The audio threads `pid` has right now.
fn audio_threads(pid: u32) -> Vec<u32> {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    tasks
        .flatten()
        .filter_map(|task| {
            let tid = task.file_name().to_str()?.parse().ok()?;
            let name = std::fs::read_to_string(task.path().join("comm")).ok()?;
            AUDIO_THREADS.contains(&name.trim_end()).then_some(tid)
        })
        .collect()
}

/// Which of a child's threads have been dealt with: each is asked for once.
#[derive(Default)]
struct Raised {
    pid: u32,
    tried: HashSet<u32>,
}
impl Raised {
    /// Tries `raise` on what is new among `threads`.
    fn pass(&mut self, pid: u32, threads: &[u32], mut raise: impl FnMut(u32) -> bool) {
        if self.pid != pid {
            self.pid = pid;
            self.tried.clear();
        }
        for tid in threads {
            if self.tried.insert(*tid) {
                if raise(*tid) {
                    log::info!("Soloist's audio thread {tid} now runs at nice {NICE}");
                } else {
                    log::debug!("Soloist's audio thread {tid} stays at normal priority");
                }
            }
        }
    }
}

fn rtkit(bus: &mut Option<zbus::blocking::Connection>, pid: u32, tid: u32) -> zbus::Result<()> {
    let connection = match bus {
        Some(connection) => connection,
        None => bus.insert(zbus::blocking::Connection::system()?),
    };
    connection.call_method(
        Some(RTKIT),
        RTKIT_PATH,
        Some(RTKIT),
        "MakeThreadHighPriorityWithPID",
        &(u64::from(pid), u64::from(tid), NICE),
    )?;
    Ok(())
}

/// What a raised RLIMIT_NICE allows without rtkit.
fn renice(tid: u32) -> bool {
    // SAFETY: setpriority reads nothing of ours; on Linux PRIO_PROCESS takes
    // a thread id, and a thread that has gone only makes it fail.
    unsafe { libc::setpriority(libc::PRIO_PROCESS as _, tid as _, NICE) == 0 }
}

/// Keeps `child`'s audio threads raised while `playing`, until `stopped`.
/// Blocks on the system bus, so this has a thread of its own.
pub fn watch(child: impl Fn() -> u32, playing: impl Fn() -> bool, stopped: impl Fn() -> bool) {
    let mut bus = None;
    let mut raised = Raised::default();
    let mut looked = None::<Instant>;
    while !stopped() {
        std::thread::sleep(STOP_CHECK);
        if !playing() || looked.is_some_and(|at| at.elapsed() < LOOK_EVERY) {
            continue;
        }
        looked = Some(Instant::now());
        let pid = child();
        if pid == 0 {
            continue;
        }
        raised.pass(pid, &audio_threads(pid), |tid| {
            match rtkit(&mut bus, pid, tid) {
                Ok(()) => return true,
                Err(error) => log::debug!("rtkit refused Soloist's thread {tid}: {error}"),
            }
            renice(tid)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A thread of this process under `name`, parked until the sender goes.
    fn named(name: &str) -> (u32, mpsc::Sender<()>) {
        let (id, tid) = mpsc::channel();
        let (stop, wait) = mpsc::channel::<()>();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                // SAFETY: gettid takes nothing and cannot fail.
                id.send(unsafe { libc::gettid() } as u32).unwrap();
                let _ = wait.recv();
            })
            .unwrap();
        (tid.recv().unwrap(), stop)
    }

    #[test]
    fn audio_threads_are_found_by_name_and_only_those() {
        let (audio, _keep) = named("pipewire-loop");
        let (other, _keep_other) = named("Default Worker");
        let found = audio_threads(std::process::id());
        assert!(found.contains(&audio));
        assert!(!found.contains(&other));
        assert_eq!(audio_threads(u32::MAX), Vec::<u32>::new());
    }

    /// Each thread is asked for once, a new one as it appears, and a
    /// restarted child starts over: its thread ids are its own.
    #[test]
    fn each_thread_is_asked_for_once_and_new_ones_when_they_appear() {
        let mut raised = Raised::default();
        let mut asked = Vec::new();
        raised.pass(7, &[1, 2], |tid| {
            asked.push(tid);
            false
        });
        raised.pass(7, &[1, 2, 3], |tid| {
            asked.push(tid);
            true
        });
        raised.pass(8, &[1], |tid| {
            asked.push(tid);
            true
        });
        assert_eq!(asked, [1, 2, 3, 1]);
    }

    /// Asks the machine's real rtkit about a thread of this test alone.
    #[test]
    #[ignore = "requires rtkit on the system bus"]
    fn rtkit_raises_a_thread_by_its_process_and_thread_id() {
        let (tid, _keep) = named("Audio Driver Th");
        let mut bus = None;
        rtkit(&mut bus, std::process::id(), tid).unwrap();
        let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).unwrap();
        let after_name = stat.rsplit(')').next().unwrap();
        let nice: i32 = after_name
            .split_whitespace()
            .nth(16)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(nice, NICE);
    }
}
