//! Compact DOM-backed XPath facade.
//!
//! The evaluator implements the location paths most commonly used by browser
//! libraries (`//tag`, `.//tag`, attribute predicates, absolute element
//! paths) plus `count()`, `string()` and `boolean()`. Unsupported XPath 1.0
//! grammar returns an empty compatible result with a one-time warning.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static EVALUATOR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static EXPRESSION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static RESULT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

const RESULT_CONSTANTS: &[(&str, u32)] = &[
    ("ANY_TYPE", 0),
    ("NUMBER_TYPE", 1),
    ("STRING_TYPE", 2),
    ("BOOLEAN_TYPE", 3),
    ("UNORDERED_NODE_ITERATOR_TYPE", 4),
    ("ORDERED_NODE_ITERATOR_TYPE", 5),
    ("UNORDERED_NODE_SNAPSHOT_TYPE", 6),
    ("ORDERED_NODE_SNAPSHOT_TYPE", 7),
    ("ANY_UNORDERED_NODE_TYPE", 8),
    ("FIRST_ORDERED_NODE_TYPE", 9),
];

fn class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
    constructible: bool,
    members: &'static [&'static str],
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = if constructible {
            realm_function(generation, |this, _| {
                w3cos_core::class::set_prototype_of(
                    &this,
                    &xpath_evaluator_class().get_property("prototype"),
                );
                Value::Undefined
            })
        } else {
            realm_function(generation, move |_, _| {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string(&format!("Illegal constructor: {name}"))],
                ))
            })
        };
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in members {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn xpath_evaluator_class() -> Value {
    let generation = crate::jsdom::realm_generation();
    let class = class(
        &EVALUATOR_CLASS,
        "XPathEvaluator",
        true,
        &["createExpression", "createNSResolver", "evaluate"],
    );
    let prototype = class.get_property("prototype");
    prototype.set_property(
        "createExpression",
        realm_function(generation, |_, args| {
            expression_value(
                args.first().map(Value::to_js_string).unwrap_or_default(),
                args.get(1).cloned().unwrap_or(Value::Null),
            )
        }),
    );
    prototype.set_property(
        "createNSResolver",
        realm_function(generation, |_, args| {
            namespace_resolver(args.first().cloned().unwrap_or_default())
        }),
    );
    prototype.set_property(
        "evaluate",
        realm_function(generation, |_, args| evaluate_arguments(&args)),
    );
    class
}

pub fn xpath_expression_class() -> Value {
    class(&EXPRESSION_CLASS, "XPathExpression", false, &["evaluate"])
}

pub fn xpath_result_class() -> Value {
    let class = class(
        &RESULT_CLASS,
        "XPathResult",
        false,
        &[
            "ANY_TYPE",
            "ANY_UNORDERED_NODE_TYPE",
            "BOOLEAN_TYPE",
            "FIRST_ORDERED_NODE_TYPE",
            "NUMBER_TYPE",
            "ORDERED_NODE_ITERATOR_TYPE",
            "ORDERED_NODE_SNAPSHOT_TYPE",
            "STRING_TYPE",
            "UNORDERED_NODE_ITERATOR_TYPE",
            "UNORDERED_NODE_SNAPSHOT_TYPE",
            "booleanValue",
            "invalidIteratorState",
            "iterateNext",
            "numberValue",
            "resultType",
            "singleNodeValue",
            "snapshotItem",
            "snapshotLength",
            "stringValue",
        ],
    );
    let prototype = class.get_property("prototype");
    for (name, value) in RESULT_CONSTANTS {
        class.set_property(name, Value::Number(*value as f64));
        prototype.set_property(name, Value::Number(*value as f64));
    }
    class
}

fn namespace_resolver(node: Value) -> Value {
    let generation = crate::jsdom::realm_generation();
    realm_function(generation, move |_, args| {
        let prefix = args.first().map(Value::to_js_string).unwrap_or_default();
        if prefix == "xml" {
            return Value::string("http://www.w3.org/XML/1998/namespace");
        }
        let resolved = node.call_method("lookupNamespaceURI", vec![Value::string(&prefix)]);
        if resolved.is_undefined() {
            Value::Null
        } else {
            resolved
        }
    })
}

fn expression_value(expression: String, resolver: Value) -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::from([
        ("__w3cos_expression".into(), Value::string(&expression)),
        ("__w3cos_resolver".into(), resolver),
    ]));
    value.set_property(
        "evaluate",
        realm_function(generation, move |_, args| {
            let mut forwarded = vec![
                Value::string(&expression),
                args.first()
                    .cloned()
                    .unwrap_or_else(crate::jsdom::document_value),
                Value::Null,
                args.get(1).cloned().unwrap_or(Value::Number(0.0)),
                args.get(2).cloned().unwrap_or(Value::Null),
            ];
            evaluate_arguments(&std::mem::take(&mut forwarded))
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &xpath_expression_class().get_property("prototype"),
    );
    value
}

fn selector_for_location_path(expression: &str) -> Option<String> {
    let path = expression
        .strip_prefix(".//")
        .or_else(|| expression.strip_prefix("//"))?;
    if path.is_empty() || path.contains('/') || path.contains("::") {
        return None;
    }
    let (tag, predicate) = path
        .split_once('[')
        .map(|(tag, rest)| (tag, Some(rest.trim_end_matches(']'))))
        .unwrap_or((path, None));
    let tag = if tag == "*" { "*" } else { tag };
    let Some(predicate) = predicate else {
        return Some(tag.to_string());
    };
    let attribute = predicate.strip_prefix('@')?;
    if let Some((name, value)) = attribute.split_once('=') {
        let value = value.trim().trim_matches(['\'', '"']);
        if name.trim() == "id" {
            return Some(format!("#{value}"));
        }
        if name.trim() == "class" {
            return Some(format!(".{value}"));
        }
        Some(format!(
            "{tag}[{}=\"{}\"]",
            name.trim(),
            value.replace('"', "\\\"")
        ))
    } else {
        Some(format!("{tag}[{}]", attribute.trim()))
    }
}

fn absolute_path_node(expression: &str) -> Option<Value> {
    if !expression.starts_with('/') || expression.starts_with("//") {
        return None;
    }
    let mut current = crate::jsdom::document_value().get_property("documentElement");
    let mut segments = expression.split('/').filter(|segment| !segment.is_empty());
    let first = segments.next()?;
    if current
        .get_property("tagName")
        .to_js_string()
        .to_ascii_lowercase()
        != first.to_ascii_lowercase()
    {
        return None;
    }
    for segment in segments {
        let selector = segment.split('[').next().unwrap_or(segment);
        current = current.call_method("querySelector", vec![Value::string(selector)]);
        if current.is_nullish() {
            return None;
        }
    }
    Some(current)
}

fn query_nodes(expression: &str, context: &Value) -> Option<Vec<Value>> {
    if let Some(node) = absolute_path_node(expression) {
        return Some(vec![node]);
    }
    let selector = selector_for_location_path(expression)?;
    let root = if context.is_nullish() {
        crate::jsdom::document_value()
    } else {
        context.clone()
    };
    Some(
        root.call_method("querySelectorAll", vec![Value::string(&selector)])
            .iter()
            .collect(),
    )
}

enum Evaluation {
    Nodes(Vec<Value>),
    Number(f64),
    String(String),
    Boolean(bool),
}

fn evaluate_expression(expression: &str, context: &Value) -> Evaluation {
    let expression = expression.trim();
    for (name, wrap) in [
        ("count(", "number"),
        ("string(", "string"),
        ("boolean(", "boolean"),
    ] {
        if let Some(inner) = expression
            .strip_prefix(name)
            .and_then(|value| value.strip_suffix(')'))
        {
            let nodes = query_nodes(inner.trim(), context).unwrap_or_default();
            return match wrap {
                "number" => Evaluation::Number(nodes.len() as f64),
                "string" => Evaluation::String(
                    nodes
                        .first()
                        .map(|node| node.get_property("textContent").to_js_string())
                        .unwrap_or_default(),
                ),
                _ => Evaluation::Boolean(!nodes.is_empty()),
            };
        }
    }
    if let Some(nodes) = query_nodes(expression, context) {
        Evaluation::Nodes(nodes)
    } else {
        static WARNING: Once = Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: XPathEvaluator supports common DOM location paths and scalar \
                 wrappers; unsupported XPath 1.0 grammar returns an empty compatible result"
            );
        });
        Evaluation::Nodes(Vec::new())
    }
}

fn result_value(evaluation: Evaluation, requested_type: u32) -> Value {
    let generation = crate::jsdom::realm_generation();
    let (result_type, nodes, number, string, boolean) = match evaluation {
        Evaluation::Nodes(nodes) => {
            let kind = if requested_type == 0 {
                4
            } else {
                requested_type
            };
            (kind, nodes, 0.0, String::new(), false)
        }
        Evaluation::Number(number) => (1, Vec::new(), number, String::new(), number != 0.0),
        Evaluation::String(string) => (2, Vec::new(), 0.0, string, false),
        Evaluation::Boolean(boolean) => (3, Vec::new(), 0.0, String::new(), boolean),
    };
    let nodes = Value::array(nodes);
    let first = nodes.get_property("0");
    let value = Value::object(HashMap::from([
        ("resultType".into(), Value::Number(result_type as f64)),
        ("numberValue".into(), Value::Number(number)),
        ("stringValue".into(), Value::string(&string)),
        ("booleanValue".into(), Value::Bool(boolean)),
        ("invalidIteratorState".into(), Value::Bool(false)),
        ("snapshotLength".into(), nodes.get_property("length")),
        (
            "singleNodeValue".into(),
            if first.is_undefined() {
                Value::Null
            } else {
                first
            },
        ),
        ("__w3cos_nodes".into(), nodes),
        ("__w3cos_index".into(), Value::Number(0.0)),
    ]));
    value.set_property(
        "iterateNext",
        realm_function(generation, |this, _| {
            let index = this.get_property("__w3cos_index").to_u32();
            let node = this
                .get_property("__w3cos_nodes")
                .get_property(&index.to_string());
            this.set_property("__w3cos_index", Value::Number((index + 1) as f64));
            if node.is_undefined() {
                Value::Null
            } else {
                node
            }
        }),
    );
    value.set_property(
        "snapshotItem",
        realm_function(generation, |this, args| {
            let node = this
                .get_property("__w3cos_nodes")
                .get_property(&args.first().map(Value::to_u32).unwrap_or(0).to_string());
            if node.is_undefined() {
                Value::Null
            } else {
                node
            }
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &xpath_result_class().get_property("prototype"));
    value
}

fn evaluate_arguments(args: &[Value]) -> Value {
    let expression = args.first().map(Value::to_js_string).unwrap_or_default();
    let context = args
        .get(1)
        .cloned()
        .unwrap_or_else(crate::jsdom::document_value);
    let requested_type = args.get(3).map(Value::to_u32).unwrap_or(0);
    result_value(evaluate_expression(&expression, &context), requested_type)
}

pub fn install_document(document: &Value) {
    let evaluator = w3cos_core::class::construct(&xpath_evaluator_class(), vec![]);
    for method in ["createExpression", "createNSResolver", "evaluate"] {
        document.set_property(method, evaluator.get_property(method));
        crate::dom_constructors::prototype("Document")
            .set_property(method, evaluator.get_property(method));
    }
}

pub fn reset_realm() {
    for slot in [&EVALUATOR_CLASS, &EXPRESSION_CLASS, &RESULT_CLASS] {
        slot.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluator_queries_dom_and_exposes_result_identity() {
        crate::jsdom::reset_bridge();
        crate::dom::reset_document();
        let document = crate::jsdom::document_value();
        install_document(&document);
        let div = document.call_method("createElement", vec![Value::string("div")]);
        div.call_method(
            "setAttribute",
            vec![Value::string("id"), Value::string("target")],
        );
        document
            .get_property("body")
            .call_method("appendChild", vec![div.clone()]);
        let result = document.call_method(
            "evaluate",
            vec![
                Value::string("//*[@id='target']"),
                document.clone(),
                Value::Null,
                Value::Number(7.0),
                Value::Null,
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &result,
            &xpath_result_class()
        ));
        assert_eq!(result.get_property("snapshotLength").to_number(), 1.0);
        assert!(result.call_method("snapshotItem", vec![Value::Number(0.0)]) == div);
        let count = document.call_method(
            "evaluate",
            vec![
                Value::string("count(//div)"),
                document.clone(),
                Value::Null,
                Value::Number(1.0),
            ],
        );
        assert_eq!(count.get_property("numberValue").to_number(), 1.0);
    }

    #[test]
    fn evaluator_expressions_results_and_resolvers_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let old_evaluator_class = xpath_evaluator_class();
        let old_expression_class = xpath_expression_class();
        let old_result_class = xpath_result_class();
        let document = crate::jsdom::document_value();
        let evaluator = w3cos_core::class::construct(&old_evaluator_class, vec![]);
        let expression = evaluator.call_method(
            "createExpression",
            vec![Value::string("//body"), Value::Null],
        );
        let result = expression.call_method(
            "evaluate",
            vec![document.clone(), Value::Number(7.0), Value::Null],
        );
        let resolver = evaluator.call_method("createNSResolver", vec![document]);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        assert!(!old_evaluator_class.strict_eq(&xpath_evaluator_class()));
        assert!(!old_expression_class.strict_eq(&xpath_expression_class()));
        assert!(!old_result_class.strict_eq(&xpath_result_class()));
        assert!(
            old_evaluator_class
                .call(Value::Undefined, vec![])
                .is_undefined()
        );
        assert!(expression.call_method("evaluate", vec![]).is_undefined());
        assert!(result.call_method("snapshotItem", vec![]).is_undefined());
        assert!(resolver.call(Value::Undefined, vec![]).is_undefined());
        assert!(
            w3cos_core::class::construct(&xpath_evaluator_class(), vec![])
                .call_method(
                    "evaluate",
                    vec![
                        Value::string("count(//body)"),
                        crate::jsdom::document_value(),
                        Value::Null,
                        Value::Number(1.0),
                    ],
                )
                .is_object()
        );
        reset_realm();
    }
}
