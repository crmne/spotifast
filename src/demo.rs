//! Sample data for screenshots and headless rendering tests.
//!
//! Nothing here talks to Spotify: the backend is switched offline, the
//! session is marked connected, and every page is filled with plausible
//! content. Cover art comes from a public placeholder service so the artwork
//! pipeline (download, disk cache, accent colour) is exercised too.

use std::time::Instant;

use jiff::{SignedDuration, Timestamp};

use crate::api::models::{
    Album, Artist, ArtistRef, Context, Copyright, Device, Episode, Followers, Image, Owner,
    Page as ApiPage, PlayHistory, PlayableItem, PlaybackState, Playlist, PlaylistItem, Queue,
    ResumePoint, SavedAlbum, SavedEpisode, SavedShow, SavedTrack, SearchResults, Show, Track,
    TrackCount, User,
};
use crate::app::{App, RemoteSnapshot};
use crate::backend::AuthStatus;
use crate::model::*;

fn image(seed: u32) -> Vec<Image> {
    vec![
        Image {
            url: format!("https://picsum.photos/seed/fastpotify{seed}/640/640"),
            width: Some(640),
            height: Some(640),
        },
        Image {
            url: format!("https://picsum.photos/seed/fastpotify{seed}/300/300"),
            width: Some(300),
            height: Some(300),
        },
        Image {
            url: format!("https://picsum.photos/seed/fastpotify{seed}/64/64"),
            width: Some(64),
            height: Some(64),
        },
    ]
}

const ARTISTS: &[&str] = &[
    "Bonobo",
    "Khruangbin",
    "Nils Frahm",
    "Little Simz",
    "Floating Points",
    "Jon Hopkins",
    "Sault",
    "Four Tet",
];

const ALBUMS: &[(&str, usize, &str)] = &[
    ("Fragments", 0, "2022"),
    ("Mordechai", 1, "2020"),
    ("All Melody", 2, "2018"),
    ("Sometimes I Might Be Introvert", 3, "2021"),
    ("Promises", 4, "2021"),
    ("Immunity", 5, "2013"),
    ("Untitled (Black Is)", 6, "2020"),
    ("There Is Love in You", 7, "2010"),
];

const TRACKS: &[&str] = &[
    "Rosewood",
    "Otomo",
    "Shadows",
    "Tides",
    "Elysian",
    "Closer",
    "Counterpart",
    "Sapien",
    "From You",
    "Day by Day",
    "Age of Phase",
    "Polyghost",
    "Time Moves Slow",
    "August 10",
    "So Rare",
    "Fugue",
    "Encores",
    "Sunlight",
    "My Friend the Forest",
    "Kaleidoscope",
];

fn demo_added_at(index: usize, now: Timestamp) -> String {
    let age = match index {
        0 => SignedDuration::from_secs(30),
        1 => SignedDuration::from_mins(5),
        2 => SignedDuration::from_hours(3),
        3 => SignedDuration::from_hours(2 * 24),
        4 => SignedDuration::from_hours(2 * 7 * 24),
        _ => SignedDuration::from_hours((35 + index as i64) * 24),
    };
    (now - age).to_string()
}

const PLAYLISTS: &[&str] = &[
    "Discover Weekly",
    "Late night focus",
    "Sunday morning",
    "Running 2026",
    "Release Radar",
    "Berlin nights",
    "Dinner party",
    "Deep work",
    "Road trip",
    "Kitchen jams",
];

fn artist_ref(index: usize) -> ArtistRef {
    ArtistRef {
        id: Some(format!("art{index}")),
        name: ARTISTS[index % ARTISTS.len()].to_string(),
        uri: Some(format!("spotify:artist:art{index}")),
    }
}

fn artist(index: usize) -> Artist {
    Artist {
        id: format!("art{index}"),
        name: ARTISTS[index % ARTISTS.len()].to_string(),
        uri: format!("spotify:artist:art{index}"),
        images: image(100 + index as u32),
        genres: vec!["electronic".into(), "downtempo".into(), "ambient".into()],
        followers: Some(Followers {
            total: 1_284_930 + index as u64 * 10_431,
        }),
        popularity: Some(70),
        ..Artist::default()
    }
}

fn album(index: usize) -> Album {
    let (name, artist_index, year) = ALBUMS[index % ALBUMS.len()];
    Album {
        id: format!("alb{index}"),
        name: name.to_string(),
        uri: format!("spotify:album:alb{index}"),
        album_type: Some(if index % 4 == 3 {
            "single".into()
        } else {
            "album".into()
        }),
        total_tracks: Some(12),
        images: image(200 + index as u32),
        artists: vec![artist_ref(artist_index)],
        release_date: Some(format!("{year}-03-1{}", index % 9)),
        label: Some("Ninja Tune".into()),
        copyrights: vec![Copyright {
            text: format!("{year} Ninja Tune"),
            kind: "C".into(),
        }],
        ..Album::default()
    }
}

fn track(index: usize) -> Track {
    let album_index = index % ALBUMS.len();
    let mut album = album(album_index);
    album.tracks = None;
    Track {
        id: Some(format!("trk{index}")),
        name: TRACKS[index % TRACKS.len()].to_string(),
        uri: format!("spotify:track:trk{index}"),
        duration_ms: 180_000 + (index as u32 * 37_000) % 240_000,
        explicit: index % 7 == 3,
        artists: vec![artist_ref(album_index)],
        album: Some(album),
        track_number: Some((index % 12) as u32 + 1),
        disc_number: Some(1),
        popularity: Some(60 + (index % 40) as u8),
        ..Track::default()
    }
}

fn playlist(index: usize) -> Playlist {
    let name = PLAYLISTS[index % PLAYLISTS.len()];
    let spotify_owned = matches!(name, "Discover Weekly" | "Release Radar");
    Playlist {
        id: format!("pl{index}"),
        name: name.to_string(),
        uri: format!("spotify:playlist:pl{index}"),
        description: Some(if spotify_owned {
            "Your weekly mixtape of fresh music. Enjoy new music and deep cuts picked for you. Updates every Monday.".into()
        } else {
            String::new()
        }),
        images: image(300 + index as u32),
        owner: Owner {
            id: Some(if spotify_owned {
                "spotify".into()
            } else {
                "demo".into()
            }),
            display_name: Some(if spotify_owned {
                "Spotify".into()
            } else {
                "Carmine".into()
            }),
            uri: None,
        },
        public: Some(index.is_multiple_of(2)),
        collaborative: false,
        snapshot_id: Some("snap".into()),
        tracks: Some(TrackCount {
            total: 30 + index as u32 * 7,
        }),
        ..Playlist::default()
    }
}

fn episode(index: usize, show_index: usize) -> Episode {
    Episode {
        id: format!("ep{show_index}_{index}"),
        name: format!("Episode {}: {}", 120 - index, TRACKS[(index * 3) % TRACKS.len()]),
        uri: format!("spotify:episode:ep{show_index}_{index}"),
        duration_ms: 2_400_000 + (index as u32 * 311_000) % 2_000_000,
        description: "A conversation about how software gets made, why some tools feel fast, and what we can learn from the people who build them. Recorded live.".into(),
        images: image(400 + index as u32),
        release_date: Some(format!("2026-0{}-{:02}", 1 + index % 8, 1 + index % 27)),
        resume_point: Some(ResumePoint {
            fully_played: index.is_multiple_of(5),
            resume_position_ms: if index % 3 == 1 { 600_000 } else { 0 },
        }),
        show: Some(show(show_index)),
        ..Episode::default()
    }
}

fn show(index: usize) -> Show {
    Show {
        id: format!("sh{index}"),
        name: ["Rework", "Song Exploder", "The Rest Is History", "Darknet Diaries"][index % 4].into(),
        uri: format!("spotify:show:sh{index}"),
        publisher: ["37signals", "Hrishikesh Hirway", "Goalhanger", "Jack Rhysider"][index % 4].into(),
        description: "A podcast about a better way to work and run your business. Hosted by the founders of 37signals.".into(),
        images: image(500 + index as u32),
        total_episodes: Some(84),
        ..Show::default()
    }
}

fn page<T>(items: Vec<T>) -> ApiPage<T> {
    let total = items.len() as u32;
    ApiPage {
        items,
        total,
        limit: total,
        offset: 0,
        next: None,
    }
}

pub fn populate(app: &mut App) {
    app.backend.set_offline(true);
    app.offline = true;
    app.auth = AuthStatus::Connected {
        username: "demo".into(),
    };
    app.local_device_id = Some("local-demo".into());
    app.local_ready = true;
    app.local_playback = crate::backend::LocalPlayback::Ready {
        device_id: "local-demo".into(),
    };
    app.user = Some(User {
        id: "demo".into(),
        display_name: Some("Carmine".into()),
        images: image(1),
        product: Some("premium".into()),
        country: Some("DE".into()),
        uri: Some("spotify:user:demo".into()),
    });

    let playlists: Vec<Playlist> = (0..PLAYLISTS.len()).map(playlist).collect();
    for playlist in &playlists {
        app.saved.insert(playlist.uri.clone(), true);
    }
    app.library.playlists = Loadable::Loaded(playlists.clone());

    let tracks: Vec<Track> = (0..40).map(track).collect();
    for (index, track) in tracks.iter().enumerate() {
        app.saved.insert(track.uri.clone(), index % 3 == 0);
    }

    // Playlist page.
    let mut playlist_page = PlaylistPage {
        playlist: Loadable::Loaded(playlists[1].clone()),
        ..PlaylistPage::default()
    };
    let demo_now = Timestamp::now();
    playlist_page.items.absorb(
        0,
        page(
            tracks
                .iter()
                .enumerate()
                .map(|(index, track)| PlaylistItem {
                    // The first rows deliberately cover each relative-date
                    // unit; the rest remain absolute dates.
                    added_at: Some(demo_added_at(index, demo_now)),
                    is_local: false,
                    // Use multiple contributors so Added By and the byline render.
                    added_by: Some(crate::api::models::UserRef {
                        id: Some(if index % 3 == 1 { "kasia" } else { "sam" }.into()),
                    }),
                    item: Some(PlayableItem::Track(track.clone())),
                    track: None,
                })
                .collect(),
        ),
    );
    playlist_page.contributors.insert("kasia".into());
    playlist_page.contributors.insert("sam".into());
    app.playlist_pages.insert("pl1".into(), playlist_page);
    app.user_names.insert("kasia".into(), Some("Kasia".into()));
    app.user_names.insert("sam".into(), Some("Sam".into()));
    let mut discover_page = PlaylistPage {
        playlist: Loadable::Loaded(playlists[0].clone()),
        ..PlaylistPage::default()
    };
    discover_page.items.absorb(
        0,
        page(
            tracks
                .iter()
                .rev()
                .take(30)
                .map(|track| PlaylistItem {
                    added_at: Some("2026-08-24T05:00:00Z".into()),
                    is_local: false,
                    added_by: None,
                    item: Some(PlayableItem::Track(track.clone())),
                    track: None,
                })
                .collect(),
        ),
    );
    app.playlist_pages.insert("pl0".into(), discover_page);

    // Album page.
    let mut album_page = AlbumPage {
        album: Loadable::Loaded(album(0)),
        ..AlbumPage::default()
    };
    album_page
        .tracks
        .absorb(0, page(tracks.iter().take(12).cloned().collect()));
    app.album_pages.insert("alb0".into(), album_page);
    app.saved.insert("spotify:album:alb0".into(), true);

    // Artist page.
    let mut artist_page = ArtistPage {
        artist: Loadable::Loaded(artist(0)),
        top_tracks: Loadable::Loaded(tracks.iter().take(10).cloned().collect()),
        related: Loadable::Loaded((1..8).map(artist).collect()),
        ..ArtistPage::default()
    };
    let mut albums = PagedList::default();
    albums.absorb(0, page((0..8).map(album).collect()));
    artist_page
        .albums
        .insert(DiscographyFilter::All.groups().to_string(), albums);
    app.artist_pages.insert("art0".into(), artist_page);
    app.saved.insert("spotify:artist:art0".into(), true);

    // Show page.
    let mut show_page = ShowPage {
        show: Loadable::Loaded(show(0)),
        ..ShowPage::default()
    };
    show_page
        .episodes
        .absorb(0, page((0..15).map(|index| episode(index, 0)).collect()));
    app.show_pages.insert("sh0".into(), show_page);

    // Library.
    app.library.liked.absorb(
        0,
        page(
            tracks
                .iter()
                .filter(|track| app.saved.get(&track.uri) == Some(&true))
                .map(|track| SavedTrack {
                    added_at: Some("2026-06-12T08:30:00Z".into()),
                    track: track.clone(),
                })
                .collect(),
        ),
    );
    app.library.albums.absorb(
        0,
        page(
            (0..8)
                .map(|index| SavedAlbum {
                    added_at: None,
                    album: album(index),
                })
                .collect(),
        ),
    );
    app.library.artists.items = (0..8).map(artist).collect();
    app.library.artists.loaded_once = true;
    app.library.artists.complete = true;
    app.library.shows.absorb(
        0,
        page(
            (0..4)
                .map(|index| SavedShow {
                    added_at: None,
                    show: show(index),
                })
                .collect(),
        ),
    );
    app.library.episodes.absorb(
        0,
        page(
            (0..6)
                .map(|index| SavedEpisode {
                    added_at: None,
                    episode: episode(index, index % 4),
                })
                .collect(),
        ),
    );

    // Home.
    app.home.requested = true;
    app.home.loaded_at = Some(Instant::now());
    app.home.recently_played = Loadable::Loaded(
        tracks
            .iter()
            .skip(5)
            .take(12)
            .map(|track| PlayHistory {
                track: track.clone(),
                played_at: Some("2026-08-26T21:12:00Z".into()),
                context: None,
            })
            .collect(),
    );
    // Recents tab (queue sidebar) – deduped, timestamped, paginated.
    let recents_now = Timestamp::now();
    app.recents.items = tracks
        .iter()
        .skip(2)
        .take(24)
        .enumerate()
        .map(|(index, track)| PlayHistory {
            track: track.clone(),
            played_at: Some(demo_added_at(index, recents_now)),
            context: None,
        })
        .collect();
    app.recents.loaded_once = true;
    app.recents.loading = false;
    app.recents.error = None;
    // Has more to load (before cursor of oldest item).
    let oldest = recents_now - SignedDuration::from_hours(48);
    // Cursor is unix millis; jiff Timestamp exposes seconds + nanos.
    let millis = oldest.as_second() * 1000 + i64::from(oldest.subsec_nanosecond() / 1_000_000);
    app.recents.after = Some(millis.to_string());
    app.recents.complete = false;
    app.home.top_artists = Loadable::Loaded((0..8).map(artist).collect());
    app.home.top_tracks = Loadable::Loaded(tracks.iter().skip(10).take(10).cloned().collect());
    app.home.top_songs = Loadable::Loaded(tracks.iter().skip(10).cloned().collect());
    app.home.top_songs_complete = true;
    app.home.recommendations = Loadable::Loaded(tracks.iter().skip(20).take(10).cloned().collect());
    for term in DISCOVER_TERMS {
        let matching: Vec<Playlist> = playlists
            .iter()
            .filter(|playlist| playlist.name.to_lowercase().contains(&term.to_lowercase()))
            .cloned()
            .collect();
        app.home
            .discover
            .insert((*term).to_string(), Loadable::Loaded(matching));
    }

    // Search.
    app.search.query = "Bonobo".into();
    app.search.committed = "Bonobo".into();
    app.search.results = Loadable::Loaded(SearchResults {
        tracks: Some(page(tracks.iter().take(10).cloned().collect())),
        artists: Some(page((0..6).map(artist).collect())),
        albums: Some(page((0..6).map(album).collect())),
        playlists: Some(page(playlists.iter().take(6).cloned().collect())),
        shows: Some(page((0..4).map(show).collect())),
        episodes: Some(page((0..4).map(|index| episode(index, 1)).collect())),
    });
    app.settings.search_history = vec!["Khruangbin".into(), "ambient".into(), "Rework".into()];

    // Playback: a remote speaker is playing the second playlist.
    app.queue = Loadable::Loaded(Queue {
        currently_playing: Some(PlayableItem::Track(tracks[0].clone())),
        queue: tracks
            .iter()
            .skip(1)
            .take(12)
            .cloned()
            .map(PlayableItem::Track)
            .collect(),
    });
    app.devices = vec![
        Device {
            id: Some("local-demo".into()),
            name: "Spotifast".into(),
            is_active: false,
            is_restricted: false,
            volume_percent: Some(70),
            supports_volume: Some(true),
            kind: "computer".into(),
        },
        Device {
            id: Some("remote1".into()),
            name: "Kitchen speaker".into(),
            is_active: true,
            is_restricted: false,
            volume_percent: Some(62),
            supports_volume: Some(true),
            kind: "speaker".into(),
        },
        Device {
            id: Some("remote2".into()),
            name: "Pixel 9".into(),
            is_active: false,
            is_restricted: false,
            volume_percent: Some(40),
            supports_volume: Some(true),
            kind: "smartphone".into(),
        },
    ];
    // Include an unsigned ZeroConf receiver in the device picker.
    app.receivers = vec![crate::zeroconf::Receiver {
        name: "House Spotify".into(),
        device_id: Some("house-speaker".into()),
        address: std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 42)),
        port: 5555,
        path: "/zc".into(),
    }];
    app.remote = Some(RemoteSnapshot {
        state: PlaybackState {
            device: Some(app.devices[1].clone()),
            repeat_state: "off".into(),
            shuffle_state: true,
            context: Some(Context {
                uri: playlists[1].uri.clone(),
                kind: "playlist".into(),
            }),
            timestamp: 0,
            progress_ms: Some(83_000),
            is_playing: true,
            item: Some(PlayableItem::Track(tracks[0].clone())),
            currently_playing_type: Some("track".into()),
        },
        received_at: Instant::now(),
    });
    for track in &tracks {
        if let Some(id) = &track.id {
            app.track_cache.insert(id.clone(), track.clone());
        }
    }
}

/// Words to go with the sample track, timed so that the one being sung
/// sits mid-panel at the demo's playback position.
#[cfg(any(test, feature = "demo"))]
fn sample_lyrics() -> crate::lyrics::Lyrics {
    let lines = [
        (40_000, "Streetlights blinking down the river road"),
        (46_500, "Every window holding someone's evening"),
        (53_000, "I keep the radio low so you can sleep"),
        (59_500, "Counting mile markers like a rosary"),
        (66_000, "We left the city with the tank half full"),
        (72_500, "And a map that only shows the way back"),
        (79_000, "But the night is wide and the road is long"),
        (85_500, "And there's nowhere I would rather be"),
        (92_000, "Coffee going cold in the cup holder"),
        (98_500, "Your hand asleep on the gear stick"),
        (105_000, "Somewhere past the county line"),
        (111_500, "The stars come out to see us through"),
        (118_000, "Still the night is wide and the road is long"),
        (124_500, "And there's nowhere I would rather be"),
    ];
    crate::lyrics::Lyrics {
        lines: lines
            .iter()
            .map(|(at_ms, text)| crate::lyrics::Line {
                at_ms: Some(*at_ms),
                text: (*text).to_string(),
            })
            .collect(),
        synced: true,
        instrumental: false,
    }
}

/// Applies `--demo-page` and `--demo-show`.
#[cfg(feature = "demo")]
pub fn apply_flags(app: &mut App, page: Option<&str>, show: Option<&str>) {
    // Default screenshots to the main window regardless of saved settings.
    app.settings.winamp_window = false;
    if let Some(page) = page.and_then(Page::decode) {
        app.open(page);
    }
    for surface in show.unwrap_or("").split(',').map(str::trim) {
        match surface {
            "queue" => app.show_queue_panel = true,
            "playing-next" => {
                app.show_queue_panel = true;
                if let Loadable::Loaded(queue) = &app.queue {
                    app.manual_queue = queue
                        .queue
                        .iter()
                        .take(6)
                        .map(|item| item.uri().to_string())
                        .collect();
                }
            }
            "recents" => {
                app.show_queue_panel = true;
                app.queue_tab = QueueTab::Recents;
            }
            // Deterministic panel states for translation and layout reviews.
            "queue-empty" | "queue-loading" | "queue-error" => {
                app.show_queue_panel = true;
                app.queue = match surface {
                    "queue-loading" => Loadable::Loading,
                    "queue-error" => Loadable::Failed("Connection interrupted".into()),
                    _ => Loadable::Loaded(Queue::default()),
                };
            }
            "recents-empty" | "recents-loading" | "recents-error" => {
                app.show_queue_panel = true;
                app.queue_tab = QueueTab::Recents;
                app.recents.items.clear();
                app.recents_view.clear();
                app.recents.complete = true;
                app.recents.loaded_once = true;
                app.recents.loading = surface == "recents-loading";
                app.recents.error =
                    (surface == "recents-error").then(|| "Connection interrupted".into());
            }
            "lyrics-follow"
            | "lyrics-empty"
            | "lyrics-loading"
            | "lyrics-error"
            | "lyrics-instrumental"
            | "lyrics-no-playback" => {
                app.show_lyrics_panel = true;
                app.lyrics_uri = app.now_playing().map(|now| now.uri);
                app.lyrics_following = false;
                app.lyrics = match surface {
                    "lyrics-empty" => Loadable::Loaded(None),
                    "lyrics-loading" => Loadable::Loading,
                    "lyrics-error" => Loadable::Failed("Connection interrupted".into()),
                    _ => {
                        let mut lyrics = sample_lyrics();
                        lyrics.instrumental = surface == "lyrics-instrumental";
                        Loadable::Loaded(Some(lyrics))
                    }
                };
                if surface == "lyrics-no-playback" {
                    app.remote = None;
                }
            }
            "devices" => app.show_devices = true,
            "german" => app.locale = crate::i18n::Locale::German,
            "update" => {
                app.update = Some(crate::updates::Release {
                    version: "0.7.1".into(),
                    url: "https://spotifast.rocks/download/".into(),
                });
            }
            "personal-app" => app.dialog = Some(Dialog::PersonalAppIntro),
            "many-devices" => {
                app.show_devices = true;
                app.devices.extend((0..40).map(|index| Device {
                    id: Some(format!("speaker-{index}")),
                    name: format!("Speaker {index:02}"),
                    kind: "speaker".into(),
                    ..Default::default()
                }));
            }
            "shortcuts" => app.dialog = Some(Dialog::Shortcuts),
            "premium" => app.dialog = Some(Dialog::PremiumNeeded),
            "edit" => {
                app.dialog = Some(Dialog::EditPlaylist {
                    id: "pl1".into(),
                    name: "Long Way Home".into(),
                    description: "Songs for the road".into(),
                    public: Some(false),
                    cover: Default::default(),
                });
            }
            "create" => {
                app.dialog = Some(Dialog::CreatePlaylist {
                    name: "Autumn drives".into(),
                    public: false,
                    add_uris: vec!["spotify:track:trk1".into()],
                })
            }
            "duplicate" => {
                app.dialog = Some(Dialog::ConfirmPlaylistDuplicates {
                    position: None,
                    playlist_id: "pl1".into(),
                    playlist_name: "Long Way Home".into(),
                    items: vec![PlayableItem::Track(track(1))],
                    duplicate_uris: vec!["spotify:track:trk1".into()],
                })
            }
            "light" => {
                app.settings.theme = crate::settings::ThemeChoice::Light;
                app.actions.push(Action::SettingsChanged);
            }
            "dark" => {
                app.settings.theme = crate::settings::ThemeChoice::Dark;
                app.actions.push(Action::SettingsChanged);
            }
            "song-top-result" => {
                if let Loadable::Loaded(results) = &mut app.search.results {
                    results.artists = None;
                }
            }
            "finite-playlist" => {
                if let Some(page) = app.playlist_pages.get_mut("pl1") {
                    let seed = page.items.items.clone();
                    page.items.items = seed.iter().cycle().take(50).cloned().collect();
                    page.items.total = Some(1000);
                    page.items.next_offset = Some(50);
                    page.items.revision += 1;
                    if let Some(playlist) = page.playlist.get_mut() {
                        playlist.tracks = Some(crate::api::models::TrackCount { total: 1000 });
                    }
                }
            }
            "focus" => app.settings.sidebar_visible = false,
            // A cold start: no device is playing anything, and all the app
            // has is the song the last session ended on.
            "resume" => {
                app.remote = None;
                app.resume_context = Some("spotify:playlist:pl1".into());
                app.resume_track = Some("spotify:track:trk0".into());
                app.resume_position_ms = 19_566;
            }
            // The same cold start, one press of Next in: the song moved on
            // and nothing started playing.
            "resume-next" => {
                app.remote = None;
                app.resume_context = Some("spotify:playlist:pl1".into());
                app.resume_track = Some("spotify:track:trk0".into());
                app.resume_position_ms = 19_566;
                app.actions.push(Action::Next);
            }
            // Use the built-in skin for deterministic screenshots.
            "winamp" => {
                app.settings.winamp_window = true;
                app.settings.skin = None;
            }
            "playlist" => app.settings.playlist_open = true,
            "shade" => app.settings.winamp_shaded = true,
            "playlist-shade" => app.settings.playlist_shaded = true,
            "eq" => {
                app.settings.eq_open = true;
                app.settings.eq_on = true;
                app.settings.eq_bands_db = crate::eq::PRESETS[13].bands_db;
            }
            "presets" => app.winamp.open_presets = true,
            "art" => app.settings.art_expanded = true,
            "folders" => {
                use crate::player::RootlistEntry;
                let uri = |index: usize| format!("spotify:playlist:pl{index}");
                app.rootlist = vec![
                    RootlistEntry::FolderStart {
                        id: "f1".into(),
                        name: "Focus".into(),
                    },
                    RootlistEntry::Playlist(uri(1)),
                    RootlistEntry::Playlist(uri(2)),
                    RootlistEntry::FolderEnd,
                    RootlistEntry::FolderStart {
                        id: "f2".into(),
                        name: "Weekend".into(),
                    },
                    RootlistEntry::Playlist(uri(3)),
                    RootlistEntry::FolderEnd,
                    RootlistEntry::Playlist(uri(4)),
                    RootlistEntry::Playlist(uri(5)),
                ];
                app.collapsed_folders = vec!["f2".into()];
            }
            "small" => app.settings.skin_scale = Some(1),
            "windows-taskbar" => app.demo_windows_controls = true,
            "compact" => {
                app.settings.sidebar_compact = true;
                app.settings.tracklist_compact = true;
            }
            "eq-shade" => {
                app.settings.eq_open = true;
                app.settings.eq_shaded = true;
            }
            "milkdrop" => app.settings.milkdrop_open = true,
            "pins" => {
                app.settings.pinned_contexts =
                    vec!["spotify:playlist:pl2".into(), "spotify:playlist:pl4".into()];
            }
            "sorted" => {
                app.table_sorts.insert(
                    Page::Playlist("pl1".into()),
                    crate::model::TableSort {
                        column: crate::model::SortColumn::Added,
                        ascending: false,
                    },
                );
            }
            "lyrics" | "lyrics-fullscreen" => {
                app.lyrics_uri = app.now_playing().map(|now| now.uri);
                app.lyrics = Loadable::Loaded(Some(sample_lyrics()));
                app.lyrics_following = true;
                app.show_lyrics_panel = true;
                if surface == "lyrics-fullscreen" {
                    app.actions.push(Action::SetLyricsFullscreen(true));
                }
            }
            // Titles in scripts the interface font does not cover.
            "scripts" => {
                let titles = [
                    ("\u{591c}\u{306b}\u{99c6}\u{3051}\u{308b}", "YOASOBI"),
                    (
                        "\u{8d77}\u{98ce}\u{4e86}",
                        "\u{4e70}\u{8fa3}\u{6912}\u{4e5f}\u{7528}\u{5238}",
                    ),
                    (
                        "\u{bd04}\u{c5ec}\u{b984}\u{ac00}\u{c744}\u{aca8}\u{c6b8} (Still Life)",
                        "BIGBANG",
                    ),
                    (
                        "\u{6253}\u{4e0a}\u{82b1}\u{706b}",
                        "DAOKO, \u{7c73}\u{6d25}\u{7384}\u{5e2b}",
                    ),
                    (
                        "\u{5149}\u{5e74}\u{4e4b}\u{5916}",
                        "G.E.M. \u{9093}\u{7d2b}\u{68cb}",
                    ),
                    ("\u{bc24}\u{d3b8}\u{c9c0}", "IU"),
                    ("Lemon", "\u{7c73}\u{6d25}\u{7384}\u{5e2b}"),
                    (
                        "\u{7ea2}\u{8272}\u{9ad8}\u{8ddf}\u{978b}",
                        "\u{8521}\u{5065}\u{96c5}",
                    ),
                ];
                let rename = |track: &mut Track, (title, artist): (&str, &str)| {
                    track.name = title.to_string();
                    track.artists = vec![ArtistRef {
                        id: None,
                        name: artist.to_string(),
                        uri: None,
                    }];
                };
                if let Some(page) = app.playlist_pages.get_mut("pl1") {
                    for (entry, names) in page.items.items.iter_mut().zip(titles) {
                        if let Some(PlayableItem::Track(track)) = &mut entry.item {
                            rename(track, names);
                        }
                    }
                }
                if let Loadable::Loaded(queue) = &mut app.queue {
                    for (item, names) in queue.queue.iter_mut().zip(titles) {
                        if let PlayableItem::Track(track) = item {
                            rename(track, names);
                        }
                    }
                }
                if let Some(remote) = &mut app.remote
                    && let Some(PlayableItem::Track(track)) = &mut remote.state.item
                {
                    rename(track, titles[0]);
                }
                if let Some(track) = app.track_cache.get_mut("trk0") {
                    rename(track, titles[0]);
                }
                if let Loadable::Loaded(playlists) = &mut app.library.playlists {
                    let names = [
                        "\u{901a}\u{52e4}\u{306e}BGM",
                        "\u{7761}\u{524d}\u{6b4c}\u{5355}",
                        "\u{cd9c}\u{adfc}\u{ae38} \u{d50c}\u{b808}\u{c774}\u{b9ac}\u{c2a4}\u{d2b8}",
                    ];
                    for (playlist, name) in playlists.iter_mut().skip(3).zip(names) {
                        playlist.name = name.to_string();
                    }
                }
            }
            // A Spotify app of one's own, in use.
            "faster" => {
                let id = "8f2c1d0e4a6b4c3d9e7f5a1b2c3d4e5f".to_string();
                app.settings.web_client_id = Some(id.clone());
                app.web_app = Some(id);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppOptions;
    use crate::paths::AppDirs;
    use crate::settings::Settings;
    use std::sync::Arc;

    fn accessible_app(name: &str) -> (egui::Context, App) {
        let root =
            std::env::temp_dir().join(format!("fastpotify-a11y-{name}-{}", std::process::id()));
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
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
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        (ctx, app)
    }

    #[test]
    fn change_cover_button_dispatches_the_native_picker_action() {
        let (ctx, mut app) = accessible_app("cover-button");
        app.dialog = Some(Dialog::EditPlaylist {
            id: "pl1".into(),
            name: "Test".into(),
            description: String::new(),
            public: Some(false),
            cover: Default::default(),
        });
        let _ = accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let button = accessible_node(&tree, "Change cover", egui::accesskit::Role::Button);
        let _ = accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(
                button,
                egui::accesskit::Action::Click,
                None,
            )],
        );
        let Some(Dialog::EditPlaylist { cover, .. }) = &app.dialog else {
            panic!()
        };
        assert!(cover.request.is_some());
        app.backend.shutdown();
    }

    #[test]
    fn cover_editing_fits_large_and_narrow_windows_in_both_themes() {
        for (width, height) in [(1240.0, 800.0), (760.0, 520.0)] {
            for light in [false, true] {
                let (ctx, mut app) = accessible_app("cover-dialog");
                app.open(Page::Playlist("pl1".into()));
                app.dialog = Some(Dialog::EditPlaylist {
                    id: "pl1".into(),
                    name: "Test".into(),
                    description: String::new(),
                    public: Some(false),
                    cover: Default::default(),
                });
                if light {
                    app.settings.theme = crate::settings::ThemeChoice::Light;
                    app.actions.push(Action::SettingsChanged);
                }
                if let Some(Dialog::EditPlaylist { cover, .. }) = &mut app.dialog {
                    cover.error = Some("Spotify refused this cover. Check that you own the playlist, then sign in again to grant image upload permission.".into());
                }
                for _ in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, height),
                            )),
                            ..Default::default()
                        },
                        |ui| app.frame_ui(ui),
                    );
                    output.textures_delta.clear();
                }
                let rect = app.dialog_rect.unwrap();
                assert!(rect.left() >= 0.0 && rect.top() >= 0.0, "{rect:?}");
                assert!(rect.right() <= width && rect.bottom() <= height, "{rect:?}");
                app.backend.shutdown();
            }
        }
    }

    fn accessible_frame(
        ctx: &egui::Context,
        app: &mut App,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.frame_ui(ui),
        );
        output.textures_delta.clear();
        output
            .platform_output
            .accesskit_update
            .expect("screen-reader tree")
    }

    fn accessible_node(
        tree: &egui::accesskit::TreeUpdate,
        label: &str,
        role: egui::accesskit::Role,
    ) -> egui::accesskit::NodeId {
        tree.nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label) && node.role() == role)
            .unwrap_or_else(|| panic!("missing {label:?} with role {role:?}"))
            .0
    }

    fn accessible_action(
        target: egui::accesskit::NodeId,
        action: egui::accesskit::Action,
        data: Option<egui::accesskit::ActionData>,
    ) -> egui::Event {
        egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            target_tree: egui::accesskit::TreeId::ROOT,
            target_node: target,
            action,
            data,
        })
    }

    fn keyboard(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn accessible_navigation_and_pause_work_without_a_pointer() {
        use egui::accesskit::{Action, Role};
        let (ctx, mut app) = accessible_app("navigate");
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let liked = accessible_node(&tree, "Liked Songs", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(liked, Action::Click, None)],
        );
        assert_eq!(app.page(), &Page::LikedSongs);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let pause = accessible_node(&tree, "Pause", Role::Button);
        assert!(app.believed_playing());
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(pause, Action::Focus, None)],
        );
        let tree = accessible_frame(
            &ctx,
            &mut app,
            vec![keyboard(egui::Key::Space, egui::Modifiers::NONE)],
        );
        assert!(
            !app.believed_playing(),
            "focused Space must pause once, without firing the global shortcut too"
        );
        assert_eq!(tree.focus, pause);
        app.backend.shutdown();
    }

    #[test]
    fn translated_sidebar_keeps_keyboard_navigation_and_accessible_names() {
        use crate::i18n::{Locale, gettext};
        use clap::ValueEnum;
        use egui::accesskit::{Action as AccessibleAction, Role};
        for &locale in Locale::value_variants() {
            let (ctx, mut app) = accessible_app(&format!("translated-sidebar-{locale:?}"));
            app.locale = locale;
            accessible_frame(&ctx, &mut app, vec![]);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let home = accessible_node(&tree, &gettext(locale, "Home"), Role::Button);
            let search = accessible_node(&tree, &gettext(locale, "Search"), Role::Button);
            accessible_node(&tree, &gettext(locale, "Create a playlist"), Role::Button);
            accessible_node(&tree, &gettext(locale, "Albums"), Role::Button);
            accessible_node(&tree, &gettext(locale, "Artists"), Role::Button);
            let liked = accessible_node(&tree, &gettext(locale, "Liked Songs"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(home, AccessibleAction::Focus, None)],
            );
            let tree = accessible_frame(
                &ctx,
                &mut app,
                vec![keyboard(egui::Key::Tab, egui::Modifiers::NONE)],
            );
            assert_eq!(tree.focus, search, "Tab order must survive translation");
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(liked, AccessibleAction::Focus, None)],
            );
            accessible_frame(
                &ctx,
                &mut app,
                vec![keyboard(egui::Key::Enter, egui::Modifiers::NONE)],
            );
            assert_eq!(app.page(), &Page::LikedSongs);

            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let library_search =
                accessible_node(&tree, &gettext(locale, "Search Your Library"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(
                    library_search,
                    AccessibleAction::Click,
                    None,
                )],
            );
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            accessible_node(
                &tree,
                &gettext(locale, "Search in Your Library"),
                Role::TextInput,
            );
            app.library.filter = gettext(locale, "Liked Songs").to_uppercase();
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            accessible_node(&tree, &gettext(locale, "Liked Songs"), Role::Button);
            assert!(
                !tree
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("Discover Weekly"))
            );
            app.backend.shutdown();
        }
    }

    #[test]
    fn translated_player_bar_keeps_control_identity_and_keyboard_actions() {
        use crate::i18n::{Locale, gettext};
        use clap::ValueEnum;
        use egui::accesskit::{Action as AccessibleAction, Role};

        for &locale in Locale::value_variants() {
            let (ctx, mut app) = accessible_app(&format!("translated-player-{locale:?}"));
            accessible_frame(&ctx, &mut app, vec![]);
            let english = accessible_frame(&ctx, &mut app, vec![]);
            let controls = [
                ("Shuffle", Role::CheckBox),
                ("Previous", Role::Button),
                ("Pause", Role::Button),
                ("Next", Role::Button),
                ("Repeat", Role::Button),
                ("Playback position (%)", Role::Slider),
                ("Volume (%)", Role::Slider),
                ("Mute", Role::Button),
                ("Connect to a device", Role::Button),
                ("Queue", Role::Button),
                ("Lyrics", Role::Button),
            ];
            let ids: Vec<_> = controls
                .iter()
                .map(|(source, role)| accessible_node(&english, source, *role))
                .collect();
            let pause = accessible_node(&english, "Pause", Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(pause, AccessibleAction::Focus, None)],
            );
            app.locale = locale;
            let translated = accessible_frame(&ctx, &mut app, vec![]);
            assert_eq!(translated.focus, pause, "translation must retain focus");
            for ((source, role), id) in controls.iter().zip(ids) {
                assert_eq!(
                    accessible_node(&translated, &gettext(locale, source), *role),
                    id,
                    "translation must retain the {source} control"
                );
            }

            // A reported pause uses the same control with its new label.
            // Offline demo requests have no backend acknowledgment, so a
            // requested remote pause would keep the pending spinner visible.
            app.remote.as_mut().unwrap().state.is_playing = false;
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            assert_eq!(
                accessible_node(&tree, &gettext(locale, "Play"), Role::Button),
                pause
            );
            app.remote.as_mut().unwrap().state.is_playing = true;
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let volume = accessible_node(&tree, &gettext(locale, "Volume (%)"), Role::Slider);
            let before = app.now_playing().unwrap().volume_percent;
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(volume, AccessibleAction::Focus, None)],
            );
            accessible_frame(
                &ctx,
                &mut app,
                vec![keyboard(egui::Key::ArrowRight, egui::Modifiers::NONE)],
            );
            assert_eq!(app.now_playing().unwrap().volume_percent, before + 5);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(pause, AccessibleAction::Focus, None)],
            );
            accessible_frame(
                &ctx,
                &mut app,
                vec![keyboard(egui::Key::Space, egui::Modifiers::NONE)],
            );
            assert!(!app.believed_playing());
            app.backend.shutdown();
        }
    }

    #[test]
    fn translated_player_bar_labels_follow_control_state_and_empty_playback() {
        use crate::i18n::{Locale, gettext};
        use clap::ValueEnum;
        use egui::accesskit::{Action as AccessibleAction, Role};

        for &locale in Locale::value_variants() {
            let (ctx, mut app) = accessible_app(&format!("translated-player-state-{locale:?}"));
            app.locale = locale;
            accessible_frame(&ctx, &mut app, vec![]);
            for (source, next) in [
                ("Repeat", "Repeat one"),
                ("Repeat one", "Repeat off"),
                ("Repeat off", "Repeat"),
                ("Mute", "Unmute"),
                ("Unmute", "Mute"),
                ("Remove from Liked Songs", "Save to Liked Songs"),
                ("Save to Liked Songs", "Remove from Liked Songs"),
            ] {
                let tree = accessible_frame(&ctx, &mut app, vec![]);
                let button = accessible_node(&tree, &gettext(locale, source), Role::Button);
                accessible_frame(
                    &ctx,
                    &mut app,
                    vec![accessible_action(button, AccessibleAction::Click, None)],
                );
                let tree = accessible_frame(&ctx, &mut app, vec![]);
                accessible_node(&tree, &gettext(locale, next), Role::Button);
            }

            app.remote = None;
            assert!(app.now_playing().is_none());
            let render = |app: &mut App, ui: &mut egui::Ui| crate::ui::player_bar::show(app, ui);
            view_frame(&ctx, &mut app, vec![], render);
            let painted = view_frame(&ctx, &mut app, vec![], render);
            for source in ["Nothing playing", "Pick a song, album, or playlist"] {
                assert!(
                    painted
                        .iter()
                        .any(|(text, _)| text == &gettext(locale, source)),
                    "missing translated empty playback label: {source}"
                );
            }
            app.backend.shutdown();
        }
    }

    #[test]
    fn translated_queue_controls_preserve_manual_and_context_rows() {
        use crate::i18n::{Locale, gettext};
        use clap::ValueEnum;
        use egui::accesskit::{Action as AccessibleAction, Role};

        for &locale in Locale::value_variants() {
            let (ctx, mut app) = accessible_app(&format!("translated-queue-{locale:?}"));
            app.locale = locale;
            app.show_queue_panel = true;
            app.local_ready = true;
            app.local.track = Some(crate::player::LocalTrack {
                uri: app.now_playing().unwrap().uri,
                ..Default::default()
            });
            app.local.playback = crate::player::Playback::Paused;
            let rows = app.queue.get().unwrap().queue.clone();
            app.manual_queue = rows[..2]
                .iter()
                .map(|item| item.uri().to_string())
                .collect();
            let saved_uris = app.queue_playlist_uris();
            accessible_frame(&ctx, &mut app, vec![]);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let save = accessible_node(&tree, &gettext(locale, "Save as a playlist"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(save, AccessibleAction::Click, None)],
            );
            let Some(Dialog::CreatePlaylist {
                name,
                public,
                add_uris,
            }) = &app.dialog
            else {
                panic!("saving the translated queue must open the existing playlist dialog");
            };
            assert_eq!(name, &app.queue_playlist_name());
            assert!(!public);
            assert_eq!(add_uris, &saved_uris);
            app.dialog = None;
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let clear = accessible_node(&tree, &gettext(locale, "Clear queue"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(clear, AccessibleAction::Click, None)],
            );
            assert!(app.manual_queue.is_empty());
            assert_eq!(app.queue.get().unwrap().queue, rows[2..]);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let recent = accessible_node(&tree, &gettext(locale, "Recent"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(recent, AccessibleAction::Click, None)],
            );
            assert_eq!(app.queue_tab, QueueTab::Recents);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let close = accessible_node(&tree, &gettext(locale, "Close"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(close, AccessibleAction::Click, None)],
            );
            assert!(!app.show_queue_panel);
            app.backend.shutdown();
        }
    }

    #[test]
    fn translated_lyrics_controls_keep_follow_retry_and_fullscreen_actions() {
        use crate::i18n::{Locale, gettext, pgettext};
        use clap::ValueEnum;
        use egui::accesskit::{Action as AccessibleAction, Role};

        for &locale in Locale::value_variants() {
            let (ctx, mut app) = accessible_app(&format!("translated-lyrics-{locale:?}"));
            app.locale = locale;
            app.show_lyrics_panel = true;
            app.lyrics_uri = app.now_playing().map(|now| now.uri);
            app.lyrics = Loadable::Loaded(Some(sample_lyrics()));
            app.lyrics_following = false;
            accessible_frame(&ctx, &mut app, vec![]);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let follow =
                accessible_node(&tree, &pgettext(locale, "lyrics", "Follow"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(follow, AccessibleAction::Click, None)],
            );
            assert!(app.lyrics_following);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let expand =
                accessible_node(&tree, &gettext(locale, "Full screen lyrics"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(expand, AccessibleAction::Click, None)],
            );
            assert!(app.lyrics_fullscreen.is_some());
            for source in ["Reduce lyrics motion", "Enable lyrics motion"] {
                let before = app.lyrics_reduce_motion;
                let tree = accessible_frame(&ctx, &mut app, vec![]);
                let motion = accessible_node(&tree, &gettext(locale, source), Role::Button);
                accessible_frame(
                    &ctx,
                    &mut app,
                    vec![accessible_action(motion, AccessibleAction::Click, None)],
                );
                assert_ne!(app.lyrics_reduce_motion, before);
            }
            app.lyrics_following = false;
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let follow =
                accessible_node(&tree, &pgettext(locale, "lyrics", "Follow"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(follow, AccessibleAction::Click, None)],
            );
            assert!(app.lyrics_following);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let leave = accessible_node(
                &tree,
                &gettext(locale, "Leave full screen (Esc)"),
                Role::Button,
            );
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(leave, AccessibleAction::Click, None)],
            );
            assert!(app.lyrics_fullscreen.is_none());
            for fullscreen in [false, true] {
                app.lyrics_fullscreen = fullscreen.then_some(false);
                app.lyrics = Loadable::Failed("fixture failure".into());
                let tree = accessible_frame(&ctx, &mut app, vec![]);
                let retry = accessible_node(&tree, &gettext(locale, "Try again"), Role::Button);
                accessible_frame(
                    &ctx,
                    &mut app,
                    vec![accessible_action(retry, AccessibleAction::Click, None)],
                );
                // Offline retries finish without contacting a lyrics provider.
                assert!(matches!(app.lyrics, Loadable::Loaded(None)));
            }
            app.lyrics_fullscreen = None;
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let close = accessible_node(&tree, &gettext(locale, "Close"), Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(close, AccessibleAction::Click, None)],
            );
            assert!(!app.show_lyrics_panel);
            app.backend.shutdown();
        }
    }

    #[test]
    fn translated_panel_states_keep_original_lyrics_and_failure_details() {
        use crate::i18n::{Locale, gettext};
        use clap::ValueEnum;
        for &locale in Locale::value_variants() {
            let (ctx, mut app) = accessible_app(&format!("translated-panel-states-{locale:?}"));
            app.locale = locale;
            app.show_lyrics_panel = true;
            app.lyrics_uri = app.now_playing().map(|now| now.uri);
            for fullscreen in [false, true] {
                app.lyrics_fullscreen = fullscreen.then_some(false);
                let view = |app: &mut App, ui: &mut egui::Ui| app.frame_ui(ui);
                let mut instrumental = sample_lyrics();
                instrumental.instrumental = true;
                for (state, messages) in [
                    (
                        Loadable::Loading,
                        vec![gettext(locale, "Loading…").into_owned()],
                    ),
                    (
                        Loadable::Failed("fixture {count} 42".into()),
                        vec![
                            gettext(locale, "Couldn't fetch the lyrics: {error}")
                                .replace("{error}", "fixture {count} 42"),
                        ],
                    ),
                    (
                        Loadable::Loaded(None),
                        vec![
                            gettext(locale, "No lyrics").into_owned(),
                            gettext(locale, "No lyrics found for this track.").into_owned(),
                        ],
                    ),
                    (
                        Loadable::Loaded(Some(instrumental)),
                        vec![
                            gettext(locale, "Instrumental").into_owned(),
                            gettext(locale, "No timed lyrics for this track.").into_owned(),
                        ],
                    ),
                    (
                        Loadable::Loaded(Some(sample_lyrics())),
                        vec![sample_lyrics().lines[0].text.clone()],
                    ),
                ] {
                    app.lyrics = state;
                    app.lyrics_following = false;
                    view_frame(&ctx, &mut app, vec![], view);
                    let painted = view_frame(&ctx, &mut app, vec![], view);
                    for message in messages {
                        assert!(
                            painted.iter().any(|(text, _)| text == &message),
                            "{locale:?}: missing {message}"
                        );
                    }
                }
            }
            for (state, messages) in [
                (
                    Loadable::Loading,
                    vec![gettext(locale, "Loading…").into_owned()],
                ),
                (
                    Loadable::Failed("fixture {count} 42".into()),
                    vec![
                        "fixture {count} 42".into(),
                        gettext(locale, "Retry").into_owned(),
                    ],
                ),
                (
                    Loadable::Loaded(Queue::default()),
                    vec![
                        gettext(locale, "Nothing queued").into_owned(),
                        gettext(locale, "Queued songs appear here.").into_owned(),
                    ],
                ),
            ] {
                app.queue = state;
                view_frame(&ctx, &mut app, vec![], crate::ui::queue::page);
                let painted = view_frame(&ctx, &mut app, vec![], crate::ui::queue::page);
                for message in messages {
                    assert!(
                        painted.iter().any(|(text, _)| text == &message),
                        "{locale:?}: missing {message}"
                    );
                }
            }
            app.queue_tab = QueueTab::Recents;
            app.recents.items.clear();
            app.recents_view.clear();
            app.recents.loaded_once = true;
            app.recents.complete = true;
            for (loading, error, sources) in [
                (
                    false,
                    None,
                    vec!["No recent plays", "Played songs appear here."],
                ),
                (true, None, vec!["Loading…"]),
                (false, Some("fixture failure".into()), vec!["Retry"]),
            ] {
                app.recents.loading = loading;
                app.recents.error = error;
                view_frame(&ctx, &mut app, vec![], crate::ui::queue::side_panel);
                let painted = view_frame(&ctx, &mut app, vec![], crate::ui::queue::side_panel);
                for source in sources {
                    assert!(
                        painted
                            .iter()
                            .any(|(text, _)| text == &gettext(locale, source))
                    );
                }
                if app.recents.error.is_some() {
                    let retry = painted
                        .iter()
                        .find(|(text, _)| text == &gettext(locale, "Retry"))
                        .unwrap()
                        .1
                        .center();
                    app.actions.clear();
                    view_frame(
                        &ctx,
                        &mut app,
                        pointer_click(retry, egui::PointerButton::Primary),
                        crate::ui::queue::side_panel,
                    );
                    assert!(
                        app.actions
                            .iter()
                            .any(|action| matches!(action, crate::model::Action::ReloadRecents))
                    );
                }
            }
            app.backend.shutdown();
        }
    }

    #[test]
    fn library_sort_menu_preserves_saved_order_and_keyboard_activation() {
        use crate::settings::{LibraryShelf, LibrarySort};
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("library-sort");
        app.settings.sidebar_order =
            vec!["spotify:playlist:pl4".into(), "spotify:playlist:pl1".into()];
        let saved = app.settings.sidebar_order.clone();
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let sort = accessible_node(&tree, "Local custom order", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(sort, AccessibleAction::Click, None)],
        );
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        assert!(
            !tree
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Recently added"))
        );
        let name = accessible_node(&tree, "Name", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(name, AccessibleAction::Focus, None)],
        );
        accessible_frame(
            &ctx,
            &mut app,
            vec![keyboard(egui::Key::Enter, egui::Modifiers::NONE)],
        );
        assert_eq!(
            app.settings.library_sort.get(&LibraryShelf::Playlists),
            Some(&LibrarySort::Name)
        );
        assert_eq!(app.settings.sidebar_order, saved);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let sort = accessible_node(&tree, "Name", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(sort, AccessibleAction::Click, None)],
        );
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let local = accessible_node(&tree, "Local custom order", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(local, AccessibleAction::Click, None)],
        );
        assert_eq!(
            app.settings.library_sort.get(&LibraryShelf::Playlists),
            Some(&LibrarySort::Local)
        );
        assert_eq!(app.settings.sidebar_order, saved);
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&app.settings).unwrap()).unwrap();
        assert_eq!(restored.library_sort, app.settings.library_sort);
        assert_eq!(restored.sidebar_order, saved);
        app.backend.shutdown();
    }

    #[test]
    fn library_sorts_finish_paging_without_retrying_failed_pages() {
        use crate::settings::{LibraryShelf, LibrarySort};
        for (shelf, label, page) in [
            (LibraryShelf::Albums, "Albums", Page::Albums),
            (LibraryShelf::Artists, "Artists", Page::Artists),
            (LibraryShelf::Podcasts, "Podcasts", Page::Podcasts),
        ] {
            let (ctx, mut app) = accessible_app(&format!("library-sort-paging-{shelf:?}"));
            let view = crate::ui::sidebar::show;
            app.settings.library_sort.insert(shelf, LibrarySort::Name);
            app.library.albums.next_offset = Some(50);
            app.library.artists.complete = false;
            app.library.artists.after = Some("next".into());
            app.library.shows.next_offset = Some(50);
            view_frame(&ctx, &mut app, vec![], view);
            let painted = view_frame(&ctx, &mut app, vec![], view);
            let position = painted
                .iter()
                .find(|(text, _)| text == label)
                .unwrap()
                .1
                .center();
            view_frame(
                &ctx,
                &mut app,
                pointer_click(position, egui::PointerButton::Primary),
                view,
            );
            app.actions.clear();
            view_frame(&ctx, &mut app, vec![], view);
            assert!(
                app.actions
                    .iter()
                    .any(|action| matches!(action, Action::LoadMore(found) if *found == page))
            );
            app.actions.clear();
            app.library.albums.error = Some("Try again later".into());
            app.library.artists.error = Some("Try again later".into());
            app.library.shows.error = Some("Try again later".into());
            for _ in 0..3 {
                view_frame(&ctx, &mut app, vec![], view);
                assert!(
                    !app.actions
                        .iter()
                        .any(|action| matches!(action, Action::LoadMore(found) if *found == page)),
                    "failed pages must not retry every frame"
                );
            }
            app.backend.shutdown();
        }
    }

    #[test]
    fn personal_app_intro_can_be_dismissed_or_open_setup_with_keyboard_focus() {
        use egui::accesskit::{Action as AccessibleAction, Role};

        for setup in [false, true] {
            let (ctx, mut app) = accessible_app(&format!("personal-app-intro-{setup}"));
            app.dialog = Some(Dialog::PersonalAppIntro);
            accessible_frame(&ctx, &mut app, vec![]);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let button = accessible_node(
                &tree,
                if setup {
                    "Set up personal app"
                } else {
                    "Keep shared app"
                },
                Role::Button,
            );
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(button, AccessibleAction::Click, None)],
            );
            assert!(app.dialog.is_none());
            assert!(app.settings.personal_app_intro_seen);
            if setup {
                assert_eq!(app.page(), &Page::Settings);
                accessible_frame(&ctx, &mut app, vec![]);
                assert!(
                    ctx.memory(|memory| memory.has_focus(egui::Id::new("personal-web-client-id")))
                );
            }
            app.backend.shutdown();
        }
        let (ctx, mut app) = accessible_app("personal-app-intro-escape");
        app.dialog = Some(Dialog::PersonalAppIntro);
        accessible_frame(&ctx, &mut app, vec![]);
        accessible_frame(
            &ctx,
            &mut app,
            vec![keyboard(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(app.dialog.is_none());
        assert!(app.settings.personal_app_intro_seen);
        app.backend.shutdown();
    }

    fn settings_filter(ctx: &egui::Context) -> String {
        ctx.data_mut(|data| {
            data.get_temp::<String>(egui::Id::new("settings-filter"))
                .unwrap_or_default()
        })
    }

    #[test]
    fn update_window_keeps_downloads_running_and_waits_for_restart() {
        use crate::updates::{
            DownloadState,
            install::{Installation, Kind, Prepared},
        };
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("update-window");
        app.update = Some(crate::updates::Release {
            version: "9.9.9".into(),
            url: "https://example.invalid/release".into(),
        });
        let installation = Installation {
            executable: std::path::PathBuf::from("/test/fastpotify"),
            kind: Kind::Portable,
        };
        app.update_support = Some(Ok(installation.clone()));
        assert!(!app.show_update);
        app.actions.push(Action::ShowUpdate);
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let download = accessible_node(&tree, "Download update", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(download, AccessibleAction::Click, None)],
        );
        assert!(matches!(
            app.update_download,
            DownloadState::Downloading { .. }
        ));
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let close = accessible_node(&tree, "Close update", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(close, AccessibleAction::Click, None)],
        );
        assert!(!app.show_update);
        assert!(matches!(
            app.update_download,
            DownloadState::Downloading { .. }
        ));
        app.update_download = DownloadState::Ready(Box::new(Prepared {
            installation,
            directory: "/test/stage".into(),
            payload: "/test/stage/payload".into(),
            version: "9.9.9".into(),
            sha256: String::new(),
        }));
        accessible_frame(&ctx, &mut app, vec![]);
        assert!(!app.show_update);
        assert!(matches!(app.update_download, DownloadState::Ready(_)));
        app.actions.push(Action::ShowUpdate);
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let restart = accessible_node(&tree, "Restart to update", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(restart, AccessibleAction::Click, None)],
        );
        assert!(matches!(app.update_download, DownloadState::Installing));
        app.backend.shutdown();
    }

    #[test]
    fn the_cross_on_a_recent_search_forgets_only_that_query() {
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("search-history");
        app.open(Page::Search);
        // Recent searches stand in for results only while nothing is searched
        app.search.query.clear();
        app.search.committed.clear();
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let forget = accessible_node(&tree, "Remove ambient", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(forget, AccessibleAction::Click, None)],
        );
        assert_eq!(app.settings.search_history, ["Khruangbin", "Rework"]);
        assert!(
            app.search.committed.is_empty(),
            "the cross must forget a query without running it"
        );
        app.backend.shutdown();
    }

    #[test]
    fn settings_search_filters_rows_clearing_and_empty_state() {
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("settings-search");
        app.open(Page::Settings);
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let field = accessible_node(&tree, "Search settings", Role::TextInput);
        // Typing narrows the page to matching rows.
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(field, AccessibleAction::Focus, None)],
        );
        let tree = accessible_frame(&ctx, &mut app, vec![egui::Event::Text("volume".into())]);
        assert_eq!(settings_filter(&ctx), "volume");
        accessible_node(&tree, "Normalize volume", Role::CheckBox);
        assert!(
            !tree
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Compact track list")),
            "non-matching rows stay hidden while filtering"
        );
        // Clearing the field brings every row back.
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let clear = accessible_node(&tree, "Clear", Role::Button);
        let tree = accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(clear, AccessibleAction::Click, None)],
        );
        assert!(settings_filter(&ctx).is_empty());
        accessible_node(&tree, "Compact track list", Role::CheckBox);
        // Gibberish matches nothing: every section's controls disappear
        // and the empty state takes the page. (Plain labels expose no
        // accesskit name, so the empty state itself is covered through
        // the absence of each section's controls.)
        let field = accessible_node(&tree, "Search settings", Role::TextInput);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(field, AccessibleAction::Focus, None)],
        );
        let tree = accessible_frame(&ctx, &mut app, vec![egui::Event::Text("zzznope".into())]);
        assert_eq!(settings_filter(&ctx), "zzznope");
        for (label, role) in [
            ("Sign out", Role::Button),
            ("Normalize volume", Role::CheckBox),
            ("Dark", Role::Button),
            ("Switch to it", Role::Button),
            ("MilkDrop window", Role::CheckBox),
            ("Equalizer", Role::CheckBox),
            ("Clear artwork", Role::Button),
            ("Check for updates", Role::Button),
        ] {
            assert!(
                tree.nodes
                    .iter()
                    .all(|(_, node)| !(node.label() == Some(label) && node.role() == role)),
                "{label} is hidden when nothing matches"
            );
        }
        app.backend.shutdown();
    }

    #[test]
    fn personal_app_setup_clears_a_saved_settings_search() {
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("settings-search-setup");
        app.open(Page::Settings);
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let field = accessible_node(&tree, "Search settings", Role::TextInput);
        // Search for something that hides the Account section...
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(field, AccessibleAction::Focus, None)],
        );
        accessible_frame(&ctx, &mut app, vec![egui::Event::Text("theme".into())]);
        assert_eq!(settings_filter(&ctx), "theme");
        // ...leave Settings...
        app.open(Page::Home);
        accessible_frame(&ctx, &mut app, vec![]);
        // ...and enter Personal App setup, which must reveal and focus
        // the Client ID field instead of landing on a filtered-out row.
        app.dialog = Some(Dialog::PersonalAppIntro);
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let setup = accessible_node(&tree, "Set up personal app", Role::Button);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(setup, AccessibleAction::Click, None)],
        );
        assert_eq!(app.page(), &Page::Settings);
        assert!(
            settings_filter(&ctx).is_empty(),
            "setup must drop the saved search so its row is visible"
        );
        accessible_frame(&ctx, &mut app, vec![]);
        assert!(
            ctx.memory(|memory| memory.has_focus(egui::Id::new("personal-web-client-id"))),
            "the Client ID field takes focus once its row is visible"
        );
        app.backend.shutdown();
    }

    fn settings_text(ctx: &egui::Context, app: &mut App, query: &str) -> Vec<String> {
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("settings-filter"), query.to_owned());
        });
        view_frame(ctx, app, vec![], crate::ui::settings::show);
        view_frame(ctx, app, vec![], crate::ui::settings::show)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    #[test]
    fn settings_search_finds_complete_descriptions_and_current_status() {
        let (ctx, mut app) = accessible_app("settings-search-text");
        for (query, row) in [
            ("without a cover", "Compact track list"),
            ("Ctrl+0", "Interface zoom"),
            ("Rust", "About"),
            (
                "Downloads in the background",
                "Download updates automatically",
            ),
        ] {
            let query = if cfg!(target_os = "macos") && query == "Ctrl+0" {
                "Cmd+0"
            } else {
                query
            };
            let text = settings_text(&ctx, &mut app, query);
            assert!(text.iter().any(|text| text == row), "{query}: {text:?}");
        }
        for query in ["Rodio", "ALSA"] {
            let text = settings_text(&ctx, &mut app, query);
            assert_eq!(
                text.iter().any(|text| text == "Audio output"),
                cfg!(target_os = "linux")
            );
            assert_eq!(
                text.iter().any(|text| text == "Playback on this computer"),
                cfg!(target_os = "linux")
            );
        }
        app.local_playback =
            crate::backend::LocalPlayback::Failed("Test connection failure".into());
        let text = settings_text(&ctx, &mut app, "connection failure");
        assert!(text.iter().any(|text| text == "Status: Unavailable"));
        app.backend.shutdown();
    }

    #[test]
    fn settings_search_never_shows_a_section_for_an_unavailable_row() {
        let (ctx, mut app) = accessible_app("settings-search-availability");
        let text = settings_text(&ctx, &mut app, "Show in taskbar");
        assert_eq!(
            text.iter().any(|text| text == "Winamp skins"),
            cfg!(windows)
        );
        if !cfg!(windows) {
            assert!(text.iter().any(|text| text.starts_with("No settings for")));
        }
        app.demo_windows_controls = true;
        let text = settings_text(&ctx, &mut app, "Show in taskbar");
        assert!(text.iter().any(|text| text == "Winamp skins"));
        assert!(text.iter().any(|text| text == "Show in taskbar"));

        app.settings.web_client_id = None;
        app.web_app = None;
        let text = settings_text(&ctx, &mut app, "Personal app ready");
        assert!(!text.iter().any(|text| text == "Account"));
        assert!(text.iter().any(|text| text.starts_with("No settings for")));
        app.settings.web_client_id = Some("test-client".into());
        app.web_app = Some("test-client".into());
        let text = settings_text(&ctx, &mut app, "Personal app ready");
        assert!(text.iter().any(|text| text == "Personal app ready"));
        app.backend.shutdown();
    }

    #[test]
    fn settings_search_keeps_apply_available_after_a_playback_edit() {
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("settings-search-apply");
        app.open(Page::Settings);
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("settings-filter"), "normalize".to_string())
        });
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let toggle = accessible_node(&tree, "Normalize volume", Role::CheckBox);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(toggle, AccessibleAction::Click, None)],
        );
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        accessible_node(&tree, "Apply and restart playback", Role::Button);
        app.backend.shutdown();
    }

    #[test]
    fn accessible_sliders_accept_keyboard_and_screen_reader_values() {
        use crate::ui::widgets::{SliderEvent, thin_slider};
        use egui::accesskit::{Action, ActionData, Role};
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::theme::install(&ctx);
        let palette = crate::theme::Palette::dark();
        let mut value = 0.5;
        let mut render = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    if let SliderEvent::Committed(next) = thin_slider(
                        ui,
                        &palette,
                        egui::Id::new("test-volume"),
                        "Volume (%)",
                        value,
                        200.0,
                        Some(0.05),
                    ) {
                        value = next;
                    }
                },
            );
            output.textures_delta.clear();
            (value, output.platform_output.accesskit_update.unwrap())
        };
        let (_, tree) = render(vec![]);
        let slider = accessible_node(&tree, "Volume (%)", Role::Slider);
        render(vec![accessible_action(slider, Action::Focus, None)]);
        let (value, _) = render(vec![keyboard(egui::Key::ArrowRight, egui::Modifiers::NONE)]);
        assert!((value - 0.55).abs() < 0.001);
        let (value, tree) = render(vec![accessible_action(
            slider,
            Action::SetValue,
            Some(ActionData::NumericValue(35.0)),
        )]);
        assert!((value - 0.35).abs() < 0.001);
        let node = &tree.nodes.iter().find(|(id, _)| *id == slider).unwrap().1;
        assert!((node.numeric_value().unwrap() - 35.0).abs() < 0.001);
        assert_eq!(node.min_numeric_value(), Some(0.0));
        assert_eq!(node.max_numeric_value(), Some(100.0));
        let (value, _) = render(vec![accessible_action(
            slider,
            Action::SetValue,
            Some(ActionData::NumericValue(200.0)),
        )]);
        assert_eq!(value, 1.0);
        let (value, _) = render(vec![accessible_action(
            slider,
            Action::SetValue,
            Some(ActionData::NumericValue(f64::NAN)),
        )]);
        assert_eq!(value, 1.0);
    }

    #[test]
    fn accessible_switch_has_a_name_state_and_keyboard_activation() {
        use egui::accesskit::{Action, Role, Toggled};
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::theme::install(&ctx);
        let palette = crate::theme::Palette::dark();
        let mut on = false;
        let mut render = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    crate::ui::widgets::switch(ui, &palette, "Autoplay", &mut on);
                },
            );
            output.textures_delta.clear();
            (on, output.platform_output.accesskit_update.unwrap())
        };
        let (_, tree) = render(vec![]);
        let switch = accessible_node(&tree, "Autoplay", Role::CheckBox);
        let node = &tree.nodes.iter().find(|(id, _)| *id == switch).unwrap().1;
        assert_eq!(node.toggled(), Some(Toggled::False));
        render(vec![accessible_action(switch, Action::Focus, None)]);
        let (on, tree) = render(vec![keyboard(egui::Key::Space, egui::Modifiers::NONE)]);
        assert!(on);
        let node = &tree.nodes.iter().find(|(id, _)| *id == switch).unwrap().1;
        assert_eq!(node.toggled(), Some(Toggled::True));
    }

    #[test]
    fn accessible_song_focus_survives_visible_row_changes_and_keeps_duplicates_distinct() {
        use crate::ui::widgets::{TrackRow, track_row};
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("rows");
        let item = app.queue.get().unwrap().queue[0].clone();
        let context = crate::model::RowContext::Uris(Arc::from(vec![item.uri().to_string(); 2]));
        let label = format!("Play {}, {}", item.name(), item.subtitle());
        let mut render = |first, events| {
            app.actions.clear();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    for index in first..2 {
                        track_row(
                            ui,
                            &mut app,
                            TrackRow {
                                index,
                                number: Some(index + 1),
                                item: &item,
                                context: &context,
                                show_cover: false,
                                show_album: false,
                                added_at: None,
                                added_by: None,
                                show_added_by: false,
                                compact: false,
                                thin: false,
                                shift: 0.0,
                                picked: false,
                                picked_songs: &[],
                            },
                        );
                    }
                },
            );
            output.textures_delta.clear();
            (
                output.platform_output.accesskit_update.unwrap(),
                app.actions.clone(),
            )
        };
        let (tree, _) = render(0, vec![]);
        let mut positioned_rows: Vec<_> = tree
            .nodes
            .iter()
            .filter(|(_, node)| node.label() == Some(label.as_str()) && node.role() == Role::Button)
            .map(|(id, node)| (*id, node.bounds().unwrap().y0))
            .collect();
        positioned_rows.sort_by(|a, b| a.1.total_cmp(&b.1));
        let rows: Vec<_> = positioned_rows.into_iter().map(|(id, _)| id).collect();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0], rows[1]);
        render(
            0,
            vec![accessible_action(rows[1], AccessibleAction::Focus, None)],
        );
        let (tree, _) = render(1, vec![]);
        assert_eq!(
            accessible_node(&tree, &label, Role::Button),
            rows[1],
            "scrolling must not give a song another row's identity"
        );
        assert_eq!(tree.focus, rows[1]);
        let (_, actions) = render(1, vec![keyboard(egui::Key::Enter, egui::Modifiers::NONE)]);
        assert!(matches!(
            actions.as_slice(),
            [crate::model::Action::PlayFromRow { index: 1, .. }]
        ));
        let (tree, _) = render(1, vec![]);
        let more = accessible_node(&tree, "More", Role::Button);
        render(
            1,
            vec![accessible_action(more, AccessibleAction::Focus, None)],
        );
        let (tree, _) = render(1, vec![]);
        assert_eq!(
            tree.focus, more,
            "More must remain reachable after focus leaves the song row"
        );
        let (tree, _) = render(1, vec![keyboard(egui::Key::Enter, egui::Modifiers::NONE)]);
        assert!(
            tree.nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Add to queue")),
            "the keyboard opens the song menu"
        );
        app.backend.shutdown();
    }

    #[test]
    fn accessible_tab_reaches_songs_beyond_the_visible_list() {
        use crate::ui::widgets::{TrackRow, track_row, virtual_rows};
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("scroll");
        let item = app.queue.get().unwrap().queue[0].clone();
        let context = crate::model::RowContext::Uris(Arc::from(vec![item.uri().to_string(); 20]));
        let label = format!("Play {}, {}", item.name(), item.subtitle());
        let mut reached_last = false;
        let mut render = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(700.0, 200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::ScrollArea::vertical().animated(false).show(ui, |ui| {
                        virtual_rows(ui, 20, crate::theme::ROW_HEIGHT, |ui, index| {
                            if index == 19 && ui.cursor().top() < ui.clip_rect().bottom() {
                                reached_last = true;
                            }
                            track_row(
                                ui,
                                &mut app,
                                TrackRow {
                                    index,
                                    number: Some(index + 1),
                                    item: &item,
                                    context: &context,
                                    show_cover: false,
                                    show_album: false,
                                    added_at: None,
                                    added_by: None,
                                    show_added_by: false,
                                    compact: false,
                                    thin: false,
                                    shift: 0.0,
                                    picked: false,
                                    picked_songs: &[],
                                },
                            );
                        });
                    });
                },
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        };
        let tree = render(vec![]);
        let first = accessible_node(&tree, &label, Role::Button);
        render(vec![accessible_action(
            first,
            AccessibleAction::Focus,
            None,
        )]);
        for _ in 0..120 {
            render(vec![keyboard(egui::Key::Tab, egui::Modifiers::NONE)]);
        }
        assert!(
            reached_last,
            "Tab must scroll through the virtual list instead of trapping focus in its first visible rows"
        );
        app.backend.shutdown();
    }

    #[test]
    fn accessible_tab_reaches_cards_beyond_the_visible_grid() {
        use crate::ui::widgets::{card, card_row_height, virtual_wrapped_cards};
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("card-grid");
        let mut reached_last = false;
        let mut render = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 280.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::ScrollArea::vertical().animated(false).show(ui, |ui| {
                        let height = card_row_height(ui);
                        virtual_wrapped_cards(ui, 24, height, |ui, index| {
                            if index == 23 && ui.cursor().top() < ui.clip_rect().bottom() {
                                reached_last = true;
                            }
                            card(
                                ui,
                                &mut app,
                                None,
                                &format!("Album {index}"),
                                "Artist",
                                false,
                                false,
                            );
                        });
                    });
                },
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        };
        let tree = render(vec![]);
        let first = accessible_node(&tree, "Album 0, Artist", Role::Button);
        render(vec![accessible_action(
            first,
            AccessibleAction::Focus,
            None,
        )]);
        for _ in 0..80 {
            render(vec![keyboard(egui::Key::Tab, egui::Modifiers::NONE)]);
        }
        assert!(
            reached_last,
            "Tab must scroll through the virtual card grid instead of trapping focus in its first visible row"
        );
        app.backend.shutdown();
    }

    #[test]
    fn accessible_playing_and_queued_copies_target_their_own_context() {
        use crate::model::RowContext;
        use crate::ui::widgets::{TrackRow, track_row};
        use egui::accesskit::{Action as AccessibleAction, Role};
        let (ctx, mut app) = accessible_app("queued-copy");
        let item = app.queue.get().unwrap().currently_playing.clone().unwrap();
        let contexts = [
            RowContext::Uris(Arc::from([item.uri().to_string()])),
            RowContext::Queue,
        ];
        let label = format!("Play {}, {}", item.name(), item.subtitle());
        let mut render = |events| {
            app.actions.clear();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    for context in &contexts {
                        track_row(
                            ui,
                            &mut app,
                            TrackRow {
                                index: 0,
                                number: Some(1),
                                item: &item,
                                context,
                                show_cover: false,
                                show_album: false,
                                added_at: None,
                                added_by: None,
                                show_added_by: false,
                                compact: false,
                                thin: false,
                                shift: 0.0,
                                picked: false,
                                picked_songs: &[],
                            },
                        );
                    }
                },
            );
            output.textures_delta.clear();
            (
                output.platform_output.accesskit_update.unwrap(),
                app.actions.clone(),
            )
        };
        let (tree, _) = render(vec![]);
        let mut rows: Vec<_> = tree
            .nodes
            .iter()
            .filter(|(_, node)| node.label() == Some(label.as_str()) && node.role() == Role::Button)
            .map(|(id, node)| (*id, node.bounds().unwrap().y0))
            .collect();
        rows.sort_by(|a, b| a.1.total_cmp(&b.1));
        assert_eq!(
            rows.len(),
            2,
            "the playing song and queued copy need separate controls"
        );
        let (_, actions) = render(vec![accessible_action(
            rows[1].0,
            AccessibleAction::Click,
            None,
        )]);
        assert!(matches!(
            actions.as_slice(),
            [crate::model::Action::PlayFromRow {
                context: RowContext::Queue,
                index: 0,
                ..
            }]
        ));
        let (_, actions) = render(vec![accessible_action(
            rows[0].0,
            AccessibleAction::Click,
            None,
        )]);
        assert!(matches!(
            actions.as_slice(),
            [crate::model::Action::PlayFromRow {
                context: RowContext::Uris(_),
                index: 0,
                ..
            }]
        ));
        app.backend.shutdown();
    }

    #[test]
    fn the_windows_taskbar_setting_keeps_its_choice_without_closing_settings() {
        use egui::accesskit::Role;
        let (ctx, mut app) = accessible_app("winamp-taskbar-setting");
        app.open(Page::Settings);
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        if !cfg!(windows) {
            assert!(
                !tree
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("Show Winamp in taskbar"))
            );
        }
        app.demo_windows_controls = true;
        for _ in 0..4 {
            accessible_frame(&ctx, &mut app, vec![]);
        }
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let control = accessible_node(&tree, "Show Winamp in taskbar", Role::CheckBox);
        accessible_frame(
            &ctx,
            &mut app,
            vec![accessible_action(
                control,
                egui::accesskit::Action::Click,
                None,
            )],
        );
        assert!(!app.settings.winamp_show_taskbar);
        assert!(!app.settings.winamp_window && !app.switch_intent);
        let path = app.dirs.config.join("winamp-taskbar-choice.json");
        app.settings.save(&path);
        app.settings = Settings::load(&path);
        assert!(!app.settings.winamp_show_taskbar);
        app.backend.shutdown();
    }

    #[test]
    fn wayland_on_top_setting_is_disabled_and_does_not_look_active() {
        use egui::accesskit::{Role, Toggled};
        let (ctx, mut app) = accessible_app("wayland-on-top");
        app.window_level_supported = false;
        app.settings.winamp_on_top = true;
        app.open(Page::Settings);
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let id = accessible_node(&tree, "Always on top", Role::CheckBox);
        let node = &tree
            .nodes
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .unwrap()
            .1;
        assert!(node.is_disabled());
        assert_eq!(node.toggled(), Some(Toggled::False));
        assert!(
            app.settings.winamp_on_top,
            "the saved preference is preserved"
        );
        app.backend.shutdown();
    }

    fn frame(ctx: &egui::Context, app: &mut App) {
        frame_events(ctx, app, Vec::new());
    }

    #[test]
    fn a_long_device_list_stays_in_the_window_and_scrolls_to_the_last_speaker() {
        let (ctx, mut app) = accessible_app("long-device-list");
        app.backend.set_offline(true);
        app.show_devices = true;
        app.local_ready = true;
        app.receivers.clear();
        app.devices = (0..40)
            .map(|index| Device {
                id: Some(format!("speaker-{index}")),
                name: format!("Speaker {index:02}"),
                kind: "speaker".into(),
                ..Default::default()
            })
            .collect();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 620.0));
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new(crate::ui::devices::BUTTON_RECT_ID),
                egui::Rect::from_min_size(egui::pos2(680.0, 580.0), egui::vec2(32.0, 32.0)),
            )
        });
        let draw = |app: &mut App, events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |_| crate::ui::devices::popup(app, &ctx),
            );
            output.textures_delta.clear();
            output
        };
        draw(&mut app, vec![]);
        draw(&mut app, vec![]);
        let popup = egui::AreaState::load(&ctx, egui::Id::new("devices-popup"))
            .unwrap()
            .rect();
        assert!(screen.contains_rect(popup), "the popup must fit: {popup:?}");
        let cursor = popup.center();
        draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(cursor),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -10_000.0),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                },
            ],
        );
        let mut found = None;
        for _ in 0..30 {
            let output = draw(&mut app, vec![]);
            for shape in output.shapes {
                if let egui::epaint::Shape::Text(text) = shape.shape
                    && text.galley.job.text == "Speaker 39"
                    && shape.clip_rect.contains_rect(text.visual_bounding_rect())
                {
                    found = Some(text.visual_bounding_rect().center());
                }
            }
        }
        let last = found.expect("scrolling reaches the last speaker");
        app.actions.clear();
        draw(&mut app, pointer_click(last, egui::PointerButton::Primary));
        assert!(app.actions.iter().any(
            |action| matches!(action, crate::model::Action::Transfer(id) if id == "speaker-39")
        ));
        app.backend.shutdown();
    }

    #[test]
    fn playlist_filter_preserves_edit_permissions_and_keyboard_selection() {
        use egui::accesskit::Role;
        for count in [1, 2] {
            let (ctx, mut app) = accessible_app(&format!("playlist-filter-{count}"));
            app.backend.set_offline(true);
            let owner = app.user_id().unwrap().to_string();
            let make = |id: &str, name: &str, owned: bool, collaborative| Playlist {
                id: id.into(),
                uri: format!("spotify:playlist:{id}"),
                name: name.into(),
                owner: crate::api::models::Owner {
                    id: Some(if owned {
                        owner.clone()
                    } else {
                        "another-user".into()
                    }),
                    ..Default::default()
                },
                collaborative,
                ..Default::default()
            };
            app.library.playlists = Loadable::Loaded(vec![
                make("readonly", "Night locked", false, false),
                make("owned", "Night drive", true, false),
                make("shared", "Night together", false, true),
                make("day", "Daylight", true, false),
            ]);
            let items: Vec<_> = (0..count).map(|i| PlayableItem::Track(track(i))).collect();
            let mut query = String::new();
            let draw = |app: &mut App, query: &mut String, focus: bool, events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(760.0, 620.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        let field = crate::ui::widgets::playlist_picker(ui, app, &items, query);
                        if focus {
                            field.request_focus();
                        }
                    },
                );
                output.textures_delta.clear();
                output
            };
            draw(&mut app, &mut query, true, vec![]);
            let output = draw(
                &mut app,
                &mut query,
                false,
                vec![egui::Event::Text("  NiGhT  ".into())],
            );
            assert_eq!(query, "  NiGhT  ");
            let tree = output.platform_output.accesskit_update.unwrap();
            for name in ["Night locked", "Daylight"] {
                assert!(
                    !tree
                        .nodes
                        .iter()
                        .any(|(_, node)| node.label() == Some(name)),
                    "{name} must not be offered"
                );
            }
            let owned = accessible_node(&tree, "Night drive", Role::Button);
            accessible_node(&tree, "Night together", Role::Button);
            let mut reached = false;
            for _ in 0..6 {
                let output = draw(
                    &mut app,
                    &mut query,
                    false,
                    vec![keyboard(egui::Key::Tab, egui::Modifiers::NONE)],
                );
                if output.platform_output.accesskit_update.unwrap().focus == owned {
                    reached = true;
                    break;
                }
            }
            assert!(
                reached,
                "Tab must reach the filtered playlist from the search field"
            );
            app.actions.clear();
            draw(
                &mut app,
                &mut query,
                false,
                vec![keyboard(egui::Key::Enter, egui::Modifiers::NONE)],
            );
            assert!(
                matches!(app.actions.as_slice(), [crate::model::Action::AddToPlaylist { playlist_id, items: selected, .. }] if playlist_id == "owned" && selected == &items)
            );
            query = "no such playlist".into();
            let output = draw(&mut app, &mut query, false, vec![]);
            let tree = output.platform_output.accesskit_update.unwrap();
            accessible_node(&tree, "New playlist", Role::Button);
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.job.text == "No matching playlists")));
            app.backend.shutdown();
        }
    }

    #[test]
    fn playlist_submenu_keeps_typing_and_resets_after_the_parent_closes() {
        use egui::accesskit::{Action, Role};
        let (ctx, mut app) = accessible_app("playlist-submenu");
        app.backend.set_offline(true);
        let songs = vec![PlayableItem::Track(track(0))];
        let draw = |app: &mut App, show_parent: bool, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(760.0, 620.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    if show_parent {
                        crate::ui::widgets::picked_menu(ui, app, &songs);
                    }
                },
            );
            output.textures_delta.clear();
            output
        };
        let tree = draw(&mut app, true, vec![])
            .platform_output
            .accesskit_update
            .unwrap();
        let add = accessible_node(&tree, "Add to playlist", Role::Button);
        let open = || vec![accessible_action(add, Action::Click, None)];
        draw(&mut app, true, open());
        draw(&mut app, true, vec![]);
        let tree = draw(&mut app, true, vec![egui::Event::Text("night".into())])
            .platform_output
            .accesskit_update
            .unwrap();
        assert!(egui::Popup::is_any_open(&ctx));
        assert!(
            tree.nodes
                .iter()
                .any(|(_, node)| node.value() == Some("night"))
        );
        accessible_node(&tree, "Late night focus", Role::Button);
        assert!(
            !tree
                .nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Sunday morning"))
        );
        draw(
            &mut app,
            true,
            vec![keyboard(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(!egui::Popup::is_any_open(&ctx));
        // A closed outer context menu no longer draws its submenu at all.
        draw(&mut app, false, vec![]);
        draw(&mut app, true, vec![]);
        draw(&mut app, true, open());
        let tree = draw(&mut app, true, vec![])
            .platform_output
            .accesskit_update
            .unwrap();
        accessible_node(&tree, "Sunday morning", Role::Button);
        assert!(
            !tree
                .nodes
                .iter()
                .any(|(_, node)| node.value() == Some("night"))
        );
        app.backend.shutdown();
    }

    #[test]
    fn an_empty_search_half_is_not_reported_as_no_results_while_the_other_waits_or_fails() {
        let (ctx, mut app) = accessible_app("partial-search-status");
        app.search.committed = "new query".into();
        app.search.results = Loadable::Loaded(SearchResults::default());
        for pending in [true, false] {
            app.search.catalogue_pending = pending;
            app.search.error = (!pending).then(|| "Search: network error".into());
            search_frame(&ctx, &mut app, vec![]);
            let text = search_frame(&ctx, &mut app, vec![]);
            assert!(!text.iter().any(|(label, _)| label.contains("No results")));
            if !pending {
                assert!(
                    text.iter()
                        .any(|(label, _)| label.contains("network error"))
                );
            }
        }
        app.backend.shutdown();
    }

    fn search_frame(
        ctx: &egui::Context,
        app: &mut App,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Rect)> {
        view_frame(ctx, app, events, crate::ui::search::show)
    }

    #[test]
    fn playing_artist_links_work_before_web_metadata_and_after_focus_returns() {
        use crate::player::{LocalState, LocalTrack, Playback};

        let (ctx, mut app) = accessible_app("playing-artist-links");
        let artists = vec![
            ArtistRef {
                id: Some("first".into()),
                name: "Tyler, the Creator".into(),
                uri: Some("spotify:artist:first".into()),
            },
            ArtistRef {
                id: Some("guest".into()),
                name: "Guest".into(),
                uri: Some("spotify:artist:guest".into()),
            },
        ];
        app.local = LocalState {
            playback: Playback::Playing,
            track: Some(LocalTrack {
                uri: "spotify:track:uncached".into(),
                title: "Song".into(),
                artists: artists.clone(),
                duration_ms: 200_000,
                ..LocalTrack::default()
            }),
            ..LocalState::default()
        };
        app.track_cache.clear();
        let view = crate::ui::player_bar::show;
        for cached in [false, true] {
            if cached {
                // A partial response must not turn working links back into text.
                app.track_cache.insert("uncached".into(), Track::default());
            }
            view_frame(&ctx, &mut app, vec![], view);
            let text = view_frame(&ctx, &mut app, vec![], view);
            for artist in &artists {
                let pos = text
                    .iter()
                    .find(|(text, _)| text == &artist.name)
                    .unwrap()
                    .1
                    .center();
                app.actions.clear();
                view_frame(
                    &ctx,
                    &mut app,
                    pointer_click(pos, egui::PointerButton::Primary),
                    view,
                );
                assert!(
                    matches!(app.actions.as_slice(), [Action::Open(Page::Artist(id))] if Some(id) == artist.id.as_ref())
                );
            }
            view_frame(
                &ctx,
                &mut app,
                vec![egui::Event::WindowFocused(false), egui::Event::PointerGone],
                view,
            );
            view_frame(&ctx, &mut app, vec![egui::Event::WindowFocused(true)], view);
        }
        app.backend.shutdown();
    }

    #[test]
    fn compact_track_rows_leave_a_gap_before_the_added_date_separator() {
        fn rows(app: &mut App, ui: &mut egui::Ui) {
            use crate::model::RowContext;
            use crate::ui::widgets::{TrackRow, track_row};
            ui.set_max_width(520.0);
            for count in 1..=2 {
                let mut song = track(count);
                song.artists = (0..count)
                    .map(|index| ArtistRef {
                        id: Some(format!("artist-{index}")),
                        name: format!("Artist {index}"),
                        uri: Some(format!("spotify:artist:artist-{index}")),
                    })
                    .collect();
                let item = PlayableItem::Track(song);
                let context = RowContext::Uris(std::sync::Arc::from([item.uri().to_owned()]));
                track_row(
                    ui,
                    app,
                    TrackRow {
                        index: count,
                        number: Some(count),
                        item: &item,
                        context: &context,
                        show_cover: false,
                        show_album: false,
                        added_at: Some("2026-01-01T00:00:00Z"),
                        added_by: None,
                        show_added_by: false,
                        compact: false,
                        thin: true,
                        shift: 0.0,
                        picked: false,
                        picked_songs: &[],
                    },
                );
            }
        }
        let (ctx, mut app) = accessible_app("compact-artist-date-gap");
        for palette in [
            crate::theme::Palette::dark(),
            crate::theme::Palette::light(),
        ] {
            app.palette = palette;
            crate::theme::apply(&ctx, &palette);
            view_frame(&ctx, &mut app, vec![], rows);
            let text = view_frame(&ctx, &mut app, vec![], rows);
            for (label, artist) in text
                .iter()
                .filter(|(label, _)| label.starts_with("Artist "))
            {
                let separator = text.iter().find(|(label, rect)| {
                    label == "•"
                        && (rect.center().y - artist.center().y).abs() < 3.0
                        && rect.left() >= artist.right() - 0.1
                });
                // Only the last artist in each row borders the date separator.
                if let Some((_, separator)) = separator {
                    let gap = separator.left() - artist.right();
                    assert!(
                        gap >= 5.9,
                        "{label} needs a gap before the date bullet, got {gap}"
                    );
                }
            }
            assert_eq!(text.iter().filter(|(label, _)| label == "•").count(), 4);
        }
        app.backend.shutdown();
    }

    fn view_frame(
        ctx: &egui::Context,
        app: &mut App,
        events: Vec<egui::Event>,
        view: fn(&mut App, &mut egui::Ui),
    ) -> Vec<(String, egui::Rect)> {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 2200.0),
                )),
                events,
                ..Default::default()
            },
            |ui| view(app, ui),
        );
        output.textures_delta.clear();
        fn walk(shape: &egui::epaint::Shape, text: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::epaint::Shape::Text(shape) => text.push((
                    shape.galley.job.text.clone(),
                    shape.galley.rect.translate(shape.pos.to_vec2()),
                )),
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().for_each(|shape| walk(shape, text));
                }
                _ => {}
            }
        }
        let mut text = Vec::new();
        for shape in &output.shapes {
            walk(&shape.shape, &mut text);
        }
        text
    }

    fn pointer_click(pos: egui::Pos2, button: egui::PointerButton) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    #[test]
    fn custom_theme_picker_applies_the_clicked_palette_and_exposes_its_name_and_value() {
        let (ctx, mut app) = accessible_app("custom-theme-picker");
        app.open(Page::Settings);
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("settings-filter"), "Appearance".to_string())
        });
        let mut palette = crate::theme::Palette::light();
        palette.accent = egui::Color32::from_rgb(140, 63, 165);
        app.custom_themes =
            crate::theme::custom::Catalog::from_themes(vec![crate::theme::custom::CustomTheme {
                filename: "local.json".into(),
                palette,
            }]);
        for _ in 0..3 {
            view_frame(&ctx, &mut app, vec![], App::frame_ui);
        }
        let painted = view_frame(&ctx, &mut app, vec![], App::frame_ui);
        let picker = sidebar_text(&painted, "Follow system").center();
        view_frame(
            &ctx,
            &mut app,
            pointer_click(picker, egui::PointerButton::Primary),
            App::frame_ui,
        );
        let painted = view_frame(&ctx, &mut app, vec![], App::frame_ui);
        let menu_y = |name: &str| {
            painted
                .iter()
                .filter(|(text, rect)| text == name && rect.center().y > picker.y)
                .map(|(_, rect)| rect.center().y)
                .next()
                .expect("theme menu entry")
        };
        assert!(menu_y("Follow system") < menu_y("Light"));
        assert!(menu_y("Light") < menu_y("Dark"));
        assert!(menu_y("Dark") < menu_y("local.json"));
        let custom = sidebar_text(&painted, "local.json").center();
        view_frame(
            &ctx,
            &mut app,
            pointer_click(custom, egui::PointerButton::Primary),
            App::frame_ui,
        );
        assert_eq!(app.settings.custom_theme.as_deref(), Some("local.json"));
        assert_eq!(app.palette, palette);
        assert_eq!(
            app.settings.custom_theme_cache.as_ref().unwrap().palette,
            palette
        );
        assert_eq!(ctx.theme(), egui::Theme::Light);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        let id = accessible_node(&tree, "Theme", egui::accesskit::Role::ComboBox);
        let node = &tree
            .nodes
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .unwrap()
            .1;
        assert_eq!(node.value(), Some("local.json"));
        app.backend.shutdown();
    }

    #[test]
    fn themes_folder_button_is_visible_accessible_and_emits_an_action() {
        let (ctx, mut app) = accessible_app("themes-folder-button");
        app.backend.shutdown();
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("settings-filter"), "Theme".to_string())
        });
        for _ in 0..3 {
            view_frame(&ctx, &mut app, vec![], crate::ui::settings::show);
        }
        let painted = view_frame(&ctx, &mut app, vec![], crate::ui::settings::show);
        let button = sidebar_text(&painted, "Open themes folder").center();
        view_frame(
            &ctx,
            &mut app,
            pointer_click(button, egui::PointerButton::Primary),
            crate::ui::settings::show,
        );
        assert!(matches!(app.actions.as_slice(), [Action::OpenThemesFolder]));
        app.actions.clear();
        app.open(Page::Settings);
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("settings-filter"), "Theme".to_string())
        });
        accessible_frame(&ctx, &mut app, vec![]);
        let tree = accessible_frame(&ctx, &mut app, vec![]);
        accessible_node(&tree, "Open themes folder", egui::accesskit::Role::Button);
        assert!(
            !app.dirs.config.join("themes").exists(),
            "drawing cannot open or create folders"
        );
    }

    /// The painted rect of a sidebar label, for pointer tests against the
    /// sidebar's private rows.
    fn sidebar_text(painted: &[(String, egui::Rect)], label: &str) -> egui::Rect {
        painted
            .iter()
            .find(|(text, _)| text == label)
            .map(|(_, rect)| *rect)
            .unwrap_or_else(|| panic!("missing sidebar text {label:?}"))
    }

    fn double_click(pos: egui::Pos2) -> [Vec<egui::Event>; 2] {
        [
            pointer_click(pos, egui::PointerButton::Primary),
            pointer_click(pos, egui::PointerButton::Primary),
        ]
    }

    fn played_contexts(app: &App) -> Vec<String> {
        app.actions
            .iter()
            .filter_map(|action| match action {
                Action::PlayContext { uri, .. } => Some(uri.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn search_top_result_opens_the_menu_for_its_item() {
        for kind in ["track", "artist", "album", "playlist", "show"] {
            let (ctx, mut app) = accessible_app(&format!("top-result-{kind}"));
            let (results, title, uri) = match kind {
                "track" => {
                    let item = track(0);
                    (
                        SearchResults {
                            tracks: Some(page(vec![item.clone()])),
                            ..Default::default()
                        },
                        item.name,
                        item.uri,
                    )
                }
                "artist" => {
                    let item = artist(0);
                    (
                        SearchResults {
                            artists: Some(page(vec![item.clone()])),
                            ..Default::default()
                        },
                        item.name,
                        item.uri,
                    )
                }
                "album" => {
                    let item = album(0);
                    (
                        SearchResults {
                            albums: Some(page(vec![item.clone()])),
                            ..Default::default()
                        },
                        item.name,
                        item.uri,
                    )
                }
                "playlist" => {
                    let item = playlist(0);
                    (
                        SearchResults {
                            playlists: Some(page(vec![item.clone()])),
                            ..Default::default()
                        },
                        item.name,
                        item.uri,
                    )
                }
                _ => {
                    let item = show(0);
                    (
                        SearchResults {
                            shows: Some(page(vec![item.clone()])),
                            ..Default::default()
                        },
                        item.name,
                        item.uri,
                    )
                }
            };
            app.search.results = Loadable::Loaded(results);
            app.saved.clear();
            search_frame(&ctx, &mut app, vec![]);
            let text = search_frame(&ctx, &mut app, vec![]);
            let pos = text
                .iter()
                .find(|(text, _)| text == &title)
                .expect("top result title")
                .1
                .center();
            app.actions.clear();
            search_frame(
                &ctx,
                &mut app,
                pointer_click(pos, egui::PointerButton::Secondary),
            );
            let text = search_frame(&ctx, &mut app, vec![]);
            assert!(
                app.actions.is_empty(),
                "right-clicking {kind} must not navigate or play"
            );
            let expected = match kind {
                "track" => &[
                    "Add to queue",
                    "Save to Liked Songs",
                    "Add to playlist",
                    "Go to song radio",
                    "Go to artist",
                    "Go to album",
                ][..],
                "artist" => &["Play", "Follow"][..],
                "album" => &[
                    "Play",
                    "Shuffle play",
                    "Add to queue",
                    "Add to Your Library",
                ][..],
                _ => &["Play", "Add to Your Library"][..],
            };
            for label in expected {
                assert!(
                    text.iter().any(|(text, _)| text == label),
                    "{kind} menu is missing {label}"
                );
            }
            let copy = text
                .iter()
                .find(|(text, _)| text == "Copy link")
                .unwrap_or_else(|| {
                    panic!("right-clicking the {kind} top result must open its menu")
                });
            search_frame(
                &ctx,
                &mut app,
                pointer_click(copy.1.center(), egui::PointerButton::Primary),
            );
            assert!(
                matches!(app.actions.as_slice(), [crate::model::Action::CopyLink(link)] if link == &uri)
            );
            app.backend.shutdown();
        }
    }

    #[test]
    fn song_top_result_artist_name_opens_the_artist() {
        let (ctx, mut app) = accessible_app("top-result-song-artist");
        let mut item = track(0);
        item.artists = vec![ArtistRef {
            id: Some("ween".into()),
            name: "Ween".into(),
            uri: Some("spotify:artist:ween".into()),
        }];
        app.search.results = Loadable::Loaded(SearchResults {
            tracks: Some(page(vec![item])),
            ..Default::default()
        });
        search_frame(&ctx, &mut app, vec![]);
        let text = search_frame(&ctx, &mut app, vec![]);
        let songs_left = text
            .iter()
            .filter(|(text, _)| text == "Songs")
            .max_by(|(_, left), (_, right)| left.left().total_cmp(&right.left()))
            .expect("songs heading")
            .1
            .left();
        let artist = text
            .iter()
            .find(|(text, rect)| {
                (text == "Song • Ween" || text == "Ween") && rect.left() < songs_left
            })
            .expect("top result artist name");
        app.actions.clear();
        search_frame(
            &ctx,
            &mut app,
            pointer_click(
                egui::pos2(artist.1.right() - 4.0, artist.1.center().y),
                egui::PointerButton::Primary,
            ),
        );
        assert!(matches!(app.actions.as_slice(), [Action::Open(Page::Artist(id))] if id == "ween"));
        app.backend.shutdown();
    }

    fn check_card_menu(
        app: &mut App,
        ctx: &egui::Context,
        view: fn(&mut App, &mut egui::Ui),
        section: &str,
        title: &str,
        uri: &str,
        labels: &[&str],
    ) {
        view_frame(ctx, app, vec![], view);
        let text = view_frame(ctx, app, vec![], view);
        let below = text
            .iter()
            .find(|(text, _)| text == section)
            .unwrap()
            .1
            .bottom();
        let pos = text
            .iter()
            .find(|(text, rect)| text == title && rect.top() >= below)
            .unwrap_or_else(|| panic!("{title} in {section}"))
            .1
            .center();
        let earlier_card = text
            .iter()
            .find(|(_, rect)| (rect.center().y - pos.y).abs() < 1.0 && rect.center().x < pos.x)
            .map(|(_, rect)| rect.center());
        app.actions.clear();
        view_frame(
            ctx,
            app,
            pointer_click(pos, egui::PointerButton::Secondary),
            view,
        );
        // A hovered Play button must not change which item owns the open menu.
        if let Some(pos) = earlier_card {
            view_frame(ctx, app, vec![egui::Event::PointerMoved(pos)], view);
        }
        let text = view_frame(ctx, app, vec![], view);
        assert!(
            app.actions.is_empty(),
            "right-click must not navigate or play"
        );
        for label in labels {
            assert!(
                text.iter().any(|(text, _)| text == label),
                "{section}: missing {label}"
            );
        }
        let copy = text
            .iter()
            .find(|(text, _)| text == "Copy link")
            .unwrap_or_else(|| panic!("{section}: no menu for {title}"));
        view_frame(
            ctx,
            app,
            pointer_click(copy.1.center(), egui::PointerButton::Primary),
            view,
        );
        assert!(matches!(app.actions.as_slice(), [Action::CopyLink(link)] if link == uri));
    }

    #[test]
    fn library_and_discography_cards_open_their_own_menus() {
        for (page, section, title, uri, labels) in [
            (
                Page::Albums,
                "Albums",
                album(1).name,
                album(1).uri,
                vec!["Add to queue", "Shuffle play"],
            ),
            (
                Page::Artists,
                "Artists",
                artist(1).name,
                artist(1).uri,
                vec!["Follow"],
            ),
            (
                Page::Podcasts,
                "Podcasts",
                show(1).name,
                show(1).uri,
                vec!["Add to Your Library"],
            ),
            (
                Page::Artist("art0".into()),
                "Discography",
                album(1).name,
                album(1).uri,
                vec!["Add to queue", "Shuffle play"],
            ),
            (
                Page::Artist("art0".into()),
                "Fans also like",
                artist(2).name,
                artist(2).uri,
                vec!["Follow"],
            ),
        ] {
            let (ctx, mut app) = accessible_app(&format!("library-card-{section}"));
            app.open(page);
            fn view(app: &mut App, ui: &mut egui::Ui) {
                if let Page::Artist(id) = app.page().clone() {
                    crate::ui::artist::show(app, ui, &id);
                } else {
                    crate::ui::library::show(app, ui, app.page().clone());
                }
            }
            check_card_menu(&mut app, &ctx, view, section, &title, &uri, &labels);
            app.backend.shutdown();
        }
    }

    #[test]
    fn search_shelves_and_filtered_grids_open_item_menus() {
        for (filter, title, uri, labels) in [
            (
                SearchFilter::Artists,
                artist(1).name,
                artist(1).uri,
                vec!["Follow"],
            ),
            (
                SearchFilter::Albums,
                album(0).name,
                album(0).uri,
                vec!["Add to queue", "Add to Your Library"],
            ),
            (
                SearchFilter::Playlists,
                playlist(1).name,
                playlist(1).uri,
                vec!["Edit details", "Delete"],
            ),
            (
                SearchFilter::Podcasts,
                show(0).name,
                show(0).uri,
                vec!["Add to Your Library"],
            ),
        ] {
            for selected in [SearchFilter::All, filter] {
                let (ctx, mut app) =
                    accessible_app(&format!("search-card-{selected:?}-{filter:?}"));
                app.saved.clear();
                app.search.filter = selected;
                // Keep matching artist subtitles in song rows out of the card lookup.
                if let Loadable::Loaded(results) = &mut app.search.results {
                    results.tracks = None;
                    results.episodes = None;
                }
                let section = if selected == SearchFilter::All {
                    filter.label()
                } else {
                    "All"
                };
                check_card_menu(
                    &mut app,
                    &ctx,
                    crate::ui::search::show,
                    section,
                    &title,
                    &uri,
                    &labels,
                );
                app.backend.shutdown();
            }
        }
    }

    #[test]
    fn home_json_hides_only_the_chosen_recommendation_shelves() {
        let (ctx, mut app) = accessible_app("home-json-visibility");
        let view = crate::ui::home::show;
        for (made_for_you, recommendations) in
            [(true, true), (false, true), (true, false), (false, false)]
        {
            app.settings.home = serde_json::from_str(&format!(
                r#"{{"made_for_you":{{"visible":{made_for_you}}},"recommendations":{{"visible":{recommendations}}}}}"#
            )).unwrap();
            view_frame(&ctx, &mut app, vec![], view);
            let text = view_frame(&ctx, &mut app, vec![], view);
            for (label, expected) in [
                ("Made for you", made_for_you),
                ("Recommended for you", recommendations),
                ("Liked Songs", true),
                ("Recently played", true),
                ("Your top artists", true),
                ("Your top songs", true),
            ] {
                assert_eq!(
                    text.iter().any(|(text, _)| text == label),
                    expected,
                    "{label}, made_for_you={made_for_you}, recommendations={recommendations}"
                );
            }
        }
        app.backend.shutdown();
    }

    #[test]
    fn home_cards_open_item_menus() {
        for (section, title, uri, labels) in [
            (
                crate::util::greeting(),
                playlist(1).name,
                playlist(1).uri,
                vec!["Edit details", "Delete"],
            ),
            (
                crate::util::greeting(),
                playlist(0).name,
                playlist(0).uri,
                vec!["Remove from Your Library"],
            ),
            (
                "Made for you",
                playlist(0).name,
                playlist(0).uri,
                vec!["Remove from Your Library"],
            ),
            (
                "Recently played",
                track(5).name,
                track(5).uri,
                vec!["Add to queue", "Add to playlist", "Go to song radio"],
            ),
            (
                "Your top artists",
                artist(1).name,
                artist(1).uri,
                vec!["Follow"],
            ),
        ] {
            let (ctx, mut app) = accessible_app(&format!("home-card-{section}-{title}"));
            check_card_menu(
                &mut app,
                &ctx,
                crate::ui::home::show,
                section,
                &title,
                &uri,
                &labels,
            );
            app.backend.shutdown();
        }
    }

    #[test]
    fn home_liked_songs_tile_keeps_its_primary_click_only() {
        let (ctx, mut app) = accessible_app("home-liked-tile");
        let view = crate::ui::home::show;
        view_frame(&ctx, &mut app, vec![], view);
        let text = view_frame(&ctx, &mut app, vec![], view);
        let pos = text
            .iter()
            .find(|(text, _)| text == "Liked Songs")
            .unwrap()
            .1
            .center();
        app.actions.clear();
        view_frame(
            &ctx,
            &mut app,
            pointer_click(pos, egui::PointerButton::Secondary),
            view,
        );
        let text = view_frame(&ctx, &mut app, vec![], view);
        assert!(app.actions.is_empty());
        assert!(!text.iter().any(|(text, _)| text == "Copy link"));
        view_frame(
            &ctx,
            &mut app,
            pointer_click(pos, egui::PointerButton::Primary),
            view,
        );
        assert!(matches!(
            app.actions.as_slice(),
            [Action::Open(Page::LikedSongs)]
        ));
        app.backend.shutdown();
    }

    fn frame_events(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            app.frame_ui(ui);
        });
        output.textures_delta.clear();
    }

    /// A toast is wide enough to avoid wrapping every word.
    #[test]
    fn a_toast_is_wide_enough_to_read() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-toast-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        // A short toast first: the toasts area remembers its size, and a
        // long toast used to inherit the narrow width and wrap inside it.
        app.toast("Saved");
        for _ in 0..2 {
            frame(&ctx, &mut app);
        }
        app.toasts.clear();
        app.toast("Wish You Were Here added to queue");
        // Two frames: an area sizes itself on its first one.
        let mut first = ctx.run_ui(input.clone(), |ui| app.frame_ui(ui));
        first.textures_delta.clear();
        let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
        output.textures_delta.clear();

        fn widest_toast_text(shape: &egui::epaint::Shape) -> Option<f32> {
            match shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.job.text.contains("Wish You Were Here") =>
                {
                    Some(text.galley.rect.width())
                }
                egui::epaint::Shape::Vec(shapes) => {
                    shapes.iter().filter_map(widest_toast_text).next()
                }
                _ => None,
            }
        }
        let width = output
            .shapes
            .iter()
            .filter_map(|clipped| widest_toast_text(&clipped.shape))
            .next()
            .expect("the toast's text is painted");
        assert!(
            width > 150.0,
            "one word per line again: the toast text is only {width}px wide"
        );
        app.backend.shutdown();
    }

    /// The shortcuts are longer than a small window is tall, so the
    /// dialog scrolls them rather than running off the bottom with the
    /// Done button somewhere past the edge of the screen.
    #[test]
    fn the_shortcuts_dialog_fits_a_small_window() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-shortcuts-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        app.dialog = Some(Dialog::Shortcuts);

        let height = 420.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, height),
            )),
            ..Default::default()
        };
        // Two frames: the dialog sizes itself on the first.
        let mut first = ctx.run_ui(input.clone(), |ui| app.frame_ui(ui));
        first.textures_delta.clear();
        let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
        output.textures_delta.clear();

        let dialog = app.dialog_rect.expect("the dialog drew itself");
        let bottom = dialog.max.y;
        assert!(
            bottom <= height + 1.0,
            "the dialog runs {} pixels past the bottom of a {height}-tall window",
            bottom - height
        );
        app.backend.shutdown();
    }

    /// Rule: the interface zoom control puts minus on the left and plus
    /// on the right. The setting row's control is right-to-left, which
    /// used to reverse the two buttons.
    #[test]
    fn interface_zoom_puts_minus_on_the_left() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-zoom-order-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        app.settings.zoom = 1.0;
        app.open(Page::Settings);

        let mut placed: Vec<(String, f32, f32)> = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 4000.0),
            )),
            ..Default::default()
        };
        for _ in 0..2 {
            placed.clear();
            let mut output = ctx.run_ui(input.clone(), |ui| app.frame_ui(ui));
            output.textures_delta.clear();
            fn walk(shape: &egui::epaint::Shape, placed: &mut Vec<(String, f32, f32)>) {
                match shape {
                    egui::epaint::Shape::Text(text) => {
                        placed.push((text.galley.job.text.clone(), text.pos.x, text.pos.y))
                    }
                    egui::epaint::Shape::Vec(shapes) => {
                        shapes.iter().for_each(|shape| walk(shape, placed))
                    }
                    _ => {}
                }
            }
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut placed);
            }
        }
        let percent = placed
            .iter()
            .find(|(text, _, _)| text == "100%")
            .unwrap_or_else(|| panic!("the zoom percent was never drawn: {placed:?}"));
        let on_row = |label: &str| -> f32 {
            placed
                .iter()
                .filter(|(text, _, y)| text == label && (y - percent.2).abs() < 8.0)
                .min_by(|a, b| (a.1 - percent.1).abs().total_cmp(&(b.1 - percent.1).abs()))
                .unwrap_or_else(|| panic!("{label} was never drawn next to 100%: {placed:?}"))
                .1
        };
        let minus = on_row("-");
        let plus = on_row("+");
        assert!(
            minus < percent.1 && percent.1 < plus,
            "zoom control should read minus, percent, plus; got - at {minus}, 100% at {}, + at {plus}",
            percent.1
        );
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// The frame rate is a dial with detents: it stops at the rates
    /// worth having, names the one it is on, and moving it one notch
    /// lands on the next of them rather than somewhere in between.
    #[cfg(feature = "milkdrop")]
    #[test]
    fn the_frame_rate_dial_steps_between_its_stops() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-fps-dial-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        app.settings.milkdrop_screen_hz = 144;
        app.settings.milkdrop_fps = 60;
        app.open(Page::Settings);

        // Read labels from the real Settings page.
        let drawn = |app: &mut App, ctx: &egui::Context| -> Vec<String> {
            let input = egui::RawInput {
                // Draw the full Settings page, including MilkDrop.
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 4000.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
            output.textures_delta.clear();
            let mut said = Vec::new();
            fn walk(shape: &egui::epaint::Shape, said: &mut Vec<String>) {
                match shape {
                    egui::epaint::Shape::Text(text) => said.push(text.galley.job.text.clone()),
                    egui::epaint::Shape::Vec(shapes) => {
                        shapes.iter().for_each(|shape| walk(shape, said))
                    }
                    _ => {}
                }
            }
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut said);
            }
            said
        };

        for _ in 0..3 {
            let said = drawn(&mut app, &ctx);
            assert!(
                said.iter().any(|text| text.contains("60 fps")),
                "the dial names the rate it is on: {said:?}"
            );
        }

        // Every stop can be reached, and each names itself.
        for (rate, expected) in [
            (144, "144 fps, your screen"),
            (0, "Uncapped"),
            (30, "30 fps"),
        ] {
            app.settings.milkdrop_fps = rate;
            let said = drawn(&mut app, &ctx);
            assert!(
                said.iter().any(|text| text == expected),
                "the dial on {rate} should read {expected}: {said:?}"
            );
        }
        app.backend.shutdown();
    }

    /// Rule: side-panel headers stay on one line at their narrowest width.
    #[test]
    fn the_narrowest_panels_keep_their_headers_on_one_row() {
        let root = std::env::temp_dir().join(format!(
            "fastpotify-queue-header-test-{}",
            std::process::id()
        ));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        app.settings.queue_width = crate::theme::SIDE_PANEL_MIN_WIDTH;
        app.settings.lyrics_width = crate::theme::SIDE_PANEL_MIN_WIDTH;
        app.lyrics = Loadable::Loaded(Some(sample_lyrics()));
        app.lyrics_following = false;

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let drawn = |app: &mut App| {
            let mut placed = Vec::new();
            // A panel applies its requested width after the first frame.
            for _ in 0..2 {
                placed.clear();
                let mut output = ctx.run_ui(input.clone(), |ui| app.frame_ui(ui));
                output.textures_delta.clear();
                fn walk(shape: &egui::epaint::Shape, placed: &mut Vec<(String, egui::Rect)>) {
                    match shape {
                        egui::epaint::Shape::Text(text) => {
                            placed.push((text.galley.job.text.clone(), text.visual_bounding_rect()))
                        }
                        egui::epaint::Shape::Vec(shapes) => {
                            shapes.iter().for_each(|shape| walk(shape, placed))
                        }
                        _ => {}
                    }
                }
                for clipped in &output.shapes {
                    walk(&clipped.shape, &mut placed);
                }
            }
            placed
        };
        let assert_same_row = |placed: &[(String, egui::Rect)], left: &str, right: &str| {
            let at = |label: &str| {
                placed
                    .iter()
                    .find(|(text, _)| text == label)
                    .unwrap_or_else(|| panic!("{label} was never drawn: {placed:?}"))
                    .1
            };
            let (left_rect, right_rect) = (at(left), at(right));
            assert!(
                (left_rect.center().y - right_rect.center().y).abs() < 10.0
                    && (left_rect.right() <= right_rect.left()
                        || right_rect.right() <= left_rect.left()),
                "{left} and {right} should share a clear row at minimum width: {left_rect:?} vs {right_rect:?}"
            );
        };

        for (queue, lyrics) in [(false, false), (true, false), (false, true), (true, true)] {
            app.show_queue_panel = queue;
            app.show_lyrics_panel = lyrics;
            let placed = drawn(&mut app);
            if queue {
                assert_same_row(&placed, "Queue", "Recent");
            }
            if lyrics {
                assert_same_row(&placed, "Lyrics", "Follow");
            }
        }
        app.backend.shutdown();
    }

    /// The queue names where the playing song comes from, on one row
    /// with its label: the playlist, or the song a radio is seeded by.
    /// With nothing reported, the row is not drawn.
    #[test]
    fn the_queue_names_where_the_song_plays_from() {
        let root = std::env::temp_dir().join(format!(
            "fastpotify-playing-from-test-{}",
            std::process::id()
        ));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        app.show_queue_panel = true;

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        let drawn = |app: &mut App| {
            let mut placed = Vec::new();
            // A panel applies its requested width after the first frame.
            for _ in 0..2 {
                placed.clear();
                let mut output = ctx.run_ui(input.clone(), |ui| app.frame_ui(ui));
                output.textures_delta.clear();
                fn walk(shape: &egui::epaint::Shape, placed: &mut Vec<(String, egui::Rect)>) {
                    match shape {
                        egui::epaint::Shape::Text(text) => {
                            placed.push((text.galley.job.text.clone(), text.visual_bounding_rect()))
                        }
                        egui::epaint::Shape::Vec(shapes) => {
                            shapes.iter().for_each(|shape| walk(shape, placed))
                        }
                        _ => {}
                    }
                }
                for clipped in &output.shapes {
                    walk(&clipped.shape, &mut placed);
                }
            }
            placed
        };
        let find = |placed: &[(String, egui::Rect)], label: &str| {
            placed
                .iter()
                .find(|(text, _)| text == label)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| panic!("{label} was never drawn: {placed:?}"))
        };

        // The name sits to the right of its label on one row. The sidebar
        // draws the same playlist name elsewhere, so look beside the label.
        let beside = |placed: &[(String, egui::Rect)], text: &str| {
            let label = find(placed, "Playing from");
            placed
                .iter()
                .find(|(drawn, rect)| {
                    drawn == text
                        && (rect.center().y - label.center().y).abs() < 10.0
                        && rect.left() >= label.right()
                })
                .unwrap_or_else(|| panic!("{text} was not drawn beside its label: {placed:?}"));
        };

        // The demo plays the second playlist.
        beside(&drawn(&mut app), "Late night focus");

        // A song radio is named after its song.
        if let Some(remote) = app.remote.as_mut() {
            remote.state.context = Some(Context {
                uri: "spotify:station:track:trk0".into(),
                kind: "station".into(),
            });
        }
        beside(&drawn(&mut app), "Rosewood Radio");

        // Nothing reported, nothing named.
        if let Some(remote) = app.remote.as_mut() {
            remote.state.context = None;
        }
        let placed = drawn(&mut app);
        assert!(
            !placed.iter().any(|(text, _)| text == "Playing from"),
            "the row hides without a context"
        );
        app.backend.shutdown();
    }

    #[test]
    fn fullscreen_lyrics_highlight_preserves_line_layout() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-lyrics-layout-{}", std::process::id()));
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
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
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        app.show_lyrics_panel = true;
        app.lyrics_fullscreen = Some(false);
        app.lyrics = Loadable::Loaded(Some(sample_lyrics()));
        app.lyrics_following = false;
        let mut sizes = Vec::new();
        for (step, position) in [0, 41_000].into_iter().enumerate() {
            let remote = app.remote.as_mut().unwrap();
            remote.state.is_playing = false;
            remote.state.progress_ms = Some(position);
            for frame in 0..30 {
                let input = egui::RawInput {
                    time: Some(step as f64 + f64::from(frame) / 30.0),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    ..Default::default()
                };
                let mut output = ctx.run_ui(input, |ui| app.frame_ui(ui));
                output.textures_delta.clear();
                if frame == 29 {
                    let line = output
                        .shapes
                        .iter()
                        .find_map(|shape| {
                            if let egui::Shape::Text(text) = &shape.shape
                                && text.galley.job.text
                                    == "Streetlights blinking down the river road"
                            {
                                Some(text.galley.size())
                            } else {
                                None
                            }
                        })
                        .expect("the lyric line is rendered");
                    sizes.push(line);
                }
            }
        }
        app.backend.shutdown();
        assert_eq!(
            sizes[0], sizes[1],
            "highlighting must not rewrap or resize a line"
        );
    }

    /// Every page, panel, and dialog lays out without panicking.
    #[test]
    fn every_surface_renders_headless() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-render-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);

        let pages = [
            Page::Home,
            Page::TopSongs,
            Page::Search,
            Page::LikedSongs,
            Page::Albums,
            Page::Artists,
            Page::Podcasts,
            Page::Episodes,
            Page::Playlist("pl1".into()),
            Page::Playlist("missing".into()),
            Page::Album("alb0".into()),
            Page::Artist("art0".into()),
            Page::Show("sh0".into()),
            Page::Queue,
            Page::Settings,
        ];
        for page in pages {
            app.open(page.clone());
            for _ in 0..3 {
                frame(&ctx, &mut app);
            }
            assert_eq!(app.page(), &page);
        }
        app.settings.sidebar_visible = false;
        frame(&ctx, &mut app);
        app.settings.sidebar_visible = true;
        app.show_queue_panel = true;
        app.show_devices = true;
        frame(&ctx, &mut app);
        // Draw the Playing next section with a manual queue row.
        if let Loadable::Loaded(queue) = &app.queue
            && let Some(first) = queue.queue.first()
        {
            app.manual_queue = vec![first.uri().to_string()];
        }
        frame(&ctx, &mut app);
        app.manual_queue.clear();
        app.lyrics_uri = app.now_playing().map(|now| now.uri);
        app.lyrics = Loadable::Loaded(Some(sample_lyrics()));
        app.show_lyrics_panel = true;
        frame(&ctx, &mut app);
        app.lyrics_fullscreen = Some(false);
        frame(&ctx, &mut app);
        app.lyrics_fullscreen = None;
        app.show_lyrics_panel = false;
        for dialog in [
            Dialog::Shortcuts,
            Dialog::CreatePlaylist {
                name: "x".into(),
                public: true,
                add_uris: vec![],
            },
            Dialog::EditPlaylist {
                cover: Default::default(),
                id: "pl1".into(),
                name: "x".into(),
                description: String::new(),
                public: Some(false),
            },
            Dialog::ConfirmDeletePlaylist {
                id: "pl1".into(),
                name: "x".into(),
                owned: true,
            },
            Dialog::ConfirmPlaylistDuplicates {
                position: None,
                playlist_id: "pl1".into(),
                playlist_name: "x".into(),
                items: vec![PlayableItem::Track(track(1))],
                duplicate_uris: vec!["spotify:track:trk1".into()],
            },
        ] {
            app.dialog = Some(dialog);
            frame(&ctx, &mut app);
        }
        app.settings.theme = crate::settings::ThemeChoice::Light;
        app.actions.push(Action::SettingsChanged);
        app.open(Page::Home);
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }
        assert!(!app.palette.dark);
        app.lyrics_fullscreen = Some(false);
        frame(&ctx, &mut app);
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// Virtual queue rows and library cards still draw a long list, and the
    /// library still asks for the next page when the end is near.
    #[test]
    fn a_long_virtual_queue_and_library_still_draw() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-virtual-long-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        if let Loadable::Loaded(queue) = &mut app.queue {
            let seed = queue.queue.clone();
            queue.queue = seed.iter().cloned().cycle().take(200).collect();
            app.manual_queue = queue
                .queue
                .iter()
                .take(80)
                .map(|item| item.uri().to_string())
                .collect();
        }
        app.show_queue_panel = true;
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }
        app.library.albums.items = (0..80)
            .map(|index| SavedAlbum {
                added_at: None,
                album: album(index % 8),
            })
            .collect();
        app.library.albums.next_offset = Some(80);
        app.library.albums.loaded_once = true;
        app.open(Page::Albums);
        app.actions.clear();
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// A drag in flight renders, and releasing it over an owned playlist
    /// row lands in the same add-to-playlist plumbing the row menu uses.
    #[test]
    fn dropping_selected_songs_on_a_sidebar_playlist_adds_them_all() {
        drop_songs_on_sidebar(2);
    }

    #[test]
    fn dropping_a_song_on_a_sidebar_playlist_adds_it() {
        drop_songs_on_sidebar(1);
    }

    fn drop_songs_on_sidebar(count: usize) {
        let root = std::env::temp_dir().join(format!(
            "fastpotify-drag-test-{}-{count}",
            std::process::id()
        ));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        app.library
            .playlists
            .get_mut()
            .expect("the demo library")
            .retain(|playlist| playlist.id == "pl1");
        app.open(Page::Playlist("pl1".into()));
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }
        let before = app.playlist_pages["pl1"].items.items.len();
        let mut dragged = vec![
            PlayableItem::Track(Track {
                id: Some("not-in-demo-playlists".into()),
                uri: "spotify:track:not-in-demo-playlists".into(),
                name: "A new song".into(),
                ..Default::default()
            }),
            PlayableItem::Track(Track {
                id: Some("also-not-in-demo-playlists".into()),
                uri: "spotify:track:also-not-in-demo-playlists".into(),
                name: "Another new song".into(),
                ..Default::default()
            }),
        ];

        dragged.truncate(count);

        // Sweep a held track down the sidebar; somewhere along the sweep
        // the pointer crosses an owned playlist row, and releasing there
        // must mark the playlist edit busy through the existing plumbing.
        // Where exactly the rows sit depends on the loaded fonts, so the
        // sweep does not hardcode a row position.
        let mut dropped = false;
        for step in 0..40 {
            let pos = egui::pos2(120.0, 120.0 + step as f32 * 15.0);
            egui::DragAndDrop::set_payload(
                &ctx,
                DragTrack {
                    title: "A new song".into(),
                    image: None,
                    items: dragged.clone(),
                    from: None,
                },
            );
            frame_events(&ctx, &mut app, vec![egui::Event::PointerMoved(pos)]);
            frame_events(
                &ctx,
                &mut app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert!(!egui::DragAndDrop::has_any_payload(&ctx));
            if app.playlist_busy {
                dropped = true;
                break;
            }
        }
        assert!(dropped, "no sweep position landed on an owned playlist row");
        assert_eq!(app.playlist_pages["pl1"].items.items.len(), before + count);
        assert_eq!(
            app.playlist_pages["pl1"].items.items[before..]
                .iter()
                .map(|row| row.playable().unwrap().uri())
                .collect::<Vec<_>>(),
            dragged.iter().map(PlayableItem::uri).collect::<Vec<_>>()
        );
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn selecting_album_rows_then_dragging_to_a_playlist_copies_display_order() {
        use egui::accesskit::Role;
        for compact in [false, true] {
            for sorted in [false, true] {
                let (ctx, mut app) =
                    accessible_app(&format!("album-selection-drag-{compact}-{sorted}"));
                app.settings.tracklist_compact = compact;
                let source = Page::Album("alb0".into());
                app.open(source.clone());
                let tracks = &mut app.album_pages.get_mut("alb0").unwrap().tracks;
                tracks.items.truncate(3);
                for (index, name) in ["Charlie", "Bravo", "Alpha"].iter().enumerate() {
                    tracks.items[index].name = name.to_string();
                    tracks.items[index].uri = format!("spotify:track:source{index}");
                    tracks.items[index].id = Some(format!("source{index}"));
                }
                tracks.total = Some(3);
                tracks.next_offset = None;
                tracks.revision += 1;
                if sorted {
                    app.table_sorts.insert(
                        source.clone(),
                        crate::model::TableSort {
                            column: crate::model::SortColumn::Title,
                            ascending: true,
                        },
                    );
                }
                let draw = |app: &mut App, mut events: Vec<egui::Event>, modifiers| {
                    events.insert(0, egui::Event::ModifiersChanged(modifiers));
                    accessible_frame(&ctx, app, events)
                };
                draw(&mut app, vec![], egui::Modifiers::NONE);
                let tree = draw(&mut app, vec![], egui::Modifiers::NONE);
                let row = |name: &str| {
                    let prefix = format!("Play {name},");
                    let bounds = tree
                        .nodes
                        .iter()
                        .find(|(_, node)| {
                            node.role() == Role::Button
                                && node.label().is_some_and(|label| label.starts_with(&prefix))
                        })
                        .unwrap()
                        .1
                        .bounds()
                        .unwrap();
                    egui::pos2(bounds.x0 as f32 + 130.0, bounds.y0 as f32 + 8.0)
                };
                let first = row(if sorted { "Alpha" } else { "Charlie" });
                let last = row(if sorted { "Charlie" } else { "Alpha" });
                // Pick in reverse order, using Cmd on macOS or Ctrl elsewhere.
                draw(
                    &mut app,
                    pointer_click(last, egui::PointerButton::Primary),
                    egui::Modifiers::NONE,
                );
                assert_eq!(
                    app.picked_rows(&source)
                        .map(|rows| rows.iter().copied().collect::<Vec<_>>()),
                    Some(vec![2]),
                    "first click at {last:?}, page {:?}",
                    app.page()
                );
                let modifier = egui::Modifiers {
                    command: true,
                    mac_cmd: cfg!(target_os = "macos"),
                    ctrl: !cfg!(target_os = "macos"),
                    ..Default::default()
                };
                let mut click = pointer_click(first, egui::PointerButton::Primary);
                for event in &mut click {
                    if let egui::Event::PointerButton { modifiers, .. } = event {
                        *modifiers = modifier;
                    }
                }
                draw(&mut app, click, modifier);
                assert_eq!(
                    app.picked_rows(&source)
                        .unwrap()
                        .iter()
                        .copied()
                        .collect::<Vec<_>>(),
                    [0, 2]
                );
                draw(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(last),
                        egui::Event::PointerButton {
                            pos: last,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    egui::Modifiers::NONE,
                );
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(last + egui::vec2(15.0, 0.0))],
                    egui::Modifiers::NONE,
                );
                let payload = egui::DragAndDrop::payload::<DragTrack>(&ctx)
                    .expect("drag from a selected row");
                let expected = if sorted {
                    ["spotify:track:source2", "spotify:track:source0"]
                } else {
                    ["spotify:track:source0", "spotify:track:source2"]
                };
                assert_eq!(
                    payload
                        .items
                        .iter()
                        .map(PlayableItem::uri)
                        .collect::<Vec<_>>(),
                    expected
                );
                assert_eq!(payload.from, None);
                let bounds = tree
                    .nodes
                    .iter()
                    .find(|(_, node)| {
                        node.role() == Role::Button && node.label() == Some("Late night focus")
                    })
                    .unwrap()
                    .1
                    .bounds()
                    .unwrap();
                let target = egui::pos2(
                    ((bounds.x0 + bounds.x1) / 2.0) as f32,
                    ((bounds.y0 + bounds.y1) / 2.0) as f32,
                );
                let before = app.playlist_pages["pl1"].items.items.len();
                let playing = app.current_track_uri();
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(target)],
                    egui::Modifiers::NONE,
                );
                draw(
                    &mut app,
                    vec![egui::Event::PointerButton {
                        pos: target,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    egui::Modifiers::NONE,
                );
                assert_eq!(
                    app.playlist_pages["pl1"].items.items[before..]
                        .iter()
                        .map(|row| row.playable().unwrap().uri())
                        .collect::<Vec<_>>(),
                    expected
                );
                let sent = app.backend.take_playlist_add_requests();
                assert!(
                    matches!(sent.as_slice(), [crate::backend::ApiRequest::AddToPlaylist { playlist_id, uris, position: None, .. }] if playlist_id == "pl1" && uris == &expected)
                );
                assert_eq!(app.current_track_uri(), playing);
                app.backend.shutdown();
            }
        }
    }

    /// Double-clicking a playable Library row plays its context, in both the
    /// normal and compact sidebar modes. The first click still opens the page.
    #[test]
    fn double_clicking_a_sidebar_row_plays_its_context() {
        for compact in [false, true] {
            let (ctx, mut app) = accessible_app(&format!("sidebar-double-click-{compact}"));
            app.settings.sidebar_compact = compact;
            let view = crate::ui::sidebar::show;
            view_frame(&ctx, &mut app, vec![], view);
            let painted = view_frame(&ctx, &mut app, vec![], view);
            let name = sidebar_text(&painted, "Sunday morning").center();
            app.actions.clear();
            let [first, second] = double_click(name);
            view_frame(&ctx, &mut app, first, view);
            view_frame(&ctx, &mut app, second, view);
            assert_eq!(played_contexts(&app), ["spotify:playlist:pl2"]);
            assert!(
                app.actions.iter().any(
                    |action| matches!(action, Action::Open(Page::Playlist(id)) if id == "pl2")
                ),
                "double click must still open the page"
            );
            app.backend.shutdown();
        }
    }

    /// A single Library row click keeps navigating and never starts playback.
    #[test]
    fn single_clicking_a_sidebar_row_only_navigates() {
        let (ctx, mut app) = accessible_app("sidebar-single-click");
        let view = crate::ui::sidebar::show;
        view_frame(&ctx, &mut app, vec![], view);
        let painted = view_frame(&ctx, &mut app, vec![], view);
        let name = sidebar_text(&painted, "Sunday morning").center();
        app.actions.clear();
        view_frame(
            &ctx,
            &mut app,
            pointer_click(name, egui::PointerButton::Primary),
            view,
        );
        assert!(played_contexts(&app).is_empty());
        assert!(
            app.actions
                .iter()
                .any(|action| matches!(action, Action::Open(Page::Playlist(id)) if id == "pl2"))
        );
        app.backend.shutdown();
    }

    #[test]
    fn double_clicking_a_sidebar_liked_row_plays_the_collection() {
        let (ctx, mut app) = accessible_app("sidebar-double-click-liked");
        let view = crate::ui::sidebar::show;
        view_frame(&ctx, &mut app, vec![], view);
        let painted = view_frame(&ctx, &mut app, vec![], view);
        let name = sidebar_text(&painted, "Liked Songs").center();
        app.actions.clear();
        let [first, second] = double_click(name);
        view_frame(&ctx, &mut app, first, view);
        view_frame(&ctx, &mut app, second, view);
        assert_eq!(played_contexts(&app), ["spotify:user:demo:collection"]);
        app.backend.shutdown();
    }

    #[test]
    fn double_clicking_a_sidebar_folder_plays_nothing() {
        use crate::player::RootlistEntry;
        let (ctx, mut app) = accessible_app("sidebar-double-click-folder");
        app.rootlist = vec![
            RootlistEntry::FolderStart {
                id: "f1".into(),
                name: "Focus".into(),
            },
            RootlistEntry::Playlist("spotify:playlist:pl1".into()),
            RootlistEntry::Playlist("spotify:playlist:pl2".into()),
            RootlistEntry::FolderEnd,
        ];
        let view = crate::ui::sidebar::show;
        view_frame(&ctx, &mut app, vec![], view);
        let painted = view_frame(&ctx, &mut app, vec![], view);
        let name = sidebar_text(&painted, "Focus").center();
        app.actions.clear();
        let [first, second] = double_click(name);
        view_frame(&ctx, &mut app, first, view);
        view_frame(&ctx, &mut app, second, view);
        assert!(played_contexts(&app).is_empty(), "folders must not play");
        app.backend.shutdown();
    }

    /// The cover play button sits on top of its row. A double click on it
    /// sends one play command, not one for each click plus the row's.
    #[test]
    fn double_clicking_a_sidebar_cover_plays_once() {
        let (ctx, mut app) = accessible_app("sidebar-double-click-cover");
        let view = crate::ui::sidebar::show;
        view_frame(&ctx, &mut app, vec![], view);
        let painted = view_frame(&ctx, &mut app, vec![], view);
        let name = sidebar_text(&painted, "Sunday morning");
        // The cover is the 44 point square centered 34 points left of the
        // name and on the row's centre, nine points below the name line.
        let cover = egui::pos2(name.left() - 34.0, name.center().y + 9.0);
        app.actions.clear();
        let [first, second] = double_click(cover);
        view_frame(&ctx, &mut app, first, view);
        assert_eq!(played_contexts(&app), ["spotify:playlist:pl2"]);
        view_frame(&ctx, &mut app, second, view);
        assert_eq!(
            played_contexts(&app),
            ["spotify:playlist:pl2"],
            "the second click must not play again"
        );
        app.backend.shutdown();
    }

    /// The cover and title in the bottom-left player are a song source, not
    /// just links. The sidebar can therefore receive the same complete row it
    /// receives when a table song is dragged.
    #[test]
    fn dragging_the_now_playing_song_supplies_a_playlist_row() {
        let root = std::env::temp_dir().join(format!(
            "fastpotify-now-playing-drag-test-{}",
            std::process::id()
        ));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }

        let start = egui::pos2(40.0, 755.0);
        frame_events(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        frame_events(
            &ctx,
            &mut app,
            vec![egui::Event::PointerMoved(start + egui::vec2(20.0, -10.0))],
        );

        let payload = egui::DragAndDrop::payload::<DragTrack>(&ctx)
            .expect("dragging the bottom-left song should create a song payload");
        assert_eq!(payload.items.len(), 1);
        assert_eq!(payload.items[0].uri(), "spotify:track:trk0");
        assert_eq!(payload.from, None, "this is an add, not a playlist move");

        egui::DragAndDrop::clear_payload(&ctx);
        frame_events(
            &ctx,
            &mut app,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// Pins are pins: dropping a pinned row at the top of the block
    /// reorders the pins themselves, and the rest of the shelf stays in
    /// its automatic order.
    #[test]
    fn dragging_within_the_pinned_block_reorders_it() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-reorder-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        app.settings.pinned_contexts =
            vec!["spotify:playlist:pl2".into(), "spotify:playlist:pl4".into()];
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }

        // Sweep from the top: the first slot inside the list drops the
        // dragged row above Liked Songs. Where the list begins
        // depends on the loaded fonts, so the sweep does not hardcode it.
        let mut dropped = false;
        for step in 0..40 {
            let pos = egui::pos2(120.0, 100.0 + step as f32 * 10.0);
            egui::DragAndDrop::set_payload(
                &ctx,
                DragEntry {
                    uri: "spotify:playlist:pl4".into(),
                    title: "Release Radar".into(),
                    image: None,
                },
            );
            frame_events(&ctx, &mut app, vec![egui::Event::PointerMoved(pos)]);
            frame_events(
                &ctx,
                &mut app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            egui::DragAndDrop::clear_payload(&ctx);
            if app.settings.pinned_contexts.first().map(String::as_str)
                == Some("spotify:playlist:pl4")
            {
                dropped = true;
                break;
            }
        }
        assert!(dropped, "no sweep position landed in the pinned block");
        assert_eq!(
            app.settings.pinned_contexts,
            vec![
                "spotify:playlist:pl4".to_string(),
                crate::settings::LIKED_SONGS_KEY.to_string(),
                "spotify:playlist:pl2".to_string(),
            ],
        );
        assert!(app.settings.sidebar_order.is_empty());
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// Reordering unpinned playlists creates a custom sidebar order.
    #[test]
    fn dropping_between_unpinned_playlists_creates_the_custom_order() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-unpinned-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        assert!(app.settings.pinned_contexts.is_empty());
        assert!(app.settings.sidebar_order.is_empty());
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }

        // Sweep from the top; the first slot inside the list is the one
        // right under Liked Songs, between what were the first two
        // unpinned playlists.
        let mut dropped = false;
        for step in 0..40 {
            let pos = egui::pos2(120.0, 100.0 + step as f32 * 10.0);
            egui::DragAndDrop::set_payload(
                &ctx,
                DragEntry {
                    uri: "spotify:playlist:pl4".into(),
                    title: "Release Radar".into(),
                    image: None,
                },
            );
            frame_events(&ctx, &mut app, vec![egui::Event::PointerMoved(pos)]);
            frame_events(
                &ctx,
                &mut app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            egui::DragAndDrop::clear_payload(&ctx);
            if !app.settings.sidebar_order.is_empty() {
                dropped = true;
                break;
            }
        }
        assert!(dropped, "no sweep position landed below Liked Songs");
        let expected: Vec<String> = std::iter::once(4)
            .chain((0..PLAYLISTS.len()).filter(|index| *index != 4))
            .map(|index| format!("spotify:playlist:pl{index}"))
            .collect();
        assert_eq!(app.settings.sidebar_order, expected);
        assert_eq!(
            app.settings.library_pins(),
            [crate::settings::LIKED_SONGS_KEY]
        );
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dragging_liked_songs_moves_it_and_its_menu_can_pin_it_again() {
        use crate::settings::LIKED_SONGS_KEY;
        use egui::accesskit::Role;
        for compact in [false, true] {
            let (ctx, mut app) = accessible_app(&format!("liked-drag-{compact}"));
            app.settings.sidebar_compact = compact;
            app.rootlist.clear();
            app.settings.pinned_contexts = vec!["spotify:playlist:pl2".into()];
            app.settings.sidebar_order = (0..PLAYLISTS.len())
                .filter(|index| *index != 2)
                .map(|index| format!("spotify:playlist:pl{index}"))
                .collect();
            let row = |tree: &egui::accesskit::TreeUpdate, label: &str| {
                let bounds = tree
                    .nodes
                    .iter()
                    .find(|(_, node)| {
                        node.role() == Role::Button
                            && node.label() == Some(label)
                            && node.bounds().is_some_and(|bounds| bounds.x0 < 250.0)
                    })
                    .unwrap_or_else(|| panic!("missing sidebar row {label}"))
                    .1
                    .bounds()
                    .unwrap();
                egui::Rect::from_min_max(
                    egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                    egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                )
            };
            accessible_frame(&ctx, &mut app, vec![]);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let source = row(&tree, "Liked Songs").center();
            let target = row(&tree, "Late night focus");
            let target = egui::pos2(source.x, target.top() + 1.0);
            accessible_frame(
                &ctx,
                &mut app,
                vec![
                    egui::Event::PointerMoved(source),
                    egui::Event::PointerButton {
                        pos: source,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            accessible_frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(source + egui::vec2(12.0, 0.0))],
            );
            assert_eq!(
                egui::DragAndDrop::payload::<DragEntry>(&ctx).unwrap().uri,
                LIKED_SONGS_KEY
            );
            accessible_frame(&ctx, &mut app, vec![egui::Event::PointerMoved(target)]);
            accessible_frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert!(!app.settings.liked_songs_pinned);
            assert_eq!(
                &app.settings.sidebar_order[..3],
                [
                    "spotify:playlist:pl0",
                    LIKED_SONGS_KEY,
                    "spotify:playlist:pl1"
                ]
            );
            let expected = app.settings.sidebar_order.clone();
            let path = app.dirs.config.join("liked-placement.json");
            app.settings.save(&path);
            app.settings = Settings::load(&path);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            assert_eq!(app.settings.sidebar_order, expected);
            assert!(row(&tree, "Discover Weekly").top() < row(&tree, "Liked Songs").top());
            assert!(row(&tree, "Liked Songs").top() < row(&tree, "Late night focus").top());
            let source = row(&tree, "Liked Songs").center();
            accessible_frame(
                &ctx,
                &mut app,
                pointer_click(source, egui::PointerButton::Secondary),
            );
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let pin = accessible_node(&tree, "Pin to top", Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(pin, egui::accesskit::Action::Click, None)],
            );
            assert_eq!(
                app.settings.library_pins(),
                ["spotify:playlist:pl2", LIKED_SONGS_KEY]
            );
            assert!(
                !app.settings
                    .sidebar_order
                    .iter()
                    .any(|key| key == LIKED_SONGS_KEY)
            );
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let source = row(&tree, "Liked Songs").center();
            accessible_frame(
                &ctx,
                &mut app,
                pointer_click(source, egui::PointerButton::Secondary),
            );
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let unpin = accessible_node(&tree, "Unpin", Role::Button);
            accessible_frame(
                &ctx,
                &mut app,
                vec![accessible_action(
                    unpin,
                    egui::accesskit::Action::Click,
                    None,
                )],
            );
            assert!(!app.settings.liked_songs_pinned);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let target = row(&tree, "Liked Songs").center();
            app.saved.insert("spotify:track:trk0".into(), false);
            let source = egui::pos2(40.0, 755.0);
            accessible_frame(
                &ctx,
                &mut app,
                vec![
                    egui::Event::PointerMoved(source),
                    egui::Event::PointerButton {
                        pos: source,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            accessible_frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(source + egui::vec2(16.0, 0.0))],
            );
            assert_eq!(
                egui::DragAndDrop::payload::<DragTrack>(&ctx).unwrap().items[0].uri(),
                "spotify:track:trk0"
            );
            accessible_frame(&ctx, &mut app, vec![egui::Event::PointerMoved(target)]);
            accessible_frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerButton {
                    pos: target,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(
                app.is_saved("spotify:track:trk0"),
                Some(true),
                "dropping a song on the relocated row still saves it"
            );
            assert!(app.backend.take_playlist_add_requests().is_empty());
            app.backend.shutdown();
        }
    }

    #[test]
    fn queue_and_player_drags_insert_at_the_chosen_playlist_position() {
        for from_queue in [false, true] {
            for compact in [false, true] {
                for position in [0, 1, 4] {
                    let (ctx, mut app) =
                        accessible_app(&format!("insert-drag-{from_queue}-{compact}-{position}"));
                    app.settings.tracklist_compact = compact;
                    app.show_queue_panel = from_queue;
                    app.open(Page::Playlist("pl1".into()));
                    let items = &mut app.playlist_pages.get_mut("pl1").unwrap().items;
                    items.items.truncate(4);
                    for (index, row) in items.items.iter_mut().enumerate() {
                        if let Some(PlayableItem::Track(track)) = &mut row.item {
                            track.uri = format!("spotify:track:destination{index}");
                            track.id = Some(format!("destination{index}"));
                            track.name = format!("Destination {index}");
                        }
                    }
                    items.total = Some(4);
                    items.next_offset = None;
                    items.revision += 1;
                    let queue_before: Vec<_> = app
                        .queue
                        .get()
                        .unwrap()
                        .queue
                        .iter()
                        .map(|item| item.uri().to_string())
                        .collect();
                    let manual_before = app.manual_queue.clone();
                    let playing_before = app.current_track_uri();
                    let source_item = if from_queue {
                        app.queue.get().unwrap().queue[0].clone()
                    } else {
                        app.queue.get().unwrap().currently_playing.clone().unwrap()
                    };
                    accessible_frame(&ctx, &mut app, vec![]);
                    let tree = accessible_frame(&ctx, &mut app, vec![]);
                    let row_rect = |name: &str| {
                        let prefix = format!("Play {name},");
                        let bounds = tree
                            .nodes
                            .iter()
                            .find(|(_, node)| {
                                node.role() == egui::accesskit::Role::Button
                                    && node.label().is_some_and(|text| text.starts_with(&prefix))
                            })
                            .unwrap_or_else(|| panic!("missing row {name}"))
                            .1
                            .bounds()
                            .unwrap();
                        egui::Rect::from_min_max(
                            egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                            egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                        )
                    };
                    let start = if from_queue {
                        let row = row_rect(source_item.name());
                        egui::pos2(row.left() + 80.0, row.center().y)
                    } else {
                        egui::pos2(40.0, 755.0)
                    };
                    let row = row_rect(&format!("Destination {}", position.min(3)));
                    let end = egui::pos2(
                        row.left() + 130.0,
                        if position == 4 {
                            row.bottom() - 1.0
                        } else {
                            row.top() + 1.0
                        },
                    );
                    frame_events(
                        &ctx,
                        &mut app,
                        vec![
                            egui::Event::PointerMoved(start),
                            egui::Event::PointerButton {
                                pos: start,
                                button: egui::PointerButton::Primary,
                                pressed: true,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    );
                    frame_events(
                        &ctx,
                        &mut app,
                        vec![egui::Event::PointerMoved(start + egui::vec2(15.0, -10.0))],
                    );
                    let payload =
                        egui::DragAndDrop::payload::<DragTrack>(&ctx).expect("real source drag");
                    assert_eq!(payload.items[0].uri(), source_item.uri());
                    assert_eq!(payload.from, None);
                    frame_events(&ctx, &mut app, vec![egui::Event::PointerMoved(end)]);
                    frame_events(
                        &ctx,
                        &mut app,
                        vec![egui::Event::PointerButton {
                            pos: end,
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            modifiers: egui::Modifiers::NONE,
                        }],
                    );
                    let rows = &app.playlist_pages["pl1"].items.items;
                    assert_eq!(rows.len(), 5, "{from_queue} {compact} {position}");
                    assert_eq!(rows[position].playable().unwrap().uri(), source_item.uri());
                    let sent = app.backend.take_playlist_add_requests();
                    assert!(
                        matches!(sent.as_slice(), [crate::backend::ApiRequest::AddToPlaylist { playlist_id, position: Some(at), uris, .. }] if playlist_id == "pl1" && *at == position as u32 && uris == &[source_item.uri()])
                    );
                    assert_eq!(app.current_track_uri(), playing_before);
                    assert_eq!(app.manual_queue, manual_before);
                    assert_eq!(
                        app.queue
                            .get()
                            .unwrap()
                            .queue
                            .iter()
                            .map(|item| item.uri().to_string())
                            .collect::<Vec<_>>(),
                        queue_before
                    );
                    app.backend.shutdown();
                }
            }
        }
    }

    #[test]
    fn playlist_drop_targets_cover_empty_lists_and_preserve_editing_boundaries() {
        for mode in [
            "foreign", "empty", "multiple", "readonly", "sorted", "filtered",
        ] {
            let (ctx, mut app) = accessible_app(&format!("playlist-drop-target-{mode}"));
            let mut target = track(0);
            target.name = "Drop target".into();
            target.uri = "spotify:track:destination".into();
            let rows = vec![(PlayableItem::Track(target), None, None)];
            if mode == "sorted" {
                app.table_sorts.insert(
                    Page::Playlist("pl1".into()),
                    crate::model::TableSort {
                        column: crate::model::SortColumn::Title,
                        ascending: true,
                    },
                );
            }
            let draw = |app: &mut App, events, empty| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(760.0, 620.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        crate::ui::collection::table(
                            app,
                            ui,
                            crate::ui::collection::Table {
                                pagination: None,
                                items: if empty { &[] } else { &rows },
                                row_offset: if mode == "empty" { 0 } else { 100 },
                                context: crate::model::RowContext::Context {
                                    uri: "spotify:playlist:pl1".into(),
                                    editable_playlist: (mode != "readonly")
                                        .then(|| ("pl1".into(), None)),
                                },
                                show_album: false,
                                show_cover: true,
                                show_added: false,
                                show_added_by: false,
                                page: Page::Playlist("pl1".into()),
                                loading: false,
                                error: None,
                                can_load_more: false,
                                filter: if mode == "filtered" {
                                    "Drop target"
                                } else {
                                    ""
                                },
                                items_revision: u64::from(empty),
                            },
                        );
                    },
                );
                output.textures_delta.clear();
                output.platform_output.accesskit_update.unwrap()
            };
            draw(&mut app, vec![], false);
            let tree = draw(&mut app, vec![], false);
            let bounds = tree
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.label()
                        .is_some_and(|label| label.starts_with("Play Drop target,"))
                })
                .unwrap()
                .1
                .bounds()
                .unwrap();
            let end = egui::pos2(
                bounds.x0 as f32 + 140.0,
                if mode == "empty" {
                    400.0
                } else {
                    bounds.y0 as f32 + 2.0
                },
            );
            let dragged =
                app.queue.get().unwrap().queue[..if mode == "multiple" { 2 } else { 1 }].to_vec();
            let expected: Vec<_> = dragged.iter().map(|item| item.uri().to_string()).collect();
            egui::DragAndDrop::set_payload(
                &ctx,
                DragTrack {
                    title: dragged[0].name().into(),
                    image: None,
                    items: dragged,
                    from: (mode == "foreign").then(|| ("pl2".into(), 5)),
                },
            );
            draw(
                &mut app,
                vec![
                    egui::Event::PointerMoved(end),
                    egui::Event::PointerButton {
                        pos: end,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                mode == "empty",
            );
            app.actions.clear();
            draw(
                &mut app,
                vec![egui::Event::PointerButton {
                    pos: end,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                mode == "empty",
            );
            if matches!(mode, "foreign" | "empty" | "multiple") {
                assert!(
                    matches!(app.actions.as_slice(), [Action::InsertInPlaylist { playlist_id, position, items }] if playlist_id == "pl1" && *position == if mode == "empty" { 0 } else { 100 } && items.iter().map(PlayableItem::uri).collect::<Vec<_>>() == expected),
                    "{mode}: {:?}",
                    app.actions
                );
            } else {
                assert!(
                    !app.actions.iter().any(|action| matches!(
                        action,
                        Action::InsertInPlaylist { .. } | Action::MoveInPlaylist { .. }
                    )),
                    "{mode}"
                );
            }
            app.backend.shutdown();
        }
    }

    #[test]
    fn dragging_at_playlist_edges_reaches_rows_beyond_the_viewport() {
        for compact in [false, true] {
            for upwards in [false, true] {
                let (ctx, mut app) = accessible_app(&format!("drag-scroll-{compact}-{upwards}"));
                app.settings.tracklist_compact = compact;
                app.open(Page::Playlist("pl1".into()));
                let items = &mut app.playlist_pages.get_mut("pl1").unwrap().items.items;
                let count = items.len();
                assert!(count > 20);
                for (index, name) in [(0, "First song"), (count - 1, "Last song")] {
                    if let Some(PlayableItem::Track(track)) = &mut items[index].item {
                        track.name = name.into();
                    }
                }
                let from = if upwards { count - 1 } else { 0 };
                let dragged_uri = items[from].playable().unwrap().uri().to_string();
                let mut time = 0.0;
                let mut draw = |app: &mut App, events, offset: Option<f32>| {
                    time += 1.0 / 60.0;
                    let mut result = None;
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(760.0, 620.0),
                            )),
                            time: Some(time),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            let mut scroll = egui::ScrollArea::vertical()
                                .id_salt("drag-scroll-playlist")
                                .auto_shrink([false, false]);
                            if let Some(offset) = offset {
                                scroll = scroll.vertical_scroll_offset(offset);
                            }
                            let shown = scroll
                                .show(ui, |ui| crate::ui::collection::playlist(app, ui, "pl1"));
                            result = Some((shown.state.offset.y, shown.inner_rect));
                        },
                    );
                    output.textures_delta.clear();
                    let (offset, viewport) = result.unwrap();
                    (
                        offset,
                        viewport,
                        output.platform_output.accesskit_update.unwrap(),
                    )
                };
                let row = |tree: &egui::accesskit::TreeUpdate, name: &str| {
                    let prefix = format!("Play {name},");
                    let bounds = tree
                        .nodes
                        .iter()
                        .find(|(_, node)| {
                            node.role() == egui::accesskit::Role::Button
                                && node.label().is_some_and(|label| label.starts_with(&prefix))
                        })
                        .unwrap_or_else(|| panic!("missing {name}"))
                        .1
                        .bounds()
                        .unwrap();
                    egui::Rect::from_min_max(
                        egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                        egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
                    )
                };
                draw(
                    &mut app,
                    vec![],
                    Some(if upwards { 100_000.0 } else { 0.0 }),
                );
                let (start, viewport, tree) = draw(&mut app, vec![], None);
                let source = row(&tree, if upwards { "Last song" } else { "First song" });
                let pos = egui::pos2(source.left() + 160.0, source.top() + 18.0);
                assert!(viewport.contains(pos));
                draw(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    None,
                );
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(pos + egui::vec2(12.0, 0.0))],
                    None,
                );
                assert!(
                    egui::DragAndDrop::has_payload_of_type::<DragTrack>(&ctx),
                    "the row must start a real drag"
                );
                let edge = egui::pos2(
                    pos.x,
                    if upwards {
                        viewport.top() + 2.0
                    } else {
                        viewport.bottom() - 2.0
                    },
                );
                draw(&mut app, vec![egui::Event::PointerMoved(edge)], None);
                for _ in 0..30 {
                    draw(&mut app, vec![], None);
                }
                let (moved, _, _) = draw(&mut app, vec![], None);
                assert!(
                    if upwards {
                        start - moved > 200.0
                    } else {
                        moved - start > 200.0
                    },
                    "holding a stationary pointer at an edge must scroll"
                );
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(viewport.center())],
                    None,
                );
                let (paused, _, _) = draw(&mut app, vec![], None);
                for _ in 0..10 {
                    draw(&mut app, vec![], None);
                }
                let (still, _, _) = draw(&mut app, vec![], None);
                assert_eq!(paused, still, "leaving the edge stops scrolling");
                draw(&mut app, vec![egui::Event::PointerMoved(edge)], None);
                for _ in 0..300 {
                    draw(&mut app, vec![], None);
                }
                let (_, _, tree) = draw(&mut app, vec![], None);
                let target = row(&tree, if upwards { "First song" } else { "Last song" });
                let pos = egui::pos2(
                    pos.x,
                    if upwards {
                        target.top() + 1.0
                    } else {
                        target.bottom() - 1.0
                    },
                );
                draw(&mut app, vec![egui::Event::PointerMoved(pos)], None);
                app.actions.clear();
                draw(
                    &mut app,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    None,
                );
                assert!(
                    matches!(app.actions.as_slice(), [Action::MoveInPlaylist { playlist_id, from: actual_from, to }] if playlist_id == "pl1" && *actual_from == from as u32 && *to == if upwards { 0 } else { count as u32 })
                );
                for action in std::mem::take(&mut app.actions) {
                    app.apply(action, &ctx);
                }
                let items = &app.playlist_pages["pl1"].items.items;
                assert_eq!(
                    items[if upwards { 0 } else { count - 1 }]
                        .playable()
                        .unwrap()
                        .uri(),
                    dragged_uri
                );
                app.backend.shutdown();
            }
        }
    }

    #[test]
    fn dragging_a_library_entry_scrolls_to_offscreen_playlists() {
        use egui::accesskit::Role;
        for compact in [false, true] {
            let (ctx, mut app) = accessible_app(&format!("sidebar-drag-scroll-{compact}"));
            app.settings.sidebar_compact = compact;
            app.rootlist.clear();
            app.settings.pinned_contexts.clear();
            let playlists: Vec<_> = (0..60)
                .map(|index| {
                    let mut playlist = playlist(1);
                    playlist.id = format!("target{index}");
                    playlist.uri = format!("spotify:playlist:target{index}");
                    playlist.name = format!("Target {index}");
                    playlist
                })
                .collect();
            app.settings.sidebar_order = playlists
                .iter()
                .map(|playlist| playlist.uri.clone())
                .collect();
            app.library.playlists = Loadable::Loaded(playlists);
            let original = app.settings.sidebar_order.clone();
            accessible_frame(&ctx, &mut app, vec![]);
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            let first = accessible_node(&tree, "Target 0", Role::Button);
            let bounds = tree
                .nodes
                .iter()
                .find(|(id, _)| *id == first)
                .unwrap()
                .1
                .bounds()
                .unwrap();
            let pos = egui::pos2(100.0, ((bounds.y0 + bounds.y1) / 2.0) as f32);
            accessible_frame(
                &ctx,
                &mut app,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            accessible_frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(pos + egui::vec2(12.0, 0.0))],
            );
            assert!(egui::DragAndDrop::has_payload_of_type::<DragEntry>(&ctx));
            accessible_frame(
                &ctx,
                &mut app,
                vec![egui::Event::PointerMoved(egui::pos2(
                    112.0,
                    800.0 - crate::theme::PLAYER_BAR_HEIGHT - 14.0,
                ))],
            );
            for _ in 0..400 {
                accessible_frame(&ctx, &mut app, vec![]);
            }
            let tree = accessible_frame(&ctx, &mut app, vec![]);
            accessible_node(&tree, "Target 59", Role::Button);
            assert_eq!(
                app.settings.sidebar_order, original,
                "scrolling alone must not reorder entries"
            );
            egui::DragAndDrop::clear_payload(&ctx);
            app.backend.shutdown();
        }
    }

    /// Dragging a row within an owned playlist's table moves it through
    /// the same MoveInPlaylist action the menu's move items use: the slot
    /// is Spotify's insert-before, which the handler mirrors locally
    /// before asking the server.
    #[test]
    fn dragging_a_row_within_a_playlist_reorders_it() {
        let root =
            std::env::temp_dir().join(format!("fastpotify-move-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);
        app.open(Page::Playlist("pl1".into()));
        for _ in 0..3 {
            frame(&ctx, &mut app);
        }
        let order = |app: &App| -> Vec<String> {
            app.playlist_pages["pl1"]
                .items
                .items
                .iter()
                .filter_map(|item| item.playable().map(|playable| playable.uri().to_string()))
                .collect()
        };
        let original = order(&app);
        let from = 5usize;
        let held = |from: usize, uri: &str| DragTrack {
            title: "Closer".into(),
            image: None,
            items: vec![PlayableItem::Track(Track {
                uri: uri.to_string(),
                name: "Closer".into(),
                ..Default::default()
            })],
            from: Some(("pl1".into(), from as u32)),
        };

        // Sweep the held row down the page; above the table nothing
        // bites, and the first slot inside it lands the row above its old
        // place. Where the table begins depends on the loaded fonts, so
        // the sweep does not hardcode it.
        let mut landed = None;
        for step in 0..45 {
            let pos = egui::pos2(700.0, 120.0 + step as f32 * 15.0);
            egui::DragAndDrop::set_payload(&ctx, held(from, &original[from]));
            frame_events(&ctx, &mut app, vec![egui::Event::PointerMoved(pos)]);
            frame_events(
                &ctx,
                &mut app,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            egui::DragAndDrop::clear_payload(&ctx);
            if app.playlist_busy {
                landed = Some(pos);
                break;
            }
        }
        let landed = landed.expect("no sweep position landed inside the table");
        let drop_at = |ctx: &egui::Context, app: &mut App, payload: DragTrack| {
            egui::DragAndDrop::set_payload(ctx, payload);
            frame_events(ctx, app, vec![egui::Event::PointerMoved(landed)]);
            frame_events(
                ctx,
                app,
                vec![egui::Event::PointerButton {
                    pos: landed,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            egui::DragAndDrop::clear_payload(ctx);
        };
        // The handler mirrored the move locally: the dragged row moved
        // up, everything else kept its order.
        let now = order(&app);
        let to = now
            .iter()
            .position(|uri| *uri == original[from])
            .expect("the dragged row vanished");
        assert!(to < from, "the row should have moved up, not to {to}");
        let mut expected = original.clone();
        let moved = expected.remove(from);
        expected.insert(to, moved);
        assert_eq!(now, expected);

        // Dropping the row on the same slot again moves nothing: the slot
        // is insert-before, so a row's own edges are a no-op. A slot sent
        // one row out would move it here.
        app.playlist_busy = false;
        drop_at(&ctx, &mut app, held(to, &expected[to]));
        assert!(!app.playlist_busy, "a row dropped on its own slot moved");
        assert_eq!(order(&app), expected);

        // A sorted view refuses the move: positions on screen no longer
        // match the server's.
        app.table_sorts.insert(
            Page::Playlist("pl1".into()),
            TableSort {
                column: SortColumn::Title,
                ascending: true,
            },
        );
        frame(&ctx, &mut app);
        drop_at(&ctx, &mut app, held(to, &expected[to]));
        assert!(!app.playlist_busy, "a sorted view accepted a move");
        assert_eq!(order(&app), expected);
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// The custom order is a setting like any other: it survives the trip
    /// through the settings file, and older files without it stay in the
    /// automatic order.
    #[test]
    fn custom_sidebar_order_round_trips_through_settings() {
        let settings = Settings {
            sidebar_order: vec![
                "spotify:playlist:pl4".to_string(),
                "spotify:playlist:pl0".to_string(),
            ],
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.sidebar_order, settings.sidebar_order);
        let older: Settings = serde_json::from_str("{}").unwrap();
        assert!(older.sidebar_order.is_empty());
    }

    /// Clicking the search icon in the library header reveals and focuses
    /// the sidebar search field.
    #[test]
    fn clicking_search_in_library_shelf_focuses_search_field() {
        let root = std::env::temp_dir().join(format!(
            "fastpotify-sidebar-search-focus-test-{}",
            std::process::id()
        ));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
        let mut app = App::new(
            &waker,
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        app.attach(&ctx);
        populate(&mut app);

        // Find the Y position of the Library header.
        let mut library_y = None;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 800.0),
            )),
            ..Default::default()
        };
        for _ in 0..2 {
            let mut output = ctx.run_ui(input.clone(), |ui| app.frame_ui(ui));
            output.textures_delta.clear();
            fn walk(shape: &egui::epaint::Shape, found: &mut Option<f32>) {
                match shape {
                    egui::epaint::Shape::Text(text) => {
                        if text.galley.job.text == "Library" {
                            *found = Some(text.pos.y);
                        }
                    }
                    egui::epaint::Shape::Vec(shapes) => {
                        shapes.iter().for_each(|shape| walk(shape, found));
                    }
                    _ => {}
                }
            }
            for clipped in &output.shapes {
                walk(&clipped.shape, &mut library_y);
            }
        }
        let y = library_y.expect("Library label was not found");
        let search_pos = egui::pos2(168.0, y + 4.0);

        // Click on the search button in the Library shelf header.
        frame_events(
            &ctx,
            &mut app,
            vec![
                egui::Event::PointerMoved(search_pos),
                egui::Event::PointerButton {
                    pos: search_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: search_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );

        // Advance one frame so the focused widget processes events.
        frame(&ctx, &mut app);

        // Verify the search field is shown and has keyboard focus.
        let search_id = egui::Id::new("sidebar-search");
        let has_focus = ctx.memory(|m| m.has_focus(search_id));
        assert!(
            has_focus,
            "sidebar-search must have keyboard focus after clicking the search icon"
        );

        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// The badges at the right end of the top bar sit in a right-to-left
    /// layout, which does not wrap and does not clip: anything that does not
    /// fit marches left over the search field. Draw the real bar and check
    /// that it never does.
    #[test]
    fn the_top_bar_badges_never_cover_the_search_field() {
        use crate::updates::{
            DownloadState,
            install::{Installation, Kind, Prepared},
        };
        use egui::accesskit::{Action as AccessibleAction, Role};
        // `widgets::search_field` insets its text this far from the pill's
        // right edge, so the pill reaches past the rect the field reports.
        const FIELD_RIGHT_INSET: f32 = 30.0;
        let (ctx, mut app) = accessible_app("topbar-badges");
        app.open(Page::Playlist("pl1".into()));
        for panel in [None, Some("queue"), Some("lyrics")] {
            app.show_queue_panel = panel == Some("queue");
            app.show_lyrics_panel = panel == Some("lyrics");
            for (label, state) in [
                (None, DownloadState::Idle),
                (Some("Update to 9.9.9"), DownloadState::Idle),
                (
                    Some("Downloading update…"),
                    DownloadState::Downloading {
                        received: 1,
                        total: 2,
                    },
                ),
                (
                    Some("Update ready"),
                    DownloadState::Ready(Box::new(Prepared {
                        installation: Installation {
                            executable: "/test/fastpotify".into(),
                            kind: Kind::Portable,
                        },
                        directory: "/test/stage".into(),
                        payload: "/test/stage/payload".into(),
                        version: "9.9.9".into(),
                        sha256: String::new(),
                    })),
                ),
            ] {
                app.update_download = state;
                app.update = label.map(|_| crate::updates::Release {
                    version: "9.9.9".into(),
                    url: "https://example.invalid/releases".into(),
                });
                // Keep the original 760-point coverage without a right panel,
                // and the reported 1080-point size with one. Full-height panel
                // placement at 760 points is checked independently below.
                let widths: &[f32] = if panel.is_some() {
                    &[1080.0, 1120.0, 1200.0, 1280.0, 1440.0, 1600.0, 1920.0]
                } else {
                    &[
                        760.0, 800.0, 860.0, 900.0, 1000.0, 1080.0, 1200.0, 1280.0, 1440.0, 1600.0,
                        1920.0,
                    ]
                };
                for &width in widths {
                    let narrowest = width == widths[0];
                    let mut draw = || {
                        let mut output = ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(width, 620.0),
                                )),
                                ..Default::default()
                            },
                            |ui| app.frame_ui(ui),
                        );
                        output.textures_delta.clear();
                        output
                            .platform_output
                            .accesskit_update
                            .expect("screen-reader tree")
                    };
                    // The first frame settles the new window size.
                    draw();
                    let tree = draw();
                    let badge = |label: &str| {
                        tree.nodes
                            .iter()
                            .find(|(_, node)| {
                                node.role() == Role::Button
                                    && node.label().is_some_and(|name| name.starts_with(label))
                            })
                            .and_then(|(_, node)| node.bounds())
                            .map(|bounds| bounds.x0 as f32)
                    };
                    let field = ctx
                        .read_response(egui::Id::new("global-search"))
                        .expect("the search field")
                        .rect
                        .right()
                        + FIELD_RIGHT_INSET;
                    // Collapsed to an icon a badge keeps its label for a screen
                    // reader, so it is found at every width.
                    let device = badge("Playing on").expect("the device badge");
                    assert!(
                        device >= field,
                        "the device badge covers {} px of the search field at {width} px",
                        field - device
                    );
                    if let Some(label) = label {
                        let release = badge(label).expect("the update badge");
                        assert!(
                            release >= field,
                            "the update badge covers {} px of the search field at {width} px",
                            field - release
                        );
                        if narrowest {
                            let button = accessible_node(&tree, label, Role::Button);
                            let mut output = ctx.run_ui(
                                egui::RawInput {
                                    screen_rect: Some(egui::Rect::from_min_size(
                                        egui::Pos2::ZERO,
                                        egui::vec2(width, 620.0),
                                    )),
                                    events: vec![accessible_action(
                                        button,
                                        AccessibleAction::Click,
                                        None,
                                    )],
                                    ..Default::default()
                                },
                                |ui| app.frame_ui(ui),
                            );
                            output.textures_delta.clear();
                            assert!(
                                app.show_update,
                                "the collapsed {label} badge must open the updater"
                            );
                            app.show_update = false;
                        }
                    }
                }
            }
        }
        app.backend.shutdown();
    }
    #[test]
    fn side_panels_keep_their_full_height_beside_the_page_toolbar() {
        for theme in ["dark", "light"] {
            let (ctx, mut app) = accessible_app(&format!("full-height-panels-{theme}"));
            app.open(Page::Playlist("pl1".into()));
            app.settings.theme = if theme == "light" {
                crate::settings::ThemeChoice::Light
            } else {
                crate::settings::ThemeChoice::Dark
            };
            app.actions.push(Action::SettingsChanged);
            for panel in ["queue", "lyrics"] {
                app.show_queue_panel = panel == "queue";
                app.show_lyrics_panel = panel == "lyrics";
                for width in [760.0, 1080.0, 1600.0] {
                    for _ in 0..3 {
                        let mut output = ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(egui::Rect::from_min_size(
                                    egui::Pos2::ZERO,
                                    egui::vec2(width, 800.0),
                                )),
                                ..Default::default()
                            },
                            |ui| app.frame_ui(ui),
                        );
                        output.textures_delta.clear();
                    }
                    let rect = |name: &str| {
                        egui::containers::panel::PanelState::load(&ctx, egui::Id::new(name))
                            .expect("the panel was drawn")
                            .outer_rect
                    };
                    let side = rect(&format!("{panel}-panel"));
                    let library = rect("sidebar");
                    let player = rect("player-bar");
                    assert_eq!(side.top(), library.top(), "{panel} at {width} in {theme}");
                    assert_eq!(side.top(), 0.0, "{panel} must start at the window top");
                    assert_eq!(side.bottom(), player.top());
                    let search = ctx.read_response(egui::Id::new("global-search")).unwrap();
                    assert!(side.top() < search.rect.top());
                }
            }
            app.backend.shutdown();
        }
    }
}
