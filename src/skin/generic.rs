//! Winamp's generic window: the frame `gen.bmp` draws around anything that is
//! not one of the player's own windows, which is how Winamp 5 framed MilkDrop.
//!
//! The title bar is six 25-pixel tiles, on two rows for focused and not. Each
//! side is a 29-pixel tile above a 24-pixel foot, and the bottom is two
//! 125-pixel ends with a tile between them. Letters for the title sit in two
//! rows of varying widths, told apart by the sheet's background colour.
//! `genex.bmp` holds, among buttons for dialogs that have no place here, the
//! colours of Winamp's generic windows in the top row of pixels.

use super::layout::Area;
use super::sprites::{Sheet, Sprite};
use super::{Bitmap, Rgb};

pub const TOP_HEIGHT: u32 = 20;
pub const BOTTOM_HEIGHT: u32 = 14;
pub const LEFT_WIDTH: u32 = 11;
pub const RIGHT_WIDTH: u32 = 8;
/// The smallest a generic window goes, the size of Winamp's main window.
pub const MIN_WIDTH: u32 = 275;
pub const MIN_HEIGHT: u32 = 116;
/// The close button on the title bar's last tile, and how it looks pressed.
pub const CLOSE_PRESSED: Sprite = Sprite::new(Sheet::Gen, 148, 42, 9, 9);
/// Where the title's letters start, down from the window's top.
pub const TITLE_Y: u32 = 4;
/// Blank pixels before and after the title, on the fill behind it.
const TITLE_PADDING: u32 = 7;

const TOP_TILE: u32 = 25;
/// The columns of the title bar's tiles, in `gen.bmp`.
const TOP_LEFT: u32 = 0;
const TOP_LEFT_END: u32 = 26;
const TOP_CENTER: u32 = 52;
const TOP_RIGHT_END: u32 = 78;
const TOP_FILL: u32 = 104;
const TOP_RIGHT: u32 = 130;
const SIDE_TILE: u32 = 29;
const SIDE_FOOT: u32 = 24;
const MIDDLE_LEFT: Sprite = Sprite::new(Sheet::Gen, 127, 42, LEFT_WIDTH, SIDE_TILE);
const MIDDLE_LEFT_FOOT: Sprite = Sprite::new(Sheet::Gen, 158, 42, LEFT_WIDTH, SIDE_FOOT);
const MIDDLE_RIGHT: Sprite = Sprite::new(Sheet::Gen, 139, 42, RIGHT_WIDTH, SIDE_TILE);
const MIDDLE_RIGHT_FOOT: Sprite = Sprite::new(Sheet::Gen, 170, 42, RIGHT_WIDTH, SIDE_FOOT);
const BOTTOM_END: u32 = 125;
const BOTTOM_LEFT: Sprite = Sprite::new(Sheet::Gen, 0, 42, BOTTOM_END, BOTTOM_HEIGHT);
const BOTTOM_RIGHT: Sprite = Sprite::new(Sheet::Gen, 0, 57, BOTTOM_END, BOTTOM_HEIGHT);
const BOTTOM_FILL: Sprite = Sprite::new(Sheet::Gen, 127, 72, TOP_TILE, BOTTOM_HEIGHT);
/// The rows the title's letters are in: focused, then not.
const LETTER_ROWS: [u32; 2] = [88, 96];
const LETTER_HEIGHT: u32 = 7;
/// The window background among `genex.bmp`'s colours.
const GENEX_WINDOW_BACKGROUND: u32 = 52;

/// One piece of a frame: what to draw, and where its top left goes.
pub type Piece = (Sprite, u32, u32);

/// Tiles a run from `x`, `y` across `width`, cutting the last tile short, or
/// the first when `from_right` keeps the tiling flush with the far end.
fn across(pieces: &mut Vec<Piece>, tile: Sprite, x: u32, y: u32, width: u32, from_right: bool) {
    let mut done = 0;
    while done < width {
        let left = width - done;
        let part = if done == 0 && from_right {
            left % tile.width
        } else {
            0
        };
        let cut = if part != 0 {
            Sprite {
                x: tile.x + tile.width - part,
                width: part,
                ..tile
            }
        } else {
            Sprite {
                width: tile.width.min(left),
                ..tile
            }
        };
        pieces.push((cut, x + done, y));
        done += cut.width;
    }
}

/// Tiles a run from `y` down `height`, cutting the last tile short.
fn down(pieces: &mut Vec<Piece>, tile: Sprite, x: u32, y: u32, height: u32) {
    let mut done = 0;
    while done < height {
        let cut = Sprite {
            height: tile.height.min(height - done),
            ..tile
        };
        pieces.push((cut, x, y + done));
        done += cut.height;
    }
}

/// Every piece of the frame of a window this big, with a title this wide
/// (see [`title_width`]) centred on its bar. Sizes below the minimum are
/// taken as the minimum.
pub fn frame(width: u32, height: u32, title: u32, focused: bool) -> Vec<Piece> {
    let (width, height) = (width.max(MIN_WIDTH), height.max(MIN_HEIGHT));
    let row = if focused { 0 } else { 21 };
    let top = |column| Sprite::new(Sheet::Gen, column, row, TOP_TILE, TOP_HEIGHT);
    let title = title.min(width - 4 * TOP_TILE);
    let spare = width - 4 * TOP_TILE - title;
    let (left_fill, right_fill) = (spare / 2, spare - spare / 2);

    let mut pieces = vec![(top(TOP_LEFT), 0, 0)];
    across(&mut pieces, top(TOP_FILL), TOP_TILE, 0, left_fill, false);
    let mut x = TOP_TILE + left_fill;
    pieces.push((top(TOP_LEFT_END), x, 0));
    x += TOP_TILE;
    across(&mut pieces, top(TOP_CENTER), x, 0, title, false);
    x += title;
    pieces.push((top(TOP_RIGHT_END), x, 0));
    x += TOP_TILE;
    across(&mut pieces, top(TOP_FILL), x, 0, right_fill, true);
    pieces.push((top(TOP_RIGHT), width - TOP_TILE, 0));

    let middle = height - TOP_HEIGHT - BOTTOM_HEIGHT;
    let tiled = middle.saturating_sub(SIDE_FOOT);
    let foot = TOP_HEIGHT + tiled;
    let right = width - RIGHT_WIDTH;
    down(&mut pieces, MIDDLE_LEFT, 0, TOP_HEIGHT, tiled);
    down(&mut pieces, MIDDLE_RIGHT, right, TOP_HEIGHT, tiled);
    pieces.push((MIDDLE_LEFT_FOOT, 0, foot));
    pieces.push((MIDDLE_RIGHT_FOOT, right, foot));

    let bottom = height - BOTTOM_HEIGHT;
    pieces.push((BOTTOM_LEFT, 0, bottom));
    across(
        &mut pieces,
        BOTTOM_FILL,
        BOTTOM_END,
        bottom,
        width - 2 * BOTTOM_END,
        false,
    );
    pieces.push((BOTTOM_RIGHT, width - BOTTOM_END, bottom));
    pieces
}

/// The part of a window this big that the frame leaves for what is in it.
pub fn inside(width: u32, height: u32) -> Area {
    let (width, height) = (width.max(MIN_WIDTH), height.max(MIN_HEIGHT));
    Area::new(
        LEFT_WIDTH,
        TOP_HEIGHT,
        width - LEFT_WIDTH - RIGHT_WIDTH,
        height - TOP_HEIGHT - BOTTOM_HEIGHT,
    )
}

/// The close button of a window this wide, on the title bar's last tile.
pub fn close_button(width: u32) -> Area {
    Area::new(width.max(MIN_WIDTH) - 11, 3, 9, 9)
}

/// Our full screen button, on the same tile: Winamp's frame has no such
/// thing, so it is drawn in the colour of the frame's own title.
pub fn full_screen_button(width: u32) -> Area {
    Area::new(width.max(MIN_WIDTH) - 22, 3, 9, 9)
}

/// The pieces of the button's four corners, inside its nine-pixel square, as
/// `(x, y, width, height)`.
pub const FULL_SCREEN_GLYPH: [(u32, u32, u32, u32); 8] = [
    (1, 1, 3, 1),
    (1, 1, 1, 3),
    (5, 1, 3, 1),
    (7, 1, 1, 3),
    (1, 7, 3, 1),
    (1, 5, 1, 3),
    (5, 7, 3, 1),
    (7, 5, 1, 3),
];

/// The colour of the title's letters: the brightest in the first of them.
pub fn title_colour(gen_sheet: &Bitmap, focused: bool) -> Rgb {
    let y = LETTER_ROWS[usize::from(!focused)];
    (1..gen_sheet.width.min(9))
        .flat_map(|x| (y..y + LETTER_HEIGHT).map(move |y| (x, y)))
        .filter_map(|(x, y)| gen_sheet.pixel(x, y))
        .map(|[r, g, b, _]| [r, g, b])
        .max_by_key(|[r, g, b]| u16::from(*r) + u16::from(*g) + u16::from(*b))
        .unwrap_or([255, 255, 255])
}

/// The corner that resizes a window this big.
pub fn grip(width: u32, height: u32) -> Area {
    let (width, height) = (width.max(MIN_WIDTH), height.max(MIN_HEIGHT));
    Area::new(width - 20, height - 20, 20, 20)
}

/// The letters A to Z, as they are in this `gen.bmp`. Each runs until the
/// sheet's background colour shows again; widths differ from skin to skin.
pub fn letters(gen_sheet: &Bitmap, focused: bool) -> Vec<Sprite> {
    let y = LETTER_ROWS[usize::from(!focused)];
    let background = gen_sheet.pixel(0, y);
    let mut x = 1;
    (b'A'..=b'Z')
        .map(|_| {
            let start = x;
            while x < gen_sheet.width && gen_sheet.pixel(x, y) != background {
                x += 1;
            }
            let letter = Sprite::new(Sheet::Gen, start, y, x - start, LETTER_HEIGHT);
            x = (x + 1).min(gen_sheet.width);
            letter
        })
        .collect()
}

/// The sprite for a character of a title; only letters are in the sheet, so
/// anything else is a gap.
fn letter(letters: &[Sprite], character: char) -> Option<Sprite> {
    let at = character.to_ascii_uppercase();
    at.is_ascii_uppercase()
        .then(|| letters.get(usize::from(at as u8 - b'A')).copied())
        .flatten()
}

/// How wide a title is on the bar, with the blank either side of it.
pub fn title_width(letters: &[Sprite], title: &str) -> u32 {
    TITLE_PADDING + title_pieces(letters, title, 0).1
}

/// The letters of a title in a row from `x`, and how far they run.
fn title_pieces(letters: &[Sprite], title: &str, x: u32) -> (Vec<Piece>, u32) {
    let mut pieces = Vec::new();
    let mut run = 0;
    for character in title.chars() {
        match letter(letters, character) {
            Some(sprite) => {
                pieces.push((sprite, x + run, TITLE_Y));
                run += sprite.width;
            }
            None => run += 5,
        }
    }
    (pieces, run)
}

/// The letters of a title, centred on a window this wide.
pub fn title(letters: &[Sprite], title: &str, width: u32) -> Vec<Piece> {
    let (_, run) = title_pieces(letters, title, 0);
    let x = width.max(MIN_WIDTH).saturating_sub(run) / 2;
    title_pieces(letters, title, x).0
}

/// The background colour of a generic window, from `genex.bmp`.
pub fn window_background(genex: &Bitmap) -> Option<Rgb> {
    let [r, g, b, _] = genex.pixel(GENEX_WINDOW_BACKGROUND, 0)?;
    Some([r, g, b])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame covers the window's edge exactly: every pixel outside the
    /// picture once, nothing inside it, nothing beyond the window.
    #[test]
    fn the_frame_covers_the_edge_and_leaves_the_middle() {
        for (width, height) in [(275, 116), (276, 117), (300, 200), (641, 503), (1000, 700)] {
            let mut covered = vec![0u8; (width * height) as usize];
            for (sprite, x, y) in frame(width, height, 60, true) {
                for row in y..y + sprite.height {
                    for column in x..x + sprite.width {
                        assert!(column < width && row < height, "past the edge");
                        covered[(row * width + column) as usize] += 1;
                    }
                }
            }
            let hole = inside(width, height);
            for y in 0..height {
                for x in 0..width {
                    let expected = u8::from(!hole.contains(x, y));
                    assert_eq!(
                        covered[(y * width + x) as usize],
                        expected,
                        "at {x},{y} of {width}x{height}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_chrome_takes_what_winamp_took() {
        assert_eq!(inside(275, 116), Area::new(11, 20, 256, 82));
        assert_eq!(inside(100, 50), inside(275, 116), "the minimum holds");
    }

    #[test]
    fn a_window_out_of_focus_uses_the_lower_row() {
        let rows = |focused| {
            frame(300, 200, 40, focused)
                .iter()
                .filter(|(_, _, y)| *y == 0)
                .map(|(sprite, _, _)| sprite.y)
                .max()
        };
        assert_eq!(rows(true), Some(0));
        assert_eq!(rows(false), Some(21));
    }

    /// A little sheet whose letters have the given widths, on a background.
    fn sheet(widths: &[u32]) -> Bitmap {
        let width = 200;
        let mut rgba = vec![0; (width * 109 * 4) as usize];
        for row in 0..109 {
            let mut x = 1;
            for letter in widths {
                for column in x..x + letter {
                    rgba[((row * width + column) * 4) as usize] = 255;
                }
                x += letter + 1;
            }
        }
        Bitmap {
            width,
            height: 109,
            rgba,
        }
    }

    #[test]
    fn letters_are_as_wide_as_the_skin_drew_them() {
        let bitmap = sheet(&[5, 6, 1, 7]);
        let found = letters(&bitmap, true);
        assert_eq!(found.len(), 26);
        let spans: Vec<(u32, u32)> = found.iter().take(4).map(|s| (s.x, s.width)).collect();
        assert_eq!(spans, [(1, 5), (7, 6), (14, 1), (16, 7)]);
        assert_eq!(found[0].y, 88);
        assert_eq!(letters(&bitmap, false)[0].y, 96);
        assert_eq!(found[3].height, 7);
    }

    /// Winamp's sheets run short in places; that is a blank letter, not a
    /// read past the end.
    #[test]
    fn a_short_sheet_has_empty_letters() {
        let bitmap = Bitmap {
            width: 10,
            height: 4,
            rgba: vec![0; 160],
        };
        assert!(
            letters(&bitmap, true)
                .iter()
                .all(|letter| letter.width == 0)
        );
    }

    #[test]
    fn a_title_is_centred_and_counts_its_width() {
        let bitmap = sheet(&[5; 26]);
        let found = letters(&bitmap, true);
        assert_eq!(title_width(&found, "AB"), 17);
        assert_eq!(title_width(&found, "A B"), 22, "a space has a width too");
        let placed = title(&found, "AB", 300);
        assert_eq!(placed.len(), 2);
        assert_eq!((placed[0].1, placed[1].1), (145, 150));
        assert_eq!(placed[0].2, TITLE_Y);
    }

    #[test]
    fn genex_gives_the_window_its_background() {
        let mut bitmap = sheet(&[]);
        bitmap.rgba[52 * 4..52 * 4 + 3].copy_from_slice(&[1, 2, 3]);
        assert_eq!(window_background(&bitmap), Some([1, 2, 3]));
        bitmap.width = 10;
        assert_eq!(window_background(&bitmap), None, "too narrow a sheet");
    }
}
