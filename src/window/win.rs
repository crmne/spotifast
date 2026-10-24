//! The Win32 half of the window's client area.
//!
//! winit keeps `WS_CAPTION` on an undecorated window, so that Windows draws
//! the drop shadow around it, and in `WM_NCCALCSIZE` it moves the client
//! area down by one physical pixel, so the caption is not painted inside the
//! window. The top row of the window is then outside the client area: the
//! app cannot paint it, Windows paints it instead, and because it is
//! outside the client area the app's repaint never reaches it again, so it
//! keeps the colour it had.
//!
//! This module puts the client area back where Windows proposed it, so the
//! top row belongs to the app like the rest of the window and takes the
//! album's tint. The window's own procedure is replaced for as long as the
//! window lives, and every message goes on to the procedure it replaced.
//!
//! Everything here runs on the thread that owns the window: `attach` from
//! the frame that creates it, and the window procedure from the same
//! message pump. Nothing in this module may be called from anywhere else.

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, GWLP_WNDPROC, GetPropW, NCCALCSIZE_PARAMS, RemovePropW,
    SetPropW, SetWindowLongPtrW, WM_NCCALCSIZE, WM_NCDESTROY, WNDPROC,
};

/// What the window procedure needs, and nothing more: the procedure it
/// replaced, and whether the window is still alive.
struct Hook {
    /// The window's own procedure, which every message is passed on to.
    /// Filled in the instant after this one is installed, which is before
    /// the thread pumps another message.
    previous: Cell<isize>,
    alive: Arc<AtomicBool>,
}

/// Where the hook hangs on the window.
fn property() -> *const u16 {
    windows_sys::w!("SpotifastClientArea")
}

/// The client area of an undecorated window, kept whole.
#[derive(Default)]
pub struct ClientArea {
    attached: Option<Attached>,
    // Window procedures stay on the shell's owning thread.
    _thread_bound: PhantomData<Rc<()>>,
}

struct Attached {
    hwnd: isize,
    alive: Arc<AtomicBool>,
}

impl ClientArea {
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts the app's own pixels on the top row of `hwnd`.
    ///
    /// # Safety
    /// `hwnd` must be a live window owned by this thread. Keep this on that
    /// thread and detach it before handing the window to another owner.
    pub unsafe fn attach(&mut self, hwnd: isize) {
        self.detach();
        if hwnd == 0 {
            return;
        }
        if !unsafe { GetPropW(hwnd as HWND, property()) }.is_null() {
            return;
        }
        let alive = Arc::new(AtomicBool::new(true));
        let hook = Box::new(Hook {
            previous: Cell::new(0),
            alive: Arc::clone(&alive),
        });
        // The window owns the hook from here, and gives it back when it is
        // destroyed. A window that will not hold a property keeps nothing.
        let hook = Box::into_raw(hook);
        let held = unsafe { SetPropW(hwnd as HWND, property(), hook.cast()) };
        if held == 0 {
            drop(unsafe { Box::from_raw(hook) });
            log::debug!("no client area: the window would not hold the hook");
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
            log::debug!("no client area: window procedure installation failed");
            return;
        }
        unsafe { (*hook).previous.set(previous) };
        self.attached = Some(Attached { hwnd, alive });
    }

    /// Lets the hook go with its window.
    pub fn detach(&mut self) {
        let Some(attached) = self.attached.take() else {
            return;
        };
        // Normally the window has already gone and given the hook back on
        // its way out. A window that is still alive (the hook being given up
        // before its window is) has to be asked for it, or nothing ever puts
        // its procedure back and nothing frees the hook.
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
    }
}

impl Drop for ClientArea {
    fn drop(&mut self) {
        self.detach();
    }
}

/// Whether the client area should be put back to the rectangle Windows
/// proposed.
///
/// winit moves the client area of an undecorated window down by one
/// physical pixel, so the caption is not painted inside it. That leaves the
/// top row of the window outside the client area, which the app cannot
/// paint: Windows paints it, and the app's repaint never reaches it again.
///
/// The one-pixel move is the only change winit makes to a window that is
/// neither decorated nor maximised, so a rectangle that differs from the
/// proposed one in exactly that way is winit's, and the top can go back.
///
/// A maximised window is left alone: there winit replaces the rectangle
/// with the monitor's work area, so the client area has to stay inside it,
/// or the app would draw over the taskbar. A decorated window is left alone
/// too: there the system works out the rectangle itself.
fn restores_client_top(proposed: RECT, after: RECT) -> bool {
    after.left == proposed.left
        && after.right == proposed.right
        && after.top == proposed.top + 1
        && after.bottom == proposed.bottom + 1
}

/// Sits in front of the window's own procedure, for the one message that
/// decides the client area. Everything else goes straight on to the
/// procedure this one replaced.
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
    if message == WM_NCCALCSIZE && wparam != 0 {
        let params = lparam as *mut NCCALCSIZE_PARAMS;
        // The rectangle Windows proposed, before the window's own procedure
        // has had its say about it.
        let proposed = unsafe { (*params).rgrc[0] };
        let result = unsafe { forward(hook.previous.get(), hwnd, message, wparam, lparam) };
        let after = unsafe { (*params).rgrc[0] };
        if restores_client_top(proposed, after) {
            // The client area goes back to the rectangle Windows proposed,
            // so the app paints the top row of the window like the rest of
            // it. The bottom goes back with it: the area keeps the size the
            // window's own procedure gave it and covers the window exactly.
            unsafe { (*params).rgrc[0] = proposed };
        }
        return result;
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

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    /// An undecorated window that is not maximised: winit moves the client
    /// area down by one pixel, and the top row goes back.
    #[test]
    fn an_undecorated_window_gets_its_top_row_back() {
        let proposed = rect(0, 0, 1240, 800);
        let after = rect(0, 1, 1240, 801);
        assert!(restores_client_top(proposed, after));
    }

    /// A maximised window: winit replaces the rectangle with the monitor's
    /// work area, so the client area stays where it was put. The app must
    /// not draw over the taskbar.
    #[test]
    fn a_maximised_window_keeps_its_work_area() {
        let proposed = rect(0, 0, 2048, 1152);
        let after = rect(0, 0, 2048, 1040);
        assert!(!restores_client_top(proposed, after));
        // The same window on a second monitor, whose work area is in the
        // screen's own coordinates.
        let proposed = rect(0, 0, 2048, 1152);
        let after = rect(1920, 0, 3968, 1040);
        assert!(!restores_client_top(proposed, after));
    }

    /// A decorated window: the system works out the rectangle itself, and it
    /// is nothing like winit's one-pixel move.
    #[test]
    fn a_decorated_window_keeps_its_frame() {
        let proposed = rect(8, 31, 1232, 792);
        let after = rect(8, 31, 1232, 792);
        assert!(!restores_client_top(proposed, after));
    }

    /// Only the one-pixel move is winit's. Anything else that moved the
    /// rectangle is left alone.
    #[test]
    fn only_the_one_pixel_move_is_windows_to_give_back() {
        let proposed = rect(0, 0, 1240, 800);
        // Two pixels down.
        assert!(!restores_client_top(proposed, rect(0, 2, 1240, 802)));
        // The top moved but the bottom did not.
        assert!(!restores_client_top(proposed, rect(0, 1, 1240, 800)));
        // The sides moved as well.
        assert!(!restores_client_top(proposed, rect(1, 1, 1241, 801)));
        // Nothing moved at all.
        assert!(!restores_client_top(proposed, proposed));
    }

    /// The hook is installed on the window and taken off it again, and the
    /// window's own procedure is back afterwards.
    #[test]
    fn the_hook_goes_on_the_window_and_comes_off_again() {
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
        let class: Vec<u16> = "SpotifastClientAreaTest"
            .encode_utf16()
            .chain([0])
            .collect();
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let descriptor = WNDCLASSW {
            lpfnWndProc: Some(original),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        assert_ne!(unsafe { RegisterClassW(&descriptor) }, 0);
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
        let previous = unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) };
        let mut area = ClientArea::new();
        unsafe { area.attach(hwnd as isize) };
        assert_ne!(unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) }, previous);
        // Every other message still reaches the window's own procedure.
        assert_eq!(unsafe { SendMessageW(hwnd, WM_APP, 0, 0) }, 42);
        area.detach();
        assert_eq!(unsafe { GetWindowLongPtrW(hwnd, GWLP_WNDPROC) }, previous);
        assert!(unsafe { GetPropW(hwnd, property()) }.is_null());
        unsafe { DestroyWindow(hwnd) };
        unsafe { UnregisterClassW(class.as_ptr(), instance) };
    }
}
