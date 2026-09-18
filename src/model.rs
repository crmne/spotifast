//! UI state, loaded data, and pending actions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::api::models::*;

/// One table row: the playable, when it was added, who added it.
pub type TableItem = (PlayableItem, Option<String>, Option<String>);

/// Cached track-table rows for one page.
///
/// Lives on the app, not in egui temp data, so it dies with page eviction,
/// sign-out, and `reset_data`. A generation token stops a recreated page
/// from reusing a stale copy that still has the old revision number.
pub struct TableRowsCache {
    pub generation: u64,
    pub items_revision: u64,
    pub user_names_revision: u64,
    pub items: Arc<[TableItem]>,
}

impl TableRowsCache {
    /// Retained heap for this cache: nested track/episode metadata, not just
    /// the top-level URI and title.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .items
                .iter()
                .map(|(item, added, by)| {
                    playable_retained_bytes(item)
                        + added.as_ref().map(String::len).unwrap_or(0)
                        + by.as_ref().map(String::len).unwrap_or(0)
                })
                .sum::<usize>()
    }
}

fn playable_retained_bytes(item: &PlayableItem) -> usize {
    match item {
        PlayableItem::Track(track) => track_retained_bytes(track),
        PlayableItem::Episode(episode) => episode_retained_bytes(episode),
    }
}

fn artist_ref_retained_bytes(artist: &ArtistRef) -> usize {
    artist.name.len()
        + artist.id.as_ref().map(String::len).unwrap_or(0)
        + artist.uri.as_ref().map(String::len).unwrap_or(0)
}

fn image_retained_bytes(images: &[Image]) -> usize {
    images.iter().map(|image| image.url.len()).sum()
}

fn album_retained_bytes(album: &Album) -> usize {
    album.id.len()
        + album.name.len()
        + album.uri.len()
        + album.album_type.as_ref().map(String::len).unwrap_or(0)
        + album.release_date.as_ref().map(String::len).unwrap_or(0)
        + image_retained_bytes(&album.images)
        + album
            .artists
            .iter()
            .map(artist_ref_retained_bytes)
            .sum::<usize>()
}

fn track_retained_bytes(track: &Track) -> usize {
    std::mem::size_of::<Track>()
        + track.name.len()
        + track.uri.len()
        + track.id.as_ref().map(String::len).unwrap_or(0)
        + track
            .artists
            .iter()
            .map(artist_ref_retained_bytes)
            .sum::<usize>()
        + track.album.as_ref().map(album_retained_bytes).unwrap_or(0)
}

fn episode_retained_bytes(episode: &Episode) -> usize {
    std::mem::size_of::<Episode>()
        + episode.id.len()
        + episode.name.len()
        + episode.uri.len()
        + episode.description.len()
        + image_retained_bytes(&episode.images)
}

/// Every screen the central panel can show.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Home,
    TopSongs,
    Search,
    LikedSongs,
    Albums,
    Artists,
    Podcasts,
    Episodes,
    Playlist(String),
    Album(String),
    Artist(String),
    Show(String),
    Queue,
    Settings,
}

impl Page {
    pub fn encode(&self) -> String {
        match self {
            Page::Home => "home".into(),
            Page::TopSongs => "top-songs".into(),
            Page::Search => "search".into(),
            Page::LikedSongs => "liked".into(),
            Page::Albums => "albums".into(),
            Page::Artists => "artists".into(),
            Page::Podcasts => "podcasts".into(),
            Page::Episodes => "episodes".into(),
            Page::Playlist(id) => format!("playlist:{id}"),
            Page::Album(id) => format!("album:{id}"),
            Page::Artist(id) => format!("artist:{id}"),
            Page::Show(id) => format!("show:{id}"),
            Page::Queue => "queue".into(),
            Page::Settings => "settings".into(),
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        Some(match text {
            "home" => Page::Home,
            "top-songs" => Page::TopSongs,
            "search" => Page::Search,
            "liked" => Page::LikedSongs,
            "albums" => Page::Albums,
            "artists" => Page::Artists,
            "podcasts" => Page::Podcasts,
            "episodes" => Page::Episodes,
            "queue" => Page::Queue,
            "settings" => Page::Settings,
            other => {
                let (kind, id) = other.split_once(':')?;
                match kind {
                    "playlist" => Page::Playlist(id.into()),
                    "album" => Page::Album(id.into()),
                    "artist" => Page::Artist(id.into()),
                    "show" => Page::Show(id.into()),
                    _ => return None,
                }
            }
        })
    }

    /// Opens whatever a Spotify URI points at.
    pub fn from_uri(uri: &str) -> Option<Self> {
        let mut parts = uri.split(':');
        let _ = parts.next()?;
        let kind = parts.next()?;
        let id = parts.next()?.to_string();
        Some(match kind {
            "playlist" => Page::Playlist(id),
            "album" => Page::Album(id),
            "artist" => Page::Artist(id),
            "show" => Page::Show(id),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QueueTab {
    #[default]
    Queue,
    Recents,
}

impl QueueTab {
    pub fn encode(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Recents => "recents",
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        match text {
            "queue" => Some(Self::Queue),
            "recents" => Some(Self::Recents),
            // Backward compatibility with the old tab name.
            "recently_played" => Some(Self::Recents),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Loadable<T> {
    #[default]
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn get(&self) -> Option<&T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }

    pub fn get_mut(&mut self) -> Option<&mut T> {
        match self {
            Loadable::Loaded(value) => Some(value),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Loadable::Loading)
    }

    pub fn needs_load(&self) -> bool {
        matches!(self, Loadable::NotLoaded | Loadable::Failed(_))
    }

    pub fn from_result<E: std::fmt::Display>(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Loadable::Loaded(value),
            Err(error) => Loadable::Failed(error.to_string()),
        }
    }

    /// Keeps an already loaded value when a refresh fails.
    pub fn refresh<E: std::fmt::Display>(&mut self, result: Result<T, E>) {
        if result.is_ok() || self.get().is_none() {
            *self = Self::from_result(result);
        }
    }
}

/// An offset-paginated list that loads on demand as the user scrolls.
#[derive(Clone, Debug)]
pub struct PagedList<T> {
    pub items: Vec<T>,
    /// Spotify offset of the first item. Nonzero for a directly opened page.
    pub base_offset: u32,
    pub windows: std::collections::BTreeMap<u32, Vec<T>>,
    pub window_request: Option<u32>,
    pub total: Option<u32>,
    pub next_offset: Option<u32>,
    pub loading: bool,
    pub error: Option<String>,
    pub loaded_once: bool,
    pub revision: u64,
}

impl<T> Default for PagedList<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            base_offset: 0,
            windows: Default::default(),
            window_request: None,
            total: None,
            next_offset: Some(0),
            loading: false,
            error: None,
            loaded_once: false,
            revision: 0,
        }
    }
}

impl<T> PagedList<T> {
    /// Select a cached window, or reserve a direct request for the visible row.
    /// Adjacent windows merge on arrival, so a viewport can cross page edges.
    pub fn window_at(&mut self, position: u32, size: u32) -> Option<u32> {
        if self.loading
            || self.error.is_some()
            || size == 0
            || self.total.is_none_or(|total| position >= total)
            || (self.base_offset..self.base_offset.saturating_add(self.items.len() as u32))
                .contains(&position)
        {
            return None;
        }
        let cached = self
            .windows
            .range(..=position)
            .next_back()
            .filter(|(start, items)| position < start.saturating_add(items.len() as u32))
            .map(|(start, _)| *start);
        if let Some(start) = cached {
            self.save_window();
            self.items = self.windows.remove(&start).unwrap();
            self.base_offset = start;
            self.loaded_once = true;
            self.join_windows();
            self.revision = self.revision.wrapping_add(1);
            return None;
        }
        let offset = position / size * size;
        let end = self.base_offset.saturating_add(self.items.len() as u32);
        // Adjacent reads extend the visible window; only a distant jump swaps it.
        if offset > end || offset.saturating_add(size) < self.base_offset {
            self.save_window();
            self.base_offset = offset;
            self.loaded_once = false;
            self.next_offset = Some(offset);
            self.revision = self.revision.wrapping_add(1);
        }
        self.window_request = Some(offset);
        self.loading = true;
        Some(offset)
    }

    fn save_window(&mut self) {
        if !self.items.is_empty() {
            self.windows
                .insert(self.base_offset, std::mem::take(&mut self.items));
        }
    }

    /// Edits or refreshes invalidate other windows' server positions.
    pub fn clear_windows(&mut self) {
        self.windows.clear();
        self.window_request = None;
    }

    fn join_windows(&mut self) {
        // The incoming page wins overlaps; retain cached rows on either side.
        loop {
            let end = self.base_offset.saturating_add(self.items.len() as u32);
            let adjacent = self
                .windows
                .iter()
                .find(|(start, items)| {
                    **start <= end && start.saturating_add(items.len() as u32) >= self.base_offset
                })
                .map(|(start, _)| *start);
            let Some(start) = adjacent else {
                break;
            };
            let mut other = self.windows.remove(&start).unwrap();
            let other_end = start.saturating_add(other.len() as u32);
            if start < self.base_offset {
                let tail = if other_end > end {
                    other.split_off((end - start) as usize)
                } else {
                    Vec::new()
                };
                other.truncate((self.base_offset - start) as usize);
                other.append(&mut self.items);
                other.extend(tail);
                self.items = other;
                self.base_offset = start;
            } else if other_end > end {
                self.items
                    .extend(other.into_iter().skip((end - start) as usize));
            }
        }
        self.next_offset = Some(self.base_offset.saturating_add(self.items.len() as u32))
            .filter(|end| self.total.is_some_and(|total| *end < total));
    }

    pub fn reset(&mut self) {
        *self = Self {
            revision: self.revision.wrapping_add(1),
            ..Default::default()
        };
    }

    pub fn can_load_more(&self) -> bool {
        !self.loading && (self.window_request.is_some() || self.next_offset.is_some())
    }

    pub fn is_complete(&self) -> bool {
        self.loaded_once && self.base_offset == 0 && self.next_offset.is_none()
    }

    pub fn absorb(&mut self, offset: u32, page: Page_<T>) {
        let window = self.window_request.take().is_some();
        let next_offset = page.next_offset();
        if window {
            if self.total.is_some_and(|total| total != page.total) {
                self.clear_windows();
                self.items.clear();
            } else {
                self.save_window();
            }
            self.base_offset = offset;
            self.items = page.items;
        } else {
            if offset == 0 {
                self.clear_windows();
                self.items.clear();
                self.base_offset = 0;
            } else if !self.loaded_once {
                self.items.clear();
                self.base_offset = offset;
            }
            let relative = offset.saturating_sub(self.base_offset) as usize;
            if relative < self.items.len() {
                self.items.truncate(relative);
            }
            self.items.extend(page.items);
        }
        self.total = Some(page.total);
        self.next_offset = next_offset;
        if window || !self.windows.is_empty() {
            self.join_windows();
        }
        self.loading = false;
        self.error = None;
        self.loaded_once = true;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&T) -> bool,
    {
        self.items.retain(f);
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn reorder(&mut self, from: usize, to: usize) {
        if from < self.items.len() && to <= self.items.len() {
            let item = self.items.remove(from);
            let insert_at = if to > from { to - 1 } else { to };
            self.items.insert(insert_at.min(self.items.len()), item);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// Adopt a disk prefix without replacing a distant viewport or its request.
    pub fn adopt_cached_prefix(&mut self, items: Vec<T>, total: u32, next_offset: Option<u32>) {
        if self.base_offset == 0 && self.window_request.is_none() {
            self.restore_cached(items, total, next_offset);
        } else {
            if self.base_offset == 0 {
                self.items = items;
                self.next_offset = next_offset;
                self.loaded_once = true;
            } else {
                self.windows.insert(0, items);
            }
            self.total = Some(total);
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn restore_cached(&mut self, items: Vec<T>, total: u32, next_offset: Option<u32>) {
        self.clear_windows();
        self.items = items;
        self.base_offset = 0;
        self.total = Some(total);
        self.next_offset = next_offset;
        self.loading = false;
        self.loaded_once = true;
        self.error = None;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn fail(&mut self, error: String) {
        self.loading = false;
        self.error = Some(error);
        self.loaded_once = true;
    }

    pub fn reset_at(&mut self, offset: u32) {
        self.reset();
        self.base_offset = offset;
        self.next_offset = Some(offset);
    }
}

type Page_<T> = crate::api::models::Page<T>;

/// Selected track-table rows for batch actions.
///
/// Selection belongs to one page and clears when sorting, filtering, or paging
/// changes the row order.
#[derive(Clone, Debug, Default)]
pub struct RowSelection {
    pub rows: std::collections::BTreeSet<usize>,
    /// Row used as the anchor for shift-click ranges.
    pub anchor: Option<usize>,
}

/// Selection behavior for a row click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPick {
    /// Select only this row.
    Only,
    /// Toggle this row.
    Toggle,
    /// Everything from the anchor to here.
    Range,
}

/// A cursor-paginated list (followed artists).
#[derive(Clone, Debug)]
pub struct CursorList<T> {
    pub items: Vec<T>,
    pub after: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    pub loaded_once: bool,
    pub complete: bool,
}

impl<T> Default for CursorList<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            after: None,
            loading: false,
            error: None,
            loaded_once: false,
            complete: false,
        }
    }
}

impl<T> CursorList<T> {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn can_load_more(&self) -> bool {
        !self.loading && !self.complete
    }
}

#[derive(Default)]
pub struct Library {
    pub playlists: Loadable<Vec<Playlist>>,
    pub playlists_next: Option<u32>,
    /// The later playlist page on its way. A reload forgets it, so a page
    /// asked for before the reload is not taken into the new list.
    pub playlists_asked: Option<u32>,
    pub liked: PagedList<SavedTrack>,
    pub albums: PagedList<SavedAlbum>,
    pub artists: CursorList<Artist>,
    pub shows: PagedList<SavedShow>,
    pub episodes: PagedList<SavedEpisode>,
    pub filter: String,
}

#[derive(Default)]
pub struct HomeData {
    pub recently_played: Loadable<Vec<PlayHistory>>,
    pub top_artists: Loadable<Vec<Artist>>,
    /// The 20-track preview shown on Home.
    pub top_tracks: Loadable<Vec<Track>>,
    /// The separately loaded, complete ranking shown by the Top Songs page.
    pub top_songs: Loadable<Vec<Track>>,
    pub top_songs_loading: bool,
    pub top_songs_complete: bool,
    pub recommendations: Loadable<Vec<Track>>,
    pub discover: HashMap<String, Loadable<Vec<Playlist>>>,
    pub discover_pending: HashMap<String, Loadable<Vec<Playlist>>>,
    pub generation: u64,
    pub top_songs_generation: u64,
    pub requested: bool,
    pub loaded_at: Option<Instant>,
}

pub const DISCOVER_TERMS: &[&str] = &["Discover Weekly", "Release Radar", "Daily Mix", "daylist"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Artists,
    Albums,
    Playlists,
    Podcasts,
    Episodes,
}

impl SearchFilter {
    pub const ALL: [SearchFilter; 7] = [
        Self::All,
        Self::Songs,
        Self::Artists,
        Self::Albums,
        Self::Playlists,
        Self::Podcasts,
        Self::Episodes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Songs => "Songs",
            Self::Artists => "Artists",
            Self::Albums => "Albums",
            Self::Playlists => "Playlists",
            Self::Podcasts => "Podcasts",
            Self::Episodes => "Episodes",
        }
    }
}

#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub committed: String,
    pub serial: u64,
    pub results: Loadable<SearchResults>,
    pub results_serial: u64,
    pub playlists: Option<(u64, crate::api::models::Page<Playlist>)>,
    pub catalogue_pending: bool,
    pub playlists_pending: bool,
    pub error: Option<String>,
    pub filter: SearchFilter,
    pub typed_at: Option<Instant>,
    pub focus_requested: bool,
}

#[derive(Default)]
pub struct PlaylistPage {
    pub generation: u64,
    /// Generation that produced the rows, which may lag during refresh.
    pub items_generation: u64,
    pub playlist: Loadable<Playlist>,
    pub items: PagedList<PlaylistItem>,
    pub filter: String,
    /// Contributor IDs from loaded pages and a sample of the final page.
    pub contributors: std::collections::BTreeSet<String>,
    /// Whether contributors were sampled from the final page.
    pub tail_checked: bool,
    /// Whether this generation's disk cache read has finished.
    pub cache_checked: bool,
    /// The Spotify offset through which this snapshot is saved on disk.
    pub cache_saved_through: Option<u32>,
    /// End of the prefix restored for this generation. An initial response
    /// below it is stale and must not replace the longer cached prefix.
    pub cache_restored_through: Option<u32>,
    /// Items read from disk, waiting for the live snapshot to confirm.
    pub pending_cache: Option<PlaylistCache>,
    /// One-based position entered in the direct page control.
    pub jump_position: u32,
    pub scroll_to: Option<u32>,
    /// Songs added here that may sit beyond the loaded prefix. They are known
    /// members immediately, even before Spotify's next read catches up.
    pub local_additions: std::collections::BTreeSet<String>,
    /// Snapshot returned by the latest successful write. A lagging metadata
    /// read must not replace it with the snapshot from before that write.
    pub optimistic_snapshot: Option<String>,
    /// Writes still awaiting a result. Keep their optimistic rows in memory
    /// even when navigation moves beyond the usual page-cache limit.
    pub pending_writes: usize,
    /// A manual refresh waiting for pending writes and their snapshot to be
    /// confirmed before requesting replacement rows.
    pub refresh_after_write: bool,
    /// Number of immediate metadata reads made while Spotify still reported
    /// the pre-write snapshot.
    pub snapshot_rechecks: u8,
}

/// A contiguous playlist prefix on disk, valid for exactly one snapshot.
#[derive(Clone, Debug)]
pub struct PlaylistCache {
    pub snapshot: String,
    pub items: Vec<PlaylistItem>,
    pub total: u32,
    pub next_offset: Option<u32>,
}

#[derive(Default)]
pub struct AlbumPage {
    pub generation: u64,
    pub album: Loadable<Album>,
    pub tracks: PagedList<Track>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiscographyFilter {
    #[default]
    All,
    Albums,
    Singles,
    AppearsOn,
}

impl DiscographyFilter {
    pub const ALL: [DiscographyFilter; 4] =
        [Self::All, Self::Albums, Self::Singles, Self::AppearsOn];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Albums => "Albums",
            Self::Singles => "Singles & EPs",
            Self::AppearsOn => "Appears On",
        }
    }

    pub fn groups(self) -> &'static str {
        match self {
            Self::All => "album,single,compilation",
            Self::Albums => "album",
            Self::Singles => "single",
            Self::AppearsOn => "appears_on",
        }
    }
}

#[derive(Default)]
pub struct ArtistPage {
    pub artist: Loadable<Artist>,
    pub top_tracks: Loadable<Vec<Track>>,
    pub albums: HashMap<String, PagedList<Album>>,
    pub related: Loadable<Vec<Artist>>,
    pub filter: DiscographyFilter,
    pub show_all_top: bool,
}

#[derive(Default)]
pub struct ShowPage {
    pub show: Loadable<Show>,
    pub episodes: PagedList<Episode>,
}

/// A table's sort, chosen by clicking a column heading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TableSort {
    pub column: SortColumn,
    pub ascending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SortColumn {
    Title,
    Album,
    Added,
    Duration,
    AddedBy,
    /// The list's own order, for playing it reversed from the # heading.
    Index,
}

/// Playback and action context for a track row.
#[derive(Clone, Debug, PartialEq)]
pub enum RowContext {
    /// A Spotify context (playlist, album) that can be played from an offset.
    Context {
        uri: String,
        /// The playlist id when the user owns it, enabling removal.
        editable_playlist: Option<(String, Option<String>)>,
    },
    /// A loose list of tracks, played as a queue of URIs.
    Uris(Arc<[String]>),
    /// A Next up row. Playing it consumes that row and all rows before it.
    Queue,
    /// A sorted or filtered context view that plays the displayed rows.
    View {
        uris: Arc<[String]>,
        context_uri: String,
    },
}

/// Track data held during a drag.
#[derive(Clone, Debug)]
pub struct DragTrack {
    pub title: String,
    /// Cover art for the drag preview.
    pub image: Option<String>,
    /// Full row data, so dropping can update an open playlist immediately.
    /// A picked table row carries the whole selection in table order.
    pub items: Vec<PlayableItem>,
    /// Source playlist ID and row index for moves within an editable playlist.
    pub from: Option<(String, u32)>,
}

/// Where the playing songs come from, as the queue's header names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayingFrom {
    pub name: String,
    /// The page that opens on click. A song radio has none.
    pub page: Option<Page>,
}

/// Sidebar entry held during a drag.
#[derive(Clone, Debug)]
pub struct DragEntry {
    pub uri: String,
    pub title: String,
    pub image: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Dialog {
    CreatePlaylist {
        name: String,
        public: bool,
        add_uris: Vec<String>,
    },
    EditPlaylist {
        cover: crate::playlist_cover::Draft,
        id: String,
        name: String,
        description: String,
        /// `None` while nothing has said whether the playlist is public;
        /// left untouched on save unless the switch is used.
        public: Option<bool>,
    },
    ConfirmDeletePlaylist {
        id: String,
        name: String,
        owned: bool,
    },
    ConfirmPlaylistDuplicates {
        playlist_id: String,
        playlist_name: String,
        items: Vec<PlayableItem>,
        position: Option<u32>,
        duplicate_uris: Vec<String>,
    },
    Shortcuts,
    /// The signed-in account is not Premium, so nothing will play.
    PremiumNeeded,
    /// Introduce personal Spotify apps to eligible listeners once.
    PersonalAppIntro,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub created: Instant,
}

/// Actions emitted while drawing and applied afterward to avoid borrow conflicts.
#[derive(Clone, Debug)]
pub enum Action {
    Open(Page),
    OpenUri(String),
    /// A Spotify link from outside the app: its page opens and the window
    /// comes forward, once the account is signed in.
    OpenLink(String),
    Back,
    Forward,
    PlayContext {
        uri: String,
        offset_uri: Option<String>,
        offset_index: Option<u32>,
    },
    PlayUris {
        uris: Vec<String>,
        index: u32,
    },
    PlayFromRow {
        context: RowContext,
        uri: String,
        index: u32,
    },
    /// Spotify's station seeded by this song.
    PlayTrackRadio(String),
    ShufflePlay(String),
    TogglePlay,
    Next,
    Previous,
    Seek(u32),
    SeekBy(i64),
    SetVolume(u8),
    /// Preview volume locally during a drag; send it to Spotify on release.
    PreviewVolume(u8),
    VolumeBy(i8),
    ToggleMute,
    ToggleShuffle,
    CycleRepeat,
    SetShuffle(bool),
    SetRepeat(crate::player::RepeatMode),
    AddToQueue {
        uri: String,
        label: String,
    },
    ToggleSaved(String),
    /// Queue several songs in order and show one notification.
    QueueMany {
        songs: Vec<(String, String)>,
    },
    /// Set saved state for several songs explicitly.
    SetSavedMany {
        uris: Vec<String>,
        saved: bool,
    },
    AddToPlaylist {
        playlist_id: String,
        playlist_name: String,
        items: Vec<PlayableItem>,
    },
    /// Copy dragged songs into an open playlist at an absolute position.
    InsertInPlaylist {
        playlist_id: String,
        position: u32,
        items: Vec<PlayableItem>,
    },
    ConfirmAddToPlaylist {
        playlist_id: String,
        playlist_name: String,
        items: Vec<PlayableItem>,
        position: Option<u32>,
    },
    RemoveFromPlaylist {
        playlist_id: String,
        uris: Vec<String>,
    },
    MoveInPlaylist {
        playlist_id: String,
        from: u32,
        to: u32,
    },
    ShowDialog(Dialog),
    CloseDialog,
    CreatePlaylist {
        name: String,
        public: bool,
        add_uris: Vec<String>,
    },
    ChoosePlaylistCover(String),
    UploadPlaylistCover(String),
    UpdatePlaylist {
        id: String,
        name: String,
        description: String,
        public: Option<bool>,
    },
    DeletePlaylist(String),
    Transfer(String),
    /// Send the account to a receiver found on the local network.
    ActivateReceiver(Box<crate::zeroconf::Receiver>),
    RefreshDevices,
    /// Empty Next up of its queued songs, keeping the context's own.
    ClearQueue,
    /// Save the current and upcoming queue as a playlist.
    SaveQueueAsPlaylist,
    RefreshQueue,
    CopyLink(String),
    /// Open a web page in the browser.
    OpenUrl(String),
    OpenInSpotify(String),
    Search(String),
    ForgetSearch(String),
    SetSearchFilter(SearchFilter),
    FocusSearch,
    LoadMore(Page),
    LoadWindow {
        page: Page,
        position: u32,
    },
    RetryWindow(Page),
    JumpToPlaylistPosition {
        id: String,
        position: u32,
    },
    LoadMoreRecents,
    ReloadRecents,
    SetQueueTab(QueueTab),
    LoadMoreArtistAlbums(String),
    SetDiscographyFilter {
        artist_id: String,
        filter: DiscographyFilter,
    },
    ToggleShowAllTop(String),
    Reload(Page),
    SignIn,
    CancelSignIn,
    SignOut,
    /// Add, replace, or remove the optional personal Web API app.
    ConfigurePersonalWebApp,
    OpenPersonalAppSetup,
    ToggleSidebar,
    ToggleQueuePanel,
    ToggleLyricsPanel,
    SetLyricsFullscreen(bool),
    LyricsLineShown(Option<usize>),
    FollowLyrics,
    PauseLyricsFollow,
    RetryLyrics,
    ToggleLyricsMotion,
    ToggleDevicesPopup,
    /// Ask GitHub for the latest release and report the result to the user.
    CheckForUpdates,
    ShowUpdate,
    DownloadUpdate,
    InstallUpdate,
    SettingsChanged,
    SetTheme(crate::settings::ThemeChoice),
    OpenThemesFolder,
    SetCustomTheme(String),
    ReloadThemes,
    SetLibrarySort {
        shelf: crate::settings::LibraryShelf,
        sort: crate::settings::LibrarySort,
    },
    ArrangeLibrary {
        pinned: Vec<String>,
        /// A drag outside the pin block selects this local playlist order.
        playlist_order: Option<Vec<String>>,
    },
    RestartEngine,
    /// Rebuild the HTTP client with the proxy in settings. Local playback
    /// restarts only when its HTTP proxy changed.
    ApplyProxy,
    ProxyEdited,
    EnablePlayback,
    ShowWindow,
    HideWindow,
    ClearArtCache,
    /// Clear local play history.
    ClearPlayHistory,
    /// Open or close the Winamp window.
    ToggleWinampWindow,
    /// Select a skin, or the built-in skin for `None`.
    SetSkin(Option<String>),
    /// Install and select a skin file.
    InstallSkin(std::path::PathBuf),
    /// Screen pixels per skin pixel in the Winamp window.
    SetSkinScale(u8),
    ToggleWinampOnTop,
    SetWinampTaskbar(bool),
    OpenSkinsFolder,
    /// Cycle bars, scope, and off.
    CycleVisualiser,
    /// Set the visualizer mode directly.
    SetVisualiser(crate::settings::VisMode),
    /// Open or close the playlist window under the mini player.
    ToggleWinampPlaylist,
    /// The playlist window's height, in skin pixels.
    SetPlaylistHeight(u32),
    /// Open or close the equalizer window under the mini player.
    ToggleWinampEq,
    /// Switch the equalizer's effect on the sound on or off.
    ToggleEq,
    SetEqBand(usize, f32),
    SetEqPreamp(f32),
    /// One of Winamp's presets, by its place in the list.
    ApplyEqPreset(usize),
    /// The balance, -1 all left to 1 all right.
    SetBalance(f32),
    ToggleMono,
    /// Roll the playlist window up to its title bar, or down again.
    ToggleWinampPlaylistShade,
    /// Roll the equalizer window up to its title bar, or down again.
    ToggleWinampEqShade,
    /// Close the window the way its close button does: into the tray when
    /// that is on, out of the app otherwise.
    CloseWindow,
    /// Roll the main window up to its title bar, or down again.
    ToggleWinampShade,
    /// Open or close the MilkDrop window.
    ToggleWinampMilkdrop,
    /// How long each MilkDrop preset plays, in seconds.
    SetMilkdropSeconds(u32),
    SetMilkdropScale(u32),
    /// How many frames a second the MilkDrop window draws; 0 is uncapped.
    SetMilkdropFps(u32),
    OpenMilkdropFolder,
    /// Fetch one of projectM's preset packs into the folder, by its place
    /// in the list.
    DownloadMilkdropPack(usize),
    Quit,
}

#[cfg(test)]
mod finite_scroll_tests {
    use super::*;

    fn page(offset: u32, count: u32, total: u32) -> Page_<u32> {
        Page_ {
            items: (offset..offset + count).collect(),
            offset,
            limit: count,
            total,
            next: (offset + count < total).then(|| "next".into()),
        }
    }

    #[test]
    fn a_failed_backward_window_can_retry_from_the_end() {
        let mut list = PagedList::default();
        list.absorb(950, page(950, 50, 1000));
        assert_eq!(list.next_offset, None);
        assert_eq!(list.window_at(920, 50), Some(900));
        list.fail("Offline".into());
        assert!(list.can_load_more());
        assert_eq!(list.window_request, Some(900));
    }

    #[test]
    fn late_cache_preserves_an_adjacent_request_and_its_cached_tail() {
        let mut list = PagedList::default();
        list.absorb(0, page(0, 50, 1000));
        list.window_at(60, 50);
        list.adopt_cached_prefix((0..500).collect(), 1000, Some(500));
        assert!(list.loading);
        list.absorb(50, page(50, 50, 1000));
        assert_eq!(list.items.len(), 500);
        assert_eq!(list.items[60], 60);
    }

    #[test]
    fn catalog_pages_keep_rows_when_nulls_shorten_a_page() {
        let mut list = PagedList::default();
        list.absorb(0, page(0, 49, 100));
        list.absorb(50, page(50, 50, 100));
        assert_eq!(list.base_offset, 0);
        assert_eq!(list.items.len(), 99);
        assert_eq!(list.items[0], 0);
    }

    #[test]
    fn distant_windows_keep_the_total_and_return_without_a_request() {
        let mut list = PagedList::default();
        list.absorb(0, page(0, 50, 1000));
        assert_eq!(list.window_at(720, 50), Some(700));
        assert_eq!(list.total, Some(1000));
        list.absorb(700, page(700, 50, 1000));
        assert_eq!(list.items[20], 720);
        assert_eq!(list.window_at(10, 50), None);
        assert_eq!(list.base_offset, 0);
        assert_eq!(list.items[10], 10);
        assert_eq!(list.window_at(720, 50), None);
        assert_eq!(list.base_offset, 700);
    }

    #[test]
    fn adjacent_windows_join_in_both_directions() {
        let mut list = PagedList::default();
        list.absorb(0, page(0, 50, 200));
        assert_eq!(list.window_at(55, 50), Some(50));
        list.absorb(50, page(50, 50, 200));
        assert_eq!(list.base_offset, 0);
        assert_eq!(list.items, (0..100).collect::<Vec<_>>());
        assert_eq!(list.window_at(155, 50), Some(150));
        list.absorb(150, page(150, 50, 200));
        assert!(!list.is_complete());
        assert_eq!(list.window_at(120, 50), Some(100));
        list.absorb(100, page(100, 50, 200));
        assert_eq!(list.items, (0..200).collect::<Vec<_>>());
        assert!(list.is_complete());
    }

    #[test]
    fn overlapping_initial_album_page_is_replaced_by_the_full_window() {
        let mut list = PagedList::default();
        list.absorb(0, page(0, 20, 200));
        assert_eq!(list.window_at(25, 50), Some(0));
        list.absorb(0, page(0, 50, 200));
        assert!(
            list.windows.is_empty(),
            "superseded prefix must not stay cached"
        );
    }

    #[test]
    fn filling_a_partial_window_keeps_existing_rows_visible() {
        let mut list = PagedList::default();
        list.absorb(0, page(0, 49, 1000));
        assert_eq!(list.window_at(49, 50), Some(0));
        assert_eq!(list.items, (0..49).collect::<Vec<_>>());
        list.absorb(0, page(0, 50, 1000));
        assert_eq!(list.items, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn overlapping_windows_keep_server_positions_after_an_edit() {
        let mut list = PagedList::default();
        list.absorb(701, page(701, 50, 1000));
        assert_eq!(list.window_at(700, 50), Some(700));
        assert_eq!(
            list.items[0], 701,
            "loaded songs stay visible during the request"
        );
        list.absorb(700, page(700, 50, 1000));
        assert_eq!(list.base_offset, 700);
        assert_eq!(list.items, (700..751).collect::<Vec<_>>());
    }

    #[test]
    fn failed_window_does_not_retry_every_frame() {
        let mut list = PagedList::default();
        list.absorb(0, page(0, 50, 200));
        assert_eq!(list.window_at(150, 50), Some(150));
        assert_eq!(list.window_at(150, 50), None);
        list.fail("offline".into());
        assert_eq!(list.window_at(150, 50), None);
    }
}
