//! The outline: headings for the overlay and the status bar breadcrumb,
//! and the geometry of the overlay boxes (shared by input handling, for
//! mouse clicks, and by the view).

use crate::ir::{Document, HeadingId};

use super::search::fold;
use super::state::Derived;

/// Widest overlay box.
const MAX_BOX_WIDTH: u16 = 72;
/// Narrowest overlay box (unless the terminal is narrower).
const MIN_BOX_WIDTH: u16 = 24;

/// Headings shown in the document whose title contains `filter`
/// (ignoring case), in document order.
pub(crate) fn filtered(doc: &Document, derived: &Derived, filter: &str) -> Vec<HeadingId> {
    let needle = fold(filter.trim());
    derived
        .headings
        .iter()
        .map(|&(_, h)| h)
        .filter(|&h| {
            needle.is_empty()
                || doc
                    .heading(h)
                    .is_some_and(|x| fold(&x.title).contains(needle.as_ref()))
        })
        .collect()
}

/// Titles from the outermost section down to heading `h`.
pub(crate) fn breadcrumb(doc: &Document, h: HeadingId) -> Vec<&str> {
    let mut out = Vec::new();
    let mut cur = Some(h);
    while let Some(id) = cur {
        let Some(heading) = doc.heading(id) else {
            break;
        };
        out.push(&*heading.title);
        // Parents always come earlier: this ends.
        cur = heading.parent.filter(|p| p.0 < id.0);
    }
    out.reverse();
    out
}

/// The lowest heading level among `items` (their indentation starts
/// there).
pub(crate) fn base_level(doc: &Document, items: &[HeadingId]) -> u8 {
    items
        .iter()
        .filter_map(|&h| doc.heading(h).map(|x| x.level))
        .min()
        .unwrap_or(1)
}

/// A box on screen (0-based cell coordinates, border included).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BoxGeom {
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) w: u16,
    pub(crate) h: u16,
}

impl BoxGeom {
    /// Rows inside the border.
    pub(crate) fn inner_rows(&self) -> usize {
        usize::from(self.h.saturating_sub(2))
    }

    /// Columns inside the border.
    pub(crate) fn inner_cols(&self) -> usize {
        usize::from(self.w.saturating_sub(2))
    }

    /// Whether the cell `(col, row)` is inside the box.
    pub(crate) fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x
            && row >= self.y
            && u32::from(col) < u32::from(self.x) + u32::from(self.w)
            && u32::from(row) < u32::from(self.y) + u32::from(self.h)
    }
}

/// A centred box `lines` rows tall inside the document area of a `cols` ×
/// `rows` terminal (the last row is the status bar).
fn centred(cols: u16, rows: u16, lines: usize) -> BoxGeom {
    let area = rows.saturating_sub(1);
    let want = u16::try_from(lines.saturating_add(2)).unwrap_or(u16::MAX);
    let h = want.min(area);
    let w = if cols < MIN_BOX_WIDTH + 4 {
        cols
    } else {
        cols.saturating_sub(4).clamp(MIN_BOX_WIDTH, MAX_BOX_WIDTH)
    };
    let x = (cols - w) / 2;
    let y = if area > h + 1 { 1 } else { 0 };
    BoxGeom { x, y, w, h }
}

/// The outline box for `items` entries: a filter row, then the entries.
pub(crate) fn outline_box(cols: u16, rows: u16, items: usize) -> BoxGeom {
    centred(cols, rows, items.max(1).saturating_add(1))
}

/// Entries the outline box shows at once.
pub(crate) fn outline_rows(g: &BoxGeom) -> usize {
    g.inner_rows().saturating_sub(1)
}

/// The help box for `lines` lines of help.
pub(crate) fn help_box(cols: u16, rows: u16, lines: usize) -> BoxGeom {
    centred(cols, rows, lines)
}

/// The first entry to show so that `selected` is visible in `rows` rows.
pub(crate) fn scroll_to(scroll: usize, selected: usize, rows: usize) -> usize {
    let rows = rows.max(1);
    if selected < scroll {
        selected
    } else if selected >= scroll + rows {
        selected + 1 - rows
    } else {
        scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::PlainHighlighter;
    use crate::layout::{NoImages, layout};
    use crate::options::RenderOptions;
    use crate::parse::{ParseOptions, parse};
    use crate::term::Caps;
    use crate::theme::Theme;

    #[test]
    fn filtering_and_breadcrumbs() {
        let doc = parse(
            "# Guide\n\n## Install\n\n### From source\n\n## Usage\n\n### Install again",
            &ParseOptions::default(),
        );
        let l = layout(
            &doc,
            60,
            &Theme::test(),
            &Caps::full(),
            &RenderOptions::default(),
            &PlainHighlighter,
            &NoImages,
        );
        let d = Derived::new(&l);
        assert_eq!(filtered(&doc, &d, "").len(), 5);
        let inst = filtered(&doc, &d, "INST");
        assert_eq!(inst, [HeadingId(1), HeadingId(4)]);
        assert_eq!(
            breadcrumb(&doc, HeadingId(2)),
            ["Guide", "Install", "From source"]
        );
        assert_eq!(breadcrumb(&doc, HeadingId(9)), Vec::<&str>::new());
        assert_eq!(base_level(&doc, &inst), 2);
        let line = l.heading_line[2] as usize;
        assert_eq!(d.section_at(line), Some(HeadingId(2)));
        assert_eq!(d.section_at(line + 1), Some(HeadingId(2)));
    }

    #[test]
    fn boxes_fit_the_screen() {
        for (cols, rows, n) in [(80, 24, 5), (80, 24, 500), (20, 5, 3), (1, 1, 9), (0, 0, 0)] {
            let g = outline_box(cols, rows, n);
            assert!(g.x + g.w <= cols, "{g:?}");
            assert!(g.y + g.h <= rows.saturating_sub(1), "{g:?}");
        }
        let g = outline_box(80, 24, 5);
        assert_eq!((g.w, g.h), (72, 8));
        assert_eq!(outline_rows(&g), 5);
        assert!(g.contains(g.x, g.y));
        assert!(!g.contains(g.x + g.w, g.y));
        assert_eq!(scroll_to(0, 7, 5), 3);
        assert_eq!(scroll_to(4, 2, 5), 2);
        assert_eq!(scroll_to(2, 3, 5), 2);
    }
}
