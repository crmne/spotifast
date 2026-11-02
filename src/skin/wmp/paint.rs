//! Turns a view's live elements into a list of things to draw.
//!
//! This holds every rule of how a skin looks, in skin pixels and free of the
//! windowing toolkit, so that the rules can be tested and the drawing code
//! stays a thin loop. Hit testing lives here too, since it must agree with
//! what is drawn.

use std::sync::Arc;
use std::time::Instant;

use super::Box2;
use super::deck::Deck;
use super::live::{Element, ElementId, Kind, ViewState, text_of};
use super::picture::{ImageKey, Images, Picture, Strip};
use super::predefined::color;
use crate::skin::Rgb;

/// Points to pixels on the 96 dpi screens skins were made for.
const PIXELS_PER_POINT: f32 = 96.0 / 72.0;
/// The gap before scrolling text comes round again.
const SCROLL_GAP: f32 = 25.0;
/// Rows in lists, in pixels of a skin's 8 point text.
const ROW_HEIGHT: f64 = 15.0;
/// Most copies of a tile drawn along one side.
const MAX_TILES: i32 = 400;

/// Where something may draw: rectangles, one when the clip is plain and
/// several when a shaped sub-view cuts it.
pub type Clip = Arc<Vec<Box2>>;

fn intersect(a: Box2, b: Box2) -> Option<Box2> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let bottom =
        a.y.saturating_add(a.height)
            .min(b.y.saturating_add(b.height));
    (right > x && bottom > y).then_some(Box2 {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

fn clip_to(clip: &Clip, area: Box2) -> Clip {
    Arc::new(
        clip.iter()
            .filter_map(|rect| intersect(*rect, area))
            .collect(),
    )
}

/// A shape's rectangles are few, since joined rows share one.
fn clip_to_shape(clip: &Clip, shape: &[Box2], at: (i32, i32)) -> Clip {
    let mut rects = Vec::new();
    for rect in clip.iter() {
        for run in shape {
            let moved = Box2 {
                x: run.x.saturating_add(at.0),
                y: run.y.saturating_add(at.1),
                ..*run
            };
            rects.extend(intersect(*rect, moved));
        }
    }
    Arc::new(rects)
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextPaint {
    pub text: String,
    pub area: Box2,
    pub size_px: f32,
    pub color: Rgb,
    pub alpha: u8,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub mono: bool,
    /// -1 left, 0 centre, 1 right.
    pub align: i8,
    /// Pixels the text has moved left.
    pub scroll: f32,
    pub wrap: bool,
}

pub enum Paint {
    Picture {
        picture: Arc<Picture>,
        frame: usize,
        /// Where the picture's top-left goes.
        x: i32,
        y: i32,
        /// The part of the frame to draw; its size is the drawn size unless
        /// `stretch` gives another.
        part: Box2,
        stretch: Option<(i32, i32)>,
        alpha: u8,
        clip: Clip,
    },
    Fill {
        area: Box2,
        color: Rgb,
        alpha: u8,
        clip: Clip,
    },
    Text {
        text: TextPaint,
        clip: Clip,
    },
    Effects {
        area: Box2,
        alpha: u8,
        clip: Clip,
    },
    /// A slim strip of the sound, for an `effects` element with `wave`.
    Wave {
        area: Box2,
        color: Rgb,
        alpha: u8,
        clip: Clip,
    },
    /// The cover art, for a video pane or an album-art picture.
    Art {
        area: Box2,
        background: Option<Rgb>,
        clip: Clip,
    },
}

/// How wide text is, in pixels, given its size, boldness and monospacing.
pub type Measure = dyn Fn(&str, f32, bool, bool) -> f32 + Send + Sync;

pub struct Painter<'a> {
    pub view: &'a mut ViewState,
    pub images: &'a mut Images,
    pub deck: &'a Deck,
    pub measure: &'a Measure,
    pub now: Instant,
    pub out: Vec<Paint>,
    /// In how long something drawn changes by itself.
    pub wake_ms: Option<u64>,
    /// Controls that were windows of their own in WMP and so sat over
    /// every picture, whatever the `zIndex`.
    pub on_top: Vec<(ElementId, Box2, f64, Clip)>,
}

fn soonest(a: &mut Option<u64>, b: u64) {
    *a = Some(a.map_or(b, |a| a.min(b)));
}

pub fn font_of(element: &Element) -> (f32, bool, bool, bool, bool, bool) {
    let points = element.num("fontsize", 10.0).clamp(1.0, 256.0) as f32;
    let style = element
        .str("fontstyle")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let face = element
        .str("fontface")
        .or_else(|| element.str("fonttype"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let normal = style.split_whitespace().any(|word| word == "normal");
    let has = |word: &str| !normal && style.split_whitespace().any(|part| part == word);
    (
        points * PIXELS_PER_POINT,
        has("bold")
            || element
                .str("fontweight")
                .is_some_and(|w| w.eq_ignore_ascii_case("bold")),
        has("italic"),
        has("underline"),
        has("strikeout"),
        face.contains("courier") || face.contains("mono") || face.contains("console"),
    )
}

/// A text element's natural line height.
pub fn line_height(size_px: f32) -> f64 {
    f64::from((size_px * 1.25).ceil())
}

impl Painter<'_> {
    pub fn run(&mut self) {
        let root = self.view.root;
        let (width, height) = self.view.size();
        let clip: Clip = Arc::new(vec![Box2 {
            x: 0,
            y: 0,
            width: width.round() as i32,
            height: height.round() as i32,
        }]);
        self.element(root, 0.0, 0.0, 255.0, &clip);
        for (id, area, alpha, inside) in std::mem::take(&mut self.on_top) {
            match self.view.elements[id].kind {
                Kind::Playlist | Kind::ListBox => self.list(id, area, &inside),
                Kind::Video => self.video(id, area, &inside),
                _ => self.effects(area, alpha, &inside),
            }
        }
        // Popups sit over everything in the window.
        for id in 0..self.view.elements.len() {
            if self.view.elements[id].popped {
                let area = self.view.world_box(id);
                self.list(id, area, &clip);
            }
        }
    }

    fn element(&mut self, id: ElementId, ox: f64, oy: f64, alpha: f64, clip: &Clip) {
        let element = &self.view.elements[id];
        if !element.visible {
            return;
        }
        let root = id == self.view.root;
        let x = if root { 0.0 } else { ox + element.left };
        let y = if root { 0.0 } else { oy + element.top };
        let alpha = alpha * element.alpha / 255.0;
        let area = Box2 {
            x: x.round() as i32,
            y: y.round() as i32,
            width: element.width.round() as i32,
            height: element.height.round() as i32,
        };
        match element.kind {
            Kind::View | Kind::SubView => self.container(id, x, y, area, alpha, clip),
            Kind::ButtonGroup => {
                if self.view.elements[id].flag("showbackground", false) {
                    self.state_picture(id, "image", area, alpha, clip);
                }
                for child in self.view.in_order(id) {
                    self.mapped_button(child, id, area, alpha, clip);
                }
            }
            Kind::Button => self.button(id, area, alpha, clip),
            Kind::Slider => self.slider(id, area, alpha, clip),
            Kind::CustomSlider => self.custom_slider(id, area, alpha, clip),
            Kind::Text => self.text(id, area, alpha, clip),
            Kind::Effects if self.view.elements[id].flag("wave", false) => {
                self.out.push(Paint::Wave {
                    area,
                    color: self.view.elements[id]
                        .color("foregroundcolor")
                        .unwrap_or([255, 255, 255]),
                    alpha: alpha as u8,
                    clip: clip.clone(),
                });
            }
            Kind::Effects => {
                if self.view.elements[id].flag("windowed", false) {
                    self.on_top.push((id, area, alpha, clip.clone()));
                } else {
                    self.effects(area, alpha, clip);
                }
            }
            Kind::Video => {
                if self.view.elements[id].flag("windowless", false) {
                    self.video(id, area, clip);
                } else {
                    self.on_top.push((id, area, alpha, clip.clone()));
                }
            }
            Kind::Playlist => self.on_top.push((id, area, alpha, clip.clone())),
            Kind::ListBox => self.list(id, area, clip),
            Kind::EditBox => self.edit(id, area, clip),
            _ => {}
        }
    }

    fn effects(&mut self, area: Box2, alpha: f64, clip: &Clip) {
        // A skin fades the sub-view it is in out to show what is under it.
        let alpha = alpha as u8;
        if alpha == 0 {
            return;
        }
        // Visualisations draw on black.
        self.out.push(Paint::Fill {
            area,
            color: [0, 0, 0],
            alpha,
            clip: clip.clone(),
        });
        self.out.push(Paint::Effects {
            area,
            alpha,
            clip: clip.clone(),
        });
    }

    fn video(&mut self, id: ElementId, area: Box2, clip: &Clip) {
        let background = self.view.elements[id].color("backgroundcolor");
        self.out.push(Paint::Art {
            area,
            background,
            clip: clip.clone(),
        });
    }

    fn container(&mut self, id: ElementId, x: f64, y: f64, area: Box2, alpha: f64, clip: &Clip) {
        let order = self.view.in_order(id);
        let element = &self.view.elements[id];
        let root = id == self.view.root;
        let name = element.image_name("backgroundimage");
        let plain = clip_to(clip, area);
        let clipping = element.str("clippingcolor");
        // A tiled picture has no one shape to cut to: Gorillaz fills its whole
        // window with a tile of nothing but the clipping colour.
        let tiled = element.flag("backgroundtiled", false);
        let outline = (!tiled && clipping.is_none() && !root)
            .then(|| name.as_ref().zip(element.str("transparencycolor")))
            .flatten();
        let key = name.as_ref().map(|name| self.key_for(id, name));
        let picture = key
            .as_ref()
            .and_then(|key| self.current(id, "backgroundimage", key));
        // The clipping colour cuts the picture into the sub-view's shape,
        // children included. Only that colour: a transparent hole is a
        // window onto what lies under the picture, not outside the shape.
        let shape = clipping
            .filter(|_| !tiled)
            .zip(name.as_ref())
            .and_then(|(clipping, name)| {
                let mut key = ImageKey {
                    file: super::file_name(name),
                    ..ImageKey::default()
                };
                if clipping.trim().eq_ignore_ascii_case("auto") {
                    key.clear_corner = true;
                } else {
                    key.clear.extend(color(&clipping));
                }
                self.images.get(&key)
            });
        let inner = match shape {
            Some(shape) => clip_to_shape(&plain, &shape.runs(), (area.x, area.y)),
            None => plain.clone(),
        };
        // A picture with see-through pixels of its own and a transparency
        // colour as well: its pixels are the window, the colour is what lies
        // outside it, and what shows through stays within that outline.
        let window = outline.and_then(|(name, colour)| {
            let key = ImageKey {
                file: super::file_name(name),
                clear: color(&colour).into_iter().collect(),
                window_only_clear: true,
                ..ImageKey::default()
            };
            self.images.get(&key)
        });
        let under = match window {
            Some(window) => clip_to_shape(&inner, &window.runs(), (area.x, area.y)),
            None => inner.clone(),
        };

        let element = &self.view.elements[id];
        let fill = element
            .str("backgroundcolor")
            .map(|text| color(&text))
            .unwrap_or(if root { Some([255, 255, 255]) } else { None });
        // A view's picture is its whole shape: its default white must not
        // show through the holes.
        if let Some(fill) = fill
            && !(root && key.is_some())
        {
            self.out.push(Paint::Fill {
                area,
                color: fill,
                alpha: alpha as u8,
                clip: inner.clone(),
            });
        }
        // The background sits at zIndex 0, so a negative child goes under
        // it and shows through its holes.
        let split = order.partition_point(|child| self.view.elements[*child].z < 0);
        for child in &order[..split] {
            self.element(*child, x, y, alpha, &under);
        }
        if name.as_deref().is_some_and(is_album_art) {
            self.out.push(Paint::Art {
                area,
                background: None,
                clip: inner.clone(),
            });
        } else if let Some((picture, frame)) = picture {
            let element = &self.view.elements[id];
            let stretch = element.flag("resizebackgroundimage", false);
            let tiled = element.flag("backgroundtiled", false);
            let (bitmap_width, bitmap_height) = frame_size(&picture, frame);
            let whole = Box2 {
                x: 0,
                y: 0,
                width: bitmap_width,
                height: bitmap_height,
            };
            if tiled {
                self.tile(&picture, frame, area, alpha, &inner);
            } else {
                self.out.push(Paint::Picture {
                    picture: picture.clone(),
                    frame,
                    x: area.x,
                    y: area.y,
                    part: whole,
                    stretch: stretch.then_some((area.width, area.height)),
                    alpha: alpha as u8,
                    clip: plain.clone(),
                });
            }
        }
        for child in &order[split..] {
            self.element(*child, x, y, alpha, &inner);
        }
    }

    /// The picture for `key`, or the one last shown in this slot while
    /// the new one is decoded. Notes when an animation wants a frame.
    fn current(
        &mut self,
        id: ElementId,
        slot: &'static str,
        key: &ImageKey,
    ) -> Option<(Arc<Picture>, usize)> {
        let now = self.now;
        let previous = self.view.elements[id].shown.get(slot).cloned();
        let same = previous.as_ref().is_some_and(|(shown, _)| shown == key);
        let picture = self.images.get(key);
        let (picture, since) = match (picture, previous) {
            (Some(picture), _) if same => {
                let since = self.view.elements[id].shown[slot].1;
                (picture, since)
            }
            (Some(picture), _) => {
                self.view.elements[id]
                    .shown
                    .insert(slot, (key.clone(), now));
                (picture, now)
            }
            (None, Some((shown, since))) => (self.images.get(&shown)?, since),
            (None, None) => return None,
        };
        let elapsed = now.saturating_duration_since(since).as_millis() as u64;
        let (frame, next) = picture.frame_at(elapsed);
        if let Some(next) = next {
            soonest(&mut self.wake_ms, next);
        }
        Some((picture, frame))
    }

    /// The see-through colours of a picture of this element: its own, and
    /// its view's, which key everything in the window.
    fn key_for(&self, id: ElementId, name: &str) -> ImageKey {
        let mut key = ImageKey {
            file: super::file_name(name),
            ..ImageKey::default()
        };
        let root = &self.view.elements[self.view.root];
        let element = &self.view.elements[id];
        let mut colours = vec![
            element.str("transparencycolor"),
            matches!(element.kind, Kind::View | Kind::SubView)
                .then(|| element.str("clippingcolor"))
                .flatten(),
            root.str("transparencycolor"),
            root.str("clippingcolor"),
        ];
        // A sub-view's colour holds for everything in it: xXx and Wild Well
        // Control name it on the group, not on each button.
        let mut parent = element.parent;
        while let Some(at) = parent {
            colours.push(self.view.elements[at].str("transparencycolor"));
            parent = self.view.elements[at].parent;
        }
        // World of Warcraft's strips name no colour, though each is cut out
        // of magenta like the 47 pictures that do. Nothing means pure magenta.
        if element.kind == Kind::CustomSlider {
            colours.push(Some("#ff00ff".to_owned()));
        }
        for text in colours.into_iter().flatten() {
            let text = text.trim().to_ascii_lowercase();
            if text == "auto" {
                key.clear_corner = true;
            } else if let Some(color) = color(&text)
                && !key.clear.contains(&color)
            {
                key.clear.push(color);
            }
        }
        if let Some(clip) = element.image_name("clippingimage") {
            key.cut = Some((super::file_name(&clip), element.color("clippingcolor")));
        }
        key
    }

    /// One picture of an element, at its corner, cut to its size.
    fn state_picture(&mut self, id: ElementId, attr: &str, area: Box2, alpha: f64, clip: &Clip) {
        let Some(name) = self.view.elements[id].image_name(attr) else {
            return;
        };
        let key = self.key_for(id, &name);
        if let Some((picture, frame)) = self.current(id, "state", &key) {
            let (width, height) = frame_size(&picture, frame);
            self.out.push(Paint::Picture {
                picture,
                frame,
                x: area.x,
                y: area.y,
                part: Box2 {
                    x: 0,
                    y: 0,
                    width: width.min(area.width),
                    height: height.min(area.height),
                },
                stretch: None,
                alpha: alpha as u8,
                clip: clip.clone(),
            });
        }
    }

    /// Repeats a picture over an area.
    fn tile(&mut self, picture: &Arc<Picture>, frame: usize, area: Box2, alpha: f64, clip: &Clip) {
        let (width, height) = frame_size(picture, frame);
        if width <= 0 || height <= 0 {
            return;
        }
        for row in 0..MAX_TILES {
            let y = row.saturating_mul(height);
            if y >= area.height {
                break;
            }
            for column in 0..MAX_TILES {
                let x = column.saturating_mul(width);
                if x >= area.width {
                    break;
                }
                self.out.push(Paint::Picture {
                    picture: picture.clone(),
                    frame,
                    x: area.x.saturating_add(x),
                    y: area.y.saturating_add(y),
                    part: Box2 {
                        x: 0,
                        y: 0,
                        width: width.min(area.width - x),
                        height: height.min(area.height - y),
                    },
                    stretch: None,
                    alpha: alpha as u8,
                    clip: clip.clone(),
                });
            }
        }
    }

    /// The attribute naming the picture a button shows now.
    fn button_state(&self, id: ElementId, owner: ElementId) -> Option<String> {
        let view = &*self.view;
        let element = &view.elements[id];
        let images = &view.elements[owner];
        let hovered = view.hover == Some(id);
        let pressed = view.pressed == Some(id) && hovered;
        let down = element.flag("down", false);
        let name = |attr: &str| images.image_name(attr);
        if !element.enabled {
            return name("disabledimage").or_else(|| name("image"));
        }
        let chosen = if (pressed || down) && hovered {
            name("hoverdownimage").or_else(|| name("downimage"))
        } else if pressed || down {
            name("downimage")
        } else if hovered {
            name("hoverimage")
        } else {
            None
        };
        chosen.or_else(|| name("image"))
    }

    fn button(&mut self, id: ElementId, area: Box2, alpha: f64, clip: &Clip) {
        let Some(name) = self.button_state(id, id) else {
            return;
        };
        if is_album_art(&name) {
            self.out.push(Paint::Art {
                area,
                background: None,
                clip: clip.clone(),
            });
            return;
        }
        let key = self.key_for(id, &name);
        let Some((picture, frame)) = self.current(id, "state", &key) else {
            return;
        };
        let inside = clip_to(clip, area);
        if self.view.elements[id].flag("tiled", false) {
            self.tile(&picture, frame, area, alpha, &inside);
            return;
        }
        let (width, height) = frame_size(&picture, frame);
        self.out.push(Paint::Picture {
            picture,
            frame,
            x: area.x,
            y: area.y,
            part: Box2 {
                x: 0,
                y: 0,
                width,
                height,
            },
            stretch: None,
            alpha: alpha as u8,
            clip: inside,
        });
    }

    /// One button of a group: only its own pixels of the group's picture.
    fn mapped_button(
        &mut self,
        id: ElementId,
        group: ElementId,
        area: Box2,
        alpha: f64,
        clip: &Clip,
    ) {
        let element = &self.view.elements[id];
        if !element.visible || element.kind != Kind::ButtonElement {
            return;
        }
        let Some(mapping) = element.color("mappingcolor") else {
            return;
        };
        let Some(map_name) = self.view.elements[group].image_name("mappingimage") else {
            return;
        };
        let Some(name) = self.button_state(id, group) else {
            return;
        };
        let mut key = self.key_for(group, &name);
        key.mask = Some((super::file_name(&map_name), mapping));
        let Some((picture, frame)) = self.current(id, "state", &key) else {
            return;
        };
        let (width, height) = frame_size(&picture, frame);
        self.out.push(Paint::Picture {
            x: area.x.saturating_add(picture.origin.0),
            y: area.y.saturating_add(picture.origin.1),
            part: Box2 {
                x: 0,
                y: 0,
                width,
                height,
            },
            picture,
            frame,
            stretch: None,
            alpha: alpha as u8,
            clip: clip.clone(),
        });
    }

    /// Lays a picture along a slider: the ends of `border` pixels drawn once
    /// and the middle repeated. WMP never stretches these. `length` is how
    /// far it runs from the near end.
    #[allow(clippy::too_many_arguments)]
    fn run_along(
        &mut self,
        picture: &Arc<Picture>,
        frame: usize,
        area: Box2,
        vertical: bool,
        border: i32,
        from: i32,
        to: i32,
        alpha: f64,
        clip: &Clip,
    ) {
        let (width, height) = frame_size(picture, frame);
        let (image_length, whole) = if vertical {
            (height, area.height)
        } else {
            (width, area.width)
        };
        if image_length <= 0 || whole <= 0 {
            return;
        }
        let border = border.min(image_length / 2).max(0);
        let middle = (image_length - 2 * border).max(1);
        let slide = |start: i32, source: i32, length: i32| -> (Box2, i32, i32) {
            if vertical {
                (
                    Box2 {
                        x: 0,
                        y: source,
                        width,
                        height: length,
                    },
                    area.x,
                    area.y.saturating_add(start),
                )
            } else {
                (
                    Box2 {
                        x: source,
                        y: 0,
                        width: length,
                        height,
                    },
                    area.x.saturating_add(start),
                    area.y,
                )
            }
        };
        // The pieces, in slider coordinates, before cutting to from..to.
        let mut pieces = vec![
            (0, 0, border),
            (whole - border, image_length - border, border),
        ];
        let mut at = border;
        let mut count = 0;
        while at < whole - border && count < MAX_TILES {
            let length = middle.min(whole - border - at);
            pieces.push((at, border, length));
            at += length;
            count += 1;
        }
        let inside = clip_to(clip, area);
        for (start, source, length) in pieces {
            let (start, source, length) = cut(start, source, length, from, to);
            if length <= 0 {
                continue;
            }
            let (part, x, y) = slide(start, source, length);
            self.out.push(Paint::Picture {
                picture: picture.clone(),
                frame,
                x,
                y,
                part,
                stretch: None,
                alpha: alpha as u8,
                clip: inside.clone(),
            });
        }
    }

    fn slider(&mut self, id: ElementId, area: Box2, alpha: f64, clip: &Clip) {
        let element = &self.view.elements[id];
        let vertical = slider_vertical(element);
        let border = element.num("bordersize", 0.0).max(0.0) as i32;
        let enabled = element.enabled;
        let hovered = self.view.hover == Some(id);
        let dragging = self.view.dragging == Some(id);
        let thumb_name = |attr: &str| element.image_name(attr);
        let thumb = if !enabled {
            thumb_name("thumbdisabledimage").or_else(|| thumb_name("thumbimage"))
        } else if dragging {
            thumb_name("thumbdownimage").or_else(|| thumb_name("thumbimage"))
        } else if hovered {
            thumb_name("thumbhoverimage").or_else(|| thumb_name("thumbimage"))
        } else {
            thumb_name("thumbimage")
        };
        let background = if !enabled {
            thumb_name("disabledimage").or_else(|| thumb_name("backgroundimage"))
        } else if hovered {
            thumb_name("backgroundhoverimage").or_else(|| thumb_name("backgroundimage"))
        } else {
            thumb_name("backgroundimage")
        };
        let foreground = enabled
            .then(|| {
                if hovered {
                    thumb_name("foregroundhoverimage").or_else(|| thumb_name("foregroundimage"))
                } else {
                    thumb_name("foregroundimage")
                }
            })
            .flatten();
        let slides = element.flag("slide", true);
        let back_color = element.color("backgroundcolor");
        let fore_color = element.str("foregroundcolor").and_then(|text| color(&text));
        let length = if vertical { area.height } else { area.width };
        let edge = slider_edge(element, length, border);
        // The near end of the value: the left, or the bottom.
        let (from, to) = if vertical { (edge, length) } else { (0, edge) };
        if let Some(back_color) = back_color {
            self.out.push(Paint::Fill {
                area,
                color: back_color,
                alpha: alpha as u8,
                clip: clip.clone(),
            });
        }
        if let Some(name) = background {
            let key = self.key_for(id, &name);
            if let Some((picture, frame)) = self.current(id, "background", &key) {
                self.run_along(
                    &picture, frame, area, vertical, border, 0, length, alpha, clip,
                );
            }
        }
        if let Some(fore_color) = fore_color {
            let part = if vertical {
                Box2 {
                    y: area.y.saturating_add(from),
                    height: to - from,
                    ..area
                }
            } else {
                Box2 {
                    width: to - from,
                    ..area
                }
            };
            self.out.push(Paint::Fill {
                area: part,
                color: fore_color,
                alpha: alpha as u8,
                clip: clip.clone(),
            });
        }
        if let Some(name) = foreground {
            let key = self.key_for(id, &name);
            if let Some((picture, frame)) = self.current(id, "foreground", &key) {
                // A sliding foreground is pulled along by the thumb: its far
                // end stays at the thumb.
                let shift = if slides {
                    if vertical { from } else { to - length }
                } else {
                    0
                };
                let moved = if vertical {
                    Box2 {
                        y: area.y.saturating_add(shift),
                        ..area
                    }
                } else {
                    Box2 {
                        x: area.x.saturating_add(shift),
                        ..area
                    }
                };
                let inside = clip_to(
                    clip,
                    if vertical {
                        Box2 {
                            y: area.y.saturating_add(from),
                            height: to - from,
                            ..area
                        }
                    } else {
                        Box2 {
                            width: to - from,
                            ..area
                        }
                    },
                );
                self.run_along(
                    &picture, frame, moved, vertical, border, 0, length, alpha, &inside,
                );
            }
        }
        if let Some(name) = thumb {
            let key = self.key_for(id, &name);
            if let Some((picture, frame)) = self.current(id, "thumb", &key) {
                let (width, height) = frame_size(&picture, frame);
                // The thumb's middle sits on the edge; across the slider it
                // lines up with the track's top or left.
                let (x, y) = if vertical {
                    (
                        area.x,
                        area.y.saturating_add(edge).saturating_sub(height / 2),
                    )
                } else {
                    (
                        area.x.saturating_add(edge).saturating_sub(width / 2),
                        area.y,
                    )
                };
                self.out.push(Paint::Picture {
                    picture,
                    frame,
                    x,
                    y,
                    part: Box2 {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                    stretch: None,
                    alpha: alpha as u8,
                    clip: clip.clone(),
                });
            }
        }
    }

    fn custom_slider(&mut self, id: ElementId, area: Box2, alpha: f64, clip: &Clip) {
        let element = &self.view.elements[id];
        let (min, max) = (element.num("min", 0.0), element.num("max", 100.0));
        let value = element.num("value", min);
        let attr = if !element.enabled && element.image_name("disabledimage").is_some() {
            "disabledimage"
        } else if self.view.dragging == Some(id) && element.image_name("downimage").is_some() {
            "downimage"
        } else if self.view.hover == Some(id) && element.image_name("hoverimage").is_some() {
            "hoverimage"
        } else {
            "image"
        };
        let Some(name) = element.image_name(attr) else {
            return;
        };
        let key = self.key_for(id, &name);
        let Some((picture, frame)) = self.current(id, "state", &key) else {
            return;
        };
        let (width, height) = frame_size(&picture, frame);
        let across = width > area.width && area.width > 0;
        let count = if across {
            width / area.width.max(1)
        } else {
            height / area.height.max(1)
        }
        .max(1);
        // A skin's strip may run against its map; the value does not.
        let map = self.view.elements[id]
            .image_name("positionimage")
            .map(|name| super::file_name(&name))
            .and_then(|name| {
                self.view
                    .wmp
                    .maps
                    .get(&name)
                    .cloned()
                    .map(|map| (name, map))
            });
        let reversed = map.is_some_and(|(name, map)| {
            picture.strip_reversed(
                &name,
                &map,
                Strip {
                    width: area.width.max(0) as u32,
                    height: area.height.max(0) as u32,
                    count: count as u32,
                    across,
                },
            )
        });
        let index = custom_index(value, min, max, count, reversed);
        self.out.push(Paint::Picture {
            picture,
            frame,
            x: area.x,
            y: area.y,
            part: Box2 {
                x: if across { index * area.width } else { 0 },
                y: if across { 0 } else { index * area.height },
                width: area.width,
                height: area.height,
            },
            stretch: None,
            alpha: alpha as u8,
            clip: clip.clone(),
        });
    }

    fn text(&mut self, id: ElementId, area: Box2, alpha: f64, clip: &Clip) {
        let (size_px, bold, italic, underline, strikeout, mono) = font_of(&self.view.elements[id]);
        let element = &self.view.elements[id];
        let hovered = self.view.hover == Some(id);
        let state_color = |base: &str| {
            let prefixed = if !element.enabled {
                format!("disabled{base}")
            } else if hovered {
                format!("hover{base}")
            } else {
                base.to_owned()
            };
            element.color(&prefixed).or_else(|| element.color(base))
        };
        let foreground = state_color("foregroundcolor").unwrap_or([0, 0, 0]);
        let background = state_color("backgroundcolor");
        let text = element.props.get("value").map(text_of).unwrap_or_default();
        let scrolling = element.flag("scrolling", false);
        let wrap = element.flag("wordwrap", false);
        let align = match element
            .str("justification")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "center" => 0,
            "right" => 1,
            _ => -1,
        };
        let auto_width = element.auto_width;
        let auto_height = element.auto_height;
        let amount = element.num("scrollingamount", 6.0).max(1.0) as f32;
        let delay = element.num("scrollingdelay", 85.0);
        let delay = if delay < 30.0 { 85.0 } else { delay } as f32;
        let backwards = element
            .str("scrollingdirection")
            .is_some_and(|text| text.eq_ignore_ascii_case("right"));
        let since = element.scrolling_since;
        let measured = (self.measure)(&text, size_px, bold, mono);
        let area = Box2 {
            width: if auto_width {
                measured.ceil() as i32 + 1
            } else {
                area.width
            },
            height: if auto_height {
                line_height(size_px) as i32
            } else {
                area.height
            },
            ..area
        };
        if let Some(background) = background {
            self.out.push(Paint::Fill {
                area,
                color: background,
                alpha: alpha as u8,
                clip: clip.clone(),
            });
        }
        if text.is_empty() {
            return;
        }
        let mut scroll = 0.0;
        if scrolling {
            let started = match since {
                Some(started) => started,
                None => {
                    self.view.elements[id].scrolling_since = Some(self.now);
                    self.now
                }
            };
            let ticks = (self.now.saturating_duration_since(started).as_secs_f32() * 1000.0
                / delay)
                .floor();
            let cycle = (measured + SCROLL_GAP).max(1.0);
            scroll = (ticks * amount).rem_euclid(cycle);
            if backwards {
                scroll = -scroll;
            }
            soonest(&mut self.wake_ms, delay as u64);
        } else if self.view.elements[id].scrolling_since.is_some() {
            self.view.elements[id].scrolling_since = None;
        }
        let shown = if scrolling || wrap || measured <= area.width as f32 + 0.5 {
            text
        } else {
            ellipsised(&text, area.width as f32, self.measure, size_px, bold, mono)
        };
        let inside = clip_to(clip, area);
        self.out.push(Paint::Text {
            text: TextPaint {
                text: shown,
                area,
                size_px,
                color: foreground,
                alpha: alpha as u8,
                bold,
                italic,
                underline,
                strikeout,
                mono,
                align: if scrolling { -1 } else { align },
                scroll,
                wrap: wrap && !scrolling,
            },
            clip: inside,
        });
    }

    /// A playlist or list box: its background, then a row of text each.
    fn list(&mut self, id: ElementId, area: Box2, clip: &Clip) {
        let rows = list_rows(self.view, self.deck, id);
        let element = &self.view.elements[id];
        let default_font = element.props.contains_key("fontsize");
        let size_px = if default_font {
            font_of(element).0
        } else {
            8.0 * PIXELS_PER_POINT
        };
        let foreground = element.color("foregroundcolor").unwrap_or([0, 0, 0]);
        let background = element
            .str("backgroundcolor")
            .map_or(Some([255, 255, 255]), |text| color(&text));
        let playing_fg = element.color("itemplayingcolor").unwrap_or([0, 255, 0]);
        let playing_bg = element.color("itemplayingbackgroundcolor");
        let selected_fg = element
            .color("itemselectedcolor")
            .unwrap_or([255, 255, 255]);
        let selected_bg = element
            .color("itemselectedbackgroundcolor")
            .unwrap_or([10, 36, 106]);
        let scroll = element.scroll;
        let selected = element.selected;
        let is_playlist = element.kind == Kind::Playlist;
        let inside = clip_to(clip, area);
        if let Some(background) = background {
            self.out.push(Paint::Fill {
                area,
                color: background,
                alpha: 255,
                clip: inside.clone(),
            });
        }
        let visible = (f64::from(area.height) / ROW_HEIGHT).ceil() as usize;
        let first = scroll.max(0.0) as usize;
        let playing = self.deck.playing;
        for (index, row) in rows.iter().enumerate().skip(first).take(visible + 1) {
            let top = area.y + ((index - first) as f64 * ROW_HEIGHT) as i32;
            let row_area = Box2 {
                x: area.x,
                y: top,
                width: area.width,
                height: ROW_HEIGHT as i32,
            };
            let is_playing = is_playlist && playing == Some(index);
            let is_selected = selected == Some(index);
            let fill = if is_selected {
                Some(selected_bg)
            } else if is_playing {
                playing_bg
            } else {
                None
            };
            if let Some(fill) = fill {
                self.out.push(Paint::Fill {
                    area: row_area,
                    color: fill,
                    alpha: 255,
                    clip: inside.clone(),
                });
            }
            let color = if is_selected {
                selected_fg
            } else if is_playing {
                playing_fg
            } else {
                foreground
            };
            let right_width = if row.1.is_empty() {
                0
            } else {
                (self.measure)(&row.1, size_px, false, false).ceil() as i32 + 6
            };
            let left_area = Box2 {
                x: row_area.x + 3,
                width: (row_area.width - right_width - 6).max(0),
                ..row_area
            };
            let label = ellipsised(
                &row.0,
                left_area.width as f32,
                self.measure,
                size_px,
                false,
                false,
            );
            let mut paint = TextPaint {
                text: label,
                area: left_area,
                size_px,
                color,
                alpha: 255,
                bold: false,
                italic: false,
                underline: false,
                strikeout: false,
                mono: false,
                align: -1,
                scroll: 0.0,
                wrap: false,
            };
            self.out.push(Paint::Text {
                text: paint.clone(),
                clip: inside.clone(),
            });
            if right_width > 0 {
                paint.text = row.1.clone();
                paint.area = Box2 {
                    x: row_area.x + row_area.width - right_width,
                    width: right_width - 3,
                    ..row_area
                };
                paint.align = 1;
                self.out.push(Paint::Text {
                    text: paint,
                    clip: inside.clone(),
                });
            }
        }
        // A thin bar says there is more.
        if rows.len() > visible && !rows.is_empty() {
            let height = (f64::from(area.height) * visible as f64 / rows.len() as f64) as i32;
            let top = (f64::from(area.height) * scroll / rows.len() as f64) as i32;
            self.out.push(Paint::Fill {
                area: Box2 {
                    x: area.x + area.width - 3,
                    y: area.y + top,
                    width: 2,
                    height: height.max(6),
                },
                color: foreground,
                alpha: 120,
                clip: inside,
            });
        }
    }

    fn edit(&mut self, id: ElementId, area: Box2, clip: &Clip) {
        let element = &self.view.elements[id];
        let (size_px, bold, italic, underline, strikeout, mono) = font_of(element);
        let foreground = element.color("foregroundcolor").unwrap_or([0, 0, 0]);
        let background = element
            .str("backgroundcolor")
            .map_or(Some([255, 255, 255]), |text| color(&text));
        let text = element.props.get("value").map(text_of).unwrap_or_default();
        let focused = self.view.focus == Some(id);
        let inside = clip_to(clip, area);
        if let Some(background) = background {
            self.out.push(Paint::Fill {
                area,
                color: background,
                alpha: 255,
                clip: inside.clone(),
            });
        }
        let width = (self.measure)(&text, size_px, bold, mono);
        self.out.push(Paint::Text {
            text: TextPaint {
                text,
                area: Box2 {
                    x: area.x + 2,
                    width: (area.width - 4).max(0),
                    ..area
                },
                size_px,
                color: foreground,
                alpha: 255,
                bold,
                italic,
                underline,
                strikeout,
                mono,
                align: -1,
                scroll: 0.0,
                wrap: false,
            },
            clip: inside.clone(),
        });
        if focused {
            self.out.push(Paint::Fill {
                area: Box2 {
                    x: area.x + 2 + width.ceil() as i32,
                    y: area.y + 1,
                    width: 1,
                    height: (area.height - 2).max(1),
                },
                color: foreground,
                alpha: 255,
                clip: inside,
            });
        }
    }
}

/// WMP's name for the playing song's cover, as a picture.
fn is_album_art(name: &str) -> bool {
    name.len() >= 17 && name.as_bytes()[..17].eq_ignore_ascii_case(b"wmpimage_albumart")
}

fn frame_size(picture: &Picture, frame: usize) -> (i32, i32) {
    picture.frames.get(frame).map_or((0, 0), |frame| {
        (frame.bitmap.width as i32, frame.bitmap.height as i32)
    })
}

/// Cuts a piece of a strip to the part from..to of the strip.
fn cut(start: i32, source: i32, length: i32, from: i32, to: i32) -> (i32, i32, i32) {
    let begin = start.max(from);
    let end = (start + length).min(to);
    (begin, source + (begin - start), end - begin)
}

pub fn slider_vertical(element: &Element) -> bool {
    element
        .str("direction")
        .is_some_and(|direction| direction.eq_ignore_ascii_case("vertical"))
}

/// How far along the slider the value's edge is, from the left or top:
/// the track is the slider less its borders, and `max` below `min` runs
/// the other way.
pub fn slider_edge(element: &Element, length: i32, border: i32) -> i32 {
    let (min, max) = (element.num("min", 0.0), element.num("max", 100.0));
    let value = element.num("value", min);
    let span = max - min;
    let share = if span == 0.0 {
        0.0
    } else {
        ((value - min) / span).clamp(0.0, 1.0)
    };
    let track = f64::from((length - 2 * border).max(0));
    let along = (share * track).round() as i32;
    if slider_vertical(element) {
        border + (track as i32 - along)
    } else {
        border + along
    }
}

/// The slider value at a place along it, the opposite of [`slider_edge`].
pub fn slider_value_at(element: &Element, at: f64, length: i32, border: i32) -> f64 {
    let (min, max) = (element.num("min", 0.0), element.num("max", 100.0));
    let track = f64::from((length - 2 * border).max(1));
    let along = ((at - f64::from(border)) / track).clamp(0.0, 1.0);
    let share = if slider_vertical(element) {
        1.0 - along
    } else {
        along
    };
    min + share * (max - min)
}

/// Which picture of a custom slider's strip shows `value`, counting from the
/// far end when the strip is `reversed`.
pub fn custom_index(value: f64, min: f64, max: f64, count: i32, reversed: bool) -> i32 {
    let span = max - min;
    if span == 0.0 || count < 2 {
        return 0;
    }
    let index = (((value - min) / span).clamp(0.0, 1.0) * f64::from(count - 1)).round() as i32;
    if reversed { count - 1 - index } else { index }
}

fn ellipsised(
    text: &str,
    width: f32,
    measure: &Measure,
    size: f32,
    bold: bool,
    mono: bool,
) -> String {
    if measure(text, size, bold, mono) <= width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let middle = (low + high).div_ceil(2);
        let cut: String = chars[..middle].iter().chain(['…'].iter()).collect();
        if measure(&cut, size, bold, mono) <= width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    chars[..low].iter().chain(['…'].iter()).collect()
}

/// A list's rows: the left text, and the right one for a playlist's times.
pub fn list_rows(view: &ViewState, deck: &Deck, id: ElementId) -> Vec<(String, String)> {
    let element = &view.elements[id];
    if element.kind == Kind::Playlist {
        if !element.flag("playlistitemsvisible", true) {
            return Vec::new();
        }
        let two_columns = element
            .str("columns")
            .is_some_and(|columns| columns.to_ascii_lowercase().contains("duration"));
        return deck
            .queue
            .iter()
            .map(|song| {
                let name = if song.artist.is_empty() {
                    song.title.clone()
                } else {
                    format!("{} - {}", song.artist, song.title)
                };
                let time = if two_columns || !element.props.contains_key("columns") {
                    super::deck::clock(song.duration_s)
                } else {
                    String::new()
                };
                (name, time)
            })
            .collect();
    }
    element
        .items
        .iter()
        .map(|item| (item.clone(), String::new()))
        .collect()
}

/// The row of a list a skin-pixel height falls in.
pub fn list_row_at(view: &ViewState, id: ElementId, y: f64) -> usize {
    let element = &view.elements[id];
    (element.scroll.max(0.0) + y / ROW_HEIGHT).max(0.0) as usize
}

/// What is under a skin pixel, in the order things are drawn over each
/// other. Sliders with no thumb, `passThrough`, and hidden things are not
/// there; see-through parts of pictures with no job are not either.
pub fn hit(view: &ViewState, images: &mut Images, x: f64, y: f64) -> Option<ElementId> {
    for id in 0..view.elements.len() {
        if view.elements[id].popped {
            let area = view.world_box(id);
            if area.contains(x as i32, y as i32) {
                return Some(id);
            }
        }
    }
    let (width, height) = view.size();
    if x < 0.0 || y < 0.0 || x >= width || y >= height {
        return None;
    }
    hit_in(view, images, view.root, 0.0, 0.0, x, y)
}

fn hit_in(
    view: &ViewState,
    images: &mut Images,
    id: ElementId,
    ox: f64,
    oy: f64,
    x: f64,
    y: f64,
) -> Option<ElementId> {
    let element = &view.elements[id];
    if !element.visible {
        return None;
    }
    let root = id == view.root;
    let left = if root { 0.0 } else { ox + element.left };
    let top = if root { 0.0 } else { oy + element.top };
    let inside = x >= left && y >= top && x < left + element.width && y < top + element.height;
    let order = view.in_order(id);
    let split = order.partition_point(|child| view.elements[*child].z < 0);
    match element.kind {
        Kind::View | Kind::SubView => {
            if !inside {
                return None;
            }
            for child in order[split..].iter().rev() {
                if let Some(found) = hit_in(view, images, *child, left, top, x, y) {
                    return Some(found);
                }
            }
            if !element.flag("passthrough", false) && solid_at(view, images, id, x - left, y - top)
            {
                return Some(id);
            }
            for child in order[..split].iter().rev() {
                if let Some(found) = hit_in(view, images, *child, left, top, x, y) {
                    return Some(found);
                }
            }
            None
        }
        Kind::ButtonGroup => {
            if !inside {
                return None;
            }
            let map = element
                .image_name("mappingimage")
                .and_then(|name| view.wmp.maps.get(&super::file_name(&name)).cloned())?;
            let pixel = map.pixel((x - left) as u32, (y - top) as u32)?;
            order.iter().rev().copied().find(|child| {
                let button = &view.elements[*child];
                button.visible
                    && button.color("mappingcolor") == Some([pixel[0], pixel[1], pixel[2]])
            })
        }
        Kind::Player | Kind::Equalizer | Kind::VideoSettings | Kind::Other => None,
        Kind::Slider if element.image_name("thumbimage").is_none() => None,
        _ if !inside || element.flag("passthrough", false) => None,
        _ if clipped_at(view, id, x - left, y - top) => None,
        Kind::Button if !element.behaves() => {
            solid_at(view, images, id, x - left, y - top).then_some(id)
        }
        Kind::CustomSlider => {
            let map = element
                .image_name("positionimage")
                .and_then(|name| view.wmp.maps.get(&super::file_name(&name)).cloned())?;
            let pixel = map.pixel((x - left) as u32, (y - top) as u32)?;
            (pixel[0] == pixel[1] && pixel[1] == pixel[2]).then_some(id)
        }
        _ => Some(id),
    }
}

/// Whether a clipping picture takes this place out of the element: not just
/// unseen there, but not clickable.
fn clipped_at(view: &ViewState, id: ElementId, x: f64, y: f64) -> bool {
    let element = &view.elements[id];
    let Some(map) = element
        .image_name("clippingimage")
        .and_then(|name| view.wmp.maps.get(&super::file_name(&name)).cloned())
    else {
        return false;
    };
    let cut = element
        .color("clippingcolor")
        .or_else(|| map.pixel(0, 0).map(|pixel| [pixel[0], pixel[1], pixel[2]]));
    map.pixel(x as u32, y as u32)
        .is_some_and(|pixel| Some([pixel[0], pixel[1], pixel[2]]) == cut)
}

/// Whether this element's own picture has a pixel at a place in it. An
/// undecoded picture counts as solid: better a click on a skin that is
/// still arriving than one through it.
fn solid_at(view: &ViewState, images: &mut Images, id: ElementId, x: f64, y: f64) -> bool {
    let element = &view.elements[id];
    let attr = if element.kind == Kind::Button {
        "image"
    } else {
        "backgroundimage"
    };
    let Some(name) = element.image_name(attr) else {
        return element
            .str("backgroundcolor")
            .is_some_and(|text| color(&text).is_some());
    };
    let Some((key, _)) = element
        .shown
        .values()
        .find(|(key, _)| key.file == super::file_name(&name))
    else {
        return element.width > 0.0;
    };
    let Some(picture) = images.get(key) else {
        return true;
    };
    let Some(frame) = picture.frames.first() else {
        return false;
    };
    if element.flag("backgroundtiled", false) && frame.bitmap.width > 0 && frame.bitmap.height > 0 {
        return frame
            .bitmap
            .pixel(
                x as u32 % frame.bitmap.width,
                y as u32 % frame.bitmap.height,
            )
            .is_some_and(|pixel| pixel[3] > 0);
    }
    frame
        .bitmap
        .pixel(x as u32, y as u32)
        .is_some_and(|pixel| pixel[3] > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_strip_piece_is_cut_to_the_filled_part() {
        assert_eq!(cut(10, 4, 8, 0, 14), (10, 4, 4));
        assert_eq!(cut(10, 4, 8, 12, 40), (12, 6, 6));
        assert!(cut(10, 4, 8, 20, 40).2 <= 0);
    }

    #[test]
    fn custom_sliders_pick_their_picture_by_value() {
        assert_eq!(custom_index(0.0, 0.0, 100.0, 5, false), 0);
        assert_eq!(custom_index(50.0, 0.0, 100.0, 5, false), 2);
        assert_eq!(custom_index(100.0, 0.0, 100.0, 5, false), 4);
        assert_eq!(custom_index(50.0, 0.0, 0.0, 5, false), 0);
    }

    #[test]
    fn a_reversed_strip_is_read_from_its_far_end() {
        assert_eq!(custom_index(0.0, 0.0, 100.0, 5, true), 4);
        assert_eq!(custom_index(25.0, 0.0, 100.0, 5, true), 3);
        assert_eq!(custom_index(100.0, 0.0, 100.0, 5, true), 0);
        assert_eq!(custom_index(50.0, 0.0, 0.0, 5, true), 0);
    }

    #[test]
    fn text_is_cut_with_an_ellipsis() {
        let measure = |text: &str, _: f32, _: bool, _: bool| text.chars().count() as f32 * 10.0;
        assert_eq!(ellipsised("abc", 30.0, &measure, 1.0, false, false), "abc");
        assert_eq!(
            ellipsised("abcdef", 40.0, &measure, 1.0, false, false),
            "abc…"
        );
        assert_eq!(ellipsised("abcdef", 1.0, &measure, 1.0, false, false), "…");
    }
}
