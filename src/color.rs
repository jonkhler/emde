//! Colour maths: sRGB/linear conversion, OKLab, mixing, luminance and
//! nearest-colour mapping to the xterm 256- and 16-colour palettes.

use std::sync::OnceLock;

use crate::style::Rgb;

/// sRGB-encoded byte → linear light in `0.0..=1.0`.
pub fn srgb_to_linear(c: u8) -> f32 {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0.0; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let x = i as f32 / 255.0;
            *v = if x <= 0.04045 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            };
        }
        t
    })[usize::from(c)]
}

/// Linear light (clamped to `0.0..=1.0`) → sRGB-encoded byte.
pub fn linear_to_srgb(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0 + 0.5) as u8
}

/// A colour in the OKLab perceptual space.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Oklab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

impl Oklab {
    /// Squared Euclidean distance (perceptual difference).
    pub fn dist2(self, o: Oklab) -> f32 {
        let (dl, da, db) = (self.l - o.l, self.a - o.a, self.b - o.b);
        dl * dl + da * da + db * db
    }
}

/// Convert linear-light RGB to OKLab.
pub fn linear_to_oklab(r: f32, g: f32, b: f32) -> Oklab {
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    Oklab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

/// Convert OKLab to linear-light RGB (not clamped).
pub fn oklab_to_linear(c: Oklab) -> (f32, f32, f32) {
    let l = c.l + 0.396_337_78 * c.a + 0.215_803_76 * c.b;
    let m = c.l - 0.105_561_346 * c.a - 0.063_854_17 * c.b;
    let s = c.l - 0.089_484_18 * c.a - 1.291_485_5 * c.b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    (
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    )
}

/// sRGB → OKLab.
pub fn to_oklab(c: Rgb) -> Oklab {
    linear_to_oklab(
        srgb_to_linear(c.0),
        srgb_to_linear(c.1),
        srgb_to_linear(c.2),
    )
}

/// OKLab → sRGB (out-of-gamut values are clamped per channel).
pub fn from_oklab(c: Oklab) -> Rgb {
    let (r, g, b) = oklab_to_linear(c);
    Rgb(linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b))
}

/// Interpolate between two colours in OKLab (`t = 0` gives `a`, `t = 1` gives `b`).
pub fn mix_oklab(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let (x, y) = (to_oklab(a), to_oklab(b));
    let t = t.clamp(0.0, 1.0);
    from_oklab(Oklab {
        l: x.l + (y.l - x.l) * t,
        a: x.a + (y.a - x.a) * t,
        b: x.b + (y.b - x.b) * t,
    })
}

/// Shift OKLab lightness by `dl` (e.g. `+0.045` lifts a dark background).
pub fn adjust_lightness(c: Rgb, dl: f32) -> Rgb {
    let mut lab = to_oklab(c);
    lab.l = (lab.l + dl).clamp(0.0, 1.0);
    from_oklab(lab)
}

/// WCAG relative luminance in `0.0..=1.0`.
pub fn luminance(c: Rgb) -> f32 {
    0.2126 * srgb_to_linear(c.0) + 0.7152 * srgb_to_linear(c.1) + 0.0722 * srgb_to_linear(c.2)
}

/// WCAG contrast ratio between two colours (1.0–21.0).
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Whether a background colour should get the dark variant of a theme.
pub fn is_dark(bg: Rgb) -> bool {
    luminance(bg) < 0.18
}

/// The six channel levels of the xterm 6×6×6 colour cube (indices 16–231).
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// xterm's default colours for indices 0–15.
const ANSI16: [Rgb; 16] = [
    Rgb(0, 0, 0),
    Rgb(205, 0, 0),
    Rgb(0, 205, 0),
    Rgb(205, 205, 0),
    Rgb(0, 0, 238),
    Rgb(205, 0, 205),
    Rgb(0, 205, 205),
    Rgb(229, 229, 229),
    Rgb(127, 127, 127),
    Rgb(255, 0, 0),
    Rgb(0, 255, 0),
    Rgb(255, 255, 0),
    Rgb(92, 92, 255),
    Rgb(255, 0, 255),
    Rgb(0, 255, 255),
    Rgb(255, 255, 255),
];

/// The RGB value of an xterm palette index (xterm defaults for 0–15).
pub fn xterm_rgb(idx: u8) -> Rgb {
    match idx {
        0..=15 => ANSI16[usize::from(idx)],
        16..=231 => {
            let i = idx - 16;
            Rgb(
                CUBE[usize::from(i / 36)],
                CUBE[usize::from(i / 6 % 6)],
                CUBE[usize::from(i % 6)],
            )
        }
        232..=255 => {
            let v = 8 + 10 * (idx - 232);
            Rgb(v, v, v)
        }
    }
}

fn palette_lab() -> &'static [Oklab; 256] {
    static LAB: OnceLock<[Oklab; 256]> = OnceLock::new();
    LAB.get_or_init(|| {
        let mut t = [Oklab::default(); 256];
        for (i, v) in t.iter_mut().enumerate() {
            *v = to_oklab(xterm_rgb(i as u8));
        }
        t
    })
}

/// Map a colour to the nearest xterm 256-colour index in OKLab. Entries
/// 0–15 are skipped because terminal themes redefine them. Exact (brute force
/// over 240 precomputed entries, well under a microsecond per call); callers
/// mapping many pixels should memoise.
pub fn to_256(c: Rgb) -> u8 {
    let lab = to_oklab(c);
    let pal = palette_lab();
    let mut best = (f32::MAX, 16u8);
    for idx in 16..=255u8 {
        let d = lab.dist2(pal[usize::from(idx)]);
        if d < best.0 {
            best = (d, idx);
        }
    }
    best.1
}

/// Map a colour to the nearest of the 16 ANSI colours (xterm defaults) in OKLab.
pub fn to_16(c: Rgb) -> u8 {
    let lab = to_oklab(c);
    let pal = palette_lab();
    (0..16u8)
        .min_by(|&a, &b| {
            lab.dist2(pal[usize::from(a)])
                .total_cmp(&lab.dist2(pal[usize::from(b)]))
        })
        .unwrap_or(7)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute_256(c: Rgb) -> u8 {
        let lab = to_oklab(c);
        (16..=255u8)
            .min_by(|&a, &b| {
                lab.dist2(to_oklab(xterm_rgb(a)))
                    .total_cmp(&lab.dist2(to_oklab(xterm_rgb(b))))
            })
            .unwrap()
    }

    #[test]
    fn srgb_roundtrip() {
        for v in 0..=255u8 {
            assert_eq!(linear_to_srgb(srgb_to_linear(v)), v);
        }
    }

    #[test]
    fn oklab_roundtrip() {
        for c in [
            Rgb(0, 0, 0),
            Rgb(255, 255, 255),
            Rgb(0x89, 0xb4, 0xfa),
            Rgb(12, 200, 99),
        ] {
            let back = from_oklab(to_oklab(c));
            for (x, y) in [(c.0, back.0), (c.1, back.1), (c.2, back.2)] {
                assert!(x.abs_diff(y) <= 1, "{c:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn palette_values() {
        assert_eq!(xterm_rgb(16), Rgb(0, 0, 0));
        assert_eq!(xterm_rgb(231), Rgb(255, 255, 255));
        assert_eq!(xterm_rgb(196), Rgb(255, 0, 0));
        assert_eq!(xterm_rgb(232), Rgb(8, 8, 8));
        assert_eq!(xterm_rgb(255), Rgb(238, 238, 238));
    }

    #[test]
    fn to_256_matches_brute_force() {
        for idx in 16..=255u8 {
            let c = xterm_rgb(idx);
            assert_eq!(xterm_rgb(to_256(c)), c, "index {idx}");
        }
        for r in (0..=255u8).step_by(15) {
            for g in (0..=255u8).step_by(15) {
                for b in (0..=255u8).step_by(15) {
                    let c = Rgb(r, g, b);
                    assert_eq!(to_256(c), brute_256(c), "{c:?}");
                }
            }
        }
    }

    #[test]
    fn to_16_basics() {
        assert_eq!(to_16(Rgb(0, 0, 0)), 0);
        assert_eq!(to_16(Rgb(255, 255, 255)), 15);
        assert_eq!(to_16(Rgb(250, 10, 10)), 9);
    }

    #[test]
    fn contrast_and_darkness() {
        assert!((contrast(Rgb(0, 0, 0), Rgb(255, 255, 255)) - 21.0).abs() < 0.01);
        assert!(is_dark(Rgb(0x1e, 0x1e, 0x2e)));
        assert!(!is_dark(Rgb(0xef, 0xf1, 0xf5)));
    }

    #[test]
    fn mixing_endpoints() {
        let (a, b) = (Rgb(0x89, 0xb4, 0xfa), Rgb(0xcb, 0xa6, 0xf7));
        assert_eq!(mix_oklab(a, b, 0.0), a);
        assert_eq!(mix_oklab(a, b, 1.0), b);
    }
}
