//! Compact property storage for page-local [`crate::JsObject`]s.
//!
//! Empty maps are a null pointer (no heap). Up to [`INLINE_CAP`] entries live
//! in one small `Box` in insertion order. Larger objects spill to
//! `HashMap` + order `Vec` (same representation as before).
//!
//! This cuts the empty-object tax inside the 544B object slab (HashMap+Vec
//! control words → 8-byte `Option<Box<_>>`) and avoids HashMap bucket / Vec
//! buffers while objects stay small.

use std::collections::HashMap;

/// Spilled storage hashes with Fx (V8 hashes property names with a cheap
/// string hash cached on the name); SipHash dominated every spilled lookup.
type SpillMap = crate::fxhash::FxHashMap<JsString, Value>;

use crate::js_string::JsString;
use crate::value::Value;

/// Inline entries before spilling to `HashMap`. Four covers common object
/// literals / instances without growing the spilled path's cold size much;
/// `(JsString, Value)` is 32B (thin string + two-word Value) so the inline box is ~128B.
pub(crate) const INLINE_CAP: usize = 4;

#[derive(Clone)]
pub(crate) struct PropertyMap {
    inner: Option<Box<PropertyStore>>,
}

#[derive(Clone)]
enum PropertyStore {
    Inline {
        len: u8,
        entries: [(JsString, Value); INLINE_CAP],
    },
    Map {
        map: SpillMap,
        order: Vec<JsString>,
    },
}

fn map_contains_prefixed(map: &SpillMap, prefix: &str, key: &str) -> bool {
    const STACK_KEY_BYTES: usize = 64;
    let total = prefix.len().saturating_add(key.len());
    if total <= STACK_KEY_BYTES {
        let mut bytes = [0_u8; STACK_KEY_BYTES];
        bytes[..prefix.len()].copy_from_slice(prefix.as_bytes());
        bytes[prefix.len()..total].copy_from_slice(key.as_bytes());
        // Concatenating two valid UTF-8 strings is valid UTF-8.
        let combined = unsafe { std::str::from_utf8_unchecked(&bytes[..total]) };
        map.contains_key(combined)
    } else {
        let mut combined = String::with_capacity(total);
        combined.push_str(prefix);
        combined.push_str(key);
        map.contains_key(combined.as_str())
    }
}

impl PropertyMap {
    #[inline]
    pub(crate) fn new() -> Self {
        Self { inner: None }
    }

    pub(crate) fn from_interned_hashmap(mut map: HashMap<JsString, Value>) -> Self {
        if map.is_empty() {
            return Self::new();
        }
        if map.len() <= INLINE_CAP {
            let mut order: Vec<JsString> = map.keys().cloned().collect();
            // Match prior `from_interned_map` behavior: sorted key order when
            // building from an unordered HashMap (object literals with known
            // keys still go through set_direct / insertion order elsewhere).
            order.sort();
            let mut entries: [(JsString, Value); INLINE_CAP] =
                std::array::from_fn(|_| (JsString::intern(""), Value::Undefined));
            let len = order.len() as u8;
            for (i, key) in order.into_iter().enumerate() {
                let value = map.remove(&key).expect("key from order");
                entries[i] = (key, value);
            }
            return Self {
                inner: Some(Box::new(PropertyStore::Inline { len, entries })),
            };
        }
        let mut order: Vec<JsString> = map.keys().cloned().collect();
        order.sort();
        let map: SpillMap = map.into_iter().collect();
        Self {
            inner: Some(Box::new(PropertyStore::Map { map, order })),
        }
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self.inner.as_deref() {
            None => 0,
            Some(PropertyStore::Inline { len, .. }) => *len as usize,
            Some(PropertyStore::Map { map, .. }) => map.len(),
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_none()
    }

    /// True when no HashMap/Vec buffers are allocated (empty or inline).
    #[inline]
    pub(crate) fn is_compact(&self) -> bool {
        !matches!(self.inner.as_deref(), Some(PropertyStore::Map { .. }))
    }

    /// Heap bytes beyond the `Option<Box>` word itself.
    pub(crate) fn heap_bytes(&self) -> usize {
        match self.inner.as_deref() {
            None => 0,
            Some(store) => std::mem::size_of_val(store).saturating_add(match store {
                PropertyStore::Inline { .. } => 0,
                PropertyStore::Map { map, order } => map
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(JsString, Value)>())
                    .saturating_add(map.len().saturating_mul(std::mem::size_of::<JsString>()))
                    .saturating_add(
                        order
                            .capacity()
                            .saturating_mul(std::mem::size_of::<JsString>()),
                    ),
            }),
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        match self.inner.as_deref() {
            None => None,
            Some(PropertyStore::Inline { len, entries }) => entries[..*len as usize]
                .iter()
                .find(|(k, _)| k.as_str() == key)
                .map(|(_, v)| v),
            Some(PropertyStore::Map { map, .. }) => map.get(key),
        }
    }

    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Check a direct key and its two prefixed accessor forms. Compact maps
    /// scan their at-most-four entries once, avoiding three separate scans and
    /// any temporary combined key. Spilled maps retain exact borrowed HashMap
    /// lookups and the stack/fallback prefix construction.
    pub(crate) fn contains_key_or_prefixed(
        &self,
        key: &str,
        first_prefix: &str,
        second_prefix: &str,
    ) -> bool {
        match self.inner.as_deref() {
            None => false,
            Some(PropertyStore::Inline { len, entries }) => {
                entries[..*len as usize].iter().any(|(candidate, _)| {
                    let candidate = candidate.as_str();
                    candidate == key
                        || candidate.strip_prefix(first_prefix) == Some(key)
                        || candidate.strip_prefix(second_prefix) == Some(key)
                })
            }
            Some(PropertyStore::Map { map, .. }) => {
                map.contains_key(key)
                    || map_contains_prefixed(map, first_prefix, key)
                    || map_contains_prefixed(map, second_prefix, key)
            }
        }
    }

    pub(crate) fn insert(&mut self, key: JsString, value: Value) -> Option<Value> {
        if self.inner.is_none() {
            let mut entries: [(JsString, Value); INLINE_CAP] =
                std::array::from_fn(|_| (JsString::intern(""), Value::Undefined));
            entries[0] = (key, value);
            self.inner = Some(Box::new(PropertyStore::Inline { len: 1, entries }));
            return None;
        }

        // Spill path may replace `inner`; handle Inline specially first.
        let spill = {
            let store = self.inner.as_mut().expect("checked");
            match store.as_mut() {
                PropertyStore::Inline { len, entries } => {
                    let n = *len as usize;
                    if let Some(slot) = entries[..n]
                        .iter_mut()
                        .find(|(k, _)| k.as_str() == key.as_str())
                    {
                        return Some(std::mem::replace(&mut slot.1, value));
                    }
                    if n < INLINE_CAP {
                        entries[n] = (key, value);
                        *len += 1;
                        return None;
                    }
                    let mut map =
                        SpillMap::with_capacity_and_hasher(INLINE_CAP + 1, Default::default());
                    let mut order = Vec::with_capacity(INLINE_CAP + 1);
                    for i in 0..n {
                        let (k, v) = std::mem::replace(
                            &mut entries[i],
                            (JsString::intern(""), Value::Undefined),
                        );
                        order.push(k.clone());
                        map.insert(k, v);
                    }
                    order.push(key.clone());
                    map.insert(key, value);
                    Some((map, order))
                }
                PropertyStore::Map { map, order } => {
                    if !map.contains_key(key.as_str()) {
                        order.push(key.clone());
                    }
                    return map.insert(key, value);
                }
            }
        };
        if let Some((map, order)) = spill {
            self.inner = Some(Box::new(PropertyStore::Map { map, order }));
        }
        None
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<Value> {
        match self.inner.as_mut() {
            None => None,
            Some(store) => match store.as_mut() {
                PropertyStore::Inline { len, entries } => {
                    let n = *len as usize;
                    let idx = entries[..n].iter().position(|(k, _)| k.as_str() == key)?;
                    let (_, removed) = std::mem::replace(
                        &mut entries[idx],
                        (JsString::intern(""), Value::Undefined),
                    );
                    for i in idx..n.saturating_sub(1) {
                        entries[i] = std::mem::replace(
                            &mut entries[i + 1],
                            (JsString::intern(""), Value::Undefined),
                        );
                    }
                    *len -= 1;
                    if *len == 0 {
                        self.inner = None;
                    }
                    Some(removed)
                }
                PropertyStore::Map { map, order } => {
                    let removed = map.remove(key)?;
                    order.retain(|candidate| candidate.as_str() != key);
                    if map.is_empty() {
                        self.inner = None;
                    }
                    Some(removed)
                }
            },
        }
    }

    pub(crate) fn keys(&self) -> PropertyMapKeys<'_> {
        PropertyMapKeys {
            map: self,
            index: 0,
        }
    }

    pub(crate) fn any_key(&self, mut f: impl FnMut(&JsString) -> bool) -> bool {
        match self.inner.as_deref() {
            None => false,
            Some(PropertyStore::Inline { len, entries }) => {
                entries[..*len as usize].iter().any(|(k, _)| f(k))
            }
            Some(PropertyStore::Map { order, .. }) => order.iter().any(f),
        }
    }
}

impl Default for PropertyMap {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) struct PropertyMapKeys<'a> {
    map: &'a PropertyMap,
    index: usize,
}

impl<'a> Iterator for PropertyMapKeys<'a> {
    type Item = &'a JsString;

    fn next(&mut self) -> Option<Self::Item> {
        match self.map.inner.as_deref() {
            None => None,
            Some(PropertyStore::Inline { len, entries }) => {
                if self.index >= *len as usize {
                    return None;
                }
                let key = &entries[self.index].0;
                self.index += 1;
                Some(key)
            }
            Some(PropertyStore::Map { order, .. }) => {
                let key = order.get(self.index)?;
                self.index += 1;
                Some(key)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_map_is_null_and_compact() {
        let map = PropertyMap::new();
        assert!(map.is_empty());
        assert!(map.is_compact());
        assert_eq!(map.heap_bytes(), 0);
        assert_eq!(
            std::mem::size_of::<PropertyMap>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn small_inserts_stay_inline_without_hashmap() {
        let mut map = PropertyMap::new();
        for i in 0..INLINE_CAP {
            let key = format!("k{i}");
            assert!(
                map.insert(JsString::intern(&key), Value::Number(i as f64))
                    .is_none()
            );
            assert!(map.is_compact(), "still compact at {i}");
            assert_eq!(map.get(&key).map(|v| v.to_number()), Some(i as f64));
        }
        assert_eq!(map.len(), INLINE_CAP);
        // Fifth insert spills.
        map.insert(JsString::intern("spill"), Value::Bool(true));
        assert!(!map.is_compact());
        assert_eq!(map.len(), INLINE_CAP + 1);
        assert_eq!(map.get("k0").map(|v| v.to_number()), Some(0.0));
        assert_eq!(map.get("spill").map(|v| v.to_bool()), Some(true));
        let keys: Vec<_> = map.keys().map(|k| k.as_str().to_string()).collect();
        assert_eq!(keys.last().map(String::as_str), Some("spill"));
    }

    #[test]
    fn combined_lookup_matches_inline_spilled_unicode_and_fallback_keys() {
        let mut map = PropertyMap::new();
        let getter_prefix = "__w3cos_getter_";
        let setter_prefix = "__w3cos_setter_";
        map.insert(
            JsString::intern(&format!("{getter_prefix}值")),
            Value::Bool(true),
        );
        map.insert(JsString::intern("direct"), Value::Bool(true));
        assert!(map.contains_key_or_prefixed("值", getter_prefix, setter_prefix));
        assert!(map.contains_key_or_prefixed("direct", getter_prefix, setter_prefix));
        assert!(!map.contains_key_or_prefixed("missing", getter_prefix, setter_prefix));

        for key in ["fill-a", "fill-b", "fill-c"] {
            map.insert(JsString::intern(key), Value::Undefined);
        }
        assert!(!map.is_compact());
        assert!(map.contains_key_or_prefixed("值", getter_prefix, setter_prefix));
        assert!(map.contains_key_or_prefixed("fill-a", getter_prefix, setter_prefix));

        let boundary_64 = "a".repeat(64 - getter_prefix.len());
        let boundary_65 = "b".repeat(65 - getter_prefix.len());
        map.insert(
            JsString::intern(&format!("{getter_prefix}{boundary_64}")),
            Value::Bool(true),
        );
        map.insert(
            JsString::intern(&format!("{getter_prefix}{boundary_65}")),
            Value::Bool(true),
        );
        assert!(map.contains_key_or_prefixed(&boundary_64, getter_prefix, setter_prefix));
        assert!(map.contains_key_or_prefixed(&boundary_65, getter_prefix, setter_prefix));

        let long = "x".repeat(128);
        map.insert(
            JsString::intern(&format!("__w3cos_setter_{long}")),
            Value::Bool(true),
        );
        assert!(map.contains_key_or_prefixed(&long, getter_prefix, setter_prefix));
        assert!(!map.contains_key_or_prefixed("other", getter_prefix, setter_prefix));
    }

    #[test]
    fn remove_back_to_empty_drops_box() {
        let mut map = PropertyMap::new();
        map.insert(JsString::intern("only"), Value::Number(1.0));
        assert!(map.is_compact());
        assert!(map.remove("only").is_some());
        assert!(map.is_empty());
        assert_eq!(map.heap_bytes(), 0);
    }

    #[test]
    fn overwrite_preserves_insertion_order() {
        let mut map = PropertyMap::new();
        map.insert(JsString::intern("a"), Value::Number(1.0));
        map.insert(JsString::intern("b"), Value::Number(2.0));
        map.insert(JsString::intern("a"), Value::Number(3.0));
        let keys: Vec<_> = map.keys().map(|k| k.as_str().to_string()).collect();
        assert_eq!(keys, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(map.get("a").map(|v| v.to_number()), Some(3.0));
    }
}
