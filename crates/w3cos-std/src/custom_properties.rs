//! Shared, copy-on-write storage for CSS custom properties.
//!
//! Workbench themes define hundreds of `--vscode-*` variables on the root
//! and every element inherits them. Holding the map behind an `Arc` makes
//! the per-element `Style` clone and the inherited-style equality check
//! O(1) when the map is shared, instead of copying and comparing every
//! entry; mutation clones the map only while it is still shared.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::sync::{Arc, OnceLock};

/// The hasher every custom-property map is built with. `RandomState` is
/// seeded per instance, so two maps with identical entries iterate in
/// different orders and an equality test has to hash and look up every key of
/// one in the other. Sharing one process-wide state (still randomly keyed, so
/// still resistant to crafted collisions) makes equal maps that were built or
/// cloned the same way lay out identically, and [`CustomProperties`] equality
/// can then compare them entry by entry in iteration order without hashing.
fn shared_state() -> RandomState {
    static STATE: OnceLock<RandomState> = OnceLock::new();
    STATE.get_or_init(RandomState::new).clone()
}

fn with_shared_state(map: HashMap<String, String>) -> HashMap<String, String> {
    use std::hash::BuildHasher;
    let shared = shared_state();
    if map.hasher().hash_one("--") == shared.hash_one("--") {
        return map;
    }
    let mut rebuilt = HashMap::with_capacity_and_hasher(map.len(), shared);
    rebuilt.extend(map);
    rebuilt
}

/// The shared map plus a lazily computed content fingerprint. Any mutation
/// resets the fingerprint, so it always describes the current entries.
struct Inner {
    map: HashMap<String, String>,
    fingerprint: OnceLock<u64>,
}

impl Default for Inner {
    fn default() -> Self {
        Self::from_map(HashMap::with_hasher(shared_state()))
    }
}

impl Clone for Inner {
    fn clone(&self) -> Self {
        // A clone holds the same entries, so a computed fingerprint stays
        // valid and later writes adjust it instead of rehashing every entry.
        let fingerprint = OnceLock::new();
        if let Some(known) = self.fingerprint.get() {
            let _ = fingerprint.set(*known);
        }
        MAP_CLONES.with(|count| count.set(count.get() + 1));
        Self {
            map: self.map.clone(),
            fingerprint,
        }
    }
}

thread_local! {
    static MAP_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static EQ_ORDERED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static EQ_LOOKUP: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Content comparisons of distinct maps with equal fingerprints on this
/// thread: `(settled by walking both maps in order, needed per-key lookups)`.
#[doc(hidden)]
pub fn eq_path_counts() -> (usize, usize) {
    (
        EQ_ORDERED.with(std::cell::Cell::get),
        EQ_LOOKUP.with(std::cell::Cell::get),
    )
}

/// Whole-map clones performed on this thread. Lets tests prove a cascade
/// path kept the map shared instead of unsharing it.
#[doc(hidden)]
pub fn map_clone_count() -> usize {
    MAP_CLONES.with(std::cell::Cell::get)
}

fn entry_hash(key: &str, value: &str) -> u64 {
    fn bytes(seed: u64, text: &str) -> u64 {
        let mut hash = seed ^ 0xcbf2_9ce4_8422_2325;
        for byte in text.as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
    let mut value = bytes(bytes(1, key), value);
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

impl Inner {
    fn from_map(map: HashMap<String, String>) -> Self {
        Self {
            map: with_shared_state(map),
            fingerprint: OnceLock::new(),
        }
    }

    /// Exact entry comparison for maps of equal length. Equal maps that share
    /// a layout (same hasher, built or cloned alike) are confirmed by one
    /// pairwise walk with no hashing; any divergence in order falls back to
    /// per-key lookups, so the answer never depends on layout.
    fn entries_equal(&self, other: &Inner) -> bool {
        if self
            .map
            .iter()
            .zip(other.map.iter())
            .all(|(left, right)| left == right)
        {
            EQ_ORDERED.with(|count| count.set(count.get() + 1));
            return true;
        }
        EQ_LOOKUP.with(|count| count.set(count.get() + 1));
        self.map == other.map
    }

    /// Order-independent hash of every entry. Two maps with different
    /// fingerprints are different; equal fingerprints still need a compare.
    fn fingerprint(&self) -> u64 {
        *self.fingerprint.get_or_init(|| {
            self.map.iter().fold(self.map.len() as u64, |acc, (k, v)| {
                acc.wrapping_add(entry_hash(k, v))
            })
        })
    }
}

#[derive(Clone, Default)]
pub struct CustomProperties(Arc<Inner>);

#[derive(Default)]
struct FingerprintDelta {
    added: u64,
    removed: u64,
    len_added: i64,
}

impl CustomProperties {
    /// Unique access to the map: unshares it and drops the stale fingerprint.
    fn map_mut(&mut self) -> &mut HashMap<String, String> {
        let inner = Arc::make_mut(&mut self.0);
        inner.fingerprint = OnceLock::new();
        &mut inner.map
    }

    /// Unique access that keeps an already computed fingerprint current: the
    /// closure reports the fingerprint change (`entries` added and the hash
    /// sum added and removed) for the writes it performed.
    fn write_tracked<R>(
        &mut self,
        write: impl FnOnce(&mut HashMap<String, String>, &mut FingerprintDelta) -> R,
    ) -> R {
        let inner = Arc::make_mut(&mut self.0);
        let known = inner.fingerprint.take();
        let mut delta = FingerprintDelta::default();
        let result = write(&mut inner.map, &mut delta);
        if let Some(known) = known {
            let _ = inner.fingerprint.set(
                known
                    .wrapping_add(delta.added)
                    .wrapping_sub(delta.removed)
                    .wrapping_add(delta.len_added as u64),
            );
        }
        result
    }
}

impl CustomProperties {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply `edit` (a pure function of the map, `tag` and `arg`) and share
    /// the outcome with every later caller that starts from the same
    /// allocation and asks for the same edit. A cascade that stamps the same
    /// marker onto thousands of elements inheriting one big map would
    /// otherwise clone that map for each of them; here the first caller pays
    /// and the rest get the same `Arc` back (which also makes their later
    /// equality checks pointer compares). Remembers the last few edits per
    /// thread.
    pub fn edit_shared(&mut self, tag: &'static str, arg: &str, edit: impl FnOnce(&mut Self)) {
        const SLOTS: usize = 16;
        if self.is_empty() {
            // A fresh empty map is never the base of a later call.
            edit(self);
            return;
        }
        struct Memo {
            base: CustomProperties,
            tag: &'static str,
            arg: String,
            result: CustomProperties,
        }
        thread_local! {
            static MEMOS: std::cell::RefCell<(Vec<Memo>, usize)> =
                const { std::cell::RefCell::new((Vec::new(), 0)) };
        }
        let hit = MEMOS.with(|memos| {
            memos
                .borrow()
                .0
                .iter()
                .find(|memo| memo.base.ptr_eq(self) && memo.tag == tag && memo.arg == arg)
                .map(|memo| memo.result.clone())
        });
        if let Some(result) = hit {
            *self = result;
            return;
        }
        let base = self.clone();
        edit(self);
        let memo = Memo {
            base,
            tag,
            arg: arg.to_string(),
            result: self.clone(),
        };
        MEMOS.with(|memos| {
            let (slots, next) = &mut *memos.borrow_mut();
            if slots.len() < SLOTS {
                slots.push(memo);
            } else {
                slots[*next] = memo;
                *next = (*next + 1) % SLOTS;
            }
        });
    }

    /// Whether both values share one map allocation.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Insert unless the map already holds this exact value, so a no-op
    /// write keeps the allocation shared.
    pub fn insert(&mut self, property: String, value: String) -> Option<String> {
        if self.0.map.get(&property) == Some(&value) {
            return Some(value);
        }
        self.write_tracked(|map, delta| {
            delta.added = delta.added.wrapping_add(entry_hash(&property, &value));
            let previous = map.insert(property.clone(), value);
            match &previous {
                Some(old) => {
                    delta.removed = delta.removed.wrapping_add(entry_hash(&property, old));
                }
                None => delta.len_added += 1,
            }
            previous
        })
    }

    /// Remove a property, unsharing the map only when it is present.
    pub fn remove<Q>(&mut self, property: &Q) -> Option<String>
    where
        String: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if !self.0.map.contains_key(property) {
            return None;
        }
        self.write_tracked(|map, delta| {
            let (key, value) = map.remove_entry(property)?;
            delta.removed = delta.removed.wrapping_add(entry_hash(&key, &value));
            delta.len_added -= 1;
            Some(value)
        })
    }

    /// Whether both maps hold the same entries once `ignored` keys are left
    /// out of each. Never clones a map: the content fingerprint (minus the
    /// ignored entries' hashes) rejects different maps in O(#ignored), and a
    /// fingerprint match is verified by lookups without allocating.
    pub fn eq_ignoring(&self, other: &Self, ignored: &[&str]) -> bool {
        if self.ptr_eq(other) {
            return true;
        }
        fn adjusted(inner: &Inner, ignored: &[&str]) -> (usize, u64) {
            let mut len = inner.map.len();
            let mut fingerprint = inner.fingerprint();
            for key in ignored {
                if let Some((key, value)) = inner.map.get_key_value(*key) {
                    len -= 1;
                    // The fingerprint folds `len` in once and one hash per entry.
                    fingerprint = fingerprint
                        .wrapping_sub(entry_hash(key, value))
                        .wrapping_sub(1);
                }
            }
            (len, fingerprint)
        }
        let (left_len, left_fingerprint) = adjusted(&self.0, ignored);
        let (right_len, right_fingerprint) = adjusted(&other.0, ignored);
        left_len == right_len
            && left_fingerprint == right_fingerprint
            && self
                .0
                .map
                .iter()
                .filter(|(key, _)| !ignored.contains(&key.as_str()))
                .all(|(key, value)| other.0.map.get(key) == Some(value))
    }

    /// Retain matching properties, unsharing the map only when one goes.
    pub fn retain(&mut self, mut keep: impl FnMut(&String, &String) -> bool) {
        if self
            .0
            .map
            .iter()
            .any(|(property, value)| !keep(property, value))
        {
            self.write_tracked(|map, delta| {
                map.retain(|property, value| {
                    let kept = keep(property, value);
                    if !kept {
                        delta.removed = delta.removed.wrapping_add(entry_hash(property, value));
                        delta.len_added -= 1;
                    }
                    kept
                });
            });
        }
    }

    /// The owned map, cloning it only when it is shared.
    pub fn into_inner(self) -> HashMap<String, String> {
        match Arc::try_unwrap(self.0) {
            Ok(inner) => inner.map,
            Err(shared) => shared.map.clone(),
        }
    }
}

impl std::ops::Deref for CustomProperties {
    type Target = HashMap<String, String>;

    fn deref(&self) -> &Self::Target {
        &self.0.map
    }
}

impl std::ops::DerefMut for CustomProperties {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.map_mut()
    }
}

impl PartialEq for CustomProperties {
    fn eq(&self, other: &Self) -> bool {
        if self.ptr_eq(other) {
            return true;
        }
        // Differing maps are the common case for a per-element marker; the
        // fingerprint rejects them without hashing every entry again.
        self.0.map.len() == other.0.map.len()
            && self.0.fingerprint() == other.0.fingerprint()
            && self.0.entries_equal(&other.0)
    }
}

impl Eq for CustomProperties {}

impl std::fmt::Debug for CustomProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.map.fmt(f)
    }
}

impl From<crate::fxhash::FxHashMap<String, String>> for CustomProperties {
    fn from(map: crate::fxhash::FxHashMap<String, String>) -> Self {
        Self::from(HashMap::<String, String>::from_iter(map))
    }
}

impl From<HashMap<String, String>> for CustomProperties {
    fn from(map: HashMap<String, String>) -> Self {
        Self(Arc::new(Inner::from_map(map)))
    }
}

impl From<CustomProperties> for HashMap<String, String> {
    fn from(map: CustomProperties) -> Self {
        map.into_inner()
    }
}

impl FromIterator<(String, String)> for CustomProperties {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        let mut map = HashMap::with_hasher(shared_state());
        map.extend(iter);
        Self(Arc::new(Inner::from_map(map)))
    }
}

impl Extend<(String, String)> for CustomProperties {
    fn extend<I: IntoIterator<Item = (String, String)>>(&mut self, iter: I) {
        for (property, value) in iter {
            self.insert(property, value);
        }
    }
}

impl<'a> IntoIterator for &'a CustomProperties {
    type Item = (&'a String, &'a String);
    type IntoIter = std::collections::hash_map::Iter<'a, String, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.map.iter()
    }
}

impl IntoIterator for CustomProperties {
    type Item = (String, String);
    type IntoIter = std::collections::hash_map::IntoIter<String, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_inner().into_iter()
    }
}

impl Serialize for CustomProperties {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.map.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CustomProperties {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        HashMap::<String, String>::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn edit_shared_clones_once_per_base_and_edit() {
        let base: CustomProperties = (0..300)
            .map(|n| (format!("--v{n}"), n.to_string()))
            .collect();
        let stamp = |props: &mut CustomProperties, value: &str| {
            props.edit_shared("stamp", value, |p| {
                p.insert("--marker".into(), value.into());
            });
        };
        let before = map_clone_count();
        let mut first = base.clone();
        stamp(&mut first, "1");
        let mut second = base.clone();
        stamp(&mut second, "1");
        assert_eq!(
            map_clone_count(),
            before + 1,
            "second stamp reuses the first"
        );
        assert!(first.ptr_eq(&second));
        assert_eq!(first.get("--marker").map(String::as_str), Some("1"));
        assert!(base.get("--marker").is_none());
        // Another value or another base is its own edit.
        let mut other_value = base.clone();
        stamp(&mut other_value, "2");
        assert!(!other_value.ptr_eq(&first));
        assert_eq!(other_value.get("--marker").map(String::as_str), Some("2"));
        let mut other_base = base.clone();
        other_base.insert("--extra".into(), "x".into());
        stamp(&mut other_base, "1");
        assert!(!other_base.ptr_eq(&first));
        assert_eq!(other_base.get("--extra").map(String::as_str), Some("x"));
        // Writing to a shared result unshares it and leaves the memo intact.
        first.insert("--marker".into(), "changed".into());
        let mut third = base.clone();
        stamp(&mut third, "1");
        assert_eq!(third.get("--marker").map(String::as_str), Some("1"));
        assert!(third.ptr_eq(&second));
    }

    #[test]

    fn equal_maps_built_or_cloned_alike_compare_without_lookups() {
        let build = || -> CustomProperties {
            (0..400)
                .map(|n| (format!("--v{n}"), n.to_string()))
                .collect()
        };
        let (a, b) = (build(), build());
        assert!(!a.ptr_eq(&b));
        let before = eq_path_counts();
        assert_eq!(a, b);
        let mut c = a.clone();
        c.insert("--extra".into(), "1".into());
        c.remove("--extra");
        assert!(!a.ptr_eq(&c));
        assert_eq!(a, c);
        let after = eq_path_counts();
        assert_eq!(after.1, before.1, "shared layout needs no per-key lookups");
        assert_eq!(after.0, before.0 + 2);
        // A foreign map is rehashed onto the shared state when wrapped; its
        // insertion order differs, so the compare may need lookups but is exact.
        let foreign: HashMap<String, String> = (0..400)
            .map(|n| (format!("--v{n}"), n.to_string()))
            .collect();
        assert_eq!(CustomProperties::from(foreign), a);
    }

    #[test]
    fn different_insertion_order_still_compares_equal() {
        let forward: CustomProperties = (0..300)
            .map(|n| (format!("--v{n}"), n.to_string()))
            .collect();
        let backward: CustomProperties = (0..300)
            .rev()
            .map(|n| (format!("--v{n}"), n.to_string()))
            .collect();
        assert_eq!(forward, backward);
        let mut changed = backward.clone();
        changed.insert("--v3".into(), "other".into());
        assert_ne!(forward, changed);
    }

    #[test]
    fn eq_ignoring_skips_listed_keys_without_cloning() {
        let base: HashMap<String, String> = (0..200)
            .map(|i| (format!("--v{i}"), format!("{i}")))
            .collect();
        let mut left = CustomProperties::from(base.clone());
        let mut right = CustomProperties::from(base);
        left.insert("--run".into(), "1".into());
        right.insert("--run".into(), "2".into());
        right.insert("--ends".into(), "3,4".into());
        assert!(left != right);
        let before = map_clone_count();
        assert!(left.eq_ignoring(&right, &["--run", "--ends"]));
        assert!(right.eq_ignoring(&left, &["--run", "--ends"]));
        // A real difference is still seen, including one hidden by a same-size swap.
        right.insert("--v7".into(), "x".into());
        assert!(!left.eq_ignoring(&right, &["--run", "--ends"]));
        let mut other = CustomProperties::from(
            (0..200)
                .map(|i| (format!("--w{i}"), format!("{i}")))
                .collect::<HashMap<_, _>>(),
        );
        other.insert("--run".into(), "1".into());
        assert!(!left.eq_ignoring(&other, &["--run"]));
        assert!(CustomProperties::default().eq_ignoring(&left_only("--run"), &["--run"]));
        assert!(!CustomProperties::default().eq_ignoring(&left_only("--x"), &["--run"]));
        assert_eq!(map_clone_count(), before, "comparison must not clone a map");
    }

    fn left_only(key: &str) -> CustomProperties {
        let mut map = CustomProperties::default();
        map.insert(key.into(), "1".into());
        map
    }

    use super::*;

    #[test]
    fn clones_share_until_written() {
        let mut a: CustomProperties = [("--a".to_string(), "1".to_string())].into_iter().collect();
        let b = a.clone();
        assert!(a.ptr_eq(&b));
        assert_eq!(a, b);
        a.insert("--b".into(), "2".into());
        assert!(!a.ptr_eq(&b));
        assert_eq!(b.len(), 1);
        assert_eq!(a.len(), 2);
        assert_ne!(a, b);
    }

    #[test]
    fn no_op_writes_keep_the_map_shared() {
        let mut a: CustomProperties = [("--a".to_string(), "1".to_string())].into_iter().collect();
        let b = a.clone();
        a.insert("--a".into(), "1".into());
        a.remove("--missing");
        a.retain(|property, _| property.starts_with("--"));
        a.extend([("--a".to_string(), "1".to_string())]);
        assert!(a.ptr_eq(&b));
        a.retain(|_, _| false);
        assert!(a.is_empty());
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn equal_contents_compare_equal_without_sharing() {
        let a: CustomProperties = [("--a".to_string(), "1".to_string())].into_iter().collect();
        let b: CustomProperties = [("--a".to_string(), "1".to_string())].into_iter().collect();
        assert!(!a.ptr_eq(&b));
        assert_eq!(a, b);
    }

    #[test]
    fn same_length_maps_with_one_different_value_are_unequal() {
        let entries = |last: &str| -> CustomProperties {
            (0..300)
                .map(|n| (format!("--v{n}"), n.to_string()))
                .chain([("--marker".to_string(), last.to_string())])
                .collect()
        };
        let a = entries("12");
        let b = entries("13");
        assert_eq!(a.len(), b.len());
        assert_ne!(a, b);
        assert_eq!(a, entries("12"));
    }

    #[test]
    fn fingerprint_follows_every_kind_of_mutation() {
        let base: CustomProperties = (0..20)
            .map(|n| (format!("--v{n}"), n.to_string()))
            .collect();
        // Force the fingerprint before each write, then compare to a fresh map.
        let mut a = base.clone();
        let mut b = base.clone();
        assert_eq!(a, b);
        a.insert("--v3".into(), "changed".into());
        assert_ne!(a, b);
        b.insert("--v3".into(), "changed".into());
        assert_eq!(a, b);
        a.remove("--v4");
        assert_ne!(a, b);
        b.remove("--v4");
        assert_eq!(a, b);
        a.retain(|property, _| property != "--v5");
        assert_ne!(a, b);
        b.retain(|property, _| property != "--v5");
        assert_eq!(a, b);
        // Mutation through DerefMut on a map that is no longer shared.
        use std::ops::DerefMut;
        a.deref_mut().insert("--v6".into(), "direct".into());
        assert_ne!(a, b);
        b.deref_mut().insert("--v6".into(), "direct".into());
        assert_eq!(a, b);
        // Unshared in-place write after the fingerprint was cached.
        let mut c = a.clone();
        assert_eq!(c, a);
        drop(a);
        c.insert("--v7".into(), "late".into());
        assert_ne!(c, b);
    }

    fn fresh_fingerprint(properties: &CustomProperties) -> u64 {
        Inner::from_map(properties.0.map.clone()).fingerprint()
    }

    #[test]
    fn incremental_fingerprint_matches_a_full_rehash_after_every_write() {
        let mut a: CustomProperties = (0..50)
            .map(|n| (format!("--v{n}"), n.to_string()))
            .collect();
        // Prime the cached fingerprint, then write through a shared clone.
        assert_eq!(a.0.fingerprint(), fresh_fingerprint(&a));
        let shared = a.clone();
        a.insert("--v1".into(), "other".into());
        assert_eq!(a.0.fingerprint(), fresh_fingerprint(&a));
        a.insert("--new".into(), "x".into());
        assert_eq!(a.0.fingerprint(), fresh_fingerprint(&a));
        a.remove("--v2");
        assert_eq!(a.0.fingerprint(), fresh_fingerprint(&a));
        a.retain(|property, _| !property.starts_with("--v3"));
        assert_eq!(a.0.fingerprint(), fresh_fingerprint(&a));
        a.extend([("--e1".to_string(), "1".to_string())]);
        assert_eq!(a.0.fingerprint(), fresh_fingerprint(&a));
        // The shared source is untouched.
        assert_eq!(shared.0.fingerprint(), fresh_fingerprint(&shared));
        assert_ne!(shared, a);
        // A write that never primed the fingerprint still hashes correctly.
        let mut b = shared.clone();
        b.insert("--late".into(), "1".into());
        assert_eq!(b.0.fingerprint(), fresh_fingerprint(&b));
        // Equal contents reached through different write orders agree.
        let mut c = shared.clone();
        c.insert("--late".into(), "1".into());
        assert_eq!(b, c);
    }
}
