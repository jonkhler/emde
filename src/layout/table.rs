//! Tables: CSS auto-layout column widths, wrapped cells, borders and zebra
//! stripes, or one record per row when the terminal is too narrow.
//!
//! Column widths follow CSS automatic table layout ([`allocate_columns`]):
//! each column has a minimum (its widest unbreakable piece, capped at
//! `max(8, avail / n)`) and a maximum (its natural width). With borders and
//! one column of padding on each side a table needs `3n + 1` extra columns.

use crate::ir::{HAlign, Table};
use crate::style::{Color, StyleId};
use crate::term::ColorDepth;
use crate::text::{Line as WrapLine, Piece};
use crate::theme::Element;

use super::build::Builder;
use super::inline::{Composed, Look};
use super::style::Ctx;
use super::{Fill, LineKind};

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Column widths for `avail` columns of cell content (borders and padding
/// excluded), CSS auto-layout style:
///
/// 1. if every column fits at its maximum, use the maxima;
/// 2. else if the minima fit, raise every column towards a common level
///    (water-filling), capped by its maximum;
/// 3. else if there are at least 4 columns per column, cut the widest
///    minima down to a common level, never below 4 (narrow columns keep
///    their words whole);
/// 4. else `None`: the table should be shown as records.
///
/// The result always sums to at most `avail`, and every width is at least 1.
pub fn allocate_columns(mins: &[u16], maxs: &[u16], avail: u16) -> Option<Vec<u16>> {
    let n = mins.len().min(maxs.len());
    if n == 0 {
        return Some(Vec::new());
    }
    let cap = (avail / to_u16(n).max(1)).max(8);
    let min: Vec<u16> = mins.iter().take(n).map(|&m| m.clamp(1, cap)).collect();
    let max: Vec<u16> = maxs
        .iter()
        .take(n)
        .zip(&min)
        .map(|(&m, &lo)| m.max(lo))
        .collect();
    let sum = |v: &[u16]| v.iter().map(|&x| u64::from(x)).sum::<u64>();
    let avail64 = u64::from(avail);
    if sum(&max) <= avail64 {
        Some(max)
    } else if sum(&min) <= avail64 {
        Some(water_fill(&min, &max, avail64))
    } else if avail64 >= 4 * n as u64 {
        Some(shrink(&min, avail64))
    } else {
        None
    }
}

/// Raise every column to a common level (capped by its maximum) so the
/// widths sum to `avail`.
fn water_fill(min: &[u16], max: &[u16], avail: u64) -> Vec<u16> {
    let total = |level: u64| -> u64 {
        min.iter()
            .zip(max)
            .map(|(&lo, &hi)| level.clamp(u64::from(lo), u64::from(hi)))
            .sum()
    };
    let (mut lo, mut hi) = (0u64, max.iter().map(|&m| u64::from(m)).max().unwrap_or(0));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if total(mid) <= avail {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut w: Vec<u16> = min
        .iter()
        .zip(max)
        .map(|(&a, &b)| u16::try_from(lo).unwrap_or(u16::MAX).clamp(a, b))
        .collect();
    let mut left = avail.saturating_sub(total(lo));
    for (x, &cap) in w.iter_mut().zip(max) {
        if left == 0 {
            break;
        }
        if *x < cap {
            *x += 1;
            left -= 1;
        }
    }
    w
}

/// Shrink the minima so they sum to `avail`: the widest columns are cut
/// down to a common level (never below 4; the caller checked
/// `avail >= 4n`), so narrow columns keep their words whole.
fn shrink(min: &[u16], avail: u64) -> Vec<u16> {
    let total = |level: u64| -> u64 { min.iter().map(|&m| u64::from(m).min(level).max(4)).sum() };
    let (mut lo, mut hi) = (
        4u64,
        min.iter().map(|&m| u64::from(m)).max().unwrap_or(4).max(4),
    );
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if total(mid) <= avail {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let level = lo;
    let mut w: Vec<u16> = min
        .iter()
        .map(|&m| {
            let x = u64::from(m).min(level).max(4);
            u16::try_from(x).unwrap_or(u16::MAX)
        })
        .collect();
    // Hand out what is left to the columns that were cut, left to right.
    let mut left = avail.saturating_sub(total(level));
    for (x, &m) in w.iter_mut().zip(min) {
        if left == 0 {
            break;
        }
        if *x < m {
            *x += 1;
            left -= 1;
        }
    }
    w
}

/// A wrapped cell: its lines and their pieces.
type Wrapped = (Vec<WrapLine>, Vec<Piece>);

impl Builder<'_> {
    /// A table.
    pub(super) fn table(&mut self, t: &Table) {
        let n = t.align.len();
        let off = self.off;
        let total: usize = t
            .head
            .iter()
            .chain(t.rows.iter().flatten())
            .map(|c| c.text.len())
            .sum();
        if n == 0 {
            self.off = off.saturating_add(to_u32(total));
            return;
        }
        let width = self.avail();
        let head_base = self.sty.el(Element::TableHeader);
        let body_base = self.sty.el(self.text_el);
        let head: Vec<Composed<'_>> = t.head.iter().map(|c| self.compose(c, head_base)).collect();
        let rows: Vec<Vec<Composed<'_>>> = t
            .rows
            .iter()
            .map(|r| r.iter().map(|c| self.compose(c, body_base)).collect())
            .collect();
        let mut row_offs = Vec::with_capacity(t.rows.len());
        let mut acc = off.saturating_add(to_u32(t.head.iter().map(|c| c.text.len()).sum()));
        for r in &t.rows {
            row_offs.push(acc);
            acc = acc.saturating_add(to_u32(r.iter().map(|c| c.text.len()).sum()));
        }
        let show_head = head.iter().any(|c| !c.text.is_empty());
        let amb = self.amb;
        let mut mins = vec![1u16; n];
        let mut maxs = vec![1u16; n];
        let shown_head = if show_head { &head[..] } else { &[] };
        for row in std::iter::once(shown_head).chain(rows.iter().map(Vec::as_slice)) {
            for (c, cell) in row.iter().enumerate() {
                if let (Some(lo), Some(hi)) = (mins.get_mut(c), maxs.get_mut(c)) {
                    *lo = (*lo).max(cell.min_width(amb));
                    *hi = (*hi).max(cell.natural_width(amb));
                }
            }
        }
        let overhead = to_u16(3 * n + 1);
        let widths = if overhead < width {
            allocate_columns(&mins, &maxs, width - overhead)
        } else {
            None
        };
        match widths {
            Some(widths) => self.grid(t, &head, &rows, &widths, show_head, off, &row_offs),
            None => self.records(t, &rows, width, off, &row_offs),
        }
        self.off = off.saturating_add(to_u32(total));
    }

    /// Zebra stripe background, when drawn.
    fn zebra(&self) -> Option<Color> {
        let bg = self.sty.of(Element::TableZebra).bg;
        (self.opts.tables.zebra
            && self.sty.depth() == ColorDepth::TrueColor
            && bg != Color::Default)
            .then_some(bg)
    }

    #[allow(clippy::too_many_arguments)]
    fn grid(
        &mut self,
        t: &Table,
        head: &[Composed<'_>],
        rows: &[Vec<Composed<'_>>],
        widths: &[u16],
        show_head: bool,
        off: u32,
        row_offs: &[u32],
    ) {
        let b = self.deco.table;
        let border = if b.none {
            StyleId(0)
        } else {
            self.sty.el(Element::TableBorder)
        };
        let head_style = self.sty.el(Element::TableHeader);
        let zebra = self.zebra();
        let wrap_row = |me: &mut Self, cells: &[Composed<'_>]| -> Vec<Wrapped> {
            cells
                .iter()
                .zip(widths)
                .map(|(cell, &w)| {
                    let mut lines = Vec::new();
                    let mut pieces = Vec::new();
                    me.wrap_composed(cell, w, w, &mut lines, &mut pieces);
                    (lines, pieces)
                })
                .collect()
        };
        let head_wrapped = wrap_row(self, head);
        let rows_wrapped: Vec<Vec<Wrapped>> = rows.iter().map(|r| wrap_row(self, r)).collect();
        let multi = rows_wrapped
            .iter()
            .any(|r| r.iter().any(|(lines, _)| lines.len() > 1));
        let separators = zebra.is_none() && multi && !b.none;
        if !b.none {
            self.border_line([b.top_left, b.down, b.top_right], widths, border, off);
        }
        if show_head {
            self.table_row(head, &head_wrapped, widths, &t.align, head_style, None, off);
            if !b.none && !rows.is_empty() {
                self.border_line([b.right, b.cross, b.left], widths, border, off);
            }
        }
        for (r, (cells, wrapped)) in rows.iter().zip(&rows_wrapped).enumerate() {
            let row_off = row_offs.get(r).copied().unwrap_or(off);
            if r > 0 && separators {
                self.border_line([b.right, b.cross, b.left], widths, border, row_off);
            }
            let stripe = zebra.filter(|_| r % 2 == 1);
            self.table_row(
                cells,
                wrapped,
                widths,
                &t.align,
                StyleId(0),
                stripe,
                row_off,
            );
        }
        if !b.none {
            let end = row_offs.last().copied().unwrap_or(off);
            self.border_line([b.bottom_left, b.up, b.bottom_right], widths, border, end);
        }
    }

    /// A horizontal border: `[left, junction, right]`.
    fn border_line(&mut self, ends: [&str; 3], widths: &[u16], style: StyleId, off: u32) {
        let [left, mid, right] = ends;
        let h = self.deco.table.horizontal;
        self.begin();
        self.put_glyph(left, style);
        for (i, &w) in widths.iter().enumerate() {
            self.repeat(h, w.saturating_add(2), style);
            let junction = if i + 1 == widths.len() { right } else { mid };
            self.put_glyph(junction, style);
        }
        self.end(LineKind::Table, Fill::None, off);
    }

    /// One table row: its cells side by side, top-aligned.
    #[allow(clippy::too_many_arguments)]
    fn table_row(
        &mut self,
        cells: &[Composed<'_>],
        wrapped: &[Wrapped],
        widths: &[u16],
        align: &[Option<HAlign>],
        pad: StyleId,
        stripe: Option<Color>,
        off: u32,
    ) {
        let b = self.deco.table;
        let border = if b.none {
            StyleId(0)
        } else {
            self.sty.el(Element::TableBorder)
        };
        let height = wrapped
            .iter()
            .map(|(lines, _)| lines.len())
            .max()
            .unwrap_or(1)
            .max(1);
        for v in 0..height {
            self.begin();
            self.put_glyph(b.vertical, border);
            for (c, &w) in widths.iter().enumerate() {
                if let Some(bg) = stripe {
                    self.push_ctx(Ctx {
                        bg: Some(bg),
                        attrs: crate::style::Attrs::empty(),
                    });
                }
                let (lines, pieces) = match wrapped.get(c) {
                    Some((l, p)) => (&l[..], &p[..]),
                    None => (&[][..], &[][..]),
                };
                let cols = lines.get(v).map_or(0, |l| l.cols).min(w);
                let left = match align.get(c).copied().flatten() {
                    Some(HAlign::Right) => w - cols,
                    Some(HAlign::Center) => (w - cols) / 2,
                    _ => 0,
                };
                self.spaces(1 + left, pad);
                let before = self.cols();
                if let Some(cell) = cells.get(c) {
                    self.cell_line(cell, lines, pieces, v);
                }
                let used = self.cols() - before;
                self.spaces(w.saturating_sub(left + used) + 1, pad);
                if stripe.is_some() {
                    self.pop_ctx();
                }
                self.put_glyph(b.vertical, border);
            }
            self.end(LineKind::Table, Fill::None, off);
        }
    }

    /// Rows as records: `header: value` lines, a rule between rows.
    fn records(
        &mut self,
        t: &Table,
        rows: &[Vec<Composed<'_>>],
        width: u16,
        off: u32,
        row_offs: &[u32],
    ) {
        let strong = self.sty.el(Element::Strong);
        let muted = self.sty.el(Element::Muted);
        let border = self.sty.el(Element::TableBorder);
        let labels: Vec<Composed<'_>> = t
            .head
            .iter()
            .enumerate()
            .map(|(i, h)| {
                if h.is_empty() {
                    self.plain_composed(&(i + 1).to_string(), strong)
                } else {
                    self.compose(h, strong)
                }
            })
            .collect();
        let sep = self.plain_composed(": ", muted);
        let look = Look {
            hang: 2,
            ..Look::TEXT
        };
        if rows.is_empty() {
            for label in &labels {
                self.emit(label, width, &look, off);
            }
            return;
        }
        let rule = self.deco.h2_light;
        for (r, row) in rows.iter().enumerate() {
            let row_off = row_offs.get(r).copied().unwrap_or(off);
            if r > 0 {
                self.begin();
                self.repeat(rule, width.min(8), border);
                self.end(LineKind::Table, Fill::None, row_off);
            }
            for (c, cell) in row.iter().enumerate() {
                let joined = match labels.get(c) {
                    Some(label) => Composed::concat(&[label, &sep, cell]),
                    None => Composed::concat(&[cell]),
                };
                self.emit(&joined, width, &look, row_off);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alloc(mins: &[u16], maxs: &[u16], avail: u16) -> Option<Vec<u16>> {
        allocate_columns(mins, maxs, avail)
    }

    #[test]
    fn natural_widths_when_they_fit() {
        assert_eq!(alloc(&[4, 1], &[10, 3], 20), Some(vec![10, 3]));
        assert_eq!(alloc(&[], &[], 5), Some(vec![]));
        // Empty columns are one wide.
        assert_eq!(alloc(&[0], &[0], 5), Some(vec![1]));
    }

    #[test]
    fn water_filling_favours_narrow_columns() {
        // Short column keeps its natural 3; the long ones share the rest.
        let w = alloc(&[4, 1, 5], &[40, 3, 50], 30).unwrap();
        assert_eq!(w.iter().sum::<u16>(), 30);
        assert_eq!(w[1], 3);
        assert!(w[0] >= 4 && w[2] >= 5);
        assert!(w[0].abs_diff(w[2]) <= 1, "{w:?}");
    }

    #[test]
    fn shrinking_cuts_the_widest_columns() {
        // Minima capped at 8 do not fit 14: shrink to a level, floor 4.
        let w = alloc(&[30, 30, 30], &[60, 60, 60], 14).unwrap();
        assert_eq!(w.iter().sum::<u16>(), 14);
        assert!(w.iter().all(|&x| x >= 4), "{w:?}");
        // A short column keeps its word; the wide one gives way.
        let w = alloc(&[6, 6, 5, 11], &[11, 9, 5, 11], 23).unwrap();
        assert_eq!(w, vec![6, 6, 5, 6]);
    }

    #[test]
    fn records_when_too_narrow() {
        assert_eq!(alloc(&[10, 10, 10], &[20, 20, 20], 11), None);
    }

    #[test]
    fn minima_are_capped() {
        // A 50-column word counts as max(8, 40 / 2) = 20.
        let w = alloc(&[50, 5], &[50, 5], 40).unwrap();
        assert_eq!(w, vec![35, 5]);
    }

    proptest::proptest! {
        #[test]
        fn widths_always_fit(
            cols in proptest::collection::vec((0u16..80, 0u16..120), 1..12),
            avail in 0u16..200,
        ) {
            let mins: Vec<u16> = cols.iter().map(|c| c.0).collect();
            let maxs: Vec<u16> = cols.iter().map(|c| c.1).collect();
            if let Some(w) = allocate_columns(&mins, &maxs, avail) {
                proptest::prop_assert_eq!(w.len(), cols.len());
                proptest::prop_assert!(w.iter().map(|&x| u32::from(x)).sum::<u32>() <= u32::from(avail));
                proptest::prop_assert!(w.iter().all(|&x| x >= 1));
            } else {
                proptest::prop_assert!(u32::from(avail) < 4 * cols.len() as u32);
            }
        }
    }
}
