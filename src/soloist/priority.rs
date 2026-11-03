//! Raises the threads that carry Soloist's audio. Its stream is async, so on
//! a busy machine a thread that wakes late leaves a hole in the music, or
//! hands PipeWire half a cycle, and neither counts as an xrun.
//!
//! The threads are made realtime, SCHED_RR at a priority below PipeWire's own
//! data loop. Soloist is closed source, so that is safe only because it runs
//! under `RLIMIT_RTTIME` (`bound_realtime`): a realtime thread that runs that
//! long without sleeping is killed by the kernel instead of stalling the
//! desktop, and Spotifast starts Soloist again. rtkit grants nothing to a
//! process without that limit. If it refuses anyway, the threads get nice -11,
//! and failing that they stay as they are.
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

/// Their names in `/proc`, cut to the kernel's 15 bytes.
const AUDIO_THREADS: [&str; 3] = ["pipewire-loop", "Audio Driver Th", "Media Mixer Ren"];
/// SCHED_RR priority: above every normal thread, below PipeWire's data loops,
/// which rtkit raises to its maximum of 20, and below anything of the
/// desktop's own that asks for realtime.
const RT_PRIORITY: u32 = 5;
/// What PipeWire's own module-rt asks for a client that can't have realtime.
const NICE: i32 = -11;
/// The CPU time in microseconds a realtime thread of Soloist may take without
/// sleeping: rtkit's own default ceiling, `RTTimeUSecMax`, and 10 graph
/// cycles at the usual quantum, which no audio thread should come near.
pub const RT_TIME_LIMIT_US: u64 = 200_000;
/// Threads appear with the first playback, and may come and go with it.
const LOOK_EVERY: Duration = Duration::from_secs(2);
const STOP_CHECK: Duration = Duration::from_millis(250);

const RTKIT: &str = "org.freedesktop.RealtimeKit1";
const RTKIT_PATH: &str = "/org/freedesktop/RealtimeKit1";

/// Makes `command`'s process start under `RLIMIT_RTTIME`, soft and hard
/// alike: without it rtkit refuses realtime, and with it a runaway realtime
/// thread is killed rather than left to hold a CPU.
pub fn bound_realtime(command: &mut tokio::process::Command) {
    // SAFETY: the closure runs in the child between fork and exec, and calls
    // setrlimit alone, which is async-signal-safe. It allocates nothing.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: RT_TIME_LIMIT_US as libc::rlim_t,
                rlim_max: RT_TIME_LIMIT_US as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_RTTIME, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// How Soloist ended, if it may have been the limit that ended it. The
/// kernel sends SIGKILL at the hard limit, and SIGXCPU at a soft one below it.
pub fn killed_by_limit(exit: &std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    match exit.signal()? {
        libc::SIGXCPU => Some("SIGXCPU".into()),
        libc::SIGKILL => Some("SIGKILL".into()),
        _ => None,
    }
}

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

/// What a thread was given.
#[derive(Debug, PartialEq, Eq)]
enum Raise {
    Realtime,
    Nice,
    /// Nothing, and why.
    Refused(String),
}

/// Which of a child's threads have been dealt with: each is asked for once.
#[derive(Default)]
struct Raised {
    pid: u32,
    tried: HashSet<u32>,
}
impl Raised {
    /// Tries `raise` on what is new among `threads`, and logs each result.
    fn pass(&mut self, pid: u32, threads: &[u32], mut raise: impl FnMut(u32) -> Raise) {
        if self.pid != pid {
            self.pid = pid;
            self.tried.clear();
        }
        for tid in threads {
            if !self.tried.insert(*tid) {
                continue;
            }
            match raise(*tid) {
                Raise::Realtime => log::info!(
                    "Soloist's audio thread {tid} now runs realtime, SCHED_RR {RT_PRIORITY}"
                ),
                Raise::Nice => log::info!("Soloist's audio thread {tid} now runs at nice {NICE}"),
                Raise::Refused(why) => {
                    log::info!("Soloist's audio thread {tid} stays at normal priority: {why}");
                }
            }
        }
    }
}

fn rtkit_call(
    bus: &mut Option<zbus::blocking::Connection>,
    method: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> zbus::Result<()> {
    let connection = match bus {
        Some(connection) => connection,
        None => bus.insert(zbus::blocking::Connection::system()?),
    };
    connection.call_method(Some(RTKIT), RTKIT_PATH, Some(RTKIT), method, body)?;
    Ok(())
}

fn realtime(bus: &mut Option<zbus::blocking::Connection>, pid: u32, tid: u32) -> zbus::Result<()> {
    rtkit_call(
        bus,
        "MakeThreadRealtimeWithPID",
        &(u64::from(pid), u64::from(tid), RT_PRIORITY),
    )
}

pub(super) fn high_priority(
    bus: &mut Option<zbus::blocking::Connection>,
    pid: u32,
    tid: u32,
) -> zbus::Result<()> {
    rtkit_call(
        bus,
        "MakeThreadHighPriorityWithPID",
        &(u64::from(pid), u64::from(tid), NICE),
    )
}

/// What a raised RLIMIT_NICE allows without rtkit.
fn renice(tid: u32) -> bool {
    // SAFETY: setpriority reads nothing of ours; on Linux PRIO_PROCESS takes
    // a thread id, and a thread that has gone only makes it fail.
    unsafe { libc::setpriority(libc::PRIO_PROCESS as _, tid as _, NICE) == 0 }
}

/// Realtime, else nice -11 through rtkit, else through RLIMIT_NICE.
fn raise(bus: &mut Option<zbus::blocking::Connection>, pid: u32, tid: u32) -> Raise {
    let refused = match realtime(bus, pid, tid) {
        Ok(()) => return Raise::Realtime,
        Err(error) => error,
    };
    log::info!("rtkit refused realtime for Soloist's thread {tid}: {refused}");
    match high_priority(bus, pid, tid) {
        Ok(()) => Raise::Nice,
        Err(_) if renice(tid) => Raise::Nice,
        Err(error) => Raise::Refused(format!(
            "{error}, and the nice limit allows no raise either"
        )),
    }
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
        raised.pass(pid, &audio_threads(pid), |tid| raise(&mut bus, pid, tid));
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

    /// The fields of `/proc/self/task/<tid>/stat` after the name.
    fn stat(tid: u32) -> Vec<String> {
        let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).unwrap();
        let after_name = stat.rsplit(')').next().unwrap().to_owned();
        after_name.split_whitespace().map(str::to_owned).collect()
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
            Raise::Refused("a test".into())
        });
        raised.pass(7, &[1, 2, 3], |tid| {
            asked.push(tid);
            Raise::Realtime
        });
        raised.pass(8, &[1], |tid| {
            asked.push(tid);
            Raise::Nice
        });
        assert_eq!(asked, [1, 2, 3, 1]);
    }

    /// The limit is there before Soloist runs a line, soft and hard, and its
    /// children have it too.
    #[tokio::test]
    async fn a_spawned_child_runs_under_the_realtime_limit() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("30").kill_on_drop(true);
        bound_realtime(&mut command);
        let mut child = command.spawn().unwrap();
        let limits =
            std::fs::read_to_string(format!("/proc/{}/limits", child.id().unwrap())).unwrap();
        let _ = child.kill().await;
        let line = limits
            .lines()
            .find(|line| line.starts_with("Max realtime timeout"))
            .unwrap();
        let fields: Vec<&str> = line.split_whitespace().collect();
        // This process's own limit says nothing: PipeWire's module-rt sets
        // the same value on it once any test here has made a context.
        assert_eq!(fields[3..], ["200000", "200000", "us"], "{line}");
    }

    #[test]
    fn only_the_limit_s_signals_are_blamed_on_it() {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::ExitStatus::from_raw;
        assert_eq!(
            killed_by_limit(&status(libc::SIGKILL)).as_deref(),
            Some("SIGKILL")
        );
        assert_eq!(
            killed_by_limit(&status(libc::SIGXCPU)).as_deref(),
            Some("SIGXCPU")
        );
        assert_eq!(killed_by_limit(&status(libc::SIGTERM)), None);
        assert_eq!(killed_by_limit(&status(10 << 8)), None, "an exit code");
    }

    /// Asks the machine's real rtkit about a thread of this test alone.
    #[test]
    #[ignore = "requires rtkit on the system bus"]
    fn rtkit_raises_a_thread_by_its_process_and_thread_id() {
        let (tid, _keep) = named("Audio Driver Th");
        let mut bus = None;
        high_priority(&mut bus, std::process::id(), tid).unwrap();
        let nice: i32 = stat(tid)[16].parse().unwrap();
        assert_eq!(nice, NICE);
    }

    /// The same for realtime, which rtkit grants only under the limit. Run it
    /// in a process of its own: it sets the limit on this one.
    #[test]
    #[ignore = "requires rtkit on the system bus, and sets this process's RLIMIT_RTTIME"]
    fn rtkit_makes_a_thread_realtime_under_the_limit() {
        let (tid, _keep) = named("Media Mixer Ren");
        let mut bus = None;
        assert!(
            realtime(&mut bus, std::process::id(), tid).is_err(),
            "not without the limit"
        );
        let limit = libc::rlimit {
            rlim_cur: RT_TIME_LIMIT_US as libc::rlim_t,
            rlim_max: RT_TIME_LIMIT_US as libc::rlim_t,
        };
        // SAFETY: setrlimit reads the struct above and nothing else.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &limit) }, 0);
        realtime(&mut bus, std::process::id(), tid).unwrap();
        let fields = stat(tid);
        // rt_priority and policy: SCHED_RR is 2.
        assert_eq!((fields[37].as_str(), fields[38].as_str()), ("5", "2"));
    }
}
