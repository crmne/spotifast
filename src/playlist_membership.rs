//! Playlist membership and the unsaved choices in the playlist picker.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::api::models::{Page, PlayableItem, Playlist};
use crate::app::App;
use crate::backend::ApiRequest;
use crate::model::Action;

#[derive(Clone, Debug, Default)]
pub struct Index {
    entries: HashMap<String, Entry>,
    serial: u64,
}

#[derive(Clone, Debug, Default)]
struct Entry {
    snapshot: Option<String>,
    uris: HashSet<String>,
    incoming: HashSet<String>,
    changes: HashMap<String, bool>,
    offset: u32,
    total: Option<u32>,
    complete: bool,
    loading: bool,
    error: Option<String>,
    generation: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub items: Vec<PlayableItem>,
    pub query: String,
    /// None means leave this playlist as it was, including partial membership.
    pub choices: BTreeMap<String, bool>,
    /// Preserve what was shown when a row was changed, even during a refresh.
    pub originals: BTreeMap<String, Vec<bool>>,
    pub new_name: Option<String>,
    /// The invoking control in viewport coordinates, captured once on opening.
    pub anchor: Option<egui::Rect>,
    /// Freeze the measured position so filtering never chases the pointer or reanchors.
    pub position: Option<egui::Pos2>,
    pub highlighted: Option<String>,
    pub focus_search: bool,
    pub liked_error: bool,
}

pub const LIKED: &str = "liked";

impl Index {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.serial += 1;
    }

    pub fn contains(&self, id: &str, uri: &str) -> Option<bool> {
        let entry = self.entries.get(id)?;
        entry.changes.get(uri).copied().or_else(|| {
            if entry.uris.contains(uri) || entry.incoming.contains(uri) {
                Some(true)
            } else {
                entry.complete.then_some(false)
            }
        })
    }

    pub fn error(&self, id: &str) -> Option<&str> {
        self.entries.get(id)?.error.as_deref()
    }

    pub fn retry(&mut self) {
        for entry in self.entries.values_mut() {
            entry.error = None;
        }
    }

    pub fn failed_write(&mut self, id: &str) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.changes.clear();
            entry.complete = false;
            entry.offset = 0;
            entry.total = None;
            entry.error = None;
        }
    }

    /// A local edit wins over every read that was started before it.
    pub fn change(&mut self, id: &str, uris: impl IntoIterator<Item = String>, present: bool) {
        self.serial += 1;
        let entry = self.entries.entry(id.into()).or_default();
        entry.generation = self.serial;
        entry.loading = false;
        for uri in uris {
            entry.changes.insert(uri, present);
        }
    }

    pub fn observe(&mut self, id: &str, snapshot: Option<String>, uris: HashSet<String>) {
        let entry = self.entries.entry(id.into()).or_default();
        if entry.loading {
            return;
        }
        entry.snapshot = snapshot;
        entry.uris = uris;
        entry.complete = true;
        entry.error = None;
    }

    pub fn needs_observation(&self, id: &str, snapshot: &Option<String>) -> bool {
        self.entries
            .get(id)
            .is_none_or(|entry| !entry.complete || entry.snapshot != *snapshot)
    }

    /// Two paged background reads at most, leaving room for the visible page.
    pub fn requests(&mut self, playlists: &[Playlist]) -> Vec<ApiRequest> {
        if self.entries.len() == playlists.len()
            && playlists.iter().all(|playlist| {
                self.entries.get(&playlist.id).is_some_and(|entry| {
                    entry.snapshot == playlist.snapshot_id
                        && (entry.complete || entry.loading || entry.error.is_some())
                })
            })
        {
            return Vec::new();
        }
        let ids: HashSet<_> = playlists
            .iter()
            .map(|playlist| playlist.id.as_str())
            .collect();
        self.entries.retain(|id, _| ids.contains(id.as_str()));
        let mut room =
            2usize.saturating_sub(self.entries.values().filter(|entry| entry.loading).count());
        let mut requests = Vec::new();
        for playlist in playlists {
            if room == 0 {
                break;
            }
            let entry = self.entries.entry(playlist.id.clone()).or_default();
            if entry.snapshot != playlist.snapshot_id && !entry.loading {
                entry.snapshot = playlist.snapshot_id.clone();
                entry.offset = 0;
                entry.total = None;
                entry.incoming.clear();
                entry.complete = false;
                entry.error = None;
                // Keep positive knowledge until its replacement is complete.
            }
            if entry.complete || entry.loading || entry.error.is_some() {
                continue;
            }
            self.serial += 1;
            entry.generation = self.serial;
            entry.loading = true;
            requests.push(ApiRequest::PlaylistMembership {
                id: playlist.id.clone(),
                offset: entry.offset,
                generation: entry.generation,
            });
            room -= 1;
        }
        requests
    }

    pub fn receive(
        &mut self,
        id: &str,
        generation: u64,
        result: crate::backend::ApiResult<Page<String>>,
    ) {
        let Some(entry) = self
            .entries
            .get_mut(id)
            .filter(|entry| entry.generation == generation && entry.loading)
        else {
            return;
        };
        entry.loading = false;
        match result {
            Ok(page) => {
                if page.offset != entry.offset
                    || entry.total.is_some_and(|total| total != page.total)
                    || (page.next.is_some() && page.items.is_empty())
                {
                    entry.error = Some("Playlist changed while checking its songs".into());
                    entry.offset = 0;
                    entry.total = None;
                    return;
                }
                if page.offset == 0 {
                    entry.incoming.clear();
                }
                entry.total = Some(page.total);
                entry.offset = page.offset.saturating_add(page.items.len() as u32);
                entry.complete = page.next.is_none();
                entry
                    .incoming
                    .extend(page.items.into_iter().filter(|uri| !uri.is_empty()));
                if entry.complete {
                    entry.uris = std::mem::take(&mut entry.incoming);
                    entry
                        .changes
                        .retain(|uri, present| entry.uris.contains(uri) != *present);
                }
            }
            Err(error) => entry.error = Some(error.to_string()),
        }
    }
}

impl App {
    /// A checked song belongs to Liked Songs or any playlist in the library.
    pub fn in_any_playlist(&self, uri: &str) -> bool {
        self.is_saved(uri) == Some(true)
            || self.library.playlists.get().is_some_and(|playlists| {
                playlists.iter().any(|playlist| {
                    self.playlist_membership.contains(&playlist.id, uri) == Some(true)
                        || self.playlist_pages.get(&playlist.id).is_some_and(|page| {
                            page.items
                                .items
                                .iter()
                                .filter_map(crate::api::models::PlaylistItem::playable)
                                .any(|item| item.uri() == uri)
                                || page.local_additions.contains(uri)
                        })
                })
            })
    }

    pub fn picker_membership(&self, id: &str, items: &[PlayableItem]) -> Option<Vec<bool>> {
        items
            .iter()
            .map(|item| {
                if id == LIKED {
                    self.is_saved(item.uri())
                } else {
                    self.playlist_membership.contains(id, item.uri())
                }
            })
            .collect()
    }

    pub(crate) fn open_playlist_picker(&mut self, items: Vec<PlayableItem>) {
        let mut seen = HashSet::new();
        let items: Vec<_> = items
            .into_iter()
            .filter(|item| item.is_track() && seen.insert(item.uri().to_string()))
            .collect();
        if items.is_empty() {
            return;
        }
        self.request_contains(items.iter().map(|item| item.uri().to_string()).collect());
        self.playlist_picker = Some(Draft {
            items,
            focus_search: true,
            ..Default::default()
        });
        self.sync_playlist_membership();
    }

    pub(crate) fn sync_playlist_membership(&mut self) {
        let Some(playlists) = self.library.playlists.get() else {
            return;
        };
        // A complete page is already a membership answer, with no extra request.
        for playlist in playlists {
            if let Some(page) = self.playlist_pages.get(&playlist.id).filter(|page| {
                page.items.is_complete()
                    && page.pending_writes == 0
                    && self
                        .playlist_membership
                        .needs_observation(&playlist.id, &playlist.snapshot_id)
            }) {
                self.playlist_membership.observe(
                    &playlist.id,
                    playlist.snapshot_id.clone(),
                    page.items
                        .items
                        .iter()
                        .filter_map(crate::api::models::PlaylistItem::playable)
                        .map(|item| item.uri().to_string())
                        .collect(),
                );
            }
        }
        if self.offline || !self.network_reachable || !self.is_connected() {
            return;
        }
        for request in self.playlist_membership.requests(playlists) {
            self.backend.api(request);
        }
    }

    pub(crate) fn commit_playlist_picker(&mut self) {
        let Some(draft) = self.playlist_picker.take() else {
            return;
        };
        for (id, present) in &draft.choices {
            let Some(membership) = self
                .picker_membership(id, &draft.items)
                .or_else(|| draft.originals.get(id).cloned())
            else {
                continue;
            };
            let items: Vec<_> = draft
                .items
                .iter()
                .zip(membership)
                .filter(|(_, was)| was != present)
                .map(|(item, _)| item.clone())
                .collect();
            if items.is_empty() {
                continue;
            }
            if id == LIKED {
                self.actions.push(Action::SetSavedMany {
                    uris: items.iter().map(|item| item.uri().into()).collect(),
                    saved: *present,
                });
            } else if let Some(playlist) = self
                .library
                .playlists
                .get()
                .and_then(|playlists| playlists.iter().find(|playlist| playlist.id == *id))
                .filter(|playlist| self.can_edit_playlist(playlist))
            {
                if *present {
                    self.actions.push(Action::ConfirmAddToPlaylist {
                        playlist_id: id.clone(),
                        playlist_name: playlist.name.clone(),
                        items,
                        position: None,
                    });
                } else {
                    self.actions.push(Action::RemoveFromPlaylist {
                        playlist_id: id.clone(),
                        uris: items.iter().map(|item| item.uri().into()).collect(),
                        positions: Vec::new(),
                    });
                }
            }
        }
        if let Some(name) = draft.new_name.filter(|name| !name.trim().is_empty()) {
            self.actions.push(Action::CreatePlaylist {
                name: name.trim().into(),
                public: false,
                add_uris: draft.items.iter().map(|item| item.uri().into()).collect(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playlist(id: &str) -> Playlist {
        Playlist {
            id: id.into(),
            snapshot_id: Some("one".into()),
            ..Default::default()
        }
    }

    fn request(index: &mut Index, playlists: &[Playlist]) -> (String, u64) {
        let ApiRequest::PlaylistMembership { id, generation, .. } =
            index.requests(playlists).remove(0)
        else {
            unreachable!()
        };
        (id, generation)
    }

    #[test]
    fn pages_do_not_report_absence_until_the_whole_playlist_has_been_read() {
        let mut index = Index::default();
        let playlists = [playlist("a")];
        let (id, generation) = request(&mut index, &playlists);
        index.receive(
            &id,
            generation,
            Ok(Page {
                items: vec!["first".into()],
                total: 2,
                limit: 1,
                next: Some("next".into()),
                ..Default::default()
            }),
        );
        assert_eq!(index.contains(&id, "first"), Some(true));
        assert_eq!(index.contains(&id, "last"), None);
        let (_, generation) = request(&mut index, &playlists);
        index.receive(
            &id,
            generation,
            Ok(Page {
                items: vec!["last".into()],
                total: 2,
                offset: 1,
                limit: 1,
                next: None,
            }),
        );
        assert_eq!(index.contains(&id, "last"), Some(true));
        assert_eq!(index.contains(&id, "absent"), Some(false));
    }

    #[test]
    fn a_late_read_cannot_undo_an_edit_or_leak_across_sign_out() {
        let mut index = Index::default();
        let playlists = [playlist("a")];
        let (id, generation) = request(&mut index, &playlists);
        index.change(&id, ["new".into()], true);
        index.receive(&id, generation, Ok(Page::whole(vec![])));
        assert_eq!(index.contains(&id, "new"), Some(true));
        index.change(&id, ["old".into()], false);
        let (_, generation) = request(&mut index, &playlists);
        index.receive(&id, generation, Ok(Page::whole(vec!["old".into()])));
        assert_eq!(index.contains(&id, "new"), Some(true));
        assert_eq!(index.contains(&id, "old"), Some(false));
        index.clear();
        let (_, new_generation) = request(&mut index, &playlists);
        assert_ne!(new_generation, generation);
        index.receive(&id, generation, Ok(Page::whole(vec!["private".into()])));
        assert_eq!(index.contains(&id, "private"), None);
    }

    #[test]
    fn reads_are_bounded_and_failures_need_an_explicit_retry() {
        let mut index = Index::default();
        let playlists = [playlist("a"), playlist("b"), playlist("c")];
        let requests = index.requests(&playlists);
        assert_eq!(requests.len(), 2);
        assert!(index.requests(&playlists).is_empty());
        let ApiRequest::PlaylistMembership { id, generation, .. } = &requests[0] else {
            unreachable!()
        };
        index.receive(id, *generation, Err(crate::api::ApiError::NotSignedIn));
        assert!(index.error(id).is_some());
        let requests = index.requests(&playlists);
        assert!(
            matches!(requests.as_slice(), [ApiRequest::PlaylistMembership { id, .. }] if id == "c")
        );
        index.retry();
        assert_eq!(index.error(id), None);
    }
}
