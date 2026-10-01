//! The images of one document: where they come from, how big they are, and
//! what their figure rows show.
//!
//! 1. [`ImageStore::load`] reads the images of the document's figures before
//!    layout (in parallel when there are several): local files relative to
//!    the document's directory, `data:` URIs, and, only with
//!    `images.remote`, `http(s)` URLs fetched by a `curl` subprocess
//!    (`fetch_remote`). A `<picture>` shows the `<source>` that matches
//!    the theme's background.
//! 2. Layout sizes each figure through [`ImageSizer`], from the header
//!    dimensions alone (HTML `width`/`height` attributes respected), so
//!    nothing waits for a decode.
//! 3. [`ImageStore::prepare_stream`] renders every placement of a layout
//!    for the chosen graphics path, decoding each image once, on up to one
//!    thread per CPU. Renditions are cached per (image, columns, rows).
//! 4. The emitter shows the rows through [`ImageRows`].
//!
//! What a figure row becomes, by graphics path (stream mode):
//!
//! | Path | Rows |
//! |---|---|
//! | blocks | [`raster::rasterize`] cells |
//! | kitty placeholders | the upload (`a=T,U=1`, wrapped for tmux passthrough), then placeholder text on every row |
//! | kitty classic, iTerm2, sixel | the first row reserves the box (and the row below it) with newlines, moves the cursor back up to the box, saves it, draws the image and restores it; the other rows only move the cursor over the image |
//!
//! Every row leaves the cursor at the box's right edge, as text would, so
//! what the line shows after the box (an alert's tint) can follow.
//!
//! Every kitty command carries `q=2`, and every upload gets a fresh id. A
//! pixel rendition that cannot be made (too large, cannot be encoded) falls
//! back to blocks, and an image that cannot be decoded keeps its
//! placeholder box. SVG images always keep it (see the `svg` feature).
//!
//! A store is made for one document and one terminal ([`StoreOptions`]);
//! the rows [`prepare_stream`](ImageStore::prepare_stream) makes are for
//! one layout, placements being known by their first line.
//!
//! # The pager
//!
//! The pager has redraw rules of its own (plan §7), so it does not use
//! [`prepare_stream`](ImageStore::prepare_stream): it sizes figures through
//! the store like stream mode does, then renders one rendition at a time
//! on its worker thread with [`make`], from the image's [`Source`]. Each
//! [`Make`] is one rendition: block cells, a kitty upload (for
//! placeholders or classic placements, under a fresh id), or the iTerm2 or
//! sixel bytes of the cell rows of the box that are on screen. Those come
//! from the same decisions as stream mode (original files when they can be
//! sent as they are, the same scaling and fallbacks); a partly visible
//! image is a slice of whole cell rows (sixel slices end on whole bands).

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::io::Read as _;
use std::num::NonZeroU32;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use super::raster::{self, Raster};
use super::{Passthrough, Rgba, b64, iterm, kitty, size};
use crate::ir::{Block, Document, ImageId, ImageRef, Length};
use crate::layout::{ImageSizer, Layout, Placement};
use crate::options::ImageOptions;
use crate::render::osc8;
use crate::render::{ImageRows, RowContent};
use crate::style::Rgb;
use crate::term::{BlockGlyphSet, Caps, ColorDepth, Graphics};
use crate::theme::{Theme, Variant};

/// Largest local image file read.
pub const MAX_FILE_BYTES: u64 = 64 << 20;

/// Largest remote image fetched (`curl --max-filesize`).
pub const MAX_REMOTE_BYTES: usize = 20 << 20;

/// `curl --max-time`: the longest a remote fetch may take, in seconds.
pub const REMOTE_TIMEOUT_SECS: u64 = 5;

/// A kitty payload may be the original PNG file up to this size (and at
/// most twice the pixels its box shows); anything else is downscaled and
/// re-encoded.
const KITTY_ORIGINAL_MAX_BYTES: usize = 1 << 20;

/// Most worker threads used for fetching and decoding.
const MAX_THREADS: usize = 8;

/// Decoded pixels held at once while images are rendered (RGBA bytes):
/// large photos are decoded on fewer threads, so a gallery of them cannot
/// take gigabytes.
const DECODE_BUDGET_BYTES: u64 = 256 << 20;

/// Threads to decode images on when the largest has `pixels` pixels: as
/// many decoded images as fit in [`DECODE_BUDGET_BYTES`], from 1 to
/// [`MAX_THREADS`].
fn decode_threads(pixels: u64) -> usize {
    let per_image = pixels.saturating_mul(4).max(1);
    usize::try_from(DECODE_BUDGET_BYTES / per_image)
        .unwrap_or(MAX_THREADS)
        .clamp(1, MAX_THREADS)
}

/// How the images are shown: the terminal's side of the decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreOptions {
    /// The graphics path ([`Caps::graphics`]).
    pub graphics: Graphics,
    /// Glyphs for block images ([`Caps::block_glyphs`]).
    pub glyphs: BlockGlyphSet,
    /// Colours of block images ([`Caps::color`]).
    pub depth: ColorDepth,
    /// The terminal's background, if known: transparent pixels are blended
    /// onto it in block images.
    pub background: Option<Rgb>,
    /// An opaque page colour for protocols without transparency (sixel):
    /// the terminal's background, else the theme's.
    pub page: Rgb,
    /// Cell size in pixels, if known ([`size::DEFAULT_CELL_PX`] otherwise).
    pub cell_px: Option<(u16, u16)>,
    /// How kitty commands reach the terminal (wrapped inside tmux).
    pub passthrough: Passthrough,
    /// Images with more pixels are not decoded (`images.max_pixels`).
    pub max_pixels: u64,
    /// Fetch `http(s)` images (`images.remote`).
    pub remote: bool,
    /// The theme's variant: which `<picture>` source to show.
    pub variant: Variant,
    /// Block-glyph images may be drawn. `false` when East Asian Ambiguous
    /// characters are wide (`render.ambiguous_width = 2`): the terminal
    /// draws block glyphs two columns wide, so such rows would overflow
    /// (see [`crate::term::caps::refuse_wide_blocks`]).
    pub blocks: bool,
}

impl StoreOptions {
    /// The options for a terminal, the `[images]` settings and a theme.
    pub fn new(caps: &Caps, images: &ImageOptions, theme: &Theme) -> StoreOptions {
        StoreOptions {
            graphics: caps.graphics,
            glyphs: caps.block_glyphs,
            depth: caps.color,
            background: caps.background,
            page: caps.background.unwrap_or(theme.base),
            cell_px: caps.cell_px,
            passthrough: if caps.in_tmux {
                Passthrough::Tmux
            } else {
                Passthrough::Direct
            },
            max_pixels: images.max_pixels,
            remote: images.remote,
            variant: theme.variant,
            blocks: true,
        }
    }

    /// The cell size to compute pixel boxes with.
    fn cell(&self) -> (u16, u16) {
        match self.cell_px {
            Some((w, h)) if w > 0 && h > 0 => (w, h),
            _ => size::DEFAULT_CELL_PX,
        }
    }
}

/// An image read into memory.
#[derive(Clone, Debug)]
struct Entry {
    /// The file.
    bytes: Vec<u8>,
    /// Pixel size from the header.
    size: (u32, u32),
    /// HTML `width` / `height` attributes.
    hints: (Option<Length>, Option<Length>),
    /// Where it came from, for messages.
    name: String,
}

/// Which rendition: an image at a size in cells.
type Key = (ImageId, u16, u16);

/// An image rendered for one cell box.
#[derive(Clone, Debug)]
enum Rendition {
    /// Block-glyph cells.
    Blocks(Raster),
    /// kitty Unicode placeholders: the text of each row, and the upload
    /// until the first placement sends it.
    Placeholders {
        upload: Option<Vec<u8>>,
        rows: Vec<Vec<u8>>,
    },
    /// A kitty image for classic placements: the upload until the first
    /// placement sends it, and how many placements were made (each needs
    /// its own placement id, or placing it again would move the first).
    KittyClassic {
        id: kitty::ImageId,
        upload: Option<Vec<u8>>,
        placed: u32,
    },
    /// Bytes that draw the image at the cursor (iTerm2, sixel).
    Pixels(Vec<u8>),
}

/// What the rows of one placement show.
#[derive(Clone, Debug)]
enum Rows {
    /// The cells of the blocks rendition `Key`.
    Blocks(Key),
    /// Row 0 (with the upload when this placement sends it) and the other
    /// rows of the placeholder rendition `Key`.
    Placeholders { key: Key, first: Vec<u8> },
    /// Row 0 draws the image over the reserved box; the others only move
    /// the cursor over it (`rest`).
    Overlay { first: Vec<u8>, rest: Vec<u8> },
}

/// An image file read for a figure, shared with the thread that renders it
/// (the pager's worker): see [`ImageStore::source`] and [`make`].
#[derive(Clone, Debug)]
pub struct Source(Arc<Entry>);

impl Source {
    /// The pixel size from the file's header.
    pub fn size(&self) -> (u32, u32) {
        self.0.size
    }

    /// Where the image came from, for messages.
    pub fn name(&self) -> &str {
        &self.0.name
    }

    /// Decode the image, refusing more than `max_pixels` pixels; `None`
    /// when it cannot be decoded (or emde was built without image support).
    pub fn decode(&self, max_pixels: u64) -> Option<Rgba> {
        codec::decode(&self.0.bytes, max_pixels).ok()
    }
}

/// The images of one document; see the module docs.
#[derive(Debug)]
pub struct ImageStore {
    opts: StoreOptions,
    /// Indexed by [`ImageId`]; `None` for images that are not figures or
    /// could not be read.
    entries: Vec<Option<Arc<Entry>>>,
    renditions: HashMap<Key, Rendition>,
    /// The rows of each placement of the last prepared layout, by its first
    /// line.
    rows: HashMap<u32, Rows>,
    /// Why images are not shown, for `-v`.
    problems: Vec<String>,
    /// Identifies the document the store was loaded for ([`ImageStore::belongs_to`]).
    fingerprint: u64,
}

impl ImageStore {
    /// Read the images of `figures` (the figures of `doc`).
    pub fn load(doc: &Document, figures: &[ImageId], opts: StoreOptions) -> ImageStore {
        let mut store = ImageStore {
            fingerprint: fingerprint(doc),
            opts,
            entries: vec![None; doc.images.len()],
            renditions: HashMap::new(),
            rows: HashMap::new(),
            problems: Vec::new(),
        };
        if store.opts.graphics == Graphics::None {
            return store;
        }
        let jobs: Vec<(ImageId, &ImageRef)> = figures
            .iter()
            .filter_map(|&id| Some((id, doc.image(id)?)))
            .collect();
        let base = doc.base_dir.as_deref();
        let loaded = crate::parallel::map(&jobs, MAX_THREADS, |&(_, img)| {
            read_image(img, base, &store.opts)
        });
        for ((id, _), result) in jobs.iter().zip(loaded) {
            match result.unwrap_or_else(|| Err("could not be read".into())) {
                Ok(entry) => {
                    if let Some(slot) = store.entries.get_mut(id.index()) {
                        *slot = Some(Arc::new(entry));
                    }
                }
                Err(problem) => store.problems.push(problem),
            }
        }
        store
    }

    /// Read the images of every figure of `doc` ([`figure_images`]).
    pub fn load_figures(doc: &Document, opts: StoreOptions) -> ImageStore {
        ImageStore::load(doc, &figure_images(doc), opts)
    }

    /// Why images are not shown (unreadable, not an image, too large, …),
    /// one message per image.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    /// How the store shows images.
    pub fn options(&self) -> &StoreOptions {
        &self.opts
    }

    /// The image of figure image `id`, to render with [`make`] (on any
    /// thread); `None` when it was not read.
    pub fn source(&self, id: ImageId) -> Option<Source> {
        self.entries
            .get(id.index())?
            .as_ref()
            .map(|e| Source(Arc::clone(e)))
    }

    /// Render every placement of `layout` for stream output (decoding each
    /// image once, in parallel), so the emitter can show them through
    /// [`ImageRows`]: pixel images are drawn over rows the output reserves
    /// as it goes, which only works for text written top to bottom.
    pub fn prepare_stream(&mut self, layout: &Layout) {
        self.rows.clear();
        // The sizes still to render, per image (each image is decoded once).
        let mut missing: Vec<(ImageId, Vec<(u16, u16)>)> = Vec::new();
        let mut slot: HashMap<ImageId, usize> = HashMap::new();
        for p in &layout.images {
            let size = (p.cols, p.rows);
            if self.renditions.contains_key(&(p.image, p.cols, p.rows)) {
                continue;
            }
            let i = *slot.entry(p.image).or_insert_with(|| {
                missing.push((p.image, Vec::new()));
                missing.len() - 1
            });
            if let Some((_, sizes)) = missing.get_mut(i)
                && !sizes.contains(&size)
            {
                sizes.push(size);
            }
        }
        let opts = &self.opts;
        let entries = &self.entries;
        let entry = |id: ImageId| entries.get(id.index()).and_then(Option::as_deref);
        let largest = missing
            .iter()
            .filter_map(|&(id, _)| entry(id))
            .map(|e| u64::from(e.size.0) * u64::from(e.size.1))
            .max()
            .unwrap_or(0);
        let threads = decode_threads(largest);
        let rendered = crate::parallel::map(&missing, threads, |(id, sizes)| {
            Some(render_sizes(*id, entry(*id)?, sizes, opts))
        });
        for (rendered, (id, _)) in rendered.into_iter().zip(&missing) {
            let Some(Some(Rendered {
                renditions,
                problem,
            })) = rendered
            else {
                continue;
            };
            if let Some(problem) = problem {
                let name = self.entry_name(*id);
                self.problems.push(format!("{name}: {problem}"));
            }
            self.renditions.extend(renditions);
        }
        for p in &layout.images {
            if let Some(rows) = self.placement_rows(p, layout.indent) {
                self.rows.insert(p.line, rows);
            }
        }
    }

    fn entry_name(&self, id: ImageId) -> String {
        self.entries
            .get(id.index())
            .and_then(Option::as_ref)
            .map_or_else(|| "image".to_owned(), |e| e.name.clone())
    }

    /// What the rows of `p` show; `indent` is the layout's indent.
    fn placement_rows(&mut self, p: &Placement, indent: u16) -> Option<Rows> {
        let key = (p.image, p.cols, p.rows);
        Some(match self.renditions.get_mut(&key)? {
            Rendition::Blocks(_) => Rows::Blocks(key),
            Rendition::Placeholders { upload, rows } => {
                let mut first = upload.take().unwrap_or_default();
                first.extend_from_slice(rows.first()?);
                Rows::Placeholders { key, first }
            }
            Rendition::KittyClassic { id, upload, placed } => {
                *placed = placed.saturating_add(1);
                let mut draw = upload.take().unwrap_or_default();
                let placement = NonZeroU32::new(*placed)?;
                let passthrough = self.opts.passthrough;
                draw.extend(kitty::place(
                    *id,
                    placement,
                    p.cols,
                    p.rows,
                    None,
                    passthrough,
                ));
                overlay_rows(&draw, p, indent)
            }
            Rendition::Pixels(draw) => overlay_rows(draw, p, indent),
        })
    }
}

/// The rows of a pixel image drawn over the box of `p` (`indent` is the
/// layout's indent, so the column is absolute).
fn overlay_rows(draw: &[u8], p: &Placement, indent: u16) -> Rows {
    let column = indent.saturating_add(p.col);
    Rows::Overlay {
        first: over_reserved_rows(draw, p.rows, column, p.cols),
        rest: cursor_forward(p.cols),
    }
}

impl ImageStore {
    /// Whether `doc` is (a reload of) the document this store was loaded
    /// for: same base directory and the same image sources, so every
    /// [`ImageId`] refers to the same file.
    pub fn belongs_to(&self, doc: &Document) -> bool {
        self.fingerprint == fingerprint(doc)
    }
}

/// FNV-1a over the document's base directory and image sources.
fn fingerprint(doc: &Document) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        // Separator, so ("ab", "c") and ("a", "bc") differ.
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    if let Some(dir) = &doc.base_dir {
        eat(dir.as_os_str().as_encoded_bytes());
    }
    for image in &doc.images {
        eat(image.src.as_bytes());
    }
    h
}

impl ImageSizer for ImageStore {
    fn cells(&self, image: ImageId, max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
        if self.opts.graphics == Graphics::None {
            return None;
        }
        let entry = self.entries.get(image.index())?.as_ref()?;
        let px = display_px(entry.size, entry.hints, self.opts.cell(), max_cols);
        let (cols, rows) = size::figure_cells(px, self.opts.cell_px, max_cols, max_rows);
        (cols > 0 && rows > 0).then_some((cols, rows))
    }
}

impl ImageRows for ImageStore {
    fn row(&self, placement: &Placement, row: u16) -> Option<RowContent<'_>> {
        if row >= placement.rows {
            return None;
        }
        match self.rows.get(&placement.line)? {
            Rows::Blocks(key) => match self.renditions.get(key)? {
                Rendition::Blocks(raster) => raster.row(row).map(RowContent::Cells),
                _ => None,
            },
            Rows::Placeholders { key, first } => {
                if row == 0 {
                    return Some(RowContent::Bytes(first));
                }
                match self.renditions.get(key)? {
                    Rendition::Placeholders { rows, .. } => {
                        rows.get(usize::from(row)).map(|r| RowContent::Bytes(r))
                    }
                    _ => None,
                }
            }
            Rows::Overlay { first, rest } => {
                let bytes = if row == 0 { first } else { rest };
                Some(RowContent::Bytes(bytes))
            }
        }
    }
}

/// Bytes that draw an image over the `rows × cols` box whose first row is
/// the current line, from `column` (0-based).
///
/// `rows` newlines first make sure the box *and the row below it* are on
/// screen (scrolling as needed): after an image some terminals leave the
/// cursor on its last row, others on the row below it, and for a box that
/// ended on the screen's last row the latter would scroll the screen under
/// the saved cursor and misplace everything after it. The cursor then goes
/// back up to the box and is saved around the image (`ESC 7`, `ESC 8`), so
/// wherever the protocol leaves it, it ends at the box's right edge, as
/// text of `cols` cells would.
fn over_reserved_rows(draw: &[u8], rows: u16, column: u16, cols: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(draw.len() + usize::from(rows) + 32);
    out.resize(usize::from(rows), b'\n');
    if rows > 0 {
        out.extend_from_slice(format!("\x1b[{rows}A").as_bytes());
    }
    out.push(b'\r');
    out.extend(cursor_forward(column));
    out.extend_from_slice(b"\x1b7");
    out.extend_from_slice(draw);
    out.extend_from_slice(b"\x1b8");
    out.extend(cursor_forward(cols));
    out
}

/// `CSI n C`: the cursor `n` columns to the right, writing nothing (empty
/// for 0, which `CSI 0 C` would read as 1).
fn cursor_forward(n: u16) -> Vec<u8> {
    if n == 0 {
        Vec::new()
    } else {
        format!("\x1b[{n}C").into_bytes()
    }
}

// --- Sizes -------------------------------------------------------------------

/// The pixel size a figure is shown at: its own size, unless HTML
/// `width`/`height` attributes say otherwise (a percentage width is a share
/// of `max_cols` columns of `cell` pixels). A single attribute keeps the
/// aspect ratio.
fn display_px(
    natural: (u32, u32),
    (width, height): (Option<Length>, Option<Length>),
    cell: (u16, u16),
    max_cols: u16,
) -> (u32, u32) {
    let (w, h) = natural;
    let avail = u32::from(max_cols).saturating_mul(u32::from(cell.0));
    let want_w = match width {
        Some(Length::Px(px)) if px > 0 => Some(px),
        Some(Length::Percent(p)) if p > 0 => {
            let px = u64::from(avail) * u64::from(p.min(100)) / 100;
            Some(u32::try_from(px).unwrap_or(u32::MAX).max(1))
        }
        _ => None,
    };
    let want_h = match height {
        Some(Length::Px(px)) if px > 0 => Some(px),
        _ => None,
    };
    let scaled = |v: u32, num: u32, den: u32| {
        let px = u64::from(v) * u64::from(num) / u64::from(den.max(1));
        u32::try_from(px).unwrap_or(u32::MAX).max(1)
    };
    match (want_w, want_h) {
        (Some(ww), Some(hh)) => (ww, hh),
        (Some(ww), None) => (ww, scaled(h, ww, w)),
        (None, Some(hh)) => (scaled(w, hh, h), hh),
        (None, None) => (w, h),
    }
}

/// The pixel box of `cols × rows` cells.
fn box_px(cols: u16, rows: u16, cell: (u16, u16)) -> (u32, u32) {
    (
        u32::from(cols) * u32::from(cell.0),
        u32::from(rows) * u32::from(cell.1),
    )
}

/// The largest size with the aspect ratio of `size` that fits `max`,
/// scaling up as well as down (each side at least 1).
fn fit_box(size: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (w, h) = (f64::from(size.0.max(1)), f64::from(size.1.max(1)));
    let scale = (f64::from(max.0) / w).min(f64::from(max.1) / h);
    let side = |v: f64, cap: u32| ((v * scale).round() as u32).clamp(1, cap.max(1));
    (side(w, max.0), side(h, max.1))
}

// --- Reading -----------------------------------------------------------------

/// Where an image's bytes are.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Location {
    /// A local file.
    File(PathBuf),
    /// The contents of a `data:` URI.
    Data(Vec<u8>),
    /// An `http(s)` URL, safe to pass to `curl`.
    Remote(String),
}

/// The source a figure shows: the `<picture>` `<source>` for the theme's
/// variant, else the image's own `src`.
fn chosen_src(img: &ImageRef, variant: Variant) -> &str {
    let wanted = match variant {
        Variant::Dark => "prefers-color-scheme:dark",
        Variant::Light => "prefers-color-scheme:light",
    };
    let matches = |media: &str| {
        let media: String = media
            .chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect();
        media.contains(wanted)
    };
    let picked = img
        .sources
        .iter()
        .find(|s| s.media.as_deref().is_some_and(matches))
        .map(|s| s.first_url())
        .filter(|u| !u.is_empty());
    match picked {
        Some(url) => url,
        None if !img.src.trim().is_empty() => img.src.trim(),
        None => img.sources.first().map_or("", |s| s.first_url()),
    }
}

/// Where `src` points, relative to `base` (the document's directory).
fn locate(src: &str, base: Option<&Path>, remote: bool) -> Result<Location, String> {
    let src = src.trim();
    if src.is_empty() {
        return Err("no image source".into());
    }
    let scheme = src
        .split_once(':')
        .map(|(s, _)| s)
        .filter(|s| {
            s.len() > 1
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        })
        .map(str::to_ascii_lowercase);
    match scheme.as_deref() {
        Some("data") => data_uri(src).map(Location::Data),
        Some("http" | "https") => remote_url(src, remote),
        Some("file") => file_uri(src).map(Location::File),
        Some(other) => Err(format!("`{other}:` images are not supported")),
        None if src.starts_with("//") => remote_url(&format!("https:{src}"), remote),
        None => Ok(Location::File(local_path(src, base))),
    }
}

/// The local path of a `file:` URI (`file:///p`, `file://localhost/p`,
/// `file:/p`), percent-escapes decoded and any query or fragment dropped.
/// Another host, or a path that is not absolute, names no file here.
fn file_uri(uri: &str) -> Result<PathBuf, String> {
    let rest = uri.get(5..).unwrap_or("");
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let path = match rest.strip_prefix("//") {
        Some(authority) => {
            let (host, path) = authority.split_at(authority.find('/').unwrap_or(authority.len()));
            if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
                return Err(format!("`file:` URI of another host (`{host}`)"));
            }
            path
        }
        None => rest,
    };
    if !path.starts_with('/') {
        return Err("a `file:` URI needs an absolute path".into());
    }
    Ok(PathBuf::from(percent_decode(path)))
}

/// An `http(s)` image, if remote images are on and the URL is safe to pass
/// on (no control characters, at most 2 KiB, non-ASCII percent-encoded).
fn remote_url(url: &str, remote: bool) -> Result<Location, String> {
    if !remote {
        return Err("remote images are off (`--remote-images` or `images.remote = true`)".into());
    }
    osc8::encode_url(url)
        .map(Location::Remote)
        .ok_or_else(|| "the URL cannot be fetched safely".into())
}

/// A local image path: the query and fragment dropped, percent-escapes
/// decoded when the file as written does not exist, relative paths taken
/// from `base`. An absolute path that does not exist is also tried below
/// the enclosing git repository, as GitHub resolves it.
fn local_path(src: &str, base: Option<&Path>) -> PathBuf {
    let plain = src.split(['?', '#']).next().unwrap_or(src);
    let resolve = |p: &str| match base {
        Some(dir) if Path::new(p).is_relative() => dir.join(p),
        _ => PathBuf::from(p),
    };
    let as_written = resolve(plain);
    if as_written.is_file() {
        return as_written;
    }
    let decoded = percent_decode(plain);
    let candidate = resolve(&decoded);
    if candidate.is_file() {
        return candidate;
    }
    if let Some(rel) = decoded.strip_prefix('/')
        && let Some(root) = base.and_then(repository_root)
    {
        let in_repo = root.join(rel);
        if in_repo.is_file() {
            return in_repo;
        }
    }
    as_written
}

/// The nearest directory at or above `dir` that holds `.git`.
fn repository_root(dir: &Path) -> Option<PathBuf> {
    let dir = fs::canonicalize(dir).ok()?;
    dir.ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Decode `%XX` escapes (invalid ones are kept as written).
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let hex = |j: usize| bytes.get(j).and_then(|&c| (c as char).to_digit(16));
        match (b, hex(i + 1), hex(i + 2)) {
            (b'%', Some(hi), Some(lo)) => {
                out.push(u8::try_from(hi * 16 + lo).unwrap_or(b'?'));
                i += 3;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The bytes of a `data:` URI (`data:[type][;base64],payload`).
fn data_uri(uri: &str) -> Result<Vec<u8>, String> {
    let rest = uri.get(5..).unwrap_or("");
    let (meta, payload) = rest
        .split_once(',')
        .ok_or_else(|| "malformed data: URI".to_owned())?;
    let bytes = if meta.to_ascii_lowercase().ends_with(";base64") {
        b64::decode(payload.as_bytes()).ok_or_else(|| "invalid base64 in data: URI".to_owned())?
    } else {
        percent_decode(payload).into_bytes()
    };
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("data: URI too large".into());
    }
    Ok(bytes)
}

/// Read a figure's image and its header size.
fn read_image(img: &ImageRef, base: Option<&Path>, opts: &StoreOptions) -> Result<Entry, String> {
    let src = chosen_src(img, opts.variant);
    let name = short_name(src);
    let bytes = locate(src, base, opts.remote)
        .and_then(|location| match location {
            Location::File(path) => read_file(&path),
            Location::Data(bytes) => Ok(bytes),
            Location::Remote(url) => fetch_remote(&url),
        })
        .map_err(|e| format!("{name}: {e}"))?;
    let size =
        codec::dimensions(&bytes).ok_or_else(|| format!("{name}: {}", unknown_format(&bytes)))?;
    if u64::from(size.0) * u64::from(size.1) > opts.max_pixels {
        return Err(format!(
            "{name}: {}×{} pixels is more than images.max_pixels ({})",
            size.0, size.1, opts.max_pixels
        ));
    }
    Ok(Entry {
        bytes,
        size,
        hints: (img.width, img.height),
        name,
    })
}

/// A source as messages show it (data URIs abbreviated).
fn short_name(src: &str) -> String {
    let src = src.trim();
    if src.len() > 5
        && src
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case("data:"))
    {
        let kind = src
            .get(5..)
            .and_then(|r| r.split([';', ',']).next())
            .unwrap_or("");
        return format!("data:{kind}");
    }
    let mut name: String = src.chars().take(120).collect();
    if name.len() < src.len() {
        name.push('…');
    }
    name
}

/// Why bytes are not an image emde can show.
fn unknown_format(bytes: &[u8]) -> &'static str {
    let head = bytes.get(..bytes.len().min(512)).unwrap_or_default();
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    if text.contains("<svg") {
        "SVG images are not supported (they are shown as their alt text)"
    } else if cfg!(feature = "images") {
        "not a PNG, JPEG, GIF or WebP image"
    } else {
        "emde was built without image support"
    }
}

/// Read a regular file of at most [`MAX_FILE_BYTES`].
fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("larger than {} MiB", MAX_FILE_BYTES >> 20));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    fs::File::open(path)
        .and_then(|f| f.take(MAX_FILE_BYTES).read_to_end(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// The `curl` command line for a remote image: no configuration file, only
/// `http(s)` (also after redirects), at most [`REMOTE_TIMEOUT_SECS`] and
/// [`MAX_REMOTE_BYTES`], and the URL after `--`, so it is never an option.
fn curl_command(url: &str) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-q",
        "--silent",
        "--fail",
        "--location",
        "--max-redirs",
        "5",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "--max-time",
    ])
    .arg(REMOTE_TIMEOUT_SECS.to_string())
    .args(["--max-filesize", "20M", "--output", "-", "--", url]);
    cmd
}

/// Fetch a remote image with `curl` (run directly, never through a shell).
fn fetch_remote(url: &str) -> Result<Vec<u8>, String> {
    let limit = Duration::from_secs(REMOTE_TIMEOUT_SECS + 1);
    crate::term::process::output_with_deadline(curl_command(url), limit, MAX_REMOTE_BYTES)
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| "could not be fetched with curl".into())
}

// --- Rendering ---------------------------------------------------------------

/// The renditions of one image, and why some could not be made.
struct Rendered {
    renditions: Vec<(Key, Rendition)>,
    problem: Option<String>,
}

/// Render `entry` at each size in `sizes`, decoding it at most once.
fn render_sizes(id: ImageId, entry: &Entry, sizes: &[(u16, u16)], opts: &StoreOptions) -> Rendered {
    let decoded: OnceLock<Result<Rgba, String>> = OnceLock::new();
    let pixels = || {
        decoded
            .get_or_init(|| codec::decode(&entry.bytes, opts.max_pixels))
            .as_ref()
            .ok()
    };
    let renditions = sizes
        .iter()
        .filter_map(|&(cols, rows)| {
            let r = render(entry, &pixels, cols, rows, opts)?;
            Some(((id, cols, rows), r))
        })
        .collect();
    let problem = decoded.get().and_then(|r| r.as_ref().err()).cloned();
    Rendered {
        renditions,
        problem,
    }
}

/// One rendition for the graphics path, falling back to blocks when a
/// pixel protocol cannot take the image.
fn render<'p>(
    entry: &Entry,
    pixels: &dyn Fn() -> Option<&'p Rgba>,
    cols: u16,
    rows: u16,
    opts: &StoreOptions,
) -> Option<Rendition> {
    let pixel = match opts.graphics {
        Graphics::None => return None,
        Graphics::Blocks => None,
        Graphics::KittyPlaceholders => {
            placeholders(entry, pixels, cols, rows, opts).map(|p| Rendition::Placeholders {
                upload: Some(p.upload),
                rows: p.rows,
            })
        }
        Graphics::KittyClassic => {
            kitty_upload(entry, pixels, cols, rows, opts).map(|k| Rendition::KittyClassic {
                id: k.id,
                upload: Some(k.upload),
                placed: 0,
            })
        }
        Graphics::Iterm => iterm_image(entry, pixels, cols, rows, opts).map(Rendition::Pixels),
        Graphics::Sixel => sixel_image(pixels, cols, rows, 0..rows, opts).map(Rendition::Pixels),
    };
    pixel.or_else(|| blocks(pixels()?, cols, rows, opts).map(Rendition::Blocks))
}

/// Block glyphs (only with colours to draw them in).
fn blocks(img: &Rgba, cols: u16, rows: u16, opts: &StoreOptions) -> Option<Raster> {
    (opts.depth >= ColorDepth::Ansi16)
        .then(|| raster::rasterize(img, cols, rows, opts.glyphs, opts.background, opts.depth))
}

/// A PNG for kitty and its size in pixels: the original file when it is a
/// PNG that is not much larger than its box, else the image downscaled to
/// (twice) the box.
fn kitty_png<'p>(
    entry: &Entry,
    pixels: &dyn Fn() -> Option<&'p Rgba>,
    cols: u16,
    rows: u16,
    opts: &StoreOptions,
) -> Option<(Vec<u8>, (u32, u32))> {
    let (bw, bh) = box_px(cols, rows, opts.cell());
    let (max_w, max_h) = (bw.saturating_mul(2), bh.saturating_mul(2));
    let (w, h) = entry.size;
    let small = w <= max_w && h <= max_h;
    if codec::is_png(&entry.bytes) && entry.bytes.len() <= KITTY_ORIGINAL_MAX_BYTES && small {
        return Some((entry.bytes.clone(), entry.size));
    }
    let img = pixels()?;
    let png = codec::png(img, max_w, max_h)?;
    Some((
        png,
        size::fit_within((img.width, img.height), (max_w, max_h)),
    ))
}

/// An image uploaded for kitty Unicode placeholders ([`Made::Placeholders`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placeholders {
    /// The image id: fresh for every upload, never reused.
    pub id: kitty::ImageId,
    /// The upload (`a=T,U=1`, wrapped for tmux passthrough when needed),
    /// written before the first row is shown.
    pub upload: Vec<u8>,
    /// The text of each row (placeholder cells with all three diacritics).
    pub rows: Vec<Vec<u8>>,
}

fn placeholders<'p>(
    entry: &Entry,
    pixels: &dyn Fn() -> Option<&'p Rgba>,
    cols: u16,
    rows: u16,
    opts: &StoreOptions,
) -> Option<Placeholders> {
    if cols > kitty::MAX_CELLS || rows > kitty::MAX_CELLS {
        return None;
    }
    let (png, _) = kitty_png(entry, pixels, cols, rows, opts)?;
    let id = kitty::next_image_id();
    let text: Option<Vec<Vec<u8>>> = (0..rows)
        .map(|row| kitty::placeholder_row(id, row, 0..cols).map(String::into_bytes))
        .collect();
    Some(Placeholders {
        id,
        upload: kitty::transmit_placeholder(id, &png, cols, rows, opts.passthrough),
        rows: text?,
    })
}

/// An image uploaded for kitty classic placements ([`Made::Kitty`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KittyImage {
    /// The image id: fresh for every upload, never reused.
    pub id: kitty::ImageId,
    /// The upload (`a=t`), written before the first placement.
    pub upload: Vec<u8>,
    /// The uploaded PNG's height in pixels: crops (`y`, `h`) count in it.
    pub height: u32,
}

fn kitty_upload<'p>(
    entry: &Entry,
    pixels: &dyn Fn() -> Option<&'p Rgba>,
    cols: u16,
    rows: u16,
    opts: &StoreOptions,
) -> Option<KittyImage> {
    let (png, (_, height)) = kitty_png(entry, pixels, cols, rows, opts)?;
    let id = kitty::next_image_id();
    Some(KittyImage {
        id,
        upload: kitty::transmit(id, &png, opts.passthrough),
        height,
    })
}

/// iTerm2 inline images: the original PNG, JPEG or GIF when it is small
/// enough, else a PNG downscaled to twice the box, or to the box itself
/// when that is still too large for one sequence.
fn iterm_image<'p>(
    entry: &Entry,
    pixels: &dyn Fn() -> Option<&'p Rgba>,
    cols: u16,
    rows: u16,
    opts: &StoreOptions,
) -> Option<Vec<u8>> {
    let direct = iterm::Options::default();
    if codec::iterm_decodes(&entry.bytes)
        && entry.bytes.len() <= iterm::ORIGINAL_MAX_BYTES
        && let Some(draw) = iterm::inline_image(&entry.bytes, cols, rows, direct)
    {
        return Some(draw);
    }
    iterm_png(pixels()?, cols, rows, opts)
}

/// `img` as an iTerm2 inline image over `cols × rows` cells: a PNG of twice
/// that box, or of the box itself when that is too large for one sequence.
fn iterm_png(img: &Rgba, cols: u16, rows: u16, opts: &StoreOptions) -> Option<Vec<u8>> {
    let (bw, bh) = box_px(cols, rows, opts.cell());
    [2, 1].into_iter().find_map(|scale| {
        let png = codec::png(img, bw.saturating_mul(scale), bh.saturating_mul(scale))?;
        iterm::inline_image(&png, cols, rows, iterm::Options::default())
    })
}

/// iTerm2 bytes for cell rows `shown` of the `cols × rows` box: the whole
/// image as [`iterm_image`] sends it, or the pixel rows those cell rows
/// show, as a PNG slice drawn over them.
fn iterm_rows<'p>(
    entry: &Entry,
    pixels: &dyn Fn() -> Option<&'p Rgba>,
    cols: u16,
    rows: u16,
    shown: Range<u16>,
    opts: &StoreOptions,
) -> Option<Vec<u8>> {
    let shown = within(shown, rows)?;
    if shown == (0..rows) {
        return iterm_image(entry, pixels, cols, rows, opts);
    }
    let img = pixels()?;
    let slice = img.crop_rows(size::visible_pixel_rows(img.height, rows, shown.clone())?);
    iterm_png(&slice, cols, shown.end - shown.start, opts)
}

/// `shown` cut to a box of `rows` rows; `None` when nothing is left.
fn within(shown: Range<u16>, rows: u16) -> Option<Range<u16>> {
    let end = shown.end.min(rows);
    (shown.start < end).then_some(shown.start..end)
}

/// Height of a sixel band in pixels.
const SIXEL_BAND: u32 = 6;

/// Sixel: the image composited onto the page colour and scaled to its box
/// (whole sixel bands, so nothing spills below it), or the slice of it
/// that cell rows `shown` of the box show (ending on a whole band within
/// the last of them).
fn sixel_image<'p>(
    pixels: &dyn Fn() -> Option<&'p Rgba>,
    cols: u16,
    rows: u16,
    shown: Range<u16>,
    opts: &StoreOptions,
) -> Option<Vec<u8>> {
    let shown = within(shown, rows)?;
    let (bw, bh) = box_px(cols, rows, opts.cell());
    let bh = bh / SIXEL_BAND * SIXEL_BAND;
    if bw == 0 || bh == 0 {
        return None;
    }
    let img = pixels()?;
    let (w, h) = fit_box((img.width, img.height), (bw, bh));
    if shown == (0..rows) {
        return codec::sixel(img, w, h, opts.page);
    }
    let cut = codec::sixel_rows(h, opts.cell().1, shown)?;
    // The source rows behind that slice of the scaled image (at least one).
    let source_row = |y: u32| {
        let row = u64::from(y) * u64::from(img.height) / u64::from(h.max(1));
        u32::try_from(row).unwrap_or(img.height)
    };
    let top = source_row(cut.start);
    let bottom = source_row(cut.end).max(top + 1);
    codec::sixel(
        &img.crop_rows(top..bottom),
        w,
        cut.end - cut.start,
        opts.page,
    )
}

/// The images of `doc`'s figures, wherever they are (nested blocks and
/// footnotes too), sorted and without duplicates: what layout sizes.
pub fn figure_images(doc: &Document) -> Vec<ImageId> {
    fn walk(blocks: &[Block], out: &mut Vec<ImageId>) {
        for block in blocks {
            match block {
                Block::Figure(f) => out.push(f.image),
                Block::Quote { body, .. }
                | Block::Details { body, .. }
                | Block::Align { body, .. } => walk(body, out),
                Block::List(list) => {
                    for item in &list.items {
                        walk(&item.body, out);
                    }
                }
                Block::DefList(items) => {
                    for def in items.iter().flat_map(|item| &item.defs) {
                        walk(def, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.blocks, &mut out);
    for note in &doc.footnotes {
        walk(&note.body, &mut out);
    }
    out.sort_unstable();
    out.dedup();
    out
}

// --- The pager -----------------------------------------------------------------

/// What the pager makes of a figure: one rendition, made by [`make`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Make {
    /// Block-glyph cells.
    Blocks,
    /// A kitty upload for Unicode placeholders, and the text of each row.
    Placeholders,
    /// A kitty upload for classic placements.
    Kitty,
    /// iTerm2 bytes that draw these cell rows of the box.
    Iterm(Range<u16>),
    /// Sixel bytes that draw these cell rows of the box.
    Sixel(Range<u16>),
}

/// A rendition made for the pager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Made {
    /// Block-glyph cells.
    Blocks(Raster),
    /// An image for Unicode placeholders.
    Placeholders(Placeholders),
    /// An image for classic placements.
    Kitty(KittyImage),
    /// Bytes that draw the image (or the slice of it) at the cursor, over
    /// the cells of the rows it shows: iTerm2 or sixel.
    Pixels(Vec<u8>),
}

impl Made {
    /// Roughly how many bytes it holds (for cache budgets).
    pub fn bytes(&self) -> usize {
        match self {
            Made::Blocks(r) => {
                usize::from(r.cols) * usize::from(r.rows) * size_of::<raster::RasterCell>()
            }
            Made::Placeholders(p) => p.upload.len() + p.rows.iter().map(Vec::len).sum::<usize>(),
            Made::Kitty(k) => k.upload.len(),
            Made::Pixels(b) => b.len(),
        }
    }
}

/// Render `source` at `cols × rows` cells as `what` says, with the
/// decisions stream mode makes (original files when they can be sent as
/// they are, the same scaling, PNG and sixel limits). `decoded` decodes the
/// image when the rendition needs pixels (it is called at most once; kitty
/// uploads and whole iTerm2 images of small files need none), so a caller
/// can keep decoded images across renditions.
///
/// `None` when the rendition cannot be made: no pixels, no colours for
/// blocks, more cells than placeholders address, or too large for the
/// protocol; the pager falls back to blocks, then to the placeholder box.
pub fn make(
    source: &Source,
    what: &Make,
    cols: u16,
    rows: u16,
    opts: &StoreOptions,
    decoded: &mut dyn FnMut() -> Option<Arc<Rgba>>,
) -> Option<Made> {
    if cols == 0 || rows == 0 {
        return None;
    }
    let decoded = RefCell::new(decoded);
    let image: OnceCell<Option<Arc<Rgba>>> = OnceCell::new();
    let pixels = || {
        image
            .get_or_init(|| {
                let mut decode = decoded.borrow_mut();
                let decode: &mut dyn FnMut() -> Option<Arc<Rgba>> = &mut **decode;
                decode()
            })
            .as_deref()
    };
    let entry = &source.0;
    match what {
        Make::Blocks => blocks(pixels()?, cols, rows, opts).map(Made::Blocks),
        Make::Placeholders => {
            placeholders(entry, &pixels, cols, rows, opts).map(Made::Placeholders)
        }
        Make::Kitty => kitty_upload(entry, &pixels, cols, rows, opts).map(Made::Kitty),
        Make::Iterm(shown) => {
            iterm_rows(entry, &pixels, cols, rows, shown.clone(), opts).map(Made::Pixels)
        }
        Make::Sixel(shown) => {
            sixel_image(&pixels, cols, rows, shown.clone(), opts).map(Made::Pixels)
        }
    }
}

/// The decoders and encoders, which need the `images` (and `sixel`)
/// features; without them nothing decodes and every figure keeps its box.
mod codec {
    use std::ops::Range;

    use super::Rgba;
    use crate::style::Rgb;

    /// Pixel size from the header.
    pub(super) fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
        #[cfg(feature = "images")]
        {
            crate::gfx::decode::dimensions(bytes)
        }
        #[cfg(not(feature = "images"))]
        {
            let _ = bytes;
            None
        }
    }

    /// Decode to RGBA, refusing more than `max_pixels` pixels.
    pub(super) fn decode(bytes: &[u8], max_pixels: u64) -> Result<Rgba, String> {
        #[cfg(feature = "images")]
        {
            crate::gfx::decode::decode(bytes, max_pixels).map_err(|e| e.to_string())
        }
        #[cfg(not(feature = "images"))]
        {
            let _ = (bytes, max_pixels);
            Err("emde was built without image support".into())
        }
    }

    /// Whether the file is a PNG.
    pub(super) fn is_png(bytes: &[u8]) -> bool {
        bytes.starts_with(b"\x89PNG\r\n\x1a\n")
    }

    /// Whether iTerm2 (and the terminals that copy its protocol) can show
    /// the file as it is: PNG, JPEG or GIF.
    pub(super) fn iterm_decodes(bytes: &[u8]) -> bool {
        is_png(bytes) || bytes.starts_with(b"\xff\xd8\xff") || bytes.starts_with(b"GIF8")
    }

    /// `img` downscaled to fit `max_w × max_h` and encoded as PNG.
    pub(super) fn png(img: &Rgba, max_w: u32, max_h: u32) -> Option<Vec<u8>> {
        #[cfg(feature = "images")]
        {
            crate::gfx::png::encode(&crate::gfx::png::fit(img, max_w, max_h))
        }
        #[cfg(not(feature = "images"))]
        {
            let _ = (img, max_w, max_h);
            None
        }
    }

    /// The pixel rows of an image `height` pixels tall (drawn with cells
    /// `cell_height` pixels tall) that cell rows `shown` show, cut to whole
    /// sixel bands ([`crate::gfx::sixel::crop_rows`]).
    pub(super) fn sixel_rows(
        height: u32,
        cell_height: u16,
        shown: Range<u16>,
    ) -> Option<Range<u32>> {
        #[cfg(feature = "sixel")]
        {
            crate::gfx::sixel::crop_rows(height, cell_height, shown)
        }
        #[cfg(not(feature = "sixel"))]
        {
            let _ = (height, cell_height, shown);
            None
        }
    }

    /// `img` composited onto `page`, scaled to `w × h` and encoded as a
    /// sixel sequence of at most 1 MiB.
    pub(super) fn sixel(img: &Rgba, w: u32, h: u32, page: Rgb) -> Option<Vec<u8>> {
        #[cfg(feature = "sixel")]
        {
            let scaled = crate::gfx::resize_onto(img, w, h, page);
            crate::gfx::sixel::encode(&scaled, page, crate::gfx::sixel::TMUX_MAX_BYTES)
        }
        #[cfg(not(feature = "sixel"))]
        {
            let _ = (img, w, h, page);
            None
        }
    }
}

#[cfg(test)]
mod tests;
