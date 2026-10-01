//! Links: which one is where on screen, hint labels, and where following
//! one leads.
//!
//! Layout records one [`crate::layout::LinkHit`] per link fragment per
//! line; the pager groups the hits of a link wrapped over several lines
//! into one [`Occurrence`], which is what Tab focuses and hints label.
//!
//! Following a link ([`target`]):
//! * anchors resolve through the document's anchors (GitHub slugs, with
//!   `-1`, `-2` for duplicates), footnote references to the footnote,
//!   back-links to the reference;
//! * a relative Markdown file, or a directory (its README), is loaded in
//!   the pager; other local files are only shown by path;
//! * web and mail links are opened (or copied) by the shell; links with a
//!   scheme that runs code are refused.

use std::path::{Path, PathBuf};

use crate::ir::{Anchor, Block, Document, FootnoteId, SrcPos, Target};
use crate::layout::{Layout, LineKind};
use crate::render::osc8;

use super::search::Corpus;
use super::state::{Derived, Occurrence};

/// Hint label characters (the home row).
pub(crate) const HINT_KEYS: [char; 10] = ['a', 's', 'd', 'f', 'g', 'h', 'j', 'k', 'l', ';'];

/// Where following a link leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Follow {
    /// A line of this document.
    Line(usize),
    /// Another Markdown document (or a directory's README).
    Load {
        path: PathBuf,
        anchor: Option<String>,
    },
    /// A local file that is not Markdown (or a directory).
    File(PathBuf),
    /// A web or mail link (already made safe for OSC 8 and arguments).
    Open(String),
    /// Nothing: what could not be found or is refused.
    Nowhere(String),
}

/// The link occurrence containing hit `hit`.
fn occurrence_of_hit(derived: &Derived, hit: u32) -> Option<usize> {
    let i = derived.links.partition_point(|o| o.first <= hit);
    let i = i.checked_sub(1)?;
    let o = derived.links.get(i)?;
    (hit < o.first.saturating_add(o.hits)).then_some(i)
}

/// The link occurrence under column `col` (relative to the text column)
/// of line `line`.
pub(crate) fn occurrence_at(
    layout: &Layout,
    derived: &Derived,
    line: usize,
    col: u16,
) -> Option<usize> {
    let line = u32::try_from(line).ok()?;
    let first = layout.link_hits.partition_point(|h| h.line < line);
    let (i, _) = layout
        .link_hits
        .iter()
        .enumerate()
        .skip(first)
        .take_while(|(_, h)| h.line == line)
        .find(|(_, h)| h.cols.contains(&col))?;
    occurrence_of_hit(derived, u32::try_from(i).ok()?)
}

/// Whether any hit of `o` is on lines `top..top + rows`.
pub(crate) fn visible(o: &Occurrence, top: usize, rows: usize) -> bool {
    let first = o.line as usize;
    let last = first + o.hits.saturating_sub(1) as usize;
    first < top + rows && last >= top
}

/// The first hit of `o` on lines `top..top + rows`: `(line, start column)`.
pub(crate) fn first_visible_hit(
    layout: &Layout,
    o: &Occurrence,
    top: usize,
    rows: usize,
) -> Option<(usize, u16)> {
    let hits = layout
        .link_hits
        .get(o.first as usize..(o.first as usize).saturating_add(o.hits as usize))?;
    hits.iter()
        .find(|h| (h.line as usize) >= top && (h.line as usize) < top + rows)
        .map(|h| (h.line as usize, h.cols.start))
}

/// `n` hint labels: single keys for up to ten links, else two keys each
/// (up to a hundred), so no label is a prefix of another.
pub(crate) fn hint_labels(n: usize) -> Vec<String> {
    if n <= HINT_KEYS.len() {
        return HINT_KEYS.iter().take(n).map(|c| c.to_string()).collect();
    }
    HINT_KEYS
        .iter()
        .flat_map(|&a| HINT_KEYS.iter().map(move |&b| format!("{a}{b}")))
        .take(n)
        .collect()
}

/// `path` relative to `base` unless absolute.
fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// The first line of an anchor's target.
pub(crate) fn anchor_line(doc: &Document, layout: &Layout, name: &str) -> Option<usize> {
    let bare = name.strip_prefix('#').unwrap_or(name);
    let anchor = doc.resolve_anchor(bare);
    let line = match anchor {
        Some(Anchor::Heading(h)) => layout
            .heading_line
            .get(h.index())
            .copied()
            .filter(|&l| l != u32::MAX)
            .map(|l| l as usize),
        Some(Anchor::Block(b)) => layout.block_lines.get(b.index()).map(|r| r.start as usize),
        // GitHub: `#` and an unknown `#top` go to the top.
        None if bare.is_empty() || bare.eq_ignore_ascii_case("top") => Some(0),
        None => None,
    };
    line.filter(|&l| l < layout.len().max(1))
}

/// The first line of footnote `f` in the footnote section.
pub(crate) fn footnote_line(doc: &Document, layout: &Layout, f: FootnoteId) -> Option<usize> {
    doc.footnote(f)?;
    let top = doc
        .blocks
        .iter()
        .position(|b| matches!(b, Block::FootnoteSection))?;
    let block = layout.block_lines.get(top)?.clone();
    let off = Corpus::footnote_offset(doc, f.index());
    let pos = SrcPos {
        top: u32::try_from(top).ok()?,
        off,
    };
    let mut line = layout.line_at(pos).max(block.start as usize);
    // Skip the section's heading and the gap after it (they share the
    // first footnote's position).
    while let (Some(cur), Some(next)) = (layout.lines.get(line), layout.lines.get(line + 1)) {
        let header = line == block.start as usize;
        if (header || cur.kind == LineKind::Blank) && next.pos == cur.pos {
            line += 1;
        } else {
            break;
        }
    }
    (line < block.end as usize).then_some(line)
}

/// Where following occurrence `o` of a document laid out as `layout` leads.
pub(crate) fn target(doc: &Document, base: &Path, layout: &Layout, o: &Occurrence) -> Follow {
    let Some(link) = doc.link(o.link) else {
        return Follow::Nowhere("that link".to_owned());
    };
    if o.back {
        // A footnote back-link: to where the reference is shown.
        return layout
            .link_hits
            .iter()
            .find(|h| h.link == o.link && !h.back)
            .map_or_else(
                || Follow::Nowhere("the footnote reference".to_owned()),
                |h| Follow::Line(h.line as usize),
            );
    }
    match &link.target {
        Target::Anchor(name) => anchor_line(doc, layout, name)
            .map_or_else(|| Follow::Nowhere(format!("#{name}")), Follow::Line),
        Target::Footnote(f) => footnote_line(doc, layout, *f)
            .map_or_else(|| Follow::Nowhere("the footnote".to_owned()), Follow::Line),
        Target::LocalDoc { path, anchor } => Follow::Load {
            path: resolve(base, path),
            anchor: anchor.as_deref().map(str::to_owned),
        },
        Target::LocalFile(path) => Follow::File(resolve(base, path)),
        Target::External | Target::Email => match osc8::link_url(link) {
            Some(url) => Follow::Open(url),
            None => Follow::Nowhere("a safe URL for that link".to_owned()),
        },
    }
}

/// The URL to copy for occurrence `o` (`y`): the link as written, with
/// web links made safe.
pub(crate) fn copy_text(doc: &Document, base: &Path, o: &Occurrence) -> Option<String> {
    let link = doc.link(o.link)?;
    match &link.target {
        Target::External | Target::Email => osc8::link_url(link),
        Target::LocalDoc { path, anchor } => {
            let mut s = resolve(base, path).display().to_string();
            if let Some(a) = anchor {
                s.push('#');
                s.push_str(a);
            }
            Some(s)
        }
        Target::LocalFile(path) => Some(resolve(base, path).display().to_string()),
        Target::Anchor(name) => Some(format!("#{name}")),
        Target::Footnote(_) => None,
    }
    .map(|s| crate::text::sanitize(&s).into_owned())
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

    fn lay(md: &str, width: u16) -> (Document, Layout, Derived) {
        let mut doc = parse(md, &ParseOptions::default());
        doc.base_dir = Some(PathBuf::from("/docs"));
        let l = layout(
            &doc,
            width,
            &Theme::test(),
            &Caps::full(),
            &RenderOptions::default(),
            &PlainHighlighter,
            &NoImages,
        );
        let d = Derived::new(&l);
        (doc, l, d)
    }

    #[test]
    fn labels_are_prefix_free() {
        assert_eq!(hint_labels(3), ["a", "s", "d"]);
        assert_eq!(hint_labels(10).last().map(String::as_str), Some(";"));
        let two = hint_labels(11);
        assert_eq!(two.len(), 11);
        assert!(two.iter().all(|l| l.chars().count() == 2));
        assert_eq!(hint_labels(500).len(), 100);
        assert!(hint_labels(0).is_empty());
    }

    #[test]
    fn wrapped_links_are_one_occurrence() {
        let (_, l, d) = lay(
            "[a link that wraps over lines](https://example.com) end",
            16,
        );
        assert!(l.link_hits.len() >= 2, "{:?}", l.link_hits);
        assert_eq!(d.links.len(), 1);
        assert_eq!(d.links[0].hits as usize, l.link_hits.len());
        let second = &l.link_hits[1];
        assert_eq!(
            occurrence_at(&l, &d, second.line as usize, second.cols.start),
            Some(0)
        );
        assert_eq!(occurrence_at(&l, &d, 99, 0), None);
    }

    #[test]
    fn targets() {
        let md = "# Top\n\n[up](#top) [file](guide.md#install) [dir](sub/) [img](a.png) \
                  [web](https://example.com) [bad](javascript:alert(1)) [gone](#nowhere) \
                  note[^n]\n\n## Install\n\n[^n]: The note.";
        let (doc, l, d) = lay(md, 200);
        let base = Path::new("/docs");
        let follow: Vec<Follow> = d.links.iter().map(|o| target(&doc, base, &l, o)).collect();
        assert_eq!(follow[0], Follow::Line(0));
        assert_eq!(
            follow[1],
            Follow::Load {
                path: PathBuf::from("/docs/guide.md"),
                anchor: Some("install".into())
            }
        );
        assert_eq!(
            follow[2],
            Follow::Load {
                path: PathBuf::from("/docs/sub/"),
                anchor: None
            }
        );
        assert_eq!(follow[3], Follow::File(PathBuf::from("/docs/a.png")));
        assert_eq!(follow[4], Follow::Open("https://example.com".into()));
        assert!(matches!(follow[5], Follow::Nowhere(_)), "{:?}", follow[5]);
        assert_eq!(follow[6], Follow::Nowhere("#nowhere".into()));
        // The footnote reference leads to the note, its back-link back.
        let Follow::Line(note) = follow[7] else {
            panic!("{:?}", follow[7]);
        };
        assert!(
            l.line_text(note).contains("The note."),
            "{}",
            l.line_text(note)
        );
        let back = d.links.iter().find(|o| o.back).unwrap();
        assert_eq!(
            target(&doc, base, &l, back),
            Follow::Line(2),
            "the reference"
        );
        assert_eq!(
            copy_text(&doc, base, &d.links[1]).as_deref(),
            Some("/docs/guide.md#install")
        );
        assert_eq!(copy_text(&doc, base, &d.links[7]), None);
    }

    #[test]
    fn anchors_resolve_like_github() {
        let (doc, l, _) = lay("# Intro\n\n## Intro\n\ntext", 40);
        assert_eq!(anchor_line(&doc, &l, "intro"), Some(0));
        let second = anchor_line(&doc, &l, "#intro-1").unwrap();
        assert!(second > 0);
        assert_eq!(anchor_line(&doc, &l, ""), Some(0));
        assert_eq!(anchor_line(&doc, &l, "missing"), None);
    }
}
