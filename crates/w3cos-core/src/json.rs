//! `JSON.parse` / `JSON.stringify` for the ESM compile pipeline.
//!
//! Parsing goes through `serde_json` and converts into `Value`s (objects
//! become plain `Value::object` hash maps). Both functions signal JS
//! exceptions with `std::panic::panic_any(error_value)` so compiled
//! `try/catch` sees an object with a `message` property.
//!
//! Best-effort corners: the reviver walks bottom-up (array indices as
//! string keys; returning `undefined` deletes object properties but only
//! blanks array slots), the replacer supports the function and key-array
//! forms, and non-finite numbers serialize as `null`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::Value;
use crate::value::js_error;

/// `JSON.parse(text[, reviver])`.
pub fn parse(args: Vec<Value>) -> Value {
    let text = args
        .first()
        .cloned()
        .unwrap_or(Value::Undefined)
        .to_js_string();
    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(parsed) => parsed,
        Err(error) => crate::throw_value(js_error(&format!("SyntaxError: {error}"))),
    };
    let value = from_json(&parsed);
    let reviver = args.get(1).cloned().unwrap_or(Value::Undefined);
    if reviver.is_function() {
        return walk_reviver(&reviver, "", value);
    }
    value
}

fn from_json(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => Value::array(items.iter().map(from_json).collect()),
        serde_json::Value::Object(map) => Value::object(
            map.iter()
                .map(|(key, value)| (key.clone(), from_json(value)))
                .collect::<HashMap<_, _>>(),
        ),
    }
}

/// Bottom-up reviver walk: children first, then `reviver(key, value)`.
fn walk_reviver(reviver: &Value, key: &str, value: Value) -> Value {
    let value = match &value {
        Value::Array(items) => {
            let len = items.borrow().len();
            for index in 0..len {
                let child = items
                    .borrow()
                    .get(index)
                    .cloned()
                    .unwrap_or(Value::Undefined);
                let revived = walk_reviver(reviver, &index.to_string(), child);
                items.borrow_mut()[index] = revived;
            }
            value
        }
        Value::Object(object) => {
            let keys = object.borrow().keys();
            for child_key in keys {
                let child = object.borrow().get_direct(&child_key);
                let revived = walk_reviver(reviver, &child_key, child);
                if revived.is_undefined() {
                    object.borrow_mut().delete(&child_key);
                } else {
                    object.borrow_mut().set_direct(&child_key, revived);
                }
            }
            value
        }
        _ => value,
    };
    reviver.call(
        Value::Undefined,
        vec![Value::String(key.to_string()), value],
    )
}

/// `JSON.stringify(value[, replacer[, space]])`.
pub fn stringify(args: Vec<Value>) -> Value {
    let value = args.first().cloned().unwrap_or(Value::Undefined);
    let replacer = args.get(1).cloned().unwrap_or(Value::Undefined);
    let gap = match args.get(2).cloned().unwrap_or(Value::Undefined) {
        Value::Number(n) => " ".repeat((n.max(0.0) as usize).min(10)),
        Value::String(s) => s.chars().take(10).collect(),
        _ => String::new(),
    };
    let mut context = SerializeContext {
        replacer,
        gap,
        indent: String::new(),
        stack: HashSet::new(),
    };
    match serialize(&mut context, "", &value) {
        Some(text) => Value::String(text),
        None => Value::Undefined,
    }
}

struct SerializeContext {
    replacer: Value,
    gap: String,
    indent: String,
    /// Pointers of the arrays/objects currently being serialized (cycle
    /// detection) — removed again on the way out, so repeated (but not
    /// circular) references serialize fine.
    stack: HashSet<usize>,
}

impl SerializeContext {
    /// Enter a container; panics with a JS error value on a cycle.
    fn enter(&mut self, pointer: usize) {
        if !self.stack.insert(pointer) {
            crate::throw_value(js_error("TypeError: Converting circular structure to JSON"));
        }
    }

    fn leave(&mut self, pointer: usize) {
        self.stack.remove(&pointer);
    }

    fn colon(&self) -> &'static str {
        if self.gap.is_empty() { ":" } else { ": " }
    }
}

/// `None` = the value is dropped from the output (undefined / function).
fn serialize(context: &mut SerializeContext, key: &str, value: &Value) -> Option<String> {
    let value = if context.replacer.is_function() {
        context.replacer.call(
            Value::Undefined,
            vec![Value::String(key.to_string()), value.clone()],
        )
    } else {
        value.clone()
    };
    match &value {
        Value::Undefined | Value::Function(_) => None,
        Value::Null => Some("null".to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(serialize_number(*n)),
        Value::String(s) => Some(serialize_string(s)),
        Value::Array(items) => {
            let pointer = Rc::as_ptr(items) as usize;
            context.enter(pointer);
            // Children serialize with the deeper indent already applied so
            // nested containers line up.
            let inner = push_indent(context);
            let mut parts = Vec::new();
            for (index, item) in items.borrow().iter().enumerate() {
                let text = serialize(context, &index.to_string(), item)
                    .unwrap_or_else(|| "null".to_string());
                parts.push(text);
            }
            let outer = pop_indent(context, &inner);
            context.leave(pointer);
            Some(render_container('[', ']', &parts, &inner, &outer))
        }
        Value::Object(object) => {
            let pointer = Rc::as_ptr(object) as usize;
            context.enter(pointer);
            let whitelist: Option<Vec<String>> = match &context.replacer {
                Value::Array(keys) => Some(keys.borrow().iter().map(Value::to_js_string).collect()),
                _ => None,
            };
            let inner = push_indent(context);
            let mut parts = Vec::new();
            for key in object.borrow().keys() {
                // Hidden runtime plumbing never reaches JSON output.
                if key.starts_with("__w3cos_") {
                    continue;
                }
                if let Some(allowed) = &whitelist {
                    if !allowed.contains(&key) {
                        continue;
                    }
                }
                let child = object.borrow().get_direct(&key);
                if let Some(text) = serialize(context, &key, &child) {
                    parts.push(format!(
                        "{}{}{}",
                        serialize_string(&key),
                        context.colon(),
                        text
                    ));
                }
            }
            let outer = pop_indent(context, &inner);
            context.leave(pointer);
            Some(render_container('{', '}', &parts, &inner, &outer))
        }
    }
}

/// Deepen the current indent by one gap; returns the new (child) indent.
fn push_indent(context: &mut SerializeContext) -> String {
    let gap = context.gap.clone();
    context.indent.push_str(&gap);
    context.indent.clone()
}

/// Restore the indent to before the matching [`push_indent`]; returns the
/// restored (parent) indent.
fn pop_indent(context: &mut SerializeContext, inner: &str) -> String {
    context.indent.truncate(inner.len() - context.gap.len());
    context.indent.clone()
}

fn render_container(open: char, close: char, parts: &[String], inner: &str, outer: &str) -> String {
    if parts.is_empty() {
        return format!("{open}{close}");
    }
    if inner == outer {
        // Compact mode (no gap).
        return format!("{open}{}{close}", parts.join(","));
    }
    let body = parts
        .iter()
        .map(|part| format!("{inner}{part}"))
        .collect::<Vec<_>>()
        .join(",\n");
    format!("{open}\n{body}\n{outer}{close}")
}

/// JS number → JSON text: integers without a trailing `.0`, non-finite
/// values as `null`.
fn serialize_number(n: f64) -> String {
    if !n.is_finite() {
        return "null".to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    if n.fract() == 0.0 && n.abs() < (1u64 << 53) as f64 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Quote + escape via serde_json (identical escaping rules to JS).
fn serialize_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn roundtrip(text: &str) -> String {
        let parsed = parse(vec![Value::string(text)]);
        stringify(vec![parsed]).to_js_string()
    }

    /// Test helper: unwrap a caught JS-exception payload into its Value.
    fn payload_value(payload: Box<dyn std::any::Any + Send>) -> Value {
        crate::promise::payload_to_value(payload)
    }

    #[test]
    fn parse_primitives() {
        assert_eq!(parse(vec![Value::string("42")]).to_number(), 42.0);
        assert_eq!(parse(vec![Value::string("\"hi\"")]).to_js_string(), "hi");
        assert!(parse(vec![Value::string("true")]).to_bool());
        assert!(parse(vec![Value::string("null")]).is_null());
    }

    #[test]
    fn roundtrips_nested() {
        // Object key order follows HashMap iteration, so compare
        // structurally: parse the roundtripped text and inspect fields.
        let text =
            r#"{"name":"monaco","tags":["editor","core"],"opts":{"tab":4,"on":true},"nil":null}"#;
        let value = parse(vec![Value::string(&roundtrip(text))]);
        assert_eq!(value.get_property("name").to_js_string(), "monaco");
        assert_eq!(
            value.get_property("tags").get_property("1").to_js_string(),
            "core"
        );
        assert_eq!(
            value.get_property("opts").get_property("tab").to_number(),
            4.0
        );
        assert!(value.get_property("opts").get_property("on").to_bool());
        assert!(value.get_property("nil").is_null());
    }

    #[test]
    fn parse_builds_navigable_values() {
        let value = parse(vec![Value::string(r#"{"a":[{"b":3}]}"#)]);
        let item = value.get_property("a").get_property("0").get_property("b");
        assert_eq!(item.to_number(), 3.0);
    }

    #[test]
    fn invalid_json_throws_js_error() {
        let outcome = catch_unwind(AssertUnwindSafe(|| parse(vec![Value::string("{oops")])));
        let payload = outcome.expect_err("invalid JSON must panic");
        let error = payload_value(payload);
        assert!(error.is_object());
        assert!(
            error
                .get_property("message")
                .to_js_string()
                .starts_with("SyntaxError:")
        );
    }

    #[test]
    fn reviver_transforms_and_deletes() {
        let reviver = Value::function(|_, args| {
            let key = args[0].to_js_string();
            let value = args.get(1).cloned().unwrap_or(Value::Undefined);
            if key == "drop" {
                Value::Undefined
            } else if value.is_number() {
                Value::Number(value.to_number() * 2.0)
            } else {
                value
            }
        });
        let parsed = parse(vec![Value::string(r#"{"a":1,"drop":2}"#), reviver]);
        assert_eq!(parsed.get_property("a").to_number(), 2.0);
        assert!(parsed.get_property("drop").is_undefined());
    }

    #[test]
    fn stringify_omits_undefined_and_functions_in_objects() {
        let mut props = HashMap::new();
        props.insert("keep".to_string(), Value::Number(1.0));
        props.insert("undef".to_string(), Value::Undefined);
        props.insert("func".to_string(), Value::function(|_, _| Value::Undefined));
        let text = stringify(vec![Value::object(props)]).to_js_string();
        assert_eq!(text, r#"{"keep":1}"#);
    }

    #[test]
    fn stringify_nulls_undefined_and_functions_in_arrays() {
        let text = stringify(vec![Value::array(vec![
            Value::Number(1.0),
            Value::Undefined,
            Value::function(|_, _| Value::Undefined),
        ])])
        .to_js_string();
        assert_eq!(text, "[1,null,null]");
    }

    #[test]
    fn stringify_top_level_undefined_is_undefined() {
        assert!(stringify(vec![Value::Undefined]).is_undefined());
        assert!(stringify(vec![Value::function(|_, _| Value::Undefined)]).is_undefined());
    }

    #[test]
    fn stringify_numbers_without_trailing_zero() {
        assert_eq!(stringify(vec![Value::Number(42.0)]).to_js_string(), "42");
        assert_eq!(stringify(vec![Value::Number(3.14)]).to_js_string(), "3.14");
        assert_eq!(
            stringify(vec![Value::Number(f64::INFINITY)]).to_js_string(),
            "null"
        );
    }

    #[test]
    fn stringify_with_space_argument() {
        let value = parse(vec![Value::string(r#"{"a":1,"b":[true,null]}"#)]);
        let text =
            stringify(vec![value.clone(), Value::Undefined, Value::Number(2.0)]).to_js_string();
        // Key order within a HashMap is not stable — check structure.
        assert!(text.contains("\n  \"a\": 1"));
        assert!(text.contains("\n  \"b\": [\n    true,\n    null\n  ]"));
        assert!(text.ends_with("\n}"));

        let compact = stringify(vec![value, Value::Undefined, Value::string("__")]).to_js_string();
        assert!(compact.contains("\n__\"a\": 1"));
    }

    #[test]
    fn stringify_string_space_argument() {
        let value = parse(vec![Value::string(r#"{"x":1}"#)]);
        let text = stringify(vec![value, Value::Undefined, Value::string("\t")]).to_js_string();
        assert!(text.contains("\n\t\"x\": 1"));
    }

    #[test]
    fn stringify_escapes_strings() {
        assert_eq!(
            stringify(vec![Value::string("a\"b\nc")]).to_js_string(),
            r#""a\"b\nc""#
        );
    }

    #[test]
    fn circular_structure_throws() {
        let array = Value::array(Vec::new());
        array.call_method("push", vec![array.clone()]);
        let outcome = catch_unwind(AssertUnwindSafe(|| stringify(vec![array])));
        let payload = outcome.expect_err("circular input must panic");
        let error = payload_value(payload);
        assert!(
            error
                .get_property("message")
                .to_js_string()
                .contains("circular")
        );
    }

    #[test]
    fn repeated_but_not_circular_references_are_fine() {
        let shared = Value::array(vec![Value::Number(1.0)]);
        let outer = Value::array(vec![shared.clone(), shared]);
        assert_eq!(stringify(vec![outer]).to_js_string(), "[[1],[1]]");
    }

    #[test]
    fn replacer_array_whitelists_keys() {
        let value = parse(vec![Value::string(r#"{"a":1,"b":2}"#)]);
        let text = stringify(vec![value, Value::array(vec![Value::string("a")])]).to_js_string();
        assert_eq!(text, r#"{"a":1}"#);
    }
}
