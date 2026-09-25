//! Right-to-left text support for egui.
//!
//! The shared egui fork (crmne/egui apps-0.36, emilk/egui#8577) splits font
//! runs where the Unicode bidi level changes and shapes each in its resolved
//! direction, so letters join and brackets mirror, but it still places the
//! shaped runs left to right in logical order. [`reorder`] moves them into
//! visual order after layout. The glyph vector stays in logical order with
//! each glyph's `rtl` flag set, so carets and hit-testing follow the text.
//!
//! [`layout`] handles wrapped and truncated text one row at a time and
//! places ellipses at the left reading edge.

use std::sync::Arc;

use egui::emath::GuiRounding as _;
use egui::epaint::text::{Glyph, Row};
use egui::epaint::{Mesh, Vec2};
use egui::text::LayoutJob;
use egui::{Align, Align2, Color32, FontId, Galley, Painter, Pos2, Rect, pos2};
use unicode_bidi::{BidiClass, BidiInfo, Level, bidi_class};

/// The character that marks a cut.
pub const ELLIPSIS: char = '\u{2026}';

/// Whether the text reads right to left: decided by its first strong
/// character, the way the bidi algorithm decides a paragraph's direction.
/// ASCII never does, and most text is ASCII, so that check comes first and
/// costs nothing per frame.
pub fn is_rtl(text: &str) -> bool {
    if text.is_ascii() {
        return false;
    }
    for character in text.chars() {
        match bidi_class(character) {
            BidiClass::L => return false,
            BidiClass::R | BidiClass::AL => return true,
            _ => {}
        }
    }
    false
}

/// Whether the text holds a right-to-left letter anywhere, so its runs need
/// reordering even when the paragraph reads left to right.
fn has_rtl(text: &str) -> bool {
    !text.is_ascii() && text.chars().any(is_strong_rtl)
}

/// The edge the text should hug.
pub fn halign_for(text: &str) -> Align {
    if is_rtl(text) {
        Align::RIGHT
    } else {
        Align::LEFT
    }
}

/// One line of `text`, unwrapped, in visual order.
pub fn layout_line(
    painter: &Painter,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
) -> Arc<Galley> {
    let mut galley = painter.layout_no_wrap(text.into(), font, color);
    reorder(&mut galley);
    galley
}

/// Lays `text` out within `wrap_width`, on at most `max_rows` rows, with
/// `overflow` marking a cut, in reading order for either direction. The
/// galley's `halign` says which edge to anchor it at; [`galley_pos`] does
/// that for a painter, and `egui::Label` does it on its own.
pub fn layout(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    wrap_width: f32,
    max_rows: usize,
    overflow: Option<char>,
) -> Arc<Galley> {
    if !has_rtl(text) {
        let mut job = LayoutJob::simple(text.to_owned(), font, color, wrap_width);
        job.wrap.max_rows = max_rows;
        job.wrap.break_anywhere = false;
        job.wrap.overflow_character = overflow;
        return painter.layout_job(job);
    }
    // The rows are found here, whole words at a time, and each becomes a
    // paragraph of its own, so a paragraph's first row still holds its first
    // words and the cut lands where it is read last. Measuring words is the
    // dependable way to know what fits: epaint shapes them itself.
    let width = |piece: &str| {
        painter
            .layout_no_wrap(piece.to_owned(), font.clone(), color)
            .size()
            .x
    };
    let rows = break_rows(text, wrap_width, max_rows, overflow, width);
    let mut job = LayoutJob::simple(rows.join("\n"), font, color, f32::INFINITY);
    if text.split('\n').any(is_rtl) {
        job.halign = Align::RIGHT;
    }
    let mut galley = painter.layout_job(job);
    reorder(&mut galley);
    galley
}

/// Fills rows with whole words up to `wrap_width`, at most `max_rows` of
/// them, and ends a cut with `overflow`. The rows come back in logical
/// order, one string each.
fn break_rows(
    text: &str,
    wrap_width: f32,
    max_rows: usize,
    overflow: Option<char>,
    width: impl Fn(&str) -> f32,
) -> Vec<String> {
    let max_rows = max_rows.max(1);
    let space = width(" ");
    let mut rows: Vec<(String, f32)> = Vec::new();
    let mut row = (String::new(), 0.0_f32);
    let mut cut = false;
    'paragraphs: for (index, paragraph) in text.split('\n').enumerate() {
        if index > 0 {
            if rows.len() + 1 >= max_rows {
                cut = true;
                break;
            }
            rows.push(std::mem::take(&mut row));
        }
        for word in paragraph.split_whitespace() {
            let word_width = width(word);
            if !row.0.is_empty() && row.1 + space + word_width > wrap_width {
                if rows.len() + 1 >= max_rows {
                    cut = true;
                    break 'paragraphs;
                }
                rows.push(std::mem::take(&mut row));
            }
            if !row.0.is_empty() {
                row.0.push(' ');
                row.1 += space;
            }
            row.0.push_str(word);
            row.1 += word_width;
        }
    }
    rows.push(row);
    // The last row can be wider than the column even when nothing was cut:
    // the first word of a row is taken whatever it measures, because a row
    // has to hold something, and a word longer than the column has nowhere
    // to break. `layout` hands the finished rows to epaint with no wrap
    // width of its own -- the fitting is meant to have happened here -- so a
    // row left too wide is drawn straight past the edge it was given.
    if let Some(mark) = overflow {
        let last = rows.last_mut().expect("one row at least");
        if cut || last.1 > wrap_width {
            // Each candidate is measured as `layout` will draw it: shaped
            // whole, mark included. Letters join and ligate, so what is left
            // of a word cannot be worked out from the widths of the letters
            // taken away. Reordering moves shaped runs without changing their
            // widths, so logical order measures the same.
            let drawn = |row: &str| width(&format!("{row}{mark}"));
            while drawn(&last.0) > wrap_width
                && let Some(at) = last.0.rfind(' ')
            {
                last.0.truncate(at);
            }
            // A single word with no space left to give up: letters go
            // instead, which is the only way the ellipsis can mean anything.
            // A piece is kept only once it has been measured to fit, so what
            // is drawn fits even where a shorter piece joins into a wider
            // form.
            if !last.0.is_empty() && drawn(&last.0) > wrap_width {
                let cuts = cuts(&last.0);
                let (mut fits, mut over) = (0, cuts.len());
                while over - fits > 1 {
                    let middle = (fits + over) / 2;
                    if drawn(&last.0[..cuts[middle]]) <= wrap_width {
                        fits = middle;
                    } else {
                        over = middle;
                    }
                }
                last.0.truncate(cuts[fits]);
            }
            last.0.push(mark);
        }
    }
    rows.into_iter().map(|(text, _)| text).collect()
}

/// Where `word` can be cut, as byte offsets from its start up to but not
/// including its end: never before a mark that rides on the letter ahead of
/// it, and never after a joiner.
fn cuts(word: &str) -> Vec<usize> {
    let mut previous = None;
    word.char_indices()
        .filter(|&(at, character)| {
            let joined = previous == Some('\u{200d}');
            previous = Some(character);
            let rides = matches!(bidi_class(character), BidiClass::NSM | BidiClass::BN);
            at == 0 || !(rides || joined)
        })
        .map(|(at, _)| at)
        .collect()
}

/// Where to paint a galley from [`layout`] so that it sits inside `rect`:
/// its left edge, or its right edge for right-to-left text.
pub fn galley_pos(rect: Rect, galley: &Galley) -> Pos2 {
    match galley.job.halign {
        Align::RIGHT => rect.right_top(),
        Align::Center => rect.center_top(),
        _ => rect.left_top(),
    }
}

/// Paints one line of text centred on `y`, starting at `left`, or ending at
/// `right` when it reads right to left. Returns the painted rect.
pub fn paint_line(
    painter: &Painter,
    left: f32,
    right: f32,
    y: f32,
    text: &str,
    font: FontId,
    color: Color32,
) -> Rect {
    if !has_rtl(text) {
        return painter.text(pos2(left, y), Align2::LEFT_CENTER, text, font, color);
    }
    let galley = layout_line(painter, text, font, color);
    let rect = if is_rtl(text) {
        Align2::RIGHT_CENTER.anchor_size(pos2(right, y), galley.size())
    } else {
        Align2::LEFT_CENTER.anchor_size(pos2(left, y), galley.size())
    };
    painter.galley(rect.min, galley, color);
    rect
}

/// Puts each row of a galley laid out in logical order into visual order.
///
/// Every row is resolved with the bidi levels of its own paragraph. Shaped
/// runs move as blocks, so letters the fork already shaped right to left keep
/// their order; the glyph vector returns to logical order afterwards, with
/// right-to-left glyphs flagged for epaint's cursor code. Galleys with no
/// right-to-left letter are left untouched and never copied.
///
/// Only for galleys without decorations and without an epaint overflow
/// character: [`layout`] cuts rows itself, so neither happens here.
pub fn reorder(galley: &mut Arc<Galley>) {
    if !has_rtl(galley.text())
        || galley
            .rows
            .iter()
            .all(|placed| placed.row.glyphs.is_empty() || already_visual(&placed.row.glyphs))
    {
        return;
    }
    let galley = Arc::make_mut(galley);
    let text = galley.job.text.clone();
    let pixels_per_point = galley.pixels_per_point;
    for placed in &mut galley.rows {
        reorder_row(Arc::make_mut(&mut placed.row), &text, pixels_per_point);
    }
    refresh_bounds(galley);
}

fn reorder_row(row: &mut Row, text: &str, pixels_per_point: f32) {
    if row.glyphs.is_empty() || already_visual(&row.glyphs) {
        return;
    }
    let Some((start, end)) = line_span(&row.glyphs, text) else {
        return;
    };
    let (levels, end) = line_levels(text, start, end);
    let line = &text[start..end];
    let visual_of_logical = visual_indices(&levels);
    let char_at_byte = char_starts(line);
    let visual_key = |glyph: &Glyph| {
        (glyph.cluster as usize)
            .checked_sub(start)
            .and_then(|offset| char_at_byte.get(offset).copied())
            .map(|logical| visual_of_logical.get(logical).copied().unwrap_or(logical))
    };

    let visual_keys: Vec<usize> = row
        .glyphs
        .iter()
        .map(|glyph| visual_key(glyph).unwrap_or(usize::MAX))
        .collect();
    let mut atoms: Vec<Atom> = split_atoms(&row.glyphs, &visual_keys)
        .into_iter()
        .filter_map(|glyphs| {
            let slice = &row.glyphs[glyphs.clone()];
            let key = slice.iter().filter_map(visual_key).min()?;
            let min_x = slice
                .iter()
                .map(|glyph| glyph.pos.x)
                .fold(f32::INFINITY, f32::min);
            // A run is as wide as its shaped advances, the measure epaint
            // gives the row. Glyph positions sit on whole pixels, and the
            // zero-width stand-ins after a ligature such as لا sit past its
            // advance, so their extent is up to a pixel wider: a row cut to
            // fit its column grew past it (Windows' Arabic face, 18 pt).
            let width = slice.iter().map(|glyph| glyph.advance_width).sum();
            min_x.is_finite().then_some(Atom {
                glyphs,
                key,
                min_x,
                width,
            })
        })
        .collect();
    atoms.sort_by(|a, b| a.key.cmp(&b.key).then(a.glyphs.start.cmp(&b.glyphs.start)));

    let mut cursor = 0.0_f32;
    for atom in &atoms {
        // Both ends on whole physical pixels. epaint rasterizes each glyph
        // for its place on the pixel grid; moving the quad by a fraction
        // left the moved runs between pixels, blurred (0.21 px at 133%).
        let delta = fastframe_text::snap_to_pixels(cursor, pixels_per_point)
            - fastframe_text::snap_to_pixels(atom.min_x, pixels_per_point);
        if delta.abs() > 0.01 {
            for glyph in &mut row.glyphs[atom.glyphs.clone()] {
                glyph.pos.x += delta;
                shift_glyph_mesh(&mut row.visuals.mesh, glyph, egui::vec2(delta, 0.0));
            }
        }
        cursor += atom.width;
    }

    for glyph in &mut row.glyphs {
        let logical = (glyph.cluster as usize)
            .checked_sub(start)
            .and_then(|offset| char_at_byte.get(offset).copied());
        glyph.rtl = logical
            .and_then(|index| levels.get(index))
            .is_some_and(|level| level.is_rtl());
    }
    let mut order: Vec<usize> = (0..row.glyphs.len()).collect();
    order.sort_by(|&left, &right| {
        row.glyphs[left]
            .cluster
            .cmp(&row.glyphs[right].cluster)
            .then(left.cmp(&right))
    });
    let glyphs = std::mem::take(&mut row.glyphs);
    row.glyphs = order.into_iter().map(|index| glyphs[index]).collect();
    repack_glyph_vertices(row);
    // The advances add up to the width epaint measured, which it rounded to
    // the interface grid (1/32 point) after summing in pixels. Summed here in
    // points, a width that falls on a rounding midpoint can land on the other
    // side of it and round a step up (198.046875 against epaint's 198.046844,
    // Arabic at 13 pt and 133%): the same width, not a wider row. Only a row
    // whose measured width left out a trailing space, now moved inside it,
    // grows, and a space is many grid steps wide.
    if cursor > row.size.x + egui::emath::GUI_ROUNDING {
        row.size.x = cursor.round_ui();
    }
}

/// Glyphs that move together, and where they sat before.
struct Atom {
    glyphs: std::ops::Range<usize>,
    key: usize,
    min_x: f32,
    width: f32,
}

/// True when a previous pass already stored logical order and bidi levels.
fn already_visual(glyphs: &[Glyph]) -> bool {
    glyphs.iter().any(|glyph| glyph.rtl)
        && glyphs
            .windows(2)
            .all(|pair| pair[0].cluster <= pair[1].cluster)
}

/// The byte range of `text` a row's glyphs cover.
fn line_span(glyphs: &[Glyph], text: &str) -> Option<(usize, usize)> {
    let start = glyphs.iter().map(|glyph| glyph.cluster as usize).min()?;
    if start > text.len() {
        return None;
    }
    let mut end = start;
    for glyph in glyphs {
        let byte = glyph.cluster as usize;
        let next = text.get(byte..)?.chars().next()?.len_utf8();
        end = end.max(byte + next);
    }
    (end <= text.len()).then_some((start, end))
}

/// Levels of the characters in `text[start..end]`, with the end clipped to
/// their paragraph. Levels come from the whole paragraph, so a row keeps its
/// paragraph's base direction instead of guessing one from its first word.
fn line_levels(text: &str, start: usize, end: usize) -> (Vec<Level>, usize) {
    let paragraph_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let paragraph_end = text[start..].find('\n').map_or(text.len(), |at| start + at);
    let end = end.min(paragraph_end);
    let info = BidiInfo::new(&text[paragraph_start..paragraph_end], None);
    let line = start - paragraph_start..end - paragraph_start;
    let mut by_byte = vec![Level::ltr(); line.len()];
    for paragraph in &info.paragraphs {
        let overlap = paragraph.range.start.max(line.start)..paragraph.range.end.min(line.end);
        if overlap.is_empty() {
            continue;
        }
        let levels = info.reordered_levels(paragraph, overlap.clone());
        by_byte[overlap.start - line.start..overlap.end - line.start]
            .copy_from_slice(&levels[overlap]);
    }
    let levels = text[start..end]
        .char_indices()
        .map(|(byte, _)| by_byte[byte])
        .collect();
    (levels, end)
}

/// The character index at each byte offset of `text`.
fn char_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0; text.len() + 1];
    for (index, (byte, _)) in text.char_indices().enumerate() {
        starts[byte] = index;
    }
    starts
}

/// The visual position of each logical character.
fn visual_indices(levels: &[Level]) -> Vec<usize> {
    let map = BidiInfo::reorder_visual(levels);
    let mut logical_to_visual = vec![0; map.len()];
    for (visual, &logical) in map.iter().enumerate() {
        if logical < logical_to_visual.len() {
            logical_to_visual[logical] = visual;
        }
    }
    logical_to_visual
}

/// Shaped runs to move as a block.
///
/// A descending cluster sequence is one font run that harfrust already shaped
/// right to left. Splitting it would reverse Arabic letters a second time.
/// A single Hebrew letter has no descent, so it stays its own run and can
/// still move relative to the neighbouring space.
fn split_atoms(glyphs: &[Glyph], visual_keys: &[usize]) -> Vec<std::ops::Range<usize>> {
    let mut atoms = Vec::new();
    let mut index = 0;
    while index < glyphs.len() {
        if let Some(end) = rtl_run_end(glyphs, index) {
            atoms.push(index..end);
            index = end;
            continue;
        }
        if is_strong_rtl(glyphs[index].chr) {
            let end = same_cluster_end(glyphs, index);
            atoms.push(index..end);
            index = end;
            continue;
        }
        // Keep a left-to-right run together only while its visual order
        // matches the buffer. A space before a number is earlier in the
        // buffer and later on screen, so it has to move on its own.
        let start = index;
        let mut previous = visual_keys.get(index).copied().unwrap_or(usize::MAX);
        index += 1;
        while index < glyphs.len()
            && rtl_run_end(glyphs, index).is_none()
            && !is_strong_rtl(glyphs[index].chr)
        {
            let key = visual_keys.get(index).copied().unwrap_or(usize::MAX);
            if key < previous {
                break;
            }
            previous = key;
            index += 1;
        }
        atoms.push(start..index);
    }
    atoms
}

/// End of the shaped cluster starting at `start`.
///
/// A ligature such as لا is one glyph followed by zero-width continuation
/// glyphs for the letters it absorbed. Those carry their own letters' byte
/// offsets, which are higher than the ligature's, so they belong to the
/// cluster before them rather than breaking a descending right-to-left run.
fn same_cluster_end(glyphs: &[Glyph], start: usize) -> usize {
    let mut end = start + 1;
    while end < glyphs.len()
        && (glyphs[end].cluster == glyphs[start].cluster || is_continuation(&glyphs[end]))
    {
        end += 1;
    }
    end
}

/// A zero-width stand-in epaint emits for a character its shaped cluster covers.
fn is_continuation(glyph: &Glyph) -> bool {
    glyph.advance_width == 0.0 && glyph.uv_rect.is_nothing()
}

fn rtl_run_end(glyphs: &[Glyph], start: usize) -> Option<usize> {
    let mut previous = start;
    let mut end = same_cluster_end(glyphs, start);
    if end >= glyphs.len() || glyphs[end].cluster >= glyphs[previous].cluster {
        return None;
    }
    while end < glyphs.len() && glyphs[end].cluster < glyphs[previous].cluster {
        previous = end;
        end = same_cluster_end(glyphs, end);
    }
    Some(end)
}

fn refresh_bounds(galley: &mut Galley) {
    let mut rect: Option<Rect> = None;
    let mut mesh_bounds: Option<Rect> = None;
    for placed in &mut galley.rows {
        let row = Arc::make_mut(&mut placed.row);
        row.visuals.mesh_bounds = row.visuals.mesh.calc_bounds();
        let row_rect = Rect::from_min_size(placed.pos, row.size);
        rect = Some(rect.map_or(row_rect, |rect| rect.union(row_rect)));
        let moved = row.visuals.mesh_bounds.translate(placed.pos.to_vec2());
        mesh_bounds = Some(mesh_bounds.map_or(moved, |bounds| bounds.union(moved)));
    }
    if let Some(rect) = rect {
        galley.rect = rect;
    }
    if let Some(bounds) = mesh_bounds {
        galley.mesh_bounds = bounds;
    }
}

fn shift_glyph_mesh(mesh: &mut Mesh, glyph: &Glyph, delta: Vec2) {
    if glyph.uv_rect.is_nothing() || delta == Vec2::ZERO {
        return;
    }
    let start = glyph.first_vertex as usize;
    let end = (start + 4).min(mesh.vertices.len());
    for vertex in &mut mesh.vertices[start..end] {
        vertex.pos += delta;
    }
}

/// Packs glyph quads into logical order so selection vertex ranges stay
/// contiguous.
fn repack_glyph_vertices(row: &mut Row) {
    let range = row.visuals.glyph_vertex_range.clone();
    if range.start > range.end || range.end > row.visuals.mesh.vertices.len() {
        return;
    }
    let mut packed = Vec::new();
    let mut remap = vec![u32::MAX; row.visuals.mesh.vertices.len()];
    for (index, slot) in remap.iter_mut().enumerate().take(range.start) {
        *slot = index as u32;
    }
    for glyph in &mut row.glyphs {
        let source = glyph.first_vertex as usize;
        let count = if glyph.uv_rect.is_nothing() { 0 } else { 4 };
        glyph.first_vertex = (range.start + packed.len()) as u32;
        if count == 0 || source.saturating_add(count) > row.visuals.mesh.vertices.len() {
            continue;
        }
        for offset in 0..count {
            remap[source + offset] = (range.start + packed.len()) as u32;
            packed.push(row.visuals.mesh.vertices[source + offset]);
        }
    }
    let shift = packed.len() as i64 - (range.end - range.start) as i64;
    for (index, slot) in remap.iter_mut().enumerate().skip(range.end) {
        *slot = (index as i64 + shift) as u32;
    }
    let mut vertices =
        Vec::with_capacity(range.start + packed.len() + remap.len().saturating_sub(range.end));
    vertices.extend_from_slice(&row.visuals.mesh.vertices[..range.start]);
    vertices.extend(packed);
    vertices.extend_from_slice(&row.visuals.mesh.vertices[range.end..]);
    row.visuals.mesh.vertices = vertices;
    for index in &mut row.visuals.mesh.indices {
        if let Some(mapped) = remap.get(*index as usize)
            && *mapped != u32::MAX
        {
            *index = *mapped;
        }
    }
    let glyph_len = row
        .glyphs
        .iter()
        .map(|glyph| usize::from(!glyph.uv_rect.is_nothing()) * 4)
        .sum::<usize>();
    row.visuals.glyph_vertex_range = range.start..range.start + glyph_len;
}

/// A letter of a right-to-left script.
fn is_strong_rtl(character: char) -> bool {
    matches!(bidi_class(character), BidiClass::R | BidiClass::AL)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One unit of width per character, so a row's width is its length.
    fn by_character(piece: &str) -> f32 {
        piece.chars().count() as f32
    }

    #[test]
    fn a_word_longer_than_the_column_still_fits_the_column() {
        // A row has to hold something, so the first word of a row is taken
        // whatever it measures. `layout` then gives epaint no wrap width of
        // its own, so a row wider than the column is drawn past its edge.
        let one = break_rows("abcdefghij", 5.0, 1, Some(ELLIPSIS), by_character);
        assert_eq!(one, ["abcd\u{2026}"]);
        assert!(by_character(&one[0]) <= 5.0);

        // The same word with room for more rows: the last row is the one the
        // mark belongs to, and it is the one that has to fit.
        let two = break_rows("kl abcdefghij", 5.0, 2, Some(ELLIPSIS), by_character);
        assert!(by_character(two.last().unwrap()) <= 5.0);
        assert!(two.last().unwrap().ends_with(ELLIPSIS));

        // A column narrower than the mark itself still terminates.
        assert_eq!(
            break_rows("abc", 0.5, 1, Some(ELLIPSIS), by_character),
            ["\u{2026}"]
        );
    }

    #[test]
    fn rows_that_already_fit_are_left_alone() {
        // No mark where nothing was cut, which is what says a title is whole.
        assert_eq!(
            break_rows("ab cd ef gh", 5.0, 2, Some(ELLIPSIS), by_character),
            ["ab cd", "ef gh"]
        );
        // And the existing cut, which breaks at a space, is unchanged.
        assert_eq!(
            break_rows("ab cd ef gh", 5.0, 1, Some(ELLIPSIS), by_character),
            ["ab\u{2026}"]
        );
        // Without an overflow character nothing is added or removed.
        assert_eq!(
            break_rows("abcdefghij", 5.0, 1, None, by_character),
            ["abcdefghij"]
        );
    }

    /// The row `layout` draws for a right-to-left word longer than its
    /// column fits the column as epaint shapes it, in the fonts the app
    /// installs. Letters join and ligate, so no sum of letters measured on
    /// their own says how wide a piece of a word is; laying it out does.
    #[test]
    fn a_shaped_word_longer_than_the_column_is_drawn_inside_it() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        // Fonts set on a context take effect from its next pass.
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter();
            let font = FontId::proportional(18.0);
            let measure = |piece: &str| {
                painter
                    .layout_no_wrap(piece.to_owned(), font.clone(), Color32::WHITE)
                    .size()
                    .x
            };
            let mark = measure(&ELLIPSIS.to_string());
            for word in ["لالالالالالا", "והתקשרויותיהם"] {
                // Which Arabic and Hebrew faces a system has decides how wide
                // the word is, so the columns are taken from the word itself.
                let whole = measure(word);
                for share in [0.4, 0.6, 0.85] {
                    let wrap_width = (whole * share).max(mark + 1.0);
                    for max_rows in [1, 2] {
                        let galley = layout(
                            painter,
                            word,
                            font.clone(),
                            Color32::WHITE,
                            wrap_width,
                            max_rows,
                            Some(ELLIPSIS),
                        );
                        let text = galley.text();
                        assert_eq!(galley.rows.len(), 1, "{text:?}");
                        assert!(text.ends_with(ELLIPSIS), "{text:?}");
                        assert_mark_drawn_leftmost(&galley);
                        let drawn = galley.rows[0].size.x;
                        assert!(
                            drawn <= wrap_width,
                            "{word} in a {wrap_width} px column is drawn as {text}, {drawn} px wide"
                        );
                    }
                }
                // Given the room, the word stays whole and unmarked.
                let galley = layout(
                    painter,
                    word,
                    font.clone(),
                    Color32::WHITE,
                    whole + 1.0,
                    1,
                    Some(ELLIPSIS),
                );
                assert_eq!(galley.text(), word);
            }
        });
        output.textures_delta.clear();
    }

    #[test]
    fn detects_rtl() {
        assert!(is_rtl("غيوم في السماء"));
        assert!(is_rtl("מה נשמע"));
        assert!(is_rtl("السماء"));
        assert!(!is_rtl("Hello world"));
        assert!(!is_rtl("Hello غيوم")); // base LTR
        assert!(is_rtl("غيوم Hello")); // base RTL
        assert!(is_rtl("2024 غيوم")); // digits are weak
        assert!(!is_rtl(""));
        assert!(!is_rtl("   "));
        assert!(!is_rtl("123"));
        assert!(!is_rtl("Ça va"));
    }

    #[test]
    fn halign() {
        assert_eq!(halign_for("غيوم"), Align::RIGHT);
        assert_eq!(halign_for("Hello"), Align::LEFT);
    }

    /// Words fill rows up to the width, and a cut ends the last row.
    #[test]
    fn rows_fill_with_whole_words() {
        let width = |piece: &str| piece.chars().count() as f32 * 10.0;
        let text = "واحد اثنان ثلاثة";
        assert_eq!(
            break_rows(text, 110.0, usize::MAX, Some(ELLIPSIS), width),
            vec!["واحد اثنان".to_string(), "ثلاثة".to_string()]
        );
        assert_eq!(
            break_rows(text, 110.0, 1, Some(ELLIPSIS), width),
            vec!["واحد اثنان\u{2026}".to_string()]
        );
        // The mark has to fit too, so a word gives way to it.
        assert_eq!(
            break_rows(text, 100.0, 1, Some(ELLIPSIS), width),
            vec!["واحد\u{2026}".to_string()]
        );
        assert_eq!(
            break_rows("واحد\nاثنان", 200.0, usize::MAX, None, width),
            vec!["واحد".to_string(), "اثنان".to_string()]
        );
        assert_eq!(
            break_rows("واحد\nاثنان", 200.0, 1, Some(ELLIPSIS), width),
            vec!["واحد\u{2026}".to_string()]
        );
    }

    /// Wrapped text keeps its first words on the first row, and a cut
    /// sits at the left end of the last row.
    #[test]
    fn wrapped_rows_keep_reading_order() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let painter = ui.painter();
            let font = FontId::proportional(14.0);
            let text = "واحد اثنان ثلاثة أربعة خمسة ستة سبعة ثمانية";
            let galley = layout(
                painter,
                text,
                font.clone(),
                Color32::WHITE,
                90.0,
                2,
                Some(ELLIPSIS),
            );
            assert_eq!(galley.job.halign, Align::RIGHT);
            let lines: Vec<&str> = galley.text().split('\n').collect();
            assert_eq!(lines.len(), 2, "{lines:?}");
            assert!(
                lines[0].starts_with("واحد"),
                "first row holds the first word: {lines:?}"
            );
            assert!(
                lines[1].ends_with(ELLIPSIS),
                "the cut ends the text: {lines:?}"
            );
            assert_mark_drawn_leftmost(&galley);
            let first = &galley.rows[0].row.glyphs;
            let rightmost = first
                .iter()
                .max_by(|a, b| a.pos.x.total_cmp(&b.pos.x))
                .expect("glyphs");
            assert_eq!(
                galley.text()[rightmost.cluster as usize..].chars().next(),
                Some('و'),
                "the first word is read first, on the right"
            );
            let plain = layout(
                painter,
                "Hello world",
                font,
                Color32::WHITE,
                200.0,
                1,
                Some(ELLIPSIS),
            );
            assert_eq!(plain.job.halign, Align::LEFT);
            assert_eq!(plain.text(), "Hello world");
        });
        output.textures_delta.clear();
    }
    /// The last row's ellipsis is its leftmost glyph: where a right-to-left
    /// reader reaches the cut.
    fn assert_mark_drawn_leftmost(galley: &Galley) {
        let last = &galley.rows.last().expect("rows").row.glyphs;
        let leftmost = last
            .iter()
            .filter(|glyph| glyph.advance_width > 0.01)
            .min_by(|a, b| a.pos.x.total_cmp(&b.pos.x))
            .expect("glyphs");
        assert_eq!(leftmost.chr, ELLIPSIS, "{:?}", galley.text());
    }

    /// Checks every row against `unicode-bidi`'s reordered line for its
    /// paragraph: the drawn glyphs, left to right, spell the reordered line,
    /// less the characters that draw nothing of their own. ASCII paired
    /// brackets must also face their resolved direction: the glyph's ink in
    /// the font atlas leans the way a mirrored or unmirrored bracket would.
    fn assert_rows_follow_uba(galley: &Galley, atlas: &egui::ColorImage) {
        use unicode_bidi::BidiDataSource as _;
        let text = galley.text();
        // Rows hold one glyph per character in logical order, so a row's
        // text follows from glyph counts alone.
        let byte_at: Vec<usize> = text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()))
            .collect();
        let mut first_char = 0usize;
        for (index, placed) in galley.rows.iter().enumerate() {
            let glyphs = &placed.row.glyphs;
            let row_chars = first_char..first_char + glyphs.len();
            first_char = row_chars.end + usize::from(placed.ends_with_newline);
            if glyphs.is_empty() {
                continue;
            }
            let (start, end) = (byte_at[row_chars.start], byte_at[row_chars.end]);
            let paragraph_start = text[..start].rfind('\n').map_or(0, |at| at + 1);
            let paragraph_end = text[start..].find('\n').map_or(text.len(), |at| start + at);
            let paragraph = &text[paragraph_start..paragraph_end];
            let info = BidiInfo::new(paragraph, None);
            let line = start - paragraph_start..end.min(paragraph_end) - paragraph_start;
            let resolved = info
                .paragraphs
                .iter()
                .find(|candidate| candidate.range.contains(&line.start))
                .expect("bidi paragraph for the row");
            let levels = info.reordered_levels(resolved, line);
            let row_text: Vec<char> = text[start..end].chars().collect();
            let row_levels: Vec<Level> = (0..row_text.len())
                .map(|offset| levels[byte_at[row_chars.start + offset] - paragraph_start])
                .collect();
            // Marks, joiners, and the letters a ligature absorbs draw no
            // glyph of their own, so only glyphs that advance are compared.
            let expected: String = BidiInfo::reorder_visual(&row_levels)
                .into_iter()
                .filter(|&offset| glyphs[offset].advance_width > 0.01)
                .map(|offset| row_text[offset])
                .collect();
            let mut drawn: Vec<&Glyph> = glyphs
                .iter()
                .filter(|glyph| glyph.advance_width > 0.01)
                .collect();
            drawn.sort_by(|a, b| a.pos.x.total_cmp(&b.pos.x));
            let visual: String = drawn.iter().map(|glyph| glyph.chr).collect();
            assert_eq!(visual, expected, "row {index} of {paragraph:?}");
            for (offset, glyph) in glyphs.iter().enumerate() {
                let Some(bracket) =
                    unicode_bidi::HardcodedBidiData.bidi_matched_opening_bracket(glyph.chr)
                else {
                    continue;
                };
                if !glyph.chr.is_ascii() || glyph.uv_rect.is_nothing() {
                    continue;
                }
                let rtl = levels[byte_at[row_chars.start + offset] - paragraph_start].is_rtl();
                // An opening bracket's ink sits left of centre; mirrored, right.
                assert_eq!(
                    ink_leans_right(atlas, glyph),
                    bracket.is_open == rtl,
                    "{:?} faces the wrong way in row {index} of {paragraph:?}",
                    glyph.chr
                );
            }
        }
    }

    /// Whether the glyph's middle bulges right of its tips, as `)`, `]`, and
    /// `}` do.
    fn ink_leans_right(atlas: &egui::ColorImage, glyph: &Glyph) -> bool {
        let [left, top] = glyph.uv_rect.min;
        let [right, bottom] = glyph.uv_rect.max;
        let height = bottom - top;
        assert!(height >= 4, "{:?} is too small to inspect", glyph.chr);
        let centre_of = |rows: &mut dyn Iterator<Item = u16>| {
            let mut mass = 0.0f32;
            let mut moment = 0.0f32;
            for y in rows {
                for x in left..right {
                    let alpha =
                        f32::from(atlas.pixels[y as usize * atlas.size[0] + x as usize].a());
                    mass += alpha;
                    moment += alpha * f32::from(x);
                }
            }
            assert!(mass > 0.0, "{:?} has no ink in the atlas", glyph.chr);
            moment / mass
        };
        let quarter = height / 4;
        let tips = centre_of(&mut (top..top + quarter).chain(bottom - quarter..bottom));
        let middle = centre_of(&mut (top + quarter..bottom - quarter));
        middle > tips
    }

    /// Lays out each sample with the app's fonts through `lay`, and checks
    /// what is drawn against the Unicode bidi algorithm.
    fn check_drawn_order(samples: &[&str], lay: impl Fn(&Painter, &str) -> Arc<Galley>) {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        // Fonts set on a context take effect from its next pass.
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        let galleys = std::cell::RefCell::new(Vec::new());
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for text in samples {
                galleys.borrow_mut().push(lay(ui.painter(), text));
            }
        });
        output.textures_delta.clear();
        let atlas = ctx.fonts(|fonts| fonts.image());
        for galley in galleys.into_inner() {
            assert_rows_follow_uba(&galley, &atlas);
        }
    }

    /// Words, punctuation, digits, Latin runs, and brackets land where the
    /// bidi algorithm puts them. Spotifast once reordered words itself for
    /// stock egui; with the fork shaping each run in its own direction, that
    /// reversed punctuation next to a space a second time.
    #[test]
    fn lines_are_drawn_in_bidi_order() {
        let samples = [
            "غيوم في السماء",
            "מה נשמע",
            "الحلقة الأولى: كيف",
            "وإدارة شركتك. يقدمه",
            "الكلمة1 الكلمة2",
            "شركة 37signals كل",
            "שיר 2024, גרסה",
            "(שלום) עולם",
            "אבג - דהו",
            "Hello (עולם) there",
            "Hello غيوم في",
            "غيوم Hello world",
            "لا بأس",
            "إلى السطر التالي",
        ];
        check_drawn_order(&samples, |painter, text| {
            layout_line(painter, text, FontId::proportional(18.0), Color32::WHITE)
        });
        check_drawn_order(&samples, |painter, text| {
            layout(
                painter,
                text,
                FontId::proportional(18.0),
                Color32::WHITE,
                1000.0,
                1,
                Some(ELLIPSIS),
            )
        });
    }

    /// Wrapped and cut rows keep bidi order row by row.
    #[test]
    fn wrapped_and_cut_rows_are_drawn_in_bidi_order() {
        check_drawn_order(
            &[
                "הכלב הגדול (קפץ) מעל החתול, והמשיך לרוץ לאורך הרחוב",
                "هذا نص عربي طويل: يختبر ترتيب الأسطر عندما تلتف الكلمات",
            ],
            |painter, text| {
                layout(
                    painter,
                    text,
                    FontId::proportional(14.0),
                    Color32::WHITE,
                    120.0,
                    3,
                    Some(ELLIPSIS),
                )
            },
        );
    }

    /// Runs moved into visual order stay on whole physical pixels at
    /// fractional scales, where epaint rasterized them, instead of landing
    /// between pixels and blurring.
    #[test]
    fn reordered_runs_stay_on_whole_pixels() {
        for pixels_per_point in [4.0 / 3.0, 1.6] {
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            let mut input = egui::RawInput::default();
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .native_pixels_per_point = Some(pixels_per_point);
            ctx.run_ui(input.clone(), |_| {}).textures_delta.clear();
            let mut output = ctx.run_ui(input, |ui| {
                for text in ["Song 12 שיר ישן, part 3", "غيوم في السماء (Live) 2024"]
                {
                    let galley = layout_line(
                        ui.painter(),
                        text,
                        FontId::proportional(13.0),
                        Color32::WHITE,
                    );
                    assert_eq!(galley.pixels_per_point, pixels_per_point);
                    let on_grid = |x: f32| {
                        let pixels = x * pixels_per_point;
                        (pixels - pixels.round()).abs() < 1e-3
                    };
                    let row = &galley.rows[0].row;
                    assert!(row.glyphs.iter().any(|glyph| glyph.rtl), "{text}");
                    for glyph in &row.glyphs {
                        assert!(on_grid(glyph.pos.x), "{text}: {glyph:?}");
                    }
                    for vertex in &row.visuals.mesh.vertices[row.visuals.glyph_vertex_range.clone()]
                    {
                        assert!(on_grid(vertex.pos.x), "{text} at {pixels_per_point}");
                    }
                }
            });
            output.textures_delta.clear();
        }
    }

    /// Moving runs into visual order keeps the width epaint shaped, at any
    /// scale: [`layout`] measures its cuts in logical order, so a reordered
    /// row that grew would be drawn past the column it was cut for. The
    /// zero-width stand-ins after a ligature such as لا sit on whole pixels
    /// beyond its advance, and neither they nor the pixel grid are part of a
    /// run's width.
    #[test]
    fn reordering_keeps_the_shaped_width() {
        for pixels_per_point in [1.0, 4.0 / 3.0, 1.5, 1.6, 2.0] {
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            let mut input = egui::RawInput::default();
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .native_pixels_per_point = Some(pixels_per_point);
            ctx.run_ui(input.clone(), |_| {}).textures_delta.clear();
            let mut output = ctx.run_ui(input, |ui| {
                for text in [
                    "لالا\u{2026}",
                    "لالالالالالا",
                    "והתקשרו\u{2026}",
                    "Song 12 שיר ישן, part 3",
                    "غيوم في السماء (Live) 2024",
                ] {
                    for size in [13.0, 18.0] {
                        let font = FontId::proportional(size);
                        let logical =
                            ui.painter()
                                .layout_no_wrap(text.to_owned(), font, Color32::WHITE);
                        let mut visual = logical.clone();
                        reorder(&mut visual);
                        assert_eq!(
                            visual.rows[0].size.x, logical.rows[0].size.x,
                            "{text} at {size} pt, {pixels_per_point}x"
                        );
                        assert_eq!(visual.size(), logical.size(), "{text}");
                    }
                }
            });
            output.textures_delta.clear();
        }
    }

    /// Text without right-to-left letters is never copied or moved.
    #[test]
    fn left_to_right_galleys_are_left_alone() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let original = ui.painter().layout_no_wrap(
                "Hello (world)".into(),
                FontId::default(),
                Color32::WHITE,
            );
            let mut galley = original.clone();
            reorder(&mut galley);
            assert!(Arc::ptr_eq(&original, &galley));
        });
        output.textures_delta.clear();
    }

    /// A second pass over a reordered galley changes nothing.
    #[test]
    fn reordering_twice_is_harmless() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut galley = layout_line(
                ui.painter(),
                "שיר 2024, גרסה",
                FontId::default(),
                Color32::WHITE,
            );
            let once: Vec<f32> = galley.rows[0].row.glyphs.iter().map(|g| g.pos.x).collect();
            let before = galley.clone();
            reorder(&mut galley);
            assert!(Arc::ptr_eq(&before, &galley));
            let twice: Vec<f32> = galley.rows[0].row.glyphs.iter().map(|g| g.pos.x).collect();
            assert_eq!(once, twice);
        });
        output.textures_delta.clear();
    }
}
