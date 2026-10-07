//! Keeps local playback following the jam without fighting the engine.
//!
//! [`reconcile`] says what differs. The engine answers each command a moment
//! later, and asking again before it has would load the song twice or seek
//! back and forth, so a [`Syncer`] holds back after each correction. It also
//! notices when a song ran out on its own: each jam song is loaded on repeat
//! so the engine cannot wander off into Spotify's autoplay, which makes the
//! end of a song look like the same song starting again.

use crate::protocol::JamState;
use crate::session::{Correction, DRIFT_TOLERANCE_MS, LocalView, reconcile};

/// Time for a load to show up in the engine's state.
const LOAD_GRACE_MS: u64 = 5_000;
/// Time for a seek, play or pause to show up.
const SETTLE_MS: u64 = 1_500;

/// What local playback reports, `track_sequence` included: it grows with
/// every start of a song, another start of the same one too.
#[derive(Clone, Copy, Debug)]
pub struct Observed<'a> {
    pub view: LocalView<'a>,
    pub track_sequence: u64,
}

#[derive(Debug, Default)]
pub struct Syncer {
    /// The song last loaded, and when, on this computer's jam clock.
    loaded: Option<(String, u64)>,
    /// Until when seeks, plays and pauses wait for the last one to land.
    settle_until: u64,
    /// The last `track_sequence` seen, and whether the next rise is the
    /// load this computer asked for.
    sequence: Option<u64>,
    awaiting_load: bool,
}

/// One pass of [`Syncer::step`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub corrections: Vec<Correction>,
    /// The jam song started over without being asked: it reached its end.
    pub song_ended: bool,
}

impl Syncer {
    /// Compares `state` with local playback and returns the corrections due
    /// now. `server_now_ms` is the server's clock; `now_ms` this computer's.
    pub fn step(
        &mut self,
        state: &JamState,
        local: Observed<'_>,
        server_now_ms: u64,
        now_ms: u64,
    ) -> Step {
        let song_ended = self.observe(state, local);
        let wanted = reconcile(state, local.view, server_now_ms);
        let mut corrections = Vec::new();
        for correction in wanted {
            match &correction {
                Correction::Load { uri, .. } => {
                    let pending = self.loaded.as_ref().is_some_and(|(loaded, at)| {
                        loaded == uri && now_ms.saturating_sub(*at) < LOAD_GRACE_MS
                    });
                    if pending {
                        continue;
                    }
                    self.loaded = Some((uri.clone(), now_ms));
                    self.awaiting_load = true;
                    self.settle_until = now_ms + SETTLE_MS;
                }
                Correction::Seek(target) => {
                    if now_ms < self.settle_until || near_the_end(state, *target) {
                        continue;
                    }
                    self.settle_until = now_ms + SETTLE_MS;
                }
                Correction::SetPlaying(_) => {
                    if now_ms < self.settle_until {
                        continue;
                    }
                    self.settle_until = now_ms + SETTLE_MS;
                }
            }
            corrections.push(correction);
        }
        Step {
            corrections,
            song_ended,
        }
    }

    fn observe(&mut self, state: &JamState, local: Observed<'_>) -> bool {
        let previous = self.sequence.replace(local.track_sequence);
        let started = previous.is_some_and(|seen| local.track_sequence > seen);
        if !started {
            return false;
        }
        if self.awaiting_load {
            self.awaiting_load = false;
            return false;
        }
        let jam_song = state.current.as_ref().map(|current| current.uri.as_str());
        local.view.uri.is_some() && local.view.uri == jam_song
    }
}

/// A song this close to its end changes in a moment: seeking it now would
/// only replay its last second.
fn near_the_end(state: &JamState, target: u32) -> bool {
    state.current.as_ref().is_some_and(|current| {
        current.duration_ms > 0 && current.duration_ms.saturating_sub(target) <= DRIFT_TOLERANCE_MS
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::JamItem;

    const A: &str = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "spotify:track:bbbbbbbbbbbbbbbbbbbbbb";

    fn state(uri: &str, position_ms: u32, playing: bool) -> JamState {
        JamState {
            seq: 1,
            current: Some(JamItem {
                id: 1,
                uri: uri.into(),
                title: String::new(),
                artists: String::new(),
                duration_ms: 100_000,
                added_by: 1,
            }),
            queue: Vec::new(),
            playing,
            position_ms,
            server_time_ms: 0,
            participants: Vec::new(),
        }
    }

    fn local(
        uri: Option<&str>,
        playing: bool,
        position_ms: u32,
        track_sequence: u64,
    ) -> Observed<'_> {
        Observed {
            view: LocalView {
                uri,
                playing,
                loading: false,
                position_ms,
            },
            track_sequence,
        }
    }

    #[test]
    fn a_load_is_asked_once_while_the_engine_catches_up() {
        let mut syncer = Syncer::default();
        let jam = state(A, 0, true);
        let first = syncer.step(&jam, local(Some(B), true, 0, 1), 0, 0);
        assert!(matches!(first.corrections[..], [Correction::Load { .. }]));
        assert_eq!(
            syncer.step(&jam, local(Some(B), true, 0, 1), 500, 500),
            Step::default()
        );
        // Past the grace, the engine evidently lost the load: ask again.
        let again = syncer.step(&jam, local(Some(B), true, 0, 1), 6_000, 6_000);
        assert!(matches!(again.corrections[..], [Correction::Load { .. }]));
    }

    #[test]
    fn corrections_wait_for_the_last_one_to_land() {
        let mut syncer = Syncer::default();
        let jam = state(A, 50_000, true);
        assert_eq!(
            syncer
                .step(&jam, local(Some(A), true, 10_000, 1), 0, 0)
                .corrections,
            [Correction::Seek(50_000)]
        );
        // The engine still reports the old position: no second seek yet.
        assert_eq!(
            syncer.step(&jam, local(Some(A), true, 10_200, 1), 200, 200),
            Step::default()
        );
        assert_eq!(
            syncer
                .step(&jam, local(Some(A), true, 12_000, 1), 2_000, 2_000)
                .corrections,
            [Correction::Seek(52_000)]
        );
    }

    #[test]
    fn no_seek_into_the_last_second_of_a_song() {
        let mut syncer = Syncer::default();
        let jam = state(A, 99_500, true);
        assert_eq!(
            syncer.step(&jam, local(Some(A), true, 1_000, 1), 0, 0),
            Step::default()
        );
    }

    #[test]
    fn the_song_starting_over_on_its_own_means_it_ended() {
        let mut syncer = Syncer::default();
        let jam = state(A, 0, true);
        // The load this computer asked for is not an ending.
        syncer.step(&jam, local(Some(B), true, 0, 1), 0, 0);
        assert!(
            !syncer
                .step(&jam, local(Some(A), true, 0, 2), 100, 100)
                .song_ended
        );
        assert!(
            !syncer
                .step(&jam, local(Some(A), true, 50_000, 2), 50_000, 50_000)
                .song_ended
        );
        // Repeat played it again without being asked.
        assert!(
            syncer
                .step(&jam, local(Some(A), true, 0, 3), 100_100, 100_100)
                .song_ended
        );
        // Another song starting on its own is not the jam song ending.
        assert!(
            !syncer
                .step(&jam, local(Some(B), true, 0, 4), 100_200, 100_200)
                .song_ended
        );
    }
}
