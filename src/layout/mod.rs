//! Width-dependent layout: a [`Document`] becomes styled lines for one
//! terminal width.
//!
//! [`layout`] is a pure function of the document, the width, the theme, the
//! terminal capabilities, the render options, a [`Highlighter`] and an
//! [`ImageSizer`]. Its result, a [`Layout`], is a structure of arrays:
//!
//! * one text arena ([`Layout::text`]) holding every piece of text shown;
//! * [`Span`]s pointing into the arena, each with an interned style
//!   ([`Layout::styles`]) and an optional link;
//! * [`Line`]s pointing at runs of spans, each with a [`LineKind`], a
//!   background [`Fill`] and a width-independent [`SrcPos`];
//! * side tables: the lines of each top-level block, the first line of each
//!   heading, link hit boxes, image placements and the highlighting of each
//!   code block.
//!
//! Spans are positioned relative to the *text column*: the measure is
//! `min(max_width, width − 2·margin)` columns wide, centred on a terminal
//! with [`Align::Center`], and the emitter
//! puts [`Layout::indent`] spaces before every line. Every line fits the
//! measure; nothing a document contains can make a line wider.
//!
//! Colour-depth decisions (gradients, panels versus frames, tints, zebra
//! stripes, pills) are made here, from [`Caps::color`]; colours themselves
//! stay as the theme gives them and are downsampled when bytes are written
//! ([`crate::render`]). Syntax highlighting is an *overlay*: code lines keep
//! their plain text, and [`CodeInfo`] holds the highlight runs the emitter
//! paints by byte offset.
//!
//! The work is split by concern: `build` holds the line and prefix machinery,
//! `inline` turns inline content into wrapped lines, `blocks` lays out the
//! block elements, and `code`, `table`, `figure` and `math` the elements with
//! geometry of their own. `deco` resolves decoration glyphs (with ASCII
//! fallbacks), `style` resolves element styles and `scripts` maps
//! super- and subscripts to Unicode.

mod blocks;
mod build;
mod code;
mod deco;
mod figure;
mod inline;
mod math;
mod scripts;
mod style;
mod table;

use std::ops::Range;

use bitflags::bitflags;

use crate::highlight::{Highlighter, HlBlock};
use crate::ir::{Document, ImageId, LinkId, SrcPos};
use crate::options::{Align, RenderOptions};
use crate::style::{Rgb, StyleId, StyleTable};
use crate::term::Caps;
use crate::theme::Theme;

pub use table::allocate_columns;

/// Narrowest text column worth keeping margins for: on narrower terminals
/// the margins shrink first.
const MIN_MEASURE: u16 = 20;

/// A laid-out document; see the module docs.
#[derive(Clone, Debug)]
pub struct Layout {
    /// Total width in columns the layout was made for.
    pub width: u16,
    /// Width of the text column.
    pub measure: u16,
    /// Spaces the emitter puts before every non-blank line (centring and
    /// margins).
    pub indent: u16,
    /// Text of every span, concatenated. Never contains control characters
    /// or soft hyphens.
    pub text: String,
    /// All spans; each line owns a contiguous run of them.
    pub spans: Vec<Span>,
    /// The lines, top to bottom.
    pub lines: Vec<Line>,
    /// The lines of each top-level block (indexed like
    /// [`Document::blocks`]). The ranges tile [`Layout::lines`]: the blank
    /// line after a block belongs to it.
    pub block_lines: Vec<Range<u32>>,
    /// First line of each outline heading (indexed like
    /// [`Document::headings`]); `u32::MAX` if the heading was not shown.
    pub heading_line: Vec<u32>,
    /// Where links are on screen, one entry per fragment per line.
    pub link_hits: Vec<LinkHit>,
    /// Where figures go.
    pub images: Vec<Placement>,
    /// Highlighting of each code block, indexed by `block` in
    /// [`LineKind::Code`].
    pub code: Vec<CodeInfo>,
    /// The styles spans and fills refer to.
    pub styles: StyleTable,
}

/// One line of output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    /// Index of the line's first span in [`Layout::spans`].
    pub first: u32,
    /// Number of spans.
    pub n: u32,
    /// Columns covered by the spans (relative to the text column).
    pub cols: u16,
    /// What the line shows.
    pub kind: LineKind,
    /// Background drawn behind and after the spans.
    pub fill: Fill,
    /// Where the line's content comes from (never decreases down the
    /// layout).
    pub pos: SrcPos,
}

/// What a line shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// Prose, headings, decorations.
    Text,
    /// A blank line between blocks (it may still carry quote bars).
    Blank,
    /// A row of a code block. Spans flagged [`SpanFlags::CODE`] hold the
    /// code text, starting at byte `byte0` of line `line` (tabs expanded)
    /// of code block `block` ([`Layout::code`]); the emitter paints the
    /// block's highlight runs over them.
    Code { block: u32, line: u32, byte0: u32 },
    /// A row of display math.
    Math,
    /// A table row or border.
    Table,
    /// Row `row` of the placeholder box of image placement `placement`
    /// ([`Layout::images`]).
    Image { placement: u32, row: u16 },
}

/// A piece of text with one style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the text in [`Layout::text`].
    pub off: u32,
    /// Byte length of the text.
    pub len: u32,
    /// Display width of the text.
    pub cols: u16,
    /// Style in [`Layout::styles`].
    pub style: StyleId,
    /// The link the text belongs to.
    pub link: Option<LinkId>,
    /// What else the span is.
    pub flags: SpanFlags,
}

bitflags! {
    /// Extra facts about a [`Span`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub struct SpanFlags: u8 {
        /// Code text under the highlight overlay (see [`LineKind::Code`]).
        const CODE = 1 << 0;
        /// A footnote back-link (`↑`): following it goes to where its link
        /// (the footnote reference) is shown.
        const BACKLINK = 1 << 1;
    }
}

/// Background of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// Nothing after the last span.
    None,
    /// Spaces in `style` from the end of the spans up to column `to_col`
    /// (code panels, alert tints, bars). Backgrounds are always padded
    /// with spaces, never with erase-line.
    Panel { style: StyleId, to_col: u16 },
    /// A background gradient over columns `x0..x1` in two-column steps,
    /// interpolated in OKLab from `from` to `to`. It replaces the spans'
    /// own background there, and spaces pad the spans up to `x1`.
    Gradient {
        from: Rgb,
        to: Rgb,
        x0: u16,
        x1: u16,
    },
}

/// A link fragment on one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkHit {
    /// The line.
    pub line: u32,
    /// Columns covered (relative to the text column).
    pub cols: Range<u16>,
    /// The link.
    pub link: LinkId,
    /// A footnote back-link: it leads to where `link` is shown.
    pub back: bool,
}

/// Where a figure's image goes: a box of exactly its final size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The image.
    pub image: ImageId,
    /// First line of the box.
    pub line: u32,
    /// Column of the box's left edge (relative to the text column).
    pub col: u16,
    /// Width in cells.
    pub cols: u16,
    /// Height in cells (one [`LineKind::Image`] line per row).
    pub rows: u16,
}

/// The highlighting of one code block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeInfo {
    /// Highlight runs per source line, with byte offsets into the line
    /// *after* tab expansion; `None` when the block is not highlighted.
    pub hl: Option<HlBlock>,
}

/// Sizes images from their header dimensions; layout never decodes.
pub trait ImageSizer {
    /// The size in cells a figure of `image` should take, at most
    /// `max_cols` × `max_rows`; `None` when the image cannot be shown (not
    /// found, not an image, images off).
    fn cells(&self, image: ImageId, max_cols: u16, max_rows: u16) -> Option<(u16, u16)>;
}

/// An [`ImageSizer`] that knows no images: figures become one-line alt
/// text boxes.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoImages;

impl ImageSizer for NoImages {
    fn cells(&self, _image: ImageId, _max_cols: u16, _max_rows: u16) -> Option<(u16, u16)> {
        None
    }
}

/// Lay out a document for a terminal `width` columns wide.
///
/// Total: any document and any width (0 is treated as 1) give a layout
/// whose lines all fit the measure.
pub fn layout(
    doc: &Document,
    width: u16,
    theme: &Theme,
    caps: &Caps,
    opts: &RenderOptions,
    highlighter: &dyn Highlighter,
    sizer: &dyn ImageSizer,
) -> Layout {
    let (indent, measure) = geometry(width, opts, caps);
    let inputs = build::Inputs {
        doc,
        theme,
        caps,
        opts,
        highlighter,
        sizer,
    };
    build::Builder::new(inputs, width.max(1), indent, measure).run()
}

/// The text column for a terminal `width` columns wide: `(indent,
/// measure)`.
///
/// The measure is `min(max_width, width − 2·margin)` (`max_width` 0 means
/// no cap); the margins shrink on terminals narrower than 20 columns plus
/// the margins. On a terminal the column is centred with [`Align::Center`]
/// and starts at the margin with [`Align::Left`]; other output starts at
/// column 0.
pub fn geometry(width: u16, opts: &RenderOptions, caps: &Caps) -> (u16, u16) {
    let cols = width.max(1);
    let margin = opts.margin.min(cols.saturating_sub(MIN_MEASURE) / 2);
    let mut measure = cols.saturating_sub(margin.saturating_mul(2)).max(1);
    if opts.max_width > 0 {
        measure = measure.min(opts.max_width);
    }
    let indent = if !caps.is_tty {
        0
    } else {
        match opts.align {
            Align::Center => (cols - measure) / 2,
            Align::Left => margin,
        }
    };
    (indent, measure)
}

impl Layout {
    /// The spans of line `i` (empty if out of range).
    pub fn line_spans(&self, i: usize) -> &[Span] {
        self.lines.get(i).map_or(&[], |l| {
            let start = l.first as usize;
            self.spans
                .get(start..start.saturating_add(l.n as usize))
                .unwrap_or(&[])
        })
    }

    /// The text of a span (empty if out of range).
    pub fn span_text(&self, span: &Span) -> &str {
        let start = span.off as usize;
        self.text
            .get(start..start.saturating_add(span.len as usize))
            .unwrap_or("")
    }

    /// The plain text of line `i`, without the indent.
    pub fn line_text(&self, i: usize) -> String {
        self.line_spans(i)
            .iter()
            .map(|s| self.span_text(s))
            .collect()
    }

    /// Number of lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether there are no lines.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The line showing `pos` (for re-anchoring the view after a resize or
    /// reload): the first of the lines with the latest position at or before
    /// `pos`; 0 when every line is after it.
    pub fn line_at(&self, pos: SrcPos) -> usize {
        let last = self.lines.partition_point(|l| l.pos <= pos);
        let Some(found) = last.checked_sub(1).and_then(|i| self.lines.get(i)) else {
            return 0;
        };
        let at = found.pos;
        self.lines.partition_point(|l| l.pos < at)
    }

    /// A readable dump of every line (kind, position, width, fill and text),
    /// used by `--dump lines`.
    pub fn dump(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            let kind = match line.kind {
                LineKind::Text => "text".to_string(),
                LineKind::Blank => "blank".to_string(),
                LineKind::Code { block, line, byte0 } => format!("code {block}:{line}+{byte0}"),
                LineKind::Math => "math".to_string(),
                LineKind::Table => "table".to_string(),
                LineKind::Image { placement, row } => format!("image {placement}:{row}"),
            };
            let fill = match line.fill {
                Fill::None => String::new(),
                Fill::Panel { style, to_col } => format!(" panel s{}→{to_col}", style.0),
                Fill::Gradient { x0, x1, .. } => format!(" gradient {x0}..{x1}"),
            };
            let _ = writeln!(
                out,
                "{i:>5} {kind:<14} {:>3}:{:<6} {:>3}c{fill} |{}|",
                line.pos.top,
                line.pos.off,
                line.cols,
                self.line_text(i)
            );
        }
        out
    }
}

#[cfg(test)]
mod tests;
