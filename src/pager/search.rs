//! Searching the document.
//!
//! The search runs over the *document's* text, not over the screen: a
//! [`Corpus`] holds the text of every leaf (paragraph, heading, table cell,
//! code block, …) in reading order, each tagged with the width-independent
//! position layout uses for its lines ([`SrcPos`]: the top-level block and
//! a byte offset into that block's content). So a phrase that wraps across
//! lines is still one match, matches survive resizes unchanged, and a
//! match is mapped to the screen through the lines' positions.
//!
//! Patterns are literal (memchr's `memmem`), with smart case: a pattern
//! with an uppercase letter is case-sensitive. Case folding keeps byte
//! offsets: a character is only lowercased when its lowercase form has the
//! same UTF-8 length, so folded and original text line up byte for byte.
//!
//! # From a match to the screen
//!
//! A layout line starts at its `pos`; the next line with a later position
//! bounds the part of the block's content the line shows. For prose, the
//! line's text *ends with* exactly that content (whatever prefix — a list
//! bullet, quote bars, centring — comes before it), so a match maps to
//! exact columns. Where layout only changes the spacing (the padding of
//! code pills and key caps, tabs expanded in code), the same holds once
//! whitespace is left out of both. Where layout transforms the text
//! (typeset math, superscript footnote numbers, table rows whose lines
//! show parts of several cells), the matched text is looked up in the line
//! instead, again ignoring whitespace: its whole cell, or the match with
//! some text around it from the same cell. A match that cannot be found
//! there is simply not highlighted on that line; marks never overlap.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::ops::Range;

use memchr::memmem;

use crate::config::SearchCase;
use crate::ir::{Block, Document, SrcPos};
use crate::layout::{Layout, LineKind};
use crate::options::FrontMatterMode;
use crate::render::Mark;
use crate::style::StylePatch;

/// Most matches kept for one pattern.
pub(crate) const MAX_MATCHES: usize = 100_000;

/// Lines scanned for the end of a line's content range.
const MAX_SCAN: usize = 256;

/// Bytes of context on each side of a match looked up in a line, tried
/// longest first. A line that shares its position with others showing
/// words (and is no table, code or math row) only gets the first: a short
/// context could find the text in a decoration (the footnotes heading has
/// "Footnotes" in it).
const CONTEXTS: [usize; 3] = [12, 6, 3];

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A match: block `top`, content bytes `start..end` (see [`SrcPos`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Match {
    pub(crate) top: u32,
    pub(crate) start: u32,
    pub(crate) end: u32,
}

impl Match {
    /// Where the match starts.
    pub(crate) fn pos(&self) -> SrcPos {
        SrcPos {
            top: self.top,
            off: self.start,
        }
    }
}

/// A piece of leaf text in the corpus.
#[derive(Clone, Copy, Debug)]
struct Leaf {
    /// Byte offset in [`Corpus::text`].
    start: u32,
    len: u32,
    /// Its position in the document.
    top: u32,
    off: u32,
}

/// The document's text for searching; see the module docs.
#[derive(Debug, Default)]
pub(crate) struct Corpus {
    /// Every leaf's text followed by `\n` (a pattern never contains one, so
    /// no match crosses from one leaf into the next).
    text: String,
    leaves: Vec<Leaf>,
    folded: OnceCell<String>,
}

/// Builds a [`Corpus`], counting content offsets exactly as layout does.
struct Walker<'a> {
    doc: &'a Document,
    hide_front_matter: bool,
    top: u32,
    off: u32,
    corpus: Corpus,
}

impl Walker<'_> {
    fn leaf(&mut self, s: &str) {
        let len = to_u32(s.len());
        if len > 0 {
            let start = to_u32(self.corpus.text.len());
            self.corpus.text.push_str(s);
            self.corpus.text.push('\n');
            self.corpus.leaves.push(Leaf {
                start,
                len,
                top: self.top,
                off: self.off,
            });
        }
        self.off = self.off.saturating_add(len);
    }

    fn blocks(&mut self, blocks: &[Block]) {
        for b in blocks {
            self.block(b);
        }
    }

    fn block(&mut self, b: &Block) {
        let doc = self.doc;
        match b {
            Block::Para(t) | Block::Heading { text: t, .. } => self.leaf(&t.text),
            Block::Code(c) => self.leaf(&c.code),
            Block::Math(m) => self.leaf(&m.tex),
            Block::Quote { body, .. } | Block::Align { body, .. } => self.blocks(body),
            Block::List(list) => {
                for item in &list.items {
                    self.blocks(&item.body);
                }
            }
            Block::Table(t) => {
                for cell in t.head.iter().chain(t.rows.iter().flatten()) {
                    self.leaf(&cell.text);
                }
            }
            Block::Rule => {}
            Block::Figure(f) => self.leaf(doc.caption(f)),
            Block::DefList(items) => {
                for item in items {
                    self.leaf(&item.term.text);
                    for def in &item.defs {
                        self.blocks(def);
                    }
                }
            }
            Block::Details { summary, body } => {
                self.leaf(&summary.text);
                self.blocks(body);
            }
            Block::FrontMatter(fm) if self.hide_front_matter => {
                self.off = self.off.saturating_add(to_u32(fm.raw.len()));
            }
            Block::FrontMatter(fm) => self.leaf(&fm.raw),
            Block::Html(raw) => self.leaf(raw),
            Block::FootnoteSection => {
                for f in &doc.footnotes {
                    self.blocks(&f.body);
                }
            }
        }
    }
}

impl Corpus {
    /// The corpus of a document (hidden front matter is left out).
    pub(crate) fn new(doc: &Document, front_matter: FrontMatterMode) -> Corpus {
        let mut w = Walker {
            doc,
            hide_front_matter: front_matter == FrontMatterMode::Hide,
            top: 0,
            off: 0,
            corpus: Corpus::default(),
        };
        for (i, b) in doc.blocks.iter().enumerate() {
            w.top = to_u32(i);
            w.off = 0;
            w.block(b);
        }
        w.corpus
    }

    fn folded(&self) -> &str {
        self.folded.get_or_init(|| fold(&self.text).into_owned())
    }

    /// Every occurrence of `pattern`, in document order, at most `limit`;
    /// the flag says whether more were left out.
    pub(crate) fn find(&self, pattern: &str, sensitive: bool, limit: usize) -> (Vec<Match>, bool) {
        if pattern.is_empty() || pattern.contains('\n') {
            return (Vec::new(), false);
        }
        let (hay, needle): (&str, Cow<'_, str>) = if sensitive {
            (&self.text, Cow::Borrowed(pattern))
        } else {
            (self.folded(), fold(pattern))
        };
        let finder = memmem::Finder::new(needle.as_bytes());
        let len = needle.len();
        let mut out = Vec::new();
        let mut leaf = 0usize;
        for pos in finder.find_iter(hay.as_bytes()) {
            if out.len() >= limit {
                return (out, true);
            }
            while self
                .leaves
                .get(leaf + 1)
                .is_some_and(|l| l.start as usize <= pos)
            {
                leaf += 1;
            }
            let Some(l) = self.leaves.get(leaf) else {
                continue;
            };
            let rel = pos.saturating_sub(l.start as usize);
            if pos < l.start as usize || rel + len > l.len as usize {
                continue;
            }
            let start = l.off.saturating_add(to_u32(rel));
            out.push(Match {
                top: l.top,
                start,
                end: start.saturating_add(to_u32(len)),
            });
        }
        (out, false)
    }

    /// The content of block `top` in `range`, leaves joined without
    /// separators (clamped to character boundaries).
    pub(crate) fn slice(&self, top: u32, range: Range<u32>) -> String {
        let first = self
            .leaves
            .partition_point(|l| (l.top, l.off.saturating_add(l.len)) <= (top, range.start));
        let mut out = String::new();
        for l in self.leaves.get(first..).unwrap_or(&[]) {
            if l.top != top || l.off >= range.end {
                break;
            }
            let a = range.start.max(l.off) - l.off;
            let b = range.end.min(l.off.saturating_add(l.len)) - l.off;
            let from = l.start as usize + a as usize;
            let to = l.start as usize + b as usize;
            out.push_str(safe_slice(&self.text, from, to));
        }
        out
    }

    /// The content offsets of the leaf of block `top` that holds offset
    /// `off` (a table cell, a paragraph, …).
    pub(crate) fn leaf_at(&self, top: u32, off: u32) -> Option<Range<u32>> {
        let i = self
            .leaves
            .partition_point(|l| (l.top, l.off) <= (top, off))
            .checked_sub(1)?;
        let l = self.leaves.get(i)?;
        let end = l.off.saturating_add(l.len);
        (l.top == top && off < end).then_some(l.off..end)
    }

    /// The content offset where footnote `index` starts in the footnote
    /// section, as layout counts it.
    pub(crate) fn footnote_offset(doc: &Document, index: usize) -> u32 {
        let mut w = Walker {
            doc,
            hide_front_matter: false,
            top: 0,
            off: 0,
            corpus: Corpus::default(),
        };
        for f in doc.footnotes.iter().take(index) {
            w.blocks(&f.body);
        }
        w.off
    }
}

/// `s[from..to]`, with both ends moved back to character boundaries.
fn safe_slice(s: &str, from: usize, to: usize) -> &str {
    let floor = |mut i: usize| {
        i = i.min(s.len());
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let (a, b) = (floor(from), floor(to));
    s.get(a..b.max(a)).unwrap_or("")
}

/// Lowercase `s` without changing any byte offset (see the module docs).
pub(crate) fn fold(s: &str) -> Cow<'_, str> {
    if s.is_ascii() {
        return if s.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(s.to_ascii_lowercase())
        } else {
            Cow::Borrowed(s)
        };
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let mut lower = c.to_lowercase();
        match (lower.next(), lower.next()) {
            (Some(l), None) if l.len_utf8() == c.len_utf8() => out.push(l),
            _ => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// `s` as a search compares it: folded unless the search is
/// case-sensitive.
fn fold_for(s: &str, sensitive: bool) -> Cow<'_, str> {
    if sensitive { Cow::Borrowed(s) } else { fold(s) }
}

/// Whether `pattern` is searched case-sensitively.
pub(crate) fn case_sensitive(pattern: &str, mode: SearchCase) -> bool {
    match mode {
        SearchCase::Sensitive => true,
        SearchCase::Insensitive => false,
        SearchCase::Smart => pattern.chars().any(char::is_uppercase),
    }
}

/// A search: its pattern and matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Search {
    /// The pattern as typed.
    pub(crate) pattern: String,
    /// Started with `?`: `n` goes up.
    pub(crate) backward: bool,
    pub(crate) sensitive: bool,
    pub(crate) matches: Vec<Match>,
    /// The match `n`/`N` moved to last.
    pub(crate) current: Option<usize>,
    /// More than [`MAX_MATCHES`] matches exist.
    pub(crate) capped: bool,
}

impl Search {
    /// Search `corpus` for `pattern`.
    pub(crate) fn run(corpus: &Corpus, pattern: &str, backward: bool, mode: SearchCase) -> Search {
        let sensitive = case_sensitive(pattern, mode);
        let (matches, capped) = corpus.find(pattern, sensitive, MAX_MATCHES);
        Search {
            pattern: pattern.to_owned(),
            backward,
            sensitive,
            matches,
            current: None,
            capped,
        }
    }

    /// The first match at or after `pos` (wrapping to the first), or the
    /// last one before it when `before`.
    pub(crate) fn nearest(&self, pos: SrcPos, before: bool) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let key = (pos.top, pos.off);
        let i = self.matches.partition_point(|m| (m.top, m.start) < key);
        if before {
            Some(i.checked_sub(1).unwrap_or(self.matches.len() - 1))
        } else if i < self.matches.len() {
            Some(i)
        } else {
            Some(0)
        }
    }
}

/// The text of a layout line with, for each span, where its text is in the
/// line and in [`Layout::text`].
pub(crate) struct LineText {
    pub(crate) text: String,
    /// `(start in text, offset in the arena, length)` per span.
    pieces: Vec<(u32, u32, u32)>,
}

impl LineText {
    /// Line `index` of `layout`.
    pub(crate) fn new(layout: &Layout, index: usize) -> LineText {
        let mut text = String::new();
        let mut pieces = Vec::new();
        for span in layout.line_spans(index) {
            let s = layout.span_text(span);
            pieces.push((to_u32(text.len()), span.off, to_u32(s.len())));
            text.push_str(s);
        }
        LineText { text, pieces }
    }

    /// Marks over the arena for the bytes `range` of the line's text.
    pub(crate) fn marks(&self, range: Range<usize>, patch: StylePatch, out: &mut Vec<Mark>) {
        let (a, b) = (to_u32(range.start), to_u32(range.end));
        for &(start, arena, len) in &self.pieces {
            let end = start.saturating_add(len);
            let (from, to) = (a.max(start), b.min(end));
            if from < to {
                let off = arena.saturating_add(from - start);
                out.push(Mark {
                    range: off..off.saturating_add(to - from),
                    patch,
                });
            }
        }
    }
}

/// The content range line `index` shows: from its position to the next
/// later position in the same block (`u32::MAX` when the block ends).
fn line_range(layout: &Layout, index: usize) -> Option<(u32, Range<u32>)> {
    let pos = layout.lines.get(index)?.pos;
    let next = layout
        .lines
        .iter()
        .skip(index + 1)
        .take(MAX_SCAN)
        .map(|l| l.pos)
        .find(|p| *p > pos);
    let end = match next {
        Some(p) if p.top == pos.top => p.off,
        _ => u32::MAX,
    };
    Some((pos.top, pos.off..end))
}

/// Whether no other line with the position of line `index` shows words
/// (blank lines, rules and borders may share it): then the line is no
/// decoration of the content (as a footnotes heading or an alert title
/// sharing the position of the text after it are).
fn alone(layout: &Layout, index: usize) -> bool {
    let Some(pos) = layout.lines.get(index).map(|l| l.pos) else {
        return false;
    };
    let words = |i: usize| {
        layout
            .line_spans(i)
            .iter()
            .any(|s| layout.span_text(s).chars().any(char::is_alphanumeric))
    };
    (layout.line_at(pos)..layout.len())
        .take_while(|&i| layout.lines.get(i).is_some_and(|l| l.pos == pos))
        .take(MAX_SCAN)
        .all(|i| i == index || !words(i))
}

/// Byte offsets of the non-overlapping occurrences of `needle` in `hay`.
fn occurrences(hay: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    memmem::find_iter(hay.as_bytes(), needle.as_bytes()).collect()
}

/// The last `n` bytes of `s` or fewer (whole characters), after its last
/// line break.
fn tail(s: &str, n: usize) -> &str {
    let mut from = s.len().saturating_sub(n);
    while from < s.len() && !s.is_char_boundary(from) {
        from += 1;
    }
    s.get(from..)
        .unwrap_or("")
        .rsplit('\n')
        .next()
        .unwrap_or("")
}

/// The first `n` bytes of `s` or fewer (whole characters), before its
/// first line break.
fn head(s: &str, n: usize) -> &str {
    s.split('\n')
        .next()
        .map_or("", |line| safe_slice(line, 0, n))
}

/// Where `needle` is in `hay`, if it is there exactly once.
fn unique(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let first = memmem::find(hay.as_bytes(), needle.as_bytes())?;
    let rest = hay.as_bytes().get(first + 1..).unwrap_or_default();
    memmem::find(rest, needle.as_bytes())
        .is_none()
        .then_some(first)
}

/// Whether layout may add, drop or change `c` where it shows text:
/// whitespace (the padding of code pills and key caps, tabs expanded in
/// code, the space at a wrap) and soft hyphens.
fn squeezable(c: char) -> bool {
    c.is_whitespace() || c == '\u{ad}'
}

/// A line's text without [`squeezable`] characters, and where each byte
/// that is left was: a line whose spacing differs from its content (a code
/// pill's padding) still lines up with it this way.
struct Squeezed {
    text: String,
    /// The offset in the original text of each byte of `text`.
    at: Vec<u32>,
}

impl Squeezed {
    fn new(s: &str) -> Squeezed {
        let mut text = String::with_capacity(s.len());
        let mut at = Vec::with_capacity(s.len());
        for (i, c) in s.char_indices() {
            if !squeezable(c) {
                text.push(c);
                at.extend((i..i + c.len_utf8()).map(to_u32));
            }
        }
        Squeezed { text, at }
    }

    /// Bytes of the squeezed text that come from original bytes before
    /// `i`.
    fn offset(&self, i: usize) -> usize {
        self.at.partition_point(|&o| (o as usize) < i)
    }

    /// The bytes of the original text behind bytes `range` of the squeezed
    /// one (`None` when `range` is empty).
    fn original(&self, range: Range<usize>) -> Option<Range<usize>> {
        let first = *self.at.get(range.start)?;
        let last = *self.at.get(range.end.checked_sub(1)?)?;
        (range.start < range.end).then(|| first as usize..last as usize + 1)
    }
}

/// Finds a match in a line whose text is not the content verbatim (nor
/// with only its spacing changed).
struct Lookup<'a> {
    /// The content the line shows.
    content: &'a str,
    /// `content` folded when the search is, squeezed.
    squeezed: &'a Squeezed,
    /// The line's text, the same way.
    line: &'a Squeezed,
    /// The bytes of `content` from the leaf holding the match: one cell
    /// when the line belongs to a table row (whose lines all show parts of
    /// every cell), else all of it.
    cell: Range<usize>,
    /// Whether the matched text may be looked up on its own (table, code
    /// and display math rows, whose content is laid out in pieces);
    /// elsewhere some context must come along, so that a decoration
    /// sharing the line's position — a code block's label, the footnotes
    /// heading, an alert's title — is not mistaken for the match.
    bare: bool,
    /// A code row: it shows its content from the start.
    code: bool,
    /// No other line with this line's position shows words, so it is no
    /// decoration.
    alone: bool,
}

impl Lookup<'_> {
    /// Content bytes `a..b` as they are compared with the line: folded
    /// when the search is, squeezed.
    fn key(&self, a: usize, b: usize) -> &str {
        let (x, y) = (self.squeezed.offset(a), self.squeezed.offset(b));
        self.squeezed.text.get(x..y.max(x)).unwrap_or("")
    }

    /// Where content bytes `a..b` are in the line.
    fn find(&self, a: usize, b: usize) -> Option<Range<usize>> {
        let key = self.key(a, b);
        if key.is_empty() {
            return None;
        }
        let cell = self.cell.start.min(a)..self.cell.end.max(b);
        let single = cell == (0..self.content.len());
        // The whole cell, where the line shows it once: exact. (One of the
        // cells of a table row; or all the content, on a line that cannot
        // be a decoration showing the same words.)
        let whole = self.key(cell.start, cell.end);
        if let Some(at) = unique(&self.line.text, whole).filter(|_| !single || self.alone) {
            let start = at + self.key(cell.start, a).len();
            return self.line.original(start..start + key.len());
        }
        // The matched text with some of the cell around it, shorter and
        // shorter (where layout changed the text nearby: typeset math, link
        // numbers, a wrapped table cell) unless decorations share the line.
        let before = safe_slice(self.content, cell.start, a);
        let after = safe_slice(self.content, b, cell.end);
        let lengths = if self.bare || self.alone {
            &CONTEXTS[..]
        } else {
            &CONTEXTS[..1]
        };
        let mut tried = ("", "");
        for &n in lengths {
            let pre = self.key(a - tail(before, n).len(), a);
            let post = self.key(b, b + head(after, n).len());
            if (pre.is_empty() && post.is_empty()) || (pre, post) == tried {
                continue;
            }
            for (pre, post) in [(pre, post), ("", post), (pre, "")] {
                if pre.is_empty() && post.is_empty() {
                    continue;
                }
                let needle = format!("{pre}{key}{post}");
                if let Some(at) = memmem::find(self.line.text.as_bytes(), needle.as_bytes()) {
                    let start = at + pre.len();
                    return self.line.original(start..start + key.len());
                }
            }
            tried = (pre, post);
        }
        if !self.bare && !self.alone {
            return None;
        }
        // The n-th occurrence in the content is the n-th in the line when
        // the line shows all of the content's occurrences (a table row that
        // fits on one line, the row of display math with the word, a line
        // where layout changed the text around a match), or a code line
        // shows its content from the start (clipped or not).
        let found = occurrences(&self.line.text, key);
        let all = occurrences(&self.squeezed.text, key).len();
        if found.len() != all && !(self.code && single) {
            return None;
        }
        let nth = occurrences(self.key(0, b), key).len().checked_sub(1)?;
        let at = *found.get(nth)?;
        self.line.original(at..at + key.len())
    }
}

/// Highlight marks for line `index`: `matches` (sorted, of the document
/// `corpus` was made from) that touch the line, the `current` one with
/// `current_patch`, the others with `patch`.
pub(crate) fn line_marks(
    corpus: &Corpus,
    layout: &Layout,
    index: usize,
    search: &Search,
    patch: StylePatch,
    current_patch: StylePatch,
) -> Vec<Mark> {
    let mut out = Vec::new();
    let Some((top, range)) = line_range(layout, index) else {
        return out;
    };
    let matches = &search.matches;
    let first = matches.partition_point(|m| (m.top, m.end) <= (top, range.start));
    let hits: Vec<(usize, &Match)> = matches
        .iter()
        .enumerate()
        .skip(first)
        .take_while(|(_, m)| m.top == top && m.start < range.end)
        .collect();
    if hits.is_empty() {
        return out;
    }
    let kind = layout.lines.get(index).map(|l| l.kind);
    let line = LineText::new(layout, index);
    let content = corpus.slice(top, range.clone());
    let shown = content.trim_end();
    let exact = !shown.is_empty() && line.text.ends_with(shown);
    let content_start = line.text.len().saturating_sub(shown.len());
    // Otherwise the line may still end with its content once spacing is
    // left out (code pills, key caps, tabs): then offsets map through the
    // squeezed texts; failing that, the matched text is looked up.
    let squeezed = (!exact).then(|| {
        let line = Squeezed::new(&fold_for(&line.text, search.sensitive));
        let content = Squeezed::new(&fold_for(&content, search.sensitive));
        let aligned = (!content.text.is_empty() && line.text.ends_with(&content.text))
            .then(|| line.text.len() - content.text.len());
        (line, content, aligned)
    });
    let bare = matches!(
        kind,
        Some(LineKind::Table | LineKind::Code { .. } | LineKind::Math)
    );
    let code = matches!(kind, Some(LineKind::Code { .. }));
    let mut lone = None;
    for (i, m) in hits {
        let p = if Some(i) == search.current {
            current_patch
        } else {
            patch
        };
        let a = m.start.max(range.start) - range.start;
        let b = m.end.min(range.end) - range.start;
        let (a, b) = (a as usize, b as usize);
        let Some((squeezed, squeezed_content, aligned)) = &squeezed else {
            let b = b.min(shown.len());
            if a < b {
                line.marks(content_start + a..content_start + b, p, &mut out);
            }
            continue;
        };
        let at = match aligned {
            Some(base) => squeezed
                .original(base + squeezed_content.offset(a)..base + squeezed_content.offset(b)),
            // Transformed text: look the matched part up in the line.
            None => Lookup {
                content: &content,
                squeezed: squeezed_content,
                line: squeezed,
                cell: corpus.leaf_at(top, m.start).map_or(0..content.len(), |r| {
                    let start = r.start.max(range.start) - range.start;
                    let end = r.end.min(range.end).saturating_sub(range.start);
                    start as usize..end as usize
                }),
                bare,
                code,
                alone: *lone.get_or_insert_with(|| alone(layout, index)),
            }
            .find(a, b),
        };
        if let Some(r) = at {
            line.marks(r, p, &mut out);
        }
    }
    out.sort_by_key(|m| m.range.start);
    // Marks must not overlap: should two matches have been found at the
    // same text, the first one keeps it.
    let mut end = 0;
    out.retain(|m| {
        let keep = m.range.start >= end;
        if keep {
            end = m.range.end;
        }
        keep
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::PlainHighlighter;
    use crate::layout::{NoImages, layout};
    use crate::options::RenderOptions;
    use crate::parse::{ParseOptions, parse};
    use crate::term::Caps;
    use crate::theme::Theme;

    fn doc(md: &str) -> Document {
        parse(md, &ParseOptions::default())
    }

    fn lay(d: &Document, width: u16) -> Layout {
        layout(
            d,
            width,
            &Theme::test(),
            &Caps::full(),
            &RenderOptions::default(),
            &PlainHighlighter,
            &NoImages,
        )
    }

    #[test]
    fn folding_keeps_offsets() {
        assert_eq!(fold("Hello"), "hello");
        assert!(matches!(fold("plain"), Cow::Borrowed(_)));
        assert_eq!(fold("ÄÖÜ Σ"), "äöü σ");
        // U+0130 lowercases to two characters: kept as is.
        assert_eq!(fold("İx"), "İx");
        for s in ["ẞig", "ǅ", "ΑΒΓ", "日本"] {
            assert_eq!(fold(s).len(), s.len(), "{s}");
        }
    }

    #[test]
    fn smart_case() {
        assert!(!case_sensitive("rust", SearchCase::Smart));
        assert!(case_sensitive("Rust", SearchCase::Smart));
        assert!(case_sensitive("rust", SearchCase::Sensitive));
        assert!(!case_sensitive("Rust", SearchCase::Insensitive));
    }

    #[test]
    fn matches_carry_layout_positions() {
        let d = doc("# Title\n\nSome text and more text.\n\n- item text\n- other");
        let c = Corpus::new(&d, FrontMatterMode::Card);
        let (m, capped) = c.find("text", false, 100);
        assert!(!capped);
        assert_eq!(
            m,
            [
                Match {
                    top: 1,
                    start: 5,
                    end: 9
                },
                Match {
                    top: 1,
                    start: 19,
                    end: 23
                },
                Match {
                    top: 2,
                    start: 5,
                    end: 9
                },
            ]
        );
        assert_eq!(c.slice(2, 0..9), "item text");
        assert_eq!(c.slice(2, 5..14), "textother", "leaves joined");
        assert_eq!(c.find("TITLE", false, 10).0.len(), 1, "case folded");
        assert!(c.find("TITLE", true, 10).0.is_empty());
        assert!(
            c.find("textitem", false, 10).0.is_empty(),
            "no match across leaves"
        );
        assert!(c.find("", false, 10).0.is_empty());
        let (few, capped) = c.find("t", false, 2);
        assert_eq!(few.len(), 2);
        assert!(capped);
    }

    /// Every line's content range agrees with the corpus: prose lines end
    /// with exactly the content they show, whatever their prefix.
    #[test]
    fn corpus_offsets_agree_with_layout() {
        let md = "# Heading one\n\nA paragraph of plain words that wraps over several \
                  lines at this width.\n\n- a list item that also wraps over more than \
                  one line\n- short\n\n> a quoted paragraph that wraps as well, over two \
                  or three lines\n\n1. numbered item that wraps around the width too\n";
        let d = doc(md);
        let c = Corpus::new(&d, FrontMatterMode::Card);
        for width in [20, 30, 50, 80] {
            let l = lay(&d, width);
            for i in 0..l.len() {
                let text = l.line_text(i);
                if !text.chars().any(|ch| ch.is_ascii_alphabetic()) {
                    continue;
                }
                let (top, range) = line_range(&l, i).unwrap();
                let content = c.slice(top, range);
                assert!(
                    text.ends_with(content.trim_end()),
                    "{width}: line {i} {text:?} vs {content:?}"
                );
            }
        }
        // The kitchen sink: every line's range lies within its block.
        let md = include_str!("../../tests/fixtures/md/kitchen-sink.md");
        let d = doc(md);
        let c = Corpus::new(&d, FrontMatterMode::Card);
        let l = lay(&d, 40);
        let mut exact = 0;
        for i in 0..l.len() {
            let (top, range) = line_range(&l, i).unwrap();
            let content = c.slice(top, range);
            if !content.trim_end().is_empty() && l.line_text(i).ends_with(content.trim_end()) {
                exact += 1;
            }
        }
        assert!(exact > 20, "{exact}");
    }

    #[test]
    fn nearest_match_wraps() {
        let d = doc("a x\n\nb x\n\nc x");
        let c = Corpus::new(&d, FrontMatterMode::Card);
        let s = Search::run(&c, "x", false, SearchCase::Smart);
        assert_eq!(s.matches.len(), 3);
        let at = |top, off| SrcPos { top, off };
        assert_eq!(s.nearest(at(1, 0), false), Some(1));
        assert_eq!(s.nearest(at(2, 3), false), Some(0), "wraps to the first");
        assert_eq!(s.nearest(at(1, 0), true), Some(0));
        assert_eq!(s.nearest(at(0, 0), true), Some(2), "wraps to the last");
        let empty = Search::run(&c, "zzz", false, SearchCase::Smart);
        assert_eq!(empty.nearest(at(0, 0), false), None);
    }

    fn marked(l: &Layout, marks: &[Mark]) -> Vec<String> {
        marks
            .iter()
            .map(|m| {
                l.text
                    .get(m.range.start as usize..m.range.end as usize)
                    .unwrap_or("?")
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn matches_across_a_wrap_are_marked_on_both_lines() {
        let d = doc("> the quick brown fox jumps over the lazy dog");
        let l = lay(&d, 20);
        let c = Corpus::new(&d, FrontMatterMode::Card);
        let mut s = Search::run(&c, "brown fox jumps", false, SearchCase::Smart);
        assert_eq!(s.matches.len(), 1);
        s.current = Some(0);
        let cur = StylePatch {
            set: crate::style::Attrs::BOLD,
            ..StylePatch::default()
        };
        let mut all = Vec::new();
        for i in 0..l.len() {
            let marks = line_marks(&c, &l, i, &s, StylePatch::default(), cur);
            assert!(marks.iter().all(|m| m.patch == cur));
            all.extend(marked(&l, &marks));
        }
        assert_eq!(all.concat().replace(' ', ""), "brownfoxjumps", "{all:?}");
        assert!(all.len() >= 2, "split over lines: {all:?}");
    }

    #[test]
    fn transformed_lines_fall_back_to_finding_the_text() {
        let d = doc("| a | b |\n|---|---|\n| one | two words |\n| three | two |");
        let l = lay(&d, 40);
        let c = Corpus::new(&d, FrontMatterMode::Card);
        let s = Search::run(&c, "two", false, SearchCase::Smart);
        assert_eq!(s.matches.len(), 2);
        let mut found = Vec::new();
        for i in 0..l.len() {
            let marks = line_marks(&c, &l, i, &s, StylePatch::default(), StylePatch::default());
            for m in marked(&l, &marks) {
                found.push((i, m));
            }
        }
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|(_, t)| t == "two"));
        assert_ne!(found[0].0, found[1].0, "one per row");
    }

    /// The text of every mark `pattern` gets in `md` laid out at `width`.
    fn marked_texts(md: &str, pattern: &str, width: u16) -> Vec<String> {
        let d = doc(md);
        let l = lay(&d, width);
        let c = Corpus::new(&d, FrontMatterMode::Card);
        let s = Search::run(&c, pattern, false, SearchCase::Smart);
        (0..l.len())
            .flat_map(|i| {
                let marks = line_marks(&c, &l, i, &s, StylePatch::default(), StylePatch::default());
                marked(&l, &marks)
            })
            .collect()
    }

    #[test]
    fn spacing_changed_by_layout_does_not_hide_matches() {
        // Code pills and key caps are padded, tabs in code expanded: every
        // match is still marked, on exactly its own text.
        for (md, pattern, count) in [
            ("Run `cargo install emde` to install it.", "install", 2),
            ("Use `cargo` here and cargo there.", "cargo", 2),
            ("Some `a` b `c` d `e` f.", "d", 1),
            ("Press <kbd>Ctrl</kbd> + <kbd>C</kbd> to copy.", "ctrl", 1),
            ("Span `a b` and more.", "a b", 1),
            ("```\n\tfn main() {}\n\t\tlet x = 1;\n```", "let x", 1),
        ] {
            let found = marked_texts(md, pattern, 80);
            assert_eq!(found.len(), count, "{md:?} / {pattern:?}: {found:?}");
            assert!(
                found.iter().all(|t| fold(t) == pattern),
                "{md:?} / {pattern:?}: {found:?}"
            );
        }
        // A match running out of a pill covers the padding in between.
        let found = marked_texts("Use `cargo` here.", "cargo here", 80);
        let joined: String = found.concat();
        assert_eq!(Squeezed::new(&joined).text, "cargohere", "{found:?}");
    }

    /// Every mark `pattern` gets in `md` at `width`: the line and the text,
    /// checking that the marks of each line are sorted and do not overlap.
    fn marked_lines(md: &str, pattern: &str, width: u16) -> Vec<(usize, String)> {
        let d = doc(md);
        let l = lay(&d, width);
        let c = Corpus::new(&d, FrontMatterMode::Card);
        let s = Search::run(&c, pattern, false, SearchCase::Smart);
        let mut out = Vec::new();
        for i in 0..l.len() {
            let marks = line_marks(&c, &l, i, &s, StylePatch::default(), StylePatch::default());
            for pair in marks.windows(2) {
                assert!(pair[0].range.end <= pair[1].range.start, "{marks:?}");
            }
            out.extend(marked(&l, &marks).into_iter().map(|t| (i, t)));
        }
        out
    }

    #[test]
    fn table_cells_are_found_cell_by_cell() {
        // A cell wrapped over several lines (all with the row's position),
        // with "the" three times in it: each one marked once at most, on
        // its own line, never piled up on one.
        let md = "| a | b |\n|---|---|\n| x | Another cell that wraps, because the \
                  table is wider than the terminal |";
        let found = marked_lines(md, "the", 40);
        let lines: Vec<usize> = found.iter().map(|(i, _)| *i).collect();
        let mut unique = lines.clone();
        unique.dedup();
        assert_eq!(lines, unique, "one mark per line: {found:?}");
        assert!(found.len() >= 2, "{found:?}");
        // Narrow tables become records ("a: one"): each match is marked on
        // the record line showing its cell.
        let md = "| a | b | c | d | e | f | g |\n|---|---|---|---|---|---|---|\n\
                  | one | two | three | four | five | six | seven |";
        let d = doc(md);
        let l = lay(&d, 20);
        let found = marked_lines(md, "three", 20);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(l.line_text(found[0].0).ends_with("three"));
    }

    #[test]
    fn text_next_to_transformed_text_is_marked() {
        for (md, pattern) in [
            // Typeset math right after the match.
            ("Inline: $e^{i\\pi} + 1 = 0$, and more.", "inline"),
            // A footnote number shown as a superscript.
            ("Another one.[^n] And so on.\n\n[^n]: Note.", "one"),
            // A word that display math shows as it is.
            (
                "$$\nf(x) = \\begin{cases} 1 & x > 0 \\\\ 0 & \\text{otherwise} \\end{cases}\n$$",
                "otherwise",
            ),
        ] {
            let found = marked_lines(md, pattern, 60);
            assert_eq!(found.len(), 1, "{md:?}: {found:?}");
            assert_eq!(fold(&found[0].1), pattern, "{md:?}: {found:?}");
        }
    }

    #[test]
    fn squeezed_offsets_map_back() {
        let s = Squeezed::new(" a\tbé c ");
        assert_eq!(s.text, "abéc");
        assert_eq!(s.original(0..1), Some(1..2));
        assert_eq!(s.original(1..4), Some(3..6), "b and the two bytes of é");
        assert_eq!(s.original(2..2), None);
        assert_eq!(s.original(4..9), None, "out of range");
        assert_eq!(s.offset(4), 2, "a and b come before byte 4");
        assert_eq!(s.offset(0), 0);
        assert_eq!(s.offset(99), "abéc".len());
        assert_eq!(Squeezed::new("soft\u{ad}hy phen").text, "softhyphen");
    }

    #[test]
    fn decorations_sharing_a_position_are_not_marked() {
        // The footnotes heading has the first note's position; an alert's
        // title has its first paragraph's.
        let d = doc("A[^1].\n\n> [!NOTE]\n> Note this.\n\n[^1]: The footnote text.");
        let l = lay(&d, 60);
        let c = Corpus::new(&d, FrontMatterMode::Card);
        for pattern in ["foot", "note"] {
            let mut s = Search::run(&c, pattern, false, SearchCase::Smart);
            s.current = Some(0);
            let marked_lines: Vec<String> = (0..l.len())
                .filter(|&i| {
                    !line_marks(&c, &l, i, &s, StylePatch::default(), StylePatch::default())
                        .is_empty()
                })
                .map(|i| l.line_text(i))
                .collect();
            assert_eq!(
                marked_lines.len(),
                s.matches.len(),
                "{pattern}: {marked_lines:?}"
            );
            assert!(
                !marked_lines.iter().any(|t| t.contains("Footnotes")),
                "{marked_lines:?}"
            );
        }
    }

    #[test]
    fn footnote_offsets() {
        let d = doc("A[^1] b[^2].\n\n[^1]: First note.\n[^2]: Second.");
        assert_eq!(Corpus::footnote_offset(&d, 0), 0);
        assert_eq!(Corpus::footnote_offset(&d, 1), "First note.".len() as u32);
    }

    #[test]
    fn hidden_front_matter_is_not_searched() {
        let d = doc("---\ntitle: Secret\n---\n\nBody");
        let shown = Corpus::new(&d, FrontMatterMode::Card);
        assert_eq!(shown.find("secret", false, 10).0.len(), 1);
        let hidden = Corpus::new(&d, FrontMatterMode::Hide);
        assert!(hidden.find("secret", false, 10).0.is_empty());
        assert_eq!(hidden.find("body", false, 10).0.len(), 1);
    }
}
