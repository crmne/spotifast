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
    if app.home.made_for_you.get().is_none() && !app.home.made_for_you.is_loading() {
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
    if made_for_you.is_empty() && top_mixes.is_empty() {
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
    if !top_mixes.is_empty() {
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
