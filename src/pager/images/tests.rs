//! Tests of the image layer's decisions; the frames it makes with the
//! shell are tested end to end in `tests/pager_images.rs`.

use super::*;
use crate::layout::Placement;

fn options(graphics: Graphics, depth: ColorDepth) -> StoreOptions {
    StoreOptions {
        graphics,
        depth,
        ..StoreOptions::new(
            &crate::term::Caps::full(),
            &crate::options::ImageOptions::default(),
            &crate::theme::Theme::test(),
        )
    }
}

#[test]
fn modes_start_with_the_detected_one() {
    let tc = ColorDepth::TrueColor;
    assert_eq!(
        modes(Some(&options(Graphics::Iterm, tc))),
        [Graphics::Iterm, Graphics::Blocks, Graphics::None]
    );
    assert_eq!(
        modes(Some(&options(Graphics::Blocks, tc))),
        [Graphics::Blocks, Graphics::None]
    );
    // `images = none`: `i` still turns blocks on.
    assert_eq!(
        modes(Some(&options(Graphics::None, tc))),
        [Graphics::None, Graphics::Blocks]
    );
    // Without colours there are no blocks: pixels or alt text.
    assert_eq!(
        modes(Some(&options(Graphics::None, ColorDepth::Mono))),
        [Graphics::None]
    );
    assert_eq!(
        modes(Some(&options(Graphics::Iterm, ColorDepth::Mono))),
        [Graphics::Iterm, Graphics::None]
    );
    assert_eq!(modes(None), [Graphics::None]);
}

#[test]
fn wide_ambiguous_characters_keep_blocks_out_of_the_cycle() {
    let tc = ColorDepth::TrueColor;
    let wide = |graphics| StoreOptions {
        blocks: false,
        ..options(graphics, tc)
    };
    // Pixels still cycle to off, never through blocks.
    assert_eq!(
        modes(Some(&wide(Graphics::Iterm))),
        [Graphics::Iterm, Graphics::None]
    );
    // `refuse_wide_blocks` already turned blocks into none: nothing to cycle.
    assert_eq!(modes(Some(&wide(Graphics::None))), [Graphics::None]);
}

#[test]
fn cycling_says_when_sizes_change() {
    let mut images = Images::new(PagerImages::new(options(
        Graphics::KittyPlaceholders,
        ColorDepth::TrueColor,
    )));
    assert_eq!(images.mode(), Graphics::KittyPlaceholders);
    assert_eq!(images.cycle(), Some((Graphics::Blocks, false)));
    assert_eq!(images.cycle(), Some((Graphics::None, true)));
    assert_eq!(images.cycle(), Some((Graphics::KittyPlaceholders, true)));
    let mut off = Images::new(PagerImages::off());
    assert_eq!(off.mode(), Graphics::None);
    assert_eq!(off.cycle(), None, "nothing to switch to");
    assert_eq!(mode_name(Graphics::None), "off (alt text)");
    assert_eq!(
        mode_name(Graphics::KittyPlaceholders),
        "kitty (placeholders)"
    );
}

#[cfg(feature = "sixel")]
#[test]
fn the_tmux_sixel_limit_is_the_plans() {
    assert_eq!(TMUX_MAX_SIXELS, crate::gfx::sixel::TMUX_MAX_VISIBLE);
}

fn placement(line: u32, rows: u16) -> Placement {
    Placement {
        image: ImageId(0),
        line,
        col: 0,
        cols: 4,
        rows,
    }
}

#[test]
fn placements_on_lines() {
    let layout = Layout {
        images: vec![placement(2, 3), placement(10, 4), placement(20, 1)],
        ..crate::layout::layout(
            &Document::default(),
            20,
            &crate::theme::Theme::test(),
            &crate::term::Caps::full(),
            &crate::options::RenderOptions::default(),
            &crate::highlight::PlainHighlighter,
            &NoImages,
        )
    };
    let on = |lines: Range<usize>| -> Vec<(usize, Range<u16>)> {
        placements_on(&layout, lines)
            .into_iter()
            .map(|(i, _, rows)| (i, rows))
            .collect()
    };
    assert_eq!(on(0..30), [(0, 0..3), (1, 0..4), (2, 0..1)]);
    // Cut at the top and at the bottom.
    assert_eq!(on(3..12), [(0, 1..3), (1, 0..2)]);
    assert_eq!(on(4..5), [(0, 2..3)]);
    assert!(on(5..10).is_empty());
    assert!(on(21..40).is_empty());
    assert!(on(7..7).is_empty());
}

/// A state showing a document with one 128×64 blue data-URI figure.
#[cfg(feature = "images")]
fn state_with_figure(images: &mut Images) -> State {
    use crate::config::PagerOptions;
    use crate::parse::ParseOptions;
    use crate::source::{Origin, Source};
    let blue = crate::gfx::Rgba::filled(128, 64, [0, 0, 255, 255]).unwrap();
    let png = crate::gfx::png::encode(&blue).unwrap();
    let md = format!(
        "# Figure\n\n![blue](data:image/png;base64,{})\n",
        crate::gfx::b64::encode_string(&png)
    );
    let source = Source::from_bytes(md.into_bytes(), Origin::Memory);
    let doc = super::super::PagerDoc::parse(source, &ParseOptions::default());
    let opts = crate::options::RenderOptions::default();
    let sizer = images.sizer(&DocKey::Unnamed(0), &doc.doc);
    let layout = crate::layout::layout(
        &doc.doc,
        40,
        &crate::theme::Theme::test(),
        &crate::term::Caps::full(),
        &opts,
        &crate::highlight::PlainHighlighter,
        sizer,
    );
    let pager = PagerOptions::default();
    let settings = super::super::Settings::new(&pager, &opts);
    State::new(doc, None, layout, (40, 12), &pager, settings)
}

/// One frame through the image layer (with the rows the screen would
/// write): the bytes it puts before and after the rows.
#[cfg(feature = "images")]
fn frame(images: &mut Images, state: &State, screen: &mut Screen) -> (Vec<u8>, Vec<u8>) {
    let ctx = super::super::Ctx::new(&crate::theme::Theme::test(), &crate::term::Caps::full());
    let mut f = super::super::view(state, &ctx);
    let now = Instant::now();
    images.plan(state, &mut f, screen, now);
    let diff = screen.diff(&f);
    images.extras(state, &f, &diff, now)
}

#[cfg(feature = "images")]
#[test]
fn kitty_images_are_uploaded_once_and_deleted_once() {
    let opts = StoreOptions {
        cell_px: Some((8, 16)),
        ..options(Graphics::KittyPlaceholders, ColorDepth::TrueColor)
    };
    let mut images = Images::new(PagerImages::new(opts));
    let state = state_with_figure(&mut images);
    assert_eq!(state.layout().images.len(), 1);
    let mut screen = Screen::new();
    // First frame: the box; the rendition is asked for.
    let (before, after) = frame(&mut images, &state, &mut screen);
    assert!(before.is_empty() && after.is_empty());
    assert!(images.busy());
    assert!(images.settle());
    // Second frame: uploaded before the rows, once.
    let (before, _) = frame(&mut images, &state, &mut screen);
    let text = String::from_utf8_lossy(&before).into_owned();
    assert!(text.starts_with("\x1b_Ga=T,U=1,i="), "{text:?}");
    let id = images.live.values().copied().next().unwrap();
    let cleanup = images.cleanup().unwrap();
    assert_eq!(
        cleanup,
        kitty::delete(id, Deletion::Image, Passthrough::Direct)
    );
    assert_eq!(images.cleanup(), None, "unchanged");
    let (before, _) = frame(&mut images, &state, &mut screen);
    assert!(before.is_empty(), "not uploaded again");
    // ^L: deleted with the next frame, and uploaded again under a new id.
    images.reset(false);
    screen.invalidate();
    let (before, _) = frame(&mut images, &state, &mut screen);
    assert_eq!(
        before,
        kitty::delete(id, Deletion::Image, Passthrough::Direct)
    );
    assert!(images.settle());
    let (before, _) = frame(&mut images, &state, &mut screen);
    let new = images.live.values().copied().next().unwrap();
    assert_ne!(new, id);
    let keys = format!(",i={},", new.get());
    assert!(String::from_utf8_lossy(&before).contains(&keys));
    // The terminal was put back (the cleanup deleted everything): nothing
    // is left to delete.
    images.reset(true);
    assert!(images.live.is_empty() && images.doomed.is_empty());
    assert_eq!(images.cleanup(), Some(Vec::new()));
    // A document the history dropped takes its images along.
    frame(&mut images, &state, &mut screen);
    assert!(images.settle());
    frame(&mut images, &state, &mut screen);
    assert_eq!(images.live.len(), 1);
    images.retain(|_| false);
    assert!(images.live.is_empty());
    assert_eq!(images.doomed.len(), 1);
}
