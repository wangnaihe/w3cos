//! Runtime support for JavaScript classes in the ESM compile pipeline.
//!
//! A JS class is a *callable object*: a `Value::Object` whose `JsObject` has a
//! call slot (see `Value::callable`). The generated code stores the raw
//! constructor under the `"__w3cos_ctor"` key and the prototype object under
//! `"prototype"`. These helpers implement construction, `instanceof`, and
//! `super` semantics on top of that representation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Value;
use crate::object::PrivateElement;

static NEXT_PRIVATE_BRAND: AtomicU64 = AtomicU64::new(1);

/// Builds the shared runtime representation used by both generated AOT code
/// and W3VM class instructions.
pub fn create_class(constructor: &Value, super_class: Option<&Value>) -> Value {
    create_class_with_initializer(constructor, super_class, &Value::Undefined)
}

/// Builds a class with a separate instance-field initializer closure.
///
/// Base initializers run immediately before the user constructor. Derived
/// initializers are queued privately on the instance and run when that
/// class's `super(...)` call returns. A stack preserves the ECMAScript order
/// across multiple inheritance levels without exposing backend-specific
/// scheduling to AOT code or W3VM.
pub fn create_class_with_initializer(
    constructor: &Value,
    super_class: Option<&Value>,
    initializer: &Value,
) -> Value {
    let brand_id = NEXT_PRIVATE_BRAND.fetch_add(1, Ordering::Relaxed);
    let class_cell = Rc::new(RefCell::new(Value::Undefined));
    let prototype = Value::object(HashMap::new());
    if let Some(parent) = super_class {
        set_prototype_of(&prototype, &parent.get_property("prototype"));
    }

    let raw_constructor = if constructor.is_undefined() {
        if let Some(parent) = super_class {
            let parent = parent.clone();
            Value::function(move |this_value, arguments| {
                super_ctor(&this_value, &parent, arguments);
                this_value
            })
        } else {
            Value::function(|this_value, _| this_value)
        }
    } else {
        constructor.clone()
    };
    let raw_constructor = if super_class.is_some() {
        let user_constructor = raw_constructor;
        let initializer = initializer.clone();
        let class_cell = class_cell.clone();
        Value::function(move |this_value, arguments| {
            queue_class_initializer(
                &this_value,
                brand_id,
                class_cell.borrow().clone(),
                initializer.clone(),
            );
            user_constructor.call(this_value, arguments)
        })
    } else {
        let user_constructor = raw_constructor;
        let initializer = initializer.clone();
        let class_cell = class_cell.clone();
        Value::function(move |this_value, arguments| {
            install_private_brand(&this_value, brand_id);
            if !initializer.is_undefined() {
                initializer.call(this_value.clone(), vec![class_cell.borrow().clone()]);
            }
            user_constructor.call(this_value, arguments)
        })
    };
    let call_constructor = raw_constructor.clone();
    let instance_prototype = prototype.clone();
    let class = Value::callable(HashMap::new(), move |_this, arguments| {
        let instance = Value::object(HashMap::new());
        set_prototype_of(&instance, &instance_prototype);
        let result = call_constructor.call(instance.clone(), arguments);
        if result.is_object() { result } else { instance }
    });
    prototype.set_property("constructor", class.clone());
    class.set_property("prototype", prototype);
    class.set_property("__w3cos_ctor", raw_constructor);
    if let Some(parent) = super_class {
        set_prototype_of(&class, parent);
    }
    if let Value::Object(object) = &class {
        let mut object = object.borrow_mut();
        object.class_brand = Some(brand_id);
        object.private_brands.insert(brand_id);
        object.refresh_heap_accounting();
    }
    *class_cell.borrow_mut() = class.clone();
    class
}

/// `new X(...)` — invoke the class object's call slot.
///
/// Plain `Value::Function` callees are treated as classic constructor
/// functions: a fresh object becomes `this` and is returned unless the
/// function itself returns an object. Anything else yields `Undefined`
/// (JS would throw a TypeError; the runtime stays total).
pub fn construct(class_value: &Value, args: Vec<Value>) -> Value {
    match class_value {
        Value::Object(_) => class_value.call(Value::Undefined, args),
        Value::Function(function) => {
            let instance = Value::object(HashMap::new());
            let prototype = class_value.get_property("prototype");
            if prototype.is_object() {
                set_prototype_of(&instance, &prototype);
            }
            let result = function.call(instance.clone(), args);
            if result.is_object() || result.is_array() || result.is_function() {
                result
            } else {
                instance
            }
        }
        _ => Value::Undefined,
    }
}

/// `obj instanceof X` — walk `obj`'s prototype chain looking for identity
/// with `X.prototype`.
pub fn instance_of(obj: &Value, class_value: &Value) -> bool {
    if crate::binary::typed_array_instance_of(obj, class_value) {
        return true;
    }
    let target = match class_value.get_property("prototype") {
        Value::Object(target) => target,
        _ => return false,
    };
    let mut current = match obj {
        Value::Object(object) => object.borrow().get_prototype_of(),
        _ => return false,
    };
    loop {
        match current {
            Value::Object(object) => {
                if Rc::ptr_eq(&object, &target) {
                    return true;
                }
                current = object.borrow().get_prototype_of();
            }
            _ => return false,
        }
    }
}

/// `super(...)` inside a derived constructor: run the parent class's raw
/// constructor (stored under `"__w3cos_ctor"`) on the already-allocated
/// `this`. Returns `this` so the call is usable in expression position.
pub fn super_ctor(this: &Value, parent_class: &Value, args: Vec<Value>) -> Value {
    let ctor = parent_class.get_property("__w3cos_ctor");
    ctor.call(this.clone(), args);
    run_pending_class_initializer(this);
    this.clone()
}

fn queue_class_initializer(this: &Value, brand: u64, class: Value, initializer: Value) {
    if let Value::Object(object) = this {
        let mut object = object.borrow_mut();
        object
            .pending_class_initializers
            .push((brand, class, initializer));
        object.refresh_heap_accounting();
    }
}

fn run_pending_class_initializer(this: &Value) {
    let pending = match this {
        Value::Object(object) => object.borrow_mut().pending_class_initializers.pop(),
        _ => None,
    };
    if let Some((brand, class, initializer)) = pending {
        install_private_brand(this, brand);
        if !initializer.is_undefined() {
            initializer.call(this.clone(), vec![class]);
        }
    }
}

fn install_private_brand(receiver: &Value, brand: u64) {
    if let Value::Object(object) = receiver {
        let mut object = object.borrow_mut();
        object.private_brands.insert(brand);
        object.refresh_heap_accounting();
    }
}

fn brand_id(brand: &Value) -> Option<u64> {
    match brand {
        Value::Object(object) => object.borrow().class_brand,
        _ => None,
    }
}

fn require_private_brand(receiver: &Value, brand: &Value, name: &str) -> u64 {
    let Some(brand_id) = brand_id(brand) else {
        private_brand_error(name)
    };
    let has_brand = match receiver {
        Value::Object(object) => object.borrow().private_brands.contains(&brand_id),
        _ => false,
    };
    if !has_brand {
        private_brand_error(name);
    }
    brand_id
}

fn private_brand_error(name: &str) -> ! {
    crate::throw_value(crate::error_instance(
        "TypeError",
        vec![Value::string(&format!(
            "Cannot access private member #{name} on an object whose class did not declare it"
        ))],
    ))
}

/// Installs a private field in an object's unobservable internal slot table.
pub fn define_private_field(receiver: &Value, brand: &Value, name: &str, value: Value) {
    let brand_id = require_private_brand(receiver, brand, name);
    if let Value::Object(object) = receiver {
        let mut object = object.borrow_mut();
        object
            .private_elements
            .insert((brand_id, name.to_string()), PrivateElement::Field(value));
        object.refresh_heap_accounting();
    }
}

/// Installs a shared private method on the class brand object.
pub fn define_private_method(brand: &Value, name: &str, method: Value) {
    let brand_id = require_private_brand(brand, brand, name);
    if let Value::Object(object) = brand {
        let mut object = object.borrow_mut();
        object
            .private_elements
            .insert((brand_id, name.to_string()), PrivateElement::Method(method));
        object.refresh_heap_accounting();
    }
}

/// Installs or completes a shared private accessor pair.
pub fn define_private_accessor(
    brand: &Value,
    name: &str,
    getter: Option<Value>,
    setter: Option<Value>,
) {
    let brand_id = require_private_brand(brand, brand, name);
    if let Value::Object(object) = brand {
        let mut object = object.borrow_mut();
        let key = (brand_id, name.to_string());
        let (old_getter, old_setter) = match object.private_elements.remove(&key) {
            Some(PrivateElement::Accessor { getter, setter }) => (getter, setter),
            _ => (None, None),
        };
        object.private_elements.insert(
            key,
            PrivateElement::Accessor {
                getter: getter.or(old_getter),
                setter: setter.or(old_setter),
            },
        );
        object.refresh_heap_accounting();
    }
}

fn private_element(receiver: &Value, brand: &Value, brand_id: u64, name: &str) -> PrivateElement {
    let key = (brand_id, name.to_string());
    if let Value::Object(object) = receiver {
        if let Some(element) = object.borrow().private_elements.get(&key) {
            return element.clone();
        }
    }
    if let Value::Object(object) = brand {
        if let Some(element) = object.borrow().private_elements.get(&key) {
            return element.clone();
        }
    }
    private_brand_error(name)
}

pub fn get_private(receiver: &Value, brand: &Value, name: &str) -> Value {
    let brand_id = require_private_brand(receiver, brand, name);
    match private_element(receiver, brand, brand_id, name) {
        PrivateElement::Field(value) | PrivateElement::Method(value) => value,
        PrivateElement::Accessor {
            getter: Some(getter),
            ..
        } => getter.call(receiver.clone(), Vec::new()),
        PrivateElement::Accessor { getter: None, .. } => private_brand_error(name),
    }
}

pub fn set_private(receiver: &Value, brand: &Value, name: &str, value: Value) -> Value {
    let brand_id = require_private_brand(receiver, brand, name);
    let key = (brand_id, name.to_string());
    if let Value::Object(object) = receiver {
        let is_field = matches!(
            object.borrow().private_elements.get(&key),
            Some(PrivateElement::Field(_))
        );
        if is_field {
            let mut object = object.borrow_mut();
            object
                .private_elements
                .insert(key, PrivateElement::Field(value.clone()));
            object.refresh_heap_accounting();
            return value;
        }
    }
    match private_element(receiver, brand, brand_id, name) {
        PrivateElement::Accessor {
            setter: Some(setter),
            ..
        } => {
            setter.call(receiver.clone(), vec![value.clone()]);
            value
        }
        _ => private_brand_error(name),
    }
}

pub fn has_private(receiver: &Value, brand: &Value) -> bool {
    let Some(brand_id) = brand_id(brand) else {
        return false;
    };
    match receiver {
        Value::Object(object) => object.borrow().private_brands.contains(&brand_id),
        _ => false,
    }
}

/// `super.method(...)` in an instance method: look up `name` on the parent
/// class's prototype chain and invoke it with the *current* receiver.
/// A missing method is a no-op yielding `Undefined` (Monaco has optional
/// super methods; keep total).
pub fn super_method(this: &Value, parent_class: &Value, name: &str, args: Vec<Value>) -> Value {
    let prototype = parent_class.get_property("prototype");
    let method = get_with_receiver(&prototype, this, name);
    if method.is_undefined() {
        return Value::Undefined;
    }
    method.call(this.clone(), args)
}

/// `super.prop` (read, not a call) in an instance method: read through the
/// parent prototype chain, honoring the `__w3cos_getter_` convention with the
/// current receiver.
pub fn super_get(this: &Value, parent_class: &Value, name: &str) -> Value {
    let prototype = parent_class.get_property("prototype");
    get_with_receiver(&prototype, this, name)
}

/// `super.prop = value` in an instance method: begin setter lookup at the
/// parent prototype while preserving the current instance as receiver.
pub fn super_set(this: &Value, parent_class: &Value, name: &str, value: Value) -> Value {
    let prototype = parent_class.get_property("prototype");
    set_with_receiver(&prototype, this, name, value)
}

/// `super.method(...)` inside a static member: resolve from the parent class
/// object while preserving the derived class as the JavaScript receiver.
pub fn static_super_method(
    this: &Value,
    parent_class: &Value,
    name: &str,
    args: Vec<Value>,
) -> Value {
    let method = get_with_receiver(parent_class, this, name);
    if method.is_undefined() {
        return Value::Undefined;
    }
    method.call(this.clone(), args)
}

/// `super.prop` inside a static member, including inherited static getters.
pub fn static_super_get(this: &Value, parent_class: &Value, name: &str) -> Value {
    get_with_receiver(parent_class, this, name)
}

/// `super.prop = value` inside a static member: begin setter lookup at the
/// parent class object while preserving the derived class as receiver.
pub fn static_super_set(this: &Value, parent_class: &Value, name: &str, value: Value) -> Value {
    set_with_receiver(parent_class, this, name, value)
}

fn get_with_receiver(target: &Value, receiver: &Value, name: &str) -> Value {
    match target {
        Value::Object(object) => {
            let direct = object.borrow().get(name, receiver);
            if !direct.is_undefined() {
                return direct;
            }
            let getter = object
                .borrow()
                .get(&format!("__w3cos_getter_{name}"), receiver);
            getter.call(receiver.clone(), vec![])
        }
        _ => Value::Undefined,
    }
}

fn set_with_receiver(target: &Value, receiver: &Value, name: &str, value: Value) -> Value {
    if let Value::Object(object) = target {
        let setter = object
            .borrow()
            .get(&format!("__w3cos_setter_{name}"), receiver);
        if !setter.is_undefined() {
            setter.call(receiver.clone(), vec![value.clone()]);
            return value;
        }
    }
    define_field(receiver, name, value.clone());
    value
}

/// Define an own data property directly, bypassing the setter convention.
/// Used for class field initializers and private-brand installation, which
/// in JS semantics use `[[Define]]` rather than `[[Set]]`.
pub fn define_field(this: &Value, key: &str, value: Value) {
    if let Value::Object(object) = this {
        object.borrow_mut().set_direct(key, value);
    }
}

/// Set `obj`'s prototype link from generated code (`obj` must be an object).
pub fn set_prototype_of(obj: &Value, proto: &Value) {
    if let Value::Object(object) = obj {
        object.borrow_mut().set_prototype_of(proto);
    }
}

/// `Object.getPrototypeOf(obj)`: the object's prototype link, or `Null`
/// (matching JS for non-objects, which throw — kept total here).
pub fn get_prototype_of(obj: &Value) -> Value {
    match obj {
        Value::Object(object) => object.borrow().get_prototype_of(),
        _ => Value::Null,
    }
}

/// `Object.getOwnPropertyDescriptor(obj, key)`: a descriptor object for an
/// own property, or `Undefined` when absent (matching JS for data properties).
pub fn get_own_property_descriptor(obj: &Value, key: &str) -> Value {
    match obj {
        Value::Object(object) => object.borrow().get_own_property_descriptor(key),
        _ => Value::Undefined,
    }
}

/// `Object.defineProperty(obj, key, descriptor)`: define `key` from a
/// `{value}`/`{get}`/`{set}` descriptor (best-effort), returning `obj`.
pub fn define_property(obj: &Value, key: &str, descriptor: &Value) -> Value {
    if let Value::Object(object) = obj {
        object.borrow_mut().define_property(key, descriptor);
    }
    obj.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    #[test]
    fn shared_class_builder_wires_constructor_prototype_and_inheritance() {
        let parent_ctor = Value::function(|this_value, arguments| {
            this_value.set_property(
                "value",
                arguments.first().cloned().unwrap_or(Value::Undefined),
            );
            this_value
        });
        let parent = create_class(&parent_ctor, None);
        parent.get_property("prototype").set_property(
            "read",
            Value::function(|this_value, _| this_value.get_property("value")),
        );

        let child = create_class(&Value::Undefined, Some(&parent));
        let instance = construct(&child, vec![Value::Number(7.0)]);
        assert_eq!(instance.call_method("read", Vec::new()), Value::Number(7.0));
        assert!(instance_of(&instance, &child));
        assert!(instance_of(&instance, &parent));
        assert!(
            child
                .get_property("prototype")
                .get_property("constructor")
                .strict_eq(&child)
        );
    }

    #[test]
    fn shared_initializer_scheduler_preserves_multilevel_field_order() {
        use std::cell::RefCell;

        let order = Rc::new(RefCell::new(Vec::new()));
        let a_field_order = Rc::clone(&order);
        let a_fields = Value::function(move |this_value, _| {
            a_field_order.borrow_mut().push("a-field");
            define_field(&this_value, "a", Value::Number(1.0));
            Value::Undefined
        });
        let a_ctor_order = Rc::clone(&order);
        let a_ctor = Value::function(move |this_value, _| {
            a_ctor_order.borrow_mut().push("a-ctor");
            this_value
        });
        let a = create_class_with_initializer(&a_ctor, None, &a_fields);

        let b_field_order = Rc::clone(&order);
        let b_fields = Value::function(move |this_value, _| {
            b_field_order.borrow_mut().push("b-field");
            define_field(&this_value, "b", Value::Number(2.0));
            Value::Undefined
        });
        let b_ctor_order = Rc::clone(&order);
        let a_for_b = a.clone();
        let b_ctor = Value::function(move |this_value, arguments| {
            super_ctor(&this_value, &a_for_b, arguments);
            b_ctor_order.borrow_mut().push("b-ctor");
            this_value
        });
        let b = create_class_with_initializer(&b_ctor, Some(&a), &b_fields);

        let c_field_order = Rc::clone(&order);
        let c_fields = Value::function(move |this_value, _| {
            c_field_order.borrow_mut().push("c-field");
            define_field(&this_value, "c", Value::Number(3.0));
            Value::Undefined
        });
        let c = create_class_with_initializer(&Value::Undefined, Some(&b), &c_fields);
        let instance = construct(&c, Vec::new());

        assert_eq!(
            *order.borrow(),
            vec!["a-field", "a-ctor", "b-field", "b-ctor", "c-field"]
        );
        assert_eq!(instance.get_property("a"), Value::Number(1.0));
        assert_eq!(instance.get_property("b"), Value::Number(2.0));
        assert_eq!(instance.get_property("c"), Value::Number(3.0));
    }

    #[test]
    fn static_super_access_preserves_the_derived_class_receiver() {
        let parent = create_class(&Value::Undefined, None);
        parent.set_property(
            "__w3cos_getter_label",
            Value::function(|this_value, _| this_value.get_property("marker")),
        );
        parent.set_property(
            "describe",
            Value::function(|this_value, _| {
                Value::from(format!(
                    "{}!",
                    this_value.get_property("marker").to_js_string()
                ))
            }),
        );
        let child = create_class(&Value::Undefined, Some(&parent));
        child.set_property("marker", Value::string("child"));

        assert_eq!(
            static_super_get(&child, &parent, "label"),
            Value::string("child")
        );
        assert_eq!(
            static_super_method(&child, &parent, "describe", Vec::new()),
            Value::string("child!")
        );
    }

    #[test]
    fn call_slot_makes_object_callable() {
        let class = Value::callable(HashMap::new(), |_this, args| {
            args.first().cloned().unwrap_or(Value::Undefined)
        });
        assert_eq!(
            class
                .call(Value::Undefined, vec![Value::Number(7.0)])
                .to_number(),
            7.0
        );
        // Objects without a call slot stay non-callable.
        let plain = Value::object(HashMap::new());
        assert!(plain.call(Value::Undefined, vec![]).is_undefined());
    }

    #[test]
    fn construct_runs_call_slot_and_returns_instance() {
        let proto = Value::object(HashMap::new());
        proto.set_property("tag", Value::string("pointy"));
        let proto_for_slot = proto.clone();
        let class = Value::callable(HashMap::new(), move |_this, args| {
            let instance = Value::object(HashMap::new());
            crate::class::set_prototype_of(&instance, &proto_for_slot);
            instance.set_property("x", args.first().cloned().unwrap_or(Value::Undefined));
            instance
        });
        class.set_property("prototype", proto);

        let instance = construct(&class, vec![Value::Number(3.0)]);
        assert_eq!(instance.get_property("x").to_number(), 3.0);
        // Prototype link installed by the call slot.
        assert_eq!(instance.get_property("tag").to_js_string(), "pointy");
    }

    #[test]
    fn construct_supports_plain_constructor_functions() {
        let ctor = Value::function(|this, args| {
            this.set_property("v", args.first().cloned().unwrap_or(Value::Undefined));
            Value::Undefined
        });
        let instance = construct(&ctor, vec![Value::Number(9.0)]);
        assert_eq!(instance.get_property("v").to_number(), 9.0);
    }

    #[test]
    fn construct_plain_function_installs_its_prototype() {
        let ctor = Value::function(|_, _| Value::Undefined);
        let prototype = Value::object(HashMap::new());
        prototype.set_property("render", Value::string("ready"));
        ctor.set_property("prototype", prototype);

        let instance = construct(&ctor, vec![]);
        assert_eq!(instance.get_property("render").to_js_string(), "ready");
    }

    #[test]
    fn instance_of_walks_grandparent_chain() {
        // Grandparent class.
        let gp_proto = Value::object(HashMap::new());
        let gp_proto_slot = gp_proto.clone();
        let grandparent = Value::callable(HashMap::new(), move |_this, _args| {
            let instance = Value::object(HashMap::new());
            crate::class::set_prototype_of(&instance, &gp_proto_slot);
            instance
        });
        grandparent.set_property("prototype", gp_proto.clone());

        // Parent class whose proto object links to the grandparent proto.
        let p_proto = Value::object(HashMap::new());
        crate::class::set_prototype_of(&p_proto, &gp_proto);
        let p_proto_slot = p_proto.clone();
        let parent = Value::callable(HashMap::new(), move |_this, _args| {
            let instance = Value::object(HashMap::new());
            crate::class::set_prototype_of(&instance, &p_proto_slot);
            instance
        });
        parent.set_property("prototype", p_proto);

        let instance = construct(&parent, vec![]);
        assert!(instance_of(&instance, &parent));
        assert!(instance_of(&instance, &grandparent));

        let unrelated = Value::object(HashMap::new());
        assert!(!instance_of(&unrelated, &parent));
        assert!(!instance_of(&Value::Number(1.0), &parent));
    }

    #[test]
    fn super_ctor_runs_parent_raw_ctor_on_this() {
        // Parent raw ctor installs `x` from args.
        let parent_ctor = Value::function(|this, args| {
            crate::class::define_field(
                &this,
                "x",
                args.first().cloned().unwrap_or(Value::Undefined),
            );
            this.clone()
        });
        let parent = Value::object(HashMap::new());
        parent.set_property("__w3cos_ctor", parent_ctor);

        let this = Value::object(HashMap::new());
        super_ctor(&this, &parent, vec![Value::Number(5.0)]);
        assert_eq!(this.get_property("x").to_number(), 5.0);
        // Child field init runs after super (codegen order) and wins.
        crate::class::define_field(&this, "y", Value::Number(6.0));
        assert_eq!(this.get_property("y").to_number(), 6.0);
    }

    #[test]
    fn super_method_dispatches_on_parent_proto_with_receiver() {
        let parent_proto = Value::object(HashMap::new());
        parent_proto.set_property(
            "who",
            Value::function(|this, _| {
                Value::string(&format!(
                    "parent sees {}",
                    this.get_property("mark").to_js_string()
                ))
            }),
        );
        let parent = Value::object(HashMap::new());
        parent.set_property("prototype", parent_proto);

        let this = Value::object(HashMap::new());
        this.set_property("mark", Value::string("child"));
        let result = super_method(&this, &parent, "who", vec![]);
        assert_eq!(result.to_js_string(), "parent sees child");

        // Missing super methods are a total no-op.
        assert!(super_method(&this, &parent, "missing", vec![]).is_undefined());
    }

    #[test]
    fn super_get_honors_getter_convention() {
        let parent_proto = Value::object(HashMap::new());
        parent_proto.set_property(
            "__w3cos_getter_size",
            Value::function(|this, _| this.get_property("_size").js_mul(&Value::Number(2.0))),
        );
        let parent = Value::object(HashMap::new());
        parent.set_property("prototype", parent_proto);

        let this = Value::object(HashMap::new());
        this.set_property("_size", Value::Number(21.0));
        assert_eq!(super_get(&this, &parent, "size").to_number(), 42.0);
    }

    #[test]
    fn super_set_honors_parent_setter_with_derived_receiver() {
        let parent_proto = Value::object(HashMap::new());
        parent_proto.set_property(
            "__w3cos_setter_size",
            Value::function(|this, args| {
                define_field(
                    &this,
                    "_size",
                    args.first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .js_mul(&Value::Number(2.0)),
                );
                Value::Undefined
            }),
        );
        let parent = Value::object(HashMap::new());
        parent.set_property("prototype", parent_proto);

        let this = Value::object(HashMap::new());
        assert_eq!(
            super_set(&this, &parent, "size", Value::Number(21.0)).to_number(),
            21.0
        );
        assert_eq!(this.get_property("_size").to_number(), 42.0);
        assert!(this.get_property("size").is_undefined());
    }

    #[test]
    fn setter_convention_routes_through_setter_and_getter_reads_back() {
        let proto = Value::object(HashMap::new());
        proto.set_property(
            "__w3cos_setter_value",
            Value::function(|this, args| {
                crate::class::define_field(
                    &this,
                    "_value",
                    args.first().cloned().unwrap_or(Value::Undefined),
                );
                Value::Undefined
            }),
        );
        proto.set_property(
            "__w3cos_getter_value",
            Value::function(|this, _| this.get_property("_value")),
        );

        let obj = Value::object(HashMap::new());
        crate::class::set_prototype_of(&obj, &proto);
        obj.set_property("value", Value::Number(11.0));
        assert_eq!(obj.get_property("value").to_number(), 11.0);
        // The backing field was defined, not a shadowing `value` property.
        assert_eq!(obj.get_property("_value").to_number(), 11.0);
        assert!(!obj.to_js_string().contains("value")); // sanity: no display leak
    }

    #[test]
    fn define_field_bypasses_setter() {
        let proto = Value::object(HashMap::new());
        proto.set_property(
            "__w3cos_setter_x",
            Value::function(|_this, _| Value::Undefined),
        );
        let obj = Value::object(HashMap::new());
        crate::class::set_prototype_of(&obj, &proto);
        crate::class::define_field(&obj, "x", Value::Number(4.0));
        // Own data property now shadows the setter for later plain sets.
        obj.set_property("x", Value::Number(5.0));
        assert_eq!(obj.get_property("x").to_number(), 5.0);
    }

    #[test]
    fn private_slots_are_branded_inherited_and_invisible_to_reflection() {
        let base_fields = Value::function(|this, arguments| {
            define_private_field(&this, &arguments[0], "base", Value::Number(1.0));
            Value::Undefined
        });
        let base = create_class_with_initializer(&Value::Undefined, None, &base_fields);
        let child_fields = Value::function(|this, arguments| {
            define_private_field(&this, &arguments[0], "child", Value::Number(2.0));
            Value::Undefined
        });
        let child = create_class_with_initializer(&Value::Undefined, Some(&base), &child_fields);
        let instance = construct(&child, Vec::new());

        assert_eq!(get_private(&instance, &base, "base"), Value::Number(1.0));
        assert_eq!(get_private(&instance, &child, "child"), Value::Number(2.0));
        assert!(has_private(&instance, &base));
        assert!(has_private(&instance, &child));
        let Value::Object(object) = &instance else {
            panic!("constructed value should be an object");
        };
        assert_eq!(object.borrow().own_keys().to_js_string(), "");
    }

    #[test]
    fn private_access_rejects_an_object_with_the_wrong_brand() {
        let fields = Value::function(|this, arguments| {
            define_private_field(&this, &arguments[0], "secret", Value::Number(1.0));
            Value::Undefined
        });
        let owner = create_class_with_initializer(&Value::Undefined, None, &fields);
        let foreign = construct(&create_class(&Value::Undefined, None), Vec::new());

        let thrown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            get_private(&foreign, &owner, "secret")
        }));
        assert!(thrown.is_err());
        assert!(!has_private(&foreign, &owner));
    }

    /// Semantics proof for the esm_codegen class-factory pattern.
    ///
    /// Hand-writes the exact Rust shape `esm_codegen::emit_class` produces for:
    /// ```js
    /// class A {
    ///   constructor(x) { this.x = x; }
    ///   get double() { return this.x * 2; }
    ///   static make() { return new A(21); }
    /// }
    /// class B extends A {
    ///   constructor(x, y) { super(x); this.y = y; }
    ///   sum() { return this.x + this.y; }
    /// }
    /// ```
    /// and asserts the runtime behavior end to end.
    #[allow(non_snake_case)] // names mirror the esm_codegen emission scheme
    mod codegen_pattern {
        use crate::Value;
        use std::cell::RefCell;
        use std::collections::HashMap;

        // ── class A ────────────────────────────────────────────────────

        fn a__ctor(__this: Value, __args: Vec<Value>) -> Value {
            #[allow(unused_mut)]
            let mut x = __args.first().cloned().unwrap_or(Value::Undefined);
            {
                let value = x.clone();
                __this.set_property("x", value.clone());
                let _ = value;
            }
            __this
        }

        fn a__get_double(__this: Value, _args: Vec<Value>) -> Value {
            __this.get_property("x").js_mul(&Value::Number(2.0))
        }

        fn a__static_make(_this: Value, _args: Vec<Value>) -> Value {
            crate::class::construct(&a(), vec![Value::Number(21.0)])
        }

        fn a__build_class() -> Value {
            let __proto = Value::object(HashMap::new());
            __proto.set_property("__w3cos_getter_double", Value::function(a__get_double));
            let __ctor_proto = __proto.clone();
            let __class = Value::callable(HashMap::new(), move |_this, __args| {
                let __instance = Value::object(HashMap::new());
                crate::class::set_prototype_of(&__instance, &__ctor_proto);
                let __ret = a__ctor(__instance.clone(), __args);
                if __ret.is_object() { __ret } else { __instance }
            });
            __proto.set_property("constructor", __class.clone());
            __class.set_property("prototype", __proto);
            __class.set_property("__w3cos_ctor", Value::function(a__ctor));
            __class.set_property("make", Value::function(a__static_make));
            __class
        }

        thread_local! {
            static A_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
        }

        fn a() -> Value {
            A_CLASS.with(|cell| {
                if let Some(value) = cell.borrow().as_ref() {
                    return value.clone();
                }
                let value = a__build_class();
                *cell.borrow_mut() = Some(value.clone());
                value
            })
        }

        // ── class B extends A ──────────────────────────────────────────

        fn b__ctor(__this: Value, __args: Vec<Value>) -> Value {
            #[allow(unused_mut)]
            let mut x = __args.first().cloned().unwrap_or(Value::Undefined);
            #[allow(unused_mut)]
            let mut y = __args.get(1).cloned().unwrap_or(Value::Undefined);
            // super(x)
            let _ = crate::class::super_ctor(&__this, &a(), vec![x.clone()]);
            // this.y = y
            {
                let value = y.clone();
                __this.set_property("y", value.clone());
                let _ = value;
            }
            __this
        }

        fn b__sum(__this: Value, _args: Vec<Value>) -> Value {
            __this.get_property("x").js_add(&__this.get_property("y"))
        }

        fn b__build_class() -> Value {
            let __parent = a();
            let __proto = Value::object(HashMap::new());
            __proto.set_property("sum", Value::function(b__sum));
            crate::class::set_prototype_of(&__proto, &__parent.get_property("prototype"));
            let __ctor_proto = __proto.clone();
            let __class = Value::callable(HashMap::new(), move |_this, __args| {
                let __instance = Value::object(HashMap::new());
                crate::class::set_prototype_of(&__instance, &__ctor_proto);
                let __ret = b__ctor(__instance.clone(), __args);
                if __ret.is_object() { __ret } else { __instance }
            });
            __proto.set_property("constructor", __class.clone());
            __class.set_property("prototype", __proto);
            __class.set_property("__w3cos_ctor", Value::function(b__ctor));
            crate::class::set_prototype_of(&__class, &__parent);
            __class
        }

        thread_local! {
            static B_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
        }

        fn b() -> Value {
            B_CLASS.with(|cell| {
                if let Some(value) = cell.borrow().as_ref() {
                    return value.clone();
                }
                let value = b__build_class();
                *cell.borrow_mut() = Some(value.clone());
                value
            })
        }

        #[test]
        fn generated_class_pattern_behaves_like_js() {
            // const obj = new B(3, 4)
            let obj = crate::class::construct(&b(), vec![Value::Number(3.0), Value::Number(4.0)]);

            // super(x) ran the parent ctor: this.x === 3
            assert_eq!(obj.get_property("x").to_number(), 3.0);
            assert_eq!(obj.get_property("y").to_number(), 4.0);

            // Dynamic dispatch: obj.sum() === 7
            assert_eq!(obj.call_method("sum", vec![]).to_number(), 7.0);

            // Inherited getter through the prototype chain: obj.double === 6
            assert_eq!(obj.get_property("double").to_number(), 6.0);

            // instanceof across the chain
            assert!(crate::class::instance_of(&obj, &b()));
            assert!(crate::class::instance_of(&obj, &a()));
            assert!(!crate::class::instance_of(
                &Value::object(HashMap::new()),
                &b()
            ));

            // static make(): A.make() constructs an A with x = 21
            let made = a().call_method("make", vec![]);
            assert_eq!(made.get_property("x").to_number(), 21.0);
            assert!(crate::class::instance_of(&made, &a()));
            assert!(!crate::class::instance_of(&made, &b()));

            // Static inheritance: B.make is reachable through B's class object
            // (its prototype is A's class object).
            let made_by_b = b().call_method("make", vec![]);
            assert_eq!(made_by_b.get_property("x").to_number(), 21.0);

            // `constructor` back-reference and `prototype` wiring.
            let ctor = obj.get_property("constructor");
            assert!(crate::class::instance_of(&obj, &ctor));
        }
    }
}
