//! Discography pages: every release of an artist with its songs, opened by
//! Show all on the artist page.
//!
//! A page keeps lists of its own rather than the artist page's, which loads
//! releases a page at a time and must not grow behind the user's back. The
//! release list loads whole, because only the whole list can be ordered by
//! date. Song lists then load a release or two at a time while the page is
//! open: the releases on screen first, then those after them as far as a
//! play from the screen could reach. A song played here plays the page from
//! that song to its end; when some of the songs after it have not arrived,
//! the play waits for them, showing the song as starting meanwhile.

use super::*;

/// Song lists in flight at once while the user reads the page: enough to
/// keep ahead of scrolling, few enough to leave playback control room on a
/// rate-limited app.
const TRACKS_IN_FLIGHT: usize = 2;
/// Song lists in flight while a play waits for them.
const TRACKS_IN_FLIGHT_FOR_PLAY: usize = 4;
/// A waiting play that has seen no song list arrive for this long starts
/// with the songs it has.
const PLAY_STALL: Duration = Duration::from_secs(10);
/// The longest a play waits, however its song lists are coming along.
const PLAY_WAIT: Duration = Duration::from_secs(30);
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

    /// Asks for the next song lists: first those a waiting play needs, then,
    /// while the page is open, the releases on screen and those after them
    /// as far as a play from the screen could reach.
    pub(super) fn pump_discography(&mut self, artist_id: &str) {
        let open = self.discography_open(artist_id);
        let reachable = self.network_reachable;
        let Some(page) = self.discography_pages.get(artist_id) else {
            return;
        };
        if page.halted.is_some() {
            return;
        }
        let mut wanted: Vec<String> = Vec::new();
        let mut limit = TRACKS_IN_FLIGHT;
        if let Some(play) = self
            .pending_discography_play
            .as_ref()
            .filter(|play| play.artist_id == artist_id && play.generation == page.generation)
        {
            wanted.extend(waiting_for(page, play));
            limit = TRACKS_IN_FLIGHT_FOR_PLAY;
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
        let in_flight = page
            .tracks
            .values()
            .filter(|tracks| !tracks.resolved())
            .count();
        let generation = page.generation;
        let mut free = limit.saturating_sub(in_flight);
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
            if free > 0 && reachable {
                free -= 1;
                ask.push(id);
            }
        }
        let Some(page) = self.discography_pages.get_mut(artist_id) else {
            return;
        };
        for (id, mut songs) in ready {
            if let Some(album) = page.release(&id).cloned() {
                fill_album(&mut songs, &album);
            }
            page.tracks.insert(id, ReleaseTracks::Loaded(songs));
        }
        for id in &ask {
            page.tracks.insert(
                id.clone(),
                ReleaseTracks::Loading {
                    songs: Vec::new(),
                    next: 0,
                },
            );
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
        let Some(ReleaseTracks::Loading { songs, next }) = page.tracks.get_mut(&album_id) else {
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
                *next = following;
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
                page.halted = Some(error);
            }
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
        let start_resolved = page
            .tracks
            .get(&play.releases[0])
            .is_some_and(ReleaseTracks::resolved);
        // The song from before the click plays on meanwhile; once it ends,
        // its play moves on to a song the start would cut off.
        let give_up = page.halted.is_some()
            || play.clicked.elapsed() >= PLAY_WAIT
            || (start_resolved && play.progress.elapsed() >= PLAY_STALL)
            || self.playing_song_ending();
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
                self.follow_shown_lyrics();
            }
            self.pump_discography(&artist_id);
            return;
        }
        let list = play_list(page, &play);
        let album_uri = page
            .release(&play.releases[0])
            .map(|album| album.uri.clone());
        // A newer click that does not wait replaces an older play still
        // waiting, and the lyrics of the song that one showed go with it.
        if self
            .pending_discography_play
            .take()
            .is_some_and(|old| old.shown.is_some() && old.shown != play.shown)
        {
            self.follow_shown_lyrics();
        }
        let Some((songs, index)) = list else {
            // The release's songs never came: play it as Spotify knows it.
            if let Some(uri) = album_uri.filter(|_| play.start.is_none()) {
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
            self.toast(gettext(
                self.locale,
                // Translators: a play from an artist's discography page started
                // before every song after the chosen one had loaded.
                "Some songs didn't load in time, so the queue ends early",
            ));
        }
        if let Some(song) = songs.get(index) {
            self.cache_song(song.clone());
        }
        let mut uris: Vec<String> = songs.into_iter().map(|song| song.uri).collect();
        // The list starts with the clicked release's first song; a long one
        // loses songs from its end, so Previous, Shuffle and Repeat keep the
        // start of that release. Only a start past the limit itself takes
        // the usual window from the clicked song.
        let (uris, index) = if index < MAX_PLAYED_URIS {
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
    pub(super) fn settle_overdue_discography_play(&mut self) {
        let Some(play) = &self.pending_discography_play else {
            return;
        };
        let overdue = play.progress.elapsed() >= PLAY_STALL || play.clicked.elapsed() >= PLAY_WAIT;
        if overdue || self.playing_song_ending() {
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
    pub(super) fn resolve_discography_play(&mut self) {
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
            now.device_name = actual.device_name;
            now.repeat = actual.repeat;
            now.volume_percent = actual.volume_percent;
            now.can_control = actual.can_control;
            now.can_set_volume = actual.can_set_volume;
        }
        Some(now)
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
    list.items.retain(|album| seen.insert(album.id.clone()));
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
        Some(ReleaseTracks::Loading { .. }) | None => {
            album.total_tracks.unwrap_or(1).max(1) as usize
        }
    }
}

/// Gives album-less songs, as Spotify lists a release's songs, the release
/// they belong to, for their rows, menus and the player bar.
fn fill_album(songs: &mut [Track], album: &Album) {
    let stub = Album {
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
    };
    for song in songs.iter_mut().filter(|song| song.album.is_none()) {
        song.album = Some(stub.clone());
    }
}

/// The releases a play needs that have not arrived: from the clicked one
/// on, in the order shown at the click, as far as the songs a play can hold
/// reach.
fn waiting_for(page: &DiscographyPage, play: &PendingDiscographyPlay) -> Vec<String> {
    let mut waiting = Vec::new();
    let mut reach = 0usize;
    for id in &play.releases {
        if reach >= MAX_PLAYED_URIS {
            break;
        }
        if !page.tracks.get(id).is_some_and(ReleaseTracks::resolved) {
            waiting.push(id.clone());
        }
        reach += match page.release(id) {
            Some(album) => song_count(page, album),
            None => 1,
        };
    }
    waiting
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
    /// and a play waits in `queued_play` for this computer's player.
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
        app.backend.shutdown();
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
}
