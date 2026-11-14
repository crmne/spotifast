//! The words of the playing track, in a side panel that follows the song.

use egui::{Align, Color32, Frame, Layout, Margin, Rect, Sense, UiBuilder, pos2, vec2};

use crate::app::{App, NowPlaying};
use crate::i18n::{gettext, pgettext};
use crate::model::{Action, Loadable, Page};
use crate::theme::{self, Icon};

use super::widgets;

const LINE_SIZE: f32 = 19.0;
const LINE_GAP: f32 = 10.0;
/// Where the line being sung sits, as a fraction of the visible lyrics from
/// the top: high up, so the lines to come fill most of the view.
const SUNG_LINE_AT: f32 = 0.2;

/// How much bigger the line being sung is in full screen.
const SUNG_LINE_GROWTH: f32 = 1.15;

/// Scrolls so the middle of `line` sits `SUNG_LINE_AT` of the way down the
/// visible lyrics.
fn show_sung_line(ui: &egui::Ui, line: Rect, animation: Option<egui::style::ScrollAnimation>) {
    let above = (ui.clip_rect().height() * SUNG_LINE_AT - line.height() / 2.0).max(0.0);
    let target = Rect::from_min_max(pos2(line.left(), line.top() - above), line.max);
    match animation {
        Some(animation) => ui.scroll_to_rect_animation(target, Some(Align::Min), animation),
        None => ui.scroll_to_rect(target, Some(Align::Min)),
    }
}

/// How long a line takes to light up or fade.
const LIGHT_UP_SECONDS: f32 = 0.22;

/// A music note's row, in multiples of the line's font size.
const MUSIC_NOTE_HEIGHT: f32 = 1.2;

/// A timed line with no words is the band playing on, which Spotify marks
/// with a music note. It is the icon rather than a "\u{266a}" in the text,
/// because the interface fonts may have no such glyph. Its size stays put
/// while it lights up, so the icon is drawn at one size only.
fn music_note(ui: &mut egui::Ui, size: f32, color: Color32, sense: Sense) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size * MUSIC_NOTE_HEIGHT), sense);
    theme::paint_icon(ui, Icon::Music, rect, size * 0.9, color);
    response
}

fn blend(from: egui::Color32, to: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    egui::Color32::from(egui::Rgba::from(from) * (1.0 - t) + egui::Rgba::from(to) * t)
}

pub fn side_panel(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let fit = super::yielding_panel(
        ui.ctx(),
        "lyrics-panel",
        theme::SIDE_PANEL_MIN_WIDTH..=640.0,
        app.settings.lyrics_width,
        ui.available_width() - super::topbar::least_width(ui.ctx()),
    );
    let panel = egui::Panel::right("lyrics-panel")
        .resizable(true)
        .default_size(app.settings.lyrics_width)
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
        ui.add_space(window_controls.lyrics_top);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            theme::text(
                ui,
                gettext(app.locale, "Lyrics"),
                theme::bold(18.0),
                palette.text,
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if theme::icon_button(
                    ui,
                    Icon::X,
                    18.0,
                    palette.secondary,
                    palette.text,
                    &gettext(app.locale, "Close"),
                )
                .clicked()
                {
                    app.actions.push(Action::ToggleLyricsPanel);
                }
                if theme::icon_button(
                    ui,
                    Icon::Expand,
                    18.0,
                    palette.secondary,
                    palette.text,
                    &gettext(app.locale, "Full screen lyrics"),
                )
                .clicked()
                {
                    app.actions.push(Action::SetLyricsFullscreen(true));
                }
                let loaded = matches!(&app.lyrics, Loadable::Loaded(Some(_)));
                if loaded
                    && !app.lyrics_following
                    && theme::pill_button(
                        ui,
                        &palette,
                        &pgettext(app.locale, "lyrics", "Follow"),
                        false,
                    )
                    .clicked()
                {
                    app.lyrics_following = true;
                    app.lyrics_line_shown = None;
                }
            });
        });
        ui.add_space(8.0);
        contents(app, ui);
    });
    let current_width = response.response.rect.width();
    if (app.settings.lyrics_width - current_width).abs() > 1.0
        && super::panel_width_chosen(ui.ctx(), "lyrics-panel", &fit)
    {
        app.settings.lyrics_width = current_width;
        app.actions.push(Action::SettingsChanged);
    }
}

fn contents(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let Some(now) = app.now_playing() else {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Mic,
            &gettext(app.locale, "Nothing playing"),
            &gettext(app.locale, "Play a song to see its lyrics."),
        );
        return;
    };
    let lyrics = match &app.lyrics {
        Loadable::NotLoaded | Loadable::Loading => {
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
                widgets::loading_row(ui, &palette, app.locale);
            }
            return;
        }
        Loadable::Failed(error) => {
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
                return;
            }
            let message = gettext(
                app.locale,
                // Translators: Keep {error} unchanged. It is the original failure detail.
                "Couldn't fetch the lyrics: {error}",
            )
            .replace("{error}", error);
            ui.add_space(8.0);
            theme::text(ui, message, theme::regular(13.0), palette.secondary);
            ui.add_space(8.0);
            if theme::pill_button(ui, &palette, &gettext(app.locale, "Try again"), false).clicked()
            {
                app.request_lyrics();
            }
            return;
        }
        Loadable::Loaded(None) => {
            widgets::empty_state(
                ui,
                &palette,
                Icon::Mic,
                &gettext(app.locale, "No lyrics"),
                &gettext(app.locale, "No lyrics found for this track."),
            );
            return;
        }
        Loadable::Loaded(Some(lyrics)) if lyrics.instrumental => {
            widgets::empty_state(
                ui,
                &palette,
                Icon::Music,
                &gettext(app.locale, "Instrumental"),
                &gettext(app.locale, "No timed lyrics for this track."),
            );
            return;
        }
        Loadable::Loaded(Some(lyrics)) => lyrics.clone(),
    };

    let active = lyrics.active_line(now.position_ms);
    let follow = app.lyrics_following && app.lyrics_line_shown != Some(active);
    // The line being sung is bold and in the accent colour; every other
    // line is quiet, regular text, the same before and after it has been
    // sung. A line takes 220 ms to light up or fade, as in omarchy-lyrics.
    let quiet = palette.text.gamma_multiply(0.45);
    let scroll = crate::autoscroll::show(
        ui,
        egui::ScrollArea::vertical()
            .id_salt("lyrics-scroll")
            .auto_shrink([false, false]),
        egui::Vec2b::new(false, true),
        |ui| {
            // Before the first line there is nothing to highlight, so the
            // panel sits at the top rather than wherever it was left.
            if follow && lyrics.synced && active.is_none() {
                let top = ui.cursor().min;
                ui.scroll_to_rect(
                    egui::Rect::from_min_size(top, egui::vec2(1.0, 1.0)),
                    Some(Align::Min),
                );
            }
            ui.add_space(12.0);
            for (index, line) in lyrics.lines.iter().enumerate() {
                let is_active = active == Some(index);
                let lit = ui.ctx().animate_bool_with_time(
                    egui::Id::new("lyric-line").with(index),
                    is_active,
                    crate::motion::seconds(ui.ctx(), LIGHT_UP_SECONDS),
                );
                let color = blend(quiet, palette.accent, lit);
                let font = if lit > 0.5 {
                    theme::bold(LINE_SIZE)
                } else {
                    theme::regular(LINE_SIZE)
                };
                let text = line.text.as_str();
                let sense = if lyrics.synced {
                    Sense::click()
                } else {
                    Sense::hover()
                };
                let response = if text.is_empty() && lyrics.synced {
                    music_note(ui, LINE_SIZE, color, sense)
                } else if crate::bidi::is_rtl(text) {
                    let galley = crate::bidi::layout(
                        ui.painter(),
                        text,
                        font,
                        color,
                        ui.available_width(),
                        usize::MAX,
                        None,
                    );
                    ui.add(egui::Label::new(galley).sense(sense))
                } else {
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).font(font).color(color))
                            .sense(sense),
                    )
                };
                crate::autoscroll::row(ui, &response);
                let rect = response.rect;
                if lyrics.synced
                    && response.clicked()
                    && let Some(at_ms) = line.at_ms
                {
                    app.actions.push(Action::Seek(at_ms));
                    app.lyrics_following = true;
                }
                if is_active && follow {
                    show_sung_line(ui, rect, None);
                }
                ui.add_space(LINE_GAP);
            }
            // Words without timing can only be followed by the clock: sit
            // at the part of the text the song is probably at.
            if app.lyrics_following && !lyrics.synced && now.duration_ms > 0 {
                let fraction =
                    (f64::from(now.position_ms) / f64::from(now.duration_ms)).clamp(0.0, 1.0);
                let content = ui.min_rect();
                let y = content.top() + content.height() * fraction as f32;
                ui.scroll_to_rect(
                    egui::Rect::from_min_max(
                        egui::pos2(content.left(), y),
                        egui::pos2(content.right(), y + 1.0),
                    ),
                    Some(Align::Center),
                );
            }
            // Room for the last line to rise to where a sung line sits.
            ui.add_space((ui.clip_rect().height() * (1.0 - SUNG_LINE_AT)).max(60.0));
        },
    );
    crate::autoscroll::lyrics(ui, scroll.id);
    // Scrolling by hand means the reader wants to look elsewhere; the
    // Follow button in the header picks the song back up.
    if ui.rect_contains_pointer(scroll.inner_rect)
        && ui.input(|input| input.smooth_scroll_delta.y != 0.0)
    {
        app.lyrics_following = false;
    }
    app.lyrics_line_shown = Some(active);
}

/// How long full screen lyrics keep their controls, the player bar and the
/// scroll bar, after the last input.
const CONTROLS_LINGER_SECONDS: f64 = 3.0;
/// How long the player bar takes to slide away or back.
const CONTROLS_SLIDE_SECONDS: f32 = 0.3;

fn controls_id() -> egui::Id {
    egui::Id::new("fullscreen-lyrics-last-input")
}

/// How much of full screen lyrics' controls shows: 1 while there has been
/// input in the last three seconds, the pointer rests on the player bar, or
/// a menu or popup is open; 0 once they have gone. In between they are
/// sliding or fading.
pub fn fullscreen_controls(app: &App, ctx: &egui::Context) -> f32 {
    controls_shown(ctx, egui::Popup::is_any_open(ctx) || app.show_devices)
}

/// [`fullscreen_controls`], where `open` says a menu or the device list
/// holds the controls.
fn controls_shown(ctx: &egui::Context, open: bool) -> f32 {
    let now = ctx.input(|input| input.time);
    let input = ctx.input(|input| {
        input.pointer.delta() != egui::Vec2::ZERO
            || input.pointer.any_down()
            || input.pointer.any_released()
            || input.smooth_scroll_delta != egui::Vec2::ZERO
            || input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Key { .. } | egui::Event::Text(_)))
    });
    let viewport = ctx.viewport_rect();
    let bar = Rect::from_min_max(
        pos2(
            viewport.left(),
            viewport.bottom() - theme::PLAYER_BAR_HEIGHT,
        ),
        viewport.max,
    );
    let over_bar = ctx
        .input(|input| input.pointer.hover_pos())
        .is_some_and(|pointer| bar.contains(pointer));
    // The last input and whether the controls showed on the last frame.
    let previous = ctx.data(|data| data.get_temp::<(f64, bool)>(controls_id()));
    // The pointer holds the bar only once it is there to hold: resting where
    // a hidden bar would be does not bring it back.
    let was_shown = previous.is_none_or(|(_, shown)| shown);
    let held = (was_shown && over_bar) || open;
    let last = match previous {
        Some((last, _)) if !input && !held => last,
        _ => now,
    };
    let left = CONTROLS_LINGER_SECONDS - (now - last);
    let shown = left > 0.0;
    ctx.data_mut(|data| data.insert_temp(controls_id(), (last, shown)));
    if shown {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
    }
    ctx.animate_bool_with_time(
        controls_id().with("shown"),
        shown,
        crate::motion::seconds(ctx, CONTROLS_SLIDE_SECONDS),
    )
}

/// Forget the last input once full screen lyrics close, so they open with
/// their controls showing.
pub fn forget_fullscreen_controls(ctx: &egui::Context) {
    ctx.data_mut(|data| data.remove::<(f64, bool)>(controls_id()));
}

/// Full screen lyrics over the whole window. The player bar comes and goes
/// over the bottom of it, so the room the layout uses stops above the bar
/// and never moves; `controls` is how much of the bar and the scroll bar
/// shows (see [`fullscreen_controls`]).
pub fn fullscreen(app: &mut App, ui: &mut egui::Ui, controls: f32) {
    egui::CentralPanel::default()
        .frame(Frame::new().fill(theme::Palette::dark().window))
        .show(ui, |ui| {
            // The whole window, edges included, is the view's own.
            let rect = ui.ctx().viewport_rect().union(ui.max_rect());
            background(app, ui, rect);
            let room = Rect::from_min_max(
                rect.min,
                pos2(rect.right(), rect.bottom() - theme::PLAYER_BAR_HEIGHT),
            );
            // The empty space is the backdrop's switch, as the player bar's
            // is its visualizer's. Everything drawn after this takes its own
            // clicks, and a drag is no click.
            #[cfg(feature = "milkdrop")]
            if ui
                .interact(room, ui.id().with("lyrics-backdrop"), Sense::click())
                .clicked()
            {
                app.actions.push(Action::ToggleLyricsMilkdrop);
            }
            let top = theme::titlebar_inset(ui.ctx()) + 24.0;
            if app.now_playing().is_some() && room.width() >= COVER_BESIDE_MIN_WIDTH {
                with_cover(app, ui, room, top, controls);
                return;
            }
            let width = fullscreen_content_width(room.width());
            let region = Rect::from_min_max(
                pos2(room.center().x - width / 2.0, room.top() + top),
                pos2(room.center().x + width / 2.0, room.bottom()),
            );
            let mut content = ui.new_child(UiBuilder::new().max_rect(region));
            fullscreen_header(app, &mut content, controls);
            content.add_space(20.0);
            track_heading(app, &mut content);
            content.add_space(16.0);
            fullscreen_contents(app, &mut content, room.right(), controls);
        });
}

/// The key of the large cover in full screen, for [`widgets::playing_cover`].
const COVER_KEY: &str = "fullscreen-lyrics";

/// Whether the large cover sits beside the lyrics: when there are `words`,
/// except that while the last song's cover `stands_in` for the new one's it
/// keeps the place it `held`.
fn cover_beside(words: bool, held: Option<bool>, stands_in: bool) -> bool {
    match held {
        Some(held) if stands_in => held,
        _ => words,
    }
}

/// The widest the lyrics get beside the cover, so lines stay easy to read:
/// at the largest size, about forty characters.
const LYRICS_BESIDE_WIDTH: f32 = 960.0;

/// The narrowest window that shows the cover beside the lyrics; narrower
/// ones keep a single column with a small cover in the heading.
const COVER_BESIDE_MIN_WIDTH: f32 = 900.0;

/// Full screen with the cover large: beside the lyrics when there are
/// words to follow, and alone in the middle when there are none, a calm
/// view of what is playing.
fn with_cover(app: &mut App, ui: &mut egui::Ui, rect: Rect, top: f32, controls: f32) {
    let outer = Rect::from_min_max(
        pos2(rect.left() + 48.0, rect.top() + top),
        pos2(rect.right() - 48.0, rect.bottom() - 40.0),
    );
    let mut header = ui.new_child(UiBuilder::new().max_rect(outer));
    fullscreen_header(app, &mut header, controls);
    let below = Rect::from_min_max(
        pos2(outer.left(), header.min_rect().bottom() + 24.0),
        outer.max,
    );
    // The cover moves aside only for words to read. While they load it
    // stays in the middle, so a song that turns out to have none never
    // moves at all.
    let words = matches!(&app.lyrics, Loadable::Loaded(Some(lyrics)) if !lyrics.instrumental);
    // On a change of song the last song's cover stands in until the new
    // one's is made. It keeps the last song's place meanwhile, so the cover
    // moves only once the new song's own is there.
    let url = app
        .now_playing()
        .and_then(|now| now.art_url.or(now.art_small));
    let stands_in =
        widgets::playing_cover_stands_in(ui, app.backend.art(), COVER_KEY, url.as_deref());
    let held_id = egui::Id::new("fullscreen-lyrics-cover-beside");
    let held = ui.ctx().data(|data| data.get_temp::<bool>(held_id));
    let words = cover_beside(words, held, stands_in);
    ui.ctx().data_mut(|data| data.insert_temp(held_id, words));
    // The cover keeps to the middle of what shows: as the player bar slides
    // away it glides down by half the bar, and back as the bar returns. The
    // lyrics keep their place.
    let glide = theme::PLAYER_BAR_HEIGHT * (1.0 - controls.clamp(0.0, 1.0)) / 2.0;
    // The queue docks right while its flag is on, in both arrangements.
    // Without it the room below is untouched, preserving the stock layout.
    let gap = 64.0;
    let queue_width = if app.show_queue_panel {
        // Shared with the side panel's persisted width; the column's drag
        // strip below writes the same field.
        app.settings.queue_width.clamp(
            super::queue::FULLSCREEN_COLUMN_MIN,
            super::queue::FULLSCREEN_COLUMN_MAX,
        )
    } else {
        0.0
    };
    let queue_gap = if app.show_queue_panel { gap } else { 0.0 };
    let lyrics_end = below.right() - queue_width - queue_gap;
    if app.show_queue_panel {
        let queue_rect = Rect::from_min_max(
            pos2(below.right() - queue_width, below.top()),
            pos2(below.right(), below.bottom()),
        );
        let mut queue_ui = ui.new_child(UiBuilder::new().max_rect(queue_rect));
        super::queue::fullscreen_column(app, &mut queue_ui, controls);
    }
    if words {
        if app.show_queue_panel {
            // The cover docks left; the lyrics take the middle that remains.
            let room = lyrics_end - below.left();
            let side = (room * 0.42).min(below.height() - 90.0).clamp(160.0, 520.0);
            let column = Rect::from_min_size(
                pos2(below.left(), below.center().y + glide - (side + 90.0) / 2.0),
                vec2(side, side + 90.0),
            );
            big_cover(app, ui, column, Align::Min, stands_in);
            let lyrics_avail = lyrics_end - column.right() - gap;
            let lyrics_width = lyrics_avail.clamp(80.0, LYRICS_BESIDE_WIDTH);
            // Centre the block between the cover and the queue when there is
            // room to spare; it hugs the cover on narrow windows.
            let lyrics_x = column.right() + gap + (lyrics_avail - lyrics_width).max(0.0) / 2.0;
            let lyrics = Rect::from_min_max(
                pos2(lyrics_x, below.top()),
                pos2(lyrics_x + lyrics_width, below.bottom()),
            );
            let mut content = ui.new_child(UiBuilder::new().max_rect(lyrics));
            fullscreen_contents(app, &mut content, lyrics_end, controls);
        } else {
            // The cover and the lyrics are one group, centred in the window.
            let side = (below.width() * 0.38)
                .min(below.height() - 90.0)
                .clamp(200.0, 520.0);
            let lyrics_width = (below.width() - side - gap).min(LYRICS_BESIDE_WIDTH);
            let left = below.center().x - (side + gap + lyrics_width) / 2.0;
            let column = Rect::from_min_size(
                pos2(left, below.center().y + glide - (side + 90.0) / 2.0),
                vec2(side, side + 90.0),
            );
            big_cover(app, ui, column, Align::Min, stands_in);
            let lyrics = Rect::from_min_max(
                pos2(column.right() + gap, below.top()),
                pos2(column.right() + gap + lyrics_width, below.bottom()),
            );
            let mut content = ui.new_child(UiBuilder::new().max_rect(lyrics));
            fullscreen_contents(app, &mut content, rect.right(), controls);
        }
    } else {
        let span = Rect::from_min_max(
            pos2(below.left(), below.top()),
            pos2(lyrics_end, below.bottom()),
        );
        // Without the queue the span is the whole room, matching the stock
        // centred cover exactly.
        let min_side = if app.show_queue_panel { 160.0 } else { 200.0 };
        let side = (span.height() - 140.0)
            .min(span.width() * 0.5)
            .clamp(min_side, 560.0);
        let column =
            Rect::from_center_size(span.center() + vec2(0.0, glide), vec2(side, side + 90.0));
        big_cover(app, ui, column, Align::Center, stands_in);
        // Why there are no words, quietly, under the song, or that they
        // are still being fetched. It fades with the player bar, leaving
        // the song alone; a failure keeps its Try again in view.
        let fade = if matches!(app.lyrics, Loadable::Failed(_)) {
            1.0
        } else {
            controls.clamp(0.0, 1.0)
        };
        let (heading, detail) = match &app.lyrics {
            Loadable::Loaded(Some(lyrics)) if lyrics.instrumental => (
                gettext(app.locale, "Instrumental"),
                gettext(app.locale, "No timed lyrics for this track."),
            ),
            Loadable::Loaded(None) => (
                gettext(app.locale, "No lyrics"),
                gettext(app.locale, "No lyrics found for this track."),
            ),
            Loadable::Failed(error) => (
                gettext(
                    app.locale,
                    // Translators: Keep {error} unchanged. It is the original failure detail.
                    "Couldn't fetch the lyrics: {error}",
                )
                .replace("{error}", error)
                .into(),
                Default::default(),
            ),
            // Words held back while the last song's cover stands in are
            // still on their way to the view.
            Loadable::NotLoaded | Loadable::Loading | Loadable::Loaded(Some(_)) => {
                (gettext(app.locale, "Loading…"), Default::default())
            }
        };
        let heading = ui.painter().text(
            pos2(column.center().x, column.bottom() + 8.0),
            egui::Align2::CENTER_TOP,
            heading,
            theme::semibold(13.0),
            Color32::from_gray(200).gamma_multiply(fade),
        );
        if matches!(app.lyrics, Loadable::Failed(_)) {
            let retry = Rect::from_center_size(
                pos2(column.center().x, heading.bottom() + 24.0),
                vec2(column.width(), 32.0),
            );
            let mut retry_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(retry)
                    .layout(Layout::top_down(Align::Center)),
            );
            let label = gettext(app.locale, "Try again");
            if theme::pill_button(&mut retry_ui, &theme::Palette::dark(), &label, false).clicked() {
                app.actions.push(Action::RetryLyrics);
            }
            return;
        }
        ui.painter().text(
            pos2(column.center().x, heading.bottom() + 4.0),
            egui::Align2::CENTER_TOP,
            detail,
            theme::regular(13.0),
            Color32::from_gray(170).gamma_multiply(fade),
        );
    }
}

/// The playing song's cover filling the top of `column`, with its title and
/// artists beneath, aligned to its left edge or centred. `stands_in` says
/// `column` was placed for the last song's cover standing in for this one's.
fn big_cover(app: &mut App, ui: &mut egui::Ui, column: Rect, align: Align, stands_in: bool) {
    let Some(now) = app.now_playing() else {
        return;
    };
    let side = column.width();
    let cover = Rect::from_min_size(column.min, vec2(side, side));
    let radius = 10.0;
    ui.painter().add(
        egui::epaint::Shadow {
            offset: [0, 18],
            blur: 48,
            spread: 0,
            color: Color32::from_black_alpha(140),
        }
        .as_shape(cover, radius),
    );
    let url = now.art_url.as_deref().or(now.art_small.as_deref());
    widgets::playing_cover(
        ui,
        &theme::Palette::dark(),
        app.backend.art(),
        COVER_KEY,
        url,
        cover,
        radius,
    );
    widgets::settle_playing_cover(ui, app.backend.art(), url, stands_in);
    let art = now.art_url.is_some() || now.art_small.is_some();
    cover_opens_immersive(app, ui, cover, art);
    let words = Rect::from_min_max(pos2(column.left(), cover.bottom() + 18.0), column.max);
    let mut text = ui.new_child(
        UiBuilder::new()
            .max_rect(words)
            .layout(Layout::top_down(align)),
    );
    text.spacing_mut().item_spacing.y = 4.0;
    song_links(
        app,
        &mut text,
        &now,
        theme::semibold(22.0),
        theme::regular(14.0),
        Color32::from_gray(225),
    );
}

/// The playing song's title and artists, as the player bar has them: the
/// title opens its album (or the show), each artist their page. Opening a
/// page leaves full screen.
fn song_links(
    app: &mut App,
    ui: &mut egui::Ui,
    now: &NowPlaying,
    title_font: egui::FontId,
    artist_font: egui::FontId,
    artist_colour: Color32,
) {
    let opens = now
        .album_id
        .clone()
        .map(Page::Album)
        .or_else(|| now.show_id.clone().map(Page::Show));
    match opens {
        Some(page) => {
            if theme::link(ui, &now.title, title_font, Color32::WHITE).clicked() {
                app.actions.push(Action::Open(page));
            }
        }
        None => {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&now.title)
                        .font(title_font)
                        .color(Color32::WHITE),
                )
                .truncate(),
            );
        }
    }
    // The row is only as wide as its words, so a centred column centres it.
    let words = if now.artists.is_empty() {
        now.subtitle.clone()
    } else {
        now.artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let size = ui
        .painter()
        .layout_no_wrap(words, artist_font.clone(), artist_colour)
        .size();
    ui.allocate_ui_with_layout(
        vec2((size.x + 1.0).min(ui.available_width()), size.y),
        Layout::left_to_right(Align::Center),
        |ui| {
            if !now.artists.is_empty() {
                widgets::artist_links(ui, app, &now.artists, artist_font, artist_colour);
            } else if let Some(id) = &now.show_id {
                if theme::link(ui, &now.subtitle, artist_font, artist_colour).clicked() {
                    app.actions.push(Action::Open(Page::Show(id.clone())));
                }
            } else {
                theme::text(ui, &now.subtitle, artist_font, artist_colour);
            }
        },
    );
}

fn fullscreen_content_width(viewport_width: f32) -> f32 {
    let available = (viewport_width - 48.0).max(0.0);
    (viewport_width * 0.72).clamp(400.0, 960.0).min(available)
}

fn preferred_backdrop_art(small: Option<String>, large: Option<String>) -> Option<String> {
    small.or(large)
}

/// How bright the brightest of the picture may stay behind the words, so a
/// white preset reads like the dimmed art does.
const BACKDROP_GLOW: f32 = 0.2;
/// The dimming's limits: even a dark preset's sparks stay this far under the
/// white of the words, and a bright one is never lost altogether.
const BACKDROP_DIM: std::ops::RangeInclusive<f32> = 0.2..=0.32;

/// How much of the picture's brightness to keep, for one this bright.
fn backdrop_dim(glow: f32) -> f32 {
    (BACKDROP_GLOW / glow.max(0.01)).clamp(*BACKDROP_DIM.start(), *BACKDROP_DIM.end())
}

/// MilkDrop filling `rect`, dimmed so the words stay clear whatever it draws.
/// `false` leaves the art: there is no picture yet, or none to be had.
#[cfg(feature = "milkdrop")]
fn milkdrop_backdrop(app: &mut App, ui: &mut egui::Ui, rect: Rect) -> bool {
    use crate::milkdrop::pane::{Picture, UV};
    if !app.milkdrop_behind_lyrics() {
        return false;
    }
    let ctx = ui.ctx().clone();
    let pixels = rect.size() * ctx.pixels_per_point();
    let picture = app.milkdrop_picture(&ctx, [pixels.x, pixels.y]);
    let Picture::Frame(texture) = picture else {
        if picture == Picture::Waiting {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            // No MilkDrop to be had: as if the art was never clicked.
            app.actions.push(Action::LeaveImmersive);
        }
        return false;
    };
    app.immersive_arrived();
    let dim = ctx.animate_value_with_time(
        egui::Id::new("lyrics-backdrop-dim"),
        backdrop_dim(app.milkdrop_glow()),
        crate::motion::seconds(&ctx, 0.5),
    );
    let tint = Color32::from_gray((dim * 255.0).round() as u8);
    ui.painter()
        .with_clip_rect(rect)
        .image(texture, rect, UV, tint);
    if app.now_playing().is_some_and(|now| now.playing) {
        theme::visualiser_frame(&ctx, Some(app.milkdrop_pane_fps()));
    }
    true
}

/// MilkDrop alone, edge to edge and at full brightness, under the root of the
/// window so that the player bar comes and goes over it without resizing it.
/// Returns whether the controls show.
#[cfg(feature = "milkdrop")]
pub fn immersive(app: &mut App, ui: &mut egui::Ui) -> bool {
    use crate::milkdrop::pane::{Picture, UV};
    let ctx = ui.ctx().clone();
    let rect = ctx.viewport_rect();
    ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
    let pixels = rect.size() * ctx.pixels_per_point();
    match app.milkdrop_picture(&ctx, [pixels.x, pixels.y]) {
        Picture::Frame(texture) => {
            ui.painter().image(texture, rect, UV, Color32::WHITE);
        }
        Picture::Waiting => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
        Picture::None => app.actions.push(Action::LeaveImmersive),
    }
    if app.now_playing().is_some_and(|now| now.playing) {
        theme::visualiser_frame(&ctx, Some(app.milkdrop_pane_fps()));
    }
    let moved = ctx.input(|input| input.pointer.delta() != egui::Vec2::ZERO);
    let now = ctx.input(|input| input.time);
    let left = app.immersive_controls(moved, now);
    match left {
        Some(left) => ctx.request_repaint_after(std::time::Duration::from_secs_f64(left)),
        None => ctx.set_cursor_icon(egui::CursorIcon::None),
    }
    left.is_some()
}

/// Everything in immersive mode that is not a control: any click there brings
/// the view back.
#[cfg(feature = "milkdrop")]
pub fn immersive_click(app: &mut App, ui: &mut egui::Ui) {
    egui::CentralPanel::default()
        .frame(Frame::new())
        .show(ui, |ui| {
            let rect = ui.max_rect();
            if ui
                .interact(rect, ui.id().with("immersive"), Sense::click())
                .clicked()
            {
                app.actions.push(Action::LeaveImmersive);
            }
        });
}

/// The cover is the way into immersive mode, if there is MilkDrop to go to.
fn cover_opens_immersive(app: &mut App, ui: &egui::Ui, cover: Rect, art: bool) {
    #[cfg(feature = "milkdrop")]
    if art
        && ui
            .interact(cover, ui.id().with("lyrics-cover"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
    {
        app.actions.push(Action::EnterImmersive);
    }
    #[cfg(not(feature = "milkdrop"))]
    let _ = (app, ui, cover, art);
}

fn background(app: &mut App, ui: &mut egui::Ui, rect: Rect) {
    theme::apply_local(ui, &theme::Palette::dark());
    #[cfg(feature = "milkdrop")]
    if milkdrop_backdrop(app, ui, rect) {
        return;
    }
    let art = app
        .now_playing()
        .and_then(|now| preferred_backdrop_art(now.art_small, now.art_url));
    let painter = ui.painter().with_clip_rect(rect);
    if let Some(texture) = app
        .lyrics_backdrop
        .texture(ui.ctx(), app.backend.art(), art.as_deref())
    {
        painter.image(
            texture.id(),
            rect,
            cover_uv(rect.size(), texture.size_vec2()),
            Color32::from_gray(180),
        );
    }
    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(120));
    widgets::paint_vertical_gradient(
        ui,
        rect,
        Color32::from_black_alpha(0),
        Color32::from_black_alpha(95),
    );
}

fn cover_uv(view: egui::Vec2, image: egui::Vec2) -> Rect {
    let ratio = (view.x / view.y.max(1.0)) / (image.x / image.y.max(1.0));
    let size = if ratio > 1.0 {
        vec2(1.0, 1.0 / ratio)
    } else {
        vec2(ratio, 1.0)
    };
    Rect::from_center_size(pos2(0.5, 0.5), size)
}

/// The view's title and its buttons. The title and the button that leaves
/// full screen fade with the player bar (`controls`, see
/// [`fullscreen_controls`]), so only the words stay once the controls have
/// gone.
fn fullscreen_header(app: &mut App, ui: &mut egui::Ui, controls: f32) {
    let palette = theme::Palette::dark();
    let faded = palette.text.gamma_multiply(controls.clamp(0.0, 1.0));
    ui.horizontal(|ui| {
        theme::text(ui, gettext(app.locale, "Lyrics"), theme::bold(18.0), faded);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::icon_button(
                ui,
                Icon::Shrink,
                18.0,
                faded,
                faded,
                &gettext(app.locale, "Leave full screen (Esc)"),
            )
            .clicked()
            {
                app.actions.push(Action::SetLyricsFullscreen(false));
            }
            let loaded = matches!(&app.lyrics, Loadable::Loaded(Some(_)));
            if loaded
                && !app.lyrics_following
                && theme::pill_button(
                    ui,
                    &palette,
                    &pgettext(app.locale, "lyrics", "Follow"),
                    false,
                )
                .clicked()
            {
                app.actions.push(Action::FollowLyrics);
            }
        });
    });
}

fn track_heading(app: &mut App, ui: &mut egui::Ui) {
    if let Some(now) = app.now_playing() {
        ui.horizontal(|ui| {
            let size = 52.0;
            let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
            widgets::playing_cover(
                ui,
                &theme::Palette::dark(),
                app.backend.art(),
                "lyrics-heading",
                app.small_art(&now, size),
                rect,
                4.0,
            );
            let art = now.art_url.is_some() || now.art_small.is_some();
            cover_opens_immersive(app, ui, rect, art);
            ui.vertical(|ui| {
                song_links(
                    app,
                    ui,
                    &now,
                    theme::semibold(22.0),
                    theme::regular(13.0),
                    Color32::from_gray(235),
                );
            });
        });
    }
}

/// The words in `ui`'s column. Their scroll area runs on to `scroll_right`,
/// so its bar sits at the window's edge, or the queue's, rather than in the
/// space beside the words; it shows as much as the player bar does.
fn fullscreen_contents(app: &mut App, ui: &mut egui::Ui, scroll_right: f32, controls: f32) {
    let palette = theme::Palette::dark();
    let Some(now) = app.now_playing() else {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Mic,
            &gettext(app.locale, "Nothing playing"),
            &gettext(app.locale, "Play a song to see its lyrics."),
        );
        return;
    };
    let lyrics = match &app.lyrics {
        Loadable::NotLoaded | Loadable::Loading => {
            widgets::loading_row(ui, &palette, app.locale);
            return;
        }
        Loadable::Failed(error) => {
            let message = gettext(
                app.locale,
                // Translators: Keep {error} unchanged. It is the original failure detail.
                "Couldn't fetch the lyrics: {error}",
            )
            .replace("{error}", error);
            ui.add_space(8.0);
            theme::text(ui, message, theme::regular(13.0), palette.text);
            ui.add_space(8.0);
            if theme::pill_button(ui, &palette, &gettext(app.locale, "Try again"), false).clicked()
            {
                app.actions.push(Action::RetryLyrics);
            }
            return;
        }
        // Saying there are no words fades with the player bar, as the
        // cover's note does.
        Loadable::Loaded(None) => {
            ui.multiply_opacity(controls.clamp(0.0, 1.0));
            widgets::empty_state(
                ui,
                &palette,
                Icon::Mic,
                &gettext(app.locale, "No lyrics"),
                &gettext(app.locale, "No lyrics found for this track."),
            );
            return;
        }
        Loadable::Loaded(Some(lyrics)) if lyrics.instrumental => {
            ui.multiply_opacity(controls.clamp(0.0, 1.0));
            widgets::empty_state(
                ui,
                &palette,
                Icon::Music,
                &gettext(app.locale, "Instrumental"),
                &gettext(app.locale, "No timed lyrics for this track."),
            );
            return;
        }
        Loadable::Loaded(Some(lyrics)) => lyrics.clone(),
    };

    let active = lyrics.active_line(now.position_ms);
    // A Follow click resets the remembered line after drawing. Other frames
    // record the shown line before any line-click action restores following.
    if !app
        .actions
        .iter()
        .any(|action| matches!(action, Action::FollowLyrics))
    {
        app.actions.push(Action::LyricsLineShown(active));
    }
    let column = ui.available_width();
    let column_rect = ui.available_rect_before_wrap();
    let viewport = Rect::from_min_max(
        column_rect.min,
        pos2(scroll_right.max(column_rect.right()), column_rect.bottom()),
    );
    let mut ui = ui.new_child(UiBuilder::new().max_rect(viewport));
    let ui = &mut ui;
    let manual_scroll = ui.rect_contains_pointer(viewport)
        && ui.input(|input| {
            input.smooth_scroll_delta.y != 0.0
                || (input.pointer.primary_down() && input.pointer.delta().y != 0.0)
        });
    let following = app.lyrics_following && !manual_scroll;
    let follow = following && app.lyrics_line_shown != Some(active);
    let animation = crate::motion::scroll(ui.ctx(), 0.45);
    let size = (column * 0.046).clamp(28.0, 44.0);
    // The line being sung takes the theme's accent, though the view is
    // always dark, and grows; each line wraps
    // for its full size all along, so growing can't rewrap its words.
    // A line takes 300 ms to light up or fade.
    // A moving picture asks more of the quiet lines than a still one.
    let quiet = palette
        .text
        .gamma_multiply(if app.milkdrop_behind_lyrics() {
            0.62
        } else {
            0.42
        });
    let scroll = &mut ui.spacing_mut().scroll;
    scroll.fade.strength = 0.0;
    // The bar shows with the player bar, still lighting up under the pointer.
    scroll.dormant_handle_opacity = scroll.active_handle_opacity * controls;
    scroll.active_handle_opacity *= controls;
    scroll.interact_handle_opacity *= controls;
    egui::ScrollArea::vertical()
        .id_salt(("fullscreen-lyrics-scroll", &now.uri))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Before the first line there is nothing to highlight, so the
            // panel sits at the top rather than wherever it was left.
            if follow && lyrics.synced && active.is_none() {
                let top = ui.cursor().min;
                ui.scroll_to_rect_animation(
                    egui::Rect::from_min_size(top, egui::vec2(1.0, 1.0)),
                    Some(Align::Min),
                    animation,
                );
            }
            // Room for the first line to sit where a sung line sits, and for
            // the last to rise to it.
            let (padding, below) = if lyrics.synced {
                (
                    (viewport.height() * SUNG_LINE_AT - size).max(12.0),
                    viewport.height() * (1.0 - SUNG_LINE_AT),
                )
            } else {
                (12.0, 60.0)
            };
            ui.add_space(padding);
            for (index, line) in lyrics.lines.iter().enumerate() {
                let is_active = active == Some(index);
                let lit = ui.ctx().animate_bool_with_time(
                    egui::Id::new("lyric-line").with(("fullscreen", &now.uri, index)),
                    is_active,
                    crate::motion::seconds(ui.ctx(), 0.3),
                );
                let color = if lyrics.synced {
                    blend(quiet, app.palette.accent, lit)
                } else {
                    palette.text
                };
                let grown = if lyrics.synced {
                    1.0 + (SUNG_LINE_GROWTH - 1.0) * lit
                } else {
                    1.0
                };
                let wrap = column / SUNG_LINE_GROWTH * grown;
                let font = theme::bold(size * grown);
                // Untimed words have nowhere to jump to, but a click on them
                // is still on a line, so it must not reach the backdrop.
                let sense = Sense::click();
                let note = line.text.is_empty() && lyrics.synced;
                let galley = (!note).then(|| {
                    crate::bidi::layout(
                        ui.painter(),
                        &line.text,
                        font,
                        color,
                        wrap,
                        usize::MAX,
                        None,
                    )
                });
                let height = galley
                    .as_ref()
                    .map_or(size * MUSIC_NOTE_HEIGHT, |galley| galley.size().y);
                let line_rect =
                    Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), height));
                // The line being sung stays whole: in a short window it can
                // sit nearer the top than the fade reaches.
                let edge = widgets::edge_opacity(line_rect, viewport, size * 2.0);
                let opacity = edge.max(lit);
                let response = ui
                    .scope(|ui| {
                        ui.multiply_opacity(opacity);
                        match galley {
                            Some(galley) => ui.add(egui::Label::new(galley).sense(sense)),
                            None => music_note(ui, size, color, sense),
                        }
                    })
                    .inner;
                let rect = response.rect;
                if lyrics.synced
                    && response.clicked()
                    && let Some(at_ms) = line.at_ms
                {
                    app.actions.push(Action::Seek(at_ms));
                    app.actions.push(Action::FollowLyrics);
                }
                if is_active && follow {
                    show_sung_line(ui, rect, Some(animation));
                }
                ui.add_space(27.0);
            }
            // Words without timing can only be followed by the clock: sit
            // at the part of the text the song is probably at.
            if following && !lyrics.synced && now.duration_ms > 0 {
                let fraction =
                    (f64::from(now.position_ms) / f64::from(now.duration_ms)).clamp(0.0, 1.0);
                let content = ui.min_rect();
                let y = content.top() + content.height() * fraction as f32;
                ui.scroll_to_rect_animation(
                    egui::Rect::from_min_max(
                        egui::pos2(content.left(), y),
                        egui::pos2(content.right(), y + 1.0),
                    ),
                    Some(Align::Center),
                    animation,
                );
            }
            ui.add_space(below.max(60.0));
        });
    // Scrolling by hand means the reader wants to look elsewhere; the
    // Follow button in the header picks the song back up.
    if manual_scroll && app.lyrics_following {
        app.actions.push(Action::PauseLyricsFollow);
    }
    if now.playing
        && lyrics.synced
        && let Some(next) = lyrics
            .lines
            .iter()
            .filter_map(|line| line.at_ms)
            .find(|at| *at > now.position_ms)
    {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(u64::from(
                next - now.position_ms,
            )));
    }
}

#[cfg(test)]
mod tests {
    use super::{fullscreen_content_width, preferred_backdrop_art, widgets::edge_opacity};
    use egui::{Rect, pos2};

    /// A sentence wrapped over two lines is gone by the time its top line
    /// reaches the top of the view, so the clip never cuts it in half.
    #[test]
    fn a_line_is_gone_before_the_view_cuts_it() {
        let view = Rect::from_min_max(pos2(0.0, 100.0), pos2(600.0, 700.0));
        let line = |top: f32| Rect::from_min_size(pos2(0.0, top), egui::vec2(600.0, 110.0));
        assert_eq!(edge_opacity(line(100.0), view, 80.0), 0.0, "at the top");
        assert_eq!(edge_opacity(line(590.0), view, 80.0), 0.0, "at the bottom");
        assert!(edge_opacity(line(140.0), view, 80.0) < 1.0, "fading");
        assert_eq!(edge_opacity(line(300.0), view, 80.0), 1.0, "in the middle");
    }

    /// A line wrapped over so many rows that it cannot clear the fade at
    /// both ends still shows whole once it is in the middle of the view.
    #[test]
    fn a_line_too_tall_for_the_fades_still_shows_whole() {
        let view = Rect::from_min_max(pos2(0.0, 100.0), pos2(600.0, 700.0));
        let tall = |top: f32| Rect::from_min_size(pos2(0.0, top), egui::vec2(600.0, 500.0));
        assert_eq!(edge_opacity(tall(150.0), view, 80.0), 1.0, "in the middle");
        assert_eq!(edge_opacity(tall(100.0), view, 80.0), 0.0, "at the top");
        let taller = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(600.0, 900.0));
        assert_eq!(edge_opacity(taller, view, 80.0), 1.0, "never gone");
    }

    #[test]
    fn fullscreen_backdrop_prefers_small_art_with_large_art_as_fallback() {
        let small = "small".to_string();
        let large = "large".to_string();
        assert_eq!(
            preferred_backdrop_art(Some(small.clone()), Some(large.clone())),
            Some(small)
        );
        assert_eq!(
            preferred_backdrop_art(None, Some(large.clone())),
            Some(large)
        );
        assert_eq!(preferred_backdrop_art(None, None), None);
    }

    #[test]
    fn fullscreen_content_width_never_inverts_a_narrow_viewport() {
        for viewport_width in [0.0, 24.0, 47.0, 48.0, 64.0, 760.0, 2_000.0] {
            let width = fullscreen_content_width(viewport_width);
            assert!(width >= 0.0);
            assert!(width <= (viewport_width - 48.0).max(0.0));
        }
        assert_eq!(fullscreen_content_width(47.0), 0.0);
        assert_eq!(fullscreen_content_width(2_000.0), 960.0);
    }

    /// A white picture ends up as dark as the dimmed art, and full white
    /// words on it above WCAG's 7:1.
    #[test]
    fn white_is_dimmed_until_the_words_read_on_it() {
        let dim = super::backdrop_dim(1.0);
        let linear = |value: f32| ((value + 0.055) / 1.055).powf(2.4);
        let contrast = 1.05 / (linear(dim) + 0.05);
        assert!(contrast >= 7.0, "{contrast}");
    }

    #[test]
    fn a_dark_picture_is_dimmed_less_but_never_left_alone() {
        assert!(super::backdrop_dim(0.1) > super::backdrop_dim(1.0));
        assert!(super::backdrop_dim(0.0) <= 0.32);
    }

    /// A change of song moves the large cover together with the new song's
    /// own cover, never the last song's while it stands in.
    #[test]
    fn the_last_songs_cover_keeps_its_place_while_it_stands_in() {
        use super::cover_beside;

        // #given a song with words, then one without
        assert!(cover_beside(false, Some(true), true), "last cover stays");
        assert!(!cover_beside(false, Some(true), false), "new one centres");
        // #given a song without words, then one with
        assert!(!cover_beside(true, Some(false), true), "last cover stays");
        assert!(cover_beside(true, Some(false), false), "new one moves");
        // #then the first frame follows the words
        assert!(cover_beside(true, None, true));
        assert!(!cover_beside(false, None, true));
    }

    /// One frame at `time` of a window 800 points wide and `height` high,
    /// answering how much of the full screen controls shows.
    fn controls_at(
        ctx: &egui::Context,
        time: f64,
        height: f32,
        events: Vec<egui::Event>,
        open: bool,
    ) -> f32 {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                egui::vec2(800.0, height),
            )),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut shown = f32::NAN;
        let mut output = ctx.run_ui(input, |ui| shown = super::controls_shown(ui.ctx(), open));
        output.textures_delta.clear();
        shown
    }

    #[test]
    fn the_player_bar_goes_after_three_quiet_seconds_and_comes_back_with_input() {
        let ctx = egui::Context::default();
        let at = |time, events, open| controls_at(&ctx, time, 600.0, events, open);
        let key = || vec![egui::Event::Text("a".into())];
        // #given full screen lyrics just opened
        assert_eq!(at(0.0, Vec::new(), false), 1.0);
        assert_eq!(at(2.9, Vec::new(), false), 1.0);
        // #when nothing happens for three seconds #then the bar slides away
        at(3.1, Vec::new(), false);
        assert_eq!(at(3.6, Vec::new(), false), 0.0);
        // #when a key is pressed #then it slides back for three seconds
        at(4.0, key(), false);
        assert_eq!(at(4.5, Vec::new(), false), 1.0);
        assert_eq!(at(6.9, Vec::new(), false), 1.0);
        // #when a menu stays open #then the bar stays with it
        assert_eq!(at(8.0, Vec::new(), true), 1.0);
        assert_eq!(at(10.9, Vec::new(), false), 1.0);
        at(11.1, Vec::new(), false);
        assert_eq!(at(11.6, Vec::new(), false), 0.0);
    }

    #[test]
    fn the_pointer_holds_the_bar_only_where_it_shows() {
        let ctx = egui::Context::default();
        let at = |time, height, events| controls_at(&ctx, time, height, events, false);
        let moved = |to| vec![egui::Event::PointerMoved(to)];
        let middle = pos2(400.0, 300.0);
        // #given the pointer comes to rest on the bar while it shows
        at(0.0, 600.0, moved(middle));
        at(0.1, 600.0, moved(pos2(400.0, 580.0)));
        // #then the bar stays, also on the frame its three seconds run out
        assert_eq!(at(3.1, 600.0, Vec::new()), 1.0);
        assert_eq!(at(9.0, 600.0, Vec::new()), 1.0);
        // #given the pointer leaves and the bar slides away
        at(9.1, 600.0, moved(middle));
        at(12.2, 600.0, Vec::new());
        assert_eq!(at(12.7, 600.0, Vec::new()), 0.0);
        // #when the window shrinks until the bar's place is under the pointer
        // #then the bar stays away
        assert_eq!(at(13.0, 320.0, Vec::new()), 0.0);
        assert_eq!(at(20.0, 320.0, Vec::new()), 0.0);
    }
}
