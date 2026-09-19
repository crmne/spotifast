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
    pub is_remote: bool,
}

/// Pure helper to determine whether the notch overlay window should be visible on screen.
pub fn should_show_notch_window(
    enabled: bool,
    in_foreground: bool,
    has_track: bool,
    has_notch: bool,
) -> bool {
    enabled && !in_foreground && has_track && has_notch
}

/// Pure helper to determine whether entering the notch area should schedule an expansion timer.
pub fn should_schedule_expand(
    enabled: bool,
    in_foreground: bool,
    has_track: bool,
    is_expanded: bool,
    has_pending_expand: bool,
) -> bool {
    enabled && !in_foreground && has_track && !is_expanded && !has_pending_expand
}

/// Pure helper to determine whether cursor exit should schedule a collapse timer.
pub fn should_schedule_collapse(is_expanded: bool) -> bool {
    is_expanded
}

/// Pure helper to verify whether an expand timer callback should execute expansion.
pub fn should_perform_expand(
    enabled: bool,
    in_foreground: bool,
    has_track: bool,
    is_expanded: bool,
) -> bool {
    enabled && !in_foreground && has_track && !is_expanded
}

/// Pure helper to verify whether a collapse timer callback should execute collapse.
pub fn should_perform_collapse(is_expanded: bool) -> bool {
    is_expanded
}

/// Pure helper to determine whether the controller is actively animating,
/// expanded, or waiting on scheduled transitions.
pub fn is_controller_active(
    is_expanded: bool,
    has_pending_expand: bool,
    has_pending_collapse: bool,
    is_collapsing: bool,
) -> bool {
    is_expanded || has_pending_expand || has_pending_collapse || is_collapsing
}

/// Pure helper to determine whether the post-equalizer visualizer should sample audio and compute FFT.
/// Visualizer audio tap is only sampled when the widget is enabled, the main window is not in foreground,
/// the notch overlay is active (expanded or animating), playback is actively running, and audio is local.
pub fn should_sample_visualizer(
    enabled: bool,
    is_background: bool,
    is_active: bool,
    is_playing: bool,
    is_local: bool,
) -> bool {
    enabled && is_background && is_active && is_playing && is_local
}

/// Pure helper to determine if incoming track metadata differs from cached track metadata.
pub fn is_track_metadata_different(
    cached: Option<&NotchTrackInfo>,
    incoming: Option<&NotchTrackInfo>,
) -> bool {
    match (cached, incoming) {
        (Some(c), Some(t)) => {
            c.uri != t.uri
                || c.title != t.title
                || c.artist != t.artist
                || c.album != t.album
                || c.is_episode != t.is_episode
        }
        (None, None) => false,
        _ => true,
    }
}

/// Pure helper to determine whether changing tracks warrants a crossfade transition animation.
/// Transitions are only animated when the notch widget is currently expanded and visible.
pub fn should_animate_track_transition(
    is_expanded: bool,
    cached: Option<&NotchTrackInfo>,
    incoming: Option<&NotchTrackInfo>,
) -> bool {
    is_expanded
        && cached.is_some()
        && incoming.is_some()
        && is_track_metadata_different(cached, incoming)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IncrementalChanges {
    pub progress_changed: bool,
    pub duration_changed: bool,
    pub play_changed: bool,
    pub saved_changed: bool,
    pub episode_changed: bool,
    pub art_changed: bool,
    pub accent_changed: bool,
    pub remote_changed: bool,
}

pub fn detect_incremental_changes(
    cached: &NotchTrackInfo,
    latest: &NotchTrackInfo,
) -> IncrementalChanges {
    IncrementalChanges {
        progress_changed: cached.position_ms / 1000 != latest.position_ms / 1000,
        duration_changed: cached.duration_ms / 1000 != latest.duration_ms / 1000,
        play_changed: cached.playing != latest.playing,
        saved_changed: cached.saved != latest.saved,
        episode_changed: cached.is_episode != latest.is_episode,
        art_changed: cached.art_path != latest.art_path,
        accent_changed: cached.accent != latest.accent,
        remote_changed: cached.is_remote != latest.is_remote,
    }
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
/// Returns None if the display does not have a physical notch (top_inset <= 0.0)
/// or if any geometry parameters are non-finite or non-positive.
pub fn compute_notch_geometry(
    screen_origin_x: f64,
    screen_origin_y: f64,
    screen_w: f64,
    screen_h: f64,
    top_inset: f64,
) -> Option<NotchDimensions> {
    if top_inset <= 0.0
        || !top_inset.is_finite()
        || screen_w <= 0.0
        || screen_h <= 0.0
        || !screen_w.is_finite()
        || !screen_h.is_finite()
        || !screen_origin_x.is_finite()
        || !screen_origin_y.is_finite()
    {
        return None;
    }
    let center_x = screen_origin_x + screen_w / 2.0;
    let screen_top = screen_origin_y + screen_h;
    let notch_h = top_inset;
    let notch_w = 200.0f64.min(screen_w);
    let gap = 8.0;
    let card_w = 400.0f64.min(screen_w);
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
            is_remote: false,
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
    fn episode_save_restrictions() {
        // Episodes do not allow saving
        assert!(!can_toggle_saved(true));

        // Regular tracks allow saving
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

        // Defensive against non-finite or invalid inputs
        assert!(compute_notch_geometry(0.0, 0.0, 1512.0, 982.0, f64::NAN).is_none());
        assert!(compute_notch_geometry(0.0, 0.0, 1512.0, 982.0, f64::INFINITY).is_none());
        assert!(compute_notch_geometry(f64::NAN, 0.0, 1512.0, 982.0, 32.0).is_none());
        assert!(compute_notch_geometry(0.0, 0.0, -100.0, 982.0, 32.0).is_none());
        assert!(compute_notch_geometry(0.0, 0.0, 1512.0, 0.0, 32.0).is_none());

        // Narrow screen clamps width to screen width
        let narrow = compute_notch_geometry(0.0, 0.0, 300.0, 500.0, 24.0).unwrap();
        assert_eq!(narrow.expanded_w, 300.0);
        assert_eq!(narrow.collapsed_w, 200.0);
    }

    #[test]
    fn should_show_notch_window_rules() {
        // All four conditions must be true: enabled, !in_foreground, has_track, has_notch
        assert!(should_show_notch_window(true, false, true, true));

        // If disabled, never show
        assert!(!should_show_notch_window(false, false, true, true));

        // If app is in foreground, keep suppressed so main window has focus
        assert!(!should_show_notch_window(true, true, true, true));

        // If no track is present, never show
        assert!(!should_show_notch_window(true, false, false, true));

        // If display lacks a physical notch (clamshell mode or external monitor), never show
        assert!(!should_show_notch_window(true, false, true, false));
    }

    #[test]
    fn hover_timer_transition_rules() {
        // Entering notch: schedules expand only if enabled, in background, has track, collapsed, and no pending expand
        assert!(should_schedule_expand(true, false, true, false, false));
        assert!(!should_schedule_expand(false, false, true, false, false));
        assert!(!should_schedule_expand(true, true, true, false, false));
        assert!(!should_schedule_expand(true, false, false, false, false));
        assert!(!should_schedule_expand(true, false, true, true, false));
        assert!(!should_schedule_expand(true, false, true, false, true));

        // Exiting notch: schedules collapse only if currently expanded
        assert!(should_schedule_collapse(true));
        assert!(!should_schedule_collapse(false));

        // Expand timer fires: executes only if still valid
        assert!(should_perform_expand(true, false, true, false));
        assert!(!should_perform_expand(false, false, true, false));
        assert!(!should_perform_expand(true, true, true, false));
        assert!(!should_perform_expand(true, false, false, false));
        assert!(!should_perform_expand(true, false, true, true));

        // Collapse timer fires: executes only if expanded
        assert!(should_perform_collapse(true));
        assert!(!should_perform_collapse(false));
    }

    #[test]
    fn controller_active_state_rules() {
        // Controller is active if expanded, pending expand, pending collapse, or collapsing
        assert!(!is_controller_active(false, false, false, false));
        assert!(is_controller_active(true, false, false, false));
        assert!(is_controller_active(false, true, false, false));
        assert!(is_controller_active(false, false, true, false));
        assert!(is_controller_active(false, false, false, true));
    }

    #[test]
    fn track_metadata_difference_and_transition_rules() {
        let t1 = NotchTrackInfo {
            playing: true,
            title: "Song A".into(),
            artist: "Artist A".into(),
            album: "Album A".into(),
            duration_ms: 200000,
            position_ms: 10000,
            art_path: Some("/tmp/a.jpg".into()),
            uri: "spotify:track:a".into(),
            saved: false,
            accent: Some([20, 30, 40]),
            levels: [0.0; 5],
            is_episode: false,
            is_remote: false,
        };
        let mut t2 = t1.clone();

        // Identical track info
        assert!(!is_track_metadata_different(Some(&t1), Some(&t2)));
        assert!(!should_animate_track_transition(true, Some(&t1), Some(&t2)));
        assert!(!should_animate_track_transition(
            false,
            Some(&t1),
            Some(&t2)
        ));

        // Difference in title triggers metadata difference, but only animates when expanded
        t2.title = "Song B".into();
        assert!(is_track_metadata_different(Some(&t1), Some(&t2)));
        assert!(should_animate_track_transition(true, Some(&t1), Some(&t2)));
        assert!(!should_animate_track_transition(
            false,
            Some(&t1),
            Some(&t2)
        ));

        // Difference only in art_path does not trigger metadata difference or track transition
        t2 = t1.clone();
        t2.art_path = Some("/tmp/new_art.jpg".into());
        assert!(!is_track_metadata_different(Some(&t1), Some(&t2)));
        assert!(!should_animate_track_transition(true, Some(&t1), Some(&t2)));

        // None to None has no difference
        assert!(!is_track_metadata_different(None, None));
        assert!(!should_animate_track_transition(true, None, None));

        // None to Some (initial track load) or Some to None (stop) differs, but does not crossfade animate
        assert!(is_track_metadata_different(None, Some(&t1)));
        assert!(!should_animate_track_transition(true, None, Some(&t1)));
        assert!(is_track_metadata_different(Some(&t1), None));
        assert!(!should_animate_track_transition(true, Some(&t1), None));
    }

    #[test]
    fn incremental_changes_detection_rules() {
        let t1 = NotchTrackInfo {
            playing: true,
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_ms: 200000,
            position_ms: 10100, // 10s
            art_path: None,
            uri: "spotify:track:s".into(),
            saved: false,
            accent: None,
            levels: [0.0; 5],
            is_episode: false,
            is_remote: false,
        };

        // Same second (10100 -> 10900) does not flag progress changed
        let mut t2 = t1.clone();
        t2.position_ms = 10900;
        let c = detect_incremental_changes(&t1, &t2);
        assert_eq!(
            c,
            IncrementalChanges {
                progress_changed: false,
                duration_changed: false,
                play_changed: false,
                saved_changed: false,
                episode_changed: false,
                art_changed: false,
                accent_changed: false,
                remote_changed: false,
            }
        );

        // Position crosses second boundary (10100 -> 11050)
        t2.position_ms = 11050;
        assert!(detect_incremental_changes(&t1, &t2).progress_changed);

        // Duration crosses second boundary (200000 -> 205000)
        t2 = t1.clone();
        t2.duration_ms = 205000;
        assert!(detect_incremental_changes(&t1, &t2).duration_changed);

        // Playing status toggles
        t2 = t1.clone();
        t2.playing = false;
        assert!(detect_incremental_changes(&t1, &t2).play_changed);

        // Saved status toggles
        t2 = t1.clone();
        t2.saved = true;
        assert!(detect_incremental_changes(&t1, &t2).saved_changed);

        // Episode toggles
        t2 = t1.clone();
        t2.is_episode = true;
        assert!(detect_incremental_changes(&t1, &t2).episode_changed);

        // Art path updates
        t2 = t1.clone();
        t2.art_path = Some("/tmp/cover.jpg".into());
        assert!(detect_incremental_changes(&t1, &t2).art_changed);

        // Accent updates
        t2 = t1.clone();
        t2.accent = Some([30, 40, 50]);
        assert!(detect_incremental_changes(&t1, &t2).accent_changed);

        // Remote status updates
        t2 = t1.clone();
        t2.is_remote = true;
        assert!(detect_incremental_changes(&t1, &t2).remote_changed);
    }

    #[test]
    fn should_sample_visualizer_state_and_transition_rules() {
        // Active in background with local playback: samples FFT
        assert!(should_sample_visualizer(true, true, true, true, true));

        // When main window is in foreground: zero FFT sampling
        assert!(!should_sample_visualizer(true, false, true, true, true));

        // When collapsed or inactive in background: zero FFT sampling (zero collapsed overhead)
        assert!(!should_sample_visualizer(true, true, false, true, true));

        // When playback is paused: zero FFT sampling
        assert!(!should_sample_visualizer(true, true, true, false, true));

        // When playback is remote (Spotify Connect): zero FFT sampling (flat honest baseline)
        assert!(!should_sample_visualizer(true, true, true, true, false));

        // When widget is disabled in Settings: zero FFT sampling
        assert!(!should_sample_visualizer(false, true, true, true, true));

        // Transition: collapsed -> hover expand -> active
        let mut is_active = false;
        assert!(!should_sample_visualizer(true, true, is_active, true, true));
        is_active = true;
        assert!(should_sample_visualizer(true, true, is_active, true, true));

        // Transition: active -> collapse completes -> inactive
        is_active = false;
        assert!(!should_sample_visualizer(true, true, is_active, true, true));

        // Transition: background -> foreground window focus
        let mut in_background = true;
        assert!(should_sample_visualizer(
            true,
            in_background,
            true,
            true,
            true
        ));
        in_background = false;
        assert!(!should_sample_visualizer(
            true,
            in_background,
            true,
            true,
            true
        ));

        // Transition: playing locally -> paused or switched to remote device
        let mut is_playing = true;
        let mut is_local = true;
        assert!(should_sample_visualizer(
            true, true, true, is_playing, is_local
        ));
        is_playing = false;
        assert!(!should_sample_visualizer(
            true, true, true, is_playing, is_local
        ));
        is_playing = true;
        is_local = false;
        assert!(!should_sample_visualizer(
            true, true, true, is_playing, is_local
        ));
    }
}
