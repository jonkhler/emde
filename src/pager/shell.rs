//! The event loop: terminal events in, [`update()`] on them, the effects
//! carried out, frames painted.
//!
//! The loop polls the terminal with a timeout between [`MIN_POLL`] and
//! [`MAX_POLL`], set by what is due next: the resize debounce
//! ([`RESIZE_DEBOUNCE`]) and the file watcher. After an event it takes
//! whatever else is already queued before painting, so a burst of wheel
//! events costs one frame. Signal flags are checked on every turn.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::OpenCommand;
use crate::highlight::Highlighter;
use crate::layout::{self, Layout};
use crate::options::{ImageMode, RenderOptions};
use crate::render::RenderConfig;
use crate::source::{Origin, find_readme};
use crate::term::Caps;
use crate::term::env::Env;
use crate::term::tmux::TmuxInfo;
use crate::theme::Theme;

use super::diff::Screen;
use super::images::{self, ImageProvider, Sizer};
use super::keymap::Command;
use super::os::{self, ClipboardPlan, OpenPlan};
use super::state::{DocKey, Settings, State};
use super::term::{
    MAX_POLL, MIN_POLL, MOUSE_OFF, MOUSE_ON, Signals, TermEvent, Terminal, clamp_poll,
};
use super::update::{Action, Effect, LoadRequest, Nav, update};
use super::view::{Ctx, view};
use super::watch::Watcher;
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
    images: Box<dyn ImageProvider>,
    image_modes: Vec<ImageMode>,
    image_mode: usize,
    env: Env,
    tmux: Option<TmuxInfo>,
    open: OpenCommand,
    ctx: Ctx,
    cfg: RenderConfig,
    screen: Screen,
    watcher: Option<Watcher>,
    resize: Option<((u16, u16), Instant)>,
    signals: Signals,
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
        let image_modes = images::cycle(opts.images.mode);
        let sizer = Sizer {
            provider: &*images,
            doc: &doc.doc,
            enabled: opts.images.mode != ImageMode::None,
        };
        let first = first.filter(|l| l.width == size.0).unwrap_or_else(|| {
            layout::layout(
                &doc.doc,
                size.0,
                &theme,
                &caps,
                &opts,
                &*highlighter,
                &sizer,
            )
        });
        let key = key_of(&doc.source.origin);
        let mut state = State::new(doc, key, first, size, &pager, settings);
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
            image_modes,
            image_mode: 0,
            env,
            tmux,
            open: pager.open,
            ctx,
            cfg,
            screen: Screen::new(),
            watcher: None,
            resize: None,
            signals,
            panic_test,
        }
    }

    /// Run until the reader quits or a signal says so.
    pub(crate) fn run(mut self) -> io::Result<PagerExit> {
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
            if self.signals.take_continued() {
                self.resume()?;
            }
            let now = self.term.term.now();
            if let Some(exit) = self.fire_resize(now)? {
                return Ok(exit);
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
        if let Some(w) = &self.watcher {
            due = due.min(w.deadline().saturating_duration_since(now));
        }
        clamp_poll(due)
    }

    fn event(&mut self, event: TermEvent) -> io::Result<Option<PagerExit>> {
        match event {
            TermEvent::Key(key) => self.dispatch(Action::Key(key)),
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

    /// Nothing happened: time for the watcher.
    fn idle(&mut self) -> io::Result<Option<PagerExit>> {
        let now = self.term.term.now();
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
                    Vec::new()
                }
                Effect::Load(req) => self.load(req),
                Effect::ShowFile(path) => self.show_file(path),
                Effect::Open(url) => self.open_url(&url)?,
                Effect::Copy(text) => {
                    self.copy(&text)?;
                    let shown = crate::text::sanitize(&text).into_owned();
                    update(&mut self.state, Action::Message(format!("copied {shown}")))
                }
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
        self.sync_watcher();
        Ok(None)
    }

    /// Lay the current document out for the current size and settings.
    fn layout(&mut self) -> Layout {
        let (cols, rows) = self.state.size();
        self.caps.size = Some((cols, rows));
        self.opts.max_width = if self.state.wide() { 0 } else { self.max_width };
        let mode = self
            .image_modes
            .get(self.image_mode)
            .copied()
            .unwrap_or(ImageMode::Auto);
        self.opts.images.mode = mode;
        let doc = self.state.document();
        let sizer = Sizer {
            provider: &*self.images,
            doc,
            enabled: mode != ImageMode::None,
        };
        layout::layout(
            doc,
            cols,
            &self.theme,
            &self.caps,
            &self.opts,
            &*self.highlighter,
            &sizer,
        )
    }

    fn paint(&mut self) -> io::Result<()> {
        let frame = view(&self.state, &self.ctx);
        self.cfg.link_base = self.state.page.link_base;
        let bytes = self
            .screen
            .paint(&frame, &self.state.page.doc, &self.state.layout, &self.cfg);
        if !bytes.is_empty() {
            self.term.term.write(&bytes)?;
            self.term.term.flush()?;
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
        match self.loader.load(&path) {
            Ok(doc) => {
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
            Err(e) => update(&mut self.state, Action::Error(e.to_string())),
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

    /// Read the file again; `auto` (from the watcher) is quiet when nothing
    /// changed.
    fn reload(&mut self, auto: bool) -> Vec<Effect> {
        let now = self.term.term.now();
        if let Some(w) = &mut self.watcher {
            w.rebase(now);
        }
        let Some(path) = self.state.page.path().map(Path::to_path_buf) else {
            return Vec::new();
        };
        match self.loader.load(&path) {
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
        match (&self.watcher, want) {
            (Some(w), Some(p)) if w.path() == p => {}
            (_, Some(p)) => self.watcher = Some(Watcher::new(p, self.term.term.now())),
            (_, None) => self.watcher = None,
        }
    }

    fn cycle_images(&mut self) -> Vec<Effect> {
        self.image_mode = (self.image_mode + 1) % self.image_modes.len().max(1);
        let mode = self
            .image_modes
            .get(self.image_mode)
            .copied()
            .unwrap_or(ImageMode::Auto);
        update(
            &mut self.state,
            Action::Message(format!("images: {}", images::mode_name(mode))),
        )
    }

    /// Ctrl-Z: put the terminal back, stop, and set it up again on resume.
    fn suspend(&mut self) -> io::Result<()> {
        self.term.term.leave()?;
        self.term.term.suspend()?;
        let _ = self.signals.take_continued();
        self.resume()
    }

    /// Set the terminal up again after the process was stopped and
    /// continued, and redraw everything.
    fn resume(&mut self) -> io::Result<()> {
        self.term.term.enter(self.state.mouse())?;
        self.screen.invalidate();
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
