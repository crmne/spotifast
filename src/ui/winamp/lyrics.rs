//! The lyrics window, between the equalizer and the playlist.
//!
//! Winamp had no such window, so it borrows the playlist's parts: the
//! title bar with the name written over its plain ground, the side tiles,
//! and the last rows of the bottom bar for an edge, with the bar's grip in
//! the corner while nothing hangs below. The words are the ones the main
//! window's panel shows, cut into rows the window's width in the playlist's
//! font and colours, and they behave as the panel's do: the line being sung
//! is lit the way the playlist lights the playing song, a timed line plays
//! from where it is clicked, and the wheel stops following the song until
//! FOLLOW picks it back up.

use egui::{Color32, Sense};

use crate::app::{App, NowPlaying};
use crate::i18n::{gettext, pgettext};
use crate::lyrics::Line;
use crate::model::{Action, Loadable};
use crate::skin::layout::{self, Area};
use crate::skin::sprites;

use super::View;

/// The window's name, in English as the skin's own PLAYLIST is.
const TITLE: &str = "LYRICS";
/// The button that follows the song again, in the same English.
const FOLLOW: &str = "FOLLOW";
/// Room between the rows' text and the tiles either side.
const PAD: u32 = 3;

/// What the window says: the lines, and the one being sung.
struct Words {
    lines: Vec<Line>,
    active: Option<usize>,
    /// The lines are the song's words, not a line saying why there are none.
    lyrics: bool,
    /// The lines carry timing.
    synced: bool,
    /// How far into the song, from 0 to 1, for following words without
    /// timing by the clock as the panel does.
    progress: f32,
    /// Clicking the lines asks again, after a failure.
    retry: bool,
}

/// The lines cut into rows that fit the window, each row with the line it
/// belongs to. Measuring text is not free and the window redraws with the
/// visualizer, so the rows are kept until the words change.
#[derive(Default)]
pub struct LyricsRows {
    source: Vec<Line>,
    rows: Vec<(usize, String)>,
}

/// Where the song is in the rows it follows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sung {
    /// The first row of the line being sung, from words with timing.
    Row(usize),
    /// The part of the song played, for words without.
    Progress(f32),
}

pub(super) fn show(app: &mut App, view: &mut View, now: Option<&NowPlaying>, focused: bool) {
    let words = words(app, now);
    let lines = app.winamp.lyrics_lines;
    let height = layout::lyrics_height(lines);
    let area = text_area(lines);
    if app.winamp.lyrics_rows.source != words.lines {
        let room = (area.width - 2 * PAD) as f32;
        let text = &mut app.winamp.playlist_text;
        let rows = words
            .lines
            .iter()
            .enumerate()
            .flat_map(|(index, line)| {
                wrap(&line.text, room, &mut |row| text.width(row))
                    .into_iter()
                    .map(move |row| (index, row))
            })
            .collect();
        app.winamp.lyrics_rows = LyricsRows {
            source: words.lines.clone(),
            rows,
        };
    }
    let rows = std::mem::take(&mut app.winamp.lyrics_rows.rows);
    let visible = lines as usize;
    let scrolls = words.lyrics && rows.len() > visible;
    bar(app, view, focused, scrolls && !app.lyrics_following);
    frame(view, height);
    if !app.settings.playlist_open {
        grip(app, view, height);
    }

    let style = view.skin.playlist.clone();
    let rgb = |color: [u8; 3]| Color32::from_rgb(color[0], color[1], color[2]);
    view.fill(
        area.x,
        area.y,
        area.width,
        area.height,
        rgb(style.normal_background),
    );

    let sung = (words.lyrics && app.lyrics_following).then(|| {
        if words.synced {
            // Before the first line starts, the song is at the top.
            let line = words.active.unwrap_or(0);
            Sung::Row(rows.iter().position(|(of, _)| *of == line).unwrap_or(0))
        } else {
            Sung::Progress(words.progress)
        }
    });
    let first = first_row(rows.len(), visible, sung, app.winamp.lyrics_scroll);
    app.winamp.lyrics_scroll = first;

    // `contains_pointer`, not `hovered`: the rows drawn on top of the area
    // would take the hover and the wheel with it, as in the playlist.
    let hovered = view
        .interact(area, "lyrics-lines", Sense::hover())
        .contains_pointer();
    if scrolls && hovered {
        // The playlist's wheel: three rows a notch.
        let row_points = layout::PLAYLIST_TRACK_HEIGHT as f32 * view.unit;
        let (lines, points, pages) = super::playlist::wheel(view.ui.ctx());
        app.winamp.lyrics_wheel +=
            super::playlist::rows_for_wheel(lines, points, pages, row_points, visible);
        let rows_moved = app.winamp.lyrics_wheel.trunc();
        if rows_moved != 0.0 {
            app.winamp.lyrics_wheel -= rows_moved;
            let scroll = first as i64 + rows_moved as i64;
            app.winamp.lyrics_scroll = scroll.max(0) as usize;
            // Scrolling by hand means the reader wants to look elsewhere;
            // FOLLOW in the title bar picks the song back up.
            if app.lyrics_following {
                app.actions.push(Action::PauseLyricsFollow);
            }
        }
    }

    // The rows first, to know which line the pointer is on: a line cut
    // into rows lights up whole.
    let shown: Vec<(Area, usize, &str)> = rows
        .iter()
        .skip(first)
        .take(visible)
        .enumerate()
        .map(|(offset, (line, text))| {
            let row = Area::new(
                area.x,
                area.y + layout::LYRICS_PADDING + offset as u32 * layout::PLAYLIST_TRACK_HEIGHT,
                area.width,
                layout::PLAYLIST_TRACK_HEIGHT,
            );
            (row, *line, text.as_str())
        })
        .collect();
    let mut pointed = None;
    for (offset, (row, line, _)) in shown.iter().enumerate() {
        let seek = words.lines[*line].at_ms.filter(|_| words.synced);
        let sense = if seek.is_some() || words.retry {
            Sense::click()
        } else {
            Sense::hover()
        };
        let response = view.interact(*row, &format!("lyrics-row-{}", first + offset), sense);
        if let Some(at_ms) = seek {
            if response.hovered() {
                pointed = Some(*line);
            }
            if response.clicked() {
                app.actions.push(Action::Seek(at_ms));
                app.actions.push(Action::FollowLyrics);
            }
        }
        if words.retry && response.clicked() {
            app.actions.push(Action::RetryLyrics);
        }
    }

    let ctx = view.ui.ctx().clone();
    let clip = view.rect(area).intersect(view.ui.clip_rect());
    let painter = view.ui.painter().with_clip_rect(clip);
    for (row, line, text) in shown {
        if pointed == Some(line) {
            painter.rect_filled(view.rect(row), 0.0, rgb(style.selected_background));
        }
        let color = rgb(if words.active == Some(line) {
            style.current
        } else {
            style.normal
        });
        let room = row.width - 2 * PAD;
        let image = app.winamp.playlist_text.line(&ctx, text);
        let at = Area::new(
            row.x + PAD,
            row.y + row.height.saturating_sub(image.height) / 2,
            image.width.min(room),
            image.height,
        );
        let uv_right = at.width as f32 / image.width.max(1) as f32;
        painter.image(
            image.texture.id(),
            view.rect(at),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(uv_right, 1.0)),
            color,
        );
    }
    app.winamp.lyrics_rows.rows = rows;
}

/// The title bar: the playlist's, its rules running the whole width, with
/// the window's name written over a gap of plain ground where the
/// playlist's name sits, a place to drag the player by, the X that closes
/// it, and FOLLOW at the left while the words have been scrolled away from
/// the song.
fn bar(app: &mut App, view: &mut View, focused: bool, follow: bool) {
    use sprites::*;
    let (top_left, top_tile, top_right, ground) = if focused {
        (
            PLAYLIST_TOP_LEFT_ACTIVE,
            PLAYLIST_TOP_TILE_ACTIVE,
            PLAYLIST_TOP_RIGHT_ACTIVE,
            PLAYLIST_TITLE_GROUND_ACTIVE,
        )
    } else {
        (
            PLAYLIST_TOP_LEFT,
            PLAYLIST_TOP_TILE,
            PLAYLIST_TOP_RIGHT,
            PLAYLIST_TITLE_GROUND,
        )
    };
    let tile = layout::PLAYLIST_TILE_WIDTH;
    let width = layout::WINDOW_WIDTH;
    let height = layout::LYRICS_TITLE_HEIGHT;
    view.sprite_at(top_left, 0, 0);
    let run = Area::new(tile, 0, width - 2 * tile, height);
    let mut x = tile;
    while x < width - tile {
        view.sprite_clipped(top_tile, x, 0, run);
        x += tile;
    }
    view.sprite_at(top_right, width - tile, 0);
    // Centred where the playlist's title is, with room either side.
    let centre = tile + (width - 2 * tile - 100) / 2 + 50;
    let name_width = 5 * TITLE.len() as u32;
    let gap = name_width + 8;
    view.sprite_stretched(ground, Area::new(centre - gap / 2, 0, gap, height));
    view.text(TITLE, Area::new(centre - name_width / 2, 7, name_width, 6));
    // The playlist's roll-up button means nothing here.
    let shade = layout::PLAYLIST_SHADE;
    view.sprite_stretched(ground, Area::new(shade.x, 0, shade.width, height));
    let title = view.interact(
        Area::new(0, 0, width, height),
        "lyrics-title",
        Sense::click_and_drag(),
    );
    if title.drag_started() {
        view.ui
            .ctx()
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if follow {
        let button = layout::LYRICS_FOLLOW;
        view.sprite_stretched(ground, Area::new(button.x, 0, button.width, height));
        let response = view
            .interact(button, "lyrics-follow", Sense::click())
            .on_hover_text(pgettext(app.locale, "lyrics", "Follow").as_ref());
        let letters = Area::new(button.x + 4, 7, button.width - 8, 6);
        if response.hovered() {
            let [r, g, b] = view.skin.playlist.current;
            view.text_lit(FOLLOW, letters, Color32::from_rgb(r, g, b));
        } else {
            view.text(FOLLOW, letters);
        }
        if response.clicked() {
            app.actions.push(Action::FollowLyrics);
        }
    }
    if view
        .lamp_button(
            layout::PLAYLIST_CLOSE,
            PLAYLIST_CLOSE_PRESSED,
            false,
            "lyrics-close",
        )
        .clicked()
    {
        app.actions.push(Action::ToggleWinampLyrics);
    }
}

/// The side tiles down the lines, and the edge under them.
fn frame(view: &mut View, height: u32) {
    use sprites::*;
    let middle = Area::new(
        0,
        layout::LYRICS_TITLE_HEIGHT,
        layout::WINDOW_WIDTH,
        height - layout::LYRICS_TITLE_HEIGHT - layout::LYRICS_BOTTOM_HEIGHT,
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
    let bottom = height - layout::LYRICS_BOTTOM_HEIGHT;
    view.sprite_at(PLAYLIST_BOTTOM_EDGE_LEFT, 0, bottom);
    view.sprite_at(PLAYLIST_BOTTOM_EDGE_RIGHT, 125, bottom);
}

/// The corner that stretches the window a row at a time, as the
/// playlist's does by its tiles.
fn grip(app: &mut App, view: &mut View, height: u32) {
    let corner = Area::new(
        layout::WINDOW_WIDTH - layout::PLAYLIST_GRIP,
        height - layout::PLAYLIST_GRIP,
        layout::PLAYLIST_GRIP,
        layout::PLAYLIST_GRIP,
    );
    view.sprite_at(sprites::PLAYLIST_GRIP_CORNER, corner.x, corner.y);
    let response = view
        .interact(corner, "lyrics-grip", Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if response.dragged() {
        app.winamp.lyrics_resize += response.drag_delta().y / view.unit;
        let step = layout::PLAYLIST_TRACK_HEIGHT as f32;
        let steps = (app.winamp.lyrics_resize / step).trunc();
        if steps != 0.0 {
            app.winamp.lyrics_resize -= steps * step;
            let lines = app.winamp.lyrics_lines;
            let wanted = (lines as i64 + steps as i64).clamp(
                layout::LYRICS_MIN_LINES as i64,
                layout::LYRICS_MAX_LINES as i64,
            ) as u32;
            if wanted != lines {
                app.actions.push(Action::SetLyricsLines(wanted));
            }
        }
    }
    if response.drag_stopped() {
        app.winamp.lyrics_resize = 0.0;
    }
}

/// The lines' own area, between the tiles.
fn text_area(lines: u32) -> Area {
    let height = layout::lyrics_height(lines);
    Area::new(
        layout::PLAYLIST_LEFT_WIDTH,
        layout::LYRICS_TITLE_HEIGHT,
        layout::WINDOW_WIDTH - layout::PLAYLIST_LEFT_WIDTH - layout::PLAYLIST_RIGHT_WIDTH,
        height - layout::LYRICS_TITLE_HEIGHT - layout::LYRICS_BOTTOM_HEIGHT,
    )
}

/// The playing track's words, or a line saying why there are none, in the
/// panel's own words.
fn words(app: &App, now: Option<&NowPlaying>) -> Words {
    let say = |text: String| Words {
        lines: vec![Line { at_ms: None, text }],
        active: None,
        lyrics: false,
        synced: false,
        progress: 0.0,
        retry: false,
    };
    let Some(now) = now else {
        return say(gettext(app.locale, "Nothing playing").into_owned());
    };
    if !app.network_reachable && !matches!(app.lyrics, Loadable::Loaded(_)) {
        return say(gettext(app.locale, "You're offline").into_owned());
    }
    match &app.lyrics {
        Loadable::NotLoaded | Loadable::Loading => {
            say(gettext(app.locale, "Loading…").into_owned())
        }
        Loadable::Failed(error) => Words {
            retry: true,
            ..say(gettext(
                app.locale,
                // Translators: Keep {error} unchanged. It is the original failure detail.
                "Couldn't fetch the lyrics: {error}",
            )
            .replace("{error}", error))
        },
        Loadable::Loaded(None) => {
            say(gettext(app.locale, "No lyrics found for this track.").into_owned())
        }
        Loadable::Loaded(Some(lyrics)) if lyrics.instrumental => {
            say(gettext(app.locale, "Instrumental").into_owned())
        }
        Loadable::Loaded(Some(lyrics)) => Words {
            lines: lyrics.lines.clone(),
            active: lyrics.active_line(now.position_ms),
            lyrics: true,
            synced: lyrics.synced,
            progress: if now.duration_ms > 0 {
                (now.position_ms as f32 / now.duration_ms as f32).clamp(0.0, 1.0)
            } else {
                0.0
            },
            retry: false,
        },
    }
}

/// A line cut into rows no wider than `room`, between words where it can
/// be. A word wider than the window, or a script written without spaces,
/// is cut between characters instead.
fn wrap(text: &str, room: f32, width: &mut impl FnMut(&str) -> f32) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        let joined = if row.is_empty() {
            word.to_string()
        } else {
            format!("{row} {word}")
        };
        if width(&joined) <= room {
            row = joined;
            continue;
        }
        if !row.is_empty() {
            rows.push(std::mem::take(&mut row));
            if width(word) <= room {
                row = word.to_string();
                continue;
            }
        }
        for character in word.chars() {
            row.push(character);
            if width(&row) > room && row.chars().count() > 1 {
                row.pop();
                rows.push(std::mem::take(&mut row));
                row.push(character);
            }
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// The first row in view. Following words with timing keeps the line
/// being sung second, so the row before it still shows; following words
/// without puts the view as far through them as the song is. Otherwise the
/// rows stay where the wheel left them. None scrolls past the last row.
fn first_row(count: usize, visible: usize, sung: Option<Sung>, scroll: usize) -> usize {
    let most = count.saturating_sub(visible);
    match sung {
        Some(Sung::Row(row)) => row.saturating_sub(1).min(most),
        Some(Sung::Progress(progress)) => (progress * most as f32).round() as usize,
        None => scroll.min(most),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five pixels a character, as the skin's own font is.
    fn cells(text: &str) -> f32 {
        5.0 * text.chars().count() as f32
    }

    #[test]
    fn a_long_line_is_cut_between_words() {
        assert_eq!(wrap("short line", 100.0, &mut cells), vec!["short line"]);
        assert_eq!(
            wrap("I heard the piledriver waltz", 60.0, &mut cells),
            vec!["I heard the", "piledriver", "waltz"]
        );
    }

    #[test]
    fn a_word_wider_than_the_window_is_cut_between_characters() {
        assert_eq!(
            wrap("abcdefgh ij", 20.0, &mut cells),
            vec!["abcd", "efgh", "ij"]
        );
        assert_eq!(
            wrap("春の小川はさらさら", 20.0, &mut cells),
            vec!["春の小川", "はさらさ", "ら"]
        );
        assert_eq!(wrap("", 20.0, &mut cells), vec![""]);
    }

    #[test]
    fn the_sung_line_stays_second_until_the_end() {
        assert_eq!(first_row(20, 4, Some(Sung::Row(0)), 0), 0);
        assert_eq!(first_row(20, 4, Some(Sung::Row(1)), 0), 0);
        assert_eq!(first_row(20, 4, Some(Sung::Row(5)), 0), 4);
        assert_eq!(first_row(20, 4, Some(Sung::Row(19)), 0), 16);
        assert_eq!(first_row(20, 10, Some(Sung::Row(19)), 0), 10);
        // Following the song, the scroll left behind does not count.
        assert_eq!(first_row(20, 4, Some(Sung::Row(5)), 12), 4);
    }

    #[test]
    fn untimed_words_follow_the_clock() {
        assert_eq!(first_row(20, 4, Some(Sung::Progress(0.0)), 9), 0);
        assert_eq!(first_row(20, 4, Some(Sung::Progress(0.5)), 9), 8);
        assert_eq!(first_row(20, 4, Some(Sung::Progress(1.0)), 9), 16);
        assert_eq!(first_row(3, 4, Some(Sung::Progress(0.5)), 0), 0);
    }

    #[test]
    fn rows_not_followed_stay_where_the_wheel_left_them() {
        assert_eq!(first_row(20, 4, None, 0), 0);
        assert_eq!(first_row(20, 4, None, 7), 7);
        assert_eq!(first_row(20, 4, None, 50), 16);
        assert_eq!(first_row(3, 4, None, 2), 0);
    }

    #[test]
    fn the_rows_fill_the_window_between_its_bar_and_edge() {
        for lines in [layout::LYRICS_MIN_LINES, layout::LYRICS_MAX_LINES] {
            assert_eq!(
                text_area(lines).height,
                2 * layout::LYRICS_PADDING + lines * layout::PLAYLIST_TRACK_HEIGHT
            );
        }
    }
}
