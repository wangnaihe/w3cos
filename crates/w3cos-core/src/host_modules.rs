//! Framework-neutral host module registry for AOT ESM imports.
//!
//! The compiler emits canonical module/export paths such as
//! `react::useState`; embedders register implementations at startup. This
//! keeps package adapters out of compiler code generation and lets a future
//! upstream JavaScript module replace an adapter without changing the ABI.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::Value;

thread_local! {
    static EXPORTS: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

pub fn register(path: impl Into<String>, implementation: Value) {
    EXPORTS.with(|exports| exports.borrow_mut().insert(path.into(), implementation));
}

pub fn contains(path: &str) -> bool {
    EXPORTS.with(|exports| exports.borrow().contains_key(path))
}

pub fn call(path: &str, arguments: Vec<Value>) -> Value {
    if path == "w3cos/native::invoke" {
        return crate::host::invoke(arguments);
    }
    EXPORTS
        .with(|exports| exports.borrow().get(path).cloned())
        .filter(Value::is_function)
        .map(|implementation| implementation.call(Value::Undefined, arguments))
        .unwrap_or(Value::Undefined)
}

pub fn clear() {
    EXPORTS.with(|exports| exports.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_module_export_is_called_by_canonical_path() {
        clear();
        register(
            "demo::sum",
            Value::function(|_, arguments| {
                Value::Number(arguments.iter().map(Value::to_number).sum())
            }),
        );

        assert!(contains("demo::sum"));
        assert_eq!(
            call("demo::sum", vec![Value::Number(2.0), Value::Number(3.0)]).to_number(),
            5.0
        );
        clear();
    }
}
