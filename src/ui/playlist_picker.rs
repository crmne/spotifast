use egui::{CornerRadius, Rect, Sense, Ui, Vec2, pos2, vec2};

use crate::api::models::{PlayableItem, pick_image};
use crate::app::App;
use crate::i18n::gettext;
use crate::model::{Action, Dialog};
use crate::theme::{self, Icon};

use super::widgets;

pub fn show(ui: &mut Ui, app: &mut App, item: &PlayableItem, query: &mut String) -> egui::Response {
    let palette = app.palette;
    let locale = app.locale;
    ui.set_width(300.0);
    theme::subtle(ui, &palette, &gettext(locale, "Add to playlist"));
    let field_id = ui.make_persistent_id("playlist-filter");
    let choice_id = ui.make_persistent_id("playlist-choice");
    let frame = ui.ctx().cumulative_frame_nr();
    let (previous_query, mut chosen) = ui
        .data(|data| data.get_temp::<(u64, String, Option<String>)>(choice_id))
        .filter(|(last, _, _)| frame.saturating_sub(*last) <= 1)
        .map(|(_, query, chosen)| (query, chosen))
        .unwrap_or_default();
    let mut playlists: Vec<_> = app
        .library
        .playlists
        .get()
        .into_iter()
        .flatten()
        .filter(|playlist| app.can_edit_playlist(playlist))
        .cloned()
        .collect();
    playlists.sort_by(|left, right| app.playlist_picker.compare_updated(&left.id, &right.id));
    let choices: Vec<_> = playlists
        .iter()
        .filter(|playlist| {
            playlist
                .name
                .to_lowercase()
                .contains(&query.trim().to_lowercase())
        })
        .filter(|playlist| {
            app.playlist_picker
                .entries
                .get(&playlist.id)
                .and_then(|entry| entry.contains)
                != Some(true)
        })
        .map(|playlist| playlist.id.clone())
        .collect();
    let count = choices.len();
    if chosen.as_ref().is_some_and(|id| !choices.contains(id)) {
        chosen = None;
    }
    let mut moved = false;
    let mut enter = false;
    if ui.memory(|memory| memory.has_focus(field_id)) {
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) && count > 0 {
                let index = chosen
                    .as_ref()
                    .and_then(|id| choices.iter().position(|choice| choice == id));
                chosen = Some(choices[index.map_or(0, |index| (index + 1).min(count - 1))].clone());
                moved = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) && count > 0 {
                let index = chosen
                    .as_ref()
                    .and_then(|id| choices.iter().position(|choice| choice == id));
                chosen =
                    Some(choices[index.map_or(count - 1, |index| index.saturating_sub(1))].clone());
                moved = true;
            }
            enter = chosen.is_some() && input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        });
    }
    let field = widgets::search_field(
        ui,
        &palette,
        locale,
        field_id,
        query,
        &gettext(locale, "Filter playlists"),
        ui.available_width(),
    );
    if *query != previous_query {
        chosen = None;
    }
    // Apply text typed in this pass before drawing or choosing a playlist.
    playlists.retain(|playlist| {
        playlist
            .name
            .to_lowercase()
            .contains(&query.trim().to_lowercase())
    });
    if *query != previous_query && !query.trim().is_empty() {
        chosen = playlists
            .iter()
            .find(|playlist| {
                app.playlist_picker
                    .entries
                    .get(&playlist.id)
                    .and_then(|entry| entry.contains)
                    != Some(true)
            })
            .map(|playlist| playlist.id.clone());
    }
    ui.data_mut(|data| data.insert_temp(choice_id, (frame, query.clone(), chosen.clone())));
    if widgets::menu_item(
        ui,
        &palette,
        Some(Icon::Plus),
        &gettext(locale, "New playlist"),
    ) {
        app.actions.push(Action::ShowDialog(Dialog::CreatePlaylist {
            name: String::new(),
            public: false,
            add_uris: vec![item.uri().to_string()],
        }));
    }
    widgets::menu_separator(ui, &palette);
    app.actions.push(Action::ReadPlaylistMembership);
    egui::ScrollArea::vertical()
        .id_salt("player-playlist-picker")
        .max_height(320.0)
        .min_scrolled_height(320.0)
        .show(ui, |ui| {
            theme::subtle(ui, &palette, &gettext(locale, "Saved in"));
            let liked = gettext(locale, "Liked Songs");
            if liked.to_lowercase().contains(&query.trim().to_lowercase()) {
                let count = app
                    .library
                    .liked
                    .total
                    .map(|total| locale.song_count(total))
                    .unwrap_or_default();
                let response = row(ui, app, (&liked, &count), None, Some(true), true, false)
                    .on_hover_text(gettext(locale, "Remove from Liked Songs"));
                if response.clicked() {
                    app.actions
                        .push(Action::ToggleSaved(item.uri().to_string()));
                    ui.close();
                }
            }
            if playlists.is_empty() {
                theme::subtle(
                    ui,
                    &palette,
                    &if query.trim().is_empty() {
                        gettext(locale, "No editable playlists")
                    } else {
                        gettext(locale, "No matching playlists")
                    },
                );
            }
            for saved in [true, false] {
                if !saved {
                    let has_dates = playlists.iter().any(|playlist| {
                        app.playlist_picker
                            .entries
                            .get(&playlist.id)
                            .is_some_and(|entry| entry.updated_at_ms.is_some())
                    });
                    theme::subtle(
                        ui,
                        &palette,
                        &if has_dates {
                            gettext(locale, "Recently updated")
                        } else {
                            gettext(locale, "Playlists")
                        },
                    );
                }
                for playlist in &playlists {
                    let entry = (app.playlist_picker.uri == item.uri())
                        .then(|| app.playlist_picker.entries.get(&playlist.id))
                        .flatten();
                    let contains = entry.and_then(|entry| entry.contains);
                    if (contains == Some(true)) != saved {
                        continue;
                    }
                    let highlighted = !saved && chosen.as_deref() == Some(playlist.id.as_str());
                    let count = locale.song_count(playlist.track_total());
                    let response = ui
                        .push_id(&playlist.id, |ui| {
                            row(
                                ui,
                                app,
                                (&playlist.name, &count),
                                pick_image(&playlist.images, 64),
                                contains,
                                false,
                                highlighted,
                            )
                        })
                        .inner;
                    if contains == Some(true) {
                        response
                            .clone()
                            .on_hover_text(gettext(locale, "Song already in this playlist"));
                    }
                    if let Some(error) = entry.and_then(|entry| entry.error.as_ref()) {
                        response.clone().on_hover_text(error);
                    } else if contains.is_none() {
                        response.clone().on_hover_text(gettext(locale, "Checking…"));
                    }
                    if highlighted && moved {
                        response.scroll_to_me(None);
                    }
                    if (response.clicked() || (highlighted && enter)) && contains != Some(true) {
                        app.actions.push(Action::AddToPlaylist {
                            playlist_id: playlist.id.clone(),
                            playlist_name: playlist.name.clone(),
                            items: vec![item.clone()],
                        });
                        ui.close();
                    }
                }
            }
        });
    field
}

fn row(
    ui: &mut Ui,
    app: &App,
    labels: (&str, &str),
    cover: Option<&str>,
    checked: Option<bool>,
    liked: bool,
    highlighted: bool,
) -> egui::Response {
    let (name, count) = labels;
    let palette = app.palette;
    let enabled = liked || checked != Some(true);
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), 60.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name);
        info.selected = checked;
        info
    });
    if ui.is_rect_visible(rect) {
        if (response.hovered() || highlighted) && enabled {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(4), palette.surface_hover);
        }
        let cover_rect = Rect::from_min_size(rect.min + vec2(6.0, 8.0), Vec2::splat(44.0));
        if liked {
            super::sidebar::liked_cover(ui, cover_rect, 4.0);
        } else {
            widgets::paint_cover(
                ui,
                &palette,
                cover,
                cover_rect,
                4.0,
                Icon::Music,
                Some(app.backend.art()),
            );
        }
        let text_rect = Rect::from_min_max(
            pos2(cover_rect.right() + 12.0, rect.top() + 10.0),
            pos2(rect.right() - 30.0, rect.bottom()),
        );
        for (text, font, color, y) in [
            (name, theme::regular(15.0), palette.text, 0.0),
            (count, theme::regular(12.0), palette.secondary, 23.0),
        ] {
            let galley = crate::bidi::layout(
                ui.painter(),
                text,
                font,
                color,
                text_rect.width().max(0.0),
                1,
                Some(crate::bidi::ELLIPSIS),
            );
            ui.painter().galley(
                crate::bidi::galley_pos(text_rect.translate(vec2(0.0, y)), &galley),
                galley,
                color,
            );
        }
        let status = Rect::from_center_size(
            pos2(rect.right() - 14.0, rect.center().y),
            Vec2::splat(18.0),
        );
        match checked {
            Some(true) => Icon::CircleCheck
                .image(palette.accent, 18.0)
                .paint_at(ui, status),
            Some(false) => {
                ui.painter().circle_stroke(
                    status.center(),
                    8.0,
                    egui::Stroke::new(1.0, palette.secondary),
                );
            }
            None => {
                ui.painter()
                    .circle_filled(status.center(), 2.0, palette.secondary);
            }
        }
    }
    theme::focus_ring(ui, &response);
    response
}
