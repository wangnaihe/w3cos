#![allow(non_upper_case_globals, non_snake_case)]

use crate::Value;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

thread_local! {
    static OBJECT_CONSTRUCTOR: RefCell<Option<Value>> = const { RefCell::new(None) };
    static ARRAY_CONSTRUCTOR: RefCell<Option<Value>> = const { RefCell::new(None) };
    static STRING_CONSTRUCTOR: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// Drop cached constructors before the page arena reset.
///
/// Their property names are interned strings. Keeping the objects across
/// `page_arena::reset` makes the next realm observe empty keys.
pub fn reset_constructors() {
    OBJECT_CONSTRUCTOR.with(|slot| slot.borrow_mut().take());
    ARRAY_CONSTRUCTOR.with(|slot| slot.borrow_mut().take());
    STRING_CONSTRUCTOR.with(|slot| slot.borrow_mut().take());
    ERROR_CLASSES.with(|slot| slot.borrow_mut().take());
}

#[derive(Clone, Copy)]
enum BuiltinKind {
    Math,
    Object,
    Array,
    Console,
    Document,
}

#[derive(Clone, Copy)]
pub struct BuiltinObject(BuiltinKind);

pub const Math: BuiltinObject = BuiltinObject(BuiltinKind::Math);
pub const Object: BuiltinObject = BuiltinObject(BuiltinKind::Object);
pub const Array: BuiltinObject = BuiltinObject(BuiltinKind::Array);
pub const console: BuiltinObject = BuiltinObject(BuiltinKind::Console);
pub const document: BuiltinObject = BuiltinObject(BuiltinKind::Document);

pub fn object_value() -> Value {
    OBJECT_CONSTRUCTOR.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = build_object_value();
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

fn build_object_value() -> Value {
    let mut properties = HashMap::new();
    let prototype = Value::object(HashMap::from([
        (
            "hasOwnProperty".to_string(),
            Value::function(|this, arguments| this.call_method("hasOwnProperty", arguments)),
        ),
        (
            "isPrototypeOf".to_string(),
            Value::function(|this, arguments| {
                let value = arguments.first().cloned().unwrap_or(Value::Undefined);
                let mut current = crate::class::get_prototype_of(&value);
                let mut visited = Vec::new();
                while current.is_object() {
                    if current.strict_eq(&this) {
                        return Value::Bool(true);
                    }
                    if visited.iter().any(|seen: &Value| seen.strict_eq(&current)) {
                        break;
                    }
                    visited.push(current.clone());
                    current = crate::class::get_prototype_of(&current);
                }
                Value::Bool(false)
            }),
        ),
    ]));
    for method in ["hasOwnProperty", "isPrototypeOf"] {
        crate::class::define_property(
            &prototype,
            method,
            &Value::object(HashMap::from([
                ("value".into(), prototype.get_property(method)),
                ("writable".into(), Value::Bool(true)),
                ("enumerable".into(), Value::Bool(false)),
                ("configurable".into(), Value::Bool(true)),
            ])),
        );
    }
    properties.insert("prototype".to_string(), prototype);
    for name in ["keys", "values", "is"] {
        let method = name.to_string();
        properties.insert(
            name.to_string(),
            Value::function(move |_, arguments| Object.call_method(&method, arguments)),
        );
    }
    properties.insert(
        "create".into(),
        Value::function(|_, arguments| {
            let object = Value::object(HashMap::new());
            if let Some(prototype) = arguments.first()
                && !prototype.is_null()
            {
                crate::class::set_prototype_of(&object, prototype);
            }
            object
        }),
    );
    properties.insert(
        "getPrototypeOf".into(),
        Value::function(|_, arguments| {
            crate::class::get_prototype_of(&arguments.first().cloned().unwrap_or(Value::Undefined))
        }),
    );
    properties.insert(
        "setPrototypeOf".into(),
        Value::function(|_, arguments| {
            let object = arguments.first().cloned().unwrap_or(Value::Undefined);
            let prototype = arguments.get(1).cloned().unwrap_or(Value::Undefined);
            crate::class::set_prototype_of(&object, &prototype);
            object
        }),
    );
    properties.insert(
        "getOwnPropertyDescriptor".into(),
        Value::function(|_, arguments| {
            let object = arguments.first().cloned().unwrap_or(Value::Undefined);
            let key = arguments
                .get(1)
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            crate::class::get_own_property_descriptor(&object, &key)
        }),
    );
    properties.insert(
        "defineProperty".into(),
        Value::function(|_, arguments| {
            let object = arguments.first().cloned().unwrap_or(Value::Undefined);
            let key = arguments
                .get(1)
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_js_string();
            let descriptor = arguments.get(2).cloned().unwrap_or(Value::Undefined);
            crate::class::define_property_from_js(&object, &key, &descriptor)
        }),
    );
    properties.insert(
        "freeze".into(),
        Value::function(|_, arguments| arguments.first().cloned().unwrap_or(Value::Undefined)),
    );
    properties.insert(
        "getOwnPropertyNames".into(),
        Value::function(|_, arguments| {
            arguments
                .first()
                .map(object_own_property_names)
                .unwrap_or_else(|| Value::array(Vec::new()))
        }),
    );
    properties.insert(
        "assign".into(),
        Value::function(|_, arguments| {
            let target = arguments.first().cloned().unwrap_or(Value::Undefined);
            for source in arguments.iter().skip(1) {
                crate::intrinsics::copy_data_properties(&target, source);
            }
            target
        }),
    );
    properties.insert(
        "entries".into(),
        Value::function(|_, arguments| {
            let object = arguments.first().cloned().unwrap_or(Value::Undefined);
            let entries = object_keys(&object)
                .iter()
                .map(|key| {
                    let key = key.to_js_string();
                    Value::array(vec![Value::from(key.clone()), object.get_property(&key)])
                })
                .collect();
            Value::array(entries)
        }),
    );
    properties.insert(
        "fromEntries".into(),
        Value::function(|_, arguments| {
            let entries = arguments.first().cloned().unwrap_or(Value::Undefined);
            let object = Value::object(HashMap::new());
            for entry in entries.iter() {
                let key = entry.get_property("0").to_js_string();
                let value = entry.get_property("1");
                object.set_property(&key, value);
            }
            object
        }),
    );
    Value::callable(properties, |_, arguments| {
        arguments
            .first()
            .cloned()
            .unwrap_or_else(|| Value::object(HashMap::new()))
    })
}

/// Define a non-enumerable data property (`writable`/`configurable` true),
/// the shape ECMAScript gives built-in methods.
pub(crate) fn define_builtin(target: &Value, key: &str, value: Value) {
    crate::class::define_property(
        target,
        key,
        &Value::object(HashMap::from([
            ("value".into(), value),
            ("writable".into(), Value::Bool(true)),
            ("enumerable".into(), Value::Bool(false)),
            ("configurable".into(), Value::Bool(true)),
        ])),
    );
}

/// A native function carrying ECMAScript `name` / `length` (both
/// non-writable, non-enumerable, configurable).
pub(crate) fn native_function(
    name: &str,
    length: u32,
    f: impl Fn(Value, Vec<Value>) -> Value + 'static,
) -> Value {
    // A callable object (not a bare function value) so the descriptors stick.
    let function = Value::callable(HashMap::new(), f);
    for (key, value) in [
        ("length", Value::Number(length as f64)),
        ("name", Value::string(name)),
    ] {
        crate::class::define_property(
            &function,
            key,
            &Value::object(HashMap::from([
                ("value".into(), value),
                ("writable".into(), Value::Bool(false)),
                ("enumerable".into(), Value::Bool(false)),
                ("configurable".into(), Value::Bool(true)),
            ])),
        );
    }
    function
}

/// `String.prototype` methods dispatched to the primitive string methods.
const STRING_METHODS: &[(&str, u32)] = &[
    ("at", 1),
    ("charAt", 1),
    ("charCodeAt", 1),
    ("codePointAt", 1),
    ("concat", 1),
    ("endsWith", 1),
    ("includes", 1),
    ("indexOf", 1),
    ("isWellFormed", 0),
    ("lastIndexOf", 1),
    ("localeCompare", 1),
    ("match", 1),
    ("matchAll", 1),
    ("normalize", 0),
    ("padEnd", 1),
    ("padStart", 1),
    ("repeat", 1),
    ("replace", 2),
    ("replaceAll", 2),
    ("search", 1),
    ("slice", 2),
    ("split", 2),
    ("startsWith", 1),
    ("substr", 2),
    ("substring", 2),
    ("toLocaleLowerCase", 0),
    ("toLocaleUpperCase", 0),
    ("toLowerCase", 0),
    ("toUpperCase", 0),
    ("toWellFormed", 0),
    ("trim", 0),
    ("trimEnd", 0),
    ("trimStart", 0),
];

fn to_uint16(value: &Value) -> u16 {
    let number = value.to_number();
    if !number.is_finite() {
        return 0;
    }
    (number.trunc().rem_euclid(65536.0)) as u16
}

/// Hidden slot holding a `String` wrapper's primitive ([[StringData]]).
const STRING_DATA: &str = "__w3cos_string_data";

pub(crate) fn string_wrapper_data(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    if !object.borrow().has_own_property(STRING_DATA) {
        return None;
    }
    let data = value.get_property(STRING_DATA);
    data.is_string().then_some(data)
}

/// Whether `value` is the realm's `String` constructor (without creating it).
pub(crate) fn is_string_constructor(value: &Value) -> bool {
    STRING_CONSTRUCTOR.with(|slot| slot.borrow().as_ref().is_some_and(|ctor| ctor == value))
}

/// `new String(value)`: an object inheriting `String.prototype` with
/// enumerable index properties and a non-enumerable `length`.
pub(crate) fn string_wrapper(arguments: &[Value]) -> Value {
    let data = arguments
        .first()
        .map(Value::to_js_string)
        .unwrap_or_default();
    let units: Vec<u16> = data.encode_utf16().collect();
    let mut properties = HashMap::new();
    for (index, unit) in units.iter().enumerate() {
        properties.insert(
            index.to_string(),
            Value::from(String::from_utf16_lossy(&[*unit])),
        );
    }
    let wrapper = Value::object(properties);
    for (key, value) in [
        ("length", Value::Number(units.len() as f64)),
        (STRING_DATA, Value::from(data)),
    ] {
        crate::class::define_property(
            &wrapper,
            key,
            &Value::object(HashMap::from([
                ("value".into(), value),
                ("writable".into(), Value::Bool(false)),
                ("enumerable".into(), Value::Bool(false)),
                ("configurable".into(), Value::Bool(false)),
            ])),
        );
    }
    crate::class::set_prototype_of(&wrapper, &string_value().get_property("prototype"));
    wrapper
}

/// The `String` constructor (one per realm, shared by AOT code and the
/// jsdom window so `String.prototype` has a single identity).
pub fn string_value() -> Value {
    STRING_CONSTRUCTOR.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = build_string_value();
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

fn build_string_value() -> Value {
    let prototype = Value::object(HashMap::new());
    for &(name, length) in STRING_METHODS {
        let method = native_function(name, length, move |this, arguments| {
            if this.is_nullish() {
                crate::throw_value(crate::value::type_error(&format!(
                    "String.prototype.{name} called on null or undefined"
                )));
            }
            let receiver = if this.is_string() {
                this
            } else if let Some(data) = string_wrapper_data(&this) {
                data
            } else {
                Value::from(this.to_js_string())
            };
            receiver.call_method(name, arguments)
        });
        define_builtin(&prototype, name, method);
    }
    for name in ["toString", "valueOf"] {
        let method = native_function(name, 0, move |this, _| {
            if this.is_string() {
                this
            } else if let Some(data) = string_wrapper_data(&this) {
                data
            } else {
                crate::throw_value(crate::value::type_error(&format!(
                    "String.prototype.{name} requires that 'this' be a String"
                )))
            }
        });
        define_builtin(&prototype, name, method);
    }
    let constructor = Value::callable(HashMap::new(), |_, arguments| match arguments.first() {
        None => Value::string(""),
        Some(value) => Value::from(value.to_js_string()),
    });
    for (key, value) in [
        ("length", Value::Number(1.0)),
        ("name", Value::string("String")),
    ] {
        crate::class::define_property(
            &constructor,
            key,
            &Value::object(HashMap::from([
                ("value".into(), value),
                ("writable".into(), Value::Bool(false)),
                ("enumerable".into(), Value::Bool(false)),
                ("configurable".into(), Value::Bool(true)),
            ])),
        );
    }
    define_builtin(
        &constructor,
        "fromCharCode",
        native_function("fromCharCode", 1, |_, arguments| {
            let units: Vec<u16> = arguments.iter().map(to_uint16).collect();
            Value::from(String::from_utf16_lossy(&units))
        }),
    );
    define_builtin(
        &constructor,
        "fromCodePoint",
        native_function("fromCodePoint", 1, |_, arguments| {
            let mut units = Vec::new();
            for argument in &arguments {
                let number = argument.to_number();
                if number.fract() != 0.0 || !(0.0..=1_114_111.0).contains(&number) {
                    crate::throw_value(crate::value::range_error(&format!(
                        "Invalid code point {}",
                        argument.to_js_string()
                    )));
                }
                let point = number as u32;
                if point < 0x10000 {
                    units.push(point as u16);
                } else {
                    let point = point - 0x10000;
                    units.push(0xD800 + (point >> 10) as u16);
                    units.push(0xDC00 + (point & 0x3FF) as u16);
                }
            }
            Value::from(String::from_utf16_lossy(&units))
        }),
    );
    define_builtin(
        &constructor,
        "raw",
        native_function("raw", 1, |_, arguments| {
            let strings = arguments.first().cloned().unwrap_or(Value::Undefined);
            let raw = strings.get_property_checked("raw");
            let length = raw.get_property("length").to_number();
            let length = if length.is_finite() && length > 0.0 {
                length as usize
            } else {
                0
            };
            let mut out = String::new();
            for index in 0..length {
                out.push_str(&raw.get_property(&index.to_string()).to_js_string());
                if index + 1 < length {
                    if let Some(substitution) = arguments.get(index + 1) {
                        out.push_str(&substitution.to_js_string());
                    }
                }
            }
            Value::from(out)
        }),
    );
    define_builtin(&constructor, "prototype", prototype.clone());
    define_builtin(&prototype, "constructor", constructor.clone());
    constructor
}

/// `Array.prototype` methods implemented natively for arrays (see
/// `array_call_method`); only names it handles, so dispatch cannot loop.
const ARRAY_METHODS: &[(&str, u32)] = &[
    ("at", 1),
    ("concat", 1),
    ("every", 1),
    ("find", 1),
    ("findIndex", 1),
    ("flat", 0),
    ("flatMap", 1),
    ("includes", 1),
    ("indexOf", 1),
    ("join", 1),
    ("lastIndexOf", 1),
    ("map", 1),
    ("reduce", 1),
    ("reduceRight", 1),
    ("slice", 2),
    ("some", 1),
];

pub fn array_value() -> Value {
    ARRAY_CONSTRUCTOR.with(|slot| {
        if let Some(value) = slot.borrow().clone() {
            return value;
        }
        let value = build_array_value();
        *slot.borrow_mut() = Some(value.clone());
        value
    })
}

fn build_array_value() -> Value {
    let mut properties = HashMap::new();
    properties.insert(
        "isArray".into(),
        Value::function(|_, arguments| Value::Bool(arguments.first().is_some_and(Value::is_array))),
    );
    properties.insert(
        "from".into(),
        Value::function(|_, arguments| array_from(&arguments)),
    );
    properties.insert(
        "of".into(),
        Value::function(|_, arguments| Value::array(arguments)),
    );
    let prototype = Value::object(HashMap::new());
    // Non-mutating methods dispatch to the native array implementation;
    // array-likes run against a snapshot.
    for &(name, length) in ARRAY_METHODS {
        let method = native_function(name, length, move |this, arguments| {
            if this.is_array() {
                return this.call_method(name, arguments);
            }
            if this.is_nullish() {
                crate::throw_value(crate::value::type_error(&format!(
                    "Array.prototype.{name} called on null or undefined"
                )));
            }
            Value::array(array_like_snapshot(&this)).call_method(name, arguments)
        });
        define_builtin(&prototype, name, method);
    }
    let values = Value::function(|this, _| live_array_like_values_iterator(this));
    prototype.set_property("values", values.clone());
    prototype.set_property("__w3cos_symbol_iterator", values);
    prototype.set_property(
        "keys",
        Value::function(|this, _| {
            let keys = (0..array_like_snapshot(&this).len())
                .map(|index| Value::Number(index as f64))
                .collect();
            Value::array(keys).call_method("__w3cos_symbol_iterator", Vec::new())
        }),
    );
    prototype.set_property(
        "entries",
        Value::function(|this, _| {
            let entries = array_like_snapshot(&this)
                .into_iter()
                .enumerate()
                .map(|(index, value)| Value::array(vec![Value::Number(index as f64), value]))
                .collect();
            Value::array(entries).call_method("__w3cos_symbol_iterator", Vec::new())
        }),
    );
    prototype.set_property(
        "forEach",
        Value::function(|this, arguments| {
            let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
            let this_arg = arguments.get(1).cloned().unwrap_or(Value::Undefined);
            for (index, value) in array_like_snapshot(&this).into_iter().enumerate() {
                callback.call(
                    this_arg.clone(),
                    vec![value, Value::Number(index as f64), this.clone()],
                );
            }
            Value::Undefined
        }),
    );
    prototype.set_property(
        "push",
        Value::function(|this, arguments| {
            if this.is_array() {
                return this.call_method("push", arguments);
            }
            let length = this.get_property("length").to_number();
            let mut length = if length.is_finite() && length > 0.0 {
                length.floor() as usize
            } else {
                0
            };
            for value in arguments {
                this.set_property(&length.to_string(), value);
                length += 1;
            }
            this.set_property("length", Value::Number(length as f64));
            Value::Number(length as f64)
        }),
    );
    prototype.set_property(
        "map",
        Value::function(|this, arguments| {
            let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
            let this_arg = arguments.get(1).cloned().unwrap_or(Value::Undefined);
            let length = this.get_property("length").to_number();
            let length = if length.is_finite() && length > 0.0 {
                length.floor() as usize
            } else {
                0
            };
            Value::array(
                (0..length)
                    .map(|index| {
                        if !Value::string(&index.to_string()).js_in(&this).to_bool() {
                            return crate::value::array_hole();
                        }
                        callback.call(
                            this_arg.clone(),
                            vec![
                                this.get_property(&index.to_string()),
                                Value::Number(index as f64),
                                this.clone(),
                            ],
                        )
                    })
                    .collect(),
            )
        }),
    );
    prototype.set_property(
        "filter",
        Value::function(|this, arguments| {
            let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
            let this_arg = arguments.get(1).cloned().unwrap_or(Value::Undefined);
            let length = this.get_property("length").to_number();
            let length = if length.is_finite() && length > 0.0 {
                length.floor() as usize
            } else {
                0
            };
            let mut selected = Vec::new();
            for index in 0..length {
                if !Value::string(&index.to_string()).js_in(&this).to_bool() {
                    continue;
                }
                let value = this.get_property(&index.to_string());
                if callback
                    .call(
                        this_arg.clone(),
                        vec![value.clone(), Value::Number(index as f64), this.clone()],
                    )
                    .to_bool()
                {
                    selected.push(value);
                }
            }
            Value::array(selected)
        }),
    );
    prototype.set_property(
        "indexOf",
        Value::function(|this, arguments| {
            let length = this.get_property("length").to_number();
            let length = if length.is_finite() && length > 0.0 {
                length.floor() as usize
            } else {
                0
            };
            if length == 0 {
                return Value::Number(-1.0);
            }
            let needle = arguments.first().cloned().unwrap_or(Value::Undefined);
            let from = arguments.get(1).map_or(0.0, Value::to_number);
            if from.is_infinite() && from.is_sign_positive() {
                return Value::Number(-1.0);
            }
            let start = if from.is_nan() {
                0
            } else if from < 0.0 {
                (length as f64 + from.ceil()).max(0.0) as usize
            } else {
                from.floor().max(0.0) as usize
            };
            let intrinsic = this
                .as_object()
                .map(|object| object.borrow().get_direct("__w3cosArrayIndexOf"))
                .unwrap_or(Value::Undefined);
            if intrinsic.is_callable() {
                return intrinsic.call(
                    this,
                    vec![
                        needle,
                        Value::Number(length as f64),
                        Value::Number(start as f64),
                    ],
                );
            }
            for index in start..length {
                let key = index.to_string();
                if Value::string(&key).js_in(&this).to_bool()
                    && this.get_property(&key).strict_eq(&needle)
                {
                    return Value::Number(index as f64);
                }
            }
            Value::Number(-1.0)
        }),
    );
    prototype.set_property(
        "slice",
        Value::function(|this, arguments| {
            let values = array_like_slots(&this);
            let start = array_slice_index(arguments.first(), values.len(), 0);
            let end = array_slice_index(arguments.get(1), values.len(), values.len());
            Value::array(values[start.min(end)..end].to_vec())
        }),
    );
    properties.insert("prototype".into(), prototype);
    Value::callable(properties, |_, arguments| {
        if let [length] = arguments.as_slice()
            && let Some(length) = length.as_number()
            && length.is_finite()
            && length >= 0.0
            && length.fract() == 0.0
        {
            return Value::array(
                (0..length as usize)
                    .map(|_| crate::value::array_hole())
                    .collect(),
            );
        }
        Value::array(arguments)
    })
}

fn live_array_like_values_iterator(value: Value) -> Value {
    let index = Rc::new(Cell::new(0_u32));
    let next_index = Rc::clone(&index);
    let iterable = value.clone();
    let iterator = Value::object(HashMap::from([(
        "next".to_string(),
        Value::function(move |_, _| {
            let current = next_index.get();
            let length = iterable.get_property("length").to_u32();
            let done = current >= length;
            let value = if done {
                Value::Undefined
            } else {
                next_index.set(current + 1);
                iterable.get_property(&current.to_string())
            };
            Value::collectable_object(HashMap::from([
                ("value".to_string(), value),
                ("done".to_string(), Value::Bool(done)),
            ]))
        }),
    )]));
    iterator.set_property(
        "__w3cos_symbol_iterator",
        crate::value::self_returning_iterator_method(),
    );
    iterator
}

fn array_like_snapshot(value: &Value) -> Vec<Value> {
    if let Some(values) = value.as_array() {
        return values
            .borrow()
            .iter()
            .filter(|value| !crate::value::is_array_hole(value))
            .cloned()
            .collect();
    }
    let snapshot = value.get_property("__w3cosMapValuesSnapshot");
    if snapshot.is_callable() {
        return snapshot.call(value.clone(), Vec::new()).iter().collect();
    }
    let length = value.get_property("length").to_number();
    if !length.is_finite() || length <= 0.0 {
        return Vec::new();
    }
    (0..length.floor() as usize)
        .map(|index| value.get_property(&index.to_string()))
        .collect()
}

fn array_like_slots(value: &Value) -> Vec<Value> {
    if let Some(values) = value.as_array() {
        return values.borrow().clone();
    }
    let length = value.get_property("length").to_number();
    if !length.is_finite() || length <= 0.0 {
        return Vec::new();
    }
    (0..length.floor() as usize)
        .map(|index| value.get_property(&index.to_string()))
        .collect()
}

fn array_slice_index(argument: Option<&Value>, length: usize, fallback: usize) -> usize {
    let Some(argument) = argument else {
        return fallback;
    };
    if argument.is_undefined() {
        return fallback;
    }
    let number = argument.to_number();
    if number.is_nan() {
        0
    } else if number < 0.0 {
        (length as f64 + number.trunc()).max(0.0) as usize
    } else {
        (number.trunc() as usize).min(length)
    }
}

fn array_from(arguments: &[Value]) -> Value {
    let source = arguments.first().cloned().unwrap_or(Value::Undefined);
    let map_function = arguments.get(1).cloned().unwrap_or(Value::Undefined);
    if !map_function.is_undefined() && !map_function.is_callable() {
        crate::throw_value(crate::value::type_error(
            "Array.from map function must be callable",
        ));
    }
    let this_argument = arguments.get(2).cloned().unwrap_or(Value::Undefined);
    // `Array.from` must prefer a real iterator, but the lightweight runtime's
    // generic object heuristic also classifies host array-like proxies as
    // iterable. FileList is one of those: its indexed values are exposed by a
    // Proxy and its own `length`, while `Value::iter()` has no matching
    // iterator protocol and therefore yields an empty snapshot. Only enter
    // the iterable branch when an actual iterator/snapshot hook exists;
    // otherwise materialize the standard array-like shape below.
    let has_explicit_iterator = !source
        .get_property("__w3cos_symbol_iterator")
        .is_undefined()
        || source.is_array()
        || source.is_string()
        || !source
            .get_property("__w3cosIterableSnapshot")
            .is_undefined()
        || !source.get_property("_first").is_undefined()
        || !source
            .get_property("__w3cosMapValuesSnapshot")
            .is_undefined()
        || !source.get_property("__w3cosMapValues").is_undefined();
    let values = if has_explicit_iterator && source.is_iterable() {
        source.iter().collect::<Vec<_>>()
    } else {
        let length = source.get_property("length").to_number();
        if !length.is_finite() || length <= 0.0 {
            Vec::new()
        } else {
            (0..length.floor() as usize)
                .map(|index| source.get_property(&index.to_string()))
                .collect()
        }
    };
    if map_function.is_undefined() {
        return Value::array(values);
    }
    Value::array(
        values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                map_function.call(
                    this_argument.clone(),
                    vec![value, Value::Number(index as f64)],
                )
            })
            .collect(),
    )
}

pub fn json_value() -> Value {
    Value::object(HashMap::from([
        (
            "parse".into(),
            Value::function(|_, arguments| crate::json::parse(arguments)),
        ),
        (
            "stringify".into(),
            Value::function(|_, arguments| crate::json::stringify(arguments)),
        ),
    ]))
}

impl BuiltinObject {
    pub fn call_method(&self, key: &str, arguments: Vec<Value>) -> Value {
        match (self.0, key) {
            (BuiltinKind::Math, "min") => arguments
                .into_iter()
                .min_by(|left, right| left.to_number().total_cmp(&right.to_number()))
                .unwrap_or(Value::Number(f64::INFINITY)),
            (BuiltinKind::Math, "max") => arguments
                .into_iter()
                .max_by(|left, right| left.to_number().total_cmp(&right.to_number()))
                .unwrap_or(Value::Number(f64::NEG_INFINITY)),
            (BuiltinKind::Math, "abs") => unary_math(arguments, f64::abs),
            (BuiltinKind::Math, "floor") => unary_math(arguments, f64::floor),
            (BuiltinKind::Math, "ceil") => unary_math(arguments, f64::ceil),
            (BuiltinKind::Math, "round") => unary_math(arguments, js_round),
            (BuiltinKind::Math, "f16round") => unary_math(arguments, crate::binary::f16_round),
            (BuiltinKind::Math, "trunc") => unary_math(arguments, f64::trunc),
            (BuiltinKind::Math, "sqrt") => unary_math(arguments, f64::sqrt),
            (BuiltinKind::Math, "log") => unary_math(arguments, f64::ln),
            (BuiltinKind::Math, "log2") => unary_math(arguments, f64::log2),
            (BuiltinKind::Math, "exp") => unary_math(arguments, f64::exp),
            (BuiltinKind::Math, "sin") => unary_math(arguments, f64::sin),
            (BuiltinKind::Math, "cos") => unary_math(arguments, f64::cos),
            (BuiltinKind::Math, "tan") => unary_math(arguments, f64::tan),
            (BuiltinKind::Math, "pow") => binary_math(arguments, f64::powf),
            (BuiltinKind::Math, "atan2") => binary_math(arguments, f64::atan2),
            (BuiltinKind::Math, "hypot") => {
                Value::Number(arguments.iter().map(Value::to_number).fold(0.0, f64::hypot))
            }
            (BuiltinKind::Math, "random") => math_random(),
            (BuiltinKind::Math, "clz32") => Value::Number(
                arguments
                    .first()
                    .map(Value::to_i32)
                    .unwrap_or(0)
                    .cast_unsigned()
                    .leading_zeros() as f64,
            ),
            (BuiltinKind::Object, "is") => Value::Bool(
                arguments
                    .first()
                    .zip(arguments.get(1))
                    .is_some_and(|(left, right)| left.strict_eq(right)),
            ),
            (BuiltinKind::Object, "keys") => arguments
                .first()
                .map(object_keys)
                .unwrap_or_else(|| Value::array(Vec::new())),
            (BuiltinKind::Object, "values") => arguments
                .first()
                .map(object_values)
                .unwrap_or_else(|| Value::array(Vec::new())),
            (BuiltinKind::Array, "from") => array_from(&arguments),
            (BuiltinKind::Console, _) => {
                // Debug channel for compiled apps: W3COS_JS_CONSOLE=1 makes
                // console.* print to stderr (production default stays silent).
                if std::env::var_os("W3COS_JS_CONSOLE").is_some() {
                    let line = arguments
                        .iter()
                        .map(Value::to_js_string)
                        .collect::<Vec<_>>()
                        .join(" ");
                    eprintln!("[js.console.{key}] {line}");
                }
                Value::Undefined
            }
            (BuiltinKind::Document, "createElement") => dom_element(),
            _ => Value::Undefined,
        }
    }

    pub fn get_property(&self, key: &str) -> Value {
        match (self.0, key) {
            (
                BuiltinKind::Math,
                "min" | "max" | "abs" | "floor" | "ceil" | "round" | "trunc" | "sqrt" | "log"
                | "log2" | "exp" | "sin" | "cos" | "tan" | "pow" | "atan2" | "hypot" | "random"
                | "clz32" | "f16round",
            ) => {
                let builtin = *self;
                let method = key.to_string();
                Value::function(move |_, arguments| builtin.call_method(&method, arguments))
            }
            (BuiltinKind::Math, "E") => Value::Number(std::f64::consts::E),
            (BuiltinKind::Math, "LN2") => Value::Number(std::f64::consts::LN_2),
            (BuiltinKind::Math, "LN10") => Value::Number(std::f64::consts::LN_10),
            (BuiltinKind::Math, "LOG2E") => Value::Number(std::f64::consts::LOG2_E),
            (BuiltinKind::Math, "LOG10E") => Value::Number(std::f64::consts::LOG10_E),
            (BuiltinKind::Math, "PI") => Value::Number(std::f64::consts::PI),
            (BuiltinKind::Math, "SQRT1_2") => Value::Number(std::f64::consts::FRAC_1_SQRT_2),
            (BuiltinKind::Math, "SQRT2") => Value::Number(std::f64::consts::SQRT_2),
            (BuiltinKind::Document, "body") => dom_element(),
            _ => Value::Undefined,
        }
    }

    /// Builtin facades are never nullish, so checked member access is the
    /// ordinary property lookup. This mirrors [`Value::get_property_checked`]
    /// for compiler-generated member expressions without changing the
    /// facade's static Rust type.
    pub fn get_property_checked(&self, key: &str) -> Value {
        self.get_property(key)
    }
}

/// ECMAScript `Math` as a first-class value.
///
/// Direct calls and extracted methods must share the same generic builtin
/// implementation (`const clz32 = Math.clz32` is common in library code).
///
/// `Math` is a singleton: `Math === Math`, and properties user code adds to
/// it stay visible. The value is cached per thread and per page-arena epoch
/// (a [`crate::page_arena::reset`] retires arena objects, so the next page
/// builds a fresh one). Building it allocates ~30 properties, which used to
/// dominate numeric code that calls `Math.*` in a loop.
pub fn math_value() -> Value {
    let epoch = crate::page_arena::current_epoch();
    let cached = MATH_VALUE.with(|cell| {
        cell.borrow()
            .as_ref()
            .filter(|(cached_epoch, _)| *cached_epoch == epoch)
            .map(|(_, value)| value.clone())
    });
    if let Some(value) = cached {
        return value;
    }
    let value = build_math_value();
    // Drop the retired value outside the borrow.
    let retired = MATH_VALUE.with(|cell| cell.replace(Some((epoch, value.clone()))));
    drop(retired);
    value
}

fn build_math_value() -> Value {
    let mut properties = HashMap::new();
    for method in [
        "min", "max", "abs", "floor", "ceil", "round", "f16round", "trunc", "sqrt", "log", "log2",
        "exp", "sin", "cos", "tan", "pow", "atan2", "hypot", "random", "clz32",
    ] {
        properties.insert(method.to_string(), Math.get_property(method));
    }
    for constant in [
        "E", "LN2", "LN10", "LOG2E", "LOG10E", "PI", "SQRT1_2", "SQRT2",
    ] {
        properties.insert(constant.to_string(), Math.get_property(constant));
    }
    Value::object(properties)
}

/// The global `process` object: one shared object per page (like
/// `math_value`), so `process.env.X = ...` writes persist and identity holds
/// across references. `env`, `argv`, `cwd()`, `pid`, `execPath` and `exit()`
/// come from the host process; `w3cos-node`'s `process` module layers
/// EventEmitter, nextTick and hrtime on top of this object.
pub fn process_value() -> Value {
    let epoch = crate::page_arena::current_epoch();
    let cached = PROCESS_VALUE.with(|cell| {
        cell.borrow()
            .as_ref()
            .filter(|(cached_epoch, _)| *cached_epoch == epoch)
            .map(|(_, value)| value.clone())
    });
    if let Some(value) = cached {
        return value;
    }
    let value = build_process_value();
    let retired = PROCESS_VALUE.with(|cell| cell.replace(Some((epoch, value.clone()))));
    drop(retired);
    value
}

fn build_process_value() -> Value {
    let env: HashMap<String, Value> = std::env::vars()
        .map(|(key, value)| (key, Value::from(value)))
        .collect();
    let argv: Vec<Value> = std::env::args().map(Value::from).collect();
    let exec_path = std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| std::env::args().next())
        .unwrap_or_default();
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    let mut properties = HashMap::new();
    properties.insert("env".to_string(), Value::object(env));
    properties.insert("argv".to_string(), Value::array(argv));
    properties.insert("execPath".to_string(), Value::from(exec_path));
    properties.insert(
        "pid".to_string(),
        Value::Number(f64::from(std::process::id())),
    );
    properties.insert("arch".to_string(), Value::string(arch));
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    properties.insert("platform".to_string(), Value::string(platform));
    properties.insert(
        "nextTick".to_string(),
        Value::function(|_this, args| {
            let mut args = args.into_iter();
            let callback = args.next().unwrap_or(Value::Undefined);
            if !callback.is_callable() {
                crate::value::throw_value(error_instance(
                    "TypeError",
                    vec![Value::from(
                        "The \"callback\" argument must be of type function",
                    )],
                ));
            }
            let rest: Vec<Value> = args.collect();
            crate::promise::queue_microtask(Value::function(move |_this, _args| {
                callback.call(Value::Undefined, rest.clone())
            }));
            Value::Undefined
        }),
    );
    properties.insert(
        "cwd".to_string(),
        Value::function(|_this, _args| {
            std::env::current_dir()
                .map(|path| Value::from(path.to_string_lossy().into_owned()))
                .unwrap_or_else(|_| Value::string("/"))
        }),
    );
    properties.insert(
        "chdir".to_string(),
        Value::function(|_this, args| {
            let directory = args.first().map(Value::to_js_string).unwrap_or_default();
            if let Err(error) = std::env::set_current_dir(&directory) {
                crate::value::throw_value(error_instance(
                    "Error",
                    vec![Value::from(format!("ENOENT: {error}, chdir '{directory}'"))],
                ));
            }
            Value::Undefined
        }),
    );
    properties.insert(
        "exit".to_string(),
        Value::function(|_this, args| {
            let code = args.first().map(Value::to_number).unwrap_or(0.0);
            std::process::exit(if code.is_finite() { code as i32 } else { 0 })
        }),
    );
    Value::object(properties)
}

thread_local! {
    static MATH_VALUE: RefCell<Option<(u32, Value)>> = const { RefCell::new(None) };
    static PROCESS_VALUE: RefCell<Option<(u32, Value)>> = const { RefCell::new(None) };
    static MATH_RANDOM_STATE: RefCell<u64> = const { RefCell::new(0x4d59_5df4_d0f3_3173) };
}

fn math_random() -> Value {
    MATH_RANDOM_STATE.with(|state| {
        let next = (*state.borrow())
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state.borrow_mut() = next;
        Value::Number(((next >> 11) as f64) / ((1_u64 << 53) as f64))
    })
}

fn unary_math(arguments: Vec<Value>, operation: fn(f64) -> f64) -> Value {
    Value::Number(operation(
        arguments.first().map(Value::to_number).unwrap_or(f64::NAN),
    ))
}

fn binary_math(arguments: Vec<Value>, operation: fn(f64, f64) -> f64) -> Value {
    Value::Number(operation(
        arguments.first().map(Value::to_number).unwrap_or(f64::NAN),
        arguments.get(1).map(Value::to_number).unwrap_or(f64::NAN),
    ))
}

fn js_round(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        value
    } else {
        (value + 0.5).floor()
    }
}

pub(crate) fn object_keys(value: &Value) -> Value {
    if let Some(object) = value.as_object() {
        let keys = object.borrow().own_keys().iter().collect::<Vec<_>>();
        return Value::array(
            keys.into_iter()
                .filter(|key| {
                    let key = key.to_js_string();
                    object
                        .borrow()
                        .get_own_property_descriptor(&key)
                        .get_property("enumerable")
                        .to_bool()
                })
                .collect(),
        );
    }
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
    Value::array(Vec::new())
}

fn object_own_property_names(value: &Value) -> Value {
    if let Some(object) = value.as_object() {
        return object.borrow().own_keys();
    }
    if let Some(values) = value.as_array() {
        let mut keys = values
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, value)| !crate::value::is_array_hole(value))
            .map(|(index, _)| Value::from(index.to_string()))
            .collect::<Vec<_>>();
        keys.push(Value::from("length"));
        return Value::array(keys);
    }
    Value::array(Vec::new())
}

fn object_values(value: &Value) -> Value {
    if value.as_object().is_some() {
        let keys = object_keys(value);
        return Value::array(
            keys.iter()
                .map(|key| value.get_property(&key.to_js_string()))
                .collect(),
        );
    }
    if let Some(values) = value.as_array() {
        return Value::array(
            values
                .borrow()
                .iter()
                .filter(|value| !crate::value::is_array_hole(value))
                .cloned()
                .collect(),
        );
    }
    Value::array(Vec::new())
}

fn dom_element() -> Value {
    let element = Value::object(HashMap::new());
    element.set_property("style", Value::object(HashMap::new()));
    for method in [
        "appendChild",
        "removeChild",
        "observe",
        "unobserve",
        "addEventListener",
        "removeEventListener",
        "hasAttribute",
        "getAttribute",
    ] {
        element.set_property(method, Value::function(|_, _| Value::Undefined));
    }
    element
}

pub fn parseInt(arguments: Vec<Value>) -> Value {
    let value = arguments.first().cloned().unwrap_or(Value::Undefined);
    let text = value.to_js_string();
    let mut rest = text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let mut sign = 1.0;
    if let Some(stripped) = rest.strip_prefix('-') {
        sign = -1.0;
        rest = stripped;
    } else if let Some(stripped) = rest.strip_prefix('+') {
        rest = stripped;
    }
    let radix_arg = arguments.get(1).map(Value::to_number).unwrap_or(0.0);
    let mut radix = if radix_arg.is_finite() {
        (radix_arg.trunc() as i64 as i32) as u32
    } else {
        0
    };
    let mut strip_prefix = true;
    if radix != 0 {
        if !(2..=36).contains(&radix) {
            return Value::Number(f64::NAN);
        }
        if radix != 16 {
            strip_prefix = false;
        }
    } else {
        radix = 10;
    }
    if strip_prefix && (rest.starts_with("0x") || rest.starts_with("0X")) {
        rest = &rest[2..];
        radix = 16;
    }
    let digits: Vec<u32> = rest.chars().map_while(|c| c.to_digit(radix)).collect();
    if digits.is_empty() {
        return Value::Number(f64::NAN);
    }
    let number = if radix == 10 {
        // Decimal: let the float parser round the full digit string.
        let text: String = digits
            .iter()
            .map(|d| char::from_digit(*d, 10).expect("digit"))
            .collect();
        text.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        digits
            .iter()
            .fold(0.0, |acc, d| acc * f64::from(radix) + f64::from(*d))
    };
    Value::Number(sign * number)
}

pub fn parseFloat(arguments: Vec<Value>) -> Value {
    let value = arguments.first().cloned().unwrap_or(Value::Undefined);
    let text = value.to_js_string();
    let prefix: String = text
        .chars()
        .take_while(|character| {
            character.is_ascii_digit() || matches!(character, '-' | '+' | '.' | 'e' | 'E')
        })
        .collect();
    Value::Number(prefix.parse::<f64>().unwrap_or(f64::NAN))
}

pub struct Error(pub Value);

impl Error {
    pub fn new(arguments: Vec<Value>) -> Self {
        Self(arguments.first().cloned().unwrap_or(Value::Undefined))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

pub fn RangeError(arguments: Vec<Value>) -> Value {
    arguments.first().cloned().unwrap_or(Value::Undefined)
}

pub fn ErrorValue(arguments: Vec<Value>) -> Value {
    arguments.first().cloned().unwrap_or(Value::Undefined)
}

const ERROR_CLASS_NAMES: &[&str] = &[
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "AggregateError",
];

thread_local! {
    static ERROR_CLASSES: RefCell<Option<HashMap<String, Value>>> = const { RefCell::new(None) };
}

fn build_error_classes() -> HashMap<String, Value> {
    let mut classes = HashMap::new();
    for name in ERROR_CLASS_NAMES {
        let error_name = (*name).to_string();
        let constructor_name = error_name.clone();
        let constructor = Value::callable(HashMap::new(), move |_, arguments| {
            error_instance(&constructor_name, arguments)
        });
        constructor.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::from([
            ("name".to_string(), Value::string(name)),
            ("message".to_string(), Value::string("")),
        ]));
        prototype.set_property("constructor", constructor.clone());
        constructor.set_property("prototype", prototype);
        classes.insert(error_name, constructor);
    }

    let error_prototype = classes["Error"].get_property("prototype");
    error_prototype.set_property(
        "toString",
        Value::function(|this, _| {
            let name = this.get_property("name").to_js_string();
            let message = this.get_property("message").to_js_string();
            Value::from(if name.is_empty() {
                message
            } else if message.is_empty() {
                name
            } else {
                format!("{name}: {message}")
            })
        }),
    );
    for name in ERROR_CLASS_NAMES
        .iter()
        .copied()
        .filter(|name| *name != "Error")
    {
        crate::class::set_prototype_of(&classes[name], &classes["Error"]);
        crate::class::set_prototype_of(&classes[name].get_property("prototype"), &error_prototype);
    }
    classes
}

pub fn error_class(name: &str) -> Value {
    ERROR_CLASSES.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(build_error_classes());
        }
        slot.borrow()
            .as_ref()
            .and_then(|classes| classes.get(name))
            .cloned()
            .unwrap_or_else(|| {
                slot.borrow().as_ref().expect("error classes initialized")["Error"].clone()
            })
    })
}

pub fn error_instance(name: &str, arguments: Vec<Value>) -> Value {
    let (message_index, options_index) = if name == "AggregateError" {
        (1, 2)
    } else {
        (0, 1)
    };
    let message_value = arguments
        .get(message_index)
        .cloned()
        .unwrap_or(Value::Undefined);
    let message = if message_value.is_undefined() {
        String::new()
    } else {
        message_value.to_js_string()
    };
    let value = Value::object(HashMap::from([
        ("name".to_string(), Value::string(name)),
        ("message".to_string(), Value::string(&message)),
        (
            "stack".to_string(),
            Value::from(if message.is_empty() {
                name.to_string()
            } else {
                format!("{name}: {message}")
            }),
        ),
    ]));
    if name == "AggregateError" {
        let errors = arguments.first().cloned().unwrap_or(Value::Undefined);
        value.set_property("errors", Value::array(errors.iter().collect()));
    }
    let cause = arguments
        .get(options_index)
        .map(|options| options.get_property("cause"))
        .unwrap_or(Value::Undefined);
    if !cause.is_undefined() {
        value.set_property("cause", cause);
    }
    crate::class::set_prototype_of(&value, &error_class(name).get_property("prototype"));
    value
}

pub struct Map;

/// The key for the JS `Map` builtin: SameValueZero semantics — primitive
/// values by (type-tagged) value, objects/arrays/functions by reference
/// identity (Rc pointer).
fn map_key(value: &Value) -> String {
    match value.unpack() {
        crate::value::ValueUnpack::Undefined => "u:".to_string(),
        crate::value::ValueUnpack::Null => "z:".to_string(),
        crate::value::ValueUnpack::Bool(b) => format!("b:{b}"),
        // Canonicalize -0 to +0 (SameValueZero) and let all NaNs share a key.
        crate::value::ValueUnpack::Number(n) => {
            if n.is_nan() {
                "n:NaN".to_string()
            } else if n == 0.0 {
                "n:0".to_string()
            } else {
                format!("n:{n}")
            }
        }
        crate::value::ValueUnpack::String(s) => format!("s:{s}"),
        crate::value::ValueUnpack::Array(a) => format!("a:{:#x}", a.identity()),
        crate::value::ValueUnpack::Object(o) => format!("o:{:#x}", o.identity()),
        crate::value::ValueUnpack::Function(f) => format!("f:{:#x}", f.identity()),
    }
}

impl Map {
    pub fn new(arguments: Vec<Value>) -> Value {
        let mut initial = HashMap::<String, Value>::new();
        let iterable = arguments.first().cloned().unwrap_or(Value::Undefined);
        let entries_snapshot = iterable.get_property("__w3cosMapEntriesSnapshot");
        let entries = iterable.get_property("__w3cosMapEntries");
        let source = if entries_snapshot.is_function() {
            entries_snapshot.call(iterable.clone(), vec![])
        } else if entries.is_array() {
            entries
        } else {
            iterable
        };
        for entry in source.iter() {
            if let Some(pair) = entry.as_array() {
                let pair = pair.borrow();
                if let Some(key) = pair.first() {
                    initial.insert(
                        key.to_js_string(),
                        pair.get(1).cloned().unwrap_or(Value::Undefined),
                    );
                }
            }
        }
        let values = std::rc::Rc::new(std::cell::RefCell::new(initial));
        let map = Value::object(HashMap::new());
        {
            let values = values.clone();
            map.set_property(
                "get",
                Value::function(move |_, arguments| {
                    let key = arguments.first().map(map_key).unwrap_or_default();
                    values
                        .borrow()
                        .get(&key)
                        .cloned()
                        .unwrap_or(Value::Undefined)
                }),
            );
        }
        {
            let values = values.clone();
            map.set_property(
                "set",
                Value::function(move |map, arguments| {
                    let key = arguments.first().map(map_key).unwrap_or_default();
                    let value = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                    values.borrow_mut().insert(key, value);
                    sync_map_size(&map, values.borrow().len());
                    map
                }),
            );
        }
        {
            let values = values.clone();
            map.set_property(
                "has",
                Value::function(move |_, arguments| {
                    let key = arguments.first().map(map_key).unwrap_or_default();
                    Value::Bool(values.borrow().contains_key(&key))
                }),
            );
        }
        {
            let values = values.clone();
            map.set_property(
                "forEach",
                Value::function(move |_, arguments| {
                    let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
                    for (key, value) in values.borrow().iter() {
                        callback.call(
                            Value::Undefined,
                            vec![value.clone(), Value::from(key.clone())],
                        );
                    }
                    Value::Undefined
                }),
            );
        }
        {
            let values = values.clone();
            map.set_property(
                "__w3cosMapEntriesSnapshot",
                Value::function(move |_, _| map_entries_snapshot(&values.borrow())),
            );
        }
        {
            let values = values.clone();
            map.set_property(
                "__w3cosMapValuesSnapshot",
                Value::function(move |_, _| map_values_snapshot(&values.borrow())),
            );
        }
        sync_map_size(&map, values.borrow().len());
        map
    }
}

fn map_entries_snapshot(values: &HashMap<String, Value>) -> Value {
    Value::array(
        values
            .iter()
            .map(|(key, value)| Value::array(vec![Value::from(key.clone()), value.clone()]))
            .collect(),
    )
}

fn map_values_snapshot(values: &HashMap<String, Value>) -> Value {
    Value::array(values.values().cloned().collect())
}

fn sync_map_size(map: &Value, len: usize) {
    map.set_property("size", Value::Number(len as f64));
}

pub struct Set;

impl Set {
    pub fn new(arguments: Vec<Value>) -> Value {
        let values = std::rc::Rc::new(std::cell::RefCell::new(HashMap::<String, Value>::new()));
        if let Some(iterable) = arguments.first() {
            for item in iterable.iter() {
                values.borrow_mut().insert(map_key(&item), item);
            }
        }
        let set = Value::object(HashMap::new());
        {
            let values = values.clone();
            let set_reference = set.clone();
            set.set_property(
                "add",
                Value::function(move |_, arguments| {
                    let item = arguments.first().cloned().unwrap_or(Value::Undefined);
                    values.borrow_mut().insert(map_key(&item), item);
                    set_reference.set_property("size", Value::Number(values.borrow().len() as f64));
                    set_reference.clone()
                }),
            );
        }
        {
            let values = values.clone();
            set.set_property(
                "has",
                Value::function(move |_, arguments| {
                    let key = arguments.first().map(map_key).unwrap_or_default();
                    Value::Bool(values.borrow().contains_key(&key))
                }),
            );
        }
        {
            let values = values.clone();
            let set_reference = set.clone();
            set.set_property(
                "delete",
                Value::function(move |_, arguments| {
                    let key = arguments.first().map(map_key).unwrap_or_default();
                    let removed = values.borrow_mut().remove(&key).is_some();
                    set_reference.set_property("size", Value::Number(values.borrow().len() as f64));
                    Value::Bool(removed)
                }),
            );
        }
        {
            let values = values.clone();
            let set_reference = set.clone();
            set.set_property(
                "clear",
                Value::function(move |_, _| {
                    values.borrow_mut().clear();
                    set_reference.set_property("size", Value::Number(0.0));
                    Value::Undefined
                }),
            );
        }
        set.set_property("size", Value::Number(values.borrow().len() as f64));
        set
    }
}

pub struct ResizeObserver {
    _private: (),
}

pub const ResizeObserver: Value = Value::Undefined;

struct ResizeObserverTarget {
    element: Value,
    last_size: Option<(f32, f32)>,
}

struct ResizeObserverState {
    callback: Value,
    observer: Value,
    targets: std::collections::HashMap<u64, ResizeObserverTarget>,
}

thread_local! {
    static NEXT_RESIZE_OBSERVER: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    static RESIZE_OBSERVERS: std::cell::RefCell<std::collections::HashMap<u64, ResizeObserverState>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

impl ResizeObserver {
    pub fn new(arguments: Vec<Value>) -> Value {
        let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
        let observer_id = NEXT_RESIZE_OBSERVER.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        });
        RESIZE_OBSERVERS.with(|observers| {
            observers.borrow_mut().insert(
                observer_id,
                ResizeObserverState {
                    callback: callback.clone(),
                    observer: Value::Undefined,
                    targets: std::collections::HashMap::new(),
                },
            );
        });

        let observer = dom_element();
        let observe_callback = callback;
        observer.set_property(
            "observe",
            Value::function(move |_, arguments| {
                let element = arguments.first().cloned().unwrap_or(Value::Undefined);
                let options = arguments.get(1).cloned().unwrap_or_default();
                let box_kind = if options.get_property("box").is_undefined() {
                    "content-box".to_string()
                } else {
                    options.get_property("box").to_js_string()
                };
                if !matches!(
                    box_kind.as_str(),
                    "content-box" | "border-box" | "device-pixel-content-box"
                ) {
                    crate::throw_value(crate::error_instance(
                        "TypeError",
                        vec![Value::string("ResizeObserver box option is invalid")],
                    ));
                }
                let host_id = element
                    .get_property("__w3cosHostId")
                    .to_js_string()
                    .parse::<u64>()
                    .ok();
                if let Some(host_id) = host_id {
                    if std::env::var_os("W3COS_RESIZE_TRACE").is_some() {
                        eprintln!("[W3C OS][RESIZE] observe host={host_id}");
                    }
                    RESIZE_OBSERVERS.with(|observers| {
                        observers
                            .borrow_mut()
                            .entry(observer_id)
                            .or_insert_with(|| ResizeObserverState {
                                callback: observe_callback.clone(),
                                observer: Value::Undefined,
                                targets: std::collections::HashMap::new(),
                            })
                            .targets
                            .insert(
                                host_id,
                                ResizeObserverTarget {
                                    element,
                                    last_size: None,
                                },
                            );
                    });
                }
                Value::Undefined
            }),
        );
        RESIZE_OBSERVERS.with(|observers| {
            if let Some(state) = observers.borrow_mut().get_mut(&observer_id) {
                state.observer = observer.clone();
            }
        });
        observer.set_property(
            "unobserve",
            Value::function(move |_, arguments| {
                let host_id = arguments
                    .first()
                    .map(|element| element.get_property("__w3cosHostId"))
                    .map(|value| value.to_js_string())
                    .and_then(|value| value.parse::<u64>().ok());
                if let Some(host_id) = host_id {
                    RESIZE_OBSERVERS.with(|observers| {
                        if let Some(observer) = observers.borrow_mut().get_mut(&observer_id) {
                            observer.targets.remove(&host_id);
                        }
                    });
                }
                Value::Undefined
            }),
        );
        observer.set_property(
            "disconnect",
            Value::function(move |_, _| {
                RESIZE_OBSERVERS.with(|observers| {
                    if let Some(observer) = observers.borrow_mut().get_mut(&observer_id) {
                        observer.targets.clear();
                    }
                });
                Value::Undefined
            }),
        );
        observer
    }
}

/// Deliver native border-box measurements to JavaScript `ResizeObserver`
/// callbacks. Returns `true` when at least one callback was invoked.
pub fn dispatch_resize_observers(sizes: &[(u64, f32, f32)]) -> bool {
    dispatch_resize_observers_bounded(sizes, usize::MAX).0
}

/// Deliver at most `max_entries` changed native border-box measurements.
///
/// The second return value is `true` when the entry budget was exhausted.
/// Callers should schedule another delivery turn in that case; targets which
/// were not delivered deliberately keep their previous size.
pub fn dispatch_resize_observers_bounded(
    sizes: &[(u64, f32, f32)],
    max_entries: usize,
) -> (bool, bool) {
    let sizes: std::collections::HashMap<u64, (f32, f32)> = sizes
        .iter()
        .map(|(host_id, width, height)| (*host_id, (*width, *height)))
        .collect();
    let mut remaining = max_entries.max(1);
    let deliveries = RESIZE_OBSERVERS.with(|observers| {
        let mut observers = observers.borrow_mut();
        let mut deliveries = Vec::new();
        for observer in observers.values_mut() {
            let mut entries = Vec::new();
            let mut host_ids = observer.targets.keys().copied().collect::<Vec<_>>();
            host_ids.sort_unstable();
            for host_id in host_ids {
                if remaining == 0 {
                    break;
                }
                let Some(target) = observer.targets.get_mut(&host_id) else {
                    continue;
                };
                let Some(&(width, height)) = sizes.get(&host_id) else {
                    continue;
                };
                if target.last_size.is_some_and(|(last_width, last_height)| {
                    (last_width - width).abs() <= 0.01 && (last_height - height).abs() <= 0.01
                }) {
                    continue;
                }
                target.last_size = Some((width, height));
                if std::env::var_os("W3COS_RESIZE_TRACE").is_some() {
                    eprintln!("[W3C OS][RESIZE] host={host_id} border-box={width:.2}x{height:.2}");
                }

                let border_box = Value::object(std::collections::HashMap::from([
                    ("inlineSize".into(), Value::Number(width as f64)),
                    ("blockSize".into(), Value::Number(height as f64)),
                ]));
                let content_rect = Value::object(std::collections::HashMap::from([
                    ("x".into(), Value::Number(0.0)),
                    ("y".into(), Value::Number(0.0)),
                    ("top".into(), Value::Number(0.0)),
                    ("left".into(), Value::Number(0.0)),
                    ("right".into(), Value::Number(width as f64)),
                    ("bottom".into(), Value::Number(height as f64)),
                    ("width".into(), Value::Number(width as f64)),
                    ("height".into(), Value::Number(height as f64)),
                ]));
                entries.push(Value::object(std::collections::HashMap::from([
                    ("target".into(), target.element.clone()),
                    ("contentRect".into(), content_rect),
                    (
                        "borderBoxSize".into(),
                        Value::array(vec![border_box.clone()]),
                    ),
                    ("contentBoxSize".into(), Value::array(vec![border_box])),
                    (
                        "devicePixelContentBoxSize".into(),
                        Value::array(vec![Value::object(std::collections::HashMap::from([
                            ("inlineSize".into(), Value::Number(width as f64)),
                            ("blockSize".into(), Value::Number(height as f64)),
                        ]))]),
                    ),
                ])));
                remaining -= 1;
            }
            if !entries.is_empty() && observer.callback.is_function() {
                deliveries.push((
                    observer.callback.clone(),
                    observer.observer.clone(),
                    Value::array(entries),
                ));
            }
            if remaining == 0 {
                break;
            }
        }
        deliveries
    });

    let delivered = !deliveries.is_empty();
    for (callback, observer, entries) in deliveries {
        callback.call(Value::Undefined, vec![entries, observer]);
    }
    (delivered, remaining == 0)
}

#[cfg(test)]
mod monaco_tests {
    use super::*;

    #[test]
    fn math_floor_matches_javascript_number_semantics() {
        assert_eq!(
            Math.call_method("floor", vec![Value::Number(2.75)])
                .to_number(),
            2.0
        );
        assert_eq!(
            Math.call_method("floor", vec![Value::Number(-2.25)])
                .to_number(),
            -3.0
        );
        assert!(Math.call_method("floor", vec![]).to_number().is_nan());
    }

    #[test]
    fn map_constructor_copies_entries_and_iterates_values() {
        let first = Map::new(vec![]);
        first.call_method("set", vec![Value::from("24"), Value::Number(106.0)]);
        first.call_method("set", vec![Value::from("25"), Value::Number(82.0)]);

        let copy = Map::new(vec![first]);
        assert_eq!(
            copy.call_method("get", vec![Value::from("24")]).to_number(),
            106.0
        );
        assert_eq!(copy.get_property("size").to_number(), 2.0);
        let mut heights = copy
            .iter()
            .map(|value| value.to_number())
            .collect::<Vec<_>>();
        heights.sort_by(f64::total_cmp);
        assert_eq!(heights, vec![82.0, 106.0]);
    }

    #[test]
    fn map_methods_do_not_retain_their_own_receiver() {
        let map = Map::new(vec![]);
        let Some(object) = map.as_object() else {
            panic!("Map constructor did not return an object");
        };

        assert!(
            object.host_strong_count().is_none(),
            "interned Map must not wrap as host Rc; methods use the call receiver"
        );
        assert_eq!(object.interned_handle(), map.object_handle());
    }

    #[test]
    fn resize_observer_delivers_changed_border_box_sizes_once() {
        let deliveries = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = deliveries.clone();
        let observer = ResizeObserver::new(vec![Value::function(move |_, arguments| {
            let entry = arguments[0].get_property("0");
            recorded.borrow_mut().push(
                entry
                    .get_property("borderBoxSize")
                    .get_property("0")
                    .get_property("blockSize")
                    .to_number(),
            );
            Value::Undefined
        })]);
        let target = dom_element();
        target.set_property("__w3cosHostId", Value::from("42"));
        observer.call_method("observe", vec![target.clone()]);

        assert!(dispatch_resize_observers(&[(42, 320.0, 84.0)]));
        assert!(!dispatch_resize_observers(&[(42, 320.0, 84.0)]));
        assert!(dispatch_resize_observers(&[(42, 320.0, 112.0)]));
        observer.call_method("disconnect", vec![]);
        assert!(!dispatch_resize_observers(&[(42, 320.0, 128.0)]));
        observer.call_method("observe", vec![target]);
        assert!(dispatch_resize_observers(&[(42, 320.0, 128.0)]));
        assert_eq!(&*deliveries.borrow(), &[84.0, 112.0, 128.0]);
    }

    #[test]
    fn resize_observer_defers_entries_beyond_delivery_budget() {
        let deliveries = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = deliveries.clone();
        let observer = ResizeObserver::new(vec![Value::function(move |_, arguments| {
            recorded
                .borrow_mut()
                .push(arguments[0].get_property("length").to_number() as usize);
            Value::Undefined
        })]);
        for host_id in 100..106 {
            let target = dom_element();
            target.set_property("__w3cosHostId", Value::from(host_id.to_string()));
            observer.call_method("observe", vec![target]);
        }
        let sizes = (100..106)
            .map(|host_id| (host_id, 320.0, 80.0 + host_id as f32))
            .collect::<Vec<_>>();

        assert_eq!(dispatch_resize_observers_bounded(&sizes, 4), (true, true));
        assert_eq!(dispatch_resize_observers_bounded(&sizes, 4), (true, false));
        assert_eq!(dispatch_resize_observers_bounded(&sizes, 4), (false, false));
        assert_eq!(&*deliveries.borrow(), &[4, 2]);
        observer.call_method("disconnect", vec![]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_value_is_a_per_page_singleton() {
        let first = math_value();
        let second = math_value();
        assert!(first.strict_eq(&second), "Math === Math");
        first.set_property("__w3cos_test_marker", Value::Number(7.0));
        assert_eq!(
            math_value().get_property("__w3cos_test_marker").to_number(),
            7.0
        );
        let sqrt = math_value().get_property("sqrt");
        assert_eq!(
            sqrt.call(Value::Undefined, vec![Value::Number(9.0)])
                .to_number(),
            3.0
        );
        drop((first, second, sqrt));
        crate::page_arena::reset();
        let fresh = math_value();
        assert!(fresh.get_property("__w3cos_test_marker").is_undefined());
        assert_eq!(fresh.get_property("PI").to_number(), std::f64::consts::PI);
    }

    #[test]
    fn math_round_and_common_numeric_methods() {
        assert_eq!(
            Math.call_method("round", vec![Value::Number(19.6)]),
            Value::Number(20.0)
        );
        assert_eq!(
            Math.call_method("floor", vec![Value::Number(19.9)]),
            Value::Number(19.0)
        );
        assert_eq!(
            Math.call_method("pow", vec![Value::Number(3.0), Value::Number(2.0)]),
            Value::Number(9.0)
        );
    }

    #[test]
    fn math_methods_are_first_class_function_values() {
        let log = Math.get_property("log");
        assert!(log.is_function());
        assert_eq!(
            log.call(Value::Undefined, vec![Value::Number(8.0)])
                .to_number(),
            8.0_f64.ln()
        );
        assert_eq!(Math.get_property("LN2").to_number(), std::f64::consts::LN_2);
        assert_eq!(
            Math.get_property("clz32")
                .call(Value::Undefined, vec![Value::Number(32.0)])
                .to_number(),
            26.0
        );
        let math = math_value();
        assert_eq!(
            math.get_property("hypot")
                .call(
                    Value::Undefined,
                    vec![Value::Number(3.0), Value::Number(4.0)]
                )
                .to_number(),
            5.0
        );
        let random = math
            .get_property("random")
            .call(Value::Undefined, vec![])
            .to_number();
        assert!((0.0..1.0).contains(&random));
    }

    #[test]
    fn standard_global_facades_share_sparse_array_semantics() {
        let array_constructor = array_value();
        let sparse = array_constructor.call(Value::Undefined, vec![Value::Number(3.0)]);
        sparse.set_property("1", Value::string("middle"));

        let object = object_value();
        assert_eq!(
            object
                .call_method("keys", vec![sparse.clone()])
                .to_js_string(),
            "1"
        );
        assert_eq!(
            object
                .call_method("values", vec![sparse.clone()])
                .to_js_string(),
            "middle"
        );
        assert_eq!(
            array_constructor
                .call_method("isArray", vec![sparse.clone()])
                .to_bool(),
            true
        );
        assert_eq!(
            json_value()
                .call_method("stringify", vec![sparse])
                .to_js_string(),
            "[null,\"middle\",null]"
        );
    }

    #[test]
    fn array_prototype_map_is_generic_for_array_like_objects() {
        let source = Value::object(HashMap::from([
            ("0".to_string(), Value::string("first")),
            ("1".to_string(), Value::string("second")),
            ("length".to_string(), Value::Number(2.0)),
        ]));
        let map = array_value().get_property("prototype").get_property("map");
        let mapped = map.call(
            source,
            vec![Value::function(|_, arguments| {
                arguments[0].get_property("length")
            })],
        );

        assert_eq!(mapped.get_property("length"), Value::Number(2.0));
        assert_eq!(mapped.get_property("0"), Value::Number(5.0));
        assert_eq!(mapped.get_property("1"), Value::Number(6.0));
    }

    #[test]
    fn array_prototype_filter_is_generic_for_array_like_objects() {
        let source = Value::object(HashMap::from([
            ("0".to_string(), Value::string("first")),
            ("1".to_string(), Value::string("second")),
            ("length".to_string(), Value::Number(2.0)),
        ]));
        let filter = array_value()
            .get_property("prototype")
            .get_property("filter");
        let filtered = filter.call(
            source,
            vec![Value::function(|_, arguments| {
                Value::Bool(arguments[0].to_js_string().starts_with('s'))
            })],
        );

        assert_eq!(filtered.get_property("length"), Value::Number(1.0));
        assert_eq!(filtered.get_property("0"), Value::string("second"));
    }

    #[test]
    fn array_method_reference_apply_uses_the_explicit_receiver() {
        let targets = Value::array(vec![Value::string("existing")]);
        let generated = Value::array(vec![Value::string("first"), Value::string("second")]);
        let push = Value::array(Vec::new()).get_property("push");

        assert!(push.is_callable());
        assert_eq!(
            push.call_method("apply", vec![targets.clone(), generated]),
            Value::Number(3.0)
        );
        assert_eq!(targets.get_property("0"), Value::string("existing"));
        assert_eq!(targets.get_property("1"), Value::string("first"));
        assert_eq!(targets.get_property("2"), Value::string("second"));
    }

    #[test]
    fn array_prototype_index_of_is_generic_and_observes_length() {
        let needle = Value::object(HashMap::new());
        let source = Value::object(HashMap::from([
            ("0".to_string(), Value::string("first")),
            ("1".to_string(), needle.clone()),
            ("length".to_string(), Value::Number(2.0)),
        ]));
        let index_of = array_value()
            .get_property("prototype")
            .get_property("indexOf");

        assert_eq!(
            index_of.call(source.clone(), vec![needle.clone()]),
            Value::Number(1.0)
        );
        source.set_property("length", Value::Number(1.0));
        assert_eq!(index_of.call(source, vec![needle]), Value::Number(-1.0));
    }

    #[test]
    fn array_from_materializes_array_like_objects() {
        let file = Value::object(HashMap::from([(
            "name".to_string(),
            Value::string("photo.png"),
        )]));
        let file_list = Value::object(HashMap::from([
            ("0".to_string(), file),
            ("length".to_string(), Value::Number(1.0)),
        ]));

        let files = array_value().call_method("from", vec![file_list]);

        assert!(files.is_array());
        assert_eq!(files.get_property("length").to_number(), 1.0);
        assert_eq!(
            files.get_property("0").get_property("name").to_js_string(),
            "photo.png"
        );
    }

    #[test]
    fn array_from_applies_mapping_function_with_index_and_this_argument() {
        let this_argument = Value::object(HashMap::from([(
            "prefix".to_string(),
            Value::string("item"),
        )]));
        let mapped = array_value().call_method(
            "from",
            vec![
                Value::array(vec![Value::string("a"), Value::string("b")]),
                Value::function(|this, arguments| {
                    Value::string(&format!(
                        "{}-{}-{}",
                        this.get_property("prefix").to_js_string(),
                        arguments[0].to_js_string(),
                        arguments[1].to_u32()
                    ))
                }),
                this_argument,
            ],
        );

        assert_eq!(mapped.get_property("0"), Value::string("item-a-0"));
        assert_eq!(mapped.get_property("1"), Value::string("item-b-1"));
    }

    #[test]
    fn array_from_materializes_proxy_backed_array_like_objects() {
        let file = Value::object(HashMap::from([
            ("name".to_string(), Value::string("photo.png")),
            ("type".to_string(), Value::string("image/png")),
        ]));
        let indexed_file = file.clone();
        let handler = crate::ProxyBuilder::new()
            .get(move |target, key, _receiver| {
                if key == "0" {
                    indexed_file.clone()
                } else {
                    target.get_property(key)
                }
            })
            .build();
        let file_list = Value::Object(std::rc::Rc::new(std::cell::RefCell::new(
            crate::JsObject::with_proxy(
                HashMap::from([("length".to_string(), Value::Number(1.0))]),
                handler,
            ),
        )));

        let files = array_value().call_method("from", vec![file_list]);

        assert_eq!(files.get_property("length").to_number(), 1.0);
        assert_eq!(
            files.get_property("0").get_property("type").to_js_string(),
            "image/png"
        );
    }

    #[test]
    fn object_prototype_has_own_property_is_callable_with_an_explicit_receiver() {
        let object = object_value();
        let has_own_property = object
            .get_property("prototype")
            .get_property("hasOwnProperty");
        let target = Value::object(HashMap::from([("present".to_string(), Value::Bool(true))]));

        assert!(
            has_own_property
                .call_method("call", vec![target.clone(), Value::string("present")],)
                .to_bool()
        );
        assert!(
            !has_own_property
                .call_method("call", vec![target, Value::string("missing")])
                .to_bool()
        );
    }

    #[test]
    fn object_set_prototype_of_and_is_prototype_of_share_the_live_chain() {
        let object = object_value();
        let prototype = object.call_method("create", vec![object.get_property("prototype")]);
        let target = Value::object(HashMap::new());

        assert!(
            object
                .call_method("setPrototypeOf", vec![target.clone(), prototype.clone()])
                .strict_eq(&target)
        );
        assert!(
            prototype
                .call_method("isPrototypeOf", vec![target])
                .to_bool()
        );
    }

    #[test]
    fn object_entries_preserve_property_insertion_order() {
        let object = Value::object(HashMap::new());
        object.set_property("none", Value::Number(0.0));
        object.set_property("xs", Value::Number(4.0));
        object.set_property("sm", Value::Number(8.0));

        let entries = object_value().call_method("entries", vec![object]);
        assert_eq!(
            entries.get_property("0").get_property("0"),
            Value::string("none")
        );
        assert_eq!(
            entries.get_property("1").get_property("0"),
            Value::string("xs")
        );
        assert_eq!(
            entries.get_property("2").get_property("0"),
            Value::string("sm")
        );
    }

    #[test]
    fn object_from_entries_preserves_iterable_order() {
        let entries = Value::array(vec![
            Value::array(vec![Value::string("second"), Value::Number(2.0)]),
            Value::array(vec![Value::string("first"), Value::Number(1.0)]),
        ]);

        let object = object_value().call_method("fromEntries", vec![entries]);
        assert_eq!(object.get_property("second"), Value::Number(2.0));
        assert_eq!(object.get_property("first"), Value::Number(1.0));
        assert_eq!(
            object_value()
                .call_method("keys", vec![object])
                .to_js_string(),
            "second,first"
        );
    }

    #[test]
    fn error_family_has_browser_shaped_instances_and_prototypes() {
        assert!(crate::class::get_prototype_of(&error_class("TypeError")) == error_class("Error"));
        let cause = Value::string("root");
        let type_error = crate::class::construct(
            &error_class("TypeError"),
            vec![
                Value::string("bad input"),
                Value::object(HashMap::from([("cause".to_string(), cause.clone())])),
            ],
        );
        assert_eq!(type_error.get_property("name").to_js_string(), "TypeError");
        assert_eq!(
            type_error.get_property("message").to_js_string(),
            "bad input"
        );
        assert!(type_error.get_property("cause") == cause);
        assert_eq!(
            type_error.call_method("toString", vec![]).to_js_string(),
            "TypeError: bad input"
        );
        assert!(crate::class::instance_of(
            &type_error,
            &error_class("TypeError")
        ));
        assert!(crate::class::instance_of(
            &type_error,
            &error_class("Error")
        ));

        let aggregate = crate::class::construct(
            &error_class("AggregateError"),
            vec![
                Value::array(vec![type_error.clone(), Value::string("second")]),
                Value::string("many"),
            ],
        );
        assert_eq!(
            aggregate
                .get_property("errors")
                .get_property("length")
                .to_u32(),
            2
        );
        assert!(crate::class::instance_of(
            &aggregate,
            &error_class("AggregateError")
        ));
        assert!(crate::class::instance_of(&aggregate, &error_class("Error")));
    }
}

/// A host function value carrying ECMAScript `name` / `length` (both kept
/// out of enumeration by `FunctionData::keys`).
pub fn named_function(
    name: &str,
    length: u32,
    f: impl Fn(Value, Vec<Value>) -> Value + 'static,
) -> Value {
    let function = Value::function(f);
    // Built-in functions are not constructors: no own `prototype`.
    function.delete_property("prototype");
    function.set_property("name", Value::string(name));
    function.set_property("length", Value::Number(length as f64));
    function
}

/// [`named_function`] for an ordinary function declaration: also owns a
/// `prototype` whose non-enumerable `constructor` points back at it.
pub fn named_constructor_function(
    name: &str,
    length: u32,
    f: impl Fn(Value, Vec<Value>) -> Value + 'static,
) -> Value {
    let function = named_function(name, length, f);
    let prototype = Value::object(HashMap::new());
    define_builtin(&prototype, "constructor", function.clone());
    function.set_property("prototype", prototype);
    function
}
