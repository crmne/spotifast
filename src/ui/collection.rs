//! Playlist, album, and Liked Songs pages: a hero, actions, and a track table.

use std::borrow::Cow;
use std::sync::Arc;

use egui::{Align, Layout, Rect, Sense, Vec2, pos2, vec2};

use crate::api::models::{Album, Image, PlayableItem, Playlist, pick_image};
use crate::app::App;
use crate::i18n::{Locale, gettext, ngettext};
use crate::model::{
    Action, Dialog, DragTrack, Loadable, Page, PagedList, RowContext, SortColumn, TableItem,
    TableRowsCache, TableSort,
};
use crate::theme::{self, Icon, Palette};
use crate::util;

use super::widgets::{self, TrackRow};

pub(super) struct Hero<'a> {
    pub images: HeroImages<'a>,
    pub liked: bool,
    pub kind: Cow<'a, str>,
    pub title: &'a str,
    pub description: Option<String>,
    pub byline: Vec<(String, Option<Page>)>,
    pub round: bool,
}

#[derive(Default)]
pub(super) struct HeroImages<'a> {
    pub image: Option<&'a str>,
    pub previous: Option<&'a str>,
    pub thumbnail: Option<&'a str>,
    pub align_thumbnail: bool,
}

pub(super) fn hero_images<'a>(
    images: &'a [Image],
    preview: Option<&'a [Image]>,
    align_thumbnail: bool,
) -> HeroImages<'a> {
    let image = pick_image(images, 640);
    HeroImages {
        image,
        previous: image.and_then(|image| {
            preview
                .and_then(|images| pick_image(images, 640))
                .filter(|previous| *previous != image)
        }),
        thumbnail: image
            .and_then(|_| preview.and_then(|images| pick_image(images, 64)))
            .or_else(|| pick_image(images, 64)),
        align_thumbnail,
    }
}

pub(super) fn hero(app: &mut App, ui: &mut egui::Ui, hero: Hero<'_>) {
    let palette = app.palette;
    let art = app.backend.art().clone();
    let softened = hero
        .images
        .thumbnail
        .and_then(|uri| app.softened_covers.texture(ui.ctx(), &art, uri));
    ui.add_space(12.0);
    let cover_size = if ui.available_width() > 720.0 {
        212.0
    } else {
        160.0
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 24.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(cover_size), Sense::hover());
        let radius = if hero.round { cover_size / 2.0 } else { 6.0 };
        widgets::paint_shadow(ui, &palette, rect, radius);
        if hero.liked {
            super::sidebar::liked_cover(ui, rect, radius);
        } else {
            widgets::paint_cover_with_thumbnail(
                ui,
                &palette,
                widgets::CoverSources {
                    requested: hero.images.image,
                    previous: hero.images.previous,
                    softened: softened.as_ref(),
                    thumbnail: hero.images.thumbnail,
                    align_thumbnail: hero.images.align_thumbnail,
                },
                rect,
                radius,
                if hero.round { Icon::User } else { Icon::Music },
                Some(app.backend.art()),
            );
        }
        ui.vertical(|ui| {
            let width = ui.available_width();
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add_space(cover_size * 0.08);
            theme::text(ui, hero.kind.as_ref(), theme::medium(12.5), palette.text);
            let mut size = if cover_size > 200.0 { 56.0 } else { 40.0 };
            // Measured on the display text: the same glyphs, in the order
            // they are drawn.
            let display_title = crate::bidi::display_text(hero.title);
            loop {
                let galley = ui.painter().layout_no_wrap(
                    display_title.to_string(),
                    theme::bold(size),
                    palette.text,
                );
                if galley.size().x <= width || size <= 22.0 {
                    break;
                }
                size -= 6.0;
            }
            theme::text(ui, hero.title, theme::bold(size), palette.text);
            if let Some(description) = &hero.description
                && !description.is_empty()
            {
                theme::text(
                    ui,
                    description.as_str(),
                    theme::regular(13.5),
                    palette.secondary,
                );
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for (index, (text, page)) in hero.byline.iter().enumerate() {
                    if index > 0 {
                        theme::text(ui, "•", theme::regular(13.5), palette.secondary);
                    }
                    match page {
                        Some(page) => {
                            if theme::link(ui, text, theme::semibold(13.5), palette.text).clicked()
                            {
                                app.actions.push(Action::Open(page.clone()));
                            }
                        }
                        None => {
                            theme::text(ui, text, theme::regular(13.5), palette.secondary);
                        }
                    }
                }
            });
        });
    });
    ui.add_space(20.0);
}

pub struct Actions<'a> {
    pub play_uri: Option<String>,
    /// Playable songs in the sorted or filtered view, in displayed order.
    pub view: Option<Arc<[String]>>,
    pub saved: Option<(String, bool)>,
    pub saved_icons: (Icon, Icon),
    pub saved_tooltips: (Cow<'a, str>, Cow<'a, str>),
    pub owned_playlist: Option<Playlist>,
    /// A playlist page can be refreshed from its More menu.
    pub reload: Option<(Page, bool)>,
    pub name: &'a str,
    /// A radio page offers to save its songs as a playlist, by the seed's
    /// URI, in place of the Spotify item's own menu.
    pub save_radio: Option<String>,
}

/// The big play button and its neighbours; returns the filter text if a
/// filter field was shown.
pub fn actions_row(
    app: &mut App,
    ui: &mut egui::Ui,
    actions: Actions<'_>,
    filter: Option<&mut String>,
) {
    let palette = app.palette;
    let locale = app.locale;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        if let Some(uri) = &actions.play_uri {
            let now_playing_here = app.playing_context_uri().as_deref() == Some(uri.as_str())
                && app.believed_playing();
            let is_filtered = filter.as_ref().is_some_and(|f| !f.trim().is_empty());
            // A radio is mixed afresh each time Spotify is asked, so it
            // always plays the songs on screen.
            let play_view = actions.view.is_some()
                && (!app.playing_context_shuffle() || is_filtered || actions.save_radio.is_some());
            let can_start = actions.view.as_ref().is_none_or(|uris| !uris.is_empty());
            let icon = if now_playing_here {
                Icon::PauseFilled
            } else {
                Icon::PlayFilled
            };
            if app.play_pending(uri) {
                theme::circle_spinner(
                    ui,
                    56.0,
                    palette.accent,
                    palette.on_accent,
                    &gettext(locale, "Starting…"),
                );
            } else if ui
                .add_enabled_ui(now_playing_here || can_start, |ui| {
                    theme::circle_button(
                        ui,
                        icon,
                        56.0,
                        palette.accent,
                        palette.accent_hover,
                        palette.on_accent,
                        &if now_playing_here {
                            gettext(locale, "Pause")
                        } else {
                            gettext(locale, "Play")
                        },
                    )
                })
                .inner
                .on_disabled_hover_text(gettext(locale, "No playable songs in this view").as_ref())
                .clicked()
            {
                if now_playing_here {
                    app.actions.push(Action::TogglePlay);
                } else if let Some(uris) = actions.view.clone()
                    && play_view
                {
                    app.actions.push(Action::PlayFromRow {
                        context: RowContext::View {
                            uris: Arc::clone(&uris),
                            context_uri: uri.clone(),
                            // Header playback needs no edit rights; row
                            // menus carry theirs via the table conversion.
                            editable_playlist: None,
                        },
                        uri: String::new(),
                        index: 0,
                    });
                } else {
                    app.actions.push(Action::PlayContext {
                        uri: uri.clone(),
                        offset_uri: None,
                        offset_index: None,
                    });
                }
            }
            let shuffle = app.playing_context_shuffle();
            if theme::icon_button(
                ui,
                Icon::Shuffle,
                26.0,
                if shuffle {
                    palette.accent
                } else {
                    palette.secondary
                },
                palette.text,
                &if shuffle {
                    gettext(locale, "Shuffle off")
                } else {
                    gettext(locale, "Shuffle")
                },
            )
            .clicked()
            {
                app.actions.push(Action::SetShuffle(!shuffle));
            }
        }
        if let Some((uri, saved)) = &actions.saved {
            let (icon, tooltip, color) = if *saved {
                (
                    actions.saved_icons.1,
                    &actions.saved_tooltips.1,
                    palette.accent,
                )
            } else {
                (
                    actions.saved_icons.0,
                    &actions.saved_tooltips.0,
                    palette.secondary,
                )
            };
            if theme::icon_button(ui, icon, 26.0, color, palette.text, tooltip).clicked() {
                app.actions.push(Action::ToggleSaved(uri.clone()));
            }
        }
        if let Some(seed) = &actions.save_radio
            && theme::icon_button(
                ui,
                Icon::CirclePlus,
                26.0,
                palette.secondary,
                palette.text,
                &gettext(locale, "Save as playlist"),
            )
            .clicked()
        {
            app.actions.push(Action::SaveRadio(seed.clone()));
        }
        if let Some(uri) = &actions.play_uri {
            let more = theme::icon_button(
                ui,
                Icon::Ellipsis,
                26.0,
                palette.secondary,
                palette.text,
                &gettext(locale, "More"),
            );
            egui::Popup::menu(&more)
                .frame(widgets::menu_frame(&palette))
                .show(|ui| {
                    if let Some(seed) = &actions.save_radio {
                        // As narrow as every other item menu.
                        ui.set_min_width(200.0);
                        ui.set_max_width(300.0);
                        if widgets::menu_item(
                            ui,
                            &palette,
                            Some(Icon::CirclePlus),
                            &gettext(locale, "Save as playlist"),
                        ) {
                            app.actions.push(Action::SaveRadio(seed.clone()));
                        }
                    } else {
                        widgets::context_menu_items(
                            ui,
                            app,
                            uri,
                            actions.name,
                            actions.owned_playlist.as_ref(),
                        );
                    }
                    if let Some((page, loading)) = &actions.reload {
                        widgets::menu_separator(ui, &palette);
                        let clicked = ui
                            .add_enabled_ui(!loading, |ui| {
                                widgets::menu_item(
                                    ui,
                                    &palette,
                                    Some(Icon::Refresh),
                                    &if *loading {
                                        gettext(locale, "Refreshing…")
                                    } else {
                                        gettext(locale, "Refresh")
                                    },
                                )
                            })
                            .inner;
                        if clicked {
                            app.actions.push(Action::Reload(page.clone()));
                        }
                    }
                });
        }
        if let Some(filter) = filter {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                widgets::search_field(
                    ui,
                    &palette,
                    locale,
                    egui::Id::new(("collection-filter", actions.name)),
                    filter,
                    &gettext(locale, "Filter"),
                    220.0,
                );
            });
        }
    });
    ui.add_space(14.0);
}

/// A track table with virtualised rows and paging.
pub struct Table<'a> {
    pub items: &'a [TableItem],
    /// Spotify index represented by the first item.
    pub row_offset: u32,
    pub pagination: Option<TablePagination<'a>>,
    pub context: RowContext,
    pub show_album: bool,
    pub show_cover: bool,
    pub show_added: bool,
    pub show_added_by: bool,
    pub page: Page,
    pub loading: bool,
    pub error: Option<&'a str>,
    pub can_load_more: bool,
    pub filter: &'a str,
    pub items_revision: u64,
}

/// The server's row space, including unloaded and unavailable entries.
#[derive(Clone, Copy)]
pub struct TablePagination<'a> {
    pub total: u32,
    pub loaded_count: usize,
    /// Local server positions of playable items. Null playlist entries still
    /// occupy a slot even though they cannot be played or selected.
    pub positions: Option<&'a [usize]>,
}

#[derive(Clone)]
pub struct TableCache {
    pub sort: Option<TableSort>,
    pub needle: String,
    pub items_revision: u64,
    pub user_names_revision: u64,
    pub visible: Arc<[usize]>,
    pub view_uris: Option<Arc<[String]>>,
    /// Playback positions for visible rows; unavailable rows have no position.
    pub view_positions: Arc<[Option<usize>]>,
}

pub fn table_items_hit(
    app: &App,
    page: &Page,
    generation: u64,
    items_revision: u64,
    user_names_revision: u64,
) -> Option<Arc<Vec<TableItem>>> {
    app.table_rows.get(page).and_then(|cached| {
        (cached.generation == generation
            && cached.items_revision == items_revision
            && cached.user_names_revision == user_names_revision)
            .then(|| Arc::clone(&cached.items))
    })
}

pub fn remember_table_items(
    app: &mut App,
    page: Page,
    generation: u64,
    items_revision: u64,
    user_names_revision: u64,
    items: Vec<TableItem>,
) -> Arc<Vec<TableItem>> {
    let items = Arc::new(items);
    app.table_rows.insert(
        page.clone(),
        TableRowsCache {
            generation,
            items_revision,
            user_names_revision,
            items: Arc::clone(&items),
            playlist_positions: None,
            playlist_raw_count: 0,
            playlist_duration_ms: 0,
            playlist_owner: None,
            playlist_append_revision: None,
        },
    );
    app.retain_table_rows(&page);
    items
}

/// Cached table rows for one page. Rebuilt only when the source list,
/// contributor names, or page generation change, not every frame.
///
/// The cache lives on `App`, not in egui temp data. It is dropped when the
/// page is evicted, when the account resets, and when more than two tables
/// would be retained. Recreated pages get a new generation, so an old
/// revision number cannot resurrect stale rows.
pub fn cached_table_items(
    app: &mut App,
    page: Page,
    generation: u64,
    items_revision: u64,
    user_names_revision: u64,
    build: impl FnOnce() -> Vec<TableItem>,
) -> Arc<Vec<TableItem>> {
    if let Some(items) =
        table_items_hit(app, &page, generation, items_revision, user_names_revision)
    {
        app.retain_table_rows(&page);
        return items;
    }
    remember_table_items(
        app,
        page,
        generation,
        items_revision,
        user_names_revision,
        build(),
    )
}

pub fn prepare_table_view(
    ui: &mut egui::Ui,
    app: &App,
    page: &Page,
    items: &[TableItem],
    needle: &str,
    sort: Option<TableSort>,
    items_revision: u64,
) -> Arc<TableCache> {
    let cache_id = egui::Id::new("table-view-cache").with(page);
    let cached = ui.data(|d| d.get_temp::<Arc<TableCache>>(cache_id));

    let is_valid = cached.as_ref().is_some_and(|c| {
        c.sort == sort
            && c.needle == needle
            && c.items_revision == items_revision
            && c.user_names_revision == app.user_names_revision
    });

    if let Some(entry) = cached.filter(|_| is_valid) {
        entry
    } else {
        let visible = view_indices(items, needle, sort);
        let mut view_positions = Vec::new();
        let view_uris = (sort.is_some() || !needle.is_empty()).then(|| {
            let mut uris = Vec::new();
            for &index in &visible {
                let item = &items[index].0;
                view_positions.push(widgets::row_playable(item).then(|| {
                    let position = uris.len();
                    uris.push(item.uri().to_string());
                    position
                }));
            }
            Arc::<[String]>::from(uris)
        });
        let entry = Arc::new(TableCache {
            sort,
            needle: needle.to_string(),
            items_revision,
            user_names_revision: app.user_names_revision,
            visible: visible.into(),
            view_uris,
            view_positions: view_positions.into(),
        });
        ui.data_mut(|d| d.insert_temp(cache_id, Arc::clone(&entry)));
        entry
    }
}

fn view_context(base: &RowContext, view_uris: Option<&Arc<[String]>>) -> RowContext {
    if let Some(uris) = view_uris {
        match base {
            RowContext::Context {
                uri,
                editable_playlist,
            } => RowContext::View {
                uris: Arc::clone(uris),
                context_uri: uri.clone(),
                editable_playlist: editable_playlist.clone(),
            },
            _ => RowContext::Uris(Arc::clone(uris)),
        }
    } else {
        base.clone()
    }
}

pub fn table(app: &mut App, ui: &mut egui::Ui, table: Table<'_>) {
    let palette = app.palette;
    let locale = app.locale;
    let needle = table.filter.trim().to_lowercase();
    let sort = app.table_sorts.get(&table.page).copied();
    let entry = prepare_table_view(
        ui,
        app,
        &table.page,
        table.items,
        &needle,
        sort,
        table.items_revision,
    );
    let thin = app.settings.tracklist_compact;
    let show_cover = !thin && table.show_cover;
    let row_height = if thin {
        theme::THIN_ROW_HEIGHT
    } else {
        theme::ROW_HEIGHT
    };

    let finite = table
        .pagination
        .filter(|page| page.total > 0 && sort.is_none() && needle.is_empty());
    let rows = finite.map_or(entry.visible.len(), |page| page.total as usize);
    if rows > 0
        && let Some(column) = widgets::table_header(
            ui,
            &palette,
            app.locale,
            table.show_album,
            table.show_added,
            table.show_added_by,
            show_cover,
            sort,
        )
    {
        // Ascending, descending, back to the list's own order.
        let next = match sort {
            Some(sort) if sort.column == column && sort.ascending => Some(TableSort {
                column,
                ascending: false,
            }),
            Some(sort) if sort.column == column => None,
            // The # stands for the list's own order: from any other sort
            // it returns there rather than layering a sort of its own.
            Some(_) if column == SortColumn::Index => None,
            // Ascending by # is the list's own order, a click that would
            // change nothing; the first click on # reverses instead.
            _ => Some(TableSort {
                column,
                ascending: column != SortColumn::Index,
            }),
        };
        match next {
            Some(sort) => {
                app.table_sorts.insert(table.page.clone(), sort);
                app.note_session_change();
                // A sort covers the whole list, so the rest must load.
                app.actions.push(Action::LoadMore(table.page.clone()));
            }
            None => {
                app.table_sorts.remove(&table.page);
                app.note_session_change();
            }
        }
    }
    // What is displayed is what plays: a sorted view plays in its own
    // order, as a plain list of tracks, and its rows cannot edit server
    // positions that no longer match the screen.
    let context = view_context(&table.context, entry.view_uris.as_ref());
    let sorted = sort.is_some();
    // Positional playlist edits require the displayed rows to match server order.
    let move_playlist = (sort.is_none() && needle.is_empty())
        .then(|| match &table.context {
            RowContext::Context {
                editable_playlist: Some((id, _)),
                ..
            } => Some(id.clone()),
            _ => None,
        })
        .flatten();
    if move_playlist.is_some() && egui::DragAndDrop::has_payload_of_type::<DragTrack>(ui.ctx()) {
        widgets::scroll_during_drag(ui);
    }
    // Calculate the nearest drop slot from fixed row height because virtualized
    // rows are not all available during drawing.
    let list_top = ui.cursor().top();
    let move_slot = move_playlist.as_ref().and_then(|_| {
        egui::DragAndDrop::payload::<DragTrack>(ui.ctx())?;
        if !ui.rect_contains_pointer(ui.clip_rect()) {
            return None;
        }
        let pos = ui
            .ctx()
            .pointer_latest_pos()
            .filter(|pos| ui.clip_rect().contains(*pos))?;
        let row = (pos.y - list_top) / row_height;
        // The blank space after the final row accepts an append, including
        // the empty-playlist state, where there is no existing row to hit.
        (row >= 0.0)
            .then(|| (row.round() as usize).min(rows))
            .filter(|slot| {
                finite.is_none_or(|page| {
                    (table.row_offset as usize..=table.row_offset as usize + page.loaded_count)
                        .contains(slot)
                })
            })
    });
    // Selection uses display indices. Clear it when sorting, filtering, or row
    // count changes.
    let view = format!(
        "{sort:?}|{needle}|{}|{}",
        entry.visible.len(),
        table.row_offset
    );
    let item_index = |row: usize| -> Option<usize> {
        if let Some(page) = finite {
            let local = row.checked_sub(table.row_offset as usize)?;
            if local >= page.loaded_count {
                return None;
            }
            match page.positions {
                Some(positions) => positions.binary_search(&local).ok(),
                None => (local < table.items.len()).then_some(local),
            }
        } else {
            entry.visible.get(row).copied()
        }
    };
    app.keep_picked_rows_for(&table.page, &view);
    let picked: std::collections::BTreeSet<usize> =
        app.picked_rows(&table.page).cloned().unwrap_or_default();
    // Keep complete rows for immediate optimistic playlist additions.
    let picked_songs: Vec<PlayableItem> = picked
        .iter()
        .filter_map(|row| item_index(*row))
        .filter_map(|index| table.items.get(index))
        .filter(|(item, _, _)| !item.uri().is_empty())
        .map(|(item, _, _)| item.clone())
        .collect();
    let mut pick = None;
    let mut row_responses = Vec::new();
    let mut missing = None;
    let mut retry_shown = false;
    widgets::virtual_rows(ui, rows, row_height, |ui, row| {
        let Some(index) = item_index(row) else {
            let unavailable = finite.is_some_and(|page| {
                (table.row_offset as usize..table.row_offset as usize + page.loaded_count)
                    .contains(&row)
            });
            let first_missing = missing.is_none();
            if !unavailable && first_missing {
                missing = Some(row as u32);
            }
            let retry = !unavailable
                && !retry_shown
                && table.error.is_some()
                && !table.loading
                && ui.cursor().top() >= ui.clip_rect().top();
            retry_shown |= retry;
            if placeholder_row(
                ui,
                &palette,
                locale,
                row_height,
                &if unavailable {
                    gettext(locale, "Unavailable")
                } else if retry {
                    Cow::Borrowed(table.error.unwrap_or_default())
                } else if table.error.is_some() && !table.loading {
                    Cow::Borrowed("")
                } else {
                    gettext(locale, "Loading…")
                },
                retry,
            ) {
                app.actions.push(Action::RetryWindow(table.page.clone()));
            }
            return;
        };
        let local_index = table
            .pagination
            .and_then(|page| page.positions)
            .map_or(index, |positions| positions[index]);
        let actual_index = absolute_row_index(table.row_offset, local_index);
        let (item, added_at, added_by) = &table.items[index];
        if item.uri().is_empty() {
            placeholder_row(
                ui,
                &palette,
                locale,
                row_height,
                &gettext(locale, "Unavailable"),
                false,
            );
            return;
        }
        // Shift neighboring rows around the current drop slot.
        let shift = ui.ctx().animate_value_with_time(
            ui.id().with(("table-move-shift", row)),
            match move_slot {
                Some(slot) if row < slot => -4.0,
                Some(_) => 4.0,
                None => 0.0,
            },
            0.12,
        );
        let (response, asked) = widgets::track_row_response(
            ui,
            app,
            TrackRow {
                index: if entry.view_uris.is_some() {
                    entry.view_positions[row].unwrap_or(row)
                } else {
                    actual_index
                },
                number: Some(if sorted { row + 1 } else { actual_index + 1 }),
                item,
                context: &context,
                show_cover,
                show_playing_overlay: true,
                show_album: table.show_album,
                added_at: added_at.as_deref(),
                added_by: added_by.as_deref(),
                show_added_by: table.show_added_by,
                compact: false,
                thin,
                shift,
                picked: picked.contains(&row),
                picked_songs: &picked_songs,
            },
        );
        row_responses.push(response);
        if let Some(asked) = asked {
            pick = Some((row, asked));
        }
    });
    navigate_song_rows(ui, &row_responses);
    if let Some(position) = missing
        && !table.loading
        && table.error.is_none()
    {
        app.actions.push(Action::LoadWindow {
            page: table.page.clone(),
            position,
        });
    }
    if let Some((row, asked)) = pick {
        app.pick_row(&table.page, &view, row, asked, rows);
    }
    list_shortcuts(ui, app, &table, &view, rows, &item_index, picked_songs);
    // Escape clears the current selection.
    if !picked.is_empty() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        app.clear_picked_rows();
    }
    if let Some(slot) = move_slot {
        // Draw the destination line between shifted rows.
        let y = list_top + slot as f32 * row_height;
        ui.painter().hline(
            ui.max_rect().x_range().shrink(8.0),
            y,
            egui::Stroke::new(2.0, palette.accent),
        );
        if ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary))
            && let Some(track) = egui::DragAndDrop::take_payload::<DragTrack>(ui.ctx())
            && let Some(playlist_id) = move_playlist
        {
            let to = if finite.is_some() {
                slot as u32
            } else {
                table.row_offset.saturating_add(slot as u32)
            };
            // The slot is Spotify's insert_before, exactly what the
            // action's handler sends; a row dropped back on its own
            // edges moves nothing.
            if let Some((origin, from)) = &track.from
                && *origin == playlist_id
            {
                if to != *from && to != from.saturating_add(1) {
                    app.actions.push(Action::MoveInPlaylist {
                        playlist_id,
                        from: *from,
                        to,
                    });
                }
            } else {
                app.actions.push(Action::InsertInPlaylist {
                    playlist_id,
                    position: to,
                    items: track.items.clone(),
                });
            }
        }
    }
    if finite.is_none() && table.loading {
        ui.add_space(8.0);
        widgets::loading_row(ui, &palette, app.locale);
    }
    if finite.is_none()
        && let Some(error) = table.error
    {
        ui.add_space(8.0);
        widgets::error_row(ui, app, error, Some(table.page.clone()));
    }
    if rows == 0 && needle.is_empty() && !table.loading && table.error.is_none() {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Music,
            &gettext(locale, "Nothing here yet"),
            &gettext(locale, "Added songs appear here."),
        );
    } else if entry.visible.is_empty()
        && !needle.is_empty()
        && table.can_load_more
        && !table.loading
    {
        // Filtering a partially loaded list: keep fetching so matches appear.
        app.actions.push(Action::LoadMore(table.page));
    } else if finite.is_none() {
        widgets::load_more_when_near_end(
            ui,
            app,
            table.page,
            table.can_load_more && !table.loading,
        );
    }
}

/// Select all, Copy and Paste on a song list. A focused text field keeps
/// these keys for its own text, and an open dialog keeps them from the
/// list behind it.
fn list_shortcuts(
    ui: &egui::Ui,
    app: &mut App,
    table: &Table<'_>,
    view: &str,
    rows: usize,
    item_index: &dyn Fn(usize) -> Option<usize>,
    picked_songs: Vec<PlayableItem>,
) {
    if ui.ctx().text_edit_focused() || app.dialog.is_some() {
        return;
    }
    let paste_into = match &table.context {
        RowContext::Context {
            editable_playlist: Some((id, _)),
            ..
        }
        | RowContext::View {
            editable_playlist: Some((id, _)),
            ..
        } => Some(id.clone()),
        _ => None,
    };
    let (select_all, copy, pasted) = ui.input_mut(|input| {
        // The platform's Copy and Paste keys arrive as these events, not
        // as key presses.
        let copy = !picked_songs.is_empty() && input.events.contains(&egui::Event::Copy);
        let pasted = paste_into.as_ref().and_then(|_| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Paste(text) => Some(text.clone()),
                _ => None,
            })
        });
        input.events.retain(|event| match event {
            egui::Event::Copy => !copy,
            egui::Event::Paste(_) => pasted.is_none(),
            _ => true,
        });
        (
            input.consume_key(egui::Modifiers::COMMAND, egui::Key::A),
            copy,
            pasted,
        )
    });
    if select_all {
        let all = (0..rows)
            .filter(|row| {
                item_index(*row)
                    .and_then(|index| table.items.get(index))
                    .is_some_and(|(item, _, _)| !item.uri().is_empty())
            })
            .collect();
        app.pick_rows(&table.page, view, all);
    }
    if copy {
        app.actions.push(Action::CopySongs(picked_songs));
    }
    if let (Some(playlist_id), Some(text)) = (paste_into, pasted) {
        app.actions.push(Action::PasteSongs { playlist_id, text });
    }
}

/// Arrow keys follow display order, independent of the positions of the
/// artist links, Like buttons and other controls inside each song row.
fn navigate_song_rows(ui: &egui::Ui, rows: &[egui::Response]) {
    if egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let Some(current) = rows.iter().position(egui::Response::has_focus) else {
        return;
    };
    let (down, up) = ui.input_mut(|input| {
        (
            input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
        )
    });
    if down + up > 0 {
        let movement = down as isize - up as isize;
        let next = current.saturating_add_signed(movement).min(rows.len() - 1);
        // Cancel egui's spatial search at the end of this pass, including on
        // the first key after gaining focus. Tab still reaches child controls.
        ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        rows[next].request_focus();
        rows[next].scroll_to_me(None);
        ui.ctx().request_repaint();
    }
}

fn placeholder_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    locale: Locale,
    height: f32,
    label: &str,
    retry: bool,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let width = (rect.width() - if retry { 96.0 } else { 24.0 }).max(1.0);
    let text = crate::bidi::layout(
        ui.painter(),
        label,
        theme::regular(13.0),
        palette.secondary,
        width,
        1,
        None,
    );
    ui.painter().galley(
        pos2(rect.left() + 12.0, rect.center().y - text.size().y / 2.0),
        text,
        palette.secondary,
    );
    retry
        && ui
            .put(
                Rect::from_center_size(
                    pos2(rect.right() - 40.0, rect.center().y),
                    vec2(64.0, 28.0),
                ),
                egui::Button::new(gettext(locale, "Retry").as_ref()),
            )
            .clicked()
}

fn absolute_row_index(row_offset: u32, local_index: usize) -> usize {
    (row_offset as usize).saturating_add(local_index)
}

fn sort_by_text_key(visible: &mut [usize], ascending: bool, key: impl Fn(usize) -> String) {
    if ascending {
        visible.sort_by_cached_key(|&index| key(index));
    } else {
        visible.sort_by_cached_key(|&index| std::cmp::Reverse(key(index)));
    }
}

/// The indices of `items` as a view presents them: filtered by `needle`
/// (already lowercased), then ordered by `sort`.
fn view_indices(items: &[TableItem], needle: &str, sort: Option<TableSort>) -> Vec<usize> {
    let mut visible: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, (item, _, _))| {
            if item.uri().is_empty() {
                return false;
            }
            if needle.is_empty() {
                return true;
            }
            let haystack = match item {
                PlayableItem::Track(track) => format!(
                    "{} {} {}",
                    track.name,
                    track.artist_names(),
                    track
                        .album
                        .as_ref()
                        .map(|album| album.name.as_str())
                        .unwrap_or("")
                ),
                PlayableItem::Episode(episode) => episode.name.clone(),
            };
            haystack.to_lowercase().contains(needle)
        })
        .map(|(index, _)| index)
        .collect();
    if let Some(sort) = sort {
        match sort.column {
            SortColumn::Title => sort_by_text_key(&mut visible, sort.ascending, |index| {
                items[index].0.name().to_lowercase()
            }),
            SortColumn::Album => {
                sort_by_text_key(&mut visible, sort.ascending, |index| {
                    match &items[index].0 {
                        PlayableItem::Track(track) => track
                            .album
                            .as_ref()
                            .map(|album| album.name.to_lowercase())
                            .unwrap_or_default(),
                        PlayableItem::Episode(_) => String::new(),
                    }
                })
            }
            SortColumn::AddedBy => sort_by_text_key(&mut visible, sort.ascending, |index| {
                items[index].2.as_deref().unwrap_or_default().to_lowercase()
            }),
            SortColumn::Added | SortColumn::Index | SortColumn::Duration => {
                visible.sort_by(|a, b| {
                    let ordering = match sort.column {
                        SortColumn::Added => items[*a].1.cmp(&items[*b].1),
                        SortColumn::Index => a.cmp(b),
                        SortColumn::Duration => {
                            items[*a].0.duration_ms().cmp(&items[*b].0.duration_ms())
                        }
                        _ => unreachable!("text columns are handled above"),
                    };
                    if sort.ascending {
                        ordering
                    } else {
                        ordering.reverse()
                    }
                });
            }
        }
    }
    visible
}

fn total_duration(items: &[TableItem]) -> u64 {
    items
        .iter()
        .map(|(item, _, _)| item.duration_ms() as u64)
        .sum()
}

fn playlist_rows(
    list: &[crate::api::models::PlaylistItem],
    start: usize,
    owner_id: Option<&str>,
    owner_name: &str,
    names: &std::collections::HashMap<String, Option<String>>,
) -> (Vec<TableItem>, Vec<usize>, u64) {
    let mut rows = Vec::new();
    let mut positions = Vec::new();
    let mut duration_ms = 0;
    for (index, item) in list.iter().enumerate() {
        if let Some(mut playable) = item.playable().cloned() {
            if let PlayableItem::Track(track) = &mut playable {
                track.is_local |= item.is_local;
            }
            duration_ms += playable.duration_ms() as u64;
            let adder = item
                .added_by
                .as_ref()
                .and_then(|user| user.id.as_deref())
                .map(|id| {
                    if Some(id) == owner_id {
                        owner_name.to_string()
                    } else {
                        names
                            .get(id)
                            .and_then(|name| name.clone())
                            .unwrap_or_else(|| id.to_string())
                    }
                });
            positions.push(start + index);
            rows.push((playable, item.added_at.clone(), adder));
        }
    }
    (rows, positions, duration_ms)
}

pub(crate) fn playlist_cached_table_items(
    app: &mut App,
    id: &str,
    generation: u64,
    list: &PagedList<crate::api::models::PlaylistItem>,
    owner_id: Option<&str>,
    owner_name: &str,
) -> (Arc<Vec<TableItem>>, Arc<Vec<usize>>, u64) {
    let key = Page::Playlist(id.to_string());
    let revision = list.revision;
    let names_revision = app.user_names_revision;
    let same_owner = |cache: &TableRowsCache| {
        cache
            .playlist_owner
            .as_ref()
            .is_some_and(|(id, name)| id.as_deref() == owner_id && name == owner_name)
    };
    if let Some(cache) = app.table_rows.get(&key)
        && cache.generation == generation
        && cache.items_revision == revision
        && cache.user_names_revision == names_revision
        && same_owner(cache)
        && let Some(positions) = &cache.playlist_positions
    {
        let result = (
            Arc::clone(&cache.items),
            Arc::clone(positions),
            cache.playlist_duration_ms,
        );
        app.retain_table_rows(&key);
        return result;
    }

    let append_from = app.table_rows.get(&key).and_then(|cache| {
        (cache.generation == generation
            && cache.user_names_revision == names_revision
            && same_owner(cache)
            && cache.playlist_append_revision == Some(revision)
            && cache.playlist_raw_count <= list.items.len())
        .then_some(cache.playlist_raw_count)
    });
    if let Some(start) = append_from {
        let (new_rows, new_positions, new_duration) = playlist_rows(
            &list.items[start..],
            start,
            owner_id,
            owner_name,
            &app.user_names,
        );
        if let Some(cache) = app.table_rows.get_mut(&key)
            && let Some(positions) = cache.playlist_positions.as_mut()
            && let Some(rows) = Arc::get_mut(&mut cache.items)
            && let Some(old_positions) = Arc::get_mut(positions)
        {
            rows.extend(new_rows);
            old_positions.extend(new_positions);
            cache.items_revision = revision;
            cache.playlist_raw_count = list.items.len();
            cache.playlist_duration_ms += new_duration;
            cache.playlist_append_revision = None;
            let result = (
                Arc::clone(&cache.items),
                Arc::clone(positions),
                cache.playlist_duration_ms,
            );
            app.retain_table_rows(&key);
            return result;
        }
    }

    let (rows, positions, duration_ms) =
        playlist_rows(&list.items, 0, owner_id, owner_name, &app.user_names);
    let items = remember_table_items(app, key.clone(), generation, revision, names_revision, rows);
    let positions = Arc::new(positions);
    if let Some(cache) = app.table_rows.get_mut(&key) {
        cache.playlist_positions = Some(Arc::clone(&positions));
        cache.playlist_raw_count = list.items.len();
        cache.playlist_duration_ms = duration_ms;
        cache.playlist_owner = Some((owner_id.map(str::to_string), owner_name.to_string()));
    }
    (items, positions, duration_ms)
}

/// A complete, ranked view of the listener's current top tracks.
pub fn top_songs(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    ui.add_space(12.0);
    theme::text(
        ui,
        gettext(app.locale, "Your top songs"),
        theme::bold(30.0),
        palette.text,
    );
    ui.add_space(4.0);
    theme::text(
        ui,
        gettext(
            app.locale,
            "Your most-played tracks from the last four weeks.",
        ),
        theme::regular(13.5),
        palette.secondary,
    );
    ui.add_space(18.0);

    let tracks = match &app.home.top_songs {
        Loadable::Loaded(tracks) => tracks,
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::loading_row(ui, &palette, app.locale);
            return;
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            widgets::error_row(ui, app, &error, Some(Page::TopSongs));
            return;
        }
    };
    let generation = app.home.top_songs_generation;
    let names = app.user_names_revision;
    let items =
        if let Some(items) = table_items_hit(app, &Page::TopSongs, generation, generation, names) {
            items
        } else {
            let rows = tracks
                .iter()
                .cloned()
                .map(|track| (PlayableItem::Track(track), None, None))
                .collect();
            remember_table_items(app, Page::TopSongs, generation, generation, names, rows)
        };
    let uris: Arc<[String]> = items
        .iter()
        .map(|(item, _, _)| item.uri().to_string())
        .collect::<Vec<_>>()
        .into();
    table(
        app,
        ui,
        Table {
            items: &items,
            row_offset: 0,
            pagination: None,
            context: RowContext::Uris(Arc::clone(&uris)),
            show_album: true,
            show_cover: true,
            show_added: false,
            show_added_by: false,
            page: Page::TopSongs,
            loading: app.home.top_songs_loading,
            error: None,
            can_load_more: false,
            filter: "",
            items_revision: app.home.top_songs_generation,
        },
    );
}

pub fn playlist(app: &mut App, ui: &mut egui::Ui, id: &str) {
    if !app.playlist_pages.contains_key(id) {
        app.ensure_loaded(Page::Playlist(id.to_string()));
    }
    let Some(mut page) = app.playlist_pages.remove(id) else {
        return;
    };
    let preview = super::loading_preview(ui.ctx(), id, &page.playlist, || {
        app.known_playlist(id).cloned()
    });
    let user_id = app.user_id().unwrap_or("").to_string();
    match &page.playlist {
        Loadable::Loaded(playlist) => {
            let (items, positions, duration_ms) = playlist_cached_table_items(
                app,
                id,
                page.generation,
                &page.items,
                playlist.owner.id.as_deref(),
                playlist.owner_name(),
            );
            let count = page
                .items
                .total
                .unwrap_or_else(|| playlist.track_total())
                .max(items.len() as u32);
            // Spotify's collaborative flag covers secret collaborations; a
            // playlist made together today is recognised by who added songs.
            let owner_id = playlist.owner.id.as_deref();
            // Spotify's own playlists carry adder ids of their machinery;
            // nothing about them is a collaboration.
            let editorial = owner_id == Some("spotify");
            let others = if editorial {
                0
            } else {
                page.contributors
                    .iter()
                    .filter(|id| !id.is_empty() && Some(id.as_str()) != owner_id)
                    .count()
            };
            let made_together = playlist.collaborative || others > 0;
            let mut byline = vec![(playlist.owner_name().to_string(), None)];
            if others > 0 {
                let named: Vec<String> = page
                    .contributors
                    .iter()
                    .filter(|id| Some(id.as_str()) != owner_id)
                    .filter_map(|id| app.user_names.get(id)?.clone())
                    .collect();
                byline.push((contributors_text(app.locale, &named, others), None));
            }
            let count_text = if page.items.is_complete() {
                songs_and_duration(app.locale, count, duration_ms)
            } else {
                song_count(app.locale, count)
            };
            byline.push((count_text, None));
            let images = hero_images(
                &playlist.images,
                preview
                    .as_deref()
                    .map(|playlist| playlist.images.as_slice()),
                false,
            );
            playlist_hero(app, ui, playlist, images, byline, made_together);
            let owned = playlist.owned_by(&user_id);
            let saved = app.is_saved(&playlist.uri).unwrap_or(false);
            let needle = page.filter.trim().to_lowercase();
            let sort = app
                .table_sorts
                .get(&Page::Playlist(id.to_string()))
                .copied();
            let table_view = prepare_table_view(
                ui,
                app,
                &Page::Playlist(id.to_string()),
                &items,
                &needle,
                sort,
                page.items.revision,
            );
            let view_play = table_view.view_uris.as_ref().map(Arc::clone);
            actions_row(
                app,
                ui,
                playlist_actions(
                    app.locale,
                    playlist,
                    owned,
                    saved,
                    view_play,
                    Some((Page::Playlist(id.to_string()), page.items.loading)),
                ),
                Some(&mut page.filter),
            );
            if page.items.base_offset > 0 && !page.filter.trim().is_empty() {
                app.actions
                    .push(Action::LoadMore(Page::Playlist(id.to_string())));
            }
            let editable = app
                .can_edit_playlist(playlist)
                .then(|| (playlist.id.clone(), playlist.snapshot_id.clone()));
            table(
                app,
                ui,
                Table {
                    items: &items,
                    row_offset: page.items.base_offset,
                    pagination: Some(TablePagination {
                        total: page.items.total.unwrap_or(count),
                        loaded_count: page.items.items.len(),
                        positions: Some(&positions),
                    }),
                    context: RowContext::Context {
                        uri: playlist.uri.clone(),
                        editable_playlist: editable,
                    },
                    show_album: true,
                    show_cover: true,
                    show_added: true,
                    show_added_by: made_together,
                    page: Page::Playlist(id.to_string()),
                    loading: page.items.loading,
                    error: page.items.error.as_deref(),
                    can_load_more: page.items.can_load_more(),
                    filter: &page.filter,
                    items_revision: page.items.revision,
                },
            );
        }
        Loadable::Loading | Loadable::NotLoaded => {
            if let Some(playlist) = &preview {
                playlist_loading_hero(app, ui, playlist);
                playlist_loading_actions(app, ui, playlist, &mut page.filter);
            } else {
                ui.add_space(40.0);
            }
            widgets::loading_row(ui, &app.palette, app.locale);
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            if let Some(playlist) = &preview {
                playlist_loading_hero(app, ui, playlist);
                playlist_loading_actions(app, ui, playlist, &mut page.filter);
            } else {
                ui.add_space(40.0);
            }
            widgets::error_row(ui, app, &error, Some(Page::Playlist(id.to_string())));
        }
    }
    app.playlist_pages.insert(id.to_string(), page);
}

pub fn album(app: &mut App, ui: &mut egui::Ui, id: &str) {
    if !app.album_pages.contains_key(id) {
        app.ensure_loaded(Page::Album(id.to_string()));
    }
    let Some(page) = app.album_pages.remove(id) else {
        return;
    };
    let preview =
        super::loading_preview(ui.ctx(), id, &page.album, || app.known_album(id).cloned());
    let palette = app.palette;
    match &page.album {
        Loadable::Loaded(album) => {
            album_hero(app, ui, album, &page.tracks, preview.as_deref());
            let generation = page.generation;
            let revision = page.tracks.revision;
            let names = app.user_names_revision;
            let key = Page::Album(id.to_string());
            let items = if let Some(items) = table_items_hit(app, &key, generation, revision, names)
            {
                items
            } else {
                let rows = page
                    .tracks
                    .items
                    .iter()
                    .cloned()
                    .map(|mut track| {
                        if track.album.is_none() {
                            track.album = Some(Album {
                                id: album.id.clone(),
                                name: album.name.clone(),
                                uri: album.uri.clone(),
                                images: album.images.clone(),
                                ..Album::default()
                            });
                        }
                        (PlayableItem::Track(track), None, None)
                    })
                    .collect();
                remember_table_items(app, key, generation, revision, names, rows)
            };
            let saved = app.is_saved(&album.uri).unwrap_or(false);
            let sort = app.table_sorts.get(&Page::Album(id.to_string())).copied();
            let table_view = prepare_table_view(
                ui,
                app,
                &Page::Album(id.to_string()),
                &items,
                "",
                sort,
                page.tracks.revision,
            );
            let album_view = table_view.view_uris.as_ref().map(Arc::clone);
            actions_row(
                app,
                ui,
                album_actions(app.locale, album, saved, album_view),
                None,
            );
            table(
                app,
                ui,
                Table {
                    items: &items,
                    row_offset: page.tracks.base_offset,
                    pagination: Some(TablePagination {
                        total: page
                            .tracks
                            .total
                            .or(album.total_tracks)
                            .unwrap_or(items.len() as u32),
                        loaded_count: items.len(),
                        positions: None,
                    }),
                    context: RowContext::Context {
                        uri: album.uri.clone(),
                        editable_playlist: None,
                    },
                    show_album: false,
                    show_cover: false,
                    show_added: false,
                    show_added_by: false,
                    page: Page::Album(id.to_string()),
                    loading: page.tracks.loading,
                    error: page.tracks.error.as_deref(),
                    can_load_more: page.tracks.can_load_more(),
                    filter: "",
                    items_revision: page.tracks.revision,
                },
            );
            ui.add_space(24.0);
            if let Some(date) = &album.release_date {
                theme::text(
                    ui,
                    util::format_date(app.locale, date),
                    theme::regular(12.5),
                    palette.secondary,
                );
            }
            // Labels file the same line under both kinds of copyright;
            // one line wearing both marks reads better than the line twice.
            let mut credits: Vec<(String, Vec<&str>)> = Vec::new();
            for copyright in &album.copyrights {
                let core = copyright
                    .text
                    .trim_start_matches(['©', '℗'])
                    .trim_start_matches("(C)")
                    .trim_start_matches("(P)")
                    .trim()
                    .to_string();
                let mark = if copyright.kind == "P" { "℗" } else { "©" };
                match credits.iter_mut().find(|(held, _)| *held == core) {
                    Some((_, marks)) => {
                        if !marks.contains(&mark) {
                            marks.push(mark);
                        }
                    }
                    None => credits.push((core, vec![mark])),
                }
            }
            for (core, marks) in credits {
                theme::text(
                    ui,
                    format!("{} {core}", marks.join(" ")),
                    theme::regular(11.5),
                    palette.dim,
                );
            }
        }
        Loadable::Loading | Loadable::NotLoaded => {
            if let Some(album) = &preview {
                album_hero(app, ui, album, &page.tracks, None);
                album_loading_actions(app, ui, album);
            } else {
                ui.add_space(40.0);
            }
            widgets::loading_row(ui, &app.palette, app.locale);
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            if let Some(album) = &preview {
                album_hero(app, ui, album, &page.tracks, None);
                album_loading_actions(app, ui, album);
            } else {
                ui.add_space(40.0);
            }
            widgets::error_row(ui, app, &error, Some(Page::Album(id.to_string())));
        }
    }
    app.album_pages.insert(id.to_string(), page);
}

fn playlist_loading_hero(app: &mut App, ui: &mut egui::Ui, playlist: &Playlist) {
    let images = hero_images(&playlist.images, None, false);
    let mut byline = vec![(playlist.owner_name().to_string(), None)];
    let count = playlist.track_total();
    if count > 0 {
        byline.push((song_count(app.locale, count), None));
    }
    playlist_hero(app, ui, playlist, images, byline, playlist.collaborative);
}

fn playlist_hero<'a>(
    app: &mut App,
    ui: &mut egui::Ui,
    playlist: &'a Playlist,
    images: HeroImages<'a>,
    byline: Vec<(String, Option<Page>)>,
    collaborative: bool,
) {
    hero(
        app,
        ui,
        Hero {
            images,
            liked: false,
            kind: if collaborative {
                gettext(app.locale, "Collaborative Playlist")
            } else if playlist.public == Some(true) {
                gettext(app.locale, "Public Playlist")
            } else {
                gettext(app.locale, "Playlist")
            },
            title: &playlist.name,
            description: playlist.description.as_deref().map(util::strip_html),
            byline,
            round: false,
        },
    );
}

fn playlist_loading_actions(
    app: &mut App,
    ui: &mut egui::Ui,
    playlist: &Playlist,
    filter: &mut String,
) {
    let owned = app
        .user_id()
        .is_some_and(|user_id| playlist.owned_by(user_id));
    let saved = app.is_saved(&playlist.uri).unwrap_or(false);
    disabled_actions_row(
        app,
        ui,
        playlist_actions(app.locale, playlist, owned, saved, None, None),
        Some(filter),
    );
}

fn album_loading_actions(app: &mut App, ui: &mut egui::Ui, album: &Album) {
    let saved = app.is_saved(&album.uri).unwrap_or(false);
    disabled_actions_row(app, ui, album_actions(app.locale, album, saved, None), None);
}

fn playlist_actions<'a>(
    locale: Locale,
    playlist: &'a Playlist,
    owned: bool,
    saved: bool,
    view: Option<Arc<[String]>>,
    reload: Option<(Page, bool)>,
) -> Actions<'a> {
    Actions {
        play_uri: Some(playlist.uri.clone()),
        view,
        saved: (!owned).then(|| (playlist.uri.clone(), saved)),
        saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
        saved_tooltips: (
            gettext(locale, "Add to Your Library"),
            gettext(locale, "Remove from Your Library"),
        ),
        owned_playlist: owned.then(|| playlist.clone()),
        reload,
        name: &playlist.name,
        save_radio: None,
    }
}

fn album_actions<'a>(
    locale: Locale,
    album: &'a Album,
    saved: bool,
    view: Option<Arc<[String]>>,
) -> Actions<'a> {
    Actions {
        play_uri: Some(album.uri.clone()),
        view,
        saved: Some((album.uri.clone(), saved)),
        saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
        saved_tooltips: (
            gettext(locale, "Save to Your Library"),
            gettext(locale, "Remove from Your Library"),
        ),
        owned_playlist: None,
        reload: None,
        name: &album.name,
        save_radio: None,
    }
}

fn disabled_actions_row(
    app: &mut App,
    ui: &mut egui::Ui,
    actions: Actions<'_>,
    filter: Option<&mut String>,
) {
    ui.add_enabled_ui(false, |ui| actions_row(app, ui, actions, filter));
}

fn album_hero(
    app: &mut App,
    ui: &mut egui::Ui,
    album: &Album,
    tracks: &PagedList<crate::api::models::Track>,
    preview: Option<&Album>,
) {
    let mut byline: Vec<(String, Option<Page>)> = album
        .artists
        .iter()
        .map(|artist| (artist.name.clone(), artist.id.clone().map(Page::Artist)))
        .collect();
    if let Some(year) = album.year() {
        byline.push((year.to_string(), None));
    }
    let count = album.total_tracks.unwrap_or(tracks.items.len() as u32);
    let duration: u64 = tracks
        .items
        .iter()
        .map(|track| track.duration_ms as u64)
        .sum();
    let count_text = if tracks.is_complete() {
        songs_and_duration(app.locale, count, duration)
    } else {
        song_count(app.locale, count)
    };
    byline.push((count_text, None));
    let images = hero_images(
        &album.images,
        preview.map(|album| album.images.as_slice()),
        true,
    );
    hero(
        app,
        ui,
        Hero {
            images,
            liked: false,
            kind: app.album_kind_label(album),
            title: &album.name,
            description: None,
            byline,
            round: false,
        },
    );
}

pub fn liked(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let revision = app.library.liked.revision;
    let names = app.user_names_revision;
    let items =
        if let Some(items) = table_items_hit(app, &Page::LikedSongs, revision, revision, names) {
            items
        } else {
            let rows = app
                .library
                .liked
                .items
                .iter()
                .map(|saved| {
                    (
                        PlayableItem::Track(saved.track.clone()),
                        saved.added_at.clone(),
                        None,
                    )
                })
                .collect();
            remember_table_items(app, Page::LikedSongs, revision, revision, names, rows)
        };
    let total = app.library.liked.total.unwrap_or(items.len() as u32);
    let user = app
        .user
        .as_ref()
        .map(|user| user.name().to_string())
        .unwrap_or_default();
    let locale = app.locale;
    let count_text = if app.library.liked.is_complete() {
        songs_and_duration(locale, total, total_duration(&items))
    } else {
        song_count(locale, total)
    };
    let liked_title = gettext(locale, "Liked Songs");
    hero(
        app,
        ui,
        Hero {
            images: HeroImages::default(),
            liked: true,
            kind: gettext(locale, "Playlist"),
            title: &liked_title,
            description: None,
            byline: vec![(user, None), (count_text, None)],
            round: false,
        },
    );
    let collection_uri = app
        .user
        .as_ref()
        .map(|user| format!("spotify:user:{}:collection", user.id));
    let filter_id = egui::Id::new("liked-filter");
    let mut filter = ui
        .data(|data| data.get_temp::<String>(filter_id))
        .unwrap_or_default();
    let needle = filter.trim().to_lowercase();
    let sort = app.table_sorts.get(&Page::LikedSongs).copied();
    let table_view = prepare_table_view(
        ui,
        app,
        &Page::LikedSongs,
        &items,
        &needle,
        sort,
        app.library.liked.revision,
    );
    let liked_view = table_view.view_uris.as_ref().map(Arc::clone);
    actions_row(
        app,
        ui,
        Actions {
            play_uri: collection_uri.clone(),
            view: liked_view,
            saved: None,
            saved_icons: (Icon::Heart, Icon::HeartFilled),
            saved_tooltips: Default::default(),
            owned_playlist: None,
            reload: None,
            name: &liked_title,
            save_radio: None,
        },
        Some(&mut filter),
    );
    ui.data_mut(|data| data.insert_temp(filter_id, filter.clone()));
    let uris: Arc<[String]> = items
        .iter()
        .map(|(item, _, _)| item.uri().to_string())
        .collect::<Vec<_>>()
        .into();
    let context = match collection_uri {
        Some(uri) if app.library.liked.is_complete() => RowContext::Context {
            uri,
            editable_playlist: None,
        },
        _ => RowContext::Uris(uris),
    };
    let loading = app.library.liked.loading;
    let error = app.library.liked.error.clone();
    let can_load_more = app.library.liked.can_load_more();
    let _ = &palette;
    table(
        app,
        ui,
        Table {
            items: &items,
            row_offset: 0,
            pagination: None,
            context,
            show_album: true,
            show_cover: true,
            show_added: true,
            show_added_by: false,
            page: Page::LikedSongs,
            loading,
            error: error.as_deref(),
            can_load_more,
            filter: &filter,
            items_revision: app.library.liked.revision,
        },
    );
}

/// `1,234 songs` in a playlist, album or Liked Songs byline.
fn song_count(locale: Locale, count: u32) -> String {
    ngettext(
        locale,
        // Translators: {count} is a number of songs.
        "{count} song",
        "{count} songs",
        count,
    )
    .replace("{count}", &util::format_count(count as u64))
}

/// `1,234 songs, 2 hr 13 min` once the whole list is known.
pub(super) fn songs_and_duration(locale: Locale, count: u32, duration_ms: u64) -> String {
    ngettext(
        locale,
        // Translators: {count} is a number of songs and {duration} their total
        // length, such as "2 hr 13 min".
        "{count} song, {duration}",
        "{count} songs, {duration}",
        count,
    )
    .replace("{count}", &util::format_count(count as u64))
    .replace("{duration}", &util::format_total_ms(locale, duration_ms))
}

/// Who else made a playlist together with its owner: by name when there
/// are one or two known names, by count otherwise.
fn contributors_text(locale: Locale, named: &[String], others: usize) -> String {
    match named {
        [name] if others == 1 => {
            // Translators: {name} is the name of someone who added songs to the playlist.
            gettext(locale, "with {name}").replace("{name}", name)
        }
        [first, second] if others == 2 => {
            // Translators: {first} and {second} are names of people who added songs.
            gettext(locale, "with {first} and {second}")
                .replace("{first}", first)
                .replace("{second}", second)
        }
        _ => ngettext(
            locale,
            // Translators: {count} is how many other people added songs to the playlist.
            "and {count} other",
            "and {count} others",
            u32::try_from(others).unwrap_or(u32::MAX),
        )
        .replace("{count}", &others.to_string()),
    }
}

#[allow(dead_code)]
fn playlist_dialog(app: &mut App, playlist: &Playlist) {
    app.actions.push(Action::ShowDialog(Dialog::EditPlaylist {
        cover: Default::default(),
        id: playlist.id.clone(),
        name: playlist.name.clone(),
        description: playlist.description.clone().unwrap_or_default(),
        public: playlist.public,
    }));
}

#[allow(dead_code)]
fn rect_after(ui: &egui::Ui, height: f32) -> Rect {
    let cursor = ui.cursor();
    Rect::from_min_size(
        pos2(cursor.left(), cursor.top()),
        vec2(ui.available_width(), height),
    )
}

#[allow(dead_code)]
fn palette_of(app: &App) -> Palette {
    app.palette
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{Album, ArtistRef, Image, Track};
    use crate::model::PlaylistPage;

    #[test]
    fn hero_images_keep_the_previous_art_until_the_new_cover_is_ready() {
        let image = |url: &str, width| Image {
            url: url.into(),
            width: Some(width),
            height: Some(width),
        };
        let current = vec![image("current-large", 640), image("current-small", 64)];
        let preview = vec![image("preview-large", 640), image("preview-small", 64)];
        let ctx = egui::Context::default();
        let loading = Loadable::Loading;
        let loading_preview =
            super::super::loading_preview(&ctx, "collection", &loading, || Some(preview.clone()));
        assert_eq!(
            loading_preview.as_deref().map(Vec::as_slice),
            Some(preview.as_slice())
        );

        let loaded = Loadable::Loaded(current.clone());
        let retained = super::super::loading_preview(&ctx, "collection", &loaded, || {
            panic!("loaded pages must not scan known metadata")
        });
        assert!(Arc::ptr_eq(
            loading_preview.as_ref().unwrap(),
            retained.as_ref().unwrap()
        ));
        let images = hero_images(
            loaded.get().unwrap(),
            retained.as_deref().map(Vec::as_slice),
            false,
        );
        assert_eq!(images.image, Some("current-large"));
        assert_eq!(images.previous, Some("preview-large"));
        assert_eq!(images.thumbnail, Some("preview-small"));
        assert!(
            super::super::loading_preview(&ctx, "other", &loaded, || None).is_none(),
            "another page must not inherit the retained cover"
        );

        let images = hero_images(&current, Some(&current), false);
        assert_eq!(images.previous, None, "the same cover is not loaded twice");

        let images = hero_images(&[], Some(&preview), false);
        assert_eq!(images.image, None);
        assert_eq!(
            images.previous, None,
            "missing artwork keeps its placeholder"
        );
        assert_eq!(images.thumbnail, None);
    }

    #[test]
    fn finite_playlist_reserves_its_height_and_requests_the_visible_window() {
        let mut app = test_app();
        app.backend.set_offline(true);
        app.playlist_pages.insert(
            "finite".into(),
            PlaylistPage {
                playlist: Loadable::Loaded(Playlist {
                    id: "finite".into(),
                    name: "Finite".into(),
                    uri: "spotify:playlist:finite".into(),
                    tracks: Some(crate::api::models::TrackCount { total: 1000 }),
                    ..Default::default()
                }),
                items: PagedList {
                    items: make_large_tracks(50)
                        .into_iter()
                        .map(|(item, _, _)| crate::api::models::PlaylistItem {
                            item: Some(item),
                            ..Default::default()
                        })
                        .collect(),
                    total: Some(1000),
                    next_offset: Some(50),
                    loaded_once: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut height = 0.0;
        for _ in 0..2 {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    let output = egui::ScrollArea::vertical()
                        .vertical_scroll_offset(theme::ROW_HEIGHT * 720.0)
                        .show(ui, |ui| playlist(&mut app, ui, "finite"));
                    height = output.content_size.y;
                },
            );
            frame.textures_delta.clear();
        }
        assert!(
            height >= theme::ROW_HEIGHT * 1000.0,
            "full height: {height}"
        );
        assert!(app.actions.iter().any(|action| matches!(action,
            Action::LoadWindow { page: Page::Playlist(id), position } if id == "finite" && *position > 650 && *position < 750
        )), "a jump must request the distant rows, not the next sequential page");
    }

    #[test]
    fn null_album_slots_are_not_sorted_playback_entries() {
        let items = vec![
            (PlayableItem::Track(Track::default()), None, None),
            (
                PlayableItem::Track(Track {
                    uri: "spotify:track:a".into(),
                    ..Default::default()
                }),
                None,
                None,
            ),
        ];
        assert_eq!(
            view_indices(
                &items,
                "",
                Some(TableSort {
                    column: SortColumn::Title,
                    ascending: true
                })
            ),
            vec![1]
        );
    }

    #[test]
    fn finite_window_errors_offer_retry_without_changing_extent() {
        fn retry_position(shape: &egui::Shape) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(text) if text.galley.text() == "Retry" => {
                    Some(text.pos + text.galley.rect.center().to_vec2())
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(retry_position),
                _ => None,
            }
        }
        let mut app = test_app();
        app.backend.set_offline(true);
        let page = Page::Playlist("retry".into());
        app.playlist_pages.insert(
            "retry".into(),
            PlaylistPage {
                generation: 7,
                items: PagedList {
                    items: vec![crate::api::models::PlaylistItem::default(); 50],
                    base_offset: 950,
                    total: Some(1000),
                    next_offset: None,
                    window_request: Some(900),
                    loaded_once: true,
                    error: Some("Offline".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        // A failed read must wait for the user's Retry, not loop automatically.
        app.apply(Action::LoadMore(page), &egui::Context::default());
        assert!(app.backend.take_playlist_item_requests().is_empty());
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut draw = |ui: &mut egui::Ui| {
            egui::ScrollArea::vertical()
                .vertical_scroll_offset(1200.0)
                .show(ui, |ui| {
                    table(
                        &mut app,
                        ui,
                        Table {
                            items: &[],
                            row_offset: 0,
                            pagination: Some(TablePagination {
                                total: 1000,
                                loaded_count: 0,
                                positions: None,
                            }),
                            context: RowContext::Context {
                                uri: "spotify:playlist:retry".into(),
                                editable_playlist: None,
                            },
                            show_album: false,
                            show_cover: false,
                            show_added: false,
                            show_added_by: false,
                            page: Page::Playlist("retry".into()),
                            loading: false,
                            error: Some("Offline"),
                            can_load_more: true,
                            filter: "",
                            items_revision: 0,
                        },
                    )
                })
                .content_size
                .y
        };
        let mut height = 0.0;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            height = draw(ui);
        });
        let retry = output
            .shapes
            .iter()
            .find_map(|shape| retry_position(&shape.shape));
        output.textures_delta.clear();
        let pos = retry.expect("failed finite windows must expose Retry");
        let mut next_height = 0.0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            },
            |ui| {
                next_height = draw(ui);
            },
        );
        output.textures_delta.clear();
        assert_eq!(height, next_height);
        let actions = std::mem::take(&mut app.actions);
        for action in actions {
            app.apply(action, &ctx);
        }
        assert_eq!(
            app.backend.take_playlist_item_requests(),
            [("retry".into(), 900, 7)],
            "Retry must request the failed backward window, even at the end of the playlist"
        );
        let list = &app.playlist_pages["retry"].items;
        assert_eq!(list.base_offset, 950);
        assert_eq!(list.items.len(), 50);
        assert_eq!(list.total, Some(1000));
        assert!(list.loading);
        app.backend.shutdown();
    }

    #[test]
    fn a_filter_with_no_loaded_matches_keeps_fetching() {
        let mut app = test_app();
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let items = make_large_tracks(1);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            table(
                &mut app,
                ui,
                Table {
                    items: &items,
                    row_offset: 0,
                    pagination: None,
                    context: RowContext::Context {
                        uri: "spotify:playlist:filtered".into(),
                        editable_playlist: None,
                    },
                    show_album: false,
                    show_cover: false,
                    show_added: false,
                    show_added_by: false,
                    page: Page::Playlist("filtered".into()),
                    loading: false,
                    error: None,
                    can_load_more: true,
                    filter: "unmatched",
                    items_revision: 0,
                },
            )
        });
        output.textures_delta.clear();
        assert!(app.actions.iter().any(
            |action| matches!(action, Action::LoadMore(Page::Playlist(id)) if id == "filtered")
        ));
    }

    fn make_large_tracks(count: usize) -> Vec<TableItem> {
        (0..count)
            .map(|i| {
                let track = Track {
                    id: Some(format!("t_{i}")),
                    name: format!("Nested metadata song {i} with a longer title"),
                    uri: format!("spotify:track:large-{i}"),
                    duration_ms: 180_000,
                    artists: vec![ArtistRef {
                        id: Some(format!("artist-{i}")),
                        name: format!("Nested Artist Name {i}"),
                        uri: Some(format!("spotify:artist:artist-{i}")),
                    }],
                    album: Some(Album {
                        id: format!("alb-{i}"),
                        name: format!("Nested Album Title {i}"),
                        uri: format!("spotify:album:alb-{i}"),
                        images: vec![
                            Image {
                                url: format!("https://i.scdn.co/image/large-{i}-640"),
                                width: Some(640),
                                height: Some(640),
                            },
                            Image {
                                url: format!("https://i.scdn.co/image/large-{i}-300"),
                                width: Some(300),
                                height: Some(300),
                            },
                        ],
                        ..Album::default()
                    }),
                    ..Track::default()
                };
                (PlayableItem::Track(track), None, None)
            })
            .collect()
    }

    fn names_only_bytes(items: &[TableItem]) -> usize {
        items
            .iter()
            .map(|(item, ..)| item.uri().len() + item.name().len())
            .sum()
    }

    fn make_test_tracks() -> Vec<TableItem> {
        let titles = [
            "Bohemian Rhapsody",
            "Cancion Animal",
            "Despacito",
            "Ubermensch",
        ];
        let artists = ["Queen", "Soda Stereo", "Luis Fonsi", "Rammstein"];
        let albums = [
            "A Night at the Opera",
            "Cancion Animal Remastered",
            "Vida",
            "Mutter",
        ];

        (0..4)
            .map(|i| {
                let track = Track {
                    id: Some(format!("t_{i}")),
                    name: titles[i].to_string(),
                    uri: format!("spotify:track:t_{i}"),
                    duration_ms: (i as u32 + 1) * 60_000,
                    track_number: Some(i as u32 + 1),
                    disc_number: Some(1),
                    explicit: false,
                    is_local: false,
                    is_playable: Some(true),
                    artists: vec![
                        ArtistRef {
                            id: Some(format!("a_{i}")),
                            name: artists[i].to_string(),
                            uri: Some(format!("spotify:artist:a_{i}")),
                        },
                        ArtistRef {
                            id: Some(format!("feat_{i}")),
                            name: format!("Feat Artist {i}"),
                            uri: Some(format!("spotify:artist:feat_{i}")),
                        },
                    ],
                    album: Some(Album {
                        id: format!("alb_{i}"),
                        name: albums[i].to_string(),
                        uri: format!("spotify:album:alb_{i}"),
                        images: vec![],
                        release_date: Some("2020-01-01".to_string()),
                        album_type: Some("album".to_string()),
                        artists: vec![],
                        album_group: None,
                        total_tracks: Some(10),
                        label: None,
                        genres: vec![],
                        popularity: None,
                        tracks: None,
                        copyrights: vec![],
                        external_urls: Default::default(),
                    }),
                    popularity: None,
                    external_ids: Default::default(),
                    linked_from: None,
                    external_urls: Default::default(),
                };
                (
                    PlayableItem::Track(track),
                    Some(format!("2024-01-0{i}")),
                    Some(format!("User {i}")),
                )
            })
            .collect()
    }

    #[test]
    fn test_view_indices_filtering_and_sorting() {
        let items = make_test_tracks();

        // 1. Unfiltered and unsorted: natural order
        let visible = view_indices(&items, "", None);
        assert_eq!(visible, vec![0, 1, 2, 3]);

        // 2. Filter by track name
        let visible = view_indices(&items, "bohemian", None);
        assert_eq!(visible, vec![0]);

        // 3. Filter by artist name
        let visible = view_indices(&items, "soda", None);
        assert_eq!(visible, vec![1]);

        // 4. Filter by album name
        let visible = view_indices(&items, "mutter", None);
        assert_eq!(visible, vec![3]);

        // 5. Sort descending by title
        let sort = Some(TableSort {
            column: SortColumn::Title,
            ascending: false,
        });
        let visible = view_indices(&items, "", sort);
        assert_eq!(visible, vec![3, 2, 1, 0]);
    }

    #[test]
    fn text_sorts_preserve_case_insensitive_ties_in_both_directions() {
        let mut items = make_test_tracks();
        for (item, label) in items.iter_mut().zip(["Beta", "alpha", "ALPHA", "zeta"]) {
            let PlayableItem::Track(track) = &mut item.0 else {
                panic!("test rows are tracks");
            };
            track.name = label.into();
            track.album.as_mut().unwrap().name = label.into();
            item.2 = Some(label.into());
        }

        for column in [SortColumn::Title, SortColumn::Album, SortColumn::AddedBy] {
            assert_eq!(
                view_indices(
                    &items,
                    "",
                    Some(TableSort {
                        column,
                        ascending: true,
                    }),
                ),
                vec![1, 2, 0, 3],
                "{column:?} ascending"
            );
            assert_eq!(
                view_indices(
                    &items,
                    "",
                    Some(TableSort {
                        column,
                        ascending: false,
                    }),
                ),
                vec![3, 0, 1, 2],
                "{column:?} descending must keep tied rows in playlist order"
            );
        }
    }

    #[test]
    fn text_sort_normalizes_each_visible_row_once() {
        let labels = ["Beta", "alpha", "ALPHA", "zeta"];
        let mut visible = [0, 1, 2, 3];
        let calls = std::cell::Cell::new(0);

        sort_by_text_key(&mut visible, false, |index| {
            calls.set(calls.get() + 1);
            labels[index].to_lowercase()
        });

        assert_eq!(calls.get(), visible.len());
        assert_eq!(visible, [3, 0, 1, 2]);
    }

    #[test]
    fn test_table_cache_validation() {
        let sort = Some(TableSort {
            column: SortColumn::Title,
            ascending: true,
        });
        let cache = TableCache {
            sort,
            needle: "desp".to_string(),
            items_revision: 5,
            user_names_revision: 2,
            visible: Arc::new([2]),
            view_uris: Some(Arc::new(["spotify:track:t_2".to_string()])),
            view_positions: Arc::new([Some(0)]),
        };

        // Cache hit
        assert!(
            cache.sort == sort
                && cache.needle == "desp"
                && cache.items_revision == 5
                && cache.user_names_revision == 2
        );

        // Cache miss on sort change
        let diff_sort = Some(TableSort {
            column: SortColumn::Title,
            ascending: false,
        });
        assert_ne!(cache.sort, diff_sort);

        // Cache miss on filter change
        assert_ne!(cache.needle, "bohemian");

        // Cache miss on items_revision change
        assert_ne!(cache.items_revision, 6);

        // Cache miss on user_names_revision change
        assert_ne!(cache.user_names_revision, 3);
    }

    #[test]
    fn a_direct_playlist_page_keeps_spotify_row_numbers() {
        assert_eq!(absolute_row_index(6_900, 0) + 1, 6_901);
        assert_eq!(absolute_row_index(6_900, 6) + 1, 6_907);
    }

    #[test]
    fn sorted_view_context_keeps_playlist_remove_rights() {
        let uris: Arc<[String]> = Arc::from(["spotify:track:a".to_string()]);
        let editable = Some(("pl1".to_string(), None));

        let base = RowContext::Context {
            uri: "spotify:playlist:pl1".into(),
            editable_playlist: editable.clone(),
        };
        assert_eq!(
            view_context(&base, Some(&uris)),
            RowContext::View {
                uris: Arc::clone(&uris),
                context_uri: "spotify:playlist:pl1".into(),
                editable_playlist: editable.clone(),
            }
        );

        let readonly = RowContext::Context {
            uri: "spotify:playlist:pl1".into(),
            editable_playlist: None,
        };
        assert_eq!(
            view_context(&readonly, Some(&uris)),
            RowContext::View {
                uris: Arc::clone(&uris),
                context_uri: "spotify:playlist:pl1".into(),
                editable_playlist: None,
            }
        );

        let loose = RowContext::Uris(Arc::from(["spotify:track:b".to_string()]));
        assert_eq!(
            view_context(&loose, Some(&uris)),
            RowContext::Uris(Arc::clone(&uris))
        );

        assert_eq!(view_context(&base, None), base);
    }

    fn test_app() -> App {
        let root = std::env::temp_dir().join(format!(
            "spotifast-table-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        App::new(
            &crate::backend::Waker::default(),
            crate::paths::AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            crate::settings::Settings::default(),
            crate::app::AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        )
    }

    struct KeyboardTable {
        ctx: egui::Context,
        app: App,
        items: Vec<TableItem>,
        filter: String,
        height: f32,
        editable: bool,
    }

    impl KeyboardTable {
        fn new() -> Self {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            theme::install(&ctx);
            Self {
                ctx,
                app: test_app(),
                items: make_test_tracks(),
                filter: String::new(),
                height: 600.0,
                editable: false,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::accesskit::TreeUpdate {
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        pos2(0.0, 0.0),
                        vec2(1000.0, self.height),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    widgets::search_field(
                        ui,
                        &self.app.palette,
                        self.app.locale,
                        egui::Id::new("keyboard-filter"),
                        &mut self.filter,
                        "Filter",
                        220.0,
                    );
                    egui::ScrollArea::vertical().animated(false).show(ui, |ui| {
                        table(
                            &mut self.app,
                            ui,
                            Table {
                                items: &self.items,
                                row_offset: 0,
                                pagination: None,
                                context: RowContext::Context {
                                    uri: "spotify:playlist:test".into(),
                                    editable_playlist: self
                                        .editable
                                        .then(|| ("test".to_string(), None)),
                                },
                                show_album: true,
                                show_cover: true,
                                show_added: false,
                                show_added_by: false,
                                page: Page::Playlist("test".into()),
                                loading: false,
                                error: None,
                                can_load_more: false,
                                filter: &self.filter,
                                items_revision: 0,
                            },
                        );
                    });
                },
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        }

        fn focus_song(&mut self, name: &str) -> egui::accesskit::NodeId {
            let tree = self.frame(vec![]);
            let id = tree
                .nodes
                .iter()
                .filter(|(_, node)| {
                    node.label()
                        .is_some_and(|label| label.starts_with(&format!("Play {name},")))
                })
                // AccessKit node storage is not display order. Start at the
                // first visible occurrence when a song appears more than once.
                .min_by(|(_, a), (_, b)| a.bounds().unwrap().y0.total_cmp(&b.bounds().unwrap().y0))
                .expect("song row")
                .0;
            self.frame(vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Focus,
                    target_tree: egui::accesskit::TreeId::ROOT,
                    target_node: id,
                    data: None,
                },
            )]);
            id
        }

        fn key(&mut self, key: egui::Key) -> egui::accesskit::TreeUpdate {
            self.frame(vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }])
        }

        fn focused_label(&mut self) -> String {
            let tree = self.frame(vec![]);
            tree.nodes
                .iter()
                .find(|(id, _)| *id == tree.focus)
                .unwrap()
                .1
                .label()
                .unwrap()
                .to_string()
        }
    }

    /// Select all takes every song the list shows, Copy hands the picked
    /// songs on in the list's order, and Paste offers an editable playlist
    /// the clipboard's text. A focused text field keeps all three keys.
    #[test]
    fn select_all_copy_and_paste_act_on_the_song_list() {
        // The modifiers as the platform reports its command key.
        let command = if cfg!(target_os = "macos") {
            egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND
        } else {
            egui::Modifiers::CTRL | egui::Modifiers::COMMAND
        };
        let select_all = || {
            vec![egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: command,
            }]
        };
        let page = Page::Playlist("test".into());
        let copied = |table: &KeyboardTable| match table.app.actions.as_slice() {
            [Action::CopySongs(items)] => items
                .iter()
                .map(|item| item.uri().to_string())
                .collect::<Vec<_>>(),
            other => panic!("expected one copy, got {other:?}"),
        };

        // #given a filtered list
        let mut table = KeyboardTable::new();
        table.editable = true;
        table.filter = "Queen".into();
        table.frame(vec![]);

        // #when every row is selected
        table.frame(select_all());

        // #then only the rows the filter shows are picked
        assert_eq!(
            table.app.picked_rows(&page).cloned(),
            Some([0].into_iter().collect())
        );
        table.app.actions.clear();
        table.frame(vec![egui::Event::Copy]);
        assert_eq!(copied(&table), ["spotify:track:t_0"]);

        // #when the filter is cleared and every row is selected again
        table.filter.clear();
        table.frame(vec![]);
        assert_eq!(table.app.picked_rows(&page), None);
        table.frame(select_all());
        table.app.actions.clear();
        table.frame(vec![egui::Event::Copy]);

        // #then every song is copied, in the list's order
        let every: Vec<String> = table
            .items
            .iter()
            .map(|(item, _, _)| item.uri().to_string())
            .collect();
        assert_eq!(copied(&table), every);

        // #when links are pasted into the editable playlist
        table.app.actions.clear();
        let links = "https://open.spotify.com/track/abc\nspotify:track:def";
        table.frame(vec![egui::Event::Paste(links.into())]);

        // #then the playlist is offered the pasted text
        assert!(matches!(
            table.app.actions.as_slice(),
            [Action::PasteSongs { playlist_id, text }] if playlist_id == "test" && text == links
        ));

        // #when a text field has focus
        table.app.actions.clear();
        table
            .ctx
            .memory_mut(|memory| memory.request_focus(egui::Id::new("keyboard-filter")));
        table.frame(vec![]);
        table.frame(vec![egui::Event::Copy]);
        table.frame(vec![egui::Event::Paste("Queen".into())]);

        // #then the keys edit the text instead of the list
        assert!(table.app.actions.is_empty());
        assert_eq!(table.filter, "Queen");
        table
            .ctx
            .memory_mut(|memory| memory.surrender_focus(egui::Id::new("keyboard-filter")));
        table.frame(vec![]);
        table.app.clear_picked_rows();
        table
            .ctx
            .memory_mut(|memory| memory.request_focus(egui::Id::new("keyboard-filter")));
        table.frame(vec![]);
        table.frame(select_all());
        assert_eq!(table.app.picked_rows(&page), None);

        // #when a dialog is open over the list
        table
            .ctx
            .memory_mut(|memory| memory.surrender_focus(egui::Id::new("keyboard-filter")));
        table.frame(vec![]);
        table.app.dialog = Some(Dialog::Shortcuts);
        table.frame(select_all());

        // #then the list behind it is left alone
        assert_eq!(table.app.picked_rows(&page), None);
        table.app.dialog = None;

        // #when the list is not an editable playlist
        table.editable = false;
        table.frame(vec![egui::Event::Paste(links.into())]);

        // #then nothing is pasted into it
        assert!(table.app.actions.is_empty());
    }

    #[test]
    fn keyboard_arrows_follow_song_rows_and_enter_plays_the_focused_song() {
        let mut table = KeyboardTable::new();
        table.focus_song("Bohemian Rhapsody");
        table.key(egui::Key::ArrowUp);
        assert!(table.focused_label().starts_with("Play Bohemian Rhapsody,"));
        // Consecutive key frames cover repeat without relying on an idle pass
        // to establish a focus lock on each new row.
        table.key(egui::Key::ArrowDown);
        table.key(egui::Key::ArrowDown);
        assert!(table.focused_label().starts_with("Play Despacito,"));
        table.key(egui::Key::ArrowDown);
        table.key(egui::Key::ArrowDown);
        assert!(table.focused_label().starts_with("Play Ubermensch,"));
        table.key(egui::Key::ArrowUp);
        table.app.actions.clear();
        table.key(egui::Key::Enter);
        assert!(
            matches!(table.app.actions.as_slice(), [Action::PlayFromRow { uri, index: 2, .. }] if uri == "spotify:track:t_2")
        );
    }

    #[test]
    fn keyboard_arrows_follow_the_filtered_sorted_view() {
        let mut table = KeyboardTable::new();
        for index in [1, 3] {
            if let PlayableItem::Track(track) = &mut table.items[index].0 {
                track.artists[0].name = "Shared artist".into();
            }
        }
        table.filter = "Shared artist".into();
        table.app.table_sorts.insert(
            Page::Playlist("test".into()),
            TableSort {
                column: SortColumn::Title,
                ascending: false,
            },
        );
        table.focus_song("Ubermensch");
        table.key(egui::Key::ArrowDown);
        assert!(table.focused_label().starts_with("Play Cancion Animal,"));
        table.app.actions.clear();
        table.key(egui::Key::Enter);
        assert!(
            matches!(table.app.actions.as_slice(), [Action::PlayFromRow { context: RowContext::View { uris, .. }, uri, index: 1 }] if uri == "spotify:track:t_1" && uris.as_ref() == ["spotify:track:t_3", "spotify:track:t_1"])
        );
        table.filter = "Queen".into();
        table.focus_song("Bohemian Rhapsody");
        table.key(egui::Key::ArrowDown);
        assert!(table.focused_label().starts_with("Play Bohemian Rhapsody,"));
    }

    #[test]
    fn keyboard_arrows_work_after_clicking_a_song_body() {
        let mut table = KeyboardTable::new();
        let tree = table.frame(vec![]);
        let bounds = tree
            .nodes
            .iter()
            .find(|(_, node)| {
                node.label()
                    .is_some_and(|label| label.starts_with("Play Bohemian Rhapsody,"))
            })
            .unwrap()
            .1
            .bounds()
            .unwrap();
        let pos = pos2(bounds.x0 as f32 + 180.0, bounds.y0 as f32 + 8.0);
        for pressed in [true, false] {
            table.frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        assert!(table.app.actions.is_empty(), "a body click only selects");
        table.key(egui::Key::ArrowDown);
        assert!(table.focused_label().starts_with("Play Cancion Animal,"));
        table.key(egui::Key::Enter);
        assert!(matches!(
            table.app.actions.as_slice(),
            [Action::PlayFromRow { index: 1, .. }]
        ));
    }

    #[test]
    fn keyboard_tab_reaches_row_controls_and_arrows_leave_the_filter_alone() {
        let mut table = KeyboardTable::new();
        let row = table.focus_song("Bohemian Rhapsody");
        table.key(egui::Key::Tab);
        let tree = table.frame(vec![]);
        assert_ne!(tree.focus, row);
        assert_eq!(table.focused_label(), "Queen");
        table
            .ctx
            .memory_mut(|memory| memory.request_focus(egui::Id::new("keyboard-filter")));
        table.frame(vec![]);
        table.key(egui::Key::ArrowDown);
        assert!(
            table
                .ctx
                .memory(|memory| memory.has_focus(egui::Id::new("keyboard-filter")))
        );
        assert!(table.app.actions.is_empty());
    }

    #[test]
    fn keyboard_arrows_scroll_through_virtual_rows_including_duplicate_songs() {
        let mut table = KeyboardTable::new();
        table.height = 240.0;
        table.items = vec![table.items[0].clone(); 40];
        let first = table.focus_song("Bohemian Rhapsody");
        for _ in 0..25 {
            table.key(egui::Key::ArrowDown);
        }
        let tree = table.frame(vec![]);
        assert_ne!(
            tree.focus, first,
            "duplicate songs must have distinct row focus"
        );
        let node = &tree
            .nodes
            .iter()
            .find(|(id, _)| *id == tree.focus)
            .unwrap()
            .1;
        let bounds = node.bounds().unwrap();
        assert!(
            bounds.y0 >= 0.0 && bounds.y1 <= f64::from(table.height),
            "focused row must scroll into view: {bounds:?}"
        );
        table.app.actions.clear();
        table.key(egui::Key::Enter);
        assert!(matches!(
            table.app.actions.as_slice(),
            [Action::PlayFromRow { index: 25, .. }]
        ));
    }

    #[test]
    fn table_row_cache_hits_until_revision_or_generation_changes() {
        let mut app = test_app();
        let mut builds = 0;
        let page = Page::LikedSongs;
        cached_table_items(&mut app, page.clone(), 1, 0, 0, || {
            builds += 1;
            make_test_tracks()
        });
        cached_table_items(&mut app, page.clone(), 1, 0, 0, || {
            builds += 1;
            panic!("cache hit rebuilt the table");
        });
        assert_eq!(builds, 1);
        cached_table_items(&mut app, page.clone(), 1, 1, 0, || {
            builds += 1;
            make_test_tracks()
        });
        assert_eq!(builds, 2, "revision change must rebuild");
        cached_table_items(&mut app, page, 2, 1, 0, || {
            builds += 1;
            make_test_tracks()
        });
        assert_eq!(builds, 3, "generation change must rebuild");
    }

    #[test]
    fn table_row_cache_memory_counts_nested_metadata_on_a_large_collection() {
        let mut app = test_app();
        let items = make_large_tracks(500);
        let names_only = names_only_bytes(&items);
        assert_eq!(app.table_rows_retained_bytes(), 0);
        cached_table_items(&mut app, Page::LikedSongs, 0, 0, 0, || items);
        let after = app.table_rows_retained_bytes();
        assert!(
            after > names_only,
            "retained bytes must include nested album, artist, and image strings, not just titles: names_only={names_only} after={after}"
        );
        assert!(
            after > 80_000,
            "500 tracks with nested metadata should retain a substantial copy: {after}"
        );
    }

    /// A radio's More menu is as narrow as every other item menu, not as
    /// wide as the page.
    #[test]
    fn the_radio_menu_keeps_the_width_of_other_menus() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        theme::install(&ctx);
        let mut app = test_app();
        let mut frame = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1240.0, 520.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    actions_row(
                        &mut app,
                        ui,
                        Actions {
                            play_uri: Some("spotify:station:track:seed".into()),
                            view: Some(vec!["spotify:track:a".to_string()].into()),
                            saved: None,
                            saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
                            saved_tooltips: Default::default(),
                            owned_playlist: None,
                            reload: Some((Page::Radio("spotify:track:seed".into()), false)),
                            name: "Seed Radio",
                            save_radio: Some("spotify:track:seed".into()),
                        },
                        None,
                    )
                },
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        };
        frame(vec![]);
        let closed = frame(vec![]);
        let more = closed
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("More"))
            .and_then(|(_, node)| node.bounds())
            .expect("a More button");
        let pos = pos2(
            ((more.x0 + more.x1) / 2.0) as f32,
            ((more.y0 + more.y1) / 2.0) as f32,
        );
        frame(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let open = frame(vec![]);
        let refresh = open
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Refresh"))
            .and_then(|(_, node)| node.bounds())
            .expect("the open menu");
        assert!(
            refresh.width() <= 320.0,
            "the menu must not stretch across the page: {}",
            refresh.width()
        );
    }

    #[test]
    fn playlist_refresh_lives_in_more_and_accepts_pointer_and_keyboard() {
        use egui::accesskit::{Action as AccessibleAction, ActionRequest, TreeId};
        for width in [400.0, 800.0] {
            for activation in [None, Some(egui::Key::Enter), Some(egui::Key::Space)] {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                theme::install(&ctx);
                let mut app = test_app();
                let mut filter = String::new();
                let mut frame = |loading, events| {
                    app.actions.clear();
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                egui::Pos2::ZERO,
                                vec2(width, 520.0),
                            )),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            actions_row(
                                &mut app,
                                ui,
                                Actions {
                                    play_uri: Some("spotify:playlist:test".into()),
                                    view: None,
                                    saved: None,
                                    saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
                                    saved_tooltips: Default::default(),
                                    owned_playlist: None,
                                    reload: Some((Page::Playlist("test".into()), loading)),
                                    name: "Test",
                                    save_radio: None,
                                },
                                Some(&mut filter),
                            )
                        },
                    );
                    output.textures_delta.clear();
                    (
                        output.platform_output.accesskit_update.unwrap(),
                        std::mem::take(&mut app.actions),
                    )
                };
                let locate = |tree: &egui::accesskit::TreeUpdate, label: &str| {
                    let (id, node) = tree
                        .nodes
                        .iter()
                        .find(|(_, n)| n.label() == Some(label))
                        .unwrap_or_else(|| panic!("missing {label}"));
                    let b = node.bounds().unwrap();
                    (
                        *id,
                        pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32),
                    )
                };
                let click = |pos| {
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: false,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                };
                frame(false, vec![]);
                let (closed, _) = frame(false, vec![]);
                assert!(
                    !closed
                        .nodes
                        .iter()
                        .any(|(_, n)| n.label() == Some("Refresh")),
                    "no toolbar refresh control"
                );
                let (_, more) = locate(&closed, "More");
                frame(false, click(more));
                let (open, _) = frame(false, vec![]);
                let (refresh, pos) = locate(&open, "Refresh");
                assert!(pos.x >= 0.0 && pos.x <= width && pos.y <= 520.0);
                let events = if let Some(key) = activation {
                    frame(
                        false,
                        vec![egui::Event::AccessKitActionRequest(ActionRequest {
                            action: AccessibleAction::Focus,
                            target_tree: TreeId::ROOT,
                            target_node: refresh,
                            data: None,
                        })],
                    );
                    vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }]
                } else {
                    click(pos)
                };
                let (_, actions) = frame(false, events);
                assert!(
                    matches!(actions.as_slice(), [Action::Reload(Page::Playlist(id))] if id == "test")
                );
                frame(true, vec![]);
                frame(true, click(more));
                let (busy, _) = frame(true, vec![]);
                let (_, disabled) = locate(&busy, "Refreshing…");
                assert!(
                    busy.nodes
                        .iter()
                        .any(|(_, n)| n.label() == Some("Refreshing…") && n.is_disabled())
                );
                let (_, actions) = frame(true, click(disabled));
                assert!(
                    actions.is_empty(),
                    "an in-flight refresh cannot be repeated"
                );
                app.backend.shutdown();
            }
        }
    }

    #[test]
    fn collection_shuffle_button_changes_mode_without_starting_playback() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        let input = |events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            ..Default::default()
        };
        let mut draw = |events| {
            let mut output = ctx.run_ui(input(events), |ui| {
                actions_row(
                    &mut app,
                    ui,
                    Actions {
                        play_uri: Some("spotify:playlist:test".into()),
                        view: None,
                        saved: None,
                        saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
                        saved_tooltips: Default::default(),
                        owned_playlist: None,
                        reload: None,
                        name: "Test",
                        save_radio: None,
                    },
                    None,
                );
            });
            output.textures_delta.clear();
        };

        draw(vec![]);
        let pos = egui::pos2(87.0, 28.0);
        draw(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);

        assert!(
            matches!(app.actions.as_slice(), [Action::SetShuffle(true)]),
            "shuffle must not start playback: {:?}",
            app.actions
        );
    }

    #[test]
    fn sorted_collection_play_button_plays_context_when_shuffling() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.apply(Action::SetShuffle(true), &ctx);
        app.actions.clear();

        let input_layout = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input_layout, |ui| {
            actions_row(
                &mut app,
                ui,
                Actions {
                    play_uri: Some("spotify:playlist:test".into()),
                    view: Some(Arc::from([
                        "spotify:track:1".into(),
                        "spotify:track:2".into(),
                    ])),
                    saved: None,
                    saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
                    saved_tooltips: Default::default(),
                    owned_playlist: None,
                    reload: None,
                    name: "Test",
                    save_radio: None,
                },
                None,
            );
        });
        out.textures_delta.clear();

        let click_pos = egui::pos2(28.0, 28.0);
        let input_click = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events: vec![
                egui::Event::PointerMoved(click_pos),
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };

        let mut output = ctx.run_ui(input_click, |ui| {
            actions_row(
                &mut app,
                ui,
                Actions {
                    play_uri: Some("spotify:playlist:test".into()),
                    view: Some(Arc::from([
                        "spotify:track:1".into(),
                        "spotify:track:2".into(),
                    ])),
                    saved: None,
                    saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
                    saved_tooltips: Default::default(),
                    owned_playlist: None,
                    reload: None,
                    name: "Test",
                    save_radio: None,
                },
                None,
            );
        });
        output.textures_delta.clear();

        assert!(
            matches!(
                app.actions.as_slice(),
                [Action::PlayContext {
                    uri,
                    offset_uri: None,
                    offset_index: None,
                }] if uri == "spotify:playlist:test"
            ),
            "expected PlayContext, got {:?}",
            app.actions
        );
    }

    #[test]
    fn sorted_collection_play_button_plays_from_top_when_not_shuffling() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.apply(Action::SetShuffle(false), &ctx);
        app.actions.clear();

        let input_layout = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input_layout, |ui| {
            actions_row(
                &mut app,
                ui,
                Actions {
                    play_uri: Some("spotify:playlist:test".into()),
                    view: Some(Arc::from([
                        "spotify:track:1".into(),
                        "spotify:track:2".into(),
                    ])),
                    saved: None,
                    saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
                    saved_tooltips: Default::default(),
                    owned_playlist: None,
                    reload: None,
                    name: "Test",
                    save_radio: None,
                },
                None,
            );
        });
        out.textures_delta.clear();

        let click_pos = egui::pos2(28.0, 28.0);
        let input_click = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events: vec![
                egui::Event::PointerMoved(click_pos),
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };

        let mut output = ctx.run_ui(input_click, |ui| {
            actions_row(
                &mut app,
                ui,
                Actions {
                    play_uri: Some("spotify:playlist:test".into()),
                    view: Some(Arc::from([
                        "spotify:track:1".into(),
                        "spotify:track:2".into(),
                    ])),
                    saved: None,
                    saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
                    saved_tooltips: Default::default(),
                    owned_playlist: None,
                    reload: None,
                    name: "Test",
                    save_radio: None,
                },
                None,
            );
        });
        output.textures_delta.clear();

        assert!(
            matches!(
                app.actions.as_slice(),
                [Action::PlayFromRow {
                    context: RowContext::View { uris, context_uri, .. },
                    index: 0,
                    ..
                }] if uris.as_ref() == ["spotify:track:1", "spotify:track:2"] && context_uri == "spotify:playlist:test"
            ),
            "expected PlayFromRow, got {:?}",
            app.actions
        );
    }

    #[test]
    fn sorted_collection_play_button_preserves_filtered_view_when_shuffling() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.apply(Action::SetShuffle(true), &ctx);
        app.actions.clear();

        let input_layout = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut filter = "filter".to_string();
        let mut out = ctx.run_ui(input_layout, |ui| {
            actions_row(
                &mut app,
                ui,
                Actions {
                    play_uri: Some("spotify:playlist:test".into()),
                    view: Some(Arc::from([
                        "spotify:track:1".into(),
                        "spotify:track:2".into(),
                    ])),
                    saved: None,
                    saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
                    saved_tooltips: Default::default(),
                    owned_playlist: None,
                    reload: None,
                    name: "Test",
                    save_radio: None,
                },
                Some(&mut filter),
            );
        });
        out.textures_delta.clear();

        let click_pos = egui::pos2(28.0, 28.0);
        let input_click = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events: vec![
                egui::Event::PointerMoved(click_pos),
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };

        let mut output = ctx.run_ui(input_click, |ui| {
            actions_row(
                &mut app,
                ui,
                Actions {
                    play_uri: Some("spotify:playlist:test".into()),
                    view: Some(Arc::from([
                        "spotify:track:1".into(),
                        "spotify:track:2".into(),
                    ])),
                    saved: None,
                    saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
                    saved_tooltips: Default::default(),
                    owned_playlist: None,
                    reload: None,
                    name: "Test",
                    save_radio: None,
                },
                Some(&mut filter),
            );
        });
        output.textures_delta.clear();

        assert!(
            matches!(
                app.actions.as_slice(),
                [Action::PlayFromRow {
                    context: RowContext::View { uris, context_uri, .. },
                    index: 0,
                    ..
                }] if uris.as_ref() == ["spotify:track:1", "spotify:track:2"] && context_uri == "spotify:playlist:test"
            ),
            "expected PlayFromRow with filtered view, got {:?}",
            app.actions
        );
    }

    #[test]
    fn table_row_cache_drops_when_the_backing_page_is_evicted() {
        let mut app = test_app();
        let page = Page::Playlist("pl-gone".into());
        app.playlist_pages
            .insert("pl-gone".into(), PlaylistPage::default());
        cached_table_items(&mut app, page.clone(), 1, 0, 0, make_test_tracks);
        assert!(app.table_rows.contains_key(&page));
        app.playlist_pages.remove("pl-gone");
        app.open(Page::LikedSongs);
        assert!(
            !app.table_rows.contains_key(&page),
            "evicting the page map must drop the table-row copy"
        );
    }

    #[test]
    fn table_row_cache_keeps_at_most_two_pages() {
        let mut app = test_app();
        for i in 0..5 {
            let page = Page::Playlist(format!("pl{i}"));
            app.playlist_pages
                .insert(format!("pl{i}"), PlaylistPage::default());
            app.history.push(page.clone());
            app.history_index = app.history.len() - 1;
            cached_table_items(&mut app, page, 1, 0, 0, make_test_tracks);
        }
        assert_eq!(app.table_rows.len(), 2);
        assert!(
            app.table_rows.contains_key(&Page::Playlist("pl4".into())),
            "the open page stays"
        );
    }
}
