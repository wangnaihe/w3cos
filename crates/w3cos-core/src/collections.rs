//! JavaScript `Map` and `Set` (plus `WeakMap`/`WeakSet` aliases) for the ESM
//! compile pipeline.
//!
//! Both collections follow the `promise.rs` state-storage idiom: an instance
//! is a `Value::Object` whose prototype link points at the class's
//! `"prototype"` object (so `x instanceof Map` works through
//! [`crate::class::instance_of`]) and whose only own data property is the
//! hidden numeric key `__w3cos_map_id` / `__w3cos_set_id` — an id into a
//! thread-local registry holding the shared `Rc<RefCell<…>>` backing store,
//! because `Value` has no native-resource slot.
//!
//! Key equality is ECMAScript SameValueZero ([`Value::same_value_zero`]):
//! NaN keys match, -0/+0 are the same key, and object/array/function keys
//! compare by identity. That last point is what lets Monaco's DI container
//! (`InstantiationService`) use decorator *functions* as Map keys — the old
//! `builtins::Map` stringified keys, collapsing every function onto one
//! entry and corrupting the service graph.
//!
//! The backing store is a `Vec` scanned linearly so **insertion order** is
//! preserved for `forEach` / `entries` / `keys` / `values` and for
//! `for … of` / spread ([`Value::iter`] delegates to [`iter_collection`]).
//!
//! v1 limitations:
//! - `entries()` / `keys()` / `values()` return plain **arrays**, not
//!   iterator objects — `next()`-style iterator protocol is not supported.
//!   `for … of` and spread still work because they lower to `Value::iter`.
//! - `WeakMap` / `WeakSet` have no weak semantics (keys are strongly held);
//!   they alias `Map` / `Set`, so `instanceof` treats them interchangeably.
//! - The id registry never reclaims ids of dropped instances.
//! - `forEach` iterates a snapshot: entries added during the callback are
//!   not visited (deletions/mutations still land in the store).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::Value;

/// Hidden own property holding a Map instance's registry id.
const MAP_STATE_KEY: &str = "__w3cos_map_id";
/// Hidden own property holding a Set instance's registry id.
const SET_STATE_KEY: &str = "__w3cos_set_id";

/// Map backing store: insertion-ordered key/value entries.
type MapEntries = Vec<(Value, Value)>;
/// Set backing store: insertion-ordered values.
type SetValues = Vec<Value>;

thread_local! {
    /// Map registries keyed by the id stored under [`MAP_STATE_KEY`].
    static MAP_STATES: RefCell<HashMap<u64, Rc<RefCell<MapEntries>>>> =
        RefCell::new(HashMap::new());
    static SET_STATES: RefCell<HashMap<u64, Rc<RefCell<SetValues>>>> =
        RefCell::new(HashMap::new());
    static NEXT_COLLECTION_ID: Cell<u64> = const { Cell::new(1) };
    /// Class-value singletons (built once per thread so prototype identity —
    /// and therefore `instanceof` — is stable across references).
    static MAP_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn next_id() -> u64 {
    NEXT_COLLECTION_ID.with(|counter| {
        let id = counter.get();
        counter.set(id + 1);
        id
    })
}

fn register_map(entries: Rc<RefCell<MapEntries>>) -> u64 {
    let id = next_id();
    MAP_STATES.with(|registry| registry.borrow_mut().insert(id, entries));
    id
}

fn register_set(values: Rc<RefCell<SetValues>>) -> u64 {
    let id = next_id();
    SET_STATES.with(|registry| registry.borrow_mut().insert(id, values));
    id
}

/// The shared entries behind `value`, when `value` is one of our Maps.
fn map_state_of(value: &Value) -> Option<Rc<RefCell<MapEntries>>> {
    if let Value::Object(object) = value {
        if let Value::Number(id) = object.borrow().get_direct(MAP_STATE_KEY) {
            return MAP_STATES.with(|registry| registry.borrow().get(&(id as u64)).cloned());
        }
    }
    None
}

/// The shared values behind `value`, when `value` is one of our Sets.
fn set_state_of(value: &Value) -> Option<Rc<RefCell<SetValues>>> {
    if let Value::Object(object) = value {
        if let Value::Number(id) = object.borrow().get_direct(SET_STATE_KEY) {
            return SET_STATES.with(|registry| registry.borrow().get(&(id as u64)).cloned());
        }
    }
    None
}

/// Index of `key` in a Map store, by SameValueZero.
fn find_index(entries: &MapEntries, key: &Value) -> Option<usize> {
    entries
        .iter()
        .position(|(stored, _)| stored.same_value_zero(key))
}

/// Index of `item` in a Set store, by SameValueZero.
fn set_index(values: &SetValues, item: &Value) -> Option<usize> {
    values
        .iter()
        .position(|stored| stored.same_value_zero(item))
}

// ── Map ──────────────────────────────────────────────────────────────────

/// `Map.prototype.set` core: overwrite in place (position unchanged) or
/// append (insertion order preserved).
fn map_set(entries: &Rc<RefCell<MapEntries>>, key: Value, value: Value) {
    let mut entries = entries.borrow_mut();
    match find_index(&entries, &key) {
        Some(index) => entries[index].1 = value,
        None => entries.push((key, value)),
    }
}

/// A fresh Map instance linked to `proto`, seeded per `new Map(iterable)`.
fn map_instance(args: &[Value], proto: &Value) -> Value {
    let entries = Rc::new(RefCell::new(MapEntries::new()));
    let map = Value::object(HashMap::new());
    crate::class::set_prototype_of(&map, proto);
    map.set_property(
        MAP_STATE_KEY,
        Value::Number(register_map(entries.clone()) as f64),
    );
    match args.first() {
        None | Some(Value::Undefined) | Some(Value::Null) => {}
        Some(seed) => seed_map(&entries, seed),
    }
    map
}

/// Seed from an iterable: an array of [key, value] pairs, or one of our own
/// Map instances (`new Map(other)` copies its entries). Anything else is
/// tolerated as empty (JS would throw a TypeError; the runtime stays total).
fn seed_map(entries: &Rc<RefCell<MapEntries>>, seed: &Value) {
    if let Some(other) = map_state_of(seed) {
        let copied: MapEntries = other.borrow().clone();
        entries.borrow_mut().extend(copied);
        return;
    }
    for pair in seed.iter() {
        let mut items = pair.iter();
        // Pairs that are not arrays are skipped rather than throwing.
        let Some(key) = items.next() else { continue };
        let value = items.next().unwrap_or(Value::Undefined);
        map_set(entries, key, value);
    }
}

fn map_proto_get(this: Value, args: Vec<Value>) -> Value {
    let Some(entries) = map_state_of(&this) else {
        return Value::Undefined;
    };
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let entries = entries.borrow();
    find_index(&entries, &key)
        .map(|index| entries[index].1.clone())
        .unwrap_or(Value::Undefined)
}

fn map_proto_set(this: Value, args: Vec<Value>) -> Value {
    if let Some(entries) = map_state_of(&this) {
        let key = args.first().cloned().unwrap_or(Value::Undefined);
        let value = args.get(1).cloned().unwrap_or(Value::Undefined);
        map_set(&entries, key, value);
    }
    // Chaining: `map.set(a, 1).set(b, 2)` returns the receiver.
    this
}

fn map_proto_has(this: Value, args: Vec<Value>) -> Value {
    let Some(entries) = map_state_of(&this) else {
        return Value::Bool(false);
    };
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    Value::Bool(find_index(&entries.borrow(), &key).is_some())
}

fn map_proto_delete(this: Value, args: Vec<Value>) -> Value {
    let Some(entries) = map_state_of(&this) else {
        return Value::Bool(false);
    };
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let mut entries = entries.borrow_mut();
    match find_index(&entries, &key) {
        Some(index) => {
            entries.remove(index);
            Value::Bool(true)
        }
        None => Value::Bool(false),
    }
}

fn map_proto_clear(this: Value, _args: Vec<Value>) -> Value {
    if let Some(entries) = map_state_of(&this) {
        entries.borrow_mut().clear();
    }
    Value::Undefined
}

fn map_proto_for_each(this: Value, args: Vec<Value>) -> Value {
    let Some(entries) = map_state_of(&this) else {
        return Value::Undefined;
    };
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    // Snapshot (bound first so the borrow ends here): a callback mutating
    // the map must not trip the RefCell borrow.
    let snapshot: MapEntries = entries.borrow().clone();
    for (key, value) in snapshot {
        callback.call(Value::Undefined, vec![value, key, this.clone()]);
    }
    Value::Undefined
}

fn map_proto_entries(this: Value, _args: Vec<Value>) -> Value {
    let Some(entries) = map_state_of(&this) else {
        return Value::array(Vec::new());
    };
    Value::array(
        entries
            .borrow()
            .iter()
            .map(|(key, value)| Value::array(vec![key.clone(), value.clone()]))
            .collect(),
    )
}

fn map_proto_keys(this: Value, _args: Vec<Value>) -> Value {
    let Some(entries) = map_state_of(&this) else {
        return Value::array(Vec::new());
    };
    Value::array(
        entries
            .borrow()
            .iter()
            .map(|(key, _)| key.clone())
            .collect(),
    )
}

fn map_proto_values(this: Value, _args: Vec<Value>) -> Value {
    let Some(entries) = map_state_of(&this) else {
        return Value::array(Vec::new());
    };
    Value::array(
        entries
            .borrow()
            .iter()
            .map(|(_, value)| value.clone())
            .collect(),
    )
}

fn map_proto_size(this: Value, _args: Vec<Value>) -> Value {
    map_state_of(&this)
        .map(|entries| Value::Number(entries.borrow().len() as f64))
        .unwrap_or(Value::Undefined)
}

// ── Set ──────────────────────────────────────────────────────────────────

/// `Set.prototype.add` core: append unless already present (SameValueZero).
fn set_add(values: &Rc<RefCell<SetValues>>, item: Value) {
    let mut values = values.borrow_mut();
    if set_index(&values, &item).is_none() {
        values.push(item);
    }
}

/// A fresh Set instance linked to `proto`, seeded per `new Set(iterable)`.
fn set_instance(args: &[Value], proto: &Value) -> Value {
    let values = Rc::new(RefCell::new(SetValues::new()));
    let set = Value::object(HashMap::new());
    crate::class::set_prototype_of(&set, proto);
    set.set_property(
        SET_STATE_KEY,
        Value::Number(register_set(values.clone()) as f64),
    );
    match args.first() {
        None | Some(Value::Undefined) | Some(Value::Null) => {}
        Some(seed) => {
            if let Some(other) = set_state_of(seed) {
                // `new Set(other)` copies the other set's values.
                let copied: SetValues = other.borrow().clone();
                values.borrow_mut().extend(copied);
            } else {
                for item in seed.iter() {
                    set_add(&values, item);
                }
            }
        }
    }
    set
}

fn set_proto_add(this: Value, args: Vec<Value>) -> Value {
    if let Some(values) = set_state_of(&this) {
        set_add(&values, args.first().cloned().unwrap_or(Value::Undefined));
    }
    // Chaining: `set.add(a).add(b)` returns the receiver.
    this
}

fn set_proto_has(this: Value, args: Vec<Value>) -> Value {
    let Some(values) = set_state_of(&this) else {
        return Value::Bool(false);
    };
    let item = args.first().cloned().unwrap_or(Value::Undefined);
    Value::Bool(set_index(&values.borrow(), &item).is_some())
}

fn set_proto_delete(this: Value, args: Vec<Value>) -> Value {
    let Some(values) = set_state_of(&this) else {
        return Value::Bool(false);
    };
    let item = args.first().cloned().unwrap_or(Value::Undefined);
    let mut values = values.borrow_mut();
    match set_index(&values, &item) {
        Some(index) => {
            values.remove(index);
            Value::Bool(true)
        }
        None => Value::Bool(false),
    }
}

fn set_proto_clear(this: Value, _args: Vec<Value>) -> Value {
    if let Some(values) = set_state_of(&this) {
        values.borrow_mut().clear();
    }
    Value::Undefined
}

fn set_proto_for_each(this: Value, args: Vec<Value>) -> Value {
    let Some(values) = set_state_of(&this) else {
        return Value::Undefined;
    };
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    // Snapshot (bound first so the borrow ends here): a callback mutating
    // the set must not trip the RefCell borrow.
    let snapshot: SetValues = values.borrow().clone();
    for value in snapshot {
        callback.call(Value::Undefined, vec![value.clone(), value, this.clone()]);
    }
    Value::Undefined
}

fn set_proto_values(this: Value, _args: Vec<Value>) -> Value {
    let Some(values) = set_state_of(&this) else {
        return Value::array(Vec::new());
    };
    Value::array(values.borrow().clone())
}

fn set_proto_entries(this: Value, _args: Vec<Value>) -> Value {
    let Some(values) = set_state_of(&this) else {
        return Value::array(Vec::new());
    };
    Value::array(
        values
            .borrow()
            .iter()
            .map(|value| Value::array(vec![value.clone(), value.clone()]))
            .collect(),
    )
}

fn set_proto_size(this: Value, _args: Vec<Value>) -> Value {
    set_state_of(&this)
        .map(|values| Value::Number(values.borrow().len() as f64))
        .unwrap_or(Value::Undefined)
}

// ── Class values ─────────────────────────────────────────────────────────

fn build_map_class() -> Value {
    let proto = Value::object(HashMap::new());
    for (name, method) in [
        ("get", map_proto_get as fn(Value, Vec<Value>) -> Value),
        ("set", map_proto_set),
        ("has", map_proto_has),
        ("delete", map_proto_delete),
        ("clear", map_proto_clear),
        ("forEach", map_proto_for_each),
        ("entries", map_proto_entries),
        ("keys", map_proto_keys),
        ("values", map_proto_values),
    ] {
        proto.set_property(name, Value::function(method));
    }
    // Live `size` via the getter convention (see Value::get_property).
    proto.set_property("__w3cos_getter_size", Value::function(map_proto_size));
    let proto_for_slot = proto.clone();
    let class = Value::callable(HashMap::new(), move |_this, args| {
        map_instance(&args, &proto_for_slot)
    });
    proto.set_property("constructor", class.clone());
    class.set_property("prototype", proto);
    class
}

fn build_set_class() -> Value {
    let proto = Value::object(HashMap::new());
    for (name, method) in [
        ("add", set_proto_add as fn(Value, Vec<Value>) -> Value),
        ("has", set_proto_has),
        ("delete", set_proto_delete),
        ("clear", set_proto_clear),
        ("forEach", set_proto_for_each),
        ("values", set_proto_values),
        // `keys` aliases `values` per the ES spec.
        ("keys", set_proto_values),
        ("entries", set_proto_entries),
    ] {
        proto.set_property(name, Value::function(method));
    }
    proto.set_property("__w3cos_getter_size", Value::function(set_proto_size));
    let proto_for_slot = proto.clone();
    let class = Value::callable(HashMap::new(), move |_this, args| {
        set_instance(&args, &proto_for_slot)
    });
    proto.set_property("constructor", class.clone());
    class.set_property("prototype", proto);
    class
}

/// The `Map` class value (thread-local singleton): a callable object whose
/// own `"prototype"` property holds the methods, so both
/// `class::construct(&map_class(), args)` (`new Map(...)`) and
/// `class::instance_of(x, &map_class())` (`x instanceof Map`) work.
pub fn map_class() -> Value {
    MAP_CLASS.with(|cell| {
        if let Some(value) = cell.borrow().as_ref() {
            return value.clone();
        }
        let value = build_map_class();
        *cell.borrow_mut() = Some(value.clone());
        value
    })
}

/// The `Set` class value (thread-local singleton) — see [`map_class`].
pub fn set_class() -> Value {
    SET_CLASS.with(|cell| {
        if let Some(value) = cell.borrow().as_ref() {
            return value.clone();
        }
        let value = build_set_class();
        *cell.borrow_mut() = Some(value.clone());
        value
    })
}

/// `WeakMap` — v1 aliases [`map_class`]: no weak semantics (keys are
/// strongly held) and `instanceof` treats WeakMap and Map interchangeably.
pub fn weak_map_class() -> Value {
    map_class()
}

/// `WeakSet` — v1 aliases [`set_class`] (no weak semantics; see
/// [`weak_map_class`]).
pub fn weak_set_class() -> Value {
    set_class()
}

/// Minimal shared constructor for JavaScript typed arrays. The dynamic
/// runtime represents their indexed storage as `Value::Array`; this preserves
/// length, indexed access, iteration, and the `set` operation Monaco needs.
pub fn typed_array_class() -> Value {
    Value::callable(HashMap::new(), |_this, args| {
        let Some(first) = args.first() else {
            return typed_array_value(Vec::new());
        };
        if first.is_number() {
            return typed_array_value(vec![
                Value::Number(0.0);
                first.to_number().max(0.0) as usize
            ]);
        }
        let values: Vec<Value> = first.iter().collect();
        let start = args.get(1).map(Value::to_number).unwrap_or(0.0).max(0.0) as usize;
        let len = args
            .get(2)
            .map(Value::to_number)
            .map(|len| len.max(0.0) as usize)
            .unwrap_or_else(|| values.len().saturating_sub(start));
        typed_array_value(values.into_iter().skip(start).take(len).collect())
    })
}

thread_local! {
    static TYPED_ARRAYS: RefCell<Vec<Weak<RefCell<Vec<Value>>>>> = const { RefCell::new(Vec::new()) };
}

pub fn typed_array_value(values: Vec<Value>) -> Value {
    let value = Value::array(values);
    if let Value::Array(storage) = &value {
        TYPED_ARRAYS.with(|arrays| arrays.borrow_mut().push(Rc::downgrade(storage)));
    }
    value
}

pub fn is_typed_array(value: &Value) -> bool {
    let Value::Array(candidate) = value else {
        return false;
    };
    TYPED_ARRAYS.with(|arrays| {
        let mut arrays = arrays.borrow_mut();
        arrays.retain(|array| array.strong_count() > 0);
        arrays
            .iter()
            .filter_map(Weak::upgrade)
            .any(|array| Rc::ptr_eq(&array, candidate))
    })
}

/// Entries yielded by `for … of` / spread over one of our collection
/// instances ([`Value::iter`] delegates here): [key, value] pair arrays for
/// Maps, bare values for Sets, both in insertion order. `None` for any
/// other value.
pub(crate) fn iter_collection(value: &Value) -> Option<Vec<Value>> {
    if let Some(entries) = map_state_of(value) {
        return Some(
            entries
                .borrow()
                .iter()
                .map(|(key, value)| Value::array(vec![key.clone(), value.clone()]))
                .collect(),
        );
    }
    if let Some(values) = set_state_of(value) {
        return Some(values.borrow().clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::{construct, instance_of};

    fn new_map(args: Vec<Value>) -> Value {
        construct(&map_class(), args)
    }

    fn new_set(args: Vec<Value>) -> Value {
        construct(&set_class(), args)
    }

    fn pair(key: Value, value: Value) -> Value {
        Value::array(vec![key, value])
    }

    // ── same_value_zero ──────────────────────────────────────────────────

    #[test]
    fn same_value_zero_semantics() {
        let nan = Value::Number(f64::NAN);
        assert!(nan.same_value_zero(&Value::Number(f64::NAN)));
        assert!(Value::Number(-0.0).same_value_zero(&Value::Number(0.0)));
        assert!(!Value::Number(1.0).same_value_zero(&Value::Number(-1.0)));
        assert!(Value::Undefined.same_value_zero(&Value::Undefined));
        assert!(Value::Null.same_value_zero(&Value::Null));
        assert!(!Value::Undefined.same_value_zero(&Value::Null));
        assert!(Value::string("a").same_value_zero(&Value::string("a")));
        assert!(!Value::Bool(true).same_value_zero(&Value::Bool(false)));

        // Reference identity for heap values.
        let array = Value::array(vec![]);
        assert!(array.same_value_zero(&array.clone()));
        assert!(!array.same_value_zero(&Value::array(vec![])));
        let object = Value::object(HashMap::new());
        assert!(object.same_value_zero(&object.clone()));
        assert!(!object.same_value_zero(&Value::object(HashMap::new())));

        // Function identity: clones match, fresh closures don't.
        let function = Value::function(|_, _| Value::Undefined);
        assert!(function.same_value_zero(&function.clone()));
        assert!(!function.same_value_zero(&Value::function(|_, _| Value::Undefined)));
        // Cross-type never matches.
        assert!(!Value::Number(1.0).same_value_zero(&Value::string("1")));
    }

    // ── Map ─────────────────────────────────────────────────────────────

    #[test]
    fn map_object_and_function_keys_are_distinct() {
        // The Monaco DI case: decorator functions as keys must not collapse.
        let map = new_map(vec![]);
        let decorator_a = Value::function(|_, _| Value::Undefined);
        let decorator_b = Value::function(|_, _| Value::Undefined);
        let object_key = Value::object(HashMap::new());

        map.call_method("set", vec![decorator_a.clone(), Value::string("svc-a")]);
        map.call_method("set", vec![decorator_b.clone(), Value::string("svc-b")]);
        map.call_method("set", vec![object_key.clone(), Value::Number(3.0)]);

        assert_eq!(map.get_property("size").to_number(), 3.0);
        assert_eq!(
            map.call_method("get", vec![decorator_a.clone()])
                .to_js_string(),
            "svc-a"
        );
        assert_eq!(
            map.call_method("get", vec![decorator_b]).to_js_string(),
            "svc-b"
        );
        assert_eq!(map.call_method("get", vec![object_key]).to_number(), 3.0);
        // A different closure value is a different key.
        assert!(
            map.call_method("get", vec![Value::function(|_, _| Value::Undefined)])
                .is_undefined()
        );
    }

    #[test]
    fn map_nan_and_signed_zero_keys() {
        let map = new_map(vec![]);
        map.call_method("set", vec![Value::Number(f64::NAN), Value::string("nan")]);
        assert_eq!(
            map.call_method("get", vec![Value::Number(f64::NAN)])
                .to_js_string(),
            "nan"
        );
        assert_eq!(
            map.call_method("has", vec![Value::Number(f64::NAN)]),
            Value::Bool(true)
        );

        // -0 and +0 are the same key: setting one overwrites the other and
        // the size stays 1.
        map.call_method("set", vec![Value::Number(-0.0), Value::string("neg")]);
        map.call_method("set", vec![Value::Number(0.0), Value::string("pos")]);
        assert_eq!(map.get_property("size").to_number(), 2.0);
        assert_eq!(
            map.call_method("get", vec![Value::Number(-0.0)])
                .to_js_string(),
            "pos"
        );
    }

    #[test]
    fn map_set_overwrites_in_place_and_chains() {
        let map = new_map(vec![]);
        let chained = map
            .call_method("set", vec![Value::string("k"), Value::Number(1.0)])
            .call_method("set", vec![Value::string("j"), Value::Number(2.0)]);
        // set returns the map itself.
        assert_eq!(chained.get_property("size").to_number(), 2.0);

        map.call_method("set", vec![Value::string("k"), Value::Number(9.0)]);
        assert_eq!(map.get_property("size").to_number(), 2.0);
        assert_eq!(
            map.call_method("get", vec![Value::string("k")]).to_number(),
            9.0
        );
        // Insertion order kept: k first despite the overwrite.
        assert_eq!(map.call_method("keys", vec![]).to_js_string(), "k,j");
    }

    #[test]
    fn map_missing_key_is_undefined_and_has_is_false() {
        let map = new_map(vec![]);
        assert!(
            map.call_method("get", vec![Value::string("nope")])
                .is_undefined()
        );
        assert_eq!(
            map.call_method("has", vec![Value::string("nope")]),
            Value::Bool(false)
        );
    }

    #[test]
    fn map_delete_and_clear() {
        let map = new_map(vec![Value::array(vec![
            pair(Value::string("a"), Value::Number(1.0)),
            pair(Value::string("b"), Value::Number(2.0)),
        ])]);
        assert_eq!(
            map.call_method("delete", vec![Value::string("a")]),
            Value::Bool(true)
        );
        assert_eq!(
            map.call_method("delete", vec![Value::string("a")]),
            Value::Bool(false)
        );
        assert_eq!(map.get_property("size").to_number(), 1.0);
        assert_eq!(
            map.call_method("delete", vec![Value::string("missing")]),
            Value::Bool(false)
        );

        map.call_method("clear", vec![]);
        assert_eq!(map.get_property("size").to_number(), 0.0);
        assert_eq!(
            map.call_method("has", vec![Value::string("b")]),
            Value::Bool(false)
        );
    }

    #[test]
    fn map_for_each_is_insertion_ordered_with_map_arg() {
        let map = new_map(vec![]);
        for (key, value) in [("c", 3.0), ("a", 1.0), ("b", 2.0)] {
            map.call_method("set", vec![Value::string(key), Value::Number(value)]);
        }
        let log = Rc::new(RefCell::new(Vec::new()));
        let seen = log.clone();
        let map_for_arg = map.clone();
        map.call_method(
            "forEach",
            vec![Value::function(move |_, args| {
                seen.borrow_mut().push(format!(
                    "{}={}",
                    args[1].to_js_string(),
                    args[0].to_js_string()
                ));
                // Third callback arg is the map itself.
                assert_eq!(args[2], map_for_arg);
                Value::Undefined
            })],
        );
        assert_eq!(
            log.borrow().as_slice(),
            &["c=3".to_string(), "a=1".to_string(), "b=2".to_string()]
        );
    }

    #[test]
    fn map_entries_keys_values_return_arrays() {
        let map = new_map(vec![Value::array(vec![
            pair(Value::string("x"), Value::Number(1.0)),
            pair(Value::string("y"), Value::Number(2.0)),
        ])]);
        let entries = map.call_method("entries", vec![]);
        assert!(entries.is_array());
        assert_eq!(entries.to_js_string(), "x,1,y,2");
        assert_eq!(map.call_method("keys", vec![]).to_js_string(), "x,y");
        assert_eq!(map.call_method("values", vec![]).to_js_string(), "1,2");
    }

    #[test]
    fn map_size_is_live() {
        let map = new_map(vec![]);
        assert_eq!(map.get_property("size").to_number(), 0.0);
        map.call_method("set", vec![Value::string("a"), Value::Number(1.0)]);
        assert_eq!(map.get_property("size").to_number(), 1.0);
        map.call_method("set", vec![Value::string("b"), Value::Number(2.0)]);
        assert_eq!(map.get_property("size").to_number(), 2.0);
        map.call_method("delete", vec![Value::string("a")]);
        assert_eq!(map.get_property("size").to_number(), 1.0);
    }

    #[test]
    fn map_seeds_from_pairs_and_copies_maps() {
        let map = new_map(vec![Value::array(vec![
            pair(Value::string("a"), Value::Number(1.0)),
            pair(Value::string("b"), Value::Number(2.0)),
            // Duplicate key: last write wins, position of the first kept.
            pair(Value::string("a"), Value::Number(3.0)),
            // Malformed pair tolerated (skipped).
            Value::string("not-a-pair"),
        ])]);
        assert_eq!(map.get_property("size").to_number(), 2.0);
        assert_eq!(
            map.call_method("get", vec![Value::string("a")]).to_number(),
            3.0
        );
        assert_eq!(map.call_method("keys", vec![]).to_js_string(), "a,b");

        // new Map(other) copies entries; the copy is independent.
        let copy = new_map(vec![map.clone()]);
        assert_eq!(copy.get_property("size").to_number(), 2.0);
        map.call_method("delete", vec![Value::string("a")]);
        assert_eq!(copy.get_property("size").to_number(), 2.0);

        // Non-iterable seeds are tolerated.
        for seed in [Value::Undefined, Value::Null, Value::Number(4.0)] {
            let empty = new_map(vec![seed]);
            assert_eq!(empty.get_property("size").to_number(), 0.0);
        }
    }

    #[test]
    fn map_instanceof_and_plain_object_is_not() {
        let map = new_map(vec![]);
        assert!(instance_of(&map, &map_class()));
        assert!(!instance_of(&Value::object(HashMap::new()), &map_class()));
        assert!(!instance_of(&Value::array(vec![]), &map_class()));
        assert!(!instance_of(&Value::Number(1.0), &map_class()));
        // The class value is a singleton: prototype identity is stable.
        assert!(instance_of(&map, &map_class()));
    }

    #[test]
    fn map_for_of_yields_entry_pairs() {
        let map = new_map(vec![Value::array(vec![
            pair(Value::string("a"), Value::Number(1.0)),
            pair(Value::string("b"), Value::Number(2.0)),
        ])]);
        let pairs: Vec<Value> = map.iter().collect();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].to_js_string(), "a,1");
        assert_eq!(pairs[1].to_js_string(), "b,2");
    }

    #[test]
    fn map_for_each_tolerates_mutation_from_callback() {
        // Deleting inside forEach must not trip the RefCell (snapshot iter).
        let map = new_map(vec![Value::array(vec![
            pair(Value::string("a"), Value::Number(1.0)),
            pair(Value::string("b"), Value::Number(2.0)),
        ])]);
        let target = map.clone();
        map.call_method(
            "forEach",
            vec![Value::function(move |_, _| {
                target.call_method("delete", vec![Value::string("b")]);
                Value::Undefined
            })],
        );
        assert_eq!(map.get_property("size").to_number(), 1.0);
        assert_eq!(
            map.call_method("has", vec![Value::string("b")]),
            Value::Bool(false)
        );
    }

    #[test]
    fn weak_map_aliases_map() {
        let weak = construct(&weak_map_class(), vec![]);
        weak.call_method("set", vec![Value::string("k"), Value::Number(1.0)]);
        assert_eq!(weak.get_property("size").to_number(), 1.0);
        assert!(instance_of(&weak, &weak_map_class()));
    }

    // ── Set ─────────────────────────────────────────────────────────────

    #[test]
    fn set_add_has_delete_clear_and_chaining() {
        let set = new_set(vec![]);
        let chained = set
            .call_method("add", vec![Value::Number(1.0)])
            .call_method("add", vec![Value::Number(2.0)])
            .call_method("add", vec![Value::Number(1.0)]); // duplicate ignored
        assert_eq!(chained.get_property("size").to_number(), 2.0);
        assert_eq!(
            set.call_method("has", vec![Value::Number(1.0)]),
            Value::Bool(true)
        );
        assert_eq!(
            set.call_method("has", vec![Value::Number(9.0)]),
            Value::Bool(false)
        );
        assert_eq!(
            set.call_method("delete", vec![Value::Number(1.0)]),
            Value::Bool(true)
        );
        assert_eq!(
            set.call_method("delete", vec![Value::Number(1.0)]),
            Value::Bool(false)
        );
        assert_eq!(set.get_property("size").to_number(), 1.0);
        set.call_method("clear", vec![]);
        assert_eq!(set.get_property("size").to_number(), 0.0);
    }

    #[test]
    fn set_object_and_function_members_are_distinct() {
        let set = new_set(vec![]);
        let fn_a = Value::function(|_, _| Value::Undefined);
        let fn_b = Value::function(|_, _| Value::Undefined);
        set.call_method("add", vec![fn_a.clone()]);
        set.call_method("add", vec![fn_b]);
        set.call_method("add", vec![fn_a.clone()]); // same function: no-op
        assert_eq!(set.get_property("size").to_number(), 2.0);
        assert_eq!(set.call_method("has", vec![fn_a]), Value::Bool(true));
        assert_eq!(
            set.call_method("has", vec![Value::function(|_, _| Value::Undefined)]),
            Value::Bool(false)
        );
    }

    #[test]
    fn set_nan_and_signed_zero_members() {
        let set = new_set(vec![]);
        set.call_method("add", vec![Value::Number(f64::NAN)]);
        set.call_method("add", vec![Value::Number(f64::NAN)]);
        set.call_method("add", vec![Value::Number(-0.0)]);
        set.call_method("add", vec![Value::Number(0.0)]);
        assert_eq!(set.get_property("size").to_number(), 2.0);
        assert_eq!(
            set.call_method("has", vec![Value::Number(f64::NAN)]),
            Value::Bool(true)
        );
        assert_eq!(
            set.call_method("has", vec![Value::Number(0.0)]),
            Value::Bool(true)
        );
    }

    #[test]
    fn set_for_each_values_keys_entries_and_iteration() {
        let set = new_set(vec![Value::array(vec![
            Value::string("b"),
            Value::string("a"),
            Value::string("b"), // duplicate in seed ignored
        ])]);
        assert_eq!(set.get_property("size").to_number(), 2.0);
        assert_eq!(set.call_method("values", vec![]).to_js_string(), "b,a");
        assert_eq!(set.call_method("keys", vec![]).to_js_string(), "b,a");
        assert_eq!(set.call_method("entries", vec![]).to_js_string(), "b,b,a,a");

        // forEach visits (value, value, set) in insertion order.
        let log = Rc::new(RefCell::new(Vec::new()));
        let seen = log.clone();
        set.call_method(
            "forEach",
            vec![Value::function(move |_, args| {
                assert_eq!(args[0], args[1]);
                seen.borrow_mut().push(args[0].to_js_string());
                Value::Undefined
            })],
        );
        assert_eq!(log.borrow().as_slice(), &["b".to_string(), "a".to_string()]);

        // for … of yields the values.
        let iterated: Vec<String> = set.iter().map(|v| v.to_js_string()).collect();
        assert_eq!(iterated, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn set_seeds_and_copies_sets() {
        let set = new_set(vec![Value::array(vec![
            Value::Number(1.0),
            Value::Number(2.0),
            Value::Number(1.0),
        ])]);
        assert_eq!(set.get_property("size").to_number(), 2.0);

        let copy = new_set(vec![set.clone()]);
        assert_eq!(copy.get_property("size").to_number(), 2.0);
        set.call_method("delete", vec![Value::Number(1.0)]);
        assert_eq!(copy.get_property("size").to_number(), 2.0);

        // Non-iterable seeds tolerated.
        assert_eq!(
            new_set(vec![Value::Null]).get_property("size").to_number(),
            0.0
        );
        assert_eq!(
            new_set(vec![Value::Number(5.0)])
                .get_property("size")
                .to_number(),
            0.0
        );
    }

    #[test]
    fn set_for_each_tolerates_mutation_from_callback() {
        let set = new_set(vec![Value::array(vec![
            Value::Number(1.0),
            Value::Number(2.0),
        ])]);
        let target = set.clone();
        set.call_method(
            "forEach",
            vec![Value::function(move |_, _| {
                target.call_method("delete", vec![Value::Number(2.0)]);
                Value::Undefined
            })],
        );
        assert_eq!(set.get_property("size").to_number(), 1.0);
    }

    #[test]
    fn set_instanceof() {
        let set = new_set(vec![]);
        assert!(instance_of(&set, &set_class()));
        assert!(!instance_of(&Value::object(HashMap::new()), &set_class()));
        // Map and Set classes are distinct.
        assert!(!instance_of(&set, &map_class()));
        assert!(!instance_of(&new_map(vec![]), &set_class()));
    }

    #[test]
    fn typed_array_has_fixed_length_and_set() {
        let typed = crate::class::construct(&typed_array_class(), vec![Value::Number(4.0)]);
        typed.call_method(
            "set",
            vec![
                Value::array(vec![Value::Number(7.0), Value::Number(8.0)]),
                Value::Number(1.0),
            ],
        );
        assert_eq!(typed.get_property("length").to_number(), 4.0);
        assert_eq!(typed.get_property("0").to_number(), 0.0);
        assert_eq!(typed.get_property("1").to_number(), 7.0);
        assert_eq!(typed.get_property("2").to_number(), 8.0);
        assert!(typed.get_property("buffer").strict_eq(&typed));

        let view = crate::class::construct(
            &typed_array_class(),
            vec![typed, Value::Number(1.0), Value::Number(2.0)],
        );
        assert_eq!(view.get_property("length").to_number(), 2.0);
        assert_eq!(view.get_property("0").to_number(), 7.0);
        assert_eq!(view.get_property("1").to_number(), 8.0);
    }
}
