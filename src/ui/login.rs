//! The sign-in screen.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Rect, Stroke, Vec2, pos2};

use crate::app::App;
use crate::backend::{AuthStatus, SignInProblem};
use crate::i18n::gettext;
use crate::model::Action;
use crate::settings::ProxyMode;
use crate::theme;

/// Where listeners create their own Spotify app. Shown as a link they
/// click; Spotifast never opens it by itself.
pub const DASHBOARD_URL: &str = "https://developer.spotify.com/dashboard";

/// The width of the card on its own, and at most of each column beside
/// the steps.
const COLUMN: f32 = 368.0;
/// The space between the sign-in column and the setup steps.
const GUTTER: f32 = 40.0;

/// Whether the sign-in screen shows the steps for setting up an app: for a
/// listener who had none that looked right when the screen came up, and
/// whenever Spotify turned one down. Someone signing in again with their
/// app sees the button, and can still open the steps. Decided once, so the
/// card does not change shape as a Client ID is typed.
fn show_steps(app: &App, ctx: &egui::Context) -> bool {
    if matches!(
        app.auth,
        AuthStatus::Refused(problem) if problem != SignInProblem::Expired
    ) {
        return true;
    }
    let id = egui::Id::new(STEPS_OPEN);
    ctx.data_mut(|data| {
        *data.get_temp_mut_or_insert_with(id, || {
            !app.settings
                .web_client_id
                .as_deref()
                .is_some_and(crate::auth::looks_like_client_id)
        })
    })
}

const STEPS_OPEN: &str = "login-steps-open";

pub fn show(app: &mut App, ui: &mut egui::Ui, connecting: bool) {
    let palette = app.palette;
    let ctx = ui.ctx().clone();
    egui::CentralPanel::default()
        .frame(Frame::new().fill(palette.window))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            // The native double-click action belongs to the top bar, not the empty login background.
            let drag_rect = if cfg!(target_os = "macos") {
                Rect::from_min_size(
                    rect.min,
                    Vec2::new(
                        rect.width(),
                        theme::TOP_BAR_HEIGHT + theme::titlebar_inset(ui.ctx()),
                    ),
                )
            } else {
                rect
            };
            fastframe_titlebar::drag(ui, drag_rect);
            let top = super::blend(palette.window, palette.accent, 0.10);
            super::widgets::paint_vertical_gradient(ui, rect, top, palette.window);
            let waiting = connecting || matches!(app.auth, AuthStatus::WaitingForBrowser { .. });
            let steps = !waiting && show_steps(app, &ctx);
            // Two columns share what the window has beside the card's
            // margins, down to the smallest main window.
            let column = if steps {
                ((rect.width() - 120.0 - GUTTER) / 2.0).clamp(240.0, COLUMN)
            } else {
                COLUMN
            };
            let inner = if steps { column * 2.0 + GUTTER } else { COLUMN };
            let card_width = inner + 72.0;
            let proxy_id = egui::Id::new("login-proxy-open");
            let proxy_open = ui
                .ctx()
                .data(|data| data.get_temp::<bool>(proxy_id))
                .unwrap_or(false);
            let card_height: f32 = if !proxy_open {
                400.0
            } else if app.settings.proxy_mode.is_manual() {
                720.0
            } else {
                500.0
            };
            let room = (rect.height() - 64.0).max(0.0);
            // The card keeps its place, and grows down to fit what it holds
            // when that is taller, such as while waiting for the browser
            // (#782). It only scrolls when the window is too short for it.
            let content_id = egui::Id::new("login-card-content");
            let content = ui.ctx().data(|data| data.get_temp::<f32>(content_id));
            let card = card_rect(rect, card_width, card_height.min(room), content, room);
            let mut card_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(card)
                    .layout(Layout::top_down(Align::Center)),
            );
            Frame::new()
                .fill(palette.panel)
                .stroke(Stroke::new(1.0, palette.outline))
                .corner_radius(CornerRadius::same(theme::RADIUS + 8))
                .inner_margin(Margin::same(36))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 16],
                    blur: 48,
                    spread: 0,
                    color: palette.shadow,
                })
                .show(&mut card_ui, |ui| {
                    let scroll = egui::ScrollArea::vertical()
                        .id_salt("login-card-scroll")
                        .max_height((card.height() - 72.0).max(0.0))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.set_width(inner);
                            if steps {
                                ui.horizontal_top(|ui| {
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(column, 0.0),
                                        Layout::top_down(Align::Center),
                                        |ui| sign_in_column(app, ui, connecting, Some(column)),
                                    );
                                    ui.add_space(GUTTER);
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(column, 0.0),
                                        Layout::top_down(Align::Min),
                                        |ui| setup_steps(app, ui, column),
                                    );
                                });
                            } else {
                                sign_in_column(app, ui, connecting, None);
                            }
                        });
                    let measured = scroll.content_size.y;
                    if content != Some(measured) {
                        ui.ctx()
                            .data_mut(|data| data.insert_temp(content_id, measured));
                        ui.ctx().request_repaint();
                    }
                });
            ui.painter().text(
                pos2(rect.center().x, rect.bottom() - 24.0),
                egui::Align2::CENTER_BOTTOM,
                // Translators: {version} is the app's version number, such as 1.2.0.
                gettext(
                    app.locale,
                    "Spotifast {version} • not affiliated with Spotify",
                )
                .replace("{version}", env!("CARGO_PKG_VERSION")),
                theme::regular(11.5),
                palette.dim,
            );
        });
}

/// What Spotify's refusal means, and what to do about it.
fn problem_text(locale: crate::i18n::Locale, problem: SignInProblem) -> String {
    match problem {
        SignInProblem::MalformedClientId => gettext(
            locale,
            "That doesn't look like a Client ID. It is 32 letters and digits, shown on your app's page in the dashboard.",
        ),
        SignInProblem::UnknownClient => gettext(
            locale,
            "Spotify doesn't know this Client ID. Copy it again from your app's page in the dashboard.",
        ),
        SignInProblem::AccountNotAllowed => gettext(
            locale,
            "Spotify didn't let this account use the app. Sign in with the Spotify account that created it, which needs Premium.",
        ),
        SignInProblem::SharedRetired => gettext(
            locale,
            "Spotify no longer accepts the shared sign-in Spotifast used before. Set up your own Spotify app to keep listening.",
        ),
        SignInProblem::Expired => {
            gettext(locale, "Your Spotify sign-in expired. Please sign in again.")
        }
    }
    .into_owned()
}

fn wrapped(ui: &mut egui::Ui, text: impl Into<String>, font: egui::FontId, color: egui::Color32) {
    ui.add(egui::Label::new(egui::RichText::new(text.into()).font(font).color(color)).wrap());
}

/// The logo, the Client ID and the button: everything that signs in. Beside
/// the steps, `beside` is the column's width.
fn sign_in_column(app: &mut App, ui: &mut egui::Ui, connecting: bool, beside: Option<f32>) {
    let palette = app.palette;
    let locale = app.locale;
    let ctx = ui.ctx().clone();
    let steps = beside.is_some();
    ui.set_width(beside.unwrap_or(COLUMN));
    ui.spacing_mut().item_spacing.y = 8.0;
    // Beside the steps the card has more to hold in the same height, so
    // the heading is smaller.
    let logo_size = if steps { 56.0 } else { 72.0 };
    let (logo, _) = ui.allocate_exact_size(Vec2::splat(logo_size), egui::Sense::hover());
    theme::logo(ui, logo.center(), logo_size);
    ui.add_space(6.0);
    theme::text(
        ui,
        "Spotifast",
        theme::bold(if steps { 26.0 } else { 30.0 }),
        palette.text,
    );
    if !steps {
        theme::text(
            ui,
            gettext(locale, "A native Spotify client."),
            theme::regular(14.5),
            palette.secondary,
        );
    }
    ui.add_space(if steps { 12.0 } else { 22.0 });
    match &app.auth {
        AuthStatus::WaitingForBrowser { url } => {
            let url = url.clone();
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 250.0).max(0.0) / 2.0);
                theme::spinner(ui, 18.0, palette.accent);
                theme::text(
                    ui,
                    gettext(locale, "Waiting for Spotify in your browser…"),
                    theme::medium(14.0),
                    palette.text,
                );
            });
            ui.add_space(6.0);
            if theme::link(
                ui,
                gettext(locale, "Didn't open? Open the sign-in page again"),
                theme::regular(13.0),
                palette.secondary,
            )
            .clicked()
            {
                ctx.open_url(egui::OpenUrl::new_tab(url));
            }
            ui.add_space(2.0);
            // Spotify stops on its own page for this, and never tells the
            // app. An unknown Client ID was ruled out before the browser.
            wrapped(
                ui,
                // Translators: {uri} is the address to add, http://127.0.0.1:8989/login.
                gettext(
                    locale,
                    "If Spotify says “Invalid redirect URI”, add {uri} to your app exactly, save, and sign in again.",
                )
                .replace("{uri}", &crate::auth::personal_redirect_uri()),
                theme::regular(12.5),
                palette.secondary,
            );
            ui.add_space(6.0);
            if theme::pill_button(ui, &palette, &gettext(locale, "Cancel"), false).clicked() {
                app.actions.push(Action::CancelSignIn);
            }
        }
        _ if connecting => {
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - 200.0).max(0.0) / 2.0);
                theme::spinner(ui, 18.0, palette.accent);
                theme::text(
                    ui,
                    gettext(locale, "Connecting to Spotify…"),
                    theme::medium(14.0),
                    palette.text,
                );
            });
            if app.auth == AuthStatus::RateLimited {
                // Spotify can hold the account check back for a long time,
                // so say so rather than spin with no word (#861).
                ui.add_space(6.0);
                wrapped(
                    ui,
                    gettext(
                        locale,
                        "Spotify is being slow. Sign-in finishes on its own once Spotify allows it.",
                    ),
                    theme::regular(12.5),
                    palette.secondary,
                );
            }
            // The account check retries through rate limits and server
            // errors alike, so it can always be stopped. A signed-in
            // account still loading is not a sign-in to cancel.
            if matches!(app.auth, AuthStatus::Connecting | AuthStatus::RateLimited) {
                ui.add_space(6.0);
                if theme::pill_button(ui, &palette, &gettext(locale, "Cancel"), false).clicked() {
                    app.actions.push(Action::CancelSignIn);
                }
            }
        }
        status => {
            let problem = match status {
                AuthStatus::Failed(message) => Some(message.clone()),
                AuthStatus::Refused(problem) => Some(problem_text(locale, *problem)),
                _ => None,
            };
            if let Some(problem) = problem {
                wrapped(ui, problem, theme::regular(13.0), palette.danger);
                ui.add_space(4.0);
            }
            client_id_field(app, ui);
            ui.add_space(4.0);
            let ready = app
                .settings
                .web_client_id
                .as_deref()
                .is_some_and(crate::auth::looks_like_client_id);
            if ui
                .add_enabled_ui(ready, |ui| {
                    big_button(ui, app, &gettext(locale, "Sign in with Spotify"))
                })
                .inner
            {
                app.actions.push(Action::SignIn);
            }
            ui.add_space(10.0);
            wrapped(
                ui,
                gettext(
                    locale,
                    "Sign in through your browser. Spotifast never sees your password. Local playback needs Spotify Premium.",
                ),
                theme::regular(12.5),
                palette.secondary,
            );
            if !steps {
                ui.add_space(4.0);
                if theme::link(
                    ui,
                    gettext(locale, "How to set up your Spotify app"),
                    theme::regular(13.0),
                    palette.secondary,
                )
                .clicked()
                {
                    ctx.data_mut(|data| data.insert_temp(egui::Id::new(STEPS_OPEN), true));
                }
            }
        }
    }
    let proxy_id = egui::Id::new("login-proxy-open");
    let mut proxy_open = ui
        .data(|data| data.get_temp::<bool>(proxy_id))
        .unwrap_or(false);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            if theme::link(
                ui,
                gettext(locale, "Proxy Settings"),
                theme::regular(13.0),
                palette.secondary,
            )
            .clicked()
            {
                proxy_open = !proxy_open;
            }
        });
    });
    ui.add_space(4.0);
    if proxy_open {
        proxy_fields(ui, app);
    }
    ui.data_mut(|data| data.insert_temp(proxy_id, proxy_open));
}

/// The listener's own app's Client ID, kept in settings as Settings keeps it.
fn client_id_field(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let mut client_id = app.settings.web_client_id.clone().unwrap_or_default();
    let response = Frame::new()
        .fill(palette.surface)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 9))
        .show(ui, |ui| {
            super::widgets::text_edit(
                ui,
                locale,
                egui::TextEdit::singleline(&mut client_id)
                    .id(egui::Id::new("login-client-id"))
                    .hint_text(egui::RichText::new(gettext(locale, "Client ID")).color(palette.dim))
                    .font(theme::regular(14.0))
                    .frame(egui::Frame::NONE)
                    .desired_width(ui.available_width()),
            )
        })
        .inner;
    if response.changed() {
        let trimmed = client_id.trim().to_string();
        app.settings.web_client_id = (!trimmed.is_empty()).then_some(trimmed);
        app.mark_settings_dirty();
    }
    let typed = client_id.trim();
    if !typed.is_empty() && !crate::auth::looks_like_client_id(typed) {
        wrapped(
            ui,
            gettext(
                locale,
                "A Client ID is 32 letters and digits. Copy it from your app's page.",
            ),
            theme::regular(12.0),
            palette.danger,
        );
    }
}

/// One numbered step of setting up an app.
fn step(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    number: usize,
    text: &str,
    extra: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let (badge, _) = ui.allocate_exact_size(Vec2::splat(22.0), egui::Sense::hover());
        ui.painter().circle_filled(
            badge.center(),
            11.0,
            super::blend(palette.panel, palette.accent, 0.18),
        );
        ui.painter().text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            number.to_string(),
            theme::bold(12.0),
            palette.accent,
        );
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.add_space(2.0);
            wrapped(ui, text, theme::regular(13.5), palette.text);
            extra(ui);
        });
    });
    ui.add_space(4.0);
}

/// How to create the app Spotifast signs in with, the same steps the
/// guide gives (make-it-even-faster.md, Make a Spotify app).
fn setup_steps(app: &mut App, ui: &mut egui::Ui, width: f32) {
    let palette = app.palette;
    let locale = app.locale;
    let ctx = ui.ctx().clone();
    ui.set_width(width);
    ui.spacing_mut().item_spacing.y = 6.0;
    ui.add_space(4.0);
    theme::text(
        ui,
        gettext(locale, "Set up your Spotify app"),
        theme::bold(18.0),
        palette.text,
    );
    wrapped(
        ui,
        gettext(
            locale,
            "Spotifast signs in through your own free Spotify app. Spotify requires its owner to have Premium.",
        ),
        theme::regular(13.0),
        palette.secondary,
    );
    ui.add_space(6.0);
    step(
        ui,
        &palette,
        1,
        &gettext(locale, "Open Spotify's developer dashboard."),
        |ui| {
            if theme::link(
                ui,
                "developer.spotify.com/dashboard",
                theme::medium(13.0),
                palette.accent,
            )
            .on_hover_text(DASHBOARD_URL)
            .clicked()
            {
                ctx.open_url(egui::OpenUrl::new_tab(DASHBOARD_URL));
            }
        },
    );
    step(
        ui,
        &palette,
        2,
        &gettext(locale, "Click Create app. Any name will do."),
        |_| {},
    );
    step(
        ui,
        &palette,
        3,
        &gettext(locale, "Under Redirect URIs, add exactly:"),
        |ui| {
            redirect_uri_row(app, ui);
        },
    );
    step(
        ui,
        &palette,
        4,
        &gettext(locale, "Tick Web API, accept the terms, and save."),
        |_| {},
    );
    step(
        ui,
        &palette,
        5,
        &gettext(
            locale,
            "Paste the app's Client ID into the field and sign in.",
        ),
        |_| {},
    );
}

/// The redirect URI to paste, with a button that copies it.
fn redirect_uri_row(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let uri = crate::auth::personal_redirect_uri();
    let copied_id = egui::Id::new("login-redirect-copied");
    let now = ui.input(|input| input.time);
    let copied = ui
        .data(|data| data.get_temp::<f64>(copied_id))
        .is_some_and(|at| now - at < 2.0);
    // A narrow column puts the button under the address.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
        Frame::new()
            .fill(palette.surface)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&uri)
                            .font(egui::FontId::monospace(12.5))
                            .color(palette.text),
                    )
                    .selectable(true),
                );
            });
        let label = if copied {
            // Translators: shown on the Copy button for a moment after it copied.
            gettext(locale, "Copied")
        } else {
            gettext(locale, "Copy")
        };
        if theme::pill_button(ui, &palette, &label, false).clicked() {
            ui.ctx().copy_text(uri.clone());
            ui.data_mut(|data| data.insert_temp(copied_id, now));
        }
    });
    if copied {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }
}

/// The card: `base` tall around the window's centre, as it has always been,
/// and extended downwards when the `content` it measured last frame needs
/// more, up to the `room` the window has.
fn card_rect(rect: Rect, width: f32, base: f32, content: Option<f32>, room: f32) -> Rect {
    // The Frame's inner margin and its one-point stroke, above and below.
    const MARGINS: f32 = 74.0;
    let height = content
        .map_or(base, |content| content + MARGINS)
        .max(base)
        .min(room);
    // Past the room it would have, slide up rather than over the version line.
    let top = (rect.center().y - 20.0 - base / 2.0).min(rect.bottom() - 52.0 - height);
    Rect::from_min_size(
        pos2(rect.center().x - width / 2.0, top),
        Vec2::new(width, height),
    )
}

fn proxy_fields(ui: &mut egui::Ui, app: &mut App) {
    let palette = app.palette;
    let mut changed = false;
    let mut apply = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let row_width: f32 = ProxyMode::ALL
            .iter()
            .map(|choice| {
                let galley = ui.painter().layout_no_wrap(
                    choice.label(app.locale).into_owned(),
                    theme::medium(13.0),
                    palette.text,
                );
                galley.size().x + 24.0
            })
            .sum::<f32>()
            + 6.0 * (ProxyMode::ALL.len() - 1) as f32;
        ui.add_space((ui.available_width() - row_width).max(0.0) / 2.0);
        for choice in ProxyMode::ALL {
            if theme::soft_button(
                ui,
                &palette,
                None,
                &choice.label(app.locale),
                app.settings.proxy_mode == choice,
            )
            .clicked()
                && app.settings.proxy_mode != choice
            {
                app.settings.proxy_mode = choice;
                changed = true;
                apply = !choice.is_manual();
            }
        }
    });
    if app.settings.proxy_mode.is_manual() {
        ui.add_space(10.0);
        if super::widgets::proxy_manual_form(
            ui,
            &palette,
            app.locale,
            &mut app.settings.proxy_host,
            &mut app.settings.proxy_port,
            &mut app.settings.proxy_username,
            &mut app.settings.proxy_password,
        ) {
            changed = true;
        }
        ui.add_space(6.0);
        super::widgets::proxy_scope_note(ui, &palette, app.locale, app.settings.proxy_mode);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if theme::pill_button(ui, &palette, &gettext(app.locale, "Apply settings"), true)
                .clicked()
            {
                apply = true;
            }
        });
    }
    if changed {
        app.actions.push(Action::ProxyEdited);
        app.mark_settings_dirty();
    }
    if apply {
        app.actions.push(Action::ApplyProxy);
    }
}

fn big_button(ui: &mut egui::Ui, app: &App, label: &str) -> bool {
    let palette = app.palette;
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), theme::bold(15.0), palette.on_accent);
    let size = Vec2::new(ui.available_width().min(300.0), 46.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let fill = if !ui.is_enabled() {
        palette.accent.gamma_multiply(0.45)
    } else if response.hovered() {
        palette.accent_hover
    } else {
        palette.accent
    };
    ui.painter().rect_filled(rect, 23.0, fill);
    ui.painter().galley(
        rect.center() - galley.size() / 2.0,
        galley,
        palette.on_accent,
    );
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::AppOptions, backend::Waker, paths::AppDirs, settings::Settings};

    /// An account check offers Cancel, and a rate-limited one says why it is
    /// waiting (#861). A signed-in account still loading offers neither.
    #[test]
    fn the_account_check_offers_cancel_and_explains_a_rate_limit() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        theme::install(&ctx);
        let root = std::env::temp_dir().join(format!(
            "spotifast-rate-limited-login-{}",
            std::process::id()
        ));
        let mut app = App::new(
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
        );
        let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 620.0));
        let frame = |app: &mut App| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ui| show(app, ui, true),
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        };
        let cancel = |tree: &egui::accesskit::TreeUpdate| {
            tree.nodes.iter().any(|(_, node)| {
                node.label() == Some("Cancel") && node.role() == egui::accesskit::Role::Button
            })
        };
        let notice = |tree: &egui::accesskit::TreeUpdate| {
            tree.nodes.iter().any(|(_, node)| {
                node.value()
                    .or(node.label())
                    .is_some_and(|text| text.contains("finishes on its own"))
            })
        };

        app.auth = AuthStatus::RateLimited;
        let tree = frame(&mut app);
        assert!(cancel(&tree));
        assert!(notice(&tree));

        // Server errors retry without a notice, and can still be stopped.
        app.auth = AuthStatus::Connecting;
        let tree = frame(&mut app);
        assert!(cancel(&tree));
        assert!(!notice(&tree));

        // A signed-in account loading its profile is not a sign-in.
        app.auth = AuthStatus::Connected {
            username: "listener".into(),
        };
        let tree = frame(&mut app);
        assert!(!cancel(&tree));
        assert!(!notice(&tree));
    }

    #[test]
    fn expanded_proxy_settings_remain_reachable_in_a_short_window() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        theme::install(&ctx);
        let root = std::env::temp_dir().join(format!(
            "spotifast-proxy-short-login-{}",
            std::process::id()
        ));
        let mut app = App::new(
            &Waker::default(),
            AppDirs {
                config: root.join("config"),
                state: root.join("state"),
                cache: root.join("cache"),
            },
            Settings {
                proxy_mode: ProxyMode::Http,
                ..Default::default()
            },
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
                autostart: false,
            },
        );
        app.auth = AuthStatus::SignedOut;
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("login-proxy-open"), true));
        let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(760.0, 520.0));
        let mut frame = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| show(&mut app, ui, false),
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        };
        let visible_button = |tree: &egui::accesskit::TreeUpdate, label: &str| {
            let bounds = tree
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.label() == Some(label) && node.role() == egui::accesskit::Role::Button
                })
                .unwrap_or_else(|| panic!("missing accessible {label}"))
                .1
                .bounds()
                .unwrap();
            bounds.y0 >= 0.0 && bounds.y1 <= 480.0
        };
        let first = frame(vec![]);
        assert!(visible_button(&first, "Sign in with Spotify"));
        let center = |label: &str| {
            let bounds = first
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .unwrap_or_else(|| panic!("missing accessible {label}"))
                .1
                .bounds()
                .unwrap();
            (bounds.x0 + bounds.x1) / 2.0
        };
        assert!(
            (center("Proxy Settings") - center("Sign in with Spotify")).abs() < 1.0,
            "the proxy link stays centered under sign-in"
        );
        frame(vec![
            egui::Event::PointerMoved(egui::pos2(400.0, 250.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                phase: egui::TouchPhase::Move,
                delta: egui::vec2(0.0, -1000.0),
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        for _ in 0..30 {
            frame(vec![]);
        }
        assert!(
            visible_button(&frame(vec![]), "Apply settings"),
            "scrolling must reach Apply above the footer"
        );
    }
}
