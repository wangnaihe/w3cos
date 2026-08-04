#![allow(non_upper_case_globals, non_snake_case)]

use crate::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

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
    let mut properties = HashMap::new();
    let prototype = Value::object(HashMap::from([(
        "hasOwnProperty".to_string(),
        Value::function(|this, arguments| this.call_method("hasOwnProperty", arguments)),
    )]));
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
            crate::class::define_property(&object, &key, &descriptor)
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
                .map(object_keys)
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
                    Value::array(vec![Value::String(key.clone()), object.get_property(&key)])
                })
                .collect();
            Value::array(entries)
        }),
    );
    Value::callable(properties, |_, arguments| {
        arguments
            .first()
            .cloned()
            .unwrap_or_else(|| Value::object(HashMap::new()))
    })
}

pub fn array_value() -> Value {
    let mut properties = HashMap::new();
    properties.insert(
        "isArray".into(),
        Value::function(|_, arguments| Value::Bool(arguments.first().is_some_and(Value::is_array))),
    );
    properties.insert(
        "from".into(),
        Value::function(|_, arguments| {
            let source = arguments.first().cloned().unwrap_or(Value::Undefined);
            if source.is_iterable() {
                Value::array(source.iter().collect())
            } else {
                Value::array(Vec::new())
            }
        }),
    );
    properties.insert(
        "of".into(),
        Value::function(|_, arguments| Value::array(arguments)),
    );
    Value::callable(properties, |_, arguments| {
        if let [Value::Number(length)] = arguments.as_slice()
            && length.is_finite()
            && *length >= 0.0
            && length.fract() == 0.0
        {
            return Value::array(
                (0..*length as usize)
                    .map(|_| crate::value::array_hole())
                    .collect(),
            );
        }
        Value::array(arguments)
    })
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
            (BuiltinKind::Array, "from") => arguments
                .first()
                .cloned()
                .unwrap_or_else(|| Value::array(Vec::new())),
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
pub fn math_value() -> Value {
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

thread_local! {
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
    match value {
        Value::Object(object) => Value::array(
            object
                .borrow()
                .keys()
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
        Value::Array(values) => Value::array(
            values
                .borrow()
                .iter()
                .enumerate()
                .filter(|(_, value)| !crate::value::is_array_hole(value))
                .map(|(index, _)| Value::String(index.to_string()))
                .collect(),
        ),
        _ => Value::array(Vec::new()),
    }
}

fn object_values(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let object = object.borrow();
            Value::array(
                object
                    .keys()
                    .into_iter()
                    .map(|key| object.get_direct(&key))
                    .collect(),
            )
        }
        Value::Array(values) => Value::array(
            values
                .borrow()
                .iter()
                .filter(|value| !crate::value::is_array_hole(value))
                .cloned()
                .collect(),
        ),
        _ => Value::array(Vec::new()),
    }
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
    let parsed = value
        .to_js_string()
        .trim()
        .split(|character: char| !character.is_ascii_digit() && character != '-')
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(f64::NAN);
    Value::Number(parsed)
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
        let constructor =
            Value::function(move |_, arguments| error_instance(&constructor_name, arguments));
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
            Value::String(if name.is_empty() {
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
            Value::String(if message.is_empty() {
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
    match value {
        Value::Undefined => "u:".to_string(),
        Value::Null => "z:".to_string(),
        Value::Bool(b) => format!("b:{b}"),
        // Canonicalize -0 to +0 (SameValueZero) and let all NaNs share a key.
        Value::Number(n) => {
            if n.is_nan() {
                "n:NaN".to_string()
            } else if *n == 0.0 {
                "n:0".to_string()
            } else {
                format!("n:{n}")
            }
        }
        Value::String(s) => format!("s:{s}"),
        Value::Array(rc) => format!("a:{:p}", std::rc::Rc::as_ptr(rc)),
        Value::Object(rc) => format!("o:{:p}", std::rc::Rc::as_ptr(rc)),
        Value::Function(f) => format!("f:{:#x}", f.identity()),
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
        } else if matches!(entries, Value::Array(_)) {
            entries
        } else {
            iterable
        };
        for entry in source.iter() {
            if let Value::Array(pair) = entry {
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
        let Value::Object(object) = map else {
            panic!("Map constructor did not return an object");
        };

        assert_eq!(
            std::rc::Rc::strong_count(&object),
            1,
            "a method stored on the Map must use its call receiver instead of creating an Rc cycle"
        );
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
    fn object_prototype_has_own_property_is_callable_with_an_explicit_receiver() {
        let object = object_value();
        let has_own_property = object
            .get_property("prototype")
            .get_property("hasOwnProperty");
        let target = Value::object(HashMap::from([(
            "present".to_string(),
            Value::Bool(true),
        )]));

        assert!(
            has_own_property
                .call_method(
                    "call",
                    vec![target.clone(), Value::string("present")],
                )
                .to_bool()
        );
        assert!(
            !has_own_property
                .call_method("call", vec![target, Value::string("missing")])
                .to_bool()
        );
    }

    #[test]
    fn error_family_has_browser_shaped_instances_and_prototypes() {
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
