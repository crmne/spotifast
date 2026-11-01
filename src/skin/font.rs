//! The skin's bitmap font: `text.bmp`, three rows of 5x6 glyphs.
//!
//! The font has capitals only, so lower case maps onto them, and it covers
//! Latin, digits, and a fixed set of punctuation. Accented Latin letters
//! lose their accents rather than turning into question marks; anything the
//! font cannot show at all becomes one.

use super::sprites::{self, Sprite};

/// The blank cell at the end of the first row.
const SPACE: (u32, u32) = (0, 30);

/// The rows of `text.bmp`, in the order the cells appear.
const ROWS: [&str; 3] = [
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ\"@",
    "0123456789\u{2026}.:()-'!_+\\/[]^&%,=$#",
    "\u{c5}\u{d6}\u{c4}?*",
];

/// Where a character sits in the font, as (row, column).
pub fn cell(character: char) -> (u32, u32) {
    let character = fold(character);
    if character == ' ' {
        return SPACE;
    }
    for (row, cells) in ROWS.iter().enumerate() {
        if let Some(column) = cells.chars().position(|cell| cell == character) {
            return (row as u32, column as u32);
        }
    }
    cell('?')
}

/// Whether the font has a real cell for the character, rather than the
/// question mark it falls back to.
pub fn covered(character: char) -> bool {
    // Asked after folding, so a character that folds onto the question mark
    // is covered by it: the full-width one is the same question mark.
    fold(character) == '?' || cell(character) != cell('?')
}

/// The sprite for a character.
pub fn glyph(character: char) -> Sprite {
    let (row, column) = cell(character);
    sprites::glyph(row, column)
}

/// Reduces a character to one the font has a cell for.
fn fold(character: char) -> char {
    // Full-width forms are the ASCII punctuation, digits and letters again,
    // shifted up by 0xFEE0, and a Japanese or Korean release writes a title
    // with them: a star is U+FF0A, a tilde U+FF5E, the brackets U+FF08 and
    // U+FF09. Folding them is the same move as dropping an accent, and it
    // matters more, because one uncovered character sends the whole line to
    // a borrowed face instead of the skin's own font.
    let character = match character {
        '\u{ff01}'..='\u{ff5e}' => ((character as u32 - 0xfee0) as u8) as char,
        '\u{3000}' => ' ',
        other => other,
    };
    match character {
        'a'..='z' => character.to_ascii_uppercase(),
        ' ' | '\t' | '\u{a0}' => ' ',
        '\u{c5}' | '\u{e5}' => '\u{c5}',
        '\u{d6}' | '\u{f6}' => '\u{d6}',
        '\u{c4}' | '\u{e4}' => '\u{c4}',
        '\u{c0}'..='\u{c3}' | '\u{e0}'..='\u{e3}' => 'A',
        '\u{c6}' | '\u{e6}' => 'A',
        '\u{c7}' | '\u{e7}' => 'C',
        '\u{c8}'..='\u{cb}' | '\u{e8}'..='\u{eb}' => 'E',
        '\u{011e}' | '\u{011f}' => 'G',
        '\u{cc}'..='\u{cf}' | '\u{ec}'..='\u{ef}' | '\u{0130}' | '\u{0131}' => 'I',
        '\u{d0}' | '\u{f0}' => 'D',
        '\u{d1}' | '\u{f1}' => 'N',
        '\u{d2}'..='\u{d5}' | '\u{f2}'..='\u{f5}' | '\u{d8}' | '\u{f8}' => 'O',
        '\u{d9}'..='\u{dc}' | '\u{f9}'..='\u{fc}' => 'U',
        '\u{dd}' | '\u{fd}' | '\u{ff}' => 'Y',
        '\u{df}' | '\u{015e}' | '\u{015f}' => 'S',
        '\u{2018}' | '\u{2019}' | '`' => '\'',
        '\u{201c}' | '\u{201d}' => '"',
        '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
        '<' | '{' => '[',
        '>' | '}' => ']',
        '|' => '/',
        '~' => '-',
        ';' => ',',
        _ => character,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skin::sprites::Sheet;

    #[test]
    fn lower_case_shares_the_capitals() {
        assert_eq!(cell('a'), (0, 0));
        assert_eq!(cell('A'), (0, 0));
        assert_eq!(cell('z'), (0, 25));
        assert_eq!(cell(' '), (0, 30));
    }

    #[test]
    fn digits_and_punctuation_are_on_the_second_row() {
        assert_eq!(cell('0'), (1, 0));
        assert_eq!(cell('9'), (1, 9));
        assert_eq!(cell('.'), (1, 11));
        assert_eq!(cell(':'), (1, 12));
        assert_eq!(cell('('), (1, 13));
        assert_eq!(cell('-'), (1, 15));
        assert_eq!(cell('#'), (1, 30));
    }

    #[test]
    fn full_width_forms_fold_to_the_shapes_they_are() {
        assert_eq!(cell('\u{ff0a}'), cell('*'));
        assert_eq!(cell('\u{ff08}'), cell('('));
        assert_eq!(cell('\u{ff09}'), cell(')'));
        assert_eq!(cell('\u{ff21}'), cell('A'));
        assert_eq!(cell('\u{ff41}'), cell('A'));
        assert_eq!(cell('\u{ff10}'), cell('0'));
        assert_eq!(cell('\u{3000}'), cell(' '));
        // The full-width tilde takes the dash the ASCII one already took.
        assert_eq!(cell('\u{ff5e}'), cell('-'));
        // The full-width question mark is the question mark, so it is a cell
        // the font has rather than the fallback for one it does not.
        assert_eq!(cell('\u{ff1f}'), cell('?'));
        assert!(covered('\u{ff1f}'));
    }

    #[test]
    fn a_title_punctuated_full_width_keeps_the_skin_font() {
        // One uncovered character sends the whole line to a borrowed face,
        // so a title written this way used to lose the skin's font outright.
        assert!(
            "\u{ff08}2026\u{ff09}\u{3000}REMIX\u{ff5e}"
                .chars()
                .all(covered)
        );
        // Kana is still not in the font, and still falls back.
        assert!(!"\u{3042}".chars().all(covered));
    }

    #[test]
    fn turkish_titles_keep_the_skin_font() {
        let title = "Cihangir Eryıldız - Tokyo Manifesto (4:25)";
        assert!(title.chars().all(covered));
        assert_eq!(
            title.chars().map(cell).collect::<Vec<_>>(),
            "CIHANGIR ERYILDIZ - TOKYO MANIFESTO (4:25)"
                .chars()
                .map(cell)
                .collect::<Vec<_>>()
        );
        for (letter, base) in [
            ('ı', 'I'),
            ('İ', 'I'),
            ('ğ', 'G'),
            ('Ğ', 'G'),
            ('ş', 'S'),
            ('Ş', 'S'),
        ] {
            assert!(
                covered(letter),
                "{letter} must not select the fallback font"
            );
            assert_eq!(cell(letter), cell(base));
        }
    }

    #[test]
    fn accents_fold_and_the_unknown_becomes_a_question_mark() {
        assert_eq!(cell('\u{e9}'), cell('E'));
        assert_eq!(cell('\u{f1}'), cell('N'));
        assert_eq!(cell('\u{e4}'), (2, 2));
        assert_eq!(cell('\u{c5}'), (2, 0));
        assert_eq!(cell('\u{3042}'), (2, 3));
        assert_eq!(cell('?'), (2, 3));
        assert_eq!(cell('\u{2019}'), cell('\''));
    }

    #[test]
    fn a_glyph_is_a_five_by_six_cell_of_the_text_sheet() {
        let sprite = glyph('B');
        assert_eq!(sprite.sheet, Sheet::Text);
        assert_eq!(
            (sprite.x, sprite.y, sprite.width, sprite.height),
            (5, 0, 5, 6)
        );
        assert_eq!(glyph('*').x, 20);
        assert_eq!(glyph('*').y, 12);
    }
}
