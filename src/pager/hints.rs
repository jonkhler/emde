//! Hints: labels on what is on screen, Vimium-style.
//!
//! [`open`] finds the points of interest on screen for a [`HintKind`] —
//! links (footnote references and back-links among them), headings, code
//! blocks, display and inline math, tables, figures, and for copying and
//! selecting paragraphs, list items and quotes too — and gives each a
//! label from the home row ([`labels`]), as short as possible: one key
//! each while there are few enough, else some (or all) two keys long, and
//! so on. The chip of a label goes where its target starts on screen (on
//! its first line on screen, when it starts above).
//!
//! What a label does is decided when the hints open ([`Act`]); what yank
//! hints copy is the element's source ([`copy_of`]): code without its
//! fences, the TeX of math, the Markdown of a table, a paragraph, a list
//! item or a quote as written in the file (or their text where the source
//! is not known), a link's URL, `file#slug` for a heading, an image's
//! location.

use std::ops::Range;

use crate::ir::{Block, BlockId, Document, HAlign, ImageId, ListItem, SrcPos, Table};
use crate::layout::{Layout, LineKind};

use super::links::{self, HINT_KEYS};
use super::state::{HintKind, Hints, Mode, State};
use super::update::Effect;

/// A labelled point of interest: where its chip goes and what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// The line of the chip.
    pub(crate) line: usize,
    /// Its column (relative to the text column).
    pub(crate) col: u16,
    pub(crate) act: Act,
}

/// What choosing a label does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    /// Follow link occurrence `i` (of [`super::state::Derived::links`]).
    Link(usize),
    /// Show this line at the top.
    Jump(usize),
    /// Show the figure at full size.
    Zoom(ImageId),
    /// Put text on the clipboard.
    Copy(Copied),
    /// Select these lines (Visual mode).
    Select(Range<usize>),
}

/// Text to copy, and what the status bar says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Copied {
    pub(crate) text: String,
    /// `copied code block (12 lines)`.
    pub(crate) what: String,
}

impl Copied {
    /// `text`, described as `what` with its number of lines.
    #[inline(never)]
    pub(crate) fn lines(text: String, what: &str) -> Copied {
        Copied {
            what: format!("copied {what} ({})", count(line_count(&text), "line")),
            text,
        }
    }

    /// The effect that puts it on the clipboard.
    pub(crate) fn effect(self) -> Vec<Effect> {
        vec![Effect::Copy {
            text: self.text,
            what: self.what,
        }]
    }

    /// `text`, shown in full in the message.
    pub(crate) fn shown(text: String) -> Copied {
        let shown = crate::text::sanitize(&text).replace(['\n', '\t'], " ");
        Copied {
            what: format!("copied {shown}"),
            text,
        }
    }
}

/// `n` and `word`, plural unless `n` is 1: `1 line`, `3 blocks`.
#[inline(never)]
pub(crate) fn count(n: usize, word: &str) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!("{n} {word}{s}")
}

/// The number of lines of `text`: at least one, a last line break ending
/// the last line.
#[inline(never)]
pub(crate) fn line_count(text: &str) -> usize {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.bytes().filter(|&b| b == b'\n').count() + 1
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// `n` labels, shortest first, none a prefix of another: single keys for
/// up to ten targets; beyond that the last single keys start two-key
/// labels, as few as needed (then three keys beyond a hundred, …).
pub(crate) fn labels(n: usize) -> Vec<String> {
    let k = HINT_KEYS.len();
    let word = |mut i: usize, len: usize| -> String {
        let mut chars = vec!['a'; len];
        for slot in chars.iter_mut().rev() {
            *slot = HINT_KEYS[i % k];
            i /= k;
        }
        chars.into_iter().collect()
    };
    // `short` words of length `len` exist; expanding one into `k` longer
    // ones adds `k - 1` labels.
    let (mut short, mut len) = (k, 1);
    while short.saturating_mul(k) < n {
        short = short.saturating_mul(k);
        len += 1;
    }
    let expand = n.saturating_sub(short).div_ceil(k - 1).min(short);
    let mut out: Vec<String> = (0..short - expand).map(|i| word(i, len)).collect();
    for i in short - expand..short {
        if out.len() >= n {
            break;
        }
        out.extend((0..k).map(|j| word(i * k + j, len + 1)));
    }
    out.truncate(n);
    out
}

/// What a block element is, for choosing targets and describing copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Heading,
    Para,
    Item,
    Quote,
    Code,
    Math,
    Table,
    Figure,
    /// A list (its items are elements of their own).
    List,
    /// Any other block (definition lists, details, front matter, HTML).
    Other,
}

/// A block of the document with where its content is.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Element<'d> {
    pub(crate) kind: Kind,
    /// The top-level block it is in.
    pub(crate) top: u32,
    /// Its content offsets in that block (see [`SrcPos`]).
    pub(crate) start: u32,
    pub(crate) end: u32,
    /// A top-level block itself.
    pub(crate) outer: bool,
    pub(crate) block: Option<&'d Block>,
    pub(crate) item: Option<&'d ListItem>,
}

/// Walks blocks counting content offsets as layout does.
struct Walker<'d, 'f> {
    doc: &'d Document,
    top: u32,
    off: u32,
    f: &'f mut dyn FnMut(Element<'d>),
}

impl<'d> Walker<'d, '_> {
    fn leaf(&mut self, len: usize) {
        self.off = self.off.saturating_add(to_u32(len));
    }

    fn blocks(&mut self, blocks: &'d [Block]) {
        for b in blocks {
            self.block(b, false);
        }
    }

    fn block(&mut self, b: &'d Block, outer: bool) {
        let doc = self.doc;
        let start = self.off;
        let kind = match b {
            Block::Para(t) => {
                self.leaf(t.text.len());
                Kind::Para
            }
            Block::Heading { text, .. } => {
                self.leaf(text.text.len());
                Kind::Heading
            }
            Block::Code(c) => {
                self.leaf(c.code.len());
                Kind::Code
            }
            Block::Math(m) => {
                self.leaf(m.tex.len());
                Kind::Math
            }
            Block::Quote { body, .. } => {
                self.blocks(body);
                Kind::Quote
            }
            Block::List(list) => {
                for item in &list.items {
                    let start = self.off;
                    self.blocks(&item.body);
                    (self.f)(Element {
                        kind: Kind::Item,
                        top: self.top,
                        start,
                        end: self.off,
                        outer: false,
                        block: None,
                        item: Some(item),
                    });
                }
                Kind::List
            }
            Block::Table(t) => {
                for cell in t.head.iter().chain(t.rows.iter().flatten()) {
                    self.leaf(cell.text.len());
                }
                Kind::Table
            }
            Block::Rule => return,
            Block::Figure(f) => {
                self.leaf(doc.caption(f).len());
                Kind::Figure
            }
            Block::DefList(items) => {
                for item in items {
                    self.leaf(item.term.text.len());
                    for def in &item.defs {
                        self.blocks(def);
                    }
                }
                Kind::Other
            }
            Block::Details { summary, body } => {
                self.leaf(summary.text.len());
                self.blocks(body);
                Kind::Other
            }
            Block::Align { body, .. } => {
                self.blocks(body);
                Kind::Other
            }
            Block::FrontMatter(fm) => {
                self.leaf(fm.raw.len());
                Kind::Other
            }
            Block::Html(raw) => {
                self.leaf(raw.len());
                Kind::Other
            }
            Block::FootnoteSection => {
                for f in &doc.footnotes {
                    self.blocks(&f.body);
                }
                return;
            }
        };
        (self.f)(Element {
            kind,
            top: self.top,
            start,
            end: self.off,
            outer,
            block: Some(b),
            item: None,
        });
    }
}

/// Call `f` on the elements of top-level blocks `tops` (nested ones
/// before the block holding them).
pub(crate) fn elements<'d>(doc: &'d Document, tops: Range<usize>, mut f: impl FnMut(Element<'d>)) {
    let mut w = Walker {
        doc,
        top: 0,
        off: 0,
        f: &mut f,
    };
    for (i, b) in doc
        .blocks
        .iter()
        .enumerate()
        .take(tops.end)
        .skip(tops.start)
    {
        w.top = to_u32(i);
        w.off = 0;
        w.block(b, true);
    }
}

/// The top-level block showing line `line`.
#[inline(never)]
pub(crate) fn block_at(layout: &Layout, line: usize) -> usize {
    let line = to_u32(line);
    layout
        .block_lines
        .partition_point(|r| r.end <= line)
        .min(layout.block_lines.len().saturating_sub(1))
}

/// The lines of `el`, without the blank lines after it.
#[inline(never)]
pub(crate) fn element_lines(layout: &Layout, el: &Element<'_>) -> Range<usize> {
    let Some(block) = layout.block_lines.get(el.top as usize) else {
        return 0..0;
    };
    let block = block.start as usize..block.end as usize;
    let (start, end) = if el.outer {
        (block.start, block.end)
    } else {
        let at = |off: u32| {
            let pos = SrcPos { top: el.top, off };
            layout.lines.partition_point(|l| l.pos < pos)
        };
        let start = at(el.start).clamp(block.start, block.end);
        (start, at(el.end).clamp(start, block.end))
    };
    let blank = |i: usize| {
        layout
            .lines
            .get(i)
            .is_none_or(|l| l.kind == LineKind::Blank)
    };
    let mut end = end;
    while end > start && blank(end - 1) {
        end -= 1;
    }
    start..end
}

/// The first column of line `line` that shows something.
#[inline(never)]
fn first_col(layout: &Layout, line: usize) -> u16 {
    let mut col = 0u16;
    for span in layout.line_spans(line) {
        let text = layout.span_text(span);
        let trimmed = text.trim_start();
        if !trimmed.is_empty() {
            let lead = text.len() - trimmed.len();
            return col.saturating_add(u16::try_from(lead).unwrap_or(0));
        }
        col = col.saturating_add(span.cols);
    }
    0
}

/// Open hints of `kind` on what is on screen (or say there is nothing).
pub(crate) fn open(state: &mut State, kind: HintKind) {
    let found = targets(state, kind);
    if found.is_empty() {
        state.say(match kind {
            HintKind::Links => "no links on screen",
            HintKind::Follow => "nothing to follow on screen",
            HintKind::Yank => "nothing to copy on screen",
            HintKind::Visual => "nothing to select on screen",
        });
        return;
    }
    let labels = label_targets(found);
    state.mode = Mode::Hints(Hints {
        kind,
        labels,
        typed: String::new(),
    });
}

/// The targets of hints of `kind`, unlabelled.
pub(crate) fn targets(state: &State, kind: HintKind) -> Vec<Target> {
    let layout = &state.layout;
    let doc = &state.page.doc;
    let top = state.top;
    let rows = state.view_rows();
    let shown = top..(top + rows).min(layout.len());
    let mut out = Vec::new();
    if shown.is_empty() {
        return out;
    }
    let base = state.page.base_dir();
    for (i, o) in state.derived.links.iter().enumerate() {
        let Some((line, col)) = links::first_visible_hit(layout, o, top, rows) else {
            continue;
        };
        let act = match kind {
            HintKind::Links | HintKind::Follow => Act::Link(i),
            HintKind::Yank => match links::copy_text(doc, &base, o) {
                Some(url) => Act::Copy(Copied::shown(url)),
                None => continue,
            },
            HintKind::Visual => continue,
        };
        out.push(Target { line, col, act });
    }
    if kind == HintKind::Links {
        return out;
    }
    if kind != HintKind::Visual {
        let mut seen: Option<SrcPos> = None;
        for hit in &layout.math_hits {
            let line = hit.line as usize;
            if !shown.contains(&line) || seen == Some(hit.pos) {
                continue;
            }
            seen = Some(hit.pos);
            let act = if kind == HintKind::Yank {
                let off = hit.pos.off;
                let tex = state
                    .page
                    .corpus()
                    .slice(hit.pos.top, off..off.saturating_add(hit.len));
                Act::Copy(Copied::lines(tex, "inline math (TeX)"))
            } else {
                Act::Jump(layout.line_at(hit.pos).min(line))
            };
            out.push(Target {
                line,
                col: hit.col,
                act,
            });
        }
    }
    let tops = block_at(layout, shown.start)..block_at(layout, shown.end - 1) + 1;
    elements(doc, tops, |el| {
        let wanted = match el.kind {
            Kind::Heading | Kind::Code | Kind::Math | Kind::Table | Kind::Figure => true,
            Kind::Item => kind != HintKind::Follow,
            Kind::Para | Kind::Quote | Kind::Other => kind != HintKind::Follow && el.outer,
            Kind::List => false,
        };
        if !wanted {
            return;
        }
        let lines = element_lines(layout, &el);
        if lines.is_empty() || lines.end <= shown.start || lines.start >= shown.end {
            return;
        }
        let line = lines.start.max(shown.start);
        let act = match kind {
            HintKind::Yank => Act::Copy(copy_of(state, &el)),
            HintKind::Visual => Act::Select(lines.clone()),
            _ => match el.block {
                Some(Block::Figure(f)) if layout.images.iter().any(|p| p.image == f.image) => {
                    Act::Zoom(f.image)
                }
                _ => Act::Jump(lines.start),
            },
        };
        // The column where the element starts: the leftmost of its first
        // lines (a code block's label line is right-aligned).
        let col = (line..lines.end.min(line + 3))
            .map(|l| first_col(layout, l))
            .min()
            .unwrap_or(0);
        out.push(Target { line, col, act });
    });
    out
}

/// Labels for `targets`, top to bottom and left to right; chips that
/// would cover each other on a line move right.
pub(crate) fn label_targets(found: Vec<Target>) -> Vec<(String, Target)> {
    let mut targets: Vec<Target> = Vec::with_capacity(found.len());
    for t in found {
        let at = targets.partition_point(|o| (o.line, o.col) <= (t.line, t.col));
        targets.insert(at, t);
    }
    let names = labels(targets.len());
    let mut free: Option<(usize, u16)> = None;
    for (name, t) in names.iter().zip(&mut targets) {
        if let Some((line, col)) = free
            && line == t.line
        {
            t.col = t.col.max(col);
        }
        let width = u16::try_from(name.chars().count()).unwrap_or(u16::MAX);
        free = Some((t.line, t.col.saturating_add(width)));
    }
    names.into_iter().zip(targets).collect()
}

/// The source of top-level block `top`, if the document knows it.
pub(crate) fn block_source(state: &State, top: u32) -> Option<&str> {
    let range = state.page.doc.block_source(BlockId(top))?;
    state.page.source.text.get(range)
}

/// The source of list item `item`, if known.
fn item_source<'s>(state: &'s State, item: &ListItem) -> Option<&'s str> {
    let r = &item.src;
    (r.start < r.end)
        .then(|| state.page.source.text.get(r.start as usize..r.end as usize))
        .flatten()
}

/// What yanking `el` copies.
#[inline(never)]
pub(crate) fn copy_of(state: &State, el: &Element<'_>) -> Copied {
    let doc = &state.page.doc;
    let source = || {
        el.outer
            .then(|| block_source(state, el.top))
            .flatten()
            .map(str::to_owned)
    };
    let text_of = |b: &Block| doc.blocks_text(std::slice::from_ref(b));
    match (el.kind, el.block, el.item) {
        (Kind::Item, _, Some(item)) => {
            let text =
                item_source(state, item).map_or_else(|| doc.blocks_text(&item.body), str::to_owned);
            Copied::lines(text, "list item")
        }
        (_, Some(Block::Code(c)), _) => Copied::lines(c.code.clone(), "code block"),
        (_, Some(Block::Math(m)), _) => Copied::lines(m.tex.to_string(), "math (TeX)"),
        (_, Some(Block::Table(t)), _) => {
            let text = source().unwrap_or_else(|| gfm_table(t));
            Copied::lines(text, "table")
        }
        (
            _,
            Some(Block::Heading {
                id: Some(h), text, ..
            }),
            _,
        ) => match doc.heading(*h) {
            Some(heading) => {
                let file = state
                    .page
                    .path()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Copied::shown(format!("{file}#{}", heading.slug))
            }
            None => Copied::shown(text.text.clone()),
        },
        (_, Some(Block::Figure(f)), _) => {
            let src = doc.image(f.image).map_or("", |i| &i.src);
            Copied::shown(src.to_owned())
        }
        (kind, Some(b), _) => {
            let what = match kind {
                Kind::Para => "paragraph",
                Kind::Quote => "quote",
                Kind::Heading => "heading",
                _ => "block",
            };
            Copied::lines(source().unwrap_or_else(|| text_of(b)), what)
        }
        (_, None, _) => Copied::lines(String::new(), "nothing"),
    }
}

/// A table as GitHub-flavoured Markdown, from its cells' text.
pub(crate) fn gfm_table(t: &Table) -> String {
    let row = |cells: &[crate::ir::Inlines]| {
        let cells: Vec<String> = cells
            .iter()
            .map(|c| c.text.replace('|', "\\|").replace('\n', " "))
            .collect();
        format!("| {} |", cells.join(" | "))
    };
    let delims: Vec<&str> = t
        .align
        .iter()
        .map(|a| match a {
            None => "---",
            Some(HAlign::Left) => ":---",
            Some(HAlign::Center) => ":---:",
            Some(HAlign::Right) => "---:",
        })
        .collect();
    let mut lines = vec![row(&t.head), format!("| {} |", delims.join(" | "))];
    lines.extend(t.rows.iter().map(|r| row(r)));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{ParseOptions, parse};

    #[test]
    fn labels_are_short_and_prefix_free() {
        assert_eq!(labels(3), ["a", "s", "d"]);
        assert_eq!(labels(10).last().map(String::as_str), Some(";"));
        assert!(labels(0).is_empty());
        let eleven = labels(11);
        assert_eq!(&eleven[..9], ["a", "s", "d", "f", "g", "h", "j", "k", "l"]);
        assert_eq!(&eleven[9..], [";a", ";s"]);
        for n in [1, 9, 10, 11, 19, 20, 55, 99, 100, 101, 250, 1001] {
            let l = labels(n);
            assert_eq!(l.len(), n, "{n}");
            for (i, a) in l.iter().enumerate() {
                for (j, b) in l.iter().enumerate() {
                    assert!(i == j || !b.starts_with(a.as_str()), "{a} {b} ({n})");
                }
            }
            // Shortest first, and no longer than needed.
            assert!(l.windows(2).all(|w| w[0].len() <= w[1].len()), "{n}");
        }
        assert!(labels(100).iter().all(|l| l.len() == 2));
        assert_eq!(labels(101).iter().filter(|l| l.len() == 3).count(), 2);
        assert_eq!(labels(101).iter().filter(|l| l.len() == 2).count(), 99);
    }

    #[test]
    fn chips_on_one_line_do_not_cover_each_other() {
        let t = |line, col| Target {
            line,
            col,
            act: Act::Jump(0),
        };
        let labelled = label_targets(vec![t(3, 0), t(1, 4), t(1, 4), t(1, 0)]);
        let at: Vec<(&str, usize, u16)> = labelled
            .iter()
            .map(|(l, t)| (l.as_str(), t.line, t.col))
            .collect();
        assert_eq!(at, [("a", 1, 0), ("s", 1, 4), ("d", 1, 5), ("f", 3, 0)]);
    }

    #[test]
    fn tables_as_markdown() {
        let doc = parse(
            "| a | b \\| c | d |\n|:--|:-:|--:|\n| 1 | `2` | x |\n",
            &ParseOptions::default(),
        );
        let Some(Block::Table(t)) = doc.blocks.first() else {
            panic!("{}", doc.dump());
        };
        assert_eq!(
            gfm_table(t),
            "| a | b \\| c | d |\n| :--- | :---: | ---: |\n| 1 | 2 | x |"
        );
    }
}
