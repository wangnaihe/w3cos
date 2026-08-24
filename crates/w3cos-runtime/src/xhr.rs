//! Synchronous JavaScript compatibility facade over the Fetch implementation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::jsdom::realm_function;
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
    static XHR_INSTANCES: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
}

fn realm_xhr_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

fn install_realm_event_target(value: &Value) {
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    for method_name in ["addEventListener", "removeEventListener", "dispatchEvent"] {
        let method = value.get_property(method_name);
        value.set_property(
            method_name,
            realm_xhr_function(move |this, args| method.call(this, args)),
        );
    }
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

fn response_document(source: &str, content_type: &str, url: &str) -> Option<Value> {
    let mime_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let parser_content_type = match mime_type.as_str() {
        "text/html" => "text/html",
        "text/xml" => "text/xml",
        "application/xml" => "application/xml",
        "application/xhtml+xml" => "application/xhtml+xml",
        "image/svg+xml" => "image/svg+xml",
        mime_type if mime_type.ends_with("+xml") => "application/xml",
        _ => return None,
    };
    let document = crate::jsdom::parse_frame_document(source, parser_content_type, url);
    document.set_property("contentType", Value::string(&mime_type));
    Some(document)
}

pub fn xml_http_request_event_target_class() -> Value {
    XHR_EVENT_TARGET_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_xhr_function(|_, _| {
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
        let class = realm_xhr_function(|_, _| {
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
    install_realm_event_target(&value);
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
        let class = realm_xhr_function(|this, _| {
            install_realm_event_target(&this);
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
            this.set_property("responseXML", Value::Null);
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
                realm_xhr_function(move |this, args| {
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
                realm_xhr_function(move |_, args| {
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
                realm_xhr_function(move |this, args| {
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
                    let response_headers = response.get_property("headers");
                    let content_type = response_headers
                        .call_method("get", vec![Value::string("content-type")])
                        .to_js_string();
                    let response_type = this.get_property("responseType").to_js_string();
                    this.set_property("readyState", Value::Number(4.0));
                    this.set_property("status", status);
                    this.set_property("statusText", response.get_property("statusText"));
                    this.set_property("responseURL", Value::string(&url));
                    this.set_property("responseText", text.clone());
                    let response_xml = response_document(&text.to_js_string(), &content_type, &url)
                        .unwrap_or(Value::Null);
                    let result = if response_type == "json" {
                        w3cos_core::json::parse(vec![text])
                    } else if response_type == "document" {
                        response_xml.clone()
                    } else {
                        text
                    };
                    this.set_property("response", result);
                    this.set_property("responseXML", response_xml);
                    send_state.borrow_mut().response_headers = response_headers;
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
                realm_xhr_function(move |_, args| {
                    get_header_state
                        .borrow()
                        .response_headers
                        .call_method("get", vec![args.first().cloned().unwrap_or_default()])
                }),
            );
            this.set_property(
                "getAllResponseHeaders",
                realm_xhr_function(|_, _| Value::string("")),
            );
            this.set_property(
                "overrideMimeType",
                realm_xhr_function(|_, _| Value::Undefined),
            );
            this.set_property(
                "abort",
                realm_xhr_function(|this, _| {
                    this.set_property("readyState", Value::Number(0.0));
                    this.set_property("status", Value::Number(0.0));
                    dispatch(&this, "abort");
                    dispatch(&this, "loadend");
                    Value::Undefined
                }),
            );
            XHR_INSTANCES.with(|instances| instances.borrow_mut().push(this));
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

pub fn reset_realm() {
    let instances = XHR_INSTANCES.with(|instances| std::mem::take(&mut *instances.borrow_mut()));
    for instance in instances {
        for property in [
            "onreadystatechange",
            "onloadstart",
            "onprogress",
            "onabort",
            "onerror",
            "onload",
            "ontimeout",
            "onloadend",
        ] {
            instance.set_property(property, Value::Null);
        }
        let upload = instance.get_property("upload");
        for property in [
            "onabort",
            "onerror",
            "onload",
            "onloadend",
            "onloadstart",
            "onprogress",
            "ontimeout",
        ] {
            upload.set_property(property, Value::Null);
        }
        for method in ["addEventListener", "removeEventListener", "dispatchEvent"] {
            upload.set_property(method, Value::Undefined);
        }
        for method in [
            "open",
            "setRequestHeader",
            "send",
            "getResponseHeader",
            "getAllResponseHeaders",
            "overrideMimeType",
            "abort",
            "addEventListener",
            "removeEventListener",
            "dispatchEvent",
        ] {
            instance.set_property(method, Value::Undefined);
        }
        instance.set_property("response", Value::Null);
        instance.set_property("responseXML", Value::Null);
        instance.set_property("upload", Value::Null);
    }
    for slot in [&XHR_CLASS, &XHR_EVENT_TARGET_CLASS, &XHR_UPLOAD_CLASS] {
        slot.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn document_response_uses_the_normalized_response_mime_type() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let document = response_document(
            "<root><child/></root>",
            "Application/Example+XML; charset=utf-8",
            "https://example.test/blob.xml",
        )
        .expect("XML response document");
        assert_eq!(
            document.get_property("contentType").to_js_string(),
            "application/example+xml"
        );
        assert_eq!(
            document.get_property("URL").to_js_string(),
            "https://example.test/blob.xml"
        );
        assert_eq!(
            document
                .get_property("documentElement")
                .get_property("localName")
                .to_js_string(),
            "root"
        );
        assert!(response_document("plain", "text/plain", "about:blank").is_none());
    }

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

    #[test]
    fn xhr_resources_and_methods_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_xhr_class = xml_http_request_class();
        let old_event_target_class = xml_http_request_event_target_class();
        let old_upload_class = xml_http_request_upload_class();
        let xhr = w3cos_core::class::construct(&old_xhr_class, vec![]);
        let upload = xhr.get_property("upload");

        let xhr_marker = Rc::new(());
        let xhr_marker_weak = Rc::downgrade(&xhr_marker);
        xhr.call_method(
            "addEventListener",
            vec![
                Value::string("readystatechange"),
                Value::function(move |_, _| {
                    let _ = &xhr_marker;
                    Value::Undefined
                }),
            ],
        );
        let upload_marker = Rc::new(());
        let upload_marker_weak = Rc::downgrade(&upload_marker);
        upload.call_method(
            "addEventListener",
            vec![
                Value::string("progress"),
                Value::function(move |_, _| {
                    let _ = &upload_marker;
                    Value::Undefined
                }),
            ],
        );
        xhr.call_method(
            "open",
            vec![
                Value::string("GET"),
                Value::string("https://realm.invalid/data"),
            ],
        );
        assert_eq!(xhr.get_property("readyState").to_u32(), 1);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(!old_xhr_class.strict_eq(&xml_http_request_class()));
        assert!(!old_event_target_class.strict_eq(&xml_http_request_event_target_class()));
        assert!(!old_upload_class.strict_eq(&xml_http_request_upload_class()));
        assert!(old_xhr_class.call(Value::Undefined, vec![]).is_undefined());
        for method in ["open", "abort", "dispatchEvent"] {
            assert!(xhr.call_method(method, vec![]).is_undefined());
        }
        assert!(upload.call_method("dispatchEvent", vec![]).is_undefined());
        assert!(xhr.get_property("upload").is_null());
        assert!(xhr.get_property("response").is_null());
        assert!(xhr_marker_weak.upgrade().is_none());
        assert!(upload_marker_weak.upgrade().is_none());

        let fresh = w3cos_core::class::construct(&xml_http_request_class(), vec![]);
        assert!(fresh.get_property("open").is_function());
        reset_realm();
    }
}
