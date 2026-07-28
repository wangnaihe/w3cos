//! Framework-neutral host module registry for AOT ESM imports.
//!
//! The compiler emits canonical module/export paths such as
//! package-defined functions; embedders register implementations at startup. This
//! keeps package adapters out of compiler code generation and lets a future
//! upstream JavaScript module replace an adapter without changing the ABI.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::Value;

pub const DYNAMIC_IMPORT_PATH: &str = "w3cos/module::dynamicImport";

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

/// Invoke the embedder's AOT module loader.
///
/// The adapter receives `(specifier, referrer)` and must return a Promise (or
/// a value that the embedder intentionally exposes as its result). Keeping
/// this hook in Core lets ordinary AOT artifacts support `import()` without
/// linking the browser loader, compiler, W3IR, or W3VM.
pub fn dynamic_import(specifier: Value, referrer: Value) -> Value {
    let implementation = EXPORTS.with(|exports| exports.borrow().get(DYNAMIC_IMPORT_PATH).cloned());
    match implementation {
        Some(implementation) if implementation.is_callable() => {
            implementation.call(Value::Undefined, vec![specifier, referrer])
        }
        _ => crate::promise::reject(vec![Value::string(
            "TypeError: dynamic import requires an AOT module-loader adapter",
        )]),
    }
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

    #[test]
    fn dynamic_import_uses_the_optional_core_host_adapter() {
        clear();
        let missing = dynamic_import(
            Value::string("./missing.js"),
            Value::string("app:///entry.js"),
        );
        assert!(matches!(
            crate::promise::status(&missing),
            Some(crate::promise::PromiseStatus::Rejected(_))
        ));

        register(
            DYNAMIC_IMPORT_PATH,
            Value::function(|_, arguments| {
                crate::promise::resolve(vec![Value::from(format!(
                    "{}@{}",
                    arguments[0].to_js_string(),
                    arguments[1].to_js_string()
                ))])
            }),
        );
        let loaded = dynamic_import(
            Value::string("./feature.js"),
            Value::string("app:///entry.js"),
        );
        assert!(matches!(
            crate::promise::status(&loaded),
            Some(crate::promise::PromiseStatus::Fulfilled(value))
                if value.to_js_string() == "./feature.js@app:///entry.js"
        ));
        clear();
    }
}
