#![allow(clippy::collapsible_if)]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::heap::{HeapAllocation, HeapKind};
use crate::js_string::JsString;
use crate::property_map::PropertyMap;
use crate::proxy::ProxyHandler;
use crate::value::{FunctionData, JsObjectRef, Value};

#[derive(Clone)]
pub(crate) enum PrivateElement {
    Field(Value),
    Method(Value),
    Accessor {
        getter: Option<Value>,
        setter: Option<Value>,
    },
}

/// Rare class / private / enumerability state. Almost every ordinary object
/// never touches these; they live behind [`JsObject::rare`] so empty objects
/// stay slab-dense.
#[derive(Clone, Default)]
pub(crate) struct RareObjectData {
    /// Derived-class instance initializers waiting for their corresponding
    /// `super(...)` call to return. This is internal execution state, not an
    /// observable JavaScript property.
    pub(crate) pending_class_initializers: Vec<(u64, Value, Value)>,
    /// Opaque class brands and private elements are intentionally kept out of
    /// `properties`, so proxy traps and reflection cannot observe them.
    pub(crate) class_brand: Option<u64>,
    pub(crate) private_brands: HashSet<u64>,
    pub(crate) private_elements: HashMap<(u64, String), PrivateElement>,
    /// Own keys that are not enumerable. Ordinary data properties default to
    /// enumerable; `Object.defineProperty` can clear the flag so `for-in` and
    /// `CopyDataProperties` skip them, matching ECMAScript.
    non_enumerable: HashSet<String>,
}

/// A JavaScript-like dynamic object with string-keyed properties,
/// prototype chain, and optional Proxy handler for trap interception.
///
/// When `proxy_handler` is `Some`, property operations are routed through
/// the corresponding trap. When `None`, direct property-map access is used.
///
/// `call_slot` makes the object callable (like a JS class or function
/// object): `Value::call` on an object with a call slot invokes it.
pub struct JsObject {
    /// Own string-keyed data properties. Empty is heap-free; ≤4 stay inline.
    pub(crate) properties: PropertyMap,
    pub(crate) prototype: Option<JsObjectRef>,
    pub(crate) proxy_handler: Option<ProxyHandler>,
    has_getter_properties: bool,
    pub(crate) call_slot: Option<Rc<FunctionData>>,
    /// Lazily boxed rare class / private / enumerability fields. `None` on
    /// ordinary empty objects.
    pub(crate) rare: Option<Box<RareObjectData>>,
    heap_allocation: HeapAllocation,
}

impl JsObject {
    #[inline]
    pub(crate) fn heap_allocation(&self) -> &HeapAllocation {
        &self.heap_allocation
    }

    pub fn new() -> Self {
        let heap_allocation = HeapAllocation::new(HeapKind::Object, std::mem::size_of::<Self>());
        Self {
            properties: PropertyMap::new(),
            prototype: None,
            proxy_handler: None,
            has_getter_properties: false,
            call_slot: None,
            rare: None,
            heap_allocation,
        }
    }

    pub fn from_map(map: HashMap<String, Value>) -> Self {
        let properties: HashMap<JsString, Value> = map
            .into_iter()
            .map(|(key, value)| (JsString::intern(&key), value))
            .collect();
        Self::from_interned_map(properties)
    }

    fn from_interned_map(properties: HashMap<JsString, Value>) -> Self {
        let properties = PropertyMap::from_interned_hashmap(properties);
        let has_getter_properties =
            properties.any_key(|key| key.as_str().starts_with("__w3cos_getter_"));
        let heap_allocation = HeapAllocation::new(HeapKind::Object, std::mem::size_of::<Self>());
        let object = Self {
            properties,
            prototype: None,
            proxy_handler: None,
            has_getter_properties,
            call_slot: None,
            rare: None,
            heap_allocation,
        };
        object.refresh_heap_accounting();
        object
    }

    /// Create a proxied object: `new Proxy(target_props, handler)`.
    pub fn with_proxy(properties: HashMap<String, Value>, handler: ProxyHandler) -> Self {
        let properties: HashMap<JsString, Value> = properties
            .into_iter()
            .map(|(key, value)| (JsString::intern(&key), value))
            .collect();
        let properties = PropertyMap::from_interned_hashmap(properties);
        let has_getter_properties =
            properties.any_key(|key| key.as_str().starts_with("__w3cos_getter_"));
        let heap_allocation = HeapAllocation::new(HeapKind::Object, std::mem::size_of::<Self>());
        let object = Self {
            properties,
            prototype: None,
            proxy_handler: Some(handler),
            has_getter_properties,
            call_slot: None,
            rare: None,
            heap_allocation,
        };
        object.refresh_heap_accounting();
        object
    }

    /// Create a callable object (a JS class / constructor): plain properties
    /// plus a call slot invoked by `Value::call` / `class::construct`.
    pub fn with_call_slot(properties: HashMap<String, Value>, call: Rc<FunctionData>) -> Self {
        let properties: HashMap<JsString, Value> = properties
            .into_iter()
            .map(|(key, value)| (JsString::intern(&key), value))
            .collect();
        let properties = PropertyMap::from_interned_hashmap(properties);
        let has_getter_properties =
            properties.any_key(|key| key.as_str().starts_with("__w3cos_getter_"));
        let heap_allocation = HeapAllocation::new(HeapKind::Object, std::mem::size_of::<Self>());
        let object = Self {
            properties,
            prototype: None,
            proxy_handler: None,
            has_getter_properties,
            call_slot: Some(call),
            rare: None,
            heap_allocation,
        };
        object.refresh_heap_accounting();
        object
    }

    /// The call slot, if this object is callable.
    pub fn call_slot(&self) -> Option<&Rc<FunctionData>> {
        self.call_slot.as_ref()
    }

    /// Allocate rare class / private / enumerability state on first use.
    pub(crate) fn ensure_rare(&mut self) -> &mut RareObjectData {
        if self.rare.is_none() {
            self.rare = Some(Box::new(RareObjectData::default()));
        }
        self.rare.as_mut().expect("rare just allocated")
    }

    fn own_key_enumerable(&self, key: &str) -> bool {
        self.rare
            .as_ref()
            .map_or(true, |rare| !rare.non_enumerable.contains(key))
    }

    /// Mark an own property non-enumerable.
    pub(crate) fn mark_non_enumerable(&mut self, key: &str) {
        self.ensure_rare().non_enumerable.insert(key.to_string());
    }

    /// Whether `key` was marked non-enumerable (covers accessor properties,
    /// whose storage lives under internal slot names).
    pub(crate) fn rare_non_enumerable(&self, key: &str) -> bool {
        self.rare
            .as_ref()
            .is_some_and(|rare| rare.non_enumerable.contains(key))
    }

    /// Return the enumerability of an ordinary own property without
    /// materializing a JavaScript property-descriptor object.
    pub(crate) fn own_property_enumerability(&self, key: &str) -> Option<bool> {
        if self.has_own_property(key) {
            Some(self.own_key_enumerable(key))
        } else {
            None
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

    /// Read an own getter without invoking it or traversing the prototype.
    pub fn own_getter(&self, key: &str) -> Option<Value> {
        if !self.has_getter_properties {
            return None;
        }
        self.properties
            .get(format!("__w3cos_getter_{key}").as_str())
            .cloned()
    }

    /// Own data only, without invoking accessors, traps, or prototype lookup.
    pub fn own_data(&self, key: &str) -> Option<Value> {
        self.properties.get(key).cloned()
    }

    /// Read-only test used before delegating a prototype lookup to user code.
    pub fn has_get_trap(&self) -> bool {
        self.proxy_handler
            .as_ref()
            .is_some_and(|handler| handler.get.is_some())
    }

    /// Direct property lookup (no proxy), with prototype chain fallback.
    pub fn get_direct(&self, key: &str) -> Value {
        if let Some(val) = self.properties.get(key) {
            return val.clone();
        }
        // An own getter shadows prototype data even before it is invoked by
        // Value::get_property. In particular, a Document method accessor must
        // not resolve its prototype's forwarding method and recurse forever.
        if self.own_getter(key).is_some() || self.own_setter_only(key) {
            return Value::Undefined;
        }
        if let Some(ref proto) = self.prototype {
            if proto.is_live() {
                return proto.borrow().get_direct(key);
            }
        }
        Value::Undefined
    }

    /// An own accessor with a setter and no getter: `[[Get]]` yields
    /// `undefined` and must not consult the prototype.
    fn own_setter_only(&self, key: &str) -> bool {
        self.properties.may_have_marker_key()
            && self
                .properties
                .contains_key(format!("__w3cos_setter_{key}").as_str())
            && !self
                .properties
                .contains_key(format!("__w3cos_getter_{key}").as_str())
    }

    /// The getter `[[Get]]` of `key` runs: the first object on the chain
    /// owning `key` decides (data or a setter-only accessor yield `None`; a
    /// runtime accessor may also keep a placeholder data slot of its name).
    pub(crate) fn chain_getter(&self, key: &str) -> Option<Value> {
        if let Some(getter) = self.own_getter(key) {
            return Some(getter);
        }
        if self.properties.contains_key(key) || self.own_setter_only(key) {
            return None;
        }
        match &self.prototype {
            Some(proto) if proto.is_live() => proto.borrow().chain_getter(key),
            _ => None,
        }
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
        if key.starts_with("__w3cos_getter_") {
            self.has_getter_properties = true;
        }
        if self.properties.insert_str(key, value).is_none() {
            self.refresh_heap_accounting();
        }
    }

    /// Whether `[[Set]]` of a missing own key could find a
    /// `__w3cos_setter_` accessor (or must consult a proxy) on this object
    /// or its prototype chain. `false` is exact, so the caller may skip the
    /// setter lookup.
    pub(crate) fn chain_may_have_setter(&self) -> bool {
        if self.proxy_handler.is_some() || self.properties.may_have_marker_key() {
            return true;
        }
        match &self.prototype {
            Some(proto) if proto.is_live() => proto.borrow().chain_may_have_setter(),
            _ => false,
        }
    }

    /// Whether `key` is an own non-writable data property. Ordinary objects
    /// do not model `writable: false` (every own data property is
    /// writable), so only a proxy `getOwnPropertyDescriptor` trap can
    /// report one; only then is a descriptor materialized.
    pub(crate) fn own_property_non_writable(&self, key: &str) -> bool {
        let trapped = self
            .proxy_handler
            .as_ref()
            .is_some_and(|handler| handler.get_own_property_descriptor.is_some());
        if !trapped {
            return false;
        }
        let writable = self
            .get_own_property_descriptor(key)
            .get_property("writable");
        writable.is_bool() && !writable.to_bool()
    }

    pub fn may_have_getter_properties(&self) -> bool {
        self.has_getter_properties
            || self.prototype.as_ref().is_some_and(|prototype| {
                prototype.is_live() && prototype.borrow().may_have_getter_properties()
            })
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
        // Accessor halves are stored under marker keys.
        if self.properties.may_have_marker_key() && self.has_own_property(key) {
            return true;
        }
        if let Some(ref proto) = self.prototype {
            if proto.is_live() {
                return proto.borrow().has_direct(key);
            }
        }
        false
    }

    pub fn has_own_property(&self, key: &str) -> bool {
        self.properties.contains_key(key)
            || self
                .properties
                .contains_key(format!("__w3cos_getter_{key}").as_str())
            || self
                .properties
                .contains_key(format!("__w3cos_setter_{key}").as_str())
    }

    /// `[[Delete]]` — the `delete` operator.
    pub fn delete(&mut self, key: &str) -> bool {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.delete_property {
                let target = self.target_value();
                return trap(&target, key);
            }
        }
        let mut removed = self.properties.remove(key).is_some();
        // Deleting an accessor property drops both of its halves.
        if !key.starts_with("__w3cos_") && self.properties.may_have_marker_key() {
            let getter = self
                .properties
                .remove(format!("__w3cos_getter_{key}").as_str())
                .is_some();
            let setter = self
                .properties
                .remove(format!("__w3cos_setter_{key}").as_str())
                .is_some();
            if getter {
                self.has_getter_properties = self
                    .properties
                    .any_key(|key| key.as_str().starts_with("__w3cos_getter_"));
            }
            removed |= getter || setter;
        }
        if removed {
            if let Some(rare) = self.rare.as_mut() {
                rare.non_enumerable.remove(key);
            }
        }
        if removed && key.starts_with("__w3cos_getter_") {
            self.has_getter_properties = self
                .properties
                .any_key(|key| key.as_str().starts_with("__w3cos_getter_"));
        }
        if removed {
            self.refresh_heap_accounting();
        }
        true
    }

    /// `[[OwnKeys]]` — `Object.keys()` / `Reflect.ownKeys()`.
    pub fn own_keys(&self) -> Value {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.own_keys {
                let target = self.target_value();
                return trap(&target);
            }
        }
        Value::array(
            self.visible_own_keys()
                .into_iter()
                .map(Value::from)
                .collect(),
        )
    }

    /// Own property names in insertion order, with the internal
    /// `__w3cos_getter_{key}` / `__w3cos_setter_{key}` slots reported as the
    /// accessor property `key` they implement (once).
    pub fn visible_own_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for key in self.properties.keys() {
            let key = key.as_str();
            let name = key
                .strip_prefix("__w3cos_getter_")
                .or_else(|| key.strip_prefix("__w3cos_setter_"))
                .unwrap_or(key);
            if seen.insert(name.to_string()) {
                keys.push(name.to_string());
            }
        }
        keys
    }

    /// `[[GetOwnPropertyDescriptor]]`.
    pub fn get_own_property_descriptor(&self, key: &str) -> Value {
        if let Some(ref handler) = self.proxy_handler {
            if let Some(ref trap) = handler.get_own_property_descriptor {
                let target = self.target_value();
                return trap(&target, key);
            }
        }
        if let Some(value) = self.properties.get(key) {
            let mut desc = HashMap::new();
            desc.insert("value".into(), value.clone());
            desc.insert("writable".into(), Value::Bool(true));
            desc.insert(
                "enumerable".into(),
                Value::Bool(self.own_key_enumerable(key)),
            );
            desc.insert("configurable".into(), Value::Bool(true));
            Value::object(desc)
        } else {
            // Getter/setter convention: a `__w3cos_getter_{key}` /
            // `__w3cos_setter_{key}` own function is an accessor property
            // named `key` (see esm_codegen class emission).
            let getter = self
                .properties
                .get(format!("__w3cos_getter_{key}").as_str());
            let setter = self
                .properties
                .get(format!("__w3cos_setter_{key}").as_str());
            if getter.is_none() && setter.is_none() {
                return Value::Undefined;
            }
            let mut desc = HashMap::new();
            if let Some(getter) = getter {
                desc.insert("get".into(), getter.clone());
            }
            if let Some(setter) = setter {
                desc.insert("set".into(), setter.clone());
            }
            desc.insert(
                "enumerable".into(),
                Value::Bool(self.own_key_enumerable(key)),
            );
            desc.insert("configurable".into(), Value::Bool(true));
            Value::object(desc)
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
        self.define_property_direct(key, descriptor)
    }

    pub(crate) fn define_property_direct(&mut self, key: &str, descriptor: &Value) -> bool {
        if let Some(desc) = descriptor.as_object() {
            let desc = desc.borrow();
            if let Some(val) = desc.properties.get("value") {
                self.properties.insert(JsString::intern(key), val.clone());
                self.properties.remove(&format!("__w3cos_getter_{key}"));
                self.properties.remove(&format!("__w3cos_setter_{key}"));
            }
            if let Some(getter) = desc.properties.get("get") {
                self.properties.remove(key);
                self.properties.insert(
                    JsString::intern(&format!("__w3cos_getter_{key}")),
                    getter.clone(),
                );
                self.has_getter_properties = true;
            }
            if let Some(setter) = desc.properties.get("set") {
                self.properties.remove(key);
                self.properties.insert(
                    JsString::intern(&format!("__w3cos_setter_{key}")),
                    setter.clone(),
                );
            }
            if let Some(enumerable) = desc.properties.get("enumerable") {
                if enumerable.to_bool() {
                    if let Some(rare) = self.rare.as_mut() {
                        rare.non_enumerable.remove(key);
                    }
                } else {
                    self.ensure_rare().non_enumerable.insert(key.to_string());
                }
            }
            self.refresh_heap_accounting();
        }
        true
    }

    /// Prototype link when no `getPrototypeOf` trap would run.
    ///
    /// `None` means a trap is installed. Callers that must not invoke user
    /// code use this instead of [`Self::get_prototype_of`].
    pub fn untrapped_prototype(&self) -> Option<Value> {
        if self
            .proxy_handler
            .as_ref()
            .is_some_and(|handler| handler.get_prototype_of.is_some())
        {
            return None;
        }
        Some(match &self.prototype {
            Some(proto) if proto.is_live() => proto.as_value(),
            _ => Value::Null,
        })
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
            Some(proto) if proto.is_live() => proto.as_value(),
            _ => Value::Null,
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
            _ if proto.as_object().is_some() => {
                self.prototype = Some(proto.as_object().expect("object"));
                true
            }
            _ if proto.is_null() => {
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
        self.properties
            .keys()
            .map(|key| key.as_str().to_string())
            .collect()
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

    pub(crate) fn refresh_heap_accounting(&self) {
        let property_bytes = self.properties.heap_bytes();
        let mut rare_bytes = 0usize;
        if let Some(rare) = self.rare.as_ref() {
            let pending_bytes = rare
                .pending_class_initializers
                .capacity()
                .saturating_mul(std::mem::size_of::<(u64, Value, Value)>());
            let private_brand_bytes = rare
                .private_brands
                .capacity()
                .saturating_mul(std::mem::size_of::<u64>());
            let private_element_bytes = rare
                .private_elements
                .capacity()
                .saturating_mul(std::mem::size_of::<((u64, String), PrivateElement)>())
                .saturating_add(
                    rare.private_elements
                        .keys()
                        .map(|(_, name)| name.capacity())
                        .fold(0usize, usize::saturating_add),
                );
            let non_enumerable_bytes = rare
                .non_enumerable
                .iter()
                .map(|key| key.capacity())
                .fold(0usize, usize::saturating_add);
            rare_bytes = std::mem::size_of::<RareObjectData>()
                .saturating_add(pending_bytes)
                .saturating_add(private_brand_bytes)
                .saturating_add(private_element_bytes)
                .saturating_add(non_enumerable_bytes);
        }
        self.heap_allocation.set_bytes(
            std::mem::size_of::<Self>()
                .saturating_add(property_bytes)
                .saturating_add(rare_bytes),
        );
    }

    /// Snapshot the raw properties as a `Value::Object` (used as `target` arg for traps).
    pub(crate) fn target_value(&self) -> Value {
        let heap_allocation = HeapAllocation::new(HeapKind::Object, std::mem::size_of::<Self>());
        let rare = self.rare.as_ref().map(|rare| {
            Box::new(RareObjectData {
                pending_class_initializers: Vec::new(),
                class_brand: rare.class_brand,
                private_brands: HashSet::new(),
                private_elements: HashMap::new(),
                non_enumerable: rare.non_enumerable.clone(),
            })
        });
        let clone = Self {
            properties: self.properties.clone(),
            prototype: self.prototype.clone(),
            proxy_handler: None,
            has_getter_properties: self.has_getter_properties,
            call_slot: self.call_slot.clone(),
            rare,
            heap_allocation,
        };
        clone.refresh_heap_accounting();
        Value::Object(Rc::new(RefCell::new(clone)))
    }
}

impl Default for JsObject {
    fn default() -> Self {
        Self::new()
    }
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
    fn own_property_check_includes_accessors_without_walking_the_prototype() {
        let prototype = Value::object(HashMap::from([(
            "inherited".to_string(),
            Value::Bool(true),
        )]))
        .as_object()
        .expect("object prototype");
        let mut obj = JsObject::new();
        obj.prototype = Some(prototype);
        obj.set_direct(
            "__w3cos_getter_length",
            Value::function(|_, _| Value::Number(1.0)),
        );

        assert!(obj.has_own_property("length"));
        assert!(!obj.has_own_property("inherited"));
        assert!(obj.has_direct("inherited"));
    }

    #[test]
    fn define_property_can_clear_enumerable() {
        let mut obj =
            JsObject::from_map(HashMap::from([("hidden".to_string(), Value::Number(1.0))]));
        assert!(
            obj.get_own_property_descriptor("hidden")
                .get_property("enumerable")
                .to_bool()
        );
        obj.define_property(
            "hidden",
            &Value::object(HashMap::from([
                ("value".into(), Value::Number(1.0)),
                ("enumerable".into(), Value::Bool(false)),
            ])),
        );
        assert!(
            !obj.get_own_property_descriptor("hidden")
                .get_property("enumerable")
                .to_bool()
        );
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
    fn empty_and_small_objects_keep_compact_property_map() {
        let empty = JsObject::new();
        assert!(empty.properties.is_compact());
        assert!(empty.properties.is_empty());
        assert_eq!(empty.properties.heap_bytes(), 0);

        let mut small = JsObject::new();
        small.set_direct("a", Value::Number(1.0));
        small.set_direct("b", Value::Number(2.0));
        small.set_direct("c", Value::Number(3.0));
        small.set_direct("d", Value::Number(4.0));
        assert!(
            small.properties.is_compact(),
            "<=4 own properties must stay off HashMap"
        );
        assert_eq!(small.keys(), vec!["a", "b", "c", "d"]);

        small.set_direct("e", Value::Number(5.0));
        assert!(
            small.properties.is_compact(),
            "shape-backed objects stay compact past 4 keys"
        );
        small.delete("e");
        assert!(
            !small.properties.is_compact(),
            "a delete switches to dictionary mode"
        );
        assert_eq!(small.get_direct("a").to_number(), 1.0);
        assert!(small.get_direct("e").is_undefined());

        // Rare class/private/enumerability state is lazy-boxed so ordinary empty
        // objects shrink vs the prior inline HashSet/HashMap/Vec layout (472 /
        // RefCell 480). PropertyMap stays as-is.
        assert_eq!(
            std::mem::size_of::<JsObject>(),
            296,
            "lazy-boxed rare fields should keep JsObject at 296B (was 472)"
        );
        assert_eq!(
            std::mem::size_of::<RefCell<JsObject>>(),
            304,
            "RefCell<JsObject> should be 304B for denser page-arena slabs (was 480)"
        );
        assert!(
            empty.rare.is_none(),
            "ordinary empty objects must not allocate RareObjectData"
        );
    }

    #[test]
    fn define_property_non_enumerable_allocates_rare_once() {
        let mut obj = JsObject::new();
        assert!(obj.rare.is_none());
        obj.define_property(
            "hidden",
            &Value::object(HashMap::from([
                ("value".into(), Value::Number(1.0)),
                ("enumerable".into(), Value::Bool(false)),
            ])),
        );
        assert!(obj.rare.is_some());
        assert!(
            !obj.get_own_property_descriptor("hidden")
                .get_property("enumerable")
                .to_bool()
        );
        obj.delete("hidden");
        // rare box may remain; enumerability set should no longer list the key
        assert!(obj.own_key_enumerable("hidden"));
    }

    #[test]
    fn property_keys_are_interned_across_objects() {
        let mut left = JsObject::new();
        let mut right = JsObject::new();
        left.set_direct("shared-key", Value::Number(1.0));
        right.set_direct("shared-key", Value::Number(2.0));
        let left_key = left.properties.keys().next().cloned().unwrap();
        let right_key = right.properties.keys().next().cloned().unwrap();
        assert!(left_key.ptr_eq(&right_key));
        assert_eq!(left.keys(), vec!["shared-key".to_string()]);
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
        child.prototype = Some(JsObjectRef::from_host(parent_rc));
        child.set_direct("own", Value::Number(1.0));

        assert_eq!(child.get_direct("own").to_number(), 1.0);
        assert_eq!(child.get_direct("inherited").to_number(), 99.0);
        assert!(child.get_direct("missing").is_undefined());
    }
}

#[cfg(feature = "leak-detector")]
impl JsObject {
    /// Visible strong edges for [`crate::leak_detector`]; returns `true` when
    /// part of the object (a proxy handler) cannot be enumerated.
    pub(crate) fn leak_edges(
        &self,
        out: &mut Vec<(String, crate::leak_detector::LeakTarget)>,
    ) -> bool {
        use crate::value::leak_edges::push_value;
        for key in self.properties.keys() {
            if let Some(value) = self.properties.get(key.as_str()) {
                push_value(out, format!(".{}", key.as_str()), value);
            }
        }
        if let Some(prototype) = &self.prototype {
            out.push(("[[Prototype]]".to_string(), prototype.leak_target()));
        }
        if let Some(call) = &self.call_slot {
            out.push((
                "[[Call]]".to_string(),
                crate::leak_detector::LeakTarget::HostFunction(
                    std::rc::Rc::as_ptr(call),
                    std::rc::Rc::strong_count(call) as u32,
                ),
            ));
        }
        if let Some(rare) = &self.rare {
            for (_, a, b) in &rare.pending_class_initializers {
                push_value(out, "[[PendingInitializer]]".to_string(), a);
                push_value(out, "[[PendingInitializer]]".to_string(), b);
            }
            let mut private = rare.private_elements.iter().collect::<Vec<_>>();
            private.sort_by(|a, b| a.0.1.cmp(&b.0.1));
            for ((_, name), element) in private {
                match element {
                    PrivateElement::Field(value) | PrivateElement::Method(value) => {
                        push_value(out, format!(".#{name}"), value)
                    }
                    PrivateElement::Accessor { getter, setter } => {
                        if let Some(getter) = getter {
                            push_value(out, format!(".#{name} get"), getter);
                        }
                        if let Some(setter) = setter {
                            push_value(out, format!(".#{name} set"), setter);
                        }
                    }
                }
            }
        }
        self.proxy_handler.is_some()
    }

    pub(crate) fn leak_keys(&self, limit: usize) -> (Vec<String>, usize) {
        let keys = self
            .properties
            .keys()
            .take(limit)
            .map(|key| key.as_str().to_string())
            .collect();
        (keys, self.properties.len())
    }
}
