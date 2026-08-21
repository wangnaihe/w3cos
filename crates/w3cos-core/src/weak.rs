//! JavaScript weak collections and weak references.
//!
//! Dead targets are removed lazily when an API is accessed. A native host
//! cannot promise JavaScript GC scheduling, so `FinalizationRegistry`
//! callbacks are delivered through the optional `cleanupSome()` operation.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::value::{ArrayStorage, WeakJsFunction};
use crate::{JsObject, Value};

const STATE_KEY: &str = "__w3cos_weak_id";

enum WeakValue {
    Array(Weak<RefCell<ArrayStorage>>),
    Object(Weak<RefCell<JsObject>>),
    Function(WeakJsFunction),
}

impl WeakValue {
    fn new(value: &Value) -> Option<Self> {
        match value {
            Value::Array(value) => Some(Self::Array(Rc::downgrade(value))),
            _ if value.as_object().is_some() => {
                Some(Self::Object(Rc::downgrade(
                    &value.as_object().expect("object"),
                )))
            }
            Value::Function(value) => Some(Self::Function(value.downgrade())),
            _ => None,
        }
    }

    fn upgrade(&self) -> Option<Value> {
        match self {
            Self::Array(value) => value.upgrade().map(Value::Array),
            Self::Object(value) => value.upgrade().map(Value::Object),
            Self::Function(value) => value.upgrade().map(Value::Function),
        }
    }

    fn matches(&self, value: &Value) -> bool {
        self.upgrade()
            .is_some_and(|stored| stored.same_value_zero(value))
    }
}

type WeakMapEntries = Vec<(WeakValue, Value)>;
type WeakSetEntries = Vec<WeakValue>;

struct FinalizationEntry {
    target: WeakValue,
    held_value: Value,
    unregister_token: Option<WeakValue>,
}

struct FinalizationState {
    callback: Value,
    entries: Vec<FinalizationEntry>,
}

thread_local! {
    static WEAK_MAP_STATES: RefCell<HashMap<u64, Rc<RefCell<WeakMapEntries>>>> =
        RefCell::new(HashMap::new());
    static WEAK_SET_STATES: RefCell<HashMap<u64, Rc<RefCell<WeakSetEntries>>>> =
        RefCell::new(HashMap::new());
    static WEAK_REF_STATES: RefCell<HashMap<u64, WeakValue>> = RefCell::new(HashMap::new());
    static FINALIZATION_STATES: RefCell<HashMap<u64, Rc<RefCell<FinalizationState>>>> =
        RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static WEAK_MAP_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WEAK_SET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WEAK_REF_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FINALIZATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FINALIZATION_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn type_error(message: &str) -> ! {
    crate::throw_value(Value::object(HashMap::from([
        ("name".into(), Value::string("TypeError")),
        ("message".into(), Value::string(message)),
    ])))
}

fn weak_value(value: &Value, operation: &str) -> WeakValue {
    WeakValue::new(value)
        .unwrap_or_else(|| type_error(&format!("{operation} requires an object target")))
}

fn next_id() -> u64 {
    NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    })
}

fn state_id(value: &Value) -> Option<u64> {
    value
        .get_property(STATE_KEY)
        .as_number()
        .map(|id| id as u64)
}

fn instance(proto: &Value) -> Value {
    let value = Value::object(HashMap::new());
    crate::class::set_prototype_of(&value, proto);
    value
}

fn build_class(
    methods: &[(&str, fn(Value, Vec<Value>) -> Value)],
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

fn weak_map_instance(args: &[Value], proto: &Value) -> Value {
    let map = instance(proto);
    let id = next_id();
    WEAK_MAP_STATES.with(|states| {
        states
            .borrow_mut()
            .insert(id, Rc::new(RefCell::new(Vec::new())));
    });
    map.set_property(STATE_KEY, Value::Number(id as f64));
    if let Some(seed) = args.first() {
        for pair in seed.iter() {
            let values: Vec<Value> = pair.iter().collect();
            if let Some(key) = values.first() {
                weak_map_set(
                    map.clone(),
                    vec![
                        key.clone(),
                        values.get(1).cloned().unwrap_or(Value::Undefined),
                    ],
                );
            }
        }
    }
    map
}

fn weak_map_state(value: &Value) -> Option<Rc<RefCell<WeakMapEntries>>> {
    let id = state_id(value)?;
    WEAK_MAP_STATES.with(|states| states.borrow().get(&id).cloned())
}

fn weak_map_set(this: Value, args: Vec<Value>) -> Value {
    let Some(state) = weak_map_state(&this) else {
        return Value::Undefined;
    };
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let value = args.get(1).cloned().unwrap_or(Value::Undefined);
    let weak_key = weak_value(&key, "WeakMap.set");
    let mut entries = state.borrow_mut();
    entries.retain(|(stored, _)| stored.upgrade().is_some());
    if let Some((_, stored_value)) = entries.iter_mut().find(|(stored, _)| stored.matches(&key)) {
        *stored_value = value;
    } else {
        entries.push((weak_key, value));
    }
    this
}

fn weak_map_get(this: Value, args: Vec<Value>) -> Value {
    let Some(state) = weak_map_state(&this) else {
        return Value::Undefined;
    };
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let mut entries = state.borrow_mut();
    entries.retain(|(stored, _)| stored.upgrade().is_some());
    entries
        .iter()
        .find(|(stored, _)| stored.matches(&key))
        .map(|(_, value)| value.clone())
        .unwrap_or(Value::Undefined)
}

fn weak_map_has(this: Value, args: Vec<Value>) -> Value {
    let Some(state) = weak_map_state(&this) else {
        return Value::Bool(false);
    };
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let mut entries = state.borrow_mut();
    entries.retain(|(stored, _)| stored.upgrade().is_some());
    Value::Bool(entries.iter().any(|(stored, _)| stored.matches(&key)))
}

fn weak_map_delete(this: Value, args: Vec<Value>) -> Value {
    let Some(state) = weak_map_state(&this) else {
        return Value::Bool(false);
    };
    let key = args.first().cloned().unwrap_or(Value::Undefined);
    let mut entries = state.borrow_mut();
    let found = entries.iter().any(|(stored, _)| stored.matches(&key));
    entries.retain(|(stored, _)| stored.upgrade().is_some() && !stored.matches(&key));
    Value::Bool(found)
}

fn weak_set_instance(args: &[Value], proto: &Value) -> Value {
    let set = instance(proto);
    let id = next_id();
    WEAK_SET_STATES.with(|states| {
        states
            .borrow_mut()
            .insert(id, Rc::new(RefCell::new(Vec::new())));
    });
    set.set_property(STATE_KEY, Value::Number(id as f64));
    if let Some(seed) = args.first() {
        for value in seed.iter() {
            weak_set_add(set.clone(), vec![value]);
        }
    }
    set
}

fn weak_set_state(value: &Value) -> Option<Rc<RefCell<WeakSetEntries>>> {
    let id = state_id(value)?;
    WEAK_SET_STATES.with(|states| states.borrow().get(&id).cloned())
}

fn weak_set_add(this: Value, args: Vec<Value>) -> Value {
    let Some(state) = weak_set_state(&this) else {
        return Value::Undefined;
    };
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let weak = weak_value(&value, "WeakSet.add");
    let mut entries = state.borrow_mut();
    entries.retain(|stored| stored.upgrade().is_some());
    if !entries.iter().any(|stored| stored.matches(&value)) {
        entries.push(weak);
    }
    this
}

fn weak_set_has(this: Value, args: Vec<Value>) -> Value {
    let Some(state) = weak_set_state(&this) else {
        return Value::Bool(false);
    };
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let mut entries = state.borrow_mut();
    entries.retain(|stored| stored.upgrade().is_some());
    Value::Bool(entries.iter().any(|stored| stored.matches(&value)))
}

fn weak_set_delete(this: Value, args: Vec<Value>) -> Value {
    let Some(state) = weak_set_state(&this) else {
        return Value::Bool(false);
    };
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let mut entries = state.borrow_mut();
    let found = entries.iter().any(|stored| stored.matches(&value));
    entries.retain(|stored| stored.upgrade().is_some() && !stored.matches(&value));
    Value::Bool(found)
}

fn weak_ref_instance(args: &[Value], proto: &Value) -> Value {
    let target = args.first().cloned().unwrap_or(Value::Undefined);
    let weak = weak_value(&target, "WeakRef");
    let reference = instance(proto);
    let id = next_id();
    WEAK_REF_STATES.with(|states| states.borrow_mut().insert(id, weak));
    reference.set_property(STATE_KEY, Value::Number(id as f64));
    reference
}

fn weak_ref_deref(this: Value, _args: Vec<Value>) -> Value {
    let Some(id) = state_id(&this) else {
        return Value::Undefined;
    };
    WEAK_REF_STATES.with(|states| {
        states
            .borrow()
            .get(&id)
            .and_then(WeakValue::upgrade)
            .unwrap_or(Value::Undefined)
    })
}

fn finalization_instance(args: &[Value], proto: &Value) -> Value {
    FINALIZATION_WARNING_EMITTED.with(|emitted| {
        if !emitted.replace(true) {
            eprintln!(
                "warning: w3cos FinalizationRegistry uses explicit cleanupSome(); \
                 automatic GC callback timing is unavailable on this host"
            );
        }
    });
    let callback = args.first().cloned().unwrap_or(Value::Undefined);
    if !callback.is_function() {
        type_error("FinalizationRegistry requires a cleanup callback");
    }
    let registry = instance(proto);
    let id = next_id();
    FINALIZATION_STATES.with(|states| {
        states.borrow_mut().insert(
            id,
            Rc::new(RefCell::new(FinalizationState {
                callback,
                entries: Vec::new(),
            })),
        );
    });
    registry.set_property(STATE_KEY, Value::Number(id as f64));
    registry
}

fn finalization_state(value: &Value) -> Option<Rc<RefCell<FinalizationState>>> {
    let id = state_id(value)?;
    FINALIZATION_STATES.with(|states| states.borrow().get(&id).cloned())
}

fn drain_finalized(state: &Rc<RefCell<FinalizationState>>, override_callback: Option<Value>) {
    let (callback, held_values) = {
        let mut state = state.borrow_mut();
        let callback = override_callback.unwrap_or_else(|| state.callback.clone());
        let mut held_values = Vec::new();
        state.entries.retain(|entry| {
            if entry.target.upgrade().is_none() {
                held_values.push(entry.held_value.clone());
                false
            } else {
                true
            }
        });
        (callback, held_values)
    };
    for value in held_values {
        callback.call(Value::Undefined, vec![value]);
    }
}

fn finalization_register(this: Value, args: Vec<Value>) -> Value {
    let Some(state) = finalization_state(&this) else {
        return Value::Undefined;
    };
    drain_finalized(&state, None);
    let target_value = args.first().cloned().unwrap_or(Value::Undefined);
    let target = weak_value(&target_value, "FinalizationRegistry.register");
    let held_value = args.get(1).cloned().unwrap_or(Value::Undefined);
    if held_value.same_value_zero(&target_value) {
        type_error("FinalizationRegistry target and holdings must not be the same");
    }
    let unregister_token = args
        .get(2)
        .map(|value| weak_value(value, "FinalizationRegistry.register unregister token"));
    state.borrow_mut().entries.push(FinalizationEntry {
        target,
        held_value,
        unregister_token,
    });
    Value::Undefined
}

fn finalization_unregister(this: Value, args: Vec<Value>) -> Value {
    let Some(state) = finalization_state(&this) else {
        return Value::Bool(false);
    };
    drain_finalized(&state, None);
    let token = args.first().cloned().unwrap_or(Value::Undefined);
    if WeakValue::new(&token).is_none() {
        type_error("FinalizationRegistry.unregister requires an object token");
    }
    let mut state = state.borrow_mut();
    let old_len = state.entries.len();
    state.entries.retain(|entry| {
        !entry
            .unregister_token
            .as_ref()
            .is_some_and(|stored| stored.matches(&token))
    });
    Value::Bool(state.entries.len() < old_len)
}

fn finalization_cleanup_some(this: Value, args: Vec<Value>) -> Value {
    if let Some(state) = finalization_state(&this) {
        let callback = args.first().cloned().filter(Value::is_function);
        drain_finalized(&state, callback);
    }
    Value::Undefined
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
        class.set_property("cleanupMode", Value::string("explicit"));
        class.set_property("automaticCleanup", Value::Bool(false));
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PanicValue;
    use crate::class::{construct, instance_of};

    #[test]
    fn weak_map_and_set_do_not_keep_object_keys_alive() {
        let map = construct(&weak_map_class(), vec![]);
        let set = construct(&weak_set_class(), vec![]);
        // Host Rc object: page-local arena objects stay pinned until reset.
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
        let target = Value::array(vec![Value::Number(1.0)]);
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
}
