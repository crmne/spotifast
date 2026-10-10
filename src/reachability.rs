//! OS network reachability monitoring.
//!
//! Event-driven path reachability tracking for network outage detection.

use crate::backend::Command;
use tokio::sync::mpsc::UnboundedSender;

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *mut std::ffi::c_void);
    }

    #[link(name = "SystemConfiguration", kind = "framework")]
    unsafe extern "C" {
        fn SCNetworkReachabilityCreateWithName(
            allocator: *const std::ffi::c_void,
            nodename: *const std::ffi::c_char,
        ) -> *mut std::ffi::c_void;
        fn SCNetworkReachabilitySetCallback(
            target: *mut std::ffi::c_void,
            callout: Option<extern "C" fn(*mut std::ffi::c_void, u32, *mut std::ffi::c_void)>,
            context: *mut SCNetworkReachabilityContext,
        ) -> bool;
        fn SCNetworkReachabilitySetDispatchQueue(
            target: *mut std::ffi::c_void,
            queue: *const std::ffi::c_void,
        ) -> bool;
        fn SCNetworkReachabilityGetFlags(target: *mut std::ffi::c_void, flags: *mut u32) -> bool;
        static _dispatch_main_q: [u8; 0];
    }

    #[repr(C)]
    struct SCNetworkReachabilityContext {
        version: isize,
        info: *mut std::ffi::c_void,
        retain: Option<unsafe extern "C" fn(*const std::ffi::c_void) -> *const std::ffi::c_void>,
        release: Option<unsafe extern "C" fn(*const std::ffi::c_void)>,
        copy_description:
            Option<unsafe extern "C" fn(*const std::ffi::c_void) -> *const std::ffi::c_void>,
    }

    const K_SC_NETWORK_REACHABILITY_FLAGS_REACHABLE: u32 = 1 << 1;
    const K_SC_NETWORK_REACHABILITY_FLAGS_CONNECTION_REQUIRED: u32 = 1 << 2;

    extern "C" fn reachability_callback(
        _target: *mut std::ffi::c_void,
        flags: u32,
        info: *mut std::ffi::c_void,
    ) {
        if !info.is_null() {
            let tx = unsafe { &*(info as *const UnboundedSender<Command>) };
            let reachable = (flags & K_SC_NETWORK_REACHABILITY_FLAGS_REACHABLE != 0)
                && (flags & K_SC_NETWORK_REACHABILITY_FLAGS_CONNECTION_REQUIRED == 0);
            let _ = tx.send(Command::NetworkStatus(reachable));
        }
    }

    unsafe extern "C" fn release_context(info: *const std::ffi::c_void) {
        if !info.is_null() {
            drop(unsafe { Box::from_raw(info as *mut UnboundedSender<Command>) });
        }
    }

    pub struct ReachabilityGuard {
        target: *mut std::ffi::c_void,
    }

    unsafe impl Send for ReachabilityGuard {}
    unsafe impl Sync for ReachabilityGuard {}

    impl Drop for ReachabilityGuard {
        fn drop(&mut self) {
            unsafe {
                SCNetworkReachabilitySetDispatchQueue(self.target, std::ptr::null());
                SCNetworkReachabilitySetCallback(self.target, None, std::ptr::null_mut());
                CFRelease(self.target);
            }
        }
    }

    pub fn watch(commands: UnboundedSender<Command>) -> Option<ReachabilityGuard> {
        let name = std::ffi::CString::new("apresolve.spotify.com").ok()?;
        unsafe {
            let target = SCNetworkReachabilityCreateWithName(std::ptr::null(), name.as_ptr());
            if target.is_null() {
                return None;
            }
            let info = Box::into_raw(Box::new(commands.clone()));
            let mut context = SCNetworkReachabilityContext {
                version: 0,
                info: info as *mut std::ffi::c_void,
                retain: None,
                release: Some(release_context),
                copy_description: None,
            };
            if !SCNetworkReachabilitySetCallback(target, Some(reachability_callback), &mut context)
            {
                drop(Box::from_raw(info));
                CFRelease(target);
                return None;
            }
            if !SCNetworkReachabilitySetDispatchQueue(
                target,
                &_dispatch_main_q as *const _ as *const std::ffi::c_void,
            ) {
                SCNetworkReachabilitySetCallback(target, None, std::ptr::null_mut());
                CFRelease(target);
                return None;
            }
            let mut flags: u32 = 0;
            if SCNetworkReachabilityGetFlags(target, &mut flags) {
                let reachable = (flags & K_SC_NETWORK_REACHABILITY_FLAGS_REACHABLE != 0)
                    && (flags & K_SC_NETWORK_REACHABILITY_FLAGS_CONNECTION_REQUIRED == 0);
                let _ = commands.send(Command::NetworkStatus(reachable));
            }
            Some(ReachabilityGuard { target })
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod fallback {
    use super::*;

    pub struct ReachabilityGuard;

    pub fn watch(_commands: UnboundedSender<Command>) -> Option<ReachabilityGuard> {
        None
    }
}

#[cfg(target_os = "macos")]
pub use macos::{ReachabilityGuard, watch};

#[cfg(not(target_os = "macos"))]
pub use fallback::{ReachabilityGuard, watch};
