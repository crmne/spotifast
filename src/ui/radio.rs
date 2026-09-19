//! A station is browsed independently of the currently playing queue.

use egui::load::BytesLoader as _;
use std::sync::Arc;

use crate::api::models::{Track, pick_image};
use crate::app::App;
use crate::model::{Action, Loadable, Page, RowContext, TableItem};
use crate::theme::{self, Icon};

use super::{
    collection::{self, Hero, Table},
    widgets,
};

pub fn show(app: &mut App, ui: &mut egui::Ui, id: &str) {
    let seed = app.radio_seed(id).cloned();
    let Some(page) = app.radio_pages.remove(id) else {
        app.ensure_loaded(Page::Radio(id.to_owned()));
        return;
    };
    let palette = app.palette;
    let title = seed
        .as_ref()
        .map(|track| format!("{} Radio", track.name))
        .unwrap_or_else(|| "Song radio".into());
    let image = seed
        .as_ref()
        .and_then(|track| seed_image(ui.ctx(), app.backend.art(), track));
    collection::hero(
        app,
        ui,
        Hero {
            image,
            liked: false,
            kind: "Song radio",
            title: &title,
            description: Some(
                "Songs picked by Spotify. Browse now, play when you're ready.".into(),
            ),
            byline: seed
                .as_ref()
                .map(|track| vec![(track.artist_names(), None)])
                .unwrap_or_default(),
            round: false,
        },
    );
    match &page.station {
        Loadable::Loaded(station) => {
            let items: Vec<TableItem> = station
                .tracks
                .iter()
                .cloned()
                .map(|track| (crate::api::models::PlayableItem::Track(track), None, None))
                .collect();
            let uris: Arc<[String]> = station
                .tracks
                .iter()
                .map(|track| track.uri.clone())
                .collect();
            // Play the previewed list, so Spotify cannot resolve a different
            // station between browsing these rows and selecting one.
            let sorted = app.table_sorts.get(&Page::Radio(id.to_owned())).copied();
            let view = collection::prepare_table_view(
                ui,
                app,
                &Page::Radio(id.to_owned()),
                &items,
                "",
                sorted,
                page.generation,
            );
            let play_uris = view.view_uris.clone().unwrap_or_else(|| Arc::clone(&uris));
            let playing_here = app.playing_context_uri().as_deref()
                == Some(format!("spotify:station:track:{id}").as_str())
                && app.believed_playing();
            ui.horizontal(|ui| {
                if theme::circle_button(
                    ui,
                    if playing_here {
                        Icon::PauseFilled
                    } else {
                        Icon::PlayFilled
                    },
                    56.0,
                    palette.accent,
                    palette.accent_hover,
                    palette.on_accent,
                    if playing_here { "Pause" } else { "Play radio" },
                )
                .clicked()
                {
                    if playing_here {
                        app.actions.push(Action::TogglePlay);
                    } else {
                        app.actions.push(Action::PlayFromRow {
                            context: RowContext::View {
                                uris: play_uris,
                                context_uri: format!("spotify:station:track:{id}"),
                            },
                            uri: String::new(),
                            index: 0,
                        });
                    }
                }
                ui.add_space(12.0);
                if theme::soft_button(ui, &palette, Some(Icon::Refresh), "Refresh radio", false)
                    .clicked()
                {
                    app.actions.push(Action::Reload(Page::Radio(id.to_owned())));
                }
            });
            ui.add_space(16.0);
            collection::table(
                app,
                ui,
                Table {
                    items: &items,
                    row_offset: 0,
                    pagination: None,
                    context: RowContext::View {
                        uris,
                        context_uri: format!("spotify:station:track:{id}"),
                    },
                    show_album: true,
                    show_cover: true,
                    show_added: false,
                    show_added_by: false,
                    page: Page::Radio(id.to_owned()),
                    loading: false,
                    error: None,
                    can_load_more: false,
                    filter: "",
                    items_revision: page.generation,
                },
            );
        }
        Loadable::Failed(error) => {
            widgets::error_row(ui, app, error, Some(Page::Radio(id.to_owned())))
        }
        _ => widgets::loading_row(ui, &palette, app.locale),
    }
    app.radio_pages.insert(id.to_owned(), page);
}

fn seed_image<'a>(
    ctx: &egui::Context,
    art: &crate::images::ArtLoader,
    track: &'a Track,
) -> Option<&'a str> {
    let images = &track.album.as_ref()?.images;
    let preferred = pick_image(images, 300)?;
    let ready = |url: &str| {
        matches!(
            egui::Image::new(url).load_for_size(ctx, egui::Vec2::splat(300.0)),
            Ok(egui::load::TexturePoll::Ready { .. })
        )
    };
    if ready(preferred) {
        return Some(preferred);
    }
    // These are the app's two byte sources: downloaded and embedded artwork.
    // Check their caches without fetching unused cover sizes.
    images
        .iter()
        .map(|image| image.url.as_str())
        .find(|url| {
            *url != preferred
                && (art.is_ready(url) || ctx.loaders().include.load(ctx, url).is_ok())
                && ready(url)
        })
        .or(Some(preferred))
}
