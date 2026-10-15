//! The event loop: terminal events in, [`update()`] on them, the effects
//! carried out, frames painted.
//!
//! The loop polls the terminal with a timeout between [`MIN_POLL`] and
//! [`MAX_POLL`], set by what is due next: the resize debounce
//! ([`RESIZE_DEBOUNCE`]), the file watcher, the end of the image debounce,
//! and, while the image worker has jobs out, [`WORK_POLL`] (its results
//! are picked up at every turn). After an event it takes whatever else is
//! already queued before painting, so a burst of wheel events costs one
//! frame. Signal flags are checked on every turn.
//!
//! Documents of [`STEPWISE_FROM`] bytes and more are laid out in steps
//! ([`LayoutJob`]): the first screens before the first frame (and after a
//! resize, the screens around the place shown), the rest in idle slices of
//! [`LAYOUT_SLICE`]. Before every action the layout is taken as far as the
//! action needs ([`super::partial`]): all of it for `G`, a search or the
//! outline.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::OpenCommand;
use crate::highlight::Highlighter;
use crate::layout::{self, Folds, Layout, LayoutInputs, LayoutJob};
use crate::options::Height;
use crate::options::RenderOptions;
use crate::render::RenderConfig;
use crate::source::{Origin, find_readme};
use crate::term::Caps;
use crate::term::env::Env;
use crate::term::tmux::TmuxInfo;
use crate::theme::Theme;

use super::diff::{Extras, Screen};
use super::gallery;
use super::images::{self, Images, WORK_POLL};
use super::keymap::{Command, SEQUENCE_TIMEOUT};
use super::os::{self, ClipboardPlan, OpenPlan};
use super::partial::{self, Need};
use super::picker::scan::{self, Scanner};
use super::picker::{self, Event, PickerEvent, PickerJob};
use super::recent;
use super::sidebar;
use super::state::{DocKey, Mode, Page, Settings, State};
use super::term::{
    MAX_POLL, MIN_POLL, MOUSE_OFF, MOUSE_ON, Signals, TermEvent, Terminal, clamp_poll,
};
use super::update::{Action, Effect, LoadRequest, Nav, update};
use super::view::{Ctx, view};
use super::watch::{self, Stamp, Watcher};
use super::{DocLoader, PagerExit, PagerSession};

/// Quiet time after a resize before the layout follows.
pub(crate) const RESIZE_DEBOUNCE: Duration = Duration::from_millis(30);
/// Documents of this many bytes of Markdown and more are laid out in steps
/// (see the module docs); smaller ones take a few milliseconds at most and
/// are laid out at once.
const STEPWISE_FROM: usize = 256 * 1024;
/// The longest an idle slice of laying out runs: a key pressed meanwhile
/// waits at most this long.
const LAYOUT_SLICE: Duration = Duration::from_millis(4);
/// Layouts kept to show again without laying out (a `<details>` closed
/// again, a match's block opened again): those the last relayouts
/// replaced.
const LAYOUT_CACHE: usize = 2;

/// What a layout of the current document was made from (besides what
/// never changes while the pager runs).
#[derive(Clone, Debug)]
struct LayoutKey {
    /// The document (held, so its address is never reused meanwhile).
    page: Rc<Page>,
    size: (u16, u16),
    cols: u16,
    wide: bool,
    images: crate::term::Graphics,
    toggled: Vec<u32>,
}

impl PartialEq for LayoutKey {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.page, &o.page)
            && (self.size, self.cols, self.wide, self.images) == (o.size, o.cols, o.wide, o.images)
            && self.toggled == o.toggled
    }
}
/// Events taken in one go before a frame is painted.
const MAX_BATCH: usize = 64;
/// The longest the event loop waits for the picker's search when it waits
/// for it at all ([`super::PickerSetup::wait_when_idle`]).
const SCAN_SETTLE: Duration = Duration::from_secs(10);

/// Puts the terminal back when the shell ends, however it ends.
struct Guard<'t, T: Terminal> {
    term: &'t mut T,
}

impl<T: Terminal> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        let _ = self.term.leave();
    }
}

/// The key of a document read from `origin`: its canonical path.
pub(crate) fn key_of(origin: &Origin) -> Option<DocKey> {
    match origin {
        Origin::File(p) => Some(DocKey::Path(
            std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()),
        )),
        Origin::Stdin | Origin::Memory => None,
    }
}

/// Whether the regular file at `path` starts like an image (reading its
/// first bytes only; never a FIFO or a device, which could block or never
/// end).
#[inline(never)]
fn sniff_file(path: &Path) -> bool {
    use std::io::Read as _;
    let mut head = [0u8; 512];
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut n = 0;
    let mut take = file.take(head.len() as u64);
    while let Ok(k) = take.read(&mut head[n..]) {
        if k == 0 {
            break;
        }
        n += k;
    }
    gallery::sniff(&head[..n])
}

/// An I/O error as a message: what went wrong, without the OS error
/// number.
fn io_reason(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "not found".to_owned(),
        io::ErrorKind::PermissionDenied => "permission denied".to_owned(),
        io::ErrorKind::NotADirectory => "not a directory".to_owned(),
        _ => e.to_string(),
    }
}

/// A directory's README, or the path itself.
fn document_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        find_readme(path).unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

/// The rest of the current layout, while it is laid out in steps.
struct Job {
    job: LayoutJob,
    /// The `<details>` toggled when it started.
    toggled: Vec<u32>,
}

/// Lay `layout` out further with `job` until `need` is met (nothing if it
/// already is); whether the layout is complete.
fn advance_to(
    job: &mut LayoutJob,
    layout: &mut Layout,
    inputs: LayoutInputs<'_>,
    need: Need,
) -> bool {
    let mut seen = None;
    if need.met(layout, &mut seen) {
        return job.is_done();
    }
    job.advance(layout, inputs, &mut |l| need.met(l, &mut seen))
}

/// The event loop and everything it needs; see the module docs.
pub(crate) struct Shell<'t, T: Terminal> {
    term: Guard<'t, T>,
    state: State,
    loader: Box<dyn DocLoader>,
    theme: Theme,
    caps: Caps,
    opts: RenderOptions,
    max_width: u16,
    highlighter: Arc<dyn Highlighter>,
    images: Images,
    env: Env,
    /// The tmux query result: from the session, else asked for the first
    /// time something is copied inside tmux.
    tmux: Option<TmuxInfo>,
    /// Whether tmux was asked already (it is asked once).
    tmux_asked: bool,
    /// Why block images have fewer colours than they could (a tmux client
    /// without `RGB`), said once in the status bar the first time a block
    /// image is on screen; taken when said.
    image_note: Option<String>,
    open: OpenCommand,
    ctx: Ctx,
    cfg: RenderConfig,
    screen: Screen,
    watcher: Option<Watcher>,
    /// The stamp of each file when it was last read (for the watcher's
    /// baseline when a document comes back from memory).
    stamps: HashMap<PathBuf, Option<Stamp>>,
    resize: Option<((u16, u16), Instant)>,
    /// Layouts replaced lately, newest last (see [`LAYOUT_CACHE`]).
    layouts: Vec<(LayoutKey, Layout)>,
    /// What the layout shown was made from.
    made: Option<LayoutKey>,
    /// Laying out the rest of the layout shown, when it is not complete.
    job: Option<Job>,
    /// Effects of the first actions, carried out when the loop starts.
    startup: Vec<Effect>,
    /// When an unfinished key sequence stops waiting for its next key.
    key_deadline: Option<Instant>,
    signals: Signals,
    /// Something may have changed since the last frame.
    dirty: bool,
    /// Panic after the first frame (`EMDE_TEST_PANIC=1`).
    panic_test: bool,
    /// The file picker's background search.
    scanner: Scanner,
    /// What the search needs.
    scan_setup: scan::Setup,
    /// Take the search's results only when idle, all of them.
    scan_waits: bool,
    /// The file recorded in the recent files store last.
    recorded: Option<PathBuf>,
}

impl<'t, T: Terminal> Shell<'t, T> {
    /// Lay the first document out for the terminal's size (before the
    /// terminal is touched).
    pub(crate) fn new(
        term: &'t mut T,
        session: PagerSession,
        signals: Signals,
        panic_test: bool,
    ) -> Shell<'t, T> {
        let size = term
            .size()
            .ok()
            .filter(|&(c, r)| c > 0 && r > 0)
            .unwrap_or((80, 24));
        let PagerSession {
            doc,
            more,
            loader,
            theme,
            mut caps,
            opts,
            highlighter,
            images,
            pager,
            anchor,
            open_toc,
            env,
            tmux,
            late_replies: _,
            layout: first,
            picker,
        } = session;
        caps.size = Some(size);
        let mut settings = Settings::new(&pager, &opts);
        settings.picker = picker.options.clone();
        settings.home = picker.home.clone();
        settings.cwd = picker
            .cwd
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut images = Images::new(images);
        let key = key_of(&doc.source.origin);
        let sizer = images.sizer(&key.clone().unwrap_or(DocKey::Unnamed(0)), &doc.doc);
        // The columns the outline sidebar leaves.
        let width = size
            .0
            .saturating_sub(sidebar::cols(&settings.sidebar, None, size.0, &doc.doc));
        // A layout made for stream mode will do if it has neither
        // `<details>` (collapsed here) nor wide blocks (cut there).
        let first = first.filter(|l| l.width == width && l.details.is_empty() && !l.narrowed);
        let made_here = first.is_none();
        let mut job = None;
        let first = first.unwrap_or_else(|| {
            let inputs = LayoutInputs {
                doc: &doc.doc,
                theme: &theme,
                caps: &caps,
                opts: &opts,
                highlighter: &*highlighter,
                sizer,
                interactive: settings.interactive(),
            };
            let (mut steps, mut layout) = LayoutJob::start(inputs, width);
            // An anchor, the outline or the sidebar need every heading.
            let stepwise = doc.source.text.len() >= STEPWISE_FROM
                && anchor.is_none()
                && !open_toc
                && width == size.0;
            let need = if stepwise {
                partial::first_screens(size.1)
            } else {
                Need::Whole
            };
            if !advance_to(&mut steps, &mut layout, inputs, need) {
                job = Some(Job {
                    job: steps,
                    toggled: Vec::new(),
                });
            }
            layout
        });
        let mut stamps = HashMap::new();
        if let Origin::File(p) = &doc.source.origin {
            stamps.insert(p.clone(), watch::stamp(p));
        }
        let mut state = State::new(doc, key, first, size, &pager, settings);
        state.partial = job.is_some();
        let made = made_here.then(|| LayoutKey {
            page: Rc::clone(&state.page),
            size,
            cols: width,
            wide: false,
            images: images.mode(),
            toggled: Vec::new(),
        });
        let more = more
            .into_iter()
            .map(|d| {
                if let Origin::File(p) = &d.source.origin {
                    stamps.insert(p.clone(), watch::stamp(p));
                }
                let key = key_of(&d.source.origin);
                (d, key)
            })
            .collect();
        state.add_files(more);
        // Carried out once the terminal is set up (an anchor inside
        // collapsed `<details>` lays the document out again).
        let startup = match anchor {
            Some(anchor) => update(&mut state, Action::Anchor(anchor)),
            None => Vec::new(),
        };
        if open_toc {
            update(&mut state, Action::Command(Command::Outline));
        }
        let ctx = Ctx::new(&theme, &caps);
        let cfg = RenderConfig::from_caps(&caps);
        let image_note = crate::term::doctor::image_colour_note(&caps, tmux.as_ref());
        Shell {
            term: Guard { term },
            state,
            loader,
            theme,
            max_width: opts.max_width,
            caps,
            opts,
            highlighter,
            images,
            env,
            tmux_asked: tmux.is_some(),
            tmux,
            image_note,
            open: pager.open,
            ctx,
            cfg,
            screen: Screen::new(),
            watcher: None,
            stamps,
            resize: None,
            layouts: Vec::new(),
            made,
            job,
            startup,
            key_deadline: None,
            signals,
            dirty: true,
            panic_test,
            scanner: Scanner::default(),
            scan_setup: scan::Setup {
                options: picker.options,
                home: picker.home,
                recent: picker.recent,
                git: picker.git,
            },
            scan_waits: picker.wait_when_idle,
            recorded: None,
        }
    }

    /// Run until the reader quits or a signal says so.
    pub(crate) fn run(mut self) -> io::Result<PagerExit> {
        self.term.term.set_cleanup(Vec::new());
        self.term.term.enter(self.state.mouse())?;
        let startup = std::mem::take(&mut self.startup);
        if let Some(exit) = self.run_effects(startup)? {
            return Ok(exit);
        }
        self.sync_watcher();
        self.paint()?;
        if self.panic_test {
            panic!("EMDE_TEST_PANIC: a deliberate panic to test the terminal restore");
        }
        loop {
            if let Some(sig) = self.signals.exit_requested() {
                return Ok(PagerExit::Signal(sig));
            }
            if self.signals.take_stop() {
                // SIGTSTP from outside: as Ctrl-Z.
                self.suspend()?;
                self.paint()?;
            }
            if self.signals.take_continued() {
                // Stopped from outside and continued: set up again and
                // repaint now, not after the next event.
                self.resume()?;
                self.images.reset(false);
                self.paint()?;
            }
            if !self.scan_waits && self.scanner.busy() {
                let found = self.scanner.take();
                if let Some(exit) = self.found(found)? {
                    return Ok(exit);
                }
                if self.resize.is_none() {
                    self.paint()?;
                }
            }
            if !self.images.waits_when_idle() && self.images.collect() {
                // Show what came in now, not after the next poll (which
                // may be a long one once the worker has nothing left).
                self.dirty = true;
                if self.resize.is_none() {
                    self.paint()?;
                }
            }
            let now = self.term.term.now();
            if let Some(exit) = self.fire_resize(now)? {
                return Ok(exit);
            }
            if self.key_deadline.is_some_and(|at| now >= at) {
                self.key_deadline = None;
                if let Some(exit) = self.dispatch(Action::KeyTimeout)? {
                    return Ok(exit);
                }
                self.paint()?;
            }
            match self.term.term.poll(self.timeout(now))? {
                Some(event) => {
                    if let Some(exit) = self.event(event)? {
                        return Ok(exit);
                    }
                    for _ in 0..MAX_BATCH {
                        match self.term.term.poll(MIN_POLL)? {
                            Some(event) => {
                                if let Some(exit) = self.event(event)? {
                                    return Ok(exit);
                                }
                            }
                            None => break,
                        }
                    }
                }
                None => {
                    if let Some(exit) = self.idle()? {
                        return Ok(exit);
                    }
                }
            }
            if self.resize.is_none() {
                self.paint()?;
            }
        }
    }

    /// How long to wait for the next event.
    fn timeout(&self, now: Instant) -> Duration {
        let mut due = MAX_POLL;
        if self.job.is_some() {
            // Laying out goes on between polls.
            due = MIN_POLL;
        }
        if let Some((_, at)) = self.resize {
            due = due.min(at.saturating_duration_since(now));
        }
        if let Some(at) = self.key_deadline {
            due = due.min(at.saturating_duration_since(now));
        }
        if let Some(w) = &self.watcher {
            due = due.min(w.deadline().saturating_duration_since(now));
        }
        if let Some(at) = self.images.deadline() {
            due = due.min(at.saturating_duration_since(now));
        }
        if self.images.busy() && !self.images.waits_when_idle() {
            due = due.min(WORK_POLL);
        }
        if self.scanner.busy() && !self.scan_waits {
            due = due.min(WORK_POLL);
        }
        clamp_poll(due)
    }

    fn event(&mut self, event: TermEvent) -> io::Result<Option<PagerExit>> {
        match event {
            TermEvent::Key(key) => {
                let exit = self.dispatch(Action::Key(key))?;
                self.key_deadline = self
                    .state
                    .keys_pending()
                    .then(|| self.term.term.now() + SEQUENCE_TIMEOUT);
                Ok(exit)
            }
            TermEvent::Mouse(m) if self.state.mouse() => self.dispatch(Action::Mouse(m)),
            TermEvent::Mouse(_) | TermEvent::Focus(_) => Ok(None),
            TermEvent::Paste(text) => self.dispatch(Action::Paste(text)),
            TermEvent::Resize { cols, rows } => {
                let at = self.term.term.now() + RESIZE_DEBOUNCE;
                self.resize = Some(((cols, rows), at));
                Ok(None)
            }
        }
    }

    /// Apply a debounced resize once it is due.
    fn fire_resize(&mut self, now: Instant) -> io::Result<Option<PagerExit>> {
        match self.resize {
            Some(((cols, rows), at)) if now >= at => {
                self.resize = None;
                self.screen.invalidate();
                self.dirty = true;
                if cols == 0 || rows == 0 {
                    return Ok(None);
                }
                let exit = self.dispatch(Action::Resize { cols, rows })?;
                self.paint()?;
                Ok(exit)
            }
            _ => Ok(None),
        }
    }

    /// Nothing happened: time for the watcher, and for images (the end of
    /// the debounce, or the worker's results when they are waited for).
    fn idle(&mut self) -> io::Result<Option<PagerExit>> {
        if self.job.is_some() {
            let end = Instant::now() + LAYOUT_SLICE;
            self.lay_out_more(&mut |_| Instant::now() >= end);
        }
        let now = self.term.term.now();
        if self.images.deadline().is_some_and(|at| now >= at) {
            self.dirty = true;
        }
        if self.images.waits_when_idle() && self.images.busy() && self.images.settle() {
            self.dirty = true;
        }
        if self.scan_waits && self.scanner.busy() {
            let found = self.scanner.wait(SCAN_SETTLE);
            if let Some(exit) = self.found(found)? {
                return Ok(Some(exit));
            }
        }
        let changed = match &mut self.watcher {
            Some(w) => w.poll(now),
            None => false,
        };
        if changed {
            let effects = self.reload(true);
            return self.run_effects(effects);
        }
        Ok(None)
    }

    /// Update the state, then carry out the effects (which may update the
    /// state again).
    fn dispatch(&mut self, action: Action) -> io::Result<Option<PagerExit>> {
        if self.job.is_some() {
            let need = partial::action_need(&self.state, &action);
            let mut seen = None;
            if !need.met(&self.state.layout, &mut seen) {
                self.lay_out_more(&mut |l| need.met(l, &mut seen));
            }
        }
        let effects = update(&mut self.state, action);
        self.run_effects(effects)
    }

    fn run_effects(&mut self, effects: Vec<Effect>) -> io::Result<Option<PagerExit>> {
        self.dirty = true;
        let mut queue: VecDeque<Effect> = effects.into();
        while let Some(effect) = queue.pop_front() {
            let more = match effect {
                Effect::Quit => return Ok(Some(PagerExit::Quit)),
                Effect::Relayout => {
                    let was_partial = self.job.is_some();
                    let (key, layout) = self.layout();
                    let more = update(&mut self.state, Action::Layout(layout));
                    self.state.partial = self.job.is_some();
                    // The layout it replaced, for later (when complete).
                    if let (Some(made), Some(old)) =
                        (self.made.replace(key), self.state.spare.take())
                        && !was_partial
                    {
                        if self.layouts.len() >= LAYOUT_CACHE {
                            self.layouts.remove(0);
                        }
                        self.layouts.push((made, old));
                    }
                    more
                }
                Effect::Redraw => {
                    self.screen.invalidate();
                    self.images.reset(false);
                    Vec::new()
                }
                Effect::Load(req) => self.load(req),
                Effect::ShowFile(path) => self.show_file(path),
                Effect::Open(url) => self.open_url(&url)?,
                Effect::Copy { text, what } => {
                    self.copy(&text)?;
                    update(&mut self.state, Action::Message(what))
                }
                Effect::Edit { path, line } => self.edit(&path, line)?,
                Effect::Reload => self.reload(false),
                Effect::SetMouse(on) => {
                    self.term
                        .term
                        .write(if on { MOUSE_ON } else { MOUSE_OFF })?;
                    self.term.term.flush()?;
                    Vec::new()
                }
                Effect::Suspend => {
                    self.suspend()?;
                    Vec::new()
                }
                Effect::CycleImages => self.cycle_images(),
                Effect::ShowImage => self.show_image(),
                Effect::OpenFile(path) => self.open_file(&path)?,
                Effect::OpenPath(path) => self.open_path(path),
                Effect::Picker(PickerJob(job)) => self.picker_job(job),
            };
            queue.extend(more);
        }
        // The search is for the picker only.
        if self.scanner.busy() && !matches!(self.state.mode, Mode::Picker(_)) {
            self.scanner.stop();
        }
        self.record_recent();
        // Images of documents the history dropped go with them (and those
        // of the image view once it closes).
        let state = &self.state;
        self.images
            .retain(|key| state.has_page(key), state.image_view().is_some());
        self.sync_watcher();
        Ok(None)
    }

    /// What the picker's search found: all batches in one action per
    /// search.
    fn found(&mut self, batches: Vec<scan::Batch>) -> io::Result<Option<PagerExit>> {
        let mut merged: Vec<(u64, Vec<picker::Candidate>, bool)> = Vec::new();
        for b in batches {
            match merged.iter_mut().find(|(scan, ..)| *scan == b.scan) {
                Some(m) => {
                    m.1.extend(b.files);
                    m.2 |= b.done;
                }
                None => merged.push((b.scan, b.files, b.done)),
            }
        }
        for (scan, files, done) in merged {
            let event = Event::Found { scan, files, done };
            if let Some(exit) = self.dispatch(Action::Picker(PickerEvent(event)))? {
                return Ok(Some(exit));
            }
        }
        Ok(None)
    }

    /// Work for the picker or the `:` prompt.
    fn picker_job(&mut self, job: picker::Job) -> Vec<Effect> {
        let event = match job {
            picker::Job::Scan(req) => {
                self.scanner.start(req, &self.scan_setup);
                return Vec::new();
            }
            picker::Job::Stop => {
                self.scanner.stop();
                return Vec::new();
            }
            picker::Job::List(dir) => {
                let entries = picker::paths::list(&dir, picker::MAX_ENTRIES).map_err(|e| {
                    let shown = picker::paths::tilde(&dir, self.state.settings.home.as_deref());
                    format!("{}: {}", crate::text::sanitize(&shown), io_reason(&e))
                });
                Event::Listed { dir, entries }
            }
            picker::Job::Preview(path) => {
                let parse = crate::parse::ParseOptions::from(&self.opts);
                let exts = &self.state.settings.picker.extensions;
                Event::Previewed(picker::read_preview(&path, exts, &parse))
            }
        };
        update(&mut self.state, Action::Picker(PickerEvent(event)))
    }

    /// Open `path` for the picker or `:e`: a Markdown file as a document, a
    /// directory through its README or index (else listed in the picker),
    /// an image in the image view; anything else is only named.
    fn open_path(&mut self, path: PathBuf) -> Vec<Effect> {
        let home = self.state.settings.home.clone();
        let shown =
            crate::text::sanitize(&picker::paths::tilde(&path, home.as_deref())).into_owned();
        let push = |path| LoadRequest {
            path,
            anchor: None,
            restore: None,
            nav: Nav::Push,
        };
        let error = match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => {
                let index = find_readme(&path).or_else(|| {
                    let index = path.join("index.md");
                    index.is_file().then_some(index)
                });
                return match index {
                    Some(doc) => self.load(push(doc)),
                    None => update(
                        &mut self.state,
                        Action::Picker(PickerEvent(Event::Browse(path))),
                    ),
                };
            }
            Ok(m) if !m.is_file() => format!("not a regular file: {shown}"),
            Ok(_) if scan::is_markdown(&path, &self.state.settings.picker.extensions) => {
                return self.load(push(path));
            }
            Ok(_) if sniff_file(&path) => return update(&mut self.state, Action::ViewFile(path)),
            Ok(_) => {
                let text = format!("not Markdown: {shown}");
                return update(&mut self.state, Action::Message(text));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => format!("no such file: {shown}"),
            Err(e) => format!("cannot open {shown}: {}", io_reason(&e)),
        };
        update(&mut self.state, Action::Error(error))
    }

    /// Record the document shown in the recent files store (once per
    /// visit).
    fn record_recent(&mut self) {
        let (Some(store), Some(path)) = (&self.scan_setup.recent, self.state.key().path()) else {
            return;
        };
        if self.recorded.as_deref() != Some(path) {
            recent::record(store, path);
            self.recorded = Some(path.to_path_buf());
        }
    }

    /// Lay the current document out for the current size and settings, in
    /// the columns the outline sidebar leaves (or take a layout made for
    /// them before); with what it was made from.
    fn layout(&mut self) -> (LayoutKey, Layout) {
        let (cols, rows) = self.state.size();
        self.caps.size = Some((cols, rows));
        self.opts.max_width = if self.state.wide() { 0 } else { self.max_width };
        let key = LayoutKey {
            page: Rc::clone(&self.state.page),
            size: (cols, rows),
            cols: self.state.doc_cols(),
            wide: self.state.wide(),
            images: self.images.mode(),
            toggled: self
                .state
                .doc_view()
                .map(|v| v.toggled.clone())
                .unwrap_or_default(),
        };
        let doc = self.state.document();
        // Also says which store the next frames show.
        let sizer = self.images.sizer(self.state.key(), doc);
        // Only the current document's layouts are kept.
        self.layouts.retain(|(k, _)| Rc::ptr_eq(&k.page, &key.page));
        if let Some(i) = self.layouts.iter().position(|(k, _)| *k == key) {
            let (_, layout) = self.layouts.remove(i);
            self.job = None;
            return (key, layout);
        }
        let inputs = LayoutInputs {
            doc,
            theme: &self.theme,
            caps: &self.caps,
            opts: &self.opts,
            highlighter: &*self.highlighter,
            sizer,
            interactive: self.state.interactive(),
        };
        let (mut job, mut layout) = LayoutJob::start(inputs, key.cols);
        let need = if self.state.source().text.len() >= STEPWISE_FROM {
            partial::relayout_need(&self.state)
        } else {
            Need::Whole
        };
        let done = advance_to(&mut job, &mut layout, inputs, need);
        self.job = (!done).then(|| Job {
            job,
            toggled: key.toggled.clone(),
        });
        (key, layout)
    }

    /// Lay out more of the layout shown, until `enough` says so or it is
    /// complete (see [`LayoutJob::advance`]).
    fn lay_out_more(&mut self, enough: &mut dyn FnMut(&Layout) -> bool) {
        let Some(Job { job, toggled }) = self.job.as_mut() else {
            return;
        };
        let page = Rc::clone(&self.state.page);
        let key = page.key.clone();
        let mut interactive = self.state.settings.interactive();
        interactive.details = Some(Folds {
            open: self.state.settings.details_open,
            toggled,
        });
        let inputs = LayoutInputs {
            doc: &page.doc,
            theme: &self.theme,
            caps: &self.caps,
            opts: &self.opts,
            highlighter: &*self.highlighter,
            sizer: self.images.sizer(&key, &page.doc),
            interactive,
        };
        let done = job.advance(&mut self.state.layout, inputs, enough);
        if done {
            self.job = None;
            // The status bar shows the total now.
            self.dirty = true;
        }
        self.state.layout_grew(done);
    }

    /// The image view's image: read (or found among those read already, a
    /// figure's in its document's store), laid out alone to fill the screen
    /// above the status bar, keeping its aspect ratio, centred.
    #[inline(never)]
    fn show_image(&mut self) -> Vec<Effect> {
        let Some(picture) = self.state.viewed() else {
            return Vec::new();
        };
        let doc = gallery::document(picture, self.state.page.doc.base_dir.clone());
        let read = picture
            .figure
            .and_then(|id| self.images.source_of(self.state.key(), id));
        let (cols, rows) = self.state.size();
        let view_rows = rows.saturating_sub(1).max(1);
        let found = self.images.view(&doc, read);
        // The whole screen: no text column, no margins, no height cap
        // below the screen's.
        let kept = (
            self.opts.max_width,
            self.opts.margin,
            self.opts.images.max_height,
        );
        self.opts.max_width = 0;
        self.opts.margin = 0;
        self.opts.images.max_height = Height::Rows(view_rows);
        let mut layout = layout::layout(
            &doc,
            cols,
            &self.theme,
            &self.caps,
            &self.opts,
            &*self.highlighter,
            &self.images.view_sizer(),
        );
        (
            self.opts.max_width,
            self.opts.margin,
            self.opts.images.max_height,
        ) = kept;
        gallery::centre(&mut layout, usize::from(view_rows));
        let (pixels, problem) = match found {
            Ok(px) => (Some(px), None),
            Err(why) => (None, Some(why)),
        };
        update(
            &mut self.state,
            Action::ImageShown {
                doc: Box::new(doc),
                layout,
                pixels,
                problem,
            },
        )
    }

    /// Paint a frame if anything changed since the last one: the rows that
    /// changed, with the image layer's uploads before them and its pixel
    /// images and placements after.
    fn paint(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;
        let now = self.term.term.now();
        let mut frame = view(&self.state, &self.ctx);
        self.cfg.link_base = self.state.page.link_base;
        self.images
            .plan(&self.state, &mut frame, &mut self.screen, now);
        let diff = self.screen.diff(&frame);
        let (before, after) = self.images.extras(&self.state, &frame, &diff, now);
        let rows = self.images.rows();
        let extras = Extras {
            images: Some(&rows),
            before: &before,
            after: &after,
        };
        let shown = self.state.showing();
        let bytes = self
            .screen
            .write(&frame, &diff, shown.doc, shown.layout, &self.cfg, extras);
        if !bytes.is_empty() {
            self.term.term.write(&bytes)?;
            self.term.term.flush()?;
        }
        if let Some(cleanup) = self.images.cleanup() {
            self.term.term.set_cleanup(cleanup);
        }
        if self.say_image_note() {
            self.dirty = true;
            return self.paint();
        }
        Ok(())
    }

    /// The first time a block image is on screen, with no other message
    /// showing: say why it has fewer colours (once); whether it was said.
    fn say_image_note(&mut self) -> bool {
        if self.image_note.is_none() || self.state.message().is_some() || !self.images.cells_shown()
        {
            return false;
        }
        match self.image_note.take() {
            Some(note) => {
                self.state.say(note);
                true
            }
            None => false,
        }
    }

    /// Read a document for the state (or switch to it if it is in memory).
    fn load(&mut self, req: LoadRequest) -> Vec<Effect> {
        let path = document_path(&req.path);
        let key = DocKey::Path(std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()));
        if self.state.has_page(&key) {
            return update(&mut self.state, Action::Switch { key, request: req });
        }
        let before = watch::stamp(&path);
        match self.loader.load(&path) {
            Ok(doc) => {
                if let Origin::File(p) = &doc.source.origin {
                    self.stamps.insert(p.clone(), before);
                }
                let key = key_of(&doc.source.origin);
                update(
                    &mut self.state,
                    Action::Opened {
                        doc,
                        key,
                        request: req,
                    },
                )
            }
            Err(e) => update(
                &mut self.state,
                Action::LoadFailed {
                    request: req,
                    error: e.to_string(),
                },
            ),
        }
    }

    /// A link to a file that is not Markdown: a directory is shown through
    /// its README, an image (by its first bytes) in the image view,
    /// anything else only by path.
    fn show_file(&mut self, path: PathBuf) -> Vec<Effect> {
        if path.is_dir() {
            return self.load(LoadRequest {
                path,
                anchor: None,
                restore: None,
                nav: Nav::Push,
            });
        }
        if sniff_file(&path) {
            return update(&mut self.state, Action::ViewFile(path));
        }
        let shown = crate::text::sanitize(&path.display().to_string()).into_owned();
        update(
            &mut self.state,
            Action::Message(format!("not Markdown: {shown}")),
        )
    }

    fn open_url(&mut self, url: &str) -> io::Result<Vec<Effect>> {
        let plan = os::open_plan(&self.open, &self.env, self.caps.over_ssh, url);
        self.open_with(plan, url)
    }

    /// `o` in the image view: open a local image with the system's opener
    /// (or copy its path where that is not possible).
    #[inline(never)]
    fn open_file(&mut self, path: &Path) -> io::Result<Vec<Effect>> {
        let plan = os::open_file_plan(&self.open, &self.env, self.caps.over_ssh, path);
        self.open_with(plan, &path.display().to_string())
    }

    /// Carry out `plan` for `what` (a URL or a path, copied when it cannot
    /// be opened).
    fn open_with(&mut self, plan: OpenPlan, what: &str) -> io::Result<Vec<Effect>> {
        let shown = crate::text::sanitize(what).into_owned();
        let message = match plan {
            OpenPlan::Spawn(argv) => match os::spawn_detached(&argv) {
                Ok(()) => Action::Message(format!("opened {shown}")),
                Err(e) => {
                    self.copy(what)?;
                    Action::Error(format!("could not open it ({e}); copied it instead"))
                }
            },
            OpenPlan::Copy(reason) => {
                self.copy(what)?;
                Action::Message(format!("copied {shown} · {reason}"))
            }
        };
        Ok(update(&mut self.state, message))
    }

    /// Put `text` on the clipboard.
    fn copy(&mut self, text: &str) -> io::Result<()> {
        if !self.tmux_asked && self.env.is_set("TMUX") {
            // Inside tmux, OSC 52 only works with `set-clipboard on`. The
            // query takes a few milliseconds: asked now rather than at
            // every start.
            self.tmux_asked = true;
            self.tmux = crate::term::tmux::query(&self.env);
        }
        let tmux_done = match os::clipboard_plan(&self.env, self.tmux.as_ref()) {
            ClipboardPlan::TmuxBuffer => os::tmux_load_buffer(&self.env, text).is_ok(),
            ClipboardPlan::Osc52 => false,
        };
        if !tmux_done {
            self.term.term.write(&os::osc52(text))?;
            self.term.term.flush()?;
        }
        Ok(())
    }

    /// `e`: run the editor on `path` at `line` with the terminal put back,
    /// then set it up again and read the file again.
    fn edit(&mut self, path: &Path, line: usize) -> io::Result<Vec<Effect>> {
        let argv = os::editor_command(&self.env, path, line);
        self.term.term.leave()?;
        self.images.reset(true);
        self.term.term.set_cleanup(Vec::new());
        // Ctrl-Z in the editor stops its whole job, the pager included.
        self.signals.lend_terminal(true);
        let ran = self.term.term.run_foreground(&argv);
        self.signals.lend_terminal(false);
        // Signals from the terminal while the editor ran were the editor's.
        self.signals.forget_interrupt();
        let _ = self.signals.take_stop();
        let _ = self.signals.take_continued();
        self.resume()?;
        let program = argv
            .first()
            .map(|p| crate::text::sanitize(&p.to_string_lossy()).into_owned())
            .unwrap_or_default();
        Ok(match ran {
            Ok(true) => self.reload(false),
            Ok(false) => {
                let mut effects = self.reload(true);
                effects.extend(update(
                    &mut self.state,
                    Action::Error(format!("{program} ended with an error")),
                ));
                effects
            }
            Err(e) => update(
                &mut self.state,
                Action::Error(format!(
                    "could not run {program}: {e} (set $VISUAL or $EDITOR)"
                )),
            ),
        })
    }

    /// Read the file again; `auto` (from the watcher) is quiet when nothing
    /// changed.
    fn reload(&mut self, auto: bool) -> Vec<Effect> {
        let Some(path) = self.state.page.path().map(Path::to_path_buf) else {
            return Vec::new();
        };
        let now = self.term.term.now();
        let before = watch::stamp(&path);
        let loaded = self.loader.load(&path);
        if loaded.is_ok() {
            // Read: this version is the baseline (a failed read is tried
            // again at the watcher's next check).
            if let Some(w) = self.watcher.as_mut().filter(|w| w.path() == path) {
                w.rebase(before, now);
            }
            self.stamps.insert(path.clone(), before);
        }
        match loaded {
            Ok(doc) if doc.source.text == self.state.source().text => {
                if auto {
                    Vec::new()
                } else {
                    update(&mut self.state, Action::Message("unchanged".into()))
                }
            }
            Ok(doc) => update(&mut self.state, Action::Reloaded(doc)),
            Err(e) => update(
                &mut self.state,
                Action::Error(format!("reload failed: {e}")),
            ),
        }
    }

    /// Watch the current file if the state says so.
    fn sync_watcher(&mut self) {
        let want = if self.state.watching() {
            self.state.page.path().map(Path::to_path_buf)
        } else {
            None
        };
        if let Some(w) = &self.watcher {
            self.stamps.insert(w.path().to_path_buf(), w.baseline());
        }
        match (&self.watcher, want) {
            (Some(w), Some(p)) if w.path() == p => {}
            (_, Some(p)) => {
                // Unknown files count as changed: read again to be sure.
                let baseline = self.stamps.get(&p).copied().flatten();
                self.watcher = Some(Watcher::new(p, baseline, self.term.term.now()));
            }
            (_, None) => self.watcher = None,
        }
    }

    /// `i`: the next image mode, painted from scratch (laid out again when
    /// figures appear or turn into alt text).
    fn cycle_images(&mut self) -> Vec<Effect> {
        let Some((mode, relayout)) = self.images.cycle() else {
            return update(
                &mut self.state,
                Action::Message("images cannot be shown here".into()),
            );
        };
        self.screen.invalidate();
        let name = images::mode_name(mode).to_owned();
        update(&mut self.state, Action::ImageMode { name, relayout })
    }

    /// Ctrl-Z or SIGTSTP: put the terminal back (kitty images are deleted
    /// with it), stop, and set it up again on resume. Without job control
    /// nothing could continue the process: it carries on instead.
    fn suspend(&mut self) -> io::Result<()> {
        if !self.term.term.can_suspend() {
            update(
                &mut self.state,
                Action::Error("cannot suspend: no shell with job control to resume from".into()),
            );
            self.dirty = true;
            return Ok(());
        }
        self.term.term.leave()?;
        self.images.reset(true);
        self.term.term.set_cleanup(Vec::new());
        self.term.term.suspend()?;
        let _ = self.signals.take_continued();
        let _ = self.signals.take_stop();
        self.resume()
    }

    /// Set the terminal up again after the process was stopped and
    /// continued, and redraw everything.
    fn resume(&mut self) -> io::Result<()> {
        self.term.term.enter(self.state.mouse())?;
        self.screen.invalidate();
        self.dirty = true;
        if let Ok((cols, rows)) = self.term.term.size()
            && cols > 0
            && rows > 0
            && (cols, rows) != self.state.size()
        {
            self.dispatch(Action::Resize { cols, rows })?;
        }
        Ok(())
    }
}
