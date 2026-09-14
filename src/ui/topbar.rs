//! Navigation arrows, search, and the account menu above every page.

use std::sync::Arc;

use egui::{Align, CornerRadius, Galley, Layout, Sense, Vec2, pos2, vec2};

use crate::api::models::pick_image;
use crate::app::App;
use crate::model::{Action, Page};
use crate::theme::{self, Icon, Palette};

/// The gap the bar keeps between everything it lays out.
const ITEM_SPACING: f32 = 8.0;
/// The account avatar, and the icon in each of the three buttons beside it.
const AVATAR_SIZE: f32 = 36.0;
const ICON_BUTTON_ICON: f32 = 19.0;
/// `theme::icon_button` pads its icon by 12 px.
const ICON_BUTTON_SIZE: f32 = ICON_BUTTON_ICON + 12.0;
const SPINNER_SIZE: f32 = 15.0;
/// A badge is as tall as its text plus this, and as wide as its text plus
/// the padding its own label needs.
const BADGE_PADDING_Y: f32 = 12.0;
const DEVICE_BADGE_PADDING: f32 = 28.0;
/// The text starts 24 px in; leave 8 px after it to match the space before
/// the icon.
const UPDATE_BADGE_PADDING: f32 = 32.0;
/// The width the search field aims for, the most it ever takes, and the
/// least it shrinks to before the badges give up their labels instead.
const SEARCH_IDEAL: f32 = 200.0;
const SEARCH_MAX: f32 = 440.0;
const SEARCH_FLOOR: f32 = 130.0;
// After the badges collapse, a right panel can leave less than 130 points.
// Keep the original 80-point minimum inside the page's own toolbar.
const SEARCH_MIN: f32 = 80.0;
/// Everything at the right end whose width never changes: the page padding,
/// the avatar, the gap the account menu leaves, the three icon buttons, and
/// the spacing between them. The cursor stops at the left edge of the last
/// button, so this counts three gaps, not four. The spinner and the badges
/// are measured on top of it because they come and go.
const RIGHT_CONTROLS_WIDTH: f32 =
    super::widgets::PAGE_PADDING + AVATAR_SIZE + 4.0 + 3.0 * ICON_BUTTON_SIZE + 3.0 * ITEM_SPACING;

/// How the top bar divides itself for one window width.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TopbarFit {
    /// How wide the search field may be.
    search: f32,
    /// Whether the badges have the room to spell themselves out.
    labels: bool,
}

/// Divide the bar. The search field keeps the half it has always had, but
/// never so much that the right end has to reach over it, and the badges
/// fall back to their icons before the field shrinks past reading size.
///
/// `labelled` and `icons` are what the badges ask for with and without their
/// text, each already including the spacing that precedes it.
fn topbar_fit(room: f32, controls: f32, labelled: f32, icons: f32) -> TopbarFit {
    // SEARCH_IDEAL is above SEARCH_FLOOR, so the clamp below is well ordered.
    let ideal = (room * 0.5).clamp(SEARCH_IDEAL, SEARCH_MAX);
    let labels = room - controls - labelled >= SEARCH_FLOOR;
    let badges = if labels { labelled } else { icons };
    TopbarFit {
        search: (room - controls - badges).clamp(SEARCH_MIN, ideal),
        labels,
    }
}

/// What a badge asks of the bar, including the spacing before it.
fn badge_width(galley: Option<&Arc<Galley>>, padding: f32, labels: bool) -> f32 {
    galley.map_or(0.0, |galley| {
        ITEM_SPACING
            + if labels {
                galley.size().x + padding
            } else {
                galley.size().y + BADGE_PADDING_Y
            }
    })
}

/// A pill at the right end of the bar: an icon with its label, or the icon
/// alone once the bar is too narrow to spare the room for words.
fn badge(
    ui: &mut egui::Ui,
    palette: &Palette,
    icon: Icon,
    galley: Arc<Galley>,
    padding: f32,
    labels: bool,
) -> egui::Response {
    let height = galley.size().y + BADGE_PADDING_Y;
    let size = if labels {
        vec2(galley.size().x + padding, height)
    } else {
        Vec2::splat(height)
    };
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    // Collapsed to its icon the badge has no text on screen, so its label
    // reaches a screen reader as the widget's name.
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), galley.text())
    });
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(14),
        palette.accent.gamma_multiply(0.16),
    );
    let icon_center = if labels {
        pos2(rect.left() + 14.0, rect.center().y)
    } else {
        rect.center()
    };
    icon.image(palette.accent, 13.0).paint_at(
        ui,
        egui::Rect::from_center_size(icon_center, Vec2::splat(13.0)),
    );
    if labels {
        ui.painter().galley(
            pos2(rect.left() + 24.0, rect.center().y - galley.size().y / 2.0),
            galley,
            palette.accent,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn nav_button(
    ui: &mut egui::Ui,
    palette: &Palette,
    icon: Icon,
    enabled: bool,
    tooltip: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        Vec2::splat(32.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let fill = if palette.dark {
            egui::Color32::from_black_alpha(90)
        } else {
            egui::Color32::from_black_alpha(20)
        };
        ui.painter().circle_filled(rect.center(), 16.0, fill);
        let color = if !enabled {
            palette.dim
        } else if response.hovered() {
            palette.text
        } else {
            palette.secondary
        };
        theme::paint_icon(ui, icon, rect, 20.0, color);
    }
    if enabled {
        response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(tooltip)
    } else {
        response
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let width = ui.available_width();
    let window_controls = super::window_controls_reservation(
        ui.ctx(),
        app.show_queue_panel,
        app.show_lyrics_panel,
        width,
    );
    // Where the titlebar used to be: the bar grows upwards into that space and
    // its empty parts drag the window.
    let inset = theme::titlebar_inset(ui.ctx());
    let content_height = theme::TOP_BAR_HEIGHT + inset;
    if cfg!(windows) {
        super::titlebar_drag(
            ui,
            egui::Rect::from_min_size(
                ui.cursor().min,
                vec2(width, content_height + window_controls.topbar_top),
            ),
        );
    }
    ui.add_space(window_controls.topbar_top);
    ui.allocate_ui_with_layout(
        vec2(width, content_height),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(super::widgets::PAGE_PADDING);
            ui.spacing_mut().item_spacing.x = ITEM_SPACING;
            if !app.settings.sidebar_visible {
                if nav_button(
                    ui,
                    &palette,
                    Icon::PanelLeft,
                    true,
                    super::keys::platform_shortcut("Show sidebar (Ctrl+B)", "Show sidebar (Cmd+B)"),
                )
                .clicked()
                {
                    app.actions.push(Action::ToggleSidebar);
                }
                ui.add_space(2.0);
            }
            if !app.settings.sidebar_visible
                && nav_button(ui, &palette, Icon::House, true, "Home").clicked()
            {
                app.actions.push(Action::Open(Page::Home));
            }
            if nav_button(ui, &palette, Icon::ChevronLeft, app.can_go_back(), "Back").clicked() {
                app.actions.push(Action::Back);
            }
            if nav_button(
                ui,
                &palette,
                Icon::ChevronRight,
                app.can_go_forward(),
                "Forward",
            )
            .clicked()
            {
                app.actions.push(Action::Forward);
            }
            ui.add_space(8.0);

            // The badges sit at the right end but grow with their text, so
            // measure them here, before the search field takes its share.
            let device_galley = app.now_playing().filter(|now| !now.local).map(|now| {
                let label = format!(
                    "Playing on {}",
                    now.device_name.unwrap_or_else(|| "another device".into())
                );
                ui.painter()
                    .layout_no_wrap(label, theme::medium(12.5), palette.accent)
            });
            let update = app.update.clone();
            let update_galley = update.as_ref().map(|update| {
                let label = match &app.update_download {
                    crate::updates::DownloadState::Ready(_) => "Update ready".into(),
                    crate::updates::DownloadState::Downloading { .. } => {
                        "Downloading update…".into()
                    }
                    _ => format!("Update to {}", update.version),
                };
                ui.painter()
                    .layout_no_wrap(label, theme::medium(12.5), palette.accent)
            });
            // Ask once, so the bar reserves room for exactly the spinner it
            // then draws.
            let busy = app
                .backend
                .activity()
                .busy(std::time::Duration::from_millis(1000));
            let badges = |labels: bool| {
                badge_width(device_galley.as_ref(), DEVICE_BADGE_PADDING, labels)
                    + badge_width(update_galley.as_ref(), UPDATE_BADGE_PADDING, labels)
            };
            let controls = RIGHT_CONTROLS_WIDTH
                + if busy {
                    SPINNER_SIZE + ITEM_SPACING
                } else {
                    0.0
                };

            let search_room = (ui.available_width() - window_controls.topbar_width).max(0.0);
            let fit = topbar_fit(search_room, controls, badges(true), badges(false));
            let search_width = fit.search;
            let id = egui::Id::new("global-search");
            let before = app.search.query.clone();
            let response = super::widgets::search_field(
                ui,
                &palette,
                id,
                &mut app.search.query,
                "What do you want to play?",
                search_width,
            );
            if app.search.focus_requested {
                app.search.focus_requested = false;
                response.request_focus();
            }
            if response.gained_focus() && !matches!(app.page(), Page::Search) {
                app.actions.push(Action::Open(Page::Search));
            }
            if app.search.query != before {
                app.search.typed_at = Some(std::time::Instant::now());
                if !matches!(app.page(), Page::Search) {
                    app.actions.push(Action::Open(Page::Search));
                }
            }
            if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                let query = app.search.query.clone();
                app.actions.push(Action::Search(query));
            }
            if response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                response.surrender_focus();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(window_controls.topbar_width);
                ui.add_space(super::widgets::PAGE_PADDING);
                // Account.
                let (name, avatar) = app
                    .user
                    .as_ref()
                    .map(|user| {
                        (
                            user.name().to_string(),
                            pick_image(&user.images, 64).map(str::to_string),
                        )
                    })
                    .unwrap_or_default();
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::splat(AVATAR_SIZE), Sense::click());
                if ui.is_rect_visible(rect) {
                    let fill = if response.hovered() {
                        palette.surface_hover
                    } else {
                        palette.surface
                    };
                    ui.painter().circle_filled(rect.center(), 18.0, fill);
                    let inner = egui::Rect::from_center_size(rect.center(), Vec2::splat(28.0));
                    match avatar.as_deref() {
                        Some(url) => super::widgets::paint_cover(
                            ui,
                            &palette,
                            Some(url),
                            inner,
                            14.0,
                            Icon::User,
                            Some(app.backend.art()),
                        ),
                        None => {
                            let initial = name
                                .chars()
                                .next()
                                .unwrap_or('?')
                                .to_uppercase()
                                .to_string();
                            ui.painter()
                                .circle_filled(inner.center(), 14.0, palette.accent);
                            ui.painter().text(
                                inner.center(),
                                egui::Align2::CENTER_CENTER,
                                initial,
                                theme::bold(13.0),
                                palette.on_accent,
                            );
                        }
                    }
                }
                let response = response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(&name);
                egui::Popup::menu(&response)
                    .frame(super::widgets::menu_frame(&palette))
                    .align(egui::RectAlign::BOTTOM_END)
                    .show(|ui| {
                        ui.set_width(200.0);
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(10.0);
                            theme::text(ui, &name, theme::semibold(14.0), palette.text);
                        });
                        if let Some(product) =
                            app.user.as_ref().and_then(|user| user.product.clone())
                        {
                            ui.horizontal(|ui| {
                                ui.add_space(10.0);
                                theme::text(
                                    ui,
                                    capitalize(&product),
                                    theme::regular(12.0),
                                    palette.secondary,
                                );
                            });
                        }
                        super::widgets::menu_separator(ui, &palette);
                        if super::widgets::menu_item(ui, &palette, Some(Icon::Settings), "Settings")
                        {
                            app.actions.push(Action::Open(Page::Settings));
                        }
                        if super::widgets::menu_item(
                            ui,
                            &palette,
                            Some(Icon::Info),
                            "Keyboard shortcuts",
                        ) {
                            app.actions
                                .push(Action::ShowDialog(crate::model::Dialog::Shortcuts));
                        }
                        super::widgets::menu_separator(ui, &palette);
                        if super::widgets::menu_item(ui, &palette, Some(Icon::LogOut), "Sign out") {
                            app.actions.push(Action::SignOut);
                        }
                    });
                ui.add_space(4.0);
                if theme::icon_button(
                    ui,
                    Icon::Settings,
                    ICON_BUTTON_ICON,
                    palette.secondary,
                    palette.text,
                    "Settings",
                )
                .clicked()
                {
                    app.actions.push(Action::Open(Page::Settings));
                }
                if theme::icon_button(
                    ui,
                    Icon::AudioLines,
                    ICON_BUTTON_ICON,
                    if app.settings.milkdrop_open {
                        palette.accent
                    } else {
                        palette.secondary
                    },
                    palette.text,
                    super::keys::platform_shortcut(
                        "MilkDrop visualiser (Ctrl+Shift+K)",
                        "MilkDrop visualiser (Cmd+Shift+K)",
                    ),
                )
                .clicked()
                {
                    app.actions.push(Action::ToggleWinampMilkdrop);
                }
                if theme::icon_button(
                    ui,
                    Icon::Shrink,
                    ICON_BUTTON_ICON,
                    palette.secondary,
                    palette.text,
                    super::keys::platform_shortcut(
                        "Winamp mini player (Ctrl+M)",
                        "Winamp mini player (Cmd+Shift+M)",
                    ),
                )
                .clicked()
                {
                    app.actions.push(Action::ToggleWinampWindow);
                }
                // A quiet spinner once the app has been talking to Spotify for a
                // while, long enough that fast requests never flash it.
                if busy {
                    theme::spinner(ui, SPINNER_SIZE, palette.secondary)
                        .on_hover_text("Waiting for Spotify…");
                }
                // Where playback is.
                if let Some(galley) = device_galley {
                    let device = galley.text().to_owned();
                    let response = badge(
                        ui,
                        &palette,
                        Icon::Speaker,
                        galley,
                        DEVICE_BADGE_PADDING,
                        fit.labels,
                    );
                    // Without its label the badge still has to say where
                    // playback went.
                    let response = if fit.labels {
                        response
                    } else {
                        response.on_hover_text(device)
                    };
                    if response.clicked() {
                        app.actions.push(Action::ToggleDevicesPopup);
                    }
                }
                // A newer release. Most people never visit a releases page,
                // so the app says so, quietly, until they do.
                if let (Some(galley), Some(update)) = (update_galley, update)
                    && badge(
                        ui,
                        &palette,
                        Icon::Info,
                        galley,
                        UPDATE_BADGE_PADDING,
                        fit.labels,
                    )
                    .on_hover_text(format!("Version {} is available.", update.version))
                    .clicked()
                {
                    app.actions.push(Action::ShowUpdate);
                }
            });
        },
    );
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod topbar_fit_tests {
    use super::*;

    // What the badges measure on a bar showing "Playing on MacBook de Luis"
    // and "Update to 0.7.1", each including the spacing before it.
    const DEVICE: f32 = ITEM_SPACING + 176.0;
    const UPDATE: f32 = ITEM_SPACING + 152.0;
    // Collapsed, a badge is a square chip as tall as its text.
    const CHIP: f32 = ITEM_SPACING + 15.0 + BADGE_PADDING_Y;

    /// The narrowest bar the app can produce: a 760 px window, its sidebar,
    /// and the navigation buttons all taken out.
    const NARROWEST_BAR: f32 = 398.0;

    fn right_end(room: f32, labelled: f32, icons: f32) -> f32 {
        let fit = topbar_fit(room, RIGHT_CONTROLS_WIDTH, labelled, icons);
        let badges = if fit.labels { labelled } else { icons };
        RIGHT_CONTROLS_WIDTH + badges - (room - fit.search)
    }

    #[test]
    fn a_wide_bar_keeps_the_field_it_always_had() {
        let fit = topbar_fit(2000.0, RIGHT_CONTROLS_WIDTH, DEVICE + UPDATE, CHIP * 2.0);
        assert_eq!(fit.search, SEARCH_MAX);
        assert!(fit.labels);
        // Half the room, as before, while half still fits.
        let fit = topbar_fit(700.0, RIGHT_CONTROLS_WIDTH, 0.0, 0.0);
        assert_eq!(fit.search, 350.0);
    }

    #[test]
    fn the_right_end_never_reaches_over_the_search_field() {
        let mut room = NARROWEST_BAR;
        while room <= 2400.0 {
            for (labelled, icons) in [
                (0.0, 0.0),
                (DEVICE, CHIP),
                (UPDATE, CHIP),
                (DEVICE + UPDATE, CHIP * 2.0),
            ] {
                let over = right_end(room, labelled, icons);
                assert!(
                    over <= 0.0,
                    "badges overlap the field by {over} px on a {room} px bar"
                );
            }
            room += 1.0;
        }
    }

    #[test]
    fn a_right_panel_can_narrow_search_after_the_badges_collapse() {
        let room = RIGHT_CONTROLS_WIDTH + CHIP * 2.0 + 100.0;
        let fit = topbar_fit(room, RIGHT_CONTROLS_WIDTH, DEVICE + UPDATE, CHIP * 2.0);
        assert!(!fit.labels);
        assert_eq!(fit.search, 100.0);
        assert_eq!(right_end(room, DEVICE + UPDATE, CHIP * 2.0), 0.0);
    }

    #[test]
    fn a_narrow_bar_trades_the_badge_labels_for_their_icons() {
        assert!(topbar_fit(1400.0, RIGHT_CONTROLS_WIDTH, DEVICE, CHIP).labels);
        // The 1080 px window of the report that started this.
        assert!(topbar_fit(952.0, RIGHT_CONTROLS_WIDTH, DEVICE + UPDATE, CHIP * 2.0).labels);
        assert!(!topbar_fit(NARROWEST_BAR, RIGHT_CONTROLS_WIDTH, DEVICE, CHIP).labels);
    }

    #[test]
    fn the_field_stays_readable_however_tight_the_bar_gets() {
        let mut room = NARROWEST_BAR;
        while room <= 2400.0 {
            let fit = topbar_fit(room, RIGHT_CONTROLS_WIDTH, DEVICE + UPDATE, CHIP * 2.0);
            assert!(fit.search >= SEARCH_FLOOR, "field is {} px", fit.search);
            assert!(fit.search <= SEARCH_MAX);
            room += 1.0;
        }
    }
}
