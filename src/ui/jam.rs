//! The queue panel's Jam tab: joining the jam server, and the shared queue.

use egui::RichText;

use crate::app::{App, Target};
use crate::i18n::{Locale, gettext};
use crate::jam::net::EndReason;
use crate::jam::protocol::{JamItem, Rejection};
use crate::jam::view::JamStatus;
use crate::model::Action;
use crate::theme::{self, Icon, Palette};

use super::widgets;

pub fn contents(app: &mut App, ui: &mut egui::Ui) {
    match app.jam.status {
        JamStatus::Off => start(app, ui),
        JamStatus::Starting => widgets::loading_row(ui, &app.palette, app.locale),
        JamStatus::Joined | JamStatus::Reconnecting(_) => session(app, ui),
    }
}

/// Not in the jam: the server code, and the button that joins.
fn start(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    if let Some(reason) = app
        .jam
        .ended
        .as_ref()
        .and_then(|reason| ended(locale, reason))
    {
        notice(ui, &palette, &reason);
        ui.add_space(12.0);
    }
    body(
        ui,
        &palette,
        &gettext(
            locale,
            "Listen together with everyone who has the jam server's code. Each person plays the songs on their own Spotify account.",
        ),
    );
    ui.add_space(12.0);
    heading(ui, &palette, &gettext(locale, "Jam server code"));
    // The code being typed is the field's own until joining keeps it.
    let id = ui.make_persistent_id("jam-server-code");
    let mut code = ui
        .data(|data| data.get_temp::<String>(id))
        .unwrap_or_else(|| app.settings.jam_server.clone());
    let field = widgets::text_edit(
        ui,
        locale,
        egui::TextEdit::singleline(&mut code)
            .password(true)
            .hint_text(gettext(locale, "Jam server code"))
            .desired_width(f32::INFINITY),
    );
    let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    ui.add_space(8.0);
    let clicked = theme::soft_button(
        ui,
        &palette,
        Some(Icon::Users),
        &gettext(locale, "Join"),
        false,
    )
    .clicked();
    if (entered || clicked) && !code.trim().is_empty() {
        app.actions.push(Action::JoinJam(code.trim().to_string()));
    }
    ui.data_mut(|data| data.insert_temp(id, code));
}

/// In the jam: who listens, what plays, and what follows.
fn session(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    if matches!(app.jam.status, JamStatus::Reconnecting(_)) {
        notice(ui, &palette, &gettext(locale, "Reconnecting…"));
        ui.add_space(8.0);
    }
    if !app.local_ready || !matches!(app.target(), Target::Local) {
        notice(
            ui,
            &palette,
            &gettext(
                locale,
                "The jam plays on this computer. Switch playback here to follow it.",
            ),
        );
        ui.add_space(8.0);
    }

    let listeners = app
        .jam
        .state
        .as_ref()
        .map(|state| {
            state
                .participants
                .iter()
                .map(|participant| participant.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    heading(ui, &palette, &gettext(locale, "Listeners"));
    body(ui, &palette, &listeners);
    ui.add_space(12.0);

    let current = app
        .jam
        .state
        .as_ref()
        .and_then(|state| state.current.clone());
    if let Some(current) = current {
        heading(ui, &palette, &gettext(locale, "Now playing"));
        row(ui, app, &current, false, false);
        ui.add_space(12.0);
    }

    heading(
        ui,
        &palette,
        &gettext(
            locale,
            // Translators: Upcoming songs from the current playlist or album, after manually queued songs.
            "Next up",
        ),
    );
    let queued: Vec<JamItem> = app.jam.shown_queue().cloned().collect();
    let pending: Vec<JamItem> = app
        .jam
        .pending
        .iter()
        .map(|pending| pending.item.clone())
        .collect();
    if queued.is_empty() && pending.is_empty() {
        body(
            ui,
            &palette,
            &gettext(locale, "Right-click a song and choose Add to jam."),
        );
    }
    // Everyone has the same rights: any song can be taken out.
    for item in &queued {
        row(ui, app, item, true, false);
    }
    for item in &pending {
        row(ui, app, item, false, true);
    }

    ui.add_space(16.0);
    if theme::soft_button(
        ui,
        &palette,
        Some(Icon::LogOut),
        &gettext(locale, "Leave the jam"),
        false,
    )
    .clicked()
    {
        app.actions.push(Action::LeaveJam);
    }
}

/// The artists, and who added the song while they are still listening.
fn subtitle(app: &App, item: &JamItem) -> String {
    match app.jam.name_of(item.added_by) {
        Some(name) => {
            let added_by = gettext(
                app.locale,
                // Translators: Keep {name} exactly as written. It becomes the name of the person who added the song.
                "Added by {name}",
            )
            .replace("{name}", name);
            if item.artists.is_empty() {
                added_by
            } else {
                format!("{} · {added_by}", item.artists)
            }
        }
        None => item.artists.clone(),
    }
}

/// One song of the jam. A pending one is dimmed until the server shows it.
fn row(ui: &mut egui::Ui, app: &mut App, item: &JamItem, removable: bool, pending: bool) {
    let palette = app.palette;
    let subtitle = subtitle(app, item);
    ui.scope(|ui| {
        if pending {
            ui.multiply_opacity(0.55);
        }
        ui.horizontal(|ui| {
            let button = if removable { 30.0 } else { 0.0 };
            let width = (ui.available_width() - button).max(40.0);
            ui.vertical(|ui| {
                ui.set_width(width);
                let title = widgets::ellipsized(
                    ui,
                    &item.title,
                    theme::medium(13.0),
                    palette.text,
                    width,
                    1,
                );
                ui.add(egui::Label::new(title).selectable(false));
                let subtitle = widgets::ellipsized(
                    ui,
                    &subtitle,
                    theme::regular(12.0),
                    palette.secondary,
                    width,
                    1,
                );
                ui.add(egui::Label::new(subtitle).selectable(false));
            });
            if removable
                && theme::icon_button(
                    ui,
                    Icon::X,
                    14.0,
                    palette.secondary,
                    palette.text,
                    &gettext(app.locale, "Remove from the jam"),
                )
                .clicked()
            {
                app.actions.push(Action::RemoveFromJam(item.id));
            }
        });
    });
    ui.add_space(6.0);
}

fn heading(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    theme::text(ui, text, theme::semibold(14.0), palette.text);
    ui.add_space(4.0);
}

fn body(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(text)
                .font(theme::regular(13.0))
                .color(palette.secondary),
        )
        .wrap(),
    );
}

fn notice(ui: &mut egui::Ui, palette: &Palette, text: &str) {
    ui.horizontal(|ui| {
        theme::icon(ui, Icon::CircleAlert, 14.0, palette.secondary);
        body(ui, palette, text);
    });
}

/// Why the last jam ended, when that needs saying.
pub fn ended(locale: Locale, reason: &EndReason) -> Option<String> {
    Some(
        match reason {
            EndReason::Left => return None,
            EndReason::Rejected(Rejection::BadPassword) => {
                gettext(locale, "The jam server refused this code.")
            }
            EndReason::Rejected(Rejection::Full) => gettext(locale, "That jam is full."),
            EndReason::Rejected(Rejection::IncompatibleVersion) => {
                gettext(locale, "The jam server runs another version of Spotifast.")
            }
            EndReason::Unreachable(_) => gettext(locale, "Couldn't reach the jam."),
        }
        .into_owned(),
    )
}
