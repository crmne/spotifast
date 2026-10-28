//! Audio indexed from a folder on this computer.

use std::sync::Arc;

use crate::app::App;
use crate::i18n::{gettext, ngettext};
use crate::model::{Action, Page, RowContext};
use crate::theme::{self, Icon};

use super::collection::{self, Hero, HeroImages, Table, ViewOptions};
use super::widgets;

/// Draw indexed files and emit playback or rescan actions without reading disk.
pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let title = gettext(locale, "Local Files");
    if app.settings.local_files_folder().is_none() {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Music,
            &title,
            &gettext(
                locale,
                "Index audio from a folder on this computer. Local files play on this computer only.",
            ),
        );
        if theme::pill_button(ui, &palette, &gettext(locale, "Choose folder"), false).clicked() {
            app.actions.push(Action::ChooseLocalFilesFolder);
        }
        return;
    }

    let library = app.local_library.get().cloned().unwrap_or_default();
    let revision = app.local_files_revision;
    let items = collection::cached_table_items(app, Page::LocalFiles, 0, revision, 0, || {
        library
            .iter()
            .cloned()
            .map(|item| (item, None, None))
            .collect()
    });
    let count = ngettext(
        locale,
        // Translators: {count} is a number of indexed audio files.
        "{count} local file",
        "{count} local files",
        items.len() as u32,
    )
    .replace("{count}", &items.len().to_string());
    collection::hero(
        app,
        ui,
        Hero {
            images: HeroImages::default(),
            liked: false,
            kind: gettext(locale, "Library"),
            title: &title,
            description: Some(
                gettext(
                    locale,
                    "Supports MP3 and FLAC. Local files play on this computer only.",
                )
                .into_owned(),
            ),
            byline: vec![(count, None)],
            avatar: None,
            round: false,
        },
    );
    if !app.can_play_local_files() {
        ui.add_space(10.0);
        theme::text(
            ui,
            gettext(
                locale,
                "Select This computer in the device picker to play local files.",
            ),
            theme::regular(13.5),
            palette.secondary,
        );
    }

    let filter_id = ui.make_persistent_id("local-files-filter");
    #[cfg(any(test, feature = "demo"))]
    if let Some(filter) = app.demo_local_files_filter.take() {
        ui.data_mut(|data| data.insert_temp(filter_id, filter));
    }
    let mut filter = ui
        .data(|data| data.get_temp::<String>(filter_id))
        .unwrap_or_default();
    let view = collection::prepare_table_view(
        ui,
        app,
        &Page::LocalFiles,
        &items,
        ViewOptions {
            needle: &filter.trim().to_lowercase(),
            sort: app.table_sorts.get(&Page::LocalFiles).copied(),
            plays_shown_rows: true,
        },
        revision,
    );
    let uris: Arc<[String]> = view.view_uris.clone().unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        if ui
            .add_enabled_ui(!uris.is_empty(), |ui| {
                theme::circle_button(
                    ui,
                    Icon::PlayFilled,
                    56.0,
                    palette.accent,
                    palette.accent_hover,
                    palette.on_accent,
                    &gettext(locale, "Play"),
                )
            })
            .inner
            .clicked()
        {
            app.actions.push(Action::PlayUris {
                uris: uris.to_vec(),
                index: 0,
            });
        }
        if theme::soft_button(ui, &palette, None, &gettext(locale, "Change folder"), false)
            .clicked()
        {
            app.actions.push(Action::ChooseLocalFilesFolder);
        }
        if ui
            .add_enabled_ui(!app.local_files_scanning, |ui| {
                theme::soft_button(
                    ui,
                    &palette,
                    Some(Icon::Refresh),
                    &gettext(locale, "Rescan"),
                    false,
                )
            })
            .inner
            .clicked()
        {
            app.actions.push(Action::RescanLocalFiles);
        }
        widgets::search_field(
            ui,
            &palette,
            locale,
            filter_id,
            &mut filter,
            &gettext(locale, "Filter"),
            220.0,
        );
    });
    ui.data_mut(|data| data.insert_temp(filter_id, filter.clone()));
    ui.add_space(14.0);

    let loading = app.local_files_scanning;
    let error = app.local_files_error.clone();
    if items.is_empty() && !loading && error.is_none() {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Music,
            &gettext(locale, "No local files found"),
            &gettext(
                locale,
                "Subfolders are included. Rescan after changing files.",
            ),
        );
        return;
    }
    collection::table(
        app,
        ui,
        Table {
            items: &items,
            row_offset: 0,
            pagination: None,
            context: RowContext::Uris(uris),
            show_album: true,
            show_cover: true,
            show_added: false,
            show_added_by: false,
            show_plays: false,
            page: Page::LocalFiles,
            loading,
            error: error.as_deref(),
            can_load_more: false,
            filter: &filter,
            items_revision: revision,
            plays_shown_rows: true,
        },
    );
}
