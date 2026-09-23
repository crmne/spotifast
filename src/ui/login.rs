//! The sign-in screen.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Rect, Stroke, Vec2, pos2};

use crate::app::App;
use crate::backend::AuthStatus;
use crate::model::Action;
use crate::settings::ProxyMode;
use crate::theme;

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
                    Vec2::new(rect.width(), theme::TOP_BAR_HEIGHT + theme::titlebar_inset(ui.ctx())),
                )
            } else {
                rect
            };
            super::titlebar_drag(ui, drag_rect);
            let top = super::blend(palette.window, palette.accent, 0.10);
            super::widgets::paint_vertical_gradient(ui, rect, top, palette.window);
            let card_width = 440.0;
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
            let card_height = card_height.min((rect.height() - 64.0).max(0.0));
            let card = Rect::from_center_size(
                rect.center() - Vec2::new(0.0, 20.0),
                Vec2::new(card_width, card_height),
            );
            let mut card_ui = ui.new_child(egui::UiBuilder::new().max_rect(card).layout(Layout::top_down(Align::Center)));
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
                    egui::ScrollArea::vertical()
                        .id_salt("login-card-scroll")
                        .max_height((card.height() - 72.0).max(0.0))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                    ui.set_width(card_width - 72.0);
                    ui.spacing_mut().item_spacing.y = 8.0;
                    let (logo, _) = ui.allocate_exact_size(Vec2::splat(72.0), egui::Sense::hover());
                    theme::logo(ui, logo.center(), 72.0, palette.accent, palette.on_accent);
                    ui.add_space(6.0);
                    theme::text(ui, "Spotifast", theme::bold(30.0), palette.text);
                    theme::text(ui, "A native Spotify client.", theme::regular(14.5), palette.secondary);
                    ui.add_space(22.0);
                    match &app.auth {
                        AuthStatus::WaitingForBrowser { url } => {
                            let url = url.clone();
                            ui.horizontal(|ui| {
                                ui.add_space((ui.available_width() - 250.0).max(0.0) / 2.0);
                                theme::spinner(ui, 18.0, palette.accent);
                                theme::text(ui, "Waiting for Spotify in your browser…", theme::medium(14.0), palette.text);
                            });
                            ui.add_space(6.0);
                            if theme::link(ui, "Didn't open? Open the sign-in page again", theme::regular(13.0), palette.secondary).clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(url));
                            }
                            ui.add_space(14.0);
                            if theme::pill_button(ui, &palette, "Cancel", false).clicked() {
                                app.actions.push(Action::CancelSignIn);
                            }
                        }
                        _ if connecting => {
                            ui.horizontal(|ui| {
                                ui.add_space((ui.available_width() - 200.0).max(0.0) / 2.0);
                                theme::spinner(ui, 18.0, palette.accent);
                                theme::text(ui, "Connecting to Spotify…", theme::medium(14.0), palette.text);
                            });
                        }
                        AuthStatus::Failed(message) => {
                            let message = message.clone();
                            ui.add(
                                egui::Label::new(egui::RichText::new(message).font(theme::regular(13.0)).color(palette.danger)).wrap(),
                            );
                            ui.add_space(12.0);
                            if big_button(ui, app, "Try again") {
                                app.actions.push(Action::SignIn);
                            }
                            if app.settings.web_client_id.is_some() {
                                ui.add_space(10.0);
                                if theme::pill_button(
                                    ui,
                                    &palette,
                                    "Use the shared Spotify app instead",
                                    false,
                                )
                                .clicked()
                                {
                                    // A wrong personal Client ID trapped the
                                    // user here with Settings out of reach.
                                    app.settings.web_client_id = None;
                                    app.mark_settings_dirty();
                                    app.actions.push(Action::ConfigurePersonalWebApp);
                                }
                            }
                        }
                        _ => {
                            if big_button(ui, app, "Sign in with Spotify") {
                                app.actions.push(Action::SignIn);
                            }
                            ui.add_space(10.0);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new("Sign in through your browser. Spotifast never sees your password. Local playback needs Spotify Premium.")
                                        .font(theme::regular(12.5))
                                        .color(palette.secondary),
                                )
                                .wrap(),
                            );
                            if app.settings.web_client_id.is_some() {
                                // A wrong personal Client ID dead-ends in the
                                // browser on Spotify's side, so the app never
                                // hears it failed; the way out has to stand
                                // here, not only on the failure screen.
                                ui.add_space(10.0);
                                if theme::pill_button(
                                    ui,
                                    &palette,
                                    "Use the shared Spotify app instead",
                                    false,
                                )
                                .clicked()
                                {
                                    app.settings.web_client_id = None;
                                    app.mark_settings_dirty();
                                    app.actions.push(Action::ConfigurePersonalWebApp);
                                }
                            }
                        }
                    }
                    let mut proxy_open = ui.data(|data| data.get_temp::<bool>(proxy_id)).unwrap_or(false);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(Layout::top_down(Align::Center), |ui| {
                            if theme::link(
                                ui,
                                "Proxy Settings",
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
                        });
                });
            ui.painter().text(
                pos2(rect.center().x, rect.bottom() - 24.0),
                egui::Align2::CENTER_BOTTOM,
                format!("Spotifast {} • not affiliated with Spotify", env!("CARGO_PKG_VERSION")),
                theme::regular(11.5),
                palette.dim,
            );
        });
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
                    choice.label().to_string(),
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
                choice.label(),
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
            &mut app.settings.proxy_host,
            &mut app.settings.proxy_port,
            &mut app.settings.proxy_username,
            &mut app.settings.proxy_password,
        ) {
            changed = true;
        }
        ui.add_space(6.0);
        super::widgets::proxy_scope_note(ui, &palette, app.settings.proxy_mode);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if theme::pill_button(ui, &palette, "Apply settings", true).clicked() {
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
    let fill = if response.hovered() {
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
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::AppOptions, backend::Waker, paths::AppDirs, settings::Settings};

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
