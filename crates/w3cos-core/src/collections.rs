//! JavaScript `Map` and `Set` for the ESM
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
//! The backing store is a tombstone-preserving `Vec` scanned linearly so
//! **insertion order** is preserved. `forEach` and collection iterators are
//! live: they skip deleted slots, visit later additions, and revisit a value
//! deleted and reinserted after the cursor. AOT [`Value::iter`] and dynamic
//! W3IR iterator objects both consume the same [`crate::value::ValueIterator`]
//! over this insertion sequence.
//!
//! v1 limitations:
//! - `entries()` / `keys()` / `values()` return plain **arrays**, not
//!   iterator objects — `next()`-style iterator protocol is not supported.
//!   `for … of` and spread still work because they lower to `Value::iter`.
//! - The id registry never reclaims ids of dropped instances.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::Value;

/// Hidden own property holding a Map instance's registry id.
const MAP_STATE_KEY: &str = "__w3cos_map_id";
/// Hidden own property holding a Set instance's registry id.
const SET_STATE_KEY: &str = "__w3cos_set_id";

#[derive(Clone)]
struct MapEntry {
    key: Value,
    value: Value,
    active: bool,
}

#[derive(Clone)]
struct SetEntry {
    value: Value,
    active: bool,
}

/// Map/Set stores retain tombstones so live iterators keep a stable cursor
/// across deletion, clear, and delete-then-reinsert operations.
type MapEntries = Vec<MapEntry>;
type SetValues = Vec<SetEntry>;

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
    if let Some(object) = value.as_object() {
        if let Some(id) = object.borrow().get_direct(MAP_STATE_KEY).as_number() {
            return MAP_STATES.with(|registry| registry.borrow().get(&(id as u64)).cloned());
        }
    }
    None
}

/// The shared values behind `value`, when `value` is one of our Sets.
fn set_state_of(value: &Value) -> Option<Rc<RefCell<SetValues>>> {
    if let Some(object) = value.as_object() {
        if let Some(id) = object.borrow().get_direct(SET_STATE_KEY).as_number() {
            return SET_STATES.with(|registry| registry.borrow().get(&(id as u64)).cloned());
        }
    }
    None
}

/// Index of `key` in a Map store, by SameValueZero.
fn find_index(entries: &MapEntries, key: &Value) -> Option<usize> {
    entries
        .iter()
        .position(|entry| entry.active && entry.key.same_value_zero(key))
}

/// Index of `item` in a Set store, by SameValueZero.
fn set_index(values: &SetValues, item: &Value) -> Option<usize> {
    values
        .iter()
        .position(|entry| entry.active && entry.value.same_value_zero(item))
}

// ── Map ──────────────────────────────────────────────────────────────────

/// `Map.prototype.set` core: overwrite in place (position unchanged) or
/// append (insertion order preserved).
fn map_set(entries: &Rc<RefCell<MapEntries>>, key: Value, value: Value) {
    let mut entries = entries.borrow_mut();
    match find_index(&entries, &key) {
        Some(index) => entries[index].value = value,
        None => entries.push(MapEntry {
            key,
            value,
            active: true,
        }),
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
        None => {}
        Some(seed) if seed.is_nullish() => {}
        Some(seed) => seed_map(&entries, seed),
    }
    map
}

/// Seed from an iterable: an array of [key, value] pairs, or one of our own
/// Map instances (`new Map(other)` copies its entries). Anything else is
/// tolerated as empty (JS would throw a TypeError; the runtime stays total).
fn seed_map(entries: &Rc<RefCell<MapEntries>>, seed: &Value) {
    if let Some(other) = map_state_of(seed) {
        let copied = other
            .borrow()
            .iter()
            .filter(|entry| entry.active)
            .cloned()
            .collect::<MapEntries>();
        entries.borrow_mut().extend(copied);
        return;
    }
    for pair in seed.iter() {
        if !pair.is_array() {
            // Keep the runtime's total behavior for malformed entries. A
            // string is iterable too, but its yielded characters are not
            // valid Map entry objects.
            continue;
        }
        let mut items = pair.iter();
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
        .map(|index| entries[index].value.clone())
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
            entries[index].active = false;
            Value::Bool(true)
        }
        None => Value::Bool(false),
    }
}

fn map_proto_clear(this: Value, _args: Vec<Value>) -> Value {
    if let Some(entries) = map_state_of(&this) {
        for entry in entries.borrow_mut().iter_mut() {
            entry.active = false;
        }
    }
    Value::Undefined
}

fn map_proto_for_each(this: Value, args: Vec<Value>) -> Value {
    let Some(entries) = map_state_of(&this) else {
        return Value::Undefined;
    };
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    let mut index = 0;
    loop {
        let next = {
            let entries = entries.borrow();
            while index < entries.len() && !entries[index].active {
                index += 1;
            }
            entries.get(index).map(|entry| {
                index += 1;
                (entry.key.clone(), entry.value.clone())
            })
        };
        let Some((key, value)) = next else { break };
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
            .filter(|entry| entry.active)
            .map(|entry| Value::array(vec![entry.key.clone(), entry.value.clone()]))
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
            .filter(|entry| entry.active)
            .map(|entry| entry.key.clone())
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
            .filter(|entry| entry.active)
            .map(|entry| entry.value.clone())
            .collect(),
    )
}

fn map_proto_size(this: Value, _args: Vec<Value>) -> Value {
    map_state_of(&this)
        .map(|entries| {
            Value::Number(entries.borrow().iter().filter(|entry| entry.active).count() as f64)
        })
        .unwrap_or(Value::Undefined)
}

// ── Set ──────────────────────────────────────────────────────────────────

/// `Set.prototype.add` core: append unless already present (SameValueZero).
fn set_add(values: &Rc<RefCell<SetValues>>, item: Value) {
    let mut values = values.borrow_mut();
    if set_index(&values, &item).is_none() {
        values.push(SetEntry {
            value: item,
            active: true,
        });
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
        None => {}
        Some(seed) if seed.is_nullish() => {}
        Some(seed) => {
            if let Some(other) = set_state_of(seed) {
                // `new Set(other)` copies the other set's values.
                let copied = other
                    .borrow()
                    .iter()
                    .filter(|entry| entry.active)
                    .cloned()
                    .collect::<SetValues>();
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
            values[index].active = false;
            Value::Bool(true)
        }
        None => Value::Bool(false),
    }
}

fn set_proto_clear(this: Value, _args: Vec<Value>) -> Value {
    if let Some(values) = set_state_of(&this) {
        for entry in values.borrow_mut().iter_mut() {
            entry.active = false;
        }
    }
    Value::Undefined
}

fn set_proto_for_each(this: Value, args: Vec<Value>) -> Value {
    let Some(values) = set_state_of(&this) else {
        return Value::Undefined;
    };
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    let mut index = 0;
    loop {
        let next = {
            let values = values.borrow();
            while index < values.len() && !values[index].active {
                index += 1;
            }
            values.get(index).map(|entry| {
                index += 1;
                entry.value.clone()
            })
        };
        let Some(value) = next else { break };
        callback.call(Value::Undefined, vec![value.clone(), value, this.clone()]);
    }
    Value::Undefined
}

fn set_proto_values(this: Value, _args: Vec<Value>) -> Value {
    let Some(values) = set_state_of(&this) else {
        return Value::array(Vec::new());
    };
    Value::array(
        values
            .borrow()
            .iter()
            .filter(|entry| entry.active)
            .map(|entry| entry.value.clone())
            .collect(),
    )
}

fn set_proto_entries(this: Value, _args: Vec<Value>) -> Value {
    let Some(values) = set_state_of(&this) else {
        return Value::array(Vec::new());
    };
    Value::array(
        values
            .borrow()
            .iter()
            .filter(|entry| entry.active)
            .map(|entry| Value::array(vec![entry.value.clone(), entry.value.clone()]))
            .collect(),
    )
}

fn set_proto_size(this: Value, _args: Vec<Value>) -> Value {
    set_state_of(&this)
        .map(|values| {
            Value::Number(values.borrow().iter().filter(|entry| entry.active).count() as f64)
        })
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

/// JavaScript `WeakMap` backed by Rust weak pointers.
pub fn weak_map_class() -> Value {
    crate::weak::weak_map_class()
}

/// JavaScript `WeakSet` backed by Rust weak pointers.
pub fn weak_set_class() -> Value {
    crate::weak::weak_set_class()
}

/// Minimal shared constructor for JavaScript typed arrays. The dynamic
/// runtime represents their indexed storage as `Value::Array`; this preserves
/// length, indexed access, iteration, and the `set` operation Monaco needs.
pub fn typed_array_class() -> Value {
    crate::binary::typed_array_class("Uint8Array")
}

pub fn typed_array_value(values: Vec<Value>) -> Value {
    crate::binary::typed_array_value(values)
}

pub fn is_typed_array(value: &Value) -> bool {
    crate::binary::is_typed_array(value)
}

fn next_map_entry(entries: &Rc<RefCell<MapEntries>>, index: &Cell<usize>) -> Option<Value> {
    let entries = entries.borrow();
    let mut cursor = index.get();
    while cursor < entries.len() && !entries[cursor].active {
        cursor += 1;
    }
    let entry = entries.get(cursor)?;
    index.set(cursor + 1);
    Some(Value::array(vec![entry.key.clone(), entry.value.clone()]))
}

fn next_set_value(values: &Rc<RefCell<SetValues>>, index: &Cell<usize>) -> Option<Value> {
    let values = values.borrow();
    let mut cursor = index.get();
    while cursor < values.len() && !values[cursor].active {
        cursor += 1;
    }
    let entry = values.get(cursor)?;
    index.set(cursor + 1);
    Some(entry.value.clone())
}

/// Live iterator used by AOT and wrapped by the dynamic W3IR iterator protocol
/// over the same backing state.
pub(crate) fn iter_collection(value: &Value) -> Option<Box<dyn Iterator<Item = Value>>> {
    if let Some(entries) = map_state_of(value) {
        return Some(Box::new(LiveMapIterator {
            entries,
            index: Cell::new(0),
        }));
    }
    if let Some(values) = set_state_of(value) {
        return Some(Box::new(LiveSetIterator {
            values,
            index: Cell::new(0),
        }));
    }
    None
}

struct LiveMapIterator {
    entries: Rc<RefCell<MapEntries>>,
    index: Cell<usize>,
}

impl Iterator for LiveMapIterator {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        next_map_entry(&self.entries, &self.index)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self
            .entries
            .borrow()
            .iter()
            .skip(self.index.get())
            .filter(|entry| entry.active)
            .count();
        (remaining, Some(remaining))
    }
}

struct LiveSetIterator {
    values: Rc<RefCell<SetValues>>,
    index: Cell<usize>,
}

impl Iterator for LiveSetIterator {
    type Item = Value;

    fn next(&mut self) -> Option<Self::Item> {
        next_set_value(&self.values, &self.index)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self
            .values
            .borrow()
            .iter()
            .skip(self.index.get())
            .filter(|entry| entry.active)
            .count();
        (remaining, Some(remaining))
    }
}

pub enum CollectionSnapshot {
    Map(Vec<(Value, Value)>),
    Set(Vec<Value>),
}

pub fn collection_snapshot(value: &Value) -> Option<CollectionSnapshot> {
    if let Some(entries) = map_state_of(value) {
        return Some(CollectionSnapshot::Map(
            entries
                .borrow()
                .iter()
                .filter(|entry| entry.active)
                .map(|entry| (entry.key.clone(), entry.value.clone()))
                .collect(),
        ));
    }
    set_state_of(value).map(|values| {
        CollectionSnapshot::Set(
            values
                .borrow()
                .iter()
                .filter(|entry| entry.active)
                .map(|entry| entry.value.clone())
                .collect(),
        )
    })
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
    fn map_iterator_and_for_each_observe_live_insertion_sequence() {
        let map = new_map(vec![Value::array(vec![
            pair(Value::string("a"), Value::Number(1.0)),
            pair(Value::string("b"), Value::Number(2.0)),
        ])]);
        let mut iterator = map.iter();
        assert_eq!(iterator.next().unwrap().to_js_string(), "a,1");
        map.call_method("delete", vec![Value::string("b")]);
        map.call_method("set", vec![Value::string("c"), Value::Number(3.0)]);
        map.call_method("delete", vec![Value::string("a")]);
        map.call_method("set", vec![Value::string("a"), Value::Number(4.0)]);
        assert_eq!(iterator.next().unwrap().to_js_string(), "c,3");
        assert_eq!(iterator.next().unwrap().to_js_string(), "a,4");
        assert!(iterator.next().is_none());

        let live = new_map(vec![Value::array(vec![pair(
            Value::string("first"),
            Value::Number(1.0),
        )])]);
        let visited = Rc::new(RefCell::new(Vec::new()));
        let callback_target = live.clone();
        let callback_visited = Rc::clone(&visited);
        live.call_method(
            "forEach",
            vec![Value::function(move |_, args| {
                callback_visited.borrow_mut().push(args[1].to_js_string());
                if args[1].to_js_string() == "first" {
                    callback_target
                        .call_method("set", vec![Value::string("later"), Value::Number(2.0)]);
                }
                Value::Undefined
            })],
        );
        assert_eq!(
            visited.borrow().as_slice(),
            &["first".to_string(), "later".to_string()]
        );
    }

    #[test]
    fn map_for_each_tolerates_mutation_from_callback() {
        // Deleting inside forEach must not trip the RefCell live iterator.
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
    fn weak_map_uses_distinct_class_without_size() {
        let weak = construct(&weak_map_class(), vec![]);
        let key = Value::object(HashMap::new());
        weak.call_method("set", vec![key.clone(), Value::Number(1.0)]);
        assert!(weak.get_property("size").is_undefined());
        assert!(instance_of(&weak, &weak_map_class()));
        assert!(!instance_of(&weak, &map_class()));
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
    fn set_aot_iterator_observes_live_insertion_sequence() {
        let set = new_set(vec![Value::array(vec![
            Value::Number(1.0),
            Value::Number(2.0),
        ])]);
        let mut iterator = set.iter();
        assert_eq!(iterator.next(), Some(Value::Number(1.0)));
        set.call_method("delete", vec![Value::Number(2.0)]);
        set.call_method("add", vec![Value::Number(3.0)]);
        set.call_method("delete", vec![Value::Number(1.0)]);
        set.call_method("add", vec![Value::Number(1.0)]);
        assert_eq!(iterator.next(), Some(Value::Number(3.0)));
        assert_eq!(iterator.next(), Some(Value::Number(1.0)));
        assert_eq!(iterator.next(), None);
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
        let buffer = typed.get_property("buffer");
        assert_eq!(buffer.get_property("byteLength").to_number(), 4.0);

        let view = crate::class::construct(
            &typed_array_class(),
            vec![buffer, Value::Number(1.0), Value::Number(2.0)],
        );
        assert_eq!(view.get_property("length").to_number(), 2.0);
        assert_eq!(view.get_property("0").to_number(), 7.0);
        assert_eq!(view.get_property("1").to_number(), 8.0);
    }
}
