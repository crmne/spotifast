//! The playback queue, as a page or as a side panel.

use std::sync::Arc;

use egui::{Align, Frame, Layout, Margin, Rect, UiBuilder};

use crate::api::models::PlayableItem;
use crate::app::App;
use crate::i18n::{Locale, gettext};
use crate::model::{
    Action, DragTrack, Loadable, NOW_PLAYING_ROW, QUEUED_ROW, QueueRefusal, QueueTab, RowContext,
    UPCOMING_ROW,
};
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
    let fit = super::yielding_panel(
        ui.ctx(),
        "queue-panel",
        theme::SIDE_PANEL_MIN_WIDTH..=560.0,
        app.settings.queue_width,
        ui.available_width() - super::topbar::least_width(ui.ctx()),
    );
    let panel = egui::Panel::right("queue-panel")
        .resizable(true)
        .default_size(app.settings.queue_width)
        .size_range(fit.range.clone())
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
        crate::autoscroll::show(
            ui,
            egui::ScrollArea::vertical()
                .id_salt("queue-panel-scroll")
                .auto_shrink([false, false])
                // Keep rows and headings clear of the scroll bar.
                .content_margin(egui::Margin {
                    left: 0,
                    right: 12,
                    top: 0,
                    bottom: 0,
                }),
            egui::Vec2b::new(false, true),
            |ui| match app.queue_tab {
                QueueTab::Queue => contents(app, ui, true),
                QueueTab::Recents => recents_contents(app, ui),
            },
        );
    });
    let width = response.response.rect.width();
    // Adopt the drawn width only from the person's own drag. The panel
    // also draws clamped copies of widths chosen elsewhere (the
    // fullscreen column shares this setting below the panel minimum),
    // and those must never overwrite the remembered width.
    if (width - app.settings.queue_width).abs() > 1.0
        && super::panel_dragged(ui.ctx(), "queue-panel")
    {
        app.settings.queue_width = width;
        app.actions.push(Action::SettingsChanged);
    }
}

/// Fullscreen queue column width bounds, shared with the lyrics layout
/// so both agree without duplicating numbers.
pub(crate) const FULLSCREEN_COLUMN_MIN: f32 = 240.0;
pub(crate) const FULLSCREEN_COLUMN_MAX: f32 = 380.0;

/// The column width a drag gesture works from: the drawn width, not the
/// saved one. The saved side-panel width can sit outside this column's
/// range, and starting there would swallow the first movement back
/// inside it.
pub(crate) fn fullscreen_column_drag_width(saved: f32, delta_x: f32) -> f32 {
    let drawn = saved.clamp(FULLSCREEN_COLUMN_MIN, FULLSCREEN_COLUMN_MAX);
    (drawn - delta_x).clamp(FULLSCREEN_COLUMN_MIN, FULLSCREEN_COLUMN_MAX)
}

/// The queue as the right column of lyrics fullscreen: the same rows as
/// the side panel, without the resizable panel chrome. The caller provides
/// the rect; everything here draws inside it.
pub fn fullscreen_column(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    // Drag strip on the column's left edge: same persisted width the side
    // panel uses, so both follow each resize. Explicit id, stable across
    // frames for the whole drag gesture.
    let strip = egui::Rect::from_min_size(
        egui::pos2(ui.max_rect().left() - 4.0, ui.max_rect().top()),
        egui::vec2(8.0, ui.max_rect().height()),
    );
    let grip = ui.interact(
        strip,
        egui::Id::new("queue-column-resize"),
        egui::Sense::drag(),
    );
    if grip.hovered() || grip.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
    }
    // Gutter between the divider and the content, mirroring the side
    // panel's frame margin, so headings never touch the edge.
    let mut inner = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(
        egui::pos2(ui.max_rect().left() + 12.0, ui.max_rect().top()),
        ui.max_rect().max,
    )));
    let ui = &mut inner;
    // The handle stays invisible until hovered, then fades a slim edge in:
    // the theme text color, so it reads white on dark and black on bright.
    let show = ui.ctx().animate_value_with_time(
        egui::Id::new("queue-column-edge"),
        if grip.hovered() || grip.dragged() {
            1.0
        } else {
            0.0
        },
        0.15,
    );
    if show > 0.0 {
        ui.painter().vline(
            strip.center().x,
            strip.y_range(),
            egui::Stroke::new(1.0, palette.text.linear_multiply(show)),
        );
    }
    if grip.dragged() {
        let width = fullscreen_column_drag_width(app.settings.queue_width, grip.drag_delta().x);
        if (width - app.settings.queue_width).abs() > 0.5 {
            app.settings.queue_width = width;
            app.actions.push(Action::SettingsChanged);
        }
    }
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
    crate::autoscroll::show(
        ui,
        egui::ScrollArea::vertical()
            .id_salt("queue-fullscreen-scroll")
            .auto_shrink([false, false])
            // Keep rows and headings clear of the scroll bar.
            .content_margin(egui::Margin {
                left: 0,
                right: 12,
                top: 0,
                bottom: 0,
            }),
        egui::Vec2b::new(false, true),
        |ui| match app.queue_tab {
            QueueTab::Queue => contents(app, ui, true),
            QueueTab::Recents => recents_contents(app, ui),
        },
    );
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

/// Clears the songs under Next in queue, leaving the context's rows.
fn clear_button(app: &mut App, ui: &mut egui::Ui) {
    if !app.can_clear_queue() {
        return;
    }
    let palette = app.palette;
    let label = gettext(app.locale, "Clear queue");
    if theme::soft_button(ui, &palette, None, &label, false).clicked() {
        app.actions.push(Action::ClearQueue);
    }
}

/// What dropping the song being dragged on the open queue would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Landing {
    /// The dragged row of Next in queue moves to the slot under the pointer.
    Move,
    /// Songs from elsewhere go in at the slot under the pointer. Only this
    /// computer's player takes an insert at a chosen row.
    Insert,
    /// Songs from elsewhere join the end of Next in queue, as Add to queue
    /// does: a device playing elsewhere takes no insert.
    Append,
    /// A positional edit of the device's upcoming tracks.
    Edit,
    /// Nothing moves, for the reason given.
    Refused(QueueRefusal),
}

/// How to handle a drop in the manual section. A drop in Next up always
/// edits the playback order on the device.
fn landing_of(app: &App, track: &DragTrack) -> Landing {
    let insert = !matches!(
        track.from.as_ref().map(|(origin, _)| origin.as_str()),
        Some(NOW_PLAYING_ROW | QUEUED_ROW | UPCOMING_ROW)
    );
    match track.from.as_ref().map(|(origin, _)| origin.as_str()) {
        Some(NOW_PLAYING_ROW) => Landing::Refused(QueueRefusal::NowPlaying),
        Some(QUEUED_ROW | UPCOMING_ROW) if app.can_edit_play_order() => Landing::Edit,
        _ if app.queue_edit_in_flight() && !app.can_insert_in_play_order() => {
            Landing::Refused(QueueRefusal::Locked { insert })
        }
        Some(QUEUED_ROW) if app.queue_reorderable() => Landing::Move,
        Some(QUEUED_ROW) => Landing::Refused(QueueRefusal::Locked { insert }),
        Some(UPCOMING_ROW) => Landing::Refused(QueueRefusal::Locked { insert }),
        _ if app.can_insert_in_play_order() => Landing::Edit,
        _ if app.queue_locally_reorderable() => Landing::Insert,
        _ => Landing::Append,
    }
}

/// Where the dragged row of Next in queue sits now. Playback can advance
/// mid-drag and shift the rows, so the recorded index counts only while it
/// still holds the dragged song; otherwise the song is looked up, and one
/// that left the queue has nothing left to move.
fn dragged_queued_row(app: &App, track: &DragTrack) -> Option<usize> {
    let from = track.from.as_ref()?.1 as usize;
    let uri = track.items.first()?.uri();
    if app.queue_locally_reorderable() {
        if app.manual_queue.get(from).map(String::as_str) == Some(uri) {
            Some(from)
        } else {
            app.manual_queue.iter().position(|queued| queued == uri)
        }
    } else {
        // Elsewhere a move names the shown rows.
        let queue = app.queue.get()?;
        let shown = &queue.queue[..app.queued_rows_len().min(queue.queue.len())];
        if shown.get(from).map(PlayableItem::uri) == Some(uri) {
            Some(from)
        } else {
            shown.iter().position(|item| item.uri() == uri)
        }
    }
}

/// Draws the line a dragged song would land on, as the playlist table does.
fn drop_line(ui: &egui::Ui, color: egui::Color32, y: f32) {
    ui.painter().hline(
        ui.max_rect().x_range().shrink(8.0),
        y,
        egui::Stroke::new(2.0, color),
    );
}

#[derive(Clone, Copy)]
struct QueueSlot {
    index: usize,
    manual: bool,
}

/// Dispatches only the action advertised at the pointer.
fn drop_on_queue(app: &mut App, ui: &egui::Ui, landing: Landing, slot: Option<QueueSlot>) {
    let context = slot.is_some_and(|slot| !slot.manual);
    let landing = if context && !matches!(landing, Landing::Edit | Landing::Refused(_)) {
        Landing::Refused(QueueRefusal::Locked {
            insert: matches!(landing, Landing::Insert | Landing::Append),
        })
    } else {
        landing
    };
    // An append needs no row, so it also lands while the queue loads or
    // after its read failed.
    if slot.is_none() && !matches!(landing, Landing::Refused(_) | Landing::Append) {
        return;
    }
    let slot = slot.map_or(0, |slot| slot.index);
    let Some(track) = egui::DragAndDrop::take_payload::<DragTrack>(ui.ctx()) else {
        return;
    };
    match landing {
        Landing::Move => {
            // A row dropped back on its own edges moves nothing.
            if let Some(from) = dragged_queued_row(app, &track)
                && slot != from
                && slot != from + 1
            {
                app.actions.push(Action::MoveInQueue { from, to: slot });
            }
        }
        Landing::Edit => app.actions.push(Action::DropInPlaybackQueue {
            track: Box::new((*track).clone()),
            slot,
            manual: !context,
        }),
        Landing::Insert => app.actions.push(Action::InsertInQueue {
            items: track.items.clone(),
            position: slot,
        }),
        Landing::Append => app.actions.push(Action::QueueMany {
            songs: track
                .items
                .iter()
                .map(|item| (item.uri().to_string(), item.name().to_string()))
                .collect(),
        }),
        Landing::Refused(refusal) => app.actions.push(Action::RefuseQueueDrop(refusal)),
    }
}

/// Space below the now-playing row and below Next in queue, before the next
/// section's heading.
const SECTION_GAP: f32 = 14.0;

fn contents(app: &mut App, ui: &mut egui::Ui, compact: bool) {
    let dragging =
        egui::DragAndDrop::payload::<DragTrack>(ui.ctx()).map(|track| landing_of(app, &track));
    let landing = dragging.filter(|_| ui.rect_contains_pointer(ui.clip_rect()));
    let slot = rows(app, ui, compact, dragging, landing);
    if let Some(landing) = landing
        && ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary))
    {
        drop_on_queue(app, ui, landing, slot);
    }
}

/// Draws the queue's sections. `dragging` is what the song being dragged
/// would do on the queue, and `landing` the same while it is over the
/// panel; the answer is the slot in Next in queue the drop line marks.
fn rows(
    app: &mut App,
    ui: &mut egui::Ui,
    compact: bool,
    dragging: Option<Landing>,
    landing: Option<Landing>,
) -> Option<QueueSlot> {
    let palette = app.palette;
    let mut slot = None;
    match &app.queue {
        Loadable::Loaded(_) => {}
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::loading_row(ui, &palette, app.locale);
            return None;
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            widgets::error_row(ui, app, &error, Some(crate::model::Page::Queue));
            return None;
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
    if let Some(current) = current.as_ref() {
        theme::text(
            ui,
            gettext(app.locale, "Now playing"),
            theme::semibold(14.0),
            palette.text,
        );
        ui.add_space(4.0);
        let context = RowContext::Uris(Arc::from([current.uri().to_string()]));
        let dragging = egui::DragAndDrop::has_any_payload(ui.ctx());
        widgets::queue_track_row(
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
                show_plays: false,
                compact,
                thin: false,
                shift: 0.0,
                picked: false,
                picked_songs: &[],
                picked_positions: &[],
                position: None,
            },
        );
        // A drag that started on this row names it, so dropping the playing
        // song back on the queue does not queue it again.
        if !dragging && let Some(payload) = egui::DragAndDrop::payload::<DragTrack>(ui.ctx()) {
            let mut payload = (*payload).clone();
            payload.from = Some((NOW_PLAYING_ROW.to_string(), 0));
            egui::DragAndDrop::set_payload(ui.ctx(), payload);
        }
        ui.add_space(SECTION_GAP);
    }
    // Where Next in queue starts while it has no rows: halfway down the gap
    // under Now playing, or at the top without one.
    let start_line = ui.cursor().top()
        - if current.is_some() {
            SECTION_GAP / 2.0
        } else {
            0.0
        };
    if queue_is_empty(app) && dragging.is_none() {
        widgets::empty_state(
            ui,
            &palette,
            Icon::ListVideo,
            &gettext(app.locale, "Nothing queued"),
            &gettext(app.locale, "Queued songs appear here."),
        );
        return None;
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
    if dragging.is_some() {
        widgets::scroll_during_drag(ui);
    }
    if queued_len > 0 {
        // Clear sits with the songs it removes: only this section is the
        // user's to clear, the context below plays itself.
        ui.horizontal(|ui| {
            theme::text(
                ui,
                gettext(
                    app.locale,
                    // Translators: Songs added manually, before the current playlist or album continues.
                    "Next in queue",
                ),
                theme::semibold(14.0),
                palette.text,
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                clear_button(app, ui);
            });
        });
        ui.add_space(4.0);
        let gap = ui.spacing().item_spacing.y;
        // Calculate the nearest drop slot from fixed row height because
        // virtualized rows are not all available during drawing.
        let list_top = ui.cursor().top();
        // Include the manual heading and half the gap below the last manual row.
        let section_rect = egui::Rect::from_min_max(
            egui::pos2(ui.clip_rect().left(), start_line),
            egui::pos2(
                ui.clip_rect().right(),
                list_top + queued_len as f32 * (row_height + gap) + SECTION_GAP * 0.5,
            ),
        );
        slot = match landing {
            Some(Landing::Move | Landing::Insert | Landing::Edit) => ui
                .ctx()
                .pointer_latest_pos()
                .filter(|pos| ui.rect_contains_pointer(section_rect) && section_rect.contains(*pos))
                .map(|pos| {
                    (((pos.y - list_top) / (row_height + gap)).round() as usize).min(queued_len)
                }),
            // A receiver with append-only support takes the end of the manual section.
            Some(Landing::Append) if ui.rect_contains_pointer(section_rect) => Some(queued_len),
            _ => None,
        };
        widgets::virtual_rows(ui, queued_len, row_height + gap, |ui, index| {
            let width = ui.available_width();
            let shift = ui.ctx().animate_value_with_time(
                ui.id().with(("queue-move-shift", index)),
                match slot {
                    Some(slot) if index < slot => -4.0,
                    Some(_) => 4.0,
                    None => 0.0,
                },
                crate::motion::seconds(ui.ctx(), 0.12),
            );
            queue_row(app, ui, index, compact, shift);
            ui.allocate_space(egui::vec2(width, gap));
        });
        if let Some(slot) = slot {
            // The line between the parted rows, where the song will land.
            drop_line(
                ui,
                palette.accent,
                list_top + slot as f32 * (row_height + gap),
            );
        }
        ui.add_space(SECTION_GAP);
    }
    // The gap above Next up accepts the first manually queued song.
    if queued_len == 0
        && landing.is_some_and(|land| !matches!(land, Landing::Refused(_)))
        && (queue_len == 0
            || ui
                .ctx()
                .pointer_latest_pos()
                .is_some_and(|pos| pos.y < ui.cursor().top()))
    {
        slot = Some(0);
        drop_line(ui, palette.accent, start_line);
    }
    let mut target = slot.map(|index| QueueSlot {
        index,
        manual: true,
    });
    if queue_len > queued_len || (dragging.is_some() && queued_len > 0) {
        let heading_top = ui.cursor().top();
        // Named after where its songs come from, which opens on a click. A
        // radio has no page, so its name is plain text.
        match app.playing_from() {
            Some(from) => {
                ui.horizontal(|ui| {
                    theme::text(
                        ui,
                        gettext(
                            app.locale,
                            // Translators: Followed by the name of the playlist, album or radio the next songs come from.
                            "Next from:",
                        ),
                        theme::semibold(14.0),
                        palette.text,
                    );
                    let name = from.name;
                    match from.page {
                        Some(page) => {
                            if theme::link(ui, name, theme::semibold(14.0), palette.text).clicked()
                            {
                                app.actions.push(Action::Open(page));
                            }
                        }
                        None => {
                            theme::text(ui, name, theme::semibold(14.0), palette.text);
                        }
                    }
                });
            }
            None => {
                theme::text(
                    ui,
                    gettext(
                        app.locale,
                        // Translators: Upcoming songs after manually queued songs, when they come from no named playlist or album.
                        "Next up",
                    ),
                    theme::semibold(14.0),
                    palette.text,
                );
            }
        }
        ui.add_space(4.0);
        let unqueued = queue_len - queued_len;
        let top = ui.cursor().top();
        let next_slot = landing
            .and_then(|_| ui.ctx().pointer_latest_pos())
            .filter(|pos| pos.y >= heading_top && target.is_none())
            .map(|pos| (((pos.y - top) / row_height).round() as usize).min(unqueued));
        if let Some(index) = next_slot {
            target = Some(QueueSlot {
                index: queued_len + index,
                manual: false,
            });
        }
        widgets::virtual_rows(ui, queue_len - queued_len, row_height, |ui, index| {
            queue_row(app, ui, queued_len + index, compact, 0.0);
        });
        if let Some(index) = next_slot
            && landing == Some(Landing::Edit)
        {
            drop_line(ui, palette.accent, top + index as f32 * row_height);
        }
    }
    target
}

fn queue_is_empty(app: &App) -> bool {
    app.queue.get().is_none_or(|queue| queue.queue.is_empty())
}

fn recents_contents(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    // Share the rows to avoid borrow issues while drawing. The rows are both
    // histories as one: what was played here, which Spotify is never told
    // about, and what Spotify knows of every other device.
    let items = Arc::clone(&app.recents_view);
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
        widgets::queue_track_row(
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
                show_plays: false,
                compact: true,
                thin: false,
                shift: 0.0,
                picked: false,
                picked_songs: &[],
                picked_positions: &[],
                position: None,
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
/// queue, whichever section it sits in. `shift` parts rows around the slot
/// a dragged row would land in, in the "Next in queue" section only.
fn queue_row(app: &mut App, ui: &mut egui::Ui, index: usize, compact: bool, shift: f32) {
    let Some(item) = app
        .queue
        .get()
        .and_then(|queue| queue.queue.get(index))
        .cloned()
    else {
        return;
    };
    widgets::queue_track_row(
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
            show_plays: false,
            compact,
            thin: false,
            shift,
            picked: false,
            picked_songs: &[],
            picked_positions: &[],
            position: None,
        },
    );
}
