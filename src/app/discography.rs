//! Discography pages: every release of an artist with its songs, opened by
//! Show all on the artist page.
//!
//! A page keeps lists of its own rather than the artist page's, which loads
//! releases a page at a time and must not grow behind the user's back. The
//! release list loads whole, because only the whole list can be ordered by
//! date. Song lists then load while the page is open, the releases on
//! screen first, then those after them as far as a play from the screen
//! could reach: in batches over the playback session, or a release or two
//! at a time from the Web API without it. A song played here plays the
//! page from that song to its end; when some of the songs after it have not
//! arrived, the play waits for them, showing the song as starting
//! meanwhile.

use super::*;

/// Song lists read from the Web API at once while the user reads the page:
/// enough to keep ahead of scrolling, few enough to leave playback control
/// room on a rate-limited app.
const TRACKS_IN_FLIGHT: usize = 2;
/// Song lists read from the Web API at once while a play waits for them.
const TRACKS_IN_FLIGHT_FOR_PLAY: usize = 4;
/// Session batches read at once while the user reads the page.
const BATCHES_IN_FLIGHT: usize = 1;
/// Session batches read at once while a play waits: its own beside the one
/// already on its way.
const BATCHES_IN_FLIGHT_FOR_PLAY: usize = 2;
/// Releases in one session batch: as many as the New releases page asks
/// the session for together.
const RELEASES_PER_BATCH: usize = 50;
/// Songs in one session batch, by the releases' own counts: half of a play,
/// so the two batches a waiting play may have in flight hold all it can
/// play. A longer release goes alone.
const SONGS_PER_BATCH: usize = MAX_PLAYED_URIS / BATCHES_IN_FLIGHT_FOR_PLAY;
/// How long the pages read songs from the Web API after the session could
/// not read a batch, unless local playback is ready again sooner.
const SESSION_SONGS_AGAIN_AFTER: Duration = Duration::from_secs(120);
/// A waiting play that has seen no song list arrive for this long starts
/// with the songs it has.
const PLAY_STALL: Duration = Duration::from_secs(10);
// Every batch answer restarts a waiting play's stall clock, so a batch the
// session gives up on hands its releases to the Web API with a fresh wait.
const _: () = assert!(crate::backend::DISCOGRAPHY_SESSION_TIMEOUT.as_secs() < PLAY_STALL.as_secs());
/// The longest a play waits, however its song lists are coming along.
const PLAY_WAIT: Duration = Duration::from_secs(30);
/// A song list unanswered this long is taken as lost and asked for again.
/// The backend lets go of some Web API answers, such as one sent under a
/// sign-in that was replaced on its way, and a lost one must not hold its
/// request slot and its spinner for good. Well past a play's wait, the three
/// rate-limit waits of up to 30 seconds a Web API request can sit through,
/// and a session batch's eight seconds, so a slow answer is seldom asked for
/// twice. A late Web API answer is still used once; a late batch is not,
/// since its releases were asked for again.
const TRACKS_LOST_AFTER: Duration = Duration::from_secs(120);
/// How close to its end the song still playing during a wait may come
/// before the play starts anyway: a pass of the app's loop, and a remote
/// device running ahead of its last report while the start reaches it.
const PLAY_BEFORE_END: Duration = Duration::from_secs(2);

impl App {
    /// The heading of an artist's discography page.
    pub fn discography_title(&self, artist_id: &str) -> Option<String> {
        self.artist_name(artist_id).or_else(|| {
            let page = self.discography_pages.get(artist_id)?;
            page.artist
                .as_ref()
                .map(|artist| artist.name.clone())
                .or_else(|| credited_name(page, artist_id))
        })
    }

    fn artist_name(&self, artist_id: &str) -> Option<String> {
        self.known_discography_artist(artist_id)
            .map(|artist| artist.name.clone())
    }

    /// The artist as the app knows them outside the discography page.
    fn known_discography_artist(&self, artist_id: &str) -> Option<&Artist> {
        self.artist_pages
            .get(artist_id)
            .and_then(|page| page.artist.get())
            .or_else(|| self.known_artist(artist_id))
    }

    /// A release the discography pages know, for an album page's preview.
    pub(super) fn known_discography_release(&self, album_id: &str) -> Option<&Album> {
        self.discography_pages
            .values()
            .find_map(|page| page.release(album_id))
    }

    /// Makes the page when it is missing and asks for what it lacks.
    /// Offline it asks for nothing; [`Self::resume_discography`] does once
    /// the network is back.
    pub(super) fn ensure_discography(&mut self, artist_id: &str) {
        if !self.discography_pages.contains_key(artist_id) {
            let filter = self
                .artist_pages
                .get(artist_id)
                .map_or(DiscographyFilter::All, |page| page.filter);
            self.insert_discography_page(artist_id, filter, true);
        }
        self.request_discography_artist(artist_id);
        self.load_discography_releases(artist_id);
        self.pump_discography(artist_id);
        // What was held back offline for the releases on screen, should the
        // page come back without its view moving.
        self.describe_releases_in_view(artist_id);
    }

    fn insert_discography_page(
        &mut self,
        artist_id: &str,
        filter: DiscographyFilter,
        newest_first: bool,
    ) {
        self.load_generation = self.load_generation.wrapping_add(1);
        let artist = self.known_discography_artist(artist_id).cloned();
        let mut page = DiscographyPage::new(self.load_generation, artist, filter);
        page.newest_first = newest_first;
        self.discography_pages.insert(artist_id.to_string(), page);
    }

    /// Asks for the artist once when nothing names it, as for a page
    /// restored from the last run.
    fn request_discography_artist(&mut self, artist_id: &str) {
        if !self.network_reachable || self.artist_name(artist_id).is_some() {
            return;
        }
        let Some(page) = self.discography_pages.get_mut(artist_id) else {
            return;
        };
        if page.artist.is_some() || page.artist_requested {
            return;
        }
        page.artist_requested = true;
        self.backend.api(ApiRequest::Artist {
            id: artist_id.to_string(),
        });
    }

    /// Show all: the page on the filter the artist page shows.
    pub(super) fn open_discography(&mut self, artist_id: String, filter: DiscographyFilter) {
        match self.discography_pages.get_mut(&artist_id) {
            Some(page) => {
                if page.filter != filter {
                    page.filter = filter;
                    page.in_view = None;
                }
            }
            None => self.insert_discography_page(&artist_id, filter, true),
        }
        self.open(Page::Discography(artist_id));
    }

    /// Reload keeps the filter and the order the user chose.
    pub(super) fn reload_discography(&mut self, artist_id: &str) {
        if self
            .pending_discography_play
            .as_ref()
            .is_some_and(|play| play.artist_id == artist_id)
        {
            self.abandon_discography_play();
        }
        if let Some(page) = self.discography_pages.remove(artist_id) {
            self.insert_discography_page(artist_id, page.filter, page.newest_first);
        }
    }

    pub(super) fn set_discography_page_filter(
        &mut self,
        artist_id: &str,
        filter: DiscographyFilter,
    ) {
        let Some(page) = self.discography_pages.get_mut(artist_id) else {
            return;
        };
        page.filter = filter;
        page.in_view = None;
        self.load_discography_releases(artist_id);
        self.pump_discography(artist_id);
    }

    pub(super) fn set_discography_order(&mut self, artist_id: &str, newest_first: bool) {
        if let Some(page) = self.discography_pages.get_mut(artist_id) {
            page.newest_first = newest_first;
            page.in_view = None;
        }
        self.pump_discography(artist_id);
    }

    /// The view reports the releases on screen when they change.
    pub(super) fn discography_in_view(&mut self, artist_id: &str, first: usize, last: usize) {
        let Some(page) = self.discography_pages.get_mut(artist_id) else {
            return;
        };
        page.in_view = Some((first, last.max(first)));
        self.describe_releases_in_view(artist_id);
        self.pump_discography(artist_id);
    }

    /// EP labels, save states and liked hearts, for the releases on screen
    /// only: asking for the whole discography at once would overflow the EP
    /// lookups and spend requests on rows nobody sees.
    fn describe_releases_in_view(&mut self, artist_id: &str) {
        if !self.network_reachable {
            return;
        }
        let Some(page) = self.discography_pages.get(artist_id) else {
            return;
        };
        let Some((first, last)) = page.in_view else {
            return;
        };
        let shown: Vec<Album> = page
            .ordered()
            .into_iter()
            .skip(first)
            .take(last + 1 - first)
            .cloned()
            .collect();
        let mut uris: Vec<String> = shown.iter().map(|album| album.uri.clone()).collect();
        for album in &shown {
            if let Some(songs) = page.tracks.get(&album.id).and_then(ReleaseTracks::songs) {
                uris.extend(songs.iter().map(|song| song.uri.clone()));
            }
        }
        self.request_album_types(shown.iter());
        self.request_contains(uris);
    }

    /// Asks for the current filter's next page of releases, or takes them
    /// from the artist page when it already has them all.
    fn load_discography_releases(&mut self, artist_id: &str) {
        let reachable = self.network_reachable;
        let Some(groups) = self
            .discography_pages
            .get(artist_id)
            .map(|page| page.filter.groups())
        else {
            return;
        };
        let seed = self
            .artist_pages
            .get(artist_id)
            .and_then(|page| page.albums.get(groups))
            .filter(|list| list.is_complete())
            .map(|list| list.items.clone());
        let Some(page) = self.discography_pages.get_mut(artist_id) else {
            return;
        };
        let generation = page.generation;
        let list = page.releases.entry(groups.to_string()).or_default();
        if list.loading || list.error.is_some() || list.is_complete() {
            return;
        }
        if !list.loaded_once
            && let Some(items) = seed
        {
            let total = items.len() as u32;
            list.absorb(
                0,
                crate::api::models::Page {
                    items,
                    offset: 0,
                    limit: total,
                    total,
                    next: None,
                },
            );
            dedupe_releases(list);
            self.describe_releases_in_view(artist_id);
            return;
        }
        let Some(offset) = list.next_offset else {
            return;
        };
        if !reachable {
            return;
        }
        list.loading = true;
        self.backend.api(ApiRequest::DiscographyAlbums {
            artist_id: artist_id.to_string(),
            groups: groups.to_string(),
            offset,
            generation,
        });
    }

    pub(super) fn receive_discography_albums(
        &mut self,
        artist_id: String,
        groups: String,
        offset: u32,
        generation: u64,
        result: crate::backend::ApiResult<crate::api::models::Page<Album>>,
    ) {
        let open = self.discography_open(&artist_id);
        let locale = self.locale;
        let Some(page) = self
            .discography_pages
            .get_mut(&artist_id)
            .filter(|page| page.generation == generation)
        else {
            return;
        };
        let current = page.filter.groups() == groups;
        let list = page.releases.entry(groups.clone()).or_default();
        if !list.loading {
            return;
        }
        let albums = match result {
            Ok(albums) => albums,
            Err(error) => {
                list.fail(friendly_api_error(locale, &error));
                return;
            }
        };
        let next = albums.next_offset();
        // An answer for another page, or one that does not move forward,
        // would loop or cut the list short: stop and offer Retry instead.
        if albums.offset != offset
            || (albums.next.is_some() && next.is_none_or(|next| next <= offset))
        {
            list.fail(crate::api::ApiError::Decode("releases out of order".into()).to_string());
            return;
        }
        list.absorb(offset, albums);
        if let Some(next) = next {
            // Only the list on screen keeps loading; another filter's
            // resumes from here when it is chosen again.
            if current && open {
                list.loading = true;
                self.backend.api(ApiRequest::DiscographyAlbums {
                    artist_id,
                    groups,
                    offset: next,
                    generation,
                });
            }
            return;
        }
        dedupe_releases(list);
        self.describe_releases_in_view(&artist_id);
        self.pump_discography(&artist_id);
        self.resolve_discography_play();
    }

    fn discography_open(&self, artist_id: &str) -> bool {
        matches!(self.page(), Page::Discography(id) if id == artist_id)
    }

    /// Whether the pages ask the playback session for their songs: local
    /// playback is ready, and the session has not just failed a batch.
    fn songs_over_session(&self) -> bool {
        self.local_ready
            && self
                .discography_session_failed_at
                .is_none_or(|at| at.elapsed() >= SESSION_SONGS_AGAIN_AFTER)
    }

    /// Asks for the next song lists: first those a waiting play needs, then,
    /// while the page is open, the releases on screen and those after them
    /// as far as a play from the screen could reach. Over the playback
    /// session they go in batches, otherwise to the Web API one release at a
    /// time.
    pub(super) fn pump_discography(&mut self, artist_id: &str) {
        let open = self.discography_open(artist_id);
        let reachable = self.network_reachable;
        let over_session = self.songs_over_session();
        let Some(page) = self.discography_pages.get(artist_id) else {
            return;
        };
        if page.halted.is_some() {
            return;
        }
        let mut wanted: Vec<String> = Vec::new();
        let (mut limit, mut batch_limit) = (TRACKS_IN_FLIGHT, BATCHES_IN_FLIGHT);
        if let Some(play) = self
            .pending_discography_play
            .as_ref()
            .filter(|play| play.artist_id == artist_id && play.generation == page.generation)
        {
            wanted.extend(waiting_for(page, play));
            (limit, batch_limit) = (TRACKS_IN_FLIGHT_FOR_PLAY, BATCHES_IN_FLIGHT_FOR_PLAY);
        }
        if open && page.list().is_some_and(PagedList::is_complete) {
            let releases = page.ordered();
            if !releases.is_empty() {
                let end = releases.len() - 1;
                let (first, last) = page.in_view.unwrap_or((0, 0));
                let (first, last) = (first.min(end), last.min(end).max(first.min(end)));
                wanted.extend(releases[first..=last].iter().map(|album| album.id.clone()));
                let mut reach = 0;
                for album in &releases[last + 1..] {
                    if reach >= MAX_PLAYED_URIS {
                        break;
                    }
                    reach += song_count(page, album);
                    wanted.push(album.id.clone());
                }
            }
        }
        if wanted.is_empty() {
            return;
        }
        // The two lanes count apart, so a batch on its way holds no Web API
        // slot.
        let fetching = page
            .tracks
            .values()
            .filter(|tracks| matches!(tracks, ReleaseTracks::Loading { .. }))
            .count();
        let batches: HashSet<u64> = page
            .tracks
            .values()
            .filter_map(ReleaseTracks::batch)
            .collect();
        let generation = page.generation;
        let mut free = limit.saturating_sub(fetching);
        let mut batched: Vec<Album> = Vec::new();
        let mut ready = Vec::new();
        let mut ask = Vec::new();
        let mut seen = HashSet::new();
        for id in wanted {
            if id.is_empty() || !seen.insert(id.clone()) || page.tracks.contains_key(&id) {
                continue;
            }
            // An album page that has every song needs no request.
            if let Some(songs) = self
                .album_pages
                .get(&id)
                .filter(|album| album.tracks.is_complete())
                .map(|album| album.tracks.items.clone())
            {
                ready.push((id, songs));
                continue;
            }
            if !reachable {
                continue;
            }
            // A malformed entity would fail its whole batch.
            let session_album = page.release(&id).filter(|album| {
                over_session
                    && !page.web_api_only.contains(&id)
                    && album.uri == format!("spotify:album:{id}")
            });
            match session_album {
                Some(album) => batched.push(release_stub(album)),
                None if free > 0 => {
                    free -= 1;
                    ask.push(id);
                }
                None => {}
            }
        }
        let sends: Vec<Vec<Album>> = split_batches(batched)
            .into_iter()
            .take(batch_limit.saturating_sub(batches.len()))
            .collect();
        let Some(page) = self.discography_pages.get_mut(artist_id) else {
            return;
        };
        for (id, mut songs) in ready {
            if let Some(album) = page.release(&id).cloned() {
                fill_album(&mut songs, &album);
            }
            page.tracks.insert(id, ReleaseTracks::Loaded(songs));
        }
        let now = Instant::now();
        for id in &ask {
            page.tracks.insert(
                id.clone(),
                ReleaseTracks::Loading {
                    songs: Vec::new(),
                    next: 0,
                    asked: now,
                },
            );
        }
        let mut batches = Vec::new();
        for albums in sends {
            let batch = page.new_batch();
            for album in &albums {
                page.tracks.insert(
                    album.id.clone(),
                    ReleaseTracks::Batched { batch, asked: now },
                );
            }
            batches.push((batch, albums));
        }
        for (batch, albums) in batches {
            self.backend.api(ApiRequest::DiscographySongs {
                artist_id: artist_id.to_string(),
                albums,
                batch,
                generation,
            });
        }
        for album_id in ask {
            self.backend.api(ApiRequest::DiscographyTracks {
                artist_id: artist_id.to_string(),
                album_id,
                offset: 0,
                generation,
            });
        }
    }

    pub(super) fn receive_discography_tracks(
        &mut self,
        artist_id: String,
        album_id: String,
        offset: u32,
        generation: u64,
        result: crate::backend::ApiResult<crate::api::models::Page<Track>>,
    ) {
        let locale = self.locale;
        let Some(page) = self
            .discography_pages
            .get_mut(&artist_id)
            .filter(|page| page.generation == generation)
        else {
            return;
        };
        let album = page.release(&album_id).cloned();
        let Some(ReleaseTracks::Loading { songs, next, asked }) = page.tracks.get_mut(&album_id)
        else {
            return;
        };
        if *next != offset {
            return;
        }
        enum Outcome {
            More(u32),
            Done(ReleaseTracks),
            /// Every other list would fail the same way: stop asking, and
            /// read this release again first on Retry.
            Halt(String),
        }
        let out_of_order = || {
            Outcome::Done(ReleaseTracks::Failed(
                crate::api::ApiError::Decode("songs out of order".into()).to_string(),
            ))
        };
        let sign_in_expired = matches!(result, Err(crate::api::ApiError::SignInExpired { .. }));
        let outcome = match result {
            Ok(tracks) if tracks.offset == offset => {
                let following = tracks.next_offset();
                if tracks.next.is_some() && following.is_none_or(|following| following <= offset) {
                    out_of_order()
                } else {
                    let mut arrived = tracks.items;
                    if let Some(album) = &album {
                        fill_album(&mut arrived, album);
                    }
                    songs.extend(arrived);
                    match following {
                        Some(following) => Outcome::More(following),
                        None => Outcome::Done(ReleaseTracks::Loaded(std::mem::take(songs))),
                    }
                }
            }
            Ok(_) => out_of_order(),
            // As for Home's episodes: only an answer about this release
            // itself is this release's failure.
            Err(
                error @ (crate::api::ApiError::Decode(_) | crate::api::ApiError::Status { .. }),
            ) if error.status().is_none_or(|status| status < 500) => {
                Outcome::Done(ReleaseTracks::Failed(friendly_api_error(locale, &error)))
            }
            Err(error) => Outcome::Halt(friendly_api_error(locale, &error)),
        };
        if let Some(play) = self
            .pending_discography_play
            .as_mut()
            .filter(|play| play.artist_id == artist_id)
        {
            play.progress = Instant::now();
        }
        match outcome {
            Outcome::More(following) => {
                // The wait for a lost answer starts again for each page.
                *next = following;
                *asked = Instant::now();
                self.backend.api(ApiRequest::DiscographyTracks {
                    artist_id,
                    album_id,
                    offset: following,
                    generation,
                });
                return;
            }
            Outcome::Done(tracks) => {
                page.tracks.insert(album_id, tracks);
            }
            Outcome::Halt(error) => {
                page.tracks.remove(&album_id);
                // The backend lets go of the Web API answers the ended
                // sign-in refuses on their way, so the song lists still
                // waiting on the Web API give up their slots: Retry, on the
                // page or on a release, asks for them again. A session
                // batch's answer still comes.
                if sign_in_expired {
                    page.tracks
                        .retain(|_, tracks| !matches!(tracks, ReleaseTracks::Loading { .. }));
                }
                page.halted = Some(error);
            }
        }
        self.describe_releases_in_view(&artist_id);
        self.pump_discography(&artist_id);
        self.resolve_discography_play();
    }

    /// A session batch's answer. Only the batch that asked counts: a release
    /// asked for again in a newer batch waits for that one.
    pub(super) fn receive_discography_songs(
        &mut self,
        artist_id: String,
        batch: u64,
        generation: u64,
        songs: Option<Vec<(String, Vec<Track>)>>,
    ) {
        let Some(page) = self
            .discography_pages
            .get_mut(&artist_id)
            .filter(|page| page.generation == generation)
        else {
            return;
        };
        if !page
            .tracks
            .values()
            .any(|tracks| tracks.batch() == Some(batch))
        {
            return;
        }
        let failed = songs.is_none();
        for (album_id, mut songs) in songs.unwrap_or_default() {
            if page.tracks.get(&album_id).and_then(ReleaseTracks::batch) != Some(batch) {
                continue;
            }
            // A song the session leaves out, as one Spotify no longer has or
            // withholds, would shrink the room the release holds and move
            // the rows below it, which the user may be reading by the time
            // the answer comes. The Web API keeps each song's place.
            let short = page
                .release(&album_id)
                .and_then(|album| album.total_tracks)
                .is_some_and(|total| songs.len() < total as usize);
            if songs.is_empty() || short {
                // Gone, withheld, unanswered or empty: the Web API says which.
                page.tracks.remove(&album_id);
                page.web_api_only.insert(album_id);
            } else {
                if let Some(album) = page.release(&album_id).cloned() {
                    fill_album(&mut songs, &album);
                }
                page.tracks.insert(album_id, ReleaseTracks::Loaded(songs));
            }
        }
        // A release the answer did not list, or every release of a batch the
        // session could not read, is asked for again: over the Web API while
        // the session rests.
        page.tracks
            .retain(|_, tracks| tracks.batch() != Some(batch));
        if failed {
            self.discography_session_failed_at = Some(Instant::now());
        }
        if let Some(play) = self
            .pending_discography_play
            .as_mut()
            .filter(|play| play.artist_id == artist_id)
        {
            play.progress = Instant::now();
        }
        self.describe_releases_in_view(&artist_id);
        self.pump_discography(&artist_id);
        self.resolve_discography_play();
    }

    /// Retry on a release that failed, or on the page: the release list if
    /// it failed, and every song list that stopped.
    pub(super) fn retry_discography(&mut self, artist_id: &str, album_id: Option<String>) {
        let Some(page) = self.discography_pages.get_mut(artist_id) else {
            return;
        };
        page.halted = None;
        match album_id {
            Some(id) => {
                if matches!(page.tracks.get(&id), Some(ReleaseTracks::Failed(_))) {
                    page.tracks.remove(&id);
                }
            }
            None => {
                // A failed list keeps its place: resume from the page that
                // failed.
                if let Some(list) = page.releases.get_mut(page.filter.groups()) {
                    list.error = None;
                }
                self.load_discography_releases(artist_id);
            }
        }
        self.pump_discography(artist_id);
    }

    /// Forgets the song lists and session batches whose answer has not come
    /// for [`TRACKS_LOST_AFTER`] and asks for them again wherever a page
    /// still wants them. A forgotten list waits on as one not yet asked for,
    /// so nothing on the page changes meanwhile.
    pub(super) fn forget_lost_song_lists(&mut self) {
        let now = Instant::now();
        let mut forgot = Vec::new();
        for (artist_id, page) in &mut self.discography_pages {
            let before = page.tracks.len();
            page.tracks.retain(|_, tracks| {
                !matches!(tracks, ReleaseTracks::Loading { asked, .. }
                    | ReleaseTracks::Batched { asked, .. }
                    if now.saturating_duration_since(*asked) >= TRACKS_LOST_AFTER)
            });
            if page.tracks.len() < before {
                forgot.push(artist_id.clone());
            }
        }
        for artist_id in forgot {
            self.pump_discography(&artist_id);
        }
    }

    /// The network is back: as Retry, the release list resumes where it
    /// failed and the song lists that stopped are asked for again, along
    /// with whatever the page held back offline.
    pub(super) fn resume_discography(&mut self, artist_id: &str) {
        if let Some(page) = self.discography_pages.get_mut(artist_id) {
            page.halted = None;
            if let Some(list) = page.releases.get_mut(page.filter.groups()) {
                list.error = None;
            }
        }
        self.ensure_loaded(Page::Discography(artist_id.to_string()));
    }

    /// Plays the page from a release to its end: from the song at `start`,
    /// or from the release's first playable song.
    pub(super) fn play_discography(
        &mut self,
        artist_id: String,
        album_id: String,
        start: Option<(u32, String)>,
    ) {
        let Some(page) = self.discography_pages.get(&artist_id) else {
            return;
        };
        let ordered = page.ordered();
        let Some(first) = ordered.iter().position(|album| album.id == album_id) else {
            return;
        };
        let releases: Vec<String> = ordered[first..]
            .iter()
            .map(|album| album.id.clone())
            .collect();
        let generation = page.generation;
        // The click is the choice, not the start after the wait: a
        // play-search in flight is superseded now, one sent later is newer.
        self.supersede_play_search();
        // Nowhere to play: say so at once rather than after the wait. The
        // click still replaces an older play waiting for its songs. A device
        // playing without an id, a Sonos, is somewhere, as for any play.
        if matches!(self.target(), Target::Remote(None))
            && !self.local_player_coming()
            && self.remote_elsewhere().is_none()
        {
            self.abandon_discography_play();
            self.ask_for_device();
            return;
        }
        let now = Instant::now();
        let play = PendingDiscographyPlay {
            artist_id,
            releases,
            start,
            generation,
            shown: None,
            keys: Vec::new(),
            clicked: now,
            progress: now,
            position_ms: 0,
            device: None,
        };
        // The newest click wins over a play held for a connecting engine.
        self.queued_play = None;
        self.start_discography_play(play);
    }

    /// Starts `play` once every song list it needs has arrived or failed.
    /// A wait that stalls, once the clicked release is in, that runs too
    /// long, or that would outlast the song still playing, starts with the
    /// songs that have arrived.
    fn start_discography_play(&mut self, mut play: PendingDiscographyPlay) {
        let Some(page) = self
            .discography_pages
            .get(&play.artist_id)
            .filter(|page| page.generation == play.generation)
        else {
            self.abandon_discography_play();
            return;
        };
        let waiting = !waiting_for(page, &play).is_empty();
        // The song from before the click plays on meanwhile; once it ends,
        // its play moves on to a song the start would cut off.
        let give_up = wait_over(page, &play) || self.playing_song_ending();
        if waiting && !give_up {
            // The click shows at once and stays shown: the play holds the
            // song it will start and the spinners itself.
            let album_uri = page
                .release(&play.releases[0])
                .map(|album| album.uri.clone());
            let shown = starting_song(page, &play);
            play.shown = shown.as_ref().map(|song| song.uri.clone());
            play.keys = play.shown.iter().cloned().chain(album_uri).collect();
            let artist_id = play.artist_id.clone();
            // Only a change of song asks for lyrics: a later pass of the
            // same wait leaves a failed answer to Try again.
            let moved = self
                .pending_discography_play
                .as_ref()
                .and_then(|old| old.shown.as_deref())
                != play.shown.as_deref();
            if let Some(song) = shown {
                self.cache_song(song);
            }
            self.pending_discography_play = Some(play);
            if moved {
                // The desktop's controls may have shown this song already,
                // elsewhere in it, as when it is the song playing.
                self.media_seek = true;
                self.follow_shown_lyrics();
            }
            self.pump_discography(&artist_id);
            return;
        }
        let list = play_list(page, &play);
        let left_out = list.is_none() && leaves_out_later_songs(page, &play);
        let album_uri = page
            .release(&play.releases[0])
            .map(|album| album.uri.clone());
        // A newer click that does not wait replaces an older play still
        // waiting, and the lyrics of the song that one showed go with it.
        // The desktop's controls counted the wait as playing time: what
        // follows a shown song, even that song starting where it was shown,
        // reaches them as a jump.
        if let Some(old) = self
            .pending_discography_play
            .take()
            .filter(|old| old.shown.is_some())
        {
            self.media_seek = true;
            if old.shown != play.shown {
                self.follow_shown_lyrics();
            }
        }
        let Some((songs, index)) = list else {
            // The release's songs never came: play it as Spotify knows it,
            // and say so when that leaves out the releases after it.
            if let Some(uri) = album_uri.filter(|_| play.start.is_none()) {
                if left_out {
                    self.say_queue_ends_early();
                }
                self.played_row = None;
                self.start_on = play.device.clone();
                self.start_play(PlayRequest::context(uri), false);
            } else if play.shown.is_some() {
                // Nothing starts: the bar goes back to the song playing.
                self.follow_shown_lyrics();
            }
            return;
        };
        if waiting {
            self.say_queue_ends_early();
        }
        if let Some(song) = songs.get(index) {
            self.cache_song(song.clone());
        }
        let mut uris: Vec<String> = songs.into_iter().map(|song| song.uri).collect();
        // The list starts with the clicked release's first song; a long one
        // loses songs from its end, so Previous, Shuffle and Repeat keep the
        // start of that release. Only a start past the limit itself takes
        // the usual window from the clicked song.
        let (uris, index) = if window_start(index) == 0 {
            uris.truncate(MAX_PLAYED_URIS);
            (uris, index as u32)
        } else {
            cap_uris(&uris, index as u32)
        };
        let mut request = PlayRequest::tracks(uris).starting_at_index(index);
        // The song shown while the play waited starts where a seek moved it.
        if play.shown.is_some() {
            request.position_ms = play.position_ms;
        }
        // A speaker woken while the song showed as playing takes it over
        // playing, even if waking the speaker stops the music here first.
        if play.shown.is_some()
            && (self.activating_receiver.is_some() || self.pending_transfer_to.is_some())
        {
            self.activating_play = true;
        }
        // The playlist row an earlier play started from stops being the
        // marked copy now that this play starts, not at the click: a wait
        // that ends in nothing leaves it marked.
        self.played_row = None;
        // A play kept for a woken speaker starts there.
        self.start_on = play.device.clone();
        // The precedence was taken at the click.
        self.start_play(request, false);
    }

    /// A play that stopped seeing its songs arrive, waited too long, or
    /// would outlast the song still playing, starts with the songs it has.
    /// This runs every pass, so it reads only the clock, the page and the
    /// player, and looks at the wait again only once the wait can end.
    pub(super) fn settle_overdue_discography_play(&mut self) {
        let Some(play) = &self.pending_discography_play else {
            return;
        };
        // The start also clears a play whose page went away or was
        // reloaded.
        let over = self
            .discography_pages
            .get(&play.artist_id)
            .filter(|page| page.generation == play.generation)
            .is_none_or(|page| wait_over(page, play));
        if over || self.playing_song_ending() {
            self.resolve_discography_play();
        }
    }

    /// Whether the song still playing while a play waits is about to end.
    /// The wait must not outlast it: the old play would move on to its next
    /// song, perhaps one the user queued, and the start would cut that off.
    /// Not now_playing(): its preview is the waiting play's song. A song on
    /// Repeat one starts over instead, moving nothing on. This goes by the
    /// clock: the old play can still move on first when skipped from
    /// another device, or on a device further ahead of its last report than
    /// the margin allows.
    fn playing_song_ending(&self) -> bool {
        self.now_playing_live().is_some_and(|now| {
            now.playing
                && now.repeat != RepeatMode::Track
                && now.duration_ms > 0
                && Duration::from_millis(now.duration_ms.saturating_sub(now.position_ms).into())
                    <= PLAY_BEFORE_END
        })
    }

    /// Looks again at a waiting play: start it if its songs are all in or
    /// its wait is over.
    pub(crate) fn resolve_discography_play(&mut self) {
        if let Some(play) = self.pending_discography_play.clone() {
            self.start_discography_play(play);
        }
    }

    /// Forgets a waiting play and what it showed: the bar, and the lyrics
    /// on show, go back to the song still playing.
    pub(super) fn abandon_discography_play(&mut self) {
        if self
            .pending_discography_play
            .take()
            .is_some_and(|play| play.shown.is_some())
        {
            // The desktop's controls go back with the bar, perhaps to the
            // same song elsewhere in it.
            self.media_seek = true;
            self.follow_shown_lyrics();
        }
    }

    /// Lyrics on show follow the player bar's song when a waiting play
    /// changes it, as they follow a song that starts.
    fn follow_shown_lyrics(&mut self) {
        if self.show_lyrics_panel || self.lyrics_fullscreen.is_some() {
            self.request_lyrics();
        }
    }

    /// The player bar's song while a play waits for its songs: the clicked
    /// song, starting where it will start.
    pub(super) fn waiting_track_preview(&self, uri: &str, position_ms: u32) -> Option<NowPlaying> {
        let mut now = self.cached_track_preview(uri, position_ms)?;
        let actual = self.now_playing_live();
        now.local = matches!(self.target(), Target::Local);
        // Shown as playing, with a Pause that cancels the wait, rather than
        // as loading: the row and the release's button spin instead.
        now.playing = true;
        now.loading = false;
        now.resuming = false;
        if let Some(actual) = actual {
            now.take_device_from(actual);
        }
        Some(now)
    }

    /// The song a discography play shows while it waits for its songs. No
    /// queue row was taken for it, unlike a song started from Next up.
    pub fn waiting_discography_song(&self) -> Option<&str> {
        self.pending_discography_play.as_ref()?.shown.as_deref()
    }

    /// The player bar's song, except one a discography play only shows
    /// while it waits: then what a device is actually playing.
    pub(super) fn now_playing_heard(&self) -> Option<NowPlaying> {
        if self
            .pending_discography_play
            .as_ref()
            .is_some_and(|play| play.shown.is_some())
        {
            self.now_playing_live()
        } else {
            self.now_playing()
        }
    }

    /// The current song, but for the one a waiting play shows: nothing was
    /// asked to play that song yet, so the song asked for or playing before
    /// the click still counts.
    pub(super) fn started_track_uri(&self) -> Option<String> {
        if self
            .pending_discography_play
            .as_ref()
            .is_none_or(|play| play.shown.is_none())
        {
            return self.current_track_uri();
        }
        if let Some(intent) = &self.intent_track
            && (intent.at.elapsed() < PLAYBACK_HOLD || self.queued_play.is_some())
        {
            return Some(intent.uri.clone());
        }
        // Not now_playing(): its preview is the waiting play's song.
        self.now_playing_live()
            .or_else(|| self.resume_preview())
            .map(|now| now.uri)
    }

    /// A play from the page that holds fewer songs than the page would give
    /// it.
    fn say_queue_ends_early(&mut self) {
        self.toast(gettext(
            self.locale,
            // Translators: a play from an artist's discography page started
            // before every song after the chosen one had loaded.
            "Some songs didn't load in time, so the queue ends early",
        ));
    }

    fn cache_song(&mut self, song: Track) {
        let Some(id) = song
            .id
            .clone()
            .or_else(|| util::uri_id(&song.uri).map(str::to_string))
        else {
            return;
        };
        self.remember_track_recording(&song);
        self.track_used.insert(id.clone(), Instant::now());
        // A full answer from Spotify knows more than a release's song list.
        self.track_cache.entry(id).or_insert(song);
    }
}

/// The artist's name as one of the page's releases credits it.
fn credited_name(page: &DiscographyPage, artist_id: &str) -> Option<String> {
    page.releases
        .values()
        .flat_map(|list| &list.items)
        .flat_map(|album| &album.artists)
        .find(|artist| artist.id.as_deref() == Some(artist_id))
        .map(|artist| artist.name.clone())
}

/// Drops an exact repeat of a release, which Spotify can return when the
/// catalogue shifts between pages. Same-named versions all stay.
fn dedupe_releases(list: &mut PagedList<Album>) {
    let mut seen = HashSet::new();
    list.retain(|album| seen.insert(album.id.clone()));
}

/// Whether Spotify can be asked to play the song, as for any track row.
pub(crate) fn playable_song(song: &Track) -> bool {
    !song.uri.is_empty()
        && !song.uri.starts_with("spotify:local:")
        && !song.is_local
        && song.is_playable != Some(false)
}

/// The songs a release adds to a play: its playable songs once they have
/// arrived, else at least one.
fn song_count(page: &DiscographyPage, album: &Album) -> usize {
    match page.tracks.get(&album.id) {
        Some(ReleaseTracks::Loaded(songs)) => {
            songs.iter().filter(|song| playable_song(song)).count()
        }
        Some(ReleaseTracks::Failed(_)) => 0,
        Some(ReleaseTracks::Loading { .. } | ReleaseTracks::Batched { .. }) | None => {
            album.total_tracks.unwrap_or(1).max(1) as usize
        }
    }
}

/// Gives album-less songs, as Spotify lists a release's songs, the release
/// they belong to, for their rows, menus and the player bar.
fn fill_album(songs: &mut [Track], album: &Album) {
    let stub = release_stub(album);
    for song in songs.iter_mut().filter(|song| song.album.is_none()) {
        song.album = Some(stub.clone());
    }
}

/// The release as its songs carry it, for their rows, menus and the player
/// bar.
fn release_stub(album: &Album) -> Album {
    Album {
        id: album.id.clone(),
        name: album.name.clone(),
        uri: album.uri.clone(),
        album_type: album.album_type.clone(),
        album_group: album.album_group.clone(),
        total_tracks: album.total_tracks,
        images: album.images.clone(),
        artists: album.artists.clone(),
        release_date: album.release_date.clone(),
        ..Album::default()
    }
}

/// Cuts `releases`, in order, into session batches of at most
/// [`RELEASES_PER_BATCH`] releases and [`SONGS_PER_BATCH`] songs by their
/// track counts. A release longer than that goes alone.
fn split_batches(releases: Vec<Album>) -> Vec<Vec<Album>> {
    let mut batches: Vec<Vec<Album>> = Vec::new();
    let mut songs = 0;
    for release in releases {
        let count = release.total_tracks.unwrap_or(1).max(1) as usize;
        match batches.last_mut() {
            Some(batch) if batch.len() < RELEASES_PER_BATCH && songs + count <= SONGS_PER_BATCH => {
                songs += count;
                batch.push(release);
            }
            _ => {
                songs = count;
                batches.push(vec![release]);
            }
        }
    }
    batches
}

/// The releases a play needs that have not arrived: from the clicked one
/// on, in the order shown at the click, as far as the songs a play can hold
/// reach, counted from where the play's songs begin: the clicked release's
/// first song, or the clicked song itself when it lies past that limit.
fn waiting_for(page: &DiscographyPage, play: &PendingDiscographyPlay) -> Vec<String> {
    let left_out = songs_left_out(page, play);
    let mut waiting = Vec::new();
    let mut reach = 0usize;
    for (position, id) in play.releases.iter().enumerate() {
        if reach >= MAX_PLAYED_URIS {
            break;
        }
        if !page.tracks.get(id).is_some_and(ReleaseTracks::resolved) {
            waiting.push(id.clone());
        }
        let count = match page.release(id) {
            Some(album) => song_count(page, album),
            None => 1,
        };
        reach += if position == 0 {
            count.saturating_sub(left_out)
        } else {
            count
        };
    }
    waiting
}

/// Whether a waiting play starts now with the songs it has, whatever is
/// still on its way: its page stopped asking, the clicked release's songs
/// failed, it has waited as long as a play may, or the clicked release is
/// in and no song list has come for a while. The start and the overdue
/// check of every pass both ask this, so the check never runs a wait that
/// cannot end yet.
fn wait_over(page: &DiscographyPage, play: &PendingDiscographyPlay) -> bool {
    let start_tracks = page.tracks.get(&play.releases[0]);
    // A clicked release whose songs failed plays on its own whatever comes
    // after it, so waiting for the rest gains nothing.
    let start_failed = matches!(start_tracks, Some(ReleaseTracks::Failed(_)));
    let start_resolved = start_tracks.is_some_and(ReleaseTracks::resolved);
    page.halted.is_some()
        || start_failed
        || play.clicked.elapsed() >= PLAY_WAIT
        || (start_resolved && play.progress.elapsed() >= PLAY_STALL)
}

/// Whether a play of the clicked release on its own leaves out songs the
/// play would have held: a later release that adds any, while the clicked
/// one leaves room under the cap. One that failed or has nothing playable
/// adds none.
fn leaves_out_later_songs(page: &DiscographyPage, play: &PendingDiscographyPlay) -> bool {
    let count = |id: &String| page.release(id).map_or(1, |album| song_count(page, album));
    let Some((clicked, later)) = play.releases.split_first() else {
        return false;
    };
    count(clicked) < MAX_PLAYED_URIS && later.iter().any(|id| count(id) > 0)
}

/// The clicked release's playable songs a play leaves out before its
/// start: none, unless the start lies past the songs a play can hold, when
/// the play's songs begin at the start itself. None are known until the
/// release's songs have arrived.
fn songs_left_out(page: &DiscographyPage, play: &PendingDiscographyPlay) -> usize {
    let Some(songs) = play
        .releases
        .first()
        .and_then(|id| page.tracks.get(id))
        .and_then(ReleaseTracks::songs)
    else {
        return 0;
    };
    let before = start_row(songs, &play.start).map_or(0, |row| {
        songs[..row]
            .iter()
            .filter(|song| playable_song(song))
            .count()
    });
    window_start(before)
}

/// Where the songs a play sends begin in its list, given its start's place
/// there: at the clicked release's first song, unless the start lies past
/// the songs a play can hold, when they begin at the start itself. The wait
/// counts its songs from here and the play cuts its list here, so the two
/// always agree.
fn window_start(index: usize) -> usize {
    if index < MAX_PLAYED_URIS { 0 } else { index }
}

/// The row of a release's songs a play starts at: the clicked song, found
/// again by its URI should the rows have moved, or the release's first
/// playable song.
fn start_row(songs: &[Track], start: &Option<(u32, String)>) -> Option<usize> {
    match start {
        Some((row, uri)) => {
            let row = *row as usize;
            if songs
                .get(row)
                .is_some_and(|song| song.uri == *uri && playable_song(song))
            {
                Some(row)
            } else {
                songs
                    .iter()
                    .position(|song| song.uri == *uri && playable_song(song))
            }
        }
        None => songs.iter().position(playable_song),
    }
}

/// The song a play starts from, once its release's songs have arrived.
fn starting_song(page: &DiscographyPage, play: &PendingDiscographyPlay) -> Option<Track> {
    let songs = page.tracks.get(play.releases.first()?)?.songs()?;
    songs.get(start_row(songs, &play.start)?).cloned()
}

/// The playable songs from the clicked release to the end of the page, in
/// the order shown at the click, and the place the play starts at. The
/// list begins with the clicked release's first song, so Previous works
/// within it, while Shuffle and Repeat keep to the releases from it on.
/// Releases that failed add nothing; the list ends at the first release
/// still loading, so a play that could not wait longer skips nothing.
fn play_list(page: &DiscographyPage, play: &PendingDiscographyPlay) -> Option<(Vec<Track>, usize)> {
    let clicked = page
        .tracks
        .get(play.releases.first()?)
        .and_then(ReleaseTracks::songs);
    let begin = clicked.and_then(|songs| start_row(songs, &play.start));
    // The clicked song is gone, or the release's songs never came.
    if begin.is_none() && (play.start.is_some() || clicked.is_none()) {
        return None;
    }
    let mut songs = Vec::new();
    let mut index = None;
    for (position, id) in play.releases.iter().enumerate() {
        let tracks = page.tracks.get(id);
        if position > 0 && !tracks.is_some_and(ReleaseTracks::resolved) {
            break;
        }
        let Some(tracks) = tracks.and_then(ReleaseTracks::songs) else {
            continue;
        };
        for (row, song) in tracks.iter().enumerate() {
            if !playable_song(song) {
                continue;
            }
            if position == 0 && Some(row) == begin {
                index = Some(songs.len());
            }
            songs.push(song.clone());
        }
    }
    // A release with nothing playable starts at its follower's first song.
    (!songs.is_empty()).then(|| (songs, index.unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::Page as ApiPage;

    const ARTIST: &str = "art";

    /// A signed-in app with nothing sent anywhere: requests are recorded,
    /// and a play waits in `queued_play` for this computer's player. Its
    /// playback session has just failed a batch of song lists, so they come
    /// from the Web API a release at a time, as without local playback;
    /// [`session_app`] reads them in batches. A test that sends
    /// `LocalPlayback::Ready`, or runs for two minutes, reads them in
    /// batches too.
    fn app(name: &str) -> App {
        let root = std::env::temp_dir().join(format!(
            "spotifast-discography-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut app = App::new(
            &crate::backend::Waker::default(),
            AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
                autostart: false,
            },
        );
        app.local_ready = true;
        app.auth = AuthStatus::Connected {
            username: "test".into(),
        };
        app.backend.set_offline(true);
        app.discography_session_failed_at = Some(Instant::now());
        app
    }

    /// As [`app`], with a playback session that reads song lists in
    /// batches.
    fn session_app(name: &str) -> App {
        let mut app = app(name);
        app.discography_session_failed_at = None;
        app
    }

    fn release(id: &str, date: &str, songs: u32) -> Album {
        Album {
            id: id.into(),
            uri: format!("spotify:album:{id}"),
            name: format!("Release {id}"),
            album_type: Some("album".into()),
            release_date: Some(date.into()),
            total_tracks: Some(songs),
            ..Album::default()
        }
    }

    fn song(id: &str) -> Track {
        Track {
            id: Some(id.into()),
            uri: format!("spotify:track:{id}"),
            name: id.into(),
            duration_ms: 200_000,
            ..Track::default()
        }
    }

    fn uris(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| format!("spotify:track:{id}")).collect()
    }

    fn page<T>(items: Vec<T>, offset: u32, limit: u32, total: u32) -> ApiPage<T> {
        ApiPage {
            items,
            offset,
            limit,
            total,
            next: (offset + limit < total).then(|| "next".into()),
        }
    }

    fn generation(app: &App) -> u64 {
        app.discography_pages[ARTIST].generation
    }

    /// Opens the page and answers its release list in one page.
    fn open(app: &mut App, ctx: &egui::Context, releases: Vec<Album>) {
        app.apply(
            Action::OpenDiscography {
                artist_id: ARTIST.into(),
                filter: DiscographyFilter::All,
            },
            ctx,
        );
        let total = releases.len() as u32;
        let generation = generation(app);
        app.handle_api(ApiResponse::DiscographyAlbums {
            artist_id: ARTIST.into(),
            groups: DiscographyFilter::All.groups().into(),
            offset: 0,
            generation,
            result: Ok(page(releases, 0, 50, total)),
        });
    }

    fn songs_arrive(app: &mut App, album: &str, songs: Vec<Track>) {
        let generation = generation(app);
        let total = songs.len() as u32;
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: album.into(),
            offset: 0,
            generation,
            result: Ok(page(songs, 0, total.max(50), total)),
        });
    }

    fn songs(prefix: &str, count: usize) -> Vec<Track> {
        (0..count).map(|n| song(&format!("{prefix}{n}"))).collect()
    }

    /// A play from song `c1` of the newest release, waiting for "a".
    fn waiting_play(app: &mut App, ctx: &egui::Context) {
        open(app, ctx, three_releases());
        songs_arrive(app, "c", vec![song("c1"), song("c2")]);
        songs_arrive(app, "b", vec![song("b1")]);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "c".into(),
                },
                uri: "spotify:track:c1".into(),
                index: 0,
            },
            ctx,
        );
        assert!(
            app.pending_discography_play.is_some(),
            "the play waits for a"
        );
    }

    /// The song lists asked for since the last call, by album id.
    fn asked_songs(app: &App) -> Vec<String> {
        app.backend
            .take_discography_requests()
            .into_iter()
            .filter_map(|request| match request {
                ApiRequest::DiscographyTracks { album_id, .. } => Some(album_id),
                _ => None,
            })
            .collect()
    }

    /// Three releases, newest first: c (2024), b (2022), a (2019).
    fn three_releases() -> Vec<Album> {
        vec![
            release("a", "2019-05-01", 2),
            release("b", "2022", 3),
            release("c", "2024-06-07", 2),
        ]
    }

    fn load_all(app: &mut App) {
        songs_arrive(app, "c", vec![song("c1"), song("c2")]);
        songs_arrive(app, "b", vec![song("b1"), song("b2"), song("b3")]);
        songs_arrive(app, "a", vec![song("a1"), song("a2")]);
    }

    fn played(app: &App) -> (Vec<String>, Option<u32>) {
        let request = app.queued_play.as_ref().expect("a play request");
        (request.uris.clone(), request.offset_position)
    }

    /// Song `id` playing here, started from its copy at row 3 of a playlist
    /// that holds it more than once, so that copy alone shows as playing.
    fn playing_from_playlist_copy(app: &mut App, id: &str) -> RowContext {
        app.local.track = Some(crate::player::LocalTrack {
            uri: format!("spotify:track:{id}"),
            duration_ms: 200_000,
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.played_row = Some(PlayedRow {
            context: "spotify:playlist:pl".into(),
            view: false,
            index: 3,
            uri: format!("spotify:track:{id}"),
        });
        RowContext::Context {
            uri: "spotify:playlist:pl".into(),
            editable_playlist: None,
        }
    }

    /// One pass of the app's housekeeping, as between frames.
    fn background_pass(app: &mut App, ctx: &egui::Context) {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.background_frame(ui.ctx());
        });
        output.textures_delta.clear();
    }

    #[test]
    fn the_release_list_loads_every_page_before_any_song_list() {
        let ctx = egui::Context::default();
        let mut app = app("pages");
        app.apply(
            Action::OpenDiscography {
                artist_id: ARTIST.into(),
                filter: DiscographyFilter::Albums,
            },
            &ctx,
        );
        assert_eq!(app.page(), &Page::Discography(ARTIST.into()));
        assert_eq!(
            app.discography_pages[ARTIST].filter,
            DiscographyFilter::Albums
        );
        let generation = generation(&app);
        let first = app.backend.take_discography_requests();
        assert!(matches!(
            first.as_slice(),
            [ApiRequest::DiscographyAlbums { groups, offset: 0, .. }] if groups == "album"
        ));
        // A personal app's page of ten: the next page is asked for at once.
        app.handle_api(ApiResponse::DiscographyAlbums {
            artist_id: ARTIST.into(),
            groups: "album".into(),
            offset: 0,
            generation,
            result: Ok(page(vec![release("a", "2019", 1)], 0, 10, 11)),
        });
        let next = app.backend.take_discography_requests();
        assert!(
            matches!(
                next.as_slice(),
                [ApiRequest::DiscographyAlbums { offset: 10, .. }]
            ),
            "only the next release page, no song list yet: {next:?}"
        );
        app.handle_api(ApiResponse::DiscographyAlbums {
            artist_id: ARTIST.into(),
            groups: "album".into(),
            offset: 10,
            generation,
            result: Ok(page(vec![release("b", "2024", 1)], 10, 10, 11)),
        });
        assert!(app.discography_pages[ARTIST].list().unwrap().is_complete());
        assert_eq!(asked_songs(&app), ["b", "a"], "newest first, once whole");
        app.backend.shutdown();
    }

    #[test]
    fn a_release_page_that_does_not_move_forward_fails_instead_of_looping() {
        let ctx = egui::Context::default();
        let mut app = app("stuck");
        app.apply(
            Action::OpenDiscography {
                artist_id: ARTIST.into(),
                filter: DiscographyFilter::All,
            },
            &ctx,
        );
        app.backend.take_discography_requests();
        let generation = generation(&app);
        // Spotify says there is more, yet gives no way past this page.
        app.handle_api(ApiResponse::DiscographyAlbums {
            artist_id: ARTIST.into(),
            groups: DiscographyFilter::All.groups().into(),
            offset: 0,
            generation,
            result: Ok(ApiPage {
                items: Vec::new(),
                offset: 0,
                limit: 0,
                total: 5,
                next: Some("next".into()),
            }),
        });
        let list = app.discography_pages[ARTIST].list().unwrap();
        assert!(list.error.is_some());
        assert!(app.backend.take_discography_requests().is_empty());
        app.backend.shutdown();
    }

    #[test]
    fn song_lists_load_two_at_a_time_and_stop_when_the_page_closes() {
        let ctx = egui::Context::default();
        let mut app = app("pump");
        let releases = (0..6)
            .map(|index| release(&format!("r{index}"), &format!("201{index}"), 1))
            .collect();
        open(&mut app, &ctx, releases);
        assert_eq!(asked_songs(&app), ["r5", "r4"]);
        songs_arrive(&mut app, "r5", vec![song("s5")]);
        assert_eq!(asked_songs(&app), ["r3"], "one in, one more out");
        app.apply(Action::Open(Page::Home), &ctx);
        songs_arrive(&mut app, "r4", vec![song("s4")]);
        assert!(
            asked_songs(&app).is_empty(),
            "nothing more once the page is left"
        );
        app.apply(Action::Back, &ctx);
        assert_eq!(
            asked_songs(&app),
            ["r2"],
            "coming back picks up where it stopped"
        );
        app.backend.shutdown();
    }

    #[test]
    fn the_releases_on_screen_load_before_those_above_them() {
        let ctx = egui::Context::default();
        let mut app = app("in-view");
        let releases = (0..8)
            .map(|index| release(&format!("r{index}"), &format!("201{index}"), 1))
            .collect();
        open(&mut app, &ctx, releases);
        asked_songs(&app);
        // The user scrolled to the fifth and sixth releases shown.
        app.apply(
            Action::DiscographyInView {
                artist_id: ARTIST.into(),
                first: 4,
                last: 5,
            },
            &ctx,
        );
        songs_arrive(&mut app, "r7", vec![song("s7")]);
        songs_arrive(&mut app, "r6", vec![song("s6")]);
        assert_eq!(asked_songs(&app), ["r3", "r2"]);
        app.backend.shutdown();
    }

    #[test]
    fn a_rate_limit_stops_every_list_and_retry_reads_that_release_first() {
        let ctx = egui::Context::default();
        let mut app = app("halt");
        open(&mut app, &ctx, three_releases());
        assert_eq!(asked_songs(&app), ["c", "b"]);
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "c".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        let page = &app.discography_pages[ARTIST];
        assert!(page.halted.is_some());
        assert!(!page.tracks.contains_key("c"), "read again, not failed");
        songs_arrive(&mut app, "b", vec![song("b1")]);
        assert!(asked_songs(&app).is_empty(), "halted: nothing new is asked");
        app.apply(
            Action::RetryDiscography {
                artist_id: ARTIST.into(),
                album_id: None,
            },
            &ctx,
        );
        assert_eq!(asked_songs(&app), ["c", "a"]);
        app.backend.shutdown();
    }

    /// Ages every song list on its way past the wait for a lost answer.
    fn age_unanswered(app: &mut App) {
        for tracks in app
            .discography_pages
            .get_mut(ARTIST)
            .unwrap()
            .tracks
            .values_mut()
        {
            if let ReleaseTracks::Loading { asked, .. } | ReleaseTracks::Batched { asked, .. } =
                tracks
            {
                *asked -= TRACKS_LOST_AFTER;
            }
        }
    }

    #[test]
    fn a_song_list_whose_answer_never_comes_is_asked_again() {
        let ctx = egui::Context::default();
        let mut app = app("lost-answer");
        open(&mut app, &ctx, three_releases());
        assert_eq!(asked_songs(&app), ["c", "b"]);
        app.forget_lost_song_lists();
        assert!(
            asked_songs(&app).is_empty(),
            "an answer on its way is waited for"
        );
        // The backend let go of both answers.
        age_unanswered(&mut app);
        app.forget_lost_song_lists();
        assert_eq!(asked_songs(&app), ["c", "b"]);
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        assert_eq!(asked_songs(&app), ["a"], "the slot was let go");
        // A late copy of the first answer changes nothing.
        songs_arrive(&mut app, "c", vec![song("c1")]);
        assert_eq!(
            app.discography_pages[ARTIST].tracks["c"]
                .songs()
                .map(<[Track]>::len),
            Some(2)
        );
        assert!(asked_songs(&app).is_empty());
        app.backend.shutdown();
    }

    #[test]
    fn page_retry_asks_again_for_song_lists_a_halt_left_unanswered() {
        let ctx = egui::Context::default();
        let mut app = app("halt-unanswered");
        open(&mut app, &ctx, three_releases());
        assert_eq!(asked_songs(&app), ["c", "b"]);
        let generation = generation(&app);
        // The personal app's sign-in ends: c says so and stops the page,
        // and the backend lets go of b's answer.
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "c".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::SignInExpired {
                api_source: crate::api::ApiSource::Personal,
            }),
        });
        assert!(app.discography_pages[ARTIST].halted.is_some());
        app.apply(
            Action::RetryDiscography {
                artist_id: ARTIST.into(),
                album_id: None,
            },
            &ctx,
        );
        assert_eq!(asked_songs(&app), ["c", "b"]);
        app.backend.shutdown();
    }

    #[test]
    fn a_release_retry_asks_again_for_song_lists_a_halt_left_unanswered() {
        let ctx = egui::Context::default();
        let mut app = app("release-retry-unanswered");
        open(&mut app, &ctx, three_releases());
        assert_eq!(asked_songs(&app), ["c", "b"]);
        let generation = generation(&app);
        // Spotify cannot list c, and a takes its slot.
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "c".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::Status {
                status: 404,
                message: "Not found".into(),
            }),
        });
        assert_eq!(asked_songs(&app), ["a"]);
        // The personal app's sign-in ends: b says so and stops the page,
        // and the backend lets go of a's answer.
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "b".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::SignInExpired {
                api_source: crate::api::ApiSource::Personal,
            }),
        });
        assert!(app.discography_pages[ARTIST].halted.is_some());
        // The user retries the release that failed, not the page.
        app.apply(
            Action::RetryDiscography {
                artist_id: ARTIST.into(),
                album_id: Some("c".into()),
            },
            &ctx,
        );
        assert_eq!(asked_songs(&app), ["c", "b"]);
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        assert_eq!(asked_songs(&app), ["a"]);
        app.backend.shutdown();
    }

    #[test]
    fn page_retry_after_a_rate_limit_waits_for_a_song_list_on_its_way() {
        let ctx = egui::Context::default();
        let mut app = app("halt-on-its-way");
        open(&mut app, &ctx, three_releases());
        assert_eq!(asked_songs(&app), ["c", "b"]);
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "c".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert!(app.discography_pages[ARTIST].halted.is_some());
        app.apply(
            Action::RetryDiscography {
                artist_id: ARTIST.into(),
                album_id: None,
            },
            &ctx,
        );
        assert_eq!(asked_songs(&app), ["c"], "b's answer is still coming");
        songs_arrive(&mut app, "b", vec![song("b1")]);
        assert_eq!(
            app.discography_pages[ARTIST].tracks["b"]
                .songs()
                .map(<[Track]>::len),
            Some(1)
        );
        assert_eq!(asked_songs(&app), ["a"]);
        app.backend.shutdown();
    }

    #[test]
    fn each_page_of_a_long_release_is_waited_for_afresh() {
        let ctx = egui::Context::default();
        let mut app = app("long-lost");
        open(&mut app, &ctx, vec![release("long", "2020", 51)]);
        asked_songs(&app);
        age_unanswered(&mut app);
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "long".into(),
            offset: 0,
            generation,
            result: Ok(page(songs("t", 50), 0, 50, 51)),
        });
        assert!(matches!(
            app.backend.take_discography_requests().as_slice(),
            [ApiRequest::DiscographyTracks { offset: 50, .. }]
        ));
        app.forget_lost_song_lists();
        assert!(asked_songs(&app).is_empty());
        assert!(matches!(
            app.discography_pages[ARTIST].tracks.get("long"),
            Some(ReleaseTracks::Loading { next: 50, .. })
        ));
        app.backend.shutdown();
    }

    #[test]
    fn offline_the_page_asks_for_nothing_until_the_network_is_back() {
        let ctx = egui::Context::default();
        let mut app = app("offline");
        app.network_reachable = false;
        app.apply(
            Action::OpenDiscography {
                artist_id: ARTIST.into(),
                filter: DiscographyFilter::All,
            },
            &ctx,
        );
        assert!(app.discography_pages.contains_key(ARTIST));
        assert!(app.backend.take_discography_requests().is_empty());
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        assert!(matches!(
            app.backend.take_discography_requests().as_slice(),
            [ApiRequest::DiscographyAlbums { offset: 0, .. }]
        ));
        app.backend.shutdown();
    }

    #[test]
    fn song_lists_a_lost_network_stopped_are_asked_again_when_it_is_back() {
        let ctx = egui::Context::default();
        let mut app = app("network-back");
        open(&mut app, &ctx, three_releases());
        assert_eq!(asked_songs(&app), ["c", "b"]);
        app.network_reachable = false;
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "c".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::Network(
                "error sending request for url (https://api.spotify.com/v1/albums/c/tracks)".into(),
            )),
        });
        // Said as every other page says it, never with the request's URL.
        assert_eq!(
            app.discography_pages[ARTIST].halted.as_deref(),
            Some("Network connection error. Check your connection.")
        );
        songs_arrive(&mut app, "b", vec![song("b1")]);
        assert!(
            asked_songs(&app).is_empty(),
            "offline: nothing new is asked"
        );
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        assert!(app.discography_pages[ARTIST].halted.is_none());
        assert_eq!(asked_songs(&app), ["c", "a"]);
        app.backend.shutdown();
    }

    #[test]
    fn a_play_waiting_off_its_page_asks_again_when_the_network_is_back() {
        let ctx = egui::Context::default();
        let mut app = app("wait-network-back");
        open(&mut app, &ctx, three_releases());
        assert_eq!(asked_songs(&app), ["c", "b"]);
        // The network goes while c and b are on their way: they still
        // arrive, and a is not asked for.
        app.network_reachable = false;
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        songs_arrive(&mut app, "b", vec![song("b1")]);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "c".into(),
                },
                uri: "spotify:track:c1".into(),
                index: 0,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_some());
        assert!(asked_songs(&app).is_empty());
        app.apply(Action::Open(Page::Home), &ctx);
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        assert_eq!(asked_songs(&app), ["a"]);
        songs_arrive(&mut app, "a", vec![song("a1")]);
        assert_eq!(played(&app).0, uris(&["c1", "c2", "b1", "a1"]));
        app.backend.shutdown();
    }

    #[test]
    fn coming_back_to_the_page_asks_for_what_offline_held_back() {
        let ctx = egui::Context::default();
        let mut app = app("held-back");
        open(&mut app, &ctx, three_releases());
        load_all(&mut app);
        app.network_reachable = false;
        app.apply(
            Action::DiscographyInView {
                artist_id: ARTIST.into(),
                first: 1,
                last: 2,
            },
            &ctx,
        );
        assert!(!app.saved_pending.contains("spotify:album:b"));
        // The network comes back on another page; the view of this one has
        // not moved when the user returns.
        app.apply(Action::Open(Page::Home), &ctx);
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        app.apply(Action::Back, &ctx);
        assert_eq!(app.page(), &Page::Discography(ARTIST.into()));
        assert!(app.saved_pending.contains("spotify:album:b"));
        assert!(app.saved_pending.contains("spotify:track:a1"));
        app.backend.shutdown();
    }

    #[test]
    fn a_missing_release_fails_alone_and_keeps_the_others_loading() {
        let ctx = egui::Context::default();
        let mut app = app("missing");
        open(&mut app, &ctx, three_releases());
        asked_songs(&app);
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "c".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::Status {
                status: 404,
                message: "Not found".into(),
            }),
        });
        let page = &app.discography_pages[ARTIST];
        assert!(page.halted.is_none());
        assert!(matches!(
            page.tracks.get("c"),
            Some(ReleaseTracks::Failed(_))
        ));
        assert_eq!(asked_songs(&app), ["a"]);
        app.backend.shutdown();
    }

    #[test]
    fn a_long_release_shows_only_once_every_page_has_come_and_names_its_album() {
        let ctx = egui::Context::default();
        let mut app = app("long");
        open(&mut app, &ctx, vec![release("long", "2020", 51)]);
        asked_songs(&app);
        let generation = generation(&app);
        let first: Vec<Track> = (0..50).map(|n| song(&format!("t{n}"))).collect();
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "long".into(),
            offset: 0,
            generation,
            result: Ok(page(first, 0, 50, 51)),
        });
        assert!(matches!(
            app.discography_pages[ARTIST].tracks.get("long"),
            Some(ReleaseTracks::Loading { next: 50, .. })
        ));
        assert!(matches!(
            app.backend.take_discography_requests().as_slice(),
            [ApiRequest::DiscographyTracks { offset: 50, .. }]
        ));
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "long".into(),
            offset: 50,
            generation,
            result: Ok(page(vec![song("t50")], 50, 50, 51)),
        });
        let songs = app.discography_pages[ARTIST].tracks["long"]
            .songs()
            .unwrap()
            .to_vec();
        assert_eq!(songs.len(), 51);
        assert!(
            songs
                .iter()
                .all(|song| song.album.as_ref().is_some_and(|album| album.id == "long"))
        );
        app.backend.shutdown();
    }

    #[test]
    fn answers_for_a_reloaded_page_change_nothing() {
        let ctx = egui::Context::default();
        let mut app = app("stale");
        open(&mut app, &ctx, three_releases());
        asked_songs(&app);
        let old = generation(&app);
        app.apply(
            Action::SetDiscographyOrder {
                artist_id: ARTIST.into(),
                newest_first: false,
            },
            &ctx,
        );
        app.apply(Action::Reload(Page::Discography(ARTIST.into())), &ctx);
        let reloaded = &app.discography_pages[ARTIST];
        assert_ne!(reloaded.generation, old);
        assert!(
            !reloaded.newest_first,
            "Reload keeps the order the user chose"
        );
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "c".into(),
            offset: 0,
            generation: old,
            result: Ok(page(vec![song("c1")], 0, 50, 1)),
        });
        assert!(!app.discography_pages[ARTIST].tracks.contains_key("c"));
        app.backend.shutdown();
    }

    #[test]
    fn a_song_plays_from_itself_to_the_end_of_the_page() {
        let ctx = egui::Context::default();
        let mut app = app("play-row");
        open(&mut app, &ctx, three_releases());
        load_all(&mut app);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "b".into(),
                },
                uri: "spotify:track:b2".into(),
                index: 1,
            },
            &ctx,
        );
        // The list starts with the clicked release, so Previous works within
        // it, and runs to the oldest release's last song.
        assert_eq!(
            played(&app),
            (uris(&["b1", "b2", "b3", "a1", "a2"]), Some(1))
        );
        assert_eq!(app.current_track_uri().as_deref(), Some("spotify:track:b2"));
        app.backend.shutdown();
    }

    #[test]
    fn a_play_here_forgets_the_playlist_copy_an_earlier_play_marked() {
        let ctx = egui::Context::default();
        let mut app = app("played-copy");
        open(&mut app, &ctx, three_releases());
        load_all(&mut app);
        // b2 last started from its second copy in a playlist.
        let playlist = RowContext::Context {
            uri: "spotify:playlist:pl".into(),
            editable_playlist: None,
        };
        app.played_row = Some(PlayedRow {
            context: "spotify:playlist:pl".into(),
            view: false,
            index: 3,
            uri: "spotify:track:b2".into(),
        });
        let row = RowContext::Discography {
            artist_id: ARTIST.into(),
            album_id: "b".into(),
        };
        app.apply(
            Action::PlayFromRow {
                context: row.clone(),
                uri: "spotify:track:b2".into(),
                index: 1,
            },
            &ctx,
        );
        assert!(app.row_is_played_copy(&row, 1, "spotify:track:b2"));
        assert!(
            app.row_is_played_copy(&playlist, 1, "spotify:track:b2"),
            "every copy in the playlist is the playing song again"
        );
        app.backend.shutdown();
    }

    /// The playlist copy the playing song was started from stays the
    /// marked one through a wait that ends in nothing.
    #[test]
    fn a_cancelled_wait_leaves_the_playlist_copy_the_playing_song_came_from() {
        let ctx = egui::Context::default();
        let mut app = app("wait-played-copy");
        let playlist = playing_from_playlist_copy(&mut app, "x");
        waiting_play(&mut app, &ctx);
        assert_eq!(app.current_track_uri().as_deref(), Some("spotify:track:c1"));
        background_pass(&mut app, &ctx);
        app.apply(Action::TogglePlay, &ctx);
        assert!(app.pending_discography_play.is_none());
        assert_eq!(app.current_track_uri().as_deref(), Some("spotify:track:x"));
        background_pass(&mut app, &ctx);
        assert!(app.row_is_played_copy(&playlist, 3, "spotify:track:x"));
        assert!(
            !app.row_is_played_copy(&playlist, 8, "spotify:track:x"),
            "the other copy stays an ordinary row"
        );
        app.backend.shutdown();
    }

    /// A release play has no song to show until its songs arrive, and the
    /// song playing meanwhile keeps its one marked copy until the play
    /// starts.
    #[test]
    fn a_release_waiting_for_its_songs_keeps_the_playing_copy_marked_until_it_starts() {
        let ctx = egui::Context::default();
        let mut app = app("wait-release-played-copy");
        let playlist = playing_from_playlist_copy(&mut app, "x");
        open(&mut app, &ctx, three_releases());
        songs_arrive(&mut app, "c", vec![song("c1")]);
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "b".into(),
                start: None,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_some());
        assert_eq!(
            app.current_track_uri().as_deref(),
            Some("spotify:track:x"),
            "no song is shown yet"
        );
        background_pass(&mut app, &ctx);
        assert!(app.row_is_played_copy(&playlist, 3, "spotify:track:x"));
        assert!(
            !app.row_is_played_copy(&playlist, 8, "spotify:track:x"),
            "the other copy stays an ordinary row"
        );
        songs_arrive(&mut app, "b", vec![song("b1"), song("b2"), song("b3")]);
        assert_eq!(app.current_track_uri().as_deref(), Some("spotify:track:b1"));
        background_pass(&mut app, &ctx);
        assert!(
            app.played_row.is_some(),
            "x still plays while the release waits for a"
        );
        songs_arrive(&mut app, "a", vec![song("a1"), song("a2")]);
        assert_eq!(
            played(&app),
            (uris(&["b1", "b2", "b3", "a1", "a2"]), Some(0))
        );
        assert!(app.played_row.is_none(), "the release started");
        app.backend.shutdown();
    }

    /// The song playing while a play waits can still change, and the copy
    /// it was started from stops being the marked one then.
    #[test]
    fn a_song_change_during_a_wait_unmarks_the_playlist_copy() {
        let ctx = egui::Context::default();
        let mut app = app("wait-song-change-played-copy");
        let playlist = playing_from_playlist_copy(&mut app, "x");
        waiting_play(&mut app, &ctx);
        background_pass(&mut app, &ctx);
        assert!(!app.row_is_played_copy(&playlist, 8, "spotify:track:x"));
        // x ended and the playlist moved on.
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:z".into(),
            duration_ms: 200_000,
            ..Default::default()
        });
        background_pass(&mut app, &ctx);
        assert!(app.pending_discography_play.is_some());
        assert!(
            app.row_is_played_copy(&playlist, 8, "spotify:track:x"),
            "a later return to x may be any copy"
        );
        app.backend.shutdown();
    }

    /// A song asked for just before the click counts as the one playing
    /// while the play waits, so its copy stays marked.
    #[test]
    fn a_song_asked_for_before_a_wait_keeps_its_playlist_copy_marked() {
        let ctx = egui::Context::default();
        let mut app = app("wait-intent-played-copy");
        let playlist = playing_from_playlist_copy(&mut app, "x");
        // x was asked for, and z still plays until it starts.
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:z".into(),
            duration_ms: 200_000,
            ..Default::default()
        });
        app.expect_track("spotify:track:x".into(), 0);
        waiting_play(&mut app, &ctx);
        background_pass(&mut app, &ctx);
        assert!(app.row_is_played_copy(&playlist, 3, "spotify:track:x"));
        assert!(!app.row_is_played_copy(&playlist, 8, "spotify:track:x"));
        app.backend.shutdown();
    }

    /// A play of the song a playlist copy started chose no copy, so every
    /// one shows it at the click, as once it starts; a cancel gives the
    /// chosen copy back.
    #[test]
    fn a_waiting_play_of_the_marked_song_shows_every_playlist_copy() {
        let ctx = egui::Context::default();
        let mut app = app("wait-same-played-copy");
        let playlist = playing_from_playlist_copy(&mut app, "c1");
        waiting_play(&mut app, &ctx);
        background_pass(&mut app, &ctx);
        assert!(app.row_is_played_copy(&playlist, 3, "spotify:track:c1"));
        assert!(
            app.row_is_played_copy(&playlist, 8, "spotify:track:c1"),
            "the click played c1 from none of the playlist's rows"
        );
        app.apply(Action::TogglePlay, &ctx);
        assert!(app.pending_discography_play.is_none());
        background_pass(&mut app, &ctx);
        assert!(app.row_is_played_copy(&playlist, 3, "spotify:track:c1"));
        assert!(
            !app.row_is_played_copy(&playlist, 8, "spotify:track:c1"),
            "the copy c1 came from is the marked one again"
        );
        app.backend.shutdown();
    }

    #[test]
    fn oldest_first_plays_toward_the_newest_release() {
        let ctx = egui::Context::default();
        let mut app = app("oldest");
        open(&mut app, &ctx, three_releases());
        load_all(&mut app);
        app.apply(
            Action::SetDiscographyOrder {
                artist_id: ARTIST.into(),
                newest_first: false,
            },
            &ctx,
        );
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "a".into(),
                },
                uri: "spotify:track:a2".into(),
                index: 1,
            },
            &ctx,
        );
        assert_eq!(
            played(&app),
            (uris(&["a1", "a2", "b1", "b2", "b3", "c1", "c2"]), Some(1))
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_release_play_starts_at_its_first_playable_song_and_skips_the_rest_unplayable() {
        let ctx = egui::Context::default();
        let mut app = app("play-release");
        open(&mut app, &ctx, three_releases());
        let mut gone = song("b1");
        gone.is_playable = Some(false);
        let mut local = song("local");
        local.uri = "spotify:local:Artist:Album:Song:180".into();
        local.is_local = true;
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        songs_arrive(&mut app, "b", vec![gone, song("b2"), local]);
        songs_arrive(&mut app, "a", vec![song("a1"), song("a2")]);
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "b".into(),
                start: None,
            },
            &ctx,
        );
        assert_eq!(played(&app), (uris(&["b2", "a1", "a2"]), Some(0)));
        app.backend.shutdown();
    }

    #[test]
    fn a_song_on_two_releases_starts_where_it_was_clicked() {
        let ctx = egui::Context::default();
        let mut app = app("twice");
        open(&mut app, &ctx, three_releases());
        songs_arrive(&mut app, "c", vec![song("same"), song("c2")]);
        songs_arrive(&mut app, "b", vec![song("b1"), song("same"), song("b3")]);
        songs_arrive(&mut app, "a", vec![song("a1"), song("a2")]);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "b".into(),
                },
                uri: "spotify:track:same".into(),
                index: 1,
            },
            &ctx,
        );
        assert_eq!(
            played(&app),
            (uris(&["b1", "same", "b3", "a1", "a2"]), Some(1)),
            "every version stays, and the clicked copy starts"
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_play_waits_for_the_songs_after_it_and_keeps_showing_the_click() {
        let ctx = egui::Context::default();
        let mut app = app("wait");
        open(&mut app, &ctx, three_releases());
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        songs_arrive(&mut app, "b", vec![song("b1"), song("b2"), song("b3")]);
        // "a" is still on its way.
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "c".into(),
                },
                uri: "spotify:track:c2".into(),
                index: 1,
            },
            &ctx,
        );
        assert!(
            app.queued_play.is_none(),
            "nothing is sent before the list is whole"
        );
        assert_eq!(app.current_track_uri().as_deref(), Some("spotify:track:c2"));
        assert!(app.play_pending("spotify:track:c2"));
        assert_eq!(
            app.now_playing().map(|now| now.title),
            Some("c2".to_string()),
            "the player bar shows the clicked song at once"
        );
        let now = app.now_playing().unwrap();
        assert!(
            now.playing && !now.loading && !app.any_play_pending(),
            "the player bar offers a Pause that cancels the wait, not a spinner"
        );
        // Reports about the song still playing clear the ordinary hold;
        // they do not reach the waiting play's own.
        app.intent_track = None;
        app.clear_play_pending();
        assert_eq!(app.current_track_uri().as_deref(), Some("spotify:track:c2"));
        assert!(app.play_pending("spotify:track:c2"));
        songs_arrive(&mut app, "a", vec![song("a1"), song("a2")]);
        assert_eq!(
            played(&app),
            (uris(&["c1", "c2", "b1", "b2", "b3", "a1", "a2"]), Some(1))
        );
        assert!(app.pending_discography_play.is_none());
        app.backend.shutdown();
    }

    #[test]
    fn a_waiting_play_follows_the_order_shown_at_the_click() {
        let ctx = egui::Context::default();
        let mut app = app("snapshot");
        open(&mut app, &ctx, three_releases());
        songs_arrive(&mut app, "c", vec![song("c1")]);
        songs_arrive(&mut app, "b", vec![song("b1")]);
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "b".into(),
                start: None,
            },
            &ctx,
        );
        app.apply(
            Action::SetDiscographyOrder {
                artist_id: ARTIST.into(),
                newest_first: false,
            },
            &ctx,
        );
        songs_arrive(&mut app, "a", vec![song("a1")]);
        assert_eq!(played(&app), (uris(&["b1", "a1"]), Some(0)));
        app.backend.shutdown();
    }

    #[test]
    fn pause_while_a_play_waits_cancels_it() {
        let ctx = egui::Context::default();
        let mut app = app("cancel");
        open(&mut app, &ctx, three_releases());
        songs_arrive(&mut app, "c", vec![song("c1")]);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "c".into(),
                },
                uri: "spotify:track:c1".into(),
                index: 0,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_some());
        app.apply(Action::TogglePlay, &ctx);
        assert!(app.pending_discography_play.is_none());
        assert_ne!(app.current_track_uri().as_deref(), Some("spotify:track:c1"));
        songs_arrive(&mut app, "b", vec![song("b1")]);
        songs_arrive(&mut app, "a", vec![song("a1")]);
        assert!(app.queued_play.is_none(), "a cancelled play never starts");
        app.backend.shutdown();
    }

    #[test]
    fn next_previous_another_device_or_a_queue_row_cancel_a_waiting_play() {
        let ctx = egui::Context::default();
        let actions = [
            Action::Next,
            Action::Previous,
            Action::Transfer("speaker".into()),
            Action::PlayFromRow {
                context: RowContext::Queue,
                uri: "spotify:track:queued".into(),
                index: 0,
            },
        ];
        for (index, action) in actions.into_iter().enumerate() {
            let mut app = app(&format!("cancel-{index}"));
            waiting_play(&mut app, &ctx);
            app.apply(action, &ctx);
            assert!(app.pending_discography_play.is_none(), "action {index}");
            songs_arrive(&mut app, "a", vec![song("a1")]);
            let played = app.queued_play.as_ref().map(|request| request.uris.clone());
            assert_ne!(
                played,
                Some(uris(&["c1", "c2", "b1", "a1"])),
                "action {index}: a cancelled play never starts"
            );
            app.backend.shutdown();
        }
    }

    /// A speaker that had to be woken is switched to once it shows up,
    /// without an Action, and that too means the user moved on from a play
    /// clicked before the speaker was chosen. The song a waiting play shows
    /// does not count as music playing when the speaker is chosen: only what
    /// really played then, or plays when the speaker appears, makes it play.
    #[test]
    fn a_speaker_woken_for_a_switch_cancels_a_waiting_play() {
        let ctx = egui::Context::default();
        let speaker = crate::zeroconf::Receiver {
            name: "Kitchen".into(),
            device_id: Some(crate::cast::device_id("Kitchen")),
            address: "192.0.2.1".parse().unwrap(),
            port: 8009,
            path: "/".into(),
            cast: true,
        };
        // Whether the song loaded here played when the speaker was chosen,
        // whether the waiting play starts before the speaker appears (then
        // it counts as music played when the speaker was chosen, even once
        // waking the speaker stops it), and the play flag of the switch.
        for (playing_at_pick, starts, play) in [
            (false, false, false),
            (true, false, true),
            (false, true, true),
        ] {
            let mut app = app(&format!("woken-speaker-{playing_at_pick}-{starts}"));
            app.local.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:x".into(),
                duration_ms: 200_000,
                ..Default::default()
            });
            app.local.playback = if playing_at_pick {
                Playback::Playing
            } else {
                Playback::Paused
            };
            waiting_play(&mut app, &ctx);
            assert!(
                app.now_playing()
                    .is_some_and(|now| now.playing && now.uri == "spotify:track:c1"),
                "the click shows as playing"
            );
            app.apply(Action::ActivateReceiver(Box::new(speaker.clone())), &ctx);
            if starts {
                // The play starts here while the speaker wakes and the player
                // here takes it. The few seconds it shows as playing whatever
                // the player says run out before the speaker appears.
                songs_arrive(&mut app, "a", vec![song("a1")]);
                assert!(app.pending_discography_play.is_none(), "the play started");
                app.queued_play = None;
                app.optimistic_playing = None;
                app.local.track = Some(crate::player::LocalTrack {
                    uri: "spotify:track:c1".into(),
                    duration_ms: 200_000,
                    ..Default::default()
                });
            }
            // Waking the speaker can stop the music here.
            app.local.playback = Playback::Paused;
            app.handle_backend_events(vec![crate::backend::Event::ReceiverActivated {
                name: speaker.name.clone(),
                result: Ok(()),
            }]);
            app.handle_api(ApiResponse::Devices(Ok(vec![crate::api::models::Device {
                id: speaker.device_id.clone(),
                name: speaker.name.clone(),
                ..Default::default()
            }])));
            assert!(app.pending_transfer_to.is_none(), "the switch happened");
            assert!(app.pending_discography_play.is_none());
            let transfers = app.backend.take_transfer_requests();
            assert!(
                matches!(
                    transfers.as_slice(),
                    [ApiRequest::Transfer { device_id, play: sent }]
                        if Some(device_id) == speaker.device_id.as_ref() && *sent == play
                ),
                "{playing_at_pick} {starts}: {transfers:?}"
            );
            app.backend.shutdown();
        }
    }

    /// A play clicked while a sleeping speaker wakes is meant for that
    /// speaker: its arrival keeps the play waiting, moves only what is heard
    /// across, and the play then starts there, even while the player here
    /// still holds the old song because Spotify's handoff has not stopped it
    /// yet. Another speaker chosen once the first is ready does not make the
    /// play count as one clicked before the first was chosen.
    #[test]
    fn a_play_clicked_while_a_speaker_wakes_waits_on_and_starts_there() {
        let ctx = egui::Context::default();
        let receiver = |name: &str| crate::zeroconf::Receiver {
            name: name.into(),
            device_id: Some(crate::cast::device_id(name)),
            address: "192.0.2.1".parse().unwrap(),
            port: 8009,
            path: "/".into(),
            cast: true,
        };
        let speaker = receiver("Kitchen");
        // Whether the player here stops before the last songs arrive, and
        // whether another speaker is chosen once the first one is ready.
        for (stops_first, another) in [(true, false), (false, false), (false, true)] {
            let mut app = app(&format!("woken-speaker-after-{stops_first}-{another}"));
            app.local.connected = true;
            app.local.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:old".into(),
                duration_ms: 200_000,
                ..Default::default()
            });
            app.local.playback = Playback::Paused;
            app.apply(Action::ActivateReceiver(Box::new(speaker.clone())), &ctx);
            waiting_play(&mut app, &ctx);
            app.handle_backend_events(vec![crate::backend::Event::ReceiverActivated {
                name: speaker.name.clone(),
                result: Ok(()),
            }]);
            if another {
                app.apply(Action::ActivateReceiver(Box::new(receiver("Hall"))), &ctx);
            }
            app.handle_api(ApiResponse::Devices(Ok(vec![crate::api::models::Device {
                id: speaker.device_id.clone(),
                name: speaker.name.clone(),
                ..Default::default()
            }])));
            assert!(app.pending_transfer_to.is_none(), "the switch happened");
            let transfers = app.backend.take_transfer_requests();
            assert!(
                matches!(
                    transfers.as_slice(),
                    [ApiRequest::Transfer { device_id, play: false }]
                        if Some(device_id) == speaker.device_id.as_ref()
                ),
                "the paused song moves across paused: {transfers:?}"
            );
            assert!(
                app.pending_discography_play.is_some(),
                "{stops_first} {another}: the play clicked for the speaker still waits"
            );
            assert_eq!(app.current_track_uri().as_deref(), Some("spotify:track:c1"));

            if stops_first {
                // Spotify hands the session to the speaker, and the player
                // here stops.
                app.local.playback = Playback::Stopped;
            }
            app.backend.take_player_commands();
            songs_arrive(&mut app, "a", vec![song("a1")]);
            let requests = app.backend.take_remote_play_requests();
            assert!(
                matches!(
                    requests.as_slice(),
                    [ApiRequest::Remote { device_id: Some(device), play: Some(play), .. }]
                        if Some(device) == speaker.device_id.as_ref()
                            && play.uris == uris(&["c1", "c2", "b1", "a1"])
                            && play.offset_position == Some(0)
                ),
                "{stops_first} {another}: {requests:?}"
            );
            assert!(
                app.queued_play.is_none(),
                "nothing waits for the player here"
            );
            let commands = app.backend.take_player_commands();
            assert!(
                !commands
                    .iter()
                    .any(|command| matches!(command, PlayerCommand::Load(_))),
                "nothing loads here: {commands:?}"
            );
            assert!(app.pending_discography_play.is_none());
            if !stops_first {
                // The handoff stops the player here only now: the song
                // started on the speaker stays.
                let mut state = app.local.clone();
                state.playback = Playback::Stopped;
                app.handle_local(state);
            }
            assert_eq!(app.current_track_uri().as_deref(), Some("spotify:track:c1"));
            app.backend.shutdown();
        }
    }

    #[test]
    fn a_seek_while_a_play_waits_moves_where_its_song_starts() {
        let ctx = egui::Context::default();
        let mut app = app("wait-seek");
        waiting_play(&mut app, &ctx);
        app.apply(Action::Seek(42_000), &ctx);
        app.apply(Action::SeekBy(10_000), &ctx);
        let shown = app.now_playing().expect("the waiting song");
        assert_eq!(shown.uri, "spotify:track:c1");
        assert_eq!(shown.position_ms, 52_000);
        songs_arrive(&mut app, "a", vec![song("a1")]);
        let request = app.queued_play.as_ref().expect("a play request");
        assert_eq!(request.uris, uris(&["c1", "c2", "b1", "a1"]));
        assert_eq!(request.position_ms, 52_000);
        app.backend.shutdown();
    }

    /// A play waiting on another device keeps that device's icon: the
    /// waiting song plays where the music plays now.
    #[test]
    fn a_play_waiting_on_a_phone_keeps_the_phone_icon() {
        let ctx = egui::Context::default();
        let mut app = app("wait-phone");
        app.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                device: Some(Device {
                    id: Some("phone".into()),
                    name: "Galaxy S23".into(),
                    kind: "Smartphone".into(),
                    ..Default::default()
                }),
                is_playing: true,
                item: Some(PlayableItem::Track(song("x"))),
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        assert_eq!(app.target(), Target::Remote(Some("phone".into())));
        waiting_play(&mut app, &ctx);
        let now = app.now_playing().expect("the waiting song");
        assert_eq!(now.uri, "spotify:track:c1");
        assert!(!now.local);
        assert_eq!(now.device_name.as_deref(), Some("Galaxy S23"));
        assert_eq!(
            now.device_kind.as_deref(),
            Some("Smartphone"),
            "the phone's icon stays through the wait"
        );
        assert!(matches!(
            crate::ui::devices::device_icon(now.device_kind.as_deref().unwrap()),
            crate::theme::Icon::Smartphone
        ));
        app.backend.shutdown();
    }

    /// The desktop's controls take a position only with a new song or
    /// playback state, so they counted the wait as playing. A seek during
    /// the wait, and the start, reach them as jumps.
    #[test]
    fn the_desktop_controls_hear_where_a_waited_song_starts() {
        let ctx = egui::Context::default();
        let mut app = app("wait-media-seek");
        waiting_play(&mut app, &ctx);
        assert_eq!(
            media_jump(&app),
            Some(("spotify:track:c1".to_string(), 0)),
            "the click shows the song where it will start"
        );
        app.sync_media_controls(&ctx);
        assert!(!app.media_seek, "sent with the next update");
        app.apply(Action::Seek(42_000), &ctx);
        assert_eq!(
            media_jump(&app),
            Some(("spotify:track:c1".to_string(), 42_000)),
            "the seek moves the controls at once"
        );
        app.sync_media_controls(&ctx);
        assert!(!app.media_seek, "sent with the next update");
        songs_arrive(&mut app, "a", vec![song("a1")]);
        assert!(app.pending_discography_play.is_none());
        assert_eq!(
            app.queued_play.as_ref().map(|play| play.position_ms),
            Some(42_000)
        );
        assert_eq!(
            media_jump(&app),
            Some(("spotify:track:c1".to_string(), 42_000)),
            "the song starts now, from where it was shown, not where the controls counted to"
        );
        app.sync_media_controls(&ctx);
        // Every release is in by now, so this play starts at once.
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "b".into(),
                },
                uri: "spotify:track:b1".into(),
                index: 0,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_none());
        assert!(
            !app.media_seek,
            "a play that did not wait has nothing to correct"
        );
        app.backend.shutdown();
    }

    /// The jump the next sync sends the desktop's controls: to where the
    /// bar puts its song.
    fn media_jump(app: &App) -> Option<(String, u32)> {
        app.media_seek
            .then(|| app.now_playing())
            .flatten()
            .map(|now| (now.uri, now.position_ms))
    }

    /// A double-click on the song playing shows it from where the play will
    /// start it, and a wait dropped takes the bar back to where that song
    /// really is. The song and the playback state stay the same, so the
    /// desktop's controls follow only as jumps.
    #[test]
    fn the_desktop_controls_go_back_with_a_dropped_wait_on_the_song_playing() {
        let ctx = egui::Context::default();
        let mut app = app("wait-media-seek-back");
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:c1".into(),
            duration_ms: 200_000,
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.local.position_ms = 90_000;
        waiting_play(&mut app, &ctx);
        assert_eq!(
            media_jump(&app),
            Some(("spotify:track:c1".to_string(), 0)),
            "the click moves the controls to where the song will start"
        );
        app.sync_media_controls(&ctx);
        app.apply(Action::Seek(180_000), &ctx);
        assert_eq!(
            media_jump(&app),
            Some(("spotify:track:c1".to_string(), 180_000))
        );
        app.sync_media_controls(&ctx);
        app.apply(Action::Reload(Page::Discography(ARTIST.into())), &ctx);
        assert!(app.pending_discography_play.is_none());
        assert_eq!(
            media_jump(&app),
            Some(("spotify:track:c1".to_string(), 90_000)),
            "the controls go back to where the song plays"
        );
        app.backend.shutdown();
    }

    /// The session is saved every so often while music plays, and a waiting
    /// play shows its song as playing. That song has not started, so it
    /// must not become the point to resume from.
    #[test]
    fn a_cancelled_wait_leaves_the_resume_point_where_it_was() {
        let ctx = egui::Context::default();
        let mut app = app("wait-resume");
        app.resume_track = Some("spotify:track:x".into());
        app.resume_context = Some("spotify:playlist:p".into());
        app.resume_position_ms = 83_000;
        waiting_play(&mut app, &ctx);
        app.last_session_save = Instant::now() - SESSION_REFRESH - Duration::from_secs(1);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.background_frame(ui.ctx());
        });
        output.textures_delta.clear();
        assert!(
            app.last_session_save.elapsed() < SESSION_REFRESH,
            "the session was saved during the wait"
        );
        app.apply(Action::TogglePlay, &ctx);
        assert!(app.pending_discography_play.is_none());
        assert_eq!(app.resume_track.as_deref(), Some("spotify:track:x"));
        assert_eq!(app.resume_context.as_deref(), Some("spotify:playlist:p"));
        assert_eq!(app.resume_position_ms, 83_000);
        app.backend.shutdown();
    }

    /// While a play waits, the fetched queue still lists the album playing
    /// before the click, whose next songs the waiting play replaces. A
    /// queue saved meanwhile holds the waiting song and the songs the user
    /// queued, which stay ahead of the play.
    #[test]
    fn a_queue_saved_while_a_play_waits_leaves_out_the_old_next_up() {
        let ctx = egui::Context::default();
        let mut app = app("wait-save-queue");
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:o1".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.queue = Loadable::Loaded(Queue {
            currently_playing: Some(PlayableItem::Track(song("o1"))),
            queue: ["x", "o2", "o3"]
                .iter()
                .map(|id| PlayableItem::Track(song(id)))
                .collect(),
        });
        app.manual_queue = uris(&["x"]);
        waiting_play(&mut app, &ctx);
        assert_eq!(app.queue_playlist_uris(), uris(&["c1", "x"]));
        let saved: Vec<String> = app
            .saved_queue_rows()
            .iter()
            .map(|item| item.uri().to_string())
            .collect();
        assert_eq!(saved, uris(&["x"]), "Winamp's Save list keeps these");
        app.apply(Action::SaveQueueAsPlaylist, &ctx);
        assert!(
            app.actions.iter().any(|action| matches!(
                action,
                Action::CreatePlaylist { add_uris, public: false, .. }
                    if *add_uris == uris(&["c1", "x"])
            )),
            "{:?}",
            app.actions
        );
        assert!(
            app.pending_discography_play.is_some(),
            "saving leaves the play waiting"
        );
        // Pause cancels the wait: the old album plays on, and its next
        // songs count again.
        app.apply(Action::TogglePlay, &ctx);
        assert!(app.pending_discography_play.is_none());
        assert_eq!(app.queue_playlist_uris(), uris(&["o1", "x", "o2", "o3"]));
        app.backend.shutdown();
    }

    /// Right after launch, the queue on show is the last session's, its
    /// Playing next the songs queued then. Those come back only when the
    /// remembered song resumes, so a queue saved while a play of another
    /// song waits leaves them out.
    #[test]
    fn a_queue_saved_while_a_play_waits_after_launch_keeps_the_last_queue_for_its_song_only() {
        let ctx = egui::Context::default();
        for (remembered, saved) in [("o1", uris(&["c1"])), ("c1", uris(&["c1", "q1", "q2"]))] {
            let mut app = app("wait-save-restored-queue");
            app.resume_track = Some(format!("spotify:track:{remembered}"));
            app.resume_queue = uris(&["q1", "q2"]);
            app.queue = Loadable::Loaded(Queue {
                currently_playing: None,
                queue: ["q1", "q2", "o2"]
                    .iter()
                    .map(|id| PlayableItem::Track(song(id)))
                    .collect(),
            });
            waiting_play(&mut app, &ctx);
            assert_eq!(app.waiting_discography_song(), Some("spotify:track:c1"));
            assert_eq!(app.queue_playlist_uris(), saved, "remembered {remembered}");
            app.backend.shutdown();
        }
    }

    /// Lyrics opened while a play waits are the waiting song's, as the bar
    /// shows it. Pause cancels the wait, and the bar and the lyrics go back
    /// to the song still playing.
    #[test]
    fn lyrics_opened_during_a_wait_go_back_to_the_playing_song_when_it_is_cancelled() {
        let ctx = egui::Context::default();
        let mut app = app("wait-lyrics-cancel");
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:x".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        waiting_play(&mut app, &ctx);
        app.apply(Action::ToggleLyricsPanel, &ctx);
        assert_eq!(app.lyrics_uri.as_deref(), Some("spotify:track:c1"));
        app.apply(Action::TogglePlay, &ctx);
        assert!(app.pending_discography_play.is_none());
        assert_eq!(
            app.lyrics_uri.as_deref(),
            Some("spotify:track:x"),
            "a cancelled wait puts back the playing song's lyrics"
        );
        app.backend.shutdown();
    }

    /// With the lyrics already on show, a click that waits brings its
    /// song's lyrics along with the bar, and a newer click its own.
    #[test]
    fn the_lyrics_on_show_follow_the_song_a_play_waits_to_start() {
        let ctx = egui::Context::default();
        let mut app = app("wait-lyrics-follow");
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:x".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.apply(Action::ToggleLyricsPanel, &ctx);
        assert_eq!(app.lyrics_uri.as_deref(), Some("spotify:track:x"));
        waiting_play(&mut app, &ctx);
        assert_eq!(
            app.lyrics_uri.as_deref(),
            Some("spotify:track:c1"),
            "the panel shows the waiting song's lyrics, as the bar shows its song"
        );
        // A later pass of the same wait leaves a failed answer to Try
        // again rather than asking again on every pass.
        app.lyrics = Loadable::Failed("unreachable".into());
        app.resolve_discography_play();
        assert!(
            app.pending_discography_play.is_some(),
            "the play still waits"
        );
        assert!(matches!(app.lyrics, Loadable::Failed(_)));
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "c".into(),
                },
                uri: "spotify:track:c2".into(),
                index: 1,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_some(), "it waits for a too");
        assert_eq!(app.lyrics_uri.as_deref(), Some("spotify:track:c2"));
        app.backend.shutdown();
    }

    /// Another play replaces a wait, and the lyrics it showed go with it,
    /// even when that play starts the song already playing, so that no new
    /// song comes to ask for lyrics again.
    #[test]
    fn another_play_replacing_a_wait_puts_back_the_playing_song_lyrics() {
        let ctx = egui::Context::default();
        let mut app = app("wait-lyrics-replaced");
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:x".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        waiting_play(&mut app, &ctx);
        app.apply(Action::ToggleLyricsPanel, &ctx);
        assert_eq!(app.lyrics_uri.as_deref(), Some("spotify:track:c1"));
        app.apply(
            Action::PlayUris {
                uris: vec!["spotify:track:x".into()],
                index: 0,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_none());
        assert_eq!(app.lyrics_uri.as_deref(), Some("spotify:track:x"));
        app.backend.shutdown();
    }

    /// A newer click that does not wait replaces an older wait, and the
    /// lyrics that one showed go back to the song still playing, as for any
    /// play, until the new song starts.
    #[test]
    fn a_newer_click_that_does_not_wait_puts_back_the_playing_song_lyrics() {
        let ctx = egui::Context::default();
        let mut app = app("wait-lyrics-newer");
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:x".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        open(&mut app, &ctx, three_releases());
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        songs_arrive(&mut app, "a", vec![song("a1"), song("a2")]);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "c".into(),
                },
                uri: "spotify:track:c1".into(),
                index: 0,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_some(), "it waits for b");
        app.apply(Action::ToggleLyricsPanel, &ctx);
        assert_eq!(app.lyrics_uri.as_deref(), Some("spotify:track:c1"));
        // The last release has all it needs, so this play starts at once.
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "a".into(),
                },
                uri: "spotify:track:a1".into(),
                index: 0,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_none());
        assert!(app.queued_play.is_some(), "a starts");
        assert_eq!(app.lyrics_uri.as_deref(), Some("spotify:track:x"));
        app.backend.shutdown();
    }

    /// The song still playing while a play waits can give way to the next
    /// one. That song is the point to resume from, in the playlist it plays
    /// from, though only the waiting song is lit meanwhile.
    #[test]
    fn a_song_change_during_a_wait_resumes_in_the_playlist_still_playing() {
        // The playlist started just now, or long before the click.
        for (index, started) in [
            Duration::ZERO,
            ASSUMED_CONTEXT_HOLD + Duration::from_secs(1),
        ]
        .into_iter()
        .enumerate()
        {
            let ctx = egui::Context::default();
            let mut app = app(&format!("wait-song-change-{index}"));
            app.assumed_context = Some(AssumedContext {
                uri: "spotify:playlist:p".into(),
                shuffle: None,
                at: Instant::now() - started,
            });
            let mut playing = app.local.clone();
            playing.playback = Playback::Playing;
            playing.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:x".into(),
                duration_ms: 200_000,
                ..Default::default()
            });
            playing.track_sequence += 1;
            app.handle_local(playing);
            waiting_play(&mut app, &ctx);
            // Skipped from another device, say: the playlist moves on.
            let mut next = app.local.clone();
            next.track = Some(crate::player::LocalTrack {
                uri: "spotify:track:z".into(),
                duration_ms: 200_000,
                ..Default::default()
            });
            next.track_sequence += 1;
            app.handle_local(next);
            assert!(
                app.pending_discography_play.is_some(),
                "the play still waits"
            );
            assert_eq!(
                app.playing_context_uri(),
                None,
                "nothing else is lit meanwhile"
            );
            app.save_session();
            assert_eq!(app.resume_context.as_deref(), Some("spotify:playlist:p"));
            assert_eq!(app.resume_track.as_deref(), Some("spotify:track:z"));
            assert_eq!(app.resume_position_ms, 0);
            app.backend.shutdown();
        }
    }

    /// The same on a phone: a poll that finds the phone's playlist moved on
    /// makes its new song the point to resume from, in that playlist.
    #[test]
    fn a_song_change_on_a_phone_during_a_wait_resumes_in_its_playlist() {
        let ctx = egui::Context::default();
        let mut app = app("wait-song-change-phone");
        let phone = |song_id: &str| PlaybackState {
            device: Some(Device {
                id: Some("phone".into()),
                name: "Galaxy S23".into(),
                kind: "Smartphone".into(),
                ..Default::default()
            }),
            context: Some(crate::api::models::Context {
                uri: "spotify:playlist:p".into(),
                ..Default::default()
            }),
            is_playing: true,
            item: Some(PlayableItem::Track(song(song_id))),
            ..Default::default()
        };
        app.remote = Some(RemoteSnapshot {
            state: phone("x"),
            received_at: Instant::now(),
        });
        waiting_play(&mut app, &ctx);
        app.handle_api(ApiResponse::PlaybackState {
            seq: app.remote_poll_seq,
            result: Ok(Some(phone("z"))),
        });
        assert!(
            app.pending_discography_play.is_some(),
            "the play still waits"
        );
        assert_eq!(
            app.playing_context_uri(),
            None,
            "nothing else is lit meanwhile"
        );
        app.save_session();
        assert_eq!(app.resume_context.as_deref(), Some("spotify:playlist:p"));
        assert_eq!(app.resume_track.as_deref(), Some("spotify:track:z"));
        app.backend.shutdown();
    }

    /// What played before the click goes on during the wait. If it ended
    /// first, the old play would start the song the user queued next, and
    /// the start would then cut that song off.
    #[test]
    fn a_wait_never_outlasts_the_song_still_playing() {
        let ctx = egui::Context::default();
        let mut app = app("wait-song-end");
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:old".into(),
            duration_ms: 200_000,
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.local.position_ms = 150_000;
        app.local.position_at = Some(Instant::now());
        app.manual_queue = vec!["spotify:track:x".into()];
        waiting_play(&mut app, &ctx);
        app.settle_overdue_discography_play();
        assert!(
            app.pending_discography_play.is_some() && app.queued_play.is_none(),
            "fifty seconds are left"
        );
        app.local.playback = Playback::Paused;
        app.local.position_ms = 199_000;
        app.settle_overdue_discography_play();
        assert!(
            app.pending_discography_play.is_some(),
            "a paused song does not end"
        );
        app.local.playback = Playback::Playing;
        app.local.position_at = Some(Instant::now());
        app.local.repeat = RepeatMode::Track;
        app.settle_overdue_discography_play();
        assert!(
            app.pending_discography_play.is_some(),
            "a song on Repeat one starts over, and the queue stays where it is"
        );
        app.local.repeat = RepeatMode::Off;
        app.settle_overdue_discography_play();
        assert!(app.pending_discography_play.is_none());
        assert_eq!(played(&app), (uris(&["c1", "c2", "b1"]), Some(0)));
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("ends early"))
        );
        // The play's own song starts, not the queued one.
        let mut started = app.local.clone();
        started.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:c1".into(),
            ..Default::default()
        });
        started.track_sequence += 1;
        app.handle_local(started);
        assert_eq!(
            app.manual_queue,
            vec!["spotify:track:x".to_string()],
            "the song queued next is still next"
        );
        app.backend.shutdown();
    }

    /// A waiting play shows its song as starting in Now playing alone: it
    /// takes no queue row, so a queued copy of the song is an ordinary row
    /// further on. A play from a queue row still marks that row.
    #[test]
    fn a_waiting_play_leaves_queued_copies_of_its_song_alone() {
        let ctx = egui::Context::default();
        let mut app = app("queued-copy");
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:x".into(),
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.manual_queue = vec!["spotify:track:c1".into()];
        app.queue = Loadable::Loaded(Queue {
            currently_playing: Some(PlayableItem::Track(song("x"))),
            queue: vec![
                PlayableItem::Track(song("c1")),
                PlayableItem::Track(song("y")),
            ],
        });
        waiting_play(&mut app, &ctx);
        let now_playing = RowContext::Uris(std::sync::Arc::from(["spotify:track:c1".to_string()]));
        assert!(
            app.row_play_pending(&now_playing, "spotify:track:c1"),
            "the Now playing row shows the song starting"
        );
        assert!(
            !app.row_play_pending(&RowContext::Queue, "spotify:track:c1"),
            "the queued copy is an ordinary row"
        );
        assert!(app.play_pending("spotify:track:c1"));
        assert_eq!(
            app.queue.get().unwrap().queue[0].uri(),
            "spotify:track:c1",
            "the wait takes no queue row"
        );
        assert_eq!(app.manual_queue, vec!["spotify:track:c1".to_string()]);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Queue,
                uri: "spotify:track:y".into(),
                index: 1,
            },
            &ctx,
        );
        assert!(
            app.row_play_pending(&RowContext::Queue, "spotify:track:y"),
            "a play from a queue row spins until Spotify answers"
        );
        app.backend.shutdown();
    }

    /// Until the clicked release's songs arrive the play has no song to show,
    /// so the player bar still shows the paused song with its Play button:
    /// Play cancels the wait and resumes that song, as the button promises.
    #[test]
    fn play_before_a_release_play_shows_a_song_resumes_the_paused_one() {
        let ctx = egui::Context::default();
        let mut app = app("wait-unshown-play");
        app.resume_track = Some("spotify:track:x".into());
        app.resume_context = Some("spotify:playlist:p".into());
        app.resume_position_ms = 83_000;
        app.cache_song(song("x"));
        open(&mut app, &ctx, three_releases());
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        assert!(
            app.pending_discography_play
                .as_ref()
                .is_some_and(|play| play.shown.is_none()),
            "the release's songs have not arrived"
        );
        let bar = app.now_playing().expect("the paused song");
        assert_eq!(bar.uri, "spotify:track:x");
        assert!(!bar.playing && !app.believed_playing());
        app.apply(Action::TogglePlay, &ctx);
        assert!(
            app.pending_discography_play.is_none(),
            "the wait is cancelled"
        );
        let request = app.queued_play.as_ref().expect("the paused song resumes");
        assert_eq!(request.context_uri.as_deref(), Some("spotify:playlist:p"));
        assert_eq!(request.offset_uri.as_deref(), Some("spotify:track:x"));
        assert_eq!(request.position_ms, 83_000);
        load_all(&mut app);
        assert_eq!(
            app.queued_play
                .as_ref()
                .and_then(|request| request.offset_uri.as_deref()),
            Some("spotify:track:x"),
            "the cancelled release play never starts"
        );
        app.backend.shutdown();
    }

    /// The same for a song paused on a phone: Play resumes it there, and the
    /// release does not start over it once its songs arrive.
    #[test]
    fn play_before_a_release_play_shows_a_song_resumes_it_on_its_device() {
        let ctx = egui::Context::default();
        let mut app = app("wait-unshown-play-phone");
        app.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                device: Some(Device {
                    id: Some("phone".into()),
                    name: "Galaxy S23".into(),
                    kind: "Smartphone".into(),
                    ..Default::default()
                }),
                is_playing: false,
                item: Some(PlayableItem::Track(song("x"))),
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        assert_eq!(app.target(), Target::Remote(Some("phone".into())));
        open(&mut app, &ctx, three_releases());
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        assert!(
            app.pending_discography_play
                .as_ref()
                .is_some_and(|play| play.shown.is_none()),
            "the release's songs have not arrived"
        );
        app.apply(Action::TogglePlay, &ctx);
        assert!(
            app.pending_discography_play.is_none(),
            "the wait is cancelled"
        );
        let requests = app.backend.take_remote_play_requests();
        assert!(
            matches!(
                &requests[..],
                [ApiRequest::Remote {
                    action: RemoteAction::Play,
                    device_id: Some(device),
                    play: None,
                    ..
                }] if device == "phone"
            ),
            "the phone resumes its song: {requests:?}"
        );
        load_all(&mut app);
        assert!(
            app.backend.take_remote_play_requests().is_empty() && app.queued_play.is_none(),
            "the cancelled release play never starts"
        );
        app.backend.shutdown();
    }

    /// With no song from before either, the bar is empty and the release is
    /// the only play Play can mean: the wait goes on and the release starts.
    #[test]
    fn play_with_nothing_before_a_release_play_shows_a_song_lets_it_come() {
        let ctx = egui::Context::default();
        let mut app = app("wait-unshown-play-empty");
        open(&mut app, &ctx, three_releases());
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        assert!(app.now_playing().is_none(), "the bar has no song");
        app.apply(Action::TogglePlay, &ctx);
        assert!(
            app.pending_discography_play.is_some(),
            "the release still waits for its songs"
        );
        assert!(app.toasts.is_empty(), "nothing asks to pick something");
        load_all(&mut app);
        assert_eq!(
            played(&app),
            (uris(&["c1", "c2", "b1", "b2", "b3", "a1", "a2"]), Some(0))
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_rate_limit_during_a_wait_plays_what_came_and_says_so() {
        let ctx = egui::Context::default();
        let mut app = app("halted-wait");
        waiting_play(&mut app, &ctx);
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "a".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(played(&app), (uris(&["c1", "c2", "b1"]), Some(0)));
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("ends early"))
        );
        app.backend.shutdown();
    }

    /// A song playing on another device ends where its last report, carried
    /// on by the clock, puts it: the wait starts the play there before then.
    #[test]
    fn a_wait_starts_on_a_remote_device_before_its_song_ends() {
        let ctx = egui::Context::default();
        let mut app = app("wait-remote-song-end");
        let report = |progress_ms, is_playing| RemoteSnapshot {
            state: PlaybackState {
                device: Some(Device {
                    id: Some("phone".into()),
                    name: "Phone".into(),
                    ..Default::default()
                }),
                is_playing,
                progress_ms: Some(progress_ms),
                item: Some(PlayableItem::Track(song("old"))),
                ..Default::default()
            },
            received_at: Instant::now(),
        };
        app.remote = Some(report(150_000, true));
        waiting_play(&mut app, &ctx);
        app.settle_overdue_discography_play();
        assert!(
            app.pending_discography_play.is_some(),
            "fifty seconds are left"
        );
        app.remote = Some(report(199_000, false));
        app.settle_overdue_discography_play();
        assert!(
            app.pending_discography_play.is_some(),
            "a paused song does not end"
        );
        assert!(app.backend.take_remote_play_requests().is_empty());
        app.remote = Some(report(199_000, true));
        app.settle_overdue_discography_play();
        assert!(app.pending_discography_play.is_none());
        let requests = app.backend.take_remote_play_requests();
        assert!(
            matches!(
                requests.as_slice(),
                [ApiRequest::Remote { device_id: Some(device), play: Some(play), .. }]
                    if device == "phone"
                        && play.uris == uris(&["c1", "c2", "b1"])
                        && play.offset_position == Some(0)
            ),
            "{requests:?}"
        );
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("ends early"))
        );
        app.backend.shutdown();
    }

    /// A click in the last seconds of the song playing starts at once with
    /// the songs that have come, as a wait would outlast that song.
    #[test]
    fn a_click_as_the_playing_song_ends_starts_at_once() {
        let ctx = egui::Context::default();
        let mut app = app("click-song-end");
        app.local.track = Some(crate::player::LocalTrack {
            uri: "spotify:track:old".into(),
            duration_ms: 200_000,
            ..Default::default()
        });
        app.local.playback = Playback::Playing;
        app.local.position_ms = 199_000;
        app.local.position_at = Some(Instant::now());
        open(&mut app, &ctx, three_releases());
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        // A release whose own songs have not come plays on its own.
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "b".into(),
                start: None,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_none());
        let request = app.queued_play.as_ref().expect("a play request");
        assert_eq!(request.context_uri.as_deref(), Some("spotify:album:b"));
        assert!(request.uris.is_empty());
        app.toasts.clear();
        songs_arrive(&mut app, "b", vec![song("b1")]);
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "c".into(),
                },
                uri: "spotify:track:c1".into(),
                index: 0,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_none());
        assert_eq!(played(&app), (uris(&["c1", "c2", "b1"]), Some(0)));
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("ends early"))
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_play_over_five_hundred_songs_keeps_the_start_of_the_clicked_release() {
        let ctx = egui::Context::default();
        let mut app = app("cap");
        open(
            &mut app,
            &ctx,
            vec![release("old", "2020", 300), release("new", "2024", 300)],
        );
        songs_arrive(&mut app, "new", songs("n", 300));
        songs_arrive(&mut app, "old", songs("o", 300));
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "new".into(),
                },
                uri: "spotify:track:n3".into(),
                index: 3,
            },
            &ctx,
        );
        let (played, index) = played(&app);
        assert_eq!(played.len(), MAX_PLAYED_URIS);
        assert_eq!(
            played[0], "spotify:track:n0",
            "Previous reaches the release's start"
        );
        assert_eq!(index, Some(3));
        assert_eq!(
            played.last().map(String::as_str),
            Some("spotify:track:o199")
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_start_past_five_hundred_songs_plays_the_window_from_the_click() {
        let ctx = egui::Context::default();
        let mut app = app("cap-past");
        open(&mut app, &ctx, vec![release("long", "2024", 600)]);
        songs_arrive(&mut app, "long", songs("l", 600));
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "long".into(),
                },
                uri: "spotify:track:l550".into(),
                index: 550,
            },
            &ctx,
        );
        let (played, index) = played(&app);
        assert_eq!(
            played.first().map(String::as_str),
            Some("spotify:track:l550")
        );
        assert_eq!(played.len(), 50);
        assert_eq!(index, Some(0));
        app.backend.shutdown();
    }

    /// A play from song 551 of a 600-song release, before the 460 songs of
    /// the release after it have arrived.
    fn play_past_the_cap(app: &mut App, ctx: &egui::Context) {
        play_long_release_from(app, ctx, 550);
    }

    /// A play from row `row` of a 600-song release, before the 460 songs of
    /// the release after it have arrived.
    fn play_long_release_from(app: &mut App, ctx: &egui::Context, row: u32) {
        open(
            app,
            ctx,
            vec![release("next", "2020", 460), release("long", "2024", 600)],
        );
        songs_arrive(app, "long", songs("l", 600));
        app.apply(
            Action::PlayFromRow {
                context: RowContext::Discography {
                    artist_id: ARTIST.into(),
                    album_id: "long".into(),
                },
                uri: format!("spotify:track:l{row}"),
                index: row,
            },
            ctx,
        );
    }

    #[test]
    fn a_start_at_song_five_hundred_plays_at_once_from_its_release_start() {
        let ctx = egui::Context::default();
        let mut app = app("cap-edge-in");
        play_long_release_from(&mut app, &ctx, 499);
        assert!(app.pending_discography_play.is_none());
        let (played, index) = played(&app);
        assert_eq!(
            played,
            (0..500)
                .map(|n| format!("spotify:track:l{n}"))
                .collect::<Vec<_>>()
        );
        assert_eq!(index, Some(499));
        app.backend.shutdown();
    }

    #[test]
    fn a_start_at_song_five_hundred_and_one_waits_for_the_releases_after_it() {
        let ctx = egui::Context::default();
        let mut app = app("cap-edge-past");
        play_long_release_from(&mut app, &ctx, 500);
        assert!(
            app.queued_play.is_none(),
            "the songs after the click are waited for"
        );
        songs_arrive(&mut app, "next", songs("x", 460));
        let (played, index) = played(&app);
        assert_eq!(
            played,
            (500..600)
                .map(|n| format!("spotify:track:l{n}"))
                .chain((0..400).map(|n| format!("spotify:track:x{n}")))
                .collect::<Vec<_>>()
        );
        assert_eq!(index, Some(0));
        app.backend.shutdown();
    }

    #[test]
    fn a_start_past_five_hundred_songs_waits_for_the_releases_after_it() {
        let ctx = egui::Context::default();
        let mut app = app("cap-past-wait");
        play_past_the_cap(&mut app, &ctx);
        assert!(
            app.queued_play.is_none(),
            "the songs after the click are waited for"
        );
        assert!(app.pending_discography_play.is_some());
        assert_eq!(
            app.current_track_uri().as_deref(),
            Some("spotify:track:l550")
        );
        songs_arrive(&mut app, "next", songs("x", 460));
        let (played, index) = played(&app);
        assert_eq!(played.len(), MAX_PLAYED_URIS);
        assert_eq!(
            played.first().map(String::as_str),
            Some("spotify:track:l550")
        );
        assert_eq!(
            played.last().map(String::as_str),
            Some("spotify:track:x449")
        );
        assert_eq!(index, Some(0));
        app.backend.shutdown();
    }

    #[test]
    fn a_start_past_five_hundred_songs_says_when_its_queue_ends_early() {
        let ctx = egui::Context::default();
        let mut app = app("cap-past-early");
        play_past_the_cap(&mut app, &ctx);
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "next".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::RateLimited),
        });
        assert_eq!(
            played(&app),
            (
                (550..600)
                    .map(|n| format!("spotify:track:l{n}"))
                    .collect::<Vec<_>>(),
                Some(0)
            )
        );
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("ends early"))
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_waiting_play_lights_no_other_context() {
        let ctx = egui::Context::default();
        let mut app = app("context-light");
        app.assumed_context = Some(AssumedContext {
            uri: "spotify:playlist:before".into(),
            shuffle: None,
            at: Instant::now(),
        });
        waiting_play(&mut app, &ctx);
        assert_eq!(
            app.playing_context_uri(),
            None,
            "nothing else shows as playing"
        );
        songs_arrive(&mut app, "a", vec![song("a1")]);
        assert_eq!(
            app.playing_context_uri(),
            None,
            "a plain list names no context"
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_click_with_nowhere_to_play_still_replaces_a_waiting_play() {
        let ctx = egui::Context::default();
        let mut app = app("refused");
        waiting_play(&mut app, &ctx);
        app.local_ready = false;
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "b".into(),
                start: None,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_none());
        assert!(app.show_devices);
        app.backend.shutdown();
    }

    #[test]
    fn each_page_of_a_long_release_keeps_a_waiting_play_alive() {
        let ctx = egui::Context::default();
        let mut app = app("progress");
        open(
            &mut app,
            &ctx,
            vec![release("long", "2020", 120), release("new", "2024", 1)],
        );
        songs_arrive(&mut app, "new", vec![song("n1")]);
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "new".into(),
                start: None,
            },
            &ctx,
        );
        let generation = generation(&app);
        app.pending_discography_play.as_mut().unwrap().progress -= PLAY_STALL;
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "long".into(),
            offset: 0,
            generation,
            result: Ok(page(songs("p", 50), 0, 50, 120)),
        });
        app.settle_overdue_discography_play();
        assert!(
            app.pending_discography_play.is_some(),
            "a page arriving is progress, so the play keeps waiting"
        );
        app.backend.shutdown();
    }

    #[test]
    fn show_all_on_another_filter_forgets_where_the_old_list_was_scrolled() {
        let ctx = egui::Context::default();
        let mut app = app("in-view-reset");
        open(&mut app, &ctx, three_releases());
        app.apply(
            Action::DiscographyInView {
                artist_id: ARTIST.into(),
                first: 2,
                last: 2,
            },
            &ctx,
        );
        app.apply(
            Action::OpenDiscography {
                artist_id: ARTIST.into(),
                filter: DiscographyFilter::Singles,
            },
            &ctx,
        );
        assert_eq!(app.discography_pages[ARTIST].in_view, None);
        app.backend.shutdown();
    }

    #[test]
    fn a_newer_play_replaces_a_waiting_one() {
        let ctx = egui::Context::default();
        let mut app = app("newer");
        open(&mut app, &ctx, three_releases());
        songs_arrive(&mut app, "c", vec![song("c1")]);
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        app.apply(
            Action::PlayUris {
                uris: uris(&["elsewhere"]),
                index: 0,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_none());
        songs_arrive(&mut app, "b", vec![song("b1")]);
        songs_arrive(&mut app, "a", vec![song("a1")]);
        assert_eq!(played(&app).0, uris(&["elsewhere"]));
        app.backend.shutdown();
    }

    /// A waiting play takes its precedence over a play-search at the
    /// click, not when its songs have come.
    #[test]
    fn a_waiting_play_outranks_older_play_searches_and_yields_to_newer_ones() {
        let ctx = egui::Context::default();
        let mut app = app("play-search");
        let search = |app: &mut App, query: &str| {
            app.apply(
                Action::PlaySearch {
                    kind: "track".into(),
                    query: query.into(),
                },
                &ctx,
            );
            app.play_search_serial
        };
        let answer = |app: &mut App, serial: u64, uri: &str| {
            app.handle_api(ApiResponse::PlaySearch {
                query: "words".into(),
                serial,
                result: Ok(Some(uri.into())),
            });
            app.apply_actions(&ctx);
        };
        open(&mut app, &ctx, three_releases());
        let older = search(&mut app, "older");
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        answer(&mut app, older, "spotify:track:older");
        assert!(
            app.pending_discography_play.is_some(),
            "a search sent before the click never plays"
        );
        let newer = search(&mut app, "newer");
        load_all(&mut app);
        assert_eq!(
            played(&app).0,
            uris(&["c1", "c2", "b1", "b2", "b3", "a1", "a2"])
        );
        answer(&mut app, newer, "spotify:track:newer");
        assert_eq!(
            app.queued_play
                .as_ref()
                .and_then(|request| request.context_uri.as_deref()),
            Some("spotify:track:newer"),
            "a search sent during the wait still plays its match"
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_stalled_wait_plays_up_to_the_first_missing_release_and_says_so() {
        let ctx = egui::Context::default();
        let mut app = app("stall");
        open(
            &mut app,
            &ctx,
            vec![
                release("a", "2019", 1),
                release("b", "2020", 1),
                release("c", "2021", 1),
                release("d", "2022", 1),
            ],
        );
        songs_arrive(&mut app, "d", vec![song("d1")]);
        songs_arrive(&mut app, "c", vec![song("c1")]);
        // "b" never arrives; "a" does.
        songs_arrive(&mut app, "a", vec![song("a1")]);
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "d".into(),
                start: None,
            },
            &ctx,
        );
        assert!(app.queued_play.is_none());
        let play = app.pending_discography_play.as_mut().unwrap();
        play.progress -= PLAY_STALL;
        app.settle_overdue_discography_play();
        assert_eq!(
            played(&app),
            (uris(&["d1", "c1"]), Some(0)),
            "never past the release that did not come"
        );
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("ends early"))
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_release_whose_songs_never_came_plays_as_spotify_knows_it() {
        let ctx = egui::Context::default();
        let mut app = app("context");
        open(&mut app, &ctx, three_releases());
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "a".into(),
                start: None,
            },
            &ctx,
        );
        let play = app.pending_discography_play.as_mut().unwrap();
        play.clicked -= PLAY_WAIT;
        app.settle_overdue_discography_play();
        let request = app.queued_play.as_ref().expect("a play request");
        assert_eq!(request.context_uri.as_deref(), Some("spotify:album:a"));
        assert!(
            !app.toasts
                .iter()
                .any(|toast| toast.message.contains("ends early")),
            "the last release leaves nothing out"
        );
        app.backend.shutdown();
    }

    /// The overdue check of every pass runs a wait again only once it can
    /// end: a stall does not end it while the clicked release's own songs
    /// are on their way.
    #[test]
    fn a_stall_leaves_a_wait_for_the_clicked_release_itself_alone() {
        let ctx = egui::Context::default();
        let mut app = app("overdue-check");
        open(&mut app, &ctx, three_releases());
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        asked_songs(&app);
        let play = app.pending_discography_play.as_mut().unwrap();
        assert_eq!(play.keys, ["spotify:album:c"]);
        play.progress -= PLAY_STALL;
        // The wait writes the spinning keys on every pass it runs: cleared,
        // they show whether the check ran it again.
        play.keys.clear();
        assert!(
            !wait_over(
                &app.discography_pages[ARTIST],
                app.pending_discography_play.as_ref().unwrap()
            ),
            "c's own songs are still on their way"
        );
        app.settle_overdue_discography_play();
        let play = app
            .pending_discography_play
            .as_ref()
            .expect("the play still waits");
        assert!(play.keys.is_empty(), "the check left the wait alone");
        assert!(asked_songs(&app).is_empty());
        assert!(app.queued_play.is_none());
        app.pending_discography_play.as_mut().unwrap().clicked -= PLAY_WAIT;
        app.settle_overdue_discography_play();
        assert!(
            app.pending_discography_play.is_none(),
            "the longest wait is over"
        );
        assert_eq!(
            app.queued_play
                .as_ref()
                .and_then(|request| request.context_uri.as_deref()),
            Some("spotify:album:c")
        );
        app.backend.shutdown();
    }

    /// The newest release's songs stop at a rate limit, fail on their own,
    /// or never come: it plays alone, without the releases after it, and
    /// the play says so. A failure ends the wait at once.
    #[test]
    fn a_release_whose_songs_never_came_plays_alone_and_says_so() {
        fn fail(app: &mut App, error: crate::api::ApiError) {
            let generation = generation(app);
            app.handle_api(ApiResponse::DiscographyTracks {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                offset: 0,
                generation,
                result: Err(error),
            });
        }
        let ends: [fn(&mut App); 3] = [
            |app| fail(app, crate::api::ApiError::RateLimited),
            |app| {
                fail(
                    app,
                    crate::api::ApiError::Status {
                        status: 404,
                        message: "Not found".into(),
                    },
                )
            },
            |app| {
                app.pending_discography_play.as_mut().unwrap().clicked -= PLAY_WAIT;
                app.settle_overdue_discography_play();
            },
        ];
        for (case, end) in ends.into_iter().enumerate() {
            let ctx = egui::Context::default();
            let mut app = app(&format!("alone-{case}"));
            open(&mut app, &ctx, three_releases());
            songs_arrive(&mut app, "b", vec![song("b1")]);
            app.apply(
                Action::PlayDiscography {
                    artist_id: ARTIST.into(),
                    album_id: "c".into(),
                    start: None,
                },
                &ctx,
            );
            assert!(app.pending_discography_play.is_some(), "case {case}");
            end(&mut app);
            assert!(app.pending_discography_play.is_none(), "case {case}");
            let request = app.queued_play.as_ref().expect("a play request");
            assert_eq!(
                request.context_uri.as_deref(),
                Some("spotify:album:c"),
                "case {case}"
            );
            assert!(
                app.toasts
                    .iter()
                    .any(|toast| toast.message.contains("ends early")),
                "case {case}: b and a are left out"
            );
            app.backend.shutdown();
        }
    }

    /// The newest release fails once every release after it has loaded:
    /// nothing else is on its way, and the play still says it ends early.
    #[test]
    fn a_release_failing_after_the_rest_loaded_plays_alone_and_says_so() {
        let ctx = egui::Context::default();
        let mut app = app("alone-rest-loaded");
        open(&mut app, &ctx, three_releases());
        songs_arrive(&mut app, "b", vec![song("b1")]);
        songs_arrive(&mut app, "a", vec![song("a1")]);
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        let play = app
            .pending_discography_play
            .as_ref()
            .expect("the play waits for c");
        assert_eq!(
            waiting_for(&app.discography_pages[ARTIST], play),
            ["c"],
            "only c's own songs are on their way"
        );
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "c".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::Status {
                status: 404,
                message: "Not found".into(),
            }),
        });
        assert!(app.pending_discography_play.is_none());
        let request = app.queued_play.as_ref().expect("a play request");
        assert_eq!(request.context_uri.as_deref(), Some("spotify:album:c"));
        assert!(
            app.toasts
                .iter()
                .any(|toast| toast.message.contains("ends early")),
            "b and a are left out"
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_release_played_alone_says_nothing_when_the_rest_failed() {
        let ctx = egui::Context::default();
        let mut app = app("alone-rest-failed");
        open(&mut app, &ctx, three_releases());
        let generation = generation(&app);
        for album in ["b", "a"] {
            app.handle_api(ApiResponse::DiscographyTracks {
                artist_id: ARTIST.into(),
                album_id: album.into(),
                offset: 0,
                generation,
                result: Err(crate::api::ApiError::Status {
                    status: 404,
                    message: "Not found".into(),
                }),
            });
        }
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        app.pending_discography_play.as_mut().unwrap().clicked -= PLAY_WAIT;
        app.settle_overdue_discography_play();
        let request = app.queued_play.as_ref().expect("a play request");
        assert_eq!(request.context_uri.as_deref(), Some("spotify:album:c"));
        assert!(
            !app.toasts
                .iter()
                .any(|toast| toast.message.contains("ends early")),
            "the releases after it had no songs to add"
        );
        app.backend.shutdown();
    }

    /// A release whose songs never came plays alone without a word when
    /// that leaves nothing out: the releases after it have nothing
    /// playable, or it alone fills a play.
    #[test]
    fn a_release_played_alone_says_nothing_when_nothing_is_left_out() {
        fn unplayable(id: &str) -> Track {
            Track {
                is_playable: Some(false),
                ..song(id)
            }
        }
        let setups: [fn(&mut App, &egui::Context) -> &'static str; 2] = [
            |app, ctx| {
                open(app, ctx, three_releases());
                songs_arrive(app, "b", vec![unplayable("b1")]);
                songs_arrive(app, "a", vec![unplayable("a1")]);
                "c"
            },
            |app, ctx| {
                open(
                    app,
                    ctx,
                    vec![release("long", "2024", 600), release("next", "2020", 3)],
                );
                songs_arrive(app, "next", songs("n", 3));
                "long"
            },
        ];
        for (case, setup) in setups.into_iter().enumerate() {
            let ctx = egui::Context::default();
            let mut app = app(&format!("alone-nothing-left-out-{case}"));
            let clicked = setup(&mut app, &ctx);
            app.apply(
                Action::PlayDiscography {
                    artist_id: ARTIST.into(),
                    album_id: clicked.into(),
                    start: None,
                },
                &ctx,
            );
            app.pending_discography_play
                .as_mut()
                .unwrap_or_else(|| panic!("case {case}: the play waits"))
                .clicked -= PLAY_WAIT;
            app.settle_overdue_discography_play();
            let request = app.queued_play.as_ref().expect("a play request");
            assert_eq!(
                request.context_uri,
                Some(format!("spotify:album:{clicked}")),
                "case {case}"
            );
            assert!(
                !app.toasts
                    .iter()
                    .any(|toast| toast.message.contains("ends early")),
                "case {case}: nothing is left out"
            );
            app.backend.shutdown();
        }
    }

    #[test]
    fn nowhere_to_play_is_said_at_the_click() {
        let ctx = egui::Context::default();
        let mut app = app("nowhere");
        open(&mut app, &ctx, three_releases());
        app.local_ready = false;
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_none());
        assert!(app.show_devices, "the devices open at once");
        app.backend.shutdown();
    }

    #[test]
    fn a_waiting_play_keeps_its_page_from_eviction() {
        let ctx = egui::Context::default();
        let mut app = app("evict");
        open(&mut app, &ctx, three_releases());
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        for other in ["x", "y", "z", "w"] {
            app.apply(
                Action::OpenDiscography {
                    artist_id: other.into(),
                    filter: DiscographyFilter::All,
                },
                &ctx,
            );
        }
        assert!(app.discography_pages.contains_key(ARTIST));
        assert!(app.discography_pages.len() <= 4);
        app.backend.shutdown();
    }

    /// `count` releases r0 to r{count-1}, dated 2000 on, each of `songs`
    /// songs: the newest is the last.
    fn releases(count: usize, songs: u32) -> Vec<Album> {
        (0..count)
            .map(|index| release(&format!("r{index}"), &format!("{}", 2000 + index), songs))
            .collect()
    }

    /// The song lists asked for since the last call, in both lanes: the
    /// session batches by number with their album ids, and the Web API's
    /// album ids. It drains both.
    fn asked_lanes(app: &App) -> (Vec<(u64, Vec<String>)>, Vec<String>) {
        let mut batches = Vec::new();
        let mut web_api = Vec::new();
        for request in app.backend.take_discography_requests() {
            match request {
                ApiRequest::DiscographySongs { batch, albums, .. } => {
                    batches.push((batch, albums.into_iter().map(|album| album.id).collect()));
                }
                ApiRequest::DiscographyTracks { album_id, .. } => web_api.push(album_id),
                _ => {}
            }
        }
        (batches, web_api)
    }

    fn batch(number: u64, ids: &[&str]) -> (u64, Vec<String>) {
        (number, ids.iter().map(|id| id.to_string()).collect())
    }

    /// The session's answer for batch `batch`: each release with its songs.
    fn batch_arrives(app: &mut App, batch: u64, lists: Vec<(&str, Vec<Track>)>) {
        let generation = generation(app);
        app.handle_api(ApiResponse::DiscographySongs {
            artist_id: ARTIST.into(),
            batch,
            generation,
            songs: Some(
                lists
                    .into_iter()
                    .map(|(album, songs)| (album.to_string(), songs))
                    .collect(),
            ),
        });
    }

    /// No playback session of this account could read batch `batch`.
    fn batch_unread(app: &mut App, batch: u64) {
        let generation = generation(app);
        app.handle_api(ApiResponse::DiscographySongs {
            artist_id: ARTIST.into(),
            batch,
            generation,
            songs: None,
        });
    }

    fn all_three() -> Vec<(&'static str, Vec<Track>)> {
        vec![
            ("c", vec![song("c1"), song("c2")]),
            ("b", vec![song("b1"), song("b2"), song("b3")]),
            ("a", vec![song("a1"), song("a2")]),
        ]
    }

    fn batched_in(app: &App, album: &str) -> Option<u64> {
        app.discography_pages[ARTIST]
            .tracks
            .get(album)
            .and_then(ReleaseTracks::batch)
    }

    fn loaded(app: &App, album: &str) -> bool {
        matches!(
            app.discography_pages[ARTIST].tracks.get(album),
            Some(ReleaseTracks::Loaded(_))
        )
    }

    #[test]
    fn song_lists_come_over_the_session_in_a_batch_from_the_screen_down() {
        let ctx = egui::Context::default();
        let mut app = session_app("session-batch");
        open(&mut app, &ctx, three_releases());
        let requests = app.backend.take_discography_requests();
        let [
            ApiRequest::DiscographyAlbums { .. },
            ApiRequest::DiscographySongs {
                artist_id,
                albums,
                batch: 1,
                generation: asked_generation,
            },
        ] = requests.as_slice()
        else {
            panic!("the release list, then one batch: {requests:?}");
        };
        assert_eq!(artist_id, ARTIST);
        assert_eq!(*asked_generation, generation(&app));
        let named: Vec<(&str, &str, Option<u32>)> = albums
            .iter()
            .map(|album| (album.id.as_str(), album.uri.as_str(), album.total_tracks))
            .collect();
        assert_eq!(
            named,
            [
                ("c", "spotify:album:c", Some(2)),
                ("b", "spotify:album:b", Some(3)),
                ("a", "spotify:album:a", Some(2)),
            ]
        );
        for album in ["c", "b", "a"] {
            assert_eq!(batched_in(&app, album), Some(1), "{album}");
        }
        batch_arrives(&mut app, 1, all_three());
        for album in ["c", "b", "a"] {
            let songs = app.discography_pages[ARTIST].tracks[album]
                .songs()
                .expect("loaded");
            assert!(!songs.is_empty());
            assert!(
                songs
                    .iter()
                    .all(|song| song.album.as_ref().is_some_and(|it| it.id == album)),
                "{album}'s songs name it"
            );
        }
        assert_eq!(asked_lanes(&app), (vec![], vec![]));
        app.backend.shutdown();
    }

    #[test]
    fn a_batch_holds_at_most_fifty_releases_and_two_hundred_fifty_songs() {
        let sizes = |counts: &[Option<u32>]| -> Vec<Vec<Option<u32>>> {
            let releases = counts
                .iter()
                .enumerate()
                .map(|(index, count)| Album {
                    id: format!("r{index}"),
                    total_tracks: *count,
                    ..Album::default()
                })
                .collect();
            split_batches(releases)
                .into_iter()
                .map(|batch| batch.into_iter().map(|album| album.total_tracks).collect())
                .collect()
        };
        let singles = (0..60)
            .map(|index| Album {
                id: format!("r{index}"),
                total_tracks: Some(1),
                ..Album::default()
            })
            .collect();
        let batches = split_batches(singles);
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [50, 10]);
        let order: Vec<String> = batches
            .into_iter()
            .flatten()
            .map(|album| album.id)
            .collect();
        let expected: Vec<String> = (0..60).map(|index| format!("r{index}")).collect();
        assert_eq!(order, expected, "in order");
        assert_eq!(
            sizes(&[Some(200), Some(100), Some(600), Some(10)]),
            [
                vec![Some(200)],
                vec![Some(100)],
                vec![Some(600)],
                vec![Some(10)]
            ],
            "a release longer than a batch goes alone"
        );
        assert_eq!(
            sizes(&[Some(100), Some(100), Some(50), Some(1)]),
            [vec![Some(100), Some(100), Some(50)], vec![Some(1)]]
        );
        assert_eq!(sizes(&[None, None]), [vec![None, None]]);
    }

    #[test]
    fn one_batch_is_read_at_a_time_while_browsing_and_one_more_for_a_waiting_play() {
        let ctx = egui::Context::default();
        let mut app = session_app("batches-in-flight");
        open(&mut app, &ctx, releases(10, 100));
        assert_eq!(asked_lanes(&app), (vec![batch(1, &["r9", "r8"])], vec![]));
        let in_view = |app: &mut App| {
            app.apply(
                Action::DiscographyInView {
                    artist_id: ARTIST.into(),
                    first: 2,
                    last: 3,
                },
                &ctx,
            );
        };
        in_view(&mut app);
        assert_eq!(asked_lanes(&app), (vec![], vec![]), "one on its way");
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "r1".into(),
                start: None,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_some());
        assert_eq!(
            asked_lanes(&app),
            (vec![batch(2, &["r1", "r0"])], vec![]),
            "the play's own batch goes beside it"
        );
        in_view(&mut app);
        assert_eq!(asked_lanes(&app), (vec![], vec![]));
        batch_arrives(
            &mut app,
            1,
            vec![("r9", songs("r9-", 100)), ("r8", songs("r8-", 100))],
        );
        assert_eq!(asked_lanes(&app), (vec![batch(3, &["r7", "r6"])], vec![]));
        app.backend.shutdown();
    }

    #[test]
    fn a_batch_the_session_cannot_read_goes_to_the_web_api_two_at_a_time() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-unread");
        open(&mut app, &ctx, three_releases());
        assert_eq!(
            asked_lanes(&app),
            (vec![batch(1, &["c", "b", "a"])], vec![])
        );
        batch_unread(&mut app, 1);
        assert!(
            app.discography_pages[ARTIST]
                .tracks
                .values()
                .all(|tracks| tracks.batch().is_none())
        );
        assert!(app.discography_session_failed_at.is_some());
        assert_eq!(asked_lanes(&app), (vec![], vec!["c".into(), "b".into()]));
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        assert_eq!(asked_lanes(&app), (vec![], vec!["a".into()]));
        app.backend.shutdown();
    }

    #[test]
    fn songs_come_over_the_session_again_two_minutes_after_it_failed() {
        let ctx = egui::Context::default();
        let mut app = session_app("session-again");
        open(&mut app, &ctx, three_releases());
        asked_lanes(&app);
        batch_unread(&mut app, 1);
        assert_eq!(asked_lanes(&app), (vec![], vec!["c".into(), "b".into()]));
        app.discography_session_failed_at = Some(Instant::now() - SESSION_SONGS_AGAIN_AFTER);
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        assert_eq!(asked_lanes(&app), (vec![batch(2, &["a"])], vec![]));
        app.backend.shutdown();
    }

    #[test]
    fn local_playback_coming_up_sends_song_lists_back_to_the_session() {
        let ctx = egui::Context::default();
        let mut app = session_app("session-ready");
        open(&mut app, &ctx, three_releases());
        asked_lanes(&app);
        batch_unread(&mut app, 1);
        assert_eq!(asked_lanes(&app), (vec![], vec!["c".into(), "b".into()]));
        app.handle_backend_events(vec![Event::Playback(LocalPlayback::Ready {
            device_id: "dev".into(),
        })]);
        assert!(app.discography_session_failed_at.is_none());
        songs_arrive(&mut app, "c", vec![song("c1"), song("c2")]);
        assert_eq!(asked_lanes(&app), (vec![batch(2, &["a"])], vec![]));
        app.backend.shutdown();
    }

    #[test]
    fn without_local_playback_song_lists_come_from_the_web_api() {
        let ctx = egui::Context::default();
        let mut app = session_app("no-session");
        app.local_ready = false;
        open(&mut app, &ctx, three_releases());
        assert_eq!(asked_lanes(&app), (vec![], vec!["c".into(), "b".into()]));
        app.backend.shutdown();
    }

    #[test]
    fn a_release_the_session_lists_no_songs_for_is_read_from_the_web_api_alone() {
        let ctx = egui::Context::default();
        let mut app = session_app("session-empty");
        open(&mut app, &ctx, three_releases());
        asked_lanes(&app);
        batch_arrives(
            &mut app,
            1,
            vec![
                ("c", vec![song("c1"), song("c2")]),
                ("b", vec![]),
                ("a", vec![song("a1"), song("a2")]),
            ],
        );
        assert!(loaded(&app, "c") && loaded(&app, "a"));
        let page = &app.discography_pages[ARTIST];
        assert!(matches!(
            page.tracks.get("b"),
            Some(ReleaseTracks::Loading { .. })
        ));
        assert!(page.web_api_only.contains("b"));
        assert_eq!(asked_lanes(&app), (vec![], vec!["b".into()]));
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "b".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::Status {
                status: 404,
                message: "Not found".into(),
            }),
        });
        let page = &app.discography_pages[ARTIST];
        assert!(matches!(
            page.tracks.get("b"),
            Some(ReleaseTracks::Failed(_))
        ));
        assert!(page.halted.is_none());
        assert_eq!(asked_lanes(&app), (vec![], vec![]));
        app.apply(
            Action::RetryDiscography {
                artist_id: ARTIST.into(),
                album_id: Some("b".into()),
            },
            &ctx,
        );
        assert_eq!(asked_lanes(&app), (vec![], vec!["b".into()]));
        app.backend.shutdown();
    }

    #[test]
    fn a_batch_answer_for_a_reloaded_page_changes_nothing() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-stale");
        open(&mut app, &ctx, three_releases());
        asked_lanes(&app);
        let old = generation(&app);
        app.apply(Action::Reload(Page::Discography(ARTIST.into())), &ctx);
        let reloaded = generation(&app);
        assert_ne!(reloaded, old);
        app.handle_api(ApiResponse::DiscographyAlbums {
            artist_id: ARTIST.into(),
            groups: DiscographyFilter::All.groups().into(),
            offset: 0,
            generation: reloaded,
            result: Ok(page(three_releases(), 0, 50, 3)),
        });
        assert_eq!(
            asked_lanes(&app),
            (vec![batch(1, &["c", "b", "a"])], vec![])
        );
        // The old page's batch 1 shares the new one's number.
        app.handle_api(ApiResponse::DiscographySongs {
            artist_id: ARTIST.into(),
            batch: 1,
            generation: old,
            songs: Some(vec![("c".into(), vec![song("c1")])]),
        });
        assert_eq!(batched_in(&app, "c"), Some(1));
        app.backend.shutdown();
    }

    #[test]
    fn a_batch_whose_answer_never_comes_is_asked_again_and_a_late_answer_changes_nothing() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-lost");
        open(&mut app, &ctx, three_releases());
        assert_eq!(
            asked_lanes(&app),
            (vec![batch(1, &["c", "b", "a"])], vec![])
        );
        app.forget_lost_song_lists();
        assert_eq!(
            asked_lanes(&app),
            (vec![], vec![]),
            "a batch on its way is waited for"
        );
        age_unanswered(&mut app);
        app.forget_lost_song_lists();
        assert_eq!(
            asked_lanes(&app),
            (vec![batch(2, &["c", "b", "a"])], vec![])
        );
        batch_arrives(&mut app, 1, all_three());
        for album in ["c", "b", "a"] {
            assert_eq!(batched_in(&app, album), Some(2), "{album} waits for 2");
        }
        batch_unread(&mut app, 1);
        for album in ["c", "b", "a"] {
            assert_eq!(batched_in(&app, album), Some(2), "{album} waits for 2");
        }
        assert!(
            app.discography_session_failed_at.is_none(),
            "a late failure says nothing of the session now"
        );
        batch_arrives(&mut app, 2, all_three());
        assert!(["c", "b", "a"].iter().all(|album| loaded(&app, album)));
        app.backend.shutdown();
    }

    #[test]
    fn offline_no_batch_is_asked_until_the_network_is_back() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-offline");
        open(&mut app, &ctx, releases(5, 200));
        assert_eq!(asked_lanes(&app), (vec![batch(1, &["r4"])], vec![]));
        app.network_reachable = false;
        batch_arrives(&mut app, 1, vec![("r4", songs("r4-", 200))]);
        assert_eq!(asked_lanes(&app), (vec![], vec![]));
        app.handle_backend_events(vec![Event::NetworkStatus(true)]);
        assert_eq!(asked_lanes(&app), (vec![batch(2, &["r3"])], vec![]));
        app.backend.shutdown();
    }

    #[test]
    fn a_waiting_play_starts_once_its_batch_arrives() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-play");
        open(&mut app, &ctx, three_releases());
        asked_lanes(&app);
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        let play = app
            .pending_discography_play
            .as_ref()
            .expect("a waiting play");
        assert_eq!(play.keys, ["spotify:album:c"]);
        assert_eq!(asked_lanes(&app), (vec![], vec![]), "already on its way");
        batch_arrives(&mut app, 1, all_three());
        assert!(app.pending_discography_play.is_none());
        assert_eq!(
            played(&app).0,
            uris(&["c1", "c2", "b1", "b2", "b3", "a1", "a2"])
        );
        assert!(app.toasts.is_empty());
        app.backend.shutdown();
    }

    #[test]
    fn a_waiting_play_whose_batch_the_session_cannot_read_asks_the_web_api_for_its_releases() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-play-unread");
        open(&mut app, &ctx, three_releases());
        asked_lanes(&app);
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        batch_unread(&mut app, 1);
        assert_eq!(
            asked_lanes(&app),
            (vec![], vec!["c".into(), "b".into(), "a".into()])
        );
        assert!(app.pending_discography_play.is_some());
        load_all(&mut app);
        assert_eq!(
            played(&app).0,
            uris(&["c1", "c2", "b1", "b2", "b3", "a1", "a2"])
        );
        app.backend.shutdown();
    }

    #[test]
    fn a_batch_answer_keeps_a_waiting_play_alive_even_when_the_session_cannot_read_it() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-progress");
        open(&mut app, &ctx, releases(10, 100));
        assert_eq!(asked_lanes(&app), (vec![batch(1, &["r9", "r8"])], vec![]));
        batch_arrives(
            &mut app,
            1,
            vec![("r9", songs("r9-", 100)), ("r8", songs("r8-", 100))],
        );
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "r9".into(),
                start: None,
            },
            &ctx,
        );
        assert!(app.pending_discography_play.is_some());
        assert_eq!(
            asked_lanes(&app),
            (
                vec![batch(2, &["r7", "r6"]), batch(3, &["r5", "r4"])],
                vec![]
            )
        );
        app.pending_discography_play.as_mut().unwrap().progress -= PLAY_STALL;
        batch_arrives(
            &mut app,
            2,
            vec![("r7", songs("r7-", 100)), ("r6", songs("r6-", 100))],
        );
        app.settle_overdue_discography_play();
        assert!(
            app.pending_discography_play.is_some(),
            "a batch arriving is progress, so the play keeps waiting for r5"
        );
        app.pending_discography_play.as_mut().unwrap().progress -= PLAY_STALL;
        batch_unread(&mut app, 3);
        app.settle_overdue_discography_play();
        assert!(
            app.pending_discography_play.is_some(),
            "the Web API gets a fresh wait for the releases the session could not read"
        );
        assert!(asked_lanes(&app).1.contains(&"r5".to_string()));
        app.backend.shutdown();
    }

    #[test]
    fn a_release_the_session_lists_fewer_songs_for_is_read_from_the_web_api_alone() {
        let ctx = egui::Context::default();
        let mut app = session_app("session-short");
        open(&mut app, &ctx, three_releases());
        asked_lanes(&app);
        batch_arrives(
            &mut app,
            1,
            vec![
                ("c", vec![song("c1"), song("c2"), song("c3")]),
                ("b", vec![song("b1"), song("b3")]),
                ("a", vec![song("a1"), song("a2")]),
            ],
        );
        assert!(
            loaded(&app, "c") && loaded(&app, "a"),
            "only a list shorter than its release would move the rows below"
        );
        let page = &app.discography_pages[ARTIST];
        assert!(matches!(
            page.tracks.get("b"),
            Some(ReleaseTracks::Loading { .. })
        ));
        assert!(page.web_api_only.contains("b"));
        assert_eq!(asked_lanes(&app), (vec![], vec!["b".into()]));
        songs_arrive(
            &mut app,
            "b",
            vec![song("b1"), Track::default(), song("b3")],
        );
        assert_eq!(
            app.discography_pages[ARTIST].tracks["b"]
                .songs()
                .map(<[Track]>::len),
            Some(3),
            "the Web API keeps the missing song's place"
        );
        app.backend.shutdown();
    }

    #[test]
    fn an_expired_sign_in_keeps_releases_on_their_way_over_the_session() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-sign-in");
        open(&mut app, &ctx, releases(6, 100));
        assert_eq!(asked_lanes(&app), (vec![batch(1, &["r5", "r4"])], vec![]));
        batch_arrives(&mut app, 1, vec![("r5", vec![]), ("r4", songs("r4-", 100))]);
        assert_eq!(
            asked_lanes(&app),
            (vec![batch(2, &["r3", "r2"])], vec!["r5".into()])
        );
        let generation = generation(&app);
        app.handle_api(ApiResponse::DiscographyTracks {
            artist_id: ARTIST.into(),
            album_id: "r5".into(),
            offset: 0,
            generation,
            result: Err(crate::api::ApiError::SignInExpired {
                api_source: crate::api::ApiSource::Personal,
            }),
        });
        let page = &app.discography_pages[ARTIST];
        assert!(page.halted.is_some());
        assert!(!page.tracks.contains_key("r5"));
        assert_eq!(batched_in(&app, "r3"), Some(2));
        assert_eq!(batched_in(&app, "r2"), Some(2));
        batch_arrives(
            &mut app,
            2,
            vec![("r3", songs("r3-", 100)), ("r2", songs("r2-", 100))],
        );
        assert!(loaded(&app, "r3") && loaded(&app, "r2"));
        app.backend.shutdown();
    }

    #[test]
    fn a_batch_on_its_way_holds_no_web_api_slot() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-slots");
        open(&mut app, &ctx, releases(6, 100));
        assert_eq!(asked_lanes(&app), (vec![batch(1, &["r5", "r4"])], vec![]));
        app.discography_session_failed_at = Some(Instant::now());
        app.apply(
            Action::DiscographyInView {
                artist_id: ARTIST.into(),
                first: 2,
                last: 3,
            },
            &ctx,
        );
        assert_eq!(asked_lanes(&app), (vec![], vec!["r3".into(), "r2".into()]));
        app.backend.shutdown();
    }

    #[test]
    fn a_release_without_a_spotify_album_uri_is_read_from_the_web_api() {
        let ctx = egui::Context::default();
        let mut app = session_app("batch-uri");
        let mut releases = three_releases();
        releases[2].uri.clear();
        assert_eq!(releases[2].id, "c");
        open(&mut app, &ctx, releases);
        assert_eq!(
            asked_lanes(&app),
            (vec![batch(1, &["b", "a"])], vec!["c".into()])
        );
        app.backend.shutdown();
    }

    /// A device the Web API reports without an id, a Sonos, is somewhere to
    /// play, as it is for any other play: the click starts there rather
    /// than asking for a device.
    #[test]
    fn a_click_plays_on_a_device_reported_without_an_id() {
        let ctx = egui::Context::default();
        let mut app = app("no-id-device");
        open(&mut app, &ctx, three_releases());
        load_all(&mut app);
        app.local_ready = false;
        app.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                device: Some(Device {
                    id: None,
                    name: "Living room".into(),
                    ..Default::default()
                }),
                is_playing: true,
                item: Some(PlayableItem::Track(song("x"))),
                ..Default::default()
            },
            received_at: Instant::now(),
        });
        assert_eq!(app.target(), Target::Remote(None));
        app.apply(
            Action::PlayDiscography {
                artist_id: ARTIST.into(),
                album_id: "c".into(),
                start: None,
            },
            &ctx,
        );
        assert!(!app.show_devices, "no device is asked for");
        let requests = app.backend.take_remote_play_requests();
        assert!(
            matches!(
                requests.as_slice(),
                [ApiRequest::Remote {
                    device_id: None,
                    play: Some(_),
                    ..
                }]
            ),
            "{requests:?}"
        );
        app.backend.shutdown();
    }

    /// A verified Soloist named "owned", with a speaker beside it in the list.
    fn soloist_ready(app: &mut App) {
        use crate::soloist::{Status, Update};
        app.on_soloist(Update::Status(Status::Ready {
            name: "owned".into(),
            version: "test".into(),
            expires: u64::MAX,
        }));
        app.devices = vec![
            Device {
                id: Some("own".into()),
                name: "owned".into(),
                ..Default::default()
            },
            Device {
                id: Some("kitchen".into()),
                name: "Kitchen".into(),
                ..Default::default()
            },
        ];
        app.bind_soloist_device();
    }

    /// As [`app`], for a listener who finished Soloist's setup.
    fn soloist_app(name: &str) -> App {
        let mut app = app(name);
        app.user = Some(crate::api::models::User {
            id: "listener".into(),
            product: Some("premium".into()),
            ..Default::default()
        });
        app.soloist_onboarding = crate::settings::SoloistOnboarding::Complete;
        app
    }

    /// A play kept for a woken speaker can reach its start while Soloist
    /// needs the listener, and the start is dropped. The speaker goes with
    /// it: the next play starts where the listener has since pointed the app.
    #[test]
    fn a_dropped_start_leaves_its_speaker_out_of_the_next_play() {
        use crate::soloist::{Failure, Status, Update};
        let ctx = egui::Context::default();
        let mut app = soloist_app("dropped-start-speaker");
        app.settings.soloist_enabled = true;
        soloist_ready(&mut app);
        waiting_play(&mut app, &ctx);
        app.pending_discography_play.as_mut().unwrap().device = Some("kitchen".into());
        app.on_soloist(Update::Status(Status::Failed(Failure::KeyStore)));
        assert!(app.soloist_waiting() && !app.soloist_recovering());

        songs_arrive(&mut app, "a", vec![song("a1")]);
        assert!(app.pending_discography_play.is_none());
        assert!(app.queued_play.is_none(), "the start was dropped");
        assert_eq!(app.start_on, None);

        soloist_ready(&mut app);
        app.selected_device = Some("phone".into());
        app.backend.take_remote_play_requests();
        app.apply(
            Action::PlayUris {
                uris: vec!["spotify:track:later".into()],
                index: 0,
            },
            &ctx,
        );
        let requests = app.backend.take_remote_play_requests();
        assert!(
            matches!(
                requests.as_slice(),
                [ApiRequest::Remote { device_id: Some(id), .. }] if id == "phone"
            ),
            "{requests:?}"
        );
        app.backend.shutdown();
    }

    /// What `device` reports: the song from before the click, playing or not.
    fn song_on(app: &mut App, device: &str, is_playing: bool) {
        app.remote = Some(RemoteSnapshot {
            state: PlaybackState {
                device: Some(Device {
                    id: Some(device.into()),
                    name: device.into(),
                    ..Default::default()
                }),
                is_playing,
                item: Some(PlayableItem::Track(song("x"))),
                ..Default::default()
            },
            received_at: Instant::now(),
        });
    }

    fn transfers(app: &App) -> Vec<(String, bool)> {
        app.backend
            .take_transfer_requests()
            .into_iter()
            .map(|request| match request {
                ApiRequest::Transfer { device_id, play } => (device_id, play),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    /// Turning Soloist on moves what is heard. A waiting play shows its song
    /// as playing while nothing plays it, and that must not start the song
    /// the listener paused; one really playing moves across playing.
    #[test]
    fn turning_soloist_on_during_a_wait_moves_what_is_heard() {
        let ctx = egui::Context::default();
        for is_playing in [false, true] {
            let mut app = soloist_app(&format!("soloist-on-wait-{is_playing}"));
            song_on(&mut app, "phone", is_playing);
            waiting_play(&mut app, &ctx);
            assert!(app.now_playing().is_some_and(|now| now.playing));
            app.apply(Action::SetSoloist(true), &ctx);
            soloist_ready(&mut app);
            assert_eq!(transfers(&app), [("own".to_string(), is_playing)]);
            app.backend.shutdown();
        }
    }

    /// Soloist carries on after a restart only what it was really playing,
    /// not the song a waiting play shows over a paused one.
    #[test]
    fn a_soloist_restart_during_a_wait_resumes_only_what_was_heard() {
        use crate::soloist::{Failure, Status, Update};
        let ctx = egui::Context::default();
        for is_playing in [false, true] {
            let mut app = soloist_app(&format!("soloist-restart-wait-{is_playing}"));
            app.settings.soloist_enabled = true;
            soloist_ready(&mut app);
            app.selected_device = Some("own".into());
            song_on(&mut app, "own", is_playing);
            waiting_play(&mut app, &ctx);
            assert!(app.targets_own_soloist());
            assert!(app.now_playing().is_some_and(|now| now.playing));
            app.on_soloist(Update::Status(Status::Failed(Failure::Stopped)));
            assert_eq!(app.soloist_resume, is_playing);
            soloist_ready(&mut app);
            let expected: &[(String, bool)] = if is_playing {
                &[("own".to_string(), true)]
            } else {
                &[]
            };
            assert_eq!(transfers(&app), expected);
            app.backend.shutdown();
        }
    }
}
