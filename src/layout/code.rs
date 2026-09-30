//! Code blocks: a panel, a frame or a gutter bar around tab-expanded,
//! wrapped (or clipped) code, with the highlight overlay prepared for the
//! emitter.
//!
//! ```text
//! panel (256 colours and up)        frame (below)            gutter
//!   hello.rs                 rust   ┌─ hello.rs ──── rust ─┐  ▎ fn main() {
//!   fn main() {                     │ fn main() {          │  ▎     body();
//!       println!("… long line       │     println!("… long │  ▎↳ wrapped
//!  ↳ wrapped");                     │↳ wrapped");          │
//!   }                               │ }                    │
//!                                   └──────────────────────┘
//! ```
//!
//! Highlighting runs through [`crate::panic::guarded`], once per block, and
//! its byte offsets are moved into the tab-expanded lines, so the emitter
//! can paint them over the spans flagged [`SpanFlags::CODE`].

use std::ops::Range;

use crate::color::mix_oklab;
use crate::highlight::{HlBlock, HlSpan};
use crate::ir::CodeBlock;
use crate::options::CodeStyle;
use crate::panic::guarded;
use crate::style::{Color, StyleId};
use crate::term::ColorDepth;
use crate::text::tabs::Expanded;
use crate::text::width::next_grapheme_end;
use crate::text::{expand_tabs, grapheme_width, str_width};
use crate::theme::Element;

use super::build::{Builder, cut_to_cols};
use super::{CodeInfo, Fill, LineKind, SpanFlags};

/// How much of the diff colour tints a `+`/`-` line's background.
const DIFF_TINT: f32 = 0.18;

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// How a code block is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Panel,
    Frame,
    Gutter,
    /// No decorations at all (terminals too narrow for any).
    Bare,
}

/// A `diff` line's sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sign {
    Added,
    Removed,
}

/// Styles of one code block.
#[derive(Clone, Copy)]
struct Look {
    mode: Mode,
    panel: StyleId,
    gutter: StyleId,
    label: StyleId,
    frame: StyleId,
    added: StyleId,
    removed: StyleId,
}

impl Builder<'_> {
    fn code_mode(&self) -> Mode {
        match self.opts.code.style {
            CodeStyle::Panel => Mode::Panel,
            CodeStyle::Frame => Mode::Frame,
            CodeStyle::Gutter => Mode::Gutter,
            CodeStyle::Auto if self.sty.depth() >= ColorDepth::Ansi256 => Mode::Panel,
            CodeStyle::Auto => Mode::Frame,
        }
    }

    /// The styles of a block drawn in `mode`. Only a panel has the
    /// background: in a frame, a gutter or bare, the background would sit
    /// behind the characters alone and leave ragged edges.
    fn code_look(&mut self, mode: Mode) -> Look {
        let block = self.sty.el(Element::CodeBlock);
        let panel_bg = self.sty.of(Element::CodeBlock).bg;
        let panel = if mode == Mode::Panel {
            block
        } else {
            self.sty.with_bg(block, Color::Default)
        };
        let on_panel = |b: &mut Self, id: StyleId| {
            if mode == Mode::Panel && panel_bg != Color::Default {
                b.sty.with_bg(id, panel_bg)
            } else {
                id
            }
        };
        let gutter = self.sty.el(Element::CodeGutter);
        let gutter = on_panel(self, gutter);
        let label = self.sty.el(Element::CodeLabel);
        let label = on_panel(self, label);
        let frame = self.sty.el(Element::CodeGutter);
        let (added, removed) = self.diff_styles(panel, mode);
        Look {
            mode,
            panel,
            gutter,
            label,
            frame,
            added,
            removed,
        }
    }

    /// Styles of added and removed `diff` lines: a background tint on a
    /// truecolor panel, a foreground colour otherwise (at 256 colours and
    /// below both tints fall on the same palette grey).
    fn diff_styles(&mut self, panel: StyleId, mode: Mode) -> (StyleId, StyleId) {
        let green = self.theme.color("green");
        let red = self.theme.color("red");
        let panel_bg = self.sty.of(Element::CodeBlock).bg;
        let tint = mode == Mode::Panel && self.sty.depth() == ColorDepth::TrueColor;
        let pick = |b: &mut Self, rgb: Option<crate::style::Rgb>| match (rgb, panel_bg) {
            (Some(c), Color::Rgb(bg)) if tint => b
                .sty
                .with_bg(panel, Color::Rgb(mix_oklab(bg, c, DIFF_TINT))),
            (Some(c), _) => b.sty.with_fg(panel, Color::Rgb(c)),
            (None, _) => panel,
        };
        let added = pick(self, green);
        let removed = pick(self, red);
        (added, removed)
    }

    /// Highlight a block (guarded), in source-line offsets.
    fn highlight(&self, cb: &CodeBlock) -> Option<HlBlock> {
        let token = cb.lang.as_deref()?;
        if cb.code.len() > self.opts.code.max_highlight_bytes {
            return None;
        }
        let hl = self.hl;
        let lang = guarded(|| hl.resolve(token)).flatten()?;
        guarded(|| hl.highlight(lang, &cb.code))
    }

    /// A code block. Its rows keep their place in aligned containers
    /// (`<div align="center">`): a panel or frame spans the whole content
    /// width, so centring rows one by one would only tear it apart.
    pub(super) fn code_block(&mut self, cb: &CodeBlock) {
        let off = self.off;
        let width = self.avail();
        let source: Vec<&str> = cb.code.split('\n').collect();
        let tab = self.opts.code.tab_width;
        let amb = self.amb;
        let expanded: Vec<Expanded<'_>> = source.iter().map(|l| expand_tabs(l, tab, amb)).collect();
        let block = to_u32(self.out.code.len());
        let hl = self
            .highlight(cb)
            .map(|h| to_expanded(h, &source, &expanded));
        self.out.code.push(CodeInfo { hl });

        let numbers = self.opts.code.line_numbers;
        let digits = to_u16(source.len().to_string().len());
        let gutter = if numbers { digits + 1 } else { 0 };
        let mode = self.code_mode();
        let (left, right) = match mode {
            Mode::Panel => (1, 1),
            Mode::Frame => (2, 2),
            Mode::Gutter => (2, 0),
            Mode::Bare => (0, 0),
        };
        // Too narrow for decorations: just the code.
        let (mode, left, right, gutter, numbers) = if width < left + right + gutter + 1 {
            (Mode::Bare, 0, 0, 0, false)
        } else {
            (mode, left, right, gutter, numbers)
        };
        let look = self.code_look(mode);
        let content = width.saturating_sub(left + right + gutter).max(1);
        let diff = cb
            .lang
            .as_deref()
            .is_some_and(|l| l.eq_ignore_ascii_case("diff") || l.eq_ignore_ascii_case("patch"));
        let label = if self.opts.code.label {
            cb.lang.as_deref()
        } else {
            None
        };
        self.code_header(&look, width, cb.title.as_deref(), label, off);
        let mut line_start = 0usize;
        for (k, (src, e)) in source.iter().zip(&expanded).enumerate() {
            let sign = if diff { diff_sign(src) } else { None };
            let style = match sign {
                Some(Sign::Added) => look.added,
                Some(Sign::Removed) => look.removed,
                None => look.panel,
            };
            let chunks = if self.opts.code.wrap {
                split_cols(&e.text, content, amb)
            } else {
                std::iter::once(0..e.text.len()).collect()
            };
            for (ci, chunk) in chunks.iter().enumerate() {
                let text = visible_code(e.text.get(chunk.clone()).unwrap_or(""));
                let text = text.as_ref();
                self.begin();
                self.placed();
                let start = self.cols();
                self.code_left(&look, style, ci > 0);
                if numbers {
                    let n = if ci == 0 {
                        format!("{:>w$} ", k + 1, w = usize::from(digits))
                    } else {
                        " ".repeat(usize::from(gutter))
                    };
                    self.put(&n, look.gutter, None);
                }
                if self.opts.code.wrap || str_width(text, amb) <= usize::from(content) {
                    self.put_flags(text, style, None, SpanFlags::CODE);
                } else {
                    let (kept, _) = cut_to_cols(text, content.saturating_sub(1), amb);
                    self.put_flags(kept, style, None, SpanFlags::CODE);
                    let used = self.cols().saturating_sub(start + left + gutter);
                    self.spaces(content.saturating_sub(1).saturating_sub(used), style);
                    let clip = self.deco.clip;
                    self.put(clip, look.gutter, None);
                }
                let fill = self.code_right(&look, style, start, width);
                let line_off = e.source_offset(to_u32(chunk.start));
                let pos = off
                    .saturating_add(to_u32(line_start))
                    .saturating_add(line_off);
                let kind = LineKind::Code {
                    block,
                    line: to_u32(k),
                    byte0: to_u32(chunk.start),
                };
                self.end(kind, fill, pos);
            }
            line_start += src.len() + 1;
        }
        self.code_footer(&look, width, off.saturating_add(to_u32(cb.code.len())));
        self.off = off.saturating_add(to_u32(cb.code.len()));
    }

    /// The left edge of a code row: the pad column (a wrap marker on
    /// continuation rows) after the frame or bar; `style` is the row's
    /// (panel, or a diff tint).
    fn code_left(&mut self, look: &Look, style: StyleId, continued: bool) {
        match look.mode {
            Mode::Panel => {}
            Mode::Bare => return,
            Mode::Frame => {
                let v = self.deco.frame.vertical;
                self.put_glyph(v, look.frame);
            }
            Mode::Gutter => {
                let bar = self.deco.gutter;
                self.put_glyph(bar, look.frame);
            }
        }
        if continued {
            let marker = if self.deco.wrap.cols == 1 {
                self.deco.wrap.text.clone()
            } else {
                ">".to_string()
            };
            self.put(&marker, look.gutter, None);
        } else {
            let pad = match look.mode {
                Mode::Panel => style,
                _ => StyleId(0),
            };
            self.put(" ", pad, None);
        }
    }

    /// The right edge of a code row; returns the row's fill.
    fn code_right(&mut self, look: &Look, style: StyleId, start: u16, width: u16) -> Fill {
        match look.mode {
            Mode::Panel => {
                let used = self.cols() - start;
                // A tinted diff line keeps its tint to the edge.
                if style != look.panel {
                    self.spaces(width.saturating_sub(used), style);
                }
                Fill::Panel {
                    style: look.panel,
                    to_col: start.saturating_add(width),
                }
            }
            Mode::Frame => {
                let used = self.cols() - start;
                self.spaces(width.saturating_sub(used).saturating_sub(1), StyleId(0));
                let v = self.deco.frame.vertical;
                self.put_glyph(v, look.frame);
                Fill::None
            }
            Mode::Gutter | Mode::Bare => Fill::None,
        }
    }

    /// The row above the code: title and language label, each cut (or
    /// left out) so the row fits `width` columns, the label first.
    fn code_header(
        &mut self,
        look: &Look,
        width: u16,
        title: Option<&str>,
        label: Option<&str>,
        off: u32,
    ) {
        let amb = self.amb;
        let clean = |s: &str| {
            crate::text::strip_soft_hyphens(&crate::text::sanitize(s)).replace(['\n', '\t'], " ")
        };
        let title = title.map(clean);
        let label = label.map(clean);
        // `text` cut to `room` columns; `None` when nothing of it fits.
        let fit = |text: &Option<String>, room: u16| -> Option<(String, u16)> {
            let (t, w) = cut_to_cols(text.as_deref()?, room, amb);
            (w > 0).then(|| (t.to_string(), w))
        };
        match look.mode {
            Mode::Gutter | Mode::Bare => {}
            Mode::Panel => {
                // ` title … label `: one column of padding on each side.
                let inner = width.saturating_sub(2);
                let label = fit(&label, inner);
                let label_room = label.as_ref().map_or(0, |(_, w)| w.saturating_add(1));
                let title = fit(&title, inner.saturating_sub(label_room));
                self.begin();
                self.placed();
                let start = self.cols();
                self.put(" ", look.panel, None);
                if let Some((t, _)) = &title {
                    self.put(t, look.label, None);
                }
                if let Some((l, label_w)) = &label {
                    let used = self.cols() - start;
                    let pad = width.saturating_sub(used.saturating_add(*label_w).saturating_add(1));
                    self.spaces(pad, look.panel);
                    self.put(l, look.label, None);
                }
                let fill = Fill::Panel {
                    style: look.panel,
                    to_col: start.saturating_add(width),
                };
                self.end(LineKind::Text, fill, off);
            }
            Mode::Frame => {
                // `┌─ title ─── label ─┐`: the label takes ` label ─`, the
                // title `─ title ` (and one more dash before a label),
                // dashes fill what is left.
                let b = self.deco.frame;
                let inner = width.saturating_sub(2);
                let label = fit(&label, inner.saturating_sub(3));
                let label_seg = label.as_ref().map_or(0, |(_, w)| w.saturating_add(3));
                let separator = u16::from(label.is_some());
                let title_room = inner
                    .saturating_sub(label_seg)
                    .saturating_sub(3 + separator);
                let title = fit(&title, title_room);
                let title_seg = title.as_ref().map_or(0, |(_, w)| w.saturating_add(3));
                self.begin();
                self.placed();
                self.put_glyph(b.top_left, look.frame);
                if let Some((t, _)) = &title {
                    self.put_glyph(b.horizontal, look.frame);
                    self.put(" ", look.frame, None);
                    self.put(t, look.label, None);
                    self.put(" ", look.frame, None);
                }
                let dashes = inner.saturating_sub(title_seg).saturating_sub(label_seg);
                self.repeat(b.horizontal, dashes, look.frame);
                if let Some((l, _)) = &label {
                    self.put(" ", look.frame, None);
                    self.put(l, look.label, None);
                    self.put(" ", look.frame, None);
                    self.put_glyph(b.horizontal, look.frame);
                }
                self.put_glyph(b.top_right, look.frame);
                self.end(LineKind::Text, Fill::None, off);
            }
        }
    }

    /// The row below the code.
    fn code_footer(&mut self, look: &Look, width: u16, off: u32) {
        match look.mode {
            Mode::Gutter | Mode::Bare => {}
            Mode::Panel => {
                self.begin();
                self.placed();
                let start = self.cols();
                self.put(" ", look.panel, None);
                let fill = Fill::Panel {
                    style: look.panel,
                    to_col: start.saturating_add(width),
                };
                self.end(LineKind::Text, fill, off);
            }
            Mode::Frame => {
                let b = self.deco.frame;
                self.begin();
                self.placed();
                self.put_glyph(b.bottom_left, look.frame);
                self.repeat(b.horizontal, width.saturating_sub(2), look.frame);
                self.put_glyph(b.bottom_right, look.frame);
                self.end(LineKind::Text, Fill::None, off);
            }
        }
    }
}

/// Code text as drawn: a soft hyphen, which terminals disagree about,
/// becomes `˗` (U+02D7, the same number of bytes, so highlight offsets stay
/// valid).
fn visible_code(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\u{ad}') {
        std::borrow::Cow::Owned(text.replace('\u{ad}', "\u{2d7}"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// The sign of a unified-diff line (file headers excluded).
fn diff_sign(line: &str) -> Option<Sign> {
    if line.starts_with("+++") || line.starts_with("---") {
        None
    } else if line.starts_with('+') {
        Some(Sign::Added)
    } else if line.starts_with('-') {
        Some(Sign::Removed)
    } else {
        None
    }
}

/// Split a line into pieces of at most `cols` columns at grapheme
/// boundaries (a grapheme wider than `cols` gets a piece of its own). An
/// empty line is one empty piece.
fn split_cols(text: &str, cols: u16, amb: bool) -> Vec<Range<usize>> {
    let cols = usize::from(cols.max(1));
    if text.is_ascii() {
        if text.is_empty() {
            return std::iter::once(0..0).collect();
        }
        return (0..text.len())
            .step_by(cols)
            .map(|s| s..(s + cols).min(text.len()))
            .collect();
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut used = 0usize;
    let mut pos = 0;
    while pos < text.len() {
        let end = next_grapheme_end(text, pos);
        let w = grapheme_width(text.get(pos..end).unwrap_or(""), amb);
        if used + w > cols && used > 0 {
            out.push(start..pos);
            start = pos;
            used = 0;
        }
        used += w;
        pos = end;
    }
    out.push(start..text.len());
    out
}

/// The largest char boundary of `s` at or below `i`.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Move a highlighting's offsets from source lines into tab-expanded lines;
/// spans past a line's end are clipped, empty ones dropped.
fn to_expanded(hl: HlBlock, source: &[&str], expanded: &[Expanded<'_>]) -> HlBlock {
    let lines = hl
        .lines
        .into_iter()
        .zip(source.iter().zip(expanded))
        .map(|(spans, (src, e))| {
            let mut out = Vec::with_capacity(spans.len());
            let mut prev = 0u32;
            for span in spans {
                let end_src = floor_boundary(src, span.end as usize);
                let end = (e.map_offset(to_u32(end_src)) as usize).min(e.text.len());
                let end = to_u32(floor_boundary(&e.text, end));
                if end > prev {
                    out.push(HlSpan {
                        end,
                        style: span.style,
                    });
                    prev = end;
                }
            }
            out
        })
        .collect();
    HlBlock { lines }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    #[test]
    fn splits_by_columns() {
        let one = |start, end| vec![Range { start, end }];
        assert_eq!(split_cols("", 4, false), one(0, 0));
        assert_eq!(split_cols("abcdefghij", 4, false), [0..4, 4..8, 8..10]);
        assert_eq!(split_cols("abcd", 4, false), one(0, 4));
        // 日本語 is 3 × 3 bytes, 2 columns each.
        assert_eq!(split_cols("日本語", 4, false), [0..6, 6..9]);
        assert_eq!(split_cols("日本", 1, false), [0..3, 3..6]);
        assert_eq!(split_cols("ae\u{301}b", 2, false), [0..4, 4..5]);
    }

    #[test]
    fn soft_hyphens_in_code_keep_their_length() {
        assert_eq!(visible_code("a\u{ad}b"), "a\u{2d7}b");
        assert_eq!(visible_code("a\u{ad}b").len(), "a\u{ad}b".len());
        assert!(matches!(visible_code("ab"), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn diff_signs() {
        assert_eq!(diff_sign("+added"), Some(Sign::Added));
        assert_eq!(diff_sign("-removed"), Some(Sign::Removed));
        assert_eq!(diff_sign("+++ b/file"), None);
        assert_eq!(diff_sign("--- a/file"), None);
        assert_eq!(diff_sign(" context"), None);
    }

    #[test]
    fn highlight_offsets_follow_tabs() {
        let span = |end| HlSpan {
            end,
            style: Style::PLAIN,
        };
        let source = ["\tx = 1", "é"];
        let expanded: Vec<Expanded<'_>> = source.iter().map(|l| expand_tabs(l, 4, false)).collect();
        let hl = HlBlock {
            lines: vec![vec![span(1), span(2), span(99)], vec![span(1), span(2)]],
        };
        let out = to_expanded(hl, &source, &expanded);
        // "\t" → 4 spaces; "x" ends at 5; the rest is clipped to the line.
        let ends: Vec<Vec<u32>> = out
            .lines
            .iter()
            .map(|l| l.iter().map(|s| s.end).collect())
            .collect();
        assert_eq!(ends, [vec![4, 5, 9], vec![2]]);
    }
}
