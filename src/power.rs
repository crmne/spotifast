//! The device's power, session and network state, and what the app may
//! spend because of it.
//!
//! fastframe-power reads the system: energy saver, battery, display, lock,
//! the network's reach and cost, sleep and wake. The app reads its
//! [`Conditions`] and [`Event`]s in the logic pass, which runs with or
//! without a window, and turns the conditions into a [`Budget`] for its
//! drawing and polling here.

pub use fastframe_power::{Ack, Activity, Conditions, Event, Power};

use crate::player::{LocalState, Playback};

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

/// MilkDrop's frame rate within `conditions`, from the user's `fps` (0 is
/// unlimited): at most 30 while saving energy, and one a second while
/// nobody can see any window (the display off or the session locked).
/// MilkDrop has a window of its own, so the main window's state does not
/// count; its own window being covered or minimised is MilkDrop's to notice.
pub fn milkdrop_fps(conditions: Conditions, fps: u32) -> u32 {
    if conditions.display_off || conditions.locked {
        1
    } else if conditions.saver && (fps == 0 || fps > 30) {
        30
    } else {
        fps
    }
}

/// The frame rate of the main window's visualizers (the player bar's and
/// the mini player's) within `budget`, from the user's `fps` (0 is uncapped,
/// the display's own rate): as chosen in focus, half of it out of focus or
/// while saving energy, and at most 30 while saving energy. `None` while
/// nobody can see the window: no frames. Uncapped halves to 30, the half
/// rate these visualizers always had.
pub fn visualiser_fps(budget: Budget, conditions: Conditions, fps: u32) -> Option<u32> {
    match budget {
        Budget::Background => None,
        Budget::Normal => Some(fps),
        Budget::Saver => {
            let half = if fps == 0 { 30 } else { (fps / 2).max(1) };
            Some(if conditions.saver { half.min(30) } else { half })
        }
    }
}

/// Whether local playback is doing work the user is waiting for, which
/// macOS must not slow down with App Nap while another app is in front:
/// playing or loading on a connected engine. Pause, stop, an output error
/// and a disconnection all let the app nap again; remote playback never
/// holds it awake.
pub fn playback_needs_activity(local: &LocalState) -> bool {
    local.connected
        && local.error.is_none()
        && (local.loading || matches!(local.playback, Playback::Playing | Playback::Loading))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The player bar and mini player visualizers keep the rate chosen in
    /// focus, halve it out of focus, draw at most 30 saving energy, and
    /// none while nobody can see them.
    #[test]
    fn the_visualizers_halve_their_rate_out_of_focus_and_stop_unseen() {
        let normal = Conditions::default();
        let saving = Conditions {
            saver: true,
            ..normal
        };
        assert_eq!(visualiser_fps(Budget::Normal, normal, 60), Some(60));
        assert_eq!(visualiser_fps(Budget::Normal, normal, 0), Some(0));
        assert_eq!(
            visualiser_fps(Budget::Saver, normal, 144),
            Some(72),
            "out of focus"
        );
        assert_eq!(visualiser_fps(Budget::Saver, normal, 0), Some(30));
        assert_eq!(
            visualiser_fps(Budget::Saver, saving, 144),
            Some(30),
            "saving energy"
        );
        assert_eq!(visualiser_fps(Budget::Saver, saving, 30), Some(15));
        assert_eq!(visualiser_fps(Budget::Background, normal, 60), None);
        assert_eq!(visualiser_fps(Budget::Background, normal, 0), None);
    }

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
    fn milkdrop_slows_down_with_the_system_not_the_main_window() {
        let normal = Conditions::default();
        assert_eq!(milkdrop_fps(normal, 60), 60);
        assert_eq!(milkdrop_fps(normal, 0), 0, "unlimited stays unlimited");
        let saving = Conditions {
            saver: true,
            ..normal
        };
        assert_eq!(milkdrop_fps(saving, 60), 30);
        assert_eq!(milkdrop_fps(saving, 0), 30);
        assert_eq!(milkdrop_fps(saving, 24), 24);
        let dark = Conditions {
            display_off: true,
            ..normal
        };
        assert_eq!(milkdrop_fps(dark, 60), 1);
    }

    /// #789: local playback holds off App Nap while it plays or loads, and
    /// lets go on pause, stop, an output error or a disconnection.
    #[test]
    fn only_local_playback_at_work_needs_an_activity() {
        let mut local = LocalState {
            connected: true,
            playback: Playback::Playing,
            ..LocalState::default()
        };
        assert!(playback_needs_activity(&local));
        local.playback = Playback::Loading;
        assert!(playback_needs_activity(&local));
        local.playback = Playback::Paused;
        local.loading = true;
        assert!(playback_needs_activity(&local), "the next track is loading");
        local.loading = false;
        for playback in [Playback::Paused, Playback::Stopped] {
            local.playback = playback;
            assert!(!playback_needs_activity(&local));
        }
        local.playback = Playback::Playing;
        local.error = Some("output unavailable".into());
        assert!(!playback_needs_activity(&local));
        local.error = None;
        local.connected = false;
        assert!(!playback_needs_activity(&local));
        assert!(!playback_needs_activity(&LocalState::default()));
    }
}
