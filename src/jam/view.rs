//! What the app knows of the jam, built from the connection's events.
//!
//! The interface is optimistic: a song added to the jam has its row at
//! once, and a removed song loses it at once. The server confirms a moment
//! later; until then a state that does not show the change yet is the
//! server's past, not a reason to undo what the listener did.

use jam_core::client::{EndReason, JamEvent};
use jam_core::protocol::{ClientMsg, ItemId, JamItem, JamState, ParticipantId, Refusal};
use jam_core::session::JamClock;

/// How long an addition may wait for the server before it is given up.
const PENDING_TIMEOUT_MS: u64 = 15_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JamStatus {
    #[default]
    Off,
    /// Joining was asked for and has not happened yet.
    Starting,
    Joined,
    Reconnecting(u32),
}

/// A song this computer added that the server has not shown yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingAdd {
    pub item: JamItem,
    /// Copies of this song by this listener the server must show before
    /// this one counts as there: two quick adds of a song are two rows.
    baseline: usize,
    sent_ms: u64,
}

#[derive(Debug, Default)]
pub struct JamView {
    pub status: JamStatus,
    /// The clock the connection was started with.
    pub clock: Option<JamClock>,
    pub you: Option<ParticipantId>,
    pub state: Option<JamState>,
    /// The server's clock minus `clock`.
    pub clock_offset: Option<i64>,
    /// The last request the server refused, until the next state.
    pub refusal: Option<Refusal>,
    /// Why the last jam ended.
    pub ended: Option<EndReason>,
    /// Songs added here, in order, until the server shows them.
    pub pending: Vec<PendingAdd>,
    /// Rows removed here, until the server drops them.
    pub hidden: Vec<ItemId>,
}

impl JamView {
    /// Joining has started with `clock`; whatever was shown is dropped.
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

    /// Let in, even while reconnecting: playback belongs to the jam.
    pub fn in_session(&self) -> bool {
        matches!(self.status, JamStatus::Joined | JamStatus::Reconnecting(_))
    }

    /// The server's clock now, once known.
    pub fn server_now_ms(&self) -> Option<u64> {
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
                added_by: self.you.unwrap_or_default(),
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

    /// The additions to send again after a reconnection: the server dropped
    /// whatever arrived while the listener was away, and knows it by a new
    /// id.
    pub fn resend(&mut self) -> Vec<ClientMsg> {
        let now = self.now_ms();
        let you = self.you.unwrap_or_default();
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

    /// Gives up additions the server never showed. While reconnecting they
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

    /// The queue as shown: the server's rows, less those removed here.
    pub fn shown_queue(&self) -> impl Iterator<Item = &JamItem> {
        self.state
            .iter()
            .flat_map(|state| state.queue.iter())
            .filter(|item| !self.hidden.contains(&item.id))
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

    pub fn apply(&mut self, event: JamEvent) {
        if !self.active() {
            return;
        }
        match event {
            JamEvent::Joined { you } => {
                self.status = JamStatus::Joined;
                self.you = Some(you);
                // A reconnection may land on a restarted server: start over.
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
                    Refusal::NotFound => self.hidden.clear(),
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

    fn now_ms(&self) -> u64 {
        self.clock.map_or(0, |clock| clock.now_ms())
    }

    /// Copies of `uri` this listener has in the jam, the playing one
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

    /// Drops what the server's latest state now shows.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use jam_core::session::JamSession;

    const SONG: &str = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa";

    fn state(seq: u64) -> JamState {
        let mut state = JamSession::new(0).state(0);
        state.seq = seq;
        state.participants = vec![jam_core::protocol::Participant {
            id: 1,
            name: "Ana".into(),
        }];
        state
    }

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

    /// Listener 3, in a jam where listener 1 queued a song.
    fn listener() -> JamView {
        let mut view = JamView::default();
        view.start(JamClock::new());
        view.apply(JamEvent::Joined { you: 3 });
        view.apply(with_queue(1, vec![item(1, 1)]));
        view
    }

    fn add(view: &mut JamView) -> ClientMsg {
        view.add(SONG.into(), "Song".into(), "Artist".into(), 1000)
    }

    #[test]
    fn events_before_a_start_or_after_the_end_are_ignored() {
        let mut view = JamView::default();
        view.apply(JamEvent::State(state(1)));
        assert!(view.state.is_none());
        view.start(JamClock::new());
        view.apply(JamEvent::Ended(EndReason::Left));
        assert_eq!(view.status, JamStatus::Off);
        assert_eq!(view.ended, Some(EndReason::Left));
        view.apply(JamEvent::State(state(2)));
        assert!(view.state.is_none(), "a late state must not revive the jam");
    }

    #[test]
    fn a_listener_keeps_the_newest_state_through_a_reconnection() {
        let mut view = JamView::default();
        view.start(JamClock::new());
        assert_eq!(view.server_now_ms(), None, "no clock sample yet");
        view.apply(JamEvent::Joined { you: 3 });
        assert!(view.in_session());
        view.apply(JamEvent::ClockOffset(0));
        assert!(view.server_now_ms().is_some());
        view.apply(JamEvent::State(state(5)));
        view.apply(JamEvent::State(state(4)));
        assert_eq!(view.state.as_ref().map(|state| state.seq), Some(5));
        view.apply(JamEvent::Refused(Refusal::NotFound));
        view.apply(JamEvent::State(state(6)));
        assert_eq!(view.refusal, None, "a new state clears the refusal");
        view.apply(JamEvent::Reconnecting { attempt: 2 });
        assert_eq!(view.status, JamStatus::Reconnecting(2));
        assert!(view.in_session());
        view.apply(JamEvent::Joined { you: 7 });
        view.apply(JamEvent::State(state(1)));
        assert_eq!(
            view.state.as_ref().map(|state| state.seq),
            Some(1),
            "a restarted server counts from the start again"
        );
        assert_eq!(view.you, Some(7));
    }

    #[test]
    fn an_added_song_shows_until_the_server_does_even_twice() {
        let mut view = listener();
        assert!(matches!(add(&mut view), ClientMsg::Add { .. }));
        add(&mut view);
        assert_eq!(view.pending.len(), 2);
        // Listener 1's copy of the song is not this listener's.
        view.apply(with_queue(2, vec![item(1, 1)]));
        assert_eq!(view.pending.len(), 2, "an older state must not drop them");
        view.apply(with_queue(3, vec![item(1, 1), item(2, 3)]));
        assert_eq!(view.pending.len(), 1, "one copy arrived, one is on its way");
        view.apply(with_queue(4, vec![item(1, 1), item(2, 3), item(3, 3)]));
        assert!(view.pending.is_empty());
    }

    #[test]
    fn a_refused_addition_or_removal_is_undone() {
        let mut view = listener();
        add(&mut view);
        add(&mut view);
        view.apply(JamEvent::Refused(Refusal::QuotaReached));
        assert_eq!(view.pending.len(), 1);
        assert_eq!(view.remove(1), ClientMsg::Remove { item: 1 });
        assert_eq!(view.shown_queue().count(), 0);
        view.apply(JamEvent::Refused(Refusal::NotFound));
        assert_eq!(view.shown_queue().count(), 1);
        assert_eq!(view.refusal, Some(Refusal::NotFound));
    }

    #[test]
    fn a_removed_row_stays_hidden_until_the_server_drops_it() {
        let mut view = listener();
        view.remove(1);
        view.apply(with_queue(2, vec![item(1, 1), item(2, 1)]));
        assert_eq!(
            view.shown_queue().map(|item| item.id).collect::<Vec<_>>(),
            [2]
        );
        view.apply(with_queue(3, vec![item(2, 1)]));
        assert!(view.hidden.is_empty());
        assert_eq!(view.name_of(1), Some("Ana"));
    }

    #[test]
    fn additions_wait_through_a_reconnection_and_are_sent_again() {
        let mut view = listener();
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
    }
}
