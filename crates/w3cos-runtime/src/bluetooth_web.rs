//! Web Bluetooth facade for `navigator.bluetooth`.
//!
//! The runtime owns the standards-shaped JavaScript objects while an embedding
//! host owns platform discovery, permission prompts and GATT I/O through the
//! `web-bluetooth` host module.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::jsdom::realm_function;
use w3cos_core::Value;

thread_local! {
    static BLUETOOTH_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static BLUETOOTH_INTERFACE_CLASSES: RefCell<HashMap<String, Value>> =
        RefCell::new(HashMap::new());
    static DEVICES: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
    static CHARACTERISTICS: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
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
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, move |_, _| {
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
                    realm_function(generation, move |_, args| {
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
    if kind == "canonicalUUID"
        && let Some(number) = value.as_number()
        && number.is_finite()
        && number >= 0.0
        && number <= u32::MAX as f64
        && number.fract() == 0.0
    {
        return Value::string(&crate::bluetooth_adapter::canonical_assigned(number as u32));
    }
    match crate::bluetooth_adapter::canonical_uuid(Some(kind), &value.to_js_string()) {
        Ok(uuid) => Value::string(&uuid),
        Err(message) => w3cos_core::throw_value(dom_error("TypeError", &message)),
    }
}

fn canonicalize(kind: Option<&str>, text: &str) -> String {
    crate::bluetooth_adapter::canonical_uuid(kind, text)
        .unwrap_or_else(|_| text.to_ascii_lowercase())
}

fn characteristic_key(device_id: &str, service_uuid: &str, characteristic_uuid: &str) -> String {
    format!("{device_id}\n{service_uuid}\n{characteristic_uuid}")
}

fn remember_device(id: &str, device: &Value) {
    DEVICES.with(|devices| {
        devices.borrow_mut().insert(id.to_string(), device.clone());
    });
}

fn remember_characteristic(key: &str, characteristic: &Value) {
    CHARACTERISTICS.with(|characteristics| {
        characteristics
            .borrow_mut()
            .insert(key.to_string(), characteristic.clone());
    });
}

fn data_view(bytes: Vec<u8>) -> Value {
    w3cos_core::class::construct(
        &w3cos_core::binary::data_view_class(),
        vec![w3cos_core::binary::array_buffer_value(bytes)],
    )
}

fn dispatch_typed(target: &Value, event_type: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(event_type)],
    );
    target.call_method("dispatchEvent", vec![event]);
}

fn properties_from(source: &Value) -> Value {
    let flag = |name: &str| Value::Bool(source.get_property(name).to_bool());
    let properties = Value::object(HashMap::from([
        (
            "authenticatedSignedWrites".into(),
            flag("authenticatedSignedWrites"),
        ),
        ("broadcast".into(), flag("broadcast")),
        ("indicate".into(), flag("indicate")),
        ("notify".into(), flag("notify")),
        ("read".into(), flag("read")),
        ("reliableWrite".into(), flag("reliableWrite")),
        ("writableAuxiliaries".into(), flag("writableAuxiliaries")),
        ("write".into(), flag("write")),
        ("writeWithoutResponse".into(), flag("writeWithoutResponse")),
    ]));
    w3cos_core::class::set_prototype_of(
        &properties,
        &interface_class("BluetoothCharacteristicProperties").get_property("prototype"),
    );
    properties
}

pub fn bluetooth_class() -> Value {
    BLUETOOTH_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let generation = crate::jsdom::realm_generation();
        let class = realm_function(generation, |_, _| {
            w3cos_core::throw_value(dom_error("TypeError", "Illegal constructor: Bluetooth"))
        });
        class.set_property("name", Value::string("Bluetooth"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["getAvailability", "getDevices", "requestDevice"] {
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

#[cfg(test)]
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
    let generation = crate::jsdom::realm_generation();
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
        realm_function(generation, move |_, _| {
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
        realm_function(generation, move |_, args| {
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
    properties: Value,
) -> Value {
    let service_uuid = canonicalize(Some("getService"), &service_uuid);
    let characteristic_uuid = canonicalize(Some("getCharacteristic"), &characteristic_uuid);
    let key = characteristic_key(&device_id, &service_uuid, &characteristic_uuid);
    if let Some(existing) =
        CHARACTERISTICS.with(|characteristics| characteristics.borrow().get(&key).cloned())
    {
        return existing;
    }
    let generation = crate::jsdom::realm_generation();
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
        ("properties".to_string(), properties),
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
            realm_function(generation, move |_, args| {
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
        realm_function(generation, move |_, _| {
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
                let view = data_view(bytes);
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
            realm_function(generation, move |_, _| {
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
        realm_function(generation, move |_, args| {
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
    let descriptors_characteristic = value.clone();
    let descriptors_device = device_id.clone();
    let descriptors_service = service_uuid.clone();
    let descriptors_characteristic_uuid = characteristic_uuid.clone();
    value.set_property(
        "getDescriptors",
        realm_function(generation, move |_, args| {
            let mut payload = HashMap::from([
                ("device_id".into(), Value::string(&descriptors_device)),
                ("service_uuid".into(), Value::string(&descriptors_service)),
                (
                    "characteristic_uuid".into(),
                    Value::string(&descriptors_characteristic_uuid),
                ),
            ]);
            if let Some(uuid) = args.first().cloned()
                && !uuid.is_undefined()
            {
                payload.insert("descriptor_uuid".into(), uuid);
            }
            promise(
                host_call("get_descriptors", Value::object(payload)).map(|response| {
                    let descriptors = response
                        .get_property("descriptors")
                        .iter()
                        .map(|item| {
                            descriptor_value(
                                descriptors_device.clone(),
                                descriptors_service.clone(),
                                descriptors_characteristic_uuid.clone(),
                                item.to_js_string(),
                                descriptors_characteristic.clone(),
                            )
                        })
                        .collect();
                    Value::array(descriptors)
                }),
            )
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &interface_class("BluetoothRemoteGATTCharacteristic").get_property("prototype"),
    );
    remember_characteristic(&key, &value);
    value
}

fn service_value(device_id: String, service_uuid: String, primary: bool) -> Value {
    let service_uuid = canonicalize(Some("getService"), &service_uuid);
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::from([
        ("device".to_string(), Value::Null),
        ("isPrimary".to_string(), Value::Bool(primary)),
        ("uuid".to_string(), Value::string(&service_uuid)),
    ]));
    let characteristic_device = device_id.clone();
    let characteristic_service = service_uuid.clone();
    value.set_property(
        "getCharacteristic",
        realm_function(generation, move |_, args| {
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
            promise(host_call("get_characteristic", payload).map(|response| {
                let uuid = response.get_property("uuid").to_js_string();
                let resolved = if uuid.is_empty() {
                    characteristic_uuid.clone()
                } else {
                    uuid
                };
                characteristic_value(
                    characteristic_device.clone(),
                    characteristic_service.clone(),
                    resolved,
                    properties_from(&response),
                )
            }))
        }),
    );
    let characteristics_device = device_id;
    let characteristics_service = service_uuid;
    value.set_property(
        "getCharacteristics",
        realm_function(generation, move |_, args| {
            let mut payload = HashMap::from([
                (
                    "device_id".to_string(),
                    Value::string(&characteristics_device),
                ),
                (
                    "service_uuid".to_string(),
                    Value::string(&characteristics_service),
                ),
            ]);
            if let Some(uuid) = args.first().cloned()
                && !uuid.is_undefined()
            {
                payload.insert("characteristic_uuid".to_string(), uuid);
            }
            promise(
                host_call("get_characteristics", Value::object(payload)).map(|response| {
                    let characteristics = response
                        .get_property("characteristics")
                        .iter()
                        .map(|item| {
                            characteristic_value(
                                characteristics_device.clone(),
                                characteristics_service.clone(),
                                item.get_property("uuid").to_js_string(),
                                properties_from(&item),
                            )
                        })
                        .collect();
                    Value::array(characteristics)
                }),
            )
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &interface_class("BluetoothRemoteGATTService").get_property("prototype"),
    );
    value
}

fn gatt_server_value(device_id: String) -> Value {
    let generation = crate::jsdom::realm_generation();
    let value = Value::object(HashMap::from([
        ("connected".to_string(), Value::Bool(true)),
        ("device".to_string(), Value::Null),
    ]));
    let connect_device_id = device_id.clone();
    value.set_property(
        "connect",
        realm_function(generation, move |this, _| {
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
        realm_function(generation, move |this, args| {
            let service_uuid = args.first().cloned().unwrap_or_default().to_js_string();
            if service_uuid.is_empty() {
                return promise(Err(dom_error("TypeError", "A service UUID is required")));
            }
            let device = this.get_property("device");
            let payload = Value::object(HashMap::from([
                ("device_id".to_string(), Value::string(&service_device_id)),
                ("service_uuid".to_string(), Value::string(&service_uuid)),
            ]));
            promise(host_call("get_primary_service", payload).map(|response| {
                let uuid = response.get_property("uuid").to_js_string();
                let resolved = if uuid.is_empty() {
                    service_uuid.clone()
                } else {
                    uuid
                };
                let service = service_value(service_device_id.clone(), resolved, true);
                service.set_property("device", device);
                service
            }))
        }),
    );
    let disconnect_device_id = device_id.clone();
    value.set_property(
        "disconnect",
        realm_function(generation, move |this, _| {
            let payload = Value::object(HashMap::from([(
                "device_id".to_string(),
                Value::string(&disconnect_device_id),
            )]));
            let _ = host_call("disconnect", payload);
            this.set_property("connected", Value::Bool(false));
            Value::Undefined
        }),
    );
    value.set_property(
        "getPrimaryServices",
        realm_function(generation, move |this, args| {
            let mut payload = HashMap::from([("device_id".to_string(), Value::string(&device_id))]);
            if let Some(uuid) = args.first().cloned()
                && !uuid.is_undefined()
            {
                payload.insert("service_uuid".to_string(), uuid);
            }
            let device = this.get_property("device");
            promise(
                host_call("get_primary_services", Value::object(payload)).map(|response| {
                    let services = response
                        .get_property("services")
                        .iter()
                        .map(|item| {
                            let service = service_value(
                                device_id.clone(),
                                item.get_property("uuid").to_js_string(),
                                true,
                            );
                            service.set_property("device", device.clone());
                            service
                        })
                        .collect();
                    Value::array(services)
                }),
            )
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
    if let Some(existing) = DEVICES.with(|devices| devices.borrow().get(&device_id).cloned()) {
        return existing;
    }
    let name = response.get_property("name");
    let device = Value::object(HashMap::from([
        ("id".to_string(), Value::string(&device_id)),
        ("name".to_string(), name),
        ("ongattserverdisconnected".to_string(), Value::Null),
    ]));
    crate::web_events::event_target_class().call(device.clone(), vec![]);
    let gatt = gatt_server_value(device_id.clone());
    gatt.set_property("connected", Value::Bool(false));
    gatt.set_property("device", device.clone());
    device.set_property("gatt", gatt);
    w3cos_core::class::set_prototype_of(
        &device,
        &interface_class("BluetoothDevice").get_property("prototype"),
    );
    remember_device(&device_id, &device);
    device
}

fn require_host() -> Result<(), Value> {
    if w3cos_core::host::has_host_module("web-bluetooth") {
        Ok(())
    } else {
        Err(dom_error(
            "NotSupportedError",
            "Web Bluetooth is unavailable on this platform",
        ))
    }
}

fn failure_error(error: crate::bluetooth_adapter::BluetoothFailure) -> Value {
    dom_error(error.name, &error.message)
}

pub fn bluetooth_value() -> Value {
    crate::bluetooth_adapter::ensure_installed();
    let generation = crate::jsdom::realm_generation();
    let bluetooth = w3cos_core::class::construct(&crate::web_events::event_target_class(), vec![]);
    bluetooth.set_property(
        "getAvailability",
        realm_function(generation, |_, _| {
            promise(require_host().and_then(|_| {
                host_call("get_availability", Value::object(HashMap::new()))
                    .map(|response| Value::Bool(response.get_property("available").to_bool()))
            }))
        }),
    );
    bluetooth.set_property(
        "getDevices",
        realm_function(generation, |_, _| {
            promise(require_host().and_then(|_| {
                host_call("get_devices", Value::object(HashMap::new())).map(|response| {
                    Value::array(
                        response
                            .get_property("devices")
                            .iter()
                            .map(|item| device_value(&item))
                            .collect(),
                    )
                })
            }))
        }),
    );
    bluetooth.set_property(
        "requestDevice",
        realm_function(generation, |_, args| {
            let options = args.first().cloned().unwrap_or_default();
            promise(request_device(options))
        }),
    );
    w3cos_core::class::set_prototype_of(&bluetooth, &bluetooth_class().get_property("prototype"));
    bluetooth
}

fn request_device(options: Value) -> Result<Value, Value> {
    require_host()?;
    if let Err(error) = crate::bluetooth_adapter::validate_request(&options) {
        return Err(failure_error(error));
    }
    if let Err(error) = crate::bluetooth_adapter::authorize_request() {
        return Err(failure_error(error));
    }
    host_call("request_device", options).map(|response| device_value(&response))
}

pub fn poll_platform_events() -> usize {
    let events = crate::bluetooth_adapter::take_events();
    let count = events.len();
    for event in events {
        match event {
            crate::bluetooth_adapter::RadioEvent::Disconnected { device_id } => {
                let Some(device) =
                    DEVICES.with(|devices| devices.borrow().get(&device_id).cloned())
                else {
                    continue;
                };
                device
                    .get_property("gatt")
                    .set_property("connected", Value::Bool(false));
                dispatch_typed(&device, "gattserverdisconnected");
            }
            crate::bluetooth_adapter::RadioEvent::Notification {
                device_id,
                service_uuid,
                characteristic_uuid,
                value,
            } => {
                let key = characteristic_key(&device_id, &service_uuid, &characteristic_uuid);
                let Some(characteristic) = CHARACTERISTICS
                    .with(|characteristics| characteristics.borrow().get(&key).cloned())
                else {
                    continue;
                };
                characteristic.set_property("value", data_view(value));
                dispatch_typed(&characteristic, "characteristicvaluechanged");
            }
        }
    }
    count
}

pub fn reset_realm() {
    BLUETOOTH_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    BLUETOOTH_INTERFACE_CLASSES.with(|classes| {
        classes.borrow_mut().clear();
    });
    DEVICES.with(|devices| devices.borrow_mut().clear());
    CHARACTERISTICS.with(|characteristics| characteristics.borrow_mut().clear());
    crate::bluetooth_adapter::on_realm_reset();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn exposes_web_bluetooth_entry_points() {
        let bluetooth = bluetooth_value();
        assert!(bluetooth.get_property("getAvailability").is_function());
        assert!(bluetooth.get_property("getDevices").is_function());
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
            characteristic_properties_value(),
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

    #[test]
    fn bluetooth_classes_and_host_entry_points_are_realm_owned() {
        reset_realm();
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        let old_bluetooth_class = bluetooth_class();
        let old_device_class = interface_class("BluetoothDevice");
        let old_uuid_class = interface_class("BluetoothUUID");
        let old_bluetooth = bluetooth_value();
        let response = Value::object(HashMap::from([
            ("device_id".to_string(), Value::string("ble:realm")),
            ("name".to_string(), Value::string("Realm device")),
        ]));
        let old_device = device_value(&response);
        let old_gatt = old_device.get_property("gatt");
        let old_characteristic = characteristic_value(
            "ble:realm".to_string(),
            "0000180f-0000-1000-8000-00805f9b34fb".to_string(),
            "00002a19-0000-1000-8000-00805f9b34fb".to_string(),
            characteristic_properties_value(),
        );
        let host_calls = Rc::new(Cell::new(0_u32));
        let host_calls_for_module = Rc::clone(&host_calls);
        w3cos_core::host::register_host_module("web-bluetooth", move |_, _| {
            host_calls_for_module.set(host_calls_for_module.get() + 1);
            Value::object(HashMap::from([("available".into(), Value::Bool(true))]))
        });

        crate::dom::reset_document();
        crate::jsdom::reset_bridge();

        assert!(!old_bluetooth_class.strict_eq(&bluetooth_class()));
        assert!(!old_device_class.strict_eq(&interface_class("BluetoothDevice")));
        assert!(!old_uuid_class.strict_eq(&interface_class("BluetoothUUID")));
        assert!(
            old_bluetooth_class
                .call(Value::Undefined, vec![])
                .is_undefined()
        );
        assert!(
            old_uuid_class
                .call_method("getService", vec![Value::string("battery_service")])
                .is_undefined()
        );
        assert!(
            old_bluetooth
                .call_method("getAvailability", vec![])
                .is_undefined()
        );
        assert!(
            old_bluetooth
                .call_method("requestDevice", vec![Value::object(HashMap::new())])
                .is_undefined()
        );
        assert!(old_gatt.call_method("connect", vec![]).is_undefined());
        assert!(
            old_gatt
                .call_method("getPrimaryService", vec![Value::string("battery_service")],)
                .is_undefined()
        );
        assert!(
            old_characteristic
                .call_method("readValue", vec![])
                .is_undefined()
        );
        assert!(
            old_characteristic
                .call_method(
                    "writeValueWithResponse",
                    vec![w3cos_core::binary::array_buffer_value(vec![1])],
                )
                .is_undefined()
        );
        assert!(
            old_characteristic
                .call_method("startNotifications", vec![])
                .is_undefined()
        );
        assert_eq!(host_calls.get(), 0);

        let fresh_bluetooth = bluetooth_value();
        assert!(
            fresh_bluetooth
                .call_method("getAvailability", vec![])
                .is_object()
        );
        assert_eq!(host_calls.get(), 1);

        w3cos_core::host::unregister_host_module("web-bluetooth");
        reset_realm();
    }

    fn promise_result(value: Value) -> Result<Value, String> {
        let ok = Rc::new(RefCell::new(None));
        let err = Rc::new(RefCell::new(None));
        let ok_for_then = Rc::clone(&ok);
        let err_for_catch = Rc::clone(&err);
        value.call_method(
            "then",
            vec![Value::function(move |_, args| {
                *ok_for_then.borrow_mut() = args.first().cloned();
                Value::Undefined
            })],
        );
        value.call_method(
            "catch",
            vec![Value::function(move |_, args| {
                *err_for_catch.borrow_mut() = args
                    .first()
                    .map(|value| value.get_property("name").to_js_string());
                Value::Undefined
            })],
        );
        crate::jsdom::drain_microtasks();
        if let Some(error) = err.borrow().clone() {
            return Err(error);
        }
        ok.borrow().clone().ok_or_else(|| "unresolved".into())
    }

    fn heart_request() -> Value {
        Value::object(HashMap::from([(
            "filters".into(),
            Value::array(vec![Value::object(HashMap::from([(
                "services".into(),
                Value::array(vec![Value::string("heart_rate")]),
            )]))]),
        )]))
    }

    fn mock_heart() -> crate::bluetooth_adapter::MockDevice {
        use crate::bluetooth_adapter::{
            CharProps, MockCharacteristic, MockDescriptor, MockDevice, MockService,
        };
        MockDevice {
            id: "ble:10:00:00:00:00:01".into(),
            name: Some("Tick".into()),
            advertised_services: vec!["heart_rate".into()],
            manufacturer_data: Vec::new(),
            services: vec![MockService {
                uuid: "heart_rate".into(),
                primary: true,
                characteristics: vec![MockCharacteristic {
                    uuid: "heart_rate_measurement".into(),
                    properties: CharProps {
                        notify: true,
                        read: true,
                        write_without_response: true,
                        ..CharProps::default()
                    },
                    value: vec![9],
                    descriptors: vec![MockDescriptor {
                        uuid: "client_characteristic_configuration".into(),
                        value: vec![0, 0],
                    }],
                }],
            }],
        }
    }

    #[test]
    fn facade_refuses_a_silent_grant_and_dispatches_gatt_events() {
        crate::capability_gate::reset();
        crate::bluetooth_adapter::set_hardware_test(false);
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        let calls = Rc::new(Cell::new(0u32));
        let calls_for_host = Rc::clone(&calls);
        w3cos_core::host::register_host_module("web-bluetooth", move |_, _| {
            calls_for_host.set(calls_for_host.get() + 1);
            Value::object(HashMap::from([(
                "device_id".into(),
                Value::string("ble:foreign"),
            )]))
        });
        let bluetooth = bluetooth_value();
        let invalid = promise_result(
            bluetooth.call_method("requestDevice", vec![Value::object(HashMap::new())]),
        );
        assert_eq!(invalid.unwrap_err(), "TypeError");
        let denied = promise_result(bluetooth.call_method("requestDevice", vec![heart_request()]));
        assert_eq!(denied.unwrap_err(), "NotAllowedError");
        assert_eq!(
            calls.get(),
            0,
            "requestDevice must not reach the host without a grant"
        );
        w3cos_core::host::unregister_host_module("web-bluetooth");

        crate::bluetooth_adapter::install_for_tests();
        crate::bluetooth_adapter::set_mock_radio(true, vec![mock_heart()]);
        let bluetooth = bluetooth_value();
        let none = promise_result(bluetooth.call_method("getDevices", vec![])).unwrap();
        assert_eq!(none.iter().count(), 0);
        crate::capability_gate::grant("bluetooth");
        let blocked = promise_result(bluetooth.call_method(
            "requestDevice",
            vec![Value::object(HashMap::from([(
                "filters".into(),
                Value::array(vec![Value::object(HashMap::from([(
                    "services".into(),
                    Value::array(vec![Value::string("human_interface_device")]),
                )]))]),
            )]))],
        ));
        assert_eq!(blocked.unwrap_err(), "SecurityError");
        let device =
            promise_result(bluetooth.call_method("requestDevice", vec![heart_request()])).unwrap();
        assert_eq!(
            device.get_property("id").to_js_string(),
            "ble:10:00:00:00:00:01"
        );
        assert!(w3cos_core::class::instance_of(
            &device,
            &interface_class("BluetoothDevice")
        ));
        let listed = promise_result(bluetooth.call_method("getDevices", vec![])).unwrap();
        assert!(listed.iter().next().unwrap().strict_eq(&device));
        let gatt = device.get_property("gatt");
        let offline = promise_result(
            gatt.call_method("getPrimaryService", vec![Value::string("heart_rate")]),
        );
        assert_eq!(offline.unwrap_err(), "NetworkError");
        promise_result(gatt.call_method("connect", vec![])).unwrap();
        assert!(gatt.get_property("connected").to_bool());
        let service = promise_result(
            gatt.call_method("getPrimaryService", vec![Value::string("heart_rate")]),
        )
        .unwrap();
        let characteristic = promise_result(service.call_method(
            "getCharacteristic",
            vec![Value::string("heart_rate_measurement")],
        ))
        .unwrap();
        assert!(
            characteristic
                .get_property("properties")
                .get_property("notify")
                .to_bool()
        );
        assert!(
            !characteristic
                .get_property("properties")
                .get_property("write")
                .to_bool()
        );
        let view = promise_result(characteristic.call_method("readValue", vec![])).unwrap();
        assert_eq!(
            w3cos_core::binary::bytes_of(&view.get_property("buffer")).unwrap(),
            vec![9]
        );
        let descriptor = promise_result(characteristic.call_method(
            "getDescriptor",
            vec![Value::string("client_characteristic_configuration")],
        ))
        .unwrap();
        let blocked_write = promise_result(descriptor.call_method(
            "writeValue",
            vec![w3cos_core::binary::array_buffer_value(vec![1, 0])],
        ));
        assert_eq!(blocked_write.unwrap_err(), "SecurityError");
        let hits = Rc::new(Cell::new(0u32));
        let hits_for_listener = Rc::clone(&hits);
        characteristic.call_method(
            "addEventListener",
            vec![
                Value::string("characteristicvaluechanged"),
                Value::function(move |_, _| {
                    hits_for_listener.set(hits_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        promise_result(characteristic.call_method("startNotifications", vec![])).unwrap();
        crate::bluetooth_adapter::emit_notification(
            "ble:10:00:00:00:00:01",
            "heart_rate",
            "heart_rate_measurement",
            vec![4, 5],
        );
        crate::jsdom::drain_microtasks();
        assert_eq!(hits.get(), 1);
        assert_eq!(
            w3cos_core::binary::bytes_of(
                &characteristic.get_property("value").get_property("buffer")
            )
            .unwrap(),
            vec![4, 5]
        );
        let drops = Rc::new(Cell::new(0u32));
        let drops_for_listener = Rc::clone(&drops);
        device.call_method(
            "addEventListener",
            vec![
                Value::string("gattserverdisconnected"),
                Value::function(move |_, _| {
                    drops_for_listener.set(drops_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        gatt.call_method("disconnect", vec![]);
        crate::jsdom::drain_microtasks();
        assert!(!gatt.get_property("connected").to_bool());
        assert_eq!(drops.get(), 1);
        crate::capability_gate::reset();
        crate::bluetooth_adapter::set_chooser(None);
    }

    #[test]
    fn hardware_availability_is_opt_in() {
        if std::env::var_os("W3COS_BLUETOOTH_DEVICE_TESTS").is_none() {
            eprintln!("skipping bluetooth device test: set W3COS_BLUETOOTH_DEVICE_TESTS=1");
            return;
        }
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                crate::bluetooth_adapter::set_hardware_test(false);
            }
        }
        let _clear = Clear;
        crate::capability_gate::reset();
        crate::capability_gate::grant("bluetooth");
        crate::dom::reset_document();
        crate::jsdom::reset_bridge();
        crate::bluetooth_adapter::install_for_tests();
        crate::bluetooth_adapter::set_hardware_test(true);
        let bluetooth = bluetooth_value();
        let available = promise_result(bluetooth.call_method("getAvailability", vec![])).unwrap();
        assert!(available.as_bool().is_some());
        if let Ok(address) = std::env::var("W3COS_BLUETOOTH_DEVICE_ADDRESS") {
            let device = promise_result(bluetooth.call_method(
                "requestDevice",
                vec![Value::object(HashMap::from([(
                    "acceptAllDevices".into(),
                    Value::Bool(true),
                )]))],
            ))
            .expect("named device was not discovered");
            let id = device.get_property("id").to_js_string();
            assert!(
                id.to_ascii_lowercase()
                    .contains(&address.to_ascii_lowercase()),
                "{id}"
            );
            let gatt = device.get_property("gatt");
            promise_result(gatt.call_method("connect", vec![])).unwrap();
            gatt.call_method("disconnect", vec![]);
            crate::jsdom::drain_microtasks();
        }
        crate::capability_gate::reset();
    }
}
