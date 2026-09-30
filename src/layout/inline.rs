//! Inline content: composing an [`Inlines`] into the text that is shown,
//! then wrapping and emitting it.
//!
//! *Composing* turns the IR's runs into display text with one style per
//! run: math is typeset, `<sup>`/`<sub>` become Unicode scripts, footnote
//! references superscript digits, image chips `▣ alt`, code spans and key
//! caps pills (padded where their background shows, in backticks or
//! brackets where nothing would set them apart), and links without OSC 8 a
//! numbered reference. Line-breaking constraints follow: atoms for math,
//! footnote numbers, key caps and code spans up to 24 columns; extra break
//! points for URLs and at math break hints. Plain prose skips all of it and
//! borrows the IR text.
//!
//! A composed text keeps a map back to IR offsets, so every line records
//! where its content came from ([`crate::ir::SrcPos`]).

use std::borrow::Cow;
use std::ops::Range;

use crate::ir::{InlineFlags, Inlines, LinkId, RunKind};
use crate::style::StyleId;
use crate::text::wrap::Piece;
use crate::text::{
    Constraints, Line as WrapLine, WrapOptions, split_runs, str_width, strip_soft_hyphens,
};
use crate::theme::Element;

use super::build::{Builder, wants_ref};
use super::scripts::{subscript, superscript};
use super::style::Piece as Kind;
use super::{Fill, LineKind, SpanFlags};

/// Code spans up to this many columns never break across lines.
const CODE_ATOM_COLS: usize = 24;

/// No-break space: pads pills without allowing a break inside them.
const NBSP: char = '\u{a0}';

/// A styled run of composed text: bytes `[previous end, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CRun {
    pub(super) end: u32,
    pub(super) style: StyleId,
    pub(super) link: Option<LinkId>,
    pub(super) flags: SpanFlags,
    /// Its no-break spaces only hold the text together (pill padding, the
    /// space after an image chip's marker) and are drawn as spaces.
    pill: bool,
}

/// Where a stretch of composed text came from.
#[derive(Clone, Copy, Debug)]
struct MapEntry {
    /// Start in the composed text.
    disp: u32,
    /// Start and end in the IR text.
    ir: u32,
    ir_end: u32,
    /// Copied byte for byte (offsets inside map one to one).
    verbatim: bool,
}

/// Inline content ready to wrap; see the module docs.
#[derive(Clone, Debug)]
pub(super) struct Composed<'t> {
    pub(super) text: Cow<'t, str>,
    pub(super) runs: Vec<CRun>,
    pub(super) atoms: Vec<Range<u32>>,
    pub(super) breaks: Vec<u32>,
    /// Empty when the text is the IR text itself.
    map: Vec<MapEntry>,
}

/// A run to build composed text from ([`Composed::from_parts`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct CRunSpec {
    pub(super) end: u32,
    pub(super) style: StyleId,
}

impl Composed<'static> {
    /// Composed text from owned parts; every line maps to IR offset 0.
    pub(super) fn from_parts(
        text: String,
        specs: &[CRunSpec],
        atoms: Vec<Range<u32>>,
        breaks: Vec<u32>,
    ) -> Composed<'static> {
        let len = to_u32(text.len());
        let mut runs: Vec<CRun> = Vec::with_capacity(specs.len());
        let mut prev = 0;
        for s in specs {
            let end = s.end.min(len);
            if end > prev {
                runs.push(CRun {
                    end,
                    style: s.style,
                    link: None,
                    flags: SpanFlags::empty(),
                    pill: false,
                });
                prev = end;
            }
        }
        if prev < len {
            let style = specs.last().map_or(StyleId(0), |s| s.style);
            runs.push(CRun {
                end: len,
                style,
                link: None,
                flags: SpanFlags::empty(),
                pill: false,
            });
        }
        Composed {
            text: Cow::Owned(text),
            runs,
            atoms,
            breaks,
            map: vec![MapEntry {
                disp: 0,
                ir: 0,
                ir_end: 0,
                verbatim: false,
            }],
        }
    }

    /// Several composed texts one after another; every line maps to IR
    /// offset 0.
    pub(super) fn concat(parts: &[&Composed<'_>]) -> Composed<'static> {
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut atoms = Vec::new();
        let mut breaks = Vec::new();
        for part in parts {
            let shift = to_u32(text.len());
            text.push_str(&part.text);
            runs.extend(part.runs.iter().map(|r| CRun {
                end: r.end.saturating_add(shift),
                ..*r
            }));
            atoms.extend(
                part.atoms
                    .iter()
                    .map(|a| a.start.saturating_add(shift)..a.end.saturating_add(shift)),
            );
            breaks.extend(part.breaks.iter().map(|b| b.saturating_add(shift)));
        }
        Composed {
            text: Cow::Owned(text),
            runs,
            atoms,
            breaks,
            map: vec![MapEntry {
                disp: 0,
                ir: 0,
                ir_end: 0,
                verbatim: false,
            }],
        }
    }
}

impl Composed<'_> {
    /// The same text with its no-break spaces drawn as spaces.
    pub(super) fn spaced(mut self) -> Self {
        for r in &mut self.runs {
            r.pill = true;
        }
        self
    }

    /// The IR offset of a composed offset.
    pub(super) fn ir_offset(&self, disp: u32) -> u32 {
        if self.map.is_empty() {
            return disp;
        }
        let i = self.map.partition_point(|e| e.disp <= disp);
        match i.checked_sub(1).and_then(|i| self.map.get(i)) {
            Some(e) if e.verbatim => e.ir.saturating_add(disp - e.disp).min(e.ir_end),
            Some(e) => e.ir,
            None => 0,
        }
    }

    /// Width of the widest line (hard breaks split lines).
    pub(super) fn natural_width(&self, amb: bool) -> u16 {
        self.text
            .split('\n')
            .map(|l| to_u16(str_width(&strip_soft_hyphens(l.trim_end_matches(' ')), amb)))
            .max()
            .unwrap_or(0)
    }

    /// Width of the widest piece that can not be broken (words, atoms).
    pub(super) fn min_width(&self, amb: bool) -> u16 {
        let mut breaks = Vec::new();
        crate::text::break_opportunities(&self.text, &self.breaks, &mut breaks);
        let mut widest = 0u16;
        let mut start = 0usize;
        let mut atoms = self.atoms.iter().peekable();
        for &b in breaks
            .iter()
            .chain(std::iter::once(&to_u32(self.text.len())))
        {
            let b = (b as usize).min(self.text.len());
            while atoms.next_if(|a| (a.end as usize) <= b).is_some() {}
            if atoms
                .peek()
                .is_some_and(|a| (a.start as usize) < b && b < a.end as usize)
            {
                continue; // inside an atom: no break here
            }
            if b <= start {
                continue;
            }
            let piece = self.text.get(start..b).unwrap_or("");
            let piece = piece.trim_end_matches([' ', '\n']);
            widest = widest.max(to_u16(str_width(&strip_soft_hyphens(piece), amb)));
            start = b;
        }
        widest
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// Text appended after the content (footnote back-links).
#[derive(Clone, Debug)]
pub(super) struct Tail {
    pub(super) text: String,
    pub(super) style: StyleId,
    pub(super) link: Option<LinkId>,
    pub(super) flags: SpanFlags,
}

/// The composed text being built.
#[derive(Default)]
struct Out {
    text: String,
    runs: Vec<CRun>,
    atoms: Vec<Range<u32>>,
    breaks: Vec<u32>,
    map: Vec<MapEntry>,
}

impl Out {
    fn len(&self) -> u32 {
        to_u32(self.text.len())
    }

    /// Append text with a style; returns its range.
    fn push(
        &mut self,
        s: &str,
        style: StyleId,
        link: Option<LinkId>,
        flags: SpanFlags,
        pill: bool,
    ) -> Range<u32> {
        let start = self.len();
        if s.is_empty() {
            return start..start;
        }
        self.text.push_str(s);
        let end = self.len();
        match self.runs.last_mut() {
            Some(last)
                if last.style == style
                    && last.link == link
                    && last.flags == flags
                    && last.pill == pill =>
            {
                last.end = end;
            }
            _ => self.runs.push(CRun {
                end,
                style,
                link,
                flags,
                pill,
            }),
        }
        start..end
    }

    /// Record where the next text comes from.
    fn map(&mut self, ir: Range<u32>, verbatim: bool) {
        let disp = self.len();
        if let Some(last) = self.map.last_mut()
            && last.disp == disp
        {
            *last = MapEntry {
                disp,
                ir: ir.start,
                ir_end: ir.end,
                verbatim,
            };
            return;
        }
        self.map.push(MapEntry {
            disp,
            ir: ir.start,
            ir_end: ir.end,
            verbatim,
        });
    }

    fn atom(&mut self, r: Range<u32>) {
        if r.start < r.end {
            self.atoms.push(r);
        }
    }
}

/// What a line-emitting caller wants around the text.
#[derive(Clone, Copy, Debug)]
pub(super) struct Look {
    pub(super) kind: LineKind,
    pub(super) fill: Fill,
    /// Columns of `pad_style` before the text on every line (bars).
    pub(super) pad: u16,
    pub(super) pad_style: StyleId,
    /// Indent of continuation lines, after the pad.
    pub(super) hang: u16,
}

impl Look {
    /// Plain text lines.
    pub(super) const TEXT: Look = Look {
        kind: LineKind::Text,
        fill: Fill::None,
        pad: 0,
        pad_style: StyleId(0),
        hang: 0,
    };
}

impl<'a> Builder<'a> {
    /// Compose inline content on top of the `base` style.
    pub(super) fn compose<'t>(&mut self, inl: &'t Inlines, base: StyleId) -> Composed<'t> {
        self.compose_with(inl, base, &[])
    }

    /// [`Builder::compose`] with text appended at the end.
    pub(super) fn compose_with<'t>(
        &mut self,
        inl: &'t Inlines,
        base: StyleId,
        tail: &[Tail],
    ) -> Composed<'t> {
        if tail.is_empty() && self.is_plain(inl) {
            let runs = inl
                .runs
                .iter()
                .map(|r| CRun {
                    end: r.end,
                    style: self.sty.inline(base, r.flags, Kind::Text, r.link.is_some()),
                    link: r.link,
                    flags: SpanFlags::empty(),
                    pill: false,
                })
                .collect();
            return Composed {
                text: Cow::Borrowed(&inl.text),
                runs,
                atoms: inl.atoms.clone(),
                breaks: inl.extra_breaks.clone(),
                map: Vec::new(),
            };
        }
        let mut out = Out {
            text: String::with_capacity(inl.text.len() + 16),
            ..Out::default()
        };
        let runs: Vec<(Range<u32>, &crate::ir::Run)> = inl.runs_with_ranges().collect();
        let mut kbd_start: Option<u32> = None;
        for (i, (range, run)) in runs.iter().enumerate() {
            let src = inl.slice(range.clone());
            let link = run.link;
            let has_link = link.is_some();
            match run.kind {
                RunKind::Text | RunKind::Html => {
                    let kind = if run.kind == RunKind::Html {
                        Kind::Html
                    } else {
                        Kind::Text
                    };
                    let style = self.sty.inline(base, run.flags, kind, has_link);
                    let kbd = run.flags.contains(InlineFlags::KBD);
                    let pill = kbd && self.sty.pill(&self.sty.get(style));
                    if kbd && kbd_start.is_none() {
                        kbd_start = Some(out.len());
                        out.map(range.start..range.start, false);
                        let open = if pill { "\u{a0}" } else { "[" };
                        out.push(open, style, link, SpanFlags::empty(), pill);
                    }
                    let scripted = if run.flags.contains(InlineFlags::SUP) {
                        superscript(src)
                    } else if run.flags.contains(InlineFlags::SUB) {
                        subscript(src)
                    } else {
                        None
                    };
                    match scripted {
                        Some(s) => {
                            out.map(range.clone(), false);
                            out.push(&s, style, link, SpanFlags::empty(), false);
                        }
                        None => {
                            out.map(range.clone(), true);
                            let at = out.len();
                            out.push(src, style, link, SpanFlags::empty(), pill);
                            for &b in &inl.extra_breaks {
                                if b > range.start && b < range.end {
                                    out.breaks.push(at + (b - range.start));
                                } else if b == range.start && b > 0 {
                                    out.breaks.push(at);
                                }
                            }
                        }
                    }
                    let next_kbd = runs
                        .get(i + 1)
                        .is_some_and(|(_, r)| r.flags.contains(InlineFlags::KBD));
                    if kbd
                        && !next_kbd
                        && let Some(start) = kbd_start.take()
                    {
                        let close = if pill { "\u{a0}" } else { "]" };
                        out.push(close, style, link, SpanFlags::empty(), pill);
                        let end = out.len();
                        out.atom(start..end);
                    }
                }
                RunKind::Code => {
                    let style = self.sty.inline(base, run.flags, Kind::Code, has_link);
                    let s = self.sty.get(style);
                    let pill = self.sty.pill(&s);
                    let marked = self.sty.distinct(&s, &self.sty.get(base));
                    let (open, close) = if pill {
                        ("\u{a0}", "\u{a0}")
                    } else if marked {
                        ("", "")
                    } else {
                        ("`", "`")
                    };
                    let start = out.len();
                    out.map(range.start..range.start, false);
                    out.push(open, style, link, SpanFlags::empty(), pill);
                    out.map(range.clone(), true);
                    out.push(src, style, link, SpanFlags::empty(), pill);
                    out.push(close, style, link, SpanFlags::empty(), pill);
                    let end = out.len();
                    let cols = str_width(out.text.get(start as usize..).unwrap_or(""), self.amb);
                    if cols <= CODE_ATOM_COLS {
                        out.atom(start..end);
                    }
                    if pill && start > 0 {
                        // UAX #14 allows a break between a space and a
                        // no-break space; keep it explicit.
                        out.breaks.push(start);
                    }
                }
                RunKind::Math => {
                    out.map(range.start..range.end, false);
                    self.compose_math(src, base, run.flags, link, &mut out);
                }
                RunKind::FootRef(_) => {
                    let style = self.sty.inline(base, run.flags, Kind::FootRef, has_link);
                    let text = match superscript(src) {
                        Some(s) if !self.deco.ascii => s,
                        _ => format!("[{src}]"),
                    };
                    out.map(range.clone(), false);
                    let r = out.push(&text, style, link, SpanFlags::empty(), false);
                    out.atom(r);
                }
                RunKind::ImageChip(_) => {
                    let glyph = self.sty.inline(base, run.flags, Kind::ChipGlyph, has_link);
                    let alt = self.sty.inline(base, run.flags, Kind::ChipAlt, has_link);
                    out.map(range.clone(), false);
                    let chip = format!("{}{NBSP}", self.deco.chip);
                    out.push(&chip, glyph, link, SpanFlags::empty(), true);
                    out.push(src, alt, link, SpanFlags::empty(), false);
                }
            }
            // A numbered reference after the last run of a link.
            if let Some(l) = link
                && self.refs.enabled
                && runs.get(i + 1).is_none_or(|(_, r)| r.link != Some(l))
                && let Some(target) = self.doc.link(l)
                && wants_ref(target)
            {
                let n = self.refs.number(l, &target.url);
                let style = self
                    .sty
                    .inline(base, InlineFlags::empty(), Kind::LinkRef, false);
                out.map(range.end..range.end, false);
                out.push(&format!("[{n}]"), style, None, SpanFlags::empty(), false);
            }
        }
        let ir_end = to_u32(inl.text.len());
        for t in tail {
            out.map(ir_end..ir_end, false);
            out.push(&t.text, t.style, t.link, t.flags, false);
        }
        out.breaks
            .retain(|&b| b > 0 && (b as usize) < out.text.len());
        out.breaks.sort_unstable();
        out.breaks.dedup();
        Composed {
            text: Cow::Owned(out.text),
            runs: out.runs,
            atoms: out.atoms,
            breaks: out.breaks,
            map: out.map,
        }
    }

    /// Whether inline content is shown exactly as its IR text.
    fn is_plain(&self, inl: &Inlines) -> bool {
        let special = InlineFlags::SUP | InlineFlags::SUB | InlineFlags::KBD;
        inl.runs.iter().all(|r| {
            r.kind == RunKind::Text
                && !r.flags.intersects(special)
                && !(self.refs.enabled
                    && r.link.and_then(|l| self.doc.link(l)).is_some_and(wants_ref))
        })
    }

    /// Typeset inline math into `out`.
    fn compose_math(
        &mut self,
        tex: &str,
        base: StyleId,
        flags: InlineFlags,
        link: Option<LinkId>,
        out: &mut Out,
    ) {
        let m = self.inline_math(tex);
        let start = out.len();
        let mut pos = 0u32;
        for span in &m.spans {
            let piece = m.text.get(pos as usize..span.end as usize).unwrap_or("");
            let role = if m.ok {
                span.role
            } else {
                emde_math::MathRole::Error
            };
            let kind = Kind::Math {
                role,
                bold: span.bold,
                dim: span.dim,
            };
            let style = self.sty.inline(base, flags, kind, link.is_some());
            out.push(piece, style, link, SpanFlags::empty(), false);
            pos = span.end;
        }
        let mut prev = start;
        for &b in &m.breaks {
            let at = start + b;
            out.atom(prev..at);
            out.breaks.push(at);
            prev = at;
        }
        let end = out.len();
        out.atom(prev..end);
    }

    // ----- wrapping and emitting ---------------------------------------------

    /// Wrap composed text: first line `first` columns, the rest `rest`.
    /// Atoms wider than a line are released so they break like long words.
    pub(super) fn wrap_composed(
        &mut self,
        c: &Composed<'_>,
        first: u16,
        rest: u16,
        lines: &mut Vec<WrapLine>,
        pieces: &mut Vec<Piece>,
    ) {
        let narrowest = usize::from(first.min(rest).max(1));
        let amb = self.amb;
        let fits = |a: &Range<u32>| {
            let s = c.text.get(a.start as usize..a.end as usize).unwrap_or("");
            str_width(s, amb) <= narrowest
        };
        let atoms: Cow<'_, [Range<u32>]> = if c.atoms.iter().all(fits) {
            Cow::Borrowed(&c.atoms)
        } else {
            Cow::Owned(c.atoms.iter().filter(|a| fits(a)).cloned().collect())
        };
        let constraints = Constraints {
            atoms: &atoms,
            extra_breaks: &c.breaks,
        };
        let opts = WrapOptions {
            first: first.max(1),
            rest: rest.max(1),
            ambiguous_wide: amb,
        };
        self.wrapper.wrap_into(&c.text, constraints, opts, lines);
        split_runs(&c.runs, |r| r.end, lines, pieces);
    }

    /// Append the pieces of one wrapped line.
    pub(super) fn put_pieces(&mut self, c: &Composed<'_>, line: &WrapLine, pieces: &[Piece]) {
        let mut last = None;
        for p in pieces {
            let Some(run) = c.runs.get(p.run as usize) else {
                continue;
            };
            let text = c
                .text
                .get(p.range.start as usize..p.range.end as usize)
                .unwrap_or("");
            let text = strip_soft_hyphens(text);
            if run.pill && text.contains(NBSP) {
                let spaced = text.replace(NBSP, " ");
                self.put_flags(&spaced, run.style, run.link, run.flags);
            } else {
                self.put_flags(&text, run.style, run.link, run.flags);
            }
            last = Some(*run);
        }
        if line.hyphen
            && let Some(run) = last
        {
            self.put_flags("-", run.style, None, SpanFlags::empty());
        }
    }

    /// Wrap composed text to `width` columns and emit its lines, recording
    /// positions from `base_off`. Returns the widest line's width.
    pub(super) fn emit(&mut self, c: &Composed<'_>, width: u16, look: &Look, base_off: u32) -> u16 {
        let mut lines = std::mem::take(&mut self.scratch_lines);
        let mut pieces = std::mem::take(&mut self.scratch_pieces);
        // The hanging indent always leaves room for text.
        let hang = look.hang.min(width.saturating_sub(1));
        let rest = width - hang;
        self.wrap_composed(c, width, rest, &mut lines, &mut pieces);
        let mut widest = 0u16;
        let mut p = 0usize;
        for (li, line) in lines.iter().enumerate() {
            let start = p;
            while pieces.get(p).is_some_and(|piece| piece.line as usize == li) {
                p += 1;
            }
            self.begin();
            if look.pad > 0 {
                self.spaces(look.pad, look.pad_style);
            }
            if li > 0 && hang > 0 {
                self.spaces(hang, look.pad_style);
            }
            let before = self.cols();
            self.put_pieces(c, line, pieces.get(start..p).unwrap_or(&[]));
            widest = widest.max(self.cols() - before);
            let off = base_off.saturating_add(c.ir_offset(line.range.start));
            self.end(look.kind, look.fill, off);
        }
        self.scratch_lines = lines;
        self.scratch_pieces = pieces;
        widest
    }

    /// A paragraph of inline content in the current text style.
    pub(super) fn para(&mut self, inl: &Inlines) {
        let base = self.sty.el(self.text_el);
        self.para_styled(inl, base, &[]);
    }

    /// A paragraph on top of `base`, with an optional tail.
    pub(super) fn para_styled(&mut self, inl: &Inlines, base: StyleId, tail: &[Tail]) {
        let c = self.compose_with(inl, base, tail);
        let width = self.avail();
        let off = self.off;
        self.emit(&c, width, &Look::TEXT, off);
        self.off = off.saturating_add(to_u32(inl.text.len()));
    }

    /// Lines showing a URL in `style` (numbered reference lists), broken at
    /// URL break points.
    pub(super) fn url_lines(&mut self, url: &str, style: StyleId, link: Option<LinkId>, off: u32) {
        let text = crate::text::sanitize(url).replace(['\n', '\t'], " ");
        let mut breaks = Vec::new();
        crate::parse::url_break_points(&text, 0..text.len(), &mut breaks);
        let c = Composed {
            runs: vec![CRun {
                end: to_u32(text.len()),
                style,
                link,
                flags: SpanFlags::empty(),
                pill: false,
            }],
            text: Cow::Owned(text),
            atoms: Vec::new(),
            breaks,
            map: Vec::new(),
        };
        let width = self.avail();
        let saved = self.off;
        self.emit(&c, width, &Look::TEXT, off);
        self.off = saved;
    }

    /// A single styled run as composed text.
    pub(super) fn plain_composed(&self, text: &str, style: StyleId) -> Composed<'static> {
        let text = crate::text::sanitize(text).replace(['\n', '\t'], " ");
        Composed {
            runs: if text.is_empty() {
                Vec::new()
            } else {
                vec![CRun {
                    end: to_u32(text.len()),
                    style,
                    link: None,
                    flags: SpanFlags::empty(),
                    pill: false,
                }]
            },
            text: Cow::Owned(text),
            atoms: Vec::new(),
            breaks: Vec::new(),
            map: Vec::new(),
        }
    }

    /// Composed text with `prefix` (heading numbers and markers) in front.
    pub(super) fn prefixed<'t>(
        &self,
        prefix: &str,
        style: StyleId,
        c: Composed<'t>,
    ) -> Composed<'t> {
        if prefix.is_empty() {
            return c;
        }
        let shift = to_u32(prefix.len());
        let mut text = String::with_capacity(prefix.len() + c.text.len());
        text.push_str(prefix);
        text.push_str(&c.text);
        let mut runs = vec![CRun {
            end: shift,
            style,
            link: None,
            flags: SpanFlags::empty(),
            pill: false,
        }];
        runs.extend(c.runs.iter().map(|r| CRun {
            end: r.end.saturating_add(shift),
            ..*r
        }));
        let mut map = vec![MapEntry {
            disp: 0,
            ir: 0,
            ir_end: 0,
            verbatim: false,
        }];
        if c.map.is_empty() {
            map.push(MapEntry {
                disp: shift,
                ir: 0,
                ir_end: to_u32(c.text.len()),
                verbatim: true,
            });
        } else {
            map.extend(c.map.iter().map(|e| MapEntry {
                disp: e.disp.saturating_add(shift),
                ..*e
            }));
        }
        Composed {
            text: Cow::Owned(text),
            runs,
            atoms: c
                .atoms
                .iter()
                .map(|a| a.start.saturating_add(shift)..a.end.saturating_add(shift))
                .collect(),
            breaks: c.breaks.iter().map(|b| b.saturating_add(shift)).collect(),
            map,
        }
    }

    /// The base style of a heading level.
    pub(super) fn heading_el(level: u8) -> Element {
        match level {
            1 => Element::H1,
            2 => Element::H2,
            3 => Element::H3,
            4 => Element::H4,
            5 => Element::H5,
            _ => Element::H6,
        }
    }
}
