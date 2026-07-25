//! Web NFC compatibility surface.
//!
//! Data objects are fully usable. Hardware operations remain explicit rejected
//! promises until an embedder supplies NFC discovery, permission and I/O.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use w3cos_core::Value;

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    )
}

fn reject_unavailable(operation: &str) -> Value {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: Web NFC exposes compatible data/event interfaces; scanning, \
             permission prompts and tag I/O require a platform NFC adapter"
        );
    });
    w3cos_core::promise::reject(vec![error(
        "NotSupportedError",
        &format!("{operation} is unavailable without a platform NFC adapter"),
    )])
}

fn cached_class(name: &'static str, build: impl FnOnce() -> Value) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build();
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub fn ndef_record_class() -> Value {
    cached_class("NDEFRecord", || {
        let class = Value::function(|this, args| {
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            if init.is_undefined() || init.is_null() {
                w3cos_core::throw_value(error(
                    "TypeError",
                    "NDEFRecord requires a record initialization object",
                ));
            }
            let record_type = init.get_property("recordType");
            this.set_property(
                "recordType",
                if record_type.is_undefined() {
                    Value::string("empty")
                } else {
                    record_type
                },
            );
            for name in ["mediaType", "id", "encoding", "lang", "data"] {
                let value = init.get_property(name);
                this.set_property(
                    name,
                    if value.is_undefined() {
                        if name == "data" {
                            Value::Null
                        } else {
                            Value::string("")
                        }
                    } else {
                        value
                    },
                );
            }
            this.set_property(
                "toRecords",
                Value::function(|this, _| {
                    if this.get_property("recordType").to_js_string() != "smart-poster" {
                        return Value::Null;
                    }
                    let data = this.get_property("data");
                    if data.get_property("records").is_undefined() {
                        Value::Null
                    } else {
                        data.get_property("records")
                    }
                }),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("NDEFRecord"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        class
    })
}

pub fn ndef_message_class() -> Value {
    cached_class("NDEFMessage", || {
        let class = Value::function(|this, args| {
            let init = args.first().cloned().unwrap_or(Value::Undefined);
            let records = if init.is_undefined() {
                Value::array(vec![])
            } else {
                let records = init.get_property("records");
                if records.is_undefined() {
                    Value::array(vec![])
                } else {
                    Value::array(
                        records
                            .iter()
                            .map(|record| {
                                if w3cos_core::class::instance_of(&record, &ndef_record_class()) {
                                    record
                                } else {
                                    w3cos_core::class::construct(&ndef_record_class(), vec![record])
                                }
                            })
                            .collect(),
                    )
                }
            };
            this.set_property("records", records);
            Value::Undefined
        });
        class.set_property("name", Value::string("NDEFMessage"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        class.set_property("prototype", prototype);
        class
    })
}

pub fn ndef_reading_event_class() -> Value {
    cached_class("NDEFReadingEvent", || {
        let class = Value::function(|this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            let message = init.get_property("message");
            if message.is_undefined() {
                w3cos_core::throw_value(error("TypeError", "NDEFReadingEvent requires a message"));
            }
            this.set_property("serialNumber", init.get_property("serialNumber"));
            this.set_property(
                "message",
                if w3cos_core::class::instance_of(&message, &ndef_message_class()) {
                    message
                } else {
                    w3cos_core::class::construct(&ndef_message_class(), vec![message])
                },
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("NDEFReadingEvent"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        class
    })
}

pub fn ndef_reader_class() -> Value {
    cached_class("NDEFReader", || {
        let class = Value::function(|this, _| {
            crate::web_events::event_target_class().call(this.clone(), vec![]);
            this.set_property("onreading", Value::Null);
            this.set_property("onreadingerror", Value::Null);
            this.set_property(
                "scan",
                Value::function(|_, args| {
                    let signal = args
                        .first()
                        .cloned()
                        .unwrap_or(Value::Undefined)
                        .get_property("signal");
                    if !signal.is_undefined() && signal.get_property("aborted").to_bool() {
                        return w3cos_core::promise::reject(vec![signal.get_property("reason")]);
                    }
                    reject_unavailable("NDEFReader.scan")
                }),
            );
            this.set_property(
                "write",
                Value::function(|_, args| {
                    let message = args.first().cloned().unwrap_or(Value::Undefined);
                    if message.is_undefined() || message.is_null() {
                        return w3cos_core::promise::reject(vec![error(
                            "TypeError",
                            "NDEFReader.write requires a message",
                        )]);
                    }
                    reject_unavailable("NDEFReader.write")
                }),
            );
            this.set_property(
                "makeReadOnly",
                Value::function(|_, _| reject_unavailable("NDEFReader.makeReadOnly")),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("NDEFReader"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        class
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn constructs_ndef_data_and_rejects_missing_hardware_explicitly() {
        let message = w3cos_core::class::construct(
            &ndef_message_class(),
            vec![Value::object(HashMap::from([(
                "records".into(),
                Value::array(vec![Value::object(HashMap::from([
                    ("recordType".into(), Value::string("text")),
                    ("data".into(), Value::string("hello")),
                ]))]),
            )]))],
        );
        assert_eq!(
            message.get_property("records").get_property("length"),
            1.into()
        );
        assert!(w3cos_core::class::instance_of(
            &message.get_property("records").get_property("0"),
            &ndef_record_class()
        ));

        let reader = w3cos_core::class::construct(&ndef_reader_class(), vec![]);
        let errors = Rc::new(RefCell::new(Vec::new()));
        for (method, args) in [
            ("scan", vec![]),
            ("write", vec![]),
            ("write", vec![Value::string("payload")]),
            ("makeReadOnly", vec![]),
        ] {
            let errors_for_handler = Rc::clone(&errors);
            reader.call_method(method, args).call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    errors_for_handler
                        .borrow_mut()
                        .push(args[0].get_property("name").to_js_string());
                    Value::Undefined
                })],
            );
        }
        crate::jsdom::drain_microtasks();
        assert_eq!(
            &*errors.borrow(),
            &[
                "NotSupportedError",
                "TypeError",
                "NotSupportedError",
                "NotSupportedError"
            ]
        );
    }
}
