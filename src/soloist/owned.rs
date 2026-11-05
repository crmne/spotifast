//! Starts the Soloist child so that it cannot outlive Spotifast.
//!
//! A normal quit stops the child itself, and `kill_on_drop` covers a dropped
//! task. Neither runs when Spotifast is killed (SIGKILL, a crash, Task
//! Manager, a SIGTERM before a handler sees it), and Soloist then kept
//! running as an orphan that held its data directory's lock. Each platform
//! ties the child to this process in the kernel instead:
//!
//! - Linux: the child asks for SIGKILL when its parent dies
//!   (`PR_SET_PDEATHSIG`), from one thread that lives as long as the process.
//! - Windows: the child joins a job object that kills its processes when
//!   Spotifast's handle to it closes, which the system does however the
//!   process ends.
//! - macOS has neither, so a small `/bin/sh` watcher holds the reading end
//!   of a pipe only Spotifast writes to, and kills the child when the pipe
//!   closes with Spotifast.
use std::io;
use tokio::process::{Child, Command};

/// What keeps the child tied to this process. Dropping it unties the child,
/// so it is held until the child has been reaped.
pub struct Bond {
    #[cfg(windows)]
    _job: Option<std::os::windows::io::OwnedHandle>,
    #[cfg(target_os = "macos")]
    _watcher: Option<Watcher>,
}

impl Bond {
    /// Unties the reaped child. On Linux there is nothing to let go: the
    /// kernel forgets the tie with the child.
    pub fn release(self) {}
}

/// Starts `command`, whose child ends with this process however it exits.
/// When the platform's tie cannot be made after the child started, the
/// child still runs and only that is logged.
pub async fn spawn(mut command: Command) -> io::Result<(Child, Bond)> {
    #[cfg(target_os = "linux")]
    {
        die_with_parent(&mut command);
        Ok((spawner::spawn(command).await?, Bond {}))
    }
    #[cfg(windows)]
    {
        let child = command.spawn()?;
        let job = child
            .raw_handle()
            .map(job::contain)
            .transpose()
            .inspect_err(|error| log::warn!("Soloist runs outside a job object: {error}"))
            .ok()
            .flatten();
        Ok((child, Bond { _job: job }))
    }
    #[cfg(target_os = "macos")]
    {
        let child = command.spawn()?;
        let watcher = child
            .id()
            .map(Watcher::start)
            .transpose()
            .inspect_err(|error| log::warn!("Soloist runs without its exit watcher: {error}"))
            .ok()
            .flatten();
        Ok((child, Bond { _watcher: watcher }))
    }
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    {
        Ok((command.spawn()?, Bond {}))
    }
}

/// The child gets SIGKILL when the thread that forked it exits, which is the
/// process dying only if that thread lives as long as the process: see
/// `spawner`. SIGKILL rather than SIGTERM, as a normal stop sends it too
/// (`Child::kill`), and nothing is left to wait for a slower exit.
#[cfg(target_os = "linux")]
fn die_with_parent(command: &mut Command) {
    let parent = libc::pid_t::try_from(std::process::id()).unwrap_or(0);
    // SAFETY: runs in the forked child before exec, and only calls the
    // async-signal-safe prctl and getppid; neither error allocates.
    unsafe {
        command.pre_exec(move || {
            let unused: libc::c_ulong = 0;
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                unused,
                unused,
                unused,
            ) == -1
            {
                return Err(io::Error::last_os_error());
            }
            // Spotifast may have died between the fork and the prctl, before
            // there was a death to be told of: the child has been handed to
            // another parent by now, so it must not go on.
            if libc::getppid() != parent {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

/// Forks the child on one thread that never exits.
///
/// `PR_SET_PDEATHSIG` fires when the forking *thread* ends, not the process.
/// Soloist starts from a task on the backend runtime, whose threads come
/// and go: a `spawn_blocking` thread expires after ten idle seconds, and a
/// worker that enters `block_in_place` gives its place to a new thread and
/// then becomes one of those. A child forked there would be killed while
/// Spotifast still ran. This thread waits on a queue held in a static, so it
/// lasts until the process ends, and only then is the child told.
#[cfg(target_os = "linux")]
mod spawner {
    use std::{io, sync::Mutex, sync::mpsc};
    use tokio::process::{Child, Command};

    type Job = Box<dyn FnOnce() + Send>;

    fn queue() -> io::Result<mpsc::Sender<Job>> {
        static QUEUE: Mutex<Option<mpsc::Sender<Job>>> = Mutex::new(None);
        let mut queue = QUEUE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(queue) = &*queue {
            return Ok(queue.clone());
        }
        let (send, receive) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("soloist-spawner".into())
            .spawn(move || {
                for job in receive {
                    job();
                }
            })?;
        *queue = Some(send.clone());
        Ok(send)
    }

    /// Spawns `command` on the spawning thread, inside the caller's runtime,
    /// which then owns the child's pipes and reaps it as usual.
    pub async fn spawn(mut command: Command) -> io::Result<Child> {
        let runtime = tokio::runtime::Handle::current();
        let (reply, answer) = tokio::sync::oneshot::channel();
        queue()?
            .send(Box::new(move || {
                let _inside = runtime.enter();
                let _ = reply.send(command.spawn());
            }))
            .map_err(|_| io::Error::other("the Soloist spawning thread has stopped"))?;
        answer
            .await
            .map_err(|_| io::Error::other("the Soloist spawning thread has stopped"))?
    }
}

#[cfg(windows)]
mod job {
    use std::{
        io,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// Puts `process` in a new job that kills it when the returned handle,
    /// the only one, closes: when it is dropped, or when this process ends
    /// for any reason and the system closes its handles. Children Soloist
    /// starts join the job with it.
    ///
    /// The child runs for a moment before it joins. Starting it suspended
    /// needs its main thread's handle, which `std::process` does not give.
    pub fn contain(process: RawHandle) -> io::Result<OwnedHandle> {
        // SAFETY: no security attributes and no name make a new private job.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a new handle that nothing else owns.
        let job = unsafe { OwnedHandle::from_raw_handle(job) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the information class matches the structure and its size.
        let set = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `process` is the child's handle, open while the child is.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

/// A `/bin/sh` that kills the child once this process is gone.
///
/// It reads its standard input, a pipe whose writing end only this process
/// holds (Rust opens pipes close-on-exec), so the read ends when Spotifast
/// does, whether it quit, crashed or was killed. It kills only the process
/// that started when the child did: a PID freed by an exit Spotifast has
/// reaped may since belong to an unrelated program. It runs in a process
/// group of its own, so a terminal's Ctrl+C reaches Spotifast, not it.
#[cfg(target_os = "macos")]
pub struct Watcher {
    process: std::process::Child,
    _alive: std::io::PipeWriter,
}

#[cfg(target_os = "macos")]
impl Watcher {
    const SCRIPT: &str = r#"started=$(/bin/ps -o lstart= -p "$1") || exit 0
read -r _
[ "$(/bin/ps -o lstart= -p "$1")" = "$started" ] && kill -KILL "$1""#;

    fn start(child: u32) -> io::Result<Self> {
        use std::os::unix::process::CommandExt;
        let (read, write) = std::io::pipe()?;
        let process = std::process::Command::new("/bin/sh")
            .args(["-c", Self::SCRIPT, "sh", &child.to_string()])
            .stdin(read)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()?;
        Ok(Self {
            process,
            _alive: write,
        })
    }
}

/// Spotifast stops the child itself before it lets the bond go, so the
/// watcher has nothing left to do; it is stopped and reaped rather than
/// left to find that out.
#[cfg(target_os = "macos")]
impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    /// Which part of `a_child_dies_with_its_killed_parent` a re-run of this
    /// test binary plays.
    const ROLE: &str = "SPOTIFAST_OWNED_TEST_ROLE";
    const STAND_IN: &str = "soloist::owned::tests::stand_in";

    fn stand_in_command(role: &str) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([STAND_IN, "--exact", "--ignored", "--test-threads=1"])
            .env(ROLE, role)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    }

    /// A stand-in Spotifast starts a stand-in Soloist the way Spotifast
    /// does and is then killed with SIGKILL (TerminateProcess on Windows),
    /// so no code of its own runs: the stand-in Soloist must go with it.
    /// It must not go earlier, with the short-lived thread that asked for
    /// it, as `PR_SET_PDEATHSIG` would have it on Linux.
    #[cfg(any(target_os = "linux", windows, target_os = "macos"))]
    #[test]
    fn a_child_dies_with_its_killed_parent() {
        let pid_file =
            std::env::temp_dir().join(format!("soloist-owned-test-{}.pid", rand::random::<u64>()));
        let mut parent = stand_in_command(&format!("parent:{}", pid_file.display()))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Some(pid) = text.strip_suffix('\n')
            {
                break pid.parse::<u32>().unwrap();
            }
            assert!(
                parent.try_wait().unwrap().is_none(),
                "the stand-in Spotifast exited before it started its child"
            );
            assert!(Instant::now() < deadline, "the stand-in never started");
            std::thread::sleep(Duration::from_millis(20));
        };
        let _ = std::fs::remove_file(&pid_file);
        let pid = sysinfo::Pid::from_u32(pid);
        let mut system = sysinfo::System::new();
        let alive_before = !super::super::exited(&mut system, pid);

        parent.kill().unwrap();
        parent.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut gone = super::super::exited(&mut system, pid);
        while !gone && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            gone = super::super::exited(&mut system, pid);
        }
        if !gone {
            system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
            if let Some(process) = system.process(pid) {
                process.kill();
            }
        }
        assert!(alive_before, "the child died before its parent did");
        assert!(gone, "the child outlived its killed parent");
    }

    /// Plays a part for `a_child_dies_with_its_killed_parent`, and does
    /// nothing in a run of the ignored tests.
    #[test]
    #[ignore = "a stand-in process for a_child_dies_with_its_killed_parent"]
    fn stand_in() {
        let Ok(role) = std::env::var(ROLE) else {
            return;
        };
        if role == "child" {
            std::thread::sleep(Duration::from_secs(60));
            return;
        }
        let Some(pid_file) = role.strip_prefix("parent:") else {
            return;
        };
        // Blocking threads that expire at once, so the one that starts the
        // child is gone well before the child is checked.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_keep_alive(Duration::from_millis(10))
            .enable_all()
            .build()
            .unwrap();
        let (child, _bond) = runtime.block_on(async {
            tokio::task::spawn_blocking(|| {
                let command = tokio::process::Command::from(stand_in_command("child"));
                tokio::runtime::Handle::current().block_on(super::spawn(command))
            })
            .await
            .unwrap()
            .unwrap()
        });
        std::thread::sleep(Duration::from_millis(500));
        std::fs::write(pid_file, format!("{}\n", child.id().unwrap())).unwrap();
        std::thread::sleep(Duration::from_secs(60));
    }
}
