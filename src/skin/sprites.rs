//! Where each piece of the interface lives inside a skin's bitmaps.
//!
//! A classic skin is a handful of sprite sheets with fixed coordinates that
//! every skin honours, so the same table cuts every skin. The numbers are
//! Webamp's (`skinSprites.ts`), which were measured against Winamp itself.

/// One of the skin's bitmaps, by the file it comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sheet {
    /// The main window's background.
    Main,
    /// Transport buttons.
    CButtons,
    /// Title bar, its buttons, the clutter bar, and the shade-mode bar.
    TitleBar,
    /// Shuffle and repeat, plus the EQ and playlist toggles.
    ShufRep,
    /// The seek bar and its thumb.
    PosBar,
    /// The volume slider's 28 frames and thumb.
    Volume,
    /// The balance slider's frames and thumb.
    Balance,
    /// The mono and stereo lamps.
    MonoSter,
    /// The play, pause, and stop status glyphs.
    PlayPaus,
    /// The time display's digits.
    Numbers,
    /// Digits with a blank cell and a minus sign; newer skins ship this.
    NumsEx,
    /// The 5x6 bitmap font.
    Text,
    /// The playlist editor's frame and buttons.
    PlEdit,
    /// The equalizer window.
    EqMain,
    /// The equalizer rolled up: its bar, mini sliders, and buttons. Skins
    /// from Winamp 2.9 on ship this; older ones borrow the built-in one.
    EqEx,
}

impl Sheet {
    pub const ALL: [Sheet; 15] = [
        Sheet::Main,
        Sheet::CButtons,
        Sheet::TitleBar,
        Sheet::ShufRep,
        Sheet::PosBar,
        Sheet::Volume,
        Sheet::Balance,
        Sheet::MonoSter,
        Sheet::PlayPaus,
        Sheet::Numbers,
        Sheet::NumsEx,
        Sheet::Text,
        Sheet::PlEdit,
        Sheet::EqMain,
        Sheet::EqEx,
    ];

    /// The file's name without its extension, in lower case.
    pub fn file_stem(self) -> &'static str {
        match self {
            Sheet::Main => "main",
            Sheet::CButtons => "cbuttons",
            Sheet::TitleBar => "titlebar",
            Sheet::ShufRep => "shufrep",
            Sheet::PosBar => "posbar",
            Sheet::Volume => "volume",
            Sheet::Balance => "balance",
            Sheet::MonoSter => "monoster",
            Sheet::PlayPaus => "playpaus",
            Sheet::Numbers => "numbers",
            Sheet::NumsEx => "nums_ex",
            Sheet::Text => "text",
            Sheet::PlEdit => "pledit",
            Sheet::EqMain => "eqmain",
            Sheet::EqEx => "eq_ex",
        }
    }
}

/// A rectangle inside a sheet, in skin pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sprite {
    pub sheet: Sheet,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Sprite {
    pub const fn new(sheet: Sheet, x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            sheet,
            x,
            y,
            width,
            height,
        }
    }

    /// The part of this sprite a sheet of the given size actually holds.
    /// Skins are not always the sizes the table expects (a seek bar four
    /// pixels tall, a shorter volume sheet), and Winamp drew what was there.
    pub fn clipped_to(self, sheet_width: u32, sheet_height: u32) -> Option<Self> {
        let right = self.x.saturating_add(self.width).min(sheet_width);
        let bottom = self.y.saturating_add(self.height).min(sheet_height);
        if right <= self.x || bottom <= self.y {
            return None;
        }
        Some(Self {
            width: right - self.x,
            height: bottom - self.y,
            ..self
        })
    }
}

macro_rules! sprites {
    ($($(#[$attr:meta])* $name:ident = ($sheet:ident, $x:expr, $y:expr, $w:expr, $h:expr);)*) => {
        $($(#[$attr])* pub const $name: Sprite = Sprite::new(Sheet::$sheet, $x, $y, $w, $h);)*
        /// Every fixed sprite, so a skin can be checked for what it covers.
        pub const ALL: &[(&str, Sprite)] = &[$((stringify!($name), $name),)*];
    };
}

sprites! {
    MAIN_BACKGROUND = (Main, 0, 0, 275, 116);

    PREVIOUS = (CButtons, 0, 0, 23, 18);
    PREVIOUS_PRESSED = (CButtons, 0, 18, 23, 18);
    PLAY = (CButtons, 23, 0, 23, 18);
    PLAY_PRESSED = (CButtons, 23, 18, 23, 18);
    PAUSE = (CButtons, 46, 0, 23, 18);
    PAUSE_PRESSED = (CButtons, 46, 18, 23, 18);
    STOP = (CButtons, 69, 0, 23, 18);
    STOP_PRESSED = (CButtons, 69, 18, 23, 18);
    NEXT = (CButtons, 92, 0, 22, 18);
    NEXT_PRESSED = (CButtons, 92, 18, 22, 18);
    EJECT = (CButtons, 114, 0, 22, 16);
    EJECT_PRESSED = (CButtons, 114, 16, 22, 16);

    TITLE_BAR_ACTIVE = (TitleBar, 27, 0, 275, 14);
    TITLE_BAR_INACTIVE = (TitleBar, 27, 15, 275, 14);
    OPTIONS_BUTTON = (TitleBar, 0, 0, 9, 9);
    OPTIONS_BUTTON_PRESSED = (TitleBar, 0, 9, 9, 9);
    MINIMIZE_BUTTON = (TitleBar, 9, 0, 9, 9);
    MINIMIZE_BUTTON_PRESSED = (TitleBar, 9, 9, 9, 9);
    SHADE_BUTTON = (TitleBar, 0, 18, 9, 9);
    SHADE_BUTTON_PRESSED = (TitleBar, 9, 18, 9, 9);
    CLOSE_BUTTON = (TitleBar, 18, 0, 9, 9);
    CLOSE_BUTTON_PRESSED = (TitleBar, 18, 9, 9, 9);
    CLUTTER_BAR = (TitleBar, 304, 0, 8, 43);
    CLUTTER_BAR_DISABLED = (TitleBar, 312, 0, 8, 43);
    CLUTTER_O_LIT = (TitleBar, 304, 47, 8, 8);
    CLUTTER_A_LIT = (TitleBar, 312, 55, 8, 7);
    CLUTTER_I_LIT = (TitleBar, 320, 62, 8, 7);
    CLUTTER_D_LIT = (TitleBar, 328, 69, 8, 8);
    CLUTTER_V_LIT = (TitleBar, 336, 77, 8, 7);
    SHADE_BAR_ACTIVE = (TitleBar, 27, 29, 275, 14);
    SHADE_BAR_INACTIVE = (TitleBar, 27, 42, 275, 14);
    UNSHADE_BUTTON = (TitleBar, 0, 27, 9, 9);
    UNSHADE_BUTTON_PRESSED = (TitleBar, 9, 27, 9, 9);
    SHADE_POSITION_TRACK = (TitleBar, 0, 36, 17, 7);
    SHADE_POSITION_THUMB = (TitleBar, 20, 36, 3, 7);
    SHADE_POSITION_THUMB_LEFT = (TitleBar, 17, 36, 3, 7);
    SHADE_POSITION_THUMB_RIGHT = (TitleBar, 23, 36, 3, 7);

    SHUFFLE_OFF = (ShufRep, 28, 0, 47, 15);
    SHUFFLE_OFF_PRESSED = (ShufRep, 28, 15, 47, 15);
    SHUFFLE_ON = (ShufRep, 28, 30, 47, 15);
    SHUFFLE_ON_PRESSED = (ShufRep, 28, 45, 47, 15);
    REPEAT_OFF = (ShufRep, 0, 0, 28, 15);
    REPEAT_OFF_PRESSED = (ShufRep, 0, 15, 28, 15);
    REPEAT_ON = (ShufRep, 0, 30, 28, 15);
    REPEAT_ON_PRESSED = (ShufRep, 0, 45, 28, 15);
    EQ_OFF = (ShufRep, 0, 61, 23, 12);
    EQ_ON = (ShufRep, 0, 73, 23, 12);
    EQ_OFF_PRESSED = (ShufRep, 46, 61, 23, 12);
    EQ_ON_PRESSED = (ShufRep, 46, 73, 23, 12);
    PLAYLIST_OFF = (ShufRep, 23, 61, 23, 12);
    PLAYLIST_ON = (ShufRep, 23, 73, 23, 12);
    PLAYLIST_OFF_PRESSED = (ShufRep, 69, 61, 23, 12);
    PLAYLIST_ON_PRESSED = (ShufRep, 69, 73, 23, 12);

    POSITION_TRACK = (PosBar, 0, 0, 248, 10);
    POSITION_THUMB = (PosBar, 248, 0, 29, 10);
    POSITION_THUMB_PRESSED = (PosBar, 278, 0, 29, 10);

    VOLUME_THUMB = (Volume, 15, 422, 14, 11);
    VOLUME_THUMB_PRESSED = (Volume, 0, 422, 14, 11);
    BALANCE_THUMB = (Balance, 15, 422, 14, 11);
    BALANCE_THUMB_PRESSED = (Balance, 0, 422, 14, 11);

    STEREO_ON = (MonoSter, 0, 0, 29, 12);
    STEREO_OFF = (MonoSter, 0, 12, 29, 12);
    MONO_ON = (MonoSter, 29, 0, 27, 12);
    MONO_OFF = (MonoSter, 29, 12, 27, 12);

    STATUS_PLAYING = (PlayPaus, 0, 0, 9, 9);
    STATUS_PAUSED = (PlayPaus, 9, 0, 9, 9);
    STATUS_STOPPED = (PlayPaus, 18, 0, 9, 9);
    WORK_INDICATOR_OFF = (PlayPaus, 36, 0, 3, 9);
    WORK_INDICATOR_ON = (PlayPaus, 39, 0, 3, 9);

    /// Skins without `nums_ex` have no minus sign; Winamp borrowed the
    /// middle bar of the 2 and, to clear it, the same row of the 1.
    NUMBERS_MINUS = (Numbers, 20, 6, 5, 1);
    NUMBERS_NO_MINUS = (Numbers, 9, 6, 5, 1);
    NUMBERS_BLANK = (Numbers, 90, 0, 9, 13);
    NUMS_EX_BLANK = (NumsEx, 90, 0, 9, 13);
    NUMS_EX_MINUS = (NumsEx, 99, 0, 9, 13);

    PLAYLIST_TOP_LEFT_ACTIVE = (PlEdit, 0, 0, 25, 20);
    PLAYLIST_TITLE_ACTIVE = (PlEdit, 26, 0, 100, 20);
    PLAYLIST_TOP_TILE_ACTIVE = (PlEdit, 127, 0, 25, 20);
    PLAYLIST_TOP_RIGHT_ACTIVE = (PlEdit, 153, 0, 25, 20);
    /// One column of the title's plain ground, between its rules and its
    /// name, to stretch behind another name.
    PLAYLIST_TITLE_GROUND_ACTIVE = (PlEdit, 52, 0, 1, 20);
    PLAYLIST_TOP_LEFT = (PlEdit, 0, 21, 25, 20);
    PLAYLIST_TITLE = (PlEdit, 26, 21, 100, 20);
    PLAYLIST_TITLE_GROUND = (PlEdit, 52, 21, 1, 20);
    PLAYLIST_TOP_TILE = (PlEdit, 127, 21, 25, 20);
    PLAYLIST_TOP_RIGHT = (PlEdit, 153, 21, 25, 20);
    PLAYLIST_LEFT_TILE = (PlEdit, 0, 42, 12, 29);
    PLAYLIST_RIGHT_TILE = (PlEdit, 31, 42, 20, 29);
    PLAYLIST_BOTTOM_TILE = (PlEdit, 179, 0, 25, 38);
    PLAYLIST_BOTTOM_LEFT = (PlEdit, 0, 72, 125, 38);
    PLAYLIST_BOTTOM_RIGHT = (PlEdit, 126, 72, 150, 38);
    /// The bottom bar's last two rows, below its buttons and its grip: the
    /// lower edge of the lyrics window, which has no bar.
    PLAYLIST_BOTTOM_EDGE_LEFT = (PlEdit, 0, 108, 125, 2);
    PLAYLIST_BOTTOM_EDGE_RIGHT = (PlEdit, 126, 108, 150, 2);
    /// The bottom bar's last twenty pixels square, with the grip that
    /// resizes the window, for the lyrics window's corner.
    PLAYLIST_GRIP_CORNER = (PlEdit, 256, 90, 20, 20);
    PLAYLIST_VISUALIZER_BACKGROUND = (PlEdit, 205, 0, 75, 38);
    PLAYLIST_SCROLL_HANDLE = (PlEdit, 52, 53, 8, 18);
    PLAYLIST_SCROLL_HANDLE_PRESSED = (PlEdit, 61, 53, 8, 18);
    PLAYLIST_CLOSE_PRESSED = (PlEdit, 52, 42, 9, 9);
    PLAYLIST_SHADE_PRESSED = (PlEdit, 62, 42, 9, 9);
    PLAYLIST_UNSHADE_PRESSED = (PlEdit, 150, 42, 9, 9);
    PLAYLIST_SHADE_LEFT = (PlEdit, 72, 42, 25, 14);
    PLAYLIST_SHADE_TILE = (PlEdit, 72, 57, 25, 14);
    PLAYLIST_SHADE_RIGHT = (PlEdit, 99, 57, 50, 14);
    PLAYLIST_SHADE_RIGHT_ACTIVE = (PlEdit, 99, 42, 50, 14);

    EQ_BACKGROUND = (EqMain, 0, 0, 275, 116);
    EQ_TITLE_BAR_ACTIVE = (EqMain, 0, 134, 275, 14);
    EQ_TITLE_BAR_INACTIVE = (EqMain, 0, 149, 275, 14);
    EQ_CLOSE_BUTTON = (EqMain, 0, 116, 9, 9);
    EQ_CLOSE_BUTTON_PRESSED = (EqMain, 0, 125, 9, 9);
    EQ_ON_OFF = (EqMain, 10, 119, 26, 12);
    EQ_ON_OFF_PRESSED = (EqMain, 128, 119, 26, 12);
    EQ_ON_ON = (EqMain, 69, 119, 26, 12);
    EQ_ON_ON_PRESSED = (EqMain, 187, 119, 26, 12);
    EQ_AUTO_OFF = (EqMain, 36, 119, 32, 12);
    EQ_AUTO_OFF_PRESSED = (EqMain, 154, 119, 32, 12);
    EQ_AUTO_ON = (EqMain, 95, 119, 32, 12);
    EQ_AUTO_ON_PRESSED = (EqMain, 213, 119, 32, 12);
    EQ_PRESETS = (EqMain, 224, 164, 44, 12);
    EQ_PRESETS_PRESSED = (EqMain, 224, 176, 44, 12);
    EQ_GRAPH = (EqMain, 0, 294, 113, 19);
    /// One pixel wide: the colour the curve takes at each row.
    EQ_GRAPH_LINE = (EqMain, 115, 294, 1, 19);
    EQ_PREAMP_LINE = (EqMain, 0, 314, 113, 1);
    EQ_THUMB = (EqMain, 0, 164, 11, 11);
    EQ_THUMB_PRESSED = (EqMain, 0, 176, 11, 11);
    /// The equalizer's shade bar, the three looks of each mini slider's
    /// thumb (low, middle, high), and the bar's buttons. The shade
    /// button's resting look is part of the bars themselves.
    EQ_SHADE_BAR_ACTIVE = (EqEx, 0, 0, 275, 14);
    EQ_SHADE_BAR_INACTIVE = (EqEx, 0, 15, 275, 14);
    EQ_SHADE_VOLUME_THUMB_LOW = (EqEx, 1, 30, 3, 7);
    EQ_SHADE_VOLUME_THUMB_MIDDLE = (EqEx, 4, 30, 3, 7);
    EQ_SHADE_VOLUME_THUMB_HIGH = (EqEx, 7, 30, 3, 7);
    EQ_SHADE_BALANCE_THUMB_LEFT = (EqEx, 11, 30, 3, 7);
    EQ_SHADE_BALANCE_THUMB_MIDDLE = (EqEx, 14, 30, 3, 7);
    EQ_SHADE_BALANCE_THUMB_RIGHT = (EqEx, 17, 30, 3, 7);
    EQ_SHADE_BUTTON_PRESSED = (EqEx, 1, 38, 9, 9);
    EQ_UNSHADE_BUTTON_PRESSED = (EqEx, 1, 47, 9, 9);
    EQ_SHADE_CLOSE_BUTTON = (EqEx, 11, 38, 9, 9);
    EQ_SHADE_CLOSE_BUTTON_PRESSED = (EqEx, 11, 47, 9, 9);
}

/// How many steps an equalizer slider's track is drawn in.
pub const EQ_SLIDER_FRAMES: u32 = 28;

/// An equalizer slider's track at a given position, `frame` from 0 (all
/// the way down) to 27: two rows of fourteen, fifteen pixels apart.
pub fn eq_slider_frame(frame: u32) -> Sprite {
    let frame = frame.min(EQ_SLIDER_FRAMES - 1);
    Sprite::new(
        Sheet::EqMain,
        13 + (frame % 14) * 15,
        164 + (frame / 14) * 65,
        14,
        63,
    )
}

/// How many steps the volume and balance sliders are drawn in.
pub const SLIDER_FRAMES: u32 = 28;

/// The volume track at a given fill, `frame` from 0 (silent) to 27 (full).
pub fn volume_frame(frame: u32) -> Sprite {
    Sprite::new(Sheet::Volume, 0, 15 * frame.min(SLIDER_FRAMES - 1), 68, 13)
}

/// The balance track at a given deflection, `frame` from 0 (centred) to 27.
pub fn balance_frame(frame: u32) -> Sprite {
    Sprite::new(Sheet::Balance, 9, 15 * frame.min(SLIDER_FRAMES - 1), 38, 13)
}

/// A digit of the time display from the plain digit sheet.
pub fn digit(value: u32) -> Sprite {
    Sprite::new(Sheet::Numbers, 9 * value.min(9), 0, 9, 13)
}

/// A digit of the time display from the extended sheet.
pub fn digit_ex(value: u32) -> Sprite {
    Sprite::new(Sheet::NumsEx, 9 * value.min(9), 0, 9, 13)
}

/// A cell of the bitmap font: three rows of thirty-one 5x6 glyphs.
pub fn glyph(row: u32, column: u32) -> Sprite {
    Sprite::new(Sheet::Text, 5 * column, 6 * row, 5, 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sheet_has_a_distinct_file_name() {
        let mut stems: Vec<&str> = Sheet::ALL.iter().map(|sheet| sheet.file_stem()).collect();
        stems.sort_unstable();
        stems.dedup();
        assert_eq!(stems.len(), Sheet::ALL.len());
    }

    #[test]
    fn a_sprite_beyond_a_short_sheet_is_clipped_or_dropped() {
        let clipped = POSITION_TRACK.clipped_to(307, 4).unwrap();
        assert_eq!((clipped.width, clipped.height), (248, 4));
        assert!(VOLUME_THUMB.clipped_to(68, 419).is_none());
        assert_eq!(VOLUME_THUMB.clipped_to(68, 433), Some(VOLUME_THUMB));
    }

    #[test]
    fn indexed_sprites_stay_inside_their_sheets() {
        assert_eq!(volume_frame(27).y, 405);
        assert_eq!(volume_frame(99).y, 405);
        assert_eq!(balance_frame(0).x, 9);
        assert_eq!(digit(9).x, 81);
        assert_eq!(digit(12).x, 81);
        assert_eq!(digit_ex(0).sheet, Sheet::NumsEx);
        assert_eq!(
            eq_slider_frame(0),
            Sprite::new(Sheet::EqMain, 13, 164, 14, 63)
        );
        assert_eq!(
            eq_slider_frame(14),
            Sprite::new(Sheet::EqMain, 13, 229, 14, 63)
        );
        assert_eq!(eq_slider_frame(27).x, 13 + 13 * 15);
        assert_eq!(glyph(2, 30), Sprite::new(Sheet::Text, 150, 12, 5, 6));
    }
}
