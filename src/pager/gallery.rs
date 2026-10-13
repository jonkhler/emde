//! The image view's gallery: every image of the current document, figures
//! and links to images alike, in document order and once each.
//!
//! A link leads to an image when its target names one: a local file with
//! an image extension (`png`, `jpg`, `jpeg`, `gif`, `webp`, and `svg` with
//! the `svg` feature), a `data:image/…` URI, or, only with
//! `images.remote`, an `http(s)` URL with such an extension. A local file
//! without one is an image when its first bytes say so ([`sniff`], which
//! the shell runs on files it would otherwise only name). Figures are
//! always in the gallery, whatever their source.
//!
//! Entries are told apart by where the image is ([`Location`]): a figure
//! and a link to the same file are one entry. Each entry carries the
//! [`ImageRef`] the image view loads ([`document`] makes it a document of
//! one figure, which is laid out to fill the screen and centred
//! vertically, [`centre`]).

use std::path::{Component, Path, PathBuf};

use crate::ir::{Block, Document, Figure, ImageId, ImageRef, SrcPos, Target};
use crate::layout::{Fill, Layout, Line, LineKind};

use super::hints::{self, Kind};
use super::state::{Occurrence, State};

/// Image file extensions (lowercase).
const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Whether SVG images are shown (the `svg` feature).
const SVG: bool = cfg!(feature = "svg");

/// Where an image is: what tells the entries of the gallery apart, and
/// what `y` copies and `o` opens.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Location {
    /// A local file (relative to the document's directory when the
    /// document's directory is).
    File(PathBuf),
    /// An `http(s)` URL.
    Url(String),
    /// A `data:` URI.
    Data(String),
}

impl Location {
    /// The location as text: the path, the URL or the URI.
    pub(crate) fn text(&self) -> String {
        match self {
            Location::File(p) => p.display().to_string(),
            Location::Url(s) | Location::Data(s) => s.clone(),
        }
    }
}

/// One image of the gallery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picture {
    /// What the image view loads: the figure's image (with its
    /// `<picture>` sources), or the link's target as an image.
    pub(crate) image: ImageRef,
    pub(crate) at: Location,
    /// As the status bar names it: the source as written (a `data:` URI
    /// as `data:image/png`).
    pub(crate) name: String,
    /// The alt text and caption of a figure, the text of a link.
    pub(crate) label: String,
    /// The figure it is, in the current document (its file may be read
    /// already).
    pub(crate) figure: Option<ImageId>,
}

/// Whether a path (or the path of a URL) ends in an image extension.
fn image_name(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
                || SVG && ext.eq_ignore_ascii_case("svg"))
    })
}

/// Whether a `data:` URI holds an image.
fn image_data(uri: &str) -> bool {
    let mime = uri.get(5..).unwrap_or("").split([';', ',']).next();
    let mime = mime.unwrap_or("").trim().to_ascii_lowercase();
    match mime.strip_prefix("image/") {
        Some("svg+xml") => SVG,
        Some(kind) => ["png", "jpeg", "jpg", "gif", "webp"].contains(&kind),
        None => false,
    }
}

/// Whether the first bytes of a file are those of an image emde reads:
/// PNG, JPEG, GIF, WebP (and SVG with the `svg` feature).
pub(crate) fn sniff(head: &[u8]) -> bool {
    head.starts_with(b"\x89PNG\r\n\x1a\n")
        || head.starts_with(b"\xff\xd8\xff")
        || head.starts_with(b"GIF8")
        || head.get(..4) == Some(b"RIFF") && head.get(8..12) == Some(b"WEBP")
        || SVG && crate::gfx::svg::sniff(head)
}

/// `path` without `.` components (lexically; nothing is read).
fn tidy(path: &Path) -> PathBuf {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// Where an image source written in a document whose directory is `base`
/// is (`None`: a scheme that names no image, an empty source).
pub(crate) fn locate(src: &str, base: &Path) -> Option<Location> {
    let src = src.trim();
    if src
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("data:"))
    {
        return Some(Location::Data(src.to_owned()));
    }
    match crate::parse::classify(src) {
        Target::LocalFile(path) | Target::LocalDoc { path, .. } => {
            Some(Location::File(tidy(&base.join(path))))
        }
        Target::External => {
            if let Some(rest) = src.strip_prefix("//") {
                return crate::render::osc8::encode_url(&format!("https://{rest}"))
                    .map(Location::Url);
            }
            // Only `http(s)`, made safe to pass on (as remote images are).
            let scheme = src.split_once(':').map_or("", |(s, _)| s);
            let web = ["http", "https"]
                .iter()
                .any(|s| scheme.eq_ignore_ascii_case(s));
            web.then(|| crate::render::osc8::encode_url(src))
                .flatten()
                .map(Location::Url)
        }
        Target::Anchor(_) | Target::Email | Target::Footnote(_) => None,
    }
}

/// Whether the image view shows `at` when a link leads there: images by
/// their name, remote ones only with `remote` (`images.remote`; without
/// it the link opens as any other).
fn viewable(at: &Location, remote: bool) -> bool {
    match at {
        Location::File(p) => image_name(&p.to_string_lossy()),
        Location::Url(u) => remote && image_name(u),
        Location::Data(d) => image_data(d),
    }
}

/// A source as the status bar names it.
fn short_name(src: &str) -> String {
    let src = src.trim();
    if src
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("data:"))
    {
        return src.split([';', ',']).next().unwrap_or(src).to_owned();
    }
    src.to_owned()
}

/// The source a figure shows (its first `<picture>` source when it has no
/// `src` of its own).
fn figure_src(img: &ImageRef) -> &str {
    if img.src.trim().is_empty() {
        img.sources.first().map_or("", |s| s.first_url())
    } else {
        &img.src
    }
}

/// A local file as the gallery knows it.
pub(crate) fn file(path: &Path) -> Location {
    Location::File(tidy(path))
}

/// Where the image of figure `id` of the current document is.
pub(crate) fn figure_location(state: &State, id: ImageId) -> Option<Location> {
    let img = state.page.doc.image(id)?;
    locate(figure_src(img), &state.page.base_dir())
}

/// Where link occurrence `o` leads, if the image view shows it.
pub(crate) fn link_location(state: &State, o: &Occurrence) -> Option<Location> {
    if o.back {
        return None;
    }
    let link = state.page.doc.link(o.link)?;
    let at = locate(&link.url, &state.page.base_dir())?;
    viewable(&at, state.settings.remote_images).then_some(at)
}

/// The text of link occurrence `o`, as laid out.
fn link_text(layout: &Layout, o: &Occurrence) -> String {
    let first = o.line as usize;
    let mut out = String::new();
    for line in first..first.saturating_add(o.hits as usize) {
        if line > first {
            out.push(' ');
        }
        for span in layout.line_spans(line) {
            if span.link == Some(o.link) {
                out.push_str(layout.span_text(span));
            }
        }
    }
    out
}

/// A figure's label: its alt text, and its title when that says more.
fn figure_label(img: &ImageRef) -> String {
    let (alt, title) = (img.alt.trim(), img.title.trim());
    let mut label = alt.to_owned();
    if !title.is_empty() && title != alt {
        if !label.is_empty() {
            label.push_str(" — ");
        }
        label.push_str(title);
    }
    label
}

/// The gallery of the current document: its figures and the links to
/// images on its lines, top to bottom, once per location. `extra` is a
/// file a link leads to that is an image although its name does not say
/// so (the shell sniffed it).
pub(crate) fn build(state: &State, extra: Option<&Path>) -> Vec<Picture> {
    let doc = &state.page.doc;
    let layout = &state.layout;
    let base = state.page.base_dir();
    let mut found: Vec<(usize, Picture)> = Vec::new();
    hints::elements(doc, 0..doc.blocks.len(), |el| {
        let Some(Block::Figure(f)) = el.block.filter(|_| el.kind == Kind::Figure) else {
            return;
        };
        let Some(img) = doc.image(f.image) else {
            return;
        };
        let Some(at) = locate(figure_src(img), &base) else {
            return;
        };
        let pos = SrcPos {
            top: el.top,
            off: el.start,
        };
        add(
            &mut found,
            layout.line_at(pos),
            Picture {
                image: img.clone(),
                at,
                name: short_name(figure_src(img)),
                label: figure_label(img),
                figure: Some(f.image),
            },
        );
    });
    for o in &state.derived.links {
        let at = match link_location(state, o) {
            Some(at) => at,
            None => match (extra, doc.link(o.link)) {
                (Some(path), Some(link)) if !o.back => match locate(&link.url, &base) {
                    Some(Location::File(p)) if p == tidy(path) => Location::File(p),
                    _ => continue,
                },
                _ => continue,
            },
        };
        let Some(link) = doc.link(o.link) else {
            continue;
        };
        let image = ImageRef {
            src: link.url.clone(),
            ..ImageRef::default()
        };
        add(
            &mut found,
            o.line as usize,
            Picture {
                image,
                at,
                name: short_name(&link.url),
                label: link_text(layout, o),
                figure: None,
            },
        );
    }
    found.into_iter().map(|(_, p)| p).collect()
}

/// Put `p`, on line `line`, among `found` (kept top to bottom, an entry
/// after those on its line), where the first entry for its location is.
fn add(found: &mut Vec<(usize, Picture)>, line: usize, p: Picture) {
    if let Some(i) = found.iter().position(|(_, o)| o.at == p.at) {
        match found.get(i) {
            // The first one in the document stays.
            Some(&(l, _)) if l <= line => return,
            _ => {
                found.remove(i);
            }
        }
    }
    let at = found.partition_point(|(l, _)| *l <= line);
    found.insert(at, (line, p));
}

/// A document of one figure showing `picture`, its relative sources read
/// from `base_dir`: what the image view lays out. No caption (the status
/// bar names the image).
pub(crate) fn document(picture: &Picture, base_dir: Option<PathBuf>) -> Document {
    let image = ImageRef {
        alt: Box::default(),
        title: Box::default(),
        width: None,
        height: None,
        link: None,
        ..picture.image.clone()
    };
    Document {
        blocks: vec![Block::Figure(Figure { image: ImageId(0) })],
        images: vec![image],
        base_dir,
        ..Document::default()
    }
}

/// Put the figure of the image view's layout (a [`document`]: one figure,
/// no caption, so no links, headings or math to move along) in the middle
/// of `rows` rows: blank lines above it.
pub(crate) fn centre(layout: &mut Layout, rows: usize) {
    let Some(p) = layout.images.first() else {
        return;
    };
    let pad = rows.saturating_sub(usize::from(p.rows)) / 2;
    let Ok(n) = u32::try_from(pad) else {
        return;
    };
    let blank = Line {
        first: 0,
        n: 0,
        cols: 0,
        kind: LineKind::Blank,
        fill: Fill::None,
        pos: SrcPos::default(),
    };
    layout.lines.splice(0..0, std::iter::repeat_n(blank, pad));
    // The figure's block takes the blank lines (the ranges tile the lines).
    for r in layout.block_lines.iter_mut().skip(1) {
        r.start = r.start.saturating_add(n);
    }
    for r in &mut layout.block_lines {
        r.end = r.end.saturating_add(n);
    }
    for p in &mut layout.images {
        p.line = p.line.saturating_add(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_names_and_data() {
        for yes in ["a.png", "x/y.JPG", "p.jpeg?raw=1", "q.gif#x", "w.webp"] {
            assert!(image_name(yes), "{yes}");
        }
        for no in ["a.md", "png", ".png", "a.png/", "a.pdf", "noext"] {
            assert!(!image_name(no), "{no}");
        }
        assert_eq!(image_name("a.svg"), cfg!(feature = "svg"));
        assert!(image_data("data:image/png;base64,AAAA"));
        assert!(image_data("DATA:Image/JPEG,xx"));
        assert!(!image_data("data:text/plain,hello"));
        assert_eq!(
            image_data("data:image/svg+xml,<svg/>"),
            cfg!(feature = "svg")
        );
    }

    #[test]
    fn sniffing() {
        assert!(sniff(b"\x89PNG\r\n\x1a\n...."));
        assert!(sniff(b"\xff\xd8\xff\xe0"));
        assert!(sniff(b"GIF89a"));
        assert!(sniff(b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(!sniff(b"RIFF\0\0\0\0WAVE"));
        assert!(!sniff(b"# Markdown"));
        assert!(!sniff(b""));
        assert_eq!(sniff(b"<svg xmlns='x'/>"), cfg!(feature = "svg"));
    }

    #[test]
    fn locations() {
        let base = Path::new("/docs");
        let file = |p: &str| Some(Location::File(PathBuf::from(p)));
        assert_eq!(locate("img/a.png", base), file("/docs/img/a.png"));
        assert_eq!(
            locate("./img/a%20b.png?x#y", base),
            file("/docs/img/a b.png")
        );
        assert_eq!(locate("/abs/a.png", base), file("/abs/a.png"));
        assert_eq!(locate("file:///tmp/a.png", base), file("/tmp/a.png"));
        assert_eq!(
            locate("https://e.com/a.png", base),
            Some(Location::Url("https://e.com/a.png".into()))
        );
        assert_eq!(
            locate("//cdn.e.com/a.png", base),
            Some(Location::Url("https://cdn.e.com/a.png".into()))
        );
        assert!(matches!(
            locate(" data:image/png;base64,AA", base),
            Some(Location::Data(_))
        ));
        assert_eq!(locate("ftp://e.com/a.png", base), None);
        assert_eq!(locate("#anchor", base), None);
        assert_eq!(locate("mailto:me@e.com", base), None);
        assert_eq!(locate("   ", base), None);
        // Relative base directories stay relative.
        assert_eq!(locate("a.png", Path::new(".")), file("a.png"));
        assert_eq!(short_name("data:image/png;base64,AAAA"), "data:image/png");
        // URLs are made safe to pass on.
        assert_eq!(
            locate("https://e.com/a b.png", base),
            Some(Location::Url("https://e.com/a%20b.png".into()))
        );
        assert_eq!(locate("https://e.com/\u{1b}.png", base), None);
        assert_eq!(
            super::file(Path::new("/d/./x.png")),
            Location::File("/d/x.png".into())
        );
        assert_eq!(short_name(" figures/x.png "), "figures/x.png");
    }

    #[test]
    fn viewable_links() {
        let url = Location::Url("https://e.com/a.png".into());
        assert!(!viewable(&url, false), "remote images are off");
        assert!(viewable(&url, true));
        assert!(!viewable(&Location::Url("https://e.com/".into()), true));
        assert!(viewable(&Location::File("x/a.gif".into()), false));
        assert!(!viewable(&Location::File("x/a.txt".into()), false));
    }

    #[test]
    fn labels() {
        let img = |alt: &str, title: &str| ImageRef {
            alt: alt.into(),
            title: title.into(),
            ..ImageRef::default()
        };
        assert_eq!(figure_label(&img("Loss", "")), "Loss");
        assert_eq!(figure_label(&img("", "Title")), "Title");
        assert_eq!(figure_label(&img("Loss", "Loss")), "Loss");
        assert_eq!(figure_label(&img("Loss", "Training")), "Loss — Training");
    }
}
