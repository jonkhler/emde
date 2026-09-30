//! Lines to bytes.
//!
//! [`segments`] walks one layout line as a sequence of `(text, style,
//! link)` pieces, applying what layout left for output time: the highlight
//! overlay of code lines (by byte offset into the tab-expanded line), the
//! per-column background of gradient bars, and panel padding. The
//! [`Emitter`] turns the pieces into bytes: the indent, SGR transitions
//! between downsampled styles, OSC 8 around each link fragment, and a reset
//! at the end of every styled line so styles never bleed into the next line
//! (or into `less -R`). [`super::debug`] renders the same pieces as tags.

use crate::color::mix_oklab;
use crate::highlight::HlSpan;
use crate::ir::{Document, LinkId};
use crate::layout::{Fill, Layout, LineKind, Span, SpanFlags};
use crate::style::{Color, Rgb, Style, StyleId, Underline};
use crate::term::{Caps, ColorDepth};
use crate::text::grapheme_width;
use crate::text::width::next_grapheme_end;

use super::osc8;
use super::sgr::{self, Palette};

/// The style of a piece: interned in the layout, or computed at output
/// time (overlays, gradients).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegStyle {
    Id(StyleId),
    Style(Style),
}

/// How output is encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderConfig {
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

/// Call `f` for each piece of line `index`: its text, style and link.
pub fn segments(layout: &Layout, index: usize, f: &mut dyn FnMut(&str, SegStyle, Option<LinkId>)) {
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
                overlay_segments(text, &base, hl_spans, cursor, span.link, f);
                cursor = cursor.saturating_add(span.len);
            }
            _ => f(text, SegStyle::Id(span.style), span.link),
        }
    }
    if let Fill::Panel { style, to_col } = line.fill
        && to_col > line.cols
    {
        spaces(to_col - line.cols, &mut |pad| {
            f(pad, SegStyle::Id(style), None)
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
/// offset in the expanded code line.
fn overlay_segments(
    text: &str,
    base: &Style,
    hl: &[HlSpan],
    start: u32,
    link: Option<LinkId>,
    f: &mut dyn FnMut(&str, SegStyle, Option<LinkId>),
) {
    let end = start.saturating_add(u32::try_from(text.len()).unwrap_or(u32::MAX));
    let mut pos = 0usize; // in `text`
    let mut run_start = 0u32; // in the line
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
        let piece = text.get(pos..piece_end).unwrap_or("");
        f(piece, SegStyle::Style(overlay(base, &h.style)), link);
        pos = piece_end;
    }
    if pos < text.len() {
        f(text.get(pos..).unwrap_or(""), SegStyle::Style(*base), link);
    }
}

/// Pieces of a line with a gradient background: grapheme by grapheme
/// inside the gradient, then padding up to its end.
fn gradient_segments(
    layout: &Layout,
    spans: &[Span],
    cols: u16,
    (from, to, x0, x1): (Rgb, Rgb, u16, u16),
    f: &mut dyn FnMut(&str, SegStyle, Option<LinkId>),
) {
    let mut col = 0u16;
    for span in spans {
        let text = layout.span_text(span);
        let base = *layout.styles.get(span.style);
        let span_end = col.saturating_add(span.cols);
        if span_end <= x0 || col >= x1 {
            f(text, SegStyle::Id(span.style), span.link);
            col = span_end;
            continue;
        }
        let mut pos = 0;
        let mut piece_start = 0;
        let mut piece_bg: Option<Rgb> = None;
        while pos < text.len() {
            let end = next_grapheme_end(text, pos);
            let w = grapheme_width(text.get(pos..end).unwrap_or(""), false);
            let bg = (col >= x0 && col < x1).then(|| gradient_color(from, to, x0, x1, col));
            if bg != piece_bg && pos > piece_start {
                emit_bg(
                    text.get(piece_start..pos).unwrap_or(""),
                    &base,
                    piece_bg,
                    span.link,
                    f,
                );
                piece_start = pos;
            }
            piece_bg = bg;
            col = col.saturating_add(u16::try_from(w).unwrap_or(0));
            pos = end;
        }
        if pos > piece_start {
            emit_bg(
                text.get(piece_start..pos).unwrap_or(""),
                &base,
                piece_bg,
                span.link,
                f,
            );
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
        spaces(n, &mut |pad| emit_bg(pad, &Style::PLAIN, bg, None, f));
        c = c.saturating_add(n);
    }
}

fn emit_bg(
    text: &str,
    base: &Style,
    bg: Option<Rgb>,
    link: Option<LinkId>,
    f: &mut dyn FnMut(&str, SegStyle, Option<LinkId>),
) {
    let style = match bg {
        Some(rgb) => base.on(Color::Rgb(rgb)),
        None => *base,
    };
    f(text, SegStyle::Style(style), link);
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
        let styles = layout
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
        }
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
    fn piece(
        &mut self,
        out: &mut Vec<u8>,
        pen: &mut Pen,
        text: &str,
        style: SegStyle,
        link: Option<LinkId>,
    ) {
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
        let layout = self.layout;
        let Some(line) = layout.lines.get(index) else {
            return;
        };
        if line.n == 0 && matches!(line.fill, Fill::None) {
            out.push(b'\n');
            return;
        }
        out.resize(out.len() + usize::from(layout.indent), b' ');
        let mut pen = Pen {
            style: Style::PLAIN,
            link: None,
        };
        segments(layout, index, &mut |text, style, link| {
            self.piece(out, &mut pen, text, style, link);
        });
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
