//! Runtime support for JavaScript classes in the ESM compile pipeline.
//!
//! A JS class is a *callable object*: a `Value::Object` whose `JsObject` has a
//! call slot (see `Value::callable`). The generated code stores the raw
//! constructor under the `"__w3cos_ctor"` key and the prototype object under
//! `"prototype"`. These helpers implement construction, `instanceof`, and
//! `super` semantics on top of that representation.

use std::collections::HashMap;
use std::rc::Rc;

use crate::Value;

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
            if result.is_object() || result.is_array() {
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
    this.clone()
}

/// `super.method(...)` in an instance method: look up `name` on the parent
/// class's prototype chain and invoke it with the *current* receiver.
/// A missing method is a no-op yielding `Undefined` (Monaco has optional
/// super methods; keep total).
pub fn super_method(this: &Value, parent_class: &Value, name: &str, args: Vec<Value>) -> Value {
    let prototype = parent_class.get_property("prototype");
    let method = prototype.get_property(name);
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
    match &prototype {
        Value::Object(object) => {
            let direct = object.borrow().get(name, this);
            if !direct.is_undefined() {
                return direct;
            }
            let getter = object.borrow().get(&format!("__w3cos_getter_{name}"), this);
            getter.call(this.clone(), vec![])
        }
        _ => Value::Undefined,
    }
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
