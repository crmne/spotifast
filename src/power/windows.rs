//! Windows: power settings, sleep and wake, the network's reach and cost,
//! the session lock and the end of the session.
//!
//! Power settings, sleep and the network come through system callbacks that
//! need no window, on threads of the system's pool. The session lock and the
//! end of the session are window messages, so a thread of its own keeps a
//! hidden top-level window for them (a message-only window does not get
//! them).
//! <https://learn.microsoft.com/en-us/windows/win32/power/power-setting-guids>

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::Arc;

use windows_sys::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::NetworkManagement::IpHelper::CancelMibChangeNotify2;
use windows_sys::Win32::Networking::WinSock::{
    NL_NETWORK_CONNECTIVITY_HINT, NetworkConnectivityCostHintFixed,
    NetworkConnectivityCostHintVariable, NetworkConnectivityLevelHintLocalAccess,
    NetworkConnectivityLevelHintNone,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY, POWERBROADCAST_SETTING,
    PowerRegisterSuspendResumeNotification, PowerSettingRegisterNotification,
    PowerSettingUnregisterNotification, PowerUnregisterSuspendResumeNotification,
};
use windows_sys::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows_sys::Win32::System::SystemServices::{
    GUID_ACDC_POWER_SOURCE, GUID_CONSOLE_DISPLAY_STATE, GUID_POWER_SAVING_STATUS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DEVICE_NOTIFY_CALLBACK, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PBT_POWERSETTINGCHANGE, PostMessageW,
    PostQuitMessage, RegisterClassW, WM_CLOSE, WM_DESTROY, WM_ENDSESSION, WM_QUERYENDSESSION,
    WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT,
    WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};
use windows_sys::core::GUID;

use super::{END_SESSION_ACK, Event, SUSPEND_ACK, Shared};

/// Energy Saver, Windows 11 24H2's successor to battery saver, which
/// windows-sys does not define yet. From winnt.h in SDK 10.0.26100.
const GUID_ENERGY_SAVER_STATUS: GUID = GUID::from_u128(0x550e8400_e29b_41d4_a716_446655440000);

/// The power settings to follow, each reported at once and on every change.
const SETTINGS: [GUID; 4] = [
    GUID_ACDC_POWER_SOURCE,
    GUID_ENERGY_SAVER_STATUS,
    GUID_POWER_SAVING_STATUS,
    GUID_CONSOLE_DISPLAY_STATE,
];

/// What one power setting says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setting {
    OnBattery(bool),
    EnergySaver(bool),
    BatterySaver(bool),
    DisplayOff(bool),
}

/// Reads a power setting's value: the power source is 0 on AC, 1 on battery
/// and 2 on a UPS; the savers are 0 when off; the display is 0 off, 1 on and
/// 2 dimmed.
fn setting(guid: &GUID, value: u32) -> Option<Setting> {
    if same(guid, &GUID_ACDC_POWER_SOURCE) {
        Some(Setting::OnBattery(value != 0))
    } else if same(guid, &GUID_ENERGY_SAVER_STATUS) {
        Some(Setting::EnergySaver(value != 0))
    } else if same(guid, &GUID_POWER_SAVING_STATUS) {
        Some(Setting::BatterySaver(value != 0))
    } else if same(guid, &GUID_CONSOLE_DISPLAY_STATE) {
        Some(Setting::DisplayOff(value == 0))
    } else {
        None
    }
}

fn same(a: &GUID, b: &GUID) -> bool {
    (a.data1, a.data2, a.data3, a.data4) == (b.data1, b.data2, b.data3, b.data4)
}

/// Whether the network reaches the internet, and whether it is metered. An
/// unknown level counts as online, so a system that cannot tell never holds
/// anything back.
fn network(hint: &NL_NETWORK_CONNECTIVITY_HINT) -> (bool, bool) {
    let level = hint.ConnectivityLevel;
    let online = level != NetworkConnectivityLevelHintNone
        && level != NetworkConnectivityLevelHintLocalAccess;
    let cost = hint.ConnectivityCost;
    let metered = cost == NetworkConnectivityCostHintFixed
        || cost == NetworkConnectivityCostHintVariable
        || hint.Roaming
        || hint.OverDataLimit;
    (online, metered)
}

/// The session's lock and its connection to a console or remote client, as
/// the session changes report them. They are kept apart because a session
/// switched back to the console is connected again but still shows the
/// lock screen until the user unlocks it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SessionState {
    locked: bool,
    disconnected: bool,
}

impl SessionState {
    /// The state after a session change, or `None` for a change that says
    /// nothing about either.
    fn apply(self, code: u32) -> Option<Self> {
        match code {
            WTS_SESSION_LOCK => Some(Self {
                locked: true,
                ..self
            }),
            WTS_SESSION_UNLOCK => Some(Self {
                locked: false,
                ..self
            }),
            WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => Some(Self {
                disconnected: true,
                ..self
            }),
            WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => Some(Self {
                disconnected: false,
                ..self
            }),
            _ => None,
        }
    }

    /// Nobody can see the session: it is locked, or away from every console
    /// and remote client.
    fn hidden(self) -> bool {
        self.locked || self.disconnected
    }
}

/// What the callbacks share: the app's side, and the two savers, which
/// together make `Conditions::saver`.
struct Context {
    shared: Arc<Shared>,
    savers: std::sync::Mutex<(bool, bool)>,
}

impl Context {
    fn apply(&self, setting: Setting) {
        match setting {
            Setting::OnBattery(on) => self.shared.update(|c| c.on_battery = on),
            Setting::DisplayOff(off) => self.shared.update(|c| c.display_off = off),
            Setting::EnergySaver(on) | Setting::BatterySaver(on) => {
                let mut savers = self
                    .savers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if matches!(setting, Setting::EnergySaver(_)) {
                    savers.0 = on;
                } else {
                    savers.1 = on;
                }
                // Held through the update: the two settings can change
                // together, on two of the system's threads, and the one
                // that read the savers last must be the one written last.
                let saver = savers.0 || savers.1;
                self.shared.update(|c| c.saver = saver);
                drop(savers);
            }
        }
    }
}

/// The registrations, undone in reverse when the reader is dropped.
pub(super) struct Reader {
    context: *mut Context,
    params: *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS,
    settings: Vec<HPOWERNOTIFY>,
    suspend: Option<HPOWERNOTIFY>,
    network: Option<HANDLE>,
    session: Option<SessionWindow>,
}

// SAFETY: the raw pointers are owned by the reader and only freed in `drop`,
// after every registration that could still use them is undone.
unsafe impl Send for Reader {}

impl Reader {
    pub(super) fn start(shared: Arc<Shared>) -> Result<Self, String> {
        let context = Box::into_raw(Box::new(Context {
            shared: Arc::clone(&shared),
            savers: std::sync::Mutex::new((false, false)),
        }));
        let params = Box::into_raw(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(on_power),
            Context: context.cast(),
        }));
        let mut reader = Self {
            context,
            params,
            settings: Vec::new(),
            suspend: None,
            network: None,
            session: None,
        };
        for guid in &SETTINGS {
            let mut handle: *mut c_void = std::ptr::null_mut();
            // SAFETY: `guid` and `params` stay alive until the registration
            // is undone in `drop`; the recipient is the subscribe parameters,
            // as DEVICE_NOTIFY_CALLBACK asks.
            let error = unsafe {
                PowerSettingRegisterNotification(
                    guid,
                    DEVICE_NOTIFY_CALLBACK,
                    params as HANDLE,
                    &mut handle,
                )
            };
            if error == 0 {
                reader.settings.push(handle as HPOWERNOTIFY);
            } else {
                log::debug!("power setting {:08x}: not followed ({error})", guid.data1);
            }
        }
        let mut handle: *mut c_void = std::ptr::null_mut();
        // SAFETY: as above.
        let error = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                params as HANDLE,
                &mut handle,
            )
        };
        if error == 0 {
            reader.suspend = Some(handle as HPOWERNOTIFY);
        } else {
            log::warn!("sleep and wake are not followed ({error})");
        }
        reader.network = follow_network(context);
        reader.session = SessionWindow::spawn(shared)
            .inspect_err(|error| log::warn!("the session lock is not followed: {error}"))
            .ok();
        Ok(reader)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: each handle came from its registration and is undone once.
        unsafe {
            for handle in self.settings.drain(..) {
                PowerSettingUnregisterNotification(handle);
            }
            if let Some(handle) = self.suspend.take() {
                PowerUnregisterSuspendResumeNotification(handle);
            }
            if let Some(handle) = self.network.take() {
                CancelMibChangeNotify2(handle);
            }
        }
        self.session.take();
        // SAFETY: no registration uses them any more.
        unsafe {
            drop(Box::from_raw(self.params));
            drop(Box::from_raw(self.context));
        }
    }
}

/// A power setting changed, or the system is about to sleep or woke up.
unsafe extern "system" fn on_power(
    context: *const c_void,
    kind: u32,
    setting: *const c_void,
) -> u32 {
    // SAFETY: the context is the reader's `Context`, alive while registered.
    let context = unsafe { &*context.cast::<Context>() };
    match kind {
        PBT_POWERSETTINGCHANGE if !setting.is_null() => {
            // SAFETY: for this kind the system passes a POWERBROADCAST_SETTING
            // whose data holds `DataLength` bytes.
            let (guid, value) = unsafe {
                let setting = &*setting.cast::<POWERBROADCAST_SETTING>();
                let value = (setting.DataLength >= 4)
                    .then(|| std::ptr::read_unaligned(setting.Data.as_ptr().cast::<u32>()));
                (setting.PowerSetting, value)
            };
            if let Some(setting) = value.and_then(|value| self::setting(&guid, value)) {
                context.apply(setting);
            }
        }
        // The system waits a little for this callback: long enough to save.
        PBT_APMSUSPEND => context.shared.push_and_wait(Event::Suspending, SUSPEND_ACK),
        PBT_APMRESUMEAUTOMATIC => context.shared.push(Event::Resumed),
        _ => {}
    }
    0
}

type HintCallback = unsafe extern "system" fn(*const c_void, NL_NETWORK_CONNECTIVITY_HINT);
type NotifyHint =
    unsafe extern "system" fn(Option<HintCallback>, *const c_void, bool, *mut HANDLE) -> u32;

/// Follows the network's reach and cost. Looked up at run time: the call
/// arrived in Windows 10 2004, and a missing import would stop the app from
/// starting on anything older.
fn follow_network(context: *mut Context) -> Option<HANDLE> {
    let library: Vec<u16> = "iphlpapi.dll".encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated name; iphlpapi stays loaded for the process.
    let notify = unsafe {
        let module = LoadLibraryW(library.as_ptr());
        if module.is_null() {
            return None;
        }
        GetProcAddress(
            module,
            c"NotifyNetworkConnectivityHintChange".as_ptr().cast(),
        )?
    };
    // SAFETY: the function has this signature (netioapi.h).
    let notify =
        unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, NotifyHint>(notify) };
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: the context outlives the registration, which `drop` cancels.
    let error = unsafe {
        notify(
            Some(on_network),
            context.cast_const().cast(),
            true,
            &mut handle,
        )
    };
    if error == 0 {
        Some(handle)
    } else {
        log::warn!("the network is not followed ({error})");
        None
    }
}

unsafe extern "system" fn on_network(context: *const c_void, hint: NL_NETWORK_CONNECTIVITY_HINT) {
    // SAFETY: the context is the reader's `Context`, alive while registered.
    let context = unsafe { &*context.cast::<Context>() };
    let (online, metered) = network(&hint);
    context.shared.update(|c| {
        c.online = online;
        c.metered = metered;
    });
}

/// What to do with the process's power throttling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Throttle {
    /// Write this: on is EcoQoS, off hands the choice back to the system.
    Write(bool),
    /// Already as wanted.
    Leave,
    /// Someone else manages it (the Task Manager's efficiency mode, say):
    /// stop touching it for the rest of the run.
    GiveUp,
}

/// Decides, from what the app last wrote (`None`: nothing yet), the current
/// setting (`None`: not controlled, the system decides) and whether the
/// process runs at idle priority, how to get to `want`.
fn throttle(written: Option<bool>, current: Option<bool>, idle: bool, want: bool) -> Throttle {
    let expected = match written {
        None => None,
        Some(true) => Some(true),
        Some(false) => None,
    };
    if idle || current != expected {
        Throttle::GiveUp
    } else if written.unwrap_or(false) == want {
        Throttle::Leave
    } else {
        Throttle::Write(want)
    }
}

/// Puts the process under EcoQoS while nobody can see its window, so the
/// work it still does runs on efficient cores at efficient clocks. Microsoft
/// asks not to throttle a window in the foreground. The audio thread keeps
/// its place through MMCSS (fastframe-audio).
/// <https://learn.microsoft.com/en-us/windows/win32/procthread/quality-of-service>
#[derive(Debug, Default)]
pub(super) struct EcoQos {
    written: Option<bool>,
    given_up: bool,
}

impl EcoQos {
    pub(super) fn set(&mut self, eco: bool) {
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, GetPriorityClass, GetProcessInformation, IDLE_PRIORITY_CLASS,
            PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
        };
        if self.given_up {
            return;
        }
        let size = std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32;
        let mut state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: 0,
            StateMask: 0,
        };
        // SAFETY: the pseudo-handle needs no closing, and `state` is a valid
        // PROCESS_POWER_THROTTLING_STATE of the size given.
        let (read, idle) = unsafe {
            let process = GetCurrentProcess();
            let read = GetProcessInformation(
                process,
                ProcessPowerThrottling,
                (&raw mut state).cast(),
                size,
            );
            (read != 0, GetPriorityClass(process) == IDLE_PRIORITY_CLASS)
        };
        // A read or a write that fails gives nothing up: the next change
        // tries again, so a process left under EcoQoS by a failed hand-back
        // does not stay there in the foreground. Only someone else managing
        // the setting makes the app stop touching it.
        if !read {
            log::debug!(
                "power throttling unreadable: {}",
                std::io::Error::last_os_error()
            );
            return;
        }
        let speed = PROCESS_POWER_THROTTLING_EXECUTION_SPEED;
        let current = (state.ControlMask & speed != 0).then_some(state.StateMask & speed != 0);
        match throttle(self.written, current, idle, eco) {
            Throttle::Leave => {}
            Throttle::GiveUp => {
                log::info!("power throttling is managed elsewhere; leaving it alone");
                self.given_up = true;
            }
            Throttle::Write(eco) => {
                let mask = if eco { speed } else { 0 };
                let state = PROCESS_POWER_THROTTLING_STATE {
                    Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                    ControlMask: mask,
                    StateMask: mask,
                };
                // SAFETY: as above.
                let written = unsafe {
                    SetProcessInformation(
                        GetCurrentProcess(),
                        ProcessPowerThrottling,
                        (&raw const state).cast(),
                        size,
                    )
                };
                if written != 0 {
                    log::info!(
                        "power throttling (EcoQoS): {}",
                        if eco { "on" } else { "off" }
                    );
                    self.written = Some(eco);
                } else {
                    log::debug!(
                        "power throttling not written: {}",
                        std::io::Error::last_os_error()
                    );
                }
            }
        }
    }
}

thread_local! {
    /// The app's side, for the session window's procedure on its thread.
    static SESSION: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    /// What the session changes have said so far.
    static SESSION_STATE: Cell<SessionState> = const {
        Cell::new(SessionState {
            locked: false,
            disconnected: false,
        })
    };
}

/// The thread and hidden window that receive the session messages.
struct SessionWindow {
    window: usize,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SessionWindow {
    fn spawn(shared: Arc<Shared>) -> Result<Self, String> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("spotifast-session".to_string())
            .spawn(move || {
                SESSION.with(|slot| *slot.borrow_mut() = Some(shared));
                let window = match create_session_window() {
                    Ok(window) => {
                        let _ = sender.send(Ok(window as usize));
                        window
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        return;
                    }
                };
                // SAFETY: the window belongs to this thread.
                unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) };
                // SAFETY: an all-zero MSG is a valid value for GetMessageW.
                let mut message: MSG = unsafe { std::mem::zeroed() };
                // SAFETY: `message` is a valid MSG; the loop ends on WM_QUIT
                // or an error.
                while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
                    // SAFETY: `message` came from GetMessageW just now.
                    unsafe { DispatchMessageW(&message) };
                }
            })
            .map_err(|error| error.to_string())?;
        let window = receiver.recv().map_err(|error| error.to_string())??;
        Ok(Self {
            window,
            thread: Some(thread),
        })
    }
}

impl Drop for SessionWindow {
    fn drop(&mut self) {
        // SAFETY: posting to a window that may already be gone is harmless.
        unsafe { PostMessageW(self.window as HWND, WM_CLOSE, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn create_session_window() -> Result<HWND, String> {
    let class: Vec<u16> = "SpotifastSession".encode_utf16().chain([0]).collect();
    // SAFETY: the class name outlives the calls; the window is top-level so
    // it gets session and end-of-session messages, and is never shown.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(session_procedure),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&window_class);
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if window.is_null() {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(window)
        }
    }
}

unsafe extern "system" fn session_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let shared = SESSION.with(|slot| slot.borrow().clone());
    match (message, shared) {
        (WM_WTSSESSION_CHANGE, Some(shared)) => {
            if let Some(state) = SESSION_STATE.get().apply(wparam as u32) {
                SESSION_STATE.set(state);
                shared.update(|c| c.locked = state.hidden());
            }
            0
        }
        // Never stand in the way of signing out or shutting down.
        (WM_QUERYENDSESSION, _) => 1,
        (WM_ENDSESSION, Some(shared)) => {
            if wparam != 0 {
                shared.push_and_wait(Event::EndingSession, END_SESSION_ACK);
            }
            0
        }
        (WM_CLOSE, _) => {
            // SAFETY: the window belongs to this thread.
            unsafe { DestroyWindow(window) };
            0
        }
        (WM_DESTROY, _) => {
            // SAFETY: as above; ends this thread's message loop.
            unsafe {
                WTSUnRegisterSessionNotification(window);
                PostQuitMessage(0);
            }
            0
        }
        // SAFETY: the default procedure for everything else.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_power_setting_reads_as_documented() {
        assert_eq!(
            setting(&GUID_ACDC_POWER_SOURCE, 1),
            Some(Setting::OnBattery(true))
        );
        assert_eq!(
            setting(&GUID_ACDC_POWER_SOURCE, 2),
            Some(Setting::OnBattery(true)),
            "a UPS is not mains power"
        );
        assert_eq!(
            setting(&GUID_ENERGY_SAVER_STATUS, 2),
            Some(Setting::EnergySaver(true)),
            "high savings"
        );
        assert_eq!(
            setting(&GUID_POWER_SAVING_STATUS, 0),
            Some(Setting::BatterySaver(false))
        );
        assert_eq!(
            setting(&GUID_CONSOLE_DISPLAY_STATE, 2),
            Some(Setting::DisplayOff(false)),
            "dimmed is still on"
        );
        assert_eq!(
            setting(&GUID_CONSOLE_DISPLAY_STATE, 0),
            Some(Setting::DisplayOff(true))
        );
        assert_eq!(setting(&GUID::from_u128(1), 1), None);
    }

    #[test]
    fn either_saver_saves() {
        let shared = Arc::new(Shared::new(|| {}));
        let context = Context {
            shared: Arc::clone(&shared),
            savers: std::sync::Mutex::new((false, false)),
        };
        let saver = || shared.conditions.lock().unwrap().saver;
        context.apply(Setting::BatterySaver(true));
        assert!(saver());
        context.apply(Setting::EnergySaver(false));
        assert!(saver(), "the battery saver is still on");
        context.apply(Setting::BatterySaver(false));
        assert!(!saver());
    }

    fn hint(level: i32, cost: i32) -> NL_NETWORK_CONNECTIVITY_HINT {
        NL_NETWORK_CONNECTIVITY_HINT {
            ConnectivityLevel: level,
            ConnectivityCost: cost,
            ApproachingDataLimit: false,
            OverDataLimit: false,
            Roaming: false,
        }
    }

    #[test]
    fn the_network_hint_reads_as_reach_and_cost() {
        use windows_sys::Win32::Networking::WinSock::{
            NetworkConnectivityCostHintUnknown, NetworkConnectivityCostHintUnrestricted,
            NetworkConnectivityLevelHintConstrainedInternetAccess,
            NetworkConnectivityLevelHintInternetAccess, NetworkConnectivityLevelHintUnknown,
        };
        let unrestricted = NetworkConnectivityCostHintUnrestricted;
        assert_eq!(
            network(&hint(
                NetworkConnectivityLevelHintInternetAccess,
                unrestricted
            )),
            (true, false)
        );
        assert_eq!(
            network(&hint(NetworkConnectivityLevelHintNone, unrestricted)),
            (false, false)
        );
        assert_eq!(
            network(&hint(NetworkConnectivityLevelHintLocalAccess, unrestricted)),
            (false, false),
            "a network without the internet"
        );
        assert_eq!(
            network(&hint(
                NetworkConnectivityLevelHintConstrainedInternetAccess,
                NetworkConnectivityCostHintFixed
            )),
            (true, true)
        );
        assert_eq!(
            network(&hint(
                NetworkConnectivityLevelHintUnknown,
                NetworkConnectivityCostHintUnknown
            )),
            (true, false),
            "unknown holds nothing back"
        );
        let mut roaming = hint(NetworkConnectivityLevelHintInternetAccess, unrestricted);
        roaming.Roaming = true;
        assert_eq!(network(&roaming), (true, true));
    }

    #[test]
    fn eco_qos_follows_the_window_unless_someone_else_set_it() {
        // Nothing written yet, the system decides: write what is wanted.
        assert_eq!(throttle(None, None, false, true), Throttle::Write(true));
        assert_eq!(throttle(None, None, false, false), Throttle::Leave);
        // As last written: write only a change.
        assert_eq!(
            throttle(Some(true), Some(true), false, false),
            Throttle::Write(false)
        );
        assert_eq!(
            throttle(Some(true), Some(true), false, true),
            Throttle::Leave
        );
        assert_eq!(
            throttle(Some(false), None, false, true),
            Throttle::Write(true)
        );
        // Changed behind the app's back, or efficiency mode: hands off.
        assert_eq!(throttle(None, Some(true), false, true), Throttle::GiveUp);
        assert_eq!(
            throttle(Some(false), Some(true), false, false),
            Throttle::GiveUp
        );
        assert_eq!(throttle(Some(true), None, false, true), Throttle::GiveUp);
        assert_eq!(throttle(None, None, true, true), Throttle::GiveUp);
    }

    /// Whether nobody can see the session after each change in turn.
    fn hidden_after(codes: &[u32]) -> Vec<bool> {
        let mut state = SessionState::default();
        codes
            .iter()
            .map(|&code| {
                state = state.apply(code).unwrap_or(state);
                state.hidden()
            })
            .collect()
    }

    /// The lock and the connection are separate: with fast user switching,
    /// a locked session switched back to the console shows the lock screen
    /// until it is unlocked, and an RDP session that disconnects is not
    /// locked but nobody sees it either.
    #[test]
    fn lock_and_unlock_are_read_from_session_changes() {
        assert_eq!(
            hidden_after(&[
                WTS_SESSION_LOCK,
                WTS_CONSOLE_DISCONNECT,
                WTS_CONSOLE_CONNECT,
                WTS_SESSION_UNLOCK,
            ]),
            [true, true, true, false],
        );
        assert_eq!(
            hidden_after(&[WTS_REMOTE_DISCONNECT, WTS_REMOTE_CONNECT]),
            [true, false],
        );
        assert_eq!(
            SessionState::default().apply(5),
            None,
            "a logon changes nothing here"
        );
    }

    /// Registers with the running system and reads the current state. Needs
    /// a Windows desktop session, so it only runs when asked for.
    #[test]
    #[ignore]
    fn the_running_system_answers() {
        let power = super::super::Power::spawn(|| {});
        std::thread::sleep(std::time::Duration::from_millis(500));
        let conditions = power.conditions();
        log::info!("{conditions:?}");
        assert!(!conditions.locked);
    }
}
