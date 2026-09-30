//! Style resolution for layout.
//!
//! The theme resolves one full [`Style`] per [`Element`]. Layout combines
//! them: a block gives the *base* style (a paragraph's text, a heading, a
//! table header) and inline markup applies each element's difference from
//! plain text on top ([`Styles::inline`]), so emphasis inside a heading
//! keeps the heading's colour. A *context* ([`Ctx`]) adds a background and
//! attributes to everything inside a region: an alert's tint, an `h1` bar,
//! the dimmed text of a finished task.
//!
//! Everything is interned into the layout's [`StyleTable`], with small
//! caches so the per-run work is a hash lookup.

use std::collections::HashMap;

use emde_math::MathRole;

use crate::ir::InlineFlags;
use crate::style::{Attrs, Color, Style, StyleId, StylePatch, StyleTable, Underline};
use crate::term::ColorDepth;
use crate::theme::{Element, Theme};

/// What a piece of inline text is, for styling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Piece {
    /// Prose.
    Text,
    /// A code span.
    Code,
    /// A footnote reference number.
    FootRef,
    /// The marker of an image chip.
    ChipGlyph,
    /// The alt text of an image chip.
    ChipAlt,
    /// A raw HTML tag.
    Html,
    /// A numbered link reference (`[1]`).
    LinkRef,
    /// Typeset math.
    Math {
        role: MathRole,
        bold: bool,
        dim: bool,
    },
}

/// A region's background and extra attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct Ctx {
    /// Background for text without one of its own.
    pub(super) bg: Option<Color>,
    /// Attributes added to everything.
    pub(super) attrs: Attrs,
}

impl Ctx {
    /// Whether the context changes nothing.
    pub(super) fn is_empty(&self) -> bool {
        self.bg.is_none() && self.attrs.is_empty()
    }

    /// `inner` inside `self`: the innermost background wins, attributes add
    /// up.
    pub(super) fn within(self, inner: Ctx) -> Ctx {
        Ctx {
            bg: inner.bg.or(self.bg),
            attrs: self.attrs | inner.attrs,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct InlineKey {
    base: StyleId,
    flags: InlineFlags,
    piece: Piece,
    link: bool,
}

/// The layout's style table plus resolution caches.
#[derive(Debug)]
pub(super) struct Styles {
    table: StyleTable,
    depth: ColorDepth,
    /// Every element's resolved style.
    styles: Vec<Style>,
    /// Every element's difference from `Text`.
    deltas: Vec<StylePatch>,
    ids: Vec<Option<StyleId>>,
    inline_cache: HashMap<InlineKey, StyleId>,
    ctx_cache: HashMap<(StyleId, Ctx), StyleId>,
}

/// The patch that turns `base` into `s`.
fn diff(base: &Style, s: &Style) -> StylePatch {
    StylePatch {
        fg: (s.fg != base.fg).then_some(s.fg),
        bg: (s.bg != base.bg).then_some(s.bg),
        underline_color: (s.underline_color != base.underline_color).then_some(s.underline_color),
        set: s.attrs - base.attrs,
        clear: base.attrs - s.attrs,
        underline: (s.underline != base.underline).then_some(s.underline),
    }
}

impl Styles {
    pub(super) fn new(theme: &Theme, depth: ColorDepth) -> Styles {
        let styles: Vec<Style> = Element::ALL.iter().map(|&e| theme.style(e)).collect();
        let text = theme.style(Element::Text);
        let deltas = styles.iter().map(|s| diff(&text, s)).collect();
        Styles {
            table: StyleTable::new(),
            depth,
            styles,
            deltas,
            ids: vec![None; Element::COUNT],
            inline_cache: HashMap::new(),
            ctx_cache: HashMap::new(),
        }
    }

    /// The finished table.
    pub(super) fn into_table(self) -> StyleTable {
        self.table
    }

    /// The colour depth styles are made for.
    pub(super) fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// An element's resolved style.
    pub(super) fn of(&self, e: Element) -> Style {
        self.styles.get(e.index()).copied().unwrap_or_default()
    }

    /// Intern a style.
    pub(super) fn intern(&mut self, s: Style) -> StyleId {
        self.table.intern(s)
    }

    /// An interned style by id.
    pub(super) fn get(&self, id: StyleId) -> Style {
        *self.table.get(id)
    }

    /// An element's style, interned.
    pub(super) fn el(&mut self, e: Element) -> StyleId {
        if let Some(Some(id)) = self.ids.get(e.index()) {
            return *id;
        }
        let id = self.table.intern(self.of(e));
        if let Some(slot) = self.ids.get_mut(e.index()) {
            *slot = Some(id);
        }
        id
    }

    /// `id` with a foreground colour.
    pub(super) fn with_fg(&mut self, id: StyleId, fg: Color) -> StyleId {
        let mut s = self.get(id);
        s.fg = fg;
        self.intern(s)
    }

    /// `id` with a background colour.
    pub(super) fn with_bg(&mut self, id: StyleId, bg: Color) -> StyleId {
        let mut s = self.get(id);
        s.bg = bg;
        self.intern(s)
    }

    /// The style of an inline piece on top of a block's `base` style.
    ///
    /// The piece's element comes first, then emphasis flags, then the link
    /// style (footnote references carry their own).
    pub(super) fn inline(
        &mut self,
        base: StyleId,
        flags: InlineFlags,
        piece: Piece,
        link: bool,
    ) -> StyleId {
        let key = InlineKey {
            base,
            flags,
            piece,
            link,
        };
        if let Some(&id) = self.inline_cache.get(&key) {
            return id;
        }
        let mut s = self.get(base);
        let apply = |s: &mut Style, e: Element, deltas: &[StylePatch]| {
            if let Some(p) = deltas.get(e.index()) {
                *s = s.patch(p);
            }
        };
        let deltas = &self.deltas;
        match piece {
            Piece::Text => {}
            Piece::Code => apply(&mut s, Element::CodeInline, deltas),
            Piece::FootRef => apply(&mut s, Element::FootnoteRef, deltas),
            Piece::ChipGlyph => apply(&mut s, Element::ImageFrame, deltas),
            Piece::ChipAlt => apply(&mut s, Element::ImageAlt, deltas),
            Piece::Html => apply(&mut s, Element::Html, deltas),
            Piece::LinkRef => apply(&mut s, Element::LinkRef, deltas),
            Piece::Math { role, bold, dim } => {
                apply(&mut s, math_element(role), deltas);
                if bold {
                    s.attrs |= Attrs::BOLD;
                }
                if dim {
                    s.attrs |= Attrs::DIM;
                }
            }
        }
        for (flag, e) in [
            (InlineFlags::EMPH, Element::Emph),
            (InlineFlags::STRONG, Element::Strong),
            (InlineFlags::STRIKE, Element::Strike),
            (InlineFlags::MARK, Element::Mark),
            (InlineFlags::KBD, Element::Kbd),
        ] {
            if flags.contains(flag) {
                apply(&mut s, e, deltas);
            }
        }
        if flags.contains(InlineFlags::UNDERLINE) {
            s.underline = Underline::Single;
        }
        if link && !matches!(piece, Piece::FootRef | Piece::LinkRef) {
            apply(&mut s, Element::Link, deltas);
        }
        let id = self.table.intern(s);
        self.inline_cache.insert(key, id);
        id
    }

    /// `id` inside a context.
    pub(super) fn in_ctx(&mut self, id: StyleId, ctx: Ctx) -> StyleId {
        if ctx.is_empty() {
            return id;
        }
        if let Some(&out) = self.ctx_cache.get(&(id, ctx)) {
            return out;
        }
        let mut s = self.get(id);
        if s.bg == Color::Default
            && let Some(bg) = ctx.bg
        {
            s.bg = bg;
        }
        s.attrs |= ctx.attrs;
        let out = self.table.intern(s);
        self.ctx_cache.insert((id, ctx), out);
        out
    }

    /// Whether a background colour shows at this depth.
    pub(super) fn bg_visible(&self, s: &Style) -> bool {
        self.depth >= ColorDepth::Ansi16 && s.bg != Color::Default
    }

    /// Whether a space in this style shows anything (a background, reverse
    /// video, a line through or under it).
    pub(super) fn space_visible(&self, id: StyleId) -> bool {
        let s = self.get(id);
        self.depth != ColorDepth::None
            && (self.bg_visible(&s)
                || s.attrs
                    .intersects(Attrs::REVERSE | Attrs::STRIKE | Attrs::OVERLINE)
                || s.underline != Underline::None)
    }

    /// Whether text in this style is told apart from `plain` text at all
    /// (colours at 16 colours and up, attributes unless there are no
    /// escapes).
    pub(super) fn distinct(&self, s: &Style, plain: &Style) -> bool {
        match self.depth {
            ColorDepth::None => false,
            ColorDepth::Mono => s.attrs != plain.attrs || s.underline != plain.underline,
            _ => s != plain,
        }
    }

    /// Whether inline pills (code spans, key caps) get padding: only where
    /// their background shows well (256 colours and up).
    pub(super) fn pill(&self, s: &Style) -> bool {
        self.depth >= ColorDepth::Ansi256 && s.bg != Color::Default
    }
}

/// The element of a math role.
pub(super) fn math_element(role: MathRole) -> Element {
    match role {
        MathRole::Plain => Element::Math,
        MathRole::Var => Element::MathVar,
        MathRole::Num => Element::MathNum,
        MathRole::Op => Element::MathOp,
        MathRole::Rel => Element::MathRel,
        MathRole::Func => Element::MathFunc,
        MathRole::Text => Element::MathText,
        MathRole::Delim => Element::MathDelim,
        MathRole::Error => Element::MathError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Rgb;

    fn styles() -> Styles {
        Styles::new(&Theme::test(), ColorDepth::TrueColor)
    }

    #[test]
    fn inline_combines_on_top_of_the_base() {
        let mut s = styles();
        let h2 = s.el(Element::H2);
        let em = s.inline(h2, InlineFlags::EMPH, Piece::Text, false);
        let st = s.get(em);
        assert_eq!(st.fg, s.of(Element::H2).fg, "keeps the heading colour");
        assert!(st.attrs.contains(Attrs::BOLD | Attrs::ITALIC));
        let link = s.inline(h2, InlineFlags::empty(), Piece::Text, true);
        assert_eq!(s.get(link).underline, Underline::Single);
        assert_eq!(s.get(link).fg, s.of(Element::Link).fg);
        // Cached: the same id again.
        assert_eq!(s.inline(h2, InlineFlags::EMPH, Piece::Text, false), em);
    }

    #[test]
    fn footnote_refs_are_not_underlined() {
        let mut s = styles();
        let text = s.el(Element::Text);
        let r = s.inline(text, InlineFlags::empty(), Piece::FootRef, true);
        assert_eq!(s.get(r).underline, Underline::None);
        assert_eq!(s.get(r).fg, s.of(Element::Link).fg);
    }

    #[test]
    fn code_inline_is_a_pill() {
        let mut s = styles();
        let text = s.el(Element::Text);
        let c = s.inline(text, InlineFlags::empty(), Piece::Code, false);
        let st = s.get(c);
        assert_eq!(st.bg, Color::Rgb(Theme::test().surface));
        assert!(s.pill(&st));
        let low = Styles::new(&Theme::test(), ColorDepth::Ansi16);
        assert!(!low.pill(&st));
    }

    #[test]
    fn math_roles_map_to_elements() {
        let mut s = styles();
        let text = s.el(Element::Text);
        let piece = Piece::Math {
            role: MathRole::Var,
            bold: true,
            dim: true,
        };
        let v = s.inline(text, InlineFlags::empty(), piece, false);
        let st = s.get(v);
        assert!(st.attrs.contains(Attrs::ITALIC | Attrs::BOLD | Attrs::DIM));
        assert_eq!(st.fg, s.of(Element::MathVar).fg);
        assert_eq!(math_element(MathRole::Error), Element::MathError);
    }

    #[test]
    fn context_adds_background_only_where_missing() {
        let mut s = styles();
        let tint = Color::Rgb(Rgb(1, 2, 3));
        let ctx = Ctx {
            bg: Some(tint),
            attrs: Attrs::DIM,
        };
        let text = s.el(Element::Text);
        let t = s.in_ctx(text, ctx);
        assert_eq!(s.get(t).bg, tint);
        assert!(s.get(t).attrs.contains(Attrs::DIM));
        let code = s.el(Element::CodeInline);
        let c = s.in_ctx(code, ctx);
        assert_eq!(s.get(c).bg, s.of(Element::CodeInline).bg, "own bg wins");
        assert_eq!(s.in_ctx(text, Ctx::default()), text);
        let outer = Ctx {
            bg: Some(tint),
            attrs: Attrs::empty(),
        };
        let inner = Ctx {
            bg: None,
            attrs: Attrs::DIM,
        };
        assert_eq!(outer.within(inner), ctx);
    }

    #[test]
    fn visibility_depends_on_depth() {
        let t = Theme::test();
        let s = Styles::new(&t, ColorDepth::None);
        let panel = Style::PLAIN.on(Color::Rgb(t.surface));
        assert!(!s.bg_visible(&panel));
        let mut m = Styles::new(&t, ColorDepth::Mono);
        let rev = m.intern(Style::PLAIN.with(Attrs::REVERSE));
        assert!(m.space_visible(rev));
        let bg = m.intern(panel);
        assert!(!m.space_visible(bg));
        let mut c = Styles::new(&t, ColorDepth::Ansi16);
        let bg = c.intern(panel);
        assert!(c.space_visible(bg));
        assert!(!m.distinct(&Style::fg(Color::Ansi(1)), &Style::PLAIN));
        assert!(c.distinct(&Style::fg(Color::Ansi(1)), &Style::PLAIN));
    }
}
