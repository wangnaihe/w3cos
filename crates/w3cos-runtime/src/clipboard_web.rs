//! ClipboardItem and DataTransfer browser facades.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Once;

use w3cos_core::{JsObject, ProxyBuilder, Value};

thread_local! {
    static CLIPBOARD_ITEMS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static CLIPBOARD_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CLIPBOARD_ITEM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DATA_TRANSFER_ITEM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DATA_TRANSFER_ITEM_LIST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DATA_TRANSFER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FILE_LIST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub fn clipboard_class() -> Value {
    CLIPBOARD_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: Clipboard"),
                ),
            ])))
        });
        class.set_property("name", Value::string("Clipboard"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in [
            "onclipboardchange",
            "read",
            "readText",
            "write",
            "writeText",
        ] {
            prototype.set_property(property, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn clipboard_value() -> Value {
    let clipboard = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
    w3cos_core::class::set_prototype_of(&clipboard, &clipboard_class().get_property("prototype"));
    clipboard.set_property("onclipboardchange", Value::Null);
    clipboard.set_property(
        "readText",
        Value::function(|_, _| {
            w3cos_core::promise::resolve(vec![Value::string(&crate::jsdom::clipboard_read_text())])
        }),
    );
    clipboard.set_property(
        "writeText",
        Value::function(|_, args| {
            crate::jsdom::clipboard_write_text(
                &args.first().cloned().unwrap_or_default().to_js_string(),
            );
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    clipboard.set_property(
        "read",
        Value::function(|_, _| w3cos_core::promise::resolve(vec![read_items()])),
    );
    clipboard.set_property(
        "write",
        Value::function(|_, args| {
            write_items(&args.first().cloned().unwrap_or_default());
            w3cos_core::promise::resolve(vec![Value::Undefined])
        }),
    );
    clipboard
}

pub fn read_items() -> Value {
    Value::array(CLIPBOARD_ITEMS.with(|items| items.borrow().clone()))
}

pub fn write_items(value: &Value) {
    CLIPBOARD_ITEMS.with(|items| *items.borrow_mut() = value.iter().collect());
}

pub fn clipboard_item_class() -> Value {
    CLIPBOARD_ITEM_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let input = args.first().cloned().unwrap_or_default();
            let entries = Rc::new(RefCell::new(HashMap::<String, Value>::new()));
            if let Value::Object(object) = input {
                let object = object.borrow();
                for key in object.keys() {
                    let value = object.get_direct(&key);
                    let blob = if crate::files::blob_bytes(&value).is_some() {
                        value
                    } else {
                        w3cos_core::class::construct(
                            &crate::files::blob_class(),
                            vec![
                                Value::array(vec![value]),
                                Value::object(HashMap::from([(
                                    "type".to_string(),
                                    Value::string(&key),
                                )])),
                            ],
                        )
                    };
                    entries.borrow_mut().insert(key, blob);
                }
            }
            let value = Value::object(HashMap::new());
            value.set_property(
                "types",
                Value::array(
                    entries
                        .borrow()
                        .keys()
                        .map(|key| Value::string(key))
                        .collect(),
                ),
            );
            value.set_property("presentationStyle", Value::string("unspecified"));
            let entries_for_get = entries;
            value.set_property(
                "getType",
                Value::function(move |_, args| {
                    entries_for_get
                        .borrow()
                        .get(&args.first().cloned().unwrap_or_default().to_js_string())
                        .cloned()
                        .unwrap_or_else(|| {
                            w3cos_core::class::construct(
                                &crate::files::blob_class(),
                                vec![Value::array(vec![])],
                            )
                        })
                }),
            );
            w3cos_core::class::set_prototype_of(
                &value,
                &clipboard_item_class().get_property("prototype"),
            );
            value
        });
        class.set_property(
            "supports",
            Value::function(|_, args| {
                Value::Bool(
                    args.first()
                        .cloned()
                        .unwrap_or_default()
                        .to_js_string()
                        .starts_with("text/"),
                )
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["getType", "types"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn illegal_constructor_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
    properties: &'static [&'static str],
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string(&format!("Illegal constructor: {name}")),
                ),
            ])))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in properties {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn data_transfer_item_class() -> Value {
    illegal_constructor_class(
        &DATA_TRANSFER_ITEM_CLASS,
        "DataTransferItem",
        &[
            "getAsFile",
            "getAsFileSystemHandle",
            "getAsString",
            "kind",
            "type",
            "webkitGetAsEntry",
        ],
    )
}

pub fn data_transfer_item_list_class() -> Value {
    illegal_constructor_class(
        &DATA_TRANSFER_ITEM_LIST_CLASS,
        "DataTransferItemList",
        &["add", "clear", "length", "remove"],
    )
}

pub fn file_list_class() -> Value {
    illegal_constructor_class(&FILE_LIST_CLASS, "FileList", &["item", "length"])
}

fn data_transfer_item(kind: &str, type_name: &str, data: Value) -> Value {
    let value = Value::object(HashMap::from([
        ("kind".to_string(), Value::string(kind)),
        ("type".to_string(), Value::string(type_name)),
        ("__w3cos_data".to_string(), data.clone()),
    ]));
    let string_data = data.clone();
    let is_string = kind == "string";
    value.set_property(
        "getAsString",
        Value::function(move |_, args| {
            if is_string
                && let Some(callback) = args.first()
                && callback.is_function()
            {
                callback.call(
                    Value::Undefined,
                    vec![Value::string(&string_data.to_js_string())],
                );
            }
            Value::Undefined
        }),
    );
    let is_file = kind == "file";
    value.set_property(
        "getAsFile",
        Value::function(move |_, _| if is_file { data.clone() } else { Value::Null }),
    );
    value.set_property(
        "getAsFileSystemHandle",
        Value::function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: DataTransferItem.getAsFileSystemHandle() requires a \
                     platform file-system permission adapter; rejecting with NotSupportedError"
                );
            });
            w3cos_core::promise::reject(vec![Value::object(HashMap::from([
                ("name".into(), Value::string("NotSupportedError")),
                (
                    "message".into(),
                    Value::string("File-system handles are unavailable"),
                ),
            ]))])
        }),
    );
    value.set_property(
        "webkitGetAsEntry",
        Value::function(|_, _| {
            static WARNING: Once = Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: DataTransferItem.webkitGetAsEntry() requires a platform \
                     file-system adapter; returning null"
                );
            });
            Value::Null
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &data_transfer_item_class().get_property("prototype"),
    );
    value
}

fn transfer_files(entries: &[Value]) -> Vec<Value> {
    entries
        .iter()
        .filter(|item| item.get_property("kind").to_js_string() == "file")
        .map(|item| item.get_property("__w3cos_data"))
        .collect()
}

fn data_transfer_item_list(entries: Rc<RefCell<Vec<Value>>>) -> Value {
    let length_entries = Rc::clone(&entries);
    let add_entries = Rc::clone(&entries);
    let remove_entries = Rc::clone(&entries);
    let clear_entries = Rc::clone(&entries);
    let properties = HashMap::from([
        (
            "__w3cos_getter_length".into(),
            Value::function(move |_, _| Value::Number(length_entries.borrow().len() as f64)),
        ),
        (
            "add".into(),
            Value::function(move |_, args| {
                let data = args.first().cloned().unwrap_or_default();
                let (kind, type_name) =
                    if w3cos_core::class::instance_of(&data, &crate::files::file_class()) {
                        ("file", data.get_property("type").to_js_string())
                    } else {
                        (
                            "string",
                            args.get(1).cloned().unwrap_or_default().to_js_string(),
                        )
                    };
                let item = data_transfer_item(kind, &type_name, data);
                add_entries.borrow_mut().push(item.clone());
                item
            }),
        ),
        (
            "remove".into(),
            Value::function(move |_, args| {
                let index = args.first().cloned().unwrap_or_default().to_u32() as usize;
                if index < remove_entries.borrow().len() {
                    remove_entries.borrow_mut().remove(index);
                }
                Value::Undefined
            }),
        ),
        (
            "clear".into(),
            Value::function(move |_, _| {
                clear_entries.borrow_mut().clear();
                Value::Undefined
            }),
        ),
    ]);
    let indexed_entries = entries;
    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
            let inherited = target.get_property(key);
            if !inherited.is_undefined() {
                return inherited;
            }
            key.parse::<usize>()
                .ok()
                .and_then(|index| indexed_entries.borrow().get(index).cloned())
                .unwrap_or(Value::Undefined)
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        properties, handler,
    ))));
    w3cos_core::class::set_prototype_of(
        &value,
        &data_transfer_item_list_class().get_property("prototype"),
    );
    value
}

fn file_list(entries: Rc<RefCell<Vec<Value>>>) -> Value {
    let length_entries = Rc::clone(&entries);
    let item_entries = Rc::clone(&entries);
    let properties = HashMap::from([
        (
            "__w3cos_getter_length".into(),
            Value::function(move |_, _| {
                Value::Number(transfer_files(&length_entries.borrow()).len() as f64)
            }),
        ),
        (
            "item".into(),
            Value::function(move |_, args| {
                let index = args.first().cloned().unwrap_or_default().to_u32() as usize;
                transfer_files(&item_entries.borrow())
                    .get(index)
                    .cloned()
                    .unwrap_or(Value::Null)
            }),
        ),
    ]);
    let indexed_entries = entries;
    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
            let inherited = target.get_property(key);
            if !inherited.is_undefined() {
                return inherited;
            }
            key.parse::<usize>()
                .ok()
                .and_then(|index| {
                    transfer_files(&indexed_entries.borrow())
                        .get(index)
                        .cloned()
                })
                .unwrap_or(Value::Undefined)
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        properties, handler,
    ))));
    w3cos_core::class::set_prototype_of(&value, &file_list_class().get_property("prototype"));
    value
}

pub fn data_transfer_class() -> Value {
    DATA_TRANSFER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            let entries = Rc::new(RefCell::new(Vec::<Value>::new()));
            let value = Value::object(HashMap::from([
                ("dropEffect".to_string(), Value::string("none")),
                ("effectAllowed".to_string(), Value::string("uninitialized")),
            ]));
            let getter_entries = Rc::clone(&entries);
            value.set_property(
                "__w3cos_getter_types",
                Value::function(move |_, _| {
                    Value::array(
                        getter_entries
                            .borrow()
                            .iter()
                            .filter(|item| item.get_property("kind").to_js_string() == "string")
                            .map(|item| Value::string(&item.get_property("type").to_js_string()))
                            .chain(
                                (!transfer_files(&getter_entries.borrow()).is_empty())
                                    .then(|| Value::string("Files")),
                            )
                            .collect(),
                    )
                }),
            );
            value.set_property("items", data_transfer_item_list(Rc::clone(&entries)));
            value.set_property("files", file_list(Rc::clone(&entries)));
            let set_entries = Rc::clone(&entries);
            value.set_property(
                "setData",
                Value::function(move |_, args| {
                    let type_name = args.first().cloned().unwrap_or_default().to_js_string();
                    let data = args.get(1).cloned().unwrap_or_default();
                    let mut entries = set_entries.borrow_mut();
                    entries.retain(|item| {
                        item.get_property("kind").to_js_string() != "string"
                            || item.get_property("type").to_js_string() != type_name
                    });
                    entries.push(data_transfer_item(
                        "string",
                        &type_name,
                        Value::string(&data.to_js_string()),
                    ));
                    Value::Undefined
                }),
            );
            let get_entries = Rc::clone(&entries);
            value.set_property(
                "getData",
                Value::function(move |_, args| {
                    let kind = args.first().cloned().unwrap_or_default().to_js_string();
                    get_entries
                        .borrow()
                        .iter()
                        .find(|item| {
                            item.get_property("kind").to_js_string() == "string"
                                && item.get_property("type").to_js_string() == kind
                        })
                        .map(|item| item.get_property("__w3cos_data"))
                        .unwrap_or_else(|| Value::string(""))
                }),
            );
            let clear_entries = entries;
            value.set_property(
                "clearData",
                Value::function(move |_, args| {
                    let kind = args.first().cloned().unwrap_or_default();
                    if kind.is_undefined() {
                        clear_entries
                            .borrow_mut()
                            .retain(|item| item.get_property("kind").to_js_string() != "string");
                    } else {
                        let kind = kind.to_js_string();
                        clear_entries.borrow_mut().retain(|item| {
                            item.get_property("kind").to_js_string() != "string"
                                || item.get_property("type").to_js_string() != kind
                        });
                    }
                    Value::Undefined
                }),
            );
            value.set_property(
                "setDragImage",
                Value::function(|_, _| {
                    static WARNING: Once = Once::new();
                    WARNING.call_once(|| {
                        eprintln!(
                            "[w3cos] warning: DataTransfer.setDragImage() is accepted for \
                             compatibility; native drag-image rendering requires a host adapter"
                        );
                    });
                    Value::Undefined
                }),
            );
            w3cos_core::class::set_prototype_of(
                &value,
                &data_transfer_class().get_property("prototype"),
            );
            value
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in [
            "clearData",
            "dropEffect",
            "effectAllowed",
            "files",
            "getData",
            "items",
            "setData",
            "setDragImage",
            "types",
        ] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_has_standard_identity_and_event_target_shape() {
        let clipboard = clipboard_value();
        assert!(w3cos_core::class::instance_of(
            &clipboard,
            &clipboard_class()
        ));
        assert!(w3cos_core::class::instance_of(
            &clipboard,
            &crate::web_events::event_target_class()
        ));
        assert!(clipboard.get_property("read").is_function());
        assert!(clipboard.get_property("readText").is_function());
        assert!(clipboard.get_property("write").is_function());
        assert!(clipboard.get_property("writeText").is_function());
        assert!(clipboard.get_property("onclipboardchange").is_null());
    }

    #[test]
    fn data_transfer_exposes_live_item_and_file_collections() {
        let transfer = w3cos_core::class::construct(&data_transfer_class(), vec![]);
        let items = transfer.get_property("items");
        let files = transfer.get_property("files");
        let file = w3cos_core::class::construct(
            &crate::files::file_class(),
            vec![
                Value::array(vec![Value::string("payload")]),
                Value::string("payload.txt"),
                Value::object(HashMap::from([(
                    "type".into(),
                    Value::string("text/plain"),
                )])),
            ],
        );
        let file_item = items.call_method("add", vec![file.clone()]);
        let string_item = items.call_method(
            "add",
            vec![Value::string("memo"), Value::string("text/x-note")],
        );

        assert!(w3cos_core::class::instance_of(
            &items,
            &data_transfer_item_list_class()
        ));
        assert!(w3cos_core::class::instance_of(&files, &file_list_class()));
        assert!(w3cos_core::class::instance_of(
            &file_item,
            &data_transfer_item_class()
        ));
        assert_eq!(items.get_property("length").to_number(), 2.0);
        assert_eq!(files.get_property("length").to_number(), 1.0);
        assert_eq!(items.get_property("0"), file_item);
        assert_eq!(files.get_property("0"), file);
        assert_eq!(file_item.call_method("getAsFile", vec![]), file);
        assert_eq!(string_item.get_property("kind").to_js_string(), "string");

        items.call_method("remove", vec![Value::Number(1.0)]);
        assert_eq!(items.get_property("length").to_number(), 1.0);
        assert_eq!(
            files.call_method("item", vec![Value::Number(1.0)]),
            Value::Null
        );
        items.call_method("clear", vec![]);
        assert_eq!(items.get_property("length").to_number(), 0.0);
        assert_eq!(files.get_property("length").to_number(), 0.0);
    }
}
