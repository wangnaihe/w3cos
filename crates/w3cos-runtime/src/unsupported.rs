//! Explicit browser-surface errors for APIs unavailable in the native host.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

pub const UNSUPPORTED_CONSTRUCTORS: &[&str] = &[];

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
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
    w3cos_core::web::dom_exception_class()
}
