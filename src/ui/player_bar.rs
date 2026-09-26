//! The now-playing bar along the bottom of the window.

use egui::{Align, Color32, Frame, Layout, Margin, Rect, Sense, UiBuilder, Vec2, pos2, vec2};

use crate::app::{App, NowPlaying};
use crate::i18n::gettext;
use crate::model::{Action, DragTrack, Page};
use crate::player::RepeatMode;
use crate::theme::{self, Icon};
use crate::util;

use super::widgets::{SliderEvent, thin_slider};

/// How much of the playing art's tint the bar's fill carries.
const TINT_STRENGTH: f32 = 0.12;
/// How long the bar takes to cross over to a new song's tint.
const TINT_FADE_SECONDS: f32 = 0.45;
const TINT_SESSION_ID: &str = "player-bar-tint-session";
/// How strongly the visualizer shows through behind the controls: the
/// spectrum's bars fade from their foot to their top, with a glow around
/// them, a brighter cap above, and a pulse along the foot with the bass.
const SPECTRUM_ALPHA: (f32, f32) = (0.28, 0.08);
const GLOW_ALPHA: f32 = 0.12;
const PEAK_ALPHA: f32 = 0.7;
const PULSE_ALPHA: f32 = 0.35;
/// The peak cap's thickness, and its gap above the band.
const PEAK_HEIGHT: f32 = 2.0;
const PEAK_GAP: f32 = 2.0;
/// The waveform's line, as layers from the widest glow to the core, the
/// shading under it, and its mirrored echo.
const WAVE_LAYERS: [(f32, f32); 3] = [(9.0, 0.06), (4.0, 0.16), (1.6, 0.85)];
const WAVE_FILL_ALPHA: f32 = 0.14;
const WAVE_ECHO_ALPHA: f32 = 0.18;
/// How much more strongly the visualizer paints over a light bar.
const LIGHT_STRENGTH: f32 = 1.5;
/// The gap between spectrum bars.
const SPECTRUM_GAP: f32 = 2.0;
/// How often a moving visualizer is drawn: sixty times a second, as the
/// mini player's.
const VIS_FRAME: std::time::Duration = std::time::Duration::from_micros(16_667);

/// Forget this bar's animation session while the sign-in screen is shown.
pub(crate) fn end_tint_session(ctx: &egui::Context) {
    ctx.data_mut(|data| data.remove::<u64>(egui::Id::new(TINT_SESSION_ID)));
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let fill = eased_fill(ui.ctx(), palette.panel, app.now_playing_tint());
    egui::Panel::bottom("player-bar")
        .exact_size(theme::PLAYER_BAR_HEIGHT)
        .resizable(false)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(fill)
                .inner_margin(Margin::symmetric(16, 0)),
        )
        .show(ui, |ui| {
            let rect = ui.max_rect();
            let now = app.now_playing();
            // The whole bar, margins included, behind everything else.
            let behind = rect.expand2(vec2(16.0, 0.0));
            if visualizer(app, ui, behind, now.as_ref()) {
                ui.ctx().request_repaint_after(VIS_FRAME);
            }
            // Its empty space is the visualizer's control, as Winamp's
            // visualizer was: a click moves to the next mode. The controls
            // drawn after it take their own clicks.
            let empty = ui
                .interact(
                    behind,
                    ui.id().with("player-bar-visualizer"),
                    Sense::click(),
                )
                .on_hover_text_at_pointer(gettext(app.locale, "Click to change the visualizer"));
            if empty.clicked() {
                app.actions.push(Action::CyclePlayerBarVis);
            }
            ui.painter().hline(
                rect.x_range(),
                rect.top() + 0.5,
                egui::Stroke::new(1.0, palette.outline),
            );
            let width = rect.width();
            let side = (width * 0.3).clamp(200.0, 420.0);
            let cy = rect.center().y;
            let left = Rect::from_min_max(rect.min, pos2(rect.left() + side, rect.bottom()));
            let center = Rect::from_min_max(
                pos2(rect.left() + side, rect.top()),
                pos2(rect.right() - side, rect.bottom()),
            );

            // egui's cross-axis centring is unreliable across nested layouts of
            // mixed heights, so each region is placed in an explicit band that
            // is sized to its content and centred on the bar's midline.
            now_playing_block(app, ui, left, now.as_ref());

            transport(app, ui, now.as_ref(), center);

            let right_band =
                Rect::from_min_size(pos2(rect.right() - side, cy - 15.0), vec2(side, 30.0));
            let mut right_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(right_band)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            extras(app, &mut right_ui, now.as_ref());
        });
}

/// Draws the chosen spectrum or waveform of the song playing on this
/// computer across `rect`, in colours drawn from the cover. It reads the
/// same post-equalizer, pre-volume sound as the mini player's visualizer,
/// so the volume never moves it. Returns whether it is still moving.
fn visualizer(app: &mut App, ui: &egui::Ui, rect: Rect, now: Option<&NowPlaying>) -> bool {
    use crate::settings::PlayerBarVis;
    use crate::vis;
    let mode = app.settings.player_bar_vis;
    let sounding = now.is_some_and(|now| (now.playing || now.loading) && now.local);
    let dark = ui.visuals().dark_mode;
    let (low, high) = vis_colours(app.now_playing_tint().unwrap_or(app.palette.accent), dark);
    // Over a light bar the same colour paints more strongly, or white
    // would wash it out.
    let strength = if dark { 1.0 } else { LIGHT_STRENGTH };
    let painter = ui.painter().with_clip_rect(rect);
    match mode {
        PlayerBarVis::Off => false,
        PlayerBarVis::Spectrum => {
            if !sounding && app.player_bar_analyser.settled() {
                return false;
            }
            let samples = if sounding {
                app.winamp.tap.window(vis::WIDE_SAMPLES, vis::LAG)
            } else {
                Vec::new()
            };
            let levels = app
                .player_bar_analyser
                .step(&samples, std::time::Instant::now());
            let peaks = app.player_bar_analyser.peaks();
            spectrum(&painter, rect, &levels, &peaks, (low, high), strength);
            sounding || !app.player_bar_analyser.settled()
        }
        PlayerBarVis::Waveform => {
            if !sounding {
                return false;
            }
            // Winamp's scope with a column every eight points or so.
            let count = (rect.width() / 8.0).clamp(75.0, 320.0) as usize;
            let samples = app.winamp.tap.window(count * vis::SCOPE_STEP, vis::LAG);
            waveform(
                &painter,
                rect,
                &vis::scope_line(&samples, count),
                (low, high),
                strength,
            );
            true
        }
    }
}

/// The visualizer's two colours: the cover's own for the bass, and the same
/// turned a sixth of the way round the colour wheel for the treble. Both
/// are made vivid, then kept to a band of brightness, so the words the bars
/// pass behind always stand out: no lighter than [`DARK_CEILING`] under the
/// dark theme's white text, no darker than [`LIGHT_FLOOR`] under the light
/// theme's dark text.
fn vis_colours(base: Color32, dark: bool) -> (Color32, Color32) {
    let mut low = egui::ecolor::Hsva::from(base);
    low.s = low.s.max(0.55);
    low.v = 1.0;
    low.a = 1.0;
    let mut high = low;
    high.h = (high.h + 1.0 / 6.0).fract();
    let keep = |colour: egui::ecolor::Hsva| within_brightness(Color32::from(colour), dark);
    (keep(low), keep(high))
}

/// The most luminance a visualizer colour has under the dark theme, and the
/// band it is kept in under the light theme, as relative luminance from 0
/// to 1. On white the colour must be dark enough to show at all yet light
/// enough that dark words stay clear over it.
const DARK_CEILING: f32 = 0.35;
const LIGHT_FLOOR: f32 = 0.2;
const LIGHT_CEILING: f32 = 0.4;

/// `colour` with its hue kept and its brightness moved into the band the
/// theme's text reads against: darkened towards black when too light,
/// lightened towards white when too dark.
fn within_brightness(colour: Color32, dark: bool) -> Color32 {
    let linear = egui::Rgba::from(colour);
    let luminance = 0.2126 * linear.r() + 0.7152 * linear.g() + 0.0722 * linear.b();
    let ceiling = if dark { DARK_CEILING } else { LIGHT_CEILING };
    let adjusted = if luminance > ceiling {
        linear * (ceiling / luminance)
    } else if !dark && luminance < LIGHT_FLOOR {
        let toward_white = (LIGHT_FLOOR - luminance) / (1.0 - luminance).max(f32::EPSILON);
        egui::Rgba::from_rgb(
            linear.r() + (1.0 - linear.r()) * toward_white,
            linear.g() + (1.0 - linear.g()) * toward_white,
            linear.b() + (1.0 - linear.b()) * toward_white,
        )
    } else {
        linear
    };
    Color32::from(egui::Rgba::from_rgb(
        adjusted.r(),
        adjusted.g(),
        adjusted.b(),
    ))
}

/// Adds a rectangle shaded from `top` to `bottom`.
fn shaded(mesh: &mut egui::Mesh, rect: Rect, top: Color32, bottom: Color32) {
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(base, base + 1, base + 2);
    mesh.add_triangle(base, base + 2, base + 3);
}

/// Bars rising from the foot of the player bar, swept from the bass colour
/// to the treble colour, with a soft glow, a peak cap hanging above each,
/// and the whole foot of the bar breathing with the bass.
fn spectrum(
    painter: &egui::Painter,
    rect: Rect,
    levels: &[f32],
    peaks: &[f32],
    (low, high): (Color32, Color32),
    strength: f32,
) {
    let bands = levels.len() as f32;
    let width = (rect.width() - SPECTRUM_GAP * (bands - 1.0)) / bands;
    // A full band reaches the top, with room above for its cap.
    let reach = rect.height() - PEAK_GAP - PEAK_HEIGHT - 1.0;
    let mut mesh = egui::Mesh::default();

    // The bass pulse: a glow along the foot, as strong as the low bands.
    let bass = levels.iter().take(levels.len() / 8).copied().sum::<f32>()
        / (levels.len() / 8).max(1) as f32;
    let pulse = Rect::from_min_max(pos2(rect.left(), rect.center().y), rect.right_bottom());
    shaded(
        &mut mesh,
        pulse,
        Color32::TRANSPARENT,
        low.gamma_multiply(PULSE_ALPHA * strength * bass * bass),
    );

    for (index, (level, peak)) in levels.iter().zip(peaks).enumerate() {
        let left = rect.left() + index as f32 * (width + SPECTRUM_GAP);
        let colour = low.lerp_to_gamma(high, index as f32 / (bands - 1.0));
        let height = level * reach;
        if height >= 1.0 {
            let bar = Rect::from_min_max(
                pos2(left, rect.bottom() - height),
                pos2(left + width, rect.bottom()),
            );
            // The glow: the bar again, wider and fainter.
            shaded(
                &mut mesh,
                bar.expand2(vec2(SPECTRUM_GAP, 3.0)),
                colour.gamma_multiply(GLOW_ALPHA * strength * 0.3),
                colour.gamma_multiply(GLOW_ALPHA * strength),
            );
            let (foot, top) = (SPECTRUM_ALPHA.0 * strength, SPECTRUM_ALPHA.1 * strength);
            shaded(
                &mut mesh,
                bar,
                colour.gamma_multiply(foot + (top - foot) * level),
                colour.gamma_multiply(foot),
            );
        }
        let cap = peak * reach;
        if cap >= 2.0 {
            let y = rect.bottom() - cap - PEAK_GAP;
            shaded(
                &mut mesh,
                Rect::from_min_max(pos2(left, y - PEAK_HEIGHT), pos2(left + width, y)),
                colour.gamma_multiply(PEAK_ALPHA),
                colour.gamma_multiply(PEAK_ALPHA),
            );
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// The wave as a neon line in layers of glow, swept from the bass colour to
/// the treble colour, over a faint shading down to the midline and a dim
/// mirrored echo.
fn waveform(
    painter: &egui::Painter,
    rect: Rect,
    trace: &[f32],
    (low, high): (Color32, Color32),
    strength: f32,
) {
    if trace.len() < 2 {
        return;
    }
    let reach = rect.height() * 0.46;
    let step = rect.width() / (trace.len() - 1) as f32;
    let midline = rect.center().y;
    let point =
        |index: usize, value: f32| pos2(rect.left() + index as f32 * step, midline - value * reach);
    let colour_at = |index: usize| low.lerp_to_gamma(high, index as f32 / (trace.len() - 1) as f32);

    // The shading between the wave and the midline.
    let mut mesh = egui::Mesh::default();
    for index in 0..trace.len() - 1 {
        let colour = colour_at(index).gamma_multiply(WAVE_FILL_ALPHA * strength);
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(point(index, trace[index]), colour);
        mesh.colored_vertex(point(index + 1, trace[index + 1]), colour);
        mesh.colored_vertex(pos2(point(index + 1, 0.0).x, midline), Color32::TRANSPARENT);
        mesh.colored_vertex(pos2(point(index, 0.0).x, midline), Color32::TRANSPARENT);
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    painter.add(egui::Shape::mesh(mesh));

    // Short runs of line, so the colour can sweep along the wave.
    let run = 8;
    let stroke = |mirror: f32, width: f32, alpha: f32| {
        let mut start = 0;
        while start < trace.len() - 1 {
            let end = (start + run).min(trace.len() - 1);
            let points = (start..=end)
                .map(|index| point(index, trace[index] * mirror))
                .collect();
            let colour = colour_at((start + end) / 2).gamma_multiply(alpha);
            painter.add(egui::Shape::line(points, egui::Stroke::new(width, colour)));
            start = end;
        }
    };
    stroke(-0.6, 1.0, WAVE_ECHO_ALPHA);
    for (width, alpha) in WAVE_LAYERS {
        stroke(1.0, width, alpha);
    }
}

/// Ease the final fill's RGB. Untinted custom panels keep their alpha while
/// the colour returns to the panel colour.
fn eased_fill(ctx: &egui::Context, panel: Color32, tint: Option<Color32>) -> Color32 {
    let target = tint.map_or(panel, |tint| super::blend(panel, tint, TINT_STRENGTH));
    // Color32 stores premultiplied RGB, so interpolate unmultiplied channels.
    let [r, g, b, _] = target.to_srgba_unmultiplied();
    // A new pass after sign-out gets new ids without clearing other animations.
    let pass = ctx.cumulative_pass_nr();
    let session = ctx.data_mut(|data| {
        *data.get_temp_mut_or_insert_with(egui::Id::new(TINT_SESSION_ID), || pass)
    });
    let channel = |axis: &'static str, value: u8| {
        ctx.animate_value_with_time(
            egui::Id::new(("player-bar-tint", session, axis)),
            f32::from(value),
            TINT_FADE_SECONDS,
        )
        .round() as u8
    };
    let eased = [channel("r", r), channel("g", g), channel("b", b)];
    if eased == [r, g, b] {
        target
    } else {
        Color32::from_rgba_unmultiplied(eased[0], eased[1], eased[2], target.a())
    }
}

fn now_playing_block(app: &mut App, ui: &mut egui::Ui, region: Rect, now: Option<&NowPlaying>) {
    let palette = app.palette;
    let cy = region.center().y;
    let cover_rect = Rect::from_min_size(pos2(region.left() + 4.0, cy - 28.0), Vec2::splat(56.0));

    let Some(now) = now else {
        super::widgets::paint_cover(ui, &palette, None, cover_rect, 6.0, Icon::Music, None);
        let text_left = cover_rect.right() + 12.0;
        let text_rect = Rect::from_min_size(
            pos2(text_left, cy - 17.0),
            vec2((region.right() - text_left - 8.0).max(40.0), 34.0),
        );
        let mut text_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(text_rect)
                .layout(Layout::top_down(Align::Min)),
        );
        text_ui.spacing_mut().item_spacing.y = 2.0;
        theme::text(
            &mut text_ui,
            gettext(app.locale, "Nothing playing"),
            theme::medium(14.0),
            palette.secondary,
        );
        theme::text(
            &mut text_ui,
            gettext(app.locale, "Pick a song, album, or playlist"),
            theme::regular(12.0),
            palette.dim,
        );
        return;
    };

    super::widgets::paint_cover(
        ui,
        &palette,
        now.art_small.as_deref().or(now.art_url.as_deref()),
        cover_rect,
        6.0,
        Icon::Music,
        Some(app.backend.art()),
    );
    let song = app.now_playing_item();
    let drag_sense = if song.is_some() {
        Sense::click_and_drag()
    } else {
        Sense::click()
    };
    let cover_response = ui
        .interact(cover_rect, egui::Id::new("now-playing-cover"), drag_sense)
        .on_hover_cursor(if song.is_some() {
            egui::CursorIcon::Grab
        } else {
            egui::CursorIcon::Default
        });
    // Hovering the cover offers to dock the art large at the sidebar's
    // bottom, the way Spotify expands it. (#92)
    let art_available = now.art_url.is_some() || now.art_small.is_some();
    let expand_rect = Rect::from_center_size(
        pos2(cover_rect.right() - 10.0, cover_rect.top() + 10.0),
        Vec2::splat(18.0),
    );
    let offer_expand = art_available && !app.settings.art_expanded && app.settings.sidebar_visible;
    let over_expand = offer_expand && ui.rect_contains_pointer(expand_rect);
    if cover_response.clicked() && !over_expand {
        if let Some(id) = &now.album_id {
            app.actions.push(Action::Open(Page::Album(id.clone())));
        } else if let Some(id) = &now.show_id {
            app.actions.push(Action::Open(Page::Show(id.clone())));
        }
    }
    if offer_expand && (cover_response.hovered() || over_expand) {
        let expand = ui.interact(
            expand_rect,
            egui::Id::new("now-playing-art-expand"),
            Sense::click(),
        );
        ui.painter()
            .circle_filled(expand_rect.center(), 9.0, palette.panel.gamma_multiply(0.9));
        Icon::ChevronUp.image(palette.text, 12.0).paint_at(
            ui,
            Rect::from_center_size(expand_rect.center(), Vec2::splat(12.0)),
        );
        if expand.clicked() {
            app.settings.art_expanded = true;
            app.actions.push(Action::SettingsChanged);
        }
    }
    let heart_width = if now.is_episode { 0.0 } else { 42.0 };
    let text_left = cover_rect.right() + 12.0;
    let text_width = (region.right() - text_left - heart_width).max(40.0);
    let text_rect = Rect::from_min_size(pos2(text_left, cy - 18.0), vec2(text_width, 36.0));
    let info_response = ui.interact(text_rect, egui::Id::new("now-playing-info"), drag_sense);
    let mut text_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    text_ui.set_clip_rect(text_rect.intersect(ui.clip_rect()));
    text_ui.spacing_mut().item_spacing.y = 2.0;
    let title_response = theme::link(&mut text_ui, &now.title, theme::medium(14.0), palette.text);
    if title_response.clicked() {
        if let Some(id) = &now.album_id {
            app.actions.push(Action::Open(Page::Album(id.clone())));
        } else if let Some(id) = &now.show_id {
            app.actions.push(Action::Open(Page::Show(id.clone())));
        }
    }
    text_ui.horizontal_top(|ui| {
        if now.artists.is_empty() {
            if theme::link(ui, &now.subtitle, theme::regular(12.0), palette.secondary).clicked()
                && let Some(id) = &now.show_id
            {
                app.actions.push(Action::Open(Page::Show(id.clone())));
            }
        } else {
            super::widgets::artist_links(
                ui,
                app,
                &now.artists,
                theme::regular(12.0),
                palette.secondary,
            );
        }
    });
    if (cover_response.drag_started_by(egui::PointerButton::Primary)
        || info_response.drag_started_by(egui::PointerButton::Primary))
        && let Some(item) = &song
    {
        egui::DragAndDrop::set_payload(
            ui.ctx(),
            DragTrack {
                title: item.name().to_string(),
                image: item.image(64).map(str::to_string),
                items: vec![item.clone()],
                from: None,
            },
        );
    }

    // The playing thing answers the same right-click menu as a table row,
    // from the cover, the empty space around the words, or the words.
    if let Some(item) = song {
        for response in [&cover_response, &info_response, &title_response] {
            egui::Popup::context_menu(response)
                .frame(super::widgets::menu_frame(&palette))
                .show(|ui| super::widgets::item_menu(ui, app, &item, None, None));
        }
    }

    if !now.is_episode {
        let saved = app.is_saved(&now.uri).unwrap_or(false);
        let (icon, color, tooltip) = if saved {
            (
                Icon::HeartFilled,
                palette.accent,
                gettext(app.locale, "Remove from Liked Songs"),
            )
        } else {
            (
                Icon::Heart,
                palette.secondary,
                gettext(app.locale, "Save to Liked Songs"),
            )
        };
        // Sit the heart just past the actual text, not at the region's far
        // edge, so it stays visually attached to the title.
        let natural = {
            let title =
                ui.painter()
                    .layout_no_wrap(now.title.clone(), theme::medium(14.0), palette.text);
            let subtitle = ui.painter().layout_no_wrap(
                now.subtitle.clone(),
                theme::regular(12.0),
                palette.secondary,
            );
            title.size().x.max(subtitle.size().x).min(text_width)
        };
        let heart_x = (text_left + natural + 21.0).min(region.right() - 21.0);
        let heart_rect = Rect::from_center_size(pos2(heart_x, cy), Vec2::splat(30.0));
        let mut heart_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(heart_rect)
                .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
        );
        if theme::icon_button(&mut heart_ui, icon, 17.0, color, palette.text, &tooltip).clicked() {
            app.actions.push(Action::ToggleSaved(now.uri.clone()));
        }
    }
}

fn transport(app: &mut App, ui: &mut egui::Ui, now: Option<&NowPlaying>, region: Rect) {
    let palette = app.palette;
    // Everything here is placed with explicit rects: egui's implicit rows
    // centre each widget in the row height known when it is added, which
    // left earlier icons riding high next to the play disc.
    //
    // The buttons row (36) and the progress row (~15, after a 6px gap) form
    // one cluster, centred as a group in the 88px bar: the buttons sit 8px
    // above the bar's midline and the progress row 23px below it. Measured
    // on screen this puts equal breathing room above and beneath the
    // cluster.
    let cy = region.center().y - 8.0;
    let enabled = now.is_some_and(|now| now.can_control) || app.is_connected();
    let playing = now.is_some_and(|now| now.playing);
    let loading = now.is_some_and(|now| now.loading);
    let shuffle = now.map_or_else(|| app.playing_context_shuffle(), |now| now.shuffle);
    let repeat = now.map(|now| now.repeat).unwrap_or_default();
    let dim = if enabled {
        palette.secondary
    } else {
        palette.dim
    };

    // Button widths: icon buttons occupy icon size + 12; the disc is 36.
    let widths = [29.0, 30.0, 36.0, 30.0, 29.0];
    let gap = 10.0;
    let total: f32 = widths.iter().sum::<f32>() + gap * 4.0;
    let mut x = region.center().x - total / 2.0;
    let mut slot = |width: f32| {
        let rect = Rect::from_center_size(pos2(x + width / 2.0, cy), vec2(width, 36.0));
        x += width + gap;
        rect
    };
    let centered = |ui: &mut egui::Ui, rect: Rect| {
        ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
        )
    };

    let shuffle_color = if shuffle { palette.accent } else { dim };
    let mut cell = centered(ui, slot(widths[0]));
    let shuffle_button = theme::icon_button(
        &mut cell,
        Icon::Shuffle,
        17.0,
        shuffle_color,
        if shuffle {
            palette.accent_hover
        } else {
            palette.text
        },
        &gettext(app.locale, "Shuffle"),
    );
    shuffle_button.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            cell.is_enabled(),
            shuffle,
            gettext(app.locale, "Shuffle"),
        )
    });
    if shuffle_button.clicked() {
        app.actions.push(Action::ToggleShuffle);
    }

    let mut cell = centered(ui, slot(widths[1]));
    if theme::icon_button(
        &mut cell,
        Icon::SkipBackFilled,
        18.0,
        dim,
        palette.text,
        &gettext(app.locale, "Previous"),
    )
    .clicked()
    {
        app.actions.push(Action::Previous);
    }

    let disc = slot(widths[2]);
    if loading || app.any_play_pending() {
        ui.painter()
            .circle_filled(disc.center(), 18.0, palette.text);
        let mut cell = centered(ui, disc);
        theme::spinner(&mut cell, 22.0, palette.window);
    } else {
        let icon = if playing {
            Icon::PauseFilled
        } else {
            Icon::PlayFilled
        };
        let hover = if palette.dark {
            egui::Color32::WHITE
        } else {
            palette.text
        };
        let mut cell = centered(ui, disc);
        if theme::circle_button(
            &mut cell,
            icon,
            36.0,
            palette.text,
            hover,
            palette.window,
            &if playing {
                gettext(app.locale, "Pause")
            } else {
                gettext(app.locale, "Play")
            },
        )
        .clicked()
        {
            app.actions.push(Action::TogglePlay);
        }
    }

    let mut cell = centered(ui, slot(widths[3]));
    if theme::icon_button(
        &mut cell,
        Icon::SkipForwardFilled,
        18.0,
        dim,
        palette.text,
        &gettext(app.locale, "Next"),
    )
    .clicked()
    {
        app.actions.push(Action::Next);
    }

    let (repeat_icon, repeat_color, tooltip) = match repeat {
        RepeatMode::Off => (Icon::Repeat, dim, gettext(app.locale, "Repeat")),
        RepeatMode::Context => (
            Icon::Repeat,
            palette.accent,
            gettext(app.locale, "Repeat one"),
        ),
        RepeatMode::Track => (
            Icon::Repeat1,
            palette.accent,
            gettext(app.locale, "Repeat off"),
        ),
    };
    let mut cell = centered(ui, slot(widths[4]));
    if theme::icon_button(
        &mut cell,
        repeat_icon,
        17.0,
        repeat_color,
        if repeat == RepeatMode::Off {
            palette.text
        } else {
            palette.accent_hover
        },
        &tooltip,
    )
    .clicked()
    {
        app.actions.push(Action::CycleRepeat);
    }

    // Progress row, just below the buttons (disc bottom + 6px gap + half of
    // the time text's line height).
    let row_cy = cy + 31.0;
    let slider_width = (region.width() - 120.0).clamp(120.0, 620.0);
    let (position, duration) = now
        .map(|now| (now.position_ms, now.duration_ms))
        .unwrap_or((0, 0));
    let shown_position = match app.seek_preview {
        Some(fraction) => (fraction * duration as f32) as u32,
        None => position,
    };
    let time_color = if now.is_some() {
        palette.secondary
    } else {
        palette.dim
    };
    let slider_left = region.center().x - slider_width / 2.0;
    ui.painter().text(
        pos2(slider_left - 8.0, row_cy),
        egui::Align2::RIGHT_CENTER,
        util::format_duration_ms(shown_position),
        theme::regular(11.5),
        time_color,
    );
    let slider_rect =
        Rect::from_center_size(pos2(region.center().x, row_cy), vec2(slider_width, 16.0));
    let mut slider_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(slider_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let fraction = if duration > 0 {
        position as f32 / duration as f32
    } else {
        0.0
    };
    match thin_slider(
        &mut slider_ui,
        &palette,
        egui::Id::new("seek-slider"),
        &gettext(app.locale, "Playback position (%)"),
        fraction,
        slider_width,
        None,
    ) {
        SliderEvent::Dragging(value) => app.seek_preview = Some(value),
        SliderEvent::Committed(value) => {
            app.seek_preview = None;
            if duration > 0 {
                app.actions
                    .push(Action::Seek((value * duration as f32) as u32));
            }
        }
        SliderEvent::None => {}
    }
    ui.painter().text(
        pos2(slider_left + slider_width + 8.0, row_cy),
        egui::Align2::LEFT_CENTER,
        util::format_duration_ms(duration),
        theme::regular(11.5),
        time_color,
    );
}

fn extras(app: &mut App, ui: &mut egui::Ui, now: Option<&NowPlaying>) {
    let palette = app.palette;
    ui.spacing_mut().item_spacing.x = 6.0;
    let volume = now
        .map(|now| now.volume_percent)
        .unwrap_or_else(|| crate::app::volume_to_percent(app.local.volume));
    let shown = match app.volume_preview {
        Some(fraction) => (fraction * 100.0).round() as u8,
        None => volume,
    };
    match thin_slider(
        ui,
        &palette,
        egui::Id::new("volume-slider"),
        &gettext(app.locale, "Volume (%)"),
        shown as f32 / 100.0,
        92.0,
        Some(0.05),
    ) {
        SliderEvent::Dragging(value) => {
            app.volume_preview = Some(value);
            // Local volume is cheap to apply continuously; remote goes on release.
            if now.is_none_or(|now| now.local) {
                app.actions
                    .push(Action::PreviewVolume((value * 100.0).round() as u8));
            }
        }
        SliderEvent::Committed(value) => {
            app.volume_preview = None;
            app.actions
                .push(Action::SetVolume((value * 100.0).round() as u8));
        }
        SliderEvent::None => {}
    }
    let volume_icon = match shown {
        0 => Icon::VolumeX,
        1..=33 => Icon::Volume,
        34..=66 => Icon::Volume1,
        _ => Icon::Volume2,
    };
    if theme::icon_button(
        ui,
        volume_icon,
        18.0,
        palette.secondary,
        palette.text,
        &if shown == 0 {
            gettext(app.locale, "Unmute")
        } else {
            gettext(app.locale, "Mute")
        },
    )
    .clicked()
    {
        app.actions.push(Action::ToggleMute);
    }
    ui.add_space(4.0);
    let remote = now.is_some_and(|now| !now.local);
    let devices = theme::icon_button(
        ui,
        Icon::Speaker,
        18.0,
        if remote {
            palette.accent
        } else {
            palette.secondary
        },
        palette.text,
        &gettext(app.locale, "Connect to a device"),
    );
    ui.ctx().data_mut(|data| {
        data.insert_temp(egui::Id::new(super::devices::BUTTON_RECT_ID), devices.rect)
    });
    if devices.clicked() {
        app.actions.push(Action::ToggleDevicesPopup);
    }
    let queue_open = app.show_queue_panel || matches!(app.page(), Page::Queue);
    let queue_button = theme::icon_button(
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
    );
    if queue_button.clicked() {
        app.actions.push(Action::ToggleQueuePanel);
    }
    // A dragged song dropped on the queue button queues it, same as the
    // "Add to queue" menu item.
    if let Some(track) = queue_button.dnd_release_payload::<DragTrack>() {
        app.actions.push(Action::QueueMany {
            songs: track
                .items
                .iter()
                .map(|item| (item.uri().to_string(), item.name().to_string()))
                .collect(),
        });
    }
    if theme::icon_button(
        ui,
        Icon::Mic,
        18.0,
        if app.show_lyrics_panel {
            palette.accent
        } else {
            palette.secondary
        },
        palette.text,
        &gettext(app.locale, "Lyrics"),
    )
    .clicked()
    {
        app.actions.push(Action::ToggleLyricsPanel);
    }
}

#[cfg(test)]
mod player_bar_tint_tests {
    use super::*;
    use crate::theme::Palette;

    /// Run one frame at `time` and report the bar's actual fill.
    fn frame(ctx: &egui::Context, time: f64, panel: Color32, tint: Option<Color32>) -> Color32 {
        let mut fill = Color32::PLACEHOLDER;
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                ..Default::default()
            },
            |ui| fill = eased_fill(ui.ctx(), panel, tint),
        );
        output.textures_delta.clear();
        fill
    }

    #[test]
    fn a_song_without_art_preserves_translucent_panel() {
        for panel in [
            Palette::dark().panel,
            Palette::light().panel,
            Color32::from_rgba_unmultiplied(80, 120, 180, 128),
        ] {
            let ctx = egui::Context::default();
            assert_eq!(frame(&ctx, 0.0, panel, None), panel);
            let art = Color32::from_rgb(200, 40, 90);
            frame(&ctx, 0.1, panel, Some(art));
            frame(&ctx, 0.6, panel, Some(art));
            assert_eq!(frame(&ctx, 0.7, panel, None).a(), panel.a());
            assert_eq!(frame(&ctx, 1.2, panel, None), panel);
        }
    }

    #[test]
    fn the_first_frame_shows_the_tint_without_fading_in() {
        let ctx = egui::Context::default();
        let panel = Palette::dark().panel;
        let art = Color32::from_rgb(200, 40, 90);
        assert_eq!(
            frame(&ctx, 0.0, panel, Some(art)),
            super::super::blend(panel, art, TINT_STRENGTH)
        );
    }

    #[test]
    fn first_art_after_an_untinted_frame_fades_in() {
        let ctx = egui::Context::default();
        let panel = Palette::dark().panel;
        let art = Color32::from_rgb(220, 140, 160);
        let target = super::super::blend(panel, art, TINT_STRENGTH);
        let fade = f64::from(TINT_FADE_SECONDS);

        assert_eq!(frame(&ctx, 0.0, panel, None), panel);
        assert_eq!(frame(&ctx, 0.1, panel, Some(art)), panel);
        let middle = frame(&ctx, 0.1 + fade / 2.0, panel, Some(art));
        assert_ne!(middle, panel);
        assert_ne!(middle, target);
        assert_eq!(frame(&ctx, 0.1 + fade, panel, Some(art)), target);
    }

    #[test]
    fn changing_songs_mid_fade_continues_from_the_visible_colour() {
        let ctx = egui::Context::default();
        let panel = Palette::dark().panel;
        let first = Color32::from_rgb(20, 40, 60);
        let second = Color32::from_rgb(220, 140, 160);
        let third = Color32::from_rgb(40, 230, 30);
        let first_fill = super::super::blend(panel, first, TINT_STRENGTH);
        let second_fill = super::super::blend(panel, second, TINT_STRENGTH);
        let third_fill = super::super::blend(panel, third, TINT_STRENGTH);
        let fade = f64::from(TINT_FADE_SECONDS);

        assert_eq!(frame(&ctx, 0.0, panel, Some(first)), first_fill);
        assert_eq!(frame(&ctx, 0.05, panel, Some(second)), first_fill);
        let middle = frame(&ctx, 0.2, panel, Some(second));
        assert_ne!(middle, first_fill);
        assert_ne!(middle, second_fill);
        assert_eq!(frame(&ctx, 0.2, panel, Some(third)), middle);
        assert_ne!(frame(&ctx, 0.2 + fade / 2.0, panel, Some(third)), middle);
        assert_eq!(frame(&ctx, 0.2 + fade, panel, Some(third)), third_fill);
    }

    #[test]
    fn a_new_session_does_not_reuse_the_previous_tint() {
        let ctx = egui::Context::default();
        let panel = Palette::dark().panel;
        let first = Color32::from_rgb(20, 40, 60);
        let second = Color32::from_rgb(220, 140, 160);
        let next_session = Color32::from_rgb(40, 230, 30);

        frame(&ctx, 0.0, panel, Some(first));
        frame(&ctx, 0.1, panel, Some(second));
        assert_ne!(
            frame(&ctx, 0.2, panel, Some(second)),
            super::super::blend(panel, second, TINT_STRENGTH)
        );

        end_tint_session(&ctx);
        assert_eq!(
            frame(&ctx, 0.21, panel, Some(next_session)),
            super::super::blend(panel, next_session, TINT_STRENGTH)
        );

        end_tint_session(&ctx);
        assert_eq!(frame(&ctx, 0.22, panel, None), panel);
    }

    /// However light or dark the cover, the visualizer's colours stay in the
    /// band the theme's text reads against, and keep their hue.
    #[test]
    fn visualizer_colours_stay_behind_the_words() {
        let luminance = |colour: Color32| {
            let linear = egui::Rgba::from(colour);
            0.2126 * linear.r() + 0.7152 * linear.g() + 0.0722 * linear.b()
        };
        for base in [
            Color32::from_rgb(255, 240, 80),
            Color32::from_rgb(20, 30, 90),
            Color32::WHITE,
            Color32::BLACK,
            Color32::from_rgb(30, 215, 96),
        ] {
            let (low, high) = vis_colours(base, true);
            for colour in [low, high] {
                assert!(
                    luminance(colour) <= DARK_CEILING + 0.01,
                    "{base:?} on dark: {colour:?}"
                );
            }
            let (low, high) = vis_colours(base, false);
            for colour in [low, high] {
                assert!(
                    (LIGHT_FLOOR - 0.01..=LIGHT_CEILING + 0.01).contains(&luminance(colour)),
                    "{base:?} on light: {colour:?}"
                );
            }
        }
        // A yellow cover stays yellow, only deeper.
        let (low, _) = vis_colours(Color32::from_rgb(255, 240, 80), true);
        assert!(low.r() > low.b() && low.g() > low.b(), "{low:?}");
    }
}
