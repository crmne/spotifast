//! The Sleep Timer popup.

use std::time::{Duration, Instant};

use egui::{Align, CornerRadius, Layout, Rect, Sense, pos2, vec2};

use crate::app::App;
use crate::model::{Action, SleepTimerSetting};
use crate::theme::{self, Icon};

#[derive(Clone, Debug)]
pub struct SleepTimer {
    pub setting: SleepTimerSetting,
    pub deadline: Option<Instant>,
    pub initial_track_uri: Option<String>,
    pub wait_for_song_end: bool,
    pub waiting_for_song_end: bool,
}

impl SleepTimer {
    pub fn new(
        setting: SleepTimerSetting,
        current_track_uri: Option<String>,
        wait_for_song_end: bool,
    ) -> Self {
        let deadline = match setting {
            SleepTimerSetting::Duration(d) => Some(Instant::now() + d),
            SleepTimerSetting::EndOfTrack => None,
        };
        Self {
            setting,
            deadline,
            initial_track_uri: current_track_uri,
            wait_for_song_end,
            waiting_for_song_end: false,
        }
    }

    pub fn remaining_duration(&self) -> Option<Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    pub fn status_text(&self) -> String {
        if self.waiting_for_song_end {
            return "Sleep timer: waiting for song to finish".to_string();
        }
        match self.setting {
            SleepTimerSetting::EndOfTrack => "Sleep timer: end of track".to_string(),
            SleepTimerSetting::Duration(_) => {
                if let Some(rem) = self.remaining_duration() {
                    let total_secs = rem.as_secs();
                    let mins = total_secs / 60;
                    let secs = total_secs % 60;
                    let suffix = if self.wait_for_song_end {
                        " (waits for song end)"
                    } else {
                        ""
                    };
                    if mins > 0 {
                        format!("Sleep timer: {mins}m {secs:02}s{suffix}")
                    } else {
                        format!("Sleep timer: {secs}s{suffix}")
                    }
                } else {
                    "Sleep timer".to_string()
                }
            }
        }
    }
}

pub const BUTTON_RECT_ID: &str = "sleep-timer-button-rect";

pub fn popup(app: &mut App, ctx: &egui::Context) {
    if !app.show_sleep_timer {
        return;
    }
    let palette = app.palette;
    let button = ctx
        .data(|data| data.get_temp::<Rect>(egui::Id::new(BUTTON_RECT_ID)))
        .unwrap_or_else(|| Rect::from_min_size(pos2(400.0, 400.0), vec2(0.0, 0.0)));
    let width = 280.0;
    let position = pos2(
        (button.right() - width).max(8.0),
        (button.top() - 12.0).max(8.0),
    );
    let area = egui::Area::new(egui::Id::new("sleep-timer-popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(position)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .show(ctx, |ui| {
            super::widgets::menu_frame(&palette).show(ui, |ui| {
                ui.set_width(width);
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    theme::icon(ui, Icon::Moon, 18.0, palette.accent);
                    ui.add_space(4.0);
                    theme::text(ui, "Sleep timer", theme::bold(16.0), palette.text);
                    if let Some(ref timer) = app.sleep_timer {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let text = match &timer.setting {
                                SleepTimerSetting::EndOfTrack => "End of track".to_string(),
                                SleepTimerSetting::Duration(_) => {
                                    if let Some(rem) = timer.remaining_duration() {
                                        let secs = rem.as_secs();
                                        format!("{}m {:02}s", secs / 60, secs % 60)
                                    } else {
                                        "".to_string()
                                    }
                                }
                            };
                            theme::text(ui, text, theme::medium(12.0), palette.accent);
                        });
                    }
                });

                ui.add_space(6.0);
                ui.painter().hline(
                    ui.max_rect().x_range().shrink(4.0),
                    ui.cursor().top(),
                    egui::Stroke::new(1.0, palette.outline),
                );
                ui.add_space(6.0);

                // Option: Wait for song to finish
                let wait_active = app.sleep_timer_wait_song_end;
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
                if response.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), palette.surface_hover);
                }
                let check_icon = if wait_active {
                    Icon::Check
                } else {
                    Icon::Minus
                };
                let check_color = if wait_active {
                    palette.accent
                } else {
                    palette.dim
                };
                let icon_rect = Rect::from_center_size(
                    pos2(rect.left() + 18.0, rect.center().y),
                    egui::Vec2::splat(16.0),
                );
                check_icon.image(check_color, 16.0).paint_at(ui, icon_rect);

                let painter = ui.painter().with_clip_rect(rect);
                painter.text(
                    pos2(rect.left() + 38.0, rect.center().y - 8.0),
                    egui::Align2::LEFT_CENTER,
                    "Wait for song to finish",
                    theme::medium(13.5),
                    if wait_active {
                        palette.text
                    } else {
                        palette.secondary
                    },
                );
                painter.text(
                    pos2(rect.left() + 38.0, rect.center().y + 10.0),
                    egui::Align2::LEFT_CENTER,
                    "Let the current song finish before stopping",
                    theme::regular(11.0),
                    palette.dim,
                );
                if response.clicked() {
                    app.actions.push(Action::ToggleSleepTimerWaitSongEnd);
                }
                response.on_hover_cursor(egui::CursorIcon::PointingHand);

                ui.add_space(4.0);
                ui.painter().hline(
                    ui.max_rect().x_range().shrink(4.0),
                    ui.cursor().top(),
                    egui::Stroke::new(1.0, palette.outline),
                );
                ui.add_space(4.0);

                let presets = [
                    (
                        "5 minutes",
                        SleepTimerSetting::Duration(Duration::from_secs(5 * 60)),
                    ),
                    (
                        "10 minutes",
                        SleepTimerSetting::Duration(Duration::from_secs(10 * 60)),
                    ),
                    (
                        "15 minutes",
                        SleepTimerSetting::Duration(Duration::from_secs(15 * 60)),
                    ),
                    (
                        "30 minutes",
                        SleepTimerSetting::Duration(Duration::from_secs(30 * 60)),
                    ),
                    (
                        "45 minutes",
                        SleepTimerSetting::Duration(Duration::from_secs(45 * 60)),
                    ),
                    (
                        "1 hour",
                        SleepTimerSetting::Duration(Duration::from_secs(60 * 60)),
                    ),
                    ("End of track", SleepTimerSetting::EndOfTrack),
                ];

                for (label, setting) in presets {
                    let active = app
                        .sleep_timer
                        .as_ref()
                        .is_some_and(|timer| timer.setting == setting);
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                    if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            palette.surface_hover,
                        );
                    }
                    if active {
                        let icon_rect = Rect::from_center_size(
                            pos2(rect.left() + 18.0, rect.center().y),
                            egui::Vec2::splat(15.0),
                        );
                        Icon::Check
                            .image(palette.accent, 15.0)
                            .paint_at(ui, icon_rect);
                    }
                    let text_color = if active { palette.accent } else { palette.text };
                    ui.painter().text(
                        pos2(rect.left() + 38.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        theme::medium(13.5),
                        text_color,
                    );
                    if response.clicked() {
                        app.actions.push(Action::SetSleepTimer(Some(setting)));
                    }
                    response.on_hover_cursor(egui::CursorIcon::PointingHand);
                }

                if app.sleep_timer.is_some() {
                    ui.add_space(4.0);
                    ui.painter().hline(
                        ui.max_rect().x_range().shrink(4.0),
                        ui.cursor().top(),
                        egui::Stroke::new(1.0, palette.outline),
                    );
                    ui.add_space(4.0);

                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
                    if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            palette.surface_hover,
                        );
                    }
                    let icon_rect = Rect::from_center_size(
                        pos2(rect.left() + 18.0, rect.center().y),
                        egui::Vec2::splat(15.0),
                    );
                    Icon::X
                        .image(palette.secondary, 15.0)
                        .paint_at(ui, icon_rect);
                    ui.painter().text(
                        pos2(rect.left() + 38.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "Turn off timer",
                        theme::medium(13.5),
                        palette.secondary,
                    );
                    if response.clicked() {
                        app.actions.push(Action::SetSleepTimer(None));
                    }
                    response.on_hover_cursor(egui::CursorIcon::PointingHand);
                }
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
        app.show_sleep_timer = false;
    }
}
