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

use crate::ir::Document;
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
pub fn parse(src: &str, opts: &ParseOptions) -> Document {
    let events = Parser::new_ext(src, opts.pulldown()).into_offset_iter();
    let tex = opts.markdown.math && opts.tex_delimiters;
    let mut builder = builder::Builder::new(*opts);
    for (event, range) in math_fixup::MathFixup::new(events, src, tex) {
        builder.event_at(event, range);
    }
    builder.finish()
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
