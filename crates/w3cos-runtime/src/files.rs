//! Browser-shaped Blob, File, and FileReader facades.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use w3cos_core::Value;

const BLOB_STATE_KEY: &str = "__w3cos_blob_id";

#[derive(Clone)]
struct BlobState {
    bytes: Vec<u8>,
    type_name: String,
}

thread_local! {
    static NEXT_BLOB_ID: Cell<u64> = const { Cell::new(1) };
    static BLOBS: RefCell<HashMap<u64, Rc<BlobState>>> = RefCell::new(HashMap::new());
    static BLOB_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FILE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static FILE_READER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn state_of(value: &Value) -> Option<Rc<BlobState>> {
    let Value::Object(object) = value else {
        return None;
    };
    let Value::Number(id) = object.borrow().get_direct(BLOB_STATE_KEY) else {
        return None;
    };
    BLOBS.with(|states| states.borrow().get(&(id as u64)).cloned())
}

pub fn blob_bytes(value: &Value) -> Option<Vec<u8>> {
    state_of(value).map(|state| state.bytes.clone())
}

fn part_bytes(value: &Value) -> Vec<u8> {
    if let Some(bytes) = blob_bytes(value) {
        return bytes;
    }
    if let Some(bytes) = w3cos_core::binary::bytes_of(value) {
        return bytes;
    }
    value.to_js_string().into_bytes()
}

fn make_blob(bytes: Vec<u8>, type_name: String, prototype: Value) -> Value {
    let id = NEXT_BLOB_ID.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    let state = Rc::new(BlobState {
        bytes,
        type_name: type_name.to_ascii_lowercase(),
    });
    BLOBS.with(|states| states.borrow_mut().insert(id, state.clone()));
    let value = Value::object(HashMap::from([(
        BLOB_STATE_KEY.to_string(),
        Value::Number(id as f64),
    )]));
    w3cos_core::class::set_prototype_of(&value, &prototype);
    value.set_property("size", Value::Number(state.bytes.len() as f64));
    value.set_property("type", Value::string(&state.type_name));

    let state_for_text = state.clone();
    value.set_property(
        "text",
        Value::function(move |_, _| Value::string(&String::from_utf8_lossy(&state_for_text.bytes))),
    );
    let state_for_buffer = state.clone();
    value.set_property(
        "arrayBuffer",
        Value::function(move |_, _| {
            w3cos_core::binary::array_buffer_value(state_for_buffer.bytes.clone())
        }),
    );
    let state_for_bytes = state.clone();
    value.set_property(
        "bytes",
        Value::function(move |_, _| {
            w3cos_core::binary::typed_array_value(
                state_for_bytes
                    .bytes
                    .iter()
                    .map(|byte| Value::Number(*byte as f64))
                    .collect(),
            )
        }),
    );
    let state_for_slice = state;
    value.set_property(
        "slice",
        Value::function(move |_, args| {
            let length = state_for_slice.bytes.len();
            let start = normalize_index(args.first(), length, 0);
            let end = normalize_index(args.get(1), length, length).max(start);
            let type_name = args.get(2).map(Value::to_js_string).unwrap_or_default();
            make_blob(
                state_for_slice.bytes[start..end].to_vec(),
                type_name,
                blob_class().get_property("prototype"),
            )
        }),
    );
    value
}

fn normalize_index(value: Option<&Value>, length: usize, fallback: usize) -> usize {
    let number = value.map(Value::to_number).unwrap_or(fallback as f64);
    if number.is_sign_negative() {
        (length as i64 + number as i64).max(0) as usize
    } else {
        (number.max(0.0) as usize).min(length)
    }
}

fn construct_blob(args: &[Value], prototype: Value) -> Value {
    let parts = arg(args, 0);
    let options = arg(args, 1);
    let bytes = parts
        .iter()
        .flat_map(|part| part_bytes(&part))
        .collect::<Vec<_>>();
    let type_name = if options.is_object() {
        options.get_property("type").to_js_string()
    } else {
        String::new()
    };
    make_blob(bytes, type_name, prototype)
}

pub fn blob_class() -> Value {
    BLOB_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            construct_blob(&args, blob_class().get_property("prototype"))
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn file_class() -> Value {
    FILE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, args| {
            let parts = arg(&args, 0);
            let name = arg(&args, 1).to_js_string();
            let options = arg(&args, 2);
            let value = construct_blob(
                &[parts, options.clone()],
                file_class().get_property("prototype"),
            );
            value.set_property("name", Value::string(&name.replace('/', ":")));
            value.set_property("webkitRelativePath", Value::string(""));
            let last_modified = if options.is_object() {
                let value = options.get_property("lastModified");
                if value.is_undefined() {
                    now_millis()
                } else {
                    value.to_number()
                }
            } else {
                now_millis()
            };
            value.set_property("lastModified", Value::Number(last_modified));
            value
        });
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        w3cos_core::class::set_prototype_of(&prototype, &blob_class().get_property("prototype"));
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn now_millis() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.0
}

fn dispatch_reader_event(reader: &Value, type_name: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_subclass_class("ProgressEvent"),
        vec![Value::string(type_name)],
    );
    reader.call_method("dispatchEvent", vec![event]);
}

fn read_blob(reader: &Value, source: Value, mode: &str) {
    let Some(state) = state_of(&source) else {
        reader.set_property(
            "error",
            Value::object(HashMap::from([
                ("name".to_string(), Value::string("NotReadableError")),
                (
                    "message".to_string(),
                    Value::string("FileReader source must be a Blob or File"),
                ),
            ])),
        );
        reader.set_property("readyState", Value::Number(2.0));
        dispatch_reader_event(reader, "error");
        dispatch_reader_event(reader, "loadend");
        return;
    };
    reader.set_property("readyState", Value::Number(1.0));
    dispatch_reader_event(reader, "loadstart");
    let result = match mode {
        "arrayBuffer" => w3cos_core::binary::array_buffer_value(state.bytes.clone()),
        "dataURL" => {
            let binary = state
                .bytes
                .iter()
                .map(|byte| char::from(*byte))
                .collect::<String>();
            let encoded = w3cos_core::web::btoa(vec![Value::string(&binary)]).to_js_string();
            Value::string(&format!("data:{};base64,{encoded}", state.type_name))
        }
        "binaryString" => Value::string(
            &state
                .bytes
                .iter()
                .map(|byte| char::from(*byte))
                .collect::<String>(),
        ),
        _ => Value::string(&String::from_utf8_lossy(&state.bytes)),
    };
    reader.set_property("result", result);
    reader.set_property("error", Value::Null);
    reader.set_property("readyState", Value::Number(2.0));
    dispatch_reader_event(reader, "load");
    dispatch_reader_event(reader, "loadend");
}

pub fn file_reader_class() -> Value {
    FILE_READER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, _| {
            crate::web_events::event_target_class().call(this.clone(), vec![]);
            this.set_property("readyState", Value::Number(0.0));
            this.set_property("result", Value::Null);
            this.set_property("error", Value::Null);
            for (name, mode) in [
                ("readAsArrayBuffer", "arrayBuffer"),
                ("readAsText", "text"),
                ("readAsDataURL", "dataURL"),
                ("readAsBinaryString", "binaryString"),
            ] {
                this.set_property(
                    name,
                    Value::function(move |this, args| {
                        read_blob(&this, arg(&args, 0), mode);
                        Value::Undefined
                    }),
                );
            }
            this.set_property(
                "abort",
                Value::function(|this, _| {
                    this.set_property("result", Value::Null);
                    this.set_property("readyState", Value::Number(2.0));
                    this.set_property(
                        "error",
                        Value::object(HashMap::from([(
                            "name".to_string(),
                            Value::string("AbortError"),
                        )])),
                    );
                    dispatch_reader_event(&this, "abort");
                    dispatch_reader_event(&this, "loadend");
                    Value::Undefined
                }),
            );
            for (name, value) in [("EMPTY", 0.0), ("LOADING", 1.0), ("DONE", 2.0)] {
                this.set_property(name, Value::Number(value));
            }
            Value::Undefined
        });
        for (name, value) in [("EMPTY", 0.0), ("LOADING", 1.0), ("DONE", 2.0)] {
            class.set_property(name, Value::Number(value));
        }
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_file_and_reader_preserve_bytes_and_metadata() {
        let blob = w3cos_core::class::construct(
            &blob_class(),
            vec![
                Value::array(vec![Value::string("hello"), Value::string("世界")]),
                Value::object(HashMap::from([(
                    "type".to_string(),
                    Value::string("Text/Plain"),
                )])),
            ],
        );
        assert_eq!(blob.get_property("size").to_number(), 11.0);
        assert_eq!(blob.get_property("type").to_js_string(), "text/plain");
        assert_eq!(blob.call_method("text", vec![]).to_js_string(), "hello世界");

        let file = w3cos_core::class::construct(
            &file_class(),
            vec![
                Value::array(vec![blob.clone()]),
                Value::string("note.txt"),
                Value::object(HashMap::from([(
                    "lastModified".to_string(),
                    Value::Number(123.0),
                )])),
            ],
        );
        assert_eq!(file.get_property("name").to_js_string(), "note.txt");
        assert_eq!(file.get_property("lastModified").to_number(), 123.0);
        assert!(w3cos_core::class::instance_of(&file, &blob_class()));

        let reader = w3cos_core::class::construct(&file_reader_class(), vec![]);
        reader.call_method("readAsText", vec![file]);
        assert_eq!(reader.get_property("result").to_js_string(), "hello世界");
        assert_eq!(reader.get_property("readyState").to_number(), 2.0);
    }
}
