//! An artist's discography page: every release with its songs, newest or
//! oldest first, opened by Show all on the artist page. A song played here
//! plays the page from that song to its end.

use egui::{Align, Id, Layout, Rect, Sense, UiBuilder, vec2};

use crate::api::models::{Album, PlayableItem, pick_image};
use crate::app::App;
use crate::i18n::{gettext, pgettext};
use crate::model::{Action, DiscographyFilter, DiscographyPage, Page, ReleaseTracks, RowContext};
use crate::theme::{self, Icon};

use super::collection::placeholder_row;
use super::widgets::{self, TrackRow};

/// A release's artwork in its heading.
const COVER: f32 = 128.0;
/// Room above a release's artwork.
const HEADER_TOP: f32 = 12.0;
const HEADER_HEIGHT: f32 = HEADER_TOP + COVER + 16.0;
/// The `#`, title and duration headings over a release's songs.
const COLUMNS_HEIGHT: f32 = 34.0;
/// Space after a release's last song.
const RELEASE_GAP: f32 = 28.0;
/// The release play button.
const PLAY_DIAMETER: f32 = 36.0;

/// One line of the page's list. Releases are drawn line by line so only the
/// ones on screen are laid out, however long the discography.
#[derive(Clone, Copy)]
enum Line {
    Heading(usize),
    Columns(usize),
    Song(usize, usize),
    /// Room held for a release's songs until they arrive. The first line
    /// says why they are not there yet.
    Reserved(usize, usize),
    Gap(usize),
}

impl Line {
    fn release(self) -> usize {
        match self {
            Line::Heading(index)
            | Line::Columns(index)
            | Line::Song(index, _)
            | Line::Reserved(index, _)
            | Line::Gap(index) => index,
        }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui, artist_id: &str) {
    if !app.discography_pages.contains_key(artist_id) {
        app.ensure_loaded(Page::Discography(artist_id.to_string()));
    }
    let title = app
        .discography_title(artist_id)
        .unwrap_or_else(|| gettext(app.locale, "Discography").into_owned());
    let Some(page) = app.discography_pages.remove(artist_id) else {
        widgets::skeleton_rows(ui, &app.palette, app.locale, 8);
        return;
    };
    draw(app, ui, artist_id, &page, &title);
    app.discography_pages.insert(artist_id.to_string(), page);
}

fn draw(app: &mut App, ui: &mut egui::Ui, artist_id: &str, page: &DiscographyPage, title: &str) {
    let palette = app.palette;
    let locale = app.locale;
    ui.add_space(8.0);
    heading(app, ui, artist_id, page, title);
    ui.add_space(12.0);
    let page_retry = || Action::RetryDiscography {
        artist_id: artist_id.to_string(),
        album_id: None,
    };
    // Nothing is asked for offline, so a list still to come says why it
    // is not coming instead of showing a load that is not happening.
    if !app.network_reachable && !page.list().is_some_and(|list| list.is_complete()) {
        widgets::empty_state(
            ui,
            &palette,
            Icon::WifiOff,
            &gettext(locale, "You're offline"),
            &gettext(
                locale,
                "Spotifast will reconnect automatically when your connection returns.",
            ),
        );
        return;
    }
    // The releases to come wait in skeleton rows below the known header,
    // like an album's songs, not in a spinner row.
    let Some(list) = page.list() else {
        widgets::skeleton_rows(ui, &palette, locale, 8);
        return;
    };
    if let Some(error) = &list.error {
        retry_row(ui, app, error, page_retry());
        return;
    }
    if !list.is_complete() {
        widgets::skeleton_rows(ui, &palette, locale, 8);
        return;
    }
    let releases = page.ordered();
    if releases.is_empty() {
        theme::subtle(ui, &palette, &gettext(locale, "Nothing in this category."));
        return;
    }
    let song_height = if app.settings.tracklist_compact {
        theme::THIN_ROW_HEIGHT
    } else {
        theme::ROW_HEIGHT
    };
    let mut lines = Vec::new();
    for (index, album) in releases.iter().enumerate() {
        lines.push((Line::Heading(index), HEADER_HEIGHT));
        match page.tracks.get(&album.id).and_then(ReleaseTracks::songs) {
            Some(songs) => {
                if !songs.is_empty() {
                    lines.push((Line::Columns(index), COLUMNS_HEIGHT));
                }
                lines.extend((0..songs.len()).map(|row| (Line::Song(index, row), song_height)));
            }
            // Hold the room the songs will take, whether they are on their
            // way, failed or stopped, so nothing above the view moves what
            // the user is reading.
            None => {
                lines.push((Line::Columns(index), COLUMNS_HEIGHT));
                let count = album.total_tracks.unwrap_or(1).max(1) as usize;
                lines.extend((0..count).map(|row| (Line::Reserved(index, row), song_height)));
            }
        }
        lines.push((Line::Gap(index), RELEASE_GAP));
    }
    let thin = app.settings.tracklist_compact;
    // Lines keep their ids by artist, release and row, never by position, so
    // focus and open menus stay with their song as the page scrolls and as
    // song lists above them arrive.
    let base = Id::new(("discography", artist_id));
    let key = |line: Line| {
        let (kind, row) = match line {
            Line::Heading(_) => ("heading", 0),
            Line::Columns(_) => ("columns", 0),
            Line::Song(_, row) => ("song", row),
            Line::Reserved(_, row) => ("reserved", row),
            Line::Gap(_) => ("gap", 0),
        };
        base.with((kind, &releases[line.release()].uri, row))
    };
    // Headings and songs hold the page's controls, as does the first line
    // of a release whose songs failed or stopped; the rest are spacing.
    let focusable = |line: Line| match line {
        Line::Heading(_) | Line::Song(..) => true,
        Line::Reserved(index, 0) => {
            page.halted.is_some()
                || matches!(
                    page.tracks.get(&releases[index].id),
                    Some(ReleaseTracks::Failed(_))
                )
        }
        Line::Columns(_) | Line::Reserved(..) | Line::Gap(_) => false,
    };
    let drawn = measured_lines(ui, &lines, key, focusable, |ui, line| match line {
        Line::Heading(index) => release_heading(app, ui, artist_id, page, releases[index]),
        Line::Columns(_) => columns(ui, app),
        Line::Song(index, row) => {
            let album = releases[index];
            let Some(song) = page
                .tracks
                .get(&album.id)
                .and_then(ReleaseTracks::songs)
                .and_then(|songs| songs.get(row))
            else {
                return;
            };
            let item = PlayableItem::Track(song.clone());
            let context = RowContext::Discography {
                artist_id: artist_id.to_string(),
                album_id: album.id.clone(),
            };
            // No picking out rows on this page: a click focuses the row
            // and a double-click plays from it.
            let draw = |ui: &mut egui::Ui, app: &mut App| {
                let _ = widgets::track_row(
                    ui,
                    app,
                    TrackRow {
                        index: row,
                        number: Some(row + 1),
                        item: &item,
                        context: &context,
                        show_cover: false,
                        show_album: false,
                        added_at: None,
                        added_by: None,
                        show_added_by: false,
                        show_plays: false,
                        compact: false,
                        thin,
                        shift: 0.0,
                        picked: false,
                        picked_songs: &[],
                        picked_positions: &[],
                        position: None,
                    },
                );
            };
            // Offline the songs stay readable but, as in other track lists,
            // cannot be played: the play would stop short of the page's end.
            if app.network_reachable {
                draw(ui, app);
            } else {
                ui.add_enabled_ui(false, |ui| {
                    ui.multiply_opacity(0.55);
                    draw(ui, app);
                });
            }
        }
        Line::Reserved(index, 0) => {
            let album = releases[index];
            let height = ui.available_height();
            match (page.tracks.get(&album.id), &page.halted) {
                (Some(ReleaseTracks::Failed(error)), _) => {
                    let error = widgets::sanitize_error_message(error, locale);
                    if placeholder_row(ui, &palette, locale, height, &error, true) {
                        app.actions.push(Action::RetryDiscography {
                            artist_id: artist_id.to_string(),
                            album_id: Some(album.id.clone()),
                        });
                    }
                }
                (_, Some(error)) => {
                    let error = widgets::sanitize_error_message(error, locale);
                    if placeholder_row(ui, &palette, locale, height, &error, true) {
                        app.actions.push(page_retry());
                    }
                }
                _ if !app.network_reachable => {
                    placeholder_row(
                        ui,
                        &palette,
                        locale,
                        height,
                        &gettext(locale, "You're offline. Reconnect to load more songs."),
                        false,
                    );
                }
                _ => widgets::loading_row(ui, &palette, locale),
            }
        }
        Line::Reserved(..) | Line::Gap(_) => {}
    });
    // Tell the app which releases are on screen, when that changes, so
    // their songs load first.
    if let Some((first, last)) = drawn {
        let shown = (lines[first].0.release(), lines[last].0.release());
        if page.in_view != Some(shown) {
            app.actions.push(Action::DiscographyInView {
                artist_id: artist_id.to_string(),
                first: shown.0,
                last: shown.1,
            });
        }
    }
}

/// The artist's name, with the filter and the order on the right.
fn heading(app: &mut App, ui: &mut egui::Ui, artist_id: &str, page: &DiscographyPage, title: &str) {
    let palette = app.palette;
    let locale = app.locale;
    let filters: Vec<(DiscographyFilter, String)> = DiscographyFilter::ALL
        .iter()
        .map(|filter| (*filter, filter.label(locale).into_owned()))
        .collect();
    // The filter, then the order, in reading and Tab order at the right
    // edge, with the name in the room left. The controls' width comes from
    // the last frame, so a long name gives way to them.
    let width_id = ui.id().with("discography-controls-width");
    let width = ui
        .ctx()
        .data(|data| data.get_temp::<f32>(width_id))
        .unwrap_or(260.0);
    let gap = ui.spacing().item_spacing.x;
    let mut row = ui.available_rect_before_wrap();
    row.max.y = row.min.y + 36.0;
    let controls_rect = row.with_min_x((row.max.x - width).max(row.min.x));
    let mut controls = ui.new_child(
        UiBuilder::new()
            .max_rect(controls_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let filter = filter_menu(app, &mut controls, &filters, page.filter);
    let newest_first = order_toggle(app, &mut controls, page.newest_first);
    let used = controls.min_rect().width();
    if (used - width).abs() > 0.5 {
        ui.ctx().data_mut(|data| data.insert_temp(width_id, used));
        ui.ctx().request_repaint();
    }
    let mut name = ui.new_child(
        UiBuilder::new()
            .max_rect(row.with_max_x((controls_rect.min.x - gap).max(row.min.x)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    theme::text(&mut name, title, theme::bold(28.0), palette.text);
    ui.advance_cursor_after_rect(name.min_rect() | controls.min_rect() | row);
    if let Some(newest_first) = newest_first {
        app.actions.push(Action::SetDiscographyOrder {
            artist_id: artist_id.to_string(),
            newest_first,
        });
    }
    if let Some(filter) = filter {
        app.actions.push(Action::SetDiscographyPageFilter {
            artist_id: artist_id.to_string(),
            filter,
        });
    }
}

/// A pill styled like the library's sort menu.
fn pill(app: &App, ui: &mut egui::Ui, icon: Icon, label: &str) -> egui::Response {
    let palette = app.palette;
    ui.add(
        egui::Button::image_and_text(
            icon.image(palette.text, 15.0),
            egui::RichText::new(label).font(theme::medium(13.0)),
        )
        .fill(palette.surface)
        .corner_radius(12)
        .min_size(vec2(0.0, 28.0)),
    )
}

/// Newest first or oldest first, switched by a click. Returns the new order.
fn order_toggle(app: &App, ui: &mut egui::Ui, newest_first: bool) -> Option<bool> {
    let locale = app.locale;
    let label = if newest_first {
        // Translators: the order of an artist's releases on their discography page.
        gettext(locale, "Newest first")
    } else {
        // Translators: the order of an artist's releases on their discography page.
        gettext(locale, "Oldest first")
    };
    // A clock rather than a chevron: a click turns the order round instead
    // of opening a menu.
    pill(app, ui, Icon::Clock, &label)
        .clicked()
        .then_some(!newest_first)
}

/// The filter, as a menu of the artist page's filters. Returns a different
/// filter when one is chosen.
fn filter_menu(
    app: &App,
    ui: &mut egui::Ui,
    options: &[(DiscographyFilter, String)],
    selected: DiscographyFilter,
) -> Option<DiscographyFilter> {
    let palette = app.palette;
    let label = options
        .iter()
        .find(|(value, _)| *value == selected)
        .map_or(String::new(), |(_, label)| label.clone());
    let response = pill(app, ui, Icon::ChevronDown, &label);
    // Announced as the filter it is and what it shows, not as a button
    // named after its value.
    // Translators: the name of the menu choosing which of an artist's
    // releases show, read out by screen readers; a noun, not a command.
    let name = pgettext(app.locale, "menu name", "Filter");
    response.widget_info(|| {
        let mut info =
            egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), name.as_ref());
        info.current_text_value = Some(label.clone());
        info
    });
    let mut picked = None;
    egui::Popup::menu(&response)
        .frame(widgets::menu_frame(&palette))
        .align(egui::RectAlign::BOTTOM_END)
        .show(|ui| {
            let width = options
                .iter()
                .map(|(_, label)| {
                    ui.painter()
                        .layout_no_wrap(label.clone(), theme::regular(13.5), palette.text)
                        .size()
                        .x
                })
                .fold(140.0_f32, f32::max)
                + 52.0;
            ui.set_width(width.min(ui.ctx().content_rect().width() - 24.0));
            for (value, label) in options {
                if widgets::menu_item(
                    ui,
                    &palette,
                    (*value == selected).then_some(Icon::Check),
                    label,
                ) && *value != selected
                {
                    picked = Some(*value);
                }
            }
        });
    picked
}

/// A release's artwork, name, "Kind • Year • N songs" and its buttons.
fn release_heading(
    app: &mut App,
    ui: &mut egui::Ui,
    artist_id: &str,
    page: &DiscographyPage,
    album: &Album,
) {
    let palette = app.palette;
    ui.add_space(HEADER_TOP);
    ui.horizontal_top(|ui| {
        // The title opens the album; the artwork is only a picture, so it is
        // not another stop for Tab.
        let (cover, _) = ui.allocate_exact_size(vec2(COVER, COVER), Sense::hover());
        widgets::paint_cover(
            ui,
            &palette,
            pick_image(&album.images, 300),
            cover,
            theme::RADIUS_SMALL as f32,
            Icon::Disc,
            Some(app.backend.art()),
        );
        ui.add_space(16.0);
        ui.vertical(|ui| {
            ui.add_space(6.0);
            if theme::link(ui, &album.name, theme::bold(24.0), palette.text).clicked() {
                app.actions
                    .push(Action::Open(Page::Album(album.id.clone())));
            }
            ui.add_space(2.0);
            let count = page
                .tracks
                .get(&album.id)
                .and_then(ReleaseTracks::songs)
                .map(|songs| songs.len() as u32)
                .or(album.total_tracks);
            let mut byline = vec![app.album_kind_label(album).into_owned()];
            byline.extend(album.year().map(str::to_string));
            byline.extend(count.map(|count| app.locale.song_count(count)));
            theme::text(
                ui,
                byline.join(" • "),
                theme::regular(13.5),
                palette.secondary,
            );
            ui.add_space(12.0);
            // Offline, as on album pages, a release can be neither played
            // nor saved: its songs could not load to the end of the page.
            let reachable = app.network_reachable;
            ui.horizontal(|ui| {
                ui.add_enabled_ui(reachable, |ui| {
                    release_buttons(app, ui, artist_id, page, album);
                });
            });
        });
    });
}

/// Play from this release to the end of the page, save it, and its menu.
fn release_buttons(
    app: &mut App,
    ui: &mut egui::Ui,
    artist_id: &str,
    page: &DiscographyPage,
    album: &Album,
) {
    let palette = app.palette;
    let locale = app.locale;
    let tracks = page.tracks.get(&album.id);
    let songs = tracks.and_then(ReleaseTracks::songs);
    let current = app.current_track_uri();
    let playing_here = current
        .as_ref()
        .is_some_and(|uri| songs.is_some_and(|songs| songs.iter().any(|song| song.uri == *uri)))
        && app.believed_playing();
    let first_song = songs.and_then(|songs| songs.iter().find(|song| playable(song)));
    // A release whose songs arrived, or failed, with none to play.
    let playable_release = !tracks.is_some_and(ReleaseTracks::resolved) || first_song.is_some();
    let pending =
        app.play_pending(&album.uri) || first_song.is_some_and(|song| app.play_pending(&song.uri));
    let (icon, tooltip) = if playing_here {
        (Icon::PauseFilled, gettext(locale, "Pause {name}"))
    } else {
        (Icon::PlayFilled, gettext(locale, "Play {name}"))
    };
    let tooltip = tooltip.replace("{name}", &album.name);
    let clicked = if pending {
        theme::circle_spinner(
            ui,
            PLAY_DIAMETER,
            palette.accent,
            palette.on_accent,
            &tooltip,
        );
        false
    } else {
        ui.add_enabled_ui(playable_release, |ui| {
            theme::circle_button(
                ui,
                icon,
                PLAY_DIAMETER,
                palette.accent,
                palette.accent_hover,
                palette.on_accent,
                &tooltip,
            )
            .clicked()
        })
        .inner
    };
    if clicked {
        if playing_here {
            app.actions.push(Action::TogglePlay);
        } else {
            app.actions.push(Action::PlayDiscography {
                artist_id: artist_id.to_string(),
                album_id: album.id.clone(),
                start: None,
            });
        }
    }
    let saved = app.is_saved(&album.uri) == Some(true);
    // A saved button stays green under the pointer, as on album pages.
    let (icon, tooltip, color, hover) = if saved {
        (
            Icon::CircleCheck,
            gettext(locale, "Remove from Your Library"),
            palette.accent,
            palette.accent_hover,
        )
    } else {
        (
            Icon::CirclePlus,
            gettext(locale, "Save to Your Library"),
            palette.secondary,
            palette.text,
        )
    };
    if theme::icon_button(ui, icon, 22.0, color, hover, &tooltip).clicked() {
        app.actions.push(Action::ToggleSaved(album.uri.clone()));
    }
    let more = theme::icon_button(
        ui,
        Icon::Ellipsis,
        22.0,
        palette.secondary,
        palette.text,
        &gettext(locale, "More"),
    );
    egui::Popup::menu(&more)
        .id(ui.make_persistent_id(("discography-release-menu", &album.uri)))
        .frame(widgets::menu_frame(&palette))
        .show(|ui| widgets::context_menu_items(ui, app, &album.uri, &album.name, None));
}

/// Whether Spotify can be asked to play the song, as for any track row.
fn playable(song: &crate::api::models::Track) -> bool {
    !song.uri.is_empty()
        && !song.uri.starts_with("spotify:local:")
        && !song.is_local
        && song.is_playable != Some(false)
}

/// The headings over a release's songs, lined up with the song rows.
fn columns(ui: &mut egui::Ui, app: &App) {
    let palette = app.palette;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, COLUMNS_HEIGHT), Sense::hover());
    let font = theme::regular(12.0);
    let painter = ui.painter();
    painter.text(
        egui::pos2(rect.left() + 8.0 + 22.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        "#",
        font.clone(),
        palette.secondary,
    );
    let clock = Rect::from_center_size(
        egui::pos2(rect.right() - 36.0 - 56.0 / 2.0 - 6.0, rect.center().y),
        vec2(15.0, 15.0),
    );
    crate::bidi::paint_line(
        painter,
        rect.left() + 8.0 + 44.0,
        clock.left() - 8.0,
        rect.center().y,
        &pgettext(app.locale, "column heading", "TITLE"),
        font,
        palette.secondary,
    );
    Icon::Clock
        .image(palette.secondary, 15.0)
        .paint_at(ui, clock);
    ui.painter().hline(
        rect.x_range().shrink(8.0),
        rect.bottom() - 0.5,
        egui::Stroke::new(1.0, palette.outline),
    );
}

/// An error with a Retry button that sends `retry`.
fn retry_row(ui: &mut egui::Ui, app: &mut App, message: &str, retry: Action) {
    let palette = app.palette;
    let message = widgets::sanitize_error_message(message, app.locale);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        theme::icon(ui, Icon::CircleAlert, 16.0, palette.danger);
        theme::text(
            ui,
            message.as_ref(),
            theme::regular(13.0),
            palette.secondary,
        );
        if theme::soft_button(
            ui,
            &palette,
            Some(Icon::Refresh),
            &gettext(app.locale, "Retry"),
            false,
        )
        .clicked()
        {
            app.actions.push(retry);
        }
    });
}

/// Lays out only the lines that meet the visible part of the enclosing
/// scroll view, plus one neighbour on either side so Tab can reach it and
/// scroll it in. Every line takes exactly its given height, so the lines
/// skipped above and below are stood in for by empty space. Each drawn line
/// gets the id `key` names, independent of what was drawn before it.
/// Returns the first and last line drawn.
fn measured_lines(
    ui: &mut egui::Ui,
    lines: &[(Line, f32)],
    key: impl Fn(Line) -> Id,
    focusable: impl Fn(Line) -> bool,
    mut draw: impl FnMut(&mut egui::Ui, Line),
) -> Option<(usize, usize)> {
    if lines.is_empty() {
        return None;
    }
    let previous_spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing.y = 0.0;
    let clip = ui.clip_rect();
    let width = ui.available_width();
    let range = visible_lines(
        ui.cursor().top(),
        lines.iter().map(|(_, height)| *height),
        clip,
    );
    let range = reach_focusable(range, lines.len(), |index| focusable(lines[index].0));
    let mut top = ui.cursor().top();
    let mut tops = Vec::with_capacity(lines.len() + 1);
    for (_, height) in lines {
        tops.push(top);
        top += height;
    }
    tops.push(top);
    let (first, last) = range;
    if first > 0 {
        ui.allocate_space(vec2(width, tops[first] - tops[0]));
    }
    for (line, height) in &lines[first..last] {
        let (_, rect) = ui.allocate_space(vec2(width, *height));
        let mut child = ui.new_child(
            UiBuilder::new()
                .id(key(*line))
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        draw(&mut child, *line);
    }
    if last < lines.len() {
        ui.allocate_space(vec2(width, tops[lines.len()] - tops[last]));
    }
    ui.spacing_mut().item_spacing = previous_spacing;
    (first < last).then(|| (first, last - 1))
}

/// Widens a drawn range past lines that hold nothing to focus, so the
/// neighbour Tab moves to next is drawn even across a release's headings,
/// spacing and the room held for songs on their way.
fn reach_focusable(
    (mut first, mut last): (usize, usize),
    count: usize,
    focusable: impl Fn(usize) -> bool,
) -> (usize, usize) {
    while first > 0 && first < count && !focusable(first) {
        first -= 1;
    }
    while last < count && last > 0 && !focusable(last - 1) {
        last += 1;
    }
    (first, last)
}

/// The lines to draw, as a half-open range: those that meet `clip`, with
/// one neighbour on either side, for lines of the given heights laid out
/// from `top`.
fn visible_lines(top: f32, heights: impl Iterator<Item = f32>, clip: Rect) -> (usize, usize) {
    let mut first = None;
    let mut last = 0;
    let mut y = top;
    let mut count = 0;
    for (index, height) in heights.enumerate() {
        let bottom = y + height;
        if first.is_none() && bottom > clip.top() {
            first = Some(index);
        }
        if y < clip.bottom() {
            last = index + 1;
        }
        y = bottom;
        count = index + 1;
    }
    let first = first.unwrap_or(count).saturating_sub(1);
    let last = (last + 1).min(count);
    (first.min(last), last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(top: f32, bottom: f32) -> Rect {
        Rect::from_min_max(egui::pos2(0.0, top), egui::pos2(100.0, bottom))
    }

    #[test]
    fn only_the_lines_on_screen_and_a_neighbour_each_side_are_drawn() {
        // Lines of 100, 30, 56, 56, 56 and 100 from y = 0; the view shows
        // y 120 to 200, the 30 and the first two 56s.
        let heights = [100.0, 30.0, 56.0, 56.0, 56.0, 100.0];
        assert_eq!(
            visible_lines(0.0, heights.into_iter(), clip(120.0, 200.0)),
            (0, 5)
        );
        // Scrolled to the end: the last line and the one before it.
        assert_eq!(
            visible_lines(0.0, heights.into_iter(), clip(300.0, 398.0)),
            (4, 6)
        );
    }

    #[test]
    fn a_list_below_the_view_draws_only_its_first_line() {
        let heights = [56.0; 10];
        assert_eq!(
            visible_lines(1000.0, heights.into_iter(), clip(0.0, 500.0)),
            (0, 1)
        );
    }

    #[test]
    fn a_list_above_the_view_draws_only_its_last_line() {
        let heights = [56.0; 10];
        assert_eq!(
            visible_lines(0.0, heights.into_iter(), clip(1000.0, 1500.0)),
            (9, 10)
        );
    }

    #[test]
    fn the_drawn_range_reaches_past_spacing_to_something_tab_can_focus() {
        // Already focusable at both ends: nothing changes.
        let focusable = [true, false, true, false, true];
        assert_eq!(reach_focusable((2, 5), 5, |index| focusable[index]), (2, 5));
        // Spacing at an end: the range grows until that end holds a line
        // Tab can reach.
        let focusable = [true, false, false, false, true, false, false, true];
        assert_eq!(reach_focusable((2, 5), 8, |index| focusable[index]), (0, 5));
        assert_eq!(reach_focusable((4, 6), 8, |index| focusable[index]), (4, 8));
        // Nothing focusable at all: the whole list, and no further.
        assert_eq!(reach_focusable((1, 2), 3, |_| false), (0, 3));
        assert_eq!(reach_focusable((0, 0), 0, |_| false), (0, 0));
    }

    #[test]
    fn an_empty_list_draws_nothing() {
        assert_eq!(
            visible_lines(0.0, std::iter::empty(), clip(0.0, 500.0)),
            (0, 0)
        );
    }

    /// Focus, open menus and screen readers follow a song by its id, so the
    /// id must not depend on which lines above it happen to be drawn.
    #[test]
    fn a_line_keeps_its_id_whatever_is_drawn_above_it() {
        let ctx = egui::Context::default();
        let lines: Vec<(Line, f32)> = (0..40).map(|row| (Line::Song(0, row), 56.0)).collect();
        let mut ids = Vec::new();
        let mut first_drawn = Vec::new();
        for offset in [300.0, 600.0] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(400.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    egui::ScrollArea::vertical()
                        .vertical_scroll_offset(offset)
                        .show(ui, |ui| {
                            let drawn = measured_lines(
                                ui,
                                &lines,
                                |line| match line {
                                    Line::Song(_, row) => Id::new(("song", row)),
                                    _ => Id::new("other"),
                                },
                                |_| true,
                                |ui, line| {
                                    if matches!(line, Line::Song(_, 15)) {
                                        ids.push(ui.unique_id());
                                    }
                                },
                            );
                            first_drawn.push(drawn.map(|(first, _)| first));
                        });
                },
            );
            output.textures_delta.clear();
        }
        assert_ne!(
            first_drawn[0], first_drawn[1],
            "the scroll changed what is drawn above"
        );
        assert_eq!(ids.len(), 2, "line 15 is on screen at both offsets");
        assert_eq!(ids[0], ids[1]);
    }
}
