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

/// What a search found and the event loop has not taken yet.
pub(crate) type Queue = Arc<Mutex<Vec<Batch>>>;

/// Runs searches on a thread; see the module docs.
#[derive(Debug)]
pub(crate) struct Scanner {
    /// The search wanted now (0: none).
    live: Arc<AtomicU64>,
    /// Its results (`None`: no search running).
    results: Option<Queue>,
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
        let queue: Queue = Arc::default();
        let live = Arc::clone(&self.live);
        let setup = setup.clone();
        let scan = req.scan;
        let out = Arc::clone(&queue);
        let spawned = std::thread::Builder::new()
            .name("emde-picker".into())
            .spawn(move || {
                // A panic loses this search's remaining files only.
                let found = crate::panic::guarded(|| search(&req, &setup, live, &out));
                if found.is_none() {
                    push(&out, scan, Vec::new(), true);
                }
            });
        if spawned.is_err() {
            // No thread: nothing found, and done.
            push(&queue, scan, Vec::new(), true);
        }
        self.results = Some(queue);
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
        let out = match &self.results {
            Some(q) => q
                .lock()
                .map(|mut v| std::mem::take(&mut *v))
                .unwrap_or_default(),
            None => return Vec::new(),
        };
        if out.last().is_some_and(|b| b.done) {
            self.results = None;
        }
        out
    }

    /// Wait for the search to end (at most `limit`) and take its batches.
    pub(crate) fn wait(&mut self, limit: Duration) -> Vec<Batch> {
        let end = Instant::now() + limit;
        let mut out = self.take();
        while self.busy() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(1));
            out.extend(self.take());
        }
        out
    }
}

/// Add a batch to `queue`.
fn push(queue: &Queue, scan: u64, files: Vec<Candidate>, done: bool) {
    if let Ok(mut v) = queue.lock() {
        v.push(Batch { scan, files, done });
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
    out: &'a Queue,
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
        push(self.out, self.req.scan, files, done);
        if self.cancelled() {
            Flow::Stop
        } else {
            Flow::Go
        }
    }
}

/// `path` with symlinks resolved, or as it is.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Search as `req` says, adding batches to `out`; see the module docs.
fn search(req: &Request, setup: &Setup, live: Arc<AtomicU64>, out: &Queue) {
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
        out,
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
            if visited.is_multiple_of(256) && Instant::now() >= until {
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
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory with these files (a trailing `/` makes a
    /// directory).
    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emde-scan-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for f in files {
            let p = dir.join(f);
            if f.ends_with('/') {
                fs::create_dir_all(&p).unwrap();
            } else {
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, "# x\n").unwrap();
            }
        }
        canonical(&dir)
    }

    fn setup() -> Setup {
        Setup {
            options: PickerOptions::default(),
            home: None,
            recent: None,
            git: false,
        }
    }

    /// What a search of `dir` finds: the shown paths, in the order found.
    fn search_in(dir: &Path, setup: &Setup, all: bool, current: Option<PathBuf>) -> Vec<Candidate> {
        let mut scanner = Scanner::default();
        let req = Request {
            scan: 1,
            dir: dir.to_path_buf(),
            current,
            all,
        };
        scanner.start(req, setup);
        let batches = scanner.wait(Duration::from_secs(20));
        assert!(!scanner.busy(), "the search ended");
        assert!(batches.last().is_some_and(|b| b.done));
        batches.into_iter().flat_map(|b| b.files).collect()
    }

    fn shown(found: &[Candidate]) -> Vec<&str> {
        let mut v: Vec<&str> = found.iter().map(|c| &*c.shown).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn markdown_is_recognised() {
        let exts = PickerOptions::default().extensions;
        for yes in [
            "a.md",
            "b.MD",
            "c.markdown",
            "d.mdx",
            "e.mkd",
            "README",
            "x/readme",
        ] {
            assert!(is_markdown(Path::new(yes), &exts), "{yes}");
        }
        for no in ["a.txt", "README.txt", "md", "a.md.bak", "Makefile"] {
            assert!(!is_markdown(Path::new(no), &exts), "{no}");
        }
    }

    #[test]
    fn walks_skip_heavy_and_hidden_directories() {
        let dir = tree(
            "walk",
            &[
                "top.md",
                "README",
                "notes.txt",
                "a/b/c/deep.md",
                "a/b/c/d/too-deep.md",
                "node_modules/x.md",
                "target/y.md",
                ".hidden/z.md",
                ".dot.md",
                "build/w.md",
                "docs/guide.md",
            ],
        );
        let found = search_in(&dir, &setup(), false, Some(dir.join("top.md")));
        assert_eq!(
            shown(&found),
            ["README", "a/b/c/deep.md", "docs/guide.md"],
            "the current document is left out"
        );
        let near: Vec<u8> = {
            let mut v: Vec<_> = found.iter().map(|c| (&*c.shown, c.near)).collect();
            v.sort_unstable();
            v.into_iter().map(|(_, n)| n).collect()
        };
        assert_eq!(near, [2, 1, 1], "beside the document, below it");
        // Every file, hidden ones too.
        let mut s = setup();
        s.options.show_hidden = true;
        let found = search_in(&dir, &s, true, None);
        assert!(shown(&found).contains(&"notes.txt"));
        assert!(shown(&found).contains(&".hidden/z.md"));
        assert!(!shown(&found).iter().any(|p| p.starts_with("node_modules")));
        // At most max_files.
        let mut s = setup();
        s.options.max_files = 2;
        assert_eq!(search_in(&dir, &s, true, None).len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_not_followed() {
        let dir = tree("links", &["a/one.md", "b/two.md"]);
        std::os::unix::fs::symlink(&dir, dir.join("a/loop")).unwrap();
        std::os::unix::fs::symlink(dir.join("b"), dir.join("b/self")).unwrap();
        std::os::unix::fs::symlink(dir.join("a/one.md"), dir.join("link.md")).unwrap();
        let mut s = setup();
        s.options.roots = vec![dir.clone(), dir.join("a")];
        let found = search_in(&dir, &s, false, None);
        // `link.md` is `a/one.md`: found once, whichever came first.
        assert_eq!(found.len(), 2, "{:?}", shown(&found));
        assert!(shown(&found).contains(&"b/two.md"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recent_files_come_first_with_their_frecency() {
        let dir = tree("recent", &["here.md", "elsewhere/far.md"]);
        let store = dir.join("state/recent");
        recent::record(&store, &dir.join("elsewhere/far.md"));
        recent::record(&store, &dir.join("gone.md"));
        let mut s = setup();
        s.recent = Some(store);
        s.home = Some(dir.clone());
        let found = search_in(&dir.join("elsewhere"), &s, false, None);
        assert_eq!(&*found[0].shown, "far.md");
        assert!(found[0].frecency > 0);
        assert!(
            found.iter().all(|c| !c.shown.contains("gone")),
            "files that are gone"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_repositories_are_listed_by_git() {
        let ok = Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return; // no git here
        }
        let dir = tree(
            "git",
            &[
                "docs/guide.md",
                "README.md",
                "ignored/x.md",
                "untracked.md",
                "code.rs",
            ],
        );
        fs::write(dir.join(".gitignore"), "ignored/\n").unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .unwrap()
        };
        if !git(&["init", "-q"]).status.success() {
            return;
        }
        git(&["add", "docs/guide.md", "README.md", ".gitignore", "code.rs"]);
        let mut s = setup();
        s.git = true;
        let found = search_in(&dir.join("docs"), &s, false, None);
        assert_eq!(
            shown(&found),
            ["README.md", "docs/guide.md", "untracked.md"],
            "relative to the repository, ignored files left out"
        );
        let guide = found.iter().find(|c| &*c.shown == "docs/guide.md").unwrap();
        assert_eq!(guide.base, 5);
        assert_eq!(guide.near, 2);
        // Outside a repository: the directory is walked instead.
        let plain = tree("no-git", &["a.md"]);
        assert_eq!(shown(&search_in(&plain, &s, false, None)), ["a.md"]);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&plain);
    }

    #[test]
    fn a_stopped_search_ends() {
        let dir = tree("stop", &["a.md"]);
        let mut scanner = Scanner::default();
        let req = Request {
            scan: 3,
            dir: dir.clone(),
            current: None,
            all: false,
        };
        scanner.start(req, &setup());
        scanner.stop();
        assert!(!scanner.busy());
        assert!(scanner.take().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
