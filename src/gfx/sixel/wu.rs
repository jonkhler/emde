//! Wu's colour quantiser: a palette of up to 256 colours for an opaque
//! image, and a constant-time map from any colour to its palette entry.
//!
//! Xiaolin Wu, "Efficient Statistical Computations for Optimal Color
//! Quantization", Graphics Gems II (1991). Colours are counted in a
//! histogram of 32 levels per channel; cumulative moments (count, channel
//! sums and the sum of squares) give the variance of any box of that grid
//! in constant time. The box with the largest variance is cut in two, on
//! the axis and at the plane that leave the least variance, until there
//! are as many boxes as colours wanted (or no box can be cut). Each box's
//! colour is the mean of its pixels; every histogram cell is tagged with
//! the box holding it, which maps colours to the palette.
//!
//! The moments are computed once per image ([`Histogram::of`]) and can be
//! cut into palettes of several sizes ([`Histogram::palette`]).

/// Histogram levels per channel, plus the zero border the cumulative
/// moments need at index 0.
const SIDE: usize = 33;

/// The histogram cell holding a colour.
fn cell(r: usize, g: usize, b: usize) -> usize {
    (r * SIDE + g) * SIDE + b
}

/// The histogram level (1 to 32) of an 8-bit channel value.
fn level(v: u8) -> usize {
    usize::from(v >> 3) + 1
}

/// The histogram cell of an 8-bit colour.
pub(super) fn cell_of([r, g, b]: [u8; 3]) -> usize {
    cell(level(r), level(g), level(b))
}

/// Cumulative moments of an image's colours over the histogram grid: entry
/// `(r, g, b)` sums the cells `1..=r × 1..=g × 1..=b`.
pub(super) struct Histogram {
    /// Pixel counts.
    weight: Vec<i64>,
    /// Channel sums.
    red: Vec<i64>,
    green: Vec<i64>,
    blue: Vec<i64>,
    /// Sums of `r² + g² + b²`.
    squares: Vec<f64>,
}

/// A box of histogram cells: levels `r0 + 1 ..= r1` and so on (the lower
/// bounds are exclusive, as the inclusion–exclusion sums need).
#[derive(Clone, Copy, Debug, Default)]
struct Cube {
    r0: usize,
    r1: usize,
    g0: usize,
    g1: usize,
    b0: usize,
    b1: usize,
}

impl Cube {
    /// The number of histogram cells in the box.
    fn volume(&self) -> usize {
        (self.r1 - self.r0) * (self.g1 - self.g0) * (self.b1 - self.b0)
    }
}

/// A channel: the axis of a cut.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Axis {
    Red,
    Green,
    Blue,
}

/// The four sums of a box ([`Histogram::sums`]).
#[derive(Clone, Copy, Default)]
struct Sums {
    weight: i64,
    red: i64,
    green: i64,
    blue: i64,
}

impl Sums {
    fn minus(self, o: Sums) -> Sums {
        Sums {
            weight: self.weight - o.weight,
            red: self.red - o.red,
            green: self.green - o.green,
            blue: self.blue - o.blue,
        }
    }

    /// `|sum|² / weight`: how much the box's mean explains of its squares.
    fn explained(self) -> f64 {
        let (r, g, b) = (self.red as f64, self.green as f64, self.blue as f64);
        (r * r + g * g + b * b) / self.weight as f64
    }
}

/// A palette: its colours, and the entry each histogram cell maps to.
pub(super) struct Palette {
    pub(super) colors: Vec<[u8; 3]>,
    /// For every histogram cell ([`cell_of`]), its colour's index.
    tags: Vec<u8>,
}

impl Palette {
    /// The index of the palette colour for `rgb`.
    pub(super) fn index(&self, rgb: [u8; 3]) -> u8 {
        self.tags[cell_of(rgb)]
    }
}

impl Histogram {
    /// The cumulative moments of `pixels` (`[r, g, b, _]` each).
    pub(super) fn of(pixels: &[[u8; 4]]) -> Histogram {
        let n = SIDE * SIDE * SIDE;
        let mut h = Histogram {
            weight: vec![0; n],
            red: vec![0; n],
            green: vec![0; n],
            blue: vec![0; n],
            squares: vec![0.0; n],
        };
        for &[r, g, b, _] in pixels {
            let i = cell_of([r, g, b]);
            let (r, g, b) = (i64::from(r), i64::from(g), i64::from(b));
            h.weight[i] += 1;
            h.red[i] += r;
            h.green[i] += g;
            h.blue[i] += b;
            h.squares[i] += (r * r + g * g + b * b) as f64;
        }
        h.accumulate();
        h
    }

    /// Turn the per-cell counts into cumulative moments.
    fn accumulate(&mut self) {
        for r in 1..SIDE {
            let mut area = [Sums::default(); SIDE];
            let mut area2 = [0.0f64; SIDE];
            for g in 1..SIDE {
                let mut line = Sums::default();
                let mut line2 = 0.0f64;
                for b in 1..SIDE {
                    let i = cell(r, g, b);
                    line.weight += self.weight[i];
                    line.red += self.red[i];
                    line.green += self.green[i];
                    line.blue += self.blue[i];
                    line2 += self.squares[i];
                    area[b].weight += line.weight;
                    area[b].red += line.red;
                    area[b].green += line.green;
                    area[b].blue += line.blue;
                    area2[b] += line2;
                    let below = cell(r - 1, g, b);
                    self.weight[i] = self.weight[below] + area[b].weight;
                    self.red[i] = self.red[below] + area[b].red;
                    self.green[i] = self.green[below] + area[b].green;
                    self.blue[i] = self.blue[below] + area[b].blue;
                    self.squares[i] = self.squares[below] + area2[b];
                }
            }
        }
    }

    /// The moment sums of the cells of `c`.
    fn sums(&self, c: &Cube) -> Sums {
        let at = |i: usize| Sums {
            weight: self.weight[i],
            red: self.red[i],
            green: self.green[i],
            blue: self.blue[i],
        };
        let plus = [
            cell(c.r1, c.g1, c.b1),
            cell(c.r1, c.g0, c.b0),
            cell(c.r0, c.g1, c.b0),
            cell(c.r0, c.g0, c.b1),
        ];
        let minus = [
            cell(c.r1, c.g1, c.b0),
            cell(c.r1, c.g0, c.b1),
            cell(c.r0, c.g1, c.b1),
            cell(c.r0, c.g0, c.b0),
        ];
        let mut s = Sums::default();
        for (p, m) in plus.into_iter().zip(minus) {
            let d = at(p).minus(at(m));
            s.weight += d.weight;
            s.red += d.red;
            s.green += d.green;
            s.blue += d.blue;
        }
        s
    }

    /// The sum of squares of the cells of `c`.
    fn squares(&self, c: &Cube) -> f64 {
        let m = &self.squares;
        m[cell(c.r1, c.g1, c.b1)] - m[cell(c.r1, c.g1, c.b0)] - m[cell(c.r1, c.g0, c.b1)]
            + m[cell(c.r1, c.g0, c.b0)]
            - m[cell(c.r0, c.g1, c.b1)]
            + m[cell(c.r0, c.g1, c.b0)]
            + m[cell(c.r0, c.g0, c.b1)]
            - m[cell(c.r0, c.g0, c.b0)]
    }

    /// The sum of squared distances of `c`'s pixels from their mean.
    fn variance(&self, c: &Cube) -> f64 {
        let s = self.sums(c);
        if s.weight == 0 {
            return 0.0;
        }
        self.squares(c) - s.explained()
    }

    /// The sums of the part of `c` up to level `at` on `axis` (included).
    fn lower_part(&self, c: &Cube, axis: Axis, at: usize) -> Sums {
        let mut part = *c;
        match axis {
            Axis::Red => part.r1 = at,
            Axis::Green => part.g1 = at,
            Axis::Blue => part.b1 = at,
        }
        self.sums(&part)
    }

    /// The best cut of `c` on `axis`: how much of the variance the two
    /// halves' means explain, and the last level of the lower half. `None`
    /// if every cut leaves a half empty.
    fn best_cut(&self, c: &Cube, axis: Axis, whole: Sums) -> Option<(f64, usize)> {
        let (first, last) = match axis {
            Axis::Red => (c.r0, c.r1),
            Axis::Green => (c.g0, c.g1),
            Axis::Blue => (c.b0, c.b1),
        };
        let mut best: Option<(f64, usize)> = None;
        for at in first + 1..last {
            let lower = self.lower_part(c, axis, at);
            let upper = whole.minus(lower);
            if lower.weight == 0 || upper.weight == 0 {
                continue;
            }
            let score = lower.explained() + upper.explained();
            if best.is_none_or(|(b, _)| score > b) {
                best = Some((score, at));
            }
        }
        best
    }

    /// Cut `c` in two where the halves' variance is least: the lower and
    /// the upper half, or `None` if it cannot be cut.
    fn cut(&self, c: &Cube) -> Option<(Cube, Cube)> {
        let whole = self.sums(c);
        let (axis, at) = [Axis::Red, Axis::Green, Axis::Blue]
            .into_iter()
            .filter_map(|axis| self.best_cut(c, axis, whole).map(|(s, at)| (s, axis, at)))
            // Ties go to the earlier axis (red, then green), as in Wu's code.
            .reduce(|best, next| if next.0 > best.0 { next } else { best })
            .map(|(_, axis, at)| (axis, at))?;
        let (mut lower, mut upper) = (*c, *c);
        match axis {
            Axis::Red => (lower.r1, upper.r0) = (at, at),
            Axis::Green => (lower.g1, upper.g0) = (at, at),
            Axis::Blue => (lower.b1, upper.b0) = (at, at),
        }
        Some((lower, upper))
    }

    /// A palette of at most `max_colors` (2 to 256) colours. An image with
    /// no pixels gets an empty palette.
    pub(super) fn palette(&self, max_colors: usize) -> Palette {
        let max_colors = max_colors.clamp(1, 256);
        let whole = Cube {
            r1: SIDE - 1,
            g1: SIDE - 1,
            b1: SIDE - 1,
            ..Cube::default()
        };
        let mut cubes = vec![whole];
        // The variance of each cube; 0 once it cannot be cut.
        let mut spread = vec![self.variance(&whole)];
        while cubes.len() < max_colors {
            let Some((next, &v)) = spread
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(&a.0)))
            else {
                break;
            };
            if v <= 0.0 {
                break;
            }
            let Some((lower, upper)) = self.cut(&cubes[next]) else {
                spread[next] = 0.0;
                continue;
            };
            // A single cell cannot be cut further.
            let spread_of = |c: &Cube| {
                if c.volume() > 1 {
                    self.variance(c)
                } else {
                    0.0
                }
            };
            spread[next] = spread_of(&lower);
            spread.push(spread_of(&upper));
            cubes[next] = lower;
            cubes.push(upper);
        }
        let mut colors = Vec::with_capacity(cubes.len());
        let mut tags = vec![0u8; SIDE * SIDE * SIDE];
        for c in &cubes {
            let s = self.sums(c);
            if s.weight == 0 {
                // Only the first cube of an image with no pixels.
                continue;
            }
            let mean = |sum: i64| ((sum + s.weight / 2) / s.weight).clamp(0, 255) as u8;
            let index = colors.len() as u8;
            colors.push([mean(s.red), mean(s.green), mean(s.blue)]);
            for r in c.r0 + 1..=c.r1 {
                for g in c.g0 + 1..=c.g1 {
                    let row = cell(r, g, 0);
                    tags[row + c.b0 + 1..=row + c.b1].fill(index);
                }
            }
        }
        Palette { colors, tags }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(colors: &[[u8; 3]]) -> Vec<[u8; 4]> {
        colors.iter().map(|&[r, g, b]| [r, g, b, 255]).collect()
    }

    #[test]
    fn one_colour_is_one_entry() {
        let p = Histogram::of(&pixels(&[[255, 0, 0]; 9])).palette(128);
        assert_eq!(p.colors, [[255, 0, 0]]);
        assert_eq!(p.index([0, 0, 255]), 0, "every colour maps somewhere");
    }

    #[test]
    fn distinct_colours_get_their_own_entries() {
        let colors = [[0, 0, 0], [255, 255, 255], [255, 0, 0], [0, 128, 255]];
        let p = Histogram::of(&pixels(&colors)).palette(16);
        assert_eq!(p.colors.len(), 4);
        for c in colors {
            assert_eq!(p.colors[usize::from(p.index(c))], c, "{c:?}");
        }
    }

    #[test]
    fn palette_size_is_capped() {
        let colors: Vec<[u8; 3]> = (0..=255u8)
            .flat_map(|v| [[v, 0, 0], [0, v, 0], [0, 0, v], [v, v, 255 - v]])
            .collect();
        let h = Histogram::of(&pixels(&colors));
        for n in [2, 8, 64, 128] {
            let p = h.palette(n);
            assert!(p.colors.len() <= n);
            assert!(p.colors.len() >= n / 2, "{} of {n}", p.colors.len());
            assert!(p.tags.iter().all(|&t| usize::from(t) < p.colors.len()));
        }
    }

    #[test]
    fn means_are_rounded_averages() {
        // Two shades in one histogram cell: one entry, their mean.
        let p = Histogram::of(&pixels(&[[8, 8, 8], [11, 11, 11]])).palette(8);
        assert_eq!(p.colors, [[10, 10, 10]]);
    }

    #[test]
    fn no_pixels_no_palette() {
        let p = Histogram::of(&[]).palette(128);
        assert!(p.colors.is_empty());
    }
}
