use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fancy_regex::Regex;

use crate::Value;

const SOURCE: &str = "__w3cos_regexp_source";
const FLAGS: &str = "__w3cos_regexp_flags";

fn syntax_error(message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string("SyntaxError")),
        ("message".into(), Value::string(message)),
    ]))
}

fn type_error(message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string("TypeError")),
        ("message".into(), Value::string(message)),
    ]))
}

fn validate(source: &str, flags: &str) {
    let mut seen = Vec::new();
    for flag in flags.chars() {
        if !matches!(flag, 'd' | 'g' | 'i' | 'm' | 's' | 'u' | 'v' | 'y') || seen.contains(&flag) {
            crate::throw_value(syntax_error(&format!(
                "Invalid regular expression flags: {flags}"
            )));
        }
        seen.push(flag);
    }
    if flags.contains('u') && flags.contains('v') {
        crate::throw_value(syntax_error(
            "The 'u' and 'v' regular expression flags cannot be used together",
        ));
    }
    if build_regex(source, flags).is_none() {
        crate::throw_value(syntax_error(&format!(
            "Invalid or unsupported regular expression: /{source}/{flags}"
        )));
    }
}

fn canonical_flags(flags: &str) -> String {
    "dgimsuvy"
        .chars()
        .filter(|flag| flags.contains(*flag))
        .collect()
}

fn display_source(source: &str) -> String {
    if source.is_empty() {
        "(?:)".into()
    } else {
        source
            .replace('/', r"\/")
            .replace('\n', r"\n")
            .replace('\r', r"\r")
    }
}

/// Create the runtime representation of a JavaScript regular-expression
/// literal. The source and flags stay on an object so string methods can
/// recognize it without expanding the core `Value` enum.
pub fn create(source: &str, flags: &str) -> Value {
    let class = regexp_class();
    create_with_prototype(source, flags, &class.get_property("prototype"))
}

fn create_with_prototype(source: &str, flags: &str, prototype: &Value) -> Value {
    validate(source, flags);
    let flags = canonical_flags(flags);
    let exec_source = source.to_string();
    let exec_flags = flags.clone();
    let test_source = source.to_string();
    let test_flags = flags.clone();
    let string_source = display_source(source);
    let string_flags = flags.clone();
    let value = Value::object(HashMap::from([
        (SOURCE.into(), Value::String(source.into())),
        (FLAGS.into(), Value::String(flags.clone())),
        ("source".into(), Value::String(display_source(source))),
        ("flags".into(), Value::String(flags.clone())),
        ("global".into(), Value::Bool(flags.contains('g'))),
        ("ignoreCase".into(), Value::Bool(flags.contains('i'))),
        ("multiline".into(), Value::Bool(flags.contains('m'))),
        ("dotAll".into(), Value::Bool(flags.contains('s'))),
        ("unicode".into(), Value::Bool(flags.contains('u'))),
        ("unicodeSets".into(), Value::Bool(flags.contains('v'))),
        ("sticky".into(), Value::Bool(flags.contains('y'))),
        ("hasIndices".into(), Value::Bool(flags.contains('d'))),
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
        (
            "toString".into(),
            Value::function(move |_, _| Value::from(format!("/{string_source}/{string_flags}"))),
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
            let pattern = args.first().cloned().unwrap_or_default();
            let inherited = parts(&pattern);
            let source = inherited
                .as_ref()
                .map(|(source, _)| source.clone())
                .unwrap_or_else(|| pattern.to_js_string());
            let flags = if args.get(1).is_none_or(Value::is_undefined) {
                inherited.map(|(_, flags)| flags).unwrap_or_default()
            } else {
                args[1].to_js_string()
            };
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
            .filter_map(Result::ok)
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

/// `String.prototype.search` for a runtime RegExp object.
pub fn string_search(input: &str, pattern: &Value) -> Option<Value> {
    let (source, flags) = parts(pattern)?;
    let regex = build_regex(&source, &flags)?;
    Some(Value::Number(
        regex
            .find(input)
            .ok()
            .flatten()
            .map(|matched| input[..matched.start()].encode_utf16().count() as f64)
            .unwrap_or(-1.0),
    ))
}

/// `String.prototype.split` for a runtime RegExp object.
pub fn string_split(input: &str, pattern: &Value, limit: usize) -> Option<Value> {
    let (source, flags) = parts(pattern)?;
    if limit == 0 {
        return Some(Value::array(Vec::new()));
    }
    if source.is_empty() {
        return Some(Value::array(
            input
                .chars()
                .take(limit)
                .map(|character| Value::String(character.to_string()))
                .collect(),
        ));
    }
    let regex = build_regex(&source, &flags)?;
    let mut output = Vec::new();
    let mut previous_end = 0;
    for captures in regex.captures_iter(input).filter_map(Result::ok) {
        let full = captures
            .get(0)
            .expect("captures always include the full match");
        output.push(Value::String(input[previous_end..full.start()].into()));
        if output.len() == limit {
            return Some(Value::array(output));
        }
        for index in 1..captures.len() {
            output.push(
                captures
                    .get(index)
                    .map(|capture| Value::String(capture.as_str().into()))
                    .unwrap_or(Value::Undefined),
            );
            if output.len() == limit {
                return Some(Value::array(output));
            }
        }
        previous_end = full.end();
    }
    if output.len() < limit {
        output.push(Value::String(input[previous_end..].into()));
    }
    Some(Value::array(output))
}

/// `String.prototype.matchAll` iterator for a runtime global RegExp.
pub fn string_match_all(input: &str, pattern: &Value) -> Option<Value> {
    let (source, flags) = parts(pattern)?;
    if !flags.contains('g') {
        crate::throw_value(type_error(
            "String.prototype.matchAll requires a global regular expression",
        ));
    }
    let regex = build_regex(&source, &flags)?;
    let mut matches = Vec::new();
    let mut start_utf16 = pattern.get_property("lastIndex").to_number().max(0.0) as usize;
    loop {
        let Some(start_byte) = utf16_offset_to_byte(input, start_utf16) else {
            break;
        };
        let Some(captures) = regex
            .captures_from_pos(input, start_byte)
            .ok()
            .flatten()
            .filter(|captures| {
                !flags.contains('y')
                    || captures
                        .get(0)
                        .is_some_and(|matched| matched.start() == start_byte)
            })
        else {
            break;
        };
        let full = captures
            .get(0)
            .expect("captures always include the full match");
        matches.push(captures_value(
            &regex,
            input,
            &captures,
            flags.contains('d'),
        ));
        let end_utf16 = input[..full.end()].encode_utf16().count();
        start_utf16 = if full.start() == full.end() {
            end_utf16
                + input[full.end()..]
                    .chars()
                    .next()
                    .map(char::len_utf16)
                    .unwrap_or(1)
        } else {
            end_utf16
        };
    }
    Some(match_iterator(matches))
}

fn match_iterator(matches: Vec<Value>) -> Value {
    let matches = Rc::new(matches);
    let index = Rc::new(RefCell::new(0usize));
    let next_matches = Rc::clone(&matches);
    let next_index = Rc::clone(&index);
    let snapshot_matches = Rc::clone(&matches);
    Value::object(HashMap::from([
        (
            "next".into(),
            Value::function(move |_, _| {
                let mut index = next_index.borrow_mut();
                let (value, done) = if let Some(value) = next_matches.get(*index).cloned() {
                    *index += 1;
                    (value, false)
                } else {
                    (Value::Undefined, true)
                };
                Value::object(HashMap::from([
                    ("value".into(), value),
                    ("done".into(), Value::Bool(done)),
                ]))
            }),
        ),
        (
            "__w3cosMapValuesSnapshot".into(),
            Value::function(move |_, _| Value::array(snapshot_matches.as_ref().clone())),
        ),
    ]))
}

pub fn string_replace(input: &str, pattern: &Value, replacement: &Value) -> Option<Value> {
    let (source, flags) = parts(pattern)?;
    let regex = build_regex(&source, &flags)?;
    let mut output = String::with_capacity(input.len());
    let mut previous_end = 0;
    let mut matched = false;
    for captures in regex.captures_iter(input).filter_map(Result::ok) {
        let full = captures
            .get(0)
            .expect("captures always include the full match");
        output.push_str(&input[previous_end..full.start()]);
        if replacement.is_function() {
            let mut arguments = (0..captures.len())
                .map(|index| {
                    captures
                        .get(index)
                        .map(|capture| Value::String(capture.as_str().into()))
                        .unwrap_or(Value::Undefined)
                })
                .collect::<Vec<_>>();
            arguments.push(Value::Number(
                input[..full.start()].encode_utf16().count() as f64
            ));
            arguments.push(Value::String(input.into()));
            let groups = capture_groups(&regex, &captures);
            if !groups.is_undefined() {
                arguments.push(groups);
            }
            output.push_str(&replacement.call(Value::Undefined, arguments).to_js_string());
        } else {
            output.push_str(&expand_replacement(
                &replacement.to_js_string(),
                input,
                &captures,
            ));
        }
        previous_end = full.end();
        matched = true;
        if !flags.contains('g') {
            break;
        }
    }
    if !matched {
        return Some(Value::String(input.into()));
    }
    output.push_str(&input[previous_end..]);
    Some(Value::String(output))
}

fn capture_groups(regex: &Regex, captures: &fancy_regex::Captures<'_>) -> Value {
    let groups = regex
        .capture_names()
        .flatten()
        .map(|name| {
            (
                name.to_string(),
                captures
                    .name(name)
                    .map(|matched| Value::String(matched.as_str().into()))
                    .unwrap_or(Value::Undefined),
            )
        })
        .collect::<HashMap<_, _>>();
    if groups.is_empty() {
        Value::Undefined
    } else {
        Value::object(groups)
    }
}

fn capture_indices(regex: &Regex, input: &str, captures: &fancy_regex::Captures<'_>) -> Value {
    let range_value = |matched: fancy_regex::Match<'_>| {
        Value::array(vec![
            Value::Number(input[..matched.start()].encode_utf16().count() as f64),
            Value::Number(input[..matched.end()].encode_utf16().count() as f64),
        ])
    };
    let mut properties = (0..captures.len())
        .map(|index| {
            (
                index.to_string(),
                captures
                    .get(index)
                    .map(&range_value)
                    .unwrap_or(Value::Undefined),
            )
        })
        .collect::<HashMap<_, _>>();
    properties.insert("length".into(), Value::Number(captures.len() as f64));
    let groups = regex
        .capture_names()
        .flatten()
        .map(|name| {
            (
                name.to_string(),
                captures
                    .name(name)
                    .map(&range_value)
                    .unwrap_or(Value::Undefined),
            )
        })
        .collect::<HashMap<_, _>>();
    properties.insert(
        "groups".into(),
        if groups.is_empty() {
            Value::Undefined
        } else {
            Value::object(groups)
        },
    );
    Value::object(properties)
}

fn expand_replacement(
    replacement: &str,
    input: &str,
    captures: &fancy_regex::Captures<'_>,
) -> String {
    let full = captures
        .get(0)
        .expect("captures always include the full match");
    let bytes = replacement.as_bytes();
    let mut output = String::with_capacity(replacement.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' || index + 1 >= bytes.len() {
            let ch = replacement[index..]
                .chars()
                .next()
                .expect("index is at a character boundary");
            output.push(ch);
            index += ch.len_utf8();
            continue;
        }
        match bytes[index + 1] {
            b'$' => {
                output.push('$');
                index += 2;
            }
            b'&' => {
                output.push_str(full.as_str());
                index += 2;
            }
            b'`' => {
                output.push_str(&input[..full.start()]);
                index += 2;
            }
            b'\'' => {
                output.push_str(&input[full.end()..]);
                index += 2;
            }
            b'<' => {
                let Some(relative_end) = replacement[index + 2..].find('>') else {
                    output.push('$');
                    index += 1;
                    continue;
                };
                let end = index + 2 + relative_end;
                let name = &replacement[index + 2..end];
                if let Some(capture) = captures.name(name) {
                    output.push_str(capture.as_str());
                }
                index = end + 1;
            }
            digit @ b'1'..=b'9' => {
                let first = usize::from(digit - b'0');
                let second = bytes
                    .get(index + 2)
                    .filter(|next| next.is_ascii_digit())
                    .map(|next| usize::from(*next - b'0'));
                let (capture_index, consumed) = second
                    .map(|second| (first * 10 + second, 3))
                    .filter(|(candidate, _)| *candidate < captures.len())
                    .unwrap_or((first, 2));
                if capture_index < captures.len() {
                    if let Some(capture) = captures.get(capture_index) {
                        output.push_str(capture.as_str());
                    }
                    index += consumed;
                } else {
                    output.push('$');
                    index += 1;
                }
            }
            _ => {
                output.push('$');
                index += 1;
            }
        }
    }
    output
}

fn translate_unicode_sets(source: &str, enabled: bool) -> String {
    if !enabled || !source.contains(r"\q{") {
        return source.to_string();
    }
    let mut output = String::with_capacity(source.len());
    let mut remaining = source;
    while let Some(start) = remaining.find(r"[\q{") {
        output.push_str(&remaining[..start]);
        let contents = &remaining[start + 4..];
        let Some(end) = contents.find("}]") else {
            output.push_str(&remaining[start..]);
            remaining = "";
            break;
        };
        let alternatives = contents[..end]
            .split('|')
            .map(regex::escape)
            .collect::<Vec<_>>();
        output.push_str("(?:");
        output.push_str(&alternatives.join("|"));
        output.push(')');
        remaining = &contents[end + 2..];
    }
    output.push_str(remaining);
    output
}

fn build_regex(source: &str, flags: &str) -> Option<Regex> {
    let source = translate_unicode_sets(source, flags.contains('v'));
    let modifiers = ['i', 'm', 's']
        .into_iter()
        .filter(|flag| flags.contains(*flag))
        .collect::<String>();
    let pattern = if modifiers.is_empty() {
        source
    } else {
        format!("(?{modifiers}:{source})")
    };
    Regex::new(&pattern).ok()
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
    let captures = regex
        .captures_from_pos(input, start_byte)
        .ok()
        .flatten()
        .filter(|captures| {
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
    captures_value(&regex, input, &captures, flags.contains('d'))
}

fn captures_value(
    regex: &Regex,
    input: &str,
    captures: &fancy_regex::Captures<'_>,
    has_indices: bool,
) -> Value {
    let full_match = captures
        .get(0)
        .expect("captures always include the full match");
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
    properties.insert("groups".into(), capture_groups(regex, captures));
    if has_indices {
        properties.insert("indices".into(), capture_indices(regex, input, captures));
    }
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
            string_replace("a.b.c", &create(r"\.", "g"), &Value::from(" "))
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

    #[test]
    fn constructor_metadata_named_groups_and_search_follow_browser_shape() {
        let original = create(r"(?<word>[a-z]+)", "gi");
        assert_eq!(
            original.get_property("source").to_js_string(),
            r"(?<word>[a-z]+)"
        );
        assert_eq!(original.get_property("flags").to_js_string(), "gi");
        assert!(original.get_property("global").to_bool());
        assert!(original.get_property("ignoreCase").to_bool());
        assert_eq!(
            original.call_method("toString", vec![]).to_js_string(),
            r"/(?<word>[a-z]+)/gi"
        );
        let matched = original.call_method("exec", vec![Value::from("12Ab")]);
        assert_eq!(
            matched
                .get_property("groups")
                .get_property("word")
                .to_js_string(),
            "Ab"
        );
        assert_eq!(
            string_search("😀 Ab", &create("Ab", ""))
                .unwrap()
                .to_number(),
            3.0
        );

        let copied = crate::class::construct(&regexp_class(), vec![original, Value::from("m")]);
        assert_eq!(
            copied.get_property("source").to_js_string(),
            r"(?<word>[a-z]+)"
        );
        assert_eq!(copied.get_property("flags").to_js_string(), "m");
        assert_eq!(create("", "").get_property("source").to_js_string(), "(?:)");
    }

    #[test]
    fn invalid_pattern_and_duplicate_flags_raise_syntax_error() {
        for (source, flags) in [("[", ""), ("x", "gg"), ("x", "q")] {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| create(source, flags)));
            let payload = result.expect_err("invalid regular expression must throw");
            let error = payload
                .downcast_ref::<crate::PanicValue>()
                .expect("regexp uses the JavaScript exception channel");
            assert_eq!(error.0.get_property("name").to_js_string(), "SyntaxError");
        }
    }

    #[test]
    fn replacement_supports_callbacks_and_javascript_substitution_tokens() {
        let callback = Value::function(|_, args| {
            Value::from(format!(
                "{}@{}",
                args.get(1).unwrap().to_js_string(),
                args.get(2).unwrap().to_number()
            ))
        });
        assert_eq!(
            string_replace("a1 b22", &create(r"([0-9]+)", "g"), &callback)
                .unwrap()
                .to_js_string(),
            "a1@1 b22@4"
        );
        assert_eq!(
            string_replace(
                "abc123xyz",
                &create(r"(?<digits>[0-9]+)", ""),
                &Value::from("[$$][$&][$`][$'][$<digits>][$1]")
            )
            .unwrap()
            .to_js_string(),
            "abc[$][123][abc][xyz][123][123]xyz"
        );
    }

    #[test]
    fn regexp_split_includes_captures_and_honors_limit() {
        let result = string_split("a,b;c", &create(r"([,;])", ""), 4).unwrap();
        assert_eq!(
            result
                .iter()
                .map(|value| value.to_js_string())
                .collect::<Vec<_>>(),
            ["a", ",", "b", ";"]
        );
        assert_eq!(
            string_split("😀x", &create("", ""), usize::MAX)
                .unwrap()
                .iter()
                .map(|value| value.to_js_string())
                .collect::<Vec<_>>(),
            ["😀", "x"]
        );
    }

    #[test]
    fn match_all_returns_iterable_matches_without_mutating_original_last_index() {
        let pattern = create(r"(?<digit>[0-9])", "g");
        pattern.set_property("lastIndex", Value::Number(1.0));
        let iterator = string_match_all("a1b2", &pattern).unwrap();
        let values = iterator.iter().collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].get_property("index").to_number(), 1.0);
        assert_eq!(
            values[1]
                .get_property("groups")
                .get_property("digit")
                .to_js_string(),
            "2"
        );
        assert_eq!(pattern.get_property("lastIndex").to_number(), 1.0);

        let first = iterator.call_method("next", vec![]);
        assert!(!first.get_property("done").to_bool());
        assert_eq!(
            first.get_property("value").get_property("0").to_js_string(),
            "1"
        );
    }

    #[test]
    fn match_all_rejects_non_global_regexp() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            string_match_all("a", &create("a", ""))
        }));
        let payload = result.expect_err("non-global matchAll must throw");
        let error = payload
            .downcast_ref::<crate::PanicValue>()
            .expect("matchAll uses the JavaScript exception channel");
        assert_eq!(error.0.get_property("name").to_js_string(), "TypeError");
    }

    #[test]
    fn indices_flag_reports_utf16_ranges_and_canonical_flag_order() {
        let pattern = create(r"(?<face>😀)(x)?", "igd");
        assert_eq!(pattern.get_property("flags").to_js_string(), "dgi");
        assert!(pattern.get_property("hasIndices").to_bool());
        let matched = pattern.call_method("exec", vec![Value::from("a😀x")]);
        let indices = matched.get_property("indices");
        assert_eq!(indices.get_property("0").get_property("0").to_number(), 1.0);
        assert_eq!(indices.get_property("0").get_property("1").to_number(), 4.0);
        assert_eq!(
            indices
                .get_property("groups")
                .get_property("face")
                .get_property("1")
                .to_number(),
            3.0
        );
    }

    #[test]
    fn lookaround_and_backreferences_follow_javascript_matching() {
        let lookahead = create(r"\w+(?=:)", "");
        assert_eq!(
            lookahead
                .call_method("exec", vec![Value::from("key:value")])
                .get_property("0")
                .to_js_string(),
            "key"
        );
        let lookbehind = create(r"(?<=\$)\d+", "");
        assert_eq!(
            lookbehind
                .call_method("exec", vec![Value::from("$42")])
                .get_property("0")
                .to_js_string(),
            "42"
        );
        assert!(
            create(r"^(?<word>\w+)\s+\k<word>$", "")
                .call_method("test", vec![Value::from("same same")])
                .to_bool()
        );
        assert!(
            create(r"^(\w+)\s+\1$", "")
                .call_method("test", vec![Value::from("same other")])
                .strict_eq(&Value::Bool(false))
        );
    }

    #[test]
    fn unicode_sets_flag_supports_set_operations_and_string_alternatives() {
        let letters = create(r"[\p{ASCII}&&\p{Letter}]+", "v");
        assert!(letters.get_property("unicodeSets").to_bool());
        assert!(!letters.get_property("unicode").to_bool());
        assert_eq!(
            letters
                .call_method("exec", vec![Value::from("éabc")])
                .get_property("0")
                .to_js_string(),
            "abc"
        );
        assert!(
            create(r"[\q{ab|cd}]", "v")
                .call_method("test", vec![Value::from("cd")])
                .to_bool()
        );
    }

    #[test]
    fn unicode_and_unicode_sets_flags_are_mutually_exclusive() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| create("x", "uv")));
        let payload = result.expect_err("u and v together must throw");
        let error = payload
            .downcast_ref::<crate::PanicValue>()
            .expect("regexp uses the JavaScript exception channel");
        assert_eq!(error.0.get_property("name").to_js_string(), "SyntaxError");
    }
}
