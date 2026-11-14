//! Which of the account's own playlists hold each song, so a heart can
//! tell a song saved in any playlist from one saved nowhere and its
//! checklist can tick every playlist that holds it. Liked Songs is not
//! here: the saved map answers for it.
//!
//! Each editable playlist is read once over the playback session, one
//! request per thousand rows, and again when the library reports a new
//! snapshot for it. Edits made here change the index at once and hold
//! back reads of that playlist until Spotify answers them, so an answer
//! from before an edit cannot undo it; an edit that fails is undone.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Reads on their way at once. The playback session's background reads
/// share four slots, so two stay free for the page on screen.
const READS_AT_ONCE: usize = 2;
/// How long a playlist whose read failed waits before it is asked again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct Membership {
    lists: HashMap<String, List>,
    /// How many lists hold each song.
    anywhere: HashMap<String, u32>,
    /// Reads on their way, by playlist id.
    reading: HashMap<String, Reading>,
    /// Edits sent and not yet answered, by playlist id.
    writing: HashMap<String, u32>,
    failed: HashMap<String, Instant>,
    /// Each editable playlist's snapshot as the library last reported it.
    library: HashMap<String, Option<String>>,
}

#[derive(Default)]
struct List {
    /// The library snapshot the rows were read at.
    snapshot: Option<String>,
    /// How many rows hold each song.
    counts: HashMap<String, u32>,
    /// Every row is known, so a song missing from `counts` is not there.
    complete: bool,
    /// Changed elsewhere since it was read, so it is read again whatever
    /// snapshot it is at.
    outdated: bool,
    /// The rows each song gained or lost through edits not answered yet,
    /// which are put back if one of them fails.
    unsettled: HashMap<String, i64>,
    /// One of the edits not settled yet failed.
    edit_failed: bool,
}

struct Reading {
    snapshot: Option<String>,
    /// An edit since the read was asked makes its answer out of date.
    stale: bool,
}

impl Membership {
    fn wants(&self, id: &str, snapshot: Option<&str>) -> bool {
        !self.reading.contains_key(id)
            && self.writing.get(id).is_none_or(|count| *count == 0)
            && self
                .failed
                .get(id)
                .is_none_or(|at| at.elapsed() >= RETRY_AFTER)
            && !self.lists.get(id).is_some_and(|list| {
                list.complete && !list.outdated && list.snapshot.as_deref() == snapshot
            })
    }

    /// Takes the account's editable playlists (id and library snapshot),
    /// forgets those no longer among them, and returns the ids to read
    /// now, marked as being read.
    pub(crate) fn start_reads(&mut self, playlists: &[(String, Option<String>)]) -> Vec<String> {
        let editable: HashSet<&str> = playlists.iter().map(|(id, _)| id.as_str()).collect();
        let gone: Vec<String> = self
            .lists
            .keys()
            .filter(|id| !editable.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            self.forget(&id);
        }
        self.library = playlists.iter().cloned().collect();
        let mut started = Vec::new();
        for (id, snapshot) in playlists {
            if self.reading.len() >= READS_AT_ONCE {
                break;
            }
            if self.wants(id, snapshot.as_deref()) {
                self.reading.insert(
                    id.clone(),
                    Reading {
                        snapshot: snapshot.clone(),
                        stale: false,
                    },
                );
                started.push(id.clone());
            }
        }
        started
    }

    fn forget(&mut self, id: &str) {
        if let Some(list) = self.lists.remove(id) {
            for uri in list.counts.keys() {
                lost(&mut self.anywhere, uri);
            }
        }
    }

    /// Takes a read's answer: every song in the playlist `id`.
    pub(crate) fn receive(&mut self, id: &str, result: Result<Vec<String>, String>) {
        let Some(reading) = self.reading.remove(id) else {
            return;
        };
        match result {
            Ok(uris) => {
                self.failed.remove(id);
                if reading.stale {
                    return;
                }
                let mut counts = HashMap::new();
                for uri in uris {
                    *counts.entry(uri).or_insert(0u32) += 1;
                }
                self.forget(id);
                for uri in counts.keys() {
                    *self.anywhere.entry(uri.clone()).or_insert(0) += 1;
                }
                self.lists.insert(
                    id.to_string(),
                    List {
                        snapshot: reading.snapshot,
                        counts,
                        complete: true,
                        ..List::default()
                    },
                );
            }
            Err(error) => {
                log::debug!("could not read which songs a playlist holds: {error}");
                self.failed.insert(id.to_string(), Instant::now());
            }
        }
    }

    /// Whether the playlist `id` holds `uri`; `None` until it has been read.
    pub(crate) fn holds(&self, id: &str, uri: &str) -> Option<bool> {
        let list = self.lists.get(id)?;
        if list.counts.contains_key(uri) {
            Some(true)
        } else {
            list.complete.then_some(false)
        }
    }

    /// Whether any of the account's playlists is known to hold `uri`.
    pub(crate) fn holds_anywhere(&self, uri: &str) -> bool {
        self.anywhere.contains_key(uri)
    }

    /// The playlist `id` was read at `snapshot` for its page. A snapshot
    /// other than the index's means the playlist changed since its read.
    pub(crate) fn seen(&mut self, id: &str, snapshot: Option<&str>) {
        if self.writing.contains_key(id) || self.reading.contains_key(id) {
            return;
        }
        if let Some(list) = self.lists.get_mut(id)
            && list.complete
            && snapshot.is_some()
            && list.snapshot.as_deref() != snapshot
        {
            list.outdated = true;
        }
    }

    /// An edit of the playlist `id` is on its way: reads wait for it.
    pub(crate) fn edit_started(&mut self, id: &str) {
        *self.writing.entry(id.to_string()).or_insert(0) += 1;
        let read_on_way = self.reading.contains_key(id);
        if let Some(reading) = self.reading.get_mut(id) {
            reading.stale = true;
        }
        let library = self.library.get(id);
        // The edit's snapshot covers only what the index knows; a change
        // made elsewhere since the read still needs a read of its own.
        if let Some(list) = self.lists.get_mut(id)
            && (read_on_way || library.is_some_and(|at| *at != list.snapshot))
        {
            list.outdated = true;
        }
    }

    /// Spotify confirmed an edit of the playlist `id`, which is now at
    /// `snapshot`. The edit is already in the index, so the new snapshot
    /// needs no read of its own.
    pub(crate) fn edit_confirmed(&mut self, id: &str, snapshot: Option<String>) {
        self.edit_answered(id, false);
        if let Some(list) = self.lists.get_mut(id)
            && list.complete
        {
            list.snapshot = snapshot;
        }
    }

    /// An edit of the playlist `id` failed. Once its other edits are
    /// answered, the rows they changed are put back and the playlist is
    /// read again, since the index can no longer be sure of it.
    pub(crate) fn edit_failed(&mut self, id: &str) {
        self.edit_answered(id, true);
    }

    fn edit_answered(&mut self, id: &str, failed: bool) {
        let left = self.writing.get_mut(id).map_or(0, |count| {
            *count = count.saturating_sub(1);
            *count
        });
        if left == 0 {
            self.writing.remove(id);
        }
        let Some(list) = self.lists.get_mut(id) else {
            return;
        };
        list.edit_failed |= failed;
        if left > 0 {
            return;
        }
        let unsettled = std::mem::take(&mut list.unsettled);
        if std::mem::take(&mut list.edit_failed) {
            for (uri, rows) in unsettled {
                shift(list, &mut self.anywhere, &uri, -rows);
            }
            list.unsettled.clear();
            list.complete = false;
        }
    }

    /// Songs added to the playlist `id` here, one row each.
    pub(crate) fn add<'a>(&mut self, id: &str, uris: impl IntoIterator<Item = &'a str>) {
        self.edit_started(id);
        let list = self.lists.entry(id.to_string()).or_default();
        for uri in uris {
            shift(list, &mut self.anywhere, uri, 1);
        }
    }

    /// Every row of each song taken out of the playlist `id` here.
    pub(crate) fn remove_all<'a>(&mut self, id: &str, uris: impl IntoIterator<Item = &'a str>) {
        self.edit_started(id);
        let Some(list) = self.lists.get_mut(id) else {
            return;
        };
        for uri in uris {
            let rows = list.counts.get(uri).copied().unwrap_or(0);
            shift(list, &mut self.anywhere, uri, -i64::from(rows));
        }
    }

    /// One row per entry taken out of the playlist `id` here; other rows
    /// of the same song stay.
    pub(crate) fn remove_rows<'a>(&mut self, id: &str, uris: impl IntoIterator<Item = &'a str>) {
        self.edit_started(id);
        let Some(list) = self.lists.get_mut(id) else {
            return;
        };
        for uri in uris {
            shift(list, &mut self.anywhere, uri, -1);
        }
    }
}

/// Gives `uri` `by` more rows in `list` (fewer when negative, never below
/// none), noting the change as unsettled and keeping `anywhere` in step.
fn shift(list: &mut List, anywhere: &mut HashMap<String, u32>, uri: &str, by: i64) {
    let before = list.counts.get(uri).copied().unwrap_or(0);
    let wanted = (i64::from(before) + by).max(0);
    let after = u32::try_from(wanted).unwrap_or(u32::MAX);
    if after == before {
        return;
    }
    *list.unsettled.entry(uri.to_string()).or_insert(0) += i64::from(after) - i64::from(before);
    if after == 0 {
        list.counts.remove(uri);
        lost(anywhere, uri);
    } else {
        list.counts.insert(uri.to_string(), after);
        if before == 0 {
            *anywhere.entry(uri.to_string()).or_insert(0) += 1;
        }
    }
}

/// One list fewer holds `uri`.
fn lost(anywhere: &mut HashMap<String, u32>, uri: &str) {
    if let Some(count) = anywhere.get_mut(uri) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            anywhere.remove(uri);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playlists(ids: &[(&str, &str)]) -> Vec<(String, Option<String>)> {
        ids.iter()
            .map(|(id, at)| (id.to_string(), Some(at.to_string())))
            .collect()
    }

    fn uris(list: &[&str]) -> Result<Vec<String>, String> {
        Ok(list.iter().map(|uri| uri.to_string()).collect())
    }

    #[test]
    fn a_read_playlist_answers_for_its_songs_and_an_unread_one_does_not() {
        let mut index = Membership::default();
        let all = playlists(&[("a", "1"), ("b", "1"), ("c", "1")]);
        assert_eq!(index.start_reads(&all), ["a", "b"], "two at once");
        assert!(index.start_reads(&all).is_empty(), "no slot is free");
        assert_eq!(index.holds("a", "song:1"), None, "not read yet");

        index.receive("a", uris(&["song:1", "song:1"]));
        assert_eq!(index.holds("a", "song:1"), Some(true));
        assert_eq!(index.holds("a", "song:2"), Some(false));
        assert!(index.holds_anywhere("song:1"));
        assert!(!index.holds_anywhere("song:2"));
        assert_eq!(index.start_reads(&all), ["c"], "a slot came free");
        index.receive("b", uris(&["song:2"]));
        assert!(index.start_reads(&all).is_empty(), "all read or reading");
    }

    #[test]
    fn a_new_snapshot_reads_again_and_a_playlist_no_longer_editable_is_forgotten() {
        let mut index = Membership::default();
        index.start_reads(&playlists(&[("a", "1"), ("b", "1")]));
        index.receive("a", uris(&["song:1"]));
        index.receive("b", uris(&["song:1"]));

        assert_eq!(index.start_reads(&playlists(&[("a", "2")])), ["a"]);
        assert_eq!(index.holds("b", "song:1"), None, "b is forgotten");
        assert!(index.holds_anywhere("song:1"), "a still holds it");
        index.receive("a", uris(&["song:2"]));
        assert!(!index.holds_anywhere("song:1"));
        assert!(index.holds_anywhere("song:2"));
    }

    #[test]
    fn an_edit_during_a_read_drops_its_answer_and_holds_back_the_next_read() {
        let mut index = Membership::default();
        let all = playlists(&[("a", "1")]);
        index.start_reads(&all);
        index.add("a", ["song:new"]);
        index.receive("a", uris(&["song:old"]));
        assert_eq!(index.holds("a", "song:new"), Some(true));
        assert_eq!(index.holds("a", "song:old"), None, "answer dropped");
        assert!(index.start_reads(&all).is_empty(), "the edit is unanswered");

        index.edit_confirmed("a", Some("1".into()));
        assert_eq!(index.start_reads(&all), ["a"], "the drop left it unread");
        index.receive("a", uris(&["song:old", "song:new"]));
        assert_eq!(index.holds("a", "song:new"), Some(true));
        assert_eq!(index.holds("a", "song:old"), Some(true));
    }

    #[test]
    fn a_confirmed_edit_needs_no_read_and_a_failed_one_reads_again() {
        let mut index = Membership::default();
        index.start_reads(&playlists(&[("a", "1"), ("b", "1")]));
        index.receive("a", uris(&["song:1"]));
        index.receive("b", uris(&["song:1"]));
        index.add("a", ["song:2"]);
        index.remove_all("b", ["song:1"]);
        index.edit_confirmed("a", Some("2".into()));
        index.edit_failed("b");

        let now = playlists(&[("a", "2"), ("b", "1")]);
        assert_eq!(index.start_reads(&now), ["b"]);
        assert_eq!(index.holds("a", "song:2"), Some(true));
        assert_eq!(
            index.holds("b", "song:1"),
            Some(true),
            "the removal is undone"
        );
    }

    #[test]
    fn a_failed_edit_is_undone_once_the_playlists_edits_are_answered() {
        let mut index = Membership::default();
        let all = playlists(&[("a", "1")]);
        index.start_reads(&all);
        index.receive("a", uris(&["song:1"]));
        index.add("a", ["song:2"]);
        index.remove_rows("a", ["song:1"]);
        index.edit_failed("a");
        assert!(index.holds_anywhere("song:2"), "an edit is unanswered");

        index.edit_confirmed("a", Some("2".into()));
        assert!(!index.holds_anywhere("song:2"), "the failed add is undone");
        assert_eq!(index.holds("a", "song:1"), Some(true), "and the removal");
        assert_eq!(index.holds("a", "song:3"), None, "a is unsure");
        assert_eq!(index.start_reads(&all), ["a"]);
    }

    #[test]
    fn an_edit_does_not_vouch_for_a_change_made_elsewhere() {
        let mut index = Membership::default();
        index.start_reads(&playlists(&[("a", "1")]));
        index.receive("a", uris(&["song:1"]));

        // #given another app changed it and its new read is on its way
        assert_eq!(index.start_reads(&playlists(&[("a", "2")])), ["a"]);
        // #when an edit interrupts that read
        index.add("a", ["song:2"]);
        index.receive("a", uris(&["song:1", "song:3"]));
        index.edit_confirmed("a", Some("3".into()));
        // #then the edit's snapshot does not stand for the other change
        assert_eq!(index.start_reads(&playlists(&[("a", "3")])), ["a"]);
        index.receive("a", uris(&["song:1", "song:3", "song:2"]));
        assert!(index.holds_anywhere("song:3"));

        // #given another change while every read slot is taken
        let busy = playlists(&[("x", "1"), ("y", "1"), ("a", "4")]);
        assert_eq!(index.start_reads(&busy), ["x", "y"]);
        // #when an edit is confirmed before a reads it
        index.add("a", ["song:4"]);
        index.edit_confirmed("a", Some("5".into()));
        index.receive("x", uris(&[]));
        index.receive("y", uris(&[]));
        // #then a is still read
        let now = playlists(&[("x", "1"), ("y", "1"), ("a", "5")]);
        assert_eq!(index.start_reads(&now), ["a"]);
    }

    #[test]
    fn a_page_read_at_another_snapshot_reads_the_playlist_again() {
        let mut index = Membership::default();
        let all = playlists(&[("a", "1")]);
        index.start_reads(&all);
        index.receive("a", uris(&["song:1"]));
        index.seen("a", Some("1"));
        assert!(index.start_reads(&all).is_empty(), "the page agrees");

        index.seen("a", Some("2"));
        assert_eq!(index.start_reads(&all), ["a"], "the page is newer");
        index.receive("a", uris(&["song:1", "song:2"]));
        assert!(index.holds_anywhere("song:2"));
        assert!(index.start_reads(&all).is_empty());
    }

    #[test]
    fn removing_rows_keeps_a_song_with_rows_left_and_removing_it_all_does_not() {
        let mut index = Membership::default();
        index.start_reads(&playlists(&[("a", "1"), ("b", "1")]));
        index.receive("a", uris(&["song:1", "song:1", "song:2"]));
        index.receive("b", uris(&["song:2"]));

        index.remove_rows("a", ["song:1"]);
        assert_eq!(index.holds("a", "song:1"), Some(true), "a row is left");
        index.remove_rows("a", ["song:1"]);
        assert_eq!(index.holds("a", "song:1"), Some(false));
        assert!(!index.holds_anywhere("song:1"));

        index.remove_all("a", ["song:2"]);
        assert_eq!(index.holds("a", "song:2"), Some(false));
        assert!(index.holds_anywhere("song:2"), "b still holds it");
    }

    #[test]
    fn a_failed_read_waits_before_it_is_asked_again() {
        let mut index = Membership::default();
        let all = playlists(&[("a", "1")]);
        index.start_reads(&all);
        index.receive("a", Err("offline".into()));
        assert_eq!(index.holds("a", "song:1"), None);
        assert!(index.start_reads(&all).is_empty(), "too soon to retry");
        let long_ago = Instant::now() - RETRY_AFTER;
        index.failed.insert("a".into(), long_ago);
        assert_eq!(index.start_reads(&all), ["a"]);
    }
}
