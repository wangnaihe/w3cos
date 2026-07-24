//! Web Bluetooth facade for `navigator.bluetooth`.
//!
//! The runtime owns the standards-shaped JavaScript objects while an embedding
//! host owns platform discovery, permission prompts and GATT I/O through the
//! `web-bluetooth` host module.

use std::collections::HashMap;

use w3cos_core::Value;

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

fn characteristic_value(
    device_id: String,
    service_uuid: String,
    characteristic_uuid: String,
) -> Value {
    let value = Value::object(HashMap::from([
        ("uuid".to_string(), Value::string(&characteristic_uuid)),
        (
            "service".to_string(),
            Value::object(HashMap::from([(
                "uuid".to_string(),
                Value::string(&service_uuid),
            )])),
        ),
    ]));
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
    let read_device_id = device_id;
    let read_service_uuid = service_uuid;
    let read_characteristic_uuid = characteristic_uuid;
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
                w3cos_core::class::construct(
                    &w3cos_core::binary::data_view_class(),
                    vec![w3cos_core::binary::array_buffer_value(bytes)],
                )
            }))
        }),
    );
    value
}

fn service_value(device_id: String, service_uuid: String) -> Value {
    let value = Value::object(HashMap::from([(
        "uuid".to_string(),
        Value::string(&service_uuid),
    )]));
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
                ("device_id".to_string(), Value::string(&device_id)),
                ("service_uuid".to_string(), Value::string(&service_uuid)),
                (
                    "characteristic_uuid".to_string(),
                    Value::string(&characteristic_uuid),
                ),
            ]));
            promise(host_call("get_characteristic", payload).map(|_| {
                characteristic_value(device_id.clone(), service_uuid.clone(), characteristic_uuid)
            }))
        }),
    );
    value
}

fn gatt_server_value(device_id: String) -> Value {
    let value = Value::object(HashMap::from([
        ("connected".to_string(), Value::Bool(true)),
        ("device".to_string(), Value::Null),
    ]));
    let service_device_id = device_id.clone();
    value.set_property(
        "getPrimaryService",
        Value::function(move |_, args| {
            let service_uuid = args.first().cloned().unwrap_or_default().to_js_string();
            if service_uuid.is_empty() {
                return promise(Err(dom_error("TypeError", "A service UUID is required")));
            }
            let payload = Value::object(HashMap::from([
                ("device_id".to_string(), Value::string(&service_device_id)),
                ("service_uuid".to_string(), Value::string(&service_uuid)),
            ]));
            promise(
                host_call("get_primary_service", payload)
                    .map(|_| service_value(service_device_id.clone(), service_uuid)),
            )
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
    let gatt = Value::object(HashMap::from([
        ("connected".to_string(), Value::Bool(false)),
        ("device".to_string(), device.clone()),
    ]));
    let connect_device_id = device_id.clone();
    let connect_device = device.clone();
    gatt.set_property(
        "connect",
        Value::function(move |this, _| {
            let payload = Value::object(HashMap::from([(
                "device_id".to_string(),
                Value::string(&connect_device_id),
            )]));
            promise(host_call("connect", payload).map(|_| {
                let server = gatt_server_value(connect_device_id.clone());
                server.set_property("device", connect_device.clone());
                this.set_property("connected", Value::Bool(true));
                server
            }))
        }),
    );
    device.set_property("gatt", gatt);
    device
}

pub fn bluetooth_value() -> Value {
    let bluetooth = Value::object(HashMap::new());
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
        assert!(gatt.get_property("connect").is_function());
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
    }
}
