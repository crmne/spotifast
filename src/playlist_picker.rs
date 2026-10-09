//! Membership reads for the playing song's playlist menu. One page is read
//! at a time, only while the menu is visible. Unknown is never shown as absent.

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct MembershipPage {
    pub contains: bool,
    pub next_offset: Option<u32>,
    pub snapshot: Option<String>,
}

#[derive(Default)]
pub struct Entry {
    pub contains: Option<bool>,
    pub error: Option<String>,
    offset: u32,
    snapshot: Option<String>,
}

#[derive(Default)]
pub struct Picker {
    pub uri: String,
    pub entries: HashMap<String, Entry>,
    generation: u64,
    order: Vec<String>,
    pending: Option<String>,
}

impl Picker {
    pub fn begin(&mut self, uri: String, ids: Vec<String>) {
        self.generation = self.generation.wrapping_add(1);
        self.uri = uri;
        self.entries = ids
            .iter()
            .map(|id| (id.clone(), Entry::default()))
            .collect();
        self.order = ids;
        self.pending = None;
    }

    pub fn extend(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            if !self.entries.contains_key(&id) {
                self.entries.insert(id.clone(), Entry::default());
                self.order.push(id);
            }
        }
    }

    pub fn next_request(&mut self) -> Option<crate::backend::ApiRequest> {
        if self.pending.is_some() {
            return None;
        }
        let id = self
            .order
            .iter()
            .find(|id| {
                self.entries
                    .get(*id)
                    .is_some_and(|entry| entry.contains.is_none() && entry.error.is_none())
            })?
            .clone();
        let offset = self.entries[&id].offset;
        self.pending = Some(id.clone());
        Some(crate::backend::ApiRequest::PlaylistMembership {
            id,
            uri: self.uri.clone(),
            offset,
            generation: self.generation,
        })
    }

    pub fn receive(&mut self, id: &str, generation: u64, result: Result<MembershipPage, String>) {
        if generation != self.generation || self.pending.as_deref() != Some(id) {
            return;
        }
        self.pending = None;
        let Some(entry) = self.entries.get_mut(id) else {
            return;
        };
        match result {
            Ok(page) => {
                if entry.offset > 0 && entry.snapshot != page.snapshot {
                    entry.error = Some("Playlist changed during the check".into());
                    return;
                }
                entry.snapshot = page.snapshot;
                if page.contains {
                    entry.contains = Some(true);
                } else if let Some(next) = page.next_offset.filter(|next| *next > entry.offset) {
                    entry.offset = next;
                } else {
                    entry.contains = Some(false);
                }
            }
            Err(error) => entry.error = Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ApiRequest;

    fn page(contains: bool, next: Option<u32>) -> MembershipPage {
        MembershipPage {
            contains,
            next_offset: next,
            snapshot: Some("one".into()),
        }
    }

    #[test]
    fn reads_one_page_at_a_time_and_keeps_absence_unknown_until_the_end() {
        let mut picker = Picker::default();
        picker.begin("track".into(), vec!["list".into()]);
        assert!(matches!(
            picker.next_request(),
            Some(ApiRequest::PlaylistMembership { offset: 0, .. })
        ));
        assert!(picker.next_request().is_none());
        picker.receive("list", picker.generation, Ok(page(false, Some(500))));
        assert_eq!(picker.entries["list"].contains, None);
        assert!(matches!(
            picker.next_request(),
            Some(ApiRequest::PlaylistMembership { offset: 500, .. })
        ));
        picker.receive("list", picker.generation, Ok(page(false, None)));
        assert_eq!(picker.entries["list"].contains, Some(false));
        assert!(picker.next_request().is_none());
    }

    #[test]
    fn a_match_stops_reading_and_old_results_cannot_update_a_new_song() {
        let mut picker = Picker::default();
        picker.begin("old".into(), vec!["list".into()]);
        let generation = picker.generation;
        picker.next_request();
        picker.begin("new".into(), vec!["list".into()]);
        picker.next_request();
        picker.receive("list", generation, Ok(page(true, None)));
        assert_eq!(picker.entries["list"].contains, None);
        picker.receive("list", picker.generation, Ok(page(true, Some(500))));
        assert_eq!(picker.entries["list"].contains, Some(true));
        assert!(picker.next_request().is_none());
    }

    #[test]
    fn failed_or_changed_playlists_are_not_reported_as_absent() {
        let mut picker = Picker::default();
        picker.begin("track".into(), vec!["list".into(), "other".into()]);
        picker.next_request();
        picker.receive("list", picker.generation, Ok(page(false, Some(500))));
        picker.next_request();
        let mut changed = page(false, None);
        changed.snapshot = Some("two".into());
        picker.receive("list", picker.generation, Ok(changed));
        assert_eq!(picker.entries["list"].contains, None);
        assert!(picker.entries["list"].error.is_some());
        picker.next_request();
        picker.receive("other", picker.generation, Err("offline".into()));
        assert_eq!(picker.entries["other"].contains, None);
        assert!(picker.next_request().is_none());
    }
}
