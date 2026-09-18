//! Guards against a known AppKit bug that turns closing the window into a
//! process-ending crash.
//!
//! Closing the window sometimes makes AppKit's automatic Touch Bar support
//! try to remove a KVO observer from the window's content view a second
//! time, after something has already invalidated it once. The second
//! removal throws an `NSRangeException` from inside a run-loop observer
//! callback, a place nothing upstream of us wraps in a `@try`/`@catch`, so
//! the exception reaches the process' top-level uncaught-exception handler
//! and aborts. `WinitView`, the class named in the crash, is winit's own
//! `NSView` subclass; other AppKit-based Rust and C++ GUIs (egui/eframe,
//! Godot) hit the identical crash. There is no public API to opt a view out
//! of the automatic Touch Bar responder chain, so, the same way
//! godotengine/godot#105804 fixed the identical crash there, this patches
//! `WinitView`'s inherited `removeObserver:forKeyPath:` to run inside a
//! `@try`/`@catch`. Only an exception matching this one documented AppKit
//! bug's signature is swallowed; anything else is re-thrown so a genuinely
//! different observer-lifecycle bug still crashes and gets reported, rather
//! than being hidden by this guard.
//!
//! Install once a window (and so the `WinitView` class) exists — the
//! `Box::new(move |cc| ...)` app creator in `entrypoint.rs` is early enough
//! and already runs other one-time macOS setup.

use std::ffi::CStr;
use std::sync::OnceLock;

use objc2::exception::catch;
use objc2::ffi::{class_getInstanceMethod, class_replaceMethod, method_getImplementation, method_getTypeEncoding};
use objc2::runtime::{AnyClass, AnyObject, Imp, MethodImplementation, Sel};
use objc2::sel;

type RemoveObserverForKeyPath =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject);

static ORIGINAL: OnceLock<Imp> = OnceLock::new();

/// Patches `WinitView`. Safe to call more than once, including across the
/// several `eframe::run_native` calls a hide-to-tray round trip makes;
/// only the first call does anything.
pub fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let class_name = CStr::from_bytes_with_nul(b"WinitView\0").unwrap();
    let Some(view_class) = AnyClass::get(class_name) else {
        // A winit version that renamed or restructured its view class:
        // nothing to patch here.
        return;
    };
    let selector = sel!(removeObserver:forKeyPath:);
    // SAFETY: `view_class` is a real, registered class and `selector` is a
    // valid selector; `class_getInstanceMethod` returns null rather than a
    // dangling pointer when the method isn't found.
    let method = unsafe { class_getInstanceMethod(view_class, selector) };
    if method.is_null() {
        return;
    }
    // SAFETY: `method` was just checked non-null and came straight from
    // `class_getInstanceMethod`.
    let (Some(original), types) =
        (unsafe { method_getImplementation(method) }, unsafe {
            method_getTypeEncoding(method)
        })
    else {
        return;
    };
    let _ = ORIGINAL.set(original);
    let replacement: RemoveObserverForKeyPath = swizzled_remove_observer;
    // SAFETY: `replacement` has the exact signature
    // `removeObserver:forKeyPath:` is declared with — `(id, SEL, id, id) ->
    // void` — and `types` is that same method's own encoding, so the
    // class' dispatch table stays internally consistent.
    unsafe {
        class_replaceMethod(
            (view_class as *const AnyClass).cast_mut(),
            selector,
            replacement.__imp(),
            types,
        );
    }
}

/// The one substring of the exception's reason that identifies this exact,
/// documented AppKit bug (rather than some other, genuinely unexpected KVO
/// misuse this same override might otherwise also see).
const KNOWN_BENIGN_MARKER: &str = "_NSTouchBarFinderObservation";

/// Runs the inherited implementation inside an Objective-C `@try`/`@catch`.
/// Only the one benign, known exception AppKit throws here is swallowed;
/// anything else is re-thrown so it still crashes and gets reported exactly
/// as it would have without this guard.
unsafe extern "C-unwind" fn swizzled_remove_observer(
    this: *mut AnyObject,
    cmd: Sel,
    observer: *mut AnyObject,
    key_path: *mut AnyObject,
) {
    let Some(original) = ORIGINAL.get().copied() else {
        return;
    };
    // SAFETY: `original` was obtained from the real
    // `removeObserver:forKeyPath:` method, whose signature this matches.
    let original: RemoveObserverForKeyPath = unsafe { std::mem::transmute(original) };
    let outcome = catch(std::panic::AssertUnwindSafe(|| unsafe {
        original(this, cmd, observer, key_path);
    }));
    let Err(exception) = outcome else { return };
    let Some(exception) = exception else {
        // A `nil` exception: nothing sensible to re-throw, and this is
        // documented as basically theoretical (OOM-adjacent).
        log::debug!("ignored a nil exception from an AppKit KVO cleanup call");
        return;
    };
    let reason = exception.to_string();
    if reason.contains(KNOWN_BENIGN_MARKER) {
        log::debug!("ignored a known-benign AppKit Touch Bar KVO cleanup exception on window close: {reason}");
    } else {
        // Some other, genuinely unexpected failure removing a KVO observer:
        // don't hide it behind this guard, let it crash and get reported
        // the same way it would have before this guard existed.
        objc2::exception::throw(exception);
    }
}
