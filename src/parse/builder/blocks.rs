//! Block structure: the container stack, leaves (paragraph-like blocks),
//! headings and the outline, and finished code and front matter blocks.

use super::{
    Builder, Container, HtmlBox, Kind, Leaf, LeafKind, MAX_DEPTH, Scope, collapse_ws, to_u32,
};
use crate::ir::{
    Anchor, Block, BlockId, CodeBlock, DefItem, Figure, Heading, HeadingId, ImageId, Inlines, List,
    ListItem, MathBlock, RunKind,
};
use crate::parse::front_matter;
use crate::parse::inline::InlineBuf;
use crate::source::{Diagnostic, DiagnosticKind};
use crate::text::sanitize::sanitize;

impl Builder {
    /// A finished fenced or indented code block (`math` fences become display math).
    pub(super) fn finish_code(&mut self) {
        let Some(capture) = self.code.take() else {
            return;
        };
        let mut code = sanitize(&capture.text).into_owned();
        if code.ends_with('\n') {
            code.pop();
        }
        let (lang, title) = parse_info(&capture.info);
        let is_math = lang
            .as_deref()
            .is_some_and(|l| l.eq_ignore_ascii_case("math"));
        if is_math && self.opts.markdown.math {
            let tex = code.trim();
            if !tex.is_empty() {
                self.push_block(Block::Math(MathBlock { tex: tex.into() }), Vec::new());
            }
            return;
        }
        self.push_block(
            Block::Code(CodeBlock {
                lang,
                info: capture.info,
                title,
                code,
            }),
            Vec::new(),
        );
    }

    /// A finished front matter block.
    pub(super) fn finish_meta(&mut self) {
        let Some(meta) = self.meta.take() else {
            return;
        };
        let raw = sanitize(&meta.raw);
        self.push_block(
            Block::FrontMatter(front_matter::parse(meta.format, &raw)),
            Vec::new(),
        );
    }

    /// Open a Markdown container (ignored beyond [`MAX_DEPTH`]).
    pub(super) fn push_container(&mut self, c: Container) {
        if self.overflow > 0 || self.stack.len() >= MAX_DEPTH {
            if self.overflow == 0 {
                self.doc.diagnostics.push(Diagnostic::new(
                    DiagnosticKind::Markup,
                    format!("nesting deeper than {MAX_DEPTH} levels was flattened"),
                ));
            }
            self.overflow += 1;
            return;
        }
        self.stack.push(c);
    }

    /// Push an HTML container (silently dropped when too deep; its end tag
    /// then matches nothing).
    pub(super) fn push_html(&mut self, name: &'static str, kind: HtmlBox) {
        if self.overflow == 0 && self.stack.len() < MAX_DEPTH {
            self.stack.push(Container::Html {
                name,
                kind,
                body: Vec::new(),
            });
        }
    }

    /// Close the nearest open container of `kind` and everything above it.
    pub(super) fn close(&mut self, kind: Kind) {
        self.flush_leaf(true);
        if self.overflow > 0 {
            self.overflow -= 1;
            return;
        }
        let Some(idx) = self.stack.iter().rposition(|c| c.kind() == Some(kind)) else {
            return;
        };
        self.pop_to(idx);
    }

    /// Close the nearest HTML container named `name` above the innermost
    /// Markdown container.
    pub(super) fn close_html(&mut self, name: &str) {
        let mut found = None;
        for (i, c) in self.stack.iter().enumerate().rev() {
            match c {
                Container::Html { name: n, .. } if *n == name => {
                    found = Some(i);
                    break;
                }
                Container::Html { .. } => {}
                _ => break,
            }
        }
        if let Some(idx) = found {
            self.pop_to(idx);
        }
    }

    /// Finish the containers at `idx` and above.
    pub(super) fn pop_to(&mut self, idx: usize) {
        while self.stack.len() > idx {
            let Some(c) = self.stack.pop() else { break };
            self.finish_container(c);
        }
    }

    /// Turn a closed container into its block (or merge it into its parent).
    pub(super) fn finish_container(&mut self, c: Container) {
        match c {
            Container::Quote { alert, body } => {
                self.push_block(Block::Quote { alert, body }, Vec::new())
            }
            Container::List {
                start,
                tight,
                items,
            } => {
                self.push_block(
                    Block::List(List {
                        start,
                        tight,
                        items,
                    }),
                    Vec::new(),
                );
            }
            Container::Item { task, body } => {
                if let Some(Container::List { items, .. }) = self.stack.last_mut() {
                    items.push(ListItem { task, body });
                } else {
                    self.push_blocks(body);
                }
            }
            Container::Footnote { label, body } => {
                self.footnote_defs.entry(label).or_insert(body);
                // Anchors still waiting belong to the definition.
                self.footnote_anchors.append(&mut self.pending_anchors);
            }
            Container::Table(t) => self.push_block(Block::Table(t.finish()), Vec::new()),
            Container::DefList { items } => self.push_block(Block::DefList(items), Vec::new()),
            Container::Definition { body } => {
                if let Some(Container::DefList { items }) = self.stack.last_mut() {
                    if items.is_empty() {
                        items.push(DefItem::default());
                    }
                    if let Some(item) = items.last_mut() {
                        item.defs.push(body);
                    }
                } else {
                    self.push_blocks(body);
                }
            }
            Container::Html { kind, body, .. } => match kind {
                HtmlBox::Details { summary } => self.push_block(
                    Block::Details {
                        summary: summary.unwrap_or_default(),
                        body,
                    },
                    Vec::new(),
                ),
                HtmlBox::Align(align) if !body.is_empty() => {
                    self.push_block(Block::Align { align, body }, Vec::new());
                }
                HtmlBox::Align(_) => {}
                HtmlBox::Group => self.push_blocks(body),
            },
        }
    }

    /// Add a finished block to the innermost container that holds blocks,
    /// resolving waiting anchors to it.
    pub(super) fn push_block(&mut self, block: Block, mut anchors: Vec<Box<str>>) {
        anchors.append(&mut self.pending_anchors);
        if !anchors.is_empty() {
            let target = match &block {
                Block::Heading { id: Some(h), .. } => Anchor::Heading(*h),
                _ => Anchor::Block(BlockId(to_u32(self.root.len()))),
            };
            self.add_anchors(anchors, target);
        }
        let mut dest = &mut self.root;
        for c in self.stack.iter_mut().rev() {
            if let Some(blocks) = c.blocks_mut() {
                dest = blocks;
                break;
            }
        }
        dest.push(block);
    }

    /// Push several finished blocks.
    pub(super) fn push_blocks(&mut self, blocks: Vec<Block>) {
        for b in blocks {
            self.push_block(b, Vec::new());
        }
    }

    /// Register anchor names (the first definition of a name wins). Inside
    /// a footnote definition they are kept for the footnote section instead.
    pub(super) fn add_anchors(&mut self, names: Vec<Box<str>>, target: Anchor) {
        let in_footnote = self.in_footnote();
        for name in names.into_iter().filter(|n| !n.is_empty()) {
            if in_footnote {
                self.footnote_anchors.push(name);
            } else {
                self.doc.anchors.entry(name).or_insert(target);
            }
        }
    }

    /// Whether blocks currently go into a footnote definition.
    pub(super) fn in_footnote(&self) -> bool {
        self.stack
            .iter()
            .any(|c| matches!(c, Container::Footnote { .. }))
    }

    /// Start a paragraph-like block, finishing the current one.
    pub(super) fn open_leaf(&mut self, kind: LeafKind) {
        self.flush_leaf(true);
        self.leaf_seq += 1;
        self.leaf = Some(Leaf {
            kind,
            buf: InlineBuf::with_capacity(self.size_hint),
            anchors: Vec::new(),
            seq: self.leaf_seq,
        });
    }

    /// The current leaf, opening an implicit paragraph if there is none.
    pub(super) fn leaf_mut(&mut self) -> &mut Leaf {
        if self.leaf.is_none() {
            self.leaf_seq += 1;
        }
        let seq = self.leaf_seq;
        self.leaf.get_or_insert_with(|| Leaf {
            kind: LeafKind::Para,
            buf: InlineBuf::default(),
            anchors: Vec::new(),
            seq,
        })
    }

    /// The leaf and offset where content added next will go.
    pub(super) fn leaf_pos(&self) -> (u32, usize) {
        self.leaf
            .as_ref()
            .map_or((self.leaf_seq + 1, 0), |l| (l.seq, l.buf.len()))
    }

    /// Finish the current leaf into a block. `end_of_leaf` is false when a
    /// paragraph is only split (display math), so open formatting goes on.
    pub(super) fn flush_leaf(&mut self, end_of_leaf: bool) {
        let Some(leaf) = self.leaf.take() else {
            return;
        };
        if end_of_leaf {
            // Markdown formatting never spans blocks; inline HTML formatting
            // ends with its paragraph.
            self.md_flags.clear();
            self.links
                .retain(|l| l.html && l.scope != Some(Scope::Leaf(leaf.seq)));
            self.html_flags.retain(|f| f.scope != Scope::Leaf(leaf.seq));
            self.skip = None;
        }
        let Leaf {
            kind, buf, anchors, ..
        } = leaf;
        let mut inl = buf.finish();
        if self.opts.markdown.linkify {
            self.linkify(&mut inl);
        }
        match kind {
            LeafKind::Para => {
                if inl.is_empty() {
                    self.pending_anchors.extend(anchors);
                    return;
                }
                let block = match single_image(&inl) {
                    Some(image) => Block::Figure(Figure { image }),
                    None => Block::Para(inl),
                };
                self.push_block(block, anchors);
            }
            LeafKind::Heading {
                level,
                explicit_id,
                html_id,
            } => self.finish_heading(level, inl, explicit_id, html_id, anchors),
            LeafKind::Cell => {
                let here = Anchor::Block(BlockId(to_u32(self.root.len())));
                if let Some(Container::Table(t)) = self.stack.last_mut() {
                    if t.in_head {
                        t.head.push(inl);
                    } else {
                        t.row.push(inl);
                    }
                    self.add_anchors(anchors, here);
                } else if !inl.is_empty() {
                    self.push_block(Block::Para(inl), anchors);
                }
            }
            LeafKind::Term => {
                let here = Anchor::Block(BlockId(to_u32(self.root.len())));
                if let Some(Container::DefList { items }) = self.stack.last_mut() {
                    items.push(DefItem {
                        term: inl,
                        defs: Vec::new(),
                    });
                    self.add_anchors(anchors, here);
                } else if !inl.is_empty() {
                    self.push_block(Block::Para(inl), anchors);
                }
            }
            LeafKind::Summary => {
                let here = Anchor::Block(BlockId(to_u32(self.root.len())));
                if let Some(Container::Html {
                    kind: HtmlBox::Details { summary },
                    ..
                }) = self.stack.last_mut()
                {
                    if summary.is_none() {
                        *summary = Some(inl);
                    }
                    self.add_anchors(anchors, here);
                } else if !inl.is_empty() {
                    self.push_block(Block::Para(inl), anchors);
                }
            }
        }
    }

    /// Add a heading to the outline (unless it is in a footnote) and push it.
    pub(super) fn finish_heading(
        &mut self,
        level: u8,
        text: Inlines,
        explicit_id: Option<Box<str>>,
        html_id: Option<Box<str>>,
        mut anchors: Vec<Box<str>>,
    ) {
        let id = if self.in_footnote() {
            None
        } else {
            let id = HeadingId(to_u32(self.doc.headings.len()));
            let source = slug_source(&text);
            let title: Box<str> = collapse_ws(&source.replace('\n', " ")).into();
            let slug: Box<str> = match explicit_id {
                Some(explicit) => {
                    self.slugger.reserve(&explicit);
                    explicit
                }
                None => self.slugger.unique(&source.replace('\n', "")).into(),
            };
            while self.heading_stack.last().is_some_and(|&(l, _)| l >= level) {
                self.heading_stack.pop();
            }
            let parent = self.heading_stack.last().map(|&(_, h)| h);
            self.heading_stack.push((level, id));
            if level == 1 && self.first_h1.is_none() && !title.is_empty() {
                self.first_h1 = Some(title.clone());
            }
            self.doc.headings.push(Heading {
                level,
                title,
                slug: slug.clone(),
                parent,
                top: BlockId(to_u32(self.root.len())),
            });
            anchors.insert(0, slug);
            Some(id)
        };
        anchors.extend(html_id);
        self.push_block(Block::Heading { level, text, id }, anchors);
    }
}

/// First word of a code fence info string (`rust`, `{.rust}`, `rust,ignore`)
/// and its `title="…"` attribute.
fn parse_info(info: &str) -> (Option<Box<str>>, Option<Box<str>>) {
    let first = info
        .split(|c: char| c.is_whitespace() || c == ',')
        .next()
        .unwrap_or("");
    let lang = first
        .trim_start_matches('{')
        .trim_start_matches('.')
        .trim_end_matches('}');
    let lang = (!lang.is_empty()).then(|| Box::from(lang));
    (lang, info_title(info))
}

fn info_title(info: &str) -> Option<Box<str>> {
    let mut search = 0;
    while let Some(found) = info.get(search..)?.find("title=") {
        let at = search + found;
        let boundary = info
            .get(..at)?
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '{' | ','));
        if boundary {
            let rest = info.get(at + "title=".len()..)?;
            let value = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let body = rest.get(1..)?;
                    body.get(..body.find(q)?)?
                }
                _ => rest
                    .split(|c: char| c.is_whitespace() || matches!(c, '}' | ','))
                    .next()?,
            };
            return (!value.is_empty()).then(|| Box::from(value));
        }
        search = at + 1;
    }
    None
}

/// The text content of a heading: its text without image chips (GitHub
/// slugs use the HTML text content, and images have none). Hard breaks are
/// kept as `\n` for the caller to drop (slug) or turn into spaces (title).
fn slug_source(text: &Inlines) -> String {
    let mut s = String::with_capacity(text.text.len());
    for (range, run) in text.runs_with_ranges() {
        if !matches!(run.kind, RunKind::ImageChip(_)) {
            s.push_str(text.slice(range));
        }
    }
    s
}

/// The single image of a paragraph that holds nothing else (whitespace aside).
fn single_image(inl: &Inlines) -> Option<ImageId> {
    let mut image = None;
    for (range, run) in inl.runs_with_ranges() {
        match run.kind {
            RunKind::ImageChip(id) if image.is_none() => image = Some(id),
            RunKind::Text if inl.slice(range).chars().all(char::is_whitespace) => {}
            _ => return None,
        }
    }
    image
}
