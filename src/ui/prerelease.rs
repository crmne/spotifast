//! Native counterpart of Spotify's countdown page, inside Spotifast's shell.

use std::sync::Arc;
use std::time::Duration;

use egui::{Color32, Frame, Margin, Sense, Stroke, Vec2, vec2};

use super::widgets;
use crate::api::models::pick_image;
use crate::app::App;
use crate::i18n::gettext;
use crate::model::{Action, Page, RowContext};
use crate::prerelease::Prerelease;
use crate::theme::{self, Icon};

pub(super) fn show(app: &mut App, ui: &mut egui::Ui, data: &Prerelease) {
    let palette = app.palette;
    let locale = app.locale;
    let hero_text = if data.color.is_some() {
        Color32::WHITE
    } else {
        palette.text
    };
    let hero_secondary = if data.color.is_some() {
        Color32::from_white_alpha(210)
    } else {
        palette.secondary
    };
    let now = jiff::Timestamp::now().as_second();
    let wide = ui.available_width() > 700.0;
    let cover_size = if wide { 232.0 } else { 160.0 };
    if let Some([red, green, blue]) = data.color {
        let bounds = ui.max_rect().expand2(vec2(widgets::PAGE_PADDING, 4.0));
        let top = Color32::from_rgb(red, green, blue);
        let dark = Color32::from_rgb(
            (red as f32 * 0.40) as u8,
            (green as f32 * 0.40) as u8,
            (blue as f32 * 0.40) as u8,
        );
        widgets::paint_vertical_gradient(
            ui,
            egui::Rect::from_min_size(bounds.min, vec2(bounds.width(), cover_size + 104.0)),
            top,
            dark,
        );
        widgets::paint_vertical_gradient(
            ui,
            egui::Rect::from_min_size(
                bounds.min + vec2(0.0, cover_size + 104.0),
                vec2(bounds.width(), 300.0),
            ),
            dark,
            palette.window,
        );
    }
    ui.add_space(80.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 24.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(cover_size), Sense::hover());
        widgets::paint_cover(
            ui,
            &palette,
            pick_image(&data.album.images, 640),
            rect,
            4.0,
            Icon::Music,
            Some(app.backend.art()),
        );
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            theme::text(ui, gettext(locale, "Album"), theme::medium(14.0), hero_text);
            let mut title_size = if ui.available_width() > 1050.0 {
                72.0
            } else if wide {
                48.0
            } else {
                28.0
            };
            if ui.available_width() >= 900.0 {
                let measured = ui
                    .painter()
                    .layout_no_wrap(data.album.name.clone(), theme::bold(title_size), hero_text)
                    .size()
                    .x;
                title_size *= (ui.available_width() / measured.max(1.0)).min(1.0);
            }
            let title = crate::bidi::layout(
                ui.painter(),
                &data.album.name,
                theme::bold(title_size),
                hero_text,
                ui.available_width(),
                3,
                Some(crate::bidi::ELLIPSIS),
            );
            ui.add(egui::Label::new(title));
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if let Some(url) = &data.artist_image {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                    widgets::paint_cover(
                        ui,
                        &palette,
                        Some(url),
                        rect,
                        12.0,
                        Icon::User,
                        Some(app.backend.art()),
                    );
                }
                for artist in &data.album.artists {
                    if theme::link(ui, &artist.name, theme::semibold(13.0), hero_text).clicked()
                        && let Some(id) = &artist.id
                    {
                        app.actions.push(Action::Open(Page::Artist(id.clone())));
                    }
                }
                let date = data.album.release_date.clone().or_else(|| {
                    data.release_unix
                        .and_then(|seconds| jiff::Timestamp::from_second(seconds).ok())
                        .map(|timestamp| {
                            timestamp
                                .to_zoned(jiff::tz::TimeZone::system())
                                .strftime("%Y-%m-%d")
                                .to_string()
                        })
                });
                if let Some(date) = date {
                    theme::text(
                        ui,
                        format!(
                            "• {} {}",
                            gettext(locale, "Releases"),
                            crate::util::format_date(locale, &date)
                        ),
                        theme::regular(13.0),
                        hero_secondary,
                    );
                }
            });
            if let Some(parts) = data.countdown(now) {
                ui.add_space(12.0);
                countdown(ui, parts, locale, hero_text);
                ui.ctx().request_repaint_after(Duration::from_secs(1));
            }
        });
    });
    ui.add_space(48.0);
    ui.horizontal(|ui| {
        let green = Color32::from_rgb(0x1e, 0xd7, 0x60);
        let saved = data.saved == Some(true);
        let ink = if saved { palette.text } else { Color32::BLACK };
        let save_label = if data.saving {
            gettext(locale, "Saving…")
        } else if data.saved == Some(true) {
            gettext(locale, "Pre-saved")
        } else {
            gettext(locale, "Pre-save")
        };
        let enabled = data.saved.is_some() && !data.saving && app.local_ready;
        let text_color = if enabled {
            ink
        } else {
            ink.gamma_multiply(0.45)
        };
        let label = crate::bidi::layout_line(
            ui.painter(),
            save_label.as_ref(),
            theme::semibold(14.0),
            text_color,
        );
        let content_width = label.size().x + 8.0 + 16.0;
        let save_button = ui
            .add_enabled(
                enabled,
                egui::Button::new("")
                    .fill(if saved { palette.surface } else { green })
                    .stroke(if saved {
                        Stroke::new(1.0, palette.outline)
                    } else {
                        Stroke::NONE
                    })
                    .corner_radius(24)
                    .min_size(vec2(content_width + 48.0, 48.0)),
            )
            .on_hover_text(gettext(locale, "Pre-save on Spotify"));
        save_button.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, save_label.as_ref())
        });
        let label_left = save_button.rect.center().x - content_width / 2.0;
        let icon_left = label_left + label.size().x + 8.0;
        ui.painter().galley(
            egui::pos2(
                label_left,
                save_button.rect.center().y - label.size().y / 2.0,
            ),
            label,
            text_color,
        );
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(icon_left, save_button.rect.center().y - 8.0),
            Vec2::splat(16.0),
        );
        theme::paint_icon(
            ui,
            if saved {
                Icon::CircleCheck
            } else {
                Icon::CirclePlus
            },
            icon_rect,
            16.0,
            if saved { green } else { ink },
        );
        if save_button.clicked() {
            app.actions.push(Action::SetPrereleaseSaved {
                uri: data.uri.clone(),
                saved: data.saved != Some(true),
            });
        }
        ui.add_space(12.0);
        ui.scope(|ui| {
            ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
            ui.visuals_mut().widgets.inactive.bg_fill = Color32::TRANSPARENT;
            ui.menu_button("•••", |ui| {
                if ui.button(gettext(locale, "Copy link")).clicked() {
                    app.actions.push(Action::CopyLink(data.uri.clone()));
                    ui.close();
                }
                if ui.button(gettext(locale, "Open in Spotify")).clicked() {
                    let id = data.uri.rsplit(':').next().unwrap_or_default();
                    app.actions.push(Action::OpenUrl(format!(
                        "https://open.spotify.com/prerelease/{id}?spotifast_web=1"
                    )));
                    ui.close();
                }
            });
        });
    });
    if let Some(error) = &data.save_error {
        theme::text(ui, error, theme::regular(13.0), palette.secondary);
        if ui.button(gettext(locale, "Retry")).clicked() {
            app.actions.push(Action::OpenLink(data.uri.clone()));
        }
    }
    ui.add_space(28.0);
    theme::text(
        ui,
        gettext(locale, "Track list preview"),
        theme::bold(24.0),
        palette.text,
    );
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.add_space(18.0);
        theme::text(ui, "#", theme::regular(13.0), palette.secondary);
        ui.add_space(16.0);
        theme::text(
            ui,
            crate::i18n::pgettext(locale, "column", "Title"),
            theme::regular(13.0),
            palette.secondary,
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            theme::icon(ui, Icon::Clock, 16.0, palette.secondary);
            ui.add_space(30.0);
        });
    });
    ui.separator();
    let playable: Arc<[String]> = data
        .tracks
        .iter()
        .flatten()
        .filter(|track| track.is_playable == Some(true))
        .map(|track| track.uri.clone())
        .collect();
    for (index, track) in data.tracks.iter().enumerate() {
        let enabled = track
            .as_ref()
            .is_some_and(|track| track.is_playable == Some(true));
        let color = if enabled { palette.text } else { palette.dim };
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), 56.0),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if response.hovered() {
            ui.painter().rect_filled(rect, 4.0, palette.surface_hover);
        }
        if response.clicked() {
            response.request_focus();
        }
        let mut play_clicked = false;
        if enabled && response.contains_pointer() {
            let button = egui::Rect::from_center_size(
                rect.left_center() + vec2(20.0, 0.0),
                Vec2::splat(24.0),
            );
            theme::paint_icon(ui, Icon::PlayFilled, button, 16.0, palette.text);
            let play = ui.interact(
                button,
                ui.make_persistent_id(("prerelease-play", &data.uri, index)),
                Sense::click(),
            );
            play.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, gettext(locale, "Play"))
            });
            play_clicked = play.clicked();
        } else {
            ui.painter().text(
                rect.left_center() + vec2(20.0, 0.0),
                egui::Align2::CENTER_CENTER,
                (index + 1).to_string(),
                theme::regular(14.0),
                palette.secondary,
            );
        }
        let mut name = track
            .as_ref()
            .map(|track| track.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                gettext(locale, "Track {number}").replace("{number}", &(index + 1).to_string())
            });
        if !enabled
            && track.as_ref().is_some_and(|track| {
                track.duration_ms == 0 && track.name == format!("Track {}", index + 1)
            })
        {
            name = gettext(locale, "Track {number}").replace("{number}", &(index + 1).to_string());
        }
        let artist = track
            .as_ref()
            .map(|track| track.artist_names())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| {
                data.album
                    .artists
                    .iter()
                    .map(|artist| artist.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            });
        let title_rect = egui::Rect::from_min_size(
            rect.min + vec2(48.0, 9.0),
            vec2((rect.width() - 132.0).max(1.0), 42.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(title_rect), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(title_rect));
            ui.spacing_mut().item_spacing.y = 1.0;
            theme::text(ui, name, theme::medium(15.0), color);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 1.0;
                if track.as_ref().is_some_and(|track| track.explicit) {
                    ui.add(egui::Label::new(
                        egui::RichText::new("E")
                            .font(theme::medium(9.0))
                            .color(palette.window)
                            .background_color(palette.secondary),
                    ));
                }
                let artists = track
                    .as_ref()
                    .map(|track| &track.artists)
                    .unwrap_or(&data.album.artists);
                if artists.is_empty() {
                    theme::text(ui, artist, theme::regular(13.0), palette.secondary);
                }
                for (number, artist) in artists.iter().enumerate() {
                    if number > 0 {
                        theme::text(ui, ",", theme::regular(13.0), palette.secondary);
                    }
                    let color = if enabled {
                        palette.secondary
                    } else {
                        palette.dim
                    };
                    if let Some(id) = &artist.id {
                        if theme::link(ui, &artist.name, theme::regular(13.0), color).clicked() {
                            app.actions.push(Action::Open(Page::Artist(id.clone())));
                        }
                    } else {
                        theme::text(ui, &artist.name, theme::regular(13.0), color);
                    }
                }
            });
        });
        let duration = track
            .as_ref()
            .filter(|track| track.duration_ms > 0)
            .map(|track| {
                let seconds = track.duration_ms / 1000;
                format!("{}:{:02}", seconds / 60, seconds % 60)
            })
            .unwrap_or_else(|| "-:--".into());
        ui.painter().text(
            rect.right_center() - vec2(30.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            duration,
            theme::regular(14.0),
            if enabled {
                palette.secondary
            } else {
                palette.dim
            },
        );
        if enabled
            && (play_clicked
                || response.double_clicked()
                || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))))
            && let Some(track) = track
        {
            let start = data
                .tracks
                .iter()
                .take(index)
                .flatten()
                .filter(|track| track.is_playable == Some(true))
                .count();
            app.actions.push(Action::PlayFromRow {
                context: RowContext::View {
                    uris: Arc::clone(&playable),
                    context_uri: data.album.uri.clone(),
                    editable_playlist: None,
                },
                uri: track.uri.clone(),
                index: start as u32,
            });
        }
    }
    ui.add_space(24.0);
    for copyright in &data.album.copyrights {
        theme::text(ui, &copyright.text, theme::regular(11.5), palette.dim);
    }
}

fn countdown(ui: &mut egui::Ui, parts: [i64; 4], locale: crate::i18n::Locale, color: Color32) {
    let column_width = ((ui.available_width() - 27.0) / 4.0).clamp(36.0, 78.0);
    let values = parts.into_iter().zip([
        gettext(locale, "days"),
        gettext(locale, "hours"),
        gettext(locale, "minutes"),
        gettext(locale, "seconds"),
    ]);
    Frame::new()
        .fill(Color32::from_black_alpha(105))
        .corner_radius(6)
        .inner_margin(Margin::symmetric(12, 12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (index, (number, label)) in values.enumerate() {
                    if index > 0 {
                        let (rect, _) = ui.allocate_exact_size(vec2(1.0, 24.0), Sense::hover());
                        ui.painter().line_segment(
                            [rect.center_top(), rect.center_bottom()],
                            Stroke::new(1.0, Color32::from_white_alpha(65)),
                        );
                    }
                    ui.allocate_ui_with_layout(
                        vec2(column_width, 48.0),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            theme::text(ui, number.to_string(), theme::bold(24.0), color);
                            theme::text(ui, label, theme::regular(11.0), color.gamma_multiply(0.7));
                        },
                    );
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::Track;
    use crate::app::AppOptions;
    use crate::paths::AppDirs;
    use crate::settings::Settings;

    fn texts(shape: &egui::epaint::Shape, found: &mut Vec<egui::Rect>) {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == "Repeated track" => {
                found.push(text.galley.rect.translate(text.pos.to_vec2()));
            }
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    texts(shape, found);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn clicking_a_repeated_preview_track_starts_that_occurrence() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-prerelease-occurrence-{}",
            std::process::id()
        ));
        let ctx = egui::Context::default();
        let waker = crate::backend::Waker::default();
        waker.attach(&ctx);
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
        app.attach(&ctx);
        app.local_ready = true;
        let mut data = Prerelease::decode(
            include_bytes!("../testdata/prerelease.pb"),
            "spotify:prerelease:0kRaNkRxpO16BjxJU0IQAL",
        )
        .unwrap();
        data.album.images.clear();
        data.artist_image = None;
        data.release_unix = None;
        let track = |uri: &str, name: &str, playable| {
            Some(Track {
                uri: uri.into(),
                name: name.into(),
                is_playable: Some(playable),
                ..Default::default()
            })
        };
        data.tracks = vec![
            None,
            track("spotify:track:repeated", "Repeated track", true),
            track("spotify:track:hidden", "Unavailable track", false),
            track("spotify:track:other", "Other track", true),
            track("spotify:track:repeated", "Repeated track", true),
        ];
        let mut frame = |time, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        vec2(1600.0, 1400.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| show(&mut app, ui, &data),
            );
            output.textures_delta.clear();
            output
        };
        let _ = frame(0.0, vec![]);
        let output = frame(0.1, vec![]);
        let mut matches = vec![];
        for shape in &output.shapes {
            texts(&shape.shape, &mut matches);
        }
        matches.sort_by(|a, b| a.top().total_cmp(&b.top()));
        assert_eq!(matches.len(), 2, "both occurrences must be rendered");
        let last = matches.last().unwrap();
        let point = egui::pos2(last.left() - 28.0, last.top() + 19.0);
        let _ = frame(0.2, vec![egui::Event::PointerMoved(point)]);
        for (time, pressed) in [(0.3, true), (0.4, false)] {
            let _ = frame(
                time,
                vec![egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                }],
            );
        }
        let plays: Vec<_> = app
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::PlayFromRow {
                    index,
                    context: RowContext::View { uris, .. },
                    ..
                } => Some((*index, uris.to_vec())),
                _ => None,
            })
            .collect();
        app.backend.shutdown();
        assert_eq!(plays.len(), 1, "one click emits one playback action");
        assert_eq!(plays[0].0, 2, "start at the clicked second occurrence");
        assert_eq!(
            plays[0].1,
            vec![
                "spotify:track:repeated",
                "spotify:track:other",
                "spotify:track:repeated"
            ]
        );
    }
}
