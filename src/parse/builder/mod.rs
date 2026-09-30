//! The IR builder: a total stack machine over pulldown-cmark events.
//!
//! [`Builder::event`] accepts events in *any* order and never panics:
//! Markdown's block structure lives on a container stack, the current
//! paragraph-like block is a *leaf* ([`InlineBuf`]), and inline formatting
//! is a flag stack. Every mismatch degrades gracefully:
//!
//! * an `End` closes the nearest open container of its kind, finishing any
//!   container opened after it; an `End` with no matching `Start` is
//!   ignored;
//! * inline content outside any leaf opens an implicit paragraph (this is
//!   how tight list items arrive), and a block starting inside a leaf
//!   finishes the leaf first;
//! * a block that arrives where it cannot live (a paragraph directly in a
//!   list or table) goes to the nearest container that can hold it;
//! * nesting deeper than [`MAX_DEPTH`] containers is flattened, so the IR
//!   stays shallow enough for recursive consumers.
//!
//! These rules make the event orders that crashed mdcat harmless:
//! footnote references in table cells, `<br>` anywhere, and task list
//! items containing paragraphs.
//!
//! HTML (block and inline) goes through the tolerant lexer in
//! [`super::html`]. Container tags (`<details>`, `<div align>`, …) that open
//! and close in different HTML blocks are matched on the same container
//! stack; they close with their tag, or at the end of the Markdown container
//! they were opened in.
//!
//! This module holds the state and the event dispatch; [`blocks`] manages
//! containers, leaves and the outline, [`inlines`] inline content and links,
//! and [`html_subset`] the HTML tags.

mod blocks;
mod html_subset;
mod inlines;

use std::collections::HashMap;
use std::mem;
use std::ops::Range;

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, MetadataBlockKind, Tag, TagEnd,
};

use super::ParseOptions;
use super::inline::InlineBuf;
use super::linkify;
use super::slug::Slugger;
use crate::ir::{
    Alert, Anchor, Block, BlockId, DefItem, Document, Footnote, FootnoteId, FrontMatterFormat,
    HAlign, HeadingId, InlineFlags, Inlines, LinkId, ListItem, PictureSource, Table,
};
use crate::text::sanitize::sanitize;

/// Deepest container nesting kept; deeper structure is flattened.
pub(crate) const MAX_DEPTH: usize = 128;

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// An open block container.
#[derive(Debug)]
enum Container {
    Quote {
        alert: Option<Alert>,
        body: Vec<Block>,
    },
    List {
        start: Option<u64>,
        tight: bool,
        items: Vec<ListItem>,
    },
    Item {
        task: Option<bool>,
        body: Vec<Block>,
    },
    Footnote {
        label: Box<str>,
        body: Vec<Block>,
    },
    Table(TableBuild),
    DefList {
        items: Vec<DefItem>,
    },
    Definition {
        body: Vec<Block>,
    },
    /// An HTML container element, closed by its (canonical) tag name.
    Html {
        name: &'static str,
        kind: HtmlBox,
        body: Vec<Block>,
    },
}

/// What an HTML container becomes.
#[derive(Debug)]
enum HtmlBox {
    Details {
        summary: Option<Inlines>,
    },
    Align(HAlign),
    /// A plain `<div>`/`<p>`: transparent, only separates paragraphs.
    Group,
}

/// Markdown container kinds, for matching `End` events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Quote,
    List,
    Item,
    Footnote,
    Table,
    DefList,
    Definition,
}

impl Container {
    fn kind(&self) -> Option<Kind> {
        Some(match self {
            Container::Quote { .. } => Kind::Quote,
            Container::List { .. } => Kind::List,
            Container::Item { .. } => Kind::Item,
            Container::Footnote { .. } => Kind::Footnote,
            Container::Table(_) => Kind::Table,
            Container::DefList { .. } => Kind::DefList,
            Container::Definition { .. } => Kind::Definition,
            Container::Html { .. } => return None,
        })
    }

    /// The block list new blocks go to (`None` for tables).
    fn blocks_mut(&mut self) -> Option<&mut Vec<Block>> {
        match self {
            Container::Quote { body, .. }
            | Container::Item { body, .. }
            | Container::Footnote { body, .. }
            | Container::Definition { body }
            | Container::Html { body, .. } => Some(body),
            Container::List { items, .. } => {
                if items.is_empty() {
                    items.push(ListItem::default());
                }
                items.last_mut().map(|item| &mut item.body)
            }
            Container::DefList { items } => {
                if items.is_empty() {
                    items.push(DefItem::default());
                }
                let item = items.last_mut()?;
                if item.defs.is_empty() {
                    item.defs.push(Vec::new());
                }
                item.defs.last_mut()
            }
            Container::Table(_) => None,
        }
    }
}

/// A table being built.
#[derive(Debug, Default)]
struct TableBuild {
    align: Vec<Option<HAlign>>,
    head: Vec<Inlines>,
    rows: Vec<Vec<Inlines>>,
    row: Vec<Inlines>,
    in_head: bool,
}

impl TableBuild {
    fn end_row(&mut self) {
        if !self.row.is_empty() {
            self.rows.push(mem::take(&mut self.row));
        }
    }

    /// Normalise to a rectangular table: every row gets exactly one cell
    /// per column.
    fn finish(mut self) -> Table {
        self.end_row();
        let cols = if self.align.is_empty() {
            let widest = self.rows.iter().map(Vec::len).max().unwrap_or(0);
            self.head.len().max(widest)
        } else {
            self.align.len()
        };
        self.align.resize(cols, None);
        self.head.resize_with(cols, Inlines::default);
        for row in &mut self.rows {
            row.resize_with(cols, Inlines::default);
        }
        Table {
            align: self.align,
            head: self.head,
            rows: self.rows,
        }
    }
}

/// The paragraph-like block being filled.
#[derive(Debug)]
struct Leaf {
    kind: LeafKind,
    buf: InlineBuf,
    /// `<a id>`/`<a name>` anchors found inside.
    anchors: Vec<Box<str>>,
    seq: u32,
}

#[derive(Debug)]
enum LeafKind {
    Para,
    Heading {
        level: u8,
        /// `{#id}` (heading attributes): replaces the generated slug.
        explicit_id: Option<Box<str>>,
        /// HTML `id=`: an extra anchor.
        html_id: Option<Box<str>>,
    },
    Cell,
    Term,
    Summary,
}

/// Where HTML formatting was opened: it ends with that scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// Inline HTML in the leaf with this sequence number.
    Leaf(u32),
    /// The HTML block with this sequence number.
    Block(u32),
}

/// Formatting opened by an HTML tag.
#[derive(Debug)]
struct HtmlFlag {
    name: &'static str,
    flag: InlineFlags,
    /// `<code>`/`<tt>`/`<samp>`: text becomes a code run.
    code: bool,
    scope: Scope,
}

/// An open link.
#[derive(Debug)]
struct LinkFrame {
    id: LinkId,
    /// Opened by HTML `<a href>` (closed by `</a>`), not Markdown.
    html: bool,
    scope: Option<Scope>,
    /// The leaf and text offset where the link text starts.
    leaf: u32,
    start: usize,
    /// An autolink: its text is a URL and gets URL break points.
    url_like: bool,
}

#[derive(Debug)]
struct ImageCapture {
    src: Box<str>,
    title: Box<str>,
    alt: String,
    /// Nested images inside the alt text.
    depth: u32,
}

#[derive(Debug)]
struct CodeCapture {
    info: Box<str>,
    text: String,
}

#[derive(Debug)]
struct MetaCapture {
    format: FrontMatterFormat,
    raw: String,
}

/// Builds a [`Document`] from pulldown-cmark events; see the module docs.
#[derive(Debug)]
pub(crate) struct Builder {
    opts: ParseOptions,
    doc: Document,
    root: Vec<Block>,
    stack: Vec<Container>,
    /// Container starts ignored because of [`MAX_DEPTH`] (their ends are
    /// ignored too).
    overflow: usize,
    leaf: Option<Leaf>,
    leaf_seq: u32,
    md_flags: Vec<(TagEnd, InlineFlags)>,
    html_flags: Vec<HtmlFlag>,
    links: Vec<LinkFrame>,
    image: Option<ImageCapture>,
    code: Option<CodeCapture>,
    meta: Option<MetaCapture>,
    html_block: Option<String>,
    html_seq: u32,
    /// Inside `<pre>`: its text.
    pre: Option<String>,
    /// Inside `<script>`/`<style>`/…: content is dropped until this end tag.
    skip: Option<&'static str>,
    /// Inside `<picture>`: the `<source>`s seen so far.
    picture: Option<Vec<PictureSource>>,
    /// Anchors waiting for the next block.
    pending_anchors: Vec<Box<str>>,
    /// Anchors inside footnote definitions: they point at the footnote
    /// section, whose position is only known at the end.
    footnote_anchors: Vec<Box<str>>,
    slugger: Slugger,
    heading_stack: Vec<(u8, HeadingId)>,
    first_h1: Option<Box<str>>,
    footnote_ids: HashMap<Box<str>, FootnoteId>,
    /// Label and reference links of each numbered footnote.
    footnotes: Vec<(Box<str>, Vec<LinkId>)>,
    footnote_defs: HashMap<Box<str>, Vec<Block>>,
    found: Vec<linkify::Found>,
    /// Source length of the event being handled (a capacity hint).
    size_hint: usize,
}

impl Builder {
    pub(crate) fn new(opts: ParseOptions) -> Builder {
        Builder {
            opts,
            doc: Document::default(),
            root: Vec::new(),
            stack: Vec::new(),
            overflow: 0,
            leaf: None,
            leaf_seq: 0,
            md_flags: Vec::new(),
            html_flags: Vec::new(),
            links: Vec::new(),
            image: None,
            code: None,
            meta: None,
            html_block: None,
            html_seq: 0,
            pre: None,
            skip: None,
            picture: None,
            pending_anchors: Vec::new(),
            footnote_anchors: Vec::new(),
            slugger: Slugger::default(),
            heading_stack: Vec::new(),
            first_h1: None,
            footnote_ids: HashMap::new(),
            footnotes: Vec::new(),
            footnote_defs: HashMap::new(),
            found: Vec::new(),
            size_hint: 0,
        }
    }

    /// Feed one event (without a source range).
    #[cfg(test)]
    pub(crate) fn event(&mut self, ev: Event<'_>) {
        self.event_at(ev, 0..0);
    }

    /// Feed one event with its source range, which sizes the buffers of the
    /// blocks it starts.
    pub(crate) fn event_at(&mut self, ev: Event<'_>, range: Range<usize>) {
        if !self.capture(&ev) {
            self.size_hint = range.len();
            self.dispatch(ev);
        }
    }

    /// Finish all open structure and return the document.
    pub(crate) fn finish(mut self) -> Document {
        self.finish_code();
        self.finish_meta();
        self.finish_html_block();
        self.finish_image();
        self.flush_leaf(true);
        while let Some(c) = self.stack.pop() {
            self.finish_container(c);
        }
        if !self.pending_anchors.is_empty() {
            let last = BlockId(to_u32(self.root.len().saturating_sub(1)));
            for name in mem::take(&mut self.pending_anchors) {
                self.doc.anchors.entry(name).or_insert(Anchor::Block(last));
            }
        }
        let anchors_in_footnotes = mem::take(&mut self.footnote_anchors);
        if !self.footnotes.is_empty() {
            let section = Anchor::Block(BlockId(to_u32(self.root.len())));
            for name in anchors_in_footnotes {
                self.doc.anchors.entry(name).or_insert(section);
            }
            let footnotes = mem::take(&mut self.footnotes);
            self.doc.footnotes = footnotes
                .into_iter()
                .map(|(label, refs)| Footnote {
                    body: self.footnote_defs.remove(&label).unwrap_or_default(),
                    label,
                    refs,
                })
                .collect();
            self.root.push(Block::FootnoteSection);
        }
        // Every block anchor must name an existing block.
        let last = BlockId(to_u32(self.root.len().saturating_sub(1)));
        for anchor in self.doc.anchors.values_mut() {
            if let Anchor::Block(b) = anchor
                && *b > last
            {
                *b = last;
            }
        }
        let fm_title = self.root.iter().find_map(|b| match b {
            Block::FrontMatter(fm) => fm
                .fields
                .as_ref()?
                .iter()
                .find(|(k, v)| &**k == "title" && !v.is_empty())
                .map(|(_, v)| v.clone()),
            _ => None,
        });
        self.doc.title = fm_title.or_else(|| self.first_h1.take());
        self.doc.blocks = mem::take(&mut self.root);
        self.doc
    }

    /// Route an event into an open capture (code block, front matter, HTML
    /// block, image alt text). Returns whether the event was consumed; an
    /// event that cannot belong to the capture closes it first.
    fn capture(&mut self, ev: &Event<'_>) -> bool {
        if let Some(code) = &mut self.code {
            match ev {
                Event::Text(t) => code.text.push_str(t),
                Event::End(TagEnd::CodeBlock) => self.finish_code(),
                _ => {
                    self.finish_code();
                    return false;
                }
            }
            return true;
        }
        if let Some(meta) = &mut self.meta {
            match ev {
                Event::Text(t) => meta.raw.push_str(t),
                Event::End(TagEnd::MetadataBlock(_)) => self.finish_meta(),
                _ => {
                    self.finish_meta();
                    return false;
                }
            }
            return true;
        }
        if let Some(html) = &mut self.html_block {
            match ev {
                Event::Html(t) | Event::InlineHtml(t) | Event::Text(t) => html.push_str(t),
                Event::End(TagEnd::HtmlBlock) => self.finish_html_block(),
                _ => {
                    self.finish_html_block();
                    return false;
                }
            }
            return true;
        }
        if let Some(img) = &mut self.image {
            match ev {
                Event::Text(t) | Event::Code(t) | Event::InlineMath(t) | Event::DisplayMath(t) => {
                    img.alt.push_str(t);
                }
                Event::SoftBreak | Event::HardBreak => img.alt.push(' '),
                Event::Start(Tag::Image { .. }) => img.depth += 1,
                Event::End(TagEnd::Image) => {
                    if img.depth == 0 {
                        self.finish_image();
                    } else {
                        img.depth -= 1;
                    }
                }
                // Alt text is plain: formatting, links and HTML are dropped.
                Event::Start(
                    Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Superscript
                    | Tag::Subscript
                    | Tag::Link { .. },
                )
                | Event::End(
                    TagEnd::Emphasis
                    | TagEnd::Strong
                    | TagEnd::Strikethrough
                    | TagEnd::Superscript
                    | TagEnd::Subscript
                    | TagEnd::Link,
                )
                | Event::InlineHtml(_)
                | Event::FootnoteReference(_)
                | Event::TaskListMarker(_) => {}
                _ => {
                    self.finish_image();
                    return false;
                }
            }
            return true;
        }
        false
    }

    /// Handle an event outside any capture.
    fn dispatch(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => self.end(end),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => {
                let style = self.style();
                self.leaf_mut().buf.push_code(&t, style);
            }
            Event::InlineMath(t) => self.inline_math(&t),
            Event::DisplayMath(t) => self.display_math(&t),
            Event::Html(t) | Event::InlineHtml(t) => self.inline_html(&t),
            Event::FootnoteReference(label) => self.footnote_ref(&label),
            Event::SoftBreak => {
                let style = self.style();
                if let Some(leaf) = self.leaf.as_mut() {
                    leaf.buf.soft_break(style);
                }
            }
            Event::HardBreak => {
                let style = self.style();
                if let Some(leaf) = self.leaf.as_mut() {
                    leaf.buf.hard_break(style);
                }
            }
            Event::Rule => {
                self.flush_leaf(true);
                self.push_block(Block::Rule, Vec::new());
            }
            Event::TaskListMarker(done) => self.task(done),
        }
    }

    /// A `Start` event: open a container, leaf, capture or inline style.
    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.flush_leaf(true);
                // A paragraph directly in an item makes the list loose.
                if let [.., Container::List { tight, .. }, Container::Item { .. }] =
                    self.stack.as_mut_slice()
                {
                    *tight = false;
                }
                self.open_leaf(LeafKind::Para);
            }
            Tag::Heading { level, id, .. } => {
                self.flush_leaf(true);
                let explicit_id = id
                    .filter(|_| self.opts.heading_attributes)
                    .and_then(|i| non_empty(sanitize(&i).trim()));
                self.open_leaf(LeafKind::Heading {
                    level: heading_level(level),
                    explicit_id,
                    html_id: None,
                });
            }
            Tag::BlockQuote(kind) => {
                self.flush_leaf(true);
                self.push_container(Container::Quote {
                    alert: kind.map(alert),
                    body: Vec::new(),
                });
            }
            Tag::CodeBlock(kind) => {
                self.flush_leaf(true);
                let info = match kind {
                    CodeBlockKind::Fenced(info) => Box::from(sanitize(&info).trim()),
                    CodeBlockKind::Indented => Box::from(""),
                };
                self.code = Some(CodeCapture {
                    info,
                    text: String::new(),
                });
            }
            Tag::HtmlBlock => {
                self.flush_leaf(true);
                self.html_block = Some(String::new());
            }
            Tag::List(start) => {
                self.flush_leaf(true);
                self.push_container(Container::List {
                    start,
                    tight: true,
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                self.flush_leaf(true);
                self.push_container(Container::Item {
                    task: None,
                    body: Vec::new(),
                });
            }
            Tag::FootnoteDefinition(label) => {
                self.flush_leaf(true);
                self.push_container(Container::Footnote {
                    label: sanitize(&label).into(),
                    body: Vec::new(),
                });
            }
            Tag::DefinitionList => {
                self.flush_leaf(true);
                self.push_container(Container::DefList { items: Vec::new() });
            }
            Tag::DefinitionListTitle => self.open_leaf(LeafKind::Term),
            Tag::DefinitionListDefinition => {
                self.flush_leaf(true);
                self.push_container(Container::Definition { body: Vec::new() });
            }
            Tag::Table(align) => {
                self.flush_leaf(true);
                self.push_container(Container::Table(TableBuild {
                    align: align.iter().map(|&a| column_align(a)).collect(),
                    ..TableBuild::default()
                }));
            }
            Tag::TableHead => {
                self.flush_leaf(true);
                if let Some(Container::Table(t)) = self.stack.last_mut() {
                    t.end_row();
                    t.in_head = true;
                }
            }
            Tag::TableRow => {
                self.flush_leaf(true);
                if let Some(Container::Table(t)) = self.stack.last_mut() {
                    t.end_row();
                }
            }
            Tag::TableCell => self.open_leaf(LeafKind::Cell),
            Tag::Emphasis => self.md_flags.push((TagEnd::Emphasis, InlineFlags::EMPH)),
            Tag::Strong => self.md_flags.push((TagEnd::Strong, InlineFlags::STRONG)),
            Tag::Strikethrough => self
                .md_flags
                .push((TagEnd::Strikethrough, InlineFlags::STRIKE)),
            Tag::Superscript => self.md_flags.push((TagEnd::Superscript, InlineFlags::SUP)),
            Tag::Subscript => self.md_flags.push((TagEnd::Subscript, InlineFlags::SUB)),
            Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            } => self.start_link(link_type, &dest_url, &title),
            Tag::Image {
                dest_url, title, ..
            } => {
                self.image = Some(ImageCapture {
                    src: Box::from(sanitize(&dest_url).trim()),
                    title: sanitize(&title).into(),
                    alt: String::new(),
                    depth: 0,
                });
            }
            Tag::MetadataBlock(kind) => {
                self.flush_leaf(true);
                self.meta = Some(MetaCapture {
                    format: match kind {
                        MetadataBlockKind::YamlStyle => FrontMatterFormat::Yaml,
                        MetadataBlockKind::PlusesStyle => FrontMatterFormat::Toml,
                    },
                    raw: String::new(),
                });
            }
        }
    }

    /// An `End` event: close what the matching `Start` opened (if anything).
    fn end(&mut self, end: TagEnd) {
        match end {
            TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::TableCell
            | TagEnd::DefinitionListTitle => self.flush_leaf(true),
            TagEnd::TableHead => {
                self.flush_leaf(true);
                if let Some(Container::Table(t)) = self.stack.last_mut() {
                    t.in_head = false;
                }
            }
            TagEnd::TableRow => {
                self.flush_leaf(true);
                if let Some(Container::Table(t)) = self.stack.last_mut() {
                    t.end_row();
                }
            }
            TagEnd::BlockQuote(_) => self.close(Kind::Quote),
            TagEnd::List(_) => self.close(Kind::List),
            TagEnd::Item => self.close(Kind::Item),
            TagEnd::FootnoteDefinition => self.close(Kind::Footnote),
            TagEnd::Table => self.close(Kind::Table),
            TagEnd::DefinitionList => self.close(Kind::DefList),
            TagEnd::DefinitionListDefinition => self.close(Kind::Definition),
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript => {
                if let Some(i) = self.md_flags.iter().rposition(|(e, _)| *e == end) {
                    self.md_flags.remove(i);
                }
            }
            TagEnd::Link => self.end_link(false),
            // Closed by their captures; stray ends are ignored.
            TagEnd::Image | TagEnd::CodeBlock | TagEnd::HtmlBlock | TagEnd::MetadataBlock(_) => {}
        }
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn alert(kind: BlockQuoteKind) -> Alert {
    match kind {
        BlockQuoteKind::Note => Alert::Note,
        BlockQuoteKind::Tip => Alert::Tip,
        BlockQuoteKind::Important => Alert::Important,
        BlockQuoteKind::Warning => Alert::Warning,
        BlockQuoteKind::Caution => Alert::Caution,
    }
}

fn column_align(a: Alignment) -> Option<HAlign> {
    match a {
        Alignment::None => None,
        Alignment::Left => Some(HAlign::Left),
        Alignment::Center => Some(HAlign::Center),
        Alignment::Right => Some(HAlign::Right),
    }
}

/// `Some` boxed copy of a non-empty string.
fn non_empty(s: &str) -> Option<Box<str>> {
    (!s.is_empty()).then(|| Box::from(s))
}

/// Collapse whitespace runs to single spaces and trim.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
