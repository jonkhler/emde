//! Figure geometry (plan §5): the cell box of an image, computed from its
//! header size alone, and the pixel rows behind partly visible cell rows.
//!
//! The box is fixed before any pixel is decoded, and the placeholder box,
//! the block raster and every pixel protocol use exactly that box, so
//! nothing reflows when an image arrives or the image mode changes.

use std::ops::Range;

use crate::options::Height;

/// Cell size assumed when the terminal does not report one: a common 1:2
/// cell at one image pixel per screen pixel.
pub const DEFAULT_CELL_PX: (u16, u16) = (8, 16);

/// Figure height cap in stream mode, where there is no screen to relate
/// a percentage to.
pub const STREAM_MAX_ROWS: u16 = 30;

/// The height cap for figures: `limit` resolved against the pager's screen
/// height, or [`STREAM_MAX_ROWS`] in stream mode (`screen_rows` = `None`),
/// where a smaller row limit still applies. Never below 1.
pub fn max_rows(limit: Height, screen_rows: Option<u16>) -> u16 {
    let rows = match (limit, screen_rows) {
        (Height::Rows(n), Some(_)) => n,
        (Height::Percent(p), Some(screen)) => {
            let p = u32::from(p.min(100));
            u16::try_from(u32::from(screen) * p / 100).unwrap_or(u16::MAX)
        }
        (Height::Rows(n), None) => n.min(STREAM_MAX_ROWS),
        (Height::Percent(_), None) => STREAM_MAX_ROWS,
    };
    rows.max(1)
}

/// The cell box of an image of `px` pixels:
/// `cols = min(measure, px_w / cell_w)` and
/// `rows = cols · (cell_w / cell_h) · (px_h / px_w)`, both rounded and at
/// least 1. When `rows` exceeds `max_rows` the box is shrunk to that height,
/// keeping the aspect ratio. `cell_px` defaults to [`DEFAULT_CELL_PX`].
/// A zero-sized image or a zero limit gives `(0, 0)`.
pub fn figure_cells(
    px: (u32, u32),
    cell_px: Option<(u16, u16)>,
    measure: u16,
    max_rows: u16,
) -> (u16, u16) {
    let (w, h) = (f64::from(px.0), f64::from(px.1));
    let (cw, ch) = match cell_px {
        Some((cw, ch)) if cw > 0 && ch > 0 => (f64::from(cw), f64::from(ch)),
        _ => (f64::from(DEFAULT_CELL_PX.0), f64::from(DEFAULT_CELL_PX.1)),
    };
    if w == 0.0 || h == 0.0 || measure == 0 || max_rows == 0 {
        return (0, 0);
    }
    let fit = |v: f64, max: u16| (v.round().max(1.0).min(f64::from(max))) as u16;
    let cols = fit(w / cw, measure);
    let rows_for = |cols: u16| f64::from(cols) * (cw / ch) * (h / w);
    let rows = rows_for(cols).round().max(1.0);
    if rows <= f64::from(max_rows) {
        return (cols, rows as u16);
    }
    let cols = fit(f64::from(max_rows) * (ch / cw) * (w / h), measure);
    (cols, max_rows)
}

/// The largest size with the aspect ratio of `size` that fits in `max`,
/// never larger than `size` itself (each side at least 1 unless the input
/// is empty).
pub fn fit_within(size: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (w, h) = size;
    let (mw, mh) = max;
    if w == 0 || h == 0 || mw == 0 || mh == 0 {
        return (0, 0);
    }
    if w <= mw && h <= mh {
        return size;
    }
    let scale = (f64::from(mw) / f64::from(w)).min(f64::from(mh) / f64::from(h));
    let side = |v: u32, cap: u32| ((f64::from(v) * scale).round() as u32).clamp(1, cap);
    (side(w, mw), side(h, mh))
}

/// The source pixel rows shown by cell rows `visible` of an image of
/// `height` pixels drawn over `rows` cell rows: proportional rows, for kitty
/// classic crops (`y`, `h`) and iTerm2 slices. `None` if nothing is visible.
pub fn visible_pixel_rows(height: u32, rows: u16, visible: Range<u16>) -> Option<Range<u32>> {
    let end = visible.end.min(rows);
    if rows == 0 || height == 0 || visible.start >= end {
        return None;
    }
    let at = |row: u16| {
        let px = u64::from(height) * u64::from(row) / u64::from(rows);
        u32::try_from(px).unwrap_or(height)
    };
    let range = at(visible.start)..at(end);
    (!range.is_empty()).then_some(range)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_formula() {
        // 800×400 px at 8×16 cells: 100 columns, 100 · ½ · ½ = 25 rows.
        assert_eq!(figure_cells((800, 400), Some((8, 16)), 100, 60), (100, 25));
        // The measure caps the width, rows follow.
        assert_eq!(figure_cells((1600, 800), Some((8, 16)), 80, 60), (80, 20));
        // Small images keep their pixel size in cells.
        assert_eq!(figure_cells((64, 64), Some((8, 16)), 100, 60), (8, 4));
        // tmux's fallback cells (16×32) halve both.
        assert_eq!(figure_cells((800, 400), Some((16, 32)), 100, 60), (50, 13));
    }

    #[test]
    fn height_cap_keeps_the_aspect_ratio() {
        // A tall screenshot: 400×2000 px → 50 cols would need 125 rows.
        assert_eq!(figure_cells((400, 2000), Some((8, 16)), 100, 30), (12, 30));
        assert_eq!(
            figure_cells((400, 2000), Some((8, 16)), 100, 200),
            (50, 125)
        );
    }

    #[test]
    fn extremes() {
        assert_eq!(figure_cells((10_000, 3), Some((8, 16)), 100, 30), (100, 1));
        assert_eq!(figure_cells((3, 10_000), Some((8, 16)), 100, 30), (1, 30));
        assert_eq!(figure_cells((1, 1), None, 100, 30), (1, 1));
        assert_eq!(figure_cells((u32::MAX, u32::MAX), None, 100, 30), (60, 30));
        assert_eq!(figure_cells((0, 10), None, 100, 30), (0, 0));
        assert_eq!(figure_cells((10, 10), None, 0, 30), (0, 0));
        assert_eq!(figure_cells((10, 10), None, 10, 0), (0, 0));
        // A zero cell size falls back to the default.
        assert_eq!(
            figure_cells((800, 400), Some((0, 16)), 100, 60),
            figure_cells((800, 400), None, 100, 60)
        );
    }

    #[test]
    fn row_caps() {
        assert_eq!(max_rows(Height::Percent(60), Some(54)), 32);
        assert_eq!(max_rows(Height::Percent(60), None), STREAM_MAX_ROWS);
        assert_eq!(max_rows(Height::Rows(12), Some(54)), 12);
        assert_eq!(max_rows(Height::Rows(80), None), STREAM_MAX_ROWS);
        assert_eq!(max_rows(Height::Rows(12), None), 12);
        assert_eq!(max_rows(Height::Percent(1), Some(10)), 1);
        assert_eq!(max_rows(Height::Rows(0), Some(10)), 1);
        assert_eq!(max_rows(Height::Percent(250), Some(10)), 10);
    }

    #[test]
    fn fitting() {
        assert_eq!(fit_within((4000, 3000), (800, 800)), (800, 600));
        assert_eq!(fit_within((3000, 4000), (800, 800)), (600, 800));
        assert_eq!(fit_within((100, 50), (800, 800)), (100, 50));
        assert_eq!(fit_within((10_000, 1), (100, 100)), (100, 1));
        assert_eq!(fit_within((0, 5), (100, 100)), (0, 0));
        assert_eq!(fit_within((5, 5), (0, 100)), (0, 0));
    }

    #[test]
    fn visible_rows() {
        // 320 px over 10 rows: 32 px per row.
        assert_eq!(visible_pixel_rows(320, 10, 0..10), Some(0..320));
        assert_eq!(visible_pixel_rows(320, 10, 3..5), Some(96..160));
        assert_eq!(visible_pixel_rows(320, 10, 8..20), Some(256..320));
        assert_eq!(visible_pixel_rows(100, 3, 1..2), Some(33..66));
        assert_eq!(visible_pixel_rows(320, 10, 10..12), None);
        assert_eq!(visible_pixel_rows(320, 10, 4..4), None);
        assert_eq!(visible_pixel_rows(0, 10, 0..2), None);
        assert_eq!(visible_pixel_rows(320, 0, 0..2), None);
        // More rows than pixels: some rows map to no pixel.
        assert_eq!(visible_pixel_rows(2, 10, 0..1), None);
    }
}
