//! Checks saved window positions before restoring them.
//!
//! eframe already places the window on-screen. The session can still refer
//! to a monitor that was unplugged, so check before moving the window there.

pub const ON_TOP_UNAVAILABLE: &str =
    "On Wayland, use your desktop's Keep Above shortcut or window rule.";

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MacosDoubleClickAction {
    Ignore,
    Minimize,
    Fill,
    Zoom,
}

#[cfg(any(target_os = "macos", test))]
fn macos_double_click_action(preference: Option<&str>) -> MacosDoubleClickAction {
    match preference {
        Some("Minimize") => MacosDoubleClickAction::Minimize,
        Some("None") => MacosDoubleClickAction::Ignore,
        Some("Maximize" | "Fill") => MacosDoubleClickAction::Fill,
        Some("Zoom") | None => MacosDoubleClickAction::Zoom,
        Some(_) => MacosDoubleClickAction::Ignore,
    }
}

#[cfg(target_os = "macos")]
fn macos_fill_target(
    frame: objc2_foundation::NSRect,
    visible: objc2_foundation::NSRect,
    previous: Option<objc2_foundation::NSRect>,
) -> objc2_foundation::NSRect {
    let same_frame = (frame.origin.x - visible.origin.x).abs() < 1.0
        && (frame.origin.y - visible.origin.y).abs() < 1.0
        && (frame.size.width - visible.size.width).abs() < 1.0
        && (frame.size.height - visible.size.height).abs() < 1.0;
    if same_frame {
        previous.unwrap_or(frame)
    } else {
        visible
    }
}

#[cfg(target_os = "macos")]
fn macos_fill_window(window_number: isize, ctx: &egui::Context) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSRect;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(window) = app.windowWithWindowNumber(window_number) else {
        return;
    };
    let Some(screen) = window.screen() else {
        return;
    };
    let frame = window.frame();
    let visible = screen.visibleFrame();
    let id = egui::Id::new(("macos-titlebar-fill", window_number));
    let target = ctx.data_mut(|data| {
        let previous = data.get_temp::<NSRect>(id);
        let target = macos_fill_target(frame, visible, previous);
        if target == visible {
            data.insert_temp(id, frame);
        }
        target
    });
    window.setFrame_display_animate(target, true, true);
}

/// Handles a macOS title-bar double-click, or leaves a first click to drag.
#[cfg(target_os = "macos")]
pub fn macos_titlebar_should_drag(ctx: &egui::Context) -> bool {
    use objc2::{MainThreadMarker, sel};
    use objc2_app_kit::{NSApplication, NSEventType};
    use objc2_foundation::{NSObjectNSDelayedPerforming, NSUserDefaults, ns_string};

    let Some(mtm) = MainThreadMarker::new() else {
        return true;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(event) = app.currentEvent() else {
        return true;
    };
    if event.r#type() != NSEventType::LeftMouseDown || event.clickCount() != 2 {
        return true;
    }

    let preference =
        NSUserDefaults::standardUserDefaults().stringForKey(ns_string!("AppleActionOnDoubleClick"));
    let action =
        macos_double_click_action(preference.as_deref().map(ToString::to_string).as_deref());
    if let Some(window) = event.window(mtm) {
        // Let egui finish this frame before AppKit starts resizing the window.
        // SAFETY: Both NSWindow selectors take one optional sender argument.
        unsafe {
            match action {
                MacosDoubleClickAction::Ignore => {}
                MacosDoubleClickAction::Minimize => window.performSelector_withObject_afterDelay(
                    sel!(performMiniaturize:),
                    None,
                    0.0,
                ),
                MacosDoubleClickAction::Fill => {
                    let window_number = window.windowNumber();
                    let ctx = ctx.clone();
                    dispatch2::DispatchQueue::main()
                        .exec_async(move || macos_fill_window(window_number, &ctx));
                }
                MacosDoubleClickAction::Zoom => {
                    window.performSelector_withObject_afterDelay(sel!(performZoom:), None, 0.0)
                }
            }
        }
    }
    false
}

#[cfg(not(target_os = "macos"))]
pub fn macos_titlebar_should_drag(_ctx: &egui::Context) -> bool {
    true
}

/// The active backend matters: a Wayland session can also host X11 windows.
/// winit's Wayland backend cannot change a window's stacking level.
pub fn supports_window_level(display: raw_window_handle::RawDisplayHandle) -> bool {
    !matches!(display, raw_window_handle::RawDisplayHandle::Wayland(_))
}

/// Whether the window already covers the screen, maximized or full screen.
///
/// eframe restores that state as it creates the window, and sizing or moving
/// the window afterwards restores it down again.
pub fn fills_the_screen(viewport: &egui::ViewportInfo) -> bool {
    viewport.maximized.unwrap_or(false) || viewport.fullscreen.unwrap_or(false)
}

/// Checks a position in egui points against the fixed coordinate limits.
#[cfg(not(windows))]
pub fn can_restore(pos: [f32; 2], _pixels_per_point: f32) -> bool {
    // Wayland ignores window moves; keep the old limits on other platforms.
    (-1000.0..=5000.0).contains(&pos[0]) && (-1000.0..=5000.0).contains(&pos[1])
}

#[cfg(any(windows, test))]
fn titlebar_anchor(pos: [f32; 2], pixels_per_point: f32) -> Option<egui::Pos2> {
    // A maximized window can start at (-8, -8), so check a point inside
    // its title bar. Convert from egui points to pixels for Win32.
    let anchor = (egui::pos2(pos[0], pos[1]) + egui::vec2(32.0, 16.0)) * pixels_per_point;
    (pixels_per_point.is_finite() && pixels_per_point > 0.0 && anchor.is_finite()).then_some(anchor)
}

/// Checks that the saved position leaves the title bar in a monitor's work area.
#[cfg(windows)]
pub fn can_restore(pos: [f32; 2], pixels_per_point: f32) -> bool {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
    };

    let Some(anchor) = titlebar_anchor(pos, pixels_per_point) else {
        return false;
    };
    let point = POINT {
        x: anchor.x as i32,
        y: anchor.y as i32,
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        // Asking for the nearest monitor would also accept off-screen positions.
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONULL);
        if monitor.is_null() || GetMonitorInfoW(monitor, &mut info) == 0 {
            return false;
        }
    }
    // Use the work area so the title bar cannot end up behind the taskbar.
    let area = info.rcWork;
    egui::Rect::from_min_max(
        egui::pos2(area.left as f32, area.top as f32),
        egui::pos2(area.right as f32, area.bottom as f32),
    )
    .contains(anchor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_titlebar_preferences_map_to_native_actions() {
        for (preference, action) in [
            (Some("Minimize"), MacosDoubleClickAction::Minimize),
            (Some("None"), MacosDoubleClickAction::Ignore),
            (Some("Maximize"), MacosDoubleClickAction::Fill),
            (Some("Fill"), MacosDoubleClickAction::Fill),
            (Some("Zoom"), MacosDoubleClickAction::Zoom),
            (None, MacosDoubleClickAction::Zoom),
            (Some("FutureAction"), MacosDoubleClickAction::Ignore),
        ] {
            assert_eq!(macos_double_click_action(preference), action);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn filling_again_restores_the_previous_window_frame() {
        use objc2_foundation::{NSPoint, NSRect, NSSize};

        let original = NSRect::new(NSPoint::new(120.0, 80.0), NSSize::new(800.0, 600.0));
        let visible = NSRect::new(NSPoint::new(0.0, 40.0), NSSize::new(1400.0, 860.0));
        assert_eq!(macos_fill_target(original, visible, None), visible);
        assert_eq!(
            macos_fill_target(visible, visible, Some(original)),
            original
        );
        let moved = NSRect::new(NSPoint::new(300.0, 150.0), NSSize::new(700.0, 500.0));
        assert_eq!(macos_fill_target(moved, visible, Some(original)), visible);
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

    fn reachable(pos: [f32; 2], scale: f32, area: [f32; 4]) -> bool {
        let rect =
            egui::Rect::from_min_max(egui::pos2(area[0], area[1]), egui::pos2(area[2], area[3]));
        titlebar_anchor(pos, scale).is_some_and(|anchor| rect.contains(anchor))
    }

    #[test]
    fn disconnected_monitor_position_is_not_restored() {
        assert!(!reachable([1912.0, -8.0], 1.0, [0.0, 0.0, 1920.0, 1032.0]));
        assert!(reachable(
            [1912.0, -8.0],
            1.0,
            [1920.0, 0.0, 3840.0, 1080.0]
        ));
    }

    #[test]
    fn visible_positions_include_maximized_and_negative_coordinates() {
        assert!(reachable([-8.0, -8.0], 1.0, [0.0, 0.0, 1920.0, 1032.0]));
        assert!(reachable(
            [-1920.0, 100.0],
            1.0,
            [-1920.0, 0.0, 0.0, 1080.0]
        ));
    }

    #[test]
    fn logical_coordinates_are_scaled_to_monitor_pixels() {
        assert!(reachable([900.0, 100.0], 2.0, [0.0, 0.0, 1920.0, 1080.0]));
        assert!(!reachable([1000.0, 100.0], 2.0, [0.0, 0.0, 1920.0, 1080.0]));
    }

    #[test]
    fn only_a_maximized_or_full_screen_window_fills_the_screen() {
        let state = |maximized, fullscreen| {
            fills_the_screen(&egui::ViewportInfo {
                maximized,
                fullscreen,
                ..Default::default()
            })
        };
        assert!(state(Some(true), Some(false)));
        assert!(state(Some(false), Some(true)));
        assert!(!state(Some(false), Some(false)));
        // A backend that does not report the state leaves the window ordinary.
        assert!(!state(None, None));
    }

    #[test]
    fn invalid_coordinates_and_scale_are_rejected() {
        assert!(titlebar_anchor([f32::NAN, 0.0], 1.0).is_none());
        assert!(titlebar_anchor([0.0, f32::INFINITY], 1.0).is_none());
        assert!(titlebar_anchor([0.0, 0.0], 0.0).is_none());
    }
}
