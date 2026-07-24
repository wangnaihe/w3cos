//! Explicit browser-surface errors for APIs unavailable in the native host.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

pub const UNSUPPORTED_CONSTRUCTORS: &[&str] = &[
    "eval",
    "escape",
    "unescape",
    "CSS",
    "DOMRect",
    "DOMPoint",
    "DOMMatrix",
    "Report",
    "Function",
    "ShadowRoot",
    "NodeList",
    "CSSStyleDeclaration",
    "DOMParser",
    "XMLSerializer",
    "CSSStyleSheet",
];

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static DOM_EXCEPTION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub fn error_value(api: &str) -> Value {
    Value::object(HashMap::from([
        ("name".to_string(), Value::string("NotSupportedError")),
        (
            "message".to_string(),
            Value::string(&format!("{api} is not supported by this w3cos target")),
        ),
        ("code".to_string(), Value::Number(9.0)),
        ("api".to_string(), Value::string(api)),
        ("supported".to_string(), Value::Bool(false)),
    ]))
}

pub fn unsupported_constructor(api: &str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(api).cloned() {
            return class;
        }
        let api_name = api.to_string();
        let class = Value::function(move |_, _| error_value(&api_name));
        class.set_property("name", Value::string(api));
        class.set_property("supported", Value::Bool(false));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(api.to_string(), class.clone());
        class
    })
}

pub fn dom_exception_class() -> Value {
    DOM_EXCEPTION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, args| {
            this.set_property(
                "message",
                args.first().cloned().unwrap_or_else(|| Value::string("")),
            );
            this.set_property(
                "name",
                args.get(1)
                    .cloned()
                    .unwrap_or_else(|| Value::string("Error")),
            );
            this.set_property("code", Value::Number(0.0));
            Value::Undefined
        });
        for (name, code) in [
            ("INDEX_SIZE_ERR", 1.0),
            ("HIERARCHY_REQUEST_ERR", 3.0),
            ("INVALID_CHARACTER_ERR", 5.0),
            ("NOT_SUPPORTED_ERR", 9.0),
            ("INVALID_STATE_ERR", 11.0),
            ("SYNTAX_ERR", 12.0),
            ("ABORT_ERR", 20.0),
            ("DATA_CLONE_ERR", 25.0),
        ] {
            class.set_property(name, Value::Number(code));
        }
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}
