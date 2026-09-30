//! The 2D box model: a grid of terminal cells with a baseline.
//!
//! A [`MBox`] is `w × h` cells, row-major, with `h ≥ 1` and `base < h`. A
//! cell holds one cluster (see [`width`](crate::width): a character with its
//! combining marks, or a joining sequence) and its attributes; a wide
//! character takes two cells, the second marked `cont`. A combining mark never
//! starts a cell.
//!
//! The combinators copy cells, which is cheap at formula sizes. Every
//! constructor refuses boxes over [`MAX_CELLS`] cells, so pathological input
//! cannot allocate without bound; callers turn `None` into the linear
//! fallback.

use crate::MathBox;
use crate::MathLine;
use crate::linear::{Attrs, Frag};
use crate::width::{clusters, is_zero_width, joins, str_width, to_u16};

/// The most cells one box may have (a 2D rendering that large could never
/// fit a terminal anyway); about 9 MiB.
pub(crate) const MAX_CELLS: usize = 1 << 18;

/// The most UTF-8 bytes a cell holds; a longer cluster (some emoji ZWJ
/// sequences, or a pile of combining marks) loses its tail.
const GLYPH_BYTES: usize = 31;

/// The content of one cell, stored inline as UTF-8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Glyph {
    bytes: [u8; GLYPH_BYTES],
    len: u8,
}

impl Glyph {
    pub(crate) const SPACE: Glyph = {
        let mut bytes = [0; GLYPH_BYTES];
        bytes[0] = b' ';
        Glyph { bytes, len: 1 }
    };

    const EMPTY: Glyph = Glyph {
        bytes: [0; GLYPH_BYTES],
        len: 0,
    };

    /// A one-character glyph; a zero-width character gets a no-break space
    /// to sit on, so that the cell is one column wide.
    pub(crate) fn new(c: char) -> Glyph {
        let mut buf = [0; 4];
        Glyph::from_cluster(c.encode_utf8(&mut buf))
    }

    /// A cluster as a glyph; an orphan mark gets a no-break space to sit on.
    fn from_cluster(cluster: &str) -> Glyph {
        let mut glyph = Glyph::EMPTY;
        match cluster.chars().next() {
            Some(c) if is_zero_width(c) => {
                glyph.push('\u{A0}');
            }
            Some(_) => {}
            None => return Glyph::SPACE,
        }
        for c in cluster.chars() {
            if !glyph.push(c) {
                break;
            }
        }
        glyph
    }

    /// Append a character; `false` (and nothing appended) when it does not
    /// fit.
    pub(crate) fn push(&mut self, c: char) -> bool {
        let mut buf = [0; 4];
        let encoded = c.encode_utf8(&mut buf).as_bytes();
        let start = usize::from(self.len);
        let end = start + encoded.len();
        match (self.bytes.get_mut(start..end), u8::try_from(end)) {
            (Some(slot), Ok(len)) => {
                slot.copy_from_slice(encoded);
                self.len = len;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or(" ")
    }

    fn is_space(&self) -> bool {
        self.as_str() == " "
    }
}

/// One terminal cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cell {
    pub(crate) glyph: Glyph,
    pub(crate) attrs: Attrs,
    /// The second column of a wide character.
    pub(crate) cont: bool,
}

impl Cell {
    pub(crate) const BLANK: Cell = Cell {
        glyph: Glyph::SPACE,
        attrs: Attrs::PLAIN,
        cont: false,
    };

    pub(crate) fn new(c: char, attrs: Attrs) -> Cell {
        Cell {
            glyph: Glyph::new(c),
            attrs,
            cont: false,
        }
    }

    pub(crate) fn is_blank(&self) -> bool {
        !self.cont && self.glyph.is_space()
    }
}

/// A box of cells with a baseline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MBox {
    pub(crate) w: usize,
    pub(crate) h: usize,
    /// The row aligned with the surrounding baseline.
    pub(crate) base: usize,
    cells: Vec<Cell>,
}

impl MBox {
    /// A blank box, or `None` when it would be too large.
    pub(crate) fn blank(w: usize, h: usize, base: usize) -> Option<MBox> {
        let h = h.max(1);
        if w.checked_mul(h)? > MAX_CELLS {
            return None;
        }
        Some(MBox {
            w,
            h,
            base: base.min(h - 1),
            cells: vec![Cell::BLANK; w * h],
        })
    }

    /// One row of text from a styled fragment.
    pub(crate) fn text(frag: &Frag, cjk: bool) -> Option<MBox> {
        let mut cells = Vec::with_capacity(frag.text.len());
        let mut spans = frag.spans.iter().peekable();
        let mut offset = 0;
        for (cluster, _) in clusters(&frag.text, cjk) {
            while spans
                .next_if(|s| usize::try_from(s.end).is_ok_and(|end| end <= offset))
                .is_some()
            {}
            let attrs = spans.peek().map_or(Attrs::PLAIN, |s| Attrs::of(s));
            offset += cluster.len();
            let glyph = Glyph::from_cluster(cluster);
            let width = str_width(glyph.as_str(), cjk).max(1);
            cells.push(Cell {
                glyph,
                attrs,
                cont: false,
            });
            for _ in 1..width {
                cells.push(Cell {
                    glyph: Glyph::SPACE,
                    attrs,
                    cont: true,
                });
            }
            if cells.len() > MAX_CELLS {
                return None;
            }
        }
        Some(MBox {
            w: cells.len(),
            h: 1,
            base: 0,
            cells,
        })
    }

    /// One row of single-width characters.
    pub(crate) fn row_of(chars: &[char], attrs: Attrs) -> Option<MBox> {
        let mut b = MBox::blank(chars.len(), 1, 0)?;
        for (x, &c) in chars.iter().enumerate() {
            b.set(x, 0, Cell::new(c, attrs));
        }
        Some(b)
    }

    /// One column of single-width characters with its baseline at `base`.
    pub(crate) fn column_of(chars: &[char], attrs: Attrs, base: usize) -> Option<MBox> {
        let mut b = MBox::blank(1, chars.len(), base)?;
        for (y, &c) in chars.iter().enumerate() {
            b.set(0, y, Cell::new(c, attrs));
        }
        Some(b)
    }

    pub(crate) fn get(&self, x: usize, y: usize) -> Option<&Cell> {
        if x < self.w {
            self.cells.get(y * self.w + x)
        } else {
            None
        }
    }

    pub(crate) fn get_mut(&mut self, x: usize, y: usize) -> Option<&mut Cell> {
        if x < self.w {
            self.cells.get_mut(y * self.w + x)
        } else {
            None
        }
    }

    /// Set one cell (ignored outside the box).
    pub(crate) fn set(&mut self, x: usize, y: usize, cell: Cell) {
        if let Some(slot) = self.get_mut(x, y) {
            *slot = cell;
        }
    }

    /// Copy `src` into this box with its top-left corner at `(x, y)`; blank
    /// cells of `src` leave what is below them.
    pub(crate) fn place(&mut self, x: usize, y: usize, src: &MBox) {
        for sy in 0..src.h {
            for sx in 0..src.w {
                if let Some(cell) = src.get(sx, sy)
                    && !cell.is_blank()
                {
                    self.set(x + sx, y + sy, *cell);
                }
            }
        }
    }

    /// Place boxes side by side, aligned on their baselines.
    pub(crate) fn hcat(parts: &[MBox]) -> Option<MBox> {
        let above = parts.iter().map(|p| p.base).max().unwrap_or(0);
        let below = parts
            .iter()
            .map(|p| p.h.saturating_sub(p.base + 1))
            .max()
            .unwrap_or(0);
        let w = parts.iter().try_fold(0usize, |w, p| w.checked_add(p.w))?;
        let mut out = MBox::blank(w, above + 1 + below, above)?;
        let mut x = 0;
        for p in parts {
            out.place(x, above - p.base, p);
            x += p.w;
        }
        Some(out)
    }

    /// Stack boxes vertically, each centred (rounding left); the baseline is
    /// that of `parts[base_part]`.
    pub(crate) fn vstack(parts: &[MBox], base_part: usize) -> Option<MBox> {
        let w = parts.iter().map(|p| p.w).max().unwrap_or(0);
        let h = parts.iter().try_fold(0usize, |h, p| h.checked_add(p.h))?;
        let mut out = MBox::blank(w, h, 0)?;
        let mut y = 0;
        for (i, p) in parts.iter().enumerate() {
            if i == base_part {
                out.base = y + p.base;
            }
            out.place((w - p.w) / 2, y, p);
            y += p.h;
        }
        Some(out)
    }

    /// A fraction: the numerator over a bar as wide as the wider part (plus
    /// `overhang` columns on each side), over the denominator; the bar is the
    /// baseline. Without `bar` (`\binom`) the bar row stays blank.
    pub(crate) fn fraction(num: &MBox, den: &MBox, bar: bool, overhang: usize) -> Option<MBox> {
        let w = num.w.max(den.w) + 2 * overhang;
        let rule = if bar { '─' } else { ' ' };
        let bar_row = MBox::row_of(&vec![rule; w], Attrs::role(crate::MathRole::Delim))?;
        let mut out = MBox::vstack(&[num.clone(), bar_row, den.clone()], 1)?;
        out.base = num.h;
        Some(out)
    }

    /// Convert to the public form: every row exactly `w` columns, padded with
    /// plain spaces.
    pub(crate) fn to_math_box(&self, cjk: bool) -> MathBox {
        let rows = (0..self.h).map(|y| self.row_line(y, cjk)).collect();
        MathBox {
            width: to_u16(self.w),
            height: to_u16(self.h),
            baseline: to_u16(self.base),
            rows,
        }
    }

    /// Row `y` as a line. Where two neighbouring cells would be measured
    /// together (an emoji and a joiner, lam and alef), a zero-width
    /// non-joiner keeps them apart so the row stays exactly `w` wide.
    fn row_line(&self, y: usize, cjk: bool) -> MathLine {
        let mut frag = Frag::default();
        let mut prev: Option<Glyph> = None;
        let mut buf = String::new();
        for x in 0..self.w {
            let Some(cell) = self.get(x, y) else { continue };
            if cell.cont {
                continue;
            }
            let text = cell.glyph.as_str();
            if prev.is_some_and(|p| joins(p.as_str(), text, cjk, &mut buf)) {
                frag.push("\u{200C}", cell.attrs);
            }
            frag.push(text, cell.attrs);
            prev = Some(cell.glyph);
        }
        MathLine {
            text: frag.text,
            spans: frag.spans,
            breaks: Vec::new(),
            width: to_u16(self.w),
            ok: true,
        }
    }

    /// Add `mark` to every visible cell of row `y` (`\not` over a tall box).
    pub(crate) fn mark_row(&mut self, y: usize, mark: char) {
        for x in 0..self.w {
            if let Some(cell) = self.get_mut(x, y)
                && !cell.is_blank()
                && !cell.cont
            {
                let _ = cell.glyph.push(mark);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MathRole;

    fn frag(s: &str) -> Frag {
        let mut f = Frag::default();
        f.push(s, Attrs::role(MathRole::Var));
        f
    }

    fn text(s: &str) -> MBox {
        MBox::text(&frag(s), false).unwrap()
    }

    /// Rows as strings (trailing padding kept).
    fn rows(b: &MBox) -> Vec<String> {
        b.to_math_box(false)
            .rows
            .into_iter()
            .map(|r| r.text)
            .collect()
    }

    #[test]
    fn text_boxes_hold_clusters_and_wide_characters() {
        let b = text("x\u{302}日");
        assert_eq!((b.w, b.h, b.base), (3, 1, 0));
        assert_eq!(rows(&b), ["x\u{302}日"]);
        let orphan = text("\u{301}");
        assert_eq!(rows(&orphan), ["\u{A0}\u{301}"]);
        // Joining sequences share a cell; neighbours that would join are kept
        // apart by a zero-width non-joiner.
        let family = "\u{1F468}\u{200D}\u{1F469}";
        assert_eq!(
            (text(family).w, rows(&text(family))),
            (2, vec![family.to_string()])
        );
        let row = MBox::hcat(&[text("\u{1F468}\u{200D}"), text("\u{1F469}")]).unwrap();
        assert_eq!(row.w, 4);
        let line = &row.to_math_box(false).rows[0];
        assert_eq!(line.text, "\u{1F468}\u{200D}\u{200C}\u{1F469}");
        assert_eq!(str_width(&line.text, false), 4);
        // A cell holds at most 31 bytes.
        let pile = format!("a{}", "\u{301}".repeat(40));
        assert_eq!(rows(&text(&pile))[0].len(), 31);
    }

    #[test]
    fn hcat_aligns_baselines() {
        let tall = MBox::fraction(&text("a"), &text("b"), true, 0).unwrap();
        let b = MBox::hcat(&[text("x="), tall, text("+1")]).unwrap();
        assert_eq!((b.w, b.h, b.base), (5, 3, 1));
        assert_eq!(rows(&b), ["  a  ", "x=─+1", "  b  "]);
    }

    #[test]
    fn vstack_centres_rounding_left() {
        let b = MBox::vstack(&[text("n"), text("∑"), text("i=1")], 1).unwrap();
        assert_eq!(rows(&b), [" n ", " ∑ ", "i=1"]);
        assert_eq!(b.base, 1);
        let b = MBox::vstack(&[text("ab"), text("wxyz")], 0).unwrap();
        assert_eq!(rows(&b), [" ab ", "wxyz"]);
    }

    #[test]
    fn fractions_have_no_overhang() {
        let b = MBox::fraction(&text("n(n+1)(2n+1)"), &text("6"), true, 0).unwrap();
        assert_eq!(rows(&b), ["n(n+1)(2n+1)", "────────────", "     6      "]);
        assert_eq!(b.base, 1);
        let binom = MBox::fraction(&text("n"), &text("k"), false, 0).unwrap();
        assert_eq!(rows(&binom), ["n", " ", "k"]);
    }

    #[test]
    fn rows_are_padded_to_the_width() {
        let b = MBox::vstack(&[text("a"), text("bcd")], 0).unwrap();
        let mb = b.to_math_box(false);
        assert_eq!(mb.width, 3);
        for row in &mb.rows {
            assert_eq!(row.width, 3);
            assert_eq!(str_width(&row.text, false), 3);
            assert_eq!(
                row.spans.last().map(|s| s.end as usize),
                Some(row.text.len())
            );
        }
        // The padding is plain, the content keeps its role.
        assert_eq!(mb.rows[0].text, " a ");
        assert_eq!(mb.rows[0].spans.len(), 3);
        let (pad, content) = (mb.rows[0].spans[0], mb.rows[0].spans[1]);
        assert_eq!((pad.role, content.role), (MathRole::Plain, MathRole::Var));
    }

    #[test]
    fn oversized_boxes_are_refused() {
        assert!(MBox::blank(MAX_CELLS, 2, 0).is_none());
        assert!(MBox::blank(usize::MAX, 2, 0).is_none());
        assert!(MBox::blank(10, 10, 20).is_some_and(|b| b.base == 9));
        assert_eq!(
            MBox::blank(0, 1, 0).unwrap().to_math_box(false).rows.len(),
            1
        );
    }

    #[test]
    fn single_character_cells_are_one_column() {
        // A zero-width character sits on a no-break space.
        let row = MBox::row_of(&['\u{200D}', 'x'], Attrs::PLAIN).unwrap();
        assert_eq!(rows(&row), ["\u{A0}\u{200D}x"]);
        assert_eq!(str_width(&rows(&row)[0], false), 2);
    }

    #[test]
    fn place_skips_blanks_and_clips() {
        let mut canvas = MBox::row_of(&['a', 'b', 'c'], Attrs::PLAIN).unwrap();
        canvas.place(1, 0, &text(" x"));
        assert_eq!(rows(&canvas), ["abx"]);
        canvas.place(2, 0, &text("yz"));
        assert_eq!(rows(&canvas), ["aby"]);
        canvas.place(0, 5, &text("q"));
        assert_eq!(rows(&canvas), ["aby"]);
    }
}
