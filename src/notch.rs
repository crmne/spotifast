//! Cross-platform notch widget interface.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotchCommand {
    PlayPause,
    Next,
    Previous,
    ShowWindow,
    ToggleSaved(String),
    Seek(u32),
}

impl NotchCommand {
    pub fn action(&self) -> crate::model::Action {
        match self {
            Self::PlayPause => crate::model::Action::TogglePlay,
            Self::Next => crate::model::Action::Next,
            Self::Previous => crate::model::Action::Previous,
            Self::ShowWindow => crate::model::Action::ShowWindow,
            Self::ToggleSaved(uri) => crate::model::Action::ToggleSaved(uri.clone()),
            Self::Seek(pos) => crate::model::Action::Seek(*pos),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotchTrackInfo {
    pub playing: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u32,
    pub position_ms: u32,
    pub art_path: Option<std::path::PathBuf>,
    pub uri: String,
    pub saved: bool,
    pub accent: Option<[u8; 3]>,
    pub levels: [f32; 5],
    pub is_episode: bool,
}

/// Pure helper to determine whether the notch widget should expand on hover.
pub fn should_expand_notch(enabled: bool, in_foreground: bool) -> bool {
    enabled && !in_foreground
}

/// Pure helper to determine if the save button should be displayed.
pub fn should_show_save_button(is_episode: bool) -> bool {
    !is_episode
}

/// Pure helper to determine whether toggling saved status is allowed for a media item.
pub fn can_toggle_saved(is_episode: bool) -> bool {
    !is_episode
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NotchDimensions {
    pub collapsed_x: f64,
    pub collapsed_y: f64,
    pub collapsed_w: f64,
    pub collapsed_h: f64,
    pub expanded_x: f64,
    pub expanded_y: f64,
    pub expanded_w: f64,
    pub expanded_h: f64,
}

/// Pure helper to compute notch window geometry.
/// Returns None if the display does not have a physical notch (top_inset <= 0.0).
pub fn compute_notch_geometry(
    screen_origin_x: f64,
    screen_origin_y: f64,
    screen_w: f64,
    screen_h: f64,
    top_inset: f64,
) -> Option<NotchDimensions> {
    if top_inset <= 0.0 {
        return None;
    }
    let center_x = screen_origin_x + screen_w / 2.0;
    let screen_top = screen_origin_y + screen_h;
    let notch_h = top_inset;
    let notch_w = 200.0;
    let gap = 8.0;
    let card_w = 400.0;
    let card_h = 148.0;
    let total_h = notch_h + gap + card_h;

    Some(NotchDimensions {
        collapsed_x: center_x - notch_w / 2.0,
        collapsed_y: screen_top - notch_h,
        collapsed_w: notch_w,
        collapsed_h: notch_h,
        expanded_x: center_x - card_w / 2.0,
        expanded_y: screen_top - total_h,
        expanded_w: card_w,
        expanded_h: total_h,
    })
}

use std::sync::Mutex;

static COMMANDS: Mutex<Vec<NotchCommand>> = Mutex::new(Vec::new());
static WAKER: Mutex<Option<Box<dyn Fn() + Send + Sync>>> = Mutex::new(None);

pub fn set_waker(wake: impl Fn() + Send + Sync + 'static) {
    if let Ok(mut w) = WAKER.lock() {
        *w = Some(Box::new(wake));
    }
}

pub fn wake() {
    if let Ok(w) = WAKER.lock()
        && let Some(wake) = w.as_ref()
    {
        wake();
    }
}

pub fn push_command(cmd: NotchCommand) {
    if let Ok(mut list) = COMMANDS.lock() {
        list.push(cmd);
    }
    wake();
}

pub fn drain_commands() -> Vec<NotchCommand> {
    if let Ok(mut list) = COMMANDS.lock() {
        std::mem::take(&mut *list)
    } else {
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
pub use crate::mac_notch::{init, is_active, sync_state};

#[cfg(not(target_os = "macos"))]
pub fn init() {}

#[cfg(not(target_os = "macos"))]
pub fn sync_state(_enabled: bool, _is_background: bool, _track: Option<&NotchTrackInfo>) {}

#[cfg(not(target_os = "macos"))]
pub fn is_active() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notch_command_queuing_and_draining() {
        push_command(NotchCommand::PlayPause);
        push_command(NotchCommand::Next);
        push_command(NotchCommand::Previous);
        push_command(NotchCommand::ShowWindow);

        let commands = drain_commands();
        assert_eq!(
            commands,
            vec![
                NotchCommand::PlayPause,
                NotchCommand::Next,
                NotchCommand::Previous,
                NotchCommand::ShowWindow,
            ]
        );

        let empty = drain_commands();
        assert!(empty.is_empty());
    }

    #[test]
    fn notch_track_info_equality() {
        let a = NotchTrackInfo {
            playing: true,
            title: "Test Track".into(),
            artist: "Test Artist".into(),
            album: "Test Album".into(),
            duration_ms: 180000,
            position_ms: 45000,
            art_path: None,
            uri: "spotify:track:test".into(),
            saved: false,
            accent: Some([30, 215, 96]),
            levels: [0.1, 0.3, 0.5, 0.7, 0.9],
            is_episode: false,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn notch_command_actions() {
        use crate::model::Action;
        assert!(matches!(
            NotchCommand::PlayPause.action(),
            Action::TogglePlay
        ));
        assert!(matches!(NotchCommand::Next.action(), Action::Next));
        assert!(matches!(NotchCommand::Previous.action(), Action::Previous));
        assert!(matches!(
            NotchCommand::ShowWindow.action(),
            Action::ShowWindow
        ));
        assert!(matches!(
            NotchCommand::ToggleSaved("spotify:track:abc".into()).action(),
            Action::ToggleSaved(uri) if uri == "spotify:track:abc"
        ));
        assert!(matches!(
            NotchCommand::Seek(12345).action(),
            Action::Seek(12345)
        ));
    }

    #[test]
    fn should_expand_notch_rules() {
        // Disabled widget never expands
        assert!(!should_expand_notch(false, false));
        assert!(!should_expand_notch(false, true));

        // When Spotifast is in the foreground, widget remains suppressed
        assert!(!should_expand_notch(true, true));

        // When in background and enabled, expands on hover even when music is not playing
        assert!(should_expand_notch(true, false));
    }

    #[test]
    fn episode_save_restrictions() {
        // Episodes should neither show the save button nor allow saving
        assert!(!should_show_save_button(true));
        assert!(!can_toggle_saved(true));

        // Regular tracks show the save button and allow saving
        assert!(should_show_save_button(false));
        assert!(can_toggle_saved(false));
    }

    #[test]
    fn compute_notch_geometry_rules() {
        // Displays without physical notch (top_inset <= 0.0) return None
        assert_eq!(compute_notch_geometry(0.0, 0.0, 1920.0, 1080.0, 0.0), None);
        assert_eq!(
            compute_notch_geometry(0.0, 0.0, 1920.0, 1080.0, -10.0),
            None
        );

        // MacBook display with notch (e.g. 1728 x 1117 with 34pt top inset)
        let dims = compute_notch_geometry(0.0, 0.0, 1728.0, 1117.0, 34.0)
            .expect("should compute notch dimensions");
        assert_eq!(dims.collapsed_w, 200.0);
        assert_eq!(dims.collapsed_h, 34.0);
        assert_eq!(dims.collapsed_x, 1728.0 / 2.0 - 100.0);
        assert_eq!(dims.collapsed_y, 1117.0 - 34.0);

        assert_eq!(dims.expanded_w, 400.0);
        let total_h = 34.0 + 8.0 + 148.0;
        assert_eq!(dims.expanded_h, total_h);
        assert_eq!(dims.expanded_x, 1728.0 / 2.0 - 200.0);
        assert_eq!(dims.expanded_y, 1117.0 - total_h);
    }
}
