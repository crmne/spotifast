//! The words of the playing track, in a side panel that follows the song.

use egui::{Align, Color32, Frame, Layout, Margin, Rect, Sense, UiBuilder, pos2, vec2};

use crate::app::App;
use crate::i18n::{gettext, pgettext};
use crate::model::{Action, Loadable};
use crate::theme::{self, Icon};

use super::widgets;

const LINE_GAP: f32 = 14.0;
/// Where the line being sung sits, as a fraction of the visible lyrics from
/// the top: centered focus similar to Apple Music and Spicy Lyrics.
const SUNG_LINE_AT: f32 = 0.36;

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
const LIGHT_UP_SECONDS: f32 = 0.32;
const LYRIC_PROGRESS_REPAINT: std::time::Duration = std::time::Duration::from_millis(50);
const LYRIC_COARSE_REPAINT: std::time::Duration = std::time::Duration::from_millis(250);

fn blend(from: egui::Color32, to: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    egui::Color32::from(egui::Rgba::from(from) * (1.0 - t) + egui::Rgba::from(to) * t)
}

fn side_line_size(width: f32) -> f32 {
    (width * 0.055).clamp(17.5, 22.0)
}

fn fullscreen_line_size(width: f32) -> f32 {
    (width * 0.055).clamp(32.0, 42.0)
}

fn line_animation_id(surface: &str, track: &str, index: usize) -> egui::Id {
    egui::Id::new("lyric-line").with((surface, track, index))
}

fn lyrics_layout_changed(ui: &egui::Ui, surface: &str, track: &str) -> bool {
    // Read these from the ScrollArea child, where the scrollbar has already
    // reserved its width and the viewport has its final clip height.
    let clip = ui.clip_rect();
    let current = (
        ui.available_width().to_bits(),
        clip.width().to_bits(),
        clip.height().to_bits(),
    );
    let id = egui::Id::new("lyrics-layout").with((surface, track));
    let previous = ui.ctx().data(|data| data.get_temp::<(u32, u32, u32)>(id));
    ui.ctx().data_mut(|data| data.insert_temp(id, current));
    previous != Some(current)
}

fn line_lit(ui: &egui::Ui, id: egui::Id, is_active: bool) -> f32 {
    let y = ui.cursor().min.y;
    let clip = ui.clip_rect();
    let nearby = y >= clip.top() - clip.height() && y <= clip.bottom() + clip.height();
    if nearby {
        ui.ctx().animate_bool_with_time(
            id,
            is_active,
            crate::motion::seconds(ui.ctx(), LIGHT_UP_SECONDS),
        )
    } else if is_active {
        1.0
    } else {
        0.0
    }
}

fn line_interval_progress(
    lyrics: &crate::lyrics::Lyrics,
    index: usize,
    position_ms: u32,
) -> Option<f32> {
    if !lyrics.synced {
        return None;
    }
    let at_ms = lyrics.lines.get(index)?.at_ms?;
    let next_ms = lyrics
        .lines
        .iter()
        .skip(index + 1)
        .filter_map(|line| line.at_ms)
        .find(|next_ms| *next_ms > at_ms)?;
    let duration = next_ms - at_ms;
    Some((position_ms.saturating_sub(at_ms) as f32 / duration as f32).clamp(0.0, 1.0))
}

fn next_lyric_boundary(
    lyrics: &crate::lyrics::Lyrics,
    position_ms: u32,
) -> Option<std::time::Duration> {
    lyrics
        .lines
        .iter()
        .filter_map(|line| line.at_ms)
        .filter(|at_ms| *at_ms > position_ms)
        .min()
        .map(|at_ms| std::time::Duration::from_millis(u64::from(at_ms - position_ms)))
}

fn lyric_repaint_after(
    lyrics: &crate::lyrics::Lyrics,
    position_ms: u32,
    playing: bool,
    following: bool,
    reduced_motion: bool,
    visible_interval: bool,
) -> Option<std::time::Duration> {
    if !playing || !lyrics.synced {
        return None;
    }
    let boundary = next_lyric_boundary(lyrics, position_ms);
    if visible_interval {
        let interval_repaint = if following && !reduced_motion {
            LYRIC_PROGRESS_REPAINT
        } else {
            LYRIC_COARSE_REPAINT
        };
        return Some(boundary.map_or(interval_repaint, |delay| delay.min(interval_repaint)));
    }
    boundary
}

#[derive(Clone, Copy)]
struct SideBackdropStyle {
    image_tint: Color32,
    scrim: Color32,
    gradient_top: Color32,
    gradient_bottom: Color32,
}

fn side_backdrop_style(palette: &theme::Palette) -> SideBackdropStyle {
    let panel = |alpha| {
        Color32::from_rgba_unmultiplied(
            palette.panel.r(),
            palette.panel.g(),
            palette.panel.b(),
            alpha,
        )
    };
    if palette.dark {
        SideBackdropStyle {
            image_tint: Color32::from_white_alpha(220),
            scrim: panel(218),
            gradient_top: panel(12),
            gradient_bottom: panel(76),
        }
    } else {
        SideBackdropStyle {
            image_tint: Color32::from_white_alpha(150),
            scrim: panel(160),
            gradient_top: panel(12),
            gradient_bottom: panel(58),
        }
    }
}

fn selection_fill(palette: &theme::Palette, alpha: u8) -> Color32 {
    if palette.dark {
        Color32::from_white_alpha(alpha)
    } else {
        Color32::from_black_alpha(alpha)
    }
}

fn lyric_opacity(distance: Option<isize>, is_past: bool, following: bool) -> f32 {
    if !following {
        return 1.0;
    }
    match (distance, is_past) {
        (Some(0), _) => 1.0,
        (Some(1), true) => 0.80,
        (Some(1), false) => 0.85,
        (Some(2), true) => 0.72,
        (Some(2), false) => 0.76,
        (Some(_), true) => 0.66,
        (Some(_), false) => 0.70,
        (None, _) => 0.70,
    }
}

fn paint_interval_indicator(
    ui: &egui::Ui,
    line_rect: Rect,
    progress: f32,
    palette: &theme::Palette,
) -> bool {
    let inset = if line_rect.width() > 32.0 { 16.0 } else { 0.0 };
    let width = (line_rect.width() - inset * 2.0).max(0.0);
    if width <= 0.0 {
        return false;
    }
    let indicator = Rect::from_min_size(
        pos2(line_rect.left() + inset, line_rect.bottom() + 2.0),
        vec2(width, 2.0),
    );
    if !ui.clip_rect().intersects(indicator) {
        return false;
    }
    let track =
        Color32::from_rgba_unmultiplied(palette.text.r(), palette.text.g(), palette.text.b(), 38);
    let accent = Color32::from_rgba_unmultiplied(
        palette.accent.r(),
        palette.accent.g(),
        palette.accent.b(),
        150,
    );
    ui.painter().rect_filled(indicator, 1.0, track);
    if progress > 0.0 {
        let fill = Rect::from_min_size(indicator.min, vec2(width * progress.clamp(0.0, 1.0), 2.0));
        ui.painter().rect_filled(fill, 1.0, accent);
    }
    true
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
        let rect = ui.max_rect();
        let art = app
            .now_playing()
            .and_then(|now| preferred_backdrop_art(now.art_small, now.art_url));
        let painter = ui.painter().with_clip_rect(rect);
        if let Some(texture) =
            app.lyrics_backdrop
                .texture(ui.ctx(), app.backend.art(), art.as_deref())
        {
            let backdrop = side_backdrop_style(&palette);
            painter.image(
                texture.id(),
                rect,
                cover_uv(rect.size(), texture.size_vec2()),
                backdrop.image_tint,
            );
        }
        let backdrop = side_backdrop_style(&palette);
        painter.rect_filled(rect, 0.0, backdrop.scrim);
        widgets::paint_vertical_gradient(ui, rect, backdrop.gradient_top, backdrop.gradient_bottom);
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
                    app.actions.push(Action::FollowLyrics);
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
    let line_changed = app.lyrics_line_shown != Some(active);
    let viewport = ui.available_rect_before_wrap();
    let manual_scroll = ui.rect_contains_pointer(viewport)
        && ui.input(|input| {
            input.smooth_scroll_delta.y != 0.0
                || (input.pointer.primary_down() && input.pointer.delta().y != 0.0)
        });
    let following = app.lyrics_following && !manual_scroll;
    let mut visible_interval = false;
    let scroll = crate::autoscroll::show(
        ui,
        egui::ScrollArea::vertical()
            .id_salt("lyrics-scroll")
            .auto_shrink([false, false]),
        egui::Vec2b::new(false, true),
        |ui| {
            let layout_changed = lyrics_layout_changed(ui, "side-panel", &now.uri);
            let follow = following && (line_changed || layout_changed);
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
                let distance = active.map(|a| (index as isize - a as isize).abs());
                let is_past = active.is_some_and(|a| index < a);
                let target_opacity = lyric_opacity(distance, is_past, app.lyrics_following);
                let lit = line_lit(
                    ui,
                    line_animation_id("side-panel", &now.uri, index),
                    is_active,
                );
                let base_color = palette.text.gamma_multiply(target_opacity);
                let color = blend(base_color, palette.text, lit);
                let font = theme::semibold(side_line_size(ui.available_width()));
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
                let pill_alpha = (lit * 34.0) as u8;
                let response = Frame::new()
                    .fill(selection_fill(&palette, pill_alpha))
                    .corner_radius(6.0)
                    .inner_margin(Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        if crate::bidi::is_rtl(text) {
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
                        }
                    })
                    .inner;
                crate::autoscroll::row(ui, &response);
                let rect = response.rect;
                if is_active
                    && let Some(progress) = line_interval_progress(&lyrics, index, now.position_ms)
                {
                    visible_interval |= paint_interval_indicator(ui, rect, progress, &palette);
                }
                if lyrics.synced
                    && response.clicked()
                    && let Some(at_ms) = line.at_ms
                {
                    app.actions.push(Action::Seek(at_ms));
                    app.actions.push(Action::FollowLyrics);
                }
                if is_active && follow {
                    let animation = crate::motion::scroll(ui.ctx(), 0.60);
                    show_sung_line(ui, rect, Some(animation));
                }
                ui.add_space(LINE_GAP);
            }
            // Words without timing can only be followed by the clock: sit
            // at the part of the text the song is probably at.
            if following && !lyrics.synced && now.duration_ms > 0 {
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
    if manual_scroll && app.lyrics_following {
        app.actions.push(Action::PauseLyricsFollow);
    }
    app.lyrics_line_shown = Some(active);
    if let Some(delay) = lyric_repaint_after(
        &lyrics,
        now.position_ms,
        now.playing,
        app.lyrics_following,
        crate::motion::reduced(ui.ctx()),
        visible_interval,
    ) {
        ui.ctx().request_repaint_after(delay);
    }
}

pub fn fullscreen(app: &mut App, ui: &mut egui::Ui) {
    egui::CentralPanel::default()
        .frame(Frame::new().fill(theme::Palette::dark().window))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            background(app, ui, rect);
            let top = theme::titlebar_inset(ui.ctx()) + 24.0;
            if app.now_playing().is_some() && rect.width() >= COVER_BESIDE_MIN_WIDTH {
                with_cover(app, ui, rect, top);
                return;
            }
            let width = fullscreen_content_width(rect.width());
            let region = Rect::from_min_max(
                pos2(rect.center().x - width / 2.0, rect.top() + top),
                pos2(rect.center().x + width / 2.0, rect.bottom()),
            );
            let mut content = ui.new_child(UiBuilder::new().max_rect(region));
            fullscreen_header(app, &mut content);
            content.add_space(20.0);
            track_heading(app, &mut content);
            content.add_space(16.0);
            fullscreen_contents(app, &mut content);
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
fn with_cover(app: &mut App, ui: &mut egui::Ui, rect: Rect, top: f32) {
    let outer = Rect::from_min_max(
        pos2(rect.left() + 48.0, rect.top() + top),
        pos2(rect.right() - 48.0, rect.bottom() - 40.0),
    );
    let mut header = ui.new_child(UiBuilder::new().max_rect(outer));
    fullscreen_header(app, &mut header);
    let below = Rect::from_min_max(
        pos2(outer.left(), header.min_rect().bottom() + 24.0),
        outer.max,
    );
    // The cover moves aside only for words to read. While they load it
    // stays in the middle, so a song that turns out to have none never
    // moves at all.
    let words = matches!(&app.lyrics, Loadable::Loaded(Some(lyrics)) if !lyrics.instrumental);
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
        super::queue::fullscreen_column(app, &mut queue_ui);
    }
    if words {
        if app.show_queue_panel {
            // The cover docks left; the lyrics take the middle that remains.
            let room = lyrics_end - below.left();
            let side = (room * 0.42).min(below.height() - 90.0).clamp(160.0, 520.0);
            let column = Rect::from_min_size(
                pos2(below.left(), below.center().y - (side + 90.0) / 2.0),
                vec2(side, side + 90.0),
            );
            big_cover(app, ui, column, Align::Min);
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
            fullscreen_contents(app, &mut content);
        } else {
            // The cover and the lyrics are one group, centred in the window.
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
        let column = Rect::from_center_size(span.center(), vec2(side, side + 90.0));
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

/// The playing song's cover filling the top of `column`, with its title and
/// artists beneath, aligned to its left edge or centred.
fn big_cover(app: &App, ui: &mut egui::Ui, column: Rect, align: Align) {
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
    text.add(
        egui::Label::new(
            egui::RichText::new(&now.title)
                .font(theme::semibold(22.0))
                .color(Color32::WHITE),
        )
        .truncate(),
    );
    text.add(
        egui::Label::new(
            egui::RichText::new(&now.subtitle)
                .font(theme::regular(14.0))
                .color(Color32::from_gray(225)),
        )
        .truncate(),
    );
}

fn fullscreen_content_width(viewport_width: f32) -> f32 {
    let available = (viewport_width - 48.0).max(0.0);
    (viewport_width * 0.72).clamp(400.0, 960.0).min(available)
}

fn preferred_backdrop_art(small: Option<String>, large: Option<String>) -> Option<String> {
    small.or(large)
}

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
            Color32::from_white_alpha(205),
        );
    }
    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(190));
    widgets::paint_vertical_gradient(
        ui,
        rect,
        Color32::from_black_alpha(12),
        Color32::from_black_alpha(76),
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

fn fullscreen_header(app: &mut App, ui: &mut egui::Ui) {
    let palette = theme::Palette::dark();
    ui.horizontal(|ui| {
        theme::text(
            ui,
            gettext(app.locale, "Lyrics"),
            theme::bold(18.0),
            palette.text,
        );
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

fn track_heading(app: &App, ui: &mut egui::Ui) {
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
            ui.vertical(|ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&now.title)
                            .font(theme::semibold(22.0))
                            .color(Color32::WHITE),
                    )
                    .truncate(),
                );
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
    let line_changed = app.lyrics_line_shown != Some(active);
    let animation = crate::motion::scroll(ui.ctx(), 0.45);
    let line_size = fullscreen_line_size(ui.available_width());
    ui.spacing_mut().scroll.fade.strength = 0.0;
    let mut visible_interval = false;
    egui::ScrollArea::vertical()
        .id_salt(("fullscreen-lyrics-scroll", &now.uri))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let layout_changed = lyrics_layout_changed(ui, "fullscreen", &now.uri);
            let follow = following && (line_changed || layout_changed);
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
                    (viewport.height() * SUNG_LINE_AT - line_size).max(12.0),
                    viewport.height() * (1.0 - SUNG_LINE_AT),
                )
            } else {
                (12.0, 60.0)
            };
            ui.add_space(padding);
            for (index, line) in lyrics.lines.iter().enumerate() {
                let is_active = active == Some(index);
                let distance = active.map(|a| (index as isize - a as isize).abs());
                let is_past = active.is_some_and(|a| index < a);
                let target_opacity = lyric_opacity(distance, is_past, following);
                let lit = line_lit(
                    ui,
                    line_animation_id("fullscreen", &now.uri, index),
                    is_active,
                );
                let base_color = palette.text.gamma_multiply(target_opacity);
                let color = if lyrics.synced {
                    blend(base_color, Color32::WHITE, lit)
                } else {
                    palette.text
                };
                let font = theme::semibold(line_size);
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
                let pill_alpha = (lit * 32.0) as u8;
                let response = Frame::new()
                    .fill(selection_fill(&palette, pill_alpha))
                    .corner_radius(10.0)
                    .inner_margin(Margin::symmetric(14, 6))
                    .show(ui, |ui| {
                        let galley = crate::bidi::layout(
                            ui.painter(),
                            text,
                            font,
                            color,
                            ui.available_width(),
                            usize::MAX,
                            None,
                        );
                        // Soften rows cut by the viewport while following.
                        // Manual browsing keeps the full text contrast.
                        if following {
                            let clip = ui.clip_rect();
                            let center = ui.cursor().top() + galley.size().y * 0.5;
                            let edge = ((center - clip.top()).min(clip.bottom() - center)
                                / (line_size * 2.0))
                                .clamp(0.0, 1.0);
                            ui.multiply_opacity(edge * edge * (3.0 - 2.0 * edge));
                        }
                        ui.add(egui::Label::new(galley).sense(sense))
                    })
                    .inner;
                let rect = response.rect;
                if is_active
                    && let Some(progress) = line_interval_progress(&lyrics, index, now.position_ms)
                {
                    visible_interval |= paint_interval_indicator(ui, rect, progress, &palette);
                }
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
                ui.add_space(34.0);
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
    if let Some(delay) = lyric_repaint_after(
        &lyrics,
        now.position_ms,
        now.playing,
        following,
        crate::motion::reduced(ui.ctx()),
        visible_interval,
    ) {
        ui.ctx().request_repaint_after(delay);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fullscreen_content_width, line_interval_progress, lyric_opacity, lyric_repaint_after,
        next_lyric_boundary, preferred_backdrop_art, side_backdrop_style,
    };

    fn timed_lyrics(times: &[u32]) -> crate::lyrics::Lyrics {
        crate::lyrics::Lyrics {
            lines: times
                .iter()
                .map(|at_ms| crate::lyrics::Line {
                    at_ms: Some(*at_ms),
                    text: "line".into(),
                })
                .collect(),
            synced: true,
            instrumental: false,
        }
    }

    fn composite(foreground: egui::Color32, background: egui::Color32) -> egui::Color32 {
        let [red, green, blue, alpha] = foreground.to_srgba_unmultiplied();
        let [background_red, background_green, background_blue, _] =
            background.to_srgba_unmultiplied();
        let alpha = f32::from(alpha) / 255.0;
        let channel = |foreground: u8, background: u8| {
            (f32::from(foreground) * alpha + f32::from(background) * (1.0 - alpha)).round() as u8
        };
        egui::Color32::from_rgb(
            channel(red, background_red),
            channel(green, background_green),
            channel(blue, background_blue),
        )
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

    #[test]
    fn line_interval_uses_a_strictly_later_timestamp_and_has_no_last_line_guess() {
        let lyrics = timed_lyrics(&[1_000, 1_000, 3_000]);
        assert_eq!(line_interval_progress(&lyrics, 0, 2_000), Some(0.5));
        assert_eq!(line_interval_progress(&lyrics, 1, 2_000), Some(0.5));
        assert_eq!(line_interval_progress(&lyrics, 2, 4_000), None);
        assert_eq!(
            line_interval_progress(&timed_lyrics(&[1_000, 1_000]), 0, 1_500),
            None
        );
        let mut unsynced = lyrics;
        unsynced.synced = false;
        assert_eq!(line_interval_progress(&unsynced, 0, 2_000), None);
    }

    #[test]
    fn lyrics_repaint_only_for_visible_progress_or_the_next_real_boundary() {
        let lyrics = timed_lyrics(&[1_000, 1_000, 3_000]);
        assert_eq!(
            next_lyric_boundary(&lyrics, 1_000),
            Some(std::time::Duration::from_secs(2))
        );
        assert_eq!(
            lyric_repaint_after(&lyrics, 1_000, true, true, false, true),
            Some(std::time::Duration::from_millis(50))
        );
        assert_eq!(
            lyric_repaint_after(
                &timed_lyrics(&[1_000, 1_010]),
                1_000,
                true,
                true,
                false,
                true
            ),
            Some(std::time::Duration::from_millis(10))
        );
        assert_eq!(
            lyric_repaint_after(&lyrics, 1_000, true, false, false, true),
            Some(std::time::Duration::from_millis(250))
        );
        assert_eq!(
            lyric_repaint_after(&lyrics, 1_000, true, true, true, true),
            Some(std::time::Duration::from_millis(250))
        );
        assert_eq!(
            lyric_repaint_after(&lyrics, 1_000, true, false, false, false),
            Some(std::time::Duration::from_secs(2))
        );
        let mut unsynced = lyrics.clone();
        unsynced.synced = false;
        assert_eq!(
            lyric_repaint_after(&unsynced, 1_000, true, true, false, true),
            None
        );
        assert_eq!(
            lyric_repaint_after(&lyrics, 1_000, false, true, false, true),
            None
        );

        let last_line = timed_lyrics(&[1_000]);
        assert_eq!(line_interval_progress(&last_line, 0, 1_500), None);
        assert_eq!(
            lyric_repaint_after(&last_line, 1_500, true, true, false, false),
            None
        );
    }

    #[test]
    fn follow_off_keeps_the_whole_lyrics_readable() {
        assert_eq!(lyric_opacity(Some(7), true, false), 1.0);
        assert_eq!(lyric_opacity(None, false, false), 1.0);
        assert!(lyric_opacity(Some(1), true, true) >= 0.8);
        assert!(lyric_opacity(Some(3), true, true) >= 0.66);
    }

    fn luminance(color: egui::Color32) -> f32 {
        let linear = egui::Rgba::from(color);
        0.2126 * linear.r() + 0.7152 * linear.g() + 0.0722 * linear.b()
    }

    fn contrast(left: egui::Color32, right: egui::Color32) -> f32 {
        let (light, dark) = if luminance(left) >= luminance(right) {
            (luminance(left), luminance(right))
        } else {
            (luminance(right), luminance(left))
        };
        (light + 0.05) / (dark + 0.05)
    }

    #[test]
    fn side_backdrop_keeps_text_contrast_with_or_without_extreme_art() {
        for palette in [
            crate::theme::Palette::dark(),
            crate::theme::Palette::light(),
        ] {
            let style = side_backdrop_style(&palette);
            let min_opacity = lyric_opacity(Some(3), true, true);
            for art in [None, Some(egui::Color32::BLACK), Some(egui::Color32::WHITE)] {
                let background = art.map_or(palette.panel, |art| {
                    let art = egui::Color32::from_rgba_unmultiplied(
                        art.r(),
                        art.g(),
                        art.b(),
                        style.image_tint.a(),
                    );
                    composite(art, palette.panel)
                });
                let background = composite(style.scrim, background);
                for gradient in [style.gradient_top, style.gradient_bottom] {
                    let backdrop = composite(gradient, background);
                    assert!(
                        contrast(palette.text, backdrop) >= 4.5,
                        "text contrast is preserved for dark={}",
                        palette.dark
                    );
                    let dimmed_text = composite(palette.text.gamma_multiply(min_opacity), backdrop);
                    assert!(
                        contrast(dimmed_text, backdrop) >= 4.5,
                        "dimmed text contrast is preserved for dark={} (contrast={})",
                        palette.dark,
                        contrast(dimmed_text, backdrop)
                    );
                }
            }
        }
    }
}
