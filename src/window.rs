//! Checks saved window positions before restoring them.
//!
//! eframe already places the window on-screen. The session can still refer
//! to a monitor that was unplugged, so check before moving the window there.

pub const ON_TOP_UNAVAILABLE: &str =
    "On Wayland, use your desktop's Keep Above shortcut or window rule.";

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

/// Hides macOS windows before eframe destroys their native views.
#[cfg(target_os = "macos")]
pub fn prepare_for_close() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("the macOS window can only close on the main thread");
        return;
    };
    for window in NSApplication::sharedApplication(mtm).windows().iter() {
        window.orderOut(None);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn prepare_for_close() {}

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
