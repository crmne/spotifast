//! Compact, independent lyric toggles, shared by the panel and full screen.

use std::sync::Arc;

use egui::{Color32, Galley, Rect, Response, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::lyrics::assistance::ControlsPresentation;
use crate::theme::{self, Icon, Palette};

const HEIGHT: f32 = 30.0;
const ICON: f32 = 15.0;
const CHECK: f32 = 12.0;
const GAP: f32 = 6.0;

pub(super) struct Toggle<'a> {
    pub label: &'a str,
    pub selected: bool,
    pub enabled: bool,
}

/// Reserve the whole group once so wrapping cannot expand the panel. Both
/// labels remain intact: a narrow group stacks, and an individual label may wrap.
pub(super) fn show(
    ui: &mut Ui,
    palette: &Palette,
    presentation: ControlsPresentation,
    controls: [Toggle<'_>; 2],
) -> [Response; 2] {
    let grouped = presentation.grouped();
    let inset = if grouped { 4.0 } else { 0.0 };
    let spacing = if grouped { 4.0 } else { 10.0 };
    let padding = if grouped { 8.0 } else { 6.0 };
    let chrome = padding * 2.0 + ICON + CHECK + GAP * 2.0;
    let available = (ui.available_width() - inset * 2.0).max(1.0);
    let natural = controls.each_ref().map(|control| {
        crate::bidi::layout_line(
            ui.painter(),
            control.label,
            theme::medium(13.0),
            palette.text,
        )
        .size()
        .x
        .ceil()
            + chrome
    });
    let stacked = natural[0] + natural[1] + spacing > available;
    let widths = if stacked { [available; 2] } else { natural };
    let labels = std::array::from_fn::<_, 2, _>(|index| {
        crate::bidi::layout(
            ui.painter(),
            controls[index].label,
            theme::medium(13.0),
            Color32::PLACEHOLDER,
            (widths[index] - chrome).max(1.0),
            usize::MAX,
            None,
        )
    });
    let heights = labels
        .each_ref()
        .map(|label| HEIGHT.max(label.size().y + 10.0));
    let content = if stacked {
        vec2(available, heights[0] + spacing + heights[1])
    } else {
        vec2(widths[0] + spacing + widths[1], heights[0].max(heights[1]))
    };
    let (group, _) =
        ui.allocate_exact_size(content + vec2(inset * 2.0, inset * 2.0), Sense::hover());
    if grouped {
        ui.painter().rect_filled(group, 10.0, palette.surface);
        ui.painter().rect_stroke(
            group,
            10.0,
            Stroke::new(1.0, palette.outline),
            egui::StrokeKind::Inside,
        );
    }
    let start = group.min + vec2(inset, inset);
    std::array::from_fn(|index| {
        let offset = if index == 0 {
            vec2(0.0, 0.0)
        } else if stacked {
            vec2(0.0, heights[0] + spacing)
        } else {
            vec2(widths[0] + spacing, 0.0)
        };
        let rect = Rect::from_min_size(start + offset, vec2(widths[index], heights[index]));
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(("lyrics-toggle", index))
                .max_rect(rect),
        );
        if !controls[index].enabled {
            child.disable();
        }
        let layout = ButtonLayout {
            grouped,
            padding,
            rect,
            label: labels[index].clone(),
            icon: [Icon::Languages, Icon::WholeWord][index],
        };
        toggle(&mut child, palette, &controls[index], layout)
    })
}

struct ButtonLayout {
    grouped: bool,
    padding: f32,
    rect: Rect,
    label: Arc<Galley>,
    icon: Icon,
}

fn toggle(ui: &mut Ui, palette: &Palette, control: &Toggle<'_>, layout: ButtonLayout) -> Response {
    let ButtonLayout {
        grouped,
        padding,
        rect,
        label,
        icon,
    } = layout;
    let response = ui.interact(rect, ui.id(), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Button,
            ui.is_enabled(),
            control.selected,
            control.label,
        )
    });
    let enabled = ui.is_enabled();
    let hovered = enabled && (response.hovered() || response.has_focus());
    let selected = enabled && control.selected;
    let color = if !enabled {
        palette.dim
    } else if selected || hovered {
        palette.text
    } else {
        palette.secondary
    };
    if ui.is_rect_visible(rect) {
        if grouped && selected {
            let fill = if hovered {
                palette.surface_hover
            } else {
                palette.surface_active
            };
            let tint = if hovered { 0.12 } else { 0.08 };
            let fill = Color32::from(
                egui::Rgba::from(fill) * (1.0 - tint) + egui::Rgba::from(palette.accent) * tint,
            );
            ui.painter().rect_filled(rect, 8.0, fill);
            ui.painter().rect_stroke(
                rect,
                8.0,
                Stroke::new(1.0, palette.accent.gamma_multiply(0.28)),
                egui::StrokeKind::Inside,
            );
        } else if hovered {
            ui.painter().rect_filled(rect, 8.0, palette.surface_hover);
        }
        let icon_color = if selected { palette.accent } else { color };
        let icon_rect = Rect::from_center_size(
            pos2(rect.left() + padding + ICON / 2.0, rect.center().y),
            vec2(ICON, ICON),
        );
        icon.image(icon_color, ICON).paint_at(ui, icon_rect);
        ui.painter().galley(
            pos2(
                rect.left() + padding + ICON + GAP,
                rect.center().y - label.size().y / 2.0,
            ),
            label,
            color,
        );
        if control.selected {
            let check_rect = Rect::from_center_size(
                pos2(rect.right() - padding - CHECK / 2.0, rect.center().y),
                vec2(CHECK, CHECK),
            );
            Icon::Check
                .image(icon_color, CHECK)
                .paint_at(ui, check_rect);
            if !grouped {
                ui.painter().line_segment(
                    [
                        pos2(rect.left() + padding, rect.bottom() - 1.0),
                        pos2(rect.right() - padding, rect.bottom() - 1.0),
                    ],
                    Stroke::new(2.0, icon_color),
                );
            }
        }
    }
    theme::focus_ring(ui, &response);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Locale, gettext};
    use clap::ValueEnum;

    struct Fixture {
        presentation: ControlsPresentation,
        locale: Locale,
        dark: bool,
        width: f32,
        selected: [bool; 2],
        enabled: [bool; 2],
    }

    impl Default for Fixture {
        fn default() -> Self {
            Self {
                presentation: ControlsPresentation::Toolbar,
                locale: Locale::English,
                dark: true,
                width: 336.0,
                selected: [false; 2],
                enabled: [true; 2],
            }
        }
    }

    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        theme::install(&ctx);
        ctx
    }

    fn frame(
        ctx: &egui::Context,
        fixture: &Fixture,
        events: Vec<egui::Event>,
    ) -> ([Response; 2], egui::FullOutput) {
        let mut responses = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 600.0))),
                events,
                ..Default::default()
            },
            |ui| {
                ui.set_width(fixture.width);
                let palette = if fixture.dark {
                    Palette::dark()
                } else {
                    Palette::light()
                };
                let labels = [
                    gettext(fixture.locale, "Translation"),
                    gettext(fixture.locale, "Pronunciation"),
                ];
                responses = Some(show(
                    ui,
                    &palette,
                    fixture.presentation,
                    std::array::from_fn(|index| Toggle {
                        label: &labels[index],
                        selected: fixture.selected[index],
                        enabled: fixture.enabled[index],
                    }),
                ));
            },
        );
        // CPU-only frames have no renderer to consume texture uploads.
        output.textures_delta.clear();
        (responses.unwrap(), output)
    }

    fn click(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn key(key: egui::Key) -> Vec<egui::Event> {
        [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .collect()
    }

    /// Long translated labels must remain visible at the 280px panel minimum
    /// (256px inside its margins), and state changes must never shift hit boxes.
    #[test]
    fn all_locales_fit_both_layouts_without_truncation_or_state_jitter() {
        let ctx = context();
        for &locale in Locale::value_variants() {
            for presentation in [
                ControlsPresentation::Toolbar,
                ControlsPresentation::Separate,
            ] {
                for dark in [false, true] {
                    for width in [256.0, 336.0] {
                        let mut fixture = Fixture {
                            locale,
                            presentation,
                            dark,
                            width,
                            ..Default::default()
                        };
                        let (initial, _) = frame(&ctx, &fixture, vec![]);
                        for selected in [[false, false], [true, false], [false, true], [true, true]]
                        {
                            fixture.selected = selected;
                            let (responses, output) = frame(&ctx, &fixture, vec![]);
                            assert!(!responses[0].rect.intersects(responses[1].rect));
                            for index in 0..2 {
                                let rect = responses[index].rect;
                                assert_eq!(
                                    rect, initial[index].rect,
                                    "{locale:?} {presentation:?}"
                                );
                                assert!(
                                    rect.right() <= width + 1.0 && rect.left() >= 0.0,
                                    "{locale:?} {presentation:?}: {rect:?}, width {width}"
                                );
                                assert_eq!(rect.height(), HEIGHT, "{locale:?}");
                            }
                            let mut shapes: Vec<_> =
                                output.shapes.iter().map(|shape| &shape.shape).collect();
                            let mut painted = 0;
                            while let Some(shape) = shapes.pop() {
                                match shape {
                                    egui::epaint::Shape::Vec(children) => shapes.extend(children),
                                    egui::epaint::Shape::Text(text) => {
                                        let index = ["Translation", "Pronunciation"]
                                            .iter()
                                            .position(|source| {
                                                text.galley.job.text == gettext(locale, source)
                                            })
                                            .expect("only complete labels are painted");
                                        let bounds = text.galley.rect.translate(text.pos.to_vec2());
                                        assert!(
                                            responses[index].rect.contains_rect(bounds),
                                            "clipped {locale:?}: {bounds:?}"
                                        );
                                        assert!(
                                            bounds.right()
                                                <= responses[index].rect.right() - CHECK - GAP,
                                            "label overlaps check mark: {locale:?}"
                                        );
                                        painted += 1;
                                    }
                                    _ => {}
                                }
                            }
                            assert_eq!(painted, 2);
                            // Full labels and independent toggle state are exposed
                            // to screen readers, including when both are off.
                            let tree = output.platform_output.accesskit_update.unwrap();
                            for (index, source) in
                                ["Translation", "Pronunciation"].iter().enumerate()
                            {
                                let name = gettext(locale, source);
                                let node = tree
                                    .nodes
                                    .iter()
                                    .find(|(_, node)| node.label() == Some(name.as_ref()))
                                    .unwrap_or_else(|| panic!("missing {locale:?} label: {name}"));
                                assert_eq!(
                                    node.1.toggled(),
                                    Some(if selected[index] {
                                        egui::accesskit::Toggled::True
                                    } else {
                                        egui::accesskit::Toggled::False
                                    })
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn pointer_keyboard_and_disabled_controls_are_independent() {
        for presentation in [
            ControlsPresentation::Toolbar,
            ControlsPresentation::Separate,
        ] {
            let ctx = context();
            let mut fixture = Fixture {
                presentation,
                ..Default::default()
            };
            frame(&ctx, &fixture, vec![]);
            let (responses, _) = frame(&ctx, &fixture, vec![]);
            for index in 0..2 {
                let (clicked, _) = frame(&ctx, &fixture, click(responses[index].rect.center()));
                assert!(clicked[index].clicked());
                assert!(!clicked[1 - index].clicked());
                for activation in [egui::Key::Enter, egui::Key::Space] {
                    ctx.memory_mut(|memory| memory.request_focus(responses[index].id));
                    let (activated, _) = frame(&ctx, &fixture, key(activation));
                    assert!(activated[index].clicked());
                    assert!(!activated[1 - index].clicked());
                }
            }
            ctx.memory_mut(|memory| memory.surrender_focus(responses[1].id));
            let (tabbed, _) = frame(&ctx, &fixture, key(egui::Key::Tab));
            assert!(tabbed[0].has_focus());
            let (tabbed, _) = frame(&ctx, &fixture, key(egui::Key::Tab));
            assert!(tabbed[1].has_focus());
            fixture.enabled = [false, true];
            let (disabled, _) = frame(&ctx, &fixture, click(responses[0].rect.center()));
            assert!(!disabled[0].enabled());
            assert!(!disabled[0].clicked());
            let (enabled, _) = frame(&ctx, &fixture, click(responses[1].rect.center()));
            assert!(enabled[1].clicked());
            fixture.enabled = [false; 2];
            for index in 0..2 {
                let (disabled, _) = frame(&ctx, &fixture, click(responses[index].rect.center()));
                assert!(!disabled[index].clicked());
            }
        }
    }
}
