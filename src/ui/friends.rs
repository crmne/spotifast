//! What friends are listening to, in the right panel.

use egui::{Frame, Margin, Sense, Vec2};

use crate::app::App;
use crate::i18n::gettext;
use crate::model::{Action, Loadable, Page};
use crate::session_reads::FriendActivity;
use crate::theme::{self, Icon, Palette};

use super::widgets;

const AVATAR: f32 = 40.0;
const ROW_GAP: f32 = 18.0;
/// The header's title starts this far in, and the close button's glyph
/// ends this far from the right (`theme::icon_button` pads it by half of
/// `ICON_BUTTON_PAD`).
const TITLE_INSET: i8 = 4;
const CLOSE_GLYPH_INSET: i8 = 6;

pub fn side_panel(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let fit = super::yielding_panel(
        ui.ctx(),
        "friends-panel",
        theme::SIDE_PANEL_MIN_WIDTH..=560.0,
        app.settings.queue_width,
        ui.available_width() - super::topbar::least_width(ui.ctx()),
    );
    let panel = egui::Panel::right("friends-panel")
        .resizable(true)
        .default_size(app.settings.queue_width)
        .size_range(fit.range.clone())
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(palette.panel)
                .inner_margin(Margin::symmetric(12, 12)),
        );
    let response = panel.show(ui, |ui| {
        // The panel sits where the Queue would, under the same controls.
        let window_controls =
            super::window_controls_reservation(ui.ctx(), true, false, ui.available_width());
        ui.add_space(window_controls.queue_top);
        let mut close = false;
        egui::Sides::new().shrink_left().show(
            ui,
            |ui| {
                ui.add_space(4.0);
                theme::text(
                    ui,
                    gettext(locale, "Listening activity"),
                    theme::bold(16.0),
                    palette.text,
                );
            },
            |ui| {
                close = theme::icon_button(
                    ui,
                    Icon::X,
                    18.0,
                    palette.secondary,
                    palette.text,
                    &gettext(locale, "Close"),
                )
                .clicked();
            },
        );
        if close {
            app.actions.push(Action::ToggleFriendsPanel);
        }
        ui.add_space(12.0);
        egui::ScrollArea::vertical()
            .id_salt("friends-panel-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| contents(app, ui));
    });
    // The Listening activity panel shares the Queue panel's width, so switching
    // between the two never moves the page.
    let width = response.response.rect.width();
    if (width - app.settings.queue_width).abs() > 1.0
        && super::panel_width_chosen(ui.ctx(), "friends-panel", &fit)
    {
        app.settings.queue_width = width;
        app.actions.push(Action::SettingsChanged);
    }
}

fn contents(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    match &app.friends {
        Loadable::NotLoaded | Loadable::Loading => widgets::loading_row(ui, &palette, locale),
        Loadable::Failed(error) => {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                theme::icon(ui, Icon::CircleAlert, 16.0, palette.danger);
                theme::text(
                    ui,
                    widgets::sanitize_error_message(error, locale),
                    theme::regular(13.0),
                    palette.secondary,
                );
            });
        }
        Loadable::Loaded(friends) if friends.is_empty() => widgets::empty_state(
            ui,
            &palette,
            Icon::Users,
            &gettext(locale, "No listening activity"),
            &gettext(
                locale,
                "What the people you follow play on Spotify shows here.",
            ),
        ),
        Loadable::Loaded(friends) => {
            let now = jiff::Timestamp::now();
            let mut picked = None;
            for friend in friends {
                // Lined up with the header: the title's left edge and the
                // close button's glyph on the right.
                let inset = Frame::new().inner_margin(Margin {
                    left: TITLE_INSET,
                    right: CLOSE_GLYPH_INSET,
                    top: 0,
                    bottom: 0,
                });
                picked = picked.or(inset
                    .show(ui, |ui| row(app, ui, &palette, friend, now))
                    .inner);
                ui.add_space(ROW_GAP);
            }
            app.actions.extend(picked);
        }
    }
}

/// A friend's avatar beside their name, the song, and where it plays from.
/// Hovering the avatar offers to play the song, or to pause it when it is
/// the song playing here; the song, its artist and where it plays from open
/// their pages. Someone listening right now has a dot on their avatar and
/// bars in place of the time.
fn row(
    app: &App,
    ui: &mut egui::Ui,
    palette: &Palette,
    friend: &FriendActivity,
    now: jiff::Timestamp,
) -> Option<Action> {
    let mut action = None;
    // Whether this song is the one here, and if so whether it plays.
    let playing_here = app
        .now_playing()
        .filter(|now| now.uri == friend.track_uri)
        .map(|now| now.playing);
    ui.horizontal_top(|ui| {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(AVATAR), Sense::click());
        if ui.is_rect_visible(rect) {
            widgets::paint_cover(
                ui,
                palette,
                friend.image_url.as_deref(),
                rect,
                AVATAR / 2.0,
                Icon::User,
                Some(app.backend.art()),
            );
            if response.hovered() {
                ui.painter().circle_filled(
                    rect.center(),
                    AVATAR / 2.0,
                    egui::Color32::from_black_alpha(140),
                );
                let icon = if playing_here == Some(true) {
                    Icon::PauseFilled
                } else {
                    Icon::PlayFilled
                };
                theme::paint_icon(ui, icon, rect, 16.0, egui::Color32::WHITE);
            }
            if friend.playing {
                // Inside the avatar's box, so the list's edge never cuts it.
                let center = rect.right_top() + egui::vec2(-6.0, 6.0);
                ui.painter().circle_filled(center, 6.0, palette.panel);
                ui.painter().circle_filled(center, 4.5, LISTENING_DOT);
            }
        }
        let hint = if playing_here == Some(true) {
            gettext(app.locale, "Pause").to_string()
        } else {
            gettext(app.locale, "Play {song}").replace("{song}", &friend.track_name)
        };
        if response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(hint)
            .clicked()
        {
            // The song already here pauses or resumes; any other starts.
            action = Some(match playing_here {
                Some(_) => Action::TogglePlay,
                None => Action::PlayUris {
                    uris: vec![friend.track_uri.clone()],
                    index: 0,
                },
            });
        }
        ui.add_space(4.0);
        ui.vertical(|ui| {
            // Lines as tall as their text, not as a button.
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.spacing_mut().interact_size.y = 0.0;
            egui::Sides::new().shrink_left().show(
                ui,
                |ui| {
                    theme::text(ui, &friend.user_name, theme::semibold(13.5), palette.text);
                },
                |ui| {
                    if friend.playing {
                        listening_bars(ui, palette.accent)
                            .on_hover_text(gettext(app.locale, "Listening now"));
                    } else {
                        let when = when(app.locale, friend.timestamp_ms, now);
                        theme::text(ui, &when, theme::regular(11.5), palette.secondary);
                    }
                },
            );
            let line = ui.fonts_mut(|fonts| fonts.row_height(&line_font()));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                // The song gives way to its artist only as far as half the
                // line, so both stay readable and neither widens the panel.
                let width = ui.available_width();
                let dot = ui.fonts_mut(|fonts| {
                    fonts
                        .layout_no_wrap("•".into(), line_font(), palette.secondary)
                        .size()
                        .x
                });
                let artist = if friend.artist_name.is_empty() {
                    0.0
                } else {
                    let natural = ui.fonts_mut(|fonts| {
                        fonts
                            .layout_no_wrap(
                                friend.artist_name.clone(),
                                line_font(),
                                palette.secondary,
                            )
                            .size()
                            .x
                    });
                    natural.min(width / 2.0)
                };
                let song_width = if artist > 0.0 {
                    (width - artist - dot - 8.0).max(0.0)
                } else {
                    width
                };
                ui.allocate_ui(egui::vec2(song_width, line), |ui| {
                    let song = link(
                        ui,
                        &friend.track_name,
                        friend.album_uri.as_deref(),
                        palette.secondary,
                    );
                    action = action.take().or(song);
                });
                if artist > 0.0 {
                    theme::text(ui, "•", line_font(), palette.secondary);
                    ui.allocate_ui(egui::vec2(ui.available_width(), line), |ui| {
                        let artist = link(
                            ui,
                            &friend.artist_name,
                            friend.artist_uri.as_deref(),
                            palette.secondary,
                        );
                        action = action.take().or(artist);
                    });
                }
            });
            let uri = friend.context_uri.as_deref();
            // A friend's Liked Songs has no page here: ours is not theirs.
            let liked = uri.is_some_and(|uri| uri.ends_with(":collection"));
            let name = if liked {
                Some(gettext(app.locale, "Liked Songs").to_string())
            } else {
                friend.context_name.clone()
            };
            if let Some(name) = name {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    let icon = match uri.and_then(crate::util::uri_kind) {
                        Some("album") => Icon::Disc,
                        Some("artist") => Icon::User,
                        Some("track") => Icon::Music,
                        _ => Icon::ListMusic,
                    };
                    // Centred on the text's line, not on the icon's own box.
                    let (cell, _) =
                        ui.allocate_exact_size(egui::vec2(CONTEXT_ICON, line), Sense::hover());
                    theme::paint_icon(ui, icon, cell, CONTEXT_ICON, palette.secondary);
                    // A song played on its own opens its album, as its title does.
                    let target = if uri.and_then(crate::util::uri_kind) == Some("track") {
                        friend.album_uri.as_deref()
                    } else {
                        uri
                    };
                    let context = link(ui, &name, target, palette.secondary);
                    action = action.take().or(context);
                });
            }
        });
    });
    action
}

/// The blue Spotify puts on someone listening right now.
const LISTENING_DOT: egui::Color32 = egui::Color32::from_rgb(66, 133, 244);
/// The song, its artist and where it plays from share one style.
fn line_font() -> egui::FontId {
    theme::regular(12.5)
}
const CONTEXT_ICON: f32 = 13.0;

/// Four bars of uneven height while a friend listens. They hold still: a
/// moving picture would redraw the window every frame for as long as anyone
/// listens, and the panel stays open for hours.
fn listening_bars(ui: &mut egui::Ui, color: egui::Color32) -> egui::Response {
    const LEVELS: [f32; 4] = [0.6, 1.0, 0.45, 0.8];
    const BAR: f32 = 2.5;
    const GAP: f32 = 2.0;
    const HEIGHT: f32 = 13.0;
    let bars = LEVELS.len() as f32;
    let size = egui::vec2(bars * BAR + (bars - 1.0) * GAP, HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    for (bar, level) in LEVELS.iter().enumerate() {
        let x = rect.left() + bar as f32 * (BAR + GAP);
        let bar_rect = egui::Rect::from_min_max(
            egui::pos2(x, rect.bottom() - HEIGHT * level),
            egui::pos2(x + BAR, rect.bottom()),
        );
        ui.painter().rect_filled(bar_rect, BAR / 2.0, color);
    }
    response
}

/// Text that opens the page behind `uri` when it has one, plain text
/// otherwise.
fn link(ui: &mut egui::Ui, text: &str, uri: Option<&str>, color: egui::Color32) -> Option<Action> {
    match uri.and_then(page_of) {
        Some(page) => theme::link(ui, text, line_font(), color)
            .clicked()
            .then_some(Action::Open(page)),
        None => {
            theme::text(ui, text, line_font(), color);
            None
        }
    }
}

/// The page a buddy list URI opens: an album, an artist or a playlist,
/// including the older `spotify:user:…:playlist:…` form.
fn page_of(uri: &str) -> Option<Page> {
    let id = crate::util::uri_id(uri)?.to_string();
    if uri.contains(":playlist:") {
        return Some(Page::Playlist(id));
    }
    match crate::util::uri_kind(uri)? {
        "album" => Some(Page::Album(id)),
        "artist" => Some(Page::Artist(id)),
        _ => None,
    }
}

/// How long ago a friend played their song.
fn when(locale: crate::i18n::Locale, timestamp_ms: i64, now: jiff::Timestamp) -> String {
    match jiff::Timestamp::from_millisecond(timestamp_ms) {
        Ok(at) => crate::util::format_relative_date(locale, &at.to_string(), now),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buddy_list_uris_open_their_pages() {
        assert_eq!(page_of("spotify:album:1"), Some(Page::Album("1".into())));
        assert_eq!(page_of("spotify:artist:2"), Some(Page::Artist("2".into())));
        assert_eq!(
            page_of("spotify:playlist:3"),
            Some(Page::Playlist("3".into()))
        );
        assert_eq!(
            page_of("spotify:user:spotify:playlist:4"),
            Some(Page::Playlist("4".into()))
        );
        assert_eq!(page_of("spotify:track:5"), None);
    }
}
