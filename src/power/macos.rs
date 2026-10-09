//! macOS: Low Power Mode.
//!
//! `NSProcessInfo.isLowPowerModeEnabled` is read in the logic pass rather
//! than followed through its notification: one message send, and no
//! observer object to keep. Sleep and wake from NSWorkspace are not followed
//! yet.
//! <https://developer.apple.com/documentation/foundation/processinfo/islowpowermodeenabled>

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, sel};

use super::Shared;

/// Nothing to register on macOS: [`refresh`] reads what is needed.
pub(super) struct Reader;

impl Reader {
    pub(super) fn start(_shared: Arc<Shared>) -> Result<Self, String> {
        Ok(Self)
    }
}

/// Reads Low Power Mode. It arrived in macOS 12 and the app runs from 11,
/// where asking would raise an exception, so the selector is checked first.
pub(super) fn refresh(shared: &Shared) {
    let Some(class) = AnyClass::get(c"NSProcessInfo") else {
        return;
    };
    // SAFETY: `processInfo` takes no arguments and returns the process's
    // shared NSProcessInfo.
    let info: Option<Retained<AnyObject>> = unsafe { msg_send![class, processInfo] };
    let Some(info) = info else {
        return;
    };
    // SAFETY: `respondsToSelector:` takes a selector and returns a BOOL.
    let available: bool =
        unsafe { msg_send![&*info, respondsToSelector: sel!(isLowPowerModeEnabled)] };
    if !available {
        return;
    }
    // SAFETY: checked just above that the object answers this selector,
    // which takes no arguments and returns a BOOL.
    let saver: bool = unsafe { msg_send![&*info, isLowPowerModeEnabled] };
    shared.update(|conditions| conditions.saver = saver);
}
