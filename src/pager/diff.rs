//! Painting frames: only the rows that changed, in one write.
//!
//! [`Screen`] remembers the row hashes of the last frame it painted. A new
//! [`Frame`] becomes one byte string:
//!
//! ```text
//! ESC[?2026h                                   synchronized output on
//! <before>                                     kitty uploads and deletions
//! ESC[1;{h-1}r ESC[{k}S|T ESC[r                the scroll fast path, if it pays
//! ESC[{r};1H <line bytes> ESC[0m ESC[K         each changed row …
//! ESC[{r};{c}H <overlay bytes>                 … and what is drawn over it
//! <after>                                      pixel images, kitty placements
//! ESC[?2026l                                   synchronized output off
//! ```
//!
//! The fast path: when the document scrolled by `k` rows and nothing is
//! drawn over it (no overlay, no pixel image that would not move with the
//! text), the terminal scrolls the document rows itself (a scroll region
//! that leaves the status bar alone) and only the `k` exposed rows are
//! drawn — about one line of bytes per line scrolled.
//!
//! Line bytes come from [`Emitter`] (with the image layer's figure rows,
//! [`Extras::images`]); `ESC[K` clears the rest of a row unless the line
//! fills it (erasing from the last column would erase the character just
//! written there).
//!
//! The image layer works in two steps around the painter: [`Screen::diff`]
//! says which rows a frame writes (so it knows which pixel images the
//! frame erases), and [`Screen::write`] then encodes the frame with the
//! image bytes it decided on. [`Screen::paint`] does both for frames
//! without images.

use std::io::Write as _;
use std::ops::Range;

use crate::ir::Document;
use crate::layout::{Fill, Layout};
use crate::render::sgr::RESET;
use crate::render::{Emitter, ImageRows, RenderConfig};

use super::view::{Body, Frame};

/// Synchronized output on.
pub const SYNC_ON: &[u8] = b"\x1b[?2026h";
/// Synchronized output off.
pub const SYNC_OFF: &[u8] = b"\x1b[?2026l";
/// Erase to the end of the line.
const ERASE_LINE: &[u8] = b"\x1b[K";

/// What the terminal shows, as far as the painter knows.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    /// Hash of each row; `None` where the content is unknown.
    rows: Vec<Option<u64>>,
    cols: u16,
    top: usize,
    generation: u64,
    overlay: bool,
    images: bool,
}

/// The rows a frame writes, from [`Screen::diff`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Diff {
    /// Screen rows to write, top to bottom.
    pub(crate) dirty: Vec<usize>,
    /// The scroll fast path: the scroll region's height and the rows
    /// scrolled (positive: the content moves up).
    pub(crate) scroll: Option<(usize, isize)>,
}

impl Diff {
    /// Whether screen row `row` is written.
    pub(crate) fn writes(&self, row: usize) -> bool {
        self.dirty.binary_search(&row).is_ok()
    }
}

/// What a frame carries besides its rows: the image layer's part.
#[derive(Clone, Copy, Default)]
pub(crate) struct Extras<'a> {
    /// What figure rows show (without it, their placeholder boxes).
    pub(crate) images: Option<&'a dyn ImageRows>,
    /// Written before the rows: kitty uploads and deletions.
    pub(crate) before: &'a [u8],
    /// Written after the rows and overlays: pixel images, kitty
    /// placements.
    pub(crate) after: &'a [u8],
}

impl Screen {
    /// A screen whose content is unknown.
    pub fn new() -> Screen {
        Screen::default()
    }

    /// Forget what is on screen: the next frame is painted in full.
    pub fn invalidate(&mut self) {
        self.rows.clear();
    }

    /// Forget the rows that show document lines `lines` (their figure
    /// changed): the next frame writes them again.
    pub(crate) fn invalidate_lines(&mut self, lines: Range<usize>) {
        // The last row is the status bar, never a document line.
        let doc_rows = self.rows.len().saturating_sub(1);
        let from = lines.start.max(self.top);
        let to = lines.end.min(self.top.saturating_add(doc_rows));
        for line in from..to {
            if let Some(row) = self.rows.get_mut(line - self.top) {
                *row = None;
            }
        }
    }

    /// The bytes that turn the screen into `frame` (empty when nothing
    /// changed). `doc` and `layout` are what the frame's document rows
    /// refer to; `cfg` encodes them.
    pub fn paint(
        &mut self,
        frame: &Frame,
        doc: &Document,
        layout: &Layout,
        cfg: &RenderConfig,
    ) -> Vec<u8> {
        let diff = self.diff(frame);
        self.write(frame, &diff, doc, layout, cfg, Extras::default())
    }

    /// Which rows `frame` changes, and whether the document scrolled; the
    /// screen then counts `frame` as painted.
    pub(crate) fn diff(&mut self, frame: &Frame) -> Diff {
        let n = frame.lines.len();
        let known = self.rows.len() == n && self.cols == frame.cols;
        let mut old: Vec<Option<u64>> = if known {
            self.rows.clone()
        } else {
            vec![None; n]
        };
        let scroll = if known {
            self.scroll_by(frame, &mut old)
        } else {
            None
        };
        let dirty = (0..n)
            .filter(|&i| old.get(i).copied().flatten() != frame.lines.get(i).map(|r| r.hash))
            .collect();
        self.rows = frame.lines.iter().map(|r| Some(r.hash)).collect();
        self.cols = frame.cols;
        self.top = frame.top;
        self.generation = frame.generation;
        self.overlay = frame.overlay;
        self.images = frame.images;
        Diff { dirty, scroll }
    }

    /// The bytes of `frame` as `diff` says, with the image layer's
    /// `extras`; empty when there is nothing to write.
    pub(crate) fn write(
        &self,
        frame: &Frame,
        diff: &Diff,
        doc: &Document,
        layout: &Layout,
        cfg: &RenderConfig,
        extras: Extras<'_>,
    ) -> Vec<u8> {
        if diff.dirty.is_empty()
            && diff.scroll.is_none()
            && extras.before.is_empty()
            && extras.after.is_empty()
        {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(
            512 + diff.dirty.len() * 128 + extras.before.len() + extras.after.len(),
        );
        out.extend_from_slice(SYNC_ON);
        out.extend_from_slice(extras.before);
        if let Some((region, delta)) = diff.scroll {
            let k = delta.unsigned_abs();
            let dir = if delta > 0 { 'S' } else { 'T' };
            let _ = write!(out, "\x1b[1;{region}r\x1b[{k}{dir}\x1b[r");
        }
        let mut emitter: Option<Emitter<'_>> = None;
        for &i in &diff.dirty {
            let Some(row) = frame.lines.get(i) else {
                continue;
            };
            let _ = write!(out, "\x1b[{};1H", i + 1);
            match &row.body {
                Body::Line { index, marks } => {
                    let e = emitter.get_or_insert_with(|| {
                        let e = Emitter::new(doc, layout, cfg);
                        match extras.images {
                            Some(images) => e.with_images(images),
                            None => e,
                        }
                    });
                    let start = out.len();
                    e.write_line_marked(*index, marks, &mut out);
                    if out.last() == Some(&b'\n') {
                        out.pop();
                    }
                    if !out.get(start..).is_some_and(|b| b.ends_with(RESET)) {
                        out.extend_from_slice(RESET);
                    }
                    if row_end(layout, *index) < usize::from(frame.cols) {
                        out.extend_from_slice(ERASE_LINE);
                    }
                }
                Body::Blank => {
                    out.extend_from_slice(RESET);
                    out.extend_from_slice(ERASE_LINE);
                }
                Body::Text(bytes) => out.extend_from_slice(bytes),
            }
            for seg in &row.overlays {
                let _ = write!(out, "\x1b[{};{}H", i + 1, u32::from(seg.col) + 1);
                out.extend_from_slice(&seg.bytes);
            }
        }
        out.extend_from_slice(extras.after);
        out.extend_from_slice(SYNC_OFF);
        out
    }

    /// Use the scroll fast path if it saves rows: returns the scroll
    /// region's height and the rows scrolled (positive: content moves up),
    /// with `old` shifted to match.
    fn scroll_by(&self, frame: &Frame, old: &mut [Option<u64>]) -> Option<(usize, isize)> {
        let region = frame.lines.len().checked_sub(1)?;
        if region < 2
            || self.generation != frame.generation
            || self.overlay
            || frame.overlay
            || self.images
            || frame.images
        {
            return None;
        }
        let delta = isize::try_from(frame.top).ok()? - isize::try_from(self.top).ok()?;
        let k = delta.unsigned_abs();
        if delta == 0 || k >= region {
            return None;
        }
        let mut shifted = vec![None; region];
        for (i, slot) in shifted.iter_mut().enumerate() {
            let from = if delta > 0 {
                i.checked_add(k)
            } else {
                i.checked_sub(k)
            };
            *slot = from
                .filter(|&f| f < region)
                .and_then(|f| old.get(f).copied().flatten());
        }
        let hash = |i: usize| frame.lines.get(i).map(|r| r.hash);
        let kept_shifted = (0..region)
            .filter(|&i| shifted.get(i).copied().flatten() == hash(i))
            .count();
        let kept_plain = (0..region)
            .filter(|&i| old.get(i).copied().flatten() == hash(i))
            .count();
        if kept_shifted <= kept_plain {
            return None;
        }
        old.get_mut(..region)?.copy_from_slice(&shifted);
        Some((region, delta))
    }
}

/// The column where line `index` ends (indent included).
fn row_end(layout: &Layout, index: usize) -> usize {
    let Some(line) = layout.lines.get(index) else {
        return 0;
    };
    if line.n == 0 && line.fill == Fill::None {
        return 0;
    }
    let fill = match line.fill {
        Fill::None => 0,
        Fill::Panel { to_col, .. } => to_col,
        Fill::Gradient { x1, .. } => x1,
    };
    usize::from(layout.indent) + usize::from(line.cols.max(fill))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pager::view::Row;

    /// A frame of `rows` rows, the last one the status bar, showing
    /// document lines from `top` (row hashes are just the line numbers).
    fn frame(rows: usize, top: usize, images: bool) -> Frame {
        let mut lines: Vec<Row> = (0..rows - 1)
            .map(|i| Row {
                hash: (top + i) as u64,
                body: Body::Blank,
                overlays: Vec::new(),
            })
            .collect();
        lines.push(Row {
            hash: 9999,
            body: Body::Text(b"status".to_vec()),
            overlays: Vec::new(),
        });
        Frame {
            cols: 20,
            rows: u16::try_from(rows).unwrap(),
            top,
            generation: 1,
            lines,
            overlay: false,
            images,
        }
    }

    fn empty() -> (Document, Layout) {
        let doc = Document::default();
        let layout = crate::layout::layout(
            &doc,
            20,
            &crate::theme::Theme::test(),
            &crate::term::Caps::full(),
            &crate::options::RenderOptions::default(),
            &crate::highlight::PlainHighlighter,
            &crate::layout::NoImages,
        );
        (doc, layout)
    }

    #[test]
    fn invalidated_lines_are_written_again() {
        let mut s = Screen::new();
        assert_eq!(s.diff(&frame(6, 10, false)).dirty, [0, 1, 2, 3, 4, 5]);
        assert!(s.diff(&frame(6, 10, false)).dirty.is_empty());
        // Lines 12..14 are rows 2 and 3; lines off screen change nothing.
        s.invalidate_lines(12..14);
        s.invalidate_lines(0..5);
        s.invalidate_lines(40..50);
        let d = s.diff(&frame(6, 10, false));
        assert_eq!(d.dirty, [2, 3]);
        assert!(d.writes(3) && !d.writes(4));
        // Never the status bar.
        s.invalidate_lines(0..usize::MAX);
        assert_eq!(s.diff(&frame(6, 10, false)).dirty, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn pixel_images_turn_the_fast_path_off() {
        let mut s = Screen::new();
        s.diff(&frame(10, 0, false));
        assert!(s.diff(&frame(10, 1, false)).scroll.is_some());
        assert_eq!(s.diff(&frame(10, 2, true)).scroll, None, "visible now");
        assert_eq!(s.diff(&frame(10, 3, false)).scroll, None, "visible before");
        assert!(s.diff(&frame(10, 4, false)).scroll.is_some());
    }

    #[test]
    fn extras_frame_the_rows() {
        let (doc, layout) = empty();
        let cfg = RenderConfig::plain();
        let mut s = Screen::new();
        let f = frame(4, 0, false);
        let d = s.diff(&f);
        let extras = Extras {
            images: None,
            before: b"<upload>",
            after: b"<pixels>",
        };
        let out = s.write(&f, &d, &doc, &layout, &cfg, extras);
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b[?2026h<upload>\x1b[1;1H"), "{text:?}");
        assert!(text.ends_with("status<pixels>\x1b[?2026l"), "{text:?}");
        // No rows to write, only image bytes: still a frame.
        let d = s.diff(&f);
        assert!(d.dirty.is_empty());
        let only = Extras {
            after: b"<pixels>",
            ..Extras::default()
        };
        assert_eq!(
            s.write(&f, &d, &doc, &layout, &cfg, only),
            b"\x1b[?2026h<pixels>\x1b[?2026l"
        );
        assert!(
            s.write(&f, &d, &doc, &layout, &cfg, Extras::default())
                .is_empty()
        );
    }
}
