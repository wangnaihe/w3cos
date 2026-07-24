//! ClipboardItem and DataTransfer browser facades.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

thread_local! {
    static CLIPBOARD_ITEMS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static CLIPBOARD_ITEM_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DATA_TRANSFER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
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
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn data_transfer_item(kind: &str, type_name: &str, data: Value) -> Value {
    let value = Value::object(HashMap::from([
        ("kind".to_string(), Value::string(kind)),
        ("type".to_string(), Value::string(type_name)),
    ]));
    let string_data = data.clone();
    value.set_property(
        "getAsString",
        Value::function(move |_, args| {
            if let Some(callback) = args.first()
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
    value.set_property(
        "getAsFile",
        Value::function(move |_, _| {
            if crate::files::blob_bytes(&data).is_some() {
                data.clone()
            } else {
                Value::Null
            }
        }),
    );
    value
}

pub fn data_transfer_class() -> Value {
    DATA_TRANSFER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            let entries = Rc::new(RefCell::new(Vec::<(String, Value)>::new()));
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
                            .map(|(kind, _)| Value::string(kind))
                            .collect(),
                    )
                }),
            );
            let item_entries = Rc::clone(&entries);
            value.set_property(
                "__w3cos_getter_items",
                Value::function(move |_, _| {
                    Value::array(
                        item_entries
                            .borrow()
                            .iter()
                            .map(|(kind, data)| data_transfer_item("string", kind, data.clone()))
                            .collect(),
                    )
                }),
            );
            value.set_property("files", Value::array(vec![]));
            let set_entries = Rc::clone(&entries);
            value.set_property(
                "setData",
                Value::function(move |_, args| {
                    let kind = args.first().cloned().unwrap_or_default().to_js_string();
                    let data = args.get(1).cloned().unwrap_or_default();
                    let mut entries = set_entries.borrow_mut();
                    entries.retain(|(candidate, _)| candidate != &kind);
                    entries.push((kind, Value::string(&data.to_js_string())));
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
                        .find(|(candidate, _)| candidate == &kind)
                        .map(|(_, data)| data.clone())
                        .unwrap_or_else(|| Value::string(""))
                }),
            );
            let clear_entries = entries;
            value.set_property(
                "clearData",
                Value::function(move |_, args| {
                    let kind = args.first().cloned().unwrap_or_default();
                    if kind.is_undefined() {
                        clear_entries.borrow_mut().clear();
                    } else {
                        let kind = kind.to_js_string();
                        clear_entries
                            .borrow_mut()
                            .retain(|(candidate, _)| candidate != &kind);
                    }
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
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}
