//! Trusted Types API compatibility layer for inert AOT sinks.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static FACTORY: RefCell<Option<Value>> = const { RefCell::new(None) };
    static POLICY_NAMES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static DEFAULT_POLICY: RefCell<Value> = const { RefCell::new(Value::Null) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn exception(message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string("TypeError")),
        ("message".into(), Value::string(message)),
    ]))
}

pub fn trusted_class(name: &str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class_name = name.to_string();
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(exception(&format!("Illegal constructor: {class_name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        let members: &[&str] = match name {
            "TrustedHTML" | "TrustedScript" | "TrustedScriptURL" => &["toJSON", "toString"],
            "TrustedTypePolicy" => &["createHTML", "createScript", "createScriptURL", "name"],
            "TrustedTypePolicyFactory" => &[
                "createPolicy",
                "defaultPolicy",
                "emptyHTML",
                "emptyScript",
                "getAttributeType",
                "getPropertyType",
                "getTypeMapping",
                "isHTML",
                "isScript",
                "isScriptURL",
            ],
            _ => &[],
        };
        for member in members {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

fn trusted_value(kind: &str, value: String) -> Value {
    let result = Value::object(HashMap::new());
    let string_value = value.clone();
    result.set_property(
        "toString",
        Value::function(move |_, _| Value::string(&string_value)),
    );
    result.set_property("toJSON", Value::function(move |_, _| Value::string(&value)));
    result.set_property("__w3cos_trusted_type", Value::string(kind));
    w3cos_core::class::set_prototype_of(&result, &trusted_class(kind).get_property("prototype"));
    result
}

fn policy_method(kind: &'static str, callback: Value) -> Value {
    Value::function(move |_, args| {
        if !callback.is_function() {
            w3cos_core::throw_value(exception(&format!(
                "TrustedTypePolicy does not define create{}",
                kind.trim_start_matches("Trusted")
            )));
        }
        let output = callback.call(Value::Undefined, args).to_js_string();
        trusted_value(kind, output)
    })
}

fn policy_value(name: &str, rules: Value) -> Value {
    let policy = Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        (
            "createHTML".into(),
            policy_method("TrustedHTML", rules.get_property("createHTML")),
        ),
        (
            "createScript".into(),
            policy_method("TrustedScript", rules.get_property("createScript")),
        ),
        (
            "createScriptURL".into(),
            policy_method("TrustedScriptURL", rules.get_property("createScriptURL")),
        ),
    ]));
    w3cos_core::class::set_prototype_of(
        &policy,
        &trusted_class("TrustedTypePolicy").get_property("prototype"),
    );
    policy
}

pub fn factory_value() -> Value {
    FACTORY.with(|slot| {
        if let Some(factory) = slot.borrow().clone() {
            return factory;
        }
        WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!(
                    "[w3cos] warning: Trusted Types brands and policies are enforced at \
                     compatible sinks; CSP policy-name directives and script execution remain pending"
                );
            }
        });
        let factory = Value::object(HashMap::new());
        factory.set_property(
            "createPolicy",
            Value::function(|_, args| {
                let name = args
                    .first()
                    .cloned()
                    .unwrap_or(Value::Undefined)
                    .to_js_string();
                let rules = args.get(1).cloned().unwrap_or(Value::Undefined);
                if name.is_empty() {
                    w3cos_core::throw_value(exception(
                        "TrustedTypePolicy name must not be empty",
                    ));
                }
                let duplicate = POLICY_NAMES.with(|names| !names.borrow_mut().insert(name.clone()));
                if duplicate {
                    w3cos_core::throw_value(exception(
                        "a TrustedTypePolicy with this name already exists",
                    ));
                }
                let policy = policy_value(&name, rules);
                if name == "default" {
                    DEFAULT_POLICY.with(|default| *default.borrow_mut() = policy.clone());
                }
                policy
            }),
        );
        factory.set_property(
            "__w3cos_getter_defaultPolicy",
            Value::function(|_, _| DEFAULT_POLICY.with(|default| default.borrow().clone())),
        );
        for (method, kind) in [
            ("isHTML", "TrustedHTML"),
            ("isScript", "TrustedScript"),
            ("isScriptURL", "TrustedScriptURL"),
        ] {
            factory.set_property(
                method,
                Value::function(move |_, args| {
                    Value::Bool(
                        args.first()
                            .is_some_and(|value| value.get_property("__w3cos_trusted_type") == Value::string(kind)),
                    )
                }),
            );
        }
        factory.set_property("emptyHTML", trusted_value("TrustedHTML", String::new()));
        factory.set_property(
            "emptyScript",
            trusted_value("TrustedScript", String::new()),
        );
        factory.set_property(
            "getAttributeType",
            Value::function(|_, args| {
                let tag = args.first().map(Value::to_js_string).unwrap_or_default();
                let attribute = args.get(1).map(Value::to_js_string).unwrap_or_default();
                let kind = match (
                    tag.to_ascii_lowercase().as_str(),
                    attribute.to_ascii_lowercase().as_str(),
                ) {
                    ("script", "src") => Some("TrustedScriptURL"),
                    ("iframe", "srcdoc") => Some("TrustedHTML"),
                    _ => None,
                };
                kind.map(Value::string).unwrap_or(Value::Null)
            }),
        );
        factory.set_property(
            "getPropertyType",
            Value::function(|_, args| {
                let tag = args.first().map(Value::to_js_string).unwrap_or_default();
                let property = args.get(1).map(Value::to_js_string).unwrap_or_default();
                let kind = match (
                    tag.to_ascii_lowercase().as_str(),
                    property.to_ascii_lowercase().as_str(),
                ) {
                    (_, "innerhtml" | "outerhtml") => Some("TrustedHTML"),
                    ("script", "src") => Some("TrustedScriptURL"),
                    ("script", "text" | "textcontent" | "innertext") => Some("TrustedScript"),
                    _ => None,
                };
                kind.map(Value::string).unwrap_or(Value::Null)
            }),
        );
        w3cos_core::class::set_prototype_of(
            &factory,
            &trusted_class("TrustedTypePolicyFactory").get_property("prototype"),
        );
        *slot.borrow_mut() = Some(factory.clone());
        factory
    })
}

pub fn reset() {
    POLICY_NAMES.with(|names| names.borrow_mut().clear());
    DEFAULT_POLICY.with(|default| *default.borrow_mut() = Value::Null);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_brand_callback_results_and_factory_introspects_sinks() {
        reset();
        let factory = factory_value();
        let policy = factory.call_method(
            "createPolicy",
            vec![
                Value::string("default"),
                Value::object(HashMap::from([
                    (
                        "createHTML".into(),
                        Value::function(|_, args| {
                            Value::string(&args[0].to_js_string().replace("<script>", ""))
                        }),
                    ),
                    (
                        "createScriptURL".into(),
                        Value::function(|_, args| args[0].clone()),
                    ),
                ])),
            ],
        );
        let html = policy.call_method("createHTML", vec![Value::string("<b>safe</b>")]);
        assert!(factory.call_method("isHTML", vec![html.clone()]).to_bool());
        assert!(w3cos_core::class::instance_of(
            &html,
            &trusted_class("TrustedHTML")
        ));
        assert_eq!(html.to_js_string(), "<b>safe</b>");
        assert_eq!(
            html.call_method("toString", vec![]),
            Value::string("<b>safe</b>")
        );
        assert_eq!(factory.get_property("defaultPolicy"), policy);
        assert_eq!(
            factory.call_method(
                "getPropertyType",
                vec![Value::string("div"), Value::string("innerHTML")]
            ),
            Value::string("TrustedHTML")
        );
    }
}
