//! Inline content: text, math, footnote references, images, links and
//! bare-URL linkification, plus the current inline style.

use std::mem;
use std::ops::Range;

use pulldown_cmark::LinkType;

use super::{Builder, Container, LeafKind, LinkFrame, collapse_ws, to_u32};
use crate::ir::{
    Block, FootnoteId, ImageId, ImageRef, InlineFlags, Inlines, Link, LinkId, LinkKind, MathBlock,
    Run, RunKind, Target,
};
use crate::parse::inline::{Style, url_break_points};
use crate::parse::{linkify, target};
use crate::text::sanitize::sanitize;

impl Builder {
    /// A finished Markdown image: record it and add its inline chip.
    pub(super) fn finish_image(&mut self) {
        let Some(img) = self.image.take() else {
            return;
        };
        let alt = collapse_ws(&sanitize(&img.alt));
        let id = self.add_image(ImageRef {
            src: img.src,
            alt: alt.into(),
            title: img.title,
            link: self.links.last().map(|l| l.id),
            ..ImageRef::default()
        });
        self.push_chip(id);
    }

    /// Turn bare URLs and emails in link-less text runs into links.
    pub(super) fn linkify(&mut self, inl: &mut Inlines) {
        let mut found = mem::take(&mut self.found);
        found.clear();
        let mut start = 0usize;
        let mut span: Option<usize> = None;
        for run in &inl.runs {
            let eligible = run.kind == RunKind::Text && run.link.is_none();
            match (eligible, span) {
                (true, None) => span = Some(start),
                (false, Some(s)) => {
                    find_links(&inl.text, s..start, &mut found);
                    span = None;
                }
                _ => {}
            }
            start = run.end as usize;
        }
        if let Some(s) = span {
            find_links(&inl.text, s..start, &mut found);
        }
        if !found.is_empty() {
            let mut links = Vec::with_capacity(found.len());
            for f in &found {
                let target = if f.email {
                    Target::Email
                } else {
                    target::classify(&f.url)
                };
                let id = self.new_link(Link {
                    url: f.url.as_str().into(),
                    title: "".into(),
                    target,
                    kind: LinkKind::Bare,
                });
                links.push((f.range.clone(), id));
                if !f.email {
                    url_break_points(&inl.text, f.range.clone(), &mut inl.extra_breaks);
                }
            }
            inl.runs = split_at_links(&inl.runs, &links);
            inl.extra_breaks.sort_unstable();
            inl.extra_breaks.dedup();
        }
        self.found = found;
    }

    /// The inline flags in effect (Markdown and HTML formatting).
    pub(super) fn flags(&self) -> InlineFlags {
        let md = self
            .md_flags
            .iter()
            .fold(InlineFlags::empty(), |acc, (_, f)| acc | *f);
        self.html_flags.iter().fold(md, |acc, f| acc | f.flag)
    }

    /// The style of text added now.
    pub(super) fn style(&self) -> Style {
        Style {
            flags: self.flags(),
            link: self.links.last().map(|l| l.id),
        }
    }

    /// Inside HTML `<code>`/`<tt>`/`<samp>`.
    pub(super) fn in_code(&self) -> bool {
        self.html_flags.iter().any(|f| f.code)
    }

    /// Prose text (or code text inside HTML `<code>`).
    pub(super) fn text(&mut self, s: &str) {
        if self.skip.is_some() {
            return;
        }
        let style = self.style();
        let code = self.in_code();
        let leaf = self.leaf_mut();
        if code {
            leaf.buf.push_code(s, style);
        } else {
            leaf.buf.push_text(s, style);
        }
    }

    /// Inline math: an atom holding the TeX.
    pub(super) fn inline_math(&mut self, tex: &str) {
        let style = self.style();
        self.leaf_mut()
            .buf
            .push_atom(tex.trim(), RunKind::Math, style);
    }

    /// Display math is lifted out of a paragraph (splitting it); in table
    /// cells, headings, terms, summaries and link text it stays inline.
    pub(super) fn display_math(&mut self, tex: &str) {
        let tex = tex.trim();
        if tex.is_empty() {
            return;
        }
        let liftable = matches!(
            self.leaf.as_ref().map(|l| &l.kind),
            None | Some(LeafKind::Para)
        ) && self.links.is_empty()
            && !matches!(self.stack.last(), Some(Container::Table(_)));
        if liftable {
            self.flush_leaf(false);
            let tex = sanitize(tex);
            self.push_block(Block::Math(MathBlock { tex: tex.into() }), Vec::new());
        } else {
            let style = self.style();
            self.leaf_mut().buf.push_atom(tex, RunKind::Math, style);
        }
    }

    /// A footnote reference: numbered by first reference, an atom and a link.
    pub(super) fn footnote_ref(&mut self, label: &str) {
        let label: Box<str> = sanitize(label).into();
        let id = match self.footnote_ids.get(&label) {
            Some(&id) => id,
            None => {
                let id = FootnoteId(to_u32(self.footnotes.len()));
                self.footnote_ids.insert(label.clone(), id);
                self.footnotes.push((label.clone(), Vec::new()));
                id
            }
        };
        let link = self.new_link(Link {
            url: format!("#fn-{label}").into(),
            title: "".into(),
            target: Target::Footnote(id),
            kind: LinkKind::Footnote,
        });
        if let Some((_, refs)) = self.footnotes.get_mut(id.index()) {
            refs.push(link);
        }
        let style = Style {
            flags: self.flags(),
            link: Some(link),
        };
        let number = id.0.saturating_add(1).to_string();
        self.leaf_mut()
            .buf
            .push_atom(&number, RunKind::FootRef(id), style);
    }

    /// A task list marker for the innermost list item.
    pub(super) fn task(&mut self, done: bool) {
        for c in self.stack.iter_mut().rev() {
            if let Container::Item { task, body } = c {
                if task.is_none() && body.is_empty() {
                    *task = Some(done);
                }
                return;
            }
        }
    }

    /// Record a link occurrence.
    pub(super) fn new_link(&mut self, link: Link) -> LinkId {
        let id = LinkId(to_u32(self.doc.links.len()));
        self.doc.links.push(link);
        id
    }

    /// Record an image occurrence.
    pub(super) fn add_image(&mut self, image: ImageRef) -> ImageId {
        let id = ImageId(to_u32(self.doc.images.len()));
        self.doc.images.push(image);
        id
    }

    /// Append the inline chip of an image.
    pub(super) fn push_chip(&mut self, id: ImageId) {
        let text = self
            .doc
            .image(id)
            .map(chip_text)
            .unwrap_or_else(|| "image".into());
        let style = self.style();
        self.leaf_mut()
            .buf
            .push_run(&text, RunKind::ImageChip(id), style);
    }

    /// Open a Markdown link.
    pub(super) fn start_link(&mut self, link_type: LinkType, dest: &str, title: &str) {
        let dest = sanitize(dest);
        let dest = dest.trim();
        let (kind, url, target) = match link_type {
            LinkType::Email => {
                let url = if dest.starts_with("mailto:") {
                    dest.to_string()
                } else {
                    format!("mailto:{dest}")
                };
                (LinkKind::Email, url, Target::Email)
            }
            LinkType::Autolink => (LinkKind::Autolink, dest.to_string(), target::classify(dest)),
            LinkType::WikiLink { .. } => (
                LinkKind::Wiki,
                dest.to_string(),
                target::classify_wiki(dest),
            ),
            LinkType::Inline => (LinkKind::Inline, dest.to_string(), target::classify(dest)),
            LinkType::Reference
            | LinkType::ReferenceUnknown
            | LinkType::Collapsed
            | LinkType::CollapsedUnknown
            | LinkType::Shortcut
            | LinkType::ShortcutUnknown => (
                LinkKind::Reference,
                dest.to_string(),
                target::classify(dest),
            ),
        };
        let id = self.new_link(Link {
            url: url.into(),
            title: sanitize(title).into(),
            target,
            kind,
        });
        let (leaf, start) = self.leaf_pos();
        self.links.push(LinkFrame {
            id,
            html: false,
            scope: None,
            leaf,
            start,
            url_like: kind == LinkKind::Autolink,
        });
    }

    /// Close the innermost Markdown (`html == false`) or HTML link; link
    /// text that is a URL gets URL break points.
    pub(super) fn end_link(&mut self, html: bool) {
        let Some(i) = self.links.iter().rposition(|l| l.html == html) else {
            return;
        };
        let frame = self.links.remove(i);
        if let Some(leaf) = self.leaf.as_mut()
            && leaf.seq == frame.leaf
        {
            let end = leaf.buf.len();
            let text = leaf.buf.text().get(frame.start..end).unwrap_or("");
            if frame.url_like || linkify::looks_like_url(text) {
                leaf.buf.url_breaks(frame.start..end);
            }
        }
    }
}

/// Text shown for an image chip: the alt text, else the file name.
fn chip_text(img: &ImageRef) -> String {
    if !img.alt.is_empty() {
        return img.alt.to_string();
    }
    let path = img.src.split(['?', '#']).next().unwrap_or("");
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let name = target::percent_decode(name);
    let name = collapse_ws(&name);
    if name.is_empty() {
        "image".to_string()
    } else {
        name
    }
}

/// Run linkify over `text[range]`, with results in `text` coordinates.
fn find_links(text: &str, range: Range<usize>, out: &mut Vec<linkify::Found>) {
    let Some(slice) = text.get(range.clone()) else {
        return;
    };
    let before = out.len();
    linkify::find(slice, out);
    for f in out.iter_mut().skip(before) {
        f.range = f.range.start + range.start..f.range.end + range.start;
    }
}

/// Split runs at link boundaries and attach the links.
fn split_at_links(runs: &[Run], links: &[(Range<usize>, LinkId)]) -> Vec<Run> {
    let mut out = Vec::with_capacity(runs.len() + 2 * links.len());
    let mut li = 0;
    let mut start = 0u32;
    for run in runs {
        let mut s = start;
        while s < run.end {
            while links.get(li).is_some_and(|(r, _)| to_u32(r.end) <= s) {
                li += 1;
            }
            let piece_end = match links.get(li) {
                Some((r, id)) if to_u32(r.start) <= s => {
                    let e = to_u32(r.end).min(run.end);
                    out.push(Run {
                        end: e,
                        link: Some(*id),
                        ..*run
                    });
                    e
                }
                Some((r, _)) if to_u32(r.start) < run.end => {
                    let e = to_u32(r.start);
                    out.push(Run { end: e, ..*run });
                    e
                }
                _ => {
                    out.push(Run {
                        end: run.end,
                        ..*run
                    });
                    run.end
                }
            };
            s = piece_end;
        }
        start = run.end;
    }
    out
}
