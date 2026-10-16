//! The Home page.

use std::sync::Arc;

use egui::{CornerRadius, Rect, Sense, Vec2, pos2, vec2};

use crate::api::models::{Album, Episode, PlayableItem, Playlist, Show, pick_image};
use crate::app::App;
use crate::i18n::gettext;
use crate::model::{Action, Loadable, Page, RowContext, TopPeriod};
use crate::theme::{self, Icon};

use super::widgets::{self, TrackRow};

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    if !app.network_reachable {
        let space = ((ui.available_height() - 160.0) / 2.0).max(48.0);
        ui.add_space(space);
        ui.vertical_centered(|ui| {
            theme::icon(ui, Icon::WifiOff, 54.0, palette.dim);
            ui.add_space(16.0);
            theme::text(
                ui,
                gettext(app.locale, "You're offline"),
                theme::bold(28.0),
                palette.text,
            );
            ui.add_space(8.0);
            theme::text(
                ui,
                gettext(
                    app.locale,
                    "Spotifast will reconnect automatically when your connection returns.",
                ),
                theme::regular(14.0),
                palette.secondary,
            );
        });
        return;
    }
    ui.add_space(6.0);
    let greeting = crate::util::greeting(app.locale);
    theme::text(ui, greeting.as_ref(), theme::bold(30.0), palette.text);
    ui.add_space(12.0);
    quick_access(app, ui);
    ui.add_space(16.0);

    if app.settings.home.made_for_you.visible {
        made_for_you(app, ui);
        top_mixes(app, ui);
    }
    new_releases(app, ui);
    recently_played(app, ui);
    podcasts(app, ui);
    top_artists(app, ui);
    top_tracks(app, ui);
    if app.settings.home.recommendations.visible {
        recommendations(app, ui);
    }
}

struct Tile {
    image: Option<String>,
    name: String,
    page: Page,
    uri: Option<String>,
    liked: bool,
    owned_playlist: Option<Playlist>,
    /// A song played on its own, which the tile plays and whose menu it
    /// offers, rather than a playlist, album or artist.
    song: Option<crate::api::models::Track>,
}

/// The height of a quick-access tile, and so the side of its cover.
const TILE_HEIGHT: f32 = 60.0;
/// How many quick-access tiles Home shows, as Spotify does.
const TILES: usize = 8;

/// What was played most recently, as Spotify's Home shows it: the playlists,
/// albums, artists and podcasts songs were played from, Liked Songs, and
/// songs played on their own, newest first (#942). Until Spotify's play
/// history arrives, or when it holds fewer, Liked Songs and the first
/// playlists in the Library fill the rest.
fn quick_access_tiles(app: &App) -> Vec<Tile> {
    let tile_art = app.art_px(TILE_HEIGHT);
    let collection = app
        .user
        .as_ref()
        .map(|user| format!("spotify:user:{}:collection", user.id));
    let liked_tile = || Tile {
        image: None,
        name: gettext(app.locale, "Liked Songs").into_owned(),
        page: Page::LikedSongs,
        uri: collection.clone(),
        liked: true,
        owned_playlist: None,
        song: None,
    };
    let playlist_tile = |playlist: &Playlist| Tile {
        image: pick_image(&playlist.images, tile_art).map(str::to_string),
        name: playlist.name.clone(),
        page: Page::Playlist(playlist.id.clone()),
        uri: Some(playlist.uri.clone()),
        liked: false,
        owned_playlist: app
            .user_id()
            .is_some_and(|id| playlist.owned_by(id))
            .then(|| playlist.clone()),
        song: None,
    };
    let mut tiles: Vec<Tile> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for play in app.home.recently_played.get().into_iter().flatten() {
        if tiles.len() == TILES {
            break;
        }
        let track = &play.track;
        let context = play.context.as_ref().map(|context| context.uri.as_str());
        let tile = match context.map(|uri| (uri, uri.rsplit(':').next().unwrap_or(""))) {
            Some((uri, _)) if uri.ends_with(":collection") => Some(liked_tile()),
            Some((uri, id)) if uri.starts_with("spotify:playlist:") => {
                app.known_playlist(id).map(playlist_tile)
            }
            Some((uri, id)) if uri.starts_with("spotify:album:") => app
                .known_album(id)
                .or(track.album.as_ref().filter(|album| album.id == id))
                .map(|album| Tile {
                    image: pick_image(&album.images, tile_art).map(str::to_string),
                    name: album.name.clone(),
                    page: Page::Album(album.id.clone()),
                    uri: Some(uri.to_string()),
                    liked: false,
                    owned_playlist: None,
                    song: None,
                }),
            Some((uri, id)) if uri.starts_with("spotify:artist:") => {
                app.known_artist(id).map(|artist| Tile {
                    image: pick_image(&artist.images, tile_art).map(str::to_string),
                    name: artist.name.clone(),
                    page: Page::Artist(artist.id.clone()),
                    uri: Some(uri.to_string()),
                    liked: false,
                    owned_playlist: None,
                    song: None,
                })
            }
            Some((uri, id)) if uri.starts_with("spotify:show:") => {
                app.known_show(id).map(|show| Tile {
                    image: pick_image(&show.images, tile_art).map(str::to_string),
                    name: show.name.clone(),
                    page: Page::Show(show.id.clone()),
                    uri: Some(uri.to_string()),
                    liked: false,
                    owned_playlist: None,
                    song: None,
                })
            }
            // A song played on its own, or from somewhere Home cannot name.
            _ => track
                .album
                .as_ref()
                .filter(|album| !album.id.is_empty())
                .map(|album| Tile {
                    image: track.image(tile_art).map(str::to_string),
                    name: track.name.clone(),
                    page: Page::Album(album.id.clone()),
                    uri: Some(track.uri.clone()),
                    liked: false,
                    owned_playlist: None,
                    song: Some(track.clone()),
                }),
        };
        if let Some(tile) = tile
            && seen.insert(tile.page.clone())
        {
            tiles.push(tile);
        }
    }
    let library = app.library.playlists.get().into_iter().flatten();
    for tile in std::iter::once(liked_tile()).chain(library.map(playlist_tile)) {
        if tiles.len() == TILES {
            break;
        }
        if seen.insert(tile.page.clone()) {
            tiles.push(tile);
        }
    }
    tiles
}

fn quick_access(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let tiles = quick_access_tiles(app);
    let available = ui.available_width();
    let columns = ((available / 300.0).floor() as usize).clamp(2, 4);
    let gap = 10.0;
    let tile_width = (available - gap * (columns as f32 - 1.0)) / columns as f32;
    let rows = tiles.len().div_ceil(columns);
    for row in 0..rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for column in 0..columns {
                let Some(Tile {
                    image,
                    name,
                    page,
                    uri,
                    liked,
                    owned_playlist,
                    song,
                }) = tiles.get(row * columns + column)
                else {
                    break;
                };
                let (rect, response) =
                    ui.allocate_exact_size(vec2(tile_width, TILE_HEIGHT), Sense::click());
                if ui.is_rect_visible(rect) {
                    let hovered = ui.rect_contains_pointer(rect);
                    let fill = if hovered {
                        palette.surface_hover
                    } else {
                        palette.surface
                    };
                    ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
                    let cover = Rect::from_min_size(rect.min, Vec2::splat(TILE_HEIGHT));
                    if *liked {
                        super::sidebar::liked_cover(ui, cover, 6.0);
                    } else {
                        widgets::paint_cover(
                            ui,
                            &palette,
                            image.as_deref(),
                            cover,
                            6.0,
                            Icon::Music,
                            Some(app.backend.art()),
                        );
                    }
                    let play_room = if hovered && uri.is_some() { 52.0 } else { 12.0 };
                    let text_rect = Rect::from_min_max(
                        pos2(cover.right() + 12.0, rect.top()),
                        pos2(rect.right() - play_room, rect.bottom()),
                    );
                    crate::bidi::paint_line(
                        &ui.painter().with_clip_rect(text_rect),
                        text_rect.left(),
                        text_rect.right(),
                        rect.center().y,
                        name,
                        theme::bold(14.5),
                        palette.text,
                    );
                    if hovered && let Some(uri) = uri {
                        let context_here =
                            app.playing_context_uri().as_deref() == Some(uri.as_str());
                        let playing_here = context_here && app.believed_playing();
                        let button = Rect::from_center_size(
                            pos2(rect.right() - 28.0, rect.center().y),
                            Vec2::splat(40.0),
                        );
                        let mut child =
                            ui.new_child(egui::UiBuilder::new().max_rect(button).layout(
                                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                            ));
                        if theme::circle_button(
                            &mut child,
                            if playing_here {
                                Icon::PauseFilled
                            } else {
                                Icon::PlayFilled
                            },
                            40.0,
                            palette.accent,
                            palette.accent_hover,
                            palette.on_accent,
                            &gettext(app.locale, if playing_here { "Pause" } else { "Play" }),
                        )
                        .clicked()
                        {
                            if context_here {
                                app.actions.push(Action::TogglePlay);
                            } else if song.is_some() {
                                app.actions.push(Action::PlayUris {
                                    uris: vec![uri.clone()],
                                    index: 0,
                                });
                            } else {
                                app.actions.push(Action::PlayContext {
                                    uri: uri.clone(),
                                    offset_uri: None,
                                    offset_index: None,
                                });
                            }
                        }
                    }
                }
                if response.clicked() {
                    app.actions.push(Action::Open(page.clone()));
                }
                if !liked && let Some(uri) = uri {
                    egui::Popup::context_menu(&response)
                        .id(ui.make_persistent_id(("quick-access-menu", uri)))
                        .frame(widgets::menu_frame(&palette))
                        .show(|ui| match song {
                            Some(track) => widgets::item_menu(
                                ui,
                                app,
                                &PlayableItem::Track(track.clone()),
                                None,
                                None,
                            ),
                            None => widgets::context_menu_items(
                                ui,
                                app,
                                uri,
                                name,
                                owned_playlist.as_ref(),
                            ),
                        });
                }
            }
        });
    }
}

/// Renders session Home cards or approximate fallback cards in the same shelf.
fn made_for_you(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let (playlists, loading, failure) = match &app.home.made_for_you {
        Loadable::Loaded(playlists) => (playlists.clone(), false, None),
        Loadable::Loading | Loadable::NotLoaded => (Vec::new(), true, None),
        Loadable::Failed(error) => (Vec::new(), false, Some(error.clone())),
    };
    if playlists.is_empty() && !loading && failure.is_none() {
        return;
    }
    widgets::shelf(
        ui,
        &palette,
        "made-for-you",
        &gettext(app.locale, "Made for you"),
        |ui| {
            if playlists.is_empty() && loading {
                widgets::skeleton_cards(ui, &palette, app.locale, 7, false);
            } else if playlists.is_empty()
                && let Some(message) = &failure
            {
                widgets::error_row(ui, app, message, Some(Page::Home));
            }
            playlist_cards(app, ui, "home-made_for_you-menu", &playlists);
        },
    );
}

fn top_mixes(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let Some(playlists) = app
        .home
        .top_mixes
        .get()
        .filter(|playlists| !playlists.is_empty())
        .cloned()
    else {
        return;
    };
    widgets::shelf(
        ui,
        &palette,
        "top-mixes",
        &gettext(app.locale, "Your top mixes"),
        |ui| playlist_cards(app, ui, "home-top_mixes-menu", &playlists),
    );
}

/// The latest releases from followed artists. A card opens its release;
/// the title opens the page that plays all their songs, newest first.
fn new_releases(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let (albums, loading, failure) = match &app.home.new_releases {
        Loadable::Loaded(albums) => (albums.clone(), false, None),
        Loadable::Loading | Loadable::NotLoaded => (Vec::new(), true, None),
        Loadable::Failed(error) => (Vec::new(), false, Some(error.clone())),
    };
    if albums.is_empty() && !loading && failure.is_none() {
        return;
    }
    let open = widgets::linked_shelf(
        ui,
        &palette,
        "new-releases",
        &gettext(app.locale, "New releases"),
        |ui| {
            if albums.is_empty() && loading {
                widgets::loading_row(ui, &palette, app.locale);
            } else if albums.is_empty()
                && let Some(message) = &failure
            {
                widgets::error_row(ui, app, message, Some(Page::Home));
            }
            for album in &albums {
                release_card(app, ui, album);
            }
        },
    );
    if open {
        app.actions.push(Action::Open(Page::NewReleases));
    }
}

fn release_card(app: &mut App, ui: &mut egui::Ui, album: &Album) {
    let palette = app.palette;
    let artists =
        crate::api::models::join_names(album.artists.iter().map(|artist| artist.name.as_str()));
    let subtitle = if artists.is_empty() {
        album.kind_label(app.locale).into_owned()
    } else {
        format!("{} • {artists}", album.kind_label(app.locale))
    };
    let context_here = app.playing_context_uri().as_deref() == Some(album.uri.as_str());
    let playing_here = context_here && app.believed_playing();
    let card_art = app.art_px(widgets::CARD_ART);
    let card = widgets::card(
        ui,
        app,
        pick_image(&album.images, card_art),
        &album.name,
        &subtitle,
        widgets::CardCover::square(playing_here),
    );
    if card.play {
        if context_here {
            app.actions.push(Action::TogglePlay);
        } else {
            app.actions.push(Action::PlayContext {
                uri: album.uri.clone(),
                offset_uri: None,
                offset_index: None,
            });
        }
    }
    if card.clicked {
        app.actions
            .push(Action::Open(Page::Album(album.id.clone())));
    }
    egui::Popup::context_menu(&card.response)
        .id(ui.make_persistent_id(("home-new-releases-menu", &album.uri)))
        .frame(widgets::menu_frame(&palette))
        .show(|ui| {
            widgets::context_menu_items(ui, app, &album.uri, &album.name, None);
        });
}

fn playlist_cards(app: &mut App, ui: &mut egui::Ui, menu: &str, playlists: &[Playlist]) {
    let palette = app.palette;
    for playlist in playlists {
        let subtitle = playlist
            .description
            .as_deref()
            .map(crate::util::strip_html)
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| {
                // Translators: {owner} is the name of the playlist's owner.
                gettext(app.locale, "By {owner}").replace("{owner}", playlist.owner_name())
            });
        let context_here = app.playing_context_uri().as_deref() == Some(playlist.uri.as_str());
        let playing_here = context_here && app.believed_playing();
        let card_art = app.art_px(widgets::CARD_ART);
        let card = widgets::card(
            ui,
            app,
            pick_image(&playlist.images, card_art),
            &playlist.name,
            &subtitle,
            widgets::CardCover::square(playing_here),
        );
        if card.play {
            if context_here {
                app.actions.push(Action::TogglePlay);
            } else {
                app.actions.push(Action::PlayContext {
                    uri: playlist.uri.clone(),
                    offset_uri: None,
                    offset_index: None,
                });
            }
        }
        if card.clicked {
            app.actions
                .push(Action::Open(Page::Playlist(playlist.id.clone())));
        }
        egui::Popup::context_menu(&card.response)
            .id(ui.make_persistent_id((menu, &playlist.uri)))
            .frame(widgets::menu_frame(&palette))
            .show(|ui| {
                let owned = app.user_id().is_some_and(|id| playlist.owned_by(id));
                widgets::context_menu_items(
                    ui,
                    app,
                    &playlist.uri,
                    &playlist.name,
                    owned.then_some(playlist),
                );
            });
    }
}

fn recently_played(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let history = match app.home.recently_played.clone() {
        Loadable::Loaded(history) => history,
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::shelf(
                ui,
                &palette,
                "recent",
                &gettext(app.locale, "Recently played"),
                |ui| widgets::skeleton_cards(ui, &palette, app.locale, 7, false),
            );
            return;
        }
        Loadable::Failed(message) => {
            widgets::shelf(
                ui,
                &palette,
                "recent",
                &gettext(app.locale, "Recently played"),
                |ui| {
                    widgets::error_row(ui, app, &message, Some(Page::Home));
                },
            );
            return;
        }
    };
    let mut seen = std::collections::HashSet::new();
    let tracks: Vec<_> = history
        .into_iter()
        .filter(|entry| {
            entry
                .track
                .id
                .as_ref()
                .is_some_and(|id| seen.insert(id.clone()))
        })
        .take(16)
        .collect();
    if tracks.is_empty() {
        return;
    }
    widgets::shelf(
        ui,
        &palette,
        "recent",
        &gettext(app.locale, "Recently played"),
        |ui| {
            for entry in &tracks {
                let track = &entry.track;
                let card_art = app.art_px(widgets::CARD_ART);
                let card = widgets::card(
                    ui,
                    app,
                    track.image(card_art),
                    &track.name,
                    &track.artist_names(),
                    widgets::CardCover::square(false),
                );
                if card.play {
                    app.actions.push(Action::PlayUris {
                        uris: vec![track.uri.clone()],
                        index: 0,
                    });
                }
                if card.clicked
                    && let Some(album) = &track.album
                    && !album.id.is_empty()
                {
                    app.actions
                        .push(Action::Open(Page::Album(album.id.clone())));
                }
                egui::Popup::context_menu(&card.response)
                    .id(ui.make_persistent_id(("home-recently_played-menu", &track.uri)))
                    .frame(widgets::menu_frame(&palette))
                    .show(|ui| {
                        widgets::item_menu(
                            ui,
                            app,
                            &PlayableItem::Track(track.clone()),
                            None,
                            None,
                        );
                    });
            }
        },
    );
}

/// Why an episode is on the podcast shelf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EpisodeReason {
    /// Started and not finished, with this much left.
    Continue { left_ms: u32 },
    /// The show's newest episode, recently released and not yet started.
    New,
}

/// How many days after its release an unplayed episode still counts as new.
const NEW_EPISODE_DAYS: i64 = 30;
/// The podcast shelf's card limit, as for Recently played.
const PODCAST_CARDS: usize = 16;

/// The episodes on the podcast shelf: those to continue first, then each
/// show's newest unstarted episode from the last month, each group newest
/// first. `skip` leaves out shows that are audiobooks or no longer saved.
pub(crate) fn podcast_episodes(
    podcasts: &[(Show, Vec<Episode>)],
    skip: impl Fn(&Show) -> bool,
    today: jiff::civil::Date,
) -> Vec<(Show, Episode, EpisodeReason)> {
    let oldest_new = today
        .checked_sub(jiff::Span::new().days(NEW_EPISODE_DAYS))
        .unwrap_or(today);
    let released = |episode: &Episode| {
        episode
            .release_date
            .as_deref()
            .and_then(|date| date.get(..10))
            .and_then(|date| date.parse::<jiff::civil::Date>().ok())
    };
    let mut continuing = Vec::new();
    let mut new = Vec::new();
    for (show, episodes) in podcasts {
        if skip(show) {
            continue;
        }
        for episode in episodes {
            if let Some(resume) = &episode.resume_point
                && !resume.fully_played
                && resume.resume_position_ms > 0
            {
                let left_ms = episode
                    .duration_ms
                    .saturating_sub(resume.resume_position_ms);
                continuing.push((show, episode, EpisodeReason::Continue { left_ms }));
            }
        }
        // Spotify lists a show's episodes newest first.
        if let Some(newest) = episodes.first()
            && newest
                .resume_point
                .as_ref()
                .is_none_or(|resume| !resume.fully_played && resume.resume_position_ms == 0)
            && released(newest).is_some_and(|date| date >= oldest_new)
        {
            new.push((show, newest, EpisodeReason::New));
        }
    }
    for group in [&mut continuing, &mut new] {
        group.sort_by_key(|(_, episode, _)| std::cmp::Reverse(released(episode)));
    }
    let mut seen = std::collections::HashSet::new();
    continuing
        .into_iter()
        .chain(new)
        .filter(|(_, episode, _)| !episode.uri.is_empty() && seen.insert(&episode.uri))
        .take(PODCAST_CARDS)
        .map(|(show, episode, reason)| {
            let mut episode = episode.clone();
            // A show's episode list leaves out the show; menus need it.
            if episode.show.is_none() {
                episode.show = Some(show.clone());
            }
            (show.clone(), episode, reason)
        })
        .collect()
}

fn podcasts(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let episodes = podcast_episodes(
        &app.home.podcasts,
        |show| app.audiobook_shows.contains(&show.uri) || app.is_saved(&show.uri) == Some(false),
        jiff::Zoned::now().date(),
    );
    if episodes.is_empty() {
        return;
    }
    widgets::shelf(
        ui,
        &palette,
        "podcasts",
        &gettext(app.locale, "Your podcasts"),
        |ui| {
            for (show, episode, reason) in &episodes {
                let subtitle = match reason {
                    EpisodeReason::Continue { left_ms } => {
                        // Translators: {time} is the time left in an episode, such as 12 min; {show} is the podcast's name.
                        gettext(app.locale, "{time} left • {show}")
                            .replace(
                                "{time}",
                                &crate::util::format_episode_ms(app.locale, *left_ms),
                            )
                            .replace("{show}", &show.name)
                    }
                    EpisodeReason::New => {
                        // Translators: {show} is the podcast's name.
                        gettext(app.locale, "New • {show}").replace("{show}", &show.name)
                    }
                };
                let card_art = app.art_px(widgets::CARD_ART);
                let card = widgets::card(
                    ui,
                    app,
                    pick_image(&episode.images, card_art)
                        .or_else(|| pick_image(&show.images, card_art)),
                    &episode.name,
                    &subtitle,
                    widgets::CardCover::square(false),
                );
                if card.play {
                    app.actions.push(Action::PlayEpisode {
                        uri: episode.uri.clone(),
                        resume_ms: episode.resume_ms(),
                    });
                }
                if card.clicked && !show.id.is_empty() {
                    app.actions.push(Action::Open(Page::Show(show.id.clone())));
                }
                egui::Popup::context_menu(&card.response)
                    .id(ui.make_persistent_id(("home-podcasts-menu", &episode.uri)))
                    .frame(widgets::menu_frame(&palette))
                    .show(|ui| {
                        widgets::item_menu(
                            ui,
                            app,
                            &PlayableItem::Episode(episode.clone()),
                            None,
                            None,
                        );
                    });
            }
        },
    );
}

fn top_artists(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let artists = match app.home.top_artists.clone() {
        Loadable::Loaded(artists) => artists,
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::shelf(
                ui,
                &palette,
                "top-artists",
                &gettext(app.locale, "Your top artists"),
                |ui| widgets::skeleton_cards(ui, &palette, app.locale, 7, true),
            );
            return;
        }
        Loadable::Failed(message) => {
            widgets::shelf(
                ui,
                &palette,
                "top-artists",
                &gettext(app.locale, "Your top artists"),
                |ui| {
                    widgets::error_row(ui, app, &message, Some(Page::Home));
                },
            );
            return;
        }
    };
    if artists.is_empty() {
        return;
    }
    widgets::shelf(
        ui,
        &palette,
        "top-artists",
        &gettext(app.locale, "Your top artists"),
        |ui| {
            for artist in &artists {
                let context_here =
                    app.playing_context_uri().as_deref() == Some(artist.uri.as_str());
                let playing_here = context_here && app.believed_playing();
                let card_art = app.art_px(widgets::CARD_ART);
                let card = widgets::card(
                    ui,
                    app,
                    pick_image(&artist.images, card_art),
                    &artist.name,
                    &gettext(app.locale, "Artist"),
                    widgets::CardCover::portrait(playing_here),
                );
                if card.play {
                    if context_here {
                        app.actions.push(Action::TogglePlay);
                    } else {
                        app.actions.push(Action::PlayContext {
                            uri: artist.uri.clone(),
                            offset_uri: None,
                            offset_index: None,
                        });
                    }
                }
                if card.clicked {
                    app.actions
                        .push(Action::Open(Page::Artist(artist.id.clone())));
                }
                egui::Popup::context_menu(&card.response)
                    .id(ui.make_persistent_id(("home-top_artists-menu", &artist.uri)))
                    .frame(widgets::menu_frame(&palette))
                    .show(|ui| {
                        widgets::context_menu_items(ui, app, &artist.uri, &artist.name, None);
                    });
            }
        },
    );
}

fn track_list(
    app: &mut App,
    ui: &mut egui::Ui,
    title: &str,
    tracks: Loadable<Vec<crate::api::models::Track>>,
    limit: usize,
    title_page: Option<Page>,
    more_label: Option<&str>,
) {
    let palette = app.palette;
    let tracks = match tracks {
        Loadable::Loaded(tracks) => tracks,
        Loadable::Loading | Loadable::NotLoaded => {
            if let Some(page) = title_page {
                if theme::link(ui, title, theme::bold(17.0), palette.text).clicked() {
                    app.actions.push(Action::Open(page));
                }
            } else {
                theme::section_title(ui, &palette, title);
            }
            widgets::skeleton_rows(ui, &palette, app.locale, 4);
            ui.add_space(12.0);
            return;
        }
        Loadable::Failed(message) => {
            theme::section_title(ui, &palette, title);
            widgets::error_row(ui, app, &message, Some(title_page.unwrap_or(Page::Home)));
            ui.add_space(12.0);
            return;
        }
    };
    if tracks.is_empty() {
        return;
    }
    if let Some(page) = title_page {
        if theme::link(ui, title, theme::bold(17.0), palette.text).clicked() {
            app.actions.push(Action::Open(page));
        }
    } else {
        theme::section_title(ui, &palette, title);
    }
    ui.add_space(4.0);
    let uris: Arc<[String]> = tracks
        .iter()
        .map(|track| track.uri.clone())
        .collect::<Vec<_>>()
        .into();
    let context = RowContext::Uris(Arc::clone(&uris));
    for (index, track) in tracks.iter().take(limit).enumerate() {
        let item = PlayableItem::Track(track.clone());
        widgets::track_row(
            ui,
            app,
            TrackRow {
                index,
                number: None,
                item: &item,
                context: &context,
                show_cover: !app.settings.tracklist_compact,
                show_album: true,
                added_at: None,
                added_by: None,
                show_added_by: false,
                show_plays: false,
                compact: false,
                thin: app.settings.tracklist_compact,
                shift: 0.0,
                picked: false,
                picked_songs: &[],
                picked_positions: &[],
                position: None,
            },
        );
    }
    if let Some(label) = more_label
        && tracks.len() > limit
        && theme::link(ui, label, theme::semibold(14.0), palette.secondary).clicked()
    {
        app.actions.push(Action::Open(Page::TopSongs));
    }
    ui.add_space(16.0);
}

fn top_tracks(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let title = gettext(app.locale, "Your top songs");
    let more = gettext(app.locale, "Show more top songs");
    let tracks = app.home.top_tracks.clone();

    if theme::link(ui, &*title, theme::bold(17.0), palette.text).clicked() {
        app.actions.push(Action::Open(Page::TopSongs));
    }
    ui.add_space(4.0);

    let labels: Vec<_> = TopPeriod::ALL
        .iter()
        .map(|p| (*p, p.label(app.locale)))
        .collect();
    let options: Vec<(TopPeriod, &str)> = labels
        .iter()
        .map(|(period, label)| (*period, label.as_ref()))
        .collect();
    if let Some(period) = widgets::chips(ui, &palette, &options, app.home.top_period) {
        app.actions.push(Action::SetTopPeriod(period));
    }
    ui.add_space(8.0);

    let tracks = match tracks {
        Loadable::Loaded(tracks) => tracks,
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::loading_row(ui, &palette, app.locale);
            ui.add_space(12.0);
            return;
        }
        Loadable::Failed(message) => {
            widgets::error_row(ui, app, &message, Some(Page::TopSongs));
            ui.add_space(12.0);
            return;
        }
    };

    let uris: Arc<[String]> = tracks
        .iter()
        .map(|track| track.uri.clone())
        .collect::<Vec<_>>()
        .into();
    let context = RowContext::Uris(Arc::clone(&uris));
    for (index, track) in tracks.iter().take(10).enumerate() {
        let item = PlayableItem::Track(track.clone());
        widgets::track_row(
            ui,
            app,
            TrackRow {
                index,
                number: None,
                item: &item,
                context: &context,
                show_cover: !app.settings.tracklist_compact,
                show_album: true,
                added_at: None,
                added_by: None,
                show_added_by: false,
                show_plays: false,
                compact: false,
                thin: app.settings.tracklist_compact,
                shift: 0.0,
                picked: false,
                picked_songs: &[],
                picked_positions: &[],
                position: None,
            },
        );
    }
    if tracks.len() > 10
        && theme::link(ui, &*more, theme::semibold(14.0), palette.secondary).clicked()
    {
        app.actions.push(Action::Open(Page::TopSongs));
    }
    ui.add_space(16.0);
}

fn recommendations(app: &mut App, ui: &mut egui::Ui) {
    let tracks = app.home.recommendations.clone();
    let title = gettext(app.locale, "Recommended for you");
    track_list(app, ui, &title, tracks, 20, None, None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::ResumePoint;

    fn show(id: &str) -> Show {
        Show {
            id: id.into(),
            uri: format!("spotify:show:{id}"),
            name: id.to_uppercase(),
            ..Show::default()
        }
    }

    fn episode(id: &str, date: &str, played: Option<u32>, finished: bool) -> Episode {
        Episode {
            id: id.into(),
            uri: format!("spotify:episode:{id}"),
            duration_ms: 3_600_000,
            release_date: Some(date.into()),
            resume_point: Some(ResumePoint {
                fully_played: finished,
                resume_position_ms: played.unwrap_or(0),
            }),
            ..Episode::default()
        }
    }

    fn ids(shelf: &[(Show, Episode, EpisodeReason)]) -> Vec<&str> {
        shelf
            .iter()
            .map(|(_, episode, _)| episode.id.as_str())
            .collect()
    }

    /// A tile's cover is 60 points. Spotify's 64 pixel rendition would be
    /// blurry even at 100%, so the tile takes the 300 pixel one and scales
    /// it down; at 300% the 300 pixel one still has a quarter more pixels
    /// than the tile shows.
    #[cfg(feature = "demo")]
    #[test]
    fn quick_access_tiles_ask_for_art_the_display_can_show() {
        let root = std::env::temp_dir().join(format!("spotifast-tiles-{}", std::process::id()));
        let mut app = App::new(
            &crate::backend::Waker::default(),
            crate::paths::AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            crate::settings::Settings::default(),
            crate::app::AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
                autostart: false,
            },
        );
        crate::demo::populate(&mut app);
        // Only the Library's playlists, so the second tile is the first one.
        app.home.recently_played = Loadable::Loaded(Vec::new());
        let first = app.library.playlists.get().unwrap()[0].images.clone();
        let width = |url: Option<&str>| {
            first
                .iter()
                .find(|image| Some(image.url.as_str()) == url)
                .and_then(|image| image.width)
        };
        app.pixels_per_point = 1.0;
        assert_eq!(
            width(quick_access_tiles(&app)[1].image.as_deref()),
            Some(300)
        );
        app.pixels_per_point = 3.0;
        assert_eq!(
            width(quick_access_tiles(&app)[1].image.as_deref()),
            Some(300)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Home's tiles follow what was played, newest first: the playlist,
    /// album or Liked Songs a song came from, or the song itself, each once.
    /// The Library's playlists fill what history leaves, and stand in until
    /// it arrives (#942).
    #[cfg(feature = "demo")]
    #[test]
    fn quick_access_tiles_follow_what_was_played() {
        let root =
            std::env::temp_dir().join(format!("spotifast-recent-tiles-{}", std::process::id()));
        let mut app = App::new(
            &crate::backend::Waker::default(),
            crate::paths::AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            crate::settings::Settings::default(),
            crate::app::AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
                autostart: false,
            },
        );
        crate::demo::populate(&mut app);
        let history = app.home.recently_played.get().unwrap().clone();
        let tiles = quick_access_tiles(&app);
        assert_eq!(tiles.len(), TILES);
        assert_eq!(
            tiles[0].page,
            Page::Playlist("pl1".into()),
            "played from it"
        );
        let song = &history[2].track;
        assert_eq!(
            tiles[1].uri.as_deref(),
            Some(song.uri.as_str()),
            "played on its own"
        );
        assert!(tiles[1].song.is_some());
        let pages: Vec<_> = tiles.iter().map(|tile| tile.page.clone()).collect();
        assert_eq!(
            pages
                .iter()
                .filter(|page| **page == Page::LikedSongs)
                .count(),
            1
        );
        let unique: std::collections::HashSet<_> = pages.iter().collect();
        assert_eq!(unique.len(), pages.len(), "each once");

        app.home.recently_played = Loadable::Loading;
        let tiles = quick_access_tiles(&app);
        assert_eq!(
            tiles[0].page,
            Page::LikedSongs,
            "the Library until history arrives"
        );
        let first = app.library.playlists.get().unwrap()[0].id.clone();
        assert_eq!(tiles[1].page, Page::Playlist(first));
        assert_eq!(tiles.len(), TILES);
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    fn today() -> jiff::civil::Date {
        "2026-09-23".parse().unwrap()
    }

    #[test]
    fn episodes_to_continue_come_before_new_ones() {
        let podcasts = vec![
            (
                show("a"),
                vec![
                    episode("a-new", "2026-09-20", None, false),
                    episode("a-started", "2026-09-01", Some(600_000), false),
                    episode("a-done", "2026-08-25", Some(0), true),
                ],
            ),
            (
                show("b"),
                vec![
                    episode("b-started", "2026-09-10", Some(60_000), false),
                    episode("b-old", "2026-09-03", None, false),
                ],
            ),
            (show("c"), vec![episode("c-new", "2026-09-22", None, false)]),
        ];
        let shelf = podcast_episodes(&podcasts, |_| false, today());
        assert_eq!(ids(&shelf), ["b-started", "a-started", "c-new", "a-new"]);
        assert_eq!(shelf[1].2, EpisodeReason::Continue { left_ms: 3_000_000 });
        assert_eq!(shelf[2].2, EpisodeReason::New);
        assert_eq!(
            shelf[0].1.show.as_ref().map(|show| show.id.as_str()),
            Some("b"),
            "the episode menu can go to its podcast"
        );
    }

    #[test]
    fn only_a_recent_unstarted_newest_episode_is_new() {
        let podcasts = vec![
            (show("old"), vec![episode("old", "2026-07-01", None, false)]),
            (
                show("finished"),
                vec![episode("finished", "2026-09-20", Some(0), true)],
            ),
            (
                show("undated"),
                vec![episode("undated", "2026", None, false)],
            ),
            (
                show("second"),
                vec![
                    episode("second-done", "2026-09-21", Some(0), true),
                    episode("second-new", "2026-09-20", None, false),
                ],
            ),
        ];
        assert!(podcast_episodes(&podcasts, |_| false, today()).is_empty());
    }

    #[test]
    fn skipped_shows_leave_the_shelf_and_it_stays_bounded() {
        let podcasts: Vec<_> = (0..20)
            .map(|index| {
                let id = format!("s{index}");
                let started = episode(&format!("{id}-e"), "2026-09-01", Some(1), false);
                (show(&id), vec![started])
            })
            .collect();
        let shelf = podcast_episodes(&podcasts, |show| show.id == "s0", today());
        assert_eq!(shelf.len(), PODCAST_CARDS);
        assert!(!ids(&shelf).contains(&"s0-e"));
    }
}
