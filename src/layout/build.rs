//! The line machinery shared by every block: the prefix stack, building one
//! line at a time, the blank-line rhythm, contexts and source positions.
//!
//! # Prefix stack
//!
//! Containers push a [`Level`]: list items their marker (first line) and
//! hanging indent (other lines), quotes their bar, definitions an indent.
//! Every line starts with the segments of all levels, outermost first.
//! Levels have one width for all their forms, so the content width
//! ([`Builder::avail`]) is the same on every line of a block. When nesting
//! is so deep that little room would be left, the prefix is cut from the
//! left, keeping the innermost markers.
//!
//! # Lines
//!
//! [`Builder::begin`] starts a line (after any pending blank line),
//! [`Builder::put`] appends text, and [`Builder::end`] finishes it with a
//! kind, a fill and a source offset. `put` never lets a line grow past the
//! measure: whatever would stick out is cut at a grapheme boundary. `end`
//! trims trailing spaces nobody would see, applies `align`, and records link
//! hits.
//!
//! # Blank lines
//!
//! One blank line separates blocks. [`Builder::blocks`] asks for a gap
//! before every block that follows one with output; the gap is only drawn
//! when the next line comes, with the prefix of the container that asked
//! for it, so empty blocks never double it and gaps never show a bar of a
//! container that starts after them.

use std::collections::HashMap;

use crate::highlight::Highlighter;
use crate::ir::{Block, Document, HAlign, HeadingId, Link, LinkId, LinkKind, SrcPos, Target};
use crate::options::{RenderOptions, When};
use crate::style::StyleId;
use crate::term::Caps;
use crate::text::width::next_grapheme_end;
use crate::text::{Wrapper, grapheme_width, str_width};
use crate::theme::{Element, Theme};

use super::deco::Deco;
use super::style::{Ctx, Styles};
use super::{Fill, ImageSizer, Layout, LineKind, LinkHit, Span, SpanFlags};

/// Content keeps at least this many columns (or half the measure, if
/// less) however deep the nesting.
const MIN_CONTENT: u16 = 10;

/// What [`super::layout`] was called with.
pub(super) struct Inputs<'a> {
    pub(super) doc: &'a Document,
    pub(super) theme: &'a Theme,
    pub(super) caps: &'a Caps,
    pub(super) opts: &'a RenderOptions,
    pub(super) highlighter: &'a dyn Highlighter,
    pub(super) sizer: &'a dyn ImageSizer,
}

/// A piece of a prefix.
#[derive(Clone, Debug)]
pub(super) struct Seg {
    pub(super) text: String,
    pub(super) style: StyleId,
    /// Display width (measured when the level is pushed).
    cols: u16,
}

impl Seg {
    pub(super) fn new(text: impl Into<String>, style: StyleId) -> Seg {
        Seg {
            text: text.into(),
            style,
            cols: 0,
        }
    }
}

/// One level of the prefix stack.
#[derive(Clone, Debug)]
struct Level {
    /// Segments of the level's first content line.
    first: Vec<Seg>,
    /// Segments of its other lines (blank lines too: trailing spaces that
    /// show nothing are trimmed).
    rest: Vec<Seg>,
    /// Width of every form.
    width: u16,
    first_pending: bool,
    /// Background drawn to the end of lines inside the level.
    panel: Option<(StyleId, u16)>,
}

/// The line being built.
#[derive(Clone, Copy, Debug, Default)]
struct Cur {
    /// First span of the line.
    first: usize,
    cols: u16,
    /// Where alignment padding goes (after the prefix outside the aligned
    /// container) and the column there.
    align_at: usize,
    align_col: u16,
    /// Spans before this index are never merged with new ones.
    merge_floor: usize,
    /// Columns taken by the whole prefix.
    prefix_cols: u16,
    /// The line places its content itself (centred figures and math):
    /// container alignment leaves it alone.
    placed: bool,
}

/// Numbered link references (when OSC 8 links are not available).
#[derive(Debug, Default)]
pub(super) struct Refs {
    pub(super) enabled: bool,
    next: u32,
    /// References of the current section, in order of first use.
    pending: Vec<(u32, LinkId)>,
    /// Numbers of the current section's URLs.
    by_url: HashMap<Box<str>, u32>,
}

impl Refs {
    /// The number of a link's URL in the current section.
    pub(super) fn number(&mut self, link: LinkId, url: &str) -> u32 {
        if let Some(&n) = self.by_url.get(url) {
            return n;
        }
        self.next = self.next.saturating_add(1);
        let n = self.next;
        self.by_url.insert(url.into(), n);
        self.pending.push((n, link));
        n
    }

    /// Take the current section's references.
    pub(super) fn take(&mut self) -> Vec<(u32, LinkId)> {
        self.by_url.clear();
        std::mem::take(&mut self.pending)
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Whether a link gets a numbered reference: it goes somewhere a reader
/// may want to look up, and its text is not already its URL.
pub(super) fn wants_ref(link: &Link) -> bool {
    !matches!(
        link.kind,
        LinkKind::Bare | LinkKind::Autolink | LinkKind::Email | LinkKind::Footnote
    ) && matches!(
        link.target,
        Target::External | Target::Email | Target::LocalDoc { .. } | Target::LocalFile(_)
    )
}

/// A fading rule: glyph width, glyph count, colour, fading at both ends.
pub(super) type FadeKey = (u16, u16, crate::style::Rgb, bool);

/// Lays out one document; see the module docs.
pub(super) struct Builder<'a> {
    pub(super) doc: &'a Document,
    pub(super) theme: &'a Theme,
    pub(super) caps: &'a Caps,
    pub(super) opts: &'a RenderOptions,
    pub(super) hl: &'a dyn Highlighter,
    pub(super) sizer: &'a dyn ImageSizer,
    pub(super) deco: Deco,
    pub(super) sty: Styles,
    pub(super) out: Layout,
    pub(super) refs: Refs,
    pub(super) wrapper: Wrapper,
    /// Fading rules already computed ([`Builder::fade`]).
    pub(super) fades: HashMap<FadeKey, Vec<(StyleId, u16)>>,
    /// Reused buffers for wrapping paragraphs.
    pub(super) scratch_lines: Vec<crate::text::Line>,
    pub(super) scratch_pieces: Vec<crate::text::Piece>,
    /// Measure East Asian Ambiguous characters as wide.
    pub(super) amb: bool,
    /// The top-level block being laid out.
    pub(super) top: u32,
    /// Content offset within it (see [`SrcPos`]).
    pub(super) off: u32,
    /// Element of paragraph text in the current container.
    pub(super) text_el: Element,
    /// Bullet lists around the current block.
    pub(super) list_depth: usize,
    /// Heading numbers per level (`heading.numbers`).
    pub(super) numbers: [u32; 6],
    levels: Vec<Level>,
    ctx: Vec<Ctx>,
    align: Option<(HAlign, usize)>,
    cur: Cur,
    pending_gap: Option<usize>,
    block_first: Option<u32>,
    pending_heading: Option<HeadingId>,
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

impl<'a> Builder<'a> {
    pub(super) fn new(inputs: Inputs<'a>, width: u16, indent: u16, measure: u16) -> Builder<'a> {
        let Inputs {
            doc,
            theme,
            caps,
            opts,
            highlighter,
            sizer,
        } = inputs;
        let refs = Refs {
            enabled: match opts.link_refs {
                When::Always => true,
                When::Never => false,
                When::Auto => !caps.hyperlinks,
            },
            ..Refs::default()
        };
        // Capacity from the size of the top-level text, so large documents
        // do not grow the arrays a doubling at a time.
        let text_hint = text_estimate(doc);
        Builder {
            doc,
            theme,
            caps,
            opts,
            hl: highlighter,
            sizer,
            deco: Deco::new(opts),
            sty: Styles::new(theme, caps.color),
            out: Layout {
                width,
                measure,
                indent,
                text: String::with_capacity(text_hint),
                spans: Vec::with_capacity(text_hint / 48),
                lines: Vec::with_capacity(text_hint / 64),
                block_lines: Vec::with_capacity(doc.blocks.len()),
                heading_line: vec![u32::MAX; doc.headings.len()],
                link_hits: Vec::new(),
                images: Vec::new(),
                code: Vec::new(),
                styles: crate::style::StyleTable::new(),
            },
            refs,
            wrapper: Wrapper::new(),
            fades: HashMap::new(),
            scratch_lines: Vec::new(),
            scratch_pieces: Vec::new(),
            amb: opts.ambiguous_wide,
            top: 0,
            off: 0,
            text_el: Element::Text,
            list_depth: 0,
            numbers: [0; 6],
            levels: Vec::new(),
            ctx: Vec::new(),
            align: None,
            cur: Cur::default(),
            pending_gap: None,
            block_first: None,
            pending_heading: None,
        }
    }

    /// Lay out every top-level block and finish the side tables.
    pub(super) fn run(mut self) -> Layout {
        let doc = self.doc;
        let mut firsts: Vec<u32> = Vec::with_capacity(doc.blocks.len());
        let mut produced = false;
        for (i, block) in doc.blocks.iter().enumerate() {
            if self.refs.has_pending() && starts_section(block) {
                self.flush_refs();
            }
            self.top = to_u32(i);
            self.off = 0;
            self.block_first = None;
            let before = self.out.lines.len();
            if produced {
                self.request_gap();
            }
            self.block(block);
            if self.out.lines.len() > before {
                produced = true;
            }
            firsts.push(self.block_first.unwrap_or(to_u32(self.out.lines.len())));
        }
        self.pending_gap = None;
        if self.refs.has_pending() {
            self.flush_refs();
        }
        let end = to_u32(self.out.lines.len());
        self.out.block_lines = firsts
            .iter()
            .enumerate()
            .map(|(i, &start)| start..firsts.get(i + 1).copied().unwrap_or(end))
            .collect();
        self.out.styles = self.sty.into_table();
        self.out
    }

    /// Lay out a list of sibling blocks, one blank line apart unless
    /// `tight`.
    pub(super) fn blocks(&mut self, blocks: &[Block], tight: bool) {
        let mut produced = false;
        for b in blocks {
            let before = self.out.lines.len();
            if produced && !tight {
                self.request_gap();
            }
            self.block(b);
            if self.out.lines.len() > before {
                produced = true;
            }
        }
        self.pending_gap = None;
    }

    /// Ask for a blank line before the next line.
    pub(super) fn request_gap(&mut self) {
        if self.pending_gap.is_none() {
            self.pending_gap = Some(self.levels.len());
        }
    }

    /// The first line of the next content line belongs to this heading.
    pub(super) fn mark_heading(&mut self, id: HeadingId) {
        self.pending_heading = Some(id);
    }

    // ----- prefix levels, contexts, alignment -------------------------------

    /// Push a prefix level. The forms are padded to one width with spaces.
    pub(super) fn push_level(&mut self, first: Vec<Seg>, rest: Vec<Seg>) {
        self.push_level_with_panel(first, rest, None);
    }

    /// [`Builder::push_level`] with a background drawn to `to_col` on every
    /// line inside the level. A level pushed inside a context (a list in an
    /// alert) takes the context on.
    pub(super) fn push_level_with_panel(
        &mut self,
        mut first: Vec<Seg>,
        mut rest: Vec<Seg>,
        panel: Option<(StyleId, u16)>,
    ) {
        let ctx = self.ctx_now();
        for seg in first.iter_mut().chain(rest.iter_mut()) {
            seg.style = self.sty.in_ctx(seg.style, ctx);
            seg.cols = to_u16(str_width(&seg.text, self.amb));
        }
        let width_of =
            |segs: &[Seg]| -> u16 { segs.iter().fold(0u16, |acc, s| acc.saturating_add(s.cols)) };
        let width = width_of(&first).max(width_of(&rest));
        let plain = self.sty.in_ctx(StyleId(0), ctx);
        for segs in [&mut first, &mut rest] {
            let w = width_of(segs);
            if w < width {
                let mut pad = Seg::new(" ".repeat(usize::from(width - w)), plain);
                pad.cols = width - w;
                segs.push(pad);
            }
        }
        self.levels.push(Level {
            first,
            rest,
            width,
            first_pending: true,
            panel,
        });
    }

    /// Push a level of `cols` spaces.
    pub(super) fn push_indent(&mut self, cols: u16) {
        let spaces = Seg::new(" ".repeat(usize::from(cols)), StyleId(0));
        self.push_level(vec![spaces.clone()], vec![spaces]);
    }

    /// Pop the innermost prefix level.
    pub(super) fn pop_level(&mut self) {
        self.levels.pop();
    }

    /// Whether the innermost level has not drawn its first line yet.
    pub(super) fn level_fresh(&self) -> bool {
        self.levels.last().is_some_and(|l| l.first_pending)
    }

    /// Push a context for content spans.
    pub(super) fn push_ctx(&mut self, ctx: Ctx) {
        let outer = self.ctx.last().copied().unwrap_or_default();
        self.ctx.push(outer.within(ctx));
    }

    /// Pop the innermost context.
    pub(super) fn pop_ctx(&mut self) {
        self.ctx.pop();
    }

    /// The context in effect.
    pub(super) fn ctx_now(&self) -> Ctx {
        self.ctx.last().copied().unwrap_or_default()
    }

    /// Align the lines of the blocks laid out by `f`.
    pub(super) fn aligned(&mut self, align: HAlign, f: impl FnOnce(&mut Self)) {
        let saved = self.align;
        self.align = match align {
            HAlign::Left => None,
            a => Some((a, self.levels.len())),
        };
        f(self);
        self.align = saved;
    }

    /// Columns available for content at the current nesting.
    pub(super) fn avail(&self) -> u16 {
        self.avail_after(self.prefix_width())
    }

    fn prefix_width(&self) -> u16 {
        self.levels
            .iter()
            .fold(0u16, |acc, l| acc.saturating_add(l.width))
    }

    fn avail_after(&self, prefix: u16) -> u16 {
        let measure = self.out.measure;
        let floor = MIN_CONTENT.min(measure / 2).max(1);
        measure.saturating_sub(prefix).max(floor)
    }

    /// The right edge of the content (relative to the text column).
    pub(super) fn right_edge(&self) -> u16 {
        self.out.measure
    }

    // ----- lines -------------------------------------------------------------

    /// Start a content line: draw a pending blank line, then the prefix.
    pub(super) fn begin(&mut self) {
        self.flush_gap();
        if self.block_first.is_none() {
            self.block_first = Some(to_u32(self.out.lines.len()));
        }
        if let Some(h) = self.pending_heading.take()
            && let Some(slot) = self.out.heading_line.get_mut(h.index())
        {
            *slot = to_u32(self.out.lines.len());
        }
        self.start_prefix(self.levels.len(), false);
    }

    /// Start a line with the prefix of the outer `depth` levels; `blank`
    /// lines (gaps) never draw a first-line marker.
    fn start_prefix(&mut self, depth: usize, blank: bool) {
        let first = self.out.spans.len();
        self.cur = Cur {
            first,
            cols: 0,
            align_at: first,
            align_col: 0,
            merge_floor: first,
            prefix_cols: 0,
            placed: false,
        };
        let total = self
            .levels
            .iter()
            .take(depth)
            .fold(0u16, |acc, l| acc.saturating_add(l.width));
        let avail = self.avail_after(total);
        let mut skip = total.saturating_add(avail).saturating_sub(self.out.measure);
        let align_depth = self.align.map(|(_, d)| d);
        let mut levels = std::mem::take(&mut self.levels);
        for (i, level) in levels.iter_mut().take(depth).enumerate() {
            if !blank && align_depth == Some(i) {
                self.mark_align();
            }
            // Blank lines never use a level's first form (its marker).
            let segs = if blank {
                &level.rest
            } else if level.first_pending {
                level.first_pending = false;
                &level.first
            } else {
                &level.rest
            };
            for seg in segs {
                self.put_prefix(seg, &mut skip);
            }
        }
        self.levels = levels;
        if !blank && align_depth.is_some_and(|d| d >= depth) {
            self.mark_align();
        }
        self.cur.prefix_cols = self.cur.cols;
        self.cur.merge_floor = self.out.spans.len();
    }

    fn mark_align(&mut self) {
        self.cur.align_at = self.out.spans.len();
        self.cur.align_col = self.cur.cols;
        self.cur.merge_floor = self.out.spans.len();
    }

    /// Append a prefix segment, dropping the first `skip` columns.
    fn put_prefix(&mut self, seg: &Seg, skip: &mut u16) {
        let (text, style) = (seg.text.as_str(), seg.style);
        if *skip == 0 {
            self.put_raw_cols(text, Some(seg.cols), style, None, SpanFlags::empty());
            return;
        }
        let mut pos = 0;
        let mut pad = 0u16;
        while pos < text.len() && *skip > 0 {
            let end = next_grapheme_end(text, pos);
            let w = to_u16(grapheme_width(text.get(pos..end).unwrap_or(""), self.amb));
            if w > *skip {
                // A wide glyph straddles the cut: keep its visible part as spaces.
                pad = w - *skip;
                *skip = 0;
            } else {
                *skip -= w;
            }
            pos = end;
        }
        if pad > 0 {
            self.put_raw(
                &" ".repeat(usize::from(pad)),
                style,
                None,
                SpanFlags::empty(),
            );
        }
        if let Some(rest) = text.get(pos..)
            && !rest.is_empty()
        {
            self.put_raw(rest, style, None, SpanFlags::empty());
        }
    }

    /// Draw a pending blank line.
    fn flush_gap(&mut self) {
        let Some(depth) = self.pending_gap.take() else {
            return;
        };
        let depth = depth.min(self.levels.len());
        self.start_prefix(depth, true);
        let pos = self.out.lines.last().map_or(
            SrcPos {
                top: self.top,
                off: self.off,
            },
            |l| l.pos,
        );
        let fill = self.panel_fill(depth);
        self.finish(LineKind::Blank, fill, pos);
    }

    /// The innermost panel among the outer `depth` levels.
    fn panel_fill(&self, depth: usize) -> Fill {
        self.levels
            .iter()
            .take(depth)
            .rev()
            .find_map(|l| l.panel)
            .map_or(Fill::None, |(style, to_col)| Fill::Panel { style, to_col })
    }

    /// The current line positions its content itself (see
    /// [`Builder::aligned`]).
    pub(super) fn placed(&mut self) {
        self.cur.placed = true;
    }

    /// Columns used on the current line.
    pub(super) fn cols(&self) -> u16 {
        self.cur.cols
    }

    /// Columns used after the prefix.
    pub(super) fn content_cols(&self) -> u16 {
        self.cur.cols.saturating_sub(self.cur.prefix_cols)
    }

    /// Append content text in `style` (with the current context).
    pub(super) fn put(&mut self, text: &str, style: StyleId, link: Option<LinkId>) {
        self.put_flags(text, style, link, SpanFlags::empty());
    }

    /// [`Builder::put`] with span flags.
    pub(super) fn put_flags(
        &mut self,
        text: &str,
        style: StyleId,
        link: Option<LinkId>,
        flags: SpanFlags,
    ) {
        let ctx = self.ctx_now();
        let style = self.sty.in_ctx(style, ctx);
        self.put_raw(text, style, link, flags);
    }

    /// Append a decoration whose width is known (`cols`), in `style` (with
    /// the current context): borders and rules need no measuring.
    pub(super) fn put_known(&mut self, text: &str, cols: u16, style: StyleId) {
        let ctx = self.ctx_now();
        let style = self.sty.in_ctx(style, ctx);
        self.put_raw_cols(text, Some(cols), style, None, SpanFlags::empty());
    }

    /// A one-column border glyph (every [`super::deco::Borders`] piece).
    pub(super) fn put_glyph(&mut self, glyph: &str, style: StyleId) {
        self.put_known(glyph, 1, style);
    }

    /// `n` spaces in `style` (with the current context).
    pub(super) fn spaces(&mut self, n: u16, style: StyleId) {
        const SPACES: &str = "                                                                ";
        let mut left = n;
        while left > 0 {
            let take = left.min(SPACES.len() as u16);
            self.put_known(SPACES.get(..usize::from(take)).unwrap_or(""), take, style);
            left -= take;
        }
    }

    /// `glyph` repeated to fill `cols` columns (spaces make up a remainder).
    pub(super) fn repeat(&mut self, glyph: &str, cols: u16, style: StyleId) {
        let w = to_u16(str_width(glyph, self.amb)).max(1);
        let n = cols / w;
        if n > 0 {
            self.put_known(&glyph.repeat(usize::from(n)), n * w, style);
        }
        self.spaces(cols - n * w, style);
    }

    /// Append text without the context, cut at the measure.
    pub(super) fn put_raw(
        &mut self,
        text: &str,
        style: StyleId,
        link: Option<LinkId>,
        flags: SpanFlags,
    ) {
        self.put_raw_cols(text, None, style, link, flags);
    }

    /// [`Builder::put_raw`] with the width given when it is known.
    fn put_raw_cols(
        &mut self,
        text: &str,
        known: Option<u16>,
        style: StyleId,
        link: Option<LinkId>,
        flags: SpanFlags,
    ) {
        if text.is_empty() {
            return;
        }
        let limit = self.out.measure;
        let room = limit.saturating_sub(self.cur.cols);
        let full = known.map_or_else(|| str_width(text, self.amb), usize::from);
        let (text, cols) = if full <= usize::from(room) {
            (text, to_u16(full))
        } else {
            cut_to_cols(text, room, self.amb)
        };
        if text.is_empty() {
            return;
        }
        let off = to_u32(self.out.text.len());
        let len = to_u32(text.len());
        self.out.text.push_str(text);
        self.cur.cols = self.cur.cols.saturating_add(cols);
        if self.out.spans.len() > self.cur.merge_floor
            && let Some(last) = self.out.spans.last_mut()
            && last.style == style
            && last.link == link
            && last.flags == flags
            && last.off.saturating_add(last.len) == off
        {
            last.len = last.len.saturating_add(len);
            last.cols = last.cols.saturating_add(cols);
            return;
        }
        self.out.spans.push(Span {
            off,
            len,
            cols,
            style,
            link,
            flags,
        });
    }

    /// Finish the current line. A line without a fill of its own gets the
    /// innermost panel of its container.
    pub(super) fn end(&mut self, kind: LineKind, fill: Fill, off: u32) {
        let fill = match fill {
            Fill::None => self.panel_fill(self.levels.len()),
            f => f,
        };
        let pos = SrcPos { top: self.top, off };
        self.finish(kind, fill, pos);
    }

    fn finish(&mut self, kind: LineKind, fill: Fill, pos: SrcPos) {
        let fill = self.visible_fill(fill);
        if fill == Fill::None {
            self.trim_trailing();
        }
        if let Some((align, _)) = self.align
            && kind != LineKind::Blank
            && !self.cur.placed
        {
            self.align_line(align);
        }
        let first = self.cur.first.min(self.out.spans.len());
        let n = self.out.spans.len() - first;
        let line = to_u32(self.out.lines.len());
        self.record_links(line, first);
        // Monotonic positions even if a caller computes one too small.
        let pos = match self.out.lines.last() {
            Some(prev) if prev.pos > pos => prev.pos,
            _ => pos,
        };
        self.out.lines.push(super::Line {
            first: to_u32(first),
            n: to_u32(n),
            cols: self.cur.cols,
            kind,
            fill,
            pos,
        });
        self.cur = Cur {
            first: self.out.spans.len(),
            ..Cur::default()
        };
    }

    /// A fill that would show nothing at this depth is dropped.
    fn visible_fill(&self, fill: Fill) -> Fill {
        match fill {
            Fill::Panel { style, to_col }
                if to_col <= self.cur.cols || !self.sty.space_visible(style) =>
            {
                Fill::None
            }
            Fill::Panel { style, to_col } => Fill::Panel {
                style,
                to_col: to_col.min(self.out.measure),
            },
            Fill::Gradient { x0, x1, .. } if x1 <= x0 => Fill::None,
            Fill::Gradient { from, to, x0, x1 } => Fill::Gradient {
                from,
                to,
                x0,
                x1: x1.min(self.out.measure),
            },
            Fill::None => Fill::None,
        }
    }

    /// Remove trailing spaces that show nothing.
    fn trim_trailing(&mut self) {
        while self.out.spans.len() > self.cur.first {
            let Some(&last) = self.out.spans.last() else {
                break;
            };
            if self.sty.space_visible(last.style) || last.link.is_some() {
                break;
            }
            let text = self.out.span_text(&last);
            let kept = text.trim_end_matches(' ').len();
            let removed = text.len() - kept;
            if removed == 0 {
                break;
            }
            self.out.text.truncate(last.off as usize + kept);
            self.cur.cols = self.cur.cols.saturating_sub(to_u16(removed));
            if kept == 0 {
                self.out.spans.pop();
            } else if let Some(span) = self.out.spans.last_mut() {
                span.len = to_u32(kept);
                span.cols = span.cols.saturating_sub(to_u16(removed));
                break;
            }
        }
    }

    /// Insert padding so the content after the aligned container's prefix
    /// is centred or right-aligned.
    fn align_line(&mut self, align: HAlign) {
        let content = self.cur.cols.saturating_sub(self.cur.align_col);
        let room = self.out.measure.saturating_sub(self.cur.align_col);
        let pad = match align {
            HAlign::Center => room.saturating_sub(content) / 2,
            HAlign::Right => room.saturating_sub(content),
            HAlign::Left => 0,
        };
        if pad == 0 || self.cur.cols == self.cur.align_col {
            return;
        }
        let off = to_u32(self.out.text.len());
        self.out.text.push_str(&" ".repeat(usize::from(pad)));
        let at = self
            .cur
            .align_at
            .clamp(self.cur.first, self.out.spans.len());
        self.out.spans.insert(
            at,
            Span {
                off,
                len: u32::from(pad),
                cols: pad,
                style: StyleId(0),
                link: None,
                flags: SpanFlags::empty(),
            },
        );
        self.cur.cols = self.cur.cols.saturating_add(pad);
    }

    /// Record the link fragments of the line's spans.
    fn record_links(&mut self, line: u32, first: usize) {
        let mut col = 0u16;
        let mut open: Option<LinkHit> = None;
        for span in self.out.spans.get(first..).unwrap_or(&[]) {
            let start = col;
            col = col.saturating_add(span.cols);
            let back = span.flags.contains(SpanFlags::BACKLINK);
            match (span.link, open.as_mut()) {
                (Some(l), Some(hit))
                    if hit.link == l && hit.back == back && hit.cols.end == start =>
                {
                    hit.cols.end = col;
                }
                (link, _) => {
                    if let Some(hit) = open.take() {
                        self.out.link_hits.push(hit);
                    }
                    open = link.map(|link| LinkHit {
                        line,
                        cols: start..col,
                        link,
                        back,
                    });
                }
            }
        }
        if let Some(hit) = open {
            self.out.link_hits.push(hit);
        }
    }

    /// A line holding only `text` in `style`.
    pub(super) fn text_line(&mut self, text: &str, style: StyleId, off: u32) {
        self.begin();
        self.put(text, style, None);
        self.end(LineKind::Text, Fill::None, off);
    }

    // ----- link references ---------------------------------------------------

    /// Draw the reference list of the current section: `[n]: url` lines.
    pub(super) fn flush_refs(&mut self) {
        let refs = self.refs.take();
        if refs.is_empty() {
            return;
        }
        if !self.out.lines.is_empty() {
            self.request_gap();
        }
        let marker_style = self.sty.el(Element::LinkRef);
        let url_style = self.sty.el(Element::LinkUrl);
        let width = refs
            .last()
            .map_or(3, |(n, _)| to_u16(n.to_string().len() + 3));
        let off = self.off;
        let doc = self.doc;
        for (n, link) in refs {
            let url = doc.link(link).map_or("", |l| &l.url);
            let marker = format!("[{n}]:");
            let pad = width.saturating_sub(to_u16(marker.len()));
            let mut first = vec![Seg::new(marker, marker_style)];
            first.push(Seg::new(" ".repeat(usize::from(pad) + 1), StyleId(0)));
            let rest = vec![Seg::new(" ".repeat(usize::from(width) + 1), StyleId(0))];
            self.push_level(first, rest);
            self.url_lines(url, url_style, Some(link), off);
            self.pop_level();
        }
    }
}

/// About how much text a document shows: its paragraphs and code, plus a
/// little for every other block.
fn text_estimate(doc: &Document) -> usize {
    doc.blocks
        .iter()
        .map(|b| match b {
            Block::Para(t) | Block::Heading { text: t, .. } => t.text.len() + 8,
            Block::Code(c) => c.code.len() + 256,
            _ => 256,
        })
        .fold(0usize, usize::saturating_add)
        .min(1 << 30)
}

/// Whether a top-level block starts a new h1/h2 section.
fn starts_section(block: &Block) -> bool {
    matches!(block, Block::Heading { level, .. } if *level <= 2)
}

/// The longest prefix of `text` that fits `cols` columns (whole graphemes)
/// and its width.
pub(super) fn cut_to_cols(text: &str, cols: u16, amb: bool) -> (&str, u16) {
    let mut pos = 0;
    let mut used = 0u16;
    while pos < text.len() {
        let end = next_grapheme_end(text, pos);
        let w = to_u16(grapheme_width(text.get(pos..end).unwrap_or(""), amb));
        if used.saturating_add(w) > cols {
            break;
        }
        used += w;
        pos = end;
    }
    (text.get(..pos).unwrap_or(""), used)
}
