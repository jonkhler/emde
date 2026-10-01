//! The built-in pager (plan §7).
//!
//! [`run`] takes a [`PagerSession`] — the first document, a loader for the
//! documents its links lead to, the render context and the pager options —
//! and shows it on the terminal until the reader quits.
//!
//! # Architecture
//!
//! * [`update()`] is pure: `update(&mut State, Action) -> Vec<Effect>`. Keys
//!   go through one table ([`keymap::BINDINGS`], which also makes the help
//!   and the documentation) to commands that move the view, search, focus
//!   and follow links, open overlays.
//! * [`view()`] is pure: `view(&State, &Ctx) -> Frame`, one row per screen
//!   row, each with a 64-bit hash of what it shows.
//! * The shell (the event loop) owns the [`Terminal`], reads documents,
//!   lays them out, carries out the [`Effect`]s, and paints frames with
//!   [`Screen`]: only rows whose hash changed, a DECSTBM scroll when the
//!   document just moved, everything in one synchronized write.
//!
//! # The terminal
//!
//! Setup and teardown, signals and the panic hook are described in
//! [`term`]. The terminal is put back on every way out: the shell's drop
//! guard, SIGTERM/SIGHUP/SIGINT (flags checked at least every
//! [`term::MAX_POLL`]), and unguarded panics; Ctrl-Z suspends with the
//! terminal restored and redraws on resume.
//!
//! # Paging decision
//!
//! Whether to page at all is the caller's decision: with `pager.enabled =
//! "auto"` the pager is for a terminal and a document taller than it, like
//! `less -F` ([`fits_on_screen`]).

mod diff;
mod images;
pub mod keymap;
mod links;
mod os;
mod search;
mod shell;
mod state;
pub mod term;
mod toc;
mod update;
mod view;
mod watch;

use std::io;
use std::path::Path;
use std::sync::Arc;

pub use diff::{SYNC_OFF, SYNC_ON, Screen};
pub use images::ImageProvider;
pub use search::Match;
pub use state::{DocKey, Place, Settings, State};
pub use term::{Signals, Terminal};
pub use update::{Action, Effect, LoadRequest, Nav, update};
pub use view::{Body, Ctx, Frame, Row, Segment, view};

use crate::config::PagerOptions;
use crate::highlight::{Highlighter, PlainHighlighter};
use crate::ir::Document;
use crate::layout::{Layout, NoImages};
use crate::options::RenderOptions;
use crate::parse::{ParseOptions, parse_source};
use crate::source::{Input, Source, SourceError};
use crate::term::Caps;
use crate::term::env::Env;
use crate::term::probe::LateReplyFilter;
use crate::term::tmux::TmuxInfo;
use crate::theme::Theme;

/// A document for the pager: its text and the parsed result.
#[derive(Clone, Debug)]
pub struct PagerDoc {
    /// The text; its origin is the file that is reloaded and watched
    /// (standard input is neither).
    pub source: Source,
    /// The parsed document.
    pub doc: Document,
}

impl PagerDoc {
    /// A document from its parts.
    pub fn new(source: Source, doc: Document) -> PagerDoc {
        PagerDoc { source, doc }
    }

    /// Parse `source`.
    pub fn parse(source: Source, opts: &ParseOptions) -> PagerDoc {
        let doc = parse_source(&source, opts);
        PagerDoc { source, doc }
    }

    /// Read and parse the file at `path` (a directory is read through its
    /// README).
    pub fn load(path: &Path, opts: &ParseOptions) -> Result<PagerDoc, SourceError> {
        let source = Source::load(&Input::Path(path.to_path_buf()))?;
        Ok(PagerDoc::parse(source, opts))
    }
}

/// Reads the documents links lead to, and reloads the current one.
pub trait DocLoader {
    /// Read and parse the Markdown file at `path` (a directory is read
    /// through its README).
    fn load(&self, path: &Path) -> Result<PagerDoc, SourceError>;
}

/// A [`DocLoader`] for files on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileLoader {
    pub parse: ParseOptions,
}

impl FileLoader {
    /// A loader that parses with `parse`.
    pub fn new(parse: ParseOptions) -> FileLoader {
        FileLoader { parse }
    }
}

impl DocLoader for FileLoader {
    fn load(&self, path: &Path) -> Result<PagerDoc, SourceError> {
        PagerDoc::load(path, &self.parse)
    }
}

/// Everything the pager needs. [`PagerSession::new`] fills in defaults;
/// set the fields to change them.
pub struct PagerSession {
    /// The first document.
    pub doc: PagerDoc,
    /// Reads linked documents and reloads.
    pub loader: Box<dyn DocLoader>,
    pub theme: Theme,
    /// The terminal's capabilities (`size` is kept up to date by the
    /// pager).
    pub caps: Caps,
    pub opts: RenderOptions,
    pub highlighter: Arc<dyn Highlighter>,
    /// Sizes figures.
    pub images: Box<dyn ImageProvider>,
    pub pager: PagerOptions,
    /// An anchor to show first (`FILE#anchor`, `--anchor`).
    pub anchor: Option<String>,
    /// Open the outline right away (`--toc`).
    pub open_toc: bool,
    /// The environment (SSH detection for opening links, tmux).
    pub env: Env,
    /// The tmux query result, if it ran (the clipboard path inside tmux).
    pub tmux: Option<TmuxInfo>,
    /// Swallows probe replies that arrive late (after a probe timeout).
    pub late_replies: LateReplyFilter,
    /// A layout of `doc` made with these settings for the terminal's width
    /// (e.g. for the paging decision); used for the first frame if the
    /// width still matches.
    pub layout: Option<Layout>,
}

impl PagerSession {
    /// A session for `doc` with the given render context and defaults for
    /// the rest: files loaded with parse options from `opts`, no syntax
    /// highlighting, no image sizes, default pager options, the process
    /// environment.
    pub fn new(doc: PagerDoc, theme: Theme, caps: Caps, opts: RenderOptions) -> PagerSession {
        PagerSession {
            loader: Box::new(FileLoader::new(ParseOptions::from(&opts))),
            doc,
            theme,
            caps,
            opts,
            highlighter: Arc::new(PlainHighlighter),
            images: Box::new(NoImages),
            pager: PagerOptions::default(),
            anchor: None,
            open_toc: false,
            env: Env::from_process(),
            tmux: None,
            late_replies: LateReplyFilter::inactive(),
            layout: None,
        }
    }
}

/// How the pager ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagerExit {
    /// The reader quit.
    Quit,
    /// A signal (SIGTERM, SIGHUP, SIGINT) ended it.
    Signal(i32),
}

impl PagerExit {
    /// The conventional exit status: 0, or 128 plus the signal number.
    pub fn code(self) -> u8 {
        match self {
            PagerExit::Quit => 0,
            PagerExit::Signal(sig) => u8::try_from(128 + sig.clamp(0, 127)).unwrap_or(255),
        }
    }
}

/// Show the session on the terminal until the reader quits.
///
/// Installs the panic hook that restores the terminal and the signal flags,
/// then runs on a [`term::CrosstermTerminal`]. With `EMDE_TEST_PANIC=1` in
/// the environment it panics right after the first frame (to test the
/// restore).
pub fn run(session: PagerSession) -> io::Result<PagerExit> {
    term::install_panic_hook();
    let signals = Signals::register()?;
    let mut terminal = term::CrosstermTerminal::new(session.late_replies.clone());
    let panic_test = std::env::var_os("EMDE_TEST_PANIC").is_some_and(|v| v == "1");
    shell::Shell::new(&mut terminal, session, signals, panic_test).run()
}

/// [`run`] on any terminal, with the given signal flags (tests use a
/// [`term::FakeTerminal`] and [`Signals::new`]).
pub fn run_on<T: Terminal>(
    terminal: &mut T,
    session: PagerSession,
    signals: &Signals,
) -> io::Result<PagerExit> {
    shell::Shell::new(terminal, session, signals.clone(), false).run()
}

/// Whether `layout` fits on a terminal `rows` rows high with a line to
/// spare for the shell prompt after it: then the pager is not needed
/// (`pager.enabled = "auto"`, like `less -F`).
pub fn fits_on_screen(layout: &Layout, rows: u16) -> bool {
    layout.len() < usize::from(rows)
}

#[cfg(test)]
mod tests;
