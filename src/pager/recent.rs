//! The files opened lately, for the file picker: a small "frecency" store.
//!
//! The store is a text file (`$XDG_STATE_HOME/emde/recent`, else
//! `~/.local/state/emde/recent`) with one line per file:
//! `count<TAB>last-unix-time<TAB>path`. Every document the pager shows is
//! recorded ([`record`]): its count goes up and its time is now. The file
//! keeps the [`CAP`] files with the highest [`frecency`], and is written
//! atomically (a temporary file renamed over it), so a reader never sees
//! half of it.
//!
//! The store is never needed: a missing, unreadable or corrupt file reads
//! as empty (corrupt lines are skipped), and a failure to write is only
//! noted for `-v` ([`take_problems`]).

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The most files the store keeps.
pub(crate) const CAP: usize = 500;

/// The longest store file read (a bigger one is not ours).
const MAX_BYTES: u64 = 1 << 20;

/// What went wrong with the store, for `-v`.
static PROBLEMS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Problems with the store since the last call (for `-v`).
pub(crate) fn take_problems() -> Vec<String> {
    PROBLEMS
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default()
}

fn note(problem: String) {
    if let Ok(mut v) = PROBLEMS.lock()
        && !v.contains(&problem)
    {
        v.push(problem);
    }
}

/// One file in the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Visit {
    /// How often it was opened.
    pub(crate) count: u32,
    /// When it was opened last (seconds since the Unix epoch).
    pub(crate) last: u64,
    pub(crate) path: PathBuf,
}

/// The store's file: `$XDG_STATE_HOME/emde/recent` (an absolute
/// `$XDG_STATE_HOME` only, as the XDG spec says), else
/// `$HOME/.local/state/emde/recent`.
pub(crate) fn store_path(xdg_state_home: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    let state = xdg_state_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(".local/state")))?;
    Some(state.join("emde").join("recent"))
}

/// Seconds since the Unix epoch.
pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How much a visit counts at time `now`: its count, weighted by how long
/// ago it was (×16 within the hour, ×8 within the day, ×2 within the week,
/// ×1 after that).
pub(crate) fn frecency(v: &Visit, now: u64) -> u32 {
    let age = now.saturating_sub(v.last);
    let weight = match age {
        0..3_600 => 16,
        3_600..86_400 => 8,
        86_400..604_800 => 2,
        _ => 1,
    };
    v.count.saturating_mul(weight)
}

/// The visits in a store file's text, most frecent first (lines that do
/// not parse are skipped).
pub(crate) fn parse(text: &str, now: u64) -> Vec<Visit> {
    let out: Vec<Visit> = text
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let count = parts.next()?.parse().ok()?;
            let last = parts.next()?.parse().ok()?;
            let path = PathBuf::from(parts.next()?);
            (count > 0 && path.is_absolute()).then_some(Visit { count, last, path })
        })
        .collect();
    sort(out, now)
}

/// Most frecent first, then most recent.
fn sort(visits: Vec<Visit>, now: u64) -> Vec<Visit> {
    let key = |v: &Visit| (frecency(v, now), v.last);
    super::picker::sorted(visits, &|a, b| key(b).cmp(&key(a)))
}

/// The text of a store file.
fn render(visits: &[Visit]) -> String {
    let mut out = String::new();
    for v in visits {
        if let Some(p) = v.path.to_str() {
            out.push_str(&format!("{}\t{}\t{p}\n", v.count, v.last));
        }
    }
    out
}

/// The visits in the store at `store` (none when it cannot be read).
pub(crate) fn load(store: &Path, now: u64) -> Vec<Visit> {
    let Ok(file) = fs::File::open(store) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if io::Read::read_to_end(&mut io::Read::take(file, MAX_BYTES), &mut bytes).is_err() {
        return Vec::new();
    }
    match String::from_utf8(bytes) {
        Ok(text) => parse(&text, now),
        Err(_) => Vec::new(),
    }
}

/// Whether `path` can be written as a line of the store.
fn storable(path: &Path) -> bool {
    path.is_absolute()
        && path
            .to_str()
            .is_some_and(|s| !s.contains(['\n', '\r', '\t']))
}

/// `visits` with `path` opened at `now`, capped at [`CAP`].
pub(crate) fn visited(mut visits: Vec<Visit>, path: &Path, now: u64) -> Vec<Visit> {
    match visits.iter_mut().find(|v| v.path == path) {
        Some(v) => {
            v.count = v.count.saturating_add(1);
            v.last = now;
        }
        None => visits.push(Visit {
            count: 1,
            last: now,
            path: path.to_path_buf(),
        }),
    }
    let mut visits = sort(visits, now);
    visits.truncate(CAP);
    visits
}

/// Record that `path` was opened, in the store at `store`. Failures are
/// noted for `-v`, never reported otherwise.
pub(crate) fn record(store: &Path, path: &Path) {
    if !storable(path) {
        return;
    }
    let now = now();
    let visits = visited(load(store, now), path, now);
    if let Err(e) = write_atomically(store, render(&visits).as_bytes()) {
        note(format!("could not update {}: {e}", store.display()));
    }
}

/// Write `bytes` to `path` through a temporary file in the same directory
/// (created if need be), renamed over it.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("no directory"))?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    // No fsync: the store is a convenience, and recording must never make
    // opening a document wait for the disk.
    let written = fs::File::create(&tmp).and_then(|mut f| f.write_all(bytes));
    match written.and_then(|()| fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emde-recent-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn where_the_store_is() {
        let home = Path::new("/home/u");
        assert_eq!(
            store_path(Some("/state"), Some(home)),
            Some(PathBuf::from("/state/emde/recent"))
        );
        assert_eq!(
            store_path(Some("relative"), Some(home)),
            Some(PathBuf::from("/home/u/.local/state/emde/recent")),
            "a relative XDG_STATE_HOME is ignored"
        );
        assert_eq!(store_path(None, None), None, "no home: no store");
    }

    #[test]
    fn round_trip_and_ordering() {
        let dir = temp("round-trip");
        let store = dir.join("state/emde/recent");
        record(&store, Path::new("/a.md"));
        record(&store, Path::new("/b.md"));
        record(&store, Path::new("/b.md"));
        let visits = load(&store, now());
        let paths: Vec<_> = visits.iter().map(|v| v.path.to_str().unwrap()).collect();
        assert_eq!(paths, ["/b.md", "/a.md"], "more often first");
        assert_eq!(visits[0].count, 2);
        // Not storable: relative, or a newline in it.
        record(&store, Path::new("rel.md"));
        record(&store, Path::new("/x\ny.md"));
        assert_eq!(load(&store, now()).len(), 2);
        // No temporary file is left behind.
        let left: Vec<_> = fs::read_dir(store.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn frecency_weighs_recent_visits() {
        let now = 100_000_000;
        let v = |count, ago| Visit {
            count,
            last: now - ago,
            path: PathBuf::from("/x"),
        };
        assert_eq!(frecency(&v(1, 10), now), 16);
        assert_eq!(frecency(&v(1, 7_200), now), 8);
        assert_eq!(frecency(&v(3, 200_000), now), 6);
        assert_eq!(frecency(&v(5, 10_000_000), now), 5);
        // Yesterday's single visit beats last month's three.
        let text = "3\t97000000\t/old.md\n1\t99990000\t/new.md\n";
        let got: Vec<_> = parse(text, now).into_iter().map(|v| v.path).collect();
        assert_eq!(got, [PathBuf::from("/new.md"), PathBuf::from("/old.md")]);
    }

    #[test]
    fn the_store_is_capped() {
        let mut visits = Vec::new();
        for i in 0..CAP + 20 {
            visits = visited(visits, Path::new(&format!("/f{i}.md")), 1_000 + i as u64);
        }
        assert_eq!(visits.len(), CAP);
        // The newest stayed.
        assert!(
            visits
                .iter()
                .any(|v| v.path == Path::new(&format!("/f{}.md", CAP + 19)))
        );
    }

    #[test]
    fn corrupt_stores_read_as_empty_or_partly() {
        let dir = temp("corrupt");
        fs::create_dir_all(&dir).unwrap();
        let store = dir.join("recent");
        fs::write(&store, b"\xff\xfe garbage \x00\x01").unwrap();
        assert!(load(&store, now()).is_empty());
        fs::write(
            &store,
            "x\ty\t/a.md\n2\t5\t/b.md\nno tabs\n0\t1\t/zero.md\n1\t1\trel\n",
        )
        .unwrap();
        let got: Vec<_> = load(&store, now()).into_iter().map(|v| v.path).collect();
        assert_eq!(got, [PathBuf::from("/b.md")]);
        // A missing store, and one that cannot be written.
        assert!(load(&dir.join("missing"), now()).is_empty());
        let blocked = dir.join("recent/inside");
        record(&blocked, Path::new("/a.md"));
        assert!(
            take_problems()
                .iter()
                .any(|p| p.contains("could not update")),
            "noted for -v"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_writes_replace_the_file() {
        let dir = temp("atomic");
        let path = dir.join("sub/file");
        write_atomically(&path, b"one").unwrap();
        write_atomically(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
