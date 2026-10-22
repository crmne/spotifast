//! A Windows Media Player skin in the mini player's window.
//!
//! The skin is one picture with buttons, sliders and text laid on it, all in
//! the skin's own pixels. There is no playlist, equalizer or shade mode:
//! those belong to Winamp skins.

use std::collections::HashMap;

use egui::{
    Align2, Color32, FontId, Id, Pos2, Rect, Sense, TextureId, Ui, ViewportCommand, pos2, vec2,
};

use crate::app::{App, NowPlaying};
use crate::model::Action;
use crate::settings::VisMode;
use crate::skin::wmp::{Align, Box2, Button, Control, Field, Slider, SliderKind, Text, Wmp};
use crate::vis;
use crate::winamp::MAX_SCALE;

use super::{fit_window, menu, options_menu, stopped};

/// How big a skin's `fontSize` is on screen, relative to a skin pixel:
/// the skin means points, and a point is a little more than a pixel.
const POINT: f32 = 1.3;

/// Logical points per skin pixel. A skin is drawn for a screen of its day,
/// so by default one skin pixel is one pixel of this display, rather than
/// the doubling Winamp's tiny skins get.
fn unit(app: &App, ctx: &egui::Context) -> f32 {
    let pixels_per_point = ctx.pixels_per_point();
    let chosen = app
        .settings
        .skin_scale
        .map(u32::from)
        .unwrap_or_else(|| pixels_per_point.round().max(1.0) as u32)
        .clamp(1, MAX_SCALE);
    chosen as f32 / pixels_per_point
}

struct Canvas<'a> {
    origin: Pos2,
    unit: f32,
    textures: &'a HashMap<String, TextureId>,
    wmp: &'a Wmp,
}

impl Canvas<'_> {
    fn rect(&self, area: Box2) -> Rect {
        Rect::from_min_size(
            self.origin + vec2(area.x as f32, area.y as f32) * self.unit,
            vec2(area.width as f32, area.height as f32) * self.unit,
        )
    }

    /// A picture at its own size, from a corner.
    fn picture(&self, ui: &Ui, key: &Option<String>, x: i32, y: i32) {
        let (Some(bitmap), Some(texture)) = (
            self.wmp.image(key),
            key.as_ref().and_then(|key| self.textures.get(key)),
        ) else {
            return;
        };
        let rect = self.rect(Box2 {
            x,
            y,
            width: bitmap.width as i32,
            height: bitmap.height as i32,
        });
        ui.painter().image(
            *texture,
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    /// The part of a picture a box covers, with the picture's corner at
    /// `(x, y)`, drawn where it belongs.
    fn piece(&self, ui: &Ui, key: &Option<String>, corner: (i32, i32), part: Box2) {
        let (Some(bitmap), Some(texture)) = (
            self.wmp.image(key),
            key.as_ref().and_then(|key| self.textures.get(key)),
        ) else {
            return;
        };
        let (width, height) = (bitmap.width as f32, bitmap.height as f32);
        let (x, y) = (part.x - corner.0, part.y - corner.1);
        let uv = Rect::from_min_max(
            pos2(
                (x as f32 / width).clamp(0.0, 1.0),
                (y as f32 / height).clamp(0.0, 1.0),
            ),
            pos2(
                ((x + part.width) as f32 / width).clamp(0.0, 1.0),
                ((y + part.height) as f32 / height).clamp(0.0, 1.0),
            ),
        );
        ui.painter()
            .image(*texture, self.rect(part), uv, Color32::WHITE);
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let skin = app.winamp.skin.clone();
    let Some(wmp) = skin.wmp.as_ref() else {
        return;
    };
    let unit = unit(app, &ctx);
    fit_window(&ctx, vec2(wmp.width as f32, wmp.height as f32) * unit);
    super::super::keys::handle(app, &ctx);
    for path in super::dropped_skins(&ctx) {
        app.actions.push(Action::InstallSkin(path));
    }
    let textures = app.winamp.wmp_textures(&ctx);
    let canvas = Canvas {
        origin: ui.max_rect().min,
        unit,
        textures: &textures,
        wmp,
    };
    let now = app.now_playing();

    // Anything not a control moves the window; right-click for the menu.
    let window = Box2 {
        x: 0,
        y: 0,
        width: wmp.width as i32,
        height: wmp.height as i32,
    };
    let background = ui.interact(
        canvas.rect(window),
        Id::new("wmp-window"),
        Sense::click_and_drag(),
    );
    if background.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    menu(
        egui::Popup::context_menu(&background),
        &skin,
        unit.max(2.0),
        |ui| {
            options_menu(app, ui, unit.max(2.0));
        },
    );

    if let Some([r, g, b]) = wmp.background_color {
        ui.painter()
            .rect_filled(canvas.rect(window), 0.0, Color32::from_rgb(r, g, b));
    }
    canvas.picture(ui, &wmp.background, 0, 0);
    let mut pictures: Vec<_> = wmp.pictures.iter().collect();
    pictures.sort_by_key(|picture| picture.z);
    for picture in pictures {
        let key = Some(picture.key.clone());
        match (picture.tile, wmp.image(&key)) {
            (Some((width, height)), Some(bitmap)) if bitmap.width > 0 && bitmap.height > 0 => {
                // Repeated over the area, cut off at its edge.
                let (tile_width, tile_height) = (bitmap.width as i32, bitmap.height as i32);
                let mut y = 0;
                while y < height && (y / tile_height) < 200 {
                    let mut x = 0;
                    while x < width && (x / tile_width) < 200 {
                        let part = Box2 {
                            x: picture.x + x,
                            y: picture.y + y,
                            width: tile_width.min(width - x),
                            height: tile_height.min(height - y),
                        };
                        canvas.piece(ui, &key, (part.x, part.y), part);
                        x += tile_width;
                    }
                    y += tile_height;
                }
            }
            _ => canvas.picture(ui, &key, picture.x, picture.y),
        }
    }

    for (index, button) in wmp.buttons.iter().enumerate() {
        draw_button(app, ui, &canvas, button, index, now.as_ref());
    }
    for (index, slider) in wmp.sliders.iter().enumerate() {
        draw_slider(app, ui, &canvas, slider, index, now.as_ref());
    }
    let mut moving = false;
    for area in &wmp.effects {
        moving |= draw_effects(app, ui, &canvas, *area, &skin.vis_colors, now.as_ref());
    }
    for text in &wmp.texts {
        draw_text(app, ui, &canvas, text, now.as_ref());
    }

    if moving {
        crate::theme::visualiser_frame(&ctx, app.visualiser_fps());
    } else if now.as_ref().is_some_and(|now| now.playing) {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// The colour at a point of a mapped button's mapping picture is the
/// button's own.
fn on_mapped(canvas: &Canvas, button: &Button, point: Pos2) -> bool {
    let Some((origin, key, color)) = &button.mapped else {
        return true;
    };
    let Some(bitmap) = canvas.wmp.images.get(key) else {
        return false;
    };
    let x = ((point.x - canvas.origin.x) / canvas.unit).floor() as i32 - origin.x;
    let y = ((point.y - canvas.origin.y) / canvas.unit).floor() as i32 - origin.y;
    if x < 0 || y < 0 {
        return false;
    }
    bitmap
        .pixel(x as u32, y as u32)
        .is_some_and(|pixel| pixel[..3] == *color)
}

fn draw_button(
    app: &mut App,
    ui: &mut Ui,
    canvas: &Canvas,
    button: &Button,
    index: usize,
    now: Option<&NowPlaying>,
) {
    let area = button.area;
    if area.width <= 0 || area.height <= 0 || !button.shown.is(now.is_some_and(|now| now.playing)) {
        return;
    }
    let corner = button
        .mapped
        .as_ref()
        .map_or((area.x, area.y), |(origin, _, _)| (origin.x, origin.y));
    let interactive = button.control != Control::Inert
        || button.hover.is_some()
        || button.down.is_some()
        || !button.tooltip.is_empty();
    let mut hovered = false;
    let mut pressed = false;
    let mut clicked = false;
    if interactive {
        let response = ui.interact(
            canvas.rect(area),
            Id::new(("wmp-button", index)),
            Sense::click(),
        );
        let over = response
            .hover_pos()
            .is_some_and(|point| on_mapped(canvas, button, point));
        hovered = response.hovered() && over;
        pressed = hovered && response.is_pointer_button_down_on();
        clicked = response.clicked()
            && response
                .interact_pointer_pos()
                .is_some_and(|point| on_mapped(canvas, button, point));
        if hovered && !button.tooltip.is_empty() {
            response.on_hover_text(&button.tooltip);
        }
    }
    let state = if pressed {
        button.down.as_ref().or(button.hover.as_ref())
    } else if hovered {
        button.hover.as_ref()
    } else {
        None
    };
    if button.mapped.is_some() {
        // The group's resting picture is drawn with the pictures; only the
        // lit button's part changes.
        if let Some(key) = state {
            canvas.piece(ui, &Some(key.clone()), corner, area);
        }
    } else {
        let key = state.or(button.up.as_ref()).cloned();
        canvas.picture(ui, &key, area.x, area.y);
    }
    if clicked {
        perform(app, ui.ctx(), button.control, now);
    }
}

fn perform(app: &mut App, ctx: &egui::Context, control: Control, now: Option<&NowPlaying>) {
    let playing = now.is_some_and(|now| now.playing);
    match control {
        Control::Play if !playing => app.actions.push(Action::TogglePlay),
        Control::Pause if playing => app.actions.push(Action::TogglePlay),
        Control::PlayPause => app.actions.push(Action::TogglePlay),
        Control::Stop if now.is_some() => {
            if playing {
                app.actions.push(Action::TogglePlay);
            }
            app.actions.push(Action::Seek(0));
        }
        Control::Next => app.actions.push(Action::Next),
        Control::Previous => app.actions.push(Action::Previous),
        Control::Mute => app.actions.push(Action::ToggleMute),
        Control::Shuffle => app.actions.push(Action::ToggleShuffle),
        Control::Repeat => app.actions.push(Action::CycleRepeat),
        Control::Close => app.actions.push(Action::ToggleWinampWindow),
        Control::Minimize => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
        _ => {}
    }
}

fn draw_slider(
    app: &mut App,
    ui: &mut Ui,
    canvas: &Canvas,
    slider: &Slider,
    index: usize,
    now: Option<&NowPlaying>,
) {
    let area = slider.area;
    if area.width <= 0 || area.height <= 0 {
        return;
    }
    let enabled = match slider.kind {
        SliderKind::Seek => now.is_some_and(|now| now.duration_ms > 0 && !stopped(Some(now))),
        SliderKind::Volume => true,
    };
    let rect = canvas.rect(area);
    let id = Id::new(("wmp-slider", index));
    let response = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let thumb_picture = if response.dragged() || response.is_pointer_button_down_on() {
        slider.thumb_down.as_ref().or(slider.thumb.as_ref())
    } else if response.hovered() {
        slider.thumb_hover.as_ref().or(slider.thumb.as_ref())
    } else {
        slider.thumb.as_ref()
    };
    let thumb = canvas
        .wmp
        .image(&thumb_picture.cloned())
        .map_or((0, 0), |bitmap| (bitmap.width as i32, bitmap.height as i32));
    let (length, thumb_length) = if slider.vertical {
        (area.height, thumb.1)
    } else {
        (area.width, thumb.0)
    };
    let border = slider.border.min(length / 2);
    let travel = (length - thumb_length - 2 * border).max(1) as f32;

    // The pointer as a value from 0 to 1.
    let along = |point: Pos2| {
        let offset = if slider.vertical {
            (point.y - rect.min.y) / canvas.unit - (border + thumb_length / 2) as f32
        } else {
            (point.x - rect.min.x) / canvas.unit - (border + thumb_length / 2) as f32
        };
        let travelled = (offset / travel).clamp(0.0, 1.0);
        // Horizontal grows to the right and vertical upward, unless the
        // skin says otherwise.
        let toward_end = if slider.vertical {
            1.0 - travelled
        } else {
            travelled
        };
        if slider.reversed {
            1.0 - toward_end
        } else {
            toward_end
        }
    };
    let remembered = Id::new(("wmp-slider-value", index));
    let pointer = response.interact_pointer_pos().map(along);
    let mut dragging = None;
    let mut committed = None;
    if (response.drag_started() || response.dragged())
        && let Some(value) = pointer
    {
        ui.data_mut(|data| data.insert_temp(remembered, value));
        dragging = Some(value);
    }
    if response.drag_stopped() {
        committed = ui.data(|data| data.get_temp::<f32>(remembered)).or(pointer);
        ui.data_mut(|data| data.remove::<f32>(remembered));
    } else if response.clicked() {
        committed = pointer;
    }
    match slider.kind {
        SliderKind::Seek => {
            if let Some(value) = dragging {
                app.seek_preview = Some(value);
            }
            if let (Some(value), Some(now)) = (committed, now) {
                app.seek_preview = None;
                app.actions
                    .push(Action::Seek((value * now.duration_ms as f32) as u32));
            }
        }
        SliderKind::Volume => {
            if let Some(value) = dragging {
                app.volume_preview = Some(value);
                if now.is_none_or(|now| now.local) {
                    app.actions
                        .push(Action::PreviewVolume((value * 100.0).round() as u8));
                }
            }
            if let Some(value) = committed {
                app.volume_preview = None;
                app.actions
                    .push(Action::SetVolume((value * 100.0).round() as u8));
            }
        }
    }

    let value = match slider.kind {
        SliderKind::Seek => app
            .seek_preview
            .or_else(|| {
                now.filter(|now| now.duration_ms > 0)
                    .map(|now| now.position_ms as f32 / now.duration_ms as f32)
            })
            .unwrap_or(0.0),
        SliderKind::Volume => app.volume_preview.unwrap_or_else(|| {
            f32::from(now.map_or_else(
                || crate::app::volume_to_percent(app.local.volume),
                |now| now.volume_percent,
            )) / 100.0
        }),
    }
    .clamp(0.0, 1.0);
    // Where the thumb sits, from the slider's start edge.
    let toward_end = if slider.reversed { 1.0 - value } else { value };
    let offset = border
        + (if slider.vertical {
            1.0 - toward_end
        } else {
            toward_end
        } * travel)
            .round() as i32;

    canvas.picture(ui, &slider.background, area.x, area.y);
    if let Some(bitmap) = canvas.wmp.image(&slider.foreground) {
        // The filled part: from the start of the track to the thumb's middle.
        let fill = offset + thumb_length / 2;
        let (width, height) = (bitmap.width as i32, bitmap.height as i32);
        // The thumb sits at `offset` from the top or left edge, so the
        // fill is the side the value grew from.
        let part = match (slider.vertical, slider.reversed) {
            (true, false) => Box2 {
                x: area.x,
                y: area.y + fill.clamp(0, height),
                width,
                height: height - fill.clamp(0, height),
            },
            (true, true) => Box2 {
                x: area.x,
                y: area.y,
                width,
                height: fill.clamp(0, height),
            },
            (false, false) => Box2 {
                x: area.x,
                y: area.y,
                width: fill.clamp(0, width),
                height,
            },
            (false, true) => Box2 {
                x: area.x + fill.clamp(0, width),
                y: area.y,
                width: width - fill.clamp(0, width),
                height,
            },
        };
        if part.width > 0 && part.height > 0 {
            canvas.piece(ui, &slider.foreground, (area.x, area.y), part);
        }
    }
    let (thumb_x, thumb_y) = if slider.vertical {
        (area.x, area.y + offset)
    } else {
        (area.x + offset, area.y)
    };
    canvas.picture(ui, &thumb_picture.cloned(), thumb_x, thumb_y);
}

fn clock(milliseconds: u32) -> String {
    let seconds = milliseconds / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn draw_text(app: &App, ui: &mut Ui, canvas: &Canvas, text: &Text, now: Option<&NowPlaying>) {
    let shown = match (text.field, now) {
        (Field::Literal, _) => text.literal.clone(),
        (_, None) => String::new(),
        (Field::Title, Some(now)) => now.title.clone(),
        (Field::Artist, Some(now)) => now
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        (Field::Album, Some(now)) => now.album_name.clone(),
        (Field::Elapsed, Some(now)) => clock(app.seek_preview.map_or(now.position_ms, |value| {
            (value * now.duration_ms as f32) as u32
        })),
        (Field::Duration, Some(now)) => clock(now.duration_ms),
    };
    if shown.is_empty() || text.area.width <= 0 {
        return;
    }
    let rect = canvas.rect(text.area);
    let [r, g, b] = text.color;
    let (anchor, x) = match text.align {
        Align::Left => (Align2::LEFT_CENTER, rect.min.x),
        Align::Center => (Align2::CENTER_CENTER, rect.center().x),
        Align::Right => (Align2::RIGHT_CENTER, rect.max.x),
    };
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.text(
        pos2(x, rect.center().y),
        anchor,
        shown,
        FontId::proportional(text.size * POINT * canvas.unit),
        Color32::from_rgb(r, g, b),
    );
}

/// The skin's visualisation window: the same bars or wave the Winamp
/// display shows, from the sound after the equalizer and before the
/// volume. A click changes it. Returns whether it is still moving.
fn draw_effects(
    app: &mut App,
    ui: &mut Ui,
    canvas: &Canvas,
    area: Box2,
    palette: &crate::skin::VisColors,
    now: Option<&NowPlaying>,
) -> bool {
    let rect = canvas.rect(area);
    if ui
        .interact(
            rect,
            Id::new(("wmp-effects", area.x, area.y)),
            Sense::click(),
        )
        .clicked()
    {
        app.actions.push(Action::CycleVisualiser);
    }
    let mode = app.settings.vis;
    let color = |index: usize| {
        let [r, g, b] = palette[index.min(palette.len() - 1)];
        Color32::from_rgb(r, g, b)
    };
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let sounding = now.is_some_and(|now| (now.playing || now.loading) && now.local);
    match mode {
        VisMode::Off => false,
        VisMode::Bars => {
            let samples = if sounding {
                app.winamp.tap.window(vis::FFT_SAMPLES, vis::LAG)
            } else {
                vec![0.0; vis::FFT_SAMPLES]
            };
            let bars = app
                .winamp
                .analyser
                .step(&samples, std::time::Instant::now());
            let slot = rect.width() / bars.len() as f32;
            let row = rect.height() / f32::from(vis::ROWS);
            for (index, bar) in bars.iter().enumerate() {
                let left = rect.min.x + slot * index as f32;
                for level in (vis::ROWS - bar.height)..vis::ROWS {
                    let top = rect.min.y + row * f32::from(level);
                    painter.rect_filled(
                        Rect::from_min_size(
                            pos2(left + slot * 0.1, top),
                            vec2(slot * 0.8, row * 0.85),
                        ),
                        0.0,
                        color(2 + usize::from(level)),
                    );
                }
                if let Some(peak) = bar.peak {
                    let top = rect.min.y + row * f32::from(vis::ROWS - peak);
                    painter.rect_filled(
                        Rect::from_min_size(
                            pos2(left + slot * 0.1, top),
                            vec2(slot * 0.8, row * 0.85),
                        ),
                        0.0,
                        color(23),
                    );
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
            let step = rect.width() / rows.len() as f32;
            let row = rect.height() / f32::from(vis::ROWS);
            let points: Vec<Pos2> = rows
                .iter()
                .enumerate()
                .map(|(x, &y)| {
                    pos2(
                        rect.min.x + step * (x as f32 + 0.5),
                        rect.min.y + row * (f32::from(y) + 0.5),
                    )
                })
                .collect();
            for pair in points.windows(2) {
                let shade = color(18 + vis::scope_shade(((pair[0].y - rect.min.y) / row) as u8));
                painter.line_segment(
                    [pair[0], pair[1]],
                    egui::Stroke::new(canvas.unit.max(1.5), shade),
                );
            }
            sounding
        }
    }
}
