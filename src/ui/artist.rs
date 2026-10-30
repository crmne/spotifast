//! The artist page.

use std::sync::Arc;

use crate::api::models::{Artist, PlayableItem, pick_image};
use crate::app::App;
use crate::i18n::{gettext, ngettext, pgettext};
use crate::model::{Action, DiscographyFilter, Loadable, Page, RowContext};
use crate::theme::{self, Icon};
use crate::util;

use super::collection::{Hero, HeroArt, hero, hero_images};
use super::widgets::{self, TrackRow};

pub fn show(app: &mut App, ui: &mut egui::Ui, id: &str) {
    if !app.artist_pages.contains_key(id) {
        app.ensure_loaded(Page::Artist(id.to_string()));
    }
    let Some(page) = app.artist_pages.remove(id) else {
        return;
    };
    let preview =
        super::loading_preview(ui.ctx(), id, &page.artist, || app.known_artist(id).cloned());
    let palette = app.palette;
    let locale = app.locale;
    match &page.artist {
        Loadable::Loaded(artist) => {
            artist_hero(app, ui, artist, preview.as_deref());
            artist_actions(app, ui, artist);
            // The same overview supplies both play counts and the upcoming release.
            app.request_play_counts(&artist.uri);
            // Popular.
            theme::section_title(ui, &palette, &gettext(locale, "Popular"));
            ui.add_space(4.0);
            match &page.top_tracks {
                Loadable::Loaded(tracks) if !tracks.is_empty() => {
                    let uris: Arc<[String]> = tracks
                        .iter()
                        .map(|track| track.uri.clone())
                        .collect::<Vec<_>>()
                        .into();
                    let context = RowContext::Uris(Arc::clone(&uris));
                    let items: Vec<PlayableItem> =
                        tracks.iter().cloned().map(PlayableItem::Track).collect();
                    let limit = if page.show_all_top { items.len() } else { 5 };
                    let show_plays = app.has_play_counts(&artist.uri);
                    let render_top = |ui: &mut egui::Ui, app: &mut App| {
                        for (index, item) in items.iter().take(limit).enumerate() {
                            widgets::track_row(
                                ui,
                                app,
                                TrackRow {
                                    index,
                                    number: Some(index + 1),
                                    item,
                                    context: &context,
                                    show_cover: !app.settings.tracklist_compact,
                                    show_album: false,
                                    added_at: None,
                                    added_by: None,
                                    show_added_by: false,
                                    show_plays,
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
                    };
                    if !app.network_reachable {
                        ui.add_enabled_ui(false, |ui| {
                            ui.multiply_opacity(0.55);
                            render_top(ui, app);
                        });
                    } else {
                        render_top(ui, app);
                    }
                    if items.len() > 5 {
                        ui.add_space(6.0);
                        if theme::soft_button(
                            ui,
                            &palette,
                            None,
                            &if page.show_all_top {
                                gettext(locale, "Show less")
                            } else {
                                gettext(locale, "See more")
                            },
                            false,
                        )
                        .clicked()
                        {
                            app.actions.push(Action::ToggleShowAllTop(id.to_string()));
                        }
                    }
                }
                Loadable::Loaded(_) => {
                    theme::subtle(ui, &palette, &gettext(locale, "No popular songs to show."));
                }
                Loadable::Loading | Loadable::NotLoaded => {
                    widgets::skeleton_rows(ui, &palette, app.locale, 5)
                }
                Loadable::Failed(error) => {
                    let error = error.clone();
                    widgets::error_row(ui, app, &error, None);
                }
            }
            ui.add_space(20.0);

            if let Some(upcoming) = app.artist_prereleases.get(&artist.uri).cloned()
                && upcoming.is_upcoming(jiff::Timestamp::now().as_second())
            {
                upcoming_release(app, ui, &upcoming);
                ui.add_space(32.0);
            }

            // Discography.
            theme::section_title(ui, &palette, &gettext(locale, "Discography"));
            ui.add_space(6.0);
            let labels: Vec<_> = DiscographyFilter::ALL
                .iter()
                .map(|f| (*f, f.label(locale)))
                .collect();
            let options: Vec<(DiscographyFilter, &str)> = labels
                .iter()
                .map(|(filter, label)| (*filter, label.as_ref()))
                .collect();
            if let Some(filter) = widgets::chips(ui, &palette, &options, page.filter) {
                app.actions.push(Action::SetDiscographyFilter {
                    artist_id: id.to_string(),
                    filter,
                });
            }
            ui.add_space(10.0);
            match page.albums.get(page.filter.groups()) {
                Some(list) => {
                    let mut seen = std::collections::HashSet::new();
                    let albums: Vec<_> = list
                        .items
                        .iter()
                        .filter(|album| seen.insert(album.name.to_lowercase()))
                        .collect();
                    widgets::grid(ui, |ui| {
                        for album in &albums {
                            let subtitle = format!(
                                "{} • {}",
                                album.year().unwrap_or(""),
                                app.album_kind_label(album)
                            );
                            let context_here =
                                app.playing_context_uri().as_deref() == Some(album.uri.as_str());
                            let playing_here = context_here && app.believed_playing();
                            let card_art = app.art_px(widgets::CARD_ART);
                            let card = widgets::card(
                                ui,
                                app,
                                pick_image(&album.images, card_art),
                                &album.name,
                                subtitle.trim_start_matches(" • "),
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
                                .id(ui.make_persistent_id(("discography-menu", &album.uri)))
                                .frame(widgets::menu_frame(&palette))
                                .show(|ui| {
                                    widgets::context_menu_items(
                                        ui,
                                        app,
                                        &album.uri,
                                        &album.name,
                                        None,
                                    )
                                });
                        }
                    });
                    if list.loading {
                        widgets::loading_row(ui, &palette, app.locale);
                    } else if let Some(error) = &list.error {
                        let error = error.clone();
                        widgets::error_row(ui, app, &error, None);
                    } else if list.items.is_empty() {
                        theme::subtle(ui, &palette, &gettext(locale, "Nothing in this category."));
                    } else if list.can_load_more() {
                        ui.add_space(8.0);
                        if theme::soft_button(
                            ui,
                            &palette,
                            None,
                            &gettext(locale, "Load more"),
                            false,
                        )
                        .clicked()
                        {
                            app.actions
                                .push(Action::LoadMoreArtistAlbums(id.to_string()));
                        }
                    }
                }
                None => widgets::grid(ui, |ui| {
                    widgets::skeleton_cards(ui, &palette, app.locale, 6, false)
                }),
            }
            ui.add_space(20.0);

            // Related.
            if let Loadable::Loaded(related) = &page.related
                && !related.is_empty()
            {
                let artist_label = gettext(locale, "Artist");
                let title = gettext(locale, "Fans also like");
                widgets::shelf(ui, &palette, "related", &title, |ui| {
                    for artist in related {
                        let context_here =
                            app.playing_context_uri().as_deref() == Some(artist.uri.as_str());
                        let playing_here = context_here && app.believed_playing();
                        let card_art = app.art_px(widgets::CARD_ART);
                        let card = widgets::card(
                            ui,
                            app,
                            pick_image(&artist.images, card_art),
                            &artist.name,
                            &artist_label,
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
                            .id(ui.make_persistent_id(("related-artist-menu", &artist.uri)))
                            .frame(widgets::menu_frame(&palette))
                            .show(|ui| {
                                widgets::context_menu_items(
                                    ui,
                                    app,
                                    &artist.uri,
                                    &artist.name,
                                    None,
                                )
                            });
                    }
                });
            }
        }
        Loadable::Loading | Loadable::NotLoaded => {
            if let Some(artist) = &preview {
                artist_hero(app, ui, artist, None);
            } else {
                ui.add_space(40.0);
            }
            if !app.network_reachable {
                widgets::empty_state(
                    ui,
                    &palette,
                    Icon::WifiOff,
                    &gettext(app.locale, "You're offline"),
                    &gettext(
                        app.locale,
                        "Spotifast will reconnect automatically when your connection returns.",
                    ),
                );
            } else {
                if let Some(artist) = &preview {
                    ui.add_enabled_ui(false, |ui| artist_actions(app, ui, artist));
                }
                widgets::skeleton_rows(ui, &palette, app.locale, 5);
            }
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            if let Some(artist) = &preview {
                artist_hero(app, ui, artist, None);
            } else {
                ui.add_space(40.0);
            }
            if !app.network_reachable
                || error.contains("network error")
                || error.contains("Network connection error")
            {
                widgets::empty_state(
                    ui,
                    &palette,
                    Icon::WifiOff,
                    &gettext(app.locale, "You're offline"),
                    &gettext(
                        app.locale,
                        "Spotifast will reconnect automatically when your connection returns.",
                    ),
                );
            } else {
                if let Some(artist) = &preview {
                    ui.add_enabled_ui(false, |ui| artist_actions(app, ui, artist));
                }
                widgets::error_row(ui, app, &error, Some(Page::Artist(id.to_string())));
            }
        }
    }
    app.artist_pages.insert(id.to_string(), page);
}

fn upcoming_release(
    app: &mut App,
    ui: &mut egui::Ui,
    release: &crate::prerelease::UpcomingRelease,
) {
    let palette = app.palette;
    let heading = gettext(app.locale, "Release countdown");
    theme::section_title(ui, &palette, &heading);
    ui.add_space(12.0);
    let album_label = gettext(app.locale, "Album");
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(440.0), 96.0),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("{heading}: {}", release.name),
        )
    });
    if response.gained_focus() {
        response.scroll_to_me(None);
    }
    ui.painter().rect_filled(
        rect,
        theme::RADIUS as f32,
        if response.hovered() || response.has_focus() {
            palette.surface_hover
        } else {
            egui::Color32::TRANSPARENT
        },
    );
    let cover = egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 4.0), egui::Vec2::splat(88.0));
    widgets::paint_cover(
        ui,
        &palette,
        pick_image(&release.images, app.art_px(88.0)),
        cover,
        4.0,
        Icon::Music,
        Some(app.backend.art()),
    );
    let left = cover.right() + 12.0;
    let right = rect.right() - 12.0;
    for (text, y, font, color) in [
        (
            release.name.as_str(),
            28.0,
            theme::semibold(16.0),
            palette.text,
        ),
        (
            album_label.as_ref(),
            52.0,
            theme::regular(13.0),
            palette.secondary,
        ),
    ] {
        let galley = widgets::ellipsized(ui, text, font, color, (right - left).max(1.0), 1);
        let x = if galley.job.halign == egui::Align::RIGHT {
            right
        } else {
            left
        };
        ui.painter()
            .galley(egui::pos2(x, rect.top() + y), galley, color);
    }
    if response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(&release.name)
        .clicked()
    {
        app.actions.push(Action::OpenLink(release.uri.clone()));
    }
}

fn artist_hero(app: &mut App, ui: &mut egui::Ui, artist: &Artist, preview: Option<&Artist>) {
    let locale = app.locale;
    let mut byline = Vec::new();
    if let Some(followers) = &artist.followers {
        byline.push((
            ngettext(
                locale,
                // Translators: {count} is the number of people who follow an artist.
                "{count} follower",
                "{count} followers",
                u32::try_from(followers.total).unwrap_or(u32::MAX),
            )
            .replace("{count}", &util::format_count(followers.total)),
            None,
        ));
    }
    if !artist.genres.is_empty() {
        byline.push((
            artist
                .genres
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            None,
        ));
    }
    let images = hero_images(
        &artist.images,
        preview.map(|artist| artist.images.as_slice()),
        false,
        HeroArt::of(app),
    );
    hero(
        app,
        ui,
        Hero {
            images,
            liked: false,
            kind: gettext(locale, "Artist"),
            title: &artist.name,
            description: None,
            byline,
            byline_tooltip: None,
            avatar: None,
            round: true,
        },
    );
}

fn artist_actions(app: &mut App, ui: &mut egui::Ui, artist: &Artist) {
    let palette = app.palette;
    let locale = app.locale;
    let following = app.is_saved(&artist.uri).unwrap_or(false);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        if app.play_pending(&artist.uri) {
            theme::circle_spinner(
                ui,
                56.0,
                palette.accent,
                palette.on_accent,
                &gettext(locale, "Starting…"),
            );
        } else if theme::circle_button(
            ui,
            Icon::PlayFilled,
            56.0,
            palette.accent,
            palette.accent_hover,
            palette.on_accent,
            &gettext(locale, "Play"),
        )
        .clicked()
        {
            app.actions.push(Action::PlayContext {
                uri: artist.uri.clone(),
                offset_uri: None,
                offset_index: None,
            });
        }
        if theme::pill_button(
            ui,
            &palette,
            &if following {
                pgettext(locale, "artist", "Following")
            } else {
                pgettext(locale, "artist", "Follow")
            },
            false,
        )
        .clicked()
        {
            app.actions.push(Action::ToggleSaved(artist.uri.clone()));
        }
        let more = theme::icon_button(
            ui,
            Icon::Ellipsis,
            26.0,
            palette.secondary,
            palette.text,
            &gettext(locale, "More"),
        );
        egui::Popup::menu(&more)
            .frame(widgets::menu_frame(&palette))
            .show(|ui| widgets::context_menu_items(ui, app, &artist.uri, &artist.name, None));
    });
    ui.add_space(20.0);
}
