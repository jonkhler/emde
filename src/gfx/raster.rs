//! Text-mode images: pixels as coloured block glyphs.
//!
//! [`rasterize`] resamples the image to the glyph set's sub-pixel grid
//! (`cols × sx` by `rows × sy`, width and height scaled independently, so
//! non-square sub-pixels keep the picture's proportions), composites it and
//! turns each cell into one glyph with a foreground and a background colour:
//!
//! * **Half blocks** are exact: `▀` with the top colour as foreground and
//!   the bottom one as background; `▀` or `▄` over the terminal's own
//!   background when one half is transparent.
//! * **Quadrants, sextants and octants** hold two colours per cell. The
//!   sub-pixels are sorted along the colour channel with the largest range
//!   and split where the summed squared error of the two groups is smallest
//!   (prefix sums make every split O(1)). Each group's colour is its average
//!   in linear light. The foreground is always the smaller group (on a tie,
//!   the one holding the top-left sub-pixel), so a fully covered cell never
//!   inks more than half of it and a flat cell is a plain space. Where the
//!   image leaves sub-pixels uncovered, the glyph inks exactly the covered
//!   ones, in their average colour.
//!
//! Fully transparent sub-pixels become [`Color::Default`], letting the
//! terminal's background show. With a known background, every pixel is
//! blended onto it (on sRGB values, as browsers do) *before* the
//! linear-light average, so sub-pixels that are only partly covered by the
//! image mix like light, and only sub-pixels the image does not cover at all
//! stay transparent. Without a known background, sub-pixels at least half
//! covered keep the image's own colour and the rest are transparent. A cell
//! whose two colours end up equal after mapping to the terminal's palette
//! becomes a space on that background.
//!
//! At 256 and 16 colours, with `dither` on (`images.dither`), the
//! sub-pixels are dithered to the palette before the cells are made
//! (ordered, with an 8×8 Bayer matrix anchored to the image, in the
//! private `gfx::dither` module), so gradients keep their steps fine and photographs
//! their shading; the two-colour split then works on palette colours. At
//! 24-bit colour nothing is dithered.
//!
//! The output is deterministic: the same input always gives the same cells.

use std::collections::HashMap;

use super::Rgba;
use super::dither::Dither;
use super::glyphs;
use super::resample::{self, Blend, LinearImage};
use crate::color;
use crate::style::{Color, Rgb};
use crate::term::{BlockGlyphSet, ColorDepth};

/// One character cell of a rasterised image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RasterCell {
    /// The block glyph (a space for flat or empty cells).
    pub ch: char,
    /// Colour of the inked part of the glyph (`Default` for spaces).
    pub fg: Color,
    /// Colour of the rest of the cell; `Default` shows the terminal's own
    /// background.
    pub bg: Color,
}

impl RasterCell {
    /// An empty cell: a space in the terminal's default colours.
    pub const BLANK: RasterCell = RasterCell {
        ch: ' ',
        fg: Color::Default,
        bg: Color::Default,
    };
}

/// A rasterised image: `rows` rows of `cols` cells each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Raster {
    /// Width in cells.
    pub cols: u16,
    /// Height in cells.
    pub rows: u16,
    /// The cells, top row first; always `rows` rows of `cols` cells.
    pub cells: Vec<Vec<RasterCell>>,
}

impl Raster {
    /// A raster of blank cells.
    pub fn blank(cols: u16, rows: u16) -> Raster {
        Raster {
            cols,
            rows,
            cells: vec![vec![RasterCell::BLANK; usize::from(cols)]; usize::from(rows)],
        }
    }

    /// The cells of row `row`, if it exists.
    pub fn row(&self, row: u16) -> Option<&[RasterCell]> {
        self.cells.get(usize::from(row)).map(Vec::as_slice)
    }

    /// The glyphs only, one line per row (for tests and debugging).
    pub fn glyphs(&self) -> String {
        let mut out = String::new();
        for row in &self.cells {
            out.extend(row.iter().map(|c| c.ch));
            out.push('\n');
        }
        out
    }
}

/// Render `img` into `cols × rows` cells of `glyphs`.
///
/// `background` is the terminal's background colour, if known, for
/// blending partly transparent pixels. `depth` selects the colour encoding:
/// 24-bit, the xterm 256-colour palette, the 16 ANSI colours, or, without
/// colours, a raster of blank cells. With `dither`, 256 and 16 colours are
/// dithered; without, each colour takes its nearest palette entry. An
/// invalid or empty image gives blank cells.
pub fn rasterize(
    img: &Rgba,
    cols: u16,
    rows: u16,
    glyphs: BlockGlyphSet,
    background: Option<Rgb>,
    depth: ColorDepth,
    dither: bool,
) -> Raster {
    if depth < ColorDepth::Ansi16 || cols == 0 || rows == 0 || img.is_empty() {
        return Raster::blank(cols, rows);
    }
    let (sx, sy) = glyphs::grid(glyphs);
    let (sx, sy) = (usize::from(sx), usize::from(sy));
    let blend = match background {
        Some(Rgb(r, g, b)) => Blend::Onto([r, g, b]),
        None => Blend::Premultiplied,
    };
    let grid = resample::resample(img, usize::from(cols) * sx, usize::from(rows) * sy, blend);
    let mut subs = composite(&grid, background.is_some());
    let width = grid.width;
    if dither && let Some(d) = Dither::new(depth) {
        dither_subs(&mut subs, width, &d);
    }
    let mut palette = Palette::new(depth);
    let mut cell_subs = Vec::with_capacity(sx * sy);
    let cells = (0..usize::from(rows))
        .map(|row| {
            (0..usize::from(cols))
                .map(|col| {
                    cell_subs.clear();
                    for dy in 0..sy {
                        let start = (row * sy + dy) * width + col * sx;
                        let line = subs.get(start..start + sx).unwrap_or_default();
                        cell_subs.extend_from_slice(line);
                    }
                    let cell = match glyphs {
                        BlockGlyphSet::Half => half_cell(&cell_subs),
                        _ => split_cell(glyphs, &cell_subs),
                    };
                    palette.cell(cell)
                })
                .collect()
        })
        .collect();
    Raster { cols, rows, cells }
}

/// A sub-pixel after compositing.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sub {
    /// Transparent: the terminal's background shows.
    Clear,
    /// An opaque colour, as sRGB bytes and in linear light.
    Solid { srgb: [u8; 3], lin: [f32; 3] },
}

impl Sub {
    fn solid(srgb: [u8; 3]) -> Sub {
        Sub::Solid {
            srgb,
            lin: srgb.map(resample::srgb_to_linear),
        }
    }
}

/// Coverage below which a sub-pixel counts as fully transparent when the
/// background is known (it would round to alpha 0 in 8 bits).
const CLEAR_ALPHA: f32 = 0.5 / 255.0;

/// Coverage from which a sub-pixel counts as opaque when the background is
/// unknown.
const OPAQUE_ALPHA: f32 = 0.5;

/// Turn the resampled grid into sub-pixels. With a known background the
/// grid was composited onto it before averaging ([`Blend::Onto`]), so any
/// covered sub-pixel is solid; otherwise the grid is premultiplied and
/// coverage is thresholded.
fn composite(grid: &LinearImage, composited: bool) -> Vec<Sub> {
    grid.pixels
        .iter()
        .map(|&px| {
            let [r, g, b, a] = px;
            if composited && a >= CLEAR_ALPHA {
                Sub::solid([r, g, b].map(resample::linear_to_srgb))
            } else if !composited && a >= OPAQUE_ALPHA {
                let [r, g, b, _] = LinearImage::to_srgba(px);
                Sub::solid([r, g, b])
            } else {
                Sub::Clear
            }
        })
        .collect()
}

/// Replace every opaque sub-pixel of a grid `width` wide with the palette
/// colour `dither` picks for it at its place in the image.
fn dither_subs(subs: &mut [Sub], width: usize, dither: &Dither) {
    let width = width.max(1);
    for (i, sub) in subs.iter_mut().enumerate() {
        if let Sub::Solid { srgb, lin } = *sub {
            let Rgb(r, g, b) = color::xterm_rgb(dither.index(srgb, lin, i % width, i / width));
            *sub = Sub::solid([r, g, b]);
        }
    }
}

/// A cell before palette mapping: colours are sRGB, `None` is transparent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrueCell {
    ch: char,
    fg: Option<Rgb>,
    bg: Option<Rgb>,
}

fn rgb(srgb: [u8; 3]) -> Rgb {
    Rgb(srgb[0], srgb[1], srgb[2])
}

/// Half blocks: exact top and bottom colours.
fn half_cell(subs: &[Sub]) -> TrueCell {
    let colour = |s: Option<&Sub>| match s {
        Some(Sub::Solid { srgb, .. }) => Some(rgb(*srgb)),
        _ => None,
    };
    match (colour(subs.first()), colour(subs.get(1))) {
        (None, None) => TrueCell {
            ch: ' ',
            fg: None,
            bg: None,
        },
        (None, bottom) => TrueCell {
            ch: '▄',
            fg: bottom,
            bg: None,
        },
        (top, bottom) => TrueCell {
            ch: '▀',
            fg: top,
            bg: bottom,
        },
    }
}

/// The linear-light average of the sub-pixels selected by `mask`, as sRGB.
fn mean(subs: &[Sub], mask: u8) -> Option<Rgb> {
    let mut sum = [0.0f32; 3];
    let mut n = 0u8;
    for (i, s) in subs.iter().enumerate() {
        if let Sub::Solid { lin, .. } = s
            && mask >> i & 1 == 1
        {
            for (acc, v) in sum.iter_mut().zip(lin) {
                *acc += v;
            }
            n += 1;
        }
    }
    (n > 0).then(|| rgb(sum.map(|v| resample::linear_to_srgb(v / f32::from(n)))))
}

/// Most sub-pixels a cell can have (masks are 8 bits wide).
const MAX_SUBS: usize = 8;

/// Quadrants, sextants and octants: the best two-colour split of a cell.
fn split_cell(set: BlockGlyphSet, subs: &[Sub]) -> TrueCell {
    let subs = subs.get(..MAX_SUBS).unwrap_or(subs);
    let n = subs.len();
    let full = ((1u16 << n) - 1) as u8;
    let solid = subs
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, Sub::Solid { .. }))
        .fold(0u8, |m, (i, _)| m | 1 << i);
    if solid == 0 {
        return TrueCell {
            ch: ' ',
            fg: None,
            bg: None,
        };
    }
    if solid != full {
        // Transparent sub-pixels must stay background, so all opaque ones
        // share the foreground colour.
        return TrueCell {
            ch: glyphs::glyph(set, solid),
            fg: mean(subs, solid),
            bg: None,
        };
    }
    let mut colours = [[0u8; 3]; MAX_SUBS];
    for (c, s) in colours.iter_mut().zip(subs) {
        if let Sub::Solid { srgb, .. } = s {
            *c = *srgb;
        }
    }
    let Some(mut mask) = best_split(colours.get(..n).unwrap_or_default()) else {
        return TrueCell {
            ch: ' ',
            fg: None,
            bg: mean(subs, full),
        };
    };
    // Ink the smaller group; on a tie, the one holding the top-left.
    let ones = mask.count_ones() as usize;
    if 2 * ones > n || (2 * ones == n && mask & 1 == 0) {
        mask = !mask & full;
    }
    TrueCell {
        ch: glyphs::glyph(set, mask),
        fg: mean(subs, mask),
        bg: mean(subs, !mask & full),
    }
}

/// The mask of the lower group of the best split of (up to 8) colours, or
/// `None` when all colours are equal. The colours are ordered along the
/// channel with the largest range (ties: red, green, blue), and the split
/// point minimises the total squared error of the two groups.
fn best_split(colours: &[[u8; 3]]) -> Option<u8> {
    let colours = colours.get(..MAX_SUBS).unwrap_or(colours);
    let n = colours.len();
    let ranges: [u8; 3] = std::array::from_fn(|ch| {
        let (lo, hi) = colours
            .iter()
            .fold((u8::MAX, 0), |(lo, hi), c| (lo.min(c[ch]), hi.max(c[ch])));
        hi.saturating_sub(lo)
    });
    let mut axis = 0;
    for ch in 1..3 {
        if ranges[ch] > ranges[axis] {
            axis = ch;
        }
    }
    if ranges[axis] == 0 {
        return None;
    }
    // Stable insertion sort of the indices along the axis.
    let mut order: [usize; MAX_SUBS] = std::array::from_fn(|i| i);
    let order = order.get_mut(..n).unwrap_or_default();
    for i in 1..n {
        let mut j = i;
        while j > 0 && colours[order[j - 1]][axis] > colours[order[j]][axis] {
            order.swap(j - 1, j);
            j -= 1;
        }
    }
    // Prefix sums of the sorted colours. With the sum of squares fixed,
    // minimising the groups' squared error means maximising
    // |ΣA|²/|A| + |ΣB|²/|B|.
    let mut prefix = [[0.0f64; 3]; MAX_SUBS];
    let mut acc = [0.0f64; 3];
    for (p, &i) in prefix.iter_mut().zip(order.iter()) {
        for (a, &c) in acc.iter_mut().zip(&colours[i]) {
            *a += f64::from(c);
        }
        *p = acc;
    }
    let norm2 = |v: [f64; 3]| v.iter().map(|x| x * x).sum::<f64>();
    let mut best = (f64::MIN, 1);
    for (k, a) in (1..n).zip(&prefix) {
        let b = [acc[0] - a[0], acc[1] - a[1], acc[2] - a[2]];
        let score = norm2(*a) / k as f64 + norm2(b) / (n - k) as f64;
        if score > best.0 {
            best = (score, k);
        }
    }
    let lower = order.get(..best.1).unwrap_or_default();
    Some(lower.iter().fold(0u8, |m, &i| m | 1 << i))
}

/// Maps sRGB colours to the terminal's colour depth, memoising the
/// palette searches.
struct Palette {
    depth: ColorDepth,
    memo: HashMap<Rgb, Color>,
}

impl Palette {
    fn new(depth: ColorDepth) -> Palette {
        Palette {
            depth,
            memo: HashMap::new(),
        }
    }

    fn color(&mut self, c: Option<Rgb>) -> Color {
        let Some(c) = c else {
            return Color::Default;
        };
        match self.depth {
            ColorDepth::TrueColor => Color::Rgb(c),
            ColorDepth::Ansi256 => *self
                .memo
                .entry(c)
                .or_insert_with(|| Color::Indexed(color::to_256(c))),
            ColorDepth::Ansi16 => *self
                .memo
                .entry(c)
                .or_insert_with(|| Color::Ansi(color::to_16(c))),
            ColorDepth::Mono | ColorDepth::None => Color::Default,
        }
    }

    /// The final cell: mapped colours, and a plain space (with the default
    /// foreground) when the glyph is a space or would be invisible because
    /// both colours are equal.
    fn cell(&mut self, cell: TrueCell) -> RasterCell {
        let fg = self.color(cell.fg);
        let bg = self.color(cell.bg);
        if cell.ch == ' ' || fg == bg {
            RasterCell {
                ch: ' ',
                fg: Color::Default,
                bg,
            }
        } else {
            RasterCell {
                ch: cell.ch,
                fg,
                bg,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];

    fn image(width: u32, height: u32, pixels: &[[u8; 4]]) -> Rgba {
        Rgba::new(width, height, pixels.concat()).unwrap()
    }

    fn rgb(c: [u8; 4]) -> Color {
        Color::Rgb(Rgb(c[0], c[1], c[2]))
    }

    fn cell(ch: char, fg: Color, bg: Color) -> RasterCell {
        RasterCell { ch, fg, bg }
    }

    fn true_color(img: &Rgba, cols: u16, rows: u16, set: BlockGlyphSet) -> Raster {
        rasterize(img, cols, rows, set, None, ColorDepth::TrueColor, true)
    }

    #[test]
    fn half_blocks_are_exact() {
        let img = image(2, 2, &[RED, WHITE, BLUE, WHITE]);
        let r = true_color(&img, 2, 1, BlockGlyphSet::Half);
        assert_eq!(r.cells[0][0], cell('▀', rgb(RED), rgb(BLUE)));
        // Equal halves are a flat space.
        assert_eq!(r.cells[0][1], cell(' ', Color::Default, rgb(WHITE)));
    }

    #[test]
    fn half_blocks_with_transparency() {
        let img = image(3, 2, &[RED, CLEAR, CLEAR, CLEAR, BLUE, CLEAR]);
        let r = true_color(&img, 3, 1, BlockGlyphSet::Half);
        assert_eq!(r.cells[0][0], cell('▀', rgb(RED), Color::Default));
        assert_eq!(r.cells[0][1], cell('▄', rgb(BLUE), Color::Default));
        assert_eq!(r.cells[0][2], RasterCell::BLANK);
    }

    #[test]
    fn two_by_two_quadrants() {
        // One odd sub-pixel inks a single quadrant, whichever corner it is in.
        let corners = [('▘', 0), ('▝', 1), ('▖', 2), ('▗', 3)];
        for (glyph, odd) in corners {
            let mut px = [WHITE; 4];
            px[odd] = RED;
            let r = true_color(&image(2, 2, &px), 1, 1, BlockGlyphSet::Quadrant);
            assert_eq!(
                r.cells[0][0],
                cell(glyph, rgb(RED), rgb(WHITE)),
                "corner {odd}"
            );
            // Three odd ones: the white corner is now the minority.
            let inverted = px.map(|p| if p == RED { WHITE } else { RED });
            let r = true_color(&image(2, 2, &inverted), 1, 1, BlockGlyphSet::Quadrant);
            assert_eq!(
                r.cells[0][0],
                cell(glyph, rgb(WHITE), rgb(RED)),
                "corner {odd}"
            );
        }
        // Halves tie: the group holding the top-left is inked.
        let left = image(2, 2, &[BLUE, RED, BLUE, RED]);
        let r = true_color(&left, 1, 1, BlockGlyphSet::Quadrant);
        assert_eq!(r.cells[0][0], cell('▌', rgb(BLUE), rgb(RED)));
        let top = image(2, 2, &[RED, RED, BLUE, BLUE]);
        let r = true_color(&top, 1, 1, BlockGlyphSet::Quadrant);
        assert_eq!(r.cells[0][0], cell('▀', rgb(RED), rgb(BLUE)));
        let diagonal = image(2, 2, &[RED, BLUE, BLUE, RED]);
        let r = true_color(&diagonal, 1, 1, BlockGlyphSet::Quadrant);
        assert_eq!(r.cells[0][0], cell('▚', rgb(RED), rgb(BLUE)));
    }

    #[test]
    fn two_by_four_octants() {
        // Column pattern: left column black except the bottom sub-pixel.
        let px = [BLACK, WHITE, BLACK, WHITE, BLACK, WHITE, WHITE, WHITE];
        let img = image(2, 4, &px);
        let r = true_color(&img, 1, 1, BlockGlyphSet::Octant);
        // Octants 1, 3, 5 are black: BLOCK OCTANT-135 inked in black.
        assert_eq!(r.cells[0][0].ch, glyphs::octant(0b0001_0101));
        assert_eq!(r.cells[0][0].fg, rgb(BLACK));
        assert_eq!(r.cells[0][0].bg, rgb(WHITE));
        // As half blocks each cell averages a black and a white column: the
        // top cell is flat grey, the bottom one grey over white.
        let r = true_color(&img, 1, 2, BlockGlyphSet::Half);
        let grey = Color::Rgb(Rgb(188, 188, 188));
        assert_eq!(r.cells[0][0], cell(' ', Color::Default, grey));
        assert_eq!(r.cells[1][0], cell('▀', grey, rgb(WHITE)));
    }

    #[test]
    fn two_colour_split_on_the_widest_channel() {
        // Bright reds, dark reds and blues, one pair per sextant row. Red
        // has the widest range and the blues sit at its low end; splitting
        // them off leaves the least error.
        let dark = [100, 0, 0, 255];
        let blue = [0, 0, 250, 255];
        let img = image(2, 3, &[RED, RED, dark, dark, blue, blue]);
        let r = true_color(&img, 1, 1, BlockGlyphSet::Sextant);
        let c = r.cells[0][0];
        assert_eq!(c.ch, glyphs::sextant(0b11_0000), "bottom row inked");
        assert_eq!(c.fg, rgb(blue));
        // The reds average in linear light: 198, where sRGB codes give 178.
        assert_eq!(c.bg, Color::Rgb(Rgb(198, 0, 0)));
    }

    #[test]
    fn group_colours_average_light() {
        let subs = [
            Sub::solid([0, 0, 0]),
            Sub::solid([255, 255, 255]),
            Sub::Clear,
        ];
        assert_eq!(mean(&subs, 0b011), Some(Rgb(188, 188, 188)));
        assert_eq!(mean(&subs, 0b110), Some(Rgb(255, 255, 255)));
        assert_eq!(mean(&subs, 0b100), None);
    }

    #[test]
    fn transparent_subpixels_become_default() {
        let px = [RED, CLEAR, RED, CLEAR];
        let r = true_color(&image(2, 2, &px), 1, 1, BlockGlyphSet::Quadrant);
        assert_eq!(r.cells[0][0], cell('▌', rgb(RED), Color::Default));
        // Opaque colours share one foreground when transparency is present.
        let px = [RED, CLEAR, BLUE, CLEAR];
        let r = true_color(&image(2, 2, &px), 1, 1, BlockGlyphSet::Quadrant);
        assert_eq!(r.cells[0][0].ch, '▌');
        assert_eq!(r.cells[0][0].bg, Color::Default);
        let clear = true_color(&image(2, 2, &[CLEAR; 4]), 1, 1, BlockGlyphSet::Quadrant);
        assert_eq!(clear.cells[0][0], RasterCell::BLANK);
    }

    #[test]
    fn partial_alpha_blends_onto_a_known_background() {
        let half_red = [255, 0, 0, 128];
        let img = image(1, 2, &[half_red, CLEAR]);
        let r = rasterize(
            &img,
            1,
            1,
            BlockGlyphSet::Half,
            Some(Rgb(0, 0, 255)),
            ColorDepth::TrueColor,
            true,
        );
        assert_eq!(
            r.cells[0][0],
            cell('▀', Color::Rgb(Rgb(128, 0, 127)), Color::Default)
        );
        // Unknown background: at least half opaque keeps its own colour.
        let r = true_color(&img, 1, 1, BlockGlyphSet::Half);
        assert_eq!(r.cells[0][0], cell('▀', rgb(RED), Color::Default));
        let faint = image(1, 2, &[[255, 0, 0, 100], CLEAR]);
        let r = true_color(&faint, 1, 1, BlockGlyphSet::Half);
        assert_eq!(r.cells[0][0], RasterCell::BLANK);
    }

    #[test]
    fn partial_coverage_mixes_as_light_on_a_known_background() {
        // The top sub-pixel averages a white and a clear pixel on black:
        // half the light of white, sRGB 188. Blending the averaged alpha on
        // sRGB values would give 128 and dim every antialiased edge.
        let black = Some(Rgb(0, 0, 0));
        let img = image(2, 2, &[WHITE, CLEAR, WHITE, WHITE]);
        let r = rasterize(
            &img,
            1,
            1,
            BlockGlyphSet::Half,
            black,
            ColorDepth::TrueColor,
            true,
        );
        let grey = Color::Rgb(Rgb(188, 188, 188));
        assert_eq!(r.cells[0][0], cell('▀', grey, rgb(WHITE)));
        // Sub-pixels the image does not cover still show the terminal's own
        // background.
        let img = image(2, 2, &[CLEAR, CLEAR, WHITE, CLEAR]);
        let r = rasterize(
            &img,
            1,
            1,
            BlockGlyphSet::Half,
            black,
            ColorDepth::TrueColor,
            true,
        );
        assert_eq!(r.cells[0][0], cell('▄', grey, Color::Default));
        // The same in a two-colour split: covered sub-pixels are composited,
        // the uncovered one stays transparent.
        let img = image(
            4,
            2,
            &[WHITE, CLEAR, CLEAR, CLEAR, WHITE, WHITE, WHITE, WHITE],
        );
        let r = rasterize(
            &img,
            1,
            1,
            BlockGlyphSet::Quadrant,
            black,
            ColorDepth::TrueColor,
            true,
        );
        assert_eq!(r.cells[0][0].ch, '▙');
        assert_eq!(r.cells[0][0].bg, Color::Default);
    }

    #[test]
    fn gradient_downscale() {
        // A horizontal black → white gradient, 64 px wide, as 4 half-block
        // cells: brightness must increase monotonically across cells.
        let px: Vec<[u8; 4]> = (0..64u32 * 2)
            .map(|i| {
                let v = ((i % 64) * 255 / 63) as u8;
                [v, v, v, 255]
            })
            .collect();
        let img = image(64, 2, &px);
        let r = true_color(&img, 4, 1, BlockGlyphSet::Half);
        let levels: Vec<u8> = r.cells[0]
            .iter()
            .map(|c| match c.bg {
                Color::Rgb(Rgb(v, _, _)) => v,
                _ => panic!("{c:?}"),
            })
            .collect();
        assert!(levels.windows(2).all(|w| w[0] < w[1]), "{levels:?}");
        // Linear-light averaging lifts each cell above the mean of its sRGB
        // codes (30 for the first cell's 0, 4, …, 60).
        let first: u32 = (0..16u32).map(|x| x * 255 / 63).sum::<u32>() / 16;
        assert_eq!(first, 30);
        assert!(u32::from(levels[0]) > first, "{levels:?}");
    }

    #[test]
    fn colour_depths() {
        let img = image(1, 2, &[RED, BLUE]);
        let at = |depth| rasterize(&img, 1, 1, BlockGlyphSet::Half, None, depth, false).cells[0][0];
        assert_eq!(at(ColorDepth::TrueColor), cell('▀', rgb(RED), rgb(BLUE)));
        assert_eq!(
            at(ColorDepth::Ansi256),
            cell('▀', Color::Indexed(196), Color::Indexed(21))
        );
        assert_eq!(
            at(ColorDepth::Ansi16),
            cell('▀', Color::Ansi(9), Color::Ansi(4))
        );
        assert_eq!(at(ColorDepth::Mono), RasterCell::BLANK);
        assert_eq!(at(ColorDepth::None), RasterCell::BLANK);
        // Colours that collapse onto one palette entry give a flat space.
        let close = image(1, 2, &[[250, 0, 0, 255], [252, 0, 0, 255]]);
        let c = rasterize(
            &close,
            1,
            1,
            BlockGlyphSet::Half,
            None,
            ColorDepth::Ansi16,
            false,
        );
        assert_eq!(c.cells[0][0], cell(' ', Color::Default, Color::Ansi(9)));
    }

    #[test]
    fn dithered_cells_follow_the_image() {
        // A flat colour between palette entries, 32 × 16 cells of half blocks.
        let teal = [40, 140, 150, 255];
        let img = image(4, 4, &[teal; 16]);
        for (depth, palette_only) in [
            (
                ColorDepth::Ansi256,
                (|c: Color| matches!(c, Color::Indexed(16..))) as fn(Color) -> bool,
            ),
            (ColorDepth::Ansi16, |c: Color| {
                matches!(c, Color::Ansi(0..16))
            }),
        ] {
            let r = rasterize(&img, 32, 16, BlockGlyphSet::Half, None, depth, true);
            let cells: Vec<RasterCell> = r.cells.iter().flatten().copied().collect();
            assert!(
                cells
                    .iter()
                    .all(|c| palette_only(c.bg) && (c.ch == ' ' || palette_only(c.fg))),
                "{depth:?}"
            );
            // A mix, not one entry.
            let colours: std::collections::HashSet<Color> =
                cells.iter().flat_map(|c| [c.fg, c.bg]).collect();
            assert!(colours.len() > 2, "{depth:?}: {colours:?}");
            // The pattern repeats every 8 sub-pixels: 8 columns, 4 rows of
            // half blocks, from the image's own top-left corner.
            for row in 0..12 {
                for col in 0..24 {
                    assert_eq!(r.cells[row][col], r.cells[row][col + 8], "{depth:?}");
                    assert_eq!(r.cells[row][col], r.cells[row + 4][col], "{depth:?}");
                }
            }
            // Without dithering the colour takes its nearest entry: flat.
            let flat = rasterize(&img, 32, 16, BlockGlyphSet::Half, None, depth, false);
            let first = flat.cells[0][0];
            assert!(flat.cells.iter().flatten().all(|c| *c == first));
            assert_eq!(first.ch, ' ');
        }
    }

    #[test]
    fn shapes_and_degenerate_inputs() {
        let img = image(1, 1, &[RED]);
        let r = true_color(&img, 3, 2, BlockGlyphSet::Octant);
        assert_eq!(
            (r.cols, r.rows, r.cells.len(), r.cells[1].len()),
            (3, 2, 2, 3)
        );
        assert_eq!(r.cells[1][2], cell(' ', Color::Default, rgb(RED)));
        assert_eq!(
            true_color(&img, 0, 2, BlockGlyphSet::Half),
            Raster::blank(0, 2)
        );
        assert_eq!(true_color(&img, 2, 0, BlockGlyphSet::Half).cells.len(), 0);
        let invalid = Rgba {
            width: 5,
            height: 5,
            pixels: vec![1, 2, 3],
        };
        assert_eq!(
            true_color(&invalid, 2, 1, BlockGlyphSet::Sextant),
            Raster::blank(2, 1)
        );
        assert_eq!(Raster::blank(2, 1).glyphs(), "  \n");
        assert_eq!(r.row(1).map(<[RasterCell]>::len), Some(3));
        assert_eq!(r.row(2), None);
    }

    #[test]
    fn deterministic() {
        let px: Vec<[u8; 4]> = (0..37u32 * 23)
            .map(|i| {
                [
                    (i * 7) as u8,
                    (i * 13) as u8,
                    (i * 29) as u8,
                    (i * 31) as u8,
                ]
            })
            .collect();
        let img = image(37, 23, &px);
        for set in [
            BlockGlyphSet::Half,
            BlockGlyphSet::Quadrant,
            BlockGlyphSet::Sextant,
            BlockGlyphSet::Octant,
        ] {
            for depth in [
                ColorDepth::TrueColor,
                ColorDepth::Ansi256,
                ColorDepth::Ansi16,
            ] {
                for dither in [false, true] {
                    let bg = Some(Rgb(30, 30, 46));
                    let a = rasterize(&img, 9, 4, set, bg, depth, dither);
                    let b = rasterize(&img, 9, 4, set, bg, depth, dither);
                    assert_eq!(a, b, "{set:?} {depth:?} {dither}");
                }
            }
        }
    }

    #[test]
    fn best_split_basics() {
        assert_eq!(best_split(&[[5, 5, 5]; 4]), None);
        assert_eq!(best_split(&[[0, 0, 0], [255, 255, 255]]), Some(0b01));
        // The lone outlier is split off, not the median.
        let px = [[10, 0, 0], [12, 0, 0], [11, 0, 0], [250, 0, 0]];
        assert_eq!(best_split(&px), Some(0b0111));
        // Green has the widest range here.
        let px = [[0, 200, 0], [40, 0, 0], [0, 190, 0], [30, 5, 0]];
        assert_eq!(best_split(&px), Some(0b1010));
        // Masks have 8 bits: longer inputs are cut to their first 8 colours.
        let mut long = [[9, 9, 9]; 12];
        long[3] = [200, 0, 0];
        assert_eq!(best_split(&long), Some(0b1111_0111));
        assert_eq!(best_split(&[]), None);
        let many = [Sub::solid([1, 2, 3]); 11];
        let c = split_cell(BlockGlyphSet::Octant, &many);
        assert_eq!((c.ch, c.bg), (' ', Some(Rgb(1, 2, 3))));
    }

    const SETS: [(&str, BlockGlyphSet); 4] = [
        ("half", BlockGlyphSet::Half),
        ("quadrant", BlockGlyphSet::Quadrant),
        ("sextant", BlockGlyphSet::Sextant),
        ("octant", BlockGlyphSet::Octant),
    ];

    /// The glyph grid with flat coloured cells drawn as `█`, so filled and
    /// empty cells can be told apart.
    fn shape(r: &Raster) -> String {
        let mut out = String::new();
        for row in &r.cells {
            out.extend(row.iter().map(|c| match c {
                RasterCell { ch: ' ', bg, .. } if *bg != Color::Default => '█',
                c => c.ch,
            }));
            out.push('\n');
        }
        out
    }

    /// A hard-edged white disc on a transparent `size × size` square.
    fn disc(size: u32) -> Rgba {
        let r = size as f32 / 2.0;
        let px: Vec<[u8; 4]> = (0..size * size)
            .map(|i| {
                let x = (i % size) as f32 + 0.5 - r;
                let y = (i / size) as f32 + 0.5 - r;
                if x * x + y * y <= r * r { WHITE } else { CLEAR }
            })
            .collect();
        image(size, size, &px)
    }

    /// The disc as 16 × 8 cells (round on 1:2 cells) in every glyph set:
    /// finer sets follow the outline more closely.
    #[test]
    fn disc_snapshots() {
        let img = disc(32);
        for (name, set) in SETS {
            let r = true_color(&img, 16, 8, set);
            for c in r.cells.iter().flatten() {
                let flat = c.ch == ' ' && c.fg == Color::Default;
                let edge = c.fg == rgb(WHITE) && c.bg == Color::Default;
                assert!(flat || edge, "{name}: {c:?}");
            }
            insta::assert_snapshot!(format!("disc_{name}"), shape(&r));
        }
    }

    /// A red triangle over a blue one, split along the diagonal: every edge
    /// cell holds both colours.
    #[test]
    fn diagonal_snapshots() {
        let px: Vec<[u8; 4]> = (0..32 * 32u32)
            .map(|i| if i % 32 + i / 32 < 32 { RED } else { BLUE })
            .collect();
        let img = image(32, 32, &px);
        for (name, set) in SETS {
            let r = true_color(&img, 16, 8, set);
            insta::assert_snapshot!(format!("diagonal_{name}"), shape(&r));
        }
    }

    /// 1 MP → 100×40 cells within the plan's 15 ms budget (release build).
    /// The budget is set for half blocks; the finer sets meet it too, and so
    /// does dithering at 256 and 16 colours.
    #[test]
    #[ignore = "timing; run with --release -- --ignored --nocapture"]
    #[allow(clippy::print_stderr)]
    fn timing_one_megapixel() {
        let (w, h) = (1000u32, 1000u32);
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&[(x ^ y) as u8, (x * 3) as u8, (y * 5) as u8, 255]);
            }
        }
        let img = Rgba::new(w, h, pixels).unwrap();
        // The first dithered raster of a run also makes the colour bins'
        // decisions (every bin, for this noise).
        for depth in [ColorDepth::Ansi16, ColorDepth::Ansi256] {
            let bg = Some(Rgb(30, 30, 46));
            let start = std::time::Instant::now();
            std::hint::black_box(rasterize(
                &img,
                100,
                40,
                BlockGlyphSet::Half,
                bg,
                depth,
                true,
            ));
            let cold = start.elapsed();
            eprintln!("rasterize 1 MP → 100×40 half {depth:?}, first of the run: {cold:?}");
            assert!(
                cold.as_millis() <= 30,
                "{depth:?}: {cold:?} for the first raster"
            );
        }
        let depths = [
            ColorDepth::TrueColor,
            ColorDepth::Ansi256,
            ColorDepth::Ansi16,
        ];
        for (name, set) in SETS {
            for depth in depths {
                let bg = Some(Rgb(30, 30, 46));
                let run = || rasterize(&img, 100, 40, set, bg, depth, true);
                let _ = run(); // warm the tables
                let n = 10;
                let start = std::time::Instant::now();
                for _ in 0..n {
                    std::hint::black_box(run());
                }
                let per = start.elapsed() / n;
                eprintln!("rasterize 1 MP → 100×40 {name} {depth:?}: {per:?}");
                assert!(
                    per.as_millis() <= 15,
                    "{name} {depth:?}: {per:?} per raster"
                );
            }
        }
    }
}

/// How close block images come to the picture at reduced colour depth,
/// with and without dithering: what the reader sees is compared with the
/// image resampled to the same sub-pixel grid.
#[cfg(test)]
mod quality {
    use super::*;
    use crate::color::{Oklab, linear_to_oklab, xterm_rgb};

    /// Picture quality of one raster.
    #[derive(Clone, Copy, Debug)]
    struct Quality {
        /// Mean OKLab distance per sub-pixel.
        pixel: f32,
        /// Mean OKLab distance once both are blurred over 4×4 sub-pixels
        /// (in linear light): the colour a reader sees from a little away.
        seen: f32,
        /// Banding: how unevenly the shading steps from one sub-pixel to
        /// the next compared with the picture ([`uneven_steps`]), once both
        /// are blurred over 8×8 (the dither pattern's period, so its texture
        /// averages out and only steps in the shading are left).
        banding: f32,
    }

    /// A smooth horizontal gradient from navy to orange over a grey ramp
    /// (the bottom half).
    fn gradient() -> Rgba {
        let (w, h) = (400u32, 160u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / (w - 1) as f32;
                let c = if y < h / 2 {
                    let lerp = |a: f32, b: f32| (a + (b - a) * t).round() as u8;
                    [lerp(20.0, 250.0), lerp(30.0, 150.0), lerp(90.0, 40.0)]
                } else {
                    let v = (t * 255.0).round() as u8;
                    [v, v, v]
                };
                px.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        Rgba::new(w, h, px).unwrap()
    }

    /// Smooth waves of colour, like the shading of a photograph.
    fn photo() -> Rgba {
        let (w, h) = (400u32, 320u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32, y as f32);
                let wave = (fx / 37.0).sin() * 60.0 + (fy / 23.0).cos() * 60.0;
                let r = (wave + 128.0).clamp(0.0, 255.0) as u8;
                let g = (fx * 0.5 + (fy / 41.0).sin() * 30.0 + 20.0).clamp(0.0, 255.0) as u8;
                let b = (fy * 0.6 + (fx / 29.0).cos() * 40.0 + 30.0).clamp(0.0, 255.0) as u8;
                px.extend_from_slice(&[r, g, b, 255]);
            }
        }
        Rgba::new(w, h, px).unwrap()
    }

    /// The colour a cell colour shows (xterm's defaults for indices).
    fn shown(c: Color) -> Rgb {
        match c {
            Color::Rgb(c) => c,
            Color::Indexed(i) | Color::Ansi(i) => xterm_rgb(i),
            _ => Rgb(0, 0, 0),
        }
    }

    /// The raster as sub-pixels in linear light, row by row.
    fn subpixels_of(r: &Raster, set: BlockGlyphSet) -> (usize, Vec<[f32; 3]>) {
        let (sx, sy) = glyphs::grid(set);
        let (sx, sy) = (usize::from(sx), usize::from(sy));
        let masks: HashMap<char, u8> = (0..1u16 << (sx * sy))
            .map(|m| (glyphs::glyph(set, m as u8), m as u8))
            .collect();
        let width = usize::from(r.cols) * sx;
        let mut out = vec![[0.0; 3]; width * usize::from(r.rows) * sy];
        for (row, cells) in r.cells.iter().enumerate() {
            for (col, cell) in cells.iter().enumerate() {
                let mask = if cell.ch == ' ' { 0 } else { masks[&cell.ch] };
                for i in 0..sx * sy {
                    let c = if mask >> i & 1 == 1 { cell.fg } else { cell.bg };
                    let Rgb(red, green, blue) = shown(c);
                    let (x, y) = (col * sx + i % sx, row * sy + i / sx);
                    out[y * width + x] = [red, green, blue].map(resample::srgb_to_linear);
                }
            }
        }
        (width, out)
    }

    /// A `size × size` box blur (clipped at the edges), one axis at a time.
    fn blur(width: usize, px: &[[f32; 3]], size: usize) -> Vec<[f32; 3]> {
        let height = px.len() / width;
        let back = (size - 1) / 2;
        let window = |i: usize, len: usize| i.saturating_sub(back)..(i + size - back).min(len);
        let mean = |values: &mut dyn Iterator<Item = [f32; 3]>| {
            let (mut sum, mut n) = ([0.0f32; 3], 0.0);
            for v in values {
                for (s, v) in sum.iter_mut().zip(v) {
                    *s += v;
                }
                n += 1.0;
            }
            sum.map(|v| v / n)
        };
        let mut across = vec![[0.0; 3]; px.len()];
        for y in 0..height {
            for x in 0..width {
                across[y * width + x] = mean(&mut window(x, width).map(|xx| px[y * width + xx]));
            }
        }
        let mut out = vec![[0.0; 3]; px.len()];
        for y in 0..height {
            for x in 0..width {
                out[y * width + x] = mean(&mut window(y, height).map(|yy| across[yy * width + x]));
            }
        }
        out
    }

    fn lab([r, g, b]: [f32; 3]) -> Oklab {
        linear_to_oklab(r, g, b)
    }

    fn labs(px: &[[f32; 3]]) -> Vec<Oklab> {
        px.iter().copied().map(lab).collect()
    }

    fn mean_distance(a: &[Oklab], b: &[Oklab]) -> f32 {
        let total: f32 = a.iter().zip(b).map(|(x, y)| x.dist2(*y).sqrt()).sum();
        total / a.len() as f32
    }

    /// How unevenly `got` changes along its rows where `want` changes
    /// smoothly: the RMS difference between their steps from one sub-pixel
    /// to the next (OKLab distances). Bands are flat runs and sudden jumps,
    /// so they score high; following the picture's own steps scores 0.
    fn uneven_steps(width: usize, got: &[Oklab], want: &[Oklab]) -> f32 {
        let steps = |px: &[Oklab]| -> Vec<f32> {
            px.chunks(width)
                .flat_map(|row| row.windows(2).map(|w| w[0].dist2(w[1]).sqrt()))
                .collect()
        };
        let (got, want) = (steps(got), steps(want));
        let sum: f32 = got.iter().zip(&want).map(|(g, w)| (g - w) * (g - w)).sum();
        (sum / got.len().max(1) as f32).sqrt()
    }

    /// The quality of `img` as `cols × rows` cells.
    fn quality(
        img: &Rgba,
        (cols, rows): (u16, u16),
        set: BlockGlyphSet,
        depth: ColorDepth,
        dither: bool,
    ) -> Quality {
        let (sx, sy) = glyphs::grid(set);
        let grid = resample::resample(
            img,
            usize::from(cols * u16::from(sx)),
            usize::from(rows * u16::from(sy)),
            Blend::Premultiplied,
        );
        let reference: Vec<[f32; 3]> = composite(&grid, false)
            .iter()
            .map(|s| match s {
                Sub::Solid { lin, .. } => *lin,
                Sub::Clear => [0.0; 3],
            })
            .collect();
        let raster = rasterize(img, cols, rows, set, None, depth, dither);
        let (width, got) = subpixels_of(&raster, set);
        let seen = |px: &[[f32; 3]]| labs(&blur(width, px, 4));
        let far = |px: &[[f32; 3]]| labs(&blur(width, px, 8));
        Quality {
            pixel: mean_distance(&labs(&reference), &labs(&got)),
            seen: mean_distance(&seen(&reference), &seen(&got)),
            banding: uneven_steps(width, &far(&got), &far(&reference)),
        }
    }

    #[test]
    fn dithering_brings_the_picture_closer() {
        for (name, img) in [("gradient", gradient()), ("photo", photo())] {
            for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16] {
                for set in [BlockGlyphSet::Half, BlockGlyphSet::Octant] {
                    // Smaller than the report's, for debug builds.
                    let nearest = quality(&img, (80, 32), set, depth, false);
                    let dithered = quality(&img, (80, 32), set, depth, true);
                    let what = format!("{name} {depth:?} {set:?}: {nearest:?} → {dithered:?}");
                    assert!(dithered.seen < nearest.seen * 0.7, "{what}");
                    // At 16 colours a gradient still steps where the pair of
                    // entries it mixes changes; it must not step more.
                    let banding = match depth {
                        ColorDepth::Ansi256 => nearest.banding * 0.8,
                        _ => nearest.banding + 0.005,
                    };
                    assert!(dithered.banding < banding, "{what}");
                }
            }
        }
    }

    #[test]
    fn true_colour_is_never_dithered() {
        let img = photo();
        for set in [BlockGlyphSet::Half, BlockGlyphSet::Sextant] {
            let plain = rasterize(&img, 40, 16, set, None, ColorDepth::TrueColor, false);
            let dithered = rasterize(&img, 40, 16, set, None, ColorDepth::TrueColor, true);
            assert_eq!(plain, dithered);
        }
    }

    /// The numbers behind the defaults: `cargo test --release quality --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "a report; run with --release -- --ignored --nocapture"]
    #[allow(clippy::print_stderr)]
    fn report() {
        eprintln!(
            "image     depth      glyphs   nearest: pixel  seen   banding   dithered: pixel  seen   banding"
        );
        for (name, img) in [("gradient", gradient()), ("photo", photo())] {
            for depth in [
                ColorDepth::TrueColor,
                ColorDepth::Ansi256,
                ColorDepth::Ansi16,
            ] {
                for set in [
                    BlockGlyphSet::Half,
                    BlockGlyphSet::Quadrant,
                    BlockGlyphSet::Octant,
                ] {
                    let a = quality(&img, (100, 40), set, depth, false);
                    let b = quality(&img, (100, 40), set, depth, true);
                    eprintln!(
                        "{name:<9} {depth:<10?} {set:<8?} {:>14.4} {:>6.4} {:>8.4} {:>16.4} {:>6.4} {:>8.4}",
                        a.pixel, a.seen, a.banding, b.pixel, b.seen, b.banding
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod props {
    use proptest::prelude::*;

    use super::*;

    /// Deterministic pseudo-random bytes (SplitMix64).
    fn bytes(seed: u64, len: usize) -> Vec<u8> {
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

    const SETS: [BlockGlyphSet; 4] = [
        BlockGlyphSet::Half,
        BlockGlyphSet::Quadrant,
        BlockGlyphSet::Sextant,
        BlockGlyphSet::Octant,
    ];

    const DEPTHS: [ColorDepth; 5] = [
        ColorDepth::None,
        ColorDepth::Mono,
        ColorDepth::Ansi16,
        ColorDepth::Ansi256,
        ColorDepth::TrueColor,
    ];

    fn depth_colour_ok(depth: ColorDepth, c: Color) -> bool {
        match (depth, c) {
            (_, Color::Default) => true,
            (ColorDepth::TrueColor, Color::Rgb(_)) => true,
            (ColorDepth::Ansi256, Color::Indexed(i)) => i >= 16,
            (ColorDepth::Ansi16, Color::Ansi(i)) => i < 16,
            _ => false,
        }
    }

    proptest! {
        #[test]
        fn rasterize_is_total_and_well_formed(
            w in 0u32..14,
            h in 0u32..14,
            extra in 0usize..3,
            seed in any::<u64>(),
            cols in 0u16..9,
            rows in 0u16..6,
            set in 0usize..4,
            depth in 0usize..5,
            bg in proptest::option::of(any::<(u8, u8, u8)>()),
            dither in any::<bool>(),
        ) {
            // `extra` > 0 makes the buffer length inconsistent (invalid image).
            let len = (w * h * 4) as usize + extra;
            let img = Rgba { width: w, height: h, pixels: bytes(seed, len) };
            let (set, depth) = (SETS[set], DEPTHS[depth]);
            let bg = bg.map(|(r, g, b)| Rgb(r, g, b));
            let r = rasterize(&img, cols, rows, set, bg, depth, dither);
            prop_assert_eq!((r.cols, r.rows), (cols, rows));
            prop_assert_eq!(r.cells.len(), usize::from(rows));
            let glyphs: Vec<char> = (0..1u16 << glyphs::subpixels(set))
                .map(|m| glyphs::glyph(set, m as u8))
                .collect();
            for row in &r.cells {
                prop_assert_eq!(row.len(), usize::from(cols));
                for c in row {
                    prop_assert!(glyphs.contains(&c.ch), "{:?} not in {:?}", c.ch, set);
                    if c.ch == ' ' {
                        prop_assert_eq!(c.fg, Color::Default);
                    } else {
                        prop_assert!(c.fg != c.bg, "{:?}", c);
                    }
                    prop_assert!(depth_colour_ok(depth, c.fg), "{:?} at {:?}", c, depth);
                    prop_assert!(depth_colour_ok(depth, c.bg), "{:?} at {:?}", c, depth);
                }
            }
            if depth < ColorDepth::Ansi16 || extra > 0 {
                prop_assert_eq!(r, Raster::blank(cols, rows));
            }
        }

        #[test]
        fn resize_gives_a_valid_image(
            w in 0u32..20,
            h in 0u32..20,
            seed in any::<u64>(),
            tw in 0u32..24,
            th in 0u32..24,
        ) {
            let img = Rgba { width: w, height: h, pixels: bytes(seed, (w * h * 4) as usize) };
            let out = crate::gfx::resize(&img, tw, th);
            prop_assert_eq!((out.width, out.height), (tw, th));
            prop_assert!(out.is_valid());
            // Resizing to the same size is the identity for opaque pixels.
            if (tw, th) == (w, h) {
                for (a, b) in img.pixels.as_chunks::<4>().0.iter().zip(out.pixels.as_chunks::<4>().0) {
                    if a[3] == 255 {
                        prop_assert_eq!(a, b);
                    }
                }
            }
        }
    }
}
