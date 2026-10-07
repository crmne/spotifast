//! The queue panel's Jam tab: hosting, joining, and the shared queue.

use egui::{Align, Layout, RichText};

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
        JamStatus::Hosting | JamStatus::Joined | JamStatus::Reconnecting(_) => session(app, ui),
    }
}

/// No jam yet: host one, or join one with a code.
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
    heading(ui, &palette, &gettext(locale, "Host a jam"));
    body(
        ui,
        &palette,
        &gettext(
            locale,
            "Friends on your network join with the invitation code. Everyone listens on their own Spotify account.",
        ),
    );
    ui.add_space(8.0);
    if theme::soft_button(
        ui,
        &palette,
        Some(Icon::Users),
        &gettext(locale, "Host a jam"),
        false,
    )
    .clicked()
    {
        app.actions.push(Action::HostJam);
    }
    ui.add_space(20.0);
    heading(ui, &palette, &gettext(locale, "Join a jam"));
    // The code being typed is the field's own, like any text in progress.
    let id = ui.make_persistent_id("jam-invitation-code");
    let mut code = ui
        .data(|data| data.get_temp::<String>(id))
        .unwrap_or_default();
    let field = widgets::text_edit(
        ui,
        locale,
        egui::TextEdit::singleline(&mut code)
            .hint_text(gettext(locale, "Invitation code"))
            .desired_width(f32::INFINITY),
    );
    let entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    ui.add_space(8.0);
    let clicked = theme::soft_button(
        ui,
        &palette,
        Some(Icon::CirclePlus),
        &gettext(locale, "Join"),
        false,
    )
    .clicked();
    if (entered || clicked) && !code.trim().is_empty() {
        app.actions.push(Action::JoinJam(code.trim().to_string()));
    }
    ui.data_mut(|data| data.insert_temp(id, code));
}

/// In a jam: the invitation, who listens, what plays, and what follows.
fn session(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let hosting = app.jam.status == JamStatus::Hosting;
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
    if hosting && let Some(invite) = &app.jam.invite {
        let code = invite.code();
        heading(ui, &palette, &gettext(locale, "Invitation code"));
        ui.horizontal(|ui| {
            let copy = ui
                .with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let copy = theme::icon_button(
                        ui,
                        Icon::Copy,
                        16.0,
                        palette.secondary,
                        palette.text,
                        &gettext(locale, "Copy invitation"),
                    )
                    .clicked();
                    ui.add(
                        egui::Label::new(
                            RichText::new(&code)
                                .monospace()
                                .size(12.0)
                                .color(palette.text),
                        )
                        .wrap(),
                    );
                    copy
                })
                .inner;
            if copy {
                app.actions.push(Action::CopyJamInvite);
            }
        });
        ui.add_space(8.0);
        let mut control = app
            .jam
            .state
            .as_ref()
            .is_some_and(|state| state.permissions.guests_control_playback);
        let label = gettext(locale, "Guests control playback");
        ui.horizontal(|ui| {
            if widgets::switch(ui, &palette, &label, &mut control).changed() {
                app.actions.push(Action::SetJamGuestControl(control));
            }
            ui.add_space(6.0);
            theme::text(ui, &*label, theme::regular(13.0), palette.text);
        });
        ui.add_space(12.0);
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
    for item in &queued {
        let removable = app.jam.can_remove(item);
        row(ui, app, item, removable, false);
    }
    for item in &pending {
        row(ui, app, item, false, true);
    }

    ui.add_space(16.0);
    let leave = if hosting {
        gettext(locale, "End the jam")
    } else {
        gettext(locale, "Leave the jam")
    };
    if theme::soft_button(ui, &palette, Some(Icon::LogOut), &leave, false).clicked() {
        app.actions.push(Action::LeaveJam);
    }
}

/// The artists, and who added the song when that is still known.
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

/// One song of the jam. A pending one is dimmed until the host shows it.
fn row(ui: &mut egui::Ui, app: &mut App, item: &JamItem, removable: bool, pending: bool) {
    let palette = app.palette;
    let title = item.title.as_str();
    let subtitle = subtitle(app, item);
    let subtitle = subtitle.as_str();
    let removable = removable.then_some(item.id);
    ui.scope(|ui| {
        if pending {
            ui.multiply_opacity(0.55);
        }
        ui.horizontal(|ui| {
            let button = if removable.is_some() { 30.0 } else { 0.0 };
            let width = (ui.available_width() - button).max(40.0);
            ui.vertical(|ui| {
                ui.set_width(width);
                let title =
                    widgets::ellipsized(ui, title, theme::medium(13.0), palette.text, width, 1);
                ui.add(egui::Label::new(title).selectable(false));
                let subtitle = widgets::ellipsized(
                    ui,
                    subtitle,
                    theme::regular(12.0),
                    palette.secondary,
                    width,
                    1,
                );
                ui.add(egui::Label::new(subtitle).selectable(false));
            });
            if let Some(item) = removable
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
                app.actions.push(Action::RemoveFromJam(item));
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
            EndReason::HostClosed => gettext(locale, "The host ended the jam."),
            EndReason::Rejected(Rejection::BadInvite) => {
                gettext(locale, "That invitation is wrong or has expired.")
            }
            EndReason::Rejected(Rejection::Full) => gettext(locale, "That jam is full."),
            EndReason::Rejected(Rejection::IncompatibleVersion) => {
                gettext(locale, "That jam runs another version of Spotifast.")
            }
            EndReason::CannotListen(_) => {
                gettext(locale, "Couldn't start the jam on this network.")
            }
            EndReason::Unreachable(_) => gettext(locale, "Couldn't reach the jam."),
        }
        .into_owned(),
    )
}
