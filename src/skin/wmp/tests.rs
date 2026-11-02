//! A skin run from end to end, with synthetic definitions and scripts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::deck::{Command, Deck, PlayState, Song};
use super::live::WindowCommand;
use super::paint::Paint;
use super::runtime::{Input, Modifiers, Runtime};
use super::*;

fn png(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| image::Rgb(paint(x, y)));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn playing() -> Deck {
    let song = Song {
        title: "Song".into(),
        artist: "Band".into(),
        album: "Album".into(),
        uri: "spotify:track:1".into(),
        duration_s: 200.0,
    };
    Deck {
        state: PlayState::Playing,
        song: Some(song.clone()),
        queue: vec![song],
        playing: Some(0),
        ..Deck::default()
    }
}

fn start_with(definition: &str, files: Vec<(&str, Vec<u8>)>, deck: Deck) -> Runtime {
    let files: HashMap<String, Vec<u8>> = files
        .into_iter()
        .map(|(name, bytes)| (name.to_owned(), bytes))
        .collect();
    let wmp = Wmp::parse(definition, "skin", Arc::new(files)).expect("a view");
    Runtime::new(
        "skin".into(),
        Arc::new(wmp),
        deck,
        HashMap::new(),
        Arc::new(|text: &str, _, _, _| text.chars().count() as f32 * 6.0),
        || {},
    )
}

fn start(definition: &str, script: &str) -> Runtime {
    start_with(
        definition,
        vec![("s.js", script.as_bytes().to_vec())],
        playing(),
    )
}

fn key_of(runtime: &Runtime) -> u32 {
    let at = runtime.primary().expect("a window");
    runtime.views[at].state.key
}

fn pointer(runtime: &mut Runtime, x: f32, y: f32, now: Instant) {
    let key = key_of(runtime);
    runtime.input(key, Input::Pointer { x, y }, Modifiers::default(), now);
}

fn click(runtime: &mut Runtime, x: f32, y: f32, now: Instant) {
    let key = key_of(runtime);
    pointer(runtime, x, y, now);
    let none = Modifiers::default();
    runtime.input(key, Input::Press { secondary: false }, none, now);
    runtime.input(key, Input::Release { secondary: false }, none, now);
}

const VIEW: &str = r#"<THEME title="T"><VIEW id="main" width="100" height="50" scriptFile="s.js">"#;

#[test]
fn a_script_opens_a_drawer_and_hears_it_arrive() {
    let definition = format!(
        r#"{VIEW}
        <BUTTON id="b" width="10" height="10" onClick="openDrawer();"/>
        <SUBVIEW id="drawer" left="20" top="0" width="30" height="30" onEndMove="arrived();"/>
        <TEXT id="flag" left="0" top="30" visible="false" value="x"/>
        </VIEW></THEME>"#
    );
    let script = "function openDrawer(){drawer.moveTo(60,5,100);}\
                  function arrived(){flag.visible=true;}";
    let mut runtime = start(&definition, script);
    let key = key_of(&runtime);
    let now = Instant::now();
    click(&mut runtime, 5.0, 5.0, now);
    assert_eq!(runtime.next_wake(now), Some(Duration::from_millis(16)));
    runtime.tick(playing(), now + Duration::from_millis(50));
    let left = runtime.read(key, "drawer", "left");
    assert!(left > 20.0 && left < 60.0, "{left}");
    assert_eq!(runtime.read(key, "flag", "visible"), 0.0);
    runtime.tick(playing(), now + Duration::from_millis(150));
    assert_eq!(runtime.read(key, "drawer", "left"), 60.0);
    assert_eq!(runtime.read(key, "drawer", "top"), 5.0);
    assert_eq!(runtime.read(key, "flag", "visible"), 1.0);
}

#[test]
fn a_bug_in_a_handler_stops_only_that_handler() {
    let definition = format!(
        r#"{VIEW}<TEXT id="t" value="a"/>
        <BUTTON id="bad" left="0" width="10" height="10" onClick="nothingHere.go();"/>
        <BUTTON id="good" left="20" width="10" height="10" onClick="t.value='b';"/>
        </VIEW></THEME>"#
    );
    // A syntax error in the second file does not hurt the first.
    let mut runtime = start(&definition, "var loaded = 1;");
    let now = Instant::now();
    click(&mut runtime, 5.0, 5.0, now);
    click(&mut runtime, 25.0, 5.0, now);
    let view = runtime.view(key_of(&runtime)).unwrap();
    let id = view.state.ids["t"];
    assert_eq!(view.state.elements[id].str("value").as_deref(), Some("b"));
}

#[test]
fn scripts_may_get_the_case_of_a_name_wrong() {
    let definition = format!(
        r#"{VIEW}<TEXT id="tName" value=""/>
        <BUTTON width="10" height="10" onClick="TNAME.Value = PLAYER.currentmedia.NAME; Startvideo();"/>
        </VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "function StartVideo(){tName.value += '!';}");
    click(&mut runtime, 5.0, 5.0, Instant::now());
    let view = runtime.view(key_of(&runtime)).unwrap();
    let id = view.state.ids["tname"];
    assert_eq!(
        view.state.elements[id].str("value").as_deref(),
        Some("Song!")
    );
}

#[test]
fn a_runaway_handler_is_stopped_then_switched_off() {
    let definition = format!(
        r#"{VIEW}<BUTTON width="10" height="10" onClick="spin();"/>
        <BUTTON left="20" width="10" height="10" onClick="player.controls.pause();"/>
        </VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "function spin(){while(true){}}");
    let now = Instant::now();
    for _ in 0..5 {
        click(&mut runtime, 5.0, 5.0, now);
    }
    // The skin still works.
    click(&mut runtime, 25.0, 5.0, now);
    assert_eq!(runtime.tick(playing(), now), [Command::Pause]);
}

#[test]
fn bindings_follow_the_player_and_fire_their_handlers() {
    let definition = format!(
        r#"{VIEW}
        <PLAYER playstatechange="seen.value = 'state ' + NewState;" openstate_onchange="media.value = player.currentMedia.name;"/>
        <TEXT id="seen" value=""/><TEXT id="media" value=""/>
        <TEXT id="clock" value="wmpprop:player.controls.currentPositionString"/>
        <SLIDER id="vol" value="wmpprop:player.settings.volume" value_onchange="echo.value=value;" width="50" height="10"/>
        <TEXT id="echo" value=""/>
        <PAUSEBUTTON id="p" width="9" height="9" visible="wmpenabled:player.controls.pause"/>
        </VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "");
    let key = key_of(&runtime);
    let text = |runtime: &Runtime, id: &str| {
        let view = runtime.view(key).unwrap();
        view.state.elements[view.state.ids[id]].str("value")
    };
    assert_eq!(runtime.read(key, "p", "visible"), 1.0);
    let mut deck = playing();
    deck.state = PlayState::Paused;
    deck.position_s = 83.0;
    deck.volume = 40;
    let mut other = deck.song.clone().unwrap();
    other.title = "Next".into();
    deck.song = Some(other);
    runtime.tick(deck, Instant::now());
    assert_eq!(text(&runtime, "seen").as_deref(), Some("state 2"));
    assert_eq!(text(&runtime, "media").as_deref(), Some("Next"));
    assert_eq!(text(&runtime, "clock").as_deref(), Some("01:23"));
    assert_eq!(text(&runtime, "echo").as_deref(), Some("40"));
    assert_eq!(
        runtime.read(key, "p", "visible"),
        0.0,
        "pause goes when paused"
    );
}

#[test]
fn a_jscript_attribute_is_worked_out_once_and_sees_the_elements_size() {
    let definition = format!(
        r#"{VIEW}
        <SUBVIEW id="a" left="10" width="30" height="5"/>
        <TEXT id="t" left="jscript:a.left+a.width+2;" top="jscript:view.height-left;" value="x"/>
        </VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "");
    let key = key_of(&runtime);
    assert_eq!(runtime.read(key, "t", "left"), 42.0);
    assert_eq!(runtime.read(key, "t", "top"), 8.0);
    runtime.run_script(key, "a.width = 90;", Instant::now());
    assert_eq!(runtime.read(key, "t", "left"), 42.0);
}

#[test]
fn a_resize_moves_what_follows_the_parents_edges() {
    let definition = format!(
        r#"{VIEW}
        <SUBVIEW id="stretch" left="10" width="80" height="10" horizontalAlignment="stretch">
          <SUBVIEW id="grip" left="70" width="10" height="10" horizontalAlignment="right"/>
        </SUBVIEW>
        <SUBVIEW id="corner" left="90" top="40" width="10" height="10"
           horizontalAlignment="right" verticalAlignment="bottom"/>
        <SUBVIEW id="still" left="5" top="5" width="10" height="10"/>
        </VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "");
    let key = key_of(&runtime);
    runtime.run_script(key, "view.width = 200; view.height = 80;", Instant::now());
    assert_eq!(runtime.read(key, "stretch", "width"), 180.0);
    assert_eq!(runtime.read(key, "grip", "left"), 170.0);
    assert_eq!(runtime.read(key, "corner", "left"), 190.0);
    assert_eq!(runtime.read(key, "corner", "top"), 70.0);
    assert_eq!(runtime.read(key, "still", "left"), 5.0);
}

#[test]
fn a_centred_element_with_no_place_starts_in_the_middle() {
    let definition = format!(
        r#"{VIEW}
        <SUBVIEW id="c" width="20" height="10" horizontalAlignment="center" verticalAlignment="center"/>
        <SUBVIEW id="d" left="3" width="20" height="10" horizontalAlignment="center"/>
        </VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "");
    let key = key_of(&runtime);
    assert_eq!(runtime.read(key, "c", "left"), 40.0);
    assert_eq!(runtime.read(key, "c", "top"), 20.0);
    assert_eq!(runtime.read(key, "d", "left"), 3.0);
    runtime.run_script(key, "view.width = 140;", Instant::now());
    assert_eq!(runtime.read(key, "c", "left"), 60.0, "and it stays there");
}

#[test]
fn views_have_their_own_names_and_share_preferences() {
    let definition = r#"<THEME>
      <VIEW id="control" scriptFile="s.js" onLoad="startUp();"/>
      <VIEW id="main" width="50" height="50" scriptFile="s.js" onLoad="shared = 2; theme.savePreference('k', shared);">
        <BUTTON width="9" height="9" onClick="theme.openView('eq');"/>
        <BUTTON left="20" width="9" height="9" onClick="view.close();"/>
      </VIEW>
      <VIEW id="eq" width="20" height="20" scriptFile="s.js" onLoad="t.value = shared + theme.loadPreference('k') + theme.loadPreference('none');">
        <TEXT id="t" value=""/>
      </VIEW></THEME>"#;
    let script = "var shared = 1; function startUp(){ theme.openView('main'); }";
    let mut runtime = start(definition, script);
    let now = Instant::now();
    assert_eq!(
        runtime.views.len(),
        2,
        "the control view runs without a window"
    );
    assert_eq!(runtime.views[runtime.primary().unwrap()].state.id, "main");
    click(&mut runtime, 4.0, 4.0, now);
    assert_eq!(runtime.views.len(), 3);
    let eq = runtime
        .views
        .iter()
        .find(|view| view.state.id == "eq")
        .unwrap();
    let id = eq.state.ids["t"];
    assert_eq!(eq.state.elements[id].str("value").as_deref(), Some("12--"));
    // Opening it again does not make a second one.
    click(&mut runtime, 4.0, 4.0, now);
    assert_eq!(runtime.views.len(), 3);
    // Closing the main one leaves the rest; closing the last window ends it.
    click(&mut runtime, 24.0, 4.0, now);
    assert_eq!(runtime.views[runtime.primary().unwrap()].state.id, "eq");
    let key = key_of(&runtime);
    runtime.run_script(key, "view.close();", now);
    assert_eq!(runtime.tick(playing(), now), [Command::BackToMain]);
}

#[test]
fn a_view_can_give_way_to_another_at_load() {
    // Preview views do this: they empty themselves and name the real one.
    let definition = r#"<THEME>
      <VIEW id="preview" backgroundImage="p.png" scriptFile="s.js" onLoad="view.width=0; view.height=0; theme.currentViewID='real';"/>
      <VIEW id="real" width="40" height="30" onLoad="theme.savePreference('b', true);"/>
      </THEME>"#;
    let runtime = start_with(
        definition,
        vec![("p.png", png(9, 9, |_, _| [1, 2, 3]))],
        playing(),
    );
    assert_eq!(runtime.views.len(), 1);
    assert_eq!(runtime.views[0].state.id, "real");
    // A boolean reads back as COM wrote it.
    assert_eq!(runtime.shared.prefs["b"], "True");
}

#[test]
fn a_sticky_button_toggles_before_its_click_runs() {
    let definition = format!(r#"{VIEW}<MUTEBUTTON id="m" width="10" height="10"/></VIEW></THEME>"#);
    let mut runtime = start(&definition, "");
    let key = key_of(&runtime);
    let now = Instant::now();
    click(&mut runtime, 5.0, 5.0, now);
    assert_eq!(runtime.tick(playing(), now), [Command::SetMute(true)]);
    assert_eq!(runtime.read(key, "m", "down"), 1.0);
}

#[test]
fn a_button_group_is_hit_by_its_map_and_drawn_only_where_mapped() {
    let map = png(40, 20, |x, _| {
        if x < 10 {
            [0, 255, 0]
        } else if (20..30).contains(&x) {
            [255, 0, 0]
        } else {
            [0, 0, 0]
        }
    });
    let definition = format!(
        r##"{VIEW}<BUTTONGROUP left="5" top="6" image="up.png" mappingImage="map.png">
          <PLAYELEMENT mappingColor="#00FF00"/>
          <BUTTONELEMENT mappingColor="#FF0000" onClick="player.controls.next();"/>
        </BUTTONGROUP></VIEW></THEME>"##
    );
    let mut deck = playing();
    deck.state = PlayState::Paused;
    let mut runtime = start_with(
        &definition,
        vec![
            ("map.png", map),
            ("up.png", png(40, 20, |_, _| [1, 1, 1])),
            ("s.js", Vec::new()),
        ],
        deck.clone(),
    );
    let now = Instant::now();
    click(&mut runtime, 8.0, 8.0, now);
    assert_eq!(runtime.tick(deck.clone(), now), [Command::Play]);
    click(&mut runtime, 30.0, 8.0, now);
    assert_eq!(runtime.tick(deck.clone(), now), [Command::Next]);
    // Between the colours there is no button.
    click(&mut runtime, 20.0, 8.0, now);
    assert_eq!(runtime.tick(deck, now), []);
}

#[test]
fn dragging_a_seek_slider_holds_its_value_and_seeks_at_the_end() {
    let definition = format!(
        r#"{VIEW}<SEEKSLIDER id="s" width="100" height="10" thumbImage="t.png"/></VIEW></THEME>"#
    );
    let mut runtime = start_with(
        &definition,
        vec![
            ("t.png", png(4, 10, |_, _| [9, 9, 9])),
            ("s.js", Vec::new()),
        ],
        playing(),
    );
    let key = key_of(&runtime);
    let none = Modifiers::default();
    let now = Instant::now();
    pointer(&mut runtime, 50.0, 5.0, now);
    runtime.input(key, Input::Press { secondary: false }, none, now);
    assert_eq!(runtime.read(key, "s", "value"), 100.0);
    // The player's report does not fight the hand, and nothing is sought yet.
    let mut deck = playing();
    deck.position_s = 7.0;
    assert_eq!(runtime.tick(deck.clone(), now), []);
    assert_eq!(runtime.read(key, "s", "value"), 100.0);
    pointer(&mut runtime, 25.0, 5.0, now);
    runtime.input(key, Input::Release { secondary: false }, none, now);
    assert_eq!(runtime.tick(deck, now), [Command::Seek(50_000)]);
}

/// As the app's own sliders do: heard while it moves, settled on release.
#[test]
fn dragging_a_volume_slider_previews_and_settles_on_release() {
    let definition = format!(
        r#"{VIEW}<VOLUMESLIDER id="v" width="100" height="10" thumbImage="t.png"/></VIEW></THEME>"#
    );
    let mut runtime = start_with(
        &definition,
        vec![
            ("t.png", png(4, 10, |_, _| [9, 9, 9])),
            ("s.js", Vec::new()),
        ],
        playing(),
    );
    let volumes = |commands: Vec<Command>| -> Vec<Command> {
        commands
            .into_iter()
            .filter(|command| matches!(command, Command::SetVolume(_) | Command::PreviewVolume(_)))
            .collect()
    };
    let key = key_of(&runtime);
    let none = Modifiers::default();
    let now = Instant::now();
    pointer(&mut runtime, 30.0, 5.0, now);
    runtime.input(key, Input::Press { secondary: false }, none, now);
    let first = volumes(runtime.tick(playing(), now));
    assert!(
        matches!(first.as_slice(), [Command::PreviewVolume(_)]),
        "{first:?}"
    );
    pointer(&mut runtime, 60.0, 5.0, now);
    let second = volumes(runtime.tick(playing(), now));
    let [Command::PreviewVolume(dragged)] = second.as_slice() else {
        panic!("{second:?}");
    };
    let dragged = *dragged;
    assert_eq!(volumes(runtime.tick(playing(), now)), []);
    runtime.input(key, Input::Release { secondary: false }, none, now);
    assert_eq!(
        volumes(runtime.tick(playing(), now)),
        [Command::SetVolume(dragged)]
    );
    assert_eq!(volumes(runtime.tick(playing(), now)), []);
}

#[test]
fn moving_onto_a_child_does_not_leave_its_parent() {
    let definition = format!(
        r#"{VIEW}
        <SUBVIEW id="panel" width="60" height="40" backgroundColor="black" onMouseOut="out.value = out.value + 'o';">
          <BUTTON id="b" left="5" top="5" width="10" height="10" onClick="x=1;"/>
        </SUBVIEW>
        <TEXT id="out" top="42" value=""/></VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "");
    let now = Instant::now();
    pointer(&mut runtime, 30.0, 30.0, now);
    pointer(&mut runtime, 8.0, 8.0, now);
    let read = |runtime: &Runtime| {
        let view = runtime.view(key_of(runtime)).unwrap();
        view.state.elements[view.state.ids["out"]].str("value")
    };
    assert_eq!(read(&runtime).as_deref(), Some(""));
    pointer(&mut runtime, 80.0, 30.0, now);
    assert_eq!(read(&runtime).as_deref(), Some("o"));
}

#[test]
fn a_skin_with_no_timer_asks_for_no_frames() {
    let definition = format!(r#"{VIEW}<TEXT value="still"/></VIEW></THEME>"#);
    let mut deck = playing();
    deck.state = PlayState::Paused;
    let mut runtime = start_with(&definition, vec![("s.js", Vec::new())], deck.clone());
    let now = Instant::now();
    runtime.tick(deck, now);
    assert_eq!(runtime.next_wake(now), None);
}

#[test]
fn only_a_clock_or_a_seek_bar_wants_frames_while_playing() {
    let definition = format!(r#"{VIEW}<TEXT value="still"/></VIEW></THEME>"#);
    let mut runtime = start(&definition, "");
    let now = Instant::now();
    runtime.tick(playing(), now);
    assert_eq!(runtime.next_wake(now), None);
    let definition = format!(
        r#"{VIEW}<TEXT value="wmpprop:player.controls.currentPositionString"/></VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "");
    runtime.tick(playing(), now);
    assert_eq!(runtime.next_wake(now), Some(Duration::from_millis(500)));
}

#[test]
fn a_clipping_picture_takes_clicks_away_too() {
    let definition = format!(
        r##"{VIEW}<BUTTON width="20" height="10" clippingImage="c.png" clippingColor="#FF0000" onClick="player.controls.next();"/></VIEW></THEME>"##
    );
    let clip = png(20, 10, |x, _| if x < 10 { [255, 0, 0] } else { [0, 0, 0] });
    let mut runtime = start_with(
        &definition,
        vec![("c.png", clip), ("s.js", Vec::new())],
        playing(),
    );
    let now = Instant::now();
    click(&mut runtime, 5.0, 5.0, now);
    assert_eq!(runtime.tick(playing(), now), []);
    click(&mut runtime, 15.0, 5.0, now);
    assert_eq!(runtime.tick(playing(), now), [Command::Next]);
}

#[test]
fn a_timer_runs_at_its_interval_and_a_script_can_stop_it() {
    let definition = r#"<THEME><VIEW width="20" height="20" scriptFile="s.js" timerInterval="100" onTimer="beat();">
        <TEXT id="t" value=""/></VIEW></THEME>"#;
    let script =
        "var n = 0; function beat(){ n++; t.value = n; if (n == 2) view.timerInterval = 0; }";
    let mut runtime = start(definition, script);
    let key = key_of(&runtime);
    let start = runtime.views[0].state.timer_due.unwrap();
    let mut deck = playing();
    deck.state = PlayState::Paused;
    runtime.tick(deck.clone(), start);
    runtime.tick(deck.clone(), start + Duration::from_millis(100));
    runtime.tick(deck.clone(), start + Duration::from_millis(5000));
    assert_eq!(runtime.read(key, "t", "value"), 2.0);
    assert_eq!(runtime.next_wake(start), None);
}

#[test]
fn what_a_skin_cannot_touch_stays_untouched() {
    let definition = format!(
        r#"{VIEW}<BUTTON width="10" height="10" onClick="player.URL='file:///etc/passwd'; player.launchURL('http://x'); var f = theme.openDialog('FILE_OPEN','FILES_ALLMEDIA'); theme.savePreference('f', f);"/></VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "");
    let now = Instant::now();
    click(&mut runtime, 5.0, 5.0, now);
    assert_eq!(runtime.tick(playing(), now), []);
    assert_eq!(runtime.shared.prefs["f"], "");
}

#[test]
fn a_view_with_no_size_takes_its_picture_or_what_is_in_it() {
    let definition = r#"<THEME><VIEW backgroundImage="b.png"/></THEME>"#;
    let runtime = start_with(
        definition,
        vec![("b.png", png(30, 20, |_, _| [1, 1, 1]))],
        playing(),
    );
    assert_eq!(runtime.views[0].state.size(), (30.0, 20.0));
    let definition =
        r#"<THEME><VIEW><SUBVIEW left="5" top="6" width="20" height="10"/></VIEW></THEME>"#;
    let runtime = start_with(definition, Vec::new(), playing());
    assert_eq!(runtime.views[0].state.size(), (25.0, 16.0));
}

#[test]
fn a_definition_without_a_view_is_not_a_skin() {
    assert!(Wmp::parse("<THEME/>", "s", Arc::default()).is_none());
    assert!(Wmp::parse("just text", "s", Arc::default()).is_none());
}

#[test]
fn crafted_numbers_do_not_overflow() {
    let definition = r#"<THEME><VIEW width="2147483647" height="1e300">
        <SUBVIEW left="-1e308" top="2147483647" width="1e308" height="1e308">
          <BUTTON left="2147483647" top="2147483647" width="2147483647" height="2147483647" image="x"/>
          <SLIDER min="1e308" max="-1e308" value="1e308" width="9" height="9" thumbImage="x"/>
          <TEXT left="NaN" value="x" fontSize="1e9"/>
        </SUBVIEW></VIEW></THEME>"#;
    let mut runtime = start_with(definition, Vec::new(), playing());
    let now = Instant::now();
    let key = runtime.views[0].state.key;
    let _ = runtime.paint(key, now);
    runtime.input(
        key,
        Input::Pointer { x: 5.0, y: 5.0 },
        Modifiers::default(),
        now,
    );
}

/// Z order: a negative child under the picture, the rest over it.
#[test]
fn a_negative_z_child_sits_under_the_background() {
    let definition = r##"<THEME><VIEW width="20" height="20" backgroundColor="none">
        <SUBVIEW id="s" width="20" height="20" backgroundImage="b.png">
          <EFFECTS id="under" zIndex="-1" width="20" height="20"/>
          <TEXT id="over" value="x"/>
        </SUBVIEW></VIEW></THEME>"##;
    let mut runtime = start_with(
        definition,
        vec![("b.png", png(20, 20, |_, _| [1, 1, 1]))],
        playing(),
    );
    let key = runtime.views[0].state.key;
    let now = Instant::now();
    // The picture needs a moment on the worker.
    let mut paints = runtime.paint(key, now).0;
    for _ in 0..1000 {
        if paints
            .iter()
            .any(|paint| matches!(paint, Paint::Picture { .. }))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
        runtime.images.poll();
        paints = runtime.paint(key, now).0;
    }
    let order: Vec<u8> = paints
        .iter()
        .map(|paint| match paint {
            Paint::Effects { .. } | Paint::Wave { .. } => 0,
            Paint::Picture { .. } => 1,
            Paint::Text { .. } => 2,
            _ => 9,
        })
        .collect();
    assert_eq!(order, [9, 0, 1, 2], "black ground, effects, picture, text");
}

/// Loads every `.wmz` in `$SPOTIFAST_SKIN_SAMPLES`, when set, and starts it,
/// to check the engine against real files without shipping any. A skin may
/// fail to load only because it is no skin; none may panic.
#[test]
fn sample_skins_run() {
    let Ok(dir) = std::env::var("SPOTIFAST_SKIN_SAMPLES") else {
        return;
    };
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "wmz") {
            continue;
        }
        let Ok(skin) = crate::skin::Skin::load(&path) else {
            continue;
        };
        let Some(wmp) = skin.wmp.clone() else {
            continue;
        };
        let mut runtime = Runtime::new(
            "sample".into(),
            wmp,
            playing(),
            HashMap::new(),
            Arc::new(|text: &str, _, _, _| text.chars().count() as f32 * 6.0),
            || {},
        );
        let now = Instant::now();
        for step in 0..20u64 {
            let at = now + Duration::from_millis(step * 100);
            runtime.tick(playing(), at);
            let keys: Vec<u32> = runtime.views.iter().map(|view| view.state.key).collect();
            for key in keys {
                let _ = runtime.paint(key, at);
                runtime.input(
                    key,
                    Input::Pointer {
                        x: step as f32 * 15.0,
                        y: step as f32 * 15.0,
                    },
                    Modifiers::default(),
                    at,
                );
            }
        }
        // Real skins must fit the stack allowance in every build.
        let errors = runtime.logged_errors();
        let deep = errors
            .iter()
            .find(|error| error.contains("nested too deeply"));
        assert!(deep.is_none(), "{}: {deep:?}", path.display());
        runtime.shut_down();
    }
}

/// A view nested as deep as a file can make it, built, laid out, painted, hit
/// and resized on a small stack.
#[test]
fn deep_elements_fit_a_small_stack() {
    let mut definition = String::from(r#"<THEME><VIEW width="50" height="50" onResize="">"#);
    definition.push_str(
        &r#"<SUBVIEW width="50" height="50" horizontalAlignment="stretch">"#.repeat(100_000),
    );
    let run = move || {
        let mut runtime = start_with(&definition, Vec::new(), playing());
        let key = runtime.views[0].state.key;
        let now = Instant::now();
        let _ = runtime.paint(key, now);
        runtime.input(
            key,
            Input::Pointer { x: 5.0, y: 5.0 },
            Modifiers::default(),
            now,
        );
        runtime.run_script(key, "view.width = 80;", now);
        runtime.tick(playing(), now);
        drop(runtime);
    };
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn a_tiled_edge_is_its_pictures_size_until_the_view_grows() {
    // A frame: the edge's picture fills the gap between its corners exactly
    // at the size the view was drawn for. Running it to the parent's far
    // side would lay a second tile over the right-hand corner.
    let definition = r#"<THEME><VIEW id="main" width="30" height="50">
        <SUBVIEW id="right" left="20" width="10" height="10" zIndex="4"
            horizontalAlignment="right"/>
        <SUBVIEW id="edge" left="5" backgroundImage="tile.png" backgroundTiled="true"
            zIndex="4" horizontalAlignment="stretch" verticalAlignment="stretch"/>
        </VIEW></THEME>"#;
    let mut runtime = start_with(
        definition,
        vec![("tile.png", png(15, 40, |_, _| [9, 9, 9]))],
        playing(),
    );
    let key = key_of(&runtime);
    assert_eq!(runtime.read(key, "edge", "width"), 15.0);
    assert_eq!(runtime.read(key, "edge", "height"), 40.0);
    // Growing keeps both distances to the edges.
    runtime.run_script(key, "view.width = 50; view.height = 70;", Instant::now());
    assert_eq!(runtime.read(key, "edge", "width"), 35.0);
    assert_eq!(runtime.read(key, "edge", "height"), 60.0);
    assert_eq!(runtime.read(key, "right", "left"), 40.0);
}

#[test]
fn a_view_is_told_its_size_once_as_it_opens() {
    let definition = r#"<THEME><VIEW id="main" width="100" height="50" scriptFile="s.js" onResize="fit();">
        <SUBVIEW id="half" width="10" height="5"/></VIEW></THEME>"#;
    let runtime = start(
        definition,
        "function fit(){ half.height = view.height / 2; }",
    );
    assert_eq!(runtime.read(key_of(&runtime), "half", "height"), 25.0);
}

#[test]
fn a_binding_follows_an_element_a_handler_moved() {
    let definition = format!(
        r#"{VIEW}
        <SUBVIEW id="a" top="5" width="10" height="10"/>
        <SUBVIEW id="b" top="wmpprop:a.top" width="10" height="10"/>
        <BUTTON left="50" width="10" height="10" onClick="a.top = 30;"/>
        </VIEW></THEME>"#
    );
    let mut runtime = start(&definition, "");
    let key = key_of(&runtime);
    let now = Instant::now();
    assert_eq!(runtime.read(key, "b", "top"), 5.0);
    click(&mut runtime, 55.0, 5.0, now);
    // The player has not changed: this must not wait for it to.
    assert_eq!(runtime.next_wake(now), Some(Duration::ZERO));
    runtime.tick(playing(), now);
    assert_eq!(runtime.read(key, "b", "top"), 30.0);
}

#[test]
fn a_pass_through_picture_lets_clicks_reach_what_is_under_it() {
    let definition = r#"<THEME><VIEW width="20" height="20">
      <BUTTON zIndex="1" width="20" height="20" onClick="theme.savePreference('hit', 'yes');"/>
      <SUBVIEW zIndex="2" backgroundImage="cover.png" passThrough="true"/>
    </VIEW></THEME>"#;
    let cover = png(20, 20, |_, _| [9, 9, 9]);
    let mut runtime = start_with(definition, vec![("cover.png", cover)], playing());
    let key = key_of(&runtime);
    let now = Instant::now();
    runtime.input(
        key,
        Input::Pointer { x: 5.0, y: 5.0 },
        Modifiers::default(),
        now,
    );
    let press = Input::Press { secondary: false };
    runtime.input(key, press, Modifiers::default(), now);
    let release = Input::Release { secondary: false };
    runtime.input(key, release, Modifiers::default(), now);
    assert_eq!(
        runtime.shared.prefs.get("hit").map(String::as_str),
        Some("yes")
    );
    assert!(runtime.window_commands(key).is_empty(), "no window drag");
}

const CONTROLLED: &str = r#"<THEME currentViewID="control">
  <VIEW id="control" scriptFile="s.js" onLoad="theme.openView('main');" onTimer="act();" timerInterval="100"/>
  <VIEW id="main" width="20" height="20" scriptFile="s.js"/>
</THEME>"#;

#[test]
fn a_view_without_a_window_minimizes_the_skin() {
    let mut runtime = start(
        CONTROLLED,
        "function act(){ view.minimize(); view.timerInterval = 0; }",
    );
    runtime.tick(playing(), Instant::now() + Duration::from_secs(1));
    let key = runtime.views[runtime.primary().unwrap()].state.key;
    assert!(matches!(
        runtime.window_commands(key)[..],
        [WindowCommand::Minimize]
    ));
}

#[test]
fn closing_the_view_that_controls_the_skin_closes_the_skin() {
    let mut runtime = start(CONTROLLED, "function act(){ view.close(); }");
    let commands = runtime.tick(playing(), Instant::now() + Duration::from_secs(1));
    assert!(runtime.views.is_empty());
    assert_eq!(commands, [Command::BackToMain]);
}

#[test]
fn a_view_that_hands_over_as_it_closes_leaves_the_skin_open() {
    let definition = r#"<THEME currentViewID="boot">
      <VIEW id="boot" scriptFile="s.js" onLoad="theme.openView('main'); view.close();"/>
      <VIEW id="main" width="20" height="20" scriptFile="s.js"/>
    </THEME>"#;
    let runtime = start(definition, "");
    assert_eq!(runtime.views.len(), 1);
    assert!(runtime.primary().is_some());
}

/// The strip's frame on screen for a custom slider at `value`.
fn slider_frame(runtime: &mut Runtime, value: f64) -> i32 {
    let key = key_of(runtime);
    let now = Instant::now();
    runtime.run_script(key, &format!("dial.value={value};"), now);
    let (paints, _) = runtime.paint(key, now);
    paints
        .iter()
        .find_map(|paint| match paint {
            Paint::Picture { part, .. } => Some(part.y / 4),
            _ => None,
        })
        .expect("the strip is drawn")
}

#[test]
fn a_custom_slider_drawn_against_its_map_follows_the_pointer() {
    // A mark moving right to left down the strip, on a map bright to the right.
    let strip = png(20, 28, |x, y| {
        let at = 18 - y / 4 * 3;
        if x == at { [255, 60, 60] } else { [10, 10, 10] }
    });
    let map = png(20, 4, |x, _| {
        let gray = (x * 255 / 19) as u8;
        [gray, gray, gray]
    });
    let definition = r#"<THEME title="T"><VIEW id="main" width="100" height="50">
        <CUSTOMSLIDER id="dial" image="strip.png" positionImage="map.png" min="0" max="100"/>
        </VIEW></THEME>"#;
    let mut runtime = start_with(
        definition,
        vec![("strip.png", strip), ("map.png", map)],
        playing(),
    );
    let key = key_of(&runtime);
    let wait = Instant::now();
    let _ = runtime.paint(key, Instant::now());
    while runtime.images.loading() && wait.elapsed() < Duration::from_secs(5) {
        runtime.images.poll();
        std::thread::yield_now();
    }
    assert_eq!(slider_frame(&mut runtime, 0.0), 6);
    assert_eq!(slider_frame(&mut runtime, 100.0), 0);
}

/// Lists which real strips run backwards, for
/// `SPOTIFAST_WMZ_SAMPLES=folder cargo test sample_strips -- --nocapture`.
#[test]
fn sample_strips() {
    let Ok(folder) = std::env::var("SPOTIFAST_WMZ_SAMPLES") else {
        return;
    };
    let mut paths: Vec<_> = std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "wmz"))
        .collect();
    paths.sort();
    for path in paths {
        let Some(wmp) = crate::skin::Skin::load(&path)
            .ok()
            .and_then(|skin| skin.wmp)
        else {
            continue;
        };
        let mut pairs = std::collections::BTreeSet::new();
        for view in &wmp.views {
            view.walk(&mut |node| {
                if let (Some(image), Some(map)) = (node.attr("image"), node.attr("positionimage")) {
                    pairs.insert((file_name(image), file_name(map)));
                }
            });
        }
        for (image, map_name) in pairs {
            let key = picture::ImageKey {
                file: image.clone(),
                ..Default::default()
            };
            let (Some(map), Some(strip)) =
                (wmp.maps.get(&map_name), picture::decode(&wmp.files, &key))
            else {
                continue;
            };
            let (width, height) = (map.width, map.height);
            let across = strip.size.0 > width;
            let count = if across {
                strip.size.0 / width.max(1)
            } else {
                strip.size.1 / height.max(1)
            };
            let reversed = strip.strip_reversed(
                &map_name,
                map,
                picture::Strip {
                    width,
                    height,
                    count,
                    across,
                },
            );
            println!(
                "STRIP\t{}\t{image}\t{count}\t{}",
                path.file_stem().unwrap_or_default().to_string_lossy(),
                if reversed { "REVERSED" } else { "as drawn" }
            );
        }
    }
}

/// A skin once its pictures are in, drawn off screen. Every pixel is
/// `[r, g, b, a]`, and a clear one is what the desktop shows through.
fn drawn(runtime: &mut Runtime) -> crate::skin::Bitmap {
    let key = key_of(runtime);
    let now = Instant::now();
    let mut paints = runtime.paint(key, now).0;
    for _ in 0..2000 {
        if !runtime.images.loading() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
        runtime.images.poll();
        paints = runtime.paint(key, now).0;
    }
    let (width, height) = runtime.views[runtime.primary().unwrap()].state.size();
    let mut canvas = raster::canvas(width as u32, height as u32);
    raster::draw(&paints, &mut canvas);
    canvas
}

const MAGENTA: [u8; 3] = [255, 0, 255];
const RED: [u8; 3] = [200, 10, 10];

/// Pharaoh looks for its other pane with an empty view that closes itself.
/// That is no control view going away: the window it was opened from stays.
#[test]
fn a_helper_view_closing_itself_leaves_the_window_that_opened_it() {
    let definition = r#"<THEME>
      <VIEW width="40" height="30" onLoad="theme.openView('ghost');"/>
      <VIEW id="ghost" width="0" height="0" onLoad="view.close();"/>
      </THEME>"#;
    let runtime = start_with(definition, Vec::new(), playing());
    assert!(!runtime.closed);
    assert_eq!(runtime.views.len(), 1);
    assert_eq!(runtime.views[0].state.size(), (40.0, 30.0));
}

/// WMP shows a view whatever its `visible` says, and gnome's says false.
#[test]
fn a_view_is_shown_even_if_it_says_it_is_not() {
    let definition = r##"<THEME><VIEW width="10" height="10" visible="false">
      <SUBVIEW width="10" height="10" backgroundColor="#c80a0a"/></VIEW></THEME>"##;
    let mut runtime = start_with(definition, Vec::new(), playing());
    let canvas = drawn(&mut runtime);
    assert_eq!(canvas.pixel(5, 5).unwrap()[..3], RED);
    // A script may still hide it.
    let key = key_of(&runtime);
    runtime.run_script(key, "view.visible = false;", Instant::now());
    assert_eq!(drawn(&mut runtime).pixel(5, 5).unwrap()[3], 0);
}

/// Gorillaz tiles a little square of nothing but its clipping colour over
/// the window, and draws the window's real shape in its sub-views.
#[test]
fn a_tiled_clipping_background_does_not_cut_the_window_to_one_tile() {
    let definition = r##"<THEME><VIEW width="40" height="20" clippingColor="#ff00ff"
        backgroundImage="tile.png" backgroundTiled="true">
      <SUBVIEW left="20" top="10" backgroundImage="head.png"/></VIEW></THEME>"##;
    let files = vec![
        ("tile.png", png(5, 5, |_, _| MAGENTA)),
        ("head.png", png(10, 10, |_, _| RED)),
    ];
    let mut runtime = start_with(definition, files, playing());
    let canvas = drawn(&mut runtime);
    assert_eq!(canvas.pixel(25, 15).unwrap()[..3], RED);
    assert_eq!(canvas.pixel(2, 2).unwrap()[3], 0);
}

/// xXx and Wild Well Control name the colour on the group of buttons.
#[test]
fn a_sub_views_transparency_colour_holds_for_what_is_in_it() {
    let definition = r##"<THEME><VIEW width="20" height="10" backgroundColor="none">
      <SUBVIEW width="20" height="10" transparencyColor="#ff00ff">
        <BUTTON image="b.png"/></SUBVIEW></VIEW></THEME>"##;
    let files = vec![(
        "b.png",
        png(20, 10, |x, _| if x < 10 { MAGENTA } else { RED }),
    )];
    let mut runtime = start_with(definition, files, playing());
    let canvas = drawn(&mut runtime);
    assert_eq!(canvas.pixel(5, 5).unwrap()[3], 0);
    assert_eq!(canvas.pixel(15, 5).unwrap()[..3], RED);
}

/// World of Warcraft's strips name no colour at all.
#[test]
fn a_custom_slider_strip_is_cut_out_of_magenta() {
    let definition = r##"<THEME><VIEW width="20" height="10" backgroundColor="none">
      <CUSTOMSLIDER width="20" height="10" image="s.png" min="0" max="1"/></VIEW></THEME>"##;
    let files = vec![(
        "s.png",
        png(40, 10, |x, _| if x % 20 < 10 { MAGENTA } else { RED }),
    )];
    let mut runtime = start_with(definition, files, playing());
    let canvas = drawn(&mut runtime);
    assert_eq!(canvas.pixel(5, 5).unwrap()[3], 0);
    assert_eq!(canvas.pixel(15, 5).unwrap()[..3], RED);
}

/// The Bionic Dot's pane: effects under a picture whose middle is see-through
/// and whose corners are its transparency colour, faded out until wanted.
const PANE: &str = r##"<THEME><VIEW width="30" height="10" backgroundColor="none">
  <SUBVIEW id="mask" width="30" height="10" backgroundImage="m.png"
      transparencyColor="#ff00ff" alphaBlend="ALPHA">
    <EFFECTS zIndex="-15" width="30" height="10" windowed="false"/>
  </SUBVIEW></VIEW></THEME>"##;

fn pane_mask() -> Vec<u8> {
    // Ten pixels of outside, ten of window, ten of rim.
    let image = image::RgbaImage::from_fn(30, 10, |x, _| match x {
        0..10 => image::Rgba([255, 0, 255, 255]),
        10..20 => image::Rgba([0, 0, 0, 0]),
        _ => image::Rgba([200, 10, 10, 255]),
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[test]
fn effects_fade_with_their_sub_view() {
    let definition = PANE.replace("ALPHA", "0");
    let mut runtime = start_with(&definition, vec![("m.png", pane_mask())], playing());
    let canvas = drawn(&mut runtime);
    assert!(
        canvas.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0),
        "nothing is left of a faded pane"
    );
}

#[test]
fn effects_stay_inside_a_pictures_outline_and_show_in_its_window() {
    let definition = PANE.replace("ALPHA", "255");
    let mut runtime = start_with(&definition, vec![("m.png", pane_mask())], playing());
    let canvas = drawn(&mut runtime);
    assert_eq!(canvas.pixel(5, 5).unwrap()[3], 0, "outside the outline");
    assert_eq!(canvas.pixel(15, 9).unwrap()[3], 255, "the window");
    assert_eq!(canvas.pixel(25, 5).unwrap()[..3], RED, "the rim on top");
}

/// A transparent hole cut by colour alone is still a window on what is under
/// it, as Alienware's and ALX's panes are.
#[test]
fn effects_show_through_a_hole_cut_by_a_transparency_colour() {
    let definition = PANE.replace("ALPHA", "255");
    let mask = png(30, 10, |x, _| if x < 20 { MAGENTA } else { RED });
    let mut runtime = start_with(&definition, vec![("m.png", mask)], playing());
    let canvas = drawn(&mut runtime);
    assert_eq!(canvas.pixel(5, 9).unwrap()[3], 255);
    assert_eq!(canvas.pixel(25, 5).unwrap()[..3], RED);
}

/// A windowed pane really was a window: it stays over every picture, but it
/// goes when the sub-view around it fades.
#[test]
fn a_windowed_effects_pane_stays_on_top_until_its_sub_view_fades() {
    let pane = |alpha: u8| {
        format!(
            r##"<THEME><VIEW width="10" height="10" backgroundColor="none">
            <SUBVIEW width="10" height="10" alphaBlend="{alpha}">
              <EFFECTS width="10" height="10" zIndex="-5" windowed="true"/>
              <SUBVIEW width="10" height="10" backgroundColor="#c80a0a"/>
            </SUBVIEW></VIEW></THEME>"##
        )
    };
    let mut shown = start_with(&pane(255), Vec::new(), playing());
    assert_eq!(drawn(&mut shown).pixel(2, 2).unwrap()[..3], [0, 0, 0]);
    let mut faded = start_with(&pane(0), Vec::new(), playing());
    assert_eq!(drawn(&mut faded).pixel(2, 2).unwrap()[3], 0);
}
