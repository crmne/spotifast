//! The Windows Media Player skin the app ships, run from its real archive.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::deck::{Command, Deck, PlayState, Song};
use super::live::{Kind, WindowCommand};
use super::runtime::{Input, Modifiers, Runtime};
use crate::skin::Skin;

pub(super) const FRAME: Duration = Duration::from_millis(16);
/// Longer than the opening.
const OPENED: Duration = Duration::from_millis(2600);

pub(super) fn song() -> Song {
    Song {
        title: "Song".into(),
        artist: "Band".into(),
        album: "Album".into(),
        uri: "spotify:track:1".into(),
        duration_s: 200.0,
    }
}

pub(super) fn deck(state: PlayState) -> Deck {
    Deck {
        state,
        song: Some(song()),
        queue: vec![song()],
        playing: Some(0),
        position_s: 50.0,
        volume: 80,
        ..Deck::default()
    }
}

fn start(deck: &Deck) -> (Runtime, u32, Instant) {
    let skin = Skin::ribbon().expect("the skin reads");
    let wmp = skin.wmp.clone().expect("a Windows Media Player skin");
    assert_eq!(wmp.title, "Spotifast Tall");
    let runtime = Runtime::new(
        crate::skin::RIBBON.into(),
        wmp,
        deck.clone(),
        HashMap::new(),
        Arc::new(|text: &str, _, _, _| text.chars().count() as f32 * 6.0),
        || {},
    );
    let at = runtime.primary().expect("a window");
    let key = runtime.views[at].state.key;
    (runtime, key, Instant::now())
}

pub(super) fn run(
    runtime: &mut Runtime,
    deck: &Deck,
    now: &mut Instant,
    length: Duration,
) -> Vec<Command> {
    let end = *now + length;
    let mut commands = Vec::new();
    while *now < end {
        *now += FRAME;
        commands.extend(runtime.tick(deck.clone(), *now));
    }
    commands
}

/// Paints until every picture is in: a click on a picture not yet decoded
/// counts as landing on it, as it does before the app's first frames.
pub(super) fn paint_all(runtime: &mut Runtime, key: u32, now: Instant) {
    loop {
        let _ = runtime.paint(key, now);
        if !runtime.images.loading() {
            break;
        }
        runtime.images.poll();
        std::thread::sleep(Duration::from_millis(1));
    }
    let _ = runtime.paint(key, now);
}

fn opened(deck: &Deck) -> (Runtime, u32, Instant) {
    let (mut runtime, key, mut now) = start(deck);
    run(&mut runtime, deck, &mut now, OPENED);
    paint_all(&mut runtime, key, now);
    assert_eq!(runtime.logged_errors(), Vec::<String>::new());
    (runtime, key, now)
}

pub(super) fn text(runtime: &Runtime, key: u32, id: &str) -> String {
    let view = runtime.view(key).expect("the view");
    view.state.elements[view.state.ids[id]]
        .str("value")
        .unwrap_or_default()
}

pub(super) fn click(runtime: &mut Runtime, key: u32, place: (f32, f32), now: Instant) {
    let none = Modifiers::default();
    let pointer = Input::Pointer {
        x: place.0,
        y: place.1,
    };
    runtime.input(key, pointer, none, now);
    runtime.input(key, Input::Press { secondary: false }, none, now);
    runtime.input(key, Input::Release { secondary: false }, none, now);
}

/// What a click on a place asks of the player.
fn asks(state: PlayState, place: (f32, f32)) -> Vec<Command> {
    let deck = deck(state);
    let (mut runtime, key, mut now) = opened(&deck);
    click(&mut runtime, key, place, now);
    run(&mut runtime, &deck, &mut now, FRAME)
}

#[test]
fn it_is_small_and_holds_every_picture_it_names() {
    let skin = Skin::ribbon().expect("the skin reads");
    let wmp = skin.wmp.as_ref().expect("a Windows Media Player skin");
    let total: usize = wmp.files.values().map(Vec::len).sum();
    assert!(total < 4 << 20, "{total} bytes unpacked");
    let (runtime, key, _) = start(&deck(PlayState::Playing));
    let view = runtime.view(key).expect("the view");
    for element in &view.state.elements {
        for name in [
            "image",
            "hoverimage",
            "downimage",
            "disabledimage",
            "backgroundimage",
            "positionimage",
            "thumbimage",
        ] {
            if let Some(file) = element.image_name(name)
                && !file.starts_with("wmpimage")
            {
                assert!(wmp.files.contains_key(&file), "{file} is not in the skin");
            }
        }
    }
}

#[test]
fn every_control_shows_hover_and_press() {
    let (runtime, key, _) = start(&deck(PlayState::Playing));
    let view = runtime.view(key).expect("the view");
    let mut checked = 0;
    for element in &view.state.elements {
        if element.kind != Kind::Button || element.image_name("image").is_none() {
            continue;
        }
        assert!(
            element.image_name("hoverimage").is_some(),
            "{:?}",
            element.id
        );
        assert!(
            element.image_name("downimage").is_some(),
            "{:?}",
            element.id
        );
        checked += 1;
    }
    assert_eq!(checked, 11, "window, pages, pane and transport");
}

#[test]
fn the_source_and_the_archive_agree() {
    let skin = Skin::ribbon().expect("the skin reads");
    let wmp = skin.wmp.as_ref().expect("a Windows Media Player skin");
    let hint = "run `cargo run --example ribbon_skin` after changing the skin's source";
    assert_eq!(
        wmp.files["ribbon.js"],
        include_bytes!("../../../assets/skins/ribbon/ribbon.js"),
        "{hint}"
    );
    let source = include_str!("../../../assets/skins/ribbon/ribbon.wms");
    let (before, after) = source.split_once("<!-- @bands -->\n").expect("the marker");
    let definition = String::from_utf8(wmp.files["ribbon.wms"].clone()).expect("text");
    assert!(
        definition.starts_with(before) && definition.ends_with(after),
        "{hint}"
    );
}

#[test]
fn light_comes_first_then_the_whole_object_then_its_screen() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = start(&deck);
    let whole = |runtime: &Runtime| {
        let view = runtime.view(key).expect("the view");
        view.state.elements[view.state.ids["object"]].alpha
    };
    assert_eq!(whole(&runtime), 1.0, "nothing of the object shows");
    run(&mut runtime, &deck, &mut now, Duration::from_millis(320));
    let traced = runtime.read(key, "trace", "value");
    assert!(traced > 0.0 && traced < 13.0, "part way round: {traced}");
    assert_eq!(runtime.read(key, "wipe", "value"), 0.0, "light alone");
    assert_eq!(whole(&runtime), 1.0);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(400));
    assert_eq!(runtime.read(key, "trace", "value"), 13.0, "all the way");
    assert!(runtime.read(key, "wipe", "value") > 0.0, "the wipe follows");
    assert_eq!(whole(&runtime), 1.0, "the plate, not its layers");
    run(&mut runtime, &deck, &mut now, Duration::from_millis(200));
    assert_eq!(whole(&runtime), 255.0, "both materials at once");
    assert_eq!(runtime.read(key, "wipe", "visible"), 0.0);
    assert_eq!(runtime.read(key, "trace", "visible"), 0.0);
    assert_eq!(runtime.read(key, "art", "visible"), 0.0, "not yet");
    run(&mut runtime, &deck, &mut now, OPENED);
    assert_eq!(runtime.read(key, "art", "visible"), 1.0);
    assert_eq!(runtime.read(key, "visual", "visible"), 0.0);
    assert_eq!(runtime.read(key, "veil", "top"), -126.0);
    assert_eq!(runtime.read(key, "playButton", "visible"), 0.0);
    assert_eq!(runtime.read(key, "pauseButton", "visible"), 1.0);
    assert_eq!(text(&runtime, key, "title"), "Song");
    assert_eq!(text(&runtime, key, "artist"), "Band");
    assert_eq!(text(&runtime, key, "time"), "00:50");
    assert!(
        runtime.next_wake(now) > Some(Duration::from_millis(100)),
        "nothing animates once it has opened"
    );
}

#[test]
fn the_opening_waits_for_the_window() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = start(&deck);
    runtime.await_window();
    run(&mut runtime, &deck, &mut now, Duration::from_millis(800));
    assert_eq!(runtime.read(key, "trace", "value"), 0.0, "still waiting");
    paint_all(&mut runtime, key, now);
    runtime.window_ready(key);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(320));
    assert!(runtime.read(key, "trace", "value") > 0.0, "off it goes");
}

#[test]
fn a_window_that_never_fits_does_not_leave_it_dark() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = start(&deck);
    runtime.await_window();
    run(&mut runtime, &deck, &mut now, Duration::from_millis(1500));
    run(&mut runtime, &deck, &mut now, OPENED);
    assert_eq!(runtime.read(key, "art", "visible"), 1.0);
}

#[test]
fn the_visualizer_fills_the_glass_under_the_ribbon_and_the_words() {
    use super::paint::Paint;
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    click(&mut runtime, key, (125.0, 138.0), now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(800));
    assert_eq!(runtime.read(key, "wave", "visible"), 0.0);
    assert_eq!(runtime.read(key, "fx", "width"), 176.0);
    assert_eq!(runtime.read(key, "fx", "height"), 276.0);
    paint_all(&mut runtime, key, now);
    let (paints, _) = runtime.paint(key, now);
    let pane = paints
        .iter()
        .position(|paint| matches!(paint, Paint::Effects { .. }))
        .expect("the pane");
    let Paint::Effects { clip, .. } = &paints[pane] else {
        unreachable!();
    };
    assert!(clip.len() > 8, "cut to the glass, not a box");
    let words = paints
        .iter()
        .position(|paint| matches!(paint, Paint::Text { text, .. } if text.text == "Song"))
        .expect("the title");
    assert!(words > pane, "the title draws over the picture");
    // Back to the art, by clicking the picture.
    click(&mut runtime, key, (125.0, 138.0), now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(800));
    assert_eq!(runtime.read(key, "visual", "visible"), 0.0);
    assert_eq!(runtime.read(key, "art", "visible"), 1.0);
    assert_eq!(runtime.read(key, "veil", "top"), -126.0);
}

#[test]
fn the_glass_turns_through_its_pages_and_remembers() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    let dots = (86.0, 64.0);
    click(&mut runtime, key, dots, now);
    assert_eq!(runtime.shared.prefs["page"], "eq");
    run(&mut runtime, &deck, &mut now, Duration::from_millis(400));
    assert_eq!(runtime.read(key, "eqPage", "visible"), 1.0);
    assert_eq!(runtime.read(key, "nowPage", "visible"), 0.0);
    assert_eq!(runtime.read(key, "art", "visible"), 0.0);
    assert_eq!(runtime.read(key, "veil", "top"), 0.0, "plain glass");
    paint_all(&mut runtime, key, now);
    click(&mut runtime, key, dots, now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(400));
    assert_eq!(runtime.read(key, "queuePage", "visible"), 1.0);
    assert_eq!(runtime.read(key, "eqPage", "visible"), 0.0);
    assert_eq!(runtime.shared.prefs["page"], "queue");

    let prefs = runtime.shared.prefs.clone();
    let skin = Skin::ribbon().expect("the skin reads");
    let mut again = Runtime::new(
        crate::skin::RIBBON.into(),
        skin.wmp.clone().expect("a Windows Media Player skin"),
        deck.clone(),
        prefs,
        Arc::new(|text: &str, _, _, _| text.chars().count() as f32 * 6.0),
        || {},
    );
    let reopened = again.views[again.primary().expect("a window")].state.key;
    let mut later = Instant::now();
    run(&mut again, &deck, &mut later, OPENED);
    assert_eq!(again.read(reopened, "queuePage", "visible"), 1.0);

    paint_all(&mut runtime, key, now);
    click(&mut runtime, key, dots, now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(500));
    assert_eq!(runtime.read(key, "nowPage", "visible"), 1.0);
    assert_eq!(runtime.read(key, "art", "visible"), 1.0);
    assert_eq!(runtime.read(key, "veil", "top"), -126.0, "open again");
}

#[test]
fn the_equalizer_page_reaches_the_player() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    click(&mut runtime, key, (86.0, 64.0), now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(400));
    paint_all(&mut runtime, key, now);
    // Near the top of the eighth band, then the preamp, then the switch.
    click(&mut runtime, key, (159.0, 120.0), now);
    let commands = run(&mut runtime, &deck, &mut now, FRAME);
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, Command::SetEqBand(7, gain) if *gain > 0.0)),
        "{commands:?}"
    );
    click(&mut runtime, key, (66.0, 120.0), now);
    let commands = run(&mut runtime, &deck, &mut now, FRAME);
    assert!(
        matches!(commands[..], [Command::SetEqPreamp(gain)] if gain > 0.0),
        "{commands:?}"
    );
    click(&mut runtime, key, (178.0, 84.0), now);
    let commands = run(&mut runtime, &deck, &mut now, FRAME);
    // Moving a band turned it on, so this turns it off.
    assert_eq!(commands, [Command::SetEqOn(false)]);
}

#[test]
fn a_click_in_the_queue_plays_that_row() {
    let mut deck = deck(PlayState::Playing);
    for row in 0..30 {
        let mut next = song();
        next.title = format!("Song {row}");
        deck.queue.push(next);
    }
    let (mut runtime, key, mut now) = opened(&deck);
    for _ in 0..2 {
        click(&mut runtime, key, (86.0, 64.0), now);
        run(&mut runtime, &deck, &mut now, Duration::from_millis(400));
        paint_all(&mut runtime, key, now);
    }
    // The third row, one click.
    click(&mut runtime, key, (120.0, 98.0 + 37.0), now);
    assert_eq!(
        run(&mut runtime, &deck, &mut now, FRAME),
        [Command::PlayQueued(2)]
    );
    // The wheel scrolls it.
    runtime.input(key, Input::Wheel(-3.0), Modifiers::default(), now);
    click(&mut runtime, key, (120.0, 98.0 + 7.0), now);
    assert_eq!(
        run(&mut runtime, &deck, &mut now, FRAME),
        [Command::PlayQueued(9)]
    );
}

#[test]
fn the_glass_closes_over_the_square_with_nothing_playing() {
    let deck = Deck::default();
    let (runtime, key, _) = opened(&deck);
    assert_eq!(runtime.read(key, "veil", "top"), 0.0);
    assert_eq!(text(&runtime, key, "title"), "Nothing playing");
    assert_eq!(runtime.read(key, "wave", "visible"), 0.0);
}

#[test]
fn the_square_switches_and_remembers() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    // Clicking the art.
    click(&mut runtime, key, (125.0, 138.0), now);
    assert_eq!(runtime.shared.prefs["pane"], "visual");
    run(&mut runtime, &deck, &mut now, Duration::from_millis(800));
    assert_eq!(runtime.read(key, "visual", "visible"), 1.0);
    assert_eq!(runtime.read(key, "art", "visible"), 0.0);
    paint_all(&mut runtime, key, now);
    // Clicking the visualizer goes back.
    click(&mut runtime, key, (125.0, 138.0), now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(800));
    assert_eq!(runtime.read(key, "art", "visible"), 1.0);
    assert_eq!(runtime.shared.prefs["pane"], "art");
    // And the small button.
    click(&mut runtime, key, (182.0, 273.0), now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(800));
    assert_eq!(runtime.read(key, "visual", "visible"), 1.0);

    let prefs = runtime.shared.prefs.clone();
    let skin = Skin::ribbon().expect("the skin reads");
    let mut again = Runtime::new(
        crate::skin::RIBBON.into(),
        skin.wmp.clone().expect("a Windows Media Player skin"),
        deck.clone(),
        prefs,
        Arc::new(|text: &str, _, _, _| text.chars().count() as f32 * 6.0),
        || {},
    );
    let key = again.views[again.primary().expect("a window")].state.key;
    let mut now = Instant::now();
    run(&mut again, &deck, &mut now, OPENED);
    assert_eq!(again.read(key, "visual", "visible"), 1.0, "after a restart");
    assert_eq!(again.read(key, "art", "visible"), 0.0);
}

#[test]
fn the_waveform_is_drawn_only_while_the_music_plays() {
    let wave_in = |runtime: &mut Runtime, key: u32, now: Instant| {
        let (paints, _) = runtime.paint(key, now);
        paints
            .iter()
            .any(|paint| matches!(paint, super::paint::Paint::Wave { color, .. } if *color == [0x39, 0xff, 0x88]))
    };
    let mut deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    assert!(wave_in(&mut runtime, key, now));
    deck.state = PlayState::Paused;
    run(&mut runtime, &deck, &mut now, FRAME);
    assert!(!wave_in(&mut runtime, key, now));
}

#[test]
fn the_transport_reaches_the_player() {
    assert_eq!(asks(PlayState::Playing, (118.0, 338.0)), [Command::Pause]);
    assert_eq!(asks(PlayState::Paused, (118.0, 338.0)), [Command::Play]);
    assert_eq!(asks(PlayState::Playing, (79.0, 338.0)), [Command::Previous]);
    assert_eq!(asks(PlayState::Playing, (157.0, 338.0)), [Command::Next]);
}

#[test]
fn seek_volume_and_mute_reach_the_player() {
    let halfway = asks(PlayState::Playing, (126.0, 285.0));
    assert!(
        matches!(halfway[..], [Command::Seek(at)] if at.abs_diff(100_000) < 2000),
        "{halfway:?}"
    );
    let middle = asks(PlayState::Playing, (132.0, 381.0));
    assert!(
        matches!(middle[..], [Command::SetVolume(48..=52)]),
        "{middle:?}"
    );
    assert_eq!(
        asks(PlayState::Playing, (74.0, 381.0)),
        [Command::SetMute(true)]
    );
}

#[test]
fn the_ribbon_drags_the_window() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, now) = opened(&deck);
    // The ceramic on the left, and the arch at the top.
    for place in [(26.0, 200.0), (130.0, 30.0)] {
        click(&mut runtime, key, place, now);
        assert_eq!(runtime.window_commands(key), [WindowCommand::Drag]);
    }
}

#[test]
fn the_window_buttons_work() {
    let deck = deck(PlayState::Playing);
    let (mut runtime, key, mut now) = opened(&deck);
    click(&mut runtime, key, (157.0, 64.0), now);
    assert_eq!(runtime.window_commands(key), [WindowCommand::Minimize]);
    click(&mut runtime, key, (171.0, 64.0), now);
    assert_eq!(
        run(&mut runtime, &deck, &mut now, FRAME),
        [Command::BackToMain]
    );
    assert!(!runtime.closed, "returning leaves the skin to the app");
    click(&mut runtime, key, (185.0, 64.0), now);
    run(&mut runtime, &deck, &mut now, Duration::from_millis(300));
    assert!(!runtime.closed, "it folds away first");
    let commands = run(&mut runtime, &deck, &mut now, Duration::from_millis(1000));
    assert!(runtime.closed);
    assert_eq!(commands, [Command::BackToMain]);
}

#[test]
fn the_fallback_bars_wear_the_classic_palette() {
    let skin = Skin::ribbon().expect("the skin reads");
    assert_eq!(skin.vis_colors, Skin::builtin().vis_colors);
}

#[test]
fn the_card_shows_it_open() {
    let skin = Skin::ribbon().expect("the skin reads");
    assert!(crate::skin::preview::of_skin(&skin).is_some());
}
