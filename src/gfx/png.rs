//! PNG payloads for the pixel protocols: kitty needs PNG (`f=100`), and
//! iTerm2 gets a PNG when the original file is too big to send as is.

use std::borrow::Cow;

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};

use super::{Rgba, resize, size};
use crate::panic::guarded;

/// The 8-byte signature every PNG file starts with.
const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Whether `bytes` look like a PNG file (so kitty can take them as they
/// are).
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(SIGNATURE)
}

/// Encode an image as PNG: RGB when every pixel is opaque (smaller), RGBA
/// otherwise. `None` for an invalid or empty image.
pub fn encode(img: &Rgba) -> Option<Vec<u8>> {
    if img.is_empty() {
        return None;
    }
    guarded(|| {
        let mut out = Vec::new();
        let encoder =
            PngEncoder::new_with_quality(&mut out, CompressionType::Default, FilterType::Adaptive);
        let result = if img.has_alpha() {
            encoder.write_image(&img.pixels, img.width, img.height, ExtendedColorType::Rgba8)
        } else {
            let (pixels, _) = img.pixels.as_chunks::<4>();
            let rgb: Vec<u8> = pixels.iter().flat_map(|&[r, g, b, _]| [r, g, b]).collect();
            encoder.write_image(&rgb, img.width, img.height, ExtendedColorType::Rgb8)
        };
        result.ok().map(|()| out)
    })
    .flatten()
}

/// `img` downscaled (in linear light, keeping its aspect ratio) to fit in
/// `max_width × max_height` pixels; borrowed unchanged when it already fits.
/// Use it to keep payloads small: there is no point in sending more pixels
/// than the cell box shows.
pub fn fit(img: &Rgba, max_width: u32, max_height: u32) -> Cow<'_, Rgba> {
    let (w, h) = size::fit_within((img.width, img.height), (max_width, max_height));
    if (w, h) == (img.width, img.height) {
        Cow::Borrowed(img)
    } else {
        Cow::Owned(resize(img, w, h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::decode;

    fn image(width: u32, height: u32, pixels: &[[u8; 4]]) -> Rgba {
        Rgba::new(width, height, pixels.concat()).unwrap()
    }

    #[test]
    fn round_trips_through_the_decoder() {
        let img = image(
            2,
            2,
            &[
                [255, 0, 0, 255],
                [0, 255, 0, 128],
                [0, 0, 255, 0],
                [9, 9, 9, 255],
            ],
        );
        let png = encode(&img).unwrap();
        assert!(is_png(&png));
        assert_eq!(decode::decode(&png, 100).unwrap(), img);
    }

    #[test]
    fn opaque_images_are_stored_as_rgb() {
        let img = Rgba::filled(64, 64, [10, 20, 30, 255]).unwrap();
        let png = encode(&img).unwrap();
        // IHDR colour type 2 (truecolour) instead of 6 (truecolour + alpha).
        assert_eq!(png[25], 2);
        assert_eq!(decode::decode(&png, 10_000).unwrap(), img);
        let with_alpha = Rgba::filled(4, 4, [10, 20, 30, 7]).unwrap();
        assert_eq!(encode(&with_alpha).unwrap()[25], 6);
    }

    #[test]
    fn empty_and_invalid_images() {
        assert_eq!(encode(&Rgba::default()), None);
        let invalid = Rgba {
            width: 9,
            height: 9,
            pixels: vec![0; 4],
        };
        assert_eq!(encode(&invalid), None);
        assert!(!is_png(b"GIF89a"));
    }

    #[test]
    fn fit_downscales_only_when_needed() {
        let img = Rgba::filled(400, 100, [255, 255, 255, 255]).unwrap();
        assert!(matches!(fit(&img, 800, 800), Cow::Borrowed(_)));
        let small = fit(&img, 100, 100);
        assert_eq!((small.width, small.height), (100, 25));
        let (px, _) = small.pixels.as_chunks::<4>();
        assert!(px.iter().all(|p| *p == [255; 4]));
    }
}
