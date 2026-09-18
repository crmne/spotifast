//! The playback queue, as a page or as a side panel.

use std::sync::Arc;

use egui::{Align, Frame, Layout, Margin};

use crate::api::models::PlayableItem;
use crate::app::App;
use crate::i18n::{Locale, gettext};
use crate::model::{Action, Loadable, QueueTab, RowContext};
use crate::theme::{self, Icon};

use super::widgets::{self, TrackRow};

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    ui.add_space(8.0);
    // The queue refreshes on track changes, additions, and while visible.
    let offer_save = !app.queue_playlist_uris().is_empty();
    ui.horizontal(|ui| {
        theme::text(
            ui,
            gettext(app.locale, "Queue"),
            theme::bold(28.0),
            palette.text,
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if save_button(ui, &palette, offer_save, app.locale) {
                app.actions.push(Action::SaveQueueAsPlaylist);
            }
        });
    });
    ui.add_space(12.0);
    contents(app, ui, false);
}

pub fn side_panel(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let panel = egui::Panel::right("queue-panel")
        .resizable(true)
        .default_size(app.settings.queue_width)
        .size_range(theme::SIDE_PANEL_MIN_WIDTH..=560.0)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(palette.panel)
                .inner_margin(Margin::symmetric(12, 12)),
        );
    let response = panel.show(ui, |ui| {
        let window_controls = super::window_controls_reservation(
            ui.ctx(),
            app.show_queue_panel,
            app.show_lyrics_panel,
            ui.available_width(),
        );
        ui.add_space(window_controls.queue_top);
        // Measure buttons first and give the remaining width to the chips.
        // Without `shrink_left`, wrapped chips can overlap the close button.
        let tab = app.queue_tab;
        let offer_save = tab == QueueTab::Queue && !app.queue_playlist_uris().is_empty();
        let mut picked = None;
        let mut close = false;
        let mut save = false;
        egui::Sides::new().shrink_left().show(
            ui,
            |ui| {
                ui.add_space(4.0);
                picked = widgets::chips(
                    ui,
                    &palette,
                    &[
                        (QueueTab::Queue, &gettext(app.locale, "Queue")),
                        (QueueTab::Recents, &gettext(app.locale, "Recent")),
                    ],
                    tab,
                );
            },
            |ui| {
                close = theme::icon_button(
                    ui,
                    Icon::X,
                    18.0,
                    palette.secondary,
                    palette.text,
                    &gettext(app.locale, "Close"),
                )
                .clicked();
                save = save_button(ui, &palette, offer_save, app.locale);
            },
        );
        if let Some(tab) = picked {
            app.actions.push(Action::SetQueueTab(tab));
        }
        if close {
            app.actions.push(Action::ToggleQueuePanel);
        }
        if save {
            app.actions.push(Action::SaveQueueAsPlaylist);
        }
        ui.add_space(8.0);
        // Lazy load recents when tab becomes visible.
        if app.queue_tab == QueueTab::Recents
            && !app.recents.loading
            && !app.recents.complete
            && app.recents.items.is_empty()
            && app.recents.error.is_none()
        {
            app.actions.push(Action::LoadMoreRecents);
        }
        crate::ui::scroll::show(
            ui,
            egui::ScrollArea::vertical()
                .id_salt("queue-panel-scroll")
                .auto_shrink([false, false]),
            egui::Vec2b::new(false, true),
            |ui| match app.queue_tab {
                QueueTab::Queue => contents(app, ui, true),
                QueueTab::Recents => recents_contents(app, ui),
            },
        );
    });
    let width = response.response.rect.width();
    if (width - app.settings.queue_width).abs() > 1.0 {
        app.settings.queue_width = width;
        app.actions.push(Action::SettingsChanged);
    }
}

/// Saves the current and upcoming queue as a playlist.
fn save_button(
    ui: &mut egui::Ui,
    palette: &crate::theme::Palette,
    offer: bool,
    locale: Locale,
) -> bool {
    offer
        && theme::icon_button(
            ui,
            Icon::ListPlus,
            18.0,
            palette.secondary,
            palette.text,
            &gettext(locale, "Save as a playlist"),
        )
        .clicked()
}

/// Clears manual rows from the active local queue.
fn clear_button(app: &mut App, ui: &mut egui::Ui) {
    if !app.can_clear_queue() {
        return;
    }
    let palette = app.palette;
    if theme::icon_button(
        ui,
        Icon::Trash,
        18.0,
        palette.secondary,
        palette.text,
        &gettext(app.locale, "Clear queue"),
    )
    .clicked()
    {
        app.actions.push(Action::ClearQueue);
    }
}

fn contents(app: &mut App, ui: &mut egui::Ui, compact: bool) {
    let palette = app.palette;
    match &app.queue {
        Loadable::Loaded(_) => {}
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::loading_row(ui, &palette, app.locale);
            return;
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            widgets::error_row(ui, app, &error, Some(crate::model::Page::Queue));
            return;
        }
    };
    let now = app.now_playing();
    // Prefer the player's current track because the Web API can lag after a
    // skip.
    let current: Option<PlayableItem> = match &now {
        Some(now) => app
            .queue
            .get()
            .and_then(|queue| queue.currently_playing.clone())
            .filter(|item| item.uri() == now.uri)
            .or_else(|| app.now_playing_item()),
        None => app
            .queue
            .get()
            .and_then(|queue| queue.currently_playing.clone()),
    };
    // Where the songs come from, above the song itself. A radio has no
    // page, so its name is plain text.
    if let Some(from) = app.playing_from().filter(|_| current.is_some()) {
        ui.horizontal(|ui| {
            theme::text(
                ui,
                gettext(app.locale, "Playing from"),
                theme::regular(13.0),
                palette.secondary,
            );
            match from.page {
                Some(page) => {
                    if theme::link(ui, from.name, theme::semibold(13.0), palette.text).clicked() {
                        app.actions.push(Action::Open(page));
                    }
                }
                None => {
                    theme::text(ui, from.name, theme::semibold(13.0), palette.text);
                }
            }
        });
        ui.add_space(10.0);
    }
    if let Some(current) = current.as_ref() {
        theme::text(
            ui,
            gettext(app.locale, "Now playing"),
            theme::semibold(14.0),
            palette.text,
        );
        ui.add_space(4.0);
        let context = RowContext::Uris(Arc::from([current.uri().to_string()]));
        widgets::track_row(
            ui,
            app,
            TrackRow {
                index: 0,
                number: Some(1),
                item: current,
                context: &context,
                show_cover: true,
                show_album: !compact,
                added_at: None,
                added_by: None,
                show_added_by: false,
                compact,
                thin: false,
                shift: 0.0,
                picked: false,
                picked_songs: &[],
            },
        );
        ui.add_space(14.0);
    }
    if queue_is_empty(app) {
        widgets::empty_state(
            ui,
            &palette,
            Icon::ListVideo,
            &gettext(app.locale, "Nothing queued"),
            &gettext(app.locale, "Queued songs appear here."),
        );
        return;
    }
    let row_height = if compact {
        theme::COMPACT_ROW_HEIGHT
    } else {
        theme::ROW_HEIGHT
    };
    let queue_len = app.queue.get().map(|queue| queue.queue.len()).unwrap_or(0);
    // The user's own songs get their own section on top; the playing
    // context's rows follow under the usual heading. One numbering runs
    // through both, because that is the order things play.
    let queued_len = app.queued_rows_len().min(queue_len);
    if queued_len > 0 {
        // The trash sits with the songs it removes: only this section is
        // the user's to clear, the context below plays itself.
        ui.horizontal(|ui| {
            // Translators: Songs added manually, before the current playlist or album continues.
            theme::text(
                ui,
                gettext(app.locale, "Playing next"),
                theme::semibold(14.0),
                palette.text,
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                clear_button(app, ui);
            });
        });
        ui.add_space(4.0);
        let gap = ui.spacing().item_spacing.y;
        widgets::virtual_rows(ui, queued_len, row_height + gap, |ui, index| {
            let width = ui.available_width();
            queue_row(app, ui, index, compact);
            ui.allocate_space(egui::vec2(width, gap));
        });
        ui.add_space(14.0);
    }
    if queue_len > queued_len {
        // Translators: Upcoming songs from the current playlist or album, after manually queued songs.
        theme::text(
            ui,
            gettext(app.locale, "Next up"),
            theme::semibold(14.0),
            palette.text,
        );
        ui.add_space(4.0);
        widgets::virtual_rows(ui, queue_len - queued_len, row_height, |ui, index| {
            queue_row(app, ui, queued_len + index, compact);
        });
    }
}

fn queue_is_empty(app: &App) -> bool {
    app.queue.get().is_none_or(|queue| queue.queue.is_empty())
}

fn recents_contents(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    // Snapshot to avoid borrow issues while drawing. The rows are both
    // histories as one: what was played here, which Spotify is never told
    // about, and what Spotify knows of every other device.
    let items = app.recents_view.clone();
    let loading = app.recents.loading;
    let error = app.recents.error.clone();
    let complete = app.recents.complete;
    let loaded_once = app.recents.loaded_once;

    if items.is_empty() {
        if loading {
            widgets::loading_row(ui, &palette, app.locale);
            return;
        }
        if let Some(err) = error {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                theme::icon(ui, Icon::CircleAlert, 16.0, palette.danger);
                theme::text(ui, &err, theme::regular(13.0), palette.secondary);
                if theme::soft_button(
                    ui,
                    &palette,
                    Some(Icon::Refresh),
                    &gettext(app.locale, "Retry"),
                    false,
                )
                .clicked()
                {
                    app.actions.push(Action::ReloadRecents);
                }
            });
            return;
        }
        if loaded_once {
            widgets::empty_state(
                ui,
                &palette,
                Icon::Clock,
                &gettext(app.locale, "No recent plays"),
                &gettext(app.locale, "Played songs appear here."),
            );
        } else {
            widgets::loading_row(ui, &palette, app.locale);
        }
        return;
    }

    // Show error inline if we have items but also an error on next page.
    if let Some(err) = error {
        ui.horizontal(|ui| {
            theme::icon(ui, Icon::CircleAlert, 14.0, palette.danger);
            theme::text(ui, &err, theme::regular(12.0), palette.secondary);
            if theme::soft_button(
                ui,
                &palette,
                Some(Icon::Refresh),
                &gettext(app.locale, "Retry"),
                false,
            )
            .clicked()
            {
                app.actions.push(Action::LoadMoreRecents);
            }
        });
        ui.add_space(6.0);
    }

    let row_height = theme::COMPACT_ROW_HEIGHT;
    // Build PlayableItems on the fly; virtual_rows needs stable index.
    widgets::virtual_rows(ui, items.len(), row_height, |ui, index| {
        let entry = &items[index];
        // Need owned PlayableItem for track_row; clone track.
        let item = PlayableItem::Track(entry.track.clone());
        let context = RowContext::Uris(Arc::from([entry.track.uri.clone()]));
        widgets::track_row(
            ui,
            app,
            TrackRow {
                index,
                number: None,
                item: &item,
                context: &context,
                show_cover: true,
                show_album: false,
                added_at: entry.played_at.as_deref(),
                added_by: None,
                show_added_by: false,
                compact: true,
                thin: false,
                shift: 0.0,
                picked: false,
                picked_songs: &[],
            },
        );
    });

    // Footer: loading more or load more trigger
    if loading {
        ui.add_space(8.0);
        widgets::loading_row(ui, &palette, app.locale);
    } else if !complete {
        ui.add_space(8.0);
        // Auto-load when near end, plus manual button as fallback.
        let can_load = app.recents.can_load_more();
        // Check if scroll is near end (same heuristic as widgets::load_more_when_near_end)
        let clip = ui.clip_rect();
        let cursor = ui.cursor().top();
        if can_load && cursor - clip.bottom() < 900.0 {
            app.actions.push(Action::LoadMoreRecents);
        }
        if theme::soft_button(
            ui,
            &palette,
            Some(Icon::Refresh),
            &gettext(app.locale, "Load more"),
            false,
        )
        .clicked()
        {
            app.actions.push(Action::LoadMoreRecents);
        }
    }
}

/// One row of the queue, numbered and indexed by its place in the whole
/// queue, whichever section it sits in.
fn queue_row(app: &mut App, ui: &mut egui::Ui, index: usize, compact: bool) {
    let Some(item) = app
        .queue
        .get()
        .and_then(|queue| queue.queue.get(index))
        .cloned()
    else {
        return;
    };
    widgets::track_row(
        ui,
        app,
        TrackRow {
            index,
            number: Some(index + 1),
            item: &item,
            context: &RowContext::Queue,
            show_cover: true,
            show_album: !compact,
            added_at: None,
            added_by: None,
            show_added_by: false,
            compact,
            thin: false,
            shift: 0.0,
            picked: false,
            picked_songs: &[],
        },
    );
}
