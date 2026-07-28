//! WebSocketStream and WebTransport compatibility surfaces.
//!
//! The runtime has an RFC 6455 WebSocket client, but it does not yet expose
//! network I/O as Web Streams or provide HTTP/3/QUIC. These newer transports
//! therefore keep their standard object graph and reject connection promises
//! explicitly instead of claiming a connection was established.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use w3cos_core::Value;

use crate::jsdom::{
    WeakRealmObject, disconnect_realm_class, realm_function, register_weak_realm_object,
    upgrade_realm_object,
};

thread_local! {
    static CLASSES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static VALUES: RefCell<Vec<WeakRealmObject>> = const { RefCell::new(Vec::new()) };
    static WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
}

fn realm_transport_function(callback: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    realm_function(crate::jsdom::realm_generation(), callback)
}

fn error(name: &str, message: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, name)
}

fn type_error(message: &str) -> Value {
    w3cos_core::error_instance("TypeError", vec![Value::string(message)])
}

fn throw_type(message: &str) -> ! {
    w3cos_core::throw_value(type_error(message))
}

fn warn_once() {
    WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: WebSocketStream and WebTransport expose compatible stream and \
                 lifecycle objects; WebSocket-to-Streams adaptation and HTTP/3/QUIC transport \
                 require native network adapters"
            );
        }
    });
}

fn unavailable(api: &str) -> Value {
    warn_once();
    w3cos_core::promise::reject(vec![error(
        "NotSupportedError",
        &format!("{api} requires a native streaming transport adapter"),
    )])
}

fn require_url(args: &[Value], api: &str, secure_only: bool) -> String {
    if args.is_empty() {
        throw_type(&format!("{api} requires 1 argument"));
    }
    let url = args[0].to_js_string();
    let accepted = if secure_only {
        url.starts_with("https://")
    } else {
        ["http://", "https://", "ws://", "wss://"]
            .iter()
            .any(|scheme| url.starts_with(scheme))
    };
    if !accepted {
        w3cos_core::throw_value(error(
            "SyntaxError",
            &format!("{api} received an invalid transport URL"),
        ));
    }
    url
}

fn readable() -> Value {
    w3cos_core::class::construct(&crate::streams_web::readable_stream_class(), vec![])
}

fn writable() -> Value {
    w3cos_core::class::construct(&crate::streams_web::writable_stream_class(), vec![])
}

fn set_prototype(value: &Value, name: &'static str) {
    w3cos_core::class::set_prototype_of(value, &class_for(name).get_property("prototype"));
}

fn bidirectional_stream() -> Value {
    let value = Value::object(HashMap::from([
        ("readable".into(), readable()),
        ("writable".into(), writable()),
    ]));
    set_prototype(&value, "WebTransportBidirectionalStream");
    register_weak_realm_object(&VALUES, &value);
    value
}

fn datagram_stream() -> Value {
    let value = Value::object(HashMap::from([
        ("readable".into(), readable()),
        ("writable".into(), writable()),
        ("maxDatagramSize".into(), Value::Number(0.0)),
        ("incomingMaxAge".into(), Value::Null),
        ("outgoingMaxAge".into(), Value::Null),
        ("incomingHighWaterMark".into(), Value::Number(1.0)),
        ("outgoingHighWaterMark".into(), Value::Number(1.0)),
    ]));
    set_prototype(&value, "WebTransportDatagramDuplexStream");
    register_weak_realm_object(&VALUES, &value);
    value
}

fn web_socket_stream(args: Vec<Value>) -> Value {
    let url = require_url(&args, "WebSocketStream", false);
    let value = Value::object(HashMap::from([
        ("url".into(), Value::string(&url)),
        ("opened".into(), unavailable("WebSocketStream.opened")),
        ("closed".into(), unavailable("WebSocketStream.closed")),
    ]));
    value.set_property("close", realm_transport_function(|_, _| Value::Undefined));
    set_prototype(&value, "WebSocketStream");
    register_weak_realm_object(&VALUES, &value);
    value
}

fn web_transport(args: Vec<Value>) -> Value {
    require_url(&args, "WebTransport", true);
    let value = Value::object(HashMap::from([
        ("incomingUnidirectionalStreams".into(), readable()),
        ("incomingBidirectionalStreams".into(), readable()),
        ("datagrams".into(), datagram_stream()),
        ("ready".into(), unavailable("WebTransport.ready")),
        ("closed".into(), unavailable("WebTransport.closed")),
        ("protocol".into(), Value::string("")),
    ]));
    value.set_property("close", realm_transport_function(|_, _| Value::Undefined));
    value.set_property(
        "createBidirectionalStream",
        realm_transport_function(|_, _| unavailable("WebTransport.createBidirectionalStream")),
    );
    value.set_property(
        "createUnidirectionalStream",
        realm_transport_function(|_, _| unavailable("WebTransport.createUnidirectionalStream")),
    );
    set_prototype(&value, "WebTransport");
    register_weak_realm_object(&VALUES, &value);
    value
}

fn web_transport_error(args: Vec<Value>) -> Value {
    let init = args.first().cloned().unwrap_or(Value::Undefined);
    if !init.is_undefined() && !init.is_object() {
        throw_type("WebTransportError init must be an object");
    }
    let message = init.get_property("message").to_js_string();
    let value = error("WebTransportError", &message);
    let source = init.get_property("source");
    value.set_property(
        "source",
        if source.is_undefined() {
            Value::string("stream")
        } else {
            Value::string(&source.to_js_string())
        },
    );
    let code = init.get_property("streamErrorCode");
    value.set_property(
        "streamErrorCode",
        if code.is_undefined() {
            Value::Null
        } else {
            code
        },
    );
    set_prototype(&value, "WebTransportError");
    register_weak_realm_object(&VALUES, &value);
    value
}

fn build_class(name: &'static str) -> Value {
    let constructor = match name {
        "WebSocketStream" => realm_transport_function(|_, args| web_socket_stream(args)),
        "WebTransport" => realm_transport_function(|_, args| web_transport(args)),
        "WebTransportError" => realm_transport_function(|_, args| web_transport_error(args)),
        _ => realm_transport_function(move |_, _| {
            throw_type(&format!("Illegal constructor: {name}"))
        }),
    };
    constructor.set_property("name", Value::string(name));
    let prototype = Value::object(HashMap::from([("constructor".into(), constructor.clone())]));
    let members: &[&str] = match name {
        "WebSocketStream" => &["url", "opened", "closed", "close"],
        "WebTransport" => &[
            "incomingUnidirectionalStreams",
            "incomingBidirectionalStreams",
            "datagrams",
            "ready",
            "closed",
            "close",
            "createBidirectionalStream",
            "createUnidirectionalStream",
            "protocol",
        ],
        "WebTransportBidirectionalStream" => &["readable", "writable"],
        "WebTransportDatagramDuplexStream" => &[
            "readable",
            "writable",
            "maxDatagramSize",
            "incomingMaxAge",
            "outgoingMaxAge",
            "incomingHighWaterMark",
            "outgoingHighWaterMark",
        ],
        "WebTransportError" => &["streamErrorCode", "source"],
        _ => &[],
    };
    for member in members {
        prototype.set_property(member, Value::Undefined);
    }
    if name == "WebTransportError" {
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::unsupported::dom_exception_class().get_property("prototype"),
        );
    }
    constructor.set_property("prototype", prototype);
    constructor
}

pub fn class_for(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = build_class(name);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

pub const INTERFACES: &[&str] = &[
    "WebSocketStream",
    "WebTransport",
    "WebTransportBidirectionalStream",
    "WebTransportDatagramDuplexStream",
    "WebTransportError",
];

pub fn reset() {
    VALUES.with(|values| {
        for value in values
            .borrow_mut()
            .drain(..)
            .filter_map(|value| upgrade_realm_object(&value))
        {
            for reference in [
                "readable",
                "writable",
                "opened",
                "closed",
                "incomingUnidirectionalStreams",
                "incomingBidirectionalStreams",
                "datagrams",
                "ready",
                "close",
                "createBidirectionalStream",
                "createUnidirectionalStream",
                "protocol",
                "source",
                "streamErrorCode",
            ] {
                value.set_property(reference, Value::Undefined);
            }
        }
    });
    let classes = CLASSES.with(|classes| std::mem::take(&mut *classes.borrow_mut()));
    for class in classes.into_values() {
        disconnect_realm_class(class);
    }
    WARNING_EMITTED.with(|warned| warned.set(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_stream_keeps_url_and_rejects_unadapted_transport() {
        reset();
        let stream = w3cos_core::class::construct(
            &class_for("WebSocketStream"),
            vec![Value::string("wss://example.test/socket")],
        );
        assert_eq!(
            stream.get_property("url").to_js_string(),
            "wss://example.test/socket"
        );
        assert!(stream.get_property("opened").is_object());
        assert!(w3cos_core::class::instance_of(
            &stream,
            &class_for("WebSocketStream")
        ));
    }

    #[test]
    fn webtransport_exposes_stream_graph_without_fake_connection() {
        reset();
        let transport = w3cos_core::class::construct(
            &class_for("WebTransport"),
            vec![Value::string("https://example.test/transport")],
        );
        assert!(w3cos_core::class::instance_of(
            &transport.get_property("datagrams"),
            &class_for("WebTransportDatagramDuplexStream")
        ));
        assert!(transport.get_property("ready").is_object());
        assert_eq!(transport.get_property("protocol").to_js_string(), "");
    }

    #[test]
    fn webtransport_error_preserves_standard_fields() {
        reset();
        let value = w3cos_core::class::construct(
            &class_for("WebTransportError"),
            vec![Value::object(HashMap::from([
                ("message".into(), Value::string("reset")),
                ("source".into(), Value::string("session")),
                ("streamErrorCode".into(), Value::Number(7.0)),
            ]))],
        );
        assert_eq!(value.get_property("source").to_js_string(), "session");
        assert_eq!(value.get_property("streamErrorCode").to_number(), 7.0);
    }

    #[test]
    fn transports_stream_references_methods_and_classes_are_realm_owned() {
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_transport_class = class_for("WebTransport");
        let old_datagram_class = class_for("WebTransportDatagramDuplexStream");
        let transport = w3cos_core::class::construct(
            &old_transport_class,
            vec![Value::string("https://example.test/transport")],
        );
        let datagrams = transport.get_property("datagrams");
        let readable = datagrams.get_property("readable");
        let datagrams_weak = crate::jsdom::weak_realm_object(&datagrams);
        let readable_weak = crate::jsdom::weak_realm_object(&readable);
        drop(datagrams);
        drop(readable);

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(old_transport_class.get_property("prototype").is_undefined());
        assert!(old_datagram_class.get_property("prototype").is_undefined());
        assert!(!old_transport_class.strict_eq(&class_for("WebTransport")));
        assert!(transport.get_property("datagrams").is_undefined());
        assert!(transport.get_property("ready").is_undefined());
        assert!(
            transport
                .call_method("createBidirectionalStream", vec![])
                .is_undefined()
        );
        assert!(datagrams_weak.upgrade().is_none());
        assert!(readable_weak.upgrade().is_none());
    }
}
