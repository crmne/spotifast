//! Windows Media Player skins.
//!
//! A `.wmz` file is a zip holding a `.wms` skin definition (XML) and the
//! bitmaps it names. This module reads the parts Spotifast can draw: the
//! view's background, push buttons and mapped button groups, the seek and
//! volume sliders, and text showing the song. Scripts are not run; a
//! button's job is read from its name and its `onClick`.

use std::collections::HashMap;

use super::{Bitmap, Rgb};

/// How tall a line is, in skin pixels per point of type.
const POINT_LINE: f32 = 1.8;

/// What a button does here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Play,
    Pause,
    /// One button that does both.
    PlayPause,
    Stop,
    Next,
    Previous,
    Mute,
    Shuffle,
    Repeat,
    /// Back to the big window.
    Close,
    Minimize,
    /// Drawn and hoverable, but with nothing to do.
    Inert,
}

impl Control {
    /// Reads a button's job from its names and script, as WMP skins say
    /// it in many ways: `id="PlayBtn"`, `onClick="jscript:player.controls.play();"`.
    pub fn infer(words: &str) -> Control {
        // `player.controls.pause()` is not a play button.
        let words = words
            .to_ascii_lowercase()
            .replace("player", "")
            .replace("preview", "");
        let has = |needles: &[&str]| needles.iter().any(|needle| words.contains(needle));
        if has(&["playpause", "play_pause", "play/pause", "togglepause"])
            || (has(&["play"]) && has(&["pause"]))
        {
            Control::PlayPause
        } else if has(&["pause"]) {
            Control::Pause
        } else if has(&["stop"]) {
            Control::Stop
        } else if has(&["next", "forward", "fastforward"]) {
            Control::Next
        } else if has(&["prev", "rewind", "seekback"]) {
            Control::Previous
        } else if has(&["mute"]) {
            Control::Mute
        } else if has(&["shuffle"]) {
            Control::Shuffle
        } else if has(&["repeat", "loop"]) {
            Control::Repeat
        } else if has(&["minimize"]) {
            Control::Minimize
        } else if has(&["close", "exit", "quit", "returntomediacenter"]) {
            Control::Close
        } else if has(&["play"]) {
            Control::Play
        } else {
            Control::Inert
        }
    }
}

/// A rectangle in the skin, in skin pixels. Skins place things with
/// negative or large offsets, so these are signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Box2 {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Box2 {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// When a button is on screen: skins show a pause button only while
/// something plays, over the play button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Always,
    Playing,
    Stopped,
}

impl Shown {
    fn of(value: Option<&str>) -> Shown {
        match value
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("wmpenabled:player.controls.pause") => Shown::Playing,
            Some("wmpenabled:player.controls.play") => Shown::Stopped,
            _ => Shown::Always,
        }
    }

    pub fn is(self, playing: bool) -> bool {
        match self {
            Shown::Always => true,
            Shown::Playing => playing,
            Shown::Stopped => !playing,
        }
    }
}

/// A picture that is only there to be seen: a sub-view's background, or a
/// button group's resting picture.
#[derive(Clone, Debug)]
pub struct Picture {
    pub key: String,
    pub x: i32,
    pub y: i32,
    /// The skin's `zIndex`: lower is drawn first.
    pub z: i32,
    /// For a tiled picture: the area it repeats over.
    pub tile: Option<(i32, i32)>,
}

#[derive(Clone, Debug)]
pub struct Button {
    pub shown: Shown,
    pub control: Control,
    pub tooltip: String,
    /// Where it sits and what it covers.
    pub area: Box2,
    /// Image keys. A grouped button's images are the group's, drawn
    /// cropped to `area`.
    pub up: Option<String>,
    pub hover: Option<String>,
    pub down: Option<String>,
    /// For a grouped button: where the group's image sits, and the key of
    /// the mapping bitmap with the colour that picks this button.
    pub mapped: Option<(Box2, String, Rgb)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderKind {
    Seek,
    Volume,
}

#[derive(Clone, Debug)]
pub struct Slider {
    pub kind: SliderKind,
    pub area: Box2,
    pub vertical: bool,
    /// The fill grows from the far end: a downward or leftward slider.
    pub reversed: bool,
    /// Pixels at each end of the track the thumb does not travel.
    pub border: i32,
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub thumb: Option<String>,
    pub thumb_hover: Option<String>,
    pub thumb_down: Option<String>,
}

/// What a text element shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Title,
    Artist,
    Album,
    Elapsed,
    Duration,
    Literal,
}

#[derive(Clone, Debug)]
pub struct Text {
    pub field: Field,
    pub literal: String,
    pub area: Box2,
    pub size: f32,
    pub color: Rgb,
    pub align: Align,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Default)]
pub struct Wmp {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub background: Option<String>,
    pub background_color: Option<Rgb>,
    /// Where the skin shows its visualisation.
    pub effects: Vec<Box2>,
    pub pictures: Vec<Picture>,
    pub buttons: Vec<Button>,
    pub sliders: Vec<Slider>,
    pub texts: Vec<Text>,
    /// Decoded bitmaps by key: the file name, plus `#rrggbb` when a
    /// transparency colour was applied.
    pub images: HashMap<String, Bitmap>,
}

impl Wmp {
    /// Reads the definition `definition` against the bitmaps in `files`
    /// (lower-case file name to bytes). `None` when the skin has no view.
    pub fn parse(definition: &str, files: &HashMap<String, Vec<u8>>) -> Option<Self> {
        let mut parser = Parser {
            files,
            wmp: Wmp::default(),
            transparency: None,
            group: None,
            videos: Vec::new(),
            ids: HashMap::new(),
        };
        parser.run(definition)?;
        let mut wmp = parser.wmp;
        // The picture's screen is the video's place, inside the effects'
        // rectangle, whose corners the skin's shape cuts off.
        for effects in &mut wmp.effects {
            let outer = *effects;
            if let Some(video) = parser.videos.iter().find(|video| {
                outer.contains(video.x, video.y)
                    && outer.contains(video.x + video.width - 1, video.y + video.height - 1)
            }) {
                *effects = *video;
            }
        }
        if wmp.background.is_none() && wmp.background_color.is_none() {
            wmp.crop_to_content();
        }
        if wmp.width == 0 || wmp.height == 0 {
            let background = wmp
                .background
                .as_ref()
                .and_then(|key| wmp.images.get(key))?;
            wmp.width = background.width;
            wmp.height = background.height;
        }
        wmp.width = wmp.width.clamp(1, 4096);
        wmp.height = wmp.height.clamp(1, 4096);
        Some(wmp)
    }

    /// A view with no picture or colour of its own is only as big as what
    /// is drawn in it: the rest is see-through, and a window the size of
    /// the view would be mostly empty.
    fn crop_to_content(&mut self) {
        let size = |key: &str| {
            self.images
                .get(key)
                .map_or((0, 0), |bitmap| (bitmap.width as i32, bitmap.height as i32))
        };
        let mut boxes: Vec<Box2> = Vec::new();
        for picture in &self.pictures {
            let (width, height) = size(&picture.key);
            boxes.push(Box2 {
                x: picture.x,
                y: picture.y,
                width,
                height,
            });
        }
        boxes.extend(self.buttons.iter().map(|button| button.area));
        boxes.extend(self.sliders.iter().map(|slider| slider.area));
        boxes.extend(self.texts.iter().map(|text| text.area));
        boxes.extend(self.effects.iter().copied());
        let boxes: Vec<Box2> = boxes
            .into_iter()
            .filter(|b| b.width > 0 && b.height > 0)
            .collect();
        let (Some(left), Some(top), Some(right), Some(bottom)) = (
            boxes.iter().map(|b| b.x).min(),
            boxes.iter().map(|b| b.y).min(),
            boxes.iter().map(|b| b.x + b.width).max(),
            boxes.iter().map(|b| b.y + b.height).max(),
        ) else {
            return;
        };
        for picture in &mut self.pictures {
            picture.x -= left;
            picture.y -= top;
        }
        for button in &mut self.buttons {
            button.area.x -= left;
            button.area.y -= top;
            if let Some((origin, _, _)) = &mut button.mapped {
                origin.x -= left;
                origin.y -= top;
            }
        }
        for slider in &mut self.sliders {
            slider.area.x -= left;
            slider.area.y -= top;
        }
        for effects in &mut self.effects {
            effects.x -= left;
            effects.y -= top;
        }
        for text in &mut self.texts {
            text.area.x -= left;
            text.area.y -= top;
        }
        self.width = (right - left) as u32;
        self.height = (bottom - top) as u32;
    }

    pub fn image(&self, key: &Option<String>) -> Option<&Bitmap> {
        key.as_ref().and_then(|key| self.images.get(key))
    }
}

/// The fields a mapped group shares among its buttons.
struct Group {
    area: Box2,
    up: Option<String>,
    hover: Option<String>,
    down: Option<String>,
    mapping: Option<String>,
}

struct Parser<'a> {
    files: &'a HashMap<String, Vec<u8>>,
    wmp: Wmp,
    transparency: Option<Rgb>,
    group: Option<Group>,
    videos: Vec<Box2>,
    /// Left, top, width and height of each element by `id`, for the
    /// `jscript:` sums skins use to place one thing by another.
    ids: HashMap<String, [i32; 4]>,
}

impl Parser<'_> {
    fn run(&mut self, definition: &str) -> Option<()> {
        let mut seen_view = false;
        // Offsets of the enclosing sub-views, and how deep a hidden one
        // runs, so its contents are skipped.
        let mut offsets: Vec<(i32, i32)> = Vec::new();
        let mut hidden = 0usize;
        let mut stack: Vec<(String, bool, bool)> = Vec::new();
        for mut tag in tags(definition) {
            if !tag.closing {
                self.resolve(&mut tag);
            }
            if tag.closing {
                if let Some(at) = stack.iter().rposition(|(name, _, _)| *name == tag.name) {
                    for (name, hides, offset) in stack.drain(at..).rev() {
                        if hides {
                            hidden -= 1;
                        }
                        if offset {
                            offsets.pop();
                        }
                        if name == "buttongroup" {
                            self.group = None;
                        }
                    }
                }
                continue;
            }
            let container = matches!(tag.name.as_str(), "subview" | "buttongroup" | "view")
                && !tag.self_closing;
            if tag.name == "view" && seen_view {
                // Only the first view is the player; further ones are other
                // windows of the skin.
                hidden += 1;
                stack.push((tag.name, true, false));
                continue;
            }
            // A sub-view that moves itself (`onEndMove`) is a drawer a script
            // opens; with no scripts it would only be a dead handle.
            // Effects and video are shown and hidden by script, which here
            // is the visualisation's job, so their `visible` is not read.
            let scripted = matches!(tag.name.as_str(), "effects" | "video");
            let hides = hidden > 0
                || (!scripted && tag.attr("visible").is_some_and(is_false))
                || tag.attr("onendmove").is_some();
            if hides {
                if container {
                    hidden += 1;
                    stack.push((tag.name, true, false));
                }
                continue;
            }
            let (dx, dy) = offsets
                .iter()
                .fold((0, 0), |(x, y), (ox, oy)| (x + ox, y + oy));
            match tag.name.as_str() {
                "theme" => {
                    self.wmp.title = tag.attr("title").unwrap_or_default().to_owned();
                }
                "view" => {
                    seen_view = true;
                    self.view(&tag);
                    stack.push((tag.name, false, false));
                }
                "subview" => {
                    let (x, y) = (tag.number("left"), tag.number("top"));
                    offsets.push((x, y));
                    if let Some(key) = self.shaped_image(&tag, "backgroundimage") {
                        self.wmp.pictures.push(Picture {
                            key,
                            x: dx + x,
                            y: dy + y,
                            z: tag.number("zindex"),
                            tile: tag
                                .attr("backgroundtiled")
                                .is_some_and(|value| !is_false(value))
                                .then(|| (tag.number("width"), tag.number("height")))
                                .filter(|(width, height)| *width > 0 && *height > 0),
                        });
                    }
                    if !tag.self_closing {
                        stack.push((tag.name, false, true));
                    } else {
                        offsets.pop();
                    }
                }
                "buttongroup" => {
                    let transparency = self.element_transparency(&tag);
                    let up = self.image(tag.attr("image"), transparency);
                    if let Some(key) = &up {
                        self.wmp.pictures.push(Picture {
                            key: key.clone(),
                            x: dx + tag.number("left"),
                            y: dy + tag.number("top"),
                            z: tag.number("zindex"),
                            tile: None,
                        });
                    }
                    self.group = Some(Group {
                        area: Box2 {
                            x: dx + tag.number("left"),
                            y: dy + tag.number("top"),
                            width: tag.number("width"),
                            height: tag.number("height"),
                        },
                        up,
                        hover: self.image(tag.attr("hoverimage"), transparency),
                        down: self.image(tag.attr("downimage"), transparency),
                        mapping: self.image(tag.attr("mappingimage"), None),
                    });
                    if !tag.self_closing {
                        stack.push((tag.name, false, false));
                    } else {
                        self.group = None;
                    }
                }
                name if name.ends_with("element") && name != "equalizerelement" => {
                    self.grouped_button(&tag)
                }
                name if name.ends_with("button") => self.button(&tag, (dx, dy)),
                "effects" | "video" => {
                    let area = Box2 {
                        x: dx + tag.number("left"),
                        y: dy + tag.number("top"),
                        width: tag.number("width"),
                        height: tag.number("height"),
                    };
                    if area.width > 0 && area.height > 0 {
                        if tag.name == "effects" {
                            self.wmp.effects.push(area);
                        } else {
                            self.videos.push(area);
                        }
                    }
                }
                "slider" => self.slider(&tag, (dx, dy)),
                "text" | "label" => self.text(&tag, (dx, dy)),
                _ => {
                    if container {
                        stack.push((tag.name, false, false));
                    }
                }
            }
        }
        seen_view.then_some(())
    }

    /// Turns `left="jscript:other.left+other.width+10;"` into a number, and
    /// remembers where this element is for the ones after it.
    fn resolve(&mut self, tag: &mut Tag) {
        for name in ["left", "top", "width", "height"] {
            let Some(value) = tag.attr(name) else {
                continue;
            };
            let Some(expression) = value
                .trim()
                .get(..7)
                .filter(|head| head.eq_ignore_ascii_case("jscript"))
                .and_then(|_| value.trim()[7..].strip_prefix(':'))
            else {
                continue;
            };
            let number = self.sum(expression);
            if let Some(attr) = tag.attrs.iter_mut().find(|(key, _)| key == name) {
                attr.1 = number.to_string();
            }
        }
        if let Some(id) = tag.attr("id") {
            let id = id.to_ascii_lowercase();
            let size = |tag: &Tag, name: &str| tag.number(name);
            self.ids.insert(
                id,
                [
                    size(tag, "left"),
                    size(tag, "top"),
                    size(tag, "width"),
                    size(tag, "height"),
                ],
            );
        }
    }

    /// A sum of whole numbers and `id.left`, `id.top`, `id.width`,
    /// `id.height` terms, with + and -. Anything else counts as 0.
    fn sum(&self, expression: &str) -> i32 {
        let mut total = 0i32;
        let mut sign = 1i32;
        let mut term = String::new();
        let finish = |term: &mut String, sign: i32, total: &mut i32| {
            let text = term.trim().to_ascii_lowercase();
            term.clear();
            let value = text.parse::<i32>().unwrap_or_else(|_| {
                text.split_once('.')
                    .and_then(|(id, property)| {
                        let at = ["left", "top", "width", "height"]
                            .iter()
                            .position(|name| *name == property.trim())?;
                        self.ids.get(id.trim()).map(|numbers| numbers[at])
                    })
                    .unwrap_or(0)
            });
            *total += sign * value;
        };
        for character in expression.trim().trim_end_matches(';').chars() {
            match character {
                '+' | '-' if !term.trim().is_empty() => {
                    finish(&mut term, sign, &mut total);
                    sign = if character == '+' { 1 } else { -1 };
                }
                '-' => sign = -sign,
                _ => term.push(character),
            }
        }
        finish(&mut term, sign, &mut total);
        total
    }

    fn view(&mut self, tag: &Tag) {
        self.transparency = tag.attr("transparencycolor").and_then(color);
        self.wmp.width = tag.number("width").max(0) as u32;
        self.wmp.height = tag.number("height").max(0) as u32;
        self.ids.insert(
            "view".into(),
            [0, 0, tag.number("width"), tag.number("height")],
        );
        self.wmp.background_color = tag.attr("backgroundcolor").and_then(color);
        self.wmp.background = self.shaped_image(tag, "backgroundimage");
    }

    fn element_transparency(&self, tag: &Tag) -> Option<Rgb> {
        tag.attr("transparencycolor")
            .and_then(color)
            .or(self.transparency)
    }

    /// Decodes a bitmap by the name a skin gives it, once, with the
    /// colour that stands for "see through" cleared. Returns its key.
    fn image(&mut self, name: Option<&str>, transparency: Option<Rgb>) -> Option<String> {
        self.image_clearing(name, &transparency.into_iter().collect::<Vec<_>>())
    }

    /// A view or sub-view's picture: its transparency colour and its
    /// clipping colour (the part outside the window's shape) both clear.
    fn shaped_image(&mut self, tag: &Tag, name: &str) -> Option<String> {
        let clear: Vec<Rgb> = [
            self.element_transparency(tag),
            tag.attr("clippingcolor").and_then(color),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.image_clearing(tag.attr(name), &clear)
    }

    fn image_clearing(&mut self, name: Option<&str>, clear: &[Rgb]) -> Option<String> {
        let name = name?;
        // Skins write paths with backslashes and any case.
        let base = name.rsplit(['\\', '/']).next()?.trim().to_ascii_lowercase();
        if base.is_empty() {
            return None;
        }
        let mut key = base.clone();
        for [r, g, b] in clear {
            key.push_str(&format!("#{r:02x}{g:02x}{b:02x}"));
        }
        if self.wmp.images.contains_key(&key) {
            return Some(key);
        }
        let mut bitmap = Bitmap::decode(self.files.get(&base)?)?;
        for pixel in bitmap.rgba.as_chunks_mut::<4>().0 {
            if clear.iter().any(|clear| pixel[..3] == *clear) {
                *pixel = [0, 0, 0, 0];
            }
        }
        self.wmp.images.insert(key.clone(), bitmap);
        Some(key)
    }

    fn control(tag: &Tag) -> Control {
        let words = [
            Some(tag.name.as_str()),
            tag.attr("elementid"),
            tag.attr("id"),
            tag.attr("name"),
            tag.attr("onclick"),
            tag.attr("onmouseup"),
            tag.attr("uptooltip"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        Control::infer(&words)
    }

    fn grouped_button(&mut self, tag: &Tag) {
        let Some(group) = &self.group else {
            return;
        };
        let (Some(mapping), Some(map_color)) =
            (&group.mapping, tag.attr("mappingcolor").and_then(color))
        else {
            return;
        };
        let Some(bitmap) = self.wmp.images.get(mapping) else {
            return;
        };
        let Some(region) = region_of(bitmap, map_color) else {
            return;
        };
        let origin = group.area;
        let area = Box2 {
            x: origin.x + region.x,
            y: origin.y + region.y,
            width: region.width,
            height: region.height,
        };
        self.wmp.buttons.push(Button {
            shown: Shown::of(tag.attr("visible")),
            control: Self::control(tag),
            tooltip: tag.attr("uptooltip").unwrap_or_default().to_owned(),
            area,
            up: group.up.clone(),
            hover: group.hover.clone(),
            down: group.down.clone(),
            mapped: Some((
                Box2 {
                    x: origin.x,
                    y: origin.y,
                    ..region
                },
                mapping.clone(),
                map_color,
            )),
        });
    }

    fn button(&mut self, tag: &Tag, (dx, dy): (i32, i32)) {
        let transparency = self.element_transparency(tag);
        let up = self.image(tag.attr("image").or(tag.attr("upimage")), transparency);
        let hover = self.image(tag.attr("hoverimage"), transparency);
        let down = self.image(tag.attr("downimage"), transparency);
        let size = |wmp: &Wmp, key: &Option<String>| {
            wmp.image(key)
                .map_or((0, 0), |bitmap| (bitmap.width as i32, bitmap.height as i32))
        };
        let (image_width, image_height) = size(&self.wmp, &up);
        let width = if tag.attr("width").is_some() {
            tag.number("width")
        } else {
            image_width
        };
        let height = if tag.attr("height").is_some() {
            tag.number("height")
        } else {
            image_height
        };
        self.wmp.buttons.push(Button {
            shown: Shown::of(tag.attr("visible")),
            control: Self::control(tag),
            tooltip: tag.attr("uptooltip").unwrap_or_default().to_owned(),
            area: Box2 {
                x: dx + tag.number("left"),
                y: dy + tag.number("top"),
                width,
                height,
            },
            up,
            hover,
            down,
            mapped: None,
        });
    }

    fn slider(&mut self, tag: &Tag, (dx, dy): (i32, i32)) {
        let words = [tag.attr("value"), tag.attr("id"), tag.attr("name")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        let kind = if words.contains("volume") {
            SliderKind::Volume
        } else if words.contains("position") || words.contains("seek") {
            SliderKind::Seek
        } else {
            return;
        };
        let transparency = self.element_transparency(tag);
        let background = self.image(tag.attr("backgroundimage"), transparency);
        // A slider with no size is its track's picture.
        let (track_width, track_height) = self
            .wmp
            .image(&background)
            .map_or((0, 0), |bitmap| (bitmap.width as i32, bitmap.height as i32));
        let area = Box2 {
            x: dx + tag.number("left"),
            y: dy + tag.number("top"),
            width: match tag.number("width") {
                0 => track_width,
                width => width,
            },
            height: match tag.number("height") {
                0 => track_height,
                height => height,
            },
        };
        let direction = tag
            .attr("direction")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let vertical = match direction.as_str() {
            "up" | "down" | "vertical" => true,
            "left" | "right" | "horizontal" => false,
            _ => area.height > area.width,
        };
        // The direction is the way the thumb moves as the value grows.
        let reversed = matches!(direction.as_str(), "left" | "down");
        let image = |this: &mut Self, name: &str| this.image(tag.attr(name), transparency);
        let slider = Slider {
            kind,
            area,
            vertical,
            reversed,
            border: tag.number("bordersize").max(0),
            background,
            foreground: image(self, "foregroundimage"),
            thumb: image(self, "thumbimage"),
            thumb_hover: image(self, "thumbhoverimage"),
            thumb_down: image(self, "thumbdownimage"),
        };
        self.wmp.sliders.push(slider);
    }

    fn text(&mut self, tag: &Tag, (dx, dy): (i32, i32)) {
        let value = tag.attr("value").unwrap_or_default();
        let lower = value.to_ascii_lowercase();
        let (field, literal) = if let Some(property) = lower.strip_prefix("wmpprop:") {
            let field = if property.contains("positionstring") {
                Field::Elapsed
            } else if property.contains("durationstring") {
                Field::Duration
            } else if property.contains("author") || property.contains("artist") {
                Field::Artist
            } else if property.contains("album") {
                Field::Album
            } else if property.contains("name") || property.contains("title") {
                Field::Title
            } else {
                return;
            };
            (field, String::new())
        } else if lower.starts_with("wmp") {
            return;
        } else {
            (Field::Literal, value.to_owned())
        };
        self.wmp.texts.push(Text {
            field,
            literal,
            area: Box2 {
                x: dx + tag.number("left"),
                y: dy + tag.number("top"),
                width: tag.number("width"),
                // A text with no height is one line of its type.
                height: match tag.number("height") {
                    0 => (tag.number("fontsize").max(8) as f32 * POINT_LINE) as i32,
                    height => height,
                },
            },
            size: match tag.number("fontsize") {
                0 => 10.0,
                size => size.clamp(6, 72) as f32,
            },
            color: tag
                .attr("foregroundcolor")
                .and_then(color)
                .unwrap_or([255, 255, 255]),
            align: match tag
                .attr("justification")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "center" => Align::Center,
                "right" => Align::Right,
                _ => Align::Left,
            },
        });
    }
}

/// The smallest box holding every pixel of `color` in a mapping bitmap.
fn region_of(bitmap: &Bitmap, color: Rgb) -> Option<Box2> {
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, -1, -1);
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            if bitmap.pixel(x, y).is_some_and(|p| p[..3] == color) {
                x0 = x0.min(x as i32);
                y0 = y0.min(y as i32);
                x1 = x1.max(x as i32);
                y1 = y1.max(y as i32);
            }
        }
    }
    (x1 >= 0).then_some(Box2 {
        x: x0,
        y: y0,
        width: x1 - x0 + 1,
        height: y1 - y0 + 1,
    })
}

fn is_false(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "false" | "0")
}

/// `#rrggbb`, `#rgb`, `0xrrggbb` or `rrggbb`.
fn color(value: &str) -> Option<Rgb> {
    let value = value.trim();
    let digits = value
        .strip_prefix('#')
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(value);
    let channel = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    match digits.len() {
        6 => Some([channel(0)?, channel(2)?, channel(4)?]),
        3 => {
            let nibble = |at: usize| u8::from_str_radix(digits.get(at..at + 1)?, 16).ok();
            Some([nibble(0)? * 17, nibble(1)? * 17, nibble(2)? * 17])
        }
        _ => None,
    }
}

#[derive(Debug)]
struct Tag {
    /// Lower case.
    name: String,
    closing: bool,
    self_closing: bool,
    /// Attribute names are lower case: skins write them any way.
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// A pixel count; a missing, odd or percentage value is 0.
    fn number(&self, name: &str) -> i32 {
        self.attr(name)
            .and_then(|value| value.trim().parse::<f32>().ok())
            .map_or(0, |value| value as i32)
    }
}

/// The tags of a document, tolerating what real skins do: unquoted or
/// missing values, stray text, upper-case names, comments.
fn tags(text: &str) -> Vec<Tag> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        if let Some(after) = rest.strip_prefix("!--") {
            rest = after.split_once("-->").map_or("", |(_, tail)| tail);
            continue;
        }
        if rest.starts_with('?') || rest.starts_with('!') {
            rest = rest.split_once('>').map_or("", |(_, tail)| tail);
            continue;
        }
        // The tag ends at the first `>` outside quotes.
        let mut quote = None;
        let mut end = rest.len();
        for (at, character) in rest.char_indices() {
            match (quote, character) {
                (None, '"' | '\'') => quote = Some(character),
                (Some(open), c) if c == open => quote = None,
                (None, '>') => {
                    end = at;
                    break;
                }
                _ => {}
            }
        }
        let body = &rest[..end];
        rest = rest.get(end + 1..).unwrap_or("");
        found.extend(parse_tag(body));
    }
    found
}

fn parse_tag(body: &str) -> Option<Tag> {
    let mut body = body.trim();
    let closing = body.starts_with('/');
    body = body.trim_start_matches('/').trim();
    let self_closing = body.ends_with('/');
    body = body.trim_end_matches('/').trim();
    let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
    let name = body[..name_end].to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let mut attrs = Vec::new();
    let mut rest = &body[name_end..];
    loop {
        rest = rest.trim_start();
        let key_end = rest
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(rest.len());
        if key_end == 0 {
            break;
        }
        let key = rest[..key_end].to_ascii_lowercase();
        rest = rest[key_end..].trim_start();
        let Some(after) = rest.strip_prefix('=') else {
            attrs.push((key, String::new()));
            continue;
        };
        let after = after.trim_start();
        let (value, tail) = match after.chars().next() {
            Some(open @ ('"' | '\'')) => {
                let inner = &after[1..];
                let close = inner.find(open).unwrap_or(inner.len());
                (&inner[..close], inner.get(close + 1..).unwrap_or(""))
            }
            _ => {
                let end = after.find(char::is_whitespace).unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        attrs.push((key, unescape(value)));
        rest = tail;
    }
    Some(Tag {
        name,
        closing,
        self_closing,
        attrs,
    })
}

fn unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| image::Rgb(paint(x, y)));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn files(entries: &[(&str, Vec<u8>)]) -> HashMap<String, Vec<u8>> {
        entries
            .iter()
            .map(|(name, bytes)| ((*name).to_owned(), bytes.clone()))
            .collect()
    }

    #[test]
    fn buttons_are_read_from_their_names_and_scripts() {
        for (words, control) in [
            ("PlayBtn", Control::Play),
            ("jscript:player.controls.pause();", Control::Pause),
            ("PlayPauseBtn", Control::PlayPause),
            ("jscript:player.controls.stop()", Control::Stop),
            ("NextTrack", Control::Next),
            ("jscript:player.controls.previous()", Control::Previous),
            ("jscript:player.settings.mute=true", Control::Mute),
            ("close", Control::Close),
            ("view.minimize();", Control::Minimize),
            ("view.returnToMediaCenter();", Control::Close),
            ("pausebutton", Control::Pause),
            ("nextelement", Control::Next),
            ("prevelement", Control::Previous),
            ("Equalizer", Control::Inert),
            (
                "jscript:player.URL=\'x_056k.asx\'; preview56_btn",
                Control::Inert,
            ),
        ] {
            assert_eq!(Control::infer(words), control, "{words}");
        }
    }

    #[test]
    fn the_tag_reader_forgives_what_real_skins_do() {
        let found = tags(
            "<?xml version='1.0'?><!-- hi > there --><THEME TITLE=\"A &amp; B\">\
             <VIEW width=300 Height='100'/><BUTTON onClick=\"if (a > b) go()\" checked></THEME>",
        );
        let names: Vec<_> = found.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, ["theme", "view", "button", "theme"]);
        assert_eq!(found[0].attr("title"), Some("A & B"));
        assert_eq!(found[1].number("width"), 300);
        assert_eq!(found[1].number("height"), 100);
        assert!(found[1].self_closing && found[3].closing);
        assert_eq!(found[2].attr("onclick"), Some("if (a > b) go()"));
        assert_eq!(found[2].attr("checked"), Some(""));
    }

    #[test]
    fn colours_come_in_the_forms_skins_write() {
        assert_eq!(color("#FF00ff"), Some([255, 0, 255]));
        assert_eq!(color("0x0a0b0c"), Some([10, 11, 12]));
        assert_eq!(color("#f80"), Some([255, 136, 0]));
        assert_eq!(color("red"), None);
    }

    #[test]
    fn a_mapped_group_gives_each_button_the_box_of_its_colour() {
        let map = png(40, 20, |x, _| {
            if x < 10 {
                [0, 255, 0]
            } else if (20..30).contains(&x) {
                [255, 0, 0]
            } else {
                [0, 0, 0]
            }
        });
        let skin = files(&[
            ("bg.png", png(100, 50, |_, _| [9, 9, 9])),
            ("map.png", map),
            ("up.png", png(40, 20, |_, _| [1, 1, 1])),
            ("over.png", png(40, 20, |_, _| [2, 2, 2])),
        ]);
        let wmp = Wmp::parse(
            r##"<THEME title="Test"><VIEW backgroundImage="BG.png" transparencyColor="#ff00ff">
              <BUTTONGROUP left="5" top="6" image="up.png" hoverImage="over.png" mappingImage="map.png">
                <BUTTONELEMENT mappingColor="#00FF00" upToolTip="Play" onClick="jscript:player.controls.play();"/>
                <BUTTONELEMENT mappingColor="#FF0000" upToolTip="Stop"/>
                <BUTTONELEMENT mappingColor="#0000FF" upToolTip="Nowhere"/>
              </BUTTONGROUP></VIEW></THEME>"##,
            &skin,
        )
        .unwrap();
        assert_eq!((wmp.width, wmp.height), (100, 50));
        assert_eq!(wmp.title, "Test");
        assert_eq!(wmp.buttons.len(), 2, "a colour the map lacks is no button");
        assert_eq!(wmp.buttons[0].control, Control::Play);
        assert_eq!(
            wmp.buttons[0].area,
            Box2 {
                x: 5,
                y: 6,
                width: 10,
                height: 20
            }
        );
        assert_eq!(wmp.buttons[1].control, Control::Stop);
        assert_eq!(
            wmp.buttons[1].area,
            Box2 {
                x: 25,
                y: 6,
                width: 10,
                height: 20
            }
        );
    }

    #[test]
    fn plain_buttons_take_their_size_from_their_picture() {
        let skin = files(&[
            ("bg.png", png(60, 30, |_, _| [0, 0, 0])),
            ("play.png", png(12, 14, |_, _| [5, 5, 5])),
        ]);
        let wmp = Wmp::parse(
            r#"<VIEW width="60" height="30" backgroundImage="bg.png">
               <SUBVIEW left="10" top="4"><BUTTON id="play" left="3" top="2" image="play.png"/></SUBVIEW>
               </VIEW>"#,
            &skin,
        )
        .unwrap();
        assert_eq!(wmp.buttons[0].control, Control::Play);
        assert_eq!(
            wmp.buttons[0].area,
            Box2 {
                x: 13,
                y: 6,
                width: 12,
                height: 14
            }
        );
    }

    #[test]
    fn the_transparency_colour_is_cleared() {
        let skin = files(&[(
            "bg.png",
            png(2, 1, |x, _| if x == 0 { [255, 0, 255] } else { [1, 2, 3] }),
        )]);
        let wmp = Wmp::parse(
            r##"<VIEW backgroundImage="bg.png" transparencyColor="#FF00FF"/>"##,
            &skin,
        )
        .unwrap();
        let background = wmp.image(&wmp.background).unwrap();
        assert_eq!(background.pixel(0, 0), Some([0, 0, 0, 0]));
        assert_eq!(background.pixel(1, 0), Some([1, 2, 3, 255]));
    }

    #[test]
    fn sliders_and_text_say_what_they_show() {
        let wmp = Wmp::parse(
            r##"<VIEW width="200" height="80">
              <SLIDER id="SeekSlider" value="wmpprop:player.controls.currentPosition" left="1" top="2" width="100" height="10"/>
              <SLIDER id="Vol" value="wmpprop:player.settings.volume" left="1" top="20" width="10" height="50"/>
              <SLIDER id="Other" value="wmpprop:player.settings.balance" width="10" height="50"/>
              <TEXT value="wmpprop:player.currentMedia.name" left="0" top="0" width="90" height="12" fontSize="12" foregroundColor="#ffcc00" justification="center"/>
              <TEXT value="wmpprop:player.controls.currentPositionString" width="9" height="9"/>
              <TEXT value="Hello" width="9" height="9"/>
              <TEXT value="wmpenabled:player.controls.play" width="9" height="9"/>
              <VIEW width="5" height="5"><TEXT value="Hidden"/></VIEW>
              <SUBVIEW visible="false"><TEXT value="Gone"/></SUBVIEW>
            </VIEW>"##,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(wmp.sliders.len(), 2);
        assert_eq!(wmp.sliders[0].kind, SliderKind::Seek);
        assert!(!wmp.sliders[0].vertical);
        assert_eq!(wmp.sliders[1].kind, SliderKind::Volume);
        assert!(wmp.sliders[1].vertical && !wmp.sliders[1].reversed);
        let fields: Vec<_> = wmp.texts.iter().map(|text| text.field).collect();
        assert_eq!(fields, [Field::Title, Field::Elapsed, Field::Literal]);
        assert_eq!(wmp.texts[0].color, [255, 204, 0]);
        assert_eq!(wmp.texts[0].align, Align::Center);
        assert_eq!(wmp.texts[2].literal, "Hello");
    }

    #[test]
    fn positions_can_be_sums_of_other_elements() {
        let wmp = Wmp::parse(
            r##"<VIEW width="300" height="100" backgroundColor="#000000">
              <SLIDER id="a" value="wmpprop:player.settings.volume" left="10" top="4" width="50" height="8"/>
              <TEXT id="t" value="Hi" left="jscript:a.left+a.width+10;" top="JScript:a.top-1" width="jscript:view.width-a.left" height="9"/>
            </VIEW>"##,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            wmp.texts[0].area,
            Box2 {
                x: 70,
                y: 3,
                width: 290,
                height: 9
            }
        );
    }

    #[test]
    fn a_tiled_picture_remembers_its_area_and_effects_use_the_video_box() {
        let skin = files(&[("t.png", png(4, 4, |_, _| [1, 1, 1]))]);
        let wmp = Wmp::parse(
            r##"<VIEW width="50" height="40" backgroundColor="#000000">
              <SUBVIEW left="2" top="3" width="30" height="20" backgroundImage="t.png" backgroundTiled="true">
                <EFFECTS visible="false" width="30" height="20"/>
                <VIDEO visible="false" left="5" top="4" width="20" height="10"/>
              </SUBVIEW></VIEW>"##,
            &skin,
        )
        .unwrap();
        assert_eq!(wmp.pictures[0].tile, Some((30, 20)));
        assert_eq!(
            wmp.effects,
            [Box2 {
                x: 7,
                y: 7,
                width: 20,
                height: 10
            }]
        );
    }

    #[test]
    fn a_gif_picture_is_read() {
        let mut gif = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(3, 2, image::Rgb([9, 8, 7]))
            .write_to(&mut gif, image::ImageFormat::Gif)
            .unwrap();
        let skin = files(&[("back.gif", gif.into_inner())]);
        let wmp = Wmp::parse(r#"<VIEW backgroundImage="Back.GIF"/>"#, &skin).unwrap();
        assert_eq!((wmp.width, wmp.height), (3, 2));
    }

    #[test]
    fn a_definition_without_a_view_is_not_a_skin() {
        assert!(Wmp::parse("<THEME/>", &HashMap::new()).is_none());
        assert!(Wmp::parse("just text", &HashMap::new()).is_none());
    }
}
