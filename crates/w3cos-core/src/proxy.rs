use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::object::JsObject;
use crate::value::Value;

type TrapGetFn = dyn Fn(&Value, &str, &Value) -> Value;
type TrapSetFn = dyn Fn(&Value, &str, Value, &Value) -> bool;
type TrapHasFn = dyn Fn(&Value, &str) -> bool;
type TrapKeyFn = dyn Fn(&Value, &str) -> bool;
type TrapDescFn = dyn Fn(&Value, &str) -> Value;
type TrapDefFn = dyn Fn(&Value, &str, &Value) -> bool;
type TrapKeysFn = dyn Fn(&Value) -> Value;
type TrapApplyFn = dyn Fn(&Value, &Value, Vec<Value>) -> Value;
type TrapConstructFn = dyn Fn(&Value, Vec<Value>, &Value) -> Value;
type TrapProtoFn = dyn Fn(&Value) -> Value;
type TrapSetProtoFn = dyn Fn(&Value, &Value) -> bool;
type TrapBoolFn = dyn Fn(&Value) -> bool;

/// All 13 ECMAScript Proxy handler traps.
///
/// Each trap is an optional closure. When `None`, the default behavior
/// (direct operation on the target object) is used.
///
/// Trap signatures follow the ES spec:
///   - `target`: the original (unwrapped) target object as `Value`
///   - `receiver`/`this_arg`: the proxy itself or the `this` binding
///   - return types match the spec (bool for invariant-checked traps, Value otherwise)
#[allow(clippy::type_complexity)]
pub struct ProxyHandler {
    pub get: Option<Rc<TrapGetFn>>,
    pub set: Option<Rc<TrapSetFn>>,
    pub has: Option<Rc<TrapHasFn>>,
    pub delete_property: Option<Rc<TrapKeyFn>>,
    pub get_own_property_descriptor: Option<Rc<TrapDescFn>>,
    pub define_property: Option<Rc<TrapDefFn>>,
    pub own_keys: Option<Rc<TrapKeysFn>>,
    pub apply: Option<Rc<TrapApplyFn>>,
    pub construct: Option<Rc<TrapConstructFn>>,
    pub get_prototype_of: Option<Rc<TrapProtoFn>>,
    pub set_prototype_of: Option<Rc<TrapSetProtoFn>>,
    pub is_extensible: Option<Rc<TrapBoolFn>>,
    pub prevent_extensions: Option<Rc<TrapBoolFn>>,
}

impl ProxyHandler {
    pub fn new() -> Self {
        Self {
            get: None,
            set: None,
            has: None,
            delete_property: None,
            get_own_property_descriptor: None,
            define_property: None,
            own_keys: None,
            apply: None,
            construct: None,
            get_prototype_of: None,
            set_prototype_of: None,
            is_extensible: None,
            prevent_extensions: None,
        }
    }
}

impl Default for ProxyHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Fluent builder for constructing a `ProxyHandler` trap by trap.
///
/// Mirrors the pattern `new Proxy(target, { get(...){}, set(...){} })`.
pub struct ProxyBuilder {
    handler: ProxyHandler,
}

impl ProxyBuilder {
    pub fn new() -> Self {
        Self {
            handler: ProxyHandler::new(),
        }
    }

    pub fn get(mut self, f: impl Fn(&Value, &str, &Value) -> Value + 'static) -> Self {
        self.handler.get = Some(Rc::new(f));
        self
    }

    pub fn set(mut self, f: impl Fn(&Value, &str, Value, &Value) -> bool + 'static) -> Self {
        self.handler.set = Some(Rc::new(f));
        self
    }

    pub fn has(mut self, f: impl Fn(&Value, &str) -> bool + 'static) -> Self {
        self.handler.has = Some(Rc::new(f));
        self
    }

    pub fn delete_property(mut self, f: impl Fn(&Value, &str) -> bool + 'static) -> Self {
        self.handler.delete_property = Some(Rc::new(f));
        self
    }

    pub fn get_own_property_descriptor(
        mut self,
        f: impl Fn(&Value, &str) -> Value + 'static,
    ) -> Self {
        self.handler.get_own_property_descriptor = Some(Rc::new(f));
        self
    }

    pub fn define_property(mut self, f: impl Fn(&Value, &str, &Value) -> bool + 'static) -> Self {
        self.handler.define_property = Some(Rc::new(f));
        self
    }

    pub fn own_keys(mut self, f: impl Fn(&Value) -> Value + 'static) -> Self {
        self.handler.own_keys = Some(Rc::new(f));
        self
    }

    pub fn apply(mut self, f: impl Fn(&Value, &Value, Vec<Value>) -> Value + 'static) -> Self {
        self.handler.apply = Some(Rc::new(f));
        self
    }

    pub fn construct(mut self, f: impl Fn(&Value, Vec<Value>, &Value) -> Value + 'static) -> Self {
        self.handler.construct = Some(Rc::new(f));
        self
    }

    pub fn get_prototype_of(mut self, f: impl Fn(&Value) -> Value + 'static) -> Self {
        self.handler.get_prototype_of = Some(Rc::new(f));
        self
    }

    pub fn set_prototype_of(mut self, f: impl Fn(&Value, &Value) -> bool + 'static) -> Self {
        self.handler.set_prototype_of = Some(Rc::new(f));
        self
    }

    pub fn is_extensible(mut self, f: impl Fn(&Value) -> bool + 'static) -> Self {
        self.handler.is_extensible = Some(Rc::new(f));
        self
    }

    pub fn prevent_extensions(mut self, f: impl Fn(&Value) -> bool + 'static) -> Self {
        self.handler.prevent_extensions = Some(Rc::new(f));
        self
    }

    pub fn build(self) -> ProxyHandler {
        self.handler
    }
}

impl Default for ProxyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// The dynamic `Proxy` constructor used by ESM-lowered code.
///
/// Handler properties are ordinary JavaScript function values. The native
/// proxy callbacks forward operations to those functions while retaining the
/// original target value supplied to `new Proxy(target, handler)`.
pub fn proxy_class() -> Value {
    Value::callable(HashMap::new(), |_this, args| {
        let target = args.first().cloned().unwrap_or(Value::Undefined);
        let handler = args.get(1).cloned().unwrap_or(Value::Undefined);
        create_dynamic_proxy(target, handler)
    })
}

fn create_dynamic_proxy(target: Value, handler: Value) -> Value {
    let mut properties = HashMap::new();
    if let Value::Object(object) = &target {
        let object = object.borrow();
        for key in object.keys() {
            properties.insert(key.clone(), object.get_direct(&key));
        }
    }

    let mut traps = ProxyHandler::new();

    let get = handler.get_property("get");
    if get.is_function() || get.is_object() {
        let original_target = target.clone();
        let handler = handler.clone();
        traps.get = Some(Rc::new(move |_snapshot, key, receiver| {
            get.call(
                handler.clone(),
                vec![
                    original_target.clone(),
                    Value::String(key.to_string()),
                    receiver.clone(),
                ],
            )
        }));
    }

    let set = handler.get_property("set");
    if set.is_function() || set.is_object() {
        let original_target = target.clone();
        let handler = handler.clone();
        traps.set = Some(Rc::new(move |_snapshot, key, value, receiver| {
            set.call(
                handler.clone(),
                vec![
                    original_target.clone(),
                    Value::String(key.to_string()),
                    value,
                    receiver.clone(),
                ],
            )
            .to_bool()
        }));
    }

    let has = handler.get_property("has");
    if has.is_function() || has.is_object() {
        let original_target = target.clone();
        let handler = handler.clone();
        traps.has = Some(Rc::new(move |_snapshot, key| {
            has.call(
                handler.clone(),
                vec![original_target.clone(), Value::String(key.to_string())],
            )
            .to_bool()
        }));
    }

    let delete_property = handler.get_property("deleteProperty");
    if delete_property.is_function() || delete_property.is_object() {
        let original_target = target.clone();
        let handler = handler.clone();
        traps.delete_property = Some(Rc::new(move |_snapshot, key| {
            delete_property
                .call(
                    handler.clone(),
                    vec![original_target.clone(), Value::String(key.to_string())],
                )
                .to_bool()
        }));
    }

    let get_prototype_of = handler.get_property("getPrototypeOf");
    if get_prototype_of.is_function() || get_prototype_of.is_object() {
        let original_target = target;
        let handler = handler.clone();
        traps.get_prototype_of = Some(Rc::new(move |_snapshot| {
            get_prototype_of.call(handler.clone(), vec![original_target.clone()])
        }));
    }

    Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        properties, traps,
    ))))
}

#[cfg(test)]
mod dynamic_tests {
    use super::*;
    use crate::class;

    #[test]
    fn dynamic_proxy_forwards_get_set_and_get_prototype_of() {
        let target = Value::object(HashMap::from([("answer".into(), Value::Number(41.0))]));
        let prototype = Value::object(HashMap::new());
        let stored = Rc::new(RefCell::new(Value::Undefined));
        let stored_for_set = stored.clone();
        let stored_for_get = stored.clone();
        let prototype_for_trap = prototype.clone();
        let handler = Value::object(HashMap::from([
            (
                "get".into(),
                Value::function(move |_, args| {
                    let key = args.get(1).cloned().unwrap_or_default().to_js_string();
                    if key == "stored" {
                        stored_for_get.borrow().clone()
                    } else {
                        args.first().cloned().unwrap_or_default().get_property(&key)
                    }
                }),
            ),
            (
                "set".into(),
                Value::function(move |_, args| {
                    *stored_for_set.borrow_mut() = args.get(2).cloned().unwrap_or_default();
                    Value::Bool(true)
                }),
            ),
            (
                "getPrototypeOf".into(),
                Value::function(move |_, _| prototype_for_trap.clone()),
            ),
        ]));

        let proxy = class::construct(&proxy_class(), vec![target, handler]);
        assert_eq!(proxy.get_property("answer").to_number(), 41.0);
        proxy.set_property("stored", Value::Number(42.0));
        assert_eq!(proxy.get_property("stored").to_number(), 42.0);
        assert!(class::get_prototype_of(&proxy).strict_eq(&prototype));
    }
}
