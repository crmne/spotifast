//! Spotify Web API response shapes.
//!
//! Every field that Spotify may omit, null, or rename is optional or
//! defaulted, so a response that changed shape degrades to a blank field
//! instead of a failed page. The 2026 endpoint changes (`/playlists/{id}/items`
//! returning `item` instead of `track`, `items.total` beside `tracks.total`)
//! are accepted alongside the classic shapes.

use serde::{Deserialize, Deserializer, Serialize};

fn skip_nulls<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let items: Vec<Option<T>> = Vec::deserialize(deserializer)?;
    Ok(items.into_iter().flatten().collect())
}

fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// One page of a paginated collection.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(bound(deserialize = "T: Deserialize<'de>", serialize = "T: Serialize"))]
pub struct Page<T> {
    #[serde(default = "Vec::new", deserialize_with = "skip_nulls")]
    pub items: Vec<T>,
    #[serde(default)]
    pub total: u32,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub next: Option<String>,
}

impl<T> Page<T> {
    pub fn next_offset(&self) -> Option<u32> {
        let consumed = self.limit.max(self.items.len() as u32);
        (self.next.is_some() && consumed > 0).then_some(self.offset + consumed)
    }
}

/// Positional track endpoints must retain null slots: dropping one changes
/// every subsequent playback and edit offset. Other catalogue lists may skip them.
#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub(super) struct PositionedPage<T> {
    #[serde(default)]
    items: Vec<Option<T>>,
    #[serde(default)]
    total: u32,
    #[serde(default)]
    limit: u32,
    #[serde(default)]
    offset: u32,
    #[serde(default)]
    next: Option<String>,
}

impl<T: Default> From<PositionedPage<T>> for Page<T> {
    fn from(page: PositionedPage<T>) -> Self {
        Self {
            items: page
                .items
                .into_iter()
                .map(Option::unwrap_or_default)
                .collect(),
            total: page.total,
            limit: page.limit,
            offset: page.offset,
            next: page.next,
        }
    }
}

fn positioned_album_tracks<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Page<Track>>, D::Error> {
    Ok(Option::<PositionedPage<Track>>::deserialize(deserializer)?.map(Into::into))
}

/// A cursor-paginated collection (followed artists, recently played).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct CursorPage<T> {
    #[serde(default = "Vec::new", deserialize_with = "skip_nulls")]
    pub items: Vec<T>,
    #[serde(default)]
    pub total: Option<u32>,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub cursors: Option<Cursors>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Cursors {
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub before: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Image {
    #[serde(default, deserialize_with = "null_default")]
    pub url: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

/// Picks the smallest image at least `target` pixels wide, or the largest.
pub fn pick_image(images: &[Image], target: u32) -> Option<&str> {
    let mut best: Option<&Image> = None;
    for image in images {
        let width = image.width.unwrap_or(u32::MAX);
        match best {
            None => best = Some(image),
            Some(current) => {
                let current_width = current.width.unwrap_or(u32::MAX);
                let current_ok = current_width >= target;
                let candidate_ok = width >= target;
                let better = match (current_ok, candidate_ok) {
                    (true, true) => width < current_width,
                    (false, true) => true,
                    (true, false) => false,
                    (false, false) => width > current_width,
                };
                if better {
                    best = Some(image);
                }
            }
        }
    }
    best.map(|image| image.url.as_str())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ExternalUrls {
    #[serde(default)]
    pub spotify: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Followers {
    #[serde(default)]
    pub total: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ArtistRef {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    pub uri: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Artist {
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub uri: String,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Vec<Image>,
    #[serde(default, deserialize_with = "null_default")]
    pub genres: Vec<String>,
    #[serde(default)]
    pub followers: Option<Followers>,
    #[serde(default)]
    pub popularity: Option<u8>,
    #[serde(default)]
    pub external_urls: ExternalUrls,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Album {
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub uri: String,
    #[serde(default)]
    pub album_type: Option<String>,
    #[serde(default)]
    pub album_group: Option<String>,
    #[serde(default)]
    pub total_tracks: Option<u32>,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Vec<Image>,
    #[serde(default, deserialize_with = "null_default")]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub genres: Vec<String>,
    #[serde(default)]
    pub popularity: Option<u8>,
    #[serde(default, deserialize_with = "positioned_album_tracks")]
    pub tracks: Option<Page<Track>>,
    #[serde(default)]
    pub external_urls: ExternalUrls,
    #[serde(default, deserialize_with = "null_default")]
    pub copyrights: Vec<Copyright>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Copyright {
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub text: String,
    #[serde(default, rename = "type")]
    #[serde(deserialize_with = "null_default")]
    pub kind: String,
}

impl Album {
    pub fn year(&self) -> Option<&str> {
        self.release_date
            .as_deref()
            .map(|date| date.get(..4).unwrap_or(date))
    }

    pub fn kind_label(&self, locale: crate::i18n::Locale) -> std::borrow::Cow<'static, str> {
        use crate::i18n::gettext;
        match self
            .album_group
            .as_deref()
            .or(self.album_type.as_deref())
            .unwrap_or("album")
        {
            "single" => gettext(locale, "Single"),
            "compilation" => gettext(locale, "Compilation"),
            "appears_on" => gettext(locale, "Appears On"),
            _ => gettext(locale, "Album"),
        }
    }

    pub(crate) fn is_single_release(&self) -> bool {
        self.album_group.as_deref().or(self.album_type.as_deref()) == Some("single")
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Track {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub uri: String,
    #[serde(default)]
    pub duration_ms: u32,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub album: Option<Album>,
    #[serde(default)]
    pub track_number: Option<u32>,
    #[serde(default)]
    pub disc_number: Option<u32>,
    #[serde(default)]
    pub is_local: bool,
    #[serde(default)]
    pub is_playable: Option<bool>,
    #[serde(default)]
    pub popularity: Option<u8>,
    #[serde(default)]
    pub external_ids: ExternalIds,
    /// The originally requested track when Spotify substituted a playable
    /// release for the account's market.
    #[serde(default)]
    pub linked_from: Option<LinkedTrack>,
    #[serde(default)]
    pub external_urls: ExternalUrls,
}

impl Track {
    pub fn artist_names(&self) -> String {
        join_names(self.artists.iter().map(|artist| artist.name.as_str()))
    }

    pub fn image(&self, target: u32) -> Option<&str> {
        self.album
            .as_ref()
            .and_then(|album| pick_image(&album.images, target))
    }

    /// Stable identity shared by market-specific releases of one recording.
    pub fn recording_key(&self) -> Option<String> {
        self.external_ids
            .isrc
            .as_ref()
            .filter(|isrc| !isrc.is_empty())
            .map(|isrc| format!("isrc:{isrc}"))
            .or_else(|| {
                self.linked_from
                    .as_ref()
                    .filter(|track| !track.uri.is_empty())
                    .map(|track| format!("linked:{}", track.uri))
            })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ExternalIds {
    #[serde(default)]
    pub isrc: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct LinkedTrack {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub uri: String,
}

pub fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for (index, name) in names.enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(name);
    }
    out
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ResumePoint {
    #[serde(default)]
    pub fully_played: bool,
    #[serde(default)]
    pub resume_position_ms: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Episode {
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub uri: String,
    #[serde(default)]
    pub duration_ms: u32,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub description: String,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Vec<Image>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default)]
    pub resume_point: Option<ResumePoint>,
    #[serde(default)]
    pub show: Option<Show>,
    #[serde(default)]
    pub external_urls: ExternalUrls,
}

impl Episode {
    /// Spotify's saved place in this episode when it was started and not
    /// finished, kept a moment short of the end. A finished or unstarted
    /// episode starts from the beginning.
    pub fn resume_ms(&self) -> Option<u32> {
        let resume = self.resume_point.as_ref()?;
        if resume.fully_played || resume.resume_position_ms == 0 {
            return None;
        }
        Some(if self.duration_ms > 0 {
            resume
                .resume_position_ms
                .min(self.duration_ms.saturating_sub(1))
        } else {
            resume.resume_position_ms
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Show {
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub uri: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub publisher: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub description: String,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Vec<Image>,
    #[serde(default)]
    pub total_episodes: Option<u32>,
    #[serde(default)]
    pub episodes: Option<Page<Episode>>,
    #[serde(default)]
    pub external_urls: ExternalUrls,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Owner {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct TrackCount {
    #[serde(default)]
    pub total: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Playlist {
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub uri: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Vec<Image>,
    #[serde(default)]
    pub owner: Owner,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default)]
    pub collaborative: bool,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub tracks: Option<TrackCount>,
    #[serde(default, rename = "items")]
    pub items_count: Option<TrackCount>,
    #[serde(default)]
    pub external_urls: ExternalUrls,
}

impl Playlist {
    pub fn track_total(&self) -> u32 {
        self.items_count
            .as_ref()
            .or(self.tracks.as_ref())
            .map_or(0, |count| count.total)
    }

    /// The owner as shown: the display name, else the id of anyone but
    /// Spotify, whose lists carry no name over the streaming session.
    /// Take from the library list's entry what this header lacks: the
    /// streaming session's header carries no public flag, may leave the
    /// owner unnamed, and has no picture for a playlist without a cover of
    /// its own. What Spotify did say stays.
    pub fn fill_from(&mut self, listed: &Playlist) {
        if self.public.is_none() {
            self.public = listed.public;
        }
        if self.owner.display_name.is_none() {
            self.owner.display_name = listed.owner.display_name.clone();
        }
        if self.images.is_empty() {
            self.images = listed.images.clone();
        }
    }

    pub fn owner_name(&self) -> &str {
        self.owner
            .display_name
            .as_deref()
            .or_else(|| self.owner.id.as_deref().filter(|id| *id != "spotify"))
            .unwrap_or("Spotify")
    }

    pub fn owned_by(&self, user_id: &str) -> bool {
        self.owner.id.as_deref() == Some(user_id)
    }
}

/// A track or an episode, as returned wherever Spotify mixes both.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PlayableItem {
    Track(Track),
    Episode(Episode),
}

impl PlayableItem {
    pub fn uri(&self) -> &str {
        match self {
            Self::Track(track) => &track.uri,
            Self::Episode(episode) => &episode.uri,
        }
    }

    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Track(track) => track.id.as_deref(),
            Self::Episode(episode) => Some(&episode.id),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Track(track) => &track.name,
            Self::Episode(episode) => &episode.name,
        }
    }

    pub fn duration_ms(&self) -> u32 {
        match self {
            Self::Track(track) => track.duration_ms,
            Self::Episode(episode) => episode.duration_ms,
        }
    }

    pub fn subtitle(&self) -> String {
        match self {
            Self::Track(track) => track.artist_names(),
            Self::Episode(episode) => episode
                .show
                .as_ref()
                .map(|show| show.name.clone())
                .unwrap_or_default(),
        }
    }

    pub fn image(&self, target: u32) -> Option<&str> {
        match self {
            Self::Track(track) => track.image(target),
            Self::Episode(episode) => pick_image(&episode.images, target).or_else(|| {
                episode
                    .show
                    .as_ref()
                    .and_then(|show| pick_image(&show.images, target))
            }),
        }
    }

    pub fn is_track(&self) -> bool {
        matches!(self, Self::Track(_))
    }
}

/// An entry in a playlist. `item` is the 2026 name, `track` the classic one.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PlaylistItem {
    #[serde(default)]
    pub added_at: Option<String>,
    #[serde(default)]
    pub added_by: Option<UserRef>,
    #[serde(default)]
    pub is_local: bool,
    #[serde(default)]
    pub item: Option<PlayableItem>,
    #[serde(default)]
    pub track: Option<PlayableItem>,
}

/// A bare user reference, as `added_by` carries it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct UserRef {
    #[serde(default)]
    pub id: Option<String>,
}

impl PlaylistItem {
    pub fn playable(&self) -> Option<&PlayableItem> {
        self.item.as_ref().or(self.track.as_ref())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct SavedTrack {
    #[serde(default)]
    pub added_at: Option<String>,
    pub track: Track,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct SavedAlbum {
    #[serde(default)]
    pub added_at: Option<String>,
    pub album: Album,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct SavedShow {
    #[serde(default)]
    pub added_at: Option<String>,
    pub show: Show,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct SavedEpisode {
    #[serde(default)]
    pub added_at: Option<String>,
    pub episode: Episode,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct FollowedArtists {
    pub artists: CursorPage<Artist>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PlayHistory {
    pub track: Track,
    #[serde(default)]
    pub played_at: Option<String>,
    #[serde(default)]
    pub context: Option<Context>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Context {
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub uri: String,
    #[serde(default, rename = "type")]
    #[serde(deserialize_with = "null_default")]
    pub kind: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Device {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_restricted: bool,
    #[serde(default)]
    pub volume_percent: Option<u8>,
    #[serde(default)]
    pub supports_volume: Option<bool>,
    #[serde(default, rename = "type")]
    #[serde(deserialize_with = "null_default")]
    pub kind: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct DeviceList {
    #[serde(default)]
    #[serde(deserialize_with = "skip_nulls")]
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct PlaybackState {
    #[serde(default)]
    pub device: Option<Device>,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub repeat_state: String,
    #[serde(default)]
    pub shuffle_state: bool,
    #[serde(default)]
    pub context: Option<Context>,
    #[serde(default)]
    pub timestamp: u64,
    #[serde(default)]
    pub progress_ms: Option<u32>,
    #[serde(default)]
    pub is_playing: bool,
    #[serde(default)]
    pub item: Option<PlayableItem>,
    #[serde(default)]
    pub currently_playing_type: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Queue {
    #[serde(default)]
    pub currently_playing: Option<PlayableItem>,
    #[serde(default, deserialize_with = "skip_nulls")]
    pub queue: Vec<PlayableItem>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct SearchResults {
    #[serde(default)]
    pub tracks: Option<Page<Track>>,
    #[serde(default)]
    pub artists: Option<Page<Artist>>,
    #[serde(default)]
    pub albums: Option<Page<Album>>,
    #[serde(default)]
    pub playlists: Option<Page<Playlist>>,
    #[serde(default)]
    pub shows: Option<Page<Show>>,
    #[serde(default)]
    pub episodes: Option<Page<Episode>>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        [
            self.tracks
                .as_ref()
                .is_none_or(|page| page.items.is_empty()),
            self.artists
                .as_ref()
                .is_none_or(|page| page.items.is_empty()),
            self.albums
                .as_ref()
                .is_none_or(|page| page.items.is_empty()),
            self.playlists
                .as_ref()
                .is_none_or(|page| page.items.is_empty()),
            self.shows.as_ref().is_none_or(|page| page.items.is_empty()),
            self.episodes
                .as_ref()
                .is_none_or(|page| page.items.is_empty()),
        ]
        .iter()
        .all(|empty| *empty)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct User {
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub images: Vec<Image>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
}

impl User {
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct TopTracks {
    #[serde(default, deserialize_with = "skip_nulls")]
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct RelatedArtists {
    #[serde(default, deserialize_with = "skip_nulls")]
    pub artists: Vec<Artist>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Recommendations {
    #[serde(default, deserialize_with = "skip_nulls")]
    pub tracks: Vec<Track>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct SnapshotId {
    #[serde(default)]
    pub snapshot_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ApiErrorBody {
    #[serde(default)]
    pub error: ApiErrorDetail,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ApiErrorDetail {
    #[serde(default)]
    pub status: u16,
    #[serde(default)]
    #[serde(deserialize_with = "null_default")]
    pub message: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_albums_year_reads_a_date_the_way_format_date_does() {
        let dated = |date: &str| {
            let json = format!(
                r#"{{"id":"a","name":"A","uri":"spotify:album:a","release_date":"{date}"}}"#
            );
            serde_json::from_str::<Album>(&json).unwrap()
        };
        // The dates Spotify documents, at all three precisions.
        assert_eq!(dated("2024-03-15").year(), Some("2024"));
        assert_eq!(dated("2024-03").year(), Some("2024"));
        assert_eq!(dated("2024").year(), Some("2024"));
        // Shorter than a year, which `min` was there to hold.
        assert_eq!(dated("20").year(), Some("20"));
        // A date whose fourth byte is inside a character. `min` clamps the
        // length and says nothing about boundaries, so this used to panic;
        // `util::format_date` reads the same field with `get` and does not.
        assert_eq!(dated("\u{c791}\u{b144}").year(), Some("\u{c791}\u{b144}"));
        assert_eq!(
            crate::util::format_date(crate::i18n::Locale::English, "\u{c791}\u{b144}"),
            "\u{c791}\u{b144}"
        );
        // No date at all is still no year.
        let json = r#"{"id":"a","name":"A","uri":"spotify:album:a"}"#;
        assert_eq!(serde_json::from_str::<Album>(json).unwrap().year(), None);
    }

    #[test]
    fn album_tracks_keep_null_server_positions() {
        let album: Album = serde_json::from_str(r#"{"tracks":{"items":[{"uri":"spotify:track:a"},null,{"uri":"spotify:track:c"}],"total":3,"limit":3}}"#).unwrap();
        let tracks = album.tracks.unwrap();
        assert_eq!(tracks.items.len(), 3);
        assert_eq!(tracks.items[2].uri, "spotify:track:c");
    }

    #[test]
    fn playlist_items_accept_both_item_and_track_keys() {
        let classic = r#"{"items":[{"added_at":"2024-01-01T00:00:00Z","track":{"type":"track","id":"a","name":"One","uri":"spotify:track:a","duration_ms":1000,"artists":[{"name":"Artist"}]}}],"total":1}"#;
        let modern = r#"{"items":[{"added_at":"2024-01-01T00:00:00Z","item":{"type":"episode","id":"e","name":"Ep","uri":"spotify:episode:e","duration_ms":2000}}, null],"total":2}"#;
        let classic: Page<PlaylistItem> = serde_json::from_str(classic).unwrap();
        let modern: Page<PlaylistItem> = serde_json::from_str(modern).unwrap();
        assert_eq!(classic.items[0].playable().unwrap().name(), "One");
        assert_eq!(modern.items.len(), 1);
        assert_eq!(modern.items[0].playable().unwrap().name(), "Ep");
        assert!(!modern.items[0].playable().unwrap().is_track());
    }

    #[test]
    fn playlist_total_prefers_items_count() {
        let json = r#"{"id":"p","name":"P","uri":"spotify:playlist:p","items":{"total":12},"tracks":{"total":3},"owner":{"id":"me","display_name":"Me"}}"#;
        let playlist: Playlist = serde_json::from_str(json).unwrap();
        assert_eq!(playlist.track_total(), 12);
        assert!(playlist.owned_by("me"));
        assert_eq!(playlist.owner_name(), "Me");
    }

    /// An owner without a display name shows as the id, except Spotify,
    /// whose lists carry no name over the streaming session.
    /// A header takes from the library list only what it lacks.
    #[test]
    fn a_header_takes_from_the_list_only_what_it_lacks() {
        let listed = Playlist {
            public: Some(true),
            owner: Owner {
                id: Some("alice".into()),
                display_name: Some("Alice".into()),
                ..Default::default()
            },
            images: vec![Image {
                url: "mosaic".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut bare = Playlist::default();
        bare.fill_from(&listed);
        assert_eq!(bare.public, Some(true));
        assert_eq!(bare.owner.display_name.as_deref(), Some("Alice"));
        assert_eq!(bare.images[0].url, "mosaic");

        let mut told = Playlist {
            public: Some(false),
            owner: Owner {
                display_name: Some("Spotify".into()),
                ..Default::default()
            },
            images: vec![Image {
                url: "cover".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        told.fill_from(&listed);
        assert_eq!(told.public, Some(false));
        assert_eq!(told.owner.display_name.as_deref(), Some("Spotify"));
        assert_eq!(told.images[0].url, "cover");
    }

    #[test]
    fn owner_name_falls_back_to_the_id_but_not_for_spotify() {
        let named = |id: Option<&str>, name: Option<&str>| Playlist {
            owner: Owner {
                id: id.map(str::to_string),
                display_name: name.map(str::to_string),
                uri: None,
            },
            ..Playlist::default()
        };
        assert_eq!(named(Some("1263908142"), None).owner_name(), "1263908142");
        assert_eq!(named(Some("1263908142"), Some("mgc")).owner_name(), "mgc");
        assert_eq!(named(Some("spotify"), None).owner_name(), "Spotify");
        assert_eq!(named(None, None).owner_name(), "Spotify");
    }

    #[test]
    fn image_picker_prefers_smallest_sufficient() {
        let images = vec![
            Image {
                url: "large".into(),
                width: Some(640),
                height: Some(640),
            },
            Image {
                url: "medium".into(),
                width: Some(300),
                height: Some(300),
            },
            Image {
                url: "small".into(),
                width: Some(64),
                height: Some(64),
            },
        ];
        assert_eq!(pick_image(&images, 64), Some("small"));
        assert_eq!(pick_image(&images, 100), Some("medium"));
        assert_eq!(pick_image(&images, 1000), Some("large"));
        assert_eq!(pick_image(&[], 64), None);
    }

    #[test]
    fn null_fields_fall_back_to_defaults() {
        let json = r#"{"id":"x","name":"X","uri":"spotify:artist:x","images":null,"genres":null,"followers":null}"#;
        let artist: Artist = serde_json::from_str(json).unwrap();
        assert!(artist.images.is_empty());
        assert!(artist.genres.is_empty());
    }

    #[test]
    fn track_keeps_artist_objects_when_a_name_has_a_comma() {
        let json = r#"{"name":"Song","artists":[{"id":"tyler","name":"Tyler, the Creator"},{"id":"guest","name":"Guest"}]}"#;
        let track: Track = serde_json::from_str(json).unwrap();

        assert_eq!(track.artists.len(), 2);
        assert_eq!(track.artists[0].name, "Tyler, the Creator");
        assert_eq!(track.artists[1].id.as_deref(), Some("guest"));
    }

    #[test]
    fn track_keeps_spotifys_recording_and_relink_identities() {
        let json = r#"{"id":"playable","uri":"spotify:track:playable","external_ids":{"isrc":"GBUM71029604"},"linked_from":{"id":"original","uri":"spotify:track:original"}}"#;
        let track: Track = serde_json::from_str(json).unwrap();

        assert_eq!(track.external_ids.isrc.as_deref(), Some("GBUM71029604"));
        assert_eq!(
            track.linked_from.as_ref().map(|track| track.uri.as_str()),
            Some("spotify:track:original")
        );
        assert_eq!(track.recording_key().as_deref(), Some("isrc:GBUM71029604"));
    }

    #[test]
    fn search_playlists_skip_null_entries() {
        let json = r#"{"playlists":{"items":[null,{"id":"p","name":"P","uri":"spotify:playlist:p"}],"total":2,"limit":2,"offset":0,"next":"next page"}}"#;
        let results: SearchResults = serde_json::from_str(json).unwrap();
        let playlists = results.playlists.unwrap();
        assert_eq!(playlists.items.len(), 1);
        assert_eq!(playlists.next_offset(), Some(2));
    }
}
