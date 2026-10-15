//! Ordered dithering of block images for 256- and 16-colour terminals.
//!
//! Without 24-bit colour (a tmux client behind mosh, `--color=256`), every
//! sub-pixel of a block image would snap to its nearest palette entry:
//! gradients turn into bands and photographs look posterised. [`Dither`]
//! instead mixes the palette entries around a colour in the proportions that
//! reproduce it *in linear light* (what the eye averages over neighbouring
//! sub-pixels), picking one per sub-pixel with an 8×8 Bayer matrix.
//!
//! * **256 colours** ([`color::to_256`] maps to entries 16–255): chromatic
//!   colours are dithered in the 6×6×6 cube, each channel between its two
//!   neighbouring cube levels, with one threshold for all three so the hue
//!   holds together. Colours with little chroma use the finer grey ladder
//!   instead (the 24-step ramp plus the cube's six greys), matched on
//!   luminance.
//! * **16 colours**: the two palette entries whose mix comes closest in
//!   OKLab, with a penalty for mixing entries far apart (the noise a
//!   reader sees); the nearest entry alone when no mix beats it. The
//!   colours are xterm's defaults, as for [`color::to_16`].
//!
//! The threshold depends only on the sub-pixel's position in the grid the
//! image was resampled to, so the result is deterministic and anchored to
//! the image: a raster is made once for the whole image, and scrolling
//! shows its rows without changing the pattern.
//!
//! The decisions that need OKLab (grey or cube; which pair) are made once
//! per bin of colours (32 or 16 levels a channel) and remembered for the
//! run; the mixing proportion is computed for each sub-pixel's exact
//! colour.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU16, Ordering};

use crate::color::{self, CUBE, palette_lab, xterm_rgb};
use crate::term::ColorDepth;

/// The 8×8 Bayer matrix: every threshold from 0 to 63, spread so that any
/// proportion of a block's cells is evenly scattered.
const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// The threshold at sub-pixel `(x, y)` of the image, in `(0, 1)`: the upper
/// of two palette entries is taken where the mix wants more of it.
fn threshold(x: usize, y: usize) -> f32 {
    let row = BAYER.get(y % 8).copied().unwrap_or_default();
    let level = row.get(x % 8).copied().unwrap_or_default();
    (f32::from(level) + 0.5) / 64.0
}

/// OKLab chroma below which a colour is dithered on the grey ladder (256
/// colours): there it is close to the grey the reader would call it, and
/// the ladder's 30 steps are much finer than the cube's 6.
const GREY_CHROMA: f32 = 0.02;

/// Weight of the noise of a two-entry mix (16 colours): the variance
/// `t(1 − t)·ΔE²` of mixing `t` of one entry into the other, against the
/// squared error of the mix's mean.
const MIX_NOISE: f32 = 0.03;

/// Bits a channel keeps in the bins decisions are remembered for: 32
/// levels for 256 colours (where the grey-or-cube line runs), 16 for the
/// 16 colours (whose entries lie far apart, and whose pair search is the
/// costlier one).
const fn bin_bits(target: Target) -> u32 {
    match target {
        Target::Ansi256 => 5,
        Target::Ansi16 => 4,
    }
}

/// Cube entries start at palette index 16.
const CUBE_BASE: u8 = 16;

/// Picks palette entries for the sub-pixels of a raster.
pub(crate) struct Dither {
    target: Target,
}

/// The decisions made so far, per colour bin, shared by every raster and
/// thread of the run (a decision only depends on the bin): 0 while
/// undecided, else 1 + the decision (256 colours: [`GREY`] or
/// [`CUBE_FAMILY`]; 16 colours: the pair `a·16 + b`).
static DECIDED_256: [AtomicU16; 1 << (3 * bin_bits(Target::Ansi256))] =
    [const { AtomicU16::new(0) }; 1 << (3 * bin_bits(Target::Ansi256))];
static DECIDED_16: [AtomicU16; 1 << (3 * bin_bits(Target::Ansi16))] =
    [const { AtomicU16::new(0) }; 1 << (3 * bin_bits(Target::Ansi16))];

/// The palette dithered to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Ansi256,
    Ansi16,
}

/// 256-colour decisions.
const GREY: u16 = 0;
const CUBE_FAMILY: u16 = 1;

impl Dither {
    /// A ditherer for `depth`; `None` where there is nothing to dither
    /// (24-bit colour, or no colours).
    pub(crate) fn new(depth: ColorDepth) -> Option<Dither> {
        let target = match depth {
            ColorDepth::Ansi256 => Target::Ansi256,
            ColorDepth::Ansi16 => Target::Ansi16,
            ColorDepth::TrueColor | ColorDepth::Mono | ColorDepth::None => return None,
        };
        Some(Dither { target })
    }

    /// The palette index for the colour `srgb` (`lin` in linear light) at
    /// sub-pixel `(x, y)` of the image: 16–255 for 256 colours, 0–15 for 16.
    pub(crate) fn index(&self, srgb: [u8; 3], lin: [f32; 3], x: usize, y: usize) -> u8 {
        let threshold = threshold(x, y);
        let decision = self.decide(srgb);
        let t = tables();
        match self.target {
            Target::Ansi256 if decision == GREY => {
                let [r, g, b] = lin;
                let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                let (lo, hi, frac) = bracket(&t.greys, luminance);
                if frac > threshold { hi } else { lo }
            }
            Target::Ansi256 => {
                let level = |c: u8| {
                    let (lo, frac) = t.cube_steps[usize::from(c)];
                    lo + u8::from(frac > threshold)
                };
                let [r, g, b] = srgb.map(level);
                CUBE_BASE + 36 * r + 6 * g + b
            }
            Target::Ansi16 => {
                let (a, b) = (decision / 16, decision % 16);
                let (a, b) = (a as u8, b as u8);
                if a == b {
                    return a;
                }
                let frac = mix_fraction(lin, t.ansi16[usize::from(a)], t.ansi16[usize::from(b)]);
                if frac > threshold { b } else { a }
            }
        }
    }

    /// The decision for the bin of `srgb`, made the first time it is met.
    fn decide(&self, srgb: [u8; 3]) -> u16 {
        let bits = bin_bits(self.target);
        let [r, g, b] = srgb.map(|c| usize::from(c >> (8 - bits)));
        let bin = (r << (2 * bits)) | (g << bits) | b;
        let decided = match self.target {
            Target::Ansi256 => DECIDED_256.get(bin),
            Target::Ansi16 => DECIDED_16.get(bin),
        };
        if let Some(known) = decided.map(|d| d.load(Ordering::Relaxed))
            && known != 0
        {
            return known - 1;
        }
        // The middle of the bin stands for all of it.
        let half = 1u8 << (7 - bits);
        let centre = srgb.map(|c| (c & !(2 * half - 1)) | half);
        let decision = match self.target {
            Target::Ansi256 => family(centre),
            Target::Ansi16 => {
                let (a, b) = pair(centre);
                u16::from(a) * 16 + u16::from(b)
            }
        };
        // Racing threads compute the same decision: either store will do.
        if let Some(slot) = decided {
            slot.store(decision + 1, Ordering::Relaxed);
        }
        decision
    }
}

/// 256 colours: whether `srgb` goes on the grey ladder or in the cube.
fn family(srgb: [u8; 3]) -> u16 {
    let lab = color::to_oklab(crate::style::Rgb(srgb[0], srgb[1], srgb[2]));
    if lab.a.hypot(lab.b) < GREY_CHROMA {
        GREY
    } else {
        CUBE_FAMILY
    }
}

/// Pairs [`pair`] scores in OKLab, out of the closest in linear light.
const PAIR_CANDIDATES: usize = 6;

/// 16 colours: the two entries to mix (the same one twice when mixing does
/// not help). Every pair is ranked by how close its best mix comes in
/// linear light (cheap); the closest few are scored in OKLab, mix noise
/// included.
fn pair(srgb: [u8; 3]) -> (u8, u8) {
    let rgb = crate::style::Rgb(srgb[0], srgb[1], srgb[2]);
    let lab = color::to_oklab(rgb);
    let lin = srgb.map(color::srgb_to_linear);
    let pal = palette_lab();
    let ansi16 = &tables().ansi16;
    let nearest = color::to_16(rgb);
    // The closest pairs so far, closest first: (residual in linear light,
    // a, b, how much of b).
    let mut ranked = [(f32::INFINITY, 0u8, 0u8, 0.0f32); PAIR_CANDIDATES];
    for a in 0..16u8 {
        for b in a + 1..16u8 {
            let (pa, pb) = (ansi16[usize::from(a)], ansi16[usize::from(b)]);
            let t = mix_fraction(lin, pa, pb);
            let residual: f32 = (0..3)
                .map(|i| {
                    let d = pa[i] + t * (pb[i] - pa[i]) - lin[i];
                    d * d
                })
                .sum();
            if let Some(at) = ranked.iter().position(|r| residual < r.0) {
                ranked.copy_within(at..PAIR_CANDIDATES - 1, at + 1);
                if let Some(slot) = ranked.get_mut(at) {
                    *slot = (residual, a, b, t);
                }
            }
        }
    }
    let mut best = (lab.dist2(pal[usize::from(nearest)]), nearest, nearest);
    for &(residual, a, b, t) in &ranked {
        if !residual.is_finite() {
            continue;
        }
        let (pa, pb) = (ansi16[usize::from(a)], ansi16[usize::from(b)]);
        let mixed: [f32; 3] = std::array::from_fn(|i| pa[i] + t * (pb[i] - pa[i]));
        let error = color::linear_to_oklab(mixed[0], mixed[1], mixed[2]).dist2(lab)
            + MIX_NOISE * t * (1.0 - t) * pal[usize::from(a)].dist2(pal[usize::from(b)]);
        if error < best.0 {
            best = (error, a, b);
        }
    }
    (best.1, best.2)
}

/// How much of `b` (0 to 1) a mix of `a` and `b` takes to come closest to
/// `c`, all in linear light: `c` projected onto the segment from `a` to `b`.
fn mix_fraction(c: [f32; 3], a: [f32; 3], b: [f32; 3]) -> f32 {
    let d: [f32; 3] = std::array::from_fn(|i| b[i] - a[i]);
    let len2: f32 = d.iter().map(|v| v * v).sum();
    if len2 <= 0.0 {
        return 0.0;
    }
    let along: f32 = (0..3).map(|i| (c[i] - a[i]) * d[i]).sum();
    (along / len2).clamp(0.0, 1.0)
}

/// The entries of an ascending ladder `(linear value, palette index)` on
/// either side of `v`, and how far `v` is from the lower one to the upper.
fn bracket(ladder: &[(f32, u8)], v: f32) -> (u8, u8, f32) {
    let above = ladder.partition_point(|&(level, _)| level <= v);
    let (Some(&(lo, lo_idx)), Some(&(hi, hi_idx))) = (
        ladder.get(above.saturating_sub(1)),
        ladder.get(above.min(ladder.len().saturating_sub(1))),
    ) else {
        return (0, 0, 0.0);
    };
    if hi <= lo {
        return (lo_idx, hi_idx, 0.0);
    }
    (lo_idx, hi_idx, ((v - lo) / (hi - lo)).clamp(0.0, 1.0))
}

/// Tables shared by every raster.
struct Tables {
    /// For each sRGB byte: the cube level at or below it (0–4) and how far
    /// it is towards the next one, in linear light.
    cube_steps: [(u8, f32); 256],
    /// The 30 greys of the 256-colour palette (the ramp and the cube's
    /// greys), ascending: linear value and palette index.
    greys: Vec<(f32, u8)>,
    /// The 16 colours in linear light (xterm defaults).
    ansi16: [[f32; 3]; 16],
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let levels = CUBE.map(color::srgb_to_linear);
        let cube_steps = std::array::from_fn(|s| {
            let v = color::srgb_to_linear(s as u8);
            let lo = levels
                .iter()
                .rposition(|&l| l <= v)
                .unwrap_or(0)
                .min(levels.len() - 2);
            let (a, b) = (levels[lo], levels[lo + 1]);
            (lo as u8, ((v - a) / (b - a)).clamp(0.0, 1.0))
        });
        let mut greys: Vec<(f32, u8)> = (0..6u8)
            .map(|i| CUBE_BASE + 43 * i)
            .chain(232..=255)
            .map(|idx| (color::srgb_to_linear(xterm_rgb(idx).0), idx))
            .collect();
        greys.sort_by(|a, b| a.0.total_cmp(&b.0));
        let ansi16 = std::array::from_fn(|i| {
            let c = xterm_rgb(i as u8);
            [c.0, c.1, c.2].map(color::srgb_to_linear)
        });
        Tables {
            cube_steps,
            greys,
            ansi16,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Rgb;

    fn lin(srgb: [u8; 3]) -> [f32; 3] {
        srgb.map(color::srgb_to_linear)
    }

    /// The indices of an 8×8 block of one colour.
    fn block(dither: &Dither, srgb: [u8; 3], at: (usize, usize)) -> Vec<u8> {
        (0..64)
            .map(|i| dither.index(srgb, lin(srgb), at.0 + i % 8, at.1 + i / 8))
            .collect()
    }

    /// The linear-light mean of `indices`, back in sRGB.
    fn mean(indices: &[u8]) -> Rgb {
        let mut sum = [0.0f32; 3];
        for &i in indices {
            let c = xterm_rgb(i);
            for (acc, v) in sum.iter_mut().zip([c.0, c.1, c.2]) {
                *acc += color::srgb_to_linear(v);
            }
        }
        let n = indices.len() as f32;
        let [r, g, b] = sum.map(|v| color::linear_to_srgb(v / n));
        Rgb(r, g, b)
    }

    #[test]
    fn nothing_to_dither_in_24_bit_or_without_colour() {
        for depth in [ColorDepth::TrueColor, ColorDepth::Mono, ColorDepth::None] {
            assert!(Dither::new(depth).is_none(), "{depth:?}");
        }
    }

    #[test]
    fn the_bayer_matrix_has_every_level_once() {
        let mut seen: Vec<u8> = BAYER.iter().flatten().copied().collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..64).collect::<Vec<u8>>());
        assert!((threshold(0, 0) - 0.5 / 64.0).abs() < 1e-6);
        assert_eq!(threshold(3, 5), threshold(11, 13), "period 8");
    }

    #[test]
    fn palette_colours_stay_as_they_are() {
        let d256 = Dither::new(ColorDepth::Ansi256).unwrap();
        for idx in 16..=255u8 {
            let c = xterm_rgb(idx);
            let srgb = [c.0, c.1, c.2];
            assert!(
                block(&d256, srgb, (0, 0)).iter().all(|&i| i == idx),
                "{idx}"
            );
        }
        let d16 = Dither::new(ColorDepth::Ansi16).unwrap();
        for idx in 0..16u8 {
            let c = xterm_rgb(idx);
            let srgb = [c.0, c.1, c.2];
            assert!(block(&d16, srgb, (5, 3)).iter().all(|&i| i == idx), "{idx}");
        }
    }

    #[test]
    fn mixes_reproduce_the_colour_in_linear_light() {
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16] {
            let d = Dither::new(depth).unwrap();
            let range = if depth == ColorDepth::Ansi256 {
                16..=255
            } else {
                0..=15
            };
            for srgb in [[200, 60, 30], [30, 30, 30], [120, 160, 220], [10, 0, 60]] {
                let picked = block(&d, srgb, (0, 0));
                assert!(
                    picked.iter().all(|i| range.contains(i)),
                    "{depth:?} {picked:?}"
                );
                let mean = mean(&picked);
                let error = color::to_oklab(mean)
                    .dist2(color::to_oklab(Rgb(srgb[0], srgb[1], srgb[2])))
                    .sqrt();
                let nearest = if depth == ColorDepth::Ansi256 {
                    xterm_rgb(color::to_256(Rgb(srgb[0], srgb[1], srgb[2])))
                } else {
                    xterm_rgb(color::to_16(Rgb(srgb[0], srgb[1], srgb[2])))
                };
                let snapped = color::to_oklab(nearest)
                    .dist2(color::to_oklab(Rgb(srgb[0], srgb[1], srgb[2])))
                    .sqrt();
                // Never further than the nearest entry; at 256 colours, close.
                let close = depth == ColorDepth::Ansi256 && error < 0.01;
                assert!(
                    close || (depth == ColorDepth::Ansi16 && error <= snapped + 1e-4),
                    "{depth:?} {srgb:?}: mean {mean:?} is {error} away, nearest {snapped}"
                );
            }
        }
    }

    #[test]
    fn greys_use_the_fine_ladder() {
        let d = Dither::new(ColorDepth::Ansi256).unwrap();
        // 100 lies between ramp greys 98 (index 241) and 108 (242).
        let picked = block(&d, [100, 100, 100], (0, 0));
        assert!(picked.iter().all(|&i| i == 241 || i == 242), "{picked:?}");
        assert!(picked.contains(&241) && picked.contains(&242));
        // A saturated dark blue keeps its hue: black and the cube's darkest blue.
        let picked = block(&d, [0, 0, 40], (0, 0));
        assert!(picked.iter().all(|&i| i == 16 || i == 17), "{picked:?}");
        assert!(picked.contains(&17));
    }

    #[test]
    fn the_pattern_belongs_to_the_position() {
        let d = Dither::new(ColorDepth::Ansi256).unwrap();
        let srgb = [150, 90, 200];
        let a = block(&d, srgb, (0, 0));
        let b = block(&d, srgb, (16, 8));
        assert_eq!(a, b, "the same 8×8 phase");
        let c = block(&d, srgb, (1, 0));
        assert_ne!(a, c, "a different phase");
        let fresh = Dither::new(ColorDepth::Ansi256).unwrap();
        assert_eq!(a, block(&fresh, srgb, (0, 0)), "deterministic");
    }

    #[test]
    fn brackets() {
        let ladder = [(0.0, 1), (0.5, 2), (1.0, 3)];
        assert_eq!(bracket(&ladder, 0.25), (1, 2, 0.5));
        assert_eq!(bracket(&ladder, 0.0), (1, 2, 0.0));
        assert_eq!(bracket(&ladder, 1.0), (3, 3, 0.0));
        assert_eq!(bracket(&[], 0.5), (0, 0, 0.0));
        assert_eq!(mix_fraction([0.5; 3], [0.0; 3], [1.0; 3]), 0.5);
        assert_eq!(mix_fraction([2.0; 3], [0.0; 3], [1.0; 3]), 1.0);
        assert_eq!(mix_fraction([0.3; 3], [0.2; 3], [0.2; 3]), 0.0);
        let t = tables();
        assert_eq!(t.greys.len(), 30);
        assert_eq!(t.cube_steps[0], (0, 0.0));
        assert_eq!(t.cube_steps[255], (4, 1.0));
        assert_eq!(t.cube_steps[95], (1, 0.0));
    }
}
