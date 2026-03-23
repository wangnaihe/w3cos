#![allow(clippy::collapsible_if)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::proxy::ProxyHandler;
use crate::value::Value;

/// A JavaScript-like dynamic object with string-keyed properties,
/// prototype chain, and optional Proxy handler for trap interception.
///
/// When `proxy_handler` is `Some`, property operations are routed through
/// the corresponding trap. When `None`, direct HashMap access is used.
pub struct JsObject {
    pub(crate) properties: HashMap<String, Value>,
    pub(crate) prototype: Option<Rc<RefCell<JsObject>>>,
    pub(crate) proxy_handler: Option<ProxyHandler>,
}

impl JsObject {
    pub fn new() -> Self {
        Self {
            properties: HashMap::new(),
            prototype: None,
            proxy_handler: None,
        }
    }

    pub fn from_map(map: HashMap<String, Value>) -> Self {
        Self {
            properties: map,
            prototype: None,
            proxy_handler: None,
        }
    }

    /// Create a proxied object: `new Proxy(target_props, handler)`.
    pub fn with_proxy(properties: HashMap<String, Value>, handler: ProxyHandler) -> Self {
        Self {
            properties,
            prototype: None,
            proxy_handler: Some(handler),
        }
    }

    // ── Proxy-aware property access ────────────────────────────────────

    /// `[[Get]]` — routes through `handler.get` if present.
    pub fn get(&self, key: &str, receiver: &Value) -> Value {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.get {
                let target = self.target_value();
                return trap(&target, key, receiver);
            }
        }
        self.get_direct(key)
    }

    /// Direct property lookup (no proxy), with prototype chain fallback.
    pub fn get_direct(&self, key: &str) -> Value {
        if let Some(val) = self.properties.get(key) {
            return val.clone();
        }
        if let Some(ref proto) = self.prototype {
            return proto.borrow().get_direct(key);
        }
        Value::Undefined
    }

    /// `[[Set]]` — routes through `handler.set` if present.
    pub fn set(&mut self, key: &str, value: Value, receiver: &Value) -> bool {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.set {
                let target = self.target_value();
                return trap(&target, key, value, receiver);
            }
        }
        self.set_direct(key, value);
        true
    }

    pub fn set_direct(&mut self, key: &str, value: Value) {
        self.properties.insert(key.to_string(), value);
    }

    /// `[[Has]]` — the `in` operator.
    pub fn has(&self, key: &str) -> bool {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.has {
                let target = self.target_value();
                return trap(&target, key);
            }
        }
        self.has_direct(key)
    }

    pub fn has_direct(&self, key: &str) -> bool {
        if self.properties.contains_key(key) {
            return true;
        }
        if let Some(ref proto) = self.prototype {
            return proto.borrow().has_direct(key);
        }
        false
    }

    /// `[[Delete]]` — the `delete` operator.
    pub fn delete(&mut self, key: &str) -> bool {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.delete_property {
                let target = self.target_value();
                return trap(&target, key);
            }
        }
        self.properties.remove(key).is_some()
    }

    /// `[[OwnKeys]]` — `Object.keys()` / `Reflect.ownKeys()`.
    pub fn own_keys(&self) -> Value {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.own_keys {
                let target = self.target_value();
                return trap(&target);
            }
        }
        let keys: Vec<Value> = self.properties.keys().map(|k| Value::String(k.clone())).collect();
        Value::array(keys)
    }

    /// `[[GetOwnPropertyDescriptor]]`.
    pub fn get_own_property_descriptor(&self, key: &str) -> Value {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.get_own_property_descriptor {
                let target = self.target_value();
                return trap(&target, key);
            }
        }
        if self.properties.contains_key(key) {
            let mut desc = HashMap::new();
            desc.insert("value".into(), self.properties[key].clone());
            desc.insert("writable".into(), Value::Bool(true));
            desc.insert("enumerable".into(), Value::Bool(true));
            desc.insert("configurable".into(), Value::Bool(true));
            Value::object(desc)
        } else {
            Value::Undefined
        }
    }

    /// `[[DefineProperty]]`.
    pub fn define_property(&mut self, key: &str, descriptor: &Value) -> bool {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.define_property {
                let target = self.target_value();
                return trap(&target, key, descriptor);
            }
        }
        if let Value::Object(desc) = descriptor {
            let desc = desc.borrow();
            if let Some(val) = desc.properties.get("value") {
                self.properties.insert(key.to_string(), val.clone());
            }
        }
        true
    }

    /// `[[GetPrototypeOf]]`.
    pub fn get_prototype_of(&self) -> Value {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.get_prototype_of {
                let target = self.target_value();
                return trap(&target);
            }
        }
        match &self.prototype {
            Some(proto) => Value::Object(proto.clone()),
            None => Value::Null,
        }
    }

    /// `[[SetPrototypeOf]]`.
    pub fn set_prototype_of(&mut self, proto: &Value) -> bool {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.set_prototype_of {
                let target = self.target_value();
                return trap(&target, proto);
            }
        }
        match proto {
            Value::Object(obj) => {
                self.prototype = Some(obj.clone());
                true
            }
            Value::Null => {
                self.prototype = None;
                true
            }
            _ => false,
        }
    }

    /// `[[IsExtensible]]`.
    pub fn is_extensible(&self) -> bool {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.is_extensible {
                let target = self.target_value();
                return trap(&target);
            }
        }
        true
    }

    /// `[[PreventExtensions]]`.
    pub fn prevent_extensions(&self) -> bool {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.prevent_extensions {
                let target = self.target_value();
                return trap(&target);
            }
        }
        false
    }

    // ── Helpers ────────────────────────────────────────────────────────

    pub fn keys(&self) -> Vec<String> {
        self.properties.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.properties.len()
    }

    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    pub fn is_proxy(&self) -> bool {
        self.proxy_handler.is_some()
    }

    /// Snapshot the raw properties as a `Value::Object` (used as `target` arg for traps).
    fn target_value(&self) -> Value {
        let clone = JsObject {
            properties: self.properties.clone(),
            prototype: self.prototype.clone(),
            proxy_handler: None,
        };
        Value::Object(Rc::new(RefCell::new(clone)))
    }
}

impl Default for JsObject {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::ProxyBuilder;

    #[test]
    fn basic_get_set() {
        let mut obj = JsObject::new();
        obj.set_direct("x", Value::Number(42.0));
        assert_eq!(obj.get_direct("x").to_number(), 42.0);
        assert!(obj.get_direct("y").is_undefined());
    }

    #[test]
    fn has_and_delete() {
        let mut obj = JsObject::new();
        obj.set_direct("a", Value::Bool(true));
        assert!(obj.has_direct("a"));
        assert!(!obj.has_direct("b"));
        obj.delete("a");
        assert!(!obj.has_direct("a"));
    }

    #[test]
    fn proxy_get_trap() {
        let handler = ProxyBuilder::new()
            .get(|_target, key, _receiver| {
                if key == "secret" {
                    Value::String("intercepted".into())
                } else {
                    Value::Undefined
                }
            })
            .build();

        let mut props = HashMap::new();
        props.insert("secret".into(), Value::String("original".into()));
        let obj = JsObject::with_proxy(props, handler);
        let receiver = Value::Undefined;

        let val = obj.get("secret", &receiver);
        assert_eq!(val.to_js_string(), "intercepted");
    }

    #[test]
    fn proxy_set_trap() {
        use std::cell::Cell;
        use std::rc::Rc as StdRc;

        let set_called = StdRc::new(Cell::new(false));
        let set_called_clone = set_called.clone();

        let handler = ProxyBuilder::new()
            .set(move |_target, _key, _value, _receiver| {
                set_called_clone.set(true);
                true
            })
            .build();

        let mut obj = JsObject::with_proxy(HashMap::new(), handler);
        let receiver = Value::Undefined;
        obj.set("x", Value::Number(1.0), &receiver);
        assert!(set_called.get());
    }

    #[test]
    fn proxy_has_trap() {
        let handler = ProxyBuilder::new()
            .has(|_target, key| key == "magic")
            .build();

        let obj = JsObject::with_proxy(HashMap::new(), handler);
        assert!(obj.has("magic"));
        assert!(!obj.has("other"));
    }

    #[test]
    fn proxy_own_keys_trap() {
        let handler = ProxyBuilder::new()
            .own_keys(|_target| {
                Value::array(vec![Value::String("a".into()), Value::String("b".into())])
            })
            .build();

        let obj = JsObject::with_proxy(HashMap::new(), handler);
        let keys = obj.own_keys();
        assert_eq!(keys.to_js_string(), "a,b");
    }

    #[test]
    fn prototype_chain() {
        let mut parent = JsObject::new();
        parent.set_direct("inherited", Value::Number(99.0));
        let parent_rc = Rc::new(RefCell::new(parent));

        let mut child = JsObject::new();
        child.prototype = Some(parent_rc);
        child.set_direct("own", Value::Number(1.0));

        assert_eq!(child.get_direct("own").to_number(), 1.0);
        assert_eq!(child.get_direct("inherited").to_number(), 99.0);
        assert!(child.get_direct("missing").is_undefined());
    }
}
