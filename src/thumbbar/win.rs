//! The Win32 half of the thumbnail toolbar.
//!
//! `windows-sys` carries the taskbar's class identifier, its button struct,
//! and every function used here, but no COM interface definitions at all, so
//! `ITaskbarList3` is declared below as the vtable it is. The order of those
//! entries is the interface: three from `IUnknown`, five from
//! `ITaskbarList`, one from `ITaskbarList2`, then its own. Only `HrInit`,
//! `ThumbBarAddButtons` and `ThumbBarUpdateButtons` are ever called; the
//! rest are named so the two that matter sit at the offsets Windows expects.
//!
//! A window-procedure hook reports taskbar creation and button presses to the
//! shell. COM calls run during shell frames, outside the window procedure.
//!
//! Everything here runs on the thread that owns the window: `attach` from
//! the frame that creates it, `sync` from the frame loop, and the window
//! procedure from the same message pump. That is the rule COM apartments
//! and window handles both want, and nothing in this module may be called
//! from anywhere else.

use std::cell::Cell;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, RPC_E_CHANGED_MODE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, COLOR_BTNFACE, CreateBitmap, CreateDIBSection,
    DIB_RGB_COLORS, DeleteObject, GetSysColor,
};
use windows_sys::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Shell::{
    THB_FLAGS, THB_ICON, THB_TOOLTIP, THBF_DISABLED, THBF_ENABLED, THBN_CLICKED, THUMBBUTTON,
    TaskbarList,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateIconIndirect, DefWindowProcW, DestroyIcon, GWLP_WNDPROC, GetPropW,
    HICON, ICONINFO, RegisterWindowMessageW, RemovePropW, SM_CXSMICON, SPI_GETHIGHCONTRAST,
    SetPropW, SetWindowLongPtrW, SystemParametersInfoW, WM_COMMAND, WM_NCDESTROY, WM_SETTINGCHANGE,
    WM_THEMECHANGED, WNDPROC,
};
use windows_sys::core::{GUID, HRESULT};

use super::{Button, COUNT, ThumbCommand, ThumbState, command_for};

/// `ITaskbarList3`, whose identifier the taskbar answers to.
const IID_ITASKBARLIST3: GUID = GUID::from_u128(0xea1afb91_9e28_4b86_90e9_9e9f8a5eefaf);

/// Layout, not data: every entry has to be present and in this order for
/// the three that are called to sit where Windows looks for them. The
/// unread ones are the interface's shape, so `dead_code` has nothing to say
/// about them.
#[repr(C)]
#[allow(dead_code)]
struct ITaskbarList3Vtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    hr_init: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    add_tab: unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT,
    delete_tab: unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT,
    activate_tab: unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT,
    set_active_alt: unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT,
    mark_fullscreen_window: unsafe extern "system" fn(*mut c_void, HWND, i32) -> HRESULT,
    set_progress_value: unsafe extern "system" fn(*mut c_void, HWND, u64, u64) -> HRESULT,
    set_progress_state: unsafe extern "system" fn(*mut c_void, HWND, i32) -> HRESULT,
    register_tab: unsafe extern "system" fn(*mut c_void, HWND, HWND) -> HRESULT,
    unregister_tab: unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT,
    set_tab_order: unsafe extern "system" fn(*mut c_void, HWND, HWND) -> HRESULT,
    set_tab_active: unsafe extern "system" fn(*mut c_void, HWND, HWND, u32) -> HRESULT,
    thumb_bar_add_buttons:
        unsafe extern "system" fn(*mut c_void, HWND, u32, *const THUMBBUTTON) -> HRESULT,
    thumb_bar_update_buttons:
        unsafe extern "system" fn(*mut c_void, HWND, u32, *const THUMBBUTTON) -> HRESULT,
    thumb_bar_set_image_list: unsafe extern "system" fn(*mut c_void, HWND, *mut c_void) -> HRESULT,
    set_overlay_icon: unsafe extern "system" fn(*mut c_void, HWND, HICON, *const u16) -> HRESULT,
    set_thumbnail_tooltip: unsafe extern "system" fn(*mut c_void, HWND, *const u16) -> HRESULT,
    set_thumbnail_clip: unsafe extern "system" fn(*mut c_void, HWND, *const RECT) -> HRESULT,
}

#[repr(C)]
struct ITaskbarList3 {
    vtable: *const ITaskbarList3Vtbl,
}

/// What the window procedure needs, and nothing more. It reports presses
/// and the arrival of the taskbar button; every COM call happens in `sync`,
/// on the same thread but out of the message pump.
struct Hook {
    presses: Sender<ThumbCommand>,
    wake: Box<dyn Fn() + Send + Sync>,
    /// The message Explorer sends once this window has a taskbar button,
    /// which is the first moment the toolbar can be added to it.
    taskbar_created: u32,
    /// Counted rather than set, because Explorer sends that message again
    /// every time it restarts. The taskbar button that comes back is a new
    /// one with no toolbar on it, so the count is what tells `sync` the
    /// difference between the first arrival and a replacement.
    created: Arc<AtomicU32>,
    alive: Arc<AtomicBool>,
    appearance_changed: Arc<AtomicBool>,
    /// The window's own procedure, which every message is passed on to.
    /// Filled in the instant after this one is installed, which is before
    /// the thread pumps another message.
    previous: Cell<isize>,
}

/// Where the hook hangs on the window.
fn property() -> *const u16 {
    windows_sys::w!("SpotizgegThumbBar")
}

/// The toolbar under one window's taskbar preview.
#[derive(Default)]
pub struct ThumbBar {
    attached: Option<Attached>,
    presses: Option<Receiver<ThumbCommand>>,
    // Window procedures and COM objects stay on the shell's owning thread.
    _thread_bound: PhantomData<Rc<()>>,
}

struct Attached {
    hwnd: isize,
    taskbar: isize,
    apartment: Option<ComApartment>,
    created: Arc<AtomicU32>,
    alive: Arc<AtomicBool>,
    appearance_changed: Arc<AtomicBool>,
    /// The taskbar button this toolbar was built for. Anything higher means
    /// Explorer has been restarted since, and everything below has to be
    /// done again on the button that replaced it.
    seen: u32,
    /// Whether the buttons have been handed over. Windows takes them once
    /// per taskbar button and refuses a second set, so this only goes back
    /// when that button has been replaced.
    installed: bool,
    /// What the buttons were last given, so an unchanged frame is free.
    shown: Option<ThumbState>,
    /// The glyphs, made once each and kept for as long as the toolbar can
    /// draw them. There are four, a button wears three at a time, and a
    /// change usually moves one of them, so making them per change spent
    /// most of its time handing Windows handles it had just been given
    /// back. Keeping them also settles the question of when the taskbar has
    /// finished reading one: not until the toolbar itself is gone.
    icons: Vec<(super::Icon, isize)>,
    /// The theme those were drawn for. It is the desktop's, not the app's,
    /// so it can change under a running window and turn every one of them
    /// the wrong colour.
    icons_dark: bool,
    system_dark: Option<bool>,
    icon_size: u32,
}

impl ThumbBar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts a toolbar on `hwnd`, replacing any earlier one.
    ///
    /// The buttons cannot go up yet: Windows only accepts them once the
    /// window has a taskbar button, which it says with a message the
    /// subclass installed here waits for.
    /// # Safety
    /// `hwnd` must be a live window owned by this thread. Keep this toolbar
    /// on that thread and detach it before handing the window to another owner.
    pub unsafe fn attach(&mut self, hwnd: isize, wake: impl Fn() + Send + Sync + 'static) {
        self.detach();
        if hwnd == 0 {
            return;
        }
        if !unsafe { GetPropW(hwnd as HWND, property()) }.is_null() {
            return;
        }
        let (presses, receiver) = std::sync::mpsc::channel();
        let created = Arc::new(AtomicU32::new(0));
        let alive = Arc::new(AtomicBool::new(true));
        let appearance_changed = Arc::new(AtomicBool::new(true));
        let name = wide("TaskbarButtonCreated");
        let taskbar_created = unsafe { RegisterWindowMessageW(name.as_ptr()) };
        if taskbar_created == 0 {
            return;
        }
        let hook = Box::new(Hook {
            presses,
            wake: Box::new(wake),
            taskbar_created,
            created: Arc::clone(&created),
            alive: Arc::clone(&alive),
            appearance_changed: Arc::clone(&appearance_changed),
            previous: Cell::new(0),
        });
        // The window owns the hook from here, and gives it back when it is
        // destroyed. A window that will not hold a property keeps nothing.
        let hook = Box::into_raw(hook);
        let held = unsafe { SetPropW(hwnd as HWND, property(), hook.cast()) };
        if held == 0 {
            drop(unsafe { Box::from_raw(hook) });
            log::debug!("no taskbar toolbar: the window would not hold the hook");
            return;
        }
        // From here the procedure below is the window's, and passes what it
        // does not want to the one it replaced.
        let ours: WNDPROC = Some(procedure);
        let previous = unsafe {
            SetWindowLongPtrW(
                hwnd as HWND,
                GWLP_WNDPROC,
                std::mem::transmute::<WNDPROC, isize>(ours),
            )
        };
        if previous == 0 {
            unsafe {
                RemovePropW(hwnd as HWND, property());
                drop(Box::from_raw(hook));
            }
            log::debug!("no taskbar toolbar: window procedure installation failed");
            return;
        }
        unsafe { (*hook).previous.set(previous) };
        // The system's small-icon size, which follows the desktop's scaling.
        let icon_size = icon_size(hwnd as HWND);
        self.presses = Some(receiver);
        self.attached = Some(Attached {
            hwnd,
            taskbar: 0,
            apartment: None,
            created,
            alive,
            appearance_changed,
            seen: 0,
            installed: false,
            shown: None,
            icons: Vec::new(),
            icons_dark: false,
            system_dark: None,
            icon_size,
        });
    }

    /// Lets the toolbar go with its window.
    pub fn detach(&mut self) {
        self.presses = None;
        let Some(mut attached) = self.attached.take() else {
            return;
        };
        // Normally the window has already gone and given the hook back on
        // its way out. A window that is still alive (the toolbar being given
        // up before its window is) has to be asked for it, or nothing ever
        // puts its procedure back and nothing frees the hook.
        // WM_NCDESTROY may already have freed the hook, and Windows may
        // have reused the numeric handle for an unrelated window.
        let hook = if attached.alive.swap(false, Ordering::Relaxed) {
            unsafe { RemovePropW(attached.hwnd as HWND, property()) }.cast::<Hook>()
        } else {
            std::ptr::null_mut()
        };
        if !hook.is_null() {
            unsafe {
                let previous = (*hook).previous.get();
                if previous != 0 {
                    SetWindowLongPtrW(attached.hwnd as HWND, GWLP_WNDPROC, previous);
                }
                drop(Box::from_raw(hook));
            }
        }
        attached.forget_icons();
        attached.release();
    }

    /// Brings the buttons in line with what is playing.
    pub fn sync(&mut self, state: ThumbState) {
        if let Some(attached) = &mut self.attached {
            attached.sync(state);
        }
    }

    pub fn drain_commands(&self) -> Vec<ThumbCommand> {
        self.presses
            .as_ref()
            .map(|presses| presses.try_iter().collect())
            .unwrap_or_default()
    }
}

impl Drop for ThumbBar {
    fn drop(&mut self) {
        self.detach();
    }
}

impl Attached {
    fn sync(&mut self, mut state: ThumbState) {
        if self.appearance_changed.swap(false, Ordering::Relaxed) {
            self.system_dark = system_dark();
        }
        state.dark = self.system_dark.unwrap_or(state.dark);
        let created = self.created.load(Ordering::Relaxed);
        if created == 0 {
            // No taskbar button yet, so nothing to hang a toolbar under.
            return;
        }
        if created != self.seen {
            self.taskbar_replaced(created);
        }
        let size = icon_size(self.hwnd as HWND);
        if size != self.icon_size {
            self.forget_icons();
            self.icon_size = size;
            self.shown = None;
        }
        if self.shown == Some(state) {
            return;
        }
        if self.taskbar == 0 && !self.connect() {
            // Nothing to be done about it, and saying so every frame would
            // fill the log. Remembering the state stops the retry.
            self.shown = Some(state);
            return;
        }
        let buttons = super::buttons(&state);
        let mut descriptors = Vec::with_capacity(COUNT);
        for button in &buttons {
            let icon = self.icon_handle(button.icon, state.dark);
            descriptors.push(descriptor(button, icon as HICON));
        }

        let taskbar = self.taskbar as *mut ITaskbarList3;
        let vtable = unsafe { &*(*taskbar).vtable };
        let count = descriptors.len() as u32;
        let result = unsafe {
            if self.installed {
                (vtable.thumb_bar_update_buttons)(
                    taskbar.cast(),
                    self.hwnd as HWND,
                    count,
                    descriptors.as_ptr(),
                )
            } else {
                (vtable.thumb_bar_add_buttons)(
                    taskbar.cast(),
                    self.hwnd as HWND,
                    count,
                    descriptors.as_ptr(),
                )
            }
        };
        if result < 0 {
            // A refused add has not spent the one chance to add, but a
            // refusal repeated every frame is a log full of the same line,
            // so this waits for the state to change again. The glyphs stay:
            // the next attempt wants the same ones.
            log::debug!("the taskbar refused the toolbar buttons: {result:#x}");
            self.shown = Some(state);
            return;
        }
        if !self.installed {
            log::debug!(
                "the taskbar toolbar is up: {count} buttons at {}px",
                self.icon_size
            );
        }
        self.installed = true;
        self.shown = Some(state);
    }

    /// Asks Explorer for the taskbar, once.
    fn connect(&mut self) -> bool {
        if self.apartment.is_none() {
            self.apartment = ComApartment::new();
            if self.apartment.is_none() {
                return false;
            }
        }
        let mut taskbar: *mut c_void = std::ptr::null_mut();
        let created = unsafe {
            CoCreateInstance(
                &TaskbarList,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ITASKBARLIST3,
                &mut taskbar,
            )
        };
        if created < 0 || taskbar.is_null() {
            log::debug!("no taskbar toolbar: the taskbar refused to appear ({created:#x})");
            return false;
        }
        let object = taskbar.cast::<ITaskbarList3>();
        let vtable = unsafe { &*(*object).vtable };
        let started = unsafe { (vtable.hr_init)(taskbar) };
        if started < 0 {
            unsafe { (vtable.release)(taskbar) };
            log::debug!("no taskbar toolbar: it would not start ({started:#x})");
            return false;
        }
        self.taskbar = object as isize;
        true
    }

    /// Starts over on a taskbar button that has never had a toolbar.
    ///
    /// Explorer sends its message again whenever it restarts. The object in
    /// hand then talks to a taskbar that is gone, and the button that
    /// replaced ours has no buttons on it, so the next call has to be an
    /// add rather than an update. The glyphs are this process's own and
    /// survive it, which is the whole of what is kept.
    fn taskbar_replaced(&mut self, created: u32) {
        self.seen = created;
        self.release();
        self.installed = false;
        self.shown = None;
    }

    /// The handle for `icon`, drawn the first time it is asked for and kept
    /// after that. A theme change throws the whole set away, because every
    /// glyph in it is now the wrong colour.
    ///
    /// A glyph that cannot be made is remembered as nothing, which is what
    /// a button with no icon is given. Retrying it on every change would
    /// not make it work and would cost the same again each time.
    fn icon_handle(&mut self, icon: super::Icon, dark: bool) -> isize {
        if dark != self.icons_dark {
            self.forget_icons();
            self.icons_dark = dark;
        }
        if let Some((_, handle)) = self.icons.iter().find(|(known, _)| *known == icon) {
            return *handle;
        }
        let handle = super::draw(icon, self.icon_size, dark)
            .and_then(|pixmap| make_icon(&pixmap))
            .map_or(0, |handle| handle as isize);
        self.icons.push((icon, handle));
        handle
    }

    fn forget_icons(&mut self) {
        for (_, icon) in self.icons.drain(..) {
            if icon != 0 {
                unsafe { DestroyIcon(icon as HICON) };
            }
        }
    }

    fn release(&mut self) {
        if self.taskbar == 0 {
            return;
        }
        let taskbar = self.taskbar as *mut ITaskbarList3;
        unsafe {
            let vtable = &*(*taskbar).vtable;
            (vtable.release)(taskbar.cast());
        }
        self.taskbar = 0;
    }
}

/// One initialization is paired even when COM was already initialized
/// (S_FALSE). A different existing apartment stays owned by its caller.
struct ComApartment {
    uninitialize: bool,
}

impl ComApartment {
    fn new() -> Option<Self> {
        let result = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        (result >= 0 || result == RPC_E_CHANGED_MODE).then_some(Self {
            uninitialize: result >= 0,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninitialize {
            unsafe { CoUninitialize() };
        }
    }
}

/// Windows allows its shell and applications to use different color modes.
/// Read the shell preference at attachment and on setting-change messages.
fn system_dark() -> Option<bool> {
    let mut contrast: HIGHCONTRASTW = unsafe { std::mem::zeroed() };
    contrast.cbSize = size_of::<HIGHCONTRASTW>() as u32;
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.cbSize,
            (&mut contrast as *mut HIGHCONTRASTW).cast(),
            0,
        )
    };
    if read != 0 && contrast.dwFlags & HCF_HIGHCONTRASTON != 0 {
        let background = unsafe { GetSysColor(COLOR_BTNFACE) };
        let brightness = (background & 255) * 299
            + ((background >> 8) & 255) * 587
            + ((background >> 16) & 255) * 114;
        return Some(brightness < 128_000);
    }
    let mut light: u32 = 0;
    let mut bytes = size_of::<u32>() as u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows_sys::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            windows_sys::w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut light as *mut u32).cast(),
            &mut bytes,
        )
    };
    (read == 0 && bytes == 4 && light <= 1).then_some(light == 0)
}

fn icon_size(hwnd: HWND) -> u32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) }.clamp(16, 64) as u32
}

/// One button as Windows wants it described.
fn descriptor(button: &Button, icon: HICON) -> THUMBBUTTON {
    let mut thumb: THUMBBUTTON = unsafe { std::mem::zeroed() };
    thumb.dwMask = THB_ICON | THB_TOOLTIP | THB_FLAGS;
    thumb.iId = button.id;
    thumb.hIcon = icon;
    // The zeroed tail is the terminator, so the last slot stays untouched.
    let tip = thumb.szTip.len() - 1;
    for (slot, unit) in thumb.szTip[..tip].iter_mut().zip(button.tip.encode_utf16()) {
        *slot = unit;
    }
    thumb.dwFlags = if button.enabled {
        THBF_ENABLED
    } else {
        THBF_DISABLED
    };
    thumb
}

/// Turns a drawn glyph into an icon Windows can hang on a button.
fn make_icon(pixmap: &tiny_skia::Pixmap) -> Option<HICON> {
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
    info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width;
    // Negative for rows top down, which is the order the glyph was drawn in.
    info.bmiHeader.biHeight = -height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    let mut bits: *mut c_void = std::ptr::null_mut();
    let colour = unsafe {
        CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    if colour.is_null() || bits.is_null() {
        if !colour.is_null() {
            unsafe { DeleteObject(colour) };
        }
        return None;
    }
    // tiny-skia writes premultiplied RGBA and an icon's colour bitmap is
    // read as premultiplied BGRA, so only the two ends trade places.
    let source = pixmap.data();
    let destination = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), source.len()) };
    for (out, pixel) in destination
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(source.as_chunks::<4>().0)
    {
        *out = [pixel[2], pixel[1], pixel[0], pixel[3]];
    }
    // A 32-bit colour bitmap carries its own transparency, but an icon is
    // still made of two bitmaps, so this hands over an empty mask and lets
    // the alpha decide.
    let mask_bits = vec![0_u8; (width as usize).div_ceil(16) * 2 * height as usize];
    let mask = unsafe { CreateBitmap(width, height, 1, 1, mask_bits.as_ptr().cast()) };
    if mask.is_null() {
        unsafe { DeleteObject(colour) };
        return None;
    }
    let parts = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: colour,
    };
    let icon = unsafe { CreateIconIndirect(&parts) };
    // The icon holds copies of both.
    unsafe {
        DeleteObject(mask);
        DeleteObject(colour);
    }
    (!icon.is_null()).then_some(icon)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Sits in front of the window's own procedure, for the two messages the
/// toolbar needs: the taskbar button appearing, and a button being pressed.
/// Everything else goes straight on to the procedure this one replaced.
unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let hook = unsafe { GetPropW(hwnd, property()) }.cast::<Hook>();
    if hook.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    if message == WM_NCDESTROY {
        // Last message the window gets: put its procedure back, take the
        // hook off it, and let the original see the message too.
        unsafe {
            RemovePropW(hwnd, property());
            let hook = Box::from_raw(hook);
            hook.alive.store(false, Ordering::Relaxed);
            let previous = hook.previous.get();
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, previous);
            return forward(previous, hwnd, message, wparam, lparam);
        }
    }
    let hook = unsafe { &*hook };
    if message == hook.taskbar_created {
        hook.created.fetch_add(1, Ordering::Relaxed);
        (hook.wake)();
        return unsafe { forward(hook.previous.get(), hwnd, message, wparam, lparam) };
    }
    if message == WM_SETTINGCHANGE || message == WM_THEMECHANGED {
        hook.appearance_changed.store(true, Ordering::Relaxed);
        (hook.wake)();
    }
    // A press arrives as a command from the toolbar, the button's own
    // identifier in the low half and the reason in the high one.
    if message == WM_COMMAND
        && ((wparam >> 16) & 0xffff) as u32 == THBN_CLICKED
        && let Some(command) = command_for((wparam & 0xffff) as u32)
        && hook.presses.send(command).is_ok()
    {
        // The window may be behind a preview and drawing nothing, so
        // the press has to ask for the frame that acts on it.
        (hook.wake)();
        return 0;
    }
    unsafe { forward(hook.previous.get(), hwnd, message, wparam, lparam) }
}

/// Hands a message to the procedure this one replaced.
unsafe fn forward(
    previous: isize,
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if previous == 0 {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    // A window procedure is what was read out of the window, and a null one
    // is the `None` this transmutes to.
    let previous: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(previous) };
    unsafe { CallWindowProcW(previous, hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_hooks_forward_messages_and_discard_commands_when_recreated() {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, GetWindowLongPtrW, RegisterClassW, SendMessageW,
            UnregisterClassW, WM_APP, WNDCLASSW,
        };
        unsafe extern "system" fn original(
            hwnd: HWND,
            message: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            if message == WM_APP {
                42
            } else {
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
        let class = wide("SpotizgegThumbbarTest");
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let descriptor = WNDCLASSW {
            lpfnWndProc: Some(original),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        assert_ne!(unsafe { RegisterClassW(&descriptor) }, 0);
        let create = || {
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    class.as_ptr(),
                    0,
                    0,
                    0,
                    100,
                    100,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null());
            hwnd
        };
        let hwnd = create();
        let previous = unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) };
        let mut toolbar = ThumbBar::new();
        unsafe { toolbar.attach(hwnd as isize, || {}) };
        assert_ne!(unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) }, previous);
        assert_eq!(unsafe { SendMessageW(hwnd, WM_APP, 0, 0) }, 42);
        let created = unsafe { RegisterWindowMessageW(windows_sys::w!("TaskbarButtonCreated")) };
        unsafe { SendMessageW(hwnd, created, 0, 0) };
        assert_eq!(
            toolbar
                .attached
                .as_ref()
                .unwrap()
                .created
                .load(Ordering::Relaxed),
            1
        );
        unsafe {
            SendMessageW(
                hwnd,
                WM_COMMAND,
                ((THBN_CLICKED as usize) << 16) | super::super::ID_NEXT as usize,
                0,
            )
        };
        assert_eq!(toolbar.drain_commands(), [ThumbCommand::Next]);
        toolbar.detach();
        assert_eq!(unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) }, previous);
        assert!(unsafe { GetPropW(hwnd, property()) }.is_null());
        unsafe { toolbar.attach(hwnd as isize, || {}) };
        unsafe {
            SendMessageW(
                hwnd,
                WM_COMMAND,
                ((THBN_CLICKED as usize) << 16) | super::super::ID_PLAY as usize,
                0,
            )
        };
        unsafe { DestroyWindow(hwnd) };
        assert!(
            !toolbar
                .attached
                .as_ref()
                .unwrap()
                .alive
                .load(Ordering::Relaxed)
        );
        let second = create();
        unsafe { toolbar.attach(second as isize, || {}) };
        assert!(
            toolbar.drain_commands().is_empty(),
            "commands from the destroyed window cannot control the replacement"
        );
        assert_eq!(
            toolbar
                .attached
                .as_ref()
                .unwrap()
                .created
                .load(Ordering::Relaxed),
            0
        );
        unsafe { DestroyWindow(second) };
        toolbar.detach();
        unsafe { UnregisterClassW(class.as_ptr(), instance) };
    }

    /// The vtable above is written out by hand, and a wrong order means
    /// calling a different method than the one named, with nothing to say
    /// so. This asks Explorer for the real object and calls `HrInit`
    /// through the declaration: a shifted entry lands on `Release` or
    /// `AddTab` instead, and cannot answer `S_OK`. `AddRef` and `Release`
    /// sit either side of the entries under test, so a shift at the top
    /// shows up as a refcount that does not come back.
    ///
    /// A session with no taskbar has nothing to check and says so rather
    /// than failing.
    #[test]
    fn the_vtable_offsets_match_the_object_explorer_hands_over() {
        let _apartment = ComApartment::new().expect("a COM apartment");
        let mut taskbar: *mut c_void = std::ptr::null_mut();
        let created = unsafe {
            CoCreateInstance(
                &TaskbarList,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ITASKBARLIST3,
                &mut taskbar,
            )
        };
        if created < 0 || taskbar.is_null() {
            println!("no taskbar in this session ({created:#x}); nothing to check");
            return;
        }
        let object = taskbar.cast::<ITaskbarList3>();
        let vtable = unsafe { &*(*object).vtable };
        let started = unsafe { (vtable.hr_init)(taskbar) };
        let raised = unsafe { (vtable.add_ref)(taskbar) };
        let lowered = unsafe { (vtable.release)(taskbar) };
        unsafe { (vtable.release)(taskbar) };
        assert!(
            started >= 0,
            "HrInit through the vtable failed: {started:#x}"
        );
        assert_eq!(raised, lowered + 1, "AddRef and Release are not a pair");
    }

    /// Every icon made is destroyed. A leak here ends with the desktop
    /// refusing to draw: GDI stops a process at 10,000 objects.
    #[test]
    fn making_and_destroying_icons_does_not_leak_gdi_objects() {
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // Not in the feature set this crate turns on, and wanted only here.
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetGuiResources(process: *mut c_void, flags: u32) -> u32;
        }
        // 0 is GR_GDIOBJECTS.
        let count = || unsafe { GetGuiResources(GetCurrentProcess(), 0) };
        let every = [
            super::super::Icon::Previous,
            super::super::Icon::Play,
            super::super::Icon::Pause,
            super::super::Icon::Next,
        ];
        let round = || {
            for icon in every {
                let pixmap = super::super::draw(icon, 16, true).expect("a glyph");
                let handle = make_icon(&pixmap).expect("an icon");
                unsafe { DestroyIcon(handle) };
            }
        };
        // Whatever the first few allocate and hold on to is not a leak.
        for _ in 0..50 {
            round();
        }
        let before = count();
        for _ in 0..1_000 {
            round();
        }
        let after = count();
        assert!(
            after <= before + 16,
            "leaked GDI objects over 4000 icons: {before} -> {after}"
        );
    }

    /// The glyphs are made once. A change that moves one button must not
    /// hand Windows four new handles, and a theme change must replace all
    /// of them rather than leaving the old colour on the toolbar.
    #[test]
    fn the_glyphs_are_drawn_once_and_kept() {
        let mut attached = Attached {
            hwnd: 0,
            taskbar: 0,
            apartment: None,
            created: Arc::new(AtomicU32::new(0)),
            alive: Arc::new(AtomicBool::new(true)),
            appearance_changed: Arc::new(AtomicBool::new(false)),
            seen: 0,
            installed: false,
            shown: None,
            icons: Vec::new(),
            icons_dark: true,
            system_dark: None,
            icon_size: 16,
        };
        let first = attached.icon_handle(super::super::Icon::Play, true);
        assert_ne!(first, 0, "no handle for a glyph that draws");
        assert_eq!(
            attached.icon_handle(super::super::Icon::Play, true),
            first,
            "asked twice, made twice"
        );
        assert_eq!(attached.icons.len(), 1);

        // A second glyph joins it rather than replacing it.
        attached.icon_handle(super::super::Icon::Pause, true);
        assert_eq!(attached.icons.len(), 2);

        // The desktop goes light: every glyph is now the wrong colour.
        let relit = attached.icon_handle(super::super::Icon::Play, false);
        assert_eq!(attached.icons.len(), 1, "the old set was kept");
        // Windows may recycle a released handle, so identity cannot prove
        // whether the replacement has the new pixels.
        assert_ne!(relit, 0);
        assert!(!attached.icons_dark);
        attached.forget_icons();
        assert!(attached.icons.is_empty());
    }

    /// Explorer restarting takes the taskbar button with it and makes a new
    /// one, which has never been given a toolbar. Windows accepts buttons
    /// once per button, so the reply to a second arrival has to be another
    /// add; carrying on with updates leaves the preview bare until the
    /// window is recreated.
    #[test]
    fn a_restarted_explorer_gets_the_buttons_again() {
        let state = ThumbState {
            has_track: true,
            playing: true,
            can_control: true,
            dark: true,
        };
        // #given a toolbar standing on the first taskbar button
        let mut attached = Attached {
            hwnd: 0,
            taskbar: 0,
            apartment: None,
            created: Arc::new(AtomicU32::new(1)),
            alive: Arc::new(AtomicBool::new(true)),
            appearance_changed: Arc::new(AtomicBool::new(false)),
            seen: 1,
            installed: true,
            shown: Some(state),
            icons: vec![(super::super::Icon::Play, 0)],
            icons_dark: true,
            system_dark: None,
            icon_size: 16,
        };

        // #when Explorer restarts and says so a second time
        attached.taskbar_replaced(2);

        // #then the next frame adds the buttons rather than updating a
        // button that has never had any
        assert_eq!(attached.seen, 2);
        assert!(
            !attached.installed,
            "an update would go to a taskbar button with no toolbar on it"
        );
        assert_eq!(
            attached.shown, None,
            "an unchanged state would skip the frame that rebuilds"
        );
        // The glyphs belong to this process, not to Explorer.
        assert_eq!(attached.icons.len(), 1, "the glyphs were thrown away");
    }
}
