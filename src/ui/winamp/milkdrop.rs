//! MilkDrop in the mini player: a panel under the equalizer and the playlist,
//! in the same window, drawn from the classic skin's `gen.bmp` as Winamp 5
//! framed it, around the stream of frames that also fills a Windows Media
//! Player skin's pane.
//!
//! It is a panel and not a window of its own because a window could not stay
//! with the player: Wayland lets no app place its windows, so nothing could
//! snap, and moving the player would leave it behind. Sharing one surface, it
//! moves with the player for free.

use std::time::Instant;

use egui::{Color32, Context, Key, Modifiers, Rect, Sense};

use crate::app::App;
use crate::i18n::gettext;
use crate::milkdrop::pane::{Picture, UV};
use crate::model::Action;
use crate::skin::layout::{self, Area};
use crate::skin::{Sheet, generic};

use super::View;

/// Winamp's frame spells its titles with the skin's own letters.
const TITLE: &str = "MILKDROP";

/// The panel, its top left at the view's origin. Returns whether it moves by
/// itself.
pub(super) fn show(app: &mut App, view: &mut View, focused: bool) -> bool {
    let ctx = view.ui.ctx().clone();
    let width = layout::WINDOW_WIDTH;
    let height = super::clamped_panel(app.settings.milkdrop_panel_height);
    read_keys(app, &ctx);

    // What the frame's sprites leave over between them.
    view.fill(0, 0, width, height, Color32::BLACK);
    let skin = view.skin;
    let letters = generic::letters(skin.sheet(Sheet::Gen), focused);
    let title_width = generic::title_width(&letters, TITLE);
    for (sprite, x, y) in generic::frame(width, height, title_width, focused) {
        view.sprite_at(sprite, x, y);
    }
    for (sprite, x, y) in generic::title(&letters, TITLE, width) {
        view.sprite_at(sprite, x, y);
    }

    let unit = view.unit;
    let inside = generic::inside(width, height);
    let inside_rect = view.rect(inside);
    let moving = picture(app, view, inside_rect, unit);

    let body = view.interact(inside, "milkdrop-body", Sense::click());
    if body.double_clicked() {
        app.actions.push(Action::MilkdropFullscreen);
    } else if body.secondary_clicked() {
        app.actions.push(Action::MilkdropEffect(1));
    }
    let bar = Area::new(0, 0, width, generic::TOP_HEIGHT);
    let title = view.interact(bar, "milkdrop-title", Sense::click_and_drag());
    if title.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if title.double_clicked() {
        app.actions.push(Action::MilkdropFullscreen);
    }
    let close_area = generic::close_button(width);
    let close = view.interact(close_area, "milkdrop-close", Sense::click());
    if close.is_pointer_button_down_on() {
        view.sprite(generic::CLOSE_PRESSED, close_area);
    }
    if close.clicked() {
        app.actions.push(Action::ToggleWinampMilkdrop);
    }
    let big = generic::full_screen_button(width);
    let [r, g, b] = generic::title_colour(skin.sheet(Sheet::Gen), focused);
    for (x, y, across, down) in generic::FULL_SCREEN_GLYPH {
        view.fill(
            big.x + x,
            big.y + y,
            across,
            down,
            Color32::from_rgb(r, g, b),
        );
    }
    if view
        .interact(big, "milkdrop-full-screen", Sense::click())
        .on_hover_text(gettext(app.locale, "Full screen with lyrics").as_ref())
        .clicked()
    {
        app.actions.push(Action::MilkdropFullscreen);
    }
    grip(app, view, width, height);
    moving
}

/// The corner that makes the panel, and the window with it, taller, in whole
/// skin pixels so the frame's tiles stay crisp.
fn grip(app: &mut App, view: &mut View, width: u32, height: u32) {
    let response = view
        .interact(generic::grip(width, height), "milkdrop-grip", Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if response.dragged() {
        app.winamp.milkdrop_resize += response.drag_delta().y / view.unit;
        let steps = app.winamp.milkdrop_resize.trunc();
        app.winamp.milkdrop_resize -= steps;
        if steps != 0.0 {
            let wanted = (i64::from(height) + steps as i64).max(0) as u32;
            app.actions
                .push(Action::SetMilkdropPanel(super::clamped_panel(wanted)));
        }
    }
    if response.drag_stopped() {
        app.winamp.milkdrop_resize = 0.0;
    }
}

/// The picture, or the bars on the window's own background until there is
/// one. Returns whether it moves by itself.
fn picture(app: &mut App, view: &mut View, inside: Rect, unit: f32) -> bool {
    let ctx = view.ui.ctx().clone();
    let pixels = inside.size() * ctx.pixels_per_point();
    let picture = app.milkdrop_picture(&ctx, [pixels.x, pixels.y]);
    if let Picture::Frame(texture) = picture {
        view.ui.painter().image(texture, inside, UV, Color32::WHITE);
        return true;
    }
    let background = generic::window_background(view.skin.sheet(Sheet::GenEx))
        .map_or(Color32::BLACK, |[r, g, b]| Color32::from_rgb(r, g, b));
    view.ui.painter().rect_filled(inside, 0.0, background);
    let (shapes, moving) = super::wmp::effect_shapes(app, inside, unit, Instant::now());
    view.ui.painter().extend(shapes);
    moving || picture == Picture::Waiting
}

/// The keys the plain window has for what it shows. The app's own have had
/// their turn; **F** is among them.
fn read_keys(app: &mut App, ctx: &Context) {
    let pressed = |key: Key| ctx.input_mut(|input| input.consume_key(Modifiers::NONE, key));
    if pressed(Key::Escape) {
        app.actions.push(Action::ToggleWinampMilkdrop);
    }
    if pressed(Key::N) {
        app.actions.push(Action::MilkdropEffect(1));
    }
    if pressed(Key::P) {
        app.actions.push(Action::MilkdropEffect(-1));
    }
}
