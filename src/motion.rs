//! Whether the desktop asks for less motion: Windows' "Animation effects",
//! macOS's "Reduce motion" and GNOME's "Animations". When it does, the
//! interface shows where each animation ends at once: egui's own easing and
//! scrolling to a place, the reveal of a new theme, the fades that follow
//! the song, the lyrics, and rows making room for a drag.

use std::time::{Duration, Instant};

/// How long an answer is trusted before the system is asked again.
const RECHECK: Duration = Duration::from_secs(1);

/// egui's easing time for hover, expansion and similar state changes.
pub const ANIMATION_TIME: f32 = 0.12;

/// The system's preference, asked at most once a second.
#[derive(Default)]
pub struct Motion {
    reduced: bool,
    checked: Option<Instant>,
    /// What the Linux desktop portal said at the last check.
    desktop: Option<bool>,
}

impl Motion {
    /// Whether less motion is asked for, as of the last check.
    pub fn reduced(&self) -> bool {
        self.reduced
    }

    /// Asks the system again when the last answer is a second old, and
    /// answers whether the preference changed. `desktop_animations` is what
    /// the Linux desktop portal last said about animations, if anything.
    /// The portal wakes the app when that changes, and nothing asks again
    /// a second later, so a new answer from it applies at once.
    pub fn refresh(&mut self, now: Instant, desktop_animations: Option<bool>) -> bool {
        if desktop_animations == self.desktop
            && self
                .checked
                .is_some_and(|checked| now.duration_since(checked) < RECHECK)
        {
            return false;
        }
        self.checked = Some(now);
        self.desktop = desktop_animations;
        self.set(system_reduced(desktop_animations))
    }

    /// Takes `reduced` as the preference, and answers whether it changed.
    pub fn set(&mut self, reduced: bool) -> bool {
        let changed = self.reduced != reduced;
        self.reduced = reduced;
        changed
    }
}

/// Sets egui's easing and scrolling for `reduced`, none when less motion is
/// asked for, and keeps it for the views' own animations to read.
pub fn apply(ctx: &egui::Context, reduced: bool) {
    let time = if reduced { 0.0 } else { ANIMATION_TIME };
    let scroll = if reduced {
        egui::style::ScrollAnimation::none()
    } else {
        egui::style::ScrollAnimation::default()
    };
    ctx.all_styles_mut(|style| {
        style.animation_time = time;
        style.scroll_animation = scroll;
    });
    ctx.data_mut(|data| data.insert_temp(reduced_id(), reduced));
}

fn reduced_id() -> egui::Id {
    egui::Id::new("spotifast-reduced-motion")
}

/// Whether less motion is asked for, as last applied to `ctx`.
pub fn reduced(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp(reduced_id()))
        .unwrap_or(false)
}

/// How long an animation of `seconds` takes: none when less motion is asked
/// for, so it shows where it ends at once.
pub fn seconds(ctx: &egui::Context, seconds: f32) -> f32 {
    if reduced(ctx) { 0.0 } else { seconds }
}

/// A scroll to a place over `seconds`, or at once when less motion is asked
/// for.
pub fn scroll(ctx: &egui::Context, seconds: f32) -> egui::style::ScrollAnimation {
    if reduced(ctx) {
        egui::style::ScrollAnimation::none()
    } else {
        egui::style::ScrollAnimation::duration(seconds)
    }
}

#[cfg(windows)]
fn system_reduced(_desktop_animations: Option<bool>) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };
    let mut enabled: windows_sys::core::BOOL = 1;
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut windows_sys::core::BOOL).cast(),
            0,
        )
    };
    read != 0 && enabled == 0
}

#[cfg(target_os = "macos")]
fn system_reduced(_desktop_animations: Option<bool>) -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn system_reduced(desktop_animations: Option<bool>) -> bool {
    desktop_animations == Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_is_asked_at_most_once_a_second() {
        let mut motion = Motion::default();
        let start = Instant::now();
        motion.refresh(start, None);
        motion.set(true);
        assert!(!motion.refresh(start + Duration::from_millis(500), None));
        assert!(motion.reduced(), "a fresh answer is kept");
    }

    #[test]
    fn a_change_is_reported_once() {
        let mut motion = Motion::default();
        assert!(motion.set(true));
        assert!(!motion.set(true));
        assert!(motion.set(false));
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_follows_the_desktops_animations_setting() {
        assert!(system_reduced(Some(false)));
        assert!(!system_reduced(Some(true)));
        assert!(!system_reduced(None));
    }

    /// The portal wakes the app once when its switch changes; that answer
    /// applies then, even within a second of the last check.
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn a_portal_change_applies_within_the_second() {
        let mut motion = Motion::default();
        let start = Instant::now();
        assert!(!motion.refresh(start, Some(true)));
        assert!(motion.refresh(start + Duration::from_millis(100), Some(false)));
        assert!(motion.reduced());
        assert!(
            !motion.refresh(start + Duration::from_millis(200), Some(false)),
            "an unchanged answer waits out the second"
        );
        assert!(motion.refresh(start + Duration::from_millis(300), Some(true)));
        assert!(!motion.reduced());
    }

    #[test]
    fn reduced_motion_turns_egui_easing_off() {
        let ctx = egui::Context::default();
        apply(&ctx, true);
        assert_eq!(ctx.global_style().animation_time, 0.0);
        assert_eq!(
            ctx.global_style().scroll_animation,
            egui::style::ScrollAnimation::none()
        );
        apply(&ctx, false);
        assert_eq!(ctx.global_style().animation_time, ANIMATION_TIME);
        assert_eq!(
            ctx.global_style().scroll_animation,
            egui::style::ScrollAnimation::default()
        );
    }

    /// The views' own animations read the same preference: none with less
    /// motion, their own length otherwise.
    #[test]
    fn the_views_own_animations_follow_the_preference() {
        let ctx = egui::Context::default();
        assert_eq!(seconds(&ctx, 0.3), 0.3, "motion until told otherwise");
        apply(&ctx, true);
        assert_eq!(seconds(&ctx, 0.3), 0.0);
        assert_eq!(scroll(&ctx, 0.45), egui::style::ScrollAnimation::none());
        apply(&ctx, false);
        assert_eq!(seconds(&ctx, 0.3), 0.3);
        assert_eq!(
            scroll(&ctx, 0.45),
            egui::style::ScrollAnimation::duration(0.45)
        );
    }
}
