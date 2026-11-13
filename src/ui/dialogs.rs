//! Modal dialogs: playlist details, confirmations, shortcuts.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Stroke};

use crate::app::App;
use crate::i18n::{Locale, gettext, ngettext};
use crate::model::{Action, Dialog};
use crate::theme;

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(dialog) = app.dialog.clone() else {
        return;
    };
    if let Dialog::CoverArt { url, shown, title } = &dialog {
        cover_art(app, ctx, url, shown.as_deref(), title);
        return;
    }
    let palette = app.palette;
    let locale = app.locale;
    let frame = Frame::new()
        .fill(palette.overlay)
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(CornerRadius::same(theme::RADIUS + 4))
        .inner_margin(Margin::same(24))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 40,
            spread: 0,
            color: palette.shadow,
        });
    let response = egui::Modal::new(egui::Id::new("dialog"))
        .frame(frame)
        .backdrop_color(egui::Color32::from_black_alpha(if palette.dark {
            150
        } else {
            80
        }))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            match dialog {
                // Drawn by cover_art above.
                Dialog::CoverArt { .. } => {}
                Dialog::PersonalAppIntro => {
                    theme::text(ui, gettext(locale, "Register Spotifast with Spotify"), theme::bold(20.0), palette.text);
                    ui.add_space(12.0);
                    ui.add(egui::Label::new(egui::RichText::new(gettext(locale, "You're on the shared sign-in Spotifast used before. Spotifast no longer offers it, and Spotify can stop accepting it at any time. Register Spotifast so nothing stops your music. It takes about two minutes.")).font(theme::regular(14.0)).color(palette.secondary)).wrap());
                    ui.add_space(10.0);
                    ui.add_space(8.0);
                    buttons(ui, |ui| {
                        if theme::pill_button(ui, &palette, &gettext(locale, "Register Spotifast"), true).clicked() {
                            app.actions.push(Action::OpenPersonalAppSetup);
                        }
                        if theme::pill_button(ui, &palette, &gettext(locale, "Not now"), false).clicked() {
                            app.actions.push(Action::CloseDialog);
                        }
                    });
                }
                Dialog::CreatePlaylist { .. } => create_playlist(app, ui),
                Dialog::EditPlaylist { .. } => edit_playlist(app, ui),
                Dialog::ConfirmDeletePlaylist { id, name, owned } => {
                    theme::text(
                        ui,
                        if owned {
                            gettext(locale, "Delete playlist?")
                        } else {
                            gettext(locale, "Remove from Your Library?")
                        },
                        theme::bold(20.0),
                        palette.text,
                    );
                    ui.add_space(8.0);
                    let body = if owned {
                        // Translators: {name} is a playlist name.
                        gettext(locale, "Delete “{name}”? You can recover it from Spotify for 90 days.")
                    } else {
                        // Translators: {name} is a playlist name.
                        gettext(locale, "“{name}” will no longer appear in Your Library.")
                    }
                    .replace("{name}", &name);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(body)
                                .font(theme::regular(14.0))
                                .color(palette.secondary),
                        )
                        .wrap(),
                    );
                    ui.add_space(20.0);
                    buttons(ui, |ui| {
                        if theme::pill_button(
                            ui,
                            &palette,
                            &if owned {
                                gettext(locale, "Delete")
                            } else {
                                gettext(locale, "Remove")
                            },
                            true,
                        )
                        .clicked()
                        {
                            app.actions.push(Action::DeletePlaylist(id.clone()));
                        }
                        if theme::pill_button(ui, &palette, &gettext(locale, "Cancel"), false).clicked() {
                            app.actions.push(Action::CloseDialog);
                        }
                    });
                }
                Dialog::ConfirmPlaylistDuplicates {
                    playlist_id,
                    playlist_name,
                    items,
                    position,
                    duplicate_uris,
                } => {
                    let multiple = items.len() > 1;
                    theme::text(
                        ui,
                        if multiple {
                            gettext(locale, "Songs already in this playlist")
                        } else {
                            gettext(locale, "Song already in this playlist")
                        },
                        theme::bold(20.0),
                        palette.text,
                    );
                    ui.add_space(8.0);
                    let body = duplicate_message(locale, &playlist_name, &items, &duplicate_uris);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(body)
                                .font(theme::regular(14.0))
                                .color(palette.secondary),
                        )
                        .wrap(),
                    );
                    ui.add_space(20.0);
                    buttons(ui, |ui| {
                        if theme::pill_button(ui, &palette, &gettext(locale, "Add anyway"), true).clicked() {
                            app.actions.push(Action::ConfirmAddToPlaylist {
                                playlist_id: playlist_id.clone(),
                                playlist_name: playlist_name.clone(),
                                items: items.clone(),
                                position,
                            });
                        }
                        if theme::pill_button(ui, &palette, &gettext(locale, "Cancel"), false).clicked() {
                            app.actions.push(Action::CloseDialog);
                        }
                    });
                }
                Dialog::EpisodeDetails { episode } => {
                    bidi_label(
                        ui,
                        &episode.name,
                        theme::bold(20.0),
                        palette.text,
                        false,
                    );
                    let mut meta = Vec::new();
                    if let Some(show) = &episode.show
                        && !show.name.is_empty()
                    {
                        meta.push(show.name.clone());
                    }
                    if let Some(date) = &episode.release_date {
                        meta.push(crate::util::format_date(locale, date));
                    }
                    if episode.duration_ms > 0 {
                        meta.push(crate::util::format_episode_ms(locale, episode.duration_ms));
                    }
                    if !meta.is_empty() {
                        ui.add_space(4.0);
                        bidi_label(
                            ui,
                            &meta.join(" • "),
                            theme::regular(13.0),
                            palette.secondary,
                            false,
                        );
                    }
                    // Row descriptions stop at two lines; the notes run as long
                    // as the podcast made them, so they scroll inside the dialog.
                    let description = plain_notes(&episode.description);
                    if !description.is_empty() {
                        ui.add_space(16.0);
                        theme::text(
                            ui,
                            gettext(locale, "Description"),
                            theme::medium(13.0),
                            palette.secondary,
                        );
                        ui.add_space(6.0);
                        let room = ui.ctx().content_rect().height() - 260.0;
                        episode_description(
                            ui,
                            &episode.uri,
                            &description,
                            &palette,
                            room.max(120.0),
                        );
                    }
                    ui.add_space(20.0);
                    buttons(ui, |ui| {
                        if theme::pill_button(ui, &palette, &gettext(locale, "Close"), true)
                            .clicked()
                        {
                            app.actions.push(Action::CloseDialog);
                        }
                    });
                }
                Dialog::Shortcuts => {
                    theme::text(ui, gettext(locale, "Keyboard shortcuts"), theme::bold(20.0), palette.text);
                    ui.add_space(12.0);
                    // `theme::text` truncates, which in a grid makes each cell
                    // claim almost no width and turns "Ctrl+Shift+A" into
                    // "Ctrl…". A shortcut is unusable when abbreviated, so
                    // these cells are sized to their content.
                    let cell = |ui: &mut egui::Ui, text: &str, font: egui::FontId, color| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).font(font).color(color))
                                .extend()
                                .selectable(false),
                        );
                    };
                    // The list is longer than a small screen is tall, so it
                    // scrolls inside the dialog rather than running off the
                    // bottom with the Done button beyond reach.
                    let room = ui.ctx().content_rect().height() - 190.0;
                    crate::autoscroll::show(
                        ui,
                        egui::ScrollArea::vertical()
                        .max_height(room.max(120.0))
                        .auto_shrink([false, true]),
                        egui::Vec2b::new(false, true),
                        |ui| {
                            egui::Grid::new("shortcuts")
                                .num_columns(2)
                                .spacing([24.0, 8.0])
                                .show(ui, |ui| {
                                    for (keys, description) in super::keys::shortcuts(locale) {
                                        cell(ui, &keys, theme::semibold(13.0), palette.text);
                                        cell(
                                            ui,
                                            &description,
                                            theme::regular(13.5),
                                            palette.secondary,
                                        );
                                        ui.end_row();
                                    }
                                });
                        },
                    );
                    ui.add_space(16.0);
                    buttons(ui, |ui| {
                        if theme::pill_button(ui, &palette, &gettext(locale, "Done"), true).clicked() {
                            app.actions.push(Action::CloseDialog);
                        }
                    });
                }
                Dialog::PremiumNeeded => {
                    theme::text(
                        ui,
                        gettext(locale, "This account cannot play music here"),
                        theme::bold(20.0),
                        palette.text,
                    );
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(gettext(
                                locale,
                                "Playback needs Spotify Premium. Free accounts can browse and search, but cannot play music through Spotifast.",
                            ))
                            .font(theme::regular(14.0))
                            .color(palette.secondary),
                        )
                        .wrap(),
                    );
                    ui.add_space(20.0);
                    buttons(ui, |ui| {
                        if theme::pill_button(ui, &palette, &gettext(locale, "OK"), true).clicked() {
                            app.actions.push(Action::CloseDialog);
                        }
                    });
                }
            }
        });
    app.dialog_rect = Some(response.response.rect);
    if response.should_close() {
        app.actions.push(Action::CloseDialog);
    }
}

// Keep button rows content-sized when the shared modal area last held a taller dialog.
fn buttons(ui: &mut egui::Ui, add_buttons: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
        Layout::right_to_left(Align::Center),
        add_buttons,
    );
}

/// One line or wrapped text in reading order, for either direction, and
/// optionally selectable.
fn bidi_label(
    ui: &mut egui::Ui,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    selectable: bool,
) -> egui::Response {
    let galley = crate::bidi::layout(
        ui.painter(),
        text,
        font,
        color,
        ui.available_width(),
        usize::MAX,
        None,
    );
    ui.add(egui::Label::new(galley).selectable(selectable))
}

/// The episode's notes as plain text. Unlike
/// [`crate::util::strip_html`], paragraphs, lists, and `<br>` keep their
/// boundaries instead of running together on one line.
fn plain_notes(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut tag = String::new();
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let name = tag
                    .trim()
                    .trim_start_matches('/')
                    .trim_end_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let closing = tag.trim_start().starts_with('/');
                let breaks = match name.as_str() {
                    "p" | "div" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => 2,
                    "ul" | "ol" if closing => 2,
                    "ul" | "ol" => 1,
                    "br" | "li" | "tr" => 1,
                    _ => 0,
                };
                if breaks > 0 {
                    keep_breaks(&mut out, breaks);
                }
            }
            '>' => {}
            _ if in_tag => tag.push(character),
            _ => out.push(character),
        }
    }
    crate::util::strip_html(&out).trim().to_string()
}

/// Leaves exactly `count` newlines at the end of `out`, never starting the
/// text with one.
fn keep_breaks(out: &mut String, count: usize) {
    if out.is_empty() {
        return;
    }
    while out.ends_with([' ', '\t']) {
        out.pop();
    }
    let trailing = out.chars().rev().take_while(|c| *c == '\n').count();
    if trailing > count {
        out.truncate(out.len() - (trailing - count));
    } else {
        for _ in trailing..count {
            out.push('\n');
        }
    }
}

/// The episode's notes, scrolling inside the dialog when they run long.
fn episode_description(
    ui: &mut egui::Ui,
    episode_uri: &str,
    description: &str,
    palette: &theme::Palette,
    max_height: f32,
) -> egui::scroll_area::ScrollAreaOutput<()> {
    crate::autoscroll::show(
        ui,
        egui::ScrollArea::vertical()
            .id_salt(("episode-description", episode_uri))
            .max_height(max_height)
            .auto_shrink([false, true]),
        egui::Vec2b::new(false, true),
        |ui| {
            bidi_label(ui, description, theme::regular(14.0), palette.text, true);
        },
    )
}

fn duplicate_message(
    locale: Locale,
    playlist_name: &str,
    items: &[crate::api::models::PlayableItem],
    duplicate_uris: &[String],
) -> String {
    let mut seen = std::collections::HashSet::new();
    let names: Vec<&str> = items
        .iter()
        .filter(|item| duplicate_uris.iter().any(|uri| uri == item.uri()))
        .map(crate::api::models::PlayableItem::name)
        .filter(|name| seen.insert(*name))
        .collect();
    let one_item = items.len() == 1;
    let message = match names.as_slice() {
        [] if one_item => {
            // Translators: {playlist} is a playlist name.
            gettext(
                locale,
                "This song is already in “{playlist}”. Add it again?",
            )
        }
        [] => {
            // Translators: {playlist} is a playlist name.
            gettext(
                locale,
                "This song is already in “{playlist}”. Add them anyway?",
            )
        }
        [name] if one_item => {
            // Translators: {name} is a song name and {playlist} a playlist name.
            gettext(locale, "“{name}” is already in “{playlist}”. Add it again?")
                .replace("{name}", name)
                .into()
        }
        [name] => {
            // Translators: {name} is a song name and {playlist} a playlist name.
            gettext(
                locale,
                "“{name}” is already in “{playlist}”. Add them anyway?",
            )
            .replace("{name}", name)
            .into()
        }
        [first, second] => {
            // Translators: {first} and {second} are song names, {playlist} a playlist name.
            gettext(
                locale,
                "“{first}” and “{second}” are already in “{playlist}”. Add them anyway?",
            )
            .replace("{first}", first)
            .replace("{second}", second)
            .into()
        }
        [first, second, rest @ ..] => ngettext(
            locale,
            // Translators: {first} and {second} are song names, {count} is how many more songs are already in the playlist, and {playlist} is a playlist name.
            "“{first}”, “{second}”, and {count} more are already in “{playlist}”. Add them anyway?",
            "“{first}”, “{second}”, and {count} more are already in “{playlist}”. Add them anyway?",
            rest.len() as u32,
        )
        .replace("{first}", first)
        .replace("{second}", second)
        .replace("{count}", &rest.len().to_string())
        .into(),
    };
    message.replace("{playlist}", playlist_name)
}

fn text_field(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    locale: crate::i18n::Locale,
    id: &str,
    text: &mut String,
    hint: &str,
    focus: bool,
) -> egui::Response {
    let response = Frame::new()
        .fill(palette.surface)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            super::widgets::text_edit(
                ui,
                locale,
                egui::TextEdit::singleline(text)
                    .id(egui::Id::new(id))
                    .hint_text(egui::RichText::new(hint).color(palette.dim))
                    .font(theme::regular(14.0))
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY),
            )
        })
        .inner;
    if focus && ui.memory(|memory| memory.focused().is_none()) {
        response.request_focus();
    }
    response
}

fn create_playlist(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let busy = app.playlist_busy;
    let Some(Dialog::CreatePlaylist {
        name,
        public,
        add_uris,
    }) = &mut app.dialog
    else {
        return;
    };
    theme::text(
        ui,
        gettext(locale, "New playlist"),
        theme::bold(20.0),
        palette.text,
    );
    ui.add_space(12.0);
    theme::text(
        ui,
        gettext(locale, "Name"),
        theme::medium(13.0),
        palette.secondary,
    );
    let field = text_field(
        ui,
        &palette,
        locale,
        "playlist-name",
        name,
        &gettext(locale, "My playlist"),
        true,
    );
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        super::widgets::switch(ui, &palette, &gettext(locale, "Public playlist"), public);
        theme::text(
            ui,
            gettext(locale, "Public playlist"),
            theme::regular(14.0),
            palette.text,
        );
    });
    if !add_uris.is_empty() {
        ui.add_space(6.0);
        let count = add_uris.len();
        theme::text(
            ui,
            ngettext(
                locale,
                // Translators: {count} is the number of songs.
                "{count} song will be added.",
                "{count} songs will be added.",
                count as u32,
            )
            .replace("{count}", &count.to_string()),
            theme::regular(13.0),
            palette.secondary,
        );
    }
    ui.add_space(20.0);
    let submit = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    let name_value = name.trim().to_string();
    let public_value = *public;
    let uris = add_uris.clone();
    buttons(ui, |ui| {
        if busy {
            theme::spinner(ui, 18.0, palette.accent);
        } else {
            let create = theme::pill_button(ui, &palette, &gettext(locale, "Create"), true)
                .clicked()
                || submit;
            if create && !name_value.is_empty() {
                app.actions.push(Action::CreatePlaylist {
                    name: name_value.clone(),
                    public: public_value,
                    add_uris: uris.clone(),
                });
            }
            if theme::pill_button(ui, &palette, &gettext(locale, "Cancel"), false).clicked() {
                app.actions.push(Action::CloseDialog);
            }
        }
    });
}

fn edit_playlist(app: &mut App, ui: &mut egui::Ui) {
    let existing_image = if let Some(Dialog::EditPlaylist { id, .. }) = &app.dialog {
        app.library
            .playlists
            .get()
            .and_then(|playlists| playlists.iter().find(|playlist| &playlist.id == id))
            .and_then(|playlist| crate::api::models::pick_image(&playlist.images, 100))
            .map(str::to_owned)
    } else {
        None
    };
    let palette = app.palette;
    let locale = app.locale;
    let busy = app.playlist_busy;
    let Some(Dialog::EditPlaylist {
        cover,
        id,
        name,
        description,
        public,
    }) = &mut app.dialog
    else {
        return;
    };
    theme::text(
        ui,
        gettext(locale, "Edit details"),
        theme::bold(20.0),
        palette.text,
    );
    ui.add_space(12.0);
    egui::ScrollArea::vertical()
        .max_height((ui.ctx().content_rect().height() - 210.0).max(100.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(selected) = &cover.selection {
                    ui.ctx()
                        .include_bytes(selected.uri.clone(), selected.jpeg.clone());
                    ui.add(
                        egui::Image::new(selected.uri.clone())
                            .texture_options(crate::images::COVER_TEXTURE)
                            .fit_to_exact_size(egui::vec2(72.0, 72.0))
                            .alt_text(gettext(locale, "Selected playlist cover")),
                    );
                } else if let Some(url) = &existing_image {
                    ui.add(
                        egui::Image::new(url)
                            .texture_options(crate::images::COVER_TEXTURE)
                            .fit_to_exact_size(egui::vec2(72.0, 72.0))
                            .alt_text(gettext(locale, "Current playlist cover")),
                    );
                }
                ui.vertical(|ui| {
                    if cover.request.is_some() || cover.uploading.is_some() {
                        theme::spinner(ui, 18.0, palette.accent);
                        theme::text(
                            ui,
                            if cover.uploading.is_some() {
                                gettext(locale, "Uploading cover…")
                            } else {
                                gettext(locale, "Choosing cover…")
                            },
                            theme::regular(13.0),
                            palette.secondary,
                        );
                    } else {
                        if theme::pill_button(ui, &palette, &gettext(locale, "Change cover"), false)
                            .clicked()
                        {
                            app.actions.push(Action::ChoosePlaylistCover(id.clone()));
                        }
                        if cover.selection.is_some()
                            && theme::pill_button(
                                ui,
                                &palette,
                                &gettext(locale, "Upload cover"),
                                true,
                            )
                            .clicked()
                        {
                            app.actions.push(Action::UploadPlaylistCover(id.clone()));
                        }
                    }
                });
            });
            if let Some(error) = &cover.error {
                ui.add(egui::Label::new(egui::RichText::new(error).color(palette.text)).wrap());
            }
            ui.add_space(10.0);
            theme::text(
                ui,
                gettext(locale, "Name"),
                theme::medium(13.0),
                palette.secondary,
            );
            text_field(
                ui,
                &palette,
                locale,
                "edit-name",
                name,
                &gettext(locale, "Playlist name"),
                true,
            );
            ui.add_space(10.0);
            theme::text(
                ui,
                gettext(locale, "Description"),
                theme::medium(13.0),
                palette.secondary,
            );
            Frame::new()
                .fill(palette.surface)
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    super::widgets::text_edit(
                        ui,
                        locale,
                        egui::TextEdit::multiline(description)
                            .id(egui::Id::new("edit-description"))
                            .hint_text(
                                egui::RichText::new(gettext(locale, "Optional description"))
                                    .color(palette.dim),
                            )
                            .font(theme::regular(14.0))
                            .frame(egui::Frame::NONE)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                // Unknown shows as off; only a change of the switch is sent, so
                // a playlist nothing has described keeps whatever it was.
                let mut shown = public.unwrap_or(false);
                if super::widgets::switch(
                    ui,
                    &palette,
                    &gettext(locale, "Public playlist"),
                    &mut shown,
                )
                .changed()
                {
                    *public = Some(shown);
                }
                theme::text(
                    ui,
                    gettext(locale, "Public playlist"),
                    theme::regular(14.0),
                    palette.text,
                );
            });
        });
    ui.add_space(20.0);
    let id = id.clone();
    let busy = busy || cover.uploading.is_some() || cover.request.is_some();
    let name_value = name.trim().to_string();
    let description_value = description.trim().to_string();
    let public_value = *public;
    buttons(ui, |ui| {
        if busy {
            theme::spinner(ui, 18.0, palette.accent);
        } else {
            if theme::pill_button(ui, &palette, &gettext(locale, "Save"), true).clicked()
                && !name_value.is_empty()
            {
                app.actions.push(Action::UpdatePlaylist {
                    id: id.clone(),
                    name: name_value.clone(),
                    description: description_value.clone(),
                    public: public_value,
                });
            }
            if theme::pill_button(ui, &palette, &gettext(locale, "Cancel"), false).clicked() {
                app.actions.push(Action::CloseDialog);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::duplicate_message;
    use crate::api::models::{PlayableItem, Track};
    use crate::i18n::Locale;

    fn song(uri: &str, name: &str) -> PlayableItem {
        PlayableItem::Track(Track {
            uri: uri.into(),
            name: name.into(),
            ..Default::default()
        })
    }

    #[test]
    fn duplicate_dialog_names_the_song() {
        let items = vec![song("spotify:track:honey", "Honey")];
        let message = duplicate_message(
            Locale::English,
            "The best music ever",
            &items,
            &["spotify:track:honey".into()],
        );

        assert_eq!(
            message,
            "“Honey” is already in “The best music ever”. Add it again?"
        );
    }

    #[test]
    fn duplicate_dialog_names_only_the_duplicates_in_a_selection() {
        let items = vec![
            song("spotify:track:honey", "Honey"),
            song("spotify:track:new", "New song"),
        ];
        let message = duplicate_message(
            Locale::English,
            "The best music ever",
            &items,
            &["spotify:track:honey".into()],
        );

        assert_eq!(
            message,
            "“Honey” is already in “The best music ever”. Add them anyway?"
        );
    }
}

/// A page's header cover, as large as the window allows, over the dimmed
/// page. A click anywhere, on the cover or around it, or Esc closes it.
fn cover_art(app: &mut App, ctx: &egui::Context, url: &str, shown: Option<&str>, title: &str) {
    let palette = app.palette;
    let side = cover_art_side(ctx.content_rect().size());
    let response = egui::Modal::new(egui::Id::new("cover-art"))
        .frame(Frame::new().shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 40,
            spread: 0,
            color: palette.shadow,
        }))
        .backdrop_color(egui::Color32::from_black_alpha(if palette.dark {
            190
        } else {
            150
        }))
        .show(ctx, |ui| {
            let (rect, cover) =
                ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::click());
            cover.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, title));
            super::widgets::paint_cover_with_thumbnail(
                ui,
                &palette,
                super::widgets::CoverSources {
                    requested: Some(url),
                    previous: shown,
                    ..Default::default()
                },
                rect,
                8.0,
                theme::Icon::Music,
                Some(app.backend.art()),
            );
            cover.clicked()
        });
    app.dialog_rect = Some(response.response.rect);
    if response.inner || response.should_close() {
        app.actions.push(Action::CloseDialog);
    }
}

/// The enlarged cover's side: as much of the window as leaves a margin
/// around it, and no larger than Spotify's largest artwork, 640 pixels wide.
fn cover_art_side(window: egui::Vec2) -> f32 {
    (window.x.min(window.y) - 96.0).clamp(120.0, 640.0)
}

#[cfg(test)]
mod cover_art_tests {
    use super::cover_art_side;

    #[test]
    fn the_enlarged_cover_fits_the_window_up_to_spotifys_largest_artwork() {
        assert_eq!(cover_art_side(egui::vec2(1240.0, 800.0)), 640.0);
        assert_eq!(cover_art_side(egui::vec2(760.0, 620.0)), 524.0);
        assert_eq!(cover_art_side(egui::vec2(100.0, 100.0)), 120.0);
    }
}

#[cfg(test)]
mod episode_details_tests {
    use super::*;
    use crate::api::models::{Episode, Show};
    use crate::app::AppOptions;
    use crate::backend::Waker;
    use crate::paths::AppDirs;
    use crate::settings::Settings;

    fn test_app() -> App {
        let root = std::env::temp_dir().join(format!(
            "spotifast-episode-details-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        App::new(
            &Waker::default(),
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
                autostart: false,
            },
        )
    }

    #[test]
    fn episode_details_dialog_shows_the_full_description() {
        let mut app = test_app();
        app.dialog = Some(Dialog::EpisodeDetails {
            episode: Box::new(Episode {
                name: "A long conversation".into(),
                description:
                    "<p>First paragraph.</p><p>Second paragraph that the row cuts off.</p>".into(),
                duration_ms: 3_600_000,
                release_date: Some("2026-11-01".into()),
                show: Some(Show {
                    name: "Rework".into(),
                    ..Show::default()
                }),
                ..Episode::default()
            }),
        });
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut frame = || {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    ..Default::default()
                },
                |_| show(&mut app, &ctx),
            );
            output.textures_delta.clear();
            output.shapes
        };
        frame();
        let shapes = frame();
        let mut painted = Vec::new();
        for clipped in &shapes {
            collect_text(&clipped.shape, &mut painted);
        }
        assert!(
            painted
                .iter()
                .any(|text| text.contains("Second paragraph that the row cuts off")),
            "the dialog paints the notes past the row's two lines: {painted:?}"
        );
        assert_eq!(
            painted
                .iter()
                .filter(|text| text.contains("Second paragraph"))
                .count(),
            1,
            "the notes appear once, whole"
        );
        assert!(
            painted
                .iter()
                .any(|text| text.contains("Rework") && text.contains("2026")),
            "the dialog names the podcast and the date: {painted:?}"
        );
        assert!(
            painted.iter().any(|text| text.contains("1 hr")),
            "the dialog shows the length: {painted:?}"
        );
    }

    /// Paragraphs, lists, and `<br>` keep their boundaries in the notes.
    #[test]
    fn episode_notes_keep_paragraphs_and_line_breaks() {
        assert_eq!(
            plain_notes("<p>First paragraph.</p><p>Second paragraph.</p>"),
            "First paragraph.\n\nSecond paragraph."
        );
        assert_eq!(
            plain_notes("Chapters:<br>00:00 One<br>08:12 Two"),
            "Chapters:\n00:00 One\n08:12 Two"
        );
        assert_eq!(plain_notes("<ul><li>One</li><li>Two</li></ul>"), "One\nTwo");
        assert_eq!(plain_notes("Tom &amp; Jerry"), "Tom & Jerry");
    }

    fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => out.push(text.galley.text().to_string()),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect_text(shape, out);
                }
            }
            _ => {}
        }
    }

    /// Notes longer than the dialog scroll instead of running past its edge.
    #[test]
    fn long_episode_notes_scroll_inside_the_dialog() {
        let notes: String = (0..80)
            .map(|index| {
                format!("Paragraph {index}: the notes run past what the dialog can show at once.")
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let palette = test_app().palette;
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let capture = |events| {
            let mut seen = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800.0, 600.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    seen = Some(episode_description(
                        ui,
                        "spotify:episode:test",
                        &notes,
                        &palette,
                        200.0,
                    ));
                },
            );
            output.textures_delta.clear();
            seen.expect("the notes draw")
        };
        let first = capture(vec![]);
        assert!(
            first.content_size.y > first.inner_rect.height(),
            "the notes are taller than the area: {} against {}",
            first.content_size.y,
            first.inner_rect.height()
        );
        assert_eq!(first.state.offset.y, 0.0, "the notes start at the top");
        let second = capture(vec![
            egui::Event::PointerMoved(first.inner_rect.center()),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -5.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            },
        ]);
        assert!(
            second.state.offset.y > 0.0,
            "the wheel scrolls the notes: {}",
            second.state.offset.y
        );
    }
}
