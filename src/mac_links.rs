//! Spotify links macOS hands the app.
//!
//! An app registered for the `spotify` scheme (`CFBundleURLTypes` in
//! `packaging/macos/Info.plist`) gets each link as an Apple Event, never on
//! the command line: the one it was launched for as well as the ones that
//! arrive while it runs. The handler here puts each on the running
//! instance's command queue, where it is treated like a link from any
//! other launch.

use std::sync::{Arc, Mutex};

use crate::backend::Waker;
use crate::single_instance::ControlCommand;

/// Answers the desktop's links for the rest of the process. Main thread
/// only, before the event loop that delivers Apple Events starts.
pub fn install(commands: Arc<Mutex<Vec<ControlCommand>>>, waker: Waker) {
    mac_impl::install(commands, waker);
}

mod mac_impl {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use objc2::rc::Retained;
    use objc2::{MainThreadOnly, define_class, msg_send, sel};
    use objc2_foundation::{
        MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSObject,
    };

    use crate::backend::Waker;
    use crate::single_instance::ControlCommand;

    /// `kInternetEventClass` and `kAEGetURL`: both the four characters
    /// `GURL`, the Apple Event a URL open is.
    const GET_URL: u32 = u32::from_be_bytes(*b"GURL");
    /// `keyDirectObject`, the parameter that carries the URL.
    const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

    /// The queue a link is pushed onto, and the wake that makes the app
    /// read it.
    type Sink = (Arc<Mutex<Vec<ControlCommand>>>, Waker);

    /// Where links go, and the wake that makes the app read them.
    static SINK: Mutex<Option<Sink>> = Mutex::new(None);

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "SpotizgegLinkHandler"]
        pub struct SpotizgegLinkHandler;

        impl SpotizgegLinkHandler {
            #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
            fn handle_get_url(
                &self,
                event: &NSAppleEventDescriptor,
                _reply: &NSAppleEventDescriptor,
            ) {
                let url: Option<Retained<NSAppleEventDescriptor>> =
                    unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
                let Some(text) = url.and_then(|url| url.stringValue()) else {
                    return;
                };
                deliver(&text.to_string());
            }
        }
    );

    fn deliver(text: &str) {
        let Some(uri) = crate::link::parse(text) else {
            log::warn!("not a Spotify link: {text}");
            return;
        };
        let Ok(sink) = SINK.lock() else {
            return;
        };
        if let Some((commands, waker)) = sink.as_ref() {
            commands
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(ControlCommand::OpenLink(uri));
            waker.wake();
        }
    }

    pub fn install(commands: Arc<Mutex<Vec<ControlCommand>>>, waker: Waker) {
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("links can only be taken on the main thread");
            return;
        };
        if let Ok(mut sink) = SINK.lock() {
            *sink = Some((commands, waker));
        }
        static INSTALLED: AtomicBool = AtomicBool::new(false);
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        let handler: Retained<SpotizgegLinkHandler> =
            unsafe { msg_send![mtm.alloc::<SpotizgegLinkHandler>(), init] };
        let target: &NSObject = &handler;
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // The typed binding for this call wants the Core Services crate for
        // two four-character codes; the message itself is two integers.
        unsafe {
            let _: () = msg_send![
                &*manager,
                setEventHandler: target,
                andSelector: sel!(handleGetURLEvent:withReplyEvent:),
                forEventClass: GET_URL,
                andEventID: GET_URL,
            ];
        }
        // The manager does not retain its handler, and this one answers for
        // as long as the process runs.
        std::mem::forget(handler);
    }
}
