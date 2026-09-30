//! Linear rendering: one line of Unicode, for inline math and as the display
//! fallback.
//!
//! | Construct | Rule | Example |
//! |---|---|---|
//! | Spacing | [`spacing`]: a column around binary operators and relations, none in scripts, fractions and radicands, none after a unary sign | `a − b = −c` |
//! | Fractions | `a/b`, parenthesising compound parts; digit/digit as a vulgar fraction ([`Fractions`]); the whole fraction in parentheses before juxtaposed terms or after a function name | `(a+b)/c`, `½`, `(a/b)c`, `log (a/b)` |
//! | Scripts | all-or-nothing Unicode (letters mapped from their plain form), else dim `^(…)`/`_(…)` (`^q` for one character); subscript first; primes, `°`, `*`, `†` stay inline | `A⁻¹`, `x^(n+q)`, `90°` |
//! | Roots | `√x`, `√(…)`, `∛`, `∜`, `ⁿ√`, else `x^(1/k)` | `√(x²+1)` |
//! | Operators | limits as scripts to the right | `∑ᵢ₌₁ⁿ`, `lim_(x→0)` |
//! | Accents, `\not` | precomposed when NFC has it, else a combining mark | `â`, `∉` |
//! | Environments | `(a b; c d)`, `{1, x > 0; 0, otherwise}` | |
//!
//! Every span has a role. Line breaks are allowed after top-level binary
//! operators and relations (the break offset is where the next line starts).

use crate::ast::{
    Accent, Atom, Brace, BraceShape, Font, Formula, Grid, GridKind, Limits, Node, Side,
};
use crate::spacing::{self, Class};
use crate::style;
use crate::tables;
use crate::width::{clusters, str_width, to_u16};
use crate::{Bold, Fractions, Letters, MathLine, MathOptions, MathRole, MathSpan};

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
}

impl Ctx {
    pub(crate) fn top() -> Ctx {
        Ctx {
            tight: false,
            font: Font::Normal,
            top: true,
        }
    }

    fn inner(self) -> Ctx {
        Ctx { top: false, ..self }
    }

    pub(crate) fn tight(self) -> Ctx {
        Ctx {
            tight: true,
            top: false,
            ..self
        }
    }
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
                self.node(base, ctx.inner(), out);
                if let Some(sub) = sub {
                    self.script(sub, false, ctx, out);
                }
                if let Some(sup) = sup {
                    self.script(sup, true, ctx, out);
                }
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
            let child = if matches!(item, Node::Styled { .. }) {
                ctx
            } else {
                ctx.inner()
            };
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

    fn root(&self, radicand: &Node, index: Option<&Node>, ctx: Ctx, out: &mut Frag) {
        let tight = ctx.tight();
        let index_node = index;
        let body = self.frag(radicand, tight);
        let simple = body.cluster_count() <= 1
            || matches!(radicand.as_atom(), Some(Atom::Num(_)))
            || matches!(unwrap(radicand), Node::Fenced { .. });
        let index = index.map(|i| self.frag(i, tight));
        let sign = match index.as_ref().map(|i| i.text.as_str()) {
            None | Some("" | "2") => Some("√".to_string()),
            Some("3") => Some("∛".to_string()),
            Some("4") => Some("∜".to_string()),
            Some(_) => None,
        };
        let (mut prefix, sign) = match (sign, &index) {
            (Some(sign), _) => (Frag::default(), sign),
            (None, Some(i)) => match index_node.and_then(|n| self.script_unicode(n, true, ctx)) {
                Some(mapped) => (mapped, "√".to_string()),
                None => {
                    // x^(1/k)
                    operand(body, !simple, out);
                    out.push("^(", Attrs::DIM);
                    out.push("1", Attrs::role(MathRole::Num));
                    out.push("/", Attrs::DELIM);
                    let compound = i.cluster_count() > 1;
                    operand(i.clone(), compound, out);
                    out.push(")", Attrs::DIM);
                    return;
                }
            },
            (None, None) => (Frag::default(), "√".to_string()),
        };
        prefix.push(&sign, Attrs::DELIM);
        out.append(prefix);
        operand(body, !simple, out);
    }

    /// A subscript (`sup == false`) or superscript: Unicode when every
    /// character has a form, else the dim fallback notation.
    fn script(&self, node: &Node, sup: bool, ctx: Ctx, out: &mut Frag) {
        match self.script_unicode(node, sup, ctx) {
            Some(mapped) => out.append(mapped),
            None => fallback_script(self.frag(node, ctx.tight()), sup, out),
        }
    }

    /// `node` as a script in Unicode super/subscript characters, if every
    /// character has one. Letters are mapped from their plain form (a math
    /// italic `𝑥` has no superscript, `x` has `ˣ`); the result keeps the
    /// roles letters have under the current options.
    pub(crate) fn script_unicode(&self, node: &Node, sup: bool, ctx: Ctx) -> Option<Frag> {
        let plain = MathOptions {
            letters: Letters::Italic,
            bold: Bold::Sgr,
            ..*self.opts
        };
        let frag = Linear { opts: &plain }.frag(node, ctx.tight());
        let mapped = self.map_script(&frag, sup)?;
        if self.opts.letters == Letters::Italic {
            return Some(mapped);
        }
        let mut relabelled = Frag::default();
        for (piece, attrs) in mapped.pieces() {
            let role = match attrs.role {
                MathRole::Var => MathRole::Plain,
                role => role,
            };
            relabelled.push(piece, Attrs { role, ..attrs });
        }
        Some(relabelled)
    }

    /// `frag` in superscript or subscript characters, if all of them map.
    fn map_script(&self, frag: &Frag, sup: bool) -> Option<Frag> {
        let set = self.opts.scripts;
        frag.map_clusters(|cluster| {
            let mut chars = cluster.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            let mapped = if sup {
                tables::superscript(c, set)
            } else {
                tables::subscript(c, set)
            }?;
            Some(mapped.to_string())
        })
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
        for (label, sup) in [(under, false), (over, true)] {
            if let Some(label) = label {
                match self.script_unicode(label, sup, ctx) {
                    Some(mapped) => {
                        for (piece, attrs) in mapped.pieces() {
                            out.push(piece, Attrs { dim: true, ..attrs });
                        }
                    }
                    None => fallback_script(self.frag(label, ctx.tight()), sup, out),
                }
            }
        }
    }

    fn grid(&self, grid: &Grid, ctx: Ctx, out: &mut Frag) {
        // Cells are spaced normally, except in `\substack` (always in limits).
        let cell_ctx = Ctx {
            tight: ctx.tight && matches!(grid.kind, GridKind::Substack(_)),
            ..ctx.inner()
        };
        let (open, cell_sep, row_sep, close) = match grid.kind {
            GridKind::Matrix(_) | GridKind::Array => ("", " ", "; ", ""),
            GridKind::Cases { .. } => ("{", ", ", "; ", "}"),
            GridKind::Aligned => ("", "", "; ", ""),
            GridKind::Gathered => ("", " ", "; ", ""),
            GridKind::Substack(_) => ("", " ", ", ", ""),
        };
        out.push(open, Attrs::DELIM);
        for (r, row) in grid.rows.iter().enumerate() {
            if grid.kind == GridKind::Aligned {
                // The cells of an aligned row are one expression.
                let items: Vec<Node> = row
                    .iter()
                    .flat_map(|cell| cell.items().iter().cloned())
                    .collect();
                if r > 0 {
                    let rel_first = items
                        .iter()
                        .find_map(spacing::edges)
                        .is_some_and(|(left, _)| left == Class::Rel);
                    out.push(if rel_first { " " } else { row_sep }, Attrs::PLAIN);
                }
                self.row(&items, cell_ctx, None, None, out);
                continue;
            }
            if r > 0 {
                out.push(row_sep, Attrs::PLAIN);
            }
            for (c, cell) in row.iter().enumerate() {
                if c > 0 {
                    out.push(cell_sep, Attrs::PLAIN);
                }
                self.node(cell, cell_ctx, out);
            }
        }
        out.push(close, Attrs::DELIM);
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
    let juxtaposed = next.is_some_and(|j| {
        j == i + 1
            && spacing.before.get(j) == Some(&0)
            && matches!(
                spacing.classes.get(j),
                Some(Some((Class::Ord | Class::Open, _)))
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
    if paren {
        out.push("(", Attrs::DELIM);
    }
    out.append(frag);
    if paren {
        out.push(")", Attrs::DELIM);
    }
}

/// The dim fallback for a script without Unicode forms: `^q`, `^(…)`.
fn fallback_script(frag: Frag, sup: bool, out: &mut Frag) {
    let marker = if sup { "^" } else { "_" };
    if frag.cluster_count() <= 1 {
        out.push(marker, Attrs::DIM);
        out.append(frag);
    } else {
        out.push(marker, Attrs::DIM);
        out.push("(", Attrs::DIM);
        out.append(frag);
        out.push(")", Attrs::DIM);
    }
}

/// `frag` with `mark` on every non-space cluster, precomposed by `compose`
/// where a single character allows it.
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
        s.push(mark);
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
    use crate::{ScriptSet, inline};

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
}
