//! The jam's state machines, free of sockets and of the player.
//!
//! The host holds a [`HostSession`]: it admits guests, applies their
//! requests under the host's permissions, and moves on when a song ends.
//! Every participant, the host included, then [`reconcile`]s the state it
//! last saw with its own local playback, which yields the few player
//! corrections needed to follow along.

use std::collections::VecDeque;

use super::invite::{self, Secret};
use super::protocol::{
    ClientMsg, HOST_ID, ItemId, JamItem, JamState, MAX_NAME_CHARS, MAX_PARTICIPANTS, MAX_QUEUE,
    MAX_QUEUED_PER_GUEST, PROTOCOL_VERSION, Participant, ParticipantId, Permissions, Refusal,
    Rejection, clean_text,
};

/// Below this, a guest's position is left alone: librespot cannot change
/// speed, so every correction is an audible seek.
pub const DRIFT_TOLERANCE_MS: u32 = 750;
/// Clock samples kept; their median resists one slow round trip.
const CLOCK_SAMPLES: usize = 5;
/// A round trip slower than this says little about either clock.
const MAX_ROUND_TRIP_MS: u64 = 5_000;

/// The host's authoritative jam. Times are milliseconds on the host's own
/// clock since the jam started.
#[derive(Debug)]
pub struct HostSession {
    seq: u64,
    current: Option<JamItem>,
    queue: Vec<JamItem>,
    playing: bool,
    /// Position in `current` at `anchor_ms`.
    position_ms: u32,
    anchor_ms: u64,
    participants: Vec<Participant>,
    permissions: Permissions,
    next_item: ItemId,
    next_participant: ParticipantId,
}

impl HostSession {
    pub fn new(host_name: &str, now_ms: u64) -> Self {
        let mut name = clean_text(host_name, MAX_NAME_CHARS);
        if name.is_empty() {
            name = "Host".into();
        }
        Self {
            seq: 0,
            current: None,
            queue: Vec::new(),
            playing: false,
            position_ms: 0,
            anchor_ms: now_ms,
            participants: vec![Participant { id: HOST_ID, name }],
            permissions: Permissions::default(),
            next_item: 1,
            next_participant: HOST_ID + 1,
        }
    }

    /// Lets a guest in when its [`ClientMsg::Hello`] answers this
    /// connection's `nonce` with the jam's secret.
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
            return Err(Rejection::BadInvite);
        };
        if *version != PROTOCOL_VERSION {
            return Err(Rejection::IncompatibleVersion);
        }
        if !invite::verify(secret, nonce, name, binding, proof) {
            return Err(Rejection::BadInvite);
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

    /// A guest left or was disconnected. Its songs stay in the queue.
    /// Returns whether it was still in the jam.
    pub fn leave(&mut self, id: ParticipantId) -> bool {
        if id == HOST_ID {
            return false;
        }
        let before = self.participants.len();
        self.participants.retain(|participant| participant.id != id);
        let left = self.participants.len() != before;
        if left {
            self.changed();
        }
        left
    }

    pub fn set_permissions(&mut self, permissions: Permissions) {
        if self.permissions != permissions {
            self.permissions = permissions;
            self.changed();
        }
    }

    /// Applies one request from a participant. On a refusal nothing
    /// changes.
    pub fn apply(
        &mut self,
        from: ParticipantId,
        message: ClientMsg,
        now_ms: u64,
    ) -> Result<(), Refusal> {
        if !self
            .participants
            .iter()
            .any(|participant| participant.id == from)
        {
            return Err(Refusal::NotAllowed);
        }
        let control = from == HOST_ID || self.permissions.guests_control_playback;
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
                if from != HOST_ID
                    && self
                        .queue
                        .iter()
                        .filter(|item| item.added_by == from)
                        .count()
                        >= MAX_QUEUED_PER_GUEST
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
                if !control && self.queue[index].added_by != from {
                    return Err(Refusal::NotAllowed);
                }
                self.queue.remove(index);
            }
            ClientMsg::Move { item, to } => {
                if !control {
                    return Err(Refusal::NotAllowed);
                }
                let index = self.queue_index(item)?;
                let moved = self.queue.remove(index);
                self.queue.insert(to.min(self.queue.len()), moved);
            }
            ClientMsg::Skip => {
                if !control {
                    return Err(Refusal::NotAllowed);
                }
                if self.current.is_none() {
                    return Err(Refusal::NotFound);
                }
                self.advance(now_ms);
                return Ok(());
            }
            ClientMsg::SetPlaying { playing } => {
                if !control {
                    return Err(Refusal::NotAllowed);
                }
                if self.current.is_none() {
                    return Err(Refusal::NotFound);
                }
                self.position_ms = self.position_at(now_ms);
                self.anchor_ms = now_ms;
                self.playing = playing;
            }
            ClientMsg::Seek { position_ms } => {
                if !control {
                    return Err(Refusal::NotAllowed);
                }
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
            host_time_ms: now_ms,
            participants: self.participants.clone(),
            permissions: self.permissions,
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

    /// Two guests named alike, or a guest named like the host, are told
    /// apart by a number.
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

/// Estimates the host's clock from ping round trips.
#[derive(Debug, Default)]
pub struct ClockSync {
    offsets: VecDeque<i64>,
}

impl ClockSync {
    /// A ping sent at `t0` and answered at `t1`, both on the local clock,
    /// which the host stamped `host_time_ms`.
    pub fn sample(&mut self, t0: u64, host_time_ms: u64, t1: u64) {
        let Some(round_trip) = t1.checked_sub(t0) else {
            return;
        };
        if round_trip > MAX_ROUND_TRIP_MS {
            return;
        }
        let midpoint = t0 + round_trip / 2;
        let offset = host_time_ms as i64 - midpoint as i64;
        if self.offsets.len() == CLOCK_SAMPLES {
            self.offsets.pop_front();
        }
        self.offsets.push_back(offset);
    }

    /// The host's clock now, once at least one sample arrived.
    pub fn host_now(&self, local_ms: u64) -> Option<u64> {
        Some(local_ms.saturating_add_signed(self.offset()?))
    }

    /// How far the host's clock runs ahead of this one: the median of the
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

/// Milliseconds since a jam began on this computer. The network tasks and
/// the app share one, so a clock offset measured by one applies to the
/// other.
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

/// The newest state a guest has seen.
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
/// the host's clock now.
pub fn reconcile(state: &JamState, local: LocalView<'_>, host_now_ms: u64) -> Vec<Correction> {
    let Some(current) = &state.current else {
        return if local.playing {
            vec![Correction::SetPlaying(false)]
        } else {
            Vec::new()
        };
    };
    let target = if state.playing {
        let elapsed = host_now_ms.saturating_sub(state.host_time_ms);
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
            proof: invite::proof(secret, nonce, name, b""),
        }
    }

    /// A host with one admitted guest.
    fn jam() -> (HostSession, ParticipantId, Secret) {
        let secret = Secret::generate();
        let mut host = HostSession::new("Host", 0);
        let nonce = invite::new_nonce();
        let guest = host
            .admit(&hello(&secret, &nonce, "Ana"), &secret, &nonce, b"")
            .unwrap();
        (host, guest, secret)
    }

    fn uris(state: &JamState) -> (Option<&str>, Vec<&str>) {
        (
            state.current.as_ref().map(|item| item.uri.as_str()),
            state.queue.iter().map(|item| item.uri.as_str()).collect(),
        )
    }

    #[test]
    fn only_a_proof_for_this_nonce_and_secret_gets_in() {
        let secret = Secret::generate();
        let mut host = HostSession::new("Host", 0);
        let nonce = invite::new_nonce();
        let replayed = hello(&secret, &invite::new_nonce(), "Ana");
        assert_eq!(
            host.admit(&replayed, &secret, &nonce, b""),
            Err(Rejection::BadInvite)
        );
        let wrong_secret = hello(&Secret::generate(), &nonce, "Ana");
        assert_eq!(
            host.admit(&wrong_secret, &secret, &nonce, b""),
            Err(Rejection::BadInvite)
        );
        let ClientMsg::Hello { name, proof, .. } = hello(&secret, &nonce, "Ana") else {
            unreachable!()
        };
        let old = ClientMsg::Hello {
            version: PROTOCOL_VERSION + 1,
            name,
            proof,
        };
        assert_eq!(
            host.admit(&old, &secret, &nonce, b""),
            Err(Rejection::IncompatibleVersion)
        );
        assert_eq!(host.state(0).participants.len(), 1);
        assert!(
            host.admit(&hello(&secret, &nonce, "Ana"), &secret, &nonce, b"")
                .is_ok()
        );
    }

    #[test]
    fn the_jam_fills_up_and_names_stay_distinct() {
        let secret = Secret::generate();
        let mut host = HostSession::new("Ana", 0);
        for _ in 1..MAX_PARTICIPANTS {
            let nonce = invite::new_nonce();
            host.admit(&hello(&secret, &nonce, "ana"), &secret, &nonce, b"")
                .unwrap();
        }
        let nonce = invite::new_nonce();
        assert_eq!(
            host.admit(&hello(&secret, &nonce, "Bob"), &secret, &nonce, b""),
            Err(Rejection::Full)
        );
        let names: Vec<String> = host
            .state(0)
            .participants
            .into_iter()
            .map(|participant| participant.name)
            .collect();
        assert_eq!(names[..3], ["Ana", "ana (2)", "ana (3)"]);
        let unique: std::collections::HashSet<String> =
            names.iter().map(|name| name.to_lowercase()).collect();
        assert_eq!(unique.len(), names.len());
    }

    #[test]
    fn the_first_song_added_plays_next_and_later_ones_queue() {
        let (mut host, guest, _) = jam();
        host.apply(guest, add(A, 1000), 0).unwrap();
        host.apply(HOST_ID, add(B, 1000), 0).unwrap();
        host.apply(guest, add(A, 1000), 0).unwrap();
        let state = host.state(0);
        assert_eq!(uris(&state), (Some(A), vec![B, A]));
        assert_ne!(state.queue[0].id, state.queue[1].id);
        assert_eq!(state.queue[1].added_by, guest);
    }

    #[test]
    fn guests_control_playback_only_when_the_host_allows_it() {
        let (mut host, guest, _) = jam();
        host.apply(HOST_ID, add(A, 1000), 0).unwrap();
        host.apply(HOST_ID, add(B, 1000), 0).unwrap();
        let seq = host.state(0).seq;
        for request in [
            ClientMsg::Skip,
            ClientMsg::SetPlaying { playing: true },
            ClientMsg::Seek { position_ms: 5 },
        ] {
            assert_eq!(
                host.apply(guest, request.clone(), 0),
                Err(Refusal::NotAllowed)
            );
        }
        let b = host.state(0).queue[0].id;
        assert_eq!(
            host.apply(guest, ClientMsg::Move { item: b, to: 0 }, 0),
            Err(Refusal::NotAllowed)
        );
        assert_eq!(
            host.apply(guest, ClientMsg::Remove { item: b }, 0),
            Err(Refusal::NotAllowed),
            "a guest removes only its own songs"
        );
        assert_eq!(host.state(0).seq, seq, "a refusal changes nothing");

        host.set_permissions(Permissions {
            guests_control_playback: true,
        });
        host.apply(guest, ClientMsg::Skip, 0).unwrap();
        assert_eq!(uris(&host.state(0)), (Some(B), vec![]));
    }

    #[test]
    fn a_guest_may_remove_its_own_songs() {
        let (mut host, guest, _) = jam();
        host.apply(HOST_ID, add(A, 1000), 0).unwrap();
        host.apply(guest, add(B, 1000), 0).unwrap();
        let b = host.state(0).queue[0].id;
        host.apply(guest, ClientMsg::Remove { item: b }, 0).unwrap();
        assert_eq!(uris(&host.state(0)), (Some(A), vec![]));
        assert_eq!(
            host.apply(guest, ClientMsg::Remove { item: b }, 0),
            Err(Refusal::NotFound)
        );
    }

    #[test]
    fn strangers_and_handshake_messages_are_not_requests() {
        let (mut host, guest, _) = jam();
        assert_eq!(host.apply(99, add(A, 1000), 0), Err(Refusal::NotAllowed));
        assert_eq!(
            host.apply(guest, ClientMsg::Ping { t0: 0 }, 0),
            Err(Refusal::Unexpected)
        );
        host.leave(guest);
        assert_eq!(host.apply(guest, add(A, 1000), 0), Err(Refusal::NotAllowed));
    }

    #[test]
    fn a_guest_cannot_flood_the_queue() {
        let (mut host, guest, _) = jam();
        host.apply(HOST_ID, add(A, 1000), 0).unwrap();
        for _ in 0..MAX_QUEUED_PER_GUEST {
            host.apply(guest, add(B, 1000), 0).unwrap();
        }
        assert_eq!(
            host.apply(guest, add(B, 1000), 0),
            Err(Refusal::QuotaReached)
        );
        // The host is not held to a guest's quota, only to the queue's size.
        while host.state(0).queue.len() < MAX_QUEUE {
            host.apply(HOST_ID, add(C, 1000), 0).unwrap();
        }
        assert_eq!(
            host.apply(HOST_ID, add(C, 1000), 0),
            Err(Refusal::QueueFull)
        );
    }

    #[test]
    fn moving_a_song_lands_it_at_the_asked_place() {
        let (mut host, _, _) = jam();
        for uri in [A, A, B, C] {
            host.apply(HOST_ID, add(uri, 1000), 0).unwrap();
        }
        let c = host.state(0).queue[2].id;
        host.apply(HOST_ID, ClientMsg::Move { item: c, to: 0 }, 0)
            .unwrap();
        assert_eq!(uris(&host.state(0)).1, [C, A, B]);
        host.apply(HOST_ID, ClientMsg::Move { item: c, to: 99 }, 0)
            .unwrap();
        assert_eq!(uris(&host.state(0)).1, [A, B, C]);
    }

    #[test]
    fn playback_runs_on_the_host_clock_and_moves_on_at_the_end() {
        let (mut host, _, _) = jam();
        host.apply(HOST_ID, add(A, 10_000), 0).unwrap();
        host.apply(HOST_ID, add(B, 10_000), 0).unwrap();
        host.apply(HOST_ID, ClientMsg::SetPlaying { playing: true }, 1_000)
            .unwrap();
        assert_eq!(host.state(4_000).position_ms, 3_000);
        host.apply(HOST_ID, ClientMsg::SetPlaying { playing: false }, 5_000)
            .unwrap();
        assert_eq!(host.state(9_000).position_ms, 4_000, "paused holds still");
        host.apply(
            HOST_ID,
            ClientMsg::Seek {
                position_ms: 99_000,
            },
            9_000,
        )
        .unwrap();
        assert_eq!(
            host.state(9_000).position_ms,
            10_000,
            "seek stays in the song"
        );
        host.apply(HOST_ID, ClientMsg::Seek { position_ms: 8_000 }, 9_000)
            .unwrap();
        host.apply(HOST_ID, ClientMsg::SetPlaying { playing: true }, 9_000)
            .unwrap();
        assert!(!host.tick(10_999));
        assert!(host.tick(11_000));
        let state = host.state(11_000);
        assert_eq!(uris(&state), (Some(B), vec![]));
        assert!(state.playing);
        assert_eq!(state.position_ms, 0);
        assert!(host.tick(21_000));
        let state = host.state(21_000);
        assert_eq!(uris(&state), (None, vec![]));
        assert!(!state.playing, "an empty jam stops");
    }

    #[test]
    fn every_change_advances_the_sequence_and_a_late_state_is_ignored() {
        let (mut host, _, _) = jam();
        let before = host.state(0);
        host.apply(HOST_ID, add(A, 1000), 0).unwrap();
        let after = host.state(0);
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
    fn the_host_clock_is_the_median_of_recent_round_trips() {
        let mut clock = ClockSync::default();
        assert_eq!(clock.host_now(0), None);
        // The host runs 1000 ms ahead; a 100 ms round trip is split evenly.
        clock.sample(0, 1_050, 100);
        assert_eq!(clock.host_now(200), Some(1_200));
        // One lopsided sample does not move the median of three.
        clock.sample(1_000, 2_050, 1_100);
        clock.sample(2_000, 9_000, 2_100);
        assert_eq!(clock.host_now(3_000), Some(4_000));
        // Out of order and very slow round trips are dropped.
        clock.sample(5_000, 0, 4_000);
        clock.sample(0, 0, MAX_ROUND_TRIP_MS + 1);
        assert_eq!(clock.offsets.len(), 3);
        // A host behind this clock is fine too.
        let mut behind = ClockSync::default();
        behind.sample(10_000, 5_000, 10_000);
        assert_eq!(behind.host_now(10_000), Some(5_000));
    }

    fn playing_state(uri: &str, position_ms: u32, host_time_ms: u64, playing: bool) -> JamState {
        JamState {
            seq: 1,
            current: Some(JamItem {
                id: 1,
                uri: uri.into(),
                title: String::new(),
                artists: String::new(),
                duration_ms: 200_000,
                added_by: HOST_ID,
            }),
            queue: Vec::new(),
            playing,
            position_ms,
            host_time_ms,
            participants: Vec::new(),
            permissions: Permissions::default(),
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
    fn a_guest_loads_the_jam_song_where_the_host_is_now() {
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
    fn a_guest_pauses_and_resumes_with_the_jam() {
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
