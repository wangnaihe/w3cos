//! Integration tests for new W3C standard APIs
//! Tests: ReadableStream, EventSource, Canvas2D, Clipboard, contenteditable, FileSystemObserver, FontFace

use w3cos_runtime::streams::{ReadableStream, ReadResult};
use w3cos_runtime::canvas2d::CanvasRenderingContext2D;
use w3cos_runtime::clipboard::{Clipboard, ClipboardItem};
use w3cos_runtime::fs_watch::{FileSystemObserver, FileSystemFileHandle, FileSystemDirectoryHandle, ObserveOptions, ChangeType};
use w3cos_runtime::font_face::{FontFace, FontRegistry, FontSource, FontWeight, FontFaceStyle, parse_and_register};
use w3cos_dom::events::{EventData, EventType, InputType};
use w3cos_dom::document::Document;
use std::io::Cursor;
use std::time::Duration;
use std::thread;

// ── 1. ReadableStream ──────────────────────────────────────────────────────

#[test]
fn stream_from_reader_chunks() {
    let data = b"Hello, W3C Streams API!";
    let stream = ReadableStream::from_reader(Cursor::new(data), 4);
    let reader = stream.get_reader();
    let mut collected = Vec::new();
    loop {
        match reader.read() {
            ReadResult::Chunk(chunk) => collected.extend_from_slice(&chunk),
            ReadResult::Done => break,
            ReadResult::Error(e) => panic!("stream error: {e}"),
        }
    }
    assert_eq!(collected, data);
    println!("[PASS] ReadableStream: read {} bytes in 4-byte chunks", collected.len());
}

#[test]
fn stream_from_bytes_text() {
    let stream = ReadableStream::from_bytes(b"w3cos rocks".to_vec());
    let reader = stream.get_reader();
    let text = reader.read_to_string().unwrap();
    assert_eq!(text, "w3cos rocks");
    println!("[PASS] ReadableStream::from_bytes -> text: {text}");
}

#[test]
fn stream_locked_flag() {
    let stream = ReadableStream::from_bytes(vec![1, 2, 3]);
    assert!(!stream.locked());
    let reader = stream.get_reader();
    assert!(stream.locked());
    drop(reader);
    assert!(!stream.locked());
    println!("[PASS] ReadableStream lock/unlock lifecycle");
}

// ── 2. Canvas 2D Context ───────────────────────────────────────────────────

#[test]
fn canvas2d_fill_and_read_pixels() {
    let mut ctx = CanvasRenderingContext2D::new(100, 100);
    ctx.set_fill_style("#1e1e1e");
    ctx.fill_rect(0.0, 0.0, 100.0, 100.0);
    ctx.set_fill_style("#ff0000");
    ctx.fill_rect(10.0, 10.0, 20.0, 20.0);

    let bg = ctx.get_image_data(0, 0, 1, 1);
    assert_eq!(bg.data[0], 0x1e, "background R");
    assert_eq!(bg.data[1], 0x1e, "background G");
    assert_eq!(bg.data[2], 0x1e, "background B");

    let red = ctx.get_image_data(15, 15, 1, 1);
    assert_eq!(red.data[0], 255, "red R");
    assert_eq!(red.data[1], 0,   "red G");
    assert_eq!(red.data[2], 0,   "red B");
    println!("[PASS] Canvas2D: fill_rect + pixel verification bg=#1e1e1e, rect=red");
}

#[test]
fn canvas2d_clear_rect() {
    let mut ctx = CanvasRenderingContext2D::new(50, 50);
    ctx.set_fill_style("#ffffff");
    ctx.fill_rect(0.0, 0.0, 50.0, 50.0);
    ctx.clear_rect(10.0, 10.0, 10.0, 10.0);
    let cleared = ctx.get_image_data(12, 12, 1, 1);
    assert_eq!(cleared.data[3], 0, "cleared pixel alpha should be 0");
    println!("[PASS] Canvas2D: clear_rect -> transparent pixels");
}

#[test]
fn canvas2d_save_restore() {
    let mut ctx = CanvasRenderingContext2D::new(50, 50);
    ctx.set_fill_style("#ff0000");
    ctx.save();
    ctx.set_fill_style("#00ff00");
    assert_eq!(ctx.state.fill_style.to_rgba(), (0, 255, 0, 255));
    ctx.restore();
    assert_eq!(ctx.state.fill_style.to_rgba(), (255, 0, 0, 255));
    println!("[PASS] Canvas2D: save/restore state");
}

#[test]
fn canvas2d_measure_text() {
    let mut ctx = CanvasRenderingContext2D::new(400, 100);
    ctx.set_font("14px monospace");
    let m4  = ctx.measure_text("abcd");
    let m8  = ctx.measure_text("abcdefgh");
    assert!(m4.width > 0.0);
    assert!((m8.width - m4.width * 2.0).abs() < 1.0, "width should scale linearly");
    println!("[PASS] Canvas2D: measureText 4-char={:.1}px 8-char={:.1}px", m4.width, m8.width);
}

#[test]
fn canvas2d_stroke_rect() {
    let mut ctx = CanvasRenderingContext2D::new(100, 100);
    ctx.set_fill_style("#000000");
    ctx.fill_rect(0.0, 0.0, 100.0, 100.0);
    ctx.set_stroke_style("#ffffff");
    ctx.set_line_width(2.0);
    ctx.stroke_rect(10.0, 10.0, 80.0, 80.0);
    // Top edge of stroke rect should be white
    let top = ctx.get_image_data(50, 10, 1, 1);
    assert!(top.data[0] > 200, "stroke top edge should be white");
    println!("[PASS] Canvas2D: stroke_rect draws white border on black canvas");
}

// ── 3. Clipboard API ───────────────────────────────────────────────────────

#[test]
fn clipboard_item_construction() {
    let text_item = ClipboardItem::text("copy me");
    assert_eq!(text_item.mime_type, "text/plain");
    assert_eq!(text_item.as_text(), Some("copy me"));

    let html_item = ClipboardItem::html("<b>bold</b>");
    assert_eq!(html_item.mime_type, "text/html");

    let bytes_item = ClipboardItem::bytes("image/png", vec![0x89, 0x50, 0x4e, 0x47]);
    assert_eq!(bytes_item.mime_type, "image/png");
    println!("[PASS] Clipboard: ClipboardItem text/html/bytes construction");
}

#[test]
#[ignore]
fn clipboard_roundtrip() {
    let text = "w3cos clipboard test 12345";
    Clipboard::write_text(text).expect("write_text failed");
    let read_back = Clipboard::read_text().expect("read_text failed");
    assert_eq!(read_back, text);
    println!("[PASS] Clipboard: write_text -> read_text roundtrip");
}

// ── 4. contenteditable + InputEvent ───────────────────────────────────────

#[test]
fn contenteditable_typing() {
    let mut doc = Document::new();
    let editor = doc.create_element("div");
    let editor_id = editor.id;  // NodeId is a public field
    let body_id = doc.body().id;
    doc.append_child(body_id, editor_id);
    doc.get_node_mut(editor_id).set_content_editable("true");

    assert!(doc.is_content_editable(editor_id));

    // Type "Hello"
    for ch in ["H", "e", "l", "l", "o"] {
        let handled = doc.handle_contenteditable_key(editor_id, ch, false, false);
        assert!(handled, "char {ch} should be handled");
    }

    let text = doc.get_node(editor_id).text_content.clone().unwrap_or_default();
    assert_eq!(text, "Hello");
    println!("[PASS] contenteditable: typed 5 chars -> text_content = '{text}'");
}

#[test]
fn contenteditable_backspace() {
    let mut doc = Document::new();
    let editor = doc.create_element("div");
    let editor_id = editor.id;
    let body_id = doc.body().id;
    doc.append_child(body_id, editor_id);
    doc.get_node_mut(editor_id).set_content_editable("true");

    doc.handle_contenteditable_key(editor_id, "H", false, false);
    doc.handle_contenteditable_key(editor_id, "i", false, false);
    doc.handle_contenteditable_key(editor_id, "Backspace", false, false);

    let text = doc.get_node(editor_id).text_content.clone().unwrap_or_default();
    assert_eq!(text, "H");
    println!("[PASS] contenteditable: Backspace removes last char -> '{text}'");
}

#[test]
fn input_event_input_type_fired() {
    let mut doc = Document::new();
    let editor = doc.create_element("div");
    let editor_id = editor.id;
    let body_id = doc.body().id;
    doc.append_child(body_id, editor_id);
    doc.get_node_mut(editor_id).set_content_editable("true");

    // Track InputEvent input_type values via event listener
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let received_clone = received.clone();
    // Use Element.add_event_listener which takes &mut Document
    let editor_elem = w3cos_dom::element::Element::new(editor_id);
    editor_elem.add_event_listener(&mut doc, "input", Box::new(move |ev| {
        if let EventData::Input { input_type, .. } = &ev.data {
            if let Some(it) = input_type {
                received_clone.lock().unwrap().push(it.as_str().to_string());
            }
        }
    }));

    doc.handle_contenteditable_key(editor_id, "A", false, false);
    doc.handle_contenteditable_key(editor_id, "Backspace", false, false);

    let events = received.lock().unwrap().clone();
    assert_eq!(events.len(), 2, "should have 2 InputEvents");
    assert_eq!(events[0], "insertText");
    assert_eq!(events[1], "deleteContentBackward");
    println!("[PASS] InputEvent.inputType fired: {:?}", events);
}

// ── 5. FileSystemObserver ──────────────────────────────────────────────────

#[test]
fn fs_file_handle_roundtrip() {
    let tmp = std::env::temp_dir().join("w3cos_int_fh.txt");
    let handle = FileSystemFileHandle::new(&tmp);
    handle.write_text("integration test").unwrap();
    let text = handle.get_text().unwrap();
    assert_eq!(text, "integration test");
    std::fs::remove_file(&tmp).ok();
    println!("[PASS] FileSystemFileHandle: write + read roundtrip");
}

#[test]
fn fs_directory_handle_entries() {
    let tmp = std::env::temp_dir().join("w3cos_int_dir");
    std::fs::create_dir_all(&tmp).ok();
    std::fs::write(tmp.join("main.rs"), "fn main() {}").ok();
    std::fs::write(tmp.join("lib.rs"), "pub fn hello() {}").ok();

    let dir = FileSystemDirectoryHandle::new(&tmp);
    let entries = dir.entries().unwrap();
    assert_eq!(entries.len(), 2);
    let names: Vec<String> = entries.iter().map(|e| e.name()).collect();
    assert!(names.contains(&"lib.rs".to_string()));
    assert!(names.contains(&"main.rs".to_string()));
    std::fs::remove_dir_all(&tmp).ok();
    println!("[PASS] FileSystemDirectoryHandle: entries() = {:?}", names);
}

#[test]
fn fs_observer_detects_created() {
    let tmp = std::env::temp_dir().join("w3cos_int_obs");
    std::fs::create_dir_all(&tmp).ok();

    let observer = FileSystemObserver::new();
    observer.observe(&tmp, ObserveOptions::default());
    thread::sleep(Duration::from_millis(150));

    std::fs::write(tmp.join("new.txt"), "hello").ok();
    thread::sleep(Duration::from_millis(250));

    let records = observer.poll_records();
    observer.disconnect();

    let created = records.iter().any(|r|
        r.change_type == ChangeType::Created &&
        r.path.file_name().map(|n| n == "new.txt").unwrap_or(false)
    );
    assert!(created, "expected Created event, got: {:?}",
        records.iter().map(|r| (&r.change_type, r.path.file_name())).collect::<Vec<_>>());
    std::fs::remove_dir_all(&tmp).ok();
    println!("[PASS] FileSystemObserver: detected Created event for new.txt");
}

// ── 6. FontFace / FontRegistry ─────────────────────────────────────────────

#[test]
fn font_registry_register_and_resolve() {
    // Use global registry with unique family names to avoid test interference
    FontRegistry::global().register(FontFace {
        family: "IntTestMono".into(),
        src: FontSource::Bytes(vec![0u8; 16]),
        weight: FontWeight::NORMAL,
        style: FontFaceStyle::Normal,
        ..Default::default()
    }).unwrap();

    FontRegistry::global().register(FontFace {
        family: "IntTestMono".into(),
        src: FontSource::Bytes(vec![0u8; 16]),
        weight: FontWeight::BOLD,
        style: FontFaceStyle::Normal,
        ..Default::default()
    }).unwrap();

    let normal = FontRegistry::global().resolve("IntTestMono", FontWeight::NORMAL, FontFaceStyle::Normal);
    assert!(normal.is_some());
    assert!(normal.unwrap().is_monospace, "IntTestMono should be detected as monospace");

    // Request weight 500 — should fall back to 400 (closer than 700)
    let medium = FontRegistry::global().resolve("IntTestMono", FontWeight(500), FontFaceStyle::Normal);
    assert!(medium.is_some());
    assert_eq!(medium.unwrap().weight, FontWeight::NORMAL);
    println!("[PASS] FontRegistry: register + resolve + closest-weight fallback");
}

#[test]
fn font_registry_family_stack() {
    FontRegistry::global().register(FontFace {
        family: "IntFallbackSans".into(),
        src: FontSource::Bytes(vec![]),
        weight: FontWeight::NORMAL,
        style: FontFaceStyle::Normal,
        ..Default::default()
    }).unwrap();

    let resolved = FontRegistry::global().resolve_stack(
        "Missing Font, IntFallbackSans, sans-serif",
        FontWeight::NORMAL,
        FontFaceStyle::Normal,
    );
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().family, "IntFallbackSans");
    println!("[PASS] FontRegistry: font-family stack resolution with fallback");
}

#[test]
fn font_face_css_parse() {
    let css = r#"
        font-family: IntCSSFont;
        src: local(Arial);
        font-weight: bold;
        font-style: italic;
        font-display: swap;
    "#;
    parse_and_register(css).expect("CSS @font-face parse failed");
    let resolved = FontRegistry::global().resolve(
        "IntCSSFont", FontWeight::BOLD, FontFaceStyle::Italic
    );
    assert!(resolved.is_some());
    println!("[PASS] @font-face CSS parse and register");
}

// ── 7. MutationObserver ────────────────────────────────────────────────────

#[test]
fn mutation_observer_callback_and_init() {
    use w3cos_runtime::observers::{MutationObserver, MutationObserverInit, MutationRecord, MutationType};
    use w3cos_dom::node::NodeId;

    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<MutationRecord>::new()));
    let received_clone = received.clone();

    let mut observer = MutationObserver::new(move |records| {
        received_clone.lock().unwrap().extend(records);
    });

    let target = NodeId::from_u32(1);
    observer.observe(target, MutationObserverInit {
        child_list: true,
        character_data: true,
        subtree: true,
        ..Default::default()
    });

    // Queue a child_list mutation
    observer.queue_mutation(MutationRecord::child_list(
        target,
        vec![NodeId::from_u32(2)],
        vec![],
    ));
    // Queue a character_data mutation
    observer.queue_mutation(MutationRecord::character_data(
        target,
        Some("old text".to_string()),
    ));
    // Attributes not observed — should be filtered out
    observer.queue_mutation(MutationRecord::attributes(target, "class", None));

    assert_eq!(observer.take_records().len(), 2, "attributes should be filtered");

    // Queue again and deliver via callback
    observer.queue_mutation(MutationRecord::child_list(target, vec![], vec![NodeId::from_u32(3)]));
    observer.deliver();

    let got = received.lock().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].mutation_type, MutationType::ChildList);
    println!("[PASS] MutationObserver: callback + MutationObserverInit filtering");
}

#[test]
fn mutation_observer_disconnect_clears_queue() {
    use w3cos_runtime::observers::{MutationObserver, MutationObserverInit, MutationRecord};
    use w3cos_dom::node::NodeId;

    let mut observer = MutationObserver::new(|_| {});
    let target = NodeId::from_u32(5);
    observer.observe(target, MutationObserverInit { child_list: true, ..Default::default() });
    observer.queue_mutation(MutationRecord::child_list(target, vec![], vec![]));
    observer.disconnect();
    assert_eq!(observer.take_records().len(), 0, "disconnect should clear queue");
    println!("[PASS] MutationObserver: disconnect clears queue and observations");
}

// ── 8. Element.getBoundingClientRect ──────────────────────────────────────

#[test]
fn get_bounding_client_rect_zero_before_layout() {
    use w3cos_dom::document::Document;

    let mut doc = Document::new();
    let el = doc.create_element("div");
    doc.body().append_child(&mut doc, el);

    let rect = el.get_bounding_client_rect(&doc);
    assert_eq!(rect.x, 0.0);
    assert_eq!(rect.y, 0.0);
    assert_eq!(rect.width, 0.0);
    assert_eq!(rect.height, 0.0);
    println!("[PASS] getBoundingClientRect: returns zero before layout");
}

#[test]
fn get_bounding_client_rect_after_set_layout() {
    use w3cos_dom::document::Document;
    use w3cos_dom::dom_rect::DOMRect;

    let mut doc = Document::new();
    let el = doc.create_element("div");
    doc.body().append_child(&mut doc, el);

    // Simulate layout engine writing back computed rects
    doc.set_layout_rect(el.id, DOMRect::new(10.0, 20.0, 300.0, 150.0));

    let rect = el.get_bounding_client_rect(&doc);
    assert_eq!(rect.x, 10.0);
    assert_eq!(rect.y, 20.0);
    assert_eq!(rect.width, 300.0);
    assert_eq!(rect.height, 150.0);
    assert_eq!(rect.right(), 310.0);
    assert_eq!(rect.bottom(), 170.0);
    println!("[PASS] getBoundingClientRect: ({}, {}, {}, {})", rect.x, rect.y, rect.width, rect.height);
}

// ── 9. beforeinput + selectionchange ──────────────────────────────────────

#[test]
fn beforeinput_event_fires_and_cancellable() {
    use w3cos_dom::document::Document;
    use w3cos_dom::events::{EventType, ListenerOptions};

    let mut doc = Document::new();
    let editor = doc.create_element("div");
    doc.body().append_child(&mut doc, editor);
    editor.set_attribute(&mut doc, "contenteditable", "true");

    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fired_clone = fired.clone();

    editor.add_event_listener(&mut doc, "beforeinput", Box::new(move |ev| {
        fired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        ev.prevent_default();
    }));

    let prevented = doc.dispatch_before_input(
        editor.id,
        Some("a".to_string()),
        Some(w3cos_dom::events::InputType::InsertText),
        vec![],
    );

    assert!(fired.load(std::sync::atomic::Ordering::SeqCst), "beforeinput handler not called");
    assert!(prevented, "preventDefault should have been recorded");
    println!("[PASS] beforeinput: fires on contenteditable, preventDefault works");
}

#[test]
fn selectionchange_fires_on_document() {
    use w3cos_dom::document::Document;
    use w3cos_dom::node::NodeId;
    use w3cos_dom::events::EventType;

    let mut doc = Document::new();

    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fired_clone = fired.clone();

    // selectionchange is dispatched on document root (NodeId::ROOT)
    doc.add_document_event_listener("selectionchange", Box::new(move |_ev| {
        fired_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    }));

    doc.dispatch_selection_change();

    assert!(fired.load(std::sync::atomic::Ordering::SeqCst), "selectionchange not fired");
    println!("[PASS] selectionchange: fires on document root");
}

// ── 10. document.fonts.ready (FontFaceSet) ────────────────────────────────

#[test]
fn font_face_set_ready_callback() {
    use w3cos_runtime::font_face::FontFaceSet;

    let set = FontFaceSet::new();
    assert!(!set.is_ready(), "should not be ready before mark_ready");

    let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let called_clone = called.clone();
    set.ready_then(move || {
        called_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    set.mark_ready();

    assert!(set.is_ready(), "should be ready after mark_ready");
    assert!(called.load(std::sync::atomic::Ordering::SeqCst), "ready callback not called");
    println!("[PASS] FontFaceSet.ready: callback fires on mark_ready");
}

#[test]
fn font_face_set_add_and_check() {
    use w3cos_runtime::font_face::{FontFaceSet, FontFace, FontSource, FontWeight, FontFaceStyle};

    let set = FontFaceSet::new();
    set.add(FontFace {
        family: "IntTestFaceSet".into(),
        src: FontSource::Bytes(vec![]),
        weight: FontWeight::NORMAL,
        style: FontFaceStyle::Normal,
        ..Default::default()
    }).unwrap();

    assert!(set.check("IntTestFaceSet", FontWeight::NORMAL, FontFaceStyle::Normal));
    assert!(!set.check("NonExistent", FontWeight::NORMAL, FontFaceStyle::Normal));
    println!("[PASS] FontFaceSet.add + check");
}

// ── 11. getComputedStyle ───────────────────────────────────────────────────

#[test]
fn get_computed_style_returns_inline_style() {
    use w3cos_dom::document::Document;

    let mut doc = Document::new();
    let el = doc.create_element("div");
    doc.body().append_child(&mut doc, el);

    // Set inline style
    el.style_mut(&mut doc).set_property("display", "flex");
    el.style_mut(&mut doc).set_property("width", "200px");

    let computed = el.get_computed_style(&doc);
    assert_eq!(
        computed.get_property("display"),
        "flex",
        "computed display should be flex"
    );
    assert_eq!(
        computed.get_property("width"),
        "200px",
        "computed width should be 200px"
    );
    println!("[PASS] getComputedStyle: returns inline style properties");
}

// ── 12. DOMRect ────────────────────────────────────────────────────────────

#[test]
fn dom_rect_geometry() {
    use w3cos_dom::dom_rect::DOMRect;

    let r = DOMRect::new(5.0, 10.0, 200.0, 100.0);
    assert_eq!(r.top(), 10.0);
    assert_eq!(r.left(), 5.0);
    assert_eq!(r.right(), 205.0);
    assert_eq!(r.bottom(), 110.0);
    assert!(r.contains_point(100.0, 50.0));
    assert!(!r.contains_point(0.0, 0.0));
    assert!(!r.is_empty());
    println!("[PASS] DOMRect: geometry helpers correct");
}
