//! Raises the threads that carry Soloist's audio. Its stream is async, so on
//! a busy machine a thread that wakes late leaves a hole in the music, or
//! hands PipeWire half a cycle, and neither counts as an xrun.
//!
//! The threads are made realtime, SCHED_RR 15. That sits between the two
//! kinds of realtime thread a desktop's audio has. PipeWire's data loops, the
//! daemon's and those of every client, Spotifast's own bridge among them,
//! ask for more than rtkit allows and get its maximum, `MaxRealtimePriority`,
//! which is 20 by default: Soloist must stay below them, since it feeds them.
//! Browsers, through cubeb, and other audio libraries ask rtkit for 10, and a
//! page that plays sound can hold dozens of such threads: Soloist must not
//! wait behind them. 15 is in the middle. On a system whose rtkit allows
//! less than 15, rtkit refuses it and the threads take the fallback below.
//!
//! Soloist is closed source, so realtime is safe only because it runs under
//! `RLIMIT_RTTIME` (`bound_realtime`): a realtime thread that runs that long
//! without sleeping is killed by the kernel instead of stalling the desktop,
//! and Spotifast starts Soloist again. rtkit grants nothing to a process
//! without that limit.
//!
//! rtkit can say no for a while: it allows each user a burst of requests, and
//! a browser starting up can spend it. So nothing is taken from its answer
//! alone: what a thread runs at is read back from `/proc`, a thread that is
//! not realtime is asked again, every 2 seconds at first and at most every 30,
//! and a thread demoted later, as rtkit does to everything it raised when it
//! thinks the machine is starving, is asked again at once. Until realtime is
//! granted a thread gets nice -11 where that is allowed, through rtkit or
//! `RLIMIT_NICE`.
use std::{
    collections::HashMap,
    fmt,
    time::{Duration, Instant},
};

/// Their names in `/proc`, cut to the kernel's 15 bytes.
const AUDIO_THREADS: [&str; 3] = ["pipewire-loop", "Audio Driver Th", "Media Mixer Ren"];
/// SCHED_RR priority: above the 10 browsers ask for, below the 20 PipeWire's
/// data loops get. See the module's notes.
const RT_PRIORITY: u32 = 15;
/// What PipeWire's own module-rt asks for a client that can't have realtime.
const NICE: i32 = -11;
/// The CPU time in microseconds a realtime thread of Soloist may take without
/// sleeping: rtkit's own default ceiling, `RTTimeUSecMax`, and 10 graph
/// cycles at the usual quantum, which no audio thread should come near.
pub const RT_TIME_LIMIT_US: u64 = 200_000;
/// Threads appear with the first playback, and may come and go with it: the
/// mixer's is made anew at each track.
const LOOK_EVERY: Duration = Duration::from_secs(2);
const STOP_CHECK: Duration = Duration::from_millis(250);
/// How long a thread that is not realtime waits before it is asked again:
/// doubled at each refusal, up to the most.
const FIRST_RETRY: Duration = Duration::from_secs(2);
const LAST_RETRY: Duration = Duration::from_secs(30);

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

/// What a thread runs at, as `/proc` has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Realtime(u32),
    Normal(i32),
}
impl Level {
    /// The fields of a thread's `stat` after its name, which may hold spaces.
    fn parse(stat: &str) -> Option<Self> {
        let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        let nice = fields.get(16)?.parse().ok()?;
        let rt_priority = fields.get(37)?.parse().ok()?;
        let policy: i32 = fields.get(38)?.parse().ok()?;
        Some(match policy {
            libc::SCHED_FIFO | libc::SCHED_RR => Self::Realtime(rt_priority),
            _ => Self::Normal(nice),
        })
    }

    /// `None` once the thread has gone.
    fn of(pid: u32, tid: u32) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")).ok()?)
    }

    fn realtime(self) -> bool {
        matches!(self, Self::Realtime(_))
    }

    /// Realtime, or at least the fallback's nice.
    fn raised(self) -> bool {
        match self {
            Self::Realtime(_) => true,
            Self::Normal(nice) => nice <= NICE,
        }
    }
}
impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Realtime(priority) => write!(f, "SCHED_RR {priority}"),
            Self::Normal(nice) => write!(f, "nice {nice}"),
        }
    }
}

/// One audio thread, until it goes.
struct Thread {
    /// Seen realtime when last looked at.
    realtime: bool,
    /// When it may be asked again, and the wait after that.
    next: Instant,
    wait: Duration,
    /// A refusal has been logged for it.
    warned: bool,
}

/// How the child's audio threads all stand, once none is left at normal
/// priority. Logged when it changes, so retries and new threads say nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Summary {
    Realtime(usize),
    Raised { realtime: usize, nice: usize },
}

/// Which of a child's threads run realtime, and when the others are asked
/// again. A thread found realtime is not asked again until it is demoted.
#[derive(Default)]
struct Raised {
    pid: u32,
    threads: HashMap<u32, Thread>,
    summary: Option<Summary>,
    /// Refusals already logged for this child: a new mixer thread at each
    /// track would repeat them.
    reasons: Vec<String>,
}
impl Raised {
    /// Asks `raise` for whatever among `threads` is due and not realtime, and
    /// believes only what `level` reads afterwards. `level` is `None` for a
    /// thread that has gone, and `raise` gives the refusal's reason.
    fn pass(
        &mut self,
        now: Instant,
        pid: u32,
        threads: &[u32],
        level: impl Fn(u32) -> Option<Level>,
        mut raise: impl FnMut(u32) -> Result<(), String>,
    ) {
        if self.pid != pid {
            *self = Self {
                pid,
                ..Self::default()
            };
        }
        self.threads.retain(|tid, _| threads.contains(tid));
        let mut levels = Vec::with_capacity(threads.len());
        for &tid in threads {
            let Some(mut at) = level(tid) else {
                self.threads.remove(&tid);
                continue;
            };
            let thread = self.threads.entry(tid).or_insert(Thread {
                realtime: false,
                next: now,
                wait: FIRST_RETRY,
                warned: false,
            });
            if !at.realtime() && thread.realtime {
                log::warn!(
                    "Soloist's audio thread {tid} was demoted to {at}; asking for realtime again"
                );
                thread.next = now;
                thread.wait = FIRST_RETRY;
            }
            if !at.realtime() && now >= thread.next {
                let refused = raise(tid).err();
                at = level(tid).unwrap_or(at);
                if !at.realtime() {
                    thread.next = now + thread.wait;
                    thread.wait = (thread.wait * 2).min(LAST_RETRY);
                    let why =
                        refused.unwrap_or_else(|| format!("rtkit agreed, but it runs at {at}"));
                    if !thread.warned {
                        thread.warned = true;
                        if self.reasons.contains(&why) {
                            log::debug!("Soloist's audio thread {tid} runs at {at}: {why}");
                        } else {
                            log::warn!(
                                "Soloist's audio thread {tid} runs at {at}, not realtime, and is asked again from time to time: {why}"
                            );
                            self.reasons.push(why);
                        }
                    }
                }
            }
            if at.realtime() && !thread.realtime {
                thread.wait = FIRST_RETRY;
                if thread.warned {
                    log::info!("Soloist's audio thread {tid} runs realtime at last, {at}");
                }
            }
            thread.realtime = at.realtime();
            levels.push(at);
        }
        self.summarize(&levels);
    }

    fn summarize(&mut self, levels: &[Level]) {
        if levels.is_empty() {
            return;
        }
        let realtime = levels.iter().filter(|at| at.realtime()).count();
        let summary = if realtime == levels.len() {
            Some(Summary::Realtime(realtime))
        } else if levels.iter().all(|at| at.raised()) {
            Some(Summary::Raised {
                realtime,
                nice: levels.len() - realtime,
            })
        } else {
            None
        };
        if summary == self.summary {
            return;
        }
        // Logged when the kind changes, not when a count does: the mixer's
        // thread is replaced at each track.
        let changed = summary.as_ref().map(std::mem::discriminant)
            != self.summary.as_ref().map(std::mem::discriminant);
        self.summary = summary;
        if !changed {
            return;
        }
        match summary {
            Some(Summary::Realtime(count)) => log::info!(
                "Soloist's audio threads all run realtime, SCHED_RR {RT_PRIORITY} ({count} so far)"
            ),
            Some(Summary::Raised { realtime, nice }) => log::info!(
                "Soloist's audio threads are all raised: {realtime} realtime, {nice} at nice {NICE} until rtkit allows realtime"
            ),
            None => {}
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
    let called = connection.call_method(Some(RTKIT), RTKIT_PATH, Some(RTKIT), method, body);
    // A refusal is rtkit's answer. Anything else may be a connection that
    // broke, which would refuse every retry after it, so the next call makes
    // a new one.
    if called
        .as_ref()
        .is_err_and(|error| !matches!(error, zbus::Error::MethodError(..)))
    {
        *bus = None;
    }
    called?;
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

/// Realtime, else nice -11 through rtkit, else through RLIMIT_NICE. Whether
/// any of it took is for `/proc` to say; the error is rtkit's.
fn raise(bus: &mut Option<zbus::blocking::Connection>, pid: u32, tid: u32) -> Result<(), String> {
    let refused = match realtime(bus, pid, tid) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    // Spends no more of rtkit's burst on a thread that has nice already.
    if !Level::of(pid, tid).is_some_and(Level::raised) && high_priority(bus, pid, tid).is_err() {
        renice(tid);
    }
    Err(format!("rtkit refused realtime: {refused}"))
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
        let now = Instant::now();
        looked = Some(now);
        let pid = child();
        if pid == 0 {
            continue;
        }
        raised.pass(
            now,
            pid,
            &audio_threads(pid),
            |tid| Level::of(pid, tid),
            |tid| raise(&mut bus, pid, tid),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, sync::mpsc};

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

    /// A fake machine: what each thread runs at, and what rtkit does when
    /// asked, which can change between passes.
    struct Fake {
        levels: RefCell<HashMap<u32, Level>>,
        grants: RefCell<Option<Level>>,
        asked: RefCell<Vec<u32>>,
    }
    impl Fake {
        fn new(threads: &[u32]) -> Self {
            Self {
                levels: RefCell::new(threads.iter().map(|&tid| (tid, NORMAL)).collect()),
                grants: RefCell::new(None),
                asked: RefCell::default(),
            }
        }

        fn pass(&self, raised: &mut Raised, now: Instant, pid: u32, threads: &[u32]) {
            raised.pass(
                now,
                pid,
                threads,
                |tid| self.levels.borrow().get(&tid).copied(),
                |tid| {
                    self.asked.borrow_mut().push(tid);
                    match *self.grants.borrow() {
                        Some(level) => {
                            self.levels.borrow_mut().insert(tid, level);
                            Ok(())
                        }
                        None => Err("burst limit".into()),
                    }
                },
            );
        }

        fn asked(&self) -> Vec<u32> {
            std::mem::take(&mut self.asked.borrow_mut())
        }
    }

    const NORMAL: Level = Level::Normal(0);
    const REALTIME: Level = Level::Realtime(RT_PRIORITY);

    fn secs(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    /// A thread found realtime is not asked again, a new one is asked as it
    /// appears, and a restarted child starts over: its thread ids are its own.
    #[test]
    fn a_realtime_thread_is_asked_for_once_and_new_ones_when_they_appear() {
        let start = Instant::now();
        let fake = Fake::new(&[1, 2, 3]);
        *fake.grants.borrow_mut() = Some(REALTIME);
        let mut raised = Raised::default();
        fake.pass(&mut raised, start, 7, &[1, 2]);
        assert_eq!(fake.asked(), [1, 2]);
        fake.pass(&mut raised, secs(start, 2), 7, &[1, 2, 3]);
        assert_eq!(fake.asked(), [3]);
        fake.pass(&mut raised, secs(start, 60), 7, &[1, 2, 3]);
        assert_eq!(fake.asked(), Vec::<u32>::new());
        fake.levels.borrow_mut().insert(1, NORMAL);
        fake.pass(&mut raised, secs(start, 62), 8, &[1]);
        assert_eq!(fake.asked(), [1]);
    }

    /// A refused thread is asked again after 2, 4, 8 and 16 seconds, then
    /// every 30, until rtkit agrees; then never again.
    #[test]
    fn a_refused_thread_is_asked_again_with_backoff_until_it_is_realtime() {
        let start = Instant::now();
        let fake = Fake::new(&[1]);
        let mut raised = Raised::default();
        let mut asked_at = Vec::new();
        for second in 0..=150 {
            if second == 125 {
                *fake.grants.borrow_mut() = Some(REALTIME);
            }
            fake.pass(&mut raised, secs(start, second), 7, &[1]);
            if !fake.asked().is_empty() {
                asked_at.push(second);
            }
        }
        assert_eq!(asked_at, [0, 2, 6, 14, 30, 60, 90, 120, 150]);
        assert!(raised.threads[&1].realtime);
        assert_eq!(raised.summary, Some(Summary::Realtime(1)));
    }

    /// rtkit's answer alone proves nothing: a thread it claims to have raised
    /// but that `/proc` shows at normal priority is asked again.
    #[test]
    fn a_grant_that_did_not_take_is_asked_again() {
        let start = Instant::now();
        let fake = Fake::new(&[1]);
        *fake.grants.borrow_mut() = Some(NORMAL);
        let mut raised = Raised::default();
        fake.pass(&mut raised, start, 7, &[1]);
        fake.pass(&mut raised, secs(start, 2), 7, &[1]);
        assert_eq!(fake.asked(), [1, 1]);
        assert_eq!(raised.summary, None);
    }

    /// A realtime thread demoted later is asked again at once, and its wait
    /// starts over from 2 seconds.
    #[test]
    fn a_demoted_thread_is_raised_again_at_once() {
        let start = Instant::now();
        let fake = Fake::new(&[1, 2]);
        *fake.grants.borrow_mut() = Some(REALTIME);
        let mut raised = Raised::default();
        fake.pass(&mut raised, start, 7, &[1, 2]);
        assert_eq!(fake.asked(), [1, 2]);
        *fake.grants.borrow_mut() = None;
        fake.levels.borrow_mut().insert(2, NORMAL);
        fake.pass(&mut raised, secs(start, 40), 7, &[1, 2]);
        assert_eq!(fake.asked(), [2]);
        assert_eq!(raised.summary, None);
        fake.pass(&mut raised, secs(start, 41), 7, &[1, 2]);
        assert_eq!(fake.asked(), Vec::<u32>::new());
        *fake.grants.borrow_mut() = Some(REALTIME);
        fake.pass(&mut raised, secs(start, 42), 7, &[1, 2]);
        assert_eq!(fake.asked(), [2]);
        assert_eq!(raised.summary, Some(Summary::Realtime(2)));
    }

    /// Threads that have gone are forgotten, and a thread that is at nice
    /// -11 counts as raised while it waits for realtime.
    #[test]
    fn gone_threads_are_forgotten_and_nice_counts_as_raised() {
        let start = Instant::now();
        let fake = Fake::new(&[1, 2]);
        *fake.grants.borrow_mut() = Some(Level::Normal(NICE));
        let mut raised = Raised::default();
        fake.pass(&mut raised, start, 7, &[1, 2]);
        assert_eq!(
            raised.summary,
            Some(Summary::Raised {
                realtime: 0,
                nice: 2
            })
        );
        fake.levels.borrow_mut().remove(&2);
        fake.pass(&mut raised, secs(start, 2), 7, &[1, 2]);
        assert_eq!(raised.threads.keys().collect::<Vec<_>>(), [&1]);
        fake.pass(&mut raised, secs(start, 3), 7, &[1]);
        assert_eq!(raised.threads.len(), 1);
    }

    #[test]
    fn a_thread_s_level_is_read_from_its_stat() {
        let stat = |nice: &str, rt: &str, policy: &str| {
            let mut fields = vec!["0"; 39];
            fields[16] = nice;
            fields[37] = rt;
            fields[38] = policy;
            format!("12 (Media Mixer Ren) {}", fields.join(" "))
        };
        assert_eq!(
            Level::parse(&stat("0", "15", "2")),
            Some(Level::Realtime(15))
        );
        assert_eq!(
            Level::parse(&stat("0", "10", "1")),
            Some(Level::Realtime(10))
        );
        assert_eq!(
            Level::parse(&stat("-11", "0", "0")),
            Some(Level::Normal(-11))
        );
        assert_eq!(Level::parse("12 (x) 1 2"), None);
        let (tid, _keep) = named("Audio Driver Th");
        assert!(matches!(
            Level::of(std::process::id(), tid),
            Some(Level::Normal(_))
        ));
        assert_eq!(Level::of(std::process::id(), u32::MAX), None);
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
        assert_eq!(
            Level::of(std::process::id(), tid),
            Some(Level::Normal(NICE))
        );
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
        assert_eq!(Level::of(std::process::id(), tid), Some(REALTIME));
    }
}
