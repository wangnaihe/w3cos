//! Shared, copy-on-write storage for CSS custom properties.
//!
//! Workbench themes define hundreds of `--vscode-*` variables on the root
//! and every element inherits them. Holding the map behind an `Arc` makes
//! the per-element `Style` clone and the inherited-style equality check
//! O(1) when the map is shared, instead of copying and comparing every
//! entry; mutation clones the map only while it is still shared.
//!
//! Keys and values are `Arc<str>`: a map unshared to stamp one per-element
//! marker onto an inherited theme copies only its table, and every theme
//! string stays shared with the map it came from. (A Code-OSS boot holds a
//! few hundred such copies of a ~1000-entry theme; with owned `String`s each
//! copy also duplicated every key and value.)
//!
//! Unsharing a large map does not copy its table either: the writer gets a
//! small overlay (its own entries and removals) on top of the shared flat
//! map, so stamping a per-element marker onto an inherited ~1000-entry theme
//! costs a few entries instead of a full table. Overlays never nest (an
//! overlay's base is always flat) and fold back into a flat map once their
//! own entries grow past a small limit.

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

type Text = Arc<str>;
type Map = HashMap<Text, Text>;

fn with_shared_state(map: Map) -> Map {
    use std::hash::BuildHasher;
    let shared = shared_state();
    if map.hasher().hash_one("--") == shared.hash_one("--") {
        return map;
    }
    let mut rebuilt = HashMap::with_capacity_and_hasher(map.len(), shared);
    rebuilt.extend(map);
    rebuilt
}

/// Smallest shared map that a write overlays instead of copying.
const OVERLAY_MIN_BASE: usize = 128;
/// Overlay entries (including removals) past which it folds back into a
/// flat map, keeping lookups at most two probes and iteration cheap.
const OVERLAY_MAX_LOCAL: usize = 64;

/// A writer's own entries on top of a shared flat map: `Some` adds or
/// overrides a property, `None` removes one the base holds.
#[derive(Clone)]
struct Overlay {
    base: Arc<Inner>,
    local: HashMap<Text, Option<Text>>,
    len: usize,
}

/// The shared map plus a lazily computed content fingerprint. Any mutation
/// resets the fingerprint, so it always describes the current entries.
/// `map` holds every entry of a flat map and is empty when `overlay` is set.
struct Inner {
    map: Map,
    overlay: Option<Box<Overlay>>,
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
        MAP_CLONES.with(|count| count.set(count.get() + 1));
        Self {
            map: self.map.clone(),
            overlay: self.overlay.clone(),
            fingerprint: self.known_fingerprint(),
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

/// Times a shared map was unshared for a write on this thread (a full copy,
/// or an overlay on a large map). Lets tests prove a cascade path kept the
/// map shared instead of unsharing it.
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
    fn from_map(map: Map) -> Self {
        Self {
            map: with_shared_state(map),
            overlay: None,
            fingerprint: OnceLock::new(),
        }
    }

    fn known_fingerprint(&self) -> OnceLock<u64> {
        let fingerprint = OnceLock::new();
        if let Some(known) = self.fingerprint.get() {
            let _ = fingerprint.set(*known);
        }
        fingerprint
    }

    fn len(&self) -> usize {
        match &self.overlay {
            Some(overlay) => overlay.len,
            None => self.map.len(),
        }
    }

    fn get_entry(&self, property: &str) -> Option<(&Text, &Text)> {
        match &self.overlay {
            None => self.map.get_key_value(property),
            Some(overlay) => match overlay.local.get_key_value(property) {
                Some((key, Some(value))) => Some((key, value)),
                Some((_, None)) => None,
                None => overlay.base.map.get_key_value(property),
            },
        }
    }

    fn iter(&self) -> Iter<'_> {
        match &self.overlay {
            None => Iter {
                state: IterState::Flat(self.map.iter()),
            },
            Some(overlay) => Iter {
                state: IterState::Base(overlay.base.map.iter(), &overlay.local),
            },
        }
    }

    /// Exact entry comparison for maps of equal length. Equal maps that share
    /// a layout (same hasher, built or cloned alike) are confirmed by one
    /// pairwise walk with no hashing; any divergence in order falls back to
    /// per-key lookups, so the answer never depends on layout.
    fn entries_equal(&self, other: &Inner) -> bool {
        if self
            .iter()
            .zip(other.iter())
            .all(|(left, right)| left == right)
        {
            EQ_ORDERED.with(|count| count.set(count.get() + 1));
            return true;
        }
        EQ_LOOKUP.with(|count| count.set(count.get() + 1));
        self.iter().all(|(key, value)| {
            other
                .get_entry(key)
                .is_some_and(|(_, other_value)| &**other_value == value)
        })
    }

    /// Order-independent hash of every entry. Two maps with different
    /// fingerprints are different; equal fingerprints still need a compare.
    fn fingerprint(&self) -> u64 {
        *self.fingerprint.get_or_init(|| {
            self.iter().fold(self.len() as u64, |acc, (k, v)| {
                acc.wrapping_add(entry_hash(k, v))
            })
        })
    }

    /// Fold an overlay into a flat map holding the same entries (and the
    /// same shared key/value text).
    fn flatten(&mut self) {
        if let Some(overlay) = self.overlay.take() {
            let mut map = HashMap::with_capacity_and_hasher(overlay.len, shared_state());
            for (key, value) in overlay.base.map.iter() {
                if !overlay.local.contains_key(key) {
                    map.insert(key.clone(), value.clone());
                }
            }
            for (key, value) in overlay.local.iter() {
                if let Some(value) = value {
                    map.insert(key.clone(), value.clone());
                }
            }
            self.map = map;
        }
    }

    /// Set `key` to `value`; `old` is the entry's current value.
    fn put(&mut self, key: Text, value: Text, old: Option<&Text>) {
        match &mut self.overlay {
            None => {
                self.map.insert(key, value);
            }
            Some(overlay) => {
                if old.is_none() {
                    overlay.len += 1;
                }
                overlay.local.insert(key, Some(value));
            }
        }
        self.fold_large_overlay();
    }

    /// Remove the present entry `key`.
    fn take(&mut self, key: &Text) {
        match &mut self.overlay {
            None => {
                self.map.remove(key);
            }
            Some(overlay) => {
                overlay.len -= 1;
                if overlay.base.map.contains_key(key) {
                    overlay.local.insert(key.clone(), None);
                } else {
                    overlay.local.remove(key);
                }
            }
        }
        self.fold_large_overlay();
    }

    fn fold_large_overlay(&mut self) {
        if self
            .overlay
            .as_ref()
            .is_some_and(|overlay| overlay.local.len() > OVERLAY_MAX_LOCAL)
        {
            self.flatten();
        }
    }
}

/// Borrowing iterator over `(property, value)` pairs. An overlay yields the
/// base entries it keeps, then its own entries.
pub struct Iter<'a> {
    state: IterState<'a>,
}

enum IterState<'a> {
    Flat(std::collections::hash_map::Iter<'a, Text, Text>),
    Base(
        std::collections::hash_map::Iter<'a, Text, Text>,
        &'a HashMap<Text, Option<Text>>,
    ),
    Local(std::collections::hash_map::Iter<'a, Text, Option<Text>>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match &mut self.state {
                IterState::Flat(entries) => {
                    return entries.next().map(|(key, value)| (&**key, &**value));
                }
                IterState::Base(entries, local) => match entries.next() {
                    Some((key, value)) => {
                        if !local.contains_key(key) {
                            return Some((&**key, &**value));
                        }
                    }
                    None => self.state = IterState::Local(local.iter()),
                },
                IterState::Local(entries) => match entries.next() {
                    Some((key, Some(value))) => return Some((&**key, &**value)),
                    Some((_, None)) => {}
                    None => return None,
                },
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct CustomProperties(Arc<Inner>);

impl CustomProperties {
    /// Unique access for a write, keeping an already computed fingerprint
    /// (the caller adjusts it). A shared large flat map gets an empty overlay
    /// on top of it instead of a copy; anything else shared is copied (an
    /// overlay copies only its own entries).
    fn writable(&mut self) -> &mut Inner {
        if Arc::get_mut(&mut self.0).is_none()
            && self.0.overlay.is_none()
            && self.0.map.len() >= OVERLAY_MIN_BASE
        {
            MAP_CLONES.with(|count| count.set(count.get() + 1));
            let base = self.0.clone();
            self.0 = Arc::new(Inner {
                map: HashMap::with_hasher(shared_state()),
                overlay: Some(Box::new(Overlay {
                    len: base.map.len(),
                    local: HashMap::with_hasher(shared_state()),
                    base: base.clone(),
                })),
                fingerprint: base.known_fingerprint(),
            });
        }
        Arc::make_mut(&mut self.0)
    }

    /// Run `write` on unique access, keeping a computed fingerprint current
    /// from the `(added, removed, len change)` the write reports.
    fn write_tracked<R>(
        &mut self,
        write: impl FnOnce(&mut Inner, &mut FingerprintDelta) -> R,
    ) -> R {
        let inner = self.writable();
        let known = inner.fingerprint.take();
        let mut delta = FingerprintDelta::default();
        let result = write(inner, &mut delta);
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

#[derive(Default)]
struct FingerprintDelta {
    added: u64,
    removed: u64,
    len_added: i64,
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
        const SLOTS: usize = 32;
        if self.is_empty() {
            // A fresh empty map is never the base of a later call.
            edit(self);
            return;
        }
        if Arc::strong_count(&self.0) == 1 {
            // Nobody else holds this map, so no later caller can start from
            // it: edit in place. Remembering it would first take a second
            // reference, and that alone makes the edit clone the whole map.
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

    /// `insert` for a marker stamped onto many elements that start from one
    /// inherited map: equal edits of equal bases share one result through
    /// [`Self::edit_shared`] instead of each unsharing (cloning) the map.
    pub fn insert_shared(&mut self, property: &'static str, value: &str) {
        if self.get(property) == Some(value) {
            return;
        }
        self.edit_shared(property, value, |properties| {
            properties.insert(property.to_string(), value.to_string());
        });
    }

    /// `remove` with the sharing of [`Self::insert_shared`].
    pub fn remove_shared(&mut self, property: &'static str) {
        if !self.contains_key(property) {
            return;
        }
        // No inserted value contains NUL, so this arg never names an insert.
        self.edit_shared(property, "\u{0}remove", |properties| {
            properties.remove(property);
        });
    }

    /// Whether both values share one map allocation.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Address of the shared map allocation, for memory accounting (two
    /// values with the same address share one map).
    pub fn allocation_addr(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    /// Estimated heap bytes owned by this allocation: the `Inner`
    /// allocation, its hash table slots, and its share of every key/value
    /// string buffer. Counted once per allocation, however many styles share
    /// it. An overlay is charged for its own entries; its base is charged to
    /// the styles that hold the base itself.
    pub fn estimated_heap_bytes(&self) -> usize {
        // Key and value text is shared by every map cloned from this one, so
        // each map is charged its share (the allocation over its holders):
        // summing over all maps counts every string once.
        fn share(text: &Text) -> usize {
            (2 * std::mem::size_of::<usize>() + text.len()) / Arc::strong_count(text)
        }
        let inner = &*self.0;
        let bytes = std::mem::size_of::<Inner>();
        match &inner.overlay {
            None => {
                let slot = std::mem::size_of::<(Text, Text)>() + 1;
                inner.map.iter().fold(
                    bytes + inner.map.capacity() * slot,
                    |bytes, (key, value)| bytes + share(key) + share(value),
                )
            }
            Some(overlay) => {
                let slot = std::mem::size_of::<(Text, Option<Text>)>() + 1;
                overlay.local.iter().fold(
                    bytes + std::mem::size_of::<Overlay>() + overlay.local.capacity() * slot,
                    |bytes, (key, value)| bytes + share(key) + value.as_ref().map_or(0, share),
                )
            }
        }
    }

    /// Insert unless the map already holds this exact value, so a no-op
    /// write keeps the allocation shared.
    pub fn insert(&mut self, property: String, value: String) -> Option<Text> {
        let (property, value) = (property.as_str(), value.as_str());
        // Reuse the stored key when overwriting, so its text stays shared.
        let (key, old) = match self.0.get_entry(property) {
            Some((_, existing)) if &**existing == value => return Some(existing.clone()),
            Some((key, existing)) => (key.clone(), Some(existing.clone())),
            None => (Text::from(property), None),
        };
        self.write_tracked(|inner, delta| {
            delta.added = delta.added.wrapping_add(entry_hash(property, value));
            match &old {
                Some(old) => delta.removed = delta.removed.wrapping_add(entry_hash(property, old)),
                None => delta.len_added += 1,
            }
            inner.put(key, Text::from(value), old.as_ref());
            old
        })
    }

    /// Remove a property, unsharing the map only when it is present.
    pub fn remove(&mut self, property: &str) -> Option<Text> {
        let (key, value) = self
            .0
            .get_entry(property)
            .map(|(key, value)| (key.clone(), value.clone()))?;
        self.write_tracked(|inner, delta| {
            delta.removed = delta.removed.wrapping_add(entry_hash(&key, &value));
            delta.len_added -= 1;
            inner.take(&key);
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
            let mut len = inner.len();
            let mut fingerprint = inner.fingerprint();
            for key in ignored {
                if let Some((key, value)) = inner.get_entry(key) {
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
                .iter()
                .filter(|(key, _)| !ignored.contains(key))
                .all(|(key, value)| other.get(key) == Some(value))
    }

    /// Retain matching properties, unsharing the map only when one goes.
    pub fn retain(&mut self, mut keep: impl FnMut(&str, &str) -> bool) {
        let dropped: Vec<(Text, Text)> = match &self.0.overlay {
            None => self
                .0
                .map
                .iter()
                .filter(|(property, value)| !keep(property, value))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            Some(_) => self
                .iter()
                .filter(|(property, value)| !keep(property, value))
                .filter_map(|(property, _)| self.0.get_entry(property))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        };
        if dropped.is_empty() {
            return;
        }
        self.write_tracked(|inner, delta| {
            for (key, value) in &dropped {
                delta.removed = delta.removed.wrapping_add(entry_hash(key, value));
                delta.len_added -= 1;
                inner.take(key);
            }
        });
    }

    /// The owned map, cloning it only when it is shared.
    pub fn into_inner(self) -> HashMap<String, String> {
        self.iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    pub fn get(&self, property: &str) -> Option<&str> {
        self.0.get_entry(property).map(|(_, value)| &**value)
    }

    pub fn get_key_value(&self, property: &str) -> Option<(&str, &str)> {
        self.0
            .get_entry(property)
            .map(|(key, value)| (&**key, &**value))
    }

    pub fn contains_key(&self, property: &str) -> bool {
        self.0.get_entry(property).is_some()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.len() == 0
    }

    pub fn iter(&self) -> Iter<'_> {
        self.0.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> + '_ {
        self.iter().map(|(key, _)| key)
    }

    pub fn values(&self) -> impl Iterator<Item = &str> + '_ {
        self.iter().map(|(_, value)| value)
    }

    /// Drop every property (a no-op that keeps sharing when already empty).
    pub fn clear(&mut self) {
        if !self.is_empty() {
            *self = Self::default();
        }
    }
}

impl PartialEq for CustomProperties {
    fn eq(&self, other: &Self) -> bool {
        if self.ptr_eq(other) {
            return true;
        }
        // Differing maps are the common case for a per-element marker; the
        // fingerprint rejects them without hashing every entry again.
        self.0.len() == other.0.len()
            && self.0.fingerprint() == other.0.fingerprint()
            && self.0.entries_equal(&other.0)
    }
}

impl Eq for CustomProperties {}

impl std::fmt::Debug for CustomProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl From<crate::fxhash::FxHashMap<String, String>> for CustomProperties {
    fn from(map: crate::fxhash::FxHashMap<String, String>) -> Self {
        map.into_iter().collect()
    }
}

impl From<HashMap<String, String>> for CustomProperties {
    fn from(map: HashMap<String, String>) -> Self {
        map.into_iter().collect()
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
        map.extend(
            iter.into_iter()
                .map(|(key, value)| (Text::from(key), Text::from(value))),
        );
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
    type Item = (&'a str, &'a str);
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
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
        serializer.collect_map(self.iter())
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
    fn edit_shared_edits_an_unshared_map_in_place() {
        let mut map = CustomProperties::from_iter([("--a".to_string(), "1".to_string())]);
        let before = map_clone_count();
        map.edit_shared("unique-edit", "", |p| {
            p.insert("--b".into(), "2".into());
        });
        assert_eq!(
            map_clone_count(),
            before,
            "a map nobody shares needs no clone"
        );
        assert_eq!(map.get("--b"), Some("2"));
    }

    #[test]
    fn insert_shared_and_remove_shared_share_equal_edits_of_one_base() {
        let base =
            CustomProperties::from_iter((0..32).map(|i| (format!("--theme-{i}"), i.to_string())));
        let before = map_clone_count();
        let inserted: Vec<_> = (0..12)
            .map(|_| {
                let mut map = base.clone();
                map.insert_shared("--w3cos-internal-test-marker", "1");
                map
            })
            .collect();
        assert_eq!(map_clone_count() - before, 1);
        assert!(inserted.iter().all(|m| m.ptr_eq(&inserted[0])));
        assert_eq!(inserted[0].get("--w3cos-internal-test-marker"), Some("1"));
        let before = map_clone_count();
        let removed: Vec<_> = (0..12)
            .map(|_| {
                let mut map = inserted[0].clone();
                map.remove_shared("--w3cos-internal-test-marker");
                map.remove_shared("--absent");
                map
            })
            .collect();
        assert_eq!(map_clone_count() - before, 1);
        assert!(removed.iter().all(|m| m.ptr_eq(&removed[0]) && *m == base));
        // A different value is a different edit.
        let mut other = base.clone();
        other.insert_shared("--w3cos-internal-test-marker", "2");
        assert_eq!(other.get("--w3cos-internal-test-marker"), Some("2"));
    }

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
        assert_eq!(first.get("--marker"), Some("1"));
        assert!(base.get("--marker").is_none());
        // Another value or another base is its own edit.
        let mut other_value = base.clone();
        stamp(&mut other_value, "2");
        assert!(!other_value.ptr_eq(&first));
        assert_eq!(other_value.get("--marker"), Some("2"));
        let mut other_base = base.clone();
        other_base.insert("--extra".into(), "x".into());
        stamp(&mut other_base, "1");
        assert!(!other_base.ptr_eq(&first));
        assert_eq!(other_base.get("--extra"), Some("x"));
        // Writing to a shared result unshares it and leaves the memo intact.
        first.insert("--marker".into(), "changed".into());
        let mut third = base.clone();
        stamp(&mut third, "1");
        assert_eq!(third.get("--marker"), Some("1"));
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
        // Mutation of a map that is no longer shared.
        a.insert("--v6".into(), "direct".into());
        assert_ne!(a, b);
        b.insert("--v6".into(), "direct".into());
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

    #[test]
    fn heap_estimate_counts_shared_map_once_and_grows_with_entries() {
        let mut map = CustomProperties::new();
        let empty = map.estimated_heap_bytes();
        map.insert("--long-name".to_string(), "x".repeat(1000));
        let shared = map.clone();
        assert_eq!(map.allocation_addr(), shared.allocation_addr());
        assert!(map.estimated_heap_bytes() >= empty + 1000 + "--long-name".len());
        let mut edited = shared.clone();
        edited.insert("--other".to_string(), "y".to_string());
        assert_ne!(edited.allocation_addr(), map.allocation_addr());
    }

    #[test]
    fn unsharing_a_map_copies_its_table_but_not_its_text() {
        let base = CustomProperties::from_iter(
            (0..64).map(|i| (format!("--theme-color-{i}"), format!("#{i:06x}"))),
        );
        let mut marked = base.clone();
        marked.insert("--w3cos-internal-marker".into(), "1".into());
        assert!(!marked.ptr_eq(&base));
        for (key, value) in base.iter() {
            let (marked_key, marked_value) = marked.get_key_value(key).unwrap();
            assert_eq!(marked_key.as_ptr(), key.as_ptr());
            assert_eq!(marked_value.as_ptr(), value.as_ptr());
        }
        // Overwriting a value keeps the stored key's text.
        let key_text = marked.get_key_value("--theme-color-3").unwrap().0.as_ptr();
        marked.insert("--theme-color-3".into(), "red".into());
        assert_eq!(
            marked.get_key_value("--theme-color-3").unwrap().0.as_ptr(),
            key_text
        );
        assert_eq!(marked.get("--theme-color-3"), Some("red"));
        assert_eq!(base.get("--theme-color-3"), Some("#000003"));
        // The shared text is charged once across both maps.
        let text: usize = base.iter().map(|(k, v)| k.len() + v.len()).sum();
        let alone = CustomProperties::from_iter(
            (0..64).map(|i| (format!("--theme-color-{i}"), format!("#{i:06x}"))),
        );
        assert!(
            base.estimated_heap_bytes() + marked.estimated_heap_bytes()
                < 2 * alone.estimated_heap_bytes()
        );
        assert!(alone.estimated_heap_bytes() > text);
    }

    fn theme(n: usize) -> CustomProperties {
        (0..n)
            .map(|i| (format!("--theme-{i}"), format!("#{i:06x}")))
            .collect()
    }

    fn flat_copy(properties: &CustomProperties) -> CustomProperties {
        properties
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn full_rehash(properties: &CustomProperties) -> u64 {
        properties
            .iter()
            .fold(properties.len() as u64, |acc, (k, v)| {
                acc.wrapping_add(entry_hash(k, v))
            })
    }

    #[test]
    fn unsharing_a_large_map_overlays_it_instead_of_copying_the_table() {
        let base = theme(1000);
        let mut marked = base.clone();
        marked.insert("--w3cos-internal-marker".into(), "1".into());
        assert!(!marked.ptr_eq(&base));
        assert!(marked.0.overlay.is_some());
        assert!(marked.0.map.is_empty());
        assert!(marked.estimated_heap_bytes() * 20 < base.estimated_heap_bytes());
        assert_eq!(marked.len(), 1001);
        assert_eq!(marked.get("--w3cos-internal-marker"), Some("1"));
        assert_eq!(marked.get("--theme-7"), Some("#000007"));
        assert_eq!(base.get("--w3cos-internal-marker"), None);
        // Overlay text is the base's text, not a copy.
        let (key, value) = marked.get_key_value("--theme-7").unwrap();
        let (base_key, base_value) = base.get_key_value("--theme-7").unwrap();
        assert_eq!(
            (key.as_ptr(), value.as_ptr()),
            (base_key.as_ptr(), base_value.as_ptr())
        );
        assert_eq!(marked, flat_copy(&marked));
        assert_eq!(marked.iter().count(), 1001);
    }

    #[test]
    fn overlay_overrides_and_removals_read_through_correctly() {
        let base = theme(300);
        let mut a = base.clone();
        a.insert("--theme-1".into(), "red".into());
        a.remove("--theme-2");
        a.insert("--extra".into(), "x".into());
        assert_eq!(a.get("--theme-1"), Some("red"));
        assert_eq!(a.get("--theme-2"), None);
        assert!(!a.contains_key("--theme-2"));
        assert_eq!(a.len(), 300);
        assert_eq!(a.iter().count(), 300);
        assert_eq!(a.keys().filter(|k| *k == "--theme-1").count(), 1);
        // Re-adding a removed base key and removing an overlay-only key.
        a.insert("--theme-2".into(), "back".into());
        a.remove("--extra");
        assert_eq!(a.get("--theme-2"), Some("back"));
        assert_eq!(a.len(), 300);
        assert_eq!(a, flat_copy(&a));
        assert_eq!(base.get("--theme-1"), Some("#000001"));
        assert_eq!(base.len(), 300);
        // Restoring the base value makes it equal to the base again.
        a.insert("--theme-1".into(), "#000001".into());
        a.insert("--theme-2".into(), "#000002".into());
        assert_eq!(a, base);
    }

    #[test]
    fn overlay_fingerprint_matches_a_full_rehash_after_every_write() {
        let base = theme(200);
        assert_eq!(base.0.fingerprint(), full_rehash(&base));
        let mut a = base.clone();
        a.insert("--theme-3".into(), "z".into());
        assert_eq!(a.0.fingerprint(), full_rehash(&a));
        a.remove("--theme-4");
        assert_eq!(a.0.fingerprint(), full_rehash(&a));
        a.retain(|property, _| property != "--theme-5");
        assert_eq!(a.0.fingerprint(), full_rehash(&a));
        // A shared overlay unshares by copying only its own entries.
        let b = a.clone();
        let mut c = a.clone();
        c.insert("--late".into(), "1".into());
        assert!(c.0.overlay.is_some());
        assert_eq!(c.0.fingerprint(), full_rehash(&c));
        assert_eq!(b, a);
        assert_ne!(c, a);
        // An overlay built without a primed base fingerprint hashes correctly.
        let fresh = theme(200);
        let mut d = fresh.clone();
        d.insert("--late".into(), "1".into());
        assert_eq!(d.0.fingerprint(), full_rehash(&d));
    }

    #[test]
    fn retain_on_an_overlay_drops_base_and_own_entries() {
        let mut base = theme(150);
        base.insert("--w3cos-internal-a".into(), "1".into());
        let mut child = base.clone();
        child.insert("--w3cos-internal-b".into(), "2".into());
        child.retain(|property, _| !property.starts_with("--w3cos-internal-"));
        assert!(child.0.overlay.is_some());
        assert_eq!(child.len(), 150);
        assert!(!child.contains_key("--w3cos-internal-a"));
        assert!(!child.contains_key("--w3cos-internal-b"));
        assert_eq!(child, theme(150));
        assert_eq!(base.get("--w3cos-internal-a"), Some("1"));
    }

    #[test]
    fn large_overlays_fold_back_into_a_flat_map() {
        let base = theme(500);
        let mut a = base.clone();
        for i in 0..=OVERLAY_MAX_LOCAL {
            a.insert(format!("--own-{i}"), i.to_string());
        }
        assert!(a.0.overlay.is_none());
        assert_eq!(a.len(), 500 + OVERLAY_MAX_LOCAL + 1);
        assert_eq!(a.0.fingerprint(), full_rehash(&a));
        assert_eq!(a, flat_copy(&a));
        assert_eq!(base.len(), 500);
    }

    #[test]
    fn small_maps_still_unshare_by_copying() {
        let base = theme(OVERLAY_MIN_BASE - 1);
        let mut a = base.clone();
        a.insert("--x".into(), "1".into());
        assert!(a.0.overlay.is_none());
        assert_eq!(a.len(), OVERLAY_MIN_BASE);
    }
}
