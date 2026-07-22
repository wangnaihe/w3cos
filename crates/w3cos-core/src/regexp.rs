use std::cell::RefCell;
use std::collections::HashMap;

use regex::RegexBuilder;

use crate::Value;

const SOURCE: &str = "__w3cos_regexp_source";
const FLAGS: &str = "__w3cos_regexp_flags";

/// Create the runtime representation of a JavaScript regular-expression
/// literal. The source and flags stay on an object so string methods can
/// recognize it without expanding the core `Value` enum.
pub fn create(source: &str, flags: &str) -> Value {
    let class = regexp_class();
    create_with_prototype(source, flags, &class.get_property("prototype"))
}

fn create_with_prototype(source: &str, flags: &str, prototype: &Value) -> Value {
    let exec_source = source.to_string();
    let exec_flags = flags.to_string();
    let test_source = source.to_string();
    let test_flags = flags.to_string();
    let value = Value::object(HashMap::from([
        (SOURCE.into(), Value::String(source.into())),
        (FLAGS.into(), Value::String(flags.into())),
        ("lastIndex".into(), Value::Number(0.0)),
        (
            "exec".into(),
            Value::function(move |this, args| {
                exec_pattern_with_receiver(
                    &this,
                    &args.first().cloned().unwrap_or_default().to_js_string(),
                    &exec_source,
                    &exec_flags,
                )
            }),
        ),
        (
            "test".into(),
            Value::function(move |this, args| {
                let input = args.first().cloned().unwrap_or_default().to_js_string();
                Value::Bool(
                    !exec_pattern_with_receiver(&this, &input, &test_source, &test_flags).is_null(),
                )
            }),
        ),
    ]));
    crate::class::set_prototype_of(&value, prototype);
    value
}

/// The global JavaScript `RegExp` constructor. A stable class/prototype pair
/// makes regex literals pass `value instanceof RegExp` checks.
pub fn regexp_class() -> Value {
    thread_local! {
        static REGEXP_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    }

    REGEXP_CLASS.with(|cell| {
        if let Some(value) = cell.borrow().as_ref() {
            return value.clone();
        }

        let prototype = Value::object(HashMap::new());
        let constructor_prototype = prototype.clone();
        let class = Value::callable(HashMap::new(), move |_this, args| {
            let source = args.first().cloned().unwrap_or_default().to_js_string();
            let flags = args.get(1).cloned().unwrap_or_default().to_js_string();
            create_with_prototype(&source, &flags, &constructor_prototype)
        });
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *cell.borrow_mut() = Some(class.clone());
        class
    })
}

fn parts(value: &Value) -> Option<(String, String)> {
    let source = value.get_property(SOURCE);
    if source.is_undefined() {
        return None;
    }
    Some((
        source.to_js_string(),
        value.get_property(FLAGS).to_js_string(),
    ))
}

/// `String.prototype.match` for a runtime RegExp object.
pub fn string_match(input: &str, pattern: &Value) -> Option<Value> {
    let (source, flags) = parts(pattern)?;
    if flags.contains('g') {
        let regex = build_regex(&source, &flags)?;
        let matches: Vec<Value> = regex
            .find_iter(input)
            .map(|matched| Value::String(matched.as_str().into()))
            .collect();
        return Some(if matches.is_empty() {
            Value::Null
        } else {
            Value::array(matches)
        });
    }
    Some(exec_pattern(input, &source, &flags))
}

pub fn string_replace(input: &str, pattern: &Value, replacement: &str) -> Option<Value> {
    let (source, flags) = parts(pattern)?;
    let regex = build_regex(&source, &flags)?;
    let result = if flags.contains('g') {
        regex.replace_all(input, replacement)
    } else {
        regex.replace(input, replacement)
    };
    Some(Value::String(result.into_owned()))
}

fn build_regex(source: &str, flags: &str) -> Option<regex::Regex> {
    let mut builder = RegexBuilder::new(&source);
    builder
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'));
    builder.build().ok()
}

fn exec_pattern(input: &str, source: &str, flags: &str) -> Value {
    exec_pattern_with_receiver(&Value::Undefined, input, source, flags)
}

fn exec_pattern_with_receiver(this: &Value, input: &str, source: &str, flags: &str) -> Value {
    let Some(regex) = build_regex(source, flags) else {
        return Value::Null;
    };
    let stateful = flags.contains('g') || flags.contains('y');
    let start_utf16 = if stateful {
        this.get_property("lastIndex").to_number().max(0.0) as usize
    } else {
        0
    };
    let Some(start_byte) = utf16_offset_to_byte(input, start_utf16) else {
        if stateful {
            this.set_property("lastIndex", Value::Number(0.0));
        }
        return Value::Null;
    };
    let captures = regex.captures_at(input, start_byte).filter(|captures| {
        !flags.contains('y')
            || captures
                .get(0)
                .is_some_and(|matched| matched.start() == start_byte)
    });
    let Some(captures) = captures else {
        if stateful {
            this.set_property("lastIndex", Value::Number(0.0));
        }
        return Value::Null;
    };
    let full_match = captures
        .get(0)
        .expect("captures always include the full match");
    if stateful {
        let end_utf16 = input[..full_match.end()].encode_utf16().count();
        this.set_property("lastIndex", Value::Number(end_utf16 as f64));
    }
    let mut properties: HashMap<String, Value> = (0..captures.len())
        .map(|index| {
            (
                index.to_string(),
                captures
                    .get(index)
                    .map(|matched| Value::String(matched.as_str().into()))
                    .unwrap_or(Value::Undefined),
            )
        })
        .collect();
    properties.insert("length".into(), Value::Number(captures.len() as f64));
    properties.insert(
        "index".into(),
        Value::Number(input[..full_match.start()].encode_utf16().count() as f64),
    );
    properties.insert("input".into(), Value::String(input.into()));
    Value::object(properties)
}

fn utf16_offset_to_byte(input: &str, offset: usize) -> Option<usize> {
    let mut units = 0usize;
    for (byte, ch) in input.char_indices() {
        if units == offset {
            return Some(byte);
        }
        units += ch.len_utf16();
        if units > offset {
            return None;
        }
    }
    (units == offset).then_some(input.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_returns_full_match_and_capture_groups() {
        let pattern = create(r"^#?([0-9A-Fa-f]{6})([0-9A-Fa-f]{2})?$", "");
        let matched = string_match("000000", &pattern).unwrap();
        assert_eq!(matched.get_property("0").to_js_string(), "000000");
        assert_eq!(matched.get_property("1").to_js_string(), "000000");
        assert!(matched.get_property("2").is_undefined());
        assert!(string_match("bad", &pattern).unwrap().is_null());
        assert_eq!(
            pattern
                .call_method("exec", vec![Value::from("#abcdef")])
                .get_property("1")
                .to_js_string(),
            "abcdef"
        );
        assert!(
            pattern
                .call_method("test", vec![Value::from("ffffff")])
                .to_bool()
        );
        assert_eq!(
            string_replace("a.b.c", &create(r"\.", "g"), " ")
                .unwrap()
                .to_js_string(),
            "a b c"
        );
        assert!(crate::class::instance_of(&pattern, &regexp_class()));
        assert!(!crate::class::instance_of(
            &Value::object(HashMap::new()),
            &regexp_class()
        ));
    }

    #[test]
    fn global_exec_advances_last_index_and_resets_after_failure() {
        let pattern = create("a", "g");
        let first = pattern.call_method("exec", vec![Value::from("baab")]);
        assert_eq!(first.get_property("index").to_number(), 1.0);
        assert_eq!(pattern.get_property("lastIndex").to_number(), 2.0);

        let second = pattern.call_method("exec", vec![Value::from("baab")]);
        assert_eq!(second.get_property("index").to_number(), 2.0);
        assert_eq!(pattern.get_property("lastIndex").to_number(), 3.0);

        assert!(
            pattern
                .call_method("exec", vec![Value::from("baab")])
                .is_null()
        );
        assert_eq!(pattern.get_property("lastIndex").to_number(), 0.0);
    }

    #[test]
    fn exec_reports_utf16_indices() {
        let pattern = create("x", "g");
        let matched = pattern.call_method("exec", vec![Value::from("😀x")]);
        assert_eq!(matched.get_property("index").to_number(), 2.0);
        assert_eq!(pattern.get_property("lastIndex").to_number(), 3.0);
    }
}
