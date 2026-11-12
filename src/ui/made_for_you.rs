//! The Made for you page: every playlist Spotify made for the account, the
//! Made for you mixes and Your top mixes, one sidebar click from anywhere.

use crate::app::App;
use crate::i18n::gettext;
use crate::model::{Loadable, Page};
use crate::theme::{self, Icon};

use super::home::playlist_cards;
use super::widgets;

pub fn page(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    // Only a page that never asked: a failure keeps its Retry.
    if matches!(app.home.made_for_you, Loadable::NotLoaded) {
        app.ensure_loaded(Page::MadeForYou);
    }
    ui.add_space(12.0);
    theme::text(
        ui,
        gettext(locale, "Made for you"),
        theme::bold(30.0),
        palette.text,
    );
    ui.add_space(4.0);
    theme::text(
        ui,
        gettext(
            locale,
            "Your daily mixes, Discover Weekly, Release Radar and the other playlists Spotify made for you.",
        ),
        theme::regular(13.5),
        palette.secondary,
    );
    ui.add_space(18.0);

    let made_for_you = match &app.home.made_for_you {
        Loadable::Loaded(playlists) => playlists.clone(),
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::skeleton_cards(ui, &palette, locale, 7, false);
            return;
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            widgets::error_row(ui, app, &error, Some(Page::MadeForYou));
            return;
        }
    };
    let top_mixes = app.home.top_mixes.get().cloned().unwrap_or_default();
    let top_mixes_error = top_mixes_error(app);
    if made_for_you.is_empty() && top_mixes.is_empty() && top_mixes_error.is_none() {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Sparkles,
            &gettext(locale, "Nothing made for you yet"),
            &gettext(
                locale,
                "Spotify makes mixes once it knows what you like to listen to.",
            ),
        );
        return;
    }
    if !made_for_you.is_empty() {
        widgets::grid(ui, |ui| {
            playlist_cards(app, ui, "made_for_you-page-menu", &made_for_you)
        });
    }
    if let Some(error) = top_mixes_error {
        ui.add_space(24.0);
        theme::text(
            ui,
            gettext(locale, "Your top mixes"),
            theme::bold(22.0),
            palette.text,
        );
        ui.add_space(12.0);
        widgets::error_row(ui, app, &error, Some(Page::MadeForYou));
    } else if !top_mixes.is_empty() {
        ui.add_space(24.0);
        theme::text(
            ui,
            gettext(locale, "Your top mixes"),
            theme::bold(22.0),
            palette.text,
        );
        ui.add_space(12.0);
        widgets::grid(ui, |ui| {
            playlist_cards(app, ui, "top_mixes-page-menu", &top_mixes)
        });
    }
}

/// Why Your top mixes could not be read, while the playback session is up.
/// Without it the shelf has no source at all, so, as on Home, it stays
/// hidden and is read once playback comes up.
pub(crate) fn top_mixes_error(app: &App) -> Option<String> {
    match &app.home.top_mixes {
        Loadable::Failed(error) if app.local_ready => Some(error.clone()),
        _ => None,
    }
}
