//! The skin cards in Settings: a picture of each skin with its name under
//! it, wrapped to the width there is.
//!
//! A folder can hold hundreds of skins, so the grid's height comes from
//! arithmetic and only the cards in view are painted or ask for their picture.
//! Every card is still a widget, so the keyboard and screen readers reach
//! them all.

use egui::{Color32, CornerRadius, Rect, Sense, StrokeKind, Ui, pos2, vec2};

use crate::skin_previews::{SkinPreviews, Thumb};
use crate::theme::{self, Palette};

/// The narrowest a card may be before a column goes.
const MIN_CARD_WIDTH: f32 = 148.0;
const GAP: f32 = 10.0;
const PADDING: f32 = 8.0;
/// A card's picture is this share of its width tall.
const PICTURE_SHARE: f32 = 0.5;
const LABEL_HEIGHT: f32 = 26.0;
const LABEL_SIZE: f32 = 13.0;
const ROUNDING: u8 = 8;
const RING: f32 = 2.0;

/// One card: what its picture is filed under, and the name it shows.
pub struct Card<'a> {
    pub key: &'a str,
    pub label: &'a str,
}

/// How the cards fit a width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub columns: usize,
    pub card: egui::Vec2,
}

impl Grid {
    pub fn fit(width: f32) -> Self {
        let columns = (((width + GAP) / (MIN_CARD_WIDTH + GAP)).floor() as usize).max(1);
        let card_width = ((width - GAP * (columns as f32 - 1.0)) / columns as f32).max(1.0);
        let height = card_width * PICTURE_SHARE + LABEL_HEIGHT + 2.0 * PADDING;
        Self {
            columns,
            card: vec2(card_width, height),
        }
    }

    pub fn height(&self, count: usize) -> f32 {
        let rows = count.div_ceil(self.columns) as f32;
        rows * self.card.y + (rows - 1.0).max(0.0) * GAP
    }

    pub fn card_rect(&self, origin: egui::Pos2, index: usize) -> Rect {
        let (row, column) = (index / self.columns, index % self.columns);
        let at = vec2(
            column as f32 * (self.card.x + GAP),
            row as f32 * (self.card.y + GAP),
        );
        Rect::from_min_size(origin + at, self.card)
    }
}

/// `size` as big as fits `room`, in the middle of it, in whole pixels of
/// the picture when it is made bigger.
fn fitted(room: Rect, size: [usize; 2]) -> Rect {
    let (width, height) = (size[0] as f32, size[1] as f32);
    let mut scale = (room.width() / width).min(room.height() / height);
    if scale >= 1.0 {
        scale = scale.floor();
    }
    Rect::from_center_size(room.center(), vec2(width, height) * scale)
}

fn paint_label(ui: &Ui, palette: &Palette, rect: Rect, label: &str, selected: bool) {
    let font = theme::medium(LABEL_SIZE);
    let color = if selected {
        palette.window
    } else {
        palette.text
    };
    let room = rect.shrink2(vec2(8.0, 0.0));
    let galley = crate::bidi::layout(
        ui.painter(),
        label,
        font,
        color,
        room.width().max(1.0),
        1,
        Some('…'),
    );
    if selected {
        let pill = rect.height() / 2.0;
        ui.painter().rect_filled(rect, pill, palette.text);
    }
    let top = rect.center().y - galley.size().y / 2.0;
    let line = Rect::from_min_max(pos2(room.left(), top), pos2(room.right(), rect.bottom()));
    ui.painter()
        .galley(crate::bidi::galley_pos(line, &galley), galley, color);
}

/// Draws the cards and says which was clicked.
pub fn grid(
    ui: &mut Ui,
    palette: &Palette,
    previews: &mut SkinPreviews,
    cards: &[Card<'_>],
    current: Option<usize>,
) -> Option<usize> {
    let grid = Grid::fit(ui.available_width());
    let size = vec2(ui.available_width(), grid.height(cards.len()));
    let (area, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut clicked = None;
    for (index, card) in cards.iter().enumerate() {
        let rect = grid.card_rect(area.min, index);
        let selected = current == Some(index);
        let id = ui.id().with(("skin-card", card.key));
        let response = ui.interact(rect, id, Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::RadioButton,
                ui.is_enabled(),
                selected,
                card.label,
            )
        });
        if response.gained_focus() {
            response.scroll_to_me(None);
        }
        if response.clicked() {
            clicked = Some(index);
        }
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let response = response.on_hover_text(card.label);
        // The section around them is `surface`: a card must stand off it.
        let fill = if response.hovered() {
            palette.surface_hover
        } else {
            palette.window
        };
        let rounding = CornerRadius::same(ROUNDING);
        ui.painter().rect_filled(rect, rounding, fill);
        let inner = rect.shrink(PADDING);
        let (picture, label) = inner.split_top_bottom_at_y(inner.bottom() - LABEL_HEIGHT);
        match previews.thumb(card.key) {
            Thumb::Picture(texture, size) => {
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                ui.painter()
                    .image(texture, fitted(picture, size), uv, Color32::WHITE);
                paint_label(ui, palette, label, card.label, selected);
            }
            Thumb::Placeholder => {
                let rounding = CornerRadius::same(ROUNDING / 2);
                ui.painter()
                    .rect_filled(picture, rounding, palette.surface_hover);
                paint_label(ui, palette, label, card.label, selected);
            }
            // No picture: the name is all there is, in the middle.
            Thumb::Failed => {
                let middle =
                    Rect::from_center_size(inner.center(), vec2(inner.width(), LABEL_HEIGHT));
                paint_label(ui, palette, middle, card.label, selected);
            }
        }
        if selected {
            let ring = egui::Stroke::new(RING, palette.text);
            ui.painter()
                .rect_stroke(rect, rounding, ring, StrokeKind::Inside);
        }
        theme::focus_ring(ui, &response);
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_wrap_to_the_width_and_fill_it() {
        let wide = Grid::fit(640.0);
        assert_eq!(wide.columns, 4);
        let right = wide.card_rect(egui::Pos2::ZERO, 3).right();
        assert!((right - 640.0).abs() < 0.01, "{right}");
        assert_eq!(wide.card_rect(egui::Pos2::ZERO, 4).left(), 0.0);
        assert_eq!(Grid::fit(100.0).columns, 1);
        assert_eq!(Grid::fit(100.0).card.x, 100.0);
    }

    #[test]
    fn the_grid_is_as_tall_as_its_rows() {
        let grid = Grid::fit(640.0);
        assert_eq!(grid.height(0), 0.0);
        assert_eq!(grid.height(4), grid.card.y);
        assert_eq!(grid.height(5), 2.0 * grid.card.y + GAP);
    }

    /// Scrolls a grid of 500 cards, every one a widget, for 120 frames.
    #[test]
    fn hundreds_of_cards_cost_little_a_frame() {
        let names: Vec<String> = (0..500).map(|index| format!("Skin {index}")).collect();
        let cards: Vec<Card> = names
            .iter()
            .map(|name| Card {
                key: name,
                label: name,
            })
            .collect();
        let palette = crate::theme::Palette::dark();
        let ctx = egui::Context::default();
        ctx.set_fonts(fastframe_fonts::FontSetup::default().definitions());
        let mut previews = SkinPreviews::default();
        let mut slowest = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;
        for frame in 0..120 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 700.0))),
                ..Default::default()
            };
            let started = std::time::Instant::now();
            let mut output = ctx.run_ui(input, |ui| {
                egui::ScrollArea::vertical()
                    .vertical_scroll_offset(frame as f32 * 80.0)
                    .show(ui, |ui| {
                        grid(ui, &palette, &mut previews, &cards, Some(3));
                    });
            });
            let spent = started.elapsed();
            output.textures_delta.clear();
            total += spent;
            if frame > 2 {
                slowest = slowest.max(spent);
            }
        }
        println!("500 cards: {:?} a frame, {slowest:?} at worst", total / 120);
        // Unoptimised, on a test runner: well inside a 60 Hz frame on average.
        assert!(
            total / 120 < std::time::Duration::from_millis(12),
            "{total:?}"
        );
    }

    #[test]
    fn a_small_picture_grows_in_whole_steps_and_a_big_one_just_fits() {
        let room = Rect::from_min_size(pos2(0.0, 0.0), vec2(280.0, 120.0));
        assert_eq!(fitted(room, [120, 50]).size(), vec2(240.0, 100.0));
        assert_eq!(fitted(room, [560, 100]).size(), vec2(280.0, 50.0));
    }
}
