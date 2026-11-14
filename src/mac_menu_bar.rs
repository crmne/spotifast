//! The macOS menu-bar item that names the song (#1259), beside the tray's
//! icon. It shows while a song is loaded, playing or paused, and hides
//! while none is. A click opens a card under it that the cover fills; with
//! the pointer over the card the cover blurs and dims behind the song, its
//! artists and the playback buttons, as in SpotMenu. Another click on the
//! item, or one anywhere else, closes it. Settings > Playback turns it on.
//!
//! The card is a panel that never brings the app forward, and its buttons
//! take the first click. A status item's click leaves the frontmost app in
//! front, so a card that had to be made key first would spend the click
//! on that and drop it.
//!
//! AppKit allows status items on the main thread only, so the item is made
//! by the first [`MenuBar::show`] there and lives in a thread-local. Its
//! clicks reach the app as [`fastframe_tray::Event`]s, read with the tray's.

use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use fastframe_tray::Event;

use crate::i18n::{Locale, gettext};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSApplication, NSBackingStoreType, NSButton,
    NSCellImagePosition, NSColor, NSEvent, NSEventMask, NSFont, NSFontWeightRegular, NSImage,
    NSImageScaling, NSImageSymbolConfiguration, NSImageView, NSLineBreakMode, NSPanel, NSScreen,
    NSStatusBar, NSStatusItem, NSTextAlignment, NSTextField, NSTrackingArea, NSTrackingAreaOptions,
    NSVariableStatusItemLength, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSData, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use crate::notch::NotchTrackInfo;

/// The most characters the menu bar shows of the song and its artists. Past
/// it the text ends in an ellipsis, so a long title leaves room for the
/// other items (on a MacBook, room that would otherwise go behind the notch).
pub const TITLE_MAX_CHARS: usize = 32;

/// What the item says: the loaded song and its artists, cut to
/// [`TITLE_MAX_CHARS`], playing or paused. Nothing while no song is
/// loaded, which hides the item.
pub fn status_title(track: Option<&NotchTrackInfo>) -> String {
    let Some(track) = track else {
        return String::new();
    };
    let title = track.title.trim();
    let artist = track.artist.trim();
    if title.is_empty() {
        return String::new();
    }
    if artist.is_empty() {
        ellipsize(title, TITLE_MAX_CHARS)
    } else {
        ellipsize(&format!("{title} · {artist}"), TITLE_MAX_CHARS)
    }
}

/// `text` cut to `max` characters, the last of them an ellipsis.
fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// The tray menu ids the card's buttons send, so a click there is the same
/// event as the matching menu entry.
#[derive(Clone, Copy, Debug)]
pub struct Controls {
    pub previous: &'static str,
    pub play_pause: &'static str,
    pub next: &'static str,
    /// The app's icon in the card's corner, which opens the window.
    pub window: &'static str,
}

/// Where the item's clicks go: the app's queue, then a wake so a closed
/// window's tick reads them at once.
#[derive(Clone)]
struct Route {
    sender: Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Route {
    fn send(&self, event: Event) {
        if self.sender.send(event).is_ok() {
            (self.wake)();
        }
    }
}

/// The song item, seen from the app. Dropping it removes the item.
pub struct MenuBar {
    events: Receiver<Event>,
    controls: Controls,
    route: Route,
}

impl std::fmt::Debug for MenuBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuBar").finish_non_exhaustive()
    }
}

impl MenuBar {
    /// Gets the item ready; [`show`](Self::show) makes it on the main
    /// thread once it is wanted. `wake` is called after each click.
    pub fn spawn(controls: Controls, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (sender, events) = std::sync::mpsc::channel();
        Self {
            events,
            controls,
            route: Route {
                sender,
                wake: Arc::new(wake),
            },
        }
    }

    /// The card's clicks since the last call, oldest first.
    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Shows `track` in the menu bar and on the card while `enabled`, and
    /// removes the item while not. VoiceOver reads the item and the card's
    /// buttons in `locale`. On the main thread only.
    pub fn show(&mut self, enabled: bool, track: Option<&NotchTrackInfo>, locale: Locale) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        ITEM.with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return;
            };
            if !enabled {
                slot.take();
                return;
            }
            slot.get_or_insert_with(|| Item::new(self.controls, self.route.clone(), mtm))
                .show(track, locale, mtm);
        });
    }
}

impl Drop for MenuBar {
    fn drop(&mut self) {
        if MainThreadMarker::new().is_some() {
            ITEM.with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut() {
                    slot.take();
                }
            });
        }
    }
}

thread_local! {
    /// The status item and its card, which only the main thread touches.
    static ITEM: RefCell<Option<Item>> = const { RefCell::new(None) };
}

/// Each cover load's number; a load only lands while it is the newest.
static ART_GENERATION: AtomicU64 = AtomicU64::new(0);
/// A loaded cover, with its load's number, for the next frame.
static ART_LOADED: Mutex<Option<(u64, Cover)>> = Mutex::new(None);

/// Acts on the item, once it exists.
fn with_item(act: impl FnOnce(&Item)) {
    ITEM.with(|slot| {
        if let Ok(slot) = slot.try_borrow()
            && let Some(item) = slot.as_ref()
        {
            act(item);
        }
    });
}

/// The card's side and corner radius, in points, as SpotMenu's.
const CARD_SIZE: f64 = 300.0;
const CARD_RADIUS: f64 = 16.0;
/// The gap between the menu bar and the card.
const CARD_GAP: f64 = 4.0;
/// The app icon sits this far into the card's top corner.
const CORNER_INSET: f64 = 16.0;
const CORNER_ICON: f64 = 20.0;
/// The playback buttons' symbols and the squares that take their clicks.
const PLAY_POINTS: f64 = 40.0;
const SKIP_POINTS: f64 = 30.0;
const PLAY_HIT: f64 = 64.0;
const SKIP_HIT: f64 = 56.0;
const SKIP_OFFSET: f64 = 80.0;
/// The song sits under the buttons and its artists over them.
const LINE_HEIGHT: f64 = 20.0;
const LINE_GAP: f64 = 10.0;
/// How long the controls take to fade in or out under the pointer.
const HOVER_SECONDS: f64 = 0.2;

/// What the item last showed, so a frame changes AppKit only when it did.
#[derive(Default)]
struct Shown {
    title: String,
    song: String,
    artist: String,
    playing: bool,
    locale: Option<Locale>,
    art: Option<PathBuf>,
    art_generation: u64,
}

struct Item {
    status: Retained<NSStatusItem>,
    panel: Retained<NSPanel>,
    /// What fades in under the pointer: the blurred, dimmed cover and the
    /// controls over it.
    controls_layer: Retained<NSView>,
    art: Retained<NSImageView>,
    blurred: Retained<NSImageView>,
    /// The note drawn while the song has no cover.
    placeholder: Retained<NSImageView>,
    song: Retained<NSTextField>,
    artist: Retained<NSTextField>,
    previous: Retained<CardButton>,
    play_pause: Retained<CardButton>,
    next: Retained<CardButton>,
    window_button: Retained<CardButton>,
    play: Retained<NSImage>,
    pause: Retained<NSImage>,
    /// The item's and the card's actions. Kept alive here, since AppKit
    /// holds targets weakly.
    _target: Retained<Target>,
    /// The tracking area that tells the card the pointer came or went.
    _tracking: Retained<NSTrackingArea>,
    controls: Controls,
    route: Route,
    /// The event monitors that close the open card on a click elsewhere.
    monitors: RefCell<Vec<Retained<AnyObject>>>,
    shown: Shown,
}

impl Item {
    fn new(controls: Controls, route: Route, mtm: MainThreadMarker) -> Self {
        let target = Target::new(mtm);
        let status =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        // Its own name, so a place the user drags it to is kept apart from
        // the tray's item and lasts across runs.
        status.setAutosaveName(Some(&NSString::from_str("now-playing")));
        status.setVisible(false);
        if let Some(button) = status.button(mtm) {
            // SAFETY: `clicked:` is defined on Target and takes one object.
            unsafe {
                button.setTarget(Some(&target));
                button.setAction(Some(sel!(clicked:)));
            }
            button.sendActionOn(NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown);
        }

        let bounds = NSRect::new(NSPoint::ZERO, NSSize::new(CARD_SIZE, CARD_SIZE));
        // Borderless and non-activating: a click on it leaves the app in
        // front where it is. It is only ever ordered out, never closed.
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            bounds,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        // SAFETY: the panel is retained by Item, so AppKit must not free it.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHasShadow(true);
        panel.setHidesOnDeactivate(false);
        panel.setLevel(3); // NSFloatingWindowLevel, as SpotMenu's card
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::Transient
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );

        // SAFETY: CardView is an NSView subclass with no state of its own.
        let card: Retained<CardView> =
            unsafe { msg_send![mtm.alloc::<CardView>(), initWithFrame: bounds] };
        card.setWantsLayer(true);
        crate::mac_notch::set_view_corner_radius(&card, CARD_RADIUS);
        let tracking = {
            let options = NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect;
            // SAFETY: the card owns the area and outlives it with Item.
            unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    mtm.alloc(),
                    NSRect::ZERO,
                    options,
                    Some(&card),
                    None,
                )
            }
        };
        card.addTrackingArea(&tracking);
        panel.setContentView(Some(&card));

        let backdrop = NSVisualEffectView::initWithFrame(mtm.alloc(), bounds);
        backdrop.setMaterial(NSVisualEffectMaterial::HUDWindow);
        backdrop.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        backdrop.setState(NSVisualEffectState::Active);
        backdrop.setWantsLayer(true);
        crate::mac_notch::set_view_corner_radius(&backdrop, CARD_RADIUS);
        card.addSubview(&backdrop);

        let note_side = 100.0;
        let placeholder = NSImageView::initWithFrame(
            mtm.alloc(),
            NSRect::new(
                NSPoint::new((CARD_SIZE - note_side) / 2.0, (CARD_SIZE - note_side) / 2.0),
                NSSize::new(note_side, note_side),
            ),
        );
        placeholder.setImage(symbol("music.note", 72.0).as_deref());
        placeholder.setContentTintColor(Some(&NSColor::whiteColor().colorWithAlphaComponent(0.2)));
        card.addSubview(&placeholder);

        let art = cover_view(mtm, bounds);
        card.addSubview(&art);

        let controls_layer = NSView::initWithFrame(mtm.alloc(), bounds);
        controls_layer.setWantsLayer(true);
        controls_layer.setAlphaValue(0.0);
        card.addSubview(&controls_layer);
        let blurred = cover_view(mtm, bounds);
        controls_layer.addSubview(&blurred);

        let button = |image: Option<&NSImage>, action: Sel, frame: NSRect| {
            // SAFETY: CardButton is an NSButton subclass with no state of its own.
            let button: Retained<CardButton> =
                unsafe { msg_send![mtm.alloc::<CardButton>(), initWithFrame: frame] };
            button.setBordered(false);
            button.setTitle(&NSString::new());
            button.setImage(image);
            button.setImagePosition(NSCellImagePosition::ImageOnly);
            button.setContentTintColor(Some(&NSColor::whiteColor()));
            // SAFETY: each action is defined on Target and takes one object.
            unsafe {
                button.setTarget(Some(&target));
                button.setAction(Some(action));
            }
            controls_layer.addSubview(&button);
            button
        };
        let centred = |x: f64, side: f64| {
            NSRect::new(
                NSPoint::new(x - side / 2.0, (CARD_SIZE - side) / 2.0),
                NSSize::new(side, side),
            )
        };
        let middle = CARD_SIZE / 2.0;
        let play = symbol("play.fill", PLAY_POINTS);
        let pause = symbol("pause.fill", PLAY_POINTS);
        let previous = button(
            symbol("backward.fill", SKIP_POINTS).as_deref(),
            sel!(previous:),
            centred(middle - SKIP_OFFSET, SKIP_HIT),
        );
        let play_pause = button(play.as_deref(), sel!(playPause:), centred(middle, PLAY_HIT));
        let next = button(
            symbol("forward.fill", SKIP_POINTS).as_deref(),
            sel!(next:),
            centred(middle + SKIP_OFFSET, SKIP_HIT),
        );
        let corner_y = CARD_SIZE - CORNER_INSET - CORNER_ICON;
        let corner = NSSize::new(CORNER_ICON, CORNER_ICON);
        let icon = NSApplication::sharedApplication(mtm).applicationIconImage();
        let window_button = button(
            icon.as_deref(),
            sel!(openWindow:),
            NSRect::new(NSPoint::new(CORNER_INSET, corner_y), corner),
        );
        window_button.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);

        let line = |y: f64, font: Retained<NSFont>| {
            let field = NSTextField::labelWithString(&NSString::new(), mtm);
            field.setFrame(NSRect::new(
                NSPoint::new(CORNER_INSET, y),
                NSSize::new(CARD_SIZE - 2.0 * CORNER_INSET, LINE_HEIGHT),
            ));
            field.setFont(Some(&font));
            field.setTextColor(Some(&NSColor::whiteColor()));
            field.setAlignment(NSTextAlignment::Center);
            field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            field.setMaximumNumberOfLines(1);
            controls_layer.addSubview(&field);
            field
        };
        let artist = line(
            middle + PLAY_HIT / 2.0 + LINE_GAP,
            NSFont::systemFontOfSize(15.0),
        );
        let song = line(
            middle - PLAY_HIT / 2.0 - LINE_GAP - LINE_HEIGHT,
            NSFont::boldSystemFontOfSize(15.0),
        );

        Self {
            status,
            panel,
            controls_layer,
            art,
            blurred,
            placeholder,
            song,
            artist,
            previous,
            play_pause,
            next,
            window_button,
            play: play.unwrap_or_default(),
            pause: pause.unwrap_or_default(),
            _target: target,
            _tracking: tracking,
            controls,
            route,
            monitors: RefCell::new(Vec::new()),
            shown: Shown::default(),
        }
    }

    fn show(&mut self, track: Option<&NotchTrackInfo>, locale: Locale, mtm: MainThreadMarker) {
        self.label(locale, mtm);
        let title = status_title(track);
        if title != self.shown.title {
            if let Some(button) = self.status.button(mtm) {
                button.setTitle(&NSString::from_str(&title));
            }
            if title.is_empty() {
                self.close(mtm);
            }
            self.status.setVisible(!title.is_empty());
            self.shown.title = title;
        }
        let song = track.map(|track| track.title.as_str()).unwrap_or_default();
        if song != self.shown.song {
            self.song.setStringValue(&NSString::from_str(song));
            song.clone_into(&mut self.shown.song);
        }
        let artist = track.map(|track| track.artist.as_str()).unwrap_or_default();
        if artist != self.shown.artist {
            self.artist.setStringValue(&NSString::from_str(artist));
            artist.clone_into(&mut self.shown.artist);
        }
        self.show_playing(track.is_some_and(|track| track.playing));
        let art = track.and_then(|track| track.art_path.clone());
        if art != self.shown.art {
            self.set_cover(None);
            let wake = &self.route.wake;
            self.shown.art_generation = art
                .clone()
                .map_or(0, |path| load_art(path, Arc::clone(wake)));
            self.shown.art = art;
        }
        self.take_loaded_art(mtm);
    }

    fn show_playing(&mut self, playing: bool) {
        if playing != self.shown.playing {
            self.shown.playing = playing;
            let image = if playing { &self.pause } else { &self.play };
            self.play_pause.setImage(Some(image));
            if let Some(locale) = self.shown.locale {
                set_accessibility_label(&self.play_pause, &play_pause_label(locale, playing));
            }
        }
    }

    /// Names the item and the card's buttons for VoiceOver in `locale`,
    /// again whenever Settings changes the language.
    fn label(&mut self, locale: Locale, mtm: MainThreadMarker) {
        if self.shown.locale == Some(locale) {
            return;
        }
        self.shown.locale = Some(locale);
        if let Some(button) = self.status.button(mtm) {
            set_accessibility_label(&button, &gettext(locale, "Now playing"));
        }
        set_accessibility_label(&self.previous, &gettext(locale, "Previous"));
        set_accessibility_label(
            &self.play_pause,
            &play_pause_label(locale, self.shown.playing),
        );
        set_accessibility_label(&self.next, &gettext(locale, "Next"));
        set_accessibility_label(&self.window_button, &gettext(locale, "Open main window"));
    }

    /// Puts a cover loaded since the last frame on the card, unless a newer
    /// song has asked for another since.
    fn take_loaded_art(&self, mtm: MainThreadMarker) {
        let Ok(mut loaded) = ART_LOADED.lock() else {
            return;
        };
        let Some((generation, _)) = loaded.as_ref() else {
            return;
        };
        if *generation != self.shown.art_generation {
            return;
        }
        if let Some((_, cover)) = loaded.take() {
            let image =
                |bytes: &[u8]| NSImage::initWithData(mtm.alloc(), &NSData::with_bytes(bytes));
            let sharp = image(&cover.sharp);
            let blurred = image(&cover.blurred);
            self.set_cover(sharp.as_deref().zip(blurred.as_deref()));
        }
    }

    /// Shows a cover and its blurred copy, or the note without one.
    fn set_cover(&self, cover: Option<(&NSImage, &NSImage)>) {
        self.art.setImage(cover.map(|(sharp, _)| sharp));
        self.blurred.setImage(cover.map(|(_, blurred)| blurred));
        self.placeholder.setHidden(cover.is_some());
    }

    fn is_open(&self) -> bool {
        self.panel.isVisible()
    }

    /// Fades the controls in while the pointer is over the card, and out
    /// once it leaves.
    fn reveal(&self, shown: bool) {
        NSAnimationContext::beginGrouping();
        NSAnimationContext::currentContext().setDuration(HOVER_SECONDS);
        self.controls_layer
            .animator()
            .setAlphaValue(if shown { 1.0 } else { 0.0 });
        NSAnimationContext::endGrouping();
    }

    /// Opens the card under the item, fading in as SpotMenu's does, and
    /// watches for a click elsewhere to close it.
    fn open(&self, mtm: MainThreadMarker) {
        let frame = card_frame(&self.status, mtm);
        self.panel.setFrame_display(frame, false);
        let pointer = NSEvent::mouseLocation();
        let inside = pointer.x >= frame.origin.x
            && pointer.x <= frame.origin.x + frame.size.width
            && pointer.y >= frame.origin.y
            && pointer.y <= frame.origin.y + frame.size.height;
        self.controls_layer
            .setAlphaValue(if inside { 1.0 } else { 0.0 });
        self.panel.setAlphaValue(0.0);
        self.panel.orderFrontRegardless();
        self.panel.invalidateShadow();
        NSAnimationContext::beginGrouping();
        NSAnimationContext::currentContext().setDuration(HOVER_SECONDS);
        self.panel.animator().setAlphaValue(1.0);
        NSAnimationContext::endGrouping();
        if let Some(button) = self.status.button(mtm) {
            button.highlight(true);
        }
        self.watch_clicks_elsewhere();
    }

    fn close(&self, mtm: MainThreadMarker) {
        for monitor in self.monitors.borrow_mut().drain(..) {
            // SAFETY: each was returned by an NSEvent add…Monitor call.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        self.panel.orderOut(None);
        if let Some(button) = self.status.button(mtm) {
            button.highlight(false);
        }
    }

    /// A click anywhere but on the card or the item closes the card; the
    /// item's own click toggles it in [`clicked`].
    fn watch_clicks_elsewhere(&self) {
        let mut monitors = self.monitors.borrow_mut();
        if !monitors.is_empty() {
            return;
        }
        let mask = NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown;
        let elsewhere = RcBlock::new(|_: NonNull<NSEvent>| close_unless_on_card_or_item());
        monitors.extend(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            mask, &elsewhere,
        ));
        let here = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            close_unless_on_card_or_item();
            event.as_ptr()
        });
        // SAFETY: the handler returns the event it was given, unchanged.
        monitors
            .extend(unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &here) });
    }
}

impl Drop for Item {
    fn drop(&mut self) {
        if let Some(mtm) = MainThreadMarker::new() {
            self.close(mtm);
        }
        NSStatusBar::systemStatusBar().removeStatusItem(&self.status);
    }
}

/// A full-card image view for a cover.
fn cover_view(mtm: MainThreadMarker, bounds: NSRect) -> Retained<NSImageView> {
    let view = NSImageView::initWithFrame(mtm.alloc(), bounds);
    // Covers come square (see `prepare_cover`), so this fills the card.
    view.setImageScaling(NSImageScaling::ScaleAxesIndependently);
    view
}

/// Where the card goes: just under the menu bar, centred on the item and
/// kept on its screen. An item macOS has hidden for want of room has no
/// place of its own, so the card goes under the pointer instead.
fn card_frame(status: &NSStatusItem, mtm: MainThreadMarker) -> NSRect {
    let item_window: Option<Retained<NSWindow>> =
        status.button(mtm).and_then(|button| button.window());
    let screen = item_window
        .as_ref()
        .and_then(|window| window.screen())
        .or_else(|| NSScreen::mainScreen(mtm));
    let Some(screen) = screen else {
        return NSRect::new(NSPoint::ZERO, NSSize::new(CARD_SIZE, CARD_SIZE));
    };
    let visible = screen.visibleFrame();
    let full = screen.frame();
    let top = visible.origin.y + visible.size.height;
    let placed = item_window.map(|window| window.frame()).filter(|frame| {
        frame.size.width > 0.0
            && frame.size.height > 0.0
            && frame.origin.x >= full.origin.x
            && frame.origin.x + frame.size.width <= full.origin.x + full.size.width
            && frame.origin.y + frame.size.height >= top
    });
    let centre = placed.map_or_else(
        || NSEvent::mouseLocation().x,
        |frame| frame.origin.x + frame.size.width / 2.0,
    );
    let margin = 8.0;
    let lowest = visible.origin.x + margin;
    let highest = (visible.origin.x + visible.size.width - margin - CARD_SIZE).max(lowest);
    let x = (centre - CARD_SIZE / 2.0).clamp(lowest, highest);
    NSRect::new(
        NSPoint::new(x, top - CARD_GAP - CARD_SIZE),
        NSSize::new(CARD_SIZE, CARD_SIZE),
    )
}

/// Closes the card for a click that is not on it or on the item. Where the
/// pointer is decides, not the click's window: a click on the item reaches
/// a monitor before the item, and closing the card there would have the
/// item's own click open it again.
fn close_unless_on_card_or_item() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let at = NSEvent::mouseLocation();
    let holds = |frame: NSRect| {
        at.x >= frame.origin.x
            && at.x <= frame.origin.x + frame.size.width
            && at.y >= frame.origin.y
            && at.y <= frame.origin.y + frame.size.height
    };
    let mut ours = false;
    with_item(|item| {
        let item_window: Option<Retained<NSWindow>> =
            item.status.button(mtm).and_then(|button| button.window());
        ours = holds(item.panel.frame()) || item_window.is_some_and(|window| holds(window.frame()));
    });
    if !ours {
        close_card();
    }
}

fn close_card() {
    if let Some(mtm) = MainThreadMarker::new() {
        with_item(|item| item.close(mtm));
    }
}

/// A cover ready for the card: the cover cut square, and a blurred,
/// dimmed copy for under the controls.
struct Cover {
    sharp: Vec<u8>,
    blurred: Vec<u8>,
}

/// The blurred copy's side in pixels; small, since a blur shows no detail.
const BLUR_PIXELS: u32 = 128;
/// The blur's strength at that size, close to SpotMenu's on its card.
const BLUR_SIGMA: f32 = 2.5;
/// How much darker the blurred copy is, so white controls read on any cover.
const DIM: f32 = 0.4;

/// Cuts the cover in `bytes` square and makes its blurred, dimmed copy.
/// `None` when the bytes are not an image.
fn prepare_cover(bytes: Vec<u8>) -> Option<Cover> {
    let image = image::load_from_memory(&bytes).ok()?;
    let (width, height) = (image.width(), image.height());
    let side = width.min(height);
    let square = image.crop_imm((width - side) / 2, (height - side) / 2, side, side);
    let sharp = if width == height {
        bytes
    } else {
        png(&square.to_rgba8())?
    };
    let small = square
        .resize_exact(
            BLUR_PIXELS,
            BLUR_PIXELS,
            image::imageops::FilterType::Triangle,
        )
        .to_rgba8();
    let mut blurred = image::imageops::fast_blur(&small, BLUR_SIGMA);
    for pixel in blurred.pixels_mut() {
        for channel in &mut pixel.0[..3] {
            *channel = (f32::from(*channel) * (1.0 - DIM)).round() as u8;
        }
    }
    Some(Cover {
        sharp,
        blurred: png(&blurred)?,
    })
}

fn png(image: &image::RgbaImage) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .ok()?;
    Some(bytes)
}

/// Reads and prepares the cover at `path` off the main thread, which never
/// waits on the disk, and wakes the app to show it. Returns the load's
/// number.
fn load_art(path: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> u64 {
    let generation = ART_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    std::thread::spawn(move || {
        // A cover is a few hundred kilobytes; anything far larger is not one.
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        if meta.len() > 10 * 1024 * 1024 {
            return;
        }
        let Some(cover) = std::fs::read(&path).ok().and_then(prepare_cover) else {
            return;
        };
        if ART_GENERATION.load(Ordering::Relaxed) != generation {
            return;
        }
        if let Ok(mut loaded) = ART_LOADED.lock() {
            *loaded = Some((generation, cover));
        }
        wake();
    });
    generation
}

/// An SF Symbol at `points`.
fn symbol(name: &str, points: f64) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    )?;
    // SAFETY: an extern constant AppKit defines.
    let weight = unsafe { NSFontWeightRegular };
    let configuration =
        NSImageSymbolConfiguration::configurationWithPointSize_weight(points, weight);
    image.imageWithSymbolConfiguration(&configuration)
}

/// What the play button does now: pause while playing, else play.
fn play_pause_label(locale: Locale, playing: bool) -> std::borrow::Cow<'static, str> {
    if playing {
        gettext(locale, "Pause")
    } else {
        gettext(locale, "Play")
    }
}

fn set_accessibility_label(view: &NSView, label: &str) {
    let label = NSString::from_str(label);
    // SAFETY: NSView answers setAccessibilityLabel: with a string.
    unsafe {
        let () = msg_send![view, setAccessibilityLabel: &*label];
    }
}

/// A click on the item, either button: the card opens, or closes if open.
fn clicked(mtm: MainThreadMarker) {
    with_item(|item| {
        if item.is_open() {
            item.close(mtm);
        } else {
            item.open(mtm);
        }
    });
}

define_class!(
    /// The card: it shows its controls while the pointer is over it.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpotifastMenuBarCard"]
    struct CardView;

    impl CardView {
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            with_item(|item| item.reveal(true));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            with_item(|item| item.reveal(false));
        }
    }
);

define_class!(
    /// A card button that acts on the click that reaches it, though the
    /// card is not the key window and the app is not in front.
    #[unsafe(super(NSButton))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpotifastMenuBarCardButton"]
    struct CardButton;

    impl CardButton {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }
    }
);

define_class!(
    /// The item's and the card's actions.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpotifastMenuBarTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(clicked:))]
        fn clicked(&self, _sender: Option<&AnyObject>) {
            clicked(self.mtm());
        }

        #[unsafe(method(previous:))]
        fn previous(&self, _sender: Option<&AnyObject>) {
            with_item(|item| item.route.send(Event::Menu(item.controls.previous)));
        }

        #[unsafe(method(playPause:))]
        fn play_pause(&self, _sender: Option<&AnyObject>) {
            // The button shows the result at once; the app's next frame
            // confirms it.
            ITEM.with(|slot| {
                if let Ok(mut slot) = slot.try_borrow_mut()
                    && let Some(item) = slot.as_mut()
                {
                    let playing = !item.shown.playing;
                    item.show_playing(playing);
                    item.route.send(Event::Menu(item.controls.play_pause));
                }
            });
        }

        #[unsafe(method(next:))]
        fn next(&self, _sender: Option<&AnyObject>) {
            with_item(|item| item.route.send(Event::Menu(item.controls.next)));
        }

        #[unsafe(method(openWindow:))]
        fn open_window(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            with_item(|item| {
                item.close(mtm);
                item.route.send(Event::Menu(item.controls.window));
            });
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initializer, called once.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str, playing: bool) -> NotchTrackInfo {
        NotchTrackInfo {
            playing,
            title: title.into(),
            artist: artist.into(),
            ..NotchTrackInfo::default()
        }
    }

    #[test]
    fn the_menu_bar_names_the_playing_song_and_its_artists() {
        assert_eq!(
            status_title(Some(&track("Halo", "Beyoncé", true))),
            "Halo · Beyoncé"
        );
        assert_eq!(status_title(Some(&track("Intro", "", true))), "Intro");
        assert_eq!(status_title(Some(&track("  ", "Someone", true))), "");
    }

    /// Paused, the song stays named, as in SpotMenu.
    #[test]
    fn a_paused_song_stays_named() {
        assert_eq!(
            status_title(Some(&track("Halo", "Beyoncé", false))),
            "Halo · Beyoncé"
        );
    }

    /// With nothing loaded the item has nothing to say, so it hides and
    /// only the tray's icon is left.
    #[test]
    fn the_item_hides_while_nothing_is_loaded() {
        assert_eq!(status_title(None), "");
    }

    #[test]
    fn a_long_title_ends_in_an_ellipsis_at_the_limit() {
        let shown = status_title(Some(&track(
            "Bohemian Rhapsody - Remastered 2011",
            "Queen",
            true,
        )));
        // The space before the cut goes, so the ellipsis follows the word.
        assert_eq!(shown, "Bohemian Rhapsody - Remastered…");
        // Counted in characters, not bytes, so a word in another script is
        // never cut through a character.
        let wide = "東京事変".repeat(10);
        let shown = status_title(Some(&track(&wide, "", true)));
        assert_eq!(shown.chars().count(), TITLE_MAX_CHARS);
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn a_title_at_the_limit_is_kept_whole() {
        let exact = "a".repeat(TITLE_MAX_CHARS);
        assert_eq!(status_title(Some(&track(&exact, "", true))), exact);
    }

    /// The card fills with the cover, so a cover that is not square is cut
    /// square, and the copy under the controls is darker than the cover.
    #[test]
    fn a_cover_is_cut_square_with_a_dimmed_copy() {
        let wide = image::RgbaImage::from_pixel(40, 20, image::Rgba([255, 255, 255, 255]));
        let cover = prepare_cover(png(&wide).unwrap()).unwrap();
        let sharp = image::load_from_memory(&cover.sharp).unwrap().to_rgba8();
        assert_eq!(sharp.dimensions(), (20, 20));
        let blurred = image::load_from_memory(&cover.blurred).unwrap().to_rgba8();
        let middle = blurred.get_pixel(BLUR_PIXELS / 2, BLUR_PIXELS / 2);
        assert!(middle.0[0] < 200, "{middle:?}");
        assert!(prepare_cover(b"not a picture".to_vec()).is_none());
    }

    /// The play button reads what a press does, in the interface's language.
    #[test]
    fn the_play_button_is_named_in_the_interface_language() {
        assert_eq!(play_pause_label(Locale::English, false), "Play");
        assert_eq!(play_pause_label(Locale::English, true), "Pause");
        assert_eq!(play_pause_label(Locale::German, false), "Wiedergabe");
    }

    /// The card's buttons send the tray's own menu ids, so the app reads a
    /// click on them as the matching tray entry.
    #[test]
    fn the_cards_buttons_arrive_as_the_trays_menu_events() {
        let bar = MenuBar::spawn(
            Controls {
                previous: "previous",
                play_pause: "play-pause",
                next: "next",
                window: "main-window",
            },
            || {},
        );
        bar.route.send(Event::Menu(bar.controls.next));
        bar.route.send(Event::Menu(bar.controls.window));
        assert_eq!(
            bar.events(),
            vec![Event::Menu("next"), Event::Menu("main-window")]
        );
        assert!(bar.events().is_empty());
    }
}
