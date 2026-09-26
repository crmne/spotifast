//! The Settings page.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Stroke, Vec2};

use crate::api::models::pick_image;
use crate::app::App;
use crate::i18n::{gettext, ngettext, pgettext};
use crate::model::{Action, Dialog};
use crate::settings::{LanguageChoice, ProxyMode, ThemeChoice};
use crate::theme::{self, Icon, Palette};

use super::widgets;

const PLAYBACK_DIRTY_ID: &str = "playback-settings-dirty";
pub(crate) const PERSONAL_APP_FOCUS_ID: &str = "focus-personal-app-setup";
const SETTINGS_FILTER_ID: &str = "settings-filter";

/// Whether a settings row matches the filter query (case-insensitive).
/// An empty query matches everything, so the page reads exactly as
/// before when the search field is empty.
pub(crate) fn row_matches(needle: &str, title: &str, description: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    title.to_lowercase().contains(&needle) || description.to_lowercase().contains(&needle)
}

/// Text shared by section selection and the rendered row. Conditional rows
/// participate only on the platforms and account states where they exist.
struct RowText<'a> {
    title: std::borrow::Cow<'a, str>,
    description: std::borrow::Cow<'a, str>,
    available: bool,
}

impl<'a> RowText<'a> {
    fn new(
        title: impl Into<std::borrow::Cow<'a, str>>,
        description: impl Into<std::borrow::Cow<'a, str>>,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            available: true,
        }
    }

    fn when(mut self, available: bool) -> Self {
        self.available = available;
        self
    }

    fn matches(&self, needle: &str, section: &str) -> bool {
        self.available
            && (needle.is_empty()
                || section.to_lowercase().contains(needle)
                || row_matches(needle, &self.title, &self.description))
    }
}

fn section_matches(needle: &str, title: &str, rows: &[RowText<'_>]) -> bool {
    let needle = needle.trim().to_lowercase();
    rows.iter().any(|row| row.matches(&needle, title))
}

fn filtered_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    needle: &str,
    section: &str,
    row: &RowText<'_>,
    control: impl FnOnce(&mut egui::Ui),
) {
    if row.matches(needle, section) {
        widgets::setting_row(ui, palette, &row.title, &row.description, control);
    }
}

/// `filtered_row` for a control that needs `control_width` points.
fn filtered_row_sized(
    ui: &mut egui::Ui,
    palette: &Palette,
    needle: &str,
    section: &str,
    row: &RowText<'_>,
    control_width: f32,
    control: impl FnOnce(&mut egui::Ui),
) {
    if row.matches(needle, section) {
        widgets::setting_row_sized(
            ui,
            palette,
            &row.title,
            &row.description,
            control_width,
            control,
        );
    }
}

/// Forget the search text, so a flow that lands on a specific row (like
/// the Personal App setup) always finds that row visible and focusable.
pub(crate) fn clear_search(ctx: &egui::Context) {
    ctx.data_mut(|data| data.remove::<String>(egui::Id::new(SETTINGS_FILTER_ID)));
}
const PROXY_DIRTY_ID: &str = "proxy-settings-dirty";

fn section(
    ui: &mut egui::Ui,
    palette: &Palette,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(10.0);
    theme::text(ui, title, theme::bold(18.0), palette.text);
    ui.add_space(8.0);
    Frame::new()
        .fill(
            palette
                .surface
                .gamma_multiply(if palette.dark { 0.7 } else { 1.0 }),
        )
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(CornerRadius::same(theme::RADIUS + 2))
        .inner_margin(Margin::symmetric(20, 16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width().min(760.0));
            add_contents(ui);
        });
    ui.add_space(8.0);
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    ui.add_space(8.0);
    theme::text(
        ui,
        gettext(locale, "Settings"),
        theme::bold(28.0),
        palette.text,
    );
    ui.add_space(4.0);
    let mut filter = ui
        .data(|data| data.get_temp::<String>(egui::Id::new(SETTINGS_FILTER_ID)))
        .unwrap_or_default();
    widgets::search_field(
        ui,
        &palette,
        app.locale,
        egui::Id::new("settings-search-field"),
        &mut filter,
        &gettext(locale, "Search settings"),
        ui.available_width().min(400.0),
    );
    let needle = filter.trim().to_lowercase();
    ui.data_mut(|data| data.insert_temp(egui::Id::new(SETTINGS_FILTER_ID), filter));
    ui.add_space(4.0);
    let dirty_id = egui::Id::new(PLAYBACK_DIRTY_ID);
    let mut playback_dirty = ui
        .data(|data| data.get_temp::<bool>(dirty_id))
        .unwrap_or(false);
    let proxy_dirty_id = egui::Id::new(PROXY_DIRTY_ID);
    let mut proxy_dirty = ui
        .data(|data| data.get_temp::<bool>(proxy_dirty_id))
        .unwrap_or(false);
    let mut changed = false;
    let mut any_visible = false;
    let open_folder = gettext(locale, "Open folder");

    let wanted = app
        .settings
        .web_client_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let in_use = wanted
        .as_deref()
        .is_some_and(|wanted| app.web_app.as_deref() == Some(wanted));
    let account = gettext(locale, "Account");
    let sign_out = gettext(locale, "Sign out");
    let account_rows = [
        RowText::new(
            gettext(locale, "Personal Spotify app"),
            gettext(
                locale,
                "Use a personal Development Mode app for a separate API quota. The shared app stays active.",
            ),
        ),
        RowText::new(
            gettext(locale, "Create an app"),
            gettext(
                locale,
                "Create one for free in Spotify's developer dashboard.",
            ),
        )
        .when(!in_use),
        RowText::new(
            gettext(locale, "Personal app ready"),
            gettext(
                locale,
                "Supported requests use your app. Other requests use the shared app.",
            ),
        )
        .when(in_use),
        RowText::new(
            gettext(locale, "Authorize your personal app"),
            gettext(
                locale,
                "Spotify opens in your browser to verify the account.",
            ),
        )
        .when(!in_use && wanted.is_some()),
        RowText::new(
            gettext(locale, "Remove personal app"),
            gettext(locale, "Shared access remains signed in."),
        )
        .when(!in_use && wanted.is_none() && app.web_app.is_some()),
        RowText::new(sign_out.clone(), account.clone()),
    ];
    if section_matches(&needle, &account, &account_rows) {
        any_visible = true;
        section(ui, &palette, &account, |ui| {
            if row_matches(&needle, &format!("{sign_out} {account}"), "") {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    let avatar = app
                        .user
                        .as_ref()
                        .and_then(|user| pick_image(&user.images, 64).map(str::to_string));
                    widgets::cover(ui, &palette, avatar.as_deref(), 56.0, 28.0, Icon::User);
                    ui.vertical(|ui| {
                        let name = app
                            .user
                            .as_ref()
                            .map(|user| user.name().to_string())
                            .unwrap_or_default();
                        theme::text(ui, name, theme::semibold(16.0), palette.text);
                        let product = app
                            .user
                            .as_ref()
                            .and_then(|user| user.product.clone())
                            .map(|product| match product.as_str() {
                                "premium" => "Spotify Premium".to_string(),
                                "free" | "open" => {
                                    gettext(locale, "Spotify Free, local playback needs Premium")
                                        .into_owned()
                                }
                                other => other.to_string(),
                            })
                            .unwrap_or_default();
                        theme::text(ui, product, theme::regular(13.0), palette.secondary);
                        if let Some(username) =
                            app.local.connected.then(|| app.local.username.clone())
                            && !username.is_empty()
                        {
                            theme::text(
                                ui,
                                // Translators: {username} is the Spotify account's user name.
                                gettext(locale, "Connected as {username}")
                                    .replace("{username}", &username),
                                theme::regular(12.0),
                                palette.dim,
                            );
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::pill_button(ui, &palette, &sign_out, false).clicked() {
                            app.actions.push(Action::SignOut);
                        }
                    });
                });
                ui.add_space(10.0);
            }
            let mut client_id = app.settings.web_client_id.clone().unwrap_or_default();
            filtered_row(ui, &palette, &needle, &account, &account_rows[0], |ui| {
                let response = Frame::new()
                    .fill(palette.surface)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        widgets::text_edit(
                            ui,
                            app.locale,
                            egui::TextEdit::singleline(&mut client_id)
                                .id(egui::Id::new("personal-web-client-id"))
                                .hint_text(
                                    egui::RichText::new(gettext(locale, "Client ID"))
                                        .color(palette.dim),
                                )
                                .font(theme::regular(13.0))
                                .frame(egui::Frame::NONE)
                                .desired_width(200.0),
                        )
                    })
                    .inner;
                if ui
                    .data_mut(|data| data.remove_temp::<bool>(egui::Id::new(PERSONAL_APP_FOCUS_ID)))
                    .unwrap_or(false)
                {
                    response.scroll_to_me(Some(Align::Center));
                    response.request_focus();
                }
                if response.changed() {
                    let trimmed = client_id.trim().to_string();
                    app.settings.web_client_id = (!trimmed.is_empty()).then_some(trimmed);
                    changed = true;
                }
            });
            filtered_row(ui, &palette, &needle, &account, &account_rows[1], |ui| {
                if theme::pill_button(ui, &palette, &gettext(locale, "Setup guide"), false)
                    .clicked()
                {
                    app.actions.push(Action::OpenUrl(
                        "https://spotifast.rocks/make-it-even-faster/#make-a-spotify-app".into(),
                    ));
                }
            });
            if in_use {
                filtered_row(ui, &palette, &needle, &account, &account_rows[2], |ui| {
                    if theme::pill_button(ui, &palette, &gettext(locale, "Remove"), false).clicked()
                    {
                        app.settings.web_client_id = None;
                        app.actions.push(Action::ConfigurePersonalWebApp);
                    }
                });
            } else if wanted.is_some() {
                filtered_row(ui, &palette, &needle, &account, &account_rows[3], |ui| {
                    if theme::pill_button(ui, &palette, &gettext(locale, "Authorize"), true)
                        .clicked()
                    {
                        app.actions.push(Action::ConfigurePersonalWebApp);
                    }
                });
            } else if app.web_app.is_some() {
                filtered_row(ui, &palette, &needle, &account, &account_rows[4], |ui| {
                    if theme::pill_button(ui, &palette, &gettext(locale, "Remove"), false).clicked()
                    {
                        app.actions.push(Action::ConfigurePersonalWebApp);
                    }
                });
            }
        });
    }

    let (status, detail, action) = match &app.local_playback {
        crate::backend::LocalPlayback::Ready { .. } => (
            pgettext(locale, "playback status", "Ready"),
            gettext(locale, "This computer is a Spotify Connect device."),
            None,
        ),
        crate::backend::LocalPlayback::Authorizing => (
            pgettext(locale, "playback status", "Setting up"),
            gettext(locale, "Finish authorizing in your browser."),
            None,
        ),
        crate::backend::LocalPlayback::Connecting => (
            pgettext(locale, "playback status", "Connecting"),
            gettext(locale, "Connecting to Spotify…"),
            None,
        ),
        crate::backend::LocalPlayback::Failed(message) => (
            pgettext(locale, "playback status", "Unavailable"),
            message.clone().into(),
            Some(gettext(locale, "Try again")),
        ),
        crate::backend::LocalPlayback::Unavailable => (
            pgettext(locale, "playback status", "Not set up"),
            gettext(
                locale,
                "Requires Spotify Premium and a one-time browser sign-in.",
            ),
            Some(gettext(locale, "Enable playback here")),
        ),
    };
    let playback = gettext(locale, "Playback on this computer");
    let normalize_volume = gettext(locale, "Normalize volume");
    let autoplay = gettext(locale, "Autoplay");
    let gapless = gettext(locale, "Gapless playback");
    let keep_playing = gettext(locale, "Keep music playing when the window closes");
    let update_checks = gettext(locale, "Automatic update checks");
    let audio_cache = gettext(locale, "Audio cache");
    let apply_playback = gettext(locale, "Apply and restart playback");
    let apply_playback_note = gettext(locale, "Restart local playback to apply these settings.");
    let download_updates = gettext(locale, "Download updates automatically");
    let playback_rows = [
        RowText::new(
            // Translators: {status} is a playback state such as Ready or Not set up.
            gettext(locale, "Status: {status}").replace("{status}", &status),
            detail,
        ),
        RowText::new(
            gettext(locale, "Device name"),
            gettext(locale, "How this computer appears in Spotify Connect."),
        ),
        RowText::new(
            gettext(locale, "Audio quality"),
            gettext(locale, "Higher bitrates use more data and cache space."),
        ),
        RowText::new(
            normalize_volume.clone(),
            gettext(locale, "Keep loud and quiet tracks at a similar level."),
        ),
        RowText::new(
            autoplay.clone(),
            gettext(locale, "Keep playing similar songs when your music ends."),
        ),
        RowText::new(
            gapless.clone(),
            gettext(locale, "Play tracks without silence between them."),
        ),
        RowText::new(
            keep_playing.clone(),
            super::keys::platform_shortcut(
                &gettext(
                    locale,
                    "Spotifast hides to the system tray. Quit from the tray menu or with Ctrl+Q.",
                ),
                &gettext(
                    locale,
                    "Spotifast hides to the system tray. Quit from the tray menu or with Cmd+Q.",
                ),
            )
            .to_owned(),
        ),
        RowText::new(
            update_checks.clone(),
            gettext(locale, "Checks GitHub once a day. No personal data is sent."),
        ),
        RowText::new(
            gettext(locale, "Audio output"),
            gettext(
                locale,
                "PulseAudio also covers PipeWire. Rodio talks to ALSA directly.",
            ),
        )
        .when(cfg!(target_os = "linux")),
        RowText::new(
            gettext(locale, "Output buffer"),
            gettext(
                locale,
                "More buffering can prevent clicks on busy computers. Less buffering makes controls respond sooner.",
            ),
        )
        .when(cfg!(windows)),
        RowText::new(
            audio_cache.clone(),
            gettext(locale, "Save downloaded audio for later playback."),
        ),
        RowText::new(apply_playback.clone(), apply_playback_note.clone()).when(playback_dirty),
        RowText::new(gettext(locale, "Playback settings applied"), "").when(!playback_dirty),
        RowText::new(
            download_updates.clone(),
            gettext(
                locale,
                "Downloads in the background. You choose when to restart.",
            ),
        ),
    ];
    if section_matches(&needle, &playback, &playback_rows) {
        any_visible = true;
        section(ui, &palette, &playback, |ui| {
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[0], |ui| {
                if let Some(label) = action {
                    if theme::pill_button(ui, &palette, &label, true).clicked() {
                        app.actions.push(Action::EnablePlayback);
                    }
                } else if app.local_ready
                    && theme::soft_button(
                        ui,
                        &palette,
                        Some(Icon::Refresh),
                        &gettext(locale, "Reconnect"),
                        false,
                    )
                    .clicked()
                {
                    app.actions.push(Action::RestartEngine);
                }
            });
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[1], |ui| {
                let response = Frame::new()
                    .fill(palette.surface)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        widgets::text_edit(
                            ui,
                            locale,
                            egui::TextEdit::singleline(&mut app.settings.device_name)
                                .font(theme::regular(14.0))
                                .frame(egui::Frame::NONE)
                                .desired_width(200.0),
                        )
                    })
                    .inner;
                if response.changed() {
                    changed = true;
                    playback_dirty = true;
                }
            });
            // Normal to Very high, left to right when side by side and top
            // to bottom in a column.
            let choices = [
                (96u16, gettext(locale, "Normal · 96 kbps")),
                (160, gettext(locale, "High · 160 kbps")),
                (320, gettext(locale, "Very high · 320 kbps")),
            ];
            let choice_gap = 6.0;
            let choices_width = choices
                .iter()
                .map(|(_, label)| theme::soft_button_width(ui, label))
                .sum::<f32>()
                + choice_gap * (choices.len() - 1) as f32;
            filtered_row_sized(
                ui,
                &palette,
                &needle,
                &playback,
                &playback_rows[2],
                choices_width,
                |ui| {
                    let mut choose = |ui: &mut egui::Ui, kbps: u16, label: &str| {
                        if theme::soft_button(
                            ui,
                            &palette,
                            None,
                            label,
                            app.settings.bitrate == kbps,
                        )
                        .clicked()
                            && app.settings.bitrate != kbps
                        {
                            app.settings.bitrate = kbps;
                            changed = true;
                            playback_dirty = true;
                        }
                    };
                    if ui.available_width() >= choices_width {
                        // Laid right to left, so the last choice goes first.
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = choice_gap;
                            for (kbps, label) in choices.iter().rev() {
                                choose(ui, *kbps, label);
                            }
                        });
                    } else {
                        // Too narrow even for a line of their own: a column.
                        ui.with_layout(Layout::top_down(Align::Max), |ui| {
                            ui.spacing_mut().item_spacing.y = choice_gap;
                            for (kbps, label) in &choices {
                                choose(ui, *kbps, label);
                            }
                        });
                    }
                },
            );
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[3], |ui| {
                if widgets::switch(
                    ui,
                    &palette,
                    &normalize_volume,
                    &mut app.settings.normalisation,
                )
                .changed()
                {
                    changed = true;
                    playback_dirty = true;
                }
            });
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[4], |ui| {
                if widgets::switch(ui, &palette, &autoplay, &mut app.settings.autoplay).changed() {
                    changed = true;
                    playback_dirty = true;
                }
            });
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[5], |ui| {
                if widgets::switch(ui, &palette, &gapless, &mut app.settings.gapless).changed() {
                    changed = true;
                    playback_dirty = true;
                }
            });
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[6], |ui| {
                if widgets::switch(
                    ui,
                    &palette,
                    &keep_playing,
                    &mut app.settings.keep_playing_in_background,
                )
                .changed()
                {
                    changed = true;
                }
            });
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[7], |ui| {
                if widgets::switch(
                    ui,
                    &palette,
                    &update_checks,
                    &mut app.settings.check_for_updates,
                )
                .changed()
                {
                    changed = true;
                }
            });
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[13], |ui| {
                if widgets::switch(
                    ui,
                    &palette,
                    &download_updates,
                    &mut app.settings.download_updates_automatically,
                )
                .changed()
                {
                    changed = true;
                }
            });
            if cfg!(target_os = "linux") {
                filtered_row(ui, &palette, &needle, &playback, &playback_rows[8], |ui| {
                    let current = app
                        .settings
                        .platform_backend()
                        .unwrap_or_else(|| "rodio".into());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        for backend in ["rodio", "pulseaudio"] {
                            let label = if backend == "pulseaudio" {
                                "PulseAudio / PipeWire"
                            } else {
                                "ALSA (rodio)"
                            };
                            if theme::soft_button(ui, &palette, None, label, current == backend)
                                .clicked()
                                && current != backend
                            {
                                app.settings.audio_backend = Some(backend.to_string());
                                changed = true;
                                playback_dirty = true;
                            }
                        }
                    });
                });
            }
            #[cfg(windows)]
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[9], |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let current = app.settings.audio_buffer_ms;
                    for ms in [50u32, 100, 200] {
                        let label = format!("{ms} ms");
                        if theme::soft_button(ui, &palette, None, &label, current == ms).clicked()
                            && current != ms
                        {
                            app.settings.audio_buffer_ms = ms;
                            changed = true;
                            playback_dirty = true;
                        }
                    }
                });
            });
            filtered_row(ui, &palette, &needle, &playback, &playback_rows[10], |ui| {
                // The control area lays out right-to-left: add the rightmost item first.
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if widgets::switch(ui, &palette, &audio_cache, &mut app.settings.audio_cache)
                        .changed()
                    {
                        changed = true;
                        playback_dirty = true;
                    }
                    if app.settings.audio_cache {
                        ui.add_space(6.0);
                        for (mb, label) in [(4096u64, "4 GB"), (1024, "1 GB"), (512, "512 MB")] {
                            if theme::soft_button(
                                ui,
                                &palette,
                                None,
                                label,
                                app.settings.audio_cache_mb == mb,
                            )
                            .clicked()
                                && app.settings.audio_cache_mb != mb
                            {
                                app.settings.audio_cache_mb = mb;
                                changed = true;
                                playback_dirty = true;
                            }
                        }
                    }
                });
            });
            ui.add_space(4.0);
            if playback_dirty
                || playback_rows[11].matches(&needle, &playback)
                || playback_rows[12].matches(&needle, &playback)
            {
                ui.horizontal(|ui| {
                    if playback_dirty {
                        if theme::pill_button(ui, &palette, &apply_playback, true).clicked() {
                            app.actions.push(Action::RestartEngine);
                            playback_dirty = false;
                        }
                        theme::subtle(ui, &palette, &apply_playback_note);
                    } else {
                        theme::subtle(ui, &palette, &gettext(locale, "Playback settings applied."));
                    }
                });
            }
        });
    }

    let appearance = gettext(locale, "Appearance");
    let theme_title = gettext(locale, "Theme");
    let accent_from_art = gettext(locale, "Colour from album art");
    let sidebar_compact = gettext(locale, "Compact library sidebar");
    let tracklist_compact = gettext(locale, "Compact track list");
    let middle_click = gettext(locale, "Middle-click autoscroll");
    let custom_titlebar = gettext(locale, "Custom title bar");
    let player_bar_vis = gettext(locale, "Player bar visualizer");
    let appearance_rows = [
        RowText::new(theme_title.clone(), {
            let detail = theme::catalog_detail(
                &app.custom_themes,
                locale,
                app.settings.custom_theme.as_deref(),
            );
            if !detail.is_empty() {
                detail
            } else if app.custom_themes.follows_omarchy() {
                gettext(locale, "Follow system uses your Omarchy colours.")
            } else {
                gettext(
                    locale,
                    "Follow system uses your desktop's light or dark appearance.",
                )
            }
        }),
        RowText::new(
            gettext(locale, "Language"),
            gettext(
                locale,
                "System follows your computer's language. Untranslated text stays in English.",
            ),
        ),
        RowText::new(
            accent_from_art.clone(),
            gettext(
                locale,
                "Use the current cover's colour on pages and the player bar.",
            ),
        ),
        RowText::new(
            sidebar_compact.clone(),
            gettext(locale, "Show names without covers in the sidebar."),
        ),
        RowText::new(
            tracklist_compact.clone(),
            gettext(locale, "Show each track on one line without a cover."),
        ),
        RowText::new(
            gettext(locale, "Interface zoom"),
            super::keys::platform_shortcut(
                &gettext(
                    locale,
                    "Ctrl+Plus and Ctrl+Minus work anywhere; Ctrl+0 resets.",
                ),
                &gettext(
                    locale,
                    "Cmd+Plus and Cmd+Minus work anywhere; Cmd+0 resets.",
                ),
            )
            .to_owned(),
        ),
        RowText::new(
            middle_click.clone(),
            gettext(
                locale,
                "Middle-click a list, then move the pointer to scroll it. Off by default, because a middle click usually pastes on Linux.",
            ),
        )
        .when(cfg!(target_os = "linux")),
        RowText::new(
            custom_titlebar.clone(),
            gettext(
                locale,
                "Draw Spotifast's own title bar and window buttons instead of the standard Windows ones.",
            ),
        )
        .when(app.windows_controls_visible()),
        RowText::new(
            player_bar_vis.clone(),
            gettext(
                locale,
                "Show the song moving behind the player bar's controls while it plays here.",
            ),
        ),
    ];
    if section_matches(&needle, &appearance, &appearance_rows) {
        any_visible = true;
        section(ui, &palette, &appearance, |ui| {
            filtered_row(
                ui,
                &palette,
                &needle,
                &appearance,
                &appearance_rows[0],
                |ui| {
                    ui.with_layout(Layout::top_down(Align::Max), |ui| {
                        let selected = app
                            .settings
                            .custom_theme
                            .as_deref()
                            .map(|filename| fastframe_theme::display_name(filename).into())
                            .unwrap_or_else(|| app.settings.theme.label(locale));
                        let response = egui::ComboBox::from_id_salt("appearance_theme")
                            .selected_text(selected.as_ref())
                            .width(200.0_f32.min(ui.available_width()))
                            .show_ui(ui, |ui| {
                                for choice in ThemeChoice::ALL {
                                    if ui
                                        .selectable_label(
                                            app.settings.custom_theme.is_none()
                                                && app.settings.theme == choice,
                                            choice.label(locale).as_ref(),
                                        )
                                        .clicked()
                                    {
                                        app.actions.push(Action::SetTheme(choice));
                                    }
                                }
                                if app.custom_themes.picker_themes().next().is_some() {
                                    ui.separator();
                                }
                                for theme in app.custom_themes.picker_themes() {
                                    if ui
                                        .selectable_label(
                                            app.settings.custom_theme.as_deref()
                                                == Some(theme.filename.as_str()),
                                            fastframe_theme::display_name(&theme.filename),
                                        )
                                        .clicked()
                                    {
                                        app.actions
                                            .push(Action::SetCustomTheme(theme.filename.clone()));
                                    }
                                }
                            });
                        response.response.widget_info(|| {
                            let mut info = egui::WidgetInfo::labeled(
                                egui::WidgetType::ComboBox,
                                ui.is_enabled(),
                                theme_title.as_ref(),
                            );
                            info.current_text_value = Some(selected.to_string());
                            info
                        });
                        if theme::soft_button(
                            ui,
                            &palette,
                            Some(Icon::ExternalLink),
                            &gettext(locale, "Open themes folder"),
                            false,
                        )
                        .clicked()
                        {
                            app.actions.push(Action::OpenThemesFolder);
                        }
                    });
                },
            );
            filtered_row(
                ui,
                &palette,
                &needle,
                &appearance,
                &appearance_rows[1],
                |ui| language_picker(app, ui),
            );
            filtered_row(
                ui,
                &palette,
                &needle,
                &appearance,
                &appearance_rows[2],
                |ui| {
                    if widgets::switch(
                        ui,
                        &palette,
                        &accent_from_art,
                        &mut app.settings.accent_from_art,
                    )
                    .changed()
                    {
                        changed = true;
                    }
                },
            );
            {
                use crate::settings::PlayerBarVis;
                let choices = [
                    (PlayerBarVis::Off, gettext(locale, "Off")),
                    (PlayerBarVis::Spectrum, gettext(locale, "Spectrum")),
                    (PlayerBarVis::Waveform, gettext(locale, "Waveform")),
                ];
                let choice_gap = 6.0;
                let choices_width = choices
                    .iter()
                    .map(|(_, label)| theme::soft_button_width(ui, label))
                    .sum::<f32>()
                    + choice_gap * (choices.len() - 1) as f32;
                filtered_row_sized(
                    ui,
                    &palette,
                    &needle,
                    &appearance,
                    &appearance_rows[8],
                    choices_width,
                    |ui| {
                        let mut choose = |ui: &mut egui::Ui, mode: PlayerBarVis, label: &str| {
                            if theme::soft_button(
                                ui,
                                &palette,
                                None,
                                label,
                                app.settings.player_bar_vis == mode,
                            )
                            .clicked()
                                && app.settings.player_bar_vis != mode
                            {
                                app.settings.player_bar_vis = mode;
                                changed = true;
                            }
                        };
                        if ui.available_width() >= choices_width {
                            // Laid right to left, so the last choice goes first.
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = choice_gap;
                                for (mode, label) in choices.iter().rev() {
                                    choose(ui, *mode, label);
                                }
                            });
                        } else {
                            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                                ui.spacing_mut().item_spacing.y = choice_gap;
                                for (mode, label) in &choices {
                                    choose(ui, *mode, label);
                                }
                            });
                        }
                    },
                );
            }
            filtered_row(
                ui,
                &palette,
                &needle,
                &appearance,
                &appearance_rows[3],
                |ui| {
                    if widgets::switch(
                        ui,
                        &palette,
                        &sidebar_compact,
                        &mut app.settings.sidebar_compact,
                    )
                    .changed()
                    {
                        changed = true;
                    }
                },
            );
            filtered_row(
                ui,
                &palette,
                &needle,
                &appearance,
                &appearance_rows[4],
                |ui| {
                    if widgets::switch(
                        ui,
                        &palette,
                        &tracklist_compact,
                        &mut app.settings.tracklist_compact,
                    )
                    .changed()
                    {
                        changed = true;
                    }
                },
            );
            filtered_row(
                ui,
                &palette,
                &needle,
                &appearance,
                &appearance_rows[5],
                |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let mut zoom = app.settings.zoom;
                        if theme::soft_button(ui, &palette, None, "+", false).clicked() {
                            zoom = (zoom + 0.1).min(2.5);
                        }
                        theme::text(
                            ui,
                            format!("{:.0}%", zoom * 100.0),
                            theme::medium(13.5),
                            palette.text,
                        );
                        if theme::soft_button(ui, &palette, None, "-", false).clicked() {
                            zoom = (zoom - 0.1).max(0.5);
                        }
                        if (zoom - app.settings.zoom).abs() > 0.001 {
                            app.settings.zoom = zoom;
                            ui.ctx().set_zoom_factor(zoom);
                            app.mark_settings_dirty();
                        }
                    });
                },
            );
            if cfg!(target_os = "linux") {
                filtered_row(
                    ui,
                    &palette,
                    &needle,
                    &appearance,
                    &appearance_rows[6],
                    |ui| {
                        if widgets::switch(
                            ui,
                            &palette,
                            &middle_click,
                            &mut app.settings.middle_click_autoscroll,
                        )
                        .changed()
                        {
                            changed = true;
                        }
                    },
                );
            }
            if app.windows_controls_visible() {
                filtered_row(
                    ui,
                    &palette,
                    &needle,
                    &appearance,
                    &appearance_rows[7],
                    |ui| {
                        let mut custom = app.settings.custom_titlebar;
                        if widgets::switch(ui, &palette, &custom_titlebar, &mut custom).changed() {
                            app.actions.push(Action::SetCustomTitlebar(custom));
                        }
                    },
                );
            }
        });
    }

    let proxy = gettext(locale, "Proxy");
    let proxy_rows = [RowText::new(
        gettext(locale, "Mode, host, port, username and password"),
        gettext(
            locale,
            "Off, System, HTTP or SOCKS5 proxy for network requests.",
        ),
    )];
    if section_matches(&needle, &proxy, &proxy_rows) {
        any_visible = true;
        ui.push_id("proxy-settings", |ui| {
    section(ui, &palette, &proxy, |ui| {
        widgets::setting_row(
            ui,
            &palette,
            &pgettext(locale, "proxy", "Mode"),
            &gettext(
                locale,
                "Off ignores environment variables. System uses them, and the OS proxy on macOS and Windows.",
            ),
            |_| {},
        );
        // The row's control slot is right-to-left and too narrow for four
        // choices; they sit on this line so they read Off, System, HTTP, SOCKS5.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for choice in ProxyMode::ALL {
                if theme::soft_button(
                    ui,
                    &palette,
                    None,
                    &choice.label(locale),
                    app.settings.proxy_mode == choice,
                )
                .clicked()
                    && app.settings.proxy_mode != choice
                {
                    app.settings.proxy_mode = choice;
                    changed = true;
                    app.actions.push(Action::ProxyEdited);
                    if choice.is_manual() {
                        proxy_dirty = true;
                    } else {
                        proxy_dirty = false;
                        app.actions.push(Action::ApplyProxy);
                    }
                }
            }
        });
        ui.add_space(10.0);
        if app.settings.proxy_mode.is_manual() {
            if widgets::proxy_manual_form(
                ui,
                &palette,
                app.locale,
                &mut app.settings.proxy_host,
                &mut app.settings.proxy_port,
                &mut app.settings.proxy_username,
                &mut app.settings.proxy_password,
            ) {
                changed = true;
                app.actions.push(Action::ProxyEdited);
                proxy_dirty = true;
            }
            ui.add_space(6.0);
            widgets::proxy_scope_note(ui, &palette, app.locale, app.settings.proxy_mode);
            ui.add_space(10.0);
        }
        if app.settings.proxy_mode.is_manual() {
            ui.horizontal(|ui| {
                if theme::pill_button(ui, &palette, &gettext(locale, "Apply settings"), true).clicked() {
                    app.actions.push(Action::ApplyProxy);
                    if app.settings.proxy_config().is_ok() {
                        proxy_dirty = false;
                    }
                }
            });
        } else {
            theme::subtle(
                ui,
                &palette,
                &match app.settings.proxy_mode {
                    ProxyMode::Off => gettext(locale, "Not using a proxy."),
                    ProxyMode::System => gettext(locale, "Using the system proxy."),
                    ProxyMode::Http | ProxyMode::Socks => "".into(),
                },
            );
        }
    });
        });
    }

    let skins_folder = app.dirs.skins_dir();
    app.winamp.refresh_choices(&skins_folder);
    let skins = gettext(locale, "Winamp skins");
    let always_on_top = gettext(locale, "Always on top");
    let show_in_taskbar = gettext(locale, "Show Winamp in taskbar");
    let skins_rows = [
        RowText::new(
            gettext(locale, "Mini player"),
            super::keys::platform_shortcut(
                &gettext(
                    locale,
                    "Use classic Winamp .wsz skins. Press Ctrl+M or click the skin logo to return. Drop a skin on either window to add it.",
                ),
                &gettext(
                    locale,
                    "Use classic Winamp .wsz skins. Press Cmd+Shift+M or click the skin logo to return. Drop a skin on either window to add it.",
                ),
            )
            .to_owned(),
        ),
        RowText::new(
            gettext(locale, "Skin"),
            gettext(
                locale,
                // Translators: {folder} is the path of the skins folder.
                "Installed skins are in {folder}. Find more at the Winamp Skin Museum.",
            )
            .replace("{folder}", &skins_folder.display().to_string()),
        ),
        RowText::new(
            gettext(locale, "Size"),
            gettext(locale, "Whole-number scaling keeps skin pixels sharp."),
        ),
        RowText::new(
            always_on_top.clone(),
            if app.window_level_supported {
                gettext(locale, "Keep the Winamp window above everything else.")
            } else {
                crate::window::on_top_unavailable(locale)
            },
        ),
        RowText::new(
            gettext(locale, "Show in taskbar"),
            gettext(
                locale,
                "Keep a taskbar button for the mini player. The tray icon stays available when hidden.",
            ),
        )
        .when(app.taskbar_setting_visible()),
        RowText::new(
            gettext(locale, "Installed skins"),
            app.winamp
                .choices
                .iter()
                .map(|choice| choice.name.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        ),
    ];
    if section_matches(&needle, &skins, &skins_rows) {
        any_visible = true;
        section(ui, &palette, &skins, |ui| {
            filtered_row(ui, &palette, &needle, &skins, &skins_rows[0], |ui| {
                if theme::pill_button(ui, &palette, &gettext(locale, "Switch to it"), true)
                    .clicked()
                {
                    app.actions.push(Action::ToggleWinampWindow);
                }
            });
            filtered_row(ui, &palette, &needle, &skins, &skins_rows[1], |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if theme::soft_button(
                        ui,
                        &palette,
                        Some(Icon::Globe),
                        &gettext(locale, "Skin Museum"),
                        false,
                    )
                    .clicked()
                    {
                        app.actions
                            .push(Action::OpenUrl("https://skins.webamp.org/".into()));
                    }
                    if theme::soft_button(
                        ui,
                        &palette,
                        Some(Icon::ExternalLink),
                        &open_folder,
                        false,
                    )
                    .clicked()
                    {
                        app.actions.push(Action::OpenSkinsFolder);
                    }
                });
            });
            let choices = app.winamp.choices.clone();
            if skins_rows[1].matches(&needle, &skins) || skins_rows[5].matches(&needle, &skins) {
                let mut options: Vec<(usize, &str)> = vec![(0, "Spotifast")];
                options.extend(
                    choices
                        .iter()
                        .enumerate()
                        .map(|(index, choice)| (index + 1, choice.label())),
                );
                let current = app
                    .settings
                    .skin
                    .as_deref()
                    .and_then(|name| choices.iter().position(|choice| choice.name == name))
                    .map_or(0, |index| index + 1);
                if let Some(picked) = widgets::chips(ui, &palette, &options, current)
                    && picked != current
                {
                    let name = picked
                        .checked_sub(1)
                        .map(|index| choices[index].name.clone());
                    app.actions.push(Action::SetSkin(name));
                }
                ui.add_space(4.0);
            }
            filtered_row(ui, &palette, &needle, &skins, &skins_rows[2], |ui| {
                let scale =
                    crate::winamp::WinampState::scale(&app.settings, ui.ctx().pixels_per_point());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for candidate in 1..=crate::winamp::MAX_SCALE {
                        let label = format!("{candidate}x");
                        if theme::soft_button(ui, &palette, None, &label, candidate == scale)
                            .clicked()
                            && candidate != scale
                        {
                            app.actions.push(Action::SetSkinScale(candidate as u8));
                        }
                    }
                });
            });
            filtered_row(ui, &palette, &needle, &skins, &skins_rows[3], |ui| {
                ui.add_enabled_ui(app.window_level_supported, |ui| {
                    let mut on_top = app.settings.winamp_on_top && app.window_level_supported;
                    if widgets::switch(ui, &palette, &always_on_top, &mut on_top).changed() {
                        app.actions.push(Action::ToggleWinampOnTop);
                    }
                });
            });
            if app.taskbar_setting_visible() {
                filtered_row(ui, &palette, &needle, &skins, &skins_rows[4], |ui| {
                    let mut visible = app.settings.winamp_show_taskbar;
                    let response = widgets::switch(ui, &palette, &show_in_taskbar, &mut visible);
                    if response.changed() {
                        app.actions.push(Action::SetWinampTaskbar(visible));
                    }
                    #[cfg(any(test, feature = "demo"))]
                    if app.demo_windows_controls {
                        let id = egui::Id::new("demo-winamp-taskbar-focus");
                        if !ui.data(|data| data.get_temp::<bool>(id)).unwrap_or(false) {
                            response.scroll_to_me(Some(Align::Center));
                            ui.data_mut(|data| data.insert_temp(id, true));
                        }
                    }
                });
            }
        });
    }

    let presets_folder = app.dirs.milkdrop_dir();
    app.winamp.presets.refresh(&presets_folder);
    let count = app.winamp.presets.count();
    let screen_hz = app.settings.milkdrop_screen_hz;
    let milkdrop_window = gettext(locale, "MilkDrop window");
    let folder = presets_folder.display().to_string();
    let milkdrop_rows = [
        RowText::new(
            milkdrop_window.clone(),
            super::keys::platform_shortcut(
                &gettext(
                    locale,
                    "A projectM visualiser for local playback. Open it here, from the top bar, with Ctrl+Shift+K, or from the mini player's V menu. Press ? or F1 for its shortcuts.",
                ),
                &gettext(
                    locale,
                    "A projectM visualiser for local playback. Open it here, from the top bar, with Cmd+Shift+K, or from the mini player's V menu. Press ? or F1 for its shortcuts.",
                ),
            )
            .to_owned(),
        ),
        RowText::new(
            gettext(locale, "Presets"),
            match count {
                0 => gettext(
                    locale,
                    // Translators: {folder} is the path of the MilkDrop presets folder.
                    "None yet in {folder}. Add .milk files here. Spotifast downloads presets when MilkDrop first opens with an empty folder.",
                )
                .replace("{folder}", &folder),
                1 => gettext(
                    locale,
                    // Translators: {folder} is the path of the MilkDrop presets folder.
                    "One preset in {folder}. Add .milk files here. Spotifast downloads presets when MilkDrop first opens with an empty folder.",
                )
                .replace("{folder}", &folder),
                n => ngettext(
                    locale,
                    // Translators: {count} is the number of presets, {folder} the path of the MilkDrop presets folder.
                    "{count} preset in {folder}. Add .milk files here. Spotifast downloads presets when MilkDrop first opens with an empty folder.",
                    "{count} presets in {folder}. Add .milk files here. Spotifast downloads presets when MilkDrop first opens with an empty folder.",
                    u32::try_from(n).unwrap_or(u32::MAX),
                )
                .replace("{count}", &n.to_string())
                .replace("{folder}", &folder),
            },
        ),
        RowText::new(
            gettext(locale, "Time per preset"),
            gettext(
                locale,
                "How long each preset plays before the next fades in.",
            ),
        ),
        RowText::new(
            gettext(locale, "Frame rate"),
            match screen_hz {
                0 => gettext(
                    locale,
                    "Lower rates use fewer resources. Uncapped draws as fast as possible.",
                ),
                hz => gettext(
                    locale,
                    // Translators: {hz} is the screen's refresh rate in hertz.
                    "Your screen refreshes at {hz} Hz. Higher rates do not add visible frames. Uncapped draws as fast as possible.",
                )
                .replace("{hz}", &hz.to_string())
                .into(),
            },
        ),
        RowText::new(
            gettext(locale, "Resolution"),
            gettext(
                locale,
                "Half and Quarter use fewer resources and scale the image back up.",
            ),
        ),
        RowText::new(
            format!(
                "{} {}",
                // Translators: search keywords for the MilkDrop preset download buttons.
                gettext(locale, "Get presets"),
                open_folder
            ),
            crate::milkdrop::PACKS
                .iter()
                .map(|pack| pack.name)
                .collect::<Vec<_>>()
                .join(" "),
        ),
    ];
    if section_matches(&needle, "MilkDrop", &milkdrop_rows) {
        any_visible = true;
        section(ui, &palette, "MilkDrop", |ui| {
            filtered_row(ui, &palette, &needle, "MilkDrop", &milkdrop_rows[0], |ui| {
                let mut open = app.settings.milkdrop_open;
                if widgets::switch(ui, &palette, &milkdrop_window, &mut open).changed() {
                    app.actions.push(Action::ToggleWinampMilkdrop);
                }
            });
            let downloading = app.winamp.presets.downloading();
            filtered_row(
                ui,
                &palette,
                &needle,
                "MilkDrop",
                &milkdrop_rows[1],
                |_ui| {},
            );
            // Three buttons are wider than a row's control slot; they get a
            // line of their own under the words.
            if milkdrop_rows[1].matches(&needle, "MilkDrop")
                || milkdrop_rows[5].matches(&needle, "MilkDrop")
            {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (index, pack) in crate::milkdrop::PACKS.iter().enumerate() {
                        let label = match downloading {
                            Some(name) if name == pack.name => {
                                gettext(locale, "Fetching...").into_owned()
                            }
                            // Translators: {pack} is the name of a MilkDrop preset pack.
                            _ => gettext(locale, "Get {pack}").replace("{pack}", pack.name),
                        };
                        if theme::soft_button(ui, &palette, Some(Icon::Globe), &label, false)
                            .on_hover_text(pack.note)
                            .clicked()
                            && downloading.is_none()
                        {
                            app.actions.push(Action::DownloadMilkdropPack(index));
                        }
                    }
                    if theme::soft_button(
                        ui,
                        &palette,
                        Some(Icon::ExternalLink),
                        &open_folder,
                        false,
                    )
                    .clicked()
                    {
                        app.actions.push(Action::OpenMilkdropFolder);
                    }
                });
                ui.add_space(10.0);
            }
            filtered_row(ui, &palette, &needle, "MilkDrop", &milkdrop_rows[2], |ui| {
                let mut seconds = app.settings.milkdrop_seconds.clamp(2, 300);
                let slider = egui::Slider::new(&mut seconds, 2..=300)
                    .logarithmic(true)
                    .suffix(" s");
                if ui.add(slider).changed() {
                    app.actions.push(Action::SetMilkdropSeconds(seconds));
                }
            });
            filtered_row(ui, &palette, &needle, "MilkDrop", &milkdrop_rows[3], |ui| {
                let fps = app.settings.milkdrop_fps;
                // The dial stops at the rates worth having and passes
                // through nothing in between, the way a gear lever does.
                let stops = crate::milkdrop::fps_stops(screen_hz, fps);
                let last = stops.len().saturating_sub(1);
                let mut at = stops.iter().position(|rate| *rate == fps).unwrap_or(1);
                let labels: Vec<String> = stops
                    .iter()
                    .map(|rate| crate::milkdrop::fps_label(locale, *rate, screen_hz))
                    .collect();
                let shown = labels.clone();
                let typed = stops.clone();
                let slider = egui::Slider::new(&mut at, 0..=last)
                    .step_by(1.0)
                    .custom_formatter(move |value, _| {
                        shown
                            .get((value.round().max(0.0) as usize).min(shown.len() - 1))
                            .cloned()
                            .unwrap_or_default()
                    })
                    .custom_parser(move |text| {
                        // A rate typed in lands on the nearest stop, since
                        // the stops are all this dial can hold.
                        let text = text.trim().to_lowercase();
                        if text.starts_with("un") {
                            return Some(typed.len().saturating_sub(1) as f64);
                        }
                        let wanted: u32 = text
                            .trim_end_matches("fps")
                            .trim()
                            .split(',')
                            .next()?
                            .trim()
                            .parse()
                            .ok()?;
                        typed
                            .iter()
                            .enumerate()
                            .filter(|(_, rate)| **rate > 0)
                            .min_by_key(|(_, rate)| rate.abs_diff(wanted))
                            .map(|(index, _)| index as f64)
                    });
                if ui.add(slider).changed()
                    && let Some(rate) = stops.get(at)
                {
                    app.actions.push(Action::SetMilkdropFps(*rate));
                }
            });
            filtered_row(ui, &palette, &needle, "MilkDrop", &milkdrop_rows[4], |ui| {
                let current = app.settings.milkdrop_scale.max(1);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (scale, label) in [
                        (1u32, pgettext(locale, "resolution", "Full")),
                        (2, pgettext(locale, "resolution", "Half")),
                        (4, pgettext(locale, "resolution", "Quarter")),
                    ] {
                        if theme::soft_button(ui, &palette, None, &label, scale == current)
                            .clicked()
                            && scale != current
                        {
                            app.actions.push(Action::SetMilkdropScale(scale));
                        }
                    }
                });
            });
        });
    }

    let equalizer = gettext(locale, "Equalizer");
    let equalizer_rows = [
        RowText::new(
            equalizer.clone(),
            gettext(
                locale,
                "A ten-band equalizer for playback on this computer. It does not affect other devices.",
            ),
        ),
        RowText::new(
            format!(
                "{} {} {} dB",
                gettext(locale, "Presets"),
                // Translators: a search keyword for the equalizer's preamplifier slider.
                gettext(locale, "Preamp"),
                // Translators: a search keyword for the equalizer's frequency band sliders.
                gettext(locale, "Bands"),
            ),
            crate::eq::PRESETS
                .iter()
                .map(|preset| preset.name)
                .collect::<Vec<_>>()
                .join(" "),
        ),
    ];
    if section_matches(&needle, &equalizer, &equalizer_rows) {
        any_visible = true;
        section(ui, &palette, &equalizer, |ui| {
            filtered_row(
                ui,
                &palette,
                &needle,
                &equalizer,
                &equalizer_rows[0],
                |ui| {
                    let mut on = app.settings.eq_on;
                    if widgets::switch(ui, &palette, &equalizer, &mut on).changed() {
                        app.actions.push(Action::ToggleEq);
                    }
                },
            );
            let names: Vec<(usize, &str)> = crate::eq::PRESETS
                .iter()
                .enumerate()
                .map(|(index, preset)| (index, preset.name))
                .collect();
            let current = crate::eq::PRESETS
                .iter()
                .position(|preset| preset.bands_db == app.settings.eq_bands_db)
                .unwrap_or(usize::MAX);
            let eq_extra_visible = equalizer_rows[1].matches(&needle, &equalizer);
            if eq_extra_visible {
                if let Some(picked) = widgets::chips(ui, &palette, &names, current) {
                    app.actions.push(Action::ApplyEqPreset(picked));
                }
                ui.add_space(10.0);
                eq_curve(ui, &palette, &crate::app::eq_settings(&app.settings));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    let on = app.settings.eq_on;
                    let mut preamp = app.settings.eq_preamp_db;
                    if eq_slider(
                        ui,
                        &palette,
                        // Translators: short label under the equalizer's preamplifier slider.
                        &pgettext(locale, "equalizer", "Pre"),
                        &mut preamp,
                        on,
                    ) {
                        app.actions.push(Action::SetEqPreamp(preamp));
                    }
                    for (band, hz) in crate::eq::BANDS.iter().enumerate() {
                        let mut gain = app.settings.eq_bands_db[band];
                        if eq_slider(ui, &palette, &hertz(*hz), &mut gain, on) {
                            app.actions.push(Action::SetEqBand(band, gain));
                        }
                    }
                });
            }
        });
    }

    let storage = gettext(locale, "Storage");
    let storage_rows = [
        RowText::new(
            gettext(locale, "Artwork cache"),
            // Translators: {folder} is the path of a cache folder.
            gettext(locale, "Stored in {folder}")
                .replace("{folder}", &app.dirs.art_cache_dir().display().to_string()),
        ),
        RowText::new(
            audio_cache.clone(),
            // Translators: {folder} is the path of a cache folder.
            gettext(locale, "Stored in {folder}").replace(
                "{folder}",
                &app.dirs.audio_cache_dir().display().to_string(),
            ),
        ),
        RowText::new(
            gettext(locale, "Play history"),
            gettext(
                locale,
                // Translators: {file} is the path of the play history file.
                "Tracks played here are stored in {file}. This file is never uploaded.",
            )
            .replace("{file}", &app.dirs.history_file().display().to_string()),
        ),
        RowText::new(
            gettext(locale, "Sign-in"),
            gettext(
                locale,
                "Sign-ins are saved in the system credential store when available.",
            ),
        ),
    ];
    if section_matches(&needle, &storage, &storage_rows) {
        any_visible = true;
        section(ui, &palette, &storage, |ui| {
            filtered_row(ui, &palette, &needle, &storage, &storage_rows[0], |ui| {
                if theme::soft_button(
                    ui,
                    &palette,
                    Some(Icon::Trash),
                    &gettext(locale, "Clear artwork"),
                    false,
                )
                .clicked()
                {
                    app.actions.push(Action::ClearArtCache);
                }
            });
            filtered_row(ui, &palette, &needle, &storage, &storage_rows[1], |_| {});
            filtered_row(ui, &palette, &needle, &storage, &storage_rows[2], |ui| {
                if theme::soft_button(
                    ui,
                    &palette,
                    Some(Icon::Trash),
                    &gettext(locale, "Clear history"),
                    false,
                )
                .clicked()
                {
                    app.actions.push(Action::ClearPlayHistory);
                }
            });
            filtered_row(ui, &palette, &needle, &storage, &storage_rows[3], |_| {});
        });
    }

    let about = gettext(locale, "About");
    let built_with = gettext(
        locale,
        "Built with Rust, egui, and librespot. Not affiliated with Spotify.",
    );
    let check_for_updates = gettext(locale, "Check for updates");
    let checking = gettext(locale, "Checking…");
    let keyboard_shortcuts = gettext(locale, "Keyboard shortcuts");
    let source_code = gettext(locale, "Source code");
    let about_rows = [
        RowText::new(
            format!("Spotifast {}", env!("CARGO_PKG_VERSION")),
            built_with.clone(),
        ),
        RowText::new(
            format!("{check_for_updates} {checking}"),
            format!("{keyboard_shortcuts} {source_code}"),
        ),
    ];
    if section_matches(&needle, &about, &about_rows) {
        any_visible = true;
        section(ui, &palette, &about, |ui| {
            ui.horizontal(|ui| {
                let (logo, _) = ui.allocate_exact_size(Vec2::splat(40.0), egui::Sense::hover());
                theme::logo(ui, logo.center(), 40.0, palette.accent, palette.on_accent);
                ui.vertical(|ui| {
                    theme::text(
                        ui,
                        format!("Spotifast {}", env!("CARGO_PKG_VERSION")),
                        theme::semibold(15.0),
                        palette.text,
                    );
                    theme::text(
                        ui,
                        built_with.as_ref(),
                        theme::regular(13.0),
                        palette.secondary,
                    );
                });
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let check_label = if app.update_checking {
                    &checking
                } else {
                    &check_for_updates
                };
                if theme::soft_button(ui, &palette, Some(Icon::Refresh), check_label, false)
                    .clicked()
                    && !app.update_checking
                {
                    app.actions.push(Action::CheckForUpdates);
                }
                if theme::soft_button(ui, &palette, Some(Icon::Info), &keyboard_shortcuts, false)
                    .clicked()
                {
                    app.actions.push(Action::ShowDialog(Dialog::Shortcuts));
                }
                if theme::soft_button(ui, &palette, Some(Icon::ExternalLink), &source_code, false)
                    .clicked()
                {
                    ui.ctx()
                        .open_url(egui::OpenUrl::new_tab(env!("CARGO_PKG_REPOSITORY")));
                }
            });
            ui.add_space(14.0);
            if widgets::credit(ui, &palette, locale) {
                app.actions
                    .push(Action::OpenUrl(widgets::AUTHOR_URL.to_owned()));
            }
        });
    }

    if !needle.is_empty() && !any_visible {
        widgets::empty_state(
            ui,
            &palette,
            Icon::Search,
            // Translators: {query} is the text typed into the settings search field.
            &gettext(locale, "No settings for “{query}”").replace("{query}", &needle),
            &gettext(locale, "Try fewer words, or check the spelling."),
        );
    }

    ui.data_mut(|data| data.insert_temp(dirty_id, playback_dirty));
    ui.data_mut(|data| data.insert_temp(proxy_dirty_id, proxy_dirty));
    if changed {
        app.actions.push(Action::SettingsChanged);
    }
}

/// A band's frequency the short way: 60, 170, 1K, 16K.
/// The interface language: System first, then each language by its own name,
/// so a reader can find theirs whatever language the app is showing.
fn language_picker(app: &mut App, ui: &mut egui::Ui) {
    let locale = app.locale;
    let system = pgettext(locale, "language", "System");
    let current = app.settings.language;
    let selected = match current {
        LanguageChoice::System => system.clone(),
        LanguageChoice::Locale(chosen) => chosen.native_name().into(),
    };
    let response = egui::ComboBox::from_id_salt("interface_language")
        .selected_text(selected.as_ref())
        .width(200.0_f32.min(ui.available_width()))
        // As many languages as a menu holds before it scrolls, not five.
        .height(1000.0)
        .show_ui(ui, |ui| {
            let choices = std::iter::once((LanguageChoice::System, system.clone())).chain(
                crate::i18n::LOCALES
                    .iter()
                    .map(|&each| (LanguageChoice::Locale(each), each.native_name().into())),
            );
            for (choice, label) in choices {
                if ui
                    .selectable_label(current == choice, label.as_ref())
                    .clicked()
                    && current != choice
                {
                    app.actions.push(Action::SetLanguage(choice));
                }
            }
        });
    let name = gettext(locale, "Language");
    response.response.widget_info(|| {
        let mut info =
            egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, ui.is_enabled(), name.as_ref());
        info.current_text_value = Some(selected.to_string());
        info
    });
}

fn hertz(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{}K", (hz / 1000.0).round() as u32)
    } else {
        format!("{}", hz.round() as u32)
    }
}

/// One vertical slider in the app's own style: the track filled from
/// 0 dB, the handle in the middle when flat, a double-click to put it
/// back there. Returns whether it moved.
fn eq_slider(ui: &mut egui::Ui, palette: &Palette, label: &str, value: &mut f32, on: bool) -> bool {
    use egui::{Rect, Stroke, pos2, vec2};
    let range = crate::eq::RANGE_DB;
    ui.vertical(|ui| {
        let (rect, response) =
            ui.allocate_exact_size(vec2(30.0, 118.0), egui::Sense::click_and_drag());
        let track = Rect::from_center_size(rect.center(), vec2(4.0, rect.height() - 20.0));
        let y_of = |db: f32| track.bottom() - (db + range) / (2.0 * range) * track.height();
        let mut changed = false;
        if response.double_clicked() {
            *value = 0.0;
            changed = true;
        } else if (response.dragged() || response.clicked())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let db = (track.bottom() - pos.y) / track.height() * 2.0 * range - range;
            let db = (db.clamp(-range, range) * 10.0).round() / 10.0;
            if db != *value {
                *value = db;
                changed = true;
            }
        }
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            painter.rect_filled(track, 2.0, palette.surface_active);
            let fill = if on { palette.accent } else { palette.dim };
            let (top, bottom) = (y_of(value.max(0.0)), y_of(value.min(0.0)));
            painter.rect_filled(
                Rect::from_min_max(pos2(track.left(), top), pos2(track.right(), bottom)),
                2.0,
                fill,
            );
            painter.hline(
                (track.left() - 3.0)..=(track.right() + 3.0),
                y_of(0.0),
                Stroke::new(1.0, palette.dim),
            );
            let handle = pos2(track.center().x, y_of(*value));
            painter.circle_filled(handle, 7.0, palette.text);
            if response.hovered() || response.dragged() {
                painter.text(
                    pos2(track.center().x, rect.top() + 2.0),
                    egui::Align2::CENTER_TOP,
                    format!("{value:+.1}"),
                    theme::regular(11.0),
                    palette.secondary,
                );
            }
        }
        theme::text(ui, label, theme::regular(11.5), palette.secondary);
        changed
    })
    .inner
}

/// The equalizer's response over the audible range, the bands marked on
/// it: the shape says what a row of numbers cannot.
fn eq_curve(ui: &mut egui::Ui, palette: &Palette, settings: &crate::eq::EqSettings) {
    use egui::{Shape, Stroke, pos2, vec2};
    let width = ui.available_width().min(720.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 120.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, theme::RADIUS as f32, palette.surface);
    let plot = rect.shrink2(vec2(10.0, 12.0));
    let (low, high) = (20f32.log10(), 20_000f32.log10());
    let x_of = |hz: f32| plot.left() + (hz.log10() - low) / (high - low) * plot.width();
    let y_of = |db: f32| {
        plot.center().y
            - db.clamp(-crate::eq::RANGE_DB, crate::eq::RANGE_DB) / crate::eq::RANGE_DB
                * plot.height()
                / 2.0
    };
    for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
        let color = if db == 0.0 {
            palette.dim
        } else {
            palette.outline
        };
        painter.hline(plot.x_range(), y_of(db), Stroke::new(1.0, color));
    }
    for hz in crate::eq::BANDS {
        painter.vline(x_of(hz), plot.y_range(), Stroke::new(1.0, palette.outline));
    }
    let curve = settings.curve();
    let points: Vec<egui::Pos2> = (0..=240)
        .map(|step| {
            let t = step as f32 / 240.0;
            let hz = 10f32.powf(low + t * (high - low));
            pos2(plot.left() + t * plot.width(), y_of(curve.db_at(hz)))
        })
        .collect();
    let color = if settings.on {
        palette.accent
    } else {
        palette.dim
    };
    painter.add(Shape::line(points, Stroke::new(2.0, color)));
    for (hz, db) in crate::eq::BANDS.iter().zip(settings.bands_db) {
        painter.circle_filled(pos2(x_of(*hz), y_of(db + settings.preamp_db)), 3.0, color);
    }
}

#[cfg(test)]
mod tests {
    use super::{RowText, row_matches, section_matches};

    #[test]
    fn empty_filter_matches_everything() {
        assert!(row_matches(
            "",
            "Normalize volume",
            "Keep loud tracks level"
        ));
        assert!(section_matches(
            "",
            "Playback on this computer",
            &[RowText::new("Status", "Spotify")]
        ));
    }

    #[test]
    fn row_matches_title_or_description_case_insensitively() {
        assert!(row_matches(
            "volume",
            "Normalize volume",
            "Keep loud tracks level"
        ));
        assert!(row_matches(
            "VOLUME",
            "Normalize volume",
            "Keep loud tracks level"
        ));
        assert!(row_matches(
            "quiet",
            "Normalize volume",
            "Keep loud and quiet tracks level"
        ));
        assert!(!row_matches(
            "theme",
            "Normalize volume",
            "Keep loud tracks level"
        ));
    }

    #[test]
    fn section_matches_title_or_any_row() {
        let rows = [
            RowText::new("Normalize volume", "Keep loud and quiet tracks level"),
            RowText::new("Autoplay", "Keep playing similar songs"),
        ];
        assert!(section_matches(
            "playback",
            "Playback on this computer",
            &rows
        ));
        assert!(section_matches(
            "autoplay",
            "Playback on this computer",
            &rows
        ));
        assert!(section_matches("quiet", "Playback on this computer", &rows));
        assert!(!section_matches(
            "theme",
            "Playback on this computer",
            &rows
        ));
    }
}
