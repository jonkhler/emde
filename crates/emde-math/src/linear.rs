//! Linear rendering: one line of Unicode, for inline math and as the display
//! fallback.
//!
//! | Construct | Rule | Example |
//! |---|---|---|
//! | Spacing | [`spacing`]: a column around binary operators and relations, none in scripts, fractions and radicands, none after a unary sign | `a − b = −c` |
//! | Fractions | `a/b`, parenthesising compound parts; digit/digit as a vulgar fraction ([`Fractions`]); the whole fraction in parentheses before juxtaposed terms or after a function name | `(a+b)/c`, `½`, `(a/b)c`, `log (a/b)` |
//! | Scripts | all-or-nothing Unicode (letters mapped from their plain form), else dim `^(…)`/`_(…)` (`^q` for one character that nothing follows directly); subscript first; primes, `°`, `*`, `†` stay inline; a fraction or root base in parentheses | `A⁻¹`, `x^(n+q)`, `x_N`, `∇_(θ)J`, `90°`, `(a/b)²` |
//! | Roots | `√x`, `√(…)`, `∛`, `∜`, `ⁿ√`, else `x^(1/k)`; a bare radicand in parentheses when something follows directly | `√(x²+1)`, `√(2)π` |
//! | Operators | limits as scripts to the right | `∑ᵢ₌₁ⁿ`, `lim_(x→0)` |
//! | Accents, `\not` | precomposed when NFC has it, else a combining mark | `â`, `∉` |
//! | Environments | `(a b; c d)`, commas between cells that have spaces, `{1, x > 0; 0, otherwise}`; aligned rows keep their column pairs apart and run on when they start with an operator | `(a + b, c)`, `x = 1  y = 2`, `a = b = c` |
//!
//! Every span has a role. Line breaks are allowed after top-level binary
//! operators and relations (the break offset is where the next line starts),
//! including those in the rows of a top-level `aligned` or `gathered`
//! block, and after the `; ` between its rows.

use crate::ast::{
    Accent, Atom, Brace, BraceShape, Font, Formula, Grid, GridKind, Limits, Node, Side,
};
use crate::spacing::{self, Class};
use crate::style;
use crate::tables;
use crate::width::{clusters, is_zero_width, str_width, to_u16};
use crate::{Fractions, MathLine, MathOptions, MathRole, MathSpan};

/// Render a formula as one line.
pub(crate) fn render(formula: &Formula, opts: &MathOptions) -> MathLine {
    let r = Linear { opts };
    let mut out = Frag::default();
    r.node(&formula.body, Ctx::top(), &mut out);
    if let Some(tag) = &formula.tag {
        out.push("  ", Attrs::PLAIN);
        out.push(tag, Attrs::role(MathRole::Text));
    }
    out.into_line(opts.ambiguous_wide)
}

/// Span attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attrs {
    pub(crate) role: MathRole,
    pub(crate) bold: bool,
    pub(crate) dim: bool,
}

impl Attrs {
    pub(crate) const PLAIN: Attrs = Attrs::role(MathRole::Plain);
    const DELIM: Attrs = Attrs::role(MathRole::Delim);
    /// Fallback notation such as `^(`.
    const DIM: Attrs = Attrs {
        role: MathRole::Plain,
        bold: false,
        dim: true,
    };

    pub(crate) const fn role(role: MathRole) -> Attrs {
        Attrs {
            role,
            bold: false,
            dim: false,
        }
    }

    pub(crate) fn of(span: &MathSpan) -> Attrs {
        Attrs {
            role: span.role,
            bold: span.bold,
            dim: span.dim,
        }
    }
}

/// A styled piece of text under construction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Frag {
    pub(crate) text: String,
    /// Contiguous spans covering `text`.
    pub(crate) spans: Vec<MathSpan>,
    /// Byte offsets where a line may break.
    pub(crate) breaks: Vec<u32>,
}

impl Frag {
    fn end(&self) -> u32 {
        u32::try_from(self.text.len()).unwrap_or(u32::MAX)
    }

    /// Append `s` with `attrs`, extending the last span when it matches.
    pub(crate) fn push(&mut self, s: &str, attrs: Attrs) {
        if s.is_empty() {
            return;
        }
        self.text.push_str(s);
        let end = self.end();
        match self.spans.last_mut() {
            Some(last) if Attrs::of(last) == attrs => last.end = end,
            _ => self.spans.push(MathSpan {
                end,
                role: attrs.role,
                bold: attrs.bold,
                dim: attrs.dim,
            }),
        }
    }

    fn push_char(&mut self, c: char, attrs: Attrs) {
        let mut buf = [0; 4];
        self.push(c.encode_utf8(&mut buf), attrs);
    }

    fn push_spaces(&mut self, n: usize) {
        for _ in 0..n {
            self.push(" ", Attrs::PLAIN);
        }
    }

    /// Remove a trailing space; `false` when there is none.
    fn pop_space(&mut self) -> bool {
        if !self.text.ends_with(' ') {
            return false;
        }
        self.text.pop();
        let end = self.end();
        if let Some(last) = self.spans.last_mut() {
            last.end = end;
        }
        let empty = self.spans.len() >= 2
            && self.spans.get(self.spans.len() - 2).map(|s| s.end) == Some(end);
        if empty || self.spans.last().is_some_and(|s| s.end == 0) {
            self.spans.pop();
        }
        self.breaks.retain(|&b| b <= end);
        true
    }

    /// Append another fragment.
    pub(crate) fn append(&mut self, other: Frag) {
        let base = self.end();
        for (piece, attrs) in other.pieces() {
            self.push(piece, attrs);
        }
        self.breaks
            .extend(other.breaks.iter().map(|b| b.saturating_add(base)));
    }

    fn mark_break(&mut self) {
        let end = self.end();
        if end > 0 && self.breaks.last() != Some(&end) {
            self.breaks.push(end);
        }
    }

    /// The fragment without its leading spaces.
    fn trim_start(self) -> Frag {
        let cut = self.text.len() - self.text.trim_start_matches(' ').len();
        if cut == 0 {
            return self;
        }
        let mut out = Frag::default();
        let mut skip = cut;
        for (piece, attrs) in self.pieces() {
            // Only ASCII spaces are skipped, so this is a char boundary.
            let n = skip.min(piece.len());
            skip -= n;
            out.push(piece.get(n..).unwrap_or(""), attrs);
        }
        let shift = u32::try_from(cut).unwrap_or(u32::MAX);
        out.breaks = self
            .breaks
            .iter()
            .filter_map(|b| b.checked_sub(shift))
            .filter(|&b| b > 0)
            .collect();
        out
    }

    /// `(text, attrs)` for each span.
    pub(crate) fn pieces(&self) -> impl Iterator<Item = (&str, Attrs)> + '_ {
        let mut start = 0usize;
        self.spans.iter().filter_map(move |span| {
            let end = usize::try_from(span.end).ok()?;
            let piece = self.text.get(start..end)?;
            start = end;
            Some((piece, Attrs::of(span)))
        })
    }

    /// Rebuild the fragment with every cluster transformed by `f` (keeping
    /// span attributes); `None` if `f` refuses any cluster.
    fn map_clusters(&self, mut f: impl FnMut(&str) -> Option<String>) -> Option<Frag> {
        let mut out = Frag::default();
        for (piece, attrs) in self.pieces() {
            for (cluster, _) in clusters(piece, false) {
                out.push(&f(cluster)?, attrs);
            }
        }
        Some(out)
    }

    /// The number of clusters in the text.
    pub(crate) fn cluster_count(&self) -> usize {
        clusters(&self.text, false).count()
    }

    pub(crate) fn width(&self, cjk: bool) -> usize {
        str_width(&self.text, cjk)
    }

    pub(crate) fn into_line(self, cjk: bool) -> MathLine {
        let width = to_u16(self.width(cjk));
        MathLine {
            text: self.text,
            spans: self.spans,
            breaks: self.breaks,
            width,
            ok: true,
        }
    }
}

/// Rendering context.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ctx {
    /// No spacing around binary operators and relations (scripts, fraction
    /// parts, radicands).
    pub(crate) tight: bool,
    /// The font in effect.
    pub(crate) font: Font,
    /// In the formula's outermost row, where lines may break.
    pub(crate) top: bool,
    /// Something is set right after the node with no space between
    /// (juxtaposed material or a script of its own), so a one-character
    /// fallback script at its end needs parentheses: `∇_(θ)J`, not `∇_θJ`.
    pub(crate) followed: bool,
}

impl Ctx {
    pub(crate) fn top() -> Ctx {
        Ctx {
            tight: false,
            font: Font::Normal,
            top: true,
            followed: false,
        }
    }

    /// The context for a node nested inside another construction.
    fn inner(self) -> Ctx {
        Ctx {
            top: false,
            followed: false,
            ..self
        }
    }

    /// The context for a tightly spaced part (script, fraction part,
    /// radicand).
    pub(crate) fn tight(self) -> Ctx {
        Ctx {
            tight: true,
            top: false,
            followed: false,
            ..self
        }
    }

    /// The same context with [`Ctx::followed`] set.
    pub(crate) fn followed_by(self, followed: bool) -> Ctx {
        Ctx { followed, ..self }
    }
}

/// A script's content in the form it is shown in.
pub(crate) enum ScriptText {
    /// Unicode superscript or subscript characters (every character has
    /// one).
    Unicode(Frag),
    /// The tight linear form, for the fallback notation (`^(…)`) or a
    /// raised 2D box.
    Linear(Frag),
}

/// The linear renderer.
pub(crate) struct Linear<'o> {
    pub(crate) opts: &'o MathOptions,
}

impl Linear<'_> {
    /// Render `node` into a fresh fragment.
    pub(crate) fn frag(&self, node: &Node, ctx: Ctx) -> Frag {
        let mut out = Frag::default();
        self.node(node, ctx, &mut out);
        out
    }

    pub(crate) fn node(&self, node: &Node, ctx: Ctx, out: &mut Frag) {
        match node {
            Node::Atom(atom) => {
                let look = style::atom(atom, ctx.font, self.opts);
                out.push(
                    &look.text,
                    Attrs {
                        role: look.role,
                        bold: look.bold,
                        dim: false,
                    },
                );
            }
            Node::Row(items) => self.row(items, ctx, None, None, out),
            Node::Frac { num, den, bar } => self.frac(num, den, *bar, ctx, out),
            Node::Root { radicand, index } => self.root(radicand, index.as_deref(), ctx, out),
            Node::Scripts { base, sub, sup, .. } => {
                self.scripts(base, sub.as_deref(), sup.as_deref(), ctx, out);
            }
            Node::Accent { base, accent } => self.accent(base, *accent, ctx, out),
            Node::Fenced { open, close, body } => {
                if let Some(open) = open {
                    out.push_char(*open, Attrs::DELIM);
                }
                self.node(body, ctx.inner(), out);
                if let Some(close) = close {
                    out.push_char(*close, Attrs::DELIM);
                }
            }
            Node::Grid(grid) => self.grid(grid, ctx, out),
            Node::Styled { font, body } => self.node(body, Ctx { font: *font, ..ctx }, out),
            Node::Space(n) => {
                if *n > 0 {
                    out.push_spaces(usize::from(n.unsigned_abs()));
                } else {
                    for _ in 0..n.unsigned_abs() {
                        out.pop_space();
                    }
                }
            }
            Node::Not(inner) => self.not(inner, ctx, out),
            Node::OverUnder {
                base,
                over,
                under,
                brace,
            } => self.over_under(base, over.as_deref(), under.as_deref(), *brace, ctx, out),
        }
    }

    /// A row with TeX spacing; `lead`/`trail` are virtual neighbours (see
    /// [`spacing::space`]).
    pub(crate) fn row(
        &self,
        items: &[Node],
        ctx: Ctx,
        lead: Option<Class>,
        trail: Option<Class>,
        out: &mut Frag,
    ) {
        let spacing = spacing::space(items, ctx.tight, lead, trail);
        let mut pending: i32 = 0;
        let mut break_before = false;
        for (i, item) in items.iter().enumerate() {
            if let Node::Space(n) = item {
                pending += i32::from(*n);
                continue;
            }
            pending += i32::try_from(spacing.before.get(i).copied().unwrap_or(0)).unwrap_or(0);
            flush(&mut pending, out);
            if break_before && ctx.top {
                out.mark_break();
            }
            // Font switches are transparent: their content is still top-level.
            // So are the rows of an `aligned` or `gathered` block.
            let child = if matches!(item, Node::Styled { .. }) || is_block(item) {
                ctx
            } else {
                ctx.inner()
            };
            let child = child.followed_by(followed(items, &spacing, i).unwrap_or(ctx.followed));
            if matches!(item, Node::Frac { bar: true, .. })
                && fraction_needs_parens(items, &spacing, i)
            {
                operand(self.frag(item, child), true, out);
            } else {
                self.node(item, child, out);
            }
            break_before = spacing.breaks_after(i);
        }
        pending += i32::try_from(spacing.after).unwrap_or(0);
        flush(&mut pending, out);
    }

    /// A base with scripts to its right, subscript first (`x₁²`). A fraction
    /// or root base is parenthesised because its scripts apply to all of it
    /// (`(a/b)²`, `(√x)²`). A one-character fallback script is parenthesised
    /// when something follows it directly: a Unicode superscript
    /// (`x_(N)²`) or juxtaposed material (see [`Ctx::followed`]).
    fn scripts(
        &self,
        base: &Node,
        sub: Option<&Node>,
        sup: Option<&Node>,
        ctx: Ctx,
        out: &mut Frag,
    ) {
        if matches!(
            unwrap(base),
            Node::Frac { bar: true, .. } | Node::Root { .. }
        ) {
            operand(self.frag(base, ctx.inner()), true, out);
        } else {
            self.node(base, ctx.inner().followed_by(true), out);
        }
        self.script_pair(sub, sup, false, ctx, out);
    }

    fn frac(&self, num: &Node, den: &Node, bar: bool, ctx: Ctx, out: &mut Frag) {
        let tight = ctx.tight();
        if !bar {
            // `\binom` (its parentheses are a surrounding `Fenced`).
            self.node(num, tight, out);
            out.push(" choose ", Attrs::role(MathRole::Text));
            self.node(den, tight, out);
            return;
        }
        if let Some(v) = self.vulgar(num, den) {
            out.push(&v, Attrs::role(MathRole::Num));
            return;
        }
        let num_frag = self.frag(num, tight);
        let den_frag = self.frag(den, tight);
        operand(num_frag, is_compound(num), out);
        out.push("/", Attrs::DELIM);
        operand(den_frag, is_compound(den), out);
    }

    /// `½` for `\frac12`, `⅟16` for `\frac{1}{16}` (with [`Fractions::Vulgar`]).
    fn vulgar(&self, num: &Node, den: &Node) -> Option<String> {
        if self.opts.fractions != Fractions::Vulgar {
            return None;
        }
        let (Some(Atom::Num(n)), Some(Atom::Num(d))) = (num.as_atom(), den.as_atom()) else {
            return None;
        };
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !digits(n) || !digits(d) {
            return None;
        }
        if let Some(v) = tables::vulgar_fraction(n, d) {
            return Some(v.to_string());
        }
        (n == "1" && d.len() > 1).then(|| format!("{}{d}", tables::NUMERATOR_ONE))
    }

    /// `√x`, `√(…)`, `∛x`, `∜x`, `ⁿ√x` when the index has superscript
    /// forms, else `x^(1/k)`. A bare radicand that something follows
    /// directly is parenthesised too: `√(2)π`, not `√2π`.
    fn root(&self, radicand: &Node, index: Option<&Node>, ctx: Ctx, out: &mut Frag) {
        let body = self.frag(radicand, ctx.tight());
        let paren = !body.text.is_empty()
            && !delimited(radicand)
            && (ctx.followed
                || (body.cluster_count() > 1 && !matches!(radicand.as_atom(), Some(Atom::Num(_)))));
        if let Some(sign) = plain_radical(index) {
            out.push_char(sign, Attrs::DELIM);
        } else if let Some(index) = index {
            match self.script_text(index, true, ctx) {
                ScriptText::Unicode(mapped) => {
                    out.append(mapped);
                    out.push_char('√', Attrs::DELIM);
                }
                ScriptText::Linear(k) => {
                    // x^(1/k)
                    let paren = paren && body.cluster_count() > 1;
                    parenthesised(body, paren, out);
                    out.push("^(", Attrs::DIM);
                    out.push("1", Attrs::role(MathRole::Num));
                    out.push("/", Attrs::DELIM);
                    let compound = k.cluster_count() > 1;
                    operand(k, compound, out);
                    out.push(")", Attrs::DIM);
                    return;
                }
            }
        }
        parenthesised(body, paren, out);
    }

    /// `node` as a script: in Unicode superscript or subscript characters
    /// when every character has one, else its tight linear form.
    ///
    /// The content is rendered once and the mapping works on that text.
    /// Rendering it again for the fallback would double the work at every
    /// level of nested scripts.
    pub(crate) fn script_text(&self, node: &Node, sup: bool, ctx: Ctx) -> ScriptText {
        let frag = self.frag(node, ctx.tight());
        match self.map_script(&frag, sup) {
            Some(mapped) => ScriptText::Unicode(mapped),
            None => ScriptText::Linear(frag),
        }
    }

    /// `frag` in superscript or subscript characters, if all of them map.
    /// Math italic and bold letters (from
    /// [`Letters::UnicodeItalic`](crate::Letters::UnicodeItalic) and
    /// [`Bold::Unicode`](crate::Bold::Unicode)) map from their plain form,
    /// as `𝑥` has no superscript but `x` has `ˣ`; bold ones keep the `bold`
    /// flag.
    fn map_script(&self, frag: &Frag, sup: bool) -> Option<Frag> {
        let set = self.opts.scripts;
        let mut out = Frag::default();
        for (piece, attrs) in frag.pieces() {
            for (cluster, _) in clusters(piece, false) {
                let mut chars = cluster.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    return None;
                };
                let (c, bold) = tables::unstyled(c).unwrap_or((c, false));
                let mapped = if sup {
                    tables::superscript(c, set)
                } else {
                    tables::subscript(c, set)
                }?;
                let attrs = Attrs {
                    bold: attrs.bold || bold,
                    ..attrs
                };
                out.push_char(mapped, attrs);
            }
        }
        Some(out)
    }

    fn accent(&self, base: &Node, accent: Accent, ctx: Ctx, out: &mut Frag) {
        let body = self.frag(base, ctx.inner());
        if body.text.is_empty() {
            out.push_char(accent.ch, Attrs::PLAIN);
            return;
        }
        let Some(mark) = tables::accent_mark(accent, body.cluster_count()) else {
            out.append(body);
            return;
        };
        out.append(with_mark(&body, mark, |c| tables::compose(c, mark)));
    }

    fn not(&self, inner: &Node, ctx: Ctx, out: &mut Frag) {
        let body = self.frag(inner, ctx.inner());
        out.append(with_mark(&body, tables::NEGATION_MARK, tables::negate));
    }

    fn over_under(
        &self,
        base: &Node,
        over: Option<&Node>,
        under: Option<&Node>,
        brace: Option<Brace>,
        ctx: Ctx,
        out: &mut Frag,
    ) {
        if let (None, Some(over), None, Some(Atom::Rel(rel))) = (brace, over, under, base.as_atom())
            && let Some(c) = tables::overset_composite(&plain_text(over), rel)
        {
            out.push_char(c, Attrs::role(MathRole::Rel));
            return;
        }
        self.node(base, ctx.inner(), out);
        if let Some(brace) = brace {
            out.push_char(brace_glyph(brace), Attrs::DIM);
        }
        self.script_pair(under, over, true, ctx, out);
    }

    /// A subscript and/or superscript, the subscript first. With `dim`, the
    /// Unicode forms are dim too (labels of `\overset` and braces).
    fn script_pair(
        &self,
        sub: Option<&Node>,
        sup: Option<&Node>,
        dim: bool,
        ctx: Ctx,
        out: &mut Frag,
    ) {
        let sup = sup.map(|s| self.script_text(s, true, ctx));
        if let Some(sub) = sub {
            // A one-character fallback would run into a Unicode superscript.
            let followed = match &sup {
                Some(ScriptText::Unicode(mapped)) => !mapped.text.is_empty(),
                Some(ScriptText::Linear(_)) => false,
                None => ctx.followed,
            };
            emit_script(self.script_text(sub, false, ctx), false, dim, followed, out);
        }
        if let Some(sup) = sup {
            emit_script(sup, true, dim, ctx.followed, out);
        }
    }

    fn grid(&self, grid: &Grid, ctx: Ctx, out: &mut Frag) {
        // Cells are spaced normally, except in `\substack` (always in
        // limits). The rows of a top-level `aligned` or `gathered` block may
        // break like the top level itself.
        let cell_ctx = Ctx {
            tight: ctx.tight && matches!(grid.kind, GridKind::Substack(_)),
            top: ctx.top && is_block_kind(grid.kind),
            ..ctx.inner()
        };
        let (open, row_sep, close) = match grid.kind {
            GridKind::Cases { .. } => ("{", "; ", "}"),
            GridKind::Substack(_) => ("", ", ", ""),
            _ => ("", "; ", ""),
        };
        out.push(open, Attrs::DELIM);
        for (r, (row, continues)) in self.grid_rows(grid, cell_ctx).into_iter().enumerate() {
            if r > 0 && !continues {
                out.push(row_sep, Attrs::PLAIN);
                if cell_ctx.top {
                    out.mark_break();
                }
            }
            out.append(row);
        }
        out.push(close, Attrs::DELIM);
    }

    /// The rows of `grid`, each on one line, and whether each continues the
    /// row before (see [`continues_row`]). The cells of an `aligned` row
    /// pair up into expressions (`x &= 1`), with two columns between pairs.
    /// Other environments separate their cells with a space (`a b`), or
    /// with a comma when a cell has spaces of its own (`a + b, c`), as cases
    /// always do (`1, x > 0`). Every cell is rendered once.
    fn grid_rows(&self, grid: &Grid, ctx: Ctx) -> Vec<(Frag, bool)> {
        if grid.kind == GridKind::Aligned {
            return grid
                .rows
                .iter()
                .enumerate()
                .map(|(r, row)| {
                    let continues = r > 0 && continues_row(grid, row);
                    (self.aligned_row(row, ctx, continues), continues)
                })
                .collect();
        }
        let cells: Vec<Vec<Frag>> = grid
            .rows
            .iter()
            .map(|row| row.iter().map(|cell| self.frag(cell, ctx)).collect())
            .collect();
        let spaced = cells.iter().flatten().any(|cell| cell.text.contains(' '));
        let sep = if spaced || matches!(grid.kind, GridKind::Cases { .. }) {
            ", "
        } else {
            " "
        };
        cells
            .into_iter()
            .map(|row| {
                let mut frag = Frag::default();
                for (c, cell) in row.into_iter().enumerate() {
                    if c > 0 {
                        frag.push(sep, Attrs::PLAIN);
                    }
                    frag.append(cell);
                }
                (frag, false)
            })
            .collect()
    }

    /// One row of an `aligned` block. A row that `continues` the one before
    /// (`&= c`, `&\quad + d`) keeps its leading operator spaced as it is
    /// after the previous row's content, and drops the explicit space that
    /// indents it in 2D.
    fn aligned_row(&self, row: &[Node], ctx: Ctx, continues: bool) -> Frag {
        let mut out = Frag::default();
        for (p, pair) in row.chunks(2).enumerate() {
            let mut items: Vec<Node> = pair
                .iter()
                .flat_map(|cell| cell.items().iter().cloned())
                .collect();
            let lead = if p == 0 && continues {
                let indent = items
                    .iter()
                    .take_while(|i| matches!(i, Node::Space(_)))
                    .count();
                items.drain(..indent);
                Some(Class::Ord)
            } else {
                None
            };
            if p > 0 {
                out.push("  ", Attrs::PLAIN);
            }
            self.row(&items, ctx, lead, None, &mut out);
        }
        out
    }

    /// The rows of a formula that is one multi-row block (`aligned`,
    /// `gathered`, or a bare `\\`), each on a line of its own with break
    /// hints at its top-level operators, for the display fallback. `None`
    /// for any other formula.
    pub(crate) fn rows(&self, body: &Node) -> Option<Vec<Frag>> {
        let (grid, font) = top_block(body, Font::Normal)?;
        let ctx = Ctx { font, ..Ctx::top() };
        let rows = self.grid_rows(grid, ctx);
        Some(rows.into_iter().map(|(row, _)| row.trim_start()).collect())
    }
}

/// Whether `node` is one delimited group, `\left(…\right)` or `(…)`, which
/// needs no parentheses of its own.
fn delimited(node: &Node) -> bool {
    let items = match unwrap(node) {
        Node::Fenced {
            open: Some(_),
            close: Some(_),
            ..
        } => return true,
        Node::Row(items) => items,
        _ => return false,
    };
    let side = |i: usize| match items.get(i) {
        Some(Node::Atom(Atom::Delim { side, .. })) => Some(*side),
        _ => None,
    };
    let Some(last) = items.len().checked_sub(1) else {
        return false;
    };
    if last == 0 || side(0) != Some(Side::Open) || side(last) != Some(Side::Close) {
        return false;
    }
    // The opening delimiter must close at the very end.
    let mut depth = 0usize;
    for i in 0..=last {
        match side(i) {
            Some(Side::Open) => depth += 1,
            Some(Side::Close) => {
                depth = depth.saturating_sub(1);
                if depth == 0 && i < last {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

/// Whether `node` is a multi-row block: an `aligned` or `gathered` grid.
fn is_block(node: &Node) -> bool {
    matches!(node, Node::Grid(grid) if is_block_kind(grid.kind))
}

/// Whether grids of this kind are multi-row blocks, whose rows are
/// expressions of their own.
fn is_block_kind(kind: GridKind) -> bool {
    matches!(kind, GridKind::Aligned | GridKind::Gathered)
}

/// The multi-row block (`aligned` or `gathered`) that `node` consists of,
/// looking through groups and font switches, with the font in effect.
fn top_block(node: &Node, font: Font) -> Option<(&Grid, Font)> {
    match node {
        Node::Row(items) => {
            let mut visible = items.iter().filter(|i| !i.is_empty());
            match (visible.next(), visible.next()) {
                (Some(only), None) => top_block(only, font),
                _ => None,
            }
        }
        Node::Styled { font, body } => top_block(body, *font),
        Node::Grid(grid) if is_block(node) => Some((grid, font)),
        _ => None,
    }
}

/// Whether an `aligned` row continues the expression of the row before:
/// it starts with a relation (`&= c`), or its left cell is empty and it
/// starts with a binary operator (`&\quad + d`; `-x &= 3` is a new row).
fn continues_row(grid: &Grid, row: &[Node]) -> bool {
    if grid.kind != GridKind::Aligned {
        return false;
    }
    let first = row
        .iter()
        .flat_map(Node::items)
        .find_map(spacing::edges)
        .map(|(left, _)| left);
    match first {
        Some(Class::Rel) => true,
        Some(Class::Bin) => row.first().is_none_or(Node::is_empty),
        _ => false,
    }
}

/// Whether something is set right after `items[i]` with no space between
/// (see [`Ctx::followed`]): the next item that renders, with no net space
/// before it. Delimiters and punctuation do not count: they cannot be
/// misread as part of a script (`p_θ(x)`, `(x_N)`, `x_N,`). `None` when
/// nothing follows in the row.
pub(crate) fn followed(items: &[Node], spacing: &spacing::Spacing, i: usize) -> Option<bool> {
    let mut gap = 0i32;
    for (j, item) in items.iter().enumerate().skip(i + 1) {
        if let Node::Space(n) = item {
            gap += i32::from(*n);
            continue;
        }
        let Some(Some((left, _))) = spacing.classes.get(j) else {
            continue;
        };
        gap += i32::try_from(spacing.before.get(j).copied().unwrap_or(0)).unwrap_or(0);
        return Some(gap <= 0 && !matches!(left, Class::Open | Class::Close | Class::Punct));
    }
    None
}

/// The radical sign when no index needs to be drawn: `√` without an index
/// (or for 2), `∛` and `∜` for 3 and 4.
pub(crate) fn plain_radical(index: Option<&Node>) -> Option<char> {
    let Some(index) = index.filter(|i| !i.is_empty()) else {
        return Some('√');
    };
    match index.as_atom() {
        Some(Atom::Num(n)) => match n.as_str() {
            "2" => Some('√'),
            "3" => Some('∛'),
            "4" => Some('∜'),
            _ => None,
        },
        _ => None,
    }
}

/// Whether the `a/b` fraction at `items[i]` must be parenthesised because
/// of its neighbours: juxtaposed material right after it (`(a/b)c`, not
/// `a/bc`, which reads as `a/(bc)`), or a function name before it
/// (`log (a/b)`). Limit-style operators (`lim`, `max`, and anything with
/// limits) take the whole expression anyway: `lim_(x→0) (sin x)/x`.
fn fraction_needs_parens(items: &[Node], spacing: &spacing::Spacing, i: usize) -> bool {
    let visible = |j: &usize| !matches!(items.get(*j), Some(Node::Space(_)));
    let next = (i + 1..items.len()).find(visible);
    // A `\middle|` divider is not a factor.
    let juxtaposed = next.is_some_and(|j| {
        j == i + 1
            && spacing.before.get(j) == Some(&0)
            && matches!(
                spacing.classes.get(j),
                Some(Some((Class::Ord | Class::Open, _)))
            )
            && !matches!(
                items.get(j),
                Some(Node::Atom(Atom::Delim {
                    side: Side::Middle,
                    ..
                }))
            )
    });
    let function = |node: &Node| match unwrap(node) {
        Node::Atom(Atom::Func(name)) => !is_limit_operator(name),
        _ => false,
    };
    let after_function = (0..i)
        .rev()
        .find(visible)
        .is_some_and(|j| match items.get(j) {
            Some(Node::Scripts {
                base,
                limits: Limits::Right,
                ..
            }) => function(base),
            Some(node) => function(node),
            None => false,
        });
    juxtaposed || after_function
}

/// Operators that pulldown-latex gives movable limits.
fn is_limit_operator(name: &str) -> bool {
    matches!(
        name,
        "lim" | "lim inf" | "lim sup" | "max" | "min" | "sup" | "inf" | "Pr" | "gcd"
    )
}

/// Write pending spaces; a negative balance takes back trailing spaces.
fn flush(pending: &mut i32, out: &mut Frag) {
    if *pending > 0 {
        out.push_spaces(usize::try_from(*pending).unwrap_or(0));
    } else {
        for _ in 0..pending.unsigned_abs() {
            if !out.pop_space() {
                break;
            }
        }
    }
    *pending = 0;
}

/// A fraction part or radicand, parenthesised when `paren` and longer than
/// one character.
fn operand(frag: Frag, paren: bool, out: &mut Frag) {
    let paren = paren && frag.cluster_count() > 1;
    parenthesised(frag, paren, out);
}

/// `frag`, in parentheses when `paren`.
fn parenthesised(frag: Frag, paren: bool, out: &mut Frag) {
    if paren {
        out.push("(", Attrs::DELIM);
    }
    out.append(frag);
    if paren {
        out.push(")", Attrs::DELIM);
    }
}

/// Write a script: its Unicode form (dim when `dim`), else the fallback
/// notation.
fn emit_script(text: ScriptText, sup: bool, dim: bool, followed: bool, out: &mut Frag) {
    match text {
        ScriptText::Unicode(mapped) if dim => {
            for (piece, attrs) in mapped.pieces() {
                out.push(piece, Attrs { dim: true, ..attrs });
            }
        }
        ScriptText::Unicode(mapped) => out.append(mapped),
        ScriptText::Linear(frag) => fallback_script(frag, sup, followed, out),
    }
}

/// The dim fallback for a script without Unicode forms: `^q` for one
/// character, else (or when something follows directly) `^(…)`.
fn fallback_script(frag: Frag, sup: bool, followed: bool, out: &mut Frag) {
    out.push(if sup { "^" } else { "_" }, Attrs::DIM);
    if frag.cluster_count() <= 1 && !followed {
        out.append(frag);
    } else {
        out.push("(", Attrs::DIM);
        out.append(frag);
        out.push(")", Attrs::DIM);
    }
}

/// The most combining marks accents stack on one character. More are
/// unreadable in any font, and nested accents over a long base would
/// otherwise grow the text quadratically.
const MAX_MARKS: usize = 4;

/// `frag` with `mark` on every non-space cluster, precomposed by `compose`
/// where a single character allows it. A cluster that already carries
/// [`MAX_MARKS`] marks is left as it is.
fn with_mark(frag: &Frag, mark: char, compose: impl Fn(char) -> Option<char>) -> Frag {
    frag.map_clusters(|cluster| {
        if cluster == " " {
            return Some(cluster.to_string());
        }
        let mut chars = cluster.chars();
        if let (Some(c), None) = (chars.next(), chars.next())
            && let Some(composed) = compose(c)
        {
            return Some(composed.to_string());
        }
        let mut s = cluster.to_string();
        if cluster.chars().filter(|&c| is_zero_width(c)).count() < MAX_MARKS {
            s.push(mark);
        }
        Some(s)
    })
    .unwrap_or_default()
}

/// The brace character shown in linear text.
fn brace_glyph(brace: Brace) -> char {
    match (brace.shape, brace.over) {
        (BraceShape::Brace, true) => '⏞',
        (BraceShape::Brace, false) => '⏟',
        (BraceShape::Bracket, true) => '⎴',
        (BraceShape::Bracket, false) => '⎵',
        (BraceShape::Paren, true) => '⏜',
        (BraceShape::Paren, false) => '⏝',
    }
}

/// Look through one-item rows and font switches.
pub(crate) fn unwrap(node: &Node) -> &Node {
    match node {
        Node::Row(items) => match items.as_slice() {
            [only] => unwrap(only),
            _ => node,
        },
        Node::Styled { body, .. } => unwrap(body),
        _ => node,
    }
}

/// The characters of a node's atoms, ignoring structure and fonts (to match
/// `\overset{\text{def}}{=}` against the composites).
pub(crate) fn plain_text(node: &Node) -> String {
    let mut s = String::new();
    collect_text(node, &mut s);
    s
}

fn collect_text(node: &Node, s: &mut String) {
    match node {
        Node::Atom(atom) => match atom {
            Atom::Ord(c) | Atom::LargeOp(c) | Atom::Bin(c) | Atom::Punct(c) => s.push(*c),
            Atom::Delim { ch, .. } => s.push(*ch),
            Atom::Num(t) | Atom::Func(t) | Atom::Text(t) | Atom::Rel(t) => s.push_str(t),
        },
        Node::Row(items) => items.iter().for_each(|i| collect_text(i, s)),
        Node::Styled { body, .. } => collect_text(body, s),
        _ => s.push('\u{FFFD}'),
    }
}

/// Whether a fraction part needs parentheses in `a/b` form: it has a
/// top-level binary operator, relation, punctuation or space (outside any
/// delimiters), or a nested fraction or environment. `n(n+1)` and `x²` do
/// not; `a+b` and `sin x` do.
pub(crate) fn is_compound(node: &Node) -> bool {
    match node {
        Node::Row(items) => row_is_compound(items),
        Node::Styled { body, .. } => is_compound(body),
        Node::Atom(Atom::Text(t)) => t.trim().contains(' '),
        Node::Atom(_) | Node::Space(_) | Node::Root { .. } => false,
        Node::Accent { .. } | Node::Not(_) | Node::Fenced { .. } => false,
        Node::Frac { .. } | Node::Grid(_) => true,
        Node::Scripts { base, .. } | Node::OverUnder { base, .. } => is_compound(base),
    }
}

fn row_is_compound(items: &[Node]) -> bool {
    let spacing = spacing::space(items, true, None, None);
    let mut depth = 0usize;
    for (i, item) in items.iter().enumerate() {
        match item {
            Node::Atom(Atom::Delim {
                side: Side::Open, ..
            }) => depth += 1,
            Node::Atom(Atom::Delim {
                side: Side::Close, ..
            }) => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            Node::Space(n) if *n > 0 => return true,
            _ => {
                let class = spacing.classes.get(i).copied().flatten();
                let operator = class.is_some_and(|(l, r)| {
                    [l, r]
                        .iter()
                        .any(|c| matches!(c, Class::Bin | Class::Rel | Class::Punct))
                });
                let spaced = spacing.before.get(i).is_some_and(|&b| b > 0);
                if operator || spaced || is_compound(item) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bold, Letters, ScriptSet, inline};

    fn text(tex: &str) -> String {
        inline(tex, &MathOptions::default()).text
    }

    fn with(tex: &str, f: impl FnOnce(&mut MathOptions)) -> String {
        let mut opts = MathOptions::default();
        f(&mut opts);
        inline(tex, &opts).text
    }

    #[test]
    fn plan_examples() {
        assert_eq!(text(r"a-b=-c"), "a − b = −c");
        assert_eq!(text(r"\frac{a+b}{c}"), "(a+b)/c");
        assert_eq!(text(r"\frac12"), "½");
        assert_eq!(text(r"A^{-1}"), "A⁻¹");
        assert_eq!(text(r"x^{n+q}"), "x^(n+q)");
        assert_eq!(text(r"\sqrt{x^2+1}"), "√(x²+1)");
        assert_eq!(text(r"\sum_{i=1}^n"), "∑ᵢ₌₁ⁿ");
        assert_eq!(text(r"\int_0^\infty"), "∫₀^∞");
        assert_eq!(text(r"\lim_{x\to 0}"), "lim_(x→0)");
        assert_eq!(text(r"\mathbb{R}^n"), "ℝⁿ");
        assert_eq!(text(r"\hat a"), "â");
        assert_eq!(text(r"\not\in"), "∉");
        assert_eq!(text(r"\alpha^2 + \frac{a}{b}"), "α² + a/b");
    }

    #[test]
    fn spacing_rules() {
        assert_eq!(text(r"a+b"), "a + b");
        assert_eq!(text(r"x^{a+b}"), "xᵃ⁺ᵇ");
        assert_eq!(text(r"-x"), "−x");
        assert_eq!(text(r"f(x, y)"), "f(x, y)");
        assert_eq!(text(r"\sin x"), "sin x");
        assert_eq!(text(r"\sin(x)"), "sin(x)");
        assert_eq!(text(r"2\sin^2 x"), "2 sin² x");
        assert_eq!(text(r"\log_2 n"), "log₂ n");
        assert_eq!(text(r"a\,b\:c\;d~e\quad f\qquad g"), "a b c d e  f    g");
        assert_eq!(text(r"a\!b"), "ab");
        assert_eq!(text(r"-\sum_i x_i"), "−∑ᵢ xᵢ");
        assert_eq!(text(r"\operatorname{softmax}\left(x\right)"), "softmax(x)");
        assert_eq!(text(r"\sum_{\substack{i<n\\j<m}}"), "∑_(i<n, j<m)");
        assert_eq!(text(r"\int\!\!\int"), "∫∫");
        assert_eq!(text(r"a \, + b"), "a  + b");
    }

    #[test]
    fn fractions() {
        assert_eq!(text(r"\frac{n(n+1)(2n+1)}{6}"), "n(n+1)(2n+1)/6");
        assert_eq!(text(r"\frac{\sin x}{x}"), "(sin x)/x");
        assert_eq!(text(r"\frac{1}{\frac{a}{b}}"), "1/(a/b)");
        assert_eq!(text(r"\frac{\frac12}{x}"), "½/x");
        assert_eq!(text(r"\frac{-1}{2}"), "−1/2");
        assert_eq!(text(r"\frac{3}{16}"), "3/16");
        assert_eq!(text(r"\frac{1}{16}"), "⅟16");
        assert_eq!(with(r"\frac12", |o| o.fractions = Fractions::Slash), "1/2");
        assert_eq!(text(r"\binom{n}{k}"), "(n choose k)");
        assert_eq!(text(r"\dfrac{a}{b}"), "a/b");
    }

    #[test]
    fn scripts() {
        assert_eq!(text(r"x_N"), "x_N");
        assert_eq!(text(r"e^{i\pi}"), "e^(iπ)");
        assert_eq!(text(r"x_1^2"), "x₁²");
        assert_eq!(text(r"x^2_1"), "x₁²");
        assert_eq!(text(r"f'(x)"), "f′(x)");
        assert_eq!(text(r"f''"), "f′′");
        assert_eq!(text(r"90^\circ"), "90°");
        assert_eq!(text(r"A^\dagger"), "A†");
        assert_eq!(text(r"z^*"), "z*");
        assert_eq!(text(r"e^{-x^2}"), "e^(−x²)");
        assert_eq!(text(r"x^{}"), "x");
        assert_eq!(text(r"x_{i,j}"), "x_(i,j)");
        assert_eq!(with(r"x^q", |o| o.scripts = ScriptSet::Full), "x𐞥");
    }

    #[test]
    fn fractions_in_context() {
        assert_eq!(text(r"\frac{a}{b}c"), "(a/b)c");
        assert_eq!(text(r"\frac{a}{b}(x+1)"), "(a/b)(x + 1)");
        assert_eq!(text(r"\frac{a}{b} c"), "(a/b)c");
        assert_eq!(text(r"\frac{a}{b}\,c"), "a/b c");
        assert_eq!(text(r"\frac{a}{b} + c"), "a/b + c");
        assert_eq!(text(r"x\frac{a}{b}"), "xa/b");
        assert_eq!(text(r"\frac12 x"), "½x");
        assert_eq!(text(r"\log \frac{a}{b}"), "log (a/b)");
        assert_eq!(text(r"\sin^2 \frac{x}{2}"), "sin² (x/2)");
        assert_eq!(text(r"\max \frac{a}{b}"), "max a/b");
        assert_eq!(
            text(r"i\hbar \frac{\partial}{\partial t} \Psi"),
            "iℏ(∂/∂t)Ψ"
        );
        assert_eq!(text(r"\frac{n!}{k!(n-k)!}"), "n!/k!(n−k)!");
    }

    #[test]
    fn unicode_italic_scripts_map_from_plain_letters() {
        let line = inline(
            r"x_i^n",
            &MathOptions {
                letters: Letters::UnicodeItalic,
                ..MathOptions::default()
            },
        );
        assert_eq!(line.text, "𝑥ᵢⁿ");
        assert!(line.spans.iter().all(|s| s.role == MathRole::Plain));
        assert_eq!(with(r"x_{\mathbf{a}}", |o| o.bold = Bold::Unicode), "xₐ");
        assert_eq!(
            with(r"\sqrt[n]{x}", |o| o.letters = Letters::UnicodeItalic),
            "ⁿ√𝑥"
        );
    }

    #[test]
    fn roots() {
        assert_eq!(text(r"\sqrt{x}"), "√x");
        assert_eq!(text(r"\sqrt{2}"), "√2");
        assert_eq!(text(r"\sqrt{12}"), "√12");
        assert_eq!(text(r"\sqrt{2x}"), "√(2x)");
        assert_eq!(text(r"\sqrt[3]{x}"), "∛x");
        assert_eq!(text(r"\sqrt[4]{x+1}"), "∜(x+1)");
        assert_eq!(text(r"\sqrt[n]{x}"), "ⁿ√x");
        assert_eq!(text(r"\sqrt[\pi]{x}"), "x^(1/π)");
        assert_eq!(text(r"\sqrt[\pi]{x+1}"), "(x+1)^(1/π)");
        assert_eq!(text(r"\sqrt{\frac12}"), "√½");
    }

    #[test]
    fn operators_and_functions() {
        assert_eq!(
            text(r"\lim_{x \to 0} \frac{\sin x}{x} = 1"),
            "lim_(x→0) (sin x)/x = 1"
        );
        assert_eq!(
            text(r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}"),
            "∫₀^∞ e^(−x²) dx = √π/2"
        );
        assert_eq!(text(r"\max_i x_i"), "maxᵢ xᵢ");
        assert_eq!(text(r"\operatorname*{argmax}_x f"), "argmaxₓ f");
        assert_eq!(text(r"a \bmod b"), "a mod b");
        assert_eq!(text(r"a \equiv b \pmod n"), "a ≡ b (mod n)");
    }

    #[test]
    fn fonts() {
        assert_eq!(text(r"\mathcal{L} \mathfrak{g} \mathscr{H}"), "ℒ𝔤ℋ");
        assert_eq!(text(r"\mathrm{d}x"), "dx");
        let line = inline(r"\mathbf{x}", &MathOptions::default());
        assert!(line.spans[0].bold);
        assert_eq!(with(r"\mathbf{x}", |o| o.bold = Bold::Unicode), "𝐱");
        assert_eq!(with(r"h", |o| o.letters = Letters::UnicodeItalic), "ℎ");
        assert_eq!(text(r"\phi \varphi"), "ϕφ");
    }

    #[test]
    fn accents_and_negation() {
        assert_eq!(text(r"\bar x"), "x\u{304}");
        assert_eq!(text(r"\bar a"), "ā");
        assert_eq!(text(r"\dot x"), "ẋ");
        assert_eq!(text(r"\vec v"), "v\u{20D7}");
        assert_eq!(text(r"\overline{AB}"), "A\u{305}B\u{305}");
        assert_eq!(text(r"\hat{}"), "^");
        assert_eq!(text(r"\not="), "≠");
        assert_eq!(text(r"a \not< b"), "a ≮ b");
        assert_eq!(text(r"\not\preceq"), "⪯\u{338}");
    }

    #[test]
    fn delimiters_and_environments() {
        assert_eq!(text(r"\left( \frac{a}{b} \right)"), "(a/b)");
        assert_eq!(text(r"\left. x \right|"), "x|");
        assert_eq!(
            text(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}"),
            "(a b; c d)"
        );
        assert_eq!(
            text(r"\begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}"),
            "{1, x > 0; 0, otherwise}"
        );
        assert_eq!(
            text(r"\begin{aligned} a &= b \\ &= c \end{aligned}"),
            "a = b = c"
        );
        assert_eq!(text(r"a \\ b"), "a; b");
        assert_eq!(text(r"\overset{\text{def}}{=}"), "≝");
        assert_eq!(text(r"\stackrel{?}{=}"), "≟");
    }

    #[test]
    fn roles_dim_and_breaks() {
        let line = inline(r"x^{n+q} = y", &MathOptions::default());
        assert_eq!(line.text, "x^(n+q) = y");
        let roles: Vec<(&str, MathRole, bool)> = {
            let mut start = 0;
            line.spans
                .iter()
                .map(|s| {
                    let piece = &line.text[start..s.end as usize];
                    start = s.end as usize;
                    (piece, s.role, s.dim)
                })
                .collect()
        };
        assert_eq!(
            roles,
            [
                ("x", MathRole::Var, false),
                ("^(", MathRole::Plain, true),
                ("n", MathRole::Var, false),
                ("+", MathRole::Op, false),
                ("q", MathRole::Var, false),
                (")", MathRole::Plain, true),
                (" ", MathRole::Plain, false),
                ("=", MathRole::Rel, false),
                (" ", MathRole::Plain, false),
                ("y", MathRole::Var, false),
            ]
        );
        assert_eq!(line.breaks, [10]);
        assert_eq!(&line.text[10..], "y");
        let line = inline(r"a + b = -c", &MathOptions::default());
        assert_eq!(line.text, "a + b = −c");
        assert_eq!(line.breaks, [4, 8]);
        // Not inside groups.
        assert_eq!(inline(r"(a+b)", &MathOptions::default()).breaks, [5]);
        assert!(
            inline(r"\left(a+b\right)", &MathOptions::default())
                .breaks
                .is_empty()
        );
    }

    #[test]
    fn widths() {
        let line = inline(r"\hat{x} + \alpha", &MathOptions::default());
        assert_eq!(line.width, 5);
        let opts = MathOptions {
            ambiguous_wide: true,
            ..MathOptions::default()
        };
        assert_eq!(inline(r"\alpha", &opts).width, 1);
        assert_eq!(inline(r"\sum", &opts).width, 2);
    }

    /// Nested scripts without Unicode forms were rendered twice per level
    /// (once to try the mapping, once for the fallback): 2^depth work, so
    /// these formulas took minutes. Each subtree is now rendered once.
    #[test]
    fn nested_scripts_render_once_per_level() {
        let nested =
            |open: &str, close: &str, n: usize| format!("{}x{}", open.repeat(n), close.repeat(n));
        let deep = [
            nested(r"\pi^{", "}", 40),
            nested(r"\pi_{", "}", 40),
            nested(r"\sqrt[\pi^{", "}]{y}", 20),
            nested(r"\overset{\pi^{", "}}{=}", 20),
            nested(r"\underbrace{\pi}_{", "}", 40),
        ];
        let options = [
            MathOptions::default(),
            MathOptions {
                letters: Letters::UnicodeItalic,
                bold: Bold::Unicode,
                ..MathOptions::default()
            },
        ];
        for tex in &deep {
            for opts in &options {
                assert!(inline(tex, opts).ok, "{tex}");
                let _ = crate::display(tex, opts, 80);
            }
        }
        assert_eq!(text(r"\pi^{\pi^{\pi}}"), "π^(π^π)");
        assert_eq!(text(r"\sqrt[\pi^{\pi}]{x}"), "x^(1/(π^π))");
    }

    #[test]
    fn one_character_fallbacks_are_parenthesised_when_followed() {
        // Juxtaposed material or a Unicode superscript right after it.
        assert_eq!(text(r"\nabla_\theta J(\theta)"), "∇_(θ)J(θ)");
        assert_eq!(text(r"x_\alpha y_\beta"), "x_(α)yᵦ");
        assert_eq!(text(r"{x_N}y"), "x_(N)y");
        assert_eq!(text(r"x_N^2"), "x_(N)²");
        assert_eq!(text(r"a_{x_N+1}"), "a_(x_(N)+1)");
        assert_eq!(text(r"x_N\!y"), "x_(N)y");
        // Otherwise the short form stays.
        assert_eq!(text(r"x_N"), "x_N");
        assert_eq!(text(r"x_N + y"), "x_N + y");
        assert_eq!(text(r"x_N^Q"), "x_N^Q");
        assert_eq!(text(r"(x_N)"), "(x_N)");
        assert_eq!(text(r"p_\theta(x)"), "p_θ(x)");
        assert_eq!(text(r"x_N, y"), "x_N, y");
        assert_eq!(text(r"x_N\,y"), "x_N y");
        assert_eq!(text(r"\sum_N x"), "∑_N x");
    }

    #[test]
    fn scripts_on_fractions_and_roots_cover_all_of_them() {
        assert_eq!(text(r"\frac{a}{b}^2"), "(a/b)²");
        assert_eq!(text(r"{\frac{a}{b}}^2"), "(a/b)²");
        assert_eq!(text(r"\sqrt{x}^2"), "(√x)²");
        assert_eq!(text(r"\sqrt{x+1}^2"), "(√(x+1))²");
        assert_eq!(text(r"\sqrt{x}_1"), "(√x)₁");
        // A vulgar fraction is one character already.
        assert_eq!(text(r"\frac12^2"), "½²");
        // Groups keep TeX's reading: the script is on the last symbol.
        assert_eq!(text(r"{a+b}^2"), "a + b²");
    }

    #[test]
    fn bare_radicands_are_parenthesised_when_followed() {
        assert_eq!(text(r"\sqrt{2}\pi"), "√(2)π");
        assert_eq!(text(r"\sqrt[3]{x}y"), "∛(x)y");
        assert_eq!(text(r"\sqrt{2} + x"), "√2 + x");
        assert_eq!(text(r"2\sqrt{2}"), "2√2");
        assert_eq!(text(r"\sqrt{x}\,dx"), "√x dx");
        // One delimited group needs no parentheses of its own.
        assert_eq!(text(r"\sqrt{(a+b)}c"), "√(a+b)c");
        assert_eq!(text(r"\sqrt{(a+b)}"), "√(a+b)");
        assert_eq!(text(r"\sqrt{(a)(b)}"), "√((a)(b))");
    }

    #[test]
    fn aligned_rows_pair_their_cells_and_continue() {
        assert_eq!(
            text(r"\begin{aligned} x &= 1 & y &= 2 \end{aligned}"),
            "x = 1  y = 2"
        );
        assert_eq!(
            text(r"\begin{aligned} a &= b \\ c &= d & e &= f \end{aligned}"),
            "a = b; c = d  e = f"
        );
        // A row with an empty left cell that starts with an operator
        // continues the expression; its 2D indentation is dropped.
        assert_eq!(
            text(r"\begin{aligned} a + b &= c \\ &\quad + d \end{aligned}"),
            "a + b = c + d"
        );
        assert_eq!(
            text(r"\begin{aligned} x + y &= 1 \\ -x + y &= 3 \end{aligned}"),
            "x + y = 1; −x + y = 3"
        );
        // Top-level rows break like the top level.
        let line = inline(
            r"\begin{aligned} f &= a+b \\ g &= c \end{aligned}",
            &MathOptions::default(),
        );
        assert_eq!(line.text, "f = a + b; g = c");
        assert_eq!(line.breaks, [4, 8, 11, 15]);
        // Not inside other environments.
        assert!(
            inline(
                r"\begin{pmatrix} a+b \\ c \end{pmatrix}",
                &MathOptions::default()
            )
            .breaks
            .is_empty()
        );
    }

    #[test]
    fn matrix_cells_with_spaces_are_comma_separated() {
        assert_eq!(
            text(r"\begin{pmatrix} a+b & c \\ d & e \end{pmatrix}"),
            "(a + b, c; d, e)"
        );
        assert_eq!(
            text(r"\begin{bmatrix} \sin x & 0 \end{bmatrix}"),
            "[sin x, 0]"
        );
        assert_eq!(
            text(r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}"),
            "(a b; c d)"
        );
    }

    #[test]
    fn middle_delimiters_are_not_factors() {
        assert_eq!(text(r"\left(\frac{a}{b}\middle|c\right)"), "(a/b|c)");
        assert_eq!(text(r"\frac{a}{b}|x|"), "(a/b)|x|");
    }

    #[test]
    fn stacked_accents_are_capped() {
        let line = text(r"\hat{\bar{\dot{\tilde{\check{\breve{x}}}}}}");
        let marks = line.chars().filter(|&c| is_zero_width(c)).count();
        assert_eq!(line.chars().next(), Some('x'));
        assert_eq!(marks, MAX_MARKS);
    }

    #[test]
    fn typed_combining_marks_attach() {
        assert_eq!(text("x\u{301} + e\u{301}"), "x\u{301} + é");
        assert_eq!(text("a =\u{338} b"), "a ≠ b");
        assert_eq!(text("a \u{2208}\u{338} B"), "a ∉ B");
    }
}
