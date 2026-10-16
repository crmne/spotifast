//! Account-scoped Liked Songs metadata and optimistic edits.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::api::models::{Page, SavedTrack, Track};
use crate::model::PagedList;

const VERSION: u32 = 1;
const FRESH_SECONDS: i64 = 15 * 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cache {
    version: u32,
    pub account_id: String,
    items: Vec<SavedTrack>,
    total: u32,
    next_offset: Option<u32>,
    refreshed_at: i64,
    changes: BTreeMap<String, Change>,
}

impl Cache {
    pub fn valid_for(&self, account: &str) -> bool {
        self.version == VERSION
            && self.account_id == account
            && self.items.len() <= self.total as usize
            && match self.next_offset {
                Some(offset) => offset == self.items.len() as u32 && offset < self.total,
                None => self.items.len() == self.total as usize,
            }
            && self
                .changes
                .iter()
                .all(|(uri, change)| change.confirmed && uri == &change.item.track.uri)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Change {
    saved: bool,
    item: SavedTrack,
    confirmed: bool,
    /// Only a refresh started after the write can retire its optimistic state.
    #[serde(skip)]
    confirmed_generation: u64,
    /// Whether `item.added_at` was supplied by a Spotify server row.
    #[serde(default)]
    server_origin: bool,
}

struct Refresh {
    rows: PagedList<SavedTrack>,
    through: usize,
}

/// What the last [`LikedSongs::sync_view`] made of the view. While nothing
/// but new pages at the end of the server rows has happened since, the next
/// sync appends those pages instead of rebuilding every row, which made
/// loading a large library quadratic.
struct Synced {
    server_len: usize,
    view_revision: u64,
    view_len: usize,
    /// The view's revision after the last full rebuild: every revision from
    /// it to `view_revision` is a prefix of the view.
    run_start: u64,
    /// Whether each change added a row (true) or removed one (false), in
    /// order, for recounting the total.
    counted: Vec<bool>,
}

#[derive(Default)]
pub struct LikedSongs {
    pub generation: u64,
    pub cache_checked: bool,
    pub cache_loading: bool,
    server: PagedList<SavedTrack>,
    refresh: Option<Refresh>,
    changes: BTreeMap<String, Change>,
    refreshed_at: i64,
    saved_through: Option<u32>,
    synced: Option<Synced>,
}

impl LikedSongs {
    pub fn restore(&mut self, cache: Cache) {
        self.synced = None;
        self.server
            .restore_cached(cache.items, cache.total, cache.next_offset);
        self.refreshed_at = if cache.changes.is_empty() {
            cache.refreshed_at
        } else {
            0
        };
        for (uri, change) in cache.changes {
            self.changes.entry(uri).or_insert(change);
        }
        self.saved_through = Some(self.server.next_offset.unwrap_or(cache.total));
    }

    pub fn fresh(&self, now: i64) -> bool {
        self.server.loaded_once
            && (0..FRESH_SECONDS).contains(&now.saturating_sub(self.refreshed_at))
    }

    pub fn refreshing(&self) -> bool {
        self.refresh.is_some()
    }

    pub fn start_refresh(&mut self, generation: u64) {
        self.generation = generation;
        self.refresh = Some(Refresh {
            rows: PagedList::default(),
            through: self.server.items.len().max(50),
        });
    }

    pub fn request_offset(&mut self) -> Option<u32> {
        if !self.cache_checked {
            return None;
        }
        let rows = self
            .refresh
            .as_mut()
            .map(|r| &mut r.rows)
            .unwrap_or(&mut self.server);
        if !rows.can_load_more() {
            return None;
        }
        let offset = rows.next_offset?;
        rows.loading = true;
        Some(offset)
    }

    /// Keep the last usable rows until a refreshed prefix is equally useful.
    /// Returns whether another refresh page is needed immediately.
    pub fn absorb(&mut self, offset: u32, page: Page<SavedTrack>, now: i64) -> bool {
        if let Some(refresh) = &mut self.refresh {
            refresh.rows.absorb(offset, page);
            if refresh.rows.items.len() < refresh.through && refresh.rows.next_offset.is_some() {
                return true;
            }
            self.server = self.refresh.take().expect("refresh exists").rows;
            self.synced = None;
            self.refreshed_at = now;
            self.saved_through = None;
            let complete = self.server.is_complete();
            let contiguous_prefix = self.server.base_offset == 0 && self.server.windows.is_empty();
            let oldest_loaded_ts = if contiguous_prefix {
                self.server.items.last().and_then(track_timestamp)
            } else {
                None
            };
            self.changes.retain(|uri, change| {
                let server_item = self.server.items.iter().find(|item| &item.track.uri == uri);
                let change_ts = track_timestamp(&change.item);
                let retired = change.confirmed
                    && change.confirmed_generation < self.generation
                    && if change.saved {
                        server_item.is_some()
                    } else if let Some(server_track) = server_item {
                        match (track_timestamp(server_track), change_ts) {
                            (Some(server_ts), Some(change_ts)) => server_ts > change_ts,
                            _ => false,
                        }
                    } else {
                        complete
                            || (change.server_origin
                                && match (change_ts, oldest_loaded_ts) {
                                    (Some(change_ts), Some(oldest_ts)) => change_ts > oldest_ts,
                                    _ => false,
                                })
                    };
                !retired
            });
        } else {
            // A page at the end of the loaded rows leaves those rows as they
            // were; any other page may replace them.
            let appends = self.server.loaded_once
                && offset != 0
                && offset as usize == self.server.items.len()
                && self.server.base_offset == 0
                && self.server.window_request.is_none()
                && self.server.windows.is_empty();
            if !appends {
                self.synced = None;
            }
            self.server.absorb(offset, page);
        }
        false
    }

    pub fn fail(&mut self, error: String) {
        self.refresh = None;
        self.server.fail(error);
    }

    pub fn change(&mut self, uri: String, saved: bool, track: Track) {
        self.synced = None;
        let (item, server_origin) = self
            .server
            .items
            .iter()
            .find(|item| item.track.uri == uri)
            .map(|item| (item.clone(), true))
            .or_else(|| {
                self.changes
                    .get(&uri)
                    .map(|change| (change.item.clone(), change.server_origin))
            })
            .unwrap_or_else(|| {
                (
                    SavedTrack {
                        added_at: Some(jiff::Timestamp::now().to_string()),
                        track,
                    },
                    false,
                )
            });
        self.changes.insert(
            uri,
            Change {
                saved,
                item,
                confirmed: false,
                confirmed_generation: self.generation,
                server_origin,
            },
        );
    }

    pub fn confirm(&mut self, uri: &str, saved: bool, success: bool) {
        self.synced = None;
        if let Some(change) = self.changes.get_mut(uri)
            && change.saved == saved
        {
            change.saved = if success { saved } else { !saved };
            change.confirmed = true;
            change.confirmed_generation = self.generation;
        }
    }

    pub fn intent(&self, uri: &str) -> Option<bool> {
        self.changes.get(uri).map(|change| change.saved)
    }

    pub fn update_track(&mut self, track: &Track) -> bool {
        if let Some(change) = self.changes.get_mut(&track.uri) {
            self.synced = None;
            change.item.track = track.clone();
            return true;
        }
        false
    }

    pub fn has_confirmed_changes(&self) -> bool {
        self.changes.values().any(|change| change.confirmed)
    }

    pub fn intents(&self) -> Vec<(String, bool)> {
        self.changes
            .iter()
            .map(|(uri, change)| (uri.clone(), change.saved))
            .collect()
    }

    /// Demo fixtures and already loaded UI data can seed the same model.
    pub fn seed(&mut self, view: &PagedList<SavedTrack>) {
        if !self.server.loaded_once && view.loaded_once {
            self.synced = None;
            self.server = view.clone();
        }
    }

    pub fn sync_view(&mut self, view: &mut PagedList<SavedTrack>) {
        if !self.append_to_view(view) {
            self.rebuild_view(view);
        }
        view.next_offset = self.server.next_offset;
        view.loading = self.cache_loading || self.refresh.is_some() || self.server.loading;
        view.loaded_once = self.server.loaded_once || !self.changes.is_empty();
        view.error = self.server.error.clone();
    }

    /// Adds the server rows that arrived since the last sync to the end of
    /// the view, when that is all that changed and no change concerns them.
    fn append_to_view(&mut self, view: &mut PagedList<SavedTrack>) -> bool {
        let Some(synced) = &mut self.synced else {
            return false;
        };
        let Some(tail) = self.server.items.get(synced.server_len..) else {
            return false;
        };
        if synced.view_revision != view.revision
            || synced.view_len != view.items.len()
            || tail
                .iter()
                .any(|item| self.changes.contains_key(&item.track.uri))
        {
            return false;
        }
        let total = count(self.server.total.unwrap_or(0), &synced.counted);
        if !tail.is_empty() || view.total != Some(total) {
            view.revision = view.revision.wrapping_add(1);
            view.items.extend_from_slice(tail);
            view.total = Some(total);
        }
        synced.server_len = self.server.items.len();
        synced.view_revision = view.revision;
        synced.view_len = view.items.len();
        true
    }

    fn rebuild_view(&mut self, view: &mut PagedList<SavedTrack>) {
        let mut items = self.server.items.clone();
        let mut additions = Vec::new();
        let mut counted = Vec::new();
        for (uri, change) in &self.changes {
            let index = items.iter().position(|item| &item.track.uri == uri);
            match (change.saved, index) {
                (true, None) => {
                    additions.push(change.item.clone());
                    counted.push(true);
                }
                (false, Some(index)) => {
                    items.remove(index);
                    counted.push(false);
                }
                _ => {}
            }
        }
        let total = count(self.server.total.unwrap_or(0), &counted);
        additions.sort_by(|a, b| b.added_at.cmp(&a.added_at));
        items.splice(0..0, additions);
        if view.items != items || view.total != Some(total) {
            view.revision = view.revision.wrapping_add(1);
            view.items = items;
            view.total = Some(total);
        }
        self.synced = Some(Synced {
            server_len: self.server.items.len(),
            view_revision: view.revision,
            view_len: view.items.len(),
            run_start: view.revision,
            counted,
        });
    }

    /// Whether the view at `revision` is the start of the view as it is now,
    /// with only appended pages after it, so rows made from it can be kept.
    pub fn view_extends(&self, view: &PagedList<SavedTrack>, revision: u64) -> bool {
        self.synced.as_ref().is_some_and(|synced| {
            synced.view_revision == view.revision
                && synced.view_len == view.items.len()
                && revision.wrapping_sub(synced.run_start)
                    <= view.revision.wrapping_sub(synced.run_start)
        })
    }

    pub fn checkpoint(&mut self, account_id: String, force: bool) -> Option<Cache> {
        if !self.server.loaded_once || self.server.error.is_some() || self.refresh.is_some() {
            return None;
        }
        let total = self.server.total?;
        let through = self.server.next_offset.unwrap_or(total);
        if !force
            && self.saved_through.is_some_and(|previous| {
                through == previous
                    || (self.server.next_offset.is_some() && through.saturating_sub(previous) < 500)
            })
        {
            return None;
        }
        self.saved_through = Some(through);
        Some(Cache {
            version: VERSION,
            account_id,
            items: self.server.items.clone(),
            total,
            next_offset: self.server.next_offset,
            refreshed_at: self.refreshed_at,
            changes: self
                .changes
                .iter()
                .filter(|(_, change)| change.confirmed)
                .map(|(uri, change)| (uri.clone(), change.clone()))
                .collect(),
        })
    }
}

/// The server total with each change's row added or removed, in order.
fn count(total: u32, counted: &[bool]) -> u32 {
    counted.iter().fold(total, |total, &added| {
        if added {
            total.saturating_add(1)
        } else {
            total.saturating_sub(1)
        }
    })
}

fn track_timestamp(track: &SavedTrack) -> Option<jiff::Timestamp> {
    track.added_at.as_deref()?.parse().ok()
}

pub async fn read(path: &std::path::Path, account: &str) -> Option<Cache> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let cache: Cache = serde_json::from_slice(&bytes).ok()?;
    cache.valid_for(account).then_some(cache)
}

/// Replaces the cache whole, on a file worker: the write waits for the disk.
pub async fn write(path: &std::path::Path, cache: &Cache) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(cache).map_err(std::io::Error::other)?;
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || fastframe_file::write(&path, bytes))
        .await
        .map_err(std::io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(n: u32) -> SavedTrack {
        SavedTrack {
            track: Track {
                uri: format!("spotify:track:{n}"),
                name: format!("Song {n}"),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn page(offset: u32, count: u32, total: u32) -> Page<SavedTrack> {
        Page {
            offset,
            limit: count,
            total,
            next: (offset + count < total).then(|| "next".into()),
            items: (offset..offset + count).map(item).collect(),
        }
    }

    fn loaded(count: u32, total: u32) -> LikedSongs {
        let mut songs = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        songs.start_refresh(1);
        songs.absorb(0, page(0, count, total), 10_000);
        songs
    }

    #[test]
    fn warm_restart_restores_ten_thousand_rows_without_refetching_fresh_pages() {
        let mut original = loaded(10_000, 10_000);
        let cache = original.checkpoint("alice".into(), false).unwrap();
        let encoded = serde_json::to_vec(&cache).unwrap();
        let mut restored = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        restored.restore(serde_json::from_slice(&encoded).unwrap());
        let mut view = PagedList::default();
        restored.sync_view(&mut view);
        assert_eq!(view.items.len(), 10_000);
        assert!(view.is_complete());
        assert!(restored.fresh(10_001));
        assert_eq!(restored.request_offset(), None);
        assert!(!restored.fresh(10_901));
        assert!(!restored.fresh(9_999));
    }

    #[test]
    fn partial_restart_resumes_from_its_server_offset() {
        let mut original = loaded(500, 10_000);
        let cache = original.checkpoint("alice".into(), false).unwrap();
        let mut restored = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        restored.restore(cache);
        assert_eq!(restored.request_offset(), Some(500));
        assert_eq!(restored.request_offset(), None, "only one page in flight");
        restored.absorb(500, page(500, 50, 10_000), 10_001);
        let mut view = PagedList::default();
        restored.sync_view(&mut view);
        assert_eq!(view.items.len(), 550);
        assert_eq!(view.next_offset, Some(550));
        assert!(!view.is_complete());
    }

    #[test]
    fn a_refresh_keeps_the_old_prefix_until_its_replacement_is_complete() {
        let mut songs = loaded(100, 100);
        let mut view = PagedList::default();
        songs.sync_view(&mut view);
        let original = view.items.clone();
        let revision = view.revision;
        songs.start_refresh(2);
        let mut first = page(0, 50, 100);
        first.items[0].track.name = "Updated".into();
        assert!(songs.absorb(0, first, 20_000));
        songs.sync_view(&mut view);
        assert_eq!(view.items, original);
        assert_eq!(
            view.revision, revision,
            "unchanged visible rows retain their table revision"
        );
        assert!(!songs.absorb(50, page(50, 50, 100), 20_000));
        songs.sync_view(&mut view);
        assert_eq!(view.items.len(), 100);
        assert_eq!(view.items[0].track.name, "Updated");
    }

    #[test]
    fn failed_refresh_keeps_the_last_good_prefix_and_checkpoint() {
        let mut songs = loaded(100, 100);
        let mut view = PagedList::default();
        songs.sync_view(&mut view);
        let original = view.items.clone();
        songs.start_refresh(2);
        songs.absorb(0, page(0, 50, 100), 20_000);
        songs.fail("Spotify is unavailable".into());
        songs.sync_view(&mut view);
        assert_eq!(view.items, original);
        assert!(view.error.is_some());
        assert!(!view.loading);
        assert!(songs.checkpoint("alice".into(), true).is_none());
    }

    #[test]
    fn late_refresh_cannot_hide_a_like_or_resurrect_an_unlike() {
        let mut songs = loaded(100, 100);
        songs.start_refresh(2);
        songs.change(item(999).track.uri, true, item(999).track);
        songs.change(item(0).track.uri, false, item(0).track);
        songs.confirm(&item(999).track.uri, true, true);
        songs.confirm(&item(0).track.uri, false, true);
        songs.absorb(0, page(0, 100, 100), 20_000);
        let mut view = PagedList::default();
        songs.sync_view(&mut view);
        assert_eq!(view.items[0].track.uri, item(999).track.uri);
        assert!(!view.items.iter().any(|x| x.track.uri == item(0).track.uri));
        assert_eq!(view.items.len(), 100);
        let cache = songs.checkpoint("alice".into(), true).unwrap();
        assert_eq!(cache.changes.len(), 2);
        let mut restored = LikedSongs::default();
        restored.restore(cache);
        restored.sync_view(&mut view);
        assert_eq!(view.items[0].track.uri, item(999).track.uri);
        assert!(
            !restored.fresh(20_001),
            "unreconciled edits trigger a refresh on restart"
        );
        songs.start_refresh(3);
        let mut answer = page(0, 100, 100);
        answer.items.remove(0);
        answer.items.insert(0, item(999));
        songs.absorb(0, answer, 20_002);
        assert!(
            songs.changes.is_empty(),
            "a later confirming refresh retires the overrides"
        );
    }

    #[test]
    fn unconfirmed_writes_are_not_persisted_and_failed_writes_restore_the_row() {
        let mut songs = loaded(50, 50);
        songs.change(item(0).track.uri, false, item(0).track);
        let cache = songs.checkpoint("alice".into(), true).unwrap();
        assert!(cache.changes.is_empty());
        let mut view = PagedList::default();
        songs.sync_view(&mut view);
        assert_eq!(view.items.len(), 49);
        songs.confirm(&item(0).track.uri, false, false);
        songs.sync_view(&mut view);
        assert_eq!(view.items.len(), 50);
        assert_eq!(view.items[0].track.uri, item(0).track.uri);
    }

    #[test]
    fn new_likes_follow_added_time_instead_of_uri_order() {
        let mut songs = loaded(50, 50);
        songs.change(item(999).track.uri, true, item(999).track);
        songs.change(item(888).track.uri, true, item(888).track);
        songs
            .changes
            .get_mut(&item(999).track.uri)
            .unwrap()
            .item
            .added_at = Some("2026-09-09T10:00:00Z".into());
        songs
            .changes
            .get_mut(&item(888).track.uri)
            .unwrap()
            .item
            .added_at = Some("2026-09-09T10:00:01Z".into());
        let mut view = PagedList::default();
        songs.sync_view(&mut view);
        assert_eq!(view.items[0].track.uri, item(888).track.uri);
        assert_eq!(view.items[1].track.uri, item(999).track.uri);
        assert_eq!(view.items[2].track.uri, item(0).track.uri);
    }

    /// Syncs, then checks the view against a rebuild from scratch, so the
    /// shortcut for appended pages can never show other rows than a full
    /// sync would.
    fn sync_and_compare(songs: &mut LikedSongs, view: &mut PagedList<SavedTrack>) {
        songs.sync_view(view);
        let held = songs.synced.take();
        let mut rebuilt = PagedList::default();
        songs.rebuild_view(&mut rebuilt);
        songs.synced = held;
        assert_eq!(view.items, rebuilt.items);
        assert_eq!(view.total, rebuilt.total);
    }

    #[test]
    fn appended_pages_show_what_a_full_sync_shows() {
        let mut songs = loaded(50, 400);
        let mut view = PagedList::default();
        sync_and_compare(&mut songs, &mut view);
        let revision = view.revision;
        songs.absorb(50, page(50, 50, 400), 10_000);
        sync_and_compare(&mut songs, &mut view);
        assert_eq!(view.items.len(), 100);
        assert_ne!(view.revision, revision, "new rows are a new revision");
        let revision = view.revision;
        sync_and_compare(&mut songs, &mut view);
        assert_eq!(view.revision, revision, "nothing new, nothing redrawn");
        // A like and an unlike while pages are still arriving, one of them
        // for a song in a page that has not come yet.
        songs.change(item(999).track.uri, true, item(999).track);
        songs.change(item(10).track.uri, false, item(10).track);
        songs.change(item(170).track.uri, false, item(170).track);
        sync_and_compare(&mut songs, &mut view);
        songs.absorb(100, page(100, 50, 400), 10_000);
        sync_and_compare(&mut songs, &mut view);
        songs.absorb(150, page(150, 50, 400), 10_000);
        sync_and_compare(&mut songs, &mut view);
        assert!(
            !view
                .items
                .iter()
                .any(|row| row.track.uri == item(170).track.uri)
        );
        songs.confirm(&item(10).track.uri, false, true);
        songs.absorb(200, page(200, 50, 400), 10_000);
        sync_and_compare(&mut songs, &mut view);
        // Rows dropped from the view by something else are not built on.
        view.retain(|row| row.track.uri != item(60).track.uri);
        songs.absorb(250, page(250, 50, 400), 10_000);
        sync_and_compare(&mut songs, &mut view);
        // A page that is not the next one replaces rows instead of adding.
        songs.absorb(0, page(0, 50, 380), 10_000);
        sync_and_compare(&mut songs, &mut view);
        assert_eq!(view.items.len(), 50);
        songs.absorb(50, page(50, 50, 380), 10_000);
        sync_and_compare(&mut songs, &mut view);
        assert_eq!(view.total, Some(380));
    }

    #[tokio::test]
    async fn corrupt_wrong_account_and_interrupted_cache_writes_are_harmless() {
        let root =
            std::env::temp_dir().join(format!("spotifast-liked-cache-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        let path = root.join("liked.json");
        let mut songs = loaded(50, 100);
        let cache = songs.checkpoint("alice".into(), true).unwrap();
        write(&path, &cache).await.unwrap();
        assert!(read(&path, "alice").await.is_some());
        assert!(read(&path, "bob").await.is_none());
        // What an interrupted write leaves behind, under this version's
        // temporary name and an older one's, is never read as the cache and
        // does not stand in the next write's way.
        for stale in [
            path.with_extension("json.tmp"),
            root.join(".liked.json.1.0.tmp"),
        ] {
            tokio::fs::write(stale, b"{\"half").await.unwrap();
        }
        assert!(
            read(&path, "alice").await.is_some(),
            "interrupted replacement preserves previous data"
        );
        write(&path, &cache).await.unwrap();
        assert!(read(&path, "alice").await.is_some());
        tokio::fs::write(&path, b"broken JSON").await.unwrap();
        assert!(read(&path, "alice").await.is_none());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn incompatible_or_incoherent_records_are_cache_misses() {
        let mut songs = loaded(50, 100);
        let cache = songs.checkpoint("alice".into(), true).unwrap();
        assert!(cache.valid_for("alice"));
        let mut broken = cache.clone();
        broken.version += 1;
        assert!(!broken.valid_for("alice"));
        let mut broken = cache.clone();
        broken.next_offset = None;
        assert!(!broken.valid_for("alice"));
        let mut broken = cache;
        broken.next_offset = Some(75);
        assert!(!broken.valid_for("alice"));
    }

    fn item_with_date(n: u32, added_at: &str) -> SavedTrack {
        SavedTrack {
            added_at: Some(added_at.into()),
            track: Track {
                uri: format!("spotify:track:{n}"),
                name: format!("Song {n}"),
                ..Default::default()
            },
        }
    }

    #[test]
    fn cross_device_relike_with_newer_timestamp_retires_unlike_override() {
        let mut songs = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        songs.start_refresh(1);
        let mut first_page = page(0, 50, 500);
        first_page.items[0] = item_with_date(0, "2026-09-01T10:00:00Z");
        songs.absorb(0, first_page, 10_000);

        songs.change(item(0).track.uri, false, item(0).track);
        songs.confirm(&item(0).track.uri, false, true);
        assert_eq!(songs.changes.len(), 1);

        // A refresh starts (generation 2).
        songs.start_refresh(2);
        // Spotify returns item(0) with a newer timestamp (re-liked on another device).
        let mut refreshed_page = page(0, 50, 500);
        refreshed_page.items[0] = item_with_date(0, "2026-10-15T12:00:00Z");
        songs.absorb(0, refreshed_page, 20_000);

        assert!(
            songs.changes.is_empty(),
            "newer server timestamp retires the unlike override"
        );
        let mut view = PagedList::default();
        songs.sync_view(&mut view);
        assert_eq!(view.items[0].track.uri, item(0).track.uri);
    }

    #[test]
    fn lagging_server_with_equal_or_older_timestamp_retains_unlike_override() {
        let mut songs = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        songs.start_refresh(1);
        let mut first_page = page(0, 50, 500);
        first_page.items[0] = item_with_date(0, "2026-09-01T10:00:00Z");
        songs.absorb(0, first_page, 10_000);

        songs.change(item(0).track.uri, false, item(0).track);
        songs.confirm(&item(0).track.uri, false, true);

        // Spotify lags on DELETE and returns the same timestamp.
        songs.start_refresh(2);
        let mut lagging_page = page(0, 50, 500);
        lagging_page.items[0] = item_with_date(0, "2026-09-01T10:00:00Z");
        songs.absorb(0, lagging_page, 20_000);

        assert_eq!(
            songs.changes.len(),
            1,
            "equal timestamp preserves the override against server lag"
        );
        let mut view = PagedList::default();
        songs.sync_view(&mut view);
        assert!(
            !view.items.iter().any(|x| x.track.uri == item(0).track.uri),
            "unliked track remains suppressed in the view"
        );
    }

    #[test]
    fn missing_or_invalid_timestamps_retain_unlike_override() {
        let mut songs = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        songs.start_refresh(1);
        let mut first_page = page(0, 50, 500);
        first_page.items[0] = item_with_date(0, "2026-09-01T10:00:00Z");
        songs.absorb(0, first_page, 10_000);

        songs.change(item(0).track.uri, false, item(0).track);
        songs.confirm(&item(0).track.uri, false, true);

        // Server returns track with invalid timestamp string.
        songs.start_refresh(2);
        let mut invalid_page = page(0, 50, 500);
        invalid_page.items[0].added_at = Some("not-a-valid-date".into());
        songs.absorb(0, invalid_page, 20_000);
        assert_eq!(songs.changes.len(), 1, "invalid timestamp retains override");

        // Server returns track with missing timestamp.
        songs.start_refresh(3);
        let mut missing_page = page(0, 50, 500);
        missing_page.items[0].added_at = None;
        songs.absorb(0, missing_page, 20_000);
        assert_eq!(songs.changes.len(), 1, "missing timestamp retains override");
    }

    #[test]
    fn partial_library_absent_track_retired_when_newer_than_oldest_loaded() {
        let mut songs = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        songs.start_refresh(1);
        let mut first_page = page(0, 50, 500);
        first_page.items[0] = item_with_date(0, "2026-08-01T00:00:00Z");
        first_page.items[49] = item_with_date(49, "2026-05-01T00:00:00Z");
        songs.absorb(0, first_page, 10_000);

        songs.change(item(0).track.uri, false, item(0).track);
        songs.confirm(&item(0).track.uri, false, true);

        // Refresh completes and the 50 items do not contain item(0).
        // Since item(0) was 2026-08-01 and the last item is 2026-05-01,
        // it cannot be on later pages.
        songs.start_refresh(2);
        let mut next_page = page(0, 50, 500);
        for (i, it) in next_page.items.iter_mut().enumerate() {
            it.added_at = Some(format!("2026-07-{:02}T00:00:00Z", 29 - (i % 28)));
        }
        next_page.items[0] = item_with_date(999, "2026-07-30T00:00:00Z");
        next_page.items[49] = item_with_date(49, "2026-05-01T00:00:00Z");
        songs.absorb(0, next_page, 20_000);

        assert!(
            songs.changes.is_empty(),
            "deletion confirmed because track is absent from range newer than oldest loaded"
        );
    }

    #[test]
    fn partial_library_absent_track_retained_on_boundary_or_older() {
        let mut songs = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        songs.start_refresh(1);
        let mut first_page = page(0, 50, 500);
        first_page.items[0] = item_with_date(0, "2026-05-01T00:00:00Z");
        first_page.items[49] = item_with_date(49, "2026-05-01T00:00:00Z");
        songs.absorb(0, first_page, 10_000);

        songs.change(item(0).track.uri, false, item(0).track);
        songs.confirm(&item(0).track.uri, false, true);

        // Refresh completes without item(0), but oldest is equal (boundary tie) or newer.
        songs.start_refresh(2);
        let mut next_page = page(0, 50, 500);
        for it in next_page.items.iter_mut() {
            it.added_at = Some("2026-05-01T00:00:00Z".into());
        }
        next_page.items[0] = item_with_date(999, "2026-05-01T00:00:00Z");
        songs.absorb(0, next_page, 20_000);

        assert_eq!(
            songs.changes.len(),
            1,
            "boundary tie must preserve override because item may be on next page"
        );
    }

    #[test]
    fn generation_safety_unconfirmed_and_concurrent_writes_preserved() {
        let mut songs = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        songs.start_refresh(1);
        songs.absorb(0, page(0, 50, 500), 10_000);

        // Change 1: created during refresh generation 2, unconfirmed.
        songs.start_refresh(2);
        songs.change(item(10).track.uri, false, item(10).track);

        // Change 2: confirmed during generation 2 (confirmed_generation == 2).
        songs.change(item(20).track.uri, false, item(20).track);
        songs.confirm(&item(20).track.uri, false, true);

        let mut next_page = page(0, 50, 500);
        next_page.items.remove(10);
        next_page.items.remove(19);
        songs.absorb(0, next_page, 20_000);

        assert_eq!(
            songs.changes.len(),
            2,
            "both unconfirmed and concurrently confirmed changes are preserved"
        );
    }

    #[test]
    fn unloaded_track_unlike_with_placeholder_timestamp_is_not_retired_by_prefix() {
        let mut songs = LikedSongs {
            cache_checked: true,
            ..Default::default()
        };
        songs.start_refresh(1);
        let mut first_page = page(0, 50, 500);
        first_page.items[0] = item_with_date(0, "2026-08-01T00:00:00Z");
        first_page.items[49] = item_with_date(49, "2026-05-01T00:00:00Z");
        songs.absorb(0, first_page, 10_000);

        // Track 999 is unliked when not in server.items (placeholder timestamp).
        songs.change(item(999).track.uri, false, item(999).track);
        songs.confirm(&item(999).track.uri, false, true);

        // Refresh completes for page 0 without track 999.
        songs.start_refresh(2);
        let mut next_page = page(0, 50, 500);
        next_page.items[0] = item_with_date(0, "2026-08-01T00:00:00Z");
        next_page.items[49] = item_with_date(49, "2026-05-01T00:00:00Z");
        songs.absorb(0, next_page, 20_000);

        assert_eq!(
            songs.changes.len(),
            1,
            "placeholder timestamp must not authorize partial-prefix deletion inference"
        );
    }
}
