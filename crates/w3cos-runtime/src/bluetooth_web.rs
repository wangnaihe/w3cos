//! Web Bluetooth facade for `navigator.bluetooth`.
//!
//! The runtime owns the standards-shaped JavaScript objects while an embedding
//! host owns platform discovery, permission prompts and GATT I/O through the
//! `web-bluetooth` host module.

use std::cell::RefCell;
use std::collections::HashMap;

use w3cos_core::Value;

thread_local! {
    static BLUETOOTH_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BLUETOOTH_INTERFACE_CLASSES: RefCell<HashMap<String, Value>> =
        RefCell::new(HashMap::new());
}

pub const BLUETOOTH_INTERFACE_NAMES: &[&str] = &[
    "BluetoothCharacteristicProperties",
    "BluetoothDevice",
    "BluetoothRemoteGATTCharacteristic",
    "BluetoothRemoteGATTDescriptor",
    "BluetoothRemoteGATTServer",
    "BluetoothRemoteGATTService",
    "BluetoothUUID",
];

fn interface_members(name: &str) -> &'static [&'static str] {
    match name {
        "BluetoothCharacteristicProperties" => &[
            "authenticatedSignedWrites",
            "broadcast",
            "indicate",
            "notify",
            "read",
            "reliableWrite",
            "writableAuxiliaries",
            "write",
            "writeWithoutResponse",
        ],
        "BluetoothDevice" => &["gatt", "id", "name", "ongattserverdisconnected"],
        "BluetoothRemoteGATTCharacteristic" => &[
            "getDescriptor",
            "getDescriptors",
            "oncharacteristicvaluechanged",
            "properties",
            "readValue",
            "service",
            "startNotifications",
            "stopNotifications",
            "uuid",
            "value",
            "writeValue",
            "writeValueWithResponse",
            "writeValueWithoutResponse",
        ],
        "BluetoothRemoteGATTDescriptor" => {
            &["characteristic", "readValue", "uuid", "value", "writeValue"]
        }
        "BluetoothRemoteGATTServer" => &[
            "connect",
            "connected",
            "device",
            "disconnect",
            "getPrimaryService",
            "getPrimaryServices",
        ],
        "BluetoothRemoteGATTService" => &[
            "device",
            "getCharacteristic",
            "getCharacteristics",
            "isPrimary",
            "uuid",
        ],
        _ => &[],
    }
}

pub fn interface_class(name: &str) -> Value {
    BLUETOOTH_INTERFACE_CLASSES.with(|classes| {
        if let Some(class) = classes.borrow().get(name).cloned() {
            return class;
        }
        let Some(name) = BLUETOOTH_INTERFACE_NAMES
            .iter()
            .copied()
            .find(|candidate| candidate == &name)
        else {
            return Value::Undefined;
        };
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(dom_error(
                "TypeError",
                &format!("Illegal constructor: {name}"),
            ))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in interface_members(name) {
            prototype.set_property(member, Value::Undefined);
        }
        if matches!(
            name,
            "BluetoothDevice" | "BluetoothRemoteGATTCharacteristic"
        ) {
            w3cos_core::class::set_prototype_of(
                &prototype,
                &crate::web_events::event_target_class().get_property("prototype"),
            );
        }
        class.set_property("prototype", prototype);
        if name == "BluetoothUUID" {
            for method in [
                "canonicalUUID",
                "getCharacteristic",
                "getDescriptor",
                "getService",
            ] {
                class.set_property(
                    method,
                    Value::function(move |_, args| {
                        bluetooth_uuid(method, args.first().cloned().unwrap_or_default())
                    }),
                );
            }
        }
        classes.borrow_mut().insert(name.to_string(), class.clone());
        class
    })
}

fn bluetooth_uuid(kind: &str, value: Value) -> Value {
    let text = value.to_js_string();
    let assigned = if let Ok(number) = text.parse::<u32>() {
        Some(number)
    } else {
        match (kind, text.as_str()) {
            ("getService", "battery_service") => Some(0x180f),
            ("getCharacteristic", "battery_level") => Some(0x2a19),
            ("getDescriptor", "gatt.client_characteristic_configuration")
            | ("getDescriptor", "client_characteristic_configuration") => Some(0x2902),
            _ => None,
        }
    };
    if let Some(number) = assigned {
        Value::string(&format!("{number:08x}-0000-1000-8000-00805f9b34fb"))
    } else if text.contains('-') {
        Value::string(&text.to_ascii_lowercase())
    } else {
        w3cos_core::throw_value(dom_error(
            "TypeError",
            &format!("Unknown Bluetooth UUID name: {text}"),
        ))
    }
}

pub fn bluetooth_class() -> Value {
    BLUETOOTH_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(dom_error("TypeError", "Illegal constructor: Bluetooth"))
        });
        class.set_property("name", Value::string("Bluetooth"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["getAvailability", "requestDevice"] {
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

fn dom_error(name: &str, message: &str) -> Value {
    w3cos_core::class::construct(
        &crate::unsupported::dom_exception_class(),
        vec![Value::string(message), Value::string(name)],
    )
}

fn host_call(operation: &str, payload: Value) -> Result<Value, Value> {
    let response = w3cos_core::host::invoke(vec![
        Value::string("web-bluetooth"),
        Value::string(operation),
        payload,
    ]);
    if response.is_undefined() {
        return Err(dom_error(
            "NotSupportedError",
            "Web Bluetooth is unavailable on this platform",
        ));
    }
    let error = response.get_property("error").to_js_string();
    if !error.is_empty() && error != "undefined" {
        let name = response.get_property("error_name").to_js_string();
        return Err(dom_error(
            if name.is_empty() || name == "undefined" {
                "NetworkError"
            } else {
                &name
            },
            &error,
        ));
    }
    Ok(response)
}

fn promise(result: Result<Value, Value>) -> Value {
    match result {
        Ok(value) => w3cos_core::promise::resolve(vec![value]),
        Err(error) => w3cos_core::promise::reject(vec![error]),
    }
}

fn characteristic_properties_value() -> Value {
    let properties = Value::object(HashMap::from([
        ("authenticatedSignedWrites".into(), Value::Bool(false)),
        ("broadcast".into(), Value::Bool(false)),
        ("indicate".into(), Value::Bool(false)),
        ("notify".into(), Value::Bool(true)),
        ("read".into(), Value::Bool(true)),
        ("reliableWrite".into(), Value::Bool(false)),
        ("writableAuxiliaries".into(), Value::Bool(false)),
        ("write".into(), Value::Bool(true)),
        ("writeWithoutResponse".into(), Value::Bool(true)),
    ]));
    w3cos_core::class::set_prototype_of(
        &properties,
        &interface_class("BluetoothCharacteristicProperties").get_property("prototype"),
    );
    properties
}

fn descriptor_value(
    device_id: String,
    service_uuid: String,
    characteristic_uuid: String,
    descriptor_uuid: String,
    characteristic: Value,
) -> Value {
    let descriptor = Value::object(HashMap::from([
        ("characteristic".into(), characteristic),
        ("uuid".into(), Value::string(&descriptor_uuid)),
        ("value".into(), Value::Null),
    ]));
    let read_descriptor = descriptor.clone();
    let read_device = device_id.clone();
    let read_service = service_uuid.clone();
    let read_characteristic = characteristic_uuid.clone();
    let read_uuid = descriptor_uuid.clone();
    descriptor.set_property(
        "readValue",
        Value::function(move |_, _| {
            promise(
                host_call(
                    "read_descriptor",
                    Value::object(HashMap::from([
                        ("device_id".into(), Value::string(&read_device)),
                        ("service_uuid".into(), Value::string(&read_service)),
                        (
                            "characteristic_uuid".into(),
                            Value::string(&read_characteristic),
                        ),
                        ("descriptor_uuid".into(), Value::string(&read_uuid)),
                    ])),
                )
                .map(|response| {
                    let view = w3cos_core::class::construct(
                        &w3cos_core::binary::data_view_class(),
                        vec![w3cos_core::binary::array_buffer_value(
                            response
                                .get_property("value")
                                .iter()
                                .map(|item| item.to_number().clamp(0.0, 255.0) as u8)
                                .collect(),
                        )],
                    );
                    read_descriptor.set_property("value", view.clone());
                    view
                }),
            )
        }),
    );
    let write_device = device_id;
    let write_service = service_uuid;
    let write_characteristic = characteristic_uuid;
    let write_uuid = descriptor_uuid;
    descriptor.set_property(
        "writeValue",
        Value::function(move |_, args| {
            let Some(bytes) = args.first().and_then(w3cos_core::binary::bytes_of) else {
                return promise(Err(dom_error(
                    "TypeError",
                    "GATT descriptor write requires an ArrayBuffer or ArrayBufferView",
                )));
            };
            promise(
                host_call(
                    "write_descriptor",
                    Value::object(HashMap::from([
                        ("device_id".into(), Value::string(&write_device)),
                        ("service_uuid".into(), Value::string(&write_service)),
                        (
                            "characteristic_uuid".into(),
                            Value::string(&write_characteristic),
                        ),
                        ("descriptor_uuid".into(), Value::string(&write_uuid)),
                        (
                            "value".into(),
                            Value::array(
                                bytes
                                    .into_iter()
                                    .map(|byte| Value::Number(byte as f64))
                                    .collect(),
                            ),
                        ),
                    ])),
                )
                .map(|_| Value::Undefined),
            )
        }),
    );
    w3cos_core::class::set_prototype_of(
        &descriptor,
        &interface_class("BluetoothRemoteGATTDescriptor").get_property("prototype"),
    );
    descriptor
}

fn characteristic_value(
    device_id: String,
    service_uuid: String,
    characteristic_uuid: String,
) -> Value {
    let service = Value::object(HashMap::from([
        ("device".into(), Value::Null),
        ("isPrimary".into(), Value::Bool(true)),
        ("uuid".into(), Value::string(&service_uuid)),
    ]));
    w3cos_core::class::set_prototype_of(
        &service,
        &interface_class("BluetoothRemoteGATTService").get_property("prototype"),
    );
    let value = Value::object(HashMap::from([
        ("uuid".to_string(), Value::string(&characteristic_uuid)),
        ("service".to_string(), service),
        ("properties".to_string(), characteristic_properties_value()),
        ("value".to_string(), Value::Null),
        ("oncharacteristicvaluechanged".to_string(), Value::Null),
    ]));
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    for method in [
        "writeValue",
        "writeValueWithResponse",
        "writeValueWithoutResponse",
    ] {
        let write_type = if method == "writeValueWithoutResponse" {
            "without_response"
        } else {
            "with_response"
        };
        let device_id = device_id.clone();
        let service_uuid = service_uuid.clone();
        let characteristic_uuid = characteristic_uuid.clone();
        value.set_property(
            method,
            Value::function(move |_, args| {
                let Some(bytes) = args.first().and_then(w3cos_core::binary::bytes_of) else {
                    return promise(Err(dom_error(
                        "TypeError",
                        "GATT write requires an ArrayBuffer or ArrayBufferView",
                    )));
                };
                let payload = Value::object(HashMap::from([
                    ("device_id".to_string(), Value::string(&device_id)),
                    ("service_uuid".to_string(), Value::string(&service_uuid)),
                    (
                        "characteristic_uuid".to_string(),
                        Value::string(&characteristic_uuid),
                    ),
                    (
                        "value".to_string(),
                        Value::array(
                            bytes
                                .into_iter()
                                .map(|byte| Value::Number(byte as f64))
                                .collect(),
                        ),
                    ),
                    ("write_type".to_string(), Value::string(write_type)),
                ]));
                promise(host_call("write_characteristic", payload).map(|_| Value::Undefined))
            }),
        );
    }
    let read_value = value.clone();
    let read_device_id = device_id.clone();
    let read_service_uuid = service_uuid.clone();
    let read_characteristic_uuid = characteristic_uuid.clone();
    value.set_property(
        "readValue",
        Value::function(move |_, _| {
            let payload = Value::object(HashMap::from([
                ("device_id".to_string(), Value::string(&read_device_id)),
                (
                    "service_uuid".to_string(),
                    Value::string(&read_service_uuid),
                ),
                (
                    "characteristic_uuid".to_string(),
                    Value::string(&read_characteristic_uuid),
                ),
            ]));
            promise(host_call("read_characteristic", payload).map(|response| {
                let bytes = response
                    .get_property("value")
                    .iter()
                    .map(|item| item.to_number().clamp(0.0, 255.0) as u8)
                    .collect();
                let view = w3cos_core::class::construct(
                    &w3cos_core::binary::data_view_class(),
                    vec![w3cos_core::binary::array_buffer_value(bytes)],
                );
                read_value.set_property("value", view.clone());
                view
            }))
        }),
    );
    for method in ["startNotifications", "stopNotifications"] {
        let notify_value = value.clone();
        let notify_device = device_id.clone();
        let notify_service = service_uuid.clone();
        let notify_characteristic = characteristic_uuid.clone();
        value.set_property(
            method,
            Value::function(move |_, _| {
                promise(
                    host_call(
                        if method == "startNotifications" {
                            "start_notifications"
                        } else {
                            "stop_notifications"
                        },
                        Value::object(HashMap::from([
                            ("device_id".into(), Value::string(&notify_device)),
                            ("service_uuid".into(), Value::string(&notify_service)),
                            (
                                "characteristic_uuid".into(),
                                Value::string(&notify_characteristic),
                            ),
                        ])),
                    )
                    .map(|_| notify_value.clone()),
                )
            }),
        );
    }
    let descriptor_characteristic = value.clone();
    let descriptor_device = device_id.clone();
    let descriptor_service = service_uuid.clone();
    let descriptor_characteristic_uuid = characteristic_uuid.clone();
    value.set_property(
        "getDescriptor",
        Value::function(move |_, args| {
            let descriptor_uuid = args.first().cloned().unwrap_or_default().to_js_string();
            if descriptor_uuid.is_empty() {
                return promise(Err(dom_error("TypeError", "A descriptor UUID is required")));
            }
            promise(
                host_call(
                    "get_descriptor",
                    Value::object(HashMap::from([
                        ("device_id".into(), Value::string(&descriptor_device)),
                        ("service_uuid".into(), Value::string(&descriptor_service)),
                        (
                            "characteristic_uuid".into(),
                            Value::string(&descriptor_characteristic_uuid),
                        ),
                        ("descriptor_uuid".into(), Value::string(&descriptor_uuid)),
                    ])),
                )
                .map(|_| {
                    descriptor_value(
                        descriptor_device.clone(),
                        descriptor_service.clone(),
                        descriptor_characteristic_uuid.clone(),
                        descriptor_uuid,
                        descriptor_characteristic.clone(),
                    )
                }),
            )
        }),
    );
    value.set_property(
        "getDescriptors",
        Value::function(|_, _| {
            promise(Err(dom_error(
                "NotSupportedError",
                "Enumerating GATT descriptors requires a host Bluetooth adapter",
            )))
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &interface_class("BluetoothRemoteGATTCharacteristic").get_property("prototype"),
    );
    value
}

fn service_value(device_id: String, service_uuid: String) -> Value {
    let value = Value::object(HashMap::from([
        ("device".to_string(), Value::Null),
        ("isPrimary".to_string(), Value::Bool(true)),
        ("uuid".to_string(), Value::string(&service_uuid)),
    ]));
    let characteristic_device = device_id.clone();
    let characteristic_service = service_uuid.clone();
    value.set_property(
        "getCharacteristic",
        Value::function(move |_, args| {
            let characteristic_uuid = args.first().cloned().unwrap_or_default().to_js_string();
            if characteristic_uuid.is_empty() {
                return promise(Err(dom_error(
                    "TypeError",
                    "A characteristic UUID is required",
                )));
            }
            let payload = Value::object(HashMap::from([
                (
                    "device_id".to_string(),
                    Value::string(&characteristic_device),
                ),
                (
                    "service_uuid".to_string(),
                    Value::string(&characteristic_service),
                ),
                (
                    "characteristic_uuid".to_string(),
                    Value::string(&characteristic_uuid),
                ),
            ]));
            promise(host_call("get_characteristic", payload).map(|_| {
                characteristic_value(
                    characteristic_device.clone(),
                    characteristic_service.clone(),
                    characteristic_uuid,
                )
            }))
        }),
    );
    value.set_property(
        "getCharacteristics",
        Value::function(|_, _| {
            promise(Err(dom_error(
                "NotSupportedError",
                "Enumerating GATT characteristics requires a host Bluetooth adapter",
            )))
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &interface_class("BluetoothRemoteGATTService").get_property("prototype"),
    );
    value
}

fn gatt_server_value(device_id: String) -> Value {
    let value = Value::object(HashMap::from([
        ("connected".to_string(), Value::Bool(true)),
        ("device".to_string(), Value::Null),
    ]));
    let connect_device_id = device_id.clone();
    value.set_property(
        "connect",
        Value::function(move |this, _| {
            promise(
                host_call(
                    "connect",
                    Value::object(HashMap::from([(
                        "device_id".to_string(),
                        Value::string(&connect_device_id),
                    )])),
                )
                .map(|_| {
                    this.set_property("connected", Value::Bool(true));
                    this
                }),
            )
        }),
    );
    let service_device_id = device_id.clone();
    value.set_property(
        "getPrimaryService",
        Value::function(move |this, args| {
            let service_uuid = args.first().cloned().unwrap_or_default().to_js_string();
            if service_uuid.is_empty() {
                return promise(Err(dom_error("TypeError", "A service UUID is required")));
            }
            let device = this.get_property("device");
            let payload = Value::object(HashMap::from([
                ("device_id".to_string(), Value::string(&service_device_id)),
                ("service_uuid".to_string(), Value::string(&service_uuid)),
            ]));
            promise(host_call("get_primary_service", payload).map(|_| {
                let service = service_value(service_device_id.clone(), service_uuid);
                service.set_property("device", device);
                service
            }))
        }),
    );
    value.set_property(
        "disconnect",
        Value::function(move |this, _| {
            let payload = Value::object(HashMap::from([(
                "device_id".to_string(),
                Value::string(&device_id),
            )]));
            let _ = host_call("disconnect", payload);
            this.set_property("connected", Value::Bool(false));
            Value::Undefined
        }),
    );
    value.set_property(
        "getPrimaryServices",
        Value::function(|_, _| {
            promise(Err(dom_error(
                "NotSupportedError",
                "Enumerating primary GATT services requires a host Bluetooth adapter",
            )))
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &interface_class("BluetoothRemoteGATTServer").get_property("prototype"),
    );
    value
}

fn device_value(response: &Value) -> Value {
    let device_id = response.get_property("device_id").to_js_string();
    let name = response.get_property("name");
    let device = Value::object(HashMap::from([
        ("id".to_string(), Value::string(&device_id)),
        ("name".to_string(), name),
        ("ongattserverdisconnected".to_string(), Value::Null),
    ]));
    crate::web_events::event_target_class().call(device.clone(), vec![]);
    let gatt = gatt_server_value(device_id);
    gatt.set_property("connected", Value::Bool(false));
    gatt.set_property("device", device.clone());
    device.set_property("gatt", gatt);
    w3cos_core::class::set_prototype_of(
        &device,
        &interface_class("BluetoothDevice").get_property("prototype"),
    );
    device
}

pub fn bluetooth_value() -> Value {
    let bluetooth = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
    bluetooth.set_property(
        "getAvailability",
        Value::function(|_, _| {
            promise(
                host_call("get_availability", Value::object(HashMap::new()))
                    .map(|response| Value::Bool(response.get_property("available").to_bool())),
            )
        }),
    );
    bluetooth.set_property(
        "requestDevice",
        Value::function(|_, args| {
            let options = args.first().cloned().unwrap_or_default();
            promise(host_call("request_device", options).map(|response| device_value(&response)))
        }),
    );
    w3cos_core::class::set_prototype_of(&bluetooth, &bluetooth_class().get_property("prototype"));
    bluetooth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_web_bluetooth_entry_points() {
        let bluetooth = bluetooth_value();
        assert!(bluetooth.get_property("getAvailability").is_function());
        assert!(bluetooth.get_property("requestDevice").is_function());
    }

    #[test]
    fn exposes_gatt_device_and_characteristic_methods() {
        let response = Value::object(HashMap::from([
            ("device_id".to_string(), Value::string("ble:00:11")),
            ("name".to_string(), Value::string("Label Printer")),
        ]));
        let device = device_value(&response);
        let gatt = device.get_property("gatt");
        let characteristic = characteristic_value(
            "ble:00:11".to_string(),
            "0000180f-0000-1000-8000-00805f9b34fb".to_string(),
            "00002a19-0000-1000-8000-00805f9b34fb".to_string(),
        );

        assert_eq!(device.get_property("id").to_js_string(), "ble:00:11");
        assert!(w3cos_core::class::instance_of(
            &device,
            &interface_class("BluetoothDevice")
        ));
        assert!(w3cos_core::class::instance_of(
            &gatt,
            &interface_class("BluetoothRemoteGATTServer")
        ));
        assert!(gatt.get_property("connect").is_function());
        assert!(w3cos_core::class::instance_of(
            &characteristic,
            &interface_class("BluetoothRemoteGATTCharacteristic")
        ));
        assert!(w3cos_core::class::instance_of(
            &characteristic.get_property("properties"),
            &interface_class("BluetoothCharacteristicProperties")
        ));
        assert!(w3cos_core::class::instance_of(
            &characteristic.get_property("service"),
            &interface_class("BluetoothRemoteGATTService")
        ));
        assert!(characteristic.get_property("readValue").is_function());
        assert!(
            characteristic
                .get_property("writeValueWithResponse")
                .is_function()
        );
        assert!(
            characteristic
                .get_property("writeValueWithoutResponse")
                .is_function()
        );
        let uuid = interface_class("BluetoothUUID");
        assert_eq!(
            uuid.call_method("getService", vec![Value::string("battery_service")])
                .to_js_string(),
            "0000180f-0000-1000-8000-00805f9b34fb"
        );
        assert_eq!(
            uuid.call_method("canonicalUUID", vec![Value::Number(0x2a19 as f64)])
                .to_js_string(),
            "00002a19-0000-1000-8000-00805f9b34fb"
        );
    }
}
