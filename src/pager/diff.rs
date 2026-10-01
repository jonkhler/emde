//! Painting frames: only the rows that changed, in one write.
//!
//! [`Screen`] remembers the row hashes of the last frame it painted. A new
//! [`Frame`] becomes one byte string:
//!
//! ```text
//! ESC[?2026h                                   synchronized output on
//! ESC[1;{h-1}r ESC[{k}S|T ESC[r                the scroll fast path, if it pays
//! ESC[{r};1H <line bytes> ESC[0m ESC[K         each changed row …
//! ESC[{r};{c}H <overlay bytes>                 … and what is drawn over it
//! ESC[?2026l                                   synchronized output off
//! ```
//!
//! The fast path: when the document scrolled by `k` rows and nothing is
//! drawn over it (no overlay, no pixel image that would not move with the
//! text), the terminal scrolls the document rows itself (a scroll region
//! that leaves the status bar alone) and only the `k` exposed rows are
//! drawn — about one line of bytes per line scrolled.
//!
//! Line bytes come from [`Emitter`]; `ESC[K` clears the rest of a row
//! unless the line fills it (erasing from the last column would erase the
//! character just written there).

use std::io::Write as _;

use crate::ir::Document;
use crate::layout::{Fill, Layout};
use crate::render::sgr::RESET;
use crate::render::{Emitter, RenderConfig};

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

impl Screen {
    /// A screen whose content is unknown.
    pub fn new() -> Screen {
        Screen::default()
    }

    /// Forget what is on screen: the next frame is painted in full.
    pub fn invalidate(&mut self) {
        self.rows.clear();
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
        let dirty: Vec<usize> = (0..n)
            .filter(|&i| old.get(i).copied().flatten() != frame.lines.get(i).map(|r| r.hash))
            .collect();
        self.rows = frame.lines.iter().map(|r| Some(r.hash)).collect();
        self.cols = frame.cols;
        self.top = frame.top;
        self.generation = frame.generation;
        self.overlay = frame.overlay;
        self.images = frame.images;
        if dirty.is_empty() && scroll.is_none() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(512 + dirty.len() * 128);
        out.extend_from_slice(SYNC_ON);
        if let Some((region, delta)) = scroll {
            let k = delta.unsigned_abs();
            let dir = if delta > 0 { 'S' } else { 'T' };
            let _ = write!(out, "\x1b[1;{region}r\x1b[{k}{dir}\x1b[r");
        }
        let mut emitter: Option<Emitter<'_>> = None;
        for i in dirty {
            let Some(row) = frame.lines.get(i) else {
                continue;
            };
            let _ = write!(out, "\x1b[{};1H", i + 1);
            match &row.body {
                Body::Line { index, marks } => {
                    let e = emitter.get_or_insert_with(|| Emitter::new(doc, layout, cfg));
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
