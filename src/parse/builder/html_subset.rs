//! The HTML subset: formatting tags, links and anchors, images and
//! `<picture>`, `<br>`/`<wbr>`, and container tags (`<details>`, `align=`,
//! `<center>`, headings) matched on the builder's container stack.

use std::mem;

use super::{
    Builder, Container, HtmlBox, HtmlFlag, LeafKind, LinkFrame, Scope, collapse_ws, non_empty,
};
use crate::ir::{
    Block, CodeBlock, HAlign, ImageRef, InlineFlags, Link, LinkKind, PictureSource, RunKind,
};
use crate::options::HtmlMode;
use crate::parse::html::{self, Lexer, Token};
use crate::parse::target;
use crate::text::sanitize::sanitize;

impl Builder {
    /// Interpret a finished HTML block (or keep it raw).
    pub(super) fn finish_html_block(&mut self) {
        let Some(html) = self.html_block.take() else {
            return;
        };
        self.html_seq += 1;
        if self.opts.html == HtmlMode::Raw {
            let raw = sanitize(html.trim_end());
            if !raw.is_empty() {
                self.push_block(Block::Html(raw.into()), Vec::new());
            }
            return;
        }
        let scope = Scope::Block(self.html_seq);
        for tok in Lexer::new(&html) {
            self.html_token(tok, scope, true);
        }
        // The scope of the block ends: finish what it left open.
        if let Some(pre) = self.pre.take() {
            self.push_pre(&pre);
        }
        self.skip = None;
        self.picture = None;
        self.flush_leaf(true);
        self.html_flags.retain(|f| f.scope != scope);
        self.links.retain(|l| l.scope != Some(scope));
        // Headings cannot span blocks: close their alignment wrappers.
        while let Some(Container::Html { name, .. }) = self.stack.last()
            && is_heading_tag(name)
        {
            let top = self.stack.len().saturating_sub(1);
            self.pop_to(top);
        }
    }

    /// Interpret inline HTML (one tag, usually) inside the current leaf.
    pub(super) fn inline_html(&mut self, s: &str) {
        if self.opts.html == HtmlMode::Raw {
            let style = self.style();
            self.leaf_mut()
                .buf
                .push_run(&sanitize(s), RunKind::Html, style);
            return;
        }
        let (seq, _) = self.leaf_pos();
        for tok in Lexer::new(s) {
            self.html_token(tok, Scope::Leaf(seq), false);
        }
    }

    /// Handle one HTML token; `block` says whether it came from an HTML block.
    pub(super) fn html_token(&mut self, tok: Token<'_>, scope: Scope, block: bool) {
        if let Some(name) = self.skip {
            if matches!(tok, Token::End(n) if n.eq_ignore_ascii_case(name)) {
                self.skip = None;
            }
            return;
        }
        if let Some(pre) = &mut self.pre {
            match tok {
                Token::Text(t) => pre.push_str(&html::decode_entities(t)),
                Token::Start(tag) if tag.is("br") => pre.push('\n'),
                Token::End(n)
                    if n.eq_ignore_ascii_case("pre") || n.eq_ignore_ascii_case("textarea") =>
                {
                    if let Some(pre) = self.pre.take() {
                        self.push_pre(&pre);
                    }
                }
                _ => {}
            }
            return;
        }
        match tok {
            Token::Text(t) => self.html_text(t, block),
            Token::Start(tag) => self.html_start(&tag, scope, block),
            Token::End(name) => self.html_end(name, block),
            Token::Comment | Token::Other => {}
        }
    }

    /// Text between tags: entity-decoded prose.
    pub(super) fn html_text(&mut self, t: &str, block: bool) {
        let decoded = html::decode_entities(t);
        if block
            && self.leaf.is_none()
            && decoded
                .trim_matches(|c: char| c.is_ascii_whitespace())
                .is_empty()
        {
            return;
        }
        self.text(&decoded);
    }

    /// An opening tag.
    pub(super) fn html_start(&mut self, tag: &html::Tag<'_>, scope: Scope, block: bool) {
        let name = tag.name.to_ascii_lowercase();
        if let Some((canonical, flag, code)) = inline_tag(&name) {
            if !tag.self_closing {
                self.html_flags.push(HtmlFlag {
                    name: canonical,
                    flag,
                    code,
                    scope,
                });
            }
            return;
        }
        match name.as_str() {
            "a" => {
                for key in ["id", "name"] {
                    if let Some(v) = tag.attr(key) {
                        self.anchor_here(sanitize(v.trim()).into());
                    }
                }
                if let Some(href) = tag.attr("href")
                    && !tag.self_closing
                {
                    let url = sanitize(href.trim()).into_owned();
                    let target = target::classify(&url);
                    let title = tag
                        .attr("title")
                        .map(|t| sanitize(&t).into())
                        .unwrap_or_default();
                    let id = self.new_link(Link {
                        url: url.into(),
                        title,
                        target,
                        kind: LinkKind::Html,
                    });
                    let (leaf, start) = self.leaf_pos();
                    self.links.push(LinkFrame {
                        id,
                        html: true,
                        scope: Some(scope),
                        leaf,
                        start,
                        url_like: false,
                    });
                }
            }
            "img" => self.html_img(tag),
            "br" => {
                let style = self.style();
                if let Some(leaf) = self.leaf.as_mut() {
                    leaf.buf.hard_break(style);
                }
            }
            "wbr" => {
                if let Some(leaf) = self.leaf.as_mut() {
                    leaf.buf.break_here();
                }
            }
            "picture" => self.picture = Some(Vec::new()),
            "source" => {
                if let Some(sources) = self.picture.as_mut()
                    && let Some(srcset) = tag.attr("srcset").or_else(|| tag.attr("src"))
                {
                    sources.push(PictureSource {
                        srcset: Box::from(sanitize(srcset.trim())),
                        media: tag.attr("media").map(|m| Box::from(sanitize(m.trim()))),
                    });
                }
            }
            "script" | "style" | "template" | "noscript" | "iframe" | "object" => {
                if !tag.self_closing {
                    self.skip = Some(skipped_tag(&name));
                }
            }
            "td" | "th" => {
                let style = self.style();
                if let Some(leaf) = self.leaf.as_mut()
                    && !leaf.buf.is_empty()
                {
                    leaf.buf.push_text(" ", style);
                }
            }
            _ if !block => {} // block-level tags inside a paragraph are dropped
            "pre" | "textarea" => {
                self.close_open_p();
                self.flush_leaf(true);
                self.pre = Some(String::new());
            }
            "details" => {
                self.close_open_p();
                self.flush_leaf(true);
                self.push_html("details", HtmlBox::Details { summary: None });
            }
            "summary" => self.open_leaf(LeafKind::Summary),
            "p" | "div" | "center" => {
                self.close_open_p();
                self.flush_leaf(true);
                let align = if name == "center" {
                    Some(HAlign::Center)
                } else {
                    tag.attr("align").and_then(|a| parse_align(&a))
                };
                let canonical = match name.as_str() {
                    "p" => "p",
                    "div" => "div",
                    _ => "center",
                };
                self.push_html(canonical, align.map_or(HtmlBox::Group, HtmlBox::Align));
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_open_p();
                self.flush_leaf(true);
                let level = name.as_bytes().get(1).map_or(1, |d| d.saturating_sub(b'0'));
                let canonical = heading_tag(level);
                if let Some(align) = tag.attr("align").and_then(|a| parse_align(&a)) {
                    self.push_html(canonical, HtmlBox::Align(align));
                }
                let html_id = tag.attr("id").and_then(|i| non_empty(&sanitize(i.trim())));
                self.open_leaf(LeafKind::Heading {
                    level,
                    explicit_id: None,
                    html_id,
                });
            }
            "hr" => {
                self.close_open_p();
                self.flush_leaf(true);
                self.push_block(Block::Rule, Vec::new());
            }
            n if is_block_tag(n) => {
                self.close_open_p();
                self.flush_leaf(true);
            }
            _ => {} // unknown tags are dropped, their text is kept
        }
    }

    /// A closing tag.
    pub(super) fn html_end(&mut self, name: &str, block: bool) {
        let name = name.to_ascii_lowercase();
        if let Some((canonical, ..)) = inline_tag(&name) {
            if let Some(i) = self.html_flags.iter().rposition(|f| f.name == canonical) {
                self.html_flags.remove(i);
            }
            return;
        }
        match name.as_str() {
            "a" => self.end_link(true),
            "picture" => self.picture = None,
            _ if !block => {}
            "details" => {
                self.flush_leaf(true);
                self.close_html("details");
            }
            "summary" => {
                if matches!(self.leaf.as_ref().map(|l| &l.kind), Some(LeafKind::Summary)) {
                    self.flush_leaf(true);
                }
            }
            "p" | "div" | "center" => {
                self.flush_leaf(true);
                let canonical = match name.as_str() {
                    "p" => "p",
                    "div" => "div",
                    _ => "center",
                };
                self.close_html(canonical);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if matches!(
                    self.leaf.as_ref().map(|l| &l.kind),
                    Some(LeafKind::Heading { .. })
                ) {
                    self.flush_leaf(true);
                }
                let level = name.as_bytes().get(1).map_or(1, |d| d.saturating_sub(b'0'));
                self.close_html(heading_tag(level));
            }
            n if is_block_tag(n) => self.flush_leaf(true),
            _ => {}
        }
    }

    /// A `<p>` cannot contain block elements: close one that is open on top.
    pub(super) fn close_open_p(&mut self) {
        if let Some(Container::Html { name: "p", .. }) = self.stack.last() {
            self.flush_leaf(true);
            let top = self.stack.len().saturating_sub(1);
            self.pop_to(top);
        }
    }

    /// An `<img>`: record it (with any `<picture>` sources) and add its chip.
    pub(super) fn html_img(&mut self, tag: &html::Tag<'_>) {
        let attr = |key: &str| tag.attr(key).map(|v| sanitize(v.trim()).into_owned());
        let src = attr("src")
            .filter(|s| !s.is_empty())
            .or_else(|| {
                attr("srcset").map(|s| {
                    PictureSource {
                        srcset: s.into(),
                        media: None,
                    }
                    .first_url()
                    .to_string()
                })
            })
            .unwrap_or_default();
        let image = ImageRef {
            src: src.into(),
            alt: collapse_ws(&attr("alt").unwrap_or_default()).into(),
            title: attr("title").unwrap_or_default().into(),
            width: tag.attr("width").and_then(|w| html::parse_length(&w)),
            height: tag.attr("height").and_then(|h| html::parse_length(&h)),
            sources: self.picture.as_mut().map(mem::take).unwrap_or_default(),
            link: self.links.last().map(|l| l.id),
        };
        let id = self.add_image(image);
        self.push_chip(id);
    }

    /// A finished `<pre>` block, as code.
    pub(super) fn push_pre(&mut self, text: &str) {
        let text = sanitize(text);
        // HTML ignores a newline right after <pre>.
        let text = text.strip_prefix('\n').unwrap_or(&text);
        let code = text.trim_end_matches([' ', '\t', '\n']).to_string();
        if code.is_empty() {
            return;
        }
        self.push_block(
            Block::Code(CodeBlock {
                lang: None,
                info: "".into(),
                title: None,
                code,
            }),
            Vec::new(),
        );
    }

    /// An `<a id>`/`<a name>` anchor at the current position.
    pub(super) fn anchor_here(&mut self, name: Box<str>) {
        if name.is_empty() {
            return;
        }
        match self.leaf.as_mut() {
            Some(leaf) => leaf.anchors.push(name),
            None => self.pending_anchors.push(name),
        }
    }
}

fn heading_tag(level: u8) -> &'static str {
    match level {
        1 => "h1",
        2 => "h2",
        3 => "h3",
        4 => "h4",
        5 => "h5",
        _ => "h6",
    }
}

fn is_heading_tag(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

fn parse_align(value: &str) -> Option<HAlign> {
    let v = value.trim();
    if v.eq_ignore_ascii_case("center") || v.eq_ignore_ascii_case("middle") {
        Some(HAlign::Center)
    } else if v.eq_ignore_ascii_case("right") {
        Some(HAlign::Right)
    } else if v.eq_ignore_ascii_case("left") {
        Some(HAlign::Left)
    } else {
        None
    }
}

/// Inline HTML formatting tags: canonical name, flag, and whether the
/// content is code.
fn inline_tag(name: &str) -> Option<(&'static str, InlineFlags, bool)> {
    Some(match name {
        "b" => ("b", InlineFlags::STRONG, false),
        "strong" => ("strong", InlineFlags::STRONG, false),
        "i" => ("i", InlineFlags::EMPH, false),
        "em" => ("em", InlineFlags::EMPH, false),
        "cite" => ("cite", InlineFlags::EMPH, false),
        "var" => ("var", InlineFlags::EMPH, false),
        "u" => ("u", InlineFlags::UNDERLINE, false),
        "ins" => ("ins", InlineFlags::UNDERLINE, false),
        "s" => ("s", InlineFlags::STRIKE, false),
        "del" => ("del", InlineFlags::STRIKE, false),
        "strike" => ("strike", InlineFlags::STRIKE, false),
        "mark" => ("mark", InlineFlags::MARK, false),
        "sub" => ("sub", InlineFlags::SUB, false),
        "sup" => ("sup", InlineFlags::SUP, false),
        "kbd" => ("kbd", InlineFlags::KBD, false),
        "code" => ("code", InlineFlags::empty(), true),
        "tt" => ("tt", InlineFlags::empty(), true),
        "samp" => ("samp", InlineFlags::empty(), true),
        _ => return None,
    })
}

fn skipped_tag(name: &str) -> &'static str {
    match name {
        "script" => "script",
        "style" => "style",
        "template" => "template",
        "noscript" => "noscript",
        "iframe" => "iframe",
        _ => "object",
    }
}

/// Block-level HTML elements without special meaning: they separate
/// paragraphs.
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "caption"
            | "dd"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "header"
            | "hgroup"
            | "li"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "section"
            | "table"
            | "tbody"
            | "tfoot"
            | "thead"
            | "tr"
            | "ul"
    )
}
