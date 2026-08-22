//! Ordered `FormData` entries and Fetch multipart serialization.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

const FORM_DATA_ID: &str = "__w3cos_form_data_id";

#[derive(Clone)]
struct Entry {
    name: String,
    value: Value,
    filename: Option<String>,
}

thread_local! {
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static STATES: RefCell<HashMap<u64, Rc<RefCell<Vec<Entry>>>>> = RefCell::new(HashMap::new());
    static FORM_DATA_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn state_of(value: &Value) -> Option<Rc<RefCell<Vec<Entry>>>> {
    let Some(object) = value.as_object() else {
        return None;
    };
    let Some(id) = object.borrow().get_direct(FORM_DATA_ID).as_number() else {
        return None;
    };
    STATES.with(|states| states.borrow().get(&(id as u64)).cloned())
}

fn normalize_value(value: Value) -> Value {
    if crate::files::blob_bytes(&value).is_some() {
        value
    } else {
        Value::string(&value.to_js_string())
    }
}

fn entry_value(entry: &Entry) -> Value {
    entry.value.clone()
}

fn make_form_data() -> Value {
    let id = NEXT_ID.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    let state = Rc::new(RefCell::new(Vec::<Entry>::new()));
    STATES.with(|states| states.borrow_mut().insert(id, Rc::clone(&state)));
    let value = Value::object(HashMap::from([(
        FORM_DATA_ID.to_string(),
        Value::Number(id as f64),
    )]));
    w3cos_core::class::set_prototype_of(&value, &form_data_class().get_property("prototype"));

    let append_state = Rc::clone(&state);
    value.set_property(
        "append",
        Value::function(move |_, args| {
            let name = args.first().cloned().unwrap_or_default().to_js_string();
            let value = normalize_value(args.get(1).cloned().unwrap_or_default());
            let filename = args.get(2).map(Value::to_js_string);
            append_state.borrow_mut().push(Entry {
                name,
                value,
                filename,
            });
            Value::Undefined
        }),
    );
    let set_state = Rc::clone(&state);
    value.set_property(
        "set",
        Value::function(move |_, args| {
            let name = args.first().cloned().unwrap_or_default().to_js_string();
            let value = normalize_value(args.get(1).cloned().unwrap_or_default());
            let filename = args.get(2).map(Value::to_js_string);
            let mut entries = set_state.borrow_mut();
            let position = entries.iter().position(|entry| entry.name == name);
            entries.retain(|entry| entry.name != name);
            let entry = Entry {
                name,
                value,
                filename,
            };
            if let Some(position) = position {
                let insert_at = position.min(entries.len());
                entries.insert(insert_at, entry);
            } else {
                entries.push(entry);
            }
            Value::Undefined
        }),
    );
    let delete_state = Rc::clone(&state);
    value.set_property(
        "delete",
        Value::function(move |_, args| {
            let name = args.first().cloned().unwrap_or_default().to_js_string();
            delete_state.borrow_mut().retain(|entry| entry.name != name);
            Value::Undefined
        }),
    );
    let get_state = Rc::clone(&state);
    value.set_property(
        "get",
        Value::function(move |_, args| {
            let name = args.first().cloned().unwrap_or_default().to_js_string();
            get_state
                .borrow()
                .iter()
                .find(|entry| entry.name == name)
                .map(entry_value)
                .unwrap_or(Value::Null)
        }),
    );
    let get_all_state = Rc::clone(&state);
    value.set_property(
        "getAll",
        Value::function(move |_, args| {
            let name = args.first().cloned().unwrap_or_default().to_js_string();
            Value::array(
                get_all_state
                    .borrow()
                    .iter()
                    .filter(|entry| entry.name == name)
                    .map(entry_value)
                    .collect(),
            )
        }),
    );
    let has_state = Rc::clone(&state);
    value.set_property(
        "has",
        Value::function(move |_, args| {
            let name = args.first().cloned().unwrap_or_default().to_js_string();
            Value::Bool(has_state.borrow().iter().any(|entry| entry.name == name))
        }),
    );
    for method in ["entries", "keys", "values"] {
        let method_state = Rc::clone(&state);
        value.set_property(
            method,
            Value::function(move |_, _| {
                Value::array(
                    method_state
                        .borrow()
                        .iter()
                        .map(|entry| match method {
                            "keys" => Value::string(&entry.name),
                            "values" => entry_value(entry),
                            _ => Value::array(vec![Value::string(&entry.name), entry_value(entry)]),
                        })
                        .collect(),
                )
            }),
        );
    }
    let for_each_state = state;
    value.set_property(
        "forEach",
        Value::function(move |this, args| {
            let callback = args.first().cloned().unwrap_or_default();
            for entry in for_each_state.borrow().iter() {
                callback.call(
                    Value::Undefined,
                    vec![entry_value(entry), Value::string(&entry.name), this.clone()],
                );
            }
            Value::Undefined
        }),
    );
    value
}

pub fn form_data_class() -> Value {
    FORM_DATA_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| make_form_data());
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in [
            "append", "delete", "entries", "forEach", "get", "getAll", "has", "keys", "set",
            "values",
        ] {
            prototype.set_property(method, Value::function(|_, _| Value::Undefined));
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn serialize(value: &Value) -> Option<(String, String)> {
    let state = state_of(value)?;
    let boundary = format!("----w3cos-form-data-{}", state.borrow().len());
    let mut body = String::new();
    for entry in state.borrow().iter() {
        body.push_str(&format!("--{boundary}\r\n"));
        body.push_str(&format!(
            "Content-Disposition: form-data; name=\"{}\"",
            entry.name.replace('"', "%22")
        ));
        if let Some(bytes) = crate::files::blob_bytes(&entry.value) {
            let filename = entry.filename.clone().unwrap_or_else(|| {
                let name = entry.value.get_property("name");
                if name.is_undefined() {
                    "blob".to_string()
                } else {
                    name.to_js_string()
                }
            });
            body.push_str(&format!(
                "; filename=\"{}\"\r\n",
                filename.replace('"', "%22")
            ));
            let content_type = entry.value.get_property("type").to_js_string();
            if !content_type.is_empty() {
                body.push_str(&format!("Content-Type: {content_type}\r\n"));
            }
            body.push_str("\r\n");
            body.push_str(&String::from_utf8_lossy(&bytes));
        } else {
            body.push_str("\r\n\r\n");
            body.push_str(&entry.value.to_js_string());
        }
        body.push_str("\r\n");
    }
    body.push_str(&format!("--{boundary}--\r\n"));
    Some((body, format!("multipart/form-data; boundary={boundary}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_and_serializes_files() {
        let form = w3cos_core::class::construct(&form_data_class(), vec![]);
        form.call_method("append", vec![Value::string("tag"), Value::string("a")]);
        form.call_method("append", vec![Value::string("tag"), Value::string("b")]);
        let file = w3cos_core::class::construct(
            &crate::files::file_class(),
            vec![
                Value::array(vec![Value::string("hello")]),
                Value::string("a.txt"),
            ],
        );
        form.call_method("append", vec![Value::string("file"), file]);
        assert_eq!(
            form.call_method("getAll", vec![Value::string("tag")])
                .get_property("length")
                .to_u32(),
            2
        );
        let (body, content_type) = serialize(&form).unwrap();
        assert!(content_type.starts_with("multipart/form-data; boundary="));
        assert!(body.contains("name=\"tag\"\r\n\r\na"));
        assert!(body.contains("filename=\"a.txt\""));
        assert!(body.contains("\r\n\r\nhello\r\n"));
    }
}
