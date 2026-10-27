//! Shared, copy-on-write storage for CSS custom properties.
//!
//! Workbench themes define hundreds of `--vscode-*` variables on the root
//! and every element inherits them. Holding the map behind an `Arc` makes
//! the per-element `Style` clone and the inherited-style equality check
//! O(1) when the map is shared, instead of copying and comparing every
//! entry; mutation clones the map only while it is still shared.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct CustomProperties(Arc<HashMap<String, String>>);

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
        if self.0.get(&property) == Some(&value) {
            return Some(value);
        }
        Arc::make_mut(&mut self.0).insert(property, value)
    }

    /// Remove a property, unsharing the map only when it is present.
    pub fn remove<Q>(&mut self, property: &Q) -> Option<String>
    where
        String: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        if !self.0.contains_key(property) {
            return None;
        }
        Arc::make_mut(&mut self.0).remove(property)
    }

    /// Retain matching properties, unsharing the map only when one goes.
    pub fn retain(&mut self, mut keep: impl FnMut(&String, &String) -> bool) {
        if self
            .0
            .iter()
            .any(|(property, value)| !keep(property, value))
        {
            Arc::make_mut(&mut self.0).retain(|property, value| keep(property, value));
        }
    }

    /// The owned map, cloning it only when it is shared.
    pub fn into_inner(self) -> HashMap<String, String> {
        Arc::try_unwrap(self.0).unwrap_or_else(|shared| (*shared).clone())
    }
}

impl std::ops::Deref for CustomProperties {
    type Target = HashMap<String, String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for CustomProperties {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl PartialEq for CustomProperties {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other) || *self.0 == *other.0
    }
}

impl Eq for CustomProperties {}

impl std::fmt::Debug for CustomProperties {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<HashMap<String, String>> for CustomProperties {
    fn from(map: HashMap<String, String>) -> Self {
        Self(Arc::new(map))
    }
}

impl From<CustomProperties> for HashMap<String, String> {
    fn from(map: CustomProperties) -> Self {
        map.into_inner()
    }
}

impl FromIterator<(String, String)> for CustomProperties {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Self(Arc::new(iter.into_iter().collect()))
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
        self.0.iter()
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
        self.0.serialize(serializer)
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
}
