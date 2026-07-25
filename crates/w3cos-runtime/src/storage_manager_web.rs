//! Storage Manager API compatibility surface.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static STORAGE_MANAGER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn error(name: &str, message: &str) -> Value {
    Value::object(HashMap::from([
        ("name".into(), Value::string(name)),
        ("message".into(), Value::string(message)),
    ]))
}

fn warn_storage_adapter() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: navigator.storage exposes compatibility results; accurate quota, \
             persistent-storage grants and OPFS require a platform storage adapter"
        );
    });
}

pub fn storage_manager_class() -> Value {
    STORAGE_MANAGER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: StorageManager"))
        });
        class.set_property("name", Value::string("StorageManager"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["estimate", "getDirectory", "persist", "persisted"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn storage_manager_value() -> Value {
    let manager = Value::object(HashMap::new());
    w3cos_core::class::set_prototype_of(
        &manager,
        &storage_manager_class().get_property("prototype"),
    );
    manager.set_property(
        "estimate",
        Value::function(|_, _| {
            warn_storage_adapter();
            w3cos_core::promise::resolve(vec![Value::object(HashMap::from([
                ("usage".into(), Value::Number(0.0)),
                ("quota".into(), Value::Number(0.0)),
                ("usageDetails".into(), Value::object(HashMap::new())),
            ]))])
        }),
    );
    manager.set_property(
        "persisted",
        Value::function(|_, _| {
            warn_storage_adapter();
            w3cos_core::promise::resolve(vec![Value::Bool(false)])
        }),
    );
    manager.set_property(
        "persist",
        Value::function(|_, _| {
            warn_storage_adapter();
            w3cos_core::promise::resolve(vec![Value::Bool(false)])
        }),
    );
    manager.set_property(
        "getDirectory",
        Value::function(|_, _| match crate::file_system_web::opfs_root_value() {
            Ok(root) => w3cos_core::promise::resolve(vec![root]),
            Err(error) => w3cos_core::promise::reject(vec![error]),
        }),
    );
    manager
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn estimates_and_persistence_use_explicit_compatibility_results() {
        let manager = storage_manager_value();
        assert!(w3cos_core::class::instance_of(
            &manager,
            &storage_manager_class()
        ));

        let values = Rc::new(RefCell::new(Vec::<String>::new()));
        for (method, property) in [
            ("estimate", "quota"),
            ("persisted", ""),
            ("persist", ""),
            ("getDirectory", "name"),
        ] {
            let values_for_handler = Rc::clone(&values);
            let property = property.to_string();
            let promise = manager.call_method(method, vec![]);
            let handler = Value::function(move |_, args| {
                let value = if property.is_empty() {
                    args[0].to_js_string()
                } else {
                    args[0].get_property(&property).to_js_string()
                };
                values_for_handler.borrow_mut().push(value);
                Value::Undefined
            });
            promise.call_method("then", vec![handler]);
        }
        w3cos_core::promise::drain_microtasks();
        assert_eq!(&*values.borrow(), &["0", "false", "false", "w3cos-opfs"]);
    }
}
