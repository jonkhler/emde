//! Figures: a placeholder box of exactly the image's final size, centred,
//! with a caption.
//!
//! The size comes from [`ImageSizer::cells`](super::ImageSizer::cells)
//! (header dimensions only): the box reserves those cells so nothing
//! reflows when the pixels arrive, and [`Layout::images`](super::Layout)
//! says where they go. Without a size (images off, unknown file) the figure
//! is a one-line alt text box. The caption is the title, else the alt text
//! (not repeated when the box already shows it).

use crate::ir::{Figure, ImageRef};
use crate::options::Height;
use crate::style::StyleId;
use crate::theme::Element;

use super::build::{Builder, cut_to_cols};
use super::inline::Composed;
use super::{Fill, LineKind, Placement};

/// Figure height cap when the screen height is unknown.
const DEFAULT_MAX_ROWS: u16 = 30;

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Builder<'_> {
    /// The tallest figure allowed.
    fn max_image_rows(&self) -> u16 {
        match self.opts.images.max_height {
            Height::Rows(n) => n.max(1),
            Height::Percent(p) => match self.caps.size {
                Some((_, rows)) => {
                    let r = u32::from(rows) * u32::from(p.min(100)) / 100;
                    u16::try_from(r).unwrap_or(u16::MAX).max(1)
                }
                None => DEFAULT_MAX_ROWS,
            },
        }
    }

    /// A figure.
    pub(super) fn figure(&mut self, f: &Figure) {
        let doc = self.doc;
        let off = self.off;
        let Some(img) = doc.image(f.image) else {
            return;
        };
        let caption = doc.caption(f);
        let alt = alt_label(img);
        let label = match self.figure_ref(img) {
            Some(n) => format!("{alt}[{n}]"),
            None => alt.clone(),
        };
        let width = self.avail();
        let max_rows = self.max_image_rows();
        let sized = self
            .sizer
            .cells(f.image, width, max_rows)
            .filter(|&(c, r)| c > 0 && r > 0)
            .map(|(c, r)| (c.min(width), r.min(max_rows)));
        let shows_label = match sized {
            Some((cols, rows)) => self.image_box(f, img, &label, cols, rows, off),
            None => {
                self.alt_box(img, &label, off);
                true
            }
        };
        if !caption.is_empty() && !(shows_label && sized.is_none() && caption == alt) {
            let style = self.sty.el(Element::ImageCaption);
            let c = self.plain_composed(caption, style);
            self.aligned_lines(&c, crate::ir::HAlign::Center, off);
        }
        self.off = off.saturating_add(to_u32(caption.len()));
    }

    /// The number of a linked figure's reference, when references are on.
    fn figure_ref(&mut self, img: &ImageRef) -> Option<u32> {
        let doc = self.doc;
        let l = img.link?;
        let link = doc.link(l)?;
        (self.refs.enabled && super::build::wants_ref(link)).then(|| self.refs.number(l, &link.url))
    }

    /// The placeholder box of a sized image; returns whether the label is
    /// shown in it.
    fn image_box(
        &mut self,
        f: &Figure,
        img: &ImageRef,
        label: &str,
        cols: u16,
        rows: u16,
        off: u32,
    ) -> bool {
        let width = self.avail();
        let pad = width.saturating_sub(cols) / 2;
        let frame = self.sty.el(Element::ImageFrame);
        let alt = self.sty.el(Element::ImageAlt);
        let b = self.deco.frame;
        let placement = to_u32(self.out.images.len());
        let amb = self.amb;
        let boxed = rows >= 3 && cols >= 3;
        let label_row = if boxed { rows / 2 } else { 0 };
        let inner = if boxed { cols - 2 } else { cols };
        let text = format!("{} {label}", self.deco.chip);
        let (shown, shown_w) = cut_to_cols(&text, inner, amb);
        let shown = shown.to_string();
        for row in 0..rows {
            self.begin();
            self.placed();
            if row == 0 {
                let col = self.right_edge().saturating_sub(width).saturating_add(pad);
                self.out.images.push(Placement {
                    image: f.image,
                    line: to_u32(self.out.lines.len()),
                    col,
                    cols,
                    rows,
                });
            }
            self.spaces(pad, StyleId(0));
            let (left, right) = if !boxed {
                ("", "")
            } else if row == 0 {
                (b.top_left, b.top_right)
            } else if row + 1 == rows {
                (b.bottom_left, b.bottom_right)
            } else {
                (b.vertical, b.vertical)
            };
            self.put_known(left, u16::from(!left.is_empty()), frame);
            let edge = boxed && (row == 0 || row + 1 == rows);
            if edge {
                self.repeat(b.horizontal, inner, frame);
            } else if row == label_row {
                let before = inner.saturating_sub(shown_w) / 2;
                self.spaces(before, StyleId(0));
                self.put(&shown, alt, img.link);
                self.spaces(inner - before - shown_w, StyleId(0));
            } else {
                self.spaces(inner, StyleId(0));
            }
            self.put_known(right, u16::from(!right.is_empty()), frame);
            let kind = LineKind::Image { placement, row };
            self.end(kind, Fill::None, off);
        }
        !shown.is_empty()
    }

    /// A one-line box with the alt text, for images that cannot be shown.
    fn alt_box(&mut self, img: &ImageRef, label: &str, off: u32) {
        let glyph = self.sty.el(Element::ImageFrame);
        let mut alt = self.sty.el(Element::ImageAlt);
        if img.link.is_some() {
            // A linked figure looks like a link.
            let flags = crate::ir::InlineFlags::empty();
            alt = self.sty.inline(alt, flags, super::style::Piece::Text, true);
        }
        let chip = format!("{}\u{a0}", self.deco.chip);
        let mut c = self.plain_composed(&chip, glyph);
        if let Some(l) = img.link {
            c = with_link(c, l);
        }
        let mut t = self.plain_composed(label, alt);
        if let Some(l) = img.link {
            t = with_link(t, l);
        }
        let joined = Composed::concat(&[&c, &t]).spaced();
        self.aligned_lines(&joined, crate::ir::HAlign::Center, off);
    }
}

/// Composed text with every run linked.
fn with_link(c: Composed<'static>, link: crate::ir::LinkId) -> Composed<'static> {
    let mut c = c;
    for r in &mut c.runs {
        r.link = Some(link);
    }
    c
}

/// What the box says: the alt text, else the file name (whitespace as
/// single spaces, no controls or soft hyphens).
fn alt_label(img: &ImageRef) -> String {
    let path = img.src.split(['?', '#']).next().unwrap_or("");
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let label = if img.alt.is_empty() { name } else { &img.alt };
    let label = crate::text::strip_soft_hyphens(&crate::text::sanitize(label))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if label.is_empty() {
        "image".to_string()
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_fall_back_to_the_file_name() {
        let img = |src: &str, alt: &str| ImageRef {
            src: src.into(),
            alt: alt.into(),
            ..ImageRef::default()
        };
        assert_eq!(alt_label(&img("a/b.png", "Alt")), "Alt");
        assert_eq!(alt_label(&img("a/b.png?x=1#y", "")), "b.png");
        assert_eq!(alt_label(&img("", "")), "image");
        assert_eq!(alt_label(&img("x/", "")), "x");
        assert_eq!(alt_label(&img("a\tb\u{ad}c.png", "")), "a bc.png");
    }
}
