//! What the app knows of the jam, built from the network's events.
//!
//! The interface is optimistic: a song added to the jam has its row at
//! once, and a removed song loses it at once. The host confirms a moment
//! later; until then a state that does not show the change yet is the
//! host's past, not a reason to undo what the listener did.

use super::invite::Invite;
use super::net::{EndReason, JamEvent};
use super::protocol::{ClientMsg, HOST_ID, ItemId, JamItem, JamState, ParticipantId, Refusal};
use super::session::JamClock;

/// How long an addition may wait for the host before it is given up.
const PENDING_TIMEOUT_MS: u64 = 15_000;

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

/// A song this computer added that the host has not shown yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingAdd {
    pub item: JamItem,
    /// Copies of this song by this participant the host must show before
    /// this one counts as there: two quick adds of a song are two rows.
    baseline: usize,
    sent_ms: u64,
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
    /// Songs added here, in order, until the host shows them.
    pub pending: Vec<PendingAdd>,
    /// Rows removed here, until the host drops them.
    pub hidden: Vec<ItemId>,
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

    /// Hosting, or a guest let in, even while reconnecting: playback
    /// belongs to the jam.
    pub fn in_session(&self) -> bool {
        matches!(
            self.status,
            JamStatus::Hosting | JamStatus::Joined | JamStatus::Reconnecting(_)
        )
    }

    /// The host's clock now, once known.
    pub fn host_now_ms(&self) -> Option<u64> {
        let clock = self.clock?;
        Some(clock.now_ms().saturating_add_signed(self.clock_offset?))
    }

    /// Shows a song as added and returns the request that adds it.
    pub fn add(
        &mut self,
        uri: String,
        title: String,
        artists: String,
        duration_ms: u32,
    ) -> ClientMsg {
        let baseline = self.copies_shown(&uri)
            + self
                .pending
                .iter()
                .filter(|pending| pending.item.uri == uri)
                .count();
        self.pending.push(PendingAdd {
            item: JamItem {
                id: 0,
                uri: uri.clone(),
                title: title.clone(),
                artists: artists.clone(),
                duration_ms,
                added_by: self.you.unwrap_or(HOST_ID),
            },
            baseline,
            sent_ms: self.now_ms(),
        });
        ClientMsg::Add {
            uri,
            title,
            artists,
            duration_ms,
        }
    }

    /// Hides a row and returns the request that removes it.
    pub fn remove(&mut self, item: ItemId) -> ClientMsg {
        if !self.hidden.contains(&item) {
            self.hidden.push(item);
        }
        ClientMsg::Remove { item }
    }

    /// The additions to send again after a reconnection: the host dropped
    /// whatever arrived while the guest was away, and knows it by a new id.
    pub fn resend(&mut self) -> Vec<ClientMsg> {
        let now = self.now_ms();
        let you = self.you.unwrap_or(HOST_ID);
        let mut messages = Vec::new();
        for index in 0..self.pending.len() {
            let uri = self.pending[index].item.uri.clone();
            let earlier = self.pending[..index]
                .iter()
                .filter(|pending| pending.item.uri == uri)
                .count();
            let pending = &mut self.pending[index];
            pending.baseline = earlier;
            pending.sent_ms = now;
            pending.item.added_by = you;
            messages.push(ClientMsg::Add {
                uri,
                title: pending.item.title.clone(),
                artists: pending.item.artists.clone(),
                duration_ms: pending.item.duration_ms,
            });
        }
        messages
    }

    /// Gives up additions the host never showed. While reconnecting they
    /// wait, to be sent again. Returns whether any was given up.
    pub fn expire(&mut self) -> bool {
        if matches!(self.status, JamStatus::Reconnecting(_)) {
            return false;
        }
        let now = self.now_ms();
        let before = self.pending.len();
        self.pending
            .retain(|pending| now.saturating_sub(pending.sent_ms) < PENDING_TIMEOUT_MS);
        self.pending.len() != before
    }

    /// The queue as shown: the host's rows, less those removed here.
    pub fn shown_queue(&self) -> impl Iterator<Item = &JamItem> {
        self.state
            .iter()
            .flat_map(|state| state.queue.iter())
            .filter(|item| !self.hidden.contains(&item.id))
    }

    /// Whether this participant may remove `item`: the host may remove
    /// anything, guests their own songs, or any once the host allows it.
    pub fn can_remove(&self, item: &JamItem) -> bool {
        let you = self.you;
        you == Some(HOST_ID)
            || you == Some(item.added_by)
            || self
                .state
                .as_ref()
                .is_some_and(|state| state.permissions.guests_control_playback)
    }

    pub fn name_of(&self, participant: ParticipantId) -> Option<&str> {
        self.state.as_ref().and_then(|state| {
            state
                .participants
                .iter()
                .find(|known| known.id == participant)
                .map(|known| known.name.as_str())
        })
    }

    fn now_ms(&self) -> u64 {
        self.clock.map_or(0, |clock| clock.now_ms())
    }

    /// Copies of `uri` this participant has in the jam, the playing one
    /// included.
    fn copies_shown(&self, uri: &str) -> usize {
        let Some(state) = &self.state else {
            return 0;
        };
        state
            .current
            .iter()
            .chain(&state.queue)
            .filter(|item| item.uri == uri && Some(item.added_by) == self.you)
            .count()
    }

    /// Drops what the host's latest state now shows.
    fn settle(&mut self) {
        let shown: Vec<usize> = self
            .pending
            .iter()
            .map(|pending| self.copies_shown(&pending.item.uri))
            .collect();
        let mut kept = shown.iter();
        self.pending
            .retain(|pending| kept.next().is_none_or(|&shown| shown <= pending.baseline));
        if let Some(state) = &self.state {
            self.hidden
                .retain(|id| state.queue.iter().any(|item| item.id == *id));
        }
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
                    self.settle();
                }
            }
            JamEvent::Refused(refusal) => {
                match refusal {
                    // Only an addition is refused for room: the latest one.
                    Refusal::QueueFull | Refusal::QuotaReached => {
                        self.pending.pop();
                    }
                    Refusal::NotAllowed | Refusal::NotFound => self.hidden.clear(),
                    Refusal::Unexpected => {}
                }
                self.refusal = Some(refusal);
            }
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

    const SONG: &str = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa";

    fn item(id: ItemId, added_by: ParticipantId) -> JamItem {
        JamItem {
            id,
            uri: SONG.into(),
            title: "Song".into(),
            artists: "Artist".into(),
            duration_ms: 1000,
            added_by,
        }
    }

    fn with_queue(seq: u64, queue: Vec<JamItem>) -> JamEvent {
        let mut state = state(seq);
        state.queue = queue;
        JamEvent::State(state)
    }

    fn guest() -> JamView {
        let mut view = JamView::default();
        view.start(JamClock::new());
        view.apply(JamEvent::Joined { you: 3 });
        view.apply(with_queue(1, vec![item(1, HOST_ID)]));
        view
    }

    fn add(view: &mut JamView) -> ClientMsg {
        view.add(SONG.into(), "Song".into(), "Artist".into(), 1000)
    }

    #[test]
    fn an_added_song_shows_until_the_host_does_even_twice() {
        let mut view = guest();
        assert!(matches!(add(&mut view), ClientMsg::Add { .. }));
        add(&mut view);
        assert_eq!(view.pending.len(), 2);
        // The host's copy of the song is not this guest's.
        view.apply(with_queue(2, vec![item(1, HOST_ID)]));
        assert_eq!(view.pending.len(), 2, "an older state must not drop them");
        view.apply(with_queue(3, vec![item(1, HOST_ID), item(2, 3)]));
        assert_eq!(
            view.pending.len(),
            1,
            "one copy arrived, one is still on its way"
        );
        view.apply(with_queue(
            4,
            vec![item(1, HOST_ID), item(2, 3), item(3, 3)],
        ));
        assert!(view.pending.is_empty());
    }

    #[test]
    fn a_refused_addition_or_removal_is_undone() {
        let mut view = guest();
        add(&mut view);
        add(&mut view);
        view.apply(JamEvent::Refused(Refusal::QuotaReached));
        assert_eq!(view.pending.len(), 1);
        assert_eq!(view.remove(1), ClientMsg::Remove { item: 1 });
        assert_eq!(view.shown_queue().count(), 0);
        view.apply(JamEvent::Refused(Refusal::NotAllowed));
        assert_eq!(view.shown_queue().count(), 1);
        assert_eq!(view.refusal, Some(Refusal::NotAllowed));
    }

    #[test]
    fn a_removed_row_stays_hidden_until_the_host_drops_it() {
        let mut view = guest();
        view.remove(1);
        view.apply(with_queue(2, vec![item(1, HOST_ID), item(2, HOST_ID)]));
        assert_eq!(
            view.shown_queue().map(|item| item.id).collect::<Vec<_>>(),
            [2]
        );
        view.apply(with_queue(3, vec![item(2, HOST_ID)]));
        assert!(view.hidden.is_empty());
    }

    #[test]
    fn additions_wait_through_a_reconnection_and_are_sent_again() {
        let mut view = guest();
        add(&mut view);
        view.apply(JamEvent::Reconnecting { attempt: 1 });
        for pending in &mut view.pending {
            pending.sent_ms = 0;
        }
        assert!(!view.expire(), "nothing expires while reconnecting");
        view.apply(JamEvent::Joined { you: 9 });
        let resent = view.resend();
        assert!(matches!(resent[..], [ClientMsg::Add { .. }]));
        assert_eq!(view.pending[0].item.added_by, 9);
        assert!(!view.expire(), "sent again just now");
        view.pending[0].sent_ms = 0;
        if view.now_ms() >= PENDING_TIMEOUT_MS {
            assert!(view.expire());
        }
    }

    #[test]
    fn who_may_remove_a_row() {
        let mut view = guest();
        assert!(!view.can_remove(&item(1, HOST_ID)));
        assert!(view.can_remove(&item(2, 3)));
        let mut state = state(5);
        state.permissions.guests_control_playback = true;
        view.apply(JamEvent::State(state));
        assert!(view.can_remove(&item(1, HOST_ID)));
        assert_eq!(view.name_of(HOST_ID), Some("Host"));
    }
}
