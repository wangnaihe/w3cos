#![allow(non_upper_case_globals, non_snake_case)]

use crate::Value;
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
            (BuiltinKind::Math, "floor") => unary_number(arguments, f64::floor),
            (BuiltinKind::Math, "ceil") => unary_number(arguments, f64::ceil),
            (BuiltinKind::Math, "round") => unary_number(arguments, f64::round),
            (BuiltinKind::Math, "trunc") => unary_number(arguments, f64::trunc),
            (BuiltinKind::Math, "abs") => unary_number(arguments, f64::abs),
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
            (BuiltinKind::Console, _) => Value::Undefined,
            (BuiltinKind::Document, "createElement") => dom_element(),
            _ => Value::Undefined,
        }
    }

    pub fn get_property(&self, key: &str) -> Value {
        match (self.0, key) {
            (BuiltinKind::Document, "body") => dom_element(),
            _ => Value::Undefined,
        }
    }
}

fn unary_number(arguments: Vec<Value>, operation: fn(f64) -> f64) -> Value {
    Value::Number(operation(
        arguments.first().map(Value::to_number).unwrap_or(f64::NAN),
    ))
}

fn object_keys(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::array(
            object
                .borrow()
                .keys()
                .into_iter()
                .map(Value::String)
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

pub struct Map;

impl Map {
    pub fn new(_arguments: Vec<Value>) -> Value {
        let values = std::rc::Rc::new(std::cell::RefCell::new(HashMap::<String, Value>::new()));
        let map = Value::object(HashMap::new());
        {
            let values = values.clone();
            map.set_property(
                "get",
                Value::function(move |_, arguments| {
                    let key = arguments
                        .first()
                        .map(Value::to_js_string)
                        .unwrap_or_default();
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
            let map_reference = map.clone();
            map.set_property(
                "set",
                Value::function(move |_, arguments| {
                    let key = arguments
                        .first()
                        .map(Value::to_js_string)
                        .unwrap_or_default();
                    let value = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                    values.borrow_mut().insert(key, value);
                    map_reference.set_property("size", Value::Number(values.borrow().len() as f64));
                    map_reference.clone()
                }),
            );
        }
        {
            let values = values.clone();
            map.set_property(
                "has",
                Value::function(move |_, arguments| {
                    let key = arguments
                        .first()
                        .map(Value::to_js_string)
                        .unwrap_or_default();
                    Value::Bool(values.borrow().contains_key(&key))
                }),
            );
        }
        map.set_property("size", Value::Number(0.0));
        map
    }
}

pub struct ResizeObserver {
    _private: (),
}

pub const ResizeObserver: Value = Value::Undefined;

impl ResizeObserver {
    pub fn new(_arguments: Vec<Value>) -> Value {
        dom_element()
    }
}

#[cfg(test)]
mod tests {
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
}
