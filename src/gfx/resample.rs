//! Area-averaging (box filter) resampling in linear light.
//!
//! Averaging must add up *light*, not sRGB code values: a fine black-and-white
//! pattern has to become 50 % grey (sRGB 188), not sRGB 128. Pixels are
//! therefore converted to linear RGB with premultiplied alpha, so that
//! transparent pixels contribute no colour, and each output pixel averages
//! exactly the source area it covers.
//!
//! Sources much larger than the target are first reduced by an integer
//! factor per axis with plain block averages (still in linear light),
//! keeping at least [`KEEP`] blocks per output pixel and axis. The exact
//! fractional pass then weighs each block by the source area it covers, so
//! the result stays within a fraction of a sub-pixel of the direct average
//! while touching far fewer pixels.

use std::sync::OnceLock;

use super::Rgba;

/// Linear-light RGBA with premultiplied alpha: `[r·a, g·a, b·a, a]`, each in
/// `0.0..=1.0`.
pub(crate) type Premul = [f32; 4];

/// Minimum blocks per output pixel and axis that the integer pre-shrink
/// leaves for the exact fractional pass.
const KEEP: usize = 4;

/// Resolution of the coarse index into the linear → sRGB thresholds.
const COARSE: usize = 4096;

/// Conversion tables between sRGB bytes and linear light.
struct Tables {
    /// sRGB byte → linear light.
    to_linear: [f32; 256],
    /// `thresholds[i]`: the linear value from which sRGB rounds to `i + 1`.
    thresholds: [f32; 255],
    /// `coarse[k]`: the sRGB byte of linear `k / COARSE`.
    coarse: Vec<u8>,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let decode = |s: f64| {
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        let to_linear = std::array::from_fn(|i| decode(i as f64 / 255.0) as f32);
        let thresholds = std::array::from_fn(|i| decode((i as f64 + 0.5) / 255.0) as f32);
        let mut coarse = Vec::with_capacity(COARSE + 1);
        let mut srgb = 0u8;
        for k in 0..=COARSE {
            let v = k as f32 / COARSE as f32;
            while thresholds.get(usize::from(srgb)).is_some_and(|&t| v >= t) {
                srgb += 1;
            }
            coarse.push(srgb);
        }
        Tables {
            to_linear,
            thresholds,
            coarse,
        }
    })
}

/// sRGB byte → linear light (table lookup).
pub(crate) fn srgb_to_linear(c: u8) -> f32 {
    tables().to_linear[usize::from(c)]
}

/// Linear light → nearest sRGB byte: exact rounding of the sRGB transfer
/// function, via a coarse index plus at most a couple of threshold steps.
/// Values outside `0.0..=1.0` (and NaN) are clamped.
pub(crate) fn linear_to_srgb(v: f32) -> u8 {
    let t = tables();
    if v.is_nan() || v <= 0.0 {
        return 0;
    }
    if v >= 1.0 {
        return 255;
    }
    let k = (v * COARSE as f32) as usize;
    let mut srgb = t.coarse.get(k).copied().unwrap_or(255);
    while t
        .thresholds
        .get(usize::from(srgb))
        .is_some_and(|&threshold| v >= threshold)
    {
        srgb += 1;
    }
    srgb
}

/// Convert one sRGB pixel to premultiplied linear light.
fn premultiply(t: &Tables, [r, g, b, a]: [u8; 4]) -> Premul {
    let a = f32::from(a) / 255.0;
    let lin = |c: u8| t.to_linear[usize::from(c)] * a;
    [lin(r), lin(g), lin(b), a]
}

/// An image in premultiplied linear light.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LinearImage {
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// `width × height` pixels, top row first.
    pub(crate) pixels: Vec<Premul>,
}

impl LinearImage {
    /// A fully transparent image, or an empty one if the size overflows.
    fn clear(width: usize, height: usize) -> LinearImage {
        match width.checked_mul(height) {
            Some(n) => LinearImage {
                width,
                height,
                pixels: vec![[0.0; 4]; n],
            },
            None => LinearImage {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            },
        }
    }

    /// Straight-alpha sRGB bytes of one pixel.
    pub(crate) fn to_srgba(px: Premul) -> [u8; 4] {
        let [r, g, b, a] = px;
        if a.is_nan() || a <= 0.0 {
            return [0; 4];
        }
        let straight = |c: f32| linear_to_srgb(c / a);
        let alpha = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [straight(r), straight(g), straight(b), alpha]
    }
}

/// For each of `dst` outputs spread evenly over `src` source pixels grouped
/// in blocks of `block` (the last one may be shorter): the blocks it
/// overlaps, weighted by the covered source length (weights sum to 1).
fn taps(src: usize, block: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = src as f64 / dst as f64;
    let size = block as f64;
    let blocks = src.div_ceil(block);
    (0..dst)
        .map(|o| {
            let (a, b) = (o as f64 * scale, (o + 1) as f64 * scale);
            let first = (a / size).floor() as usize;
            let last = ((b / size).ceil() as usize).min(blocks);
            (first..last)
                .filter_map(|i| {
                    let lo = i as f64 * size;
                    let hi = (lo + size).min(src as f64);
                    let overlap = b.min(hi) - a.max(lo);
                    (overlap > 0.0).then_some((i, (overlap / (b - a)) as f32))
                })
                .collect()
        })
        .collect()
}

fn add_scaled(acc: &mut Premul, px: &Premul, w: f32) {
    for (a, p) in acc.iter_mut().zip(px) {
        *a += p * w;
    }
}

/// Integer pre-shrink: averages of `fx × fy` blocks of source pixels in
/// linear light, produced one block row at a time.
struct BlockRows<'a> {
    img: &'a Rgba,
    fx: usize,
    fy: usize,
    /// Blocks per row (the last block may be narrower).
    width: usize,
    /// Block rows (the last one may be shorter).
    height: usize,
}

impl<'a> BlockRows<'a> {
    /// Blocks for resampling a non-empty `img` to `dst_w × dst_h`.
    fn new(img: &'a Rgba, dst_w: usize, dst_h: usize) -> Self {
        let (w, h) = (img.width as usize, img.height as usize);
        let fx = (w / dst_w.saturating_mul(KEEP)).max(1);
        let fy = (h / dst_h.saturating_mul(KEEP)).max(1);
        BlockRows {
            img,
            fx,
            fy,
            width: w.div_ceil(fx),
            height: h.div_ceil(fy),
        }
    }

    /// Fill `out` (length `self.width`) with the averages of block row `row`.
    fn row(&self, t: &Tables, row: usize, out: &mut [Premul]) {
        out.fill([0.0; 4]);
        let (w, h) = (self.img.width as usize, self.img.height as usize);
        let stride = w * 4;
        let y0 = row * self.fy;
        let y1 = (y0 + self.fy).min(h);
        let rows = self
            .img
            .pixels
            .get(y0 * stride..y1 * stride)
            .unwrap_or_default();
        for src_row in rows.chunks_exact(stride) {
            for (acc, block) in out.iter_mut().zip(src_row.chunks(self.fx * 4)) {
                for &px in block.as_chunks::<4>().0 {
                    let p = premultiply(t, px);
                    for (a, v) in acc.iter_mut().zip(p) {
                        *a += v;
                    }
                }
            }
        }
        let block_h = y1.saturating_sub(y0);
        for (i, acc) in out.iter_mut().enumerate() {
            let x0 = i * self.fx;
            let block_w = (x0 + self.fx).min(w).saturating_sub(x0);
            let count = (block_w * block_h).max(1) as f32;
            for a in acc.iter_mut() {
                *a /= count;
            }
        }
    }
}

/// Resample `img` to `width × height` pixels: an area average in
/// premultiplied linear light. An invalid or empty source gives a fully
/// transparent result.
pub(crate) fn resample(img: &Rgba, width: usize, height: usize) -> LinearImage {
    if width == 0 || height == 0 || img.is_empty() || width.checked_mul(height).is_none() {
        return LinearImage::clear(width, height);
    }
    let t = tables();
    let blocks = BlockRows::new(img, width, height);
    let htaps = taps(img.width as usize, blocks.fx, width);
    let vtaps = taps(img.height as usize, blocks.fy, height);

    // Horizontal pass: every block row averaged down to `width` columns.
    let mut row = vec![[0.0f32; 4]; blocks.width];
    let mut narrow = Vec::with_capacity(blocks.height * width);
    for y in 0..blocks.height {
        blocks.row(t, y, &mut row);
        for taps in &htaps {
            let mut acc = [0.0f32; 4];
            for &(i, w) in taps {
                if let Some(px) = row.get(i) {
                    add_scaled(&mut acc, px, w);
                }
            }
            narrow.push(acc);
        }
    }

    // Vertical pass over the narrow rows.
    let mut pixels = vec![[0.0f32; 4]; width * height];
    for (out_row, taps) in pixels.chunks_exact_mut(width).zip(&vtaps) {
        for &(j, w) in taps {
            let Some(src) = narrow.get(j * width..(j + 1) * width) else {
                continue;
            };
            for (acc, px) in out_row.iter_mut().zip(src) {
                add_scaled(acc, px, w);
            }
        }
    }
    LinearImage {
        width,
        height,
        pixels,
    }
}

/// `img` resized to `width × height` pixels with a linear-light box filter:
/// gamma-correct downscaling, while upscaling repeats pixels and blends only
/// at the seams. An invalid source gives a transparent image of that size;
/// a size whose buffer would not fit in memory addresses gives an empty
/// image.
pub fn resize(img: &Rgba, width: u32, height: u32) -> Rgba {
    if Rgba::byte_len(width, height).is_none() {
        return Rgba::default();
    }
    let lin = resample(img, width as usize, height as usize);
    let pixels = lin
        .pixels
        .into_iter()
        .flat_map(LinearImage::to_srgba)
        .collect();
    Rgba {
        width,
        height,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, pixels: &[[u8; 4]]) -> Rgba {
        Rgba::new(width, height, pixels.concat()).unwrap()
    }

    #[test]
    fn transfer_tables_round_trip() {
        for c in 0..=255u8 {
            assert_eq!(linear_to_srgb(srgb_to_linear(c)), c);
            let reference = crate::color::srgb_to_linear(c);
            assert!((srgb_to_linear(c) - reference).abs() <= 1e-6, "{c}");
        }
        assert_eq!(linear_to_srgb(-1.0), 0);
        assert_eq!(linear_to_srgb(f32::NAN), 0);
        assert_eq!(linear_to_srgb(2.0), 255);
        assert_eq!(linear_to_srgb(0.5), 188);
    }

    #[test]
    fn fast_encoding_matches_the_reference() {
        for i in 0..=100_000 {
            let v = i as f32 / 100_000.0;
            let fast = linear_to_srgb(v);
            let slow = crate::color::linear_to_srgb(v);
            assert!(fast.abs_diff(slow) <= 1, "{v}: {fast} vs {slow}");
        }
    }

    #[test]
    fn taps_cover_each_output_exactly() {
        for (src, block, dst) in [(10, 1, 3), (3, 1, 10), (7, 1, 7), (1, 1, 5), (1000, 3, 80)] {
            let taps = taps(src, block, dst);
            assert_eq!(taps.len(), dst);
            for (o, t) in taps.iter().enumerate() {
                let sum: f32 = t.iter().map(|&(_, w)| w).sum();
                assert!(
                    (sum - 1.0).abs() < 1e-5,
                    "{src}/{block}->{dst} output {o}: {sum}"
                );
                assert!(t.iter().all(|&(i, w)| i < src.div_ceil(block) && w > 0.0));
            }
        }
        assert_eq!(
            taps(4, 1, 2),
            vec![vec![(0, 0.5), (1, 0.5)], vec![(2, 0.5), (3, 0.5)]]
        );
        assert_eq!(taps(2, 1, 4)[1], vec![(0, 1.0)]);
        // A short last block counts for its real width: 10 px in blocks of 4.
        assert_eq!(taps(10, 4, 1), vec![vec![(0, 0.4), (1, 0.4), (2, 0.2)]]);
    }

    #[test]
    fn identity_size_is_exact() {
        let img = image(2, 1, &[[10, 20, 30, 255], [200, 100, 0, 255]]);
        assert_eq!(resize(&img, 2, 1), img);
    }

    #[test]
    fn averages_light_not_codes() {
        let img = image(2, 1, &[[0, 0, 0, 255], [255, 255, 255, 255]]);
        assert_eq!(resize(&img, 1, 1).pixel(0, 0), [188, 188, 188, 255]);
    }

    #[test]
    fn transparent_pixels_add_no_colour() {
        let img = image(2, 1, &[[255, 0, 0, 255], [0, 255, 0, 0]]);
        assert_eq!(resize(&img, 1, 1).pixel(0, 0), [255, 0, 0, 128]);
        let clear = image(2, 1, &[[255, 0, 0, 0], [0, 255, 0, 0]]);
        assert_eq!(resize(&clear, 1, 1).pixel(0, 0), [0; 4]);
    }

    /// The direct linear-light average of a source rectangle, as sRGB.
    fn direct_average(img: &Rgba, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>) -> [u8; 3] {
        let mut acc = [0.0f64; 3];
        let n = f64::from(xs.len() as u32 * ys.len() as u32);
        for y in ys {
            for x in xs.clone() {
                for (a, c) in acc.iter_mut().zip(img.pixel(x, y)) {
                    *a += f64::from(srgb_to_linear(c));
                }
            }
        }
        acc.map(|a| linear_to_srgb((a / n) as f32))
    }

    #[test]
    fn preshrink_matches_the_direct_average() {
        // A 400×400 gradient into 5×5 pre-shrinks by 20 in each direction.
        let mut px = Vec::new();
        for y in 0..400u32 {
            for x in 0..400u32 {
                px.push([(x * 255 / 399) as u8, (y * 255 / 399) as u8, 90, 255]);
            }
        }
        let img = image(400, 400, &px);
        let blocks = BlockRows::new(&img, 5, 5);
        assert_eq!((blocks.fx, blocks.fy), (20, 20));
        let out = resize(&img, 5, 5);
        for oy in 0..5u32 {
            for ox in 0..5u32 {
                let want = direct_average(&img, ox * 80..(ox + 1) * 80, oy * 80..(oy + 1) * 80);
                let got = out.pixel(ox, oy);
                for (w, g) in want.iter().zip(got) {
                    assert!(w.abs_diff(g) <= 1, "({ox},{oy}): {want:?} vs {got:?}");
                }
            }
        }
    }

    #[test]
    fn short_last_block_is_weighted_by_area() {
        // 90 columns into 1: blocks of 22 with a 2-column last block that
        // holds the only black pixel.
        let mut px = vec![[255, 255, 255, 255]; 89];
        px.push([0, 0, 0, 255]);
        let img = image(90, 1, &px);
        let blocks = BlockRows::new(&img, 1, 1);
        assert_eq!((blocks.fx, blocks.width), (22, 5));
        let got = resize(&img, 1, 1).pixel(0, 0);
        assert_eq!(got[..3], direct_average(&img, 0..90, 0..1));
    }

    #[test]
    fn degenerate_inputs() {
        let img = image(1, 1, &[[1, 2, 3, 255]]);
        assert_eq!(resize(&img, 0, 3), Rgba::new(0, 3, Vec::new()).unwrap());
        let invalid = Rgba {
            width: 2,
            height: 2,
            pixels: vec![255; 3],
        };
        assert_eq!(resize(&invalid, 1, 1).pixel(0, 0), [0; 4]);
        assert_eq!(resize(&Rgba::default(), 2, 1).pixels, vec![0; 8]);
        let up = resize(&img, 3, 2);
        let (up_px, _) = up.pixels.as_chunks::<4>();
        assert!(up_px.iter().all(|p| *p == [1, 2, 3, 255]));
        assert_eq!(resample(&img, usize::MAX, 2).pixels.len(), 0);
    }
}
