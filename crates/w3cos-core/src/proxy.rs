use std::rc::Rc;

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
    fn default() -> Self { Self::new() }
}

/// Fluent builder for constructing a `ProxyHandler` trap by trap.
///
/// Mirrors the pattern `new Proxy(target, { get(...){}, set(...){} })`.
pub struct ProxyBuilder {
    handler: ProxyHandler,
}

impl ProxyBuilder {
    pub fn new() -> Self {
        Self { handler: ProxyHandler::new() }
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

    pub fn define_property(
        mut self,
        f: impl Fn(&Value, &str, &Value) -> bool + 'static,
    ) -> Self {
        self.handler.define_property = Some(Rc::new(f));
        self
    }

    pub fn own_keys(mut self, f: impl Fn(&Value) -> Value + 'static) -> Self {
        self.handler.own_keys = Some(Rc::new(f));
        self
    }

    pub fn apply(
        mut self,
        f: impl Fn(&Value, &Value, Vec<Value>) -> Value + 'static,
    ) -> Self {
        self.handler.apply = Some(Rc::new(f));
        self
    }

    pub fn construct(
        mut self,
        f: impl Fn(&Value, Vec<Value>, &Value) -> Value + 'static,
    ) -> Self {
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
    fn default() -> Self { Self::new() }
}
