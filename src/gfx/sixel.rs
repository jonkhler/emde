//! DEC sixel output via `icy_sixel` (Wu quantiser), for terminals without
//! kitty or iTerm2 graphics, and for tmux's own sixel support.
//!
//! Sixel draws pixels 1:1 at the cursor, so the image must already be
//! scaled to its cell box and composited onto the background (tmux 3.4
//! drops transparency): [`super::resize_onto`] does both, blending each
//! pixel before it averages. Crop that image with [`crop_rows`] and
//! [`Rgba::crop_rows`] for partly visible figures. The palette has 128
//! colours with light dithering; output that exceeds the byte cap (tmux
//! 3.4 discards strings over 1 MiB) is retried with 64 colours, and if it
//! still does not fit the image is shown as blocks.

use std::ops::Range;

use icy_sixel::{BackgroundMode, EncodeOptions, PixelAspectRatio, QuantizeMethod, SixelImage};

use super::Rgba;
use crate::panic::guarded;
use crate::style::Rgb;

/// The largest sixel sequence tmux 3.4 accepts.
pub const TMUX_MAX_BYTES: usize = 1 << 20;

/// The most sixel images to keep on screen inside tmux (plan §7). tmux 3.4
/// keeps only 10 sixel images for the whole server and silently frees the
/// oldest; staying at 6 leaves room for other panes. Show further images as
/// blocks.
pub const TMUX_MAX_VISIBLE: usize = 6;

/// Palette sizes tried in turn until the output fits.
const PALETTES: [u16; 2] = [128, 64];

/// Floyd–Steinberg strength: enough to hide banding in photos without
/// bloating the output with noise.
const DIFFUSION: f32 = 0.5;

/// Height of one sixel band in pixels.
pub const BAND: u32 = 6;

/// Encode `img` as a sixel sequence (`ESC P … ESC \`). Pixels that are not
/// opaque are composited onto `background` first (for an image from
/// [`super::resize_onto`] there are none). `None` if it cannot be encoded
/// within `max_bytes` even with the smaller palette, or the image is empty
/// or invalid.
pub fn encode(img: &Rgba, background: Rgb, max_bytes: usize) -> Option<Vec<u8>> {
    if img.is_empty() {
        return None;
    }
    let flat = img.flatten(background);
    PALETTES
        .iter()
        .filter_map(|&colors| encode_with(&flat, colors))
        .find(|out| out.len() <= max_bytes)
}

fn encode_with(img: &Rgba, max_colors: u16) -> Option<Vec<u8>> {
    let opts = EncodeOptions {
        max_colors,
        diffusion: DIFFUSION,
        quantize_method: QuantizeMethod::Wu,
    };
    guarded(|| {
        SixelImage::try_from_rgba(img.pixels.clone(), img.width as usize, img.height as usize)
            .ok()?
            .with_aspect_ratio(PixelAspectRatio::Square)
            .with_background_mode(BackgroundMode::Opaque)
            .encode_with(&opts)
            .ok()
    })
    .flatten()
    .map(String::into_bytes)
}

/// The pixel rows to send when only cell rows `visible` of an image
/// `image_height` pixels tall (drawn at `cell_height` pixels per row) are
/// on screen. The crop starts at the first visible cell row; its height is
/// cut to whole sixel bands when a partial last band would reach below the
/// last visible row (into the status row, or scrolling the screen). `None`
/// if nothing is left.
pub fn crop_rows(image_height: u32, cell_height: u16, visible: Range<u16>) -> Option<Range<u32>> {
    let cell = u32::from(cell_height);
    let top = u32::from(visible.start).saturating_mul(cell);
    let bottom = u32::from(visible.end).saturating_mul(cell);
    if top >= image_height.min(bottom) {
        return None;
    }
    let room = bottom - top;
    let mut height = image_height.min(bottom) - top;
    if height.div_ceil(BAND) * BAND > room {
        height = room / BAND * BAND;
    }
    (height > 0).then(|| top..top + height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(width: u32, height: u32) -> Rgba {
        let mut px = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                px.extend_from_slice(&[
                    (x * 255 / width.max(1)) as u8,
                    (y * 255 / height.max(1)) as u8,
                    ((x + y) * 3) as u8,
                    255,
                ]);
            }
        }
        Rgba::new(width, height, px).unwrap()
    }

    /// The raster attributes `"Pan;Pad;W;H` of a sixel sequence.
    fn raster_size(sixel: &[u8]) -> (usize, usize) {
        let text = std::str::from_utf8(sixel).unwrap();
        let attrs = text.split('"').nth(1).unwrap();
        let fields: Vec<usize> = attrs
            .split(|c: char| !c.is_ascii_digit())
            .take(4)
            .map(|f| f.parse().unwrap())
            .collect();
        (fields[2], fields[3])
    }

    #[test]
    fn encodes_an_opaque_sequence() {
        let out = encode(&gradient(40, 30), Rgb(30, 30, 46), TMUX_MAX_BYTES).unwrap();
        // DCS with square pixels (P1 = 9) and an opaque background (P2 = 0).
        assert!(out.starts_with(b"\x1bP9;0;0q\""), "{:?}", &out[..12]);
        assert!(out.ends_with(b"\x1b\\"));
        assert_eq!(raster_size(&out), (40, 30));
        assert!(out.len() <= TMUX_MAX_BYTES);
    }

    #[test]
    fn transparency_is_composited_away() {
        let clear = Rgba::filled(12, 12, [0, 0, 0, 0]).unwrap();
        let out = encode(&clear, Rgb(255, 0, 0), TMUX_MAX_BYTES).unwrap();
        let text = String::from_utf8(out).unwrap();
        // One palette entry: pure red, in percent.
        assert!(text.contains("#0;2;100;0;0"), "{text}");
        assert!(!text.contains("#1;"), "{text}");
    }

    #[test]
    fn stays_under_the_cap_or_gives_up() {
        let img = gradient(300, 200);
        let full = encode(&img, Rgb(0, 0, 0), usize::MAX).unwrap();
        // A cap just below the 128-colour size forces the 64-colour retry.
        let smaller = encode(&img, Rgb(0, 0, 0), full.len() - 1).unwrap();
        assert!(smaller.len() < full.len());
        assert!(!String::from_utf8(smaller.clone()).unwrap().contains("#64;"));
        assert_eq!(raster_size(&smaller), (300, 200));
        // Nothing fits in a handful of bytes.
        assert_eq!(encode(&img, Rgb(0, 0, 0), 64), None);
        for cap in [TMUX_MAX_BYTES, 200_000, 50_000] {
            if let Some(out) = encode(&img, Rgb(0, 0, 0), cap) {
                assert!(out.len() <= cap, "{} > {cap}", out.len());
            }
        }
    }

    #[test]
    fn empty_and_invalid_images() {
        assert_eq!(encode(&Rgba::default(), Rgb(0, 0, 0), TMUX_MAX_BYTES), None);
        let invalid = Rgba {
            width: 4,
            height: 4,
            pixels: vec![1; 3],
        };
        assert_eq!(encode(&invalid, Rgb(0, 0, 0), TMUX_MAX_BYTES), None);
    }

    #[test]
    fn crops_end_on_whole_bands() {
        // 20-px cells: rows 2..5 are pixels 40..100, exactly 10 bands.
        assert_eq!(crop_rows(200, 20, 2..5), Some(40..100));
        // 16-px cells: 3 rows are 48 px, 8 bands.
        assert_eq!(crop_rows(160, 16, 0..3), Some(0..48));
        // 17-px cells: 2 rows are 34 px; a sixth band would reach 36 px.
        assert_eq!(crop_rows(170, 17, 1..3), Some(17..47));
        // The image ends first: its last partial band still fits the rows.
        assert_eq!(crop_rows(50, 17, 0..4), Some(0..50));
        // It ends in the last row and the padded band would spill below.
        assert_eq!(crop_rows(33, 17, 0..2), Some(0..30));
        assert_eq!(crop_rows(100, 20, 5..9), None, "below the image");
        assert_eq!(crop_rows(100, 20, 3..3), None);
        assert_eq!(crop_rows(100, 4, 0..1), None, "less than one band");
        assert_eq!(crop_rows(100, 0, 0..4), None);
    }

    #[test]
    fn composite_scale_crop_encode() {
        // The pipeline a pager runs: composite and scale once (cached),
        // then crop and encode the visible rows.
        let mut img = gradient(64, 128);
        for a in img.pixels.iter_mut().skip(3).step_by(4) {
            *a = 100;
        }
        let scaled = crate::gfx::resize_onto(&img, 32, 64, Rgb(0, 0, 0));
        assert!(!scaled.has_alpha());
        let rows = crop_rows(scaled.height, 16, 1..3).unwrap();
        let out = encode(&scaled.crop_rows(rows), Rgb(0, 0, 0), TMUX_MAX_BYTES).unwrap();
        assert_eq!(raster_size(&out), (32, 30));
    }

    #[test]
    fn crop_and_encode_a_slice() {
        let img = gradient(32, 64);
        let rows = crop_rows(img.height, 16, 1..3).unwrap();
        let slice = img.crop_rows(rows);
        assert_eq!((slice.width, slice.height), (32, 30));
        let out = encode(&slice, Rgb(0, 0, 0), TMUX_MAX_BYTES).unwrap();
        assert_eq!(raster_size(&out), (32, 30));
    }
}
