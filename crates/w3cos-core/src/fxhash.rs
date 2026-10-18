//! Fast non-cryptographic hasher for hot in-process caches.
//!
//! Same hasher as `w3cos_std::fxhash` (the crates do not depend on each
//! other); generated AOT code keys its module registries with it.
//!
//! Blink keys its per-thread layout caches (ShapeCache, FontCache lookups)
//! with WTF's StringHasher instead of a DoS-resistant SipHash. These caches
//! are private to the UI thread and bounded, so the same trade-off applies.
//! Word-at-a-time multiply/rotate (rustc's FxHasher) with a murmur3 fmix64
//! finaliser so the 64-bit result is also usable as a stand-alone key.

use std::hash::{BuildHasherDefault, Hasher};

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

#[derive(Clone, Copy, Default)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while bytes.len() >= 8 {
            self.add(u64::from_le_bytes(bytes[..8].try_into().unwrap()));
            bytes = &bytes[8..];
        }
        if bytes.len() >= 4 {
            self.add(u64::from(u32::from_le_bytes(
                bytes[..4].try_into().unwrap(),
            )));
            bytes = &bytes[4..];
        }
        for &byte in bytes {
            self.add(u64::from(byte));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        let mut h = self.hash;
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 33;
        h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        h ^ (h >> 33)
    }
}

pub type FxBuildHasher = BuildHasherDefault<FxHasher>;
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuildHasher>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    fn hash<T: Hash + ?Sized>(value: &T) -> u64 {
        let mut hasher = FxHasher::default();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn distinguishes_nearby_strings_and_lengths() {
        assert_ne!(hash("abc"), hash("abd"));
        assert_ne!(hash("a"), hash("a\0"));
        assert_eq!(hash("same text"), hash("same text"));
        let mut map = FxHashMap::default();
        map.insert("k".to_string(), 1);
        assert_eq!(map.get("k"), Some(&1));
    }
}
