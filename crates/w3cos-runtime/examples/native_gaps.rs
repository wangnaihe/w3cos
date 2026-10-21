//! Headless demo for the native-gap adapters.
//!
//! Exercises permission denial, a real process, window-management displays,
//! battery (or an honest `NotSupportedError`), an accelerometer reading (or an
//! honest `NotReadableError` when this host has no live sensor), Bluetooth
//! availability (or `NotSupportedError` / `NotFoundError`), a camera
//! frame (or an honest `NotFoundError` / `NotAllowedError` / `NotReadableError`
//! when this host has no usable camera), an application shortcut, the
//! origin-scoped keyring (or `NotSupportedError` when no secret store is
//! open), a system-tray adapter (or an honest `NotSupportedError` when the
//! session has no status watcher), display capture, Web Serial (permission
//! or adapter rejection, then `NotFoundError` for a filter that matches
//! nothing), a TCP echo, and a WebNN add.
//! Rasterizes with the CPU renderer.
//!
//! ```sh
//! DISPLAY=:1 cargo +stable run -p w3cos-runtime --example native_gaps \
//!   --no-default-features --features cpu-render,web-sensors,web-bluetooth
//! # Optional: add web-device-access to print live hidapi/nusb summaries.
//! # The probe does not grant hid/usb, so requestDevice must not succeed.
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::rc::Rc;

use w3cos_core::Value;
use w3cos_runtime::capability_gate;
use w3cos_runtime::device_access_web;
use w3cos_runtime::direct_sockets_web::{self, tcp_socket_class};
use w3cos_runtime::dom;
use w3cos_runtime::headless::{self, render_document_rgba_cpu};
use w3cos_runtime::jsdom;
use w3cos_runtime::platform_host;
use w3cos_runtime::w3cos_extensions::{self, dispatch_accelerator, namespace_value};
use w3cos_runtime::webnn_web::{self, ml_graph_builder_class, ml_value};

fn await_value(promise: Value) -> Result<Value, String> {
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
    jsdom::drain_microtasks();
    if let Some(error) = err.borrow().clone() {
        return Err(error);
    }
    ok.borrow().clone().ok_or_else(|| "unresolved".into())
}

fn row(label: &str, detail: &str, ok: bool) {
    let line = dom::create_element("div");
    dom::set_style_property(line, "display", "flex");
    dom::set_style_property(line, "align-items", "center");
    dom::set_style_property(line, "height", "44px");
    dom::set_style_property(line, "margin", "8px 16px");
    dom::set_style_property(line, "padding", "0 16px");
    dom::set_style_property(line, "border-radius", "8px");
    dom::set_style_property(line, "background", if ok { "#1f6b45" } else { "#8a5a12" });
    dom::set_style_property(line, "color", "#f4f7f8");
    dom::set_style_property(line, "font-size", "18px");
    let text = dom::create_text_node(&format!("{label}  {detail}"));
    dom::append_child(line, text);
    dom::append_child(dom::body_id(), line);
    println!("{label}: {detail}");
}

fn tcp_echo() -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| err.to_string())?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().map_err(|err| err.to_string())?;
        let mut buffer = [0u8; 16];
        let count = stream.read(&mut buffer).map_err(|err| err.to_string())?;
        stream
            .write_all(&buffer[..count])
            .map_err(|err| err.to_string())?;
        Ok::<_, String>(())
    });
    let socket = w3cos_core::class::construct(
        &tcp_socket_class(),
        vec![Value::string("127.0.0.1"), Value::Number(f64::from(port))],
    );
    let opened = await_value(socket.get_property("opened"))?;
    let writer = opened
        .get_property("writable")
        .call_method("getWriter", vec![]);
    let bytes = w3cos_core::binary::array_buffer_value(b"ping".to_vec());
    let chunk = w3cos_core::class::construct(
        &w3cos_core::binary::typed_array_class("Uint8Array"),
        vec![bytes],
    );
    await_value(writer.call_method("write", vec![chunk]))?;
    let reader = opened
        .get_property("readable")
        .call_method("getReader", vec![]);
    let read = await_value(reader.call_method("read", vec![]))?;
    let echoed = w3cos_core::binary::bytes_of(&read.get_property("value")).unwrap_or_default();
    let _ = server.join();
    Ok(String::from_utf8_lossy(&echoed).to_string())
}

fn webnn_add() -> Result<String, String> {
    let context = await_value(ml_value().call_method("createContext", vec![]))?;
    let builder = w3cos_core::class::construct(&ml_graph_builder_class(), vec![context.clone()]);
    let shape = Value::object(HashMap::from([
        ("dataType".into(), Value::string("float32")),
        ("dimensions".into(), Value::array(vec![Value::Number(2.0)])),
    ]));
    let input = builder.call_method("input", vec![Value::string("a"), shape.clone()]);
    let constant_bytes = [1.0f32, 2.0]
        .into_iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    let constant = builder.call_method(
        "constant",
        vec![
            shape,
            w3cos_core::class::construct(
                &w3cos_core::binary::typed_array_class("Float32Array"),
                vec![w3cos_core::binary::array_buffer_value(constant_bytes)],
            ),
        ],
    );
    let added = builder.call_method("add", vec![input, constant]);
    let graph = await_value(builder.call_method(
        "build",
        vec![Value::object(HashMap::from([("output".into(), added)]))],
    ))?;
    let feed_bytes = [3.0f32, 4.0]
        .into_iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    let feeds = Value::object(HashMap::from([(
        "a".into(),
        w3cos_core::class::construct(
            &w3cos_core::binary::typed_array_class("Float32Array"),
            vec![w3cos_core::binary::array_buffer_value(feed_bytes)],
        ),
    )]));
    let results = await_value(context.call_method("compute", vec![graph, feeds]))?;
    let output = results.get_property("output");
    let bytes = w3cos_core::binary::bytes_of(&output).unwrap_or_default();
    let values = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    Ok(values)
}

fn device_probe(kind: &str) -> (String, bool) {
    let manager = if kind == "hid" {
        device_access_web::hid_value()
    } else {
        device_access_web::usb_value()
    };
    let summary = if kind == "hid" {
        device_access_web::hid_backend_summary()
    } else {
        device_access_web::usb_backend_summary()
    };
    let listed = await_value(manager.call_method("getDevices", vec![]));
    let requested = await_value(manager.call_method(
        "requestDevice",
        vec![Value::object(HashMap::from([(
            "filters".into(),
            Value::array(vec![]),
        )]))],
    ));
    let list_len = listed
        .as_ref()
        .map(|devices| devices.get_property("length").to_number())
        .unwrap_or(-1.0);
    let request_error = match &requested {
        Ok(_) => "unexpected success".to_string(),
        Err(err) => err.clone(),
    };
    let ok =
        list_len == 0.0 && matches!(request_error.as_str(), "NotFoundError" | "NotAllowedError");
    (
        format!("{summary} devices {list_len} request {request_error}"),
        ok,
    )
}

fn sensor_probe() -> (String, bool) {
    w3cos_runtime::orientation_web::set_permission_granted(true);
    capability_gate::grant("accelerometer");
    let sensor = w3cos_core::class::construct(
        &w3cos_runtime::sensors_web::accelerometer_class(),
        vec![Value::object(HashMap::from([(
            "frequency".into(),
            Value::Number(10.0),
        )]))],
    );
    let error = Rc::new(RefCell::new(String::new()));
    let error_slot = Rc::clone(&error);
    sensor.call_method(
        "addEventListener",
        vec![
            Value::string("error"),
            Value::function(move |_, args| {
                *error_slot.borrow_mut() = args
                    .first()
                    .map(|event| {
                        event
                            .get_property("error")
                            .get_property("name")
                            .to_js_string()
                    })
                    .unwrap_or_default();
                Value::Undefined
            }),
        ],
    );
    sensor.call_method("start", vec![]);
    for _ in 0..30 {
        jsdom::drain_microtasks();
        if sensor.get_property("hasReading").to_bool() || !error.borrow().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    let has_reading = sensor.get_property("hasReading").to_bool();
    let text = if has_reading {
        format!(
            "x {} y {} z {}",
            sensor.get_property("x").to_number(),
            sensor.get_property("y").to_number(),
            sensor.get_property("z").to_number()
        )
    } else if error.borrow().is_empty() {
        "no live sample".to_string()
    } else {
        error.borrow().clone()
    };
    let ok = has_reading || error.borrow().as_str() == "NotReadableError";
    sensor.call_method("stop", vec![]);
    w3cos_runtime::orientation_web::reset();
    w3cos_runtime::sensors_web::reset();
    (text, ok)
}

fn bluetooth_probe() -> (String, bool) {
    let bluetooth = w3cos_runtime::bluetooth_web::bluetooth_value();
    let request = Value::object(HashMap::from([(
        "acceptAllDevices".into(),
        Value::Bool(true),
    )]));
    let denied = await_value(bluetooth.call_method("requestDevice", vec![request.clone()]));
    if matches!(denied.as_ref(), Err(name) if name == "NotSupportedError") {
        return ("NotSupportedError".to_string(), true);
    }
    let denied_ok = matches!(denied.as_ref(), Err(name) if name == "NotAllowedError");
    capability_gate::grant("bluetooth");
    let available = await_value(bluetooth.call_method("getAvailability", vec![]));
    let available_text = match &available {
        Ok(value) => format!("available {}", value.to_bool()),
        Err(err) => err.clone(),
    };
    let found = await_value(bluetooth.call_method("requestDevice", vec![request]));
    let detail = match &found {
        Ok(device) => format!(
            "{available_text}; {}",
            device.get_property("id").to_js_string()
        ),
        Err(err) => format!("{available_text}; {err}"),
    };
    let ok = denied_ok
        && available
            .as_ref()
            .is_ok_and(|value| value.as_bool().is_some())
        && match &found {
            Ok(device) => device.get_property("id").to_js_string().starts_with("ble:"),
            Err(err) => err == "NotFoundError",
        };
    (detail, ok)
}

fn camera_probe() -> (String, bool) {
    capability_gate::grant("camera");
    let media = w3cos_runtime::media_devices_web::media_devices_value();
    let opened = await_value(media.call_method(
        "getUserMedia",
        vec![Value::object(HashMap::from([(
            "video".into(),
            Value::Bool(true),
        )]))],
    ));
    let result = match opened {
        Ok(stream) => {
            let track = stream
                .call_method("getVideoTracks", vec![])
                .get_property("0");
            let settings = track.call_method("getSettings", vec![]);
            let width = settings.get_property("width").to_number();
            let height = settings.get_property("height").to_number();
            let label = track.get_property("label").to_js_string();
            track.call_method("stop", vec![]);
            (
                format!("{label} {width}x{height}"),
                width > 0.0 && height > 0.0,
            )
        }
        Err(err) => {
            let honest = matches!(
                err.as_str(),
                "NotFoundError" | "NotAllowedError" | "NotReadableError" | "OverconstrainedError"
            );
            (err, honest)
        }
    };
    w3cos_runtime::media_devices_web::reset();
    result
}

fn main() {
    dom::reset_document();
    jsdom::reset_bridge();
    capability_gate::reset();
    platform_host::reset_overrides();
    direct_sockets_web::reset_realm();
    webnn_web::reset_realm();
    w3cos_extensions::reset_realm();

    let namespace = namespace_value();
    let serial_before = await_value(
        w3cos_runtime::device_access_web::serial_value().call_method("requestPort", vec![]),
    );

    let denied = await_value(namespace.get_property("process").call_method(
        "exec",
        vec![Value::object(HashMap::from([(
            "file".into(),
            Value::string("/bin/echo"),
        )]))],
    ));

    for name in [
        "w3cos-process",
        "window-management",
        "battery",
        "w3cos-global-shortcuts",
        "w3cos-tray",
        "display-capture",
        "direct-sockets",
        "webnn",
        "w3cos-keyring",
        "serial",
    ] {
        capability_gate::grant(name);
    }

    let echoed = await_value(namespace.get_property("process").call_method(
        "exec",
        vec![Value::object(HashMap::from([
            ("file".into(), Value::string("/bin/echo")),
            (
                "args".into(),
                Value::array(vec![Value::string("native-gaps")]),
            ),
        ]))],
    ));
    let screens = await_value(w3cos_runtime::screen_details_web::get_screen_details());
    let battery =
        await_value(w3cos_runtime::battery_web::get_battery_value().call(Value::Undefined, vec![]));
    let hits = Rc::new(RefCell::new(0u32));
    let hits_for_callback = Rc::clone(&hits);
    let shortcut = await_value(namespace.get_property("shortcuts").call_method(
        "register",
        vec![
            Value::string("Control+Shift+K"),
            Value::function(move |_, _| {
                *hits_for_callback.borrow_mut() += 1;
                Value::Undefined
            }),
            Value::object(HashMap::from([("global".into(), Value::Bool(false))])),
        ],
    ));
    if shortcut.is_ok() {
        dispatch_accelerator("Control+Shift+K");
    }
    let keyring = await_value(namespace.get_property("keyring").call_method(
        "set",
        vec![Value::string("native-gaps"), Value::string("probe")],
    ));
    let keyring_read = if keyring.is_ok() {
        let read = await_value(
            namespace
                .get_property("keyring")
                .call_method("get", vec![Value::string("native-gaps")]),
        );
        let _ = await_value(
            namespace
                .get_property("keyring")
                .call_method("delete", vec![Value::string("native-gaps")]),
        );
        read
    } else {
        keyring.clone()
    };
    let tray = await_value(namespace.get_property("tray").call_method(
        "show",
        vec![Value::object(HashMap::from([(
            "tooltip".into(),
            Value::string("w3cos native gaps"),
        )]))],
    ));
    if let Ok(handle) = &tray {
        let _ = await_value(handle.call_method("remove", vec![]));
    }
    let serial_after = await_value(
        w3cos_runtime::device_access_web::serial_value().call_method(
            "requestPort",
            vec![Value::object(HashMap::from([(
                "filters".into(),
                Value::array(vec![Value::object(HashMap::from([
                    ("usbVendorId".into(), Value::Number(65535.0)),
                    ("usbProductId".into(), Value::Number(65535.0)),
                ]))]),
            )]))],
        ),
    );
    let capture = await_value(
        namespace
            .get_property("agent")
            .call_method("captureScreen", vec![]),
    );
    let tcp = tcp_echo();
    let webnn = webnn_add();
    let hid = device_probe("hid");
    let usb = device_probe("usb");

    dom::set_style_property(dom::body_id(), "margin", "0");
    dom::set_style_property(dom::body_id(), "background", "#101418");
    let title = dom::create_element("div");
    dom::set_style_property(title, "color", "#f4f7f8");
    dom::set_style_property(title, "font-size", "22px");
    dom::set_style_property(title, "padding", "16px 16px 4px");
    dom::append_child(title, dom::create_text_node("w3cos native gaps"));
    dom::append_child(dom::body_id(), title);

    let denied_text = match &denied {
        Ok(_) => "unexpected success".to_string(),
        Err(err) => err.clone(),
    };
    row(
        "process denied",
        &denied_text,
        denied_text == "NotAllowedError",
    );
    let exec_text = match &echoed {
        Ok(value) => value.get_property("stdout").to_js_string(),
        Err(err) => err.clone(),
    };
    row(
        "process exec",
        &exec_text,
        exec_text.contains("native-gaps"),
    );
    let screen_text = match &screens {
        Ok(details) => {
            let screen = details.get_property("currentScreen");
            format!(
                "{} {}x{}",
                screen.get_property("label").to_js_string(),
                screen.get_property("width").to_number(),
                screen.get_property("height").to_number()
            )
        }
        Err(err) => err.clone(),
    };
    row("screens", &screen_text, screens.is_ok());
    let battery_text = match &battery {
        Ok(manager) => format!(
            "level {} charging {}",
            manager.get_property("level").to_number(),
            manager.get_property("charging").to_bool()
        ),
        Err(err) => err.clone(),
    };
    let battery_ok = match &battery {
        Ok(_) => true,
        Err(err) => err != "NotAllowedError",
    };
    row("battery", &battery_text, battery_ok);
    let shortcut_text = format!("hits {}", hits.borrow());
    row("shortcut", &shortcut_text, *hits.borrow() == 1);
    let tray_text = match &tray {
        Ok(handle) => format!(
            "id {} {}",
            handle.get_property("id").to_number(),
            handle.get_property("tooltip").to_js_string()
        ),
        Err(err) => err.clone(),
    };
    let keyring_text = match &keyring_read {
        Ok(value) if value.to_js_string() == "probe" => "stored".to_string(),
        Ok(_) => "mismatch".to_string(),
        Err(err) => err.clone(),
    };
    row(
        "keyring",
        &keyring_text,
        keyring_text == "stored" || keyring_text == "NotSupportedError",
    );
    let tray_ok = match &tray {
        Ok(_) => true,
        Err(err) => err == "NotSupportedError",
    };
    row("tray adapter", &tray_text, tray_ok);
    let capture_text = match &capture {
        Ok(image) => format!(
            "{}x{}",
            image.get_property("width").to_number(),
            image.get_property("height").to_number()
        ),
        Err(err) => err.clone(),
    };
    row("capture", &capture_text, capture.is_ok());
    let serial_before_text = match &serial_before {
        Ok(_) => "unexpected success".to_string(),
        Err(err) => err.clone(),
    };
    let serial_after_text = match &serial_after {
        Ok(_) => "unexpected success".to_string(),
        Err(err) => err.clone(),
    };
    row(
        "serial",
        &format!("{serial_before_text} / {serial_after_text}"),
        matches!(
            serial_before_text.as_str(),
            "NotAllowedError" | "NotFoundError"
        ) && serial_after_text == "NotFoundError",
    );
    row(
        "tcp echo",
        &tcp.clone().unwrap_or_else(|err| err),
        tcp.as_deref() == Ok("ping"),
    );
    row(
        "webnn add",
        &webnn.clone().unwrap_or_else(|err| err),
        webnn.as_deref() == Ok("4,6"),
    );
    row("hid", &hid.0, hid.1);
    row("usb", &usb.0, usb.1);
    let (sensor_text, sensor_ok) = sensor_probe();
    row("accelerometer", &sensor_text, sensor_ok);
    let (bluetooth_text, bluetooth_ok) = bluetooth_probe();
    row("bluetooth", &bluetooth_text, bluetooth_ok);
    let (camera_text, camera_ok) = camera_probe();
    row("camera", &camera_text, camera_ok);

    let frame = render_document_rgba_cpu(960, 1100).expect("cpu frame");
    let path = std::path::PathBuf::from("/opt/cursor/artifacts/native-gaps.png");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    headless::write_frame_png(&path, &frame).expect("png");
    let copy = std::path::PathBuf::from("/tmp/native-gaps.png");
    headless::write_frame_png(&copy, &frame).expect("png copy");
    println!("wrote {}", path.display());
}
