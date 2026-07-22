//! W3C Feature Matrix — integration smoke for every public API surface.
//!
//! Run: `cargo test -p w3cos-runtime --test w3c_feature_matrix`

use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[test]
fn dom_document_tree_and_query() {
    use w3cos_dom::document::Document;

    let mut doc = Document::new();
    let root = doc.create_element("div");
    root.set_attribute(&mut doc, "id", "app");
    doc.body().append_child(&mut doc, root);

    let btn = doc.create_element("button");
    btn.set_text_content(&mut doc, "Go");
    root.append_child(&mut doc, btn);

    let found = doc.query_selector("button").expect("query_selector button");
    assert_eq!(found.text_content(&doc), Some("Go"));
}

#[test]
fn dom_click_event_listener() {
    use w3cos_dom::document::Document;
    use w3cos_dom::events::Event;

    let mut doc = Document::new();
    let btn = doc.create_element("button");
    doc.body().append_child(&mut doc, btn);

    let fired = Arc::new(AtomicBool::new(false));
    let fired_clone = fired.clone();
    btn.add_event_listener(
        &mut doc,
        "click",
        Box::new(move |_| {
            fired_clone.store(true, Ordering::SeqCst);
        }),
    );

    let mut ev = Event::click(btn.id, 0.0, 0.0);
    btn.dispatch_event(&mut doc, &mut ev);
    assert!(fired.load(Ordering::SeqCst));
}

#[test]
fn web_storage_local_storage_roundtrip() {
    let key = format!("w3c_matrix_{}", std::process::id());
    w3cos_runtime::storage::set_item(&key, "w3cos");
    assert_eq!(
        w3cos_runtime::storage::get_item(&key).as_deref(),
        Some("w3cos")
    );
    w3cos_runtime::storage::remove_item(&key);
    assert!(w3cos_runtime::storage::get_item(&key).is_none());
}

#[test]
fn indexed_db_put_get_delete() {
    use w3cos_runtime::indexed_db::{self, TransactionMode};

    let dir = tempfile::tempdir().unwrap();
    indexed_db::set_base_dir(dir.path().to_path_buf());

    let db = indexed_db::open("matrix_test", 1, |db, _old, _new| {
        db.create_object_store("items", "id", true)
    })
    .unwrap();

    let tx = db
        .transaction(&["items"], TransactionMode::ReadWrite)
        .unwrap();
    let store = tx.object_store("items").unwrap();
    let key = store.put(json!({"id": 1, "label": "hello"})).unwrap();
    assert_eq!(key, json!(1));

    let got = store.get(&json!(1)).unwrap().unwrap();
    assert_eq!(got["label"], "hello");

    indexed_db::delete("matrix_test").unwrap();
}

#[test]
fn history_push_and_back() {
    w3cos_runtime::history::reset();
    w3cos_runtime::history::push_state(Some(r#"{"page":1}"#), "", "/a");
    w3cos_runtime::history::push_state(Some(r#"{"page":2}"#), "", "/b");
    assert_eq!(w3cos_runtime::history::get_pathname(), "/b");
    w3cos_runtime::history::back();
    assert_eq!(w3cos_runtime::history::get_pathname(), "/a");
}

#[test]
fn dedicated_worker_message_roundtrip() {
    use w3cos_runtime::worker::{Worker, WorkerOptions};

    let worker = Worker::spawn(WorkerOptions::default(), |scope| {
        while let Some(msg) = scope.recv() {
            let n = msg.get("n").and_then(|v| v.as_u64()).unwrap_or(0);
            scope.post_message(json!({"square": n * n})).ok();
        }
    });

    worker.post_message(json!({"n": 6})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut got = false;
    while Instant::now() < deadline {
        if let Some(reply) = worker.try_recv() {
            assert_eq!(reply["square"], json!(36));
            got = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    worker.terminate();
    assert!(got, "worker should reply with square");
}

#[test]
fn ipc_message_serialization_roundtrip() {
    use w3cos_runtime::ipc::IpcMessage;

    let msg = IpcMessage::new("ping", json!({"ok": true}));
    let bytes = serde_json::to_vec(&msg).unwrap();
    let decoded: IpcMessage = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded.channel, "ping");
    assert_eq!(decoded.payload["ok"], json!(true));
}

#[test]
fn timers_timeout_fires() {
    use w3cos_runtime::state;
    use w3cos_runtime::timers;
    use w3cos_std::EventAction;

    let id = state::create_signal(0);
    let timer_id = timers::set_timeout(EventAction::Set(id, 99), 5);
    assert!(timer_id > 0);

    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline {
        for action in timers::tick() {
            state::execute_action(&action);
        }
        if state::get_signal(id) == 99 {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("setTimeout did not fire");
}

#[test]
fn text_encoder_decoder_utf8() {
    use w3cos_runtime::text_encoding::{TextDecoder, TextEncoder};

    let enc = TextEncoder::new();
    let bytes = enc.encode("W3C OS");
    let dec = TextDecoder::new("utf-8");
    let text = dec.decode(&bytes).unwrap();
    assert_eq!(text, "W3C OS");
}

#[test]
fn readable_stream_from_bytes() {
    use w3cos_runtime::streams::{ReadResult, ReadableStream};

    let stream = ReadableStream::from_bytes(b"matrix".to_vec());
    let reader = stream.get_reader();
    match reader.read() {
        ReadResult::Chunk(c) => assert_eq!(c, b"matrix"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn canvas2d_path_and_stroke() {
    use w3cos_runtime::canvas2d::CanvasRenderingContext2D;

    let mut ctx = CanvasRenderingContext2D::new(64, 64);
    ctx.set_stroke_style("#00ff00");
    ctx.begin_path();
    ctx.move_to(0.0, 0.0);
    ctx.line_to(32.0, 32.0);
    ctx.stroke();
    let px = ctx.get_image_data(16, 16, 1, 1);
    assert!(px.data[1] > 200, "green channel along stroke");
}

#[test]
fn filesystem_read_write_text() {
    use w3cos_runtime::fs;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.txt");
    let path_str = path.to_string_lossy();
    let write = fs::write_text_file(&path_str, "w3c-fs");
    assert!(write.ok);
    let read = fs::read_text_file(&path_str);
    assert_eq!(read.text, "w3c-fs");
}

#[test]
fn pwa_manifest_parse_and_display_name() {
    use w3cos_runtime::pwa::PwaManifest;

    let manifest =
        PwaManifest::from_json(r#"{"name":"LogiDesk","short_name":"LD","display":"standalone"}"#)
            .unwrap();
    assert_eq!(manifest.display_name(), "LogiDesk");
}

#[test]
fn media_query_matches_viewport() {
    use w3cos_runtime::media::{MediaCondition, Viewport, matches_media};

    let cond = MediaCondition::MinWidth(800.0);
    let vp = Viewport::new(1024.0, 768.0, 2.0);
    assert!(matches_media(&cond, &vp));
}

#[test]
fn reactive_signal_execute_action() {
    use w3cos_runtime::state;
    use w3cos_std::EventAction;

    let id = state::create_signal(0);
    state::execute_action(&EventAction::Increment(id));
    assert_eq!(state::get_signal(id), 1);
    state::execute_action(&EventAction::Set(id, 42));
    assert_eq!(state::get_signal(id), 42);
}

#[test]
fn flex_layout_column_row() {
    use w3cos_runtime::layout;
    use w3cos_std::color::Color;
    use w3cos_std::style::*;
    use w3cos_std::{Component, Style};

    let ui = Component::column(
        Style {
            gap: 8.0,
            padding: Edges::all(16.0),
            flex_grow: 1.0,
            ..Style::default()
        },
        vec![
            Component::text("A", Style::default()),
            Component::row(
                Style {
                    gap: 4.0,
                    ..Style::default()
                },
                vec![Component::button(
                    "OK",
                    Style {
                        background: Color::from_hex("#2563eb"),
                        ..Style::default()
                    },
                )],
            ),
        ],
    );
    let rects = layout::compute(&ui, 400.0, 300.0).unwrap();
    assert!(!rects.is_empty());
    assert!(rects.iter().all(|(r, _)| r.width > 0.0 && r.height > 0.0));
}

#[test]
fn a11y_tree_from_dom_document() {
    use w3cos_a11y::tree::{build_a11y_tree, flatten_for_ai};
    use w3cos_dom::document::Document;

    let mut doc = Document::new();
    let btn = doc.create_element("button");
    btn.set_text_content(&mut doc, "Submit");
    doc.body().append_child(&mut doc, btn);

    let tree = build_a11y_tree(&doc);
    let flat = flatten_for_ai(&tree);
    assert!(
        flat.iter().any(|line| line.contains("Submit")),
        "a11y tree should expose button label"
    );
}

#[test]
fn ecma_proxy_get_trap() {
    use std::collections::HashMap;
    use w3cos_core::{JsObject, ProxyBuilder, Value};

    let mut props = HashMap::new();
    props.insert("x".into(), Value::Number(1.0));
    let handler = ProxyBuilder::new()
        .get(|_target, key, _receiver| Value::String(format!("proxy:{key}")))
        .build();
    let obj = JsObject::with_proxy(props, handler);
    let receiver = Value::Undefined;
    match obj.get("x", &receiver) {
        Value::String(s) => assert_eq!(s, "proxy:x"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn frame_cache_store_and_png() {
    use w3cos_runtime::frame_cache;

    let pixels = vec![255u8; 4 * 4 * 4];
    frame_cache::store(4, 4, pixels);
    assert!(frame_cache::has_frame());
    let png = frame_cache::encode_png().expect("png");
    assert_eq!(&png[..4], &[0x89, 0x50, 0x4E, 0x47]);
}

#[test]
fn resize_observer_detects_size_change() {
    use w3cos_dom::node::NodeId;
    use w3cos_runtime::observers::ResizeObserver;

    let mut ro = ResizeObserver::new();
    let target = NodeId::from_u32(1);
    ro.observe(target);
    let entries = ro.check_for_changes(&[(target, 100.0, 50.0)]);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].content_width, 100.0);
    let stable = ro.check_for_changes(&[(target, 100.0, 50.0)]);
    assert!(
        stable.is_empty(),
        "no duplicate entries when size unchanged"
    );
}

#[test]
fn dom_to_component_tree_smoke() {
    use w3cos_dom::document::Document;
    use w3cos_runtime::dom;
    use w3cos_std::component::ComponentKind;

    let mut doc = Document::new();
    let el = doc.create_element("div");
    el.style_mut(&mut doc).set_property("display", "flex");
    doc.body().append_child(&mut doc, el);

    dom::reset_document();
    let component = dom::to_component_tree();
    assert!(matches!(
        component.kind,
        ComponentKind::Column | ComponentKind::Root
    ));
}
