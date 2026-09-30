//! Area-averaging (box filter) resampling in linear light.
//!
//! Averaging must add up *light*, not sRGB code values: a fine black-and-white
//! pattern has to become 50 % grey (sRGB 188), not sRGB 128. Pixels are
//! therefore converted to linear RGB before averaging, and each output pixel
//! averages exactly the source area it covers. Transparency enters in one of
//! two ways ([`Blend`]): with premultiplied alpha, so transparent pixels add
//! no colour, or composited onto a known background first.
//!
//! Sources much larger than the target are first reduced by an integer
//! factor per axis with plain block sums (still in linear light), keeping at
//! least [`KEEP`] blocks per output pixel and axis. The exact fractional pass
//! then weighs each block by the source area it covers, so the result stays
//! within a fraction of a sub-pixel of the direct average while touching far
//! fewer pixels.
//!
//! Output rows are produced top to bottom from a sliding window of the few
//! block rows each one needs, so the working memory beyond the result is
//! proportional to the output width, whatever the size of the source.

use std::collections::VecDeque;
use std::sync::OnceLock;

use super::{Rgba, blend_srgb};
use crate::style::Rgb;

/// Linear-light RGBA: `[r, g, b, a]`, each in `0.0..=1.0`. With
/// [`Blend::Premultiplied`] the colour is premultiplied by the alpha; with
/// [`Blend::Onto`] it is the colour composited onto the background.
pub(crate) type Premul = [f32; 4];

/// How source pixels enter the average.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blend {
    /// Premultiplied alpha: transparent pixels add no colour, and the
    /// average alpha says how much of the output pixel is covered.
    Premultiplied,
    /// Every pixel is first composited onto this opaque sRGB background, on
    /// sRGB values as browsers do, and the opaque results are averaged. The
    /// alpha is still averaged alongside, so uncovered areas can be told
    /// apart.
    Onto([u8; 3]),
}

/// Minimum blocks per output pixel and axis that the integer pre-shrink
/// leaves for the exact fractional pass.
const KEEP: usize = 4;

/// Resolution of the coarse index into the linear → sRGB thresholds.
const COARSE: usize = 4096;

/// Conversion tables between sRGB bytes and linear light.
struct Tables {
    /// sRGB byte → linear light.
    to_linear: [f32; 256],
    /// Alpha byte → alpha in `0.0..=1.0`.
    alpha: [f32; 256],
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
        let alpha = std::array::from_fn(|i| i as f32 / 255.0);
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
            alpha,
            thresholds,
            coarse,
        }
    })
}

impl Tables {
    /// Linear light → nearest sRGB byte: exact rounding of the sRGB
    /// transfer function, via the coarse index plus at most a couple of
    /// threshold steps. Values outside `0.0..=1.0` (and NaN) are clamped.
    fn encode(&self, v: f32) -> u8 {
        if v.is_nan() || v <= 0.0 {
            return 0;
        }
        if v >= 1.0 {
            return 255;
        }
        let k = (v * COARSE as f32) as usize;
        let mut srgb = self.coarse.get(k).copied().unwrap_or(255);
        while self
            .thresholds
            .get(usize::from(srgb))
            .is_some_and(|&threshold| v >= threshold)
        {
            srgb += 1;
        }
        srgb
    }

    /// One sRGB pixel in premultiplied linear light.
    fn premultiply(&self, [r, g, b, a]: [u8; 4]) -> Premul {
        let a = self.alpha[usize::from(a)];
        let lin = |c: u8| self.to_linear[usize::from(c)] * a;
        [lin(r), lin(g), lin(b), a]
    }

    /// One sRGB pixel composited onto `under`, in linear light, with its
    /// alpha.
    fn composite(&self, [r, g, b, a]: [u8; 4], under: [u8; 3]) -> Premul {
        let lin = |c: u8| self.to_linear[usize::from(c)];
        if a == 255 {
            return [lin(r), lin(g), lin(b), 1.0];
        }
        let [ur, ug, ub] = under;
        [
            lin(blend_srgb(r, ur, a)),
            lin(blend_srgb(g, ug, a)),
            lin(blend_srgb(b, ub, a)),
            self.alpha[usize::from(a)],
        ]
    }

    /// Straight-alpha sRGB bytes of a premultiplied pixel.
    fn to_srgba(&self, [r, g, b, a]: Premul) -> [u8; 4] {
        if a.is_nan() || a <= 0.0 {
            return [0; 4];
        }
        let straight = |c: f32| self.encode(c / a);
        let alpha = (a.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [straight(r), straight(g), straight(b), alpha]
    }

    /// Opaque sRGB bytes of a composited ([`Blend::Onto`]) pixel.
    fn to_opaque(&self, [r, g, b, _]: Premul) -> [u8; 4] {
        [self.encode(r), self.encode(g), self.encode(b), 255]
    }
}

/// sRGB byte → linear light (table lookup).
pub(crate) fn srgb_to_linear(c: u8) -> f32 {
    tables().to_linear[usize::from(c)]
}

/// Linear light → nearest sRGB byte (exact rounding; out-of-range values
/// and NaN are clamped).
pub(crate) fn linear_to_srgb(v: f32) -> u8 {
    tables().encode(v)
}

/// An image in linear light (see [`Premul`] for what the colour holds).
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

    /// Straight-alpha sRGB bytes of one premultiplied pixel.
    pub(crate) fn to_srgba(px: Premul) -> [u8; 4] {
        tables().to_srgba(px)
    }
}

/// For each of `dst` outputs spread evenly over `src` source pixels grouped
/// in blocks of `block` (the last one may be shorter): the blocks it
/// overlaps, in order, weighted by the covered source length (weights sum
/// to 1).
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

/// Resampling along one axis: the integer pre-shrink factor and the blocks
/// each output pixel averages.
struct Axis {
    /// Source pixels per block (the last block may be shorter).
    block: usize,
    /// Number of blocks.
    blocks: usize,
    /// Per output pixel, `(block, weight)` pairs in block order. A weight is
    /// the covered share of the output pixel divided by the block's length,
    /// so it applies to block *sums* and the per-pixel loops never divide.
    taps: Vec<Vec<(usize, f32)>>,
}

impl Axis {
    /// The axis from `src` source pixels to `dst` output pixels (both > 0).
    fn new(src: usize, dst: usize) -> Axis {
        let block = (src / dst.saturating_mul(KEEP)).max(1);
        let len = |i: usize| block.min(src.saturating_sub(i * block)).max(1) as f32;
        let taps = taps(src, block, dst)
            .into_iter()
            .map(|taps| taps.into_iter().map(|(i, w)| (i, w / len(i))).collect())
            .collect();
        Axis {
            block,
            blocks: src.div_ceil(block),
            taps,
        }
    }
}

fn add_scaled(acc: &mut Premul, px: &Premul, w: f32) {
    for (a, p) in acc.iter_mut().zip(px) {
        *a += p * w;
    }
}

fn accumulate(acc: &mut Premul, px: Premul) {
    for (a, p) in acc.iter_mut().zip(px) {
        *a += p;
    }
}

/// Fill `sums` (one entry per column block) with the sums of the converted
/// pixels in block row `row`.
fn block_sums(
    img: &Rgba,
    (x, y): (&Axis, &Axis),
    row: usize,
    sums: &mut [Premul],
    convert: impl Fn([u8; 4]) -> Premul,
) {
    sums.fill([0.0; 4]);
    let stride = img.width as usize * 4;
    let height = img.height as usize;
    let y0 = row.saturating_mul(y.block).min(height);
    let y1 = y0.saturating_add(y.block).min(height);
    let rows = img.pixels.get(y0 * stride..y1 * stride).unwrap_or_default();
    for src_row in rows.chunks(stride.max(1)) {
        let (pixels, _) = src_row.as_chunks::<4>();
        if x.block == 1 {
            for (acc, &px) in sums.iter_mut().zip(pixels) {
                accumulate(acc, convert(px));
            }
        } else {
            for (acc, block) in sums.iter_mut().zip(pixels.chunks(x.block)) {
                for &px in block {
                    accumulate(acc, convert(px));
                }
            }
        }
    }
}

/// One row of block sums averaged down to the output width.
fn narrow(taps: &[Vec<(usize, f32)>], sums: &[Premul], out: &mut [Premul]) {
    for (acc, taps) in out.iter_mut().zip(taps) {
        let mut px = [0.0; 4];
        for &(i, w) in taps {
            if let Some(sum) = sums.get(i) {
                add_scaled(&mut px, sum, w);
            }
        }
        *acc = px;
    }
}

/// Resample a valid, non-empty `img` to `width × height` (both > 0) and
/// hand each output row, top to bottom, to `emit`.
fn for_each_row(
    img: &Rgba,
    width: usize,
    height: usize,
    blend: Blend,
    mut emit: impl FnMut(&[Premul]),
) {
    let t = tables();
    let x = Axis::new(img.width as usize, width);
    let y = Axis::new(img.height as usize, height);
    let mut sums = vec![[0.0; 4]; x.blocks];
    // The narrow rows of block rows `start..start + window.len()`.
    let mut window: VecDeque<Vec<Premul>> = VecDeque::new();
    let mut start = 0;
    let mut spare: Vec<Vec<Premul>> = Vec::new();
    let mut out = vec![[0.0; 4]; width];
    for taps in &y.taps {
        out.fill([0.0; 4]);
        let (Some(&(first, _)), Some(&(last, _))) = (taps.first(), taps.last()) else {
            emit(&out);
            continue;
        };
        // Block rows above this output row are done with.
        while start < first {
            spare.extend(window.pop_front());
            start += 1;
        }
        // Narrow the block rows it needs that are not in the window yet.
        while start + window.len() <= last.min(y.blocks.saturating_sub(1)) {
            let j = start + window.len();
            match blend {
                Blend::Premultiplied => {
                    block_sums(img, (&x, &y), j, &mut sums, |px| t.premultiply(px));
                }
                Blend::Onto(under) => {
                    block_sums(img, (&x, &y), j, &mut sums, |px| t.composite(px, under));
                }
            }
            let mut row = spare.pop().unwrap_or_else(|| vec![[0.0; 4]; width]);
            narrow(&x.taps, &sums, &mut row);
            window.push_back(row);
        }
        for &(j, w) in taps {
            if let Some(row) = j.checked_sub(start).and_then(|k| window.get(k)) {
                for (acc, px) in out.iter_mut().zip(row) {
                    add_scaled(acc, px, w);
                }
            }
        }
        emit(&out);
    }
}

/// Resample `img` to `width × height` pixels: an area average in linear
/// light, with transparency handled as `blend` says. An invalid or empty
/// source gives a fully transparent result.
pub(crate) fn resample(img: &Rgba, width: usize, height: usize, blend: Blend) -> LinearImage {
    let n = match width.checked_mul(height) {
        Some(n) if n > 0 && !img.is_empty() => n,
        _ => return LinearImage::clear(width, height),
    };
    let mut pixels = Vec::with_capacity(n);
    for_each_row(img, width, height, blend, |row| {
        pixels.extend_from_slice(row)
    });
    LinearImage {
        width,
        height,
        pixels,
    }
}

/// `width × height` pixels made by `pixel` from the resampled image; the
/// fill for an invalid or empty source is `empty`.
fn resize_with(
    img: &Rgba,
    (width, height): (u32, u32),
    blend: Blend,
    empty: [u8; 4],
    pixel: impl Fn(&Tables, Premul) -> [u8; 4],
) -> Rgba {
    let Some(len) = Rgba::byte_len(width, height) else {
        return Rgba::default();
    };
    let mut pixels = Vec::new();
    if pixels.try_reserve_exact(len).is_err() {
        return Rgba::default();
    }
    if len > 0 && !img.is_empty() {
        let t = tables();
        for_each_row(img, width as usize, height as usize, blend, |row| {
            for &px in row {
                pixels.extend_from_slice(&pixel(t, px));
            }
        });
    }
    while pixels.len() < len {
        pixels.extend_from_slice(&empty);
    }
    Rgba {
        width,
        height,
        pixels,
    }
}

/// `img` resized to `width × height` pixels with a linear-light box filter:
/// gamma-correct downscaling, while upscaling repeats pixels and blends only
/// at the seams. Colour is averaged premultiplied by alpha, so the result
/// keeps the image's transparency. An invalid source gives a transparent
/// image of that size; a size whose buffer cannot be allocated gives an
/// empty image. Working memory beyond the result is proportional to
/// `width`.
pub fn resize(img: &Rgba, width: u32, height: u32) -> Rgba {
    resize_with(
        img,
        (width, height),
        Blend::Premultiplied,
        [0; 4],
        Tables::to_srgba,
    )
}

/// `img` composited onto `background` and resized to `width × height`
/// opaque pixels, for protocols without transparency (sixel).
///
/// Each source pixel is blended onto the background on sRGB values, as
/// browsers do, *before* the linear-light average, so edges that the
/// downscale leaves partly covered mix like light. Flattening after
/// [`resize`] would blend that coverage in sRGB instead, which darkens thin
/// light strokes on a dark background. An invalid source gives the plain
/// background; sizes behave as for [`resize`].
pub fn resize_onto(img: &Rgba, width: u32, height: u32, background: Rgb) -> Rgba {
    let under = [background.0, background.1, background.2];
    resize_with(
        img,
        (width, height),
        Blend::Onto(under),
        [under[0], under[1], under[2], 255],
        Tables::to_opaque,
    )
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
                assert!(t.windows(2).all(|p| p[0].0 + 1 == p[1].0), "contiguous");
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
    fn axis_weights_apply_to_block_sums() {
        // 90 columns into 1: blocks of 22 and a 2-column last block. Each
        // weight is coverage ÷ block length, so weight × length sums to 1
        // and every source pixel counts 1/90.
        let axis = Axis::new(90, 1);
        assert_eq!((axis.block, axis.blocks), (22, 5));
        let lens = [22.0, 22.0, 22.0, 22.0, 2.0];
        let total: f32 = axis.taps[0].iter().map(|&(i, w)| w * lens[i]).sum();
        assert!((total - 1.0).abs() < 1e-6, "{total}");
        assert!(
            axis.taps[0]
                .iter()
                .all(|&(_, w)| (w - 1.0 / 90.0).abs() < 1e-7)
        );
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

    #[test]
    fn composited_resize_mixes_coverage_as_light() {
        // White and clear, onto black: half the light of white (sRGB 188).
        // Blending the averaged alpha on sRGB values would give 128.
        let img = image(2, 1, &[[255, 255, 255, 255], [0, 0, 0, 0]]);
        let flat = resize_onto(&img, 1, 1, Rgb(0, 0, 0));
        assert_eq!(flat.pixel(0, 0), [188, 188, 188, 255]);
        assert_eq!(
            resize(&img, 1, 1).flatten(Rgb(0, 0, 0)).pixel(0, 0),
            [128, 128, 128, 255]
        );
        // At 1:1, partial alpha is blended on sRGB values like `flatten`.
        let half = image(1, 1, &[[255, 0, 0, 128]]);
        assert_eq!(
            resize_onto(&half, 1, 1, Rgb(0, 0, 255)),
            half.flatten(Rgb(0, 0, 255))
        );
        // Opaque images come out as a plain resize.
        let opaque = image(2, 1, &[[0, 0, 0, 255], [255, 255, 255, 255]]);
        assert_eq!(
            resize_onto(&opaque, 1, 1, Rgb(9, 9, 9)),
            resize(&opaque, 1, 1)
        );
        // Nothing to draw: the background.
        let bg = resize_onto(&Rgba::default(), 2, 1, Rgb(1, 2, 3));
        assert_eq!(bg.pixels, [1, 2, 3, 255, 1, 2, 3, 255]);
        // The coverage is still there for telling uncovered areas apart.
        let composited = resample(&img, 1, 1, Blend::Onto([0, 0, 0]));
        assert!((composited.pixels[0][3] - 0.5).abs() < 1e-6);
    }

    /// The exact linear-light area average (in `f64`) of the source
    /// rectangle `xs × ys` (fractional bounds): straight-alpha sRGB for
    /// [`Blend::Premultiplied`], opaque composited sRGB for [`Blend::Onto`].
    fn exact_with(img: &Rgba, xs: (f64, f64), ys: (f64, f64), blend: Blend) -> [u8; 4] {
        let overlap = |i: u32, (lo, hi): (f64, f64)| {
            (f64::from(i + 1).min(hi) - f64::from(i).max(lo)).max(0.0)
        };
        let mut acc = [0.0f64; 4];
        for y in ys.0.floor() as u32..(ys.1.ceil() as u32).min(img.height) {
            for x in xs.0.floor() as u32..(xs.1.ceil() as u32).min(img.width) {
                let w = overlap(x, xs) * overlap(y, ys);
                let [r, g, b, a] = img.pixel(x, y);
                let (colour, weight) = match blend {
                    Blend::Premultiplied => ([r, g, b], w * f64::from(a) / 255.0),
                    Blend::Onto([ur, ug, ub]) => (
                        [
                            blend_srgb(r, ur, a),
                            blend_srgb(g, ug, a),
                            blend_srgb(b, ub, a),
                        ],
                        w,
                    ),
                };
                for (acc, c) in acc.iter_mut().zip(colour) {
                    *acc += weight * f64::from(srgb_to_linear(c));
                }
                acc[3] += w * f64::from(a) / 255.0;
            }
        }
        let area = (xs.1 - xs.0) * (ys.1 - ys.0);
        let [r, g, b, a] = acc;
        let c = |v: f64| linear_to_srgb(v as f32);
        match blend {
            Blend::Onto(_) => [c(r / area), c(g / area), c(b / area), 255],
            Blend::Premultiplied if a <= 0.0 => [0; 4],
            Blend::Premultiplied => [c(r / a), c(g / a), c(b / a), (a / area * 255.0 + 0.5) as u8],
        }
    }

    /// [`exact_with`] for premultiplied averaging.
    fn exact_average(img: &Rgba, xs: (f64, f64), ys: (f64, f64)) -> [u8; 4] {
        exact_with(img, xs, ys, Blend::Premultiplied)
    }

    /// Check `resize(img, w, h)` against [`exact_average`] per pixel.
    fn assert_exact(img: &Rgba, (w, h): (u32, u32), tolerance: u8) {
        let out = resize(img, w, h);
        let sx = f64::from(img.width) / f64::from(w);
        let sy = f64::from(img.height) / f64::from(h);
        for oy in 0..h {
            for ox in 0..w {
                let xs = (f64::from(ox) * sx, f64::from(ox + 1) * sx);
                let ys = (f64::from(oy) * sy, f64::from(oy + 1) * sy);
                let want = exact_average(img, xs, ys);
                let got = out.pixel(ox, oy);
                for (a, b) in want.iter().zip(got) {
                    assert!(
                        a.abs_diff(b) <= tolerance,
                        "{w}x{h} ({ox},{oy}): {want:?} vs {got:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn matches_the_exact_average_without_preshrink() {
        // Fractional ratios, down and up, with transparency everywhere.
        let px: Vec<[u8; 4]> = (0..23u32 * 17)
            .map(|i| {
                let (x, y) = (i % 23, i / 23);
                [
                    (x * 11) as u8,
                    (y * 15) as u8,
                    ((x ^ y) * 9) as u8,
                    (40 + x * 9) as u8,
                ]
            })
            .collect();
        let img = image(23, 17, &px);
        for size in [(7, 5), (23, 17), (9, 13), (30, 4), (1, 1), (46, 34)] {
            assert_exact(&img, size, 1);
        }
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
        let axis = Axis::new(400, 5);
        assert_eq!((axis.block, axis.blocks), (20, 20));
        assert_exact(&img, (5, 5), 1);
        // 400 into 7 splits blocks between outputs: still close.
        assert_exact(&img, (7, 7), 2);
    }

    #[test]
    fn short_last_block_is_weighted_by_area() {
        // 90 columns into 1: blocks of 22 with a 2-column last block that
        // holds the only black pixel.
        let mut px = vec![[255, 255, 255, 255]; 89];
        px.push([0, 0, 0, 255]);
        let img = image(90, 1, &px);
        assert_exact(&img, (1, 1), 0);
    }

    #[test]
    fn rows_stream_through_the_window_for_every_ratio() {
        // A vertical gradient through up, down and mixed ratios: each
        // output row must be the average of exactly its band of rows.
        let px: Vec<[u8; 4]> = (0..3 * 50u32)
            .map(|i| {
                let v = (i / 3 * 5) as u8;
                [v, 255 - v, v / 2, 255]
            })
            .collect();
        let img = image(3, 50, &px);
        for h in [7, 12, 13, 25, 49, 50, 51, 100, 333] {
            assert_exact(&img, (2, h), 1);
        }
        // Pre-shrunk ratios are close to the exact average.
        for h in [1, 2, 3, 5] {
            assert_exact(&img, (2, h), 3);
        }
    }

    /// Deterministic pseudo-random bytes (SplitMix64).
    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut z = seed;
        (0..len)
            .map(|_| {
                z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut x = z;
                x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                (x >> 56) as u8
            })
            .collect()
    }

    proptest::proptest! {
        /// Every ratio the exact pass handles alone (no integer pre-shrink)
        /// matches the direct `f64` area average, in both blend modes.
        #[test]
        fn resizing_is_the_exact_area_average(
            (w, h) in (1u32..14, 1u32..14),
            (tw, th) in (1u32..18, 1u32..18),
            seed in proptest::prelude::any::<u64>(),
            onto in proptest::option::of(proptest::prelude::any::<[u8; 3]>()),
        ) {
            // Below 8 source pixels per output pixel there is no integer
            // pre-shrink, so the exact pass alone must match.
            let (w, h) = (if tw == 1 { w.min(7) } else { w }, if th == 1 { h.min(7) } else { h });
            proptest::prop_assert_eq!(Axis::new(w as usize, tw as usize).block, 1);
            proptest::prop_assert_eq!(Axis::new(h as usize, th as usize).block, 1);
            let img = Rgba::new(w, h, noise(seed, (w * h * 4) as usize)).unwrap();
            let blend = onto.map_or(Blend::Premultiplied, Blend::Onto);
            let out = match onto {
                Some([r, g, b]) => resize_onto(&img, tw, th, Rgb(r, g, b)),
                None => resize(&img, tw, th),
            };
            proptest::prop_assert!(out.is_valid());
            let (sx, sy) = (f64::from(w) / f64::from(tw), f64::from(h) / f64::from(th));
            for oy in 0..th {
                for ox in 0..tw {
                    let xs = (f64::from(ox) * sx, f64::from(ox + 1) * sx);
                    let ys = (f64::from(oy) * sy, f64::from(oy + 1) * sy);
                    let want = exact_with(&img, xs, ys, blend);
                    let got = out.pixel(ox, oy);
                    proptest::prop_assert!(want[3].abs_diff(got[3]) <= 1, "{:?} vs {:?}", want, got);
                    // Straight colour is ill-conditioned when almost nothing
                    // is covered; compare it once there is enough alpha.
                    if blend != Blend::Premultiplied || want[3] >= 32 {
                        for (a, b) in want.iter().zip(got).take(3) {
                            proptest::prop_assert!(
                                a.abs_diff(b) <= 1,
                                "{}x{} → {}x{} ({},{}): {:?} vs {:?}", w, h, tw, th, ox, oy, want, got
                            );
                        }
                    }
                }
            }
        }
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
        let too_big = resample(&img, usize::MAX, 2, Blend::Premultiplied);
        assert_eq!(too_big.pixels.len(), 0);
    }

    #[test]
    fn impossible_sizes_give_an_empty_image() {
        // Buffers beyond the address space are refused instead of
        // panicking or aborting.
        let img = image(1, 1, &[[1, 2, 3, 255]]);
        assert_eq!(resize(&img, u32::MAX, u32::MAX), Rgba::default());
        assert_eq!(resize(&img, u32::MAX, 1 << 30), Rgba::default());
        assert_eq!(
            resize_onto(&img, u32::MAX, 1 << 30, Rgb(0, 0, 0)),
            Rgba::default()
        );
    }
}
