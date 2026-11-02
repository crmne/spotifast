//! A picture of a skin, for its card in Settings.
//!
//! A classic skin's main window is put together from its bitmaps. A Windows
//! Media Player skin runs: its view starts in the real engine, scripts and
//! all, under the engine's own budgets, with a player that is partway through
//! a song, and what it then asks to draw is drawn in software. This all
//! happens on a worker thread, one skin at a time, and nothing here knows
//! about the window. A skin that cannot be drawn gives `None`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::layout::{self, Area};
use super::sprites::{self, Sprite};
use super::wmp::deck::{Deck, PlayState, Song};
use super::wmp::paint::Paint;
use super::wmp::picture::Images;
use super::wmp::runtime::Runtime;
use super::wmp::{Box2, Wmp, raster};
use super::{Bitmap, Mask, Skin, font};

/// A picture with less of it than this share solid is not worth a card: the
/// skin keeps itself hidden until something happens that never does here.
const MIN_SOLID_PERCENT: usize = 2;
/// The longest side a stored preview has, in pixels: twice a card's picture.
pub const MAX_SIDE: u32 = 480;
/// The largest view drawn, however big a script makes it.
const MAX_VIEW: f64 = 4096.0;
/// The turns the skin gets to open its intro and drawers. Time in them is
/// made up, so an intro of many seconds is over in a moment.
const TICKS: u32 = 6;
const TICK: Duration = Duration::from_millis(1500);
/// How long a skin may take in all, in real time.
const BUDGET: Duration = Duration::from_secs(5);
/// How long to wait for a batch of pictures before drawing without them.
const PICTURE_WAIT: Duration = Duration::from_millis(1500);

/// Song and sliders partway along, so a skin shows what it looks like in use.
fn deck() -> Deck {
    let song = |title: &str| Song {
        title: title.to_owned(),
        artist: "Spotifast".to_owned(),
        album: "Spotifast".to_owned(),
        uri: format!("spotify:track:{title}"),
        duration_s: 214.0,
    };
    let queue = vec![song("Song one"), song("Song two"), song("Song three")];
    Deck {
        state: PlayState::Playing,
        song: queue.first().cloned(),
        position_s: 83.0,
        bitrate: 320_000,
        volume: 70,
        playing: Some(0),
        queue,
        ..Deck::default()
    }
}

/// Waits for the pictures asked for, up to the moment given.
fn wait_for_pictures(images: &mut Images, until: Instant) {
    while images.loading() && Instant::now() < until {
        images.poll();
        std::thread::sleep(Duration::from_millis(1));
    }
    images.poll();
}

fn media_player(name: &str, wmp: Arc<Wmp>) -> Option<Bitmap> {
    let deadline = Instant::now() + BUDGET;
    let deck = deck();
    let mut runtime = Runtime::new(
        name.to_owned(),
        wmp,
        deck.clone(),
        HashMap::new(),
        raster::measure(),
        || {},
    );
    let mut now = Instant::now();
    for _ in 0..TICKS {
        if runtime.closed || Instant::now() >= deadline {
            break;
        }
        runtime.tick(deck.clone(), now);
        let key = runtime.views.get(runtime.primary()?)?.state.key;
        // Painting is what asks for the pictures.
        let _ = runtime.paint(key, now);
        let until = deadline.min(Instant::now() + PICTURE_WAIT);
        wait_for_pictures(&mut runtime.images, until);
        now += TICK;
    }
    let key = runtime.views.get(runtime.primary()?)?.state.key;
    let paints = settled_paints(&mut runtime, key, now, deadline);
    let view = runtime.view(key)?;
    let (width, height) = view.state.size();
    let (width, height) = (width.min(MAX_VIEW), height.min(MAX_VIEW));
    if width < 1.0 || height < 1.0 {
        return None;
    }
    let mut canvas = raster::canvas(width.round() as u32, height.round() as u32);
    raster::draw(&paints, &mut canvas);
    Some(canvas)
}

/// The list once the last pictures it asks for are in.
fn settled_paints(runtime: &mut Runtime, key: u32, now: Instant, deadline: Instant) -> Vec<Paint> {
    loop {
        let (paints, _) = runtime.paint(key, now);
        if !runtime.images.loading() || Instant::now() >= deadline {
            return paints;
        }
        wait_for_pictures(&mut runtime.images, deadline);
    }
}

/// Lays the main window out from a skin's sheets.
struct Sheets<'a> {
    skin: &'a Skin,
    canvas: Bitmap,
    clip: [Box2; 1],
}

impl Sheets<'_> {
    fn at(&mut self, sprite: Sprite, x: u32, y: u32) {
        let Some((bitmap, cut)) = self.skin.sprite(sprite) else {
            return;
        };
        let part = Box2 {
            x: cut.x as i32,
            y: cut.y as i32,
            width: cut.width as i32,
            height: cut.height as i32,
        };
        let size = (part.width, part.height);
        let (x, y) = (x as i32, y as i32);
        raster::draw_bitmap(&mut self.canvas, bitmap, part, x, y, size, 255, &self.clip);
    }

    fn put(&mut self, sprite: Sprite, area: Area) {
        self.at(sprite, area.x, area.y);
    }

    fn text(&mut self, text: &str, area: Area) {
        for (index, character) in text.chars().enumerate() {
            let x = area.x + 5 * index as u32;
            if x + 5 > area.x + area.width {
                break;
            }
            self.at(font::glyph(character), x, area.y);
        }
    }

    fn bars(&mut self) {
        let area = layout::VISUALIZER;
        let colors = self.skin.vis_colors;
        let block = |x: u32, y: u32, width: u32, height: u32| Box2 {
            x: (area.x + x) as i32,
            y: (area.y + y) as i32,
            width: width as i32,
            height: height as i32,
        };
        let whole = block(0, 0, area.width, area.height);
        raster::fill(&mut self.canvas, whole, colors[0], 255, &self.clip);
        for y in (0..area.height).step_by(2) {
            for x in (0..area.width).step_by(2) {
                let dot = block(x, y, 1, 1);
                raster::fill(&mut self.canvas, dot, colors[1], 255, &self.clip);
            }
        }
        const HEIGHTS: [u32; 19] = [
            5, 9, 14, 12, 15, 10, 12, 8, 10, 7, 9, 11, 8, 6, 7, 5, 4, 5, 2,
        ];
        for (index, height) in HEIGHTS.iter().enumerate() {
            let x = 4 * index as u32;
            for row in (area.height - height)..area.height {
                let bar = block(x, row, 3, 1);
                let shade = colors[2 + row as usize];
                raster::fill(&mut self.canvas, bar, shade, 255, &self.clip);
            }
        }
    }
}

/// The classic main window, as it stands partway through a song.
fn classic(skin: &Skin) -> Bitmap {
    let (width, height) = (layout::WINDOW_WIDTH, layout::WINDOW_HEIGHT);
    let mut sheets = Sheets {
        skin,
        canvas: raster::canvas(width, height),
        clip: [Box2 {
            x: 0,
            y: 0,
            width: width as i32,
            height: height as i32,
        }],
    };
    let s = &mut sheets;
    s.put(sprites::MAIN_BACKGROUND, Area::new(0, 0, width, height));
    s.put(sprites::TITLE_BAR_ACTIVE, layout::TITLE_BAR);
    s.put(sprites::OPTIONS_BUTTON, layout::OPTIONS_BUTTON);
    s.put(sprites::MINIMIZE_BUTTON, layout::MINIMIZE_BUTTON);
    s.put(sprites::SHADE_BUTTON, layout::SHADE_BUTTON);
    s.put(sprites::CLOSE_BUTTON, layout::CLOSE_BUTTON);
    s.put(sprites::CLUTTER_BAR, layout::CLUTTER_BAR);
    s.put(sprites::STATUS_PLAYING, layout::STATUS);
    s.put(sprites::WORK_INDICATOR_OFF, layout::WORK_INDICATOR);
    s.put(sprites::STEREO_ON, layout::STEREO);
    s.put(sprites::MONO_OFF, layout::MONO);
    let extended = skin.has_extended_digits();
    for (digit, cell) in [0, 1, 2, 3].into_iter().zip(layout::TIME_DIGITS) {
        let sprite = if extended {
            sprites::digit_ex(digit)
        } else {
            sprites::digit(digit)
        };
        s.put(sprite, cell);
    }
    if extended {
        s.put(sprites::NUMS_EX_BLANK, layout::MINUS_EX);
    } else {
        s.put(sprites::NUMBERS_NO_MINUS, layout::MINUS);
    }
    s.bars();
    s.text("SPOTIFAST - SONG ONE", layout::MARQUEE);
    s.text("320", layout::KBPS);
    s.text("44", layout::KHZ);
    let volume = 70;
    s.put(
        sprites::volume_frame((volume * (sprites::SLIDER_FRAMES - 1) + 50) / 100),
        layout::VOLUME,
    );
    s.at(
        sprites::VOLUME_THUMB,
        layout::VOLUME.x + (volume * layout::VOLUME_TRAVEL + 50) / 100,
        layout::VOLUME.y + 1,
    );
    s.put(sprites::balance_frame(0), layout::BALANCE);
    s.at(
        sprites::BALANCE_THUMB,
        layout::BALANCE.x + layout::BALANCE_TRAVEL / 2,
        layout::BALANCE.y + 1,
    );
    s.put(sprites::EQ_OFF, layout::EQ_BUTTON);
    s.put(sprites::PLAYLIST_OFF, layout::PLAYLIST_BUTTON);
    s.put(sprites::POSITION_TRACK, layout::POSITION);
    s.at(
        sprites::POSITION_THUMB,
        layout::POSITION.x + layout::POSITION_TRAVEL * 2 / 5,
        layout::POSITION.y,
    );
    s.put(sprites::PREVIOUS, layout::PREVIOUS);
    s.put(sprites::PLAY_PRESSED, layout::PLAY);
    s.put(sprites::PAUSE, layout::PAUSE);
    s.put(sprites::STOP, layout::STOP);
    s.put(sprites::NEXT, layout::NEXT);
    s.put(sprites::EJECT, layout::EJECT);
    s.put(sprites::SHUFFLE_OFF, layout::SHUFFLE);
    s.put(sprites::REPEAT_OFF, layout::REPEAT);
    let mut canvas = sheets.canvas;
    if let Some(mask) = &skin.regions.normal {
        cut_to(&mut canvas, mask);
    }
    canvas
}

/// Clears what lies outside a skin's window shape.
fn cut_to(canvas: &mut Bitmap, mask: &Mask) {
    let width = canvas.width;
    for (y, row) in canvas.rgba.chunks_exact_mut(4 * width as usize).enumerate() {
        for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            if !mask.contains(x as u32, y as u32) {
                pixel.fill(0);
            }
        }
    }
}

/// `bitmap` no bigger than [`MAX_SIDE`], with its see-through edges kept
/// clean: the colour under a clear pixel must not bleed into its neighbours.
fn shrunk(bitmap: Bitmap) -> Option<Bitmap> {
    let longest = bitmap.width.max(bitmap.height);
    if longest <= MAX_SIDE {
        return Some(bitmap);
    }
    let width = (u64::from(bitmap.width) * u64::from(MAX_SIDE) / u64::from(longest)).max(1) as u32;
    let height =
        (u64::from(bitmap.height) * u64::from(MAX_SIDE) / u64::from(longest)).max(1) as u32;
    let mut rgba = bitmap.rgba;
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * alpha / 255) as u8;
        }
    }
    let image = image::RgbaImage::from_raw(bitmap.width, bitmap.height, rgba)?;
    let small =
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle);
    let mut rgba = small.into_raw();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        if let Some(alpha) = std::num::NonZeroU32::new(u32::from(pixel[3])) {
            for channel in &mut pixel[..3] {
                *channel = (u32::from(*channel) * 255 / alpha).min(255) as u8;
            }
        }
    }
    Some(Bitmap {
        width,
        height,
        rgba,
    })
}

/// The preview of a loaded skin.
pub fn of_skin(skin: &Skin) -> Option<Bitmap> {
    let picture = match &skin.wmp {
        Some(wmp) => media_player(&skin.name, wmp.clone())?,
        None => classic(skin),
    };
    let solid = picture
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] > 0)
        .count();
    if solid * 100 < MIN_SOLID_PERCENT * (picture.rgba.len() / 4) {
        return None;
    }
    shrunk(picture)
}

/// The preview of a skin in a file or folder.
pub fn of_file(path: &Path) -> Option<Bitmap> {
    of_skin(&Skin::load(path).ok()?)
}

/// A preview as the PNG file that is kept of it.
pub fn encode(bitmap: &Bitmap) -> Option<Vec<u8>> {
    let image = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.rgba.clone())?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    Some(bytes.into_inner())
}

#[cfg(test)]
mod tests {
    use super::super::zip;
    use super::*;

    fn png(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb(color));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn a_classic_skin_is_its_main_window() {
        let preview = of_skin(&Skin::builtin()).unwrap();
        assert_eq!(
            (preview.width, preview.height),
            (layout::WINDOW_WIDTH, layout::WINDOW_HEIGHT)
        );
        let pixels = preview.rgba.as_chunks::<4>().0;
        assert!(pixels.iter().all(|pixel| pixel[3] == 255));
        let distinct: std::collections::HashSet<&[u8; 4]> = pixels.iter().collect();
        assert!(distinct.len() > 8, "{}", distinct.len());
    }

    #[test]
    fn a_shaped_classic_skin_stays_see_through_outside_its_shape() {
        let archive = zip::write(&[
            ("main.bmp", &png(275, 116, [200, 10, 10]), false),
            (
                "region.txt",
                b"[Normal]\nNumPoints=4\nPointList=20,0, 275,0, 275,116, 20,116\n",
                false,
            ),
        ]);
        let skin = Skin::from_archive("shaped", &archive).unwrap();
        let preview = of_skin(&skin).unwrap();
        assert_eq!(preview.pixel(5, 50).map(|pixel| pixel[3]), Some(0));
        assert_eq!(preview.pixel(200, 110).map(|pixel| pixel[3]), Some(255));
    }

    #[test]
    fn a_media_player_skin_is_run_and_drawn() {
        // The script moves the panel away: only a running skin looks so.
        let definition = br##"<THEME title="T"><VIEW id="main" backgroundImage="bg.png"
            transparencyColor="#FF00FF" scriptFile="s.js" onLoad="start();">
            <SUBVIEW id="panel" left="0" top="0" width="20" height="20" backgroundColor="#00FF00"/>
            <EFFECTS left="40" top="30" width="30" height="20"/>
            </VIEW></THEME>"##;
        let archive = zip::write(&[
            ("skin.wms", definition, false),
            ("s.js", b"function start(){panel.left=30;}", false),
            ("bg.png", &png(80, 60, [0, 0, 200]), false),
        ]);
        let skin = Skin::from_archive("run", &archive).unwrap();
        let preview = of_skin(&skin).unwrap();
        assert_eq!((preview.width, preview.height), (80, 60));
        assert_eq!(preview.pixel(5, 5), Some([0, 0, 200, 255]));
        assert_eq!(preview.pixel(35, 5), Some([0, 255, 0, 255]));
    }

    #[test]
    fn a_skin_that_closes_itself_has_no_preview_and_a_loop_does_not_hang() {
        let definition = br#"<VIEW id="main" width="40" height="40" scriptFile="s.js"
            onLoad="view.close();"/>"#;
        let archive = zip::write(&[("skin.wms", definition, false), ("s.js", b"", false)]);
        let skin = Skin::from_archive("gone", &archive).unwrap();
        assert!(of_skin(&skin).is_none());

        let definition = br#"<VIEW id="main" width="40" height="40" scriptFile="s.js"
            onLoad="for(;;){}"/>"#;
        let archive = zip::write(&[("skin.wms", definition, false), ("s.js", b"", false)]);
        let skin = Skin::from_archive("loop", &archive).unwrap();
        let started = Instant::now();
        let _ = of_skin(&skin);
        assert!(started.elapsed() <= BUDGET + Duration::from_secs(2));
    }

    #[test]
    fn a_skin_that_stays_hidden_has_no_preview() {
        let definition =
            br#"<VIEW id="main" width="40" height="40" onLoad="view.visible=false;"/>"#;
        let archive = zip::write(&[("skin.wms", definition, false)]);
        let skin = Skin::from_archive("hidden", &archive).unwrap();
        assert!(of_skin(&skin).is_none());
    }

    #[test]
    fn a_big_view_is_stored_small_with_clean_edges() {
        let mut rgba = Vec::new();
        for _ in 0..600 {
            for x in 0..960 {
                // A clear left half whose hidden colour is pure red.
                rgba.extend(if x < 480 {
                    [255, 0, 0, 0]
                } else {
                    [0, 0, 255, 255]
                });
            }
        }
        let small = shrunk(Bitmap {
            width: 960,
            height: 600,
            rgba,
        })
        .unwrap();
        assert_eq!((small.width, small.height), (MAX_SIDE, 300));
        assert!(
            small
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 0 || pixel[0] < 8)
        );
    }

    #[test]
    fn a_preview_survives_its_file() {
        let bitmap = Bitmap {
            width: 2,
            height: 1,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 255],
        };
        assert_eq!(Bitmap::decode(&encode(&bitmap).unwrap()), Some(bitmap));
    }

    /// Draws every skin in `$SPOTIFAST_PREVIEW_SAMPLES` into
    /// `$SPOTIFAST_PREVIEW_OUTPUT`, and says what each cost.
    #[test]
    fn sample_previews() {
        let (Ok(folder), Ok(output)) = (
            std::env::var("SPOTIFAST_PREVIEW_SAMPLES"),
            std::env::var("SPOTIFAST_PREVIEW_OUTPUT"),
        ) else {
            return;
        };
        std::fs::create_dir_all(&output).unwrap();
        let mut skins = crate::winamp::list_skins(Path::new(&folder));
        skins.sort_by(|a, b| a.name.cmp(&b.name));
        for choice in skins {
            let started = Instant::now();
            let preview = of_file(&choice.path);
            let spent = started.elapsed().as_millis();
            let size = preview.as_ref().map(|p| (p.width, p.height));
            if let Some(bytes) = preview.as_ref().and_then(encode) {
                std::fs::write(
                    Path::new(&output).join(format!("{}.png", choice.name)),
                    bytes,
                )
                .unwrap();
            }
            println!("PREVIEW\t{}\t{spent}\t{size:?}", choice.name);
        }
    }
}
