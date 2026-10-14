//! A clicked cover flying from where it was into the header of the page it
//! opens: the card, tile or library row's cover moves and grows into the
//! page's big cover, so the eye follows it from one screen to the next.
//!
//! The click sends the cover's place along with the page to open. The page
//! header reports where its cover sits each frame and keeps it hidden while
//! the flight lasts; the flight is painted over everything, after the page.
//! Both paint the same artwork from the same scaled copies, choosing by the
//! same clock, so the header takes over on the frame the flight lands with
//! nothing changing on screen.

use egui::emath::GuiRounding as _;
use egui::{Context, Rect, Ui};

use crate::app::App;
use crate::model::{Action, CoverOrigin, Page};
use crate::theme::Icon;

use super::widgets;

/// How long a cover takes to fly into its page.
pub const FLIGHT_SECONDS: f64 = 0.28;

/// How long a cover waits for a page that has no header to land in yet,
/// one whose details are on their way, before it is let go.
pub const LANDING_WAIT: f64 = 0.25;

/// A cover on its way into `page`'s header, or landed there: until another
/// page opens, the header shows it while its own artwork loads.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverFlight {
    pub origin: CoverOrigin,
    pub page: Page,
    /// The input time the flight began at, moved on past any frame that
    /// stalled.
    pub start: f64,
    /// The input time the page opened at: the flight begins then, or once
    /// the page has a header to land in.
    pub opened: f64,
    /// Whether the last frame had no header for the cover to land in, so
    /// the flight begins again with the next.
    pub held: bool,
    /// How its frames have come, to tell one that stalled.
    pub pace: Pace,
}

impl CoverFlight {
    /// Holds the flight back at `now`, the cover waiting where it was
    /// clicked: a frame that drew no header for it to land in.
    pub fn hold(&mut self, now: f64) {
        self.start = now;
        self.held = true;
    }

    /// Paces the flight by the frame drawn at `now`, an animation's frames
    /// coming at least `frame` seconds apart within the power budget.
    /// Called before anything draws, so the header and the flight read the
    /// same start: a frame that stalled, as a page's first can while it
    /// lays out its text and icons, is taken out of the flight, and the
    /// cover carries on from where it was shown rather than jumping.
    pub fn pace(&mut self, now: f64, frame: f64) {
        if std::mem::take(&mut self.held) {
            // A flight held back begins with this frame, so the frame that
            // first shows it shows the cover where it was clicked. The
            // frames it waited through came when whatever else was moving
            // asked for them, a spinner's turns say, and are not counted.
            self.start = now;
            self.pace.at = Some(now);
            return;
        }
        // Only a cover in the air: a landed one stays put however long the
        // window then sits idle.
        if progress(self.start, now).is_some() {
            self.start = self.pace.step(self.start, now, frame);
        }
    }
}

/// The smallest stall a flight steps over: a frame this much longer than
/// its usual ones.
const STALL: f64 = 0.004;

/// How many of a flight's latest frames its usual frame is the longest of.
const PACE_FRAMES: usize = 3;

/// When a flight's clock was last read, and how long its latest frames
/// took.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pace {
    at: Option<f64>,
    /// The latest frames' lengths, the last one last.
    frames: [f64; PACE_FRAMES],
}

impl Pace {
    /// When a flight that started at `start` starts once the frame at `now`
    /// is paced, an animation's frames coming at least `frame` seconds
    /// apart. A frame is measured against the longest of the few before
    /// it, so a short one, such as a repaint just after a tick, does not
    /// make every ordinary frame after it look like a stall, and against
    /// no less than `frame`, so neither do the quick repaints that answer a
    /// click while saving energy.
    fn step(&mut self, start: f64, now: f64, frame: f64) -> f64 {
        let Some(at) = self.at else {
            // The flight's first frame. The time since the click stands in
            // for an ordinary frame, so the page's first, which lays out
            // its text and icons, can be told to have stalled. At a
            // display's own rate the click came partway through a frame,
            // which can take the next for a stall and hold the cover back
            // by less than a frame: telling the two apart needs the
            // display's rate, which is not known. A second pass of the
            // click's own frame has taken no time.
            if now > start {
                self.at = Some(now);
                self.frames[PACE_FRAMES - 1] = now - start;
            }
            return start;
        };
        // A second pass of the same frame.
        if now <= at {
            return start;
        }
        let interval = now - at;
        let usual = self.frames.into_iter().fold(frame, f64::max);
        self.frames.rotate_left(1);
        self.frames[PACE_FRAMES - 1] = interval;
        self.at = Some(now);
        stepped_over(start, interval, usual)
    }
}

/// When a flight that started at `start` starts once a frame `interval`
/// long, after frames at most `usual` long, is stepped over: a frame half
/// as long again as those, and longer by more than [`STALL`], stalled (at
/// a display's rate, it missed a vblank), and its excess is taken out of
/// the flight rather than shown as a jump.
fn stepped_over(start: f64, interval: f64, usual: f64) -> f64 {
    if interval > usual * 1.5 && interval - usual > STALL {
        start + (interval - usual)
    } else {
        start
    }
}

/// Where a page header drew its cover this frame, and the artwork it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Landing {
    pub rect: Rect,
    pub radius: f32,
    pub requested: Option<String>,
    pub previous: Option<String>,
    /// The page's visible area, below the top bar and above the player bar.
    pub page: Rect,
    /// Whether any of it is on screen, rather than scrolled away.
    pub visible: bool,
}

fn landing_id() -> egui::Id {
    egui::Id::new("spotifast-cover-landing")
}

/// Records where the page header drew its cover this frame.
pub fn land(ui: &Ui, landing: Landing) {
    let pass = ui.ctx().cumulative_pass_nr();
    ui.ctx()
        .data_mut(|data| data.insert_temp(landing_id(), (pass, landing)));
}

/// The header's cover if a header drew one this frame.
fn landing(ctx: &Context) -> Option<Landing> {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|data| data.get_temp::<(u64, Landing)>(landing_id()))
        .filter(|(drawn, _)| *drawn == pass)
        .map(|(_, landing)| landing)
}

/// How far along a flight is at `now`, eased, or `None` once it has landed.
/// A clock that went back, as a new window's does, has landed it too.
///
/// The cover leaves at once and slows into the header: an ease-in would
/// hold it in place for the first frames after the click, while the page
/// has already changed under it, and then rush it at three times its
/// average speed.
pub fn progress(start: f64, now: f64) -> Option<f32> {
    let linear = (now - start) / FLIGHT_SECONDS;
    (0.0..1.0)
        .contains(&linear)
        .then(|| egui::emath::easing::quadratic_out(linear as f32))
}

/// How much longer a flight whose page opened at `opened` waits at `now`
/// for a header to land in, or `None` once it has waited enough.
pub fn waiting(opened: f64, now: f64) -> Option<f64> {
    let left = opened + LANDING_WAIT - now;
    (left > 0.0).then_some(left)
}

/// The cover's place and corner radius `t` of the way from `from` to `to`.
/// Both are places a cover is painted at rest, on whole pixels at
/// `pixels_per_point`, so it leaves the one and lands on the other exactly;
/// in between it is painted where it is.
pub fn between(from: (Rect, f32), to: (Rect, f32), t: f32, pixels_per_point: f32) -> (Rect, f32) {
    let [start, end] = [from.0, to.0].map(|rect| rect.round_to_pixels(pixels_per_point));
    let rect = Rect::from_min_max(start.min.lerp(end.min, t), start.max.lerp(end.max, t));
    (rect, from.1 + (to.1 - from.1) * t)
}

/// Where the flying cover may be seen: the window's width, between the
/// page's top and bottom edges.
pub fn clip(window: Rect, page: Rect) -> Rect {
    Rect::from_x_y_ranges(window.x_range(), page.y_range())
}

/// Where the flight that has been seen in the air is kept: the time its
/// page opened, which tells one flight from the next.
fn in_the_air_id() -> egui::Id {
    egui::Id::new("spotifast-cover-in-the-air")
}

/// Paints the flying cover over the page, after the page has drawn. A
/// flight whose page has no header on screen to land in yet waits a moment
/// for one, the cover where it was clicked, and then ends. One already on
/// its way whose header has gone, scrolled away say, lands at once rather
/// than starting over.
pub fn show(app: &mut App, ctx: &Context) {
    let Some(flight) = app.cover_flight.clone() else {
        return;
    };
    let now = ctx.input(|input| input.time);
    let Some(t) = progress(flight.start, now) else {
        return;
    };
    if flight.page != *app.page() {
        app.actions.push(Action::EndCoverFlight(flight.page));
        return;
    }
    let origin = &flight.origin;
    let from_radius = if origin.round {
        origin.rect.width() / 2.0
    } else {
        6.0
    };
    let in_the_air = ctx.data(|data| data.get_temp::<f64>(in_the_air_id())) == Some(flight.opened);
    let Some(landing) = landing(ctx).filter(|landing| landing.visible) else {
        match waiting(flight.opened, now).filter(|_| !in_the_air) {
            Some(left) => {
                app.actions
                    .push(Action::HoldCoverFlight(flight.page.clone()));
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(left));
                // It stays where it was clicked rather than leave with the
                // page it came from and come back once the header is drawn.
                paint(
                    app,
                    ctx,
                    origin,
                    (
                        origin.rect.round_to_pixels(ctx.pixels_per_point()),
                        from_radius,
                    ),
                    ctx.content_rect(),
                    0.0,
                    None,
                );
            }
            None => app.actions.push(Action::EndCoverFlight(flight.page)),
        }
        return;
    };
    ctx.data_mut(|data| data.insert_temp(in_the_air_id(), flight.opened));
    let (rect, radius) = between(
        (origin.rect, from_radius),
        (landing.rect, landing.radius),
        t,
        ctx.pixels_per_point(),
    );
    // The header's cover scrolls under the top bar, and so does the cover
    // flying into it: it keeps between the page's top and bottom edges,
    // across the whole width so a cover leaving the library is not cut.
    let clip = clip(ctx.content_rect(), landing.page);
    paint(app, ctx, origin, (rect, radius), clip, t, Some(&landing));
    crate::theme::animation_frame(ctx);
}

/// Paints the cover from `origin` at `rect` with corner `radius`, `t` of
/// the way into the header of `landing`, seen within `clip`.
fn paint(
    app: &App,
    ctx: &Context,
    origin: &CoverOrigin,
    (rect, radius): (Rect, f32),
    clip: Rect,
    t: f32,
    landing: Option<&Landing>,
) {
    let palette = app.palette;
    let art = app.backend.art().clone();
    // Painted straight onto a layer over the page. An egui area would lay
    // its content out unseen on its first frame and then fade it in, so the
    // cover would vanish as it took off and rise in faint.
    let id = egui::Id::new("spotifast-cover-flight");
    let mut ui = Ui::new(
        ctx.clone(),
        id,
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::new(egui::Order::Foreground, id))
            .max_rect(rect),
    );
    ui.set_clip_rect(clip);
    {
        // A cover that cast a shadow where it was keeps it as it takes off;
        // one that did not grows it on the way.
        let mut shadow = ui.new_child(egui::UiBuilder::new());
        if !origin.shadow {
            shadow.multiply_opacity(t);
        }
        widgets::paint_moving_shadow(&shadow, &palette, rect, radius);
    }
    // What the header will show once it lands, scaled for the header: its
    // own cover, asked for now so it is likely ready by then, or the one
    // flying in until it is.
    widgets::paint_cover_with_thumbnail(
        &ui,
        &palette,
        widgets::CoverSources {
            requested: landing.and_then(|landing| landing.requested.as_deref()),
            previous: landing.and_then(|landing| landing.previous.as_deref()),
            flown: Some(origin.url.as_str()),
            scale_for: Some(landing.map_or(origin.rect, |landing| landing.rect)),
            moving: landing.is_some(),
            ..Default::default()
        },
        rect,
        radius,
        if origin.round {
            Icon::User
        } else {
            Icon::Music
        },
        Some(&art),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    #[test]
    fn a_flight_leaves_at_once_slows_down_and_lands_on_time() {
        assert_eq!(progress(10.0, 10.0), Some(0.0));
        let early = progress(10.0, 10.0 + FLIGHT_SECONDS * 0.1).unwrap();
        let middle = progress(10.0, 10.0 + FLIGHT_SECONDS * 0.5).unwrap();
        let late = progress(10.0, 10.0 + FLIGHT_SECONDS * 0.9).unwrap();
        assert!(early > 0.15, "it leaves at once: {early}");
        assert!((middle - 0.75).abs() < 1e-4, "{middle}");
        assert!(late > 0.98, "and settles into place: {late}");
        assert_eq!(progress(10.0, 10.0 + FLIGHT_SECONDS + 1e-6), None);
        assert_eq!(progress(10.0, 3.0), None, "a clock that went back");
    }

    /// A cover clicked at 0, before the first frame after the click.
    fn clicked() -> CoverFlight {
        CoverFlight {
            origin: CoverOrigin {
                url: "https://i.scdn.co/image/flight".into(),
                rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(148.0, 148.0)),
                round: false,
                shadow: true,
            },
            page: Page::Album("flight".into()),
            start: 0.0,
            opened: 0.0,
            held: false,
            pace: Pace::default(),
        }
    }

    /// A cover clicked at 0 and flown through frames `intervals` apart, each
    /// paced before it draws, an animation's frames coming at least `frame`
    /// seconds apart: when it lands, and the start it lands with.
    fn fly(frame: f64, intervals: impl IntoIterator<Item = f64>) -> (f64, f64) {
        let mut flight = clicked();
        let mut now = 0.0;
        for interval in intervals {
            now += interval;
            flight.pace(now, frame);
            if progress(flight.start, now).is_none() {
                let start = flight.start;
                flight.pace(now + 10.0, frame);
                assert_eq!(flight.start, start, "a landed cover stays put");
                return (now, start);
            }
        }
        panic!("the cover never landed");
    }

    /// The shortest frame an animation asks for while saving energy.
    fn saver() -> f64 {
        crate::theme::shortest_animation_frame(crate::power::Budget::Saver).as_secs_f64()
    }

    /// A frame that stalls, as a page's first can while it lays out its
    /// text, is taken out of the flight: the cover carries on from where
    /// it was rather than jumping. Ordinary frames, at any rate, are not.
    #[test]
    fn a_stalled_frame_is_stepped_over() {
        // 164 Hz, and frames that took 11 and 13 ms.
        assert_eq!(stepped_over(1.0, 0.0061, 0.0061), 1.0);
        let start = stepped_over(1.0, 0.0122, 0.0061);
        assert!((start - 1.0061).abs() < 1e-9, "{start}");
        let start = stepped_over(1.0, 0.0191, 0.0061);
        assert!((start - 1.013).abs() < 1e-9, "{start}");
        // 60 Hz frames, and one that missed a vblank.
        assert_eq!(stepped_over(1.0, 0.0167, 0.0167), 1.0);
        let start = stepped_over(1.0, 0.0333, 0.0167);
        assert!((start - 1.0166).abs() < 1e-9, "{start}");
        // An unthrottled window's frames vary by a millisecond or so.
        assert_eq!(stepped_over(1.0, 0.0023, 0.0009), 1.0);

        // Over a whole flight the stall is taken out once, and the cover
        // lands that much later, on the next frame after its time.
        let vblank = 0.0061;
        let frames = [vblank, vblank, vblank + 0.013]
            .into_iter()
            .chain(std::iter::repeat(vblank));
        let (landed, start) = fly(0.0, frames.take(200));
        assert!((start - 0.013).abs() < 1e-9, "{start}");
        assert!(
            landed <= FLIGHT_SECONDS + 0.013 + vblank,
            "landed at {landed}"
        );
    }

    /// A frame shorter than the usual ones, as a repaint just after a tick
    /// is, does not make every ordinary frame after it look like a stall:
    /// the cover still lands on the first frame after its time, saving
    /// energy at 30 frames a second and at a 60 Hz display's rate.
    #[test]
    fn a_short_frame_does_not_slow_the_flight() {
        for (least, frame, short) in [(saver(), 1.0 / 30.0, 0.005), (0.0, 1.0 / 60.0, 0.003)] {
            let frames = [frame, frame, short, frame - short]
                .into_iter()
                .chain(std::iter::repeat(frame));
            let (landed, start) = fly(least, frames.take(200));
            assert_eq!(start, 0.0, "nothing stalled");
            assert!(landed <= FLIGHT_SECONDS + frame, "landed at {landed}");
        }
    }

    /// While saving energy, egui answers a click with a quick repaint or
    /// two, at once or at the display's rate, before the flight's frames
    /// come at the budget's. The first of those is no stall.
    #[test]
    fn the_repaints_answering_a_click_do_not_slow_a_flight_saving_energy() {
        let frame = 1.0 / 30.0;
        for quick in [[0.0015, 0.0015], [0.008, 1.0 / 60.0], [0.008, frame]] {
            let frames = quick.into_iter().chain(std::iter::repeat(frame));
            let (landed, start) = fly(saver(), frames.take(100));
            assert_eq!(start, 0.0, "nothing stalled after {quick:?}");
            assert!(landed <= FLIGHT_SECONDS + frame, "landed at {landed}");
        }
    }

    /// The click's own frame drawn a second time, as egui can, has taken
    /// no time, and does not make the first ordinary frame look like a
    /// stall.
    #[test]
    fn a_second_pass_of_the_click_frame_does_not_slow_the_flight() {
        let frame = 1.0 / 60.0;
        let frames = std::iter::once(0.0).chain(std::iter::repeat(frame));
        let (landed, start) = fly(0.0, frames.take(100));
        assert_eq!(start, 0.0, "nothing stalled");
        assert!(landed <= FLIGHT_SECONDS + frame, "landed at {landed}");
    }

    /// A cover held back while its page has no header leaves from where it
    /// was on the frame a header first shows it. The frames it waited
    /// through, a spinner's turns, are not counted, so that header's first
    /// frame, which can stall while it lays out, is stepped over.
    #[test]
    fn a_held_flight_begins_where_a_header_first_shows_it() {
        let vblank = 0.0061;
        let mut flight = clicked();
        // The page's first frame after the click has no header yet, nor do
        // the spinner's turns after it.
        let mut now = vblank;
        flight.pace(now, 0.0);
        flight.hold(now);
        for _ in 0..3 {
            now += 1.0 / 30.0;
            flight.pace(now, 0.0);
            assert_eq!(flight.start, now, "it has not begun");
            flight.hold(now);
        }
        // The page's details come in 20 ms after a turn, and its header is
        // drawn, taking 30 ms over its first frame.
        now += 0.02;
        flight.pace(now, 0.0);
        let shown = |flight: &CoverFlight, now| progress(flight.start, now).expect("on its way");
        assert_eq!(shown(&flight, now), 0.0, "it leaves from where it was");
        now += 0.03;
        flight.pace(now, 0.0);
        let stalled = shown(&flight, now);
        now += vblank;
        flight.pace(now, 0.0);
        let next = shown(&flight, now) - stalled;
        assert!(
            stalled < next * 1.5,
            "it moved {stalled} over the stall and then {next}"
        );
    }

    #[test]
    fn a_flight_waits_a_moment_for_a_header_to_land_in() {
        assert_eq!(waiting(10.0, 10.0), Some(LANDING_WAIT));
        assert!(waiting(10.0, 10.0 + LANDING_WAIT / 2.0).is_some());
        assert_eq!(waiting(10.0, 10.0 + LANDING_WAIT), None);
    }

    /// A page scrolled down hides the top of its header under the top bar;
    /// the cover flying into it hides there too, but not when it leaves a
    /// library row beside the page.
    #[test]
    fn a_flying_cover_stays_below_the_top_bar() {
        let window = Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0));
        let page = Rect::from_min_max(pos2(250.0, 92.0), pos2(1280.0, 712.0));
        let clip = clip(window, page);
        assert_eq!(clip.top(), 92.0);
        assert_eq!(clip.bottom(), 712.0);
        assert_eq!(clip.left(), 0.0, "a cover leaving the sidebar is whole");
    }

    #[test]
    fn the_cover_moves_and_grows_between_its_two_places() {
        let card = (
            Rect::from_min_size(pos2(100.0, 400.0), vec2(148.0, 148.0)),
            74.0,
        );
        let header = (
            Rect::from_min_size(pos2(24.0, 80.0), vec2(212.0, 212.0)),
            106.0,
        );
        assert_eq!(between(card, header, 0.0, 1.0), card);
        assert_eq!(between(card, header, 1.0, 1.0), header);
        let (rect, radius) = between(card, header, 0.5, 1.0);
        assert_eq!(rect.min, pos2(62.0, 240.0));
        assert_eq!(rect.width(), 180.0);
        assert_eq!(radius, 90.0);
    }

    /// The card and the header paint their covers on whole pixels, so the
    /// flying cover leaves from and lands on those, and glides between.
    #[test]
    fn the_cover_leaves_and_lands_on_whole_pixels() {
        let card = (
            Rect::from_min_size(pos2(100.3, 400.45), vec2(148.0, 148.0)),
            6.0,
        );
        let header = (
            Rect::from_min_size(pos2(24.1, 80.7), vec2(212.0, 212.0)),
            6.0,
        );
        let pixels = 1.25;
        let place = |t| between(card, header, t, pixels).0;
        assert_eq!(place(0.0), card.0.round_to_pixels(pixels));
        assert_eq!(place(1.0), header.0.round_to_pixels(pixels));
        assert_ne!(
            place(0.3),
            place(0.3).round_to_pixels(pixels),
            "in between it is painted where it is"
        );
    }
}
