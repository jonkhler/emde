//! Display math: the 2D layout and its fallback ladder.
//!
//! [`render`] tries, in order:
//!
//! 1. the 2D box, if it is at most `avail` columns wide and
//!    [`MathOptions::max_height`] rows tall;
//! 2. the 2D rows split before top-level relations, the relations aligned
//!    under the first one (else indented by two columns, else none), within
//!    the same limits;
//! 3. the linear form, wrapped at top-level operators
//!    ([`MathDisplay::Lines`]); a multi-row block (`aligned`, `gathered`, a
//!    bare `\\`) keeps one line per row, each wrapped on its own.
//!
//! Parse errors never get here: the caller shows raw TeX. With
//! [`MathOptions::ambiguous_wide`] the 2D steps are skipped, because box
//! drawing and bracket pieces are East Asian Ambiguous and break the grid.
//!
//! Layout policies:
//!
//! * fractions stack their tightly spaced parts over a bar as wide as the
//!   wider part, which is the baseline;
//! * large operators with limits stack them (centred, rounding left), and
//!   integrals are `⌠⎮⌡` with the limits at the right corners;
//! * a script uses Unicode super/subscripts when every character has one,
//!   else its linear form is raised or lowered: inner levels stay linear, so
//!   `e^{-x^2}` is `e` under a raised `−x²`;
//! * delimiters grow with `⎛⎜⎝`-style pieces (`⎰⎱` for two-row braces);
//!   `\big`-style delimiters grow to the height of their row;
//! * radicals are `√` under a `▁` vinculum (left out over one character
//!   that nothing follows directly: `√x`), taller ones a `╱` diagonal with a
//!   `╲` foot;
//! * matrices have two columns between cells and no gap between one-row
//!   rows; cases always leave a row between rows, so the brace gets a real
//!   `⎨`; array rules side by side (`||`) touch;
//! * braces over and under are `╭─┴─╮` and `╰─┬─╯`;
//! * a `\tag` makes the box `avail` wide: the formula centred, the tag flush
//!   right on the baseline row (or below when they do not fit side by side).

use crate::adapter::MAX_DEPTH;
use crate::ast::{Accent, Align, Atom, Brace, Column, Formula, Grid, GridKind, Limits, Node};
use crate::boxes::{Cell, MBox};
use crate::linear::{self, Attrs, Ctx, Frag, Linear, ScriptText};
use crate::spacing::{self, Class};
use crate::style;
use crate::tables;
use crate::width::{is_zero_width, str_width, to_u16};
use crate::{MathDisplay, MathLine, MathOptions, MathRole, MathSpan};

/// Lay out display math within `avail` columns.
pub(crate) fn render(formula: &Formula, opts: &MathOptions, avail: u16) -> MathDisplay {
    let avail = usize::from(avail);
    if !opts.ambiguous_wide {
        let layout = Layout {
            opts,
            lin: Linear { opts },
            max_w: avail,
            max_h: usize::from(opts.max_height),
        };
        if let Some(b) = layout.formula(formula) {
            return MathDisplay::Box(b.to_math_box(opts.ambiguous_wide));
        }
    }
    MathDisplay::Lines(lines(formula, opts, avail))
}

/// Step 3: the linear form, wrapped to `avail` columns at its break hints.
/// A multi-row block (`aligned`, `gathered`, a bare `\\`) keeps its rows,
/// each wrapped on its own.
fn lines(formula: &Formula, opts: &MathOptions, avail: usize) -> Vec<MathLine> {
    let cjk = opts.ambiguous_wide;
    let lin = Linear { opts };
    let rows = lin
        .rows(&formula.body)
        .unwrap_or_else(|| vec![lin.frag(&formula.body, Ctx::top())]);
    let mut out: Vec<MathLine> = rows
        .into_iter()
        .flat_map(|row| wrap(&row.into_line(cjk), avail, cjk))
        .collect();
    if let Some(tag) = &formula.tag {
        let tag_width = str_width(tag, cjk);
        match out.last_mut() {
            Some(last)
                if usize::from(last.width) + 2 + tag_width <= avail || last.text.is_empty() =>
            {
                let mut frag = line_frag(last);
                if !frag.text.is_empty() {
                    frag.push("  ", Attrs::PLAIN);
                }
                frag.push(tag, Attrs::role(MathRole::Text));
                *last = frag.into_line(cjk);
            }
            _ => {
                let mut frag = Frag::default();
                frag.push(tag, Attrs::role(MathRole::Text));
                out.push(frag.into_line(cjk));
            }
        }
    }
    out
}

/// A line back as a fragment.
fn line_frag(line: &MathLine) -> Frag {
    Frag {
        text: line.text.clone(),
        spans: line.spans.clone(),
        breaks: line.breaks.clone(),
    }
}

/// Split `line` at its break hints so that each piece fits `avail` where
/// possible; a piece without a break that fits is left too wide.
pub(crate) fn wrap(line: &MathLine, avail: usize, cjk: bool) -> Vec<MathLine> {
    if usize::from(line.width) <= avail || line.breaks.is_empty() {
        return vec![line.clone()];
    }
    let len = line.text.len();
    // Cut points: the start, every usable break (a space precedes each), the
    // end. Widths add up across them.
    let mut cuts = vec![0];
    cuts.extend(
        line.breaks
            .iter()
            .filter_map(|&b| usize::try_from(b).ok())
            .filter(|&b| b > 0 && b < len && line.text.is_char_boundary(b)),
    );
    cuts.push(len);
    cuts.dedup();
    // Each piece's width, and its width without the trailing whitespace
    // that is dropped where a line ends (measured in columns: whitespace
    // such as U+00A0 is wider in bytes).
    let pieces: Vec<(usize, usize)> = cuts
        .windows(2)
        .map(|w| {
            let text = match *w {
                [a, b] => line.text.get(a..b).unwrap_or(""),
                _ => "",
            };
            (str_width(text, cjk), str_width(text.trim_end(), cjk))
        })
        .collect();
    let mut out = Vec::new();
    let mut first = 0;
    while first < pieces.len() {
        // As many pieces as fit, and at least one.
        let mut width = 0;
        let mut last = first;
        for (k, &(full, trimmed)) in pieces.iter().enumerate().skip(first) {
            // A piece of only whitespace always fits: it is dropped where a
            // line ends.
            if k > first && trimmed > 0 && width + trimmed > avail {
                break;
            }
            width += full;
            last = k;
        }
        let start = cuts.get(first).copied().unwrap_or(len);
        let end = cuts.get(last + 1).copied().unwrap_or(len);
        out.push(slice_line(line, start, end, cjk));
        first = last + 1;
    }
    out
}

/// Bytes `start..end` of `line` as a line of its own, trailing spaces
/// dropped.
fn slice_line(line: &MathLine, start: usize, end: usize, cjk: bool) -> MathLine {
    let text = line.text.get(start..end).unwrap_or("").trim_end();
    let stop = start + text.len();
    let mut spans = Vec::new();
    for span in &line.spans {
        let span_end = usize::try_from(span.end).unwrap_or(usize::MAX).min(stop);
        if span_end <= start {
            continue;
        }
        let rel = u32::try_from(span_end - start).unwrap_or(u32::MAX);
        if spans.last().is_some_and(|s: &MathSpan| s.end >= rel) {
            continue;
        }
        spans.push(MathSpan { end: rel, ..*span });
        if span_end >= stop {
            break;
        }
    }
    let breaks = line
        .breaks
        .iter()
        .filter_map(|&b| usize::try_from(b).ok())
        .filter(|&b| b > start && b < stop)
        .filter_map(|b| u32::try_from(b - start).ok())
        .collect();
    MathLine {
        text: text.to_string(),
        spans,
        breaks,
        width: to_u16(str_width(text, cjk)),
        ok: true,
    }
}

/// A laid-out row item: its box, the blank columns before it and its
/// spacing classes.
struct Part {
    b: MBox,
    gap: usize,
    class: Option<(Class, Class)>,
}

/// Join parts (and `after` trailing columns) into one box.
fn join(parts: &[Part], after: usize) -> Option<MBox> {
    let mut boxes = Vec::with_capacity(parts.len() * 2 + 1);
    for p in parts {
        if p.gap > 0 {
            boxes.push(MBox::blank(p.gap, 1, 0)?);
        }
        boxes.push(p.b.clone());
    }
    if after > 0 {
        boxes.push(MBox::blank(after, 1, 0)?);
    }
    MBox::hcat(&boxes)
}

/// A super/subscript in 2D.
enum Script {
    /// Unicode super/subscript characters, set next to the base.
    Unicode(MBox),
    /// The linear form, raised above or lowered below the base.
    Raised(MBox),
}

impl Script {
    fn width(&self) -> usize {
        match self {
            Script::Unicode(b) | Script::Raised(b) => b.w,
        }
    }
}

/// The 2D layout.
struct Layout<'o> {
    opts: &'o MathOptions,
    lin: Linear<'o>,
    /// No box wider than this can be part of a result (`avail`).
    max_w: usize,
    /// Nor taller than this (`max_height`).
    max_h: usize,
}

impl Layout<'_> {
    fn cjk(&self) -> bool {
        self.opts.ambiguous_wide
    }

    /// `b` if it can still be part of a result.
    fn fit(&self, b: MBox) -> Option<MBox> {
        (b.w <= self.max_w && b.h <= self.max_h).then_some(b)
    }

    fn text(&self, frag: &Frag) -> Option<MBox> {
        MBox::text(frag, self.cjk())
    }

    fn glyph(&self, c: char, role: MathRole) -> Option<MBox> {
        MBox::row_of(&[c], Attrs::role(role))
    }

    /// Steps 1 and 2 for the whole formula.
    fn formula(&self, formula: &Formula) -> Option<MBox> {
        let items = flatten(formula.body.items());
        let (parts, after) = self.parts(&items, Ctx::top(), None, None)?;
        let tag = formula.tag.as_deref();
        if let Some(b) = join(&parts, after).and_then(|b| self.fit(b))
            && let Some(b) = self.with_tag(b, tag)
        {
            return Some(b);
        }
        self.split(&parts, tag)
    }

    /// Step 2: rows split before top-level relations.
    fn split(&self, parts: &[Part], tag: Option<&str>) -> Option<MBox> {
        let is_rel = |p: &Part| p.class.is_some_and(|(l, _)| l == Class::Rel);
        let ends_rel = |p: &Part| p.class.is_some_and(|(_, r)| r == Class::Rel);
        let starts: Vec<usize> = (1..parts.len())
            .filter(|&i| {
                parts.get(i).is_some_and(is_rel) && !parts.get(i - 1).is_some_and(ends_rel)
            })
            .collect();
        let first = *starts.first()?;
        let head = join(parts.get(..first)?, 0)?;
        let mut chunks = Vec::new();
        for (k, &s) in starts.iter().enumerate() {
            let e = starts.get(k + 1).copied().unwrap_or(parts.len());
            let chunk = parts.get(s..e)?;
            let gap = chunk.first().map_or(0, |p| p.gap);
            let mut body: Vec<Part> = Vec::with_capacity(chunk.len());
            for (j, p) in chunk.iter().enumerate() {
                body.push(Part {
                    b: p.b.clone(),
                    gap: if j == 0 { 0 } else { p.gap },
                    class: p.class,
                });
            }
            chunks.push((gap, join(&body, 0)?));
        }
        let aligned = head.w + chunks.first().map_or(0, |c| c.0);
        [aligned, 2, 0]
            .into_iter()
            .find_map(|indent| self.pack(&head, &chunks, indent))
            .and_then(|b| self.with_tag(b, tag))
    }

    /// Greedily fill rows: the head and as many chunks as fit, then rows of
    /// `indent` blank columns and chunks.
    fn pack(&self, head: &MBox, chunks: &[(usize, MBox)], indent: usize) -> Option<MBox> {
        let mut rows: Vec<Vec<MBox>> = vec![vec![head.clone()]];
        let mut width = head.w;
        for (gap, chunk) in chunks {
            if width + gap + chunk.w <= self.max_w {
                let row = rows.last_mut()?;
                row.push(MBox::blank(*gap, 1, 0)?);
                row.push(chunk.clone());
                width += gap + chunk.w;
            } else {
                if indent + chunk.w > self.max_w {
                    return None;
                }
                rows.push(vec![MBox::blank(indent, 1, 0)?, chunk.clone()]);
                width = indent + chunk.w;
            }
        }
        if rows.len() < 2 {
            return None;
        }
        let rows: Vec<MBox> = rows
            .iter()
            .map(|r| MBox::hcat(r).and_then(|b| self.fit(b)))
            .collect::<Option<_>>()?;
        stack_left(&rows).and_then(|b| self.fit(b))
    }

    /// Put the tag flush right: on the baseline row when it fits beside the
    /// centred formula, else on a row of its own below.
    fn with_tag(&self, b: MBox, tag: Option<&str>) -> Option<MBox> {
        let Some(tag) = tag else {
            return Some(b);
        };
        let mut frag = Frag::default();
        frag.push(tag, Attrs::role(MathRole::Text));
        let t = self.text(&frag)?;
        let avail = self.max_w;
        if b.w + 2 + t.w <= avail {
            let x = ((avail - b.w) / 2).min(avail - t.w - 2 - b.w);
            let mut out = MBox::blank(avail, b.h, b.base)?;
            out.place(x, 0, &b);
            out.place(avail - t.w, b.base, &t);
            Some(out)
        } else if b.w <= avail && t.w <= avail && b.h < self.max_h {
            let mut out = MBox::blank(avail, b.h + 1, b.base)?;
            out.place((avail - b.w) / 2, 0, &b);
            out.place(avail - t.w, b.h, &t);
            Some(out)
        } else {
            None
        }
    }

    fn node(&self, node: &Node, ctx: Ctx) -> Option<MBox> {
        let b = match node {
            Node::Atom(atom) => self.atom(atom, ctx)?,
            Node::Row(items) => self.row(items, ctx, None, None)?,
            Node::Frac { num, den, bar } => {
                // A bar over or under another fraction's bar overhangs it, so
                // the main bar stands out.
                let nested = is_fraction(num) || is_fraction(den);
                let num = self.node(num, ctx.tight())?;
                let den = self.node(den, ctx.tight())?;
                MBox::fraction(&num, &den, *bar, usize::from(nested))?
            }
            Node::Root { radicand, index } => self.root(radicand, index.as_deref(), ctx)?,
            Node::Scripts {
                base,
                sub,
                sup,
                limits,
            } => self.scripts(base, sub.as_deref(), sup.as_deref(), *limits, ctx)?,
            Node::Accent { base, accent } => self.accent(node, base, *accent, ctx)?,
            Node::Fenced { open, close, body } => {
                // The closing delimiter follows the body, not what follows
                // the fence.
                let inner = self.node(body, ctx.followed_by(false))?;
                self.fence(*open, inner, *close)?
            }
            Node::Grid(grid) => self.grid(grid, ctx)?,
            Node::Styled { font, body } => self.node(body, Ctx { font: *font, ..ctx })?,
            Node::Space(n) => MBox::blank(usize::from(n.max(&0).unsigned_abs()), 1, 0)?,
            Node::Not(inner) => self.not(node, inner, ctx)?,
            Node::OverUnder {
                base,
                over,
                under,
                brace,
            } => self.over_under(base, over.as_deref(), under.as_deref(), *brace, ctx)?,
        };
        self.fit(b)
    }

    fn atom(&self, atom: &Atom, ctx: Ctx) -> Option<MBox> {
        if let Atom::LargeOp(op) = atom
            && let Some(n) = tables::integral_count(*op)
        {
            return self.integral_sign(n);
        }
        let look = style::atom(atom, ctx.font, self.opts);
        let mut frag = Frag::default();
        frag.push(
            &look.text,
            Attrs {
                role: look.role,
                bold: look.bold,
                dim: false,
            },
        );
        self.text(&frag)
    }

    /// `n` three-row integral signs side by side, baseline in the middle.
    fn integral_sign(&self, n: usize) -> Option<MBox> {
        let col = MBox::column_of(&tables::INTEGRAL.column(3), Attrs::role(MathRole::Op), 1)?;
        MBox::hcat(&vec![col; n])
    }

    /// Lay out a row's items with TeX spacing; sized delimiters stretch to
    /// the row's height.
    fn parts(
        &self,
        items: &[Node],
        ctx: Ctx,
        lead: Option<Class>,
        trail: Option<Class>,
    ) -> Option<(Vec<Part>, usize)> {
        let spacing = spacing::space(items, ctx.tight, lead, trail);
        let mut parts = Vec::with_capacity(items.len());
        let mut stretchy = Vec::new();
        let mut pending: i32 = 0;
        for (i, item) in items.iter().enumerate() {
            if let Node::Space(n) = item {
                pending += i32::from(*n);
                continue;
            }
            pending += i32::try_from(spacing.before.get(i).copied().unwrap_or(0)).ok()?;
            let gap = usize::try_from(pending.max(0)).ok()?;
            pending = 0;
            if let Node::Atom(Atom::Delim {
                ch, sized: true, ..
            }) = item
            {
                stretchy.push((parts.len(), *ch));
            }
            let followed = linear::followed(items, &spacing, i).unwrap_or(ctx.followed);
            parts.push(Part {
                b: self.node(item, ctx.followed_by(followed))?,
                gap,
                class: spacing.classes.get(i).copied().flatten(),
            });
        }
        let after = usize::try_from((pending + i32::try_from(spacing.after).ok()?).max(0)).ok()?;
        if !stretchy.is_empty() {
            let others = || {
                parts
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !stretchy.iter().any(|(s, _)| s == i))
                    .map(|(_, p)| &p.b)
            };
            let above = others().map(|b| b.base).max().unwrap_or(0);
            let below = others()
                .map(|b| b.h.saturating_sub(b.base + 1))
                .max()
                .unwrap_or(0);
            for &(i, ch) in &stretchy {
                let b = self.delimiter(ch, above + 1 + below, above)?;
                if let Some(p) = parts.get_mut(i) {
                    p.b = b;
                }
            }
        }
        Some((parts, after))
    }

    fn row(
        &self,
        items: &[Node],
        ctx: Ctx,
        lead: Option<Class>,
        trail: Option<Class>,
    ) -> Option<MBox> {
        let (parts, after) = self.parts(items, ctx, lead, trail)?;
        join(&parts, after)
    }

    /// A delimiter `h` rows tall with its baseline at `base`.
    fn delimiter(&self, ch: char, h: usize, base: usize) -> Option<MBox> {
        let attrs = Attrs::role(MathRole::Delim);
        if h <= 1 {
            return MBox::row_of(&[ch], attrs);
        }
        // Delimiters without pieces sit on the baseline row.
        let column = tables::tall_delimiter(ch, h)
            .unwrap_or_else(|| (0..h).map(|y| if y == base { ch } else { ' ' }).collect());
        MBox::column_of(&column, attrs, base)
    }

    fn fence(&self, open: Option<char>, inner: MBox, close: Option<char>) -> Option<MBox> {
        let mut parts = Vec::with_capacity(3);
        if let Some(open) = open {
            parts.push(self.delimiter(open, inner.h, inner.base)?);
        }
        let (h, base) = (inner.h, inner.base);
        parts.push(inner);
        if let Some(close) = close {
            parts.push(self.delimiter(close, h, base)?);
        }
        MBox::hcat(&parts)
    }

    /// A script's content as a box: its tight linear form on one row, except
    /// that `\substack` stays stacked.
    fn script_box(&self, node: &Node, frag: &Frag, ctx: Ctx) -> Option<MBox> {
        if has_substack(node, 0) {
            self.node(node, ctx.tight())
        } else {
            self.text(frag)
        }
    }

    fn script(&self, node: &Node, sup: bool, ctx: Ctx) -> Option<Script> {
        Some(match self.lin.script_text(node, sup, ctx) {
            ScriptText::Unicode(mapped) => Script::Unicode(self.text(&mapped)?),
            ScriptText::Linear(frag) => Script::Raised(self.script_box(node, &frag, ctx)?),
        })
    }

    /// Limits above/below, the labels of braces and oversets: the content in
    /// its linear form.
    fn label(&self, node: &Node, ctx: Ctx) -> Option<MBox> {
        let frag = self.lin.frag(node, ctx.tight());
        self.script_box(node, &frag, ctx)
    }

    fn scripts(
        &self,
        base: &Node,
        sub: Option<&Node>,
        sup: Option<&Node>,
        limits: Limits,
        ctx: Ctx,
    ) -> Option<MBox> {
        let op = match linear::unwrap(base) {
            Node::Atom(Atom::LargeOp(c)) => Some(tables::integral_count(*c)),
            Node::Atom(Atom::Func(_)) => Some(None),
            _ => None,
        };
        match (limits, op) {
            (Limits::Display, Some(_)) => {
                let op = self.node(base, ctx)?;
                let over = each(sup, |s| self.label(s, ctx))?;
                let under = each(sub, |s| self.label(s, ctx))?;
                let base_part = usize::from(over.is_some());
                let parts: Vec<MBox> = over.into_iter().chain([op]).chain(under).collect();
                MBox::vstack(&parts, base_part)
            }
            (Limits::Right, Some(Some(n))) => {
                let upper = each(sup, |s| self.label(s, ctx))?;
                let lower = each(sub, |s| self.label(s, ctx))?;
                self.integral(n, upper, lower)
            }
            _ => {
                let b = self.node(base, ctx.followed_by(true))?;
                let sub = each(sub, |s| self.script(s, false, ctx))?;
                let sup = each(sup, |s| self.script(s, true, ctx))?;
                attach(b, sub, sup)
            }
        }
    }

    /// `⌠⎮⌡` with the upper limit right of `⌠` and the lower right of `⌡`.
    fn integral(&self, n: usize, upper: Option<MBox>, lower: Option<MBox>) -> Option<MBox> {
        let sign = self.integral_sign(n)?;
        let up_h = upper.as_ref().map_or(1, |u| u.h);
        let low_h = lower.as_ref().map_or(1, |l| l.h);
        let above = up_h - 1;
        let w = sign.w
            + upper
                .as_ref()
                .map_or(0, |u| u.w)
                .max(lower.as_ref().map_or(0, |l| l.w));
        let mut out = MBox::blank(w, above + 3 + low_h - 1, above + 1)?;
        out.place(0, above, &sign);
        if let Some(u) = &upper {
            out.place(sign.w, 0, u);
        }
        if let Some(l) = &lower {
            out.place(sign.w, above + 2, l);
        }
        Some(out)
    }

    fn root(&self, radicand: &Node, index: Option<&Node>, ctx: Ctx) -> Option<MBox> {
        let body = self.node(radicand, ctx.tight())?;
        let delim = Attrs::role(MathRole::Delim);
        // `∛` and `∜` only fit a one-row radicand; taller ones draw the index.
        let (sign, index) = match linear::plain_radical(index) {
            Some('√') => ('√', None),
            Some(sign) if body.h == 1 => (sign, None),
            _ => ('√', index),
        };
        // The index: superscript characters when they all map, else linear.
        let index = match index {
            Some(node) => Some(match self.lin.script_text(node, true, ctx) {
                ScriptText::Unicode(mapped) => (self.text(&mapped)?, true),
                ScriptText::Linear(frag) => (self.text(&frag)?, false),
            }),
            None => None,
        };
        if body.h == 1 {
            // A one-character radicand needs no vinculum (`√π`, `ⁿ√x`), unless
            // something follows it directly: `√2π` would read as `√(2π)`.
            let raised_index = index.as_ref().is_some_and(|(_, unicode)| !unicode);
            let short = body.w == 1 && !raised_index && !ctx.followed;
            let iw = index.as_ref().map_or(0, |(b, _)| b.w);
            let rows = if short { 1 } else { 2 };
            let row = rows - 1;
            let mut out = MBox::blank(iw + 1 + body.w, rows, row)?;
            if let Some((b, unicode)) = &index {
                // Superscript characters sit before the sign; a linear index
                // sits on the vinculum row, left of it.
                out.place(0, if *unicode { row } else { 0 }, b);
            }
            out.set(iw, row, Cell::new(sign, delim));
            if rows == 2 {
                for x in 0..body.w {
                    out.set(iw + 1 + x, 0, Cell::new('▁', delim));
                }
            }
            out.place(iw + 1, row, &body);
            return Some(out);
        }
        // Tall: a diagonal of `body.h` rows ending in a `╲` foot.
        let hh = body.h;
        let extra = index.as_ref().map_or(0, |(b, _)| b.w.saturating_sub(2));
        let w = extra + hh + 1 + body.w;
        let mut out = MBox::blank(w, hh + 1, body.base + 1)?;
        for i in 0..hh {
            out.set(extra + hh - i, i + 1, Cell::new('╱', delim));
        }
        out.set(extra, hh, Cell::new('╲', delim));
        for x in 0..body.w {
            out.set(extra + hh + 1 + x, 0, Cell::new('▁', delim));
        }
        out.place(extra + hh + 1, 1, &body);
        if let Some((b, _)) = &index {
            out.place(extra + 2 - b.w.min(extra + 2), hh - 1, b);
        }
        Some(out)
    }

    fn accent(&self, node: &Node, base: &Node, accent: Accent, ctx: Ctx) -> Option<MBox> {
        let b = self.node(base, ctx)?;
        let line_accent = matches!(accent.ch, '‾' | '→' | '←' | '↔' | '⇒' | '↼' | '⇀');
        // A combining mark typed after a symbol stays on it, whatever its
        // height; it has no row form of its own.
        let typed = is_zero_width(accent.ch);
        if typed || (b.h == 1 && (b.w <= 1 || !line_accent || accent.under)) {
            // Combining marks, as in the linear form.
            return self.text(&self.lin.frag(node, ctx));
        }
        let row = MBox::row_of(
            &tables::accent_row(accent, b.w),
            Attrs::role(MathRole::Delim),
        )?;
        if accent.under {
            MBox::vstack(&[b, row], 0)
        } else {
            MBox::vstack(&[row, b], 1)
        }
    }

    fn not(&self, node: &Node, inner: &Node, ctx: Ctx) -> Option<MBox> {
        let b = self.node(inner, ctx)?;
        if b.h == 1 {
            return self.text(&self.lin.frag(node, ctx));
        }
        let mut b = b;
        b.mark_row(b.base, tables::NEGATION_MARK);
        Some(b)
    }

    fn over_under(
        &self,
        base: &Node,
        over: Option<&Node>,
        under: Option<&Node>,
        brace: Option<Brace>,
        ctx: Ctx,
    ) -> Option<MBox> {
        if let (None, Some(o), None, Some(Atom::Rel(rel))) = (brace, over, under, base.as_atom())
            && let Some(c) = tables::overset_composite(&linear::plain_text(o), rel)
        {
            return self.glyph(c, MathRole::Rel);
        }
        let over = each(over, |o| self.label(o, ctx))?;
        let under = each(under, |u| self.label(u, ctx))?;
        let label_w = over.iter().chain(&under).map(|b| b.w).max().unwrap_or(0);
        let b = match (brace, stretchable_arrow(base)) {
            (None, Some(arrow)) if label_w > 0 => {
                let accent = Accent {
                    ch: arrow,
                    wide: true,
                    under: false,
                };
                MBox::row_of(
                    &tables::accent_row(accent, label_w + 2),
                    Attrs::role(MathRole::Rel),
                )?
            }
            _ => self.node(base, ctx)?,
        };
        let brace_row = match brace {
            Some(brace) => Some(MBox::row_of(
                &tables::brace_row(brace.shape, brace.over, b.w),
                Attrs::role(MathRole::Delim),
            )?),
            None => None,
        };
        let mut parts = Vec::with_capacity(4);
        parts.extend(over);
        if let (Some(row), Some(Brace { over: true, .. })) = (&brace_row, brace) {
            parts.push(row.clone());
        }
        let base_part = parts.len();
        parts.push(b);
        if let (Some(row), Some(Brace { over: false, .. })) = (&brace_row, brace) {
            parts.push(row.clone());
        }
        parts.extend(under);
        MBox::vstack(&parts, base_part)
    }

    fn grid(&self, grid: &Grid, ctx: Ctx) -> Option<MBox> {
        // Cells are spaced normally, except in `\substack` (always in limits).
        let cell_ctx = Ctx {
            tight: ctx.tight && matches!(grid.kind, GridKind::Substack(_)),
            followed: false,
            ..ctx
        };
        let aligned = grid.kind == GridKind::Aligned;
        let mut cells: Vec<Vec<MBox>> = Vec::with_capacity(grid.rows.len());
        for row in &grid.rows {
            let mut boxes = Vec::with_capacity(row.len());
            for (c, cell) in row.iter().enumerate() {
                let (lead, trail) = match (aligned, c % 2) {
                    (true, 1) => (Some(Class::Ord), None),
                    (true, _) => (None, Some(Class::Ord)),
                    _ => (None, None),
                };
                boxes.push(self.row(cell.items(), cell_ctx, lead, trail)?);
            }
            cells.push(boxes);
        }
        let ncols = cells.iter().map(Vec::len).max().unwrap_or(0);
        let mut col_w = vec![0; ncols];
        for row in &cells {
            for (c, b) in row.iter().enumerate() {
                if let Some(w) = col_w.get_mut(c) {
                    *w = (*w).max(b.w);
                }
            }
        }
        let extents: Vec<(usize, usize)> = cells
            .iter()
            .map(|row| {
                let above = row.iter().map(|b| b.base).max().unwrap_or(0);
                let below = row
                    .iter()
                    .map(|b| b.h.saturating_sub(b.base + 1))
                    .max()
                    .unwrap_or(0);
                (above, below)
            })
            .collect();

        // Columns: content, gaps and vertical rules, left to right.
        let slots = column_slots(grid, ncols);
        let mut x = 0;
        let mut col_x = vec![0; ncols];
        let mut rules = Vec::new();
        for slot in &slots {
            match *slot {
                Slot::Cells(c) => {
                    if let Some(cx) = col_x.get_mut(c) {
                        *cx = x;
                    }
                    x += col_w.get(c).copied().unwrap_or(0);
                }
                Slot::Gap(n) => x += n,
                Slot::Rule(dashed) => {
                    rules.push((x, dashed));
                    x += 1;
                }
            }
        }
        let width = x;

        // Rows: content, gaps and horizontal rules, top to bottom.
        let tall = |r: usize| extents.get(r).is_some_and(|&(a, b)| a + b > 0);
        let mut y = 0;
        let mut row_y = Vec::with_capacity(cells.len());
        let mut hrules = Vec::new();
        for r in 0..cells.len() {
            let lines = grid.hlines.get(r).copied().unwrap_or(0);
            if lines > 0 {
                hrules.push(y);
                y += 1;
            } else if r > 0 {
                let gap = match grid.kind {
                    GridKind::Cases { .. } => 1,
                    _ => usize::from(tall(r - 1) || tall(r)),
                };
                y += gap;
            }
            row_y.push(y);
            let (above, below) = extents.get(r).copied().unwrap_or((0, 0));
            y += above + 1 + below;
        }
        if grid.hlines.get(cells.len()).copied().unwrap_or(0) > 0 {
            hrules.push(y);
            y += 1;
        }
        let height = y.max(1);
        let mut out = MBox::blank(width, height, (height - 1) / 2)?;

        for (r, row) in cells.iter().enumerate() {
            let (above, _) = extents.get(r).copied().unwrap_or((0, 0));
            let top = row_y.get(r).copied().unwrap_or(0);
            for (c, b) in row.iter().enumerate() {
                let cw = col_w.get(c).copied().unwrap_or(0);
                let dx = match column_align(grid, c) {
                    Align::Left => 0,
                    Align::Center => cw.saturating_sub(b.w) / 2,
                    Align::Right => cw.saturating_sub(b.w),
                };
                let cx = col_x.get(c).copied().unwrap_or(0);
                out.place(cx + dx, top + above - b.base, b);
            }
        }
        let delim = Attrs::role(MathRole::Delim);
        for &hy in &hrules {
            for x in 0..width {
                out.set(x, hy, Cell::new('─', delim));
            }
        }
        for &(rx, dashed) in &rules {
            for y in 0..height {
                let ch = if hrules.contains(&y) {
                    crossing(rx == 0, rx + 1 == width, y == 0, y + 1 == height)
                } else if dashed {
                    '┆'
                } else {
                    '│'
                };
                out.set(rx, y, Cell::new(ch, delim));
            }
        }

        match grid.kind {
            GridKind::Cases { left } => {
                let brace = self.delimiter(if left { '{' } else { '}' }, out.h, out.base)?;
                let space = MBox::blank(1, 1, 0)?;
                if left {
                    MBox::hcat(&[brace, space, out])
                } else {
                    MBox::hcat(&[out, space, brace])
                }
            }
            _ => Some(out),
        }
    }
}

/// `f` applied to an optional value: `Some(None)` when there is no value,
/// `None` when `f` fails.
fn each<T, U>(value: Option<T>, f: impl FnOnce(T) -> Option<U>) -> Option<Option<U>> {
    match value {
        None => Some(None),
        Some(v) => f(v).map(Some),
    }
}

/// Attach scripts to a base (see the module docs).
fn attach(base: MBox, sub: Option<Script>, sup: Option<Script>) -> Option<MBox> {
    let tall = base.h > 1;
    let raised = |s: &Option<Script>| match s {
        Some(Script::Raised(b)) => b.h,
        _ => 0,
    };
    let above = raised(&sup);
    let below = raised(&sub);
    // Both Unicode on a one-row base: side by side, subscript first (`x₁²`).
    let inline_pair =
        !tall && matches!(sub, Some(Script::Unicode(_))) && matches!(sup, Some(Script::Unicode(_)));
    let sub_w = sub.as_ref().map_or(0, Script::width);
    let sup_w = sup.as_ref().map_or(0, Script::width);
    let scripts_w = if inline_pair {
        sub_w + sup_w
    } else {
        sub_w.max(sup_w)
    };
    let mut out = MBox::blank(
        base.w + scripts_w,
        above + base.h + below,
        above + base.base,
    )?;
    out.place(0, above, &base);
    let x = base.w;
    match &sub {
        Some(Script::Unicode(b)) => {
            let y = if tall {
                above + base.h - 1
            } else {
                above + base.base
            };
            out.place(x, y, b);
        }
        Some(Script::Raised(b)) => out.place(x, above + base.h, b),
        None => {}
    }
    match &sup {
        Some(Script::Unicode(b)) => {
            let y = if tall { above } else { above + base.base };
            out.place(if inline_pair { x + sub_w } else { x }, y, b);
        }
        Some(Script::Raised(b)) => out.place(x, 0, b),
        None => {}
    }
    Some(out)
}

/// Stack rows flush left, with a blank row between rows taller than one.
fn stack_left(rows: &[MBox]) -> Option<MBox> {
    let w = rows.iter().map(|r| r.w).max().unwrap_or(0);
    let mut ys = Vec::with_capacity(rows.len());
    let mut y = 0;
    let mut prev_tall: Option<bool> = None;
    for r in rows {
        let tall = r.h > 1;
        if prev_tall.is_some_and(|p| p || tall) {
            y += 1;
        }
        prev_tall = Some(tall);
        ys.push(y);
        y += r.h;
    }
    let base = rows.first().map_or(0, |r| r.base);
    let mut out = MBox::blank(w, y, base)?;
    for (r, &y) in rows.iter().zip(&ys) {
        out.place(0, y, r);
    }
    Some(out)
}

/// A horizontal slot of a grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// Content column `c`.
    Cells(usize),
    /// Blank columns.
    Gap(usize),
    /// A vertical rule, dashed or not.
    Rule(bool),
}

/// The horizontal layout of a grid's columns.
fn column_slots(grid: &Grid, ncols: usize) -> Vec<Slot> {
    let mut slots = Vec::new();
    let mut next = 0;
    let push_cells = |slots: &mut Vec<Slot>, c: usize| {
        if matches!(slots.last(), Some(Slot::Cells(_))) {
            let gap = if grid.kind == GridKind::Aligned && c % 2 == 1 {
                0
            } else {
                2
            };
            if gap > 0 {
                slots.push(Slot::Gap(gap));
            }
        }
        slots.push(Slot::Cells(c));
    };
    for column in &grid.columns {
        match column {
            Column::Cells(_) if next < ncols => {
                push_cells(&mut slots, next);
                next += 1;
            }
            Column::Cells(_) => {}
            Column::Rule { dashed } => {
                // Rules side by side (`||`) touch.
                if matches!(slots.as_slice(), [.., Slot::Rule(_), Slot::Gap(_)]) {
                    slots.pop();
                } else if !slots.is_empty() {
                    slots.push(Slot::Gap(1));
                }
                slots.push(Slot::Rule(*dashed));
                slots.push(Slot::Gap(1));
            }
        }
    }
    while next < ncols {
        push_cells(&mut slots, next);
        next += 1;
    }
    if matches!(slots.last(), Some(Slot::Gap(_))) {
        slots.pop();
    }
    slots
}

/// The alignment of column `c`.
fn column_align(grid: &Grid, c: usize) -> Align {
    match grid.kind {
        GridKind::Matrix(a) | GridKind::Substack(a) => a,
        GridKind::Cases { .. } => Align::Left,
        GridKind::Aligned if c.is_multiple_of(2) => Align::Right,
        GridKind::Aligned => Align::Left,
        GridKind::Gathered => Align::Center,
        GridKind::Array => grid
            .columns
            .iter()
            .filter_map(|col| match col {
                Column::Cells(a) => Some(*a),
                Column::Rule { .. } => None,
            })
            .nth(c)
            .unwrap_or(Align::Center),
    }
}

/// Where a vertical rule meets a horizontal one.
fn crossing(left: bool, right: bool, top: bool, bottom: bool) -> char {
    match (left, right, top, bottom) {
        (true, _, true, _) => '┌',
        (true, _, _, true) => '└',
        (true, ..) => '├',
        (_, true, true, _) => '┐',
        (_, true, _, true) => '┘',
        (_, true, ..) => '┤',
        (_, _, true, _) => '┬',
        (_, _, _, true) => '┴',
        _ => '┼',
    }
}

/// The arrow a stretchable base (`\xrightarrow`) is drawn with.
fn stretchable_arrow(base: &Node) -> Option<char> {
    match base.as_atom() {
        Some(Atom::Rel(r)) => match r.as_str() {
            "→" | "⟶" => Some('→'),
            "←" | "⟵" => Some('←'),
            "↔" | "⟷" => Some('↔'),
            "⇒" | "⟹" => Some('⇒'),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `node` is (behind groups and font switches) a fraction with a bar.
fn is_fraction(node: &Node) -> bool {
    matches!(linear::unwrap(node), Node::Frac { bar: true, .. })
}

/// Whether a `\substack` occurs in `node` (it is laid out in 2D even in
/// limits).
fn has_substack(node: &Node, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match node {
        Node::Grid(g) => matches!(g.kind, GridKind::Substack(_)),
        Node::Row(items) => items.iter().any(|i| has_substack(i, depth + 1)),
        Node::Styled { body, .. } => has_substack(body, depth + 1),
        _ => false,
    }
}

/// Top-level items with font switches pushed down onto each item, so that
/// splitting at relations sees through `\color{…}` and `\bf`.
fn flatten(items: &[Node]) -> Vec<Node> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Node::Styled { font, body } => {
                for inner in flatten(body.items()) {
                    out.push(Node::Styled {
                        font: *font,
                        body: Box::new(inner),
                    });
                }
            }
            other => out.push(other.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display as public_display;

    fn rows(tex: &str, avail: u16) -> Vec<String> {
        match public_display_box(tex, avail) {
            Some(b) => b
                .rows
                .iter()
                .map(|r| r.text.trim_end().to_string())
                .collect(),
            None => panic!("{tex}: no box"),
        }
    }

    fn public_display_box(tex: &str, avail: u16) -> Option<crate::MathBox> {
        match public_display(tex, &MathOptions::default(), avail) {
            MathDisplay::Box(b) => Some(b),
            _ => None,
        }
    }

    #[test]
    fn approved_sum() {
        let b = public_display_box(r"\sum_{i=1}^{n} i^2 = \frac{n(n+1)(2n+1)}{6}", 80).unwrap();
        assert_eq!((b.width, b.height, b.baseline), (21, 3, 1));
        let r: Vec<&str> = b.rows.iter().map(|r| r.text.trim_end()).collect();
        assert_eq!(
            r,
            [
                " n       n(n+1)(2n+1)",
                " ∑  i² = ────────────",
                "i=1           6"
            ]
        );
    }

    #[test]
    fn approved_matrix_and_cases() {
        assert_eq!(
            rows(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}", 80),
            ["⎛a  b⎞", "⎝c  d⎠"]
        );
        assert_eq!(
            rows(r"A = \begin{pmatrix} a & b \\ c & d \end{pmatrix}", 80),
            ["A = ⎛a  b⎞", "    ⎝c  d⎠"]
        );
        assert_eq!(
            rows(
                r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}",
                80
            ),
            ["       ⎧ 1  x > 0", "f(x) = ⎨", "       ⎩ 0  otherwise"]
        );
    }

    #[test]
    fn scripts_in_2d() {
        assert_eq!(rows(r"e^{-x^2}", 80), [" −x²", "e"]);
        assert_eq!(rows(r"x_1^2", 80), ["x₁²"]);
        assert_eq!(rows(r"x_N", 80), ["x", " N"]);
        assert_eq!(rows(r"x_1^{i\pi}", 80), [" iπ", "x₁"]);
        assert_eq!(
            rows(r"\left(\frac{a}{b}\right)^2", 80),
            ["⎛a⎞²", "⎜─⎟", "⎝b⎠"]
        );
    }

    #[test]
    fn integrals_and_limits() {
        assert_eq!(
            rows(r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}", 80),
            ["⌠∞  −x²      √π", "⎮  e    dx = ──", "⌡0           2"]
        );
        assert_eq!(
            rows(r"\lim_{x \to 0} \frac{\sin x}{x} = 1", 80),
            ["    sin x", "lim ───── = 1", "x→0   x"]
        );
    }

    #[test]
    fn radicals() {
        assert_eq!(rows(r"\sqrt{x^2+1}", 80), [" ▁▁▁▁", "√x²+1"]);
        assert_eq!(rows(r"\sqrt{x}", 80), ["√x"]);
        assert_eq!(rows(r"\sqrt[3]{x}", 80), ["∛x"]);
        assert_eq!(rows(r"\sqrt[n]{x}", 80), ["ⁿ√x"]);
        assert_eq!(rows(r"\sqrt[n]{x+1}", 80), ["  ▁▁▁", "ⁿ√x+1"]);
        assert_eq!(rows(r"\sqrt[\pi]{x+1}", 80), ["π ▁▁▁", " √x+1"]);
        assert_eq!(
            rows(r"\sqrt{\frac{a}{b}}", 80),
            ["    ▁", "   ╱a", "  ╱ ─", "╲╱  b"]
        );
    }

    #[test]
    fn braces_and_accents() {
        assert_eq!(rows(r"\overbrace{a+b}^{n}", 80), ["  n", "╭─┴─╮", "a + b"]);
        assert_eq!(rows(r"\underbrace{a+b}_{n}", 80), ["a + b", "╰─┬─╯", "  n"]);
        assert_eq!(rows(r"\overline{AB}", 80), ["▁▁", "AB"]);
        assert_eq!(rows(r"\hat{x}", 80), ["x\u{302}"]);
        assert_eq!(rows(r"\xrightarrow{f}", 80), [" f", "──→"]);
    }

    #[test]
    fn fallback_ladder() {
        let tex = r"a + b = c + d = e + f";
        assert!(matches!(
            public_display(tex, &MathOptions::default(), 80),
            MathDisplay::Box(_)
        ));
        // Too narrow for one row: split before the relations, aligned.
        assert_eq!(rows(tex, 13), ["a + b = c + d", "      = e + f"]);
        // The aligned rows do not fit: indent by two instead.
        assert_eq!(rows(tex, 12), ["a + b", "  = c + d", "  = e + f"]);
        // Narrower than any chunk with the aligned indent: indent 2.
        assert_eq!(rows(r"xxxxxxxx = y + z", 10), ["xxxxxxxx", "  = y + z"]);
        // Too narrow for any 2D row: linear lines.
        match public_display(r"\frac{aaaa+bbbb}{c} + d", &MathOptions::default(), 6) {
            MathDisplay::Lines(lines) => {
                let text: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
                assert_eq!(text, ["(aaaa+bbbb)/c +", "d"]);
            }
            other => panic!("{other:?}"),
        }
        // Too tall: linear.
        let tall = r"\frac{1}{\frac{1}{\frac{1}{\frac{1}{x}}}}";
        let short = MathOptions {
            max_height: 5,
            ..MathOptions::default()
        };
        assert!(matches!(
            public_display(tall, &short, 80),
            MathDisplay::Lines(_)
        ));
        assert!(matches!(
            public_display(tall, &MathOptions::default(), 80),
            MathDisplay::Box(_)
        ));
        // Ambiguous-wide terminals get the linear form.
        let wide = MathOptions {
            ambiguous_wide: true,
            ..MathOptions::default()
        };
        assert!(matches!(
            public_display(r"\frac12", &wide, 80),
            MathDisplay::Lines(_)
        ));
    }

    #[test]
    fn nested_fraction_bars_overhang() {
        assert_eq!(
            rows(r"\frac{\frac{a}{b}}{\frac{c}{d}}", 80),
            [" a", " ─", " b", "───", " c", " ─", " d"]
        );
        // Not for a fraction inside a longer part.
        assert_eq!(
            rows(r"\frac{1}{1+\frac{1}{x}}", 80),
            [" 1", "───", "  1", "1+─", "  x"]
        );
    }

    #[test]
    fn substack_limits_are_tight() {
        assert_eq!(
            rows(r"\sum_{\substack{i<n\\j<m}} a", 80),
            [" ∑  a", "i<n", "j<m"]
        );
    }

    #[test]
    fn tags() {
        let b = public_display_box(r"E = mc^2 \tag{1}", 20).unwrap();
        assert_eq!(b.width, 20);
        let r: Vec<&str> = b.rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(r, ["      E = mc²    (1)"]);
        let b = public_display_box(r"E = mc^2 \tag{1}", 9).unwrap();
        let r: Vec<&str> = b.rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(r, [" E = mc² ", "      (1)"]);
    }

    #[test]
    fn wrap_splits_at_breaks() {
        let line = crate::inline(r"a + b = c", &MathOptions::default());
        let parts = wrap(&line, 5, false);
        let text: Vec<&str> = parts.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text, ["a +", "b = c"]);
        for part in &parts {
            assert_eq!(
                part.spans.last().map(|s| s.end as usize),
                Some(part.text.len())
            );
            assert_eq!(usize::from(part.width), str_width(&part.text, false));
        }
        assert_eq!(wrap(&line, 80, false), std::slice::from_ref(&line));
        assert_eq!(wrap(&line, 0, false).len(), 3);
    }

    /// Trailing whitespace is measured in columns: U+00A0 is two bytes but
    /// one column, and subtracting its byte length from a width underflowed
    /// (a panic in debug builds).
    #[test]
    fn wrap_measures_trailing_whitespace_in_columns() {
        let line = crate::inline("aaaa+\\char\"A0", &MathOptions::default());
        assert_eq!(line.text, "aaaa + \u{A0}");
        let parts = wrap(&line, 3, false);
        let text: Vec<&str> = parts.iter().map(|l| l.text.as_str()).collect();
        // The whitespace-only piece stays on the line it ends.
        assert_eq!(text, ["aaaa +"]);
        assert!(matches!(
            public_display("aaaa+\\char\"A0", &MathOptions::default(), 3),
            MathDisplay::Lines(_)
        ));
    }

    #[test]
    fn multi_row_blocks_fall_back_row_by_row() {
        let lines =
            |tex: &str, avail: u16| match public_display(tex, &MathOptions::default(), avail) {
                MathDisplay::Lines(lines) => lines.into_iter().map(|l| l.text).collect::<Vec<_>>(),
                other => panic!("{tex}: {other:?}"),
            };
        assert_eq!(
            lines(
                r"\begin{aligned} f(x) &= a+b+c+d \\ &= e+f+g+h \end{aligned}",
                12
            ),
            ["f(x) = a +", "b + c + d", "= e + f +", "g + h"]
        );
        assert_eq!(
            lines(r"a + b + c = d \\ e = f", 12),
            ["a + b + c =", "d", "e = f"]
        );
        // The tag follows the last row.
        assert_eq!(
            lines(r"a + b + c = d \\ e = f \tag{2}", 12),
            ["a + b + c =", "d", "e = f  (2)"]
        );
    }

    #[test]
    fn one_character_radicands_get_a_vinculum_when_followed() {
        assert_eq!(rows(r"\sqrt{2}\pi", 80), [" ▁", "√2π"]);
        assert_eq!(rows(r"\sqrt{x}^2", 80), [" ▁²", "√x"]);
        assert_eq!(rows(r"\sqrt{2} + x", 80), ["√2 + x"]);
        assert_eq!(rows(r"2\sqrt{2}", 80), ["2√2"]);
    }

    #[test]
    fn adjacent_array_rules_touch() {
        assert_eq!(
            rows(r"\begin{array}{c||c} a & b \\ \hline c & d \end{array}", 80),
            ["a ││ b", "──┼┼──", "c ││ d"]
        );
    }

    #[test]
    fn typed_combining_marks_share_their_base_cell() {
        assert_eq!(rows("x\u{301} + 1", 80), ["x\u{301} + 1"]);
        assert_eq!(rows("a =\u{338} b", 80), ["a ≠ b"]);
        let b = public_display_box("x\u{301}", 80).unwrap();
        assert_eq!(b.width, 1);
        // On a base that is already two rows tall (found by proptest).
        let b = public_display_box("\\vec{\u{1F468}}\u{200D}", 80).unwrap();
        for row in &b.rows {
            assert_eq!(str_width(&row.text, false), usize::from(b.width));
        }
    }
}
