//! A skin running: its open views, their scripts, timers and animations.
//!
//! The window's thread drives this between frames. It is safe to run there
//! because a script cannot take long: every entry into the interpreter has a
//! step budget, a handler that keeps using all of it is switched off, and
//! what is queued when a frame's share of time is gone waits for the next.
//! A skin's scripts have bugs, many of them; an error is logged once and the
//! next handler runs.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::script::{Engine, Limits, ScriptError, Value};
use super::Wmp;
use super::deck::{Command, Deck, OPEN_MEDIA_CHANGING};
use super::host::{Bridge, EventInfo, Shared, ViewRequest};
use super::live::{Element, ElementId, Handler, Kind, Motion, ViewState, WindowCommand, text_of};
use super::paint::{self, Measure, Paint, Painter};
use super::picture::Images;

/// How long scripts may run in one frame.
const FRAME_SHARE: Duration = Duration::from_millis(8);
/// A handler stopped this many times for running too long no longer runs.
const MAX_OVERRUNS: u32 = 3;
/// The most views a skin may have open, against scripts that open forever.
const MAX_VIEWS: usize = 32;
/// Two clicks this close apart are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Pointer {
        x: f32,
        y: f32,
    },
    Left,
    Press {
        secondary: bool,
    },
    Release {
        secondary: bool,
    },
    Wheel(f32),
    Key {
        code: u32,
        pressed: bool,
    },
    Character(char),
    /// The user dragged the window to this size in skin pixels.
    Resized(f32, f32),
}

/// Modifier keys as an input came with them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

pub struct LiveView {
    pub state: ViewState,
    engine: Engine,
    overruns: HashMap<Arc<str>, u32>,
    disabled: HashSet<Arc<str>>,
}

pub struct Runtime {
    /// The name the settings hold for this skin, for its preferences.
    pub name: String,
    pub wmp: Arc<Wmp>,
    pub views: Vec<LiveView>,
    pub shared: Shared,
    pub images: Images,
    next_key: u32,
    last_deck: Option<Deck>,
    /// Where a volume drag has got to.
    dragged_volume: Option<u8>,
    /// Errors already logged.
    logged: HashSet<String>,
    /// The whole skin closed.
    pub closed: bool,
}

impl Runtime {
    /// Starts a skin: opens the view it starts with, runs its scripts.
    pub fn new(
        name: String,
        wmp: Arc<Wmp>,
        deck: Deck,
        prefs: HashMap<String, String>,
        measure: Arc<Measure>,
        arrived: impl Fn() + Send + 'static,
    ) -> Self {
        let now = Instant::now();
        let images = Images::new(wmp.files.clone(), arrived);
        let shared = Shared {
            deck: deck.clone(),
            prefs,
            prefs_changed: false,
            commands: Vec::new(),
            requests: Vec::new(),
            event: EventInfo::default(),
            mediacenter: HashMap::new(),
            measure,
            title: wmp.title.clone(),
            author: wmp.author.clone(),
            current_view: String::new(),
        };
        let mut runtime = Self {
            name,
            wmp,
            views: Vec::new(),
            shared,
            images,
            next_key: 0,
            last_deck: Some(deck),
            dragged_volume: None,
            logged: HashSet::new(),
            closed: false,
        };
        let start = runtime.wmp.start_view();
        runtime.open(start, now);
        runtime.settle(now);
        runtime
    }

    fn report(&mut self, source: &str, error: &ScriptError) {
        let line = format!("skin {}: {error} in {:.60}", self.wmp.title, source);
        if self.logged.len() < 256 && self.logged.insert(line.clone()) {
            log::debug!("{line}");
        }
    }

    /// The first view with a window: the root window draws it.
    pub fn primary(&self) -> Option<usize> {
        self.views
            .iter()
            .position(|view| view.state.windowed() && !view.state.closing)
    }

    pub fn view(&self, key: u32) -> Option<&LiveView> {
        self.views.iter().find(|view| view.state.key == key)
    }

    fn index_of(&self, key: u32) -> Option<usize> {
        self.views.iter().position(|view| view.state.key == key)
    }

    fn open(&mut self, definition: usize, now: Instant) -> Option<usize> {
        if self.views.len() >= MAX_VIEWS {
            return None;
        }
        // Each view runs by itself, with its own global names.
        let key = self.next_key;
        self.next_key += 1;
        let state = ViewState::new(self.wmp.clone(), key, definition, now);
        let mut live = LiveView {
            state,
            engine: Engine::new(Limits::default()),
            overruns: HashMap::new(),
            disabled: HashSet::new(),
        };
        let node = self.wmp.views.get(definition)?.clone();
        // The file named like the definition stands in when none is named.
        let mut names: Vec<String> = node
            .first_attr("scriptfile")
            .unwrap_or_default()
            .split(';')
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty() && !name.to_ascii_lowercase().starts_with("res:"))
            .collect();
        if node.first_attr("scriptfile").is_none() {
            names.push(format!("{}.js", self.wmp.stem));
        }
        for name in names {
            let Some(source) = self.wmp.script(&name) else {
                continue;
            };
            let mut bridge = Bridge {
                view: &mut live.state,
                shared: &mut self.shared,
            };
            if let Err(error) = live.engine.load(&source, &mut bridge) {
                self.report(&name, &error);
            }
        }
        live.state.settle_sizes();
        self.views.push(live);
        let index = self.views.len() - 1;
        // Worked out once, in order, now that the scripts are in.
        let deferred = std::mem::take(&mut self.views[index].state.deferred);
        for (element, attr, source) in deferred {
            if let Some(value) = self.evaluate(index, Element::host(element), &source) {
                self.views[index].state.store(element, &attr, value);
            }
            self.views[index].state.size_from_picture(element);
        }
        self.views[index].state.settle_sizes();
        self.views[index].state.settle_view_size();
        self.refresh_bindings(index);
        self.views[index].state.settle_sizes();
        self.views[index].state.settle_view_size();
        self.views[index].state.settle_centres();
        // Bindings may read what was just placed.
        self.refresh_bindings(index);
        self.schedule_timer(index, now);
        let root = self.views[index].state.root;
        self.views[index].state.fire(root, "onload", Vec::new());
        // Layout done in `onResize` happens once as the view first shows.
        self.views[index].state.fire(root, "onresize", Vec::new());
        self.pump(index);
        Some(index)
    }

    fn evaluate(&mut self, index: usize, scope: u64, source: &Arc<str>) -> Option<Value> {
        let live = &mut self.views[index];
        let code = match live.engine.compile(source) {
            Ok(code) => code,
            Err(error) => {
                let source = source.clone();
                self.report(&source, &error);
                return None;
            }
        };
        let mut bridge = Bridge {
            view: &mut live.state,
            shared: &mut self.shared,
        };
        match live.engine.run(code, Some(scope), &[], &mut bridge) {
            Ok(value) => Some(value),
            Err(error) => {
                let source = source.clone();
                self.report(&source, &error);
                None
            }
        }
    }

    /// Reads every `wmpprop:` again and applies what changed.
    fn refresh_bindings(&mut self, index: usize) {
        for at in 0..self.views[index].state.bindings.len() {
            let binding = &self.views[index].state.bindings[at];
            let (element, source, negate) = (binding.element, binding.code.clone(), binding.negate);
            let attr = binding.attr.clone();
            // The user's hand outranks the player's report.
            if self.views[index].state.dragging == Some(element) && attr == "value" {
                continue;
            }
            let Some(value) = self.evaluate(index, Element::host(element), &source) else {
                continue;
            };
            let value = if negate {
                Value::Bool(!crate::skin::wmp::live::truthy(&value))
            } else {
                value
            };
            // Undefined means the skin bound to something that does not
            // exist; what it set by hand stands.
            if value != Value::Undefined {
                self.views[index].state.set(element, &attr, value);
            }
        }
    }

    fn schedule_timer(&mut self, index: usize, now: Instant) {
        let state = &mut self.views[index].state;
        let root = &state.elements[state.root];
        let interval = root.num("timerinterval", 1000.0);
        state.timer_due = (root.handlers.contains_key("ontimer") && interval >= 50.0)
            .then(|| now + Duration::from_millis(interval as u64));
    }

    /// Runs what is queued for a view, until it is empty or the frame's
    /// share is gone.
    fn pump(&mut self, index: usize) {
        let started = Instant::now();
        while let Some(queued) = self.views[index].state.queue.pop_front() {
            self.run_one(index, queued);
            self.collect_if_full(index);
            if started.elapsed() > FRAME_SHARE {
                break;
            }
        }
    }

    /// Frees what scripts no longer reach, between handlers.
    fn collect_if_full(&mut self, index: usize) {
        let live = &mut self.views[index];
        if live.engine.heap_bytes() > 4 * 1024 * 1024 {
            let roots: Vec<Value> = live
                .state
                .elements
                .iter()
                .flat_map(|element| {
                    element
                        .props
                        .values()
                        .cloned()
                        .chain(
                            element
                                .handlers
                                .values()
                                .filter_map(|handler| match handler {
                                    Handler::Function(value) => Some(value.clone()),
                                    Handler::Source(_) => None,
                                }),
                        )
                })
                .collect();
            live.engine.collect(&roots);
        }
    }

    fn run_one(&mut self, index: usize, queued: super::live::Queued) {
        let live = &mut self.views[index];
        let args: Vec<(&str, Value)> = queued
            .args
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect();
        let mut bridge = Bridge {
            view: &mut live.state,
            shared: &mut self.shared,
        };
        let (source, outcome) = match queued.handler {
            Handler::Function(function) => {
                let values: Vec<Value> = args.into_iter().map(|(_, value)| value).collect();
                let outcome = live.engine.call_value(&function, &values, &mut bridge);
                let name: Arc<str> = "a function".into();
                (name, outcome)
            }
            Handler::Source(source) => {
                if live.disabled.contains(&source) {
                    return;
                }
                let outcome = live.engine.compile(&source).and_then(|code| {
                    live.engine
                        .run(code, Some(queued.scope), &args, &mut bridge)
                });
                (source, outcome)
            }
        };
        if let Err(error) = outcome {
            if matches!(
                error,
                ScriptError::Budget | ScriptError::Memory | ScriptError::Depth
            ) {
                let overruns = live.overruns.entry(source.clone()).or_insert(0);
                *overruns += 1;
                if *overruns >= MAX_OVERRUNS {
                    log::warn!("skin handler switched off after overrunning: {source:.60}");
                    live.disabled.insert(source.clone());
                }
            }
            self.report(&source, &error);
        }
    }

    /// Carries out what scripts asked of the views. Opening a view can
    /// run scripts that ask for more, so this repeats; a skin that keeps
    /// asking stops at the limit on open views.
    fn settle(&mut self, now: Instant) {
        for _ in 0..MAX_VIEWS * 2 {
            let requests = std::mem::take(&mut self.shared.requests);
            if requests.is_empty() {
                break;
            }
            let mut opened = false;
            for request in requests {
                match request {
                    ViewRequest::Open(name) => {
                        if self.find_named(&name).is_none()
                            && let Some(definition) = self.wmp.view_named(&name)
                        {
                            self.open(definition, now);
                            opened = true;
                        }
                    }
                    ViewRequest::CloseNamed(name) => {
                        if let Some(index) = self.find_named(&name) {
                            self.close_asked(index, opened);
                        }
                    }
                    ViewRequest::Close(key) => {
                        if let Some(index) = self.index_of(key) {
                            self.close_asked(index, opened);
                        }
                    }
                    ViewRequest::Switch(key, name) => {
                        let Some(definition) = self.wmp.view_named(&name) else {
                            continue;
                        };
                        if let Some(index) = self.index_of(key) {
                            self.close(index);
                        }
                        self.open(definition, now);
                    }
                }
            }
        }
        // A control view runs on without a window of its own only while
        // there is one to control.
        if self.primary().is_none() {
            self.finish();
        }
    }

    fn finish(&mut self) {
        if !self.closed {
            self.closed = true;
            self.shared.commands.push(Command::BackToMain);
        }
    }

    fn find_named(&self, name: &str) -> Option<usize> {
        self.views
            .iter()
            .position(|view| view.state.id.eq_ignore_ascii_case(name))
    }

    /// A script's close. A view with no window that came first is what runs
    /// the skin's other views, so they go with it, unless it just opened them
    /// and is leaving. One opened by a window is that window's helper: skins
    /// probe with an empty view that closes itself.
    fn close_asked(&mut self, index: usize, handing_over: bool) {
        let control = !self.views[..=index]
            .iter()
            .any(|view| view.state.windowed());
        self.close(index);
        if control && !handing_over {
            while !self.views.is_empty() {
                self.close(self.views.len() - 1);
            }
        }
    }

    fn close(&mut self, index: usize) {
        let root = self.views[index].state.root;
        self.views[index].state.closing = true;
        self.views[index].state.fire(root, "onclose", Vec::new());
        self.pump(index);
        self.views.remove(index);
    }

    /// Closes every view as the skin comes off, so each can save its state.
    pub fn shut_down(&mut self) {
        while !self.views.is_empty() {
            self.close(self.views.len() - 1);
        }
        self.shared.requests.clear();
        self.closed = true;
    }

    fn player_event(&mut self, index: usize, name: &str, args: &[(&str, Value)]) {
        let state = &mut self.views[index].state;
        for player in state.players.clone() {
            if let Some(handler) = state.elements[player].handlers.get(name).cloned() {
                let args = args
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), value.clone()))
                    .collect();
                // Bare names in these handlers are the player's.
                state.queue_handler(1, handler, args);
            }
        }
    }

    /// Tells every view what changed in the player since last time.
    fn deck_changed(&mut self, old: &Deck, new: &Deck) {
        let new_state = Value::Number(f64::from(new.state as i32));
        for index in 0..self.views.len() {
            if old.song != new.song {
                let open = |state: i32| Value::Number(f64::from(state));
                self.player_event(
                    index,
                    "openstatechange",
                    &[("NewState", open(OPEN_MEDIA_CHANGING))],
                );
                self.player_event(index, "openstate_onchange", &[]);
                self.player_event(
                    index,
                    "openstatechange",
                    &[("NewState", open(new.open_state()))],
                );
                self.player_event(index, "openstate_onchange", &[]);
                self.player_event(index, "mediachange", &[("Item", Value::Host(4))]);
                self.player_event(
                    index,
                    "currentitemchange",
                    &[("pdispMedia", Value::Host(4))],
                );
                self.player_event(index, "currentmedia_onchange", &[]);
                self.player_event(index, "currentplaylist_onchange", &[]);
            }
            if old.state != new.state {
                self.player_event(index, "playstatechange", &[("NewState", new_state.clone())]);
                self.player_event(index, "playstate_onchange", &[]);
            }
            if old.status() != new.status() {
                self.player_event(index, "statuschange", &[]);
                self.player_event(index, "status_onchange", &[]);
            }
            if old.shuffle != new.shuffle {
                self.player_event(
                    index,
                    "modechange",
                    &[
                        ("ModeName", Value::Str("shuffle".into())),
                        ("NewValue", Value::Bool(new.shuffle)),
                    ],
                );
            }
            if old.repeat != new.repeat {
                self.player_event(
                    index,
                    "modechange",
                    &[
                        ("ModeName", Value::Str("loop".into())),
                        ("NewValue", Value::Bool(new.repeat)),
                    ],
                );
            }
            if old.queue != new.queue {
                self.player_event(index, "playlistchange", &[("change", Value::Number(0.0))]);
                self.player_event(
                    index,
                    "currentplaylistchange",
                    &[("change", Value::Number(0.0))],
                );
            }
        }
    }

    /// Advances the skin to `now` with the player as `deck` shows it, and
    /// returns what it asks of the app.
    pub fn tick(&mut self, deck: Deck, now: Instant) -> Vec<Command> {
        self.images.poll();
        // What a script just set stands until the app has seen the command.
        let waiting = !self.shared.commands.is_empty();
        if !waiting {
            let old = self.last_deck.take().unwrap_or_else(|| deck.clone());
            // The position moves all the time; bindings only need it often
            // enough for a clock and a seek bar.
            let moved = (old.position_s - deck.position_s).abs() >= 0.25;
            let same = Deck {
                position_s: deck.position_s,
                ..old.clone()
            } == deck;
            self.shared.deck = deck.clone();
            if same {
                self.last_deck = Some(if moved { deck } else { old });
                if moved {
                    for index in 0..self.views.len() {
                        self.player_event(index, "currentposition_onchange", &[]);
                        self.refresh_bindings(index);
                    }
                }
            } else {
                self.deck_changed(&old, &deck);
                self.last_deck = Some(deck);
                for index in 0..self.views.len() {
                    self.refresh_bindings(index);
                }
            }
        }
        for index in 0..self.views.len() {
            self.views[index].state.now = now;
            self.advance_motions(index, now);
            let due = self.views[index]
                .state
                .timer_due
                .is_some_and(|due| now >= due);
            if due {
                let root = self.views[index].state.root;
                self.views[index].state.fire(root, "ontimer", Vec::new());
                self.schedule_timer(index, now);
            }
            if !self.views[index].state.queue.is_empty() {
                self.pump(index);
                self.views[index].state.bindings_stale = true;
            } else if self.views[index].state.timer_due.is_none() {
                self.schedule_timer(index, now);
            }
            // A binding may read any element; what it sets may start
            // another round, which ends when nothing changes.
            if std::mem::take(&mut self.views[index].state.bindings_stale) {
                self.refresh_bindings(index);
            }
        }
        self.settle(now);
        let mut commands = std::mem::take(&mut self.shared.commands);
        self.pace_volume(&mut commands);
        commands
    }

    /// A slider writes the volume at every step. While the hand is on it
    /// those are previews, as on the app's own sliders, and the last one
    /// settles on release. Scripts outside a drag set the volume outright.
    fn pace_volume(&mut self, commands: &mut Vec<Command>) {
        let dragging = self.views.iter().any(|view| view.state.dragging.is_some());
        let mut settled = false;
        for command in commands.iter_mut() {
            if let Command::SetVolume(volume) = *command {
                if dragging {
                    *command = Command::PreviewVolume(volume);
                    self.dragged_volume = Some(volume);
                } else {
                    settled = true;
                }
            }
        }
        if dragging {
            return;
        }
        if let Some(volume) = self.dragged_volume.take()
            && !settled
        {
            commands.push(Command::SetVolume(volume));
        }
    }

    fn advance_motions(&mut self, index: usize, now: Instant) {
        let state = &mut self.views[index].state;
        if state.motions.is_empty() {
            return;
        }
        state.changed = true;
        let motions = std::mem::take(&mut state.motions);
        for motion in motions {
            let elapsed = now.saturating_duration_since(motion.start).as_secs_f64() * 1000.0;
            let mut share = if motion.ms <= 0.0 {
                1.0
            } else {
                (elapsed / motion.ms).min(1.0)
            };
            if motion.eased {
                share = share * share * (3.0 - 2.0 * share);
            }
            let at =
                |slot: usize| motion.from[slot] + (motion.to[slot] - motion.from[slot]) * share;
            let id = motion.element;
            if motion.fade {
                state.elements[id].alpha = at(4);
            } else {
                let (old_width, old_height) = (state.elements[id].width, state.elements[id].height);
                state.elements[id].left = at(0);
                state.elements[id].top = at(1);
                state.elements[id].width = at(2);
                state.elements[id].height = at(3);
                let (width, height) = (state.elements[id].width, state.elements[id].height);
                if width != old_width || height != old_height {
                    state.resized(id, width - old_width, height - old_height);
                }
            }
            if share >= 1.0 {
                state.fire(
                    id,
                    if motion.fade {
                        "onendalphablend"
                    } else {
                        "onendmove"
                    },
                    Vec::new(),
                );
            } else {
                state.motions.push(motion);
            }
        }
    }

    /// Tells the skin its window is not up yet, as `view.onScreen`, so that
    /// an opening can wait for it. A preview has no window and never says so.
    pub fn await_window(&mut self) {
        if let Some(at) = self.primary() {
            let state = &mut self.views[at].state;
            let root = state.root;
            state.store(root, "onscreen", Value::Bool(false));
        }
    }

    /// The window has the view's size and something of it has been drawn.
    /// It counts once the pictures that drawing asked for are in.
    pub fn window_ready(&mut self, key: u32) {
        if self.images.loading() {
            return;
        }
        if let Some(index) = self.index_of(key) {
            let state = &mut self.views[index].state;
            let root = state.root;
            if state.elements[root].props.contains_key("onscreen") {
                state.store(root, "onscreen", Value::Bool(true));
            }
        }
    }

    /// When the skin next wants a frame on its own, if ever.
    pub fn next_wake(&self, now: Instant) -> Option<Duration> {
        let mut wake: Option<Duration> = None;
        let mut soonest = |after: Duration| {
            wake = Some(wake.map_or(after, |wake| wake.min(after)));
        };
        for live in &self.views {
            let state = &live.state;
            if !state.motions.is_empty() {
                soonest(Duration::from_millis(16));
            }
            if !state.queue.is_empty() || state.bindings_stale {
                soonest(Duration::ZERO);
            }
            if let Some(due) = state.timer_due {
                soonest(due.saturating_duration_since(now));
            }
        }
        if self.images.loading() {
            soonest(Duration::from_millis(50));
        }
        // Clocks and seek bars; a skin with neither has no need of frames.
        if self.shared.deck.state == super::deck::PlayState::Playing
            && self.views.iter().any(|view| {
                view.state
                    .bindings
                    .iter()
                    .any(|binding| binding.code.to_ascii_lowercase().contains("position"))
            })
        {
            soonest(Duration::from_millis(500));
        }
        wake
    }

    /// What to draw for a view now, and when it next changes by itself.
    pub fn paint(&mut self, key: u32, now: Instant) -> (Vec<Paint>, Option<u64>) {
        let Some(index) = self.index_of(key) else {
            return (Vec::new(), None);
        };
        let measure = self.shared.measure.clone();
        let mut painter = Painter {
            view: &mut self.views[index].state,
            images: &mut self.images,
            deck: &self.shared.deck,
            measure: &*measure,
            now,
            out: Vec::new(),
            wake_ms: None,
            on_top: Vec::new(),
        };
        painter.run();
        let wake = painter.wake_ms;
        let out = painter.out;
        self.views[index].state.changed = false;
        (out, wake)
    }

    #[cfg(test)]
    pub fn logged_errors(&self) -> Vec<String> {
        self.logged.iter().cloned().collect()
    }

    /// Takes what a view asked of its window.
    /// The primary view also carries out what views with no window asked.
    pub fn window_commands(&mut self, key: u32) -> Vec<WindowCommand> {
        let Some(index) = self.index_of(key) else {
            return Vec::new();
        };
        let mut commands = std::mem::take(&mut self.views[index].state.window);
        if self.primary() == Some(index) {
            for view in &mut self.views {
                if !view.state.windowed() {
                    commands.append(&mut view.state.window);
                }
            }
        }
        commands
    }

    /// The tooltip and cursor of what is under the pointer.
    pub fn hover_info(&self, key: u32) -> (Option<String>, Option<String>) {
        let Some(view) = self.view(key) else {
            return (None, None);
        };
        let Some(element) = view.state.hover.and_then(|id| view.state.elements.get(id)) else {
            return (None, None);
        };
        let tip = match element.kind {
            Kind::Button | Kind::ButtonElement => {
                let down = element.flag("down", false);
                let wanted = if down { "downtooltip" } else { "uptooltip" };
                element.str(wanted).or_else(|| element.str("uptooltip"))
            }
            _ => element.str("tooltip"),
        }
        .filter(|tip| !tip.is_empty() && element.enabled);
        let cursor = element.str("cursor").or_else(|| {
            matches!(element.kind, Kind::Slider | Kind::CustomSlider).then(|| "hand".to_owned())
        });
        (tip, cursor)
    }

    /// Takes in one thing the user did to a view. Handlers run at once;
    /// what they ask of the player comes out of the next [`tick`].
    ///
    /// [`tick`]: Self::tick
    pub fn input(&mut self, key: u32, input: Input, modifiers: Modifiers, now: Instant) {
        let Some(index) = self.index_of(key) else {
            return;
        };
        self.views[index].state.now = now;
        self.shared.event.shift = modifiers.shift;
        self.shared.event.ctrl = modifiers.ctrl;
        self.shared.event.alt = modifiers.alt;
        match input {
            Input::Pointer { x, y } => self.pointer_moved(index, f64::from(x), f64::from(y)),
            Input::Left => {
                if self.views[index].state.dragging.is_none() {
                    self.hover(index, None);
                }
                self.views[index].state.pointer = (-1.0, -1.0);
            }
            Input::Press { secondary } => self.press(index, secondary, now),
            Input::Release { secondary } => self.release(index, secondary, now),
            Input::Wheel(lines) => self.wheel(index, lines),
            Input::Key { code, pressed } => self.key(index, code, pressed),
            Input::Character(character) => self.typed(index, character),
            Input::Resized(width, height) => {
                let root = self.views[index].state.root;
                let state = &mut self.views[index].state;
                state.set(root, "width", Value::Number(f64::from(width)));
                state.set(root, "height", Value::Number(f64::from(height)));
            }
        }
        self.pump(index);
        self.settle(now);
    }

    fn event_at(&mut self, index: usize, id: ElementId) {
        let (x, y) = self.views[index].state.pointer;
        let area = self.views[index].state.world_box(id);
        let event = &mut self.shared.event;
        event.x = f64::from(x);
        event.y = f64::from(y);
        event.offset_x = f64::from(x) - f64::from(area.x);
        event.offset_y = f64::from(y) - f64::from(area.y);
        event.source = Some(id);
    }

    /// Whether `inner` is `outer` or inside it.
    fn within(state: &ViewState, inner: ElementId, outer: ElementId) -> bool {
        let mut at = Some(inner);
        while let Some(current) = at {
            if current == outer {
                return true;
            }
            at = state.elements[current].parent;
        }
        false
    }

    /// Moving onto an element's child does not leave the element: a view
    /// that fades on `onmouseout` must not do it over its own buttons.
    fn hover(&mut self, index: usize, target: Option<ElementId>) {
        let old = self.views[index].state.hover;
        if old == target {
            return;
        }
        let state = &mut self.views[index].state;
        state.hover = target;
        state.changed = true;
        let mut leaving = old;
        while let Some(id) = leaving {
            if target.is_some_and(|target| Self::within(state, target, id)) {
                break;
            }
            state.fire(id, "onmouseout", Vec::new());
            leaving = state.elements[id].parent;
        }
        let mut entering = target;
        while let Some(id) = entering {
            if old.is_some_and(|old| Self::within(state, old, id)) {
                break;
            }
            state.fire(id, "onmouseover", Vec::new());
            entering = state.elements[id].parent;
        }
    }

    fn pointer_moved(&mut self, index: usize, x: f64, y: f64) {
        self.views[index].state.pointer = (x as f32, y as f32);
        if let Some(id) = self.views[index].state.dragging {
            self.drag_to(index, id, x, y);
            return;
        }
        let target = paint::hit(&self.views[index].state, &mut self.images, x, y);
        self.hover(index, target);
        if let Some(target) = target
            && self.views[index].state.elements[target].enabled
        {
            self.event_at(index, target);
            self.views[index]
                .state
                .fire(target, "onmousemove", Vec::new());
        }
    }

    fn drag_to(&mut self, index: usize, id: ElementId, x: f64, y: f64) {
        let state = &mut self.views[index].state;
        let area = state.world_box(id);
        let element = &state.elements[id];
        let value = if element.kind == Kind::CustomSlider {
            let Some(map) = element
                .image_name("positionimage")
                .and_then(|name| state.wmp.maps.get(&super::file_name(&name)).cloned())
            else {
                return;
            };
            let Some(pixel) = map.pixel(
                (x - f64::from(area.x)).max(0.0) as u32,
                (y - f64::from(area.y)).max(0.0) as u32,
            ) else {
                return;
            };
            if pixel[0] != pixel[1] || pixel[1] != pixel[2] {
                return;
            }
            let (min, max) = (element.num("min", 0.0), element.num("max", 100.0));
            min + f64::from(pixel[0]) / 255.0 * (max - min)
        } else {
            let vertical = paint::slider_vertical(element);
            let border = element.num("bordersize", 0.0).max(0.0) as i32;
            let (at, length) = if vertical {
                (y - f64::from(area.y), area.height)
            } else {
                (x - f64::from(area.x), area.width)
            };
            paint::slider_value_at(element, at, length, border)
        };
        state.set(id, "value", Value::Number(value));
        state.fire(id, "onpositionchange", Vec::new());
    }

    fn press(&mut self, index: usize, secondary: bool, now: Instant) {
        self.shared.event.button = if secondary { 2.0 } else { 1.0 };
        let (x, y) = self.views[index].state.pointer;
        let (x, y) = (f64::from(x), f64::from(y));
        let target = paint::hit(&self.views[index].state, &mut self.images, x, y);
        self.hover(index, target);
        // A click anywhere away from an open popup closes it.
        for id in 0..self.views[index].state.elements.len() {
            let element = &mut self.views[index].state.elements[id];
            if element.popped && target != Some(id) {
                element.popped = false;
                element.visible = false;
                self.views[index].state.changed = true;
            }
        }
        let Some(target) = target else {
            if !secondary {
                self.views[index].state.window.push(WindowCommand::Drag);
            }
            return;
        };
        let element = &self.views[index].state.elements[target];
        if !element.enabled {
            return;
        }
        let behaves = element.behaves();
        let kind = element.kind;
        self.event_at(index, target);
        self.views[index]
            .state
            .fire(target, "onmousedown", Vec::new());
        if secondary {
            return;
        }
        let state = &mut self.views[index].state;
        let was_focus = state.focus;
        if matches!(kind, Kind::EditBox | Kind::ListBox | Kind::Playlist) {
            state.focus = Some(target);
        } else {
            state.focus = None;
        }
        if state.focus != was_focus {
            if let Some(old) = was_focus {
                state.fire(old, "onblur", Vec::new());
            }
            if state.focus.is_some() {
                state.fire(target, "onfocus", Vec::new());
            }
            state.changed = true;
        }
        match kind {
            Kind::Slider | Kind::CustomSlider => {
                state.dragging = Some(target);
                state.fire(target, "ondragbegin", Vec::new());
                self.drag_to(index, target, x, y);
            }
            Kind::Playlist | Kind::ListBox => self.list_press(index, target, y, now),
            _ if behaves => {
                state.pressed = Some(target);
                state.changed = true;
            }
            _ => state.window.push(WindowCommand::Drag),
        }
    }

    fn list_press(&mut self, index: usize, id: ElementId, y: f64, now: Instant) {
        let deck = self.shared.deck.clone();
        let state = &mut self.views[index].state;
        let area = state.world_box(id);
        let row = paint::list_row_at(state, id, y - f64::from(area.y));
        let count = paint::list_rows(state, &deck, id).len();
        if row >= count {
            return;
        }
        let double = state
            .last_click
            .is_some_and(|(element, at)| element == id && now - at < DOUBLE_CLICK)
            && state.elements[id].selected == Some(row);
        state.last_click = Some((id, now));
        state.elements[id].selected = Some(row);
        state.changed = true;
        state.notify(id, "selecteditem");
        state.fire(id, "onclick", Vec::new());
        let element = &mut state.elements[id];
        if element.popped {
            element.popped = false;
            element.visible = false;
        } else if double {
            state.fire(id, "ondblclick", Vec::new());
            if state.elements[id].kind == Kind::Playlist {
                self.shared.commands.push(Command::PlayQueued(row));
            }
        }
    }

    fn release(&mut self, index: usize, secondary: bool, now: Instant) {
        let (x, y) = self.views[index].state.pointer;
        let target = paint::hit(
            &self.views[index].state,
            &mut self.images,
            f64::from(x),
            f64::from(y),
        );
        let state = &mut self.views[index].state;
        state.resizing = false;
        if let Some(id) = target {
            state.fire(id, "onmouseup", Vec::new());
        }
        if secondary {
            return;
        }
        if let Some(id) = state.dragging.take() {
            state.changed = true;
            state.fire(id, "ondragend", Vec::new());
            return;
        }
        let Some(id) = state.pressed.take() else {
            return;
        };
        state.changed = true;
        if target != Some(id) {
            return;
        }
        if state.elements[id].flag("sticky", false) {
            let down = !state.elements[id].flag("down", false);
            state.set(id, "down", Value::Bool(down));
        }
        let double = state
            .last_click
            .is_some_and(|(element, at)| element == id && now - at < DOUBLE_CLICK);
        state.last_click = Some((id, now));
        state.fire(id, "onclick", Vec::new());
        if double {
            state.fire(id, "ondblclick", Vec::new());
        }
    }

    fn wheel(&mut self, index: usize, lines: f32) {
        let deck_length = self.shared.deck.queue.len();
        let state = &mut self.views[index].state;
        let Some(id) = state.hover else {
            return;
        };
        let element = &mut state.elements[id];
        match element.kind {
            Kind::Playlist | Kind::ListBox => {
                let count = if element.kind == Kind::Playlist {
                    deck_length
                } else {
                    element.items.len()
                };
                let last = count.saturating_sub(1) as f64;
                element.scroll = (element.scroll - f64::from(lines) * 3.0)
                    .clamp(0.0, last)
                    .round();
                state.changed = true;
            }
            _ => {}
        }
    }

    fn key(&mut self, index: usize, code: u32, pressed: bool) {
        self.shared.event.key_code = f64::from(code);
        let state = &mut self.views[index].state;
        let target = state.focus.unwrap_or(state.root);
        let name = if pressed { "onkeydown" } else { "onkeyup" };
        state.fire(target, name, Vec::new());
        if pressed
            && code == 8
            && let Some(element) = state.focus.and_then(|id| state.elements.get(id))
            && element.kind == Kind::EditBox
            && !element.flag("readonly", false)
        {
            let id = state.focus.unwrap_or(target);
            let mut text = element.props.get("value").map(text_of).unwrap_or_default();
            text.pop();
            state.set(id, "value", Value::Str(text.as_str().into()));
        }
    }

    fn typed(&mut self, index: usize, character: char) {
        self.shared.event.key_code = f64::from(u32::from(character));
        let state = &mut self.views[index].state;
        let target = state.focus.unwrap_or(state.root);
        state.fire(target, "onkeypress", Vec::new());
        let Some(id) = state.focus else {
            return;
        };
        let element = &state.elements[id];
        if element.kind != Kind::EditBox || element.flag("readonly", false) {
            return;
        }
        let limit = element.num("textlimit", 0.0);
        let mut text = element.props.get("value").map(text_of).unwrap_or_default();
        if (limit > 0.0 && text.chars().count() as f64 >= limit) || text.len() >= 4096 {
            return;
        }
        match element
            .str("editstyle")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "uppercase" => text.extend(character.to_uppercase()),
            "lowercase" => text.extend(character.to_lowercase()),
            "number" if !character.is_ascii_digit() => return,
            _ => text.push(character),
        }
        state.set(id, "value", Value::Str(text.as_str().into()));
    }

    /// Runs script text in a view, for the demo's captures.
    #[cfg(any(test, feature = "demo"))]
    pub fn run_script(&mut self, key: u32, source: &str, now: Instant) {
        if let Some(index) = self.index_of(key) {
            let root = self.views[index].state.root;
            let source: Arc<str> = source.into();
            self.views[index].state.queue_handler(
                Element::host(root),
                Handler::Source(source),
                Vec::new(),
            );
            self.pump(index);
            self.settle(now);
        }
    }

    /// The numbers a test or demo wants from a view: an element's attribute.
    #[cfg(any(test, feature = "demo"))]
    pub fn read(&self, key: u32, id: &str, name: &str) -> f64 {
        self.view(key).map_or(f64::NAN, |view| {
            view.state
                .ids
                .get(&id.to_ascii_lowercase())
                .map_or(f64::NAN, |at| {
                    let element = &view.state.elements[*at];
                    match name {
                        "left" => element.left,
                        "top" => element.top,
                        "width" => element.width,
                        "height" => element.height,
                        "visible" => f64::from(u8::from(element.visible)),
                        other => element
                            .props
                            .get(other)
                            .map_or(f64::NAN, super::live::number_of),
                    }
                })
        })
    }
}

impl ViewState {
    pub fn start_move(
        &mut self,
        id: ElementId,
        to: (f64, f64),
        size: Option<(f64, f64)>,
        ms: f64,
        eased: bool,
    ) {
        let element = &self.elements[id];
        let (width, height) = size.unwrap_or((element.width, element.height));
        let motion = Motion {
            element: id,
            start: self.now,
            ms: ms.max(0.0),
            from: [
                element.left,
                element.top,
                element.width,
                element.height,
                0.0,
            ],
            to: [to.0, to.1, width, height, 0.0],
            eased,
            fade: false,
        };
        self.motions.retain(|old| old.element != id || old.fade);
        self.motions.push(motion);
    }

    pub fn start_fade(&mut self, id: ElementId, to: f64, ms: f64) {
        let element = &self.elements[id];
        let motion = Motion {
            element: id,
            start: self.now,
            ms: ms.max(0.0),
            from: [0.0, 0.0, 0.0, 0.0, element.alpha],
            to: [0.0, 0.0, 0.0, 0.0, to.clamp(0.0, 255.0)],
            eased: false,
            fade: true,
        };
        self.motions.retain(|old| old.element != id || !old.fade);
        self.motions.push(motion);
    }
}
