//! Radio pages: Spotify's station for a song, playlist, album, or artist.
//!
//! Spotify mixes a station afresh each time it is resolved, so the page
//! keeps the songs it was given and its Play button plays exactly those,
//! with the station as their context so the queue names the radio.

use super::*;

/// A radio slower than this is not waited for: the song plays alone.
const SONG_RADIO_WAIT: Duration = Duration::from_secs(2);

/// A lone song asked of a remote device, held a moment for its radio.
pub(super) struct SongRadio {
    pub(super) seed: String,
    request: PlayRequest,
    device_id: Option<String>,
    shuffle: bool,
    pub(super) asked: Instant,
}

impl App {
    /// "`<Name>` Radio" for a seed whose name is known.
    pub fn radio_name(&self, seed: &str) -> Option<String> {
        let id = util::uri_id(seed)?;
        let name = match util::uri_kind(seed)? {
            "track" => self.track_cache.get(id).map(|track| track.name.clone()),
            "playlist" => self
                .playlist_pages
                .get(id)
                .and_then(|page| page.playlist.get())
                .or_else(|| self.known_playlist(id))
                .map(|playlist| playlist.name.clone()),
            "album" => self
                .album_pages
                .get(id)
                .and_then(|page| page.album.get())
                .or_else(|| self.known_album(id))
                .map(|album| album.name.clone()),
            "artist" => self
                .artist_pages
                .get(id)
                .and_then(|page| page.artist.get())
                .or_else(|| self.known_artist(id))
                .map(|artist| artist.name.clone()),
            _ => None,
        }?;
        Some(
            // Translators: Keep {track} unchanged. It is the name of the song,
            // playlist, album, or artist the radio is based on, not translated.
            gettext(self.locale, "{track} Radio").replace("{track}", &name),
        )
    }

    /// The seed's artwork, for the radio page's cover.
    pub fn radio_images(&self, seed: &str) -> Vec<crate::api::models::Image> {
        let Some(id) = util::uri_id(seed) else {
            return Vec::new();
        };
        let images = match util::uri_kind(seed) {
            Some("track") => self
                .track_cache
                .get(id)
                .and_then(|track| track.album.as_ref())
                .map(|album| album.images.clone()),
            Some("playlist") => self
                .playlist_pages
                .get(id)
                .and_then(|page| page.playlist.get())
                .or_else(|| self.known_playlist(id))
                .map(|playlist| playlist.images.clone()),
            Some("album") => self
                .album_pages
                .get(id)
                .and_then(|page| page.album.get())
                .or_else(|| self.known_album(id))
                .map(|album| album.images.clone()),
            Some("artist") => self
                .artist_pages
                .get(id)
                .and_then(|page| page.artist.get())
                .or_else(|| self.known_artist(id))
                .map(|artist| artist.images.clone()),
            _ => None,
        };
        images.unwrap_or_default()
    }

    /// Opens the radio of a song shown in a list. The page takes its name
    /// and cover from the song cache, which holds only songs that played or
    /// came back from a radio, so a song from Liked Songs, a playlist or a
    /// search opened as an unnamed "Radio" with no cover (#644). The row
    /// already has the song's details, so cache them first. A cached entry
    /// is kept, but takes the row's album when it has no artwork of its own:
    /// an album's song list comes without the album.
    pub(crate) fn open_song_radio(&mut self, uri: &str, track: &Track) {
        if let Some(id) = util::uri_id(uri) {
            let cached = self
                .track_cache
                .entry(id.to_owned())
                .or_insert_with(|| track.clone());
            let has_art = |t: &Track| t.album.as_ref().is_some_and(|a| !a.images.is_empty());
            if !has_art(cached) && has_art(track) {
                cached.album = track.album.clone();
            }
            self.track_used.insert(id.to_owned(), Instant::now());
        }
        self.open(Page::Radio(uri.to_string()));
    }

    /// Asks for the seed's songs unless the page already has or awaits them.
    pub(super) fn load_radio(&mut self, seed: &str) {
        let name = self.radio_name(seed);
        let images = self.radio_images(seed);
        let page = self.radio_pages.entry(seed.to_string()).or_default();
        if name.is_some() {
            page.name = name;
        }
        if !images.is_empty() {
            page.images = images;
        }
        if !page.songs.needs_load() {
            return;
        }
        self.load_generation = self.load_generation.wrapping_add(1);
        page.generation = self.load_generation;
        page.songs = Loadable::Loading;
        self.backend.send(Command::Radio {
            seed: seed.to_string(),
            generation: page.generation,
        });
    }

    /// Asks for a new mix while the page keeps showing its songs. False
    /// when there are no songs to keep, so the page loads afresh.
    pub(super) fn refresh_radio(&mut self, seed: &str) -> bool {
        let Some(page) = self
            .radio_pages
            .get_mut(seed)
            .filter(|page| page.songs.get().is_some())
        else {
            return false;
        };
        if !page.refreshing {
            self.load_generation = self.load_generation.wrapping_add(1);
            page.generation = self.load_generation;
            page.refreshing = true;
            self.backend.send(Command::Radio {
                seed: seed.to_string(),
                generation: page.generation,
            });
        }
        true
    }

    pub(super) fn receive_radio(
        &mut self,
        seed: &str,
        generation: u64,
        result: Result<Vec<Track>, String>,
    ) {
        let Some(page) = self
            .radio_pages
            .get_mut(seed)
            .filter(|page| page.generation == generation)
        else {
            return;
        };
        let refreshing = std::mem::take(&mut page.refreshing);
        match result {
            Ok(songs) => {
                let uris = songs.iter().map(|track| track.uri.clone()).collect();
                for track in &songs {
                    if let Some(id) = &track.id {
                        self.track_cache
                            .entry(id.clone())
                            .or_insert_with(|| track.clone());
                    }
                }
                page.songs = Loadable::Loaded(songs);
                // The table's cached rows follow the new songs.
                self.load_generation = self.load_generation.wrapping_add(1);
                page.generation = self.load_generation;
                self.request_contains(uris);
            }
            // A failed refresh keeps the mix on screen and says why.
            Err(error) if refreshing => self.toast(error),
            Err(error) => page.songs = Loadable::Failed(error),
        }
        // A song held for this radio goes now, followed by it or alone.
        if let Some(wait) = &self.song_radio
            && wait.seed == seed
        {
            self.send_song_radio();
        }
    }

    /// Plays `request` on a remote device. A lone song would stop there,
    /// as a Sonos does, so with Autoplay on its radio follows it, as it does
    /// in Spotify's apps; the song waits a moment for the radio.
    pub(super) fn play_remote(
        &mut self,
        device_id: Option<String>,
        request: PlayRequest,
        shuffle: bool,
    ) {
        let Some(seed) = self.lone_song(&request) else {
            self.send_remote_play(device_id, request, shuffle);
            return;
        };
        if let Some(followed) = self.with_radio(&seed, &request) {
            self.send_remote_play(device_id, followed, shuffle);
            return;
        }
        self.load_radio(&seed);
        self.song_radio = Some(SongRadio {
            seed,
            request,
            device_id,
            shuffle,
            asked: Instant::now(),
        });
    }

    /// The song `request` plays on its own, when its radio may follow it:
    /// Autoplay is on, Repeat is off, and the session can find the radio.
    /// Soloist is left out: its own play command can start a lone song as
    /// a context of its own, which Spotify's Autoplay follows.
    fn lone_song(&self, request: &PlayRequest) -> Option<String> {
        // A song can also be its own context, as a search match is; the
        // song its offset names, when it names one, is the one that plays.
        let uri = match (request.context_uri.as_deref(), request.uris.as_slice()) {
            (None, [uri]) => uri.as_str(),
            (Some(context), []) if util::uri_kind(context) == Some("track") => {
                request.offset_uri.as_deref().unwrap_or(context)
            }
            _ => return None,
        };
        let repeat = self.now_playing().map(|now| now.repeat).unwrap_or_default();
        (self.settings.autoplay
            && self.local_ready
            && !self.targets_own_soloist()
            && repeat == RepeatMode::Off
            && util::uri_kind(uri) == Some("track")
            && util::station_uri(uri).is_some())
        .then(|| uri.to_owned())
    }

    /// The song followed by its radio, once the radio has come.
    fn with_radio(&self, seed: &str, request: &PlayRequest) -> Option<PlayRequest> {
        let songs = self.radio_pages.get(seed)?.songs.get()?;
        let mut uris = vec![seed.to_string()];
        for song in songs {
            if !uris.contains(&song.uri) {
                uris.push(song.uri.clone());
            }
        }
        // A list starting at the song, whether it was asked as a list or
        // as its own context.
        Some(PlayRequest {
            context_uri: None,
            uris,
            offset_uri: None,
            offset_position: Some(0),
            ..request.clone()
        })
    }

    /// Sends the song held for its radio, with the radio if it has come.
    pub(super) fn send_song_radio(&mut self) {
        let Some(wait) = self.song_radio.take() else {
            return;
        };
        // The song is asked of the device only now, so only a poll from now
        // on can confirm it, and the player bar holds it from now.
        if let Some(intent) = &mut self.intent_track
            && intent.uri == wait.seed
            && let TrackConfirmation::Remote { after_poll, .. } = &mut intent.confirmation
        {
            *after_poll = self.remote_poll_seq;
            intent.at = Instant::now();
        }
        let request = self
            .with_radio(&wait.seed, &wait.request)
            .unwrap_or(wait.request);
        self.send_remote_play(wait.device_id, request, wait.shuffle);
    }

    /// Lets the held song go alone once its radio is late. Returns how long
    /// it may still wait.
    pub(super) fn send_song_radio_when_due(&mut self) -> Option<Duration> {
        let waited = self.song_radio.as_ref()?.asked.elapsed();
        if waited < SONG_RADIO_WAIT {
            return Some(SONG_RADIO_WAIT - waited);
        }
        self.send_song_radio();
        None
    }

    /// Saves the radio's songs, as shown, to a new playlist named after it.
    pub(super) fn save_radio(&mut self, seed: &str) {
        let Some(songs) = self.radio_pages.get(seed).and_then(|page| page.songs.get()) else {
            return;
        };
        let add_uris: Vec<String> = songs.iter().map(|track| track.uri.clone()).collect();
        if add_uris.is_empty() {
            return;
        }
        let name = self
            .radio_name(seed)
            .or_else(|| self.radio_pages.get(seed)?.name.clone())
            .unwrap_or_else(|| gettext(self.locale, "Radio").into_owned());
        self.actions.push(Action::CreatePlaylist {
            name,
            public: false,
            add_uris,
        });
    }
}
