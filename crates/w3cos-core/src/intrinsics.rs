//! Backend-neutral JavaScript semantic operations.
//!
//! Generated AOT code and the dynamic executor call this module instead of
//! implementing coercion, object, call, or construction behavior themselves.

use crate::Value;
use std::collections::{HashMap, HashSet};

pub fn reference_error(message: &str) -> Value {
    crate::error_instance("ReferenceError", vec![Value::string(message)])
}

pub fn type_error(message: &str) -> Value {
    crate::error_instance("TypeError", vec![Value::string(message)])
}

pub fn add(lhs: &Value, rhs: &Value) -> Value {
    lhs.js_add(rhs)
}

pub fn subtract(lhs: &Value, rhs: &Value) -> Value {
    lhs.js_sub(rhs)
}

pub fn multiply(lhs: &Value, rhs: &Value) -> Value {
    lhs.js_mul(rhs)
}

pub fn divide(lhs: &Value, rhs: &Value) -> Value {
    lhs.js_div(rhs)
}

pub fn remainder(lhs: &Value, rhs: &Value) -> Value {
    lhs.js_rem(rhs)
}

pub fn exponentiate(lhs: &Value, rhs: &Value) -> Value {
    lhs.js_pow(rhs)
}

pub fn abstract_equal(lhs: &Value, rhs: &Value) -> Value {
    Value::Bool(lhs.abstract_eq(rhs))
}

pub fn strict_equal(lhs: &Value, rhs: &Value) -> Value {
    Value::Bool(lhs.strict_eq(rhs))
}

pub fn logical_not(value: &Value) -> Value {
    value.js_not()
}

pub fn less_than(lhs: &Value, rhs: &Value) -> Value {
    Value::Bool(lhs.js_lt(rhs))
}

pub fn less_than_or_equal(lhs: &Value, rhs: &Value) -> Value {
    Value::Bool(lhs.js_le(rhs))
}

pub fn greater_than(lhs: &Value, rhs: &Value) -> Value {
    Value::Bool(lhs.js_gt(rhs))
}

pub fn greater_than_or_equal(lhs: &Value, rhs: &Value) -> Value {
    Value::Bool(lhs.js_ge(rhs))
}

pub fn type_of(value: &Value) -> Value {
    Value::string(value.type_of())
}

fn to_uint32(value: &Value) -> u32 {
    let number = value.to_number();
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    number.trunc().rem_euclid(4_294_967_296.0) as u32
}

fn to_int32(value: &Value) -> i32 {
    to_uint32(value) as i32
}

pub fn bitwise_and(lhs: &Value, rhs: &Value) -> Value {
    Value::Number((to_int32(lhs) & to_int32(rhs)) as f64)
}

pub fn bitwise_or(lhs: &Value, rhs: &Value) -> Value {
    Value::Number((to_int32(lhs) | to_int32(rhs)) as f64)
}

pub fn bitwise_xor(lhs: &Value, rhs: &Value) -> Value {
    Value::Number((to_int32(lhs) ^ to_int32(rhs)) as f64)
}

pub fn left_shift(lhs: &Value, rhs: &Value) -> Value {
    let shift = to_uint32(rhs) & 0x1f;
    Value::Number(to_int32(lhs).wrapping_shl(shift) as f64)
}

pub fn signed_right_shift(lhs: &Value, rhs: &Value) -> Value {
    let shift = to_uint32(rhs) & 0x1f;
    Value::Number((to_int32(lhs) >> shift) as f64)
}

pub fn unsigned_right_shift(lhs: &Value, rhs: &Value) -> Value {
    let shift = to_uint32(rhs) & 0x1f;
    Value::Number((to_uint32(lhs) >> shift) as f64)
}

pub fn bitwise_not(value: &Value) -> Value {
    value.js_bitnot()
}

pub fn negate(value: &Value) -> Value {
    value.js_neg()
}

pub fn get_property(object: &Value, key: &Value) -> Value {
    object.get_property(&key.to_js_string())
}

pub fn get_property_checked(object: &Value, key: &Value) -> Value {
    object.get_property_checked(&key.to_js_string())
}

pub fn delete_property(object: &Value, key: &Value) -> Value {
    object.delete_property(&key.to_js_string())
}

pub fn set_property(object: &Value, key: &Value, value: Value) -> Value {
    object.set_property(&key.to_js_string(), value.clone());
    value
}

pub fn in_operator(key: &Value, object: &Value) -> Value {
    key.js_in(object)
}

pub fn instance_of(value: &Value, constructor: &Value) -> Value {
    Value::Bool(crate::class::instance_of(value, constructor))
}

pub fn define_field(object: &Value, key: &Value, value: Value) -> Value {
    crate::class::define_field(object, &key.to_js_string(), value.clone());
    value
}

pub fn define_private(object: &Value, brand: &Value, name: &Value, value: Value) -> Value {
    crate::class::define_private_field(object, brand, &name.to_js_string(), value.clone());
    value
}

pub fn get_private(object: &Value, brand: &Value, name: &Value) -> Value {
    crate::class::get_private(object, brand, &name.to_js_string())
}

pub fn set_private(object: &Value, brand: &Value, name: &Value, value: Value) -> Value {
    crate::class::set_private(object, brand, &name.to_js_string(), value)
}

pub fn has_private(object: &Value, brand: &Value) -> Value {
    Value::Bool(crate::class::has_private(object, brand))
}

pub fn define_private_method(brand: &Value, name: &Value, method: Value) -> Value {
    crate::class::define_private_method(brand, &name.to_js_string(), method.clone());
    method
}

pub fn define_private_accessor(
    brand: &Value,
    name: &Value,
    getter: Option<Value>,
    setter: Option<Value>,
) -> Value {
    crate::class::define_private_accessor(brand, &name.to_js_string(), getter, setter);
    brand.clone()
}

pub fn create_object(properties: Vec<(Value, Value)>) -> Value {
    let object = Value::object(HashMap::new());
    crate::class::set_prototype_of(
        &object,
        &crate::builtins::object_value().get_property("prototype"),
    );
    for (key, value) in properties {
        set_property(&object, &key, value);
    }
    object
}

/// ECMAScript CopyDataProperties used by object literals and JSX props.
///
/// `null` and `undefined` contribute no properties. Other supported source
/// values expose their own enumerable string keys, and each value is read
/// before being defined directly on the target so getters/proxy traps run
/// without invoking inherited setters on the destination.
pub fn copy_data_properties(target: &Value, source: &Value) -> Value {
    let keys = match source {
        Value::Undefined | Value::Null => Vec::new(),
        Value::Object(object) => {
            let own_keys = object.borrow().own_keys();
            let Value::Array(keys) = own_keys else {
                return target.clone();
            };
            keys.borrow()
                .iter()
                .map(Value::to_js_string)
                .collect::<Vec<_>>()
        }
        Value::Array(values) => values
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, value)| !crate::value::is_array_hole(value))
            .map(|(index, _)| index.to_string())
            .collect(),
        Value::String(value) => (0..value.encode_utf16().count())
            .map(|index| index.to_string())
            .collect(),
        Value::Function(function) => function
            .keys()
            .into_iter()
            .filter(|key| key != "prototype")
            .collect(),
        Value::Bool(_) | Value::Number(_) => Vec::new(),
    };

    let mut seen = HashSet::new();
    for raw_key in keys {
        let key = raw_key
            .strip_prefix("__w3cos_getter_")
            .or_else(|| raw_key.strip_prefix("__w3cos_setter_"))
            .unwrap_or(&raw_key)
            .to_string();
        if !seen.insert(key.clone()) {
            continue;
        }
        if let Value::Object(object) = source {
            let descriptor = object.borrow().get_own_property_descriptor(&key);
            if descriptor.is_undefined() || !descriptor.get_property("enumerable").to_bool() {
                continue;
            }
        }
        let value = source.get_property(&key);
        define_field(target, &Value::string(&key), value);
    }
    target.clone()
}

pub fn create_array(elements: Vec<Value>) -> Value {
    Value::array(elements)
}

pub fn append_array_element(array: &Value, value: Value) -> Value {
    let Value::Array(elements) = array else {
        crate::throw_value(Value::string(
            "TypeError: array construction target is not an array",
        ));
    };
    let mut elements = elements.borrow_mut();
    elements.push(value);
    array.clone()
}

pub fn append_iterable(array: &Value, iterable: &Value) -> Value {
    if !iterable.is_iterable() {
        crate::throw_value(crate::value::type_error(
            "array spread value is not iterable",
        ));
    }
    for value in iterable.iter() {
        append_array_element(array, value);
    }
    array.clone()
}

pub fn array_rest(value: &Value, start: usize) -> Value {
    value.call_method("slice", vec![Value::Number(start as f64)])
}

pub fn object_rest(value: &Value, excluded: &[Value]) -> Value {
    let excluded = excluded.iter().map(Value::to_js_string).collect::<Vec<_>>();
    value.object_rest(&excluded.iter().map(String::as_str).collect::<Vec<_>>())
}

pub fn for_in_keys(value: &Value) -> Value {
    if let Value::Array(values) = value {
        return Value::array(
            values
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, value)| !crate::value::is_array_hole(value))
                .map(|(index, _)| Value::from(index.to_string()))
                .collect(),
        );
    }
    let mut keys = Vec::new();
    let mut seen_keys = HashSet::new();
    let mut seen_objects = HashSet::new();
    let mut current = value.clone();
    while let Value::Object(object) = current {
        if !seen_objects.insert(std::rc::Rc::as_ptr(&object) as usize) {
            break;
        }
        let (own_keys, prototype) = {
            let object = object.borrow();
            (
                object.own_keys().iter().collect::<Vec<_>>(),
                object.get_prototype_of(),
            )
        };
        for key in own_keys {
            let key = key.to_js_string();
            let enumerable = {
                let object = object.borrow();
                let descriptor = object.get_own_property_descriptor(&key);
                !descriptor.is_undefined() && descriptor.get_property("enumerable").to_bool()
            };
            if !enumerable {
                continue;
            }
            if seen_keys.insert(key.clone()) {
                keys.push(Value::from(key));
            }
        }
        current = prototype;
    }
    Value::array(keys)
}

pub fn call(callee: &Value, this_value: Value, arguments: Vec<Value>) -> Value {
    callee.call(this_value, arguments)
}

fn materialized_arguments(arguments: &Value) -> Vec<Value> {
    let Value::Array(arguments) = arguments else {
        crate::throw_value(crate::value::type_error(
            "materialized call arguments are not an array",
        ));
    };
    arguments
        .borrow()
        .iter()
        .cloned()
        .map(crate::value::array_slot_value)
        .collect()
}

pub fn call_with_arguments(callee: &Value, this_value: Value, arguments: &Value) -> Value {
    call(callee, this_value, materialized_arguments(arguments))
}

pub fn call_method(object: &Value, key: &Value, arguments: Vec<Value>) -> Value {
    object.call_method(&key.to_js_string(), arguments)
}

pub fn call_method_with_arguments(object: &Value, key: &Value, arguments: &Value) -> Value {
    call_method(object, key, materialized_arguments(arguments))
}

pub fn construct(constructor: &Value, arguments: Vec<Value>) -> Value {
    crate::class::construct(constructor, arguments)
}

pub fn construct_with_arguments(constructor: &Value, arguments: &Value) -> Value {
    construct(constructor, materialized_arguments(arguments))
}

pub fn super_construct(this_value: &Value, super_class: &Value, arguments: Vec<Value>) -> Value {
    crate::class::super_ctor(this_value, super_class, arguments)
}

pub fn super_get(this_value: &Value, super_class: &Value, key: &Value) -> Value {
    crate::class::super_get(this_value, super_class, &key.to_js_string())
}

pub fn super_set(this_value: &Value, super_class: &Value, key: &Value, value: Value) -> Value {
    crate::class::super_set(this_value, super_class, &key.to_js_string(), value)
}

pub fn super_call(
    this_value: &Value,
    super_class: &Value,
    key: &Value,
    arguments: Vec<Value>,
) -> Value {
    crate::class::super_method(this_value, super_class, &key.to_js_string(), arguments)
}

pub fn create_class(constructor: &Value, super_class: Option<&Value>) -> Value {
    crate::class::create_class(constructor, super_class)
}

pub fn create_class_with_initializer(
    constructor: &Value,
    super_class: Option<&Value>,
    initializer: &Value,
) -> Value {
    crate::class::create_class_with_initializer(constructor, super_class, initializer)
}

pub fn promise_new(arguments: Vec<Value>) -> Value {
    crate::promise::new(arguments)
}

pub fn promise_resolve(arguments: Vec<Value>) -> Value {
    crate::promise::resolve(arguments)
}

pub fn promise_reject(arguments: Vec<Value>) -> Value {
    crate::promise::reject(arguments)
}

pub fn promise_all(arguments: Vec<Value>) -> Value {
    crate::promise::all(arguments)
}

pub fn promise_race(arguments: Vec<Value>) -> Value {
    crate::promise::race(arguments)
}

/// ECMAScript `Await(value)` begins by applying the current realm's
/// PromiseResolve semantics. Both native AOT and W3VM call this entry point
/// before subscribing their resumable frames.
pub fn await_value(value: &Value) -> Value {
    promise_resolve(vec![value.clone()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn addition_uses_ecmascript_coercion() {
        assert_eq!(
            add(&Value::string("count:"), &Value::Number(3.0)),
            Value::string("count:3")
        );
        assert_eq!(
            add(&Value::Number(2.0), &Value::Number(3.0)),
            Value::Number(5.0)
        );
    }

    #[test]
    fn execution_errors_use_standard_error_values() {
        let reference = reference_error("binding is not initialized");
        assert_eq!(
            reference.get_property("name"),
            Value::string("ReferenceError")
        );
        assert_eq!(
            reference.get_property("message"),
            Value::string("binding is not initialized")
        );
        assert_eq!(
            reference.to_js_string(),
            "ReferenceError: binding is not initialized"
        );

        let type_error = type_error("binding is immutable");
        assert_eq!(type_error.get_property("name"), Value::string("TypeError"));
        assert_eq!(
            type_error.get_property("message"),
            Value::string("binding is immutable")
        );
    }

    #[test]
    fn property_operations_use_dynamic_keys_and_return_assignment_value() {
        let object = Value::object(HashMap::new());
        let assigned = set_property(&object, &Value::string("answer"), Value::Number(42.0));
        assert_eq!(assigned, Value::Number(42.0));
        assert_eq!(
            get_property(&object, &Value::string("answer")),
            Value::Number(42.0)
        );
    }

    #[test]
    fn call_preserves_explicit_this_value() {
        let function = Value::function(|this, _| this.get_property("value"));
        let receiver = Value::object(HashMap::from([("value".into(), Value::string("receiver"))]));
        assert_eq!(
            call(&function, receiver, Vec::new()),
            Value::string("receiver")
        );
    }

    #[test]
    fn method_calls_use_shared_builtin_and_receiver_semantics() {
        let array = create_array(vec![Value::Number(1.0)]);
        assert_eq!(
            call_method(&array, &Value::string("push"), vec![Value::Number(2.0)]),
            Value::Number(2.0)
        );
        assert_eq!(array.get_property("1"), Value::Number(2.0));

        let receiver = Value::object(HashMap::from([("value".into(), Value::Number(7.0))]));
        receiver.set_property(
            "read",
            Value::function(|this, _| this.get_property("value")),
        );
        assert_eq!(
            call_method(&receiver, &Value::string("read"), Vec::new()),
            Value::Number(7.0)
        );
    }

    #[test]
    fn super_and_await_operations_stay_behind_the_shared_abi() {
        let constructor = Value::function(|this, arguments| {
            set_property(
                &this,
                &Value::string("name"),
                arguments.first().cloned().unwrap_or(Value::Undefined),
            );
            Value::Undefined
        });
        let parent = create_class(&constructor, None);
        let prototype = get_property(&parent, &Value::string("prototype"));
        set_property(
            &prototype,
            &Value::string("describe"),
            Value::function(|this, _| get_property(&this, &Value::string("name"))),
        );
        let receiver = Value::object(HashMap::new());

        super_construct(&receiver, &parent, vec![Value::string("shared")]);
        assert_eq!(
            super_call(&receiver, &parent, &Value::string("describe"), Vec::new()),
            Value::string("shared")
        );

        let awaited = await_value(&Value::Number(42.0));
        assert!(matches!(
            crate::promise::status(&awaited),
            Some(crate::promise::PromiseStatus::Fulfilled(Value::Number(
                42.0
            )))
        ));
    }

    #[test]
    fn aggregate_creation_preserves_order_and_shared_value_types() {
        let object = create_object(vec![
            (Value::string("answer"), Value::Number(1.0)),
            (Value::string("answer"), Value::Number(42.0)),
            (
                Value::string("items"),
                create_array(vec![Value::string("a"), Value::string("b")]),
            ),
        ]);
        assert_eq!(object.get_property("answer"), Value::Number(42.0));
        assert_eq!(
            object.get_property("items").get_property("1"),
            Value::string("b")
        );
        assert!(
            crate::class::get_prototype_of(&object)
                .strict_eq(&crate::builtins::object_value().get_property("prototype")),
            "object literals must inherit from the shared Object.prototype",
        );
    }

    #[test]
    fn destructuring_rest_uses_shared_array_and_object_copy_semantics() {
        let array = create_array(vec![
            Value::string("head"),
            Value::string("middle"),
            Value::string("tail"),
        ]);
        let tail = array_rest(&array, 1);
        assert_eq!(tail.get_property("length"), Value::Number(2.0));
        assert_eq!(tail.get_property("0"), Value::string("middle"));

        let object = create_object(vec![
            (Value::string("picked"), Value::Number(1.0)),
            (Value::string("kept"), Value::Number(2.0)),
        ]);
        let rest = object_rest(&object, &[Value::string("picked")]);
        assert!(rest.get_property("picked").is_undefined());
        assert_eq!(rest.get_property("kept"), Value::Number(2.0));
    }

    #[test]
    fn copy_data_properties_overwrites_in_order_and_skips_nullish_sources() {
        let target = create_object(vec![(Value::string("kept"), Value::string("before"))]);
        let source = create_object(vec![
            (Value::string("kept"), Value::string("after")),
            (Value::string("added"), Value::Number(2.0)),
        ]);

        copy_data_properties(&target, &Value::Null);
        copy_data_properties(&target, &source);

        assert_eq!(target.get_property("kept"), Value::string("after"));
        assert_eq!(target.get_property("added"), Value::Number(2.0));
    }

    #[test]
    fn incremental_array_construction_appends_iterables_in_order() {
        let target = create_array(vec![Value::string("first")]);
        append_iterable(
            &target,
            &create_array(vec![Value::string("second"), Value::string("third")]),
        );
        append_array_element(&target, Value::string("last"));

        assert_eq!(target.get_property("length"), Value::Number(4.0));
        assert_eq!(target.get_property("0"), Value::string("first"));
        assert_eq!(target.get_property("1"), Value::string("second"));
        assert_eq!(target.get_property("2"), Value::string("third"));
        assert_eq!(target.get_property("3"), Value::string("last"));
    }

    #[test]
    fn arithmetic_and_comparison_keep_ecmascript_coercion_in_core() {
        assert_eq!(
            subtract(&Value::string("5"), &Value::Number(2.0)),
            Value::Number(3.0)
        );
        assert_eq!(
            multiply(&Value::string("3"), &Value::Number(2.0)),
            Value::Number(6.0)
        );
        assert_eq!(
            abstract_equal(&Value::string("1"), &Value::Number(1.0)),
            Value::Bool(true)
        );
        assert_eq!(
            strict_equal(&Value::string("1"), &Value::Number(1.0)),
            Value::Bool(false)
        );
        assert_eq!(
            logical_not(&strict_equal(&Value::string("1"), &Value::Number(1.0))),
            Value::Bool(true)
        );
        assert_eq!(
            less_than(&Value::string("2"), &Value::Number(10.0)),
            Value::Bool(true)
        );
    }

    #[test]
    fn typeof_uses_the_shared_value_model() {
        assert_eq!(type_of(&Value::Undefined), Value::string("undefined"));
        assert_eq!(type_of(&Value::Null), Value::string("object"));
        assert_eq!(
            type_of(&Value::function(|_, _| Value::Undefined)),
            Value::string("function")
        );
    }

    #[test]
    fn bitwise_and_shift_operations_use_ecmascript_int32_coercion() {
        assert_eq!(
            bitwise_and(&Value::string("7"), &Value::Number(3.0)),
            Value::Number(3.0)
        );
        assert_eq!(
            bitwise_or(&Value::Number(4.0), &Value::Number(1.0)),
            Value::Number(5.0)
        );
        assert_eq!(
            bitwise_xor(&Value::Number(7.0), &Value::Number(3.0)),
            Value::Number(4.0)
        );
        assert_eq!(
            left_shift(&Value::Number(1.0), &Value::Number(33.0)),
            Value::Number(2.0)
        );
        assert_eq!(
            signed_right_shift(&Value::Number(-8.0), &Value::Number(2.0)),
            Value::Number(-2.0)
        );
        assert_eq!(
            unsigned_right_shift(&Value::Number(-1.0), &Value::Number(1.0)),
            Value::Number(2_147_483_647.0)
        );
        assert_eq!(bitwise_not(&Value::Number(0.0)), Value::Number(-1.0));
    }

    #[test]
    fn for_in_keys_and_internal_method_share_the_prototype_snapshot() {
        let prototype = Value::object(HashMap::from([
            ("shadowed".into(), Value::Number(0.0)),
            ("inherited".into(), Value::Number(3.0)),
        ]));
        let object = Value::object(HashMap::from([
            ("first".into(), Value::Number(1.0)),
            ("second".into(), Value::Number(2.0)),
            ("shadowed".into(), Value::Number(4.0)),
        ]));
        crate::class::set_prototype_of(&object, &prototype);
        let mut direct = for_in_keys(&object)
            .iter()
            .map(|value| value.to_js_string())
            .collect::<Vec<_>>();
        let mut bridged = object
            .call_method("__w3cos_for_in_keys", Vec::new())
            .iter()
            .map(|value| value.to_js_string())
            .collect::<Vec<_>>();
        direct.sort();
        bridged.sort();
        assert_eq!(direct, vec!["first", "inherited", "second", "shadowed"]);
        assert_eq!(bridged, direct);
    }

    #[test]
    fn for_in_keys_stops_on_a_cyclic_prototype_chain() {
        let first = Value::object(HashMap::from([("first".into(), Value::Bool(true))]));
        let second = Value::object(HashMap::from([("second".into(), Value::Bool(true))]));
        crate::class::set_prototype_of(&first, &second);
        crate::class::set_prototype_of(&second, &first);
        let mut keys = for_in_keys(&first)
            .iter()
            .map(|value| value.to_js_string())
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, vec!["first", "second"]);
    }

    #[test]
    fn for_in_keys_skip_non_enumerable_object_prototype_methods() {
        let object = create_object(vec![(Value::string("only"), Value::Number(1.0))]);
        let keys = for_in_keys(&object)
            .iter()
            .map(|value| value.to_js_string())
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["only"]);
    }
}
