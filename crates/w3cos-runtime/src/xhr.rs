//! Synchronous JavaScript compatibility facade over the Fetch implementation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use w3cos_core::Value;

#[derive(Default)]
struct XhrState {
    method: String,
    url: String,
    request_headers: Vec<(String, String)>,
    response_headers: Value,
}

thread_local! {
    static XHR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static XHR_EVENT_TARGET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static XHR_UPLOAD_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn dispatch(target: &Value, event_type: &str) {
    let event = if event_type == "readystatechange" {
        w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![Value::string(event_type)],
        )
    } else {
        w3cos_core::class::construct(
            &crate::web_events::event_subclass_class("ProgressEvent"),
            vec![
                Value::string(event_type),
                Value::object(HashMap::from([
                    ("lengthComputable".into(), Value::Bool(false)),
                    ("loaded".into(), Value::Number(0.0)),
                    ("total".into(), Value::Number(0.0)),
                ])),
            ],
        )
    };
    target.call_method("dispatchEvent", vec![event]);
}

pub fn xml_http_request_event_target_class() -> Value {
    XHR_EVENT_TARGET_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string(
                    "Illegal constructor: XMLHttpRequestEventTarget",
                )],
            ))
        });
        class.set_property("name", Value::string("XMLHttpRequestEventTarget"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in [
            "onabort",
            "onerror",
            "onload",
            "onloadend",
            "onloadstart",
            "onprogress",
            "ontimeout",
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

pub fn xml_http_request_upload_class() -> Value {
    XHR_UPLOAD_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("Illegal constructor: XMLHttpRequestUpload")],
            ))
        });
        class.set_property("name", Value::string("XMLHttpRequestUpload"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        w3cos_core::class::set_prototype_of(
            &prototype,
            &xml_http_request_event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn upload_value() -> Value {
    let value = Value::object(HashMap::new());
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    for name in [
        "abort",
        "error",
        "load",
        "loadend",
        "loadstart",
        "progress",
        "timeout",
    ] {
        value.set_property(&format!("on{name}"), Value::Null);
    }
    w3cos_core::class::set_prototype_of(
        &value,
        &xml_http_request_upload_class().get_property("prototype"),
    );
    value
}

pub fn xml_http_request_class() -> Value {
    XHR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|this, _| {
            crate::web_events::event_target_class().call(this.clone(), vec![]);
            let state = Rc::new(RefCell::new(XhrState::default()));
            for (name, value) in [
                ("UNSENT", 0.0),
                ("OPENED", 1.0),
                ("HEADERS_RECEIVED", 2.0),
                ("LOADING", 3.0),
                ("DONE", 4.0),
            ] {
                this.set_property(name, Value::Number(value));
            }
            this.set_property("readyState", Value::Number(0.0));
            this.set_property("status", Value::Number(0.0));
            this.set_property("statusText", Value::string(""));
            this.set_property("response", Value::Null);
            this.set_property("responseText", Value::string(""));
            this.set_property("responseURL", Value::string(""));
            this.set_property("responseType", Value::string(""));
            this.set_property("timeout", Value::Number(0.0));
            this.set_property("withCredentials", Value::Bool(false));
            this.set_property("upload", upload_value());
            for name in [
                "readystatechange",
                "loadstart",
                "progress",
                "abort",
                "error",
                "load",
                "timeout",
                "loadend",
            ] {
                this.set_property(&format!("on{name}"), Value::Null);
            }

            let open_state = Rc::clone(&state);
            this.set_property(
                "open",
                Value::function(move |this, args| {
                    let mut state = open_state.borrow_mut();
                    state.method = args
                        .first()
                        .cloned()
                        .unwrap_or_else(|| Value::string("GET"))
                        .to_js_string()
                        .to_uppercase();
                    state.url = args.get(1).cloned().unwrap_or_default().to_js_string();
                    this.set_property("readyState", Value::Number(1.0));
                    dispatch(&this, "readystatechange");
                    Value::Undefined
                }),
            );
            let header_state = Rc::clone(&state);
            this.set_property(
                "setRequestHeader",
                Value::function(move |_, args| {
                    header_state.borrow_mut().request_headers.push((
                        args.first().cloned().unwrap_or_default().to_js_string(),
                        args.get(1).cloned().unwrap_or_default().to_js_string(),
                    ));
                    Value::Undefined
                }),
            );
            let send_state = Rc::clone(&state);
            this.set_property(
                "send",
                Value::function(move |this, args| {
                    dispatch(&this, "loadstart");
                    let upload = this.get_property("upload");
                    dispatch(&upload, "loadstart");
                    let state = send_state.borrow();
                    let headers = w3cos_core::class::construct(
                        &crate::fetch::headers_class(),
                        vec![Value::array(
                            state
                                .request_headers
                                .iter()
                                .map(|(name, value)| {
                                    Value::array(vec![Value::string(name), Value::string(value)])
                                })
                                .collect(),
                        )],
                    );
                    let init = Value::object(HashMap::from([
                        ("method".to_string(), Value::string(&state.method)),
                        ("headers".to_string(), headers),
                        (
                            "body".to_string(),
                            args.first().cloned().unwrap_or(Value::Undefined),
                        ),
                    ]));
                    let url = state.url.clone();
                    drop(state);
                    let response = crate::fetch::fetch_value(vec![Value::string(&url), init]);
                    let status = response.get_property("status");
                    let ok = status.to_u32() > 0;
                    let text = response.call_method("text", vec![]);
                    this.set_property("readyState", Value::Number(4.0));
                    this.set_property("status", status);
                    this.set_property("statusText", response.get_property("statusText"));
                    this.set_property("responseURL", Value::string(&url));
                    this.set_property("responseText", text.clone());
                    let result = if this.get_property("responseType").to_js_string() == "json" {
                        w3cos_core::json::parse(vec![text])
                    } else {
                        text
                    };
                    this.set_property("response", result);
                    send_state.borrow_mut().response_headers = response.get_property("headers");
                    dispatch(&upload, "progress");
                    dispatch(&upload, "load");
                    dispatch(&upload, "loadend");
                    dispatch(&this, "readystatechange");
                    dispatch(&this, "progress");
                    dispatch(&this, if ok { "load" } else { "error" });
                    dispatch(&this, "loadend");
                    Value::Undefined
                }),
            );
            let get_header_state = Rc::clone(&state);
            this.set_property(
                "getResponseHeader",
                Value::function(move |_, args| {
                    get_header_state
                        .borrow()
                        .response_headers
                        .call_method("get", vec![args.first().cloned().unwrap_or_default()])
                }),
            );
            this.set_property(
                "getAllResponseHeaders",
                Value::function(|_, _| Value::string("")),
            );
            this.set_property("overrideMimeType", Value::function(|_, _| Value::Undefined));
            this.set_property(
                "abort",
                Value::function(|this, _| {
                    this.set_property("readyState", Value::Number(0.0));
                    this.set_property("status", Value::Number(0.0));
                    dispatch(&this, "abort");
                    dispatch(&this, "loadend");
                    Value::Undefined
                }),
            );
            Value::Undefined
        });
        for (name, value) in [
            ("UNSENT", 0.0),
            ("OPENED", 1.0),
            ("HEADERS_RECEIVED", 2.0),
            ("LOADING", 3.0),
            ("DONE", 4.0),
        ] {
            class.set_property(name, Value::Number(value));
        }
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for (name, value) in [
            ("DONE", Value::Number(4.0)),
            ("HEADERS_RECEIVED", Value::Number(2.0)),
            ("LOADING", Value::Number(3.0)),
            ("OPENED", Value::Number(1.0)),
            ("UNSENT", Value::Number(0.0)),
            ("abort", Value::Undefined),
            ("getAllResponseHeaders", Value::Undefined),
            ("getResponseHeader", Value::Undefined),
            ("onreadystatechange", Value::Undefined),
            ("open", Value::Undefined),
            ("overrideMimeType", Value::Undefined),
            ("readyState", Value::Undefined),
            ("response", Value::Undefined),
            ("responseText", Value::Undefined),
            ("responseType", Value::Undefined),
            ("responseURL", Value::Undefined),
            ("responseXML", Value::Undefined),
            ("send", Value::Undefined),
            ("setAttributionReporting", Value::Undefined),
            ("setPrivateToken", Value::Undefined),
            ("setRequestHeader", Value::Undefined),
            ("status", Value::Undefined),
            ("statusText", Value::Undefined),
            ("timeout", Value::Undefined),
            ("upload", Value::Undefined),
            ("withCredentials", Value::Undefined),
        ] {
            prototype.set_property(name, value);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &xml_http_request_event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn xhr_and_upload_use_the_standard_event_target_hierarchy() {
        let xhr = w3cos_core::class::construct(&xml_http_request_class(), vec![]);
        let upload = xhr.get_property("upload");
        assert!(w3cos_core::class::instance_of(
            &xhr,
            &xml_http_request_event_target_class()
        ));
        assert!(w3cos_core::class::instance_of(
            &upload,
            &xml_http_request_upload_class()
        ));
        assert!(w3cos_core::class::instance_of(
            &upload,
            &crate::web_events::event_target_class()
        ));

        let calls = Rc::new(Cell::new(0));
        let listener_calls = Rc::clone(&calls);
        upload.call_method(
            "addEventListener",
            vec![
                Value::string("progress"),
                Value::function(move |_, args| {
                    assert!(w3cos_core::class::instance_of(
                        &args[0],
                        &crate::web_events::event_subclass_class("ProgressEvent")
                    ));
                    listener_calls.set(listener_calls.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        dispatch(&upload, "progress");
        assert_eq!(calls.get(), 1);
    }
}
