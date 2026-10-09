//! A radio page: Spotify's mix for a song, playlist, album, or artist, laid
//! out as a playlist, with the songs it shows being the songs it plays.

use std::sync::Arc;

use crate::api::models::{PlayableItem, Track};
use crate::app::App;
use crate::backend::LocalPlayback;
use crate::i18n::{Locale, gettext};
use crate::model::{Loadable, Page, RowContext};
use crate::theme::Icon;
use crate::util;

use super::collection::{
    Actions, Hero, Table, actions_row, hero, hero_images, remember_table_items, songs_and_duration,
    table, table_items_hit,
};
use super::widgets;

pub fn radio(app: &mut App, ui: &mut egui::Ui, seed: &str) {
    let key = Page::Radio(seed.to_string());
    let Some(station) = util::station_uri(seed) else {
        return;
    };
    if !app.radio_pages.contains_key(seed) {
        app.ensure_loaded(key.clone());
    }
    let Some(page) = app.radio_pages.get(seed) else {
        return;
    };
    let locale = app.locale;
    let name = app
        .radio_name(seed)
        .or_else(|| page.name.clone())
        .unwrap_or_else(|| gettext(locale, "Radio").into_owned());
    let mut images = app.radio_images(seed);
    if images.is_empty() {
        images.clone_from(&page.images);
    }
    let generation = page.generation;
    let refreshing = page.refreshing;
    let state = match &page.songs {
        Loadable::Loaded(songs) => Ok(Some((
            songs.len(),
            songs
                .iter()
                .map(|song| song.duration_ms as u64)
                .sum::<u64>(),
            featuring(locale, songs),
        ))),
        Loadable::Failed(error) => Err(error.clone()),
        Loadable::Loading | Loadable::NotLoaded => Ok(None),
    };
    let seed_page = match util::uri_kind(seed) {
        Some("track") => None,
        _ => Page::from_uri(seed),
    };

    let mut byline = Vec::new();
    if let Some(seed_page) = seed_page {
        byline.push((based_on(locale, seed).into_owned(), Some(seed_page)));
    }
    let mut description = None;
    if let Ok(Some((count, duration, artists))) = &state {
        byline.push((
            songs_and_duration(locale, u32::try_from(*count).unwrap_or(u32::MAX), *duration),
            None,
        ));
        description.clone_from(artists);
    }
    hero(
        app,
        ui,
        Hero {
            images: hero_images(&images, None, false),
            liked: false,
            kind: gettext(locale, "Radio"),
            title: &name,
            description,
            byline,
            round: false,
        },
    );

    match state {
        Ok(Some(_)) => {}
        Ok(None) => {
            ui.add_enabled_ui(false, |ui| {
                actions_row(
                    app,
                    ui,
                    radio_actions(seed, &station, &name, None, false),
                    None,
                )
            });
            if matches!(
                app.local_playback,
                LocalPlayback::Unavailable | LocalPlayback::Failed(_)
            ) {
                widgets::error_row(
                    ui,
                    app,
                    &gettext(
                        locale,
                        "Radio comes from playback on this computer. Turn it on in Settings.",
                    ),
                    None,
                );
            } else {
                widgets::loading_row(ui, &app.palette, app.locale);
            }
            return;
        }
        Err(error) => {
            ui.add_enabled_ui(false, |ui| {
                actions_row(
                    app,
                    ui,
                    radio_actions(seed, &station, &name, None, false),
                    None,
                )
            });
            widgets::error_row(ui, app, &error, Some(key));
            return;
        }
    }
    let names = app.user_names_revision;
    let items = match table_items_hit(app, &key, generation, generation, names) {
        Some(items) => items,
        None => {
            let rows = app
                .radio_pages
                .get(seed)
                .and_then(|page| page.songs.get())
                .into_iter()
                .flatten()
                .map(|track| (PlayableItem::Track(track.clone()), None, None))
                .collect();
            remember_table_items(app, key.clone(), generation, generation, names, rows)
        }
    };
    let uris: Arc<[String]> = items
        .iter()
        .map(|(item, _, _)| item.uri().to_string())
        .collect::<Vec<_>>()
        .into();
    actions_row(
        app,
        ui,
        radio_actions(seed, &station, &name, Some(Arc::clone(&uris)), !refreshing),
        None,
    );
    table(
        app,
        ui,
        Table {
            items: &items,
            row_offset: 0,
            pagination: None,
            context: RowContext::View {
                uris,
                context_uri: station,
                editable_playlist: None,
            },
            show_album: true,
            show_cover: true,
            show_year: app.settings.tracklist_year,
            show_added: false,
            show_added_by: false,
            page: key,
            loading: false,
            error: None,
            can_load_more: false,
            filter: "",
            items_revision: generation,
        },
    );
}

fn radio_actions<'a>(
    seed: &str,
    station: &str,
    name: &'a str,
    view: Option<Arc<[String]>>,
    loaded: bool,
) -> Actions<'a> {
    Actions {
        play_uri: Some(station.to_string()),
        view,
        saved: None,
        saved_icons: (Icon::CirclePlus, Icon::CircleCheck),
        saved_tooltips: Default::default(),
        owned_playlist: None,
        reload: Some((Page::Radio(seed.to_string()), !loaded)),
        name,
        save_radio: Some(seed.to_string()),
    }
}

/// What the radio is based on, linking to the seed's page.
fn based_on(locale: Locale, seed: &str) -> std::borrow::Cow<'static, str> {
    match util::uri_kind(seed) {
        Some("playlist") => gettext(locale, "Based on this playlist"),
        Some("album") => gettext(locale, "Based on this album"),
        Some("artist") => gettext(locale, "Based on this artist"),
        _ => gettext(locale, "Based on this song"),
    }
}

/// "With A, B and C": the first few artists the mix plays, as Spotify's
/// own radio pages describe themselves.
fn featuring(locale: Locale, songs: &[Track]) -> Option<String> {
    let mut names: Vec<&str> = Vec::new();
    for artist in songs.iter().flat_map(|song| &song.artists) {
        if !artist.name.is_empty() && !names.contains(&artist.name.as_str()) {
            names.push(&artist.name);
        }
        if names.len() == 4 {
            break;
        }
    }
    match names.as_slice() {
        [] => None,
        // Translators: {first} is an artist name.
        [first] => Some(gettext(locale, "With {first}").replace("{first}", first)),
        [first, second] => Some(
            // Translators: {first} and {second} are artist names.
            gettext(locale, "With {first} and {second}")
                .replace("{first}", first)
                .replace("{second}", second),
        ),
        [first, second, third] => Some(
            // Translators: {first}, {second} and {third} are artist names.
            gettext(locale, "With {first}, {second} and {third}")
                .replace("{first}", first)
                .replace("{second}", second)
                .replace("{third}", third),
        ),
        [first, second, third, ..] => Some(
            // Translators: {first}, {second} and {third} are artist names; more follow.
            gettext(locale, "With {first}, {second}, {third} and more")
                .replace("{first}", first)
                .replace("{second}", second)
                .replace("{third}", third),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::featuring;
    use crate::api::models::{ArtistRef, Track};
    use crate::i18n::Locale;

    fn by(names: &[&str]) -> Track {
        Track {
            artists: names
                .iter()
                .map(|name| ArtistRef {
                    name: (*name).into(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_radio_names_its_first_artists() {
        assert_eq!(featuring(Locale::English, &[]), None);
        assert_eq!(
            featuring(Locale::English, &[by(&["Björk"])]).as_deref(),
            Some("With Björk")
        );
        assert_eq!(
            featuring(Locale::English, &[by(&["Björk", "Arca"]), by(&["Björk"])]).as_deref(),
            Some("With Björk and Arca")
        );
        assert_eq!(
            featuring(
                Locale::English,
                &[by(&["A"]), by(&["B"]), by(&["C"]), by(&["D"]), by(&["E"])]
            )
            .as_deref(),
            Some("With A, B, C and more")
        );
    }
}
