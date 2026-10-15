use super::*;

struct Fixture {
    ctx: Context,
    controller: Autoscroll,
    enabled: bool,
    tall: bool,
    second: bool,
    frame_number: u64,
    frame_rate: f64,
    continuous: bool,
    areas: Vec<(Id, Rect)>,
    rows: Vec<Rect>,
    buttons: Vec<Rect>,
    inputs: Vec<Rect>,
    draggable: Vec<Rect>,
    sliders: Vec<Rect>,
    popup: bool,
    text: String,
    value: f32,
    tag_lyrics: bool,
    following: bool,
}

impl Fixture {
    fn new() -> Self {
        Self::at_rate(60.0)
    }

    fn at_rate(frame_rate: f64) -> Self {
        let mut fixture = Self {
            ctx: Context::default(),
            controller: Autoscroll::default(),
            enabled: true,
            tall: true,
            second: true,
            frame_number: 0,
            frame_rate,
            continuous: false,
            areas: Vec::new(),
            rows: Vec::new(),
            buttons: Vec::new(),
            inputs: Vec::new(),
            draggable: Vec::new(),
            sliders: Vec::new(),
            popup: false,
            text: String::new(),
            value: 0.5,
            tag_lyrics: false,
            following: true,
        };
        fixture.frame(vec![], true);
        fixture.frame(vec![], true);
        fixture
    }

    fn frame(&mut self, events: Vec<egui::Event>, focused: bool) {
        let ctx = self.ctx.clone();
        self.frame_number += 1;
        self.areas.clear();
        self.rows.clear();
        self.buttons.clear();
        self.inputs.clear();
        self.draggable.clear();
        self.sliders.clear();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(560.0, 340.0))),
                time: Some(self.frame_number as f64 / self.frame_rate),
                events,
                focused,
                ..Default::default()
            },
            |ui| {
                if self.continuous {
                    ctx.request_repaint();
                }
                self.controller.begin(&ctx, self.enabled);
                ui.put(
                    Rect::from_min_size(egui::pos2(10.0, 275.0), egui::vec2(180.0, 35.0)),
                    egui::Button::new("Outside control"),
                );
                for index in 0..if self.second { 2 } else { 1 } {
                    let rect = Rect::from_min_size(
                        egui::pos2(10.0 + index as f32 * 280.0, 10.0),
                        egui::vec2(240.0, 220.0),
                    );
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(index)
                            .max_rect(rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    let output = show(
                        &mut child,
                        ScrollArea::vertical()
                            .id_salt(index)
                            .max_height(220.0)
                            .animated(false),
                        egui::Vec2b::new(false, true),
                        |ui| {
                            let response =
                                ui.add_sized([220.0, 36.0], egui::Button::new("Song row"));
                            row(ui, &response);
                            self.rows.push(response.rect);
                            let button = ui.add_sized(
                                [200.0, 30.0],
                                egui::Button::new("A control inside the list"),
                            );
                            self.buttons.push(button.rect);
                            let input = ui.add(
                                egui::TextEdit::singleline(&mut self.text).desired_width(200.0),
                            );
                            self.inputs.push(input.rect);
                            let draggable = ui.add_sized(
                                [220.0, 30.0],
                                egui::Label::new("Dragged with the primary button")
                                    .sense(egui::Sense::click_and_drag()),
                            );
                            row(ui, &draggable);
                            self.draggable.push(draggable.rect);
                            let slider = ui.add(egui::Slider::new(&mut self.value, 0.0..=1.0));
                            self.sliders.push(slider.rect);
                            if self.tall {
                                ui.allocate_space(egui::vec2(220.0, 700.0));
                            }
                        },
                    );
                    if self.tag_lyrics {
                        lyrics(&child, output.id);
                    }
                    self.areas.push((output.id, output.inner_rect));
                }
                if self.popup {
                    egui::Area::new(Id::new("popup"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(egui::pos2(20.0, 60.0))
                        .show(&ctx, |ui| {
                            ui.allocate_space(egui::vec2(150.0, 120.0));
                        });
                }
                let outcome = self.controller.finish(&ctx, self.enabled);
                if outcome.stop_following_lyrics {
                    self.following = false;
                }
            },
        );
        output.textures_delta.clear();
    }

    fn middle(&mut self, position: Pos2) {
        self.frame(
            vec![
                egui::Event::PointerMoved(position),
                button(position, egui::PointerButton::Middle, true),
            ],
            true,
        );
        self.frame(
            vec![button(position, egui::PointerButton::Middle, false)],
            true,
        );
    }

    fn offsets(&self) -> Vec<Vec2> {
        self.areas
            .iter()
            .map(|(id, _)| {
                egui::scroll_area::State::load(&self.ctx, *id)
                    .unwrap()
                    .offset
            })
            .collect()
    }
}

fn button(pos: Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn scrolling_stays_with_the_starting_list_when_the_pointer_crosses_another_pane() {
    let mut f = Fixture::new();
    f.middle(f.rows[0].center());
    assert!(f.controller.active(), "the list row must arm the gesture");
    let across = egui::pos2(400.0, 200.0);
    for _ in 0..4 {
        f.frame(vec![egui::Event::PointerMoved(across)], true);
    }
    let offsets = f.offsets();
    assert!(offsets[0].y > 0.0);
    assert_eq!(
        offsets[1],
        Vec2::ZERO,
        "the pointer must not transfer scroll ownership"
    );
}

#[test]
fn a_press_anywhere_in_a_list_arms_except_on_text_fields_sliders_and_popups() {
    for target in 0..3 {
        let mut f = Fixture::new();
        let position = match target {
            0 => f.rows[0].center(),
            1 => f.buttons[0].center(),
            _ => f.draggable[0].center(),
        };
        f.middle(position);
        assert!(
            f.controller.active(),
            "row, button or row drag {target} arms"
        );
    }
    for target in 0..4 {
        let mut f = Fixture::new();
        let position = match target {
            0 => f.inputs[0].center(),
            1 => f.sliders[0].center(),
            2 => egui::pos2(100.0, 290.0),
            _ => egui::pos2(500.0, 320.0),
        };
        f.middle(position);
        assert!(
            !f.controller.active(),
            "text field, slider or chrome {target} must not arm"
        );
        assert!(f.offsets().iter().all(|offset| *offset == Vec2::ZERO));
    }
    let mut f = Fixture::new();
    f.middle(egui::pos2(100.0, 180.0));
    assert!(
        f.controller.active(),
        "blank space inside an overflowing list is eligible"
    );
    let mut f = Fixture::new();
    f.popup = true;
    f.frame(vec![], true);
    f.frame(vec![], true);
    f.middle(egui::pos2(60.0, 100.0));
    assert!(
        !f.controller.active(),
        "a popup keeps the list beneath it still"
    );
    let mut f = Fixture::new();
    f.tall = false;
    f.frame(vec![], true);
    f.frame(vec![], true);
    f.middle(f.rows[0].center());
    assert!(
        !f.controller.active(),
        "a list that fits must not claim the gesture"
    );
}

#[test]
fn focus_loss_disabling_and_disappearing_owner_cancel_without_more_movement() {
    for reason in 0..3 {
        let mut f = Fixture::new();
        f.middle(f.rows[1].center());
        f.frame(
            vec![egui::Event::PointerMoved(egui::pos2(400.0, 180.0))],
            true,
        );
        assert!(f.controller.active());
        if reason == 1 {
            f.enabled = false;
        }
        if reason == 2 {
            f.second = false;
        }
        f.frame(vec![], reason != 0);
        assert!(!f.controller.active());
        let stopped = f.offsets();
        for _ in 0..3 {
            f.frame(vec![], true);
        }
        assert_eq!(f.offsets(), stopped);
    }
}

#[test]
fn escape_clicks_and_wheel_stop_the_gesture_without_immediately_rearming_it() {
    for reason in 0..4 {
        let mut f = Fixture::new();
        let anchor = f.rows[0].center();
        f.middle(anchor);
        assert!(f.controller.active());
        let event = match reason {
            0 => egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
            1 => button(anchor, egui::PointerButton::Middle, true),
            2 => button(anchor, egui::PointerButton::Primary, true),
            _ => egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -20.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            },
        };
        f.frame(vec![event], true);
        assert!(
            !f.controller.active(),
            "cancellation {reason} must not rearm in the same frame"
        );
    }
}

#[test]
fn a_middle_click_and_small_pointer_movements_do_not_nudge_the_list() {
    let mut f = Fixture::new();
    let anchor = f.rows[0].center();
    f.middle(anchor);
    for y in [0.0, 5.0, 11.0] {
        f.frame(
            vec![egui::Event::PointerMoved(anchor + egui::vec2(50.0, y))],
            true,
        );
    }
    assert_eq!(f.offsets(), vec![Vec2::ZERO, Vec2::ZERO]);
    f.frame(
        vec![egui::Event::PointerMoved(anchor + egui::vec2(0.0, 40.0))],
        true,
    );
    assert!(f.offsets()[0].y > 0.0);
}

#[test]
fn a_nested_horizontal_shelf_keeps_its_axis_and_never_scrolls_the_outer_page() {
    let ctx = Context::default();
    let mut controller = Autoscroll::default();
    let mut frame = 0;
    let mut render = |events| {
        frame += 1;
        let mut ids = [Id::NULL; 2];
        let mut first = Pos2::ZERO;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 340.0))),
                events,
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            },
            |ui| {
                controller.begin(&ctx, true);
                let outer = show(
                    ui,
                    ScrollArea::vertical().id_salt("outer").animated(false),
                    egui::Vec2b::new(false, true),
                    |ui| {
                        let shelf = show(
                            ui,
                            ScrollArea::horizontal().id_salt("shelf").animated(false),
                            egui::Vec2b::new(true, false),
                            |ui| {
                                ui.horizontal(|ui| {
                                    for index in 0..12 {
                                        let response = ui.add_sized(
                                            [160.0, 60.0],
                                            egui::Button::new(format!("Album {index}")),
                                        );
                                        row(ui, &response);
                                        if index == 0 {
                                            first = response.rect.center();
                                        }
                                    }
                                });
                            },
                        );
                        ids[0] = shelf.id;
                        ui.allocate_space(egui::vec2(200.0, 900.0));
                    },
                );
                ids[1] = outer.id;
                controller.finish(&ctx, true);
            },
        );
        output.textures_delta.clear();
        let offsets = ids.map(|id| egui::scroll_area::State::load(&ctx, id).unwrap().offset);
        (first, controller.active(), offsets)
    };
    render(vec![]);
    let (anchor, _, _) = render(vec![]);
    let (_, active, _) = render(vec![
        egui::Event::PointerMoved(anchor),
        button(anchor, egui::PointerButton::Middle, true),
    ]);
    assert!(active);
    render(vec![button(anchor, egui::PointerButton::Middle, false)]);
    let (_, active, offsets) = render(vec![egui::Event::PointerMoved(egui::pos2(450.0, 280.0))]);
    assert!(active);
    assert!(offsets[0].x > 0.0);
    assert_eq!(offsets[0].y, 0.0);
    assert_eq!(offsets[1], Vec2::ZERO);
}

#[test]
fn the_skinned_playlist_accumulates_fractional_motion_and_returns_whole_row_updates() {
    let ctx = Context::default();
    let mut controller = Autoscroll::default();
    let mut frame = 0;
    let mut scroll = 0;
    let mut render = |events, focused| {
        frame += 1;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(500.0, 340.0))),
                events,
                focused,
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            },
            |ui| {
                controller.begin(&ctx, true);
                playlist(
                    ui,
                    Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(240.0, 200.0)),
                    scroll,
                    80,
                    13.0,
                );
                let outcome = controller.finish(&ctx, true);
                if let Some(offset) = outcome.playlist_scroll {
                    scroll = offset;
                }
            },
        );
        output.textures_delta.clear();
        (controller.active(), scroll)
    };
    render(vec![], true);
    render(vec![], true);
    let anchor = egui::pos2(100.0, 80.0);
    assert!(
        render(
            vec![
                egui::Event::PointerMoved(anchor),
                button(anchor, egui::PointerButton::Middle, true)
            ],
            true
        )
        .0
    );
    render(
        vec![button(anchor, egui::PointerButton::Middle, false)],
        true,
    );
    let moved = egui::pos2(100.0, 100.0);
    for _ in 0..30 {
        render(vec![egui::Event::PointerMoved(moved)], true);
    }
    let (active, position) = render(vec![], true);
    assert!(active);
    assert!(position > 0 && position < 80);
    let (active, stopped) = render(vec![], false);
    assert!(!active);
    assert_eq!(stopped, position);
    assert_eq!(render(vec![], true).1, position);
}

#[test]
fn lyric_following_stops_only_when_a_scrollable_lyrics_area_accepts_the_gesture() {
    for tall in [false, true] {
        let mut f = Fixture::new();
        f.tag_lyrics = true;
        f.tall = tall;
        f.frame(vec![], true);
        f.frame(vec![], true);
        f.middle(f.rows[0].center());
        assert_eq!(f.controller.active(), tall);
        assert_eq!(f.following, !tall);
    }
}

#[test]
fn losing_focus_while_drawing_is_suspended_cannot_resume_autoscroll_on_return() {
    let mut f = Fixture::new();
    f.middle(f.rows[0].center());
    f.frame(
        vec![egui::Event::PointerMoved(egui::pos2(100.0, 180.0))],
        true,
    );
    assert!(f.controller.active());
    let stopped = f.offsets();
    f.frame_number += 1;
    let mut output = f.ctx.run_ui(
        egui::RawInput {
            focused: false,
            time: Some(f.frame_number as f64 / 60.0),
            ..Default::default()
        },
        |ui| {
            assert!(f.controller.cancel_if_unfocused(ui.ctx()));
        },
    );
    output.textures_delta.clear();
    f.frame(vec![], true);
    assert!(!f.controller.active());
    assert_eq!(f.offsets(), stopped);
}

#[test]
fn extra_redraws_do_not_make_the_same_one_second_gesture_scroll_further() {
    let distance = |rate: u32| {
        let mut f = Fixture::at_rate(f64::from(rate));
        f.continuous = true;
        f.frame(vec![], true);
        let anchor = f.rows[0].center();
        f.middle(anchor);
        for _ in 0..rate {
            f.frame(
                vec![egui::Event::PointerMoved(anchor + egui::vec2(0.0, 42.0))],
                true,
            );
        }
        f.offsets()[0].y
    };
    let ordinary = distance(60);
    let frequent = distance(2000);
    assert!(ordinary > 0.0);
    assert!(
        (ordinary - frequent).abs() < 1.0,
        "equal time and pointer distance must give equal scrolling: {ordinary} vs {frequent}"
    );
}

#[test]
fn releasing_a_held_middle_button_after_moving_stops_but_a_plain_click_keeps_scrolling() {
    let mut f = Fixture::new();
    let anchor = f.rows[0].center();
    f.frame(
        vec![
            egui::Event::PointerMoved(anchor),
            button(anchor, egui::PointerButton::Middle, true),
        ],
        true,
    );
    assert!(f.controller.active());
    let below = anchor + egui::vec2(0.0, 60.0);
    for _ in 0..10 {
        f.frame(vec![egui::Event::PointerMoved(below)], true);
    }
    let held = f.offsets()[0];
    assert!(held.y > 0.0, "holding the button scrolls");
    f.frame(
        vec![button(below, egui::PointerButton::Middle, false)],
        true,
    );
    assert!(!f.controller.active(), "releasing after moving ends it");
    for _ in 0..5 {
        f.frame(vec![egui::Event::PointerMoved(below)], true);
    }
    assert_eq!(f.offsets()[0], held);

    // Released inside the dead zone, the press was a click: the list keeps
    // following the pointer until the next click.
    let mut f = Fixture::new();
    let anchor = f.rows[0].center();
    f.frame(
        vec![
            egui::Event::PointerMoved(anchor),
            button(anchor, egui::PointerButton::Middle, true),
        ],
        true,
    );
    f.frame(
        vec![
            egui::Event::PointerMoved(anchor + egui::vec2(0.0, 5.0)),
            button(anchor, egui::PointerButton::Middle, false),
        ],
        true,
    );
    assert!(f.controller.active());
    for _ in 0..10 {
        f.frame(
            vec![egui::Event::PointerMoved(anchor + egui::vec2(0.0, 60.0))],
            true,
        );
    }
    assert!(f.controller.active());
    assert!(f.offsets()[0].y > 0.0);
}

#[test]
fn speed_follows_chromium_and_the_time_that_actually_passed() {
    // Without an immediate repaint request egui reports a predicted frame
    // time, not the real one. Thirty frames a second must still scroll as far
    // per second as sixty.
    let distance = |rate: u32, pointer: f32| {
        let mut f = Fixture::at_rate(f64::from(rate));
        let anchor = f.rows[0].center();
        f.middle(anchor);
        let start = f.offsets()[0].y;
        for _ in 0..rate {
            f.frame(
                vec![egui::Event::PointerMoved(anchor + egui::vec2(0.0, pointer))],
                true,
            );
        }
        f.offsets()[0].y - start
    };
    // Chromium scrolls 0.04 * distance ^ 2.2 points a second.
    for pointer in [40.0_f32, 60.0] {
        let expected = 0.04 * pointer.powf(2.2);
        for rate in [30, 60] {
            let scrolled = distance(rate, pointer);
            assert!(
                (scrolled - expected).abs() < expected * 0.05,
                "{pointer} points away at {rate} Hz: {scrolled} instead of {expected}"
            );
        }
    }
    assert_eq!(distance(60, DEAD_ZONE), 0.0);
}

#[test]
fn the_cursor_matches_chromium() {
    use egui::CursorIcon::*;
    let both = egui::vec2(100.0, 100.0);
    let vertical = egui::vec2(0.0, 100.0);
    assert_eq!(cursor(Vec2::ZERO, both), AllScroll);
    assert_eq!(cursor(Vec2::ZERO, vertical), AllScroll);
    for (velocity, icon) in [
        (egui::vec2(0.0, -40.0), ResizeNorth),
        (egui::vec2(0.0, 40.0), ResizeSouth),
        (egui::vec2(40.0, 0.0), ResizeEast),
        (egui::vec2(-40.0, 0.0), ResizeWest),
        (egui::vec2(30.0, -30.0), ResizeNorthEast),
        (egui::vec2(-30.0, 30.0), ResizeSouthWest),
        (egui::vec2(30.0, 30.0), ResizeSouthEast),
        (egui::vec2(-30.0, -30.0), ResizeNorthWest),
    ] {
        assert_eq!(cursor(velocity, both), icon, "{velocity:?}");
    }
    // A list that only scrolls vertically shows only vertical arrows.
    assert_eq!(cursor(egui::vec2(30.0, -30.0), vertical), ResizeNorth);
    assert_eq!(cursor(egui::vec2(30.0, 0.0), vertical), AllScroll);
}
