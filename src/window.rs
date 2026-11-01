//! Checks saved window positions before restoring them.
//!
//! eframe already places the window on-screen. The session can still refer
//! to a monitor that was unplugged, so check before moving the window there.

/// Why a Wayland window cannot be kept above others from the app, and what
/// to do instead.
pub fn on_top_unavailable(locale: crate::i18n::Locale) -> std::borrow::Cow<'static, str> {
    use crate::i18n::gettext;
    gettext(
        locale,
        "On Wayland, use your desktop's Keep Above shortcut or window rule.",
    )
}

/// The download guide, which documents the OpenGL 2.0 requirement and is
/// the help page the no-window dialog offers.
pub const DOWNLOAD_PAGE: &str = "https://spotifast.rocks/download/";

/// The dialog fastframe-graphics shows when no window could open, in the
/// interface language: Windows on ARM is pointed at Microsoft's
/// compatibility pack, where its OpenGL comes from; anything else at the
/// graphics driver and the download guide.
pub fn no_window_dialog(
    locale: crate::i18n::Locale,
    trouble: &fastframe_graphics::Trouble<'_>,
) -> fastframe_graphics::Dialog {
    use crate::i18n::gettext;
    let english = fastframe_graphics::Dialog::english(trouble);
    fastframe_graphics::Dialog {
        title: gettext(locale, "Spotifast cannot open its window").into_owned(),
        body: no_window_body(locale, trouble.cause, trouble.help_url)
            .unwrap_or_else(|| english.body.clone()),
        open_label: gettext(locale, "Open page").into_owned(),
        close_label: gettext(locale, "Close").into_owned(),
        ..english
    }
}

/// The dialog's explanation for `cause`, or `None` for a cause fastframe
/// added later, which keeps fastframe's English.
fn no_window_body(
    locale: crate::i18n::Locale,
    cause: fastframe_graphics::Cause,
    help_url: Option<&str>,
) -> Option<String> {
    use crate::i18n::gettext;
    Some(match cause {
        fastframe_graphics::Cause::WindowsOnArm => gettext(
            locale,
            "On Windows on ARM, the OpenGL that Spotifast draws with comes from Microsoft's OpenCL, OpenGL & Vulkan Compatibility Pack. Open its Microsoft Store page to install or repair it?",
        )
        .into_owned(),
        fastframe_graphics::Cause::NoGraphicsDriver => gettext(
            locale,
            // Translators: Keep {url} exactly as written. It becomes the address
            // of the Spotifast download page.
            "Spotifast needs a graphics driver with OpenGL 2.0 or newer, and this PC or virtual machine does not provide one. Install the graphics driver, or turn on GPU acceleration for the virtual machine. Open {url} to read more?",
        )
        .replace("{url}", help_url.unwrap_or(DOWNLOAD_PAGE)),
        _ => return None,
    })
}

/// Minimize the active window without leaving egui's macOS viewport flag stale
/// when the user later restores it from the Dock.
pub fn minimize_window(ctx: &egui::Context) {
    #[cfg(target_os = "macos")]
    {
        use objc2::{MainThreadMarker, sel};
        use objc2_app_kit::NSApplication;
        use objc2_foundation::NSObjectNSDelayedPerforming;

        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let window = app.currentEvent().and_then(|event| event.window(mtm));
        if let Some(window) = window.or_else(|| app.keyWindow()) {
            // Let egui finish this frame before AppKit minimizes the window.
            // SAFETY: NSWindow's selector takes one optional sender argument.
            unsafe {
                window.performSelector_withObject_afterDelay(sel!(miniaturize:), None, 0.0);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
    #[cfg(target_os = "macos")]
    let _ = ctx;
}

/// The active backend matters: a Wayland session can also host X11 windows.
/// winit's Wayland backend cannot change a window's stacking level.
pub fn supports_window_level(display: raw_window_handle::RawDisplayHandle) -> bool {
    !matches!(display, raw_window_handle::RawDisplayHandle::Wayland(_))
}

/// Whether the mini player can keep its window without a taskbar entry.
/// Windows and X11 can; Wayland has no protocol for it, and macOS has the
/// Dock instead of a taskbar.
pub fn supports_hiding_from_taskbar(display: raw_window_handle::RawDisplayHandle) -> bool {
    use raw_window_handle::RawDisplayHandle;
    matches!(
        display,
        RawDisplayHandle::Windows(_) | RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_)
    )
}

/// Keeps an X11 window out of the taskbar with `_NET_WM_STATE_SKIP_TASKBAR`.
///
/// winit can only do this on Windows, so this asks the window manager
/// directly. A window that is not mapped yet carries the state in its
/// property, which the window manager reads when it maps the window; a
/// mapped one asks the window manager, as EWMH requires. Any other handle is
/// left alone.
#[cfg(target_os = "linux")]
pub fn skip_x11_taskbar(window: raw_window_handle::RawWindowHandle) {
    use raw_window_handle::RawWindowHandle;
    let window = match window {
        RawWindowHandle::Xlib(handle) => match u32::try_from(handle.window) {
            Ok(window) => window,
            Err(_) => return,
        },
        RawWindowHandle::Xcb(handle) => handle.window.get(),
        _ => return,
    };
    if let Err(error) = x11::skip_taskbar(window) {
        log::warn!("could not hide the mini player from the taskbar: {error}");
    }
}

#[cfg(target_os = "linux")]
mod x11 {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, MapState, PropMode,
    };
    use x11rb::wrapper::ConnectionExt as _;

    /// `_NET_WM_STATE_ADD` in a `_NET_WM_STATE` client message.
    const ADD: u32 = 1;
    /// The request comes from an ordinary application.
    const FROM_APPLICATION: u32 = 1;

    pub(super) fn skip_taskbar(window: u32) -> Result<(), Box<dyn std::error::Error>> {
        let (connection, screen) = x11rb::connect(None)?;
        let root = connection.setup().roots[screen].root;
        let state = connection
            .intern_atom(false, b"_NET_WM_STATE")?
            .reply()?
            .atom;
        let skip = connection
            .intern_atom(false, b"_NET_WM_STATE_SKIP_TASKBAR")?
            .reply()?
            .atom;
        let mapped =
            connection.get_window_attributes(window)?.reply()?.map_state != MapState::UNMAPPED;
        if mapped {
            let event = ClientMessageEvent::new(32, window, state, message_data(skip));
            connection.send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )?;
        } else {
            connection.change_property32(
                PropMode::APPEND,
                window,
                state,
                AtomEnum::ATOM,
                &[skip],
            )?;
        }
        connection.flush()?;
        Ok(())
    }

    /// The `_NET_WM_STATE` request that adds one state.
    pub(super) fn message_data(atom: u32) -> [u32; 5] {
        [ADD, atom, 0, FROM_APPLICATION, 0]
    }
}

/// The main window's smallest size with no side panel open. The page and
/// its top bar fit beside the narrowest sidebar from here.
pub const MAIN_MIN_SIZE: [f32; 2] = [760.0, 520.0];

/// A remembered main window size to open at, or `None` for the default. Under
/// the minimum is not a size anyone chose, since the window cannot be dragged
/// that small, and opening at it leaves a window too small to find (#704).
pub fn usable_size(size: Option<[f32; 2]>) -> Option<[f32; 2]> {
    size.filter(|size| size[0] >= MAIN_MIN_SIZE[0] && size[1] >= MAIN_MIN_SIZE[1])
}

static FIXED_SIZE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the main window keeps one size it was given, as demo shots ask,
/// so the interface must not raise its minimum.
pub fn fixed_size() -> bool {
    FIXED_SIZE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pins the main window to the size it is created with.
pub fn set_fixed_size(on: bool) {
    FIXED_SIZE.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_window_points_arm_at_the_store_pack_and_others_at_the_guide() {
        use fastframe_graphics::{Cause, Trouble};
        let english = crate::i18n::Locale::English;
        let details = "egui_glow requires opengl 2.0+";
        let arm = no_window_dialog(
            english,
            &Trouble::new(
                "Spotifast",
                Cause::WindowsOnArm,
                details,
                Some(DOWNLOAD_PAGE),
            ),
        );
        assert!(arm.body.contains("Compatibility Pack"));
        assert!(
            arm.link
                .as_deref()
                .is_some_and(|link| link.starts_with("ms-windows-store:"))
        );

        let x64 = no_window_dialog(
            english,
            &Trouble::new(
                "Spotifast",
                Cause::NoGraphicsDriver,
                details,
                Some(DOWNLOAD_PAGE),
            ),
        );
        assert_eq!(x64.title, "Spotifast cannot open its window");
        assert_eq!(x64.link.as_deref(), Some(DOWNLOAD_PAGE));
        assert!(x64.body.contains("OpenGL 2.0"));
        assert!(x64.body.contains("virtual machine"));
        assert!(x64.body.contains(DOWNLOAD_PAGE));
        assert!(!x64.body.contains("{url}"));
        assert_eq!(
            (x64.open_label.as_str(), x64.close_label.as_str()),
            ("Open page", "Close")
        );
    }

    #[test]
    fn only_the_wayland_backend_lacks_window_level_control() {
        use raw_window_handle::{
            RawDisplayHandle, WaylandDisplayHandle, XcbDisplayHandle, XlibDisplayHandle,
        };
        let wayland = WaylandDisplayHandle::new(std::ptr::NonNull::dangling());
        assert!(!supports_window_level(RawDisplayHandle::Wayland(wayland)));
        assert!(supports_window_level(RawDisplayHandle::Xlib(
            XlibDisplayHandle::new(None, 0)
        )));
        assert!(supports_window_level(RawDisplayHandle::Xcb(
            XcbDisplayHandle::new(None, 0)
        )));
    }

    #[test]
    fn only_windows_and_x11_can_hide_the_mini_player_from_the_taskbar() {
        use raw_window_handle::{
            AppKitDisplayHandle, RawDisplayHandle, WaylandDisplayHandle, WindowsDisplayHandle,
            XcbDisplayHandle, XlibDisplayHandle,
        };
        let wayland = WaylandDisplayHandle::new(std::ptr::NonNull::dangling());
        assert!(!supports_hiding_from_taskbar(RawDisplayHandle::Wayland(
            wayland
        )));
        assert!(!supports_hiding_from_taskbar(RawDisplayHandle::AppKit(
            AppKitDisplayHandle::new()
        )));
        assert!(supports_hiding_from_taskbar(RawDisplayHandle::Windows(
            WindowsDisplayHandle::new()
        )));
        assert!(supports_hiding_from_taskbar(RawDisplayHandle::Xlib(
            XlibDisplayHandle::new(None, 0)
        )));
        assert!(supports_hiding_from_taskbar(RawDisplayHandle::Xcb(
            XcbDisplayHandle::new(None, 0)
        )));
    }

    /// EWMH's layout: add, the state, no second state, from an application.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_skip_taskbar_request_adds_one_state_as_an_application() {
        assert_eq!(x11::message_data(42), [1, 42, 0, 1, 0]);
    }

    /// Needs an X server (`DISPLAY`), so it is not part of the ordinary
    /// suite. The window is never mapped, so nothing appears on screen.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs an X server"]
    fn an_unmapped_x11_window_keeps_its_states_and_skips_the_taskbar() {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, WindowClass};
        use x11rb::wrapper::ConnectionExt as _;

        let (connection, screen) = x11rb::connect(None).expect("an X server");
        let root = connection.setup().roots[screen].root;
        let window = connection.generate_id().unwrap();
        connection
            .create_window(
                0,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &CreateWindowAux::new(),
            )
            .unwrap();
        let atom = |name: &[u8]| {
            connection
                .intern_atom(false, name)
                .unwrap()
                .reply()
                .unwrap()
                .atom
        };
        let state = atom(b"_NET_WM_STATE");
        let above = atom(b"_NET_WM_STATE_ABOVE");
        let skip = atom(b"_NET_WM_STATE_SKIP_TASKBAR");
        connection
            .change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                window,
                state,
                AtomEnum::ATOM,
                &[above],
            )
            .unwrap();
        connection.flush().unwrap();

        x11::skip_taskbar(window).unwrap();

        let states: Vec<u32> = connection
            .get_property(false, window, state, AtomEnum::ATOM, 0, 16)
            .unwrap()
            .reply()
            .unwrap()
            .value32()
            .unwrap()
            .collect();
        connection.destroy_window(window).unwrap();
        connection.flush().unwrap();
        assert_eq!(states, vec![above, skip]);
    }

    /// A remembered size under the minimum is not one anyone chose, since the
    /// window cannot be dragged that small. It is not restored, so the window
    /// opens at its default size instead.
    #[test]
    fn a_remembered_size_below_the_minimum_is_not_restored() {
        assert_eq!(usable_size(Some([64.0, 64.0])), None);
        assert_eq!(usable_size(Some([MAIN_MIN_SIZE[0] - 1.0, 800.0])), None);
        assert_eq!(usable_size(Some([1240.0, MAIN_MIN_SIZE[1] - 1.0])), None);
        assert_eq!(usable_size(None), None);
        for size in [MAIN_MIN_SIZE, [1240.0, 800.0], [1920.0, 1129.0]] {
            assert_eq!(usable_size(Some(size)), Some(size));
        }
    }
}
