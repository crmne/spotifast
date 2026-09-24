//! Widgets shared by every view: covers, cards, track rows, menus, sliders.

use egui::{
    Align, Color32, CornerRadius, Frame, Layout, Margin, Rect, Sense, Stroke, Ui, UiBuilder, Vec2,
    pos2, vec2,
};

use crate::api::models::*;
use crate::app::App;
use crate::i18n::{Locale, gettext, ngettext, pgettext};
use crate::model::{Action, Dialog, DragEntry, DragTrack, Page, RowContext, RowPick};
use crate::theme::{self, Icon, Palette};
use crate::util;

pub const CARD_WIDTH: f32 = 172.0;
pub const CARD_GAP: f32 = 14.0;
pub const PAGE_PADDING: f32 = 24.0;

/// Draws an image (or a placeholder) in a square.
pub fn cover(
    ui: &mut Ui,
    palette: &Palette,
    url: Option<&str>,
    size: f32,
    radius: f32,
    fallback: Icon,
) -> Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_cover(ui, palette, url, rect, radius, fallback, None);
    rect
}

#[derive(Default)]
pub(super) struct CoverSources<'a> {
    pub requested: Option<&'a str>,
    pub previous: Option<&'a str>,
    pub softened: Option<&'a egui::TextureHandle>,
    pub thumbnail: Option<&'a str>,
    pub align_thumbnail: bool,
}

pub fn paint_cover(
    ui: &Ui,
    palette: &Palette,
    url: Option<&str>,
    rect: Rect,
    radius: f32,
    fallback: Icon,
    art: Option<&crate::images::ArtLoader>,
) {
    paint_cover_with_thumbnail(
        ui,
        palette,
        CoverSources {
            requested: url,
            ..Default::default()
        },
        rect,
        radius,
        fallback,
        art,
    );
}

/// Paints the requested cover, keeping a softened thumbnail visible until it
/// is ready.
pub(super) fn paint_cover_with_thumbnail(
    ui: &Ui,
    palette: &Palette,
    sources: CoverSources<'_>,
    rect: Rect,
    radius: f32,
    fallback: Icon,
    art: Option<&crate::images::ArtLoader>,
) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let corner = CornerRadius::same(radius.min(127.0) as u8);
    let loaded = sources
        .requested
        .is_some_and(|url| paint_cover_url(ui, url, rect, corner, art, false))
        || sources
            .previous
            .is_some_and(|url| paint_ready_cover_url(ui, url, rect, corner, art))
        || sources.softened.is_some_and(|texture| {
            paint_cover_texture(
                ui,
                egui::load::SizedTexture::from_handle(texture),
                rect,
                corner,
                sources.align_thumbnail,
            );
            true
        })
        || sources.thumbnail.is_some_and(|url| {
            paint_cover_url(ui, url, rect, corner, art, sources.align_thumbnail)
        });
    if !loaded {
        let painter = ui.painter();
        let fill = if palette.dark {
            palette.surface_hover
        } else {
            palette.surface_active
        };
        if radius >= rect.width() / 2.0 - 0.5 {
            painter.circle_filled(rect.center(), rect.width() / 2.0, fill);
        } else {
            painter.rect_filled(rect, corner, fill);
        }
        let icon_size = (rect.width() * 0.42).clamp(12.0, 64.0);
        theme::paint_icon(ui, fallback, rect, icon_size, palette.dim);
    }
}

fn paint_ready_cover_url(
    ui: &Ui,
    url: &str,
    rect: Rect,
    corner: CornerRadius,
    art: Option<&crate::images::ArtLoader>,
) -> bool {
    art.is_some_and(|art| art.is_ready(url)) && paint_cover_url(ui, url, rect, corner, art, false)
}

fn paint_cover_url(
    ui: &Ui,
    url: &str,
    rect: Rect,
    corner: CornerRadius,
    art: Option<&crate::images::ArtLoader>,
    shift_thumbnail: bool,
) -> bool {
    if let Some(art) = art {
        art.touch(url);
    }
    let image = egui::Image::new(url).show_loading_spinner(false);
    let Ok(egui::load::TexturePoll::Ready { texture }) = image.load_for_size(ui.ctx(), rect.size())
    else {
        return false;
    };
    if let Some(art) = art {
        art.release_bytes(url);
        art.note_decoded(
            url,
            texture.size.x.round() as usize,
            texture.size.y.round() as usize,
        );
    }
    paint_cover_texture(ui, texture, rect, corner, shift_thumbnail);
    true
}

fn paint_cover_texture(
    ui: &Ui,
    texture: egui::load::SizedTexture,
    rect: Rect,
    corner: CornerRadius,
    shift_thumbnail: bool,
) {
    let image_aspect = texture.size.x / texture.size.y;
    let rect_aspect = rect.width() / rect.height();
    let mut uv = if image_aspect > rect_aspect {
        let visible_width = rect_aspect / image_aspect;
        let inset = (1.0 - visible_width) / 2.0;
        Rect::from_min_max(pos2(inset, 0.0), pos2(1.0 - inset, 1.0))
    } else {
        let visible_height = image_aspect / rect_aspect;
        let inset = (1.0 - visible_height) / 2.0;
        Rect::from_min_max(pos2(0.0, inset), pos2(1.0, 1.0 - inset))
    };
    if shift_thumbnail {
        // Spotify's larger rendition lands about one thumbnail texel down
        // and right. Crop the preview's last row and column so it starts in
        // that same position and holds still when the full cover replaces it.
        uv.max -= uv.size() / 64.0;
    }
    egui::Image::new(texture)
        .uv(uv)
        .corner_radius(corner)
        .paint_at(ui, rect);
}

/// A soft drop shadow under a cover or card.
pub fn paint_shadow(ui: &Ui, palette: &Palette, rect: Rect, radius: f32) {
    if !palette.dark {
        return;
    }
    let shadow = egui::epaint::Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    ui.painter()
        .add(shadow.as_shape(rect, CornerRadius::same(radius as u8)));
}

/// Fills `rect` with a vertical gradient from `top` to `bottom`.
pub fn paint_vertical_gradient(ui: &Ui, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// Lays out only the rows that intersect the visible area of the enclosing
/// scroll view. Every row must occupy exactly `row_height`.
pub fn virtual_rows(
    ui: &mut Ui,
    count: usize,
    row_height: f32,
    mut row: impl FnMut(&mut Ui, usize),
) {
    if count == 0 {
        return;
    }
    let previous_spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing.y = 0.0;
    let clip = ui.clip_rect();
    let start_y = ui.cursor().top();
    let width = ui.available_width();
    // Retain a neighbour on either side so Tab can focus it and scroll it in.
    let first = (((clip.top() - start_y) / row_height).floor().max(0.0) as usize)
        .min(count)
        .saturating_sub(1);
    let last = (((clip.bottom() - start_y) / row_height).ceil().max(0.0) as usize + 1).min(count);
    if first > 0 {
        ui.allocate_space(vec2(width, first as f32 * row_height));
    }
    for index in first..last {
        row(ui, index);
    }
    if last < count {
        ui.allocate_space(vec2(width, (count - last) as f32 * row_height));
    }
    ui.spacing_mut().item_spacing = previous_spacing;
}

/// A wrapping grid of cards, laid out row by row so only visible cards are
/// measured and painted.
pub fn virtual_wrapped_cards(
    ui: &mut Ui,
    count: usize,
    card_height: f32,
    mut card: impl FnMut(&mut Ui, usize),
) {
    if count == 0 {
        return;
    }
    let spacing = CARD_GAP / 2.0;
    let row_width = ui.available_width().max(CARD_WIDTH);
    let cards_per_row = ((row_width + spacing) / (CARD_WIDTH + spacing))
        .floor()
        .max(1.0) as usize;
    let row_count = count.div_ceil(cards_per_row);
    // CARD_GAP is already in the row height. Do not also inherit item_spacing.y.
    let previous_spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing.y = 0.0;
    let grid_id = ui.unique_id().with("virtual-card");
    virtual_rows(ui, row_count, card_height + CARD_GAP, |ui, row| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(spacing, CARD_GAP);
            let start = row * cards_per_row;
            let end = (start + cards_per_row).min(count);
            for index in start..end {
                ui.scope_builder(UiBuilder::new().id(grid_id.with(index)), |ui| {
                    card(ui, index);
                });
            }
        });
        ui.allocate_space(vec2(row_width, CARD_GAP));
    });
    ui.spacing_mut().item_spacing = previous_spacing;
}

/// Asks for the next page when the user scrolls near the end of a list.
pub fn load_more_when_near_end(ui: &Ui, app: &mut App, page: Page, can_load: bool) {
    if !can_load {
        return;
    }
    let clip = ui.clip_rect();
    let cursor = ui.cursor().top();
    if cursor - clip.bottom() < 900.0 {
        app.actions.push(Action::LoadMore(page));
    }
}

/// One entry in a popup menu. Closes the menu when chosen.
pub fn menu_item(ui: &mut Ui, palette: &Palette, icon: Option<Icon>, label: &str) -> bool {
    menu_item_enabled(ui, palette, icon, label, true)
}

pub fn menu_item_enabled(
    ui: &mut Ui,
    palette: &Palette,
    icon: Option<Icon>,
    label: &str,
    enabled: bool,
) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(
        vec2(width, 28.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        if response.hovered() && enabled {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), palette.surface_hover);
        }
        let color = if enabled { palette.text } else { palette.dim };
        let mut x = rect.left() + 10.0;
        if let Some(icon) = icon {
            let icon_rect =
                Rect::from_center_size(pos2(x + 8.0, rect.center().y), Vec2::splat(16.0));
            icon.image(
                if enabled {
                    palette.secondary
                } else {
                    palette.dim
                },
                16.0,
            )
            .paint_at(ui, icon_rect);
            x += 26.0;
        }
        // A playlist can be named a paragraph; the label ends at the menu's
        // edge instead of running past it.
        let galley = crate::bidi::layout(
            ui.painter(),
            label,
            theme::regular(13.5),
            color,
            (rect.right() - 10.0 - x).max(0.0),
            1,
            Some(crate::bidi::ELLIPSIS),
        );
        let text_rect = Rect::from_min_max(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            pos2(rect.right() - 10.0, rect.center().y + galley.size().y / 2.0),
        );
        ui.painter()
            .galley(crate::bidi::galley_pos(text_rect, &galley), galley, color);
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled && ui.is_enabled(), label)
    });
    theme::focus_ring(ui, &response);
    if enabled {
        crate::autoscroll::row(ui, &response);
    }
    let clicked = enabled && response.clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// One entry in a popup menu that opens a child submenu.
pub fn menu_submenu<R>(
    ui: &mut Ui,
    palette: &Palette,
    icon: Option<Icon>,
    label: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
    let is_in_menu = egui::menu::is_in_menu(ui);
    let submenu_id = egui::menu::SubMenu::id_from_widget_id(response.id);
    let is_open = if is_in_menu {
        egui::menu::MenuState::from_ui(ui, |state, _| state.open_item == Some(submenu_id))
    } else {
        egui::Popup::menu(&response).is_open()
    };

    if ui.is_rect_visible(rect) {
        if response.hovered() || is_open {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), palette.surface_hover);
        }
        let color = palette.text;
        let mut x = rect.left() + 10.0;
        if let Some(icon) = icon {
            let icon_rect =
                Rect::from_center_size(pos2(x + 8.0, rect.center().y), Vec2::splat(16.0));
            icon.image(palette.secondary, 16.0).paint_at(ui, icon_rect);
            x += 26.0;
        }

        let arrow_galley = crate::bidi::layout(
            ui.painter(),
            egui::menu::SubMenuButton::RIGHT_ARROW,
            theme::regular(11.0),
            palette.secondary,
            16.0,
            1,
            None,
        );
        let arrow_width = arrow_galley.size().x;
        let arrow_rect = Rect::from_min_max(
            pos2(
                rect.right() - 10.0 - arrow_width,
                rect.center().y - arrow_galley.size().y / 2.0,
            ),
            pos2(
                rect.right() - 10.0,
                rect.center().y + arrow_galley.size().y / 2.0,
            ),
        );
        ui.painter().galley(
            crate::bidi::galley_pos(arrow_rect, &arrow_galley),
            arrow_galley,
            palette.secondary,
        );

        let max_text_width = (rect.right() - 10.0 - arrow_width - 6.0 - x).max(0.0);
        let galley = crate::bidi::layout(
            ui.painter(),
            label,
            theme::regular(13.5),
            color,
            max_text_width,
            1,
            Some(crate::bidi::ELLIPSIS),
        );
        let text_rect = Rect::from_min_max(
            pos2(x, rect.center().y - galley.size().y / 2.0),
            pos2(
                rect.right() - 10.0 - arrow_width - 6.0,
                rect.center().y + galley.size().y / 2.0,
            ),
        );
        ui.painter()
            .galley(crate::bidi::galley_pos(text_rect, &galley), galley, color);
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    theme::focus_ring(ui, &response);
    if is_in_menu {
        egui::menu::SubMenu::new().show(ui, &response, add_contents)
    } else {
        let config = egui::menu::MenuConfig::find(ui);
        egui::Popup::menu(&response)
            .close_behavior(config.close_behavior)
            .style(config.style.clone())
            .info(
                egui::UiStackInfo::new(egui::UiKind::Menu)
                    .with_tag_value(egui::menu::MenuConfig::MENU_CONFIG_TAG, config),
            )
            .show(add_contents)
    }
}

pub fn menu_separator(ui: &mut Ui, palette: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
    ui.painter().hline(
        rect.x_range().shrink(6.0),
        rect.center().y,
        Stroke::new(1.0, palette.outline),
    );
}

/// The frame every popup menu uses.
pub fn menu_frame(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.overlay)
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin::same(6))
        .shadow(egui::epaint::Shadow {
            offset: [0, 6],
            blur: 20,
            spread: 0,
            color: palette.shadow,
        })
}

/// Context menu for actions on selected tracks.
///
/// Tracks stay in table order rather than selection order.
pub fn picked_menu(
    ui: &mut Ui,
    app: &mut App,
    songs: &[PlayableItem],
    editable_playlist: Option<&(String, Option<String>)>,
) {
    let palette = app.palette;
    let locale = app.locale;
    ui.set_min_width(220.0);
    ui.set_max_width(300.0);
    let count = songs.len();
    let uris: Vec<String> = songs.iter().map(|item| item.uri().to_string()).collect();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(
                ngettext(
                    locale,
                    // Translators: Keep {count} exactly as written. It becomes the number of selected songs.
                    "{count} song",
                    "{count} songs",
                    count as u32,
                )
                .replace("{count}", &count.to_string()),
            )
            .font(theme::medium(12.0))
            .color(palette.secondary),
        );
    });
    ui.add_space(4.0);
    menu_separator(ui, &palette);
    if menu_item(
        ui,
        &palette,
        Some(Icon::ListEnd),
        &gettext(locale, "Add to queue"),
    ) {
        app.actions.push(Action::QueueMany {
            songs: songs
                .iter()
                .map(|item| (item.uri().to_string(), item.name().to_string()))
                .collect(),
        });
    }
    // Set one explicit saved state for the full selection.
    let all_saved = uris.iter().all(|uri| app.is_saved(uri).unwrap_or(false));
    let (icon, text) = if all_saved {
        (
            Icon::HeartFilled,
            gettext(locale, "Remove from Liked Songs"),
        )
    } else {
        (Icon::Heart, gettext(locale, "Save to Liked Songs"))
    };
    if menu_item(ui, &palette, Some(icon), &text) {
        app.actions.push(Action::SetSavedMany {
            uris: uris.clone(),
            saved: !all_saved,
        });
    }
    // Removal is URI-based, so one entry covers the whole selection on
    // both the unsorted context and a sorted or filtered view. The caller
    // only passes a playlist when every picked row shares it.
    if let Some((playlist_id, _)) = editable_playlist
        && menu_item(
            ui,
            &palette,
            Some(Icon::Minus),
            &gettext(locale, "Remove from this playlist"),
        )
    {
        app.actions.push(Action::RemoveFromPlaylist {
            playlist_id: playlist_id.clone(),
            uris: uris.clone(),
        });
    }
    add_to_playlist_menu(ui, app, songs);
}

fn add_to_playlist_menu(ui: &mut Ui, app: &mut App, items: &[PlayableItem]) {
    let query_id = ui.make_persistent_id("add-to-playlist-query");
    let palette = app.palette;
    let opened = menu_submenu(
        ui,
        &palette,
        Some(Icon::ListPlus),
        &gettext(app.locale, "Add to playlist"),
        |ui| {
            let frame = ui.ctx().cumulative_frame_nr();
            let previous = ui
                .data(|data| data.get_temp::<(u64, String)>(query_id))
                .filter(|(last_frame, _)| frame.saturating_sub(*last_frame) <= 1);
            let fresh = previous.is_none();
            let mut query = previous.map(|(_, query)| query).unwrap_or_default();
            let field = playlist_picker(ui, app, items, &mut query);
            if fresh {
                field.request_focus();
            }
            ui.data_mut(|data| data.insert_temp(query_id, (frame, query)));
        },
    );
    if opened.is_none() {
        ui.data_mut(|data| data.remove::<(u64, String)>(query_id));
    }
}

/// Shared by single-item and selection menus. Filtering is local and keeps
/// the existing playlist edit permissions and library order.
pub(crate) fn playlist_picker(
    ui: &mut Ui,
    app: &mut App,
    items: &[PlayableItem],
    query: &mut String,
) -> egui::Response {
    let palette = app.palette;
    let locale = app.locale;
    ui.set_min_width(220.0);
    ui.set_max_width(300.0);
    let width = ui.available_width();
    let field = search_field(
        ui,
        &palette,
        locale,
        ui.make_persistent_id("playlist-filter"),
        query,
        &gettext(locale, "Filter playlists"),
        width,
    );
    ui.add_space(4.0);
    if menu_item(
        ui,
        &palette,
        Some(Icon::Plus),
        &gettext(locale, "New playlist"),
    ) {
        app.actions.push(Action::ShowDialog(Dialog::CreatePlaylist {
            name: String::new(),
            public: false,
            add_uris: items.iter().map(|item| item.uri().to_string()).collect(),
        }));
    }
    menu_separator(ui, &palette);
    let needle = query.trim().to_lowercase();
    let playlists = app.editable_playlists();
    let matches: Vec<_> = playlists
        .iter()
        .filter(|(_, name)| name.to_lowercase().contains(&needle))
        .collect();
    if matches.is_empty() {
        theme::subtle(
            ui,
            &palette,
            &if needle.is_empty() {
                gettext(locale, "No editable playlists")
            } else {
                gettext(locale, "No matching playlists")
            },
        );
    }
    crate::autoscroll::show(
        ui,
        egui::ScrollArea::vertical()
            .id_salt("filtered-playlists")
            .max_height(320.0),
        egui::Vec2b::new(false, true),
        |ui| {
            for (id, name) in matches {
                ui.push_id(id, |ui| {
                    if menu_item(ui, &palette, Some(Icon::ListMusic), name) {
                        app.actions.push(Action::AddToPlaylist {
                            playlist_id: id.clone(),
                            playlist_name: name.clone(),
                            items: items.to_vec(),
                        });
                    }
                });
            }
        },
    );
    field
}

pub fn item_menu(
    ui: &mut Ui,
    app: &mut App,
    item: &PlayableItem,
    context: Option<&RowContext>,
    index: Option<usize>,
) {
    let palette = app.palette;
    let locale = app.locale;
    ui.set_min_width(220.0);
    ui.set_max_width(300.0);
    let uri = item.uri().to_string();
    let label = item.name().to_string();
    if menu_item(
        ui,
        &palette,
        Some(Icon::ListEnd),
        &gettext(locale, "Add to queue"),
    ) {
        app.actions.push(Action::AddToQueue {
            uri: uri.clone(),
            label: label.clone(),
        });
    }
    if item.is_track() {
        let saved = app.is_saved(&uri).unwrap_or(false);
        let (icon, text) = if saved {
            (
                Icon::HeartFilled,
                gettext(locale, "Remove from Liked Songs"),
            )
        } else {
            (Icon::Heart, gettext(locale, "Save to Liked Songs"))
        };
        if menu_item(ui, &palette, Some(icon), &text) {
            app.actions.push(Action::ToggleSaved(uri.clone()));
        }
        add_to_playlist_menu(ui, app, std::slice::from_ref(item));
    } else if menu_item(
        ui,
        &palette,
        Some(Icon::Bookmark),
        &gettext(locale, "Save episode"),
    ) {
        app.actions.push(Action::ToggleSaved(uri.clone()));
    }
    // Removal is URI-based, so it stays available on a sorted or
    // filtered view. Moves are positional, so they stay on the unsorted
    // context only, where screen positions match server positions.
    let editable = match context {
        Some(RowContext::Context {
            editable_playlist: Some((playlist_id, _)),
            ..
        }) => Some((playlist_id, true)),
        Some(RowContext::View {
            editable_playlist: Some((playlist_id, _)),
            ..
        }) => Some((playlist_id, false)),
        _ => None,
    };
    if let Some((playlist_id, can_move)) = editable {
        if can_move && let Some(index) = index {
            if index > 0
                && menu_item(
                    ui,
                    &palette,
                    Some(Icon::ChevronUp),
                    &gettext(locale, "Move up"),
                )
            {
                app.actions.push(Action::MoveInPlaylist {
                    playlist_id: playlist_id.clone(),
                    from: index as u32,
                    to: index as u32 - 1,
                });
            }
            if menu_item(
                ui,
                &palette,
                Some(Icon::ChevronDown),
                &gettext(locale, "Move down"),
            ) {
                app.actions.push(Action::MoveInPlaylist {
                    playlist_id: playlist_id.clone(),
                    from: index as u32,
                    to: index as u32 + 2,
                });
            }
        }
        if menu_item(
            ui,
            &palette,
            Some(Icon::Minus),
            &gettext(locale, "Remove from this playlist"),
        ) {
            app.actions.push(Action::RemoveFromPlaylist {
                playlist_id: playlist_id.clone(),
                uris: vec![uri.clone()],
            });
        }
    }
    menu_separator(ui, &palette);
    match item {
        PlayableItem::Track(track) => {
            if menu_item(
                ui,
                &palette,
                Some(Icon::Radio),
                &gettext(locale, "Go to song radio"),
            ) {
                app.actions.push(Action::Open(Page::Radio(uri.clone())));
            }
            let artists: Vec<&ArtistRef> = track
                .artists
                .iter()
                .filter(|artist| artist.id.is_some())
                .collect();
            if artists.len() == 1 {
                if menu_item(
                    ui,
                    &palette,
                    Some(Icon::User),
                    &gettext(locale, "Go to artist"),
                ) {
                    app.actions.push(Action::Open(Page::Artist(
                        artists[0].id.clone().unwrap_or_default(),
                    )));
                }
            } else if artists.len() > 1 {
                let label = gettext(locale, "Go to artist");
                menu_submenu(ui, &palette, Some(Icon::User), &label, |ui| {
                    ui.set_min_width(200.0);
                    for artist in &artists {
                        if menu_item(ui, &palette, Some(Icon::User), &artist.name) {
                            app.actions.push(Action::Open(Page::Artist(
                                artist.id.clone().unwrap_or_default(),
                            )));
                        }
                    }
                });
            }
            if let Some(album) = &track.album
                && !album.id.is_empty()
                && menu_item(
                    ui,
                    &palette,
                    Some(Icon::Disc),
                    &gettext(locale, "Go to album"),
                )
            {
                app.actions
                    .push(Action::Open(Page::Album(album.id.clone())));
            }
        }
        PlayableItem::Episode(episode) => {
            if let Some(show) = &episode.show
                && menu_item(
                    ui,
                    &palette,
                    Some(Icon::Mic),
                    &gettext(locale, "Go to podcast"),
                )
            {
                app.actions.push(Action::Open(Page::Show(show.id.clone())));
            }
        }
    }
    menu_separator(ui, &palette);
    if menu_item(
        ui,
        &palette,
        Some(Icon::Copy),
        &gettext(locale, "Copy link"),
    ) {
        app.actions.push(Action::CopyLink(uri.clone()));
    }
    if menu_item(
        ui,
        &palette,
        Some(Icon::ExternalLink),
        &gettext(locale, "Open in Spotify"),
    ) {
        app.actions.push(Action::OpenInSpotify(uri));
    }
}

/// Menu for a context (playlist, album, artist, show).
pub fn context_menu_items(
    ui: &mut Ui,
    app: &mut App,
    uri: &str,
    name: &str,
    owned_playlist: Option<&Playlist>,
) {
    let palette = app.palette;
    let locale = app.locale;
    ui.set_min_width(200.0);
    ui.set_max_width(300.0);
    let kind = util::uri_kind(uri).unwrap_or("");
    if menu_item(ui, &palette, Some(Icon::Play), &gettext(locale, "Play")) {
        app.actions.push(Action::PlayContext {
            uri: uri.to_string(),
            offset_uri: None,
            offset_index: None,
        });
    }
    if kind != "artist"
        && menu_item(
            ui,
            &palette,
            Some(Icon::Shuffle),
            &gettext(locale, "Shuffle play"),
        )
    {
        app.actions.push(Action::ShufflePlay(uri.to_string()));
    }
    if kind == "album"
        && menu_item(
            ui,
            &palette,
            Some(Icon::ListEnd),
            &gettext(locale, "Add to queue"),
        )
    {
        app.actions.push(Action::AddToQueue {
            uri: uri.to_string(),
            label: name.to_string(),
        });
    }
    let saved = app.is_saved(uri).unwrap_or(false);
    let (icon, text) = match (kind, saved) {
        ("artist", true) => (Icon::CircleX, pgettext(locale, "artist", "Unfollow")),
        ("artist", false) => (Icon::CirclePlus, pgettext(locale, "artist", "Follow")),
        (_, true) => (Icon::CircleX, gettext(locale, "Remove from Your Library")),
        (_, false) => (Icon::CirclePlus, gettext(locale, "Add to Your Library")),
    };
    if owned_playlist.is_none() && menu_item(ui, &palette, Some(icon), &text) {
        app.actions.push(Action::ToggleSaved(uri.to_string()));
    }
    if let Some(playlist) = owned_playlist {
        if menu_item(
            ui,
            &palette,
            Some(Icon::Pencil),
            &gettext(locale, "Edit details"),
        ) {
            app.actions.push(Action::ShowDialog(Dialog::EditPlaylist {
                cover: Default::default(),
                id: playlist.id.clone(),
                name: playlist.name.clone(),
                description: playlist
                    .description
                    .clone()
                    .map(|d| util::strip_html(&d))
                    .unwrap_or_default(),
                public: playlist.public,
            }));
        }
        if menu_item(ui, &palette, Some(Icon::Trash), &gettext(locale, "Delete")) {
            app.actions
                .push(Action::ShowDialog(Dialog::ConfirmDeletePlaylist {
                    id: playlist.id.clone(),
                    name: playlist.name.clone(),
                    owned: true,
                }));
        }
    }
    menu_separator(ui, &palette);
    let radio = match kind {
        "playlist" => Some(gettext(locale, "Go to playlist radio")),
        "album" => Some(gettext(locale, "Go to album radio")),
        "artist" => Some(gettext(locale, "Go to artist radio")),
        _ => None,
    };
    if let Some(label) = radio
        && util::station_uri(uri).is_some()
        && menu_item(ui, &palette, Some(Icon::Radio), &label)
    {
        app.actions.push(Action::Open(Page::Radio(uri.to_string())));
    }
    if menu_item(
        ui,
        &palette,
        Some(Icon::Copy),
        &gettext(locale, "Copy link"),
    ) {
        app.actions.push(Action::CopyLink(uri.to_string()));
    }
    if menu_item(
        ui,
        &palette,
        Some(Icon::ExternalLink),
        &gettext(locale, "Open in Spotify"),
    ) {
        app.actions.push(Action::OpenInSpotify(uri.to_string()));
    }
}

/// Whether a row can start playback through Spotify. Unknown availability
/// remains playable, while local files and missing entries cannot be requested.
pub(crate) fn row_playable(item: &PlayableItem) -> bool {
    !item.uri().is_empty()
        && !item.uri().starts_with("spotify:local:")
        && !matches!(item, PlayableItem::Track(track)
            if track.is_local || track.is_playable == Some(false))
}

/// Describes one row of a track table.
pub struct TrackRow<'a> {
    pub index: usize,
    pub number: Option<usize>,
    pub item: &'a PlayableItem,
    pub context: &'a RowContext,
    pub show_cover: bool,
    /// Show playback status over the cover when there is no number column.
    /// Loading and hover/focus controls are independent of this flag.
    pub show_playing_overlay: bool,
    pub show_album: bool,
    pub added_at: Option<&'a str>,
    /// Who put the song here, on playlists made together.
    pub added_by: Option<&'a str>,
    pub show_added_by: bool,
    pub compact: bool,
    /// One line for the name and the artists in a shorter row without the
    /// cover: the compact track list. `compact` stays the queue's narrow row.
    pub thin: bool,
    /// Vertical offset while rows part around the slot a dragged row
    /// would land in; 0.0 everywhere else.
    pub shift: f32,
    /// Whether this row is one of the picked-out ones.
    pub picked: bool,
    /// Every picked-out song in this table, in the order they sit in it, so
    /// the menu can update a destination playlist before Spotify answers.
    /// Empty where a list does not offer picking.
    pub picked_songs: &'a [PlayableItem],
}

/// Draw each credited artist separately so its Spotify id remains clickable.
pub(crate) fn artist_links(
    ui: &mut Ui,
    app: &mut App,
    artists: &[ArtistRef],
    font: egui::FontId,
    color: Color32,
) -> bool {
    let spacing = ui.spacing().item_spacing;
    let mut clicked = false;
    ui.spacing_mut().item_spacing.x = 0.0;
    for (index, artist) in artists.iter().enumerate() {
        if index > 0 {
            theme::text(ui, ", ", font.clone(), color);
        }
        // egui advances the cursor with the spacing of the widget just
        // drawn. The final artist needs the surrounding gap before the
        // next metadata separator, while commas between artists stay snug.
        if index + 1 == artists.len() {
            ui.spacing_mut().item_spacing = spacing;
        }
        if let Some(id) = &artist.id {
            if theme::link(ui, &artist.name, font.clone(), color).clicked() {
                app.actions.push(Action::Open(Page::Artist(id.clone())));
                clicked = true;
            }
        } else {
            theme::text(ui, &artist.name, font.clone(), color);
        }
    }
    ui.spacing_mut().item_spacing = spacing;
    clicked
}

/// Column widths of the track table, computed from the available width.
struct Columns {
    number: f32,
    cover: f32,
    album: f32,
    added_by: f32,
    added: f32,
    heart: f32,
    duration: f32,
    more: f32,
}

fn columns(width: f32, row: &TrackRow<'_>) -> Columns {
    let extra_wide = width > 920.0;
    let wide = width > 760.0;
    let medium = width > 560.0;
    Columns {
        number: if row.compact { 0.0 } else { 44.0 },
        cover: if row.show_cover {
            if row.compact { 44.0 } else { 52.0 }
        } else {
            0.0
        },
        album: if row.show_album && medium {
            (width * 0.28).clamp(140.0, 360.0)
        } else {
            0.0
        },
        added_by: if row.show_added_by && extra_wide {
            130.0
        } else {
            0.0
        },
        added: if row.added_at.is_some() && wide {
            120.0
        } else {
            0.0
        },
        heart: if row.compact { 0.0 } else { 36.0 },
        duration: if row.compact { 44.0 } else { 56.0 },
        more: if row.compact { 0.0 } else { 36.0 },
    }
}

#[derive(Clone, Copy)]
struct PlayingIndicatorClock {
    time: f64,
    last_time: f64,
    was_focused: bool,
}

/// Playback activity, not an audio visualiser. Only a visible playing row
/// calls this. Keep one clock per viewport so all rows freeze at their last
/// painted phase while unfocused, without per-song state or animation frames.
fn paint_playing_indicator(ui: &Ui, cell: Rect, color: Color32) {
    let icon = Rect::from_center_size(cell.center(), Vec2::splat(16.0));
    if !ui.is_rect_visible(icon) {
        return;
    }
    let (now, focused) = ui.input(|input| (input.time, input.focused));
    let clock_id = egui::Id::new(("playing-indicator-clock", ui.ctx().viewport_id()));
    let time = ui.data_mut(|data| {
        let clock = data.get_temp_mut_or_insert_with(clock_id, || PlayingIndicatorClock {
            time: now,
            last_time: now,
            was_focused: focused,
        });
        if focused && clock.was_focused {
            clock.time += now - clock.last_time;
        }
        clock.last_time = now;
        clock.was_focused = focused;
        clock.time
    });
    for (index, (speed, phase)) in [(1.1, 0.0), (1.4, 2.1), (0.9, 4.2), (1.25, 1.0)]
        .into_iter()
        .enumerate()
    {
        let wave = (time * std::f64::consts::TAU * speed + phase).sin();
        let height = 3.0 + 11.0 * (0.5 + 0.5 * wave) as f32;
        let left = icon.center().x - 5.5 + index as f32 * 3.0;
        let bottom = icon.bottom() - 1.0;
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(left, bottom - height), pos2(left + 2.0, bottom)),
            CornerRadius::ZERO,
            color,
        );
    }
    // Same cadence as the loading spinner, without an idle animation timer.
    if focused {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// Draws one song in a list.
///
/// Returns the selection behavior for a row-body click. The caller supplies
/// the display index because sorting and filtering change row positions.
pub fn track_row(ui: &mut Ui, app: &mut App, row: TrackRow<'_>) -> Option<RowPick> {
    track_row_response(ui, app, row).1
}

/// Also exposes the row body so a collection can navigate between whole songs.
pub(crate) fn track_row_response(
    ui: &mut Ui,
    app: &mut App,
    row: TrackRow<'_>,
) -> (egui::Response, Option<RowPick>) {
    // Virtual lists reuse the visible slots as they scroll. Keep focus and
    // accessibility actions attached to the song and its occurrence instead.
    // Now playing and Next up can both contain the same song at index zero.
    // Their actions have different meanings, so they must not share an ID.
    let id = ui.unique_id().with((
        "track-row",
        std::mem::discriminant(row.context),
        row.item.uri(),
        // Playback can omit unavailable rows; the displayed occurrence keeps
        // a distinct identity even when playback positions are compacted.
        row.number
            .map(|number| number.saturating_sub(1))
            .unwrap_or(row.index),
    ));
    ui.scope_builder(UiBuilder::new().id(id), |ui| {
        track_row_contents(ui, app, row)
    })
    .inner
}

fn track_row_contents(
    ui: &mut Ui,
    app: &mut App,
    row: TrackRow<'_>,
) -> (egui::Response, Option<RowPick>) {
    let palette = app.palette;
    let row_height = if row.thin {
        theme::THIN_ROW_HEIGHT
    } else if row.compact {
        theme::COMPACT_ROW_HEIGHT
    } else {
        theme::ROW_HEIGHT
    };
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, row_height), Sense::click_and_drag());
    let rect = rect.translate(vec2(0.0, row.shift));
    let unavailable = !row_playable(row.item);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled() && !unavailable,
            row.picked,
            gettext(
                app.locale,
                // Translators: {title} is a song or episode name, {subtitle} its artists or podcast.
                "Play {title}, {subtitle}",
            )
            .replace("{title}", row.item.name())
            .replace("{subtitle}", &row.item.subtitle()),
        )
    });
    if response.gained_focus() {
        response.scroll_to_me(None);
    }
    if !ui.is_rect_visible(rect) && !response.has_focus() && !response.clicked() {
        return (response, None);
    }
    // Start a sidebar drag only after egui's drag threshold.
    if row.item.is_track() && response.drag_started_by(egui::PointerButton::Primary) {
        let items = dragged_items(row.item, row.picked, row.picked_songs);
        // Keep the source index for moves within an editable playlist, or
        // within the manually queued section while it can be rewritten.
        let from = (items.len() == 1)
            .then(|| match row.context {
                RowContext::Context {
                    editable_playlist: Some((id, _)),
                    ..
                } => Some((id.clone(), row.index as u32)),
                RowContext::Queue
                    if row.index < app.queued_rows_len() && app.queue_locally_reorderable() =>
                {
                    Some(("queue".to_string(), row.index as u32))
                }
                _ => None,
            })
            .flatten();
        let preview = items.first().unwrap_or(row.item);
        egui::DragAndDrop::set_payload(
            ui.ctx(),
            DragTrack {
                title: preview.name().to_string(),
                image: preview.image(64).map(str::to_string),
                items,
                from,
            },
        );
    }
    // A queue row is a position, not the song itself: the same song can
    // sit in the queue while it plays (a repeat wrapping around, a song
    // queued twice), and only the Now playing row is the playing one.
    let is_current = !matches!(row.context, RowContext::Queue)
        && app
            .current_track_uri()
            .is_some_and(|uri| uri == row.item.uri());
    let playing = is_current && app.believed_playing();
    let hovered = ui.rect_contains_pointer(rect) || response.has_focus();
    if row.picked {
        // Keep the existing translucent selection, using a neutral palette
        // color so selecting a song does not mark it as playing.
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            palette
                .secondary
                .gamma_multiply(if hovered { 0.30 } else { 0.20 }),
        );
    } else if hovered {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            palette
                .surface_hover
                .gamma_multiply(if palette.dark { 0.7 } else { 1.0 }),
        );
    }
    // The row highlight also shows keyboard focus. Do not add an outline
    // when a mouse click gives the row focus for arrow-key navigation.
    let cols = columns(width, &row);
    let painter = ui.painter().clone();
    let mut x = rect.left() + 8.0;

    // Number / play.
    if cols.number > 0.0 {
        let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(cols.number, row_height));
        if app.play_pending(row.item.uri()) {
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(cell)
                    .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
            );
            theme::spinner(&mut child, 16.0, palette.accent);
        } else if hovered && !unavailable {
            let icon = if playing {
                Icon::PauseFilled
            } else {
                Icon::PlayFilled
            };
            theme::paint_icon(ui, icon, cell, 14.0, palette.text);
        } else if playing {
            paint_playing_indicator(ui, cell, palette.accent);
        } else {
            let color = if is_current {
                palette.accent
            } else {
                palette.secondary
            };
            let label = row.number.unwrap_or(row.index + 1).to_string();
            painter.text(
                cell.center(),
                egui::Align2::CENTER_CENTER,
                label,
                theme::regular(14.0),
                color,
            );
        }
        x += cols.number;
    }
    // Cover.
    if cols.cover > 0.0 {
        let size = if row.compact { 36.0 } else { 40.0 };
        let cover_rect = Rect::from_center_size(
            pos2(x + size / 2.0 + 2.0, rect.center().y),
            Vec2::splat(size),
        );
        paint_cover(
            ui,
            &palette,
            row.item.image(64),
            cover_rect,
            4.0,
            if row.item.is_track() {
                Icon::Music
            } else {
                Icon::Mic
            },
            Some(app.backend.art()),
        );
        // Without a number column the cover carries the play control:
        // hover shows it and a click uses it. The Queue keeps its artwork
        // clear while playing; other compact lists show playback status.
        if cols.number == 0.0 {
            let scrim = |alpha: u8| {
                painter.rect_filled(
                    cover_rect,
                    CornerRadius::same(4),
                    Color32::from_black_alpha(alpha),
                );
            };
            if app.play_pending(row.item.uri()) {
                scrim(140);
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .max_rect(cover_rect)
                        .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
                );
                theme::spinner(&mut child, 16.0, Color32::WHITE);
            } else if hovered && !unavailable {
                scrim(140);
                let icon = if playing {
                    Icon::PauseFilled
                } else {
                    Icon::PlayFilled
                };
                theme::paint_icon(ui, icon, cover_rect, 16.0, Color32::WHITE);
            } else if playing && row.show_playing_overlay {
                scrim(110);
                paint_playing_indicator(ui, cover_rect, palette.accent);
            }
        }
        x += cols.cover;
    }
    let right_fixed = cols.heart + cols.duration + cols.more + 8.0;
    let text_right = rect.right() - right_fixed - cols.added - cols.added_by - cols.album;
    let title_rect = Rect::from_min_max(
        pos2(x, rect.top()),
        pos2((text_right - 12.0).max(x), rect.bottom()),
    );

    // Title and artists.
    let title_color = if unavailable {
        palette.dim
    } else if is_current {
        palette.accent
    } else {
        palette.text
    };
    let subtitle_color = if hovered {
        palette.text
    } else {
        palette.secondary
    };
    if row.thin {
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(title_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        child.set_clip_rect(title_rect.intersect(ui.clip_rect()));
        child.spacing_mut().item_spacing = vec2(6.0, 0.0);
        theme::text(
            &mut child,
            row.item.name(),
            theme::medium(14.0),
            title_color,
        );
        match row.item {
            PlayableItem::Track(track) => {
                if track.explicit {
                    explicit_badge(&mut child, &palette);
                }
                theme::text(
                    &mut child,
                    "•",
                    theme::regular(12.0),
                    palette.secondary.gamma_multiply(0.6),
                );
                artist_links(
                    &mut child,
                    app,
                    &track.artists,
                    theme::regular(13.0),
                    subtitle_color,
                );
                if let Some(added) = row.added_at.filter(|a| !a.starts_with("1970-01-01"))
                    && cols.added == 0.0
                {
                    let label =
                        util::format_relative_date(app.locale, added, jiff::Timestamp::now());
                    theme::text(
                        &mut child,
                        "•",
                        theme::regular(12.0),
                        palette.secondary.gamma_multiply(0.6),
                    );
                    theme::text(&mut child, &label, theme::regular(12.0), palette.secondary);
                    if label.ends_with(" ago") {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_secs(1));
                    }
                }
            }
            PlayableItem::Episode(episode) => {
                let subtitle = episode
                    .show
                    .as_ref()
                    .map(|show| show.name.clone())
                    .unwrap_or_default();
                if !subtitle.is_empty() {
                    theme::text(
                        &mut child,
                        "•",
                        theme::regular(12.0),
                        palette.secondary.gamma_multiply(0.6),
                    );
                    let show_id = episode.show.as_ref().map(|show| show.id.clone());
                    let response =
                        theme::link(&mut child, subtitle, theme::regular(13.0), subtitle_color);
                    if response.clicked()
                        && let Some(id) = show_id
                    {
                        app.actions.push(Action::Open(Page::Show(id)));
                    }
                }
                if let Some(added) = row.added_at.filter(|a| !a.starts_with("1970-01-01"))
                    && cols.added == 0.0
                {
                    theme::text(
                        &mut child,
                        "•",
                        theme::regular(12.0),
                        palette.secondary.gamma_multiply(0.6),
                    );
                    let label =
                        util::format_relative_date(app.locale, added, jiff::Timestamp::now());
                    theme::text(&mut child, &label, theme::regular(12.0), palette.secondary);
                    if label.ends_with(" ago") {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_secs(1));
                    }
                }
            }
        }
    } else {
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(title_rect)
                .layout(Layout::top_down(Align::LEFT)),
        );
        child.set_clip_rect(title_rect.intersect(ui.clip_rect()));
        child.spacing_mut().item_spacing = vec2(6.0, 1.0);
        child.spacing_mut().interact_size.y = 16.0;
        let vertical_pad = ((row_height - 37.0) / 2.0).max(4.0);
        child.add_space(vertical_pad);
        child.horizontal(|ui| {
            ui.set_max_width(title_rect.width());
            theme::text(ui, row.item.name(), theme::medium(14.5), title_color);
        });
        child.horizontal(|ui| {
            ui.set_max_width(title_rect.width());
            match row.item {
                PlayableItem::Track(track) => {
                    if track.explicit {
                        explicit_badge(ui, &palette);
                    }
                    artist_links(
                        ui,
                        app,
                        &track.artists,
                        theme::regular(12.5),
                        subtitle_color,
                    );
                    if let Some(added) = row.added_at.filter(|a| !a.starts_with("1970-01-01"))
                        && cols.added == 0.0
                    {
                        theme::text(
                            ui,
                            "•",
                            theme::regular(12.0),
                            palette.secondary.gamma_multiply(0.6),
                        );
                        let label =
                            util::format_relative_date(app.locale, added, jiff::Timestamp::now());
                        theme::text(ui, &label, theme::regular(12.0), palette.secondary);
                        if label.ends_with(" ago") {
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_secs(1));
                        }
                    }
                }
                PlayableItem::Episode(episode) => {
                    let subtitle = episode
                        .show
                        .as_ref()
                        .map(|show| show.name.clone())
                        .unwrap_or_default();
                    let show_id = episode.show.as_ref().map(|show| show.id.clone());
                    let response = theme::link(ui, subtitle, theme::regular(12.5), subtitle_color);
                    if response.clicked()
                        && let Some(id) = show_id
                    {
                        app.actions.push(Action::Open(Page::Show(id)));
                    }
                    if let Some(added) = row.added_at.filter(|a| !a.starts_with("1970-01-01"))
                        && cols.added == 0.0
                    {
                        theme::text(
                            ui,
                            "•",
                            theme::regular(12.0),
                            palette.secondary.gamma_multiply(0.6),
                        );
                        let label =
                            util::format_relative_date(app.locale, added, jiff::Timestamp::now());
                        theme::text(ui, &label, theme::regular(12.0), palette.secondary);
                        if label.ends_with(" ago") {
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_secs(1));
                        }
                    }
                }
            }
        });
    }
    x = text_right;

    // Album.
    if cols.album > 0.0 {
        if let PlayableItem::Track(track) = row.item
            && let Some(album) = &track.album
        {
            let album_rect = Rect::from_min_max(
                pos2(x, rect.top()),
                pos2(x + cols.album - 12.0, rect.bottom()),
            );
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(album_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            child.set_clip_rect(album_rect.intersect(ui.clip_rect()));
            let response = theme::link(
                &mut child,
                album.name.clone(),
                theme::regular(13.0),
                subtitle_color,
            );
            if response.clicked() && !album.id.is_empty() {
                app.actions
                    .push(Action::Open(Page::Album(album.id.clone())));
            }
        }
        x += cols.album;
    }
    // Added by.
    if cols.added_by > 0.0 {
        if let Some(adder) = row.added_by {
            let cell = Rect::from_min_max(
                pos2(x, rect.top()),
                pos2(x + cols.added_by - 12.0, rect.bottom()),
            );
            let clipped = painter.with_clip_rect(cell.intersect(ui.clip_rect()));
            crate::bidi::paint_line(
                &clipped,
                cell.left(),
                cell.right(),
                cell.center().y,
                adder,
                theme::regular(13.0),
                palette.secondary,
            );
        }
        x += cols.added_by;
    }
    // Date added.
    if cols.added > 0.0 {
        // Spotify stamps the epoch on dates it never recorded; an empty
        // cell is truer than January 1970.
        if let Some(added) = row
            .added_at
            .filter(|added| !added.starts_with("1970-01-01"))
        {
            let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(cols.added, row_height));
            let label = util::format_relative_date(app.locale, added, jiff::Timestamp::now());
            painter.text(
                pos2(cell.left(), cell.center().y),
                egui::Align2::LEFT_CENTER,
                &label,
                theme::regular(13.0),
                palette.secondary,
            );
            // Relative labels cross a boundary while the table is idle, so
            // keep the visible value in step with the clock.
            if label.ends_with(" ago") {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs(1));
            }
        }
        x += cols.added;
    }

    // Heart.
    if cols.heart > 0.0 {
        let saved = app.is_saved(row.item.uri());
        let heart_rect = Rect::from_min_size(pos2(x, rect.top()), vec2(cols.heart, row_height));
        if row.item.is_track() {
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(heart_rect)
                    .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
            );
            if !hovered
                && saved != Some(true)
                && !child.memory(|memory| memory.has_focus(child.next_auto_id()))
            {
                child.set_opacity(0.0);
            }
            let (icon, color) = if saved == Some(true) {
                (Icon::HeartFilled, palette.accent)
            } else {
                (Icon::Heart, palette.secondary)
            };
            let tooltip = if saved == Some(true) {
                gettext(app.locale, "Remove from Liked Songs")
            } else {
                gettext(app.locale, "Save to Liked Songs")
            };
            if theme::icon_button(&mut child, icon, 16.0, color, palette.text, &tooltip).clicked() {
                app.actions
                    .push(Action::ToggleSaved(row.item.uri().to_string()));
            }
        }
        x += cols.heart;
    }

    // Duration.
    let duration_rect = Rect::from_min_size(pos2(x, rect.top()), vec2(cols.duration, row_height));
    painter.text(
        pos2(duration_rect.right() - 6.0, duration_rect.center().y),
        egui::Align2::RIGHT_CENTER,
        util::format_duration_ms(row.item.duration_ms()),
        theme::regular(13.0),
        palette.secondary,
    );
    x += cols.duration;

    // More.
    // The row's menu stays alive while it is open: when the button existed
    // only on a hovered row, the pointer's trip to the menu could leave
    // the row and close it before anything was clicked.
    let menu_id = ui.id().with(("row-menu", row.index));
    if cols.more > 0.0 {
        let more_rect = Rect::from_min_size(pos2(x, rect.top()), vec2(cols.more, row_height));
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(more_rect)
                .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
        );
        if !hovered
            && !egui::Popup::is_id_open(ui.ctx(), menu_id)
            && !child.memory(|memory| memory.has_focus(child.next_auto_id()))
        {
            child.set_opacity(0.0);
        }
        let more = theme::icon_button(
            &mut child,
            Icon::Ellipsis,
            18.0,
            palette.secondary,
            palette.text,
            &gettext(app.locale, "More"),
        );
        egui::Popup::menu(&more)
            .id(menu_id)
            .frame(menu_frame(&palette))
            .show(|ui| item_menu(ui, app, row.item, Some(row.context), Some(row.index)));
    }

    // Row interactions.
    let mut pick = None;
    let accessible_click = response.clicked() && response.interact_pointer_pos().is_none();
    if (response.double_clicked() || accessible_click) && !unavailable {
        app.actions.push(Action::PlayFromRow {
            context: row.context.clone(),
            uri: row.item.uri().to_string(),
            index: row.index as u32,
        });
    } else if response.clicked() {
        // The cell that holds the play control: the number column when
        // there is one, the cover when there is not.
        let control = if cols.number > 0.0 {
            Some(vec2(cols.number, row_height))
        } else if cols.cover > 0.0 {
            Some(vec2(cols.cover, row_height))
        } else {
            None
        };
        let on_control = control.is_some_and(|size| {
            let control_rect = Rect::from_min_size(pos2(rect.left() + 8.0, rect.top()), size);
            response
                .interact_pointer_pos()
                .is_some_and(|pos| control_rect.contains(pos))
        });
        if on_control && !unavailable {
            if is_current {
                app.actions.push(Action::TogglePlay);
            } else {
                app.actions.push(Action::PlayFromRow {
                    context: row.context.clone(),
                    uri: row.item.uri().to_string(),
                    index: row.index as u32,
                });
            }
        } else if !on_control {
            // The body of the row, which plays nothing on a single click.
            response.request_focus();
            let modifiers = ui.input(|input| input.modifiers);
            pick = Some(if modifiers.shift {
                RowPick::Range
            } else if modifiers.command {
                RowPick::Toggle
            } else {
                RowPick::Only
            });
        }
    }
    egui::Popup::context_menu(&response)
        .frame(menu_frame(&palette))
        .show(|ui| {
            // Right-clicking one of several picked rows acts on all of
            // them; on anything else it is the ordinary single-song menu,
            // including a picked row that is the only one picked.
            if row.picked && row.picked_songs.len() > 1 {
                // Every picked row shares this table's context, so one
                // editable playlist covers the whole selection, including
                // a sorted or filtered view where removal stays URI-based.
                let editable = match row.context {
                    RowContext::Context {
                        editable_playlist: Some(playlist),
                        ..
                    }
                    | RowContext::View {
                        editable_playlist: Some(playlist),
                        ..
                    } => Some(playlist),
                    _ => None,
                };
                picked_menu(ui, app, row.picked_songs, editable);
            } else {
                item_menu(ui, app, row.item, Some(row.context), Some(row.index));
            }
        });
    crate::autoscroll::row(ui, &response);
    (response, pick)
}

/// Scroll the enclosing list while a held drag approaches its visible edges.
/// Call only from a list that accepts the current payload.
pub(crate) fn scroll_during_drag(ui: &Ui) {
    let viewport = ui.clip_rect();
    let Some(pos) = ui.ctx().pointer_hover_pos() else {
        return;
    };
    if !ui.is_enabled()
        || !ui.rect_contains_pointer(viewport)
        || !ui.input(|input| input.focused && input.pointer.primary_down())
    {
        return;
    }
    let edge = 48.0_f32.min(viewport.height() / 2.0);
    if edge <= 0.0 {
        return;
    }
    let strength = if pos.y < viewport.top() + edge {
        (viewport.top() + edge - pos.y) / edge
    } else if pos.y > viewport.bottom() - edge {
        -(pos.y - viewport.bottom() + edge) / edge
    } else {
        return;
    };
    // Logical pixels per second, independent of display scale and frame rate.
    // No animation tail: moving away from the edge or dropping stops at once.
    let delta = strength * 900.0 * ui.input(|input| input.stable_dt.min(0.05));
    ui.scroll_with_delta_animation(vec2(0.0, delta), egui::style::ScrollAnimation::none());
    ui.ctx().request_repaint();
}

fn dragged_items(
    item: &PlayableItem,
    picked: bool,
    picked_songs: &[PlayableItem],
) -> Vec<PlayableItem> {
    if picked && !picked_songs.is_empty() {
        picked_songs.to_vec()
    } else {
        vec![item.clone()]
    }
}

fn drag_label(locale: Locale, track: &DragTrack) -> String {
    match track.items.as_slice() {
        [] => track.title.clone(),
        [item] => item.name().to_string(),
        [first, rest @ ..] => ngettext(
            locale,
            // Translators: The label beside the pointer while songs are dragged. {name} is the first song's name and {count} how many more songs are dragged with it.
            "{name} + {count} more",
            "{name} + {count} more",
            rest.len() as u32,
        )
        .replace("{name}", first.name())
        .replace("{count}", &rest.len().to_string()),
    }
}

/// The chip that rides the pointer while a song is being dragged.
pub fn drag_ghost(ctx: &egui::Context, palette: &Palette, locale: Locale) {
    // A song and a sidebar row ride the pointer the same way.
    let chip = egui::DragAndDrop::payload::<DragTrack>(ctx)
        .map(|track| (drag_label(locale, &track), track.image.clone()))
        .or_else(|| {
            egui::DragAndDrop::payload::<DragEntry>(ctx)
                .map(|entry| (entry.title.clone(), entry.image.clone()))
        });
    let Some((title, image)) = chip else {
        return;
    };
    // The payload lives through the release frame; the chip should not.
    if !ctx.input(|input| input.pointer.any_down()) {
        return;
    }
    let Some(pos) = ctx.pointer_latest_pos() else {
        return;
    };
    egui::Area::new(egui::Id::new("drag-ghost"))
        .order(egui::Order::Tooltip)
        .interactable(false)
        .fixed_pos(pos + vec2(16.0, 6.0))
        .show(ctx, |ui| {
            ui.set_opacity(0.9);
            egui::Frame::new()
                .fill(palette.overlay)
                .stroke(Stroke::new(1.0, palette.outline))
                .corner_radius(CornerRadius::same(theme::RADIUS))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 16,
                    spread: 0,
                    color: palette.shadow,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.set_max_width(280.0);
                        ui.spacing_mut().item_spacing.x = 8.0;
                        cover(ui, palette, image.as_deref(), 24.0, 4.0, Icon::Music);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&title)
                                    .font(theme::medium(13.0))
                                    .color(palette.text),
                            )
                            .truncate()
                            .selectable(false),
                        );
                    });
                });
        });
}

pub fn explicit_badge(ui: &mut Ui, palette: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(2), palette.secondary);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "E",
        theme::bold(9.5),
        palette.window,
    );
}

/// The header row above a track table.
/// The column headings above a track table. Answers with the heading that
/// was clicked, so the table can sort by it.
// The column switches and the language are independent inputs of one
// drawing call; a struct would exist only to carry them here.
#[expect(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub fn table_header(
    ui: &mut Ui,
    palette: &Palette,
    locale: Locale,
    show_album: bool,
    show_added: bool,
    show_added_by: bool,
    show_cover: bool,
    sort: Option<crate::model::TableSort>,
) -> Option<crate::model::SortColumn> {
    use crate::model::SortColumn;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
    let font = theme::regular(12.0);
    let color = palette.secondary;
    let mut clicked = None;
    let mut heading = |ui: &mut Ui, x: f32, text: &str, column: SortColumn| {
        let active = sort.filter(|sort| sort.column == column);
        let galley =
            ui.painter()
                .layout_no_wrap(text.to_string(), font.clone(), egui::Color32::PLACEHOLDER);
        let size = galley.size();
        let arrow_room = if active.is_some() { 13.0 } else { 0.0 };
        let top_left = pos2(x, rect.center().y - size.y / 2.0);
        let head =
            Rect::from_min_size(top_left, size + vec2(arrow_room, 0.0)).expand2(vec2(4.0, 8.0));
        let response = ui.interact(head, ui.id().with(("table-header", text)), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                gettext(
                    locale,
                    // Translators: {column} is a column heading of a song list, such as Title.
                    "Sort by {column}",
                )
                .replace("{column}", text),
            )
        });
        theme::focus_ring(ui, &response);
        let color = if active.is_some() {
            palette.accent
        } else if response.hovered() {
            palette.text
        } else {
            color
        };
        ui.painter().galley(top_left, galley, color);
        if let Some(sort) = active {
            // Drawn, not typed: an arrow glyph relies on the loaded fonts
            // and rendered as a hollow box on some machines.
            let center = pos2(top_left.x + size.x + 8.0, rect.center().y);
            let (wing, tip) = if sort.ascending {
                (2.8, -3.2)
            } else {
                (-2.8, 3.2)
            };
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    center + vec2(-4.0, wing),
                    center + vec2(4.0, wing),
                    center + vec2(0.0, tip),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        if response.clicked() {
            clicked = Some(column);
        }
    };
    let mut number_clicked = false;
    let mut x = rect.left() + 8.0;
    {
        let number = Rect::from_center_size(pos2(x + 22.0, rect.center().y), vec2(30.0, 22.0));
        // With no sort chosen the list already plays its own order, and
        // the # says so: lit, arrow pointing down the list.
        let natural = sort.is_none();
        let active = sort.filter(|sort| sort.column == SortColumn::Index);
        let response = ui.interact(number, ui.id().with("table-header-number"), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                gettext(locale, "Sort by playlist order"),
            )
        });
        theme::focus_ring(ui, &response);
        let number_color = if natural || active.is_some() {
            palette.accent
        } else if response.hovered() {
            palette.text
        } else {
            color
        };
        ui.painter().text(
            number.center(),
            egui::Align2::CENTER_CENTER,
            "#",
            font.clone(),
            number_color,
        );
        if let Some(ascending) = active
            .map(|sort| sort.ascending)
            .or(natural.then_some(true))
        {
            let center = pos2(number.center().x + 12.0, rect.center().y);
            let (wing, tip) = if ascending { (2.8, -3.2) } else { (-2.8, 3.2) };
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    center + vec2(-4.0, wing),
                    center + vec2(4.0, wing),
                    center + vec2(0.0, tip),
                ],
                number_color,
                egui::Stroke::NONE,
            ));
        }
        if response
            .on_hover_text(gettext(locale, "Original order, reversed"))
            .clicked()
        {
            number_clicked = true;
        }
    }
    x += 44.0;
    if show_cover {
        x += 52.0;
    }
    heading(
        ui,
        x,
        &pgettext(locale, "column heading", "TITLE"),
        SortColumn::Title,
    );
    let medium = width > 560.0;
    let wide = width > 760.0;
    let album_width = if show_album && medium {
        (width * 0.28).clamp(140.0, 360.0)
    } else {
        0.0
    };
    let added_width = if show_added && wide { 120.0 } else { 0.0 };
    let extra_wide = width > 920.0;
    let added_by_width = if show_added_by && extra_wide {
        130.0
    } else {
        0.0
    };
    let right_fixed = 36.0 + 56.0 + 36.0 + 8.0;
    let mut cx = rect.right() - right_fixed - added_width - added_by_width - album_width;
    if album_width > 0.0 {
        heading(
            ui,
            cx,
            &pgettext(locale, "column heading", "ALBUM"),
            SortColumn::Album,
        );
        cx += album_width;
    }
    if added_by_width > 0.0 {
        heading(
            ui,
            cx,
            &pgettext(locale, "column heading", "ADDED BY"),
            SortColumn::AddedBy,
        );
        cx += added_by_width;
    }
    if added_width > 0.0 {
        heading(
            ui,
            cx,
            &pgettext(locale, "column heading", "DATE ADDED"),
            SortColumn::Added,
        );
    }
    if number_clicked {
        clicked = Some(SortColumn::Index);
    }
    let clock = Rect::from_center_size(
        pos2(rect.right() - 36.0 - 56.0 / 2.0 - 6.0, rect.center().y),
        Vec2::splat(15.0),
    );
    let duration_active = sort.is_some_and(|sort| sort.column == SortColumn::Duration);
    let response = ui.interact(
        clock.expand(8.0),
        ui.id().with("table-header-duration"),
        Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            gettext(locale, "Sort by duration"),
        )
    });
    theme::focus_ring(ui, &response);
    let clock_color = if duration_active {
        palette.accent
    } else if response.hovered() {
        palette.text
    } else {
        color
    };
    Icon::Clock.image(clock_color, 15.0).paint_at(ui, clock);
    if let Some(sort) = sort.filter(|sort| sort.column == SortColumn::Duration) {
        let center = pos2(clock.right() + 9.0, rect.center().y);
        let (wing, tip) = if sort.ascending {
            (2.8, -3.2)
        } else {
            (-2.8, 3.2)
        };
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                center + vec2(-4.0, wing),
                center + vec2(4.0, wing),
                center + vec2(0.0, tip),
            ],
            clock_color,
            egui::Stroke::NONE,
        ));
    }
    if response
        .on_hover_text(gettext(locale, "Sort by duration"))
        .clicked()
    {
        clicked = Some(SortColumn::Duration);
    }
    ui.painter().hline(
        rect.x_range().shrink(8.0),
        rect.bottom() - 0.5,
        Stroke::new(1.0, palette.outline),
    );
    ui.add_space(6.0);
    clicked
}

/// Lays out text limited to `max_rows` lines, ending with an ellipsis.
pub fn ellipsized(
    ui: &Ui,
    text: &str,
    font: egui::FontId,
    color: Color32,
    width: f32,
    max_rows: usize,
) -> std::sync::Arc<egui::Galley> {
    crate::bidi::layout(
        ui.painter(),
        text,
        font,
        color,
        width,
        max_rows,
        Some(crate::bidi::ELLIPSIS),
    )
}

pub struct CardResponse {
    /// Give attached menus an item-based ID: hovering a Play button can shift
    /// later cards' automatic response IDs.
    pub response: egui::Response,
    pub clicked: bool,
    pub play: bool,
}

/// Fixed height of a [`card`] row for virtualised grids.
pub fn card_row_height(ui: &mut Ui) -> f32 {
    const PAD: f32 = 12.0;
    const TITLE_GAP: f32 = 10.0;
    const SUBTITLE_GAP: f32 = 2.0;
    const BOTTOM_PAD: f32 = 8.0;
    let image_size = CARD_WIDTH - 2.0 * PAD;
    let title_font = theme::semibold(14.0);
    let subtitle_font = theme::regular(12.5);
    let (title_row, subtitle_row) = ui.fonts_mut(|fonts| {
        (
            fonts.row_height(&title_font),
            fonts.row_height(&subtitle_font),
        )
    });
    PAD + image_size + TITLE_GAP + title_row + SUBTITLE_GAP + 2.0 * subtitle_row + BOTTOM_PAD
}

/// A cover-and-title card for grids and shelves.
pub fn card(
    ui: &mut Ui,
    app: &mut App,
    image: Option<&str>,
    title: &str,
    subtitle: &str,
    round: bool,
    playable: bool,
) -> CardResponse {
    let palette = app.palette;
    const PAD: f32 = 12.0;
    const TITLE_GAP: f32 = 10.0;
    const SUBTITLE_GAP: f32 = 2.0;
    const BOTTOM_PAD: f32 = 8.0;
    let image_size = CARD_WIDTH - 2.0 * PAD;
    let text_width = image_size;
    let title_font = theme::semibold(14.0);
    let subtitle_font = theme::regular(12.5);
    // Every card reserves the title row and two subtitle rows, whatever its
    // own subtitle needs: a two-line subtitle then sits inside the hover
    // background, and a shelf that mixes one- and two-line subtitles keeps
    // its covers on one line instead of centring cards of different heights.
    let (title_row, subtitle_row) = ui.fonts_mut(|fonts| {
        (
            fonts.row_height(&title_font),
            fonts.row_height(&subtitle_font),
        )
    });
    let height =
        PAD + image_size + TITLE_GAP + title_row + SUBTITLE_GAP + 2.0 * subtitle_row + BOTTOM_PAD;
    let (rect, response) = ui.allocate_exact_size(vec2(CARD_WIDTH, height), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("{title}, {subtitle}"),
        )
    });
    if response.gained_focus() {
        response.scroll_to_me(None);
    }
    let mut play = false;
    if ui.is_rect_visible(rect) {
        let hovered = ui.rect_contains_pointer(rect);
        if hovered {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(theme::RADIUS),
                palette
                    .surface_hover
                    .gamma_multiply(if palette.dark { 0.8 } else { 1.0 }),
            );
        }
        let image_rect = Rect::from_min_size(rect.min + vec2(PAD, PAD), Vec2::splat(image_size));
        let radius = if round { image_size / 2.0 } else { 6.0 };
        paint_shadow(ui, &palette, image_rect, radius);
        paint_cover(
            ui,
            &palette,
            image,
            image_rect,
            radius,
            if round { Icon::User } else { Icon::Music },
            Some(app.backend.art()),
        );
        let text_left = rect.left() + PAD;
        let title_galley = ellipsized(ui, title, title_font, palette.text, text_width, 1);
        let title_rect = Rect::from_min_size(
            pos2(text_left, image_rect.bottom() + TITLE_GAP),
            vec2(text_width, title_row),
        );
        let title_pos = match title_galley.job.halign {
            Align::RIGHT => pos2(title_rect.right(), title_rect.top()),
            Align::Center => pos2(title_rect.center().x, title_rect.top()),
            _ => title_rect.min,
        };
        ui.painter().galley(title_pos, title_galley, palette.text);
        let subtitle_galley = ellipsized(
            ui,
            subtitle,
            subtitle_font,
            palette.secondary,
            text_width,
            2,
        );
        let subtitle_rect = Rect::from_min_size(
            pos2(text_left, title_rect.bottom() + SUBTITLE_GAP),
            vec2(text_width, 2.0 * subtitle_row),
        );
        let subtitle_pos = match subtitle_galley.job.halign {
            Align::RIGHT => pos2(subtitle_rect.right(), subtitle_rect.top()),
            Align::Center => pos2(subtitle_rect.center().x, subtitle_rect.top()),
            _ => subtitle_rect.min,
        };
        ui.painter()
            .galley(subtitle_pos, subtitle_galley, palette.secondary);

        if playable && hovered {
            let button_rect = Rect::from_center_size(
                pos2(image_rect.right() - 26.0, image_rect.bottom() - 26.0),
                Vec2::splat(44.0),
            );
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(button_rect)
                    .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
            );
            play = theme::circle_button(
                &mut child,
                Icon::PlayFilled,
                44.0,
                palette.accent,
                palette.accent_hover,
                palette.on_accent,
                &gettext(app.locale, "Play"),
            )
            .clicked();
        }
    }
    crate::autoscroll::row(ui, &response);
    theme::focus_ring(ui, &response);
    CardResponse {
        clicked: response.clicked() && !play,
        response,
        play,
    }
}

/// A horizontal shelf of cards with a title.
pub fn shelf(
    ui: &mut Ui,
    palette: &Palette,
    id: &str,
    title: &str,
    add_contents: impl FnOnce(&mut Ui),
) {
    ui.add_space(8.0);
    theme::section_title(ui, palette, title);
    ui.add_space(4.0);
    crate::autoscroll::show(
        ui,
        egui::ScrollArea::horizontal().id_salt(id),
        egui::Vec2b::new(true, false),
        |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = CARD_GAP / 2.0;
                add_contents(ui);
            });
        },
    );
    ui.add_space(12.0);
}

/// A wrapping grid of cards.
pub fn grid(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(CARD_GAP / 2.0, CARD_GAP);
        add_contents(ui);
    });
}

pub fn loading_row(ui: &mut Ui, palette: &Palette, locale: Locale) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        theme::spinner(ui, 18.0, palette.accent);
        theme::subtle(ui, palette, &gettext(locale, "Loading…"));
    });
}

pub fn error_row(ui: &mut Ui, app: &mut App, message: &str, retry: Option<Page>) {
    let palette = app.palette;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        theme::icon(ui, Icon::CircleAlert, 16.0, palette.danger);
        theme::text(ui, message, theme::regular(13.0), palette.secondary);
        if let Some(page) = retry
            && theme::soft_button(
                ui,
                &palette,
                Some(Icon::Refresh),
                &gettext(app.locale, "Retry"),
                false,
            )
            .clicked()
        {
            app.actions.push(Action::Reload(page));
        }
    });
}

pub fn empty_state(ui: &mut Ui, palette: &Palette, icon: Icon, title: &str, body: &str) {
    ui.add_space(48.0);
    ui.vertical_centered(|ui| {
        theme::icon(ui, icon, 40.0, palette.dim);
        ui.add_space(8.0);
        theme::text(ui, title, theme::semibold(16.0), palette.text);
        ui.add_space(2.0);
        theme::text(ui, body, theme::regular(13.5), palette.secondary);
    });
}

pub enum SliderEvent {
    None,
    Dragging(f32),
    Committed(f32),
}

/// Whole notches the wheel turned over `response` since last asked, up
/// being positive. A mouse's detent is one event, however many lines the
/// system multiplies it into (Windows says three by default, #103); a
/// free-spinning wheel's fractional lines and a trackpad's points add up
/// to the same steps, fifty points to a notch.
pub fn wheel_notches(ui: &Ui, response: &egui::Response) -> i32 {
    const NOTCH: f32 = 50.0;
    if !response.hovered() {
        return 0;
    }
    let (lines, points) = ui.input(|input| {
        let mut lines = 0.0f32;
        let mut points = 0.0f32;
        for event in &input.events {
            if let egui::Event::MouseWheel { unit, delta, .. } = event {
                match unit {
                    egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => {
                        lines += if delta.y.abs() >= 1.0 {
                            delta.y.signum()
                        } else {
                            delta.y
                        };
                    }
                    egui::MouseWheelUnit::Point => points += delta.y,
                }
            }
        }
        (lines, points)
    });
    let id = response.id.with("wheel");
    let total = ui.data(|data| data.get_temp::<f32>(id)).unwrap_or(0.0) + points + lines * NOTCH;
    let notches = (total / NOTCH).trunc();
    ui.data_mut(|data| data.insert_temp(id, total - notches * NOTCH));
    notches as i32
}

/// A thin horizontal slider whose handle appears on hover, for seeking and
/// volume. `value` is 0..=1.
pub fn thin_slider(
    ui: &mut Ui,
    palette: &Palette,
    id: egui::Id,
    label: &str,
    value: f32,
    width: f32,
    wheel_step: Option<f32>,
) -> SliderEvent {
    let (_, rect) = ui.allocate_space(vec2(width, 16.0));
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let dragging_value = ui.data(|data| data.get_temp::<f32>(id));
    let pointer_value = response
        .interact_pointer_pos()
        .map(|pos| ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let mut event = SliderEvent::None;
    if (response.drag_started() || response.dragged())
        && let Some(v) = pointer_value
    {
        ui.data_mut(|data| data.insert_temp(id, v));
        event = SliderEvent::Dragging(v);
    }
    if response.drag_stopped() {
        let v = dragging_value.or(pointer_value).unwrap_or(value);
        ui.data_mut(|data| data.remove::<f32>(id));
        event = SliderEvent::Committed(v);
    } else if response.clicked()
        && let Some(v) = pointer_value
    {
        event = SliderEvent::Committed(v);
    }
    if let Some(step) = wheel_step {
        let notches = wheel_notches(ui, &response);
        if notches != 0 {
            event = SliderEvent::Committed((value + step * notches as f32).clamp(0.0, 1.0));
        }
    }
    let step = wheel_step.unwrap_or(0.01);
    let focused = response.has_focus();
    if focused {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            )
        });
    }
    if response.enabled() {
        ui.input(|input| {
            use egui::accesskit::{Action, ActionData};
            let mut change = input.num_accesskit_action_requests(response.id, Action::Increment)
                as i32
                - input.num_accesskit_action_requests(response.id, Action::Decrement) as i32;
            if focused {
                change += input.num_presses(egui::Key::ArrowRight) as i32
                    - input.num_presses(egui::Key::ArrowLeft) as i32;
            }
            if change != 0 {
                event = SliderEvent::Committed((value + change as f32 * step).clamp(0.0, 1.0));
            }
            for request in input.accesskit_action_requests(response.id, Action::SetValue) {
                if let Some(ActionData::NumericValue(value)) = request.data
                    && value.is_finite()
                {
                    event = SliderEvent::Committed((value / 100.0).clamp(0.0, 1.0) as f32);
                }
            }
        });
    }
    let shown = match &event {
        SliderEvent::Dragging(v) => *v,
        SliderEvent::Committed(v) => *v,
        SliderEvent::None => dragging_value.unwrap_or(value),
    };
    response
        .widget_info(|| egui::WidgetInfo::slider(ui.is_enabled(), f64::from(shown) * 100.0, label));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        use egui::accesskit::Action;
        node.set_min_numeric_value(0.0);
        node.set_max_numeric_value(100.0);
        node.set_numeric_value_step(f64::from(step) * 100.0);
        node.add_action(Action::SetValue);
        if shown > 0.0 {
            node.add_action(Action::Decrement);
        }
        if shown < 1.0 {
            node.add_action(Action::Increment);
        }
    });
    if ui.is_rect_visible(rect) {
        let active = response.hovered()
            || response.has_focus()
            || response.dragged()
            || dragging_value.is_some();
        let bar = Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
        let track_color = if palette.dark {
            Color32::from_white_alpha(50)
        } else {
            Color32::from_black_alpha(40)
        };
        ui.painter().rect_filled(bar, 2.0, track_color);
        let filled = Rect::from_min_max(
            bar.min,
            pos2(bar.left() + bar.width() * shown.clamp(0.0, 1.0), bar.max.y),
        );
        let fill = if active { palette.accent } else { palette.text };
        ui.painter().rect_filled(filled, 2.0, fill);
        if active {
            ui.painter()
                .circle_filled(pos2(filled.right(), bar.center().y), 6.0, palette.text);
        }
    }
    event
}

/// A tab-like chip row: returns the newly selected index, if any.
pub fn chips<T: PartialEq + Copy>(
    ui: &mut Ui,
    palette: &Palette,
    options: &[(T, &str)],
    current: T,
) -> Option<T> {
    let mut selected = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for (value, label) in options {
            if theme::soft_button(ui, palette, None, label, *value == current).clicked() {
                selected = Some(*value);
            }
        }
    });
    selected
}

/// A text input with the native clipboard actions and a selection-aware menu.
pub fn text_edit(ui: &mut Ui, locale: Locale, edit: egui::TextEdit<'_>) -> egui::Response {
    let mut output = edit.show(ui);
    let response = &output.response;
    let selection_id = response.id.with("edit-menu-selection");
    // egui moves the caret on any mouse-button press and clears a selection
    // when a popup takes focus. Keep the field's previous selection while its
    // edit menu is opening or open.
    let keep_selection = response.context_menu_opened()
        || response.secondary_clicked()
        || (response.contains_pointer()
            && ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Secondary)));
    if keep_selection {
        if let Some(range) = ui.data(|data| data.get_temp::<egui::text::CCursorRange>(selection_id))
        {
            output.state.cursor.set_char_range(Some(range));
            output.state.clone().store(ui.ctx(), response.id);
        }
    } else if let Some(range) = output.state.cursor.char_range() {
        ui.data_mut(|data| data.insert_temp(selection_id, range));
    }
    let selected = output
        .state
        .cursor
        .char_range()
        .is_some_and(|range| !range.is_empty());
    response.context_menu(|ui| {
        for (label, enabled, command) in [
            (
                gettext(locale, "Cut"),
                selected,
                egui::ViewportCommand::RequestCut,
            ),
            (
                gettext(locale, "Copy"),
                selected,
                egui::ViewportCommand::RequestCopy,
            ),
            (
                gettext(locale, "Paste"),
                true,
                egui::ViewportCommand::RequestPaste,
            ),
        ] {
            if ui
                .add_enabled(enabled, egui::Button::new(label.as_ref()))
                .clicked()
            {
                // Menu clicks take focus. Restore this field before the native
                // integration delivers the clipboard event on the next frame.
                response.request_focus();
                ui.ctx().send_viewport_cmd(command);
                ui.close();
            }
        }
        ui.separator();
        if ui.button(gettext(locale, "Select all").as_ref()).clicked() {
            let end = output.galley.job.text.chars().count();
            output
                .state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(end),
                )));
            output.state.clone().store(ui.ctx(), response.id);
            response.request_focus();
            ui.close();
        }
    });
    output.response.response
}

/// A text field with a leading search icon.
pub fn search_field(
    ui: &mut Ui,
    palette: &Palette,
    locale: Locale,
    id: egui::Id,
    text: &mut String,
    hint: &str,
    width: f32,
) -> egui::Response {
    let height = 34.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let has_focus = ui.memory(|memory| memory.has_focus(id));
    let fill = if has_focus {
        palette.surface_hover
    } else {
        palette.surface
    };
    ui.painter().rect_filled(rect, height / 2.0, fill);
    if has_focus {
        ui.painter().rect_stroke(
            rect,
            height / 2.0,
            Stroke::new(1.5, palette.text.gamma_multiply(0.6)),
            egui::StrokeKind::Inside,
        );
    }
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), Vec2::splat(16.0));
    Icon::Search
        .image(palette.secondary, 16.0)
        .paint_at(ui, icon_rect);
    let field_rect = Rect::from_min_max(
        pos2(rect.left() + 34.0, rect.top() + 1.0),
        pos2(rect.right() - 30.0, rect.bottom() - 1.0),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(field_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    // A right-to-left query is shown in reading order. The caret keeps
    // egui's own idea of where it is: at the end of what was typed.
    let text_color = palette.text;
    let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, _wrap_width: f32| {
        let shown = crate::bidi::display_text(buffer.as_str()).into_owned();
        ui.painter()
            .layout_job(egui::text::LayoutJob::simple_singleline(
                shown,
                theme::regular(14.0),
                text_color,
            ))
    };
    let response = text_edit(
        &mut child,
        locale,
        egui::TextEdit::singleline(text)
            .id(id)
            .hint_text(egui::RichText::new(hint).color(palette.dim))
            .font(theme::regular(14.0))
            .text_color(palette.text)
            .frame(egui::Frame::NONE)
            .desired_width(field_rect.width())
            .vertical_align(Align::Center)
            .layouter(&mut layouter),
    );
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_label(hint));
    if !text.is_empty() {
        let clear_rect = Rect::from_center_size(
            pos2(rect.right() - 17.0, rect.center().y),
            Vec2::splat(24.0),
        );
        let mut clear = ui.new_child(
            UiBuilder::new()
                .max_rect(clear_rect)
                .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
        );
        if theme::icon_button(
            &mut clear,
            Icon::X,
            15.0,
            palette.secondary,
            palette.text,
            &gettext(locale, "Clear"),
        )
        .clicked()
        {
            text.clear();
            ui.memory_mut(|memory| memory.request_focus(id));
        }
    }
    response
}

/// A toggle drawn as a switch.
pub fn switch(ui: &mut Ui, palette: &Palette, label: &str, on: &mut bool) -> egui::Response {
    let size = vec2(40.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool(response.id, *on);
        let fill = egui::lerp(
            egui::Rgba::from(palette.surface_active)..=egui::Rgba::from(palette.accent),
            t,
        );
        ui.painter()
            .rect_filled(rect, rect.height() / 2.0, Color32::from(fill));
        let knob_x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
        ui.painter()
            .circle_filled(pos2(knob_x, rect.center().y), 8.0, Color32::WHITE);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, label)
    });
    theme::focus_ring(ui, &response);
    response
}

/// A labelled row in a settings section.
pub fn setting_row(
    ui: &mut Ui,
    palette: &Palette,
    label: &str,
    description: &str,
    control: impl FnOnce(&mut Ui),
) {
    // The text wraps beside the control. Most controls fit in 260 points;
    // a wider one, such as a row of choices, is measured as it is drawn so
    // a longer description (or a longer translation) wraps before it
    // instead of running underneath.
    // Keyed by the row's place rather than its label: two rows can share a
    // title, and sharing one width would make them take turns.
    let control_id = ui.next_auto_id().with("setting-row-control");
    let reserved = ui
        .data(|data| data.get_temp::<f32>(control_id))
        .map_or(260.0, |width| (width + 16.0).max(260.0));
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            // A frame can arrive before the window has its size (a fullscreen
            // request on Wayland answers a frame late), so never go negative.
            ui.set_width((ui.available_width() - reserved).max(0.0));
            theme::text(ui, label, theme::medium(14.0), palette.text);
            if !description.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(description)
                            .font(theme::regular(12.5))
                            .color(palette.secondary),
                    )
                    .wrap(),
                );
            }
        });
        let width = ui
            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                let start = ui.cursor().right();
                control(ui);
                start - ui.min_rect().left()
            })
            .inner;
        let known = ui.data(|data| data.get_temp::<f32>(control_id));
        if known.is_none_or(|known| (known - width).abs() > 0.5) {
            ui.data_mut(|data| data.insert_temp(control_id, width));
            // Lay the row out again at the width just measured rather than
            // show one frame of overlapping text.
            ui.ctx().request_discard("a settings control changed width");
        }
    });
    ui.add_space(10.0);
}

/// A labelled text field: the caption sits above the box.
pub fn labeled_field(
    ui: &mut Ui,
    palette: &Palette,
    locale: Locale,
    label: &str,
    value: &mut String,
    hint: &str,
    password: bool,
) -> egui::Response {
    let response = ui
        .vertical(|ui| {
            theme::text(ui, label, theme::medium(13.0), palette.text);
            ui.add_space(6.0);
            Frame::new()
                .fill(palette.surface)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    text_edit(
                        ui,
                        locale,
                        egui::TextEdit::singleline(value)
                            .hint_text(egui::RichText::new(hint).color(palette.dim))
                            .font(theme::regular(14.0))
                            .password(password)
                            .frame(egui::Frame::NONE)
                            .desired_width(ui.available_width()),
                    )
                })
                .inner
        })
        .inner;
    ui.ctx()
        .accesskit_node_builder(response.id, |node| node.set_label(label));
    response
}

/// Host, port, username, and password in two columns.
pub fn proxy_manual_form(
    ui: &mut Ui,
    palette: &Palette,
    locale: Locale,
    host: &mut String,
    port: &mut String,
    username: &mut String,
    password: &mut String,
) -> bool {
    let mut changed = false;
    let mut address_changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let half = ((ui.available_width() - 12.0) / 2.0).max(80.0);
        ui.vertical(|ui| {
            ui.set_width(half);
            if labeled_field(
                ui,
                palette,
                locale,
                &gettext(locale, "Host"),
                host,
                "127.0.0.1",
                false,
            )
            .changed()
            {
                changed = true;
                address_changed = true;
            }
        });
        ui.vertical(|ui| {
            ui.set_width(half);
            if labeled_field(
                ui,
                palette,
                locale,
                &gettext(locale, "Port"),
                port,
                "1080",
                false,
            )
            .changed()
            {
                changed = true;
                address_changed = true;
            }
        });
    });
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let half = ((ui.available_width() - 12.0) / 2.0).max(80.0);
        ui.vertical(|ui| {
            ui.set_width(half);
            let label = gettext(locale, "Username");
            if labeled_field(ui, palette, locale, &label, username, &label, false).changed() {
                changed = true;
                address_changed = true;
            }
        });
        ui.vertical(|ui| {
            ui.set_width(half);
            if address_changed {
                password.clear();
            }
            let label = gettext(locale, "Password");
            if labeled_field(ui, palette, locale, &label, password, &label, true).changed() {
                changed = true;
            }
        });
    });
    changed
}

pub fn proxy_scope_note(
    ui: &mut Ui,
    palette: &Palette,
    locale: Locale,
    mode: crate::settings::ProxyMode,
) {
    let note = match mode {
        crate::settings::ProxyMode::Http => gettext(
            locale,
            "Proxy login applies to Web requests. Local playback uses this proxy only without a login.",
        ),
        crate::settings::ProxyMode::Socks => gettext(
            locale,
            "Spotify hostnames are resolved by the proxy. Local playback connects directly.",
        ),
        crate::settings::ProxyMode::Off | crate::settings::ProxyMode::System => return,
    };
    ui.add(
        egui::Label::new(
            egui::RichText::new(note)
                .font(theme::regular(13.0))
                .color(palette.secondary),
        )
        .wrap(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, AppOptions};
    use crate::model::{Action, Page};
    use crate::paths::AppDirs;
    use crate::settings::Settings;

    #[test]
    fn editing_a_proxy_endpoint_clears_its_password_but_password_entry_is_preserved() {
        for label in ["Host", "Port", "Username", "Password"] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            theme::install(&ctx);
            let mut fields = [
                "127.0.0.1".to_string(),
                "8080".into(),
                "dummy-user".into(),
                "dummy-password".into(),
            ];
            let mut frame = |events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            vec2(600.0, 400.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        let [host, port, username, password] = &mut fields;
                        proxy_manual_form(
                            ui,
                            &Palette::dark(),
                            Locale::English,
                            host,
                            port,
                            username,
                            password,
                        );
                    },
                );
                output.textures_delta.clear();
                output
            };
            let tree = frame(vec![]).platform_output.accesskit_update.unwrap();
            let target = tree
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.label() == Some(label)
                        && matches!(
                            node.role(),
                            egui::accesskit::Role::TextInput | egui::accesskit::Role::PasswordInput
                        )
                })
                .expect("labeled proxy field")
                .0;
            frame(vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    target_tree: egui::accesskit::TreeId::ROOT,
                    target_node: target,
                    action: egui::accesskit::Action::Focus,
                    data: None,
                },
            )]);
            frame(vec![egui::Event::Text("x".into())]);
            if label == "Password" {
                assert!(fields[3].contains("dummy-password"));
                assert!(fields[3].contains('x'));
            } else {
                assert!(
                    fields[3].is_empty(),
                    "{label} must clear the previous endpoint's password"
                );
            }
        }
    }

    struct TextMenu {
        ctx: egui::Context,
        text: String,
        multiline: bool,
        response: Option<egui::Response>,
    }

    impl TextMenu {
        fn new(multiline: bool) -> Self {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            theme::install(&ctx);
            let mut menu = Self {
                ctx,
                text: "Björk 音楽".into(),
                multiline,
                response: None,
            };
            menu.frame(vec![]);
            menu
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 400.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    self.response = Some(if self.multiline {
                        text_edit(
                            ui,
                            Locale::English,
                            egui::TextEdit::multiline(&mut self.text).id_source("edit"),
                        )
                    } else {
                        search_field(
                            ui,
                            &Palette::dark(),
                            Locale::English,
                            egui::Id::new("edit"),
                            &mut self.text,
                            "Search",
                            300.0,
                        )
                    });
                },
            );
            output.textures_delta.clear();
            output
        }

        fn select(&mut self, start: usize, end: usize) {
            self.response.as_ref().unwrap().request_focus();
            self.frame(vec![]);
            let response = self.response.as_ref().unwrap();
            let mut state = egui::TextEdit::load_state(&self.ctx, response.id).unwrap();
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(start),
                    egui::text::CCursor::new(end),
                )));
            state.store(&self.ctx, response.id);
            response.request_focus();
            self.frame(vec![]);
        }

        fn open(&mut self) -> egui::accesskit::TreeUpdate {
            let pos = self.response.as_ref().unwrap().rect.center();
            self.frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
            self.frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            self.frame(vec![]).platform_output.accesskit_update.unwrap()
        }

        fn choose(&mut self, label: &str) -> egui::FullOutput {
            let tree = self.open();
            let (target, node) = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .expect(label);
            assert!(!node.is_disabled(), "{label} is disabled");
            self.frame(vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    target_tree: egui::accesskit::TreeId::ROOT,
                    target_node: *target,
                    action: egui::accesskit::Action::Click,
                    data: None,
                },
            )])
        }
    }

    #[test]
    fn text_menu_preserves_selection_and_uses_native_clipboard_events() {
        for multiline in [false, true] {
            let mut menu = TextMenu::new(multiline);
            menu.select(6, 8);
            let output = menu.choose("Copy");
            assert!(
                output.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .contains(&egui::ViewportCommand::RequestCopy)
            );
            // The native integration delivers these events after reading the
            // clipboard. Keep tests independent of the user's real clipboard.
            let output = menu.frame(vec![egui::Event::Copy]);
            assert!(
                output
                    .platform_output
                    .commands
                    .contains(&egui::OutputCommand::CopyText("音楽".into()))
            );
            assert_eq!(menu.text, "Björk 音楽");

            let output = menu.choose("Cut");
            assert!(
                output.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .contains(&egui::ViewportCommand::RequestCut)
            );
            menu.frame(vec![egui::Event::Cut]);
            assert_eq!(menu.text, "Björk ");
            assert!(menu.response.as_ref().unwrap().changed());

            menu.frame(vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }]);
            assert_eq!(menu.text, "Björk 音楽", "menu edits keep keyboard undo");

            menu.select(0, 5);
            let output = menu.choose("Paste");
            assert!(
                output.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .contains(&egui::ViewportCommand::RequestPaste)
            );
            menu.frame(vec![egui::Event::Paste("新しい".into())]);
            assert_eq!(menu.text, "新しい 音楽");
            assert!(menu.response.as_ref().unwrap().changed());

            menu.choose("Select all");
            menu.frame(vec![egui::Event::Text("replacement".into())]);
            assert_eq!(menu.text, "replacement");
            assert!(menu.response.as_ref().unwrap().has_focus());
        }
    }

    #[test]
    fn text_menu_disables_cut_and_copy_without_a_selection() {
        let mut menu = TextMenu::new(false);
        menu.select(2, 2);
        let tree = menu.open();
        for label in ["Cut", "Copy"] {
            let (_, node) = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .unwrap();
            assert!(node.is_disabled(), "{label} needs a selection");
        }
    }

    fn test_app() -> App {
        let root = std::env::temp_dir().join(format!(
            "spotifast-virtual-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        App::new(
            &crate::backend::Waker::default(),
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
        )
    }

    fn run_on(
        ctx: &egui::Context,
        size: Vec2,
        clip: Rect,
        events: Vec<egui::Event>,
        mut f: impl FnMut(&mut Ui),
    ) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), size)),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            ui.set_clip_rect(clip);
            f(ui);
        });
        output.textures_delta.clear();
    }

    fn run(size: Vec2, clip: Rect, events: Vec<egui::Event>, f: impl FnMut(&mut Ui)) {
        run_on(&egui::Context::default(), size, clip, events, f);
    }

    #[test]
    fn virtual_rows_only_build_visible_indices() {
        let mut painted = Vec::new();
        let size = vec2(400.0, 240.0);
        let clip = Rect::from_min_size(pos2(0.0, 0.0), size);
        run(size, clip, Vec::new(), |ui| {
            virtual_rows(ui, 10_000, 40.0, |_, index| painted.push(index));
        });
        assert!(
            painted.len() < 20,
            "a long list must not build every row: {} painted",
            painted.len()
        );
        assert!(!painted.is_empty());
        assert_eq!(painted[0], 0);
        assert!(
            painted.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "visible indices must be consecutive"
        );
    }

    #[test]
    fn virtual_rows_keep_full_height_when_scrolled() {
        let mut painted = Vec::new();
        let mut span = 0.0;
        let size = vec2(400.0, 800.0);
        let clip = Rect::from_min_max(pos2(0.0, 400.0), pos2(400.0, 600.0));
        run(size, clip, Vec::new(), |ui| {
            let start = ui.cursor().top();
            virtual_rows(ui, 10_000, 40.0, |ui, index| {
                painted.push(index);
                ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
            });
            span = ui.cursor().top() - start;
        });
        assert!(
            (span - 10_000.0 * 40.0).abs() < 1.0,
            "scroll height must match the full list, got {span}"
        );
        assert!(
            painted.first().copied().unwrap_or(0) >= 8,
            "rows above the clip must be skipped: {painted:?}"
        );
        assert!(
            painted.last().copied().unwrap_or(0) < 20,
            "rows below the clip must be skipped: {painted:?}"
        );
        assert!(painted.len() < 20);
    }

    #[test]
    fn a_click_on_a_visible_virtual_row_still_fires() {
        let ctx = egui::Context::default();
        let size = vec2(400.0, 240.0);
        let clip = Rect::from_min_size(pos2(0.0, 0.0), size);
        let mut row0 = Rect::NOTHING;
        run_on(&ctx, size, clip, Vec::new(), |ui| {
            virtual_rows(ui, 500, 40.0, |ui, index| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                if index == 0 {
                    row0 = rect;
                }
            });
        });
        let pos = row0.center();
        let mut clicked = None;
        let events = vec![
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
        ];
        run_on(&ctx, size, clip, events, |ui| {
            virtual_rows(ui, 500, 40.0, |ui, index| {
                let (_, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                if response.clicked() {
                    clicked = Some(index);
                }
            });
        });
        assert_eq!(clicked, Some(0));
    }

    #[test]
    fn load_more_asks_when_the_list_end_is_near() {
        let mut app = test_app();
        let size = vec2(400.0, 800.0);
        let clip = Rect::from_min_size(pos2(0.0, 0.0), size);
        run(size, clip, Vec::new(), |ui| {
            virtual_rows(ui, 5, 40.0, |ui, _| {
                ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
            });
            load_more_when_near_end(ui, &mut app, Page::Albums, true);
        });
        assert!(
            app.actions
                .iter()
                .any(|action| matches!(action, Action::LoadMore(Page::Albums))),
            "a short list must request the next page: {:?}",
            app.actions
        );
    }

    #[test]
    fn load_more_waits_until_a_long_list_is_near_the_end() {
        let mut app = test_app();
        let size = vec2(400.0, 200.0);
        let clip = Rect::from_min_size(pos2(0.0, 0.0), size);
        run(size, clip, Vec::new(), |ui| {
            virtual_rows(ui, 10_000, 40.0, |_, _| {});
            load_more_when_near_end(ui, &mut app, Page::Albums, true);
        });
        assert!(
            app.actions.is_empty(),
            "the top of a long list must not page: {:?}",
            app.actions
        );

        run(
            size,
            Rect::from_min_max(pos2(0.0, 399_200.0), pos2(400.0, 400_000.0)),
            Vec::new(),
            |ui| {
                virtual_rows(ui, 10_000, 40.0, |_, _| {});
                load_more_when_near_end(ui, &mut app, Page::Albums, true);
            },
        );
        assert!(
            app.actions
                .iter()
                .any(|action| matches!(action, Action::LoadMore(Page::Albums))),
            "scrolling near the end must page: {:?}",
            app.actions
        );
    }

    #[test]
    fn virtual_rows_keep_the_gap_a_for_loop_would() {
        let size = vec2(400.0, 800.0);
        let clip = Rect::from_min_size(pos2(0.0, 0.0), size);
        let mut loop_span = 0.0;
        let mut virt_span = 0.0;
        run(size, clip, Vec::new(), |ui| {
            let start = ui.cursor().top();
            for _ in 0..8 {
                ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
            }
            loop_span = ui.cursor().top() - start;
        });
        run(size, clip, Vec::new(), |ui| {
            let start = ui.cursor().top();
            let gap = ui.spacing().item_spacing.y;
            virtual_rows(ui, 8, 40.0 + gap, |ui, _| {
                let width = ui.available_width();
                ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
                ui.allocate_space(vec2(width, gap));
            });
            virt_span = ui.cursor().top() - start;
        });
        assert!(
            loop_span > 8.0 * 40.0,
            "a for-loop keeps item spacing: {loop_span}"
        );
        assert!(
            (loop_span - virt_span).abs() < 1.0,
            "playing-next style virtual rows must keep that spacing: loop={loop_span} virtual={virt_span}"
        );
    }

    #[test]
    fn virtual_wrapped_cards_keep_widget_ids_when_the_first_row_changes() {
        use std::collections::HashMap;
        let ctx = egui::Context::default();
        let size = vec2(400.0, 800.0);
        let mut top_ids = HashMap::new();
        run_on(
            &ctx,
            size,
            Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 220.0)),
            Vec::new(),
            |ui| {
                virtual_wrapped_cards(ui, 40, 180.0, |ui, index| {
                    let response = ui.button(format!("Card {index}"));
                    top_ids.insert(index, response.id);
                });
            },
        );
        let mut scrolled_ids = HashMap::new();
        run_on(
            &ctx,
            size,
            Rect::from_min_max(pos2(0.0, 400.0), pos2(400.0, 620.0)),
            Vec::new(),
            |ui| {
                virtual_wrapped_cards(ui, 40, 180.0, |ui, index| {
                    let response = ui.button(format!("Card {index}"));
                    scrolled_ids.insert(index, response.id);
                });
            },
        );
        let shared = top_ids
            .keys()
            .find(|index| scrolled_ids.contains_key(index))
            .copied()
            .expect("a card must remain built after the first visible row changes");
        assert_eq!(
            top_ids[&shared], scrolled_ids[&shared],
            "card {shared} must keep its widget id when earlier rows leave the clip"
        );
    }

    #[test]
    fn virtual_wrapped_cards_only_build_visible_rows() {
        let mut painted = Vec::new();
        let size = vec2(400.0, 220.0);
        let clip = Rect::from_min_size(pos2(0.0, 0.0), size);
        run(size, clip, Vec::new(), |ui| {
            virtual_wrapped_cards(ui, 200, 180.0, |_, index| painted.push(index));
        });
        assert!(
            painted.len() < 40,
            "a long grid must not build every card: {} painted",
            painted.len()
        );
        assert!(!painted.is_empty());
        assert_eq!(painted[0], 0);
    }

    fn song(uri: &str) -> PlayableItem {
        PlayableItem::Track(Track {
            uri: uri.to_string(),
            ..Default::default()
        })
    }

    struct PlayingRow {
        app: App,
        ctx: egui::Context,
        item: PlayableItem,
        context: RowContext,
        compact: bool,
        thin: bool,
        show_playing_overlay: bool,
        window_focused: bool,
        clip: Rect,
        rect: Rect,
        id: egui::Id,
        time: f64,
    }

    impl PlayingRow {
        fn new(compact: bool, thin: bool, palette: Palette) -> Self {
            let mut app = test_app();
            crate::demo::populate(&mut app);
            app.backend.shutdown();
            app.palette = palette;
            let ctx = egui::Context::default();
            theme::install(&ctx);
            theme::apply(&ctx, &palette);
            let item = song(&app.current_track_uri().unwrap());
            Self {
                app,
                ctx,
                item,
                context: RowContext::Uris(std::sync::Arc::from([])),
                compact,
                thin,
                show_playing_overlay: true,
                window_focused: true,
                clip: Rect::EVERYTHING,
                rect: Rect::NOTHING,
                id: egui::Id::NULL,
                time: 0.0,
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.time += 0.125;
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(760.0, 520.0))),
                    time: Some(self.time),
                    predicted_dt: 0.0,
                    focused: self.window_focused,
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.set_clip_rect(ui.clip_rect().intersect(self.clip));
                    let (response, _) = track_row_response(
                        ui,
                        &mut self.app,
                        TrackRow {
                            index: 0,
                            number: Some(1),
                            item: &self.item,
                            context: &self.context,
                            show_cover: !self.thin,
                            show_playing_overlay: self.show_playing_overlay,
                            show_album: false,
                            added_at: None,
                            added_by: None,
                            show_added_by: false,
                            compact: self.compact,
                            thin: self.thin,
                            shift: 0.0,
                            picked: false,
                            picked_songs: &[],
                        },
                    );
                    self.rect = response.rect;
                    self.id = response.id;
                },
            );
            output.textures_delta.clear();
            output
        }

        fn bars(&self, output: &egui::FullOutput) -> Vec<Rect> {
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Rect(shape)
                        if shape.fill == self.app.palette.accent && shape.rect.width() == 2.0 =>
                    {
                        Some(shape.rect)
                    }
                    _ => None,
                })
                .collect()
        }

        fn settled(&mut self) -> egui::FullOutput {
            self.frame(vec![]);
            self.frame(vec![]);
            self.frame(vec![])
        }

        fn assert_idle(&mut self) {
            let output = self.settled();
            assert!(self.bars(&output).is_empty());
            assert!(
                output.viewport_output[&egui::ViewportId::ROOT].repaint_delay
                    > std::time::Duration::from_millis(33)
            );
        }
    }

    #[test]
    fn playing_rows_animate_inside_the_existing_icon_and_follow_pause_resume() {
        for mut palette in [Palette::dark(), Palette::light()] {
            palette.accent = Color32::from_rgb(255, 0, 90);
            for (compact, thin) in [(false, false), (true, false), (false, true)] {
                let mut row = PlayingRow::new(compact, thin, palette);
                let first = row.settled();
                let second = row.frame(vec![]);
                let bars = row.bars(&first);
                assert_eq!(bars.len(), 4);
                assert_ne!(bars, row.bars(&second));
                let center_x = row.rect.left() + if compact { 28.0 } else { 30.0 };
                let icon =
                    Rect::from_center_size(pos2(center_x, row.rect.center().y), Vec2::splat(16.0));
                for _ in 0..40 {
                    let output = row.frame(vec![]);
                    let bars = row.bars(&output);
                    assert_eq!(bars.len(), 4);
                    for bar in bars {
                        assert!(icon.contains_rect(bar), "{bar:?} outside {icon:?}");
                        assert!((3.0..=14.0).contains(&bar.height()));
                        assert_eq!(bar.bottom(), icon.bottom() - 1.0);
                    }
                    assert_eq!(
                        output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
                        std::time::Duration::from_millis(33)
                    );
                }
                let height = row.rect.height();
                row.app.apply(Action::TogglePlay, &row.ctx);
                assert!(!row.app.believed_playing());
                row.assert_idle();
                row.app.apply(Action::TogglePlay, &row.ctx);
                let resumed = row.settled();
                assert_eq!(row.bars(&resumed).len(), 4);
                assert_eq!(row.rect.height(), height);
                assert!(row.app.actions.is_empty());
            }
        }
    }

    #[test]
    fn playing_rows_only_animate_while_the_window_is_focused() {
        for compact in [false, true] {
            let mut row = PlayingRow::new(compact, false, Palette::dark());
            let playing = row.settled();
            let frozen_bars = row.bars(&playing);
            assert_eq!(frozen_bars.len(), 4);
            let mut uninterrupted = PlayingRow::new(compact, false, Palette::dark());
            uninterrupted.settled();

            row.window_focused = false;
            let unfocused = row.frame(vec![egui::Event::WindowFocused(false)]);
            assert_eq!(row.bars(&unfocused), frozen_bars);
            // Progress updates can still redraw an unfocused window. Those
            // frames must keep the same bars without scheduling animation.
            for _ in 0..4 {
                row.time += 60.0;
                let background = row.settled();
                assert_eq!(row.bars(&background), frozen_bars);
                assert!(
                    background.viewport_output[&egui::ViewportId::ROOT].repaint_delay
                        > std::time::Duration::from_millis(33)
                );
            }

            row.window_focused = true;
            let resumed = row.frame(vec![egui::Event::WindowFocused(true)]);
            assert_eq!(row.bars(&resumed), frozen_bars);
            let next = row.frame(vec![]);
            assert_ne!(row.bars(&next), frozen_bars);
            let continued = uninterrupted.frame(vec![]);
            assert_eq!(row.bars(&next), uninterrupted.bars(&continued));
        }
    }

    #[test]
    fn playing_rows_start_frozen_when_the_window_is_unfocused() {
        let mut row = PlayingRow::new(false, false, Palette::dark());
        row.window_focused = false;
        let first = row.settled();
        let bars = row.bars(&first);
        assert_eq!(bars.len(), 4);
        row.time += 60.0;
        let later = row.settled();
        assert_eq!(row.bars(&later), bars);
        assert!(
            later.viewport_output[&egui::ViewportId::ROOT].repaint_delay
                > std::time::Duration::from_millis(33)
        );
        row.window_focused = true;
        let resumed = row.frame(vec![egui::Event::WindowFocused(true)]);
        assert_eq!(row.bars(&resumed), bars);
        let next = row.frame(vec![]);
        assert_ne!(row.bars(&next), bars);
    }

    #[test]
    fn queue_artwork_stays_clear_until_hover_focus_or_pending_playback() {
        let mut row = PlayingRow::new(true, false, Palette::dark());
        row.show_playing_overlay = false;
        row.assert_idle();
        let scrimmed = |output: &egui::FullOutput, alpha| {
            output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::epaint::Shape::Rect(rect)
                    if rect.fill == Color32::from_black_alpha(alpha)
                        && rect.rect.size() == Vec2::splat(36.0))
            })
        };
        assert!(!scrimmed(&row.settled(), 110));

        let hovered = row.frame(vec![egui::Event::PointerMoved(row.rect.center())]);
        assert!(scrimmed(&hovered, 140));
        row.frame(vec![egui::Event::PointerGone]);
        row.ctx.memory_mut(|memory| memory.request_focus(row.id));
        assert!(scrimmed(&row.settled(), 140));
        row.frame(vec![egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(matches!(
            row.app.actions.as_slice(),
            [Action::PlayFromRow { .. }]
        ));
        row.app.actions.clear();
        row.ctx.memory_mut(|memory| memory.surrender_focus(row.id));
        row.app.apply(
            Action::PlayUris {
                uris: vec![row.item.uri().into()],
                index: 0,
            },
            &row.ctx,
        );
        let pending = row.settled();
        assert!(scrimmed(&pending, 140));
        assert!(
            pending
                .shapes
                .iter()
                .any(|shape| matches!(shape.shape, egui::epaint::Shape::Path(_)))
        );

        // Full-width queue rows retain the separate number-column indicator.
        let mut full = PlayingRow::new(false, false, Palette::dark());
        full.show_playing_overlay = false;
        let playing = full.settled();
        assert_eq!(full.bars(&playing).len(), 4);
    }

    #[test]
    fn playing_rows_yield_to_controls_pending_plays_and_clipping() {
        for compact in [false, true] {
            let mut row = PlayingRow::new(compact, false, Palette::dark());
            row.settled();
            row.frame(vec![egui::Event::PointerMoved(row.rect.center())]);
            row.assert_idle();
            row.frame(vec![egui::Event::PointerGone]);
            row.ctx.memory_mut(|memory| memory.request_focus(row.id));
            row.assert_idle();
            row.frame(vec![egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            assert!(
                matches!(row.app.actions.as_slice(), [Action::PlayFromRow { uri, .. }] if uri == row.item.uri())
            );
            row.app.actions.clear();
            row.ctx.memory_mut(|memory| memory.surrender_focus(row.id));
            let playing = row.settled();
            assert_eq!(row.bars(&playing).len(), 4);
            // The row can still be partly visible while its indicator is clipped.
            row.clip = Rect::from_min_max(pos2(100.0, 0.0), pos2(760.0, 520.0));
            row.assert_idle();
            row.clip = Rect::NOTHING;
            row.assert_idle();
            row.clip = Rect::EVERYTHING;
            row.app.apply(
                Action::PlayUris {
                    uris: vec![row.item.uri().into()],
                    index: 0,
                },
                &row.ctx,
            );
            assert!(row.app.play_pending(row.item.uri()));
            let pending = row.settled();
            assert!(row.bars(&pending).is_empty());
            assert!(
                pending
                    .shapes
                    .iter()
                    .any(|shape| matches!(shape.shape, egui::epaint::Shape::Path(_))),
                "pending playback keeps its spinner"
            );
        }
    }

    #[test]
    fn playing_rows_stop_for_other_tracks_queue_occurrences_and_sign_out() {
        let mut row = PlayingRow::new(false, false, Palette::dark());
        let playing = row.settled();
        assert_eq!(row.bars(&playing).len(), 4);
        row.context = RowContext::Queue;
        row.assert_idle();
        row.context = RowContext::Uris(std::sync::Arc::from([]));
        row.app.remote.as_mut().unwrap().state.item = Some(song("spotify:track:another"));
        row.assert_idle();
        row.app.remote.as_mut().unwrap().state.item = Some(row.item.clone());
        let returned = row.settled();
        assert_eq!(row.bars(&returned).len(), 4);
        row.app.local.track = Some(crate::player::LocalTrack {
            uri: row.item.uri().into(),
            ..Default::default()
        });
        row.app.local.playback = crate::player::Playback::Playing;
        row.app.remote = None;
        let local = row.settled();
        assert_eq!(row.bars(&local).len(), 4);
        row.app.local.playback = crate::player::Playback::Paused;
        row.assert_idle();
        row.app.local.playback = crate::player::Playback::Playing;
        row.app.apply(Action::SignOut, &row.ctx);
        // Sign-out completes asynchronously. The signed-out view must stop
        // drawing rows even if it still has the last playback snapshot.
        row.app.auth = crate::backend::AuthStatus::SignedOut;
        for _ in 0..4 {
            let mut output = row
                .ctx
                .run_ui(egui::RawInput::default(), |ui| row.app.frame_ui(ui));
            output.textures_delta.clear();
            assert!(row.bars(&output).is_empty());
        }
    }

    #[test]
    fn track_row_selection_preserves_transparency_and_focus_without_an_outline() {
        for mut palette in [Palette::dark(), Palette::light()] {
            // A vivid custom accent must not tint a selected row either.
            palette.accent = Color32::from_rgb(255, 0, 90);
            for (picked, focused) in [(true, false), (true, true), (false, true)] {
                let mut app = test_app();
                app.backend.shutdown();
                app.palette = palette;
                let ctx = egui::Context::default();
                theme::install(&ctx);
                theme::apply(&ctx, &palette);
                let item = song("spotify:track:selected");
                let context = RowContext::Queue;
                let mut rect = Rect::NOTHING;
                let mut id = egui::Id::NULL;
                let mut draw = || {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                egui::Pos2::ZERO,
                                vec2(760.0, 520.0),
                            )),
                            events: vec![egui::Event::PointerGone],
                            ..Default::default()
                        },
                        |ui| {
                            let (response, _) = track_row_response(
                                ui,
                                &mut app,
                                TrackRow {
                                    index: 0,
                                    number: Some(1),
                                    item: &item,
                                    context: &context,
                                    show_cover: false,
                                    show_playing_overlay: true,
                                    show_album: false,
                                    added_at: None,
                                    added_by: None,
                                    show_added_by: false,
                                    compact: false,
                                    thin: false,
                                    shift: 0.0,
                                    picked,
                                    picked_songs: &[],
                                },
                            );
                            rect = response.rect;
                            id = response.id;
                            if focused {
                                response.request_focus();
                            }
                        },
                    );
                    output.textures_delta.clear();
                    output
                };
                draw();
                let output = draw();
                let fills: Vec<_> = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Rect(shape) if shape.rect == rect => Some(shape.fill),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    fills,
                    [if picked {
                        palette
                            .secondary
                            .gamma_multiply(if focused { 0.30 } else { 0.20 })
                    } else {
                        palette
                            .surface_hover
                            .gamma_multiply(if palette.dark { 0.7 } else { 1.0 })
                    }]
                );
                if focused {
                    assert!(ctx.memory(|memory| memory.has_focus(id)));
                }
                assert!(
                    output.shapes.iter().all(|shape| match &shape.shape {
                        egui::epaint::Shape::Rect(shape)
                            if shape.rect == rect || shape.rect == rect.expand(2.0) =>
                        {
                            shape.stroke == Stroke::NONE
                                && (shape.rect != rect
                                    || shape.corner_radius == CornerRadius::same(6))
                        }
                        _ => true,
                    }),
                    "row selection and focus must not add a border or change the row shape"
                );
                assert_eq!(rect.height(), theme::ROW_HEIGHT);
                assert!(app.actions.is_empty());
            }
        }
    }

    #[test]
    fn dragging_a_picked_row_carries_the_whole_selection() {
        let first = song("spotify:track:first");
        let second = song("spotify:track:second");
        let dragged = dragged_items(&second, true, &[first.clone(), second.clone()]);
        assert_eq!(
            dragged.iter().map(PlayableItem::uri).collect::<Vec<_>>(),
            [first.uri(), second.uri()],
            "the sidebar receives every selected row in table order"
        );
    }

    #[test]
    fn dragging_multiple_songs_labels_the_first_and_the_rest() {
        let track = DragTrack {
            title: "Fitraten (VDJ Fly LoFi)".into(),
            image: None,
            items: vec![
                PlayableItem::Track(Track {
                    name: "Kora Panna".into(),
                    ..Default::default()
                }),
                PlayableItem::Track(Track {
                    name: "Fitraten (VDJ Fly LoFi)".into(),
                    ..Default::default()
                }),
            ],
            from: None,
        };
        assert_eq!(drag_label(Locale::English, &track), "Kora Panna + 1 more");
    }

    #[test]
    fn menu_submenu_registers_focus_and_opens_from_keyboard() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let palette = Palette::dark();
        let mut child_rendered = false;
        let mut target_id = None;

        // Frame 1: secondary click to open context menu
        let input1 = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 400.0))),
            events: vec![egui::Event::PointerButton {
                pos: pos2(50.0, 50.0),
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut output1 = ctx.run_ui(input1, |ui| {
            let res = ui.allocate_response(vec2(100.0, 100.0), Sense::click());
            egui::Popup::menu(&res).open(true).show(|ui| {
                let prev_id = ui.next_auto_id();
                target_id = Some(prev_id);
                menu_submenu(ui, &palette, None, "Submenu", |ui| {
                    child_rendered = true;
                    ui.label("Child content");
                });
            });
        });
        output1.textures_delta.clear();

        let target_id = target_id.expect("context menu must be opened and render submenu button");
        assert!(!child_rendered, "submenu child must be closed initially");

        // Request keyboard focus onto the submenu button
        ctx.memory_mut(|mem| mem.request_focus(target_id));

        // Frame 2: trigger activation via Space key
        let input2 = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 400.0))),
            events: vec![egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut output2 = ctx.run_ui(input2, |ui| {
            let res = ui.allocate_response(vec2(100.0, 100.0), Sense::click());
            egui::Popup::menu(&res).open(true).show(|ui| {
                menu_submenu(ui, &palette, None, "Submenu", |ui| {
                    child_rendered = true;
                    ui.label("Child content");
                });
            });
        });
        output2.textures_delta.clear();

        assert!(
            child_rendered,
            "menu_submenu must open child contents when Space is pressed while focused"
        );

        // Frame 3: toggle closed with Space, then reopen with Enter
        let input3 = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 400.0))),
            events: vec![egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        child_rendered = false;
        let mut output3 = ctx.run_ui(input3, |ui| {
            let res = ui.allocate_response(vec2(100.0, 100.0), Sense::click());
            egui::Popup::menu(&res).open(true).show(|ui| {
                menu_submenu(ui, &palette, None, "Submenu", |ui| {
                    child_rendered = true;
                    ui.label("Child content");
                });
            });
        });
        output3.textures_delta.clear();
        assert!(
            !child_rendered,
            "menu_submenu must close child contents when Space is pressed again"
        );

        // Frame 4: reopen with Enter
        let input4 = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 400.0))),
            events: vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        child_rendered = false;
        let mut output4 = ctx.run_ui(input4, |ui| {
            let res = ui.allocate_response(vec2(100.0, 100.0), Sense::click());
            egui::Popup::menu(&res).open(true).show(|ui| {
                menu_submenu(ui, &palette, None, "Submenu", |ui| {
                    child_rendered = true;
                    ui.label("Child content");
                });
            });
        });
        output4.textures_delta.clear();
        assert!(
            child_rendered,
            "menu_submenu must open child contents when Enter is pressed while focused"
        );
    }

    /// A settings row whose control is wider than the usual 260 points,
    /// like the audio quality choices, wraps its description before the
    /// control instead of running underneath it.
    #[test]
    fn a_setting_row_wraps_its_description_before_a_wide_control() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::theme::install(&ctx);
        let palette = crate::theme::Palette::dark();
        // Long enough to wrap, as a translation of a short English line can be.
        let description = "Höhere Bitraten verbrauchen mehr Daten und Cache-Speicher, \
            besonders unterwegs, und brauchen länger, bis die Wiedergabe beginnt, wenn die \
            Verbindung langsam ist.";
        let frame = || {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 400.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    super::setting_row(ui, &palette, "Audioqualität", description, |ui| {
                        let _ = ui.button("Sehr hoch · 320 kbps");
                        let _ = ui.button("Hoch · 160 kbps");
                        let _ = ui.button("Normal · 96 kbps");
                        ui.add_space(300.0);
                    });
                },
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        };
        frame();
        let tree = frame();
        let bounds = |label: &str| {
            tree.nodes
                .iter()
                .find(|(_, node)| {
                    [node.label(), node.value()]
                        .into_iter()
                        .flatten()
                        .any(|text| text.starts_with(label))
                })
                .and_then(|(_, node)| node.bounds())
                .unwrap_or_else(|| panic!("{label} is drawn"))
        };
        let text = bounds("Höhere Bitraten");
        let control = bounds("Normal · 96 kbps");
        assert!(
            text.x1 <= control.x0,
            "the description ends at {} but the controls start at {}",
            text.x1,
            control.x0
        );
    }
}
