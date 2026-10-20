//! One staged membership editor for song rows, the player and context menus.

use egui::{Align, Color32, CornerRadius, Frame, Layout, Margin, Rect, Sense, Stroke, Ui, vec2};

use crate::api::models::pick_image;
use crate::app::App;
use crate::i18n::gettext;
use crate::model::Action;
use crate::playlist_membership::{Draft, LIKED};
use crate::theme::{self, Icon};

pub const GREEN: Color32 = Color32::from_rgb(30, 215, 96);

/// Spotify's membership mark: a black tick inside a solid green disc.
pub fn paint_mark(ui: &Ui, rect: Rect, checked: bool, partial: bool, color: Color32) {
    let center = rect.center();
    if checked || partial {
        ui.painter().circle_filled(center, 8.5, GREEN);
        theme::paint_icon(
            ui,
            if partial { Icon::Minus } else { Icon::Check },
            rect,
            12.0,
            Color32::BLACK,
        );
    } else {
        ui.painter()
            .circle_stroke(center, 8.0, Stroke::new(1.4, color));
    }
}

/// The hit target stays generous even though the glyph is only 18 points.
pub fn song_button(
    ui: &mut Ui,
    app: &mut App,
    item: &crate::api::models::PlayableItem,
    visible: bool,
) {
    let checked = app.in_any_playlist(item.uri());
    let label = if checked {
        gettext(app.locale, "Add to playlist")
    } else {
        gettext(app.locale, "Save to Liked Songs")
    };
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), &label)
    });
    if checked || visible || response.has_focus() || response.hovered() {
        if checked {
            paint_mark(ui, rect, true, false, app.palette.secondary);
        } else {
            theme::paint_icon(ui, Icon::CirclePlus, rect, 18.0, app.palette.secondary);
        }
    }
    theme::focus_ring(ui, &response);
    if response.clicked() {
        app.actions.push(if checked {
            Action::OpenPlaylistPicker(vec![item.clone()])
        } else {
            Action::SetSavedMany {
                uris: vec![item.uri().into()],
                saved: true,
            }
        });
    }
    response.on_hover_text(label);
}

struct Row {
    id: String,
    name: String,
    cover: Option<String>,
    folder: Option<String>,
    count: Option<u32>,
    pinned: Option<usize>,
    editable: bool,
    membership: Option<Vec<bool>>,
    error: bool,
}

fn rows(app: &App, draft: &Draft) -> Vec<Row> {
    let mut folders = std::collections::HashMap::new();
    let mut path = Vec::new();
    for entry in &app.rootlist {
        match entry {
            crate::player::RootlistEntry::FolderStart { name, .. } => path.push(name.clone()),
            crate::player::RootlistEntry::FolderEnd => {
                path.pop();
            }
            crate::player::RootlistEntry::Playlist(uri) if !path.is_empty() => {
                folders.insert(uri.clone(), path.join(" / "));
            }
            _ => {}
        }
    }
    let needle = draft.query.trim().to_lowercase();
    let mut rows: Vec<_> = app
        .library
        .playlists
        .get()
        .into_iter()
        .flatten()
        .map(|playlist| Row {
            id: playlist.id.clone(),
            name: playlist.name.clone(),
            cover: pick_image(&playlist.images, 64).map(str::to_string),
            folder: folders.get(&playlist.uri).cloned(),
            count: (playlist.items_count.is_some() || playlist.tracks.is_some())
                .then(|| playlist.track_total()),
            pinned: app
                .settings
                .pinned_contexts
                .iter()
                .position(|uri| uri == &playlist.uri),
            editable: app.can_edit_playlist(playlist),
            membership: draft
                .originals
                .get(&playlist.id)
                .cloned()
                .or_else(|| app.picker_membership(&playlist.id, &draft.items)),
            error: app.playlist_membership.error(&playlist.id).is_some(),
        })
        .filter(|row| {
            row.name.to_lowercase().contains(&needle)
                || row
                    .folder
                    .as_ref()
                    .is_some_and(|folder| folder.to_lowercase().contains(&needle))
        })
        .collect();
    // Stable groups and pin priority. A staged toggle never moves its row.
    rows.sort_by_key(|row| {
        (
            match &row.membership {
                Some(values) if values.iter().any(|value| *value) => 0,
                Some(_) => 1,
                None => 2,
            },
            row.pinned.unwrap_or(usize::MAX),
        )
    });
    let liked = gettext(app.locale, "Liked Songs").into_owned();
    if liked.to_lowercase().contains(&needle) {
        rows.insert(
            0,
            Row {
                id: LIKED.into(),
                name: liked,
                cover: None,
                folder: None,
                count: app.library.liked.total,
                pinned: None,
                editable: true,
                membership: draft
                    .originals
                    .get(LIKED)
                    .cloned()
                    .or_else(|| app.picker_membership(LIKED, &draft.items)),
                error: draft.liked_error,
            },
        );
    }
    rows
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut draft) = app.playlist_picker.clone() else {
        return;
    };
    let palette = app.palette;
    let locale = app.locale;
    let mut cancel = false;
    let mut commit = false;
    let mut retry = false;
    let response = egui::Modal::new(egui::Id::new("playlist-membership-picker"))
        .frame(
            Frame::new()
                .fill(palette.overlay)
                .stroke(Stroke::new(1.0, palette.outline))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(16))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 8],
                    blur: 32,
                    spread: 0,
                    color: palette.shadow,
                }),
        )
        .backdrop_color(Color32::from_black_alpha(if palette.dark {
            110
        } else {
            45
        }))
        .show(ctx, |ui| {
            ui.set_width(376.0f32.min((ctx.content_rect().width() - 64.0).max(220.0)));
            ui.spacing_mut().item_spacing.y = 8.0;
            theme::text(
                ui,
                gettext(locale, "Add to playlist"),
                theme::bold(20.0),
                palette.text,
            );
            let subtitle = if draft.items.len() == 1 {
                draft.items[0].name().to_string()
            } else {
                locale.song_count(draft.items.len() as u32)
            };
            theme::text(ui, subtitle, theme::regular(13.0), palette.secondary);
            ui.add_space(3.0);
            let search_id = ui.make_persistent_id("membership-filter");
            let previous_query = draft.query.clone();
            // Take navigation before the single-line field consumes Enter or moves its caret.
            let (down, up, toggle_key) = if ui.memory(|memory| memory.has_focus(search_id)) {
                ui.input_mut(|input| {
                    (
                        input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                        input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                        input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    )
                })
            } else {
                (false, false, false)
            };
            let field = super::widgets::search_field(
                ui,
                &palette,
                locale,
                search_id,
                &mut draft.query,
                &gettext(locale, "Find a playlist"),
                ui.available_width(),
            );
            if draft.focus_search {
                field.request_focus();
                draft.focus_search = false;
            }
            let rows = rows(app, &draft);
            let navigable: Vec<_> = rows
                .iter()
                .filter(|row| row.editable && row.membership.is_some())
                .map(|row| row.id.clone())
                .collect();
            if draft.query != previous_query {
                draft.highlighted = navigable.first().cloned();
            }
            let move_focus = (down || up) && !navigable.is_empty();
            if move_focus {
                let index = draft
                    .highlighted
                    .as_ref()
                    .and_then(|id| navigable.iter().position(|row| row == id));
                let next = if down {
                    index.map_or(0, |index| (index + 1).min(navigable.len() - 1))
                } else {
                    index.map_or(navigable.len() - 1, |index| index.saturating_sub(1))
                };
                draft.highlighted = Some(navigable[next].clone());
            }
            if let Some(name) = draft.new_name.as_mut() {
                let mut remove_new = false;
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(name)
                            .id(ui.make_persistent_id("new-playlist-name"))
                            .hint_text(gettext(locale, "Playlist name"))
                            .desired_width(ui.available_width() - 35.0),
                    );
                    if theme::icon_button(
                        ui,
                        Icon::X,
                        16.0,
                        palette.secondary,
                        palette.text,
                        &gettext(locale, "Cancel"),
                    )
                    .clicked()
                    {
                        remove_new = true;
                    }
                });
                if remove_new {
                    draft.new_name = None;
                }
            } else if theme::soft_button(
                ui,
                &palette,
                Some(Icon::Plus),
                &gettext(locale, "New playlist"),
                false,
            )
            .clicked()
            {
                draft.new_name = Some(String::new());
                ui.memory_mut(|memory| {
                    memory.request_focus(ui.make_persistent_id("new-playlist-name"))
                });
            }
            super::widgets::menu_separator(ui, &palette);
            let retry_height = if rows.iter().any(|row| row.error) {
                40.0
            } else {
                0.0
            };
            let max_height =
                (ctx.content_rect().height() - 300.0 - retry_height).clamp(80.0, 390.0);
            let mut previous_group = None;
            crate::autoscroll::show(
                ui,
                egui::ScrollArea::vertical()
                    .id_salt("membership-playlists")
                    .max_height(max_height)
                    .auto_shrink([false, true]),
                egui::Vec2b::new(false, true),
                |ui| {
                    if rows.is_empty() {
                        ui.add_space(16.0);
                        theme::subtle(ui, &palette, &gettext(locale, "No matching playlists"));
                        ui.add_space(16.0);
                    }
                    for row in &rows {
                        if row.id != LIKED {
                            let group = match &row.membership {
                                Some(values) if values.iter().any(|value| *value) => 0,
                                Some(_) => 1,
                                None => 2,
                            };
                            if previous_group != Some(group) {
                                if previous_group.is_some() {
                                    super::widgets::menu_separator(ui, &palette);
                                }
                                ui.add_space(4.0);
                                let title = match group {
                                    0 => gettext(locale, "Already in"),
                                    1 => gettext(locale, "Other playlists"),
                                    _ => gettext(locale, "Checking membership"),
                                };
                                theme::text(ui, title, theme::bold(12.0), palette.secondary);
                                previous_group = Some(group);
                            }
                        }
                        let selected = draft.choices.get(&row.id).copied();
                        let count = row
                            .membership
                            .as_ref()
                            .map(|values| values.iter().filter(|value| **value).count());
                        let checked = selected.unwrap_or(count == Some(draft.items.len()));
                        let partial = selected.is_none()
                            && count.is_some_and(|count| count > 0 && count < draft.items.len());
                        let enabled = row.editable && row.membership.is_some();
                        ui.push_id(&row.id, |ui| {
                            let (rect, response) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 56.0),
                                if enabled {
                                    Sense::click()
                                } else {
                                    Sense::hover()
                                },
                            );
                            response.widget_info(|| {
                                if partial {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Checkbox,
                                        enabled,
                                        &row.name,
                                    )
                                } else {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::Checkbox,
                                        enabled,
                                        checked,
                                        &row.name,
                                    )
                                }
                            });
                            if response.hovered()
                                || response.has_focus()
                                || draft.highlighted.as_ref() == Some(&row.id)
                            {
                                ui.painter().rect_filled(rect, 6.0, palette.surface_hover);
                            }
                            let cover =
                                Rect::from_min_size(rect.min + vec2(7.0, 6.0), vec2(44.0, 44.0));
                            if row.id == LIKED {
                                ui.painter().rect_filled(
                                    cover,
                                    4.0,
                                    Color32::from_rgb(93, 65, 207),
                                );
                                theme::paint_icon(ui, Icon::Check, cover, 24.0, Color32::WHITE);
                            } else {
                                super::widgets::paint_cover(
                                    ui,
                                    &palette,
                                    row.cover.as_deref(),
                                    cover,
                                    4.0,
                                    Icon::ListMusic,
                                    Some(app.backend.art()),
                                );
                            }
                            let mark = Rect::from_center_size(
                                egui::pos2(rect.right() - 18.0, rect.center().y),
                                vec2(20.0, 20.0),
                            );
                            let text_rect = Rect::from_min_max(
                                rect.min + vec2(63.0, 6.0),
                                egui::pos2(mark.left() - 10.0, rect.bottom() - 5.0),
                            );
                            let mut text_ui = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(text_rect)
                                    .layout(Layout::top_down(Align::Min)),
                            );
                            text_ui.spacing_mut().item_spacing.y = 3.0;
                            text_ui.set_clip_rect(text_rect.intersect(ui.clip_rect()));
                            theme::text(&mut text_ui, &row.name, theme::bold(14.0), palette.text);
                            text_ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                if row.pinned.is_some() {
                                    theme::icon(ui, Icon::Pin, 11.0, GREEN);
                                }
                                if row.folder.is_some() {
                                    theme::icon(ui, Icon::Folder, 12.0, palette.secondary);
                                }
                                let mut detail = row.folder.clone().unwrap_or_default();
                                if let Some(count) = row.count {
                                    if !detail.is_empty() {
                                        detail.push_str(" · ");
                                    }
                                    detail.push_str(&locale.song_count(count));
                                }
                                theme::text(ui, detail, theme::regular(11.0), palette.secondary);
                            });
                            if row.membership.is_none() {
                                theme::paint_icon(
                                    ui,
                                    if row.error {
                                        Icon::CircleAlert
                                    } else {
                                        Icon::Loader
                                    },
                                    mark,
                                    17.0,
                                    palette.secondary,
                                );
                            } else {
                                paint_mark(ui, mark, checked, partial, palette.secondary);
                            }
                            theme::focus_ring(ui, &response);
                            if move_focus && draft.highlighted.as_ref() == Some(&row.id) {
                                response.scroll_to_me(None);
                            }
                            if enabled
                                && (response.clicked()
                                    || (toggle_key && draft.highlighted.as_ref() == Some(&row.id)))
                            {
                                if let Some(values) = &row.membership {
                                    draft
                                        .originals
                                        .entry(row.id.clone())
                                        .or_insert_with(|| values.clone());
                                }
                                if selected == Some(false)
                                    && count
                                        .is_some_and(|count| count > 0 && count < draft.items.len())
                                {
                                    draft.choices.remove(&row.id);
                                    draft.originals.remove(&row.id);
                                    return;
                                }
                                let intended = !checked;
                                // Restore the original state without leaving a redundant write.
                                if row.membership.as_ref().is_some_and(|values| {
                                    values.iter().all(|value| *value == intended)
                                }) {
                                    draft.choices.remove(&row.id);
                                    draft.originals.remove(&row.id);
                                } else {
                                    draft.choices.insert(row.id.clone(), intended);
                                }
                            }
                            let hint = if !row.editable {
                                gettext(locale, "This playlist is read-only")
                            } else if row.error {
                                gettext(locale, "Couldn't check this playlist. Try again.")
                            } else if row.membership.is_none() {
                                gettext(locale, "Checking playlist…")
                            } else if partial {
                                gettext(
                                    locale,
                                    "Contains some selected songs. Select to add the rest.",
                                )
                            } else {
                                row.name.clone().into()
                            };
                            response.on_hover_text(hint);
                        });
                    }
                },
            );
            if rows.iter().any(|row| row.error)
                && theme::soft_button(
                    ui,
                    &palette,
                    Some(Icon::Refresh),
                    &gettext(locale, "Try again"),
                    false,
                )
                .clicked()
            {
                retry = true;
            }
            super::widgets::menu_separator(ui, &palette);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let changed = !draft.choices.is_empty()
                    || draft
                        .new_name
                        .as_ref()
                        .is_some_and(|name| !name.trim().is_empty());
                ui.add_enabled_ui(changed, |ui| {
                    if theme::pill_button(ui, &palette, &gettext(locale, "Done"), true).clicked() {
                        commit = true;
                    }
                });
                if theme::pill_button(ui, &palette, &gettext(locale, "Cancel"), false).clicked() {
                    cancel = true;
                }
            });
        });
    if response.should_close() || cancel {
        app.actions.push(Action::CancelPlaylistPicker);
    } else {
        app.actions.push(Action::EditPlaylistPicker(draft));
        if commit {
            app.actions.push(Action::CommitPlaylistPicker);
        }
        if retry {
            app.actions.push(Action::RetryPlaylistMembership);
        }
    }
}
