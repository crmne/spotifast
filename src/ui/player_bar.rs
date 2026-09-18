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

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    // Without a tint the bar is the plain panel, which is also what the fade
    // eases back to when playback stops or the art has no colour yet.
    let target = app.now_playing_tint().unwrap_or(palette.panel);
    let fill = super::blend(palette.panel, eased_tint(ui.ctx(), target), TINT_STRENGTH);
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
            ui.painter().hline(
                rect.x_range(),
                rect.top() + 0.5,
                egui::Stroke::new(1.0, palette.outline),
            );
            let now = app.now_playing();
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

/// Ease the art tint so the bar crosses over to a new song's colour instead of
/// switching in a single frame. Each channel animates on its own id, and egui
/// restarts an animation from the value on screen, so skipping songs mid-fade
/// carries on from the colour the listener is looking at rather than jumping
/// back to the one it came from.
fn eased_tint(ctx: &egui::Context, target: Color32) -> Color32 {
    let channel = |axis: &'static str, value: u8| {
        ctx.animate_value_with_time(
            egui::Id::new(("player-bar-tint", axis)),
            f32::from(value),
            TINT_FADE_SECONDS,
        )
        .round() as u8
    };
    Color32::from_rgb(
        channel("r", target.r()),
        channel("g", target.g()),
        channel("b", target.b()),
    )
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
            egui::CursorIcon::PointingHand
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
        let expand = ui
            .interact(
                expand_rect,
                egui::Id::new("now-playing-art-expand"),
                Sense::click(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
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
    let shuffle = now.is_some_and(|now| now.shuffle);
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
        app.actions.push(Action::ToggleQueuePanel);
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

    /// Run one frame at `time` and report the tint the bar would fill with.
    fn frame(ctx: &egui::Context, time: f64, target: Color32) -> Color32 {
        let mut tint = Color32::PLACEHOLDER;
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                ..Default::default()
            },
            |ui| tint = eased_tint(ui.ctx(), target),
        );
        output.textures_delta.clear();
        tint
    }

    #[test]
    fn a_song_without_art_leaves_the_panel_untouched() {
        // The fade eases toward the panel itself when there is no tint, so the
        // blend has to land back on exactly the colour it started from.
        for panel in [Palette::dark().panel, Palette::light().panel] {
            assert_eq!(super::super::blend(panel, panel, TINT_STRENGTH), panel);
        }
    }

    #[test]
    fn the_first_frame_shows_the_tint_without_fading_in() {
        let ctx = egui::Context::default();
        let art = Color32::from_rgb(200, 40, 90);
        assert_eq!(frame(&ctx, 0.0, art), art);
    }

    #[test]
    fn changing_songs_crosses_over_to_the_new_tint() {
        let ctx = egui::Context::default();
        let first = Color32::from_rgb(20, 40, 60);
        let second = Color32::from_rgb(220, 140, 160);
        let fade = f64::from(TINT_FADE_SECONDS);

        // The first song's colour stands on its own from the frame it arrives.
        assert_eq!(frame(&ctx, 0.0, first), first);

        // The frame the song changes on still shows the colour on screen, since
        // egui restarts the fade from there instead of snapping to the new tint.
        assert_eq!(frame(&ctx, fade, second), first);

        // Partway through, the bar carries neither song's colour.
        let middle = frame(&ctx, fade * 1.5, second);
        for channel in 0..3 {
            let (from, to, mid) = (
                first.to_array()[channel],
                second.to_array()[channel],
                middle.to_array()[channel],
            );
            assert!(
                from < mid && mid < to,
                "channel {channel} should be between {from} and {to}, got {mid}",
            );
        }

        // And once it is over the new song's colour stands on its own.
        assert_eq!(frame(&ctx, fade * 3.0, second), second);
    }
}
