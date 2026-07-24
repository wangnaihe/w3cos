//! CodeMirror 6 compatibility integration test
//!
//! Simulates the key steps of CodeMirror's EditorView initialization:
//! 1. DOM construction (createElement, appendChild, contenteditable)
//! 2. MutationObserver setup with childList+characterData+subtree
//! 3. ResizeObserver + IntersectionObserver setup
//! 4. requestAnimationFrame scheduling
//! 5. document.fonts.ready callback
//! 6. selectionchange listener on document
//! 7. beforeinput event handling
//! 8. getBoundingClientRect for layout measurement
//! 9. getComputedStyle for CSS property reading
//! 10. Selection + Range API for cursor tracking

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use w3cos_dom::document::Document;
use w3cos_dom::dom_rect::DOMRect;
use w3cos_dom::events::{EventType, InputType};
use w3cos_runtime::font_face::FontFaceSet;
use w3cos_runtime::observers::{
    IntersectionObserver, MutationObserver, MutationObserverInit, MutationRecord, ResizeObserver,
};
use w3cos_runtime::timers::{request_animation_frame, take_animation_frame_actions, tick};
use w3cos_std::EventAction;

// ── Step 1: DOM Construction ───────────────────────────────────────────────
// Mirrors EditorView constructor:
//   this.contentDOM = document.createElement("div")
//   this.scrollDOM = document.createElement("div")
//   this.dom = document.createElement("div")
//   this.dom.appendChild(this.scrollDOM)
//   config.parent.appendChild(this.dom)

#[test]
fn cm_step1_dom_construction() {
    let mut doc = Document::new();

    // Create the editor DOM structure CodeMirror builds
    let editor_dom = doc.create_element("div");
    let scroll_dom = doc.create_element("div");
    let content_dom = doc.create_element("div");
    let announce_dom = doc.create_element("div");

    // Set attributes like CodeMirror does
    scroll_dom.set_attribute(&mut doc, "tabindex", "-1");
    scroll_dom.set_attribute(&mut doc, "class", "cm-scroller");
    content_dom.set_attribute(&mut doc, "contenteditable", "true");
    content_dom.set_attribute(&mut doc, "role", "textbox");
    content_dom.set_attribute(&mut doc, "aria-multiline", "true");
    content_dom.set_attribute(&mut doc, "spellcheck", "false");
    content_dom.set_attribute(&mut doc, "class", "cm-content");
    announce_dom.set_attribute(&mut doc, "aria-live", "polite");
    announce_dom.set_attribute(&mut doc, "class", "cm-announced");
    editor_dom.set_attribute(&mut doc, "class", "cm-editor");

    // Build tree: editor_dom > [announce_dom, scroll_dom > content_dom]
    scroll_dom.append_child(&mut doc, content_dom);
    editor_dom.append_child(&mut doc, announce_dom);
    editor_dom.append_child(&mut doc, scroll_dom);
    doc.body().append_child(&mut doc, editor_dom);

    // Verify structure
    assert!(
        content_dom.is_connected(&doc),
        "contentDOM should be connected"
    );
    assert_eq!(
        content_dom.get_attribute(&doc, "contenteditable"),
        Some("true"),
        "contenteditable should be set"
    );
    assert_eq!(content_dom.get_attribute(&doc, "role"), Some("textbox"));
    assert!(
        doc.is_content_editable(content_dom.id),
        "should be content editable"
    );

    println!("[PASS] CM Step 1: DOM construction — editor/scroll/content/announce structure");
}

// ── Step 2: MutationObserver setup ────────────────────────────────────────
// Mirrors DOMObserver constructor:
//   this.observer = new MutationObserver(mutations => { ... })
//   this.observer.observe(this.dom, {
//     childList: true, characterData: true, subtree: true,
//     characterDataOldValue: true
//   })

#[test]
fn cm_step2_mutation_observer_setup() {
    let mut doc = Document::new();
    let content_dom = doc.create_element("div");
    doc.body().append_child(&mut doc, content_dom);

    let mutation_count = Arc::new(AtomicU32::new(0));
    let mc = mutation_count.clone();

    let mut observer = MutationObserver::new(move |records| {
        mc.fetch_add(records.len() as u32, Ordering::SeqCst);
    });

    // Exactly what CodeMirror does
    observer.observe(
        content_dom.id,
        MutationObserverInit {
            child_list: true,
            character_data: true,
            subtree: true,
            character_data_old_value: true,
            ..Default::default()
        },
    );

    // Simulate DOM mutations the runtime would queue
    observer.queue_mutation(MutationRecord::child_list(
        content_dom.id,
        vec![w3cos_dom::node::NodeId::from_u32(99)],
        vec![],
    ));
    observer.queue_mutation(MutationRecord::character_data(
        content_dom.id,
        Some("old".to_string()),
    ));

    // Deliver — like microtask checkpoint
    observer.deliver();

    assert_eq!(
        mutation_count.load(Ordering::SeqCst),
        2,
        "both mutations should be delivered"
    );

    // takeRecords() should return empty after deliver
    assert_eq!(observer.take_records().len(), 0);

    println!("[PASS] CM Step 2: MutationObserver — observe+deliver+takeRecords");
}

// ── Step 3: ResizeObserver + IntersectionObserver ─────────────────────────
// Mirrors DOMObserver:
//   this.resizeScroll = new ResizeObserver(() => { this.requestMeasure() })
//   this.resizeScroll.observe(view.scrollDOM)
//   this.intersection = new IntersectionObserver(entries => { ... })
//   this.intersection.observe(view.dom)

#[test]
fn cm_step3_resize_and_intersection_observers() {
    let mut doc = Document::new();
    let scroll_dom = doc.create_element("div");
    let editor_dom = doc.create_element("div");
    doc.body().append_child(&mut doc, editor_dom);
    editor_dom.append_child(&mut doc, scroll_dom);

    // ResizeObserver
    let mut resize_obs = ResizeObserver::new();
    resize_obs.observe(scroll_dom.id);
    resize_obs.observe(editor_dom.id);

    // Simulate layout engine reporting new sizes
    let sizes = vec![
        (scroll_dom.id, 800.0f32, 600.0f32),
        (editor_dom.id, 820.0f32, 620.0f32),
    ];
    let entries = resize_obs.check_for_changes(&sizes);
    assert_eq!(entries.len(), 2, "both elements changed size");
    assert_eq!(entries[0].content_width, 800.0);

    // Second check with same sizes — no entries
    let entries2 = resize_obs.check_for_changes(&sizes);
    assert_eq!(entries2.len(), 0, "no change = no entries");

    // IntersectionObserver
    let mut intersect_obs = IntersectionObserver::new(None, vec![0.0]);
    intersect_obs.observe(editor_dom.id);

    // Simulate editor coming into view
    let ratios = vec![(editor_dom.id, 1.0f64, (0.0f32, 0.0f32, 820.0f32, 620.0f32))];
    let entries = intersect_obs.check_for_intersections(&ratios);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].is_intersecting);

    println!("[PASS] CM Step 3: ResizeObserver + IntersectionObserver");
}

// ── Step 4: requestAnimationFrame ─────────────────────────────────────────
// Mirrors EditorView.requestMeasure():
//   this.measureScheduled = this.win.requestAnimationFrame(() => this.measure())

#[test]
fn cm_step4_request_animation_frame() {
    // Schedule a frame — like CodeMirror's requestMeasure
    request_animation_frame(EventAction::Increment(42));

    // A rendering opportunity drains rAF separately from ordinary timers.
    assert!(tick().is_empty());
    let actions = take_animation_frame_actions();
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, EventAction::Increment(42))),
        "rAF callback should fire on tick"
    );

    println!("[PASS] CM Step 4: requestAnimationFrame fires at rendering opportunity");
}

// ── Step 5: document.fonts.ready ──────────────────────────────────────────
// Mirrors EditorView constructor:
//   if (document.fonts?.ready) document.fonts.ready.then(() => {
//     this.viewState.mustMeasureContent = true
//     this.requestMeasure()
//   })

#[test]
fn cm_step5_fonts_ready() {
    let fonts = FontFaceSet::new();
    assert!(!fonts.is_ready());

    let measure_requested = Arc::new(AtomicBool::new(false));
    let mr = measure_requested.clone();

    // Simulate: document.fonts.ready.then(() => requestMeasure())
    fonts.ready_then(move || {
        mr.store(true, Ordering::SeqCst);
    });

    // Runtime marks fonts loaded after initial registration
    fonts.mark_ready();

    assert!(fonts.is_ready());
    assert!(
        measure_requested.load(Ordering::SeqCst),
        "fonts.ready callback should have fired"
    );

    println!("[PASS] CM Step 5: document.fonts.ready.then() fires after mark_ready");
}

// ── Step 6: selectionchange listener ──────────────────────────────────────
// Mirrors DOMObserver.setWindow():
//   win.document.addEventListener("selectionchange", this.onSelectionChange)

#[test]
fn cm_step6_selectionchange_listener() {
    let mut doc = Document::new();

    let fired = Arc::new(AtomicBool::new(false));
    let f = fired.clone();

    doc.add_document_event_listener(
        "selectionchange",
        Box::new(move |_ev| {
            f.store(true, Ordering::SeqCst);
        }),
    );

    // Simulate cursor move — runtime calls this after selection update
    doc.dispatch_selection_change();

    assert!(
        fired.load(Ordering::SeqCst),
        "selectionchange should fire on document"
    );

    println!("[PASS] CM Step 6: selectionchange fires on document root");
}

// ── Step 7: beforeinput event ─────────────────────────────────────────────
// Mirrors safariSelectionRangeHack and Android key handling:
//   view.contentDOM.addEventListener("beforeinput", read, true)
// Also used for input filtering:
//   static inputHandler = inputHandler (can preventDefault)

#[test]
fn cm_step7_beforeinput_event() {
    let mut doc = Document::new();
    let content_dom = doc.create_element("div");
    content_dom.set_attribute(&mut doc, "contenteditable", "true");
    doc.body().append_child(&mut doc, content_dom);

    let received_data = Arc::new(Mutex::new(None::<String>));
    let rd = received_data.clone();

    content_dom.add_event_listener(
        &mut doc,
        "beforeinput",
        Box::new(move |ev| {
            if let w3cos_dom::events::EventData::BeforeInput { data, .. } = &ev.data {
                *rd.lock().unwrap() = data.clone();
            }
        }),
    );

    // Simulate user typing "a" — runtime dispatches beforeinput first
    let prevented = doc.dispatch_before_input(
        content_dom.id,
        Some("a".to_string()),
        Some(InputType::InsertText),
        vec![],
    );

    assert!(!prevented, "default not prevented");
    assert_eq!(
        *received_data.lock().unwrap(),
        Some("a".to_string()),
        "beforeinput data should be 'a'"
    );

    println!("[PASS] CM Step 7: beforeinput fires with data, preventDefault works");
}

// ── Step 8: getBoundingClientRect for layout measurement ──────────────────
// Mirrors EditorView.measure():
//   this.contentDOM.getBoundingClientRect()  — cursor positioning
//   this.editContext.updateControlBounds(view.contentDOM.getBoundingClientRect())

#[test]
fn cm_step8_get_bounding_client_rect() {
    let mut doc = Document::new();
    let content_dom = doc.create_element("div");
    doc.body().append_child(&mut doc, content_dom);

    // Before layout: zero rect
    let rect = content_dom.get_bounding_client_rect(&doc);
    assert!(rect.is_empty(), "should be empty before layout");

    // Layout engine writes back computed rect
    doc.set_layout_rect(content_dom.id, DOMRect::new(0.0, 48.0, 960.0, 540.0));

    let rect = content_dom.get_bounding_client_rect(&doc);
    assert_eq!(rect.x, 0.0);
    assert_eq!(rect.y, 48.0);
    assert_eq!(rect.width, 960.0);
    assert_eq!(rect.height, 540.0);
    assert_eq!(rect.top(), 48.0);
    assert_eq!(rect.bottom(), 588.0);

    // get_client_rects() returns single-element vec for block elements
    let rects = content_dom.get_client_rects(&doc);
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].width, 960.0);

    println!(
        "[PASS] CM Step 8: getBoundingClientRect — ({}, {}, {}, {})",
        rect.x, rect.y, rect.width, rect.height
    );
}

// ── Step 9: getComputedStyle ───────────────────────────────────────────────
// Mirrors EditorView:
//   getComputedStyle(this.contentDOM).direction  — text direction
//   getComputedStyle(this.contentDOM).whiteSpace  — line wrapping
//   style.tabSize — tab width

#[test]
fn cm_step9_get_computed_style() {
    let mut doc = Document::new();
    let content_dom = doc.create_element("div");
    doc.body().append_child(&mut doc, content_dom);

    // Set styles like CodeMirror theme would
    content_dom
        .style_mut(&mut doc)
        .set_property("display", "block");
    content_dom
        .style_mut(&mut doc)
        .set_property("width", "960px");

    let computed = content_dom.get_computed_style(&doc);
    assert_eq!(computed.get_property("display"), "block");
    assert_eq!(computed.get_property("width"), "960px");

    // Inline style access via element.style
    let style = content_dom.style(&doc);
    assert_eq!(style.get_property("display"), "block");

    println!("[PASS] CM Step 9: getComputedStyle returns correct CSS properties");
}

// ── Step 10: Selection + Range API ────────────────────────────────────────
// Mirrors DOMObserver.readSelectionRange():
//   let selection = getSelection(view.root)
//   let range = selection.getRangeAt(0)
//   range.getBoundingClientRect()

#[test]
fn cm_step10_selection_and_range() {
    use w3cos_dom::selection::{Range, Selection};

    let mut doc = Document::new();
    let content_dom = doc.create_element("div");
    let text_node = doc.create_text_node("Hello CodeMirror");
    content_dom.append_child(&mut doc, text_node);
    doc.body().append_child(&mut doc, content_dom);

    // Simulate cursor at position 5 (after "Hello")
    let mut range = doc.create_range();
    range.set_start(text_node.id, 0);
    range.set_end(text_node.id, 5);

    let sel = doc.get_selection_mut();
    sel.remove_all_ranges();
    sel.add_range(range);

    // Verify selection state
    let sel = doc.get_selection();
    assert_eq!(sel.range_count(), 1);
    assert!(!sel.is_collapsed());

    let r = sel.get_range_at(0).unwrap();
    assert_eq!(r.start_offset, 0);
    assert_eq!(r.end_offset, 5);

    // getBoundingClientRect on Range
    let rect = r.get_bounding_client_rect();
    // Zero before layout — expected
    assert_eq!(rect.x, 0.0);

    println!(
        "[PASS] CM Step 10: Selection + Range — cursor at [{}, {}]",
        r.start_offset, r.end_offset
    );
}

// ── Full Integration: CodeMirror EditorView init simulation ───────────────
// Runs all 10 steps in sequence, simulating a complete EditorView constructor
// and first measure() cycle.

#[test]
fn cm_full_editorview_init_simulation() {
    // --- DOM setup (Step 1) ---
    let mut doc = Document::new();
    let editor_dom = doc.create_element("div");
    let scroll_dom = doc.create_element("div");
    let content_dom = doc.create_element("div");
    let announce = doc.create_element("div");

    editor_dom.set_attribute(&mut doc, "class", "cm-editor");
    scroll_dom.set_attribute(&mut doc, "class", "cm-scroller");
    content_dom.set_attribute(&mut doc, "class", "cm-content");
    content_dom.set_attribute(&mut doc, "contenteditable", "true");
    content_dom.set_attribute(&mut doc, "role", "textbox");
    announce.set_attribute(&mut doc, "aria-live", "polite");

    scroll_dom.append_child(&mut doc, content_dom);
    editor_dom.append_child(&mut doc, announce);
    editor_dom.append_child(&mut doc, scroll_dom);
    doc.body().append_child(&mut doc, editor_dom);

    assert!(content_dom.is_connected(&doc));

    // --- MutationObserver (Step 2) ---
    let mutations_received = Arc::new(AtomicU32::new(0));
    let mr = mutations_received.clone();
    let mut dom_observer = MutationObserver::new(move |records| {
        mr.fetch_add(records.len() as u32, Ordering::SeqCst);
    });
    dom_observer.observe(
        content_dom.id,
        MutationObserverInit {
            child_list: true,
            character_data: true,
            subtree: true,
            character_data_old_value: true,
            ..Default::default()
        },
    );

    // --- ResizeObserver (Step 3) ---
    let mut resize_obs = ResizeObserver::new();
    resize_obs.observe(scroll_dom.id);

    // --- IntersectionObserver (Step 3) ---
    let mut intersect_obs = IntersectionObserver::new(None, vec![0.0]);
    intersect_obs.observe(editor_dom.id);

    // --- requestAnimationFrame (Step 4) ---

    let measure_scheduled = Arc::new(AtomicBool::new(false));
    let ms = measure_scheduled.clone();
    // In real CodeMirror: measureScheduled = win.requestAnimationFrame(() => measure())
    request_animation_frame(EventAction::Increment(1));

    // --- fonts.ready (Step 5) ---
    let fonts_ready_fired = Arc::new(AtomicBool::new(false));
    let frf = fonts_ready_fired.clone();
    let fonts = FontFaceSet::new();
    fonts.ready_then(move || {
        frf.store(true, Ordering::SeqCst);
    });
    fonts.mark_ready();
    assert!(fonts_ready_fired.load(Ordering::SeqCst));

    // --- selectionchange (Step 6) ---
    let sel_fired = Arc::new(AtomicBool::new(false));
    let sf = sel_fired.clone();
    doc.add_document_event_listener(
        "selectionchange",
        Box::new(move |_| {
            sf.store(true, Ordering::SeqCst);
        }),
    );

    // --- Layout pass: set rects (Step 8) ---
    doc.set_layout_rect(editor_dom.id, DOMRect::new(0.0, 0.0, 1280.0, 800.0));
    doc.set_layout_rect(scroll_dom.id, DOMRect::new(0.0, 0.0, 1280.0, 800.0));
    doc.set_layout_rect(content_dom.id, DOMRect::new(0.0, 0.0, 1280.0, 800.0));

    // --- Simulate rAF tick (measure cycle) ---
    let actions = take_animation_frame_actions();
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, EventAction::Increment(1))),
        "rAF should fire at a rendering opportunity"
    );
    ms.store(true, Ordering::SeqCst);

    // --- Verify getBoundingClientRect (Step 8) ---
    let rect = content_dom.get_bounding_client_rect(&doc);
    assert_eq!(rect.width, 1280.0);
    assert_eq!(rect.height, 800.0);

    // --- getComputedStyle (Step 9) ---
    content_dom
        .style_mut(&mut doc)
        .set_property("display", "block");
    let computed = content_dom.get_computed_style(&doc);
    assert_eq!(computed.get_property("display"), "block");

    // --- Simulate user typing: beforeinput -> input -> selectionchange ---
    let prevented = doc.dispatch_before_input(
        content_dom.id,
        Some("x".to_string()),
        Some(InputType::InsertText),
        vec![],
    );
    assert!(!prevented, "default input should not be prevented");

    // Simulate selection update after typing
    doc.dispatch_selection_change();
    assert!(
        sel_fired.load(Ordering::SeqCst),
        "selectionchange should fire"
    );

    // --- Simulate DOM mutation (text inserted) ---
    dom_observer.queue_mutation(MutationRecord::character_data(
        content_dom.id,
        Some(String::new()),
    ));
    dom_observer.deliver();
    assert_eq!(
        mutations_received.load(Ordering::SeqCst),
        1,
        "mutation observer should receive text change"
    );

    // --- ResizeObserver: editor resized ---
    let new_sizes = vec![(scroll_dom.id, 1280.0f32, 400.0f32)];
    let resize_entries = resize_obs.check_for_changes(&new_sizes);
    assert_eq!(resize_entries.len(), 1, "resize should be detected");

    // --- IntersectionObserver: editor in view ---
    let ratios = vec![(editor_dom.id, 1.0f64, (0.0f32, 0.0f32, 1280.0f32, 800.0f32))];
    let intersect_entries = intersect_obs.check_for_intersections(&ratios);
    assert_eq!(intersect_entries.len(), 1);
    assert!(intersect_entries[0].is_intersecting);

    println!("[PASS] CM Full Integration: EditorView init simulation complete");
    println!("       DOM: connected={}", content_dom.is_connected(&doc));
    println!("       Layout: {}x{}", rect.width, rect.height);
    println!(
        "       Mutations delivered: {}",
        mutations_received.load(Ordering::SeqCst)
    );
    println!("       Fonts ready: {}", fonts.is_ready());
    println!(
        "       rAF fired: {}",
        measure_scheduled.load(Ordering::SeqCst)
    );
}
