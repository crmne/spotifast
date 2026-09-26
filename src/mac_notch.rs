//! Interactive MacBook notch "Now Playing" overlay widget for macOS.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::{MainThreadOnly, define_class, sel};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSApplication, NSBackingStoreType,
    NSBezierPath, NSButton, NSColor, NSEvent, NSFont, NSImage, NSImageView, NSScreen, NSTextField,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSData, NSObject, NSPoint, NSRect, NSSize, NSString};

use super::notch::{NotchCommand, NotchTrackInfo, can_toggle_saved, push_command};

static CONTROLLER: Mutex<Option<NotchController>> = Mutex::new(None);
/// Generation counter incremented each time an artwork load starts.
/// Threads capture the generation at spawn; they only write PENDING_ART
/// when their captured generation still matches the current one.
static ART_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PENDING_ART: Mutex<Option<(PathBuf, Vec<u8>)>> = Mutex::new(None);

/// Generation token for scheduled hover expand/collapse timers.
static HOVER_TIMER_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[repr(u64)]
#[derive(Copy, Clone, PartialEq, Eq)]
enum HoverTimerAction {
    Expand = 1,
    Collapse = 2,
    CompleteCollapse = 3,
}

unsafe extern "C" {
    static _dispatch_main_q: [u8; 0];
    fn dispatch_time(when: u64, delta: i64) -> u64;
    fn dispatch_after_f(
        when: u64,
        queue: *const u8,
        context: *mut std::ffi::c_void,
        work: extern "C" fn(*mut std::ffi::c_void),
    );
    fn dispatch_async_f(
        queue: *const u8,
        context: *mut std::ffi::c_void,
        work: extern "C" fn(*mut std::ffi::c_void),
    );
}

fn apply_pending_artwork_locked(ctrl: &mut NotchController, mtm: MainThreadMarker) {
    if let Ok(mut pending) = PENDING_ART.lock()
        && let Some((path, bytes)) = pending.take()
        && ctrl.current_art_path.as_ref() == Some(&path)
    {
        let ns_data = NSData::with_bytes(&bytes);
        let img = NSImage::initWithData(mtm.alloc(), &ns_data);
        ctrl.art_view.setImage(img.as_deref());
        ctrl.canvas_view.setNeedsDisplay(true);
    }
}

extern "C" fn on_artwork_loaded(_context: *mut std::ffi::c_void) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Ok(mut lock) = CONTROLLER.lock() else {
            return;
        };
        let Some(ctrl) = lock.as_mut() else {
            return;
        };
        if let Some(mtm) = MainThreadMarker::new() {
            apply_pending_artwork_locked(ctrl, mtm);
        }
    }));
}

fn spawn_art_loader(path: PathBuf) {
    let art_gen = ART_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    std::thread::spawn(move || {
        // Enforce 10MB limit to defend against unbounded memory consumption
        if let Ok(meta) = std::fs::metadata(&path)
            && meta.len() <= 10 * 1024 * 1024
            && let Ok(bytes) = std::fs::read(&path)
            && ART_GENERATION.load(std::sync::atomic::Ordering::Relaxed) == art_gen
        {
            if let Ok(mut pending) = PENDING_ART.lock() {
                *pending = Some((path, bytes));
            }
            crate::notch::wake();
            unsafe {
                dispatch_async_f(
                    &_dispatch_main_q as *const _ as *const u8,
                    std::ptr::null_mut(),
                    on_artwork_loaded,
                );
            }
        }
    });
}

fn schedule_hover_timer(delay_ms: u64, action: HoverTimerAction) {
    let token = HOVER_TIMER_TOKEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let when = unsafe { dispatch_time(0, (delay_ms as i64) * 1_000_000) };
    let payload = (token << 2) | (action as u64 & 0x3);
    unsafe {
        dispatch_after_f(
            when,
            &_dispatch_main_q as *const _ as *const u8,
            payload as usize as *mut std::ffi::c_void,
            on_hover_timer_fired,
        );
    }
}

fn cancel_hover_timers() {
    HOVER_TIMER_TOKEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

extern "C" fn on_hover_timer_fired(context: *mut std::ffi::c_void) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let payload = context as usize as u64;
        let action_id = payload & 0x3;
        let token = payload >> 2;
        if HOVER_TIMER_TOKEN.load(std::sync::atomic::Ordering::SeqCst) != token {
            return;
        }
        let Ok(mut lock) = CONTROLLER.lock() else {
            return;
        };
        let Some(ctrl) = lock.as_mut() else {
            return;
        };
        match action_id {
            1 => perform_expand_locked(ctrl),
            2 => perform_collapse_locked(ctrl),
            3 => complete_collapse_locked(ctrl),
            _ => {}
        }
    }));
}

fn sf_symbol(
    name: &str,
    point_size: f64,
    weight: f64,
    accessibility_description: Option<&str>,
) -> Option<Retained<NSImage>> {
    let name_str = NSString::from_str(name);
    let desc_ns = accessibility_description.map(NSString::from_str);
    unsafe {
        let img_cls = objc2::runtime::AnyClass::get(c"NSImage")?;
        let base_img: Option<Retained<NSImage>> = objc2::msg_send![
            img_cls,
            imageWithSystemSymbolName: &*name_str,
            accessibilityDescription: desc_ns.as_deref()
        ];
        let img = base_img?;
        if let Some(cfg_cls) = objc2::runtime::AnyClass::get(c"NSImageSymbolConfiguration") {
            let config: Option<Retained<NSObject>> = objc2::msg_send![
                cfg_cls,
                configurationWithPointSize: point_size,
                weight: weight
            ];
            if let Some(cfg) = config {
                let sized_img: Option<Retained<NSImage>> = objc2::msg_send![
                    &*img,
                    imageWithSymbolConfiguration: &*cfg
                ];
                return sized_img.or(Some(img));
            }
        }
        Some(img)
    }
}

fn set_button_accessibility_label(button: &NSButton, label: &str) {
    let s = NSString::from_str(label);
    let () = unsafe { objc2::msg_send![button, setAccessibilityLabel: &*s] };
}

fn set_button_tint(button: &NSButton, tint: &NSColor) {
    unsafe {
        let () = objc2::msg_send![button, setContentTintColor: tint];
    }
}

fn attach_button_action(
    button: &NSButton,
    target: &FastpotifyNotchActionHandler,
    action: objc2::runtime::Sel,
    tint: &NSColor,
) {
    set_button_tint(button, tint);
    unsafe {
        button.setTarget(Some(target));
        button.setAction(Some(action));
    }
}

fn setup_icon_button(
    button: &NSButton,
    symbol: &str,
    point_size: f64,
    weight: f64,
    fallback: &str,
    font: &NSFont,
    accessibility: &str,
) {
    button.setBordered(false);
    if let Some(img) = sf_symbol(symbol, point_size, weight, Some(accessibility)) {
        button.setImage(Some(&img));
        button.setTitle(&NSString::from_str(""));
    } else {
        button.setImage(None);
        button.setTitle(&NSString::from_str(fallback));
        button.setFont(Some(font));
    }
    set_button_accessibility_label(button, accessibility);
}

fn update_play_button_ui(button: &NSButton, playing: bool) {
    let (symbol, fallback, label) = if playing {
        ("pause.fill", "\u{275A}\u{275A}", "Pause")
    } else {
        ("play.fill", "\u{25B6}", "Play")
    };
    setup_icon_button(
        button,
        symbol,
        15.0,
        0.5,
        fallback,
        &NSFont::boldSystemFontOfSize(15.0),
        label,
    );
    set_button_tint(
        button,
        &NSColor::colorWithRed_green_blue_alpha(0.06, 0.07, 0.08, 1.0),
    );
}

fn update_like_button_ui(button: &NSButton, saved: bool, accent: Option<[u8; 3]>) {
    let (symbol, fallback, label) = if saved {
        ("heart.fill", "\u{2665}", "Remove from Liked Songs")
    } else {
        ("heart", "\u{2661}", "Save to Liked Songs")
    };
    setup_icon_button(
        button,
        symbol,
        18.0,
        0.2,
        fallback,
        &NSFont::systemFontOfSize(20.0),
        label,
    );
    let tint = if saved {
        let (r, g, b) = contrast_accent(accent.unwrap_or([30, 215, 96]));
        NSColor::colorWithRed_green_blue_alpha(r, g, b, 1.0)
    } else {
        NSColor::colorWithWhite_alpha(1.0, 0.65)
    };
    set_button_tint(button, &tint);
}

fn update_device_button_ui(button: &NSButton, is_remote: bool, accent: Option<[u8; 3]>) {
    let symbol = if is_remote {
        "hifispeaker.fill"
    } else {
        "hifispeaker"
    };
    setup_icon_button(
        button,
        symbol,
        17.0,
        0.2,
        "🔊",
        &NSFont::systemFontOfSize(17.0),
        "Connect to a device",
    );
    let tint = if is_remote {
        let (r, g, b) = contrast_accent(accent.unwrap_or([30, 215, 96]));
        NSColor::colorWithRed_green_blue_alpha(r, g, b, 1.0)
    } else {
        NSColor::colorWithWhite_alpha(1.0, 0.65)
    };
    set_button_tint(button, &tint);
}

fn update_shuffle_button_ui(button: &NSButton, shuffle: bool, accent: Option<[u8; 3]>) {
    setup_icon_button(
        button,
        "shuffle",
        16.0,
        0.2,
        "🔀",
        &NSFont::systemFontOfSize(16.0),
        "Shuffle",
    );
    let tint = if shuffle {
        let (r, g, b) = contrast_accent(accent.unwrap_or([30, 215, 96]));
        NSColor::colorWithRed_green_blue_alpha(r, g, b, 1.0)
    } else {
        NSColor::colorWithWhite_alpha(1.0, 0.65)
    };
    set_button_tint(button, &tint);
}

fn update_repeat_button_ui(
    button: &NSButton,
    repeat: crate::player::RepeatMode,
    accent: Option<[u8; 3]>,
) {
    let (symbol, fallback, label) = match repeat {
        crate::player::RepeatMode::Off => ("repeat", "🔁", "Repeat"),
        crate::player::RepeatMode::Context => ("repeat", "🔁", "Repeat one"),
        crate::player::RepeatMode::Track => ("repeat.1", "🔂", "Repeat off"),
    };
    setup_icon_button(
        button,
        symbol,
        16.0,
        0.2,
        fallback,
        &NSFont::systemFontOfSize(16.0),
        label,
    );
    let tint = if repeat == crate::player::RepeatMode::Off {
        NSColor::colorWithWhite_alpha(1.0, 0.65)
    } else {
        let (r, g, b) = contrast_accent(accent.unwrap_or([30, 215, 96]));
        NSColor::colorWithRed_green_blue_alpha(r, g, b, 1.0)
    };
    set_button_tint(button, &tint);
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FastpotifyNotchActionHandler"]
    pub struct FastpotifyNotchActionHandler;

    impl FastpotifyNotchActionHandler {
        #[unsafe(method(onPlayPause:))]
        fn on_play_pause(&self, _sender: &NSObject) {
            if let Ok(mut lock) = CONTROLLER.lock()
                && let Some(ctrl) = lock.as_mut()
                && let Some(track) = ctrl.track.as_mut()
            {
                track.playing = !track.playing;
                let is_playing = track.playing;
                update_play_button_ui(&ctrl.play_button, is_playing);
                ctrl.canvas_view.setNeedsDisplay(true);
            }
            push_command(NotchCommand::PlayPause);
        }

        #[unsafe(method(onNext:))]
        fn on_next(&self, _sender: &NSObject) {
            push_command(NotchCommand::Next);
        }

        #[unsafe(method(onPrev:))]
        fn on_prev(&self, _sender: &NSObject) {
            push_command(NotchCommand::Previous);
        }

        #[unsafe(method(onShuffle:))]
        fn on_shuffle(&self, _sender: &NSObject) {
            if let Ok(mut lock) = CONTROLLER.lock()
                && let Some(ctrl) = lock.as_mut()
                && let Some(track) = ctrl.track.as_mut()
            {
                track.shuffle = !track.shuffle;
                let is_shuffle = track.shuffle;
                let accent = track.accent;
                update_shuffle_button_ui(&ctrl.shuffle_button, is_shuffle, accent);
            }
            push_command(NotchCommand::ToggleShuffle);
        }

        #[unsafe(method(onRepeat:))]
        fn on_repeat(&self, _sender: &NSObject) {
            if let Ok(mut lock) = CONTROLLER.lock()
                && let Some(ctrl) = lock.as_mut()
                && let Some(track) = ctrl.track.as_mut()
            {
                track.repeat = track.repeat.next();
                let repeat_mode = track.repeat;
                let accent = track.accent;
                update_repeat_button_ui(&ctrl.repeat_button, repeat_mode, accent);
            }
            push_command(NotchCommand::CycleRepeat);
        }

        #[unsafe(method(onLike:))]
        fn on_like(&self, _sender: &NSObject) {
            if let Ok(mut lock) = CONTROLLER.lock()
                && let Some(ctrl) = lock.as_mut()
                && let Some(track) = ctrl.track.as_mut()
                && can_toggle_saved(track.is_episode)
            {
                track.saved = !track.saved;
                let is_saved = track.saved;
                let accent = track.accent;
                let uri = track.uri.clone();
                update_like_button_ui(&ctrl.like_button, is_saved, accent);
                push_command(NotchCommand::ToggleSaved(uri));
            }
        }

        #[unsafe(method(onDevice:))]
        fn on_device(&self, _sender: &NSObject) {
            push_command(NotchCommand::ShowWindow);
        }
    }
);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "FastpotifyNotchView"]
    pub struct FastpotifyNotchView;

    impl FastpotifyNotchView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            handle_mouse_entered();
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            handle_mouse_exited();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            push_command(NotchCommand::ShowWindow);
        }
    }
);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "FastpotifyCardView"]
    pub struct FastpotifyCardView;

    impl FastpotifyCardView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            push_command(NotchCommand::ShowWindow);
        }
    }
);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "FastpotifyCanvasView"]
    pub struct FastpotifyCanvasView;

    impl FastpotifyCanvasView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            handle_canvas_mouse_down(self, event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            handle_canvas_mouse_dragged(self, event);
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty_rect: NSRect) {
            handle_draw_canvas(self, dirty_rect);
        }
    }
);

struct NotchFrames {
    collapsed_window: NSRect,
    expanded_window: NSRect,
    collapsed_card: NSRect,
    expanded_card: NSRect,
}

struct NotchController {
    window: Retained<NSWindow>,
    _view: Retained<FastpotifyNotchView>,
    card_view: Retained<FastpotifyCardView>,
    visual_effect: Retained<NSVisualEffectView>,
    canvas_view: Retained<FastpotifyCanvasView>,
    title_field: Retained<NSTextField>,
    artist_field: Retained<NSTextField>,
    elapsed_field: Retained<NSTextField>,
    remaining_field: Retained<NSTextField>,
    art_view: Retained<NSImageView>,
    like_button: Retained<NSButton>,
    shuffle_button: Retained<NSButton>,
    prev_button: Retained<NSButton>,
    play_button: Retained<NSButton>,
    next_button: Retained<NSButton>,
    repeat_button: Retained<NSButton>,
    device_button: Retained<NSButton>,
    _action_handler: Retained<FastpotifyNotchActionHandler>,
    collapsed_frame: NSRect,
    expanded_frame: NSRect,
    collapsed_card_frame: NSRect,
    expanded_card_frame: NSRect,
    expanded: bool,
    enabled: bool,
    is_minimized_or_background: bool,
    track: Option<NotchTrackInfo>,
    current_art_path: Option<PathBuf>,
    pending_expand_at: Option<Instant>,
    pending_collapse_at: Option<Instant>,
    collapsing_until: Option<Instant>,
    /// When set, the card pulses / cross-fades to signal a track change.
    /// Cleared once the animation finishes.
    track_flash_until: Option<Instant>,
    pending_seek: Option<(u32, Instant)>,
}

unsafe impl Send for NotchController {}

fn contrast_accent(color: [u8; 3]) -> (f64, f64, f64) {
    let mut r = color[0] as f64 / 255.0;
    let mut g = color[1] as f64 / 255.0;
    let mut b = color[2] as f64 / 255.0;
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if lum < 0.08 {
        // Fallback for near-black album artwork to vibrant Spotify green
        return (0.118, 0.843, 0.376);
    }
    if lum < 0.42 {
        let factor = 0.42 / lum;
        r = (r * factor).min(1.0);
        g = (g * factor).min(1.0);
        b = (b * factor).min(1.0);
    }
    (r, g, b)
}

fn set_view_corner_radius(view: &NSView, radius: f64) {
    unsafe {
        let layer: Option<Retained<NSObject>> = objc2::msg_send![view, layer];
        if let Some(layer) = layer {
            let () = objc2::msg_send![&*layer, setCornerRadius: radius];
            let () = objc2::msg_send![&*layer, setMasksToBounds: true];
        }
    }
}

fn configure_single_line_label(field: &NSTextField) {
    unsafe {
        let () = objc2::msg_send![field, setMaximumNumberOfLines: 1isize];
        let () = objc2::msg_send![field, setUsesSingleLineMode: true];
        let cell: Option<Retained<NSObject>> = objc2::msg_send![field, cell];
        if let Some(cell) = cell {
            let () = objc2::msg_send![&*cell, setLineBreakMode: 4usize];
        }
    }
}

fn reset_track_fade(ctrl: &mut NotchController) {
    ctrl.track_flash_until = None;
    ctrl.title_field.setAlphaValue(1.0);
    ctrl.artist_field.setAlphaValue(1.0);
    ctrl.art_view.setAlphaValue(1.0);
}

fn update_artwork_path(ctrl: &mut NotchController, art_path: Option<PathBuf>) {
    if ctrl.current_art_path != art_path {
        ctrl.current_art_path = art_path.clone();
        ctrl.art_view.setImage(None);
        if let Some(path) = art_path {
            spawn_art_loader(path);
        }
    }
}

fn screen_top_inset(screen: &NSScreen) -> f64 {
    // safeAreaInsets was introduced in macOS 12.0 (Monterey).
    // Spotifast supports macOS 11.0+ (Big Sur), where calling the selector directly
    // would raise an unrecognized-selector exception. Notched MacBooks did not
    // exist before macOS 12, so a zero-inset fallback on macOS 11 is strictly correct.
    unsafe {
        let responds: bool = objc2::msg_send![screen, respondsToSelector: sel!(safeAreaInsets)];
        if !responds {
            return 0.0;
        }
    }
    screen.safeAreaInsets().top
}

fn compute_frames(mtm: MainThreadMarker) -> Option<NotchFrames> {
    let screens = NSScreen::screens(mtm);
    let (screen, top_inset) = screens.iter().find_map(|s| {
        let inset = screen_top_inset(&s);
        if inset > 0.0 { Some((s, inset)) } else { None }
    })?;
    let screen_frame = screen.frame();
    let dims = crate::notch::compute_notch_geometry(
        screen_frame.origin.x,
        screen_frame.origin.y,
        screen_frame.size.width,
        screen_frame.size.height,
        top_inset,
    )?;

    let collapsed_window = NSRect::new(
        NSPoint::new(dims.collapsed_x, dims.collapsed_y),
        NSSize::new(dims.collapsed_w, dims.collapsed_h),
    );
    let expanded_window = NSRect::new(
        NSPoint::new(dims.expanded_x, dims.expanded_y),
        NSSize::new(dims.expanded_w, dims.expanded_h),
    );
    let collapsed_card = NSRect::new(NSPoint::new(0.0, 6.0), NSSize::new(dims.expanded_w, 148.0));
    let expanded_card = NSRect::new(
        NSPoint::new(0.0, dims.collapsed_h + 8.0),
        NSSize::new(dims.expanded_w, 148.0),
    );

    Some(NotchFrames {
        collapsed_window,
        expanded_window,
        collapsed_card,
        expanded_card,
    })
}

fn update_time_labels(
    canvas_view: &FastpotifyCanvasView,
    elapsed_field: &NSTextField,
    remaining_field: &NSTextField,
    position_ms: u32,
    duration_ms: u32,
) {
    let elapsed = crate::util::format_duration_ms(position_ms);
    let remaining_ms = duration_ms.saturating_sub(position_ms);
    let remaining = format!("-{}", crate::util::format_duration_ms(remaining_ms));
    elapsed_field.setStringValue(&NSString::from_str(&elapsed));
    remaining_field.setStringValue(&NSString::from_str(&remaining));
    let val = NSString::from_str(&format!(
        "{elapsed} of {}",
        crate::util::format_duration_ms(duration_ms)
    ));
    unsafe {
        let () = objc2::msg_send![canvas_view, setAccessibilityValue: &*val];
    }
}

fn layout_card_subviews(ctrl: &NotchController, card_w: f64) {
    let card_bounds = NSRect::new(NSPoint::ZERO, NSSize::new(card_w, 148.0));
    ctrl.visual_effect.setFrame(card_bounds);
    ctrl.canvas_view.setFrame(card_bounds);

    let text_w = (card_w - 76.0 - 54.0).max(60.0);
    ctrl.title_field.setFrame(NSRect::new(
        NSPoint::new(76.0, 16.0),
        NSSize::new(text_w, 20.0),
    ));
    ctrl.artist_field.setFrame(NSRect::new(
        NSPoint::new(76.0, 38.0),
        NSSize::new(text_w, 18.0),
    ));

    let remaining_x = (card_w - 58.0).max(120.0);
    ctrl.remaining_field.setFrame(NSRect::new(
        NSPoint::new(remaining_x, 74.0),
        NSSize::new(44.0, 16.0),
    ));

    let device_x = (card_w - 46.0).max(120.0);
    ctrl.device_button.setFrame(NSRect::new(
        NSPoint::new(device_x, 97.0),
        NSSize::new(30.0, 36.0),
    ));

    ctrl.like_button.setFrame(NSRect::new(
        NSPoint::new(16.0, 97.0),
        NSSize::new(30.0, 36.0),
    ));

    let center_x = card_w / 2.0;
    ctrl.shuffle_button.setFrame(NSRect::new(
        NSPoint::new(center_x - 108.0, 97.0),
        NSSize::new(30.0, 36.0),
    ));
    ctrl.prev_button.setFrame(NSRect::new(
        NSPoint::new(center_x - 62.0, 97.0),
        NSSize::new(30.0, 36.0),
    ));
    ctrl.play_button.setFrame(NSRect::new(
        NSPoint::new(center_x - 18.0, 97.0),
        NSSize::new(36.0, 36.0),
    ));
    ctrl.next_button.setFrame(NSRect::new(
        NSPoint::new(center_x + 32.0, 97.0),
        NSSize::new(30.0, 36.0),
    ));
    ctrl.repeat_button.setFrame(NSRect::new(
        NSPoint::new(center_x + 78.0, 97.0),
        NSSize::new(30.0, 36.0),
    ));
}

fn update_geometry_with_frames(ctrl: &mut NotchController, frames: &NotchFrames) {
    let changed = (ctrl.collapsed_frame.origin.x - frames.collapsed_window.origin.x).abs() > 0.5
        || (ctrl.collapsed_frame.origin.y - frames.collapsed_window.origin.y).abs() > 0.5
        || (ctrl.collapsed_frame.size.width - frames.collapsed_window.size.width).abs() > 0.5
        || (ctrl.collapsed_frame.size.height - frames.collapsed_window.size.height).abs() > 0.5;
    if changed {
        ctrl.collapsed_frame = frames.collapsed_window;
        ctrl.expanded_frame = frames.expanded_window;
        ctrl.collapsed_card_frame = frames.collapsed_card;
        ctrl.expanded_card_frame = frames.expanded_card;
        let (win_frame, card_frame) = if ctrl.expanded {
            (ctrl.expanded_frame, ctrl.expanded_card_frame)
        } else {
            (ctrl.collapsed_frame, ctrl.collapsed_card_frame)
        };
        ctrl.window.setFrame_display(win_frame, false);
        ctrl.card_view.setFrame(card_frame);
        layout_card_subviews(ctrl, frames.expanded_card.size.width);
    }
}

pub fn init() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if let Ok(lock) = CONTROLLER.lock()
        && lock.is_some()
    {
        return;
    }
    let Some(frames) = compute_frames(mtm) else {
        return;
    };

    let window = unsafe {
        let win = NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            frames.collapsed_window,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        );
        win.setReleasedWhenClosed(false);
        win
    };

    window.setLevel(25); // Status window level above menus
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(true);
    window.setIgnoresMouseEvents(false);
    window.setAcceptsMouseMovedEvents(true);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );

    let view: Retained<FastpotifyNotchView> = unsafe {
        let view_frame = NSRect::new(NSPoint::ZERO, frames.collapsed_window.size);
        objc2::msg_send![mtm.alloc::<FastpotifyNotchView>(), initWithFrame: view_frame]
    };

    let options = NSTrackingAreaOptions::MouseEnteredAndExited
        | NSTrackingAreaOptions::ActiveAlways
        | NSTrackingAreaOptions::InVisibleRect;
    let tracking_area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            mtm.alloc(),
            NSRect::ZERO,
            options,
            Some(&view),
            None,
        )
    };
    view.addTrackingArea(&tracking_area);
    window.setContentView(Some(&view));

    let action_handler: Retained<FastpotifyNotchActionHandler> =
        unsafe { objc2::msg_send![mtm.alloc::<FastpotifyNotchActionHandler>(), init] };

    let card_w = frames.expanded_card.size.width;
    let card_h = 148.0;
    let card_bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(card_w, card_h));

    // Outer container view for the card
    let card_view: Retained<FastpotifyCardView> = unsafe {
        objc2::msg_send![mtm.alloc::<FastpotifyCardView>(), initWithFrame: frames.collapsed_card]
    };
    card_view.setWantsLayer(true);
    card_view.setAlphaValue(0.0); // Starts hidden with smooth fade-in
    set_view_corner_radius(&card_view, 18.0);
    view.addSubview(&card_view);

    // 1. Native dark frosted glass vibrancy layer (Bottom layer)
    // HUDWindow gives the same dark semi-transparent blur that Spotifast uses for overlays.
    let visual_effect = NSVisualEffectView::initWithFrame(mtm.alloc(), card_bounds);
    visual_effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
    visual_effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    visual_effect.setState(NSVisualEffectState::Active);
    visual_effect.setWantsLayer(true);
    set_view_corner_radius(&visual_effect, 18.0);
    card_view.addSubview(&visual_effect);

    // 2. Custom Canvas View: Drawn on top of visual effect (seek bar, waveform, outline)
    let canvas_view: Retained<FastpotifyCanvasView> = unsafe {
        objc2::msg_send![mtm.alloc::<FastpotifyCanvasView>(), initWithFrame: card_bounds]
    };
    canvas_view.setWantsLayer(true);
    // Mark the canvas with an accessible slider role so VoiceOver announces the progress
    // bar as "Playback Position, slider" and offers arrow-key seek to keyboard users.
    unsafe {
        let role = objc2_foundation::ns_string!("AXSlider");
        let label = objc2_foundation::ns_string!("Playback Position");
        let () = objc2::msg_send![&*canvas_view, setAccessibilityRole: role];
        let () = objc2::msg_send![&*canvas_view, setAccessibilityLabel: label];
        let () = objc2::msg_send![&*canvas_view, setAccessibilityElement: true];
    }
    card_view.addSubview(&canvas_view);

    // 3. Album artwork (Left of card, 50x50 with 8pt rounded corners matching theme::RADIUS)
    let art_view = NSImageView::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(16.0, 14.0), NSSize::new(50.0, 50.0)),
    );
    art_view.setWantsLayer(true);
    set_view_corner_radius(&art_view, 8.0);
    unsafe {
        let () = objc2::msg_send![&art_view, setImageScaling: 0usize];
        let label = objc2_foundation::ns_string!("Album artwork");
        let () = objc2::msg_send![&art_view, setAccessibilityLabel: label];
        let () = objc2::msg_send![&art_view, setAccessibilityElement: true];
    }
    card_view.addSubview(&art_view);

    // 4. Track Title (Row 1, Bold White, Tail Truncation)
    let text_w = (card_w - 76.0 - 54.0).max(60.0);
    let title_field = NSTextField::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(76.0, 16.0), NSSize::new(text_w, 20.0)),
    );
    title_field.setEditable(false);
    title_field.setSelectable(false);
    title_field.setBordered(false);
    title_field.setDrawsBackground(false);
    title_field.setTextColor(Some(&NSColor::whiteColor()));
    title_field.setFont(Some(&NSFont::boldSystemFontOfSize(14.5)));
    configure_single_line_label(&title_field);
    card_view.addSubview(&title_field);

    // 5. Artist Name (Row 1, Secondary Gray, Tail Truncation)
    let artist_field = NSTextField::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(76.0, 38.0), NSSize::new(text_w, 18.0)),
    );
    artist_field.setEditable(false);
    artist_field.setSelectable(false);
    artist_field.setBordered(false);
    artist_field.setDrawsBackground(false);
    artist_field.setTextColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.65)));
    artist_field.setFont(Some(&NSFont::systemFontOfSize(12.5)));
    configure_single_line_label(&artist_field);
    card_view.addSubview(&artist_field);

    // 6. Elapsed and Remaining Time Fields (Row 2, Tabular monospaced digits to eliminate jitter)
    let time_font: Option<Retained<NSFont>> = unsafe {
        let font_cls = objc2::runtime::AnyClass::get(c"NSFont");
        font_cls.and_then(|cls| {
            objc2::msg_send![
                cls,
                monospacedDigitSystemFontOfSize: 11.0f64,
                weight: 0.0f64
            ]
        })
    };
    let default_font = NSFont::systemFontOfSize(11.0);
    let time_font_ref = time_font.as_deref().unwrap_or(&default_font);

    let elapsed_field = NSTextField::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(14.0, 74.0), NSSize::new(42.0, 16.0)),
    );
    elapsed_field.setEditable(false);
    elapsed_field.setSelectable(false);
    elapsed_field.setBordered(false);
    elapsed_field.setDrawsBackground(false);
    elapsed_field.setTextColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.60)));
    elapsed_field.setFont(Some(time_font_ref));
    elapsed_field.setStringValue(&NSString::from_str("0:00"));
    card_view.addSubview(&elapsed_field);

    // 7. Remaining Time (Row 2, Right of progress bar)
    let remaining_x = (card_w - 58.0).max(120.0);
    let remaining_field = NSTextField::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(remaining_x, 74.0), NSSize::new(44.0, 16.0)),
    );
    remaining_field.setEditable(false);
    remaining_field.setSelectable(false);
    remaining_field.setBordered(false);
    remaining_field.setDrawsBackground(false);
    remaining_field.setTextColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.60)));
    remaining_field.setFont(Some(time_font_ref));
    let () = unsafe { objc2::msg_send![&remaining_field, setAlignment: 1isize] }; // Right alignment
    remaining_field.setStringValue(&NSString::from_str("-0:00"));
    card_view.addSubview(&remaining_field);

    // 8. Like / Save Button (Row 3, Leftmost)
    let like_button = NSButton::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(16.0, 97.0), NSSize::new(30.0, 36.0)),
    );
    update_like_button_ui(&like_button, false, None);
    attach_button_action(
        &like_button,
        &action_handler,
        sel!(onLike:),
        &NSColor::colorWithWhite_alpha(1.0, 0.65),
    );
    card_view.addSubview(&like_button);

    let center_x = card_w / 2.0;

    // 9. Shuffle Button (Row 3, Center group)
    let shuffle_button = NSButton::initWithFrame(
        mtm.alloc(),
        NSRect::new(
            NSPoint::new(center_x - 108.0, 97.0),
            NSSize::new(30.0, 36.0),
        ),
    );
    update_shuffle_button_ui(&shuffle_button, false, None);
    attach_button_action(
        &shuffle_button,
        &action_handler,
        sel!(onShuffle:),
        &NSColor::colorWithWhite_alpha(1.0, 0.65),
    );
    card_view.addSubview(&shuffle_button);

    // 10. Previous Button (Row 3, Center group)
    let prev_button = NSButton::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(center_x - 62.0, 97.0), NSSize::new(30.0, 36.0)),
    );
    setup_icon_button(
        &prev_button,
        "backward.end.fill",
        18.0,
        0.3,
        "\u{23EE}",
        &NSFont::boldSystemFontOfSize(18.0),
        "Previous",
    );
    attach_button_action(
        &prev_button,
        &action_handler,
        sel!(onPrev:),
        &NSColor::colorWithWhite_alpha(1.0, 0.65),
    );
    card_view.addSubview(&prev_button);

    // 11. Play / Pause Button (Row 3, Center)
    let play_button = NSButton::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(center_x - 18.0, 97.0), NSSize::new(36.0, 36.0)),
    );
    update_play_button_ui(&play_button, false);
    attach_button_action(
        &play_button,
        &action_handler,
        sel!(onPlayPause:),
        &NSColor::colorWithRed_green_blue_alpha(0.06, 0.07, 0.08, 1.0),
    );
    card_view.addSubview(&play_button);

    // 12. Next Button (Row 3, Center group)
    let next_button = NSButton::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(center_x + 32.0, 97.0), NSSize::new(30.0, 36.0)),
    );
    setup_icon_button(
        &next_button,
        "forward.end.fill",
        18.0,
        0.3,
        "\u{23ED}",
        &NSFont::boldSystemFontOfSize(18.0),
        "Next",
    );
    attach_button_action(
        &next_button,
        &action_handler,
        sel!(onNext:),
        &NSColor::colorWithWhite_alpha(1.0, 0.65),
    );
    card_view.addSubview(&next_button);

    // 13. Repeat Button (Row 3, Center group)
    let repeat_button = NSButton::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(center_x + 78.0, 97.0), NSSize::new(30.0, 36.0)),
    );
    update_repeat_button_ui(&repeat_button, crate::player::RepeatMode::Off, None);
    attach_button_action(
        &repeat_button,
        &action_handler,
        sel!(onRepeat:),
        &NSColor::colorWithWhite_alpha(1.0, 0.65),
    );
    card_view.addSubview(&repeat_button);

    // 14. Device / Connect Button (Row 3, Rightmost)
    let device_x = (card_w - 46.0).max(120.0);
    let device_button = NSButton::initWithFrame(
        mtm.alloc(),
        NSRect::new(NSPoint::new(device_x, 97.0), NSSize::new(30.0, 36.0)),
    );
    update_device_button_ui(&device_button, false, None);
    attach_button_action(
        &device_button,
        &action_handler,
        sel!(onDevice:),
        &NSColor::colorWithWhite_alpha(1.0, 0.65),
    );
    card_view.addSubview(&device_button);

    let ctrl = NotchController {
        window,
        _view: view,
        card_view,
        visual_effect,
        canvas_view,
        title_field,
        artist_field,
        elapsed_field,
        remaining_field,
        art_view,
        like_button,
        shuffle_button,
        prev_button,
        play_button,
        next_button,
        repeat_button,
        device_button,
        _action_handler: action_handler,
        collapsed_frame: frames.collapsed_window,
        expanded_frame: frames.expanded_window,
        collapsed_card_frame: frames.collapsed_card,
        expanded_card_frame: frames.expanded_card,
        expanded: false,
        enabled: true,
        is_minimized_or_background: false,
        track: None,
        current_art_path: None,
        pending_expand_at: None,
        pending_collapse_at: None,
        collapsing_until: None,
        track_flash_until: None,
        pending_seek: None,
    };

    if let Ok(mut lock) = CONTROLLER.lock() {
        *lock = Some(ctrl);
    }
}

fn is_in_foreground(ctrl: &NotchController) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return !ctrl.is_minimized_or_background;
    };
    let app = NSApplication::sharedApplication(mtm);
    if !app.isActive() {
        return false;
    }
    let windows = app.windows();
    let has_visible_unminimized = windows.iter().any(|w| unsafe {
        let is_self: bool = objc2::msg_send![&*w, isEqual: &*ctrl.window];
        if is_self {
            return false;
        }
        let is_min: bool = objc2::msg_send![&*w, isMiniaturized];
        let is_vis: bool = objc2::msg_send![&*w, isVisible];
        !is_min && is_vis
    });
    has_visible_unminimized && !ctrl.is_minimized_or_background
}

fn perform_expand_locked(ctrl: &mut NotchController) {
    ctrl.pending_expand_at = None;
    let in_foreground = is_in_foreground(ctrl);
    let should_expand = crate::notch::should_perform_expand(
        ctrl.enabled,
        in_foreground,
        ctrl.track.is_some(),
        ctrl.expanded,
    );
    if should_expand {
        ctrl.expanded = true;
        ctrl.collapsing_until = None;

        // Expand window frame immediately without animation
        ctrl.window.setFrame_display(ctrl.expanded_frame, false);

        // Snappy, smooth Apple iOS Dynamic Island style vertical drop animation (260ms duration)
        NSAnimationContext::beginGrouping();
        let ctx = NSAnimationContext::currentContext();
        ctx.setDuration(0.26);
        ctx.setAllowsImplicitAnimation(true);

        ctrl.card_view.animator().setFrame(ctrl.expanded_card_frame);
        ctrl.card_view.animator().setAlphaValue(1.0);

        NSAnimationContext::endGrouping();

        ctrl.canvas_view.setNeedsDisplay(true);
        ctrl.window.invalidateShadow();
    }
}

fn perform_collapse_locked(ctrl: &mut NotchController) {
    ctrl.pending_collapse_at = None;
    if crate::notch::should_perform_collapse(ctrl.expanded) {
        ctrl.expanded = false;
        ctrl.collapsing_until = Some(Instant::now() + std::time::Duration::from_millis(200));

        NSAnimationContext::beginGrouping();
        let ctx = NSAnimationContext::currentContext();
        ctx.setDuration(0.20);
        ctx.setAllowsImplicitAnimation(true);

        ctrl.card_view
            .animator()
            .setFrame(ctrl.collapsed_card_frame);
        ctrl.card_view.animator().setAlphaValue(0.0);

        NSAnimationContext::endGrouping();

        schedule_hover_timer(200, HoverTimerAction::CompleteCollapse);
    }
}

fn complete_collapse_locked(ctrl: &mut NotchController) {
    ctrl.collapsing_until = None;
    ctrl.card_view.setAlphaValue(0.0);
    ctrl.card_view.setFrame(ctrl.collapsed_card_frame);
    ctrl.window.setFrame_display(ctrl.collapsed_frame, false);
}

fn handle_mouse_entered() {
    let Ok(mut lock) = CONTROLLER.lock() else {
        return;
    };
    let Some(ctrl) = lock.as_mut() else {
        return;
    };

    ctrl.pending_collapse_at = None;
    ctrl.collapsing_until = None;
    cancel_hover_timers();

    let in_foreground = is_in_foreground(ctrl);
    let should_expand = crate::notch::should_schedule_expand(
        ctrl.enabled,
        in_foreground,
        ctrl.track.is_some(),
        ctrl.expanded,
        ctrl.pending_expand_at.is_some(),
    );
    if should_expand {
        // Snappy dwell delay of 60ms: filters accidental cursor flicks while feeling immediate and responsive.
        // Scheduled directly on the macOS run loop so minimized windows don't block expansion.
        ctrl.pending_expand_at = Some(Instant::now() + std::time::Duration::from_millis(60));
        schedule_hover_timer(60, HoverTimerAction::Expand);
        crate::notch::wake();
    }
}

fn handle_mouse_exited() {
    let Ok(mut lock) = CONTROLLER.lock() else {
        return;
    };
    let Some(ctrl) = lock.as_mut() else {
        return;
    };

    ctrl.pending_expand_at = None; // Cancel pending expand if mouse left before dwell time
    cancel_hover_timers();
    if crate::notch::should_schedule_collapse(ctrl.expanded) {
        // Crisp linger delay of 400ms before starting popdown.
        // Scheduled directly on the macOS run loop so minimized windows don't block collapse.
        ctrl.pending_collapse_at = Some(Instant::now() + std::time::Duration::from_millis(400));
        schedule_hover_timer(400, HoverTimerAction::Collapse);
        crate::notch::wake();
    }
}

fn handle_canvas_mouse_down(view: &FastpotifyCanvasView, event: &NSEvent) {
    let location = event.locationInWindow();
    let local: NSPoint =
        unsafe { objc2::msg_send![view, convertPoint: location, fromView: None::<&NSView>] };

    if let Ok(mut lock) = CONTROLLER.lock()
        && let Some(ctrl) = lock.as_mut()
    {
        let card_w = ctrl.expanded_card_frame.size.width;
        let start_x = 60.0f64;
        let end_x = (card_w - 58.0).max(start_x + 20.0);
        let track_w = (end_x - start_x).max(1.0);

        // Check if clicked directly on the progress bar track
        if local.y >= 70.0
            && local.y <= 94.0
            && local.x >= (start_x - 4.0)
            && local.x <= (end_x + 4.0)
        {
            let ratio = ((local.x - start_x) / track_w).clamp(0.0, 1.0);
            if let Some(track) = ctrl.track.as_mut() {
                if track.duration_ms == 0 {
                    return;
                }
                let seek_pos = (ratio * track.duration_ms as f64) as u32;
                track.position_ms = seek_pos;
                ctrl.pending_seek = Some((seek_pos, Instant::now()));
                update_time_labels(
                    &ctrl.canvas_view,
                    &ctrl.elapsed_field,
                    &ctrl.remaining_field,
                    seek_pos,
                    track.duration_ms,
                );
                ctrl.canvas_view.setNeedsDisplay(true);
                push_command(NotchCommand::Seek(seek_pos));
                return;
            }
        }
    }

    // Otherwise clicking anywhere on the card brings Spotifast to front
    push_command(NotchCommand::ShowWindow);
}

fn handle_canvas_mouse_dragged(view: &FastpotifyCanvasView, event: &NSEvent) {
    let location = event.locationInWindow();
    let local: NSPoint =
        unsafe { objc2::msg_send![view, convertPoint: location, fromView: None::<&NSView>] };

    if let Ok(mut lock) = CONTROLLER.lock()
        && let Some(ctrl) = lock.as_mut()
    {
        let card_w = ctrl.expanded_card_frame.size.width;
        let start_x = 60.0f64;
        let end_x = (card_w - 58.0).max(start_x + 20.0);
        let track_w = (end_x - start_x).max(1.0);

        if local.y >= 68.0 && local.y <= 96.0 {
            let ratio = ((local.x - start_x) / track_w).clamp(0.0, 1.0);
            if let Some(track) = ctrl.track.as_mut() {
                if track.duration_ms == 0 {
                    return;
                }
                let seek_pos = (ratio * track.duration_ms as f64) as u32;
                track.position_ms = seek_pos;
                ctrl.pending_seek = Some((seek_pos, Instant::now()));
                update_time_labels(
                    &ctrl.canvas_view,
                    &ctrl.elapsed_field,
                    &ctrl.remaining_field,
                    seek_pos,
                    track.duration_ms,
                );
                ctrl.canvas_view.setNeedsDisplay(true);
                push_command(NotchCommand::Seek(seek_pos));
            }
        }
    }
}

fn handle_draw_canvas(_view: &FastpotifyCanvasView, _dirty: NSRect) {
    let Ok(lock) = CONTROLLER.lock() else {
        return;
    };
    let Some(ctrl) = lock.as_ref() else {
        return;
    };

    let card_w = ctrl.expanded_card_frame.size.width;
    let card_h = 148.0f64;
    let card_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(card_w, card_h));

    // 1. Panel background: Subtle dark glass fill on top of HUDWindow blur.
    //    Matches Spotifast's dark panel palette.panel (0x15, 0x18, 0x1c).
    let base_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(card_rect, 18.0, 18.0);
    NSColor::colorWithRed_green_blue_alpha(0.08, 0.09, 0.11, 0.78).set();
    base_path.fill();

    // 2. 1px crisp outline: Matches Spotifast's palette.outline (0x2a, 0x30, 0x38).
    let border_rect = NSRect::new(
        NSPoint::new(0.5, 0.5),
        NSSize::new(card_w - 1.0, card_h - 1.0),
    );
    let border_path =
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(border_rect, 18.0, 18.0);
    NSColor::colorWithRed_green_blue_alpha(0.16, 0.19, 0.22, 0.75).set();
    unsafe {
        let () = objc2::msg_send![&border_path, setLineWidth: 1.0f64];
    }
    border_path.stroke();

    // 3. Album artwork placeholder if absent or still loading in background
    let has_art = unsafe {
        let img: Option<Retained<NSImage>> = objc2::msg_send![&*ctrl.art_view, image];
        img.is_some()
    };
    if !has_art {
        let art_rect = NSRect::new(NSPoint::new(16.0, 14.0), NSSize::new(50.0, 50.0));
        let art_bg = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(art_rect, 8.0, 8.0);
        NSColor::colorWithWhite_alpha(0.18, 0.9).set();
        art_bg.fill();

        let accent_rect = NSRect::new(NSPoint::new(35.0, 33.0), NSSize::new(12.0, 12.0));
        let accent = NSBezierPath::bezierPathWithOvalInRect(accent_rect);
        NSColor::colorWithWhite_alpha(0.38, 0.9).set();
        accent.fill();
    }

    // 4. Animated equalizer / waveform indicator (Row 1, Right)
    let wave_x = (card_w - 44.0).max(120.0);
    let wave_y = 22.0;
    let wave_h = 16.0;
    let playing = ctrl.track.as_ref().is_some_and(|t| t.playing);

    let mut bar_heights = [3.0; 5];
    if playing
        && let Some(levels) = ctrl.track.as_ref().map(|t| t.levels)
        && levels.iter().any(|&v| v > 0.01)
    {
        for (i, &v) in levels.iter().enumerate() {
            bar_heights[i] = 3.0 + 12.0 * (v as f64).clamp(0.0, 1.0);
        }
    }

    // Album art accent color with luminance-boosted contrast
    let accent_rgb = ctrl
        .track
        .as_ref()
        .and_then(|t| t.accent)
        .unwrap_or([30, 215, 96]); // Spotify green default (0x1e, 0xd7, 0x60)
    let (ar, ag, ab) = contrast_accent(accent_rgb);

    NSColor::colorWithRed_green_blue_alpha(ar, ag, ab, 0.92).set();
    for (i, &bh) in bar_heights.iter().enumerate() {
        let bx = wave_x + i as f64 * 4.8;
        let by = wave_y + (wave_h - bh);
        let bar_rect = NSRect::new(NSPoint::new(bx, by), NSSize::new(3.0, bh));
        let bar_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bar_rect, 1.5, 1.5);
        bar_path.fill();
    }

    // 5. Seek Bar (Row 2) - Matches Spotifast player bar thin_slider (shape, height, colors, handle)
    let start_x = 60.0f64;
    let end_x = (card_w - 58.0).max(start_x + 20.0);
    let track_w = (end_x - start_x).max(1.0);
    let center_y = 82.0f64;

    let (duration_ms, position_ms) = if let Some(track) = &ctrl.track {
        let pos = if let Some((seek_pos, at)) = ctrl.pending_seek {
            if at.elapsed() > std::time::Duration::from_millis(2000)
                || (track.position_ms as i64 - seek_pos as i64).abs() < 1200
            {
                track.position_ms
            } else {
                seek_pos
            }
        } else {
            track.position_ms
        };
        (
            track.duration_ms.max(1) as f64,
            pos.min(track.duration_ms) as f64,
        )
    } else {
        (1.0, 0.0)
    };
    let ratio = (position_ms / duration_ms).clamp(0.0, 1.0);
    let played_w = track_w * ratio;
    let current_x = start_x + played_w;

    // Track: 4.0pt height, 2.0pt corner radius, white with 0.196 alpha (matching Color32::from_white_alpha(50))
    let track_rect = NSRect::new(
        NSPoint::new(start_x, center_y - 2.0),
        NSSize::new(track_w, 4.0),
    );
    let track_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(track_rect, 2.0, 2.0);
    NSColor::colorWithWhite_alpha(1.0, 0.196).set();
    track_path.fill();

    // Progress fill: 4.0pt height, 2.0pt corner radius, palette.accent (track accent)
    if played_w > 0.0 {
        let played_rect = NSRect::new(
            NSPoint::new(start_x, center_y - 2.0),
            NSSize::new(played_w.max(4.0).min(track_w), 4.0),
        );
        let played_path =
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(played_rect, 2.0, 2.0);
        NSColor::colorWithRed_green_blue_alpha(ar, ag, ab, 1.0).set();
        played_path.fill();

        // Handle (Thumb): 6.0pt radius (12.0pt diameter) solid white circle matching thin_slider
        let thumb_rect = NSRect::new(
            NSPoint::new(current_x - 6.0, center_y - 6.0),
            NSSize::new(12.0, 12.0),
        );
        let thumb_path = NSBezierPath::bezierPathWithOvalInRect(thumb_rect);
        NSColor::whiteColor().set();
        thumb_path.fill();
    }

    // 6. Play button circular disc (Row 3, Center) - Matches Spotifast theme::circle_button (diameter 36.0)
    let center_x = card_w / 2.0;
    let disc_rect = NSRect::new(NSPoint::new(center_x - 18.0, 97.0), NSSize::new(36.0, 36.0));
    let disc_path = NSBezierPath::bezierPathWithOvalInRect(disc_rect);
    NSColor::colorWithRed_green_blue_alpha(0.95, 0.96, 0.97, 1.0).set(); // palette.text (#f2f4f6)
    disc_path.fill();
}

pub fn is_active() -> bool {
    if let Ok(lock) = CONTROLLER.lock()
        && let Some(ctrl) = lock.as_ref()
    {
        return crate::notch::is_controller_active(
            ctrl.expanded,
            ctrl.pending_expand_at.is_some(),
            ctrl.pending_collapse_at.is_some(),
            ctrl.collapsing_until.is_some(),
        );
    }
    false
}

pub fn sync_state(enabled: bool, is_background: bool, track: Option<&NotchTrackInfo>) {
    if enabled && track.is_some() {
        let is_empty = CONTROLLER.lock().map(|l| l.is_none()).unwrap_or(false);
        if is_empty {
            init();
        }
    }
    let Ok(mut lock) = CONTROLLER.lock() else {
        return;
    };
    let Some(ctrl) = lock.as_mut() else {
        return;
    };

    ctrl.enabled = enabled;
    ctrl.is_minimized_or_background = is_background;

    // Check pending expand delay (60ms hover dwell + 260ms snappy Apple animation)
    if let Some(due) = ctrl.pending_expand_at {
        if Instant::now() >= due {
            perform_expand_locked(ctrl);
        } else {
            crate::notch::wake();
        }
    }

    // Check pending collapse delay (400ms linger before crisp 200ms popdown)
    if let Some(due) = ctrl.pending_collapse_at {
        if Instant::now() >= due {
            perform_collapse_locked(ctrl);
        } else {
            crate::notch::wake();
        }
    }

    // Complete collapse: reset window frame back to physical notch size
    if let Some(until) = ctrl.collapsing_until
        && Instant::now() >= until
    {
        complete_collapse_locked(ctrl);
    }

    let frames_opt = MainThreadMarker::new().and_then(compute_frames);
    let has_notch = frames_opt.is_some();
    if let Some(ref frames) = frames_opt {
        update_geometry_with_frames(ctrl, frames);
    }

    // Visibility: only show overlay when enabled, has a track, app is in the background,
    // and a display with a notch is present (handles clamshell mode with external monitors).
    // Checking track.is_some() (the incoming state) avoids one-frame latency and prevents
    // leaving an invisible click-eater window at the notch when music clears.
    let in_foreground = is_in_foreground(ctrl);
    let should_show =
        crate::notch::should_show_notch_window(enabled, in_foreground, track.is_some(), has_notch);
    if should_show {
        ctrl.window.orderFrontRegardless();
    } else {
        ctrl.window.orderOut(None);
        ctrl.pending_expand_at = None;
        ctrl.pending_collapse_at = None;
        cancel_hover_timers();
        ctrl.expanded = false;
        complete_collapse_locked(ctrl);
        reset_track_fade(ctrl);
    }

    if let Some(mtm) = MainThreadMarker::new() {
        apply_pending_artwork_locked(ctrl, mtm);
    }

    // Phase 3: fade+slide new content in once the 120ms fade-out has elapsed.
    if let Some(due) = ctrl.track_flash_until {
        if Instant::now() >= due {
            ctrl.track_flash_until = None;
            NSAnimationContext::beginGrouping();
            let ctx = NSAnimationContext::currentContext();
            // 300ms easeInEaseOut: content floats in smoothly
            ctx.setDuration(0.30);
            ctx.setAllowsImplicitAnimation(true);
            unsafe {
                if let Some(timing_cls) = objc2::runtime::AnyClass::get(c"CAMediaTimingFunction") {
                    let timing_fn: *mut objc2::runtime::AnyObject = objc2::msg_send![
                        timing_cls,
                        functionWithName: objc2_foundation::ns_string!("easeInEaseOut")
                    ];
                    if !timing_fn.is_null() {
                        let ctx_ref = &*NSAnimationContext::currentContext();
                        let () = objc2::msg_send![
                            ctx_ref,
                            setTimingFunction: timing_fn
                        ];
                    }
                }
            }
            ctrl.title_field.animator().setAlphaValue(1.0);
            ctrl.artist_field.animator().setAlphaValue(1.0);
            ctrl.art_view.animator().setAlphaValue(1.0);
            NSAnimationContext::endGrouping();
        } else {
            crate::notch::wake();
        }
    }

    // Update track metadata
    let track_changed = crate::notch::is_track_metadata_different(ctrl.track.as_ref(), track);
    if track_changed {
        ctrl.pending_seek = None;
        // Premium Dynamic Island-style content transition:
        // Phase 1: fade old content out (120 ms crisp)
        // Phase 2: swap values while hidden, start art cross-dissolve
        // Phase 3: fade+slide new content in (220 ms spring)
        let should_animate = crate::notch::should_animate_track_transition(
            ctrl.expanded,
            ctrl.track.as_ref(),
            track,
        );
        ctrl.track = track.cloned();

        if should_animate {
            // Phase 1: fade old content out over 200ms for a smooth, unhurried exit
            NSAnimationContext::beginGrouping();
            let ctx = NSAnimationContext::currentContext();
            ctx.setDuration(0.20);
            ctx.setAllowsImplicitAnimation(true);
            ctrl.title_field.animator().setAlphaValue(0.0);
            ctrl.artist_field.animator().setAlphaValue(0.0);
            ctrl.art_view.animator().setAlphaValue(0.0);
            NSAnimationContext::endGrouping();

            // Phase 3 fires at 220ms (200ms fade-out + 20ms swap buffer)
            ctrl.track_flash_until = Some(Instant::now() + std::time::Duration::from_millis(220));
            crate::notch::wake();
        } else {
            reset_track_fade(ctrl);
        }

        if let Some(t) = track {
            let title = if t.title.trim().is_empty() {
                "Spotifast"
            } else {
                &t.title
            };
            let clean_title = title.replace(['\r', '\n'], " ");
            let clean_artist = t.artist.replace(['\r', '\n'], " ");
            ctrl.title_field
                .setStringValue(&NSString::from_str(&clean_title));
            ctrl.artist_field
                .setStringValue(&NSString::from_str(&clean_artist));

            update_time_labels(
                &ctrl.canvas_view,
                &ctrl.elapsed_field,
                &ctrl.remaining_field,
                t.position_ms,
                t.duration_ms,
            );

            update_play_button_ui(&ctrl.play_button, t.playing);
            update_like_button_ui(&ctrl.like_button, t.saved, t.accent);
            update_shuffle_button_ui(&ctrl.shuffle_button, t.shuffle, t.accent);
            update_repeat_button_ui(&ctrl.repeat_button, t.repeat, t.accent);
            update_device_button_ui(&ctrl.device_button, t.is_remote, t.accent);
            ctrl.like_button.setHidden(!can_toggle_saved(t.is_episode));

            update_artwork_path(ctrl, t.art_path.clone());
        } else {
            ctrl.title_field
                .setStringValue(&NSString::from_str("Spotifast"));
            ctrl.artist_field
                .setStringValue(&NSString::from_str("Nothing playing"));
            update_time_labels(
                &ctrl.canvas_view,
                &ctrl.elapsed_field,
                &ctrl.remaining_field,
                0,
                0,
            );
            update_play_button_ui(&ctrl.play_button, false);
            update_like_button_ui(&ctrl.like_button, false, None);
            update_shuffle_button_ui(&ctrl.shuffle_button, false, None);
            update_repeat_button_ui(&ctrl.repeat_button, crate::player::RepeatMode::Off, None);
            update_device_button_ui(&ctrl.device_button, false, None);
            ctrl.like_button.setHidden(false);
            ctrl.art_view.setImage(None);
            ctrl.current_art_path = None;
        }
        ctrl.canvas_view.setNeedsDisplay(true);
    } else if let (Some(cached), Some(latest)) = (ctrl.track.as_mut(), track) {
        let changes = crate::notch::detect_incremental_changes(cached, latest);
        let mut updated = latest.clone();
        if let Some((seek_pos, at)) = ctrl.pending_seek {
            if at.elapsed() <= std::time::Duration::from_millis(2000)
                && (latest.position_ms as i64 - seek_pos as i64).abs() >= 1200
            {
                updated.position_ms = seek_pos;
            } else {
                ctrl.pending_seek = None;
            }
        }
        *cached = updated;

        if changes.episode_changed {
            ctrl.like_button
                .setHidden(!can_toggle_saved(latest.is_episode));
        }

        if changes.art_changed {
            update_artwork_path(ctrl, latest.art_path.clone());
        }

        if changes.play_changed {
            update_play_button_ui(&ctrl.play_button, latest.playing);
        }

        if changes.saved_changed || (latest.saved && changes.accent_changed) {
            update_like_button_ui(&ctrl.like_button, latest.saved, latest.accent);
        }

        if changes.shuffle_changed || (latest.shuffle && changes.accent_changed) {
            update_shuffle_button_ui(&ctrl.shuffle_button, latest.shuffle, latest.accent);
        }

        if changes.repeat_changed
            || (latest.repeat != crate::player::RepeatMode::Off && changes.accent_changed)
        {
            update_repeat_button_ui(&ctrl.repeat_button, latest.repeat, latest.accent);
        }

        if changes.remote_changed || (latest.is_remote && changes.accent_changed) {
            update_device_button_ui(&ctrl.device_button, latest.is_remote, latest.accent);
        }

        if changes.progress_changed || changes.duration_changed {
            let pos = if let Some((seek_pos, at)) = ctrl.pending_seek {
                if at.elapsed() > std::time::Duration::from_millis(2000)
                    || (latest.position_ms as i64 - seek_pos as i64).abs() < 1200
                {
                    ctrl.pending_seek = None;
                    latest.position_ms
                } else {
                    seek_pos
                }
            } else {
                latest.position_ms
            };
            update_time_labels(
                &ctrl.canvas_view,
                &ctrl.elapsed_field,
                &ctrl.remaining_field,
                pos,
                latest.duration_ms,
            );
            ctrl.canvas_view.setNeedsDisplay(true);
        }

        if changes.accent_changed {
            ctrl.canvas_view.setNeedsDisplay(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_accent_dark_fallback() {
        // Pure black must fall back to vibrant Spotify green
        let (r, g, b) = contrast_accent([0, 0, 0]);
        assert_eq!((r, g, b), (0.118, 0.843, 0.376));

        // Near black also falls back to Spotify green
        let (r, g, b) = contrast_accent([10, 10, 10]);
        assert_eq!((r, g, b), (0.118, 0.843, 0.376));
    }

    #[test]
    fn contrast_accent_boosts_low_luminance() {
        // Medium dark color gets luminance boosted to at least 0.42
        let (r, g, b) = contrast_accent([30, 40, 50]);
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        assert!(
            lum >= 0.419,
            "Luminance should be boosted to at least 0.42, got {lum}"
        );
    }

    #[test]
    fn contrast_accent_preserves_bright_colors() {
        // Bright color is preserved without modification
        let (r, g, b) = contrast_accent([200, 220, 240]);
        assert!((r - 200.0 / 255.0).abs() < 1e-6);
        assert!((g - 220.0 / 255.0).abs() < 1e-6);
        assert!((b - 240.0 / 255.0).abs() < 1e-6);
    }
}
