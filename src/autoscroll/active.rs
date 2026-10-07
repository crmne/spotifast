//! Middle-click scrolling belongs to the list where it starts.
//!
//! Views identify scrolling areas and eligible row/background responses. The
//! application resolves those observations after drawing and changes only the
//! owning egui scroll state. Moving over another pane never transfers the gesture.

use egui::{Context, Id, LayerId, Pos2, Rect, Response, ScrollArea, Ui, Vec2, ViewportId};

const FRAME_ID: &str = "spotizgeg-autoscroll-frame";
const DEAD_ZONE: f32 = 12.0;
const SPEED: f32 = 6.0;

#[derive(Clone, Copy, Default)]
struct Input {
    viewport: ViewportId,
    focused: bool,
    pointer: Option<Pos2>,
    middle: bool,
    cancel: bool,
    dt: f32,
}

impl Input {
    fn read(ctx: &Context) -> Self {
        let viewport = ctx.viewport_id();
        ctx.input(|input| Self {
            viewport,
            focused: input.focused,
            pointer: input.pointer.hover_pos(),
            middle: input.pointer.button_pressed(egui::PointerButton::Middle),
            cancel: input.pointer.any_pressed()
                || input.key_pressed(egui::Key::Escape)
                || input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::MouseWheel { .. })),
            dt: input.stable_dt.clamp(0.0, 0.05),
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

#[derive(Clone, Copy)]
struct Candidate {
    viewport: ViewportId,
    layer: LayerId,
    position: Pos2,
}

#[derive(Clone, Default)]
struct Observations {
    enabled: bool,
    root: Input,
    areas: Vec<Area>,
    candidates: Vec<Candidate>,
}

#[derive(Clone, Copy)]
struct Active {
    id: Id,
    viewport: ViewportId,
    anchor: Pos2,
    offset: Vec2,
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
                    areas: Vec::new(),
                    candidates: Vec::new(),
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
            if frame.root.cancel || frame.areas.iter().any(|area| area.input.cancel) {
                self.active = None;
                return Outcome::owned();
            }
            let mut outcome = Outcome::owned();
            if let Some(pointer) = owner.input.pointer {
                let mut distance = pointer - active.anchor;
                if owner.max.x <= 0.0 {
                    distance.x = 0.0;
                }
                if owner.max.y <= 0.0 {
                    distance.y = 0.0;
                }
                let length = distance.length();
                let delta = if length <= DEAD_ZONE {
                    Vec2::ZERO
                } else {
                    distance / length * (length - DEAD_ZONE) * SPEED * owner.input.dt
                };
                let offset = (active.offset + delta).clamp(Vec2::ZERO, owner.max);
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
                    ctx.set_cursor_icon(egui::CursorIcon::AllScroll);
                }
            }
            self.active = Some(active);
            return outcome;
        }
        for candidate in frame.candidates {
            // Nested areas finish first. A shelf owns its gesture if it can
            // scroll; otherwise its enclosing page may own the same row.
            if let Some(owner) = frame.areas.iter().find(|area| {
                area.input.viewport == candidate.viewport
                    && area.layer == candidate.layer
                    && area.input.focused
                    && area.max != Vec2::ZERO
                    && area.rect.contains(candidate.position)
            }) {
                self.active = Some(Active {
                    id: owner.id,
                    viewport: candidate.viewport,
                    anchor: candidate.position,
                    offset: owner.state.offset,
                });
                ctx.set_cursor_icon(egui::CursorIcon::AllScroll);
                return Outcome {
                    scrolling: true,
                    stop_following_lyrics: matches!(owner.kind, Kind::Lyrics),
                    playlist_scroll: None,
                };
            }
        }
        Outcome::default()
    }
}

/// Mark a list row as eligible. Buttons and text fields deliberately do not
/// call this, so their own hit target prevents the background from arming.
pub fn row(ui: &Ui, response: &Response) {
    if !response.hovered() {
        return;
    }
    let input = Input::read(ui.ctx());
    if input.middle
        && input.focused
        && response.hovered()
        && let Some(position) = input.pointer
    {
        // egui can report a containing background as hovered as well as its
        // child button. Only the most specific interactive hit may claim the
        // press. In particular, a text field must keep middle-click paste.
        let hits = ui.ctx().interaction_snapshot(|snapshot| {
            snapshot
                .contains_pointer
                .iter()
                .copied()
                .collect::<Vec<_>>()
        });
        if hits
            .into_iter()
            .filter_map(|id| ui.ctx().read_response(id))
            .any(|hit| {
                hit.id != response.id
                    && hit.enabled()
                    && hit.sense.interactive()
                    && hit.layer_id == response.layer_id
                    && hit.interact_rect.contains(position)
                    && hit.interact_rect.area() < response.interact_rect.area()
            })
        {
            return;
        }
        ui.ctx().data_mut(|data| {
            let frame = data.get_temp_mut_or_default::<Observations>(Id::new(FRAME_ID));
            if frame.enabled {
                frame.candidates.push(Candidate {
                    viewport: input.viewport,
                    layer: ui.layer_id(),
                    position,
                });
            }
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
    let output = area.show(ui, |ui| {
        if enabled {
            let background = ui.interact(
                ui.clip_rect().intersect(ui.max_rect()),
                ui.id().with("autoscroll-background"),
                egui::Sense::click(),
            );
            row(ui, &background);
        }
        contents(ui)
    });
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
    let background = ui.interact(rect, id.with("background"), egui::Sense::click());
    row(ui, &background);
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
