//! Media Source Extensions compatibility with in-memory source buffers.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, realm_function, register_weak_realm_object, reset_realm_class,
    upgrade_realm_object, weak_realm_object,
};

struct SourceBufferRegistration {
    object: WeakRealmObject,
    bytes: Weak<RefCell<Vec<u8>>>,
}

thread_local! {
    static MEDIA_SOURCE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MEDIA_SOURCE_HANDLE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SOURCE_BUFFER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static SOURCE_BUFFER_LIST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
    static MEDIA_SOURCES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static SOURCE_BUFFERS: RefCell<Vec<SourceBufferRegistration>> = const { RefCell::new(Vec::new()) };
    static SOURCE_BUFFER_LISTS: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
}

fn realm_media_source_function(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), f)
}

fn warning() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: MediaSource buffers bytes and emits lifecycle events, but \
                 container parsing, timestamp extraction and native decoder attachment require \
                 a host media pipeline"
            );
        }
    });
}

fn dispatch(target: &Value, event_type: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(event_type)],
    );
    target.call_method("dispatchEvent", vec![event]);
}

fn illegal(name: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(&format!("Illegal constructor: {name}"))],
    ))
}

pub fn source_buffer_list_class() -> Value {
    SOURCE_BUFFER_LIST_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_media_source_function(|_, _| illegal("SourceBufferList"));
        class.set_property("name", Value::string("SourceBufferList"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in ["length", "onaddsourcebuffer", "onremovesourcebuffer"] {
            prototype.set_property(member, Value::Undefined);
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

fn source_buffer_list_value() -> Value {
    let list = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    list.set_property("length", Value::Number(0.0));
    list.set_property("onaddsourcebuffer", Value::Null);
    list.set_property("onremovesourcebuffer", Value::Null);
    w3cos_core::class::set_prototype_of(
        &list,
        &source_buffer_list_class().get_property("prototype"),
    );
    register_weak_realm_object(&SOURCE_BUFFER_LISTS, &list);
    list
}

fn list_add(list: &Value, buffer: Value) {
    let index = list.get_property("length").to_u32();
    list.set_property(&index.to_string(), buffer);
    list.set_property("length", Value::Number((index + 1) as f64));
    dispatch(list, "addsourcebuffer");
}

fn list_remove(list: &Value, buffer: &Value) {
    let length = list.get_property("length").to_u32();
    let mut values = (0..length)
        .filter_map(|index| {
            let value = list.get_property(&index.to_string());
            (!value.strict_eq(buffer)).then_some(value)
        })
        .collect::<Vec<_>>();
    for index in 0..length {
        list.set_property(
            &index.to_string(),
            values
                .get(index as usize)
                .cloned()
                .unwrap_or(Value::Undefined),
        );
    }
    list.set_property("length", Value::Number(values.len() as f64));
    values.clear();
    dispatch(list, "removesourcebuffer");
}

pub fn source_buffer_class() -> Value {
    SOURCE_BUFFER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_media_source_function(|_, _| illegal("SourceBuffer"));
        class.set_property("name", Value::string("SourceBuffer"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "abort",
            "appendBuffer",
            "appendWindowEnd",
            "appendWindowStart",
            "buffered",
            "changeType",
            "mode",
            "onabort",
            "onerror",
            "onupdate",
            "onupdateend",
            "onupdatestart",
            "remove",
            "timestampOffset",
            "updating",
        ] {
            prototype.set_property(member, Value::Undefined);
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

fn source_buffer_value(mime_type: &str) -> Value {
    let bytes = Rc::new(RefCell::new(Vec::<u8>::new()));
    let buffer = w3cos_core::class::construct(&crate::web_events::event_target_class(), Vec::new());
    for (name, value) in [
        ("appendWindowEnd", Value::Number(f64::INFINITY)),
        ("appendWindowStart", Value::Number(0.0)),
        ("buffered", crate::compat_web::time_ranges_value(Vec::new())),
        ("mode", Value::string("segments")),
        ("timestampOffset", Value::Number(0.0)),
        ("updating", Value::Bool(false)),
        ("onabort", Value::Null),
        ("onerror", Value::Null),
        ("onupdate", Value::Null),
        ("onupdateend", Value::Null),
        ("onupdatestart", Value::Null),
        ("__w3cos_mime_type", Value::string(mime_type)),
    ] {
        buffer.set_property(name, value);
    }
    let append_target = buffer.clone();
    let append_bytes = Rc::clone(&bytes);
    buffer.set_property(
        "appendBuffer",
        realm_media_source_function(move |_, args| {
            append_target.set_property("updating", Value::Bool(true));
            dispatch(&append_target, "updatestart");
            let value = args.first().cloned().unwrap_or(Value::Undefined);
            let Some(data) = w3cos_core::binary::bytes_of(&value) else {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string(
                        "SourceBuffer.appendBuffer requires binary data",
                    )],
                ));
            };
            append_bytes.borrow_mut().extend(data);
            warning();
            dispatch(&append_target, "update");
            append_target.set_property("updating", Value::Bool(false));
            dispatch(&append_target, "updateend");
            Value::Undefined
        }),
    );
    let remove_target = buffer.clone();
    buffer.set_property(
        "remove",
        realm_media_source_function(move |_, _| {
            remove_target.set_property("updating", Value::Bool(true));
            dispatch(&remove_target, "updatestart");
            warning();
            remove_target.set_property("updating", Value::Bool(false));
            dispatch(&remove_target, "update");
            dispatch(&remove_target, "updateend");
            Value::Undefined
        }),
    );
    let abort_target = buffer.clone();
    buffer.set_property(
        "abort",
        realm_media_source_function(move |_, _| {
            abort_target.set_property("updating", Value::Bool(false));
            dispatch(&abort_target, "abort");
            dispatch(&abort_target, "updateend");
            Value::Undefined
        }),
    );
    let change_target = buffer.clone();
    buffer.set_property(
        "changeType",
        realm_media_source_function(move |_, args| {
            let mime = args.first().map(Value::to_js_string).unwrap_or_default();
            if mime.is_empty() {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string("SourceBuffer type must not be empty")],
                ));
            }
            change_target.set_property("__w3cos_mime_type", Value::string(&mime));
            warning();
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&buffer, &source_buffer_class().get_property("prototype"));
    SOURCE_BUFFERS.with(|buffers| {
        let mut buffers = buffers.borrow_mut();
        buffers.retain(|buffer| buffer.object.strong_count() != 0);
        buffers.push(SourceBufferRegistration {
            object: weak_realm_object(&buffer),
            bytes: Rc::downgrade(&bytes),
        });
    });
    buffer
}

pub fn media_source_handle_class() -> Value {
    MEDIA_SOURCE_HANDLE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_media_source_function(|_, _| illegal("MediaSourceHandle"));
        class.set_property("name", Value::string("MediaSourceHandle"));
        class.set_property(
            "prototype",
            Value::object(HashMap::from([("constructor".into(), class.clone())])),
        );
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

pub fn media_source_class() -> Value {
    MEDIA_SOURCE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = realm_media_source_function(|this, _| {
            crate::web_events::event_target_class().call(this.clone(), Vec::new());
            let source_buffers = source_buffer_list_value();
            let active_buffers = source_buffer_list_value();
            for (name, value) in [
                ("sourceBuffers", source_buffers.clone()),
                ("activeSourceBuffers", active_buffers.clone()),
                ("duration", Value::Number(f64::NAN)),
                ("readyState", Value::string("closed")),
                ("onsourceclose", Value::Null),
                ("onsourceended", Value::Null),
                ("onsourceopen", Value::Null),
            ] {
                this.set_property(name, value);
            }
            let add_source = this.clone();
            this.set_property(
                "addSourceBuffer",
                realm_media_source_function(move |_, args| {
                    let mime = args.first().map(Value::to_js_string).unwrap_or_default();
                    if mime.is_empty() {
                        w3cos_core::throw_value(w3cos_core::error_instance(
                            "TypeError",
                            vec![Value::string("A MIME type is required")],
                        ));
                    }
                    if add_source.get_property("readyState").to_js_string() == "closed" {
                        warning();
                        add_source.set_property("readyState", Value::string("open"));
                        dispatch(&add_source, "sourceopen");
                    }
                    let buffer = source_buffer_value(&mime);
                    list_add(&add_source.get_property("sourceBuffers"), buffer.clone());
                    list_add(
                        &add_source.get_property("activeSourceBuffers"),
                        buffer.clone(),
                    );
                    buffer
                }),
            );
            let remove_source = this.clone();
            this.set_property(
                "removeSourceBuffer",
                realm_media_source_function(move |_, args| {
                    let buffer = args.first().cloned().unwrap_or(Value::Undefined);
                    list_remove(&remove_source.get_property("sourceBuffers"), &buffer);
                    list_remove(&remove_source.get_property("activeSourceBuffers"), &buffer);
                    Value::Undefined
                }),
            );
            let end_source = this.clone();
            this.set_property(
                "endOfStream",
                realm_media_source_function(move |_, _| {
                    end_source.set_property("readyState", Value::string("ended"));
                    dispatch(&end_source, "sourceended");
                    Value::Undefined
                }),
            );
            for method in ["setLiveSeekableRange", "clearLiveSeekableRange"] {
                this.set_property(
                    method,
                    realm_media_source_function(|_, _| {
                        warning();
                        Value::Undefined
                    }),
                );
            }
            register_weak_realm_object(&MEDIA_SOURCES, &this);
            Value::Undefined
        });
        class.set_property("name", Value::string("MediaSource"));
        class.set_property("canConstructInDedicatedWorker", Value::Bool(false));
        class.set_property(
            "isTypeSupported",
            realm_media_source_function(|_, args| {
                let mime = args.first().map(Value::to_js_string).unwrap_or_default();
                if !mime.is_empty() {
                    warning();
                }
                Value::Bool(false)
            }),
        );
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "activeSourceBuffers",
            "addSourceBuffer",
            "clearLiveSeekableRange",
            "duration",
            "endOfStream",
            "onsourceclose",
            "onsourceended",
            "onsourceopen",
            "readyState",
            "removeSourceBuffer",
            "setLiveSeekableRange",
            "sourceBuffers",
        ] {
            prototype.set_property(member, Value::Undefined);
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

pub fn reset() {
    SOURCE_BUFFERS.with(|buffers| {
        for registration in buffers.borrow_mut().drain(..) {
            if let Some(bytes) = registration.bytes.upgrade() {
                bytes.borrow_mut().clear();
            }
            let Some(buffer) = upgrade_realm_object(&registration.object) else {
                continue;
            };
            buffer.set_property("updating", Value::Bool(false));
            for callback in [
                "onabort",
                "onerror",
                "onupdate",
                "onupdateend",
                "onupdatestart",
            ] {
                buffer.set_property(callback, Value::Null);
            }
            for method in ["abort", "appendBuffer", "changeType", "remove"] {
                buffer.set_property(method, Value::Undefined);
            }
        }
    });
    SOURCE_BUFFER_LISTS.with(|lists| {
        for list in lists
            .borrow_mut()
            .drain(..)
            .filter_map(|list| upgrade_realm_object(&list))
        {
            let length = list.get_property("length").to_u32();
            for index in 0..length {
                list.set_property(&index.to_string(), Value::Undefined);
            }
            list.set_property("length", Value::Number(0.0));
            list.set_property("onaddsourcebuffer", Value::Null);
            list.set_property("onremovesourcebuffer", Value::Null);
        }
    });
    MEDIA_SOURCES.with(|sources| {
        for source in sources
            .borrow_mut()
            .drain(..)
            .filter_map(|source| upgrade_realm_object(&source))
        {
            source.set_property("readyState", Value::string("closed"));
            for callback in ["onsourceclose", "onsourceended", "onsourceopen"] {
                source.set_property(callback, Value::Null);
            }
            for method in [
                "addSourceBuffer",
                "clearLiveSeekableRange",
                "endOfStream",
                "removeSourceBuffer",
                "setLiveSeekableRange",
            ] {
                source.set_property(method, Value::Undefined);
            }
        }
    });
    reset_realm_class(&MEDIA_SOURCE_CLASS);
    reset_realm_class(&MEDIA_SOURCE_HANDLE_CLASS);
    reset_realm_class(&SOURCE_BUFFER_CLASS);
    reset_realm_class(&SOURCE_BUFFER_LIST_CLASS);
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_buffers_store_bytes_and_publish_lifecycle() {
        let source = w3cos_core::class::construct(&media_source_class(), Vec::new());
        let buffer = source.call_method("addSourceBuffer", vec![Value::string("video/webm")]);
        assert_eq!(source.get_property("readyState").to_js_string(), "open");
        buffer.call_method(
            "appendBuffer",
            vec![w3cos_core::binary::typed_array_value(vec![
                Value::Number(1.0),
                Value::Number(2.0),
            ])],
        );
        assert!(!buffer.get_property("updating").to_bool());
        source.call_method("endOfStream", Vec::new());
        assert_eq!(source.get_property("readyState").to_js_string(), "ended");
    }

    #[test]
    fn sources_buffers_lists_callbacks_and_bytes_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_source_class = media_source_class();
        let old_buffer_class = source_buffer_class();
        let source = w3cos_core::class::construct(&old_source_class, Vec::new());
        let buffer = source.call_method("addSourceBuffer", vec![Value::string("video/webm")]);
        let list = source.get_property("sourceBuffers");
        buffer.call_method(
            "appendBuffer",
            vec![w3cos_core::binary::typed_array_value(vec![
                Value::Number(1.0),
                Value::Number(2.0),
            ])],
        );
        let (buffer_object, bytes) = SOURCE_BUFFERS.with(|buffers| {
            let buffers = buffers.borrow();
            let registration = buffers.last().unwrap();
            (registration.object.clone(), registration.bytes.clone())
        });
        assert_eq!(bytes.upgrade().unwrap().borrow().len(), 2);

        let source_marker = Rc::new(());
        let source_marker_weak = Rc::downgrade(&source_marker);
        source.set_property(
            "onsourceopen",
            Value::function(move |_, _| {
                let _ = &source_marker;
                Value::Undefined
            }),
        );
        let buffer_marker = Rc::new(());
        let buffer_marker_weak = Rc::downgrade(&buffer_marker);
        buffer.set_property(
            "onupdate",
            Value::function(move |_, _| {
                let _ = &buffer_marker;
                Value::Undefined
            }),
        );
        let list_marker = Rc::new(());
        let list_marker_weak = Rc::downgrade(&list_marker);
        list.set_property(
            "onremovesourcebuffer",
            Value::function(move |_, _| {
                let _ = &list_marker;
                Value::Undefined
            }),
        );

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(!old_source_class.strict_eq(&media_source_class()));
        assert!(!old_buffer_class.strict_eq(&source_buffer_class()));
        assert!(
            old_source_class
                .call(Value::Undefined, Vec::new())
                .is_undefined()
        );
        assert_eq!(source.get_property("readyState").to_js_string(), "closed");
        assert!(
            source
                .call_method("addSourceBuffer", vec![Value::string("video/webm")])
                .is_undefined()
        );
        assert!(source.get_property("onsourceopen").is_null());
        assert!(buffer.call_method("abort", Vec::new()).is_undefined());
        assert!(buffer.get_property("onupdate").is_null());
        assert_eq!(list.get_property("length").to_number(), 0.0);
        assert!(list.get_property("onremovesourcebuffer").is_null());
        assert!(
            bytes
                .upgrade()
                .is_none_or(|bytes| bytes.borrow().is_empty())
        );
        assert!(source_marker_weak.upgrade().is_none());
        assert!(buffer_marker_weak.upgrade().is_none());
        assert!(list_marker_weak.upgrade().is_none());

        drop(source);
        drop(buffer);
        drop(list);
        assert!(buffer_object.upgrade().is_none());
        assert!(bytes.upgrade().is_none());
    }
}
