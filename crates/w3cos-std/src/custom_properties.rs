//! Shared, copy-on-write storage for CSS custom properties.
//!
//! Workbench themes define hundreds of `--vscode-*` variables on the root
//! and every element inherits them. Holding the map behind an `Arc` makes
//! the per-element `Style` clone and the inherited-style equality check
//! O(1) when the map is shared, instead of copying and comparing every
//! entry; mutation clones the map only while it is still shared.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// The shared map plus a lazily computed content fingerprint. Any mutation
/// resets the fingerprint, so it always describes the current entries.
#[derive(Default)]
struct Inner {
    map: HashMap<String, String>,
    fingerprint: OnceLock<u64>,
}

impl Clone for Inner {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
            fingerprint: OnceLock::new(),
        }
    }
}

impl Inner {
    fn from_map(map: HashMap<String, String>) -> Self {
        Self {
            map,
            fingerprint: OnceLock::new(),
        }
    }

    /// Order-independent hash of every entry. Two maps with different
    /// fingerprints are different; equal fingerprints still need a compare.
    fn fingerprint(&self) -> u64 {
        *self.fingerprint.get_or_init(|| {
            fn bytes(seed: u64, text: &str) -> u64 {
                let mut hash = seed ^ 0xcbf2_9ce4_8422_2325;
                for byte in text.as_bytes() {
                    hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
                }
                hash
            }
            fn finish(mut value: u64) -> u64 {
                value ^= value >> 30;
                value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
                value ^= value >> 27;
                value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
                value ^ (value >> 31)
            }
            self.map.iter().fold(self.map.len() as u64, |acc, (k, v)| {
                acc.wrapping_add(finish(bytes(bytes(1, k), v)))
            })
        })
    }
}

#[derive(Clone, Default)]
pub struct CustomProperties(Arc<Inner>);

impl CustomProperties {
    /// Unique access to the map: unshares it and drops the stale fingerprint.
    fn map_mut(&mut self) -> &mut HashMap<String, String> {
        let inner = Arc::make_mut(&mut self.0);
        inner.fingerprint = OnceLock::new();
        &mut inner.map
    }
}

impl CustomProperties {
    pub fn new() -> Self {
        Self::default()
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
        self.map_mut().insert(property, value)
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
        self.map_mut().remove(property)
    }

    /// Retain matching properties, unsharing the map only when one goes.
    pub fn retain(&mut self, mut keep: impl FnMut(&String, &String) -> bool) {
        if self
            .0
            .map
            .iter()
            .any(|(property, value)| !keep(property, value))
        {
            self.map_mut()
                .retain(|property, value| keep(property, value));
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
            && self.0.map == other.0.map
    }
}

impl Eq for CustomProperties {}

impl std::fmt::Debug for CustomProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.map.fmt(f)
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
        Self(Arc::new(Inner::from_map(iter.into_iter().collect())))
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
        HashMap::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
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
}
