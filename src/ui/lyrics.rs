//! The words of the playing track, in a side panel that follows the song.

use egui::{Align, Color32, Frame, Layout, Margin, Rect, Sense, UiBuilder, pos2, vec2};

use crate::api::models::PlayableItem;
use crate::app::App;
use crate::i18n::{gettext, pgettext};
use crate::model::{Action, FullscreenContent, FullscreenDrawer, Loadable, Page, RowContext};
use crate::theme::{self, Icon};

use super::widgets;

const LINE_SIZE: f32 = 19.0;
const LINE_GAP: f32 = 10.0;
/// Where the line being sung sits, as a fraction of the visible lyrics from
/// the top: high up, so the lines to come fill most of the view.
const SUNG_LINE_AT: f32 = 0.2;

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
                // A timed line with no words is the band playing on.
                let text = if line.text.is_empty() && lyrics.synced {
                    "\u{266a}"
                } else {
                    line.text.as_str()
                };
                let sense = if lyrics.synced {
                    Sense::click()
                } else {
                    Sense::hover()
                };
                let response = if crate::bidi::is_rtl(text) {
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

const DRAWER_WIDTH: f32 = 380.0;

/// Draw the full-screen view, showing either Now Playing artwork or lyrics
/// along with any active slide-out context drawer.
pub fn fullscreen(app: &mut App, ui: &mut egui::Ui) {
    egui::CentralPanel::default()
        .frame(Frame::new().fill(theme::Palette::dark().window))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            background(app, ui, rect);

            let open_t = ui.ctx().animate_value_with_time(
                egui::Id::new("fullscreen_drawer_slide"),
                if app.fullscreen_drawer.is_some() {
                    1.0
                } else {
                    0.0
                },
                0.20,
            );

            let content_rect = Rect::from_min_max(
                rect.min,
                pos2(rect.right() - DRAWER_WIDTH * open_t, rect.bottom()),
            );
            let top = theme::titlebar_inset(ui.ctx()) + 24.0;
            let outer = Rect::from_min_max(
                pos2(content_rect.left() + 48.0, content_rect.top() + top),
                pos2(content_rect.right() - 48.0, content_rect.bottom() - 24.0),
            );

            let mut header_ui = ui.new_child(UiBuilder::new().max_rect(outer));
            fullscreen_header(app, &mut header_ui);

            let below = Rect::from_min_max(
                pos2(outer.left(), header_ui.min_rect().bottom() + 20.0),
                outer.max,
            );

            let content = match app.fullscreen_content {
                FullscreenContent::NowPlaying if !app.show_lyrics_panel => {
                    FullscreenContent::NowPlaying
                }
                _ => FullscreenContent::Lyrics,
            };

            match content {
                FullscreenContent::NowPlaying => {
                    now_playing_fullscreen(app, ui, below);
                }
                FullscreenContent::Lyrics => {
                    if app.now_playing().is_some() && below.width() >= COVER_BESIDE_MIN_WIDTH {
                        with_cover(app, ui, below);
                    } else {
                        let width = fullscreen_content_width(content_rect.width());
                        let region = Rect::from_min_max(
                            pos2(below.center().x - width / 2.0, below.top()),
                            pos2(below.center().x + width / 2.0, below.bottom()),
                        );
                        let mut content = ui.new_child(UiBuilder::new().max_rect(region));
                        track_heading(app, &mut content);
                        content.add_space(16.0);
                        fullscreen_contents(app, &mut content);
                    }
                }
            }

            if open_t > 0.0 {
                let slide_offset = DRAWER_WIDTH * (1.0 - open_t);
                let drawer_rect = Rect::from_min_size(
                    pos2(rect.right() - DRAWER_WIDTH + slide_offset, rect.top()),
                    vec2(DRAWER_WIDTH, rect.height()),
                );
                let mut drawer_ui = ui.new_child(UiBuilder::new().max_rect(rect));
                drawer_ui.set_clip_rect(rect.intersect(ui.clip_rect()));
                fullscreen_drawer(app, &mut drawer_ui, drawer_rect);
            }
        });
}

/// The widest the lyrics get beside the cover, so lines stay easy to read.
const LYRICS_BESIDE_WIDTH: f32 = 640.0;

/// The narrowest window that shows the cover beside the lyrics; narrower
/// ones keep a single column with a small cover in the heading.
const COVER_BESIDE_MIN_WIDTH: f32 = 900.0;

/// Full screen with the cover large: beside the lyrics when there are
/// words to follow, and alone in the middle when there are none, a calm
/// view of what is playing.
fn with_cover(app: &mut App, ui: &mut egui::Ui, below: Rect) {
    // The cover moves aside only for words to read. While they load it
    // stays in the middle, so a song that turns out to have none never
    // moves at all.
    let words = matches!(&app.lyrics, Loadable::Loaded(Some(lyrics)) if !lyrics.instrumental);
    if words {
        // The cover and the lyrics are one group, centred in the window.
        let gap = 64.0;
        let side = (below.width() * 0.38)
            .min(below.height() - 90.0)
            .clamp(200.0, 520.0);
        let lyrics_width = (below.width() - side - gap).min(LYRICS_BESIDE_WIDTH);
        let left = below.center().x - (side + gap + lyrics_width) / 2.0;
        let column = Rect::from_min_size(
            pos2(left, below.center().y - (side + 90.0) / 2.0),
            vec2(side, side + 90.0),
        );
        big_cover(app, ui, column, Align::Min);
        let lyrics = Rect::from_min_max(
            pos2(column.right() + gap, below.top()),
            pos2(column.right() + gap + lyrics_width, below.bottom()),
        );
        let mut content = ui.new_child(UiBuilder::new().max_rect(lyrics));
        fullscreen_contents(app, &mut content);
    } else {
        let side = (below.height() - 140.0)
            .min(below.width() * 0.5)
            .clamp(200.0, 560.0);
        let column = Rect::from_center_size(below.center(), vec2(side, side + 90.0));
        big_cover(app, ui, column, Align::Center);
        // Why there are no words, quietly, under the song, or that they
        // are still being fetched.
        let (heading, detail) = match &app.lyrics {
            Loadable::Loaded(Some(_)) => (
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
            Loadable::NotLoaded | Loadable::Loading => {
                (gettext(app.locale, "Loading…"), Default::default())
            }
        };
        let heading = ui.painter().text(
            pos2(column.center().x, column.bottom() + 8.0),
            egui::Align2::CENTER_TOP,
            heading,
            theme::semibold(13.0),
            Color32::from_gray(200),
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
            Color32::from_gray(170),
        );
    }
}

/// The cover large, with subtle drop shadow, its rounded corners, and the song's title and
/// artists beneath, aligned to its left edge or centred.
/// Clicking the song title toggles the album in the drawer.
fn big_cover(app: &mut App, ui: &mut egui::Ui, column: Rect, align: Align) {
    let Some(now) = app.now_playing() else {
        return;
    };
    let side = column.width();
    let cover = Rect::from_min_size(column.min, vec2(side, side));
    let radius = 12.0;
    ui.painter().add(
        egui::epaint::Shadow {
            offset: [0, 18],
            blur: 48,
            spread: 0,
            color: Color32::from_black_alpha(140),
        }
        .as_shape(cover, radius),
    );
    widgets::playing_cover(
        ui,
        &theme::Palette::dark(),
        app.backend.art(),
        "fullscreen-lyrics",
        now.art_url.as_deref().or(now.art_small.as_deref()),
        cover,
        radius,
    );
    let words = Rect::from_min_max(pos2(column.left(), cover.bottom() + 18.0), column.max);
    let mut text = ui.new_child(
        UiBuilder::new()
            .max_rect(words)
            .layout(Layout::top_down(align)),
    );
    text.spacing_mut().item_spacing.y = 4.0;
    let title_text = egui::RichText::new(&now.title)
        .font(theme::semibold(22.0))
        .color(Color32::WHITE);
    let title_resp = text.add(
        egui::Label::new(title_text)
            .sense(if now.album_id.is_some() {
                Sense::click()
            } else {
                Sense::hover()
            })
            .truncate(),
    );
    if let Some(album_id) = &now.album_id {
        let title_resp = title_resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(gettext(app.locale, "Show album in drawer"));
        if title_resp.clicked() {
            app.actions
                .push(Action::ToggleFullscreenDrawer(FullscreenDrawer::Album(
                    album_id.clone(),
                )));
        }
    }
    text.add(
        egui::Label::new(
            egui::RichText::new(&now.subtitle)
                .font(theme::regular(14.0))
                .color(Color32::from_gray(225)),
        )
        .truncate(),
    );
}

/// Calculates the responsive content column width for full-screen lyrics.
fn fullscreen_content_width(viewport_width: f32) -> f32 {
    let available = (viewport_width - 48.0).max(0.0);
    (viewport_width * 0.72).clamp(400.0, 960.0).min(available)
}

/// Selects the best artwork URL for the full-screen blurred backdrop.
fn preferred_backdrop_art(small: Option<String>, large: Option<String>) -> Option<String> {
    small.or(large)
}

/// Paints the full-screen darkened and gradient-tinted album artwork backdrop.
fn background(app: &mut App, ui: &mut egui::Ui, rect: Rect) {
    theme::apply_local(ui, &theme::Palette::dark());
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

/// Calculates UV coordinates to crop and fill the backdrop rect while preserving aspect ratio.
fn cover_uv(view: egui::Vec2, image: egui::Vec2) -> Rect {
    let ratio = (view.x / view.y.max(1.0)) / (image.x / image.y.max(1.0));
    let size = if ratio > 1.0 {
        vec2(1.0, 1.0 / ratio)
    } else {
        vec2(ratio, 1.0)
    };
    Rect::from_center_size(pos2(0.5, 0.5), size)
}

/// The top bar of the full-screen view with title, mode switcher, queue drawer button, and exit.
fn fullscreen_header(app: &mut App, ui: &mut egui::Ui) {
    let palette = theme::Palette::dark();
    let content = match app.fullscreen_content {
        FullscreenContent::NowPlaying if !app.show_lyrics_panel => FullscreenContent::NowPlaying,
        _ => FullscreenContent::Lyrics,
    };
    ui.horizontal(|ui| {
        let title = match content {
            FullscreenContent::NowPlaying => gettext(app.locale, "Now Playing"),
            FullscreenContent::Lyrics => gettext(app.locale, "Lyrics"),
        };
        theme::text(ui, title, theme::bold(18.0), palette.text);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if theme::icon_button(
                ui,
                Icon::Shrink,
                18.0,
                palette.text,
                palette.text,
                &gettext(app.locale, "Leave full screen (Esc)"),
            )
            .clicked()
            {
                app.actions.push(Action::SetFullscreen {
                    enabled: false,
                    content,
                });
            }

            let queue_open = app.fullscreen_drawer == Some(FullscreenDrawer::Queue);
            if theme::icon_button(
                ui,
                Icon::ListVideo,
                18.0,
                if queue_open {
                    palette.accent
                } else {
                    palette.secondary
                },
                palette.text,
                &gettext(app.locale, "Queue"),
            )
            .clicked()
            {
                app.actions
                    .push(Action::ToggleFullscreenDrawer(FullscreenDrawer::Queue));
            }

            match content {
                FullscreenContent::NowPlaying => {
                    if theme::icon_button(
                        ui,
                        Icon::Mic,
                        18.0,
                        palette.secondary,
                        palette.text,
                        &gettext(app.locale, "Lyrics"),
                    )
                    .clicked()
                    {
                        app.actions
                            .push(Action::SetFullscreenContent(FullscreenContent::Lyrics));
                    }
                }
                FullscreenContent::Lyrics => {
                    if theme::icon_button(
                        ui,
                        Icon::Music,
                        18.0,
                        palette.secondary,
                        palette.text,
                        &gettext(app.locale, "Now Playing"),
                    )
                    .clicked()
                    {
                        app.actions
                            .push(Action::SetFullscreenContent(FullscreenContent::NowPlaying));
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
                }
            }
        });
    });
}

/// Full screen with the cover centred and artist and title beneath.
fn now_playing_fullscreen(app: &mut App, ui: &mut egui::Ui, below: Rect) {
    if app.now_playing().is_none() {
        let mut empty_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(below)
                .layout(Layout::top_down(Align::Center)),
        );
        empty_ui.add_space((below.height() * 0.3).max(40.0));
        widgets::empty_state(
            &mut empty_ui,
            &theme::Palette::dark(),
            Icon::Music,
            &gettext(app.locale, "Nothing playing"),
            &gettext(app.locale, "Choose a song to start listening."),
        );
        return;
    }

    let side = (below.height() - 140.0)
        .min(below.width() * 0.55)
        .clamp(200.0, 520.0);
    let column = Rect::from_center_size(below.center(), vec2(side, side + 90.0));
    big_cover(app, ui, column, Align::Center);
}

/// Draws the slide-out drawer containing playlist, album, liked songs, or queue rows.
fn fullscreen_drawer(app: &mut App, ui: &mut egui::Ui, drawer_rect: Rect) {
    let palette = theme::Palette::dark();
    let clip = drawer_rect.intersect(ui.clip_rect());
    let painter = ui.painter().with_clip_rect(clip);

    painter.rect_filled(drawer_rect, 0.0, palette.panel);
    painter.line_segment(
        [drawer_rect.left_top(), drawer_rect.left_bottom()],
        egui::Stroke::new(1.0, Color32::from_white_alpha(20)),
    );

    let drawer_content_rect = drawer_rect.shrink2(vec2(16.0, 16.0));
    let mut drawer_ui = ui.new_child(UiBuilder::new().max_rect(drawer_content_rect));
    drawer_ui.set_clip_rect(clip);

    let drawer = match &app.fullscreen_drawer {
        Some(drawer) => drawer.clone(),
        None => return,
    };

    let title = match &drawer {
        FullscreenDrawer::Playlist(id) => app
            .library_entry(id)
            .map(|p| p.name.clone())
            .or_else(|| {
                app.playlist_pages
                    .get(id)
                    .and_then(|p| p.playlist.get())
                    .map(|p| p.name.clone())
            })
            .unwrap_or_else(|| gettext(app.locale, "Playlist").into_owned()),
        FullscreenDrawer::Album(id) => app
            .album_pages
            .get(id)
            .and_then(|p| p.album.get())
            .map(|a| a.name.clone())
            .or_else(|| {
                app.now_playing()
                    .filter(|now| now.album_id.as_deref() == Some(id.as_str()))
                    .map(|now| now.album_name)
            })
            .unwrap_or_else(|| gettext(app.locale, "Album").into_owned()),
        FullscreenDrawer::LikedSongs => gettext(app.locale, "Liked Songs").into_owned(),
        FullscreenDrawer::Queue => gettext(app.locale, "Queue").into_owned(),
    };

    drawer_ui.horizontal(|ui| {
        theme::text(ui, title, theme::semibold(16.0), palette.text);
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
                app.actions.push(Action::CloseFullscreenDrawer);
            }
        });
    });

    drawer_ui.add_space(12.0);

    egui::ScrollArea::vertical()
        .id_salt(("fs_drawer_scroll", &drawer))
        .show(&mut drawer_ui, |ui| {
            let row_height = theme::COMPACT_ROW_HEIGHT;
            match drawer {
                FullscreenDrawer::Playlist(id) => {
                    let total_len = app
                        .playlist_pages
                        .get(&id)
                        .map(|p| p.items.items.len())
                        .unwrap_or(0);
                    if total_len == 0 {
                        let page = app.playlist_pages.get(&id);
                        let failed = page.is_some_and(|p| {
                            matches!(p.playlist, Loadable::Failed(_)) || p.items.error.is_some()
                        });
                        let loading = page.is_none_or(|p| {
                            p.items.loading
                                || !p.items.loaded_once
                                || matches!(p.playlist, Loadable::Loading | Loadable::NotLoaded)
                        });
                        if failed {
                            widgets::empty_state(
                                ui,
                                &palette,
                                Icon::ListMusic,
                                &gettext(app.locale, "Could not load playlist"),
                                &gettext(app.locale, "Try again later."),
                            );
                        } else if loading {
                            widgets::loading_row(ui, &palette, app.locale);
                        } else {
                            widgets::empty_state(
                                ui,
                                &palette,
                                Icon::ListMusic,
                                &gettext(app.locale, "Empty playlist"),
                                &gettext(app.locale, "No songs in this playlist."),
                            );
                        }
                    } else {
                        let context = RowContext::Context {
                            uri: format!("spotify:playlist:{id}"),
                            editable_playlist: None,
                        };
                        widgets::virtual_rows(ui, total_len, row_height, |ui, idx| {
                            let item = app
                                .playlist_pages
                                .get(&id)
                                .and_then(|p| p.items.items.get(idx))
                                .and_then(|it| it.playable().cloned());
                            if let Some(item) = item {
                                widgets::track_row(
                                    ui,
                                    app,
                                    widgets::TrackRow {
                                        index: idx,
                                        number: Some(idx + 1),
                                        item: &item,
                                        context: &context,
                                        show_cover: true,
                                        show_album: false,
                                        added_at: None,
                                        added_by: None,
                                        show_added_by: false,
                                        compact: true,
                                        thin: false,
                                        shift: 0.0,
                                        picked: false,
                                        picked_songs: &[],
                                    },
                                );
                            }
                        });
                        let can_load = app
                            .playlist_pages
                            .get(&id)
                            .is_some_and(|p| p.items.can_load_more() && !p.items.loading);
                        widgets::load_more_when_near_end(
                            ui,
                            app,
                            Page::Playlist(id.clone()),
                            can_load,
                        );
                    }
                }
                FullscreenDrawer::Album(id) => {
                    let total_len = app
                        .album_pages
                        .get(&id)
                        .map(|p| p.tracks.items.len())
                        .unwrap_or(0);
                    if total_len == 0 {
                        let page = app.album_pages.get(&id);
                        let failed = page.is_some_and(|p| {
                            matches!(p.album, Loadable::Failed(_)) || p.tracks.error.is_some()
                        });
                        let loading = page.is_none_or(|p| {
                            p.tracks.loading
                                || !p.tracks.loaded_once
                                || matches!(p.album, Loadable::Loading | Loadable::NotLoaded)
                        });
                        if failed {
                            widgets::empty_state(
                                ui,
                                &palette,
                                Icon::Disc,
                                &gettext(app.locale, "Could not load album"),
                                &gettext(app.locale, "Try again later."),
                            );
                        } else if loading {
                            widgets::loading_row(ui, &palette, app.locale);
                        } else {
                            widgets::empty_state(
                                ui,
                                &palette,
                                Icon::Disc,
                                &gettext(app.locale, "Empty album"),
                                &gettext(app.locale, "No songs in this album."),
                            );
                        }
                    } else {
                        let context = RowContext::Context {
                            uri: format!("spotify:album:{id}"),
                            editable_playlist: None,
                        };
                        widgets::virtual_rows(ui, total_len, row_height, |ui, idx| {
                            let item = app
                                .album_pages
                                .get(&id)
                                .and_then(|p| p.tracks.items.get(idx))
                                .map(|t| PlayableItem::Track(t.clone()));
                            if let Some(item) = item {
                                widgets::track_row(
                                    ui,
                                    app,
                                    widgets::TrackRow {
                                        index: idx,
                                        number: Some(idx + 1),
                                        item: &item,
                                        context: &context,
                                        show_cover: false,
                                        show_album: false,
                                        added_at: None,
                                        added_by: None,
                                        show_added_by: false,
                                        compact: true,
                                        thin: false,
                                        shift: 0.0,
                                        picked: false,
                                        picked_songs: &[],
                                    },
                                );
                            }
                        });
                        let can_load = app
                            .album_pages
                            .get(&id)
                            .is_some_and(|p| p.tracks.can_load_more() && !p.tracks.loading);
                        widgets::load_more_when_near_end(
                            ui,
                            app,
                            Page::Album(id.clone()),
                            can_load,
                        );
                    }
                }
                FullscreenDrawer::LikedSongs => {
                    let total_len = app.library.liked.items.len();
                    if total_len == 0 {
                        if app.library.liked.error.is_some() {
                            widgets::empty_state(
                                ui,
                                &palette,
                                Icon::Heart,
                                &gettext(app.locale, "Could not load liked songs"),
                                &gettext(app.locale, "Try again later."),
                            );
                        } else if app.library.liked.loading || !app.library.liked.loaded_once {
                            widgets::loading_row(ui, &palette, app.locale);
                        } else {
                            widgets::empty_state(
                                ui,
                                &palette,
                                Icon::Heart,
                                &gettext(app.locale, "No liked songs"),
                                &gettext(app.locale, "Liked songs appear here."),
                            );
                        }
                    } else {
                        let context = if let Some(user) = &app.user
                            && app.library.liked.is_complete()
                        {
                            RowContext::Context {
                                uri: format!("spotify:user:{}:collection", user.id),
                                editable_playlist: None,
                            }
                        } else {
                            let cached_id = egui::Id::new("fs_drawer_liked_uris");
                            let revision = app.library.liked.revision;
                            let uris: std::sync::Arc<[String]> = ui.data_mut(|d| {
                                if let Some((rev, uris)) =
                                    d.get_temp::<(u64, std::sync::Arc<[String]>)>(cached_id)
                                    && rev == revision
                                {
                                    uris
                                } else {
                                    let uris: std::sync::Arc<[String]> = app
                                        .library
                                        .liked
                                        .items
                                        .iter()
                                        .map(|s| s.track.uri.clone())
                                        .collect();
                                    d.insert_temp(cached_id, (revision, uris.clone()));
                                    uris
                                }
                            });
                            RowContext::Uris(uris)
                        };
                        widgets::virtual_rows(ui, total_len, row_height, |ui, idx| {
                            let item = app
                                .library
                                .liked
                                .items
                                .get(idx)
                                .map(|s| PlayableItem::Track(s.track.clone()));
                            if let Some(item) = item {
                                widgets::track_row(
                                    ui,
                                    app,
                                    widgets::TrackRow {
                                        index: idx,
                                        number: Some(idx + 1),
                                        item: &item,
                                        context: &context,
                                        show_cover: true,
                                        show_album: false,
                                        added_at: None,
                                        added_by: None,
                                        show_added_by: false,
                                        compact: true,
                                        thin: false,
                                        shift: 0.0,
                                        picked: false,
                                        picked_songs: &[],
                                    },
                                );
                            }
                        });
                        let can_load =
                            app.library.liked.can_load_more() && !app.library.liked.loading;
                        widgets::load_more_when_near_end(ui, app, Page::LikedSongs, can_load);
                    }
                }
                FullscreenDrawer::Queue => {
                    let total_len = app.queue.get().map(|q| q.queue.len()).unwrap_or(0);
                    if total_len == 0 {
                        widgets::empty_state(
                            ui,
                            &palette,
                            Icon::ListVideo,
                            &gettext(app.locale, "Nothing queued"),
                            &gettext(app.locale, "Queued songs appear here."),
                        );
                    } else {
                        widgets::virtual_rows(ui, total_len, row_height, |ui, idx| {
                            let item = app.queue.get().and_then(|q| q.queue.get(idx)).cloned();
                            if let Some(item) = item {
                                widgets::track_row(
                                    ui,
                                    app,
                                    widgets::TrackRow {
                                        index: idx,
                                        number: Some(idx + 1),
                                        item: &item,
                                        context: &RowContext::Queue,
                                        show_cover: true,
                                        show_album: false,
                                        added_at: None,
                                        added_by: None,
                                        show_added_by: false,
                                        compact: true,
                                        thin: false,
                                        shift: 0.0,
                                        picked: false,
                                        picked_songs: &[],
                                    },
                                );
                            }
                        });
                    }
                }
            }
        });
}

/// The track cover, title, and artist at the top of narrow-viewport full-screen lyrics.
fn track_heading(app: &mut App, ui: &mut egui::Ui) {
    if let Some(now) = app.now_playing() {
        let album_id = now.album_id.clone();
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
            ui.vertical(|ui| {
                let title_text = egui::RichText::new(&now.title)
                    .font(theme::semibold(22.0))
                    .color(Color32::WHITE);
                let title_resp = ui.add(
                    egui::Label::new(title_text)
                        .sense(if album_id.is_some() {
                            Sense::click()
                        } else {
                            Sense::hover()
                        })
                        .truncate(),
                );
                if let Some(album_id) = album_id {
                    let title_resp = title_resp
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(gettext(app.locale, "Show album in drawer"));
                    if title_resp.clicked() {
                        app.actions
                            .push(Action::ToggleFullscreenDrawer(FullscreenDrawer::Album(
                                album_id,
                            )));
                    }
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&now.subtitle)
                            .font(theme::regular(13.0))
                            .color(Color32::from_gray(235)),
                    )
                    .truncate(),
                );
            });
        });
    }
}

/// The scrolling lyrics list or empty state inside full-screen lyrics mode.
fn fullscreen_contents(app: &mut App, ui: &mut egui::Ui) {
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
    // A Follow click resets the remembered line after drawing. Other frames
    // record the shown line before any line-click action restores following.
    if !app
        .actions
        .iter()
        .any(|action| matches!(action, Action::FollowLyrics))
    {
        app.actions.push(Action::LyricsLineShown(active));
    }
    let viewport = ui.available_rect_before_wrap();
    let manual_scroll = ui.rect_contains_pointer(viewport)
        && ui.input(|input| {
            input.smooth_scroll_delta.y != 0.0
                || (input.pointer.primary_down() && input.pointer.delta().y != 0.0)
        });
    let following = app.lyrics_following && !manual_scroll;
    let follow = following && app.lyrics_line_shown != Some(active);
    let animation = crate::motion::scroll(ui.ctx(), 0.45);
    let size = (ui.available_width() * 0.046).clamp(28.0, 42.0);
    // The line being sung brightens; all lines keep the same font metrics
    // so highlighting cannot rewrap the words during a transition.
    // A line takes 300 ms to light up or fade.
    let quiet = palette.text.gamma_multiply(0.68);
    ui.spacing_mut().scroll.fade.strength = 0.0;
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
                    blend(quiet, palette.text, lit)
                } else {
                    palette.text
                };
                let font = theme::bold(size);
                // A timed line with no words is the band playing on.
                let text = if line.text.is_empty() && lyrics.synced {
                    "\u{266a}"
                } else {
                    line.text.as_str()
                };
                let sense = if lyrics.synced {
                    Sense::click()
                } else {
                    Sense::hover()
                };
                let galley = crate::bidi::layout(
                    ui.painter(),
                    text,
                    font,
                    color,
                    ui.available_width(),
                    usize::MAX,
                    None,
                );
                let center = ui.cursor().top() + galley.size().y * 0.5;
                let edge = ((center - viewport.top()).min(viewport.bottom() - center)
                    / (size * 2.0))
                    .clamp(0.0, 1.0);
                let response = ui
                    .scope(|ui| {
                        ui.multiply_opacity(edge * edge * (3.0 - 2.0 * edge));
                        ui.add(egui::Label::new(galley).sense(sense))
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
    use super::{fullscreen_content_width, preferred_backdrop_art};

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
}
