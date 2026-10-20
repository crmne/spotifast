//! Keyboard shortcuts.

use std::borrow::Cow;

use egui::{Key, Modifiers};

use crate::app::App;
use crate::i18n::{Locale, gettext, pgettext};
use crate::model::{Action, Dialog, Page};

pub(super) const fn platform_shortcut<'a>(ctrl: &'a str, cmd: &'a str) -> &'a str {
    if cfg!(target_os = "macos") { cmd } else { ctrl }
}

pub(super) const SIDEBAR_SHORTCUT: &str = platform_shortcut("Ctrl+B", "Cmd+B");
pub(super) const QUIT_SHORTCUT: &str = platform_shortcut("Ctrl+Q", "Cmd+Q");
pub(super) const WINAMP_SHORTCUT: &str = platform_shortcut("Ctrl+M", "Cmd+Shift+M");
pub(super) const MILKDROP_SHORTCUT: &str = platform_shortcut("Ctrl+Shift+K", "Cmd+Shift+K");

pub fn handle(app: &mut App, ctx: &egui::Context) {
    let typing = ctx.memory(|memory| memory.focused().is_some());
    // A focused song row still takes Ctrl+arrow to change songs; a text
    // field uses those keys to move its caret.
    let editing_text = ctx.text_edit_focused();
    let mut actions = Vec::new();
    ctx.input_mut(|input| {
        let mut key = |modifiers: Modifiers, key: Key, action: Action| {
            if input.consume_key(modifiers, key) {
                actions.push(action);
            }
        };
        // egui ignores an extra Shift when it matches, so a Shift shortcut
        // is looked for before the plain one it extends: Ctrl+B would
        // otherwise take Ctrl+Shift+B, and Ctrl+Q Ctrl+Shift+Q.
        key(
            Modifiers::COMMAND | Modifiers::SHIFT,
            Key::A,
            Action::OpenUri("artist".into()),
        );
        key(
            Modifiers::COMMAND | Modifiers::SHIFT,
            Key::B,
            Action::OpenUri("album".into()),
        );
        // Cmd+Shift+Q is Log Out, taken by the window server.
        if cfg!(target_os = "macos") {
            key(Modifiers::COMMAND, Key::U, Action::ToggleQueuePanel);
        } else {
            key(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::Q,
                Action::ToggleQueuePanel,
            );
        }
        key(Modifiers::COMMAND, Key::F, Action::FocusSearch);
        key(Modifiers::COMMAND, Key::B, Action::ToggleSidebar);
        key(Modifiers::COMMAND, Key::Comma, Action::Open(Page::Settings));
        key(Modifiers::COMMAND, Key::Q, Action::Quit);
        // The platform's close key. macOS only closes a window from its
        // menu, which winit does not install, and the mini player has no
        // title bar for the system to close it by.
        key(Modifiers::COMMAND, Key::W, Action::CloseWindow);
        // winit installs its own macOS app menu, whose Hide item owns Cmd+H
        // before the window is offered the key.
        if cfg!(target_os = "macos") {
            key(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::H,
                Action::Open(Page::Home),
            );
        } else {
            key(Modifiers::COMMAND, Key::H, Action::Open(Page::Home));
        }
        key(Modifiers::COMMAND, Key::L, Action::Open(Page::LikedSongs));
        // Cmd+M minimises on macOS.
        if cfg!(target_os = "macos") {
            key(
                Modifiers::COMMAND | Modifiers::SHIFT,
                Key::M,
                Action::ToggleWinampWindow,
            );
        } else {
            key(Modifiers::COMMAND, Key::M, Action::ToggleWinampWindow);
        }
        // Winamp's key for starting and stopping the visualisation plug-in.
        key(
            Modifiers::COMMAND | Modifiers::SHIFT,
            Key::K,
            Action::ToggleWinampMilkdrop,
        );
        key(
            Modifiers::COMMAND,
            Key::Slash,
            Action::ShowDialog(Dialog::Shortcuts),
        );
        // In a text field these keys move the caret: Ctrl or Alt by a word,
        // Cmd to the end of the line, Up and Down to the end of the text.
        // Taking them here skipped songs while a search was being typed.
        if !editing_text {
            // Space always controls the current song, even when a song row
            // or button has focus. Enter still activates the focused control.
            key(Modifiers::NONE, Key::Space, Action::TogglePlay);
            key(Modifiers::ALT, Key::ArrowLeft, Action::Back);
            key(Modifiers::ALT, Key::ArrowRight, Action::Forward);
            key(Modifiers::COMMAND, Key::ArrowLeft, Action::Previous);
            key(Modifiers::COMMAND, Key::ArrowRight, Action::Next);
            key(Modifiers::COMMAND, Key::ArrowUp, Action::VolumeBy(5));
            key(Modifiers::COMMAND, Key::ArrowDown, Action::VolumeBy(-5));
        }
        if !typing {
            key(
                Modifiers::NONE,
                Key::Questionmark,
                Action::ShowDialog(Dialog::Shortcuts),
            );
            key(
                Modifiers::SHIFT,
                Key::Questionmark,
                Action::ShowDialog(Dialog::Shortcuts),
            );
            key(Modifiers::SHIFT, Key::ArrowLeft, Action::SeekBy(-10_000));
            key(Modifiers::SHIFT, Key::ArrowRight, Action::SeekBy(10_000));
            key(Modifiers::NONE, Key::M, Action::ToggleMute);
            key(Modifiers::NONE, Key::S, Action::ToggleShuffle);
            key(Modifiers::NONE, Key::R, Action::CycleRepeat);
            key(Modifiers::NONE, Key::Q, Action::ToggleQueuePanel);
            key(Modifiers::NONE, Key::L, Action::ToggleLyricsPanel);
            key(Modifiers::NONE, Key::Slash, Action::FocusSearch);
            key(
                Modifiers::NONE,
                Key::F,
                Action::SetLyricsFullscreen(app.lyrics_fullscreen.is_none()),
            );
        }
    });
    if !typing
        && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::B))
        && let Some(now) = app.now_playing().filter(|now| !now.is_episode)
    {
        actions.push(Action::ToggleSaved(now.uri));
    }
    // Resolve the "open current artist/album" placeholders.
    for action in actions {
        match action {
            Action::OpenUri(kind) if kind == "artist" => {
                if let Some(id) = app
                    .now_playing()
                    .and_then(|now| now.artists.first().and_then(|artist| artist.id.clone()))
                {
                    app.actions.push(Action::Open(Page::Artist(id)));
                }
            }
            Action::OpenUri(kind) if kind == "album" => {
                if let Some(now) = app.now_playing() {
                    if let Some(id) = now.album_id {
                        app.actions.push(Action::Open(Page::Album(id)));
                    } else if let Some(id) = now.show_id {
                        app.actions.push(Action::Open(Page::Show(id)));
                    }
                }
            }
            other => app.actions.push(other),
        }
    }
    // Map mouse back and forward buttons to navigation.
    let (back, forward) = ctx.input(|input| {
        (
            input.pointer.button_pressed(egui::PointerButton::Extra1),
            input.pointer.button_pressed(egui::PointerButton::Extra2),
        )
    });
    if back {
        app.actions.push(Action::Back);
    }
    if forward {
        app.actions.push(Action::Forward);
    }
    if ctx.input(|input| input.key_pressed(Key::Escape)) {
        if app.dialog.is_some() {
            app.actions.push(Action::CloseDialog);
        } else if app.show_devices {
            app.show_devices = false;
        } else if app.lyrics_fullscreen.is_some() {
            app.actions.push(Action::SetLyricsFullscreen(false));
        }
    }
}

/// Scroll the main page by a viewport, or to either end. Call after laying
/// out its contents so virtual lists contribute their full height.
pub(super) fn scroll_page(ui: &egui::Ui) {
    if !ui.is_enabled() || ui.ctx().text_edit_focused() || egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let visible = ui.clip_rect();
    let content = ui.min_rect();
    let first = content.top();
    let last = (content.bottom() - visible.height()).max(first);
    let mut top = visible.top().clamp(first, last);
    let mut handled = false;
    // The events are walked rather than taken with consume_key: consume_key
    // ignores an extra Shift, drops the count of repeated presses, and cannot
    // keep the order of interleaved Home and Page keys.
    ui.input_mut(|input| {
        input.events.retain(|event| {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            if *modifiers != Modifiers::NONE {
                return true;
            }
            top = match key {
                Key::PageUp => (top - visible.height()).max(first),
                Key::PageDown => (top + visible.height()).min(last),
                Key::Home => first,
                Key::End => last,
                _ => return true,
            };
            handled = true;
            false
        });
    });
    if handled {
        // Center alignment avoids adding item spacing to the page step.
        let target = egui::Rect::from_min_size(
            egui::pos2(content.left(), top),
            egui::vec2(content.width(), visible.height()),
        );
        ui.scroll_to_rect(target, Some(egui::Align::Center));
    }
}

/// The rows of the keyboard shortcuts dialog: the keys, then what they do.
/// Key names stay as the keyboard prints them; words around them, and
/// every description, are translated.
pub fn shortcuts(locale: Locale) -> Vec<(Cow<'static, str>, Cow<'static, str>)> {
    let keys = |text: &'static str| Cow::Borrowed(text);
    vec![
        (
            pgettext(locale, "key", "Space"),
            gettext(locale, "Play or pause"),
        ),
        (
            keys(platform_shortcut("Ctrl+←  /  Ctrl+→", "Cmd+←  /  Cmd+→")),
            gettext(locale, "Previous or next"),
        ),
        (
            keys("Shift+←  /  Shift+→"),
            gettext(locale, "Seek 10 seconds"),
        ),
        (
            keys(platform_shortcut("Ctrl+↑  /  Ctrl+↓", "Cmd+↑  /  Cmd+↓")),
            gettext(locale, "Volume up or down"),
        ),
        (keys("M"), gettext(locale, "Mute or unmute")),
        (
            keys("B"),
            gettext(locale, "Like or unlike the playing song"),
        ),
        (keys("S"), gettext(locale, "Toggle shuffle")),
        (keys("R"), gettext(locale, "Cycle repeat")),
        (keys("Q"), gettext(locale, "Show the queue")),
        (keys("L"), gettext(locale, "Show the lyrics")),
        (keys("F"), gettext(locale, "Full screen lyrics")),
        (keys("Esc"), gettext(locale, "Lyrics: leave full screen")),
        (
            keys("Shift+↑  /  Shift+↓"),
            gettext(locale, "Song list: extend or shrink the selection"),
        ),
        (keys("Delete"), gettext(locale, "Remove from this playlist")),
        (
            keys(platform_shortcut("Ctrl+A", "Cmd+A")),
            gettext(locale, "Song list: select all"),
        ),
        (
            keys(platform_shortcut("Ctrl+C", "Cmd+C")),
            gettext(locale, "Song list: copy the selected songs' links"),
        ),
        (
            keys(platform_shortcut("Ctrl+X", "Cmd+X")),
            gettext(locale, "Playlist: cut the selected songs"),
        ),
        (
            keys(platform_shortcut("Ctrl+V", "Cmd+V")),
            gettext(locale, "Playlist: add the pasted song links"),
        ),
        (
            if cfg!(target_os = "macos") {
                // Translators: Keep the key names. Only the word "or" is translated.
                gettext(locale, "Cmd+F  or  /")
            } else {
                // Translators: Keep the key names. Only the word "or" is translated.
                gettext(locale, "Ctrl+F  or  /")
            },
            gettext(locale, "Search"),
        ),
        (
            keys(SIDEBAR_SHORTCUT),
            gettext(locale, "Show or hide the sidebar"),
        ),
        (keys("Alt+←  /  Alt+→"), gettext(locale, "Back or forward")),
        (
            keys("Page Up  /  Page Down"),
            gettext(locale, "Scroll the page"),
        ),
        (
            keys("Home  /  End"),
            gettext(locale, "Top or bottom of the page"),
        ),
        (
            keys(platform_shortcut("Ctrl+H", "Cmd+Shift+H")),
            gettext(locale, "Home"),
        ),
        (
            keys(platform_shortcut("Ctrl+L", "Cmd+L")),
            gettext(locale, "Liked Songs"),
        ),
        (
            keys(platform_shortcut("Ctrl+Shift+A", "Cmd+Shift+A")),
            gettext(locale, "Go to the playing artist"),
        ),
        (
            keys(platform_shortcut("Ctrl+Shift+B", "Cmd+Shift+B")),
            gettext(locale, "Go to the playing album"),
        ),
        (keys(WINAMP_SHORTCUT), gettext(locale, "Winamp mini player")),
        (
            keys(MILKDROP_SHORTCUT),
            gettext(locale, "MilkDrop, under the mini player"),
        ),
        (
            // Translators: Keep the key name F. Translate "or" and "double-click".
            gettext(locale, "F  or  double-click"),
            gettext(locale, "MilkDrop: fill the screen"),
        ),
        (keys("→  /  N"), gettext(locale, "MilkDrop: next preset")),
        (
            keys("←  /  P"),
            gettext(locale, "MilkDrop: previous preset"),
        ),
        (keys("L"), gettext(locale, "MilkDrop: keep this preset")),
        (
            keys("Esc"),
            gettext(locale, "MilkDrop: leave full screen, or close"),
        ),
        (
            keys(platform_shortcut("Ctrl+,", "Cmd+,")),
            gettext(locale, "Settings"),
        ),
        (
            if cfg!(target_os = "macos") {
                // Translators: Keep the key names. Only the word "or" is translated.
                gettext(locale, "Cmd+/ or ?")
            } else {
                // Translators: Keep the key names. Only the word "or" is translated.
                gettext(locale, "Ctrl+/ or ?")
            },
            gettext(locale, "Keyboard shortcuts"),
        ),
        (
            keys(platform_shortcut("Ctrl+W", "Cmd+W")),
            gettext(locale, "Close the window"),
        ),
        (keys(QUIT_SHORTCUT), gettext(locale, "Quit")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppOptions;
    use crate::paths::AppDirs;
    use crate::settings::Settings;

    /// A text field edits with the arrow keys: Ctrl or Alt moves a word,
    /// Cmd moves to the end of the line, and Ctrl or Cmd with Up or Down
    /// to either end of the text. While a field has focus those keys are
    /// the field's, and the shortcuts on them wait until it lets go.
    #[test]
    fn a_focused_text_field_keeps_the_arrow_keys_it_edits_with() {
        let root =
            std::env::temp_dir().join(format!("spotifast-text-arrows-test-{}", std::process::id()));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let mut app = App::new(
            &crate::backend::Waker::default(),
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
                autostart: false,
            },
        );
        crate::demo::populate(&mut app);

        // The modifiers as the platform reports its command key.
        let command = if cfg!(target_os = "macos") {
            Modifiers::MAC_CMD | Modifiers::COMMAND
        } else {
            Modifiers::CTRL | Modifiers::COMMAND
        };
        let ctx = egui::Context::default();
        let field = egui::Id::new("keys-test-field");
        let row = egui::Id::new("keys-test-row");
        let mut text = String::from("find this song");
        let end = text.chars().count();
        // Shortcuts first, then the page, as `ui::show` draws a frame.
        let frame = |app: &mut App, text: &mut String, events: Vec<egui::Event>| {
            app.actions.clear();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    handle(app, ui.ctx());
                    ui.add(egui::TextEdit::singleline(text).id(field));
                    if ui
                        .interact(
                            egui::Rect::from_min_size(
                                egui::pos2(0.0, 100.0),
                                egui::vec2(200.0, 28.0),
                            ),
                            row,
                            egui::Sense::click(),
                        )
                        .clicked()
                    {
                        app.actions.push(Action::Open(Page::Home));
                    }
                },
            );
            output.textures_delta.clear();
            format!("{:?}", app.actions)
        };
        let press = |key: Key, modifiers: Modifiers| {
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }]
        };
        let caret_at = |index: usize| {
            let mut state = egui::TextEdit::load_state(&ctx, field).expect("the field's state");
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(index),
                )));
            state.store(&ctx, field);
        };
        let caret = || {
            egui::TextEdit::load_state(&ctx, field)
                .and_then(|state| state.cursor.char_range())
                .map(|range| range.primary.index)
        };

        frame(&mut app, &mut text, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(field));
        frame(&mut app, &mut text, vec![]);
        assert!(ctx.memory(|memory| memory.has_focus(field)));

        // Cmd+Left goes to the start of the line; Ctrl+Left back a word.
        let command_left = if cfg!(target_os = "macos") { 0 } else { 10 };
        for (keys, modifiers, lands, what) in [
            (
                Key::ArrowLeft,
                command,
                command_left,
                "the previous-song shortcut",
            ),
            (Key::ArrowLeft, Modifiers::ALT, 10, "the back shortcut"),
            (Key::ArrowUp, command, 0, "the volume shortcut"),
        ] {
            caret_at(end);
            assert_eq!(
                frame(&mut app, &mut text, press(keys, modifiers)),
                "[]",
                "{what} took {keys:?} from the focused field"
            );
            assert_eq!(
                caret(),
                Some(egui::text::CCursor::new(lands).index),
                "{keys:?} with {modifiers:?} moves the caret"
            );
        }
        assert_eq!(text, "find this song");

        // Space belongs to text editing while the field has focus.
        caret_at(end);
        let mut space = press(Key::Space, Modifiers::NONE);
        space.push(egui::Event::Text(" ".into()));
        assert_eq!(frame(&mut app, &mut text, space), "[]");
        assert_eq!(text, "find this song ");

        // With nothing being typed into, the shortcuts are the app's again.
        ctx.memory_mut(|memory| memory.surrender_focus(field));
        frame(&mut app, &mut text, vec![]);
        assert_eq!(
            frame(&mut app, &mut text, press(Key::ArrowRight, command)),
            format!("{:?}", [Action::Next])
        );
        assert_eq!(
            frame(&mut app, &mut text, press(Key::ArrowLeft, Modifiers::ALT)),
            format!("{:?}", [Action::Back])
        );
        // A focused non-text row still receives player/navigation shortcuts.
        ctx.memory_mut(|memory| memory.request_focus(row));
        frame(&mut app, &mut text, vec![]);
        assert!(ctx.memory(|memory| memory.has_focus(row)));
        assert!(!ctx.text_edit_focused());
        assert_eq!(
            frame(&mut app, &mut text, press(Key::Space, Modifiers::NONE)),
            format!("{:?}", [Action::TogglePlay]),
            "a focused row must not steal Space from play/pause"
        );
        assert_eq!(
            frame(&mut app, &mut text, press(Key::Enter, Modifiers::NONE)),
            format!("{:?}", [Action::Open(Page::Home)]),
            "Enter still activates the focused row"
        );
        assert_eq!(
            frame(&mut app, &mut text, press(Key::ArrowRight, command)),
            format!("{:?}", [Action::Next])
        );
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_constants_name_the_platform_modifier() {
        let expected = if cfg!(target_os = "macos") {
            ["Cmd+B", "Cmd+Q", "Cmd+Shift+M", "Cmd+Shift+K"]
        } else {
            ["Ctrl+B", "Ctrl+Q", "Ctrl+M", "Ctrl+Shift+K"]
        };
        assert_eq!(
            [
                SIDEBAR_SHORTCUT,
                QUIT_SHORTCUT,
                WINAMP_SHORTCUT,
                MILKDROP_SHORTCUT,
            ],
            expected
        );
    }

    #[test]
    fn shortcut_dialog_never_names_the_other_command_modifier() {
        let other = if cfg!(target_os = "macos") {
            "Ctrl+"
        } else {
            "Cmd+"
        };
        for (keys, _) in shortcuts(Locale::English) {
            assert!(!keys.contains(other), "wrong modifier in {keys}");
        }
    }

    #[test]
    fn shortcut_dialog_names_platform_reserved_alternatives() {
        let label = |description: &str| {
            shortcuts(Locale::English)
                .into_iter()
                .find(|(_, candidate)| candidate == description)
                .map(|(keys, _)| keys)
                .unwrap()
        };
        if cfg!(target_os = "macos") {
            assert_eq!(label("Home"), "Cmd+Shift+H");
            assert_eq!(label("Winamp mini player"), "Cmd+Shift+M");
        } else {
            assert_eq!(label("Home"), "Ctrl+H");
            assert_eq!(label("Winamp mini player"), "Ctrl+M");
        }
    }

    #[test]
    fn b_toggles_the_playing_song_in_liked_songs() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-like-shortcut-test-{}",
            std::process::id()
        ));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let mut app = App::new(
            &crate::backend::Waker::default(),
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
                autostart: false,
            },
        );
        crate::demo::populate(&mut app);

        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::B,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |_ui| handle(&mut app, &ctx));
        output.textures_delta.clear();

        assert!(matches!(
            app.actions.as_slice(),
            [Action::ToggleSaved(uri)] if uri == "spotify:track:trk0"
        ));
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// egui ignores an extra Shift when it matches a shortcut, so the
    /// plain Ctrl+Q would also take Ctrl+Shift+Q if it were looked for
    /// first.
    #[test]
    fn a_shift_shortcut_is_not_taken_by_the_plain_one_it_extends() {
        let root = std::env::temp_dir().join(format!(
            "spotifast-shift-shortcut-test-{}",
            std::process::id()
        ));
        let dirs = AppDirs {
            config: root.join("config"),
            state: root.join("state"),
            cache: root.join("cache"),
        };
        let mut app = App::new(
            &crate::backend::Waker::default(),
            dirs,
            Settings::default(),
            AppOptions {
                media_controls: false,
                restore_sign_in: false,
                tray: false,
                autostart: false,
            },
        );
        crate::demo::populate(&mut app);
        let album = app.now_playing().and_then(|now| now.album_id);
        assert!(album.is_some(), "the demo song has an album to go to");

        // The modifiers as the platform reports its command key.
        let command = if cfg!(target_os = "macos") {
            Modifiers::MAC_CMD | Modifiers::COMMAND
        } else {
            Modifiers::CTRL | Modifiers::COMMAND
        };
        let ctx = egui::Context::default();
        let press = |app: &mut App, key: Key, modifiers: Modifiers| {
            app.actions.clear();
            let input = egui::RawInput {
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| handle(app, ui.ctx()));
            output.textures_delta.clear();
            format!("{:?}", app.actions)
        };

        let shift = command | Modifiers::SHIFT;
        // macOS shows the queue on Cmd+U; Cmd+Shift+Q is Log Out.
        if !cfg!(target_os = "macos") {
            assert_eq!(
                press(&mut app, Key::Q, shift),
                format!("{:?}", [Action::ToggleQueuePanel]),
                "the queue shortcut shows the queue"
            );
        }
        assert_eq!(
            press(&mut app, Key::Q, command),
            format!("{:?}", [Action::Quit]),
            "the quit shortcut still quits"
        );
        assert_eq!(
            press(&mut app, Key::B, shift),
            format!("{:?}", [Action::Open(Page::Album(album.unwrap()))]),
            "the album shortcut goes to the album"
        );
        assert_eq!(
            press(&mut app, Key::B, command),
            format!("{:?}", [Action::ToggleSidebar]),
            "the sidebar shortcut still toggles the sidebar"
        );
        assert_eq!(
            press(&mut app, Key::F, command),
            format!("{:?}", [Action::FocusSearch]),
            "the search shortcut is not taken by plain F"
        );
        assert_eq!(
            press(&mut app, Key::F, Modifiers::NONE),
            format!("{:?}", [Action::SetLyricsFullscreen(true)]),
            "F enters lyrics full screen"
        );
        app.lyrics_fullscreen = Some(false);
        assert_eq!(
            press(&mut app, Key::F, Modifiers::NONE),
            format!("{:?}", [Action::SetLyricsFullscreen(false)]),
            "F leaves lyrics full screen"
        );
        app.backend.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// A page of `content_height` points in a 600x320 window's vertical
    /// scroll area, as `central` draws the main page. Each frame returns
    /// the scroll offset, the visible height and the key presses the frame
    /// still carries afterwards.
    struct PageHarness {
        ctx: egui::Context,
        time: f64,
    }

    impl PageHarness {
        fn new() -> Self {
            PageHarness {
                ctx: egui::Context::default(),
                time: 0.0,
            }
        }

        fn frame(
            &mut self,
            content_height: f32,
            events: Vec<egui::Event>,
            fill: &mut dyn FnMut(&mut egui::Ui),
        ) -> (f32, f32, Vec<egui::Event>) {
            self.time += 1.0 / 60.0;
            let mut offset = 0.0;
            let mut height = 0.0;
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 320.0),
                    )),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let scroll = egui::ScrollArea::vertical().animated(false).show(ui, |ui| {
                        fill(ui);
                        ui.allocate_space(egui::vec2(400.0, content_height));
                        scroll_page(ui);
                    });
                    offset = scroll.state.offset.y;
                    height = scroll.inner_rect.height();
                },
            );
            output.textures_delta.clear();
            let keys = self.ctx.input(|input| {
                input
                    .events
                    .iter()
                    .filter(|event| matches!(event, egui::Event::Key { pressed: true, .. }))
                    .cloned()
                    .collect()
            });
            (offset, height, keys)
        }
    }

    fn key_press(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// Page Down steps by a viewport, Page Up steps back, Home and End
    /// jump, and both ends clamp; a page shorter than its viewport never
    /// scrolls.
    #[test]
    fn page_keys_scroll_the_main_page_by_a_viewport() {
        let mut page = PageHarness::new();
        let mut tall = |_: &mut egui::Ui| {};
        let (offset, height, _) = page.frame(2000.0, vec![], &mut tall);
        assert_eq!(offset, 0.0);
        let maximum = 2000.0 - height;

        let (down, _, _) = page.frame(
            2000.0,
            vec![key_press(Key::PageDown, Modifiers::NONE)],
            &mut tall,
        );
        assert!(
            (down - height).abs() < 1.0,
            "Page Down moves one viewport ({height}), got {down}"
        );
        let step = down;
        let (more, _, _) = page.frame(
            2000.0,
            vec![key_press(Key::PageDown, Modifiers::NONE)],
            &mut tall,
        );
        assert!(
            (more - 2.0 * height).abs() < 1.0,
            "a second Page Down moves another viewport, got {more}"
        );

        let (back, _, _) = page.frame(
            2000.0,
            vec![key_press(Key::PageUp, Modifiers::NONE)],
            &mut tall,
        );
        assert!(
            (back - step).abs() < 1.0,
            "Page Up reverses one step, got {back} from {more}"
        );

        let (bottom, _, _) = page.frame(
            2000.0,
            vec![key_press(Key::End, Modifiers::NONE)],
            &mut tall,
        );
        assert!(
            (bottom - maximum).abs() < 1.0,
            "End reaches the bottom {maximum}, got {bottom}"
        );
        let (clamped, _, _) = page.frame(
            2000.0,
            vec![key_press(Key::PageDown, Modifiers::NONE)],
            &mut tall,
        );
        assert!(
            (clamped - maximum).abs() < 1.0,
            "Page Down past the bottom clamps, got {clamped}"
        );

        let (top, _, _) = page.frame(
            2000.0,
            vec![key_press(Key::Home, Modifiers::NONE)],
            &mut tall,
        );
        assert!(top.abs() < 1.0, "Home returns to the top, got {top}");
        let (clamped, _, _) = page.frame(
            2000.0,
            vec![key_press(Key::PageUp, Modifiers::NONE)],
            &mut tall,
        );
        assert!(
            clamped.abs() < 1.0,
            "Page Up past the top clamps, got {clamped}"
        );

        let (short, _, _) = page.frame(
            100.0,
            vec![key_press(Key::PageDown, Modifiers::NONE)],
            &mut |_: &mut egui::Ui| {},
        );
        assert!(
            short.abs() < 1.0,
            "a short page does not scroll, got {short}"
        );
    }

    /// Several key events in one frame apply in order, a held key's
    /// repeats scroll again, and releases do nothing.
    #[test]
    fn page_keys_apply_events_in_order() {
        let mut page = PageHarness::new();
        let mut tall = |_: &mut egui::Ui| {};
        let (_, height, _) = page.frame(2000.0, vec![], &mut tall);
        let maximum = 2000.0 - height;

        let (offset, _, _) = page.frame(
            2000.0,
            vec![
                key_press(Key::Home, Modifiers::NONE),
                key_press(Key::PageDown, Modifiers::NONE),
            ],
            &mut tall,
        );
        assert!(offset > 0.0, "Home then Page Down lands one page in");
        let first = offset;

        let (offset, _, _) = page.frame(
            2000.0,
            vec![
                key_press(Key::End, Modifiers::NONE),
                key_press(Key::PageUp, Modifiers::NONE),
            ],
            &mut tall,
        );
        assert!(
            (offset - (maximum - first)).abs() < 1.0,
            "End then Page Up lands a page above the bottom, got {offset}"
        );

        let before_repeat = offset;
        let mut held = key_press(Key::PageDown, Modifiers::NONE);
        let egui::Event::Key { repeat, .. } = &mut held else {
            unreachable!()
        };
        *repeat = true;
        let (offset, _, _) = page.frame(2000.0, vec![held], &mut tall);
        assert!(offset > before_repeat, "a held key's repeat still scrolls");
        assert!(
            (offset - maximum).abs() < 1.0,
            "the repeat reaches the bottom {maximum}, got {offset}"
        );

        let before = offset;
        let release = egui::Event::Key {
            key: Key::PageDown,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let (offset, _, _) = page.frame(2000.0, vec![release], &mut tall);
        assert_eq!(offset, before, "a key release never scrolls");
    }

    /// A modified Page or End key belongs to the widget or system: it is
    /// neither scrolled by nor consumed.
    #[test]
    fn modified_page_keys_are_left_alone() {
        let mut page = PageHarness::new();
        let mut tall = |_: &mut egui::Ui| {};
        page.frame(2000.0, vec![], &mut tall);
        for modifiers in [
            Modifiers::SHIFT,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::MAC_CMD,
            Modifiers::COMMAND,
        ] {
            let (offset, _, keys) =
                page.frame(2000.0, vec![key_press(Key::PageDown, modifiers)], &mut tall);
            assert_eq!(offset, 0.0, "{modifiers:?}+Page Down must not scroll");
            assert!(
                keys.iter().any(|event| matches!(
                    event,
                    egui::Event::Key {
                        key: Key::PageDown,
                        ..
                    }
                )),
                "{modifiers:?}+Page Down must be left for its widget"
            );
        }
    }

    /// Text fields, open popups and disabled interfaces keep the page
    /// keys for themselves.
    #[test]
    fn page_keys_yield_to_text_fields_popups_and_disabled_views() {
        let mut page = PageHarness::new();
        let field = egui::Id::new("page-scroll-field");
        let mut text = String::from("find this song");
        let mut tall_with_field = |ui: &mut egui::Ui, enabled: bool| {
            if !enabled {
                ui.disable();
            }
            ui.add(egui::TextEdit::singleline(&mut text).id(field));
        };
        page.frame(2000.0, vec![], &mut |ui| tall_with_field(ui, true));
        page.ctx.memory_mut(|memory| memory.request_focus(field));
        page.frame(2000.0, vec![], &mut |ui| tall_with_field(ui, true));
        assert!(page.ctx.text_edit_focused());
        let (offset, _, keys) = page.frame(
            2000.0,
            vec![key_press(Key::PageDown, Modifiers::NONE)],
            &mut |ui| tall_with_field(ui, true),
        );
        assert_eq!(offset, 0.0, "a focused field keeps Page Down");
        assert!(
            keys.iter().any(|event| matches!(
                event,
                egui::Event::Key {
                    key: Key::PageDown,
                    ..
                }
            )),
            "the field did not take Page Down, so it must remain"
        );

        page.ctx.memory_mut(|memory| memory.surrender_focus(field));
        let popup = egui::Id::new("page-scroll-popup");
        egui::Popup::open_id(&page.ctx, popup);
        let (offset, _, _) = page.frame(
            2000.0,
            vec![key_press(Key::PageDown, Modifiers::NONE)],
            &mut |ui| tall_with_field(ui, true),
        );
        assert_eq!(offset, 0.0, "an open popup keeps Page Down");
        egui::Popup::close_id(&page.ctx, popup);

        let (offset, _, keys) = page.frame(
            2000.0,
            vec![key_press(Key::PageDown, Modifiers::NONE)],
            &mut |ui| tall_with_field(ui, false),
        );
        assert_eq!(offset, 0.0, "a disabled page keeps Page Down");
        assert!(
            keys.iter().any(|event| matches!(
                event,
                egui::Event::Key {
                    key: Key::PageDown,
                    ..
                }
            )),
            "a disabled page must not swallow Page Down"
        );
    }
}
