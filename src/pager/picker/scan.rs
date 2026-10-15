//! The picker's background search: the files it can offer, found on a
//! thread of their own and streamed to the event loop in batches, so
//! typing never waits for the disk.
//!
//! Sources, in order (a file found twice counts once, by its canonical
//! path):
//!
//! 1. the files opened lately (the [`recent`] store), with their frecency;
//! 2. the current document's git repository (`git ls-files`, with the top
//!    level from `git rev-parse --show-toplevel`), or without one the
//!    document's directory, [`DOC_DEPTH`] levels deep;
//! 3. the configured roots (`picker.roots`).
//!
//! Walks are breadth-first, skip hidden directories (unless
//! `picker.show_hidden`) and [`SKIPPED`] ones, and never follow a symlink
//! to a directory (no cycles). Everything stops at `picker.max_files`
//! files, after [`WALK_TIME`] of walking, and as soon as the search is
//! cancelled: every search has a number, and the thread stops when the
//! [`Scanner`]'s live number is no longer its own (a new search, the
//! picker closed, the pager gone). git runs with a deadline and is killed
//! when it passes or the search is cancelled.

use std::collections::{HashSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::fuzzy;
use super::super::recent;
use super::Candidate;
use crate::config::PickerOptions;

/// Directories never walked into.
pub(crate) const SKIPPED: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".cache",
    "build",
    "dist",
];

/// How deep the document's directory is walked without a repository.
pub(crate) const DOC_DEPTH: usize = 4;

/// The longest the walks of one search take, together.
pub(crate) const WALK_TIME: Duration = Duration::from_secs(2);

/// The longest git may take to name the repository.
const GIT_TOPLEVEL_TIME: Duration = Duration::from_secs(1);

/// The longest git may take to list the repository's files.
const GIT_LIST_TIME: Duration = Duration::from_secs(3);

/// Files sent at once (or fewer after [`BATCH_TIME`]).
const BATCH: usize = 512;

/// The longest found files wait to be sent.
const BATCH_TIME: Duration = Duration::from_millis(25);

/// What to search for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    /// The search's number (never 0).
    pub(crate) scan: u64,
    /// The current document's directory (absolute).
    pub(crate) dir: PathBuf,
    /// The current document (left out of the results).
    pub(crate) current: Option<PathBuf>,
    /// Every file, not only Markdown.
    pub(crate) all: bool,
}

/// What the search needs besides the request (from the session).
#[derive(Clone, Debug)]
pub(crate) struct Setup {
    pub(crate) options: PickerOptions,
    pub(crate) home: Option<PathBuf>,
    /// The [`recent`] store.
    pub(crate) recent: Option<PathBuf>,
    /// Ask git for the repository's files.
    pub(crate) git: bool,
}

/// Files found by search `scan`; the last batch is `done`.
#[derive(Debug)]
pub(crate) struct Batch {
    pub(crate) scan: u64,
    pub(crate) files: Vec<Candidate>,
    pub(crate) done: bool,
}

/// Runs searches on a thread; see the module docs.
#[derive(Debug)]
pub(crate) struct Scanner {
    /// The search wanted now (0: none).
    live: Arc<AtomicU64>,
    results: Option<Receiver<Batch>>,
}

impl Default for Scanner {
    fn default() -> Self {
        Scanner {
            live: Arc::new(AtomicU64::new(0)),
            results: None,
        }
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Scanner {
    /// Start search `req` (cancelling any other).
    pub(crate) fn start(&mut self, req: Request, setup: &Setup) {
        self.live.store(req.scan, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let live = Arc::clone(&self.live);
        let setup = setup.clone();
        let scan = req.scan;
        let spawned = std::thread::Builder::new()
            .name("emde-picker".into())
            .spawn(move || {
                let done = tx.clone();
                // A panic loses this search's remaining files only.
                if crate::panic::guarded(|| search(&req, &setup, live, tx)).is_none() {
                    let _ = done.send(Batch {
                        scan,
                        files: Vec::new(),
                        done: true,
                    });
                }
            });
        if spawned.is_err() {
            // No thread: nothing found, and done.
            let (tx, rx) = mpsc::channel();
            let _ = tx.send(Batch {
                scan,
                files: Vec::new(),
                done: true,
            });
            self.results = Some(rx);
            return;
        }
        self.results = Some(rx);
    }

    /// Cancel the search (its thread stops at its next check).
    pub(crate) fn stop(&mut self) {
        self.live.store(0, Ordering::Relaxed);
        self.results = None;
    }

    /// Whether a search is running.
    pub(crate) fn busy(&self) -> bool {
        self.results.is_some()
    }

    /// The batches that came in.
    pub(crate) fn take(&mut self) -> Vec<Batch> {
        let mut out = Vec::new();
        let Some(rx) = &self.results else {
            return out;
        };
        loop {
            match rx.try_recv() {
                Ok(b) => {
                    let done = b.done;
                    out.push(b);
                    if done {
                        self.results = None;
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.results = None;
                    break;
                }
            }
        }
        out
    }

    /// Wait for the search to end (at most `limit`) and take its batches.
    pub(crate) fn wait(&mut self, limit: Duration) -> Vec<Batch> {
        let end = Instant::now() + limit;
        let mut out = Vec::new();
        while let Some(rx) = &self.results {
            match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                Ok(b) => {
                    let done = b.done;
                    out.push(b);
                    if done {
                        self.results = None;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => self.results = None,
            }
        }
        out
    }
}

/// Whether `path` counts as Markdown: its extension is one of `exts`
/// (ignoring case), or it is a README without one.
pub(crate) fn is_markdown(path: &Path, exts: &[String]) -> bool {
    match path.extension().and_then(OsStr::to_str) {
        Some(ext) => exts.iter().any(|e| e.eq_ignore_ascii_case(ext)),
        None => path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|n| n.eq_ignore_ascii_case("readme")),
    }
}

/// How paths are shown and ranked: relative to the repository (or, without
/// one, the document's directory), else under `~`.
#[derive(Clone, Debug)]
pub(crate) struct Display {
    /// Paths under it are shown relative to it.
    pub(crate) base: PathBuf,
    pub(crate) repo: Option<PathBuf>,
    /// The document's directory.
    pub(crate) dir: PathBuf,
    pub(crate) home: Option<PathBuf>,
}

impl Display {
    /// The candidate for `path` (with this frecency).
    pub(crate) fn candidate(&self, path: PathBuf, frecency: u32) -> Candidate {
        let shown = match path.strip_prefix(&self.base) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => super::paths::tilde(&path, self.home.as_deref()),
        };
        let shown = crate::text::sanitize(&shown).replace(['\n', '\t'], " ");
        let base = shown.rfind('/').map_or(0, |i| i + 1);
        let near = if path.parent() == Some(self.dir.as_path()) {
            2
        } else if path.starts_with(self.repo.as_deref().unwrap_or(&self.dir)) {
            1
        } else {
            0
        };
        Candidate {
            mask: fuzzy::mask(&shown),
            base: u32::try_from(base).unwrap_or(0),
            shown: shown.into(),
            path,
            frecency,
            near,
        }
    }
}

/// Where found files go: de-duplicated, filtered, counted, batched.
struct Sink<'a> {
    req: &'a Request,
    setup: &'a Setup,
    live: Arc<AtomicU64>,
    tx: Sender<Batch>,
    display: Display,
    seen: HashSet<PathBuf>,
    batch: Vec<Candidate>,
    sent: Instant,
    found: usize,
}

/// Whether to go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Go,
    Stop,
}

impl Sink<'_> {
    fn cancelled(&self) -> bool {
        self.live.load(Ordering::Relaxed) != self.req.scan
    }

    fn full(&self) -> bool {
        self.found >= self.setup.options.max_files
    }

    /// Offer `path` (canonical as `key`).
    fn add(&mut self, path: PathBuf, key: Option<PathBuf>, frecency: u32) -> Flow {
        if self.cancelled() || self.full() {
            return Flow::Stop;
        }
        if !self.req.all && !is_markdown(&path, &self.setup.options.extensions) {
            return Flow::Go;
        }
        let key = key.unwrap_or_else(|| path.clone());
        if self.req.current.as_ref() == Some(&key) || !self.seen.insert(key) {
            return Flow::Go;
        }
        self.batch.push(self.display.candidate(path, frecency));
        self.found += 1;
        if self.batch.len() >= BATCH || self.sent.elapsed() >= BATCH_TIME {
            self.flush(false)
        } else {
            Flow::Go
        }
    }

    fn flush(&mut self, done: bool) -> Flow {
        let files = std::mem::take(&mut self.batch);
        self.sent = Instant::now();
        if files.is_empty() && !done {
            return Flow::Go;
        }
        let batch = Batch {
            scan: self.req.scan,
            files,
            done,
        };
        match self.tx.send(batch) {
            Ok(()) if !self.cancelled() => Flow::Go,
            _ => Flow::Stop,
        }
    }
}

/// `path` with symlinks resolved, or as it is.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Search as `req` says, sending batches on `tx`; see the module docs.
fn search(req: &Request, setup: &Setup, live: Arc<AtomicU64>, tx: Sender<Batch>) {
    let dir = canonical(&req.dir);
    let repo = if setup.git {
        git_toplevel(&dir, &live, req.scan).map(|r| canonical(&r))
    } else {
        None
    };
    let mut sink = Sink {
        req,
        setup,
        live,
        tx,
        display: Display {
            base: repo.clone().unwrap_or_else(|| dir.clone()),
            repo: repo.clone(),
            dir: dir.clone(),
            home: setup.home.clone(),
        },
        seen: HashSet::new(),
        batch: Vec::new(),
        sent: Instant::now(),
        found: 0,
    };
    let _ = run(&mut sink, &dir, repo.as_deref());
    sink.flush(true);
}

fn run(sink: &mut Sink<'_>, dir: &Path, repo: Option<&Path>) -> Flow {
    // 1. Recent files that still exist.
    if let Some(store) = &sink.setup.recent {
        let now = recent::now();
        for v in recent::load(store, now) {
            if fs::metadata(&v.path).is_ok_and(|m| m.is_file()) {
                let score = recent::frecency(&v, now).max(1);
                if sink.add(v.path.clone(), Some(v.path), score) == Flow::Stop {
                    return Flow::Stop;
                }
            }
        }
    }
    // 2. The repository, or the document's directory.
    let listed = match repo {
        Some(top) => git_files(sink, top),
        None => None,
    };
    let walk_until = Instant::now() + WALK_TIME;
    match listed {
        Some(Flow::Stop) => return Flow::Stop,
        Some(Flow::Go) => {}
        None => {
            if walk(sink, dir, DOC_DEPTH, walk_until) == Flow::Stop {
                return Flow::Stop;
            }
        }
    }
    // 3. The roots.
    for root in sink.setup.options.roots.clone() {
        let root = canonical(&root);
        if walk(sink, &root, usize::MAX, walk_until) == Flow::Stop {
            return Flow::Stop;
        }
    }
    Flow::Go
}

/// Walk `root` breadth-first, `depth` levels deep at most, until `until`.
fn walk(sink: &mut Sink<'_>, root: &Path, depth: usize, until: Instant) -> Flow {
    let hidden = sink.setup.options.show_hidden;
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = 0u32;
    while let Some((dir, level)) = queue.pop_front() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            visited = visited.wrapping_add(1);
            if visited % 256 == 0 && Instant::now() >= until {
                return Flow::Go;
            }
            if sink.cancelled() || sink.full() {
                return Flow::Stop;
            }
            let Ok(entry) = entry else {
                continue;
            };
            let name = entry.file_name();
            let dot = name.as_encoded_bytes().starts_with(b".");
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                let skipped = SKIPPED.iter().any(|s| OsStr::new(s) == name);
                if !skipped && (hidden || !dot) && level + 1 < depth {
                    queue.push_back((path, level + 1));
                }
            } else if kind.is_file() {
                if (hidden || !dot) && sink.add(path, None, 0) == Flow::Stop {
                    return Flow::Stop;
                }
            } else if kind.is_symlink()
                && (hidden || !dot)
                && fs::metadata(&path).is_ok_and(|m| m.is_file())
            {
                // A link to a file (never to a directory: no cycles).
                let key = canonical(&path);
                if sink.add(path, Some(key), 0) == Flow::Stop {
                    return Flow::Stop;
                }
            }
        }
    }
    Flow::Go
}

/// Kills a child process when its deadline passes or search `scan` is
/// cancelled, until told the child is done.
struct Watchdog {
    done: Arc<AtomicBool>,
}

impl Watchdog {
    fn new(child: Arc<Mutex<Child>>, until: Instant, live: Arc<AtomicU64>, scan: u64) -> Watchdog {
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        let watch = move || {
            while !flag.load(Ordering::Relaxed) {
                if Instant::now() >= until || live.load(Ordering::Relaxed) != scan {
                    if let Ok(mut c) = child.lock() {
                        let _ = c.kill();
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        if std::thread::Builder::new()
            .name("emde-picker-git".into())
            .spawn(watch)
            .is_err()
        {
            done.store(true, Ordering::Relaxed);
        }
        Watchdog { done }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
    }
}

/// Run git with `args` in `dir` for search `scan`, handing each NUL- (or
/// newline-) ended record of its output to `record` until it says stop;
/// killed after `limit` or when the search is cancelled. `None` when git
/// cannot run, or fails without output.
fn git(
    dir: &Path,
    args: &[&str],
    limit: Duration,
    (live, scan): (&Arc<AtomicU64>, u64),
    nul: bool,
    record: &mut dyn FnMut(&[u8]) -> Flow,
) -> Option<Flow> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let child = Arc::new(Mutex::new(child));
    let watchdog = Watchdog::new(
        Arc::clone(&child),
        Instant::now() + limit,
        Arc::clone(live),
        scan,
    );
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    let mut flow = Flow::Go;
    let mut any = false;
    let end = if nul { b'\0' } else { b'\n' };
    loop {
        buf.clear();
        match reader.read_until(end, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if buf.last() == Some(&end) {
            buf.pop();
        }
        if buf.is_empty() {
            continue;
        }
        any = true;
        if record(&buf) == Flow::Stop {
            flow = Flow::Stop;
            break;
        }
    }
    drop(reader);
    let status = child.lock().ok().and_then(|mut c| {
        if flow == Flow::Stop {
            let _ = c.kill();
        }
        c.wait().ok()
    });
    drop(watchdog);
    match status {
        _ if any => Some(flow),
        Some(s) if s.success() => Some(flow),
        _ => None,
    }
}

/// The top level of the git repository `dir` is in.
fn git_toplevel(dir: &Path, live: &Arc<AtomicU64>, scan: u64) -> Option<PathBuf> {
    let mut top = None;
    git(
        dir,
        &["rev-parse", "--show-toplevel"],
        GIT_TOPLEVEL_TIME,
        (live, scan),
        false,
        &mut |line| {
            top = std::str::from_utf8(line).ok().map(PathBuf::from);
            Flow::Stop
        },
    )?;
    top.filter(|t| t.is_absolute())
}

/// The repository's files (tracked, and untracked ones not ignored).
/// `None` when git cannot list them.
fn git_files(sink: &mut Sink<'_>, top: &Path) -> Option<Flow> {
    let live = Arc::clone(&sink.live);
    let scan = sink.req.scan;
    git(
        top,
        &["ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        GIT_LIST_TIME,
        (&live, scan),
        true,
        &mut |rel| match path_of(rel) {
            Some(rel) => sink.add(top.join(rel), None, 0),
            None => Flow::Go,
        },
    )
}

/// A relative path git printed.
fn path_of(bytes: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(PathBuf::from(OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes).ok().map(PathBuf::from)
    }
}
