//! Images in the pager end to end: scripted keys on a [`FakeTerminal`],
//! the bytes it wrote checked as bytes (protocol sequences, their order)
//! and replayed into a `vt100` screen (block images, boxes, status bar).
//!
//! The image worker runs on its thread as in real use, but the event loop
//! waits for it whenever the terminal is idle
//! ([`PagerImages::wait_when_idle`]), so every run paints the same frames:
//! results arrive at the first quiet moment after they were asked for.

#![cfg(feature = "images")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use emde::gfx::Passthrough;
use emde::gfx::store::StoreOptions;
use emde::options::RenderOptions;
use emde::pager::term::{Chunk, ENTER, EXIT, FakeTerminal, Key, MOUSE_ON, Step};
use emde::pager::{PagerDoc, PagerExit, PagerImages, PagerSession, SYNC_ON, Signals, run_on};
use emde::parse::ParseOptions;
use emde::style::Rgb;
use emde::term::env::Env;
use emde::term::{BlockGlyphSet, Caps, ColorDepth, Graphics};
use emde::theme::{Theme, Variant};
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A fresh directory under the target's temporary directory.
fn temp_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("pager-images-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A PNG of `w × h` pixels of one colour.
fn png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let [r, g, b] = rgb;
    let img = RgbaImage::from_pixel(w, h, image::Rgba([r, g, b, 255]));
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8)
        .unwrap();
    out
}

const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];

/// Image options: `graphics`, 8×16-pixel cells, truecolor half blocks.
fn options(graphics: Graphics) -> StoreOptions {
    StoreOptions {
        graphics,
        glyphs: BlockGlyphSet::Half,
        depth: ColorDepth::TrueColor,
        background: Some(Rgb(0, 0, 0)),
        page: Rgb(0, 0, 0),
        cell_px: Some((8, 16)),
        passthrough: Passthrough::Direct,
        max_pixels: 40_000_000,
        remote: false,
        variant: Variant::Dark,
    }
}

/// A session for the file at `path` with images drawn as `graphics` says
/// (frames wait for the image worker whenever the terminal is idle).
fn session(path: &Path, opts: StoreOptions) -> PagerSession {
    let doc = PagerDoc::load(path, &ParseOptions::default()).unwrap();
    let mut s = PagerSession::new(doc, Theme::test(), Caps::full(), RenderOptions::default());
    s.env = Env::default();
    s.images = PagerImages {
        options: Some(opts),
        stores: Vec::new(),
        wait_when_idle: true,
    };
    s
}

/// Run the script to its end (it must quit).
fn run(mut term: FakeTerminal, session: PagerSession) -> (FakeTerminal, PagerExit) {
    let exit = run_on(&mut term, session, &Signals::new()).expect("the pager runs");
    (term, exit)
}

/// The screen as it was at mark `name` (or at the end without one).
fn screen_at(term: &FakeTerminal, cols: u16, rows: u16, name: Option<&str>) -> vt100::Parser {
    let mut p = vt100::Parser::new(rows, cols, 0);
    for chunk in term.chunks() {
        match chunk {
            Chunk::Write(bytes) => p.process(bytes),
            Chunk::Resize(c, r) => p.screen_mut().set_size(*r, *c),
            Chunk::Mark(m) if Some(m.as_str()) == name => break,
            Chunk::Mark(_) | Chunk::Suspend => {}
        }
    }
    p
}

/// Every byte written between marks `from` and `to` (`None`: the start,
/// the end).
fn written(term: &FakeTerminal, from: Option<&str>, to: Option<&str>) -> Vec<u8> {
    let mut on = from.is_none();
    let mut out = Vec::new();
    for chunk in term.chunks() {
        match chunk {
            Chunk::Mark(m) if Some(m.as_str()) == to => break,
            Chunk::Mark(m) if Some(m.as_str()) == from => on = true,
            Chunk::Write(bytes) if on => out.extend_from_slice(bytes),
            _ => {}
        }
    }
    out
}

/// The frames (synchronized writes), in order.
fn frames(term: &FakeTerminal) -> Vec<&[u8]> {
    term.writes()
        .into_iter()
        .filter(|w| w.starts_with(SYNC_ON))
        .collect()
}

/// How often `needle` occurs in `hay`.
fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// Where `needle` first occurs in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The background colour of the cell at `row`, `col`.
fn bg(p: &vt100::Parser, row: u16, col: u16) -> vt100::Color {
    p.screen()
        .cell(row, col)
        .map_or(vt100::Color::Default, vt100::Cell::bgcolor)
}

/// The text of screen row `row`.
fn row_text(p: &vt100::Parser, row: u16) -> String {
    let (_, cols) = p.screen().size();
    p.screen()
        .rows(0, cols)
        .nth(usize::from(row))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

/// A document with one 128×64 red figure (16×4 cells, captioned "Red") in
/// the middle of some text.
fn red_figure(dir: &Path) -> PathBuf {
    std::fs::write(dir.join("red.png"), png(128, 64, RED)).unwrap();
    let doc = dir.join("doc.md");
    std::fs::write(
        &doc,
        "# Figures\n\nSome text above.\n\n![a red square](red.png \"Red\")\n\nSome text below.\n",
    )
    .unwrap();
    doc
}

#[test]
fn block_images_replace_their_box_once_decoded() {
    let dir = temp_dir("blocks");
    let doc = red_figure(&dir);
    let term = FakeTerminal::new(40, 16).mark("decoded").keys("q");
    let (term, exit) = run(term, session(&doc, options(Graphics::Blocks)));
    assert_eq!(exit, PagerExit::Quit);
    // The first frame never waits for the decode: the box with the alt text.
    let first = String::from_utf8_lossy(frames(&term)[0]).into_owned();
    assert!(first.contains("a red square"), "{first:?}");
    // Once the worker is done, the rows show the image.
    let p = screen_at(&term, 40, 16, Some("decoded"));
    let screen = p.screen().contents();
    assert!(!screen.contains("a red square"), "{screen}");
    let red = (0..16)
        .flat_map(|r| (0..40).map(move |c| (r, c)))
        .filter(|&(r, c)| bg(&p, r, c) == vt100::Color::Rgb(255, 0, 0))
        .count();
    assert_eq!(red, 16 * 4, "{screen}");
}

/// A document with a figure of `image` (its PNG) after `before` and before
/// `after` paragraphs of text, in `dir`.
fn figure_doc(dir: &Path, name: &str, image: &[u8], before: usize, after: usize) -> PathBuf {
    std::fs::write(dir.join(format!("{name}.png")), image).unwrap();
    let mut md = String::from("# Figures\n\n");
    for i in 1..=before {
        md.push_str(&format!("Paragraph {i} before the figure.\n\n"));
    }
    md.push_str(&format!("![{name}]({name}.png \"Caption\")\n\n"));
    for i in 1..=after {
        md.push_str(&format!("Paragraph {i} after the figure.\n\n"));
    }
    let doc = dir.join(format!("{name}.md"));
    std::fs::write(&doc, md).unwrap();
    doc
}

/// The kitty image ids uploaded in `bytes` (`a=T` or `a=t`), in order.
fn uploads(bytes: &[u8]) -> Vec<u32> {
    let text = String::from_utf8_lossy(bytes);
    text.split("\x1b_G")
        .skip(1)
        .filter(|cmd| cmd.starts_with("a=T,") || cmd.starts_with("a=t,"))
        .filter_map(|cmd| {
            let i = cmd.find(",i=")? + 3;
            cmd[i..]
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
        .collect()
}

/// The start of a placeholder row for image `id`: its colour, then the
/// placeholder character.
fn placeholder_start(id: u32) -> Vec<u8> {
    let [_, r, g, b] = id.to_be_bytes();
    let mut out = format!("\x1b[38;2;{r};{g};{b}m").into_bytes();
    out.extend_from_slice("\u{10EEEE}".as_bytes());
    out
}

/// `a=d,d=I` for image `id`.
fn deletion(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes()
}

#[test]
fn placeholders_are_uploaded_once_before_their_rows() {
    let dir = temp_dir("placeholders");
    let doc = figure_doc(&dir, "red", &png(128, 64, RED), 2, 30);
    let term = FakeTerminal::new(40, 16)
        .mark("shown")
        .keys("jjj")
        .mark("scrolled")
        .keys("G")
        .mark("away")
        .keys("g")
        .mark("back")
        .keys("q");
    let (term, exit) = run(term, session(&doc, options(Graphics::KittyPlaceholders)));
    assert_eq!(exit, PagerExit::Quit);
    let out = term.output();
    let ids = uploads(&out);
    assert_eq!(ids.len(), 1, "one upload for one image at one size");
    let id = ids[0];
    let upload = find(&out, b"\x1b_Ga=T,U=1,").unwrap();
    let keys = format!(",i={id},f=100,t=d,c=16,r=4,q=2,");
    assert!(find(&out, keys.as_bytes()).is_some(), "the box's size");
    let first_row = find(&out, &placeholder_start(id)).expect("placeholder rows");
    assert!(upload < first_row, "the upload comes before the rows");
    // In the frame that first shows the rows.
    let frame = frames(&term)
        .into_iter()
        .find(|f| find(f, b"a=T,U=1").is_some())
        .unwrap();
    assert!(find(frame, &placeholder_start(id)).is_some());
    // The rows came back after scrolling away, without a new upload.
    let back = written(&term, Some("away"), Some("back"));
    assert!(find(&back, &placeholder_start(id)).is_some());
    let p = screen_at(&term, 40, 16, Some("back"));
    let rows: Vec<String> = (0..15).map(|r| row_text(&p, r)).collect();
    assert_eq!(
        rows.iter().filter(|r| r.contains('\u{10EEEE}')).count(),
        4,
        "{rows:#?}"
    );
    // Every cell carries all three diacritics (row, column, high byte).
    let cells: Vec<String> = (0..15)
        .flat_map(|r| (0..40).map(move |c| (r, c)))
        .filter_map(|(r, c)| Some(p.screen().cell(r, c)?.contents().to_owned()))
        .filter(|cell| cell.starts_with('\u{10EEEE}'))
        .collect();
    assert_eq!(cells.len(), 16 * 4);
    assert!(
        cells.iter().all(|cell| cell.chars().count() == 4),
        "{cells:?}"
    );
    // On exit the image is deleted, before the terminal is put back.
    let last = *term.writes().last().unwrap();
    let mut want = deletion(id);
    want.extend_from_slice(EXIT);
    assert_eq!(last, want.as_slice());
}

#[test]
fn a_new_size_is_a_new_upload_and_the_old_one_is_deleted() {
    let dir = temp_dir("placeholder-resize");
    // 80 columns of pixels: as wide as the text column, whatever its width.
    let doc = figure_doc(&dir, "wide", &png(640, 160, BLUE), 1, 2);
    let term = FakeTerminal::new(40, 16)
        .mark("wide")
        .resize(30, 16)
        .mark("narrow")
        .keys("q");
    let (term, _) = run(term, session(&doc, options(Graphics::KittyPlaceholders)));
    let first = uploads(&written(&term, None, Some("wide")));
    assert_eq!(first.len(), 1);
    let after = written(&term, Some("wide"), Some("narrow"));
    let second = uploads(&after);
    assert_eq!(second.len(), 1, "the new size is uploaded");
    assert_ne!(first[0], second[0], "under a new id");
    let gone = find(&after, &deletion(first[0])).expect("the old id is deleted");
    assert!(gone < find(&after, b"a=T,U=1").unwrap());
    assert!(find(&after, &placeholder_start(second[0])).is_some());
    assert!(find(&after, &placeholder_start(first[0])).is_none());
    // On exit only the live image is left to delete.
    let last = *term.writes().last().unwrap();
    assert!(last.starts_with(&deletion(second[0])), "{last:?}");
    assert!(find(last, &deletion(first[0])).is_none());
}

#[test]
fn inside_tmux_uploads_are_wrapped_and_rows_are_plain_text() {
    let dir = temp_dir("placeholder-tmux");
    let doc = red_figure(&dir);
    let opts = StoreOptions {
        passthrough: Passthrough::Tmux,
        ..options(Graphics::KittyPlaceholders)
    };
    let term = FakeTerminal::new(40, 16).mark("shown").keys("q");
    let (term, _) = run(term, session(&doc, opts));
    let out = term.output();
    assert!(find(&out, b"\x1bPtmux;\x1b\x1b_Ga=T,U=1,").is_some());
    for (i, w) in out.windows(3).enumerate() {
        if w == b"\x1b_G" {
            assert_eq!(out[i - 1], 0x1b, "an unwrapped APC at byte {i}");
        }
    }
    let id = uploads(&out)[0];
    assert!(
        find(&out, &placeholder_start(id)).is_some(),
        "rows are text"
    );
}

// ---------------------------------------------------------------------------
// Pixels over the text: iTerm2, sixel, kitty classic
// ---------------------------------------------------------------------------

/// An iTerm2 image drawn at a cursor position: `(row, column, height)`
/// (1-based row and column, height in cells).
fn iterm_draws(bytes: &[u8]) -> Vec<(usize, usize, usize)> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    for (i, _) in text.match_indices("\x1b]1337;File=") {
        let before = &text[..i];
        let cup = before.rfind("\x1b[").unwrap();
        let pos = before[cup + 2..].trim_end_matches('H');
        let (row, col) = pos.split_once(';').unwrap();
        let rest = &text[i..];
        let h = rest.find("height=").unwrap() + 7;
        let height: String = rest[h..].chars().take_while(char::is_ascii_digit).collect();
        out.push((
            row.parse().unwrap(),
            col.parse().unwrap(),
            height.parse().unwrap(),
        ));
    }
    out
}

/// A 40×12 terminal on a document with a 7×7-cell figure (64×128 pixels,
/// at most 60% of the screen) from line 6, just above the bottom.
fn tall_figure(name: &str) -> PathBuf {
    let dir = temp_dir(name);
    figure_doc(&dir, "tall", &png(64, 128, RED), 2, 30)
}

#[test]
fn pixels_wait_until_scrolling_stops() {
    let doc = tall_figure("iterm-debounce");
    // Marks come 50 ms after the frame before them: within the debounce.
    let term = FakeTerminal::new(40, 12)
        .wait(ms(300))
        .mark("settled")
        .keys("j")
        .wait(ms(10))
        .keys("j")
        .wait(ms(10))
        .keys("j")
        .mark("burst")
        .wait(ms(300))
        .mark("after")
        .keys("q");
    let (term, _) = run(term, session(&doc, options(Graphics::Iterm)));
    // The first frame waits for nothing: the box, no pixels.
    assert!(iterm_draws(frames(&term)[0]).is_empty());
    // Once the document stood still, the visible part of the image: its
    // first 5 rows, the last of them right above the status bar.
    let start = iterm_draws(&written(&term, None, Some("settled")));
    assert_eq!(start, [(7, 17, 5)]);
    // While the keys come, blocks only (6, then 7, then 7 rows).
    let burst = written(&term, Some("settled"), Some("burst"));
    assert!(iterm_draws(&burst).is_empty());
    assert_eq!(
        count(&burst, b"\x1b[48;2;255;0;0m"),
        6 + 7 + 7,
        "blocks rows"
    );
    // 120 ms after the last one, the whole image (now all on screen).
    let after = written(&term, Some("burst"), Some("after"));
    assert_eq!(iterm_draws(&after), [(4, 17, 7)]);
}

#[test]
fn partly_visible_pixels_are_slices_above_the_status_bar() {
    let doc = tall_figure("iterm-slices");
    let mut term = FakeTerminal::new(40, 12).wait(ms(200));
    // From the top down to the figure's bottom rows at the top of the
    // screen: every position settles before the next.
    for _ in 0..11 {
        term = term.keys("j").wait(ms(200));
    }
    let (term, _) = run(term.keys("q"), session(&doc, options(Graphics::Iterm)));
    let draws = iterm_draws(&term.output());
    let heights: Vec<usize> = draws.iter().map(|d| d.2).collect();
    // Coming into view from the bottom, whole, then leaving at the top.
    assert_eq!(heights, [5, 6, 7, 7, 7, 7, 7, 6, 5, 4, 3, 2], "{draws:?}");
    for (row, col, height) in &draws {
        assert_eq!(*col, 17);
        assert!(
            row + height - 1 <= 11,
            "never over the status bar: {draws:?}"
        );
    }
    // Cut at the top: drawn from the first row.
    assert!(draws.iter().rev().take(5).all(|d| d.0 == 1), "{draws:?}");
}

#[test]
fn the_scroll_fast_path_waits_for_pixels_to_leave() {
    let doc = tall_figure("iterm-fast-path");
    let term = FakeTerminal::new(40, 12)
        .wait(ms(300))
        .keys("j")
        .mark("with")
        .keys("G")
        .wait(ms(300))
        .mark("away")
        .keys("k")
        .mark("without")
        .keys("q");
    let (term, _) = run(term, session(&doc, options(Graphics::Iterm)));
    let region = b"\x1b[1;11r";
    assert_eq!(count(&written(&term, None, Some("with")), region), 0);
    let without = written(&term, Some("away"), Some("without"));
    assert_eq!(count(&without, region), 1, "no image on screen any more");
    assert!(without.len() <= 2048, "{} bytes", without.len());
}

#[test]
fn text_images_keep_the_scroll_fast_path() {
    for graphics in [Graphics::Blocks, Graphics::KittyPlaceholders] {
        let doc = tall_figure(&format!("fast-path-{graphics:?}"));
        let term = FakeTerminal::new(40, 12)
            .wait(ms(300))
            .mark("shown")
            .keys("j")
            .mark("scrolled")
            .keys("q");
        let (term, _) = run(term, session(&doc, options(graphics)));
        let scrolled = written(&term, Some("shown"), Some("scrolled"));
        let text = String::from_utf8_lossy(&scrolled);
        assert_eq!(
            count(&scrolled, b"\x1b[1;11r\x1b[1S"),
            1,
            "{graphics:?}: {text:?}"
        );
        // The exposed row (an image row) and the status bar.
        assert_eq!(text.matches(";1H").count(), 2, "{graphics:?}: {text:?}");
        assert!(
            scrolled.len() <= 2048,
            "{graphics:?}: {} bytes",
            scrolled.len()
        );
    }
}

#[test]
fn pixels_come_back_after_an_overlay_and_a_redraw() {
    let doc = tall_figure("iterm-overlay");
    let term = FakeTerminal::new(40, 12)
        .keys("jjj")
        .wait(ms(300))
        .mark("drawn")
        .keys("h")
        .wait(ms(300))
        .mark("help")
        .keys("q")
        .mark("closed")
        .wait(ms(300))
        .key(Key::ctrl('l'))
        .mark("redrawn")
        .keys("q");
    let (term, _) = run(term, session(&doc, options(Graphics::Iterm)));
    assert_eq!(iterm_draws(&written(&term, None, Some("drawn"))).len(), 1);
    assert!(
        iterm_draws(&written(&term, Some("drawn"), Some("help"))).is_empty(),
        "not over the help"
    );
    assert_eq!(
        iterm_draws(&written(&term, Some("help"), Some("closed"))),
        [(4, 17, 7)],
        "drawn again with the frame that closes the help"
    );
    assert_eq!(
        iterm_draws(&written(&term, Some("closed"), Some("redrawn"))),
        [(4, 17, 7)],
        "and after ^L"
    );
}

#[cfg(feature = "sixel")]
#[test]
fn inside_tmux_at_most_six_sixel_images_are_on_screen() {
    let dir = temp_dir("sixel-tmux");
    std::fs::write(dir.join("dot.png"), png(16, 16, BLUE)).unwrap();
    let mut md = String::from("# Dots\n\n");
    for _ in 0..8 {
        md.push_str("![dot](dot.png)\n\n");
    }
    let doc = dir.join("dots.md");
    std::fs::write(&doc, md).unwrap();
    let opts = StoreOptions {
        passthrough: Passthrough::Tmux,
        ..options(Graphics::Sixel)
    };
    let term = FakeTerminal::new(40, 40)
        .wait(ms(300))
        .mark("drawn")
        .keys("q");
    let (term, _) = run(term, session(&doc, opts));
    let out = written(&term, None, Some("drawn"));
    // Each sixel image starts with its raster attributes: the 2×1-cell box
    // is 16×16 pixels, 12×12 in whole sixel bands.
    assert_eq!(count(&out, b"q\"1;1;12;12"), 6);
    // tmux draws sixel itself: never wrapped for passthrough.
    assert_eq!(count(&out, b"\x1bPtmux;"), 0);
    // Outside tmux all eight are drawn.
    let term = FakeTerminal::new(40, 40)
        .wait(ms(300))
        .mark("drawn")
        .keys("q");
    let (term, _) = run(term, session(&doc, options(Graphics::Sixel)));
    let out = written(&term, None, Some("drawn"));
    assert_eq!(count(&out, b"q\"1;1;12;12"), 8);
}

/// The kitty commands in `bytes` (the keys before `;` or the end).
fn kitty_commands(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    text.split("\x1b_G")
        .skip(1)
        .map(|cmd| {
            let end = cmd.find(['\x1b', ';']).unwrap_or(cmd.len());
            cmd[..end].to_owned()
        })
        .collect()
}

#[test]
fn classic_kitty_images_are_placed_after_each_move() {
    let doc = tall_figure("kitty-classic");
    let term = FakeTerminal::new(40, 12)
        .mark("start")
        .keys("jjj")
        .mark("whole")
        .keys("jjjjjjj")
        .mark("cut")
        .keys("G")
        .mark("away")
        .keys("q");
    let (term, _) = run(term, session(&doc, options(Graphics::KittyClassic)));
    let start = kitty_commands(&written(&term, None, Some("start")));
    let id = uploads(&written(&term, None, Some("start")))[0];
    // Uploaded once, placed cropped to the 5 visible rows (80 of 128 px:
    // the image was sent at its own size), the cursor left alone.
    assert_eq!(start[0], format!("a=t,i={id},f=100,t=d,q=2,m=0"));
    assert_eq!(start[1], format!("a=p,i={id},p=1,c=7,r=5,y=0,h=91,C=1,q=2"));
    // Whole on screen: no crop.
    let whole = kitty_commands(&written(&term, Some("start"), Some("whole")));
    assert_eq!(
        whole.last().unwrap(),
        &format!("a=p,i={id},p=1,c=7,r=7,C=1,q=2")
    );
    // Cut at the top: the bottom rows of the image.
    let cut = kitty_commands(&written(&term, Some("whole"), Some("cut")));
    assert_eq!(
        cut.last().unwrap(),
        &format!("a=p,i={id},p=1,c=7,r=3,y=73,h=55,C=1,q=2")
    );
    // Off screen: the placement is deleted, the image stays.
    let away = kitty_commands(&written(&term, Some("cut"), Some("away")));
    assert_eq!(away, [format!("a=d,d=i,i={id},p=1,q=2")]);
    assert_eq!(uploads(&term.output()), [id], "uploaded once");
    // On exit the image goes.
    assert!(term.writes().last().unwrap().starts_with(&deletion(id)));
}

// ---------------------------------------------------------------------------
// Modes, documents, stopping
// ---------------------------------------------------------------------------

/// The status bar at mark `name` (the last row of a `rows`-row screen).
fn status(term: &FakeTerminal, cols: u16, rows: u16, name: &str) -> String {
    row_text(&screen_at(term, cols, rows, Some(name)), rows - 1)
}

/// How many cells of the screen at mark `name` have background `rgb`.
fn cells_on(term: &FakeTerminal, cols: u16, rows: u16, name: &str, rgb: [u8; 3]) -> usize {
    let p = screen_at(term, cols, rows, Some(name));
    let want = vt100::Color::Rgb(rgb[0], rgb[1], rgb[2]);
    (0..rows)
        .flat_map(|r| (0..cols).map(move |c| (r, c)))
        .filter(|&(r, c)| bg(&p, r, c) == want)
        .count()
}

#[test]
fn i_cycles_detected_blocks_and_off() {
    let doc = tall_figure("cycle");
    let term = FakeTerminal::new(40, 12)
        .wait(ms(300))
        .mark("iterm")
        .keys("i")
        .wait(ms(300))
        .mark("blocks")
        .keys("i")
        .wait(ms(300))
        .mark("off")
        .keys("i")
        .wait(ms(300))
        .mark("again")
        .keys("q");
    let (term, _) = run(term, session(&doc, options(Graphics::Iterm)));
    assert_eq!(iterm_draws(&written(&term, None, Some("iterm"))).len(), 1);
    // Blocks: the rows are painted again as cells, and no pixels follow.
    let blocks = written(&term, Some("iterm"), Some("blocks"));
    assert!(iterm_draws(&blocks).is_empty());
    assert!(status(&term, 40, 12, "blocks").contains("images: blocks"));
    assert_eq!(cells_on(&term, 40, 12, "blocks", RED), 5 * 7);
    // Off: laid out again, the figure is its alt text.
    let off = screen_at(&term, 40, 12, Some("off")).screen().contents();
    assert!(off.contains("▣ tall"), "{off}");
    assert!(status(&term, 40, 12, "off").contains("images: off (alt text)"));
    assert_eq!(cells_on(&term, 40, 12, "off", RED), 0);
    // And back to the detected protocol.
    assert!(status(&term, 40, 12, "again").contains("images: iTerm2"));
    assert_eq!(
        iterm_draws(&written(&term, Some("off"), Some("again"))),
        [(7, 17, 5)]
    );
}

/// Two documents with a figure each: `a.md` (red) and `b.md` (blue);
/// `a.md` links to `b.md`.
fn two_documents(name: &str) -> (PathBuf, PathBuf) {
    let dir = temp_dir(name);
    std::fs::write(dir.join("red.png"), png(128, 64, RED)).unwrap();
    std::fs::write(dir.join("blue.png"), png(128, 64, BLUE)).unwrap();
    let a = dir.join("a.md");
    let b = dir.join("b.md");
    std::fs::write(&a, "# A\n\nSee [b](b.md).\n\n![red](red.png \"Red\")\n").unwrap();
    std::fs::write(&b, "# B\n\n![blue](blue.png \"Blue\")\n").unwrap();
    (a, b)
}

#[test]
fn linked_documents_get_their_own_images() {
    let (a, _) = two_documents("linked");
    let term = FakeTerminal::new(40, 14)
        .mark("a")
        .code(emde::pager::term::KeyCode::Tab)
        .code(emde::pager::term::KeyCode::Enter)
        .mark("b")
        .code(emde::pager::term::KeyCode::Backspace)
        .mark("back")
        .keys("q");
    let (term, _) = run(term, session(&a, options(Graphics::Blocks)));
    assert_eq!(cells_on(&term, 40, 14, "a", RED), 16 * 4);
    assert_eq!(cells_on(&term, 40, 14, "b", BLUE), 16 * 4);
    assert_eq!(cells_on(&term, 40, 14, "b", RED), 0);
    assert_eq!(cells_on(&term, 40, 14, "back", RED), 16 * 4);
}

#[test]
fn colon_n_and_colon_p_go_through_the_files() {
    let (a, b) = two_documents("files");
    let mut s = session(&a, options(Graphics::Blocks));
    s.more = vec![PagerDoc::load(&b, &ParseOptions::default()).unwrap()];
    let term = FakeTerminal::new(40, 14)
        .mark("first")
        .keys(":n\n")
        .mark("second")
        .keys(":n\n")
        .mark("last")
        .keys(":p\n")
        .mark("back")
        .keys(":")
        .mark("prompt")
        .keys("q\n");
    let (term, exit) = run(term, s);
    assert_eq!(exit, PagerExit::Quit, ":q quits");
    assert!(status(&term, 40, 14, "first").contains("a.md (1/2)"));
    assert!(status(&term, 40, 14, "second").contains("b.md (2/2)"));
    assert_eq!(cells_on(&term, 40, 14, "second", BLUE), 16 * 4);
    assert!(status(&term, 40, 14, "last").contains("this is the last file"));
    assert!(status(&term, 40, 14, "back").contains("a.md (1/2)"));
    assert_eq!(cells_on(&term, 40, 14, "back", RED), 16 * 4);
    assert!(
        status(&term, 40, 14, "prompt")
            .trim_start()
            .starts_with(':')
    );
}

#[test]
fn a_stop_from_outside_puts_the_terminal_back_first() {
    use signal_hook::consts::SIGTSTP;
    let doc = tall_figure("sigtstp");
    let signals = Signals::new();
    let mut term = FakeTerminal::new(40, 12)
        .with_signals(signals.clone())
        .mark("shown")
        .step(Step::Signal(SIGTSTP))
        .wait(ms(300))
        .mark("resumed")
        .keys("q");
    let exit = run_on(
        &mut term,
        session(&doc, options(Graphics::KittyPlaceholders)),
        &signals,
    )
    .unwrap();
    assert_eq!(exit, PagerExit::Quit);
    let chunks = term.chunks();
    let stop = chunks.iter().position(|c| *c == Chunk::Suspend).unwrap();
    // Restored before stopping: the kitty image deleted, then the exit
    // sequence, in one write.
    let first = uploads(&written(&term, None, Some("shown")))[0];
    let mut restore = deletion(first);
    restore.extend_from_slice(EXIT);
    assert_eq!(chunks[stop - 1], Chunk::Write(restore));
    // Set up again and painted in full on resume.
    let mut enter = ENTER.to_vec();
    enter.extend_from_slice(MOUSE_ON);
    assert_eq!(chunks[stop + 1], Chunk::Write(enter));
    let Chunk::Write(frame) = &chunks[stop + 2] else {
        panic!("{:?}", chunks[stop + 2]);
    };
    for row in 1..=12 {
        assert!(find(frame, format!("\x1b[{row};1H").as_bytes()).is_some());
    }
    // The image is uploaded again, under a new id.
    let again = uploads(&written(&term, Some("shown"), Some("resumed")));
    assert_eq!(again.len(), 1);
    assert_ne!(again[0], first);
}

#[test]
fn without_job_control_a_stop_is_refused() {
    use signal_hook::consts::SIGTSTP;
    let doc = tall_figure("no-job-control");
    let signals = Signals::new();
    let mut term = FakeTerminal::new(40, 12)
        .with_signals(signals.clone())
        .without_job_control()
        .step(Step::Signal(SIGTSTP))
        .mark("refused")
        .key(Key::ctrl('z'))
        .mark("again")
        .keys("q");
    run_on(
        &mut term,
        session(&doc, options(Graphics::Blocks)),
        &signals,
    )
    .unwrap();
    assert!(!term.chunks().contains(&Chunk::Suspend));
    let refused = status(&term, 40, 12, "refused");
    assert!(refused.contains("cannot suspend"), "{refused}");
    assert!(status(&term, 40, 12, "again").contains("cannot suspend"));
    assert_eq!(*term.writes().last().unwrap(), EXIT);
}

// ---------------------------------------------------------------------------
// The worker in real time, and random scripts
// ---------------------------------------------------------------------------

#[test]
fn results_are_picked_up_as_they_come() {
    let dir = temp_dir("threaded");
    let doc = red_figure(&dir);
    let mut s = session(&doc, options(Graphics::Blocks));
    s.images.wait_when_idle = false;
    // Real time for the worker thread: naps of 25 ms, each between two
    // polls of the event loop.
    let mut term = FakeTerminal::new(40, 16);
    for _ in 0..40 {
        term = term.run(|| std::thread::sleep(ms(25)));
    }
    let (term, _) = run(term.keys("q"), s);
    let out = term.output();
    assert!(
        find(&out, b"\x1b[48;2;255;0;0m").is_some(),
        "the image arrived"
    );
    // While the worker had the job, the loop polled often.
    assert!(term.poll_timeouts().contains(&ms(15)));
    for t in term.poll_timeouts() {
        assert!((ms(1)..=ms(250)).contains(t), "{t:?}");
    }
}

/// A random script: keys (never `q`), resizes, quiet periods, signals.
fn random_script(mut term: FakeTerminal, next: &mut impl FnMut() -> u64) -> FakeTerminal {
    use signal_hook::consts::{SIGCONT, SIGTSTP};
    let chars: Vec<char> = "jjjkkkdufbgG%][}{tnN/?oashHLywxei15 :np\n\x1b"
        .chars()
        .collect();
    for _ in 0..next() % 80 {
        let r = next();
        let pick = (r >> 8) as usize;
        let (a, b) = ((r >> 24) as u16 % 120, (r >> 40) as u16 % 50);
        term = match r % 10 {
            0..=5 => term.key(Key::char(chars[pick % chars.len()])),
            6 => term.resize(a, b),
            7 | 8 => term.wait(ms(r % 300)),
            _ if r.is_multiple_of(3) => term.step(Step::Signal(SIGCONT)),
            _ if r % 3 == 1 => term.step(Step::Signal(SIGTSTP)),
            _ => term.key(Key::ctrl('l')),
        };
    }
    term
}

#[test]
fn random_scripts_with_images_end_with_the_terminal_restored() {
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (a, b) = two_documents("random");
    let tall = tall_figure("random-tall");
    let modes = [
        (Graphics::Blocks, Passthrough::Direct),
        (Graphics::KittyPlaceholders, Passthrough::Direct),
        (Graphics::KittyPlaceholders, Passthrough::Tmux),
        (Graphics::KittyClassic, Passthrough::Direct),
        (Graphics::Iterm, Passthrough::Direct),
        (Graphics::Sixel, Passthrough::Tmux),
        (Graphics::None, Passthrough::Direct),
    ];
    for round in 0..3 {
        for (graphics, passthrough) in modes {
            for doc in [&a, &tall] {
                let opts = StoreOptions {
                    passthrough,
                    ..options(graphics)
                };
                let mut s = session(doc, opts);
                s.more = vec![PagerDoc::load(&b, &ParseOptions::default()).unwrap()];
                s.pager.open = emde::config::OpenCommand::Never;
                let signals = Signals::new();
                let size = (next() % 120 + 1, next() % 50 + 1);
                let term =
                    FakeTerminal::new(size.0 as u16, size.1 as u16).with_signals(signals.clone());
                let mut term = random_script(term, &mut next).keys("\x1b\x1b\x1b:q\n");
                let exit = run_on(&mut term, s, &signals);
                let what = format!("{graphics:?} {passthrough:?} {doc:?}, round {round}");
                assert!(exit.is_ok(), "{what}: {exit:?}");
                assert!(!term.is_raw(), "{what}");
                assert!(term.writes().last().unwrap().ends_with(EXIT), "{what}");
                // Every kitty image uploaded is deleted again (the command
                // is wrapped inside tmux).
                let out = term.output();
                for id in uploads(&out) {
                    let delete = format!("a=d,d=I,i={id},q=2");
                    assert!(find(&out, delete.as_bytes()).is_some(), "{what}: {id}");
                }
            }
        }
    }
}

/// Frames with images, through the whole event loop, at 214×54:
///
/// ```sh
/// cargo test --release --test pager_images -- --ignored --nocapture
/// ```
#[test]
#[ignore = "benchmark: cargo test --release --test pager_images -- --ignored --nocapture"]
fn image_frames_benchmark() {
    use std::io::Write as _;
    use std::time::Instant;
    let dir = temp_dir("bench");
    std::fs::write(dir.join("red.png"), png(320, 160, RED)).unwrap();
    let mut md = String::from("# Benchmark\n\n");
    for i in 0..60 {
        md.push_str(&format!(
            "Paragraph {i} with a few words of text in it.\n\n"
        ));
        md.push_str("![red](red.png \"A caption\")\n\n");
    }
    let doc = dir.join("bench.md");
    std::fs::write(&doc, md).unwrap();
    for graphics in [
        Graphics::Blocks,
        Graphics::KittyPlaceholders,
        Graphics::Iterm,
    ] {
        let n = 1000;
        let mut term = FakeTerminal::new(214, 54).wait(ms(300)).mark("ready");
        for i in 0..n {
            term = term.keys(if i % 200 < 100 { "j" } else { "k" }).wait(ms(1));
        }
        let term = term.mark("done").keys("q");
        let start = Instant::now();
        let (term, _) = run(term, session(&doc, options(graphics)));
        let total = start.elapsed();
        let scrolled = written(&term, Some("ready"), Some("done"));
        let report = format!(
            "{graphics:?}: {:?} per key (frame and event loop), {} bytes per frame\n",
            total / n,
            scrolled.len() / n as usize
        );
        let _ = std::io::stdout().write_all(report.as_bytes());
    }
}
