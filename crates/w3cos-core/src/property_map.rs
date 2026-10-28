//! Shape-based property storage for [`crate::JsObject`]s and function objects.
//!
//! An ordinary object stores its own string-keyed properties as a pointer to
//! a shared [`Shape`] (hidden class) plus a flat `Vec<Value>` of slots. A
//! shape is a node in a per-thread transition tree keyed by property name:
//! objects that receive the same keys in the same order share one shape, so
//! the key list (and a key -> slot index for wide shapes) is stored once.
//! Each shape has a process-unique [`ShapeId`], which is what AOT inline
//! caches compare.
//!
//! Objects fall back to *dictionary mode* (object-local `HashMap` + ordered
//! entry list, the pre-shape representation) when a property is deleted,
//! when they grow past [`MAX_SHAPE_KEYS`], or when the thread's shape budget
//! ([`MAX_SHAPES`]) is exhausted. Dictionary objects never hit an IC.
//!
//! Shapes are an append-only cache: they are never freed (bounded by
//! [`MAX_SHAPES`]), hold no `Value`s, and store their keys as heap strings so
//! they survive `page_arena::reset`. They therefore never keep a JS value
//! alive and do not affect refcount / weak semantics.
//!
//! Key enumeration follows ECMAScript `OrdinaryOwnPropertyKeys` for string
//! keys: canonical array-index keys first in ascending numeric order, then
//! the remaining keys in insertion order.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;

/// Hash map for the engine's own key tables: keys are script property names
/// hashed on every lookup, so use the fast Fx hasher instead of SipHash. These
/// tables are never exposed to attacker-chosen hasher state, and a collision
/// only costs lookup time.
type FastMap<K, V> = HashMap<K, V, rustc_hash::FxBuildHasher>;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::js_string::JsString;
use crate::value::Value;

/// Own keys before an object leaves the shape tree for dictionary mode.
pub const MAX_SHAPE_KEYS: usize = 64;
/// Shapes one thread may create before new transitions go to dictionary mode.
pub const MAX_SHAPES: usize = 1 << 15;
/// Prefix of internal property names (`__w3cos_getter_k`, `__w3cos_setter_k`).
pub(crate) const MARKER_PREFIX: &str = "__w3cos_";
/// Shapes with at most this many keys are searched linearly.
const LINEAR_SCAN: usize = 8;
/// Transition lists longer than this get a hash index.
const LINEAR_TRANSITIONS: usize = 8;

/// Process-unique shape identity. `0` never names a shape (IC "empty").
/// [`ROOT_SHAPE_ID`] is the empty shape on every thread (the key list is
/// empty, so sharing the id across threads is sound).
pub type ShapeId = u32;
pub const ROOT_SHAPE_ID: ShapeId = 1;
static NEXT_SHAPE_ID: AtomicU32 = AtomicU32::new(ROOT_SHAPE_ID + 1);

/// True when `key` is a canonical array index (`"0"`, `"17"`, not `"01"`).
pub(crate) fn is_index_key(key: &str) -> bool {
    index_key_value(key).is_some()
}

fn index_key_value(key: &str) -> Option<u32> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 {
        return None;
    }
    if bytes[0] == b'0' {
        return (bytes.len() == 1).then_some(0);
    }
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    // Array indices are < 2^32 - 1.
    (value < u32::MAX as u64).then_some(value as u32)
}

enum Transitions {
    Few(Vec<(JsString, Rc<Shape>)>),
    Many(FastMap<JsString, Rc<Shape>>),
}

/// A hidden class: an ordered own-key list shared by many objects.
pub(crate) struct Shape {
    id: ShapeId,
    /// Insertion-ordered own keys; slot `i` of an object holds `keys[i]`.
    keys: Box<[JsString]>,
    /// Key -> slot for shapes wider than [`LINEAR_SCAN`], built on first use.
    index: OnceCell<FastMap<JsString, u32>>,
    /// Any key is a canonical array index (enumeration must reorder).
    has_index_key: bool,
    /// Any key is an internal `__w3cos_` marker (accessor halves etc.).
    has_marker_key: bool,
    transitions: RefCell<Transitions>,
}

impl Shape {
    fn root() -> Self {
        Self {
            id: ROOT_SHAPE_ID,
            keys: Box::new([]),
            index: OnceCell::new(),
            has_index_key: false,
            has_marker_key: false,
            transitions: RefCell::new(Transitions::Few(Vec::new())),
        }
    }

    #[inline]
    pub(crate) fn lookup(&self, key: &str) -> Option<usize> {
        if self.keys.len() <= LINEAR_SCAN {
            return self.keys.iter().position(|k| k.as_str() == key);
        }
        self.index
            .get_or_init(|| {
                self.keys
                    .iter()
                    .enumerate()
                    .map(|(i, k)| (k.clone(), i as u32))
                    .collect()
            })
            .get(key)
            .map(|&i| i as usize)
    }

    /// Child shape with `key` appended, creating it if the budget allows.
    fn transition(self: &Rc<Self>, key: &str) -> Option<Rc<Shape>> {
        {
            let transitions = self.transitions.borrow();
            let found = match &*transitions {
                Transitions::Few(list) => list
                    .iter()
                    .find(|(k, _)| k.as_str() == key)
                    .map(|(_, shape)| Rc::clone(shape)),
                Transitions::Many(map) => map.get(key).cloned(),
            };
            if found.is_some() {
                return found;
            }
        }
        if self.keys.len() >= MAX_SHAPE_KEYS {
            return None;
        }
        let count = SHAPE_COUNT.with(|count| {
            let next = count.get() + 1;
            count.set(next);
            next
        });
        if count > MAX_SHAPES {
            return None;
        }
        let key = JsString::heap(key);
        let mut keys = Vec::with_capacity(self.keys.len() + 1);
        keys.extend(self.keys.iter().cloned());
        keys.push(key.clone());
        let child = Rc::new(Shape {
            id: NEXT_SHAPE_ID.fetch_add(1, Ordering::Relaxed),
            has_index_key: self.has_index_key || is_index_key(key.as_str()),
            has_marker_key: self.has_marker_key || key.as_str().starts_with(MARKER_PREFIX),
            keys: keys.into_boxed_slice(),
            index: OnceCell::new(),
            transitions: RefCell::new(Transitions::Few(Vec::new())),
        });
        let mut transitions = self.transitions.borrow_mut();
        match &mut *transitions {
            Transitions::Few(list) => {
                list.push((key, Rc::clone(&child)));
                if list.len() > LINEAR_TRANSITIONS {
                    let map = std::mem::take(list).into_iter().collect();
                    *transitions = Transitions::Many(map);
                }
            }
            Transitions::Many(map) => {
                map.insert(key, Rc::clone(&child));
            }
        }
        Some(child)
    }
}

thread_local! {
    static ROOT: Rc<Shape> = Rc::new(Shape::root());
    static SHAPE_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// Shapes created on this thread (tests / diagnostics).
pub fn shape_count() -> usize {
    SHAPE_COUNT.with(Cell::get).min(MAX_SHAPES)
}

fn root_shape() -> Rc<Shape> {
    ROOT.with(Rc::clone)
}

#[derive(Clone)]
pub(crate) struct PropertyMap {
    inner: Option<Box<PropertyStore>>,
}

#[derive(Clone)]
enum PropertyStore {
    Shaped {
        shape: Rc<Shape>,
        slots: Vec<Value>,
    },
    Dict {
        map: FastMap<JsString, Value>,
        order: Vec<JsString>,
        has_index_key: bool,
    },
}

impl PropertyMap {
    #[inline]
    pub(crate) fn new() -> Self {
        Self { inner: None }
    }

    pub(crate) fn from_interned_hashmap(mut map: HashMap<JsString, Value>) -> Self {
        let mut out = Self::new();
        if map.is_empty() {
            return out;
        }
        // An unordered HashMap has no insertion order: use sorted key order
        // (deterministic, and shared by every literal with the same keys).
        let mut order: Vec<JsString> = map.keys().cloned().collect();
        order.sort();
        let mut slots = Vec::with_capacity(order.len());
        let mut shape = Some(root_shape());
        for key in &order {
            shape = shape.and_then(|shape| shape.transition(key.as_str()));
            if shape.is_none() {
                break;
            }
            slots.push(map.remove(key).expect("key from order"));
        }
        if let Some(shape) = shape {
            out.inner = Some(Box::new(PropertyStore::Shaped { shape, slots }));
            return out;
        }
        // Too wide or out of shape budget: dictionary from the start.
        // `map` still holds the keys the shape walk did not consume.
        let mut map: FastMap<JsString, Value> = map.into_iter().collect();
        map.reserve(order.len());
        for (key, value) in order.iter().zip(slots) {
            map.insert(key.clone(), value);
        }
        let map_order: Vec<JsString> = order;
        let has_index_key = map_order.iter().any(|key| is_index_key(key.as_str()));
        out.inner = Some(Box::new(PropertyStore::Dict {
            map,
            order: map_order,
            has_index_key,
        }));
        out
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        match self.inner.as_deref() {
            None => 0,
            Some(PropertyStore::Shaped { slots, .. }) => slots.len(),
            Some(PropertyStore::Dict { map, .. }) => map.len(),
        }
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_none()
    }

    /// True when the map is empty or shape-backed (no per-object hash table).
    #[inline]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_compact(&self) -> bool {
        !matches!(self.inner.as_deref(), Some(PropertyStore::Dict { .. }))
    }

    /// True once the map has left the shape tree.
    #[inline]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn is_dictionary(&self) -> bool {
        !self.is_compact()
    }

    /// Shape id for IC checks: [`ROOT_SHAPE_ID`] when empty, `None` in
    /// dictionary mode.
    #[inline]
    pub(crate) fn shape_id(&self) -> Option<ShapeId> {
        match self.inner.as_deref() {
            None => Some(ROOT_SHAPE_ID),
            Some(PropertyStore::Shaped { shape, .. }) => Some(shape.id),
            Some(PropertyStore::Dict { .. }) => None,
        }
    }

    /// `(shape id, slot)` of an own key in a shape-backed map.
    #[inline]
    pub(crate) fn shaped_slot(&self, key: &str) -> Option<(ShapeId, usize)> {
        match self.inner.as_deref() {
            Some(PropertyStore::Shaped { shape, .. }) => Some((shape.id, shape.lookup(key)?)),
            _ => None,
        }
    }

    /// Slot value when the map currently has shape `id` (IC hit path).
    #[inline]
    pub(crate) fn slot_if_shape(&self, id: ShapeId, slot: usize) -> Option<&Value> {
        match self.inner.as_deref() {
            Some(PropertyStore::Shaped { shape, slots }) if shape.id == id => slots.get(slot),
            _ => None,
        }
    }

    /// Mutable slot when the map currently has shape `id` (IC hit path).
    #[inline]
    pub(crate) fn slot_mut_if_shape(&mut self, id: ShapeId, slot: usize) -> Option<&mut Value> {
        match self.inner.as_deref_mut() {
            Some(PropertyStore::Shaped { shape, slots }) if shape.id == id => slots.get_mut(slot),
            _ => None,
        }
    }

    /// Heap bytes owned by this object beyond the `Option<Box>` word. The
    /// shared shape is not charged to any one object.
    pub(crate) fn heap_bytes(&self) -> usize {
        match self.inner.as_deref() {
            None => 0,
            Some(store) => std::mem::size_of_val(store).saturating_add(match store {
                PropertyStore::Shaped { slots, .. } => slots
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Value>()),
                PropertyStore::Dict { map, order, .. } => map
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

    #[inline]
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        match self.inner.as_deref() {
            None => None,
            Some(PropertyStore::Shaped { shape, slots }) => {
                shape.lookup(key).and_then(|i| slots.get(i))
            }
            Some(PropertyStore::Dict { map, .. }) => map.get(key),
        }
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self.inner.as_deref_mut() {
            None => None,
            Some(PropertyStore::Shaped { shape, slots }) => {
                let i = shape.lookup(key)?;
                slots.get_mut(i)
            }
            Some(PropertyStore::Dict { map, .. }) => map.get_mut(key),
        }
    }

    #[inline]
    pub(crate) fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub(crate) fn insert(&mut self, key: JsString, value: Value) -> Option<Value> {
        if let Some(slot) = self.get_mut(key.as_str()) {
            return Some(std::mem::replace(slot, value));
        }
        self.add_new(key.as_str(), Some(key.clone()), value);
        None
    }

    /// Insert by `&str`; only allocates a key string when the key is new.
    pub(crate) fn insert_str(&mut self, key: &str, value: Value) -> Option<Value> {
        if let Some(slot) = self.get_mut(key) {
            return Some(std::mem::replace(slot, value));
        }
        self.add_new(key, None, value);
        None
    }

    /// Append a key known to be absent.
    fn add_new(&mut self, key: &str, interned: Option<JsString>, value: Value) {
        let store = match self.inner.as_deref_mut() {
            None => {
                let root = root_shape();
                match root.transition(key) {
                    Some(shape) => {
                        let mut slots = Vec::with_capacity(4);
                        slots.push(value);
                        self.inner = Some(Box::new(PropertyStore::Shaped { shape, slots }));
                        return;
                    }
                    None => {
                        self.inner = Some(Box::new(PropertyStore::Dict {
                            map: FastMap::default(),
                            order: Vec::new(),
                            has_index_key: false,
                        }));
                        self.inner.as_deref_mut().expect("just set")
                    }
                }
            }
            Some(store) => store,
        };
        if let PropertyStore::Shaped { shape, slots } = store {
            if let Some(next) = shape.transition(key) {
                *shape = next;
                slots.push(value);
                return;
            }
            store.make_dictionary();
        }
        let PropertyStore::Dict {
            map,
            order,
            has_index_key,
        } = store
        else {
            unreachable!("dictionary after make_dictionary");
        };
        let key = interned.unwrap_or_else(|| JsString::intern(key));
        *has_index_key |= is_index_key(key.as_str());
        order.push(key.clone());
        map.insert(key, value);
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<Value> {
        let only_key = match self.inner.as_deref()? {
            PropertyStore::Shaped { shape, slots } => {
                shape.lookup(key)?;
                slots.len() == 1
            }
            PropertyStore::Dict { .. } => false,
        };
        if only_key {
            return match *self.inner.take()? {
                PropertyStore::Shaped { mut slots, .. } => slots.pop(),
                PropertyStore::Dict { .. } => None,
            };
        }
        let store = self.inner.as_deref_mut()?;
        // Deletes leave the shape tree (dictionary mode), so a stale IC can
        // never resolve a slot index against the shifted slot layout.
        store.make_dictionary();
        let PropertyStore::Dict { map, order, .. } = store else {
            unreachable!("dictionary after make_dictionary");
        };
        let removed = map.remove(key)?;
        if let Some(pos) = order.iter().position(|k| k.as_str() == key) {
            order.remove(pos);
        }
        if map.is_empty() {
            self.inner = None;
        }
        Some(removed)
    }

    pub(crate) fn keys(&self) -> PropertyMapKeys<'_> {
        let (base, has_index): (&[JsString], bool) = match self.inner.as_deref() {
            None => (&[], false),
            Some(PropertyStore::Shaped { shape, .. }) => (&shape.keys, shape.has_index_key),
            Some(PropertyStore::Dict {
                order,
                has_index_key,
                ..
            }) => (order.as_slice(), *has_index_key),
        };
        if !has_index {
            return PropertyMapKeys::Plain(base.iter());
        }
        let mut indexed: Vec<(u32, &JsString)> = Vec::new();
        let mut named: Vec<&JsString> = Vec::with_capacity(base.len());
        for key in base {
            match index_key_value(key.as_str()) {
                Some(index) => indexed.push((index, key)),
                None => named.push(key),
            }
        }
        indexed.sort_by_key(|(index, _)| *index);
        let mut ordered: Vec<&JsString> = indexed.into_iter().map(|(_, key)| key).collect();
        ordered.extend(named);
        PropertyMapKeys::Ordered(ordered.into_iter())
    }

    /// May contain an internal `__w3cos_` key (accessor halves). `false`
    /// is exact: no own key starts with [`MARKER_PREFIX`].
    #[inline]
    pub(crate) fn may_have_marker_key(&self) -> bool {
        match self.inner.as_deref() {
            None => false,
            Some(PropertyStore::Shaped { shape, .. }) => shape.has_marker_key,
            Some(PropertyStore::Dict { .. }) => true,
        }
    }

    pub(crate) fn any_key(&self, mut f: impl FnMut(&JsString) -> bool) -> bool {
        match self.inner.as_deref() {
            None => false,
            Some(PropertyStore::Shaped { shape, .. }) => shape.keys.iter().any(&mut f),
            Some(PropertyStore::Dict { order, .. }) => order.iter().any(f),
        }
    }
}

impl PropertyStore {
    fn make_dictionary(&mut self) {
        if let PropertyStore::Shaped { shape, slots } = self {
            let slots = std::mem::take(slots);
            let mut map = FastMap::with_capacity_and_hasher(slots.len() + 1, Default::default());
            let mut order = Vec::with_capacity(slots.len() + 1);
            for (key, value) in shape.keys.iter().zip(slots) {
                order.push(key.clone());
                map.insert(key.clone(), value);
            }
            *self = PropertyStore::Dict {
                map,
                order,
                has_index_key: shape.has_index_key,
            };
        }
    }
}

impl Default for PropertyMap {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) enum PropertyMapKeys<'a> {
    Plain(std::slice::Iter<'a, JsString>),
    Ordered(std::vec::IntoIter<&'a JsString>),
}

impl<'a> Iterator for PropertyMapKeys<'a> {
    type Item = &'a JsString;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            PropertyMapKeys::Plain(iter) => iter.next(),
            PropertyMapKeys::Ordered(iter) => iter.next(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(map: &PropertyMap) -> Vec<String> {
        map.keys().map(|k| k.as_str().to_string()).collect()
    }

    fn build(keys: &[&str]) -> PropertyMap {
        let mut map = PropertyMap::new();
        for (i, key) in keys.iter().enumerate() {
            map.insert_str(key, Value::Number(i as f64));
        }
        map
    }

    #[test]
    fn empty_map_is_null_and_compact() {
        let map = PropertyMap::new();
        assert!(map.is_empty());
        assert!(map.is_compact());
        assert_eq!(map.heap_bytes(), 0);
        assert_eq!(map.shape_id(), Some(ROOT_SHAPE_ID));
        assert_eq!(
            std::mem::size_of::<PropertyMap>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn same_key_order_shares_one_shape() {
        let a = build(&["x", "y", "z"]);
        let b = build(&["x", "y", "z"]);
        let c = build(&["y", "x", "z"]);
        assert!(a.is_compact() && b.is_compact() && c.is_compact());
        assert_eq!(a.shape_id(), b.shape_id());
        assert_ne!(a.shape_id(), c.shape_id());
        // Prefix shapes are the transition tree's interior nodes.
        let prefix = build(&["x", "y"]);
        assert_ne!(prefix.shape_id(), a.shape_id());
        assert_eq!(prefix.shape_id(), build(&["x", "y"]).shape_id());
        assert_eq!(a.shaped_slot("z").map(|(_, slot)| slot), Some(2));
        assert_eq!(c.shaped_slot("z").map(|(_, slot)| slot), Some(2));
        assert_eq!(c.shaped_slot("x").map(|(_, slot)| slot), Some(1));
    }

    #[test]
    fn overwrite_keeps_shape_and_insertion_order() {
        let mut map = build(&["a", "b"]);
        let shape = map.shape_id();
        assert!(map.insert_str("a", Value::Number(3.0)).is_some());
        assert_eq!(map.shape_id(), shape);
        assert_eq!(keys(&map), vec!["a", "b"]);
        assert_eq!(map.get("a").map(Value::to_number), Some(3.0));
    }

    #[test]
    fn wide_shapes_use_index_and_stay_compact() {
        let names: Vec<String> = (0..MAX_SHAPE_KEYS).map(|i| format!("k{i}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut map = build(&refs);
        assert!(map.is_compact());
        for (i, name) in names.iter().enumerate() {
            assert_eq!(map.get(name).map(Value::to_number), Some(i as f64));
        }
        // One past the limit goes to dictionary mode, order preserved.
        map.insert_str("overflow", Value::Bool(true));
        assert!(map.is_dictionary());
        assert_eq!(map.shape_id(), None);
        assert_eq!(keys(&map).last().map(String::as_str), Some("overflow"));
        assert_eq!(keys(&map).len(), MAX_SHAPE_KEYS + 1);
    }

    #[test]
    fn delete_goes_to_dictionary_and_keeps_order() {
        let mut map = build(&["a", "b", "c"]);
        assert!(map.remove("missing").is_none());
        assert!(map.is_compact(), "a failed delete keeps the shape");
        assert_eq!(map.remove("b").map(|v| v.to_number()), Some(1.0));
        assert!(map.is_dictionary());
        assert_eq!(map.shape_id(), None);
        assert_eq!(keys(&map), vec!["a", "c"]);
        map.insert_str("b", Value::Number(9.0));
        assert_eq!(keys(&map), vec!["a", "c", "b"]);
        assert!(map.shaped_slot("a").is_none());
    }

    #[test]
    fn remove_back_to_empty_drops_box() {
        let mut map = PropertyMap::new();
        map.insert(JsString::intern("only"), Value::Number(1.0));
        assert!(map.is_compact());
        assert!(map.remove("only").is_some());
        assert!(map.is_empty());
        assert_eq!(map.heap_bytes(), 0);
        assert_eq!(map.shape_id(), Some(ROOT_SHAPE_ID));
    }

    #[test]
    fn integer_keys_enumerate_first_in_ascending_order() {
        let map = build(&["b", "10", "a", "2", "01", "0", "4294967295"]);
        // "01" and 2^32-1 are not array indices: they keep insertion order.
        assert_eq!(
            keys(&map),
            vec!["0", "2", "10", "b", "a", "01", "4294967295"]
        );
        let mut dict = map.clone();
        dict.remove("a");
        assert!(dict.is_dictionary());
        assert_eq!(keys(&dict), vec!["0", "2", "10", "b", "01", "4294967295"]);
    }

    #[test]
    fn shape_keys_survive_page_reset() {
        let map = build(&["survives_reset_key"]);
        crate::page_arena::reset();
        assert_eq!(keys(&map), vec!["survives_reset_key"]);
        assert_eq!(
            map.get("survives_reset_key").map(Value::to_number),
            Some(0.0)
        );
    }

    #[test]
    fn ic_slot_accessors_check_shape() {
        let mut map = build(&["x", "y"]);
        let (shape, slot) = map.shaped_slot("y").expect("shaped");
        assert_eq!(
            map.slot_if_shape(shape, slot).map(Value::to_number),
            Some(1.0)
        );
        *map.slot_mut_if_shape(shape, slot).expect("hit") = Value::Number(5.0);
        assert_eq!(map.get("y").map(Value::to_number), Some(5.0));
        map.insert_str("z", Value::Null);
        assert!(
            map.slot_if_shape(shape, slot).is_none(),
            "shape changed -> miss"
        );
        map.remove("x");
        assert!(
            map.slot_mut_if_shape(shape, slot).is_none(),
            "dictionary -> miss"
        );
    }

    #[test]
    fn marker_keys_are_tracked() {
        assert!(!build(&["a", "b"]).may_have_marker_key());
        assert!(build(&["a", "__w3cos_setter_b"]).may_have_marker_key());
    }

    #[test]
    fn from_hashmap_builds_sorted_shape_or_dictionary() {
        let small: HashMap<JsString, Value> = ["b", "a", "c"]
            .iter()
            .enumerate()
            .map(|(i, k)| (JsString::intern(k), Value::Number(i as f64)))
            .collect();
        let map = PropertyMap::from_interned_hashmap(small);
        assert!(map.is_compact());
        assert_eq!(keys(&map), vec!["a", "b", "c"]);
        assert_eq!(map.shape_id(), build(&["a", "b", "c"]).shape_id());
        assert_eq!(map.get("b").map(Value::to_number), Some(0.0));
        let wide: HashMap<JsString, Value> = (0..MAX_SHAPE_KEYS + 3)
            .map(|i| {
                (
                    JsString::intern(&format!("w{i:03}")),
                    Value::Number(i as f64),
                )
            })
            .collect();
        let map = PropertyMap::from_interned_hashmap(wide);
        assert!(map.is_dictionary());
        assert_eq!(map.len(), MAX_SHAPE_KEYS + 3);
        for i in 0..MAX_SHAPE_KEYS + 3 {
            assert_eq!(
                map.get(&format!("w{i:03}")).map(Value::to_number),
                Some(i as f64)
            );
        }
        assert_eq!(keys(&map).first().map(String::as_str), Some("w000"));
    }

    #[test]
    fn shape_ids_are_unique_across_threads() {
        let here = build(&["thread_unique_key"]).shape_id();
        let there = std::thread::spawn(|| build(&["thread_unique_key"]).shape_id())
            .join()
            .expect("thread");
        assert_ne!(here, there);
        assert_ne!(here, Some(ROOT_SHAPE_ID));
    }
}
