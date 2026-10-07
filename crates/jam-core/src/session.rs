//! The jam's state machines, free of sockets and of the player.
//!
//! The server holds a [`JamSession`]: it admits listeners, applies their
//! requests, and moves on when a song ends. Every listener has the same
//! rights. Each listener then [`reconcile`]s the state it last saw with its
//! own local playback, which yields the few player corrections needed to
//! follow along.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::auth::{self, Secret};
use crate::protocol::{
    ClientMsg, ItemId, JamItem, JamState, MAX_NAME_CHARS, MAX_PARTICIPANTS, MAX_QUEUE,
    MAX_QUEUED_PER_LISTENER, PROTOCOL_VERSION, Participant, ParticipantId, Refusal, Rejection,
    clean_text,
};

/// Below this, a listener's position is left alone: librespot cannot change
/// speed, so every correction is an audible seek.
pub const DRIFT_TOLERANCE_MS: u32 = 750;
/// Clock samples kept; their median resists one slow round trip.
const CLOCK_SAMPLES: usize = 5;
/// A round trip slower than this says little about either clock.
const MAX_ROUND_TRIP_MS: u64 = 5_000;

/// The authoritative jam, on the server. Times are milliseconds on the
/// server's own clock.
#[derive(Debug)]
pub struct JamSession {
    seq: u64,
    current: Option<JamItem>,
    queue: Vec<JamItem>,
    playing: bool,
    /// Position in `current` at `anchor_ms`.
    position_ms: u32,
    anchor_ms: u64,
    participants: Vec<Participant>,
    next_item: ItemId,
    next_participant: ParticipantId,
}

/// What outlives a server restart: the songs and where the playing one was.
/// Nobody is listening after a restart, so it comes back paused.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub current: Option<JamItem>,
    pub queue: Vec<JamItem>,
    pub position_ms: u32,
    pub next_item: ItemId,
}

impl JamSession {
    pub fn new(now_ms: u64) -> Self {
        Self::restore(Saved::default(), now_ms)
    }

    pub fn restore(saved: Saved, now_ms: u64) -> Self {
        let highest = saved
            .current
            .iter()
            .chain(&saved.queue)
            .map(|item| item.id)
            .max()
            .unwrap_or(0);
        let mut queue = saved.queue;
        queue.truncate(MAX_QUEUE);
        Self {
            seq: 0,
            current: saved.current,
            queue,
            playing: false,
            position_ms: saved.position_ms,
            anchor_ms: now_ms,
            participants: Vec::new(),
            next_item: saved.next_item.max(highest + 1),
            next_participant: 1,
        }
    }

    pub fn save(&self, now_ms: u64) -> Saved {
        Saved {
            current: self.current.clone(),
            queue: self.queue.clone(),
            position_ms: self.position_at(now_ms),
            next_item: self.next_item,
        }
    }

    /// Lets a listener in when its [`ClientMsg::Hello`] answers this
    /// connection's `nonce` with the server's password.
    pub fn admit(
        &mut self,
        hello: &ClientMsg,
        secret: &Secret,
        nonce: &str,
        binding: &[u8],
    ) -> Result<ParticipantId, Rejection> {
        let ClientMsg::Hello {
            version,
            name,
            proof,
        } = hello
        else {
            return Err(Rejection::BadPassword);
        };
        if *version != PROTOCOL_VERSION {
            return Err(Rejection::IncompatibleVersion);
        }
        if !auth::verify(secret, nonce, name, binding, proof) {
            return Err(Rejection::BadPassword);
        }
        if self.participants.len() >= MAX_PARTICIPANTS {
            return Err(Rejection::Full);
        }
        let id = self.next_participant;
        self.next_participant += 1;
        let name = self.unique_name(name);
        self.participants.push(Participant { id, name });
        self.changed();
        Ok(id)
    }

    /// A listener left or was disconnected. Its songs stay in the queue.
    /// With nobody left, the jam pauses where it was. Returns whether the
    /// listener was still in the jam.
    pub fn leave(&mut self, id: ParticipantId, now_ms: u64) -> bool {
        let before = self.participants.len();
        self.participants.retain(|participant| participant.id != id);
        let left = self.participants.len() != before;
        if left {
            if self.participants.is_empty() && self.playing {
                self.position_ms = self.position_at(now_ms);
                self.anchor_ms = now_ms;
                self.playing = false;
            }
            self.changed();
        }
        left
    }

    /// Applies one request from a listener. On a refusal nothing changes.
    pub fn apply(
        &mut self,
        from: ParticipantId,
        message: ClientMsg,
        now_ms: u64,
    ) -> Result<(), Refusal> {
        match message {
            ClientMsg::Hello { .. } | ClientMsg::Ping { .. } => return Err(Refusal::Unexpected),
            ClientMsg::Add {
                uri,
                title,
                artists,
                duration_ms,
            } => {
                if self.queue.len() >= MAX_QUEUE {
                    return Err(Refusal::QueueFull);
                }
                if self
                    .queue
                    .iter()
                    .filter(|item| item.added_by == from)
                    .count()
                    >= MAX_QUEUED_PER_LISTENER
                {
                    return Err(Refusal::QuotaReached);
                }
                let item = JamItem {
                    id: self.next_item,
                    uri,
                    title,
                    artists,
                    duration_ms,
                    added_by: from,
                };
                self.next_item += 1;
                if self.current.is_none() {
                    self.current = Some(item);
                    self.position_ms = 0;
                    self.anchor_ms = now_ms;
                } else {
                    self.queue.push(item);
                }
            }
            ClientMsg::Remove { item } => {
                let index = self.queue_index(item)?;
                self.queue.remove(index);
            }
            ClientMsg::Move { item, to } => {
                let index = self.queue_index(item)?;
                let moved = self.queue.remove(index);
                self.queue.insert(to.min(self.queue.len()), moved);
            }
            ClientMsg::Skip => {
                if self.current.is_none() {
                    return Err(Refusal::NotFound);
                }
                self.advance(now_ms);
                return Ok(());
            }
            ClientMsg::Ended { item } => {
                // Every listener reports the same ending; the first moves
                // the jam on and the rest name a song that already left.
                if self
                    .current
                    .as_ref()
                    .is_none_or(|current| current.id != item)
                {
                    return Err(Refusal::NotFound);
                }
                self.advance(now_ms);
                return Ok(());
            }
            ClientMsg::SetPlaying { playing } => {
                if self.current.is_none() {
                    return Err(Refusal::NotFound);
                }
                self.position_ms = self.position_at(now_ms);
                self.anchor_ms = now_ms;
                self.playing = playing;
            }
            ClientMsg::Seek { position_ms } => {
                let Some(current) = &self.current else {
                    return Err(Refusal::NotFound);
                };
                self.position_ms = clamp_to(position_ms, current.duration_ms);
                self.anchor_ms = now_ms;
            }
        }
        self.changed();
        Ok(())
    }

    /// Moves on once the playing song has run its known length. Returns
    /// whether the state changed.
    pub fn tick(&mut self, now_ms: u64) -> bool {
        let ended = self.playing
            && self.current.as_ref().is_some_and(|current| {
                current.duration_ms > 0 && self.position_at(now_ms) >= current.duration_ms
            });
        if ended {
            self.advance(now_ms);
        }
        ended
    }

    /// The state to broadcast, its position brought up to `now_ms`.
    pub fn state(&self, now_ms: u64) -> JamState {
        JamState {
            seq: self.seq,
            current: self.current.clone(),
            queue: self.queue.clone(),
            playing: self.playing,
            position_ms: self.position_at(now_ms),
            server_time_ms: now_ms,
            participants: self.participants.clone(),
        }
    }

    fn position_at(&self, now_ms: u64) -> u32 {
        let position = if self.playing {
            let elapsed = now_ms.saturating_sub(self.anchor_ms);
            u32::try_from(u64::from(self.position_ms) + elapsed).unwrap_or(u32::MAX)
        } else {
            self.position_ms
        };
        let duration = self
            .current
            .as_ref()
            .map_or(0, |current| current.duration_ms);
        clamp_to(position, duration)
    }

    /// The next song starts from its beginning; with none left, the jam
    /// stops.
    fn advance(&mut self, now_ms: u64) {
        self.current = (!self.queue.is_empty()).then(|| self.queue.remove(0));
        if self.current.is_none() {
            self.playing = false;
        }
        self.position_ms = 0;
        self.anchor_ms = now_ms;
        self.changed();
    }

    fn queue_index(&self, item: ItemId) -> Result<usize, Refusal> {
        self.queue
            .iter()
            .position(|queued| queued.id == item)
            .ok_or(Refusal::NotFound)
    }

    /// Two listeners named alike are told apart by a number.
    fn unique_name(&self, name: &str) -> String {
        let taken = |candidate: &str| {
            self.participants
                .iter()
                .any(|participant| participant.name.eq_ignore_ascii_case(candidate))
        };
        if !taken(name) {
            return name.to_string();
        }
        (2..)
            .map(|number| {
                let suffix = format!(" ({number})");
                let room = MAX_NAME_CHARS - suffix.chars().count();
                format!("{}{suffix}", clean_text(name, room))
            })
            .find(|candidate| !taken(candidate))
            .expect("fewer participants than numbers")
    }

    fn changed(&mut self) {
        self.seq += 1;
    }
}

/// A position no further than a known duration.
fn clamp_to(position_ms: u32, duration_ms: u32) -> u32 {
    if duration_ms > 0 {
        position_ms.min(duration_ms)
    } else {
        position_ms
    }
}

/// Estimates the server's clock from ping round trips.
#[derive(Debug, Default)]
pub struct ClockSync {
    offsets: VecDeque<i64>,
}

impl ClockSync {
    /// A ping sent at `t0` and answered at `t1`, both on the local clock,
    /// which the server stamped `server_time_ms`.
    pub fn sample(&mut self, t0: u64, server_time_ms: u64, t1: u64) {
        let Some(round_trip) = t1.checked_sub(t0) else {
            return;
        };
        if round_trip > MAX_ROUND_TRIP_MS {
            return;
        }
        let midpoint = t0 + round_trip / 2;
        let offset = server_time_ms as i64 - midpoint as i64;
        if self.offsets.len() == CLOCK_SAMPLES {
            self.offsets.pop_front();
        }
        self.offsets.push_back(offset);
    }

    /// The server's clock now, once at least one sample arrived.
    pub fn server_now(&self, local_ms: u64) -> Option<u64> {
        Some(local_ms.saturating_add_signed(self.offset()?))
    }

    /// How far the server's clock runs ahead of this one: the median of the
    /// recent samples.
    pub fn offset(&self) -> Option<i64> {
        let mut offsets: Vec<i64> = self.offsets.iter().copied().collect();
        if offsets.is_empty() {
            return None;
        }
        offsets.sort_unstable();
        Some(offsets[offsets.len() / 2])
    }
}

/// Milliseconds since a clock started. The network task and the app share
/// one, so a clock offset measured by one applies to the other.
#[derive(Clone, Copy, Debug)]
pub struct JamClock {
    epoch: std::time::Instant,
}

impl Default for JamClock {
    fn default() -> Self {
        Self::new()
    }
}

impl JamClock {
    pub fn new() -> Self {
        Self {
            epoch: std::time::Instant::now(),
        }
    }

    pub fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The newest state a listener has seen.
#[derive(Debug, Default)]
pub struct Follower {
    pub state: Option<JamState>,
}

impl Follower {
    /// Keeps `state` unless a newer one already arrived. Returns whether it
    /// was kept.
    pub fn receive(&mut self, state: JamState) -> bool {
        if self
            .state
            .as_ref()
            .is_some_and(|seen| seen.seq >= state.seq)
        {
            return false;
        }
        self.state = Some(state);
        true
    }
}

/// What local playback is doing, as far as following a jam needs.
#[derive(Clone, Copy, Debug)]
pub struct LocalView<'a> {
    pub uri: Option<&'a str>,
    pub playing: bool,
    /// The engine is fetching the song named by `uri`.
    pub loading: bool,
    pub position_ms: u32,
}

/// One step local playback should take to match the jam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Correction {
    Load {
        uri: String,
        position_ms: u32,
        play: bool,
    },
    Seek(u32),
    SetPlaying(bool),
}

/// The corrections that bring local playback in line with `state`, given
/// the server's clock now.
pub fn reconcile(state: &JamState, local: LocalView<'_>, server_now_ms: u64) -> Vec<Correction> {
    let Some(current) = &state.current else {
        return if local.playing {
            vec![Correction::SetPlaying(false)]
        } else {
            Vec::new()
        };
    };
    let target = if state.playing {
        let elapsed = server_now_ms.saturating_sub(state.server_time_ms);
        u32::try_from(u64::from(state.position_ms) + elapsed).unwrap_or(u32::MAX)
    } else {
        state.position_ms
    };
    let target = clamp_to(target, current.duration_ms);
    if local.uri != Some(current.uri.as_str()) {
        return vec![Correction::Load {
            uri: current.uri.clone(),
            position_ms: target,
            play: state.playing,
        }];
    }
    // The load already asked for is under way; its position arrives with it.
    if local.loading {
        return Vec::new();
    }
    let mut corrections = Vec::new();
    if local.position_ms.abs_diff(target) > DRIFT_TOLERANCE_MS {
        corrections.push(Correction::Seek(target));
    }
    if local.playing != state.playing {
        corrections.push(Correction::SetPlaying(state.playing));
    }
    corrections
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "spotify:track:bbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "spotify:track:cccccccccccccccccccccc";

    fn add(uri: &str, duration_ms: u32) -> ClientMsg {
        ClientMsg::Add {
            uri: uri.into(),
            title: "Song".into(),
            artists: "Artist".into(),
            duration_ms,
        }
    }

    fn hello(secret: &Secret, nonce: &str, name: &str) -> ClientMsg {
        ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            name: name.into(),
            proof: auth::proof(secret, nonce, name, b"tls"),
        }
    }

    fn join(jam: &mut JamSession, secret: &Secret, name: &str) -> ParticipantId {
        let nonce = auth::new_nonce();
        jam.admit(&hello(secret, &nonce, name), secret, &nonce, b"tls")
            .unwrap()
    }

    /// A jam with two listeners.
    fn jam() -> (JamSession, ParticipantId, ParticipantId) {
        let secret = Secret::generate();
        let mut jam = JamSession::new(0);
        let ana = join(&mut jam, &secret, "Ana");
        let bob = join(&mut jam, &secret, "Bob");
        (jam, ana, bob)
    }

    fn uris(state: &JamState) -> (Option<&str>, Vec<&str>) {
        (
            state.current.as_ref().map(|item| item.uri.as_str()),
            state.queue.iter().map(|item| item.uri.as_str()).collect(),
        )
    }

    #[test]
    fn only_a_proof_for_this_nonce_password_and_channel_gets_in() {
        let secret = Secret::generate();
        let mut jam = JamSession::new(0);
        let nonce = auth::new_nonce();
        let replayed = hello(&secret, &auth::new_nonce(), "Ana");
        assert_eq!(
            jam.admit(&replayed, &secret, &nonce, b"tls"),
            Err(Rejection::BadPassword)
        );
        let wrong = hello(&Secret::generate(), &nonce, "Ana");
        assert_eq!(
            jam.admit(&wrong, &secret, &nonce, b"tls"),
            Err(Rejection::BadPassword)
        );
        let relayed = hello(&secret, &nonce, "Ana");
        assert_eq!(
            jam.admit(&relayed, &secret, &nonce, b"another connection"),
            Err(Rejection::BadPassword)
        );
        let ClientMsg::Hello { name, proof, .. } = hello(&secret, &nonce, "Ana") else {
            unreachable!()
        };
        let old = ClientMsg::Hello {
            version: PROTOCOL_VERSION - 1,
            name,
            proof,
        };
        assert_eq!(
            jam.admit(&old, &secret, &nonce, b"tls"),
            Err(Rejection::IncompatibleVersion)
        );
        assert!(jam.state(0).participants.is_empty());
        assert!(
            jam.admit(&hello(&secret, &nonce, "Ana"), &secret, &nonce, b"tls")
                .is_ok()
        );
    }

    #[test]
    fn the_jam_fills_up_and_names_stay_distinct() {
        let secret = Secret::generate();
        let mut jam = JamSession::new(0);
        for _ in 0..MAX_PARTICIPANTS {
            join(&mut jam, &secret, "ana");
        }
        let nonce = auth::new_nonce();
        assert_eq!(
            jam.admit(&hello(&secret, &nonce, "Bob"), &secret, &nonce, b"tls"),
            Err(Rejection::Full)
        );
        let names: Vec<String> = jam
            .state(0)
            .participants
            .into_iter()
            .map(|participant| participant.name)
            .collect();
        assert_eq!(names[..3], ["ana", "ana (2)", "ana (3)"]);
    }

    #[test]
    fn every_listener_may_do_everything() {
        let (mut jam, ana, bob) = jam();
        jam.apply(ana, add(A, 1000), 0).unwrap();
        jam.apply(ana, add(B, 1000), 0).unwrap();
        jam.apply(ana, add(C, 1000), 0).unwrap();
        let c = jam.state(0).queue[1].id;
        jam.apply(bob, ClientMsg::Move { item: c, to: 0 }, 0)
            .unwrap();
        assert_eq!(uris(&jam.state(0)).1, [C, B]);
        jam.apply(bob, ClientMsg::Remove { item: c }, 0).unwrap();
        jam.apply(bob, ClientMsg::SetPlaying { playing: true }, 0)
            .unwrap();
        jam.apply(bob, ClientMsg::Seek { position_ms: 500 }, 0)
            .unwrap();
        jam.apply(bob, ClientMsg::Skip, 0).unwrap();
        assert_eq!(uris(&jam.state(0)), (Some(B), vec![]));
        assert_eq!(
            jam.apply(bob, ClientMsg::Remove { item: c }, 0),
            Err(Refusal::NotFound)
        );
        assert_eq!(
            jam.apply(ana, ClientMsg::Ping { t0: 0 }, 0),
            Err(Refusal::Unexpected)
        );
    }

    #[test]
    fn the_first_report_of_an_ending_moves_on_and_the_rest_are_ignored() {
        let (mut jam, ana, bob) = jam();
        jam.apply(ana, add(A, 0), 0).unwrap();
        jam.apply(ana, add(B, 0), 0).unwrap();
        let a = jam.state(0).current.unwrap().id;
        jam.apply(ana, ClientMsg::Ended { item: a }, 0).unwrap();
        assert_eq!(
            jam.apply(bob, ClientMsg::Ended { item: a }, 0),
            Err(Refusal::NotFound),
            "a second report must not skip the next song too"
        );
        assert_eq!(uris(&jam.state(0)), (Some(B), vec![]));
    }

    #[test]
    fn a_listener_cannot_flood_the_queue() {
        let (mut jam, ana, bob) = jam();
        jam.apply(ana, add(A, 1000), 0).unwrap();
        for _ in 0..MAX_QUEUED_PER_LISTENER {
            jam.apply(ana, add(B, 1000), 0).unwrap();
        }
        assert_eq!(jam.apply(ana, add(B, 1000), 0), Err(Refusal::QuotaReached));
        jam.apply(bob, add(C, 1000), 0).unwrap();
    }

    #[test]
    fn the_queue_has_a_ceiling() {
        let secret = Secret::generate();
        let mut jam = JamSession::new(0);
        let first = join(&mut jam, &secret, "Ana");
        jam.apply(first, add(A, 1000), 0).unwrap();
        let mut listener = first;
        while jam.state(0).queue.len() < MAX_QUEUE {
            if jam.apply(listener, add(C, 1000), 0) == Err(Refusal::QuotaReached) {
                jam.leave(listener, 0);
                listener = join(&mut jam, &secret, "Next");
            }
        }
        assert_eq!(
            jam.apply(listener, add(C, 1000), 0),
            Err(Refusal::QueueFull)
        );
    }

    #[test]
    fn playback_runs_on_the_server_clock_and_moves_on_at_the_end() {
        let (mut jam, ana, _) = jam();
        jam.apply(ana, add(A, 10_000), 0).unwrap();
        jam.apply(ana, add(B, 10_000), 0).unwrap();
        jam.apply(ana, ClientMsg::SetPlaying { playing: true }, 1_000)
            .unwrap();
        assert_eq!(jam.state(4_000).position_ms, 3_000);
        jam.apply(ana, ClientMsg::SetPlaying { playing: false }, 5_000)
            .unwrap();
        assert_eq!(jam.state(9_000).position_ms, 4_000, "paused holds still");
        jam.apply(
            ana,
            ClientMsg::Seek {
                position_ms: 99_000,
            },
            9_000,
        )
        .unwrap();
        assert_eq!(
            jam.state(9_000).position_ms,
            10_000,
            "seek stays in the song"
        );
        jam.apply(ana, ClientMsg::Seek { position_ms: 8_000 }, 9_000)
            .unwrap();
        jam.apply(ana, ClientMsg::SetPlaying { playing: true }, 9_000)
            .unwrap();
        assert!(!jam.tick(10_999));
        assert!(jam.tick(11_000));
        let state = jam.state(11_000);
        assert_eq!(uris(&state), (Some(B), vec![]));
        assert!(state.playing);
        assert_eq!(state.position_ms, 0);
        assert!(jam.tick(21_000));
        let state = jam.state(21_000);
        assert_eq!(uris(&state), (None, vec![]));
        assert!(!state.playing, "an empty jam stops");
    }

    #[test]
    fn the_jam_pauses_when_the_last_listener_leaves() {
        let (mut jam, ana, bob) = jam();
        jam.apply(ana, add(A, 100_000), 0).unwrap();
        jam.apply(ana, ClientMsg::SetPlaying { playing: true }, 0)
            .unwrap();
        assert!(jam.leave(ana, 1_000));
        assert!(jam.state(5_000).playing, "one listener remains");
        assert!(jam.leave(bob, 5_000));
        assert!(!jam.leave(bob, 5_000));
        let state = jam.state(60_000);
        assert!(!state.playing);
        assert_eq!(state.position_ms, 5_000);
    }

    #[test]
    fn a_saved_jam_comes_back_paused_with_fresh_ids() {
        let (mut jam, ana, _) = jam();
        jam.apply(ana, add(A, 100_000), 0).unwrap();
        jam.apply(ana, add(B, 100_000), 0).unwrap();
        jam.apply(ana, ClientMsg::SetPlaying { playing: true }, 0)
            .unwrap();
        let saved = jam.save(3_000);
        let text = serde_json::to_string(&saved).unwrap();
        let restored = JamSession::restore(serde_json::from_str(&text).unwrap(), 0);
        let state = restored.state(50_000);
        assert_eq!(uris(&state), (Some(A), vec![B]));
        assert_eq!(state.position_ms, 3_000);
        assert!(!state.playing);
        assert!(state.participants.is_empty());
        let mut restored = restored;
        let secret = Secret::generate();
        let cleo = join(&mut restored, &secret, "Cleo");
        restored.apply(cleo, add(C, 1000), 0).unwrap();
        let ids: Vec<ItemId> = restored
            .state(0)
            .current
            .iter()
            .chain(&restored.state(0).queue)
            .map(|item| item.id)
            .collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "ids stay distinct after a restore");
    }

    #[test]
    fn every_change_advances_the_sequence_and_a_late_state_is_ignored() {
        let (mut jam, ana, _) = jam();
        let before = jam.state(0);
        jam.apply(ana, add(A, 1000), 0).unwrap();
        let after = jam.state(0);
        assert!(after.seq > before.seq);
        let mut follower = Follower::default();
        assert!(follower.receive(after.clone()));
        assert!(
            !follower.receive(before),
            "a late state must not undo a newer one"
        );
        assert!(!follower.receive(after.clone()));
        assert_eq!(follower.state, Some(after));
    }

    #[test]
    fn the_server_clock_is_the_median_of_recent_round_trips() {
        let mut clock = ClockSync::default();
        assert_eq!(clock.server_now(0), None);
        // The server runs 1000 ms ahead; a 100 ms round trip is split evenly.
        clock.sample(0, 1_050, 100);
        assert_eq!(clock.server_now(200), Some(1_200));
        // One lopsided sample does not move the median of three.
        clock.sample(1_000, 2_050, 1_100);
        clock.sample(2_000, 9_000, 2_100);
        assert_eq!(clock.server_now(3_000), Some(4_000));
        // Out of order and very slow round trips are dropped.
        clock.sample(5_000, 0, 4_000);
        clock.sample(0, 0, MAX_ROUND_TRIP_MS + 1);
        assert_eq!(clock.offsets.len(), 3);
        let mut behind = ClockSync::default();
        behind.sample(10_000, 5_000, 10_000);
        assert_eq!(behind.server_now(10_000), Some(5_000));
    }

    fn playing_state(uri: &str, position_ms: u32, server_time_ms: u64, playing: bool) -> JamState {
        JamState {
            seq: 1,
            current: Some(JamItem {
                id: 1,
                uri: uri.into(),
                title: String::new(),
                artists: String::new(),
                duration_ms: 200_000,
                added_by: 1,
            }),
            queue: Vec::new(),
            playing,
            position_ms,
            server_time_ms,
            participants: Vec::new(),
        }
    }

    fn local(uri: Option<&str>, playing: bool, position_ms: u32) -> LocalView<'_> {
        LocalView {
            uri,
            playing,
            loading: false,
            position_ms,
        }
    }

    #[test]
    fn a_listener_loads_the_jam_song_where_the_server_is_now() {
        let state = playing_state(A, 10_000, 1_000, true);
        assert_eq!(
            reconcile(&state, local(Some(B), true, 0), 3_000),
            [Correction::Load {
                uri: A.into(),
                position_ms: 12_000,
                play: true,
            }]
        );
        let mut loading = local(Some(A), false, 0);
        loading.loading = true;
        assert_eq!(
            reconcile(&state, loading, 3_000),
            [],
            "the load is under way"
        );
    }

    #[test]
    fn small_drift_is_left_alone_and_large_drift_seeks() {
        let state = playing_state(A, 10_000, 0, true);
        let near = 10_000 + DRIFT_TOLERANCE_MS;
        assert_eq!(reconcile(&state, local(Some(A), true, near), 0), []);
        assert_eq!(
            reconcile(&state, local(Some(A), true, 20_000), 0),
            [Correction::Seek(10_000)]
        );
    }

    #[test]
    fn a_listener_pauses_and_resumes_with_the_jam() {
        let paused = playing_state(A, 10_000, 0, false);
        assert_eq!(
            reconcile(&paused, local(Some(A), true, 10_000), 5_000),
            [Correction::SetPlaying(false)],
            "a paused jam does not advance"
        );
        let playing = playing_state(A, 10_000, 0, true);
        assert_eq!(
            reconcile(&playing, local(Some(A), false, 2_000), 0),
            [Correction::Seek(10_000), Correction::SetPlaying(true)]
        );
        let mut empty = playing;
        empty.current = None;
        assert_eq!(
            reconcile(&empty, local(Some(A), true, 0), 0),
            [Correction::SetPlaying(false)]
        );
        assert_eq!(reconcile(&empty, local(None, false, 0), 0), []);
    }
}
