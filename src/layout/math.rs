//! Math through `emde_math`'s public API: inline formulas as styled runs,
//! display math as centred rows.
//!
//! Every call into the math crate runs inside [`crate::panic::guarded`], and
//! its output is checked before use: text is sanitised (no control
//! characters or line breaks), spans must tile the text on character
//! boundaries, and break hints must be ordered offsets inside it. Anything
//! that fails the checks is repaired or replaced, so a bad formula (or a
//! bad renderer) can only ever produce odd-looking text.

use std::borrow::Cow;

use emde_math::{MathDisplay, MathLine, MathOptions, MathRole, MathSpan};

use crate::ir::{InlineFlags, MathBlock};
use crate::options::{DisplayMath, InlineMath};
use crate::panic::guarded;
use crate::style::StyleId;
use crate::text::{sanitize, str_width};

use super::build::Builder;
use super::inline::{CRunSpec, Composed};
use super::style::Piece;
use super::{Fill, LineKind};

/// A formula ready to lay out: sanitised text, role spans tiling it, and
/// break offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MathText {
    pub(super) text: String,
    pub(super) spans: Vec<MathSpan>,
    pub(super) breaks: Vec<u32>,
    /// `false` when the text is TeX that could not be typeset.
    pub(super) ok: bool,
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl MathText {
    /// Raw TeX shown as text: whitespace collapsed, a break after each space.
    pub(super) fn raw(tex: &str, ok: bool) -> MathText {
        let text = clean(tex).split_whitespace().collect::<Vec<_>>().join(" ");
        let breaks = text
            .match_indices(' ')
            .map(|(i, _)| to_u32(i + 1))
            .filter(|&b| (b as usize) < text.len())
            .collect();
        let role = if ok { MathRole::Plain } else { MathRole::Error };
        MathText {
            spans: single_span(&text, role),
            text,
            breaks,
            ok,
        }
    }

    /// Check and repair a line from the math renderer.
    pub(super) fn from_line(line: MathLine) -> MathText {
        let MathLine {
            text,
            spans,
            breaks,
            ok,
            ..
        } = line;
        let fallback_role = if ok { MathRole::Plain } else { MathRole::Error };
        let valid = spans_tile(&text, &spans);
        let spans = if valid {
            spans
        } else {
            single_span(&text, fallback_role)
        };
        // Sanitise span by span so offsets stay consistent.
        let mut out = String::with_capacity(text.len());
        let mut new_spans = Vec::with_capacity(spans.len());
        let mut new_breaks = Vec::with_capacity(breaks.len());
        let mut start = 0usize;
        let mut breaks = breaks;
        breaks.sort_unstable();
        let mut breaks = breaks.iter().copied().peekable();
        for span in &spans {
            let end = (span.end as usize).min(text.len());
            let piece = text.get(start..end).unwrap_or("");
            let base = out.len();
            while let Some(b) = breaks.next_if(|&b| (b as usize) < end) {
                let b = b as usize;
                if b > start && text.is_char_boundary(b) {
                    let prefix = text.get(start..b).unwrap_or("");
                    new_breaks.push(to_u32(base + clean(prefix).len()));
                } else if b == start && b > 0 {
                    new_breaks.push(to_u32(base));
                }
            }
            out.push_str(&clean(piece));
            if out.len() > base {
                new_spans.push(MathSpan {
                    end: to_u32(out.len()),
                    ..*span
                });
            }
            start = end;
        }
        new_breaks.retain(|&b| b > 0 && (b as usize) < out.len());
        new_breaks.sort_unstable();
        new_breaks.dedup();
        MathText {
            text: out,
            spans: new_spans,
            breaks: new_breaks,
            ok,
        }
    }
}

/// Control characters as pictures, line breaks and tabs as spaces, soft
/// hyphens dropped.
fn clean(s: &str) -> Cow<'_, str> {
    let s = sanitize(s);
    if s.contains(['\n', '\t', '\u{ad}']) {
        Cow::Owned(s.replace(['\n', '\t'], " ").replace('\u{ad}', ""))
    } else {
        s
    }
}

/// One span over the whole text (none for an empty text).
fn single_span(text: &str, role: MathRole) -> Vec<MathSpan> {
    if text.is_empty() {
        return Vec::new();
    }
    vec![MathSpan {
        end: to_u32(text.len()),
        role,
        bold: false,
        dim: false,
    }]
}

/// Whether spans are non-empty, in order, on character boundaries and end
/// exactly at the end of the text.
fn spans_tile(text: &str, spans: &[MathSpan]) -> bool {
    let mut prev = 0usize;
    for s in spans {
        let end = s.end as usize;
        if end <= prev || end > text.len() || !text.is_char_boundary(end) {
            return false;
        }
        prev = end;
    }
    prev == text.len()
}

impl Builder<'_> {
    /// Options for the math renderer.
    fn math_opts(&self) -> MathOptions {
        MathOptions {
            ambiguous_wide: self.amb,
            ..self.opts.math.opts
        }
    }

    /// Typeset an inline formula (or show its TeX, per `math.inline`).
    pub(super) fn inline_math(&self, tex: &str) -> MathText {
        match self.opts.math.inline {
            InlineMath::Unicode => {
                let opts = self.math_opts();
                guarded(|| emde_math::inline(tex, &opts))
                    .map_or_else(|| MathText::raw(tex, false), MathText::from_line)
            }
            // ASCII math is not implemented yet: show the TeX.
            InlineMath::Ascii | InlineMath::Raw => MathText::raw(tex, true),
        }
    }

    /// Composed text of a formula on top of `base`.
    fn math_composed(&mut self, m: &MathText, base: StyleId) -> Composed<'static> {
        let mut specs = Vec::with_capacity(m.spans.len());
        for span in &m.spans {
            let role = if m.ok { span.role } else { MathRole::Error };
            let piece = Piece::Math {
                role,
                bold: span.bold,
                dim: span.dim,
            };
            specs.push(CRunSpec {
                end: span.end,
                style: self.sty.inline(base, InlineFlags::empty(), piece, false),
            });
        }
        // Break only at the hints: everything between two hints is an atom.
        let mut atoms = Vec::with_capacity(m.breaks.len() + 1);
        let mut prev = 0;
        for &b in &m.breaks {
            if b > prev {
                atoms.push(prev..b);
            }
            prev = b;
        }
        if to_u32(m.text.len()) > prev {
            atoms.push(prev..to_u32(m.text.len()));
        }
        Composed::from_parts(m.text.clone(), &specs, atoms, m.breaks.clone())
    }

    /// A display math block.
    pub(super) fn math_block(&mut self, m: &MathBlock) {
        let off = self.off;
        let width = self.avail();
        let base = self.sty.el(self.text_el);
        match self.opts.math.display {
            DisplayMath::Raw => {
                let style = self.sty.el(crate::theme::Element::Math);
                for line in m.tex.split('\n') {
                    let c = self.plain_composed(line.trim_end(), style);
                    self.emit_centred(&c, width, off);
                }
            }
            DisplayMath::Linear => {
                let t = self.inline_math_forced(&m.tex);
                let c = self.math_composed(&t, base);
                self.emit_centred(&c, width, off);
            }
            DisplayMath::TwoD => {
                let opts = self.math_opts();
                let tex: &str = &m.tex;
                match guarded(|| emde_math::display(tex, &opts, width)) {
                    Some(MathDisplay::Box(b)) => {
                        let rows: Vec<MathText> =
                            b.rows.into_iter().map(MathText::from_line).collect();
                        self.math_rows(&rows, base, width, off);
                    }
                    Some(MathDisplay::Lines(lines)) => {
                        for line in lines {
                            let t = MathText::from_line(line);
                            let c = self.math_composed(&t, base);
                            self.emit_centred(&c, width, off);
                        }
                    }
                    Some(MathDisplay::Raw(line)) => {
                        let t = MathText::from_line(line);
                        let c = self.math_composed(&t, base);
                        self.emit_centred(&c, width, off);
                    }
                    None => {
                        let t = MathText::raw(tex, false);
                        let c = self.math_composed(&t, base);
                        self.emit_centred(&c, width, off);
                    }
                }
            }
        }
        self.off = off.saturating_add(to_u32(m.tex.len()));
    }

    /// The linear form of a formula, whatever `math.inline` says.
    fn inline_math_forced(&self, tex: &str) -> MathText {
        let opts = self.math_opts();
        guarded(|| emde_math::inline(tex, &opts))
            .map_or_else(|| MathText::raw(tex, false), MathText::from_line)
    }

    /// Rows of a 2D box, centred as one block.
    fn math_rows(&mut self, rows: &[MathText], base: StyleId, width: u16, off: u32) {
        let amb = self.amb;
        let box_width = rows
            .iter()
            .map(|r| str_width(&r.text, amb))
            .max()
            .unwrap_or(0);
        let pad = usize::from(width).saturating_sub(box_width) / 2;
        let pad = u16::try_from(pad).unwrap_or(0);
        for row in rows {
            self.begin();
            self.placed();
            self.spaces(pad, StyleId(0));
            let mut start = 0usize;
            for span in &row.spans {
                let end = span.end as usize;
                let text = row.text.get(start..end).unwrap_or("");
                let role = if row.ok { span.role } else { MathRole::Error };
                let piece = Piece::Math {
                    role,
                    bold: span.bold,
                    dim: span.dim,
                };
                let style = self.sty.inline(base, InlineFlags::empty(), piece, false);
                self.put(text, style, None);
                start = end;
            }
            self.end(LineKind::Math, Fill::None, off);
        }
    }

    /// Wrap composed text and emit each line centred.
    pub(super) fn emit_centred(&mut self, c: &Composed<'_>, width: u16, off: u32) {
        let mut lines = Vec::new();
        let mut pieces = Vec::new();
        self.wrap_composed(c, width, width, &mut lines, &mut pieces);
        let mut p = 0usize;
        for (li, line) in lines.iter().enumerate() {
            let start = p;
            while pieces.get(p).is_some_and(|piece| piece.line as usize == li) {
                p += 1;
            }
            self.begin();
            self.placed();
            self.spaces(width.saturating_sub(line.cols) / 2, StyleId(0));
            self.put_pieces(c, line, pieces.get(start..p).unwrap_or(&[]));
            self.end(LineKind::Math, Fill::None, off);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(end: u32, role: MathRole) -> MathSpan {
        MathSpan {
            end,
            role,
            bold: false,
            dim: false,
        }
    }

    #[test]
    fn raw_collapses_whitespace() {
        let t = MathText::raw("a +\n  b\t= c", true);
        assert_eq!(t.text, "a + b = c");
        assert_eq!(t.breaks, [2, 4, 6, 8]);
        assert_eq!(t.spans, [span(9, MathRole::Plain)]);
        assert!(MathText::raw("", true).spans.is_empty());
        assert_eq!(MathText::raw("x", false).spans[0].role, MathRole::Error);
    }

    #[test]
    fn lines_are_sanitised_with_offsets_kept() {
        let line = MathLine {
            text: "x\n\u{1b}y".into(),
            spans: vec![span(1, MathRole::Var), span(4, MathRole::Op)],
            breaks: vec![1, 3],
            width: 3,
            ok: true,
        };
        let t = MathText::from_line(line);
        assert_eq!(t.text, "x ␛y");
        assert_eq!(t.spans, [span(1, MathRole::Var), span(6, MathRole::Op)]);
        assert_eq!(t.breaks, [1, 5]);
    }

    #[test]
    fn broken_spans_are_replaced() {
        for spans in [
            vec![span(2, MathRole::Var)],                        // short
            vec![span(9, MathRole::Var)],                        // past the end
            vec![span(2, MathRole::Var), span(2, MathRole::Op)], // empty
            vec![span(2, MathRole::Var), span(3, MathRole::Op)], // not a char boundary
        ] {
            let line = MathLine {
                text: "aé".into(),
                spans,
                breaks: vec![7, 1, 0],
                width: 2,
                ok: false,
            };
            let t = MathText::from_line(line);
            assert_eq!(t.text, "aé");
            assert_eq!(t.spans, [span(3, MathRole::Error)]);
            assert_eq!(t.breaks, [1]);
        }
    }
}
