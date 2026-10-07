//! Track contributor details.
use super::widgets;
use crate::app::App;
use crate::i18n::{gettext, pgettext};
use crate::model::{Action, Dialog, Loadable};
use crate::theme::{self, Icon};

fn heading(app: &mut App, ui: &mut egui::Ui, title: &str) {
    let palette = app.palette;
    ui.horizontal(|ui| {
        theme::text(ui, title, theme::bold(24.0), palette.text);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::icon_button(
                ui,
                Icon::X,
                18.0,
                palette.secondary,
                palette.text,
                &gettext(app.locale, "Close"),
            )
            .clicked()
            {
                app.actions.push(Action::CloseDialog);
            }
        });
    });
    ui.add_space(16.0);
}

pub(super) fn follow(app: &mut App, ui: &mut egui::Ui, uri: &str) {
    let following = app.is_saved(uri).unwrap_or(false);
    let label = if following {
        pgettext(app.locale, "artist", "Following")
    } else {
        pgettext(app.locale, "artist", "Follow")
    };
    if theme::pill_button(ui, &app.palette, &label, false).clicked() {
        app.actions.push(Action::ToggleSaved(uri.to_string()));
    }
}

fn state(
    app: &mut App,
    ui: &mut egui::Ui,
    uri: &str,
    retry: Dialog,
) -> Option<crate::details::Details> {
    match app.details.get(uri).cloned().unwrap_or(Loadable::NotLoaded) {
        Loadable::Loaded(details) => Some(details),
        Loadable::Loading | Loadable::NotLoaded => {
            widgets::loading_row(ui, &app.palette, app.locale);
            None
        }
        Loadable::Failed(error) => {
            ui.add(
                egui::Label::new(egui::RichText::new(error).color(app.palette.secondary)).wrap(),
            );
            if theme::soft_button(ui, &app.palette, None, &gettext(app.locale, "Retry"), false)
                .clicked()
            {
                app.actions.push(Action::ShowDialog(retry));
            }
            None
        }
    }
}

pub fn credits(app: &mut App, ui: &mut egui::Ui, uri: &str, name: &str) {
    let palette = app.palette;
    ui.set_width((ui.ctx().content_rect().width() - 96.0).clamp(240.0, 420.0));
    heading(app, ui, &gettext(app.locale, "Credits"));
    ui.add(
        egui::Label::new(
            egui::RichText::new(name)
                .font(theme::bold(16.0))
                .color(palette.text),
        )
        .wrap(),
    );
    ui.add_space(20.0);
    ui.separator();
    ui.add_space(20.0);
    let height = (ui.ctx().content_rect().height() - 230.0).max(100.0);
    egui::ScrollArea::vertical()
        .id_salt("track-credits")
        .min_scrolled_height(height)
        .max_height(height)
        .show(ui, |ui| {
            if let Some(details) = state(
                app,
                ui,
                uri,
                Dialog::TrackCredits {
                    uri: uri.into(),
                    name: name.into(),
                },
            ) {
                if details.credits.is_empty() {
                    theme::text(
                        ui,
                        gettext(app.locale, "No contributor credits are available."),
                        theme::regular(14.0),
                        palette.secondary,
                    );
                }
                for (composition, title) in [
                    (false, gettext(app.locale, "Artist")),
                    (true, gettext(app.locale, "Composition & Lyrics")),
                ] {
                    let credits: Vec<_> = details
                        .credits
                        .iter()
                        .filter(|credit| (credit.role == 5) == composition)
                        .collect();
                    if credits.is_empty() {
                        continue;
                    }
                    theme::text(ui, title, theme::bold(20.0), palette.text);
                    ui.add_space(20.0);
                    for credit in credits {
                        let role = crate::details::role_label(credit.role, app.locale);
                        credit_row(
                            app,
                            ui,
                            &credit.name,
                            &role,
                            if composition {
                                None
                            } else {
                                credit.uri.as_deref()
                            },
                        );
                        ui.add_space(22.0);
                    }
                }
                if let Some(label) = details.label {
                    theme::text(
                        ui,
                        gettext(app.locale, "Label"),
                        theme::bold(16.0),
                        palette.text,
                    );
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(label).color(palette.secondary))
                            .wrap(),
                    );
                }
            }
        });
}

pub(super) fn credit_row(
    app: &mut App,
    ui: &mut egui::Ui,
    name: &str,
    role: &str,
    uri: Option<&str>,
) {
    let palette = app.palette;
    // Reserve the Follow button first, so long names wrap without pushing it outside the card.
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(uri) = uri {
                follow(app, ui, uri);
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(name)
                            .font(theme::regular(16.0))
                            .color(palette.text),
                    )
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(role)
                            .font(theme::regular(14.0))
                            .color(palette.secondary),
                    )
                    .wrap(),
                );
            });
        });
    });
}
