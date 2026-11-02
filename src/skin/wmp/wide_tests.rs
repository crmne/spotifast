//! The wide skin, run from its real archive. It shares its script with the
//! tall one, so this checks what differs: its layout, and that it keeps its
//! own memory.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::deck::{Command, Deck, PlayState};
use super::live::WindowCommand;
use super::paint::Paint;
use super::ribbon_tests::{FRAME, click, deck, paint_all, run, song, text};
use super::runtime::{Input, Modifiers, Runtime};
use crate::skin::Skin;

const OPENED: Duration = Duration::from_millis(2600);
const DOTS: (f32, f32) = (338.0, 57.0);
const SQUARE: (f32, f32) = (72.0, 96.0);

fn start_with(deck: &Deck, prefs: HashMap<String, String>) -> (Runtime, u32, Instant) {
    let skin = Skin::wide().expect("the skin reads");
    let wmp = skin.wmp.clone().expect("a Windows Media Player skin");
    assert_eq!(wmp.title, "Spotifast Wide");
    assert_eq!((wmp.width, wmp.height), (460, 180));
    let runtime = Runtime::new(
        crate::skin::WIDE.into(),
        wmp,
        deck.clone(),
        prefs,
        Arc::new(|text: &str, _, _, _| text.chars().count() as f32 * 6.0),
        || {},
    );
    let at = runtime.primary().expect("a window");
    let key = runtime.views[at].state.key;
    (runtime, key, Instant::now())
}

fn opened(deck: &Deck) -> (Runtime, u32, Instant) {
    let (mut runtime, key, mut now) = start_with(deck, HashMap::new());
    run(&mut runtime, deck, &mut now, OPENED);
    paint_all(&mut runtime, key, now);
    assert_eq!(runtime.logged_errors(), Vec::<String>::new());
    (runtime, key, now)
}

fn asks(state: PlayState, place: (f32, f32)) -> Vec<Command> {
    let deck = deck(state);
    let (mut runtime, key, mut now) = opened(&deck);
    click(&mut runtime, key, place, now);
    run(&mut runtime, &deck, &mut now, FRAME)
}

fn turn_page(runtime: &mut Runtime, key: u32, deck: &Deck, now: &mut Instant) {
    click(runtime, key, DOTS, *now);
    run(runtime, deck, now, Duration::from_millis(400));
    paint_all(runtime, key, *now);
}

#[test]
fn the_source_and_the_archive_agree() {
    let skin = Skin::wide().expect("the skin reads");
    let wmp = skin.wmp.as_ref().expect("a Windows Media Player skin");
    let hint = "run `cargo run --example ribbon_skin` after changing the skin's source";
    assert_eq!(
        wmp.files["ribbon.js"],
        include_bytes!("../../../assets/skins/ribbon/ribbon.js"),
        "{hint}"
    );
    assert_eq!(
        wmp.files["shape.js"],
        include_bytes!("../../../assets/skins/wide/shape.js"),
        "{hint}"
    );
    let source = include_str!("../../../assets/skins/wide/wide.wms");
    let (before, after) = source.split_once("<!-- @bands -->\n").expect("the marker");
    let definition = String::from_utf8(wmp.files["wide.wms"].clone()).expect("text");
    assert!(
        definition.starts_with(before) && definition.ends_with(after),
        "{hint}"
    );
}

#[test]
fn it_holds_every_picture_it_names() {
    let (runtime, key, _) = start_with(&deck(PlayState::Playing), HashMap::new());
    let view = runtime.view(key).expect("the view");
    for element in &view.state.elements {
        for name in [
            "image",
            "hoverimage",
            "downimage",
            "disabledimage",
            "backgroundimage",
            "thumbimage",
        ] {
            if let Some(file) = element.image_name(name)
                && !file.starts_with("wmpimage")
            {
                assert!(
                    runtime.wmp.files.contains_key(&file),
                    "{file} is not in the skin"
                );
            }
        }
    }
}

#[test]
fn light_comes_first_then_the_whole_object_then_its_screen() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = start_with(&deck, HashMap::new());
    run(&mut runtime, &deck, &mut now, Duration::from_millis(320));
    let traced = runtime.read(key, "trace", "value");
    assert!(traced > 0.0 && traced < 13.0, "part way round: {traced}");
    assert_eq!(runtime.read(key, "wipe", "value"), 0.0, "light alone");
    run(&mut runtime, &deck, &mut now, Duration::from_millis(620));
    assert_eq!(
        runtime.read(key, "wipe", "visible"),
        0.0,
        "the object is on"
    );
    assert_eq!(runtime.read(key, "art", "visible"), 0.0, "not yet");
    run(&mut runtime, &deck, &mut now, OPENED);
    assert_eq!(runtime.read(key, "art", "visible"), 1.0);
    assert_eq!(runtime.read(key, "veil", "top"), -104.0);
    assert_eq!(text(&runtime, key, "title"), "Song");
    assert_eq!(text(&runtime, key, "artist"), "Band");
    assert_eq!(text(&runtime, key, "time"), "00:50");
}

#[test]
fn the_glass_closes_with_nothing_playing() {
    let (runtime, key, _) = opened(&Deck::default());
    assert_eq!(runtime.read(key, "veil", "top"), 0.0);
    assert_eq!(text(&runtime, key, "title"), "Nothing playing");
}

#[test]
fn the_visualizer_fills_the_glass_under_the_words() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    click(&mut runtime, key, SQUARE, now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(800));
    assert_eq!(runtime.read(key, "visual", "visible"), 1.0);
    assert_eq!(runtime.read(key, "wave", "visible"), 0.0);
    paint_all(&mut runtime, key, now);
    let (paints, _) = runtime.paint(key, now);
    let pane = paints
        .iter()
        .position(|paint| matches!(paint, Paint::Effects { .. }))
        .expect("the pane");
    let words = paints
        .iter()
        .position(|paint| matches!(paint, Paint::Text { text, .. } if text.text == "Song"))
        .expect("the title");
    assert!(words > pane, "the title draws over the picture");
    // The small button goes back.
    click(&mut runtime, key, (334.0, 106.0), now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(800));
    assert_eq!(runtime.read(key, "visual", "visible"), 0.0);
    assert_eq!(runtime.read(key, "art", "visible"), 1.0);
}

#[test]
fn it_keeps_its_own_memory() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    click(&mut runtime, key, SQUARE, now);
    assert_eq!(runtime.shared.prefs["wide-pane"], "visual");
    assert!(!runtime.shared.prefs.contains_key("pane"));
    run(&mut runtime, &deck, &mut now, Duration::from_millis(800));
    paint_all(&mut runtime, key, now);
    turn_page(&mut runtime, key, &deck, &mut now);
    turn_page(&mut runtime, key, &deck, &mut now);
    assert_eq!(runtime.shared.prefs["wide-page"], "queue");
    let (mut again, reopened, mut later) = start_with(&deck, runtime.shared.prefs.clone());
    run(&mut again, &deck, &mut later, OPENED);
    assert_eq!(again.read(reopened, "queuePage", "visible"), 1.0);
    // What the tall skin saved means nothing here.
    let tall = HashMap::from([("page".to_owned(), "eq".to_owned())]);
    let (mut other, reopened, mut later) = start_with(&deck, tall);
    run(&mut other, &deck, &mut later, OPENED);
    assert_eq!(other.read(reopened, "nowPage", "visible"), 1.0);
}

#[test]
fn the_equalizer_page_reaches_the_player() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    turn_page(&mut runtime, key, &deck, &mut now);
    // Near the top of the eighth band, then the preamp, then the switch.
    click(&mut runtime, key, (270.0, 50.0), now);
    let commands = run(&mut runtime, &deck, &mut now, FRAME);
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, Command::SetEqBand(7, gain) if *gain > 0.0)),
        "{commands:?}"
    );
    click(&mut runtime, key, (126.0, 50.0), now);
    let commands = run(&mut runtime, &deck, &mut now, FRAME);
    assert!(
        matches!(commands[..], [Command::SetEqPreamp(gain)] if gain > 0.0),
        "{commands:?}"
    );
    click(&mut runtime, key, (32.0, 69.0), now);
    assert_eq!(
        run(&mut runtime, &deck, &mut now, FRAME),
        [Command::SetEqOn(false)]
    );
}

#[test]
fn a_click_in_the_queue_plays_that_row_and_the_wheel_scrolls() {
    let mut deck = deck(PlayState::Playing);
    for row in 0..30 {
        let mut next = song();
        next.title = format!("Song {row}");
        deck.queue.push(next);
    }
    let (mut runtime, key, mut now) = opened(&deck);
    turn_page(&mut runtime, key, &deck, &mut now);
    turn_page(&mut runtime, key, &deck, &mut now);
    click(&mut runtime, key, (200.0, 38.0 + 37.0), now);
    assert_eq!(
        run(&mut runtime, &deck, &mut now, FRAME),
        [Command::PlayQueued(2)]
    );
    runtime.input(key, Input::Wheel(-3.0), Modifiers::default(), now);
    click(&mut runtime, key, (200.0, 38.0 + 7.0), now);
    assert_eq!(
        run(&mut runtime, &deck, &mut now, FRAME),
        [Command::PlayQueued(9)]
    );
}

#[test]
fn the_transport_reaches_the_player() {
    assert_eq!(asks(PlayState::Playing, (230.0, 147.0)), [Command::Pause]);
    assert_eq!(asks(PlayState::Paused, (230.0, 147.0)), [Command::Play]);
    assert_eq!(
        asks(PlayState::Playing, (190.0, 147.0)),
        [Command::Previous]
    );
    assert_eq!(asks(PlayState::Playing, (269.0, 147.0)), [Command::Next]);
}

#[test]
fn seek_volume_and_mute_reach_the_player() {
    let halfway = asks(PlayState::Playing, (245.0, 106.0));
    assert!(
        matches!(halfway[..], [Command::Seek(at)] if at.abs_diff(100_000) < 3000),
        "{halfway:?}"
    );
    let middle = asks(PlayState::Playing, (373.0, 151.0));
    assert!(
        matches!(middle[..], [Command::SetVolume(47..=53)]),
        "{middle:?}"
    );
    assert_eq!(
        asks(PlayState::Playing, (316.0, 151.0)),
        [Command::SetMute(true)]
    );
}

#[test]
fn the_ribbon_drags_the_window() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, now) = opened(&deck);
    for place in [(440.0, 90.0), (330.0, 18.0)] {
        click(&mut runtime, key, place, now);
        assert_eq!(runtime.window_commands(key), [WindowCommand::Drag]);
    }
}

#[test]
fn the_window_buttons_work() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    click(&mut runtime, key, (359.0, 57.0), now);
    assert_eq!(runtime.window_commands(key), [WindowCommand::Minimize]);
    click(&mut runtime, key, (373.0, 57.0), now);
    assert_eq!(
        run(&mut runtime, &deck, &mut now, FRAME),
        [Command::BackToMain]
    );
    assert!(!runtime.closed);
    click(&mut runtime, key, (387.0, 57.0), now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(300));
    assert!(!runtime.closed, "it folds away first");
    let commands = run(&mut runtime, &deck, &mut now, Duration::from_millis(1000));
    assert!(runtime.closed);
    assert_eq!(commands, [Command::BackToMain]);
}

#[test]
fn it_is_listed_right_after_the_tall_one() {
    assert_eq!(crate::skin::SHIPPED, ["Spotifast Tall", "Spotifast Wide"]);
    assert!(crate::skin::preview::of_skin(&Skin::wide().expect("reads")).is_some());
}
