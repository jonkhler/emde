//! The owned, width-independent document model.
//!
//! [`crate::parse`] turns pulldown-cmark events into a [`Document`]; layout
//! turns a document into lines for one terminal width. Nothing here depends
//! on widths, colours or the terminal, and no parser types leak through, so
//! the parser can be replaced without touching layout.
//!
//! # Inline content
//!
//! Paragraph-like content is an [`Inlines`]: one flat `String` plus
//! contiguous [`Run`]s that give each byte range a [`RunKind`], emphasis
//! [`InlineFlags`] and an optional link. Line-breaking constraints travel
//! with the text: atoms (never split), extra break points and hard breaks.
//! What a run's text *means* depends on its kind:
//!
//! | Kind | Text |
//! |---|---|
//! | [`RunKind::Text`] | prose (whitespace collapsed, `\n` only at hard breaks) |
//! | [`RunKind::Code`] | the code span, verbatim |
//! | [`RunKind::Math`] | the TeX source; layout typesets it with `emde_math` |
//! | [`RunKind::FootRef`] | the footnote number in decimal digits |
//! | [`RunKind::ImageChip`] | the image's alt text (never empty) |
//! | [`RunKind::Html`] | a raw HTML tag (only with `html = "raw"`) |
//!
//! # Identifiers
//!
//! Links, images, footnotes and headings live in side tables on the
//! document and are referenced by index ([`LinkId`], [`ImageId`],
//! [`FootnoteId`], [`HeadingId`]); top-level blocks are referenced by
//! [`BlockId`]. Lookups through the accessor methods are total.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::PathBuf;

use bitflags::bitflags;

use crate::source::Diagnostic;
use crate::text::wrap::Constraints;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl $name {
            /// The index into the owning table.
            pub const fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

id_type! {
    /// Index into [`Document::links`].
    LinkId
}
id_type! {
    /// Index into [`Document::images`].
    ImageId
}
id_type! {
    /// Index into [`Document::footnotes`]; the displayed number is `index + 1`.
    FootnoteId
}
id_type! {
    /// Index into [`Document::headings`].
    HeadingId
}
id_type! {
    /// Index into [`Document::blocks`] (top-level blocks only).
    BlockId
}

/// A parsed document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    /// Top-level blocks in reading order.
    pub blocks: Vec<Block>,
    /// Every link occurrence, including footnote references and bare URLs.
    pub links: Vec<Link>,
    /// Every image occurrence (figures and inline chips). Never decoded here.
    pub images: Vec<ImageRef>,
    /// Referenced footnotes, numbered by first reference.
    pub footnotes: Vec<Footnote>,
    /// The outline: every heading outside footnotes, in document order.
    pub headings: Vec<Heading>,
    /// Link targets within the document: heading slugs, explicit heading
    /// ids and HTML `<a id>`/`<a name>` anchors.
    pub anchors: HashMap<Box<str>, Anchor>,
    /// Front matter `title`, else the text of the first `h1`.
    pub title: Option<Box<str>>,
    /// Directory relative links and images resolve against (set by the
    /// caller from the source's origin).
    pub base_dir: Option<PathBuf>,
    /// Content problems found while parsing.
    pub diagnostics: Vec<Diagnostic>,
}

/// A block-level element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Para(Inlines),
    Heading {
        /// 1–6.
        level: u8,
        text: Inlines,
        /// The outline entry; `None` for headings inside footnotes, which
        /// are not part of the outline.
        id: Option<HeadingId>,
    },
    Code(CodeBlock),
    /// Display math (`$$…$$`, `\[…\]` or a `math` fence).
    Math(MathBlock),
    /// A block quote, or a GitHub alert (`> [!NOTE]`).
    Quote {
        alert: Option<Alert>,
        body: Vec<Block>,
    },
    List(List),
    Table(Table),
    /// A thematic break (`---`, `<hr>`).
    Rule,
    /// A paragraph that held only an image (optionally linked).
    Figure(Figure),
    DefList(Vec<DefItem>),
    /// HTML `<details>`; `summary` is empty when there was no `<summary>`.
    Details {
        summary: Inlines,
        body: Vec<Block>,
    },
    /// HTML `align=` / `<center>` wrapper.
    Align {
        align: HAlign,
        body: Vec<Block>,
    },
    FrontMatter(FrontMatter),
    /// Raw HTML shown as text (only with `html = "raw"`).
    Html(Box<str>),
    /// Where the footnotes ([`Document::footnotes`]) are shown.
    FootnoteSection,
}

/// Paragraph-like content: flat text plus runs; see the module docs.
///
/// Invariants (checked by [`Inlines::validate`]):
/// * runs are non-empty, their `end`s strictly increase, and the last one
///   ends at `text.len()`; every boundary is a char boundary;
/// * `atoms` are sorted, non-overlapping, non-empty ranges within the text;
/// * `extra_breaks` are sorted, unique offsets strictly inside the text;
/// * `hard_breaks` are exactly the offsets of the `\n` characters;
/// * the text has no control characters other than those `\n`s.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inlines {
    /// The text of all runs, concatenated.
    pub text: String,
    /// Contiguous runs covering `text`.
    pub runs: Vec<Run>,
    /// Byte ranges that must never be split across lines (footnote
    /// references, math, key caps).
    pub atoms: Vec<Range<u32>>,
    /// Break opportunities UAX #14 would not find: after `/ ? & = #` in
    /// URLs, and `<wbr>`.
    pub extra_breaks: Vec<u32>,
    /// Offsets of the `\n` characters that are hard breaks.
    pub hard_breaks: Vec<u32>,
}

/// A styled piece of an [`Inlines`]: bytes `[previous end, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// Byte offset where the run ends (it starts where the previous one
    /// ends, or at 0).
    pub end: u32,
    /// What the text is (see the module docs).
    pub kind: RunKind,
    /// Emphasis and semantic flags.
    pub flags: InlineFlags,
    /// The link this text belongs to.
    pub link: Option<LinkId>,
}

/// What a run's text is; see the table in the module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum RunKind {
    #[default]
    Text,
    Code,
    Math,
    FootRef(FootnoteId),
    ImageChip(ImageId),
    Html,
}

bitflags! {
    /// Inline emphasis and semantic flags.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
    pub struct InlineFlags: u16 {
        const EMPH = 1 << 0;
        const STRONG = 1 << 1;
        const STRIKE = 1 << 2;
        const UNDERLINE = 1 << 3;
        const MARK = 1 << 4;
        const SUP = 1 << 5;
        const SUB = 1 << 6;
        /// `<kbd>`: drawn as a key cap.
        const KBD = 1 << 7;
    }
}

/// One link occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The destination: as written (entity-decoded), with `mailto:` added
    /// for email autolinks and `http://` for bare `www.` links.
    pub url: Box<str>,
    /// The link title (`[a](url "title")`, `<a title>`), or empty.
    pub title: Box<str>,
    /// Where the link goes.
    pub target: Target,
    /// How the link was written.
    pub kind: LinkKind,
}

/// How a link was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// `[text](url)`.
    Inline,
    /// `[text][ref]`, `[text][]`, `[text]`.
    Reference,
    /// `<https://…>`.
    Autolink,
    /// `<me@example.org>`.
    Email,
    /// `[[Page]]`.
    Wiki,
    /// A bare URL or email found in text.
    Bare,
    /// HTML `<a href>`.
    Html,
    /// A footnote reference.
    Footnote,
}

/// Where a link goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A URL with a scheme (`https:`, `ftp:`, …) or protocol-relative (`//`).
    External,
    /// `mailto:`.
    Email,
    /// A fragment in this document (percent-decoded, without `#`).
    Anchor(Box<str>),
    /// A Markdown file or a directory (shown through its README), relative
    /// to [`Document::base_dir`] unless absolute.
    LocalDoc {
        path: PathBuf,
        anchor: Option<Box<str>>,
    },
    /// Any other local file.
    LocalFile(PathBuf),
    Footnote(FootnoteId),
}

/// One image occurrence. Layout sizes it from header dimensions only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageRef {
    /// The image location as written (a path or a URL).
    pub src: Box<str>,
    /// Plain alt text (markup removed).
    pub alt: Box<str>,
    /// The title (`![a](src "title")`), or empty.
    pub title: Box<str>,
    /// HTML `width=` hint.
    pub width: Option<Length>,
    /// HTML `height=` hint.
    pub height: Option<Length>,
    /// `<source>` elements of an enclosing `<picture>`, in order.
    pub sources: Vec<PictureSource>,
    /// The link wrapping the image.
    pub link: Option<LinkId>,
}

/// An HTML length.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Length {
    Px(u32),
    Percent(u32),
}

/// A `<picture>` `<source>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PictureSource {
    /// The raw `srcset` (candidates are `url [descriptor]`, comma separated).
    pub srcset: Box<str>,
    /// The `media` query, e.g. `(prefers-color-scheme: dark)`.
    pub media: Option<Box<str>>,
}

impl PictureSource {
    /// The URL of the first `srcset` candidate.
    pub fn first_url(&self) -> &str {
        self.srcset
            .split(',')
            .next()
            .and_then(|c| c.split_whitespace().next())
            .unwrap_or("")
    }
}

/// A figure: an image shown as a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Figure {
    /// The image shown (its title, else its alt text, is the caption).
    pub image: ImageId,
}

/// A referenced footnote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Footnote {
    /// The label as written (`[^label]`).
    pub label: Box<str>,
    /// The definition (empty if the label was never defined).
    pub body: Vec<Block>,
    /// The reference links, in document order (for `↑` back-links).
    pub refs: Vec<LinkId>,
}

/// Front matter (`---` YAML or `+++` TOML).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontMatter {
    /// YAML or TOML.
    pub format: FrontMatterFormat,
    /// The text between the delimiters.
    pub raw: Box<str>,
    /// Key/value pairs (quotes removed) when every entry is a flat scalar
    /// line; `None` for nested data, which is shown as code instead.
    pub fields: Option<Vec<(Box<str>, Box<str>)>>,
}

/// The syntax of a [`FrontMatter`] block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrontMatterFormat {
    Yaml,
    Toml,
}

/// A code block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeBlock {
    /// The first word of the info string (`{.rust}` and `rust,ignore` forms
    /// normalised to `rust`); `None` for indented blocks and bare fences.
    pub lang: Option<Box<str>>,
    /// The whole info string.
    pub info: Box<str>,
    /// `title="…"` from the info string.
    pub title: Option<Box<str>>,
    /// The code, without the final newline; tabs are kept (layout expands
    /// them) and lines are split on `\n`.
    pub code: String,
}

/// Display math.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MathBlock {
    /// The TeX source, trimmed.
    pub tex: Box<str>,
}

/// A bullet or ordered list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List {
    /// The first number of an ordered list; `None` for bullet lists.
    pub start: Option<u64>,
    /// No blank lines between items.
    pub tight: bool,
    /// The items, in order.
    pub items: Vec<ListItem>,
}

/// One list item.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListItem {
    /// `Some(done)` for task list items.
    pub task: Option<bool>,
    /// The item's content.
    pub body: Vec<Block>,
}

/// A table. The header and every row have exactly `align.len()` cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// Column alignment; `None` when the delimiter row gives none.
    pub align: Vec<Option<HAlign>>,
    /// The header cells.
    pub head: Vec<Inlines>,
    /// The body rows.
    pub rows: Vec<Vec<Inlines>>,
}

/// Horizontal alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// A definition list entry: a term and its definitions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DefItem {
    /// The term being defined.
    pub term: Inlines,
    /// Its definitions, each a list of blocks.
    pub defs: Vec<Vec<Block>>,
}

/// GitHub alert kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

/// An outline entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    /// 1–6.
    pub level: u8,
    /// Plain text of the heading (for the outline and breadcrumbs).
    pub title: Box<str>,
    /// The anchor: an explicit id (`{#id}`) or the GitHub slug, made unique
    /// with `-1`, `-2`, … suffixes.
    pub slug: Box<str>,
    /// The nearest preceding heading of a lower level.
    pub parent: Option<HeadingId>,
    /// The top-level block containing the heading.
    pub top: BlockId,
}

/// What an anchor points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Anchor {
    Heading(HeadingId),
    /// A top-level block (for `<a id>` anchors outside headings).
    Block(BlockId),
}

/// A width-independent position: a top-level block and a byte offset into
/// that block's content (the concatenated text of its leaves in reading
/// order). Layout records one per line, so the view can be re-anchored after
/// a resize or reload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SrcPos {
    /// Index of the top-level block.
    pub top: u32,
    /// Byte offset into the block's content.
    pub off: u32,
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Inlines {
    /// A single plain-text run (tests and synthesised content). Line breaks
    /// and tabs become spaces; control characters become pictures.
    pub fn from_text(text: &str) -> Inlines {
        let text = crate::text::sanitize(text).replace(['\t', '\n', '\u{2028}', '\u{2029}'], " ");
        let runs = if text.is_empty() {
            Vec::new()
        } else {
            vec![Run {
                end: to_u32(text.len()),
                kind: RunKind::Text,
                flags: InlineFlags::empty(),
                link: None,
            }]
        };
        Inlines {
            text,
            runs,
            ..Inlines::default()
        }
    }

    /// Whether there is no text.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The text of all runs.
    pub fn plain(&self) -> &str {
        &self.text
    }

    /// Line-breaking constraints for [`crate::text::wrap`].
    pub fn constraints(&self) -> Constraints<'_> {
        Constraints {
            atoms: &self.atoms,
            extra_breaks: &self.extra_breaks,
        }
    }

    /// Runs with their byte ranges.
    pub fn runs_with_ranges(&self) -> impl Iterator<Item = (Range<u32>, &Run)> + '_ {
        let mut start = 0;
        self.runs.iter().map(move |run| {
            let range = start..run.end;
            start = run.end;
            (range, run)
        })
    }

    /// The text of a byte range (empty if out of bounds).
    pub fn slice(&self, range: Range<u32>) -> &str {
        self.text
            .get(range.start as usize..range.end as usize)
            .unwrap_or("")
    }

    /// Check the structural invariants listed on [`Inlines`] (used by tests
    /// and fuzzing).
    pub fn validate(&self) -> Result<(), String> {
        let len = to_u32(self.text.len());
        let boundary = |p: u32| self.text.is_char_boundary(p as usize);
        let mut prev = 0;
        for (i, run) in self.runs.iter().enumerate() {
            if run.end <= prev || !boundary(run.end) {
                return Err(format!("run {i} ends at {} after {prev}", run.end));
            }
            prev = run.end;
        }
        if prev != len {
            return Err(format!("runs end at {prev}, text has {len} bytes"));
        }
        let mut last = 0;
        for a in &self.atoms {
            if a.start >= a.end || a.start < last || a.end > len {
                return Err(format!("bad atom {a:?}"));
            }
            if !boundary(a.start) || !boundary(a.end) {
                return Err(format!("atom {a:?} splits a character"));
            }
            last = a.end;
        }
        let mut last = 0;
        for &b in &self.extra_breaks {
            if b <= last || b >= len || !boundary(b) {
                return Err(format!("bad extra break {b}"));
            }
            last = b;
        }
        let newlines: Vec<u32> = self
            .text
            .match_indices('\n')
            .map(|(i, _)| to_u32(i))
            .collect();
        if newlines != self.hard_breaks {
            return Err(format!(
                "hard breaks {:?} but newlines at {newlines:?}",
                self.hard_breaks
            ));
        }
        if let Some(c) = self
            .text
            .chars()
            .find(|&c| c != '\n' && (c.is_control() || c == '\u{2028}' || c == '\u{2029}'))
        {
            return Err(format!("control character {c:?} in text"));
        }
        Ok(())
    }
}

impl Document {
    /// A link by id.
    pub fn link(&self, id: LinkId) -> Option<&Link> {
        self.links.get(id.index())
    }

    /// An image by id.
    pub fn image(&self, id: ImageId) -> Option<&ImageRef> {
        self.images.get(id.index())
    }

    /// A footnote by id.
    pub fn footnote(&self, id: FootnoteId) -> Option<&Footnote> {
        self.footnotes.get(id.index())
    }

    /// A heading by id.
    pub fn heading(&self, id: HeadingId) -> Option<&Heading> {
        self.headings.get(id.index())
    }

    /// The front matter, if the document has any.
    pub fn front_matter(&self) -> Option<&FrontMatter> {
        self.blocks.iter().find_map(|b| match b {
            Block::FrontMatter(fm) => Some(fm),
            _ => None,
        })
    }

    /// Caption of a figure: the image title, else its alt text.
    pub fn caption(&self, figure: &Figure) -> &str {
        match self.image(figure.image) {
            Some(img) if !img.title.is_empty() => &img.title,
            Some(img) => &img.alt,
            None => "",
        }
    }

    /// Resolve a link fragment (`#slug`, `slug`, `user-content-slug`), falling
    /// back to a case-insensitive match.
    pub fn resolve_anchor(&self, name: &str) -> Option<Anchor> {
        let name = name.strip_prefix('#').unwrap_or(name);
        let bare = name.strip_prefix("user-content-").unwrap_or(name);
        [name, bare]
            .into_iter()
            .find_map(|n| self.anchors.get(n).copied())
            .or_else(|| self.anchors.get(bare.to_lowercase().as_str()).copied())
    }

    /// Check the document's structural invariants (used by tests and
    /// fuzzing): every [`Inlines`] is valid, every id refers to an existing
    /// entry, tables are rectangular, and outline parents precede children.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_blocks(&self.blocks)?;
        for (i, f) in self.footnotes.iter().enumerate() {
            self.validate_blocks(&f.body)
                .map_err(|e| format!("footnote {i}: {e}"))?;
            if let Some(l) = f.refs.iter().find(|l| self.link(**l).is_none()) {
                return Err(format!("footnote {i} refers to missing {l:?}"));
            }
        }
        for (i, h) in self.headings.iter().enumerate() {
            if !(1..=6).contains(&h.level) {
                return Err(format!("heading {i} has level {}", h.level));
            }
            if h.parent.is_some_and(|p| p.index() >= i) {
                return Err(format!("heading {i} has a later parent"));
            }
            if h.top.index() >= self.blocks.len() {
                return Err(format!("heading {i} is in missing block {}", h.top.0));
            }
        }
        for (name, anchor) in &self.anchors {
            let ok = match anchor {
                Anchor::Heading(h) => self.heading(*h).is_some(),
                Anchor::Block(b) => b.index() < self.blocks.len().max(1),
            };
            if !ok {
                return Err(format!("anchor {name:?} points to {anchor:?}"));
            }
        }
        let dangling = self
            .images
            .iter()
            .filter_map(|i| i.link)
            .find(|l| self.link(*l).is_none());
        if let Some(l) = dangling {
            return Err(format!("image refers to missing {l:?}"));
        }
        Ok(())
    }

    fn validate_blocks(&self, blocks: &[Block]) -> Result<(), String> {
        for block in blocks {
            match block {
                Block::Para(t) => self.validate_inlines(t)?,
                Block::Heading { level, text, id } => {
                    if !(1..=6).contains(level) {
                        return Err(format!("heading level {level}"));
                    }
                    if id.is_some_and(|h| self.heading(h).is_none()) {
                        return Err(format!("missing heading {id:?}"));
                    }
                    self.validate_inlines(text)?;
                }
                Block::Quote { body, .. } | Block::Align { body, .. } => {
                    self.validate_blocks(body)?;
                }
                Block::Details { summary, body } => {
                    self.validate_inlines(summary)?;
                    self.validate_blocks(body)?;
                }
                Block::List(list) => {
                    for item in &list.items {
                        self.validate_blocks(&item.body)?;
                    }
                }
                Block::Table(t) => {
                    let cols = t.align.len();
                    if t.head.len() != cols || t.rows.iter().any(|r| r.len() != cols) {
                        return Err(format!("table is not {cols} columns wide"));
                    }
                    for cell in t.head.iter().chain(t.rows.iter().flatten()) {
                        self.validate_inlines(cell)?;
                    }
                }
                Block::DefList(items) => {
                    for item in items {
                        self.validate_inlines(&item.term)?;
                        for def in &item.defs {
                            self.validate_blocks(def)?;
                        }
                    }
                }
                Block::Figure(f) => {
                    if self.image(f.image).is_none() {
                        return Err(format!("figure of missing {:?}", f.image));
                    }
                }
                Block::Code(_)
                | Block::Math(_)
                | Block::Rule
                | Block::FrontMatter(_)
                | Block::Html(_)
                | Block::FootnoteSection => {}
            }
        }
        Ok(())
    }

    fn validate_inlines(&self, t: &Inlines) -> Result<(), String> {
        t.validate().map_err(|e| format!("{e} in {:?}", t.text))?;
        for run in &t.runs {
            if let Some(l) = run.link.filter(|l| self.link(*l).is_none()) {
                return Err(format!("run refers to missing {l:?}"));
            }
            let dangling = match run.kind {
                RunKind::FootRef(f) => self.footnote(f).is_none(),
                RunKind::ImageChip(i) => self.image(i).is_none(),
                _ => false,
            };
            if dangling {
                return Err(format!("run of missing {:?}", run.kind));
            }
        }
        Ok(())
    }

    /// All text content, one leaf per line: for tests and search. Math is
    /// its TeX source, table cells are separated by tabs.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            self.write_plain(block, &mut out);
        }
        out
    }

    fn write_plain(&self, block: &Block, out: &mut String) {
        let line = |s: &str, out: &mut String| {
            out.push_str(s);
            out.push('\n');
        };
        match block {
            Block::Para(t) | Block::Heading { text: t, .. } => line(&t.text, out),
            Block::Code(c) => line(&c.code, out),
            Block::Math(m) => line(&m.tex, out),
            Block::Quote { body, .. } | Block::Align { body, .. } => {
                body.iter().for_each(|b| self.write_plain(b, out));
            }
            Block::Details { summary, body } => {
                line(&summary.text, out);
                body.iter().for_each(|b| self.write_plain(b, out));
            }
            Block::List(list) => {
                for item in &list.items {
                    item.body.iter().for_each(|b| self.write_plain(b, out));
                }
            }
            Block::Table(t) => {
                for row in std::iter::once(&t.head).chain(&t.rows) {
                    let cells: Vec<&str> = row.iter().map(|c| c.text.as_str()).collect();
                    line(&cells.join("\t"), out);
                }
            }
            Block::Figure(f) => line(self.caption(f), out),
            Block::DefList(items) => {
                for item in items {
                    line(&item.term.text, out);
                    for def in &item.defs {
                        def.iter().for_each(|b| self.write_plain(b, out));
                    }
                }
            }
            Block::FrontMatter(fm) => line(fm.raw.trim_end(), out),
            Block::Html(h) => line(h.trim_end(), out),
            Block::Rule => {}
            Block::FootnoteSection => {
                for f in &self.footnotes {
                    f.body.iter().for_each(|b| self.write_plain(b, out));
                }
            }
        }
    }

    /// A stable, human-readable tree of the whole document, used by snapshot
    /// tests and `--dump ir`.
    pub fn dump(&self) -> String {
        let mut d = Dumper {
            doc: self,
            out: String::new(),
        };
        d.blocks(&self.blocks, 0);
        d.tables();
        d.out
    }
}

/// Writer for [`Document::dump`].
struct Dumper<'a> {
    doc: &'a Document,
    out: String,
}

impl Dumper<'_> {
    fn line(&mut self, depth: usize, s: &str) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn blocks(&mut self, blocks: &[Block], depth: usize) {
        for b in blocks {
            self.block(b, depth);
        }
    }

    fn block(&mut self, block: &Block, depth: usize) {
        match block {
            Block::Para(t) => self.inlines("para", t, depth),
            Block::Heading { level, text, id } => {
                let id = id.map_or_else(|| "-".to_string(), |h| format!("H{}", h.0));
                self.inlines(&format!("h{level} {id}"), text, depth);
            }
            Block::Code(c) => {
                let mut head = String::from("code");
                if let Some(lang) = &c.lang {
                    let _ = write!(head, " lang={lang}");
                }
                if let Some(title) = &c.title {
                    let _ = write!(head, " title={title:?}");
                }
                let _ = write!(head, ": {:?}", c.code);
                self.line(depth, &head);
            }
            Block::Math(m) => self.line(depth, &format!("math: {:?}", m.tex)),
            Block::Quote { alert, body } => {
                let head = match alert {
                    Some(a) => format!("quote {a:?}"),
                    None => "quote".to_string(),
                };
                self.line(depth, &head);
                self.blocks(body, depth + 1);
            }
            Block::List(list) => {
                let mut head = String::from("list");
                if let Some(n) = list.start {
                    let _ = write!(head, " start={n}");
                }
                head.push_str(if list.tight { " tight" } else { " loose" });
                self.line(depth, &head);
                for item in &list.items {
                    let head = match item.task {
                        Some(true) => "item [x]",
                        Some(false) => "item [ ]",
                        None => "item",
                    };
                    self.line(depth + 1, head);
                    self.blocks(&item.body, depth + 2);
                }
            }
            Block::Table(t) => {
                self.line(depth, &format!("table {:?}", t.align));
                for (i, cell) in t.head.iter().enumerate() {
                    self.inlines(&format!("head {i}"), cell, depth + 1);
                }
                for (r, row) in t.rows.iter().enumerate() {
                    for (i, cell) in row.iter().enumerate() {
                        self.inlines(&format!("cell {r}.{i}"), cell, depth + 1);
                    }
                }
            }
            Block::Rule => self.line(depth, "rule"),
            Block::Figure(f) => self.line(depth, &format!("figure I{}", f.image.0)),
            Block::DefList(items) => {
                self.line(depth, "deflist");
                for item in items {
                    self.inlines("term", &item.term, depth + 1);
                    for def in &item.defs {
                        self.line(depth + 2, "def");
                        self.blocks(def, depth + 3);
                    }
                }
            }
            Block::Details { summary, body } => {
                self.inlines("details", summary, depth);
                self.blocks(body, depth + 1);
            }
            Block::Align { align, body } => {
                self.line(depth, &format!("align {align:?}"));
                self.blocks(body, depth + 1);
            }
            Block::FrontMatter(fm) => {
                self.line(
                    depth,
                    &format!("front matter {:?}: {:?}", fm.format, fm.raw),
                );
                if let Some(fields) = &fm.fields {
                    for (k, v) in fields {
                        self.line(depth + 1, &format!("{k} = {v:?}"));
                    }
                }
            }
            Block::Html(h) => self.line(depth, &format!("html: {h:?}")),
            Block::FootnoteSection => self.line(depth, "footnotes"),
        }
    }

    fn inlines(&mut self, head: &str, t: &Inlines, depth: usize) {
        let mut s = format!("{head}:");
        for (range, run) in t.runs_with_ranges() {
            let _ = write!(s, " {:?}", t.slice(range));
            let mut attrs: Vec<String> = Vec::new();
            match run.kind {
                RunKind::Text => {}
                RunKind::Code => attrs.push("code".into()),
                RunKind::Math => attrs.push("math".into()),
                RunKind::FootRef(f) => attrs.push(format!("fn F{}", f.0)),
                RunKind::ImageChip(i) => attrs.push(format!("img I{}", i.0)),
                RunKind::Html => attrs.push("html".into()),
            }
            for (name, flag) in run.flags.iter_names() {
                let _ = flag;
                attrs.push(name.to_ascii_lowercase());
            }
            if let Some(l) = run.link {
                attrs.push(format!("L{}", l.0));
            }
            if !attrs.is_empty() {
                let _ = write!(s, "[{}]", attrs.join(","));
            }
        }
        self.line(depth, &s);
        if !t.atoms.is_empty() {
            let atoms: Vec<String> = t.atoms.iter().map(|a| format!("{a:?}")).collect();
            self.line(depth + 1, &format!("atoms {}", atoms.join(" ")));
        }
        if !t.extra_breaks.is_empty() {
            self.line(depth + 1, &format!("breaks {:?}", t.extra_breaks));
        }
    }

    fn tables(&mut self) {
        let doc = self.doc;
        for (i, l) in doc.links.iter().enumerate() {
            let mut s = format!("L{i} {:?} {:?}", l.kind, l.url);
            if !l.title.is_empty() {
                let _ = write!(s, " title={:?}", l.title);
            }
            let _ = write!(s, " -> {:?}", l.target);
            self.line(0, &s);
        }
        for (i, img) in doc.images.iter().enumerate() {
            let mut s = format!("I{i} src={:?} alt={:?}", img.src, img.alt);
            if !img.title.is_empty() {
                let _ = write!(s, " title={:?}", img.title);
            }
            if let Some(w) = img.width {
                let _ = write!(s, " width={w:?}");
            }
            if let Some(h) = img.height {
                let _ = write!(s, " height={h:?}");
            }
            if let Some(l) = img.link {
                let _ = write!(s, " link=L{}", l.0);
            }
            self.line(0, &s);
            for src in &img.sources {
                self.line(1, &format!("source {:?} media={:?}", src.srcset, src.media));
            }
        }
        for (i, h) in doc.headings.iter().enumerate() {
            let parent = h
                .parent
                .map_or_else(|| "-".to_string(), |p| format!("H{}", p.0));
            self.line(
                0,
                &format!(
                    "H{i} h{} {:?} #{} parent={parent} top={}",
                    h.level, h.title, h.slug, h.top.0
                ),
            );
        }
        let mut anchors: Vec<(&str, Anchor)> =
            doc.anchors.iter().map(|(k, v)| (&**k, *v)).collect();
        anchors.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (name, a) in anchors {
            let target = match a {
                Anchor::Heading(h) => format!("H{}", h.0),
                Anchor::Block(b) => format!("block {}", b.0),
            };
            self.line(0, &format!("anchor #{name} -> {target}"));
        }
        for (i, f) in doc.footnotes.iter().enumerate() {
            let refs: Vec<String> = f.refs.iter().map(|l| format!("L{}", l.0)).collect();
            self.line(0, &format!("F{i} [^{}] refs={}", f.label, refs.join(",")));
            self.blocks(&f.body, 1);
        }
        if let Some(t) = &doc.title {
            self.line(0, &format!("title {t:?}"));
        }
        for d in &doc.diagnostics {
            self.line(0, &format!("diagnostic {:?}: {}", d.kind, d.message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(end: u32) -> Run {
        Run {
            end,
            kind: RunKind::Text,
            flags: InlineFlags::empty(),
            link: None,
        }
    }

    #[test]
    fn from_text_is_valid() {
        let t = Inlines::from_text("a\tb\nc\x1b\u{2028}d");
        assert_eq!(t.text, "a b c␛ d");
        t.validate().unwrap();
        assert!(Inlines::from_text("").runs.is_empty());
        Inlines::from_text("").validate().unwrap();
    }

    #[test]
    fn validate_catches_broken_invariants() {
        let mut t = Inlines::from_text("hello");
        t.runs = vec![run(3)];
        assert!(t.validate().is_err(), "runs must cover the text");
        t.runs = vec![run(3), run(3), run(5)];
        assert!(t.validate().is_err(), "no empty runs");
        t.runs = vec![run(5)];
        t.atoms = vec![Range { start: 2, end: 1 }];
        assert!(t.validate().is_err());
        t.atoms = vec![];
        t.extra_breaks = vec![0];
        assert!(t.validate().is_err(), "no break at 0");
        t.extra_breaks = vec![];
        t.hard_breaks = vec![1];
        assert!(t.validate().is_err(), "hard breaks must be newlines");
        t.hard_breaks = vec![];
        t.validate().unwrap();
    }

    #[test]
    fn runs_with_ranges_are_contiguous() {
        let t = Inlines {
            text: "abcdef".into(),
            runs: vec![run(2), run(6)],
            ..Inlines::default()
        };
        let ranges: Vec<_> = t.runs_with_ranges().map(|(r, _)| r).collect();
        assert_eq!(ranges, [0..2, 2..6]);
        assert_eq!(t.slice(2..6), "cdef");
        assert_eq!(t.slice(4..99), "");
    }

    #[test]
    fn anchor_resolution_falls_back() {
        let mut doc = Document::default();
        doc.anchors
            .insert("intro".into(), Anchor::Heading(HeadingId(0)));
        let want = Some(Anchor::Heading(HeadingId(0)));
        assert_eq!(doc.resolve_anchor("#intro"), want);
        assert_eq!(doc.resolve_anchor("user-content-intro"), want);
        assert_eq!(doc.resolve_anchor("Intro"), want);
        assert_eq!(doc.resolve_anchor("missing"), None);
    }

    #[test]
    fn caption_prefers_title() {
        let mut doc = Document::default();
        doc.images.push(ImageRef {
            alt: "alt".into(),
            ..ImageRef::default()
        });
        doc.images.push(ImageRef {
            alt: "alt".into(),
            title: "title".into(),
            ..ImageRef::default()
        });
        assert_eq!(doc.caption(&Figure { image: ImageId(0) }), "alt");
        assert_eq!(doc.caption(&Figure { image: ImageId(1) }), "title");
        assert_eq!(doc.caption(&Figure { image: ImageId(9) }), "");
    }

    #[test]
    fn picture_source_first_url() {
        let s = PictureSource {
            srcset: "dark.png 1x, dark@2x.png 2x".into(),
            media: None,
        };
        assert_eq!(s.first_url(), "dark.png");
    }
}
