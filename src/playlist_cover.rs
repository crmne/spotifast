//! Bounded preparation of user-selected playlist artwork, off the UI thread.

use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageDecoder, ImageFormat, ImageReader};

/// Spotify limits the complete Base64 request body to 256 KB.
pub const MAX_PAYLOAD: usize = 256_000;
const MAX_FILE: usize = 20 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Cover {
    pub jpeg: Arc<[u8]>,
    pub encoded: Arc<str>,
    pub uri: String,
}

#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub selection: Option<Cover>,
    pub request: Option<u64>,
    pub uploading: Option<u64>,
    pub error: Option<String>,
}

/// Artwork held until the returned image is confirmed as this upload.
#[derive(Clone, Debug)]
pub struct PendingCover {
    pub cover: Cover,
    pub previous_urls: Vec<String>,
    pub request: u64,
    pub checking: Option<Vec<crate::api::models::Image>>,
    pub next_images: Option<Vec<crate::api::models::Image>>,
    pub rechecks_left: u8,
}

impl Cover {
    /// Compare the returned artwork, not its URL. Metadata changes are fine;
    /// another upload's pixels must never be mistaken for this image.
    pub fn matches_remote(&self, bytes: &[u8]) -> Result<bool, String> {
        if bytes == self.jpeg.as_ref() {
            return Ok(true);
        }
        let remote = bounded_decode(bytes)?;
        let selected = bounded_decode(&self.jpeg)?;
        if (remote.width(), remote.height()) != (selected.width(), selected.height()) {
            return Ok(false);
        }
        Ok(remote.into_rgba8() == selected.into_rgba8())
    }
}

pub fn read(path: &Path) -> Result<Cover, String> {
    let file = std::fs::File::open(path)
        .map_err(|_| "Couldn't open that image. Check that it is still available.".to_string())?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err("Choose a JPEG or PNG file.".into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_FILE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Couldn't read that image. Try another file.".to_string())?;
    prepare(&bytes)
}

pub fn prepare(bytes: &[u8]) -> Result<Cover, String> {
    let image = bounded_decode(bytes)?;
    // Preserve the entire image and its aspect ratio. Flatten transparency
    // onto white because JPEG has no alpha channel.
    let mut pixels = image.thumbnail(640, 640).into_rgba8();
    for pixel in pixels.pixels_mut() {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel.0[..3] {
            *channel = ((u32::from(*channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        }
        pixel[3] = 255;
    }
    let original = image::DynamicImage::ImageRgba8(pixels);
    for size in [640, 480, 320] {
        let rgb = original.thumbnail(size, size).into_rgb8();
        for quality in [90, 80, 70, 60] {
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality)
                .encode_image(&rgb)
                .map_err(|_| "Couldn't encode this image. Try another file.")?;
            let encoded = STANDARD.encode(&jpeg);
            if encoded.len() <= MAX_PAYLOAD {
                use sha2::{Digest, Sha256};
                let uri = format!("bytes://playlist-cover-{:x}.jpg", Sha256::digest(&jpeg));
                return Ok(Cover {
                    jpeg: jpeg.into(),
                    encoded: encoded.into(),
                    uri,
                });
            }
        }
    }
    Err("This image is too detailed for Spotify's upload limit. Choose a smaller image.".into())
}

fn bounded_decode(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    if bytes.len() > MAX_FILE {
        return Err("Choose an image smaller than 20 MB.".into());
    }
    let format = image::guess_format(bytes).map_err(|_| "Choose a JPEG or PNG image.")?;
    if !matches!(format, ImageFormat::Jpeg | ImageFormat::Png) {
        return Err("Choose a JPEG or PNG image.".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    decode(bytes, format, limits)
}

fn decode(
    bytes: &[u8],
    format: ImageFormat,
    mut limits: image::Limits,
) -> Result<image::DynamicImage, String> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(|_| {
        "Couldn't decode this image. Choose a valid JPEG or PNG no larger than 8192 pixels per side."
    })?;
    // Using from_decoder for EXIF orientation bypasses ImageReader::decode's
    // output-buffer reservation. Count those pixels before allocating them.
    limits
        .reserve(decoder.total_bytes())
        .and_then(|()| decoder.set_limits(limits))
        .map_err(|_| {
            "This image needs too much memory. Choose a smaller JPEG or PNG.".to_string()
        })?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = image::DynamicImage::from_decoder(decoder)
        .map_err(|_| "Couldn't decode this image. Try another JPEG or PNG.")?;
    image.apply_orientation(orientation);
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_remote_art_requires_the_uploaded_pixels() {
        let encode = |color| {
            let pixels = image::RgbImage::from_pixel(40, 24, image::Rgb(color));
            let mut bytes = Cursor::new(Vec::new());
            pixels.write_to(&mut bytes, ImageFormat::Png).unwrap();
            prepare(bytes.get_ref()).unwrap()
        };
        let earlier = encode([200, 20, 40]);
        let current = encode([20, 40, 200]);
        assert!(current.matches_remote(&current.jpeg).unwrap());
        assert!(!current.matches_remote(&earlier.jpeg).unwrap());
        // Container/metadata changes do not change the actual cover.
        let pixels = image::load_from_memory(&current.jpeg).unwrap();
        let mut png = Cursor::new(Vec::new());
        pixels.write_to(&mut png, ImageFormat::Png).unwrap();
        assert!(current.matches_remote(png.get_ref()).unwrap());
        let mut transparent = pixels.into_rgba8();
        for pixel in transparent.pixels_mut() {
            pixel[3] = 0;
        }
        let mut png = Cursor::new(Vec::new());
        transparent.write_to(&mut png, ImageFormat::Png).unwrap();
        assert!(!current.matches_remote(png.get_ref()).unwrap());
        assert!(current.matches_remote(b"not an image").is_err());
    }

    #[test]
    fn decoded_pixels_must_fit_the_memory_budget() {
        let pixels = image::RgbImage::from_pixel(100, 100, image::Rgb([30, 50, 70]));
        let mut jpeg = Cursor::new(Vec::new());
        pixels.write_to(&mut jpeg, ImageFormat::Jpeg).unwrap();
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(15_000);
        assert!(decode(jpeg.get_ref(), ImageFormat::Jpeg, limits).is_err());
    }

    #[test]
    fn rejects_invalid_unsupported_and_oversized_files() {
        assert!(prepare(b"not an image").is_err());
        assert!(prepare(b"GIF89a").is_err());
        assert!(prepare(&vec![0; MAX_FILE + 1]).is_err());
    }

    #[test]
    fn jpeg_input_and_missing_file_are_handled() {
        let image = image::RgbImage::from_pixel(24, 24, image::Rgb([30, 50, 70]));
        let mut jpeg = Cursor::new(Vec::new());
        image.write_to(&mut jpeg, ImageFormat::Jpeg).unwrap();
        assert!(prepare(jpeg.get_ref()).is_ok());
        let missing = std::env::temp_dir().join(format!(
            "spotizgeg-missing-cover-{}.jpg",
            std::process::id()
        ));
        assert!(read(&missing).unwrap_err().contains("Couldn't open"));
    }

    #[test]
    fn png_is_flattened_and_encoded_as_a_bounded_jpeg() {
        let image = image::RgbaImage::from_pixel(1200, 600, image::Rgba([0, 0, 0, 0]));
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, ImageFormat::Png).unwrap();
        let cover = prepare(png.get_ref()).unwrap();
        assert!(cover.encoded.len() <= MAX_PAYLOAD);
        assert_eq!(
            STANDARD.decode(cover.encoded.as_bytes()).unwrap(),
            &*cover.jpeg
        );
        let decoded = image::load_from_memory(&cover.jpeg).unwrap().into_rgb8();
        assert_eq!(decoded.dimensions(), (640, 320));
        assert!(
            decoded
                .pixels()
                .all(|pixel| pixel.0.iter().all(|value| *value >= 250))
        );
    }

    #[test]
    fn detailed_image_still_fits_the_encoded_payload_limit() {
        let mut state = 17_u32;
        let image = image::RgbImage::from_fn(800, 800, |_, _| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            image::Rgb([state as u8, (state >> 8) as u8, (state >> 16) as u8])
        });
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, ImageFormat::Png).unwrap();
        let cover = prepare(png.get_ref()).unwrap();
        assert!(cover.encoded.len() <= MAX_PAYLOAD);
    }
}
