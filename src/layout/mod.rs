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
//! # The pager's layouts
//!
//! [`layout_with`] takes what the pager changes about a layout
//! ([`Interactive`]): which `<details>` are collapsed, and whether wide
//! blocks keep their natural width. A collapsed `<details>` shows only its
//! summary line (`▸ Summary (42 lines)`), but its body is still counted
//! for source positions, so positions stay the same whichever blocks are
//! open ([`Layout::details`] says where each one is). A wide block (code
//! that is not wrapped, a table too wide for its columns) is laid out at
//! its natural width instead of being cut or shown as records: its lines
//! are wider than the measure, and [`Layout::wide`] tells the pager which
//! columns it may shift sideways. Stream mode ([`layout`]) has neither.
//!
//! The work is split by concern: `build` holds the line and prefix machinery,
//! `inline` turns inline content into wrapped lines, `blocks` lays out the
//! block elements, and `code`, `table`, `figure` and `math` the elements with
//! geometry of their own. `deco` resolves decoration glyphs (with ASCII
//! fallbacks), `style` resolves element styles and `scripts` maps
//! super- and subscripts to Unicode.

pub(crate) mod blocks;
mod build;
mod code;
mod deco;
mod figure;
mod inline;
mod math;
mod scripts;
mod style;
mod table;

use std::mem;
use std::ops::Range;

use bitflags::bitflags;

use crate::highlight::{Highlighter, HlBlock};
use crate::ir::{Document, HeadingId, ImageId, LinkId, SrcPos};
use crate::options::{Align, Height, RenderOptions};
use crate::style::{Rgb, StyleId, StyleTable};
use crate::term::Caps;
use crate::theme::Theme;

pub use table::allocate_columns;

/// Narrowest text column worth keeping margins for: on narrower terminals
/// the margins shrink first.
const MIN_MEASURE: u16 = 20;

/// Figure height cap when the screen height is unknown.
const DEFAULT_MAX_ROWS: u16 = 30;

/// Widest a wide block is laid out (columns); longer code lines are cut.
pub const MAX_WIDE: u16 = 2048;

/// The tallest figure `opts.images.max_height` allows on a terminal of
/// `caps.size`.
pub fn max_image_rows(opts: &RenderOptions, caps: &Caps) -> u16 {
    match opts.images.max_height {
        Height::Rows(n) => n.max(1),
        Height::Percent(p) => match caps.size {
            Some((_, rows)) => {
                let r = u32::from(rows) * u32::from(p.min(100)) / 100;
                u16::try_from(r).unwrap_or(u16::MAX).max(1)
            }
            None => DEFAULT_MAX_ROWS,
        },
    }
}

/// A laid-out document; see the module docs. The default is an empty
/// layout (no lines, zero width).
#[derive(Clone, Debug, Default, PartialEq)]
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
    /// Where inline math is on screen, one entry per formula per line.
    pub math_hits: Vec<MathHit>,
    /// Where figures go.
    pub images: Vec<Placement>,
    /// Highlighting of each code block, indexed by `block` in
    /// [`LineKind::Code`].
    pub code: Vec<CodeInfo>,
    /// The styles spans and fills refer to.
    pub styles: StyleTable,
    /// Blocks wider than the measure, top to bottom (pager layouts only;
    /// see the module docs).
    pub wide: Vec<Wide>,
    /// The `‹` and `›` marking where a shifted wide block is clipped (ASCII
    /// `<` and `>` with `ascii`).
    pub clip: [&'static str; 2],
    /// East Asian Ambiguous characters were measured as wide.
    pub ambiguous_wide: bool,
    /// Every `<details>`, in layout order (its index is its id, which
    /// [`Folds::toggled`] uses).
    pub details: Vec<DetailsInfo>,
    /// For each outline heading (indexed like [`Document::headings`]), the
    /// innermost `<details>` it is in; `u32::MAX` for none.
    pub heading_details: Vec<u32>,
    /// A wide block was cut to the measure or shown as records (only in
    /// layouts without [`Interactive::hscroll`]).
    pub narrowed: bool,
}

/// A block wider than the measure: its lines keep their natural width and
/// the pager shows a window of them, shifted sideways. Columns before `x0`
/// (the prefix of the containers around the block: list indentation,
/// quote bars) never move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wide {
    /// Its lines: `start..end`.
    pub start: u32,
    pub end: u32,
    /// The column where the block starts (relative to the text column).
    pub x0: u16,
    /// Columns its widest line takes (relative to the text column).
    pub cols: u16,
    /// The style of the `‹` and `›` at the clipped edges.
    pub marker: StyleId,
}

impl Wide {
    /// The most the block can be shifted by in a text column `measure`
    /// wide.
    pub fn max_shift(&self, measure: u16) -> u16 {
        self.cols.saturating_sub(measure)
    }
}

/// One `<details>` block of a layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetailsInfo {
    /// The first line of its summary; `u32::MAX` when not shown (it is
    /// inside a collapsed block).
    pub line: u32,
    /// Lines the summary takes.
    pub rows: u32,
    /// Column of the `▸`/`▾` marker on the first line (relative to the
    /// text column).
    pub col: u16,
    /// Lines its body takes when shown.
    pub body_lines: u32,
    /// Shown expanded.
    pub open: bool,
    /// Written `<details open>`.
    pub open_attr: bool,
    /// The `<details>` it is in; `u32::MAX` for none.
    pub parent: u32,
    /// Where it starts: the summary's position.
    pub pos: SrcPos,
    /// Content offsets (in block `pos.top`) where its body starts and
    /// where it ends.
    pub body: u32,
    pub end: u32,
}

impl DetailsInfo {
    /// Whether `pos` is in its body.
    pub fn holds(&self, pos: SrcPos) -> bool {
        pos.top == self.pos.top && (self.body..self.end).contains(&pos.off)
    }
}

/// What the pager changes about a layout ([`layout_with`]); the default
/// is stream mode's.
#[derive(Clone, Copy, Debug, Default)]
pub struct Interactive<'a> {
    /// Wide blocks keep their natural width, to be shifted sideways: code
    /// when it is not wrapped, and tables too wide for their columns when
    /// `tables.overflow` is `scroll`. Off, code is cut with `›` and such
    /// tables become records.
    pub hscroll: bool,
    /// Which `<details>` show their body; `None`: all of them.
    pub details: Option<Folds<'a>>,
}

/// Which `<details>` are expanded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Folds<'a> {
    /// Blocks without `open` are expanded too (`pager.details =
    /// "expanded"`).
    pub open: bool,
    /// Details ids (sorted) toggled from what they would be.
    pub toggled: &'a [u32],
}

impl Folds<'_> {
    /// Whether details `id` (written with `open` or not) is expanded.
    pub fn is_open(&self, id: u32, open_attr: bool) -> bool {
        (self.open || open_attr) != self.toggled.binary_search(&id).is_ok()
    }
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
        /// Typeset inline math (see [`Layout::math_hits`]).
        const MATH = 1 << 2;
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

/// A piece of inline math on one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MathHit {
    /// The line.
    pub line: u32,
    /// Column where the formula starts on the line (relative to the text
    /// column).
    pub col: u16,
    /// Where its TeX source starts: block and content offset (see
    /// [`SrcPos`]).
    pub pos: SrcPos,
    /// Length of the TeX source in bytes.
    pub len: u32,
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
    let inputs = LayoutInputs {
        doc,
        theme,
        caps,
        opts,
        highlighter,
        sizer,
        interactive: Interactive::default(),
    };
    run(inputs, width)
}

/// [`layout`] for the pager: with collapsed `<details>` and wide blocks
/// as `interactive` says (see the module docs). Lines of wide blocks may
/// be wider than the measure.
#[allow(clippy::too_many_arguments)]
pub fn layout_with(
    doc: &Document,
    width: u16,
    theme: &Theme,
    caps: &Caps,
    opts: &RenderOptions,
    highlighter: &dyn Highlighter,
    sizer: &dyn ImageSizer,
    interactive: Interactive<'_>,
) -> Layout {
    let inputs = LayoutInputs {
        doc,
        theme,
        caps,
        opts,
        highlighter,
        sizer,
        interactive,
    };
    run(inputs, width)
}

#[inline(never)]
fn run(inputs: LayoutInputs<'_>, width: u16) -> Layout {
    let (indent, measure) = geometry(width, inputs.opts, inputs.caps);
    build::Builder::new(inputs, width.max(1), indent, measure).run()
}

/// Everything a layout is made from but the width: the arguments of
/// [`layout_with`].
#[derive(Clone, Copy)]
pub struct LayoutInputs<'a> {
    pub doc: &'a Document,
    pub theme: &'a Theme,
    pub caps: &'a Caps,
    pub opts: &'a RenderOptions,
    pub highlighter: &'a dyn Highlighter,
    pub sizer: &'a dyn ImageSizer,
    pub interactive: Interactive<'a>,
}

/// A layout made a few top-level blocks at a time, so that the pager can
/// show the first screen of a long document before the rest is laid out.
///
/// [`LayoutJob::start`] gives an empty layout; [`LayoutJob::advance`] adds
/// top-level blocks to it until the caller has enough (the lines for a
/// screen, a time slice used up) or the document ends. A partial layout is
/// the beginning of the complete one, line for line: blocks are only ever
/// appended, so the lines already there never change as more come in. Its
/// [`Layout::block_lines`] cover the blocks laid out so far, its other side
/// tables what those blocks hold. Once [`LayoutJob::is_done`], the layout
/// is exactly what [`layout_with`] makes in one go.
pub struct LayoutJob {
    /// What laying out the next block needs; `None` once every block is
    /// laid out.
    saved: Option<build::Saved>,
}

impl LayoutJob {
    /// Start laying out `inputs.doc` for `width` columns: the job, and the
    /// layout it fills (no lines yet).
    pub fn start(inputs: LayoutInputs<'_>, width: u16) -> (LayoutJob, Layout) {
        let (indent, measure) = geometry(width, inputs.opts, inputs.caps);
        let (layout, saved) = build::Builder::new(inputs, width.max(1), indent, measure).suspend();
        (LayoutJob { saved: Some(saved) }, layout)
    }

    /// Whether every block is laid out.
    pub fn is_done(&self) -> bool {
        self.saved.is_none()
    }

    /// Lay out more top-level blocks into `layout` (the one
    /// [`LayoutJob::start`] gave, always with the same `inputs`) until
    /// `enough(layout)` holds after a block, or every block is laid out.
    /// `enough` should only look at the lines: the side tables are brought
    /// up to date when this returns. Returns whether the layout is
    /// complete.
    pub fn advance(
        &mut self,
        layout: &mut Layout,
        inputs: LayoutInputs<'_>,
        enough: &mut dyn FnMut(&Layout) -> bool,
    ) -> bool {
        let Some(saved) = self.saved.take() else {
            return true;
        };
        let mut builder = build::Builder::resume(inputs, mem::take(layout), saved);
        let done = builder.advance(enough);
        let (out, saved) = builder.suspend();
        *layout = out;
        if !done {
            self.saved = Some(saved);
        }
        done
    }

    /// Lay out every block that is left.
    pub fn finish(&mut self, layout: &mut Layout, inputs: LayoutInputs<'_>) {
        self.advance(layout, inputs, &mut |_| false);
    }
}

/// The glyphs of quote bars and code wrap markers as layout draws them
/// with `opts` (for taking them out of copied text).
pub fn deco_glyphs(opts: &RenderOptions) -> (String, String) {
    let d = deco::Deco::new(opts);
    (d.quote.text, d.wrap.text)
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

    /// The wide block line `line` is in.
    pub fn wide_at(&self, line: usize) -> Option<&Wide> {
        let line = u32::try_from(line).ok()?;
        let i = self.wide.partition_point(|w| w.end <= line);
        self.wide.get(i).filter(|w| w.start <= line)
    }

    /// How far line `line` is shifted when the pager's offset is `hscroll`:
    /// as far as its block allows (0 outside wide blocks).
    pub fn shift_at(&self, line: usize, hscroll: u16) -> u16 {
        self.wide_at(line)
            .map_or(0, |w| hscroll.min(w.max_shift(self.measure)))
    }

    /// Columns line `line` covers: its spans and its background.
    pub fn extent(&self, line: usize) -> u16 {
        self.lines.get(line).map_or(0, |l| match l.fill {
            Fill::Panel { to_col, .. } => l.cols.max(to_col),
            _ => l.cols,
        })
    }

    /// Where column `col` of line `line` (relative to the text column) is
    /// shown when the line is shifted by `shift`: `None` when it is
    /// shifted out of view, or under the `‹` or `›` at an edge.
    pub fn shown_col(&self, line: usize, col: u16, shift: u16) -> Option<u16> {
        match self.wide_at(line) {
            Some(w) if col >= w.x0 => {
                let shown = col.checked_sub(shift).filter(|&c| c >= w.x0)?;
                let left = shift > 0 && shown == w.x0;
                let right = shown.saturating_add(1) == self.measure
                    && self.extent(line) > self.measure.saturating_add(shift);
                (shown < self.measure && !left && !right).then_some(shown)
            }
            _ => Some(col),
        }
    }

    /// The column of line `line` shown at column `col` (relative to the
    /// text column) when the line is shifted by `shift`.
    pub fn layout_col(&self, line: usize, col: u16, shift: u16) -> u16 {
        match self.wide_at(line) {
            Some(w) if col >= w.x0 => col.saturating_add(shift),
            _ => col,
        }
    }

    /// The `<details>` whose summary shows on line `line`: its id.
    pub fn summary_at(&self, line: usize) -> Option<u32> {
        let line = u32::try_from(line).ok()?;
        let i = self.details.iter().position(|d| {
            d.line != u32::MAX && (d.line..d.line.saturating_add(d.rows)).contains(&line)
        })?;
        u32::try_from(i).ok()
    }

    /// The collapsed `<details>` holding `pos` in its body, innermost
    /// last (empty when `pos` is shown).
    pub fn folded_at(&self, pos: SrcPos) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for (i, d) in self.details.iter().enumerate() {
            if !d.open && d.holds(pos) {
                out.push(u32::try_from(i).unwrap_or(u32::MAX));
            }
        }
        out
    }

    /// The collapsed `<details>` around details `id` (itself included when
    /// it is collapsed), outermost first.
    pub fn folded_chain(&self, id: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur = id;
        while let Some(d) = self.details.get(cur as usize) {
            if !d.open {
                out.push(cur);
            }
            if d.parent >= cur {
                break;
            }
            cur = d.parent;
        }
        out.reverse();
        out
    }

    /// The collapsed `<details>` that hide heading `h`, outermost first
    /// (empty when it is shown or in none).
    pub fn heading_folds(&self, h: HeadingId) -> Vec<u32> {
        match self.heading_details.get(h.index()) {
            Some(&d) if d != u32::MAX => self.folded_chain(d),
            _ => Vec::new(),
        }
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
