//! The file picker (`O`) and `:e PATH`.
//!
//! The picker is an overlay with a query line, a result list and, on wide
//! terminals, a preview of the highlighted file's headings. What is typed
//! picks one of two modes:
//!
//! * **path mode** when the query looks like a path ([`paths::is_path`]):
//!   the entries of the directory it names, fuzzily filtered by the name
//!   typed after the last `/`, directories first; Tab completes, Enter on
//!   a directory goes into it;
//! * **fuzzy mode** otherwise: the files a background search finds
//!   ([`scan`]: recent files, the repository or the document's
//!   directory, the configured roots), ranked by the [`fuzzy`] matcher,
//!   ties broken by frecency, then nearness to the current document, then
//!   the shorter path. Markdown only, unless Ctrl-T asks for every file.
//!
//! Like the rest of [`super::update()`], everything here is pure: the
//! shell lists directories, reads previews and runs the search when asked
//! ([`Job`]) and answers with [`Event`]s.

pub(crate) mod paths;
pub(crate) mod scan;

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};

use super::fuzzy::{Pattern, Scratch};
use super::state::{Mode, State};
use super::toc::BoxGeom;
use super::update::Effect;
use paths::{Completion, Entry};

/// The most results kept (and ranked in full).
pub(crate) const MAX_HITS: usize = 200;

/// The terminal width from which the preview pane is shown.
pub(crate) const PREVIEW_COLS: u16 = 100;

/// The most bytes of a file the preview reads.
pub(crate) const PREVIEW_BYTES: u64 = 64 * 1024;

/// The most entries listed of one directory.
pub(crate) const MAX_ENTRIES: usize = 20_000;

/// A file the picker can open (fuzzy mode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) path: PathBuf,
    /// The path as shown and matched: relative to the repository (or the
    /// document's directory), else under `~`, else absolute.
    pub(crate) shown: Box<str>,
    /// Where the base name starts in `shown` (bytes).
    pub(crate) base: u32,
    /// [`super::fuzzy::mask`] of `shown`.
    pub(crate) mask: u64,
    /// How often and how lately it was opened (0: not lately).
    pub(crate) frecency: u32,
    /// 2 in the document's directory, 1 in its repository, else 0.
    pub(crate) near: u8,
}

/// A result: a candidate (fuzzy mode) or an entry (path mode), its score
/// and the byte offsets of the matched characters in its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) index: usize,
    pub(crate) score: i32,
    pub(crate) positions: Vec<u32>,
}

/// The directory path mode lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listing {
    pub(crate) dir: PathBuf,
    /// Its entries (`None` until the shell listed them).
    pub(crate) entries: Option<Vec<Entry>>,
}

/// What the preview pane shows for a file: its headings (level, title),
/// or for a file that is not Markdown its first lines (level 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Preview {
    pub(crate) path: PathBuf,
    pub(crate) lines: Vec<(u8, String)>,
    /// Why there is nothing to show.
    pub(crate) note: Option<String>,
}

/// The picker's state (in [`Mode::Picker`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picker {
    pub(crate) query: String,
    /// Every file, not only Markdown (Ctrl-T).
    pub(crate) all: bool,
    /// The fuzzy search's number (0: none running or done for this
    /// query's mode).
    pub(crate) scan: u64,
    /// The search is still going.
    pub(crate) scanning: bool,
    /// What the search found so far.
    pub(crate) candidates: Vec<Candidate>,
    /// How many candidates match (more than are kept as hits).
    pub(crate) matched: usize,
    /// The query the candidates were last ranked for, and the indexes of
    /// those that matched it (a longer query only looks at those).
    pub(crate) ranked: Option<String>,
    pub(crate) matching: Vec<u32>,
    /// Path mode's directory.
    pub(crate) listing: Option<Listing>,
    /// Why path mode lists nothing.
    pub(crate) problem: Option<String>,
    /// The results, best first.
    pub(crate) hits: Vec<Hit>,
    /// Index into `hits`.
    pub(crate) selected: usize,
    /// First result shown.
    pub(crate) scroll: usize,
    /// The file whose preview was asked for last.
    pub(crate) preview_for: Option<PathBuf>,
    pub(crate) preview: Option<Preview>,
    /// The current document's directory (absolute).
    pub(crate) dir: PathBuf,
}

impl Picker {
    /// Whether the query is a path.
    pub(crate) fn path_mode(&self) -> bool {
        paths::is_path(&self.query)
    }

    /// The text of hit `h` (its path, or its entry's name) and where its
    /// base name starts, and whether it is a directory.
    pub(crate) fn text(&self, h: &Hit) -> Option<(&str, usize, bool)> {
        if self.path_mode() {
            let e = self.entries()?.get(h.index)?;
            Some((&e.name, 0, e.dir))
        } else {
            let c = self.candidates.get(h.index)?;
            Some((&c.shown, c.base as usize, false))
        }
    }

    /// Path mode's entries, once listed.
    pub(crate) fn entries(&self) -> Option<&[Entry]> {
        self.listing.as_ref()?.entries.as_deref()
    }

    /// The file or directory of hit `h`, and whether it is a directory.
    pub(crate) fn target(&self, h: &Hit) -> Option<(PathBuf, bool)> {
        if self.path_mode() {
            let e = self.entries()?.get(h.index)?;
            Some((e.path.clone(), e.dir))
        } else {
            let c = self.candidates.get(h.index)?;
            Some((c.path.clone(), false))
        }
    }

    /// The selected hit.
    pub(crate) fn current(&self) -> Option<&Hit> {
        self.hits.get(self.selected)
    }
}

/// Something the shell found for the picker (or the `:` prompt).
#[derive(Clone, Debug)]
pub struct PickerEvent(pub(crate) Event);

#[derive(Clone, Debug)]
pub(crate) enum Event {
    /// Files search `scan` found (`done`: the last of them).
    Found {
        scan: u64,
        files: Vec<Candidate>,
        done: bool,
    },
    /// The entries of `dir` (or why it could not be listed).
    Listed {
        dir: PathBuf,
        entries: Result<Vec<Entry>, String>,
    },
    /// A preview, for [`Job::Preview`].
    Previewed(Preview),
    /// A directory with no document to show: list it in the picker.
    Browse(PathBuf),
}

/// Work for the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerJob(pub(crate) Job);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Job {
    /// Start a search (cancelling any other).
    Scan(scan::Request),
    /// Cancel the search.
    Stop,
    /// List a directory (answered with [`Event::Listed`]).
    List(PathBuf),
    /// Read a file's preview (answered with [`Event::Previewed`]).
    Preview(PathBuf),
}

fn job(j: Job) -> Effect {
    Effect::Picker(PickerJob(j))
}

/// The current document's directory, absolute (the working directory for
/// standard input).
pub(crate) fn doc_dir(state: &State) -> PathBuf {
    let cwd = &state.settings.cwd;
    match &state.page.doc.base_dir {
        Some(b) => cwd.join(b).components().collect(),
        None => cwd.clone(),
    }
}

/// `O`: open the picker on `query`.
pub(crate) fn open(state: &mut State, query: String) -> Vec<Effect> {
    let mut out = stop_scan(state);
    state.mode = Mode::Picker(Box::new(Picker {
        query,
        all: false,
        scan: 0,
        scanning: false,
        candidates: Vec::new(),
        matched: 0,
        ranked: None,
        matching: Vec::new(),
        listing: None,
        problem: None,
        hits: Vec::new(),
        selected: 0,
        scroll: 0,
        preview_for: None,
        preview: None,
        dir: doc_dir(state),
    }));
    out.extend(refresh(state));
    out
}

/// A directory to list in the picker (`:e DIR` without a document in it).
pub(crate) fn browse(state: &mut State, dir: &Path) -> Vec<Effect> {
    let mut query = paths::tilde(dir, state.settings.home.as_deref());
    if !query.ends_with('/') {
        query.push('/');
    }
    open(state, query)
}

/// The picker, if it is open.
fn picker(state: &mut State) -> Option<&mut Picker> {
    match &mut state.mode {
        Mode::Picker(p) => Some(p),
        _ => None,
    }
}

/// Stop a running search (when the picker closes).
fn stop_scan(state: &mut State) -> Vec<Effect> {
    match picker(state) {
        Some(p) if p.scanning => {
            p.scanning = false;
            p.scan = 0;
            vec![job(Job::Stop)]
        }
        _ => Vec::new(),
    }
}

/// Close the picker.
pub(crate) fn close(state: &mut State) -> Vec<Effect> {
    let out = stop_scan(state);
    state.mode = Mode::Normal;
    out
}

/// The query changed: switch modes as it says, list or rank again.
fn refresh(state: &mut State) -> Vec<Effect> {
    let home = state.settings.home.clone();
    let hidden = state.settings.picker.show_hidden;
    let current = state.page.path().map(|p| p.to_path_buf());
    let next_scan = state.scans.wrapping_add(1).max(1);
    let mut out = Vec::new();
    let mut started = false;
    let Some(p) = picker(state) else {
        return out;
    };
    p.selected = 0;
    p.scroll = 0;
    p.problem = None;
    if p.path_mode() {
        if p.scanning {
            // Leaving fuzzy mode cancels its search.
            out.push(job(Job::Stop));
        }
        p.scanning = false;
        p.scan = 0;
        p.candidates.clear();
        p.ranked = None;
        p.matching.clear();
        let (part, _) = paths::split(&p.query);
        match paths::resolve(part, &p.dir, home.as_deref()) {
            Ok(dir) => {
                if p.listing.as_ref().is_none_or(|l| l.dir != dir) {
                    p.listing = Some(Listing {
                        dir: dir.clone(),
                        entries: None,
                    });
                    out.push(job(Job::List(dir)));
                }
            }
            Err(why) => {
                p.listing = None;
                p.problem = Some(why.message().to_owned());
            }
        }
        filter_entries(p, hidden);
    } else {
        p.listing = None;
        if p.scan == 0 {
            started = true;
            p.candidates.clear();
            p.ranked = None;
            p.matching.clear();
            p.scan = next_scan;
            p.scanning = true;
            out.push(job(Job::Scan(scan::Request {
                scan: next_scan,
                dir: p.dir.clone(),
                current: current.map(|c| std::fs::canonicalize(&c).unwrap_or(c)),
                all: p.all,
            })));
        }
        rerank(p);
    }
    if started {
        state.scans = next_scan;
    }
    out.extend(want_preview(state));
    out
}

/// Path mode: the listed entries matching the name typed.
fn filter_entries(p: &mut Picker, hidden: bool) {
    let (_, leaf) = paths::split(&p.query);
    let leaf = leaf.to_owned();
    let Some(entries) = p.entries() else {
        p.hits.clear();
        return;
    };
    let pattern = Pattern::new(&leaf);
    let mut scratch = Scratch::default();
    let hits: Vec<Hit> = paths::filter(entries, &leaf, hidden)
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|(index, score)| {
            let positions = if pattern.is_empty() {
                Vec::new()
            } else {
                entries
                    .get(index)
                    .and_then(|e| pattern.matches(&e.name, 0, &mut scratch))
                    .map(|m| m.positions)
                    .unwrap_or_default()
            };
            Hit {
                index,
                score,
                positions,
            }
        })
        .collect();
    p.matched = hits.len();
    p.hits = hits;
}

/// How a candidate ranks: higher is better.
/// Ties go to the more frecent, the nearer, the shorter, then the first
/// in alphabetical order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Rank<'a> {
    score: i32,
    frecency: u32,
    near: u8,
    short: Reverse<usize>,
    name: Reverse<&'a str>,
    first: Reverse<usize>,
}

fn rank_of(c: &Candidate, index: usize, score: i32) -> Rank<'_> {
    Rank {
        score,
        frecency: c.frecency,
        near: c.near,
        short: Reverse(c.shown.len()),
        name: Reverse(&c.shown),
        first: Reverse(index),
    }
}

/// The best [`MAX_HITS`] of what is pushed (a bounded min-heap).
struct Top<T: Ord>(BinaryHeap<Reverse<T>>);

impl<T: Ord> Top<T> {
    fn new() -> Top<T> {
        Top(BinaryHeap::new())
    }

    fn push(&mut self, item: T) {
        if self.0.len() >= MAX_HITS {
            // Full: only better ones get in.
            match self.0.peek() {
                Some(Reverse(worst)) if item > *worst => {
                    self.0.pop();
                }
                _ => return,
            }
        }
        self.0.push(Reverse(item));
    }

    /// Best first.
    fn into_sorted(self) -> impl Iterator<Item = T> {
        self.0.into_sorted_vec().into_iter().map(|Reverse(t)| t)
    }
}

/// Candidates ranked on one thread at a time.
const CHUNK: usize = 8192;

/// The best ranks among candidates `fresh` (indexes into `cands`) that
/// match `pattern`, and the indexes of all that match.
fn score_chunk<'a>(
    pattern: &Pattern,
    cands: &'a [Candidate],
    fresh: &[u32],
) -> (Vec<Rank<'a>>, Vec<u32>) {
    let mut scratch = Scratch::default();
    let mut top = Top::new();
    let mut matched = Vec::new();
    for &i in fresh {
        let Some(c) = cands.get(i as usize) else {
            continue;
        };
        let score = if pattern.is_empty() {
            Some(0)
        } else {
            pattern.score(&c.shown, c.mask, c.base as usize, &mut scratch)
        };
        if let Some(score) = score {
            matched.push(i);
            top.push(rank_of(c, i as usize, score));
        }
    }
    (top.into_sorted().collect(), matched)
}

/// The best [`MAX_HITS`] of `kept` (hits already ranked) and the
/// candidates `fresh` (indexes into `cands`, ascending) that match
/// `pattern`, best first, with the matched positions; and the indexes of
/// `fresh` that match. Big sets are ranked on several threads.
fn best_of(
    pattern: &Pattern,
    cands: &[Candidate],
    kept: Vec<Hit>,
    fresh: &[u32],
) -> (Vec<Hit>, Vec<u32>) {
    let chunks: Vec<&[u32]> = fresh.chunks(CHUNK).collect();
    let scored: Vec<(Vec<Rank<'_>>, Vec<u32>)> = if chunks.len() > 1 {
        crate::parallel::map(&chunks, usize::MAX, |c| score_chunk(pattern, cands, c))
            .into_iter()
            .map(Option::unwrap_or_default)
            .collect()
    } else {
        chunks
            .iter()
            .map(|c| score_chunk(pattern, cands, c))
            .collect()
    };
    let mut top = Top::new();
    for h in kept {
        if let Some(c) = cands.get(h.index) {
            top.push((rank_of(c, h.index, h.score), Some(h.positions)));
        }
    }
    let mut matched = Vec::new();
    for (ranks, m) in scored {
        for r in ranks {
            top.push((r, None));
        }
        matched.extend(m);
    }
    let mut scratch = Scratch::default();
    let hits = top
        .into_sorted()
        .map(|(rank, positions)| {
            let index = rank.first.0;
            let positions = positions.unwrap_or_else(|| {
                if pattern.is_empty() {
                    return Vec::new();
                }
                cands
                    .get(index)
                    .and_then(|c| pattern.matches(&c.shown, c.base as usize, &mut scratch))
                    .map(|m| m.positions)
                    .unwrap_or_default()
            });
            Hit {
                index,
                score: rank.score,
                positions,
            }
        })
        .collect();
    (hits, matched)
}

/// Fuzzy mode: rank the candidates for the query. When it only adds to
/// the query ranked last, only the candidates that matched that can match
/// it.
fn rerank(p: &mut Picker) {
    let pattern = Pattern::new(&p.query);
    let narrower = p
        .ranked
        .as_ref()
        .is_some_and(|q| p.query.starts_with(q.as_str()));
    let all: Vec<u32>;
    let fresh: &[u32] = if narrower {
        &p.matching
    } else {
        all = (0..u32::try_from(p.candidates.len()).unwrap_or(u32::MAX)).collect();
        &all
    };
    let (hits, matching) = best_of(&pattern, &p.candidates, Vec::new(), fresh);
    p.hits = hits;
    p.matched = matching.len();
    p.matching = matching;
    p.ranked = Some(p.query.clone());
}

/// More candidates came in: rank only them against the hits kept.
fn absorb(p: &mut Picker, files: Vec<Candidate>) {
    let from = u32::try_from(p.candidates.len()).unwrap_or(u32::MAX);
    p.candidates.extend(files);
    let to = u32::try_from(p.candidates.len()).unwrap_or(u32::MAX);
    let chosen = (p.selected > 0)
        .then(|| p.current().map(|h| h.index))
        .flatten();
    let pattern = Pattern::new(&p.query);
    let kept = std::mem::take(&mut p.hits);
    let fresh: Vec<u32> = (from..to).collect();
    let (hits, matching) = best_of(&pattern, &p.candidates, kept, &fresh);
    p.hits = hits;
    p.matched += matching.len();
    if p.ranked.as_deref() == Some(p.query.as_str()) {
        p.matching.extend(matching);
    }
    // The reader's choice stays chosen as results come in.
    p.selected = chosen
        .and_then(|c| p.hits.iter().position(|h| h.index == c))
        .unwrap_or(0);
}

/// Something the shell found.
pub(crate) fn event(state: &mut State, e: Event) -> Vec<Effect> {
    let hidden = state.settings.picker.show_hidden;
    match e {
        Event::Found { scan, files, done } => {
            let Some(p) = picker(state).filter(|p| p.scan == scan && scan != 0) else {
                return Vec::new();
            };
            absorb(p, files);
            if done {
                p.scanning = false;
            }
            fix_scroll(state);
            want_preview(state).into_iter().collect()
        }
        Event::Listed { dir, entries } => {
            if let Mode::Command(_) = state.mode {
                return command_listed(state, &dir, entries);
            }
            let Some(p) = picker(state).filter(|p| p.path_mode()) else {
                return Vec::new();
            };
            let Some(listing) = p.listing.as_mut().filter(|l| l.dir == dir) else {
                return Vec::new();
            };
            match entries {
                Ok(e) => listing.entries = Some(e),
                Err(why) => {
                    listing.entries = Some(Vec::new());
                    p.problem = Some(why);
                }
            }
            filter_entries(p, hidden);
            fix_scroll(state);
            want_preview(state).into_iter().collect()
        }
        Event::Previewed(preview) => {
            if let Some(p) = picker(state)
                && p.preview_for.as_ref() == Some(&preview.path)
            {
                p.preview = Some(preview);
            }
            Vec::new()
        }
        Event::Browse(dir) => browse(state, &dir),
    }
}

/// Ask for the selected file's preview when the pane is shown and has
/// another file.
fn want_preview(state: &mut State) -> Option<Effect> {
    let wide = picker_box(state.cols, state.rows).preview.is_some();
    let p = picker(state)?;
    if !wide {
        return None;
    }
    let (path, dir) = p.current().and_then(|h| p.target(h))?;
    if dir || p.preview_for.as_ref() == Some(&path) {
        return None;
    }
    p.preview_for = Some(path.clone());
    p.preview = None;
    Some(job(Job::Preview(path)))
}

/// Text typed into the query.
pub(crate) fn typed(state: &mut State, text: &str) -> Vec<Effect> {
    match picker(state) {
        Some(p) => p.query.push_str(text),
        None => return Vec::new(),
    }
    refresh(state)
}

/// What the picker's keys do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Move(isize),
    Page(bool),
    Open,
    Close,
    Erase,
    EraseWord,
    Clear,
    Complete,
    ToggleAll,
}

/// A picker key.
pub(crate) fn key(state: &mut State, k: Key) -> Vec<Effect> {
    let rows = isize::try_from(list_rows(state)).unwrap_or(1).max(1);
    let hidden = state.settings.picker.show_hidden;
    let Some(p) = picker(state) else {
        return Vec::new();
    };
    match k {
        Key::Move(d) => return move_by(state, d),
        Key::Page(down) => return move_by(state, if down { rows } else { -rows }),
        Key::Open => return accept(state),
        Key::Close => return close(state),
        Key::Erase => {
            if p.query.pop().is_none() {
                return Vec::new();
            }
        }
        Key::EraseWord => {
            // Back over spaces and slashes, then over the word before.
            let q = p.query.trim_end_matches([' ', '/']);
            let keep = q.rfind([' ', '/']).map_or(0, |i| i + 1);
            p.query.truncate(keep);
        }
        Key::Clear => p.query.clear(),
        Key::Complete => {
            if !complete(p, hidden) {
                return Vec::new();
            }
        }
        Key::ToggleAll => {
            p.all = !p.all;
            if p.path_mode() {
                return Vec::new();
            }
            // Search again: the other files are wanted.
            p.scan = 0;
            p.scanning = false;
            let mut out = vec![job(Job::Stop)];
            out.extend(refresh(state));
            return out;
        }
    }
    refresh(state)
}

/// Tab in path mode: the longest common prefix of the entries starting
/// with the name typed, else the selected entry. Whether the query changed.
fn complete(p: &mut Picker, hidden: bool) -> bool {
    if !p.path_mode() {
        return false;
    }
    let (part, leaf) = paths::split(&p.query);
    let Some(entries) = p.entries() else {
        return false;
    };
    let chosen = |p: &Picker| {
        let h = p.current()?;
        let e = p.entries()?.get(h.index)?;
        Some(paths::typed_name(&e.name, e.dir).into_owned())
    };
    let name = match paths::complete(entries, leaf, hidden) {
        Completion::Unique(name, dir) => Some(paths::typed_name(&name, dir).into_owned()),
        Completion::Longer(prefix) => Some(prefix),
        Completion::Several(_) | Completion::Nothing => chosen(p),
    };
    let Some(name) = name else {
        return false;
    };
    let query = paths::join(part, &name);
    let changed = query != p.query;
    p.query = query;
    changed
}

/// Move the selection by `delta` results.
fn move_by(state: &mut State, delta: isize) -> Vec<Effect> {
    if let Some(p) = picker(state) {
        let last = p.hits.len().saturating_sub(1);
        p.selected = if delta >= 0 {
            p.selected.saturating_add(delta.unsigned_abs()).min(last)
        } else {
            p.selected.saturating_sub(delta.unsigned_abs())
        };
    }
    fix_scroll(state);
    want_preview(state).into_iter().collect()
}

/// Enter: open the selected file, go into the selected directory, or open
/// the path typed.
fn accept(state: &mut State) -> Vec<Effect> {
    let home = state.settings.home.clone();
    let Some(p) = picker(state) else {
        return Vec::new();
    };
    match p.current().and_then(|h| p.target(h)) {
        Some((_, true)) if p.path_mode() => {
            let (part, _) = paths::split(&p.query);
            let name = p
                .current()
                .and_then(|h| p.entries()?.get(h.index))
                .map(|e| paths::typed_name(&e.name, true).into_owned())
                .unwrap_or_default();
            p.query = paths::join(part, &name);
            refresh(state)
        }
        Some((path, _)) => {
            let mut out = close(state);
            out.push(Effect::OpenPath(path));
            out
        }
        None if p.path_mode() && !p.query.ends_with('/') => {
            match paths::resolve(&p.query, &p.dir, home.as_deref()) {
                Ok(path) => {
                    let mut out = close(state);
                    out.push(Effect::OpenPath(path));
                    out
                }
                Err(why) => {
                    state.complain(why.message());
                    Vec::new()
                }
            }
        }
        None => Vec::new(),
    }
}

/// A click at `(col, row)`: a result opens, outside the box closes.
pub(crate) fn click(state: &mut State, col: u16, row: u16) -> Vec<Effect> {
    let g = picker_box(state.cols, state.rows);
    let inside = g.list.contains(col, row) || g.preview.is_some_and(|b| b.contains(col, row));
    if !inside {
        return close(state);
    }
    let first = usize::from(g.list.y) + 2;
    let Some(p) = picker(state) else {
        return Vec::new();
    };
    let at = usize::from(row);
    if g.list.contains(col, row) && at >= first && at < first + list_rows_of(&g) {
        let i = p.scroll + (at - first);
        if i < p.hits.len() {
            p.selected = i;
            return accept(state);
        }
    }
    Vec::new()
}

/// The terminal changed size: the selection stays in view.
pub(crate) fn resized(state: &mut State) -> Vec<Effect> {
    fix_scroll(state);
    want_preview(state).into_iter().collect()
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Where the picker's boxes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Boxes {
    /// The query line and the results.
    pub(crate) list: BoxGeom,
    /// The preview, on terminals at least [`PREVIEW_COLS`] wide.
    pub(crate) preview: Option<BoxGeom>,
}

/// The picker's boxes on a `cols` × `rows` terminal: as tall as the
/// screen allows (a row free above and below when there is room),
/// centred.
pub(crate) fn picker_box(cols: u16, rows: u16) -> Boxes {
    let area = rows.saturating_sub(1);
    let (y, h) = if area >= 12 { (1, area - 2) } else { (0, area) };
    if cols >= PREVIEW_COLS {
        let total = cols.saturating_sub(4).min(160);
        let x = (cols - total) / 2;
        let lw = total * 11 / 20;
        return Boxes {
            list: BoxGeom { x, y, w: lw, h },
            preview: Some(BoxGeom {
                x: x + lw,
                y,
                w: total - lw,
                h,
            }),
        };
    }
    let w = if cols < 28 { cols } else { (cols - 4).min(80) };
    Boxes {
        list: BoxGeom {
            x: (cols - w) / 2,
            y,
            w,
            h,
        },
        preview: None,
    }
}

/// Results the list box shows at once (below the query line).
pub(crate) fn list_rows_of(g: &Boxes) -> usize {
    g.list.inner_rows().saturating_sub(1)
}

fn list_rows(state: &State) -> usize {
    list_rows_of(&picker_box(state.cols, state.rows))
}

fn fix_scroll(state: &mut State) {
    let rows = list_rows(state);
    if let Some(p) = picker(state) {
        p.selected = p.selected.min(p.hits.len().saturating_sub(1));
        p.scroll = super::toc::scroll_to(p.scroll, p.selected, rows);
    }
}

// ---------------------------------------------------------------------------
// `:e`
// ---------------------------------------------------------------------------

/// The argument of `:e`, `:edit` or `:open` (`Some("")` without one).
pub(crate) fn edit_arg(command: &str) -> Option<&str> {
    let (name, arg) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    matches!(name, "e" | "edit" | "open").then(|| arg.trim())
}

/// `:e PATH`: open `arg` (relative to the current document's directory,
/// `~` the home directory).
pub(crate) fn edit(state: &mut State, arg: &str) -> Vec<Effect> {
    if arg.is_empty() {
        state.complain(":e needs a file name (O opens the file picker)");
        return Vec::new();
    }
    match paths::resolve(arg, &doc_dir(state), state.settings.home.as_deref()) {
        Ok(path) => vec![Effect::OpenPath(path)],
        Err(why) => {
            state.complain(why.message());
            Vec::new()
        }
    }
}

/// Tab at the `:` prompt: complete the path after `:e` (the shell lists
/// its directory first).
pub(crate) fn command_tab(state: &mut State) -> Vec<Effect> {
    let Mode::Command(input) = &state.mode else {
        return Vec::new();
    };
    let Some(arg) = edit_arg(input.trim_start()) else {
        return Vec::new();
    };
    let (part, _) = paths::split(arg);
    let input = input.clone();
    match paths::resolve(part, &doc_dir(state), state.settings.home.as_deref()) {
        Ok(dir) => {
            state.completing = Some(input);
            vec![job(Job::List(dir))]
        }
        Err(why) => {
            state.completions = Some(vec![why.message().to_owned()]);
            Vec::new()
        }
    }
}

/// The directory the `:` prompt's Tab asked for is listed: complete.
fn command_listed(
    state: &mut State,
    dir: &Path,
    entries: Result<Vec<Entry>, String>,
) -> Vec<Effect> {
    let hidden = state.settings.picker.show_hidden;
    let home = state.settings.home.clone();
    let base = doc_dir(state);
    let Mode::Command(input) = &mut state.mode else {
        return Vec::new();
    };
    if state.completing.as_ref() != Some(input) {
        return Vec::new();
    }
    state.completing = None;
    let Some(arg) = edit_arg(input.trim_start()) else {
        return Vec::new();
    };
    let (part, leaf) = paths::split(arg);
    if paths::resolve(part, &base, home.as_deref()).ok().as_deref() != Some(dir) {
        return Vec::new();
    }
    let entries = match entries {
        Ok(e) => e,
        Err(why) => {
            state.completions = Some(vec![why]);
            return Vec::new();
        }
    };
    let head_len = input.len() - arg.len();
    let name = match paths::complete(&entries, leaf, hidden) {
        Completion::Unique(name, dir) => paths::typed_name(&name, dir).into_owned(),
        Completion::Longer(prefix) => prefix,
        Completion::Several(names) => {
            state.completions = Some(names);
            return Vec::new();
        }
        Completion::Nothing => {
            state.completions = Some(Vec::new());
            return Vec::new();
        }
    };
    let completed = format!("{}{}", &input[..head_len], paths::join(part, &name));
    *input = completed;
    Vec::new()
}

/// The preview of the file at `path`: its first [`PREVIEW_BYTES`],
/// parsed with `parse` for its headings when it is Markdown (by `exts`),
/// else its first lines.
pub(crate) fn read_preview(
    path: &Path,
    exts: &[String],
    parse: &crate::parse::ParseOptions,
) -> Preview {
    use std::io::Read as _;
    let mut preview = Preview {
        path: path.to_path_buf(),
        lines: Vec::new(),
        note: None,
    };
    let regular = std::fs::metadata(path).map(|m| m.is_file());
    let mut bytes = Vec::new();
    let read = match regular {
        Ok(true) => {
            std::fs::File::open(path).and_then(|f| f.take(PREVIEW_BYTES).read_to_end(&mut bytes))
        }
        Ok(false) => {
            preview.note = Some("not a regular file".into());
            return preview;
        }
        Err(e) => Err(e),
    };
    if let Err(e) = read {
        preview.note = Some(e.to_string());
        return preview;
    }
    if bytes.contains(&0) {
        preview.note = Some("a binary file".into());
        return preview;
    }
    let text = String::from_utf8_lossy(&bytes);
    if scan::is_markdown(path, exts) {
        let doc = crate::parse::parse(&text, parse);
        preview.lines = doc
            .headings
            .iter()
            .map(|h| (h.level, h.title.to_string()))
            .collect();
        if preview.lines.is_empty() {
            preview.note = Some("no headings".into());
        }
    } else {
        preview.lines = text.lines().take(200).map(|l| (0, l.to_owned())).collect();
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` made-up paths of a big tree of notes and code.
    fn paths(n: usize) -> Vec<PathBuf> {
        let words = [
            "alpha", "beta", "gamma", "notes", "src", "docs", "results", "protocol", "model",
            "atom", "encoder", "review", "data", "README", "layout", "pager", "update", "theme",
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |m: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m as u64) as usize
        };
        (0..n)
            .map(|i| {
                let depth = 1 + next(5);
                let mut p = PathBuf::from("/repo");
                for _ in 0..depth {
                    p.push(format!("{}_{}", words[next(words.len())], next(40)));
                }
                p.push(format!(
                    "{}_{}_{i}.{}",
                    words[next(words.len())],
                    words[next(words.len())],
                    ["md", "rs", "txt"][next(3)]
                ));
                p
            })
            .collect()
    }

    fn candidates(n: usize) -> Vec<Candidate> {
        let display = scan::Display {
            base: PathBuf::from("/repo"),
            repo: Some(PathBuf::from("/repo")),
            dir: PathBuf::from("/repo/docs"),
            home: None,
        };
        paths(n)
            .into_iter()
            .map(|p| display.candidate(p, 0))
            .collect()
    }

    #[test]
    fn the_best_hits_are_kept_in_order() {
        let cands = candidates(5_000);
        let pattern = Pattern::new("notes md");
        let all: Vec<u32> = (0..5_000).collect();
        let (hits, matched) = best_of(&pattern, &cands, Vec::new(), &all);
        assert_eq!(hits.len(), MAX_HITS.min(matched.len()));
        assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
        // Ranking in two halves keeps the same best hits.
        let (first, m1) = best_of(&pattern, &cands, Vec::new(), &all[..2_500]);
        let (both, m2) = best_of(&pattern, &cands, first, &all[2_500..]);
        assert_eq!([m1, m2].concat(), matched);
        let ids = |h: &[Hit]| h.iter().map(|h| h.index).collect::<Vec<_>>();
        assert_eq!(ids(&both), ids(&hits));
        assert!(hits.iter().all(|h| !h.positions.is_empty()));
    }

    /// `cargo test --release --lib -- --ignored --nocapture picker_speed`
    #[test]
    #[ignore = "a benchmark: run in release with --nocapture"]
    #[allow(clippy::print_stderr)]
    fn picker_speed() {
        let cands = candidates(100_000);
        let all: Vec<u32> = (0..100_000).collect();
        eprintln!("on {} threads at most", crate::parallel::cpus());
        for q in [
            "a",
            "md",
            "notes",
            "aamr",
            "pager upd",
            "zzzz",
            "README",
            "enc rev md",
        ] {
            let pattern = Pattern::new(q);
            let runs = 5;
            let start = std::time::Instant::now();
            let mut matched = 0;
            for _ in 0..runs {
                matched = best_of(&pattern, &cands, Vec::new(), &all).1.len();
            }
            let each = start.elapsed() / runs;
            // One thread, as on a single CPU.
            let start = std::time::Instant::now();
            let _ = score_chunk(&pattern, &cands, &all);
            let one = start.elapsed();
            eprintln!(
                "{q:>12}: {matched:>6} of 100000 match, ranked in {each:?} ({one:?} on one thread)"
            );
        }
    }
}
