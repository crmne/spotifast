//! MilkDrop in a Winamp frame: a window of its own, drawn from the classic
//! skin's `gen.bmp` as Winamp 5 did, around the stream of frames that also
//! fills a Windows Media Player skin's pane.

use std::time::Instant;

use egui::{
    Color32, Context, Id, Key, Modifiers, Rect, ResizeDirection, Sense, Ui, Vec2, ViewportBuilder,
    ViewportCommand, ViewportId, vec2,
};

use crate::app::App;
use crate::i18n::gettext;
use crate::milkdrop::pane::{Picture, UV};
use crate::model::Action;
use crate::skin::layout::Area;
use crate::skin::{Sheet, generic};

use super::{View, unit};

/// Winamp's frame spells its titles with the skin's own letters.
const TITLE: &str = "MILKDROP";

/// The size to open at, in points: the saved picture size and its frame.
/// Kept from the moment the window opens, because a builder with a new size
/// resizes the window, which would fight the hand dragging its corner.
fn opening_size_id() -> Id {
    Id::new("milkdrop-frame-size")
}

/// A resize has started, so the window's size is the person's to save.
fn resized_id() -> Id {
    Id::new("milkdrop-frame-resized")
}

fn chrome() -> Vec2 {
    vec2(
        (generic::LEFT_WIDTH + generic::RIGHT_WIDTH) as f32,
        (generic::TOP_HEIGHT + generic::BOTTOM_HEIGHT) as f32,
    )
}

fn min_size(unit: f32) -> Vec2 {
    vec2(generic::MIN_WIDTH as f32, generic::MIN_HEIGHT as f32) * unit
}

/// The window, while MilkDrop is open beside a classic skin.
pub(super) fn show(app: &mut App, ctx: &Context) {
    let id = opening_size_id();
    if !app.milkdrop_framed() {
        ctx.data_mut(|data| {
            data.remove::<Vec2>(id);
            data.remove::<bool>(resized_id());
        });
        return;
    }
    let unit = unit(app, ctx);
    let size = match ctx.data(|data| data.get_temp::<Vec2>(id)) {
        Some(size) => size,
        None => {
            let [width, height] = app.settings.milkdrop_size;
            let size = (vec2(width, height) + chrome() * unit).max(min_size(unit));
            ctx.data_mut(|data| data.insert_temp(id, size));
            // A window that just opened has a border like any other.
            #[cfg(target_os = "linux")]
            crate::hyprland::bare_skin_windows();
            size
        }
    };
    let builder = ViewportBuilder::default()
        .with_title("MilkDrop")
        .with_decorations(false)
        .with_resizable(true)
        .with_inner_size(size)
        .with_min_inner_size(min_size(unit))
        .with_taskbar(false);
    ctx.show_viewport_immediate(
        ViewportId::from_hash_of("milkdrop-frame"),
        builder,
        |ui, _class| draw(app, ui),
    );
}

fn draw(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let unit = unit(app, &ctx);
    let origin = ui.max_rect().min;
    let size = ctx.viewport_rect().size();
    let width = ((size.x / unit).floor() as u32).max(generic::MIN_WIDTH);
    let height = ((size.y / unit).floor() as u32).max(generic::MIN_HEIGHT);
    let focused = ctx.input(|input| input.viewport().focused.unwrap_or(true));
    if ctx.input(|input| input.viewport().close_requested()) {
        app.actions.push(Action::ToggleWinampMilkdrop);
    }
    read_keys(app, &ctx);

    let textures = app.winamp.textures(&ctx);
    let skin = app.winamp.skin.clone();
    // What a size between whole skin pixels leaves over.
    ui.painter()
        .rect_filled(ctx.viewport_rect(), 0.0, Color32::BLACK);
    let mut view = View {
        ui,
        origin,
        unit,
        skin: &skin,
        textures: &textures,
        mask: None,
    };
    let letters = generic::letters(skin.sheet(Sheet::Gen), focused);
    let title_width = generic::title_width(&letters, TITLE);
    for (sprite, x, y) in generic::frame(width, height, title_width, focused) {
        view.sprite_at(sprite, x, y);
    }
    for (sprite, x, y) in generic::title(&letters, TITLE, width) {
        view.sprite_at(sprite, x, y);
    }

    let inside = view.rect(generic::inside(width, height));
    let window = Area::new(0, 0, width, height);
    let moving = picture(app, &mut view, inside, unit);

    // Everywhere moves the window, as the plain one does.
    let body = view.interact(window, "milkdrop-window", Sense::click_and_drag());
    if body.double_clicked() {
        app.actions.push(Action::MilkdropFullscreen);
    } else if body.secondary_clicked() {
        app.actions.push(Action::MilkdropEffect(1));
    } else if body.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
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
    let grip_area = generic::grip(width, height);
    let grip = view.interact(grip_area, "milkdrop-grip", Sense::drag());
    if grip.hovered() || grip.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }
    if grip.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(ResizeDirection::SouthEast));
        ctx.data_mut(|data| data.insert_temp(resized_id(), true));
    }
    if ctx.data(|data| data.get_temp::<bool>(resized_id())) == Some(true) {
        let picture = (size - chrome() * unit).max(vec2(
            crate::milkdrop::MIN_SIZE[0],
            crate::milkdrop::MIN_SIZE[1],
        ));
        if (vec2(app.settings.milkdrop_size[0], app.settings.milkdrop_size[1]) - picture)
            .abs()
            .max_elem()
            >= 1.0
        {
            app.actions
                .push(Action::SetMilkdropSize([picture.x, picture.y]));
        }
    }

    if moving {
        crate::theme::visualiser_frame(&ctx, Some(app.milkdrop_pane_fps()));
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

/// The window's own keys, the ones the plain window has for what it shows.
/// Everything else is the app's, as in its other windows.
fn read_keys(app: &mut App, ctx: &Context) {
    let pressed = |key: Key| ctx.input_mut(|input| input.consume_key(Modifiers::NONE, key));
    if pressed(Key::Escape) {
        app.actions.push(Action::ToggleWinampMilkdrop);
    }
    if pressed(Key::F) {
        app.actions.push(Action::MilkdropFullscreen);
    }
    if pressed(Key::N) || pressed(Key::ArrowRight) {
        app.actions.push(Action::MilkdropEffect(1));
    }
    if pressed(Key::P) || pressed(Key::ArrowLeft) {
        app.actions.push(Action::MilkdropEffect(-1));
    }
    crate::ui::keys::handle(app, ctx);
}
