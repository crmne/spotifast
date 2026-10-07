//! What the app knows of the jam, built from the network's events.

use super::invite::Invite;
use super::net::{EndReason, JamEvent};
use super::protocol::{HOST_ID, JamState, ParticipantId, Refusal};
use super::session::JamClock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JamStatus {
    #[default]
    Off,
    /// Hosting or joining was asked for and has not happened yet.
    Starting,
    Hosting,
    Joined,
    Reconnecting(u32),
}

#[derive(Debug, Default)]
pub struct JamView {
    pub status: JamStatus,
    /// The clock the network tasks were started with.
    pub clock: Option<JamClock>,
    /// While hosting: the code to share.
    pub invite: Option<Invite>,
    pub you: Option<ParticipantId>,
    pub state: Option<JamState>,
    /// The host's clock minus `clock`; zero for the host itself.
    pub clock_offset: Option<i64>,
    /// The last request the host refused, until the next state.
    pub refusal: Option<Refusal>,
    /// Why the last jam ended.
    pub ended: Option<EndReason>,
}

impl JamView {
    /// A jam is being started with `clock`; whatever was shown is dropped.
    pub fn start(&mut self, clock: JamClock) {
        *self = Self {
            status: JamStatus::Starting,
            clock: Some(clock),
            ..Self::default()
        };
    }

    pub fn active(&self) -> bool {
        self.status != JamStatus::Off
    }

    /// The host's clock now, once known.
    pub fn host_now_ms(&self) -> Option<u64> {
        let clock = self.clock?;
        Some(clock.now_ms().saturating_add_signed(self.clock_offset?))
    }

    pub fn apply(&mut self, event: JamEvent) {
        if !self.active() {
            return;
        }
        match event {
            JamEvent::Hosting { invite } => {
                self.status = JamStatus::Hosting;
                self.invite = Some(invite);
                self.you = Some(HOST_ID);
                self.clock_offset = Some(0);
            }
            JamEvent::Joined { you } => {
                self.status = JamStatus::Joined;
                self.you = Some(you);
                // A reconnection may land on a restarted host: start over.
                self.state = None;
            }
            JamEvent::State(state) => {
                if self.state.as_ref().is_none_or(|seen| seen.seq < state.seq) {
                    self.state = Some(state);
                    self.refusal = None;
                }
            }
            JamEvent::Refused(refusal) => self.refusal = Some(refusal),
            JamEvent::ClockOffset(offset) => self.clock_offset = Some(offset),
            JamEvent::Reconnecting { attempt } => {
                self.status = JamStatus::Reconnecting(attempt);
            }
            JamEvent::Ended(reason) => {
                *self = Self {
                    ended: Some(reason),
                    ..Self::default()
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jam::invite::Secret;
    use crate::jam::session::HostSession;

    fn state(seq: u64) -> JamState {
        let mut state = HostSession::new("Host", 0).state(0);
        state.seq = seq;
        state
    }

    #[test]
    fn events_before_a_start_or_after_the_end_are_ignored() {
        let mut view = JamView::default();
        view.apply(JamEvent::State(state(1)));
        assert!(view.state.is_none());
        view.start(JamClock::new());
        view.apply(JamEvent::Ended(EndReason::HostClosed));
        assert_eq!(view.status, JamStatus::Off);
        assert_eq!(view.ended, Some(EndReason::HostClosed));
        view.apply(JamEvent::State(state(2)));
        assert!(view.state.is_none(), "a late state must not revive the jam");
    }

    #[test]
    fn hosting_shows_the_invite_and_runs_on_its_own_clock() {
        let mut view = JamView::default();
        view.start(JamClock::new());
        let invite = Invite {
            host: "127.0.0.1".into(),
            port: 4070,
            secret: Secret::generate(),
        };
        view.apply(JamEvent::Hosting {
            invite: invite.clone(),
        });
        assert_eq!(view.status, JamStatus::Hosting);
        assert_eq!(view.invite, Some(invite));
        assert_eq!(view.you, Some(HOST_ID));
        assert!(view.host_now_ms().is_some());
    }

    #[test]
    fn a_guest_keeps_the_newest_state_through_a_reconnection() {
        let mut view = JamView::default();
        view.start(JamClock::new());
        assert_eq!(view.host_now_ms(), None, "no clock sample yet");
        view.apply(JamEvent::Joined { you: 3 });
        view.apply(JamEvent::State(state(5)));
        view.apply(JamEvent::State(state(4)));
        assert_eq!(view.state.as_ref().map(|state| state.seq), Some(5));
        view.apply(JamEvent::Refused(Refusal::NotAllowed));
        view.apply(JamEvent::State(state(6)));
        assert_eq!(view.refusal, None, "a new state clears the refusal");
        view.apply(JamEvent::Reconnecting { attempt: 2 });
        assert_eq!(view.status, JamStatus::Reconnecting(2));
        view.apply(JamEvent::Joined { you: 7 });
        view.apply(JamEvent::State(state(1)));
        assert_eq!(
            view.state.as_ref().map(|state| state.seq),
            Some(1),
            "a restarted host counts from the start again"
        );
        assert_eq!(view.you, Some(7));
    }
}
