//! Headless demo for the native-gap adapters.
//!
//! Exercises permission denial, a real process, window-management displays,
//! battery (or an honest `NotSupportedError`), an application shortcut, the
//! origin-scoped keyring (or `NotSupportedError` when no secret store is
//! open), a system-tray adapter (or an honest `NotSupportedError` when the
//! session has no status watcher), display capture, a TCP echo, a WebNN add,
//! and Gamepad enumeration through the desktop adapter (or the host snapshot
//! path).
//! Rasterizes with the CPU renderer.
//!
//! ```sh
//! DISPLAY=:1 cargo +stable run -p w3cos-runtime --example native_gaps \
//!   --no-default-features --features cpu-render
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::rc::Rc;

use w3cos_core::Value;
use w3cos_runtime::capability_gate;
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

fn gamepad_probe() -> (String, bool) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        w3cos_runtime::gamepad_web::get_gamepads_value().call(Value::Undefined, vec![])
    }));
    match result {
        Ok(list) => {
            let backend = w3cos_runtime::gamepad_web::platform_backend_name();
            let length = list.get_property("length").to_u32();
            let mut connected = 0u32;
            for index in 0..length {
                if !list.get_property(&index.to_string()).is_null() {
                    connected += 1;
                }
            }
            (
                format!("{backend} connected {connected}"),
                backend != "gilrs-unavailable",
            )
        }
        Err(_) => ("call failed".into(), false),
    }
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

fn main() {
    dom::reset_document();
    jsdom::reset_bridge();
    capability_gate::reset();
    platform_host::reset_overrides();
    direct_sockets_web::reset_realm();
    webnn_web::reset_realm();
    w3cos_extensions::reset_realm();

    let namespace = namespace_value();
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
    let capture = await_value(
        namespace
            .get_property("agent")
            .call_method("captureScreen", vec![]),
    );
    let tcp = tcp_echo();
    let webnn = webnn_add();

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
    let (gamepad_text, gamepad_ok) = gamepad_probe();
    row("gamepad", &gamepad_text, gamepad_ok);

    let frame = render_document_rgba_cpu(960, 780).expect("cpu frame");
    let path = std::path::PathBuf::from("/opt/cursor/artifacts/native-gaps.png");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    headless::write_frame_png(&path, &frame).expect("png");
    let copy = std::path::PathBuf::from("/tmp/native-gaps.png");
    headless::write_frame_png(&copy, &frame).expect("png copy");
    println!("wrote {}", path.display());
}
