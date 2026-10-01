//! pulldown-cmark events → owned IR ([`crate::ir::Document`]).
//!
//! [`parse`] runs pulldown-cmark with an explicit option set (never
//! `Options::all()`, which would switch on old-style footnotes, subscripts
//! and heading attributes behind the user's back), applies the math fixups
//! (`$` digit rule, `` $`…`$ ``, `\(…\)`/`\[…\]`), and feeds the events to
//! a total stack machine that never panics, whatever the event order.
//!
//! Besides CommonMark and GFM (tables, footnotes, strikethrough, task
//! lists, alerts), the parser handles front matter, definition lists, bare
//! URLs, a subset of HTML (formatting tags, `<img>`, `<picture>`, `<br>`,
//! `<details>`, `align=`, headings, anchors) and GitHub-compatible heading
//! slugs. Everything width- or colour-dependent is left to layout; math is
//! stored as TeX and typeset there.

mod builder;
mod display_math;
mod front_matter;
mod html;
mod inline;
mod linkify;
mod math_fixup;
mod slug;
mod slug_table;
mod target;

use pulldown_cmark::{Options, Parser};

pub(crate) use inline::url_break_points;
pub use slug::{Slugger, slug};

use std::ops::Range;

use crate::ir::{Block, Document, ListItem};
use crate::options::{HtmlMode, MarkdownOptions, RenderOptions};
use crate::source::Source;

/// What the parser accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseOptions {
    /// Dialect switches (math, bare-URL links, definition lists, smart
    /// punctuation).
    pub markdown: MarkdownOptions,
    /// Recognise `\(…\)` and `\[…\]` math delimiters.
    pub tex_delimiters: bool,
    /// How raw HTML is treated.
    pub html: HtmlMode,
    /// `# Heading {#id .class}` attributes. Off by default: GitHub shows
    /// them literally.
    pub heading_attributes: bool,
    /// Obsidian-style `[[wiki links]]`. Off by default (not GitHub syntax).
    pub wikilinks: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions::from(&RenderOptions::default())
    }
}

impl From<&RenderOptions> for ParseOptions {
    fn from(o: &RenderOptions) -> Self {
        ParseOptions {
            markdown: o.markdown,
            tex_delimiters: o.math.tex_delimiters,
            html: o.html,
            heading_attributes: false,
            wikilinks: false,
        }
    }
}

impl ParseOptions {
    /// The pulldown-cmark options for these settings.
    fn pulldown(&self) -> Options {
        let mut opts = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_GFM
            | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
            | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS;
        let optional = [
            (self.markdown.math, Options::ENABLE_MATH),
            (
                self.markdown.definition_lists,
                Options::ENABLE_DEFINITION_LIST,
            ),
            (
                self.markdown.smart_punctuation,
                Options::ENABLE_SMART_PUNCTUATION,
            ),
            (self.heading_attributes, Options::ENABLE_HEADING_ATTRIBUTES),
            (self.wikilinks, Options::ENABLE_WIKILINKS),
        ];
        for (on, flag) in optional {
            if on {
                opts |= flag;
            }
        }
        opts
    }
}

/// Parse Markdown text into a document. Total: any input gives a document.
///
/// The source ranges of the result ([`Document::block_src`],
/// [`ListItem::src`]) are byte offsets into `src`, without the whitespace
/// at their ends.
pub fn parse(src: &str, opts: &ParseOptions) -> Document {
    let math = opts.markdown.math;
    let tex = math && opts.tex_delimiters;
    // Multi-line display math becomes a fence first, so that its content
    // (a line of `=`, `- x`) cannot start a heading or a list.
    let (fenced, map) = display_math::fence(src, tex, math);
    let events = Parser::new_ext(&fenced, opts.pulldown()).into_offset_iter();
    let mut builder = builder::Builder::new(*opts);
    for (event, range) in math_fixup::MathFixup::new(events, &fenced, tex) {
        builder.event_at(event, range);
    }
    let mut doc = builder.finish();
    let fix = |r: &mut Range<u32>| *r = trim_end(src, map.range(r.clone()));
    doc.block_src.iter_mut().for_each(fix);
    for_items(&mut doc.blocks, &mut |item| fix(&mut item.src));
    for f in &mut doc.footnotes {
        for_items(&mut f.body, &mut |item| fix(&mut item.src));
    }
    doc
}

/// `r` without the whitespace (line breaks) at its end.
fn trim_end(src: &str, r: Range<u32>) -> Range<u32> {
    let text = src.get(r.start as usize..r.end as usize).unwrap_or("");
    let kept = text.trim_end_matches([' ', '\t', '\n', '\r']).len();
    r.start
        ..r.start
            .saturating_add(u32::try_from(kept).unwrap_or(u32::MAX))
}

/// Call `f` on every list item in `blocks`, nested ones too.
fn for_items(blocks: &mut [Block], f: &mut impl FnMut(&mut ListItem)) {
    for b in blocks {
        match b {
            Block::List(list) => {
                for item in &mut list.items {
                    f(item);
                    for_items(&mut item.body, f);
                }
            }
            Block::Quote { body, .. } | Block::Align { body, .. } | Block::Details { body, .. } => {
                for_items(body, f);
            }
            Block::DefList(items) => {
                for def in items.iter_mut().flat_map(|i| i.defs.iter_mut()) {
                    for_items(def, f);
                }
            }
            _ => {}
        }
    }
}

/// Parse a loaded source: like [`parse`], plus the base directory for
/// relative links and the source's decoding diagnostics.
pub fn parse_source(source: &Source, opts: &ParseOptions) -> Document {
    let mut doc = parse(&source.text, opts);
    doc.base_dir = source.base_dir();
    doc.diagnostics
        .splice(0..0, source.diagnostics.iter().cloned());
    doc
}

/// The `html` fuzz target's entry point (built with `--cfg fuzzing` only):
/// lexes `input` as the HTML lexer does and panics if it breaks one of its
/// invariants.
#[cfg(fuzzing)]
#[doc(hidden)]
pub fn fuzz_html(input: &str) {
    html::check_lexer(input);
}

#[cfg(test)]
mod tests;
