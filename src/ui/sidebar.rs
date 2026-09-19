//! The left panel: navigation and Your Library.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Rect, Sense, Vec2, pos2, vec2};

use crate::api::models::pick_image;
use crate::app::App;
use crate::i18n::gettext;
use crate::model::{Action, Dialog, DragEntry, DragTrack, Loadable, Page};
use crate::settings::{LIKED_SONGS_KEY, LibraryShelf as Filter, LibrarySort};
use crate::theme::{self, Icon, Palette};

const DEFAULT_ROW_HEIGHT: f32 = 60.0;
const COMPACT_ROW_HEIGHT: f32 = 32.0;

struct Entry {
    image: Option<String>,
    name: String,
    subtitle: String,
    page: Page,
    uri: String,
    round: bool,
    liked: bool,
    /// The account's own playlist: the one it may rename and delete.
    owned: bool,
    /// A playlist the account may drop songs on.
    editable: bool,
    playlist_index: Option<usize>,
    /// A folder row: its rootlist id, whether it is rolled up, and how
    /// many playlists it holds.
    folder: Option<(String, bool, usize)>,
    /// How deep inside folders the row sits, for the indent.
    depth: u8,
    added_at: Option<i64>,
}

impl Entry {
    fn ordering_key(&self) -> &str {
        if self.liked {
            LIKED_SONGS_KEY
        } else {
            &self.uri
        }
    }
}

/// The context a library row plays, matching the cover play button and the
/// right-click menu. Liked Songs has no URI of its own and plays the
/// account's collection instead.
fn entry_play_uri(app: &App, entry: &Entry) -> Option<String> {
    if entry.liked {
        app.user
            .as_ref()
            .map(|user| format!("spotify:user:{}:collection", user.id))
    } else if entry.uri.is_empty() {
        None
    } else {
        Some(entry.uri.clone())
    }
}

fn liked_entry(app: &App) -> Entry {
    Entry {
        image: None,
        name: gettext(app.locale, "Liked Songs").into_owned(),
        subtitle: match app.library.liked.total {
            Some(total) => app.locale.liked_song_count(total),
            None => gettext(app.locale, "Playlist").into_owned(),
        },
        page: Page::LikedSongs,
        uri: String::new(),
        round: false,
        liked: true,
        owned: false,
        editable: false,
        playlist_index: None,
        folder: None,
        depth: 0,
        added_at: None,
    }
}

fn selected_sort(app: &App, shelf: Filter) -> LibrarySort {
    if let Some(sort) = app.settings.library_sort.get(&shelf).copied()
        && sort.supports(shelf)
    {
        return sort;
    }
    if shelf != Filter::Playlists {
        LibrarySort::Library
    } else if !app.settings.sidebar_order.is_empty() {
        LibrarySort::Local
    } else if app
        .rootlist
        .iter()
        .any(|row| matches!(row, crate::player::RootlistEntry::FolderStart { .. }))
    {
        LibrarySort::Spotify
    } else {
        LibrarySort::RecentlyPlayed
    }
}

fn sort_menu(app: &mut App, ui: &mut egui::Ui, shelf: Filter, selected: LibrarySort) {
    let locale = app.locale;
    let labels = [
        (LibrarySort::Library, gettext(locale, "Library order")),
        (
            LibrarySort::RecentlyPlayed,
            gettext(locale, "Recently played"),
        ),
        (LibrarySort::Name, gettext(locale, "Name")),
        (
            LibrarySort::RecentlyAdded,
            gettext(locale, "Recently added"),
        ),
        (LibrarySort::Local, gettext(locale, "Local custom order")),
        (
            LibrarySort::Spotify,
            gettext(locale, "Spotify custom order"),
        ),
    ];
    let label = &labels
        .iter()
        .find(|(sort, _)| *sort == selected)
        .expect("sort label")
        .1;
    ui.add_space(4.0);
    let response = ui.add(
        egui::Button::image_and_text(
            Icon::ChevronDown.image(app.palette.text, 15.0),
            egui::RichText::new(label.as_ref()).font(theme::medium(13.0)),
        )
        .wrap()
        .fill(app.palette.surface)
        .corner_radius(12)
        .min_size(vec2(0.0, 28.0)),
    );
    egui::Popup::menu(&response)
        .frame(super::widgets::menu_frame(&app.palette))
        .show(|ui| {
            let width = labels
                .iter()
                .map(|(_, label)| {
                    ui.painter()
                        .layout_no_wrap(label.to_string(), theme::regular(13.5), app.palette.text)
                        .size()
                        .x
                })
                .fold(140.0_f32, f32::max)
                + 52.0;
            ui.set_width(width.min(ui.ctx().content_rect().width() - 24.0));
            for (sort, label) in &labels {
                if !sort.supports(shelf)
                    || (*sort == LibrarySort::Library && shelf == Filter::Playlists)
                    || (*sort == LibrarySort::Local && app.settings.sidebar_order.is_empty())
                {
                    continue;
                }
                if super::widgets::menu_item(
                    ui,
                    &app.palette,
                    (*sort == selected).then_some(Icon::Check),
                    label,
                ) {
                    app.actions
                        .push(Action::SetLibrarySort { shelf, sort: *sort });
                }
            }
        });
}

fn saved_time(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|text| text.parse::<jiff::Timestamp>().ok())
        .map(|time| time.as_millisecond())
}

fn order_entries(app: &App, shelf: Filter, sort: LibrarySort, entries: &mut [Entry]) {
    match sort {
        LibrarySort::Name => {
            entries.sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.uri.clone()))
        }
        LibrarySort::RecentlyPlayed => entries.sort_by_key(|entry| {
            app.recent_contexts
                .iter()
                .position(|held| {
                    if entry.liked {
                        app.user_id()
                            .is_some_and(|id| held == &format!("spotify:user:{id}:collection"))
                    } else {
                        held == &entry.uri
                    }
                })
                .unwrap_or(usize::MAX)
        }),
        LibrarySort::RecentlyAdded => entries
            .sort_by_key(|entry| (entry.added_at.is_none(), std::cmp::Reverse(entry.added_at))),
        LibrarySort::Local => entries.sort_by_key(|entry| {
            match app
                .settings
                .sidebar_order
                .iter()
                .position(|held| held == entry.ordering_key())
            {
                Some(rank) => (1, rank),
                None => (0, entry.playlist_index.unwrap_or(0)),
            }
        }),
        LibrarySort::Spotify if !entries.iter().any(|entry| entry.folder.is_some()) => {
            entries.sort_by_key(|entry| (entry.liked, app.rootlist.iter().position(|row| matches!(row, crate::player::RootlistEntry::Playlist(uri) if uri == &entry.uri)).unwrap_or(usize::MAX)));
        }
        LibrarySort::Spotify => entries.sort_by_key(|entry| entry.liked),
        LibrarySort::Library => {}
    }
    let pins = app.settings.library_pins();
    entries.sort_by_key(|entry| {
        pins.iter()
            .position(|held| held == entry.ordering_key())
            .unwrap_or(usize::MAX)
    });
    if shelf == Filter::Playlists {
        for entry in entries {
            if app.settings.pinned_contexts.contains(&entry.uri) {
                entry.depth = 0;
            }
        }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    // The traffic lights float over the top-left of the sidebar now, so the
    // first nav row has to start below them.
    let top = 12 + theme::titlebar_inset(ui.ctx()) as i8;
    let panel = egui::Panel::left("sidebar")
        .resizable(true)
        .default_size(app.settings.sidebar_width)
        .size_range(210.0..=440.0)
        .show_separator_line(false)
        .frame(Frame::new().fill(palette.panel).inner_margin(Margin {
            left: 12,
            right: 8,
            top,
            bottom: 8,
        }));
    let response = panel.show(ui, |ui| {
        art_panel(app, ui);
        contents(app, ui);
    });
    let width = response.response.rect.width();
    if (width - app.settings.sidebar_width).abs() > 1.0 {
        app.settings.sidebar_width = width;
        app.actions.push(Action::SettingsChanged);
    }
}

/// Expanded album art at the bottom of the sidebar (#92).
fn art_panel(app: &mut App, ui: &mut egui::Ui) {
    if !app.settings.art_expanded {
        return;
    }
    let Some(now) = app.now_playing() else {
        return;
    };
    let Some(url) = now.art_url.clone().or_else(|| now.art_small.clone()) else {
        return;
    };
    let palette = app.palette;
    let art = app.backend.art();
    let side = ui
        .available_width()
        .min(ui.available_height() * 0.45)
        .max(80.0);
    egui::Panel::bottom("sidebar-art")
        .exact_size(side)
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new())
        .show(ui, |ui| {
            let rect = Rect::from_min_size(
                ui.max_rect().left_top(),
                Vec2::splat(side.min(ui.available_width())),
            );
            super::widgets::paint_cover(
                ui,
                &palette,
                Some(&url),
                rect,
                8.0,
                Icon::Music,
                Some(art),
            );
            let art = ui
                .interact(rect, egui::Id::new("sidebar-art"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let chevron_rect = Rect::from_center_size(
                pos2(rect.right() - 16.0, rect.top() + 16.0),
                Vec2::splat(20.0),
            );
            let over_chevron = ui.rect_contains_pointer(chevron_rect);
            if art.hovered() || over_chevron {
                let chevron = ui
                    .interact(
                        chevron_rect,
                        egui::Id::new("sidebar-art-collapse"),
                        Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.painter().circle_filled(
                    chevron_rect.center(),
                    10.0,
                    palette.panel.gamma_multiply(0.9),
                );
                Icon::ChevronDown.image(palette.text, 14.0).paint_at(
                    ui,
                    Rect::from_center_size(chevron_rect.center(), Vec2::splat(14.0)),
                );
                if chevron.clicked() {
                    app.settings.art_expanded = false;
                    app.actions.push(Action::SettingsChanged);
                }
            }
            if art.clicked() && !over_chevron {
                if let Some(id) = &now.album_id {
                    app.actions.push(Action::Open(Page::Album(id.clone())));
                } else if let Some(id) = &now.show_id {
                    app.actions.push(Action::Open(Page::Show(id.clone())));
                }
            }
        });
}

/// Playlist rows in account order, including collapsible folders (#95).
fn folder_rows(app: &App, user_id: &str, entries: &mut Vec<Entry>) {
    use crate::player::RootlistEntry;
    let Some(playlists) = app.library.playlists.get() else {
        return;
    };
    let by_uri: std::collections::HashMap<&str, (usize, &crate::api::models::Playlist)> = playlists
        .iter()
        .enumerate()
        .map(|(index, playlist)| (playlist.uri.as_str(), (index, playlist)))
        .collect();
    let mut depth = 0u8;
    // Rows inside a rolled-up folder stay off the list; the stack knows
    // how deep the rolled-up one sits.
    let mut hidden_from: Option<u8> = None;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for row in &app.rootlist {
        match row {
            RootlistEntry::FolderStart { id, name } => {
                let collapsed = app.collapsed_folders.contains(id);
                if hidden_from.is_none() {
                    let count = folder_playlists(&app.rootlist, id);
                    entries.push(Entry {
                        image: None,
                        name: if name.is_empty() {
                            "Folder".to_string()
                        } else {
                            name.clone()
                        },
                        subtitle: match count {
                            1 => "Folder • 1 playlist".to_string(),
                            n => format!("Folder • {n} playlists"),
                        },
                        page: Page::Home,
                        uri: String::new(),
                        round: false,
                        liked: false,
                        owned: false,
                        editable: false,
                        playlist_index: None,
                        folder: Some((id.clone(), collapsed, count)),
                        depth,
                        added_at: None,
                    });
                    if collapsed {
                        hidden_from = Some(depth);
                    }
                }
                depth += 1;
            }
            RootlistEntry::FolderEnd => {
                depth = depth.saturating_sub(1);
                if hidden_from == Some(depth) {
                    hidden_from = None;
                }
            }
            RootlistEntry::Playlist(uri) => {
                let Some((index, playlist)) = by_uri.get(uri.as_str()) else {
                    continue;
                };
                if !seen.insert(uri.as_str()) {
                    continue;
                }
                if hidden_from.is_some() && !app.settings.pinned_contexts.contains(uri) {
                    continue;
                }
                entries.push(playlist_entry(
                    playlist,
                    *index,
                    user_id,
                    app.can_edit_playlist(playlist),
                    depth,
                ));
            }
        }
    }
    // Playlists the rootlist has not met yet, the newly followed, wait at
    // the end rather than vanish.
    for (index, playlist) in playlists.iter().enumerate() {
        if !seen.contains(playlist.uri.as_str()) {
            entries.push(playlist_entry(
                playlist,
                index,
                user_id,
                app.can_edit_playlist(playlist),
                0,
            ));
        }
    }
}

/// How many playlists a folder holds, nested ones included.
fn folder_playlists(rootlist: &[crate::player::RootlistEntry], id: &str) -> usize {
    use crate::player::RootlistEntry;
    let mut counting = false;
    let mut depth = 0usize;
    let mut count = 0;
    for row in rootlist {
        match row {
            RootlistEntry::FolderStart { id: this, .. } => {
                if counting {
                    depth += 1;
                } else if this == id {
                    counting = true;
                    depth = 1;
                }
            }
            RootlistEntry::FolderEnd if counting => {
                depth -= 1;
                if depth == 0 {
                    return count;
                }
            }
            RootlistEntry::Playlist(_) if counting => count += 1,
            _ => {}
        }
    }
    count
}

fn playlist_entry(
    playlist: &crate::api::models::Playlist,
    index: usize,
    user_id: &str,
    editable: bool,
    depth: u8,
) -> Entry {
    Entry {
        image: pick_image(&playlist.images, 64).map(str::to_string),
        name: playlist.name.clone(),
        subtitle: format!("Playlist • {}", playlist.owner_name()),
        page: Page::Playlist(playlist.id.clone()),
        uri: playlist.uri.clone(),
        round: false,
        liked: false,
        owned: playlist.owned_by(user_id),
        editable,
        playlist_index: Some(index),
        folder: None,
        depth,
        added_at: None,
    }
}

fn nav_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    icon: Icon,
    label: &str,
    active: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let color = if active || response.hovered() {
            palette.text
        } else {
            palette.secondary
        };
        let icon_rect =
            Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), Vec2::splat(22.0));
        icon.image(color, 22.0).paint_at(ui, icon_rect);
        ui.painter().text(
            pos2(rect.left() + 46.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            theme::bold(15.0),
            color,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), active, label)
    });
    theme::focus_ring(ui, &response);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn contents(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let page = app.page().clone();
    let locale = app.locale;
    ui.add_space(4.0);
    if nav_row(
        ui,
        &palette,
        Icon::House,
        &gettext(locale, "Home"),
        page == Page::Home,
    )
    .clicked()
    {
        app.actions.push(Action::Open(Page::Home));
    }
    if nav_row(
        ui,
        &palette,
        Icon::Search,
        &gettext(locale, "Search"),
        page == Page::Search,
    )
    .clicked()
    {
        app.actions.push(Action::FocusSearch);
    }
    ui.add_space(10.0);
    ui.painter().hline(
        ui.max_rect().x_range().shrink(4.0),
        ui.cursor().top(),
        egui::Stroke::new(1.0, palette.outline),
    );
    ui.add_space(10.0);

    let filter_id = egui::Id::new("sidebar-filter");
    let mut filter = ui
        .data(|data| data.get_temp::<Filter>(filter_id))
        .unwrap_or_default();
    let show_search_id = egui::Id::new("sidebar-show-search");
    let mut show_search = ui
        .data(|data| data.get_temp::<bool>(show_search_id))
        .unwrap_or(false);

    let mut focus_search = false;

    ui.horizontal(|ui| {
        ui.add_space(6.0);
        theme::icon(ui, Icon::Library, 22.0, palette.secondary);
        ui.add_space(2.0);
        theme::text(
            ui,
            gettext(locale, "Library"),
            theme::bold(15.0),
            palette.text,
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if theme::icon_button(
                ui,
                Icon::PanelLeft,
                16.0,
                palette.secondary,
                palette.text,
                super::keys::platform_shortcut(
                    &gettext(locale, "Hide sidebar (Ctrl+B)"),
                    &gettext(locale, "Hide sidebar (Cmd+B)"),
                ),
            )
            .clicked()
            {
                app.actions.push(Action::ToggleSidebar);
            }
            // One item never deserved a menu: the plus creates directly.
            if theme::icon_button(
                ui,
                Icon::Plus,
                16.0,
                palette.secondary,
                palette.text,
                &gettext(locale, "Create a playlist"),
            )
            .clicked()
            {
                app.actions.push(Action::ShowDialog(Dialog::CreatePlaylist {
                    name: String::new(),
                    public: false,
                    add_uris: Vec::new(),
                }));
            }
            if theme::icon_button(
                ui,
                Icon::Search,
                16.0,
                palette.secondary,
                palette.text,
                &gettext(locale, "Search Your Library"),
            )
            .clicked()
            {
                show_search = !show_search;
                if show_search {
                    focus_search = true;
                } else {
                    app.library.filter.clear();
                }
            }
        });
    });
    ui.add_space(6.0);

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for (value, label) in [
            (Filter::Playlists, gettext(locale, "Playlists")),
            (Filter::Albums, gettext(locale, "Albums")),
            (Filter::Artists, gettext(locale, "Artists")),
            (Filter::Podcasts, gettext(locale, "Podcasts")),
        ] {
            if theme::soft_button(ui, &palette, None, &label, filter == value).clicked() {
                filter = value;
            }
        }
    });
    let sort = selected_sort(app, filter);
    sort_menu(app, ui, filter, sort);
    ui.data_mut(|data| {
        data.insert_temp(filter_id, filter);
        data.insert_temp(show_search_id, show_search);
    });
    if show_search {
        ui.add_space(4.0);
        let response = super::widgets::search_field(
            ui,
            &palette,
            egui::Id::new("sidebar-search"),
            &mut app.library.filter,
            &gettext(locale, "Search in Your Library"),
            ui.available_width() - 4.0,
        );
        if focus_search {
            response.request_focus();
        }
    }
    ui.add_space(6.0);

    // Make sure the selected shelf is loading.
    match filter {
        Filter::Playlists => {}
        Filter::Albums => {
            if !app.library.albums.loading
                && app.library.albums.error.is_none()
                && (!app.library.albums.loaded_once
                    || (sort != LibrarySort::Library && app.library.albums.can_load_more()))
            {
                app.actions.push(Action::LoadMore(Page::Albums));
            }
        }
        Filter::Artists => {
            if !app.library.artists.loading
                && app.library.artists.error.is_none()
                && (!app.library.artists.loaded_once
                    || (sort != LibrarySort::Library && app.library.artists.can_load_more()))
            {
                app.actions.push(Action::LoadMore(Page::Artists));
            }
        }
        Filter::Podcasts => {
            if !app.library.shows.loading
                && app.library.shows.error.is_none()
                && (!app.library.shows.loaded_once
                    || (sort != LibrarySort::Library && app.library.shows.can_load_more()))
            {
                app.actions.push(Action::LoadMore(Page::Podcasts));
            }
        }
    }

    let needle = app.library.filter.trim().to_lowercase();
    let user_id = app.user_id().unwrap_or("").to_string();
    let mut entries: Vec<Entry> = Vec::new();
    let mut loading = false;
    let mut error: Option<String> = None;
    let mut more_page: Option<Page> = None;
    match filter {
        Filter::Playlists => {
            let liked = liked_entry(app);
            if needle.is_empty() || liked.name.to_lowercase().contains(&needle) {
                entries.push(liked);
            }
            let show_folders = sort == LibrarySort::Spotify && needle.is_empty();
            if show_folders {
                folder_rows(app, &user_id, &mut entries);
            }
            match &app.library.playlists {
                Loadable::Loaded(_) if show_folders => {}
                Loadable::Loaded(playlists) => {
                    for (index, playlist) in playlists.iter().enumerate() {
                        if !needle.is_empty() && !playlist.name.to_lowercase().contains(&needle) {
                            continue;
                        }
                        let owned = playlist.owned_by(&user_id);
                        entries.push(Entry {
                            image: pick_image(&playlist.images, 64).map(str::to_string),
                            name: playlist.name.clone(),
                            subtitle: format!("Playlist • {}", playlist.owner_name()),
                            page: Page::Playlist(playlist.id.clone()),
                            uri: playlist.uri.clone(),
                            round: false,
                            liked: false,
                            owned,
                            editable: app.can_edit_playlist(playlist),
                            playlist_index: Some(index),
                            folder: None,
                            depth: 0,
                            added_at: None,
                        });
                    }
                }
                Loadable::Loading | Loadable::NotLoaded => loading = true,
                Loadable::Failed(message) => error = Some(message.clone()),
            }
        }
        Filter::Albums => {
            for saved in &app.library.albums.items {
                let album = &saved.album;
                if !needle.is_empty()
                    && !album.name.to_lowercase().contains(&needle)
                    && !album
                        .artists
                        .iter()
                        .any(|a| a.name.to_lowercase().contains(&needle))
                {
                    continue;
                }
                entries.push(Entry {
                    image: pick_image(&album.images, 64).map(str::to_string),
                    name: album.name.clone(),
                    subtitle: format!(
                        "{} • {}",
                        app.album_kind_label(album),
                        crate::api::models::join_names(
                            album.artists.iter().map(|a| a.name.as_str())
                        )
                    ),
                    page: Page::Album(album.id.clone()),
                    uri: album.uri.clone(),
                    round: false,
                    liked: false,
                    owned: false,
                    editable: false,
                    playlist_index: None,
                    folder: None,
                    depth: 0,
                    added_at: saved_time(saved.added_at.as_deref()),
                });
            }
            loading = app.library.albums.loading && app.library.albums.items.is_empty();
            error = app.library.albums.error.clone();
            if app.library.albums.error.is_none() && app.library.albums.can_load_more() {
                more_page = Some(Page::Albums);
            }
        }
        Filter::Artists => {
            for artist in &app.library.artists.items {
                if !needle.is_empty() && !artist.name.to_lowercase().contains(&needle) {
                    continue;
                }
                entries.push(Entry {
                    image: pick_image(&artist.images, 64).map(str::to_string),
                    name: artist.name.clone(),
                    subtitle: gettext(locale, "Artist").into_owned(),
                    page: Page::Artist(artist.id.clone()),
                    uri: artist.uri.clone(),
                    round: true,
                    liked: false,
                    owned: false,
                    editable: false,
                    playlist_index: None,
                    folder: None,
                    depth: 0,
                    added_at: None,
                });
            }
            loading = app.library.artists.loading && app.library.artists.items.is_empty();
            error = app.library.artists.error.clone();
            if app.library.artists.error.is_none() && app.library.artists.can_load_more() {
                more_page = Some(Page::Artists);
            }
        }
        Filter::Podcasts => {
            for saved in &app.library.shows.items {
                let show = &saved.show;
                if !needle.is_empty() && !show.name.to_lowercase().contains(&needle) {
                    continue;
                }
                entries.push(Entry {
                    image: pick_image(&show.images, 64).map(str::to_string),
                    name: show.name.clone(),
                    subtitle: format!("Podcast • {}", show.publisher),
                    page: Page::Show(show.id.clone()),
                    uri: show.uri.clone(),
                    round: false,
                    liked: false,
                    owned: false,
                    editable: false,
                    playlist_index: None,
                    folder: None,
                    depth: 0,
                    added_at: saved_time(saved.added_at.as_deref()),
                });
            }
            loading = app.library.shows.loading && app.library.shows.items.is_empty();
            error = app.library.shows.error.clone();
            if app.library.shows.error.is_none() && app.library.shows.can_load_more() {
                more_page = Some(Page::Podcasts);
            }
        }
    }

    order_entries(app, filter, sort, &mut entries);
    let custom_order = filter == Filter::Playlists && sort == LibrarySort::Local;
    let pins = app.settings.library_pins();
    let pinned_rows = entries
        .iter()
        .take_while(|entry| pins.iter().any(|key| key == entry.ordering_key()))
        .count();
    let playing_context = app.playing_context_uri();
    let context_playing = app.believed_playing();
    let current_page = app.page().clone();

    crate::autoscroll::show(
        ui,
        egui::ScrollArea::vertical()
            .id_salt("sidebar-list")
            .auto_shrink([false, false]),
        egui::Vec2b::new(false, true),
        |ui| {
            if egui::DragAndDrop::has_payload_of_type::<DragTrack>(ui.ctx())
                || egui::DragAndDrop::has_payload_of_type::<DragEntry>(ui.ctx())
            {
                super::widgets::scroll_during_drag(ui);
            }
            if loading {
                super::widgets::loading_row(ui, &palette, app.locale);
            }
            if let Some(error) = &error {
                super::widgets::error_row(ui, app, error, None);
            }
            if entries.is_empty() && !loading && error.is_none() {
                ui.add_space(12.0);
                theme::subtle(
                    ui,
                    &palette,
                    if needle.is_empty() {
                        "Nothing here yet."
                    } else {
                        "No matches."
                    },
                );
            }
            let compact = app.settings.sidebar_compact;
            let row_height = if compact {
                COMPACT_ROW_HEIGHT
            } else {
                DEFAULT_ROW_HEIGHT
            };
            // Calculate drop positions from fixed row height because rows shift
            // before drawing.
            let list_top = ui.cursor().top();
            let pointer = ui.ctx().pointer_latest_pos().filter(|pos| {
                ui.clip_rect().contains(*pos) && ui.rect_contains_pointer(ui.clip_rect())
            });
            // Tracks may drop on Liked Songs or playlists that take songs
            // from this account.
            let dragging_song = egui::DragAndDrop::has_payload_of_type::<DragTrack>(ui.ctx());
            let drop_target = dragging_song
                .then_some(pointer)
                .flatten()
                .map(|pos| ((pos.y - list_top) / row_height).floor())
                .filter(|row| *row >= 0.0 && *row < entries.len() as f32)
                .map(|row| row as usize)
                .filter(|row| entries[*row].liked || entries[*row].editable);
            // Sidebar entries, including Liked Songs, drop between rows.
            let reordering = egui::DragAndDrop::has_payload_of_type::<DragEntry>(ui.ctx());
            let reorder_slot = reordering.then_some(pointer).flatten().map(|pos| {
                (((pos.y - list_top) / row_height).round().max(0.0) as usize).min(entries.len())
            });
            let art = app.backend.art().clone();
            super::widgets::virtual_rows(ui, entries.len(), row_height, |ui, index| {
                let entry = &entries[index];
                if !app.settings.sidebar_compact
                    && let Some(image) = &entry.image
                {
                    // Prepare the enlarged preview before this row is opened.
                    app.softened_covers.texture(ui.ctx(), &art, image);
                }
                let droppable = entry.liked || entry.editable;
                let drop_hover = drop_target == Some(index);
                let active = entry.folder.is_none() && entry.page == current_page;
                // Liked Songs has no URI of its own here; Spotify plays it
                // as the account's collection context.
                let playing = context_playing
                    && if entry.liked {
                        playing_context
                            .as_deref()
                            .is_some_and(|context| context.ends_with(":collection"))
                    } else {
                        !entry.uri.is_empty()
                            && playing_context.as_deref() == Some(entry.uri.as_str())
                    };
                let pinned = pins.iter().any(|key| key == entry.ordering_key());
                let (_, rect) = ui.allocate_space(vec2(ui.available_width(), row_height));
                let id = ui.id().with((
                    "library-row",
                    &entry.uri,
                    entry.liked,
                    entry.folder.as_ref().map(|(id, _, _)| id),
                ));
                let response = ui.interact(rect, id, Sense::click_and_drag());
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Button,
                        ui.is_enabled(),
                        active,
                        if let Some((_, collapsed, _)) = &entry.folder {
                            format!(
                                "{}, folder, {}",
                                entry.name,
                                if *collapsed { "collapsed" } else { "expanded" }
                            )
                        } else {
                            entry.name.clone()
                        },
                    )
                });
                if response.hovered()
                    && let Some(image) = &entry.image
                {
                    app.actions.push(Action::PrepareTint(image.clone()));
                }
                // Start reordering after the drag threshold.
                if !entry.ordering_key().is_empty()
                    && response.drag_started_by(egui::PointerButton::Primary)
                {
                    egui::DragAndDrop::set_payload(
                        ui.ctx(),
                        DragEntry {
                            uri: entry.ordering_key().to_string(),
                            title: entry.name.clone(),
                            image: entry.image.clone(),
                        },
                    );
                }
                // Animate rows around the current track or entry drop target.
                let shift = ui.ctx().animate_value_with_time(
                    ui.id().with(("drop-shift", index)),
                    if let Some(slot) = reorder_slot {
                        if index < slot { -4.0 } else { 4.0 }
                    } else {
                        match drop_target {
                            Some(target) if index < target => -4.0,
                            Some(target) if index > target => 4.0,
                            _ => 0.0,
                        }
                    },
                    0.12,
                );
                let rect = rect.translate(vec2(0.0, shift));
                // Set when the cover play button takes a click, so a double
                // click on it does not also play from the row.
                let mut cover_took_click = false;
                if ui.is_rect_visible(rect) {
                    if active {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(6), palette.surface);
                    } else if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            palette.surface_hover.gamma_multiply(0.6),
                        );
                    }
                    if drop_hover {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            palette.accent.gamma_multiply(0.18),
                        );
                        ui.painter().rect_stroke(
                            rect,
                            CornerRadius::same(6),
                            egui::Stroke::new(1.5, palette.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let name_color = if playing {
                        palette.accent
                    } else {
                        palette.text
                    };
                    let indent = f32::from(entry.depth) * 14.0;
                    if let Some((_, collapsed, _)) = &entry.folder {
                        let chevron = if *collapsed {
                            Icon::ChevronRight
                        } else {
                            Icon::ChevronDown
                        };
                        let left = rect.left() + 8.0 + indent;
                        chevron.image(palette.secondary, 16.0).paint_at(
                            ui,
                            Rect::from_center_size(
                                pos2(left + 8.0, rect.center().y),
                                Vec2::splat(16.0),
                            ),
                        );
                        Icon::Library.image(palette.secondary, 20.0).paint_at(
                            ui,
                            Rect::from_center_size(
                                pos2(left + 30.0, rect.center().y),
                                Vec2::splat(20.0),
                            ),
                        );
                        let text_left = left + 46.0;
                        let text_right = rect.right() - 8.0;
                        let painter = ui.painter().with_clip_rect(Rect::from_min_max(
                            pos2(text_left, rect.top()),
                            pos2(text_right, rect.bottom()),
                        ));
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y - if compact { 0.0 } else { 9.0 },
                            &entry.name,
                            theme::medium(if compact { 13.5 } else { 14.0 }),
                            name_color,
                        );
                        if !compact {
                            crate::bidi::paint_line(
                                &painter,
                                text_left,
                                text_right,
                                rect.center().y + 10.0,
                                &entry.subtitle,
                                theme::regular(12.5),
                                palette.secondary,
                            );
                        }
                    } else if compact {
                        let text_left = rect.left() + 8.0 + indent;
                        let text_right = rect.right() - if playing || pinned { 28.0 } else { 8.0 };
                        let painter = ui.painter().with_clip_rect(Rect::from_min_max(
                            pos2(text_left, rect.top()),
                            pos2(text_right, rect.bottom()),
                        ));
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y,
                            &entry.name,
                            theme::medium(13.5),
                            name_color,
                        );
                    } else {
                        let cover_rect = Rect::from_center_size(
                            pos2(rect.left() + 8.0 + indent + 22.0, rect.center().y),
                            Vec2::splat(44.0),
                        );
                        if entry.liked {
                            liked_cover(ui, cover_rect, 6.0);
                        } else {
                            super::widgets::paint_cover(
                                ui,
                                &palette,
                                entry.image.as_deref(),
                                cover_rect,
                                if entry.round { 22.0 } else { 6.0 },
                                if entry.round { Icon::User } else { Icon::Music },
                                Some(app.backend.art()),
                            );
                        }
                        let text_left = cover_rect.right() + 12.0;
                        let text_right = rect.right() - if playing || pinned { 28.0 } else { 8.0 };
                        let painter = ui.painter().with_clip_rect(Rect::from_min_max(
                            pos2(text_left, rect.top()),
                            pos2(text_right, rect.bottom()),
                        ));
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y - 9.0,
                            &entry.name,
                            theme::medium(14.0),
                            name_color,
                        );
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y + 10.0,
                            &entry.subtitle,
                            theme::regular(12.5),
                            palette.secondary,
                        );
                        // Hovering the art offers to play right from here.
                        let can_play = !entry.uri.is_empty() || entry.liked;
                        let play_response = can_play.then(|| {
                            ui.interact(
                                cover_rect,
                                ui.id().with(("sidebar-play", index)),
                                Sense::click(),
                            )
                        });
                        let play_hover = play_response.as_ref().is_some_and(|play| play.hovered());
                        if play_hover || (response.hovered() && can_play) {
                            ui.painter().rect_filled(
                                cover_rect,
                                CornerRadius::same(if entry.round { 22 } else { 6 }),
                                egui::Color32::from_black_alpha(120),
                            );
                            Icon::PlayFilled
                                .image(
                                    if play_hover {
                                        palette.accent
                                    } else {
                                        egui::Color32::WHITE
                                    },
                                    18.0,
                                )
                                .paint_at(
                                    ui,
                                    Rect::from_center_size(
                                        cover_rect.center()
                                            + theme::play_glyph_offset(Icon::PlayFilled, 18.0),
                                        Vec2::splat(18.0),
                                    ),
                                );
                            if let Some(play) = &play_response {
                                play.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
                            }
                        }
                        if let Some(play) = &play_response
                            && play.clicked()
                        {
                            cover_took_click = true;
                            // The first click of a double click plays; the
                            // second must not play again.
                            if !play.double_clicked()
                                && let Some(uri) = entry_play_uri(app, entry)
                            {
                                app.actions.push(Action::PlayContext {
                                    uri,
                                    offset_uri: None,
                                    offset_index: None,
                                });
                            }
                        }
                    }
                    if playing {
                        let icon_rect = Rect::from_center_size(
                            pos2(rect.right() - 16.0, rect.center().y),
                            Vec2::splat(16.0),
                        );
                        Icon::Volume2
                            .image(palette.accent, 16.0)
                            .paint_at(ui, icon_rect);
                    } else if pinned {
                        let icon_rect = Rect::from_center_size(
                            pos2(rect.right() - 16.0, rect.center().y),
                            Vec2::splat(13.0),
                        );
                        Icon::Pin
                            .image(palette.secondary, 13.0)
                            .paint_at(ui, icon_rect);
                    }
                    // Rows that cannot take the song step back a little.
                    if dragging_song && !droppable {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            palette.panel.gamma_multiply(0.5),
                        );
                    }
                }
                if dragging_song
                    && droppable
                    && let Some(track) = response.dnd_release_payload::<DragTrack>()
                {
                    if entry.liked {
                        // Dropping on Liked Songs saves every dragged song;
                        // songs already saved stay saved.
                        app.actions.push(Action::SetSavedMany {
                            uris: track
                                .items
                                .iter()
                                .map(|item| item.uri().to_string())
                                .collect(),
                            saved: true,
                        });
                    } else if let Page::Playlist(id) = &entry.page {
                        app.actions.push(Action::AddToPlaylist {
                            playlist_id: id.clone(),
                            playlist_name: entry.name.clone(),
                            items: track.items.clone(),
                        });
                    }
                }
                theme::focus_ring(ui, &response);
                if response.clicked() {
                    if let Some((folder_id, collapsed, _)) = &entry.folder {
                        if *collapsed {
                            app.collapsed_folders.retain(|held| held != folder_id);
                        } else {
                            app.collapsed_folders.push(folder_id.clone());
                        }
                        app.session_dirty = true;
                    } else {
                        app.actions.push(Action::Open(entry.page.clone()));
                    }
                }
                // A double click plays the row's context. Folders only
                // collapse, and the cover button handled its own click.
                if response.double_clicked()
                    && entry.folder.is_none()
                    && !cover_took_click
                    && let Some(uri) = entry_play_uri(app, entry)
                {
                    app.actions.push(Action::PlayContext {
                        uri,
                        offset_uri: None,
                        offset_index: None,
                    });
                }
                if !entry.uri.is_empty() {
                    let owned_playlist = entry
                        .owned
                        .then_some(entry.playlist_index)
                        .flatten()
                        .and_then(|index| {
                            app.library
                                .playlists
                                .get()
                                .and_then(|list| list.get(index))
                                .cloned()
                        });
                    egui::Popup::context_menu(&response)
                        .frame(super::widgets::menu_frame(&palette))
                        .show(|ui| {
                            super::widgets::context_menu_items(
                                ui,
                                app,
                                &entry.uri,
                                &entry.name,
                                owned_playlist.as_ref(),
                            );
                            pin_menu(app, ui, entry.ordering_key());
                            if custom_order
                                && super::widgets::menu_item(
                                    ui,
                                    &palette,
                                    Some(Icon::Clock),
                                    "Sort by recently played",
                                )
                            {
                                app.actions.push(Action::SetLibrarySort {
                                    shelf: filter,
                                    sort: LibrarySort::RecentlyPlayed,
                                });
                            }
                        });
                } else if entry.liked {
                    egui::Popup::context_menu(&response)
                        .frame(super::widgets::menu_frame(&palette))
                        .show(|ui| {
                            if super::widgets::menu_item(ui, &palette, Some(Icon::Play), "Play")
                                && let Some(user) = &app.user
                            {
                                app.actions.push(Action::PlayContext {
                                    uri: format!("spotify:user:{}:collection", user.id),
                                    offset_uri: None,
                                    offset_index: None,
                                });
                            }
                            pin_menu(app, ui, entry.ordering_key());
                        });
                }
                crate::autoscroll::row(ui, &response);
                response.on_hover_cursor(egui::CursorIcon::PointingHand);
            });
            if let Some(slot) = reorder_slot {
                // A line in the gap the rows opened, so the eye lands
                // where the row will.
                let y = list_top + slot as f32 * row_height;
                ui.painter().hline(
                    ui.max_rect().x_range().shrink(6.0),
                    y,
                    egui::Stroke::new(2.0, palette.accent),
                );
                if ui.input(|input| input.pointer.button_released(egui::PointerButton::Primary))
                    && let Some(drag) = egui::DragAndDrop::take_payload::<DragEntry>(ui.ctx())
                {
                    if filter == Filter::Playlists {
                        drop_playlist_row(app, &entries, pinned_rows, slot, &drag.uri);
                    } else {
                        drop_row(app, &entries, pinned_rows, slot, &drag.uri);
                    }
                }
            }
            if let Some(page) = more_page {
                super::widgets::load_more_when_near_end(ui, app, page, true);
            }
        },
    );
}

fn pin_menu(app: &mut App, ui: &mut egui::Ui, key: &str) {
    let mut pins = app.settings.library_pins();
    let pinned = pins.iter().any(|held| held == key);
    if super::widgets::menu_item(
        ui,
        &app.palette,
        Some(if pinned { Icon::PinOff } else { Icon::Pin }),
        if pinned { "Unpin" } else { "Pin to top" },
    ) {
        if pinned {
            pins.retain(|held| held != key);
        } else {
            pins.push(key.to_string());
        }
        app.actions.push(Action::ArrangeLibrary {
            pinned: pins,
            playlist_order: None,
        });
    }
}

/// Drops within the pin block arrange pins. Below it, a drop unpins the
/// moved row and saves the full playlist arrangement at the chosen gap.
fn drop_playlist_row(app: &mut App, entries: &[Entry], pinned_rows: usize, slot: usize, key: &str) {
    if key != LIKED_SONGS_KEY
        && !app
            .library
            .playlists
            .get()
            .is_some_and(|playlists| playlists.iter().any(|playlist| playlist.uri == key))
    {
        return;
    }
    if slot < pinned_rows {
        drop_row(app, entries, pinned_rows, slot, key);
        return;
    }
    let mut pins = app.settings.library_pins();
    pins.retain(|held| held != key);
    let mut order = full_playlist_order(app);
    let anchor = entries
        .iter()
        .skip(slot)
        .find_map(|entry| {
            if let Some((id, _, _)) = &entry.folder {
                // A drop before a collapsed folder precedes its first child
                // when switching to the flat local arrangement.
                let start = app.rootlist.iter().position(|row| matches!(row, crate::player::RootlistEntry::FolderStart { id: found, .. } if found == id))?;
                app.rootlist[start + 1..].iter().find_map(|row| match row {
                    crate::player::RootlistEntry::Playlist(held) if held != key && order.contains(held) => Some(held.as_str()),
                    _ => None,
                })
            } else {
                let held = entry.ordering_key();
                (!held.is_empty() && held != key).then_some(held)
            }
        })
        .map(str::to_string);
    order.retain(|held| held != key);
    let at = anchor
        .and_then(|anchor| order.iter().position(|held| *held == anchor))
        .unwrap_or(order.len());
    order.insert(at, key.to_string());
    app.actions.push(Action::ArrangeLibrary {
        pinned: pins,
        playlist_order: Some(order),
    });
}

/// Every unpinned loaded playlist in the selected shelf order, including
/// rows hidden by a search or collapsed folder. A drag snapshots this
/// whole arrangement before applying its new local position.
fn full_playlist_order(app: &App) -> Vec<String> {
    let Some(playlists) = app.library.playlists.get() else {
        return Vec::new();
    };
    let mut entries: Vec<_> = playlists
        .iter()
        .enumerate()
        .map(|(index, playlist)| {
            playlist_entry(playlist, index, app.user_id().unwrap_or(""), false, 0)
        })
        .collect();
    entries.push(liked_entry(app));
    order_entries(
        app,
        Filter::Playlists,
        selected_sort(app, Filter::Playlists),
        &mut entries,
    );
    let pins = app.settings.library_pins();
    entries
        .iter()
        .map(|entry| entry.ordering_key().to_string())
        .filter(|key| !pins.contains(key))
        .collect()
}

/// Reorders the pin block without disturbing pins on another shelf.
fn drop_row(app: &mut App, entries: &[Entry], pinned_rows: usize, slot: usize, key: &str) {
    let mut pins = app.settings.library_pins();
    pins.retain(|held| held != key);
    if slot <= pinned_rows {
        let anchor = entries[..pinned_rows]
            .iter()
            .skip(slot)
            .map(Entry::ordering_key)
            .find(|held| *held != key);
        let at = anchor
            .and_then(|anchor| pins.iter().position(|held| held == anchor))
            .unwrap_or(pins.len());
        pins.insert(at, key.to_string());
    }
    app.actions.push(Action::ArrangeLibrary {
        pinned: pins,
        playlist_order: None,
    });
}

/// The purple-to-blue Liked Songs tile.
pub fn liked_cover(ui: &egui::Ui, rect: Rect, radius: f32) {
    let texture_id = egui::Id::new("liked-cover-gradient");
    let texture = ui
        .data(|data| data.get_temp::<egui::TextureHandle>(texture_id))
        .unwrap_or_else(|| {
            let size = 64;
            let lerp = |a: u8, b: u8, t: f32| (a as f32 + (b as f32 - a as f32) * t) as u8;
            let top_left = [0x45, 0x0a, 0xf5];
            let top_right = [0x6a, 0x3a, 0xe8];
            let bottom_left = [0x8e, 0x9f, 0xe5];
            let bottom_right = [0xc4, 0xef, 0xd9];
            let pixels = (0..size)
                .flat_map(|y| {
                    let y = y as f32 / (size - 1) as f32;
                    (0..size).map(move |x| {
                        let x = x as f32 / (size - 1) as f32;
                        egui::Color32::from_rgb(
                            lerp(
                                lerp(top_left[0], top_right[0], x),
                                lerp(bottom_left[0], bottom_right[0], x),
                                y,
                            ),
                            lerp(
                                lerp(top_left[1], top_right[1], x),
                                lerp(bottom_left[1], bottom_right[1], x),
                                y,
                            ),
                            lerp(
                                lerp(top_left[2], top_right[2], x),
                                lerp(bottom_left[2], bottom_right[2], x),
                                y,
                            ),
                        )
                    })
                })
                .collect();
            let texture = ui.ctx().load_texture(
                "liked-cover-gradient",
                egui::ColorImage::new([size, size], pixels),
                egui::TextureOptions::LINEAR,
            );
            ui.data_mut(|data| data.insert_temp(texture_id, texture.clone()));
            texture
        });
    egui::Image::new(&texture)
        .corner_radius(CornerRadius::same(radius.min(127.0) as u8))
        .paint_at(ui, rect);
    let size = rect.width() * 0.45;
    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(size));
    Icon::HeartFilled
        .image(egui::Color32::WHITE, size)
        .paint_at(ui, icon_rect);
}

#[cfg(all(test, feature = "demo"))]
mod ordering_tests {
    use super::*;
    use crate::api::models::Playlist;
    use crate::settings::Settings;

    fn app(name: &str) -> App {
        let root =
            std::env::temp_dir().join(format!("fastpotify-order-{name}-{}", std::process::id()));
        let mut app = App::new(
            &crate::backend::Waker::default(),
            crate::paths::AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            Settings::default(),
            crate::app::AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
            },
        );
        crate::demo::populate(&mut app);
        app.library.playlists = Loadable::Loaded(
            [
                ("a", "Zebra"),
                ("b", "Alpha"),
                ("c", "alpha"),
                ("d", "Beta"),
            ]
            .into_iter()
            .map(|(id, name)| Playlist {
                id: id.into(),
                name: name.into(),
                uri: format!("spotify:playlist:{id}"),
                ..Default::default()
            })
            .collect(),
        );
        app.recent_contexts = vec![uri("d"), uri("a")];
        app
    }

    fn apply_actions(app: &mut App) {
        for action in std::mem::take(&mut app.actions) {
            app.apply(action, &egui::Context::default());
        }
    }

    fn uri(id: &str) -> String {
        format!("spotify:playlist:{id}")
    }

    fn rows(app: &App) -> Vec<Entry> {
        app.library
            .playlists
            .get()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(index, playlist)| playlist_entry(playlist, index, "", false, 0))
            .collect()
    }

    fn ids(entries: &[Entry]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.uri.rsplit(':').next().unwrap())
            .collect()
    }

    #[test]
    fn name_and_recent_sorts_keep_pins_and_do_not_rewrite_library_data() {
        let mut app = app("sorts");
        let mut entries = rows(&app);
        order_entries(
            &app,
            Filter::Playlists,
            LibrarySort::RecentlyPlayed,
            &mut entries,
        );
        assert_eq!(ids(&entries), ["d", "a", "b", "c"]);
        order_entries(&app, Filter::Playlists, LibrarySort::Name, &mut entries);
        assert_eq!(ids(&entries), ["b", "c", "d", "a"]);
        app.settings.pinned_contexts = vec![uri("a")];
        order_entries(&app, Filter::Playlists, LibrarySort::Name, &mut entries);
        assert_eq!(ids(&entries), ["a", "b", "c", "d"]);
        assert_eq!(ids(&rows(&app)), ["a", "b", "c", "d"]);
        app.backend.shutdown();
    }

    #[test]
    fn switching_sort_and_filtered_drag_preserve_the_full_local_arrangement() {
        let mut app = app("saved");
        app.settings.sidebar_order = ["c", "a", "b", "d"].map(uri).to_vec();
        let saved = app.settings.sidebar_order.clone();
        assert_eq!(selected_sort(&app, Filter::Playlists), LibrarySort::Local);
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::Name);
        assert_eq!(full_playlist_order(&app), ["b", "c", "d", "a"].map(uri));
        assert_eq!(app.settings.sidebar_order, saved);
        app.rootlist = vec![crate::player::RootlistEntry::Playlist(uri("d"))];
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::Local);
        assert_eq!(
            full_playlist_order(&app),
            saved,
            "a late rootlist cannot replace the chosen local order"
        );
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::Name);
        let filtered: Vec<_> = rows(&app)
            .into_iter()
            .filter(|row| row.uri == uri("c") || row.uri == uri("a"))
            .rev()
            .collect();
        drop_playlist_row(&mut app, &filtered, 0, 0, &uri("a"));
        apply_actions(&mut app);
        assert_eq!(app.settings.sidebar_order, ["b", "a", "c", "d"].map(uri));
        assert_eq!(selected_sort(&app, Filter::Playlists), LibrarySort::Local);
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::Name);
        drop_playlist_row(&mut app, &filtered, 0, 0, &uri("a"));
        apply_actions(&mut app);
        assert_eq!(app.settings.sidebar_order, ["b", "a", "c", "d"].map(uri));
        assert_eq!(
            selected_sort(&app, Filter::Playlists),
            LibrarySort::Local,
            "recreating a saved arrangement still leaves the automatic sort"
        );
        app.backend.shutdown();
    }

    #[test]
    fn spotify_order_keeps_nested_folders_and_pins_visible_when_collapsed() {
        use crate::player::RootlistEntry::{FolderEnd, FolderStart, Playlist};
        let mut app = app("folders");
        app.rootlist = vec![
            FolderStart {
                id: "outer".into(),
                name: "Outer".into(),
            },
            Playlist(uri("a")),
            FolderStart {
                id: "inner".into(),
                name: "Inner".into(),
            },
            Playlist(uri("b")),
            Playlist(uri("c")),
            FolderEnd,
            FolderEnd,
            Playlist(uri("d")),
            Playlist(uri("b")),
        ];
        app.collapsed_folders = vec!["inner".into()];
        app.settings.pinned_contexts = vec![uri("b")];
        let mut entries = vec![];
        folder_rows(&app, "", &mut entries);
        order_entries(&app, Filter::Playlists, LibrarySort::Spotify, &mut entries);
        assert_eq!(
            entries
                .iter()
                .map(|row| (row.name.as_str(), row.depth))
                .collect::<Vec<_>>(),
            [
                ("Alpha", 0),
                ("Outer", 0),
                ("Zebra", 1),
                ("Inner", 1),
                ("Beta", 0)
            ]
        );
        app.collapsed_folders.clear();
        let mut entries = vec![];
        folder_rows(&app, "", &mut entries);
        order_entries(&app, Filter::Playlists, LibrarySort::Spotify, &mut entries);
        assert_eq!(
            entries
                .iter()
                .map(|row| (row.name.as_str(), row.depth))
                .collect::<Vec<_>>(),
            [
                ("Alpha", 0),
                ("Outer", 0),
                ("Zebra", 1),
                ("Inner", 1),
                ("alpha", 2),
                ("Beta", 0)
            ]
        );
        app.backend.shutdown();
    }

    #[test]
    fn dragging_before_a_collapsed_folder_and_unpinning_keep_the_drop_position() {
        use crate::player::RootlistEntry::{FolderEnd, FolderStart, Playlist};
        let mut app = app("folder-drop");
        app.rootlist = vec![
            FolderStart {
                id: "folder".into(),
                name: "Folder".into(),
            },
            Playlist(uri("b")),
            Playlist(uri("c")),
            FolderEnd,
            Playlist(uri("d")),
            Playlist(uri("a")),
        ];
        app.collapsed_folders = vec!["folder".into()];
        let mut entries = vec![];
        folder_rows(&app, "", &mut entries);
        order_entries(&app, Filter::Playlists, LibrarySort::Spotify, &mut entries);
        drop_playlist_row(&mut app, &entries, 0, 0, &uri("a"));
        apply_actions(&mut app);
        assert_eq!(app.settings.sidebar_order, ["a", "b", "c", "d"].map(uri));
        app.settings.sidebar_order.clear();
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::Name);
        app.settings.pinned_contexts = vec![uri("a")];
        let mut entries = rows(&app);
        order_entries(&app, Filter::Playlists, LibrarySort::Name, &mut entries);
        drop_playlist_row(&mut app, &entries, 1, 2, &uri("a"));
        apply_actions(&mut app);
        assert_eq!(app.settings.library_pins(), [LIKED_SONGS_KEY]);
        assert_eq!(app.settings.sidebar_order, ["b", "a", "c", "d"].map(uri));
        app.backend.shutdown();
    }

    #[test]
    fn added_sort_uses_actual_instants_and_keeps_missing_dates_last() {
        let mut app = app("dates");
        let mut entries = rows(&app);
        for (row, value) in entries.iter_mut().zip([
            Some("2026-09-09T09:00:00Z"),
            Some("2026-09-09T10:00:00+02:00"),
            None,
            Some("unknown"),
        ]) {
            row.added_at = saved_time(value);
        }
        entries.swap(0, 1);
        order_entries(
            &app,
            Filter::Albums,
            LibrarySort::RecentlyAdded,
            &mut entries,
        );
        assert_eq!(ids(&entries), ["a", "b", "c", "d"]);
        assert!(!LibrarySort::RecentlyAdded.supports(Filter::Playlists));
        assert!(!LibrarySort::RecentlyAdded.supports(Filter::Artists));
        assert!(LibrarySort::RecentlyAdded.supports(Filter::Podcasts));
        app.backend.shutdown();
    }

    #[test]
    fn preferences_round_trip_and_new_playlists_still_precede_saved_order() {
        let mut app = app("migration");
        app.settings = serde_json::from_str(
            r#"{"sidebar_order":["spotify:playlist:c","spotify:playlist:a","spotify:playlist:b"]}"#,
        )
        .unwrap();
        assert_eq!(selected_sort(&app, Filter::Playlists), LibrarySort::Local);
        assert_eq!(selected_sort(&app, Filter::Albums), LibrarySort::Library);
        assert_eq!(full_playlist_order(&app), ["d", "c", "a", "b"].map(uri));
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::Spotify);
        app.settings
            .library_sort
            .insert(Filter::Albums, LibrarySort::RecentlyAdded);
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&app.settings).unwrap()).unwrap();
        assert_eq!(restored, app.settings);
        app.backend.shutdown();
    }

    #[test]
    fn liked_songs_moves_among_pins_and_retains_its_local_position() {
        let mut app = app("liked-position");
        app.settings.pinned_contexts = [uri("d"), uri("a")].to_vec();
        let ordered = |app: &App| {
            let mut entries = rows(app);
            entries.push(liked_entry(app));
            order_entries(
                app,
                Filter::Playlists,
                selected_sort(app, Filter::Playlists),
                &mut entries,
            );
            entries
        };
        let entries = ordered(&app);
        assert_eq!(entries[0].ordering_key(), LIKED_SONGS_KEY);
        drop_playlist_row(&mut app, &entries, 3, 2, LIKED_SONGS_KEY);
        assert_eq!(
            app.settings.library_pins()[0],
            LIKED_SONGS_KEY,
            "the view only emits an action"
        );
        apply_actions(&mut app);
        assert_eq!(
            app.settings.library_pins(),
            [uri("d"), LIKED_SONGS_KEY.into(), uri("a")]
        );
        assert!(app.settings.sidebar_order.is_empty());
        let entries = ordered(&app);
        drop_playlist_row(&mut app, &entries, 3, 4, LIKED_SONGS_KEY);
        apply_actions(&mut app);
        assert!(!app.settings.liked_songs_pinned);
        let saved = [uri("b"), LIKED_SONGS_KEY.into(), uri("c")];
        assert_eq!(full_playlist_order(&app), saved);
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::Name);
        assert_eq!(
            full_playlist_order(&app),
            [uri("b"), uri("c"), LIKED_SONGS_KEY.into()]
        );
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::Local);
        app.rootlist = vec![crate::player::RootlistEntry::Playlist(uri("c"))];
        app.user.as_mut().unwrap().id = "another-account".into();
        app.settings =
            serde_json::from_str(&serde_json::to_string(&app.settings).unwrap()).unwrap();
        assert_eq!(
            full_playlist_order(&app),
            saved,
            "restart and account/rootlist changes preserve the local placement"
        );
        let filtered: Vec<_> = ordered(&app)
            .into_iter()
            .filter(|entry| entry.uri == uri("c") || entry.uri == uri("b"))
            .collect();
        drop_playlist_row(&mut app, &filtered, 0, 0, &uri("c"));
        apply_actions(&mut app);
        assert_eq!(
            full_playlist_order(&app),
            [uri("c"), uri("b"), LIKED_SONGS_KEY.into()],
            "a hidden Liked Songs keeps its place in the full arrangement"
        );
        assert!(
            liked_entry(&app).uri.is_empty(),
            "the ordering key is never a Spotify URI"
        );
        app.backend.shutdown();
    }

    #[test]
    fn unpinned_liked_songs_follows_recent_play_and_spotify_order_without_entering_folders() {
        use crate::player::RootlistEntry::{FolderEnd, FolderStart, Playlist};
        let mut app = app("liked-sorts");
        app.settings.liked_songs_pinned = false;
        app.recent_contexts = vec![
            uri("a"),
            format!("spotify:user:{}:collection", app.user_id().unwrap()),
            uri("c"),
        ];
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::RecentlyPlayed);
        assert_eq!(
            full_playlist_order(&app),
            [
                uri("a"),
                LIKED_SONGS_KEY.into(),
                uri("c"),
                uri("b"),
                uri("d")
            ]
        );
        app.rootlist = vec![
            FolderStart {
                id: "folder".into(),
                name: "Folder".into(),
            },
            Playlist(uri("c")),
            FolderEnd,
            Playlist(uri("a")),
        ];
        app.collapsed_folders = vec!["folder".into()];
        app.settings
            .library_sort
            .insert(Filter::Playlists, LibrarySort::Spotify);
        let mut entries = vec![liked_entry(&app)];
        folder_rows(&app, "", &mut entries);
        order_entries(&app, Filter::Playlists, LibrarySort::Spotify, &mut entries);
        assert!(entries.last().unwrap().liked);
        assert_eq!(entries.last().unwrap().depth, 0);
        drop_playlist_row(&mut app, &entries, 0, 0, LIKED_SONGS_KEY);
        apply_actions(&mut app);
        assert_eq!(
            full_playlist_order(&app),
            [
                LIKED_SONGS_KEY.into(),
                uri("c"),
                uri("a"),
                uri("b"),
                uri("d")
            ]
        );
        let mut entries = rows(&app);
        entries.push(liked_entry(&app));
        let end = entries.len();
        drop_playlist_row(&mut app, &entries, 0, end, LIKED_SONGS_KEY);
        apply_actions(&mut app);
        assert_eq!(full_playlist_order(&app).last().unwrap(), LIKED_SONGS_KEY);
        app.backend.shutdown();
    }
}
