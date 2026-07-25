//! Browser-shaped Blob, File, and FileReader facades.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static FILE_READER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

pub fn blob_bytes(value: &Value) -> Option<Vec<u8>> {
    w3cos_core::web::blob_bytes(value)
}

pub fn blob_class() -> Value {
    w3cos_core::web::blob_class()
}

pub fn file_class() -> Value {
    w3cos_core::web::file_class()
}

fn dispatch_reader_event(reader: &Value, type_name: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_subclass_class("ProgressEvent"),
        vec![Value::string(type_name)],
    );
    reader.call_method("dispatchEvent", vec![event]);
}

fn read_blob(reader: &Value, source: Value, mode: &str) {
    let Some(bytes) = blob_bytes(&source) else {
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
        "arrayBuffer" => w3cos_core::binary::array_buffer_value(bytes.clone()),
        "dataURL" => {
            let binary = bytes
                .iter()
                .map(|byte| char::from(*byte))
                .collect::<String>();
            let encoded = w3cos_core::web::btoa(vec![Value::string(&binary)]).to_js_string();
            Value::string(&format!(
                "data:{};base64,{encoded}",
                source.get_property("type").to_js_string()
            ))
        }
        "binaryString" => Value::string(
            &bytes
                .iter()
                .map(|byte| char::from(*byte))
                .collect::<String>(),
        ),
        _ => Value::string(&String::from_utf8_lossy(&bytes)),
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
        for (name, value) in [
            ("DONE", Value::Number(2.0)),
            ("EMPTY", Value::Number(0.0)),
            ("LOADING", Value::Number(1.0)),
            ("abort", Value::Undefined),
            ("error", Value::Undefined),
            ("onabort", Value::Undefined),
            ("onerror", Value::Undefined),
            ("onload", Value::Undefined),
            ("onloadend", Value::Undefined),
            ("onloadstart", Value::Undefined),
            ("onprogress", Value::Undefined),
            ("readAsArrayBuffer", Value::Undefined),
            ("readAsBinaryString", Value::Undefined),
            ("readAsDataURL", Value::Undefined),
            ("readAsText", Value::Undefined),
            ("readyState", Value::Undefined),
            ("result", Value::Undefined),
        ] {
            prototype.set_property(name, value);
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
