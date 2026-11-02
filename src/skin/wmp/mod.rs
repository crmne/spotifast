//! Windows Media Player skins.
//!
//! A `.wmz` file is a zip holding a `.wms` definition (XML), the bitmaps it
//! names and the JScript that runs it. Real skins are small scripted
//! programs: scripts open drawers, switch views and show and hide elements.
//! So this is an engine in the shape of WMP's own. [`Wmp`] is the skin as
//! read from the file; a [`runtime::Runtime`] runs it, with a live element
//! tree ([`live`]) per open view, scripts in the sandboxed interpreter in
//! [`super::script`], and the `player` object over a snapshot of the player
//! ([`host`], [`deck`]). [`paint`] turns a view into a list of things to
//! draw, which `ui::winamp::wmp` paints.
//!
//! Skins are untrusted: nothing a skin says reaches the file system, the
//! network or a process, and pictures are decoded under size limits. The
//! behaviour follows Microsoft's Skin Programming Reference. Where that is
//! silent it follows what is known of WMP's engine: the notes of
//! data-tuna/nts-desktop-wmp (docs/research/wmp-skin-engine.md), and how
//! WebSkin Player and wmz-player describe real skins behaving. Only those
//! findings are used here, not code.

pub mod deck;
pub mod host;
pub mod live;
pub mod paint;
pub mod picture;
pub mod predefined;
pub mod raster;
pub mod runtime;
pub mod xml;

use std::collections::HashMap;
use std::sync::Arc;

use super::Bitmap;
use xml::Node;

/// The largest script file read, in bytes. The largest in the 175 skins of
/// the Internet Archive's collection is under a tenth of this.
const MAX_SCRIPT: usize = 2 * 1024 * 1024;

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
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }
}

/// A file's lower-case name without its folders: how skins' files are kept.
pub fn file_name(name: &str) -> String {
    name.rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// A skin as read from its file.
#[derive(Debug, Default)]
pub struct Wmp {
    pub title: String,
    pub author: String,
    /// The definition's file name, without `.wms`.
    pub stem: String,
    pub views: Vec<Node>,
    start: Option<String>,
    /// The skin's files by lower-case name.
    pub files: Arc<HashMap<String, Vec<u8>>>,
    /// Pictures read for what is in them, not to be shown: mapping and
    /// position maps, decoded here on the loading thread.
    pub maps: HashMap<String, Arc<Bitmap>>,
    /// Every picture's size, read from its header.
    sizes: HashMap<String, (u32, u32)>,
    /// The size of the first view with one, for opening the window.
    pub width: u32,
    pub height: u32,
}

impl Wmp {
    /// Reads the definition against the skin's `files` (lower-case file
    /// name to bytes). `None` when it has no view. Happens off the window's
    /// thread: it reads each picture's header and decodes the maps.
    pub fn parse(
        definition: &str,
        stem: &str,
        files: Arc<HashMap<String, Vec<u8>>>,
    ) -> Option<Self> {
        let tree = xml::parse(definition);
        let theme = tree.find("theme");
        let mut views = Vec::new();
        collect_views(&tree, &mut views);
        if views.is_empty() {
            return None;
        }
        let mut wmp = Wmp {
            title: theme
                .and_then(|theme| theme.attr("title"))
                .unwrap_or_default()
                .to_owned(),
            author: theme
                .and_then(|theme| theme.attr("author"))
                .unwrap_or_default()
                .to_owned(),
            stem: stem.to_owned(),
            start: theme
                .and_then(|theme| theme.first_attr("currentviewid"))
                .map(str::to_owned),
            files,
            views,
            ..Wmp::default()
        };
        for (name, bytes) in wmp.files.iter() {
            if !is_picture(name) {
                continue;
            }
            let size = image::ImageReader::new(std::io::Cursor::new(bytes))
                .with_guessed_format()
                .ok()
                .and_then(|reader| reader.into_dimensions().ok());
            if let Some(size) = size {
                wmp.sizes.insert(name.clone(), size);
            }
        }
        let mut wanted = Vec::new();
        for view in &wmp.views {
            view.walk(&mut |node| {
                for attr in ["mappingimage", "positionimage", "imagemap", "clippingimage"] {
                    if let Some(name) = node.attr(attr) {
                        wanted.push(file_name(name));
                    }
                }
            });
        }
        for name in wanted {
            if !wmp.maps.contains_key(&name)
                && let Some(bitmap) = wmp.files.get(&name).and_then(|bytes| Bitmap::decode(bytes))
            {
                wmp.maps.insert(name, Arc::new(bitmap));
            }
        }
        let first_size = wmp.views.iter().find_map(|view| {
            let number = |name: &str| {
                view.attr(name)
                    .and_then(|value| value.trim().parse::<f32>().ok())
                    .filter(|value| *value >= 1.0)
            };
            match (number("width"), number("height")) {
                (Some(width), Some(height)) => Some((width as u32, height as u32)),
                _ => view
                    .attr("backgroundimage")
                    .and_then(|name| wmp.size_of(name)),
            }
        });
        let (width, height) = first_size.unwrap_or((300, 200));
        wmp.width = width.clamp(1, 4096);
        wmp.height = height.clamp(1, 4096);
        Some(wmp)
    }

    /// A picture's size by the name a skin gives it.
    pub fn size_of(&self, name: &str) -> Option<(u32, u32)> {
        self.sizes.get(&file_name(name)).copied()
    }

    /// A script file as text. Script files are encoded as definitions are.
    /// One bigger than any real skin's is not read: scripts are parsed on the
    /// window's thread as a view opens.
    pub fn script(&self, name: &str) -> Option<String> {
        self.files
            .get(&file_name(name))
            .filter(|bytes| bytes.len() <= MAX_SCRIPT)
            .map(|bytes| super::decode_definition(bytes))
    }

    /// Which view a skin opens with: the one the theme names, or the first.
    pub fn start_view(&self) -> usize {
        self.start
            .as_deref()
            .and_then(|name| self.view_named(name))
            .unwrap_or(0)
    }

    pub fn view_named(&self, name: &str) -> Option<usize> {
        self.views.iter().position(|view| {
            view.first_attr("id")
                .is_some_and(|id| id.eq_ignore_ascii_case(name))
        })
    }
}

fn is_picture(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, extension)| matches!(extension, "bmp" | "png" | "gif" | "jpg" | "jpeg"))
}

/// Views are children of the theme; a definition with no theme still counts.
fn collect_views(node: &Node, views: &mut Vec<Node>) {
    if node.name == "view" {
        views.push(node.clone());
        return;
    }
    for child in &node.children {
        collect_views(child, views);
    }
}

#[cfg(test)]
mod ribbon_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wide_tests;
