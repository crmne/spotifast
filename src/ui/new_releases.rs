//! The New releases page: every song of the latest releases from the
//! artists the account follows, newest release first, played like a
//! playlist.

use std::sync::Arc;

use crate::api::models::PlayableItem;
use crate::app::App;
use crate::i18n::gettext;
use crate::model::{Action, Loadable, Page, RowContext};
use crate::theme::{self, Icon};

use super::collection::{Table, remember_table_items, songs_and_duration, table, table_items_hit};
use super::widgets;

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    if app.home.new_release_tracks.get().is_none() && !app.home.new_release_tracks.is_loading() {
        app.ensure_loaded(Page::NewReleases);
    }
    ui.add_space(12.0);
    theme::text(
        ui,
        gettext(locale, "New releases"),
        theme::bold(30.0),
        palette.text,
    );
    ui.add_space(4.0);
    theme::text(
        ui,
        gettext(
            locale,
            "The latest releases from artists you follow, newest first.",
        ),
        theme::regular(13.5),
        palette.secondary,
    );
    ui.add_space(18.0);

    // A shelf that could not load leaves the page nothing to read.
    if let Loadable::Failed(error) = &app.home.new_releases
        && app.home.new_release_tracks.get().is_none()
    {
        let error = error.clone();
        widgets::error_row(ui, app, &error, Some(Page::NewReleases));
        return;
    }
    if app
        .home
        .new_releases
        .get()
        .is_some_and(|albums| albums.is_empty())
    {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Music,
            &gettext(locale, "No new releases"),
            &gettext(
                locale,
                "New albums and singles from artists you follow will appear here.",
            ),
        );
        return;
    }
    let tracks = match &app.home.new_release_tracks {
        Loadable::Loaded(tracks) => tracks,
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::loading_row(ui, &palette, locale);
            return;
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            widgets::error_row(ui, app, &error, Some(Page::NewReleases));
            return;
        }
    };
    let count = u32::try_from(tracks.len()).unwrap_or(u32::MAX);
    let duration = tracks
        .iter()
        .map(|track| u64::from(track.duration_ms))
        .sum();
    let generation = app.home.new_release_tracks_generation;
    let names = app.user_names_revision;
    let items = match table_items_hit(app, &Page::NewReleases, generation, generation, names) {
        Some(items) => items,
        None => {
            let rows = app
                .home
                .new_release_tracks
                .get()
                .into_iter()
                .flatten()
                .map(|track| (PlayableItem::Track(track.clone()), None, None))
                .collect();
            remember_table_items(app, Page::NewReleases, generation, generation, names, rows)
        }
    };
    let uris: Arc<[String]> = items
        .iter()
        .map(|(item, _, _)| item.uri().to_string())
        .collect::<Vec<_>>()
        .into();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        if ui
            .add_enabled_ui(!uris.is_empty(), |ui| {
                theme::circle_button(
                    ui,
                    Icon::PlayFilled,
                    56.0,
                    palette.accent,
                    palette.accent_hover,
                    palette.on_accent,
                    &gettext(locale, "Play"),
                )
            })
            .inner
            .clicked()
        {
            app.actions.push(Action::PlayUris {
                uris: uris.to_vec(),
                index: 0,
            });
        }
        let shuffle = app.playing_context_shuffle();
        if theme::icon_button(
            ui,
            Icon::Shuffle,
            26.0,
            if shuffle {
                palette.accent
            } else {
                palette.secondary
            },
            if shuffle {
                palette.accent_hover
            } else {
                palette.text
            },
            &if shuffle {
                gettext(locale, "Shuffle off")
            } else {
                gettext(locale, "Shuffle")
            },
        )
        .clicked()
        {
            app.actions.push(Action::SetShuffle(!shuffle));
        }
        theme::text(
            ui,
            songs_and_duration(locale, count, duration),
            theme::regular(13.5),
            palette.secondary,
        );
    });
    ui.add_space(12.0);

    table(
        app,
        ui,
        Table {
            items: &items,
            row_offset: 0,
            pagination: None,
            context: RowContext::Uris(Arc::clone(&uris)),
            show_album: true,
            show_cover: true,
            show_added: false,
            show_added_by: false,
            show_plays: false,
            page: Page::NewReleases,
            loading: false,
            error: None,
            can_load_more: false,
            filter: "",
            items_revision: generation,
            plays_shown_rows: false,
        },
    );
}
