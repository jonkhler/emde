//! Images in the pager.
//!
//! For now the pager only *sizes* figures, through an [`ImageProvider`]
//! (layout reserves a box of exactly the final size, so nothing reflows
//! when pixels arrive). Drawing them — text rasters as rows, kitty
//! placeholders, and the protocols that need a redraw after scrolling —
//! hooks in later: [`super::view::Frame::images`] turns the scroll fast
//! path off while such an image is visible, and
//! [`super::term::set_cleanup`] takes the kitty deletions for exit.
//!
//! `i` cycles the image mode ([`cycle`]): the configured one, blocks, and
//! none (alt text boxes).

use crate::ir::{Document, ImageId};
use crate::layout::{ImageSizer, NoImages};
use crate::options::ImageMode;

/// Sizes figures for the pager. Unlike [`ImageSizer`], it is told the
/// document, since the pager shows several documents in turn.
pub trait ImageProvider {
    /// The size in cells a figure of `image` in `doc` takes, at most
    /// `max_cols` × `max_rows`; `None` when it cannot be shown (not found,
    /// not an image).
    fn figure_cells(
        &self,
        doc: &Document,
        image: ImageId,
        max_cols: u16,
        max_rows: u16,
    ) -> Option<(u16, u16)>;
}

impl ImageProvider for NoImages {
    fn figure_cells(&self, _: &Document, _: ImageId, _: u16, _: u16) -> Option<(u16, u16)> {
        None
    }
}

/// An [`ImageSizer`] for one document.
pub(crate) struct Sizer<'a> {
    pub(crate) provider: &'a dyn ImageProvider,
    pub(crate) doc: &'a Document,
    /// `false` in [`ImageMode::None`]: every figure is its alt text.
    pub(crate) enabled: bool,
}

impl ImageSizer for Sizer<'_> {
    fn cells(&self, image: ImageId, max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
        if !self.enabled {
            return None;
        }
        self.provider
            .figure_cells(self.doc, image, max_cols, max_rows)
    }
}

/// The modes `i` goes through, starting with the configured one.
pub(crate) fn cycle(configured: ImageMode) -> Vec<ImageMode> {
    let mut modes = vec![configured];
    for m in [ImageMode::Blocks, ImageMode::None, ImageMode::Auto] {
        if !modes.contains(&m) {
            modes.push(m);
        }
    }
    modes
}

/// A mode's name, as the config spells it.
pub(crate) fn mode_name(mode: ImageMode) -> &'static str {
    match mode {
        ImageMode::Auto => "auto",
        ImageMode::Kitty => "kitty",
        ImageMode::Iterm => "iterm",
        ImageMode::Sixel => "sixel",
        ImageMode::Blocks => "blocks",
        ImageMode::None => "none (alt text)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;

    impl ImageProvider for Fixed {
        fn figure_cells(&self, _: &Document, _: ImageId, c: u16, r: u16) -> Option<(u16, u16)> {
            Some((c.min(10), r.min(4)))
        }
    }

    #[test]
    fn sizer_follows_the_mode() {
        let doc = Document::default();
        let on = Sizer {
            provider: &Fixed,
            doc: &doc,
            enabled: true,
        };
        assert_eq!(on.cells(ImageId(0), 80, 20), Some((10, 4)));
        let off = Sizer {
            enabled: false,
            ..on
        };
        assert_eq!(off.cells(ImageId(0), 80, 20), None);
        assert_eq!(NoImages.figure_cells(&doc, ImageId(0), 80, 20), None);
    }

    #[test]
    fn cycles_start_with_the_configured_mode() {
        assert_eq!(
            cycle(ImageMode::Auto),
            [ImageMode::Auto, ImageMode::Blocks, ImageMode::None]
        );
        assert_eq!(
            cycle(ImageMode::Kitty),
            [
                ImageMode::Kitty,
                ImageMode::Blocks,
                ImageMode::None,
                ImageMode::Auto
            ]
        );
        assert_eq!(mode_name(ImageMode::None), "none (alt text)");
    }
}
