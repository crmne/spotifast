//! The device's power and session state, from the operating system.
//!
//! A thin reader per platform writes what the system reports into shared
//! [`Conditions`] and queues [`Event`]s, then wakes the app. The app reads
//! both in its logic pass, which runs with or without a window, and turns
//! the conditions into a [`Budget`] for its drawing and polling.
//!
//! Shaped like a fastframe crate (a spawned watcher, a pure policy, thin
//! platform readers) so it can move there once a second app needs it.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
#[cfg(any(target_os = "windows", target_os = "linux", test))]
use std::time::Duration;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// What the system says about power, the display, the session and the
/// network. A platform that cannot tell leaves the default, which is the
/// unconstrained case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conditions {
    /// The user asked the system to save energy: Energy Saver or battery
    /// saver on Windows, Low Power Mode on macOS, the power-saver profile on
    /// Linux.
    pub saver: bool,
    /// Running on battery.
    pub on_battery: bool,
    /// Every display is off.
    pub display_off: bool,
    /// The session is locked or switched away from.
    pub locked: bool,
    /// The system reports a way to the internet.
    pub online: bool,
    /// The connection is metered or charged by use.
    pub metered: bool,
}

impl Default for Conditions {
    fn default() -> Self {
        Self {
            saver: false,
            on_battery: false,
            display_off: false,
            locked: false,
            online: true,
            metered: false,
        }
    }
}

/// Something the system is about to do, or has just done.
#[derive(Debug)]
pub enum Event {
    /// The system is about to sleep. Save and pause, then [`Ack::done`].
    Suspending(Ack),
    /// The system woke up.
    Resumed,
    /// The user is signing out or shutting down. Save, then [`Ack::done`].
    EndingSession(Ack),
}

/// Lets a reader wait, for a bounded time, until the app has handled an
/// event the system will not wait long for.
#[derive(Clone, Default)]
pub struct Ack(Arc<(Mutex<bool>, Condvar)>);

/// Events are logged; the lock and the condition variable inside say
/// nothing worth reading there.
impl std::fmt::Debug for Ack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ack")
    }
}

impl Ack {
    /// The app has done what the event asked.
    pub fn done(&self) {
        let (done, ring) = &*self.0;
        *done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        ring.notify_all();
    }

    /// Whether the app has answered.
    #[cfg(test)]
    pub fn is_done(&self) -> bool {
        *self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until [`done`](Self::done) or `timeout`, whichever is first.
    /// Returns whether the app answered in time.
    #[cfg(any(target_os = "windows", target_os = "linux", test))]
    fn wait(&self, timeout: Duration) -> bool {
        let (done, ring) = &*self.0;
        let done = done.lock().unwrap_or_else(PoisonError::into_inner);
        let (done, _) = ring
            .wait_timeout_while(done, timeout, |done| !*done)
            .unwrap_or_else(PoisonError::into_inner);
        *done
    }
}

/// How much the app may spend on drawing and polling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Budget {
    /// As designed.
    #[default]
    Normal,
    /// The user asked to save energy, or the window is not in focus:
    /// animations run at half rate.
    Saver,
    /// Nobody can see the window (none shown, the display off or the session
    /// locked): nothing is drawn and polling slows down.
    Background,
}

/// The budget for `conditions`, given whether a window is shown and focused.
pub fn budget(conditions: Conditions, window_shown: bool, focused: bool) -> Budget {
    if !window_shown || conditions.display_off || conditions.locked {
        Budget::Background
    } else if conditions.saver || !focused {
        Budget::Saver
    } else {
        Budget::Normal
    }
}

/// How long a reader waits for the app to save before the system sleeps.
/// Windows gives a suspend callback about two seconds.
#[cfg(any(target_os = "windows", target_os = "linux"))]
const SUSPEND_ACK: Duration = Duration::from_millis(1500);
/// How long a reader holds back the end of the session for the app to save.
/// Windows waits about five seconds before it lists the app as blocking.
#[cfg(target_os = "windows")]
const END_SESSION_ACK: Duration = Duration::from_secs(3);

/// What the readers write and the app reads.
/// Run by the reader itself as the system is about to sleep.
#[cfg(any(target_os = "windows", target_os = "linux", test))]
type Hook = Box<dyn Fn() + Send + Sync>;

struct Shared {
    conditions: Mutex<Conditions>,
    events: Mutex<Vec<Event>>,
    wake: Box<dyn Fn() + Send + Sync>,
    #[cfg(any(target_os = "windows", target_os = "linux", test))]
    on_suspending: Mutex<Option<Hook>>,
    /// When the system last said it was going to sleep, by the wall clock,
    /// which keeps counting through it.
    #[cfg(any(target_os = "windows", target_os = "linux", test))]
    suspended_at: Mutex<Option<std::time::SystemTime>>,
}

impl Shared {
    fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            conditions: Mutex::new(Conditions::default()),
            events: Mutex::new(Vec::new()),
            wake: Box::new(wake),
            #[cfg(any(target_os = "windows", target_os = "linux", test))]
            on_suspending: Mutex::new(None),
            #[cfg(any(target_os = "windows", target_os = "linux", test))]
            suspended_at: Mutex::new(None),
        }
    }

    /// Applies `change` and wakes the app if it changed anything.
    fn update(&self, change: impl FnOnce(&mut Conditions)) {
        let mut conditions = self
            .conditions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = *conditions;
        change(&mut conditions);
        let after = *conditions;
        drop(conditions);
        if after != before {
            log::info!("power and session: {after:?}");
            (self.wake)();
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux", test))]
    fn push(&self, event: Event) {
        log::info!("power and session: {event:?}");
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
        (self.wake)();
    }

    #[cfg(any(target_os = "windows", target_os = "linux", test))]
    fn take_suspended_at(&self) -> Option<std::time::SystemTime> {
        self.suspended_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// No reader here says when the system sleeps.
    #[cfg(not(any(target_os = "windows", target_os = "linux", test)))]
    fn take_suspended_at(&self) -> Option<std::time::SystemTime> {
        None
    }

    /// Queues the event `make` builds and waits up to `timeout` for the app
    /// to answer it.
    #[cfg(any(target_os = "windows", target_os = "linux", test))]
    fn push_and_wait(&self, make: impl FnOnce(Ack) -> Event, timeout: Duration) {
        let ack = Ack::default();
        let event = make(ack.clone());
        if matches!(event, Event::Suspending(_)) {
            // The app's logic pass may not run in time while the display
            // goes off, so the time is taken here, and what cannot wait for
            // it (pausing the music) starts here too.
            *self
                .suspended_at
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(std::time::SystemTime::now());
            if let Some(hook) = &*self
                .on_suspending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
            {
                hook();
            }
        }
        self.push(event);
        if !ack.wait(timeout) {
            log::warn!("power and session: the app did not answer within {timeout:?}");
        }
    }
}

/// Watches the system's power and session state for the app.
pub struct Power {
    shared: Arc<Shared>,
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    _reader: Option<Reader>,
    #[cfg(target_os = "windows")]
    eco_qos: windows::EcoQos,
}

#[cfg(target_os = "linux")]
use self::linux::Reader;
#[cfg(target_os = "macos")]
use self::macos::Reader;
#[cfg(target_os = "windows")]
use self::windows::Reader;

impl Power {
    /// Starts the platform's reader; `wake` runs on a system thread whenever
    /// something changes.
    pub fn spawn(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared::new(wake));
        Self {
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            _reader: Reader::start(Arc::clone(&shared))
                .inspect_err(|error| log::warn!("power and session state unavailable: {error}"))
                .ok(),
            #[cfg(target_os = "windows")]
            eco_qos: windows::EcoQos::default(),
            shared,
        }
    }

    /// Conditions that never change, for tests and for apps without a reader.
    pub fn fixed(conditions: Conditions) -> Self {
        let shared = Arc::new(Shared::new(|| {}));
        *shared
            .conditions
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = conditions;
        Self {
            shared,
            #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
            _reader: None,
            #[cfg(target_os = "windows")]
            eco_qos: windows::EcoQos::default(),
        }
    }

    /// Tells the system whether the app runs where nobody can see it, so
    /// the work it still does can run on efficient cores (EcoQoS on
    /// Windows). Only a watching `Power` touches the process.
    pub fn set_background(&mut self, background: bool) {
        #[cfg(target_os = "windows")]
        if self._reader.is_some() {
            self.eco_qos.set(background);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = background;
    }

    /// The latest conditions.
    pub fn conditions(&self) -> Conditions {
        *self
            .shared
            .conditions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Has the reader run `hook` itself as the system is about to sleep,
    /// before the app's logic pass gets to the event, which it may not do
    /// in time while the display goes off. For work that must not wait,
    /// such as pausing the music.
    pub fn on_suspending(&self, hook: impl Fn() + Send + Sync + 'static) {
        #[cfg(any(target_os = "windows", target_os = "linux", test))]
        {
            *self
                .shared
                .on_suspending
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(hook));
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux", test)))]
        let _ = hook;
    }

    /// When the system last said it was going to sleep, taken once. The
    /// reader notes it as the system says so, however late the app's logic
    /// pass reads the event.
    pub fn take_suspended_at(&self) -> Option<std::time::SystemTime> {
        self.shared.take_suspended_at()
    }

    /// The events since the last call, oldest first.
    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(
            &mut *self
                .shared
                .events
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Queues `event` as a reader would, for tests elsewhere.
    #[cfg(test)]
    pub fn emit(&self, event: Event) {
        self.shared.push(event);
    }

    /// Changes the conditions as a reader would, for tests elsewhere.
    #[cfg(test)]
    pub fn set(&self, change: impl FnOnce(&mut Conditions)) {
        self.shared.update(change);
    }

    /// Reads the platform's conditions that have no change notification.
    /// Cheap; the app calls it from its logic pass now and then. Only a
    /// watching `Power` reads them: a fixed one keeps what it was given.
    pub fn refresh(&self) {
        #[cfg(target_os = "macos")]
        if self._reader.is_some() {
            macos::refresh(&self.shared);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_drawn_when_nobody_can_see_it() {
        let normal = Conditions::default();
        assert_eq!(budget(normal, true, true), Budget::Normal);
        assert_eq!(budget(normal, false, true), Budget::Background, "no window");
        for conditions in [
            Conditions {
                display_off: true,
                ..normal
            },
            Conditions {
                locked: true,
                ..normal
            },
        ] {
            assert_eq!(budget(conditions, true, true), Budget::Background);
        }
    }

    /// Demo runs and tests get a fixed `Power`, and the device's own state
    /// (Low Power Mode on a Mac) must not leak into what they see.
    #[test]
    fn a_fixed_monitor_keeps_its_conditions() {
        let given = Conditions {
            online: false,
            ..Conditions::default()
        };
        let power = Power::fixed(given);
        power.refresh();
        assert_eq!(power.conditions(), given);
    }

    #[test]
    fn saving_energy_or_losing_focus_halves_the_animations() {
        let normal = Conditions::default();
        let saving = Conditions {
            saver: true,
            ..normal
        };
        assert_eq!(budget(saving, true, true), Budget::Saver);
        assert_eq!(budget(normal, true, false), Budget::Saver);
        assert_eq!(
            budget(
                Conditions {
                    on_battery: true,
                    ..normal
                },
                true,
                true
            ),
            Budget::Normal,
            "battery alone is not a request to save"
        );
    }

    #[test]
    fn a_change_wakes_the_app_once_and_a_repeat_does_not() {
        let woken = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let count = Arc::clone(&woken);
        let shared = Shared::new(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        shared.update(|conditions| conditions.saver = true);
        shared.update(|conditions| conditions.saver = true);
        shared.update(|conditions| conditions.online = false);
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn a_reader_waits_for_the_answer_but_not_forever() {
        let power = Power::fixed(Conditions::default());
        let shared = Arc::clone(&power.shared);
        let answered = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            shared.push_and_wait(Event::Suspending, Duration::from_secs(30));
            started.elapsed()
        });
        let ack = loop {
            if let Some(Event::Suspending(ack)) = power.take_events().pop() {
                break ack;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        ack.done();
        assert!(answered.join().unwrap() < Duration::from_secs(10));

        let started = std::time::Instant::now();
        power
            .shared
            .push_and_wait(Event::EndingSession, Duration::from_millis(30));
        assert!(started.elapsed() >= Duration::from_millis(25), "gave up");
    }

    /// The reader starts what must not wait for the app, and notes when the
    /// system went to sleep, before it waits for the answer; a session that
    /// is ending runs neither.
    #[test]
    fn going_to_sleep_starts_at_once_on_the_reader() {
        let power = Power::fixed(Conditions::default());
        let paused = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let count = Arc::clone(&paused);
        power.on_suspending(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let quick = Duration::from_millis(10);
        power.shared.push_and_wait(Event::EndingSession, quick);
        assert_eq!(paused.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(power.take_suspended_at().is_none());

        power.shared.push_and_wait(Event::Suspending, quick);
        assert_eq!(paused.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(power.take_suspended_at().is_some());
        assert!(power.take_suspended_at().is_none(), "taken once");
    }

    #[test]
    fn events_read_plainly_in_the_log() {
        let event = Event::Suspending(Ack::default());
        assert_eq!(format!("{event:?}"), "Suspending(Ack)");
    }
}
