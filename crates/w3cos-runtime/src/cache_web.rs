//! In-process JavaScript-facing Cache API compatibility layer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::jsdom::realm_function;
use w3cos_core::Value;

#[derive(Default)]
struct CacheStorageState {
    order: Vec<String>,
    entries: HashMap<String, Rc<RefCell<Vec<(String, Value)>>>>,
}

thread_local! {
    static STORAGE: RefCell<CacheStorageState> = RefCell::new(CacheStorageState::default());
    static CACHE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CACHE_STORAGE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CACHE_STORAGE_VALUE: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CACHE_WARNING_EMITTED: RefCell<bool> = const { RefCell::new(false) };
}

fn request_url(request: &Value) -> String {
    let url = request.get_property("url");
    if url.is_undefined() {
        request.to_js_string()
    } else {
        url.to_js_string()
    }
}

fn request_method(request: &Value) -> String {
    let method = request.get_property("method");
    if method.is_undefined() {
        "GET".to_string()
    } else {
        method.to_js_string().to_ascii_uppercase()
    }
}

fn comparable_url(url: &str, ignore_search: bool) -> &str {
    if ignore_search {
        url.split_once('?').map(|(base, _)| base).unwrap_or(url)
    } else {
        url
    }
}

fn options_match(stored_url: &str, request: &Value, options: &Value) -> bool {
    if request_method(request) != "GET" && !options.get_property("ignoreMethod").to_bool() {
        return false;
    }
    let requested_url = request_url(request);
    let ignore_search = options.get_property("ignoreSearch").to_bool();
    comparable_url(stored_url, ignore_search) == comparable_url(&requested_url, ignore_search)
}

fn clone_response(response: &Value) -> Value {
    let clone = response.get_property("clone");
    if clone.is_function() {
        clone.call(response.clone(), vec![])
    } else {
        response.clone()
    }
}

fn request_value(url: &str) -> Value {
    w3cos_core::class::construct(&crate::fetch::request_class(), vec![Value::string(url)])
}

fn cache_value(entries: Rc<RefCell<Vec<(String, Value)>>>) -> Value {
    let generation = crate::jsdom::realm_generation();
    let entries_for_match = Rc::clone(&entries);
    let entries_for_match_all = Rc::clone(&entries);
    let entries_for_put = Rc::clone(&entries);
    let entries_for_add = Rc::clone(&entries);
    let entries_for_add_all = Rc::clone(&entries);
    let entries_for_delete = Rc::clone(&entries);
    let entries_for_keys = Rc::clone(&entries);
    let cache = Value::object(HashMap::from([
        (
            "match".into(),
            realm_function(generation, move |_, args| {
                let request = args.first().cloned().unwrap_or(Value::Undefined);
                let options = args.get(1).cloned().unwrap_or(Value::Undefined);
                let response = entries_for_match
                    .borrow()
                    .iter()
                    .find_map(|(url, response)| {
                        options_match(url, &request, &options).then(|| clone_response(response))
                    })
                    .unwrap_or(Value::Undefined);
                w3cos_core::promise::resolve(vec![response])
            }),
        ),
        (
            "matchAll".into(),
            realm_function(generation, move |_, args| {
                let request = args.first().cloned();
                let options = args.get(1).cloned().unwrap_or(Value::Undefined);
                let responses = entries_for_match_all
                    .borrow()
                    .iter()
                    .filter_map(|(url, response)| {
                        request
                            .as_ref()
                            .is_none_or(|request| options_match(url, request, &options))
                            .then(|| clone_response(response))
                    })
                    .collect();
                w3cos_core::promise::resolve(vec![Value::array(responses)])
            }),
        ),
        (
            "put".into(),
            realm_function(generation, move |_, args| {
                let request = args.first().cloned().unwrap_or(Value::Undefined);
                let response = args.get(1).cloned().unwrap_or(Value::Undefined);
                if request_method(&request) != "GET" {
                    return w3cos_core::promise::reject(vec![Value::object(HashMap::from([
                        ("name".into(), Value::string("TypeError")),
                        (
                            "message".into(),
                            Value::string("Cache.put only accepts GET requests"),
                        ),
                    ]))]);
                }
                if response.get_property("bodyUsed").to_bool() {
                    return w3cos_core::promise::reject(vec![Value::object(HashMap::from([
                        ("name".into(), Value::string("TypeError")),
                        (
                            "message".into(),
                            Value::string("Cache.put cannot store a disturbed response"),
                        ),
                    ]))]);
                }
                let url = request_url(&request);
                let response = clone_response(&response);
                let mut entries = entries_for_put.borrow_mut();
                if let Some((_, stored)) = entries.iter_mut().find(|(stored, _)| stored == &url) {
                    *stored = response;
                } else {
                    entries.push((url, response));
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        ),
        (
            "add".into(),
            realm_function(generation, move |_, args| {
                let request = args.first().cloned().unwrap_or(Value::Undefined);
                let response = crate::fetch::fetch_value(vec![request.clone()]);
                let url = request_url(&request);
                let mut entries = entries_for_add.borrow_mut();
                entries.retain(|(stored, _)| stored != &url);
                entries.push((url, clone_response(&response)));
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        ),
        (
            "addAll".into(),
            realm_function(generation, move |_, args| {
                let requests = args.first().cloned().unwrap_or_default();
                let fetched = requests
                    .iter()
                    .map(|request| {
                        let response = crate::fetch::fetch_value(vec![request.clone()]);
                        (request_url(&request), clone_response(&response))
                    })
                    .collect::<Vec<_>>();
                let mut entries = entries_for_add_all.borrow_mut();
                for (url, response) in fetched {
                    entries.retain(|(stored, _)| stored != &url);
                    entries.push((url, response));
                }
                w3cos_core::promise::resolve(vec![Value::Undefined])
            }),
        ),
        (
            "delete".into(),
            realm_function(generation, move |_, args| {
                let request = args.first().cloned().unwrap_or(Value::Undefined);
                let options = args.get(1).cloned().unwrap_or(Value::Undefined);
                let mut entries = entries_for_delete.borrow_mut();
                let before = entries.len();
                entries.retain(|(url, _)| !options_match(url, &request, &options));
                w3cos_core::promise::resolve(vec![Value::Bool(entries.len() != before)])
            }),
        ),
        (
            "keys".into(),
            realm_function(generation, move |_, args| {
                let request = args.first().cloned();
                let options = args.get(1).cloned().unwrap_or(Value::Undefined);
                let requests = entries_for_keys
                    .borrow()
                    .iter()
                    .filter_map(|(url, _)| {
                        request
                            .as_ref()
                            .is_none_or(|request| options_match(url, request, &options))
                            .then(|| request_value(url))
                    })
                    .collect();
                w3cos_core::promise::resolve(vec![Value::array(requests)])
            }),
        ),
    ]));
    w3cos_core::class::set_prototype_of(&cache, &cache_class().get_property("prototype"));
    cache
}

pub fn cache_class() -> Value {
    CACHE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| {
            cache_value(Rc::new(RefCell::new(Vec::new())))
        });
        class.set_property("name", Value::string("Cache"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in [
            "add", "addAll", "delete", "keys", "match", "matchAll", "put",
        ] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn cache_storage_class() -> Value {
    CACHE_STORAGE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| cache_storage_value());
        class.set_property("name", Value::string("CacheStorage"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["delete", "has", "keys", "match", "open"] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn cache_storage_value() -> Value {
    CACHE_STORAGE_VALUE.with(|slot| {
        if let Some(storage) = slot.borrow().clone() {
            return storage;
        }
        CACHE_WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!(
                    "[w3cos] warning: CacheStorage currently uses process-local memory; \
                     persistence, quotas, and cross-process coordination remain pending"
                );
            }
        });
        let generation = crate::jsdom::realm_generation();
        let storage = Value::object(HashMap::from([
            (
                "open".into(),
                realm_function(generation, |_, args| {
                    let name = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string();
                    let entries = STORAGE.with(|storage| {
                        let mut storage = storage.borrow_mut();
                        if !storage.entries.contains_key(&name) {
                            storage.order.push(name.clone());
                        }
                        storage
                            .entries
                            .entry(name)
                            .or_insert_with(|| Rc::new(RefCell::new(Vec::new())))
                            .clone()
                    });
                    w3cos_core::promise::resolve(vec![cache_value(entries)])
                }),
            ),
            (
                "has".into(),
                realm_function(generation, |_, args| {
                    let name = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string();
                    let exists =
                        STORAGE.with(|storage| storage.borrow().entries.contains_key(&name));
                    w3cos_core::promise::resolve(vec![Value::Bool(exists)])
                }),
            ),
            (
                "delete".into(),
                realm_function(generation, |_, args| {
                    let name = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .to_js_string();
                    let removed = STORAGE.with(|storage| {
                        let mut storage = storage.borrow_mut();
                        storage.order.retain(|stored| stored != &name);
                        storage.entries.remove(&name).is_some()
                    });
                    w3cos_core::promise::resolve(vec![Value::Bool(removed)])
                }),
            ),
            (
                "keys".into(),
                realm_function(generation, |_, _| {
                    let names = STORAGE.with(|storage| {
                        storage
                            .borrow()
                            .order
                            .iter()
                            .map(|name| Value::string(name))
                            .collect()
                    });
                    w3cos_core::promise::resolve(vec![Value::array(names)])
                }),
            ),
            (
                "match".into(),
                realm_function(generation, |_, args| {
                    let request = args.first().cloned().unwrap_or(Value::Undefined);
                    let options = args.get(1).cloned().unwrap_or(Value::Undefined);
                    let cache_name = options.get_property("cacheName");
                    let response = STORAGE.with(|storage| {
                        let storage = storage.borrow();
                        storage
                            .order
                            .iter()
                            .filter(|name| {
                                cache_name.is_undefined() || cache_name.to_js_string() == **name
                            })
                            .find_map(|name| {
                                storage.entries.get(name).and_then(|entries| {
                                    entries.borrow().iter().find_map(|(url, response)| {
                                        options_match(url, &request, &options)
                                            .then(|| clone_response(response))
                                    })
                                })
                            })
                    });
                    w3cos_core::promise::resolve(vec![response.unwrap_or(Value::Undefined)])
                }),
            ),
        ]));
        w3cos_core::class::set_prototype_of(
            &storage,
            &cache_storage_class().get_property("prototype"),
        );
        *slot.borrow_mut() = Some(storage.clone());
        storage
    })
}

pub fn reset() {
    STORAGE.with(|storage| *storage.borrow_mut() = CacheStorageState::default());
    CACHE_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    CACHE_STORAGE_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    CACHE_STORAGE_VALUE.with(|slot| {
        slot.borrow_mut().take();
    });
    CACHE_WARNING_EMITTED.with(|warned| *warned.borrow_mut() = false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(promise: Value) -> Rc<RefCell<Value>> {
        let value = Rc::new(RefCell::new(Value::Undefined));
        let value_for_callback = Rc::clone(&value);
        promise.call_method(
            "then",
            vec![Value::function(move |_, args| {
                *value_for_callback.borrow_mut() =
                    args.first().cloned().unwrap_or(Value::Undefined);
                Value::Undefined
            })],
        );
        w3cos_core::promise::drain_microtasks();
        value
    }

    #[test]
    fn cache_storage_put_match_keys_and_delete() {
        reset();
        let storage = cache_storage_value();
        let cache = settled(storage.call_method("open", vec![Value::string("assets")]))
            .borrow()
            .clone();
        assert!(w3cos_core::class::instance_of(&cache, &cache_class()));
        let response = w3cos_core::class::construct(
            &crate::fetch::response_class(),
            vec![Value::string("cached")],
        );
        settled(cache.call_method(
            "put",
            vec![Value::string("https://example.test/a?v=1"), response],
        ));
        let matched = settled(cache.call_method(
            "match",
            vec![
                Value::string("https://example.test/a?v=2"),
                Value::object(HashMap::from([("ignoreSearch".into(), Value::Bool(true))])),
            ],
        ))
        .borrow()
        .clone();
        assert_eq!(matched.call_method("text", vec![]), Value::string("cached"));
        let keys = settled(storage.call_method("keys", vec![]))
            .borrow()
            .clone();
        assert_eq!(keys.get_property("0"), Value::string("assets"));
        assert!(
            settled(storage.call_method("delete", vec![Value::string("assets")]))
                .borrow()
                .to_bool()
        );
    }

    #[test]
    fn cache_storage_and_cache_methods_are_realm_owned() {
        reset();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_storage = cache_storage_value();
        let old_storage_class = cache_storage_class();
        let old_cache_class = cache_class();
        old_storage_class
            .get_property("prototype")
            .set_property("realmMarker", Value::Bool(true));
        let old_cache = settled(old_storage.call_method("open", vec![Value::string("old-assets")]))
            .borrow()
            .clone();

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let new_storage = cache_storage_value();
        let new_storage_class = cache_storage_class();
        let new_cache_class = cache_class();
        assert!(!old_storage.strict_eq(&new_storage));
        assert!(!old_storage_class.strict_eq(&new_storage_class));
        assert!(!old_cache_class.strict_eq(&new_cache_class));
        assert!(
            !new_storage_class
                .get_property("prototype")
                .get_property("realmMarker")
                .to_bool()
        );

        assert!(old_storage
            .call_method("open", vec![Value::string("stale-assets")])
            .is_undefined());
        assert!(old_cache.call_method("keys", vec![]).is_undefined());
        assert!(old_storage_class.call(Value::Undefined, vec![]).is_undefined());
        assert!(old_cache_class.call(Value::Undefined, vec![]).is_undefined());

        let stale_exists =
            settled(new_storage.call_method("has", vec![Value::string("stale-assets")]));
        assert!(!stale_exists.borrow().to_bool());
        let fresh_cache =
            settled(new_storage.call_method("open", vec![Value::string("fresh-assets")]))
                .borrow()
                .clone();
        assert!(w3cos_core::class::instance_of(
            &fresh_cache,
            &new_cache_class
        ));
    }
}
