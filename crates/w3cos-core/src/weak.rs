//! JavaScript weak collections and weak references on the reference-counted
//! heap.
//!
//! # Identity
//!
//! Every Core heap payload (object, array, function) carries a
//! [`HeapAllocation`](crate::heap) ticket. The first time a value is used as
//! a weak target, key, token or weak-collection instance, the ticket is given
//! a thread-local 48-bit *watch id*. Ids are monotonic and never reused, so
//! `WeakMap`/`WeakSet` are plain hash tables keyed by that id: `get`, `set`,
//! `has`, `delete` and `add` are O(1) and never scan entries.
//!
//! # Death notices
//!
//! Dropping a watched payload (last strong `Rc` released, or a page-arena
//! reset) only appends its id to a thread-local death queue; `Drop` never
//! runs JavaScript and never touches the weak tables. The queue is processed
//! at the next weak-API call or job checkpoint ([`run_pending_finalizers`],
//! called by [`crate::promise::drain_microtasks`]):
//!
//! - entries keyed by the dead id are removed from every `WeakMap`/`WeakSet`
//!   that holds them (a reverse index makes this O(holders), not O(size));
//! - a dead `WeakMap`/`WeakSet`/`WeakRef`/`FinalizationRegistry` instance
//!   frees its own table;
//! - every `FinalizationRegistry` cell whose target died is moved to the
//!   pending-cleanup queue.
//!
//! # FinalizationRegistry ordering
//!
//! Cleanup callbacks are deterministic: they run on the JS thread at the
//! first job checkpoint after the target's last strong reference is dropped,
//! i.e. once the microtask queue is empty, never re-entrantly inside `Drop`
//! and never during the job that dropped the target. Pending cells run in
//! death order (the order the payloads were dropped); cells of one target run
//! in registration order. Each callback runs inside its own panic boundary,
//! and microtasks it queues are drained before the next batch of callbacks.
//! `cleanupSome()` still works: it synchronously runs this registry's pending
//! cells (optionally with an override callback). `unregister()` also removes
//! cells that are pending but not yet delivered. A registry that is itself
//! dropped never calls back.
//!
//! Page-arena values (ordinary AOT object literals, arrays and closures) are
//! strictly reference counted like host `Rc` values: each dies, and queues
//! its death notice, as soon as its last strong reference is released. Weak
//! references never hold a count. Only values kept alive by a reference
//! cycle wait for the page `reset`, which drops them in bulk.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::Value;
use crate::value::{WeakJsArray, WeakJsFunction, WeakJsObject};

/// Watch ids are dense sequential integers; a multiplicative hash spreads
/// them across hashbrown's control bits without SipHash's cost.
#[derive(Default, Clone, Copy)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 =
                (self.0.rotate_left(5) ^ u64::from(*byte)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type IdBuild = BuildHasherDefault<IdHasher>;
type IdMap<V> = HashMap<u64, V, IdBuild>;
type IdSet = HashSet<u64, IdBuild>;

enum WeakValue {
    Array(WeakJsArray),
    Object(WeakJsObject),
    Function(WeakJsFunction),
}

impl WeakValue {
    fn new(value: &Value) -> Option<Self> {
        if let Some(array) = value.as_array() {
            return Some(Self::Array(array.downgrade()));
        }
        if let Some(object) = value.as_object() {
            return Some(Self::Object(object.downgrade()));
        }
        value
            .as_function()
            .map(|function| Self::Function(function.downgrade()))
    }

    fn upgrade(&self) -> Option<Value> {
        match self {
            Self::Array(value) => value.upgrade_value(),
            Self::Object(value) => value.upgrade_value(),
            Self::Function(value) => value.upgrade_value(),
        }
    }
}

/// Which weak table holds a watched id, for O(holders) removal on death.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Holder {
    MapKey(u64),
    SetKey(u64),
    RegistryTarget(u64),
    RegistryToken(u64),
}

struct RegistryCell {
    held_value: Value,
    token: Option<u64>,
}

struct Registry {
    callback: Value,
    /// Live cells by target id, in registration order.
    cells: IdMap<Vec<RegistryCell>>,
    /// Target ids registered with each unregister token.
    tokens: IdMap<Vec<u64>>,
}

struct PendingCleanup {
    registry: u64,
    held_value: Value,
    token: Option<u64>,
}

#[derive(Default)]
struct WeakTables {
    maps: IdMap<IdMap<Value>>,
    sets: IdMap<IdSet>,
    refs: IdMap<WeakValue>,
    registries: IdMap<Registry>,
    holders: IdMap<Vec<Holder>>,
}

impl WeakTables {
    fn hold(&mut self, target: u64, holder: Holder) {
        let holders = self.holders.entry(target).or_default();
        if !holders.contains(&holder) {
            holders.push(holder);
        }
    }

    /// Forget everything keyed by `id`. Values that must be dropped go to
    /// `garbage` so their destructors run after the tables are released.
    fn on_death(
        &mut self,
        id: u64,
        garbage: &mut Vec<Value>,
        pending: &mut VecDeque<PendingCleanup>,
    ) {
        if let Some(map) = self.maps.remove(&id) {
            garbage.extend(map.into_values());
        }
        self.sets.remove(&id);
        self.refs.remove(&id);
        if let Some(registry) = self.registries.remove(&id) {
            garbage.push(registry.callback);
            for cells in registry.cells.into_values() {
                garbage.extend(cells.into_iter().map(|cell| cell.held_value));
            }
        }
        let Some(holders) = self.holders.remove(&id) else {
            return;
        };
        for holder in holders {
            match holder {
                Holder::MapKey(map) => {
                    if let Some(value) = self.maps.get_mut(&map).and_then(|map| map.remove(&id)) {
                        garbage.push(value);
                    }
                }
                Holder::SetKey(set) => {
                    if let Some(set) = self.sets.get_mut(&set) {
                        set.remove(&id);
                    }
                }
                Holder::RegistryTarget(registry_id) => {
                    if let Some(cells) = self
                        .registries
                        .get_mut(&registry_id)
                        .and_then(|registry| registry.cells.remove(&id))
                    {
                        pending.extend(cells.into_iter().map(|cell| PendingCleanup {
                            registry: registry_id,
                            held_value: cell.held_value,
                            token: cell.token,
                        }));
                    }
                }
                Holder::RegistryToken(registry_id) => {
                    if let Some(registry) = self.registries.get_mut(&registry_id) {
                        registry.tokens.remove(&id);
                    }
                }
            }
        }
    }
}

thread_local! {
    static NEXT_WATCH_ID: Cell<u64> = const { Cell::new(1) };
    static DEATHS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    static TABLES: RefCell<WeakTables> = RefCell::new(WeakTables::default());
    static PENDING: RefCell<VecDeque<PendingCleanup>> = const { RefCell::new(VecDeque::new()) };
    static WEAK_MAP_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WEAK_SET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WEAK_REF_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FINALIZATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

const WATCH_ID_LIMIT: u64 = 1 << 48;

/// Allocate the next weak-identity id (see [`crate::heap::HeapAllocation`]).
pub(crate) fn next_watch_id() -> u64 {
    NEXT_WATCH_ID.with(|next| {
        let id = next.get();
        assert!(id < WATCH_ID_LIMIT, "w3cos weak identity space exhausted");
        next.set(id + 1);
        id
    })
}

/// Record that a watched payload was dropped. Called from `Drop`; must stay
/// allocation-light and must never run JavaScript.
pub(crate) fn note_death(id: u64) {
    let _ = DEATHS.try_with(|deaths| {
        if let Ok(mut deaths) = deaths.try_borrow_mut() {
            deaths.push(id);
        }
    });
}

/// Apply queued death notices to the weak tables. Destructors of released
/// values run after the tables are unborrowed; any further deaths they cause
/// are processed in the same call.
fn process_deaths() {
    loop {
        let deaths = DEATHS.with(|deaths| std::mem::take(&mut *deaths.borrow_mut()));
        if deaths.is_empty() {
            return;
        }
        let mut garbage = Vec::new();
        let mut pending = VecDeque::new();
        TABLES.with(|tables| {
            let mut tables = tables.borrow_mut();
            for id in deaths {
                tables.on_death(id, &mut garbage, &mut pending);
            }
        });
        if !pending.is_empty() {
            PENDING.with(|queue| queue.borrow_mut().extend(pending));
        }
        drop(garbage);
    }
}

/// Job-checkpoint hook: process death notices and run every pending
/// `FinalizationRegistry` cleanup callback. Returns the number of callbacks
/// run. Called by [`crate::promise::drain_microtasks`] once the microtask
/// queue is empty.
pub fn run_pending_finalizers() -> usize {
    process_deaths();
    let batch = PENDING.with(|queue| std::mem::take(&mut *queue.borrow_mut()));
    let mut ran = 0;
    for cleanup in batch {
        let callback = TABLES.with(|tables| {
            tables
                .borrow()
                .registries
                .get(&cleanup.registry)
                .map(|registry| registry.callback.clone())
        });
        if let Some(callback) = callback {
            let held_value = cleanup.held_value;
            let _ = catch_unwind(AssertUnwindSafe(|| {
                callback.call(Value::Undefined, vec![held_value])
            }));
            ran += 1;
        }
    }
    process_deaths();
    ran
}

/// Number of `FinalizationRegistry` cells waiting for a job checkpoint.
pub fn pending_finalizer_count() -> usize {
    process_deaths();
    PENDING.with(|queue| queue.borrow().len())
}

fn type_error(message: &str) -> ! {
    crate::throw_value(Value::object(HashMap::from([
        ("name".into(), Value::string("TypeError")),
        ("message".into(), Value::string(message)),
    ])))
}

/// Weak identity of an object/array/function. `assign` allocates an id for
/// a value that has never been watched; lookups pass `false`, since a value
/// without an id cannot be in any weak table.
fn identity(value: &Value, assign: bool) -> Option<Option<u64>> {
    fn pick(allocation: &crate::heap::HeapAllocation, assign: bool) -> Option<u64> {
        if assign {
            Some(allocation.watch_id())
        } else {
            allocation.existing_watch_id()
        }
    }
    if let Some(array) = value.as_array() {
        let storage = array
            .try_borrow()
            .unwrap_or_else(|_| type_error("weak target array is busy"));
        return Some(pick(storage.heap_allocation(), assign));
    }
    if let Some(object) = value.as_object() {
        let object = object
            .try_borrow()
            .unwrap_or_else(|_| type_error("weak target object is busy"));
        return Some(pick(object.heap_allocation(), assign));
    }
    value
        .as_function()
        .map(|function| pick(function.heap_allocation(), assign))
}

/// Id for a value that must be weakly holdable; throws a TypeError otherwise.
fn watch(value: &Value, operation: &str) -> u64 {
    identity(value, true)
        .flatten()
        .unwrap_or_else(|| type_error(&format!("{operation} requires an object target")))
}

/// Id of a value used as a lookup key: `None` for primitives and for objects
/// never placed in a weak table.
fn lookup(value: &Value) -> Option<u64> {
    identity(value, false).flatten()
}

fn instance(proto: &Value) -> (Value, u64) {
    let value = Value::object(HashMap::new());
    crate::class::set_prototype_of(&value, proto);
    let id = watch(&value, "weak collection");
    (value, id)
}

type NativeMethod = fn(Value, Vec<Value>) -> Value;

fn build_class(
    methods: &[(&str, NativeMethod)],
    constructor: impl Fn(&[Value], &Value) -> Value + 'static,
) -> Value {
    let proto = Value::object(HashMap::new());
    for (name, method) in methods {
        proto.set_property(name, Value::function(*method));
    }
    let proto_for_constructor = proto.clone();
    let class = Value::callable(HashMap::new(), move |_, args| {
        constructor(&args, &proto_for_constructor)
    });
    proto.set_property("constructor", class.clone());
    class.set_property("prototype", proto);
    class
}

// ── WeakMap ────────────────────────────────────────────────────────────

fn weak_map_instance(args: &[Value], proto: &Value) -> Value {
    process_deaths();
    let (map, id) = instance(proto);
    TABLES.with(|tables| tables.borrow_mut().maps.insert(id, IdMap::default()));
    if let Some(seed) = args.first().filter(|seed| !seed.is_nullish()) {
        for pair in seed.iter() {
            let values: Vec<Value> = pair.iter().collect();
            weak_map_set(
                map.clone(),
                vec![
                    values.first().cloned().unwrap_or(Value::Undefined),
                    values.get(1).cloned().unwrap_or(Value::Undefined),
                ],
            );
        }
    }
    map
}

fn weak_map_set(this: Value, args: Vec<Value>) -> Value {
    process_deaths();
    let Some(map_id) = lookup(&this) else {
        return Value::Undefined;
    };
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let value = args.get(1).cloned().unwrap_or(Value::Undefined);
    let key_id = watch(&key, "WeakMap.set");
    let replaced = TABLES.with(|tables| {
        let mut tables = tables.borrow_mut();
        let map = tables.maps.get_mut(&map_id)?;
        let replaced = map.insert(key_id, value);
        if replaced.is_none() {
            tables.hold(key_id, Holder::MapKey(map_id));
        }
        replaced
    });
    drop(replaced);
    this
}

fn weak_map_get(this: Value, args: Vec<Value>) -> Value {
    let (Some(map_id), Some(key_id)) = (lookup(&this), args.first().and_then(lookup)) else {
        return Value::Undefined;
    };
    TABLES.with(|tables| {
        tables
            .borrow()
            .maps
            .get(&map_id)
            .and_then(|map| map.get(&key_id).cloned())
            .unwrap_or(Value::Undefined)
    })
}

fn weak_map_has(this: Value, args: Vec<Value>) -> Value {
    let (Some(map_id), Some(key_id)) = (lookup(&this), args.first().and_then(lookup)) else {
        return Value::Bool(false);
    };
    Value::Bool(TABLES.with(|tables| {
        tables
            .borrow()
            .maps
            .get(&map_id)
            .is_some_and(|map| map.contains_key(&key_id))
    }))
}

fn weak_map_delete(this: Value, args: Vec<Value>) -> Value {
    process_deaths();
    let (Some(map_id), Some(key_id)) = (lookup(&this), args.first().and_then(lookup)) else {
        return Value::Bool(false);
    };
    // The reverse-index entry stays until the key dies; removal of an absent
    // key on death is a no-op and `hold` de-duplicates on re-insert.
    let removed = TABLES.with(|tables| {
        tables
            .borrow_mut()
            .maps
            .get_mut(&map_id)
            .and_then(|map| map.remove(&key_id))
    });
    Value::Bool(removed.is_some())
}

// ── WeakSet ────────────────────────────────────────────────────────────

fn weak_set_instance(args: &[Value], proto: &Value) -> Value {
    process_deaths();
    let (set, id) = instance(proto);
    TABLES.with(|tables| tables.borrow_mut().sets.insert(id, IdSet::default()));
    if let Some(seed) = args.first().filter(|seed| !seed.is_nullish()) {
        for value in seed.iter() {
            weak_set_add(set.clone(), vec![value]);
        }
    }
    set
}

fn weak_set_add(this: Value, args: Vec<Value>) -> Value {
    process_deaths();
    let Some(set_id) = lookup(&this) else {
        return Value::Undefined;
    };
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let value_id = watch(&value, "WeakSet.add");
    TABLES.with(|tables| {
        let mut tables = tables.borrow_mut();
        let inserted = tables
            .sets
            .get_mut(&set_id)
            .is_some_and(|set| set.insert(value_id));
        if inserted {
            tables.hold(value_id, Holder::SetKey(set_id));
        }
    });
    this
}

fn weak_set_has(this: Value, args: Vec<Value>) -> Value {
    let (Some(set_id), Some(value_id)) = (lookup(&this), args.first().and_then(lookup)) else {
        return Value::Bool(false);
    };
    Value::Bool(TABLES.with(|tables| {
        tables
            .borrow()
            .sets
            .get(&set_id)
            .is_some_and(|set| set.contains(&value_id))
    }))
}

fn weak_set_delete(this: Value, args: Vec<Value>) -> Value {
    process_deaths();
    let (Some(set_id), Some(value_id)) = (lookup(&this), args.first().and_then(lookup)) else {
        return Value::Bool(false);
    };
    Value::Bool(TABLES.with(|tables| {
        tables
            .borrow_mut()
            .sets
            .get_mut(&set_id)
            .is_some_and(|set| set.remove(&value_id))
    }))
}

// ── WeakRef ────────────────────────────────────────────────────────────

fn weak_ref_instance(args: &[Value], proto: &Value) -> Value {
    process_deaths();
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let Some(weak) = WeakValue::new(&target) else {
        type_error("WeakRef requires an object target");
    };
    let (reference, id) = instance(proto);
    TABLES.with(|tables| tables.borrow_mut().refs.insert(id, weak));
    reference
}

fn weak_ref_deref(this: Value, _args: Vec<Value>) -> Value {
    let Some(id) = lookup(&this) else {
        return Value::Undefined;
    };
    TABLES.with(|tables| {
        tables
            .borrow()
            .refs
            .get(&id)
            .and_then(WeakValue::upgrade)
            .unwrap_or(Value::Undefined)
    })
}

// ── FinalizationRegistry ───────────────────────────────────────────────

fn finalization_instance(args: &[Value], proto: &Value) -> Value {
    process_deaths();
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    if !callback.is_function() {
        type_error("FinalizationRegistry requires a cleanup callback");
    }
    let (registry, id) = instance(proto);
    TABLES.with(|tables| {
        tables.borrow_mut().registries.insert(
            id,
            Registry {
                callback,
                cells: IdMap::default(),
                tokens: IdMap::default(),
            },
        )
    });
    registry
}

fn finalization_register(this: Value, args: Vec<Value>) -> Value {
    process_deaths();
    let Some(registry_id) = lookup(&this) else {
        return Value::Undefined;
    };
    let target_value = args.first().cloned().unwrap_or(Value::Undefined);
    let target = watch(&target_value, "FinalizationRegistry.register");
    let held_value = args.get(1).cloned().unwrap_or(Value::Undefined);
    if held_value.same_value_zero(&target_value) {
        type_error("FinalizationRegistry target and holdings must not be the same");
    }
    let token = match args.get(2) {
        None => None,
        Some(value) if value.is_undefined() => None,
        Some(value) => Some(watch(
            value,
            "FinalizationRegistry.register unregister token",
        )),
    };
    TABLES.with(|tables| {
        let mut tables = tables.borrow_mut();
        let Some(registry) = tables.registries.get_mut(&registry_id) else {
            return;
        };
        registry
            .cells
            .entry(target)
            .or_default()
            .push(RegistryCell { held_value, token });
        if let Some(token) = token {
            registry.tokens.entry(token).or_default().push(target);
            tables.hold(token, Holder::RegistryToken(registry_id));
        }
        tables.hold(target, Holder::RegistryTarget(registry_id));
    });
    Value::Undefined
}

fn finalization_unregister(this: Value, args: Vec<Value>) -> Value {
    process_deaths();
    let token_value = args.first().cloned().unwrap_or(Value::Undefined);
    if WeakValue::new(&token_value).is_none() {
        type_error("FinalizationRegistry.unregister requires an object token");
    }
    let (Some(registry_id), Some(token)) = (lookup(&this), lookup(&token_value)) else {
        return Value::Bool(false);
    };
    let mut garbage = Vec::new();
    TABLES.with(|tables| {
        let mut tables = tables.borrow_mut();
        let Some(registry) = tables.registries.get_mut(&registry_id) else {
            return;
        };
        let Some(targets) = registry.tokens.remove(&token) else {
            return;
        };
        for target in targets {
            let Entry::Occupied(mut cells) = registry.cells.entry(target) else {
                continue;
            };
            let (removed, kept): (Vec<_>, Vec<_>) = std::mem::take(cells.get_mut())
                .into_iter()
                .partition(|cell| cell.token == Some(token));
            garbage.extend(removed.into_iter().map(|cell| cell.held_value));
            if kept.is_empty() {
                cells.remove();
            } else {
                *cells.get_mut() = kept;
            }
        }
    });
    PENDING.with(|queue| {
        queue.borrow_mut().retain_mut(|cleanup| {
            let matches = cleanup.registry == registry_id && cleanup.token == Some(token);
            if matches {
                garbage.push(std::mem::replace(&mut cleanup.held_value, Value::Undefined));
            }
            !matches
        })
    });
    let removed = !garbage.is_empty();
    drop(garbage);
    Value::Bool(removed)
}

fn finalization_cleanup_some(this: Value, args: Vec<Value>) -> Value {
    process_deaths();
    let Some(registry_id) = lookup(&this) else {
        return Value::Undefined;
    };
    let callback = match args.first() {
        Some(callback) if callback.is_function() => callback.clone(),
        Some(callback) if !callback.is_undefined() => {
            type_error("FinalizationRegistry.cleanupSome callback must be callable")
        }
        _ => match TABLES.with(|tables| {
            tables
                .borrow()
                .registries
                .get(&registry_id)
                .map(|registry| registry.callback.clone())
        }) {
            Some(callback) => callback,
            None => return Value::Undefined,
        },
    };
    let mine: Vec<PendingCleanup> = PENDING.with(|queue| {
        let mut queue = queue.borrow_mut();
        let (mine, rest): (VecDeque<_>, VecDeque<_>) = std::mem::take(&mut *queue)
            .into_iter()
            .partition(|cleanup| cleanup.registry == registry_id);
        *queue = rest;
        mine.into_iter().collect()
    });
    for cleanup in mine {
        callback.call(Value::Undefined, vec![cleanup.held_value]);
    }
    Value::Undefined
}

// ── AOT back-edge captures ─────────────────────────────────────────────

/// Hidden property on a weak capture getter naming its `WeakRef`.
const WEAK_CAPTURE_KEY: &str = "__w3cos_weak_capture";

/// Getter for a closure capture the compiler weakened as a unique back-edge
/// (`w3cos_compiler::unique_back_edges`): it dereferences a `WeakRef` to
/// `target`, so the closure stored on `target` does not keep it alive.
pub fn weak_capture_getter(target: Value) -> Value {
    let reference = crate::intrinsics::construct(&weak_ref_class(), vec![target]);
    let for_getter = reference.clone();
    let getter = Value::function(move |_, _| for_getter.call_method("deref", Vec::new()));
    getter.set_property(WEAK_CAPTURE_KEY, reference);
    getter
}

/// The current target of a [`weak_capture_getter`], or `None` for any other
/// getter. A closure nested inside the weakening closure captures this
/// strongly: only the self-installed closure owns the weakened edge.
pub fn strong_capture_target(getter: &Value) -> Option<Value> {
    let function = getter.as_function()?;
    let reference = function.get_property(WEAK_CAPTURE_KEY);
    if reference.is_undefined() {
        return None;
    }
    Some(reference.call_method("deref", Vec::new()))
}

/// Immutable capture getter that holds `target` strongly.
pub fn strong_capture_getter(target: Value) -> Value {
    Value::function(move |_, _| target.clone())
}

pub fn weak_map_class() -> Value {
    WEAK_MAP_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = build_class(
            &[
                ("get", weak_map_get),
                ("set", weak_map_set),
                ("has", weak_map_has),
                ("delete", weak_map_delete),
            ],
            weak_map_instance,
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn weak_set_class() -> Value {
    WEAK_SET_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = build_class(
            &[
                ("add", weak_set_add),
                ("has", weak_set_has),
                ("delete", weak_set_delete),
            ],
            weak_set_instance,
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn weak_ref_class() -> Value {
    WEAK_REF_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = build_class(&[("deref", weak_ref_deref)], weak_ref_instance);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn finalization_registry_class() -> Value {
    FINALIZATION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = build_class(
            &[
                ("register", finalization_register),
                ("unregister", finalization_unregister),
                ("cleanupSome", finalization_cleanup_some),
            ],
            finalization_instance,
        );
        class.set_property("cleanupMode", Value::string("automatic"));
        class.set_property("automaticCleanup", Value::Bool(true));
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

/// Live entry counts `(weak maps, weak map entries, reverse-index ids)`,
/// for leak tests.
#[doc(hidden)]
pub fn debug_table_sizes() -> (usize, usize, usize) {
    process_deaths();
    TABLES.with(|tables| {
        let tables = tables.borrow();
        (
            tables.maps.len(),
            tables.maps.values().map(|map| map.len()).sum(),
            tables.holders.len(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PanicValue;
    use crate::class::{construct, instance_of};
    use crate::value::ArrayStorage;
    use std::rc::Rc;

    #[test]
    fn weak_map_and_set_do_not_keep_object_keys_alive() {
        let map = construct(&weak_map_class(), vec![]);
        let set = construct(&weak_set_class(), vec![]);
        // Host Rc object (arena objects: see the arena_targets_* tests).
        let key = Value::Object(Rc::new(RefCell::new(crate::JsObject::new())));
        let weak = match &key {
            Value::Object(value) => Rc::downgrade(value),
            _ => unreachable!(),
        };
        map.call_method("set", vec![key.clone(), Value::string("value")]);
        set.call_method("add", vec![key.clone()]);
        assert_eq!(
            map.call_method("get", vec![key.clone()]),
            Value::string("value")
        );
        assert_eq!(set.call_method("has", vec![key.clone()]), Value::Bool(true));
        drop(key);
        assert!(weak.upgrade().is_none());
        assert!(instance_of(&map, &weak_map_class()));
        assert!(instance_of(&set, &weak_set_class()));
    }

    #[test]
    fn weak_ref_deref_releases_target() {
        // Host Rc array (arena arrays: see the arena_* tests).
        let target = Value::Array(Rc::new(RefCell::new(ArrayStorage::new(vec![
            Value::Number(1.0),
        ]))));
        let reference = construct(&weak_ref_class(), vec![target.clone()]);
        assert_eq!(reference.call_method("deref", vec![]), target);
        drop(target);
        assert!(reference.call_method("deref", vec![]).is_undefined());
    }

    #[test]
    fn finalization_registry_cleanup_some_delivers_holdings() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let seen_for_callback = seen.clone();
        let registry = construct(
            &finalization_registry_class(),
            vec![Value::function(move |_, args| {
                seen_for_callback.borrow_mut().push(args[0].to_js_string());
                Value::Undefined
            })],
        );
        let target = Value::Object(Rc::new(RefCell::new(crate::JsObject::new())));
        registry.call_method("register", vec![target.clone(), Value::string("shipment")]);
        drop(target);
        registry.call_method("cleanupSome", vec![]);
        assert_eq!(&*seen.borrow(), &["shipment"]);
    }

    #[test]
    fn weak_targets_reject_primitives() {
        let map = construct(&weak_map_class(), vec![]);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            map.call_method("set", vec![Value::string("key"), Value::Number(1.0)])
        }));
        let error = outcome
            .expect_err("primitive WeakMap key should throw")
            .downcast::<PanicValue>()
            .expect("exception should carry a JavaScript value");
        assert_eq!(error.0.get_property("name").to_js_string(), "TypeError");
    }

    fn host_object() -> Value {
        Value::Object(Rc::new(RefCell::new(crate::JsObject::new())))
    }

    fn recording_registry() -> (Value, Rc<RefCell<Vec<String>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let seen_for_callback = seen.clone();
        let registry = construct(
            &finalization_registry_class(),
            vec![Value::function(move |_, args| {
                seen_for_callback.borrow_mut().push(args[0].to_js_string());
                Value::Undefined
            })],
        );
        (registry, seen)
    }

    #[test]
    fn finalization_runs_at_next_checkpoint_not_inside_drop() {
        let (registry, seen) = recording_registry();
        let target = host_object();
        registry.call_method("register", vec![target.clone(), Value::string("a")]);
        drop(target);
        assert!(seen.borrow().is_empty(), "never re-entrant inside Drop");
        assert_eq!(pending_finalizer_count(), 1);
        assert_eq!(crate::promise::drain_microtasks(), 1);
        assert_eq!(&*seen.borrow(), &["a"]);
        assert_eq!(
            crate::promise::drain_microtasks(),
            0,
            "delivered exactly once"
        );
    }

    #[test]
    fn finalization_runs_after_queued_microtasks_in_death_order() {
        let (registry, seen) = recording_registry();
        let first = host_object();
        let second = host_object();
        registry.call_method("register", vec![first.clone(), Value::string("first-1")]);
        registry.call_method("register", vec![second.clone(), Value::string("second")]);
        registry.call_method("register", vec![first.clone(), Value::string("first-2")]);
        let seen_for_job = seen.clone();
        crate::promise::queue_microtask(Value::function(move |_, _| {
            seen_for_job.borrow_mut().push("microtask".into());
            Value::Undefined
        }));
        drop(second);
        drop(first);
        crate::promise::drain_microtasks();
        assert_eq!(
            &*seen.borrow(),
            &["microtask", "second", "first-1", "first-2"]
        );
    }

    #[test]
    fn unregister_cancels_live_and_pending_cells() {
        let (registry, seen) = recording_registry();
        let token = host_object();
        let live = host_object();
        let dead = host_object();
        registry.call_method(
            "register",
            vec![live.clone(), Value::string("live"), token.clone()],
        );
        registry.call_method(
            "register",
            vec![dead.clone(), Value::string("dead"), token.clone()],
        );
        registry.call_method(
            "register",
            vec![host_object(), Value::string("other"), Value::Undefined],
        );
        drop(dead);
        assert_eq!(
            registry.call_method("unregister", vec![token.clone()]),
            Value::Bool(true)
        );
        assert_eq!(
            registry.call_method("unregister", vec![token]),
            Value::Bool(false)
        );
        drop(live);
        crate::promise::drain_microtasks();
        assert_eq!(&*seen.borrow(), &["other"]);
    }

    #[test]
    fn dropped_registry_never_calls_back() {
        let (registry, seen) = recording_registry();
        let target = host_object();
        registry.call_method("register", vec![target.clone(), Value::string("x")]);
        // Registry instances live in the page arena; a reset drops them.
        drop(registry);
        crate::page_arena::reset();
        drop(target);
        crate::promise::drain_microtasks();
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn weak_map_entry_and_value_are_released_eagerly_with_the_key() {
        let map = construct(&weak_map_class(), vec![]);
        let set = construct(&weak_set_class(), vec![]);
        let (maps_before, entries_before, holders_before) = debug_table_sizes();
        let key = host_object();
        let value = host_object();
        let value_weak = match &value {
            Value::Object(object) => Rc::downgrade(object),
            _ => unreachable!(),
        };
        map.call_method("set", vec![key.clone(), value]);
        set.call_method("add", vec![key.clone()]);
        assert_eq!(debug_table_sizes().1, entries_before + 1);
        assert!(value_weak.upgrade().is_some(), "map keeps its value alive");
        drop(key);
        assert_eq!(
            debug_table_sizes(),
            (maps_before, entries_before, holders_before)
        );
        assert!(
            value_weak.upgrade().is_none(),
            "value released with its key"
        );
    }

    #[test]
    fn arena_targets_die_at_their_last_release_without_a_reset() {
        let (registry, seen) = recording_registry();
        let map = construct(&weak_map_class(), vec![]);
        let set = construct(&weak_set_class(), vec![]);
        let entries_before = debug_table_sizes().1;
        let target = Value::object(HashMap::new());
        assert!(target.object_handle().is_some(), "page-arena object");
        let reference = construct(&weak_ref_class(), vec![target.clone()]);
        registry.call_method("register", vec![target.clone(), Value::string("object")]);
        map.call_method("set", vec![target.clone(), Value::string("v")]);
        set.call_method("add", vec![target.clone()]);
        assert_eq!(debug_table_sizes().1, entries_before + 1);
        assert!(set.call_method("has", vec![target.clone()]).to_bool());
        // A WeakRef (or a deref result that was dropped) holds no count.
        assert_eq!(reference.call_method("deref", vec![]), target);
        drop(target);
        assert!(reference.call_method("deref", vec![]).is_undefined());
        assert_eq!(
            debug_table_sizes().1,
            entries_before,
            "WeakMap entry released with its arena key"
        );
        assert!(seen.borrow().is_empty(), "never inside Drop");
        crate::promise::drain_microtasks();
        assert_eq!(&*seen.borrow(), &["object"]);
    }

    #[test]
    fn arena_closures_and_arrays_fire_finalizers() {
        let (registry, seen) = recording_registry();
        let closure = Value::function(|_, _| Value::Undefined);
        let array = Value::array(vec![Value::Number(1.0)]);
        assert!(closure.function_handle().is_some() && array.array_handle().is_some());
        let closure_ref = construct(&weak_ref_class(), vec![closure.clone()]);
        let array_ref = construct(&weak_ref_class(), vec![array.clone()]);
        registry.call_method("register", vec![closure.clone(), Value::string("closure")]);
        registry.call_method("register", vec![array.clone(), Value::string("array")]);
        drop(array);
        drop(closure);
        assert!(closure_ref.call_method("deref", vec![]).is_undefined());
        assert!(array_ref.call_method("deref", vec![]).is_undefined());
        crate::promise::drain_microtasks();
        assert_eq!(&*seen.borrow(), &["array", "closure"]);
    }

    #[test]
    fn weak_map_set_get_delete_and_identity() {
        let map = construct(&weak_map_class(), vec![]);
        let a = host_object();
        let b = Value::object(HashMap::new());
        let f = Value::function(|_, _| Value::Undefined);
        map.call_method("set", vec![a.clone(), Value::Number(1.0)]);
        map.call_method("set", vec![b.clone(), Value::Number(2.0)]);
        map.call_method("set", vec![f.clone(), Value::Number(3.0)]);
        map.call_method("set", vec![a.clone(), Value::Number(4.0)]);
        assert_eq!(map.call_method("get", vec![a.clone()]), Value::Number(4.0));
        assert_eq!(map.call_method("get", vec![b.clone()]), Value::Number(2.0));
        assert_eq!(map.call_method("get", vec![f.clone()]), Value::Number(3.0));
        assert!(map.call_method("get", vec![host_object()]).is_undefined());
        assert!(
            map.call_method("get", vec![Value::Number(1.0)])
                .is_undefined()
        );
        assert_eq!(
            map.call_method("delete", vec![a.clone()]),
            Value::Bool(true)
        );
        assert_eq!(
            map.call_method("delete", vec![a.clone()]),
            Value::Bool(false)
        );
        assert_eq!(map.call_method("has", vec![a.clone()]), Value::Bool(false));
        map.call_method("set", vec![a.clone(), Value::Number(5.0)]);
        assert_eq!(map.call_method("get", vec![a]), Value::Number(5.0));
        let other = construct(&weak_map_class(), vec![]);
        assert!(other.call_method("get", vec![b]).is_undefined());
    }

    #[test]
    fn page_reset_frees_weak_collection_tables() {
        let (maps_before, _, _) = debug_table_sizes();
        let map = construct(&weak_map_class(), vec![]);
        map.call_method("set", vec![host_object(), Value::Number(1.0)]);
        assert_eq!(debug_table_sizes().0, maps_before + 1);
        drop(map);
        crate::page_arena::reset();
        assert_eq!(debug_table_sizes().0, maps_before);
    }

    #[test]
    fn hundred_thousand_entry_weak_map_is_linear_time() {
        // Guards against reintroducing per-operation scans: 100k inserts +
        // lookups take milliseconds with O(1) tables and minutes with O(n).
        let map = construct(&weak_map_class(), vec![]);
        let keys: Vec<Value> = (0..100_000).map(|_| host_object()).collect();
        let started = std::time::Instant::now();
        for (index, key) in keys.iter().enumerate() {
            map.call_method("set", vec![key.clone(), Value::Number(index as f64)]);
        }
        for (index, key) in keys.iter().enumerate() {
            assert_eq!(
                map.call_method("get", vec![key.clone()]),
                Value::Number(index as f64)
            );
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        drop(keys);
        assert_eq!(debug_table_sizes().1, 0);
    }
}
