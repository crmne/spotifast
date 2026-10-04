//! Radio pages: Spotify's station for a song, playlist, album, or artist.
//!
//! Spotify mixes a station afresh each time it is resolved, so the page
//! keeps the songs it was given and its Play button plays exactly those,
//! with the station as their context so the queue names the radio.

use super::*;

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
    /// already has the song's details, so cache them first.
    pub fn open_song_radio(&mut self, uri: &str, track: &Track) {
        if let Some(id) = util::uri_id(uri) {
            self.track_cache
                .entry(id.to_owned())
                .or_insert_with(|| track.clone());
            self.track_used.insert(id.to_owned(), Instant::now());
        }
        self.actions
            .push(Action::Open(Page::Radio(uri.to_string())));
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
