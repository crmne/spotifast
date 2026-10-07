//! Local play history.
//!
//! Spotify does not record playback from librespot clients. Spotizgeg stores
//! local plays and merges them with `/me/player/recently-played`, which covers
//! other devices. A track counts only after enough listening time, so skips do
//! not fill the history.

use std::collections::HashMap;
use std::path::Path;

use crate::api::models::{Album, Image, PlayHistory, Track};

/// A play counts after 30 seconds, or halfway through a shorter track.
const COUNTS_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// Maximum number of stored local plays.
const KEPT: usize = 500;

/// A play made here and a play of the same song Spotify reports within this
/// many seconds of it are the same play.
const SAME_PLAY: i64 = 60;

/// When a song has been listened to long enough to count.
pub fn counts_after(duration_ms: u32) -> std::time::Duration {
    let half = std::time::Duration::from_millis(u64::from(duration_ms) / 2);
    COUNTS_AFTER
        .min(half)
        .max(std::time::Duration::from_secs(1))
}

/// The plays made here, newest first.
#[derive(Default)]
pub struct History {
    plays: Vec<PlayHistory>,
    /// Set when the in-memory list differs from the file.
    dirty: bool,
}

impl History {
    /// Reads the history, or returns an empty history if the file is unreadable.
    pub fn load(path: &Path) -> Self {
        let plays = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Vec<PlayHistory>>(&text).ok())
            .unwrap_or_default();
        Self {
            plays,
            dirty: false,
        }
    }

    pub fn plays(&self) -> &[PlayHistory] {
        &self.plays
    }

    pub fn is_empty(&self) -> bool {
        self.plays.is_empty()
    }

    /// Writes the history if it changed since the last save.
    pub fn save(&mut self, path: &Path) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string(&self.plays) {
            Ok(text) => {
                if let Err(error) = std::fs::write(path, text) {
                    log::warn!("could not write the play history: {error}");
                }
            }
            Err(error) => log::warn!("could not write the play history: {error}"),
        }
    }

    /// Writes down that `track` was played at `at`, newest first.
    pub fn record(&mut self, track: Track, at: jiff::Timestamp) {
        self.plays.insert(
            0,
            PlayHistory {
                track,
                played_at: Some(at.to_string()),
                context: None,
            },
        );
        self.plays.truncate(KEPT);
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        if self.plays.is_empty() {
            return;
        }
        self.plays.clear();
        self.dirty = true;
    }
}

/// Converts the playing track into a history record.
pub fn played_track(now: &crate::app::NowPlaying) -> Track {
    Track {
        id: now.id.clone(),
        name: now.title.clone(),
        uri: now.uri.clone(),
        duration_ms: now.duration_ms,
        artists: now.artists.clone(),
        album: Some(Album {
            id: now.album_id.clone().unwrap_or_default(),
            name: now.album_name.clone(),
            images: now
                .art_url
                .as_deref()
                .or(now.art_small.as_deref())
                .map(|url| {
                    vec![Image {
                        url: url.to_string(),
                        width: None,
                        height: None,
                    }]
                })
                .unwrap_or_default(),
            ..Album::default()
        }),
        ..Track::default()
    }
}

/// Merges local and Spotify history, newest first.
///
/// A play Spotify reports within the duplicate window of one made here is
/// that same play, and is shown once. Plays from one source are never
/// merged with each other: each is its own listen, however close together
/// they start. Entries without a timestamp sort to the end.
pub fn merged(local: &[PlayHistory], remote: &[PlayHistory]) -> Vec<PlayHistory> {
    let time = |play: &PlayHistory| {
        play.played_at
            .as_deref()
            .and_then(|at| at.parse::<jiff::Timestamp>().ok())
            .map(|at| at.as_second())
    };
    // The plays made here that no report from Spotify has matched yet.
    let mut unmatched: HashMap<&str, Vec<i64>> = HashMap::new();
    let mut out: Vec<(Option<i64>, PlayHistory)> = Vec::new();
    for play in local {
        let at = time(play);
        if let Some(at) = at {
            unmatched
                .entry(play.track.uri.as_str())
                .or_default()
                .push(at);
        }
        out.push((at, play.clone()));
    }
    for play in remote {
        let at = time(play);
        // Each play made here stands for at most one report: the closest.
        if let Some(at) = at
            && let Some(times) = unmatched.get_mut(play.track.uri.as_str())
            && let Some(nearest) = times
                .iter()
                .enumerate()
                .filter(|&(_, &held)| (held - at).abs() <= SAME_PLAY)
                .min_by_key(|&(_, &held)| (held - at).abs())
                .map(|(index, _)| index)
        {
            times.swap_remove(nearest);
            continue;
        }
        out.push((at, play.clone()));
    }
    out.sort_by(|a, b| match (a.0, b.0) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    out.into_iter().map(|(_, play)| play).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(uri: &str, at: &str) -> PlayHistory {
        PlayHistory {
            track: Track {
                uri: uri.to_string(),
                ..Track::default()
            },
            played_at: Some(at.to_string()),
            context: None,
        }
    }

    /// A play counts after 30 seconds or halfway through a shorter track.
    #[test]
    fn a_song_counts_after_half_a_minute_or_half_of_it() {
        assert_eq!(counts_after(240_000).as_secs(), 30, "a four minute song");
        assert_eq!(counts_after(40_000).as_secs(), 20, "a forty second song");
        // Always require at least one second.
        assert!(counts_after(0) >= std::time::Duration::from_secs(1));
    }

    /// A counted play is not added again on later frames.
    #[test]
    fn counting_never_overflows_however_long_a_song_runs() {
        let threshold = counts_after(240_000);
        let mut listened = std::time::Duration::ZERO;
        let mut recorded = 0;
        // Continue for an hour after crossing the threshold.
        for _ in 0..3_600 {
            listened += std::time::Duration::from_secs(1);
            if recorded == 0 && listened >= threshold {
                recorded += 1;
            }
        }
        assert_eq!(recorded, 1, "written down once, and it did not panic");
    }

    /// Both sources are sorted together, newest first.
    #[test]
    fn the_two_histories_interleave_by_time() {
        let local = vec![
            play("spotify:track:here-late", "2026-09-01T15:00:00Z"),
            play("spotify:track:here-early", "2026-09-01T09:00:00Z"),
        ];
        let remote = vec![play("spotify:track:phone", "2026-09-01T12:00:00Z")];
        let rows = merged(&local, &remote);
        let uris: Vec<&str> = rows.iter().map(|play| play.track.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec![
                "spotify:track:here-late",
                "spotify:track:phone",
                "spotify:track:here-early"
            ]
        );
    }

    /// Separate plays of the same track remain separate rows.
    #[test]
    fn the_same_song_played_twice_is_two_rows() {
        let local = vec![
            play("spotify:track:a", "2026-09-01T15:00:00Z"),
            play("spotify:track:a", "2026-09-01T09:00:00Z"),
        ];
        assert_eq!(merged(&local, &[]).len(), 2);
    }

    /// A play reported by both sources appears once.
    #[test]
    fn one_play_seen_twice_is_one_row() {
        let local = vec![play("spotify:track:a", "2026-09-01T15:00:00Z")];
        let remote = vec![play("spotify:track:a", "2026-09-01T15:00:20Z")];
        assert_eq!(merged(&local, &remote).len(), 1, "twenty seconds apart");
        let distant = vec![play("spotify:track:a", "2026-09-01T15:05:00Z")];
        assert_eq!(merged(&local, &distant).len(), 2, "five minutes apart");
    }

    /// Each source reports every play it heard: a short song played twice
    /// in a row is two plays, however close together they start. Only a
    /// play that both sources report becomes one row.
    #[test]
    fn a_short_song_played_twice_by_one_source_is_two_rows() {
        // A forty-second interlude played twice, on another device.
        let remote = vec![
            play("spotify:track:short", "2026-09-01T15:00:40Z"),
            play("spotify:track:short", "2026-09-01T15:00:00Z"),
        ];
        assert_eq!(merged(&[], &remote).len(), 2, "two plays on another device");
        // The same two plays, made here.
        assert_eq!(merged(&remote, &[]).len(), 2, "two plays on this computer");
        // Made here and reported by Spotify as well: two plays, each once.
        let reported = vec![
            play("spotify:track:short", "2026-09-01T15:00:45Z"),
            play("spotify:track:short", "2026-09-01T15:00:05Z"),
        ];
        assert_eq!(merged(&remote, &reported).len(), 2, "each play once");
    }

    /// A play without a timestamp sorts to the end.
    #[test]
    fn a_play_with_no_time_sinks_to_the_end() {
        let mut timeless = play("spotify:track:timeless", "");
        timeless.played_at = None;
        let local = vec![timeless, play("spotify:track:a", "2026-09-01T09:00:00Z")];
        let rows = merged(&local, &[]);
        let uris: Vec<&str> = rows.iter().map(|play| play.track.uri.as_str()).collect();
        assert_eq!(uris, vec!["spotify:track:a", "spotify:track:timeless"]);
    }

    /// The newest play comes first and the list is capped.
    #[test]
    fn the_newest_play_is_first_and_the_list_is_capped() {
        let mut history = History::default();
        let at: jiff::Timestamp = "2026-09-01T09:00:00Z".parse().unwrap();
        for index in 0..KEPT + 10 {
            history.record(
                Track {
                    uri: format!("spotify:track:{index}"),
                    ..Track::default()
                },
                at,
            );
        }
        assert_eq!(history.plays().len(), KEPT, "the oldest fall off the end");
        assert_eq!(
            history.plays()[0].track.uri,
            format!("spotify:track:{}", KEPT + 9),
            "the newest is first"
        );
    }
}
