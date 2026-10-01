//! Images in the pager (plan §7): figures sized for layout, rendered off
//! the event loop, and drawn the way each graphics protocol needs.
//!
//! # Documents and stores
//!
//! Every document the pager shows gets an [`ImageStore`] ([`Images::sizer`]
//! loads it the first time the document is laid out with images on; the
//! ones the program loaded already come with the session, see
//! [`PagerImages`]). Stores live as long as their document is among the
//! pages the pager keeps (its 8-document history, [`Images::retain`]); a
//! reload keeps the store unless the images changed.
//!
//! # Renditions
//!
//! Layout reserves a box of the final size for every figure (sizes come
//! from file headers), so the first frame never waits: a figure shows its
//! box until its rendition is made on the worker thread ([`worker`]), and
//! then the rows it covers are painted again. What is made is kept in a
//! byte-budgeted cache ([`cache`]); after a new layout (resize, another
//! document) work still queued for the old one is skipped.
//!
//! # Drawing, by protocol
//!
//! * **Blocks**: the rows are block-glyph cells, painted like text.
//! * **kitty placeholders**: the rows are text too (U+10EEEE cells, all
//!   three diacritics). Each (image, columns, rows) is uploaded once under a
//!   fresh id, in the frame that first shows its rows, before them; a new
//!   size (after a resize) is a new upload and the old id is deleted
//!   (`a=d,d=I`), and every id is deleted on exit ([`Images::cleanup`]).
//! * **kitty classic** (never inside tmux): the image is uploaded once and
//!   placed after each frame that moved it (`a=p` with a `y`/`h` crop when
//!   partly visible, `C=1`); a placement that goes off screen is deleted.
//!   The rows under it are blank.
//! * **iTerm2 and sixel**: pixels are drawn over the rows after the text,
//!   but only once the document has stood still for
//!   [`SCROLL_DEBOUNCE`]; until then the rows show the blocks rendition. A
//!   partly visible image is drawn as a slice of whole cell rows. Pixels
//!   never reach the status row, and inside tmux at most
//!   [`TMUX_MAX_SIXELS`] sixel images are on screen at once.
//!
//! No image is placed or drawn on rows an overlay (outline, help, link
//! hints) draws on: it would hide the overlay. While a classic or pixel
//! image is on screen the painter's scroll fast path is off
//! ([`super::Frame::images`]): the terminal would scroll pixels it does not
//! track. Images are drawn (and kitty images placed) again after an
//! overlay closes, after a resize, after ^L ([`Images::reset`], which also
//! uploads kitty images again) and after a suspend.
//!
//! `i` cycles the mode ([`Images::cycle`]): the detected graphics path,
//! blocks, and off (alt text boxes).

mod cache;
mod worker;

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::num::NonZeroU32;
use std::ops::Range;
use std::time::{Duration, Instant};

use crate::gfx::kitty::{self, Deletion};
use crate::gfx::store::{ImageStore, Made, Make, StoreOptions};
use crate::gfx::{Passthrough, SCROLL_DEBOUNCE, size};
use crate::ir::{Document, ImageId};
use crate::layout::{ImageSizer, Layout, NoImages, Placement};
use crate::render::{ImageRows, RowContent};
use crate::term::{ColorDepth, Graphics};

use super::diff::{Diff, Screen};
use super::state::{DocKey, State};
use super::view::Frame;
use cache::{Key, Renditions, Slot, StoreId};
use worker::{Done, Outcome, Worker};

/// How often the event loop polls while the worker has jobs out (so a
/// rendition shows soon after it is made).
pub(crate) const WORK_POLL: Duration = Duration::from_millis(15);

/// The longest the event loop waits for the worker when it waits for it
/// at all ([`PagerImages::wait_when_idle`]).
const SETTLE_LIMIT: Duration = Duration::from_secs(10);

/// The most sixel images on screen inside tmux (plan §7): tmux 3.4 keeps
/// 10 for the whole server and silently drops the oldest. Further images
/// keep their blocks.
pub(crate) const TMUX_MAX_SIXELS: usize = 6;

/// How the pager shows figures: part of [`super::PagerSession`].
#[derive(Debug, Default)]
pub struct PagerImages {
    /// How images are drawn: the graphics path, block glyphs, colours, cell
    /// size, tmux passthrough and limits ([`StoreOptions::new`] from the
    /// terminal's capabilities, `[images]` and the theme). `None`: every
    /// figure is an alt text box, and `i` cannot change that.
    pub options: Option<StoreOptions>,
    /// Stores loaded already, for some of the documents: each serves the
    /// document it [belongs to](ImageStore::belongs_to). The pager loads
    /// the others when it shows their documents.
    pub stores: Vec<ImageStore>,
    /// Wait for the image worker whenever the terminal is idle, rather than
    /// picking results up as they come: every run then paints the same
    /// frames (tests).
    pub wait_when_idle: bool,
}

impl PagerImages {
    /// Figures shown as `options` say.
    pub fn new(options: StoreOptions) -> PagerImages {
        PagerImages {
            options: Some(options),
            ..PagerImages::default()
        }
    }

    /// Every figure an alt text box.
    pub fn off() -> PagerImages {
        PagerImages::default()
    }
}

/// The modes `i` goes through, starting with the detected one: then
/// blocks, then off. Blocks need colours (without them a blocks rendition
/// cannot be made, and the figures would only show their boxes).
fn modes(options: Option<&StoreOptions>) -> Vec<Graphics> {
    let Some(opts) = options else {
        return vec![Graphics::None];
    };
    let blocks = (opts.depth >= ColorDepth::Ansi16).then_some(Graphics::Blocks);
    let mut modes = vec![opts.graphics];
    for g in blocks.into_iter().chain([Graphics::None]) {
        if !modes.contains(&g) {
            modes.push(g);
        }
    }
    modes
}

/// A mode as the `i` message names it.
pub(crate) fn mode_name(mode: Graphics) -> &'static str {
    match mode {
        Graphics::None => "off (alt text)",
        Graphics::Blocks => "blocks",
        Graphics::KittyPlaceholders => "kitty (placeholders)",
        Graphics::KittyClassic => "kitty",
        Graphics::Iterm => "iTerm2",
        Graphics::Sixel => "sixel",
    }
}

/// Whether a mode draws pixels over the text (no scroll fast path).
fn overlays(mode: Graphics) -> bool {
    matches!(
        mode,
        Graphics::KittyClassic | Graphics::Iterm | Graphics::Sixel
    )
}

/// The rendition a figure's rows show in a mode.
fn base(mode: Graphics) -> Option<Make> {
    match mode {
        Graphics::None => None,
        Graphics::Blocks | Graphics::Iterm | Graphics::Sixel => Some(Make::Blocks),
        Graphics::KittyPlaceholders => Some(Make::Placeholders),
        Graphics::KittyClassic => Some(Make::Kitty),
    }
}

/// The rendition `make` of placement `p`'s image in `store`.
fn key(store: StoreId, p: &Placement, make: Make) -> Key {
    Key {
        store,
        image: p.image,
        cols: p.cols,
        rows: p.rows,
        make,
    }
}

/// A placement on some of the lines in question: its index in
/// [`Layout::images`], the placement, and the rows of its box on them.
type OnLines = (usize, Placement, Range<u16>);

/// The placements with rows on `lines`, top to bottom.
fn placements_on(layout: &Layout, lines: Range<usize>) -> Vec<OnLines> {
    let end = |p: &Placement| p.line as usize + usize::from(p.rows);
    let first = layout.images.partition_point(|p| end(p) <= lines.start);
    layout
        .images
        .iter()
        .enumerate()
        .skip(first)
        .take_while(|(_, p)| (p.line as usize) < lines.end)
        .filter_map(|(i, p)| {
            let from = lines.start.max(p.line as usize) - p.line as usize;
            let to = lines.end.min(end(p)) - p.line as usize;
            let rows = u16::try_from(from).ok()?..u16::try_from(to).ok()?;
            (!rows.is_empty()).then_some((i, *p, rows))
        })
        .collect()
}

/// Move the cursor to `row`, `col` (0-based).
fn cursor_to(out: &mut Vec<u8>, row: usize, col: u16) {
    let _ = write!(out, "\x1b[{};{}H", row + 1, u32::from(col) + 1);
}

/// What the rows of a visible figure show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowsShow {
    /// The layout's placeholder box (nothing ready yet, or nothing to
    /// show).
    Box,
    /// Block-glyph cells.
    Cells,
    /// kitty placeholder text for this image.
    Text(kitty::ImageId),
    /// Spaces, under a kitty classic placement.
    Blank,
}

/// An image drawn over the text, or a kitty placement.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Placed {
    /// Screen row of its first visible row.
    row: usize,
    /// The rows of the box it shows.
    shown: Range<u16>,
    /// A kitty classic placement: the image and the placement id.
    kitty: Option<(kitty::ImageId, NonZeroU32)>,
}

impl Placed {
    /// Whether a frame writes over any of its rows.
    fn written_over(&self, diff: &Diff) -> bool {
        let rows = usize::from(self.shown.end.saturating_sub(self.shown.start));
        (self.row..self.row + rows).any(|r| diff.writes(r))
    }
}

/// What the terminal shows of the current layout's figures.
#[derive(Debug, Default)]
struct Shown {
    /// The layout generation, top line and screen size of the last frame.
    generation: u64,
    top: usize,
    size: (u16, u16),
    /// What the rows of each visible figure show, by its first line.
    rows: HashMap<u32, RowsShow>,
    /// Pixel images and kitty placements on screen, by placement index.
    placed: HashMap<usize, Placed>,
}

/// Where the current frame is.
struct At {
    /// The first document line on screen.
    top: usize,
    /// The layout's indent (figure columns are relative to it).
    indent: u16,
    /// The screen rows an overlay (outline, help, link hints) draws on;
    /// empty without one.
    covered: Vec<bool>,
}

impl At {
    /// Where `frame` is.
    fn new(state: &State, frame: &Frame) -> At {
        let covered = if frame.overlay {
            frame.lines.iter().map(|r| !r.overlays.is_empty()).collect()
        } else {
            Vec::new()
        };
        At {
            top: state.top,
            indent: state.layout.indent,
            covered,
        }
    }

    /// Whether an overlay draws on any of `rows` screen rows from `row`:
    /// an image there would hide it (kitty draws placements above the
    /// text, pixel images replace it).
    fn covers(&self, row: usize, rows: usize) -> bool {
        self.covered.iter().skip(row).take(rows).any(|&c| c)
    }
}

/// A document's store.
#[derive(Debug)]
struct Kept {
    key: DocKey,
    id: StoreId,
    store: ImageStore,
}

/// The pager's image layer; see the module docs.
pub(crate) struct Images {
    /// How images are drawn (`None`: never).
    options: Option<StoreOptions>,
    /// The modes `i` cycles through, and the current one.
    modes: Vec<Graphics>,
    mode: usize,
    /// The documents' stores, least recently used first.
    stores: Vec<Kept>,
    /// Stores from the session no document has claimed yet.
    spare: Vec<ImageStore>,
    next_store: u32,
    /// The store of the document the current layout shows.
    current: Option<StoreId>,
    cache: Renditions,
    worker: Worker,
    wait_when_idle: bool,
    shown: Shown,
    /// When the document last moved (a scroll, a new layout).
    moved_at: Option<Instant>,
    /// A pixel image waits for the document to stand still.
    awaiting: bool,
    /// kitty images uploaded to the terminal, by rendition.
    live: HashMap<Key, kitty::ImageId>,
    /// kitty images to delete with the next frame.
    doomed: Vec<kitty::ImageId>,
    /// Bytes for the start of the next frame (placements to delete).
    queued: Vec<u8>,
    /// The kitty images in the terminal changed since [`Images::cleanup`].
    cleanup_changed: bool,
    /// Spaces for the rows under kitty classic placements.
    blank: Vec<u8>,
}

impl Images {
    /// The image layer for a session's images.
    pub(crate) fn new(setup: PagerImages) -> Images {
        Images {
            modes: modes(setup.options.as_ref()),
            options: setup.options,
            mode: 0,
            stores: Vec::new(),
            spare: setup.stores,
            next_store: 1,
            current: None,
            cache: Renditions::new(cache::BUDGET),
            worker: Worker::new(),
            wait_when_idle: setup.wait_when_idle,
            shown: Shown::default(),
            moved_at: None,
            awaiting: false,
            live: HashMap::new(),
            doomed: Vec::new(),
            queued: Vec::new(),
            cleanup_changed: false,
            blank: Vec::new(),
        }
    }

    /// The graphics path figures are drawn with now.
    pub(crate) fn mode(&self) -> Graphics {
        self.modes.get(self.mode).copied().unwrap_or(Graphics::None)
    }

    /// Switch to the next mode (`i`): returns it, and whether figure sizes
    /// change (images on or off: lay the document out again); `None` when
    /// there is nothing to switch to. The caller repaints the screen.
    pub(crate) fn cycle(&mut self) -> Option<(Graphics, bool)> {
        if self.modes.len() < 2 {
            return None;
        }
        let old = self.mode();
        self.mode = (self.mode + 1) % self.modes.len();
        let new = self.mode();
        self.forget_shown();
        Some((new, (old == Graphics::None) != (new == Graphics::None)))
    }

    /// How kitty commands reach the terminal.
    fn passthrough(&self) -> Passthrough {
        self.options
            .as_ref()
            .map_or(Passthrough::Direct, |o| o.passthrough)
    }

    // --- Stores ----------------------------------------------------------------

    /// Figure sizes for a layout of `doc` (whose key is `key`): its store,
    /// loaded the first time (from the files, or a store the session
    /// brought), or none when images are off. The next frames show this
    /// store's images.
    pub(crate) fn sizer(&mut self, key: &DocKey, doc: &Document) -> &dyn ImageSizer {
        self.current = None;
        let Some(opts) = self.options.clone() else {
            return &NoImages;
        };
        if self.mode() == Graphics::None {
            return &NoImages;
        }
        let id = match self.stores.iter().position(|k| k.key == *key) {
            Some(i) => {
                let kept = self.stores.remove(i);
                if kept.store.belongs_to(doc) {
                    let id = kept.id;
                    self.stores.push(kept);
                    id
                } else {
                    // Reloaded with other images.
                    self.purge(kept.id);
                    self.load(key, doc, opts)
                }
            }
            None => self.load(key, doc, opts),
        };
        self.current = Some(id);
        match self.stores.last() {
            Some(kept) => &kept.store,
            None => &NoImages,
        }
    }

    /// Load (or claim) the store of `doc` as the most recently used.
    fn load(&mut self, key: &DocKey, doc: &Document, mut opts: StoreOptions) -> StoreId {
        let store = match self.spare.iter().position(|s| s.belongs_to(doc)) {
            Some(i) => self.spare.swap_remove(i),
            None => {
                // With images off at first, `i` can still turn blocks on.
                if opts.graphics == Graphics::None {
                    opts.graphics = Graphics::Blocks;
                }
                ImageStore::load_figures(doc, opts)
            }
        };
        let id = StoreId(self.next_store);
        self.next_store += 1;
        self.stores.push(Kept {
            key: key.clone(),
            id,
            store,
        });
        id
    }

    /// Keep the stores of the documents `keep` says the pager keeps; the
    /// others go with their renditions, and their kitty images are deleted.
    pub(crate) fn retain(&mut self, keep: impl Fn(&DocKey) -> bool) {
        let mut gone = Vec::new();
        self.stores.retain(|k| {
            let kept = keep(&k.key);
            if !kept {
                gone.push(k.id);
            }
            kept
        });
        for id in gone {
            self.purge(id);
        }
    }

    /// Forget everything made from store `store`.
    fn purge(&mut self, store: StoreId) {
        self.cache.retain(|k| k.store != store);
        let mut dead = Vec::new();
        self.live.retain(|k, id| {
            let keep = k.store != store;
            if !keep {
                dead.push(*id);
            }
            keep
        });
        if !dead.is_empty() {
            self.doomed.extend(dead);
            self.cleanup_changed = true;
        }
        if self.current == Some(store) {
            self.current = None;
        }
    }

    // --- Renditions ------------------------------------------------------------

    /// Ask the worker for `key` (a figure whose file was not read has
    /// nothing to show).
    fn request(&mut self, key: Key) {
        let source = self
            .stores
            .iter()
            .find(|k| k.id == key.store)
            .and_then(|k| Some((k.store.source(key.image)?, k.store.options().clone())));
        match source {
            Some((source, opts)) => {
                let job = self.worker.submit(key.clone(), source, opts);
                self.cache.pending(key, job);
            }
            None => self.cache.fail(key),
        }
    }

    /// Make sure the rendition placement `p` shows in `mode` is made, or
    /// being made: blocks when the mode's own cannot be made.
    fn want(&mut self, store: StoreId, p: &Placement, mode: Graphics) {
        let Some(make) = base(mode) else {
            return;
        };
        let primary = key(store, p, make);
        match self.cache.get(&primary) {
            None => self.request(primary),
            Some(Slot::Failed) if primary.make != Make::Blocks => {
                let blocks = key(store, p, Make::Blocks);
                if self.cache.get(&blocks).is_none() {
                    self.request(blocks);
                }
            }
            Some(_) => self.cache.touch(&primary),
        }
    }

    /// What the rows of placement `p` show in `mode` now.
    fn show(&mut self, store: StoreId, p: &Placement, mode: Graphics) -> RowsShow {
        let Some(make) = base(mode) else {
            return RowsShow::Box;
        };
        let primary = key(store, p, make);
        match self.cache.get(&primary) {
            Some(Slot::Ready(r)) => {
                let show = match &r.made {
                    Made::Blocks(_) => RowsShow::Cells,
                    Made::Placeholders(ph) => RowsShow::Text(ph.id),
                    Made::Kitty(_) => RowsShow::Blank,
                    Made::Pixels(_) => RowsShow::Box,
                };
                self.cache.touch(&primary);
                show
            }
            Some(Slot::Failed) if primary.make != Make::Blocks => {
                let blocks = key(store, p, Make::Blocks);
                if self.cache.made(&blocks).is_some() {
                    self.cache.touch(&blocks);
                    RowsShow::Cells
                } else {
                    RowsShow::Box
                }
            }
            _ => RowsShow::Box,
        }
    }

    /// Take the worker's results; `true` when any came in (a repaint may
    /// show them).
    pub(crate) fn collect(&mut self) -> bool {
        let done = self.worker.take();
        self.absorb(done)
    }

    /// Wait for the worker to finish what it has (at most
    /// [`SETTLE_LIMIT`]) and take the results.
    pub(crate) fn settle(&mut self) -> bool {
        let done = self.worker.wait(SETTLE_LIMIT);
        self.absorb(done)
    }

    fn absorb(&mut self, done: Vec<Done>) -> bool {
        let any = !done.is_empty();
        for d in done {
            match d.outcome {
                Outcome::Skipped => self.cache.skipped(&d.key, d.job),
                Outcome::Made(made) => {
                    // A store that is gone took its renditions along.
                    if self.stores.iter().any(|k| k.id == d.key.store) {
                        self.cache.insert(d.key, made);
                    }
                }
            }
        }
        any
    }

    /// Whether the worker has jobs out.
    pub(crate) fn busy(&self) -> bool {
        self.worker.busy()
    }

    /// Whether results are only taken when the terminal is idle
    /// ([`PagerImages::wait_when_idle`]).
    pub(crate) fn waits_when_idle(&self) -> bool {
        self.wait_when_idle
    }

    /// When the debounce ends for a pixel image waiting for it.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        if self.awaiting {
            self.moved_at.map(|t| t + SCROLL_DEBOUNCE)
        } else {
            None
        }
    }

    // --- Frames ----------------------------------------------------------------

    /// Get a frame ready before the painter compares it with the screen:
    /// ask for the renditions the screen (and a screen above and below)
    /// needs, decide what each visible figure's rows show (rows whose
    /// content changed are written again), and say whether a pixel image is
    /// on screen (no scroll fast path then).
    pub(crate) fn plan(
        &mut self,
        state: &State,
        frame: &mut Frame,
        screen: &mut Screen,
        now: Instant,
    ) {
        self.cache.next_frame();
        let new_layout = state.generation != self.shown.generation;
        if new_layout {
            self.new_layout(state);
        }
        // A resized terminal repaints every row and may have moved what it
        // drew (kitty shifts placements when the screen gets shorter):
        // everything is placed and drawn afresh, also without a new layout.
        let resized = state.size() != self.shown.size;
        if resized && !new_layout {
            self.forget_shown();
        }
        if new_layout || resized || state.top != self.shown.top {
            self.moved_at = Some(now);
        }
        let mode = self.mode();
        let Some(store) = self.current else {
            self.shown.rows.clear();
            return;
        };
        let (top, view) = (state.top, state.view_rows());
        let visible = placements_on(&state.layout, top..top + view);
        let near = placements_on(
            &state.layout,
            top.saturating_sub(view)..top.saturating_add(view.saturating_mul(2)),
        );
        // What the screen needs first, then what is a screen away.
        for (_, p, _) in visible.iter().chain(&near) {
            self.want(store, p, mode);
        }
        let mut rows = HashMap::with_capacity(visible.len());
        for (_, p, _) in &visible {
            let show = self.show(store, p, mode);
            if self.shown.rows.get(&p.line).is_some_and(|old| *old != show) {
                let first = p.line as usize;
                screen.invalidate_lines(first..first + usize::from(p.rows));
            }
            if show == RowsShow::Blank && self.blank.len() < usize::from(p.cols) {
                self.blank.resize(usize::from(p.cols), b' ');
            }
            rows.insert(p.line, show);
        }
        self.shown.rows = rows;
        frame.images = overlays(mode) && !visible.is_empty();
    }

    /// The image bytes of a frame, once the painter knows which rows it
    /// writes: what goes before the rows (kitty uploads and deletions) and
    /// after them (pixel images, kitty placements).
    pub(crate) fn extras(
        &mut self,
        state: &State,
        frame: &Frame,
        diff: &Diff,
        now: Instant,
    ) -> (Vec<u8>, Vec<u8>) {
        let mut before = std::mem::take(&mut self.queued);
        if !self.doomed.is_empty() {
            let pass = self.passthrough();
            before.extend(kitty::delete_all(self.doomed.drain(..), pass));
            self.cleanup_changed = true;
        }
        let mut after = Vec::new();
        self.awaiting = false;
        if let Some(store) = self.current {
            let at = At::new(state, frame);
            let visible = placements_on(&state.layout, at.top..at.top + state.view_rows());
            self.upload(store, &visible, &mut before);
            match self.mode() {
                Graphics::KittyClassic => {
                    self.place(store, &visible, &at, &mut before, &mut after);
                }
                Graphics::Iterm | Graphics::Sixel => {
                    self.draw(store, &visible, &at, diff, now, &mut after);
                }
                Graphics::None | Graphics::Blocks | Graphics::KittyPlaceholders => {}
            }
        }
        self.shown.generation = state.generation;
        self.shown.top = state.top;
        self.shown.size = state.size();
        (before, after)
    }

    /// What the figure rows of the current frame show, for the emitter.
    pub(crate) fn rows(&self) -> FrameRows<'_> {
        FrameRows { images: self }
    }

    /// A new layout (resize, another document, a reload, `w`, `i`): queued
    /// work for the old one is skipped, its placements go, and kitty images
    /// uploaded for figure sizes the new one does not have are deleted (a
    /// new size gets a new upload under a new id).
    fn new_layout(&mut self, state: &State) {
        self.worker.bump();
        self.cache.forget_pending();
        self.forget_shown();
        let Some(store) = self.current else {
            return;
        };
        let sizes: HashSet<(ImageId, u16, u16)> = state
            .layout
            .images
            .iter()
            .map(|p| (p.image, p.cols, p.rows))
            .collect();
        let stale: Vec<Key> = self
            .live
            .keys()
            .filter(|k| k.store == store && !sizes.contains(&(k.image, k.cols, k.rows)))
            .cloned()
            .collect();
        for k in stale {
            if let Some(id) = self.live.remove(&k) {
                self.doomed.push(id);
                self.cleanup_changed = true;
            }
            self.cache.remove(&k);
        }
    }

    /// Forget what the terminal shows of the figures: kitty placements are
    /// deleted with the next frame, pixel images go with the repaint that
    /// follows.
    fn forget_shown(&mut self) {
        let pass = self.passthrough();
        for placed in self.shown.placed.values() {
            if let Some((id, p)) = placed.kitty {
                self.queued
                    .extend(kitty::delete(id, Deletion::Placement(p), pass));
            }
        }
        self.shown.placed.clear();
        self.shown.rows.clear();
    }

    /// Upload the kitty images the frame's rows need (placeholders, and
    /// the images of classic placements) that are not in the terminal yet.
    fn upload(&mut self, store: StoreId, visible: &[OnLines], before: &mut Vec<u8>) {
        for (_, p, _) in visible {
            let make = match self.shown.rows.get(&p.line) {
                Some(RowsShow::Text(_)) => Make::Placeholders,
                Some(RowsShow::Blank) => Make::Kitty,
                _ => continue,
            };
            let key = key(store, p, make);
            if self.live.contains_key(&key) {
                continue;
            }
            let id = match self.cache.made(&key) {
                Some(Made::Placeholders(ph)) => ph.id,
                Some(Made::Kitty(k)) => k.id,
                _ => continue,
            };
            if let Some(upload) = self.cache.pin(&key) {
                before.extend(upload);
                self.live.insert(key, id);
                self.cleanup_changed = true;
            }
        }
    }

    /// kitty classic: place every visible image that moved (cropped to its
    /// visible rows), and delete the placements that left the screen, or
    /// that an overlay draws on now.
    fn place(
        &mut self,
        store: StoreId,
        visible: &[OnLines],
        at: &At,
        before: &mut Vec<u8>,
        after: &mut Vec<u8>,
    ) {
        let pass = self.passthrough();
        let mut placed = HashMap::new();
        for (idx, p, shown) in visible {
            if self.shown.rows.get(&p.line) != Some(&RowsShow::Blank) {
                continue;
            }
            let row = (p.line as usize + usize::from(shown.start)).saturating_sub(at.top);
            let rows = shown.end - shown.start;
            if at.covers(row, usize::from(rows)) {
                continue;
            }
            let Some(Made::Kitty(k)) = self.cache.made(&key(store, p, Make::Kitty)) else {
                continue;
            };
            let Some(pid) = u32::try_from(idx + 1).ok().and_then(NonZeroU32::new) else {
                continue;
            };
            let crop = if *shown == (0..p.rows) {
                None
            } else {
                match size::visible_pixel_rows(k.height, p.rows, shown.clone()) {
                    Some(rows) => Some(rows),
                    None => continue,
                }
            };
            let spot = Placed {
                row,
                shown: shown.clone(),
                kitty: Some((k.id, pid)),
            };
            if self.shown.placed.get(idx) != Some(&spot) {
                cursor_to(after, row, at.indent.saturating_add(p.col));
                after.extend(kitty::place(k.id, pid, p.cols, rows, crop, pass));
            }
            placed.insert(*idx, spot);
        }
        for (idx, old) in &self.shown.placed {
            if !placed.contains_key(idx)
                && let Some((id, p)) = old.kitty
            {
                before.extend(kitty::delete(id, Deletion::Placement(p), pass));
            }
        }
        self.shown.placed = placed;
    }

    /// iTerm2 and sixel: pixels this frame writes over are gone; once the
    /// document stands still, draw every visible image that is not on
    /// screen (as a slice of its visible rows) whose rendition is made (it
    /// is asked for otherwise), unless an overlay draws on its rows.
    fn draw(
        &mut self,
        store: StoreId,
        visible: &[OnLines],
        at: &At,
        diff: &Diff,
        now: Instant,
        after: &mut Vec<u8>,
    ) {
        self.shown
            .placed
            .retain(|_, placed| !placed.written_over(diff));
        let settled = self
            .moved_at
            .is_none_or(|t| now.saturating_duration_since(t) >= SCROLL_DEBOUNCE);
        let sixel = self.mode() == Graphics::Sixel;
        let limit = if sixel && self.passthrough() == Passthrough::Tmux {
            TMUX_MAX_SIXELS
        } else {
            usize::MAX
        };
        for (idx, p, shown) in visible {
            let row = (p.line as usize + usize::from(shown.start)).saturating_sub(at.top);
            let rows = usize::from(shown.end - shown.start);
            if self.shown.placed.contains_key(idx) || at.covers(row, rows) {
                continue;
            }
            if !settled {
                self.awaiting = true;
                continue;
            }
            if self.shown.placed.len() >= limit {
                break;
            }
            let make = if sixel {
                Make::Sixel(shown.clone())
            } else {
                Make::Iterm(shown.clone())
            };
            let k = key(store, p, make);
            let bytes = match self.cache.get(&k) {
                Some(Slot::Ready(r)) => match &r.made {
                    Made::Pixels(bytes) => bytes,
                    _ => continue,
                },
                Some(Slot::Pending(_) | Slot::Failed) => continue,
                None => {
                    self.request(k);
                    continue;
                }
            };
            cursor_to(after, row, at.indent.saturating_add(p.col));
            after.extend_from_slice(bytes);
            self.cache.touch(&k);
            let placed = Placed {
                row,
                shown: shown.clone(),
                kitty: None,
            };
            self.shown.placed.insert(*idx, placed);
        }
    }

    // --- The terminal ------------------------------------------------------------

    /// The bytes that delete every kitty image the pager has in the
    /// terminal (to write when the terminal is put back), when they changed
    /// since the last call.
    pub(crate) fn cleanup(&mut self) -> Option<Vec<u8>> {
        if !std::mem::take(&mut self.cleanup_changed) {
            return None;
        }
        let ids = self.live.values().chain(&self.doomed).copied();
        Some(kitty::delete_all(ids, self.passthrough()))
    }

    /// The terminal lost what the pager drew: it was put back (`deleted`:
    /// every kitty image was deleted with it) or is to be redrawn from
    /// scratch (^L: they are deleted with the next frame). kitty images are
    /// uploaded again under new ids (an id is never used twice), and
    /// placements and pixel images drawn again.
    pub(crate) fn reset(&mut self, deleted: bool) {
        for (key, id) in self.live.drain() {
            self.cache.remove(&key);
            if !deleted {
                self.doomed.push(id);
            }
        }
        self.forget_shown();
        if deleted {
            self.doomed.clear();
            self.queued.clear();
        }
        self.cleanup_changed = true;
    }
}

/// What the figure rows of a frame show ([`Images::rows`]).
pub(crate) struct FrameRows<'a> {
    images: &'a Images,
}

impl ImageRows for FrameRows<'_> {
    fn row(&self, p: &Placement, row: u16) -> Option<RowContent<'_>> {
        let images = self.images;
        let store = images.current?;
        match images.shown.rows.get(&p.line)? {
            RowsShow::Box => None,
            RowsShow::Cells => match images.cache.made(&key(store, p, Make::Blocks))? {
                Made::Blocks(raster) => raster.row(row).map(RowContent::Cells),
                _ => None,
            },
            RowsShow::Text(_) => match images.cache.made(&key(store, p, Make::Placeholders))? {
                Made::Placeholders(ph) => {
                    ph.rows.get(usize::from(row)).map(|r| RowContent::Bytes(r))
                }
                _ => None,
            },
            RowsShow::Blank => images
                .blank
                .get(..usize::from(p.cols))
                .map(RowContent::Bytes),
        }
    }
}

#[cfg(test)]
mod tests;
