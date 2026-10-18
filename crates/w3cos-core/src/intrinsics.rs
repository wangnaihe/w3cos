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
    if let Some(index) = canonical_number_index(key)
        && let Some(value) = crate::fast_collection_get(object, Some(index))
    {
        return value;
    }
    object.get_property(&key.to_js_string())
}

fn canonical_number_index(key: &Value) -> Option<usize> {
    let n = key.as_number()?;
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n >= u32::MAX as f64 {
        return None;
    }
    Some(n as usize)
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

pub fn set_property_strict(object: &Value, key: &Value, value: Value) -> Value {
    let key = key.to_js_string();
    if !object.try_set_property(&key, value.clone()) {
        crate::throw_value(type_error(&format!(
            "Cannot assign to read only property '{key}'"
        )));
    }
    value
}

/// Strict-mode `obj.key = value` for a constant `key` through a static
/// inline cache (see [`crate::ic`]).
pub fn set_property_strict_ic(
    object: &Value,
    key: &str,
    value: Value,
    ic: &crate::ic::PropIc,
) -> Value {
    if !object.try_set_property_ic(key, value.clone(), ic) {
        crate::throw_value(type_error(&format!(
            "Cannot assign to read only property '{key}'"
        )));
    }
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
        _ if source.is_nullish() => Vec::new(),
        _ if source.as_object().is_some() => {
            let object = source.as_object().expect("object");
            let own_keys = object.borrow().own_keys();
            let Some(keys) = own_keys.as_array() else {
                return target.clone();
            };
            keys.borrow()
                .iter()
                .map(Value::to_js_string)
                .collect::<Vec<_>>()
        }
        _ if source.as_array().is_some() => source
            .as_array()
            .expect("array")
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, value)| !crate::value::is_array_hole(value))
            .map(|(index, _)| index.to_string())
            .collect(),
        _ if source.is_string() => {
            let value = source.as_js_string().expect("string");
            (0..value.encode_utf16().count())
                .map(|index| index.to_string())
                .collect()
        }
        _ if source.as_function().is_some() => source
            .as_function()
            .expect("function")
            .keys()
            .into_iter()
            .filter(|key| key != "prototype")
            .collect(),
        _ => Vec::new(),
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
        if let Some(object) = source.as_object() {
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
    let Some(elements) = array.as_array() else {
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
    if let Some(values) = value.as_array() {
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
    while let Some(object) = current.as_object() {
        if !seen_objects.insert(object.identity()) {
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
                if object.is_proxy() {
                    let descriptor = object.get_own_property_descriptor(&key);
                    !descriptor.is_undefined() && descriptor.get_property("enumerable").to_bool()
                } else {
                    object.own_property_enumerability(&key).unwrap_or(false)
                }
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

/// Invoke a callable and surface host/TDZ [`throw_value`] panics as a completion.
pub fn call_completion(
    callee: &Value,
    this_value: Value,
    arguments: Vec<Value>,
) -> crate::Completion {
    crate::catch_js(|| call(callee, this_value, arguments))
}

fn try_materialized_arguments(arguments: &Value) -> Result<Vec<Value>, Value> {
    let Some(arguments) = arguments.as_array() else {
        return Err(crate::value::type_error(
            "materialized call arguments are not an array",
        ));
    };
    Ok(arguments
        .borrow()
        .iter()
        .cloned()
        .map(crate::value::array_slot_value)
        .collect())
}

fn materialized_arguments(arguments: &Value) -> Vec<Value> {
    match try_materialized_arguments(arguments) {
        Ok(arguments) => arguments,
        Err(exception) => crate::throw_value(exception),
    }
}

pub fn call_with_arguments(callee: &Value, this_value: Value, arguments: &Value) -> Value {
    call(callee, this_value, materialized_arguments(arguments))
}

pub fn call_with_arguments_completion(
    callee: &Value,
    this_value: Value,
    arguments: &Value,
) -> crate::Completion {
    let arguments = try_materialized_arguments(arguments)?;
    call_completion(callee, this_value, arguments)
}

pub fn call_method(object: &Value, key: &Value, arguments: Vec<Value>) -> Value {
    object.call_method(&key.to_js_string(), arguments)
}

pub fn call_method_completion(
    object: &Value,
    key: &Value,
    arguments: Vec<Value>,
) -> crate::Completion {
    crate::catch_js(|| call_method(object, key, arguments))
}

pub fn call_method_with_arguments(object: &Value, key: &Value, arguments: &Value) -> Value {
    call_method(object, key, materialized_arguments(arguments))
}

pub fn call_method_with_arguments_completion(
    object: &Value,
    key: &Value,
    arguments: &Value,
) -> crate::Completion {
    let arguments = try_materialized_arguments(arguments)?;
    call_method_completion(object, key, arguments)
}

pub fn construct(constructor: &Value, arguments: Vec<Value>) -> Value {
    crate::class::construct(constructor, arguments)
}

pub fn construct_completion(constructor: &Value, arguments: Vec<Value>) -> crate::Completion {
    crate::catch_js(|| construct(constructor, arguments))
}

pub fn construct_with_arguments(constructor: &Value, arguments: &Value) -> Value {
    construct(constructor, materialized_arguments(arguments))
}

pub fn construct_with_arguments_completion(
    constructor: &Value,
    arguments: &Value,
) -> crate::Completion {
    let arguments = try_materialized_arguments(arguments)?;
    construct_completion(constructor, arguments)
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

/// Property access for a DOM node that is still a tagged immediate.
pub struct DomImmOps {
    pub generation: fn() -> u32,
    pub get: fn(u32, &str) -> Value,
    pub set: fn(u32, &str, Value) -> bool,
    pub materialize: fn(u32) -> Value,
    /// Keep a materialized wrapper alive after the caller drops its temporary
    /// object borrow. Used when authored state was stored on a tagged node.
    pub pin: fn(u32),
    pub node_of: fn(&Value) -> Option<u32>,
}

static DOM_IMM: std::sync::OnceLock<DomImmOps> = std::sync::OnceLock::new();

pub fn install_dom_imm(ops: DomImmOps) {
    let _ = DOM_IMM.set(ops);
}

pub fn dom_generation() -> Option<u32> {
    DOM_IMM.get().map(|ops| (ops.generation)())
}

pub fn dom_get(node: u32, key: &str) -> Option<Value> {
    DOM_IMM.get().map(|ops| (ops.get)(node, key))
}

pub fn dom_set(node: u32, key: &str, value: Value) -> bool {
    DOM_IMM.get().is_some_and(|ops| (ops.set)(node, key, value))
}

pub fn materialize_dom(node: u32) -> Option<Value> {
    DOM_IMM.get().map(|ops| (ops.materialize)(node))
}

pub fn pin_dom(node: u32) {
    if let Some(ops) = DOM_IMM.get() {
        (ops.pin)(node);
    }
}

pub fn dom_node_id(value: &Value) -> Option<u32> {
    if !might_be_dom(value) {
        return None;
    }
    DOM_IMM.get().and_then(|ops| (ops.node_of)(value))
}

pub fn dom_nodes_equal(left: &Value, right: &Value) -> Option<bool> {
    if !might_be_dom(left) || !might_be_dom(right) {
        return None;
    }
    let ops = DOM_IMM.get()?;
    match ((ops.node_of)(left), (ops.node_of)(right)) {
        (Some(a), Some(b)) => Some(a == b),
        _ => None,
    }
}

fn might_be_dom(value: &Value) -> bool {
    match value {
        Value::Object(_) => true,
        Value::Imm(imm) => imm.is_dom_node() || imm.is_object_handle(),
        _ => false,
    }
}

/// Direct DOM operations installed by the native runtime. AOT emits these
/// instead of a `call_method` / proxy round trip for the hot element path.
pub struct DomFast {
    pub create_element: fn(&Value, &str) -> Value,
    pub create_text: fn(&Value, &str) -> Value,
    pub append_child: fn(&Value, &Value) -> Value,
    pub set_data: fn(&Value, &str, &Value) -> bool,
    /// `name` + raw text. The bytes are not a `Value`, so a `"prefix" + i`
    /// store can format into a stack buffer and skip the page-string intern.
    pub set_data_str: fn(&Value, &str, &str) -> bool,
    /// One create + id + class + text + append. The create loop uses this
    /// instead of five document borrows and three string interns.
    pub build_appended: fn(&Value, &Value, &str, &str, f64, &str, &str, f64) -> Value,
    /// Fused create used only when `epoch` still matches [`dom_shape_epoch`].
    /// Returns [`Value::Undefined`] when the stamp is stale or the fast path
    /// refuses, so the caller runs the ordinary lookup.
    /// `tag_atom` / `class_atom` are static or prologue-interned ids.
    /// `u32::MAX` means the runtime resolves the name.
    pub build_guarded: fn(&Value, &Value, &str, u32, &str, f64, &str, u32, &str, f64, u64) -> Value,
    /// Fused create + text + one inline style + append. `Value::Undefined`
    /// when `epoch` is stale or the runtime refuses, so the caller runs the
    /// ordinary lookup. `tag_atom == u32::MAX` resolves the name.
    pub build_block: fn(&Value, &Value, &str, u32, &Value, &str, &str, u64) -> Value,
    /// Fused create + text + `height` + `flex` + append for a column-flex
    /// parent of definite uniform rows. `Value::Undefined` when `epoch` is
    /// stale or the parent/row shape is not that flex list, so the caller
    /// runs the ordinary lookup.
    pub build_flex_row: fn(&Value, &Value, &str, u32, &Value, &str, &str, u64) -> Value,
    /// Fused create + text + append. Author rules stay in force; `Undefined`
    /// means the ordinary lookup must run.
    pub build_text: fn(&Value, &Value, &str, u32, &Value, u64) -> Value,
    /// One-shot rewrite of `nodes[(k * stride) % modulus].textContent = prefix + k`
    /// followed by `parent.appendChild(nodes[index])` for `k` in `start..limit`.
    /// Returns the sum of the assigned string lengths, or `Undefined` when the
    /// fast path refuses before any mutation.
    pub rewrite_indexed_text: fn(&Value, &Value, &str, f64, f64, f64, f64, u64) -> Value,
    /// One-shot `nodes[i].textContent = prefix + left + mid + i` for `i` in
    /// `start..limit`. Returns the sum of the assigned string lengths, or
    /// `Undefined` when the fast path refuses before any mutation.
    pub fill_sequential_text: fn(&Value, &str, f64, &str, f64, f64, u64) -> Value,
    pub set_style: fn(&Value, &str, &Value) -> bool,
    pub set_element_style: fn(&Value, &str, &Value) -> bool,
}

static DOM_FAST: std::sync::OnceLock<DomFast> = std::sync::OnceLock::new();

thread_local! {
    /// Bumped by `defineProperty`, `setPrototypeOf`, and assignments that
    /// override the element/document/array surface the fused create path uses.
    static DOM_SHAPE_EPOCH: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    /// Epoch after builtins are installed and before user script. Zero means
    /// the runtime has not sealed a baseline, so fusion stays off.
    static DOM_SHAPE_BASELINE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Stamp sampled once per fused create loop. The fast path compares it on
/// every iteration and falls back when it has moved.
pub fn dom_shape_epoch() -> u64 {
    DOM_SHAPE_EPOCH.with(std::cell::Cell::get)
}

pub fn bump_dom_shape() {
    DOM_SHAPE_EPOCH.with(|epoch| epoch.set(epoch.get().wrapping_add(1)));
}

/// Record the epoch once builtins exist and before user script runs.
///
/// This stamp is not the fusion proof. Unrelated `setPrototypeOf` calls, such
/// as the first `element.style` access, move the epoch before the timed loop.
/// Fusion uses [`dom_fusion_surface_ok`] and then samples the epoch so a later
/// override still falls back.
pub fn note_dom_shape_baseline() {
    let epoch = dom_shape_epoch();
    DOM_SHAPE_BASELINE.with(|cell| cell.set(epoch));
}

/// True when no watched method or accessor has changed since [`note_dom_shape_baseline`].
pub fn dom_shape_is_pristine() -> bool {
    let epoch = dom_shape_epoch();
    DOM_SHAPE_BASELINE.with(|cell| {
        let baseline = cell.get();
        baseline != 0 && baseline == epoch
    })
}

pub fn install_dom_fast(fast: DomFast) {
    let _ = DOM_FAST.set(fast);
}

static DOM_FUSION_SURFACE: std::sync::OnceLock<fn(&Value, &Value, Option<&Value>, &str) -> bool> =
    std::sync::OnceLock::new();

pub fn install_dom_fusion_surface(check: fn(&Value, &Value, Option<&Value>, &str) -> bool) {
    let _ = DOM_FUSION_SURFACE.set(check);
}

/// True when the fused create surface is still the builtins recorded at seal.
///
/// `nodes` is `Some` for create + id/class/text + append + push, and `None`
/// for a styled block. The check does not invoke getters or proxy get traps.
/// Callers that see `true` sample [`dom_shape_epoch`] for [`dom_build_guarded`].
/// An unset checker stays false so fusion cannot run before the runtime installs it.
pub fn dom_fusion_surface_ok(
    document: &Value,
    parent: &Value,
    nodes: Option<&Value>,
    tag: &str,
) -> bool {
    DOM_FUSION_SURFACE
        .get()
        .is_some_and(|check| check(document, parent, nodes, tag))
}

pub fn dom_create_element(object: &Value, tag: &str) -> Value {
    if let Some(fast) = DOM_FAST.get() {
        (fast.create_element)(object, tag)
    } else {
        object.call_method("createElement", vec![Value::string(tag)])
    }
}

pub fn dom_create_element_value(object: &Value, tag: &Value) -> Value {
    dom_create_element(object, &tag.to_js_string())
}

pub fn dom_create_text(object: &Value, text: &str) -> Value {
    if let Some(fast) = DOM_FAST.get() {
        (fast.create_text)(object, text)
    } else {
        object.call_method("createTextNode", vec![Value::string(text)])
    }
}

pub fn dom_create_text_value(object: &Value, text: &Value) -> Value {
    dom_create_text(object, &text.to_js_string())
}

pub fn dom_append_child(parent: &Value, child: &Value) -> Value {
    if let Some(fast) = DOM_FAST.get() {
        (fast.append_child)(parent, child)
    } else {
        parent.call_method("appendChild", vec![child.clone()])
    }
}

pub fn dom_set_data(object: &Value, name: &str, value: &Value) -> bool {
    DOM_FAST
        .get()
        .is_some_and(|fast| (fast.set_data)(object, name, value))
}

/// `object[name] = left + right` when `right` is an integer. Formats into a
/// stack buffer so the create loop does not intern a throwaway `JsString`.
pub fn dom_set_data_concat(object: &Value, name: &str, left: &Value, right: &Value) -> bool {
    let Some(fast) = DOM_FAST.get() else {
        return false;
    };
    let Some(prefix) = left.as_js_string() else {
        return false;
    };
    let Some(number) = right.as_number() else {
        return false;
    };
    if !number.is_finite() || number.fract() != 0.0 || number.abs() >= 1e15 {
        return false;
    }
    let prefix = prefix.as_str();
    let mut buf = [0u8; 96];
    if prefix.len() + 20 > buf.len() {
        return false;
    }
    buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
    let digits = crate::value::write_i64(number as i64, &mut buf[prefix.len()..]);
    let text = std::str::from_utf8(&buf[..prefix.len() + digits]).unwrap_or("");
    (fast.set_data_str)(object, name, text)
}

pub fn format_prefix_index<'a>(prefix: &str, index: f64, buf: &'a mut [u8]) -> Option<&'a str> {
    if !index.is_finite() || index.fract() != 0.0 || index.abs() >= 1e15 {
        return None;
    }
    if prefix.len() + 20 > buf.len() {
        return None;
    }
    buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
    let digits = crate::value::write_i64(index as i64, &mut buf[prefix.len()..]);
    std::str::from_utf8(&buf[..prefix.len() + digits]).ok()
}

/// `prefix + left + mid + index` for two integer operands. ASCII pieces keep
/// the UTF-16 length equal to the byte length.
pub fn format_prefix_pair<'a>(
    prefix: &str,
    left: f64,
    mid: &str,
    index: f64,
    buf: &'a mut [u8],
) -> Option<&'a str> {
    if !left.is_finite() || left.fract() != 0.0 || !(0.0..1e15).contains(&left) {
        return None;
    }
    if !index.is_finite() || index.fract() != 0.0 || !(0.0..1e15).contains(&index) {
        return None;
    }
    if !prefix.is_ascii() || !mid.is_ascii() {
        return None;
    }
    if prefix.len() + mid.len() + 40 > buf.len() {
        return None;
    }
    let mut at = 0usize;
    buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
    at += prefix.len();
    at += crate::value::write_i64(left as i64, &mut buf[at..]);
    buf[at..at + mid.len()].copy_from_slice(mid.as_bytes());
    at += mid.len();
    at += crate::value::write_i64(index as i64, &mut buf[at..]);
    std::str::from_utf8(&buf[..at]).ok()
}

/// `document.createElement(tag)` + id/class/text + `parent.appendChild` in one
/// call. `id` and `text` are `prefix + index` with an integer index.
pub fn dom_build_appended(
    document: &Value,
    parent: &Value,
    tag: &str,
    id_prefix: &str,
    id_index: f64,
    class_name: &str,
    text_prefix: &str,
    text_index: f64,
) -> Value {
    if let Some(fast) = DOM_FAST.get() {
        return (fast.build_appended)(
            document,
            parent,
            tag,
            id_prefix,
            id_index,
            class_name,
            text_prefix,
            text_index,
        );
    }
    let element = dom_create_element(document, tag);
    let _ = dom_set_data_concat(
        &element,
        "id",
        &Value::string(id_prefix),
        &Value::Number(id_index),
    );
    element.set_property("className", Value::string(class_name));
    let _ = dom_set_data_concat(
        &element,
        "textContent",
        &Value::string(text_prefix),
        &Value::Number(text_index),
    );
    dom_append_child(parent, &element)
}

/// Fused create + id + class + text + append, or [`Value::Undefined`] when
/// `epoch` is stale or the runtime refuses the fast path.
pub fn dom_build_guarded(
    document: &Value,
    parent: &Value,
    tag: &str,
    tag_atom: u32,
    id_prefix: &str,
    id_index: f64,
    class_name: &str,
    class_atom: u32,
    text_prefix: &str,
    text_index: f64,
    epoch: u64,
) -> Value {
    if dom_shape_epoch() != epoch {
        return Value::Undefined;
    }
    let Some(fast) = DOM_FAST.get() else {
        return Value::Undefined;
    };
    (fast.build_guarded)(
        document,
        parent,
        tag,
        tag_atom,
        id_prefix,
        id_index,
        class_name,
        class_atom,
        text_prefix,
        text_index,
        epoch,
    )
}

/// Fused create + textContent + one inline style + appendChild, or
/// [`Value::Undefined`] when `epoch` is stale or the runtime refuses.
pub fn dom_build_block_guarded(
    document: &Value,
    parent: &Value,
    tag: &str,
    tag_atom: u32,
    text: &Value,
    style_name: &str,
    style_value: &str,
    epoch: u64,
) -> Value {
    if dom_shape_epoch() != epoch {
        return Value::Undefined;
    }
    let Some(fast) = DOM_FAST.get() else {
        return Value::Undefined;
    };
    (fast.build_block)(
        document,
        parent,
        tag,
        tag_atom,
        text,
        style_name,
        style_value,
        epoch,
    )
}

/// Fused create + textContent + height + flex + appendChild for a column flex
/// list, or [`Value::Undefined`] when `epoch` is stale or the runtime refuses.
pub fn dom_build_text_guarded(
    document: &Value,
    parent: &Value,
    tag: &str,
    tag_atom: u32,
    text: &Value,
    epoch: u64,
) -> Value {
    if let Some(fast) = DOM_FAST.get() {
        (fast.build_text)(document, parent, tag, tag_atom, text, epoch)
    } else {
        Value::Undefined
    }
}

pub fn dom_rewrite_indexed_text_guarded(
    nodes: &Value,
    parent: &Value,
    prefix: &str,
    stride: f64,
    modulus: f64,
    start: f64,
    limit: f64,
    epoch: u64,
) -> Value {
    if let Some(fast) = DOM_FAST.get() {
        (fast.rewrite_indexed_text)(nodes, parent, prefix, stride, modulus, start, limit, epoch)
    } else {
        Value::Undefined
    }
}

/// Fused `nodes[i].textContent = prefix + left + mid + i`, or
/// [`Value::Undefined`] when `epoch` is stale or the runtime refuses.
pub fn dom_fill_sequential_text_guarded(
    nodes: &Value,
    prefix: &str,
    left: f64,
    mid: &str,
    start: f64,
    limit: f64,
    epoch: u64,
) -> Value {
    if let Some(fast) = DOM_FAST.get() {
        (fast.fill_sequential_text)(nodes, prefix, left, mid, start, limit, epoch)
    } else {
        Value::Undefined
    }
}

pub fn dom_build_flex_row_guarded(
    document: &Value,
    parent: &Value,
    tag: &str,
    tag_atom: u32,
    text: &Value,
    height: &str,
    flex_value: &str,
    epoch: u64,
) -> Value {
    if dom_shape_epoch() != epoch {
        return Value::Undefined;
    }
    let Some(fast) = DOM_FAST.get() else {
        return Value::Undefined;
    };
    (fast.build_flex_row)(
        document, parent, tag, tag_atom, text, height, flex_value, epoch,
    )
}

static ATOM_PREHASH: std::sync::OnceLock<fn(&str, u64) -> u32> = std::sync::OnceLock::new();

pub fn install_atom_prehash(prehash: fn(&str, u64) -> u32) {
    let _ = ATOM_PREHASH.set(prehash);
}

/// Intern a loop-invariant name once in the prologue. `u32::MAX` if the
/// runtime has not installed the atom table yet.
pub fn dom_intern_prehashed(text: &str, hash: u64) -> u32 {
    ATOM_PREHASH
        .get()
        .map(|prehash| prehash(text, hash))
        .unwrap_or(u32::MAX)
}

/// `array.push(value)` without allocating an arguments `Vec`.
pub fn array_push(array: &Value, value: &Value) -> Value {
    if let Some(storage) = array.as_array() {
        let mut values = storage.borrow_mut();
        values.push_reserved(value.clone(), 1024);
        return Value::Number(values.len() as f64);
    }
    array.call_method("push", vec![value.clone()])
}

pub fn dom_set_style(object: &Value, name: &str, value: &Value) -> bool {
    DOM_FAST
        .get()
        .is_some_and(|fast| (fast.set_style)(object, name, value))
}

pub fn dom_set_element_style(element: &Value, name: &str, value: &Value) -> bool {
    if let Some(fast) = DOM_FAST.get() {
        if (fast.set_element_style)(element, name, value) {
            return true;
        }
    }
    let style = element.get_property("style");
    style.set_property(name, value.clone());
    true
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
    fn dom_shape_baseline_is_the_pristine_stamp() {
        note_dom_shape_baseline();
        assert!(dom_shape_is_pristine());
        let stamped = dom_shape_epoch();
        bump_dom_shape();
        assert!(!dom_shape_is_pristine());
        assert_ne!(dom_shape_epoch(), stamped);
        note_dom_shape_baseline();
        assert!(dom_shape_is_pristine());
    }

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
        assert_eq!(
            crate::promise::status(&awaited).and_then(|status| match status {
                crate::promise::PromiseStatus::Fulfilled(value) => value.as_number(),
                _ => None,
            }),
            Some(42.0)
        );
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

    #[test]
    fn for_in_keys_does_not_retain_descriptor_objects_for_plain_properties() {
        crate::page_arena::reset();
        let owner = crate::heap::HeapOwner::new();
        let _scope = owner.enter();
        let object = Value::object(HashMap::from([
            ("first".into(), Value::Number(1.0)),
            ("second".into(), Value::Number(2.0)),
            ("third".into(), Value::Number(3.0)),
        ]));
        let warmup = for_in_keys(&object);
        assert_eq!(warmup.iter().count(), 3);
        drop(warmup);
        let before = owner.snapshot();

        for _ in 0..1_000 {
            let keys = for_in_keys(&object);
            assert_eq!(keys.iter().count(), 3);
        }

        let after = owner.snapshot();
        assert_eq!(after.live_objects, before.live_objects);
    }
}

#[cfg(test)]
mod call_completion_tests {
    use super::*;
    use crate::Value;

    #[test]
    fn call_completion_surfaces_host_throw_without_escaping_panic() {
        let boom = Value::function(|_, _| crate::throw_value(Value::string("host boom")));
        let outcome = call_completion(&boom, Value::Undefined, Vec::new());
        assert_eq!(outcome, Err(Value::string("host boom")));
    }

    #[test]
    fn call_with_arguments_completion_rejects_non_array_without_unwind() {
        let noop = Value::function(|_, _| Value::Undefined);
        let outcome = call_with_arguments_completion(&noop, Value::Undefined, &Value::Number(1.0));
        let err = outcome.expect_err("non-array args");
        assert_eq!(err.get_property("name").to_js_string(), "TypeError");
    }
}
