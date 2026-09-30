//! Image decoding: header-only dimensions and full decodes to [`Rgba`].
//!
//! Supported formats are PNG (including APNG), JPEG, GIF and WebP (the
//! `image` crate's decoders; animated images give their first frame).
//! Decoding is total: corrupt, truncated or hostile input gives an error,
//! never a panic. The pixel limit is checked against the header before any
//! pixel buffer is allocated, and a panic inside a decoder is caught with
//! [`crate::panic::guarded`]. EXIF orientation is not applied, so a decoded
//! image always has the size [`dimensions`] reported for the layout.

use std::io::Cursor;

use image::{DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits};

use super::Rgba;
use crate::panic::guarded;

/// An image file format emde can decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    WebP,
}

impl Format {
    fn image_format(self) -> ImageFormat {
        match self {
            Format::Png => ImageFormat::Png,
            Format::Jpeg => ImageFormat::Jpeg,
            Format::Gif => ImageFormat::Gif,
            Format::WebP => ImageFormat::WebP,
        }
    }
}

/// Why an image could not be decoded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The bytes are not PNG, JPEG, GIF or WebP.
    #[error("not a PNG, JPEG, GIF or WebP image")]
    UnknownFormat,
    /// The header announces more pixels than allowed.
    #[error("image is {width}×{height} pixels, more than the {max_pixels}-pixel limit")]
    TooLarge {
        width: u32,
        height: u32,
        max_pixels: u64,
    },
    /// The header announces no pixels at all.
    #[error("image has no pixels")]
    Empty,
    /// The data is corrupt or truncated.
    #[error("cannot decode image: {0}")]
    Corrupt(String),
    /// The decoder panicked (a bug in it); the message was recorded by the
    /// panic hook.
    #[error("image decoder failed")]
    Panicked,
}

/// The format of an image file, from its first bytes.
pub fn format(bytes: &[u8]) -> Option<Format> {
    match image::guess_format(bytes).ok()? {
        ImageFormat::Png => Some(Format::Png),
        ImageFormat::Jpeg => Some(Format::Jpeg),
        ImageFormat::Gif => Some(Format::Gif),
        ImageFormat::WebP => Some(Format::WebP),
        _ => None,
    }
}

/// PNG limits each side to 2³¹ − 1 pixels.
const PNG_MAX_SIDE: u32 = i32::MAX as u32;

/// Width and height in pixels, read from the header only. `None` for
/// unknown formats, truncated or implausible headers and zero-sized images.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let format = format(bytes)?;
    // The header parser reads PNG sizes at fixed offsets: make sure they
    // really are the IHDR chunk's.
    if format == Format::Png && bytes.get(12..16) != Some(b"IHDR".as_slice()) {
        return None;
    }
    let size = guarded(|| imagesize::blob_size(bytes).ok())??;
    let width = u32::try_from(size.width).ok()?;
    let height = u32::try_from(size.height).ok()?;
    let plausible = format != Format::Png || (width <= PNG_MAX_SIDE && height <= PNG_MAX_SIDE);
    (width > 0 && height > 0 && plausible).then_some((width, height))
}

/// Decode an image to 8-bit RGBA, refusing images with more than
/// `max_pixels` pixels before decoding them.
pub fn decode(bytes: &[u8], max_pixels: u64) -> Result<Rgba, DecodeError> {
    guarded(|| decode_unguarded(bytes, max_pixels)).unwrap_or(Err(DecodeError::Panicked))
}

fn decode_unguarded(bytes: &[u8], max_pixels: u64) -> Result<Rgba, DecodeError> {
    let format = format(bytes).ok_or(DecodeError::UnknownFormat)?;
    // The cheap header parser first, so a huge image never reaches a
    // decoder; then the decoder's own view of the header, which is the one
    // that counts.
    if let Some(size) = dimensions(bytes) {
        check_size(size, max_pixels)?;
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format.image_format());
    reader.limits(limits(max_pixels));
    let decoder = reader.into_decoder().map_err(error)?;
    let (width, height) = decoder.dimensions();
    check_size((width, height), max_pixels)?;
    let mut budget = limits(max_pixels);
    budget.reserve(decoder.total_bytes()).map_err(error)?;
    let image = DynamicImage::from_decoder(decoder).map_err(error)?;
    let rgba = image.into_rgba8();
    Rgba::new(width, height, rgba.into_raw())
        .ok_or_else(|| DecodeError::Corrupt("decoder returned a short buffer".into()))
}

fn check_size((width, height): (u32, u32), max_pixels: u64) -> Result<(), DecodeError> {
    if width == 0 || height == 0 {
        return Err(DecodeError::Empty);
    }
    if u64::from(width) * u64::from(height) > max_pixels {
        return Err(DecodeError::TooLarge {
            width,
            height,
            max_pixels,
        });
    }
    Ok(())
}

/// Decoder limits for images of at most `max_pixels` pixels: room for
/// 16-bit RGBA output plus working buffers.
fn limits(max_pixels: u64) -> Limits {
    let mut limits = Limits::default();
    limits.max_alloc = Some(max_pixels.saturating_mul(8).saturating_add(64 << 20));
    limits
}

fn error(e: ImageError) -> DecodeError {
    match e {
        ImageError::Unsupported(_) => DecodeError::UnknownFormat,
        other => DecodeError::Corrupt(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{ExtendedColorType, Frame, ImageEncoder, RgbaImage};

    use super::*;

    /// A 3×2 test pattern: red, green, blue over white, black, half-clear grey.
    const PATTERN: [[u8; 4]; 6] = [
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
        [255, 255, 255, 255],
        [0, 0, 0, 255],
        [128, 128, 128, 128],
    ];

    fn pattern() -> Vec<u8> {
        PATTERN.concat()
    }

    fn png() -> Vec<u8> {
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(&pattern(), 3, 2, ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    fn jpeg() -> Vec<u8> {
        let rgb: Vec<u8> = PATTERN.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let mut out = Vec::new();
        JpegEncoder::new_with_quality(&mut out, 100)
            .write_image(&rgb, 3, 2, ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    fn webp() -> Vec<u8> {
        let mut out = Vec::new();
        WebPEncoder::new_lossless(&mut out)
            .write_image(&pattern(), 3, 2, ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    /// A two-frame GIF: the pattern, then solid blue.
    fn gif() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut out);
            enc.set_repeat(Repeat::Infinite).unwrap();
            let first = RgbaImage::from_raw(3, 2, pattern()).unwrap();
            let second = RgbaImage::from_pixel(3, 2, image::Rgba([0, 0, 255, 255]));
            enc.encode_frames([Frame::new(first), Frame::new(second)])
                .unwrap();
        }
        out
    }

    #[test]
    fn formats_are_sniffed() {
        assert_eq!(format(&png()), Some(Format::Png));
        assert_eq!(format(&jpeg()), Some(Format::Jpeg));
        assert_eq!(format(&gif()), Some(Format::Gif));
        assert_eq!(format(&webp()), Some(Format::WebP));
        assert_eq!(format(b"BM\0\0\0\0"), None, "BMP is not enabled");
        assert_eq!(format(b""), None);
    }

    #[test]
    fn header_dimensions() {
        for bytes in [png(), jpeg(), gif(), webp()] {
            assert_eq!(dimensions(&bytes), Some((3, 2)));
        }
        assert_eq!(dimensions(b"not an image at all"), None);
        assert_eq!(dimensions(&png()[..20]), None, "IHDR cut short");
        // A PNG magic followed by junk has no IHDR to read sizes from.
        let mut junk = b"\x89PNG\r\n\x1a\n".to_vec();
        junk.extend([0xA5; 40]);
        assert_eq!(dimensions(&junk), None);
        // PNG sizes are limited to 2³¹ − 1.
        let mut wide = png();
        wide[16..20].copy_from_slice(&0x8000_0000u32.to_be_bytes());
        assert_eq!(dimensions(&wide), None);
    }

    #[test]
    fn lossless_formats_round_trip() {
        for (name, bytes) in [("png", png()), ("webp", webp())] {
            let img = decode(&bytes, 100).unwrap();
            assert_eq!((img.width, img.height), (3, 2), "{name}");
            assert_eq!(img.pixels, pattern(), "{name}");
        }
    }

    #[test]
    fn jpeg_decodes_close_to_the_source() {
        let img = decode(&jpeg(), 100).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert!(!img.has_alpha());
        // Chroma subsampling blurs a 3×2 pattern; only check it decoded to
        // plausible, opaque pixels with the right brightness ordering.
        let luma = |x, y| {
            let [r, g, b, _] = img.pixel(x, y);
            u32::from(r) + u32::from(g) + u32::from(b)
        };
        assert!(luma(0, 1) > luma(1, 1), "white is brighter than black");
    }

    #[test]
    fn gif_gives_the_first_frame() {
        let img = decode(&gif(), 100).unwrap();
        assert_eq!((img.width, img.height), (3, 2));
        assert_eq!(img.pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(img.pixel(1, 1), [0, 0, 0, 255]);
    }

    #[test]
    fn pixel_limit_is_checked_first() {
        assert_eq!(
            decode(&png(), 5),
            Err(DecodeError::TooLarge {
                width: 3,
                height: 2,
                max_pixels: 5
            })
        );
        assert!(decode(&png(), 6).is_ok());
    }

    /// CRC-32 (IEEE), to forge valid PNG chunks.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    crc >> 1 ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn huge_headers_fail_before_allocating() {
        // A valid IHDR claiming 100000 × 100000 pixels (10¹⁰ × 4 bytes).
        let mut huge = png();
        huge[16..24].copy_from_slice(&[0, 1, 0x86, 0xA0, 0, 1, 0x86, 0xA0]);
        let crc = crc32(&huge[12..29]);
        huge[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(dimensions(&huge), Some((100_000, 100_000)));
        assert_eq!(
            decode(&huge, 40_000_000),
            Err(DecodeError::TooLarge {
                width: 100_000,
                height: 100_000,
                max_pixels: 40_000_000
            })
        );
    }

    #[test]
    fn truncated_and_garbage_input() {
        for bytes in [png(), jpeg(), gif(), webp()] {
            // Cut inside the header: always an error.
            for cut in [0, 1, 8, 16] {
                assert!(decode(&bytes[..cut], 100).is_err(), "cut {cut}");
            }
            // Cut later: an error or a partial image, never a panic.
            for cut in [30, bytes.len() / 2, bytes.len() - 1] {
                let _ = decode(&bytes[..cut.min(bytes.len())], 100);
            }
        }
        assert_eq!(decode(b"", 100), Err(DecodeError::UnknownFormat));
        assert_eq!(decode(b"hello world", 100), Err(DecodeError::UnknownFormat));
        // Right magic, garbage after it.
        let mut fake = b"\x89PNG\r\n\x1a\n".to_vec();
        fake.extend(std::iter::repeat_n(0xA5, 200));
        assert!(matches!(decode(&fake, 100), Err(DecodeError::Corrupt(_))));
        let mut fake_jpeg = vec![0xFF, 0xD8, 0xFF];
        fake_jpeg.extend((0..=255u8).cycle().take(500));
        assert!(decode(&fake_jpeg, 100).is_err());
    }

    /// The plan's budget (§10): a 1 MP JPEG decoded and drawn as 100×40
    /// half blocks in at most 15 ms (release build, on the image worker).
    #[test]
    #[ignore = "timing; run with --release -- --ignored --nocapture"]
    #[allow(clippy::print_stderr)]
    fn timing_one_megapixel_jpeg_to_half_blocks() {
        use crate::gfx::raster::rasterize;
        use crate::style::Rgb;
        use crate::term::{BlockGlyphSet, ColorDepth};

        // A smooth photo-like pattern, so the JPEG has typical entropy.
        let (w, h) = (1000u32, 1000u32);
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let wave = (x as f32 / 37.0).sin() * 60.0 + (y as f32 / 23.0).cos() * 60.0;
                rgb.extend_from_slice(&[(wave + 128.0) as u8, (x / 4) as u8, (y / 4) as u8]);
            }
        }
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 85)
            .write_image(&rgb, w, h, ExtendedColorType::Rgb8)
            .unwrap();
        let run = || {
            let img = decode(&jpeg, 40_000_000).unwrap();
            let bg = Some(Rgb(30, 30, 46));
            rasterize(
                &img,
                100,
                40,
                BlockGlyphSet::Half,
                bg,
                ColorDepth::TrueColor,
            )
        };
        let _ = run(); // warm the tables
        let n = 10;
        let start = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(run());
        }
        let per = start.elapsed() / n;
        eprintln!(
            "1 MP JPEG ({} bytes) → 100×40 half blocks: {per:?}",
            jpeg.len()
        );
        assert!(per.as_millis() <= 15, "{per:?} per image");
    }

    #[test]
    fn corrupted_bytes_never_panic() {
        // Flip bytes all over valid files; every result must be Ok or Err.
        for bytes in [png(), jpeg(), gif(), webp()] {
            for i in 0..bytes.len() {
                for flip in [0x01, 0x80, 0xFF] {
                    let mut b = bytes.clone();
                    b[i] ^= flip;
                    let _ = decode(&b, 1_000_000);
                    let _ = dimensions(&b);
                }
            }
        }
    }
}
