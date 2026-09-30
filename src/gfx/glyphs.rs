//! Block glyphs for text-mode images: sub-pixel grids and mask → glyph
//! lookup.
//!
//! A glyph set splits each cell into `sx × sy` sub-pixels, numbered row by
//! row from the top-left. Bit `i` of a *mask* is set when sub-pixel `i` is
//! inked (drawn in the foreground colour); the other sub-pixels show the
//! background colour. Every set has a glyph for every mask:
//!
//! | Set | Grid | Glyphs |
//! |---|---|---|
//! | half | 1 × 2 | space `▀` `▄` `█` |
//! | quadrant | 2 × 2 | U+2596–259F plus `▀ ▄ ▌ ▐ █` |
//! | sextant | 2 × 3 | U+1FB00–1FB3B plus `▌ ▐ █` |
//! | octant | 2 × 4 | U+1CD00–1CDE5 plus 26 older block elements |

use crate::term::BlockGlyphSet;

use super::r#gen::octants::OCTANTS;

/// Sub-pixel columns and rows per cell.
pub const fn grid(set: BlockGlyphSet) -> (u8, u8) {
    match set {
        BlockGlyphSet::Half => (1, 2),
        BlockGlyphSet::Quadrant => (2, 2),
        BlockGlyphSet::Sextant => (2, 3),
        BlockGlyphSet::Octant => (2, 4),
    }
}

/// Sub-pixels per cell.
pub const fn subpixels(set: BlockGlyphSet) -> u8 {
    let (sx, sy) = grid(set);
    sx * sy
}

/// The glyph of `set` that inks exactly the sub-pixels in `mask`. Bits
/// beyond the set's sub-pixel count are ignored.
pub fn glyph(set: BlockGlyphSet, mask: u8) -> char {
    match set {
        BlockGlyphSet::Half => half(mask),
        BlockGlyphSet::Quadrant => quadrant(mask),
        BlockGlyphSet::Sextant => sextant(mask),
        BlockGlyphSet::Octant => octant(mask),
    }
}

/// Half-block glyph: bit 0 is the top half, bit 1 the bottom half.
pub fn half(mask: u8) -> char {
    match mask & 0b11 {
        0 => ' ',
        1 => '▀',
        2 => '▄',
        _ => '█',
    }
}

/// Quadrant glyph: bits 0–3 are top-left, top-right, bottom-left and
/// bottom-right.
pub fn quadrant(mask: u8) -> char {
    const QUADRANTS: [char; 16] = [
        ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
    ];
    QUADRANTS[usize::from(mask & 0x0f)]
}

/// Sextant glyph: bits 0–5 run left to right, top to bottom. Masks 0, 21
/// (left column), 42 (right column) and 63 are space, `▌`, `▐` and `█`;
/// every other mask `m` is U+1FB00 + (m − 1) − (m > 21) − (m > 42), since
/// Unicode skips the four patterns that already existed.
pub fn sextant(mask: u8) -> char {
    let m = u32::from(mask & 0x3f);
    match m {
        0 => ' ',
        21 => '▌',
        42 => '▐',
        63 => '█',
        _ => {
            let code = 0x1FB00 + (m - 1) - u32::from(m > 21) - u32::from(m > 42);
            char::from_u32(code).unwrap_or('█')
        }
    }
}

/// Octant glyph: bits 0–7 run left to right, top to bottom (bit `n − 1` is
/// Unicode's octant `n`).
pub fn octant(mask: u8) -> char {
    OCTANTS[usize::from(mask)]
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::r#gen::sextants::SEXTANTS;
    use super::*;

    const ALL: [BlockGlyphSet; 4] = [
        BlockGlyphSet::Half,
        BlockGlyphSet::Quadrant,
        BlockGlyphSet::Sextant,
        BlockGlyphSet::Octant,
    ];

    fn masks(set: BlockGlyphSet) -> impl Iterator<Item = u8> {
        (0..1u16 << subpixels(set)).map(|m| m as u8)
    }

    /// Spread a mask of a `from`-row grid over a `to`-row grid (both two
    /// columns wide, `to` a multiple of `from`): each sub-pixel row becomes
    /// `to / from` rows.
    fn stretch(mask: u8, from_rows: u8, to_rows: u8) -> u8 {
        let k = to_rows / from_rows;
        let mut out = 0u8;
        for row in 0..from_rows {
            let pair = (mask >> (2 * row)) & 0b11;
            for r in 0..k {
                out |= pair << (2 * (row * k + r));
            }
        }
        out
    }

    #[test]
    fn grids() {
        assert_eq!(grid(BlockGlyphSet::Half), (1, 2));
        assert_eq!(grid(BlockGlyphSet::Octant), (2, 4));
        assert_eq!(subpixels(BlockGlyphSet::Sextant), 6);
    }

    #[test]
    fn every_set_has_distinct_glyphs() {
        for set in ALL {
            let glyphs: HashSet<char> = masks(set).map(|m| glyph(set, m)).collect();
            assert_eq!(glyphs.len(), 1 << subpixels(set), "{set:?}");
            assert_eq!(glyph(set, 0), ' ', "{set:?}");
            let full = ((1u16 << subpixels(set)) - 1) as u8;
            assert_eq!(glyph(set, full), '█', "{set:?}");
        }
    }

    #[test]
    fn sextant_formula_matches_the_generated_table() {
        for m in 0..64u8 {
            assert_eq!(sextant(m), SEXTANTS[usize::from(m)], "mask {m:#08b}");
        }
        assert_eq!(sextant(1), '\u{1FB00}');
        assert_eq!(sextant(62), '\u{1FB3B}');
        assert_eq!(sextant(0b010101), '▌');
        assert_eq!(sextant(0b101010), '▐');
    }

    #[test]
    fn octant_spot_checks() {
        // BLOCK OCTANT-3 is the first new character, OCTANT-2345678 the last.
        assert_eq!(octant(0b0000_0100), '\u{1CD00}');
        assert_eq!(octant(0b0000_0110), '\u{1CD01}');
        assert_eq!(octant(0b1111_1110), '\u{1CDE5}');
        // Older block elements fill the patterns they already draw.
        assert_eq!(octant(0x0F), '▀');
        assert_eq!(octant(0xF0), '▄');
        assert_eq!(octant(0x55), '▌');
        assert_eq!(octant(0xAA), '▐');
        assert_eq!(octant(0x03), '\u{1FB82}');
        assert_eq!(octant(0xC0), '▂');
        assert_eq!(octant(0x3F), '\u{1FB85}');
        assert_eq!(octant(0xFC), '▆');
        assert_eq!(octant(0x01), '\u{1CEA8}');
        assert_eq!(octant(0x02), '\u{1CEAB}');
        assert_eq!(octant(0x40), '\u{1CEA3}');
        assert_eq!(octant(0x80), '\u{1CEA0}');
        assert_eq!(octant(0x14), '\u{1FBE6}');
        assert_eq!(octant(0x28), '\u{1FBE7}');
        let new = (0..=255u8)
            .filter(|&m| ('\u{1CD00}'..='\u{1CDE5}').contains(&octant(m)))
            .count();
        assert_eq!(new, 230);
    }

    #[test]
    fn coarser_sets_agree_with_finer_ones() {
        // A quadrant pattern is the octant pattern with each row doubled.
        for m in masks(BlockGlyphSet::Quadrant) {
            assert_eq!(quadrant(m), octant(stretch(m, 2, 4)), "quadrant {m:#06b}");
        }
        // Half blocks are the quadrants with both columns set.
        for m in masks(BlockGlyphSet::Half) {
            let q = ((m & 1) * 0b0011) | ((m >> 1 & 1) * 0b1100);
            assert_eq!(half(m), quadrant(q), "half {m:#04b}");
        }
    }

    #[test]
    fn extra_bits_are_ignored() {
        assert_eq!(half(0b111), '█');
        assert_eq!(quadrant(0xf1), '▘');
        assert_eq!(sextant(0b1100_0001), '\u{1FB00}');
    }
}
