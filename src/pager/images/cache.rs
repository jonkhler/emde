//! The renditions the pager holds, by what they are, within a byte budget.
//!
//! A rendition is asked of the worker once ([`Slot::Pending`]) and then
//! kept, ready or known to fail. Ready ones are evicted least recently used
//! first when the budget is exceeded, except those the current frame uses
//! and the kitty images uploaded to the terminal ([`Renditions::pin`]):
//! their ids are live there, so they stay until the pager deletes them.

use std::collections::HashMap;

use crate::gfx::store::{Made, Make};
use crate::ir::ImageId;

/// Bytes of renditions kept (plan §3).
pub(crate) const BUDGET: usize = 32 << 20;

/// A pager-local number for an image store (one per document).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct StoreId(pub(crate) u32);

/// Which rendition: an image of a store at a size in cells, made as `make`
/// says.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub(crate) store: StoreId,
    pub(crate) image: ImageId,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) make: Make,
}

/// Where a rendition stands.
#[derive(Debug)]
pub(crate) enum Slot {
    /// Asked of the worker, as job `0`.
    Pending(u64),
    /// It cannot be made: the fallback stays.
    Failed,
    /// Made.
    Ready(Ready),
}

/// A rendition that is made.
#[derive(Debug)]
pub(crate) struct Ready {
    pub(crate) made: Made,
    /// Bytes counted against the budget.
    bytes: usize,
    /// The frame that last used it.
    used: u64,
    /// A kitty image uploaded to the terminal.
    pinned: bool,
}

/// The renditions; see the module docs.
#[derive(Debug)]
pub(crate) struct Renditions {
    slots: HashMap<Key, Slot>,
    bytes: usize,
    budget: usize,
    /// The current frame (renditions it uses are never evicted).
    frame: u64,
}

impl Renditions {
    /// An empty cache keeping at most `budget` bytes of renditions (more
    /// only while the current frame needs them).
    pub(crate) fn new(budget: usize) -> Renditions {
        Renditions {
            slots: HashMap::new(),
            bytes: 0,
            budget,
            frame: 1,
        }
    }

    /// Start a new frame: what the last one used may be evicted now.
    pub(crate) fn next_frame(&mut self) {
        self.frame += 1;
    }

    /// Where `key` stands.
    pub(crate) fn get(&self, key: &Key) -> Option<&Slot> {
        self.slots.get(key)
    }

    /// The rendition `key`, when it is made.
    pub(crate) fn made(&self, key: &Key) -> Option<&Made> {
        match self.slots.get(key)? {
            Slot::Ready(r) => Some(&r.made),
            Slot::Pending(_) | Slot::Failed => None,
        }
    }

    /// Mark `key` as used by the current frame.
    pub(crate) fn touch(&mut self, key: &Key) {
        if let Some(Slot::Ready(r)) = self.slots.get_mut(key) {
            r.used = self.frame;
        }
    }

    /// `key` was asked of the worker as job `job`.
    pub(crate) fn pending(&mut self, key: Key, job: u64) {
        self.slots.insert(key, Slot::Pending(job));
    }

    /// `key` cannot be made.
    pub(crate) fn fail(&mut self, key: Key) {
        self.remove(&key);
        self.slots.insert(key, Slot::Failed);
    }

    /// The worker's result for `key` (`None`: it cannot be made). A
    /// rendition uploaded already stays: the new one is another image under
    /// another id, never shown.
    pub(crate) fn insert(&mut self, key: Key, made: Option<Made>) {
        if let Some(Slot::Ready(r)) = self.slots.get(&key)
            && r.pinned
        {
            return;
        }
        self.remove(&key);
        let slot = match made {
            Some(made) => {
                let bytes = made.bytes();
                self.bytes += bytes;
                Slot::Ready(Ready {
                    made,
                    bytes,
                    used: self.frame,
                    pinned: false,
                })
            }
            None => Slot::Failed,
        };
        self.slots.insert(key, slot);
        self.evict();
    }

    /// Job `job` for `key` was skipped: forget the slot if it still waits
    /// for that job (so the rendition is asked for again).
    pub(crate) fn skipped(&mut self, key: &Key, job: u64) {
        if matches!(self.slots.get(key), Some(Slot::Pending(j)) if *j == job) {
            self.slots.remove(key);
        }
    }

    /// Forget every pending slot: their jobs are skipped (a new layout made
    /// them stale), and what is still wanted is asked for again.
    pub(crate) fn forget_pending(&mut self) {
        self.slots.retain(|_, s| !matches!(s, Slot::Pending(_)));
    }

    /// Take the upload of a kitty image to write it, and keep the image
    /// from now on (its id is live in the terminal). `None` when `key` is
    /// not a kitty image, or was uploaded already.
    pub(crate) fn pin(&mut self, key: &Key) -> Option<Vec<u8>> {
        let Some(Slot::Ready(r)) = self.slots.get_mut(key) else {
            return None;
        };
        if r.pinned {
            return None;
        }
        let upload = match &mut r.made {
            Made::Placeholders(p) => std::mem::take(&mut p.upload),
            Made::Kitty(k) => std::mem::take(&mut k.upload),
            Made::Blocks(_) | Made::Pixels(_) => return None,
        };
        r.pinned = true;
        self.bytes = self.bytes.saturating_sub(upload.len());
        r.bytes = r.bytes.saturating_sub(upload.len());
        Some(upload)
    }

    /// Drop `key`.
    pub(crate) fn remove(&mut self, key: &Key) {
        if let Some(Slot::Ready(r)) = self.slots.remove(key) {
            self.bytes = self.bytes.saturating_sub(r.bytes);
        }
    }

    /// Keep only the renditions `keep` says to.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&Key) -> bool) {
        let gone: Vec<Key> = self.slots.keys().filter(|k| !keep(k)).cloned().collect();
        for key in gone {
            self.remove(&key);
        }
    }

    /// Bytes held by ready renditions.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Evict ready renditions, least recently used first, until the budget
    /// holds; never pinned ones, nor what the current frame uses.
    fn evict(&mut self) {
        while self.bytes > self.budget {
            let frame = self.frame;
            let oldest = self
                .slots
                .iter()
                .filter_map(|(k, s)| match s {
                    Slot::Ready(r) if !r.pinned && r.used < frame => Some((r.used, k)),
                    _ => None,
                })
                .min_by_key(|(used, _)| *used)
                .map(|(_, k)| k.clone());
            match oldest {
                Some(key) => self.remove(&key),
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::kitty;
    use crate::gfx::store::Placeholders;

    fn key(image: u32, make: Make) -> Key {
        Key {
            store: StoreId(1),
            image: ImageId(image),
            cols: 4,
            rows: 2,
            make,
        }
    }

    fn pixels(n: usize) -> Option<Made> {
        Some(Made::Pixels(vec![0; n]))
    }

    #[test]
    fn slots_go_from_pending_to_ready_or_failed() {
        let mut c = Renditions::new(1000);
        let a = key(0, Make::Iterm(0..2));
        assert!(c.get(&a).is_none());
        c.pending(a.clone(), 7);
        assert!(matches!(c.get(&a), Some(Slot::Pending(7))));
        assert!(c.made(&a).is_none());
        c.insert(a.clone(), pixels(10));
        assert_eq!(c.made(&a), Some(&Made::Pixels(vec![0; 10])));
        assert_eq!(c.bytes(), 10);
        let b = key(1, Make::Blocks);
        c.insert(b.clone(), None);
        assert!(matches!(c.get(&b), Some(Slot::Failed)));
        c.fail(a.clone());
        assert!(matches!(c.get(&a), Some(Slot::Failed)));
        assert_eq!(c.bytes(), 0);
    }

    #[test]
    fn skipped_jobs_only_clear_their_own_slot() {
        let mut c = Renditions::new(1000);
        let a = key(0, Make::Blocks);
        c.pending(a.clone(), 1);
        // Asked for again (a new layout) before the old job was skipped.
        c.forget_pending();
        assert!(c.get(&a).is_none());
        c.pending(a.clone(), 2);
        c.skipped(&a, 1);
        assert!(matches!(c.get(&a), Some(Slot::Pending(2))));
        c.skipped(&a, 2);
        assert!(c.get(&a).is_none());
    }

    #[test]
    fn the_least_recently_used_go_first_but_never_the_current_frame() {
        let mut c = Renditions::new(100);
        let (a, b, d) = (
            key(0, Make::Iterm(0..1)),
            key(1, Make::Iterm(0..1)),
            key(2, Make::Iterm(0..1)),
        );
        c.insert(a.clone(), pixels(40));
        c.next_frame();
        c.insert(b.clone(), pixels(40));
        c.next_frame();
        c.touch(&a);
        // Over budget: b is older than a now.
        c.insert(d.clone(), pixels(40));
        assert!(c.made(&a).is_some());
        assert!(c.made(&b).is_none(), "evicted");
        assert!(c.made(&d).is_some());
        assert_eq!(c.bytes(), 80);
        // Everything in use by this frame stays, budget or not.
        c.touch(&a);
        c.insert(b.clone(), pixels(90));
        assert!(c.made(&a).is_some() && c.made(&b).is_some() && c.made(&d).is_some());
    }

    #[test]
    fn uploaded_kitty_images_are_pinned() {
        let mut c = Renditions::new(10);
        let id = kitty::ImageId::new(5).unwrap();
        let p = key(0, Make::Placeholders);
        c.insert(
            p.clone(),
            Some(Made::Placeholders(Placeholders {
                id,
                upload: b"upload".to_vec(),
                rows: vec![b"row".to_vec()],
            })),
        );
        assert_eq!(c.pin(&p).as_deref(), Some(&b"upload"[..]));
        assert_eq!(c.pin(&p), None, "uploaded once");
        assert_eq!(c.bytes(), 3, "the upload is gone");
        // A late duplicate never replaces the uploaded image.
        c.insert(p.clone(), pixels(1));
        assert!(matches!(c.made(&p), Some(Made::Placeholders(x)) if x.id == id));
        // Pinned images are not evicted.
        c.next_frame();
        c.insert(key(1, Make::Blocks), pixels(50));
        assert!(c.made(&p).is_some());
        assert_eq!(c.pin(&key(1, Make::Blocks)), None, "not a kitty image");
        c.retain(|k| k.image != ImageId(0));
        assert!(c.get(&p).is_none());
    }
}
