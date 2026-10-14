//! Where each control sits in the main window, in skin pixels.
//!
//! The positions are Winamp's own, via Webamp's `main-window.css`; every
//! classic skin paints its background to match them.

/// A rectangle in the window, in skin pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Area {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

pub const WINDOW_WIDTH: u32 = 275;
pub const WINDOW_HEIGHT: u32 = 116;
/// The window's height in shade mode, when only the title bar shows.
pub const SHADE_HEIGHT: u32 = 14;

macro_rules! areas {
    ($($(#[$attr:meta])* $name:ident = ($x:expr, $y:expr, $w:expr, $h:expr);)*) => {
        $($(#[$attr])* pub const $name: Area = Area::new($x, $y, $w, $h);)*
        /// Every control, so the layout can be checked against the window.
        pub const ALL: &[(&str, Area)] = &[$((stringify!($name), $name),)*];
    };
}

areas! {
    TITLE_BAR = (0, 0, 275, 14);
    OPTIONS_BUTTON = (6, 3, 9, 9);
    MINIMIZE_BUTTON = (244, 3, 9, 9);
    SHADE_BUTTON = (254, 3, 9, 9);
    CLOSE_BUTTON = (264, 3, 9, 9);
    CLUTTER_BAR = (10, 22, 8, 43);
    CLUTTER_O = (10, 25, 8, 8);
    CLUTTER_A = (10, 33, 8, 7);
    CLUTTER_I = (10, 40, 8, 7);
    CLUTTER_D = (10, 47, 8, 8);
    CLUTTER_V = (10, 55, 8, 7);
    WORK_INDICATOR = (24, 28, 3, 9);
    STATUS = (26, 28, 9, 9);
    /// The minus sign when the skin has `nums_ex`: a full digit cell.
    MINUS_EX = (36, 26, 9, 13);
    /// The minus sign borrowed from the digit sheet: one row of pixels.
    MINUS = (38, 32, 5, 1);
    MINUTE_TENS = (48, 26, 9, 13);
    MINUTE_ONES = (60, 26, 9, 13);
    SECOND_TENS = (78, 26, 9, 13);
    SECOND_ONES = (90, 26, 9, 13);
    MARQUEE = (111, 27, 154, 6);
    KBPS = (111, 43, 15, 6);
    KHZ = (156, 43, 10, 6);
    VISUALIZER = (24, 43, 76, 16);
    MONO = (212, 41, 27, 12);
    STEREO = (239, 41, 29, 12);
    VOLUME = (107, 57, 68, 13);
    BALANCE = (177, 57, 38, 13);
    /// Not Winamp's: the lyrics toggle, written in the skin's font in the
    /// rows the display keeps under the visualizer, flush with its right.
    LYRICS_BUTTON = (88, 59, 14, 7);
    EQ_BUTTON = (219, 58, 23, 12);
    PLAYLIST_BUTTON = (242, 58, 23, 12);
    POSITION = (16, 72, 248, 10);
    PREVIOUS = (16, 88, 23, 18);
    PLAY = (39, 88, 23, 18);
    PAUSE = (62, 88, 23, 18);
    STOP = (85, 88, 23, 18);
    NEXT = (108, 88, 22, 18);
    EJECT = (136, 89, 22, 16);
    SHUFFLE = (164, 89, 47, 15);
    REPEAT = (210, 89, 28, 15);
    ABOUT = (253, 91, 13, 15);
}

/// The playlist window, below the main one. Its height is whatever the
/// listener stretched it to, in the steps Winamp allowed; these are the
/// parts that stay put.
pub const PLAYLIST_TITLE_HEIGHT: u32 = 20;
pub const PLAYLIST_BOTTOM_HEIGHT: u32 = 38;
pub const PLAYLIST_LEFT_WIDTH: u32 = 12;
pub const PLAYLIST_RIGHT_WIDTH: u32 = 20;
pub const PLAYLIST_TILE_WIDTH: u32 = 25;
pub const PLAYLIST_TILE_HEIGHT: u32 = 29;
pub const PLAYLIST_TRACK_HEIGHT: u32 = 13;
pub const PLAYLIST_MIN_HEIGHT: u32 = 116;
/// The playlist rolled up to its title bar.
pub const PLAYLIST_SHADE_HEIGHT: u32 = 14;
pub const PLAYLIST_MAX_HEIGHT: u32 = 116 + 29 * 20;
pub const PLAYLIST_RESIZE_STEP: u32 = 29;
/// The title bar's buttons, from the playlist's top left.
pub const PLAYLIST_CLOSE: Area = Area::new(264, 3, 9, 9);
pub const PLAYLIST_SHADE: Area = Area::new(254, 3, 9, 9);
/// The scroll handle's column, and the corner that resizes.
pub const PLAYLIST_SCROLL_X: u32 = 260;
pub const PLAYLIST_SCROLL_HANDLE_HEIGHT: u32 = 18;
pub const PLAYLIST_GRIP: u32 = 20;
/// Where the times are written: x from the left, y from the bottom bar's
/// top.
pub const PLAYLIST_RUNNING_TIME: (u32, u32) = (132, 10);
pub const PLAYLIST_TRACK_TIME: (u32, u32) = (191, 23);
/// The little transport along the bottom: six ten-pixel cells from here,
/// previous to eject.
pub const PLAYLIST_MINI_TRANSPORT: (u32, u32) = (128, 22);
/// The menus along the bottom, each 22 by 18 from eight rows down, by
/// name and x.
pub const PLAYLIST_MENUS: [(&str, u32); 5] = [
    ("add", 14),
    ("rem", 43),
    ("sel", 72),
    ("misc", 101),
    ("list", 231),
];

/// The lyrics window, between the equalizer and the playlist: the
/// playlist's title bar, a few lines in the playlist's font, and the last
/// rows of its bottom bar for an edge. Its corner stretches it a line at a
/// time while nothing hangs below it.
pub const LYRICS_MIN_LINES: u32 = 4;
pub const LYRICS_MAX_LINES: u32 = 20;
pub const LYRICS_TITLE_HEIGHT: u32 = PLAYLIST_TITLE_HEIGHT;
pub const LYRICS_BOTTOM_HEIGHT: u32 = 2;
/// Room above and below the lines, so the first and last do not touch
/// the frame.
pub const LYRICS_PADDING: u32 = 2;
/// FOLLOW, over the title bar's rules at the left, inside its corner.
pub const LYRICS_FOLLOW: Area = Area::new(4, 4, 38, 12);

/// The lyrics window's height for so many lines.
pub const fn lyrics_height(lines: u32) -> u32 {
    LYRICS_TITLE_HEIGHT
        + LYRICS_PADDING
        + lines * PLAYLIST_TRACK_HEIGHT
        + LYRICS_PADDING
        + LYRICS_BOTTOM_HEIGHT
}

/// The equalizer window, between the main window and the playlist.
pub const EQ_HEIGHT: u32 = 116;
pub const EQ_TITLE_BAR: Area = Area::new(0, 0, 275, 14);
pub const EQ_CLOSE: Area = Area::new(264, 3, 9, 9);
pub const EQ_ON: Area = Area::new(14, 18, 26, 12);
pub const EQ_AUTO: Area = Area::new(40, 18, 32, 12);
pub const EQ_PRESETS_BUTTON: Area = Area::new(217, 18, 44, 12);
pub const EQ_GRAPH: Area = Area::new(86, 17, 113, 19);
pub const EQ_PREAMP: Area = Area::new(21, 38, 14, 63);

/// The slider of a band, the ten of them eighteen pixels apart.
pub const fn eq_band(band: usize) -> Area {
    Area::new(78 + 18 * band as u32, 38, 14, 63)
}

/// The equalizer rolled up to its title bar, with the volume and balance
/// along it as tiny sliders, and the button that rolls it up or down.
pub const EQ_SHADE_HEIGHT: u32 = 14;
pub const EQ_SHADE: Area = Area::new(254, 3, 9, 9);
pub const EQ_SHADE_VOLUME: Area = Area::new(61, 4, 97, 7);
pub const EQ_SHADE_BALANCE: Area = Area::new(164, 4, 43, 7);
/// The mini sliders' thumbs are three pixels wide.
pub const EQ_SHADE_THUMB: u32 = 3;

/// Shade mode: the main window as one title bar.
pub const SHADE_TIME: Area = Area::new(127, 4, 30, 6);
pub const SHADE_PREVIOUS: Area = Area::new(169, 2, 7, 10);
pub const SHADE_PLAY: Area = Area::new(176, 2, 10, 10);
pub const SHADE_PAUSE: Area = Area::new(186, 2, 9, 10);
pub const SHADE_STOP: Area = Area::new(195, 2, 9, 10);
pub const SHADE_NEXT: Area = Area::new(204, 2, 10, 10);
pub const SHADE_EJECT: Area = Area::new(215, 2, 10, 10);
pub const SHADE_POSITION: Area = Area::new(226, 4, 17, 7);

/// The digit cells of the time display, left to right.
pub const TIME_DIGITS: [Area; 4] = [MINUTE_TENS, MINUTE_ONES, SECOND_TENS, SECOND_ONES];

/// How far a slider's thumb can travel: the track's width less the thumb's.
pub const VOLUME_TRAVEL: u32 = VOLUME.width - 14;
pub const BALANCE_TRAVEL: u32 = BALANCE.width - 14;
pub const POSITION_TRAVEL: u32 = POSITION.width - 29;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_fits_inside_the_window() {
        for (name, area) in ALL {
            assert!(
                area.x + area.width <= WINDOW_WIDTH && area.y + area.height <= WINDOW_HEIGHT,
                "{name} leaves the window"
            );
        }
    }

    #[test]
    fn the_transport_row_is_contiguous() {
        assert_eq!(PREVIOUS.x + PREVIOUS.width, PLAY.x);
        assert_eq!(PLAY.x + PLAY.width, PAUSE.x);
        assert_eq!(PAUSE.x + PAUSE.width, STOP.x);
        assert_eq!(STOP.x + STOP.width, NEXT.x);
    }

    #[test]
    fn the_lyrics_toggle_sits_under_the_visualizer() {
        const { assert!(LYRICS_BUTTON.y >= VISUALIZER.y + VISUALIZER.height) };
        const { assert!(LYRICS_BUTTON.x + LYRICS_BUTTON.width <= VISUALIZER.x + VISUALIZER.width + 2) };
        // The display's frame runs along the row above the seek bar's gap.
        const { assert!(LYRICS_BUTTON.y + LYRICS_BUTTON.height <= POSITION.y - 6) };
    }

    #[test]
    fn hit_testing_uses_half_open_edges() {
        assert!(PLAY.contains(39, 88));
        assert!(PLAY.contains(61, 105));
        assert!(!PLAY.contains(62, 88));
        assert!(!PLAY.contains(39, 106));
    }
}
