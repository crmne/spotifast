//! The Jam in the device picker, and its QR big enough to scan across a
//! room. The session itself lives in [`crate::jam`]; this draws it.

use egui::{Color32, CornerRadius, Rect, Sense, Vec2, pos2, vec2};
use fast_qr::{ECL, QRBuilder};

use crate::app::App;
use crate::i18n::gettext;
use crate::model::Action;
use crate::theme::{self, Icon};

/// How often the picker asks Spotify who is in the Jam now.
const POLL: std::time::Duration = std::time::Duration::from_secs(10);

/// The code shown in the picker: the picker's width minus a two-percent
/// border on each side. Both widths are egui points, so the border scales
/// with the screen the way everything else in the app does.
fn picker_code_size(ui: &egui::Ui) -> f32 {
    let width = ui.available_width();
    width - (width * 0.02 * 2.0)
}

/// The invite as a QR code. Phones scan black on white whatever the theme
/// says, so the card keeps its colours; the white around the modules is a
/// fixed fifteen points, small next to the code but enough for a phone to
/// find its edge, so the code itself grows as large as the space allows.
fn code(
    ui: &mut egui::Ui,
    palette: &crate::theme::Palette,
    text: &str,
    side: f32,
    label: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let painter = ui.painter();
    // Dark themes keep a light card with dark modules, the one shape
    // phones read best, standing out against the dark window on its own.
    // Light themes reverse: dark modules straight on the window colour,
    // no card, the way the rest of the light interface sits on its
    // surface. Quiet zone of fifteen points, or four modules when the
    // code grows past the point where fifteen would undercut that
    // minimum.
    let dark_theme = palette.text.r() > 128;
    if dark_theme {
        painter.rect_filled(rect, CornerRadius::same(6), Color32::WHITE);
    }
    if let Ok(qr) = QRBuilder::new(text.as_bytes()).ecl(ECL::M).build() {
        let modules = qr.size as f32;
        // Fifteen points of quiet zone on every side, but never less
        // than the four modules scanners expect: the floor rises with
        // the code, so a fullscreen code keeps a scannable edge.
        // side = 2*quiet + modules*module, quiet = max(15, 4*module)
        // solves to module = side / (modules + 8) when 15 <= 4*module,
        // and to module = (side - 30) / modules otherwise.
        let module = ((side - 30.0) / modules).min(side / (modules + 8.0));
        let quiet = 15.0_f32.max(4.0 * module);
        let origin = rect.min + (quiet, quiet).into();
        let dark = if dark_theme {
            Color32::BLACK
        } else {
            palette.text
        };
        for y in 0..qr.size {
            for x in 0..qr.size {
                if qr.data[y * qr.size + x].value() {
                    painter.rect_filled(
                        Rect::from_min_size(
                            origin + (x as f32 * module, y as f32 * module).into(),
                            // A hair over one module, so rounding leaves no
                            // gap for a scanner to stumble on.
                            Vec2::splat(module + 0.5),
                        ),
                        0,
                        dark,
                    );
                }
            }
        }
    }
    response
}

/// A member's portrait, round, or nothing when Spotify names no image.
fn paint_avatar(ui: &mut egui::Ui, app: &App, url: &str, rect: Rect) {
    let palette = app.palette;
    let radius = CornerRadius::same((rect.width() / 2.0) as u8);
    ui.painter().rect_filled(rect, radius, palette.outline);
    app.backend.art().touch(url);
    let scaled = crate::images::scaled_uri(url, crate::images::cover_pixels(rect.width()));
    let image = egui::Image::new(scaled)
        .texture_options(crate::images::COVER_TEXTURE)
        .show_loading_spinner(false)
        .corner_radius(radius);
    // Paint through the widget only when the texture is actually ready:
    // otherwise its broken-image mark would sit in the row, and the
    // person icon stands in instead.
    if image.load_for_size(ui.ctx(), rect.size()).is_ok() {
        image.paint_at(ui, rect);
    } else {
        Icon::User
            .image(palette.secondary, rect.width() * 0.7)
            .paint_at(ui, rect);
    }
}

/// The picker's Jam rows: start one, join one by pasted invite, or the
/// session in progress with its code, its members, and the way out.
pub fn section(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    super::widgets::menu_separator(ui, &palette);
    match app.jam.clone() {
        None => {
            if super::widgets::menu_item(
                ui,
                &palette,
                Some(Icon::Users),
                &gettext(locale, "Start a Jam"),
            ) && !app.jam_working
            {
                app.actions.push(Action::JamStart);
            }
            join_row(app, ui);
        }
        Some(jam) => {
            let working = app.jam_working;
            let count = jam.members.len();
            // Translators: {count} is how many people are in the Jam.
            let listening =
                gettext(locale, "{count} listening").replace("{count}", &count.to_string());
            // The heading row, like the picker's own: the session's name
            // left, the count centred, the copy and leave controls right.
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                theme::text(
                    ui,
                    gettext(locale, "Jam session"),
                    theme::medium(14.0),
                    palette.text,
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (exit_icon, exit_label) = if jam.is_owner {
                        (Icon::LogOut, gettext(locale, "End Jam"))
                    } else {
                        (Icon::X, gettext(locale, "Leave Jam"))
                    };
                    if theme::icon_button(
                        ui,
                        exit_icon,
                        15.0,
                        palette.secondary,
                        palette.text,
                        exit_label.as_ref(),
                    )
                    .clicked()
                        && !working
                    {
                        if jam.is_owner {
                            app.actions.push(Action::JamEnd);
                        } else {
                            app.actions.push(Action::JamLeave);
                        }
                    }
                    if theme::icon_button(
                        ui,
                        Icon::Copy,
                        15.0,
                        palette.secondary,
                        palette.text,
                        &gettext(locale, "Copy invite link"),
                    )
                    .clicked()
                    {
                        app.actions.push(Action::CopyText(jam.invite_url()));
                    }
                    // The count sits between the title and the controls:
                    // roughly the middle of the row's free span.
                    let free = ui.available_width();
                    let count_width = {
                        let galley = ui.painter().layout_no_wrap(
                            listening.to_string(),
                            theme::regular(12.0),
                            palette.secondary,
                        );
                        galley.size().x
                    };
                    ui.add_space(((free - count_width) / 2.0).max(0.0));
                    theme::text(ui, listening, theme::regular(12.0), palette.secondary);
                });
            });
            ui.add_space(4.0);
            // The code itself is the maximize button: press it and the QR
            // fills the screen for scanning far away. It centres in the
            // row, and the members list themselves under it.
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                let qr = code(
                    ui,
                    &palette,
                    &jam.invite_url(),
                    picker_code_size(ui),
                    &gettext(locale, "Jam QR code"),
                );
                if qr.clicked() && !working {
                    app.actions.push(Action::SetJamFullscreen(true));
                }
            });
            ui.add_space(4.0);
            members_column(app, ui, &jam.members, jam.owner_id.as_deref());
            if working {
                theme::spinner(ui, 14.0, palette.accent);
            }
        }
    }
    // People join without this window noticing; the picker re-asks on its
    // own clock while a Jam is open.
    if app.jam.is_some() {
        let due = app
            .jam_polled_at
            .is_none_or(|polled| polled.elapsed() >= POLL);
        if due && !app.jam_working {
            app.actions.push(Action::JamPollDue);
        }
        ui.ctx().request_repaint_after(POLL);
    }
}

/// The members under the code: portrait, name, the host marked. The
/// account's own picture comes from the signed-in profile the top bar
/// already holds, the others from the session's profile reads; a member
/// Spotify names no picture for keeps the plain person icon.
fn members_column(
    app: &App,
    ui: &mut egui::Ui,
    members: &[crate::jam::JamMember],
    owner_id: Option<&str>,
) {
    let palette = app.palette;
    // Every member stays reachable: rows past the window's height scroll
    // instead of pushing the picker past the screen edge.
    egui::ScrollArea::vertical()
        .id_salt("jam-members")
        .auto_shrink([false, true])
        .max_height(156.0)
        .show(ui, |ui| {
            for member in members {
                let is_owner = owner_id.is_some_and(|owner| member.uri.ends_with(owner));
                let image = member_image(app, member);
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                if response.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), palette.surface_hover);
                }
                let face = Rect::from_center_size(
                    pos2(rect.left() + 11.0, rect.center().y),
                    Vec2::splat(18.0),
                );
                match image.as_deref() {
                    Some(url) => paint_avatar(ui, app, url, face),
                    None => Icon::User.image(palette.secondary, 14.0).paint_at(ui, face),
                }
                ui.painter().with_clip_rect(rect).text(
                    pos2(rect.left() + 26.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &member.name,
                    theme::regular(12.0),
                    palette.text,
                );
                if is_owner {
                    Icon::Check.image(palette.secondary, 12.0).paint_at(
                        ui,
                        Rect::from_center_size(
                            pos2(rect.right() - 8.0, rect.center().y),
                            Vec2::splat(12.0),
                        ),
                    );
                }
            }
        });
}

/// A member's portrait: the session's profile reads first; the signed-in
/// account's own picture for their row, matched by the session's owner
/// mark, the account id, or the display name, since Spotify names the
/// same person in several id shapes across its services.
fn member_image(app: &App, member: &crate::jam::JamMember) -> Option<String> {
    if let Some(url) = app.jam_member_images.get(&member.uri) {
        return Some(url.clone());
    }
    // Only the member that IS the signed-in account takes its picture:
    // the session names member uris in several shapes, so both the full
    // uri and its trailing id are compared against the account's.
    let id = app.user_id()?;
    let own = member.uri.ends_with(&format!(":{id}")) || member.uri.ends_with(id);
    if !own {
        return None;
    }
    let user = app.user.as_ref()?;
    crate::api::models::pick_image(&user.images, 64).map(str::to_owned)
}

/// The paste-an-invite row under Start a Jam.
fn join_row(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    let token = crate::link::jam_invite(&app.jam_invite_draft);
    ui.horizontal(|ui| {
        let field = egui::TextEdit::singleline(&mut app.jam_invite_draft)
            .hint_text(gettext(locale, "Paste a Jam invite to join"))
            .desired_width(ui.available_width() - 30.0)
            .font(theme::regular(13.0));
        let field = ui.add(field);
        let submitted = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
        let join = theme::icon_button(
            ui,
            Icon::CirclePlus,
            15.0,
            if token.is_some() && !app.jam_working {
                palette.secondary
            } else {
                palette.dim
            },
            palette.text,
            &gettext(locale, "Join"),
        );
        if (join.clicked() || submitted)
            && let Some(token) = token
            && !app.jam_working
        {
            app.actions.push(Action::JamJoin(token));
        }
    });
}

/// The QR fills the left half of the window, as large as the space
/// allows; the right half holds the heading and the way out, centred.
/// The player bar stays visible and usable, and the whole thing keeps a
/// five-point border above and below it.
pub fn fullscreen(app: &mut App, ui: &mut egui::Ui) {
    let Some(jam) = app.jam.clone() else {
        // The session ended while the QR stood open: stand down through
        // an action, as the draw code may not write state itself.
        app.actions.push(Action::SetJamFullscreen(false));
        return;
    };
    let palette = app.palette;
    let locale = app.locale;
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(palette.window))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            // This panel is the space above the player bar already: the
            // frame the app hands it ends where the bar begins, so the
            // five-point borders are the only insets.
            let area = Rect::from_min_max(
                pos2(rect.left(), rect.top() + 5.0),
                pos2(rect.right(), rect.bottom() - 5.0),
            );
            ui.set_clip_rect(area);
            // The code owns the left half: centred in it, as large as the
            // half's width or the height allows, whichever binds first.
            let half = area.width() / 2.0;
            let side = half.min(area.height()) - 10.0;
            let left_center = pos2(area.left() + half / 2.0, area.center().y);
            let mut host = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                pos2(left_center.x - side / 2.0, left_center.y - side / 2.0),
                Vec2::splat(side),
            )));
            code(
                &mut host,
                &palette,
                &jam.invite_url(),
                side,
                &gettext(locale, "Jam QR code"),
            );
            // The right half: the heading over the exit, the pair centred
            // in the middle of the half.
            let right = Rect::from_min_max(
                pos2(area.left() + half, area.top()),
                pos2(area.right(), area.bottom()),
            );
            let mut panel = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(right)
                    .layout(egui::Layout::top_down(egui::Align::Center)),
            );
            // The pair stands about seventy-six points tall; start it so
            // it sits in the middle of the half.
            panel.add_space(((right.height() - 76.0) / 2.0).max(0.0));
            theme::text(
                &mut panel,
                if jam.is_owner {
                    gettext(locale, "Scan to join this Jam").into_owned()
                } else {
                    gettext(locale, "Jam").into_owned()
                },
                theme::bold(18.0),
                palette.text,
            );
            panel.add_space(16.0);
            let exit_label = gettext(locale, "Exit");
            if theme::pill_button(&mut panel, &palette, exit_label.as_ref(), true).clicked() {
                app.actions.push(Action::SetJamFullscreen(false));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An invite encodes, and a longer one too: the code grows with the
    /// link, never refusing what Spotify handed out.
    #[test]
    fn the_invite_encodes_at_every_link_length() {
        let link = "https://open.spotify.com/socialsession/AbC123xyz";
        let qr = QRBuilder::new(link.as_bytes()).ecl(ECL::M).build().unwrap();
        assert!(qr.size >= 21, "the smallest QR is 21 modules");
        let longer = format!("{link}1234567890");
        assert!(
            QRBuilder::new(longer.as_bytes())
                .ecl(ECL::M)
                .build()
                .is_ok()
        );
    }
}
