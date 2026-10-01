//! SVG images: recognising them, and (with the `svg` feature) their
//! intrinsic size and pixels, made with resvg.
//!
//! Recognising an SVG is always built, so that a build without the feature
//! can say why a figure keeps its alt text. With the feature:
//!
//! * [`dimensions`] is the size the SVG asks for (its `width`/`height`, or
//!   its `viewBox`), which sizes the figure like a raster image's header;
//! * [`rasterize`] draws it at exactly the pixel size the caller asks for
//!   (the figure's box, or twice it for kitty and iTerm2), on a transparent
//!   background, so the block raster and every pixel protocol take it like
//!   any decoded image.
//!
//! SVG files are documents that can point elsewhere, so parsing is fenced
//! in: files over [`MAX_BYTES`] are refused, `<image>` elements that name
//! a file or URL are dropped (nothing is read or fetched; usvg makes no
//! network requests), embedded raster images over the pixel limit are
//! dropped before anything decodes them, gzip-compressed SVG (`.svgz`) is
//! not inflated, and usvg itself stops at a million elements and a nesting
//! depth of 1024. System fonts are loaded only for an SVG with text, once
//! per run, on the thread that draws it ([`rasterize`] runs on the image
//! workers). Parsing and drawing run inside [`crate::panic::guarded`].

/// Largest SVG file drawn (the same limit as for fetched images).
pub const MAX_BYTES: usize = 20 << 20;

/// How much of a file [`sniff`] looks at.
const SNIFF_BYTES: usize = 4096;

/// Whether the bytes look like an SVG document: XML (after an optional
/// byte order mark and white space) with an `<svg` element near the start.
pub fn sniff(bytes: &[u8]) -> bool {
    let head = bytes
        .get(..bytes.len().min(SNIFF_BYTES))
        .unwrap_or_default();
    let head = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    let start = head.iter().position(|b| !b.is_ascii_whitespace());
    start.is_some_and(|i| head.get(i) == Some(&b'<'))
        && memchr::memmem::find(head, b"<svg").is_some()
}

/// Whether an image source names an SVG: a `data:image/svg+xml` URI, or a
/// path or URL ending in `.svg` (before any query or fragment).
pub fn named_svg(src: &str) -> bool {
    let src = src.trim();
    if let Some(meta) = src
        .get(..5)
        .filter(|s| s.eq_ignore_ascii_case("data:"))
        .and_then(|_| src.get(5..))
    {
        let mime = meta.split([';', ',']).next().unwrap_or("");
        return mime.trim().eq_ignore_ascii_case("image/svg+xml");
    }
    let path = src.split(['?', '#']).next().unwrap_or(src);
    path.len() > 4
        && path
            .get(path.len() - 4..)
            .is_some_and(|ext| ext.eq_ignore_ascii_case(".svg"))
}

/// Deepest element nesting drawn. usvg stops at 1024 levels, but parts of
/// it recurse once per level before it gets there, deeply enough to
/// overflow the stack of a worker thread; real drawings stay far below.
pub const MAX_DEPTH: usize = 256;

/// Why the markup of an SVG is refused before it is parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Markup {
    /// Elements nested more than [`MAX_DEPTH`] levels deep.
    #[error("elements are nested more than {MAX_DEPTH} levels deep")]
    TooDeep,
    /// An entity declared in the document type expands to markup, which
    /// could hide any amount of nesting.
    #[error("an entity expands to markup")]
    MarkupEntity,
}

/// Check the element nesting of an XML document without parsing it (and
/// so without recursion): at most [`MAX_DEPTH`] levels, and no entities
/// that expand to markup. Comments, CDATA sections, processing
/// instructions and quoted attribute values are skipped; malformed markup
/// is left for the parser to refuse.
pub fn check_markup(bytes: &[u8]) -> Result<(), Markup> {
    use memchr::memmem::find;
    // The end of a construct opening at `at`, after `close`; else the end.
    let past = |at: usize, close: &[u8]| {
        bytes
            .get(at..)
            .and_then(|rest| find(rest, close))
            .map_or(bytes.len(), |i| at + i + close.len())
    };
    let mut depth = 0usize;
    let mut i = 0;
    while let Some(lt) = bytes.get(i..).and_then(|rest| memchr::memchr(b'<', rest)) {
        let at = i + lt;
        let rest = &bytes[at..];
        i = if rest.starts_with(b"<!--") {
            past(at + 4, b"-->")
        } else if rest.starts_with(b"<![CDATA[") {
            past(at + 9, b"]]>")
        } else if rest.starts_with(b"<?") {
            past(at + 2, b"?>")
        } else if rest.starts_with(b"<!") {
            let end = declaration_end(bytes, at);
            if markup_entities(bytes.get(at..end).unwrap_or_default()) {
                return Err(Markup::MarkupEntity);
            }
            end
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            past(at + 2, b">")
        } else {
            let (end, empty) = tag_end(bytes, at + 1);
            if !empty {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(Markup::TooDeep);
                }
            }
            end
        };
    }
    Ok(())
}

/// The end of the start tag whose name begins at `from` (past its `>`,
/// skipping quoted attribute values), and whether it is empty (`/>`).
fn tag_end(bytes: &[u8], from: usize) -> (usize, bool) {
    let mut quote = None;
    let mut prev = 0u8;
    for (i, &b) in bytes.iter().enumerate().skip(from) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return (i + 1, prev == b'/'),
            None => {}
        }
        prev = b;
    }
    (bytes.len(), false)
}

/// The end of a `<!…>` declaration at `at`: a `<!DOCTYPE` with an internal
/// subset ends at the `]` and `>` that close it.
fn declaration_end(bytes: &[u8], at: usize) -> usize {
    let mut quote = None;
    let mut subset = false;
    for (i, &b) in bytes.iter().enumerate().skip(at + 2) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'[' => subset = true,
            None if b == b']' => subset = false,
            None if b == b'>' && !subset => return i + 1,
            None => {}
        }
    }
    bytes.len()
}

/// Whether a declaration (a whole `<!DOCTYPE …>`) declares an entity whose
/// value holds markup: a `<`, or a character reference that becomes one.
fn markup_entities(decl: &[u8]) -> bool {
    use memchr::memmem::find_iter;
    find_iter(decl, b"<!ENTITY").any(|start| {
        let body = decl.get(start + 8..).unwrap_or_default();
        let end = body.iter().position(|&b| b == b'>').unwrap_or(body.len());
        let body = body.get(..end).unwrap_or_default();
        // The value is quoted, so a `<` in it ends the search above at
        // most at the `>` of the markup: look at what is before.
        body.contains(&b'<') || find_iter(body, b"&#").next().is_some()
    })
}

#[cfg(feature = "svg")]
pub use render::{SvgError, dimensions, rasterize};

#[cfg(feature = "svg")]
mod render {
    use std::sync::{Arc, OnceLock};

    use resvg::tiny_skia::{Pixmap, Transform};
    use resvg::usvg::{ImageHrefResolver, ImageKind, Options, Tree, fontdb};

    use super::MAX_BYTES;
    use crate::gfx::Rgba;
    use crate::panic::guarded;

    /// Why an SVG cannot be drawn.
    #[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
    pub enum SvgError {
        /// The file is larger than [`MAX_BYTES`].
        #[error("SVG is larger than {} MiB", MAX_BYTES >> 20)]
        TooBig,
        /// usvg cannot read it (not UTF-8, not SVG, no size, too many elements).
        #[error("cannot read SVG: {0}")]
        Invalid(String),
        /// The requested size has no pixels, or more than allowed.
        #[error("SVG cannot be drawn at {width}×{height} pixels (at most {max_pixels})")]
        BadSize {
            width: u32,
            height: u32,
            max_pixels: u64,
        },
        /// Refused before parsing ([`super::check_markup`]).
        #[error("SVG refused: {0}")]
        Refused(#[from] super::Markup),
        /// The renderer panicked (a bug in it); the message was recorded by
        /// the panic hook.
        #[error("SVG renderer failed")]
        Panicked,
    }

    /// The SVG's intrinsic size in pixels (its `width` and `height`, else
    /// its `viewBox`, at 96 dpi), rounded up and at least 1×1. Fonts are not
    /// loaded: text does not change the size.
    pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32), SvgError> {
        let tree = guarded(|| parse(bytes, None, 0)).unwrap_or(Err(SvgError::Panicked))?;
        let size = tree.size();
        let side = |v: f32| {
            // `as` saturates; usvg guarantees a positive, finite size.
            (v.ceil() as u32).max(1)
        };
        Ok((side(size.width()), side(size.height())))
    }

    /// Draw the SVG at `width × height` pixels (stretching it by less than
    /// a pixel when that size is a rounded fit of its own), on a transparent
    /// background. Refuses sizes with more than `max_pixels` pixels, and
    /// drops embedded raster images that have more.
    pub fn rasterize(
        bytes: &[u8],
        (width, height): (u32, u32),
        max_pixels: u64,
    ) -> Result<Rgba, SvgError> {
        let bad = SvgError::BadSize {
            width,
            height,
            max_pixels,
        };
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > max_pixels {
            return Err(bad);
        }
        guarded(|| {
            let fonts = has_text(bytes).then(system_fonts);
            let tree = parse(bytes, fonts, max_pixels)?;
            let mut pixmap = Pixmap::new(width, height).ok_or(bad)?;
            let size = tree.size();
            let scale =
                Transform::from_scale(width as f32 / size.width(), height as f32 / size.height());
            resvg::render(&tree, scale, &mut pixmap.as_mut());
            let pixels: Vec<u8> = pixmap
                .pixels()
                .iter()
                .flat_map(|p| {
                    let c = p.demultiply();
                    [c.red(), c.green(), c.blue(), c.alpha()]
                })
                .collect();
            Rgba::new(width, height, pixels)
                .ok_or_else(|| SvgError::Invalid("renderer returned a short buffer".into()))
        })
        .unwrap_or(Err(SvgError::Panicked))
    }

    /// Parse with emde's limits; embedded raster images over `max_pixels`
    /// (any at all when it is 0) are dropped.
    fn parse(
        bytes: &[u8],
        fonts: Option<Arc<fontdb::Database>>,
        max_pixels: u64,
    ) -> Result<Tree, SvgError> {
        if bytes.len() > MAX_BYTES {
            return Err(SvgError::TooBig);
        }
        super::check_markup(bytes)?;
        let mut opts = options(max_pixels);
        if let Some(db) = fonts {
            opts.fontdb = db;
        }
        Tree::from_data(bytes, &opts).map_err(|e| SvgError::Invalid(e.to_string()))
    }

    /// usvg options that never reach outside the document: no resources
    /// directory, `<image>` files and URLs ignored, embedded raster images
    /// only when their header shows at most `max_pixels` pixels.
    fn options(max_pixels: u64) -> Options<'static> {
        let embedded = ImageHrefResolver::default_data_resolver();
        Options {
            resources_dir: None,
            image_href_resolver: ImageHrefResolver {
                resolve_data: Box::new(move |mime, data, opts| {
                    let raster = crate::gfx::decode::format(&data).is_some();
                    if raster {
                        let (w, h) = crate::gfx::decode::dimensions(&data)?;
                        if u64::from(w) * u64::from(h) > max_pixels {
                            return None;
                        }
                    }
                    let kind = embedded(mime, data, opts)?;
                    // Only formats that were sniffed: a mime type alone
                    // does not get bytes to a raster decoder.
                    match kind {
                        ImageKind::SVG(_) => Some(kind),
                        _ if raster => Some(kind),
                        _ => None,
                    }
                }),
                resolve_string: Box::new(|_, _| None),
            },
            ..Options::default()
        }
    }

    /// Whether the SVG may draw text (and so needs fonts).
    fn has_text(bytes: &[u8]) -> bool {
        memchr::memmem::find(bytes, b"<text").is_some()
            || memchr::memmem::find(bytes, b":text").is_some()
    }

    /// The system's fonts, loaded the first time an SVG needs them.
    fn system_fonts() -> Arc<fontdb::Database> {
        static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
        Arc::clone(FONTS.get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing() {
        assert!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"));
        assert!(sniff(b"\xef\xbb\xbf  \n<?xml version=\"1.0\"?>\n<svg/>"));
        assert!(sniff(b"<!-- a comment -->\n<!DOCTYPE svg>\n<svg>"));
        assert!(!sniff(b"<?xml version=\"1.0\"?><feed/>"), "other XML");
        assert!(!sniff(b"hello <svg>"), "not markup");
        assert!(!sniff(b"\x89PNG\r\n\x1a\n<svg"));
        assert!(!sniff(b""));
        let mut late = b"<?xml version=\"1.0\"?>".to_vec();
        late.extend(std::iter::repeat_n(b' ', SNIFF_BYTES));
        late.extend(b"<svg/>");
        assert!(!sniff(&late), "only the start is looked at");
    }

    #[test]
    fn markup_checks() {
        let nested = |depth: usize| format!("{}{}", "<g>".repeat(depth), "</g>".repeat(depth));
        assert_eq!(check_markup(nested(MAX_DEPTH).as_bytes()), Ok(()));
        assert_eq!(
            check_markup(nested(MAX_DEPTH + 1).as_bytes()),
            Err(Markup::TooDeep)
        );
        // Siblings, empty elements, closed elements and skipped constructs
        // do not add up.
        let flat = "<a/><b></b>".repeat(10 * MAX_DEPTH);
        assert_eq!(check_markup(flat.as_bytes()), Ok(()));
        let skipped = format!(
            "<svg><!-- {open} --><![CDATA[{open}]]><?pi {open}?><rect x=\"1>{open}\" y='/>'/></svg>",
            open = "<g>".repeat(MAX_DEPTH + 1)
        );
        assert_eq!(check_markup(skipped.as_bytes()), Ok(()));
        // Entities: plain values are fine (Illustrator declares namespaces
        // so), markup or character references are not.
        let ok = "<!DOCTYPE svg [<!ENTITY ns_svg \"http://www.w3.org/2000/svg\">]><svg/>";
        assert_eq!(check_markup(ok.as_bytes()), Ok(()));
        for bad in [
            "<!DOCTYPE svg [<!ENTITY a \"<g>\">]><svg>&a;</svg>",
            "<!DOCTYPE svg [<!ENTITY a '&#60;g>'>]><svg>&a;</svg>",
            "<!DOCTYPE svg [<!ENTITY ok \"x\"> <!ENTITY a \"&#x3C;g>\">]><svg/>",
        ] {
            assert_eq!(
                check_markup(bad.as_bytes()),
                Err(Markup::MarkupEntity),
                "{bad}"
            );
        }
        // Unterminated constructs end the scan.
        assert_eq!(check_markup(b"<svg><!-- <g><g>"), Ok(()));
        assert_eq!(check_markup(b"<svg x=\"<g>"), Ok(()));
    }

    #[test]
    fn names() {
        assert!(named_svg("logo.svg"));
        assert!(named_svg("https://example.com/Badge.SVG?style=flat#x"));
        assert!(named_svg("DATA:image/svg+xml;base64,PHN2Zz4="));
        assert!(named_svg("data:Image/SVG+XML,%3Csvg%3E"));
        assert!(!named_svg("data:image/png;base64,AAAA"));
        assert!(!named_svg("logo.svg.png"));
        assert!(!named_svg(".svg"));
        assert!(!named_svg("https://img.shields.io/badge/a-b-green"));
    }

    #[cfg(feature = "svg")]
    mod drawing {
        use super::super::*;
        use crate::gfx::Rgba;

        const NS: &str = "xmlns=\"http://www.w3.org/2000/svg\"";

        fn svg(body: &str, attrs: &str) -> Vec<u8> {
            format!("<svg {NS} {attrs}>{body}</svg>").into_bytes()
        }

        fn fixture(name: &str) -> Vec<u8> {
            let path = format!(
                "{}/tests/fixtures/images/{name}",
                env!("CARGO_MANIFEST_DIR")
            );
            std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
        }

        /// Whether every pixel is `rgba`.
        fn all(img: &Rgba, rgba: [u8; 4]) -> bool {
            img.pixels.as_chunks::<4>().0.iter().all(|p| *p == rgba)
        }

        #[test]
        fn sizes_from_width_height_and_view_box() {
            assert_eq!(dimensions(&fixture("logo.svg")), Ok((16, 16)));
            let view_box = svg("", "viewBox=\"0 0 120 40\"");
            assert_eq!(dimensions(&view_box), Ok((120, 40)));
            // width and height win over the viewBox; units are converted at 96 dpi.
            let both = svg("", "width=\"1in\" height=\"48\" viewBox=\"0 0 10 10\"");
            assert_eq!(dimensions(&both), Ok((96, 48)));
            // One side and the viewBox give the other side.
            let one = svg("", "width=\"60\" viewBox=\"0 0 120 40\"");
            assert_eq!(dimensions(&one), Ok((60, 20)));
            // Fractions round up; nothing is ever 0 pixels.
            let tiny = svg("", "width=\"0.2\" height=\"10.5\"");
            assert_eq!(dimensions(&tiny), Ok((1, 11)));
            // No size at all.
            assert!(matches!(
                dimensions(&svg("", "width=\"0\" height=\"0\"")),
                Err(SvgError::Invalid(_))
            ));
        }

        #[test]
        fn the_fixture_rasterizes_at_the_size_asked_for() {
            let logo = fixture("logo.svg");
            for size in [(16, 16), (40, 40), (3, 3)] {
                let img = rasterize(&logo, size, 10_000).unwrap();
                assert_eq!((img.width, img.height), size);
                assert!(all(&img, [255, 0, 0, 255]), "{size:?}");
            }
            // A stretched box: the drawing fills it.
            let img = rasterize(&logo, (8, 4), 100).unwrap();
            assert!(all(&img, [255, 0, 0, 255]));
        }

        #[test]
        fn the_background_is_transparent_and_alpha_straight() {
            let half = svg(
                "<rect width=\"5\" height=\"10\" fill=\"#0000ff\" fill-opacity=\"0.5\"/>",
                "width=\"10\" height=\"10\"",
            );
            let img = rasterize(&half, (10, 10), 100).unwrap();
            assert_eq!(img.pixel(9, 5), [0, 0, 0, 0], "nothing drawn there");
            let [r, g, b, a] = img.pixel(2, 5);
            assert_eq!((r, g), (0, 0));
            assert!(b >= 254, "straight, not premultiplied: {b}");
            assert!((127..=128).contains(&a), "{a}");
        }

        #[test]
        fn limits_on_the_size_asked_for() {
            let logo = fixture("logo.svg");
            for (size, max) in [((0, 5), 100), ((5, 0), 100), ((11, 10), 100)] {
                assert!(
                    matches!(rasterize(&logo, size, max), Err(SvgError::BadSize { .. })),
                    "{size:?}"
                );
            }
            assert!(rasterize(&logo, (10, 10), 100).is_ok());
        }

        #[test]
        fn malformed_and_unsupported_svg() {
            for bad in [
                &b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"><rect"[..],
                b"<?xml version=\"1.0\"?><feed/>",
                b"<svg><unclosed></svg>",
                b"\xff\xfe<\x00s\x00v\x00g\x00",
                b"",
                // gzip (.svgz) is not inflated.
                b"\x1f\x8b\x08\x00\x00\x00\x00\x00",
            ] {
                assert!(
                    matches!(dimensions(bad), Err(SvgError::Invalid(_))),
                    "{bad:?}"
                );
                assert!(rasterize(bad, (4, 4), 100).is_err(), "{bad:?}");
            }
            let mut huge = svg("", "width=\"10\" height=\"10\"");
            huge.resize(MAX_BYTES + 1, b' ');
            assert_eq!(dimensions(&huge), Err(SvgError::TooBig));
            assert_eq!(rasterize(&huge, (4, 4), 100), Err(SvgError::TooBig));
        }

        #[test]
        fn files_and_urls_are_never_read() {
            let icon = format!(
                "{}/tests/fixtures/images/icon.png",
                env!("CARGO_MANIFEST_DIR")
            );
            assert!(std::path::Path::new(&icon).is_file());
            for href in [
                icon.clone(),
                format!("file://{icon}"),
                "icon.png".to_owned(),
                "https://example.com/icon.png".to_owned(),
            ] {
                let doc = svg(
                    &format!("<image href=\"{href}\" width=\"8\" height=\"8\"/>"),
                    "width=\"8\" height=\"8\"",
                );
                let img = rasterize(&doc, (8, 8), 100).unwrap();
                assert!(all(&img, [0; 4]), "{href} was drawn");
            }
            // The same image embedded as a data: URI is drawn.
            let png = std::fs::read(&icon).unwrap();
            let doc = svg(
                &format!(
                    "<image href=\"data:image/png;base64,{}\" width=\"8\" height=\"8\"/>",
                    crate::gfx::b64::encode_string(&png)
                ),
                "width=\"8\" height=\"8\"",
            );
            let img = rasterize(&doc, (8, 8), 100).unwrap();
            assert_eq!(img.pixel(4, 4), [250, 180, 40, 255]);
            // ...unless it has more pixels than allowed.
            let img = rasterize(&doc, (8, 8), 64).unwrap();
            assert_eq!(img.pixel(4, 4), [250, 180, 40, 255], "8×8 is 64 pixels");
            let img = rasterize(&doc, (7, 7), 49).unwrap();
            assert!(all(&img, [0; 4]), "an 8×8 image is over 49 pixels");
        }

        #[test]
        fn embedded_svg_is_drawn() {
            let inner = svg(
                "<rect width=\"4\" height=\"4\" fill=\"lime\"/>",
                "width=\"4\" height=\"4\"",
            );
            let doc = svg(
                &format!(
                    "<image href=\"data:image/svg+xml;base64,{}\" width=\"4\" height=\"4\"/>",
                    crate::gfx::b64::encode_string(&inner)
                ),
                "width=\"4\" height=\"4\"",
            );
            let img = rasterize(&doc, (4, 4), 100).unwrap();
            assert!(all(&img, [0, 255, 0, 255]));
        }

        /// `f` on a thread with the stack of emde's image workers.
        fn on_worker_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
            std::thread::Builder::new()
                .stack_size(2 << 20)
                .spawn(f)
                .unwrap()
                .join()
                .unwrap()
        }

        #[test]
        fn bombs_are_refused_or_bounded() {
            // A viewBox of a billion pixels a side: the intrinsic size is
            // only a hint, and drawing happens at the size asked for.
            let wide = svg(
                "<rect width=\"1e9\" height=\"1e9\" fill=\"red\"/>",
                "viewBox=\"0 0 1e9 1e9\"",
            );
            assert_eq!(dimensions(&wide), Ok((1_000_000_000, 1_000_000_000)));
            let img = rasterize(&wide, (20, 20), 400).unwrap();
            assert!(all(&img, [255, 0, 0, 255]));
            assert!(rasterize(&wide, (1_000_000_000, 1_000_000_000), 40_000_000).is_err());
            // Nesting up to the limit draws, even with groups that each
            // need a layer of their own; deeper is refused before parsing.
            // Neither overflows a worker thread's stack.
            let defs = "<defs><clipPath id=\"c\"><rect width=\"4\" height=\"4\"/></clipPath>\
                        <mask id=\"m\"><rect width=\"4\" height=\"4\" fill=\"white\"/></mask></defs>";
            let nested = |depth: usize| {
                let body = format!(
                    "{defs}{}<rect width=\"4\" height=\"4\" fill=\"red\"/>{}",
                    "<g clip-path=\"url(#c)\" mask=\"url(#m)\" opacity=\"0.999\">".repeat(depth),
                    "</g>".repeat(depth)
                );
                svg(&body, "width=\"4\" height=\"4\"")
            };
            // The <svg> element is a level too.
            let (deep, deeper, deepest) =
                (nested(MAX_DEPTH - 1), nested(MAX_DEPTH), nested(100_000));
            let (ok, refused, refused_too) = on_worker_stack(move || {
                (
                    rasterize(&deep, (4, 4), 100),
                    rasterize(&deeper, (4, 4), 100),
                    dimensions(&deepest),
                )
            });
            let ok = ok.unwrap();
            assert!(
                ok.pixel(2, 2)[0] > 250 && ok.pixel(2, 2)[3] > 0,
                "{:?}",
                ok.pixel(2, 2)
            );
            assert_eq!(refused, Err(SvgError::Refused(Markup::TooDeep)));
            assert_eq!(refused_too, Err(SvgError::Refused(Markup::TooDeep)));
            // Entities cannot smuggle nesting in.
            let entity = format!(
                "<!DOCTYPE svg [<!ENTITY g \"<g><g><g>\">]><svg {NS} width=\"4\" height=\"4\">&g;</svg>"
            );
            assert_eq!(
                dimensions(entity.as_bytes()),
                Err(SvgError::Refused(Markup::MarkupEntity))
            );
            // `<use>` fan-out (10 uses of 10 uses of ... 7 levels: 10⁷
            // elements) stops at usvg's element limit.
            let mut defs = String::from("<rect id=\"l0\" width=\"1\" height=\"1\"/>");
            for level in 1..=7 {
                defs.push_str(&format!("<g id=\"l{level}\">"));
                for _ in 0..10 {
                    defs.push_str(&format!("<use href=\"#l{}\"/>", level - 1));
                }
                defs.push_str("</g>");
            }
            let fan = svg(
                &format!("<defs>{defs}</defs><use href=\"#l7\"/>"),
                "width=\"4\" height=\"4\"",
            );
            let started = std::time::Instant::now();
            let result = on_worker_stack(move || rasterize(&fan, (4, 4), 100));
            assert!(result.is_err(), "{result:?}");
            assert!(started.elapsed() < std::time::Duration::from_secs(20));
        }
    }
}
