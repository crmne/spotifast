//! Middle-click scrolling belongs to the list where it starts.
//!
//! Views report their scrolling areas. The application resolves the press
//! after drawing and changes only the owning egui scroll state. Moving over
//! another pane never transfers the gesture. Speed, dead zone, cursor and the
//! hold-or-click choice follow Chromium's middle-click autoscroll.

use egui::{Context, Id, LayerId, Pos2, Rect, Response, ScrollArea, Ui, Vec2, ViewportId};

const FRAME_ID: &str = "spotifast-autoscroll-frame";
/// Points the pointer may move on each axis before that axis scrolls.
const DEAD_ZONE: f32 = 15.0;
/// Chromium's speed is `distance ^ 2.2 * 0.000008` per fling unit, and its
/// fixed-velocity fling advances 5000 units a second.
const EXPONENT: f32 = 2.2;
const MULTIPLIER: f32 = 0.000_008 * 5000.0;
/// The longest step one frame may take, so a stalled frame cannot jump.
const MAX_STEP: f64 = 0.1;

#[derive(Clone, Copy, Default)]
struct Input {
    viewport: ViewportId,
    focused: bool,
    pointer: Option<Pos2>,
    middle: bool,
    released: bool,
    cancel: bool,
    time: f64,
}

impl Input {
    fn read(ctx: &Context) -> Self {
        let viewport = ctx.viewport_id();
        ctx.input(|input| Self {
            viewport,
            focused: input.focused,
            pointer: input.pointer.hover_pos(),
            middle: input.pointer.button_pressed(egui::PointerButton::Middle),
            released: input.pointer.any_released(),
            cancel: input.pointer.any_pressed()
                || input.key_pressed(egui::Key::Escape)
                || input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::MouseWheel { .. })),
            time: input.time,
        })
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Ordinary,
    Lyrics,
    Playlist { row_points: f32 },
}

#[derive(Clone, Copy)]
struct Area {
    kind: Kind,
    id: Id,
    layer: LayerId,
    rect: Rect,
    max: Vec2,
    input: Input,
    state: egui::scroll_area::State,
}

#[derive(Clone, Default)]
struct Observations {
    enabled: bool,
    root: Input,
    areas: Vec<Area>,
    /// Rows that drag only with the primary button, pressed this frame.
    rows: Vec<Id>,
}

impl Observations {
    fn released(&self) -> bool {
        self.root.released || self.areas.iter().any(|area| area.input.released)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// The middle button is down and the pointer is still in the dead zone.
    Pressed,
    /// The pointer left the dead zone with the button down: releasing stops.
    Held,
    /// The button came up inside the dead zone: the next click stops.
    Toggled,
}

#[derive(Clone, Copy)]
struct Active {
    id: Id,
    viewport: ViewportId,
    anchor: Pos2,
    offset: Vec2,
    /// The previous frame's time. egui's `stable_dt` is only a frame-rate
    /// prediction when nothing asked for an immediate repaint, which is the
    /// usual case here, so the elapsed time is measured instead.
    time: f64,
    /// Kept while the pointer is outside the window, as in Chromium.
    velocity: Vec2,
    mode: Mode,
}

#[derive(Default)]
pub struct Outcome {
    pub scrolling: bool,
    pub stop_following_lyrics: bool,
    pub playlist_scroll: Option<usize>,
}

impl Outcome {
    fn owned() -> Self {
        Self {
            scrolling: true,
            ..Default::default()
        }
    }
}

#[derive(Default)]
pub struct Autoscroll {
    active: Option<Active>,
    cancelled: bool,
}

impl Autoscroll {
    pub fn active(&self) -> bool {
        self.active.is_some()
    }

    /// Focus events still arrive when a minimized/background window skips drawing.
    pub fn cancel_if_unfocused(&mut self, ctx: &Context) -> bool {
        let input = Input::read(ctx);
        if self
            .active
            .is_some_and(|active| active.viewport == input.viewport && !input.focused)
        {
            self.active = None;
            true
        } else {
            false
        }
    }

    pub fn begin(&mut self, ctx: &Context, enabled: bool) {
        let root = Input::read(ctx);
        self.cancelled = self.active.is_some_and(|active| {
            !enabled || root.cancel || (active.viewport == root.viewport && !root.focused)
        });
        if self.cancelled {
            self.active = None;
        }
        ctx.data_mut(|data| {
            data.insert_temp(
                Id::new(FRAME_ID),
                Observations {
                    enabled,
                    root,
                    ..Default::default()
                },
            )
        });
    }

    /// Returns whether the gesture owns this frame's scrolling, including its
    /// cancellation frame, so a previous trackpad glide cannot resume behind it.
    pub fn finish(&mut self, ctx: &Context, enabled: bool) -> Outcome {
        let Some(frame) = ctx.data_mut(|data| data.remove_temp::<Observations>(Id::new(FRAME_ID)))
        else {
            self.active = None;
            return Outcome::default();
        };
        let owned = self.active() || self.cancelled;
        if !enabled || self.cancelled {
            self.active = None;
            return Outcome {
                scrolling: owned,
                ..Default::default()
            };
        }
        if let Some(mut active) = self.active {
            let owner = frame
                .areas
                .iter()
                .find(|area| area.id == active.id && area.input.viewport == active.viewport);
            let Some(owner) = owner.filter(|area| {
                area.input.focused && area.rect.is_positive() && area.max != Vec2::ZERO
            }) else {
                self.active = None;
                return Outcome::owned();
            };
            if let Some(pointer) = owner.input.pointer {
                active.velocity = scroll_velocity(pointer - active.anchor);
            }
            if active.mode == Mode::Pressed && active.velocity != Vec2::ZERO {
                active.mode = Mode::Held;
            }
            if frame.released() {
                match active.mode {
                    Mode::Pressed => active.mode = Mode::Toggled,
                    Mode::Held => {
                        self.active = None;
                        return Outcome::owned();
                    }
                    Mode::Toggled => {}
                }
            }
            if frame.root.cancel || frame.areas.iter().any(|area| area.input.cancel) {
                self.active = None;
                return Outcome::owned();
            }
            let mut outcome = Outcome::owned();
            let dt = (owner.input.time - active.time).clamp(0.0, MAX_STEP) as f32;
            active.time = owner.input.time;
            let mut velocity = active.velocity;
            if owner.max.x <= 0.0 {
                velocity.x = 0.0;
            }
            if owner.max.y <= 0.0 {
                velocity.y = 0.0;
            }
            let offset = (active.offset + velocity * dt).clamp(Vec2::ZERO, owner.max);
            let mut state = owner.state;
            state.offset = offset;
            match owner.kind {
                Kind::Ordinary | Kind::Lyrics => state.store(ctx, owner.id),
                Kind::Playlist { row_points } => {
                    outcome.playlist_scroll = Some((offset.y / row_points).floor() as usize)
                }
            }
            if offset != active.offset {
                // egui subtracts one predicted frame from this delay. Ask
                // for two frames, as the skinned visualiser does, so the
                // app's uncapped renderer does not spin while scrolling.
                ctx.request_repaint_after(std::time::Duration::from_micros(33_334));
            }
            active.offset = offset;
            if owner.input.viewport == ctx.viewport_id() {
                ctx.set_cursor_icon(cursor(velocity, owner.max));
            }
            self.active = Some(active);
            return outcome;
        }
        let Some(position) = frame.root.pointer.filter(|_| frame.root.middle) else {
            return Outcome::default();
        };
        // Only the top layer under the press may own it, so a menu or dialog
        // keeps the list beneath it still.
        let layer = ctx
            .layer_id_at(position)
            .unwrap_or_else(LayerId::background);
        if claimed(ctx, &frame, layer, position) {
            return Outcome::default();
        }
        // Nested areas finish first. A shelf owns its gesture if it can
        // scroll; otherwise its enclosing page may own the same press.
        let Some(owner) = frame.areas.iter().find(|area| {
            area.input.viewport == frame.root.viewport
                && area.layer == layer
                && area.input.focused
                && area.max != Vec2::ZERO
                && area.rect.contains(position)
        }) else {
            return Outcome::default();
        };
        self.active = Some(Active {
            id: owner.id,
            viewport: frame.root.viewport,
            anchor: position,
            offset: owner.state.offset,
            time: owner.input.time,
            velocity: Vec2::ZERO,
            mode: if frame.released() {
                Mode::Toggled
            } else {
                Mode::Pressed
            },
        });
        ctx.set_cursor_icon(cursor(Vec2::ZERO, owner.max));
        Outcome {
            scrolling: true,
            stop_following_lyrics: matches!(owner.kind, Kind::Lyrics),
            playlist_scroll: None,
        }
    }
}

/// Whether a widget under the press keeps it: a text field, for middle-click
/// paste, or anything that drags with any button, such as a slider, a scroll
/// bar or a panel edge. List rows drag only with the primary button.
fn claimed(ctx: &Context, frame: &Observations, layer: LayerId, position: Pos2) -> bool {
    let hits = ctx.interaction_snapshot(|snapshot| {
        snapshot
            .contains_pointer
            .iter()
            .copied()
            .collect::<Vec<_>>()
    });
    hits.into_iter()
        .filter_map(|id| ctx.read_response(id))
        .any(|hit| {
            hit.enabled()
                && hit.layer_id == layer
                && hit.interact_rect.contains(position)
                && (egui::text_edit::TextEditState::load(ctx, hit.id).is_some()
                    || (hit.sense.senses_drag()
                        && !frame.rows.contains(&hit.id)
                        // A touch screen lets the area itself drag to scroll.
                        && !frame.areas.iter().any(|area| hit.id == area.id.with("area"))))
        })
}

/// Chromium's autoscroll velocity, in points per second, for the pointer's
/// offset from the anchor. Each axis has its own dead zone.
fn scroll_velocity(distance: Vec2) -> Vec2 {
    let axis = |distance: f32| {
        if distance.abs() <= DEAD_ZONE {
            0.0
        } else {
            distance.signum() * distance.abs().powf(EXPONENT) * MULTIPLIER
        }
    };
    Vec2::new(axis(distance.x), axis(distance.y))
}

/// Chromium's cursors on Linux: the four-way arrows until the list moves,
/// then the direction it is moving in, along the axes it can scroll.
fn cursor(velocity: Vec2, max: Vec2) -> egui::CursorIcon {
    use egui::CursorIcon::*;
    let sign = |velocity: f32, scrolls: bool| {
        if scrolls && velocity != 0.0 {
            velocity.signum() as i8
        } else {
            0
        }
    };
    match (sign(velocity.x, max.x > 0.0), sign(velocity.y, max.y > 0.0)) {
        (1, -1) => ResizeNorthEast,
        (-1, -1) => ResizeNorthWest,
        (0, -1) => ResizeNorth,
        (1, 1) => ResizeSouthEast,
        (-1, 1) => ResizeSouthWest,
        (0, 1) => ResizeSouth,
        (1, 0) => ResizeEast,
        (-1, 0) => ResizeWest,
        _ => AllScroll,
    }
}

/// Let a middle press on a list row start autoscroll. Rows sense dragging
/// for drag and drop with the primary button; without this, the press would
/// belong to them like a slider's.
pub fn row(ui: &Ui, response: &Response) {
    if ui.input(|input| input.pointer.button_pressed(egui::PointerButton::Middle)) {
        ui.ctx().data_mut(|data| {
            data.get_temp_mut_or_default::<Observations>(Id::new(FRAME_ID))
                .rows
                .push(response.id)
        });
    }
}

/// Keep existing ScrollArea IDs, sizing, animation and drawing. Only add
/// observations for the after-draw controller; no application state is changed.
pub fn show<R>(
    ui: &mut Ui,
    area: ScrollArea,
    axes: egui::Vec2b,
    contents: impl FnOnce(&mut Ui) -> R,
) -> egui::scroll_area::ScrollAreaOutput<R> {
    let enabled = ui.ctx().data_mut(|data| {
        data.get_temp_mut_or_default::<Observations>(Id::new(FRAME_ID))
            .enabled
    });
    let layer = ui.layer_id();
    let clip = ui.clip_rect();
    let output = area.show(ui, contents);
    if enabled {
        let max = (output.content_size - output.inner_rect.size()).max(Vec2::ZERO) * axes.to_vec2();
        let observation = Area {
            kind: Kind::Ordinary,
            id: output.id,
            layer,
            rect: output.inner_rect.intersect(clip),
            max,
            input: Input::read(ui.ctx()),
            state: output.state,
        };
        ui.ctx().data_mut(|data| {
            data.get_temp_mut_or_default::<Observations>(Id::new(FRAME_ID))
                .areas
                .push(observation);
        });
    }
    output
}

/// Reading elsewhere stops automatic lyric following only after a scrollable
/// lyrics area has actually accepted the gesture.
pub fn lyrics(ui: &Ui, id: Id) {
    ui.ctx().data_mut(|data| {
        if let Some(area) = data
            .get_temp_mut_or_default::<Observations>(Id::new(FRAME_ID))
            .areas
            .iter_mut()
            .find(|area| area.id == id)
        {
            area.kind = Kind::Lyrics;
        }
    });
}

/// The skinned playlist scrolls in whole rows instead of using ScrollArea.
/// It still participates in the same ownership and cancellation rules.
pub fn playlist(ui: &mut Ui, rect: Rect, offset: usize, maximum: usize, row_points: f32) {
    let enabled = ui.ctx().data_mut(|data| {
        data.get_temp_mut_or_default::<Observations>(Id::new(FRAME_ID))
            .enabled
    });
    if !enabled || row_points <= 0.0 {
        return;
    }
    let rect = rect.intersect(ui.clip_rect());
    let id = ui.id().with("autoscroll-winamp-playlist");
    let mut state = egui::scroll_area::State::default();
    state.offset = egui::vec2(0.0, offset as f32 * row_points);
    let area = Area {
        kind: Kind::Playlist { row_points },
        id,
        layer: ui.layer_id(),
        rect,
        max: egui::vec2(0.0, maximum as f32 * row_points),
        input: Input::read(ui.ctx()),
        state,
    };
    ui.ctx().data_mut(|data| {
        data.get_temp_mut_or_default::<Observations>(Id::new(FRAME_ID))
            .areas
            .push(area)
    });
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
