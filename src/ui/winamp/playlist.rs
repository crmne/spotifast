//! The playlist window, joined to the bottom of the main one.
//!
//! Uses `pledit.bmp` for the frame and `pledit.txt` for list colors. It shows
//! the current track and queue; double-clicking plays a row. Unsupported file
//! controls remain decorative. The panel shares the mini-player window so the
//! two parts stay attached.

use egui::{Color32, Sense};

use crate::api::models::PlayableItem;
use crate::app::{App, NowPlaying};
use crate::i18n::{gettext, pgettext};
use crate::model::{Action, Page, RowContext};
use crate::skin::layout::{self, Area};
use crate::skin::sprites;
use crate::util;

use super::View;

/// One line of the list.
struct Row {
    uri: String,
    label: String,
    duration_ms: u32,
    current: bool,
    /// Where the song sits in the queue, when it is queued rather than on.
    queued: Option<usize>,
    /// The pages the MISC menu can open for it.
    album_id: Option<String>,
    artist_id: Option<String>,
}

/// The whole panel, `height` skin pixels tall, drawn into a view whose
/// origin is the panel's top left.
pub(super) fn show(app: &mut App, view: &mut View, now: Option<&NowPlaying>, focused: bool) {
    if app.settings.playlist_shaded {
        shade(app, view, now, focused);
        return;
    }
    let height = app
        .settings
        .playlist_height
        .max(layout::PLAYLIST_MIN_HEIGHT);
    frame(view, height, focused);

    let title = view.interact(
        Area::new(0, 0, layout::WINDOW_WIDTH, layout::PLAYLIST_TITLE_HEIGHT),
        "playlist-title",
        Sense::click_and_drag(),
    );
    if title.drag_started() {
        view.ui
            .ctx()
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if view
        .lamp_button(
            layout::PLAYLIST_CLOSE,
            sprites::PLAYLIST_CLOSE_PRESSED,
            false,
            "playlist-close",
        )
        .clicked()
    {
        app.actions.push(Action::ToggleWinampPlaylist);
    }
    if view
        .lamp_button(
            layout::PLAYLIST_SHADE,
            sprites::PLAYLIST_SHADE_PRESSED,
            false,
            "playlist-shade",
        )
        .clicked()
    {
        app.actions.push(Action::ToggleWinampPlaylistShade);
    }

    let (rows, queue_uris) = rows(app, now);
    list(app, view, &rows, height);
    scrollbar(app, view, rows.len(), height);
    grip(app, view, height);
    times(app, view, now, &rows, height);
    mini_transport(app, view, now, height);
    menus(app, view, &rows, &queue_uris, height);
}

/// The little transport in the bottom right: the same as the big one,
/// painted into the skin, so these are only places to click.
fn mini_transport(app: &mut App, view: &mut View, now: Option<&NowPlaying>, height: u32) {
    let (x, dy) = layout::PLAYLIST_MINI_TRANSPORT;
    let y = height - layout::PLAYLIST_BOTTOM_HEIGHT + dy;
    let playing = now.is_some_and(|now| now.playing);
    for (index, name) in ["previous", "play", "pause", "stop", "next", "eject"]
        .into_iter()
        .enumerate()
    {
        let cell = Area::new(x + 10 * index as u32, y, 10, 10);
        let response = view.interact(cell, &format!("playlist-{name}"), Sense::click());
        if !response.clicked() {
            continue;
        }
        match name {
            "previous" => app.actions.push(Action::Previous),
            "play" => app.actions.push(if playing {
                Action::Seek(0)
            } else {
                Action::TogglePlay
            }),
            "pause" if now.is_some() => app.actions.push(Action::TogglePlay),
            "stop" if now.is_some() => {
                if playing {
                    app.actions.push(Action::TogglePlay);
                }
                app.actions.push(Action::Seek(0));
            }
            "next" => app.actions.push(Action::Next),
            "eject" => app.actions.push(Action::ToggleWinampWindow),
            _ => {}
        }
    }
}

/// The playlist rolled up: a bar with the song's name and time, the
/// button to roll it down again, and its X.
fn shade(app: &mut App, view: &mut View, now: Option<&NowPlaying>, focused: bool) {
    use sprites::*;
    let width = layout::WINDOW_WIDTH;
    let height = layout::PLAYLIST_SHADE_HEIGHT;
    view.sprite_at(PLAYLIST_SHADE_LEFT, 0, 0);
    let mut x = 25;
    while x < width - 50 {
        view.sprite_clipped(
            PLAYLIST_SHADE_TILE,
            x,
            0,
            Area::new(25, 0, width - 75, height),
        );
        x += 25;
    }
    let right = if focused {
        PLAYLIST_SHADE_RIGHT_ACTIVE
    } else {
        PLAYLIST_SHADE_RIGHT
    };
    view.sprite_at(right, width - 50, 0);
    let title = view.interact(
        Area::new(0, 0, width, height),
        "playlist-shade-title",
        Sense::click_and_drag(),
    );
    if title.drag_started() {
        view.ui
            .ctx()
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if let Some(now) = now {
        let time = util::format_duration_ms(now.position_ms);
        let time_width = 5 * time.len() as u32;
        let time_x = width - 30 - time_width;
        view.text(&time, Area::new(time_x, 4, time_width, 6));
        let name = label(1, &now.subtitle, &now.title);
        let name_area = Area::new(5, 4, time_x - 5 - 5, 6);
        if name.chars().all(crate::skin::font::covered) {
            view.text(&name, name_area);
        } else {
            super::pixel_line(app, view, &name, name_area);
        }
    }
    if view
        .lamp_button(
            layout::PLAYLIST_SHADE,
            sprites::PLAYLIST_UNSHADE_PRESSED,
            false,
            "playlist-unshade",
        )
        .clicked()
    {
        app.actions.push(Action::ToggleWinampPlaylistShade);
    }
    if view
        .lamp_button(
            layout::PLAYLIST_CLOSE,
            sprites::PLAYLIST_CLOSE_PRESSED,
            false,
            "playlist-shade-close",
        )
        .clicked()
    {
        app.actions.push(Action::ToggleWinampPlaylist);
    }
}

/// The frame: corners and tiles across the top, tiles down the sides
/// however tall the list is, and the two halves of the bottom.
fn frame(view: &mut View, height: u32, focused: bool) {
    use sprites::*;
    let (top_left, title, top_tile, top_right) = if focused {
        (
            PLAYLIST_TOP_LEFT_ACTIVE,
            PLAYLIST_TITLE_ACTIVE,
            PLAYLIST_TOP_TILE_ACTIVE,
            PLAYLIST_TOP_RIGHT_ACTIVE,
        )
    } else {
        (
            PLAYLIST_TOP_LEFT,
            PLAYLIST_TITLE,
            PLAYLIST_TOP_TILE,
            PLAYLIST_TOP_RIGHT,
        )
    };
    // The title sits centred; the tiles either side of it run out to the
    // corners, the last on each side cut to fit, as Winamp cut them.
    let tile = layout::PLAYLIST_TILE_WIDTH;
    let width = layout::WINDOW_WIDTH;
    let inner = width - 2 * tile - 100;
    let left = inner / 2;
    let right = inner - left;
    view.sprite_at(top_left, 0, 0);
    let left_run = Area::new(tile, 0, left, layout::PLAYLIST_TITLE_HEIGHT);
    let mut x = tile;
    while x < tile + left {
        view.sprite_clipped(top_tile, x, 0, left_run);
        x += tile;
    }
    view.sprite_at(title, tile + left, 0);
    let right_run = Area::new(tile + left + 100, 0, right, layout::PLAYLIST_TITLE_HEIGHT);
    let mut x = tile + left + 100;
    while x < width - tile {
        view.sprite_clipped(top_tile, x, 0, right_run);
        x += tile;
    }
    view.sprite_at(top_right, width - tile, 0);

    let middle = Area::new(
        0,
        layout::PLAYLIST_TITLE_HEIGHT,
        layout::WINDOW_WIDTH,
        height - layout::PLAYLIST_TITLE_HEIGHT - layout::PLAYLIST_BOTTOM_HEIGHT,
    );
    let mut y = middle.y;
    while y < middle.y + middle.height {
        view.sprite_clipped(PLAYLIST_LEFT_TILE, 0, y, middle);
        view.sprite_clipped(
            PLAYLIST_RIGHT_TILE,
            layout::WINDOW_WIDTH - layout::PLAYLIST_RIGHT_WIDTH,
            y,
            middle,
        );
        y += layout::PLAYLIST_TILE_HEIGHT;
    }
    let bottom = height - layout::PLAYLIST_BOTTOM_HEIGHT;
    view.sprite_at(PLAYLIST_BOTTOM_LEFT, 0, bottom);
    view.sprite_at(PLAYLIST_BOTTOM_RIGHT, 125, bottom);
}

/// The list's own area, between the tiles.
fn list_area(height: u32) -> Area {
    Area::new(
        layout::PLAYLIST_LEFT_WIDTH,
        layout::PLAYLIST_TITLE_HEIGHT,
        layout::WINDOW_WIDTH - layout::PLAYLIST_LEFT_WIDTH - layout::PLAYLIST_RIGHT_WIDTH,
        height - layout::PLAYLIST_TITLE_HEIGHT - layout::PLAYLIST_BOTTOM_HEIGHT,
    )
}

fn rows_visible(height: u32) -> usize {
    (list_area(height).height / layout::PLAYLIST_TRACK_HEIGHT) as usize
}

/// What is playing, then the queue, numbered the way Winamp numbered a
/// playlist. The queue's URIs come along for playing from a row.
fn rows(app: &App, now: Option<&NowPlaying>) -> (Vec<Row>, Vec<String>) {
    let queue = app.queue.get();
    let ids = |item: &PlayableItem| match item {
        PlayableItem::Track(track) => (
            track.album.as_ref().map(|album| album.id.clone()),
            track.artists.first().and_then(|artist| artist.id.clone()),
        ),
        _ => (None, None),
    };
    let mut rows = Vec::new();
    // The queue arrives over the Web API and lags a double-click by a
    // round trip; what the player says is on right now wins, and a queued
    // copy of that song is left out until the fetched queue catches up.
    let current = queue
        .and_then(|queue| queue.currently_playing.as_ref())
        .filter(|item| now.is_none_or(|now| now.uri == item.uri()));
    if let Some(item) = current {
        let (album_id, artist_id) = ids(item);
        rows.push(Row {
            uri: item.uri().to_string(),
            label: label(1, &item.subtitle(), item.name()),
            duration_ms: item.duration_ms(),
            current: true,
            queued: None,
            album_id,
            artist_id,
        });
    } else if let Some(now) = now {
        rows.push(Row {
            uri: now.uri.clone(),
            label: label(1, &now.subtitle, &now.title),
            duration_ms: now.duration_ms,
            current: true,
            queued: None,
            album_id: now.album_id.clone(),
            artist_id: now.artists.first().and_then(|artist| artist.id.clone()),
        });
    }
    let queued: &[PlayableItem] = queue.map(|queue| queue.queue.as_slice()).unwrap_or(&[]);
    let stale = current.is_none() && queue.is_some();
    let mut skipped_playing = false;
    for (index, item) in queued.iter().enumerate() {
        if stale && !skipped_playing && now.is_some_and(|now| now.uri == item.uri()) {
            skipped_playing = true;
            continue;
        }
        let (album_id, artist_id) = ids(item);
        rows.push(Row {
            uri: item.uri().to_string(),
            label: label(rows.len() + 1, &item.subtitle(), item.name()),
            duration_ms: item.duration_ms(),
            current: false,
            queued: Some(index),
            album_id,
            artist_id,
        });
    }
    let uris = queued.iter().map(|item| item.uri().to_string()).collect();
    (rows, uris)
}

fn label(number: usize, subtitle: &str, title: &str) -> String {
    if subtitle.is_empty() {
        format!("{number}. {title}")
    } else {
        format!("{number}. {subtitle} - {title}")
    }
}

/// The rows, in the skin's colours and a font the size Winamp's was.
fn list(app: &mut App, view: &mut View, rows: &[Row], height: u32) {
    let style = view.skin.playlist.clone();
    let rgb = |color: [u8; 3]| Color32::from_rgb(color[0], color[1], color[2]);
    let area = list_area(height);
    view.fill(
        area.x,
        area.y,
        area.width,
        area.height,
        rgb(style.normal_background),
    );

    let visible = rows_visible(height);
    let most = rows.len().saturating_sub(visible);
    // The wheel, over the list.
    // `contains_pointer`, not `hovered`: the rows drawn on top of the list
    // would take the hover and the wheel with it.
    let hovered = view
        .interact(area, "playlist-list", Sense::hover())
        .contains_pointer();
    if hovered {
        let row_points = layout::PLAYLIST_TRACK_HEIGHT as f32 * view.unit;
        let (lines, points, pages) = view.ui.ctx().input(|input| {
            input
                .events
                .iter()
                .fold((0.0, 0.0, 0.0), |sum, event| match event {
                    egui::Event::MouseWheel { unit, delta, .. } => match unit {
                        // One detent is one event however many lines the
                        // system says it is worth; a free-spinning wheel's
                        // fractions still add up.
                        egui::MouseWheelUnit::Line => {
                            let notch = if delta.y.abs() >= 1.0 {
                                delta.y.signum()
                            } else {
                                delta.y
                            };
                            (sum.0 + notch, sum.1, sum.2)
                        }
                        egui::MouseWheelUnit::Point => (sum.0, sum.1 + delta.y, sum.2),
                        egui::MouseWheelUnit::Page => (sum.0, sum.1, sum.2 + delta.y),
                    },
                    _ => sum,
                })
        });
        app.winamp.playlist_wheel += rows_for_wheel(lines, points, pages, row_points, visible);
        let rows_moved = app.winamp.playlist_wheel.trunc();
        if rows_moved != 0.0 {
            app.winamp.playlist_wheel -= rows_moved;
            let scroll = app.winamp.playlist_scroll as i64 + rows_moved as i64;
            app.winamp.playlist_scroll = scroll.clamp(0, most as i64) as usize;
        }
    }
    app.winamp.playlist_scroll = app.winamp.playlist_scroll.min(most);
    let scroll = app.winamp.playlist_scroll;
    let autoscroll_rect = view.rect(area);
    crate::autoscroll::playlist(
        view.ui,
        autoscroll_rect,
        scroll,
        most,
        layout::PLAYLIST_TRACK_HEIGHT as f32 * view.unit,
    );

    let ctx = view.ui.ctx().clone();
    let clip = view.rect(area).intersect(view.ui.clip_rect());
    let painter = view.ui.painter().with_clip_rect(clip);
    let pad = 3;
    for (offset, row) in rows.iter().skip(scroll).take(visible).enumerate() {
        let line = Area::new(
            area.x,
            area.y + offset as u32 * layout::PLAYLIST_TRACK_HEIGHT,
            area.width,
            layout::PLAYLIST_TRACK_HEIGHT,
        );
        let rect = view.rect(line);
        let response = view.ui.interact(
            rect,
            egui::Id::new(("playlist-row", scroll + offset)),
            Sense::click(),
        );
        crate::autoscroll::row(view.ui, &response);
        if response.clicked() {
            let adding = view.ui.ctx().input(|input| input.modifiers.command);
            // Selection is by row, not by song: the same song can sit in
            // the list more than once and one click means one row.
            let index = scroll + offset;
            let selection = &mut app.winamp.playlist_selection;
            if adding {
                if !selection.remove(&index) {
                    selection.insert(index);
                }
            } else {
                selection.clear();
                selection.insert(index);
            }
        }
        if response.double_clicked()
            && let Some(index) = row.queued
        {
            app.actions.push(Action::PlayFromRow {
                context: RowContext::Queue,
                uri: row.uri.clone(),
                index: index as u32,
            });
        }
        if app.winamp.playlist_selection.contains(&(scroll + offset)) {
            painter.rect_filled(rect, 0.0, rgb(style.selected_background));
        }
        let color = rgb(if row.current {
            style.current
        } else {
            style.normal
        });
        let text = &mut app.winamp.playlist_text;
        let duration = util::format_duration_ms(row.duration_ms);
        let duration_width = text.width(&duration).ceil() as u32;
        let title_room = line.width.saturating_sub(3 * pad + duration_width);
        let title = fit(text, &row.label, title_room as f32);
        let duration_line = text.line(&ctx, &duration);
        let duration_at = Area::new(
            line.x + line.width - pad - duration_line.width,
            line.y + (line.height.saturating_sub(duration_line.height)) / 2,
            duration_line.width,
            duration_line.height,
        );
        painter.image(
            duration_line.texture.id(),
            view.rect(duration_at),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            color,
        );
        let title_line = text.line(&ctx, &title);
        let title_at = Area::new(
            line.x + pad,
            line.y + (line.height.saturating_sub(title_line.height)) / 2,
            title_line.width.min(title_room),
            title_line.height,
        );
        let uv_right = title_at.width as f32 / title_line.width.max(1) as f32;
        painter.image(
            title_line.texture.id(),
            view.rect(title_at),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(uv_right, 1.0)),
            color,
        );
    }
}

/// How many rows a frame's wheel moves the list, positive downwards. A
/// notch scrolls three rows, the Windows default Winamp's list followed;
/// a trackpad's points scroll a row per row's height; a page is the rows
/// in view. egui's deltas are positive when the content moves down, which
/// is scrolling up.
fn rows_for_wheel(lines: f32, points: f32, pages: f32, row_points: f32, visible: usize) -> f32 {
    -(lines * 3.0 + points / row_points + pages * visible as f32)
}

/// Text cut to a width, with an ellipsis when it had to be.
fn fit(text: &mut super::PixelText, label: &str, width: f32) -> String {
    if text.width(label) <= width {
        return label.to_string();
    }
    let chars: Vec<char> = label.chars().collect();
    let mut keep = chars.len();
    while keep > 0 {
        keep -= 1;
        let candidate: String = chars[..keep].iter().collect::<String>() + "\u{2026}";
        if text.width(&candidate) <= width {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

/// The handle in the right-hand tiles, dragged to scroll.
fn scrollbar(app: &mut App, view: &mut View, rows: usize, height: u32) {
    let visible = rows_visible(height);
    let most = rows.saturating_sub(visible);
    let top = layout::PLAYLIST_TITLE_HEIGHT;
    let travel = height
        .saturating_sub(layout::PLAYLIST_TITLE_HEIGHT + layout::PLAYLIST_BOTTOM_HEIGHT)
        .saturating_sub(layout::PLAYLIST_SCROLL_HANDLE_HEIGHT);
    let fraction = if most == 0 {
        0.0
    } else {
        app.winamp.playlist_scroll as f32 / most as f32
    };
    let y = top + (fraction * travel as f32).round() as u32;
    let handle = Area::new(
        layout::PLAYLIST_SCROLL_X,
        y,
        8,
        layout::PLAYLIST_SCROLL_HANDLE_HEIGHT,
    );
    let response = view.interact(handle, "playlist-scroll", Sense::click_and_drag());
    if response.dragged()
        && most > 0
        && let Some(pos) = response.interact_pointer_pos()
    {
        let pointer = (pos.y - view.origin.y) / view.unit
            - top as f32
            - layout::PLAYLIST_SCROLL_HANDLE_HEIGHT as f32 / 2.0;
        let fraction = (pointer / travel as f32).clamp(0.0, 1.0);
        app.winamp.playlist_scroll = (fraction * most as f32).round() as usize;
    }
    let sprite = if response.dragged() || response.is_pointer_button_down_on() {
        sprites::PLAYLIST_SCROLL_HANDLE_PRESSED
    } else {
        sprites::PLAYLIST_SCROLL_HANDLE
    };
    view.sprite(sprite, handle);
}

/// The corner that stretches the list, a tile row at a time.
fn grip(app: &mut App, view: &mut View, height: u32) {
    let corner = Area::new(
        layout::WINDOW_WIDTH - layout::PLAYLIST_GRIP,
        height - layout::PLAYLIST_GRIP,
        layout::PLAYLIST_GRIP,
        layout::PLAYLIST_GRIP,
    );
    let response = view
        .interact(corner, "playlist-grip", Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if response.dragged() {
        app.winamp.playlist_resize += response.drag_delta().y / view.unit;
        let step = layout::PLAYLIST_RESIZE_STEP as f32;
        let steps = (app.winamp.playlist_resize / step).trunc();
        if steps != 0.0 {
            app.winamp.playlist_resize -= steps * step;
            let wanted = (height as i64 + steps as i64 * step as i64).clamp(
                layout::PLAYLIST_MIN_HEIGHT as i64,
                layout::PLAYLIST_MAX_HEIGHT as i64,
            ) as u32;
            if wanted != height {
                app.actions.push(Action::SetPlaylistHeight(wanted));
            }
        }
    }
    if response.drag_stopped() {
        app.winamp.playlist_resize = 0.0;
    }
}

/// The selected songs' time over the queue's total, as Winamp's list
/// showed them, and the song's own time where Winamp kept it, in the
/// skin's small font.
fn times(app: &App, view: &mut View, now: Option<&NowPlaying>, rows: &[Row], height: u32) {
    let bottom = height - layout::PLAYLIST_BOTTOM_HEIGHT;
    let total: u32 = rows.iter().map(|row| row.duration_ms).sum();
    let selected: u32 = rows
        .iter()
        .enumerate()
        .filter(|(index, _)| app.winamp.playlist_selection.contains(index))
        .map(|(_, row)| row.duration_ms)
        .sum();
    let running = format!(
        "{}/{}",
        util::format_duration_ms(selected),
        util::format_duration_ms(total)
    );
    let (x, dy) = layout::PLAYLIST_RUNNING_TIME;
    view.text(
        &running,
        Area::new(x, bottom + dy, 5 * running.len() as u32, 6),
    );
    if let Some(now) = now {
        let (x, dy) = layout::PLAYLIST_TRACK_TIME;
        let elapsed = util::format_duration_ms(now.position_ms);
        view.text(
            &elapsed,
            Area::new(x, bottom + dy, 5 * elapsed.len() as u32, 6),
        );
    }
}

/// Bottom menus adapted to supported Spotify actions.
fn menus(app: &mut App, view: &mut View, rows: &[Row], queue_uris: &[String], height: u32) {
    let bottom = height - layout::PLAYLIST_BOTTOM_HEIGHT;
    let unit = view.unit;
    for (name, x) in layout::PLAYLIST_MENUS {
        let area = Area::new(x, bottom + 8, 22, 18);
        let button = view.interact(area, &format!("playlist-menu-{name}"), Sense::click());
        super::menu(
            egui::Popup::menu(&button),
            view.skin,
            unit,
            |ui| match name {
                "add" => add_menu(app, ui),
                "rem" => rem_menu(app, ui, rows),
                "sel" => sel_menu(app, ui, rows),
                "misc" => misc_menu(app, ui, rows),
                _ => list_menu(app, ui, rows, queue_uris),
            },
        );
    }
}

/// Where a song comes from here: the big window's search or Liked Songs.
fn add_menu(app: &mut App, ui: &mut egui::Ui) {
    let locale = app.locale;
    if ui
        .button(gettext(locale, "Search Spotify").as_ref())
        .clicked()
    {
        app.actions.push(Action::FocusSearch);
        app.actions.push(Action::ToggleWinampWindow);
    }
    if ui.button(gettext(locale, "Liked Songs").as_ref()).clicked() {
        app.actions.push(Action::Open(Page::LikedSongs));
        app.actions.push(Action::ToggleWinampWindow);
    }
}

fn rem_menu(app: &mut App, ui: &mut egui::Ui, rows: &[Row]) {
    let locale = app.locale;
    // Selection is by list row; only the songs the user queued can go.
    let removable: Vec<(usize, String)> = rows
        .iter()
        .enumerate()
        .filter(|(row, _)| app.winamp.playlist_selection.contains(row))
        .filter_map(|(_, row)| row.queued.map(|index| (index, row.uri.clone())))
        .filter(|(index, _)| app.can_remove_from_queue(*index))
        .collect();
    let mut remove = ui.add_enabled(
        !removable.is_empty(),
        egui::Button::new(gettext(locale, "Remove selected").as_ref()),
    );
    if !app.queue_locally_reorderable() {
        remove = remove.on_disabled_hover_text(
            gettext(locale, "You can only change this computer's queue.").as_ref(),
        );
    }
    if remove.clicked() {
        // The rows below move up, so the selection's indexes would name
        // other songs.
        app.winamp.playlist_selection.clear();
        app.actions
            .push(Action::RemoveFromQueue { rows: removable });
    }
    if ui
        .add_enabled(
            app.can_clear_queue(),
            egui::Button::new(gettext(locale, "Remove all").as_ref()),
        )
        .on_disabled_hover_text(
            gettext(locale, "You can only clear this computer's queue.").as_ref(),
        )
        .clicked()
    {
        app.actions.push(Action::ClearQueue);
    }
}

fn sel_menu(app: &mut App, ui: &mut egui::Ui, rows: &[Row]) {
    let locale = app.locale;
    // Translators: Selects every song in the list.
    let all = pgettext(locale, "selection", "All");
    // Translators: Clears the selection of songs in the list.
    let none = pgettext(locale, "selection", "None");
    // Translators: Selects the songs that are not selected, and clears the rest.
    let invert = pgettext(locale, "selection", "Invert");
    let selection = &mut app.winamp.playlist_selection;
    if ui.button(all.as_ref()).clicked() {
        selection.extend(0..rows.len());
    }
    if ui.button(none.as_ref()).clicked() {
        selection.clear();
    }
    if ui.button(invert.as_ref()).clicked() {
        for index in 0..rows.len() {
            if !selection.remove(&index) {
                selection.insert(index);
            }
        }
    }
}

/// The pages of the selected song, or of the one playing.
fn misc_menu(app: &mut App, ui: &mut egui::Ui, rows: &[Row]) {
    let locale = app.locale;
    let chosen = rows
        .iter()
        .enumerate()
        .find(|(index, _)| app.winamp.playlist_selection.contains(index))
        .map(|(_, row)| row)
        .or_else(|| rows.iter().find(|row| row.current));
    let Some(row) = chosen else {
        ui.add_enabled(
            false,
            egui::Button::new(gettext(locale, "Song info").as_ref()),
        );
        return;
    };
    if let Some(album) = &row.album_id
        && ui.button(gettext(locale, "Song info").as_ref()).clicked()
    {
        app.actions.push(Action::Open(Page::Album(album.clone())));
        app.actions.push(Action::ToggleWinampWindow);
    }
    if let Some(artist) = &row.artist_id
        && ui.button(gettext(locale, "Artist").as_ref()).clicked()
    {
        app.actions.push(Action::Open(Page::Artist(artist.clone())));
        app.actions.push(Action::ToggleWinampWindow);
    }
}

/// Your playlists to play, and the queue kept as a new one.
fn list_menu(app: &mut App, ui: &mut egui::Ui, rows: &[Row], queue_uris: &[String]) {
    let playlists: Vec<(String, String)> = app
        .library
        .playlists
        .get()
        .map(|playlists| {
            playlists
                .iter()
                .map(|playlist| (playlist.name.clone(), playlist.uri.clone()))
                .collect()
        })
        .unwrap_or_default();
    let locale = app.locale;
    ui.menu_button(gettext(locale, "Load list").as_ref(), |ui| {
        crate::autoscroll::show(
            ui,
            egui::ScrollArea::vertical().max_height(super::menu_limit(ui)),
            egui::Vec2b::new(false, true),
            |ui| {
                if playlists.is_empty() {
                    ui.add_enabled(
                        false,
                        egui::Button::new(gettext(locale, "No playlists yet").as_ref()),
                    );
                }
                for (name, uri) in &playlists {
                    if ui.button(name).clicked() {
                        app.actions.push(Action::PlayContext {
                            uri: uri.clone(),
                            offset_uri: None,
                            offset_index: None,
                        });
                        ui.close();
                    }
                }
            },
        );
    });
    let saveable = !queue_uris.is_empty() || rows.iter().any(|row| row.current);
    if ui
        .add_enabled(
            saveable,
            egui::Button::new(gettext(locale, "Save list").as_ref()),
        )
        .on_hover_text(gettext(locale, "Save the queue as a new playlist").as_ref())
        .clicked()
    {
        let mut uris: Vec<String> = rows
            .iter()
            .filter(|row| row.current)
            .map(|row| row.uri.clone())
            .collect();
        uris.extend(queue_uris.iter().cloned());
        app.actions.push(Action::CreatePlaylist {
            name: app.queue_playlist_name(),
            public: false,
            add_uris: uris,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_numbered_the_way_winamp_did() {
        assert_eq!(label(1, "Bonobo", "Rosewood"), "1. Bonobo - Rosewood");
        assert_eq!(label(12, "", "Episode 12"), "12. Episode 12");
    }

    #[test]
    fn a_notch_scrolls_three_rows_as_windows_did() {
        assert_eq!(rows_for_wheel(-1.0, 0.0, 0.0, 20.0, 8), 3.0);
        assert_eq!(rows_for_wheel(2.0, 0.0, 0.0, 20.0, 8), -6.0);
        assert_eq!(rows_for_wheel(0.0, -40.0, 0.0, 20.0, 8), 2.0);
        assert_eq!(rows_for_wheel(0.0, 0.0, -1.0, 20.0, 8), 8.0);
    }

    #[test]
    fn the_list_holds_whole_rows_only() {
        assert_eq!(rows_visible(116), 4);
        assert_eq!(rows_visible(174), 8);
        assert_eq!(list_area(174), Area::new(12, 20, 243, 116));
    }

    /// REM's Remove selected names queue rows, not list rows: the list
    /// opens with the playing song, so a selected line sits one below its
    /// place in the queue. Only the songs the user queued go (#721).
    #[test]
    fn remove_selected_takes_the_selected_queued_songs() {
        use crate::app::AppOptions;
        use crate::paths::AppDirs;
        use crate::settings::Settings;
        use egui::accesskit::{Action as Access, ActionRequest, Role, TreeId};

        let root =
            std::env::temp_dir().join(format!("spotifast-winamp-remove-{}", std::process::id()));
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let waker = crate::backend::Waker::default();
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
        crate::demo::populate(&mut app);
        let now = app.now_playing();
        app.local_ready = true;
        app.local.track = Some(crate::player::LocalTrack {
            uri: now.as_ref().unwrap().uri.clone(),
            ..Default::default()
        });
        app.local.playback = crate::player::Playback::Paused;
        let queued: Vec<String> = app.queue.get().unwrap().queue[..2]
            .iter()
            .map(|item| item.uri().to_string())
            .collect();
        app.manual_queue = queued.clone();
        let (rows, _) = rows(&app, now.as_ref());
        assert!(rows[0].current);
        // The second queued song, and the first song of Next up after it.
        app.winamp.playlist_selection.extend([2, 3]);

        let frame = |app: &mut App, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| rem_menu(app, ui, &rows),
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        };
        let tree = frame(&mut app, vec![]);
        let remove = tree
            .nodes
            .iter()
            .find(|(_, node)| {
                node.label() == Some("Remove selected") && node.role() == Role::Button
            })
            .unwrap()
            .0;
        frame(
            &mut app,
            vec![egui::Event::AccessKitActionRequest(ActionRequest {
                target_tree: TreeId::ROOT,
                target_node: remove,
                action: Access::Click,
                data: None,
            })],
        );
        assert!(
            matches!(
                app.actions.as_slice(),
                [Action::RemoveFromQueue { rows }] if *rows == [(1, queued[1].clone())]
            ),
            "{:?}",
            app.actions
        );
        assert!(app.winamp.playlist_selection.is_empty());
        app.backend.shutdown();
    }
}
