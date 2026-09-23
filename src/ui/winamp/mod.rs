//! Winamp-style mini player using classic skins.
//!
//! The app has one main window at a time. Switching modes closes the current
//! window and `main` opens the other. The mini player is borderless and uses
//! nearest-neighbor scaling at an integer screen-pixel ratio. Its controls emit
//! the same actions as the main player. Stop pauses and rewinds; Eject and the
//! logo return to the main window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{
    Color32, Id, Pos2, Rect, Response, Sense, TextureId, Ui, Vec2, ViewportCommand, pos2, vec2,
};

use crate::app::{App, NowPlaying};
use crate::model::{Action, Page};
use crate::player::RepeatMode;
use crate::settings::VisMode;
use crate::skin::layout::{self, Area};
use crate::skin::{Mask, Sheet, Skin, Sprite, font, sprites};
use crate::util;
use crate::vis;
use crate::winamp::{MAX_SCALE, WinampState};

use super::widgets::{SliderEvent, wheel_notches};

mod equalizer;
mod pixel_text;
mod playlist;

pub use pixel_text::PixelText;

/// How often the visualiser moves.
const VIS_FRAME: Duration = Duration::from_micros(16_667);

fn visualiser_repaint_delay(vsync: bool) -> Duration {
    if vsync { VIS_FRAME } else { VIS_FRAME * 2 }
}

/// The stack's height in skin pixels: the main window, and the equalizer
/// and the playlist under it, whichever are open.
fn stack_height(settings: &crate::settings::Settings) -> u32 {
    let mut height = if settings.winamp_shaded {
        layout::SHADE_HEIGHT
    } else {
        layout::WINDOW_HEIGHT
    };
    if settings.eq_open {
        height += if settings.eq_shaded {
            layout::EQ_SHADE_HEIGHT
        } else {
            layout::EQ_HEIGHT
        };
    }
    if settings.playlist_open {
        height += if settings.playlist_shaded {
            layout::PLAYLIST_SHADE_HEIGHT
        } else {
            settings
                .playlist_height
                .clamp(layout::PLAYLIST_MIN_HEIGHT, layout::PLAYLIST_MAX_HEIGHT)
        };
    }
    height
}

/// The window's size in logical points, for the given points per skin
/// pixel.
pub fn window_size(settings: &crate::settings::Settings, unit: f32) -> Vec2 {
    vec2(layout::WINDOW_WIDTH as f32, stack_height(settings) as f32) * unit
}

/// A first guess at the window's size, before the display's scale is
/// known; the first frame corrects it.
pub fn initial_size(settings: &crate::settings::Settings) -> Vec2 {
    window_size(settings, WinampState::scale(settings, 1.0) as f32)
}

/// Skin files dropped on the window this frame.
pub fn dropped_skins(ctx: &egui::Context) -> Vec<PathBuf> {
    ctx.input(|input| {
        input
            .raw
            .dropped_files
            .iter()
            .map(|file| file.path().to_path_buf())
            .filter(|path| crate::winamp::is_skin_file(path))
            .collect()
    })
}

/// Logical points per skin pixel: a whole number of screen pixels.
fn unit(app: &App, ctx: &egui::Context) -> f32 {
    let pixels_per_point = ctx.pixels_per_point();
    WinampState::scale(&app.settings, pixels_per_point) as f32 / pixels_per_point
}

/// Fits the window to the skin after the display scale is known.
fn fit_window(ctx: &egui::Context, settings: &crate::settings::Settings, unit: f32) {
    let wanted = window_size(settings, unit);
    // Not `inner_rect`: Wayland reports no window positions, so that is
    // `None` there; the screen rect is the window's size on every desktop.
    let current = ctx.viewport_rect().size();
    if (current - wanted).abs().max_elem() < 1.0 {
        return;
    }
    // Retry rejected resize requests at most once per second.
    let asked = Id::new("winamp-fit-asked");
    let last: Option<f64> = ctx.data(|data| data.get_temp(asked));
    let now = ctx.input(|input| input.time);
    if last.is_some_and(|last| now - last < 1.0) {
        return;
    }
    ctx.data_mut(|data| data.insert_temp(asked, now));
    ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(wanted));
    ctx.send_viewport_cmd(ViewportCommand::MaxInnerSize(wanted));
    ctx.send_viewport_cmd(ViewportCommand::InnerSize(wanted));
}

/// Draws the skin's sprites into the window and reads the pointer against
/// the skin's layout.
struct View<'a> {
    ui: &'a mut Ui,
    origin: Pos2,
    unit: f32,
    skin: &'a Skin,
    textures: &'a HashMap<Sheet, TextureId>,
    /// The window's shape, when the skin is not a rectangle: nothing is
    /// painted outside it.
    mask: Option<&'a Mask>,
}

impl View<'_> {
    fn rect(&self, area: Area) -> Rect {
        Rect::from_min_size(
            self.origin + vec2(area.x as f32, area.y as f32) * self.unit,
            vec2(area.width as f32, area.height as f32) * self.unit,
        )
    }

    /// A pointer position as a skin x coordinate.
    fn skin_x(&self, pos: Pos2) -> f32 {
        (pos.x - self.origin.x) / self.unit
    }

    fn paint(&self, painter: &egui::Painter, sprite: Sprite, x: u32, y: u32) {
        let Some((bitmap, clipped)) = self.skin.sprite(sprite) else {
            return;
        };
        let Some(&texture) = self.textures.get(&sprite.sheet) else {
            return;
        };
        let (width, height) = (bitmap.width as f32, bitmap.height as f32);
        // A piece of the sprite, `dx` in and `columns` wide, on one row or
        // all of them.
        let piece = |dx: u32, dy: u32, columns: u32, rows: u32| {
            let uv = Rect::from_min_max(
                pos2(
                    (clipped.x + dx) as f32 / width,
                    (clipped.y + dy) as f32 / height,
                ),
                pos2(
                    (clipped.x + dx + columns) as f32 / width,
                    (clipped.y + dy + rows) as f32 / height,
                ),
            );
            let dest = Rect::from_min_size(
                self.origin + vec2((x + dx) as f32, (y + dy) as f32) * self.unit,
                vec2(columns as f32, rows as f32) * self.unit,
            );
            painter.image(texture, dest, uv, Color32::WHITE);
        };
        match self.mask {
            None => piece(0, 0, clipped.width, clipped.height),
            Some(mask) => {
                for dy in 0..clipped.height {
                    for (start, end) in mask.spans(y + dy) {
                        let from = (*start).max(x);
                        let to = (*end).min(x + clipped.width);
                        if to > from {
                            piece(from - x, dy, to - from, 1);
                        }
                    }
                }
            }
        }
    }

    fn sprite_at(&self, sprite: Sprite, x: u32, y: u32) {
        self.paint(self.ui.painter(), sprite, x, y);
    }

    /// A sprite cut to an area, for tiles that run past the edge.
    fn sprite_clipped(&self, sprite: Sprite, x: u32, y: u32, clip: Area) {
        let clip = self.rect(clip).intersect(self.ui.clip_rect());
        let painter = self.ui.painter().with_clip_rect(clip);
        self.paint(&painter, sprite, x, y);
    }

    /// A block of skin pixels in one colour.
    fn fill(&self, x: u32, y: u32, width: u32, height: u32, color: Color32) {
        let block = |x: u32, y: u32, width: u32, height: u32| {
            let rect = Rect::from_min_size(
                self.origin + vec2(x as f32, y as f32) * self.unit,
                vec2(width as f32, height as f32) * self.unit,
            );
            self.ui.painter().rect_filled(rect, 0.0, color);
        };
        match self.mask {
            None => block(x, y, width, height),
            Some(mask) => {
                for row in y..y + height {
                    for (start, end) in mask.spans(row) {
                        let from = (*start).max(x);
                        let to = (*end).min(x + width);
                        if to > from {
                            block(from, row, to - from, 1);
                        }
                    }
                }
            }
        }
    }

    fn sprite(&self, sprite: Sprite, area: Area) {
        self.sprite_at(sprite, area.x, area.y);
    }

    /// A line of the skin's bitmap font, cut off at the area's edge.
    fn text(&self, text: &str, area: Area) {
        let clip = self.rect(area).intersect(self.ui.clip_rect());
        let painter = self.ui.painter().with_clip_rect(clip);
        for (index, character) in text.chars().enumerate() {
            let x = area.x + 5 * index as u32;
            if x >= area.x + area.width {
                break;
            }
            self.paint(&painter, font::glyph(character), x, area.y);
        }
    }

    fn interact(&mut self, area: Area, id: &str, sense: Sense) -> Response {
        let rect = self.rect(area);
        self.ui.interact(rect, Id::new(("winamp", id)), sense)
    }

    /// A button drawn pressed while the pointer holds it down.
    fn button(&mut self, area: Area, normal: Sprite, pressed: Sprite, id: &str) -> Response {
        let response = self
            .interact(area, id, Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let sprite = if response.is_pointer_button_down_on() {
            pressed
        } else {
            normal
        };
        self.sprite(sprite, area);
        response
    }

    /// A button whose only sprite is its lit state, drawn over the
    /// background while it is on or held.
    fn lamp_button(&mut self, area: Area, lit: Sprite, on: bool, id: &str) -> Response {
        let response = self
            .interact(area, id, Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if on || response.is_pointer_button_down_on() {
            self.sprite(lit, area);
        }
        response
    }

    /// A slider along an area, its thumb `thumb` pixels wide: the pointer's
    /// position as a fraction of the thumb's travel.
    fn slider(&mut self, area: Area, id: &str, thumb: u32) -> (Response, SliderEvent) {
        let response = self
            .interact(area, id, Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let memory = Id::new(("winamp-slider", id));
        let dragging = self.ui.data(|data| data.get_temp::<f32>(memory));
        let travel = (area.width - thumb) as f32;
        let pointer = response.interact_pointer_pos().map(|pos| {
            ((self.skin_x(pos) - area.x as f32 - thumb as f32 / 2.0) / travel).clamp(0.0, 1.0)
        });
        let mut event = SliderEvent::None;
        if (response.drag_started() || response.dragged())
            && let Some(value) = pointer
        {
            self.ui.data_mut(|data| data.insert_temp(memory, value));
            event = SliderEvent::Dragging(value);
        }
        if response.drag_stopped() {
            if let Some(value) = dragging.or(pointer) {
                event = SliderEvent::Committed(value);
            }
            self.ui.data_mut(|data| data.remove::<f32>(memory));
        } else if response.clicked()
            && let Some(value) = pointer
        {
            event = SliderEvent::Committed(value);
        }
        (response, event)
    }
}

/// The whole window, drawn into the root.
pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let unit = unit(app, &ctx);
    fit_window(&ctx, &app.settings, unit);
    let origin = ui.max_rect().min;
    let (outer, focused) = ctx.input(|input| {
        let viewport = input.viewport();
        (viewport.outer_rect, viewport.focused)
    });
    if let Some(rect) = outer {
        app.winamp.last_pos = Some([rect.min.x, rect.min.y]);
    }
    let focused = focused.unwrap_or(true);
    super::keys::handle(app, &ctx);
    for path in dropped_skins(&ctx) {
        app.actions.push(Action::InstallSkin(path));
    }

    let textures = app.winamp.textures(&ctx);
    let skin = app.winamp.skin.clone();
    let now = app.now_playing();
    let time = ctx.input(|input| input.time);
    let shaded = app.settings.winamp_shaded;
    let mask = if shaded {
        skin.regions.shade.as_ref()
    } else {
        skin.regions.normal.as_ref()
    };
    let mut view = View {
        ui,
        origin,
        unit,
        skin: &skin,
        textures: &textures,
        mask,
    };
    let vis_moving = if shaded {
        shade_bar(app, &mut view, &ctx, focused, now.as_ref());
        false
    } else {
        full_window(app, &mut view, &ctx, focused, now.as_ref(), time)
    };

    // The other windows hang under this one in Winamp's order.
    let mut below_y = if shaded {
        layout::SHADE_HEIGHT
    } else {
        layout::WINDOW_HEIGHT
    };
    if app.settings.eq_open {
        let eq_shaded = app.settings.eq_shaded;
        let mut below = View {
            ui: view.ui,
            origin: origin + vec2(0.0, below_y as f32 * unit),
            unit,
            skin: &skin,
            textures: &textures,
            mask: if eq_shaded {
                skin.regions.equalizer_shade.as_ref()
            } else {
                skin.regions.equalizer.as_ref()
            },
        };
        equalizer::show(app, &mut below, now.as_ref(), focused);
        below_y += if eq_shaded {
            layout::EQ_SHADE_HEIGHT
        } else {
            layout::EQ_HEIGHT
        };
    }
    if app.settings.playlist_open {
        let mut below = View {
            ui: view.ui,
            origin: origin + vec2(0.0, below_y as f32 * unit),
            unit,
            skin: &skin,
            textures: &textures,
            mask: None,
        };
        playlist::show(app, &mut below, now.as_ref(), focused);
    }

    // egui subtracts one predicted frame from delayed repaints. With VSync,
    // the next buffer swap supplies that frame; without it, request two.
    if vis_moving {
        ctx.request_repaint_after(visualiser_repaint_delay(cfg!(target_os = "macos")));
    } else if now.is_some() {
        ctx.request_repaint_after(Duration::from_millis(220));
    }
}

/// The main window as it usually is. Returns whether the visualiser is
/// still moving.
fn full_window(
    app: &mut App,
    view: &mut View,
    ctx: &egui::Context,
    focused: bool,
    now: Option<&NowPlaying>,
    time: f64,
) -> bool {
    view.sprite(
        sprites::MAIN_BACKGROUND,
        Area::new(0, 0, layout::WINDOW_WIDTH, layout::WINDOW_HEIGHT),
    );
    title_bar(app, view, ctx, focused);
    clutter_bar(app, view, now);
    status(app, view, now);
    time_display(app, view, now, time);
    let vis_moving = visualiser(app, view, now);
    marquee(app, view, now);
    rates(app, view, now);
    sliders(app, view, now);
    windows_buttons(app, view);
    transport(app, view, now);
    shuffle_repeat(app, view, now);
    if view
        .interact(layout::ABOUT, "about", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(super::keys::platform_shortcut(
            "Back to the big window (Ctrl+M)",
            "Back to the big window (Cmd+Shift+M)",
        ))
        .clicked()
    {
        app.actions.push(Action::ToggleWinampWindow);
    }

    vis_moving
}

/// The main window rolled up: the bar with the time in the small font,
/// a little transport, and a little seek bar, as Winamp's shade mode had.
fn shade_bar(
    app: &mut App,
    view: &mut View,
    ctx: &egui::Context,
    focused: bool,
    now: Option<&NowPlaying>,
) {
    let bar = if focused {
        sprites::SHADE_BAR_ACTIVE
    } else {
        sprites::SHADE_BAR_INACTIVE
    };
    view.sprite(
        bar,
        Area::new(0, 0, layout::WINDOW_WIDTH, layout::SHADE_HEIGHT),
    );
    let title = view.interact(
        Area::new(0, 0, layout::WINDOW_WIDTH, layout::SHADE_HEIGHT),
        "shade-bar",
        Sense::click_and_drag(),
    );
    if title.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if title.double_clicked() {
        app.actions.push(Action::ToggleWinampShade);
    }
    let unit = view.unit;
    menu(egui::Popup::context_menu(&title), view.skin, unit, |ui| {
        options_menu(app, ui, unit);
    });
    let big_window = super::keys::platform_shortcut(
        "Back to the big window (Ctrl+M)",
        "Back to the big window (Cmd+Shift+M)",
    );
    if view
        .button(
            layout::OPTIONS_BUTTON,
            sprites::OPTIONS_BUTTON,
            sprites::OPTIONS_BUTTON_PRESSED,
            "logo",
        )
        .on_hover_text(big_window)
        .clicked()
    {
        app.actions.push(Action::ToggleWinampWindow);
    }
    if view
        .button(
            layout::MINIMIZE_BUTTON,
            sprites::MINIMIZE_BUTTON,
            sprites::MINIMIZE_BUTTON_PRESSED,
            "minimize",
        )
        .clicked()
    {
        crate::window::minimize_window(ctx);
    }
    if view
        .button(
            layout::SHADE_BUTTON,
            sprites::UNSHADE_BUTTON,
            sprites::UNSHADE_BUTTON_PRESSED,
            "unshade",
        )
        .on_hover_text("Roll the window down")
        .clicked()
    {
        app.actions.push(Action::ToggleWinampShade);
    }
    if view
        .button(
            layout::CLOSE_BUTTON,
            sprites::CLOSE_BUTTON,
            sprites::CLOSE_BUTTON_PRESSED,
            "close",
        )
        .on_hover_text(big_window)
        .clicked()
    {
        app.actions.push(Action::ToggleWinampWindow);
    }

    // The time, in the small font; a click counts down instead.
    if view
        .interact(layout::SHADE_TIME, "shade-time", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        app.winamp.time_remaining = !app.winamp.time_remaining;
    }
    let playing = now.is_some_and(|now| now.playing);
    if let Some(now) = now.filter(|now| !stopped(Some(now))) {
        let position = match app.seek_preview {
            Some(fraction) => (fraction * now.duration_ms as f32) as u32,
            None => now.position_ms,
        };
        let remaining = app.winamp.time_remaining && now.duration_ms > 0;
        let shown = if remaining {
            now.duration_ms.saturating_sub(position)
        } else {
            position
        };
        let text = format!(
            "{}{}",
            if remaining { "-" } else { " " },
            util::format_duration_ms(shown)
        );
        view.text(&text, layout::SHADE_TIME);
    }

    // The little transport: painted into the bar, so these only listen.
    let mini: [(&str, Area); 6] = [
        ("previous", layout::SHADE_PREVIOUS),
        ("play", layout::SHADE_PLAY),
        ("pause", layout::SHADE_PAUSE),
        ("stop", layout::SHADE_STOP),
        ("next", layout::SHADE_NEXT),
        ("eject", layout::SHADE_EJECT),
    ];
    for (name, area) in mini {
        let response = view
            .interact(area, &format!("shade-{name}"), Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if !response.clicked() {
            continue;
        }
        match name {
            "previous" => app.actions.push(Action::Previous),
            "play" => app.actions.push(if playing {
                Action::Seek(0)
            } else {
                Action::TogglePlay
            }),
            "pause" if now.is_some() => app.actions.push(Action::TogglePlay),
            "stop" if now.is_some() => {
                if playing {
                    app.actions.push(Action::TogglePlay);
                }
                app.actions.push(Action::Seek(0));
            }
            "next" => app.actions.push(Action::Next),
            "eject" => app.actions.push(Action::ToggleWinampWindow),
            _ => {}
        }
    }

    // The little seek bar.
    view.sprite(sprites::SHADE_POSITION_TRACK, layout::SHADE_POSITION);
    let Some(now) = now.filter(|now| now.duration_ms > 0 && !stopped(Some(now))) else {
        return;
    };
    let (response, event) = view.slider(layout::SHADE_POSITION, "shade-position", 3);
    match event {
        SliderEvent::Dragging(value) => app.seek_preview = Some(value),
        SliderEvent::Committed(value) => {
            app.seek_preview = None;
            app.actions
                .push(Action::Seek((value * now.duration_ms as f32) as u32));
        }
        SliderEvent::None => {}
    }
    let fraction = app
        .seek_preview
        .unwrap_or(now.position_ms as f32 / now.duration_ms as f32)
        .clamp(0.0, 1.0);
    let travel = layout::SHADE_POSITION.width - 3;
    let thumb = if response.dragged() {
        sprites::SHADE_POSITION_THUMB_RIGHT
    } else {
        sprites::SHADE_POSITION_THUMB
    };
    view.sprite_at(
        thumb,
        layout::SHADE_POSITION.x + (fraction * travel as f32).round() as u32,
        layout::SHADE_POSITION.y,
    );
}

fn title_bar(app: &mut App, view: &mut View, ctx: &egui::Context, focused: bool) {
    let bar = if focused {
        sprites::TITLE_BAR_ACTIVE
    } else {
        sprites::TITLE_BAR_INACTIVE
    };
    view.sprite(bar, layout::TITLE_BAR);
    let title = view.interact(layout::TITLE_BAR, "title", Sense::click_and_drag());
    if title.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if title.double_clicked() {
        app.actions.push(Action::ToggleWinampShade);
    }
    let unit = view.unit;
    menu(egui::Popup::context_menu(&title), view.skin, unit, |ui| {
        options_menu(app, ui, unit);
    });
    // The logo and the close button lead back to the big window: the mini
    // player is a way of looking at the same app, not a second one to
    // close. Quitting is in the menu and through the platform shortcut.
    let big_window = super::keys::platform_shortcut(
        "Back to the big window (Ctrl+M)",
        "Back to the big window (Cmd+Shift+M)",
    );
    if view
        .button(
            layout::OPTIONS_BUTTON,
            sprites::OPTIONS_BUTTON,
            sprites::OPTIONS_BUTTON_PRESSED,
            "logo",
        )
        .on_hover_text(big_window)
        .clicked()
    {
        app.actions.push(Action::ToggleWinampWindow);
    }
    if view
        .button(
            layout::MINIMIZE_BUTTON,
            sprites::MINIMIZE_BUTTON,
            sprites::MINIMIZE_BUTTON_PRESSED,
            "minimize",
        )
        .clicked()
    {
        crate::window::minimize_window(ctx);
    }
    if view
        .button(
            layout::SHADE_BUTTON,
            sprites::SHADE_BUTTON,
            sprites::SHADE_BUTTON_PRESSED,
            "shade",
        )
        .on_hover_text("Roll the window up")
        .clicked()
    {
        app.actions.push(Action::ToggleWinampShade);
    }
    if view
        .button(
            layout::CLOSE_BUTTON,
            sprites::CLOSE_BUTTON,
            sprites::CLOSE_BUTTON_PRESSED,
            "close",
        )
        .on_hover_text(big_window)
        .clicked()
    {
        app.actions.push(Action::ToggleWinampWindow);
    }
}

/// The menu behind a right-click on the title bar and the O of the
/// clutter bar, sized to the skin so it fits inside the window.
fn options_menu(app: &mut App, ui: &mut Ui, unit: f32) {
    let font = menu_font(unit);
    ui.set_min_width(font * 11.0);
    let scale = WinampState::scale(&app.settings, ui.ctx().pixels_per_point());
    ui.horizontal(|ui| {
        ui.label("Size");
        for candidate in 1..=MAX_SCALE {
            if ui
                .selectable_label(candidate == scale, format!("{candidate}x"))
                .clicked()
            {
                app.actions.push(Action::SetSkinScale(candidate as u8));
            }
        }
    });
    ui.add_enabled_ui(app.window_level_supported, |ui| {
        let mut on_top = app.settings.winamp_on_top && app.window_level_supported;
        if ui
            .checkbox(&mut on_top, "Always on top")
            .on_disabled_hover_text(crate::window::ON_TOP_UNAVAILABLE)
            .clicked()
        {
            app.actions.push(Action::ToggleWinampOnTop);
        }
    });
    if app.windows_controls_visible() {
        let mut visible = app.settings.winamp_show_taskbar;
        if ui.checkbox(&mut visible, "Show in taskbar").changed() {
            app.actions.push(Action::SetWinampTaskbar(visible));
        }
    }
    let mut milkdrop = app.settings.milkdrop_open;
    if ui
        .checkbox(&mut milkdrop, "MilkDrop")
        .on_hover_text(super::keys::MILKDROP_SHORTCUT)
        .clicked()
    {
        app.actions.push(Action::ToggleWinampMilkdrop);
    }
    if ui.button("Choose a skin").clicked() {
        app.actions.push(Action::Open(Page::Settings));
        app.actions.push(Action::ToggleWinampWindow);
    }
    if ui.button("Big window").clicked() {
        app.actions.push(Action::ToggleWinampWindow);
    }
    if ui.button("Quit").clicked() {
        app.actions.push(Action::Quit);
    }
}

/// Sizes a menu to the skin, so it fits inside the window at any scale;
/// returns the font size it settled on.
/// A menu for the mini player: the skin's playlist colours on a square
/// frame, type that follows the scale, and never taller than the window.
/// A long list, such as the presets or the playlists, scrolls inside
/// instead of running off the screen and being cut where the window
/// ends. Classic Winamp used the system's menus and a skin says nothing
/// about them, so the playlist's colours are the nearest thing it says
/// about text on a background.
pub(super) fn menu<R>(
    popup: egui::Popup<'_>,
    skin: &Skin,
    unit: f32,
    contents: impl FnOnce(&mut Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    let rgb = |[r, g, b]: [u8; 3]| egui::Color32::from_rgb(r, g, b);
    let text = rgb(skin.playlist.normal);
    let current = rgb(skin.playlist.current);
    let background = rgb(skin.playlist.normal_background);
    let selected = rgb(skin.playlist.selected_background);
    let font = menu_font(unit);
    let margin = unit.max(1.0).round();
    let style = move |style: &mut egui::Style| {
        for text_style in [egui::TextStyle::Body, egui::TextStyle::Button] {
            style
                .text_styles
                .insert(text_style, egui::FontId::proportional(font));
        }
        style.spacing.item_spacing = vec2(4.0, 1.0);
        style.spacing.button_padding = vec2(6.0, 1.0);
        // A row is its text and padding, not egui's 18 points.
        style.spacing.interact_size = vec2(font * 2.0, font + 2.0);
        style.spacing.menu_margin = egui::Margin::same(margin as i8);
        let visuals = &mut style.visuals;
        visuals.window_fill = background;
        visuals.panel_fill = background;
        visuals.window_stroke = egui::Stroke::new(1.0, text.gamma_multiply(0.5));
        visuals.window_corner_radius = egui::CornerRadius::ZERO;
        visuals.menu_corner_radius = egui::CornerRadius::ZERO;
        visuals.window_shadow = egui::Shadow::NONE;
        visuals.popup_shadow = egui::Shadow::NONE;
        visuals.override_text_color = None;
        visuals.selection.bg_fill = selected;
        visuals.selection.stroke = egui::Stroke::new(1.0, current);
        let widgets = &mut visuals.widgets;
        for state in [&mut widgets.noninteractive, &mut widgets.inactive] {
            state.fg_stroke.color = text;
            state.weak_bg_fill = background;
            state.bg_fill = background;
            state.bg_stroke = egui::Stroke::NONE;
        }
        for state in [&mut widgets.hovered, &mut widgets.active, &mut widgets.open] {
            state.fg_stroke.color = current;
            state.weak_bg_fill = selected;
            state.bg_fill = selected;
            state.bg_stroke = egui::Stroke::NONE;
            state.expansion = 0.0;
            state.corner_radius = egui::CornerRadius::ZERO;
        }
    };
    popup.style(style).show(|ui| {
        crate::autoscroll::show(
            ui,
            egui::ScrollArea::vertical().max_height(menu_limit(ui)),
            egui::Vec2b::new(false, true),
            contents,
        )
        .inner
    })
}

/// The type size of a menu at this scale.
pub(super) fn menu_font(unit: f32) -> f32 {
    (5.0 * unit).clamp(9.0, 14.0)
}

/// How tall a menu's contents may be before they scroll: the window less
/// the menu's own frame, so egui can always find it a place inside. A
/// submenu is a popup of its own and caps itself the same way.
pub(super) fn menu_limit(ui: &Ui) -> f32 {
    let frame = ui.spacing().menu_margin.sum().y + 6.0;
    (ui.ctx().content_rect().height() - frame).max(menu_font(1.0))
}

/// The O A I D V strip: options, always on top, info, double size, and
/// the visualiser. Each lights while held; A and D stay lit while on.
fn clutter_bar(app: &mut App, view: &mut View, now: Option<&NowPlaying>) {
    view.sprite(sprites::CLUTTER_BAR, layout::CLUTTER_BAR);
    let options = view.lamp_button(
        layout::CLUTTER_O,
        sprites::CLUTTER_O_LIT,
        false,
        "clutter-o",
    );
    let unit = view.unit;
    menu(egui::Popup::menu(&options), view.skin, unit, |ui| {
        options_menu(app, ui, unit);
    });
    if view
        .lamp_button(
            layout::CLUTTER_A,
            sprites::CLUTTER_A_LIT,
            app.settings.winamp_on_top && app.window_level_supported,
            "clutter-a",
        )
        .on_hover_text(if app.window_level_supported {
            "Always on top"
        } else {
            crate::window::ON_TOP_UNAVAILABLE
        })
        .clicked()
        && app.window_level_supported
    {
        app.actions.push(Action::ToggleWinampOnTop);
    }
    if view
        .lamp_button(
            layout::CLUTTER_I,
            sprites::CLUTTER_I_LIT,
            false,
            "clutter-i",
        )
        .on_hover_text("Song info")
        .clicked()
        && let Some(now) = now
    {
        if let Some(id) = &now.album_id {
            app.actions.push(Action::Open(Page::Album(id.clone())));
        } else if let Some(id) = &now.show_id {
            app.actions.push(Action::Open(Page::Show(id.clone())));
        }
        app.actions.push(Action::ToggleWinampWindow);
    }
    // D goes round the sizes worth having on today's displays, 2x to 4x;
    // 1x is in the menu for whoever wants it.
    let scale = WinampState::scale(&app.settings, view.ui.ctx().pixels_per_point());
    if view
        .lamp_button(
            layout::CLUTTER_D,
            sprites::CLUTTER_D_LIT,
            scale >= 2,
            "clutter-d",
        )
        .on_hover_text("Size: 2x, 3x, 4x")
        .clicked()
    {
        let next = if scale >= MAX_SCALE {
            2
        } else {
            scale.max(1) + 1
        };
        app.actions.push(Action::SetSkinScale(next as u8));
    }
    // V opened Winamp's visualisation menu; this one has the display's
    // three looks and MilkDrop.
    let vis = view
        .lamp_button(
            layout::CLUTTER_V,
            sprites::CLUTTER_V_LIT,
            app.settings.milkdrop_open,
            "clutter-v",
        )
        .on_hover_text("Visualisation");
    menu(egui::Popup::menu(&vis), view.skin, unit, |ui| {
        for (mode, label) in [
            (VisMode::Bars, "Spectrum analyser"),
            (VisMode::Scope, "Oscilloscope"),
            (VisMode::Off, "Nothing"),
        ] {
            if ui
                .selectable_label(app.settings.vis == mode, label)
                .clicked()
            {
                app.actions.push(Action::SetVisualiser(mode));
            }
        }
        ui.separator();
        let mut milkdrop = app.settings.milkdrop_open;
        if ui
            .checkbox(&mut milkdrop, "MilkDrop")
            .on_hover_text(super::keys::MILKDROP_SHORTCUT)
            .clicked()
        {
            app.actions.push(Action::ToggleWinampMilkdrop);
        }
    });
}

/// The display's left box: the spectrum analyser, the oscilloscope, or
/// nothing, in the skin's own colours. A click, or V, goes to the next.
/// Only sound from this computer passes the tap; a device across the room
/// leaves the bars flat. Returns whether anything is still moving.
fn visualiser(app: &mut App, view: &mut View, now: Option<&NowPlaying>) -> bool {
    let area = layout::VISUALIZER;
    if view
        .interact(area, "visualiser", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        app.actions.push(Action::CycleVisualiser);
    }
    let mode = app.settings.vis;
    if mode == VisMode::Off {
        return false;
    }
    let palette = view.skin.vis_colors;
    let color =
        |index: usize| Color32::from_rgb(palette[index][0], palette[index][1], palette[index][2]);
    view.fill(area.x, area.y, area.width, area.height, color(0));
    for y in (0..area.height).step_by(2) {
        for x in (0..area.width).step_by(2) {
            view.fill(area.x + x, area.y + y, 1, 1, color(1));
        }
    }
    let sounding = now.is_some_and(|now| (now.playing || now.loading) && now.local);
    match mode {
        VisMode::Bars => {
            let samples = if sounding {
                app.winamp.tap.window(vis::FFT_SAMPLES, vis::LAG)
            } else {
                vec![0.0; vis::FFT_SAMPLES]
            };
            let bars = app.winamp.analyser.step(&samples, Instant::now());
            for (index, bar) in bars.iter().enumerate() {
                let x = area.x + 4 * index as u32;
                for row in (vis::ROWS - bar.height)..vis::ROWS {
                    view.fill(
                        x,
                        area.y + u32::from(row),
                        3,
                        1,
                        color(2 + usize::from(row)),
                    );
                }
                if let Some(peak) = bar.peak {
                    let row = vis::ROWS - peak;
                    view.fill(x, area.y + u32::from(row), 3, 1, color(23));
                }
            }
            sounding || !app.winamp.analyser.settled()
        }
        VisMode::Scope => {
            let samples = if sounding {
                app.winamp.tap.window(vis::SCOPE_SAMPLES, vis::LAG)
            } else {
                vec![0.0; vis::SCOPE_SAMPLES]
            };
            let rows = vis::scope(&samples);
            let mut last = rows[0];
            for (x, &y) in rows.iter().enumerate() {
                let (mut top, mut bottom) = (y, last);
                if bottom < top {
                    (top, bottom) = (bottom + 1, top);
                }
                last = y;
                let shade = color(18 + vis::scope_shade(y));
                for row in top..=bottom {
                    view.fill(area.x + x as u32, area.y + u32::from(row), 1, 1, shade);
                }
            }
            sounding
        }
        VisMode::Off => false,
    }
}

/// Whether the player is stopped, as Winamp meant it: something loaded and
/// paused at the very start. Spotify has no stop, so this is what Stop
/// leaves behind, and what the display treats as stopped.
fn stopped(now: Option<&NowPlaying>) -> bool {
    now.is_some_and(|now| !now.playing && !now.loading && now.position_ms == 0)
}

/// The play, pause, and stop lamp, the work indicator, and the mono and
/// stereo lamps, which are a switch here: MONO folds the channels together.
fn status(app: &mut App, view: &mut View, now: Option<&NowPlaying>) {
    let status = match now {
        Some(now) if now.playing || now.loading => sprites::STATUS_PLAYING,
        Some(_) if !stopped(now) => sprites::STATUS_PAUSED,
        _ => sprites::STATUS_STOPPED,
    };
    view.sprite(status, layout::STATUS);
    let working = now.is_some_and(|now| now.loading);
    view.sprite(
        if working {
            sprites::WORK_INDICATOR_ON
        } else {
            sprites::WORK_INDICATOR_OFF
        },
        layout::WORK_INDICATOR,
    );
    let sounding = now.is_some() && !stopped(now);
    let mono = app.settings.mono;
    let (stereo, mono_lamp) = match (sounding, mono) {
        (false, _) => (sprites::STEREO_OFF, sprites::MONO_OFF),
        (true, true) => (sprites::STEREO_OFF, sprites::MONO_ON),
        (true, false) => (sprites::STEREO_ON, sprites::MONO_OFF),
    };
    view.sprite(stereo, layout::STEREO);
    view.sprite(mono_lamp, layout::MONO);
    if view
        .interact(layout::MONO, "mono", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Play in mono")
        .clicked()
        && !mono
    {
        app.actions.push(Action::ToggleMono);
    }
    if view
        .interact(layout::STEREO, "stereo", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Play in stereo")
        .clicked()
        && mono
    {
        app.actions.push(Action::ToggleMono);
    }
}

/// The time in the skin's digits: elapsed, or remaining with a minus sign,
/// blinking while paused, blank with nothing on.
fn time_display(app: &mut App, view: &mut View, now: Option<&NowPlaying>, time: f64) {
    let whole = Area::new(
        layout::MINUS_EX.x,
        layout::MINUS_EX.y,
        layout::SECOND_ONES.x + layout::SECOND_ONES.width - layout::MINUS_EX.x,
        layout::MINUS_EX.height,
    );
    if view
        .interact(whole, "time", Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        app.winamp.time_remaining = !app.winamp.time_remaining;
    }
    let extended = view.skin.has_extended_digits();
    // The blank digit is painted, not left out: what the main sheet has
    // under the digits is the skin's own idea of an empty display, which
    // is not always empty.
    let blank = |view: &mut View| {
        if extended {
            for cell in layout::TIME_DIGITS {
                view.sprite(sprites::NUMS_EX_BLANK, cell);
            }
            view.sprite(sprites::NUMS_EX_BLANK, layout::MINUS_EX);
        } else {
            for cell in layout::TIME_DIGITS {
                view.sprite(sprites::NUMBERS_BLANK, cell);
            }
            view.sprite(sprites::NUMBERS_NO_MINUS, layout::MINUS);
        }
    };
    let Some(now) = now.filter(|now| !stopped(Some(now))) else {
        blank(view);
        return;
    };
    let paused = !now.playing && !now.loading;
    if paused && (time * 2.0).floor() as i64 % 2 == 1 {
        blank(view);
        return;
    }
    let position = match app.seek_preview {
        Some(fraction) => (fraction * now.duration_ms as f32) as u32,
        None => now.position_ms,
    };
    let remaining = app.winamp.time_remaining && now.duration_ms > 0;
    let shown = if remaining {
        now.duration_ms.saturating_sub(position)
    } else {
        position
    };
    let seconds = shown / 1000;
    let minutes = (seconds / 60).min(99);
    let seconds = seconds % 60;
    let digits = [minutes / 10, minutes % 10, seconds / 10, seconds % 10];
    for (value, cell) in digits.into_iter().zip(layout::TIME_DIGITS) {
        let sprite = if extended {
            sprites::digit_ex(value)
        } else {
            sprites::digit(value)
        };
        view.sprite(sprite, cell);
    }
    match (extended, remaining) {
        (true, true) => view.sprite(sprites::NUMS_EX_MINUS, layout::MINUS_EX),
        (true, false) => view.sprite(sprites::NUMS_EX_BLANK, layout::MINUS_EX),
        (false, true) => view.sprite(sprites::NUMBERS_MINUS, layout::MINUS),
        (false, false) => view.sprite(sprites::NUMBERS_NO_MINUS, layout::MINUS),
    }
}

/// What the marquee says: a slider while it moves, as Winamp announced
/// them, a seek while it is dragged, else the song.
pub fn marquee_text(
    now: Option<&NowPlaying>,
    seek_preview: Option<f32>,
    volume_preview: Option<f32>,
    balance_preview: Option<f32>,
    notice: Option<&str>,
) -> String {
    if let Some(balance) = balance_preview {
        let percent = (balance.abs() * 100.0).round() as u32;
        return match balance {
            b if b < 0.0 => format!("Balance: {percent}% left"),
            b if b > 0.0 => format!("Balance: {percent}% right"),
            _ => "Balance: center".to_string(),
        };
    }
    if let Some(volume) = volume_preview {
        return format!("Volume: {}%", (volume * 100.0).round() as u32);
    }
    // The app's notices (a skin added, a playlist saved, an error) have
    // no toast to live in here; Winamp used the marquee for its own.
    if let Some(notice) = notice {
        return notice.to_string();
    }
    let Some(now) = now else {
        return "Spotifast".to_string();
    };
    if let Some(fraction) = seek_preview
        && now.duration_ms > 0
    {
        let target = (fraction * now.duration_ms as f32) as u32;
        return format!(
            "Seek to: {}/{} ({}%)",
            util::format_duration_ms(target),
            util::format_duration_ms(now.duration_ms),
            (fraction * 100.0).round() as u32
        );
    }
    let mut text = if now.subtitle.is_empty() {
        now.title.clone()
    } else {
        format!("{} - {}", now.subtitle, now.title)
    };
    if now.duration_ms > 0 {
        text.push_str(&format!(" ({})", util::format_duration_ms(now.duration_ms)));
    }
    text
}

fn marquee(app: &mut App, view: &mut View, now: Option<&NowPlaying>) {
    let notice = app.toasts.last().map(|toast| toast.message.clone());
    let text = marquee_text(
        now,
        app.seek_preview,
        app.volume_preview,
        app.winamp.balance_preview,
        notice.as_deref(),
    );
    let (shown, offset) = app.winamp.marquee(&text, Instant::now());
    if text.chars().all(font::covered) {
        view.text(&shown, layout::MARQUEE);
    } else {
        // The skin's bitmap font cannot say this (Japanese, say, came out
        // as question marks, #104): the whole line is rasterised in the
        // pixel face instead and slid past the window a cell at a time.
        marquee_pixels(app, view, &text, offset);
    }
}

/// A still line of text drawn from the pixel face, for skin areas whose
/// bitmap font cannot say it: scaled to the area's height, cut at its
/// edge, tinted in the playlist's colour.
fn pixel_line(app: &mut App, view: &mut View, text: &str, area: Area) {
    let ctx = view.ui.ctx().clone();
    let line = app.winamp.playlist_text.line(&ctx, text);
    let (texture, width, height) = (line.texture.id(), line.width, line.height);
    let (ink_top, ink_height) = (line.ink_top, line.ink_height);
    if width == 0 || ink_height == 0 {
        return;
    }
    let unit = view.unit;
    // Scale the ink, not the face's padded line, so the glyphs fill the bar.
    let scale = (area.height as f32 * unit) / ink_height as f32;
    let drawn = (width as f32 * scale).min(area.width as f32 * unit);
    let rect = view.rect(area);
    let colour = view.skin.playlist.normal;
    let tint = Color32::from_rgb(colour[0], colour[1], colour[2]);
    let image = egui::Rect::from_min_size(rect.min, vec2(drawn, area.height as f32 * unit));
    let uv = egui::Rect::from_min_max(
        egui::pos2(0.0, ink_top as f32 / height as f32),
        egui::pos2(
            drawn / (width as f32 * scale),
            (ink_top + ink_height) as f32 / height as f32,
        ),
    );
    view.ui.painter().image(texture, image, uv, tint);
}

/// The marquee drawn from the pixel face: the strip is rendered once,
/// scaled to the marquee's height, and a window of it shown, wrapping
/// through the gap the way the character marquee does.
fn marquee_pixels(app: &mut App, view: &mut View, text: &str, offset: usize) {
    let area = layout::MARQUEE;
    let scrolling = app.winamp.marquee_scrolling();
    let strip = if scrolling {
        crate::winamp::marquee_strip(text)
    } else {
        text.to_string()
    };
    let ctx = view.ui.ctx().clone();
    let line = app.winamp.playlist_text.line(&ctx, &strip);
    let (texture, width, height) = (line.texture.id(), line.width, line.height);
    let (ink_top, ink_height) = (line.ink_top, line.ink_height);
    if width == 0 || ink_height == 0 {
        return;
    }
    let unit = view.unit;
    // Scale the ink, not the face's padded line, so the glyphs fill the bar.
    let scale = (area.height as f32 * unit) / ink_height as f32;
    let strip_width = width as f32 * scale;
    let rect = view.rect(area);
    let painter = view.ui.painter_at(rect.intersect(view.ui.clip_rect()));
    let colour = view.skin.playlist.normal;
    let tint = Color32::from_rgb(colour[0], colour[1], colour[2]);
    let offset_px = if scrolling && strip_width > 0.0 {
        (offset as f32 * 5.0 * unit) % strip_width
    } else {
        0.0
    };
    let uv = egui::Rect::from_min_max(
        egui::pos2(0.0, ink_top as f32 / height as f32),
        egui::pos2(1.0, (ink_top + ink_height) as f32 / height as f32),
    );
    for copy in 0..2 {
        let left = rect.left() - offset_px + copy as f32 * strip_width;
        if left > rect.right() {
            break;
        }
        let image = egui::Rect::from_min_size(
            egui::pos2(left, rect.top()),
            vec2(strip_width, area.height as f32 * unit),
        );
        painter.image(texture, image, uv, tint);
        if !scrolling {
            break;
        }
    }
}

/// The bitrate and sample rate, as far as they are known: the bitrate is
/// the one chosen for this computer, so a device across the room shows
/// none.
fn rates(app: &App, view: &mut View, now: Option<&NowPlaying>) {
    let Some(now) = now.filter(|now| !stopped(Some(now))) else {
        return;
    };
    if now.local {
        view.text(&format!("{:>3}", app.settings.bitrate), layout::KBPS);
    }
    view.text("44", layout::KHZ);
}

fn sliders(app: &mut App, view: &mut View, now: Option<&NowPlaying>) {
    // Volume: the track is drawn at the level, the thumb rides on it.
    let volume = now
        .map(|now| now.volume_percent)
        .unwrap_or_else(|| crate::app::volume_to_percent(app.local.volume));
    let (response, event) = view.slider(layout::VOLUME, "volume", 14);
    let notches = wheel_notches(view.ui, &response);
    if notches != 0 {
        let level = (i32::from(volume) + 5 * notches).clamp(0, 100);
        app.actions.push(Action::SetVolume(level as u8));
    }
    match event {
        SliderEvent::Dragging(value) => {
            app.volume_preview = Some(value);
            if now.is_none_or(|now| now.local) {
                app.actions
                    .push(Action::PreviewVolume((value * 100.0).round() as u8));
            }
        }
        SliderEvent::Committed(value) => {
            app.volume_preview = None;
            app.actions
                .push(Action::SetVolume((value * 100.0).round() as u8));
        }
        SliderEvent::None => {}
    }
    let shown = match app.volume_preview {
        Some(fraction) => (fraction * 100.0).round() as u32,
        None => u32::from(volume),
    };
    let frame = (shown * (sprites::SLIDER_FRAMES - 1) + 50) / 100;
    view.sprite(sprites::volume_frame(frame), layout::VOLUME);
    let thumb = if response.dragged() || response.is_pointer_button_down_on() {
        sprites::VOLUME_THUMB_PRESSED
    } else {
        sprites::VOLUME_THUMB
    };
    let thumb_x = layout::VOLUME.x + (shown * layout::VOLUME_TRAVEL + 50) / 100;
    view.sprite_at(thumb, thumb_x, layout::VOLUME.y + 1);

    // Balance: the channels' gains in the sound path, with Winamp's snap
    // to the centre.
    let (response, event) = view.slider(layout::BALANCE, "balance", 14);
    match event {
        SliderEvent::Dragging(value) => {
            let balance = balance_of(value);
            app.winamp.balance_preview = Some(balance);
            app.actions.push(Action::SetBalance(balance));
        }
        SliderEvent::Committed(value) => {
            app.winamp.balance_preview = None;
            app.actions.push(Action::SetBalance(balance_of(value)));
        }
        SliderEvent::None => {}
    }
    let balance = app.winamp.balance_preview.unwrap_or(app.settings.balance);
    let frame = (balance.abs() * (sprites::SLIDER_FRAMES - 1) as f32).round() as u32;
    view.sprite(sprites::balance_frame(frame), layout::BALANCE);
    let thumb = if response.dragged() || response.is_pointer_button_down_on() {
        sprites::BALANCE_THUMB_PRESSED
    } else {
        sprites::BALANCE_THUMB
    };
    let thumb_x =
        layout::BALANCE.x + ((balance + 1.0) / 2.0 * layout::BALANCE_TRAVEL as f32).round() as u32;
    view.sprite_at(thumb, thumb_x, layout::BALANCE.y + 1);

    // The seek bar. The thumb only exists while something plays, as in
    // Winamp, so an empty or stopped player has nothing to drag.
    view.sprite(sprites::POSITION_TRACK, layout::POSITION);
    let Some(now) = now.filter(|now| now.duration_ms > 0 && !stopped(Some(now))) else {
        return;
    };
    let (response, event) = view.slider(layout::POSITION, "position", 29);
    match event {
        SliderEvent::Dragging(value) => app.seek_preview = Some(value),
        SliderEvent::Committed(value) => {
            app.seek_preview = None;
            app.actions
                .push(Action::Seek((value * now.duration_ms as f32) as u32));
        }
        SliderEvent::None => {}
    }
    let fraction = app
        .seek_preview
        .unwrap_or(now.position_ms as f32 / now.duration_ms as f32)
        .clamp(0.0, 1.0);
    let thumb = if response.dragged() || response.is_pointer_button_down_on() {
        sprites::POSITION_THUMB_PRESSED
    } else {
        sprites::POSITION_THUMB
    };
    let thumb_x = layout::POSITION.x + (fraction * layout::POSITION_TRAVEL as f32).round() as u32;
    view.sprite_at(thumb, thumb_x, layout::POSITION.y);
}

/// A slider position as a balance, -1 to 1, snapping to the centre the
/// way Winamp's did.
fn balance_of(value: f32) -> f32 {
    let balance = value * 2.0 - 1.0;
    if balance.abs() < 0.08 { 0.0 } else { balance }
}

/// The EQ and PL toggles, each lit while its window hangs below.
fn windows_buttons(app: &mut App, view: &mut View) {
    let (normal, pressed) = if app.settings.eq_open {
        (sprites::EQ_ON, sprites::EQ_ON_PRESSED)
    } else {
        (sprites::EQ_OFF, sprites::EQ_OFF_PRESSED)
    };
    if view
        .button(layout::EQ_BUTTON, normal, pressed, "equalizer")
        .clicked()
    {
        app.actions.push(Action::ToggleWinampEq);
    }
    let (normal, pressed) = if app.settings.playlist_open {
        (sprites::PLAYLIST_ON, sprites::PLAYLIST_ON_PRESSED)
    } else {
        (sprites::PLAYLIST_OFF, sprites::PLAYLIST_OFF_PRESSED)
    };
    if view
        .button(layout::PLAYLIST_BUTTON, normal, pressed, "playlist")
        .clicked()
    {
        app.actions.push(Action::ToggleWinampPlaylist);
    }
}

fn transport(app: &mut App, view: &mut View, now: Option<&NowPlaying>) {
    let playing = now.is_some_and(|now| now.playing);
    if view
        .button(
            layout::PREVIOUS,
            sprites::PREVIOUS,
            sprites::PREVIOUS_PRESSED,
            "previous",
        )
        .clicked()
    {
        app.actions.push(Action::Previous);
    }
    // Play sits pressed in while the music plays, pause while it waits.
    let play = if playing {
        sprites::PLAY_PRESSED
    } else {
        sprites::PLAY
    };
    if view
        .button(layout::PLAY, play, sprites::PLAY_PRESSED, "play")
        .clicked()
    {
        // Play on a playing song starts it over, as it did.
        if playing {
            app.actions.push(Action::Seek(0));
        } else {
            app.actions.push(Action::TogglePlay);
        }
    }
    let pause = if now.is_some() && !playing && !stopped(now) {
        sprites::PAUSE_PRESSED
    } else {
        sprites::PAUSE
    };
    if view
        .button(layout::PAUSE, pause, sprites::PAUSE_PRESSED, "pause")
        .clicked()
        && now.is_some()
    {
        app.actions.push(Action::TogglePlay);
    }
    let stop = if stopped(now) {
        sprites::STOP_PRESSED
    } else {
        sprites::STOP
    };
    if view
        .button(layout::STOP, stop, sprites::STOP_PRESSED, "stop")
        .clicked()
        && now.is_some()
    {
        if playing {
            app.actions.push(Action::TogglePlay);
        }
        app.actions.push(Action::Seek(0));
    }
    if view
        .button(layout::NEXT, sprites::NEXT, sprites::NEXT_PRESSED, "next")
        .clicked()
    {
        app.actions.push(Action::Next);
    }
    if view
        .button(
            layout::EJECT,
            sprites::EJECT,
            sprites::EJECT_PRESSED,
            "eject",
        )
        .on_hover_text("Open the big window")
        .clicked()
    {
        app.actions.push(Action::ToggleWinampWindow);
    }
}

fn shuffle_repeat(app: &mut App, view: &mut View, now: Option<&NowPlaying>) {
    let shuffle = now.is_some_and(|now| now.shuffle);
    let (normal, pressed) = if shuffle {
        (sprites::SHUFFLE_ON, sprites::SHUFFLE_ON_PRESSED)
    } else {
        (sprites::SHUFFLE_OFF, sprites::SHUFFLE_OFF_PRESSED)
    };
    if view
        .button(layout::SHUFFLE, normal, pressed, "shuffle")
        .clicked()
    {
        app.actions.push(Action::ToggleShuffle);
    }
    let repeat = now.is_some_and(|now| now.repeat != RepeatMode::Off);
    let (normal, pressed) = if repeat {
        (sprites::REPEAT_ON, sprites::REPEAT_ON_PRESSED)
    } else {
        (sprites::REPEAT_OFF, sprites::REPEAT_OFF_PRESSED)
    };
    if view
        .button(layout::REPEAT, normal, pressed, "repeat")
        .clicked()
    {
        // Winamp's repeat is on or off; on means the whole list.
        app.actions.push(Action::SetRepeat(if repeat {
            RepeatMode::Off
        } else {
            RepeatMode::Context
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visualiser_repaint_delay_accounts_for_vsync() {
        assert_eq!(visualiser_repaint_delay(true), VIS_FRAME);
        assert_eq!(visualiser_repaint_delay(false), VIS_FRAME * 2);
    }

    fn now(title: &str, subtitle: &str, duration_ms: u32) -> NowPlaying {
        NowPlaying {
            local: true,
            device_name: None,
            uri: "spotify:track:x".into(),
            id: None,
            title: title.into(),
            artists: Vec::new(),
            subtitle: subtitle.into(),
            album_name: String::new(),
            album_id: None,
            show_id: None,
            art_url: None,
            art_small: None,
            duration_ms,
            position_ms: 0,
            playing: true,
            loading: false,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume_percent: 50,
            can_control: true,
            is_episode: false,
            resuming: false,
        }
    }

    #[test]
    fn the_marquee_names_the_song_the_way_winamp_did() {
        let playing = now("Karma Police", "Radiohead", 264_000);
        assert_eq!(
            marquee_text(Some(&playing), None, None, None, None),
            "Radiohead - Karma Police (4:24)"
        );
        assert_eq!(marquee_text(None, None, None, None, None), "Spotifast");
        let untitled = now("Episode 12", "", 0);
        assert_eq!(
            marquee_text(Some(&untitled), None, None, None, None),
            "Episode 12"
        );
    }

    #[test]
    fn a_seek_in_progress_says_where_it_is_going() {
        let playing = now("Karma Police", "Radiohead", 264_000);
        assert_eq!(
            marquee_text(Some(&playing), Some(0.5), None, None, None),
            "Seek to: 2:12/4:24 (50%)"
        );
    }

    #[test]
    fn sliders_announce_themselves_while_they_move() {
        let playing = now("Karma Police", "Radiohead", 264_000);
        assert_eq!(
            marquee_text(Some(&playing), None, Some(0.57), None, None),
            "Volume: 57%"
        );
        assert_eq!(
            marquee_text(Some(&playing), None, None, Some(-0.25), None),
            "Balance: 25% left"
        );
        assert_eq!(
            marquee_text(None, None, None, Some(0.0), None),
            "Balance: center"
        );
        assert_eq!(
            marquee_text(Some(&playing), None, None, None, Some("Added Zaxon skin")),
            "Added Zaxon skin"
        );
        assert_eq!(balance_of(0.5), 0.0);
        assert_eq!(balance_of(0.52), 0.0);
        assert!((balance_of(1.0) - 1.0).abs() < 1e-6);
        assert!(stopped(Some(&NowPlaying {
            playing: false,
            position_ms: 0,
            ..playing.clone()
        })));
        assert!(!stopped(Some(&playing)));
    }

    #[test]
    fn the_first_size_is_a_whole_multiple_of_the_skin() {
        let mut settings = crate::settings::Settings::default();
        assert_eq!(initial_size(&settings), vec2(550.0, 232.0));
        settings.skin_scale = Some(1);
        assert_eq!(initial_size(&settings), vec2(275.0, 116.0));
        settings.playlist_open = true;
        assert_eq!(initial_size(&settings), vec2(275.0, 290.0));
        settings.eq_open = true;
        assert_eq!(initial_size(&settings), vec2(275.0, 406.0));
        settings.eq_shaded = true;
        assert_eq!(initial_size(&settings), vec2(275.0, 304.0));
    }
}
