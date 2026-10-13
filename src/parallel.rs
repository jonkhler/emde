//! A parallel map on scoped threads, for the few places where one run has
//! independent pieces of slow work: fetching and decoding images,
//! highlighting code blocks.
//!
//! No thread pool is kept: a run does this once or twice, and a scoped
//! thread costs tens of microseconds.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::panic::guarded;

/// The number of CPUs this process may use, asked once: the standard
/// library reads the cgroup's CPU quota from `/proc` and `/sys` on every
/// call (a dozen system calls).
pub(crate) fn cpus() -> usize {
    static CPUS: OnceLock<usize> = OnceLock::new();
    *CPUS.get_or_init(|| std::thread::available_parallelism().map_or(1, usize::from))
}

/// `f` applied to every item, results in the items' order, on up to
/// `max_threads` threads (never more than one per CPU or per item). The
/// calling thread works too; one item is done on it alone. A panic in `f`
/// gives `None` for that item (it is recorded like any guarded panic), and
/// if no thread can be started the calling thread does everything.
pub(crate) fn map<T: Sync, R: Send>(
    items: &[T],
    max_threads: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<Option<R>> {
    if items.len() <= 1 {
        return items.iter().map(|item| guarded(|| f(item))).collect();
    }
    let threads = cpus().min(max_threads).min(items.len());
    if threads <= 1 {
        return items.iter().map(|item| guarded(|| f(item))).collect();
    }
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(i) else {
                return done;
            };
            done.push((i, guarded(|| f(item))));
        }
    };
    let mut out: Vec<Option<R>> = items.iter().map(|_| None).collect();
    std::thread::scope(|s| {
        let helpers: Vec<_> = (1..threads)
            .filter_map(|_| std::thread::Builder::new().spawn_scoped(s, work).ok())
            .collect();
        let mut results = work();
        for helper in helpers {
            results.extend(helper.join().unwrap_or_default());
        }
        for (i, r) in results {
            if let Some(slot) = out.get_mut(i) {
                *slot = r;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_keep_the_order_of_the_items() {
        let items: Vec<u64> = (0..100).collect();
        let squares = map(&items, 8, |&n| n * n);
        assert_eq!(squares.len(), 100);
        for (n, sq) in items.iter().zip(&squares) {
            assert_eq!(*sq, Some(n * n));
        }
        assert!(map(&[] as &[u8], 4, |&b| b).is_empty());
        assert_eq!(map(&[7u8], 4, |&b| b + 1), [Some(8)]);
        assert_eq!(map(&[1u8, 2], 1, |&b| b), [Some(1), Some(2)]);
    }

    #[test]
    fn a_panic_loses_only_its_item() {
        let items = [1, 2, 3, 4];
        let out = map(&items, 4, |&n| {
            assert!(n != 3, "boom");
            n
        });
        assert_eq!(out, [Some(1), Some(2), None, Some(4)]);
        crate::panic::take_caught();
    }

    #[test]
    fn work_is_spread_over_threads() {
        use std::collections::HashSet;
        use std::sync::Mutex;
        if cpus() < 2 {
            return;
        }
        let seen = Mutex::new(HashSet::new());
        let items: Vec<u32> = (0..64).collect();
        map(&items, 4, |_| {
            std::thread::sleep(std::time::Duration::from_millis(2));
            if let Ok(mut s) = seen.lock() {
                s.insert(std::thread::current().id());
            }
        });
        let threads = seen.lock().map(|s| s.len()).unwrap_or(0);
        assert!((2..=4).contains(&threads), "{threads} threads");
    }
}
