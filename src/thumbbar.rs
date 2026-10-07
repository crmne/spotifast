//! The taskbar thumbnail toolbar: the transport buttons Windows draws
//! underneath the window's preview.
//!
//! Not the same surface as fastframe-now-playing's. Those are the System Media
//! Transport Controls: they own the media keys and the volume overlay, they
//! belong to a hidden window of their own, and they outlive the visible one
//! so the tray keeps working. These buttons belong to the real window and
//! exist only while it does, because Windows draws them under its taskbar
//! preview. Closing to the tray takes the taskbar button with it, and the
//! window that comes back gets a new toolbar.
//!
//! The three transport buttons keep fixed identifiers for the window's life.
//! State changes update that set; recreating the window or taskbar installs it
//! again. The native object belongs to the window shell, on its owning thread.
//!
//! The icons are drawn here rather than loaded, because Windows wants
//! `HICON`s at whatever size the system asks for and the app already
//! compiles `tiny-skia` for the Winamp playlist's text. They follow the
//! system's light or dark setting, not the app's: the preview flyout is
//! Windows' own surface, so a Spotizgeg set to Light on a dark desktop
//! still needs light icons there.

#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use win::ThumbBar;

/// The button identifiers, as Windows reports them back in `WM_COMMAND`.
pub const ID_PREVIOUS: u32 = 2;
pub const ID_PLAY: u32 = 3;
pub const ID_NEXT: u32 = 4;

/// How many buttons the toolbar has, fixed for the window's life.
pub const COUNT: usize = 3;

/// What a press asks the application to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbCommand {
    Previous,
    PlayPause,
    Next,
}

impl ThumbCommand {
    pub fn action(self, state: &ThumbState) -> Option<crate::model::Action> {
        if !state.has_track || !state.can_control {
            return None;
        }
        Some(match self {
            Self::Previous => crate::model::Action::Previous,
            Self::PlayPause => crate::model::Action::TogglePlay,
            Self::Next => crate::model::Action::Next,
        })
    }
}

/// Which glyph a button wears.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Previous,
    Play,
    Pause,
    Next,
}

/// The playing state the toolbar draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThumbState {
    /// Whether anything is playing or paused at all.
    pub has_track: bool,
    pub playing: bool,
    /// Whether the device playing accepts commands.
    pub can_control: bool,
    /// The system's theme, so the icons are visible against the flyout.
    pub dark: bool,
}

/// One button, as the toolbar should show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Button {
    pub id: u32,
    pub icon: Icon,
    pub tip: &'static str,
    pub enabled: bool,
}

/// The three transport buttons for a state.
///
/// Always three, always in this order, always these identifiers. Windows took
/// them at the window's first frame and will not take a different set.
pub fn buttons(state: &ThumbState) -> [Button; COUNT] {
    let transport = state.has_track && state.can_control;
    [
        Button {
            id: ID_PREVIOUS,
            icon: Icon::Previous,
            tip: "Previous",
            enabled: transport,
        },
        Button {
            id: ID_PLAY,
            icon: if state.playing {
                Icon::Pause
            } else {
                Icon::Play
            },
            tip: if state.playing { "Pause" } else { "Play" },
            enabled: transport,
        },
        Button {
            id: ID_NEXT,
            icon: Icon::Next,
            tip: "Next",
            enabled: transport,
        },
    ]
}

/// What a pressed identifier asks for, ignoring anything unrecognised.
pub fn command_for(id: u32) -> Option<ThumbCommand> {
    Some(match id {
        ID_PREVIOUS => ThumbCommand::Previous,
        ID_PLAY => ThumbCommand::PlayPause,
        ID_NEXT => ThumbCommand::Next,
        _ => return None,
    })
}

/// Draws `icon` filling a `size` square, in the colour that reads against a
/// flyout of the given theme.
///
/// Shapes are written in a unit square and scaled, so the same drawing
/// serves whatever size the system asks for, at any scaling factor.
pub fn draw(icon: Icon, size: u32, dark: bool) -> Option<tiny_skia::Pixmap> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let mut paint = tiny_skia::Paint {
        anti_alias: true,
        ..tiny_skia::Paint::default()
    };
    // Windows draws the flyout in the system's theme, and gives the icon
    // nothing behind it, so the glyph is the only thing carrying contrast.
    if dark {
        paint.set_color_rgba8(255, 255, 255, 255);
    } else {
        paint.set_color_rgba8(32, 32, 32, 255);
    }
    let scale = tiny_skia::Transform::from_scale(size as f32, size as f32);
    let unit = |value: f32| value * size as f32;

    match icon {
        Icon::Play => {
            let path = triangle(0.30, 0.16, 0.84, 0.68, false)?;
            pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, scale, None);
        }
        Icon::Pause => {
            for left in [0.28, 0.56] {
                let bar =
                    tiny_skia::Rect::from_xywh(unit(left), unit(0.18), unit(0.16), unit(0.64))?;
                pixmap.fill_rect(bar, &paint, tiny_skia::Transform::identity(), None);
            }
        }
        Icon::Next | Icon::Previous => {
            let forward = icon == Icon::Next;
            let path = triangle(0.20, 0.16, 0.68, 0.68, !forward)?;
            pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, scale, None);
            let bar_left = if forward { 0.72 } else { 0.16 };
            let bar =
                tiny_skia::Rect::from_xywh(unit(bar_left), unit(0.16), unit(0.12), unit(0.68))?;
            pixmap.fill_rect(bar, &paint, tiny_skia::Transform::identity(), None);
        }
    }
    Some(pixmap)
}

/// A triangle in the unit square, pointing right, or left when `mirror`.
fn triangle(left: f32, top: f32, right: f32, height: f32, mirror: bool) -> Option<tiny_skia::Path> {
    let bottom = top + height;
    let middle = top + height / 2.0;
    let (back, tip) = if mirror {
        (1.0 - left, 1.0 - right)
    } else {
        (left, right)
    };
    let mut builder = tiny_skia::PathBuilder::new();
    builder.move_to(back, top);
    builder.line_to(back, bottom);
    builder.line_to(tip, middle);
    builder.close();
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_playing_leaves_every_button_dead() {
        let buttons = buttons(&ThumbState::default());
        assert!(buttons.iter().all(|button| !button.enabled));
        // Still three of them: Windows will not accept a shorter set later.
        assert_eq!(buttons.len(), COUNT);
    }

    #[test]
    fn the_play_button_shows_what_a_press_would_do() {
        let paused = ThumbState {
            has_track: true,
            can_control: true,
            ..Default::default()
        };
        let playing = ThumbState {
            playing: true,
            ..paused
        };
        assert_eq!(buttons(&paused)[1].icon, Icon::Play);
        assert_eq!(buttons(&paused)[1].tip, "Play");
        assert_eq!(buttons(&playing)[1].icon, Icon::Pause);
        assert_eq!(buttons(&playing)[1].tip, "Pause");
    }

    #[test]
    fn a_restricted_device_disables_transport() {
        let state = ThumbState {
            has_track: true,
            can_control: false,
            ..Default::default()
        };
        assert!(buttons(&state).iter().all(|button| !button.enabled));
    }

    #[test]
    fn every_button_answers_for_itself_and_nothing_else_does() {
        let state = ThumbState {
            has_track: true,
            can_control: true,
            ..Default::default()
        };
        for button in buttons(&state) {
            assert!(
                command_for(button.id).is_some(),
                "no command for {button:?}"
            );
        }
        assert_eq!(command_for(ID_PLAY), Some(ThumbCommand::PlayPause));
        assert_eq!(command_for(0), None);
        assert_eq!(command_for(99), None);
    }

    #[test]
    fn presses_use_existing_transport_actions_and_recheck_restricted_state() {
        use crate::model::Action;
        let state = ThumbState {
            has_track: true,
            can_control: true,
            ..Default::default()
        };
        assert!(matches!(
            ThumbCommand::Previous.action(&state),
            Some(Action::Previous)
        ));
        assert!(matches!(
            ThumbCommand::PlayPause.action(&state),
            Some(Action::TogglePlay)
        ));
        assert!(matches!(
            ThumbCommand::Next.action(&state),
            Some(Action::Next)
        ));
        for command in [
            ThumbCommand::Previous,
            ThumbCommand::PlayPause,
            ThumbCommand::Next,
        ] {
            assert!(
                command
                    .action(&ThumbState {
                        can_control: false,
                        ..state
                    })
                    .is_none()
            );
            assert!(command.action(&ThumbState::default()).is_none());
        }
    }

    /// The toolbar is drawn at whatever size Windows asks for, which follows
    /// the desktop's scaling. Every icon has to survive the small end.
    #[test]
    fn every_icon_draws_something_at_every_size() {
        let icons = [Icon::Previous, Icon::Play, Icon::Pause, Icon::Next];
        for icon in icons {
            for size in [16, 20, 24, 32] {
                let pixmap = draw(icon, size, true).expect("an icon");
                assert_eq!(pixmap.width(), size);
                let inked = pixmap
                    .pixels()
                    .iter()
                    .filter(|pixel| pixel.alpha() > 0)
                    .count();
                assert!(inked > 0, "{icon:?} at {size} drew nothing");
                // A glyph, not a solid block: something has to stay clear or
                // the button reads as a filled square.
                let total = (size * size) as usize;
                assert!(
                    inked < total * 9 / 10,
                    "{icon:?} at {size} covered everything"
                );
            }
        }
    }

    /// Light icons on a light flyout would be invisible, and the flyout
    /// follows Windows rather than the app's own theme choice.
    #[test]
    fn the_icons_take_their_colour_from_the_system_theme() {
        let dark = draw(Icon::Play, 16, true).expect("an icon");
        let light = draw(Icon::Play, 16, false).expect("an icon");
        let brightest = |pixmap: &tiny_skia::Pixmap| {
            pixmap
                .pixels()
                .iter()
                .filter(|pixel| pixel.alpha() == 255)
                .map(|pixel| pixel.red())
                .max()
                .unwrap_or(0)
        };
        assert!(brightest(&dark) > brightest(&light));
    }
}
