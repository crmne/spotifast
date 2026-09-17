//! Podcast pages and episode rows.

use std::sync::Arc;

use egui::{CornerRadius, Layout, Rect, Sense, UiBuilder, Vec2, pos2, vec2};

use crate::api::models::{Episode, PlayableItem, Show, pick_image};
use crate::app::App;
use crate::model::{Action, Loadable, Page, RowContext};
use crate::theme::{self, Icon};
use crate::util;

use super::collection::{Hero, hero, hero_images};
use super::widgets;

pub const EPISODE_ROW_HEIGHT: f32 = 128.0;

pub fn show(app: &mut App, ui: &mut egui::Ui, id: &str) {
    if !app.show_pages.contains_key(id) {
        app.ensure_loaded(Page::Show(id.to_string()));
    }
    let Some(page) = app.show_pages.remove(id) else {
        return;
    };
    let preview = app.known_show(id).cloned();
    let palette = app.palette;
    match &page.show {
        Loadable::Loaded(show) => {
            show_hero(app, ui, show, preview.as_ref());
            show_actions(
                app,
                ui,
                show,
                page.episodes
                    .items
                    .first()
                    .map(|episode| episode.uri.as_str()),
            );
            if !show.description.is_empty() {
                theme::section_title(ui, &palette, "About");
                ui.add_space(4.0);
                let description = util::strip_html(&show.description);
                let galley = crate::bidi::layout(
                    ui.painter(),
                    &description,
                    theme::regular(13.5),
                    palette.secondary,
                    ui.available_width(),
                    usize::MAX,
                    None,
                );
                ui.add(egui::Label::new(galley));
                ui.add_space(16.0);
            }
            theme::section_title(ui, &palette, "All episodes");
            ui.add_space(6.0);
            let episodes = page.episodes.items.clone();
            let show_image = pick_image(&show.images, 64).map(str::to_string);
            widgets::virtual_rows(ui, episodes.len(), EPISODE_ROW_HEIGHT, |ui, index| {
                episode_row(app, ui, &episodes[index], show_image.as_deref());
            });
            if page.episodes.loading {
                widgets::loading_row(ui, &palette, app.locale);
            }
            if let Some(error) = &page.episodes.error {
                let error = error.clone();
                widgets::error_row(ui, app, &error, Some(Page::Show(id.to_string())));
            }
            widgets::load_more_when_near_end(
                ui,
                app,
                Page::Show(id.to_string()),
                page.episodes.can_load_more(),
            );
        }
        Loadable::Loading | Loadable::NotLoaded => {
            if let Some(show) = &preview {
                show_hero(app, ui, show, None);
                ui.add_enabled_ui(false, |ui| {
                    show_actions(app, ui, show, Some(&show.uri));
                });
            } else {
                ui.add_space(40.0);
            }
            widgets::loading_row(ui, &palette, app.locale);
        }
        Loadable::Failed(error) => {
            let error = error.clone();
            ui.add_space(40.0);
            widgets::error_row(ui, app, &error, Some(Page::Show(id.to_string())));
        }
    }
    app.show_pages.insert(id.to_string(), page);
}

fn show_hero(app: &mut App, ui: &mut egui::Ui, show: &Show, preview: Option<&Show>) {
    let mut byline = vec![(show.publisher.clone(), None)];
    if let Some(total) = show.total_episodes {
        byline.push((format!("{total} episodes"), None));
    }
    let images = hero_images(
        &show.images,
        preview.map(|show| show.images.as_slice()),
        false,
    );
    hero(
        app,
        ui,
        Hero {
            images,
            liked: false,
            kind: "Podcast",
            title: &show.name,
            description: None,
            byline,
            round: false,
        },
    );
}

fn show_actions(app: &mut App, ui: &mut egui::Ui, show: &Show, latest: Option<&str>) {
    let palette = app.palette;
    let saved = app.is_saved(&show.uri).unwrap_or(false);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 18.0;
        if let Some(uri) = latest {
            if app.play_pending(uri) {
                theme::circle_spinner(ui, 56.0, palette.accent, palette.on_accent, "Starting…");
            } else if theme::circle_button(
                ui,
                Icon::PlayFilled,
                56.0,
                palette.accent,
                palette.accent_hover,
                palette.on_accent,
                "Play latest episode",
            )
            .clicked()
            {
                app.actions.push(Action::PlayUris {
                    uris: vec![uri.to_string()],
                    index: 0,
                });
            }
        }
        let (icon, color, tooltip) = if saved {
            (
                Icon::CircleCheck,
                palette.accent,
                "Remove from Your Library",
            )
        } else {
            (Icon::CirclePlus, palette.secondary, "Follow podcast")
        };
        if theme::icon_button(ui, icon, 26.0, color, palette.text, tooltip).clicked() {
            app.actions.push(Action::ToggleSaved(show.uri.clone()));
        }
        let more = theme::icon_button(
            ui,
            Icon::Ellipsis,
            26.0,
            palette.secondary,
            palette.text,
            "More",
        );
        egui::Popup::menu(&more)
            .frame(widgets::menu_frame(&palette))
            .show(|ui| widgets::context_menu_items(ui, app, &show.uri, &show.name, None));
    });
    ui.add_space(16.0);
}

/// One episode with its description, date, length, and progress.
pub fn episode_row(
    app: &mut App,
    ui: &mut egui::Ui,
    episode: &Episode,
    fallback_image: Option<&str>,
) {
    let palette = app.palette;
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), EPISODE_ROW_HEIGHT),
        Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hovered = ui.rect_contains_pointer(rect);
    if hovered {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(6),
            palette
                .surface_hover
                .gamma_multiply(if palette.dark { 0.7 } else { 1.0 }),
        );
    }
    let inner = rect.shrink2(vec2(12.0, 12.0));
    let cover_rect = Rect::from_min_size(inner.min, Vec2::splat(96.0));
    let image = pick_image(&episode.images, 64).or(fallback_image);
    widgets::paint_cover(
        ui,
        &palette,
        image,
        cover_rect,
        6.0,
        Icon::Mic,
        Some(app.backend.art()),
    );
    let text_left = cover_rect.right() + 16.0;
    let text_rect = Rect::from_min_max(
        pos2(text_left, inner.top()),
        pos2(inner.right() - 40.0, inner.bottom()),
    );
    let painter = ui.painter().with_clip_rect(text_rect);
    let now_playing = app.now_playing();
    let is_current = now_playing
        .as_ref()
        .is_some_and(|now| now.uri == episode.uri);
    let title_color = if is_current {
        palette.accent
    } else {
        palette.text
    };
    crate::bidi::paint_line(
        &painter,
        text_left,
        text_rect.right(),
        text_rect.top() + 10.0,
        &episode.name,
        theme::semibold(15.0),
        title_color,
    );
    let description = util::strip_html(&episode.description);
    let description_galley = crate::bidi::layout(
        ui.painter(),
        &description,
        theme::regular(12.5),
        palette.secondary,
        text_rect.width(),
        2,
        None,
    );
    let description_rect = Rect::from_min_size(
        pos2(text_left, text_rect.top() + 26.0),
        vec2(text_rect.width(), 34.0),
    );
    ui.painter().with_clip_rect(description_rect).galley(
        crate::bidi::galley_pos(description_rect, &description_galley),
        description_galley,
        palette.secondary,
    );

    // Footer: play, date, duration, progress.
    let footer_y = inner.bottom() - 16.0;
    let button = Rect::from_center_size(pos2(text_left + 16.0, footer_y), Vec2::splat(32.0));
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(button)
            .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
    );
    let playing_here = is_current && now_playing.as_ref().is_some_and(|now| now.playing);
    let icon = if playing_here {
        Icon::PauseFilled
    } else {
        Icon::PlayFilled
    };
    if app.play_pending(&episode.uri) {
        theme::circle_spinner(&mut child, 32.0, palette.text, palette.window, "Starting…");
    } else if theme::circle_button(
        &mut child,
        icon,
        32.0,
        palette.text,
        if palette.dark {
            egui::Color32::WHITE
        } else {
            palette.text
        },
        palette.window,
        "Play",
    )
    .clicked()
    {
        if is_current {
            app.actions.push(Action::TogglePlay);
        } else {
            app.actions.push(Action::PlayUris {
                uris: vec![episode.uri.clone()],
                index: 0,
            });
        }
    }
    let mut meta = Vec::new();
    if let Some(date) = &episode.release_date {
        meta.push(util::format_date(date));
    }
    let resume = episode.resume_point.as_ref();
    let remaining = resume
        .filter(|resume| !resume.fully_played && resume.resume_position_ms > 0)
        .map(|resume| {
            episode
                .duration_ms
                .saturating_sub(resume.resume_position_ms)
        });
    match remaining {
        Some(left) => meta.push(format!("{} left", util::format_episode_ms(left))),
        None => meta.push(util::format_episode_ms(episode.duration_ms)),
    }
    let meta_text = meta.join(" • ");
    let meta_galley =
        ui.painter()
            .layout_no_wrap(meta_text, theme::regular(12.5), palette.secondary);
    let meta_pos = pos2(button.right() + 12.0, footer_y - meta_galley.size().y / 2.0);
    ui.painter()
        .galley(meta_pos, meta_galley.clone(), palette.secondary);
    let mut x = meta_pos.x + meta_galley.size().x + 10.0;
    if let Some(resume) = resume {
        if resume.fully_played {
            let check = Rect::from_center_size(pos2(x + 8.0, footer_y), Vec2::splat(14.0));
            Icon::CircleCheck
                .image(palette.accent, 14.0)
                .paint_at(ui, check);
            ui.painter().text(
                pos2(x + 20.0, footer_y),
                egui::Align2::LEFT_CENTER,
                "Played",
                theme::regular(12.0),
                palette.secondary,
            );
        } else if resume.resume_position_ms > 0 && episode.duration_ms > 0 {
            let bar = Rect::from_min_size(pos2(x, footer_y - 2.0), vec2(72.0, 4.0));
            ui.painter().rect_filled(bar, 2.0, palette.surface_active);
            let fraction = resume.resume_position_ms as f32 / episode.duration_ms as f32;
            let filled =
                Rect::from_min_size(bar.min, vec2(bar.width() * fraction.clamp(0.0, 1.0), 4.0));
            ui.painter().rect_filled(filled, 2.0, palette.accent);
            x += 80.0;
        }
    }
    let _ = x;
    // More menu.
    let more_rect = Rect::from_center_size(pos2(inner.right() - 16.0, footer_y), Vec2::splat(32.0));
    let mut more_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(more_rect)
            .layout(Layout::centered_and_justified(egui::Direction::LeftToRight)),
    );
    let more = theme::icon_button(
        &mut more_ui,
        Icon::Ellipsis,
        18.0,
        palette.secondary,
        palette.text,
        "More",
    );
    let item = PlayableItem::Episode(episode.clone());
    egui::Popup::menu(&more)
        .frame(widgets::menu_frame(&palette))
        .show(|ui| widgets::item_menu(ui, app, &item, None, None));
    egui::Popup::context_menu(&response)
        .frame(widgets::menu_frame(&palette))
        .show(|ui| widgets::item_menu(ui, app, &item, None, None));
    if response.double_clicked() {
        app.actions.push(Action::PlayUris {
            uris: vec![episode.uri.clone()],
            index: 0,
        });
    }
    let _ = RowContext::Uris(Arc::from([]));
}
