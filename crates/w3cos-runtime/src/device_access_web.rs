//! Compatibility entry points for WebHID and WebUSB.
//! `navigator.serial` is implemented in `serial_web` and re-exported here.
//!
//! Web Serial stays on the truthful empty/rejection surface until its adapter
//! lands. WebHID and WebUSB call the desktop `hidapi` / `nusb` adapters when
//! `web-device-access` is enabled. `requestDevice` never grants silently: a
//! host chooser or an explicit `hid` / `usb` grant has to select the device,
//! and a filter that matches nothing rejects `NotFoundError`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

static HID_WARNING: Once = Once::new();
static USB_WARNING: Once = Once::new();

pub fn serial_class() -> Value {
    crate::serial_web::serial_class()
}

pub fn serial_port_class() -> Value {
    crate::serial_web::serial_port_class()
}

pub fn serial_value() -> Value {
    crate::serial_web::serial_value()
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

pub fn hid_connection_event_class() -> Value {
    device_event_class("HIDConnectionEvent", false)
}

pub fn hid_value() -> Value {
    bind_device_manager(hid_class(), "hid")
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
    bind_device_manager(usb_class(), "usb")
}

thread_local! {
    static HID_MANAGERS: RefCell<Vec<Value>> = RefCell::new(Vec::new());
    static USB_MANAGERS: RefCell<Vec<Value>> = RefCell::new(Vec::new());
    static HID_ALLOWED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static USB_ALLOWED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static HID_OBJECTS: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static USB_OBJECTS: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

pub fn reset_realm() {
    crate::hid_adapter::reset();
    crate::usb_adapter::reset();
    HID_MANAGERS.with(|managers| managers.borrow_mut().clear());
    USB_MANAGERS.with(|managers| managers.borrow_mut().clear());
    HID_ALLOWED.with(|allowed| allowed.borrow_mut().clear());
    USB_ALLOWED.with(|allowed| allowed.borrow_mut().clear());
    HID_OBJECTS.with(|objects| objects.borrow_mut().clear());
    USB_OBJECTS.with(|objects| objects.borrow_mut().clear());
}

pub fn poll_device_events() -> usize {
    let mut dispatched = 0;
    dispatched += poll_hid_inputs();
    dispatched += poll_hid_hotplug();
    dispatched += poll_usb_hotplug();
    dispatched
}

pub fn has_pending_device_events() -> bool {
    crate::hid_adapter::any_open()
}

pub fn next_device_deadline() -> Option<std::time::Instant> {
    crate::hid_adapter::any_open()
        .then(|| std::time::Instant::now() + std::time::Duration::from_millis(20))
}

pub fn hid_backend_summary() -> String {
    crate::hid_adapter::backend_summary()
}

pub fn usb_backend_summary() -> String {
    crate::usb_adapter::backend_summary()
}

fn bind_device_manager(class: Value, kind: &'static str) -> Value {
    let manager = Value::object(HashMap::from([
        ("onconnect".into(), Value::Null),
        ("ondisconnect".into(), Value::Null),
    ]));
    crate::web_events::event_target_class().call(manager.clone(), vec![]);
    w3cos_core::class::set_prototype_of(&manager, &class.get_property("prototype"));
    manager.set_property(
        "getDevices",
        Value::function(move |_, _| {
            if kind == "hid" {
                hid_get_devices()
            } else {
                usb_get_devices()
            }
        }),
    );
    manager.set_property(
        "requestDevice",
        Value::function(move |_, args| {
            let options = args.first().cloned().unwrap_or(Value::Undefined);
            if kind == "hid" {
                hid_request_device(options)
            } else {
                usb_request_device(options)
            }
        }),
    );
    if kind == "hid" {
        HID_MANAGERS.with(|managers| managers.borrow_mut().push(manager.clone()));
    } else {
        USB_MANAGERS.with(|managers| managers.borrow_mut().push(manager.clone()));
    }
    manager
}

fn hid_ready() -> bool {
    crate::hid_adapter::using_mock() || crate::hid_adapter::platform_compiled()
}

fn usb_ready() -> bool {
    crate::usb_adapter::using_mock() || crate::usb_adapter::platform_compiled()
}

fn hid_get_devices() -> Value {
    if !hid_ready() {
        return w3cos_core::promise::resolve(vec![Value::array(Vec::new())]);
    }
    let allowed = HID_ALLOWED.with(|allowed| allowed.borrow().clone());
    let devices = crate::hid_adapter::list_devices()
        .into_iter()
        .filter(|device| allowed.contains(&device.id))
        .map(|device| hid_device_value(device))
        .collect();
    w3cos_core::promise::resolve(vec![Value::array(devices)])
}

fn usb_get_devices() -> Value {
    if !usb_ready() {
        return w3cos_core::promise::resolve(vec![Value::array(Vec::new())]);
    }
    let allowed = USB_ALLOWED.with(|allowed| allowed.borrow().clone());
    let devices = crate::usb_adapter::list_devices()
        .into_iter()
        .filter(|device| allowed.contains(&device.id))
        .map(|device| usb_device_value(device))
        .collect();
    w3cos_core::promise::resolve(vec![Value::array(devices)])
}

fn hid_request_device(options: Value) -> Value {
    let filters = match filter_list(&options, "requestDevice") {
        Ok(filters) => filters,
        Err(rejected) => return rejected,
    };
    if !hid_ready() {
        return unavailable("WebHID", "requestDevice", &HID_WARNING);
    }
    let mut parsed = Vec::new();
    let len = filters.get_property("length").to_number().max(0.0) as usize;
    for index in 0..len {
        let filter = filters.get_property(&index.to_string());
        parsed.push(crate::hid_adapter::HidFilter {
            vendor_id: optional_u16(&filter, "vendorId"),
            product_id: optional_u16(&filter, "productId"),
            usage_page: optional_u16(&filter, "usagePage"),
            usage: optional_u16(&filter, "usage"),
        });
    }
    let matched: Vec<_> = crate::hid_adapter::list_devices()
        .into_iter()
        .filter(|device| parsed.is_empty() || parsed.iter().any(|filter| device.matches(filter)))
        .collect();
    if matched.is_empty() {
        return rejected(
            "NotFoundError",
            "requestDevice could not find a matching HID device",
        );
    }
    let chosen = match crate::hid_adapter::choose(&matched) {
        Ok(crate::hid_adapter::HidChoice::Selected(device)) => device,
        Ok(crate::hid_adapter::HidChoice::Dismissed) => {
            return rejected("NotFoundError", "the HID device chooser was dismissed");
        }
        Ok(crate::hid_adapter::HidChoice::NoChooser) => {
            if !crate::capability_gate::is_granted("hid") {
                return rejected("NotAllowedError", "hid permission is not granted");
            }
            matched[0].clone()
        }
        Err(err) => return adapter_rejected(err.name, &err.message),
    };
    HID_ALLOWED.with(|allowed| {
        allowed.borrow_mut().insert(chosen.id.clone());
    });
    w3cos_core::promise::resolve(vec![hid_device_value(chosen)])
}

fn usb_request_device(options: Value) -> Value {
    let filters = match filter_list(&options, "requestDevice") {
        Ok(filters) => filters,
        Err(rejected) => return rejected,
    };
    if !usb_ready() {
        return unavailable("WebUSB", "requestDevice", &USB_WARNING);
    }
    let mut parsed = Vec::new();
    let len = filters.get_property("length").to_number().max(0.0) as usize;
    for index in 0..len {
        let filter = filters.get_property(&index.to_string());
        let serial = filter.get_property("serialNumber");
        parsed.push(crate::usb_adapter::UsbFilter {
            vendor_id: optional_u16(&filter, "vendorId"),
            product_id: optional_u16(&filter, "productId"),
            class_code: optional_u8(&filter, "classCode"),
            subclass_code: optional_u8(&filter, "subclassCode"),
            protocol_code: optional_u8(&filter, "protocolCode"),
            serial_number: if serial.is_undefined() {
                None
            } else {
                Some(serial.to_js_string())
            },
        });
    }
    let matched: Vec<_> = crate::usb_adapter::list_devices()
        .into_iter()
        .filter(|device| parsed.is_empty() || parsed.iter().any(|filter| device.matches(filter)))
        .collect();
    if matched.is_empty() {
        return rejected(
            "NotFoundError",
            "requestDevice could not find a matching USB device",
        );
    }
    let chosen = match crate::usb_adapter::choose(&matched) {
        Ok(crate::usb_adapter::UsbChoice::Selected(device)) => device,
        Ok(crate::usb_adapter::UsbChoice::Dismissed) => {
            return rejected("NotFoundError", "the USB device chooser was dismissed");
        }
        Ok(crate::usb_adapter::UsbChoice::NoChooser) => {
            if !crate::capability_gate::is_granted("usb") {
                return rejected("NotAllowedError", "usb permission is not granted");
            }
            matched[0].clone()
        }
        Err(err) => return adapter_rejected(err.name, &err.message),
    };
    USB_ALLOWED.with(|allowed| {
        allowed.borrow_mut().insert(chosen.id.clone());
    });
    w3cos_core::promise::resolve(vec![usb_device_value(chosen)])
}

fn hid_device_value(info: crate::hid_adapter::HidInfo) -> Value {
    if let Some(existing) = HID_OBJECTS.with(|objects| objects.borrow().get(&info.id).cloned()) {
        existing.set_property("opened", Value::Bool(crate::hid_adapter::is_open(&info.id)));
        return existing;
    }
    let device = Value::object(HashMap::new());
    crate::web_events::event_target_class().call(device.clone(), vec![]);
    w3cos_core::class::set_prototype_of(&device, &hid_device_class().get_property("prototype"));
    device.set_property("__w3cosDeviceId", Value::string(&info.id));
    device.set_property("__w3cosUsesReportIds", Value::Bool(info.uses_report_ids));
    device.set_property("vendorId", Value::Number(f64::from(info.vendor_id)));
    device.set_property("productId", Value::Number(f64::from(info.product_id)));
    device.set_property(
        "productName",
        if info.product_name.is_empty() {
            Value::Null
        } else {
            Value::string(&info.product_name)
        },
    );
    device.set_property("opened", Value::Bool(false));
    device.set_property("oninputreport", Value::Null);
    device.set_property(
        "collections",
        Value::array(info.collections.iter().map(hid_collection_value).collect()),
    );
    device.set_property("open", Value::function(|this, _| hid_open(&this)));
    device.set_property("close", Value::function(|this, _| hid_close(&this)));
    device.set_property("forget", Value::function(|this, _| hid_forget(&this)));
    device.set_property(
        "sendReport",
        Value::function(|this, args| hid_send_report(&this, &args, false)),
    );
    device.set_property(
        "sendFeatureReport",
        Value::function(|this, args| hid_send_report(&this, &args, true)),
    );
    device.set_property(
        "receiveFeatureReport",
        Value::function(|this, args| hid_receive_feature(&this, &args)),
    );
    HID_OBJECTS.with(|objects| {
        objects.borrow_mut().insert(info.id.clone(), device.clone());
    });
    device
}

fn usb_device_value(info: crate::usb_adapter::UsbInfo) -> Value {
    if let Some(existing) = USB_OBJECTS.with(|objects| objects.borrow().get(&info.id).cloned()) {
        apply_usb_info(&existing, &info);
        return existing;
    }
    let device = Value::object(HashMap::new());
    crate::web_events::event_target_class().call(device.clone(), vec![]);
    w3cos_core::class::set_prototype_of(&device, &usb_device_class().get_property("prototype"));
    device.set_property("__w3cosDeviceId", Value::string(&info.id));
    device.set_property("vendorId", Value::Number(f64::from(info.vendor_id)));
    device.set_property("productId", Value::Number(f64::from(info.product_id)));
    device.set_property("deviceClass", Value::Number(f64::from(info.device_class)));
    device.set_property(
        "deviceSubclass",
        Value::Number(f64::from(info.device_subclass)),
    );
    device.set_property(
        "deviceProtocol",
        Value::Number(f64::from(info.device_protocol)),
    );
    let (major, minor, sub) = bcd_parts(info.device_version);
    device.set_property("deviceVersionMajor", Value::Number(major));
    device.set_property("deviceVersionMinor", Value::Number(minor));
    device.set_property("deviceVersionSubminor", Value::Number(sub));
    let (major, minor, sub) = bcd_parts(info.usb_version);
    device.set_property("usbVersionMajor", Value::Number(major));
    device.set_property("usbVersionMinor", Value::Number(minor));
    device.set_property("usbVersionSubminor", Value::Number(sub));
    device.set_property(
        "manufacturerName",
        optional_text(info.manufacturer.as_deref()),
    );
    device.set_property("productName", optional_text(info.product.as_deref()));
    device.set_property("serialNumber", optional_text(info.serial.as_deref()));
    device.set_property("opened", Value::Bool(false));
    apply_usb_info(&device, &info);
    device.set_property("open", Value::function(|this, _| usb_open(&this)));
    device.set_property("close", Value::function(|this, _| usb_close(&this)));
    device.set_property("forget", Value::function(|this, _| usb_forget(&this)));
    device.set_property(
        "selectConfiguration",
        Value::function(|this, args| usb_select_configuration(&this, &args)),
    );
    device.set_property(
        "claimInterface",
        Value::function(|this, args| usb_claim(&this, &args, true)),
    );
    device.set_property(
        "releaseInterface",
        Value::function(|this, args| usb_claim(&this, &args, false)),
    );
    device.set_property(
        "selectAlternateInterface",
        Value::function(|this, args| usb_select_alternate(&this, &args)),
    );
    device.set_property(
        "controlTransferIn",
        Value::function(|this, args| usb_control(&this, &args, true)),
    );
    device.set_property(
        "controlTransferOut",
        Value::function(|this, args| usb_control(&this, &args, false)),
    );
    device.set_property(
        "transferIn",
        Value::function(|this, args| usb_transfer(&this, &args, true)),
    );
    device.set_property(
        "transferOut",
        Value::function(|this, args| usb_transfer(&this, &args, false)),
    );
    device.set_property(
        "clearHalt",
        Value::function(|this, args| usb_clear_halt(&this, &args)),
    );
    device.set_property("reset", Value::function(|this, _| usb_reset(&this)));
    device.set_property(
        "isochronousTransferIn",
        Value::function(|_, _| isochronous_rejected()),
    );
    device.set_property(
        "isochronousTransferOut",
        Value::function(|_, _| isochronous_rejected()),
    );
    USB_OBJECTS.with(|objects| {
        objects.borrow_mut().insert(info.id.clone(), device.clone());
    });
    device
}

fn hid_open(device: &Value) -> Value {
    let id = device_id(device);
    if crate::hid_adapter::is_open(&id) {
        device.set_property("opened", Value::Bool(true));
        return resolved_unit();
    }
    let Some(info) = crate::hid_adapter::list_devices()
        .into_iter()
        .find(|device| device.id == id)
    else {
        return rejected("NotFoundError", "HID device is no longer connected");
    };
    match crate::hid_adapter::open(&info) {
        Ok(()) => {
            device.set_property("opened", Value::Bool(true));
            resolved_unit()
        }
        Err(err) => adapter_rejected(err.name, &err.message),
    }
}

fn hid_close(device: &Value) -> Value {
    let id = device_id(device);
    crate::hid_adapter::close(&id);
    device.set_property("opened", Value::Bool(false));
    resolved_unit()
}

fn hid_forget(device: &Value) -> Value {
    let id = device_id(device);
    HID_ALLOWED.with(|allowed| {
        allowed.borrow_mut().remove(&id);
    });
    crate::hid_adapter::close(&id);
    device.set_property("opened", Value::Bool(false));
    resolved_unit()
}

fn hid_send_report(device: &Value, args: &[Value], feature: bool) -> Value {
    let id = device_id(device);
    let report_id = match require_octet(arg(args, 0), "reportId") {
        Ok(report_id) => report_id,
        Err(rejected) => return rejected,
    };
    let data = match buffer_source(arg(args, 1)) {
        Ok(data) => data,
        Err(rejected) => return rejected,
    };
    let result = if feature {
        crate::hid_adapter::send_feature(&id, report_id, &data)
    } else {
        crate::hid_adapter::send_output(&id, report_id, &data)
    };
    match result {
        Ok(()) => resolved_unit(),
        Err(err) => adapter_rejected(err.name, &err.message),
    }
}

fn hid_receive_feature(device: &Value, args: &[Value]) -> Value {
    let id = device_id(device);
    let report_id = match require_octet(arg(args, 0), "reportId") {
        Ok(report_id) => report_id,
        Err(rejected) => return rejected,
    };
    let length = crate::hid_adapter::list_devices()
        .into_iter()
        .find(|info| info.id == id)
        .map(|info| info.feature_payload_len(report_id))
        .unwrap_or(64);
    match crate::hid_adapter::receive_feature(&id, report_id, length) {
        Ok(bytes) => w3cos_core::promise::resolve(vec![data_view(bytes)]),
        Err(err) => adapter_rejected(err.name, &err.message),
    }
}

fn hid_collection_value(collection: &crate::hid_adapter::HidCollection) -> Value {
    Value::object(HashMap::from([
        (
            "usagePage".into(),
            Value::Number(f64::from(collection.usage_page)),
        ),
        ("usage".into(), Value::Number(f64::from(collection.usage))),
        (
            "type".into(),
            Value::Number(f64::from(collection.collection_type)),
        ),
        (
            "children".into(),
            Value::array(
                collection
                    .children
                    .iter()
                    .map(hid_collection_value)
                    .collect(),
            ),
        ),
        (
            "inputReports".into(),
            Value::array(
                collection
                    .input_reports
                    .iter()
                    .map(hid_report_value)
                    .collect(),
            ),
        ),
        (
            "outputReports".into(),
            Value::array(
                collection
                    .output_reports
                    .iter()
                    .map(hid_report_value)
                    .collect(),
            ),
        ),
        (
            "featureReports".into(),
            Value::array(
                collection
                    .feature_reports
                    .iter()
                    .map(hid_report_value)
                    .collect(),
            ),
        ),
    ]))
}

fn hid_report_value(report: &crate::hid_adapter::HidReport) -> Value {
    Value::object(HashMap::from([
        (
            "reportId".into(),
            Value::Number(f64::from(report.report_id)),
        ),
        (
            "items".into(),
            Value::array(report.items.iter().map(hid_report_item_value).collect()),
        ),
    ]))
}

fn hid_report_item_value(item: &crate::hid_adapter::HidReportItem) -> Value {
    Value::object(HashMap::from([
        ("isAbsolute".into(), Value::Bool(item.is_absolute)),
        ("isArray".into(), Value::Bool(item.is_array)),
        ("isRange".into(), Value::Bool(item.is_range)),
        (
            "reportSize".into(),
            Value::Number(f64::from(item.report_size)),
        ),
        (
            "reportCount".into(),
            Value::Number(f64::from(item.report_count)),
        ),
        (
            "usages".into(),
            Value::array(
                item.usages
                    .iter()
                    .map(|usage| Value::Number(f64::from(*usage)))
                    .collect(),
            ),
        ),
        (
            "usageMinimum".into(),
            Value::Number(f64::from(item.usage_minimum)),
        ),
        (
            "usageMaximum".into(),
            Value::Number(f64::from(item.usage_maximum)),
        ),
    ]))
}

fn usb_open(device: &Value) -> Value {
    let id = device_id(device);
    if crate::usb_adapter::is_open(&id) {
        device.set_property("opened", Value::Bool(true));
        return resolved_unit();
    }
    match crate::usb_adapter::open(&id) {
        Ok(info) => {
            apply_usb_info(device, &info);
            device.set_property("opened", Value::Bool(true));
            resolved_unit()
        }
        Err(err) => adapter_rejected(err.name, &err.message),
    }
}

fn usb_close(device: &Value) -> Value {
    let id = device_id(device);
    crate::usb_adapter::close(&id);
    device.set_property("opened", Value::Bool(false));
    resolved_unit()
}

fn usb_forget(device: &Value) -> Value {
    let id = device_id(device);
    USB_ALLOWED.with(|allowed| {
        allowed.borrow_mut().remove(&id);
    });
    crate::usb_adapter::close(&id);
    device.set_property("opened", Value::Bool(false));
    resolved_unit()
}

fn usb_select_configuration(device: &Value, args: &[Value]) -> Value {
    let id = device_id(device);
    let value = match require_octet(arg(args, 0), "configurationValue") {
        Ok(value) => value,
        Err(rejected) => return rejected,
    };
    match crate::usb_adapter::select_configuration(&id, value) {
        Ok(info) => {
            apply_usb_info(device, &info);
            resolved_unit()
        }
        Err(err) => adapter_rejected(err.name, &err.message),
    }
}

fn usb_claim(device: &Value, args: &[Value], claim: bool) -> Value {
    let id = device_id(device);
    let number = match require_octet(arg(args, 0), "interfaceNumber") {
        Ok(number) => number,
        Err(rejected) => return rejected,
    };
    let result = if claim {
        crate::usb_adapter::claim_interface(&id, number)
    } else {
        crate::usb_adapter::release_interface(&id, number)
    };
    match result {
        Ok(()) => {
            set_interface_claimed(device, number, claim);
            resolved_unit()
        }
        Err(err) => adapter_rejected(err.name, &err.message),
    }
}

fn usb_select_alternate(device: &Value, args: &[Value]) -> Value {
    let id = device_id(device);
    let number = match require_octet(arg(args, 0), "interfaceNumber") {
        Ok(number) => number,
        Err(rejected) => return rejected,
    };
    let setting = match require_octet(arg(args, 1), "alternateSetting") {
        Ok(setting) => setting,
        Err(rejected) => return rejected,
    };
    match crate::usb_adapter::select_alternate(&id, number, setting) {
        Ok(()) => {
            set_interface_alternate(device, number, setting);
            resolved_unit()
        }
        Err(err) => adapter_rejected(err.name, &err.message),
    }
}

fn usb_control(device: &Value, args: &[Value], incoming: bool) -> Value {
    let id = device_id(device);
    let setup = match control_setup(arg(args, 0)) {
        Ok(setup) => setup,
        Err(rejected) => return rejected,
    };
    if incoming {
        let length = match require_length(arg(args, 1)) {
            Ok(length) => length,
            Err(rejected) => return rejected,
        };
        match crate::usb_adapter::control_in(&id, setup, length) {
            Ok(transfer) => w3cos_core::promise::resolve(vec![in_transfer_result(transfer)]),
            Err(err) => adapter_rejected(err.name, &err.message),
        }
    } else {
        let data = match buffer_source(arg(args, 1)) {
            Ok(data) => data,
            Err(rejected) => return rejected,
        };
        match crate::usb_adapter::control_out(&id, setup, &data) {
            Ok(transfer) => w3cos_core::promise::resolve(vec![out_transfer_result(transfer)]),
            Err(err) => adapter_rejected(err.name, &err.message),
        }
    }
}

fn usb_transfer(device: &Value, args: &[Value], incoming: bool) -> Value {
    let id = device_id(device);
    let endpoint = match require_octet(arg(args, 0), "endpointNumber") {
        Ok(endpoint) => endpoint,
        Err(rejected) => return rejected,
    };
    if incoming {
        let length = match require_length(arg(args, 1)) {
            Ok(length) => length,
            Err(rejected) => return rejected,
        };
        match crate::usb_adapter::transfer_in(&id, endpoint, length) {
            Ok(transfer) => w3cos_core::promise::resolve(vec![in_transfer_result(transfer)]),
            Err(err) => adapter_rejected(err.name, &err.message),
        }
    } else {
        let data = match buffer_source(arg(args, 1)) {
            Ok(data) => data,
            Err(rejected) => return rejected,
        };
        match crate::usb_adapter::transfer_out(&id, endpoint, &data) {
            Ok(transfer) => w3cos_core::promise::resolve(vec![out_transfer_result(transfer)]),
            Err(err) => adapter_rejected(err.name, &err.message),
        }
    }
}

fn usb_clear_halt(device: &Value, args: &[Value]) -> Value {
    let id = device_id(device);
    let direction = arg(args, 0).to_js_string();
    let incoming = match direction.as_str() {
        "in" => true,
        "out" => false,
        _ => {
            return rejected("TypeError", "clearHalt direction must be \"in\" or \"out\"");
        }
    };
    let endpoint = match require_octet(arg(args, 1), "endpointNumber") {
        Ok(endpoint) => endpoint,
        Err(rejected) => return rejected,
    };
    match crate::usb_adapter::clear_halt(&id, incoming, endpoint) {
        Ok(()) => resolved_unit(),
        Err(err) => adapter_rejected(err.name, &err.message),
    }
}

fn usb_reset(device: &Value) -> Value {
    let id = device_id(device);
    match crate::usb_adapter::reset_device(&id) {
        Ok(info) => {
            for_each_interface(device, |interface| {
                interface.set_property("claimed", Value::Bool(false));
            });
            apply_usb_info(device, &info);
            resolved_unit()
        }
        Err(err) => adapter_rejected(err.name, &err.message),
    }
}

static ISOCHRONOUS_WARNING: Once = Once::new();

fn isochronous_rejected() -> Value {
    ISOCHRONOUS_WARNING.call_once(|| {
        eprintln!(
            "[w3cos] warning: WebUSB isochronous transfers are not supported by the nusb adapter"
        );
    });
    rejected(
        "NotSupportedError",
        "WebUSB isochronous transfers are not supported",
    )
}

fn apply_usb_info(device: &Value, info: &crate::usb_adapter::UsbInfo) {
    device.set_property("opened", Value::Bool(crate::usb_adapter::is_open(&info.id)));
    let configurations = ensure_configurations(device, info);
    let active = info
        .active_configuration
        .map(|value| configuration_by_value(&configurations, value))
        .unwrap_or(Value::Null);
    device.set_property(
        "configuration",
        if active.is_undefined() {
            Value::Null
        } else {
            active
        },
    );
}

fn ensure_configurations(device: &Value, info: &crate::usb_adapter::UsbInfo) -> Value {
    let existing = device.get_property("configurations");
    let existing_len = if existing.is_array() {
        existing.get_property("length").to_number()
    } else {
        -1.0
    };
    if existing_len > 0.0 || (existing_len == 0.0 && info.configurations.is_empty()) {
        return existing;
    }
    let built = Value::array(
        info.configurations
            .iter()
            .map(usb_configuration_value)
            .collect(),
    );
    device.set_property("configurations", built.clone());
    built
}

fn usb_configuration_value(configuration: &crate::usb_adapter::UsbConfigurationInfo) -> Value {
    usb_record_value(
        "USBConfiguration",
        Value::object(HashMap::from([
            (
                "configurationValue".into(),
                Value::Number(f64::from(configuration.value)),
            ),
            (
                "configurationName".into(),
                optional_text(configuration.name.as_deref()),
            ),
            (
                "interfaces".into(),
                Value::array(
                    configuration
                        .interfaces
                        .iter()
                        .map(usb_interface_value)
                        .collect(),
                ),
            ),
        ])),
    )
}

fn usb_interface_value(interface: &crate::usb_adapter::UsbInterfaceInfo) -> Value {
    let alternates: Vec<Value> = interface
        .alternates
        .iter()
        .map(usb_alternate_value)
        .collect();
    let alternate = alternates.first().cloned().unwrap_or(Value::Null);
    usb_record_value(
        "USBInterface",
        Value::object(HashMap::from([
            (
                "interfaceNumber".into(),
                Value::Number(f64::from(interface.number)),
            ),
            ("claimed".into(), Value::Bool(false)),
            ("alternate".into(), alternate),
            ("alternates".into(), Value::array(alternates)),
        ])),
    )
}

fn usb_alternate_value(alternate: &crate::usb_adapter::UsbAlternateInfo) -> Value {
    usb_record_value(
        "USBAlternateInterface",
        Value::object(HashMap::from([
            (
                "alternateSetting".into(),
                Value::Number(f64::from(alternate.setting)),
            ),
            (
                "interfaceClass".into(),
                Value::Number(f64::from(alternate.class)),
            ),
            (
                "interfaceSubclass".into(),
                Value::Number(f64::from(alternate.subclass)),
            ),
            (
                "interfaceProtocol".into(),
                Value::Number(f64::from(alternate.protocol)),
            ),
            (
                "interfaceName".into(),
                optional_text(alternate.name.as_deref()),
            ),
            (
                "endpoints".into(),
                Value::array(alternate.endpoints.iter().map(usb_endpoint_value).collect()),
            ),
        ])),
    )
}

fn usb_endpoint_value(endpoint: &crate::usb_adapter::UsbEndpointInfo) -> Value {
    let direction = if endpoint.address & 0x80 == 0 {
        "out"
    } else {
        "in"
    };
    let kind = match endpoint.kind {
        crate::usb_adapter::UsbEndpointKind::Bulk => "bulk",
        crate::usb_adapter::UsbEndpointKind::Interrupt => "interrupt",
        crate::usb_adapter::UsbEndpointKind::Isochronous => "isochronous",
    };
    usb_record_value(
        "USBEndpoint",
        Value::object(HashMap::from([
            ("direction".into(), Value::string(direction)),
            (
                "endpointNumber".into(),
                Value::Number(f64::from(endpoint.address & 0x0F)),
            ),
            (
                "packetSize".into(),
                Value::Number(f64::from(endpoint.packet_size)),
            ),
            ("type".into(), Value::string(kind)),
        ])),
    )
}

fn configuration_by_value(configurations: &Value, value: u8) -> Value {
    let len = configurations.get_property("length").to_number().max(0.0) as usize;
    for index in 0..len {
        let configuration = configurations.get_property(&index.to_string());
        if configuration.get_property("configurationValue").to_number() as u8 == value {
            return configuration;
        }
    }
    Value::Null
}

fn set_interface_claimed(device: &Value, number: u8, claimed: bool) {
    let configuration = device.get_property("configuration");
    let interfaces = configuration.get_property("interfaces");
    let len = interfaces.get_property("length").to_number().max(0.0) as usize;
    for index in 0..len {
        let interface = interfaces.get_property(&index.to_string());
        if interface.get_property("interfaceNumber").to_number() as u8 == number {
            interface.set_property("claimed", Value::Bool(claimed));
        }
    }
}

fn set_interface_alternate(device: &Value, number: u8, setting: u8) {
    let configuration = device.get_property("configuration");
    let interfaces = configuration.get_property("interfaces");
    let len = interfaces.get_property("length").to_number().max(0.0) as usize;
    for index in 0..len {
        let interface = interfaces.get_property(&index.to_string());
        if interface.get_property("interfaceNumber").to_number() as u8 != number {
            continue;
        }
        let alternates = interface.get_property("alternates");
        let alternate_len = alternates.get_property("length").to_number().max(0.0) as usize;
        for alternate_index in 0..alternate_len {
            let alternate = alternates.get_property(&alternate_index.to_string());
            if alternate.get_property("alternateSetting").to_number() as u8 == setting {
                interface.set_property("alternate", alternate);
            }
        }
    }
}

fn for_each_interface(device: &Value, mut body: impl FnMut(&Value)) {
    let configurations = device.get_property("configurations");
    let config_len = configurations.get_property("length").to_number().max(0.0) as usize;
    for index in 0..config_len {
        let interfaces = configurations
            .get_property(&index.to_string())
            .get_property("interfaces");
        let len = interfaces.get_property("length").to_number().max(0.0) as usize;
        for interface_index in 0..len {
            body(&interfaces.get_property(&interface_index.to_string()));
        }
    }
}

fn in_transfer_result(transfer: crate::usb_adapter::InTransfer) -> Value {
    let data = match transfer.data {
        Some(bytes) => data_view(bytes),
        None => Value::Null,
    };
    usb_record_value(
        "USBInTransferResult",
        Value::object(HashMap::from([
            ("data".into(), data),
            (
                "status".into(),
                Value::string(transfer_status(transfer.status)),
            ),
        ])),
    )
}

fn out_transfer_result(transfer: crate::usb_adapter::OutTransfer) -> Value {
    usb_record_value(
        "USBOutTransferResult",
        Value::object(HashMap::from([
            (
                "bytesWritten".into(),
                Value::Number(transfer.bytes_written as f64),
            ),
            (
                "status".into(),
                Value::string(transfer_status(transfer.status)),
            ),
        ])),
    )
}

fn transfer_status(status: crate::usb_adapter::TransferStatus) -> &'static str {
    match status {
        crate::usb_adapter::TransferStatus::Ok => "ok",
        crate::usb_adapter::TransferStatus::Stall => "stall",
        crate::usb_adapter::TransferStatus::Babble => "babble",
    }
}

fn poll_hid_inputs() -> usize {
    let mut dispatched = 0;
    for batch in crate::hid_adapter::poll_inputs() {
        let Some(device) = HID_OBJECTS.with(|objects| objects.borrow().get(&batch.id).cloned())
        else {
            continue;
        };
        let uses_report_ids = device.get_property("__w3cosUsesReportIds").to_bool();
        for raw in batch.reports {
            let (report_id, data) = crate::hid_adapter::split_input_report(uses_report_ids, &raw);
            let event = w3cos_core::class::construct(
                &hid_input_report_event_class(),
                vec![
                    Value::string("inputreport"),
                    Value::object(HashMap::from([
                        ("device".into(), device.clone()),
                        ("reportId".into(), Value::Number(f64::from(report_id))),
                        ("data".into(), data_view(data)),
                    ])),
                ],
            );
            device.call_method("dispatchEvent", vec![event]);
            dispatched += 1;
        }
        if batch.lost {
            device.set_property("opened", Value::Bool(false));
            dispatch_managers("hid", "disconnect", device);
            dispatched += 1;
        }
    }
    dispatched
}

fn poll_hid_hotplug() -> usize {
    let allowed = HID_ALLOWED.with(|allowed| allowed.borrow().clone());
    let mut dispatched = 0;
    for event in crate::hid_adapter::poll_hotplug(&allowed) {
        match event {
            crate::hid_adapter::HotplugEvent::Connected(info) => {
                let device = hid_device_value(info);
                device.set_property("opened", Value::Bool(false));
                dispatch_managers("hid", "connect", device);
                dispatched += 1;
            }
            crate::hid_adapter::HotplugEvent::Disconnected(id) => {
                crate::hid_adapter::close(&id);
                if let Some(device) = HID_OBJECTS.with(|objects| objects.borrow().get(&id).cloned())
                {
                    device.set_property("opened", Value::Bool(false));
                    dispatch_managers("hid", "disconnect", device);
                    dispatched += 1;
                }
            }
        }
    }
    dispatched
}

fn poll_usb_hotplug() -> usize {
    let allowed = USB_ALLOWED.with(|allowed| allowed.borrow().clone());
    let mut dispatched = 0;
    for event in crate::usb_adapter::poll_hotplug(&allowed) {
        match event {
            crate::usb_adapter::HotplugEvent::Connected(info) => {
                let device = usb_device_value(info);
                device.set_property("opened", Value::Bool(false));
                dispatch_managers("usb", "connect", device);
                dispatched += 1;
            }
            crate::usb_adapter::HotplugEvent::Disconnected(id) => {
                if let Some(device) = USB_OBJECTS.with(|objects| objects.borrow().get(&id).cloned())
                {
                    device.set_property("opened", Value::Bool(false));
                    dispatch_managers("usb", "disconnect", device);
                    dispatched += 1;
                }
            }
        }
    }
    dispatched
}

fn dispatch_managers(kind: &str, event_type: &str, device: Value) {
    let managers = if kind == "hid" {
        HID_MANAGERS.with(|managers| managers.borrow().clone())
    } else {
        USB_MANAGERS.with(|managers| managers.borrow().clone())
    };
    let class = if kind == "hid" {
        hid_connection_event_class()
    } else {
        usb_connection_event_class()
    };
    for manager in managers {
        let event = w3cos_core::class::construct(
            &class,
            vec![
                Value::string(event_type),
                Value::object(HashMap::from([("device".into(), device.clone())])),
            ],
        );
        manager.call_method("dispatchEvent", vec![event]);
    }
}

fn filter_list(options: &Value, request: &str) -> Result<Value, Value> {
    let filters = options.get_property("filters");
    if options.is_undefined() || filters.is_undefined() {
        return Err(rejected(
            "TypeError",
            &format!("{request} requires a filters option"),
        ));
    }
    if !filters.is_array() {
        return Err(rejected(
            "TypeError",
            &format!("{request} filters must be an array"),
        ));
    }
    Ok(filters)
}

fn device_id(device: &Value) -> String {
    device.get_property("__w3cosDeviceId").to_js_string()
}

fn arg<'a>(args: &'a [Value], index: usize) -> &'a Value {
    args.get(index).unwrap_or(&Value::Undefined)
}

fn optional_u16(object: &Value, name: &str) -> Option<u16> {
    let value = object.get_property(name);
    if value.is_undefined() || value.is_null() {
        return None;
    }
    let number = value.to_number();
    if !number.is_finite() {
        return None;
    }
    Some(number.clamp(0.0, 65535.0) as u16)
}

fn optional_u8(object: &Value, name: &str) -> Option<u8> {
    optional_u16(object, name).map(|value| value as u8)
}

fn optional_text(text: Option<&str>) -> Value {
    match text {
        Some(text) if !text.is_empty() => Value::string(text),
        _ => Value::Null,
    }
}

fn bcd_parts(bcd: u16) -> (f64, f64, f64) {
    (
        f64::from(bcd >> 8),
        f64::from((bcd >> 4) & 0x0F),
        f64::from(bcd & 0x0F),
    )
}

fn require_octet(value: &Value, name: &str) -> Result<u8, Value> {
    if value.is_undefined() {
        return Err(rejected("TypeError", &format!("{name} must be an octet")));
    }
    let number = value.to_number();
    if !number.is_finite() {
        return Err(rejected("TypeError", &format!("{name} must be an octet")));
    }
    Ok(number.rem_euclid(256.0) as u8)
}

fn require_u16(value: &Value, name: &str) -> Result<u16, Value> {
    if value.is_undefined() {
        return Err(rejected(
            "TypeError",
            &format!("{name} must be an unsigned short"),
        ));
    }
    let number = value.to_number();
    if !number.is_finite() {
        return Err(rejected(
            "TypeError",
            &format!("{name} must be an unsigned short"),
        ));
    }
    Ok(number.rem_euclid(65536.0) as u16)
}

fn require_length(value: &Value) -> Result<usize, Value> {
    let number = value.to_number();
    if !number.is_finite() || number < 0.0 {
        return Err(rejected(
            "TypeError",
            "length must be a non-negative number",
        ));
    }
    Ok(number as usize)
}

fn buffer_source(value: &Value) -> Result<Vec<u8>, Value> {
    if value.is_undefined() {
        return Ok(Vec::new());
    }
    w3cos_core::binary::bytes_of(value)
        .ok_or_else(|| rejected("TypeError", "expected a BufferSource"))
}

fn data_view(bytes: Vec<u8>) -> Value {
    w3cos_core::class::construct(
        &w3cos_core::binary::data_view_class(),
        vec![w3cos_core::binary::array_buffer_value(bytes)],
    )
}

fn control_setup(value: &Value) -> Result<crate::usb_adapter::ControlSetup, Value> {
    if value.is_undefined() || value.is_null() {
        return Err(rejected("TypeError", "control transfer setup is required"));
    }
    let control_class = match value.get_property("requestType").to_js_string().as_str() {
        "standard" => crate::usb_adapter::ControlClass::Standard,
        "class" => crate::usb_adapter::ControlClass::Class,
        "vendor" => crate::usb_adapter::ControlClass::Vendor,
        _ => {
            return Err(rejected(
                "TypeError",
                "requestType must be standard, class, or vendor",
            ));
        }
    };
    let recipient = match value.get_property("recipient").to_js_string().as_str() {
        "device" => crate::usb_adapter::ControlTarget::Device,
        "interface" => crate::usb_adapter::ControlTarget::Interface,
        "endpoint" => crate::usb_adapter::ControlTarget::Endpoint,
        "other" => crate::usb_adapter::ControlTarget::Other,
        _ => {
            return Err(rejected(
                "TypeError",
                "recipient must be device, interface, endpoint, or other",
            ));
        }
    };
    Ok(crate::usb_adapter::ControlSetup {
        control_class,
        recipient,
        request: require_octet(&value.get_property("request"), "request")?,
        value: require_u16(&value.get_property("value"), "value")?,
        index: require_u16(&value.get_property("index"), "index")?,
    })
}

fn rejected(name: &str, message: &str) -> Value {
    w3cos_core::promise::reject(vec![error(name, message)])
}

fn resolved_unit() -> Value {
    w3cos_core::promise::resolve(vec![Value::Undefined])
}

fn adapter_rejected(name: &str, message: &str) -> Value {
    rejected(name, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn isolate() {
        reset_realm();
        crate::capability_gate::reset();
    }

    fn settle(promise: Value) -> Result<Value, String> {
        let ok = Rc::new(RefCell::new(None));
        let err = Rc::new(RefCell::new(None));
        let ok_for_then = Rc::clone(&ok);
        let err_for_catch = Rc::clone(&err);
        promise.call_method(
            "then",
            vec![Value::function(move |_, args| {
                *ok_for_then.borrow_mut() = args.first().cloned();
                Value::Undefined
            })],
        );
        promise.call_method(
            "catch",
            vec![Value::function(move |_, args| {
                *err_for_catch.borrow_mut() = args.first().map(|value| {
                    let name = value.get_property("name").to_js_string();
                    if name.is_empty() {
                        value.to_js_string()
                    } else {
                        name
                    }
                });
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        if let Some(error) = err.borrow().clone() {
            return Err(error);
        }
        ok.borrow().clone().ok_or_else(|| "unresolved".into())
    }

    fn view_bytes(value: &Value) -> Vec<u8> {
        w3cos_core::binary::bytes_of(value).unwrap_or_else(|| {
            w3cos_core::binary::bytes_of(&value.get_property("buffer")).unwrap_or_default()
        })
    }

    fn filters(entries: Vec<Value>) -> Value {
        Value::object(HashMap::from([("filters".into(), Value::array(entries))]))
    }

    fn mouse(id: &str, vendor: u16) -> crate::hid_adapter::MockHidSpec {
        crate::hid_adapter::MockHidSpec {
            id: id.into(),
            vendor_id: vendor,
            product_id: 1,
            product_name: "mouse".into(),
            serial_number: String::new(),
            usage_page: 0x01,
            usage: 0x02,
            descriptor: vec![
                0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00,
                0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
                0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02,
                0x81, 0x06, 0xC0,
            ],
        }
    }

    fn gadget(id: &str, class: u8) -> crate::usb_adapter::MockUsbSpec {
        use crate::usb_adapter::{
            InterfaceClass, MockIn, MockUsbSpec, TransferStatus, UsbAlternateInfo,
            UsbConfigurationInfo, UsbEndpointInfo, UsbEndpointKind, UsbInfo, UsbInterfaceInfo,
        };
        use std::collections::VecDeque;
        MockUsbSpec {
            info: UsbInfo {
                id: id.into(),
                vendor_id: 0x1111,
                product_id: 0x2222,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                device_version: 0x0110,
                usb_version: 0x0200,
                manufacturer: Some("w3cos".into()),
                product: Some("gadget".into()),
                serial: Some(id.into()),
                interface_classes: vec![InterfaceClass {
                    number: 0,
                    class,
                    subclass: 1,
                    protocol: 2,
                }],
                configurations: vec![UsbConfigurationInfo {
                    value: 1,
                    name: Some("cfg".into()),
                    interfaces: vec![UsbInterfaceInfo {
                        number: 0,
                        alternates: vec![UsbAlternateInfo {
                            setting: 0,
                            class,
                            subclass: 1,
                            protocol: 2,
                            name: None,
                            endpoints: vec![
                                UsbEndpointInfo {
                                    address: 0x81,
                                    packet_size: 64,
                                    kind: UsbEndpointKind::Bulk,
                                },
                                UsbEndpointInfo {
                                    address: 0x01,
                                    packet_size: 64,
                                    kind: UsbEndpointKind::Bulk,
                                },
                            ],
                        }],
                    }],
                }],
                active_configuration: Some(1),
            },
            inputs: VecDeque::from([MockIn::Data(vec![1, 2, 3]), MockIn::Stall]),
            output_status: VecDeque::from([TransferStatus::Ok, TransferStatus::Stall]),
        }
    }

    #[test]
    fn device_managers_enumerate_truthfully_and_reject_unavailable_choosers() {
        isolate();
        for (manager, list, request, needs_filters) in [
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
        isolate();
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
    fn hid_request_requires_grant_or_chooser_and_honors_filters() {
        isolate();
        crate::hid_adapter::set_mock_devices(vec![
            mouse("mouse-a", 0x1111),
            mouse("mouse-b", 0x2222),
        ]);
        let manager = hid_value();
        let denied = settle(manager.call_method("requestDevice", vec![filters(vec![])]));
        assert_eq!(denied.unwrap_err(), "NotAllowedError");

        crate::capability_gate::grant("hid");
        let missing = settle(manager.call_method(
            "requestDevice",
            vec![filters(vec![Value::object(HashMap::from([(
                "vendorId".into(),
                Value::Number(f64::from(0x9999u16)),
            )]))])],
        ));
        assert_eq!(missing.unwrap_err(), "NotFoundError");

        crate::hid_adapter::set_chooser(Some(Box::new(|_| None)));
        let dismissed = settle(manager.call_method("requestDevice", vec![filters(vec![])]));
        assert_eq!(dismissed.unwrap_err(), "NotFoundError");

        crate::hid_adapter::set_chooser(Some(Box::new(|devices| {
            devices
                .iter()
                .find(|device| device.vendor_id == 0x2222)
                .map(|device| device.id.clone())
        })));
        let chosen = settle(manager.call_method("requestDevice", vec![filters(vec![])])).unwrap();
        assert_eq!(
            chosen.get_property("vendorId").to_number(),
            f64::from(0x2222u16)
        );
        assert_eq!(chosen.get_property("productName").to_js_string(), "mouse");
        let listed = settle(manager.call_method("getDevices", vec![])).unwrap();
        assert_eq!(listed.get_property("length").to_number(), 1.0);
        isolate();
    }

    #[test]
    fn hid_reports_round_trip_collections_and_hotplug() {
        isolate();
        crate::hid_adapter::set_mock_devices(vec![mouse("mouse", 0x046D)]);
        crate::capability_gate::grant("hid");
        let manager = hid_value();
        let device = settle(manager.call_method("requestDevice", vec![filters(vec![])])).unwrap();
        let collection = device.get_property("collections").get_property("0");
        assert_eq!(collection.get_property("usagePage").to_number(), 1.0);
        assert_eq!(collection.get_property("usage").to_number(), 2.0);
        assert!(
            collection
                .get_property("inputReports")
                .get_property("length")
                .to_number()
                >= 1.0
        );

        let reports = Rc::new(RefCell::new(Vec::new()));
        let reports_for_handler = Rc::clone(&reports);
        device.set_property(
            "oninputreport",
            Value::function(move |_, args| {
                let event = args.first().cloned().unwrap_or(Value::Undefined);
                let bytes = view_bytes(&event.get_property("data"));
                reports_for_handler
                    .borrow_mut()
                    .push((event.get_property("reportId").to_number() as u8, bytes));
                Value::Undefined
            }),
        );
        let connections = Rc::new(RefCell::new(Vec::new()));
        let connections_for_handler = Rc::clone(&connections);
        let record = move |kind: &'static str| {
            let connections_for_handler = Rc::clone(&connections_for_handler);
            Value::function(move |_, args| {
                let event = args.first().cloned().unwrap_or(Value::Undefined);
                connections_for_handler.borrow_mut().push((
                    kind.to_string(),
                    event
                        .get_property("device")
                        .get_property("opened")
                        .to_bool(),
                ));
                Value::Undefined
            })
        };
        manager.set_property("onconnect", record("connect"));
        manager.set_property("ondisconnect", record("disconnect"));

        settle(device.call_method("open", vec![])).unwrap();
        assert!(device.get_property("opened").to_bool());
        let payload = w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Uint8Array"),
            vec![w3cos_core::binary::array_buffer_value(vec![1, 2, 3])],
        );
        settle(device.call_method("sendReport", vec![Value::Number(4.0), payload])).unwrap();
        assert_eq!(
            crate::hid_adapter::output_log("mouse"),
            vec![vec![4, 1, 2, 3]]
        );
        crate::hid_adapter::set_feature("mouse", 7, vec![9, 8]);
        let feature =
            settle(device.call_method("receiveFeatureReport", vec![Value::Number(7.0)])).unwrap();
        assert!(w3cos_core::class::instance_of(
            &feature,
            &w3cos_core::binary::data_view_class()
        ));
        assert_eq!(view_bytes(&feature), vec![9, 8]);

        assert!(crate::hid_adapter::push_input("mouse", vec![0x10, 0x20]));
        crate::jsdom::drain_microtasks();
        assert_eq!(&*reports.borrow(), &[(0, vec![0x10, 0x20])]);

        crate::hid_adapter::set_mock_devices(vec![]);
        crate::jsdom::drain_microtasks();
        assert_eq!(&*connections.borrow(), &[("disconnect".into(), false)]);
        assert!(!device.get_property("opened").to_bool());

        crate::hid_adapter::set_mock_devices(vec![mouse("mouse", 0x046D)]);
        crate::jsdom::drain_microtasks();
        assert_eq!(
            &*connections.borrow(),
            &[("disconnect".into(), false), ("connect".into(), false)]
        );

        settle(device.call_method("close", vec![])).unwrap();
        let closed = settle(device.call_method(
            "sendReport",
            vec![
                Value::Number(0.0),
                w3cos_core::binary::array_buffer_value(vec![1]),
            ],
        ));
        assert_eq!(closed.unwrap_err(), "InvalidStateError");
        isolate();
    }

    #[test]
    fn usb_permission_protected_claim_and_transfer_results() {
        isolate();
        crate::usb_adapter::set_mock_devices(vec![
            gadget("hid-class", 0x03),
            gadget("vendor", 0xFF),
        ]);
        let manager = usb_value();
        let denied = settle(manager.call_method("requestDevice", vec![filters(vec![])]));
        assert_eq!(denied.unwrap_err(), "NotAllowedError");

        crate::capability_gate::grant("usb");
        crate::usb_adapter::set_chooser(Some(Box::new(|_| None)));
        let dismissed = settle(manager.call_method("requestDevice", vec![filters(vec![])]));
        assert_eq!(dismissed.unwrap_err(), "NotFoundError");
        crate::usb_adapter::set_chooser(None);

        let missing = settle(manager.call_method(
            "requestDevice",
            vec![filters(vec![Value::object(HashMap::from([(
                "vendorId".into(),
                Value::Number(1.0),
            )]))])],
        ));
        assert_eq!(missing.unwrap_err(), "NotFoundError");

        let protected = settle(manager.call_method(
            "requestDevice",
            vec![filters(vec![Value::object(HashMap::from([(
                "classCode".into(),
                Value::Number(3.0),
            )]))])],
        ))
        .unwrap();
        assert_eq!(
            protected.get_property("deviceVersionMajor").to_number(),
            1.0
        );
        assert_eq!(
            protected.get_property("deviceVersionMinor").to_number(),
            1.0
        );
        assert_eq!(protected.get_property("usbVersionMajor").to_number(), 2.0);
        settle(protected.call_method("open", vec![])).unwrap();
        let blocked = settle(protected.call_method("claimInterface", vec![Value::Number(0.0)]));
        assert_eq!(blocked.unwrap_err(), "SecurityError");

        let device = settle(manager.call_method(
            "requestDevice",
            vec![filters(vec![Value::object(HashMap::from([(
                "classCode".into(),
                Value::Number(255.0),
            )]))])],
        ))
        .unwrap();
        settle(device.call_method("open", vec![])).unwrap();
        assert!(device.get_property("opened").to_bool());
        let interface = device
            .get_property("configuration")
            .get_property("interfaces")
            .get_property("0");
        settle(device.call_method("claimInterface", vec![Value::Number(0.0)])).unwrap();
        assert!(interface.get_property("claimed").to_bool());
        let endpoint = interface
            .get_property("alternate")
            .get_property("endpoints")
            .get_property("0");
        assert_eq!(endpoint.get_property("direction").to_js_string(), "in");
        assert_eq!(endpoint.get_property("type").to_js_string(), "bulk");

        let incoming =
            settle(device.call_method("transferIn", vec![Value::Number(1.0), Value::Number(64.0)]))
                .unwrap();
        assert!(w3cos_core::class::instance_of(
            &incoming,
            &usb_record_class("USBInTransferResult")
        ));
        assert_eq!(incoming.get_property("status").to_js_string(), "ok");
        assert_eq!(view_bytes(&incoming.get_property("data")), vec![1, 2, 3]);
        let stalled =
            settle(device.call_method("transferIn", vec![Value::Number(1.0), Value::Number(64.0)]))
                .unwrap();
        assert_eq!(stalled.get_property("status").to_js_string(), "stall");
        assert!(stalled.get_property("data").is_null());

        let payload = w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Uint8Array"),
            vec![w3cos_core::binary::array_buffer_value(vec![9, 8])],
        );
        let outgoing =
            settle(device.call_method("transferOut", vec![Value::Number(1.0), payload.clone()]))
                .unwrap();
        assert!(w3cos_core::class::instance_of(
            &outgoing,
            &usb_record_class("USBOutTransferResult")
        ));
        assert_eq!(outgoing.get_property("status").to_js_string(), "ok");
        assert_eq!(outgoing.get_property("bytesWritten").to_number(), 2.0);
        let stalled_out =
            settle(device.call_method("transferOut", vec![Value::Number(1.0), payload])).unwrap();
        assert_eq!(stalled_out.get_property("status").to_js_string(), "stall");
        assert_eq!(stalled_out.get_property("bytesWritten").to_number(), 0.0);

        let isochronous = settle(device.call_method("isochronousTransferIn", vec![]));
        assert_eq!(isochronous.unwrap_err(), "NotSupportedError");
        settle(device.call_method("releaseInterface", vec![Value::Number(0.0)])).unwrap();
        assert!(!interface.get_property("claimed").to_bool());
        isolate();
    }

    #[test]
    fn webusb_records_preserve_nested_descriptor_and_transfer_shapes() {
        isolate();
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
