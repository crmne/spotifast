//! Vertical scroll areas with elastic ends.
//!
//! Wraps [`crate::autoscroll::show`] to keep Windows middle-click scrolling.

use egui::{UiBuilder, vec2};

use crate::overscroll;

/// Grace period between events in one gesture.
const HOLD_GRACE_SECONDS: f32 = 0.09;

/// Maximum time a gesture may hold an end.
const MAX_HOLD_SECONDS: f32 = 0.12;

/// Elastic state carried between frames.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Pull {
    pull: f32,
    release: Option<(f64, f32)>,
    held_since: Option<f64>,
    suppressed: bool,
    /// Whether the finger-driven phase is active.
    direct: bool,
    last_offset: f32,
    /// Whether this gesture moved the list before reaching an end.
    moved: bool,
}

impl Pull {
    /// Whether there is anything left to draw.
    fn idle(&self) -> bool {
        self.pull == 0.0 && self.release.is_none()
    }
}

/// Draws a scroll area with elastic vertical ends.
pub fn show<R>(
    ui: &mut egui::Ui,
    area: egui::ScrollArea,
    axes: egui::Vec2b,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::scroll_area::ScrollAreaOutput<R> {
    let id = ui.id().with("overscroll");
    let now = ui.input(|input| input.time);
    let state: Pull = ui.data(|data| data.get_temp(id)).unwrap_or_default();
    let offset = drawn_offset(&state, now);

    let output = crate::autoscroll::show(ui, area, axes, |ui| {
        if overscroll::settled(offset) {
            return contents(ui);
        }
        // Shift inside the existing clip rect.
        let shifted = ui.max_rect().translate(vec2(0.0, offset));
        let layout = *ui.layout();
        let mut child = ui.new_child(UiBuilder::new().max_rect(shifted).layout(layout));
        let inner = contents(&mut child);
        // Keep the pull out of scrollbars and row virtualisation.
        ui.expand_to_include_rect(child.min_rect().translate(vec2(0.0, -offset)));
        inner
    });

    if axes.y {
        let next = measure_pull(ui, &output, &state, offset, now);
        if next != state {
            store(ui, id, next);
        }
        if !next.idle() {
            ui.ctx().request_repaint();
        }
    }
    output
}

/// Returns the offset for this frame.
fn drawn_offset(state: &Pull, now: f64) -> f32 {
    match state.release {
        Some((began, from)) => overscroll::released(from, (now - began) as f32),
        None => overscroll::damp(state.pull.abs()) * sign(state.pull),
    }
}

/// Turns refused scrolling into the next frame's pull.
fn measure_pull<R>(
    ui: &mut egui::Ui,
    output: &egui::scroll_area::ScrollAreaOutput<R>,
    state: &Pull,
    offset: f32,
    now: f64,
) -> Pull {
    let room = (output.content_size.y - output.inner_rect.height()).max(0.0);
    let here = output.state.offset.y;
    // Allow for painting rounding.
    let at_first = here <= 0.5;
    let at_last = here >= room - 0.5;
    let over = ui.rect_contains_pointer(output.inner_rect);
    let (refused, live, active, started, direct, suppressed) = ui.input(|input| {
        let refused = if over {
            input.smooth_scroll_delta.y
        } else {
            0.0
        };
        let active = input.is_scrolling();
        let mut direct = state.direct && active;
        let mut suppressed = state.suppressed && active;
        let mut started = false;
        for event in &input.events {
            if let egui::Event::MouseWheel { phase, .. } = event {
                match phase {
                    egui::TouchPhase::Start if direct => {
                        // winit reports macOS momentum as a second Start.
                        direct = false;
                        suppressed = true;
                        started = true;
                    }
                    egui::TouchPhase::Start => {
                        direct = true;
                        suppressed = false;
                        started = true;
                    }
                    egui::TouchPhase::End | egui::TouchPhase::Cancel => {
                        direct = false;
                        suppressed = true;
                    }
                    egui::TouchPhase::Move => {}
                }
            }
        }
        // egui tracks gestures across frames without events.
        let live = active && input.time_since_last_scroll() < HOLD_GRACE_SECONDS;
        (refused, live, active, started, direct, suppressed)
    });
    let held_since = if started { None } else { state.held_since };
    let timed_out = held_since.is_some_and(|began| now - began >= f64::from(MAX_HOLD_SECONDS));
    let suppressed = suppressed || timed_out;
    // Remember movement after the list reaches an end.
    let scrolled = (here - state.last_offset).abs() > 0.5;
    let moved = if started {
        scrolled
    } else {
        scrolled || (state.moved && active)
    };
    let carry = Pull {
        last_offset: here,
        moved,
        held_since,
        suppressed,
        direct,
        ..Pull::default()
    };

    let pushing = (refused > 0.0 && at_first) || (refused < 0.0 && at_last);
    if pushing && moved && !suppressed {
        ui.input_mut(|input| input.smooth_scroll_delta.y = 0.0);
        return Pull {
            // Resume from the offset already on screen.
            pull: overscroll::undamp(offset.abs()) * sign(offset) + refused,
            held_since: Some(held_since.unwrap_or(now)),
            ..carry
        };
    }
    if overscroll::settled(offset) {
        return carry;
    }
    if let Some(release) = state.release {
        return Pull {
            release: Some(release),
            held_since: None,
            ..carry
        };
    }
    // Release after leaving the end that was pulled.
    let still_at_end = (offset > 0.0 && at_first) || (offset < 0.0 && at_last);
    if live && still_at_end && !suppressed {
        return Pull {
            pull: state.pull,
            ..carry
        };
    }
    Pull {
        release: Some((now, offset)),
        held_since: None,
        ..carry
    }
}

/// Stores drawing and gesture bookkeeping.
fn store(ui: &mut egui::Ui, id: egui::Id, next: Pull) {
    ui.data_mut(|data| data.insert_temp(id, next));
}

/// A sign function that keeps zero neutral.
fn sign(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value.signum() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_area_draws_no_offset() {
        assert_eq!(drawn_offset(&Pull::default(), 12.0), 0.0);
    }

    #[test]
    fn a_pull_draws_to_the_side_it_was_made_on() {
        let down = Pull {
            pull: 200.0,
            ..Pull::default()
        };
        let up = Pull {
            pull: -200.0,
            ..Pull::default()
        };
        assert!(drawn_offset(&down, 0.0) > 0.0);
        assert_eq!(drawn_offset(&up, 0.0), -drawn_offset(&down, 0.0));
    }

    #[test]
    fn a_release_draws_its_way_back_to_nothing() {
        let from = overscroll::damp(200.0);
        let state = Pull {
            release: Some((10.0, from)),
            ..Pull::default()
        };
        let quarter = drawn_offset(&state, 10.0 + f64::from(overscroll::RELEASE_SECONDS) * 0.25);
        assert!(quarter < from && quarter > 0.0, "got {quarter}");
        let over = drawn_offset(&state, 10.0 + f64::from(overscroll::RELEASE_SECONDS) + 0.1);
        assert_eq!(over, 0.0);
    }

    struct Frame {
        delta: Option<f32>,
        phase: egui::TouchPhase,
        after: f64,
    }

    fn tick(delta: f32) -> Frame {
        Frame {
            delta: Some(delta),
            phase: egui::TouchPhase::Move,
            after: 0.016,
        }
    }

    fn start() -> Frame {
        Frame {
            delta: Some(0.0),
            phase: egui::TouchPhase::Start,
            after: 0.016,
        }
    }

    fn gap(after: f64) -> Frame {
        Frame {
            delta: None,
            phase: egui::TouchPhase::Move,
            after,
        }
    }

    fn lift() -> Frame {
        Frame {
            delta: Some(0.0),
            phase: egui::TouchPhase::End,
            after: 0.016,
        }
    }

    fn run(frames: &[Frame]) -> Vec<(f32, Pull, f32)> {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(200.0, 100.0));
        let mut seen = Vec::new();
        let mut time = 0.0;
        for frame in frames {
            time += frame.after;
            // Keep the pointer over the area.
            let mut events = vec![egui::Event::PointerMoved(egui::pos2(100.0, 50.0))];
            if let Some(delta) = frame.delta {
                events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(0.0, delta),
                    phase: frame.phase,
                    modifiers: egui::Modifiers::default(),
                });
            }
            let input = egui::RawInput {
                time: Some(time),
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                let id = ui.id().with("overscroll");
                let before: Pull = ui.data(|data| data.get_temp(id)).unwrap_or_default();
                let drawn = drawn_offset(&before, time);
                let area = egui::ScrollArea::vertical()
                    .id_salt("exercised")
                    .auto_shrink([false, false]);
                let out = show(ui, area, egui::Vec2b::new(false, true), |ui| {
                    ui.allocate_space(vec2(150.0, 200.0));
                });
                let held = ui
                    .data(|data| data.get_temp::<Pull>(id))
                    .unwrap_or_default();
                seen.push((out.content_size.y, held, drawn));
            });
            output.textures_delta.clear();
        }
        seen
    }

    fn ticks(delta: f32, count: usize) -> Vec<Frame> {
        (0..count).map(|_| tick(delta)).collect()
    }

    fn up_off_the_first_row_by(step: f32, past: usize) -> (Vec<Frame>, usize) {
        let mut frames = ticks(-step, 3);
        frames.extend(ticks(step, 3 + past));
        let length = frames.len();
        (frames, length)
    }

    fn up_off_the_first_row(past: usize) -> Vec<Frame> {
        up_off_the_first_row_by(40.0, past).0
    }

    #[test]
    fn scrolling_within_the_content_holds_no_pull() {
        for (_, held, _) in run(&ticks(-30.0, 4)) {
            assert!(held.idle(), "held {held:?} while scrolling inside");
        }
    }

    #[test]
    fn a_pull_never_changes_what_the_area_measures() {
        let seen = run(&up_off_the_first_row(6));
        let sizes: Vec<f32> = seen.iter().map(|(size, ..)| *size).collect();
        assert!(
            sizes.windows(2).all(|pair| (pair[0] - pair[1]).abs() < 0.5),
            "content size moved with the pull: {sizes:?}",
        );
    }

    #[test]
    fn resting_on_the_first_row_gives_nothing() {
        let seen = run(&ticks(40.0, 6));
        for (index, (_, held, drawn)) in seen.iter().enumerate() {
            assert_eq!(held.pull, 0.0, "frame {index} pulled from a standstill");
            assert_eq!(*drawn, 0.0, "frame {index} drew an offset");
        }
    }

    #[test]
    fn a_gesture_that_runs_out_of_list_still_gives() {
        let seen = run(&up_off_the_first_row(4));
        let (_, held, _) = seen.last().expect("frames ran");
        assert!(held.moved, "the gesture was not credited with scrolling");
        assert!(held.pull > 0.0, "a gesture with a tail gave nothing");
    }

    #[test]
    fn scrolling_past_the_first_row_accumulates_a_pull() {
        let seen = run(&up_off_the_first_row(4));
        let (_, last, _) = seen.last().expect("frames ran");
        assert!(last.pull > 0.0, "no pull was taken: {last:?}");
        assert!(last.release.is_none(), "released while still pushing");
        let pulls: Vec<f32> = seen.iter().map(|(_, held, _)| held.pull).collect();
        assert!(
            pulls.windows(2).all(|pair| pair[1] >= pair[0]),
            "pull did not build up: {pulls:?}",
        );
    }

    #[test]
    fn a_continuing_gesture_cannot_hold_an_end_past_the_limit() {
        let (mut frames, _) = up_off_the_first_row_by(6.0, 4);
        frames.extend(ticks(6.0, 24));
        let seen = run(&frames);
        let first_pull = seen
            .iter()
            .position(|(_, held, _)| held.pull != 0.0)
            .expect("gesture pulled");
        let first_release = seen
            .iter()
            .position(|(_, held, _)| held.release.is_some())
            .expect("gesture released");
        let held_for = (first_release - first_pull) as f32 * 0.016;
        assert!(held_for <= MAX_HOLD_SECONDS + 0.016, "held for {held_for}s",);
        let (_, _, drawn) = seen.last().expect("frames ran");
        assert_eq!(*drawn, 0.0, "continued input kept the pull visible");
    }

    #[test]
    fn a_gap_inside_a_gesture_does_not_release() {
        let (mut frames, pulling) = up_off_the_first_row_by(40.0, 3);
        frames.extend([gap(0.016), gap(0.032), gap(0.016)]);
        let seen = run(&frames);
        let (_, pushed, _) = seen[pulling - 1];
        for (index, (_, held, drawn)) in seen.iter().enumerate().skip(pulling) {
            assert!(
                held.release.is_none(),
                "frame {index} released inside the gesture: {held:?}",
            );
            // egui may continue delivering a large delta across empty frames.
            assert!(
                held.pull >= pushed.pull,
                "frame {index} lost pull across a gap: {} then {}",
                pushed.pull,
                held.pull,
            );
            assert!(*drawn > 0.0, "frame {index} dropped the offset in a gap");
        }
    }

    #[test]
    fn a_pause_shorter_than_the_grace_still_holds() {
        // Six-point deltas are not split across frames by egui.
        let (mut frames, _) = up_off_the_first_row_by(6.0, 2);
        frames.push(gap(0.05));
        let seen = run(&frames);
        let (_, held, drawn) = seen.last().expect("frames ran");
        assert!(held.release.is_none(), "let go inside a gap: {held:?}");
        assert!(*drawn > 0.0, "dropped the offset in a gap");
    }

    #[test]
    fn a_pause_past_the_grace_lets_go_without_waiting_for_egui() {
        let (mut frames, _) = up_off_the_first_row_by(6.0, 6);
        frames.push(gap(0.12));
        let seen = run(&frames);
        let (_, held, _) = seen.last().expect("frames ran");
        assert!(held.release.is_some(), "stayed stretched: {held:?}");
    }

    #[test]
    fn letting_go_releases_the_pull() {
        let mut frames = up_off_the_first_row(3);
        frames.push(lift());
        let seen = run(&frames);
        let (_, held, _) = seen.last().expect("frames ran");
        assert!(held.release.is_some(), "never released: {held:?}");
        assert_eq!(held.pull, 0.0);
    }

    #[test]
    fn macos_momentum_releases_the_pull_as_it_begins() {
        // winit reports macOS momentum as a second Start.
        let (gesture, _) = up_off_the_first_row_by(6.0, 4);
        let mut frames = vec![start()];
        frames.extend(gesture);
        let momentum_start = frames.len();
        frames.push(start());
        let release_ticks = (overscroll::RELEASE_SECONDS / 0.016).ceil() as usize;
        frames.extend(ticks(6.0, release_ticks));

        let seen = run(&frames);
        let (_, released, _) = seen[momentum_start];
        assert!(
            released.release.is_some(),
            "stayed stretched when momentum began: {released:?}",
        );
        assert!(released.suppressed, "did not remember the momentum handoff",);
        let (_, _, drawn) = seen.last().expect("frames ran");
        assert_eq!(*drawn, 0.0, "momentum prolonged the visible pull");
    }

    #[test]
    fn a_fresh_gesture_after_a_lift_pulls_again() {
        let (mut frames, _) = up_off_the_first_row_by(6.0, 3);
        frames.push(lift());
        frames.push(Frame {
            delta: Some(-6.0),
            phase: egui::TouchPhase::Start,
            after: 0.016,
        });
        frames.extend(ticks(-6.0, 2));
        frames.extend(ticks(6.0, 6));
        let seen = run(&frames);
        let (_, held, _) = seen.last().expect("frames ran");
        assert!(
            !held.suppressed,
            "still suppressing a new gesture: {held:?}"
        );
        assert!(held.pull > 0.0, "a new gesture took no pull: {held:?}");
    }

    #[test]
    fn a_release_runs_to_nothing_and_forgets_itself() {
        let mut frames = up_off_the_first_row(3);
        frames.push(lift());
        frames.extend((0..30).map(|_| gap(0.016)));
        let seen = run(&frames);
        let (_, held, drawn) = seen.last().expect("frames ran");
        assert!(held.idle(), "still holding after the release: {held:?}");
        assert_eq!(*drawn, 0.0, "still drawing an offset after the release");
    }

    #[test]
    fn scrolling_back_into_the_content_gives_the_pull_up() {
        let mut frames = ticks(40.0, 3);
        frames.extend(ticks(-60.0, 3));
        let seen = run(&frames);
        let (_, held, _) = seen.last().expect("frames ran");
        assert!(
            held.release.is_some() || held.idle(),
            "kept a pull after leaving the end: {held:?}",
        );
    }

    #[test]
    fn a_state_is_idle_only_with_nothing_left_to_draw() {
        assert!(Pull::default().idle());
        assert!(
            !Pull {
                pull: 3.0,
                ..Pull::default()
            }
            .idle()
        );
        assert!(
            !Pull {
                release: Some((1.0, 20.0)),
                ..Pull::default()
            }
            .idle()
        );
    }
}
