//! Compatibility entry points for Web Serial, WebHID and WebUSB.
//!
//! Enumeration is truthful (empty until a host adapter is installed), while
//! chooser operations reject explicitly instead of pretending that access was
//! granted.

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

fn unavailable(api: &'static str, operation: &'static str, warning: &'static Once) -> Value {
    warning.call_once(|| {
        eprintln!(
            "[w3cos] warning: {api} exposes a compatibility surface; native discovery, \
             permission prompts and device I/O require a platform adapter"
        );
    });
    w3cos_core::promise::reject(vec![error(
        "NotFoundError",
        &format!("{operation} could not select a device because no {api} adapter is configured"),
    )])
}

fn illegal_class(name: &'static str, methods: &'static [&'static str]) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
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
        for method in methods {
            let operation = *method;
            prototype.set_property(
                operation,
                Value::function(move |_, _| {
                    w3cos_core::promise::reject(vec![error(
                        "NotSupportedError",
                        &format!("{name}.{operation} requires a platform device adapter"),
                    )])
                }),
            );
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

fn device_event_class(name: &'static str, hid_report: bool) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let class = Value::function(move |this, args| {
            crate::web_events::event_class().call(this.clone(), args.clone());
            let init = args.get(1).cloned().unwrap_or(Value::Undefined);
            this.set_property("device", init.get_property("device"));
            if hid_report {
                this.set_property("reportId", init.get_property("reportId"));
                this.set_property("data", init.get_property("data"));
            }
            Value::Undefined
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("device", Value::Undefined);
        if hid_report {
            prototype.set_property("data", Value::Undefined);
            prototype.set_property("reportId", Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

fn manager_class(name: &'static str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
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
        let (list, request) = match name {
            "Serial" => ("getPorts", "requestPort"),
            _ => ("getDevices", "requestDevice"),
        };
        prototype.set_property(list, Value::function(|_, _| Value::Undefined));
        prototype.set_property(request, Value::function(|_, _| Value::Undefined));
        prototype.set_property("onconnect", Value::Undefined);
        prototype.set_property("ondisconnect", Value::Undefined);
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::web_events::event_target_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.into(), class.clone());
        class
    })
}

fn manager_value(
    class: Value,
    plural: &'static str,
    request: &'static str,
    api: &'static str,
) -> Value {
    let manager = Value::object(HashMap::from([
        (
            plural.into(),
            Value::function(|_, _| w3cos_core::promise::resolve(vec![Value::array(vec![])])),
        ),
        ("onconnect".into(), Value::Null),
        ("ondisconnect".into(), Value::Null),
    ]));
    crate::web_events::event_target_class().call(manager.clone(), vec![]);
    let warning = match api {
        "Web Serial" => &SERIAL_WARNING,
        "WebHID" => &HID_WARNING,
        _ => &USB_WARNING,
    };
    manager.set_property(
        request,
        Value::function(move |_, args| {
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            if matches!(api, "WebHID" | "WebUSB") {
                let filters = options.get_property("filters");
                if options.is_undefined() || filters.is_undefined() {
                    return w3cos_core::promise::reject(vec![error(
                        "TypeError",
                        &format!("{request} requires a filters option"),
                    )]);
                }
            }
            unavailable(api, request, warning)
        }),
    );
    w3cos_core::class::set_prototype_of(&manager, &class.get_property("prototype"));
    manager
}

static SERIAL_WARNING: Once = Once::new();
static HID_WARNING: Once = Once::new();
static USB_WARNING: Once = Once::new();

pub fn serial_class() -> Value {
    manager_class("Serial")
}

pub fn serial_port_class() -> Value {
    let class = illegal_class(
        "SerialPort",
        &[
            "open",
            "close",
            "forget",
            "getInfo",
            "getSignals",
            "setSignals",
        ],
    );
    for property in [
        "connected",
        "onconnect",
        "ondisconnect",
        "readable",
        "writable",
    ] {
        class
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    class
}

pub fn serial_value() -> Value {
    manager_value(serial_class(), "getPorts", "requestPort", "Web Serial")
}

pub fn hid_class() -> Value {
    manager_class("HID")
}

pub fn hid_device_class() -> Value {
    let class = illegal_class(
        "HIDDevice",
        &[
            "open",
            "forget",
            "close",
            "sendReport",
            "sendFeatureReport",
            "receiveFeatureReport",
        ],
    );
    for property in [
        "collections",
        "oninputreport",
        "opened",
        "productId",
        "productName",
        "vendorId",
    ] {
        class
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    class
}

pub fn hid_input_report_event_class() -> Value {
    device_event_class("HIDInputReportEvent", true)
}

pub fn hid_value() -> Value {
    manager_value(hid_class(), "getDevices", "requestDevice", "WebHID")
}

pub fn usb_class() -> Value {
    manager_class("USB")
}

pub fn usb_device_class() -> Value {
    let class = illegal_class(
        "USBDevice",
        &[
            "open",
            "forget",
            "close",
            "selectConfiguration",
            "claimInterface",
            "releaseInterface",
            "selectAlternateInterface",
            "controlTransferIn",
            "controlTransferOut",
            "clearHalt",
            "transferIn",
            "transferOut",
            "isochronousTransferIn",
            "isochronousTransferOut",
            "reset",
        ],
    );
    for property in [
        "configuration",
        "configurations",
        "deviceClass",
        "deviceProtocol",
        "deviceSubclass",
        "deviceVersionMajor",
        "deviceVersionMinor",
        "deviceVersionSubminor",
        "manufacturerName",
        "opened",
        "productId",
        "productName",
        "serialNumber",
        "usbVersionMajor",
        "usbVersionMinor",
        "usbVersionSubminor",
        "vendorId",
    ] {
        class
            .get_property("prototype")
            .set_property(property, Value::Undefined);
    }
    class
}

pub const USB_RECORD_NAMES: &[&str] = &[
    "USBAlternateInterface",
    "USBConfiguration",
    "USBEndpoint",
    "USBInTransferResult",
    "USBInterface",
    "USBIsochronousInTransferPacket",
    "USBIsochronousInTransferResult",
    "USBIsochronousOutTransferPacket",
    "USBIsochronousOutTransferResult",
    "USBOutTransferResult",
];

fn usb_record_members(name: &str) -> &'static [&'static str] {
    match name {
        "USBAlternateInterface" => &[
            "alternateSetting",
            "endpoints",
            "interfaceClass",
            "interfaceName",
            "interfaceProtocol",
            "interfaceSubclass",
        ],
        "USBConfiguration" => &["configurationName", "configurationValue", "interfaces"],
        "USBEndpoint" => &["direction", "endpointNumber", "packetSize", "type"],
        "USBInTransferResult" | "USBIsochronousInTransferPacket" => &["data", "status"],
        "USBInterface" => &["alternate", "alternates", "claimed", "interfaceNumber"],
        "USBIsochronousInTransferResult" => &["data", "packets"],
        "USBIsochronousOutTransferPacket" | "USBOutTransferResult" => &["bytesWritten", "status"],
        "USBIsochronousOutTransferResult" => &["packets"],
        _ => &[],
    }
}

pub fn usb_record_class(name: &str) -> Value {
    CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let Some(name) = USB_RECORD_NAMES
            .iter()
            .copied()
            .find(|candidate| candidate == &name)
        else {
            return Value::Undefined;
        };
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(error("TypeError", &format!("Illegal constructor: {name}")))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in usb_record_members(name) {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

pub fn usb_record_value(name: &str, init: Value) -> Value {
    let value = Value::object(HashMap::new());
    for member in usb_record_members(name) {
        let supplied = init.get_property(member);
        let default = if matches!(
            *member,
            "alternates" | "endpoints" | "interfaces" | "packets"
        ) {
            Value::array(Vec::new())
        } else if *member == "data" || *member == "alternate" {
            Value::Null
        } else if *member == "claimed" {
            Value::Bool(false)
        } else if matches!(
            *member,
            "configurationName" | "direction" | "interfaceName" | "status" | "type"
        ) {
            Value::string("")
        } else {
            Value::Number(0.0)
        };
        value.set_property(
            member,
            if supplied.is_undefined() {
                default
            } else {
                supplied
            },
        );
    }
    w3cos_core::class::set_prototype_of(&value, &usb_record_class(name).get_property("prototype"));
    value
}

pub fn usb_connection_event_class() -> Value {
    device_event_class("USBConnectionEvent", false)
}

pub fn usb_value() -> Value {
    manager_value(usb_class(), "getDevices", "requestDevice", "WebUSB")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn device_managers_enumerate_truthfully_and_reject_unavailable_choosers() {
        for (manager, list, request, needs_filters) in [
            (serial_value(), "getPorts", "requestPort", false),
            (hid_value(), "getDevices", "requestDevice", true),
            (usb_value(), "getDevices", "requestDevice", true),
        ] {
            let log = Rc::new(RefCell::new(Vec::new()));
            let log_for_list = Rc::clone(&log);
            manager.call_method(list, vec![]).call_method(
                "then",
                vec![Value::function(move |_, args| {
                    log_for_list
                        .borrow_mut()
                        .push(args[0].get_property("length").to_js_string());
                    Value::Undefined
                })],
            );
            let options = if needs_filters {
                Value::object(HashMap::from([("filters".into(), Value::array(vec![]))]))
            } else {
                Value::object(HashMap::new())
            };
            let log_for_request = Rc::clone(&log);
            manager.call_method(request, vec![options]).call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    log_for_request
                        .borrow_mut()
                        .push(args[0].get_property("name").to_js_string());
                    Value::Undefined
                })],
            );
            crate::jsdom::drain_microtasks();
            assert_eq!(&*log.borrow(), &["0", "NotFoundError"]);
        }
    }

    #[test]
    fn hid_and_usb_require_filter_options() {
        for manager in [hid_value(), usb_value()] {
            let name = Rc::new(RefCell::new(String::new()));
            let name_for_handler = Rc::clone(&name);
            manager.call_method("requestDevice", vec![]).call_method(
                "catch",
                vec![Value::function(move |_, args| {
                    *name_for_handler.borrow_mut() = args[0].get_property("name").to_js_string();
                    Value::Undefined
                })],
            );
            crate::jsdom::drain_microtasks();
            assert_eq!(&*name.borrow(), "TypeError");
        }
    }

    #[test]
    fn webusb_records_preserve_nested_descriptor_and_transfer_shapes() {
        let endpoint = usb_record_value(
            "USBEndpoint",
            Value::object(HashMap::from([
                ("direction".into(), Value::string("in")),
                ("endpointNumber".into(), Value::Number(1.0)),
                ("packetSize".into(), Value::Number(64.0)),
                ("type".into(), Value::string("bulk")),
            ])),
        );
        let alternate = usb_record_value(
            "USBAlternateInterface",
            Value::object(HashMap::from([(
                "endpoints".into(),
                Value::array(vec![endpoint.clone()]),
            )])),
        );
        let interface = usb_record_value(
            "USBInterface",
            Value::object(HashMap::from([
                ("alternate".into(), alternate.clone()),
                ("alternates".into(), Value::array(vec![alternate])),
                ("claimed".into(), Value::Bool(true)),
            ])),
        );
        let configuration = usb_record_value(
            "USBConfiguration",
            Value::object(HashMap::from([(
                "interfaces".into(),
                Value::array(vec![interface]),
            )])),
        );
        assert!(w3cos_core::class::instance_of(
            &configuration,
            &usb_record_class("USBConfiguration")
        ));
        assert!(w3cos_core::class::instance_of(
            &configuration
                .get_property("interfaces")
                .get_property("0")
                .get_property("alternate")
                .get_property("endpoints")
                .get_property("0"),
            &usb_record_class("USBEndpoint")
        ));
        assert_eq!(endpoint.get_property("packetSize").to_number(), 64.0);

        let result = usb_record_value(
            "USBInTransferResult",
            Value::object(HashMap::from([
                ("data".into(), Value::Null),
                ("status".into(), Value::string("ok")),
            ])),
        );
        assert_eq!(result.get_property("status").to_js_string(), "ok");
        assert!(w3cos_core::class::instance_of(
            &result,
            &usb_record_class("USBInTransferResult")
        ));
    }
}
