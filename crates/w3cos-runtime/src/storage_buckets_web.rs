//! Storage Buckets API compatibility surface.
//!
//! Bucket metadata and lifecycle are process-local. Existing CacheStorage and
//! IndexedDB facades are exposed through each bucket while durable isolation,
//! quota accounting and OPFS remain explicit host/runtime follow-up work.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Once;

use crate::jsdom::realm_function;
use w3cos_core::Value;

#[derive(Clone, Default)]
struct BucketState {
    expires: Option<f64>,
}

thread_local! {
    static BUCKETS: RefCell<BTreeMap<String, BucketState>> = const { RefCell::new(BTreeMap::new()) };
    static MANAGER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BUCKET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    )
}

fn validate_name(value: Value) -> Result<String, Value> {
    let name = value.to_js_string();
    let bytes = name.as_bytes();
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    if bytes.is_empty()
        || bytes.len() > 64
        || !alphanumeric(bytes[0])
        || !alphanumeric(bytes[bytes.len() - 1])
        || !bytes
            .iter()
            .all(|byte| alphanumeric(*byte) || matches!(*byte, b'-' | b'_'))
    {
        return Err(error(
            "TypeError",
            "Storage bucket names must be 1-64 lowercase ASCII letters, digits, hyphens or \
             underscores and start/end with a letter or digit",
        ));
    }
    Ok(name)
}

fn warn_process_local() {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: Storage Buckets metadata is process-local; durable bucket \
             isolation, quota enforcement and OPFS require runtime storage adapters"
        );
    });
}

fn mirror_prototype(value: &Value, class: &Value, names: &[&str]) {
    let prototype = class.get_property("prototype");
    for name in names {
        prototype.set_property(name, value.get_property(name));
    }
}

pub fn storage_bucket_class() -> Value {
    BUCKET_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| {
            w3cos_core::throw_value(error("TypeError", "Illegal constructor: StorageBucket"))
        });
        class.set_property("name", Value::string("StorageBucket"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "caches",
            "estimate",
            "expires",
            "getDirectory",
            "indexedDB",
            "name",
            "persist",
            "persisted",
            "setExpires",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn bucket_value(name: String) -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::from([
        ("name".into(), Value::string(&name)),
        ("indexedDB".into(), crate::indexed_db_web::factory_value()),
        ("caches".into(), crate::cache_web::cache_storage_value()),
    ]));
    let name_for_expires = name.clone();
    value.set_property(
        "__w3cos_getter_expires",
        realm_function(generation, move |_, _| {
            BUCKETS.with(|buckets| {
                buckets
                    .borrow()
                    .get(&name_for_expires)
                    .and_then(|state| state.expires)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            })
        }),
    );
    value.set_property(
        "persisted",
        realm_function(generation, |_, _| {
            w3cos_core::promise::resolve(vec![Value::Bool(false)])
        }),
    );
    value.set_property(
        "persist",
        realm_function(generation, |_, _| {
            warn_process_local();
            w3cos_core::promise::resolve(vec![Value::Bool(false)])
        }),
    );
    value.set_property(
        "estimate",
        realm_function(generation, |_, _| {
            w3cos_core::promise::resolve(vec![Value::object(HashMap::from([
                ("usage".into(), Value::Number(0.0)),
                ("quota".into(), Value::Number(0.0)),
            ]))])
        }),
    );
    let name_for_set = name;
    value.set_property(
        "setExpires",
        realm_function(generation, move |_, args| {
            let expires = args
                .first()
                .cloned()
                .unwrap_or(Value::Undefined)
                .to_number();
            if !expires.is_finite() || expires < 0.0 {
                return w3cos_core::promise::reject(vec![error(
                    "TypeError",
                    "StorageBucket.setExpires requires a non-negative finite timestamp",
                )]);
            }
            BUCKETS.with(|buckets| {
                if let Some(state) = buckets.borrow_mut().get_mut(&name_for_set) {
                    state.expires = Some(expires);
                }
            });
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    value.set_property(
        "getDirectory",
        realm_function(generation, |_, _| {
            w3cos_core::promise::reject(vec![error(
                "NotSupportedError",
                "Bucket-scoped OPFS requires a platform storage adapter",
            )])
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &storage_bucket_class().get_property("prototype"));
    mirror_prototype(
        &value,
        &storage_bucket_class(),
        &[
            "name",
            "indexedDB",
            "caches",
            "__w3cos_getter_expires",
            "persisted",
            "persist",
            "estimate",
            "setExpires",
            "getDirectory",
        ],
    );
    value
}

pub fn storage_bucket_manager_class() -> Value {
    MANAGER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| {
            w3cos_core::throw_value(error(
                "TypeError",
                "Illegal constructor: StorageBucketManager",
            ))
        });
        class.set_property("name", Value::string("StorageBucketManager"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn storage_bucket_manager_value() -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::new());
    value.set_property(
        "open",
        realm_function(generation, |_, args| {
            let name = match validate_name(args.first().cloned().unwrap_or(Value::Undefined)) {
                Ok(name) => name,
                Err(reason) => return w3cos_core::promise::reject(vec![reason]),
            };
            let options = args.get(1).cloned().unwrap_or(Value::Undefined);
            let expires = options.get_property("expires");
            if !expires.is_undefined() {
                let expires = expires.to_number();
                if !expires.is_finite() || expires < 0.0 {
                    return w3cos_core::promise::reject(vec![error(
                        "TypeError",
                        "StorageBucket.open expires must be a non-negative finite timestamp",
                    )]);
                }
                BUCKETS.with(|buckets| {
                    buckets
                        .borrow_mut()
                        .entry(name.clone())
                        .or_default()
                        .expires = Some(expires);
                });
            } else {
                BUCKETS.with(|buckets| {
                    buckets.borrow_mut().entry(name.clone()).or_default();
                });
            }
            warn_process_local();
            w3cos_core::promise::resolve(vec![bucket_value(name)])
        }),
    );
    value.set_property(
        "keys",
        realm_function(generation, |_, _| {
            let names = BUCKETS.with(|buckets| {
                buckets
                    .borrow()
                    .keys()
                    .map(|name| Value::string(name))
                    .collect::<Vec<_>>()
            });
            w3cos_core::promise::resolve(vec![Value::array(names)])
        }),
    );
    value.set_property(
        "delete",
        realm_function(generation, |_, args| {
            let name = match validate_name(args.first().cloned().unwrap_or(Value::Undefined)) {
                Ok(name) => name,
                Err(reason) => return w3cos_core::promise::reject(vec![reason]),
            };
            BUCKETS.with(|buckets| {
                buckets.borrow_mut().remove(&name);
            });
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &storage_bucket_manager_class().get_property("prototype"),
    );
    mirror_prototype(
        &value,
        &storage_bucket_manager_class(),
        &["open", "keys", "delete"],
    );
    value
}

pub fn reset_realm() {
    MANAGER_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    BUCKET_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn bucket_lifecycle_and_metadata_are_promise_compatible() {
        BUCKETS.with(|buckets| buckets.borrow_mut().clear());
        let manager = storage_bucket_manager_value();
        let log = Rc::new(RefCell::new(Vec::new()));
        let log_for_open = Rc::clone(&log);
        manager
            .call_method(
                "open",
                vec![
                    Value::string("app-data"),
                    Value::object(HashMap::from([("expires".into(), Value::Number(1234.0))])),
                ],
            )
            .call_method(
                "then",
                vec![Value::function(move |_, args| {
                    let bucket = args[0].clone();
                    log_for_open.borrow_mut().push(format!(
                        "{}:{}:{}:{}",
                        bucket.get_property("name").to_js_string(),
                        bucket.get_property("expires").to_js_string(),
                        w3cos_core::class::instance_of(&bucket, &storage_bucket_class()),
                        bucket.get_property("caches").is_object()
                    ));
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*log.borrow(), &["app-data:1234:true:true"]);

        let names = Rc::new(RefCell::new(String::new()));
        let names_for_handler = Rc::clone(&names);
        manager.call_method("keys", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                *names_for_handler.borrow_mut() = args[0].to_js_string();
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*names.borrow(), "app-data");

        manager.call_method("delete", vec![Value::string("app-data")]);
        crate::jsdom::drain_microtasks();
        let length = Rc::new(RefCell::new(Value::Undefined));
        let length_for_handler = Rc::clone(&length);
        manager.call_method("keys", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                *length_for_handler.borrow_mut() = args[0].get_property("length");
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(*length.borrow(), 0.into());
    }

    #[test]
    fn invalid_bucket_names_reject_type_error() {
        let name = Rc::new(RefCell::new(String::new()));
        let name_for_handler = Rc::clone(&name);
        storage_bucket_manager_value()
            .call_method("open", vec![Value::string("Bad/Name")])
            .call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *name_for_handler.borrow_mut() = args[0].get_property("name").to_js_string();
                    Value::Undefined
                })],
            );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*name.borrow(), "TypeError");
    }

    #[test]
    fn metadata_persists_but_js_entry_points_are_realm_owned() {
        BUCKETS.with(|buckets| buckets.borrow_mut().clear());
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let old_manager = storage_bucket_manager_value();
        let old_manager_class = storage_bucket_manager_class();
        let old_bucket_class = storage_bucket_class();
        old_bucket_class
            .get_property("prototype")
            .set_property("oldRealmMarker", Value::Bool(true));
        let old_bucket = bucket_value("persistent-data".into());
        BUCKETS.with(|buckets| {
            buckets
                .borrow_mut()
                .insert("persistent-data".into(), BucketState::default());
        });

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let new_manager = storage_bucket_manager_value();
        let new_manager_class = storage_bucket_manager_class();
        let new_bucket_class = storage_bucket_class();
        assert!(!old_manager_class.strict_eq(&new_manager_class));
        assert!(!old_bucket_class.strict_eq(&new_bucket_class));
        assert!(
            new_bucket_class
                .get_property("prototype")
                .get_property("oldRealmMarker")
                .is_undefined()
        );
        assert!(old_manager.call_method("keys", vec![]).is_undefined());
        assert!(
            old_manager
                .call_method("delete", vec![Value::string("persistent-data")])
                .is_undefined()
        );
        assert!(
            old_bucket
                .call_method("setExpires", vec![Value::Number(42.0)])
                .is_undefined()
        );
        assert!(old_bucket.get_property("expires").is_undefined());
        assert!(
            old_bucket_class
                .call(Value::Undefined, vec![])
                .is_undefined()
        );

        let names = Rc::new(RefCell::new(String::new()));
        let names_for_then = Rc::clone(&names);
        new_manager.call_method("keys", vec![]).call_method(
            "then",
            vec![Value::function(move |_, args| {
                *names_for_then.borrow_mut() = args[0].to_js_string();
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(&*names.borrow(), "persistent-data");
        BUCKETS.with(|buckets| buckets.borrow_mut().clear());
        reset_realm();
    }
}
