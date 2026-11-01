//! Soloist is the first player offered after sign-in, with a built-in fallback.
use crate::{app::App, i18n::gettext, model::Action, soloist::Status, theme};

/// A native playback icon, not a Spotify or Soloist brand mark.
fn badge(ui: &mut egui::Ui, palette: &theme::Palette, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size / 2.0, palette.surface_active);
    theme::paint_icon(ui, theme::Icon::Headphones, rect, size * 0.52, palette.text);
}

pub fn onboarding(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let welcome = app.soloist_onboarding == crate::settings::SoloistOnboarding::Welcome;
    let setup = !welcome
        && matches!(
            app.soloist_status,
            Status::Off | Status::NeedsKey | Status::Failed(_)
        );
    let stage = if welcome {
        "welcome"
    } else {
        match app.soloist_status {
            Status::Off | Status::NeedsKey | Status::Failed(_) => "setup",
            Status::Starting | Status::Downloading { .. } => "download",
            Status::Pairing { .. } => "pairing",
            Status::Ready { .. } if app.soloist_device_id.is_none() => "pairing",
            Status::Ready { .. } => "ready",
        }
    };
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(palette.window))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            fastframe_titlebar::drag(
                ui,
                egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2(
                        rect.width(),
                        theme::TOP_BAR_HEIGHT + theme::titlebar_inset(ui.ctx()),
                    ),
                ),
            );
            super::widgets::paint_vertical_gradient(
                ui,
                rect,
                super::blend(palette.window, palette.accent, 0.08),
                palette.window,
            );
            // Centre the content actually drawn, including wrapped translations.
            // Each stage keeps its own measurement so downloading and pairing
            // do not inherit the much taller key-entry card.
            let height_id = egui::Id::new((
                "soloist-onboarding-height",
                stage,
                rect.width().to_bits(),
                locale.tag(),
            ));
            let height = ui
                .data(|data| data.get_temp::<f32>(height_id))
                .unwrap_or(360.0);
            egui::ScrollArea::vertical()
                .id_salt("soloist-onboarding")
                .show(ui, |ui| {
                    ui.add_space(((rect.height() - height) / 2.0).max(48.0));
                    let content = ui.vertical_centered(|ui| {
                        ui.set_max_width(540.0_f32.min(rect.width() - 48.0));
                        egui::Frame::new()
                            .fill(palette.panel)
                            .stroke(egui::Stroke::new(1.0, palette.outline))
                            .corner_radius(20)
                            .inner_margin(28)
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                    onboarding_content(app, ui, welcome);
                                });
                            });
                        ui.add_space(16.0);
                        if theme::link(
                            ui,
                            gettext(locale, "Use the built-in player"),
                            theme::regular(13.0),
                            palette.secondary,
                        )
                        .clicked()
                        {
                            app.actions.push(Action::UseBuiltInPlayer);
                        }
                        if setup {
                            ui.add_space(6.0);
                            if theme::link(
                                ui,
                                gettext(locale, "Setup, privacy and limitations"),
                                theme::regular(12.0),
                                palette.secondary,
                            )
                            .clicked()
                            {
                                app.actions.push(Action::OpenUrl(
                                    "https://spotifast.rocks/soloist/".into(),
                                ));
                            }
                        }
                    });
                    let measured = content.response.rect.height();
                    if (measured - height).abs() > 0.5 {
                        ui.data_mut(|data| data.insert_temp(height_id, measured));
                        ui.ctx().request_discard("centre the Soloist setup stage");
                    }
                    ui.add_space(32.0);
                });
        });
}

fn wrapped(ui: &mut egui::Ui, label: impl Into<String>, font: egui::FontId, color: egui::Color32) {
    let label = label.into();
    let galley = crate::bidi::layout(
        ui.painter(),
        &label,
        font,
        color,
        ui.available_width(),
        usize::MAX,
        None,
    );
    ui.add(egui::Label::new(galley));
}

fn heading(ui: &mut egui::Ui, palette: &theme::Palette, label: impl Into<String>) {
    wrapped(ui, label, theme::semibold(25.0), palette.text);
}

/// A name the listener needs to match exactly in Spotify, never ellipsized.
fn device_name(ui: &mut egui::Ui, palette: &theme::Palette, name: &str, connected: bool) {
    egui::Frame::new()
        .fill(palette.surface)
        .corner_radius(10)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                theme::paint_icon(
                    ui,
                    if connected {
                        theme::Icon::CircleCheck
                    } else {
                        theme::Icon::Headphones
                    },
                    rect,
                    24.0,
                    palette.secondary,
                );
                wrapped(ui, name, theme::semibold(18.0), palette.text);
            });
        });
}

fn onboarding_content(app: &mut App, ui: &mut egui::Ui, welcome: bool) {
    let locale = app.locale;
    let palette = app.palette;
    if welcome
        || matches!(
            app.soloist_status,
            Status::Off | Status::NeedsKey | Status::Failed(_)
        )
    {
        badge(ui, &palette, 56.0);
        ui.add_space(16.0);
        heading(ui, &palette, gettext(locale, "Listen with Spotify Soloist"));
        ui.add_space(8.0);
        wrapped(
            ui,
            gettext(
                locale,
                "Spotify's official player for listening outside the Spotify app.",
            ),
            theme::regular(14.0),
            palette.secondary,
        );
        ui.add_space(16.0);
        if welcome {
            ui.label(gettext(
                locale,
                "Requires Spotify Premium. Spotifast handles downloads and updates.",
            ));
            ui.add_space(16.0);
            if theme::pill_button(ui, &palette, &gettext(locale, "Set up Soloist"), true).clicked()
            {
                app.actions.push(Action::StartSoloistSetup);
            }
        } else {
            controls(app, ui, true);
        }
        return;
    }
    match &app.soloist_status {
        Status::Starting => {
            heading(ui, &palette, gettext(locale, "Starting Soloist…"));
            ui.add_space(16.0);
            ui.add(egui::Spinner::new().size(18.0).color(palette.secondary));
        }
        Status::Downloading { received, total } => {
            heading(ui, &palette, gettext(locale, "Downloading Soloist…"));
            ui.add_space(20.0);
            download_progress(ui, *received, *total);
        }
        Status::Ready { name, .. } if app.soloist_device_id.is_some() => {
            heading(ui, &palette, gettext(locale, "Ready to listen"));
            ui.add_space(20.0);
            device_name(ui, &palette, name, true);
            ui.add_space(20.0);
            if theme::pill_button(ui, &palette, &gettext(locale, "Start listening"), true).clicked()
            {
                app.actions.push(Action::FinishSoloistSetup);
            }
        }
        Status::Pairing { name, .. } | Status::Ready { name, .. } => {
            heading(ui, &palette, gettext(locale, "Connect in Spotify"));
            ui.add_space(12.0);
            wrapped(
                ui,
                gettext(
                    locale,
                    "Open Spotify with the same account and network.\nIn its device picker, choose:",
                ),
                theme::regular(14.0),
                palette.secondary,
            );
            ui.add_space(20.0);
            device_name(ui, &palette, name, false);
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0).color(palette.secondary));
                theme::subtle(
                    ui,
                    &palette,
                    &gettext(
                        locale,
                        if matches!(app.soloist_status, Status::Ready { .. }) {
                            "Verifying Soloist…"
                        } else {
                            "Waiting for Spotify…"
                        },
                    ),
                );
            });
        }
        _ => {}
    }
}

fn download_progress(ui: &mut egui::Ui, received: u64, total: u64) {
    let progress = if total == 0 {
        0.0
    } else {
        received as f32 / total as f32
    };
    let bar = egui::ProgressBar::new(progress).animate(false);
    ui.add(if total == 0 {
        bar
    } else {
        bar.show_percentage()
    });
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let locale = app.locale;
    let palette = app.palette;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        badge(ui, &palette, 48.0);
        ui.vertical(|ui| {
            if matches!(app.soloist_status, Status::Ready { .. }) && app.soloist_device_id.is_some()
            {
                ui.label(egui::RichText::new(gettext(locale, "Enabled on this computer")).strong());
                theme::subtle(ui, &palette, &gettext(locale, "Updates automatically."));
            } else {
                ui.label(gettext(
                    locale,
                    "Spotify's player, with lossless support. Requires Premium.",
                ));
            }
        });
        if app.settings.soloist_enabled {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::pill_button(ui, &palette, &gettext(locale, "Turn off Soloist"), false)
                    .clicked()
                {
                    app.actions.push(Action::SetSoloist(false));
                }
            });
        }
    });
    ui.add_space(10.0);
    controls(app, ui, false);
}

fn controls(app: &mut App, ui: &mut egui::Ui, onboarding: bool) {
    let locale = app.locale;
    let palette = app.palette;
    if !crate::soloist::install::supported() {
        ui.label(gettext(locale, "Spotify has not published Soloist for this platform. The built-in player remains available."));
        return;
    }
    if !app.is_connected() {
        ui.label(gettext(
            locale,
            "Sign in to Spotify before setting up Soloist.",
        ));
        return;
    }
    match &app.soloist_status {
        Status::Off | Status::NeedsKey | Status::Failed(_) => {
            if let Status::Failed(error) = &app.soloist_status {
                ui.label(error);
                ui.add_space(8.0);
            }
            if theme::soft_button(
                ui,
                &palette,
                None,
                &gettext(locale, "1. Get your Soloist key from Spotify"),
                false,
            )
            .clicked()
            {
                app.actions.push(Action::OpenUrl(
                    "https://developer.spotify.com/dashboard/soloist".into(),
                ));
            }
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut app.soloist_key)
                    .password(true)
                    .desired_width(ui.available_width())
                    .hint_text(gettext(locale, "Paste your personal Soloist key")),
            );
            ui.add_space(8.0);
            let retry = app.settings.soloist_enabled
                && app.soloist_key.is_empty()
                && !matches!(app.soloist_status, Status::NeedsKey);
            let label = if retry {
                gettext(locale, "Retry with saved key")
            } else {
                gettext(locale, "2. Download and enable Soloist")
            };
            ui.add_enabled_ui(retry || !app.soloist_key.trim().is_empty(), |ui| {
                if theme::pill_button(ui, &palette, &label, true).clicked() {
                    app.actions.push(if retry {
                        Action::RetrySoloist
                    } else {
                        Action::SetSoloist(true)
                    });
                }
            });
            if !onboarding
                && matches!(app.soloist_status, Status::Off)
                && app.soloist_key.trim().is_empty()
                && theme::soft_button(
                    ui,
                    &palette,
                    None,
                    &gettext(locale, "Enable with saved key"),
                    false,
                )
                .clicked()
            {
                app.actions.push(Action::RetrySoloist);
            }
        }
        Status::Starting => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(gettext(locale, "Starting Soloist…"));
            });
        }
        Status::Downloading { received, total } => {
            ui.label(gettext(locale, "Downloading Soloist from Spotify…"));
            download_progress(ui, *received, *total);
        }
        Status::Pairing { name, .. } => {
            ui.label(egui::RichText::new(gettext(locale, "3. Pair once in Spotify")).strong());
            ui.label(gettext(
                locale,
                "Open Spotify with the same account and network. In its device picker, choose:",
            ));
            ui.label(egui::RichText::new(name).strong());
        }
        Status::Ready { .. } => {
            if app.soloist_device_id.is_some() {
                if onboarding {
                    ui.label(
                        egui::RichText::new(gettext(locale, "Enabled on this computer")).strong(),
                    );
                    theme::subtle(ui, &palette, &gettext(locale, "Updates automatically."));
                }
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(gettext(locale, "Verifying Soloist…"));
                });
                ui.label(gettext(
                    locale,
                    "Open Spotify with the same account and network. In its device picker, choose:",
                ));
                if let Status::Ready { name, .. } = &app.soloist_status {
                    ui.label(name);
                }
            }
            if app.soloist_update.is_some() {
                theme::subtle(
                    ui,
                    &palette,
                    &gettext(
                        locale,
                        "Update ready. It will install when Soloist is inactive.",
                    ),
                );
            }
        }
    }
    if onboarding {
        return;
    }
    ui.add_space(8.0);
    if let Status::Ready {
        version, expires, ..
    } = &app.soloist_status
    {
        let date = jiff::Timestamp::from_second((*expires).min(i64::MAX as u64) as i64)
            .ok()
            .map(|at| at.to_zoned(jiff::tz::TimeZone::system()).date().to_string())
            .unwrap_or_default();
        theme::subtle(
            ui,
            &palette,
            &gettext(locale, "Version {version} · expires {date}")
                .replace("{version}", version)
                .replace("{date}", &date),
        );
    }
    if matches!(app.soloist_status, Status::Ready { .. }) {
        theme::subtle(
            ui,
            &palette,
            &gettext(locale, "Spotify manages audio quality."),
        );
    }
    if ui
        .link(gettext(locale, "Setup, privacy and limitations"))
        .clicked()
    {
        app.actions
            .push(Action::OpenUrl("https://spotifast.rocks/soloist/".into()));
    }
}
