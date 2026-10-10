//! Keep local playback out of App Nap while another app has focus.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

use crate::player::{LocalState, Playback};

/// Owned by the main-thread app, including while its window is covered or hidden.
#[derive(Default)]
pub(crate) struct PlaybackActivity {
    token: Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

impl PlaybackActivity {
    pub(crate) fn sync(&mut self, local: &LocalState) {
        let active = local.connected
            && local.error.is_none()
            && (local.loading || matches!(local.playback, Playback::Playing | Playback::Loading));
        if active && self.token.is_none() {
            self.token = Some(
                NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                    NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                    &NSString::from_str("Spotify playback"),
                ),
            );
        } else if !active {
            self.end();
        }
    }

    fn end(&mut self) {
        if let Some(token) = self.token.take() {
            // SAFETY: this token came from beginActivityWithOptions:reason:
            // and is ended exactly once, before its retained reference drops.
            unsafe { NSProcessInfo::processInfo().endActivity(&token) };
        }
    }
}

impl Drop for PlaybackActivity {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_playback_holds_one_activity_until_it_no_longer_needs_it() {
        let mut activity = PlaybackActivity::default();
        let mut local = LocalState {
            connected: true,
            playback: Playback::Playing,
            ..LocalState::default()
        };
        activity.sync(&local);
        let token = Retained::as_ptr(activity.token.as_ref().unwrap());
        activity.sync(&local);
        assert_eq!(Retained::as_ptr(activity.token.as_ref().unwrap()), token);

        local.loading = true;
        local.playback = Playback::Loading;
        activity.sync(&local);
        assert_eq!(Retained::as_ptr(activity.token.as_ref().unwrap()), token);

        local.loading = false;
        for playback in [Playback::Paused, Playback::Stopped] {
            local.playback = playback;
            activity.sync(&local);
            assert!(activity.token.is_none());
        }
        local.playback = Playback::Playing;
        activity.sync(&local);
        assert!(activity.token.is_some());
        local.error = Some("output unavailable".into());
        activity.sync(&local);
        assert!(activity.token.is_none());
        local.error = None;
        local.connected = false;
        activity.sync(&local);
        assert!(activity.token.is_none());
        activity.sync(&LocalState::default());
        assert!(activity.token.is_none());
    }
}
