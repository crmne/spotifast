//! Track details alongside the current page, below the shared header.
use super::widgets;
use crate::api::models::pick_image;
use crate::app::App;
use crate::i18n::{gettext, pgettext};
use crate::model::{Action, Loadable, Page};
use crate::theme::{self, Icon};

fn card(ui: &mut egui::Ui, palette: &theme::Palette, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(palette.surface)
        .corner_radius(8)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui);
        });
}

pub fn side_panel(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let fit = super::yielding_panel(
        ui.ctx(),
        "now-playing-panel",
        theme::SIDE_PANEL_MIN_WIDTH..=640.0,
        app.settings.lyrics_width,
        ui.available_width() - super::page_min_width(app, ui.ctx()),
    );
    app.actions.push(Action::LoadNowPlayingArtists);
    let panel_style = super::begin_panel_resize(app, ui);
    let response = egui::Panel::right("now-playing-panel")
        .default_size(app.settings.lyrics_width)
        .size_range(fit.range.clone())
        .resizable(true)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(palette.window)
                .inner_margin(egui::Margin {
                    left: 4,
                    right: 4,
                    top: 4,
                    bottom: 4,
                }),
        )
        .show(ui, |ui| {
            ui.set_style(panel_style.clone());
            egui::Frame::new()
                .fill(palette.panel)
                .corner_radius(8)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.set_min_height(ui.available_height());
                    ui.set_min_width(ui.available_width());
                    let from = app.playing_from();
                    let title = from
                        .as_ref()
                        .map(|from| from.name.clone())
                        .unwrap_or_else(|| gettext(app.locale, "Now playing").into_owned());
                    if theme::link(ui, &title, theme::bold(16.0), palette.text).clicked()
                        && let Some(page) = from.and_then(|from| from.page)
                    {
                        app.actions.push(Action::Open(page));
                    }
                    ui.add_space(14.0);
                    egui::ScrollArea::vertical()
                        .id_salt("now-playing-scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let Some(now) = app.now_playing() else {
                                theme::text(
                                    ui,
                                    gettext(app.locale, "Nothing playing"),
                                    theme::regular(14.0),
                                    palette.secondary,
                                );
                                return;
                            };
                            let size = ui.available_width();
                            let (rect, response) = ui
                                .allocate_exact_size(egui::Vec2::splat(size), egui::Sense::click());
                            let art_url = now.art_url.as_deref().or(now.art_small.as_deref());
                            // Preserve the complete image rather than cropping a non-square cover.
                            widgets::paint_cover_contained(
                                ui,
                                &palette,
                                art_url,
                                rect,
                                8.0,
                                Some(app.backend.art()),
                            );
                            let page = now
                                .album_id
                                .as_ref()
                                .map(|id| Page::Album(id.clone()))
                                .or_else(|| now.show_id.as_ref().map(|id| Page::Show(id.clone())));
                            if response.clicked()
                                && let Some(page) = &page
                            {
                                app.actions.push(Action::Open(page.clone()));
                            }
                            ui.add_space(12.0);
                            let saved = app.is_saved(&now.uri).unwrap_or(false);
                            let locale = app.locale;
                            let mut copy = false;
                            let mut save = false;
                            egui::Sides::new().shrink_left().show(
                                ui,
                                |ui| {
                                    ui.vertical(|ui| {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&now.title)
                                                    .font(theme::bold(20.0))
                                                    .color(palette.text),
                                            )
                                            .wrap(),
                                        );
                                        ui.horizontal_wrapped(|ui| {
                                            if now.artists.is_empty() {
                                                theme::text(
                                                    ui,
                                                    &now.subtitle,
                                                    theme::regular(13.0),
                                                    palette.secondary,
                                                );
                                            } else {
                                                widgets::artist_links(
                                                    ui,
                                                    app,
                                                    &now.artists,
                                                    theme::regular(13.0),
                                                    palette.secondary,
                                                );
                                            }
                                        });
                                    });
                                },
                                |ui| {
                                    if theme::icon_button(
                                        ui,
                                        Icon::ExternalLink,
                                        18.0,
                                        palette.secondary,
                                        palette.text,
                                        &gettext(locale, "Copy link"),
                                    )
                                    .clicked()
                                    {
                                        copy = true;
                                    }
                                    if !now.is_episode {
                                        let label = gettext(
                                            locale,
                                            if saved {
                                                "Remove from Liked Songs"
                                            } else {
                                                "Save to Liked Songs"
                                            },
                                        );
                                        if theme::icon_button(
                                            ui,
                                            if saved {
                                                Icon::CircleCheck
                                            } else {
                                                Icon::CirclePlus
                                            },
                                            20.0,
                                            if saved {
                                                palette.accent
                                            } else {
                                                palette.secondary
                                            },
                                            palette.text,
                                            &label,
                                        )
                                        .clicked()
                                        {
                                            save = true;
                                        }
                                    }
                                },
                            );
                            if copy {
                                app.actions.push(Action::CopyLink(now.uri.clone()));
                            }
                            if save {
                                app.actions.push(Action::ToggleSaved(now.uri.clone()));
                            }
                            ui.add_space(18.0);
                            for artist_ref in &now.artists {
                                let artist = artist_ref
                                    .id
                                    .as_ref()
                                    .and_then(|id| app.artist_pages.get(id))
                                    .and_then(|page| page.artist.get())
                                    .cloned();
                                card(ui, &palette, |ui| {
                                    theme::text(
                                        ui,
                                        gettext(app.locale, "Artist"),
                                        theme::bold(14.0),
                                        palette.text,
                                    );
                                    if let Some(artist) = &artist
                                        && let Some(url) = pick_image(&artist.images, 640)
                                    {
                                        ui.add_space(8.0);
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(
                                                ui.available_width(),
                                                ui.available_width() * 0.65,
                                            ),
                                            egui::Sense::hover(),
                                        );
                                        widgets::paint_cover(
                                            ui,
                                            &palette,
                                            Some(url),
                                            rect,
                                            6.0,
                                            Icon::Music,
                                            Some(app.backend.art()),
                                        );
                                    }
                                    ui.add_space(8.0);
                                    ui.horizontal(|ui| {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if let Some(id) = &artist_ref.id {
                                                    let uri = format!("spotify:artist:{id}");
                                                    let following =
                                                        app.is_saved(&uri).unwrap_or(false);
                                                    let label = if following {
                                                        pgettext(app.locale, "artist", "Following")
                                                    } else {
                                                        pgettext(app.locale, "artist", "Follow")
                                                    };
                                                    if theme::pill_button(
                                                        ui, &palette, &label, false,
                                                    )
                                                    .clicked()
                                                    {
                                                        app.actions.push(Action::ToggleSaved(uri));
                                                    }
                                                }
                                                ui.with_layout(
                                                    egui::Layout::left_to_right(
                                                        egui::Align::Center,
                                                    ),
                                                    |ui| {
                                                        if theme::link(
                                                            ui,
                                                            &artist_ref.name,
                                                            theme::bold(15.0),
                                                            palette.text,
                                                        )
                                                        .clicked()
                                                            && let Some(id) = &artist_ref.id
                                                        {
                                                            app.actions.push(Action::Open(
                                                                Page::Artist(id.clone()),
                                                            ));
                                                        }
                                                    },
                                                );
                                            },
                                        );
                                    });
                                });
                                ui.add_space(12.0);
                            }
                            if !now.is_episode {
                                card(ui, &palette, |ui| {
                                    ui.horizontal(|ui| {
                                        theme::text(
                                            ui,
                                            gettext(app.locale, "Credits"),
                                            theme::bold(14.0),
                                            palette.text,
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if theme::link(
                                                    ui,
                                                    gettext(app.locale, "Show all"),
                                                    theme::bold(13.0),
                                                    palette.secondary,
                                                )
                                                .clicked()
                                                {
                                                    app.actions.push(Action::ShowDialog(
                                                        crate::model::Dialog::TrackCredits {
                                                            uri: now.uri.clone(),
                                                            name: now.title.clone(),
                                                        },
                                                    ));
                                                }
                                            },
                                        );
                                    });
                                    ui.add_space(16.0);
                                    let details =
                                        app.details.get(&now.uri).and_then(Loadable::get).cloned();
                                    if let Some(details) = details {
                                        let mut shown = std::collections::HashSet::new();
                                        for credit in &details.credits {
                                            if !shown.insert((&credit.name, &credit.uri)) {
                                                continue;
                                            }
                                            let roles = details
                                                .credits
                                                .iter()
                                                .filter(|other| {
                                                    other.name == credit.name
                                                        && other.uri == credit.uri
                                                })
                                                .map(|other| {
                                                    crate::details::role_label(
                                                        other.role, app.locale,
                                                    )
                                                    .into_owned()
                                                })
                                                .collect::<Vec<_>>()
                                                .join(" · ");
                                            super::details::credit_row(
                                                app,
                                                ui,
                                                &credit.name,
                                                &roles,
                                                credit.uri.as_deref(),
                                            );
                                            ui.add_space(8.0);
                                        }
                                    } else if let Some(Loadable::Failed(error)) =
                                        app.details.get(&now.uri)
                                    {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(error).color(palette.secondary),
                                            )
                                            .wrap(),
                                        );
                                    } else {
                                        widgets::loading_row(ui, &palette, app.locale);
                                    }
                                });
                                ui.add_space(12.0);
                            }
                            card(ui, &palette, |ui| {
                                ui.horizontal(|ui| {
                                    theme::text(
                                        ui,
                                        gettext(app.locale, "Next up"),
                                        theme::bold(14.0),
                                        palette.text,
                                    );
                                    if theme::link(
                                        ui,
                                        gettext(app.locale, "Queue"),
                                        theme::regular(12.0),
                                        palette.secondary,
                                    )
                                    .clicked()
                                    {
                                        app.actions.push(Action::ToggleQueuePanel);
                                    }
                                    if theme::icon_button(
                                        ui,
                                        Icon::Refresh,
                                        14.0,
                                        palette.secondary,
                                        palette.text,
                                        &gettext(app.locale, "Refresh"),
                                    )
                                    .clicked()
                                    {
                                        app.actions.push(Action::RefreshQueue);
                                    }
                                });
                                ui.add_space(8.0);
                                match &app.queue {
                                    Loadable::Loaded(queue) => {
                                        if let Some(next) = queue.queue.first() {
                                            ui.horizontal(|ui| {
                                                let (rect, _) = ui.allocate_exact_size(
                                                    egui::Vec2::splat(40.0),
                                                    egui::Sense::hover(),
                                                );
                                                widgets::paint_cover(
                                                    ui,
                                                    &palette,
                                                    next.image(96),
                                                    rect,
                                                    4.0,
                                                    Icon::Music,
                                                    Some(app.backend.art()),
                                                );
                                                ui.vertical(|ui| {
                                                    ui.add(
                                                        egui::Label::new(
                                                            egui::RichText::new(next.name())
                                                                .color(palette.text),
                                                        )
                                                        .wrap(),
                                                    );
                                                    ui.add(
                                                        egui::Label::new(
                                                            egui::RichText::new(next.subtitle())
                                                                .color(palette.secondary),
                                                        )
                                                        .wrap(),
                                                    );
                                                });
                                            });
                                        } else {
                                            theme::text(
                                                ui,
                                                gettext(app.locale, "Nothing queued"),
                                                theme::regular(13.0),
                                                palette.secondary,
                                            );
                                        }
                                    }
                                    Loadable::Loading | Loadable::NotLoaded => {
                                        widgets::loading_row(ui, &palette, app.locale)
                                    }
                                    Loadable::Failed(error) => {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(error).color(palette.secondary),
                                            )
                                            .wrap(),
                                        );
                                    }
                                }
                            });
                        });
                });
        });
    super::finish_panel_resize(
        app,
        ui,
        panel_style,
        "now-playing-panel",
        true,
        response.response.rect,
    );
    let width = response.response.rect.width();
    if (width - app.settings.lyrics_width).abs() > 1.0
        && super::panel_width_chosen(ui.ctx(), "now-playing-panel", &fit)
    {
        app.settings.lyrics_width = width;
        app.actions.push(Action::SettingsChanged);
    }
}
