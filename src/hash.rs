//! A fast hasher for the small, fixed-size keys of emde's own lookup
//! tables: interned styles and the layout's style caches, hashed once per
//! span of text.
//!
//! The standard library's SipHash resists hash flooding, which costs time
//! on every lookup; these keys are styles made from the theme and the
//! document's markup (at most a few thousand distinct ones per document),
//! so a multiplicative hash (the one rustc uses for its own tables) is
//! enough.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A [`HashMap`] with [`FastHasher`].
pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

/// Rotate, xor, multiply: one step per word written.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct FastHasher {
    hash: u64,
}

/// An odd constant with well-spread bits (from rustc's `FxHasher`).
const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FastHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for w in words {
            self.add(u64::from_le_bytes(*w));
        }
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(last));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{BuildHasher, Hash};

    fn hash<T: Hash + ?Sized>(t: &T) -> u64 {
        BuildHasherDefault::<FastHasher>::default().hash_one(t)
    }

    #[test]
    fn equal_keys_hash_alike_and_close_keys_differ() {
        assert_eq!(hash(&(1u16, "a")), hash(&(1u16, "a")));
        let mut seen = std::collections::HashSet::new();
        for a in 0..64u16 {
            for b in 0..64u8 {
                assert!(seen.insert(hash(&(a, b))), "{a} {b}");
            }
        }
        // Bytes past a whole word count too.
        assert_ne!(
            hash(&[0u8; 9][..]),
            hash(&[0u8, 0, 0, 0, 0, 0, 0, 0, 1][..])
        );
    }

    #[test]
    fn a_map_works() {
        let mut m: FastMap<(u32, char), usize> = FastMap::default();
        for i in 0..1000 {
            m.insert((i, 'x'), i as usize);
        }
        assert_eq!(m.len(), 1000);
        assert_eq!(m.get(&(500, 'x')), Some(&500));
    }
}
