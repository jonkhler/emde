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

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::OpenCommand;
use crate::highlight::Highlighter;
use crate::ir::ImageId;
use crate::layout::{self, ImageSizer, Layout};
use crate::options::Height;
use crate::options::RenderOptions;
use crate::render::RenderConfig;
use crate::source::{Origin, find_readme};
use crate::term::Caps;
use crate::term::env::Env;
use crate::term::tmux::TmuxInfo;
use crate::theme::Theme;

use super::diff::{Extras, Screen};
use super::images::{self, Images, WORK_POLL};
use super::keymap::{Command, SEQUENCE_TIMEOUT};
use super::os::{self, ClipboardPlan, OpenPlan};
use super::state::{DocKey, Settings, State};
use super::term::{
    MAX_POLL, MIN_POLL, MOUSE_OFF, MOUSE_ON, Signals, TermEvent, Terminal, clamp_poll,
};
use super::update::{Action, Effect, LoadRequest, Nav, update};
use super::view::{Ctx, view};
use super::watch::{self, Stamp, Watcher};
use super::{DocLoader, PagerExit, PagerSession};

/// Quiet time after a resize before the layout follows.
pub(crate) const RESIZE_DEBOUNCE: Duration = Duration::from_millis(30);
/// Events taken in one go before a frame is painted.
const MAX_BATCH: usize = 64;

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

/// The image sizer for a layout with figure `image` zoomed: other figures
/// keep their usual height cap (`rows`).
struct Zoomed<'a> {
    inner: &'a dyn ImageSizer,
    image: Option<ImageId>,
    rows: u16,
}

impl ImageSizer for Zoomed<'_> {
    fn cells(&self, image: ImageId, max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
        let rows = if Some(image) == self.image {
            max_rows
        } else {
            max_rows.min(self.rows)
        };
        self.inner.cells(image, max_cols, rows)
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
    open: OpenCommand,
    ctx: Ctx,
    cfg: RenderConfig,
    screen: Screen,
    watcher: Option<Watcher>,
    /// The stamp of each file when it was last read (for the watcher's
    /// baseline when a document comes back from memory).
    stamps: HashMap<PathBuf, Option<Stamp>>,
    resize: Option<((u16, u16), Instant)>,
    /// When an unfinished key sequence stops waiting for its next key.
    key_deadline: Option<Instant>,
    signals: Signals,
    /// Something may have changed since the last frame.
    dirty: bool,
    /// Panic after the first frame (`EMDE_TEST_PANIC=1`).
    panic_test: bool,
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
        } = session;
        caps.size = Some(size);
        let settings = Settings::new(&pager, &opts);
        let mut images = Images::new(images);
        let key = key_of(&doc.source.origin);
        let sizer = images.sizer(&key.clone().unwrap_or(DocKey::Unnamed(0)), &doc.doc);
        let first = first.filter(|l| l.width == size.0).unwrap_or_else(|| {
            layout::layout(&doc.doc, size.0, &theme, &caps, &opts, &*highlighter, sizer)
        });
        let mut stamps = HashMap::new();
        if let Origin::File(p) = &doc.source.origin {
            stamps.insert(p.clone(), watch::stamp(p));
        }
        let mut state = State::new(doc, key, first, size, &pager, settings);
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
        if let Some(anchor) = anchor {
            update(&mut state, Action::Anchor(anchor));
        }
        if open_toc {
            update(&mut state, Action::Command(Command::Outline));
        }
        let ctx = Ctx::new(&theme, &caps);
        let cfg = RenderConfig::from_caps(&caps);
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
            open: pager.open,
            ctx,
            cfg,
            screen: Screen::new(),
            watcher: None,
            stamps,
            resize: None,
            key_deadline: None,
            signals,
            dirty: true,
            panic_test,
        }
    }

    /// Run until the reader quits or a signal says so.
    pub(crate) fn run(mut self) -> io::Result<PagerExit> {
        self.term.term.set_cleanup(Vec::new());
        self.term.term.enter(self.state.mouse())?;
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
        let now = self.term.term.now();
        if self.images.deadline().is_some_and(|at| now >= at) {
            self.dirty = true;
        }
        if self.images.waits_when_idle() && self.images.busy() && self.images.settle() {
            self.dirty = true;
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
                    let layout = self.layout();
                    update(&mut self.state, Action::Layout(layout))
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
            };
            queue.extend(more);
        }
        // Images of documents the history dropped go with them.
        let state = &self.state;
        self.images.retain(|key| state.has_page(key));
        self.sync_watcher();
        Ok(None)
    }

    /// Lay the current document out for the current size and settings
    /// (a zoomed figure as large as the screen allows).
    fn layout(&mut self) -> Layout {
        let (cols, rows) = self.state.size();
        self.caps.size = Some((cols, rows));
        let zoom = self.state.zoom();
        self.opts.max_width = if self.state.wide() || zoom.is_some() {
            0
        } else {
            self.max_width
        };
        let normal_height = self.opts.images.max_height;
        let normal_rows = layout::max_image_rows(&self.opts, &self.caps);
        if zoom.is_some() {
            self.opts.images.max_height = Height::Rows(rows.saturating_sub(2).max(1));
        }
        let doc = self.state.document();
        let inner = self.images.sizer(self.state.key(), doc);
        let zoomed = Zoomed {
            inner,
            image: zoom,
            rows: normal_rows,
        };
        let out = layout::layout(
            doc,
            cols,
            &self.theme,
            &self.caps,
            &self.opts,
            &*self.highlighter,
            &zoomed,
        );
        self.opts.images.max_height = normal_height;
        out
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
        let bytes = self.screen.write(
            &frame,
            &diff,
            &self.state.page.doc,
            &self.state.layout,
            &self.cfg,
            extras,
        );
        if !bytes.is_empty() {
            self.term.term.write(&bytes)?;
            self.term.term.flush()?;
        }
        if let Some(cleanup) = self.images.cleanup() {
            self.term.term.set_cleanup(cleanup);
        }
        Ok(())
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
    /// its README, anything else only by path.
    fn show_file(&mut self, path: PathBuf) -> Vec<Effect> {
        if path.is_dir() {
            return self.load(LoadRequest {
                path,
                anchor: None,
                restore: None,
                nav: Nav::Push,
            });
        }
        let shown = crate::text::sanitize(&path.display().to_string()).into_owned();
        update(
            &mut self.state,
            Action::Message(format!("not Markdown: {shown}")),
        )
    }

    fn open_url(&mut self, url: &str) -> io::Result<Vec<Effect>> {
        let remote = self.caps.over_ssh;
        let message = match os::open_plan(&self.open, &self.env, remote, url) {
            OpenPlan::Spawn(argv) => match os::spawn_detached(&argv) {
                Ok(()) => Action::Message(format!("opened {url}")),
                Err(e) => {
                    self.copy(url)?;
                    Action::Error(format!("could not open the link ({e}); URL copied"))
                }
            },
            OpenPlan::Copy(reason) => {
                self.copy(url)?;
                Action::Message(format!("copied {url} · {reason}"))
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
        let ran = self.term.term.run_foreground(&argv);
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
