//! Lines to bytes.
//!
//! [`segments`] walks one layout line as a sequence of [`Piece`]s (text,
//! style, link), applying what layout left for output time: the highlight
//! overlay of code lines (by byte offset into the tab-expanded line), the
//! per-column background of gradient bars, panel padding, and [`Mark`]s —
//! styles laid over byte ranges of the layout's text, such as search
//! matches. The [`Emitter`] turns the pieces into bytes: the indent, SGR
//! transitions between downsampled styles, OSC 8 around each link fragment,
//! and a reset at the end of every styled line so styles never bleed into
//! the next line (or into `less -R`). [`super::debug`] renders the same
//! pieces as tags.
//!
//! Figures are laid out as placeholder boxes ([`LineKind::Image`] rows).
//! With an [`ImageRows`] provider ([`Emitter::with_images`]) a row shows
//! the image instead: block-glyph cells, drawn with the same SGR diffing
//! as text, or bytes a graphics protocol needs (kitty placeholder text, a
//! pixel image drawn over its reserved rows). What the layout put left of
//! the box (quote bars, list indentation, the centring pad) stays. Without
//! a provider, or when it has nothing for a row, the box is shown.

use std::ops::Range;

use crate::color::mix_oklab;
use crate::gfx::raster::RasterCell;
use crate::highlight::HlSpan;
use crate::ir::{Document, LinkId};
use crate::layout::{Fill, Layout, LineKind, Placement, Span, SpanFlags};
use crate::style::{Color, Rgb, Style, StyleId, StylePatch, Underline};
use crate::term::{Caps, ColorDepth};
use crate::text::grapheme_width;
use crate::text::width::next_grapheme_end;

use super::osc8;
use super::sgr::{self, Palette};

/// The style of a piece: interned in the layout, or computed at output
/// time (overlays, gradients).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegStyle {
    /// A style of [`Layout::styles`].
    Id(StyleId),
    /// A style made for this piece (not yet downsampled).
    Style(Style),
}

/// One piece of a line, as [`segments`] hands it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece<'a> {
    /// The text (never contains control characters).
    pub text: &'a str,
    /// Its style.
    pub style: SegStyle,
    /// The link it belongs to.
    pub link: Option<LinkId>,
    /// Where the text is in [`Layout::text`]; `None` for padding.
    pub off: Option<u32>,
}

/// A style laid over a byte range of [`Layout::text`] at output time
/// (search matches, a focused link). Marks passed together must be sorted
/// by start and must not overlap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Byte range in [`Layout::text`].
    pub range: Range<u32>,
    /// What the mark changes about the style underneath.
    pub patch: StylePatch,
}

/// What one row of a figure shows instead of its placeholder box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowContent<'a> {
    /// Block-glyph cells (a text-mode image row), written from the box's
    /// left edge with their colours downsampled like any other style.
    Cells(&'a [RasterCell]),
    /// Bytes written as they are at the box's left edge, with every
    /// attribute reset first; they must leave the attributes reset (kitty
    /// placeholder text ends with `ESC[39m`). Empty for rows that another
    /// row's bytes already drew.
    Bytes(&'a [u8]),
}

/// Supplies the image content of [`LineKind::Image`] rows.
pub trait ImageRows {
    /// What row `row` of `placement` shows; `None` keeps the layout's
    /// placeholder box (the image is not ready, or cannot be shown).
    fn row(&self, placement: &Placement, row: u16) -> Option<RowContent<'_>>;
}

/// How output is encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderConfig {
    /// Colours the terminal shows (`None`: no escape sequences at all).
    pub depth: ColorDepth,
    /// Styled underlines (`4:3`) and underline colours (`58`).
    pub styled_underline: bool,
    /// OSC 8 hyperlinks.
    pub hyperlinks: bool,
    /// Link ids are `<prefix>-<n>`; `e<pid>` by default, so links of
    /// different emde runs in one terminal never merge.
    pub link_id_prefix: String,
    /// Added to link numbers, so several documents in one output keep
    /// distinct ids.
    pub link_base: u32,
}

impl RenderConfig {
    /// Encoding for a terminal's capabilities.
    pub fn from_caps(caps: &Caps) -> RenderConfig {
        RenderConfig {
            depth: caps.color,
            styled_underline: caps.styled_underline,
            hyperlinks: caps.hyperlinks && caps.color != ColorDepth::None,
            link_id_prefix: format!("e{}", std::process::id()),
            link_base: 0,
        }
    }

    /// No escape sequences at all.
    pub fn plain() -> RenderConfig {
        RenderConfig {
            depth: ColorDepth::None,
            styled_underline: false,
            hyperlinks: false,
            link_id_prefix: "e0".into(),
            link_base: 0,
        }
    }
}

/// Column of a gradient's two-column step `col` falls in, as a colour.
fn gradient_color(from: Rgb, to: Rgb, x0: u16, x1: u16, col: u16) -> Rgb {
    let steps = (x1.saturating_sub(x0)).div_ceil(2);
    if steps <= 1 {
        return from;
    }
    let step = col.saturating_sub(x0) / 2;
    let t = f32::from(step.min(steps - 1)) / f32::from(steps - 1);
    mix_oklab(from, to, t)
}

/// A style with the highlight span's colours and attributes on top.
fn overlay(base: &Style, hl: &Style) -> Style {
    let mut s = *base;
    if hl.fg != Color::Default {
        s.fg = hl.fg;
    }
    s.attrs |= hl.attrs;
    if hl.underline != Underline::None {
        s.underline = hl.underline;
    }
    s
}

/// The largest char boundary of `s` at or below `i`.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Call `f` for each piece of line `index`.
pub fn segments(layout: &Layout, index: usize, f: &mut dyn FnMut(Piece<'_>)) {
    let Some(line) = layout.lines.get(index) else {
        return;
    };
    let spans = layout.line_spans(index);
    let gradient = match line.fill {
        Fill::Gradient { from, to, x0, x1 } => Some((from, to, x0, x1)),
        _ => None,
    };
    if let Some(g) = gradient {
        gradient_segments(layout, spans, line.cols, g, f);
        return;
    }
    let hl = match line.kind {
        LineKind::Code { block, line, byte0 } => layout
            .code
            .get(block as usize)
            .and_then(|c| c.hl.as_ref())
            .and_then(|h| h.lines.get(line as usize))
            .map(|spans| (spans.as_slice(), byte0)),
        _ => None,
    };
    let mut cursor = hl.map_or(0, |(_, b)| b);
    for span in spans {
        let text = layout.span_text(span);
        match hl {
            Some((hl_spans, _)) if span.flags.contains(SpanFlags::CODE) => {
                let base = *layout.styles.get(span.style);
                overlay_segments(text, span.off, &base, hl_spans, cursor, span.link, f);
                cursor = cursor.saturating_add(span.len);
            }
            _ => f(Piece {
                text,
                style: SegStyle::Id(span.style),
                link: span.link,
                off: Some(span.off),
            }),
        }
    }
    if let Fill::Panel { style, to_col } = line.fill
        && to_col > line.cols
    {
        spaces(to_col - line.cols, &mut |pad| {
            f(Piece {
                text: pad,
                style: SegStyle::Id(style),
                link: None,
                off: None,
            })
        });
    }
}

/// [`segments`] with `marks` laid over the text they cover.
pub fn marked_segments(
    layout: &Layout,
    index: usize,
    marks: &[Mark],
    f: &mut dyn FnMut(Piece<'_>),
) {
    if marks.is_empty() {
        segments(layout, index, f);
        return;
    }
    segments(layout, index, &mut |piece| {
        split_marks(layout, piece, marks, f)
    });
}

/// Hand out `piece`, split where marks start and end.
fn split_marks(layout: &Layout, piece: Piece<'_>, marks: &[Mark], f: &mut dyn FnMut(Piece<'_>)) {
    let Some(off) = piece.off else {
        f(piece);
        return;
    };
    let len = u32::try_from(piece.text.len()).unwrap_or(u32::MAX);
    let end = off.saturating_add(len);
    let first = marks.partition_point(|m| m.range.end <= off);
    let at = |pos: usize| Some(off.saturating_add(u32::try_from(pos).unwrap_or(u32::MAX)));
    let mut pos = 0usize; // in `piece.text`
    for mark in marks.get(first..).unwrap_or(&[]) {
        if mark.range.start >= end {
            break;
        }
        let start = floor_boundary(piece.text, mark.range.start.saturating_sub(off) as usize);
        let stop = floor_boundary(piece.text, (mark.range.end.min(end) - off) as usize);
        if start > pos {
            f(Piece {
                text: piece.text.get(pos..start).unwrap_or(""),
                off: at(pos),
                ..piece
            });
        }
        let from = start.max(pos);
        if stop > from {
            let base = match piece.style {
                SegStyle::Id(id) => *layout.styles.get(id),
                SegStyle::Style(s) => s,
            };
            f(Piece {
                text: piece.text.get(from..stop).unwrap_or(""),
                style: SegStyle::Style(base.patch(&mark.patch)),
                off: at(from),
                ..piece
            });
            pos = stop;
        }
    }
    if pos < piece.text.len() {
        f(Piece {
            text: piece.text.get(pos..).unwrap_or(""),
            off: at(pos),
            ..piece
        });
    }
}

/// `n` spaces, in pieces of a static string.
fn spaces(n: u16, f: &mut dyn FnMut(&str)) {
    const SPACES: &str = "                                                                ";
    let mut left = usize::from(n);
    while left > 0 {
        let take = left.min(SPACES.len());
        f(SPACES.get(..take).unwrap_or(""));
        left -= take;
    }
}

/// Pieces of one code span under its highlight runs; `start` is the span's
/// offset in the expanded code line, `arena` its offset in the layout text.
fn overlay_segments(
    text: &str,
    arena: u32,
    base: &Style,
    hl: &[HlSpan],
    start: u32,
    link: Option<LinkId>,
    f: &mut dyn FnMut(Piece<'_>),
) {
    let end = start.saturating_add(u32::try_from(text.len()).unwrap_or(u32::MAX));
    let mut pos = 0usize; // in `text`
    let mut run_start = 0u32; // in the line
    let piece = |from: usize, to: usize, style: Style| Piece {
        text: text.get(from..to).unwrap_or(""),
        style: SegStyle::Style(style),
        link,
        off: Some(arena.saturating_add(u32::try_from(from).unwrap_or(u32::MAX))),
    };
    for h in hl {
        let run = run_start..h.end;
        run_start = h.end;
        if run.end <= start || run.start >= end {
            continue;
        }
        let piece_end = floor_boundary(text, (run.end - start) as usize);
        if piece_end <= pos {
            continue;
        }
        f(piece(pos, piece_end, overlay(base, &h.style)));
        pos = piece_end;
    }
    if pos < text.len() {
        f(piece(pos, text.len(), *base));
    }
}

/// Pieces of a line with a gradient background: grapheme by grapheme
/// inside the gradient, then padding up to its end.
fn gradient_segments(
    layout: &Layout,
    spans: &[Span],
    cols: u16,
    (from, to, x0, x1): (Rgb, Rgb, u16, u16),
    f: &mut dyn FnMut(Piece<'_>),
) {
    let mut col = 0u16;
    for span in spans {
        let text = layout.span_text(span);
        let base = *layout.styles.get(span.style);
        let span_end = col.saturating_add(span.cols);
        if span_end <= x0 || col >= x1 {
            f(Piece {
                text,
                style: SegStyle::Id(span.style),
                link: span.link,
                off: Some(span.off),
            });
            col = span_end;
            continue;
        }
        let at = |pos: usize| {
            Some(
                span.off
                    .saturating_add(u32::try_from(pos).unwrap_or(u32::MAX)),
            )
        };
        let mut pos = 0;
        let mut piece_start = 0;
        let mut piece_bg: Option<Rgb> = None;
        while pos < text.len() {
            let end = next_grapheme_end(text, pos);
            let w = grapheme_width(text.get(pos..end).unwrap_or(""), false);
            let bg = (col >= x0 && col < x1).then(|| gradient_color(from, to, x0, x1, col));
            if bg != piece_bg && pos > piece_start {
                let piece = text.get(piece_start..pos).unwrap_or("");
                emit_bg(piece, at(piece_start), &base, piece_bg, span.link, f);
                piece_start = pos;
            }
            piece_bg = bg;
            col = col.saturating_add(u16::try_from(w).unwrap_or(0));
            pos = end;
        }
        if pos > piece_start {
            let piece = text.get(piece_start..pos).unwrap_or("");
            emit_bg(piece, at(piece_start), &base, piece_bg, span.link, f);
        }
        col = span_end.max(col);
    }
    // Padding: one piece per two-column step.
    let mut c = cols;
    while c < x1 {
        let step_end = if c < x0 {
            x0
        } else {
            let end = u32::from(x0) + (u32::from(c - x0) / 2 + 1) * 2;
            u16::try_from(end).unwrap_or(u16::MAX).min(x1)
        };
        let bg = (c >= x0).then(|| gradient_color(from, to, x0, x1, c));
        let n = step_end.saturating_sub(c).max(1);
        spaces(n, &mut |pad| emit_bg(pad, None, &Style::PLAIN, bg, None, f));
        c = c.saturating_add(n);
    }
}

fn emit_bg(
    text: &str,
    off: Option<u32>,
    base: &Style,
    bg: Option<Rgb>,
    link: Option<LinkId>,
    f: &mut dyn FnMut(Piece<'_>),
) {
    let style = match bg {
        Some(rgb) => base.on(Color::Rgb(rgb)),
        None => *base,
    };
    f(Piece {
        text,
        style: SegStyle::Style(style),
        link,
        off,
    });
}

/// Writes layout lines as bytes; see the module docs.
pub struct Emitter<'a> {
    doc: &'a Document,
    layout: &'a Layout,
    cfg: &'a RenderConfig,
    palette: Palette,
    /// The layout's styles, downsampled.
    styles: Vec<Style>,
    /// OSC 8 URL of each link, computed on first use.
    urls: Vec<Option<Option<String>>>,
    /// Image content for figure rows.
    images: Option<&'a dyn ImageRows>,
}

/// The terminal state while a line is written.
struct Pen {
    style: Style,
    link: Option<LinkId>,
}

impl<'a> Emitter<'a> {
    /// An emitter for one laid-out document.
    pub fn new(doc: &'a Document, layout: &'a Layout, cfg: &'a RenderConfig) -> Emitter<'a> {
        let mut palette = Palette::new(cfg.depth, cfg.styled_underline);
        let styles: Vec<Style> = layout
            .styles
            .styles()
            .iter()
            .map(|s| palette.style(s))
            .collect();
        Emitter {
            doc,
            layout,
            cfg,
            palette,
            styles,
            urls: vec![None; doc.links.len()],
            images: None,
        }
    }

    /// Show figures with the images `images` provides instead of their
    /// placeholder boxes.
    pub fn with_images(mut self, images: &'a dyn ImageRows) -> Emitter<'a> {
        self.images = Some(images);
        self
    }

    /// Whether a link gets an OSC 8 URL (computed once per link).
    fn linked(&mut self, link: LinkId) -> bool {
        if !self.cfg.hyperlinks {
            return false;
        }
        let doc = self.doc;
        match self.urls.get_mut(link.index()) {
            Some(slot) => slot
                .get_or_insert_with(|| doc.link(link).and_then(osc8::link_url))
                .is_some(),
            None => false,
        }
    }

    fn url(&self, link: LinkId) -> &str {
        self.urls
            .get(link.index())
            .and_then(|u| u.as_ref())
            .and_then(|u| u.as_deref())
            .unwrap_or("")
    }

    /// Append one piece.
    fn piece(&mut self, out: &mut Vec<u8>, pen: &mut Pen, piece: Piece<'_>) {
        let Piece {
            text, style, link, ..
        } = piece;
        let s = match style {
            SegStyle::Id(id) => self
                .styles
                .get(usize::from(id.0))
                .copied()
                .unwrap_or(Style::PLAIN),
            SegStyle::Style(s) => self.palette.style(&s),
        };
        let target = link.filter(|&l| self.linked(l));
        if target != pen.link {
            if pen.link.is_some() {
                osc8::close(out);
            }
            if let Some(l) = target {
                let n = self.cfg.link_base.saturating_add(l.0);
                osc8::open(out, &self.cfg.link_id_prefix, n, self.url(l));
            }
            pen.link = target;
        }
        sgr::write_transition(&pen.style, &s, out);
        pen.style = s;
        out.extend_from_slice(text.as_bytes());
    }

    /// Append line `index` (with its newline) to `out`.
    pub fn write_line(&mut self, index: usize, out: &mut Vec<u8>) {
        self.write_line_marked(index, &[], out);
    }

    /// [`Emitter::write_line`] with `marks` (search matches, …) laid over
    /// the text.
    pub fn write_line_marked(&mut self, index: usize, marks: &[Mark], out: &mut Vec<u8>) {
        let layout = self.layout;
        let Some(line) = layout.lines.get(index) else {
            return;
        };
        if let Some((placement, content)) = self.image_row(line.kind) {
            self.write_image_row(index, &placement, content, out);
            return;
        }
        if line.n == 0 && matches!(line.fill, Fill::None) {
            out.push(b'\n');
            return;
        }
        out.resize(out.len() + usize::from(layout.indent), b' ');
        let mut pen = Pen {
            style: Style::PLAIN,
            link: None,
        };
        marked_segments(layout, index, marks, &mut |piece| {
            self.piece(out, &mut pen, piece);
        });
        if pen.link.is_some() {
            osc8::close(out);
        }
        if pen.style != Style::PLAIN {
            out.extend_from_slice(sgr::RESET);
        }
        out.push(b'\n');
    }

    /// The image content of a figure row, when the provider has some.
    fn image_row(&self, kind: LineKind) -> Option<(Placement, RowContent<'a>)> {
        let LineKind::Image { placement, row } = kind else {
            return None;
        };
        let placement = *self.layout.images.get(placement as usize)?;
        let content = self.images?.row(&placement, row)?;
        Some((placement, content))
    }

    /// Append a figure row showing `content`: what the layout put left of
    /// the box, then the image from the box's left edge.
    fn write_image_row(
        &mut self,
        index: usize,
        placement: &Placement,
        content: RowContent<'_>,
        out: &mut Vec<u8>,
    ) {
        let layout = self.layout;
        out.resize(out.len() + usize::from(layout.indent), b' ');
        let mut pen = Pen {
            style: Style::PLAIN,
            link: None,
        };
        let mut col = 0u16;
        for span in layout.line_spans(index) {
            let end = col.saturating_add(span.cols);
            if end > placement.col {
                break;
            }
            let piece = Piece {
                text: layout.span_text(span),
                style: SegStyle::Id(span.style),
                link: span.link,
                off: Some(span.off),
            };
            self.piece(out, &mut pen, piece);
            col = end;
        }
        spaces(placement.col.saturating_sub(col), &mut |pad| {
            let piece = Piece {
                text: pad,
                style: SegStyle::Id(StyleId(0)),
                link: None,
                off: None,
            };
            self.piece(out, &mut pen, piece);
        });
        match content {
            RowContent::Cells(cells) => {
                // A linked figure stays a link: its cells are clickable.
                let link = self.doc.image(placement.image).and_then(|i| i.link);
                let mut buf = [0u8; 4];
                for cell in cells {
                    let style = Style {
                        fg: cell.fg,
                        bg: cell.bg,
                        ..Style::PLAIN
                    };
                    let piece = Piece {
                        text: cell.ch.encode_utf8(&mut buf),
                        style: SegStyle::Style(style),
                        link,
                        off: None,
                    };
                    self.piece(out, &mut pen, piece);
                }
            }
            RowContent::Bytes(bytes) => {
                if pen.link.take().is_some() {
                    osc8::close(out);
                }
                sgr::write_transition(&pen.style, &Style::PLAIN, out);
                pen.style = Style::PLAIN;
                out.extend_from_slice(bytes);
            }
        }
        if pen.link.is_some() {
            osc8::close(out);
        }
        if pen.style != Style::PLAIN {
            out.extend_from_slice(sgr::RESET);
        }
        out.push(b'\n');
    }

    /// Append every line to `out`.
    pub fn write_all(&mut self, out: &mut Vec<u8>) {
        for i in 0..self.layout.lines.len() {
            self.write_line(i, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::PlainHighlighter;
    use crate::layout::{NoImages, layout};
    use crate::options::RenderOptions;
    use crate::parse::{ParseOptions, parse};
    use crate::style::Attrs;
    use crate::theme::Theme;

    fn lay(md: &str, width: u16, caps: &Caps) -> (Document, Layout) {
        let doc = parse(md, &ParseOptions::default());
        let opts = RenderOptions::default();
        let l = layout(
            &doc,
            width,
            &Theme::test(),
            caps,
            &opts,
            &PlainHighlighter,
            &NoImages,
        );
        (doc, l)
    }

    fn pieces(l: &Layout, i: usize, marks: &[Mark]) -> Vec<(String, Option<u32>)> {
        let mut out = Vec::new();
        marked_segments(l, i, marks, &mut |p| out.push((p.text.to_string(), p.off)));
        out
    }

    fn cfg(caps: &Caps) -> RenderConfig {
        RenderConfig {
            link_id_prefix: "t".into(),
            ..RenderConfig::from_caps(caps)
        }
    }

    #[test]
    fn pieces_carry_arena_offsets() {
        let (_, l) = lay("hello *world*", 40, &Caps::plain());
        let p = pieces(&l, 0, &[]);
        assert_eq!(p, [("hello ".into(), Some(0)), ("world".into(), Some(6))]);
    }

    #[test]
    fn marks_split_pieces_and_patch_styles() {
        let (_, l) = lay("hello world, again", 40, &Caps::full());
        let patch = StylePatch {
            set: Attrs::REVERSE,
            ..StylePatch::default()
        };
        let marks = [
            Mark {
                range: 6..11,
                patch,
            },
            Mark {
                range: 13..15,
                patch,
            },
        ];
        let texts: Vec<String> = pieces(&l, 0, &marks).into_iter().map(|p| p.0).collect();
        assert_eq!(texts, ["hello ", "world", ", ", "ag", "ain"]);
        let mut styles = Vec::new();
        marked_segments(&l, 0, &marks, &mut |p| styles.push(p.style));
        assert!(matches!(styles[1], SegStyle::Style(s) if s.attrs.contains(Attrs::REVERSE)));
        assert!(matches!(styles[0], SegStyle::Id(_)));
        // A mark across a line break only covers this line's part.
        let (_, l) = lay("aaa bbb", 3, &Caps::plain());
        let m = [Mark { range: 1..6, patch }];
        let first: Vec<String> = pieces(&l, 0, &m).into_iter().map(|p| p.0).collect();
        assert_eq!(first, ["a", "aa"]);
    }

    #[test]
    fn panels_pad_with_spaces_not_erase() {
        let (doc, l) = lay("```\nx\n```", 20, &Caps::full());
        let bytes = super::super::to_bytes(&doc, &l, &cfg(&Caps::full()));
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("\u{1b}[K"), "no erase-line");
        let widest = text
            .lines()
            .map(|line| {
                let mut plain = String::new();
                let mut esc = false;
                for c in line.chars() {
                    match (esc, c) {
                        (false, '\u{1b}') => esc = true,
                        (true, 'm') => esc = false,
                        (false, c) => plain.push(c),
                        _ => {}
                    }
                }
                plain.chars().count()
            })
            .max()
            .unwrap();
        assert_eq!(widest, 20, "indent 2 + a 16-column panel + margin");
    }

    #[test]
    fn links_are_opened_and_closed_per_line() {
        let caps = Caps::full();
        let (doc, l) = lay("[a long link](https://example.com)", 8, &caps);
        let bytes = super::super::to_bytes(&doc, &l, &cfg(&caps));
        let text = String::from_utf8(bytes).unwrap();
        for line in text.lines() {
            assert_eq!(line.matches("\u{1b}]8;id=t-0;").count(), 1, "{line:?}");
            assert_eq!(line.matches("\u{1b}]8;;\u{1b}\\").count(), 1, "{line:?}");
            assert!(line.ends_with("\u{1b}[0m"), "{line:?}");
        }
        // Without hyperlinks the text is only styled.
        let off = Caps {
            hyperlinks: false,
            ..caps
        };
        let text = String::from_utf8(super::super::to_bytes(&doc, &l, &cfg(&off))).unwrap();
        assert!(!text.contains("\u{1b}]8"));
    }

    #[test]
    fn gradient_pieces_step_every_two_columns() {
        let (_, l) = lay("# Ab", 12, &Caps::full());
        let mut bgs = Vec::new();
        segments(&l, 0, &mut |p| {
            if let SegStyle::Style(s) = p.style {
                bgs.push((p.text.to_string(), s.bg));
            }
        });
        let texts: String = bgs.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            texts.chars().count(),
            12,
            "the whole 12-column bar: {bgs:?}"
        );
        // Columns 0-1 share a colour, column 2 starts the next step.
        assert_eq!(bgs[0].0, " A");
        assert_ne!(bgs[0].1, bgs[1].1);
        assert_eq!(
            gradient_color(Rgb(0, 0, 0), Rgb(255, 255, 255), 0, 1, 0),
            Rgb(0, 0, 0)
        );
    }

    #[test]
    fn plain_output_is_the_text() {
        let (doc, l) = lay("# T\n\n- a\n\n> b", 30, &Caps::plain());
        assert_eq!(
            super::super::plain_text(&doc, &l),
            "T\n══════════════════════════\n\n• a\n\n▎ b\n"
        );
    }

    /// Figures 4 × 2 cells.
    struct FourByTwo;

    impl crate::layout::ImageSizer for FourByTwo {
        fn cells(&self, _: crate::ir::ImageId, _: u16, _: u16) -> Option<(u16, u16)> {
            Some((4, 2))
        }
    }

    /// Image rows: red-over-blue cells in row 0 (the last cell different),
    /// bytes in row 1.
    struct Rows(Vec<RasterCell>);

    impl Rows {
        fn new() -> Rows {
            let red_blue = RasterCell {
                ch: '▀',
                fg: Color::Rgb(Rgb(255, 0, 0)),
                bg: Color::Rgb(Rgb(0, 0, 255)),
            };
            let green = RasterCell {
                ch: '▄',
                fg: Color::Rgb(Rgb(0, 255, 0)),
                bg: Color::Default,
            };
            Rows(vec![red_blue, red_blue, red_blue, green])
        }
    }

    impl ImageRows for Rows {
        fn row(&self, _: &Placement, row: u16) -> Option<RowContent<'_>> {
            match row {
                0 => Some(RowContent::Cells(&self.0)),
                1 => Some(RowContent::Bytes(b"<image bytes>")),
                _ => None,
            }
        }
    }

    fn figure(md: &str, caps: &Caps) -> (Document, Layout) {
        let doc = parse(md, &ParseOptions::default());
        let l = layout(
            &doc,
            20,
            &Theme::test(),
            caps,
            &RenderOptions::default(),
            &PlainHighlighter,
            &FourByTwo,
        );
        (doc, l)
    }

    fn emitted(doc: &Document, l: &Layout, caps: &Caps, images: Option<&dyn ImageRows>) -> String {
        let cfg = cfg(caps);
        let mut e = Emitter::new(doc, l, &cfg);
        if let Some(images) = images {
            e = e.with_images(images);
        }
        let mut out = Vec::new();
        e.write_all(&mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn image_rows_without_a_provider_show_the_box() {
        let caps = Caps::full();
        let (doc, l) = figure("![alt](a.png)", &caps);
        assert_eq!(l.images.len(), 1);
        let out = emitted(&doc, &l, &caps, None);
        assert!(out.contains("▣"), "{out:?}");
        assert!(!out.contains("<image bytes>"));
    }

    #[test]
    fn image_rows_show_cells_and_bytes_at_the_box() {
        let caps = Caps::full();
        let (doc, l) = figure("![alt](a.png)", &caps);
        let rows = Rows::new();
        let out = emitted(&doc, &l, &caps, Some(&rows));
        let lines: Vec<&str> = out.lines().collect();
        // Indent 2, then the box centred in the 16-column measure at 6.
        let pad = " ".repeat(usize::from(l.indent + l.images[0].col));
        assert_eq!(
            lines[0],
            format!("{pad}\x1b[38;2;255;0;0;48;2;0;0;255m▀▀▀\x1b[0;38;2;0;255;0m▄\x1b[0m"),
            "one SGR for the run of equal cells, a reset when the background goes"
        );
        assert_eq!(lines[1], format!("{pad}<image bytes>"));
        // Plain output gets the glyphs without escapes.
        let plain = Caps::plain();
        let (doc, l) = figure("![alt](a.png)", &plain);
        let out = emitted(&doc, &l, &plain, Some(&rows));
        let pad = " ".repeat(usize::from(l.indent + l.images[0].col));
        assert!(out.starts_with(&format!("{pad}▀▀▀▄\n")), "{out:?}");
    }

    #[test]
    fn image_rows_keep_quote_bars_and_links() {
        let caps = Caps::full();
        let (doc, l) = figure("> [![alt](a.png)](https://example.com)", &caps);
        let rows = Rows::new();
        let out = emitted(&doc, &l, &caps, Some(&rows));
        let first = out.lines().next().unwrap();
        assert!(first.contains("▎"), "the quote bar stays: {first:?}");
        // A linked figure's cells are a link, closed before the line ends.
        assert!(
            first.contains("\x1b]8;id=t-0;https://example.com\x1b\\"),
            "{first:?}"
        );
        assert!(first.ends_with("\x1b]8;;\x1b\\\x1b[0m"), "{first:?}");
        // Bytes rows are written with every attribute reset first.
        let second = out.lines().nth(1).unwrap();
        let bytes_at = second.find("<image bytes>").unwrap();
        let before = &second[..bytes_at];
        let after_reset = before.rsplit("\x1b[0m").next().unwrap_or(before);
        assert!(before.contains("▎\x1b[0m"), "{second:?}");
        assert!(after_reset.chars().all(|c| c == ' '), "{second:?}");
    }
}
