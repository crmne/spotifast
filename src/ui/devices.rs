//! The Spotify Connect device picker.

use egui::{Align, CornerRadius, Layout, Rect, Sense, pos2, vec2};

use crate::api::models::Device;
use crate::app::App;
use crate::model::Action;
use crate::theme::{self, Icon};

pub const BUTTON_RECT_ID: &str = "devices-button-rect";

fn receiver_is_listed(receiver: &crate::zeroconf::Receiver, devices: &[Device]) -> bool {
    devices
        .iter()
        .any(|device| match receiver.device_id.as_deref() {
            Some(id) => device.id.as_deref() == Some(id),
            None => device.name == receiver.name,
        })
}

fn name_devices(devices: &mut [Device], receivers: &[crate::zeroconf::Receiver]) {
    for device in devices {
        if device.id.is_some()
            && let Some(receiver) = receivers
                .iter()
                .find(|receiver| receiver.device_id == device.id)
        {
            device.name.clone_from(&receiver.name);
        }
    }
}

pub fn device_icon(kind: &str) -> Icon {
    match kind.to_ascii_lowercase().as_str() {
        "computer" => Icon::Laptop,
        "smartphone" => Icon::Smartphone,
        "tablet" => Icon::Tablet,
        "tv" => Icon::Tv,
        "game_console" => Icon::Gamepad,
        "automobile" => Icon::Car,
        "cast_video" | "cast_audio" | "castaudio" | "castvideo" => Icon::Cast,
        "smartwatch" => Icon::Watch,
        "avr" | "stb" | "audio_dongle" => Icon::Monitor,
        _ => Icon::Speaker,
    }
}

/// A row offering to authorize local playback on this computer.
fn enable_playback_row(app: &mut App, ui: &mut egui::Ui) {
    use egui::{Rect, Sense, Vec2, pos2, vec2};
    let palette = app.palette;
    let authorizing = matches!(
        app.local_playback,
        crate::backend::LocalPlayback::Authorizing | crate::backend::LocalPlayback::Connecting
    );
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
    if response.hovered() && !authorizing {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(6), palette.surface_hover);
    }
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 24.0, rect.center().y), Vec2::splat(22.0));
    Icon::Laptop
        .image(palette.text, 22.0)
        .paint_at(ui, icon_rect);
    let painter = ui.painter().with_clip_rect(rect);
    painter.text(
        pos2(rect.left() + 48.0, rect.center().y - 9.0),
        egui::Align2::LEFT_CENTER,
        format!("{} (this computer)", app.settings.device_name),
        theme::medium(14.0),
        palette.text,
    );
    painter.text(
        pos2(rect.left() + 48.0, rect.center().y + 10.0),
        egui::Align2::LEFT_CENTER,
        if authorizing {
            "Setting up…"
        } else {
            "Set up playback here"
        },
        theme::regular(12.0),
        palette.accent,
    );
    if authorizing {
        let mut spin = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_center_size(
                    pos2(rect.right() - 18.0, rect.center().y),
                    Vec2::splat(20.0),
                ))
                .layout(egui::Layout::centered_and_justified(
                    egui::Direction::LeftToRight,
                )),
        );
        theme::spinner(&mut spin, 16.0, palette.accent);
    }
    if response.clicked() && !authorizing {
        app.actions.push(Action::EnablePlayback);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.painter().hline(
        rect.x_range().shrink(6.0),
        rect.bottom(),
        egui::Stroke::new(1.0, palette.outline),
    );
    ui.add_space(4.0);
}

/// A receiver announced on the local network but not yet in the account.
/// Choosing it hands over the account, after which it behaves like any other
/// Spotify Connect device.
fn receiver_row(app: &mut App, ui: &mut egui::Ui, receiver: &crate::zeroconf::Receiver) {
    use egui::{Rect, Sense, Vec2, pos2, vec2};
    let palette = app.palette;
    let activating = app.activating_receiver.as_deref() == Some(receiver.name.as_str());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
    if response.hovered() && !activating {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(6), palette.surface_hover);
    }
    let icon_rect =
        Rect::from_center_size(pos2(rect.left() + 24.0, rect.center().y), Vec2::splat(22.0));
    Icon::Speaker
        .image(palette.text, 22.0)
        .paint_at(ui, icon_rect);
    let painter = ui.painter().with_clip_rect(rect);
    crate::bidi::paint_line(
        &painter,
        rect.left() + 48.0,
        rect.right() - 12.0,
        rect.center().y - 9.0,
        &receiver.name,
        theme::medium(14.0),
        palette.text,
    );
    painter.text(
        pos2(rect.left() + 48.0, rect.center().y + 10.0),
        egui::Align2::LEFT_CENTER,
        if activating {
            "Connecting…"
        } else {
            "On your network, click to connect"
        },
        theme::regular(12.0),
        palette.secondary,
    );
    if activating {
        let mut spin = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_center_size(
                    pos2(rect.right() - 18.0, rect.center().y),
                    Vec2::splat(20.0),
                ))
                .layout(egui::Layout::centered_and_justified(
                    egui::Direction::LeftToRight,
                )),
        );
        theme::spinner(&mut spin, 16.0, palette.accent);
    }
    if response.clicked() && !activating {
        app.actions
            .push(Action::ActivateReceiver(Box::new(receiver.clone())));
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
}

pub fn popup(app: &mut App, ctx: &egui::Context) {
    if !app.show_devices {
        return;
    }
    let palette = app.palette;
    let button = ctx
        .data(|data| data.get_temp::<Rect>(egui::Id::new(BUTTON_RECT_ID)))
        .unwrap_or_else(|| Rect::from_min_size(pos2(400.0, 400.0), vec2(0.0, 0.0)));
    let width = 320.0;
    let position = pos2(
        (button.right() - width).max(8.0),
        (button.top() - 12.0).max(8.0),
    );
    let area = egui::Area::new(egui::Id::new("devices-popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(position)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .show(ctx, |ui| {
            super::widgets::menu_frame(&palette).show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    theme::text(ui, "Connect to a device", theme::bold(16.0), palette.text);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if app.devices_loading {
                            theme::spinner(ui, 16.0, palette.accent);
                        } else if theme::icon_button(
                            ui,
                            Icon::Refresh,
                            15.0,
                            palette.secondary,
                            palette.text,
                            "Refresh",
                        )
                        .clicked()
                        {
                            app.actions.push(Action::RefreshDevices);
                        }
                    });
                });
                ui.add_space(4.0);
                let local_id = app.local_device_id.clone();
                let mut devices: Vec<Device> = app.devices.clone();
                if app.local_ready
                    && let Some(local_id) = &local_id
                    && !devices
                        .iter()
                        .any(|device| device.id.as_deref() == Some(local_id.as_str()))
                {
                    devices.insert(
                        0,
                        Device {
                            id: Some(local_id.clone()),
                            name: format!("{} (this computer)", app.settings.device_name),
                            is_active: app.local.is_active(),
                            is_restricted: false,
                            volume_percent: Some(crate::app::volume_to_percent(app.local.volume)),
                            supports_volume: Some(true),
                            kind: "computer".into(),
                        },
                    );
                }
                let active_id = match app.target() {
                    crate::app::Target::Local => local_id.clone(),
                    crate::app::Target::Remote(id) => id,
                };
                devices.sort_by_key(|device| device.id != active_id);
                name_devices(&mut devices, &app.receivers);
                let mut seen = std::collections::HashSet::new();
                devices
                    .retain(|device| device.id.as_ref().is_none_or(|id| seen.insert(id.clone())));

                // Receivers on the network that Spotify has not listed yet.
                // They are real speakers the user can see in the official
                // client, so offering them here is the whole point.
                let waiting: Vec<crate::zeroconf::Receiver> = app
                    .receivers
                    .iter()
                    .filter(|receiver| !receiver_is_listed(receiver, &devices))
                    .cloned()
                    .collect();

                let max_height = (position.y - ctx.content_rect().top() - 62.0).clamp(52.0, 416.0);
                crate::autoscroll::show(
                    ui,
                    egui::ScrollArea::vertical()
                        .id_salt("connect-device-list")
                        // Grow past the Area's remembered height when discovery
                        // adds devices. Short lists still shrink to their contents.
                        .min_scrolled_height(max_height)
                        .max_height(max_height),
                    egui::Vec2b::new(false, true),
                    |ui| {
                        if !app.local_ready {
                            enable_playback_row(app, ui);
                        }
                        if devices.is_empty() && waiting.is_empty() && app.local_ready {
                            ui.add_space(8.0);
                            theme::subtle(
                                ui,
                                &palette,
                                "No devices found. Open Spotify on another device, then refresh.",
                            );
                            ui.add_space(8.0);
                        }

                        for device in &devices {
                            let is_local = device.id.is_some() && device.id == local_id;
                            let active = device.id.is_some() && device.id == active_id;
                            let name = if is_local && !device.name.contains("this computer") {
                                format!("{} (this computer)", device.name)
                            } else {
                                device.name.clone()
                            };
                            let (rect, response) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 52.0),
                                Sense::click(),
                            );
                            if response.hovered() {
                                ui.painter().rect_filled(
                                    rect,
                                    CornerRadius::same(6),
                                    palette.surface_hover,
                                );
                            }
                            let color = if active { palette.accent } else { palette.text };
                            let icon_rect = Rect::from_center_size(
                                pos2(rect.left() + 24.0, rect.center().y),
                                egui::Vec2::splat(22.0),
                            );
                            device_icon(&device.kind)
                                .image(color, 22.0)
                                .paint_at(ui, icon_rect);
                            let painter = ui.painter().with_clip_rect(rect);
                            crate::bidi::paint_line(
                                &painter,
                                rect.left() + 48.0,
                                rect.right() - 12.0,
                                rect.center().y - 9.0,
                                &name,
                                theme::medium(14.0),
                                color,
                            );
                            let status = if active {
                                "Listening on this device".to_string()
                            } else if device.is_restricted {
                                "Restricted".to_string()
                            } else if is_local {
                                "Play here".to_string()
                            } else {
                                device.kind.replace('_', " ")
                            };
                            painter.text(
                                pos2(rect.left() + 48.0, rect.center().y + 10.0),
                                egui::Align2::LEFT_CENTER,
                                status,
                                theme::regular(12.0),
                                if active {
                                    palette.accent
                                } else {
                                    palette.secondary
                                },
                            );
                            if active {
                                let dot = pos2(rect.right() - 16.0, rect.center().y);
                                ui.painter().circle_filled(dot, 4.0, palette.accent);
                            }
                            if response.clicked()
                                && !active
                                && let Some(id) = &device.id
                            {
                                app.actions.push(Action::Transfer(id.clone()));
                            }
                            response.on_hover_cursor(egui::CursorIcon::PointingHand);
                        }
                        for receiver in &waiting {
                            receiver_row(app, ui, receiver);
                        }
                    },
                );
            });
        });
    let popup_rect = area.response.rect;
    let clicked_outside = ctx.input(|input| {
        input.pointer.any_pressed()
            && input
                .pointer
                .interact_pos()
                .is_some_and(|pos| !popup_rect.contains(pos) && !button.contains(pos))
    });
    if clicked_outside {
        app.show_devices = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receivers_identity_matches_even_when_its_advertised_name_differs() {
        let receiver = crate::zeroconf::Receiver {
            name: "SpotifyConnect".into(),
            device_id: Some("living-room".into()),
            address: "127.0.0.1".parse().unwrap(),
            port: 80,
            path: "/".into(),
        };
        let mut device = Device {
            id: Some("living-room".into()),
            name: "Living Room".into(),
            ..Default::default()
        };
        assert!(receiver_is_listed(&receiver, &[device.clone()]));
        let mut named = [Device {
            name: "unnamed".into(),
            ..device.clone()
        }];
        name_devices(&mut named, std::slice::from_ref(&receiver));
        assert_eq!(named[0].name, receiver.name);
        assert_eq!(
            named[0].id, device.id,
            "renaming preserves the transfer target"
        );
        device.id = Some("other-room".into());
        device.name = receiver.name.clone();
        assert!(
            !receiver_is_listed(&receiver, &[device]),
            "two identities may have the same name"
        );
    }
}
