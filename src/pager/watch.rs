//! Watching the file for changes by polling `stat`.
//!
//! While the reader is idle the file is checked every [`INTERVAL`]. A
//! change only counts once two readings [`SETTLE`] apart agree: an editor
//! that saves by writing a new file and renaming it over the old one (or
//! writes in several steps) is caught after the save, not in the middle
//! of it. The reading compares modification time, size, inode and device,
//! so a rename that keeps the time is still seen. A file that disappears
//! is waited for: nothing happens until it is back.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Time between checks while nothing changes.
pub(crate) const INTERVAL: Duration = Duration::from_millis(500);
/// Time between the two readings that confirm a change.
pub(crate) const SETTLE: Duration = Duration::from_millis(100);

/// What `stat` says about a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    ino: u64,
    dev: u64,
}

/// Read the stamp of `path` (`None` when it cannot be read).
pub(crate) fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let (ino, dev) = {
        use std::os::unix::fs::MetadataExt as _;
        (meta.ino(), meta.dev())
    };
    #[cfg(not(unix))]
    let (ino, dev) = (0, 0);
    Some(Stamp {
        modified: meta.modified().ok(),
        len: meta.len(),
        ino,
        dev,
    })
}

/// Polls one file; see the module docs.
#[derive(Clone, Debug)]
pub(crate) struct Watcher {
    path: PathBuf,
    /// The reading the shown document belongs to.
    last: Option<Stamp>,
    /// A different reading waiting for confirmation.
    candidate: Option<Stamp>,
    next: Instant,
}

impl Watcher {
    /// Watch `path`, whose shown version had stamp `baseline` (`None`:
    /// unknown, so the first reading counts as a change).
    pub(crate) fn new(path: PathBuf, baseline: Option<Stamp>, now: Instant) -> Watcher {
        Watcher {
            path,
            last: baseline,
            candidate: None,
            next: now + INTERVAL,
        }
    }

    /// The stamp of the version shown.
    pub(crate) fn baseline(&self) -> Option<Stamp> {
        self.last
    }

    /// The file watched.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// When the next check is due.
    pub(crate) fn deadline(&self) -> Instant {
        self.next
    }

    /// Check the file if a check is due; `true` when a change is
    /// confirmed. The baseline stays until [`Watcher::rebase`] (after the
    /// file was read successfully), so a failed read is tried again.
    pub(crate) fn poll(&mut self, now: Instant) -> bool {
        self.poll_with(now, stamp)
    }

    /// [`Watcher::poll`] with the `stat` supplied (tests).
    pub(crate) fn poll_with(
        &mut self,
        now: Instant,
        stat: impl FnOnce(&Path) -> Option<Stamp>,
    ) -> bool {
        if now < self.next {
            return false;
        }
        let Some(reading) = stat(&self.path) else {
            // Gone (for now): wait for it to come back.
            self.candidate = None;
            self.next = now + INTERVAL;
            return false;
        };
        if Some(reading) == self.last {
            self.candidate = None;
            self.next = now + INTERVAL;
            return false;
        }
        if self.candidate == Some(reading) {
            self.candidate = None;
            self.next = now + INTERVAL;
            return true;
        }
        self.candidate = Some(reading);
        self.next = now + SETTLE;
        false
    }

    /// The file was just read, as it was at `baseline`.
    pub(crate) fn rebase(&mut self, baseline: Option<Stamp>, now: Instant) {
        self.last = baseline;
        self.candidate = None;
        self.next = now + INTERVAL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(len: u64) -> Option<Stamp> {
        Some(Stamp {
            modified: None,
            len,
            ino: 1,
            dev: 1,
        })
    }

    #[test]
    fn a_change_needs_two_equal_readings() {
        let t0 = Instant::now();
        let mut w = Watcher {
            path: PathBuf::from("x.md"),
            last: at(1),
            candidate: None,
            next: t0 + INTERVAL,
        };
        assert!(!w.poll_with(t0, |_| at(2)), "not due yet");
        let t1 = t0 + INTERVAL;
        assert!(!w.poll_with(t1, |_| at(2)), "first reading");
        assert_eq!(w.deadline(), t1 + SETTLE);
        let t2 = t1 + SETTLE;
        assert!(!w.poll_with(t2, |_| at(3)), "still being written");
        let t3 = t2 + SETTLE;
        assert!(w.poll_with(t3, |_| at(3)), "settled");
        assert_eq!(w.deadline(), t3 + INTERVAL);
        // Not read yet: still a change.
        let t4 = t3 + INTERVAL;
        assert!(!w.poll_with(t4, |_| at(3)));
        assert!(w.poll_with(t4 + SETTLE, |_| at(3)), "tried again");
        // Read: the new state is the baseline.
        w.rebase(at(3), t4 + SETTLE);
        let t5 = w.deadline();
        assert!(!w.poll_with(t5, |_| at(3)), "nothing new");
        assert!(!w.poll_with(t5 + SETTLE, |_| at(3)));
    }

    #[test]
    fn a_missing_file_is_waited_for() {
        let t0 = Instant::now();
        let mut w = Watcher {
            path: PathBuf::from("x.md"),
            last: at(1),
            candidate: None,
            next: t0,
        };
        assert!(!w.poll_with(t0, |_| None));
        let t1 = w.deadline();
        assert!(!w.poll_with(t1, |_| at(5)));
        assert!(w.poll_with(t1 + SETTLE, |_| at(5)));
        w.rebase(at(5), t1 + SETTLE);
        // Changed and changed back before settling: nothing.
        let t2 = w.deadline();
        assert!(!w.poll_with(t2, |_| at(6)));
        assert!(!w.poll_with(t2 + SETTLE, |_| at(5)));
        assert_eq!(w.candidate, None);
    }

    #[test]
    fn real_files_are_stamped() {
        let dir = std::env::temp_dir().join(format!("emde-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("doc.md");
        std::fs::write(&file, "one").unwrap();
        let a = stamp(&file).unwrap();
        std::fs::write(&file, "three").unwrap();
        let b = stamp(&file).unwrap();
        assert_ne!(a, b, "the size changed");
        assert_eq!(stamp(&dir.join("missing.md")), None);
        let t0 = Instant::now();
        let mut w = Watcher::new(file.clone(), stamp(&file), t0);
        assert_eq!(w.path(), file);
        assert_eq!(w.baseline(), Some(b));
        w.rebase(stamp(&file), t0);
        assert!(!w.poll(t0 + INTERVAL));
        // An old baseline: the file changed while it was not watched.
        let mut w = Watcher::new(file.clone(), Some(a), t0);
        assert!(!w.poll(t0 + INTERVAL));
        assert!(w.poll(t0 + INTERVAL + SETTLE));
        assert_eq!(w.baseline(), Some(a), "until the file is read");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
