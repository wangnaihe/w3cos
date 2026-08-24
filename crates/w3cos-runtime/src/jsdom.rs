//! Value-level DOM bridge ("jsdom").
//!
//! Exposes the real w3cos-dom [`Document`] (thread-local, see [`crate::dom`])
//! to compiled JavaScript as `w3cos_core::Value` objects:
//!
//! - [`element_value`] — a proxied `Value::Object` wrapping a DOM node.
//!   Property gets/sets are intercepted by Proxy traps and forwarded to the
//!   real DOM (attributes, style, classList, tree mutation, events, ...).
//!   Values are memoized per node so `parent.appendChild(x) === x` holds
//!   (`Value` equality on objects is `Rc::ptr_eq`).
//! - [`document_value`] / [`window_value`] — the Realm-owned global
//!   `document` / `window` values for compiled JS.
//! - [`drain_microtasks`] — runs queued microtasks AND delivers DOM events
//!   that were dispatched through the native w3cos-dom path (see below).
//! - [`tick_timers`] — fires due `setTimeout`/`setInterval` callbacks.
//! - [`run_animation_frame`] — runs one `requestAnimationFrame` batch at a
//!   rendering opportunity.
//!
//! # Event delivery model
//!
//! JS-originated `dispatchEvent` is fully synchronous: the bridge walks the
//! propagation path itself (capture → target → bubble) without holding the
//! document `RefCell` borrow, so JS handlers may freely mutate the DOM.
//!
//! Native-originated events (someone calls `doc.dispatch_event_bubbling`,
//! e.g. the window/input layer) CANNOT call JS handlers synchronously: the
//! dispatch holds a `&mut Document` borrow and any DOM access from the JS
//! handler would panic on the double borrow. Instead, the w3cos-dom listener
//! registered by this bridge only *snapshots* the event into a pending queue;
//! the snapshot is delivered to JS listeners by [`drain_microtasks`]. This
//! means `preventDefault`/`stopPropagation` from JS cannot affect native
//! dispatch (documented limitation; affects e.g. `beforeinput`).
//!
//! # Frame-loop integration
//!
//! Native task turns call `tick_timers()` followed by `drain_microtasks()`.
//! Immediately before painting, the window loop calls
//! `run_animation_frame()` followed by another microtask checkpoint. This
//! keeps rAF callbacks aligned with rendering opportunities rather than timer
//! wakeups. `drain_microtasks()` also polls native WebSocket workers and
//! dispatches their events.
//!
//! # Timer model
//!
//! JS timers are kept in a bridge-side store rather than
//! [`crate::timers`]: `timers::set_timeout` only accepts `EventAction`, which
//! has no JS-callback variant; framework adapters use the separate DOM host
//! boundary (`Notify` would fire desktop notifications via `state::execute_action`).

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "ios")]
use objc2::runtime::{AnyClass, AnyObject};
#[cfg(target_os = "ios")]
use std::ffi::CStr;

use w3cos_core::{JsObject, ProxyBuilder, Value, WeakJsObject};
use w3cos_dom::Element;
use w3cos_dom::events::{Event, EventData, EventType};
use w3cos_dom::node::NodeId;

use crate::dom;

const EVENT_COUNT_TYPES: &[&str] = &[
    "pointerdown",
    "touchend",
    "input",
    "keydown",
    "mouseleave",
    "mouseenter",
    "drop",
    "beforeinput",
    "pointerenter",
    "dragend",
    "pointercancel",
    "compositionupdate",
    "mousedown",
    "dragleave",
    "dragover",
    "mouseup",
    "pointerover",
    "lostpointercapture",
    "mouseover",
    "gotpointercapture",
    "dblclick",
    "keyup",
    "keypress",
    "pointerup",
    "compositionstart",
    "auxclick",
    "dragstart",
    "touchstart",
    "compositionend",
    "pointerout",
    "dragenter",
    "touchcancel",
    "click",
    "contextmenu",
    "mouseout",
    "pointerleave",
];

const POPOVER_OPEN_EXPANDO: &str = "__w3cos_popover_open";
const DIALOG_MODAL_EXPANDO: &str = "__w3cos_dialog_modal";
const DIALOG_RETURN_VALUE_EXPANDO: &str = "__w3cos_dialog_return_value";

fn initial_event_counts() -> HashMap<String, u64> {
    EVENT_COUNT_TYPES
        .iter()
        .map(|event_type| ((*event_type).to_string(), 0))
        .collect()
}

// ── Bridge state (all thread-local, matching the thread-local Document) ────

struct JsListener {
    node: u32,
    event_type: EventType,
    handler: Value,
    capture: bool,
    inline: bool,
}

struct JsTimer {
    id: u32,
    callback: Value,
    args: Vec<Value>,
    fire_at: Instant,
    interval: Option<Duration>,
}

#[derive(Clone)]
struct NativeTouch {
    identifier: i64,
    target: u32,
    client_x: f32,
    client_y: f32,
    force: f32,
}

#[derive(Clone)]
struct ShadowRootInfo {
    root: u32,
    mode: String,
}

/// JS wrapper intern: nodes still in a tree (document or fragment) stay
/// strong so `===` holds; parentless orphans are Weak so the cache does
/// not pin them after the last JS handle drops.
enum ElementMemo {
    Strong(Value),
    Weak(WeakJsObject),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CssMotionKind {
    Animation,
    Transition,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CssMotionValue {
    Length(f32),
    TranslateX(f32),
}

#[derive(Debug, Clone)]
struct CssMotion {
    node: u32,
    pseudo: Option<String>,
    property: String,
    kind: CssMotionKind,
    label: String,
    from: CssMotionValue,
    to: CssMotionValue,
    started_at: Instant,
    delay: Duration,
    duration: Duration,
    easing: w3cos_std::style::Easing,
    direction: w3cos_std::style::AnimationDirection,
    event_pending: bool,
}

#[derive(Debug, Clone)]
struct TransitionSnapshot {
    pseudo: Option<String>,
    property: String,
    value: CssMotionValue,
    transition: w3cos_std::style::Transition,
}

thread_local! {
    /// Monotonic identity for the active document Realm. Every node-backed JS
    /// facade captures this value so an externally retained facade cannot
    /// address a recycled node id after navigation.
    static BRIDGE_REALM_GENERATION: Cell<u32> = const { Cell::new(1) };
    /// node id → memoized element Value (identity: `a === b` via Rc::ptr_eq).
    static ELEMENT_VALUES: RefCell<HashMap<u32, ElementMemo>> = RefCell::new(HashMap::new());
    /// (node, key) → JS expando properties assigned through the set trap
    /// (plus bridge-cached "style"/"classList"/"__ctx2d" values).
    static ELEMENT_PROPS: RefCell<HashMap<(u32, String), Value>> = RefCell::new(HashMap::new());
    /// Processing-instruction attributes are projected from `data`, but values
    /// containing quotes must remain readable after standards serialization.
    static PROCESSING_INSTRUCTION_ATTRIBUTES: RefCell<HashMap<u32, (String, Vec<(String, String)>)>> = RefCell::new(HashMap::new());
    /// Native Attr identity is `(owner element, namespace, local name)`.
    /// Repeated access through `attributes`, `getAttributeNode`, and
    /// `getAttributeNodeNS` must return the same JS object until it is removed.
    static ATTRIBUTE_VALUES: RefCell<HashMap<(u32, Option<String>, String), Value>> = RefCell::new(HashMap::new());
    /// (node, kebab-prop) → raw CSS value cache. `CSSStyleDeclaration` drops
    /// properties it does not know, so reads of e.g. `lineHeight` would
    /// otherwise come back "".
    static STYLE_CACHE: RefCell<HashMap<(u32, String), String>> = RefCell::new(HashMap::new());
    /// Active CSS animations/transitions sampled by CSSOM and geometry reads.
    /// The corresponding JS facades live in `animations_web::ANIMATIONS`.
    static CSS_MOTIONS: RefCell<Vec<CssMotion>> = const { RefCell::new(Vec::new()) };
    /// JS event listener registry. Delivery for native events consults this
    /// at drain time; `dispatchEvent` consults it synchronously.
    static LISTENERS: RefCell<Vec<JsListener>> = RefCell::new(Vec::new());
    /// (node, event_type) pairs that already have a native snapshot closure
    /// registered inside w3cos-dom's EventRegistry.
    static NATIVELY_REGISTERED: RefCell<HashSet<(u32, EventType)>> = RefCell::new(HashSet::new());
    /// Event snapshots taken by native snapshot closures, awaiting delivery.
    static PENDING_EVENTS: RefCell<Vec<Event>> = RefCell::new(Vec::new());
    /// Active native touch contacts in activation order.
    static ACTIVE_TOUCHES: RefCell<Vec<NativeTouch>> = const { RefCell::new(Vec::new()) };
    /// Active native pointer ids and their browser-facing pointer type.
    static ACTIVE_POINTERS: RefCell<HashMap<i64, String>> = RefCell::new(HashMap::new());
    /// pointer id → element holding explicit pointer capture.
    static POINTER_CAPTURE: RefCell<HashMap<i64, u32>> = RefCell::new(HashMap::new());
    /// Custom event name → stable EventType (EventType::from_str mints a fresh
    /// Custom id per call for unknown names, so the bridge memoizes).
    static CUSTOM_EVENT_TYPES: RefCell<HashMap<String, EventType>> = RefCell::new(HashMap::new());
    /// Custom EventType id → name (for rebuilding the `type` string).
    static CUSTOM_EVENT_NAMES: RefCell<HashMap<u32, String>> = RefCell::new(HashMap::new());
    /// queueMicrotask queue.
    static MICROTASKS: RefCell<Vec<Value>> = RefCell::new(Vec::new());
    /// JS timers (setTimeout/setInterval).
    static JS_TIMERS: RefCell<Vec<JsTimer>> = RefCell::new(Vec::new());
    static NEXT_TIMER_ID: Cell<u32> = Cell::new(1);
    /// Stable implementation-specific ordering for roots in disconnected DOM
    /// trees. The DOM standard permits either direction, but requires the
    /// result to remain consistent when the operands are reversed.
    static NEXT_DISCONNECTED_NODE_ORDER: Cell<u64> = Cell::new(1);
    /// requestAnimationFrame callbacks: (id, callback).
    static RAF_QUEUE: RefCell<Vec<(u32, Value)>> = RefCell::new(Vec::new());
    static NEXT_RAF_ID: Cell<u32> = Cell::new(1);
    /// Viewport (width, height, devicePixelRatio) for window/matchMedia.
    static VIEWPORT: Cell<(f64, f64, f64)> = Cell::new((1024.0, 768.0, 1.0));
    /// Browser-shaped APIs whose host operation is unavailable. Each warning
    /// is emitted once per process thread so compatibility fallbacks are
    /// visible without flooding application logs.
    static HOST_API_WARNINGS: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
    /// Live MediaQueryList objects that must be reevaluated when viewport
    /// metrics change.
    static MEDIA_QUERY_LISTS: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static MEDIA_QUERY_LIST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static VISUAL_VIEWPORT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Host-reported simultaneous touch contacts. Desktop defaults to zero.
    static MAX_TOUCH_POINTS: Cell<u32> = const { Cell::new(0) };
    static WINDOW_SCROLL: Cell<(f64, f64)> = const { Cell::new((0.0, 0.0)) };
    static FULLSCREEN_NODE: RefCell<Option<u32>> = const { RefCell::new(None) };
    static DOCUMENT_VISIBILITY: RefCell<String> = RefCell::new("visible".to_string());
    static DOCUMENT_READY_STATE: RefCell<String> = RefCell::new("complete".to_string());
    static EVENT_COUNTS: RefCell<HashMap<String, u64>> = RefCell::new(initial_event_counts());
    /// Bridge-side focus tracking (no real input focus exists yet).
    static ACTIVE_ELEMENT: RefCell<Option<u32>> = RefCell::new(None);
    /// Lazily-created <html> / <head> elements.
    static HTML_ID: RefCell<Option<u32>> = RefCell::new(None);
    static HEAD_ID: RefCell<Option<u32>> = RefCell::new(None);
    /// Realm-owned globals. They are memoized only for the lifetime of one
    /// document Realm so authored expandos, event properties and child host
    /// objects cannot leak into the next navigation.
    static DOCUMENT_VALUE: RefCell<Option<Value>> = RefCell::new(None);
    static WINDOW_VALUE: RefCell<Option<Value>> = RefCell::new(None);
    static SELECTION_VALUE: RefCell<Option<Value>> = RefCell::new(None);
    static IDLE_DEADLINE_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DOM_PARSER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static XML_SERIALIZER_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CSS_STYLE_DECLARATION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CSS_STYLE_SHEET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static STYLE_SHEET_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static STYLE_SHEET_LIST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static AUTHOR_STYLE_SHEETS: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(Vec::new()));
    static MEDIA_LIST_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DOM_COLLECTION_CLASSES: RefCell<Option<HashMap<String, Value>>> =
        const { RefCell::new(None) };
    static SELECTOR_ID_CACHE_GENERATION: Cell<u64> = const { Cell::new(0) };
    static SELECTOR_ID_CACHE: RefCell<HashMap<u32, HashMap<String, Vec<u32>>>> =
        RefCell::new(HashMap::new());
    static LEGACY_ELEMENT_FACTORY_CLASSES: RefCell<Option<HashMap<String, Value>>> =
        const { RefCell::new(None) };
    static BAR_PROP_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CRYPTO_KEY_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static DOM_ERROR_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static CARET_POSITION_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static MATH_ML_ELEMENT_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    static WINDOW_CLASS: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Canvas 2D contexts per canvas node.
    static CANVAS_CONTEXTS: RefCell<HashMap<u32, Rc<RefCell<crate::canvas2d::CanvasRenderingContext2D>>>> =
        RefCell::new(HashMap::new());
    /// In-memory sessionStorage.
    static SESSION_STORAGE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Clipboard fallback for non-desktop targets.
    static CLIPBOARD_FALLBACK: RefCell<String> = RefCell::new(String::new());
    static LOCATION_RELOAD_WARNED: Cell<bool> = const { Cell::new(false) };
    static DRAW_IMAGE_SOURCE_WARNED: Cell<bool> = const { Cell::new(false) };
    static CANVAS_PAINT_STYLE_WARNED: Cell<bool> = const { Cell::new(false) };
    static SMOOTH_SCROLL_WARNED: Cell<bool> = const { Cell::new(false) };
    static XML_PARSER_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
    /// Shadow host node → detached DocumentFragment used as its shadow tree.
    static SHADOW_ROOTS: RefCell<HashMap<u32, ShadowRootInfo>> = RefCell::new(HashMap::new());
    static SHADOW_DOM_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
    static EVAL_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
    static FUNCTION_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
    static RANGE_COMPLEX_WARNING_EMITTED: Cell<bool> = const { Cell::new(false) };
    /// Mutable Range objects participate in DOM's live-range maintenance
    /// algorithms while nodes are removed and inserted.
    static LIVE_RANGES: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    /// performance.now() origin.
    static START_TIME: Instant = Instant::now();
    static TIME_ORIGIN: f64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64() * 1000.0;
}

#[cfg(target_os = "ios")]
thread_local! {
    static PENDING_FILE_INPUT: RefCell<Option<u32>> = const { RefCell::new(None) };
}

#[cfg(target_os = "ios")]
static IOS_FILE_PICKER_CALLBACK: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[cfg(target_os = "ios")]
static IOS_FILE_PICKER_REQUESTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(target_os = "ios")]
static IOS_FILE_PICKER_PATHS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(target_os = "ios")]
static IOS_FILE_PICKER_FILES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(target_os = "ios")]
static IOS_FILE_PICKER_CHANGE_LISTENERS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_set_file_picker_callback(callback: extern "C" fn(u8)) {
    IOS_FILE_PICKER_CALLBACK.store(
        callback as *const () as *mut (),
        std::sync::atomic::Ordering::Release,
    );
}

#[cfg(target_os = "ios")]
pub(crate) fn file_picker_diagnostics() -> (bool, bool, u64, u64, u64, u64) {
    let registered = !IOS_FILE_PICKER_CALLBACK
        .load(std::sync::atomic::Ordering::Acquire)
        .is_null();
    let pending = PENDING_FILE_INPUT.with(|value| value.borrow().is_some());
    let requests = IOS_FILE_PICKER_REQUESTS.load(std::sync::atomic::Ordering::Acquire);
    let paths = IOS_FILE_PICKER_PATHS.load(std::sync::atomic::Ordering::Acquire);
    let files = IOS_FILE_PICKER_FILES.load(std::sync::atomic::Ordering::Acquire);
    let change_listeners =
        IOS_FILE_PICKER_CHANGE_LISTENERS.load(std::sync::atomic::Ordering::Acquire);
    (
        registered,
        pending,
        requests,
        paths,
        files,
        change_listeners,
    )
}

#[cfg(not(target_os = "ios"))]
pub(crate) fn file_picker_diagnostics() -> (bool, bool, u64, u64, u64, u64) {
    (false, false, 0, 0, 0, 0)
}

#[cfg(target_os = "ios")]
fn request_ios_file_picker(node: u32) {
    IOS_FILE_PICKER_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    PENDING_FILE_INPUT.with(|pending| *pending.borrow_mut() = Some(node));
    let allows_multiple = dom::has_attribute(node, "multiple");
    let callback = IOS_FILE_PICKER_CALLBACK.load(std::sync::atomic::Ordering::Acquire);
    if callback.is_null() {
        if !crate::ios_input::present_document_picker(allows_multiple) {
            PENDING_FILE_INPUT.with(|pending| pending.borrow_mut().take());
        }
        return;
    }
    let callback: extern "C" fn(u8) = unsafe { std::mem::transmute(callback) };
    callback(u8::from(allows_multiple));
}

#[cfg(target_os = "ios")]
fn mime_type_for_path(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn w3cos_complete_file_picker(paths_json: *const std::ffi::c_char) {
    if paths_json.is_null() {
        PENDING_FILE_INPUT.with(|pending| pending.borrow_mut().take());
        return;
    }
    let json = unsafe { std::ffi::CStr::from_ptr(paths_json) }.to_string_lossy();
    let paths: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
    IOS_FILE_PICKER_PATHS.store(paths.len() as u64, std::sync::atomic::Ordering::Release);
    let Some(node) = PENDING_FILE_INPUT.with(|pending| pending.borrow_mut().take()) else {
        return;
    };
    let files = paths
        .into_iter()
        .filter_map(|path| {
            let path = std::path::PathBuf::from(path);
            let bytes = std::fs::read(&path).ok()?;
            let name = path.file_name()?.to_string_lossy().into_owned();
            let options = Value::object(HashMap::from([(
                "type".to_string(),
                Value::string(mime_type_for_path(&path)),
            )]));
            Some(w3cos_core::class::construct(
                &crate::files::file_class(),
                vec![
                    Value::array(vec![w3cos_core::binary::array_buffer_value(bytes)]),
                    Value::string(&name),
                    options,
                ],
            ))
        })
        .collect::<Vec<_>>();
    IOS_FILE_PICKER_FILES.store(files.len() as u64, std::sync::atomic::Ordering::Release);
    if files.is_empty() {
        return;
    }
    set_expando(
        node,
        "files",
        crate::clipboard_web::file_list_from_files(files),
    );
    let change = event_type_for("change");
    let listener_count = LISTENERS.with(|listeners| {
        let chain = shadow_event_chain(node, true);
        listeners
            .borrow()
            .iter()
            .filter(|listener| chain.contains(&listener.node) && listener.event_type == change)
            .count()
    });
    IOS_FILE_PICKER_CHANGE_LISTENERS
        .store(listener_count as u64, std::sync::atomic::Ordering::Release);
    dispatch_sync(node, change, EventData::None);
    crate::uitest::request_platform_repaint();
}

// ── Small helpers ──────────────────────────────────────────────────────────

fn func(f: impl Fn(Value, Vec<Value>) -> Value + 'static) -> Value {
    let generation = realm_generation();
    realm_function(generation, f)
}

pub(crate) fn realm_function(
    generation: u32,
    f: impl Fn(Value, Vec<Value>) -> Value + 'static,
) -> Value {
    let function = Value::function(move |this, args| {
        if bridge_realm_is_current(generation) {
            f(this, args)
        } else {
            Value::Undefined
        }
    });
    function.set_property("length", Value::Number(0.0));
    function
}

pub(crate) fn associate_callback_global(callback: &Value, global: &Value) {
    if callback.is_callable() {
        callback.set_property("__w3cos_callback_global", global.clone());
    }
}

pub(crate) fn report_callback_exception(callback: &Value, exception: Value) {
    let global = callback.get_property("__w3cos_callback_global");
    let global = if global.is_object() {
        global
    } else {
        window_value()
    };
    let handler = global.get_property("onerror");
    if !handler.is_callable() {
        return;
    }
    let message = exception.get_property("message");
    let message = if message.is_undefined() {
        Value::string(&exception.to_js_string())
    } else {
        message
    };
    let _ = w3cos_core::catch_js(|| {
        handler.call(
            global,
            vec![
                message,
                Value::string(""),
                Value::Number(0.0),
                Value::Number(0.0),
                exception,
            ],
        )
    });
}

pub(crate) type WeakRealmObject = WeakJsObject;

pub(crate) fn weak_realm_object(value: &Value) -> WeakRealmObject {
    value
        .as_object()
        .map(|object| object.downgrade())
        .expect("Realm host objects must use object storage")
}

pub(crate) fn upgrade_realm_object(object: &WeakRealmObject) -> Option<Value> {
    object.upgrade_value()
}

pub(crate) fn register_weak_realm_object(
    registry: &'static std::thread::LocalKey<RefCell<Vec<WeakRealmObject>>>,
    value: &Value,
) {
    registry.with(|objects| {
        let mut objects = objects.borrow_mut();
        objects.retain(|object| object.strong_count() != 0);
        objects.push(weak_realm_object(value));
    });
}

pub(crate) fn reset_realm_class(slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>) {
    slot.with(|slot| {
        if let Some(class) = slot.borrow_mut().take() {
            disconnect_realm_class(class);
        }
    });
}

pub(crate) fn disconnect_realm_class(class: Value) {
    let prototype = class.get_property("prototype");
    if prototype.is_object() {
        prototype.set_property("constructor", Value::Undefined);
    }
    class.set_property("prototype", Value::Undefined);
}

pub(crate) fn realm_generation() -> u32 {
    BRIDGE_REALM_GENERATION.with(Cell::get)
}

fn bridge_realm_is_current(generation: u32) -> bool {
    BRIDGE_REALM_GENERATION.with(|current| current.get() == generation)
}

fn advance_bridge_realm_generation() {
    BRIDGE_REALM_GENERATION.with(|current| {
        let next = current.get().wrapping_add(1);
        current.set(if next == 0 { 1 } else { next });
    });
}

fn reset_realm_class_caches() {
    MEDIA_QUERY_LIST_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    VISUAL_VIEWPORT_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    IDLE_DEADLINE_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    DOM_PARSER_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    XML_SERIALIZER_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    CSS_STYLE_DECLARATION_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    CSS_STYLE_SHEET_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    STYLE_SHEET_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    STYLE_SHEET_LIST_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    MEDIA_LIST_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    DOM_COLLECTION_CLASSES.with(|slot| {
        slot.borrow_mut().take();
    });
    LEGACY_ELEMENT_FACTORY_CLASSES.with(|slot| {
        slot.borrow_mut().take();
    });
    BAR_PROP_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    CRYPTO_KEY_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    DOM_ERROR_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    CARET_POSITION_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    MATH_ML_ELEMENT_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    WINDOW_CLASS.with(|slot| {
        slot.borrow_mut().take();
    });
    crate::dom_constructors::reset_realm();
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Undefined)
}

fn warn_host_api(name: &'static str, fallback: &'static str) {
    HOST_API_WARNINGS.with(|warned| {
        if warned.borrow_mut().insert(name) {
            eprintln!(
                "[w3cos] warning: {name} is unavailable without a host adapter; returning {fallback}"
            );
        }
    });
}

fn get_expando(node: u32, key: &str) -> Option<Value> {
    ELEMENT_PROPS.with(|p| p.borrow().get(&(node, key.to_string())).cloned())
}

fn set_expando(node: u32, key: &str, value: Value) {
    ELEMENT_PROPS.with(|p| p.borrow_mut().insert((node, key.to_string()), value));
}

fn shadow_host_for_root(root: u32) -> Option<u32> {
    SHADOW_ROOTS.with(|roots| {
        roots
            .borrow()
            .iter()
            .find_map(|(&host, info)| (info.root == root).then_some(host))
    })
}

pub(crate) fn shadow_root_id_for_host(host: u32) -> Option<u32> {
    SHADOW_ROOTS.with(|roots| roots.borrow().get(&host).map(|info| info.root))
}

fn tree_root(mut node: u32) -> u32 {
    while let Some(parent) = dom::parent_node(node) {
        node = parent;
    }
    node
}

fn shadow_including_tree_root(mut node: u32) -> u32 {
    loop {
        let root = tree_root(node);
        let Some(host) = shadow_host_for_root(root) else {
            return root;
        };
        node = host;
    }
}

pub(crate) fn node_is_connected(node: u32) -> bool {
    if dom::is_connected(node) {
        return true;
    }
    let root = tree_root(node);
    if shadow_host_for_root(root).is_some_and(node_is_connected) {
        return true;
    }
    get_expando(root, "parentNode")
        .is_some_and(|parent| parent.get_property("nodeType").to_u32() == 9)
}

fn nodes_have_same_shadow_including_root(left: u32, right: u32) -> bool {
    let left_connected = node_is_connected(left);
    let right_connected = node_is_connected(right);
    if left_connected || right_connected {
        return left_connected
            && right_connected
            && get_expando(left, "ownerDocument")
                .unwrap_or_else(document_value)
                .strict_eq(
                    &get_expando(right, "ownerDocument").unwrap_or_else(document_value),
                );
    }
    shadow_including_tree_root(left) == shadow_including_tree_root(right)
}

fn root_node_value(node: u32, composed: bool) -> Value {
    let root = tree_root(node);
    if let Some(host) = shadow_host_for_root(root) {
        if composed {
            return root_node_value(host, true);
        }
        return element_value(root);
    }
    if dom::is_connected(node) {
        document_value()
    } else {
        element_value(root)
    }
}

fn shadow_event_chain(target: u32, composed: bool) -> Vec<u32> {
    let mut chain = vec![target];
    let mut current = target;
    loop {
        if let Some(parent) = dom::parent_node(current) {
            chain.push(parent);
            current = parent;
            continue;
        }
        let Some(host) = shadow_host_for_root(current).filter(|_| composed) else {
            break;
        };
        chain.push(host);
        current = host;
    }
    chain
}

fn retarget_shadow_event(mut target: u32, current_target: u32) -> u32 {
    loop {
        let root = tree_root(target);
        let Some(host) = shadow_host_for_root(root) else {
            return target;
        };
        if current_target == root || is_ancestor_of(root, current_target) {
            return target;
        }
        target = host;
    }
}

fn shadow_root_value(host: u32, options: Value) -> Value {
    if SHADOW_ROOTS.with(|roots| roots.borrow().contains_key(&host)) {
        w3cos_core::throw_value(Value::object(HashMap::from([
            ("name".to_string(), Value::string("NotSupportedError")),
            (
                "message".to_string(),
                Value::string("Shadow root cannot be created on a host which already hosts one"),
            ),
        ])));
    }
    let mode = options.get_property("mode").to_js_string();
    if mode != "open" && mode != "closed" {
        w3cos_core::throw_value(Value::object(HashMap::from([
            ("name".to_string(), Value::string("TypeError")),
            (
                "message".to_string(),
                Value::string("ShadowRootInit.mode must be \"open\" or \"closed\""),
            ),
        ])));
    }

    let root = dom::create_document_fragment();
    let delegates_focus = options.get_property("delegatesFocus").to_bool();
    let slot_assignment = match options
        .get_property("slotAssignment")
        .to_js_string()
        .as_str()
    {
        "manual" => "manual",
        _ => "named",
    };
    set_expando(root, "host", element_value(host));
    set_expando(root, "mode", Value::string(&mode));
    set_expando(root, "delegatesFocus", Value::Bool(delegates_focus));
    set_expando(root, "slotAssignment", Value::string(slot_assignment));
    set_expando(root, "activeElement", Value::Null);
    set_expando(root, "pictureInPictureElement", Value::Null);
    SHADOW_ROOTS.with(|roots| {
        roots
            .borrow_mut()
            .insert(host, ShadowRootInfo { root, mode });
    });

    SHADOW_DOM_WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[W3C OS][compat warning] ShadowRoot tree/query/event semantics are available; \
                 slot distribution and composed rendering are not yet implemented"
            );
        }
    });
    let value = element_value(root);
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("ShadowRoot"));
    value
}

pub(crate) fn activate_declarative_shadow_root(host: u32, template: u32, mode: &str) -> bool {
    if SHADOW_ROOTS.with(|roots| roots.borrow().contains_key(&host)) {
        return false;
    }
    let options = Value::object(HashMap::from([("mode".to_string(), Value::string(mode))]));
    let shadow_root = shadow_root_value(host, options);
    let Some(root) = node_id_of(&shadow_root) else {
        return false;
    };
    set_expando(template, "content", element_value(root));
    true
}

/// Extract the DOM node id carried by an element Value (`__node_id` hidden
/// prop, read directly so the proxy trap is bypassed).
pub fn node_id_of(value: &Value) -> Option<u32> {
    if let Value::Object(obj) = value {
        let object = obj.borrow();
        let direct = object.get_direct("__node_id");
        let generation = object.get_direct("__w3cos_realm_generation");
        if direct.is_number()
            && generation.is_number()
            && bridge_realm_is_current(generation.to_u32())
        {
            return Some(direct.to_u32());
        }
    }
    None
}

pub(crate) fn performance_now() -> f64 {
    START_TIME.with(|t| t.elapsed().as_secs_f64() * 1000.0)
}

fn performance_time_origin() -> f64 {
    TIME_ORIGIN.with(|origin| *origin)
}

fn js_array(items: Vec<Value>) -> Value {
    Value::array(items)
}

type CollectionProvider = Rc<dyn Fn() -> Vec<Value>>;

fn build_dom_collection_classes() -> HashMap<String, Value> {
    let mut classes = HashMap::new();
    for name in ["NodeList", "HTMLCollection"] {
        let api = name.to_string();
        let class = Value::function(move |_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".to_string(), Value::string("TypeError")),
                (
                    "message".to_string(),
                    Value::string(&format!("Illegal constructor: {api}")),
                ),
            ])))
        });
        class.set_property("name", Value::string(name));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property(
            "item",
            func(|this, args| {
                let value = this.get_property(&arg(&args, 0).to_u32().to_string());
                if value.is_undefined() {
                    Value::Null
                } else {
                    value
                }
            }),
        );
        if name == "HTMLCollection" {
            prototype.set_property(
                "namedItem",
                func(|this, args| {
                    let lookup = this.get_property("__w3cosCollectionNamedItem");
                    lookup.call(this, vec![arg(&args, 0)])
                }),
            );
        } else {
            let array_prototype = w3cos_core::array_value().get_property("prototype");
            for method in [
                "entries",
                "forEach",
                "keys",
                "values",
                "__w3cos_symbol_iterator",
            ] {
                prototype.set_property(method, array_prototype.get_property(method));
            }
        }
        prototype.set_property("length", Value::Undefined);
        class.set_property("prototype", prototype);
        classes.insert(name.to_string(), class);
    }
    classes
}

fn dom_collection_class(name: &str) -> Value {
    DOM_COLLECTION_CLASSES.with(|slot| {
        if slot.borrow().is_none() {
            *slot.borrow_mut() = Some(build_dom_collection_classes());
        }
        slot.borrow()
            .as_ref()
            .and_then(|classes| classes.get(name))
            .cloned()
            .unwrap_or(Value::Undefined)
    })
}

fn html_collection_supported_names(items: &[Value]) -> Vec<(String, Value)> {
    let mut names = Vec::new();
    for item in items {
        let id = item.call_method("getAttribute", vec![Value::string("id")]);
        let id = if id.is_null() {
            String::new()
        } else {
            id.to_js_string()
        };
        if !id.is_empty() && !names.iter().any(|(name, _)| name == &id) {
            names.push((id, item.clone()));
        }
        if item.get_property("namespaceURI").to_js_string()
            == crate::html_parser_state::HTML_NAMESPACE
        {
            let name = item.call_method("getAttribute", vec![Value::string("name")]);
            let name = if name.is_null() {
                String::new()
            } else {
                name.to_js_string()
            };
            if !name.is_empty() && !names.iter().any(|(known, _)| known == &name) {
                names.push((name, item.clone()));
            }
        }
    }
    names
}

fn collection_value(
    provider: CollectionProvider,
    static_items: Option<Rc<Vec<Value>>>,
    class_name: &'static str,
) -> Value {
    let generation = realm_generation();
    let snapshot_provider = provider.clone();
    let index_provider = provider.clone();
    let index_static_items = static_items.clone();
    let named_item_provider = provider.clone();
    let target = HashMap::from([
        ("__w3cosRejectIndexedSet".to_string(), Value::Bool(true)),
        (
            "__w3cosMapValuesSnapshot".to_string(),
            func(move |_, _| js_array(snapshot_provider())),
        ),
        (
            "__w3cosArrayIndexOf".to_string(),
            func(move |_, args| {
                let needle = arg(&args, 0);
                let length = arg(&args, 1).to_u32() as usize;
                let start = arg(&args, 2).to_u32() as usize;
                let index_in = |items: &[Value]| {
                    let end = length.min(items.len());
                    items[start.min(end)..end]
                        .iter()
                        .position(|value| value.strict_eq(&needle))
                        .map_or(Value::Number(-1.0), |offset| {
                            Value::Number((start + offset) as f64)
                        })
                };
                if let Some(items) = &index_static_items {
                    index_in(items)
                } else {
                    index_in(&index_provider())
                }
            }),
        ),
        (
            "__w3cosCollectionNamedItem".to_string(),
            func(move |_, args| {
                let name = arg(&args, 0).to_js_string();
                html_collection_supported_names(&named_item_provider())
                    .into_iter()
                    .find_map(|(candidate, value)| (candidate == name).then_some(value))
                    .unwrap_or(Value::Null)
            }),
        ),
    ]);
    let get_provider = provider.clone();
    let get_static_items = static_items.clone();
    let descriptor_provider = provider.clone();
    let descriptor_static_items = static_items.clone();
    let has_provider = provider.clone();
    let has_static_items = static_items.clone();
    let keys_provider = provider;
    let keys_static_items = static_items;
    let handler = ProxyBuilder::new()
        .get(move |target, key, _| {
            if !bridge_realm_is_current(generation) {
                return Value::Undefined;
            }
            if let Ok(index) = key.parse::<usize>() {
                return if let Some(items) = get_static_items.as_ref() {
                    items.get(index).cloned().unwrap_or(Value::Undefined)
                } else {
                    get_provider()
                        .get(index)
                        .cloned()
                        .unwrap_or(Value::Undefined)
                };
            }
            let has_own_property = target
                .as_object()
                .is_some_and(|object| object.borrow().has_own_property(key));
            if has_own_property {
                return target.get_property(key);
            }
            if class_name == "HTMLCollection"
                && let Some((_, value)) = html_collection_supported_names(&get_provider())
                    .into_iter()
                    .find(|(name, _)| name == key)
            {
                return value;
            }
            let inherited = target.get_property(key);
            if !inherited.is_undefined() {
                return inherited;
            }
            match key {
                "length" => Value::Number(
                    get_static_items
                        .as_ref()
                        .map_or_else(|| get_provider().len(), |items| items.len())
                        as f64,
                ),
                _ => target.get_property(key),
            }
        })
        .get_own_property_descriptor(move |target, key| {
            let value = key.parse::<usize>().ok().and_then(|index| {
                if let Some(items) = descriptor_static_items.as_ref() {
                    items.get(index).cloned()
                } else {
                    descriptor_provider().get(index).cloned()
                }
            });
            value.map_or_else(
                || {
                    if class_name == "HTMLCollection"
                        && let Some((_, value)) =
                            html_collection_supported_names(&descriptor_provider())
                                .into_iter()
                                .find(|(name, _)| name == key)
                    {
                        return Value::object(HashMap::from([
                            ("value".to_string(), value),
                            ("writable".to_string(), Value::Bool(false)),
                            ("enumerable".to_string(), Value::Bool(false)),
                            ("configurable".to_string(), Value::Bool(true)),
                        ]));
                    }
                    if key.starts_with("__w3cos") {
                        return Value::Undefined;
                    }
                    target
                        .as_object()
                        .map(|object| object.borrow().get_own_property_descriptor(key))
                        .unwrap_or(Value::Undefined)
                },
                |value| {
                    Value::object(HashMap::from([
                        ("value".to_string(), value),
                        ("writable".to_string(), Value::Bool(false)),
                        ("enumerable".to_string(), Value::Bool(true)),
                        ("configurable".to_string(), Value::Bool(true)),
                    ]))
                },
            )
        })
        .has(move |target, key| {
            key == "length"
                || matches!(key, "item" | "entries" | "forEach" | "keys" | "values")
                || (class_name == "HTMLCollection" && key == "namedItem")
                || key
                    .parse::<usize>()
                    .ok()
                    .is_some_and(|index| {
                        index
                            < has_static_items
                                .as_ref()
                                .map_or_else(|| has_provider().len(), |items| items.len())
                    })
                || (class_name == "HTMLCollection"
                    && html_collection_supported_names(&has_provider())
                        .iter()
                        .any(|(name, _)| name == key))
                || target
                    .as_object()
                    .is_some_and(|object| object.borrow().has_direct(key))
        })
        .own_keys(move |target| {
            let items = keys_static_items
                .as_ref()
                .map_or_else(|| keys_provider(), |items| items.as_ref().clone());
            let mut keys = (0..items.len())
                .map(|index| index.to_string())
                .collect::<Vec<_>>();
            if class_name == "HTMLCollection" {
                keys.extend(
                    html_collection_supported_names(&items)
                        .into_iter()
                        .map(|(name, _)| name),
                );
            }
            if let Some(target) = target.as_object() {
                for key in target.borrow().keys() {
                    if !key.starts_with("__w3cos") && !keys.contains(&key) {
                        keys.push(key);
                    }
                }
            }
            js_array(keys.into_iter().map(Value::from).collect())
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(target, handler))));
    w3cos_core::class::set_prototype_of(
        &value,
        &dom_collection_class(class_name).get_property("prototype"),
    );
    value
}

pub(crate) fn node_list(items: Vec<Value>) -> Value {
    let items = Rc::new(items);
    collection_value(
        Rc::new({
            let items = items.clone();
            move || items.as_ref().clone()
        }),
        Some(items),
        "NodeList",
    )
}

fn live_node_list(provider: impl Fn() -> Vec<Value> + 'static) -> Value {
    collection_value(Rc::new(provider), None, "NodeList")
}

fn html_collection(provider: impl Fn() -> Vec<Value> + 'static) -> Value {
    collection_value(Rc::new(provider), None, "HTMLCollection")
}

fn element_or_null(node: Option<u32>) -> Value {
    match node {
        Some(id) => element_value(id),
        None => Value::Null,
    }
}

fn child_nodes_value(node: u32) -> Value {
    if let Some(list) = get_expando(node, "childNodes") {
        return list;
    }
    let list = live_node_list(move || dom::children(node).into_iter().map(element_value).collect());
    set_expando(node, "childNodes", list.clone());
    list
}

fn normalize_node_subtree(parent: u32) {
    let children = dom::children(parent);
    let mut index = 0;
    while index < children.len() {
        let child = children[index];
        if dom::node_type(child) != 3 {
            normalize_node_subtree(child);
            index += 1;
            continue;
        }

        let start = index;
        while index < children.len() && dom::node_type(children[index]) == 3 {
            index += 1;
        }
        let run = &children[start..index];
        let survivor = run
            .iter()
            .copied()
            .find(|node| !dom::get_text_content(*node).unwrap_or_default().is_empty());
        if let Some(survivor) = survivor {
            let data = run
                .iter()
                .filter_map(|node| dom::get_text_content(*node))
                .collect::<String>();
            dom::set_text_content(survivor, &data);
            for node in run.iter().copied().filter(|node| *node != survivor) {
                dom::remove_child(parent, node);
            }
        } else {
            for node in run {
                dom::remove_child(parent, *node);
            }
        }
    }
}

fn insert_adjacent_node(target: u32, position: &str, child: u32) -> bool {
    let position = position.to_ascii_lowercase();
    let parent_or_document_error = || {
        if get_expando(target, "parentNode")
            .is_some_and(|parent| parent.get_property("nodeType").to_u32() == 9)
        {
            dom_exception(
                "The document cannot contain another element or a text node",
                "HierarchyRequestError",
            );
        }
        None
    };
    let (parent, reference) = match position.as_str() {
        "beforebegin" => {
            let Some(parent) = dom::parent_node(target).or_else(&parent_or_document_error) else {
                return false;
            };
            (parent, Some(target))
        }
        "afterbegin" => (target, dom::first_child(target)),
        "beforeend" => (target, None),
        "afterend" => {
            let Some(parent) = dom::parent_node(target).or_else(parent_or_document_error) else {
                return false;
            };
            (parent, dom::next_sibling(target))
        }
        _ => dom_exception("The insertion position is invalid", "SyntaxError"),
    };
    if parent == 0 {
        dom_exception(
            "The document cannot contain another element or a text node",
            "HierarchyRequestError",
        );
    }
    ensure_tree_insertion(parent, child);
    match reference {
        Some(reference) => dom::insert_before(parent, child, reference),
        None => dom::append_child(parent, child),
    }
    pin_element_subtree(child);
    true
}

fn descendant_text_content(node: u32) -> String {
    match dom::node_type(node) {
        3 | 4 => dom::get_text_content(node).unwrap_or_default(),
        1 | 11 => dom::children(node)
            .into_iter()
            .map(descendant_text_content)
            .collect(),
        _ => String::new(),
    }
}

fn node_text_content(node: u32) -> Value {
    match dom::node_type(node) {
        1 | 11 => Value::string(&descendant_text_content(node)),
        3 | 4 | 7 | 8 => Value::string(&dom::get_text_content(node).unwrap_or_default()),
        _ => Value::Null,
    }
}

/// camelCase CSS property → kebab-case (`fontSize` → `font-size`).
fn camel_to_kebab(s: &str) -> String {
    if s == "cssFloat" {
        return "float".to_string();
    }
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// kebab-case → camelCase (`font-size` → `fontSize`). Reserved for a future
/// `getComputedStyle` enumeration surface; currently the bridge only needs
/// the camel→kebab direction.
#[allow(dead_code)]
fn kebab_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = false;
    for c in s.chars() {
        if c == '-' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

// ── Event type mapping (memoized for custom names) ─────────────────────────

fn event_type_for(name: &str) -> EventType {
    // Known names map deterministically; unknown names mint a fresh Custom id
    // on every EventType::from_str call, so they must be memoized.
    if let Some(et) = EventType::from_str(name) {
        if !matches!(et, EventType::Custom(_)) {
            return et;
        }
        CUSTOM_EVENT_TYPES.with(|m| {
            let mut map = m.borrow_mut();
            if let Some(&memo) = map.get(name) {
                return memo;
            }
            map.insert(name.to_string(), et);
            if let EventType::Custom(id) = et {
                CUSTOM_EVENT_NAMES.with(|n| n.borrow_mut().insert(id, name.to_string()));
            }
            et
        })
    } else {
        EventType::Custom(0)
    }
}

fn event_type_name(et: EventType) -> String {
    use EventType::*;
    match et {
        Click => "click",
        DblClick => "dblclick",
        ContextMenu => "contextmenu",
        MouseDown => "mousedown",
        MouseUp => "mouseup",
        MouseMove => "mousemove",
        MouseEnter => "mouseenter",
        MouseLeave => "mouseleave",
        MouseOver => "mouseover",
        MouseOut => "mouseout",
        PointerDown => "pointerdown",
        PointerUp => "pointerup",
        PointerMove => "pointermove",
        PointerEnter => "pointerenter",
        PointerLeave => "pointerleave",
        PointerOver => "pointerover",
        PointerOut => "pointerout",
        PointerCancel => "pointercancel",
        KeyDown => "keydown",
        KeyUp => "keyup",
        KeyPress => "keypress",
        Focus => "focus",
        Blur => "blur",
        FocusIn => "focusin",
        FocusOut => "focusout",
        Input => "input",
        Change => "change",
        Scroll => "scroll",
        Wheel => "wheel",
        Resize => "resize",
        TouchStart => "touchstart",
        TouchEnd => "touchend",
        TouchMove => "touchmove",
        TouchCancel => "touchcancel",
        CompositionStart => "compositionstart",
        CompositionUpdate => "compositionupdate",
        CompositionEnd => "compositionend",
        BeforeInput => "beforeinput",
        SelectionChange => "selectionchange",
        PopState => "popstate",
        HashChange => "hashchange",
        Custom(id) => {
            return CUSTOM_EVENT_NAMES
                .with(|n| n.borrow().get(&id).cloned())
                .unwrap_or_else(|| "custom".to_string());
        }
    }
    .to_string()
}

// ── Selector matching (simple selectors + descendant combinator) ───────────
// Supported: `tag`, `#id`, `.class`, compounds (`tag.a.b`, `#id.a`) and
/// descendant chains (`div .foo`). NOT supported: `>`, `+`, `~`, `:pseudo`,
/// `[attr]`, `*` — see module docs / gap report.

fn element_is_invalid(node: u32) -> bool {
    let tag = dom::tag_name(node);
    let required = dom::get_attribute(node, "required").is_some();
    let self_invalid = match tag.as_str() {
        "input" | "textarea" if required => {
            dom::get_attribute(node, "value").is_none_or(|value| value.is_empty())
        }
        "select" if required => !descendant_elements(node).into_iter().any(|option| {
            dom::tag_name(option) == "option"
                && dom::get_attribute(option, "selected").is_some()
                && dom::get_attribute(option, "value").map_or_else(
                    || !dom::inner_text(option).is_empty(),
                    |value| !value.is_empty(),
                )
        }),
        _ => false,
    };
    self_invalid
        || matches!(tag.as_str(), "fieldset" | "form")
            && descendant_elements(node)
                .into_iter()
                .any(element_is_invalid)
}

fn popover_is_open(node: u32) -> bool {
    get_expando(node, POPOVER_OPEN_EXPANDO).as_ref() == Some(&Value::Bool(true))
}

fn ensure_popover_can_toggle(node: u32) {
    if dom::node_type(node) != 1 || !dom::has_attribute(node, "popover") {
        dom_exception(
            "The element does not have a popover attribute",
            "NotSupportedError",
        );
    }
    if !node_is_connected(node) {
        dom_exception(
            "The popover element is not connected to a document",
            "InvalidStateError",
        );
    }
}

fn set_popover_open(node: u32, open: bool) {
    ensure_popover_can_toggle(node);
    set_expando(node, POPOVER_OPEN_EXPANDO, Value::Bool(open));
}

fn dialog_is_modal(node: u32) -> bool {
    dom::tag_name(node) == "dialog"
        && dom::has_attribute(node, "open")
        && get_expando(node, DIALOG_MODAL_EXPANDO).as_ref() == Some(&Value::Bool(true))
}

fn ensure_dialog_element(node: u32) {
    if dom::tag_name(node) != "dialog" {
        type_error("Dialog methods require an HTMLDialogElement");
    }
}

fn show_dialog(node: u32, modal: bool) {
    ensure_dialog_element(node);
    if modal && !node_is_connected(node) {
        dom_exception(
            "The dialog is not connected to a document",
            "InvalidStateError",
        );
    }
    if dom::has_attribute(node, "open") {
        if dialog_is_modal(node) == modal {
            return;
        }
        dom_exception(
            "The dialog is already open in a different mode",
            "InvalidStateError",
        );
    }
    dom::set_attribute(node, "open", "");
    set_expando(node, DIALOG_MODAL_EXPANDO, Value::Bool(modal));
}

fn close_dialog(node: u32, return_value: Value) {
    ensure_dialog_element(node);
    if !dom::has_attribute(node, "open") {
        return;
    }
    if !return_value.is_undefined() {
        set_expando(
            node,
            DIALOG_RETURN_VALUE_EXPANDO,
            Value::string(&return_value.to_js_string()),
        );
    }
    dom::remove_attribute(node, "open");
    set_expando(node, DIALOG_MODAL_EXPANDO, Value::Bool(false));
    dispatch_sync(node, event_type_for("close"), EventData::None);
}

fn element_has_focus(node: u32) -> bool {
    ACTIVE_ELEMENT.with(|active| *active.borrow() == Some(node))
}

fn element_has_focus_within(node: u32) -> bool {
    ACTIVE_ELEMENT.with(|active| {
        active
            .borrow()
            .is_some_and(|focused| focused == node || is_ancestor_of(node, focused))
    })
}

fn matches_simple(selector: &str, node: u32, scope: Option<u32>) -> bool {
    if selector.is_empty() || matches!(selector, ">" | "+" | "~") {
        return false;
    }
    if dom::node_type(node) != 1 {
        return false;
    }
    if !selector.contains(":scope")
        && !selector.contains(":has(")
        && !selector.contains(":invalid")
        && !selector.contains(":target")
        && !selector.contains(":popover-open")
        && !selector.contains(":modal")
        && !selector.contains(":focus")
        && let Ok(matched) = dom::matches_selector(node, selector)
    {
        return matched;
    }
    let mut compound = selector.to_string();

    while let Some(start) = compound.find('[') {
        let Some(relative_end) = compound[start + 1..].find(']') else {
            return false;
        };
        let end = start + 1 + relative_end;
        let attribute = compound[start + 1..end].trim();
        let matches = if let Some((name, expected)) = attribute.split_once('=') {
            let expected = expected
                .trim()
                .strip_prefix(['\'', '"'])
                .and_then(|value| value.strip_suffix(['\'', '"']))
                .unwrap_or(expected.trim());
            dom::get_attribute(node, name.trim()).as_deref() == Some(expected)
        } else {
            dom::get_attribute(node, attribute).is_some()
        };
        if !matches {
            return false;
        }
        compound.replace_range(start..=end, "");
    }

    if let Some(start) = compound.find(":not(") {
        let Some(relative_end) = compound[start + 5..].find(')') else {
            return false;
        };
        let end = start + 5 + relative_end;
        if matches_simple(&compound[start + 5..end], node, scope) {
            return false;
        }
        compound.replace_range(start..=end, "");
    }
    if let Some(start) = compound.find(":has(") {
        let Some(relative_end) = compound[start + 5..].find(')') else {
            return false;
        };
        let end = start + 5 + relative_end;
        let relative = compound[start + 5..end].trim();
        let matches = relative == "> :scope"
            && scope.is_some_and(|scope| {
                dom::children(node)
                    .into_iter()
                    .any(|child| child == scope && dom::node_type(child) == 1)
            });
        if !matches {
            return false;
        }
        compound.replace_range(start..=end, "");
    }
    for pseudo in [
        ":scope",
        ":empty",
        ":first-child",
        ":last-child",
        ":invalid",
        ":target",
        ":popover-open",
        ":modal",
        ":focus-within",
        ":focus",
    ] {
        if !compound.contains(pseudo) {
            continue;
        }
        let matches = match pseudo {
            ":scope" => scope == Some(node),
            ":empty" => dom::children(node).is_empty(),
            ":first-child" => dom::parent_node(node).is_some_and(|parent| {
                dom::children(parent)
                    .into_iter()
                    .find(|child| dom::node_type(*child) == 1)
                    == Some(node)
            }),
            ":last-child" => dom::parent_node(node).is_some_and(|parent| {
                dom::children(parent)
                    .into_iter()
                    .rev()
                    .find(|child| dom::node_type(*child) == 1)
                    == Some(node)
            }),
            ":invalid" => element_is_invalid(node),
            ":target" => {
                let owner_document =
                    get_expando(node, "ownerDocument").unwrap_or_else(document_value);
                let hash = owner_document
                    .get_property("location")
                    .get_property("hash")
                    .to_js_string();
                node_is_connected(node)
                    && hash.strip_prefix('#').is_some_and(|target| {
                        !target.is_empty()
                        && dom::get_attribute(node, "id").as_deref() == Some(target)
                    })
            }
            ":popover-open" => popover_is_open(node),
            ":modal" => dialog_is_modal(node),
            ":focus-within" => element_has_focus_within(node),
            ":focus" => element_has_focus(node),
            _ => false,
        };
        if !matches {
            return false;
        }
        compound = compound.replace(pseudo, "");
    }
    if compound.contains(['>', '+', '~', ':', '[', ']']) {
        return false;
    }

    let no_namespace = compound.starts_with('|');
    if no_namespace && !namespace_uri(node).is_empty() {
        return false;
    }
    let compound = compound
        .strip_prefix("*|")
        .or_else(|| compound.strip_prefix('|'))
        .unwrap_or(&compound);
    if compound == "*" {
        return true;
    }
    // #id part
    if let Some(hash) = compound.find('#') {
        let id: String = compound[hash + 1..]
            .chars()
            .take_while(|c| *c != '.' && *c != '#')
            .collect();
        if dom::get_attribute(node, "id").as_deref() != Some(id.as_str()) {
            return false;
        }
    }
    // .class parts
    for cls in compound.split('.').skip(1) {
        let cls: String = cls.chars().take_while(|c| *c != '#').collect();
        if cls.is_empty() {
            continue;
        }
        if !dom::class_list_contains(node, &cls) {
            return false;
        }
    }
    // tag part (leading run before '.' or '#')
    let tag: String = compound
        .chars()
        .take_while(|c| *c != '.' && *c != '#')
        .collect();
    let html_document = get_expando(node, "ownerDocument")
        .unwrap_or_else(document_value)
        .get_property("contentType")
        .to_js_string()
        == "text/html";
    if !tag.is_empty() && !element_matches_tag_name(node, &tag, html_document) {
        return false;
    }
    true
}

/// Split a descendant selector only on whitespace outside attribute values.
/// A plain `split_whitespace` turns `[title='two words']` into three invalid
/// selector components even though the spaces belong to the quoted value.
fn selector_chain_parts(selector: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = None;
    let mut bracket_depth = 0_u32;
    let mut paren_depth = 0_u32;
    let mut quote = None;
    let mut escaped = false;

    for (index, ch) in selector.char_indices() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active_quote {
                quote = None;
            }
            continue;
        }

        match ch {
            '\'' | '"' if bracket_depth > 0 || paren_depth > 0 => quote = Some(ch),
            '[' => {
                bracket_depth += 1;
                start.get_or_insert(index);
            }
            ']' => {
                if bracket_depth == 0 {
                    return Vec::new();
                }
                bracket_depth -= 1;
            }
            '(' => {
                paren_depth += 1;
                start.get_or_insert(index);
            }
            ')' => {
                if paren_depth == 0 {
                    return Vec::new();
                }
                paren_depth -= 1;
            }
            _ if ch.is_whitespace() && bracket_depth == 0 && paren_depth == 0 => {
                if let Some(part_start) = start.take() {
                    parts.push(&selector[part_start..index]);
                }
            }
            _ => {
                start.get_or_insert(index);
            }
        }
    }

    if quote.is_some() || bracket_depth != 0 || paren_depth != 0 {
        return Vec::new();
    }
    if let Some(part_start) = start {
        parts.push(&selector[part_start..]);
    }
    parts
}

fn is_ancestor_of(ancestor: u32, node: u32) -> bool {
    let mut cur = dom::parent_node(node);
    while let Some(id) = cur {
        if id == ancestor {
            return true;
        }
        cur = dom::parent_node(id);
    }
    false
}

fn ensure_tree_parent_and_ancestry(parent: u32, child: u32) {
    if parent == child || is_ancestor_of(child, parent) {
        dom_exception(
            "A node cannot be inserted into itself or one of its descendants",
            "HierarchyRequestError",
        );
    }
    if matches!(dom::node_type(parent), 3 | 4 | 7 | 8 | 10) {
        dom_exception(
            "This node type cannot contain children",
            "HierarchyRequestError",
        );
    }
}

fn ensure_tree_child_type_and_adopt(parent: u32, child: u32) {
    let child_type = dom::node_type(child);
    if !matches!(child_type, 1 | 3 | 4 | 7 | 8 | 10 | 11)
        || (child_type == 10 && dom::node_type(parent) != 9)
        || (child_type == 3 && dom::node_type(parent) == 9)
    {
        dom_exception(
            "This node type is not valid at the requested position",
            "HierarchyRequestError",
        );
    }

    let owner_document = if parent == 0 {
        document_value()
    } else {
        get_expando(parent, "ownerDocument").unwrap_or_else(document_value)
    };
    walk_subtree(child, &mut |descendant| {
        set_expando(descendant, "ownerDocument", owner_document.clone());
        update_cached_attribute_owner_document(descendant, &owner_document);
    });
}

fn ensure_tree_insertion(parent: u32, child: u32) {
    ensure_tree_parent_and_ancestry(parent, child);
    ensure_tree_child_type_and_adopt(parent, child);
}

pub(crate) fn is_ancestor_node(ancestor: u32, node: u32) -> bool {
    is_ancestor_of(ancestor, node)
}

fn previous_element_sibling(node: u32) -> Option<u32> {
    let mut sibling = dom::previous_sibling(node);
    while let Some(candidate) = sibling {
        if dom::node_type(candidate) == 1 {
            return Some(candidate);
        }
        sibling = dom::previous_sibling(candidate);
    }
    None
}

fn matches_selector_chain_at(node: u32, parts: &[&str], index: usize, scope: Option<u32>) -> bool {
    if !matches_simple(parts[index], node, scope) {
        return false;
    }
    if index == 0 {
        return true;
    }
    let (left, combinator) = if matches!(parts[index - 1], ">" | "+" | "~") {
        if index < 2 {
            return false;
        }
        (index - 2, Some(parts[index - 1]))
    } else {
        (index - 1, None)
    };
    match combinator {
        Some(">") => dom::parent_node(node)
            .is_some_and(|parent| matches_selector_chain_at(parent, parts, left, scope)),
        Some("+") => previous_element_sibling(node)
            .is_some_and(|sibling| matches_selector_chain_at(sibling, parts, left, scope)),
        Some("~") => {
            let mut sibling = previous_element_sibling(node);
            while let Some(candidate) = sibling {
                if matches_selector_chain_at(candidate, parts, left, scope) {
                    return true;
                }
                sibling = previous_element_sibling(candidate);
            }
            false
        }
        _ => {
            let mut ancestor = dom::parent_node(node);
            while let Some(candidate) = ancestor {
                if matches_selector_chain_at(candidate, parts, left, scope) {
                    return true;
                }
                ancestor = dom::parent_node(candidate);
            }
            false
        }
    }
}

/// Right-to-left selector matching against the ancestor/sibling chain.
fn matches_selector_chain_in_scope(node: u32, parts: &[&str], scope: Option<u32>) -> bool {
    !parts.is_empty() && matches_selector_chain_at(node, parts, parts.len() - 1, scope)
}

fn selector_uses_runtime_state(selector: &str) -> bool {
    [
        ":scope",
        ":invalid",
        ":target",
        ":popover-open",
        ":modal",
        ":focus-within",
        ":focus",
    ]
    .into_iter()
    .any(|pseudo| selector.contains(pseudo))
        || selector_has_no_namespace_type(selector)
}

fn selector_has_no_namespace_type(selector: &str) -> bool {
    let chars = selector.chars().collect::<Vec<_>>();
    chars.iter().enumerate().any(|(index, character)| {
        if *character != '|' {
            return false;
        }
        let boundary_before = index == 0
            || matches!(
                chars[index - 1],
                ' ' | '\t' | '\n' | '\r' | '\u{000c}' | '>' | '+' | '~' | ',' | '('
            );
        boundary_before
            && chars
                .get(index + 1)
                .is_some_and(|next| *next == '*' || *next == '-' || *next == '_' || next.is_alphabetic())
    })
}

fn selector_for_static_validation(selector: &str) -> String {
    let chars = selector.chars().collect::<Vec<_>>();
    let mut normalized = String::with_capacity(selector.len() + 2);
    for (index, character) in chars.iter().enumerate() {
        if *character == '|'
            && (index == 0
                || matches!(
                    chars[index - 1],
                    ' ' | '\t' | '\n' | '\r' | '\u{000c}' | '>' | '+' | '~' | ',' | '('
                ))
            && chars
                .get(index + 1)
                .is_some_and(|next| *next == '*' || *next == '-' || *next == '_' || next.is_alphabetic())
        {
            normalized.push('*');
        }
        normalized.push(*character);
    }
    normalized
}

fn query_selector_matches(node: u32, selector: &str, scope: Option<u32>) -> bool {
    if selector_uses_runtime_state(selector) {
        let parts = selector_chain_parts(selector);
        return matches_selector_chain_in_scope(node, &parts, scope);
    }
    dom::matches_selector(node, selector).is_ok_and(|matched| matched)
}

fn simple_id_selector(selector: &str) -> Option<String> {
    w3cos_dom::stylesheet::parse_simple_id_selector(selector)
}

fn cached_subtree_id_matches(root: u32, include_root: bool, id: &str) -> Vec<u32> {
    let generation = dom::mutation_generation();
    SELECTOR_ID_CACHE_GENERATION.with(|cached_generation| {
        if cached_generation.get() != generation {
            cached_generation.set(generation);
            SELECTOR_ID_CACHE.with(|cache| cache.borrow_mut().clear());
        }
    });
    SELECTOR_ID_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let index = cache.entry(root).or_insert_with(|| {
            let mut index = HashMap::<String, Vec<u32>>::new();
            for node in inclusive_descendant_elements(root) {
                if let Some(id) = dom::get_attribute(node, "id") {
                    index.entry(id).or_default().push(node);
                }
            }
            index
        });
        index
            .get(id)
            .into_iter()
            .flatten()
            .copied()
            .filter(|node| include_root || *node != root)
            .collect()
    })
}

fn query_selector_all_scoped(scope: Option<u32>, selector: &str) -> Vec<u32> {
    let selector = w3cos_dom::stylesheet::trim_css_whitespace(selector);
    if let Some(id) = simple_id_selector(selector) {
        let root = scope.unwrap_or_else(document_element_id);
        return cached_subtree_id_matches(root, scope.is_none(), &id);
    }
    if selector_uses_runtime_state(selector) && selector_chain_parts(selector).is_empty() {
        return Vec::new();
    }
    let candidates = scope.map_or_else(
        || inclusive_descendant_elements(document_element_id()),
        descendant_elements,
    );
    candidates
        .into_iter()
        .filter(|candidate| query_selector_matches(*candidate, selector, scope))
        .collect()
}

fn query_selector_argument(args: &[Value], validation_node: u32) -> String {
    if args.is_empty() {
        type_error("querySelector requires a selector");
    }
    let selector = arg(args, 0).to_js_string();
    let selector_for_validation = [
        ":focus-within",
        ":popover-open",
        ":invalid",
        ":target",
        ":modal",
        ":focus",
        ":scope",
    ]
    .into_iter()
    .fold(selector.clone(), |selector, pseudo| {
        selector.replace(pseudo, ":root")
    });
    let selector_for_validation = selector_for_static_validation(&selector_for_validation);
    if dom::matches_selector(validation_node, &selector_for_validation).is_err() {
        dom_exception("Invalid selector", "SyntaxError");
    }
    selector
}

fn query_live_document_all(selector: &str) -> Vec<u32> {
    let root = document_element_id();
    let mut matches = Vec::new();
    if query_selector_matches(root, selector, Some(root)) {
        matches.push(root);
    }
    matches.extend(query_selector_all_scoped(Some(root), selector));
    matches
}

// ── Element values ─────────────────────────────────────────────────────────

/// True when the native node still belongs to a live tree. Template
/// contents are never `is_connected`, but their children have parents.
/// Those wrappers stay Strong so intern does not drop identity or wipe
/// expandos such as `template.content`.
fn js_wrapper_should_pin(node: u32) -> bool {
    dom::is_connected(node) || dom::parent_node(node).is_some()
}

/// Get (or create) the JS `Value` for a DOM node. Memoized per node so
/// identity comparisons (`parent.appendChild(x) === x`) hold.
///
/// Tree-owned nodes intern Strong. Parentless orphans intern Weak so
/// dropping the last JS handle can collect the wrapper without
/// `reset_bridge()`.
pub fn element_value(node: u32) -> Value {
    let cached = ELEMENT_VALUES.with(|c| {
        let mut map = c.borrow_mut();
        match map.get(&node) {
            Some(ElementMemo::Strong(value)) => Some(value.clone()),
            Some(ElementMemo::Weak(weak)) => match upgrade_realm_object(weak) {
                Some(value) => {
                    if js_wrapper_should_pin(node) {
                        map.insert(node, ElementMemo::Strong(value.clone()));
                    }
                    Some(value)
                }
                None => {
                    map.remove(&node);
                    None
                }
            },
            None => None,
        }
    });
    // Do not purge expandos/listeners on a cache miss. A dead Weak can
    // still belong to a native node the parser or a template fragment
    // owns; wiping `template.content` would orphan the real tree. Purge
    // happens in `release_element_wrapper` after detach when no JS
    // handle remains.
    if let Some(value) = cached {
        return value;
    }
    let value = build_element_value(node);
    intern_element_value(node, value.clone());
    value
}

fn intern_element_value(node: u32, value: Value) {
    ELEMENT_VALUES.with(|c| {
        c.borrow_mut().insert(
            node,
            if js_wrapper_should_pin(node) {
                ElementMemo::Strong(value)
            } else {
                ElementMemo::Weak(weak_realm_object(&value))
            },
        );
    });
}

fn walk_subtree(root: u32, visit: &mut impl FnMut(u32)) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        visit(id);
        let mut child = dom::first_child(id);
        while let Some(cid) = child {
            stack.push(cid);
            child = dom::next_sibling(cid);
        }
    }
}

fn copy_cloned_node_identity(source: u32, clone: u32) {
    for key in [
        "namespaceURI",
        "prefix",
        "localName",
        "ownerDocument",
        "publicId",
        "systemId",
    ] {
        if let Some(value) = get_expando(source, key) {
            set_expando(clone, key, value);
        }
    }
    for (source_child, clone_child) in dom::children(source).into_iter().zip(dom::children(clone)) {
        copy_cloned_node_identity(source_child, clone_child);
    }
}

fn pin_element_subtree(root: u32) {
    walk_subtree(root, &mut pin_element_wrapper);
}

fn pin_element_wrapper(node: u32) {
    if !js_wrapper_should_pin(node) {
        return;
    }
    ELEMENT_VALUES.with(|c| {
        let mut map = c.borrow_mut();
        if let Some(ElementMemo::Weak(weak)) = map.get(&node) {
            if let Some(value) = upgrade_realm_object(weak) {
                map.insert(node, ElementMemo::Strong(value));
            }
        }
    });
}

fn release_element_subtree(root: u32) {
    walk_subtree(root, &mut release_element_wrapper);
}

fn release_element_wrapper(node: u32) {
    if js_wrapper_should_pin(node) {
        return;
    }
    ELEMENT_VALUES.with(|c| {
        let mut map = c.borrow_mut();
        match map.remove(&node) {
            Some(ElementMemo::Strong(value)) => {
                map.insert(node, ElementMemo::Weak(weak_realm_object(&value)));
            }
            Some(ElementMemo::Weak(weak)) => {
                if weak.strong_count() == 0 {
                    purge_detached_node_js(node);
                } else {
                    map.insert(node, ElementMemo::Weak(weak));
                }
            }
            None => {}
        }
    });
}

fn purge_detached_node_js(node: u32) {
    ELEMENT_PROPS.with(|props| props.borrow_mut().retain(|(id, _), _| *id != node));
    STYLE_CACHE.with(|cache| cache.borrow_mut().retain(|(id, _), _| *id != node));
    LISTENERS.with(|listeners| {
        listeners
            .borrow_mut()
            .retain(|listener| listener.node != node)
    });
    NATIVELY_REGISTERED.with(|registered| registered.borrow_mut().retain(|(id, _)| *id != node));
}

pub(crate) fn create_namespaced_element(namespace: &str, tag: &str) -> u32 {
    let id = dom::create_element(tag);
    dom::set_html_element(id, namespace == crate::html_parser_state::HTML_NAMESPACE);
    let (prefix, local_name) = tag
        .split_once(':')
        .map_or((None, tag), |(prefix, local_name)| {
            (Some(prefix), local_name)
        });
    set_expando(
        id,
        "namespaceURI",
        if namespace.is_empty() {
            Value::Null
        } else {
            Value::string(namespace)
        },
    );
    set_expando(
        id,
        "prefix",
        prefix.map(Value::string).unwrap_or(Value::Null),
    );
    set_expando(id, "localName", Value::string(local_name));
    id
}

pub(crate) fn namespace_uri(node: u32) -> String {
    match get_expando(node, "namespaceURI") {
        Some(namespace) if namespace.is_null() => String::new(),
        Some(namespace) => namespace.to_js_string(),
        None => "http://www.w3.org/1999/xhtml".to_string(),
    }
}

fn normalized_namespace_argument(value: &Value) -> Option<String> {
    (!value.is_null() && !value.is_undefined())
        .then(|| value.to_js_string())
        .filter(|value| !value.is_empty())
}

fn declared_namespace_uri(node: u32, prefix: Option<&str>) -> Option<Option<String>> {
    dom::with_document(|document| {
        let node = document.get_node(NodeId::from_u32(node));
        node.attributes
            .iter()
            .enumerate()
            .find_map(|(index, (qualified_name, value))| {
                let metadata = node.attribute_namespace_at(index);
                let namespace = metadata
                    .and_then(|attribute| attribute.namespace.as_ref())
                    .map(|namespace| namespace.as_str());
                if namespace.as_deref() != Some(crate::html_parser_state::XMLNS_NAMESPACE) {
                    return None;
                }
                let attribute_prefix = metadata
                    .and_then(|attribute| attribute.prefix.as_ref())
                    .map(|prefix| prefix.as_str());
                let local_name = metadata
                    .map(|attribute| attribute.local_name.as_str())
                    .unwrap_or_else(|| qualified_name.as_str());
                let matches = match prefix {
                    None => attribute_prefix.is_none() && local_name == "xmlns",
                    Some(prefix) => {
                        attribute_prefix.as_deref() == Some("xmlns") && local_name == prefix
                    }
                };
                matches.then(|| (!value.is_empty()).then(|| value.clone()))
            })
    })
}

fn declared_prefix_for_namespace(node: u32, namespace: &str) -> Option<String> {
    dom::with_document(|document| {
        let node = document.get_node(NodeId::from_u32(node));
        node.attributes
            .iter()
            .enumerate()
            .find_map(|(index, (_, value))| {
                let metadata = node.attribute_namespace_at(index);
                let attribute_namespace = metadata
                    .and_then(|attribute| attribute.namespace.as_ref())
                    .map(|namespace| namespace.as_str());
                let attribute_prefix = metadata
                    .and_then(|attribute| attribute.prefix.as_ref())
                    .map(|prefix| prefix.as_str());
                (attribute_namespace.as_deref() == Some(crate::html_parser_state::XMLNS_NAMESPACE)
                    && attribute_prefix.as_deref() == Some("xmlns")
                    && value == namespace)
                    .then(|| {
                        metadata
                            .expect("namespaced attribute metadata was present")
                            .local_name
                            .as_str()
                    })
            })
    })
}

fn lookup_namespace_uri_for_native_node(node: u32, prefix: Option<&str>) -> Option<String> {
    match dom::node_type(node) {
        1 => {
            if prefix == Some("xml") {
                return Some(crate::html_parser_state::XML_NAMESPACE.to_string());
            }
            if prefix == Some("xmlns") {
                return Some(crate::html_parser_state::XMLNS_NAMESPACE.to_string());
            }
            let element_prefix = get_expando(node, "prefix")
                .and_then(|prefix| (!prefix.is_null()).then(|| prefix.to_js_string()))
                .filter(|prefix| !prefix.is_empty());
            let element_namespace = namespace_uri(node);
            if !element_namespace.is_empty() && element_prefix.as_deref() == prefix {
                return Some(element_namespace);
            }
            if let Some(namespace) = declared_namespace_uri(node, prefix) {
                return namespace;
            }
            dom::parent_node(node)
                .filter(|parent| dom::node_type(*parent) == 1)
                .and_then(|parent| lookup_namespace_uri_for_native_node(parent, prefix))
        }
        3 | 4 | 7 | 8 => dom::parent_node(node)
            .filter(|parent| dom::node_type(*parent) == 1)
            .and_then(|parent| lookup_namespace_uri_for_native_node(parent, prefix)),
        _ => None,
    }
}

fn lookup_prefix_for_native_node(node: u32, namespace: &str) -> Option<String> {
    match dom::node_type(node) {
        1 => {
            if namespace == crate::html_parser_state::XML_NAMESPACE {
                return Some("xml".to_string());
            }
            if namespace == crate::html_parser_state::XMLNS_NAMESPACE {
                return Some("xmlns".to_string());
            }
            let element_prefix = get_expando(node, "prefix")
                .and_then(|prefix| (!prefix.is_null()).then(|| prefix.to_js_string()))
                .filter(|prefix| !prefix.is_empty());
            if namespace_uri(node) == namespace && element_prefix.is_some() {
                return element_prefix;
            }
            if let Some(prefix) = declared_prefix_for_namespace(node, namespace) {
                return Some(prefix);
            }
            dom::parent_node(node)
                .filter(|parent| dom::node_type(*parent) == 1)
                .and_then(|parent| lookup_prefix_for_native_node(parent, namespace))
        }
        3 | 4 | 7 | 8 => dom::parent_node(node)
            .filter(|parent| dom::node_type(*parent) == 1)
            .and_then(|parent| lookup_prefix_for_native_node(parent, namespace)),
        _ => None,
    }
}

fn lookup_namespace_uri_for_value(node: &Value, prefix: &Value) -> Option<String> {
    let prefix = normalized_namespace_argument(prefix);
    match node.get_property("nodeType").to_u32() {
        9 => node
            .get_property("documentElement")
            .as_object()
            .and_then(|_| node_id_of(&node.get_property("documentElement")))
            .and_then(|root| lookup_namespace_uri_for_native_node(root, prefix.as_deref())),
        2 => node
            .get_property("ownerElement")
            .as_object()
            .and_then(|_| node_id_of(&node.get_property("ownerElement")))
            .and_then(|owner| lookup_namespace_uri_for_native_node(owner, prefix.as_deref())),
        _ => node_id_of(node)
            .and_then(|node| lookup_namespace_uri_for_native_node(node, prefix.as_deref())),
    }
}

fn lookup_namespace_uri_result(node: &Value, prefix: &Value) -> Value {
    lookup_namespace_uri_for_value(node, prefix)
        .map(|namespace| Value::string(&namespace))
        .unwrap_or(Value::Null)
}

fn lookup_prefix_result(node: &Value, namespace: &Value) -> Value {
    let Some(namespace) = normalized_namespace_argument(namespace) else {
        return Value::Null;
    };
    let prefix = match node.get_property("nodeType").to_u32() {
        9 => node
            .get_property("documentElement")
            .as_object()
            .and_then(|_| node_id_of(&node.get_property("documentElement")))
            .and_then(|root| lookup_prefix_for_native_node(root, &namespace)),
        2 => node
            .get_property("ownerElement")
            .as_object()
            .and_then(|_| node_id_of(&node.get_property("ownerElement")))
            .and_then(|owner| lookup_prefix_for_native_node(owner, &namespace)),
        _ => node_id_of(node).and_then(|node| lookup_prefix_for_native_node(node, &namespace)),
    };
    prefix
        .map(|prefix| Value::string(&prefix))
        .unwrap_or(Value::Null)
}

fn is_default_namespace_result(node: &Value, namespace: &Value) -> Value {
    Value::Bool(
        lookup_namespace_uri_for_value(node, &Value::Null)
            == normalized_namespace_argument(namespace),
    )
}

fn native_node_namespace(node: u32) -> Option<String> {
    match get_expando(node, "namespaceURI") {
        Some(namespace) if namespace.is_null() => None,
        Some(namespace) => Some(namespace.to_js_string()),
        None if dom::node_type(node) == 1 => {
            Some(crate::html_parser_state::HTML_NAMESPACE.to_string())
        }
        None => None,
    }
}

fn native_node_prefix(node: u32) -> Option<String> {
    get_expando(node, "prefix")
        .filter(|prefix| !prefix.is_null() && !prefix.is_undefined())
        .map(|prefix| prefix.to_js_string())
        .filter(|prefix| !prefix.is_empty())
}

fn native_node_local_name(node: u32) -> String {
    get_expando(node, "localName")
        .map(|name| name.to_js_string())
        .unwrap_or_else(|| dom::tag_name(node))
}

fn native_node_attributes(node: u32) -> Vec<(Option<String>, String, String)> {
    let mut attributes = dom::with_document(|document| {
        let node = document.get_node(NodeId::from_u32(node));
        node.attributes
            .iter()
            .enumerate()
            .map(|(index, (qualified_name, value))| {
                let metadata = node.attribute_namespace_at(index);
                (
                    metadata
                        .and_then(|attribute| attribute.namespace.as_ref())
                        .map(|namespace| namespace.as_str()),
                    metadata
                        .map(|attribute| attribute.local_name.as_str())
                        .unwrap_or_else(|| qualified_name.as_str()),
                    value.clone(),
                )
            })
            .collect::<Vec<_>>()
    });
    attributes.sort();
    attributes
}

fn native_nodes_are_equal(left: u32, right: u32) -> bool {
    if left == right {
        return true;
    }
    let node_type = dom::node_type(left);
    if node_type != dom::node_type(right) {
        return false;
    }

    let properties_are_equal = match node_type {
        1 => {
            native_node_namespace(left) == native_node_namespace(right)
                && native_node_prefix(left) == native_node_prefix(right)
                && native_node_local_name(left) == native_node_local_name(right)
                && native_node_attributes(left) == native_node_attributes(right)
        }
        3 | 4 | 8 => dom::get_text_content(left) == dom::get_text_content(right),
        7 => {
            dom::tag_name(left) == dom::tag_name(right)
                && dom::get_text_content(left) == dom::get_text_content(right)
        }
        10 => {
            dom::tag_name(left) == dom::tag_name(right)
                && get_expando(left, "publicId").unwrap_or_else(|| Value::string(""))
                    == get_expando(right, "publicId").unwrap_or_else(|| Value::string(""))
                && get_expando(left, "systemId").unwrap_or_else(|| Value::string(""))
                    == get_expando(right, "systemId").unwrap_or_else(|| Value::string(""))
        }
        11 => true,
        _ => true,
    };
    properties_are_equal
        && dom::children(left).len() == dom::children(right).len()
        && dom::children(left)
            .into_iter()
            .zip(dom::children(right))
            .all(|(left, right)| native_nodes_are_equal(left, right))
}

fn document_children_for_equality(document: &Value) -> Vec<Value> {
    let children = document.get_property("__w3cos_document_children");
    if let Some(children) = children.as_array() {
        return children.borrow().clone();
    }
    if document.strict_eq(&document_value()) {
        return global_document_children();
    }
    Vec::new()
}

fn nodes_are_equal(left: &Value, right: &Value) -> bool {
    if left.strict_eq(right) {
        return true;
    }
    match (node_id_of(left), node_id_of(right)) {
        (Some(left), Some(right)) => native_nodes_are_equal(left, right),
        (None, None)
            if left.get_property("nodeType").to_u32() == 9
                && right.get_property("nodeType").to_u32() == 9 =>
        {
            let left_children = document_children_for_equality(left);
            let right_children = document_children_for_equality(right);
            left_children.len() == right_children.len()
                && left_children
                    .iter()
                    .zip(&right_children)
                    .all(|(left, right)| nodes_are_equal(left, right))
        }
        (None, None)
            if left.get_property("nodeType").to_u32() == 2
                && right.get_property("nodeType").to_u32() == 2 =>
        {
            normalized_namespace_argument(&left.get_property("namespaceURI"))
                == normalized_namespace_argument(&right.get_property("namespaceURI"))
                && left.get_property("localName").to_js_string()
                    == right.get_property("localName").to_js_string()
                && left.get_property("value").to_js_string()
                    == right.get_property("value").to_js_string()
        }
        _ => false,
    }
}

fn element_qualified_name(node: u32) -> String {
    let qualified_name = dom::tag_name(node);
    if namespace_uri(node) != crate::html_parser_state::HTML_NAMESPACE {
        return qualified_name;
    }

    let owner_document = get_expando(node, "ownerDocument").unwrap_or_else(document_value);
    if owner_document.get_property("contentType").to_js_string() == "text/html" {
        qualified_name.to_ascii_uppercase()
    } else {
        qualified_name
    }
}

fn normalized_attribute_name(node: u32, name: &str) -> String {
    let owner_document = get_expando(node, "ownerDocument").unwrap_or_else(document_value);
    if namespace_uri(node) == crate::html_parser_state::HTML_NAMESPACE
        && owner_document.get_property("contentType").to_js_string() == "text/html"
    {
        name.to_ascii_lowercase()
    } else {
        name.to_string()
    }
}

fn valid_processing_instruction_attribute_name(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(|character| {
            character == '\0'
                || character.is_ascii_whitespace()
                || matches!(character, '=' | '>' | '/')
        })
}

fn processing_instruction_attributes(node: u32) -> Vec<(String, String)> {
    let data = dom::get_text_content(node).unwrap_or_default();
    if let Some(attributes) = PROCESSING_INSTRUCTION_ATTRIBUTES.with(|cache| {
        cache
            .borrow()
            .get(&node)
            .filter(|(cached_data, _)| cached_data == &data)
            .map(|(_, attributes)| attributes.clone())
    }) {
        return attributes;
    }
    let bytes = data.as_bytes();
    let mut attributes = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index == bytes.len() {
            break;
        }
        let name_start = index;
        while index < bytes.len() && bytes[index] != b'=' && !bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index == name_start || index == bytes.len() || bytes[index] != b'=' {
            return Vec::new();
        }
        let name = &data[name_start..index];
        if !valid_processing_instruction_attribute_name(name) {
            return Vec::new();
        }
        index += 1;
        let Some(quote) = bytes.get(index).copied().filter(|quote| matches!(quote, b'\'' | b'"'))
        else {
            return Vec::new();
        };
        index += 1;
        let value_start = index;
        while index < bytes.len() && bytes[index] != quote {
            index += 1;
        }
        if index == bytes.len() {
            return Vec::new();
        }
        attributes.push((
            name.to_string(),
            decode_html_entities(&data[value_start..index]),
        ));
        index += 1;
        if index < bytes.len() && !bytes[index].is_ascii_whitespace() {
            return Vec::new();
        }
    }
    PROCESSING_INSTRUCTION_ATTRIBUTES.with(|cache| {
        cache
            .borrow_mut()
            .insert(node, (data, attributes.clone()));
    });
    attributes
}

fn serialize_processing_instruction_attributes(attributes: &[(String, String)]) -> String {
    attributes
        .iter()
        .map(|(name, value)| {
            let value = value
                .replace('&', "&amp;")
                .replace('"', "&quot;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            format!("{name}=\"{value}\"")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn set_processing_instruction_attribute(node: u32, name: &str, value: Option<String>) {
    if !valid_processing_instruction_attribute_name(name) {
        dom_exception(
            "Processing instruction attribute name is not valid",
            "InvalidCharacterError",
        );
    }
    let mut attributes = processing_instruction_attributes(node);
    if let Some(index) = attributes.iter().position(|(current, _)| current == name) {
        if let Some(value) = value {
            attributes[index].1 = value;
        } else {
            attributes.remove(index);
        }
    } else if let Some(value) = value {
        attributes.push((name.to_string(), value));
    } else {
        return;
    }
    let data = serialize_processing_instruction_attributes(&attributes);
    dom::set_text_content(node, &data);
    PROCESSING_INSTRUCTION_ATTRIBUTES.with(|cache| {
        cache.borrow_mut().insert(node, (data, attributes));
    });
}

pub(crate) fn ensure_template_content(node: u32) -> u32 {
    if let Some(content) = get_expando(node, "content").and_then(|value| node_id_of(&value)) {
        return content;
    }
    let content = dom::create_document_fragment();
    set_expando(node, "content", element_value(content));
    content
}

pub(crate) fn install_document_doctype(name: &str, public_id: &str, system_id: &str) {
    let doctype = create_document_type_value(name, public_id, system_id);
    let document = document_value();
    if let Some(node) = node_id_of(&doctype) {
        set_expando(node, "ownerDocument", document.clone());
        set_expando(node, "parentNode", document.clone());
    }
    document.set_property("doctype", doctype);
}

pub(crate) fn sync_global_document_child_relationships() {
    let document = document_value();
    let children = global_document_children();
    for (index, child) in children.iter().enumerate() {
        if let Some(node) = node_id_of(child) {
            set_expando(node, "parentNode", document.clone());
            set_expando(
                node,
                "previousSibling",
                index
                    .checked_sub(1)
                    .and_then(|previous| children.get(previous))
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            set_expando(
                node,
                "nextSibling",
                children.get(index + 1).cloned().unwrap_or(Value::Null),
            );
        }
    }
}

pub(crate) fn set_document_compat_mode(quirks: bool) {
    document_value().set_property(
        "compatMode",
        Value::string(if quirks { "BackCompat" } else { "CSS1Compat" }),
    );
}

pub(crate) fn set_document_content_type(content_type: &str) {
    let document = document_value();
    document.set_property("contentType", Value::string(content_type));
    w3cos_core::class::set_prototype_of(
        &document,
        &crate::dom_constructors::prototype(if content_type == "text/html" {
            "HTMLDocument"
        } else {
            "XMLDocument"
        }),
    );
}

fn legacy_element_factory_class(name: &'static str) -> Value {
    LEGACY_ELEMENT_FACTORY_CLASSES.with(|slot| {
        if let Some(class) = slot
            .borrow()
            .as_ref()
            .and_then(|classes| classes.get(name))
            .cloned()
        {
            return class;
        }
        let class = match name {
            "Image" => Value::function(|_, args| {
                let image = element_value(dom::create_element("img"));
                if let Some(width) = args.first() {
                    image.set_property("width", Value::Number(width.to_u32() as f64));
                }
                if let Some(height) = args.get(1) {
                    image.set_property("height", Value::Number(height.to_u32() as f64));
                }
                image
            }),
            "Audio" => Value::function(|_, args| {
                let audio = element_value(dom::create_element("audio"));
                audio.set_property("preload", Value::string("auto"));
                if let Some(src) = args.first() {
                    audio.set_property("src", Value::string(&src.to_js_string()));
                }
                audio
            }),
            "Option" => Value::function(|_, args| {
                let option = element_value(dom::create_element("option"));
                let text = args.first().map(Value::to_js_string).unwrap_or_default();
                if !text.is_empty() {
                    dom::append_child(
                        node_id_of(&option).unwrap_or_default(),
                        dom::create_text_node(&text),
                    );
                }
                option.set_property(
                    "value",
                    Value::string(
                        &args
                            .get(1)
                            .map(Value::to_js_string)
                            .unwrap_or_else(|| text.clone()),
                    ),
                );
                let default_selected = args.get(2).is_some_and(Value::to_bool);
                option.set_property("defaultSelected", Value::Bool(default_selected));
                if default_selected {
                    dom::set_attribute(node_id_of(&option).unwrap_or_default(), "selected", "");
                }
                option.set_property(
                    "selected",
                    Value::Bool(args.get(3).map(Value::to_bool).unwrap_or(default_selected)),
                );
                option
            }),
            _ => unreachable!("unknown legacy element factory"),
        };
        class.set_property("name", Value::string(name));
        let prototype_name = match name {
            "Image" => "HTMLImageElement",
            "Audio" => "HTMLAudioElement",
            "Option" => "HTMLOptionElement",
            _ => unreachable!("unknown legacy element factory"),
        };
        class.set_property(
            "prototype",
            crate::dom_constructors::prototype(prototype_name),
        );
        let mut classes = slot.borrow_mut();
        classes
            .get_or_insert_with(HashMap::new)
            .insert(name.to_string(), class.clone());
        class
    })
}

fn bar_prop_class() -> Value {
    BAR_PROP_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("Illegal constructor: BarProp")],
            ))
        });
        class.set_property("name", Value::string("BarProp"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("visible", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn bar_prop_value() -> Value {
    let value = Value::object(HashMap::from([("visible".to_string(), Value::Bool(false))]));
    w3cos_core::class::set_prototype_of(&value, &bar_prop_class().get_property("prototype"));
    value
}

fn build_element_value(node: u32) -> Value {
    let generation = realm_generation();
    let mut props = HashMap::new();
    props.insert("__node_id".to_string(), Value::Number(node as f64));
    props.insert(
        "__w3cos_realm_generation".to_string(),
        Value::Number(generation as f64),
    );

    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
            if !bridge_realm_is_current(generation) {
                return Value::Undefined;
            }
            // 1. JS expandos / bridge-cached sub-objects (style, classList).
            if let Some(v) = get_expando(node, key) {
                return v;
            }
            // 2. Stored props on the target snapshot (e.g. __node_id).
            let stored = target.get_property(key);
            if !stored.is_undefined() {
                return stored;
            }
            // 3. Computed DOM surface.
            element_computed_get(node, key)
        })
        .set(move |_target, key, value, _receiver| {
            if bridge_realm_is_current(generation) {
                element_computed_set(node, key, value)
            } else {
                // The old Realm is already gone. Accept the write as an inert
                // operation so strict-mode callers cannot mutate the new DOM.
                true
            }
        })
        .build();

    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(props, handler))));
    let namespace_value = get_expando(node, "namespaceURI");
    let has_explicit_namespace = namespace_value.is_some();
    let namespace = namespace_value
        .and_then(|namespace| (!namespace.is_null()).then(|| namespace.to_js_string()));
    let local_name = get_expando(node, "localName")
        .map(|name| name.to_js_string())
        .unwrap_or_else(|| dom::tag_name(node));
    if namespace.as_deref() == Some("http://www.w3.org/1998/Math/MathML") {
        w3cos_core::class::set_prototype_of(
            &value,
            &math_ml_element_class().get_property("prototype"),
        );
    } else if namespace.as_deref() == Some(crate::html_parser_state::HTML_NAMESPACE)
        && local_name.bytes().any(|byte| byte.is_ascii_uppercase())
    {
        // Namespace-aware creation preserves ASCII case. An upper-case local
        // name therefore does not identify the lower-case built-in HTML tag.
        w3cos_core::class::set_prototype_of(
            &value,
            &crate::dom_constructors::prototype("HTMLUnknownElement"),
        );
    } else if has_explicit_namespace
        && namespace.as_deref() != Some(crate::html_parser_state::HTML_NAMESPACE)
        && namespace.as_deref() != Some("http://www.w3.org/2000/svg")
    {
        w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("Element"));
    } else {
        w3cos_core::class::set_prototype_of(
            &value,
            &crate::dom_constructors::prototype_for_node(
                dom::node_type(node),
                &local_name,
                namespace.as_deref() == Some("http://www.w3.org/2000/svg"),
            ),
        );
    }
    value
}

fn child_elements(node: u32) -> Vec<u32> {
    dom::children(node)
        .into_iter()
        .filter(|&c| dom::node_type(c) == 1)
        .collect()
}

fn child_elements_with_tags(node: u32, tags: &[&str]) -> Vec<u32> {
    child_elements(node)
        .into_iter()
        .filter(|child| tags.contains(&dom::tag_name(*child).as_str()))
        .collect()
}

fn table_rows(table: u32) -> Vec<u32> {
    descendant_elements(table)
        .into_iter()
        .filter(|candidate| {
            dom::tag_name(*candidate) == "tr"
                && std::iter::successors(dom::parent_node(*candidate), |parent| {
                    dom::parent_node(*parent)
                })
                .find(|ancestor| dom::tag_name(*ancestor) == "table")
                    == Some(table)
        })
        .collect()
}

fn remove_indexed_table_row(rows: Vec<u32>, requested_index: Value) {
    let index = requested_index.to_number() as i64;
    let resolved = if index == -1 {
        rows.len().checked_sub(1)
    } else {
        usize::try_from(index)
            .ok()
            .filter(|index| *index < rows.len())
    };
    let Some(index) = resolved else {
        if index == -1 && rows.is_empty() {
            return;
        }
        dom_exception("The row index is outside the collection", "IndexSizeError");
    };
    let row = rows[index];
    let Some(parent) = dom::parent_node(row) else {
        return;
    };
    let value = element_value(row);
    let was_connected = dom::is_connected(row);
    dom::remove_child(parent, row);
    release_element_subtree(row);
    if was_connected {
        crate::custom_elements_web::disconnected_subtree(&value);
    }
}

fn first_element_child(node: u32) -> Option<u32> {
    child_elements(node).into_iter().next()
}

fn sibling_element(node: u32, next: bool) -> Option<u32> {
    let mut cur = if next {
        dom::next_sibling(node)
    } else {
        dom::previous_sibling(node)
    };
    while let Some(id) = cur {
        if dom::node_type(id) == 1 {
            return Some(id);
        }
        cur = if next {
            dom::next_sibling(id)
        } else {
            dom::previous_sibling(id)
        };
    }
    None
}

fn clear_children(node: u32) {
    for c in dom::children(node) {
        dom::remove_child(node, c);
        release_element_subtree(c);
    }
}

fn with_deferred_dom_post_insertion_steps<T>(operation: impl FnOnce() -> T) -> T {
    #[cfg(feature = "dynamic-js")]
    {
        crate::dynamic_script::with_dom_post_insertion_steps_suppressed(operation)
    }
    #[cfg(not(feature = "dynamic-js"))]
    {
        operation()
    }
}

fn run_dom_post_insertion_steps(node: u32) {
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::notify_node_inserted(node);
    #[cfg(not(feature = "dynamic-js"))]
    let _ = node;
}

fn run_dom_batch_insertion_steps(nodes: &[u32]) {
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::prepare_inserted_stylesheets(nodes);
    #[cfg(not(feature = "dynamic-js"))]
    let _ = nodes;
}

fn run_script_mutation_steps(node: u32) {
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::notify_script_mutated(node);
    #[cfg(not(feature = "dynamic-js"))]
    let _ = node;
}

pub(crate) fn run_media_source_insertion_steps(parent: u32, child: u32) {
    if matches!(dom::tag_name(parent).as_str(), "audio" | "video")
        && dom::tag_name(child).eq_ignore_ascii_case("source")
        && !dom::has_attribute(child, "src")
    {
        // The media resource selection algorithm runs during insertion,
        // before post-insertion steps execute an earlier sibling script.
        set_expando(parent, "networkState", Value::Number(3.0));
    }
}

fn insert_child_or_fragment(parent: u32, child: u32, reference: Option<u32>) {
    ensure_tree_insertion(parent, child);
    if dom::node_type(child) != 11 {
        preserve_moved_option_selectedness(child);
        blur_focus_for_standard_reparent(child);
        match reference {
            Some(reference) => dom::insert_before(parent, child, reference),
            None => dom::append_child(parent, child),
        }
        pin_element_subtree(child);
        if dom::is_connected(child) {
            crate::custom_elements_web::connected_subtree(&element_value(child));
        }
        return;
    }

    let added = dom::children(child);
    if added.is_empty() {
        return;
    }
    for node in &added {
        ensure_tree_insertion(parent, *node);
    }
    for node in &added {
        blur_focus_for_standard_reparent(*node);
    }
    let previous = reference.and_then(dom::previous_sibling).or_else(|| {
        reference
            .is_none()
            .then(|| dom::last_child(parent))
            .flatten()
    });
    with_deferred_dom_post_insertion_steps(|| {
        crate::observers_web::with_mutation_notifications_suppressed(|| {
            for node in &added {
                match reference {
                    Some(reference) => dom::insert_before(parent, *node, reference),
                    None => dom::append_child(parent, *node),
                }
            }
        });
    });
    crate::observers_web::notify_child_list(child, &[], &added, None, None);
    crate::observers_web::notify_child_list(parent, &added, &[], previous, reference);
    for node in &added {
        pin_element_subtree(*node);
        if dom::is_connected(*node) {
            crate::custom_elements_web::connected_subtree(&element_value(*node));
        }
    }
    run_dom_batch_insertion_steps(&added);
    run_script_mutation_steps(parent);
    for node in added {
        run_dom_post_insertion_steps(node);
    }
}

fn replace_child_or_fragment(parent: u32, child: u32, old_child: u32) {
    if dom::node_type(child) != 11 {
        dom::replace_child(parent, child, old_child);
        pin_element_subtree(child);
        return;
    }

    let added = dom::children(child);
    for node in &added {
        ensure_tree_insertion(parent, *node);
    }
    let previous = dom::previous_sibling(old_child);
    let next = dom::next_sibling(old_child);
    with_deferred_dom_post_insertion_steps(|| {
        crate::observers_web::with_mutation_notifications_suppressed(|| {
            for node in &added {
                dom::insert_before(parent, *node, old_child);
            }
            dom::remove_child(parent, old_child);
        });
    });
    if !added.is_empty() {
        crate::observers_web::notify_child_list(child, &[], &added, None, None);
    }
    crate::observers_web::notify_child_list(parent, &added, &[old_child], previous, next);
    for node in &added {
        pin_element_subtree(*node);
        if dom::is_connected(*node) {
            crate::custom_elements_web::connected_subtree(&element_value(*node));
        }
    }
    run_dom_batch_insertion_steps(&added);
    run_script_mutation_steps(parent);
    for node in added {
        run_dom_post_insertion_steps(node);
    }
}

fn replace_all_children(node: u32, replacement: impl FnOnce()) {
    let removed = dom::children(node);
    crate::observers_web::with_mutation_notifications_suppressed(|| {
        clear_children(node);
        replacement();
    });
    let added = dom::children(node);
    if !added.is_empty() || !removed.is_empty() {
        crate::observers_web::notify_child_list(node, &added, &removed, None, None);
    }
    run_dom_batch_insertion_steps(&added);
    run_script_mutation_steps(node);
    for added_node in added {
        run_dom_post_insertion_steps(added_node);
    }
}

pub(crate) fn decode_html_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        output.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.find(';') else {
            output.push_str(rest);
            return output;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => {
                entity[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        if let Some(ch) = decoded {
            output.push(ch);
        } else {
            output.push_str(&rest[..=semi]);
        }
        rest = &rest[semi + 1..];
    }
    output.push_str(rest);
    output
}

pub(crate) fn html_tag_end(input: &str) -> Option<usize> {
    let mut quote = None;
    for (index, ch) in input.char_indices() {
        match (quote, ch) {
            (Some(active), current) if current == active => quote = None,
            (None, '\'' | '"') => quote = Some(ch),
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

pub(crate) fn parse_html_attributes(mut input: &str) -> Vec<(String, String)> {
    let mut attributes = Vec::new();
    while !input.trim_start().is_empty() {
        input = input.trim_start();
        let name_end = input
            .find(|ch: char| ch.is_whitespace() || ch == '=')
            .unwrap_or(input.len());
        if name_end == 0 {
            break;
        }
        let name = input[..name_end].to_ascii_lowercase();
        input = &input[name_end..];
        input = input.trim_start();
        let mut value = String::new();
        if let Some(after_equals) = input.strip_prefix('=') {
            input = after_equals.trim_start();
            if let Some(quote @ ('\'' | '"')) = input.chars().next() {
                input = &input[quote.len_utf8()..];
                if let Some(end) = input.find(quote) {
                    value = decode_html_entities(&input[..end]);
                    input = &input[end + quote.len_utf8()..];
                } else {
                    value = decode_html_entities(input);
                    input = "";
                }
            } else {
                let end = input.find(char::is_whitespace).unwrap_or(input.len());
                value = decode_html_entities(&input[..end]);
                input = &input[end..];
            }
        }
        attributes.push((name, value));
    }
    attributes
}

pub(crate) fn apply_html_attribute(node: u32, name: &str, value: &str) {
    match name {
        "class" => dom::set_class_name(node, value),
        "style" => {
            dom::set_attribute(node, name, value);
            parse_css_text(node, value);
        }
        _ => dom::set_attribute(node, name, value),
    }
}

pub(crate) fn apply_html_attribute_ns(
    node: u32,
    namespace: Option<&str>,
    qualified_name: &str,
    prefix: Option<&str>,
    local_name: &str,
    value: &str,
) {
    if namespace.is_none() {
        apply_html_attribute(node, qualified_name, value);
    } else {
        dom::set_attribute_ns_parts(node, namespace, qualified_name, prefix, local_name, value);
    }
}

/// Parse a trusted HTML fragment into real DOM nodes. Monaco's view layer
/// renders visible lines through `innerHTML`, so treating markup as a text
/// node leaves an otherwise healthy editor visually empty.
fn append_html_fragment(parent: u32, html: &str) {
    append_html_fragment_mode(parent, html, false);
}

pub(crate) fn append_sanitized_html_fragment(parent: u32, html: &str) {
    append_html_fragment_mode(parent, html, true);
}

fn append_html_fragment_mode(parent: u32, html: &str, sanitize: bool) {
    let existing_children = dom::children(parent).into_iter().collect::<HashSet<_>>();
    match crate::html_tree_builder::append_html_fragment_with_streaming_parser(
        parent, html, sanitize,
    ) {
        Ok(()) => {
            for child in dom::children(parent) {
                if !existing_children.contains(&child) {
                    crate::custom_elements_web::upgrade_subtree(&element_value(child));
                }
            }
        }
        Err(error) => {
            eprintln!("[w3cos] inert HTML fragment parse failed: {error}");
        }
    }
}

fn descendant_elements(root: u32) -> Vec<u32> {
    let mut output = Vec::new();
    let mut pending = dom::children(root).into_iter().rev().collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        if dom::node_type(node) == 1 {
            output.push(node);
        }
        pending.extend(dom::children(node).into_iter().rev());
    }
    output
}

fn inclusive_descendant_elements(root: u32) -> Vec<u32> {
    let mut output = Vec::new();
    if dom::node_type(root) == 1 {
        output.push(root);
    }
    output.extend(descendant_elements(root));
    output
}

fn exposes_window_name(node: u32) -> bool {
    matches!(
        dom::tag_name(node).as_str(),
        "embed" | "form" | "img" | "object"
    )
}

/// Resolve the named properties exposed by the active Window. The HTML
/// named-access algorithm is live: adding, removing, or renaming a connected
/// element must be observable by a later classic-script identifier lookup.
pub(crate) fn window_named_property(name: &str) -> Option<Value> {
    if name.is_empty() {
        return None;
    }
    inclusive_descendant_elements(document_element_id())
        .into_iter()
        .find(|node| {
            dom::get_attribute(*node, "id").as_deref() == Some(name)
                || (exposes_window_name(*node)
                    && dom::get_attribute(*node, "name").as_deref() == Some(name))
        })
        .map(element_value)
}

fn virtual_document_elements(document: &Value) -> Vec<u32> {
    virtual_document_children(document)
        .into_iter()
        .filter_map(|child| node_id_of(&child))
        .flat_map(inclusive_descendant_elements)
        .collect()
}

fn normalized_namespace(value: &Value) -> Option<String> {
    if value.is_null() || value.is_undefined() || value.to_js_string().is_empty() {
        None
    } else {
        Some(value.to_js_string())
    }
}

fn valid_element_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if first.is_ascii_alphabetic() {
        return characters.all(|character| {
            character != '\0'
                && !matches!(character, '\t' | '\n' | '\u{000c}' | '\r' | ' ' | '/' | '>')
        });
    }
    (matches!(first, ':' | '_') || !first.is_ascii())
        && characters.all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '.' | ':' | '_')
                || !character.is_ascii()
        })
}

fn valid_namespace_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix.chars().all(|character| {
            character != '\0'
                && !matches!(
                    character,
                    '\t' | '\n' | '\u{000c}' | '\r' | ' ' | '/' | '>' | ':'
                )
        })
}

fn validate_and_extract_qualified_name(
    namespace: Option<&str>,
    qualified_name: &str,
) -> (Option<String>, String) {
    let (prefix, local_name) = qualified_name
        .split_once(':')
        .map_or((None, qualified_name), |(prefix, local_name)| {
            (Some(prefix), local_name)
        });
    if !valid_element_name(local_name)
        || prefix.is_some_and(|prefix| !valid_namespace_prefix(prefix))
    {
        dom_exception(
            "The qualified name is not a valid element name",
            "InvalidCharacterError",
        );
    }

    const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
    const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";
    if prefix.is_some() && namespace.is_none()
        || prefix == Some("xml") && namespace != Some(XML_NAMESPACE)
        || (qualified_name == "xmlns" || prefix == Some("xmlns"))
            && namespace != Some(XMLNS_NAMESPACE)
        || namespace == Some(XMLNS_NAMESPACE)
            && qualified_name != "xmlns"
            && prefix != Some("xmlns")
    {
        dom_exception(
            "The qualified name is not valid for the supplied namespace",
            "NamespaceError",
        );
    }
    (prefix.map(str::to_string), local_name.to_string())
}

fn html_class_tokens(value: &str) -> Vec<&str> {
    value
        .split([' ', '\t', '\n', '\r', '\x0c'])
        .filter(|token| !token.is_empty())
        .collect()
}

fn element_matches_class_names(node: u32, requested_names: &str) -> bool {
    let requested_names = html_class_tokens(requested_names);
    if requested_names.is_empty() {
        return false;
    }
    let classes = dom::class_name(node);
    let classes = html_class_tokens(&classes);
    let quirks_mode = get_expando(node, "ownerDocument")
        .unwrap_or_else(document_value)
        .get_property("compatMode")
        .to_js_string()
        == "BackCompat";
    requested_names.into_iter().all(|requested| {
        classes.iter().any(|class| {
            if quirks_mode {
                class.eq_ignore_ascii_case(requested)
            } else {
                class == &requested
            }
        })
    })
}

fn element_matches_tag_name(node: u32, requested_name: &str, html_document: bool) -> bool {
    if requested_name == "*" {
        return true;
    }
    if namespace_uri(node) == crate::html_parser_state::HTML_NAMESPACE && html_document {
        dom::tag_name(node) == requested_name.to_ascii_lowercase()
    } else {
        dom::tag_name(node) == requested_name
    }
}

fn element_matches_namespace(node: u32, namespace: &Option<String>, local_name: &str) -> bool {
    let namespace_matches = namespace.as_deref() == Some("*")
        || match namespace {
            Some(namespace) => namespace_uri(node) == *namespace,
            None => get_expando(node, "namespaceURI").is_some_and(|namespace| namespace.is_null()),
        };
    let qualified_name = dom::tag_name(node);
    let node_local_name = qualified_name
        .rsplit_once(':')
        .map_or(qualified_name.as_str(), |(_, local_name)| local_name);
    namespace_matches && (local_name == "*" || node_local_name == local_name)
}

fn document_node_value(mut props: HashMap<String, Value>) -> Value {
    // Node.nodeValue and Node.textContent are writable-but-inert on Document.
    // Use the runtime's accessor convention so ordinary mutable document
    // properties (doctype, contentType, title, ...) retain their live storage.
    for name in ["nodeValue", "textContent"] {
        props.insert(format!("__w3cos_getter_{name}"), func(|_, _| Value::Null));
        props.insert(
            format!("__w3cos_setter_{name}"),
            func(|_, _| Value::Undefined),
        );
    }
    for name in [
        "nextSibling",
        "previousSibling",
        "ownerDocument",
        "parentNode",
        "parentElement",
    ] {
        props.entry(name.to_string()).or_insert(Value::Null);
    }
    props
        .entry("isConnected".to_string())
        .or_insert(Value::Bool(true));
    let character_set = props
        .get("characterSet")
        .cloned()
        .unwrap_or_else(|| Value::string("UTF-8"));
    props
        .entry("charset".to_string())
        .or_insert_with(|| character_set.clone());
    props
        .entry("inputEncoding".to_string())
        .or_insert(character_set);
    props.remove("childNodes");
    props
        .entry("__w3cos_getter_childNodes".to_string())
        .or_insert_with(|| func(|document, _| virtual_document_child_nodes(document)));
    props.entry("cloneNode".to_string()).or_insert_with(|| {
        func(|document, args| clone_document_node(&document, arg(&args, 0).to_bool()))
    });
    props
        .entry("lookupNamespaceURI".to_string())
        .or_insert_with(|| {
            func(|document, args| lookup_namespace_uri_result(&document, &arg(&args, 0)))
        });
    props
        .entry("lookupPrefix".to_string())
        .or_insert_with(|| func(|document, args| lookup_prefix_result(&document, &arg(&args, 0))));
    props
        .entry("isDefaultNamespace".to_string())
        .or_insert_with(|| {
            func(|document, args| is_default_namespace_result(&document, &arg(&args, 0)))
        });
    props.entry("isEqualNode".to_string()).or_insert_with(|| {
        func(|document, args| Value::Bool(nodes_are_equal(&document, &arg(&args, 0))))
    });
    props
        .entry("replaceChildren".to_string())
        .or_insert_with(|| {
            func(|document, args| {
                replace_virtual_document_children(&document, args);
                Value::Undefined
            })
        });
    Value::object(props)
}

fn clone_document_node(document: &Value, deep: bool) -> Value {
    let prototype_name = document
        .get_property("__w3cos_document_prototype")
        .to_js_string();
    let clone = empty_document_value(
        &document.get_property("contentType").to_js_string(),
        &prototype_name,
    );
    if deep {
        let children = virtual_document_children(document)
            .into_iter()
            .filter_map(|child| node_id_of(&child))
            .map(|child| {
                let cloned_child = dom::clone_node(child, true);
                copy_cloned_node_identity(child, cloned_child);
                walk_subtree(cloned_child, &mut |descendant| {
                    set_expando(descendant, "ownerDocument", clone.clone())
                });
                element_value(cloned_child)
            })
            .collect();
        set_virtual_document_children(&clone, children);
    }
    clone
}

fn parsed_document_value(
    root: u32,
    content_type: &str,
    head: Option<u32>,
    body: Option<u32>,
) -> Value {
    let implementation = dom_implementation_value();
    let html_document = content_type == "text/html";
    let mut props = HashMap::from([
        ("nodeType".to_string(), Value::Number(9.0)),
        ("nodeName".to_string(), Value::string("#document")),
        ("contentType".to_string(), Value::string(content_type)),
        ("URL".to_string(), Value::string("about:blank")),
        ("documentURI".to_string(), Value::string("about:blank")),
        ("compatMode".to_string(), Value::string("CSS1Compat")),
        ("characterSet".to_string(), Value::string("UTF-8")),
        ("charset".to_string(), Value::string("UTF-8")),
        ("inputEncoding".to_string(), Value::string("UTF-8")),
        ("title".to_string(), Value::string("")),
        ("location".to_string(), Value::Null),
        ("defaultView".to_string(), Value::Null),
        ("documentElement".to_string(), element_value(root)),
        (
            "head".to_string(),
            head.map(element_value).unwrap_or(Value::Null),
        ),
        (
            "body".to_string(),
            body.map(element_value).unwrap_or(Value::Null),
        ),
        (
            "childNodes".to_string(),
            node_list(vec![element_value(root)]),
        ),
        (
            "__w3cos_document_prototype".to_string(),
            Value::string(if content_type == "text/html" {
                "HTMLDocument"
            } else {
                "XMLDocument"
            }),
        ),
        (
            "__w3cos_document_root".to_string(),
            Value::Number(root as f64),
        ),
        (
            "__w3cos_document_children".to_string(),
            js_array(vec![element_value(root)]),
        ),
    ]);
    props.insert(
        "querySelector".to_string(),
        func(move |_, args| {
            let selector = query_selector_argument(&args, root);
            if query_selector_matches(root, &selector, Some(root)) {
                return element_value(root);
            }
            element_or_null(
                query_selector_all_scoped(Some(root), &selector)
                    .into_iter()
                    .next(),
            )
        }),
    );
    props.insert(
        "querySelectorAll".to_string(),
        func(move |_, args| {
            let selector = query_selector_argument(&args, root);
            let mut matches = Vec::new();
            if query_selector_matches(root, &selector, Some(root)) {
                matches.push(root);
            }
            matches.extend(query_selector_all_scoped(Some(root), &selector));
            node_list(matches.into_iter().map(element_value).collect())
        }),
    );
    props.insert(
        "getElementById".to_string(),
        func(move |_, args| {
            let id = arg(&args, 0).to_js_string();
            let found = inclusive_descendant_elements(root)
                .into_iter()
                .find(|node| dom::get_attribute(*node, "id").as_deref() == Some(&id));
            element_or_null(found)
        }),
    );
    props.insert(
        "getElementsByTagName".to_string(),
        func(move |_, args| {
            let tag = arg(&args, 0).to_js_string();
            html_collection(move || {
                inclusive_descendant_elements(root)
                    .into_iter()
                    .filter(|node| element_matches_tag_name(*node, &tag, html_document))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "getElementsByTagNameNS".to_string(),
        func(move |_, args| {
            let namespace = normalized_namespace(&arg(&args, 0));
            let local_name = arg(&args, 1).to_js_string();
            html_collection(move || {
                inclusive_descendant_elements(root)
                    .into_iter()
                    .filter(|node| element_matches_namespace(*node, &namespace, &local_name))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "getElementsByClassName".to_string(),
        func(move |_, args| {
            let class_names = arg(&args, 0).to_js_string();
            html_collection(move || {
                inclusive_descendant_elements(root)
                    .into_iter()
                    .filter(|node| element_matches_class_names(*node, &class_names))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "createElement".to_string(),
        func({
            let is_html = content_type == "text/html";
            let element_namespace = match content_type {
                "text/html" => None,
                "application/xhtml+xml" => Some(crate::html_parser_state::HTML_NAMESPACE),
                _ => Some(""),
            };
            move |document, args| {
                let mut tag = arg(&args, 0).to_js_string();
                if !valid_element_name(&tag) {
                    dom_exception("The element name is not valid", "InvalidCharacterError");
                }
                if is_html {
                    tag.make_ascii_lowercase();
                }
                let node = dom::create_element(&tag);
                if let Some(namespace) = element_namespace {
                    dom::set_html_element(
                        node,
                        namespace == crate::html_parser_state::HTML_NAMESPACE,
                    );
                    set_expando(
                        node,
                        "namespaceURI",
                        if namespace.is_empty() {
                            Value::Null
                        } else {
                            Value::string(namespace)
                        },
                    );
                }
                set_expando(node, "ownerDocument", document);
                element_value(node)
            }
        }),
    );
    props.insert(
        "createElementNS".to_string(),
        func(|document, args| {
            let namespace = normalized_namespace(&arg(&args, 0));
            let qualified_name = arg(&args, 1).to_js_string();
            validate_and_extract_qualified_name(namespace.as_deref(), &qualified_name);
            let node =
                create_namespaced_element(namespace.as_deref().unwrap_or(""), &qualified_name);
            set_expando(node, "ownerDocument", document);
            let element = element_value(node);
            if namespace.as_deref() == Some("http://www.w3.org/1998/Math/MathML") {
                w3cos_core::class::set_prototype_of(
                    &element,
                    &math_ml_element_class().get_property("prototype"),
                );
            }
            element
        }),
    );
    props.insert(
        "createTextNode".to_string(),
        func(|document, args| {
            let node = dom::create_text_node(&arg(&args, 0).to_js_string());
            set_expando(node, "ownerDocument", document);
            element_value(node)
        }),
    );
    props.insert(
        "createComment".to_string(),
        func(|document, args| {
            let node = dom::create_comment(&arg(&args, 0).to_js_string());
            set_expando(node, "ownerDocument", document);
            element_value(node)
        }),
    );
    props.insert(
        "createCDATASection".to_string(),
        func(|document, args| create_cdata_section_value(&document, &arg(&args, 0).to_js_string())),
    );
    props.insert(
        "createProcessingInstruction".to_string(),
        func(|document, args| {
            let instruction = create_processing_instruction_value(
                &arg(&args, 0).to_js_string(),
                &arg(&args, 1).to_js_string(),
            );
            if let Some(node) = node_id_of(&instruction) {
                set_expando(node, "ownerDocument", document);
            }
            instruction
        }),
    );
    props.insert(
        "createDocumentFragment".to_string(),
        func(|document, _| {
            let node = dom::create_document_fragment();
            set_expando(node, "ownerDocument", document);
            element_value(node)
        }),
    );
    props.insert(
        "createAttribute".to_string(),
        func({
            let is_html = content_type == "text/html";
            move |document, args| detached_attribute_value(&document, arg(&args, 0), is_html)
        }),
    );
    props.insert(
        "createAttributeNS".to_string(),
        func(|document, args| {
            detached_namespaced_attribute_value(&document, arg(&args, 0), arg(&args, 1))
        }),
    );
    props.insert(
        "isSameNode".to_string(),
        func(|document, args| Value::Bool(document.strict_eq(&arg(&args, 0)))),
    );
    props.insert(
        "isEqualNode".to_string(),
        func(|document, args| Value::Bool(nodes_are_equal(&document, &arg(&args, 0)))),
    );
    props.insert("getRootNode".to_string(), func(|document, _| document));
    props.insert(
        "normalize".to_string(),
        func(move |_, _| {
            normalize_node_subtree(root);
            Value::Undefined
        }),
    );
    props.insert("parentNode".to_string(), Value::Null);
    props.insert("parentElement".to_string(), Value::Null);
    props.insert(
        "adoptNode".to_string(),
        func(|document, args| adopt_node_into(&document, arg(&args, 0))),
    );
    props.insert(
        "importNode".to_string(),
        func(|document, args| import_node_into(&document, arg(&args, 0), arg(&args, 1).to_bool())),
    );
    for (name, prepend) in [("append", false), ("prepend", true)] {
        props.insert(
            name.to_string(),
            func(move |document, args| {
                validate_document_parent_node_insertion(&document, &args, prepend);
                insert_virtual_document_children(&document, args, prepend);
                Value::Undefined
            }),
        );
    }
    props.insert(
        "appendChild".to_string(),
        func(|document, args| {
            let child = arg(&args, 0);
            validate_document_parent_node_insertion(&document, std::slice::from_ref(&child), false);
            insert_virtual_document_children(&document, vec![child.clone()], false);
            child
        }),
    );
    props.insert(
        "createCDATASection".to_string(),
        func(|document, args| create_cdata_section_value(&document, &arg(&args, 0).to_js_string())),
    );
    props.insert(
        "createProcessingInstruction".to_string(),
        func(|_, args| {
            create_processing_instruction_value(
                &arg(&args, 0).to_js_string(),
                &arg(&args, 1).to_js_string(),
            )
        }),
    );
    props.insert("implementation".to_string(), implementation.clone());
    install_virtual_document_node_mutations(&mut props);
    let document = document_node_value(props);
    implementation.set_property("__w3cos_owner_document", document.clone());
    walk_subtree(root, &mut |node| {
        set_expando(node, "ownerDocument", document.clone())
    });
    set_virtual_document_children(&document, vec![element_value(root)]);
    w3cos_core::class::set_prototype_of(
        &document,
        &crate::dom_constructors::prototype(if content_type == "text/html" {
            "HTMLDocument"
        } else {
            "XMLDocument"
        }),
    );
    document
}

pub(crate) fn parse_frame_document(source: &str, content_type: &str, url: &str) -> Value {
    let document = if matches!(content_type, "text/css" | "text/plain")
        || (content_type.starts_with("image/") && content_type != "image/svg+xml")
    {
        empty_document_value(content_type, "Document")
    } else {
        parse_document(source, content_type)
    };
    document.set_property("URL", Value::string(url));
    document.set_property("documentURI", Value::string(url));
    document
}

pub(crate) fn set_document_encoding(document: &Value, encoding: &str) {
    let encoding = Value::string(encoding);
    document.set_property("characterSet", encoding.clone());
    document.set_property("charset", encoding.clone());
    document.set_property("inputEncoding", encoding);
}

pub(crate) fn install_frame_document(node: u32, document: Value, url: &str) {
    let parent_window = window_value();
    let parsed_url = url::Url::parse(url).ok();
    let location = Value::object(HashMap::from([
        ("href".to_string(), Value::string(url)),
        (
            "hash".to_string(),
            Value::string(
                &parsed_url
                    .as_ref()
                    .and_then(|url| url.fragment())
                    .map(|fragment| format!("#{fragment}"))
                    .unwrap_or_default(),
            ),
        ),
        (
            "origin".to_string(),
            Value::string(
                &parsed_url
                    .as_ref()
                    .map(|url| url.origin().ascii_serialization())
                    .unwrap_or_else(|| "null".to_string()),
            ),
        ),
    ]));
    let frame_window = Value::object(HashMap::from([
        ("document".to_string(), document.clone()),
        ("location".to_string(), location.clone()),
        (
            "TypeError".to_string(),
            parent_window.get_property("TypeError"),
        ),
        (
            "DOMException".to_string(),
            parent_window.get_property("DOMException"),
        ),
        (
            "NodeList".to_string(),
            parent_window.get_property("NodeList"),
        ),
    ]));
    for key in ["self", "window", "globalThis"] {
        frame_window.set_property(key, frame_window.clone());
    }
    for name in [
        "Error",
        "EvalError",
        "RangeError",
        "ReferenceError",
        "SyntaxError",
        "TypeError",
        "URIError",
        "AggregateError",
        "MutationObserver",
    ] {
        frame_window.set_property(name, parent_window.get_property(name));
    }
    frame_window.set_property(
        "Function",
        frame_function_compat_class(frame_window.clone(), document.clone()),
    );
    for name in crate::dom_constructors::DOM_CONSTRUCTOR_NAMES {
        frame_window.set_property(name, parent_window.get_property(name));
    }
    for (name, node_type) in [
        ("Text", 3_u16),
        ("Comment", 8_u16),
        ("DocumentFragment", 11_u16),
    ] {
        let owner_document = document.clone();
        let parent_constructor = parent_window.get_property(name);
        let constructor = Value::function(move |_, args| {
            let node = match node_type {
                3 => dom::create_text_node(
                    &args
                        .first()
                        .filter(|value| !value.is_undefined())
                        .map(Value::to_js_string)
                        .unwrap_or_default(),
                ),
                8 => dom::create_comment(
                    &args
                        .first()
                        .filter(|value| !value.is_undefined())
                        .map(Value::to_js_string)
                        .unwrap_or_default(),
                ),
                _ => dom::create_document_fragment(),
            };
            set_expando(node, "ownerDocument", owner_document.clone());
            element_value(node)
        });
        constructor.set_property("name", Value::string(name));
        constructor.set_property("prototype", parent_constructor.get_property("prototype"));
        frame_window.set_property(name, constructor);
    }
    frame_window.set_property("top", parent_window.clone());
    frame_window.set_property("parent", parent_window);
    document.set_property("defaultView", frame_window.clone());
    document.set_property("location", location);
    set_expando(node, "contentDocument", document);
    set_expando(node, "contentWindow", frame_window);
}

fn ensure_frame_browsing_context(node: u32) {
    if get_expando(node, "contentDocument").is_some() || !node_is_connected(node) {
        return;
    }
    let document = parse_frame_document("", "text/html", "about:blank");
    install_frame_document(node, document, "about:blank");
}

fn frame_windows_value() -> Value {
    js_array(
        dom::get_elements_by_tag_name("iframe")
            .into_iter()
            .filter(|node| dom::is_connected(*node))
            .filter_map(|node| {
                ensure_frame_browsing_context(node);
                get_expando(node, "contentWindow")
            })
            .collect(),
    )
}

pub(crate) fn graft_frame_component_subtrees(component: &mut w3cos_std::Component) {
    let host_node = match &component.on_click {
        w3cos_std::EventAction::NativeHost { id, .. } => u32::try_from(*id).ok(),
        _ => None,
    };
    if let Some(frame_node) = host_node.filter(|node| dom::tag_name(*node) == "iframe") {
        let frame_body = get_expando(frame_node, "contentDocument")
            .map(|document| document.get_property("body"))
            .and_then(|body| node_id_of(&body));
        if let Some(frame_body) = frame_body {
            let frame = dom::with_document(|document| {
                document.to_component_subtree(NodeId::from_u32(frame_body))
            });
            component.children = vec![frame];
        }
    }
    for child in &mut component.children {
        graft_frame_component_subtrees(child);
    }
}

fn assigned_nodes_for_slot_node(slot: u32) -> Vec<u32> {
    let root = tree_root(slot);
    let Some(host) = shadow_host_for_root(root) else {
        return Vec::new();
    };
    let slot_name = dom::get_attribute(slot, "name").unwrap_or_default();
    dom::children(host)
        .into_iter()
        .filter(|child| {
            dom::get_attribute(*child, "slot").unwrap_or_default() == slot_name
                && matches!(dom::node_type(*child), 1 | 3)
        })
        .collect()
}

fn assigned_slot_for_node(node: u32) -> Option<u32> {
    let host = dom::parent_node(node)?;
    let root = shadow_root_id_for_host(host)?;
    let requested_name = dom::get_attribute(node, "slot").unwrap_or_default();
    descendant_elements(root).into_iter().find(|candidate| {
        dom::tag_name(*candidate) == "slot"
            && dom::get_attribute(*candidate, "name").unwrap_or_default() == requested_name
    })
}

fn affected_slots_for_node_position(node: u32) -> HashSet<u32> {
    let mut slots = HashSet::new();
    if dom::tag_name(node) == "slot" {
        slots.insert(node);
    }
    let Some(parent) = dom::parent_node(node) else {
        return slots;
    };
    if dom::tag_name(parent) == "slot" {
        slots.insert(parent);
    }
    if let Some(root) = shadow_root_id_for_host(parent) {
        let requested_name = dom::get_attribute(node, "slot").unwrap_or_default();
        slots.extend(descendant_elements(root).into_iter().filter(|candidate| {
            dom::tag_name(*candidate) == "slot"
                && dom::get_attribute(*candidate, "name").unwrap_or_default() == requested_name
        }));
    }
    slots
}

fn queue_slotchange(slot: u32) {
    let target = element_value(slot);
    queue_microtask_value(Value::function(move |_, _| {
        let event = w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![Value::string("slotchange")],
        );
        target.call_method("dispatchEvent", vec![event]);
        Value::Undefined
    }));
}

fn compose_shadow_slots(component: &mut w3cos_std::Component) {
    let node = match &component.on_click {
        w3cos_std::EventAction::NativeHost { id, .. } => u32::try_from(*id).ok(),
        _ => None,
    };
    if let Some(slot) = node.filter(|node| dom::tag_name(*node) == "slot") {
        let assigned = assigned_nodes_for_slot_node(slot);
        if !assigned.is_empty() {
            component.children = assigned
                .into_iter()
                .map(|node| {
                    dom::with_document(|document| {
                        document.to_component_subtree(NodeId::from_u32(node))
                    })
                })
                .collect();
        }
    }
    for child in &mut component.children {
        compose_shadow_slots(child);
    }
}

pub(crate) fn graft_shadow_component_subtrees(component: &mut w3cos_std::Component) {
    let host = match &component.on_click {
        w3cos_std::EventAction::NativeHost { id, .. } => u32::try_from(*id).ok(),
        _ => None,
    };
    if let Some(root) = host.and_then(shadow_root_id_for_host) {
        let mut shadow = dom::with_document(|document| {
            document.to_component_subtree(NodeId::from_u32(root))
        });
        compose_shadow_slots(&mut shadow);
        component.children = shadow.children;
    }
    for child in &mut component.children {
        graft_shadow_component_subtrees(child);
    }
}

pub(crate) fn empty_document_value(content_type: &str, prototype_name: &str) -> Value {
    let implementation = dom_implementation_value();
    let html_document = content_type == "text/html";
    let mut props = HashMap::from([
        ("nodeType".to_string(), Value::Number(9.0)),
        ("nodeName".to_string(), Value::string("#document")),
        ("contentType".to_string(), Value::string(content_type)),
        ("URL".to_string(), Value::string("about:blank")),
        ("documentURI".to_string(), Value::string("about:blank")),
        ("compatMode".to_string(), Value::string("CSS1Compat")),
        ("characterSet".to_string(), Value::string("UTF-8")),
        ("charset".to_string(), Value::string("UTF-8")),
        ("inputEncoding".to_string(), Value::string("UTF-8")),
        ("title".to_string(), Value::string("")),
        ("location".to_string(), Value::Null),
        ("defaultView".to_string(), Value::Null),
        ("doctype".to_string(), Value::Null),
        ("documentElement".to_string(), Value::Null),
        ("firstChild".to_string(), Value::Null),
        ("lastChild".to_string(), Value::Null),
        ("childNodes".to_string(), node_list(Vec::new())),
        ("implementation".to_string(), implementation.clone()),
        (
            "__w3cos_document_children".to_string(),
            js_array(Vec::new()),
        ),
        (
            "__w3cos_document_prototype".to_string(),
            Value::string(prototype_name),
        ),
    ]);
    props.insert(
        "createElement".to_string(),
        func(|document, args| {
            let tag = arg(&args, 0).to_js_string();
            if !valid_element_name(&tag) {
                dom_exception("The element name is not valid", "InvalidCharacterError");
            }
            let node = dom::create_element(&tag);
            dom::set_html_element(node, false);
            set_expando(node, "ownerDocument", document);
            set_expando(node, "namespaceURI", Value::Null);
            let element = element_value(node);
            w3cos_core::class::set_prototype_of(
                &element,
                &crate::dom_constructors::prototype("Element"),
            );
            element
        }),
    );
    props.insert(
        "createElementNS".to_string(),
        func(|document, args| {
            let namespace = normalized_namespace(&arg(&args, 0));
            let qualified_name = arg(&args, 1).to_js_string();
            validate_and_extract_qualified_name(namespace.as_deref(), &qualified_name);
            let node =
                create_namespaced_element(namespace.as_deref().unwrap_or(""), &qualified_name);
            set_expando(node, "ownerDocument", document);
            element_value(node)
        }),
    );
    props.insert(
        "createTextNode".to_string(),
        func(|document, args| {
            let node = dom::create_text_node(&arg(&args, 0).to_js_string());
            set_expando(node, "ownerDocument", document);
            element_value(node)
        }),
    );
    props.insert(
        "createComment".to_string(),
        func(|document, args| {
            let node = dom::create_comment(&arg(&args, 0).to_js_string());
            set_expando(node, "ownerDocument", document);
            element_value(node)
        }),
    );
    props.insert(
        "createCDATASection".to_string(),
        func(|document, args| create_cdata_section_value(&document, &arg(&args, 0).to_js_string())),
    );
    props.insert(
        "createProcessingInstruction".to_string(),
        func(|document, args| {
            let instruction = create_processing_instruction_value(
                &arg(&args, 0).to_js_string(),
                &arg(&args, 1).to_js_string(),
            );
            if let Some(node) = node_id_of(&instruction) {
                set_expando(node, "ownerDocument", document);
            }
            instruction
        }),
    );
    props.insert(
        "createDocumentFragment".to_string(),
        func(|document, _| {
            let node = dom::create_document_fragment();
            set_expando(node, "ownerDocument", document);
            element_value(node)
        }),
    );
    props.insert(
        "createAttribute".to_string(),
        func(|document, args| detached_attribute_value(&document, arg(&args, 0), false)),
    );
    props.insert(
        "createAttributeNS".to_string(),
        func(|document, args| {
            detached_namespaced_attribute_value(&document, arg(&args, 0), arg(&args, 1))
        }),
    );
    props.insert(
        "getElementById".to_string(),
        func(|document, args| {
            let id = arg(&args, 0).to_js_string();
            if id.is_empty() {
                return Value::Null;
            }
            element_or_null(
                virtual_document_elements(&document)
                    .into_iter()
                    .find(|node| dom::get_attribute(*node, "id").as_deref() == Some(&id)),
            )
        }),
    );
    props.insert(
        "getElementsByTagName".to_string(),
        func(move |document, args| {
            let tag = arg(&args, 0).to_js_string();
            let provider_document = document.clone();
            html_collection(move || {
                virtual_document_elements(&provider_document)
                    .into_iter()
                    .filter(|node| element_matches_tag_name(*node, &tag, html_document))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "getElementsByTagNameNS".to_string(),
        func(|document, args| {
            let namespace = normalized_namespace(&arg(&args, 0));
            let local_name = arg(&args, 1).to_js_string();
            let provider_document = document.clone();
            html_collection(move || {
                virtual_document_elements(&provider_document)
                    .into_iter()
                    .filter(|node| element_matches_namespace(*node, &namespace, &local_name))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "getElementsByClassName".to_string(),
        func(|document, args| {
            let class_names = arg(&args, 0).to_js_string();
            let provider_document = document.clone();
            html_collection(move || {
                virtual_document_elements(&provider_document)
                    .into_iter()
                    .filter(|node| element_matches_class_names(*node, &class_names))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "isSameNode".to_string(),
        func(|document, args| Value::Bool(document.strict_eq(&arg(&args, 0)))),
    );
    props.insert(
        "normalize".to_string(),
        func(|document, _| {
            for child in virtual_document_children(&document) {
                if let Some(node) = node_id_of(&child) {
                    normalize_node_subtree(node);
                }
            }
            Value::Undefined
        }),
    );
    props.insert("getRootNode".to_string(), func(|document, _| document));
    props.insert("parentNode".to_string(), Value::Null);
    props.insert("parentElement".to_string(), Value::Null);
    props.insert(
        "adoptNode".to_string(),
        func(|document, args| adopt_node_into(&document, arg(&args, 0))),
    );
    props.insert(
        "importNode".to_string(),
        func(|document, args| import_node_into(&document, arg(&args, 0), arg(&args, 1).to_bool())),
    );
    for (name, prepend) in [("append", false), ("prepend", true)] {
        props.insert(
            name.to_string(),
            func(move |document, args| {
                validate_document_parent_node_insertion(&document, &args, prepend);
                insert_virtual_document_children(&document, args, prepend);
                Value::Undefined
            }),
        );
    }
    props.insert(
        "appendChild".to_string(),
        func(|document, args| {
            let child = arg(&args, 0);
            validate_document_parent_node_insertion(&document, std::slice::from_ref(&child), false);
            insert_virtual_document_children(&document, vec![child.clone()], false);
            child
        }),
    );
    install_virtual_document_node_mutations(&mut props);
    let document = document_node_value(props);
    implementation.set_property("__w3cos_owner_document", document.clone());
    w3cos_core::class::set_prototype_of(
        &document,
        &crate::dom_constructors::prototype(prototype_name),
    );
    document
}

pub(crate) fn document_fragment_value() -> Value {
    element_value(dom::create_document_fragment())
}

pub(crate) fn document_fragment_get_element_by_id(fragment: Value, args: Vec<Value>) -> Value {
    let Some(node) = node_id_of(&fragment) else {
        return Value::Null;
    };
    let id = arg(&args, 0).to_js_string();
    if id.is_empty() {
        return Value::Null;
    }
    descendant_elements(node)
        .into_iter()
        .find(|candidate| dom::get_attribute(*candidate, "id").as_deref() == Some(id.as_str()))
        .map(element_value)
        .unwrap_or(Value::Null)
}

pub(crate) fn node_prototype_insert_before(receiver: Value, args: Vec<Value>) -> Value {
    if let Some(node) = node_id_of(&receiver) {
        return element_computed_get(node, "insertBefore").call(receiver, args);
    }
    if receiver.get_property("nodeType").to_u32() == 9
        && receiver
            .get_property("__w3cos_document_children")
            .as_array()
            .is_some()
    {
        return virtual_document_insert_before(receiver, args);
    }

    if args.len() < 2 {
        type_error("insertBefore requires 2 arguments");
    }
    if !is_dom_node_value(&arg(&args, 0)) {
        type_error("insertBefore requires a Node as its first argument");
    }
    dom_exception(
        "This node type cannot contain children",
        "HierarchyRequestError",
    )
}

pub(crate) fn node_prototype_replace_child(receiver: Value, args: Vec<Value>) -> Value {
    if let Some(node) = node_id_of(&receiver) {
        return element_computed_get(node, "replaceChild").call(receiver, args);
    }
    if receiver.get_property("nodeType").to_u32() == 9
        && receiver
            .get_property("__w3cos_document_children")
            .as_array()
            .is_some()
    {
        return virtual_document_replace_child(receiver, args);
    }

    if args.len() < 2 {
        type_error("replaceChild requires 2 arguments");
    }
    if !is_dom_node_value(&arg(&args, 0)) || !is_dom_node_value(&arg(&args, 1)) {
        type_error("replaceChild requires Node arguments");
    }
    dom_exception(
        "This node type cannot contain children",
        "HierarchyRequestError",
    )
}

pub(crate) fn node_prototype_clone_node(receiver: Value, args: Vec<Value>) -> Value {
    let Some(node) = node_id_of(&receiver) else {
        type_error("Node.prototype.cloneNode called on an incompatible receiver");
    };
    element_computed_get(node, "cloneNode").call(receiver, args)
}

pub(crate) fn element_prototype_attach_shadow(receiver: Value, args: Vec<Value>) -> Value {
    let Some(node) = node_id_of(&receiver) else {
        type_error("Element.prototype.attachShadow called on an incompatible receiver");
    };
    element_computed_get(node, "attachShadow").call(receiver, args)
}

pub(crate) fn shadow_root_prototype_inner_html_get(receiver: Value) -> Value {
    node_id_of(&receiver)
        .map(|node| element_computed_get(node, "innerHTML"))
        .unwrap_or(Value::Undefined)
}

pub(crate) fn shadow_root_prototype_inner_html_set(receiver: Value, args: Vec<Value>) -> Value {
    let Some(node) = node_id_of(&receiver) else {
        type_error("ShadowRoot.innerHTML requires a ShadowRoot receiver");
    };
    element_computed_set(node, "innerHTML", arg(&args, 0));
    Value::Undefined
}

fn dom_node_parent(node: &Value) -> Option<Value> {
    let parent = node.get_property("parentNode");
    (!parent.is_nullish() && is_dom_node_value(&parent)).then_some(parent)
}

fn dom_node_ancestor_path(node: &Value) -> Vec<Value> {
    let mut path = vec![node.clone()];
    while let Some(parent) = dom_node_parent(path.last().expect("a node path is never empty")) {
        if path.iter().any(|ancestor| ancestor.strict_eq(&parent)) {
            break;
        }
        path.push(parent);
    }
    path
}

fn disconnected_node_order(root: &Value) -> u64 {
    const KEY: &str = "__w3cos_disconnected_node_order";
    let existing = root.get_property(KEY).to_number();
    if existing.is_finite() && existing >= 1.0 {
        return existing as u64;
    }
    let next = NEXT_DISCONNECTED_NODE_ORDER.with(|order| {
        let next = order.get();
        order.set(next.saturating_add(1));
        next
    });
    root.set_property(KEY, Value::Number(next as f64));
    next
}

fn dom_node_children(node: &Value) -> Vec<Value> {
    if node.get_property("nodeType").to_u32() == 9 {
        if node.strict_eq(&document_value()) {
            return global_document_children();
        }
        return virtual_document_children(node);
    }
    node_id_of(node)
        .map(dom::children)
        .unwrap_or_default()
        .into_iter()
        .map(element_value)
        .collect()
}

pub(crate) fn node_prototype_contains(receiver: Value, args: Vec<Value>) -> Value {
    if !is_dom_node_value(&receiver) {
        type_error("Node.prototype.contains called on an incompatible receiver");
    }
    let other = arg(&args, 0);
    if other.is_nullish() {
        return Value::Bool(false);
    }
    if !is_dom_node_value(&other) {
        type_error("Node.prototype.contains requires a Node or null");
    }
    Value::Bool(
        receiver.strict_eq(&other)
            || dom_node_ancestor_path(&other)
                .into_iter()
                .skip(1)
                .any(|ancestor| ancestor.strict_eq(&receiver)),
    )
}

pub(crate) fn node_prototype_compare_document_position(
    receiver: Value,
    args: Vec<Value>,
) -> Value {
    const DISCONNECTED: u16 = 0x01;
    const PRECEDING: u16 = 0x02;
    const FOLLOWING: u16 = 0x04;
    const CONTAINS: u16 = 0x08;
    const CONTAINED_BY: u16 = 0x10;
    const IMPLEMENTATION_SPECIFIC: u16 = 0x20;

    if !is_dom_node_value(&receiver) {
        type_error("Node.prototype.compareDocumentPosition called on an incompatible receiver");
    }
    let other = arg(&args, 0);
    if !is_dom_node_value(&other) {
        type_error("Node.prototype.compareDocumentPosition requires a Node");
    }
    if receiver.strict_eq(&other) {
        return Value::Number(0.0);
    }

    let receiver_path = dom_node_ancestor_path(&receiver);
    let other_path = dom_node_ancestor_path(&other);
    let receiver_root = receiver_path.last().expect("a node path has a root");
    let other_root = other_path.last().expect("a node path has a root");
    if !receiver_root.strict_eq(other_root) {
        let direction = if disconnected_node_order(receiver_root)
            < disconnected_node_order(other_root)
        {
            FOLLOWING
        } else {
            PRECEDING
        };
        return Value::Number((DISCONNECTED | IMPLEMENTATION_SPECIFIC | direction) as f64);
    }

    if receiver_path
        .iter()
        .skip(1)
        .any(|ancestor| ancestor.strict_eq(&other))
    {
        return Value::Number((CONTAINS | PRECEDING) as f64);
    }
    if other_path
        .iter()
        .skip(1)
        .any(|ancestor| ancestor.strict_eq(&receiver))
    {
        return Value::Number((CONTAINED_BY | FOLLOWING) as f64);
    }

    let receiver_from_root = receiver_path.iter().rev().collect::<Vec<_>>();
    let other_from_root = other_path.iter().rev().collect::<Vec<_>>();
    let divergent_index = receiver_from_root
        .iter()
        .zip(&other_from_root)
        .position(|(left, right)| !left.strict_eq(right))
        .expect("distinct nodes in one tree must have divergent branches");
    let common_parent = receiver_from_root[divergent_index - 1];
    let receiver_branch = receiver_from_root[divergent_index];
    let other_branch = other_from_root[divergent_index];
    let children = dom_node_children(common_parent);
    let receiver_index = children
        .iter()
        .position(|child| child.strict_eq(receiver_branch));
    let other_index = children
        .iter()
        .position(|child| child.strict_eq(other_branch));
    let direction = if receiver_index.zip(other_index).is_some_and(|(left, right)| left < right) {
        FOLLOWING
    } else {
        PRECEDING
    };
    Value::Number(direction as f64)
}

pub(crate) fn node_prototype_has_child_nodes(receiver: Value, _args: Vec<Value>) -> Value {
    if !is_dom_node_value(&receiver) {
        type_error("Node.prototype.hasChildNodes called on an incompatible receiver");
    }
    if receiver.get_property("nodeType").to_u32() == 9 {
        let children = if receiver.strict_eq(&document_value()) {
            global_document_children()
        } else {
            virtual_document_children(&receiver)
        };
        return Value::Bool(!children.is_empty());
    }
    Value::Bool(
        node_id_of(&receiver)
            .and_then(dom::first_child)
            .is_some(),
    )
}

fn validate_document_parent_node_insertion(document: &Value, args: &[Value], prepend: bool) {
    let existing_children = virtual_document_children(document);
    let existing_element_count = existing_children
        .iter()
        .filter(|child| child.get_property("nodeType").to_u32() == 1)
        .count();
    let existing_doctype_count = existing_children
        .iter()
        .filter(|child| child.get_property("nodeType").to_u32() == 10)
        .count();
    let mut inserted_element_count = 0;
    let mut inserted_doctype_count = 0;

    for argument in args {
        let Some(node) = node_id_of(argument) else {
            dom_exception(
                "Documents cannot be inserted into documents and text is not a valid document child",
                "HierarchyRequestError",
            );
        };

        match dom::node_type(node) {
            3 => dom_exception(
                "Text is not a valid document child",
                "HierarchyRequestError",
            ),
            10 => inserted_doctype_count += 1,
            1 => inserted_element_count += 1,
            11 => {
                let children = dom::children(node);
                let element_count = children
                    .iter()
                    .filter(|child| dom::node_type(**child) == 1)
                    .count();
                let contains_text = children.iter().any(|child| dom::node_type(*child) == 3);
                let doctype_count = children
                    .iter()
                    .filter(|child| dom::node_type(**child) == 10)
                    .count();
                if contains_text {
                    dom_exception(
                        "The document fragment is not a valid document child sequence",
                        "HierarchyRequestError",
                    );
                }
                inserted_element_count += element_count;
                inserted_doctype_count += doctype_count;
            }
            _ => {}
        }
    }

    if existing_element_count + inserted_element_count > 1 {
        dom_exception(
            "A document can contain only one element",
            "HierarchyRequestError",
        );
    }
    if existing_doctype_count + inserted_doctype_count > 1
        || (!prepend && inserted_doctype_count > 0 && existing_element_count > 0)
    {
        dom_exception(
            "The document already has a doctype or the requested position follows its element",
            "HierarchyRequestError",
        );
    }
}

fn virtual_document_children(document: &Value) -> Vec<Value> {
    document
        .get_property("__w3cos_document_children")
        .as_array()
        .map(|children| children.borrow().clone())
        .unwrap_or_default()
}

fn virtual_document_child_nodes(document: Value) -> Value {
    let cached = document.get_property("__w3cos_cached_child_nodes");
    if !cached.is_undefined() {
        return cached;
    }
    let provider_document = document.clone();
    let list = live_node_list(move || virtual_document_children(&provider_document));
    document.set_property("__w3cos_cached_child_nodes", list.clone());
    list
}

fn set_virtual_document_children(document: &Value, children: Vec<Value>) {
    for child in virtual_document_children(document) {
        if let Some(node) = node_id_of(&child) {
            set_expando(node, "parentNode", Value::Null);
            set_expando(node, "previousSibling", Value::Null);
            set_expando(node, "nextSibling", Value::Null);
        }
    }
    for (index, child) in children.iter().enumerate() {
        if let Some(node) = node_id_of(child) {
            set_expando(node, "ownerDocument", document.clone());
            set_expando(node, "parentNode", document.clone());
            set_expando(
                node,
                "previousSibling",
                index
                    .checked_sub(1)
                    .and_then(|previous| children.get(previous))
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            set_expando(
                node,
                "nextSibling",
                children.get(index + 1).cloned().unwrap_or(Value::Null),
            );
        }
    }
    let first = children.first().cloned().unwrap_or(Value::Null);
    let last = children.last().cloned().unwrap_or(Value::Null);
    let doctype = children
        .iter()
        .find(|child| child.get_property("nodeType").to_u32() == 10)
        .cloned()
        .unwrap_or(Value::Null);
    let document_element = children
        .iter()
        .find(|child| child.get_property("nodeType").to_u32() == 1)
        .cloned()
        .unwrap_or(Value::Null);
    document.set_property("__w3cos_document_children", js_array(children.clone()));
    document.set_property("firstChild", first);
    document.set_property("lastChild", last);
    document.set_property("doctype", doctype);
    document.set_property("documentElement", document_element);
}

fn is_dom_node_value(value: &Value) -> bool {
    node_id_of(value).is_some()
        || matches!(value.get_property("nodeType").to_u32(), 2 | 9)
}

fn validate_virtual_document_child_sequence(children: &[Value]) {
    let mut element_index = None;
    let mut doctype_index = None;
    for (index, child) in children.iter().enumerate() {
        let node_type = child.get_property("nodeType").to_u32();
        match node_type {
            1 if element_index.replace(index).is_some() => dom_exception(
                "A document can contain only one element",
                "HierarchyRequestError",
            ),
            10 if doctype_index.replace(index).is_some() => dom_exception(
                "A document can contain only one doctype",
                "HierarchyRequestError",
            ),
            3 | 4 => dom_exception(
                "Text is not a valid document child",
                "HierarchyRequestError",
            ),
            1 | 7 | 8 | 10 => {}
            _ => dom_exception(
                "This node type is not a valid document child",
                "HierarchyRequestError",
            ),
        }
    }
    if doctype_index
        .zip(element_index)
        .is_some_and(|(doctype, element)| doctype > element)
    {
        dom_exception(
            "A document doctype must precede its element",
            "HierarchyRequestError",
        );
    }
}

fn virtual_document_insert_before(document: Value, args: Vec<Value>) -> Value {
    if args.len() < 2 {
        type_error("insertBefore requires 2 arguments");
    }
    let new_child = arg(&args, 0);
    let reference = arg(&args, 1);
    if !is_dom_node_value(&new_child) {
        type_error("insertBefore requires a Node as its first argument");
    }

    // DOM pre-insert checks the parent and host-including ancestry before
    // reference membership and node-position validity.
    if new_child.get_property("nodeType").to_u32() == 9 {
        dom_exception("Documents cannot be inserted", "HierarchyRequestError");
    }
    let children = virtual_document_children(&document);
    let reference = if reference.is_null() || reference.is_undefined() {
        None
    } else {
        if !is_dom_node_value(&reference) {
            type_error("insertBefore reference must be a Node, null, or undefined");
        }
        if !children
            .iter()
            .any(|existing| existing.strict_eq(&reference))
        {
            dom_exception(
                "The reference node is not a child of this parent",
                "NotFoundError",
            );
        }
        Some(reference)
    };

    if reference
        .as_ref()
        .is_some_and(|reference| reference.strict_eq(&new_child))
    {
        return new_child;
    }

    let node = node_id_of(&new_child)
        .unwrap_or_else(|| dom_exception("This node cannot be inserted", "HierarchyRequestError"));
    let inserted = if dom::node_type(node) == 11 {
        dom::children(node)
            .into_iter()
            .map(element_value)
            .collect::<Vec<_>>()
    } else {
        vec![new_child.clone()]
    };

    let mut candidate = children;
    candidate.retain(|existing| {
        !inserted
            .iter()
            .any(|inserted| inserted.strict_eq(existing))
    });
    let insertion_index = reference
        .as_ref()
        .and_then(|reference| {
            candidate
                .iter()
                .position(|existing| existing.strict_eq(reference))
        })
        .unwrap_or(candidate.len());
    candidate.splice(insertion_index..insertion_index, inserted.iter().cloned());
    validate_virtual_document_child_sequence(&candidate);

    for inserted in &inserted {
        adopt_node_into(&document, inserted.clone());
    }
    set_virtual_document_children(&document, candidate);
    new_child
}

fn virtual_document_replace_child(document: Value, args: Vec<Value>) -> Value {
    if args.len() < 2 {
        type_error("replaceChild requires 2 arguments");
    }
    let new_child = arg(&args, 0);
    let old_child = arg(&args, 1);
    if !is_dom_node_value(&new_child) || !is_dom_node_value(&old_child) {
        type_error("replaceChild requires Node arguments");
    }

    let children = virtual_document_children(&document);
    if !children
        .iter()
        .any(|existing| existing.strict_eq(&old_child))
    {
        dom_exception("The node is not a child of this parent", "NotFoundError");
    }
    if new_child.strict_eq(&old_child) {
        return old_child;
    }
    if new_child.get_property("nodeType").to_u32() == 9 {
        dom_exception("Documents cannot be inserted", "HierarchyRequestError");
    }
    let node = node_id_of(&new_child)
        .unwrap_or_else(|| dom_exception("This node cannot be inserted", "HierarchyRequestError"));
    let inserted = if dom::node_type(node) == 11 {
        dom::children(node)
            .into_iter()
            .map(element_value)
            .collect::<Vec<_>>()
    } else {
        vec![new_child]
    };

    let mut candidate = children;
    candidate.retain(|existing| {
        !inserted
            .iter()
            .any(|inserted| inserted.strict_eq(existing))
    });
    let old_index = candidate
        .iter()
        .position(|existing| existing.strict_eq(&old_child))
        .expect("the replaced document child remains after moving inserted nodes");
    candidate.splice(old_index..=old_index, inserted.iter().cloned());
    validate_virtual_document_child_sequence(&candidate);

    for inserted in &inserted {
        adopt_node_into(&document, inserted.clone());
    }
    set_virtual_document_children(&document, candidate);
    old_child
}

fn virtual_document_move_before(document: Value, args: Vec<Value>) -> Value {
    if args.len() < 2 {
        type_error("moveBefore requires 2 arguments");
    }
    let moving = arg(&args, 0);
    let reference = arg(&args, 1);
    if !is_dom_node_value(&moving) {
        type_error("moveBefore requires a Node as its first argument");
    }
    if !reference.is_null() && !reference.is_undefined() && !is_dom_node_value(&reference) {
        type_error("moveBefore reference must be a Node, null, or undefined");
    }
    let Some(moving_id) = node_id_of(&moving) else {
        dom_exception(
            "Only Element and CharacterData nodes can be moved",
            "HierarchyRequestError",
        );
    };

    let moving_document = get_expando(moving_id, "ownerDocument").unwrap_or_else(document_value);
    if !moving_document.strict_eq(&document) || !node_is_connected(moving_id) {
        dom_exception(
            "The destination and moved node must have the same shadow-including root",
            "HierarchyRequestError",
        );
    }

    // A preserved move into a Document may place document-compatible
    // CharacterData there, but it cannot move the document element/doctype or
    // introduce Text. This is intentionally narrower than ordinary pre-insert.
    if !matches!(dom::node_type(moving_id), 7 | 8) {
        dom_exception(
            "This node type cannot be moved directly into a Document",
            "HierarchyRequestError",
        );
    }

    let children = virtual_document_children(&document);
    let reference = if reference.is_null() || reference.is_undefined() {
        None
    } else {
        if !children
            .iter()
            .any(|existing| existing.strict_eq(&reference))
        {
            dom_exception(
                "The reference node is not a child of this parent",
                "NotFoundError",
            );
        }
        Some(reference)
    };
    if reference
        .as_ref()
        .is_some_and(|reference| reference.strict_eq(&moving))
    {
        return Value::Undefined;
    }

    let mut candidate = children
        .into_iter()
        .filter(|existing| !existing.strict_eq(&moving))
        .collect::<Vec<_>>();
    let insertion_index = reference
        .as_ref()
        .and_then(|reference| {
            candidate
                .iter()
                .position(|existing| existing.strict_eq(reference))
        })
        .unwrap_or(candidate.len());

    with_deferred_dom_post_insertion_steps(|| {
        if let Some(parent) = dom::parent_node(moving_id) {
            dom::remove_child(parent, moving_id);
        }
        candidate.insert(insertion_index, moving.clone());
        set_virtual_document_children(&document, candidate);
    });
    pin_element_subtree(moving_id);
    Value::Undefined
}

fn install_virtual_document_node_mutations(props: &mut HashMap<String, Value>) {
    props.insert(
        "removeChild".to_string(),
        func(|document, args| {
            let child = arg(&args, 0);
            if !is_dom_node_value(&child) {
                type_error("removeChild requires a Node");
            }
            let mut children = virtual_document_children(&document);
            let Some(index) = children
                .iter()
                .position(|existing| existing.strict_eq(&child))
            else {
                dom_exception("The node is not a child of this parent", "NotFoundError");
            };
            children.remove(index);
            set_virtual_document_children(&document, children);
            child
        }),
    );
    let insert_before = func(virtual_document_insert_before);
    insert_before.set_property("length", Value::Number(2.0));
    props.insert("insertBefore".to_string(), insert_before);
    let replace_child = func(virtual_document_replace_child);
    replace_child.set_property("length", Value::Number(2.0));
    props.insert("replaceChild".to_string(), replace_child);
    let move_before = func(virtual_document_move_before);
    move_before.set_property("length", Value::Number(2.0));
    props.insert("moveBefore".to_string(), move_before);
}

fn replace_virtual_document_children(document: &Value, arguments: Vec<Value>) {
    let mut replacements = Vec::new();
    for argument in arguments {
        let Some(node) = node_id_of(&argument) else {
            dom_exception(
                "Only document fragments, doctypes, elements, character data, and comments can be document children",
                "HierarchyRequestError",
            );
        };
        if dom::node_type(node) == 11 {
            replacements.extend(dom::children(node).into_iter().map(element_value));
        } else {
            replacements.push(argument);
        }
    }

    let mut element_index = None;
    let mut doctype_index = None;
    for (index, replacement) in replacements.iter().enumerate() {
        let node = node_id_of(replacement).expect("document replacement nodes were normalized");
        match dom::node_type(node) {
            1 if element_index.replace(index).is_some() => dom_exception(
                "A document can contain only one element",
                "HierarchyRequestError",
            ),
            10 if doctype_index.replace(index).is_some() => dom_exception(
                "A document can contain only one doctype",
                "HierarchyRequestError",
            ),
            3 | 4 => dom_exception(
                "Text is not a valid document child",
                "HierarchyRequestError",
            ),
            1 | 7 | 8 | 10 => {}
            _ => dom_exception(
                "This node type is not a valid document child",
                "HierarchyRequestError",
            ),
        }
    }
    if doctype_index
        .zip(element_index)
        .is_some_and(|(doctype, element)| doctype > element)
    {
        dom_exception(
            "A document doctype must precede its element",
            "HierarchyRequestError",
        );
    }

    for replacement in &replacements {
        adopt_node_into(document, replacement.clone());
    }
    set_virtual_document_children(document, replacements);
}

fn insert_virtual_document_children(document: &Value, args: Vec<Value>, prepend: bool) {
    let mut inserted = Vec::new();
    for argument in args {
        let Some(node) = node_id_of(&argument) else {
            continue;
        };
        if dom::node_type(node) == 11 {
            inserted.extend(dom::children(node).into_iter().map(element_value));
        } else {
            inserted.push(argument);
        }
    }
    let mut children = virtual_document_children(document);
    for child in &inserted {
        children.retain(|existing| !existing.strict_eq(child));
    }
    if prepend {
        inserted.extend(children);
        set_virtual_document_children(document, inserted);
    } else {
        children.extend(inserted);
        set_virtual_document_children(document, children);
    }
}

fn adopt_node_into(document: &Value, node: Value) -> Value {
    let Some(id) = node_id_of(&node) else {
        if node.get_property("nodeType").to_u32() == 9 {
            dom_exception("Documents cannot be adopted", "NotSupportedError");
        }
        return node;
    };

    if let Some(parent) = dom::parent_node(id) {
        dom::remove_child(parent, id);
    }
    if let Some(parent_document) = get_expando(id, "parentNode")
        && parent_document.get_property("nodeType").to_u32() == 9
    {
        if parent_document
            .get_property("__w3cos_document_children")
            .as_array()
            .is_some()
        {
            let children = virtual_document_children(&parent_document)
                .into_iter()
                .filter(|child| !child.strict_eq(&node))
                .collect();
            set_virtual_document_children(&parent_document, children);
        } else if parent_document.get_property("doctype").strict_eq(&node) {
            parent_document.set_property("doctype", Value::Null);
        }
    }
    set_expando(id, "parentNode", Value::Null);
    walk_subtree(id, &mut |child| {
        set_expando(child, "ownerDocument", document.clone());
        update_cached_attribute_owner_document(child, document);
    });
    node
}

fn import_node_into(document: &Value, node: Value, deep: bool) -> Value {
    if node.get_property("nodeType").to_u32() == 9 {
        dom_exception("Documents cannot be imported", "NotSupportedError");
    }
    if let Some(id) = node_id_of(&node) {
        let clone = dom::clone_node(id, deep);
        copy_cloned_node_identity(id, clone);
        walk_subtree(clone, &mut |child| {
            set_expando(child, "ownerDocument", document.clone())
        });
        return element_value(clone);
    }
    if node.get_property("nodeType").to_u32() == 2 {
        let imported = Value::object(HashMap::from([
            ("nodeType".to_string(), Value::Number(2.0)),
            ("nodeName".to_string(), node.get_property("nodeName")),
            ("nodeValue".to_string(), node.get_property("nodeValue")),
            ("name".to_string(), node.get_property("name")),
            ("value".to_string(), node.get_property("value")),
            ("prefix".to_string(), node.get_property("prefix")),
            (
                "namespaceURI".to_string(),
                node.get_property("namespaceURI"),
            ),
            ("localName".to_string(), node.get_property("localName")),
            ("ownerDocument".to_string(), document.clone()),
            ("ownerElement".to_string(), Value::Null),
            ("specified".to_string(), Value::Bool(true)),
        ]));
        w3cos_core::class::set_prototype_of(&imported, &crate::dom_constructors::prototype("Attr"));
        return imported;
    }
    Value::Null
}

fn parse_document(source: &str, content_type: &str) -> Value {
    parse_document_mode(source, content_type, false)
}

fn html_start_tag_attributes(source: &str, expected_tag: &str) -> Vec<(String, String)> {
    let bytes = source.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let Some(relative_start) = source[pos..].find('<') else {
            break;
        };
        let start = pos + relative_start + 1;
        if start >= bytes.len() || matches!(bytes[start], b'/' | b'!' | b'?') {
            pos = start;
            continue;
        }
        let mut name_end = start;
        while name_end < bytes.len()
            && (bytes[name_end].is_ascii_alphanumeric() || matches!(bytes[name_end], b'-' | b':'))
        {
            name_end += 1;
        }
        if !source[start..name_end].eq_ignore_ascii_case(expected_tag) {
            pos = name_end.max(start + 1);
            continue;
        }
        let mut end = name_end;
        let mut quote = None;
        while end < bytes.len() {
            match (quote, bytes[end]) {
                (Some(active), current) if active == current => quote = None,
                (None, current @ (b'\'' | b'"')) => quote = Some(current),
                (None, b'>') => return parse_html_attributes(&source[name_end..end]),
                _ => {}
            }
            end += 1;
        }
        break;
    }
    Vec::new()
}

fn apply_html_start_tag_attributes(node: u32, source: &str, tag: &str) {
    for (name, value) in html_start_tag_attributes(source, tag) {
        apply_html_attribute(node, &name, &value);
    }
}

fn html_document_type_name(source: &str) -> Option<String> {
    let lower = source.to_ascii_lowercase();
    let marker = "<!doctype";
    let marker_end = lower.find(marker)? + marker.len();
    let remainder = source
        .get(marker_end..)?
        .trim_start_matches(['\u{0009}', '\u{000a}', '\u{000c}', '\u{000d}', '\u{0020}']);
    let name_end = remainder
        .find(|character: char| {
            matches!(
                character,
                '\u{0009}' | '\u{000a}' | '\u{000c}' | '\u{000d}' | '\u{0020}' | '>'
            )
        })
        .unwrap_or(remainder.len());
    let name = remainder.get(..name_end)?;
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

fn parse_document_mode(source: &str, content_type: &str, sanitize: bool) -> Value {
    let supported = matches!(
        content_type,
        "text/html" | "text/xml" | "application/xml" | "application/xhtml+xml" | "image/svg+xml"
    );
    let container = dom::create_element("div");
    if sanitize {
        append_sanitized_html_fragment(container, source);
    } else if content_type == "text/html" || !supported {
        append_html_fragment(container, source);
    } else {
        if let Err(error) = crate::xml_tree_builder::append_xml_document_fragment(container, source)
        {
            let parser_error = dom::create_element("parsererror");
            dom::set_text_content(parser_error, &error.to_string());
            return parsed_document_value(parser_error, "application/xml", None, None);
        }
    }
    if !supported {
        let error = dom::create_element("parsererror");
        dom::set_text_content(error, &format!("Unsupported MIME type: {content_type}"));
        return parsed_document_value(error, "application/xml", None, None);
    }
    if content_type == "text/html" {
        let existing_html = child_elements(container)
            .into_iter()
            .find(|node| dom::tag_name(*node) == "html");
        let html = existing_html.unwrap_or_else(|| dom::create_element("html"));
        let mut head = child_elements(html)
            .into_iter()
            .find(|node| dom::tag_name(*node) == "head");
        let mut body = child_elements(html)
            .into_iter()
            .find(|node| dom::tag_name(*node) == "body");
        if head.is_none() {
            let node = dom::create_element("head");
            match dom::first_child(html) {
                Some(first) => dom::insert_before(html, node, first),
                None => dom::append_child(html, node),
            }
            head = Some(node);
        }
        if body.is_none() {
            let node = dom::create_element("body");
            dom::append_child(html, node);
            body = Some(node);
        }
        apply_html_start_tag_attributes(html, source, "html");
        apply_html_start_tag_attributes(head.expect("HTML head was created"), source, "head");
        apply_html_start_tag_attributes(body.expect("HTML body was created"), source, "body");
        if existing_html.is_none() {
            for child in dom::children(container) {
                dom::append_child(body.expect("HTML body was created"), child);
            }
        }
        let document = parsed_document_value(html, content_type, head, body);
        if let Some(name) = html_document_type_name(source) {
            let doctype = create_document_type_value(&name, "", "");
            set_virtual_document_children(&document, vec![doctype, element_value(html)]);
        }
        return document;
    }

    XML_PARSER_WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[w3cos] warning: DOMParser XML modes use the compatibility parser; DTD, \
                 namespace validation, and strict well-formedness diagnostics are unavailable"
            );
        }
    });
    let root = first_element_child(container);
    if root.is_none() {
        let children = dom::children(container);
        if children.iter().any(|node| dom::node_type(*node) == 7) {
            let document = empty_document_value(content_type, "XMLDocument");
            let children = children
                .into_iter()
                .map(|node| {
                    dom::remove_child(container, node);
                    set_expando(node, "ownerDocument", document.clone());
                    element_value(node)
                })
                .collect();
            set_virtual_document_children(&document, children);
            return document;
        }
    }
    let root = root.unwrap_or_else(|| {
        let error = dom::create_element("parsererror");
        dom::set_text_content(error, "No document element");
        error
    });
    if dom::parent_node(root) == Some(container) {
        dom::remove_child(container, root);
    }
    if content_type == "image/svg+xml" {
        set_expando(
            root,
            "namespaceURI",
            Value::string("http://www.w3.org/2000/svg"),
        );
    }
    let xhtml_document = namespace_uri(root) == crate::html_parser_state::HTML_NAMESPACE
        && dom::tag_name(root) == "html";
    let head = xhtml_document.then(|| {
        child_elements(root)
            .into_iter()
            .find(|node| dom::tag_name(*node) == "head")
    });
    let body = xhtml_document.then(|| {
        child_elements(root)
            .into_iter()
            .find(|node| dom::tag_name(*node) == "body")
    });
    let document = parsed_document_value(root, content_type, head.flatten(), body.flatten());
    if let Some(name) = html_document_type_name(source) {
        let doctype = create_document_type_value(&name, "", "");
        set_virtual_document_children(&document, vec![doctype, element_value(root)]);
    }
    document
}

pub(crate) fn sanitized_document_value(source: &str) -> Value {
    parse_document_mode(source, "text/html", true)
}

pub(crate) fn unsafe_document_value(source: &str) -> Value {
    parse_document(source, "text/html")
}

pub(crate) fn sanitized_fragment_value(source: &str) -> Value {
    let fragment = dom::create_document_fragment();
    append_sanitized_html_fragment(fragment, source);
    element_value(fragment)
}

pub(crate) fn sanitized_element_value(tag_name: &str, source: &str) -> Value {
    let element = dom::create_element(tag_name);
    append_sanitized_html_fragment(element, source);
    element_value(element)
}

fn dom_parser_class() -> Value {
    DOM_PARSER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = func(|_, _| {
            let value = Value::object(HashMap::from([(
                "parseFromString".to_string(),
                func(|_, args| {
                    parse_document(&arg(&args, 0).to_js_string(), &arg(&args, 1).to_js_string())
                }),
            )]));
            w3cos_core::class::set_prototype_of(
                &value,
                &dom_parser_class().get_property("prototype"),
            );
            value
        });
        class.set_property("supported", Value::Bool(true));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property(
            "parseFromString",
            func(|_, args| {
                parse_document(&arg(&args, 0).to_js_string(), &arg(&args, 1).to_js_string())
            }),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn xml_serializer_class() -> Value {
    XML_SERIALIZER_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = func(|_, _| {
            let value = Value::object(HashMap::from([(
                "serializeToString".to_string(),
                func(|_, args| {
                    let input = arg(&args, 0);
                    let root = node_id_of(&input).or_else(|| {
                        let root = input.get_property("__w3cos_document_root");
                        (!root.is_undefined()).then(|| root.to_u32())
                    });
                    root.map(dom::outer_html)
                        .map(Value::from)
                        .unwrap_or_else(|| Value::string(""))
                }),
            )]));
            w3cos_core::class::set_prototype_of(
                &value,
                &xml_serializer_class().get_property("prototype"),
            );
            value
        });
        class.set_property("supported", Value::Bool(true));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property(
            "serializeToString",
            func(|_, args| {
                let input = arg(&args, 0);
                let root = node_id_of(&input).or_else(|| {
                    let root = input.get_property("__w3cos_document_root");
                    (!root.is_undefined()).then(|| root.to_u32())
                });
                root.map(dom::outer_html)
                    .map(Value::from)
                    .unwrap_or_else(|| Value::string(""))
            }),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn css_escape(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut output = String::new();
    for (index, ch) in chars.iter().copied().enumerate() {
        let code = ch as u32;
        if code == 0 {
            output.push('\u{fffd}');
        } else if (1..=31).contains(&code) || code == 127 {
            output.push_str(&format!("\\{code:x} "));
        } else if index == 0 && ch.is_ascii_digit() {
            output.push_str(&format!("\\{code:x} "));
        } else if index == 1 && ch.is_ascii_digit() && chars.first() == Some(&'-') {
            output.push_str(&format!("\\{code:x} "));
        } else if index == 0 && ch == '-' && chars.len() == 1 {
            output.push_str("\\-");
        } else if code >= 128 || ch == '-' || ch == '_' || ch.is_ascii_alphanumeric() {
            output.push(ch);
        } else {
            output.push('\\');
            output.push(ch);
        }
    }
    output
}

fn legacy_escape(value: &str) -> String {
    let mut output = String::new();
    for unit in value.encode_utf16() {
        let ch = char::from_u32(unit as u32);
        if ch.is_some_and(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '@' | '*' | '_' | '+' | '-' | '.' | '/')
        }) {
            output.push(ch.expect("ASCII escape-safe code unit"));
        } else if unit < 256 {
            output.push_str(&format!("%{unit:02X}"));
        } else {
            output.push_str(&format!("%u{unit:04X}"));
        }
    }
    output
}

fn legacy_unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut units = Vec::<u16>::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 5 < bytes.len()
            && bytes[index + 1] == b'u'
            && let Ok(unit) = u16::from_str_radix(&value[index + 2..index + 6], 16)
        {
            units.push(unit);
            index += 6;
            continue;
        }
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(unit) = u16::from_str_radix(&value[index + 1..index + 3], 16)
        {
            units.push(unit);
            index += 3;
            continue;
        }
        let ch = value[index..].chars().next().expect("valid UTF-8 suffix");
        units.extend(ch.to_string().encode_utf16());
        index += ch.len_utf8();
    }
    String::from_utf16_lossy(&units)
}

fn eval_compat_value() -> Value {
    func(|_, args| {
        let input = arg(&args, 0);
        if !input.is_string() {
            return input;
        }
        #[cfg(feature = "dynamic-js")]
        {
            return crate::dynamic_script::evaluate_global_expression(&input.to_js_string())
                .unwrap_or_else(|error| {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "EvalError",
                        vec![Value::string(&error.to_string())],
                    ))
                });
        }
        #[cfg(not(feature = "dynamic-js"))]
        EVAL_WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!(
                    "[W3C OS][compat warning] eval(string) is unavailable in the AOT runtime; \
                     returning undefined"
                );
            }
        });
        #[cfg(not(feature = "dynamic-js"))]
        Value::Undefined
    })
}

fn function_compat_class() -> Value {
    let class = Value::function(|_, args| {
        #[cfg(feature = "dynamic-js")]
        {
            return crate::dynamic_script::construct_global_function(&args).unwrap_or_else(
                |error| {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "SyntaxError",
                        vec![Value::string(&error.to_string())],
                    ))
                },
            );
        }
        #[cfg(not(feature = "dynamic-js"))]
        FUNCTION_WARNING_EMITTED.with(|warned| {
            if !warned.replace(true) {
                eprintln!(
                    "[W3C OS][compat warning] Function constructor source compilation is \
                     unavailable in the AOT runtime; returned function yields undefined"
                );
            }
        });
        #[cfg(not(feature = "dynamic-js"))]
        Value::function(|_, _| Value::Undefined)
    });
    class.set_property("name", Value::string("Function"));
    let prototype = Value::object(HashMap::new());
    prototype.set_property("constructor", class.clone());
    class.set_property("prototype", prototype);
    class
}

fn frame_function_compat_class(global: Value, document: Value) -> Value {
    let parent_function = window_value().get_property("Function");
    let class = Value::function(move |_, args| {
        #[cfg(feature = "dynamic-js")]
        {
            return crate::dynamic_script::construct_global_function_in_realm(
                &args,
                &global,
                &document,
            )
            .unwrap_or_else(|error| {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "SyntaxError",
                    vec![Value::string(&error.to_string())],
                ))
            });
        }
        #[cfg(not(feature = "dynamic-js"))]
        Value::function(|_, _| Value::Undefined)
    });
    class.set_property("name", Value::string("Function"));
    class.set_property("prototype", parent_function.get_property("prototype"));
    class
}

fn string_compat_class() -> Value {
    let class = Value::function(|_, args| {
        Value::string(&args.first().map(Value::to_js_string).unwrap_or_default())
    });
    class.set_property("name", Value::string("String"));
    class.set_property(
        "fromCharCode",
        Value::function(|_, args| {
            let units = args
                .iter()
                .map(|value| value.to_u32() as u16)
                .collect::<Vec<_>>();
            Value::string(&String::from_utf16_lossy(&units))
        }),
    );
    class.set_property(
        "fromCodePoint",
        Value::function(|_, args| {
            let text = args
                .iter()
                .filter_map(|value| char::from_u32(value.to_u32()))
                .collect::<String>();
            Value::string(&text)
        }),
    );
    let prototype = class.get_property("prototype");
    prototype.set_property("constructor", class.clone());
    class
}

fn number_compat_class() -> Value {
    let class =
        Value::function(|_, args| Value::Number(args.first().map(Value::to_number).unwrap_or(0.0)));
    class.set_property("name", Value::string("Number"));
    class.set_property(
        "isFinite",
        Value::function(|_, args| {
            Value::Bool(
                args.first()
                    .is_some_and(|value| value.is_number() && value.to_number().is_finite()),
            )
        }),
    );
    class.set_property(
        "isInteger",
        Value::function(|_, args| {
            Value::Bool(args.first().is_some_and(|value| {
                let number = value.to_number();
                value.is_number() && number.is_finite() && number.fract() == 0.0
            }))
        }),
    );
    class.set_property(
        "isNaN",
        Value::function(|_, args| {
            Value::Bool(
                args.first()
                    .is_some_and(|value| value.is_number() && value.to_number().is_nan()),
            )
        }),
    );
    class.set_property(
        "isSafeInteger",
        Value::function(|_, args| {
            Value::Bool(args.first().is_some_and(|value| {
                let number = value.to_number();
                value.is_number()
                    && number.is_finite()
                    && number.fract() == 0.0
                    && number.abs() <= 9_007_199_254_740_991.0
            }))
        }),
    );
    let prototype = class.get_property("prototype");
    prototype.set_property("constructor", class.clone());
    class
}

fn boolean_compat_class() -> Value {
    let class = Value::function(|_, args| Value::Bool(args.first().is_some_and(Value::to_bool)));
    class.set_property("name", Value::string("Boolean"));
    let prototype = class.get_property("prototype");
    prototype.set_property("constructor", class.clone());
    class
}

fn css_property_supported(property: &str, value: &str) -> bool {
    let property = camel_to_kebab(property.trim());
    if property.starts_with("--") {
        return true;
    }
    if value.trim().is_empty() {
        return false;
    }
    matches!(
        property.as_str(),
        "align-content"
            | "align-items"
            | "align-self"
            | "animation"
            | "appearance"
            | "aspect-ratio"
            | "backdrop-filter"
            | "background"
            | "background-color"
            | "background-image"
            | "background-size"
            | "background-position"
            | "background-repeat"
            | "background-origin"
            | "background-clip"
            | "background-attachment"
            | "background-blend-mode"
            | "border"
            | "border-bottom-style"
            | "border-collapse"
            | "border-left-style"
            | "border-radius"
            | "border-right-style"
            | "border-top-style"
            | "bottom"
            | "box-shadow"
            | "box-sizing"
            | "color"
            | "column-gap"
            | "contain"
            | "content"
            | "clear"
            | "cursor"
            | "display"
            | "filter"
            | "flex"
            | "flex-basis"
            | "flex-direction"
            | "flex-grow"
            | "flex-shrink"
            | "flex-wrap"
            | "float"
            | "font"
            | "font-family"
            | "font-size"
            | "font-style"
            | "font-weight"
            | "gap"
            | "grid"
            | "grid-area"
            | "grid-template-columns"
            | "grid-template-rows"
            | "height"
            | "inset"
            | "justify-content"
            | "left"
            | "letter-spacing"
            | "line-height"
            | "margin"
            | "margin-bottom"
            | "margin-left"
            | "margin-right"
            | "margin-top"
            | "max-height"
            | "max-width"
            | "min-height"
            | "min-width"
            | "object-fit"
            | "opacity"
            | "order"
            | "empty-cells"
            | "outline"
            | "overflow"
            | "overflow-x"
            | "overflow-y"
            | "padding"
            | "padding-bottom"
            | "padding-left"
            | "padding-right"
            | "padding-top"
            | "pointer-events"
            | "position"
            | "right"
            | "row-gap"
            | "text-align"
            | "text-decoration"
            | "text-overflow"
            | "top"
            | "transform"
            | "transform-origin"
            | "transition"
            | "user-select"
            | "visibility"
            | "white-space"
            | "width"
            | "will-change"
            | "z-index"
    )
}

fn css_condition_parts<'a>(condition: &'a str, keyword: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut depth = 0_u32;
    let mut start = 0;
    let mut index = 0;
    while index < condition.len() {
        if depth == 0 && condition[index..].starts_with(keyword) {
            parts.push(condition[start..index].trim());
            index += keyword.len();
            start = index;
            continue;
        }
        let ch = condition[index..].chars().next().expect("valid UTF-8");
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        index += ch.len_utf8();
    }
    parts.push(condition[start..].trim());
    parts
}

fn css_outer_group(condition: &str) -> Option<&str> {
    if !condition.starts_with('(') || !condition.ends_with(')') {
        return None;
    }
    let mut depth = 0_u32;
    for (index, ch) in condition.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 && index + ch.len_utf8() != condition.len() {
                    return None;
                }
            }
            _ => {}
        }
    }
    (depth == 0).then(|| &condition[1..condition.len() - 1])
}

fn css_declaration_parts(condition: &str) -> Option<(&str, &str)> {
    let mut depth = 0_u32;
    for (index, ch) in condition.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                return Some((&condition[..index], &condition[index + 1..]));
            }
            _ => {}
        }
    }
    None
}

fn css_condition_supported(condition: &str) -> bool {
    let condition = condition.trim();
    if condition.is_empty() {
        return false;
    }
    let or_parts = css_condition_parts(condition, " or ");
    if or_parts.len() > 1 {
        return or_parts.into_iter().any(css_condition_supported);
    }
    let and_parts = css_condition_parts(condition, " and ");
    if and_parts.len() > 1 {
        return and_parts.into_iter().all(css_condition_supported);
    }
    if condition
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("not "))
    {
        return !css_condition_supported(&condition[4..]);
    }
    if condition
        .get(..9)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("selector("))
        && condition.ends_with(')')
    {
        return !condition[9..condition.len() - 1].trim().is_empty();
    }
    if let Some(inner) = css_outer_group(condition) {
        if let Some((property, value)) = css_declaration_parts(inner) {
            return css_property_supported(property, value);
        }
        return css_condition_supported(inner);
    }
    css_declaration_parts(condition)
        .is_some_and(|(property, value)| css_property_supported(property, value))
}

fn css_namespace_value() -> Value {
    Value::object(HashMap::from([
        (
            "highlights".to_string(),
            crate::highlight_web::highlight_registry_value(),
        ),
        (
            "escape".to_string(),
            func(|_, args| Value::string(&css_escape(&arg(&args, 0).to_js_string()))),
        ),
        (
            "supports".to_string(),
            func(|_, args| {
                if args.len() >= 2 {
                    return Value::Bool(css_property_supported(
                        &arg(&args, 0).to_js_string(),
                        &arg(&args, 1).to_js_string(),
                    ));
                }
                Value::Bool(css_condition_supported(&arg(&args, 0).to_js_string()))
            }),
        ),
    ]))
}

fn css_style_declaration_class() -> Value {
    CSS_STYLE_DECLARATION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".to_string(), Value::string("TypeError")),
                (
                    "message".to_string(),
                    Value::string("Illegal constructor: CSSStyleDeclaration"),
                ),
            ])))
        });
        class.set_property("name", Value::string("CSSStyleDeclaration"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for member in [
            "cssFloat",
            "cssText",
            "getPropertyPriority",
            "getPropertyValue",
            "item",
            "length",
            "parentRule",
            "removeProperty",
            "setProperty",
        ] {
            prototype.set_property(member, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn illegal_cssom_class(
    slot: &'static std::thread::LocalKey<RefCell<Option<Value>>>,
    name: &'static str,
    properties: &'static [&'static str],
) -> Value {
    slot.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
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
        for property in properties {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn style_sheet_class() -> Value {
    illegal_cssom_class(
        &STYLE_SHEET_CLASS,
        "StyleSheet",
        &[
            "disabled",
            "href",
            "media",
            "ownerNode",
            "parentStyleSheet",
            "title",
            "type",
        ],
    )
}

fn media_list_class() -> Value {
    illegal_cssom_class(
        &MEDIA_LIST_CLASS,
        "MediaList",
        &[
            "appendMedium",
            "deleteMedium",
            "item",
            "length",
            "mediaText",
            "toString",
        ],
    )
}

fn style_sheet_list_class() -> Value {
    illegal_cssom_class(
        &STYLE_SHEET_LIST_CLASS,
        "StyleSheetList",
        &["item", "length"],
    )
}

fn parse_media_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|medium| !medium.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn media_list_value(text: &str) -> Value {
    let media = Rc::new(RefCell::new(parse_media_list(text)));
    let getter_media = Rc::clone(&media);
    let setter_media = Rc::clone(&media);
    let length_media = Rc::clone(&media);
    let item_media = Rc::clone(&media);
    let append_media = Rc::clone(&media);
    let delete_media = Rc::clone(&media);
    let string_media = media;
    let value = Value::object(HashMap::from([
        (
            "__w3cos_getter_mediaText".into(),
            func(move |_, _| Value::string(&getter_media.borrow().join(", "))),
        ),
        (
            "__w3cos_setter_mediaText".into(),
            func(move |_, args| {
                *setter_media.borrow_mut() = parse_media_list(&arg(&args, 0).to_js_string());
                Value::Undefined
            }),
        ),
        (
            "__w3cos_getter_length".into(),
            func(move |_, _| Value::Number(length_media.borrow().len() as f64)),
        ),
        (
            "item".into(),
            func(move |_, args| {
                item_media
                    .borrow()
                    .get(arg(&args, 0).to_u32() as usize)
                    .map(|medium| Value::string(medium))
                    .unwrap_or(Value::Null)
            }),
        ),
        (
            "appendMedium".into(),
            func(move |_, args| {
                let medium = arg(&args, 0).to_js_string();
                if !append_media.borrow().iter().any(|item| item == &medium) {
                    append_media.borrow_mut().push(medium);
                }
                Value::Undefined
            }),
        ),
        (
            "deleteMedium".into(),
            func(move |_, args| {
                let medium = arg(&args, 0).to_js_string();
                let mut media = delete_media.borrow_mut();
                let Some(index) = media.iter().position(|item| item == &medium) else {
                    w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                        "The medium was not found",
                        "NotFoundError",
                    ));
                };
                media.remove(index);
                Value::Undefined
            }),
        ),
        (
            "toString".into(),
            func(move |_, _| Value::string(&string_media.borrow().join(", "))),
        ),
    ]));
    w3cos_core::class::set_prototype_of(&value, &media_list_class().get_property("prototype"));
    value
}

fn style_sheet_list_value(sheets: Rc<RefCell<Vec<Value>>>) -> Value {
    let length_sheets = Rc::clone(&sheets);
    let item_sheets = Rc::clone(&sheets);
    let indexed_sheets = sheets;
    let properties = HashMap::from([
        (
            "__w3cos_getter_length".into(),
            func(move |_, _| Value::Number(length_sheets.borrow().len() as f64)),
        ),
        (
            "item".into(),
            func(move |_, args| {
                item_sheets
                    .borrow()
                    .get(arg(&args, 0).to_u32() as usize)
                    .cloned()
                    .unwrap_or(Value::Null)
            }),
        ),
    ]);
    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
            let inherited = target.get_property(key);
            if !inherited.is_undefined() {
                return inherited;
            }
            key.parse::<usize>()
                .ok()
                .and_then(|index| indexed_sheets.borrow().get(index).cloned())
                .unwrap_or(Value::Undefined)
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        properties, handler,
    ))));
    w3cos_core::class::set_prototype_of(
        &value,
        &style_sheet_list_class().get_property("prototype"),
    );
    value
}

fn stylesheet_rules_value(rules: &Rc<RefCell<Vec<String>>>) -> Value {
    js_array(
        rules
            .borrow()
            .iter()
            .map(|css_text| {
                let selector = css_text
                    .split_once('{')
                    .map(|(selector, _)| selector.trim())
                    .unwrap_or_default();
                crate::css_rules_web::style_rule_value(css_text, selector)
            })
            .collect(),
    )
}

fn parse_stylesheet_rules(text: &str) -> Vec<String> {
    text.split_inclusive('}')
        .map(str::trim)
        .filter(|rule| !rule.is_empty() && rule.contains('{'))
        .map(ToString::to_string)
        .collect()
}

fn stylesheet_value() -> Value {
    let rules = Rc::new(RefCell::new(Vec::<String>::new()));
    let value = Value::object(HashMap::from([
        ("disabled".to_string(), Value::Bool(false)),
        ("href".to_string(), Value::Null),
        ("ownerNode".to_string(), Value::Null),
        ("parentStyleSheet".to_string(), Value::Null),
        ("title".to_string(), Value::Null),
        ("type".to_string(), Value::string("text/css")),
        ("media".to_string(), media_list_value("")),
    ]));
    for name in ["cssRules", "rules"] {
        let rules = rules.clone();
        value.set_property(
            &format!("__w3cos_getter_{name}"),
            func(move |_, _| stylesheet_rules_value(&rules)),
        );
    }
    {
        let rules = rules.clone();
        value.set_property(
            "insertRule",
            func(move |_, args| {
                let rule = arg(&args, 0).to_js_string();
                let mut rules = rules.borrow_mut();
                let index = if arg(&args, 1).is_undefined() {
                    0
                } else {
                    arg(&args, 1).to_u32() as usize
                };
                if index > rules.len() {
                    w3cos_core::throw_value(Value::object(HashMap::from([(
                        "name".to_string(),
                        Value::string("IndexSizeError"),
                    )])));
                }
                rules.insert(index, rule);
                Value::Number(index as f64)
            }),
        );
    }
    {
        let rules = rules.clone();
        value.set_property(
            "deleteRule",
            func(move |_, args| {
                let index = arg(&args, 0).to_u32() as usize;
                if index < rules.borrow().len() {
                    rules.borrow_mut().remove(index);
                }
                Value::Undefined
            }),
        );
    }
    {
        let rules = rules.clone();
        value.set_property(
            "replaceSync",
            func(move |_, args| {
                *rules.borrow_mut() = parse_stylesheet_rules(&arg(&args, 0).to_js_string());
                Value::Undefined
            }),
        );
    }
    {
        let rules = rules.clone();
        let sheet = value.clone();
        value.set_property(
            "replace",
            func(move |_, args| {
                *rules.borrow_mut() = parse_stylesheet_rules(&arg(&args, 0).to_js_string());
                resolved_thenable(sheet.clone())
            }),
        );
    }
    value.set_property("addRule", value.get_property("insertRule"));
    value.set_property("removeRule", value.get_property("deleteRule"));
    w3cos_core::class::set_prototype_of(&value, &css_style_sheet_class().get_property("prototype"));
    value
}

fn processing_instruction_sheet(node: u32) -> Value {
    if let Some(sheet) = get_expando(node, "sheet") {
        return sheet;
    }
    if dom::tag_name(node) != "xml-stylesheet" {
        return Value::Null;
    }
    let attributes = parse_html_attributes(&dom::get_text_content(node).unwrap_or_default());
    let href = attributes
        .iter()
        .find_map(|(name, value)| (name == "href").then(|| value.clone()));
    let css_type = attributes
        .iter()
        .find_map(|(name, value)| (name == "type").then(|| value.as_str()));
    let Some(href) = href.filter(|_| css_type.is_none_or(|value| value == "text/css")) else {
        return Value::Null;
    };
    let sheet = stylesheet_value();
    sheet.set_property("href", Value::string(&href));
    sheet.set_property("ownerNode", element_value(node));
    set_expando(node, "sheet", sheet.clone());
    sheet
}

pub(crate) fn install_author_stylesheet(node: u32, href: Option<&str>, source: &str) -> Value {
    remove_author_stylesheet(node);
    let sheet = stylesheet_value();
    sheet.set_property("href", href.map(Value::string).unwrap_or(Value::Null));
    sheet.set_property("ownerNode", element_value(node));
    sheet.set_property("__w3cos_owner_node_id", Value::Number(f64::from(node)));
    sheet.set_property(
        "media",
        media_list_value(&dom::get_attribute(node, "media").unwrap_or_default()),
    );
    sheet.call_method("replaceSync", vec![Value::string(source)]);
    set_expando(node, "sheet", sheet.clone());
    AUTHOR_STYLE_SHEETS.with(|sheets| sheets.borrow_mut().push(sheet.clone()));
    sheet
}

pub(crate) fn remove_author_stylesheet(node: u32) {
    AUTHOR_STYLE_SHEETS.with(|sheets| {
        sheets
            .borrow_mut()
            .retain(|sheet| sheet.get_property("__w3cos_owner_node_id").to_u32() != node);
    });
    set_expando(node, "sheet", Value::Null);
}

pub(crate) fn order_author_stylesheets(nodes: &[u32]) {
    AUTHOR_STYLE_SHEETS.with(|sheets| {
        let mut sheets = sheets.borrow_mut();
        sheets.sort_by_key(|sheet| {
            let owner = sheet.get_property("__w3cos_owner_node_id").to_u32();
            nodes
                .iter()
                .position(|node| *node == owner)
                .unwrap_or(usize::MAX)
        });
    });
}

fn css_style_sheet_class() -> Value {
    CSS_STYLE_SHEET_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| stylesheet_value());
        class.set_property("name", Value::string("CSSStyleSheet"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in [
            "addRule",
            "cssRules",
            "deleteRule",
            "insertRule",
            "ownerRule",
            "removeRule",
            "replace",
            "replaceSync",
            "rules",
        ] {
            prototype.set_property(property, Value::Undefined);
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &style_sheet_class().get_property("prototype"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn rect_value(rect: w3cos_dom::DOMRect) -> Value {
    crate::geometry_web::rect(
        rect.x as f64,
        rect.y as f64,
        rect.width as f64,
        rect.height as f64,
    )
}

fn caret_position_class() -> Value {
    CARET_POSITION_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("Illegal constructor: CaretPosition")],
            ))
        });
        class.set_property("name", Value::string("CaretPosition"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("getClientRect", Value::Undefined);
        prototype.set_property("offset", Value::Undefined);
        prototype.set_property("offsetNode", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn math_ml_element_class() -> Value {
    MATH_ML_ELEMENT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("Illegal constructor: MathMLElement")],
            ))
        });
        class.set_property("name", Value::string("MathMLElement"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        if let Value::Object(html_prototype) = crate::dom_constructors::prototype("HTMLElement") {
            for key in html_prototype.borrow().keys() {
                if key != "constructor" {
                    prototype.set_property(&key, Value::Undefined);
                }
            }
        }
        w3cos_core::class::set_prototype_of(
            &prototype,
            &crate::dom_constructors::prototype("Element"),
        );
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn window_class() -> Value {
    WINDOW_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("Illegal constructor: Window")],
            ))
        });
        class.set_property("name", Value::string("Window"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for (name, value) in [("TEMPORARY", 0.0), ("PERSISTENT", 1.0)] {
            prototype.set_property(name, Value::Number(value));
            class.set_property(name, Value::Number(value));
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

fn deepest_node_at_point(node: u32, x: f32, y: f32) -> Option<u32> {
    let mut found = None;
    for child in dom::children(node) {
        if let Some(descendant) = deepest_node_at_point(child, x, y) {
            found = Some(descendant);
            continue;
        }
        let rect = dom::bounding_rect(child);
        if x >= rect.left() && x <= rect.right() && y >= rect.top() && y <= rect.bottom() {
            found = Some(child);
        }
    }
    found
}

fn first_text_descendant(node: u32) -> Option<u32> {
    if dom::node_type(node) == 3 {
        return Some(node);
    }
    dom::children(node)
        .into_iter()
        .find_map(first_text_descendant)
}

fn caret_position_from_point(x: f32, y: f32) -> Value {
    let Some(hit) = deepest_node_at_point(document_element_id(), x, y) else {
        return Value::Null;
    };
    let offset_node = first_text_descendant(hit).unwrap_or(hit);
    let text_length = dom::get_text_content(offset_node)
        .unwrap_or_default()
        .encode_utf16()
        .count();
    let rect = dom::bounding_rect(hit);
    let offset = if text_length == 0 || rect.width <= 0.0 {
        0
    } else {
        (((x - rect.left()) / rect.width).clamp(0.0, 1.0) * text_length as f32).round() as usize
    };
    let value = Value::object(HashMap::from([
        ("offsetNode".into(), element_value(offset_node)),
        ("offset".into(), Value::Number(offset as f64)),
    ]));
    value.set_property(
        "getClientRect",
        func(move |_, _| rect_value(dom::bounding_rect(hit))),
    );
    w3cos_core::class::set_prototype_of(&value, &caret_position_class().get_property("prototype"));
    value
}

fn is_svg_node(node: u32) -> bool {
    get_expando(node, "namespaceURI")
        .is_some_and(|namespace| namespace.to_js_string() == "http://www.w3.org/2000/svg")
}

fn svg_number_attribute(node: u32, name: &str, default: f64) -> f64 {
    dom::get_attribute(node, name)
        .and_then(|value| value.trim().trim_end_matches("px").parse::<f64>().ok())
        .unwrap_or(default)
}

fn svg_animated_length(node: u32, name: &'static str) -> Value {
    let current = svg_number_attribute(node, name, 0.0);
    let length = Value::object(HashMap::from([
        ("value".to_string(), Value::Number(current)),
        (
            "valueAsString".to_string(),
            Value::string(&dom::get_attribute(node, name).unwrap_or_else(|| current.to_string())),
        ),
        ("valueInSpecifiedUnits".to_string(), Value::Number(current)),
        ("unitType".to_string(), Value::Number(1.0)),
    ]));
    Value::object(HashMap::from([
        ("baseVal".to_string(), length.clone()),
        ("animVal".to_string(), length),
    ]))
}

fn svg_bbox(node: u32) -> w3cos_dom::DOMRect {
    match dom::tag_name(node).as_str() {
        "rect" | "image" | "use" => w3cos_dom::DOMRect::new(
            svg_number_attribute(node, "x", 0.0) as f32,
            svg_number_attribute(node, "y", 0.0) as f32,
            svg_number_attribute(node, "width", 0.0).max(0.0) as f32,
            svg_number_attribute(node, "height", 0.0).max(0.0) as f32,
        ),
        "circle" => {
            let r = svg_number_attribute(node, "r", 0.0).max(0.0) as f32;
            w3cos_dom::DOMRect::new(
                svg_number_attribute(node, "cx", 0.0) as f32 - r,
                svg_number_attribute(node, "cy", 0.0) as f32 - r,
                r * 2.0,
                r * 2.0,
            )
        }
        "ellipse" => {
            let rx = svg_number_attribute(node, "rx", 0.0).max(0.0) as f32;
            let ry = svg_number_attribute(node, "ry", 0.0).max(0.0) as f32;
            w3cos_dom::DOMRect::new(
                svg_number_attribute(node, "cx", 0.0) as f32 - rx,
                svg_number_attribute(node, "cy", 0.0) as f32 - ry,
                rx * 2.0,
                ry * 2.0,
            )
        }
        "line" => {
            let x1 = svg_number_attribute(node, "x1", 0.0) as f32;
            let y1 = svg_number_attribute(node, "y1", 0.0) as f32;
            let x2 = svg_number_attribute(node, "x2", 0.0) as f32;
            let y2 = svg_number_attribute(node, "y2", 0.0) as f32;
            w3cos_dom::DOMRect::new(x1.min(x2), y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs())
        }
        "svg" => w3cos_dom::DOMRect::new(
            0.0,
            0.0,
            svg_number_attribute(node, "width", 300.0) as f32,
            svg_number_attribute(node, "height", 150.0) as f32,
        ),
        _ => dom::bounding_rect(node),
    }
}

fn svg_identity_matrix() -> Value {
    crate::geometry_web::identity_matrix()
}

fn owner_svg_element(node: u32) -> Value {
    let mut current = dom::parent_node(node);
    while let Some(parent) = current {
        if is_svg_node(parent) && dom::tag_name(parent) == "svg" {
            return element_value(parent);
        }
        current = dom::parent_node(parent);
    }
    Value::Null
}

fn svg_computed_get(node: u32, key: &str) -> Option<Value> {
    if !is_svg_node(node) {
        return None;
    }
    let value = match key {
        "tagName" => Value::string(&dom::tag_name(node)),
        "ownerSVGElement" | "viewportElement" => owner_svg_element(node),
        "className" => {
            let class_name = dom::class_name(node);
            Value::object(HashMap::from([
                ("baseVal".to_string(), Value::string(&class_name)),
                ("animVal".to_string(), Value::string(&class_name)),
            ]))
        }
        "x" | "y" | "x1" | "y1" | "x2" | "y2" | "cx" | "cy" | "r" | "rx" | "ry" | "width"
        | "height" => {
            let name: &'static str = match key {
                "x" => "x",
                "y" => "y",
                "x1" => "x1",
                "y1" => "y1",
                "x2" => "x2",
                "y2" => "y2",
                "cx" => "cx",
                "cy" => "cy",
                "r" => "r",
                "rx" => "rx",
                "ry" => "ry",
                "width" => "width",
                _ => "height",
            };
            svg_animated_length(node, name)
        }
        "viewBox" => {
            let parts = dom::get_attribute(node, "viewBox")
                .or_else(|| dom::get_attribute(node, "viewbox"))
                .unwrap_or_default()
                .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
                .filter_map(|part| part.parse::<f64>().ok())
                .collect::<Vec<_>>();
            let rect = Value::object(HashMap::from([
                (
                    "x".to_string(),
                    Value::Number(parts.first().copied().unwrap_or(0.0)),
                ),
                (
                    "y".to_string(),
                    Value::Number(parts.get(1).copied().unwrap_or(0.0)),
                ),
                (
                    "width".to_string(),
                    Value::Number(parts.get(2).copied().unwrap_or(0.0)),
                ),
                (
                    "height".to_string(),
                    Value::Number(parts.get(3).copied().unwrap_or(0.0)),
                ),
            ]));
            Value::object(HashMap::from([
                ("baseVal".to_string(), rect.clone()),
                ("animVal".to_string(), rect),
            ]))
        }
        "getBBox" => func(move |_, _| rect_value(svg_bbox(node))),
        "getCTM" | "getScreenCTM" => func(|_, _| svg_identity_matrix()),
        "getTotalLength" => func(move |_, _| {
            let length = match dom::tag_name(node).as_str() {
                "line" => {
                    let dx = svg_number_attribute(node, "x2", 0.0)
                        - svg_number_attribute(node, "x1", 0.0);
                    let dy = svg_number_attribute(node, "y2", 0.0)
                        - svg_number_attribute(node, "y1", 0.0);
                    dx.hypot(dy)
                }
                "circle" => std::f64::consts::TAU * svg_number_attribute(node, "r", 0.0),
                "rect" => {
                    2.0 * (svg_number_attribute(node, "width", 0.0)
                        + svg_number_attribute(node, "height", 0.0))
                }
                _ => 0.0,
            };
            Value::Number(length)
        }),
        "createSVGPoint" => func(|_, _| crate::geometry_web::point(0.0, 0.0, 0.0, 1.0)),
        "createSVGMatrix" => func(|_, _| svg_identity_matrix()),
        "createSVGRect" => func(|_, _| rect_value(w3cos_dom::DOMRect::zero())),
        _ => return None,
    };
    Some(value)
}

fn is_inputish(node: u32) -> bool {
    matches!(
        dom::tag_name(node).as_str(),
        "input" | "textarea" | "select" | "option"
    )
}

#[derive(Default)]
struct ValiditySnapshot {
    bad_input: bool,
    custom_error: bool,
    pattern_mismatch: bool,
    range_overflow: bool,
    range_underflow: bool,
    step_mismatch: bool,
    too_long: bool,
    too_short: bool,
    type_mismatch: bool,
    value_missing: bool,
}

impl ValiditySnapshot {
    fn valid(&self) -> bool {
        !(self.bad_input
            || self.custom_error
            || self.pattern_mismatch
            || self.range_overflow
            || self.range_underflow
            || self.step_mismatch
            || self.too_long
            || self.too_short
            || self.type_mismatch
            || self.value_missing)
    }
}

fn constraint_validation_candidate(node: u32) -> bool {
    let tag = dom::tag_name(node);
    if !matches!(tag.as_str(), "input" | "textarea" | "select") {
        return false;
    }
    if dom::has_attribute(node, "disabled") || dom::has_attribute(node, "readonly") {
        return false;
    }
    if tag == "input" {
        let input_type = dom::get_attribute(node, "type")
            .unwrap_or_else(|| "text".into())
            .to_ascii_lowercase();
        if matches!(
            input_type.as_str(),
            "hidden" | "button" | "reset" | "submit" | "image"
        ) {
            return false;
        }
    }
    true
}

fn validity_snapshot(node: u32) -> ValiditySnapshot {
    if !constraint_validation_candidate(node) {
        return ValiditySnapshot::default();
    }
    let value = dom::get_attribute(node, "value").unwrap_or_default();
    let input_type = dom::get_attribute(node, "type")
        .unwrap_or_else(|| "text".into())
        .to_ascii_lowercase();
    let value_missing = dom::has_attribute(node, "required") && value.is_empty();
    let type_mismatch = !value.is_empty()
        && match input_type.as_str() {
            "email" => {
                let (local, domain) = value.split_once('@').unwrap_or_default();
                local.is_empty()
                    || domain.is_empty()
                    || domain.starts_with('.')
                    || domain.ends_with('.')
            }
            "url" => {
                let lower = value.to_ascii_lowercase();
                !(lower.starts_with("http://")
                    || lower.starts_with("https://")
                    || lower.starts_with("ftp://"))
            }
            _ => false,
        };
    let utf16_len = value.encode_utf16().count();
    let too_short = !value.is_empty()
        && dom::get_attribute(node, "minlength")
            .and_then(|length| length.parse::<usize>().ok())
            .is_some_and(|minimum| utf16_len < minimum);
    let too_long = dom::get_attribute(node, "maxlength")
        .and_then(|length| length.parse::<usize>().ok())
        .is_some_and(|maximum| utf16_len > maximum);
    let numeric_value = value.parse::<f64>().ok();
    let range_underflow = numeric_value.is_some_and(|number| {
        dom::get_attribute(node, "min")
            .and_then(|minimum| minimum.parse::<f64>().ok())
            .is_some_and(|minimum| number < minimum)
    });
    let range_overflow = numeric_value.is_some_and(|number| {
        dom::get_attribute(node, "max")
            .and_then(|maximum| maximum.parse::<f64>().ok())
            .is_some_and(|maximum| number > maximum)
    });
    let step_mismatch = numeric_value.is_some_and(|number| {
        let Some(step) = dom::get_attribute(node, "step")
            .filter(|step| step != "any")
            .and_then(|step| step.parse::<f64>().ok())
            .filter(|step| *step > 0.0)
        else {
            return false;
        };
        let base = dom::get_attribute(node, "min")
            .and_then(|minimum| minimum.parse::<f64>().ok())
            .unwrap_or(0.0);
        let quotient = (number - base) / step;
        (quotient - quotient.round()).abs() > 1e-9
    });
    let pattern_mismatch = if !value.is_empty() && dom::has_attribute(node, "pattern") {
        static WARNING: std::sync::Once = std::sync::Once::new();
        WARNING.call_once(|| {
            eprintln!(
                "[w3cos] warning: HTML pattern constraint parsing is not yet available; \
                 ValidityState.patternMismatch remains false"
            );
        });
        false
    } else {
        false
    };
    ValiditySnapshot {
        custom_error: get_expando(node, "__validation_message")
            .is_some_and(|message| !message.to_js_string().is_empty()),
        pattern_mismatch,
        range_overflow,
        range_underflow,
        step_mismatch,
        too_long,
        too_short,
        type_mismatch,
        value_missing,
        ..Default::default()
    }
}

fn validity_state_value(node: u32) -> Value {
    if let Some(value) = get_expando(node, "validity") {
        return value;
    }
    let value = Value::object(HashMap::new());
    let getters: [(&str, fn(&ValiditySnapshot) -> bool); 11] = [
        ("badInput", |state: &ValiditySnapshot| state.bad_input),
        ("customError", |state: &ValiditySnapshot| state.custom_error),
        ("patternMismatch", |state: &ValiditySnapshot| {
            state.pattern_mismatch
        }),
        ("rangeOverflow", |state: &ValiditySnapshot| {
            state.range_overflow
        }),
        ("rangeUnderflow", |state: &ValiditySnapshot| {
            state.range_underflow
        }),
        ("stepMismatch", |state: &ValiditySnapshot| {
            state.step_mismatch
        }),
        ("tooLong", |state: &ValiditySnapshot| state.too_long),
        ("tooShort", |state: &ValiditySnapshot| state.too_short),
        ("typeMismatch", |state: &ValiditySnapshot| {
            state.type_mismatch
        }),
        ("valid", |state: &ValiditySnapshot| state.valid()),
        ("valueMissing", |state: &ValiditySnapshot| {
            state.value_missing
        }),
    ];
    for (property, getter) in getters {
        value.set_property(
            &format!("__w3cos_getter_{property}"),
            func(move |_, _| Value::Bool(getter(&validity_snapshot(node)))),
        );
    }
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::dom_constructors::prototype("ValidityState"),
    );
    set_expando(node, "validity", value.clone());
    value
}

fn validation_message(node: u32) -> String {
    let state = validity_snapshot(node);
    if state.valid() {
        return String::new();
    }
    if state.custom_error {
        return get_expando(node, "__validation_message")
            .map(|message| message.to_js_string())
            .unwrap_or_default();
    }
    if state.value_missing {
        return "Please fill out this field.".into();
    }
    if state.type_mismatch {
        return "Please enter a valid value.".into();
    }
    if state.too_short || state.too_long {
        return "Please use the requested length.".into();
    }
    if state.range_underflow || state.range_overflow || state.step_mismatch {
        return "Please enter a valid value within the requested range.".into();
    }
    "The value is invalid.".into()
}

fn validate_control(node: u32, report: bool) -> bool {
    let valid = validity_snapshot(node).valid();
    if !valid {
        let event = w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![
                Value::string("invalid"),
                Value::object(HashMap::from([("cancelable".into(), Value::Bool(true))])),
            ],
        );
        let _ = js_dispatch_event(node, event);
        if report {
            static WARNING: std::sync::Once = std::sync::Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: reportValidity() dispatches invalid events, but native \
                     validation UI requires a host adapter"
                );
            });
        }
    }
    valid
}

fn form_controls(node: u32, controls: &mut Vec<u32>) {
    for child in dom::children(node) {
        if constraint_validation_candidate(child) {
            controls.push(child);
        }
        form_controls(child, controls);
    }
}

fn validate_form(node: u32, report: bool) -> bool {
    let mut controls = Vec::new();
    form_controls(node, &mut controls);
    controls.into_iter().fold(true, |valid, control| {
        validate_control(control, report) && valid
    })
}

fn form_owner_value(node: u32) -> Value {
    if let Some(form_id) = dom::get_attribute(node, "form").filter(|id| !id.is_empty()) {
        return inclusive_descendant_elements(document_element_id())
            .into_iter()
            .find(|candidate| {
                dom::tag_name(*candidate).eq_ignore_ascii_case("form")
                    && dom::get_attribute(*candidate, "id").as_deref() == Some(form_id.as_str())
            })
            .map(element_value)
            .unwrap_or(Value::Null);
    }
    let mut ancestor = dom::parent_node(node);
    while let Some(candidate) = ancestor {
        if dom::tag_name(candidate).eq_ignore_ascii_case("form") {
            return element_value(candidate);
        }
        ancestor = dom::parent_node(candidate);
    }
    Value::Null
}

fn select_owner(option: u32) -> Option<u32> {
    let mut ancestor = dom::parent_node(option);
    while let Some(candidate) = ancestor {
        if dom::tag_name(candidate).eq_ignore_ascii_case("select") {
            return Some(candidate);
        }
        ancestor = dom::parent_node(candidate);
    }
    None
}

fn select_options(select: u32) -> Vec<u32> {
    descendant_elements(select)
        .into_iter()
        .filter(|candidate| dom::tag_name(*candidate).eq_ignore_ascii_case("option"))
        .collect()
}

fn explicit_option_selectedness(option: u32) -> Option<bool> {
    get_expando(option, "selected")
        .map(|selected| selected.to_bool())
        .or_else(|| dom::has_attribute(option, "selected").then_some(true))
}

fn option_is_selected(option: u32) -> bool {
    if let Some(selected) = explicit_option_selectedness(option) {
        return selected;
    }
    let Some(select) = select_owner(option) else {
        return false;
    };
    if dom::has_attribute(select, "multiple") {
        return false;
    }
    let options = select_options(select);
    !options
        .iter()
        .any(|candidate| explicit_option_selectedness(*candidate) == Some(true))
        && options.first().copied() == Some(option)
}

fn set_option_selectedness(option: u32, selected: bool) {
    set_expando(option, "selected", Value::Bool(selected));
    if !selected {
        return;
    }
    let Some(select) = select_owner(option) else {
        return;
    };
    if dom::has_attribute(select, "multiple") {
        return;
    }
    for candidate in select_options(select) {
        if candidate != option {
            set_expando(candidate, "selected", Value::Bool(false));
        }
    }
}

fn preserve_moved_option_selectedness(root: u32) {
    for option in inclusive_descendant_elements(root)
        .into_iter()
        .filter(|candidate| dom::tag_name(*candidate).eq_ignore_ascii_case("option"))
    {
        if option_is_selected(option) {
            set_expando(option, "selected", Value::Bool(true));
        }
    }
}

fn select_selected_index(select: u32) -> i64 {
    select_options(select)
        .into_iter()
        .position(option_is_selected)
        .map_or(-1, |index| index as i64)
}

fn scroll_alignment_delta(
    target_start: f32,
    target_size: f32,
    viewport_start: f32,
    viewport_size: f32,
    alignment: &str,
) -> f32 {
    match alignment {
        "center" => target_start + target_size / 2.0 - (viewport_start + viewport_size / 2.0),
        "end" => target_start + target_size - (viewport_start + viewport_size),
        "nearest" => {
            if target_start < viewport_start {
                target_start - viewport_start
            } else if target_start + target_size > viewport_start + viewport_size {
                target_start + target_size - (viewport_start + viewport_size)
            } else {
                0.0
            }
        }
        _ => target_start - viewport_start,
    }
}

fn scroll_container_axes(node: u32) -> (bool, bool) {
    dom::with_document(|document| {
        let style = Element::new(NodeId::from_u32(node)).get_computed_style(document);
        (
            style.get_property("overflow-x") != "visible",
            style.get_property("overflow-y") != "visible",
        )
    })
}

fn scroll_element_into_view(node: u32, options: Value) {
    let (block, inline, nearest_only) = if options.is_object() {
        let behavior = options.get_property("behavior").to_js_string();
        if behavior == "smooth" {
            SMOOTH_SCROLL_WARNED.with(|warned| {
                if !warned.replace(true) {
                    eprintln!(
                        "[w3cos] warning: smooth scrollIntoView animation is unavailable; \
                         applying the final scroll position immediately"
                    );
                }
            });
        }
        let block = options.get_property("block").to_js_string();
        let inline = options.get_property("inline").to_js_string();
        (
            if block.is_empty() {
                "start".to_string()
            } else {
                block
            },
            if inline.is_empty() {
                "nearest".to_string()
            } else {
                inline
            },
            options.get_property("container").to_js_string() == "nearest",
        )
    } else {
        (
            if options.as_bool() == Some(false) {
                "end".to_string()
            } else {
                "start".to_string()
            },
            "nearest".to_string(),
            false,
        )
    };

    let mut target = dom::bounding_rect(node);
    let mut parent = dom::parent_node(node);
    let mut scrolled_container = false;
    while let Some(ancestor) = parent {
        let (scroll_x, scroll_y) = scroll_container_axes(ancestor);
        if scroll_x || scroll_y {
            let viewport = dom::bounding_rect(ancestor);
            let (current_left, current_top) = dom::get_scroll_offset(ancestor);
            let dx = scroll_x.then(|| {
                scroll_alignment_delta(target.x, target.width, viewport.x, viewport.width, &inline)
            });
            let dy = scroll_y.then(|| {
                scroll_alignment_delta(target.y, target.height, viewport.y, viewport.height, &block)
            });
            let new_left = dx.map(|delta| current_left + delta);
            let new_top = dy.map(|delta| current_top + delta);
            let changed = new_left.is_some_and(|value| value != current_left)
                || new_top.is_some_and(|value| value != current_top);
            if changed {
                dom::set_scroll_offset(ancestor, new_left, new_top);
                dispatch_sync(ancestor, EventType::Scroll, EventData::None);
                target.x -= dx.unwrap_or(0.0);
                target.y -= dy.unwrap_or(0.0);
            }
            scrolled_container = true;
            if nearest_only {
                break;
            }
        }
        parent = dom::parent_node(ancestor);
    }

    if !nearest_only || !scrolled_container {
        let (width, height, _) = VIEWPORT.with(Cell::get);
        let dx = scroll_alignment_delta(target.x, target.width, 0.0, width as f32, &inline);
        let dy = scroll_alignment_delta(target.y, target.height, 0.0, height as f32, &block);
        if dx != 0.0 || dy != 0.0 {
            let (left, top) = WINDOW_SCROLL.with(Cell::get);
            WINDOW_SCROLL.with(|offset| offset.set((left + dx as f64, top + dy as f64)));
            if let Some(window) = WINDOW_VALUE.with(|value| value.borrow().clone()) {
                let event = w3cos_core::class::construct(
                    &crate::web_events::event_class(),
                    vec![Value::string("scroll")],
                );
                window
                    .get_property("visualViewport")
                    .call_method("dispatchEvent", vec![event]);
            }
        }
    }
}

fn forced_scroll_size(node: u32) -> (f64, f64) {
    let live = dom::with_document(|document| {
        let element = Element::new(NodeId::from_u32(node));
        (
            element.scroll_width(document) as f64,
            element.scroll_height(document) as f64,
        )
    });
    if live.0 > 0.0 || live.1 > 0.0 {
        return live;
    }

    // CSSOM layout reads are synchronous in browsers. React relies on that
    // when it reads scrollHeight in an effect that can run before our first
    // native paint, so compute an ephemeral layout rather than exposing zero.
    let root = dom::to_component_tree();
    let flat = crate::layout::pre_flatten(&root);
    let Some(target_index) = flat.iter().position(|info| {
        matches!(
            info.on_click,
            w3cos_std::EventAction::NativeHost { id, .. } if *id == u64::from(node)
        )
    }) else {
        return live;
    };
    let (viewport_width, viewport_height, _) = viewport();
    let Ok((layouts, scrollable, _)) =
        crate::layout::compute_with_scroll(&root, viewport_width as f32, viewport_height as f32)
    else {
        return live;
    };
    let Some((rect, _)) = layouts.iter().find(|(_, index)| *index == target_index) else {
        return live;
    };
    let extent = scrollable
        .iter()
        .find(|(index, _, _)| *index == target_index)
        .map(|(_, _, extent)| *extent);
    (
        f64::from(rect.width + extent.map_or(0.0, |value| value.max_x)),
        f64::from(rect.height + extent.map_or(0.0, |value| value.max_y)),
    )
}

fn forced_bounding_rect(node: u32) -> w3cos_dom::DOMRect {
    let live = dom::bounding_rect(node);
    if !dom::is_document_dirty()
        && (live.width != 0.0 || live.height != 0.0 || live.x != 0.0 || live.y != 0.0)
    {
        return apply_css_motion_to_rect(node, live);
    }

    // Geometry APIs synchronously flush pending style and layout in browsers.
    // Compute a transient tree here so script reads observe parser styles and
    // inline mutations even before the native render loop's next frame.
    let root = dom::to_component_tree();
    let flat = crate::layout::pre_flatten(&root);
    let Some(target_index) = flat.iter().position(|info| {
        matches!(
            info.on_click,
            w3cos_std::EventAction::NativeHost { id, .. } if *id == u64::from(node)
        )
    }) else {
        return live;
    };
    let (viewport_width, viewport_height, _) = viewport();
    let Ok(layouts) = crate::layout::compute(
        &root,
        viewport_width as f32,
        viewport_height as f32,
    ) else {
        return live;
    };
    let rect = layouts
        .into_iter()
        .find(|(_, index)| *index == target_index)
        .map_or(live, |(rect, _)| {
            w3cos_dom::DOMRect::new(rect.x, rect.y, rect.width, rect.height)
        });
    apply_css_motion_to_rect(node, rect)
}

fn apply_css_motion_to_rect(node: u32, mut rect: w3cos_dom::DOMRect) -> w3cos_dom::DOMRect {
    if let Some(CssMotionValue::Length(sampled)) = sampled_css_motion_value(node, None, "left") {
        let final_left = CSS_MOTIONS.with(|motions| {
            motions
                .borrow()
                .iter()
                .rev()
                .find(|motion| {
                    motion.node == node
                        && motion.pseudo.is_none()
                        && motion.property == "left"
                })
                .and_then(|motion| match motion.to {
                    CssMotionValue::Length(value) => Some(value),
                    _ => None,
                })
                .unwrap_or_default()
        });
        rect.x += sampled - final_left;
    }
    if let Some(CssMotionValue::TranslateX(sampled)) =
        sampled_css_motion_value(node, None, "transform")
    {
        rect.x += sampled;
    }
    rect
}

fn element_computed_get(node: u32, key: &str) -> Value {
    if let Some(value) = svg_computed_get(node, key) {
        return value;
    }
    match key {
        // ── Node identity ──
        "nodeType" => Value::Number(dom::node_type(node) as f64),
        "nodeName" => {
            if dom::node_type(node) == 1 {
                Value::string(&element_qualified_name(node))
            } else {
                Value::string(&dom::node_name(node))
            }
        }
        "localName" => {
            if dom::node_type(node) == 1 {
                get_expando(node, "localName")
                    .unwrap_or_else(|| Value::string(&dom::tag_name(node)))
            } else {
                Value::Undefined
            }
        }
        "prefix" => {
            if dom::node_type(node) == 1 {
                get_expando(node, "prefix").unwrap_or(Value::Null)
            } else {
                Value::Undefined
            }
        }
        "tagName" => {
            if dom::node_type(node) == 1 {
                Value::string(&element_qualified_name(node))
            } else {
                Value::Undefined
            }
        }
        "namespaceURI" => get_expando(node, "namespaceURI").unwrap_or_else(|| {
            if dom::node_type(node) == 1 {
                Value::string(crate::html_parser_state::HTML_NAMESPACE)
            } else {
                Value::Null
            }
        }),
        "ownerDocument" => get_expando(node, "ownerDocument").unwrap_or_else(document_value),
        "baseURI" => get_expando(node, "ownerDocument")
            .unwrap_or_else(document_value)
            .get_property("URL"),
        "isConnected" => Value::Bool(node_is_connected(node)),
        "contentDocument" | "contentWindow" if dom::tag_name(node) == "iframe" => {
            #[cfg(feature = "dynamic-js")]
            let can_create_context =
                !crate::dynamic_script::frame_post_insertion_pending(node);
            #[cfg(not(feature = "dynamic-js"))]
            let can_create_context = true;
            if can_create_context {
                ensure_frame_browsing_context(node);
            }
            get_expando(node, key).unwrap_or(Value::Null)
        }
        "buffered" | "played" | "seekable"
            if matches!(dom::tag_name(node).as_str(), "audio" | "video") =>
        {
            crate::compat_web::time_ranges_value(Vec::new())
        }
        "error" if matches!(dom::tag_name(node).as_str(), "audio" | "video") => Value::Null,
        "networkState" if matches!(dom::tag_name(node).as_str(), "audio" | "video") => {
            get_expando(node, "networkState").unwrap_or(Value::Number(0.0))
        }
        "textTracks" if matches!(dom::tag_name(node).as_str(), "audio" | "video") => {
            if let Some(list) = get_expando(node, "textTracks") {
                list
            } else {
                let list = crate::text_tracks_web::text_track_list_value();
                set_expando(node, "textTracks", list.clone());
                list
            }
        }
        "addTextTrack" if matches!(dom::tag_name(node).as_str(), "audio" | "video") => {
            func(move |_, args| {
                let track = crate::text_tracks_web::text_track_value(
                    &arg(&args, 0).to_js_string(),
                    &arg(&args, 1).to_js_string(),
                    &arg(&args, 2).to_js_string(),
                );
                let list = element_computed_get(node, "textTracks");
                crate::text_tracks_web::append_track(&list, track.clone());
                track
            })
        }
        "getVideoPlaybackQuality" if dom::tag_name(node) == "video" => {
            func(|_, _| crate::text_tracks_web::playback_quality_value())
        }
        "attachInternals" if dom::node_type(node) == 1 => func(move |_, _| {
            if !dom::tag_name(node).contains('-') {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "ElementInternals are only available to custom elements",
                    "NotSupportedError",
                ));
            }
            if get_expando(node, "ElementInternals").is_some() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "attachInternals() may only be called once",
                    "NotSupportedError",
                ));
            }
            let internals =
                crate::custom_elements_web::element_internals_value(element_value(node), node);
            set_expando(node, "ElementInternals", internals.clone());
            internals
        }),
        "pseudo" if dom::node_type(node) == 1 => func(move |_, args| {
            let element = element_value(node);
            crate::custom_elements_web::css_pseudo_element_value(
                element.clone(),
                element,
                arg(&args, 0).to_js_string(),
            )
        }),
        #[cfg(feature = "web-media-advanced")]
        "audioPlaybackStats" if matches!(dom::tag_name(node).as_str(), "audio" | "video") => {
            if let Some(stats) = get_expando(node, "audioPlaybackStats") {
                stats
            } else {
                let stats = crate::media_devices_web::media_stats_value("AudioPlaybackStats");
                set_expando(node, "audioPlaybackStats", stats.clone());
                stats
            }
        }
        "remote" if matches!(dom::tag_name(node).as_str(), "audio" | "video") => {
            if let Some(remote) = get_expando(node, "remote") {
                remote
            } else {
                let remote = crate::compat_web::remote_playback_value();
                set_expando(node, "remote", remote.clone());
                remote
            }
        }
        "requestPictureInPicture" if dom::tag_name(node) == "video" => func(|_, _| {
            warn_host_api(
                "HTMLVideoElement.requestPictureInPicture()",
                "rejected NotSupportedError Promise",
            );
            w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                "Picture-in-Picture requires native multi-window video integration",
                "NotSupportedError",
            )])
        }),
        "animate" if dom::node_type(node) == 1 => func(move |_, args| {
            crate::animations_web::animate_element(
                element_value(node),
                arg(&args, 0),
                arg(&args, 1),
                node,
            )
        }),
        "getAnimations" if dom::node_type(node) == 1 => func(move |_, options| {
            let subtree = options
                .first()
                .is_some_and(|options| options.get_property("subtree").to_bool());
            crate::animations_web::animations_for(Some(node), subtree)
        }),
        "showPopover" if dom::node_type(node) == 1 => func(move |_, _| {
            set_popover_open(node, true);
            Value::Undefined
        }),
        "hidePopover" if dom::node_type(node) == 1 => func(move |_, _| {
            set_popover_open(node, false);
            Value::Undefined
        }),
        "togglePopover" if dom::node_type(node) == 1 => func(move |_, args| {
            let force = arg(&args, 0);
            let open = if force.is_undefined() {
                !popover_is_open(node)
            } else {
                force.to_bool()
            };
            set_popover_open(node, open);
            Value::Bool(open)
        }),
        "open" if dom::tag_name(node) == "dialog" => {
            Value::Bool(dom::has_attribute(node, "open"))
        }
        "returnValue" if dom::tag_name(node) == "dialog" => {
            get_expando(node, DIALOG_RETURN_VALUE_EXPANDO).unwrap_or_else(|| Value::string(""))
        }
        "show" if dom::tag_name(node) == "dialog" => func(move |_, _| {
            show_dialog(node, false);
            Value::Undefined
        }),
        "showModal" if dom::tag_name(node) == "dialog" => func(move |_, _| {
            show_dialog(node, true);
            Value::Undefined
        }),
        "close" if dom::tag_name(node) == "dialog" => func(move |_, args| {
            close_dialog(node, arg(&args, 0));
            Value::Undefined
        }),

        // ── Tree traversal ──
        "parentNode" => {
            get_expando(node, "parentNode").unwrap_or_else(|| match dom::parent_node(node) {
                Some(0) => document_value(),
                parent => element_or_null(parent),
            })
        }
        "parentElement" => match dom::parent_node(node) {
            Some(0) => Value::Null,
            Some(p) if dom::node_type(p) == 1 => element_value(p),
            _ => Value::Null,
        },
        "children" => html_collection(move || {
            child_elements(node)
                .into_iter()
                .map(element_value)
                .collect()
        }),
        "tBodies" if dom::tag_name(node) == "table" => html_collection(move || {
            child_elements_with_tags(node, &["tbody"])
                .into_iter()
                .map(element_value)
                .collect()
        }),
        "rows" if dom::tag_name(node) == "table" => {
            html_collection(move || table_rows(node).into_iter().map(element_value).collect())
        }
        "rows" if matches!(dom::tag_name(node).as_str(), "thead" | "tbody" | "tfoot") => {
            html_collection(move || {
                child_elements_with_tags(node, &["tr"])
                    .into_iter()
                    .map(element_value)
                    .collect()
            })
        }
        "cells" if dom::tag_name(node) == "tr" => html_collection(move || {
            child_elements_with_tags(node, &["td", "th"])
                .into_iter()
                .map(element_value)
                .collect()
        }),
        "deleteRow" if dom::tag_name(node) == "table" => func(move |_, args| {
            remove_indexed_table_row(table_rows(node), arg(&args, 0));
            Value::Undefined
        }),
        "deleteRow" if matches!(dom::tag_name(node).as_str(), "thead" | "tbody" | "tfoot") => {
            func(move |_, args| {
                remove_indexed_table_row(child_elements_with_tags(node, &["tr"]), arg(&args, 0));
                Value::Undefined
            })
        }
        "childNodes" => child_nodes_value(node),
        "childElementCount" => Value::Number(child_elements(node).len() as f64),
        "firstChild" => element_or_null(dom::first_child(node)),
        "lastChild" => element_or_null(dom::last_child(node)),
        "nextSibling" => {
            if is_global_document_child(node) {
                global_document_sibling(node, true)
            } else {
                get_expando(node, "nextSibling")
                    .unwrap_or_else(|| element_or_null(dom::next_sibling(node)))
            }
        }
        "previousSibling" => {
            if is_global_document_child(node) {
                global_document_sibling(node, false)
            } else {
                get_expando(node, "previousSibling")
                    .unwrap_or_else(|| element_or_null(dom::previous_sibling(node)))
            }
        }
        "firstElementChild" => element_or_null(first_element_child(node)),
        "lastElementChild" => element_or_null(child_elements(node).into_iter().last()),
        "nextElementSibling" => element_or_null(sibling_element(node, true)),
        "previousElementSibling" => element_or_null(sibling_element(node, false)),
        "hasChildNodes" => func(move |_, _| Value::Bool(dom::first_child(node).is_some())),
        "normalize" => func(move |_, _| {
            normalize_node_subtree(node);
            Value::Undefined
        }),
        "getRootNode" => func(move |_, args| {
            let composed = arg(&args, 0).get_property("composed").to_bool();
            root_node_value(node, composed)
        }),
        "contains" => func(move |_, args| {
            let Some(other) = node_id_of(&arg(&args, 0)) else {
                return Value::Bool(false);
            };
            Value::Bool(other == node || is_ancestor_of(node, other))
        }),
        "lookupNamespaceURI" => {
            func(move |_, args| lookup_namespace_uri_result(&element_value(node), &arg(&args, 0)))
        }
        "lookupPrefix" => {
            func(move |_, args| lookup_prefix_result(&element_value(node), &arg(&args, 0)))
        }
        "isDefaultNamespace" => {
            func(move |_, args| is_default_namespace_result(&element_value(node), &arg(&args, 0)))
        }
        "isSameNode" => {
            func(move |_, args| Value::Bool(node_id_of(&arg(&args, 0)) == Some(node)))
        }
        "isEqualNode" => func(move |_, args| {
            Value::Bool(nodes_are_equal(&element_value(node), &arg(&args, 0)))
        }),

        // ── Text content ──
        "textContent" => node_text_content(node),
        "nodeValue" | "data" => match dom::get_text_content(node) {
            Some(t) => Value::string(&t),
            None => Value::Null,
        },
        "length" if matches!(dom::node_type(node), 3 | 4 | 7 | 8) => Value::Number(
            dom::get_text_content(node)
                .unwrap_or_default()
                .encode_utf16()
                .count() as f64,
        ),
        "substringData" if matches!(dom::node_type(node), 3 | 4 | 7 | 8) => func(move |_, args| {
            if args.len() < 2 {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string("substringData requires 2 arguments")],
                ));
            }
            let units = dom::get_text_content(node)
                .unwrap_or_default()
                .encode_utf16()
                .collect::<Vec<_>>();
            let offset = arg(&args, 0).to_u32() as usize;
            if offset > units.len() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "CharacterData offset is outside the data",
                    "IndexSizeError",
                ));
            }
            let end = offset
                .saturating_add(arg(&args, 1).to_u32() as usize)
                .min(units.len());
            Value::string(&String::from_utf16_lossy(&units[offset..end]))
        }),
        "appendData" if matches!(dom::node_type(node), 3 | 4 | 7 | 8) => func(move |_, args| {
            if args.is_empty() {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string("appendData requires 1 argument")],
                ));
            }
            let mut data = dom::get_text_content(node).unwrap_or_default();
            data.push_str(&arg(&args, 0).to_js_string());
            dom::set_text_content(node, &data);
            Value::Undefined
        }),
        "insertData" | "deleteData" | "replaceData"
            if matches!(dom::node_type(node), 3 | 4 | 7 | 8) =>
        {
            let operation = key.to_string();
            func(move |_, args| {
                let mut units = dom::get_text_content(node)
                    .unwrap_or_default()
                    .encode_utf16()
                    .collect::<Vec<_>>();
                let offset = arg(&args, 0).to_u32() as usize;
                if offset > units.len() {
                    w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                        "CharacterData offset is outside the data",
                        "IndexSizeError",
                    ));
                }
                let (count_index, data_index) = if operation == "insertData" {
                    (None, 1)
                } else {
                    (Some(1), 2)
                };
                let end = count_index
                    .map(|index| {
                        offset
                            .saturating_add(arg(&args, index).to_u32() as usize)
                            .min(units.len())
                    })
                    .unwrap_or(offset);
                let replacement = if operation == "deleteData" {
                    Vec::new()
                } else {
                    arg(&args, data_index)
                        .to_js_string()
                        .encode_utf16()
                        .collect()
                };
                units.splice(offset..end, replacement);
                dom::set_text_content(node, &String::from_utf16_lossy(&units));
                Value::Undefined
            })
        }
        "wholeText" if matches!(dom::node_type(node), 3 | 4) => {
            let mut previous = Vec::new();
            let mut cursor = dom::previous_sibling(node);
            while let Some(sibling) = cursor {
                if !matches!(dom::node_type(sibling), 3 | 4) {
                    break;
                }
                previous.push(dom::get_text_content(sibling).unwrap_or_default());
                cursor = dom::previous_sibling(sibling);
            }
            previous.reverse();
            let mut text = previous.concat();
            text.push_str(&dom::get_text_content(node).unwrap_or_default());
            cursor = dom::next_sibling(node);
            while let Some(sibling) = cursor {
                if !matches!(dom::node_type(sibling), 3 | 4) {
                    break;
                }
                text.push_str(&dom::get_text_content(sibling).unwrap_or_default());
                cursor = dom::next_sibling(sibling);
            }
            Value::string(&text)
        }
        "assignedSlot" if matches!(dom::node_type(node), 1 | 3 | 4) => {
            element_or_null(assigned_slot_for_node(node))
        }
        "splitText" if matches!(dom::node_type(node), 3 | 4) => func(move |_, args| {
            let mut units = dom::get_text_content(node)
                .unwrap_or_default()
                .encode_utf16()
                .collect::<Vec<_>>();
            let offset = arg(&args, 0).to_u32() as usize;
            if offset > units.len() {
                w3cos_core::throw_value(w3cos_core::web::dom_exception_instance(
                    "Text offset is outside the data",
                    "IndexSizeError",
                ));
            }
            let trailing = units.split_off(offset);
            dom::set_text_content(node, &String::from_utf16_lossy(&units));
            let new_node = if dom::node_type(node) == 4 {
                dom::create_cdata_section(&String::from_utf16_lossy(&trailing))
            } else {
                dom::create_text_node(&String::from_utf16_lossy(&trailing))
            };
            if let Some(parent) = dom::parent_node(node) {
                match dom::next_sibling(node) {
                    Some(next) => dom::insert_before(parent, new_node, next),
                    None => dom::append_child(parent, new_node),
                }
            }
            element_value(new_node)
        }),
        "target" if dom::node_type(node) == 7 => Value::string(&dom::tag_name(node)),
        "sheet" if dom::node_type(node) == 7 => processing_instruction_sheet(node),
        "name" if dom::node_type(node) == 10 => Value::string(&dom::tag_name(node)),
        "publicId" | "systemId" if dom::node_type(node) == 10 => {
            get_expando(node, key).unwrap_or_else(|| Value::string(""))
        }
        "innerText" => Value::string(&dom::inner_text(node)),
        "innerHTML" => {
            let mut s = dom::get_text_content(node).unwrap_or_default();
            for c in dom::children(node) {
                s.push_str(&dom::outer_html(c));
            }
            Value::string(&s)
        }
        "outerHTML" => Value::string(&dom::outer_html(node)),
        "insertAdjacentElement" => func(move |_, args| {
            let element = arg(&args, 1);
            let Some(child) = node_id_of(&element) else {
                type_error("insertAdjacentElement requires an Element");
            };
            if dom::node_type(child) != 1 {
                type_error("insertAdjacentElement requires an Element");
            }
            if insert_adjacent_node(node, &arg(&args, 0).to_js_string(), child) {
                element
            } else {
                Value::Null
            }
        }),
        "insertAdjacentText" => func(move |_, args| {
            let child = dom::create_text_node(&arg(&args, 1).to_js_string());
            insert_adjacent_node(node, &arg(&args, 0).to_js_string(), child);
            Value::Undefined
        }),

        // ── Attributes ──
        "getAttribute" if dom::node_type(node) == 7 => func(move |_, args| {
            let name = arg(&args, 0).to_js_string();
            processing_instruction_attributes(node)
                .into_iter()
                .find(|(current, _)| current == &name)
                .map(|(_, value)| Value::string(&value))
                .unwrap_or(Value::Null)
        }),
        "getAttributeNames" if dom::node_type(node) == 7 => func(move |_, _| {
            js_array(
                processing_instruction_attributes(node)
                    .into_iter()
                    .map(|(name, _)| Value::string(&name))
                    .collect(),
            )
        }),
        "hasAttribute" if dom::node_type(node) == 7 => func(move |_, args| {
            let name = arg(&args, 0).to_js_string();
            Value::Bool(
                processing_instruction_attributes(node)
                    .iter()
                    .any(|(current, _)| current == &name),
            )
        }),
        "hasAttributes" if dom::node_type(node) == 7 => func(move |_, _| {
            Value::Bool(!processing_instruction_attributes(node).is_empty())
        }),
        "setAttribute" if dom::node_type(node) == 7 => func(move |_, args| {
            set_processing_instruction_attribute(
                node,
                &arg(&args, 0).to_js_string(),
                Some(arg(&args, 1).to_js_string()),
            );
            Value::Undefined
        }),
        "removeAttribute" if dom::node_type(node) == 7 => func(move |_, args| {
            set_processing_instruction_attribute(node, &arg(&args, 0).to_js_string(), None);
            Value::Undefined
        }),
        "toggleAttribute" if dom::node_type(node) == 7 => func(move |_, args| {
            let name = arg(&args, 0).to_js_string();
            if !valid_processing_instruction_attribute_name(&name) {
                dom_exception(
                    "Processing instruction attribute name is not valid",
                    "InvalidCharacterError",
                );
            }
            let has = processing_instruction_attributes(node)
                .iter()
                .any(|(current, _)| current == &name);
            let force = arg(&args, 1);
            let want = if force.is_undefined() {
                !has
            } else {
                force.to_bool()
            };
            if want && !has {
                set_processing_instruction_attribute(node, &name, Some(String::new()));
            } else if !want && has {
                set_processing_instruction_attribute(node, &name, None);
            }
            Value::Bool(want)
        }),
        "id" => Value::string(&dom::get_attribute(node, "id").unwrap_or_default()),
        "name" if exposes_window_name(node) => {
            Value::string(&dom::get_attribute(node, "name").unwrap_or_default())
        }
        "name" if dom::tag_name(node) == "slot" => {
            Value::string(&dom::get_attribute(node, "name").unwrap_or_default())
        }
        "className" => Value::string(&dom::class_name(node)),
        "classList" => class_list_value(node),
        "attributes" => attributes_value(node),
        "dataset" => dataset_value(node),
        "getAttributeNames" => func(move |_, _| {
            js_array(dom::with_document(|document| {
                document
                    .get_node(NodeId::from_u32(node))
                    .attributes
                    .iter()
                    .map(|(name, _)| Value::string(&name.as_str()))
                    .collect()
            }))
        }),
        "getAttribute" => func(move |_, args| {
            let name = normalized_attribute_name(node, &arg(&args, 0).to_js_string());
            match dom::get_attribute(node, &name) {
                Some(v) => Value::from(v),
                None => Value::Null,
            }
        }),
        "getAttributeNS" => func(move |_, args| {
            let namespace = normalized_namespace_argument(&arg(&args, 0));
            match dom::get_attribute_ns(node, namespace.as_deref(), &arg(&args, 1).to_js_string()) {
                Some(v) => Value::from(v),
                None => Value::Null,
            }
        }),
        "getAttributeNode" => func(move |_, args| {
            let name = normalized_attribute_name(node, &arg(&args, 0).to_js_string());
            attribute_node_by_qualified_name(node, &name)
        }),
        "getAttributeNodeNS" => func(move |_, args| {
            let namespace = normalized_namespace_argument(&arg(&args, 0));
            attribute_node_by_namespace(node, namespace.as_deref(), &arg(&args, 1).to_js_string())
        }),
        "setAttributeNode" | "setAttributeNodeNS" => func(move |_, args| {
            let attribute = arg(&args, 0);
            if attribute.get_property("nodeType").to_u32() != 2 {
                type_error("setAttributeNode requires an Attr node");
            }
            let current_owner = attribute.get_property("ownerElement");
            if !current_owner.is_null()
                && !current_owner.is_undefined()
                && node_id_of(&current_owner) != Some(node)
            {
                dom_exception("The attribute is already in use", "InUseAttributeError");
            }
            let qualified_name = attribute.get_property("name").to_js_string();
            let local_name = attribute.get_property("localName").to_js_string();
            let namespace = normalized_namespace_argument(&attribute.get_property("namespaceURI"));
            let prefix = normalized_namespace_argument(&attribute.get_property("prefix"));
            let previous = if namespace.is_some() {
                attribute_node_by_namespace(node, namespace.as_deref(), &local_name)
            } else {
                attribute_node_by_qualified_name(node, &qualified_name)
            };
            dom::set_attribute_ns_parts(
                node,
                namespace.as_deref(),
                &qualified_name,
                prefix.as_deref(),
                &local_name,
                &attribute.get_property("value").to_js_string(),
            );
            if !previous.is_null() && !previous.strict_eq(&attribute) {
                detach_attribute_value(node, &previous);
            }
            attribute.set_property("ownerElement", element_value(node));
            cache_attribute_value(
                node,
                namespace.as_deref(),
                &local_name,
                attribute.clone(),
            );
            previous
        }),
        "setAttribute" => func(move |_, args| {
            let requested_name = arg(&args, 0).to_js_string();
            if !valid_attribute_name(&requested_name) {
                dom_exception("Attribute name is not valid", "InvalidCharacterError");
            }
            let name = normalized_attribute_name(node, &requested_name);
            let value = arg(&args, 1).to_js_string();
            dom::set_attribute(node, &name, &value);
            #[cfg(feature = "dynamic-js")]
            if let Some(event_type) = name.strip_prefix("on").filter(|event| !event.is_empty()) {
                crate::dynamic_script::update_inline_event_handler(node, event_type, &value);
            }
            Value::Undefined
        }),
        "setAttributeNS" => func(move |_, args| {
            let namespace = normalized_namespace_argument(&arg(&args, 0));
            let qualified_name = arg(&args, 1).to_js_string();
            let (prefix, local_name) =
                validate_and_extract_qualified_name(namespace.as_deref(), &qualified_name);
            dom::set_attribute_ns_parts(
                node,
                namespace.as_deref(),
                &qualified_name,
                prefix.as_deref(),
                &local_name,
                &arg(&args, 2).to_js_string(),
            );
            Value::Undefined
        }),
        "hasAttribute" => func(move |_, args| {
            let name = normalized_attribute_name(node, &arg(&args, 0).to_js_string());
            Value::Bool(dom::has_attribute(node, &name))
        }),
        "hasAttributes" => func(move |_, _| {
            Value::Bool(dom::with_document(|document| {
                !document
                    .get_node(NodeId::from_u32(node))
                    .attributes
                    .is_empty()
            }))
        }),
        "hasAttributeNS" => func(move |_, args| {
            let namespace = normalized_namespace_argument(&arg(&args, 0));
            Value::Bool(dom::has_attribute_ns(
                node,
                namespace.as_deref(),
                &arg(&args, 1).to_js_string(),
            ))
        }),
        "removeAttribute" => func(move |_, args| {
            let name = normalized_attribute_name(node, &arg(&args, 0).to_js_string());
            let previous = attribute_node_by_qualified_name(node, &name);
            dom::remove_attribute(node, &name);
            if !previous.is_null() {
                detach_attribute_value(node, &previous);
            }
            Value::Undefined
        }),
        "removeAttributeNS" => func(move |_, args| {
            let namespace = normalized_namespace_argument(&arg(&args, 0));
            let local_name = arg(&args, 1).to_js_string();
            let previous =
                attribute_node_by_namespace(node, namespace.as_deref(), &local_name);
            dom::remove_attribute_ns(node, namespace.as_deref(), &local_name);
            if !previous.is_null() {
                detach_attribute_value(node, &previous);
            }
            Value::Undefined
        }),
        "removeAttributeNode" => func(move |_, args| {
            let attribute = arg(&args, 0);
            if attribute.get_property("nodeType").to_u32() != 2 {
                type_error("removeAttributeNode requires an Attr node");
            }
            if node_id_of(&attribute.get_property("ownerElement")) != Some(node) {
                dom_exception("The attribute is not owned by this element", "NotFoundError");
            }
            let namespace = normalized_namespace_argument(&attribute.get_property("namespaceURI"));
            let local_name = attribute.get_property("localName").to_js_string();
            let current = if namespace.is_some() {
                attribute_node_by_namespace(node, namespace.as_deref(), &local_name)
            } else {
                attribute_node_by_qualified_name(
                    node,
                    &attribute.get_property("name").to_js_string(),
                )
            };
            if current.is_null() || !current.strict_eq(&attribute) {
                dom_exception("The attribute is not owned by this element", "NotFoundError");
            }
            if namespace.is_some() {
                dom::remove_attribute_ns(node, namespace.as_deref(), &local_name);
            } else {
                dom::remove_attribute(node, &attribute.get_property("name").to_js_string());
            }
            detach_attribute_value(node, &attribute);
            attribute
        }),
        "toggleAttribute" => func(move |_, args| {
            let requested_name = arg(&args, 0).to_js_string();
            if !valid_attribute_name(&requested_name) {
                dom_exception("Attribute name is not valid", "InvalidCharacterError");
            }
            let name = normalized_attribute_name(node, &requested_name);
            let force = arg(&args, 1);
            let has = dom::has_attribute(node, &name);
            let want = if force.is_undefined() {
                !has
            } else {
                force.to_bool()
            };
            if want && !has {
                dom::set_attribute(node, &name, "");
            } else if !want && has {
                dom::remove_attribute(node, &name);
            }
            Value::Bool(want)
        }),
        "title" => Value::string(&dom::get_attribute(node, "title").unwrap_or_default()),
        "dir" => Value::string(&dom::get_attribute(node, "dir").unwrap_or_default()),
        "contentEditable" => Value::string(
            &dom::get_attribute(node, "contenteditable").unwrap_or_else(|| "inherit".into()),
        ),
        "editContext" if dom::node_type(node) == 1 => {
            get_expando(node, "editContext").unwrap_or(Value::Null)
        }
        "hidden" => Value::Bool(dom::has_attribute(node, "hidden")),
        "tabIndex" => Value::Number(
            dom::get_attribute(node, "tabindex")
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(-1.0),
        ),
        "lang" => Value::string(&dom::get_attribute(node, "lang").unwrap_or_default()),
        "draggable" => {
            Value::Bool(dom::get_attribute(node, "draggable").as_deref() == Some("true"))
        }
        "slot" => Value::string(&dom::get_attribute(node, "slot").unwrap_or_default()),

        // ── Style ──
        "style" => style_value(node),
        "attributeStyleMap" if dom::node_type(node) == 1 => typed_style_map_value(node, false),
        "computedStyleMap" if dom::node_type(node) == 1 => {
            func(move |_, _| typed_style_map_value(node, true))
        }

        // ── Tree mutation ──
        "appendChild" => func(move |_, args| {
            let child = arg(&args, 0);
            let Some(cid) = node_id_of(&child) else {
                if child.get_property("nodeType").to_u32() == 9 {
                    dom_exception("Documents cannot be inserted", "HierarchyRequestError");
                }
                type_error("appendChild requires a Node");
            };
            insert_child_or_fragment(node, cid, None);
            child
        }),
        "removeChild" => func(move |_, args| {
            let child = arg(&args, 0);
            let Some(cid) = node_id_of(&child) else {
                if child.get_property("nodeType").to_u32() == 9 {
                    dom_exception("The node is not a child of this parent", "NotFoundError");
                }
                type_error("removeChild requires a Node");
            };
            if dom::parent_node(cid) != Some(node) {
                dom_exception("The node is not a child of this parent", "NotFoundError");
            }
            let was_connected = dom::is_connected(cid);
            dom::remove_child(node, cid);
            release_element_subtree(cid);
            if was_connected {
                crate::custom_elements_web::disconnected_subtree(&child);
            }
            child
        }),
        "insertBefore" => func(move |_, args| {
            if args.len() < 2 {
                type_error("insertBefore requires 2 arguments");
            }
            let new_child = arg(&args, 0);
            let ref_child = arg(&args, 1);
            let inserted_node = node_id_of(&new_child);
            if inserted_node.is_none() && new_child.get_property("nodeType").to_u32() != 9 {
                type_error("insertBefore requires a Node as its first argument");
            }
            let reference = if ref_child.is_null() || ref_child.is_undefined() {
                None
            } else if let Some(reference) = node_id_of(&ref_child) {
                Some(reference)
            } else {
                type_error("insertBefore reference must be a Node, null, or undefined");
            };

            // DOM pre-insert validates the parent and inclusive-ancestor
            // relationship before checking whether the reference is a child,
            // then validates the inserted node's type and document position.
            if let Some(inserted_node) = inserted_node {
                ensure_tree_parent_and_ancestry(node, inserted_node);
            }
            if let Some(reference) = reference
                && dom::parent_node(reference) != Some(node)
            {
                dom_exception(
                    "The reference node is not a child of this parent",
                    "NotFoundError",
                );
            }
            let Some(nid) = inserted_node else {
                dom_exception("Documents cannot be inserted", "HierarchyRequestError");
            };
            ensure_tree_child_type_and_adopt(node, nid);
            insert_child_or_fragment(node, nid, reference);
            new_child
        }),
        "moveBefore" => {
            let method = func(move |_, args| {
                if args.len() < 2 {
                    type_error("moveBefore requires 2 arguments");
                }
                let moving = arg(&args, 0);
                let reference = arg(&args, 1);
                if !reference.is_null()
                    && !reference.is_undefined()
                    && !is_dom_node_value(&reference)
                {
                    type_error("moveBefore reference must be a Node, null, or undefined");
                }
                let Some(moving_id) = node_id_of(&moving) else {
                    if is_dom_node_value(&moving) {
                        dom_exception(
                            "Only Element and CharacterData nodes can be moved",
                            "HierarchyRequestError",
                        );
                    }
                    type_error("moveBefore requires a Node");
                };
                if !nodes_have_same_shadow_including_root(node, moving_id) {
                    dom_exception(
                        "The destination and moved node must have the same shadow-including root",
                        "HierarchyRequestError",
                    );
                }
                ensure_tree_parent_and_ancestry(node, moving_id);
                if !matches!(dom::node_type(moving_id), 1 | 3 | 4 | 7 | 8) {
                    dom_exception(
                        "Only Element and CharacterData nodes can be moved",
                        "HierarchyRequestError",
                    );
                }
                let reference_id = if reference.is_null() || reference.is_undefined() {
                    None
                } else {
                    let Some(reference_id) = node_id_of(&reference) else {
                        dom_exception(
                            "The reference node is not a child of this parent",
                            "NotFoundError",
                        );
                    };
                    if dom::parent_node(reference_id) != Some(node) {
                        dom_exception(
                            "The reference node is not a child of this parent",
                            "NotFoundError",
                        );
                    }
                    Some(reference_id)
                };
                if reference_id == Some(moving_id) {
                    return Value::Undefined;
                }
                let transition_before = (dom::node_type(moving_id) == 1)
                    .then(|| capture_transition_snapshots(moving_id));
                let was_connected = node_is_connected(moving_id);
                let old_parent = dom::parent_node(moving_id);
                let mut affected_slots = affected_slots_for_node_position(moving_id);
                preserve_moved_option_selectedness(moving_id);
                let destination_children = dom::children(node)
                    .into_iter()
                    .filter(|child| *child != moving_id)
                    .collect::<Vec<_>>();
                let insertion_index = reference_id
                    .and_then(|reference| {
                        destination_children
                            .iter()
                            .position(|child| *child == reference)
                    })
                    .unwrap_or(destination_children.len()) as u32;
                adjust_live_ranges_for_removal(moving_id);
                adjust_live_ranges_for_insertion(node, insertion_index);
                with_deferred_dom_post_insertion_steps(|| {
                    match reference_id {
                        Some(reference_id) => dom::insert_before(node, moving_id, reference_id),
                        None => dom::append_child(node, moving_id),
                    }
                });
                pin_element_subtree(moving_id);
                affected_slots.extend(affected_slots_for_node_position(moving_id));
                for slot in affected_slots {
                    queue_slotchange(slot);
                }
                if was_connected && node_is_connected(moving_id) {
                    crate::custom_elements_web::moved_subtree(&moving);
                }
                crate::dynamic_script::notify_picture_relevant_move(moving_id, old_parent, node);
                schedule_focus_revalidation_after_move(moving_id);
                if let Some(before) = transition_before {
                    let after = capture_transition_snapshots(moving_id);
                    start_changed_transitions(moving_id, &before, &after);
                }
                Value::Undefined
            });
            method.set_property("length", Value::Number(2.0));
            method
        }
        "replaceChild" => func(move |_, args| {
            if args.len() < 2 {
                type_error("replaceChild requires 2 arguments");
            }
            let new_child = arg(&args, 0);
            let old_child = arg(&args, 1);
            if !is_dom_node_value(&new_child) || !is_dom_node_value(&old_child) {
                type_error("replaceChild requires Node arguments");
            }
            let inserted_node = node_id_of(&new_child);
            if let Some(inserted_node) = inserted_node {
                ensure_tree_parent_and_ancestry(node, inserted_node);
            } else if matches!(dom::node_type(node), 3 | 4 | 7 | 8 | 10) {
                dom_exception(
                    "This node type cannot contain children",
                    "HierarchyRequestError",
                );
            }
            let Some(old_node) = node_id_of(&old_child) else {
                dom_exception("The node is not a child of this parent", "NotFoundError");
            };
            if dom::parent_node(old_node) != Some(node) {
                dom_exception("The node is not a child of this parent", "NotFoundError");
            }
            let Some(inserted_node) = inserted_node else {
                dom_exception("Documents cannot be inserted", "HierarchyRequestError");
            };
            ensure_tree_child_type_and_adopt(node, inserted_node);
            let old_was_connected = dom::is_connected(old_node);
            replace_child_or_fragment(node, inserted_node, old_node);
            release_element_subtree(old_node);
            if old_was_connected {
                crate::custom_elements_web::disconnected_subtree(&old_child);
            }
            if dom::node_type(inserted_node) != 11 && dom::is_connected(inserted_node) {
                crate::custom_elements_web::connected_subtree(&new_child);
            }
            old_child
        }),
        "cloneNode" => func(move |_, args| {
            let deep = arg(&args, 0).to_bool();
            let clone = dom::clone_node(node, deep);
            copy_cloned_node_identity(node, clone);
            element_value(clone)
        }),
        "remove" => func(move |_, _| {
            if let Some(parent) = dom::parent_node(node) {
                let element = element_value(node);
                let was_connected = dom::is_connected(node);
                dom::remove_child(parent, node);
                release_element_subtree(node);
                if was_connected {
                    crate::custom_elements_web::disconnected_subtree(&element);
                }
            } else if let Some(parent_document) = get_expando(node, "parentNode")
                && parent_document.get_property("nodeType").to_u32() == 9
            {
                let element = element_value(node);
                if parent_document
                    .get_property("__w3cos_document_children")
                    .as_array()
                    .is_some()
                {
                    let children = virtual_document_children(&parent_document)
                        .into_iter()
                        .filter(|child| !child.strict_eq(&element))
                        .collect();
                    set_virtual_document_children(&parent_document, children);
                } else if parent_document.get_property("doctype").strict_eq(&element) {
                    parent_document.set_property("doctype", Value::Null);
                    set_expando(node, "parentNode", Value::Null);
                }
            }
            Value::Undefined
        }),
        "append" | "prepend" => {
            let prepend = key == "prepend";
            func(move |_, args| {
                let reference = prepend.then(|| dom::first_child(node)).flatten();
                let mut inserted = Vec::new();
                with_deferred_dom_post_insertion_steps(|| {
                    for argument in args {
                        let child = if let Some(child) = node_id_of(&argument) {
                            ensure_tree_insertion(node, child);
                            blur_focus_for_standard_reparent(child);
                            child
                        } else {
                            dom::create_text_node(&argument.to_js_string())
                        };
                        match reference {
                            Some(reference) => dom::insert_before(node, child, reference),
                            None => dom::append_child(node, child),
                        }
                        inserted.push(child);
                    }
                });
                run_dom_batch_insertion_steps(&inserted);
                run_script_mutation_steps(node);
                for child in inserted {
                    run_dom_post_insertion_steps(child);
                }
                Value::Undefined
            })
        }
        "before" | "after" => {
            let before = key == "before";
            func(move |_, args| {
                let Some(parent) = dom::parent_node(node) else {
                    return Value::Undefined;
                };
                let argument_nodes = args.iter().filter_map(node_id_of).collect::<HashSet<_>>();
                if before {
                    let mut sibling = dom::previous_sibling(node);
                    while sibling.is_some_and(|id| argument_nodes.contains(&id)) {
                        sibling = sibling.and_then(dom::previous_sibling);
                    }
                    let mut previous = sibling;
                    for argument in args {
                        let child = if let Some(child) = node_id_of(&argument) {
                            ensure_tree_insertion(parent, child);
                            child
                        } else {
                            dom::create_text_node(&argument.to_js_string())
                        };
                        let reference = previous.and_then(dom::next_sibling).or_else(|| {
                            previous
                                .is_none()
                                .then(|| dom::first_child(parent))
                                .flatten()
                        });
                        match reference {
                            Some(reference) => dom::insert_before(parent, child, reference),
                            None => dom::append_child(parent, child),
                        }
                        previous = Some(child);
                    }
                } else {
                    let mut reference = dom::next_sibling(node);
                    while reference.is_some_and(|id| argument_nodes.contains(&id)) {
                        reference = reference.and_then(dom::next_sibling);
                    }
                    for argument in args {
                        let child = if let Some(child) = node_id_of(&argument) {
                            ensure_tree_insertion(parent, child);
                            child
                        } else {
                            dom::create_text_node(&argument.to_js_string())
                        };
                        match reference {
                            Some(reference) => dom::insert_before(parent, child, reference),
                            None => dom::append_child(parent, child),
                        }
                    }
                }
                Value::Undefined
            })
        }
        "replaceWith" => func(move |_, args| {
            if let Some(parent) = dom::parent_node(node) {
                let argument_nodes = args.iter().filter_map(node_id_of).collect::<HashSet<_>>();
                let keeps_context = argument_nodes.contains(&node);
                let reference = if keeps_context {
                    let mut sibling = dom::next_sibling(node);
                    while sibling.is_some_and(|id| argument_nodes.contains(&id)) {
                        sibling = sibling.and_then(dom::next_sibling);
                    }
                    sibling
                } else {
                    Some(node)
                };
                for argument in args {
                    let child = if let Some(child) = node_id_of(&argument) {
                        ensure_tree_insertion(parent, child);
                        child
                    } else {
                        dom::create_text_node(&argument.to_js_string())
                    };
                    match reference {
                        Some(reference_node) => dom::insert_before(parent, child, reference_node),
                        None => dom::append_child(parent, child),
                    }
                }
                if !keeps_context && dom::parent_node(node) == Some(parent) {
                    dom::remove_child(parent, node);
                }
            }
            Value::Undefined
        }),
        "replaceChildren" => func(move |_, args| {
            let replacements = args
                .into_iter()
                .map(|argument| {
                    node_id_of(&argument)
                        .unwrap_or_else(|| dom::create_text_node(&argument.to_js_string()))
                })
                .collect::<Vec<_>>();
            for replacement in &replacements {
                ensure_tree_insertion(node, *replacement);
            }
            for replacement in &replacements {
                if let Some(previous_parent) = dom::parent_node(*replacement)
                    && previous_parent != node
                {
                    dom::remove_child(previous_parent, *replacement);
                }
            }
            replace_all_children(node, || {
                for replacement in &replacements {
                    dom::append_child(node, *replacement);
                    pin_element_subtree(*replacement);
                }
            });
            Value::Undefined
        }),
        "insertAdjacentHTML" => func(move |_, args| {
            let position = arg(&args, 0).to_js_string().to_ascii_lowercase();
            let html = arg(&args, 1).to_js_string();
            match position.as_str() {
                "beforeend" => append_html_fragment(node, &html),
                "afterbegin" => {
                    let first = dom::first_child(node);
                    let holder = dom::create_element("div");
                    append_html_fragment(holder, &html);
                    for child in dom::children(holder) {
                        match first {
                            Some(first) => dom::insert_before(node, child, first),
                            None => dom::append_child(node, child),
                        }
                    }
                }
                "beforebegin" | "afterend" => {
                    if let Some(parent) = dom::parent_node(node) {
                        let reference = if position == "beforebegin" {
                            Some(node)
                        } else {
                            dom::next_sibling(node)
                        };
                        let holder = dom::create_element("div");
                        append_html_fragment(holder, &html);
                        for child in dom::children(holder) {
                            match reference {
                                Some(reference) => dom::insert_before(parent, child, reference),
                                None => dom::append_child(parent, child),
                            }
                        }
                    }
                }
                _ => {}
            }
            Value::Undefined
        }),
        "setHTML" => func(move |_, args| {
            clear_children(node);
            dom::set_text_content(node, "");
            append_sanitized_html_fragment(node, &arg(&args, 0).to_js_string());
            Value::Undefined
        }),
        "setHTMLUnsafe" => func(move |_, args| {
            static WARNING: std::sync::Once = std::sync::Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: setHTMLUnsafe parses inert markup; script execution and \
                     declarative shadow-root activation remain unavailable"
                );
            });
            clear_children(node);
            dom::set_text_content(node, "");
            append_html_fragment(node, &arg(&args, 0).to_js_string());
            Value::Undefined
        }),

        // ── Selectors ──
        "matches" | "webkitMatchesSelector" => func(move |_, args| {
            if args.is_empty() {
                type_error("matches requires one argument");
            }
            let sel = arg(&args, 0).to_js_string();
            let owner_document = get_expando(node, "ownerDocument").unwrap_or_else(document_value);
            let hash = owner_document
                .get_property("location")
                .get_property("hash")
                .to_js_string();
            let target_id = hash.strip_prefix('#').filter(|target| !target.is_empty());
            if sel.contains(":popover-open")
                || sel.contains(":modal")
                || sel.contains(":focus")
            {
                let parts = selector_chain_parts(&sel);
                if parts.is_empty() {
                    dom_exception("Invalid selector", "SyntaxError");
                }
                return Value::Bool(matches_selector_chain_in_scope(
                    node,
                    &parts,
                    Some(node),
                ));
            }
            match dom::matches_selector_with_target(node, &sel, target_id) {
                Ok(true) => Value::Bool(true),
                Ok(false) => {
                    let relative_match = args
                        .get(1)
                        .and_then(node_id_of)
                        .filter(|scope| dom::node_type(*scope) == 1)
                        .is_some_and(|scope| {
                            dom::matches_selector_relative_to_scope(node, &sel, scope, target_id)
                                .is_ok_and(|matched| matched)
                        });
                    Value::Bool(relative_match)
                }
                Err(()) => dom_exception("Invalid selector", "SyntaxError"),
            }
        }),
        "closest" => func(move |_, args| {
            let sel = arg(&args, 0).to_js_string();
            let parts = selector_chain_parts(&sel);
            let mut cur = Some(node);
            while let Some(id) = cur {
                if dom::node_type(id) == 1
                    && matches_selector_chain_in_scope(id, &parts, Some(node))
                {
                    return element_value(id);
                }
                cur = dom::parent_node(id);
            }
            Value::Null
        }),
        "querySelector" => func(move |_, args| {
            let sel = query_selector_argument(&args, node);
            element_or_null(
                query_selector_all_scoped(Some(node), &sel)
                    .into_iter()
                    .next(),
            )
        }),
        "querySelectorAll" => func(move |_, args| {
            let sel = query_selector_argument(&args, node);
            node_list(
                query_selector_all_scoped(Some(node), &sel)
                    .into_iter()
                    .map(element_value)
                    .collect(),
            )
        }),
        "getElementById" if dom::node_type(node) == 11 => func(move |_, args| {
            let id = arg(&args, 0).to_js_string();
            if id.is_empty() {
                return Value::Null;
            }
            descendant_elements(node)
                .into_iter()
                .find(|candidate| {
                    dom::get_attribute(*candidate, "id").as_deref() == Some(id.as_str())
                })
                .map(element_value)
                .unwrap_or(Value::Null)
        }),
        "getElementsByTagName" => func(move |_, args| {
            let tag = arg(&args, 0).to_js_string();
            let html_document = get_expando(node, "ownerDocument")
                .unwrap_or_else(document_value)
                .get_property("contentType")
                .to_js_string()
                == "text/html";
            html_collection(move || {
                descendant_elements(node)
                    .into_iter()
                    .filter(|candidate| element_matches_tag_name(*candidate, &tag, html_document))
                    .map(element_value)
                    .collect()
            })
        }),
        "getElementsByTagNameNS" => func(move |_, args| {
            let namespace = normalized_namespace(&arg(&args, 0));
            let local_name = arg(&args, 1).to_js_string();
            html_collection(move || {
                descendant_elements(node)
                    .into_iter()
                    .filter(|candidate| {
                        element_matches_namespace(*candidate, &namespace, &local_name)
                    })
                    .map(element_value)
                    .collect()
            })
        }),
        "getElementsByClassName" => func(move |_, args| {
            let class_names = arg(&args, 0).to_js_string();
            html_collection(move || {
                descendant_elements(node)
                    .into_iter()
                    .filter(|candidate| element_matches_class_names(*candidate, &class_names))
                    .map(element_value)
                    .collect()
            })
        }),

        // ── Layout (zeros until the layout engine runs) ──
        "getBoundingClientRect" => func(move |_, _| rect_value(forced_bounding_rect(node))),
        "getClientRects" => func(move |_, _| {
            crate::geometry_web::rect_list(vec![rect_value(forced_bounding_rect(node))])
        }),
        "offsetWidth" | "clientWidth" => Value::Number(forced_bounding_rect(node).width as f64),
        "offsetHeight" | "clientHeight" => Value::Number(forced_bounding_rect(node).height as f64),
        "scrollWidth" => Value::Number(forced_scroll_size(node).0),
        "scrollHeight" => Value::Number(forced_scroll_size(node).1),
        "offsetTop" => Value::Number(forced_bounding_rect(node).y as f64),
        "offsetLeft" => Value::Number(forced_bounding_rect(node).x as f64),
        "offsetParent" => Value::Null,
        "clientTop" | "clientLeft" => Value::Number(0.0),
        "scrollTop" => Value::Number(dom::get_scroll_offset(node).1 as f64),
        "scrollLeft" => Value::Number(dom::get_scroll_offset(node).0 as f64),
        "scrollIntoView" => func(move |_, args| {
            scroll_element_into_view(node, arg(&args, 0));
            Value::Undefined
        }),
        "scrollTo" | "scrollBy" | "scroll" => func(move |_, args| {
            // Accepts (x, y) or an options object {left, top}.
            let (mut left, mut top) = dom::get_scroll_offset(node);
            let first = arg(&args, 0);
            if first.is_object() {
                let l = first.get_property("left");
                let t = first.get_property("top");
                if !l.is_undefined() {
                    left = l.to_number() as f32;
                }
                if !t.is_undefined() {
                    top = t.to_number() as f32;
                }
            } else {
                left = first.to_number() as f32;
                top = arg(&args, 1).to_number() as f32;
            }
            dom::set_scroll_offset(node, Some(left), Some(top));
            Value::Undefined
        }),

        // ── Focus (bridge-side tracking; no real input focus yet) ──
        "focus" => func(move |_, _| {
            focus_element(node);
            Value::Undefined
        }),
        "blur" => func(move |_, _| {
            blur_element(node);
            Value::Undefined
        }),
        "click" => func(move |_, _| {
            let _prevented = !dispatch_sync(node, EventType::Click, EventData::None);
            #[cfg(target_os = "ios")]
            if !_prevented
                && dom::tag_name(node) == "input"
                && dom::get_attribute(node, "type")
                    .is_some_and(|value| value.eq_ignore_ascii_case("file"))
            {
                request_ios_file_picker(node);
            }
            Value::Undefined
        }),

        // ── Events ──
        "addEventListener" => func(move |_, args| {
            js_add_event_listener(
                node,
                &arg(&args, 0).to_js_string(),
                arg(&args, 1),
                arg(&args, 2),
            );
            Value::Undefined
        }),
        "removeEventListener" => func(move |_, args| {
            js_remove_event_listener(node, &arg(&args, 0).to_js_string());
            Value::Undefined
        }),
        "dispatchEvent" => func(move |_, args| Value::Bool(js_dispatch_event(node, arg(&args, 0)))),

        // ── Form-ish ──
        "form"
            if matches!(
                dom::tag_name(node).as_str(),
                "button" | "fieldset" | "input" | "object" | "output" | "select" | "textarea"
            ) =>
        {
            form_owner_value(node)
        }
        "selectedIndex" if dom::tag_name(node) == "select" => {
            Value::Number(select_selected_index(node) as f64)
        }
        "validity"
            if matches!(
                dom::tag_name(node).as_str(),
                "input" | "textarea" | "select"
            ) =>
        {
            validity_state_value(node)
        }
        "willValidate"
            if matches!(
                dom::tag_name(node).as_str(),
                "input" | "textarea" | "select"
            ) =>
        {
            Value::Bool(constraint_validation_candidate(node))
        }
        "validationMessage"
            if matches!(
                dom::tag_name(node).as_str(),
                "input" | "textarea" | "select"
            ) =>
        {
            Value::string(&validation_message(node))
        }
        "setCustomValidity"
            if matches!(
                dom::tag_name(node).as_str(),
                "input" | "textarea" | "select"
            ) =>
        {
            func(move |_, args| {
                set_expando(node, "__validation_message", arg(&args, 0));
                Value::Undefined
            })
        }
        "checkValidity" if dom::tag_name(node) == "form" => {
            func(move |_, _| Value::Bool(validate_form(node, false)))
        }
        "reportValidity" if dom::tag_name(node) == "form" => {
            func(move |_, _| Value::Bool(validate_form(node, true)))
        }
        "checkValidity"
            if matches!(
                dom::tag_name(node).as_str(),
                "input" | "textarea" | "select"
            ) =>
        {
            func(move |_, _| Value::Bool(validate_control(node, false)))
        }
        "reportValidity"
            if matches!(
                dom::tag_name(node).as_str(),
                "input" | "textarea" | "select"
            ) =>
        {
            func(move |_, _| Value::Bool(validate_control(node, true)))
        }
        "value" => {
            if is_inputish(node) {
                Value::string(&dom::get_attribute(node, "value").unwrap_or_default())
            } else if dom::tag_name(node) == "option" {
                Value::string(
                    &dom::get_attribute(node, "value").unwrap_or_else(|| dom::inner_text(node)),
                )
            } else {
                Value::Undefined
            }
        }
        "files"
            if dom::tag_name(node) == "input"
                && dom::get_attribute(node, "type")
                    .is_some_and(|value| value.eq_ignore_ascii_case("file")) =>
        {
            get_expando(node, "files")
                .unwrap_or_else(|| crate::clipboard_web::file_list_from_files(Vec::new()))
        }
        "text" if dom::tag_name(node) == "option" => Value::string(&dom::inner_text(node)),
        "text" if dom::tag_name(node) == "script" => Value::string(&dom::inner_text(node)),
        "src" | "type" | "integrity" | "referrerPolicy" | "crossOrigin"
            if dom::tag_name(node) == "script" =>
        {
            let attribute = match key {
                "referrerPolicy" => "referrerpolicy",
                "crossOrigin" => "crossorigin",
                other => other,
            };
            Value::string(&dom::get_attribute(node, attribute).unwrap_or_default())
        }
        "src" if dom::tag_name(node) == "iframe" => {
            Value::string(&dom::get_attribute(node, "src").unwrap_or_default())
        }
        "src" | "srcset" | "sizes" | "crossOrigin" | "referrerPolicy" | "decoding" | "loading"
        | "fetchPriority"
            if dom::tag_name(node) == "img" =>
        {
            let attribute = match key {
                "crossOrigin" => "crossorigin",
                "referrerPolicy" => "referrerpolicy",
                "fetchPriority" => "fetchpriority",
                other => other,
            };
            Value::string(&dom::get_attribute(node, attribute).unwrap_or_default())
        }
        "srcset" | "sizes" | "media" | "type" if dom::tag_name(node) == "source" => {
            Value::string(&dom::get_attribute(node, key).unwrap_or_default())
        }
        "currentSrc" if dom::tag_name(node) == "img" => {
            get_expando(node, "__w3cos_image_current_src").unwrap_or_else(|| Value::string(""))
        }
        "href" if dom::tag_name(node) == "a" => {
            let href = dom::get_attribute(node, "href").unwrap_or_default();
            Value::string(
                &url::Url::parse(&href)
                    .map(|url| url.to_string())
                    .unwrap_or(href),
            )
        }
        "naturalWidth" if dom::tag_name(node) == "img" => {
            get_expando(node, "__w3cos_image_natural_width").unwrap_or(Value::Number(0.0))
        }
        "naturalHeight" if dom::tag_name(node) == "img" => {
            get_expando(node, "__w3cos_image_natural_height").unwrap_or(Value::Number(0.0))
        }
        "complete" if dom::tag_name(node) == "img" => get_expando(node, "__w3cos_image_complete")
            .unwrap_or_else(|| {
                Value::Bool(
                    dom::get_attribute(node, "src").is_none_or(|src| src.is_empty())
                        && dom::get_attribute(node, "srcset")
                            .is_none_or(|srcset| srcset.is_empty()),
                )
            }),
        "decode" if dom::tag_name(node) == "img" => func(move |_, _| {
            #[cfg(feature = "dynamic-js")]
            {
                crate::dynamic_script::decode_image_element(node)
            }
            #[cfg(not(feature = "dynamic-js"))]
            {
                w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                    "Image decoding requires a Browser document loader",
                    "EncodingError",
                )])
            }
        }),
        "width" | "height" if dom::tag_name(node) == "img" => {
            let natural = if key == "width" {
                "__w3cos_image_natural_width"
            } else {
                "__w3cos_image_natural_height"
            };
            dom::get_attribute(node, key)
                .and_then(|value| value.parse::<f64>().ok())
                .map(Value::Number)
                .or_else(|| get_expando(node, natural))
                .unwrap_or(Value::Number(0.0))
        }
        "href" | "rel" | "type" | "media" | "integrity" | "referrerPolicy" | "crossOrigin"
            if dom::tag_name(node) == "link" =>
        {
            let attribute = match key {
                "referrerPolicy" => "referrerpolicy",
                "crossOrigin" => "crossorigin",
                other => other,
            };
            Value::string(&dom::get_attribute(node, attribute).unwrap_or_default())
        }
        "type" | "media" if dom::tag_name(node) == "style" => {
            Value::string(&dom::get_attribute(node, key).unwrap_or_default())
        }
        "content" | "name" | "media" | "scheme" if dom::tag_name(node) == "meta" => {
            Value::string(&dom::get_attribute(node, key).unwrap_or_default())
        }
        "httpEquiv" if dom::tag_name(node) == "meta" => Value::string(
            &dom::get_attribute(node, "http-equiv").unwrap_or_default(),
        ),
        "disabled" if matches!(dom::tag_name(node).as_str(), "link" | "style") => {
            Value::Bool(dom::has_attribute(node, "disabled"))
        }
        "defer" | "noModule" if dom::tag_name(node) == "script" => {
            let attribute = if key == "noModule" { "nomodule" } else { key };
            Value::Bool(dom::has_attribute(node, attribute))
        }
        "async" if dom::tag_name(node) == "script" => Value::Bool(
            dom::has_attribute(node, "async")
                || get_expando(node, "__w3cos_force_async").is_some_and(|value| value.to_bool()),
        ),
        "defaultSelected" if dom::tag_name(node) == "option" => {
            Value::Bool(dom::has_attribute(node, "selected"))
        }
        "selected" if dom::tag_name(node) == "option" => Value::Bool(option_is_selected(node)),
        "checked" => {
            if is_inputish(node) {
                Value::Bool(dom::has_attribute(node, "checked"))
            } else {
                Value::Undefined
            }
        }
        "disabled" => Value::Bool(dom::has_attribute(node, "disabled")),
        "readOnly" => Value::Bool(dom::has_attribute(node, "readonly")),
        "required" => Value::Bool(dom::has_attribute(node, "required")),
        "min" | "max" | "step" | "minLength" | "maxLength" | "pattern" => {
            let attribute = match key {
                "minLength" => "minlength",
                "maxLength" => "maxlength",
                other => other,
            };
            Value::string(&dom::get_attribute(node, attribute).unwrap_or_default())
        }
        "placeholder" => {
            Value::string(&dom::get_attribute(node, "placeholder").unwrap_or_default())
        }
        "type" if dom::tag_name(node) == "input" => {
            Value::string(&dom::get_attribute(node, "type").unwrap_or_else(|| "text".to_string()))
        }
        "selectionStart" | "selectionEnd" => get_expando(node, key).unwrap_or(Value::Number(0.0)),
        "selectionDirection" => get_expando(node, key).unwrap_or_else(|| Value::string("none")),
        "setSelectionRange" => func(move |_, args| {
            set_text_control_selection(
                node,
                arg(&args, 0).to_number().max(0.0) as usize,
                arg(&args, 1).to_number().max(0.0) as usize,
                &arg(&args, 2).to_js_string(),
            );
            Value::Undefined
        }),
        "setRangeText" => func(move |_, args| {
            set_range_text(node, &args);
            Value::Undefined
        }),
        "select" => func(move |_, _| {
            let len = dom::get_attribute(node, "value")
                .unwrap_or_default()
                .encode_utf16()
                .count();
            set_text_control_selection(node, 0, len, "none");
            Value::Undefined
        }),

        // ── Canvas ──
        "width" | "height" if dom::tag_name(node) == "canvas" => {
            let default = if key == "width" { 300.0 } else { 150.0 };
            Value::Number(
                dom::get_attribute(node, key)
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(default),
            )
        }
        "getContext" if dom::tag_name(node) == "canvas" => func(move |_, args| {
            let kind = arg(&args, 0).to_js_string();
            match kind.as_str() {
                "2d" => canvas_context_value(node),
                "bitmaprenderer" => {
                    if let Some(context) = get_expando(node, "__ctxbitmap") {
                        context
                    } else {
                        let context = crate::canvas_web::image_bitmap_rendering_context_value(
                            element_value(node),
                        );
                        set_expando(node, "__ctxbitmap", context.clone());
                        context
                    }
                }
                #[cfg(feature = "web-graphics-advanced")]
                "webgl" | "experimental-webgl" => {
                    if let Some(context) = get_expando(node, "__ctxwebgl") {
                        context
                    } else {
                        let context = crate::webgl_web::context_value(element_value(node), false);
                        set_expando(node, "__ctxwebgl", context.clone());
                        context
                    }
                }
                #[cfg(feature = "web-graphics-advanced")]
                "webgl2" => {
                    if let Some(context) = get_expando(node, "__ctxwebgl2") {
                        context
                    } else {
                        let context = crate::webgl_web::context_value(element_value(node), true);
                        set_expando(node, "__ctxwebgl2", context.clone());
                        context
                    }
                }
                _ => Value::Null,
            }
        }),
        #[cfg(feature = "web-media-advanced")]
        "captureStream" if dom::tag_name(node) == "canvas" => func(move |_, args| {
            let frame_rate = args.first().map(Value::to_number).unwrap_or(0.0);
            if frame_rate < 0.0 {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "NotSupportedError",
                    vec![Value::string(
                        "Canvas capture frame rate must not be negative",
                    )],
                ));
            }
            let track =
                crate::canvas_web::canvas_capture_media_stream_track_value(element_value(node));
            track.set_property("__w3cos_frame_rate", Value::Number(frame_rate));
            crate::media_devices_web::stream_value(vec![track])
        }),
        "requestFullscreen" => func(move |_, _| {
            FULLSCREEN_NODE.with(|current| *current.borrow_mut() = Some(node));
            let event = w3cos_core::class::construct(
                &crate::web_events::event_class(),
                vec![Value::string("fullscreenchange")],
            );
            document_value().call_method("dispatchEvent", vec![event]);
            resolved_thenable(Value::Undefined)
        }),

        "assignedNodes" if dom::tag_name(node) == "slot" => func(move |_, _| {
            js_array(
                assigned_nodes_for_slot_node(node)
                    .into_iter()
                    .map(element_value)
                    .collect(),
            )
        }),
        "assignedElements" if dom::tag_name(node) == "slot" => func(move |_, _| {
            js_array(
                assigned_nodes_for_slot_node(node)
                    .into_iter()
                    .filter(|assigned| dom::node_type(*assigned) == 1)
                    .map(element_value)
                    .collect(),
            )
        }),

        // ── Pointer capture ──
        "setPointerCapture" => func(move |_, args| {
            set_pointer_capture(node, arg(&args, 0).to_number() as i64);
            Value::Undefined
        }),
        "releasePointerCapture" => func(move |_, args| {
            release_pointer_capture(node, arg(&args, 0).to_number() as i64);
            Value::Undefined
        }),
        "hasPointerCapture" => func(move |_, args| {
            let pointer_id = arg(&args, 0).to_number() as i64;
            Value::Bool(
                POINTER_CAPTURE.with(|capture| capture.borrow().get(&pointer_id) == Some(&node)),
            )
        }),

        // ── Shadow DOM ──
        "shadowRoot" => SHADOW_ROOTS.with(|roots| {
            roots
                .borrow()
                .get(&node)
                .filter(|info| info.mode == "open")
                .map(|info| element_value(info.root))
                .unwrap_or(Value::Null)
        }),
        "attachShadow" => func(move |_, args| shadow_root_value(node, arg(&args, 0))),

        _ => Value::Undefined,
    }
}

fn element_computed_set(node: u32, key: &str, value: Value) -> bool {
    match key {
        "textContent" if matches!(dom::node_type(node), 1 | 11) => {
            replace_all_children(node, || {
                if !value.is_null() && !value.is_undefined() {
                    let text = value.to_js_string();
                    if !text.is_empty() {
                        let child = dom::create_text_node(&text);
                        ensure_tree_insertion(node, child);
                        dom::append_child(node, child);
                    }
                }
            });
        }
        "textContent" if matches!(dom::node_type(node), 3 | 4 | 7 | 8) => {
            let text = if value.is_null() {
                String::new()
            } else {
                value.to_js_string()
            };
            dom::set_text_content(node, &text);
        }
        "textContent" => {}
        "nodeValue" | "data" if matches!(dom::node_type(node), 3 | 4 | 7 | 8) => {
            let text = if value.is_null() {
                String::new()
            } else {
                value.to_js_string()
            };
            dom::set_text_content(node, &text);
        }
        "nodeValue" => {}
        "innerText" => {
            clear_children(node);
            dom::set_text_content(node, &value.to_js_string());
        }
        "innerHTML" => {
            replace_all_children(node, || {
                append_html_fragment(node, &value.to_js_string());
            });
        }
        "outerHTML" => {
            if let Some(parent) = dom::parent_node(node) {
                let container = dom::create_element("div");
                append_html_fragment(container, &value.to_js_string());
                let added = dom::children(container);
                let previous = dom::previous_sibling(node);
                let next = dom::next_sibling(node);
                let element = element_value(node);
                let was_connected = dom::is_connected(node);
                crate::observers_web::with_mutation_notifications_suppressed(|| {
                    for child in &added {
                        ensure_tree_insertion(parent, *child);
                        dom::insert_before(parent, *child, node);
                        pin_element_subtree(*child);
                        if dom::is_connected(*child) {
                            crate::custom_elements_web::connected_subtree(&element_value(*child));
                        }
                    }
                    dom::remove_child(parent, node);
                    release_element_subtree(node);
                });
                crate::observers_web::notify_child_list(parent, &added, &[node], previous, next);
                if was_connected
                    && get_expando(node, "__w3cos_custom_element_upgraded")
                        .is_some_and(|upgraded| upgraded.to_bool())
                {
                    queue_microtask_value(func(move |_, _| {
                        crate::custom_elements_web::disconnected_subtree(&element);
                        Value::Undefined
                    }));
                }
            }
        }
        "text" if dom::tag_name(node) == "script" => {
            clear_children(node);
            dom::set_text_content(node, &value.to_js_string());
        }
        "src" | "type" | "integrity" | "referrerPolicy" | "crossOrigin"
            if dom::tag_name(node) == "script" =>
        {
            let attribute = match key {
                "referrerPolicy" => "referrerpolicy",
                "crossOrigin" => "crossorigin",
                other => other,
            };
            dom::set_attribute(node, attribute, &value.to_js_string());
        }
        "src" if dom::tag_name(node) == "iframe" => {
            dom::set_attribute(node, "src", &value.to_js_string());
        }
        "src" | "srcset" | "sizes" | "crossOrigin" | "referrerPolicy" | "decoding" | "loading"
        | "fetchPriority"
            if dom::tag_name(node) == "img" =>
        {
            if matches!(key, "src" | "srcset" | "sizes") {
                set_expando(node, "__w3cos_image_complete", Value::Bool(false));
                set_expando(node, "__w3cos_image_current_src", Value::string(""));
                set_expando(node, "__w3cos_image_natural_width", Value::Number(0.0));
                set_expando(node, "__w3cos_image_natural_height", Value::Number(0.0));
            }
            let attribute = match key {
                "crossOrigin" => "crossorigin",
                "referrerPolicy" => "referrerpolicy",
                "fetchPriority" => "fetchpriority",
                other => other,
            };
            dom::set_attribute(node, attribute, &value.to_js_string());
        }
        "srcset" | "sizes" | "media" | "type" if dom::tag_name(node) == "source" => {
            dom::set_attribute(node, key, &value.to_js_string());
        }
        "href" if dom::tag_name(node) == "a" => {
            dom::set_attribute(node, "href", &value.to_js_string());
        }
        "width" | "height" if dom::tag_name(node) == "img" => {
            let dimension = value.to_number().max(0.0).trunc() as u32;
            dom::set_attribute(node, key, &dimension.to_string());
        }
        "href" | "rel" | "type" | "media" | "integrity" | "referrerPolicy" | "crossOrigin"
            if dom::tag_name(node) == "link" =>
        {
            let attribute = match key {
                "referrerPolicy" => "referrerpolicy",
                "crossOrigin" => "crossorigin",
                other => other,
            };
            dom::set_attribute(node, attribute, &value.to_js_string());
        }
        "type" | "media" if dom::tag_name(node) == "style" => {
            dom::set_attribute(node, key, &value.to_js_string());
        }
        "content" | "name" | "media" | "scheme" if dom::tag_name(node) == "meta" => {
            dom::set_attribute(node, key, &value.to_js_string());
        }
        "httpEquiv" if dom::tag_name(node) == "meta" => {
            dom::set_attribute(node, "http-equiv", &value.to_js_string());
        }
        "disabled" if matches!(dom::tag_name(node).as_str(), "link" | "style") => {
            if value.to_bool() {
                dom::set_attribute(node, "disabled", "");
            } else {
                dom::remove_attribute(node, "disabled");
            }
        }
        "async" | "defer" | "noModule" if dom::tag_name(node) == "script" => {
            let attribute = if key == "noModule" { "nomodule" } else { key };
            if key == "async" {
                set_expando(node, "__w3cos_force_async", Value::Bool(false));
            }
            if value.to_bool() {
                dom::set_attribute(node, attribute, "");
            } else {
                dom::remove_attribute(node, attribute);
            }
        }
        "disabled" => {
            if value.to_bool() {
                dom::set_attribute(node, "disabled", "");
            } else {
                dom::remove_attribute(node, "disabled");
            }
        }
        "readOnly" => {
            if value.to_bool() {
                dom::set_attribute(node, "readonly", "");
            } else {
                dom::remove_attribute(node, "readonly");
            }
        }
        "id" => dom::set_attribute(node, "id", &value.to_js_string()),
        "name" if exposes_window_name(node) => {
            dom::set_attribute(node, "name", &value.to_js_string())
        }
        "name" if dom::tag_name(node) == "slot" => {
            dom::set_attribute(node, "name", &value.to_js_string())
        }
        "className" => dom::set_attribute(node, "class", &value.to_js_string()),
        "style" => apply_html_attribute(node, "style", &value.to_js_string()),
        // Web IDL exposes Element.classList as a readonly [SameObject]
        // attribute. Assignment is ignored rather than replacing the live
        // DOMTokenList bridge object.
        "classList" => {}
        "value" => dom::set_attribute(node, "value", &value.to_js_string()),
        "defaultSelected" if dom::tag_name(node) == "option" => {
            if value.to_bool() {
                dom::set_attribute(node, "selected", "");
            } else {
                dom::remove_attribute(node, "selected");
            }
        }
        "selected" if dom::tag_name(node) == "option" => {
            set_option_selectedness(node, value.to_bool());
        }
        "checked" => {
            if value.to_bool() {
                dom::set_attribute(node, "checked", "");
            } else {
                dom::remove_attribute(node, "checked");
            }
        }
        "title" | "dir" | "lang" | "slot" | "placeholder" | "type" | "min" | "max" | "step"
        | "pattern" => {
            dom::set_attribute(node, key, &value.to_js_string());
        }
        "minLength" => dom::set_attribute(node, "minlength", &value.to_js_string()),
        "maxLength" => dom::set_attribute(node, "maxlength", &value.to_js_string()),
        "required" => {
            if value.to_bool() {
                dom::set_attribute(node, "required", "");
            } else {
                dom::remove_attribute(node, "required");
            }
        }
        "contentEditable" => dom::set_attribute(node, "contenteditable", &value.to_js_string()),
        "editContext" if dom::node_type(node) == 1 => {
            let element = element_value(node);
            if let Some(previous) = get_expando(node, "editContext") {
                if !previous.is_null() {
                    crate::edit_context_web::detach_element(&previous, &element);
                }
            }
            if value.is_null() || value.is_undefined() {
                set_expando(node, "editContext", Value::Null);
            } else {
                crate::edit_context_web::attach_element(&value, element);
                set_expando(node, "editContext", value);
            }
        }
        "draggable" => dom::set_attribute(node, "draggable", &value.to_js_string()),
        "hidden" => {
            if value.to_bool() {
                dom::set_attribute(node, "hidden", "");
            } else {
                dom::remove_attribute(node, "hidden");
            }
        }
        "tabIndex" => dom::set_attribute(node, "tabindex", &value.to_js_string()),
        "scrollTop" => dom::set_scroll_offset(node, None, Some(value.to_number() as f32)),
        "scrollLeft" => dom::set_scroll_offset(node, Some(value.to_number() as f32), None),
        "width" | "height" if dom::tag_name(node) == "canvas" => {
            dom::set_attribute(node, key, &value.to_js_string());
            let w = dom::get_attribute(node, "width")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(300);
            let h = dom::get_attribute(node, "height")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(150);
            CANVAS_CONTEXTS.with(|c| {
                if let Some(ctx) = c.borrow().get(&node) {
                    ctx.borrow_mut().resize(w, h);
                }
            });
        }
        // Everything else becomes a JS expando (stored bridge-side; the proxy
        // target handed to the set trap is only a snapshot, so we cannot
        // persist through it).
        _ => set_expando(node, key, value),
    }
    true
}

fn named_node_map_reserved_property(name: &str) -> bool {
    matches!(
        name,
        "length"
            | "item"
            | "getNamedItem"
            | "getNamedItemNS"
            | "removeNamedItem"
            | "removeNamedItemNS"
            | "setNamedItem"
            | "setNamedItemNS"
            | "constructor"
            | "__proto__"
            | "hasOwnProperty"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "toLocaleString"
            | "toString"
            | "valueOf"
    )
}

fn attribute_count(node: u32) -> usize {
    dom::with_document(|document| document.get_node(NodeId::from_u32(node)).attributes.len())
}

fn attributes_value(node: u32) -> Value {
    let attrs: Vec<(String, String, Option<String>, Option<String>, String)> =
        dom::with_document(|doc| {
            let node = doc.get_node(NodeId::from_u32(node));
            node.attributes
                .iter()
                .enumerate()
                .map(|(index, (name, value))| {
                    let qualified_name = name.as_str();
                    let metadata = node.attribute_namespace_at(index);
                    (
                        qualified_name.clone(),
                        value.clone(),
                        metadata
                            .and_then(|attribute| attribute.namespace.as_ref())
                            .map(|namespace| namespace.as_str()),
                        metadata
                            .and_then(|attribute| attribute.prefix.as_ref())
                            .map(|prefix| prefix.as_str()),
                        metadata
                            .map(|attribute| attribute.local_name.as_str())
                            .unwrap_or(qualified_name),
                    )
                })
                .collect()
        });
    let lowercase_names_only = namespace_uri(node) == crate::html_parser_state::HTML_NAMESPACE
        && get_expando(node, "ownerDocument")
            .unwrap_or_else(document_value)
            .get_property("contentType")
            .to_js_string()
            == "text/html";
    let mut seen_names = HashSet::new();
    let supported_names = attrs
        .iter()
        .map(|(qualified_name, _, _, _, _)| qualified_name.clone())
        .filter(|name| !lowercase_names_only || name == &name.to_ascii_lowercase())
        .filter(|name| !named_node_map_reserved_property(name))
        .filter(|name| seen_names.insert(name.clone()))
        .collect::<Vec<_>>();
    let mut props = HashMap::new();
    let len = attrs.len();
    let mut attr_values = Vec::with_capacity(len);
    let mut attrs_by_name = HashMap::with_capacity(len);
    for (i, (name, value, namespace, prefix, local_name)) in attrs.into_iter().enumerate() {
        let attr = attribute_value(
            node,
            &name,
            &local_name,
            namespace.as_deref(),
            prefix.as_deref(),
            &value,
        );
        attr_values.push(attr.clone());
        attrs_by_name.insert(name.clone(), attr.clone());
        props.insert(i.to_string(), attr.clone());
        if !named_node_map_reserved_property(&name) {
            props.insert(name, attr);
        }
    }
    props.insert("length".to_string(), Value::Number(len as f64));
    let attr_values_for_ns = attr_values.clone();
    let item_values = attr_values;
    props.insert(
        "item".to_string(),
        func(move |_, args| {
            let index = arg(&args, 0).to_number();
            if !index.is_finite() || index < 0.0 {
                return Value::Null;
            }
            item_values
                .get(index as usize)
                .cloned()
                .unwrap_or(Value::Null)
        }),
    );
    props.insert(
        "getNamedItem".to_string(),
        func(move |_, args| {
            attrs_by_name
                .get(&arg(&args, 0).to_js_string())
                .cloned()
                .unwrap_or(Value::Null)
        }),
    );
    props.insert(
        "getNamedItemNS".to_string(),
        func(move |_, args| {
            let requested_namespace = arg(&args, 0);
            let requested_namespace = (!requested_namespace.is_null())
                .then(|| requested_namespace.to_js_string())
                .filter(|namespace| !namespace.is_empty());
            let name = arg(&args, 1).to_js_string();
            attr_values_for_ns
                .iter()
                .find(|attribute| {
                    let namespace = attribute.get_property("namespaceURI");
                    let namespace = (!namespace.is_null())
                        .then(|| namespace.to_js_string())
                        .filter(|namespace| !namespace.is_empty());
                    namespace == requested_namespace
                        && attribute.get_property("localName").to_js_string() == name
                })
                .cloned()
                .unwrap_or(Value::Null)
        }),
    );
    props.insert(
        "removeNamedItem".to_string(),
        func(move |this, args| {
            let name = arg(&args, 0).to_js_string();
            let identity_key = format!("__w3cos_named_attribute_{name}");
            let attached = this.get_property(&identity_key);
            let previous = if attached.is_undefined() {
                dom::get_attribute(node, &name)
                    .map(|value| attribute_value(node, &name, &name, None, None, &value))
                    .unwrap_or(Value::Null)
            } else {
                attached
            };
            dom::remove_attribute(node, &name);
            this.delete_property(&identity_key);
            if !named_node_map_reserved_property(&name) {
                this.delete_property(&name);
            }
            this.set_property("length", Value::Number(attribute_count(node) as f64));
            previous
        }),
    );
    props.insert(
        "removeNamedItemNS".to_string(),
        func(move |_, args| {
            let namespace_value = arg(&args, 0);
            let namespace = (!namespace_value.is_null())
                .then(|| namespace_value.to_js_string())
                .filter(|namespace| !namespace.is_empty());
            let local_name = arg(&args, 1).to_js_string();
            let previous = dom::get_attribute_ns(node, namespace.as_deref(), &local_name)
                .map(|value| {
                    attribute_value(
                        node,
                        &local_name,
                        &local_name,
                        namespace.as_deref(),
                        None,
                        &value,
                    )
                })
                .unwrap_or(Value::Null);
            dom::remove_attribute_ns(node, namespace.as_deref(), &local_name);
            previous
        }),
    );
    props.insert(
        "setNamedItem".to_string(),
        func(move |this, args| {
            let attribute = arg(&args, 0);
            let name = attribute.get_property("name").to_js_string();
            let identity_key = format!("__w3cos_named_attribute_{name}");
            let attached = this.get_property(&identity_key);
            let previous = if attached.is_undefined() {
                dom::get_attribute(node, &name)
                    .map(|value| attribute_value(node, &name, &name, None, None, &value))
                    .unwrap_or(Value::Null)
            } else {
                attached
            };
            dom::set_attribute(node, &name, &attribute.get_property("value").to_js_string());
            attribute.set_property("ownerElement", element_value(node));
            this.set_property(&identity_key, attribute.clone());
            if !named_node_map_reserved_property(&name) {
                this.set_property(&name, attribute);
            }
            this.set_property("length", Value::Number(attribute_count(node) as f64));
            previous
        }),
    );
    props.insert(
        "setNamedItemNS".to_string(),
        func(move |_, args| {
            let attribute = arg(&args, 0);
            let qualified_name = attribute.get_property("name").to_js_string();
            let local_name = attribute.get_property("localName").to_js_string();
            let namespace_value = attribute.get_property("namespaceURI");
            let namespace = (!namespace_value.is_null() && !namespace_value.is_undefined())
                .then(|| namespace_value.to_js_string())
                .filter(|namespace| !namespace.is_empty());
            let prefix_value = attribute.get_property("prefix");
            let prefix = (!prefix_value.is_null() && !prefix_value.is_undefined())
                .then(|| prefix_value.to_js_string())
                .filter(|prefix| !prefix.is_empty());
            let previous = dom::get_attribute_ns(node, namespace.as_deref(), &local_name)
                .map(|value| {
                    attribute_value(
                        node,
                        &qualified_name,
                        &local_name,
                        namespace.as_deref(),
                        prefix.as_deref(),
                        &value,
                    )
                })
                .unwrap_or(Value::Null);
            dom::set_attribute_ns_parts(
                node,
                namespace.as_deref(),
                &qualified_name,
                prefix.as_deref(),
                &local_name,
                &attribute.get_property("value").to_js_string(),
            );
            previous
        }),
    );
    let own_names = supported_names.clone();
    let descriptor_names = supported_names.into_iter().collect::<HashSet<_>>();
    let handler = ProxyBuilder::new()
        .get_own_property_descriptor(move |target, key| {
            if let Ok(index) = key.parse::<usize>()
                && index < len
            {
                return Value::object(HashMap::from([
                    ("value".to_string(), target.get_property(key)),
                    ("writable".to_string(), Value::Bool(false)),
                    ("enumerable".to_string(), Value::Bool(true)),
                    ("configurable".to_string(), Value::Bool(true)),
                ]));
            }
            if descriptor_names.contains(key) {
                return Value::object(HashMap::from([
                    ("value".to_string(), target.get_property(key)),
                    ("writable".to_string(), Value::Bool(false)),
                    ("enumerable".to_string(), Value::Bool(false)),
                    ("configurable".to_string(), Value::Bool(true)),
                ]));
            }
            Value::Undefined
        })
        .own_keys(move |_target| {
            let mut keys = (0..len)
                .map(|index| Value::from(index.to_string()))
                .collect::<Vec<_>>();
            keys.extend(own_names.iter().cloned().map(Value::from));
            js_array(keys)
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(props, handler))));
    let prototype = crate::dom_constructors::prototype("NamedNodeMap");
    w3cos_core::class::set_prototype_of(&value, &prototype);
    prototype.set_property("item", value.get_property("item"));
    if prototype.get_property("toString").is_undefined() {
        prototype.set_property(
            "toString",
            func(|_, _| Value::string("[object NamedNodeMap]")),
        );
    }
    value
}

fn valid_attribute_name(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(|character| {
            character == '\0'
                || character == '/'
                || character == '>'
                || character == '='
                || character.is_ascii_whitespace()
        })
}

fn detached_attribute_value(document: &Value, name: Value, is_html: bool) -> Value {
    let mut name = name.to_js_string();
    if is_html {
        name.make_ascii_lowercase();
    }
    if !valid_attribute_name(&name) {
        dom_exception("Attribute name is not valid", "InvalidCharacterError");
    }
    detached_attribute_value_from_parts(document, &name, &name, None, None, "")
}

fn detached_namespaced_attribute_value(
    document: &Value,
    namespace: Value,
    qualified_name: Value,
) -> Value {
    let namespace = normalized_namespace(&namespace);
    let qualified_name = qualified_name.to_js_string();
    let (prefix, local_name) =
        validate_and_extract_qualified_name(namespace.as_deref(), &qualified_name);
    detached_attribute_value_from_parts(
        document,
        &qualified_name,
        &local_name,
        namespace.as_deref(),
        prefix.as_deref(),
        "",
    )
}

fn detached_attribute_value_from_parts(
    document: &Value,
    qualified_name: &str,
    local_name: &str,
    namespace: Option<&str>,
    prefix: Option<&str>,
    value: &str,
) -> Value {
    let mut props = HashMap::from([
        ("name".to_string(), Value::string(qualified_name)),
        ("localName".to_string(), Value::string(local_name)),
        ("value".to_string(), Value::string(value)),
        ("nodeName".to_string(), Value::string(qualified_name)),
        ("nodeValue".to_string(), Value::string(value)),
        ("textContent".to_string(), Value::string(value)),
        ("nodeType".to_string(), Value::Number(2.0)),
        (
            "namespaceURI".to_string(),
            namespace.map(Value::string).unwrap_or(Value::Null),
        ),
        (
            "prefix".to_string(),
            prefix.map(Value::string).unwrap_or(Value::Null),
        ),
        ("ownerDocument".to_string(), document.clone()),
        ("baseURI".to_string(), document.get_property("URL")),
        ("ownerElement".to_string(), Value::Null),
        ("specified".to_string(), Value::Bool(true)),
        (
            "isSameNode".to_string(),
            func(|attribute, args| Value::Bool(attribute.strict_eq(&arg(&args, 0)))),
        ),
    ]);
    props.insert(
        "isEqualNode".to_string(),
        func(|attribute, args| Value::Bool(nodes_are_equal(&attribute, &arg(&args, 0)))),
    );
    props.insert(
        "cloneNode".to_string(),
        func(|attribute, _| {
            let document = attribute.get_property("ownerDocument");
            let namespace = normalized_namespace_argument(&attribute.get_property("namespaceURI"));
            let prefix = normalized_namespace_argument(&attribute.get_property("prefix"));
            detached_attribute_value_from_parts(
                &document,
                &attribute.get_property("name").to_js_string(),
                &attribute.get_property("localName").to_js_string(),
                namespace.as_deref(),
                prefix.as_deref(),
                &attribute.get_property("value").to_js_string(),
            )
        }),
    );
    props.insert(
        "lookupNamespaceURI".to_string(),
        func(|attribute, args| lookup_namespace_uri_result(&attribute, &arg(&args, 0))),
    );
    props.insert(
        "lookupPrefix".to_string(),
        func(|attribute, args| lookup_prefix_result(&attribute, &arg(&args, 0))),
    );
    props.insert(
        "isDefaultNamespace".to_string(),
        func(|attribute, args| is_default_namespace_result(&attribute, &arg(&args, 0))),
    );
    let attribute = Value::object(props);
    w3cos_core::class::set_prototype_of(&attribute, &crate::dom_constructors::prototype("Attr"));
    attribute
}

fn attribute_value(
    owner: u32,
    qualified_name: &str,
    local_name: &str,
    namespace: Option<&str>,
    prefix: Option<&str>,
    value: &str,
) -> Value {
    let cache_key = attribute_cache_key(owner, namespace, local_name);
    if let Some(attribute) =
        ATTRIBUTE_VALUES.with(|cache| cache.borrow().get(&cache_key).cloned())
    {
        return attribute;
    }
    let qualified_name = qualified_name.to_string();
    let local_name = local_name.to_string();
    let namespace = namespace.map(str::to_string);
    let prefix = prefix.map(str::to_string);
    let stored_value = Rc::new(RefCell::new(value.to_string()));
    let mut props = HashMap::from([
        ("name".to_string(), Value::string(&qualified_name)),
        ("localName".to_string(), Value::string(&local_name)),
        ("nodeName".to_string(), Value::string(&qualified_name)),
        ("nodeType".to_string(), Value::Number(2.0)),
        (
            "namespaceURI".to_string(),
            namespace
                .as_deref()
                .map(Value::string)
                .unwrap_or(Value::Null),
        ),
        (
            "prefix".to_string(),
            prefix.as_deref().map(Value::string).unwrap_or(Value::Null),
        ),
        ("ownerElement".to_string(), element_value(owner)),
        (
            "ownerDocument".to_string(),
            get_expando(owner, "ownerDocument").unwrap_or_else(document_value),
        ),
        (
            "baseURI".to_string(),
            get_expando(owner, "ownerDocument")
                .unwrap_or_else(document_value)
                .get_property("URL"),
        ),
        ("specified".to_string(), Value::Bool(true)),
        (
            "isSameNode".to_string(),
            func(|attribute, args| Value::Bool(attribute.strict_eq(&arg(&args, 0)))),
        ),
    ]);
    for property in ["value", "nodeValue", "textContent"] {
        let getter_qualified_name = qualified_name.clone();
        let getter_local_name = local_name.clone();
        let getter_namespace = namespace.clone();
        let getter_stored_value = Rc::clone(&stored_value);
        props.insert(
            format!("__w3cos_getter_{property}"),
            func(move |_, _| {
                let current = if let Some(namespace) = getter_namespace.as_deref() {
                    dom::get_attribute_ns(owner, Some(namespace), &getter_local_name)
                } else {
                    dom::get_attribute(owner, &getter_qualified_name)
                };
                if let Some(current) = current {
                    *getter_stored_value.borrow_mut() = current;
                }
                Value::string(&getter_stored_value.borrow())
            }),
        );
        let setter_qualified_name = qualified_name.clone();
        let setter_local_name = local_name.clone();
        let setter_namespace = namespace.clone();
        let setter_prefix = prefix.clone();
        let setter_stored_value = Rc::clone(&stored_value);
        props.insert(
            format!("__w3cos_setter_{property}"),
            func(move |_, args| {
                let value = arg(&args, 0).to_js_string();
                let attached = if let Some(namespace) = setter_namespace.as_deref() {
                    dom::get_attribute_ns(owner, Some(namespace), &setter_local_name).is_some()
                } else {
                    dom::get_attribute(owner, &setter_qualified_name).is_some()
                };
                if attached {
                    dom::set_attribute_ns_parts(
                        owner,
                        setter_namespace.as_deref(),
                        &setter_qualified_name,
                        setter_prefix.as_deref(),
                        &setter_local_name,
                        &value,
                    );
                }
                *setter_stored_value.borrow_mut() = value;
                Value::Undefined
            }),
        );
    }
    props.insert(
        "lookupNamespaceURI".to_string(),
        func(|attribute, args| lookup_namespace_uri_result(&attribute, &arg(&args, 0))),
    );
    props.insert(
        "lookupPrefix".to_string(),
        func(|attribute, args| lookup_prefix_result(&attribute, &arg(&args, 0))),
    );
    props.insert(
        "isDefaultNamespace".to_string(),
        func(|attribute, args| is_default_namespace_result(&attribute, &arg(&args, 0))),
    );
    let attribute = Value::object(props);
    w3cos_core::class::set_prototype_of(&attribute, &crate::dom_constructors::prototype("Attr"));
    ATTRIBUTE_VALUES.with(|cache| {
        cache.borrow_mut().insert(cache_key, attribute.clone());
    });
    attribute
}

fn attribute_cache_key(
    owner: u32,
    namespace: Option<&str>,
    local_name: &str,
) -> (u32, Option<String>, String) {
    (
        owner,
        namespace.map(str::to_string),
        local_name.to_string(),
    )
}

fn cache_attribute_value(
    owner: u32,
    namespace: Option<&str>,
    local_name: &str,
    attribute: Value,
) {
    ATTRIBUTE_VALUES.with(|cache| {
        cache.borrow_mut().insert(
            attribute_cache_key(owner, namespace, local_name),
            attribute,
        );
    });
}

fn detach_attribute_value(owner: u32, attribute: &Value) {
    let namespace = normalized_namespace_argument(&attribute.get_property("namespaceURI"));
    let local_name = attribute.get_property("localName").to_js_string();
    ATTRIBUTE_VALUES.with(|cache| {
        cache
            .borrow_mut()
            .remove(&attribute_cache_key(owner, namespace.as_deref(), &local_name));
    });
    attribute.set_property("ownerElement", Value::Null);
}

fn update_cached_attribute_owner_document(owner: u32, document: &Value) {
    ATTRIBUTE_VALUES.with(|cache| {
        for ((candidate_owner, _, _), attribute) in cache.borrow().iter() {
            if *candidate_owner == owner {
                attribute.set_property("ownerDocument", document.clone());
                attribute.set_property("baseURI", document.get_property("URL"));
            }
        }
    });
}

fn attribute_node_by_qualified_name(node: u32, requested_name: &str) -> Value {
    let attribute = dom::with_document(|document| {
        let node = document.get_node(NodeId::from_u32(node));
        node.attributes
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name.as_str() == requested_name)
            .map(|(index, (name, value))| {
                let metadata = node.attribute_namespace_at(index);
                (
                    name.as_str().to_string(),
                    metadata
                        .map(|attribute| attribute.local_name.as_str())
                        .unwrap_or_else(|| name.as_str())
                        .to_string(),
                    metadata
                        .and_then(|attribute| attribute.namespace.as_ref())
                        .map(|namespace| namespace.as_str().to_string()),
                    metadata
                        .and_then(|attribute| attribute.prefix.as_ref())
                        .map(|prefix| prefix.as_str().to_string()),
                    value.clone(),
                )
            })
    });
    attribute
        .map(|(qualified_name, local_name, namespace, prefix, value)| {
            attribute_value(
                node,
                &qualified_name,
                &local_name,
                namespace.as_deref(),
                prefix.as_deref(),
                &value,
            )
        })
        .unwrap_or(Value::Null)
}

fn attribute_node_by_namespace(
    node: u32,
    requested_namespace: Option<&str>,
    requested_local_name: &str,
) -> Value {
    let attribute = dom::with_document(|document| {
        let node = document.get_node(NodeId::from_u32(node));
        node.attributes
            .iter()
            .enumerate()
            .find_map(|(index, (qualified_name, value))| {
                let metadata = node.attribute_namespace_at(index);
                let namespace = metadata
                    .and_then(|attribute| attribute.namespace.as_ref())
                    .map(|namespace| namespace.as_str());
                let local_name = metadata
                    .map(|attribute| attribute.local_name.as_str())
                    .unwrap_or_else(|| qualified_name.as_str());
                (namespace.as_deref() == requested_namespace && local_name == requested_local_name)
                    .then(|| {
                        (
                            qualified_name.as_str(),
                            local_name,
                            namespace,
                            metadata
                                .and_then(|attribute| attribute.prefix.as_ref())
                                .map(|prefix| prefix.as_str()),
                            value.clone(),
                        )
                    })
            })
    });
    attribute
        .map(|(qualified_name, local_name, namespace, prefix, value)| {
            attribute_value(
                node,
                &qualified_name,
                &local_name,
                namespace.as_deref(),
                prefix.as_deref(),
                &value,
            )
        })
        .unwrap_or(Value::Null)
}

fn dataset_attribute_name(key: &str) -> String {
    let mut attribute = String::from("data-");
    for character in key.chars() {
        if character.is_ascii_uppercase() {
            attribute.push('-');
            attribute.push(character.to_ascii_lowercase());
        } else {
            attribute.push(character);
        }
    }
    attribute
}

fn dataset_property_name(attribute: &str) -> Option<String> {
    let suffix = attribute.strip_prefix("data-")?;
    let mut property = String::new();
    let mut uppercase_next = false;
    for character in suffix.chars() {
        if character == '-' {
            uppercase_next = true;
        } else if uppercase_next {
            property.push(character.to_ascii_uppercase());
            uppercase_next = false;
        } else {
            property.push(character);
        }
    }
    Some(property)
}

fn dataset_keys(node: u32) -> Vec<String> {
    dom::with_document(|doc| {
        doc.get_node(NodeId::from_u32(node))
            .attributes
            .iter()
            .filter_map(|(name, _)| dataset_property_name(&name.as_str()))
            .collect()
    })
}

fn dataset_value(node: u32) -> Value {
    if let Some(value) = get_expando(node, "dataset") {
        return value;
    }
    let generation = realm_generation();
    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
            if !bridge_realm_is_current(generation) {
                return Value::Undefined;
            }
            let inherited = target.get_property(key);
            if !inherited.is_undefined() {
                return inherited;
            }
            dom::get_attribute(node, &dataset_attribute_name(key))
                .map(Value::from)
                .unwrap_or(Value::Undefined)
        })
        .set(move |_target, key, value, _receiver| {
            if bridge_realm_is_current(generation) {
                dom::set_attribute(node, &dataset_attribute_name(key), &value.to_js_string());
            }
            true
        })
        .has(move |_target, key| {
            bridge_realm_is_current(generation)
                && dom::has_attribute(node, &dataset_attribute_name(key))
        })
        .delete_property(move |_target, key| {
            if bridge_realm_is_current(generation) {
                dom::remove_attribute(node, &dataset_attribute_name(key));
            }
            true
        })
        .own_keys(move |_target| {
            if bridge_realm_is_current(generation) {
                Value::array(dataset_keys(node).into_iter().map(Value::from).collect())
            } else {
                Value::array(Vec::new())
            }
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        HashMap::new(),
        handler,
    ))));
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::dom_constructors::prototype("DOMStringMap"),
    );
    set_expando(node, "dataset", value.clone());
    value
}

// ── classList ──────────────────────────────────────────────────────────────

fn class_token_strings(node: u32) -> Vec<String> {
    dom::with_document(|doc| {
        doc.get_node(NodeId::from_u32(node))
            .class_list
            .iter()
            .map(|token| token.as_str().to_string())
            .collect()
    })
}

fn class_token_values(node: u32) -> Vec<Value> {
    class_token_strings(node)
        .into_iter()
        .map(Value::from)
        .collect()
}

fn validate_class_token_sequence(tokens: &[&str]) {
    if tokens.iter().any(|token| token.is_empty()) {
        dom_exception("The token must not be empty.", "SyntaxError");
    }
    if tokens.iter().any(|token| {
        token
            .bytes()
            .any(|byte| matches!(byte, b'\t' | b'\n' | 0x0c | b'\r' | b' '))
    }) {
        dom_exception(
            "The token must not contain ASCII whitespace.",
            "InvalidCharacterError",
        );
    }
}

fn validate_class_token(token: &str) {
    validate_class_token_sequence(&[token]);
}

fn validated_class_tokens(args: &[Value]) -> Vec<String> {
    let tokens = args.iter().map(Value::to_js_string).collect::<Vec<_>>();
    validate_class_token_sequence(&tokens.iter().map(String::as_str).collect::<Vec<_>>());
    tokens
}

fn write_class_tokens(node: u32, tokens: &[String]) {
    let before = capture_transition_snapshots(node);
    let old_value = dom::get_attribute(node, "class");
    dom::set_class_name(node, &tokens.join(" "));
    crate::observers_web::notify_attribute(node, "class", old_value.as_deref());
    let after = capture_transition_snapshots(node);
    start_changed_transitions(node, &before, &after);
}

fn write_class_value(node: u32, value: &str) {
    let before = capture_transition_snapshots(node);
    let old_value = dom::get_attribute(node, "class");
    dom::set_class_name(node, value);
    crate::observers_web::notify_attribute(node, "class", old_value.as_deref());
    let after = capture_transition_snapshots(node);
    start_changed_transitions(node, &before, &after);
}

fn class_list_value(node: u32) -> Value {
    if let Some(v) = get_expando(node, "classList") {
        return v;
    }
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "add".to_string(),
        func(move |_, args| {
            let additions = validated_class_tokens(&args);
            let mut tokens = class_token_strings(node);
            for addition in additions {
                if !tokens.contains(&addition) {
                    tokens.push(addition);
                }
            }
            if dom::get_attribute(node, "class").is_some() || !tokens.is_empty() {
                write_class_tokens(node, &tokens);
            }
            Value::Undefined
        }),
    );
    props.insert(
        "remove".to_string(),
        func(move |_, args| {
            let removals = validated_class_tokens(&args);
            if dom::get_attribute(node, "class").is_some() {
                let mut tokens = class_token_strings(node);
                tokens.retain(|token| !removals.contains(token));
                write_class_tokens(node, &tokens);
            }
            Value::Undefined
        }),
    );
    props.insert(
        "toggle".to_string(),
        func(move |_, args| {
            let token = arg(&args, 0).to_js_string();
            validate_class_token(&token);
            let force = arg(&args, 1);
            let mut tokens = class_token_strings(node);
            let contains = tokens.contains(&token);
            if !force.is_undefined() && force.to_bool() == contains {
                return Value::Bool(contains);
            }
            if contains {
                tokens.retain(|candidate| candidate != &token);
            } else {
                tokens.push(token);
            }
            write_class_tokens(node, &tokens);
            Value::Bool(!contains)
        }),
    );
    props.insert(
        "contains".to_string(),
        func(move |_, args| {
            Value::Bool(dom::class_list_contains(
                node,
                &arg(&args, 0).to_js_string(),
            ))
        }),
    );
    props.insert(
        "replace".to_string(),
        func(move |_, args| {
            let old = arg(&args, 0).to_js_string();
            let new = arg(&args, 1).to_js_string();
            validate_class_token_sequence(&[&old, &new]);
            let mut tokens = class_token_strings(node);
            let Some(old_index) = tokens.iter().position(|token| token == &old) else {
                return Value::Bool(false);
            };
            if old != new {
                tokens[old_index] = new;
                let mut seen = HashSet::new();
                tokens.retain(|token| seen.insert(token.clone()));
            }
            write_class_tokens(node, &tokens);
            Value::Bool(true)
        }),
    );
    props.insert(
        "item".to_string(),
        func(move |_, args| {
            let idx = arg(&args, 0).to_u32() as usize;
            dom::with_document(|doc| {
                doc.get_node(NodeId::from_u32(node))
                    .class_list
                    .get(idx)
                    .map(|a| Value::string(&a.as_str()))
                    .unwrap_or(Value::Null)
            })
        }),
    );
    props.insert(
        "toString".to_string(),
        func(move |_, _| Value::string(&dom::get_attribute(node, "class").unwrap_or_default())),
    );
    props.insert(
        "values".to_string(),
        func(move |_, _| Value::array(class_token_values(node))),
    );
    props.insert(
        "keys".to_string(),
        func(move |_, _| {
            Value::array(
                (0..class_token_values(node).len())
                    .map(|index| Value::Number(index as f64))
                    .collect(),
            )
        }),
    );
    props.insert(
        "entries".to_string(),
        func(move |_, _| {
            Value::array(
                class_token_values(node)
                    .into_iter()
                    .enumerate()
                    .map(|(index, token)| Value::array(vec![Value::Number(index as f64), token]))
                    .collect(),
            )
        }),
    );
    props.insert(
        "forEach".to_string(),
        func(move |this, args| {
            let callback = arg(&args, 0);
            for (index, token) in class_token_values(node).into_iter().enumerate() {
                callback.call(
                    Value::Undefined,
                    vec![token.clone(), Value::Number(index as f64), this.clone()],
                );
            }
            Value::Undefined
        }),
    );
    props.insert(
        "supports".to_string(),
        func(|_, _| type_error("DOMTokenList.supports is not supported for classList.")),
    );
    // Live getters via the value.rs getter convention (plain object).
    props.insert(
        "__w3cos_getter_length".to_string(),
        func(move |_, _| {
            Value::Number(dom::with_document(|doc| {
                doc.get_node(NodeId::from_u32(node)).class_list.len() as f64
            }))
        }),
    );
    props.insert(
        "__w3cos_getter_value".to_string(),
        func(move |_, _| Value::string(&dom::get_attribute(node, "class").unwrap_or_default())),
    );
    props.insert(
        "__w3cos_setter_value".to_string(),
        func(move |_, args| {
            write_class_value(node, &arg(&args, 0).to_js_string());
            Value::Undefined
        }),
    );
    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
            if let Ok(index) = key.parse::<u32>() {
                if index.to_string() == key {
                    return class_token_strings(node)
                        .get(index as usize)
                        .map(|token| Value::string(token))
                        .unwrap_or(Value::Undefined);
                }
            }
            target.get_property(key)
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(props, handler))));
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::dom_constructors::prototype("DOMTokenList"),
    );
    set_expando(node, "classList", value.clone());
    value
}

// ── CSS motion + style proxy ──────────────────────────────────────────────

fn css_motion_value(property: &str, value: &str) -> Option<CssMotionValue> {
    let mut declaration = w3cos_dom::css_style::CSSStyleDeclaration::new();
    declaration.set_property(property, value);
    let style = declaration.to_style();
    match property {
        "left" => Some(CssMotionValue::Length(match style.left {
            w3cos_std::style::Dimension::Px(value) => value,
            w3cos_std::style::Dimension::Rem(value) | w3cos_std::style::Dimension::Em(value) => {
                value * 16.0
            }
            _ => 0.0,
        })),
        "transform" => Some(CssMotionValue::TranslateX(style.transform.translate_x)),
        _ => None,
    }
}

fn css_motion_value_from_style(
    style: &w3cos_std::style::Style,
    property: &str,
) -> Option<CssMotionValue> {
    match property {
        "left" => Some(CssMotionValue::Length(match style.left {
            w3cos_std::style::Dimension::Px(value) => value,
            w3cos_std::style::Dimension::Rem(value) | w3cos_std::style::Dimension::Em(value) => {
                value * style.font_size
            }
            _ => 0.0,
        })),
        "transform" => Some(CssMotionValue::TranslateX(style.transform.translate_x)),
        _ => None,
    }
}

fn css_motion_value_lerp(
    from: CssMotionValue,
    to: CssMotionValue,
    progress: f32,
) -> CssMotionValue {
    match (from, to) {
        (CssMotionValue::Length(from), CssMotionValue::Length(to)) => {
            CssMotionValue::Length(from + (to - from) * progress)
        }
        (CssMotionValue::TranslateX(from), CssMotionValue::TranslateX(to)) => {
            CssMotionValue::TranslateX(from + (to - from) * progress)
        }
        _ => to,
    }
}

fn css_motion_value_to_css(value: CssMotionValue) -> String {
    match value {
        CssMotionValue::Length(value) => format!("{value}px"),
        CssMotionValue::TranslateX(value) => {
            format!("matrix(1, 0, 0, 1, {value}, 0)")
        }
    }
}

fn sample_css_motion(motion: &CssMotion, now: Instant) -> CssMotionValue {
    let elapsed = now.saturating_duration_since(motion.started_at);
    if elapsed < motion.delay {
        return motion.from;
    }
    let active = elapsed.saturating_sub(motion.delay).as_secs_f32();
    let duration = motion.duration.as_secs_f32().max(0.001);
    let mut progress = if motion.kind == CssMotionKind::Animation {
        (active / duration).fract()
    } else {
        (active / duration).clamp(0.0, 1.0)
    };
    if motion.kind == CssMotionKind::Animation {
        let iteration = (active / duration).floor() as u64;
        progress = match motion.direction {
            w3cos_std::style::AnimationDirection::Normal => progress,
            w3cos_std::style::AnimationDirection::Reverse => 1.0 - progress,
            w3cos_std::style::AnimationDirection::Alternate => {
                if iteration.is_multiple_of(2) {
                    progress
                } else {
                    1.0 - progress
                }
            }
            w3cos_std::style::AnimationDirection::AlternateReverse => {
                if iteration.is_multiple_of(2) {
                    1.0 - progress
                } else {
                    progress
                }
            }
        };
    }
    css_motion_value_lerp(motion.from, motion.to, motion.easing.interpolate(progress))
}

fn sampled_css_motion_value(
    node: u32,
    pseudo: Option<&str>,
    property: &str,
) -> Option<CssMotionValue> {
    let now = Instant::now();
    CSS_MOTIONS.with(|motions| {
        motions
            .borrow()
            .iter()
            .rev()
            .find(|motion| {
                motion.node == node
                    && motion.pseudo.as_deref() == pseudo
                    && motion.property == property
            })
            .map(|motion| sample_css_motion(motion, now))
    })
}

fn transition_applies(transition: &w3cos_std::style::Transition, property: &str) -> bool {
    use w3cos_std::style::TransitionProperty;
    match &transition.property {
        TransitionProperty::All => true,
        TransitionProperty::Transform => property == "transform",
        TransitionProperty::Custom(candidate) => candidate.eq_ignore_ascii_case(property),
        _ => false,
    }
}

fn motion_style(node: u32, pseudo: Option<&str>) -> w3cos_std::style::Style {
    dom::with_document(|document| match pseudo {
        Some(pseudo) => {
            document.computed_pseudo_style_for(NodeId::from_u32(node), pseudo)
        }
        None => document.computed_style_for(NodeId::from_u32(node)),
    })
}

fn capture_transition_snapshots(node: u32) -> Vec<TransitionSnapshot> {
    let mut snapshots = Vec::new();
    for pseudo in [None, Some("::before"), Some("::after")] {
        let style = motion_style(node, pseudo);
        let Some(transition) = style.transition.clone() else {
            continue;
        };
        if transition.duration_ms == 0 {
            continue;
        }
        for property in ["left", "transform"] {
            if !transition_applies(&transition, property) {
                continue;
            }
            if let Some(value) = css_motion_value_from_style(&style, property) {
                snapshots.push(TransitionSnapshot {
                    pseudo: pseudo.map(str::to_string),
                    property: property.to_string(),
                    value,
                    transition: transition.clone(),
                });
            }
        }
    }
    snapshots
}

fn start_css_transition(
    node: u32,
    from: &TransitionSnapshot,
    to: &TransitionSnapshot,
) {
    if from.value == to.value {
        return;
    }
    let sampled_from = sampled_css_motion_value(node, from.pseudo.as_deref(), &from.property)
        .unwrap_or(from.value);
    CSS_MOTIONS.with(|motions| {
        let mut motions = motions.borrow_mut();
        motions.retain(|motion| {
            !(motion.node == node
                && motion.kind == CssMotionKind::Transition
                && motion.pseudo == from.pseudo
                && motion.property == from.property)
        });
        motions.push(CssMotion {
            node,
            pseudo: from.pseudo.clone(),
            property: from.property.clone(),
            kind: CssMotionKind::Transition,
            label: from.property.clone(),
            from: sampled_from,
            to: to.value,
            started_at: Instant::now(),
            delay: Duration::from_millis(u64::from(to.transition.delay_ms)),
            duration: Duration::from_millis(u64::from(to.transition.duration_ms)),
            easing: to.transition.easing,
            direction: w3cos_std::style::AnimationDirection::Normal,
            event_pending: true,
        });
    });
    crate::animations_web::css_motion_animation(
        node,
        "transition",
        &from.property,
        from.pseudo.as_deref(),
        &from.property,
    );
}

fn start_changed_transitions(
    node: u32,
    before: &[TransitionSnapshot],
    after: &[TransitionSnapshot],
) {
    for to in after {
        if let Some(from) = before.iter().find(|from| {
            from.pseudo == to.pseudo && from.property == to.property
        }) {
            start_css_transition(node, from, to);
        }
    }
}

fn scan_css_animations() {
    for node in 0..dom::node_count() as u32 {
        if dom::node_type(node) != 1 || !dom::is_connected(node) {
            continue;
        }
        let style = motion_style(node, None);
        let Some(animation) = style.animation.clone() else {
            continue;
        };
        if animation.name.is_empty() || animation.name == "none" || animation.duration_ms == 0 {
            continue;
        }
        let already_started = CSS_MOTIONS.with(|motions| {
            motions.borrow().iter().any(|motion| {
                motion.node == node
                    && motion.kind == CssMotionKind::Animation
                    && motion.label == animation.name
            })
        });
        if already_started {
            continue;
        }
        let keyframes = ["transform", "left"].into_iter().find_map(|property| {
            crate::dynamic_script::active_keyframe_property(&animation.name, property)
                .and_then(|(from, to)| {
                    Some((
                        property,
                        css_motion_value(property, &from)?,
                        css_motion_value(property, &to)?,
                    ))
                })
        });
        let Some((property, from, to)) = keyframes else {
            continue;
        };
        CSS_MOTIONS.with(|motions| {
            motions.borrow_mut().push(CssMotion {
                node,
                pseudo: None,
                property: property.to_string(),
                kind: CssMotionKind::Animation,
                label: animation.name.clone(),
                from,
                to,
                started_at: Instant::now(),
                delay: Duration::from_millis(u64::from(animation.delay_ms)),
                duration: Duration::from_millis(u64::from(animation.duration_ms)),
                easing: animation.easing,
                direction: animation.direction,
                event_pending: true,
            });
        });
        crate::animations_web::css_motion_animation(
            node,
            "animation",
            &animation.name,
            None,
            property,
        );
    }
}

fn dispatch_css_motion_event(node: u32, event_type: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![
            Value::string(event_type),
            Value::object(HashMap::from([("bubbles".to_string(), Value::Bool(true))])),
        ],
    );
    element_value(node).call_method("dispatchEvent", vec![event]);
}

fn tick_css_motions() {
    scan_css_animations();
    let now = Instant::now();
    let events = CSS_MOTIONS.with(|motions| {
        motions
            .borrow_mut()
            .iter_mut()
            .filter_map(|motion| {
                if motion.event_pending && now.saturating_duration_since(motion.started_at) >= motion.delay {
                    motion.event_pending = false;
                    Some((
                        motion.node,
                        if motion.kind == CssMotionKind::Animation {
                            "animationstart"
                        } else {
                            "transitionstart"
                        },
                    ))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    });
    for (node, event_type) in events {
        dispatch_css_motion_event(node, event_type);
    }
}

pub(crate) fn commit_css_motion_style(
    node: u32,
    pseudo: Option<&str>,
    property: &str,
) {
    if pseudo.is_some() {
        return;
    }
    let Some(value) = sampled_css_motion_value(node, None, property) else {
        return;
    };
    let value = css_motion_value_to_css(value);
    STYLE_CACHE.with(|cache| {
        cache
            .borrow_mut()
            .insert((node, property.to_string()), value.clone());
    });
    dom::set_style_property(node, property, &value);
}

fn style_read(node: u32, kebab: &str) -> String {
    if let Some(v) = STYLE_CACHE.with(|c| c.borrow().get(&(node, kebab.to_string())).cloned()) {
        return v;
    }
    dom::with_document(|doc| {
        Element::new(NodeId::from_u32(node))
            .style(doc)
            .get_property(kebab)
    })
}

pub(crate) fn resolved_style_property(node: u32, kebab: &str) -> String {
    let inline = style_read(node, kebab);
    if inline.is_empty() {
        dom::computed_style_property(node, kebab)
    } else {
        inline
    }
}

fn style_apply(node: u32, kebab: &str, value: &str) {
    let Some(value) = normalize_inline_style_property(kebab, value) else {
        return;
    };
    // The runtime currently samples transitions only for these two motion
    // values. Avoid six full computed-style cascades around unrelated inline
    // writes (CSS parsing tests perform thousands of color assignments).
    let tracks_motion = matches!(kebab, "left" | "transform");
    let before = tracks_motion.then(|| capture_transition_snapshots(node));
    STYLE_CACHE.with(|c| {
        c.borrow_mut()
            .insert((node, kebab.to_string()), value.clone());
    });
    // Forward to the typed style (known properties drive layout; unknown ones
    // are dropped there but stay in the bridge cache).
    dom::set_style_property(node, kebab, &value);
    if let Some(before) = before {
        let after = capture_transition_snapshots(node);
        start_changed_transitions(node, &before, &after);
    }
}

fn normalize_inline_style_property(property: &str, value: &str) -> Option<String> {
    let value = value.trim();
    if property != "color" && !property.ends_with("-color") || value.is_empty() {
        return Some(value.to_string());
    }
    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "currentcolor"
    ) || w3cos_std::color::Color::from_named(&lower).is_some()
    {
        return Some(lower);
    }
    w3cos_std::color::Color::from_css(&lower).map(serialize_css_color)
}

fn style_css_text(node: u32) -> String {
    STYLE_CACHE.with(|c| {
        let cache = c.borrow();
        let mut pairs: Vec<(&String, &String)> = cache
            .iter()
            .filter(|((n, _), value)| *n == node && !value.is_empty())
            .map(|((_, k), v)| (k, v))
            .collect();
        pairs.sort();
        let mut out = String::new();
        for (k, v) in pairs {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(v);
            out.push_str("; ");
        }
        out
    })
}

fn style_property_names(node: u32) -> Vec<String> {
    STYLE_CACHE.with(|cache| {
        let mut names = cache
            .borrow()
            .iter()
            .filter(|((candidate, _), value)| *candidate == node && !value.is_empty())
            .map(|((_, name), _)| name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    })
}

fn typed_style_map_value(node: u32, readonly: bool) -> Value {
    let expando = if readonly {
        "__computedStyleMap"
    } else {
        "attributeStyleMap"
    };
    if let Some(value) = get_expando(node, expando) {
        return value;
    }
    let value = Value::object(HashMap::new());
    let read = move |name: &str| {
        if readonly {
            dom::computed_style_property(node, name)
        } else {
            STYLE_CACHE.with(|cache| {
                cache
                    .borrow()
                    .get(&(node, name.to_string()))
                    .cloned()
                    .unwrap_or_default()
            })
        }
    };
    let read_get = read;
    value.set_property(
        "get",
        func(move |_, args| {
            let name = camel_to_kebab(&arg(&args, 0).to_js_string());
            let text = read_get(&name);
            if text.is_empty() {
                Value::Undefined
            } else {
                crate::css_typed_om_web::parse_style_value(&text)
            }
        }),
    );
    let read_all = read;
    value.set_property(
        "getAll",
        func(move |_, args| {
            let name = camel_to_kebab(&arg(&args, 0).to_js_string());
            let text = read_all(&name);
            if text.is_empty() {
                Value::array(Vec::new())
            } else {
                Value::array(vec![crate::css_typed_om_web::parse_style_value(&text)])
            }
        }),
    );
    let read_has = read;
    value.set_property(
        "has",
        func(move |_, args| {
            let name = camel_to_kebab(&arg(&args, 0).to_js_string());
            Value::Bool(!read_has(&name).is_empty())
        }),
    );
    value.set_property(
        "__w3cos_getter_size",
        func(move |_, _| Value::Number(style_property_names(node).len() as f64)),
    );
    for method in ["keys", "values", "entries"] {
        value.set_property(
            method,
            func(move |_, _| {
                let names = style_property_names(node);
                let values = names
                    .into_iter()
                    .map(|name| {
                        let parsed = crate::css_typed_om_web::parse_style_value(&if readonly {
                            dom::computed_style_property(node, &name)
                        } else {
                            STYLE_CACHE.with(|cache| {
                                cache
                                    .borrow()
                                    .get(&(node, name.clone()))
                                    .cloned()
                                    .unwrap_or_default()
                            })
                        });
                        match method {
                            "keys" => Value::string(&name),
                            "values" => parsed,
                            _ => Value::array(vec![Value::string(&name), parsed]),
                        }
                    })
                    .collect();
                Value::array(values).call_method("__w3cos_symbol_iterator", Vec::new())
            }),
        );
    }
    let each_value = value.clone();
    value.set_property(
        "forEach",
        func(move |_, args| {
            let callback = arg(&args, 0);
            if !callback.is_function() {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "TypeError",
                    vec![Value::string(
                        "StylePropertyMap.forEach requires a callback",
                    )],
                ));
            }
            let this_arg = arg(&args, 1);
            for name in style_property_names(node) {
                let text = if readonly {
                    dom::computed_style_property(node, &name)
                } else {
                    STYLE_CACHE.with(|cache| {
                        cache
                            .borrow()
                            .get(&(node, name.clone()))
                            .cloned()
                            .unwrap_or_default()
                    })
                };
                callback.call(
                    this_arg.clone(),
                    vec![
                        crate::css_typed_om_web::parse_style_value(&text),
                        Value::string(&name),
                        each_value.clone(),
                    ],
                );
            }
            Value::Undefined
        }),
    );
    if !readonly {
        value.set_property(
            "set",
            func(move |_, args| {
                let name = camel_to_kebab(&arg(&args, 0).to_js_string());
                let serialized = args
                    .iter()
                    .skip(1)
                    .map(crate::css_typed_om_web::serialize_value)
                    .collect::<Vec<_>>()
                    .join(" ");
                if serialized.is_empty() {
                    w3cos_core::throw_value(w3cos_core::error_instance(
                        "TypeError",
                        vec![Value::string("StylePropertyMap.set requires a value")],
                    ));
                }
                style_apply(node, &name, &serialized);
                Value::Undefined
            }),
        );
        value.set_property(
            "append",
            func(move |_, args| {
                let name = camel_to_kebab(&arg(&args, 0).to_js_string());
                let addition = args
                    .iter()
                    .skip(1)
                    .map(crate::css_typed_om_web::serialize_value)
                    .collect::<Vec<_>>()
                    .join(" ");
                let current = STYLE_CACHE.with(|cache| {
                    cache
                        .borrow()
                        .get(&(node, name.clone()))
                        .cloned()
                        .unwrap_or_default()
                });
                style_apply(
                    node,
                    &name,
                    &if current.is_empty() {
                        addition
                    } else {
                        format!("{current} {addition}")
                    },
                );
                Value::Undefined
            }),
        );
        value.set_property(
            "delete",
            func(move |_, args| {
                style_apply(node, &camel_to_kebab(&arg(&args, 0).to_js_string()), "");
                Value::Undefined
            }),
        );
        value.set_property(
            "clear",
            func(move |_, _| {
                for name in style_property_names(node) {
                    style_apply(node, &name, "");
                }
                Value::Undefined
            }),
        );
    }
    let class_name = if readonly {
        "StylePropertyMapReadOnly"
    } else {
        "StylePropertyMap"
    };
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::css_typed_om_web::class(class_name).get_property("prototype"),
    );
    set_expando(node, expando, value.clone());
    value
}

fn parse_css_text(node: u32, css: &str) {
    for decl in css.split(';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }
        if let Some((prop, value)) = decl.split_once(':') {
            style_apply(node, &camel_to_kebab(prop.trim()), value.trim());
        }
    }
}

fn style_value(node: u32) -> Value {
    if let Some(v) = get_expando(node, "style") {
        return v;
    }
    let generation = realm_generation();
    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
            if !bridge_realm_is_current(generation) {
                return Value::Undefined;
            }
            let stored = target.get_property(key);
            if !stored.is_undefined() {
                return stored;
            }
            match key {
                "setProperty" => func(move |_, args| {
                    let prop = camel_to_kebab(&arg(&args, 0).to_js_string());
                    let value = arg(&args, 1).to_js_string();
                    style_apply(node, &prop, &value);
                    Value::Undefined
                }),
                "getPropertyValue" => func(move |_, args| {
                    let prop = camel_to_kebab(&arg(&args, 0).to_js_string());
                    Value::string(&style_read(node, &prop))
                }),
                "removeProperty" => func(move |_, args| {
                    let prop = camel_to_kebab(&arg(&args, 0).to_js_string());
                    let old = style_read(node, &prop);
                    style_apply(node, &prop, "");
                    Value::string(&old)
                }),
                "cssText" => Value::string(&style_css_text(node)),
                "length" => Value::Number(STYLE_CACHE.with(|c| {
                    c.borrow()
                        .iter()
                        .filter(|((candidate, _), value)| *candidate == node && !value.is_empty())
                        .count()
                }) as f64),
                // Magic w3cos-core convention keys must stay Undefined:
                // returning "" (a non-undefined value) for `__w3cos_setter_*`
                // makes `Value::set_property` "call" the empty string and
                // skip the proxy set trap entirely.
                _ if key.starts_with("__w3cos_") => Value::Undefined,
                _ => Value::string(&style_read(node, &camel_to_kebab(key))),
            }
        })
        .has(move |target, key| {
            if target
                .as_object()
                .is_some_and(|object| object.borrow().has_direct(key))
            {
                return true;
            }
            let property = camel_to_kebab(key);
            css_property_supported(&property, "initial")
                || STYLE_CACHE.with(|cache| {
                    cache
                        .borrow()
                        .contains_key(&(node, property.to_string()))
                })
        })
        .set(move |_target, key, value, _receiver| {
            if bridge_realm_is_current(generation) {
                if key == "cssText" {
                    parse_css_text(node, &value.to_js_string());
                } else {
                    style_apply(node, &camel_to_kebab(key), &value.to_js_string());
                }
            }
            true
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        HashMap::new(),
        handler,
    ))));
    w3cos_core::class::set_prototype_of(
        &value,
        &css_style_declaration_class().get_property("prototype"),
    );
    set_expando(node, "style", value.clone());
    value
}

fn serialize_computed_style_property(property: &str, value: &str) -> String {
    if property != "color" && !property.ends_with("-color") {
        return value.to_string();
    }
    let Some(color) = w3cos_std::color::Color::from_css(value) else {
        return value.to_string();
    };
    serialize_css_color(color)
}

fn serialize_css_color(color: w3cos_std::color::Color) -> String {
    if color.a == 255 {
        return format!("rgb({}, {}, {})", color.r, color.g, color.b);
    }
    let mut alpha = format!("{:.3}", f64::from(color.a) / 255.0);
    while alpha.ends_with('0') {
        alpha.pop();
    }
    if alpha.ends_with('.') {
        alpha.pop();
    }
    format!("rgba({}, {}, {}, {alpha})", color.r, color.g, color.b)
}

fn computed_style_property_value(node: u32, pseudo: Option<&str>, property: &str) -> String {
    if let Some(value) = sampled_css_motion_value(node, pseudo, property) {
        return css_motion_value_to_css(value);
    }
    let mut value = match pseudo {
        Some(pseudo) => dom::computed_pseudo_style_property(node, pseudo, property),
        None => dom::computed_style_property(node, property),
    };
    if value.is_empty() && pseudo.is_none() {
        value = inline_computed_style_fallback(node, property);
    }
    serialize_computed_style_property(property, &value)
}

fn inline_computed_style_fallback(node: u32, property: &str) -> String {
    let initial = match property {
        "border-bottom-style" | "border-left-style" | "border-right-style"
        | "border-top-style" | "clear" | "float" => "none",
        "border-collapse" => "separate",
        "empty-cells" => "show",
        _ => return String::new(),
    };
    let inline = style_read(node, property);
    match inline.to_ascii_lowercase().as_str() {
        "" | "initial" | "unset" | "revert" | "revert-layer" => initial.to_string(),
        "inherit" => dom::parent_node(node)
            .map(|parent| computed_style_property_value(parent, None, property))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| initial.to_string()),
        _ => inline,
    }
}

fn computed_style_value(node: u32, pseudo: Option<String>) -> Value {
    let generation = realm_generation();
    let getter_pseudo = pseudo.clone();
    let handler = ProxyBuilder::new()
        .get(move |target, key, _| {
            if !bridge_realm_is_current(generation) {
                return Value::Undefined;
            }
            let stored = target.get_property(key);
            if !stored.is_undefined() {
                return stored;
            }
            let property = camel_to_kebab(key);
            Value::string(&computed_style_property_value(
                node,
                getter_pseudo.as_deref(),
                &property,
            ))
        })
        .build();
    let method_pseudo = pseudo;
    let target = HashMap::from([
        (
            "getPropertyValue".to_string(),
            func(move |_, args| {
                let property = camel_to_kebab(&arg(&args, 0).to_js_string());
                Value::string(&computed_style_property_value(
                    node,
                    method_pseudo.as_deref(),
                    &property,
                ))
            }),
        ),
        (
            "getPropertyPriority".to_string(),
            func(|_, _| Value::string("")),
        ),
    ]);
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(target, handler))));
    w3cos_core::class::set_prototype_of(
        &value,
        &css_style_declaration_class().get_property("prototype"),
    );
    value
}

// ── JS event bridge ────────────────────────────────────────────────────────

fn js_add_event_listener(node: u32, type_name: &str, handler: Value, options: Value) {
    if !handler.is_function() {
        return;
    }
    let capture = if let Some(b) = options.as_bool() {
        b
    } else if options.is_object() {
        options.get_property("capture").to_bool()
    } else {
        false
    };
    let et = event_type_for(type_name);
    LISTENERS.with(|l| {
        l.borrow_mut().push(JsListener {
            node,
            event_type: et,
            handler,
            capture,
            inline: false,
        })
    });
    ensure_native_registration(node, et);
}

pub(crate) fn js_set_inline_event_listener(node: u32, type_name: &str, handler: Value) {
    let event_type = event_type_for(type_name);
    let installs_handler = handler.is_function();
    LISTENERS.with(|listeners| {
        let mut listeners = listeners.borrow_mut();
        listeners.retain(|listener| {
            !(listener.inline && listener.node == node && listener.event_type == event_type)
        });
        if installs_handler {
            listeners.push(JsListener {
                node,
                event_type,
                handler,
                capture: false,
                inline: true,
            });
        }
    });
    if installs_handler {
        ensure_native_registration(node, event_type);
    }
}

/// Register the w3cos-dom-side snapshot closure once per (node, event_type).
/// The closure only clones the event into PENDING_EVENTS — it must not call
/// JS (dispatch holds the document borrow) and must not touch the DOM.
fn ensure_native_registration(node: u32, et: EventType) {
    let already = NATIVELY_REGISTERED.with(|r| r.borrow().contains(&(node, et)));
    if already {
        return;
    }
    NATIVELY_REGISTERED.with(|r| r.borrow_mut().insert((node, et)));
    dom::with_document_mut(|doc| {
        Element::new(NodeId::from_u32(node)).add_event_listener_typed(
            doc,
            et,
            Box::new(|ev: &mut Event| {
                PENDING_EVENTS.with(|q| q.borrow_mut().push(ev.clone()));
            }),
        );
    });
}

/// v1 limitation: removes ALL bridge listeners for (node, type) — individual
/// JS function identity cannot be compared (`Value` equality on functions is
/// always false).
fn js_remove_event_listener(node: u32, type_name: &str) {
    let et = event_type_for(type_name);
    LISTENERS.with(|l| {
        l.borrow_mut()
            .retain(|jl| !(jl.node == node && jl.event_type == et));
    });
}

fn key_code_for(key: &str, code: &str) -> u32 {
    match key {
        "Enter" => 13,
        "Escape" => 27,
        "Backspace" => 8,
        "Tab" => 9,
        "Delete" => 46,
        "Insert" => 45,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Home" => 36,
        "End" => 35,
        "PageUp" => 33,
        "PageDown" => 34,
        "Shift" => 16,
        "Control" => 17,
        "Alt" => 18,
        "Meta" => 91,
        "CapsLock" => 20,
        " " | "Spacebar" => 32,
        "F1" => 112,
        "F2" => 113,
        "F3" => 114,
        "F4" => 115,
        "F5" => 116,
        "F6" => 117,
        "F7" => 118,
        "F8" => 119,
        "F9" => 120,
        "F10" => 121,
        "F11" => 122,
        "F12" => 123,
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap();
            if c.is_ascii() {
                c.to_ascii_uppercase() as u32
            } else {
                0
            }
        }
        _ => {
            if let Some(d) = code.strip_prefix("Digit") {
                d.parse::<u32>().map(|n| 48 + n).unwrap_or(0)
            } else if let Some(k) = code.strip_prefix("Key") {
                k.chars().next().map(|c| c as u32).unwrap_or(0)
            } else {
                0
            }
        }
    }
}

fn insert_mouse_props(props: &mut HashMap<String, Value>, d: &w3cos_dom::events::MouseEventData) {
    let mut put = |k: &str, v: f64| {
        props.insert(k.to_string(), Value::Number(v));
    };
    put("clientX", d.client_x);
    put("clientY", d.client_y);
    put("pageX", d.page_x);
    put("pageY", d.page_y);
    put("offsetX", d.offset_x);
    put("offsetY", d.offset_y);
    put("screenX", d.client_x);
    put("screenY", d.client_y);
    put("movementX", 0.0);
    put("movementY", 0.0);
    put("button", d.button as f64);
    put("buttons", d.buttons as f64);
    props.insert("ctrlKey".to_string(), Value::Bool(d.ctrl_key));
    props.insert("shiftKey".to_string(), Value::Bool(d.shift_key));
    props.insert("altKey".to_string(), Value::Bool(d.alt_key));
    props.insert("metaKey".to_string(), Value::Bool(d.meta_key));
}

/// Build the JS event object passed to handlers. Flag state lives in hidden
/// props (`__pd`/`__sp`/`__sip`) so `preventDefault()` etc. can mutate them
/// through self-referential closures.
fn build_event_value(ev: &Event) -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "type".to_string(),
        Value::string(&event_type_name(ev.event_type)),
    );
    props.insert("target".to_string(), element_value(ev.target.as_u32()));
    props.insert(
        "currentTarget".to_string(),
        element_value(ev.current_target.as_u32()),
    );
    props.insert("srcElement".to_string(), element_value(ev.target.as_u32()));
    props.insert("relatedTarget".to_string(), Value::Null);
    props.insert("bubbles".to_string(), Value::Bool(ev.bubbles));
    props.insert("cancelable".to_string(), Value::Bool(ev.cancelable));
    props.insert("composed".to_string(), Value::Bool(ev.composed));
    props.insert(
        "eventPhase".to_string(),
        Value::Number(ev.event_phase as u8 as f64),
    );
    props.insert("timeStamp".to_string(), Value::Number(ev.timestamp));
    props.insert("__pd".to_string(), Value::Bool(ev.prevent_default));
    props.insert("__sp".to_string(), Value::Bool(ev.stop_propagation));
    props.insert(
        "__sip".to_string(),
        Value::Bool(ev.stop_immediate_propagation),
    );
    props.insert("returnValue".to_string(), Value::Bool(!ev.prevent_default));
    // Phase constants (also present on the Event constructor in browsers).
    props.insert("NONE".to_string(), Value::Number(0.0));
    props.insert("CAPTURING_PHASE".to_string(), Value::Number(1.0));
    props.insert("AT_TARGET".to_string(), Value::Number(2.0));
    props.insert("BUBBLING_PHASE".to_string(), Value::Number(3.0));

    match &ev.data {
        EventData::Mouse(d) => insert_mouse_props(&mut props, d),
        EventData::Pointer(d) => {
            insert_mouse_props(&mut props, &d.mouse);
            props.insert("pointerId".to_string(), Value::Number(d.pointer_id as f64));
            props.insert("pointerType".to_string(), Value::string(&d.pointer_type));
            props.insert("pressure".to_string(), Value::Number(d.pressure as f64));
            props.insert("width".to_string(), Value::Number(d.width as f64));
            props.insert("height".to_string(), Value::Number(d.height as f64));
            props.insert("isPrimary".to_string(), Value::Bool(d.is_primary));
        }
        EventData::Wheel(d) => {
            insert_mouse_props(&mut props, &d.mouse);
            props.insert("deltaX".to_string(), Value::Number(d.delta_x));
            props.insert("deltaY".to_string(), Value::Number(d.delta_y));
            props.insert("deltaZ".to_string(), Value::Number(d.delta_z));
            props.insert("deltaMode".to_string(), Value::Number(d.delta_mode as f64));
        }
        EventData::Keyboard(d) => {
            props.insert("key".to_string(), Value::string(&d.key));
            props.insert("code".to_string(), Value::string(&d.code));
            props.insert("ctrlKey".to_string(), Value::Bool(d.ctrl_key));
            props.insert("shiftKey".to_string(), Value::Bool(d.shift_key));
            props.insert("altKey".to_string(), Value::Bool(d.alt_key));
            props.insert("metaKey".to_string(), Value::Bool(d.meta_key));
            props.insert("repeat".to_string(), Value::Bool(d.repeat));
            props.insert("location".to_string(), Value::Number(d.location as f64));
            let key_code = key_code_for(&d.key, &d.code) as f64;
            props.insert("keyCode".to_string(), Value::Number(key_code));
            props.insert("which".to_string(), Value::Number(key_code));
        }
        EventData::Input {
            data,
            input_type,
            is_composing,
        }
        | EventData::BeforeInput {
            data,
            input_type,
            is_composing,
            ..
        } => {
            props.insert(
                "data".to_string(),
                data.as_deref().map(Value::string).unwrap_or(Value::Null),
            );
            props.insert(
                "inputType".to_string(),
                input_type
                    .as_ref()
                    .map(|t| Value::string(t.as_str()))
                    .unwrap_or(Value::Null),
            );
            props.insert("isComposing".to_string(), Value::Bool(*is_composing));
        }
        EventData::Composition { data } => {
            props.insert("data".to_string(), Value::string(data));
        }
        EventData::Custom { detail } => {
            props.insert(
                "detail".to_string(),
                detail.as_deref().map(Value::string).unwrap_or(Value::Null),
            );
        }
        EventData::Focus | EventData::None => {}
    }

    let value = Value::object(props);

    // Self-referential flag mutators.
    let v = value.clone();
    value.set_property(
        "preventDefault",
        func(move |_, _| {
            if v.get_property("cancelable").to_bool() {
                v.set_property("__pd", Value::Bool(true));
                v.set_property("returnValue", Value::Bool(false));
            }
            Value::Undefined
        }),
    );
    let v = value.clone();
    value.set_property(
        "stopPropagation",
        func(move |_, _| {
            v.set_property("__sp", Value::Bool(true));
            Value::Undefined
        }),
    );
    let v = value.clone();
    value.set_property(
        "stopImmediatePropagation",
        func(move |_, _| {
            v.set_property("__sp", Value::Bool(true));
            v.set_property("__sip", Value::Bool(true));
            Value::Undefined
        }),
    );
    let v = value.clone();
    value.set_property(
        "__w3cos_getter_defaultPrevented",
        func(move |_, _| v.get_property("__pd")),
    );
    let v = value.clone();
    value.set_property(
        "__w3cos_getter_cancelBubble",
        func(move |_, _| v.get_property("__sp")),
    );
    let constructor = match &ev.data {
        EventData::Mouse(_) => crate::web_events::event_subclass_class("MouseEvent"),
        EventData::Pointer(_) => crate::web_events::event_subclass_class("PointerEvent"),
        EventData::Wheel(_) => crate::web_events::event_subclass_class("WheelEvent"),
        EventData::Keyboard(_) => crate::web_events::event_subclass_class("KeyboardEvent"),
        EventData::Input { .. } | EventData::BeforeInput { .. } => {
            crate::web_events::event_subclass_class("InputEvent")
        }
        EventData::Composition { .. } => {
            crate::web_events::event_subclass_class("CompositionEvent")
        }
        EventData::Custom { .. } => crate::web_events::custom_event_class(),
        EventData::Focus => crate::web_events::event_subclass_class("FocusEvent"),
        EventData::None => crate::web_events::event_class(),
    };
    w3cos_core::class::set_prototype_of(&value, &constructor.get_property("prototype"));
    value
}

/// Synchronous JS dispatch with capture/target/bubble phases. No document
/// borrow is held while JS handlers run, so handlers may mutate the DOM.
/// Returns false when the event was canceled (preventDefault).
fn dispatch_event_to_js(ev: Event) -> bool {
    let js_ev = build_event_value(&ev);
    js_ev.set_property("isTrusted", Value::Bool(true));
    dispatch_event_value_to_js(&ev, js_ev)
}

fn dispatch_event_value_to_js(ev: &Event, js_ev: Value) -> bool {
    if js_ev.get_property("isTrusted").to_bool() {
        record_event_count(&js_ev.get_property("type").to_js_string());
    }
    let _activation = (js_ev.get_property("isTrusted").to_bool()
        && matches!(
            ev.event_type,
            EventType::KeyDown | EventType::MouseDown | EventType::PointerUp | EventType::TouchEnd
        ))
    .then(crate::user_activation_web::begin_transient_activation);
    let target = ev.target.as_u32();
    let trace = std::env::var_os("W3COS_INPUT_TRACE").is_some();
    let mut listener_calls = 0usize;
    // [target, parent, ..., root], crossing shadow boundaries only for
    // composed events.
    let chain = shadow_event_chain(target, ev.composed);
    js_ev.set_property(
        "__w3cos_path",
        js_array(chain.iter().copied().map(element_value).collect()),
    );

    let target_value = element_value(target);
    js_ev.set_property("target", target_value.clone());
    js_ev.set_property("srcElement", target_value);
    let stopped = |v: &Value| v.get_property("__sp").to_bool();
    let immediate = |v: &Value| v.get_property("__sip").to_bool();
    let set_current_target = |event: &Value, id: u32, phase: f64| {
        event.set_property("target", element_value(retarget_shadow_event(target, id)));
        event.set_property("srcElement", event.get_property("target"));
        event.set_property("currentTarget", element_value(id));
        event.set_property("eventPhase", Value::Number(phase));
    };

    let snapshot_listeners = |node_id: u32, capture_phase: Option<bool>| -> Vec<Value> {
        LISTENERS.with(|l| {
            l.borrow()
                .iter()
                .filter(|jl| {
                    jl.node == node_id
                        && jl.event_type == ev.event_type
                        && capture_phase.is_none_or(|cp| jl.capture == cp)
                })
                .map(|jl| jl.handler.clone())
                .collect()
        })
    };

    // Phase 1: capture, root → parent (skip target).
    for &id in chain.iter().rev().skip(1) {
        if stopped(&js_ev) {
            break;
        }
        set_current_target(&js_ev, id, 1.0);
        for h in snapshot_listeners(id, Some(true)) {
            listener_calls += 1;
            h.call(Value::Undefined, vec![js_ev.clone()]);
            if immediate(&js_ev) {
                break;
            }
        }
    }

    // Phase 2: at target (both capture and bubble listeners).
    if !stopped(&js_ev) {
        set_current_target(&js_ev, target, 2.0);
        for h in snapshot_listeners(target, None) {
            listener_calls += 1;
            h.call(Value::Undefined, vec![js_ev.clone()]);
            if immediate(&js_ev) {
                break;
            }
        }
    }

    // Phase 3: bubble, parent → root.
    if ev.bubbles && !stopped(&js_ev) {
        for &id in chain.iter().skip(1) {
            if stopped(&js_ev) {
                break;
            }
            set_current_target(&js_ev, id, 3.0);
            for h in snapshot_listeners(id, Some(false)) {
                listener_calls += 1;
                h.call(Value::Undefined, vec![js_ev.clone()]);
                if immediate(&js_ev) {
                    break;
                }
            }
        }
    }

    js_ev.set_property("eventPhase", Value::Number(0.0));
    js_ev.set_property("target", element_value(target));
    js_ev.set_property("srcElement", element_value(target));
    if trace
        && matches!(
            ev.event_type,
            EventType::Focus
                | EventType::Blur
                | EventType::PointerDown
                | EventType::PointerUp
                | EventType::MouseDown
                | EventType::MouseUp
                | EventType::Click
                | EventType::KeyDown
                | EventType::KeyUp
                | EventType::BeforeInput
                | EventType::Input
        )
    {
        eprintln!(
            "[W3C OS][DOM INPUT] target={target} type={} listeners={listener_calls} prevented={}",
            event_type_name(ev.event_type),
            js_ev.get_property("__pd").to_bool()
        );
    }
    !js_ev.get_property("__pd").to_bool()
}

fn record_event_count(event_type: &str) {
    EVENT_COUNTS.with(|counts| {
        if let Some(count) = counts.borrow_mut().get_mut(event_type) {
            *count = count.saturating_add(1);
        }
    });
}

fn dispatch_sync(target: u32, et: EventType, data: EventData) -> bool {
    let mut ev = Event::new(et, NodeId::from_u32(target));
    ev.data = data;
    dispatch_event_to_js(ev)
}

fn blur_element(node: u32) {
    let was_active = ACTIVE_ELEMENT.with(|active| {
        if *active.borrow() != Some(node) {
            return false;
        }
        *active.borrow_mut() = None;
        true
    });
    if !was_active {
        return;
    }
    dispatch_sync(node, EventType::Blur, EventData::Focus);
    dispatch_sync(node, EventType::FocusOut, EventData::Focus);
}

fn focus_element(node: u32) {
    let previous = ACTIVE_ELEMENT.with(|active| *active.borrow());
    if previous == Some(node) {
        return;
    }
    if let Some(previous) = previous {
        blur_element(previous);
    }
    ACTIVE_ELEMENT.with(|active| *active.borrow_mut() = Some(node));
    dispatch_sync(node, EventType::Focus, EventData::Focus);
    dispatch_sync(node, EventType::FocusIn, EventData::Focus);
}

fn blur_focus_for_standard_reparent(node: u32) {
    if dom::parent_node(node).is_none() {
        return;
    }
    let focused = ACTIVE_ELEMENT.with(|active| *active.borrow());
    if let Some(focused) = focused
        && (focused == node || is_ancestor_of(node, focused))
    {
        blur_element(focused);
    }
}

fn focus_is_suppressed(node: u32) -> bool {
    let mut current = Some(node);
    while let Some(candidate) = current {
        if dom::has_attribute(candidate, "inert") || dom::has_attribute(candidate, "hidden") {
            return true;
        }
        let display = dom::with_document(|document| {
            Element::new(NodeId::from_u32(candidate))
                .get_computed_style(document)
                .get_property("display")
        });
        if display == "none" {
            return true;
        }
        current = dom::parent_node(candidate);
    }
    false
}

fn schedule_focus_revalidation_after_move(moved: u32) {
    let focused = ACTIVE_ELEMENT.with(|active| *active.borrow());
    let Some(focused) = focused.filter(|focused| {
        *focused == moved || is_ancestor_of(moved, *focused)
    }) else {
        return;
    };
    queue_microtask_value(func(move |_, _| {
        if element_has_focus(focused) && focus_is_suppressed(focused) {
            blur_element(focused);
        }
        Value::Undefined
    }));
}

/// Deliver a platform/session-history traversal to `window` listeners.
pub(crate) fn dispatch_native_popstate(state: Option<String>) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_subclass_class("PopStateEvent"),
        vec![
            Value::string("popstate"),
            Value::object(HashMap::from([(
                "state".to_string(),
                state.as_deref().map(Value::string).unwrap_or(Value::Null),
            )])),
        ],
    );
    event.set_property("isTrusted", Value::Bool(true));
    let native_event = Event::new(EventType::PopState, NodeId::from_u32(0));
    let _ = dispatch_event_value_to_js(&native_event, event);
}

pub(crate) fn dispatch_native_hashchange(old_url: &str, new_url: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_subclass_class("HashChangeEvent"),
        vec![
            Value::string("hashchange"),
            Value::object(HashMap::from([
                ("oldURL".to_string(), Value::string(old_url)),
                ("newURL".to_string(), Value::string(new_url)),
            ])),
        ],
    );
    event.set_property("isTrusted", Value::Bool(true));
    let native_event = Event::new(EventType::HashChange, NodeId::from_u32(0));
    let _ = dispatch_event_value_to_js(&native_event, event);
}

/// Synchronously bridge native window focus into the compiled-JS DOM.
pub(crate) fn dispatch_native_focus(target: u32, focused: bool) -> bool {
    ACTIVE_ELEMENT.with(|active| {
        if focused {
            *active.borrow_mut() = Some(target);
        } else if *active.borrow() == Some(target) {
            *active.borrow_mut() = None;
        }
    });
    dispatch_sync(
        target,
        if focused {
            EventType::Focus
        } else {
            EventType::Blur
        },
        EventData::Focus,
    )
}

/// Return the DOM element most recently focused through the JS bridge.
/// The window runtime uses this after pointer handlers run so a script-driven
/// `textarea.focus()` also becomes the native keyboard target.
pub(crate) fn active_element_id() -> Option<u32> {
    ACTIVE_ELEMENT.with(|active| *active.borrow())
}

fn native_touch_value(touch: &NativeTouch) -> Value {
    let x = Value::Number(touch.client_x as f64);
    let y = Value::Number(touch.client_y as f64);
    w3cos_core::class::construct(
        &crate::web_events::touch_class(),
        vec![Value::object(HashMap::from([
            (
                "identifier".to_string(),
                Value::Number(touch.identifier as f64),
            ),
            ("target".to_string(), element_value(touch.target)),
            ("screenX".to_string(), x.clone()),
            ("screenY".to_string(), y.clone()),
            ("clientX".to_string(), x.clone()),
            ("clientY".to_string(), y.clone()),
            ("pageX".to_string(), x),
            ("pageY".to_string(), y),
            ("radiusX".to_string(), Value::Number(1.0)),
            ("radiusY".to_string(), Value::Number(1.0)),
            ("rotationAngle".to_string(), Value::Number(0.0)),
            ("force".to_string(), Value::Number(touch.force as f64)),
        ]))],
    )
}

fn touch_list(touches: &[NativeTouch]) -> Value {
    crate::web_events::touch_list_value(Value::array(
        touches.iter().map(native_touch_value).collect(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn dispatch_native_touch(
    target: u32,
    phase: &str,
    client_x: f32,
    client_y: f32,
    pointer_id: i64,
    pressure: f32,
    alt_key: bool,
    ctrl_key: bool,
    meta_key: bool,
    shift_key: bool,
) -> bool {
    let Some(event_type) = (match phase {
        "down" => Some(EventType::TouchStart),
        "move" => Some(EventType::TouchMove),
        "up" => Some(EventType::TouchEnd),
        "cancel" => Some(EventType::TouchCancel),
        _ => None,
    }) else {
        return true;
    };
    let (event_target, changed, active) = ACTIVE_TOUCHES.with(|touches| {
        let mut touches = touches.borrow_mut();
        let existing = touches
            .iter()
            .position(|touch| touch.identifier == pointer_id);
        let changed = NativeTouch {
            identifier: pointer_id,
            target: existing
                .map(|index| touches[index].target)
                .unwrap_or(target),
            client_x,
            client_y,
            force: pressure,
        };
        match phase {
            "down" | "move" => {
                if let Some(index) = existing {
                    touches[index] = changed.clone();
                } else {
                    touches.push(changed.clone());
                }
            }
            "up" | "cancel" => {
                if let Some(index) = existing {
                    touches.remove(index);
                }
            }
            _ => {}
        }
        (changed.target, changed, touches.clone())
    });
    let target_touches = active
        .iter()
        .filter(|touch| touch.target == event_target)
        .cloned()
        .collect::<Vec<_>>();
    let event = w3cos_core::class::construct(
        &crate::web_events::event_subclass_class("TouchEvent"),
        vec![
            Value::string(&event_type_name(event_type)),
            Value::object(HashMap::from([
                ("bubbles".to_string(), Value::Bool(true)),
                (
                    "cancelable".to_string(),
                    Value::Bool(event_type != EventType::TouchCancel),
                ),
                ("composed".to_string(), Value::Bool(true)),
                ("view".to_string(), window_value()),
                ("touches".to_string(), touch_list(&active)),
                ("targetTouches".to_string(), touch_list(&target_touches)),
                (
                    "changedTouches".to_string(),
                    touch_list(std::slice::from_ref(&changed)),
                ),
                ("altKey".to_string(), Value::Bool(alt_key)),
                ("ctrlKey".to_string(), Value::Bool(ctrl_key)),
                ("metaKey".to_string(), Value::Bool(meta_key)),
                ("shiftKey".to_string(), Value::Bool(shift_key)),
            ])),
        ],
    );
    event.set_property("isTrusted", Value::Bool(true));
    let mut native_event = Event::new(event_type, NodeId::from_u32(event_target));
    native_event.bubbles = true;
    native_event.cancelable = event_type != EventType::TouchCancel;
    native_event.composed = true;
    dispatch_event_value_to_js(&native_event, event)
}

fn pointer_capture_error_value(pointer_id: i64) -> Value {
    Value::object(HashMap::from([
        ("name".to_string(), Value::string("NotFoundError")),
        (
            "message".to_string(),
            Value::string(&format!("Pointer {pointer_id} is not active")),
        ),
    ]))
}

fn pointer_capture_error(pointer_id: i64) -> ! {
    w3cos_core::throw_value(pointer_capture_error_value(pointer_id))
}

fn dispatch_pointer_capture_event(target: u32, event_type: &str, pointer_id: i64) {
    let pointer_type = ACTIVE_POINTERS.with(|active| {
        active
            .borrow()
            .get(&pointer_id)
            .cloned()
            .unwrap_or_default()
    });
    let mouse = w3cos_dom::events::MouseEventData {
        client_x: 0.0,
        client_y: 0.0,
        page_x: 0.0,
        page_y: 0.0,
        offset_x: 0.0,
        offset_y: 0.0,
        button: 0,
        buttons: 0,
        ctrl_key: false,
        shift_key: false,
        alt_key: false,
        meta_key: false,
    };
    dispatch_sync(
        target,
        event_type_for(event_type),
        EventData::Pointer(w3cos_dom::events::PointerEventData {
            mouse,
            pointer_id: pointer_id as i32,
            pointer_type,
            pressure: 0.0,
            width: 1.0,
            height: 1.0,
            is_primary: true,
        }),
    );
}

fn set_pointer_capture(node: u32, pointer_id: i64) {
    if !ACTIVE_POINTERS.with(|active| active.borrow().contains_key(&pointer_id)) {
        pointer_capture_error(pointer_id);
    }
    let previous = POINTER_CAPTURE.with(|capture| capture.borrow_mut().insert(pointer_id, node));
    if previous == Some(node) {
        return;
    }
    if let Some(previous) = previous {
        dispatch_pointer_capture_event(previous, "lostpointercapture", pointer_id);
    }
    dispatch_pointer_capture_event(node, "gotpointercapture", pointer_id);
}

fn release_pointer_capture(node: u32, pointer_id: i64) {
    if !ACTIVE_POINTERS.with(|active| active.borrow().contains_key(&pointer_id)) {
        pointer_capture_error(pointer_id);
    }
    let released = POINTER_CAPTURE.with(|capture| {
        let mut capture = capture.borrow_mut();
        (capture.get(&pointer_id) == Some(&node))
            .then(|| capture.remove(&pointer_id))
            .flatten()
    });
    if released.is_some() {
        dispatch_pointer_capture_event(node, "lostpointercapture", pointer_id);
    }
}

/// Synchronously bridge the native pointer/mouse sequence into the compiled
/// DOM. Desktop browsers emit the corresponding mouse event after each mouse
/// pointer event; Monaco installs its editor focus handler on `mousedown`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch_native_pointer(
    target: u32,
    phase: &str,
    client_x: f32,
    client_y: f32,
    pointer_id: i64,
    pointer_type: &str,
    button: i16,
    buttons: u16,
    pressure: f32,
    primary: bool,
    alt_key: bool,
    ctrl_key: bool,
    meta_key: bool,
    shift_key: bool,
) -> bool {
    let pointer_type_event = match phase {
        "down" => EventType::PointerDown,
        "up" => EventType::PointerUp,
        "move" => EventType::PointerMove,
        "enter" => EventType::PointerEnter,
        "leave" => EventType::PointerLeave,
        "cancel" => EventType::PointerCancel,
        _ => return false,
    };
    if phase == "down" {
        if let Some(previous) =
            POINTER_CAPTURE.with(|capture| capture.borrow_mut().remove(&pointer_id))
        {
            dispatch_pointer_capture_event(previous, "lostpointercapture", pointer_id);
        }
        ACTIVE_POINTERS.with(|active| {
            active
                .borrow_mut()
                .insert(pointer_id, pointer_type.to_string());
        });
    }
    let pointer_target = POINTER_CAPTURE
        .with(|capture| capture.borrow().get(&pointer_id).copied())
        .unwrap_or(target);
    let mouse = w3cos_dom::events::MouseEventData {
        client_x: client_x as f64,
        client_y: client_y as f64,
        page_x: client_x as f64,
        page_y: client_y as f64,
        offset_x: client_x as f64,
        offset_y: client_y as f64,
        button: button.max(0) as u16,
        buttons,
        ctrl_key,
        shift_key,
        alt_key,
        meta_key,
    };
    let primary = if pointer_type == "touch" {
        ACTIVE_TOUCHES.with(|touches| {
            touches
                .borrow()
                .first()
                .map(|touch| touch.identifier == pointer_id)
                .unwrap_or(phase == "down")
        })
    } else {
        primary
    };
    let pointer_allowed = dispatch_sync(
        pointer_target,
        pointer_type_event,
        EventData::Pointer(w3cos_dom::events::PointerEventData {
            mouse: mouse.clone(),
            pointer_id: pointer_id as i32,
            pointer_type: pointer_type.to_string(),
            pressure,
            width: 1.0,
            height: 1.0,
            is_primary: primary,
        }),
    );
    let mouse_allowed = if pointer_type == "mouse" && phase != "cancel" {
        let mouse_type = match phase {
            "down" => EventType::MouseDown,
            "up" => EventType::MouseUp,
            "move" => EventType::MouseMove,
            "enter" => EventType::MouseEnter,
            "leave" => EventType::MouseLeave,
            _ => return !pointer_allowed,
        };
        dispatch_sync(pointer_target, mouse_type, EventData::Mouse(mouse))
    } else {
        true
    };
    let touch_allowed = if pointer_type == "touch" {
        dispatch_native_touch(
            target, phase, client_x, client_y, pointer_id, pressure, alt_key, ctrl_key, meta_key,
            shift_key,
        )
    } else {
        true
    };
    let prevented = !pointer_allowed || !mouse_allowed || !touch_allowed;
    if matches!(phase, "up" | "cancel") {
        let captured = POINTER_CAPTURE.with(|capture| capture.borrow_mut().remove(&pointer_id));
        if let Some(captured) = captured {
            dispatch_pointer_capture_event(captured, "lostpointercapture", pointer_id);
        }
        ACTIVE_POINTERS.with(|active| {
            active.borrow_mut().remove(&pointer_id);
        });
    }
    prevented
}

fn hit_tested_touch_target(x: f32, y: f32) -> Option<u32> {
    let node = deepest_node_at_point(document_element_id(), x, y)?;
    let rect = dom::bounding_rect(node);
    (rect.width > 0.0 && rect.height > 0.0).then_some(node)
}

fn active_touch_target(pointer_id: i64) -> Option<u32> {
    ACTIVE_TOUCHES.with(|touches| {
        touches
            .borrow()
            .iter()
            .find(|touch| touch.identifier == pointer_id)
            .map(|touch| touch.target)
    })
}

/// Hit-test the live document and dispatch a paired PointerEvent/TouchEvent
/// lifecycle.
///
/// Hosts without a compositor (including `w3cos-mobile::TouchEvent::dispatch`)
/// use CSSOM layout boxes via the same geometry as `document.elementFromPoint`.
/// A `down` that misses every box with positive width/height is ignored. Later `move`/`up`/`cancel` for an
/// active contact stay on that contact's target even if the point has left the
/// box. Android MotionEvent / iOS UITouch adapters are separate work.
pub fn dispatch_hit_tested_touch(
    phase: &str,
    client_x: f32,
    client_y: f32,
    pointer_id: i64,
    pressure: f32,
) -> bool {
    let hit = hit_tested_touch_target(client_x, client_y);
    let target = match phase {
        "down" => hit,
        "move" | "up" | "cancel" => active_touch_target(pointer_id).or(hit),
        _ => return false,
    };
    let Some(target) = target else {
        return false;
    };
    let (button, buttons, pressure) = match phase {
        "down" | "move" => (0_i16, 1_u16, pressure),
        _ => (0, 0, 0.0),
    };
    dispatch_native_pointer(
        target, phase, client_x, client_y, pointer_id, "touch", button, buttons, pressure, true,
        false, false, false, false,
    )
}

pub(crate) fn dispatch_native_click(target: u32) -> bool {
    let prevented = !dispatch_sync(target, EventType::Click, EventData::None);
    if !prevented {
        apply_details_summary_default_action(target);
    }
    #[cfg(target_os = "ios")]
    if !prevented
        && dom::tag_name(target) == "input"
        && dom::get_attribute(target, "type")
            .is_some_and(|value| value.eq_ignore_ascii_case("file"))
    {
        request_ios_file_picker(target);
    }
    prevented
}

fn apply_details_summary_default_action(target: u32) {
    let mut current = Some(target);
    while let Some(node) = current {
        if dom::tag_name(node).eq_ignore_ascii_case("summary") {
            let Some(details) = dom::parent_node(node) else {
                return;
            };
            if !dom::tag_name(details).eq_ignore_ascii_case("details") {
                return;
            }
            if dom::has_attribute(details, "open") {
                dom::remove_attribute(details, "open");
            } else {
                dom::set_attribute(details, "open", "");
            }
            return;
        }
        current = dom::parent_node(node);
    }
}

/// Apply the HTML default action for an activated submit button.
///
/// Returns `None` when `target` is not a submit control or has no ancestor
/// form. Otherwise returns whether the dispatched submit event was canceled.
pub(crate) fn dispatch_native_submit_for_control(target: u32) -> Option<bool> {
    // A click can hit text/span content inside a button. HTML activation
    // behavior belongs to the nearest ancestor submit control, not only the
    // innermost event target.
    let mut control = Some(target);
    let control = loop {
        let node = control?;
        let tag = dom::tag_name(node);
        if tag.eq_ignore_ascii_case("button") || tag.eq_ignore_ascii_case("input") {
            break node;
        }
        control = dom::parent_node(node);
    };
    let tag = dom::tag_name(control);
    let control_type = dom::get_attribute(control, "type").unwrap_or_else(|| {
        if tag.eq_ignore_ascii_case("button") {
            "submit".to_string()
        } else {
            "text".to_string()
        }
    });
    if !control_type.eq_ignore_ascii_case("submit") {
        return None;
    }

    let mut current = dom::parent_node(control);
    while let Some(node) = current {
        if dom::tag_name(node).eq_ignore_ascii_case("form") {
            return Some(!dispatch_sync(
                node,
                event_type_for("submit"),
                EventData::None,
            ));
        }
        current = dom::parent_node(node);
    }
    None
}

/// Synchronously bridge a native keyboard event. Returns true when JS called
/// `preventDefault()`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch_native_key(
    target: u32,
    key: &str,
    code: &str,
    repeat: bool,
    alt_key: bool,
    ctrl_key: bool,
    meta_key: bool,
    shift_key: bool,
    pressed: bool,
) -> bool {
    !dispatch_sync(
        target,
        if pressed {
            EventType::KeyDown
        } else {
            EventType::KeyUp
        },
        EventData::Keyboard(w3cos_dom::events::KeyboardEventData {
            key: key.to_string(),
            code: code.to_string(),
            ctrl_key,
            shift_key,
            alt_key,
            meta_key,
            repeat,
            location: 0,
        }),
    )
}

/// Bridge the cancelable `beforeinput` phase. Returns true when canceled.
pub(crate) fn dispatch_native_before_input(
    target: u32,
    data: &str,
    input_type: &str,
    is_composing: bool,
) -> bool {
    !dispatch_sync(
        target,
        EventType::BeforeInput,
        EventData::BeforeInput {
            data: (!data.is_empty()).then(|| data.to_string()),
            input_type: w3cos_dom::events::InputType::from_str(input_type),
            is_composing,
            target_ranges: Vec::new(),
        },
    )
}

fn utf16_offset_to_byte(value: &str, offset: usize) -> usize {
    let len = value.encode_utf16().count();
    if offset >= len {
        return value.len();
    }
    let mut units = 0usize;
    for (byte, ch) in value.char_indices() {
        if units >= offset {
            return byte;
        }
        units += ch.len_utf16();
    }
    value.len()
}

fn text_control_selection(node: u32, len: usize) -> (usize, usize, String) {
    let mut start = get_expando(node, "selectionStart")
        .map(|value| value.to_number().max(0.0) as usize)
        .unwrap_or(0)
        .min(len);
    let end = get_expando(node, "selectionEnd")
        .map(|value| value.to_number().max(0.0) as usize)
        .unwrap_or(start)
        .min(len);
    if start > end {
        start = end;
    }
    let direction = get_expando(node, "selectionDirection")
        .map(|value| value.to_js_string())
        .unwrap_or_else(|| "none".to_string());
    (start, end, direction)
}

fn set_text_control_selection(node: u32, start: usize, end: usize, direction: &str) {
    let len = dom::get_attribute(node, "value")
        .unwrap_or_default()
        .encode_utf16()
        .count();
    let end = end.min(len);
    let start = start.min(end);
    let direction = match direction {
        "forward" | "backward" => direction,
        _ => "none",
    };
    set_expando(node, "selectionStart", Value::Number(start as f64));
    set_expando(node, "selectionEnd", Value::Number(end as f64));
    set_expando(node, "selectionDirection", Value::string(direction));
}

fn set_range_text(node: u32, args: &[Value]) {
    let replacement = arg(args, 0).to_js_string();
    let value = dom::get_attribute(node, "value").unwrap_or_default();
    let len = value.encode_utf16().count();
    let (old_start, old_end, old_direction) = text_control_selection(node, len);
    let explicit_range = args.len() >= 3;
    let start = if explicit_range {
        arg(args, 1).to_number().max(0.0) as usize
    } else {
        old_start
    };
    let end = if explicit_range {
        arg(args, 2).to_number().max(0.0) as usize
    } else {
        old_end
    };
    if start > end {
        w3cos_core::throw_value(Value::object(HashMap::from([
            ("name".to_string(), Value::string("IndexSizeError")),
            (
                "message".to_string(),
                Value::string("The start index is greater than the end index"),
            ),
        ])));
    }
    let start = start.min(len);
    let end = end.min(len);
    let start_byte = utf16_offset_to_byte(&value, start);
    let end_byte = utf16_offset_to_byte(&value, end);
    let edited = format!(
        "{}{}{}",
        &value[..start_byte],
        replacement,
        &value[end_byte..]
    );
    dom::set_attribute(node, "value", &edited);

    let replacement_len = replacement.encode_utf16().count();
    let replacement_end = start + replacement_len;
    let mode = if explicit_range {
        arg(args, 3).to_js_string()
    } else {
        "preserve".to_string()
    };
    let (selection_start, selection_end, direction) = match mode.as_str() {
        "select" => (start, replacement_end, "none"),
        "start" => (start, start, "none"),
        "end" => (replacement_end, replacement_end, "none"),
        _ => {
            let adjust = |position: usize, inside_end: usize| {
                if position > end {
                    position
                        .saturating_sub(end - start)
                        .saturating_add(replacement_len)
                } else if position > start {
                    inside_end
                } else {
                    position
                }
            };
            (
                adjust(old_start, start),
                adjust(old_end, replacement_end),
                old_direction.as_str(),
            )
        }
    };
    set_text_control_selection(node, selection_start, selection_end, direction);
}

/// Compute the value a browser text control would have after applying an
/// edit at its current UTF-16 selection. The actual mutation still happens
/// after the cancelable `beforeinput` phase.
pub(crate) fn text_control_value_after_edit(target: u32, data: &str, input_type: &str) -> String {
    let value = dom::get_attribute(target, "value").unwrap_or_default();
    let len = value.encode_utf16().count();
    let (mut start, end, _) = text_control_selection(target, len);
    if input_type == "deleteContentBackward" && start == end && start > 0 {
        let mut units = 0usize;
        for ch in value.chars() {
            let next = units + ch.len_utf16();
            if next >= start {
                start = units;
                break;
            }
            units = next;
        }
    }

    let start_byte = utf16_offset_to_byte(&value, start);
    let end_byte = utf16_offset_to_byte(&value, end);
    let inserted = if input_type.starts_with("delete") {
        ""
    } else {
        data
    };
    let edited = format!("{}{}{}", &value[..start_byte], inserted, &value[end_byte..]);
    if std::env::var_os("W3COS_INPUT_TRACE").is_some() {
        eprintln!(
            "[W3C OS][DOM EDIT] target={target} type={input_type} selection={start}..{end} old_len={} new_len={}",
            value.encode_utf16().count(),
            edited.encode_utf16().count()
        );
    }
    edited
}

/// Update the DOM control value and synchronously deliver its `input` event.
pub(crate) fn dispatch_native_input(
    target: u32,
    value: &str,
    data: &str,
    input_type: &str,
    is_composing: bool,
) {
    let previous_len = dom::get_attribute(target, "value")
        .unwrap_or_default()
        .encode_utf16()
        .count();
    let selection_start = get_expando(target, "selectionStart")
        .map(|value| value.to_number().max(0.0) as usize)
        .unwrap_or(0);
    let selection_end = get_expando(target, "selectionEnd")
        .map(|value| value.to_number().max(0.0) as usize)
        .unwrap_or(selection_start);
    dom::set_attribute(target, "value", value);
    // Native text controls advance their selection before firing `input`.
    // Editors such as Monaco diff the new value around selectionStart/End;
    // leaving the bridge-side expandos stale makes them discard an otherwise
    // correctly delivered insertion.
    let cursor = if input_type == "deleteContentBackward" {
        selection_start.saturating_sub(if selection_start == selection_end {
            previous_len.saturating_sub(value.encode_utf16().count())
        } else {
            selection_end.saturating_sub(selection_start)
        })
    } else if input_type.starts_with("delete") {
        selection_start
    } else {
        selection_start + data.encode_utf16().count()
    }
    .min(value.encode_utf16().count()) as f64;
    set_expando(target, "selectionStart", Value::Number(cursor));
    set_expando(target, "selectionEnd", Value::Number(cursor));
    let _ = dispatch_sync(
        target,
        EventType::Input,
        EventData::Input {
            data: (!data.is_empty()).then(|| data.to_string()),
            input_type: w3cos_dom::events::InputType::from_str(input_type),
            is_composing,
        },
    );
}

/// `element.dispatchEvent(eventValue)` — reads `.type` (and `.detail`,
/// `.bubbles`) from the given object and dispatches synchronously.
fn install_compat_event_controls(event: &Value) {
    for (name, default) in [
        ("__pd", Value::Bool(false)),
        ("__sp", Value::Bool(false)),
        ("__sip", Value::Bool(false)),
        ("returnValue", Value::Bool(true)),
    ] {
        if event.get_property(name).is_undefined() {
            event.set_property(name, default);
        }
    }
    if !event.get_property("preventDefault").is_function() {
        let event = event.clone();
        event.clone().set_property(
            "preventDefault",
            func(move |_, _| {
                if event.get_property("cancelable").to_bool() {
                    event.set_property("__pd", Value::Bool(true));
                    event.set_property("returnValue", Value::Bool(false));
                }
                Value::Undefined
            }),
        );
    }
    if !event.get_property("stopPropagation").is_function() {
        let event = event.clone();
        event.clone().set_property(
            "stopPropagation",
            func(move |_, _| {
                event.set_property("__sp", Value::Bool(true));
                Value::Undefined
            }),
        );
    }
    if !event.get_property("stopImmediatePropagation").is_function() {
        let event = event.clone();
        event.clone().set_property(
            "stopImmediatePropagation",
            func(move |_, _| {
                event.set_property("__sp", Value::Bool(true));
                event.set_property("__sip", Value::Bool(true));
                Value::Undefined
            }),
        );
    }
    if event
        .get_property("__w3cos_getter_defaultPrevented")
        .is_undefined()
    {
        let event = event.clone();
        event.clone().set_property(
            "__w3cos_getter_defaultPrevented",
            func(move |_, _| event.get_property("__pd")),
        );
    }
    if event
        .get_property("__w3cos_getter_cancelBubble")
        .is_undefined()
    {
        let event = event.clone();
        event.clone().set_property(
            "__w3cos_getter_cancelBubble",
            func(move |_, _| event.get_property("__sp")),
        );
    }
}

fn js_dispatch_event(node: u32, event_val: Value) -> bool {
    let type_name = event_val.get_property("type").to_js_string();
    if type_name.is_empty() || type_name == "undefined" {
        return true;
    }
    install_compat_event_controls(&event_val);
    let et = event_type_for(&type_name);
    let detail = event_val.get_property("detail");
    let data = if detail.is_nullish() {
        EventData::None
    } else {
        EventData::Custom {
            detail: Some(detail.to_js_string()),
        }
    };
    let mut ev = Event::new(et, NodeId::from_u32(node));
    ev.data = data;
    let bubbles = event_val.get_property("bubbles");
    if !bubbles.is_undefined() {
        ev.bubbles = bubbles.to_bool();
    }
    let cancelable = event_val.get_property("cancelable");
    if !cancelable.is_undefined() {
        ev.cancelable = cancelable.to_bool();
    }
    let composed = event_val.get_property("composed");
    if !composed.is_undefined() {
        ev.composed = composed.to_bool();
    }
    let was_canceled = event_val.get_property("defaultPrevented").to_bool();
    let dispatched = dispatch_event_value_to_js(&ev, event_val.clone());
    if !dispatched {
        event_val.set_property("__pd", Value::Bool(true));
        event_val.set_property("returnValue", Value::Bool(false));
    }
    dispatched && !was_canceled
}

/// Deliver event snapshots taken by native dispatch to JS listeners.
/// Called by [`drain_microtasks`]; returns how many handler calls ran.
fn deliver_pending_events() -> usize {
    let mut ran = 0;
    loop {
        let ev = PENDING_EVENTS.with(|q| {
            let mut q = q.borrow_mut();
            if q.is_empty() {
                None
            } else {
                Some(q.remove(0))
            }
        });
        let Some(ev) = ev else { break };
        let listeners: Vec<Value> = LISTENERS.with(|l| {
            l.borrow()
                .iter()
                .filter(|jl| {
                    jl.node == ev.current_target.as_u32()
                        && jl.event_type == ev.event_type
                        && match ev.event_phase {
                            w3cos_dom::events::EventPhase::Capturing => jl.capture,
                            w3cos_dom::events::EventPhase::Bubbling
                            | w3cos_dom::events::EventPhase::None => !jl.capture,
                            w3cos_dom::events::EventPhase::AtTarget => true,
                        }
                })
                .map(|jl| jl.handler.clone())
                .collect()
        });
        if listeners.is_empty() {
            continue;
        }
        let js_ev = build_event_value(&ev);
        for h in listeners {
            h.call(Value::Undefined, vec![js_ev.clone()]);
            ran += 1;
            if js_ev.get_property("__sip").to_bool() {
                break;
            }
        }
    }
    ran
}

// ── Canvas 2D context ──────────────────────────────────────────────────────

fn farg(args: &[Value], i: usize) -> f32 {
    arg(args, i).to_number() as f32
}

fn image_data_value(data: &crate::canvas2d::ImageData) -> Value {
    let bytes = w3cos_core::class::construct(
        &w3cos_core::binary::typed_array_class("Uint8ClampedArray"),
        vec![js_array(
            data.data.iter().map(|b| Value::Number(*b as f64)).collect(),
        )],
    );
    crate::canvas_web::image_data_value(bytes, data.width, data.height)
}

fn canvas_context_value(node: u32) -> Value {
    if let Some(v) = get_expando(node, "__ctx2d") {
        return v;
    }
    CANVAS_CONTEXTS.with(|c| {
        let mut map = c.borrow_mut();
        map.entry(node).or_insert_with(|| {
            let w = dom::get_attribute(node, "width")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(300);
            let h = dom::get_attribute(node, "height")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(150);
            Rc::new(RefCell::new(
                crate::canvas2d::CanvasRenderingContext2D::new(w, h),
            ))
        });
    });
    let ctx = CANVAS_CONTEXTS.with(|c| c.borrow().get(&node).cloned().unwrap());
    let ctx_get = ctx.clone();
    let generation = realm_generation();

    let handler = ProxyBuilder::new()
        .get(move |target, key, _receiver| {
            if !bridge_realm_is_current(generation) {
                return Value::Undefined;
            }
            if let Some(v) = get_expando(node, &format!("ctx:{key}")) {
                return v;
            }
            let live = canvas_ctx_get(node, &ctx_get, key);
            if live.is_undefined() {
                target.get_property(key)
            } else {
                live
            }
        })
        .set(move |_target, key, value, _receiver| {
            if !bridge_realm_is_current(generation) {
                return true;
            }
            if key != "lineDashOffset" {
                set_expando(node, &format!("ctx:{key}"), value.clone());
            }
            match key {
                "fillStyle" | "strokeStyle" if value.is_object() => {
                    CANVAS_PAINT_STYLE_WARNED.with(|warned| {
                        if !warned.replace(true) {
                            eprintln!(
                                "[w3cos] warning: CanvasGradient and CanvasPattern values retain \
                                 browser identity and stops/transforms; gradient and pattern raster \
                                 paint requires renderer integration"
                            );
                        }
                    });
                }
                "fillStyle" => ctx.borrow_mut().set_fill_style(&value.to_js_string()),
                "strokeStyle" => ctx.borrow_mut().set_stroke_style(&value.to_js_string()),
                "lineWidth" => ctx.borrow_mut().set_line_width(value.to_number() as f32),
                "lineDashOffset" => ctx
                    .borrow_mut()
                    .set_line_dash_offset(value.to_number() as f32),
                "font" => ctx.borrow_mut().set_font(&value.to_js_string()),
                "globalAlpha" => ctx.borrow_mut().set_global_alpha(value.to_number() as f32),
                "textAlign" => ctx.borrow_mut().set_text_align(&value.to_js_string()),
                "textBaseline" => ctx.borrow_mut().set_text_baseline(&value.to_js_string()),
                "shadowBlur" => ctx.borrow_mut().set_shadow_blur(value.to_number() as f32),
                "shadowOffsetX" => ctx
                    .borrow_mut()
                    .set_shadow_offset_x(value.to_number() as f32),
                "shadowOffsetY" => ctx
                    .borrow_mut()
                    .set_shadow_offset_y(value.to_number() as f32),
                "shadowColor" => ctx.borrow_mut().set_shadow_color(&value.to_js_string()),
                _ => {}
            }
            true
        })
        .build();
    let value = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(
        HashMap::new(),
        handler,
    ))));
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::canvas_web::canvas_rendering_context_2d_class().get_property("prototype"),
    );
    set_expando(node, "__ctx2d", value.clone());
    value
}

fn canvas_ctx_get(
    node: u32,
    ctx: &Rc<RefCell<crate::canvas2d::CanvasRenderingContext2D>>,
    key: &str,
) -> Value {
    match key {
        "canvas" => element_value(node),
        "lineDashOffset" => Value::Number(ctx.borrow().state.line_dash_offset as f64),
        "fillRect" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().fill_rect(
                    farg(&args, 0),
                    farg(&args, 1),
                    farg(&args, 2),
                    farg(&args, 3),
                );
                Value::Undefined
            })
        }
        "clearRect" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().clear_rect(
                    farg(&args, 0),
                    farg(&args, 1),
                    farg(&args, 2),
                    farg(&args, 3),
                );
                Value::Undefined
            })
        }
        "strokeRect" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().stroke_rect(
                    farg(&args, 0),
                    farg(&args, 1),
                    farg(&args, 2),
                    farg(&args, 3),
                );
                Value::Undefined
            })
        }
        "fillText" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().fill_text(
                    &arg(&args, 0).to_js_string(),
                    farg(&args, 1),
                    farg(&args, 2),
                    None,
                );
                Value::Undefined
            })
        }
        "strokeText" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().stroke_text(
                    &arg(&args, 0).to_js_string(),
                    farg(&args, 1),
                    farg(&args, 2),
                    None,
                );
                Value::Undefined
            })
        }
        "measureText" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                let m = ctx.borrow().measure_text(&arg(&args, 0).to_js_string());
                let mut props = HashMap::new();
                props.insert("width".to_string(), Value::Number(m.width as f64));
                props.insert(
                    "actualBoundingBoxAscent".to_string(),
                    Value::Number(m.actual_bounding_box_ascent as f64),
                );
                props.insert(
                    "actualBoundingBoxDescent".to_string(),
                    Value::Number(m.actual_bounding_box_descent as f64),
                );
                props.insert(
                    "fontBoundingBoxAscent".to_string(),
                    Value::Number(m.font_bounding_box_ascent as f64),
                );
                props.insert(
                    "fontBoundingBoxDescent".to_string(),
                    Value::Number(m.font_bounding_box_descent as f64),
                );
                crate::canvas_web::text_metrics_value(props)
            })
        }
        "createLinearGradient" => {
            func(move |_, args| crate::canvas_web::canvas_gradient_value("linear", &args))
        }
        "createRadialGradient" => {
            func(move |_, args| crate::canvas_web::canvas_gradient_value("radial", &args))
        }
        "createConicGradient" => {
            func(move |_, args| crate::canvas_web::canvas_gradient_value("conic", &args))
        }
        "createPattern" => func(move |_, args| {
            let source = arg(&args, 0);
            if !source.is_object() {
                return Value::Null;
            }
            let repetition = args
                .get(1)
                .map(Value::to_js_string)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "repeat".to_string());
            if !matches!(
                repetition.as_str(),
                "repeat" | "repeat-x" | "repeat-y" | "no-repeat"
            ) {
                w3cos_core::throw_value(w3cos_core::error_instance(
                    "SyntaxError",
                    vec![Value::string("Invalid CanvasPattern repetition mode")],
                ));
            }
            crate::canvas_web::canvas_pattern_value(source, &repetition)
        }),
        "beginPath" => {
            let ctx = ctx.clone();
            func(move |_, _| {
                ctx.borrow_mut().begin_path();
                Value::Undefined
            })
        }
        "closePath" => {
            let ctx = ctx.clone();
            func(move |_, _| {
                ctx.borrow_mut().close_path();
                Value::Undefined
            })
        }
        "moveTo" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().move_to(farg(&args, 0), farg(&args, 1));
                Value::Undefined
            })
        }
        "lineTo" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().line_to(farg(&args, 0), farg(&args, 1));
                Value::Undefined
            })
        }
        "rect" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().rect(
                    farg(&args, 0),
                    farg(&args, 1),
                    farg(&args, 2),
                    farg(&args, 3),
                );
                Value::Undefined
            })
        }
        "arc" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().arc(
                    farg(&args, 0),
                    farg(&args, 1),
                    farg(&args, 2),
                    farg(&args, 3),
                    farg(&args, 4),
                    arg(&args, 5).to_bool(),
                );
                Value::Undefined
            })
        }
        "quadraticCurveTo" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().quadratic_curve_to(
                    farg(&args, 0),
                    farg(&args, 1),
                    farg(&args, 2),
                    farg(&args, 3),
                );
                Value::Undefined
            })
        }
        "bezierCurveTo" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().bezier_curve_to(
                    farg(&args, 0),
                    farg(&args, 1),
                    farg(&args, 2),
                    farg(&args, 3),
                    farg(&args, 4),
                    farg(&args, 5),
                );
                Value::Undefined
            })
        }
        "fill" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                if let Some(ops) = args.first().and_then(crate::canvas_web::path_ops) {
                    apply_path_ops(&mut ctx.borrow_mut(), &ops);
                }
                ctx.borrow_mut().fill();
                Value::Undefined
            })
        }
        "stroke" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                if let Some(ops) = args.first().and_then(crate::canvas_web::path_ops) {
                    apply_path_ops(&mut ctx.borrow_mut(), &ops);
                }
                ctx.borrow_mut().stroke();
                Value::Undefined
            })
        }
        "save" => {
            let ctx = ctx.clone();
            func(move |_, _| {
                ctx.borrow_mut().save();
                Value::Undefined
            })
        }
        "restore" => {
            let ctx = ctx.clone();
            func(move |_, _| {
                ctx.borrow_mut().restore();
                Value::Undefined
            })
        }
        "translate" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().translate(farg(&args, 0), farg(&args, 1));
                Value::Undefined
            })
        }
        "scale" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().scale(farg(&args, 0), farg(&args, 1));
                Value::Undefined
            })
        }
        "setTransform" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                ctx.borrow_mut().set_transform(
                    farg(&args, 0),
                    farg(&args, 1),
                    farg(&args, 2),
                    farg(&args, 3),
                    farg(&args, 4),
                    farg(&args, 5),
                );
                Value::Undefined
            })
        }
        "resetTransform" => {
            let ctx = ctx.clone();
            func(move |_, _| {
                ctx.borrow_mut().reset_transform();
                Value::Undefined
            })
        }
        "getImageData" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                let data = ctx.borrow().get_image_data(
                    arg(&args, 0).to_u32(),
                    arg(&args, 1).to_u32(),
                    arg(&args, 2).to_u32(),
                    arg(&args, 3).to_u32(),
                );
                image_data_value(&data)
            })
        }
        "createImageData" => func(move |_, args| {
            let data = crate::canvas2d::CanvasRenderingContext2D::create_image_data(
                arg(&args, 0).to_u32(),
                arg(&args, 1).to_u32(),
            );
            image_data_value(&data)
        }),
        "putImageData" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                let obj = arg(&args, 0);
                let w = obj.get_property("width").to_u32();
                let h = obj.get_property("height").to_u32();
                let bytes: Vec<u8> = obj
                    .get_property("data")
                    .iter()
                    .map(|v| v.to_u32() as u8)
                    .collect();
                let data = crate::canvas2d::ImageData::from_bytes(bytes, w, h);
                ctx.borrow_mut().put_image_data(
                    &data,
                    arg(&args, 1).to_i32(),
                    arg(&args, 2).to_i32(),
                );
                Value::Undefined
            })
        }
        "setLineDash" => {
            let ctx = ctx.clone();
            func(move |_, args| {
                let segments = arg(&args, 0)
                    .iter()
                    .map(|value| value.to_number() as f32)
                    .collect();
                ctx.borrow_mut().set_line_dash(segments);
                Value::Undefined
            })
        }
        "getLineDash" => {
            let ctx = ctx.clone();
            func(move |_, _| {
                js_array(
                    ctx.borrow()
                        .get_line_dash()
                        .into_iter()
                        .map(|value| Value::Number(value as f64))
                        .collect(),
                )
            })
        }
        "drawImage" => {
            let target = ctx.clone();
            func(move |_, args| {
                let source = arg(&args, 0);
                let source_node = node_id_of(&source).or_else(|| {
                    let backing = source.get_property("__w3cos_canvas");
                    node_id_of(&backing)
                });
                let source_context = source_node.and_then(|node| {
                    CANVAS_CONTEXTS.with(|contexts| contexts.borrow().get(&node).cloned())
                });
                let Some(source_context) = source_context else {
                    DRAW_IMAGE_SOURCE_WARNED.with(|warned| {
                        if !warned.replace(true) {
                            eprintln!(
                                "[w3cos] warning: CanvasRenderingContext2D.drawImage currently \
                                 supports canvas-backed sources; undecoded image/video sources \
                                 are ignored"
                            );
                        }
                    });
                    return Value::Undefined;
                };
                let (source_width, source_height, source_pixels) = {
                    let source = source_context.borrow();
                    (source.width, source.height, source.pixels().to_vec())
                };
                let (sx, sy, sw, sh, dx, dy, dw, dh) = match args.len() {
                    3 => (
                        0.0,
                        0.0,
                        source_width as f32,
                        source_height as f32,
                        farg(&args, 1),
                        farg(&args, 2),
                        source_width as f32,
                        source_height as f32,
                    ),
                    5 => (
                        0.0,
                        0.0,
                        source_width as f32,
                        source_height as f32,
                        farg(&args, 1),
                        farg(&args, 2),
                        farg(&args, 3),
                        farg(&args, 4),
                    ),
                    9.. => (
                        farg(&args, 1),
                        farg(&args, 2),
                        farg(&args, 3),
                        farg(&args, 4),
                        farg(&args, 5),
                        farg(&args, 6),
                        farg(&args, 7),
                        farg(&args, 8),
                    ),
                    _ => return Value::Undefined,
                };
                target.borrow_mut().draw_image_rgba(
                    &source_pixels,
                    source_width,
                    source_height,
                    sx,
                    sy,
                    sw,
                    sh,
                    dx,
                    dy,
                    dw,
                    dh,
                );
                Value::Undefined
            })
        }
        _ => Value::Undefined,
    }
}

fn apply_path_ops(
    context: &mut crate::canvas2d::CanvasRenderingContext2D,
    ops: &[crate::canvas2d::PathOp],
) {
    context.begin_path();
    for op in ops {
        match *op {
            crate::canvas2d::PathOp::MoveTo(x, y) => context.move_to(x, y),
            crate::canvas2d::PathOp::LineTo(x, y) => context.line_to(x, y),
            crate::canvas2d::PathOp::QuadraticCurveTo(a, b, c, d) => {
                context.quadratic_curve_to(a, b, c, d)
            }
            crate::canvas2d::PathOp::BezierCurveTo(a, b, c, d, e, f) => {
                context.bezier_curve_to(a, b, c, d, e, f)
            }
            crate::canvas2d::PathOp::Arc(x, y, radius, start, end, ccw) => {
                context.arc(x, y, radius, start, end, ccw)
            }
            crate::canvas2d::PathOp::Rect(x, y, width, height) => context.rect(x, y, width, height),
            crate::canvas2d::PathOp::ClosePath => context.close_path(),
            crate::canvas2d::PathOp::ArcTo(..) | crate::canvas2d::PathOp::Ellipse(..) => {}
        }
    }
}

// ── Range / Selection ──────────────────────────────────────────────────────

fn range_hidden(v: &Value, key: &str) -> u32 {
    v.get_property(key).to_u32()
}

fn child_below_ancestor(ancestor: u32, mut node: u32) -> Option<u32> {
    while let Some(parent) = dom::parent_node(node) {
        if parent == ancestor {
            return Some(node);
        }
        node = parent;
    }
    None
}

fn compare_boundary_points(
    left_container: u32,
    left_offset: u32,
    right_container: u32,
    right_offset: u32,
) -> Option<Ordering> {
    if left_container == 0 || right_container == 0 {
        return None;
    }
    if left_container == right_container {
        return Some(left_offset.cmp(&right_offset));
    }
    if tree_root(left_container) != tree_root(right_container) {
        return None;
    }

    if let Some(child) = child_below_ancestor(left_container, right_container) {
        let child_index = dom::children(left_container)
            .iter()
            .position(|candidate| *candidate == child)? as u32;
        return Some(if child_index < left_offset {
            Ordering::Greater
        } else {
            Ordering::Less
        });
    }
    if let Some(child) = child_below_ancestor(right_container, left_container) {
        let child_index = dom::children(right_container)
            .iter()
            .position(|candidate| *candidate == child)? as u32;
        return Some(if child_index < right_offset {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }

    let mut left_branch = left_container;
    let mut left_ancestors = HashMap::new();
    while let Some(parent) = dom::parent_node(left_branch) {
        left_ancestors.insert(parent, left_branch);
        left_branch = parent;
    }
    let mut right_branch = right_container;
    while let Some(parent) = dom::parent_node(right_branch) {
        if let Some(left_child) = left_ancestors.get(&parent) {
            let children = dom::children(parent);
            let left_index = children.iter().position(|child| child == left_child)?;
            let right_index = children
                .iter()
                .position(|child| *child == right_branch)?;
            return Some(left_index.cmp(&right_index));
        }
        right_branch = parent;
    }
    None
}

fn range_intersects_node(range: &Value, node: u32) -> bool {
    let Some(parent) = dom::parent_node(node) else {
        return false;
    };
    let Some(index) = dom::children(parent)
        .iter()
        .position(|child| *child == node)
        .map(|index| index as u32)
    else {
        return false;
    };
    let start_container = range_hidden(range, "__sc");
    let end_container = range_hidden(range, "__ec");
    compare_boundary_points(start_container, range_hidden(range, "__so"), parent, index + 1)
        .is_some_and(|ordering| ordering == Ordering::Less)
        && compare_boundary_points(
            end_container,
            range_hidden(range, "__eo"),
            parent,
            index,
        )
        .is_some_and(|ordering| ordering == Ordering::Greater)
}

fn adjust_live_ranges_for_removal(node: u32) {
    let Some(parent) = dom::parent_node(node) else {
        return;
    };
    let Some(index) = dom::children(parent)
        .iter()
        .position(|child| *child == node)
        .map(|index| index as u32)
    else {
        return;
    };
    LIVE_RANGES.with(|ranges| {
        for range in ranges.borrow().iter() {
            for (container_key, offset_key) in [("__sc", "__so"), ("__ec", "__eo")] {
                let container = range_hidden(range, container_key);
                let offset = range_hidden(range, offset_key);
                if container == node || (container != 0 && is_ancestor_of(node, container)) {
                    range.set_property(container_key, Value::Number(parent as f64));
                    range.set_property(offset_key, Value::Number(index as f64));
                } else if container == parent && offset > index {
                    range.set_property(offset_key, Value::Number((offset - 1) as f64));
                }
            }
        }
    });
}

fn adjust_live_ranges_for_insertion(parent: u32, index: u32) {
    LIVE_RANGES.with(|ranges| {
        for range in ranges.borrow().iter() {
            for (container_key, offset_key) in [("__sc", "__so"), ("__ec", "__eo")] {
                if range_hidden(range, container_key) == parent {
                    let offset = range_hidden(range, offset_key);
                    if offset > index {
                        range.set_property(offset_key, Value::Number((offset + 1) as f64));
                    }
                }
            }
        }
    });
}

fn range_to_w3cos(v: &Value) -> w3cos_dom::selection::Range {
    let mut r = w3cos_dom::selection::Range::new();
    r.set_start(
        NodeId::from_u32(range_hidden(v, "__sc")),
        range_hidden(v, "__so"),
    );
    r.set_end(
        NodeId::from_u32(range_hidden(v, "__ec")),
        range_hidden(v, "__eo"),
    );
    r
}

fn range_warn_complex() {
    RANGE_COMPLEX_WARNING_EMITTED.with(|warned| {
        if !warned.replace(true) {
            eprintln!(
                "[W3C OS][compat warning] Range operations spanning different containers use \
                 text-only fallback; same-container text and child ranges preserve DOM nodes"
            );
        }
    });
}

fn range_client_rects(v: &Value) -> Vec<w3cos_dom::DOMRect> {
    let start_container = range_hidden(v, "__sc");
    let end_container = range_hidden(v, "__ec");
    let start = range_hidden(v, "__so") as usize;
    let end = range_hidden(v, "__eo") as usize;
    if start_container == end_container && start == end {
        return Vec::new();
    }
    if start_container == end_container {
        if dom::node_type(start_container) == 3 {
            return vec![dom::bounding_rect(start_container)];
        }
        let children = dom::children(start_container);
        let from = start.min(children.len());
        let to = end.min(children.len()).max(from);
        return children[from..to]
            .iter()
            .map(|child| dom::bounding_rect(*child))
            .collect();
    }

    range_warn_complex();
    [start_container, end_container]
        .into_iter()
        .map(dom::bounding_rect)
        .collect()
}

fn union_rects(rects: &[w3cos_dom::DOMRect]) -> w3cos_dom::DOMRect {
    let Some(first) = rects.first() else {
        return w3cos_dom::DOMRect::zero();
    };
    let (mut left, mut top, mut right, mut bottom) =
        (first.left(), first.top(), first.right(), first.bottom());
    for rect in &rects[1..] {
        left = left.min(rect.left());
        top = top.min(rect.top());
        right = right.max(rect.right());
        bottom = bottom.max(rect.bottom());
    }
    w3cos_dom::DOMRect::new(left, top, right - left, bottom - top)
}

fn range_fragment(v: &Value, extract: bool) -> Value {
    let fragment = dom::create_document_fragment();
    let start_container = range_hidden(v, "__sc");
    let end_container = range_hidden(v, "__ec");
    let start = range_hidden(v, "__so") as usize;
    let end = range_hidden(v, "__eo") as usize;

    if start_container == end_container {
        if dom::node_type(start_container) == 3 {
            let text = dom::get_text_content(start_container).unwrap_or_default();
            let chars = text.chars().collect::<Vec<_>>();
            let from = start.min(chars.len());
            let to = end.min(chars.len()).max(from);
            let selected = chars[from..to].iter().collect::<String>();
            if !selected.is_empty() {
                dom::append_child(fragment, dom::create_text_node(&selected));
            }
            if extract {
                let remaining = chars[..from]
                    .iter()
                    .chain(chars[to..].iter())
                    .collect::<String>();
                dom::set_text_content(start_container, &remaining);
                v.set_property("__ec", Value::Number(start_container as f64));
                v.set_property("__eo", Value::Number(from as f64));
                v.set_property("__sc", Value::Number(start_container as f64));
                v.set_property("__so", Value::Number(from as f64));
            }
            return element_value(fragment);
        }

        let children = dom::children(start_container);
        let from = start.min(children.len());
        let to = end.min(children.len()).max(from);
        for child in children[from..to].iter().copied() {
            let node = if extract {
                child
            } else {
                dom::clone_node(child, true)
            };
            dom::append_child(fragment, node);
        }
        if extract {
            v.set_property("__ec", Value::Number(start_container as f64));
            v.set_property("__eo", Value::Number(from as f64));
            v.set_property("__sc", Value::Number(start_container as f64));
            v.set_property("__so", Value::Number(from as f64));
        }
        return element_value(fragment);
    }

    let start_parent = dom::parent_node(start_container);
    let end_parent = dom::parent_node(end_container);
    if start_parent == end_parent
        && start_parent.is_some()
        && matches!(dom::node_type(start_container), 3 | 4 | 7 | 8)
        && matches!(dom::node_type(end_container), 3 | 4 | 7 | 8)
    {
        let parent = start_parent.expect("matching character-data parents");
        let children = dom::children(parent);
        let start_index = children.iter().position(|child| *child == start_container);
        let end_index = children.iter().position(|child| *child == end_container);
        if let (Some(start_index), Some(end_index)) = (start_index, end_index)
            && start_index < end_index
        {
            let start_text = dom::get_text_content(start_container).unwrap_or_default();
            let end_text = dom::get_text_content(end_container).unwrap_or_default();
            let start_chars = start_text.chars().collect::<Vec<_>>();
            let end_chars = end_text.chars().collect::<Vec<_>>();
            let from = start.min(start_chars.len());
            let to = end.min(end_chars.len());
            let mut selected = start_chars[from..].iter().collect::<String>();
            for child in &children[start_index + 1..end_index] {
                selected.push_str(&dom::get_descendant_text_content(*child));
            }
            selected.extend(end_chars[..to].iter());
            if !selected.is_empty() {
                dom::append_child(fragment, dom::create_text_node(&selected));
            }
            if extract {
                dom::set_text_content(
                    start_container,
                    &start_chars[..from].iter().collect::<String>(),
                );
                for child in children[start_index + 1..end_index].iter().copied() {
                    dom::remove_child(parent, child);
                }
                dom::set_text_content(end_container, &end_chars[to..].iter().collect::<String>());
                v.set_property("__ec", Value::Number(start_container as f64));
                v.set_property("__eo", Value::Number(from as f64));
                v.set_property("__sc", Value::Number(start_container as f64));
                v.set_property("__so", Value::Number(from as f64));
            }
            return element_value(fragment);
        }
    }

    range_warn_complex();
    let range = range_to_w3cos(v);
    let text = if extract {
        let text = dom::with_document_mut(|doc| range.extract_contents(doc));
        dom::touch_document();
        text
    } else {
        dom::with_document(|doc| range.clone_contents(doc))
    };
    if !text.is_empty() {
        dom::append_child(fragment, dom::create_text_node(&text));
    }
    element_value(fragment)
}

fn insert_dom_node(parent: u32, offset: usize, node: u32) {
    let nodes = if dom::node_type(node) == 11 {
        dom::children(node)
    } else {
        vec![node]
    };
    let mut reference = dom::children(parent).get(offset).copied();
    for node in nodes {
        if let Some(reference_node) = reference {
            dom::insert_before(parent, node, reference_node);
        } else {
            dom::append_child(parent, node);
        }
        reference = dom::next_sibling(node);
    }
}

fn range_insert_node(v: &Value, node: u32) {
    let container = range_hidden(v, "__sc");
    let offset = range_hidden(v, "__so") as usize;
    if dom::node_type(container) == 3 {
        let Some(parent) = dom::parent_node(container) else {
            return;
        };
        let text = dom::get_text_content(container).unwrap_or_default();
        let chars = text.chars().collect::<Vec<_>>();
        let split = offset.min(chars.len());
        let before = chars[..split].iter().collect::<String>();
        let after = chars[split..].iter().collect::<String>();
        dom::set_text_content(container, &before);
        let reference = dom::next_sibling(container);
        let after_node = dom::create_text_node(&after);
        if let Some(reference) = reference {
            dom::insert_before(parent, after_node, reference);
        } else {
            dom::append_child(parent, after_node);
        }
        let insertion_index = dom::children(parent)
            .iter()
            .position(|child| *child == after_node)
            .unwrap_or(dom::children(parent).len());
        insert_dom_node(parent, insertion_index, node);
    } else {
        insert_dom_node(container, offset, node);
    }
}

fn set_range_around_node(v: &Value, node: u32) {
    if let Some(parent) = dom::parent_node(node)
        && let Some(index) = dom::children(parent)
            .iter()
            .position(|child| *child == node)
    {
        v.set_property("__sc", Value::Number(parent as f64));
        v.set_property("__so", Value::Number(index as f64));
        v.set_property("__ec", Value::Number(parent as f64));
        v.set_property("__eo", Value::Number((index + 1) as f64));
    }
}

pub(crate) fn range_value(sc: u32, so: u32, ec: u32, eo: u32) -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert("__sc".to_string(), Value::Number(sc as f64));
    props.insert("__so".to_string(), Value::Number(so as f64));
    props.insert("__ec".to_string(), Value::Number(ec as f64));
    props.insert("__eo".to_string(), Value::Number(eo as f64));
    let value = Value::object(props);

    value.set_property(
        "__w3cos_getter_startContainer",
        func({
            let v = value.clone();
            move |_, _| element_or_null(Some(range_hidden(&v, "__sc")))
        }),
    );
    value.set_property(
        "__w3cos_getter_endContainer",
        func({
            let v = value.clone();
            move |_, _| element_or_null(Some(range_hidden(&v, "__ec")))
        }),
    );
    for (key, hidden) in [("startOffset", "__so"), ("endOffset", "__eo")] {
        value.set_property(
            &format!("__w3cos_getter_{key}"),
            func({
                let v = value.clone();
                move |_, _| Value::Number(range_hidden(&v, hidden) as f64)
            }),
        );
    }
    value.set_property(
        "__w3cos_getter_collapsed",
        func({
            let v = value.clone();
            move |_, _| {
                Value::Bool(
                    range_hidden(&v, "__sc") == range_hidden(&v, "__ec")
                        && range_hidden(&v, "__so") == range_hidden(&v, "__eo"),
                )
            }
        }),
    );
    value.set_property(
        "__w3cos_getter_commonAncestorContainer",
        func({
            let v = value.clone();
            move |_, _| {
                // Deepest node that is an ancestor of both endpoints.
                let sc = range_hidden(&v, "__sc");
                let ec = range_hidden(&v, "__ec");
                let mut cur = Some(sc);
                while let Some(id) = cur {
                    if id == ec || is_ancestor_of(id, ec) {
                        return element_value(id);
                    }
                    cur = dom::parent_node(id);
                }
                document_value()
            }
        }),
    );
    value.set_property(
        "setStart",
        func({
            let v = value.clone();
            move |_, args| {
                if let Some(n) = node_id_of(&arg(&args, 0)) {
                    v.set_property("__sc", Value::Number(n as f64));
                    v.set_property("__so", Value::Number(arg(&args, 1).to_number()));
                }
                Value::Undefined
            }
        }),
    );
    value.set_property(
        "setEnd",
        func({
            let v = value.clone();
            move |_, args| {
                if let Some(n) = node_id_of(&arg(&args, 0)) {
                    v.set_property("__ec", Value::Number(n as f64));
                    v.set_property("__eo", Value::Number(arg(&args, 1).to_number()));
                }
                Value::Undefined
            }
        }),
    );
    value.set_property(
        "collapse",
        func({
            let v = value.clone();
            move |_, args| {
                if arg(&args, 0).to_bool() {
                    let sc = v.get_property("__sc");
                    let so = v.get_property("__so");
                    v.set_property("__ec", sc);
                    v.set_property("__eo", so);
                } else {
                    let ec = v.get_property("__ec");
                    let eo = v.get_property("__eo");
                    v.set_property("__sc", ec);
                    v.set_property("__so", eo);
                }
                Value::Undefined
            }
        }),
    );
    value.set_property(
        "selectNode",
        func({
            let v = value.clone();
            move |_, args| {
                if let Some(n) = node_id_of(&arg(&args, 0)) {
                    set_range_around_node(&v, n);
                }
                Value::Undefined
            }
        }),
    );
    for (method, start_boundary, after) in [
        ("setStartBefore", true, false),
        ("setStartAfter", true, true),
        ("setEndBefore", false, false),
        ("setEndAfter", false, true),
    ] {
        value.set_property(
            method,
            func({
                let v = value.clone();
                move |_, args| {
                    if let Some(node) = node_id_of(&arg(&args, 0))
                        && let Some(parent) = dom::parent_node(node)
                        && let Some(index) = dom::children(parent)
                            .iter()
                            .position(|child| *child == node)
                    {
                        let offset = index + usize::from(after);
                        let (container_key, offset_key) = if start_boundary {
                            ("__sc", "__so")
                        } else {
                            ("__ec", "__eo")
                        };
                        v.set_property(container_key, Value::Number(parent as f64));
                        v.set_property(offset_key, Value::Number(offset as f64));
                    }
                    Value::Undefined
                }
            }),
        );
    }
    value.set_property(
        "selectNodeContents",
        func({
            let v = value.clone();
            move |_, args| {
                if let Some(n) = node_id_of(&arg(&args, 0)) {
                    let len = match dom::get_text_content(n) {
                        Some(t) if dom::first_child(n).is_none() => t.chars().count() as u32,
                        _ => dom::children(n).len() as u32,
                    };
                    v.set_property("__sc", Value::Number(n as f64));
                    v.set_property("__so", Value::Number(0.0));
                    v.set_property("__ec", Value::Number(n as f64));
                    v.set_property("__eo", Value::Number(len as f64));
                }
                Value::Undefined
            }
        }),
    );
    value.set_property(
        "cloneRange",
        func({
            let v = value.clone();
            move |_, _| {
                range_value(
                    range_hidden(&v, "__sc"),
                    range_hidden(&v, "__so"),
                    range_hidden(&v, "__ec"),
                    range_hidden(&v, "__eo"),
                )
            }
        }),
    );
    value.set_property(
        "intersectsNode",
        func({
            let v = value.clone();
            move |_, args| {
                Value::Bool(
                    node_id_of(&arg(&args, 0))
                        .is_some_and(|node| range_intersects_node(&v, node)),
                )
            }
        }),
    );
    value.set_property(
        "getBoundingClientRect",
        func({
            let v = value.clone();
            move |_, _| rect_value(union_rects(&range_client_rects(&v)))
        }),
    );
    value.set_property(
        "getClientRects",
        func({
            let v = value.clone();
            move |_, _| {
                crate::geometry_web::rect_list(
                    range_client_rects(&v).into_iter().map(rect_value).collect(),
                )
            }
        }),
    );
    value.set_property(
        "toString",
        func({
            let v = value.clone();
            move |_, _| {
                let r = range_to_w3cos(&v);
                Value::string(&dom::with_document(|doc| r.to_string(doc)))
            }
        }),
    );
    value.set_property(
        "cloneContents",
        func({
            let v = value.clone();
            move |_, _| range_fragment(&v, false)
        }),
    );
    value.set_property(
        "deleteContents",
        func({
            let v = value.clone();
            move |_, _| {
                range_fragment(&v, true);
                Value::Undefined
            }
        }),
    );
    value.set_property(
        "extractContents",
        func({
            let v = value.clone();
            move |_, _| range_fragment(&v, true)
        }),
    );
    value.set_property("detach", func(|_, _| Value::Undefined));
    value.set_property(
        "insertNode",
        func({
            let v = value.clone();
            move |_, args| {
                if let Some(node) = node_id_of(&arg(&args, 0)) {
                    range_insert_node(&v, node);
                }
                Value::Undefined
            }
        }),
    );
    value.set_property(
        "surroundContents",
        func({
            let v = value.clone();
            move |_, args| {
                let Some(wrapper) = node_id_of(&arg(&args, 0)) else {
                    return Value::Undefined;
                };
                let fragment = range_fragment(&v, true);
                range_insert_node(&v, wrapper);
                if let Some(fragment) = node_id_of(&fragment) {
                    for child in dom::children(fragment) {
                        dom::append_child(wrapper, child);
                    }
                }
                set_range_around_node(&v, wrapper);
                Value::Undefined
            }
        }),
    );
    value.set_property(
        "createContextualFragment",
        func(|_, args| {
            let fragment = dom::create_document_fragment();
            append_html_fragment(fragment, &arg(&args, 0).to_js_string());
            element_value(fragment)
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("Range"));
    LIVE_RANGES.with(|ranges| ranges.borrow_mut().push(value.clone()));
    value
}

pub(crate) fn static_range_value(args: Vec<Value>) -> Value {
    let init = arg(&args, 0);
    let required = |name: &str| {
        let value = init.get_property(name);
        if value.is_undefined() {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".to_string(), Value::string("TypeError")),
                (
                    "message".to_string(),
                    Value::string(&format!("StaticRange {name} is required")),
                ),
            ])));
        }
        value
    };
    let start_container = required("startContainer");
    let end_container = required("endContainer");
    let Some(sc) = node_id_of(&start_container) else {
        w3cos_core::throw_value(Value::object(HashMap::from([
            ("name".to_string(), Value::string("TypeError")),
            (
                "message".to_string(),
                Value::string("StaticRange startContainer must be a Node"),
            ),
        ])));
    };
    let Some(ec) = node_id_of(&end_container) else {
        w3cos_core::throw_value(Value::object(HashMap::from([
            ("name".to_string(), Value::string("TypeError")),
            (
                "message".to_string(),
                Value::string("StaticRange endContainer must be a Node"),
            ),
        ])));
    };
    let so = required("startOffset").to_u32();
    let eo = required("endOffset").to_u32();

    let value = Value::object(HashMap::from([
        ("__sc".to_string(), Value::Number(sc as f64)),
        ("__so".to_string(), Value::Number(so as f64)),
        ("__ec".to_string(), Value::Number(ec as f64)),
        ("__eo".to_string(), Value::Number(eo as f64)),
    ]));
    value.set_property(
        "__w3cos_getter_startContainer",
        func({
            let value = value.clone();
            move |_, _| element_or_null(Some(range_hidden(&value, "__sc")))
        }),
    );
    value.set_property(
        "__w3cos_getter_endContainer",
        func({
            let value = value.clone();
            move |_, _| element_or_null(Some(range_hidden(&value, "__ec")))
        }),
    );
    for (key, hidden) in [("startOffset", "__so"), ("endOffset", "__eo")] {
        value.set_property(
            &format!("__w3cos_getter_{key}"),
            func({
                let value = value.clone();
                move |_, _| Value::Number(range_hidden(&value, hidden) as f64)
            }),
        );
    }
    value.set_property(
        "__w3cos_getter_collapsed",
        func({
            let value = value.clone();
            move |_, _| {
                Value::Bool(
                    range_hidden(&value, "__sc") == range_hidden(&value, "__ec")
                        && range_hidden(&value, "__so") == range_hidden(&value, "__eo"),
                )
            }
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("StaticRange"));
    value
}

pub(crate) fn text_value(args: Vec<Value>) -> Value {
    let data = args
        .first()
        .filter(|value| !value.is_undefined())
        .map(Value::to_js_string)
        .unwrap_or_default();
    element_value(dom::create_text_node(&data))
}

pub(crate) fn comment_value(args: Vec<Value>) -> Value {
    let data = args
        .first()
        .filter(|value| !value.is_undefined())
        .map(Value::to_js_string)
        .unwrap_or_default();
    element_value(dom::create_comment(&data))
}

fn dom_exception_value(message: &str, name: &str) -> Value {
    w3cos_core::web::dom_exception_instance(message, name)
}

fn dom_exception(message: &str, name: &str) -> ! {
    w3cos_core::throw_value(dom_exception_value(message, name))
}

fn type_error(message: &str) -> ! {
    w3cos_core::throw_value(w3cos_core::error_instance(
        "TypeError",
        vec![Value::string(message)],
    ))
}

fn valid_xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(valid_xml_name_start_character) && chars.all(valid_xml_name_character)
}

fn valid_xml_name_start_character(character: char) -> bool {
    matches!(character, ':' | '_' | 'A'..='Z' | 'a'..='z')
        || matches!(character as u32,
            0x00C0..=0x00D6
                | 0x00D8..=0x00F6
                | 0x00F8..=0x02FF
                | 0x0370..=0x037D
                | 0x037F..=0x1FFF
                | 0x200C..=0x200D
                | 0x2070..=0x218F
                | 0x2C00..=0x2FEF
                | 0x3001..=0xD7FF
                | 0xF900..=0xFDCF
                | 0xFDF0..=0xFFFD
                | 0x10000..=0xEFFFF)
}

fn valid_xml_name_character(character: char) -> bool {
    valid_xml_name_start_character(character)
        || matches!(character, '-' | '.' | '0'..='9' | '\u{00B7}')
        || matches!(character as u32, 0x0300..=0x036F | 0x203F..=0x2040)
}

fn create_cdata_section_value(document: &Value, data: &str) -> Value {
    if document.get_property("contentType").to_js_string() == "text/html" {
        dom_exception(
            "CDATA sections are not supported in HTML documents",
            "NotSupportedError",
        );
    }
    if data.contains("]]>") {
        dom_exception("CDATA data must not contain ']]>'", "InvalidCharacterError");
    }
    let node = dom::create_cdata_section(data);
    set_expando(node, "ownerDocument", document.clone());
    element_value(node)
}

fn create_processing_instruction_value(target: &str, data: &str) -> Value {
    if !valid_xml_name(target) || target.eq_ignore_ascii_case("xml") {
        dom_exception(
            "Processing instruction target is not a valid XML name",
            "InvalidCharacterError",
        );
    }
    if data.contains("?>") {
        dom_exception(
            "Processing instruction data must not contain '?>'",
            "InvalidCharacterError",
        );
    }
    element_value(dom::create_processing_instruction(target, data))
}

fn create_document_type_value(name: &str, public_id: &str, system_id: &str) -> Value {
    if name.contains(['\0', '>'])
        || name
            .chars()
            .any(|character| matches!(character, '\t' | '\n' | '\u{000c}' | '\r' | ' '))
    {
        dom_exception(
            "Document type name is not a valid XML name",
            "InvalidCharacterError",
        );
    }
    let node = dom::create_document_type(name);
    set_expando(node, "publicId", Value::string(public_id));
    set_expando(node, "systemId", Value::string(system_id));
    element_value(node)
}

fn dom_implementation_value() -> Value {
    let value = Value::object(HashMap::new());
    value.set_property(
        "createDocumentType",
        func(|implementation, args| {
            let doctype = create_document_type_value(
                &arg(&args, 0).to_js_string(),
                &arg(&args, 1).to_js_string(),
                &arg(&args, 2).to_js_string(),
            );
            if let Some(node) = node_id_of(&doctype) {
                let owner = implementation.get_property("__w3cos_owner_document");
                if !owner.is_undefined() {
                    set_expando(node, "ownerDocument", owner);
                }
            }
            doctype
        }),
    );
    value.set_property(
        "createDocument",
        func(|_, args| {
            if args.len() < 2 {
                type_error("createDocument requires namespace and qualifiedName");
            }
            let namespace_value = arg(&args, 0);
            let namespace = normalized_namespace(&namespace_value);
            let qualified_name_value = arg(&args, 1);
            let qualified_name = if qualified_name_value.is_null() {
                String::new()
            } else {
                qualified_name_value.to_js_string()
            };
            let doctype = arg(&args, 2);
            if !doctype.is_null()
                && !doctype.is_undefined()
                && !node_id_of(&doctype).is_some_and(|node| dom::node_type(node) == 10)
            {
                type_error("createDocument doctype must be a DocumentType");
            }
            let content_type = match namespace.as_deref() {
                Some(crate::html_parser_state::HTML_NAMESPACE) => "application/xhtml+xml",
                Some("http://www.w3.org/2000/svg") => "image/svg+xml",
                _ => "application/xml",
            };
            if qualified_name.is_empty() {
                let document = empty_document_value(content_type, "XMLDocument");
                if let Some(node) = node_id_of(&doctype)
                    && dom::node_type(node) == 10
                {
                    set_virtual_document_children(&document, vec![doctype]);
                }
                return document;
            }
            validate_and_extract_qualified_name(namespace.as_deref(), &qualified_name);
            let root =
                create_namespaced_element(namespace.as_deref().unwrap_or(""), &qualified_name);
            let document = parsed_document_value(root, content_type, None, None);
            if node_id_of(&doctype).is_some_and(|node| dom::node_type(node) == 10) {
                set_virtual_document_children(&document, vec![doctype, element_value(root)]);
            } else {
                document.set_property("doctype", Value::Null);
            }
            document
        }),
    );
    value.set_property(
        "createHTMLDocument",
        func(|_, args| {
            let html = dom::create_element("html");
            let head = dom::create_element("head");
            let body = dom::create_element("body");
            dom::append_child(html, head);
            dom::append_child(html, body);
            let title = arg(&args, 0);
            if !title.is_undefined() {
                let title_node = dom::create_element("title");
                dom::append_child(title_node, dom::create_text_node(&title.to_js_string()));
                dom::append_child(head, title_node);
            }
            let document = parsed_document_value(html, "text/html", Some(head), Some(body));
            let doctype = create_document_type_value("html", "", "");
            set_virtual_document_children(&document, vec![doctype, element_value(html)]);
            document
        }),
    );
    value.set_property("hasFeature", func(|_, _| Value::Bool(true)));
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::dom_constructors::prototype("DOMImplementation"),
    );
    value
}

fn selection_value() -> Value {
    if let Some(v) = SELECTION_VALUE.with(|s| s.borrow().clone()) {
        return v;
    }
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "__w3cos_getter_rangeCount".to_string(),
        func(|_, _| {
            Value::Number(dom::with_document(|doc| doc.get_selection().range_count()) as f64)
        }),
    );
    props.insert(
        "__w3cos_getter_isCollapsed".to_string(),
        func(|_, _| Value::Bool(dom::with_document(|doc| doc.get_selection().is_collapsed()))),
    );
    props.insert(
        "__w3cos_getter_anchorNode".to_string(),
        func(|_, _| {
            element_or_null(dom::with_document(|doc| {
                doc.get_selection().anchor_node.map(|n| n.as_u32())
            }))
        }),
    );
    props.insert(
        "__w3cos_getter_focusNode".to_string(),
        func(|_, _| {
            element_or_null(dom::with_document(|doc| {
                doc.get_selection().focus_node.map(|n| n.as_u32())
            }))
        }),
    );
    props.insert(
        "__w3cos_getter_anchorOffset".to_string(),
        func(|_, _| {
            Value::Number(dom::with_document(|doc| doc.get_selection().anchor_offset) as f64)
        }),
    );
    props.insert(
        "__w3cos_getter_focusOffset".to_string(),
        func(|_, _| {
            Value::Number(dom::with_document(|doc| doc.get_selection().focus_offset) as f64)
        }),
    );
    props.insert(
        "__w3cos_getter_type".to_string(),
        func(|_, _| {
            Value::string(&dom::with_document(|doc| {
                doc.get_selection().selection_type().to_string()
            }))
        }),
    );
    props.insert(
        "getRangeAt".to_string(),
        func(|_, args| {
            let idx = arg(&args, 0).to_u32() as usize;
            dom::with_document(|doc| {
                doc.get_selection()
                    .get_range_at(idx)
                    .map(|r| {
                        range_value(
                            r.start_container.as_u32(),
                            r.start_offset,
                            r.end_container.as_u32(),
                            r.end_offset,
                        )
                    })
                    .unwrap_or(Value::Null)
            })
        }),
    );
    props.insert(
        "addRange".to_string(),
        func(|_, args| {
            let rv = arg(&args, 0);
            let r = range_to_w3cos(&rv);
            dom::with_document_mut(|doc| doc.get_selection_mut().add_range(r));
            Value::Undefined
        }),
    );
    props.insert(
        "removeAllRanges".to_string(),
        func(|_, _| {
            dom::with_document_mut(|doc| doc.get_selection_mut().remove_all_ranges());
            Value::Undefined
        }),
    );
    props.insert(
        "collapse".to_string(),
        func(|_, args| {
            if let Some(n) = node_id_of(&arg(&args, 0)) {
                let off = arg(&args, 1).to_u32();
                dom::with_document_mut(|doc| {
                    doc.get_selection_mut().collapse(NodeId::from_u32(n), off)
                });
            }
            Value::Undefined
        }),
    );
    props.insert(
        "extend".to_string(),
        func(|_, args| {
            if let Some(n) = node_id_of(&arg(&args, 0)) {
                let off = arg(&args, 1).to_u32();
                dom::with_document_mut(|doc| {
                    doc.get_selection_mut().extend(NodeId::from_u32(n), off)
                });
            }
            Value::Undefined
        }),
    );
    props.insert(
        "selectAllChildren".to_string(),
        func(|_, args| {
            if let Some(n) = node_id_of(&arg(&args, 0)) {
                dom::with_document_mut(|doc| {
                    let nid = NodeId::from_u32(n);
                    let children = doc.children_ids(nid);
                    let (anchor, focus, focus_off) = if children.is_empty() {
                        let len = doc
                            .get_node(nid)
                            .text_content
                            .as_ref()
                            .map(|t| t.chars().count() as u32)
                            .unwrap_or(0);
                        (nid, nid, len)
                    } else {
                        let first = children[0];
                        let last = *children.last().unwrap();
                        (first, last, doc.children_ids(last).len() as u32)
                    };
                    let sel = doc.get_selection_mut();
                    sel.collapse(anchor, 0);
                    sel.extend(focus, focus_off);
                });
            }
            Value::Undefined
        }),
    );
    props.insert(
        "containsNode".to_string(),
        func(|_, args| {
            let Some(n) = node_id_of(&arg(&args, 0)) else {
                return Value::Bool(false);
            };
            Value::Bool(dom::with_document(|doc| {
                doc.get_selection().contains_node(n)
            }))
        }),
    );
    props.insert(
        "toString".to_string(),
        func(|_, _| {
            Value::string(&dom::with_document(|doc| {
                doc.get_selection().to_string(doc)
            }))
        }),
    );
    props.insert(
        "empty".to_string(),
        func(|_, _| {
            dom::with_document_mut(|doc| doc.get_selection_mut().remove_all_ranges());
            Value::Undefined
        }),
    );
    let value = Value::object(props);
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("Selection"));
    SELECTION_VALUE.with(|s| *s.borrow_mut() = Some(value.clone()));
    value
}

// ── document ───────────────────────────────────────────────────────────────

fn head_id() -> u32 {
    ensure_html_structure();
    HEAD_ID.with(|h| h.borrow().unwrap())
}

fn document_element_id() -> u32 {
    if document_value().get_property("contentType").to_js_string() != "text/html" {
        return dom::children(0)
            .into_iter()
            .find(|node| dom::node_type(*node) == 1)
            .unwrap_or_else(dom::body_id);
    }
    ensure_html_structure();
    HTML_ID.with(|h| h.borrow().unwrap())
}

fn global_document_children() -> Vec<Value> {
    if document_value().get_property("contentType").to_js_string() == "text/html" {
        ensure_html_structure();
    }
    let document = document_value();
    let doctype = document.get_property("doctype");
    let mut children = dom::children(0)
        .into_iter()
        .filter(|node| matches!(dom::node_type(*node), 1 | 7 | 8))
        .map(element_value)
        .collect::<Vec<_>>();
    if !doctype.is_null() && !doctype.is_undefined() {
        let position = children
            .iter()
            .position(|child| child.get_property("nodeType").to_u32() == 1)
            .unwrap_or(children.len());
        children.insert(position, doctype);
    }
    children
}

fn is_global_document_child(node: u32) -> bool {
    dom::parent_node(node) == Some(0)
        || get_expando(node, "parentNode")
            .is_some_and(|parent| parent.strict_eq(&document_value()))
}

fn global_document_sibling(node: u32, next: bool) -> Value {
    let children = global_document_children();
    let Some(index) = children
        .iter()
        .position(|child| node_id_of(child) == Some(node))
    else {
        return Value::Null;
    };
    let sibling = if next {
        children.get(index + 1)
    } else {
        index.checked_sub(1).and_then(|index| children.get(index))
    };
    sibling.cloned().unwrap_or(Value::Null)
}

fn global_document_child_nodes() -> Value {
    let document = document_value();
    let cached = document.get_property("__w3cos_cached_child_nodes");
    if !cached.is_undefined() {
        return cached;
    }
    let list = live_node_list(global_document_children);
    document.set_property("__w3cos_cached_child_nodes", list.clone());
    list
}

/// Lazily create the `<html>`/`<head>` structure: root(#document) gets an
/// `<html>` child containing `<head>` and the (moved) `<body>`. Body-based
/// rendering is unaffected (`to_component_tree` starts at the body node).
fn ensure_html_structure() {
    let done = HTML_ID.with(|h| h.borrow().is_some());
    if done {
        return;
    }
    let html = dom::create_element("html");
    let head = dom::create_element("head");
    let body = dom::body_id();
    dom::insert_before(0, html, body); // root: <html> before <body>
    dom::append_child(html, head);
    dom::append_child(html, body); // moves body under html
    HTML_ID.with(|h| *h.borrow_mut() = Some(html));
    HEAD_ID.with(|h| *h.borrow_mut() = Some(head));
}

fn traversal_root(value: &Value) -> Option<u32> {
    node_id_of(value).or_else(|| (value == &document_value()).then_some(0))
}

fn traversal_node_value(node: u32) -> Value {
    if node == 0 {
        document_value()
    } else {
        element_value(node)
    }
}

fn node_filter_result(node: u32, what_to_show: u32, filter: &Value) -> u32 {
    let node_type = dom::node_type(node);
    let mask = (node_type > 0 && node_type <= 32)
        .then(|| 1_u32 << (node_type - 1))
        .unwrap_or(0);
    if what_to_show != u32::MAX && what_to_show & mask == 0 {
        return 3;
    }
    let node = traversal_node_value(node);
    let result = if filter.is_nullish() {
        return 1;
    } else if filter.is_function() {
        filter.call(Value::Undefined, vec![node])
    } else {
        let callback = filter.get_property("acceptNode");
        if !callback.is_function() {
            return 1;
        }
        callback.call(filter.clone(), vec![node])
    };
    match result.to_u32() {
        1..=3 => result.to_u32(),
        _ => 1,
    }
}

fn traversal_sequence(
    root: u32,
    what_to_show: u32,
    filter: &Value,
    reject_prunes: bool,
) -> Vec<(u32, bool)> {
    fn visit(
        node: u32,
        what_to_show: u32,
        filter: &Value,
        reject_prunes: bool,
        output: &mut Vec<(u32, bool)>,
    ) {
        let result = node_filter_result(node, what_to_show, filter);
        output.push((node, result == 1));
        if result == 2 && reject_prunes {
            return;
        }
        for child in dom::children(node) {
            visit(child, what_to_show, filter, reject_prunes, output);
        }
    }
    let mut output = Vec::new();
    visit(root, what_to_show, filter, reject_prunes, &mut output);
    output
}

fn visible_child(node: u32, what_to_show: u32, filter: &Value, reverse: bool) -> Option<u32> {
    fn find(node: u32, what_to_show: u32, filter: &Value, reverse: bool) -> Option<u32> {
        let result = node_filter_result(node, what_to_show, filter);
        if result == 1 {
            return Some(node);
        }
        if result == 2 {
            return None;
        }
        let mut children = dom::children(node);
        if reverse {
            children.reverse();
        }
        children
            .into_iter()
            .find_map(|child| find(child, what_to_show, filter, reverse))
    }
    let mut children = dom::children(node);
    if reverse {
        children.reverse();
    }
    children
        .into_iter()
        .find_map(|child| find(child, what_to_show, filter, reverse))
}

fn repair_iterator_reference(
    reference: &Cell<u32>,
    before: &Cell<bool>,
    previous_sequence: &RefCell<Vec<u32>>,
    sequence: &[(u32, bool)],
) {
    if sequence.iter().any(|(node, _)| *node == reference.get()) {
        return;
    }
    let previous = previous_sequence.borrow();
    let Some(position) = previous.iter().position(|node| *node == reference.get()) else {
        return;
    };
    let is_live = |candidate: &&u32| sequence.iter().any(|(node, _)| node == *candidate);
    let replacement = if before.get() {
        previous[position + 1..]
            .iter()
            .find(is_live)
            .map(|node| (*node, true))
            .or_else(|| {
                previous[..position]
                    .iter()
                    .rev()
                    .find(is_live)
                    .map(|node| (*node, false))
            })
    } else {
        previous[..position]
            .iter()
            .rev()
            .find(is_live)
            .map(|node| (*node, false))
            .or_else(|| {
                previous[position + 1..]
                    .iter()
                    .find(is_live)
                    .map(|node| (*node, true))
            })
    };
    if let Some((replacement, pointer_before)) = replacement {
        reference.set(replacement);
        before.set(pointer_before);
    }
}

fn traversal_value(root: u32, what_to_show: u32, filter: Value, iterator: bool) -> Value {
    if iterator {
        let reference = Rc::new(Cell::new(root));
        let before = Rc::new(Cell::new(true));
        let previous_sequence = Rc::new(RefCell::new(vec![root]));
        let value = Value::object(HashMap::from([
            ("root".to_string(), traversal_node_value(root)),
            ("whatToShow".to_string(), Value::Number(what_to_show as f64)),
            ("filter".to_string(), filter.clone()),
        ]));
        let next_reference = Rc::clone(&reference);
        let next_before = Rc::clone(&before);
        let next_previous_sequence = Rc::clone(&previous_sequence);
        let next_filter = filter.clone();
        value.set_property(
            "nextNode",
            func(move |_, _| {
                let sequence = traversal_sequence(root, what_to_show, &next_filter, false);
                repair_iterator_reference(
                    &next_reference,
                    &next_before,
                    &next_previous_sequence,
                    &sequence,
                );
                *next_previous_sequence.borrow_mut() =
                    sequence.iter().map(|(node, _)| *node).collect();
                let current = next_reference.get();
                let start = sequence
                    .iter()
                    .position(|(node, _)| *node == current)
                    .unwrap_or(0);
                let candidate = sequence
                    .iter()
                    .enumerate()
                    .skip(start + usize::from(!next_before.get()))
                    .find(|(_, (_, accepted))| *accepted)
                    .map(|(_, (node, _))| *node);
                if let Some(candidate) = candidate {
                    next_reference.set(candidate);
                    next_before.set(false);
                    traversal_node_value(candidate)
                } else {
                    Value::Null
                }
            }),
        );
        let previous_reference = Rc::clone(&reference);
        let previous_before = Rc::clone(&before);
        let previous_previous_sequence = previous_sequence;
        let previous_filter = filter;
        value.set_property(
            "previousNode",
            func(move |_, _| {
                let sequence = traversal_sequence(root, what_to_show, &previous_filter, false);
                repair_iterator_reference(
                    &previous_reference,
                    &previous_before,
                    &previous_previous_sequence,
                    &sequence,
                );
                *previous_previous_sequence.borrow_mut() =
                    sequence.iter().map(|(node, _)| *node).collect();
                let current = previous_reference.get();
                let position = sequence
                    .iter()
                    .position(|(node, _)| *node == current)
                    .unwrap_or(0);
                let end = position + usize::from(!previous_before.get());
                let candidate = sequence[..end.min(sequence.len())]
                    .iter()
                    .rev()
                    .find(|(_, accepted)| *accepted)
                    .map(|(node, _)| *node);
                if let Some(candidate) = candidate {
                    previous_reference.set(candidate);
                    previous_before.set(true);
                    traversal_node_value(candidate)
                } else {
                    Value::Null
                }
            }),
        );
        value.set_property("detach", func(|_, _| Value::Undefined));
        w3cos_core::class::set_prototype_of(
            &value,
            &crate::dom_constructors::prototype("NodeIterator"),
        );
        return value;
    }

    let current = Rc::new(Cell::new(root));
    let value = Value::object(HashMap::from([
        ("root".to_string(), traversal_node_value(root)),
        ("whatToShow".to_string(), Value::Number(what_to_show as f64)),
        ("filter".to_string(), filter.clone()),
    ]));
    let getter_current = Rc::clone(&current);
    value.set_property(
        "__w3cos_getter_currentNode",
        func(move |_, _| traversal_node_value(getter_current.get())),
    );
    let setter_current = Rc::clone(&current);
    value.set_property(
        "__w3cos_setter_currentNode",
        func(move |_, args| {
            if let Some(node) = traversal_root(&arg(&args, 0)) {
                setter_current.set(node);
            }
            Value::Undefined
        }),
    );
    for (name, reverse) in [("firstChild", false), ("lastChild", true)] {
        let method_current = Rc::clone(&current);
        let method_filter = filter.clone();
        value.set_property(
            name,
            func(move |_, _| {
                if let Some(node) =
                    visible_child(method_current.get(), what_to_show, &method_filter, reverse)
                {
                    method_current.set(node);
                    traversal_node_value(node)
                } else {
                    Value::Null
                }
            }),
        );
    }
    let parent_current = Rc::clone(&current);
    let parent_filter = filter.clone();
    value.set_property(
        "parentNode",
        func(move |_, _| {
            let mut parent = dom::parent_node(parent_current.get());
            while let Some(node) = parent {
                if node_filter_result(node, what_to_show, &parent_filter) == 1 {
                    parent_current.set(node);
                    return traversal_node_value(node);
                }
                if node == root {
                    break;
                }
                parent = dom::parent_node(node);
            }
            Value::Null
        }),
    );
    for (name, reverse) in [("nextSibling", false), ("previousSibling", true)] {
        let sibling_current = Rc::clone(&current);
        let sibling_filter = filter.clone();
        value.set_property(
            name,
            func(move |_, _| {
                let current_node = sibling_current.get();
                let mut sibling = if reverse {
                    dom::previous_sibling(current_node)
                } else {
                    dom::next_sibling(current_node)
                };
                while let Some(node) = sibling {
                    let result = node_filter_result(node, what_to_show, &sibling_filter);
                    let candidate = if result == 1 {
                        Some(node)
                    } else if result == 3 {
                        visible_child(node, what_to_show, &sibling_filter, reverse)
                    } else {
                        None
                    };
                    if let Some(candidate) = candidate {
                        sibling_current.set(candidate);
                        return traversal_node_value(candidate);
                    }
                    sibling = if reverse {
                        dom::previous_sibling(node)
                    } else {
                        dom::next_sibling(node)
                    };
                }
                Value::Null
            }),
        );
    }
    for (name, reverse) in [("nextNode", false), ("previousNode", true)] {
        let node_current = Rc::clone(&current);
        let node_filter = filter.clone();
        value.set_property(
            name,
            func(move |_, _| {
                let sequence = traversal_sequence(root, what_to_show, &node_filter, true);
                let position = sequence
                    .iter()
                    .position(|(node, _)| *node == node_current.get());
                let candidate = position.and_then(|position| {
                    if reverse {
                        sequence[..position]
                            .iter()
                            .rev()
                            .find(|(_, accepted)| *accepted)
                    } else {
                        sequence[position + 1..]
                            .iter()
                            .find(|(_, accepted)| *accepted)
                    }
                    .map(|(node, _)| *node)
                });
                if let Some(candidate) = candidate {
                    node_current.set(candidate);
                    traversal_node_value(candidate)
                } else {
                    Value::Null
                }
            }),
        );
    }
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("TreeWalker"));
    value
}

fn node_filter_value() -> Value {
    Value::object(HashMap::from([
        ("FILTER_ACCEPT".to_string(), Value::Number(1.0)),
        ("FILTER_REJECT".to_string(), Value::Number(2.0)),
        ("FILTER_SKIP".to_string(), Value::Number(3.0)),
        ("SHOW_ALL".to_string(), Value::Number(u32::MAX as f64)),
        ("SHOW_ELEMENT".to_string(), Value::Number(1.0)),
        ("SHOW_ATTRIBUTE".to_string(), Value::Number(2.0)),
        ("SHOW_TEXT".to_string(), Value::Number(4.0)),
        ("SHOW_CDATA_SECTION".to_string(), Value::Number(8.0)),
        ("SHOW_ENTITY_REFERENCE".to_string(), Value::Number(16.0)),
        ("SHOW_ENTITY".to_string(), Value::Number(32.0)),
        (
            "SHOW_PROCESSING_INSTRUCTION".to_string(),
            Value::Number(64.0),
        ),
        ("SHOW_COMMENT".to_string(), Value::Number(128.0)),
        ("SHOW_DOCUMENT".to_string(), Value::Number(256.0)),
        ("SHOW_DOCUMENT_TYPE".to_string(), Value::Number(512.0)),
        ("SHOW_DOCUMENT_FRAGMENT".to_string(), Value::Number(1024.0)),
        ("SHOW_NOTATION".to_string(), Value::Number(2048.0)),
    ]))
}

/// The global `document` value (memoized thread-local singleton).
pub fn document_value() -> Value {
    if let Some(v) = DOCUMENT_VALUE.with(|d| d.borrow().clone()) {
        return v;
    }
    let value = build_document_value();
    crate::view_transition_web::install_document(&value);
    crate::fragment_directive_web::install_document(&value);
    crate::xpath_web::install_document(&value);
    DOCUMENT_VALUE.with(|d| *d.borrow_mut() = Some(value.clone()));
    value
}

fn build_document_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    let implementation = dom_implementation_value();

    props.insert("nodeType".to_string(), Value::Number(9.0));
    props.insert("nodeName".to_string(), Value::string("#document"));
    props.insert("parentNode".to_string(), Value::Null);
    props.insert("parentElement".to_string(), Value::Null);
    props.insert(
        "isSameNode".to_string(),
        func(|document, args| Value::Bool(document.strict_eq(&arg(&args, 0)))),
    );
    props.insert(
        "isEqualNode".to_string(),
        func(|document, args| Value::Bool(nodes_are_equal(&document, &arg(&args, 0)))),
    );
    props.insert("getRootNode".to_string(), func(|document, _| document));
    props.insert(
        "normalize".to_string(),
        func(|_, _| {
            normalize_node_subtree(0);
            Value::Undefined
        }),
    );
    props.insert("onvisibilitychange".to_string(), Value::Null);
    props.insert("characterSet".to_string(), Value::string("UTF-8"));
    props.insert("compatMode".to_string(), Value::string("CSS1Compat"));
    props.insert("designMode".to_string(), Value::string("off"));
    props.insert("contentType".to_string(), Value::string("text/html"));
    props.insert("documentURI".to_string(), Value::string("w3cos://app"));
    props.insert("URL".to_string(), Value::string("w3cos://app"));
    props.insert("domain".to_string(), Value::string("app"));
    props.insert("referrer".to_string(), Value::string(""));
    props.insert(
        "fonts".to_string(),
        crate::font_loading_web::font_face_set_value(),
    );
    props.insert(
        "styleSheets".to_string(),
        AUTHOR_STYLE_SHEETS.with(|sheets| style_sheet_list_value(Rc::clone(sheets))),
    );
    props.insert("adoptedStyleSheets".to_string(), js_array(vec![]));
    props.insert("doctype".to_string(), Value::Null);
    props.insert("implementation".to_string(), implementation.clone());
    props.insert(
        "featurePolicy".to_string(),
        crate::compat_web::feature_policy_value(),
    );
    props.insert("pictureInPictureElement".to_string(), Value::Null);
    props.insert(
        "exitPictureInPicture".to_string(),
        func(|_, _| w3cos_core::promise::resolve(vec![Value::Undefined])),
    );
    props.insert(
        "timeline".to_string(),
        crate::animations_web::document_timeline_value(),
    );

    props.insert(
        "createElement".to_string(),
        func(|_, args| {
            let mut tag = arg(&args, 0).to_js_string();
            if !valid_element_name(&tag) {
                dom_exception("The element name is not valid", "InvalidCharacterError");
            }
            if document_value().get_property("contentType").to_js_string() == "text/html" {
                tag.make_ascii_lowercase();
            }
            let id = dom::create_element(&tag);
            let element = element_value(id);
            if tag == "template" {
                ensure_template_content(id);
            } else if tag == "script" {
                // HTML-created script elements start with the spec's
                // force-async flag. Setting `script.async = false` below
                // explicitly clears it and joins the ordered dynamic queue.
                set_expando(id, "__w3cos_force_async", Value::Bool(true));
            }
            crate::custom_elements_web::upgrade_created_element(&tag, element)
        }),
    );
    props.insert(
        "createElementNS".to_string(),
        func(|_, args| {
            let namespace = normalized_namespace(&arg(&args, 0));
            // Namespace-aware creation preserves the supplied qualified name.
            // HTML case folding is a reflected `tagName`/`nodeName` rule and
            // only applies when the owner document itself is an HTML document.
            let tag = arg(&args, 1).to_js_string();
            validate_and_extract_qualified_name(namespace.as_deref(), &tag);
            let id = create_namespaced_element(namespace.as_deref().unwrap_or(""), &tag);
            let element = element_value(id);
            if namespace.as_deref() == Some("http://www.w3.org/1998/Math/MathML") {
                w3cos_core::class::set_prototype_of(
                    &element,
                    &math_ml_element_class().get_property("prototype"),
                );
            }
            element
        }),
    );
    props.insert(
        "createTextNode".to_string(),
        func(|_, args| element_value(dom::create_text_node(&arg(&args, 0).to_js_string()))),
    );
    props.insert(
        "createComment".to_string(),
        func(|_, args| element_value(dom::create_comment(&arg(&args, 0).to_js_string()))),
    );
    props.insert(
        "createCDATASection".to_string(),
        func(|document, args| create_cdata_section_value(&document, &arg(&args, 0).to_js_string())),
    );
    props.insert(
        "createProcessingInstruction".to_string(),
        func(|_, args| {
            create_processing_instruction_value(
                &arg(&args, 0).to_js_string(),
                &arg(&args, 1).to_js_string(),
            )
        }),
    );
    props.insert(
        "createDocumentFragment".to_string(),
        func(|_, _| element_value(dom::create_document_fragment())),
    );
    props.insert(
        "createEvent".to_string(),
        func(|_, args| {
            let requested = arg(&args, 0).to_js_string();
            let interface = match requested.to_ascii_lowercase().as_str() {
                "beforeunloadevent" => "BeforeUnloadEvent",
                "compositionevent" => "CompositionEvent",
                "customevent" => "CustomEvent",
                "devicemotionevent" => "DeviceMotionEvent",
                "deviceorientationevent" => "DeviceOrientationEvent",
                "dragevent" => "DragEvent",
                "event" | "events" | "htmlevents" | "svgevents" => "Event",
                "focusevent" => "FocusEvent",
                "hashchangeevent" => "HashChangeEvent",
                "keyboardevent" => "KeyboardEvent",
                "messageevent" => "MessageEvent",
                "mouseevent" | "mouseevents" => "MouseEvent",
                "storageevent" => "StorageEvent",
                "textevent" => "TextEvent",
                "touchevent" => "TouchEvent",
                "uievent" | "uievents" => "UIEvent",
                _ => {
                    return dom_exception(
                        &format!("The event interface {requested:?} is not supported"),
                        "NotSupportedError",
                    );
                }
            };
            w3cos_core::class::construct(
                &window_value().get_property(interface),
                vec![Value::string("")],
            )
        }),
    );
    props.insert(
        "createAttribute".to_string(),
        func(|document, args| detached_attribute_value(&document, arg(&args, 0), true)),
    );
    props.insert(
        "createAttributeNS".to_string(),
        func(|document, args| {
            detached_namespaced_attribute_value(&document, arg(&args, 0), arg(&args, 1))
        }),
    );
    props.insert(
        "appendChild".to_string(),
        func(|document, args| {
            let child = arg(&args, 0);
            let Some(node) = node_id_of(&child) else {
                return child;
            };
            match dom::node_type(node) {
                7 | 8 => {
                    dom::append_child(0, node);
                    set_expando(node, "ownerDocument", document);
                    child
                }
                _ => dom_exception(
                    "This node is not valid at the requested document position",
                    "HierarchyRequestError",
                ),
            }
        }),
    );
    for (name, iterator) in [("createTreeWalker", false), ("createNodeIterator", true)] {
        props.insert(
            name.to_string(),
            func(move |_, args| {
                if args.is_empty() {
                    type_error(&format!("Document.{name} requires a root node"));
                }
                let root_value = arg(&args, 0);
                let Some(root) = traversal_root(&root_value) else {
                    return Value::Null;
                };
                let what_to_show = if arg(&args, 1).is_undefined() {
                    u32::MAX
                } else {
                    arg(&args, 1).to_u32()
                };
                let filter = if arg(&args, 2).is_undefined() {
                    Value::Null
                } else {
                    arg(&args, 2)
                };
                traversal_value(root, what_to_show, filter, iterator)
            }),
        );
    }
    props.insert(
        "getElementById".to_string(),
        func(|_, args| {
            let root = document_element_id();
            let id = arg(&args, 0).to_js_string();
            if id.is_empty() {
                return Value::Null;
            }
            let found = inclusive_descendant_elements(root)
                .into_iter()
                .find(|node| dom::get_attribute(*node, "id").as_deref() == Some(&id));
            element_or_null(found)
        }),
    );
    props.insert(
        "querySelector".to_string(),
        func(|_, args| {
            let root = document_element_id();
            let sel = query_selector_argument(&args, root);
            element_or_null(query_live_document_all(&sel).into_iter().next())
        }),
    );
    props.insert(
        "querySelectorAll".to_string(),
        func(|_, args| {
            let root = document_element_id();
            let sel = query_selector_argument(&args, root);
            node_list(
                query_live_document_all(&sel)
                    .into_iter()
                    .map(element_value)
                    .collect(),
            )
        }),
    );
    props.insert(
        "caretPositionFromPoint".to_string(),
        func(|_, args| caret_position_from_point(farg(&args, 0), farg(&args, 1))),
    );
    props.insert(
        "caretRangeFromPoint".to_string(),
        func(|_, args| {
            let position = caret_position_from_point(farg(&args, 0), farg(&args, 1));
            if position.is_null() {
                return Value::Null;
            }
            let Some(node) = node_id_of(&position.get_property("offsetNode")) else {
                return Value::Null;
            };
            let offset = position.get_property("offset").to_u32();
            range_value(node, offset, node, offset)
        }),
    );
    props.insert(
        "elementFromPoint".to_string(),
        func(|_, args| {
            element_or_null(deepest_node_at_point(
                document_element_id(),
                farg(&args, 0),
                farg(&args, 1),
            ))
        }),
    );
    props.insert(
        "elementsFromPoint".to_string(),
        func(|_, args| {
            Value::array(
                deepest_node_at_point(document_element_id(), farg(&args, 0), farg(&args, 1))
                    .into_iter()
                    .map(element_value)
                    .collect(),
            )
        }),
    );
    props.insert(
        "getElementsByTagName".to_string(),
        func(|_, args| {
            let tag = arg(&args, 0).to_js_string();
            let html_document =
                document_value().get_property("contentType").to_js_string() == "text/html";
            html_collection(move || {
                inclusive_descendant_elements(document_element_id())
                    .into_iter()
                    .filter(|node| element_matches_tag_name(*node, &tag, html_document))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "getElementsByTagNameNS".to_string(),
        func(|_, args| {
            let namespace = normalized_namespace(&arg(&args, 0));
            let local_name = arg(&args, 1).to_js_string();
            html_collection(move || {
                inclusive_descendant_elements(document_element_id())
                    .into_iter()
                    .filter(|node| element_matches_namespace(*node, &namespace, &local_name))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "getElementsByClassName".to_string(),
        func(|_, args| {
            let class_names = arg(&args, 0).to_js_string();
            html_collection(move || {
                inclusive_descendant_elements(document_element_id())
                    .into_iter()
                    .filter(|node| element_matches_class_names(*node, &class_names))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "getElementsByName".to_string(),
        func(|_, args| {
            let name = arg(&args, 0).to_js_string();
            live_node_list(move || {
                inclusive_descendant_elements(document_element_id())
                    .into_iter()
                    .filter(|node| dom::get_attribute(*node, "name").as_deref() == Some(&name))
                    .map(element_value)
                    .collect()
            })
        }),
    );
    props.insert(
        "createRange".to_string(),
        func(|_, _| range_value(0, 0, 0, 0)),
    );
    props.insert("getSelection".to_string(), func(|_, _| selection_value()));
    props.insert(
        "getAnimations".to_string(),
        func(|_, _| crate::animations_web::animations_for(None, true)),
    );
    props.insert(
        "execCommand".to_string(),
        func(|_, _| {
            warn_host_api("document.execCommand()", "false");
            Value::Bool(false)
        }),
    );
    props.insert("hasFocus".to_string(), func(|_, _| Value::Bool(true)));
    props.insert(
        "adoptNode".to_string(),
        func(|document, args| adopt_node_into(&document, arg(&args, 0))),
    );
    props.insert(
        "importNode".to_string(),
        func(|document, args| import_node_into(&document, arg(&args, 0), arg(&args, 1).to_bool())),
    );
    props.insert(
        "addEventListener".to_string(),
        func(|_, args| {
            js_add_event_listener(
                0,
                &arg(&args, 0).to_js_string(),
                arg(&args, 1),
                arg(&args, 2),
            );
            Value::Undefined
        }),
    );
    props.insert(
        "removeEventListener".to_string(),
        func(|_, args| {
            js_remove_event_listener(0, &arg(&args, 0).to_js_string());
            Value::Undefined
        }),
    );
    props.insert(
        "dispatchEvent".to_string(),
        func(|_, args| Value::Bool(js_dispatch_event(0, arg(&args, 0)))),
    );
    // Live getters via the value.rs getter convention.
    props.insert(
        "__w3cos_getter_body".to_string(),
        func(|_, _| {
            if document_value().get_property("contentType").to_js_string() == "text/html" {
                return element_value(dom::body_id());
            }
            element_or_null(
                inclusive_descendant_elements(document_element_id())
                    .into_iter()
                    .find(|node| {
                        namespace_uri(*node) == crate::html_parser_state::HTML_NAMESPACE
                            && dom::tag_name(*node) == "body"
                    }),
            )
        }),
    );
    props.insert(
        "__w3cos_getter_head".to_string(),
        func(|_, _| {
            if document_value().get_property("contentType").to_js_string() == "text/html" {
                return element_value(head_id());
            }
            element_or_null(
                inclusive_descendant_elements(document_element_id())
                    .into_iter()
                    .find(|node| {
                        namespace_uri(*node) == crate::html_parser_state::HTML_NAMESPACE
                            && dom::tag_name(*node) == "head"
                    }),
            )
        }),
    );
    props.insert(
        "__w3cos_getter_documentElement".to_string(),
        func(|_, _| element_value(document_element_id())),
    );
    props.insert(
        "__w3cos_getter_childNodes".to_string(),
        func(|_, _| global_document_child_nodes()),
    );
    props.insert(
        "__w3cos_getter_firstChild".to_string(),
        func(|_, _| {
            global_document_children()
                .into_iter()
                .next()
                .unwrap_or(Value::Null)
        }),
    );
    props.insert(
        "__w3cos_getter_lastChild".to_string(),
        func(|_, _| {
            global_document_children()
                .into_iter()
                .last()
                .unwrap_or(Value::Null)
        }),
    );
    props.insert(
        "__w3cos_getter_scrollingElement".to_string(),
        func(|_, _| element_value(dom::body_id())),
    );
    props.insert(
        "__w3cos_getter_activeElement".to_string(),
        func(|_, _| {
            element_or_null(
                ACTIVE_ELEMENT
                    .with(|a| *a.borrow())
                    .or(Some(dom::body_id())),
            )
        }),
    );
    props.insert(
        "__w3cos_getter_defaultView".to_string(),
        func(|_, _| window_value()),
    );
    props.insert(
        "__w3cos_getter_fullscreenElement".to_string(),
        func(|_, _| {
            FULLSCREEN_NODE
                .with(|current| *current.borrow())
                .map(element_value)
                .unwrap_or(Value::Null)
        }),
    );
    props.insert("fullscreenEnabled".to_string(), Value::Bool(true));
    props.insert(
        "exitFullscreen".to_string(),
        func(|_, _| {
            FULLSCREEN_NODE.with(|current| *current.borrow_mut() = None);
            let event = w3cos_core::class::construct(
                &crate::web_events::event_class(),
                vec![Value::string("fullscreenchange")],
            );
            document_value().call_method("dispatchEvent", vec![event]);
            resolved_thenable(Value::Undefined)
        }),
    );
    props.insert(
        "__w3cos_getter_visibilityState".to_string(),
        func(|_, _| DOCUMENT_VISIBILITY.with(|state| Value::string(&state.borrow()))),
    );
    props.insert(
        "__w3cos_getter_hidden".to_string(),
        func(|_, _| {
            DOCUMENT_VISIBILITY.with(|state| Value::Bool(state.borrow().as_str() == "hidden"))
        }),
    );
    props.insert(
        "__w3cos_getter_readyState".to_string(),
        func(|_, _| DOCUMENT_READY_STATE.with(|state| Value::string(&state.borrow()))),
    );
    props.insert(
        "__w3cos_getter_cookie".to_string(),
        func(|_, _| Value::string(&crate::cookie_store_web::document_cookie())),
    );
    props.insert(
        "__w3cos_setter_cookie".to_string(),
        func(|_, args| {
            crate::cookie_store_web::set_document_cookie(&arg(&args, 0).to_js_string());
            Value::Undefined
        }),
    );
    props.insert(
        "__w3cos_getter_title".to_string(),
        func(|_, _| Value::string("")),
    );
    props.insert(
        "__w3cos_getter_location".to_string(),
        func(|_, _| location_value()),
    );

    let document = document_node_value(props);
    implementation.set_property("__w3cos_owner_document", document.clone());
    w3cos_core::class::set_prototype_of(
        &document,
        &crate::dom_constructors::prototype("HTMLDocument"),
    );
    let prototype = crate::dom_constructors::prototype("Document");
    for name in ["hidden", "visibilityState", "onvisibilitychange"] {
        prototype.set_property(name, Value::Undefined);
    }
    document
}

// ── window ─────────────────────────────────────────────────────────────────

fn resolved_thenable(result: Value) -> Value {
    w3cos_core::promise::resolve(vec![result])
}

fn idle_deadline_class() -> Value {
    IDLE_DEADLINE_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = func(|_, args| idle_deadline_value(arg(&args, 0).to_bool()));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("didTimeout", Value::Undefined);
        prototype.set_property("timeRemaining", func(|_, _| Value::Number(0.0)));
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn idle_deadline_value(did_timeout: bool) -> Value {
    let started = Instant::now();
    let value = Value::object(HashMap::from([
        ("didTimeout".to_string(), Value::Bool(did_timeout)),
        (
            "timeRemaining".to_string(),
            func(move |_, _| {
                Value::Number((50.0 - started.elapsed().as_secs_f64() * 1000.0).clamp(0.0, 50.0))
            }),
        ),
    ]));
    w3cos_core::class::set_prototype_of(&value, &idle_deadline_class().get_property("prototype"));
    value
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
pub(crate) fn clipboard_read_text() -> String {
    crate::clipboard::Clipboard::read_text().unwrap_or_default()
}

#[cfg(not(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
pub(crate) fn clipboard_read_text() -> String {
    CLIPBOARD_FALLBACK.with(|c| c.borrow().clone())
}

#[cfg(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
))]
pub(crate) fn clipboard_write_text(text: &str) {
    let _ = crate::clipboard::Clipboard::write_text(text);
}

#[cfg(not(all(
    any(target_os = "macos", target_os = "linux", target_os = "windows"),
    not(target_env = "ohos")
)))]
pub(crate) fn clipboard_write_text(text: &str) {
    CLIPBOARD_FALLBACK.with(|c| *c.borrow_mut() = text.to_string());
}

#[cfg(target_os = "ios")]
fn navigator_languages() -> Vec<String> {
    let Some(locale_class) = AnyClass::get("NSLocale") else {
        return vec!["en-US".into()];
    };
    let languages: *mut AnyObject = unsafe { objc2::msg_send![locale_class, preferredLanguages] };
    if languages.is_null() {
        return vec!["en-US".into()];
    }
    let count: usize = unsafe { objc2::msg_send![&*languages, count] };
    let mut result = Vec::with_capacity(count);
    for index in 0..count {
        let language: *mut AnyObject =
            unsafe { objc2::msg_send![&*languages, objectAtIndex: index] };
        if language.is_null() {
            continue;
        }
        let bytes: *const std::ffi::c_char = unsafe { objc2::msg_send![&*language, UTF8String] };
        if !bytes.is_null() {
            result.push(
                unsafe { CStr::from_ptr(bytes) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if result.is_empty() {
        vec!["en-US".into()]
    } else {
        result
    }
}

#[cfg(not(target_os = "ios"))]
fn navigator_languages() -> Vec<String> {
    vec!["en-US".into()]
}

fn navigator_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "userAgent".to_string(),
        Value::string("W3COS/0.1 (w3cos; like Gecko)"),
    );
    props.insert("appCodeName".to_string(), Value::string("Mozilla"));
    props.insert("appName".to_string(), Value::string("Netscape"));
    props.insert("appVersion".to_string(), Value::string("0.1"));
    props.insert("platform".to_string(), Value::string("w3cos"));
    props.insert("product".to_string(), Value::string("Gecko"));
    props.insert("productSub".to_string(), Value::string("20030107"));
    props.insert("vendor".to_string(), Value::string("w3cos"));
    props.insert("vendorSub".to_string(), Value::string(""));
    props.insert("webdriver".to_string(), Value::Bool(false));
    props.insert(
        "userAgentData".to_string(),
        crate::compat_web::navigator_ua_data_value(),
    );
    props.insert("doNotTrack".to_string(), Value::Null);
    let languages = navigator_languages();
    let language = languages.first().map(String::as_str).unwrap_or("en-US");
    props.insert("language".to_string(), Value::string(language));
    props.insert(
        "languages".to_string(),
        js_array(languages.iter().map(|value| Value::string(value)).collect()),
    );
    props.insert(
        "__w3cos_getter_maxTouchPoints".to_string(),
        func(|_, _| Value::Number(MAX_TOUCH_POINTS.with(Cell::get) as f64)),
    );
    props.insert("hardwareConcurrency".to_string(), Value::Number(4.0));
    props.insert("onLine".to_string(), Value::Bool(true));
    props.insert("cookieEnabled".to_string(), Value::Bool(true));
    props.insert("pdfViewerEnabled".to_string(), Value::Bool(false));
    props.insert(
        "plugins".to_string(),
        crate::navigator_web::plugin_array_value(),
    );
    props.insert(
        "mimeTypes".to_string(),
        crate::navigator_web::mime_type_array_value(),
    );
    props.insert("javaEnabled".to_string(), func(|_, _| Value::Bool(false)));
    props.insert(
        "registerProtocolHandler".to_string(),
        crate::navigator_web::register_protocol_handler_value(),
    );
    props.insert(
        "requestMIDIAccess".to_string(),
        crate::midi_web::request_midi_access_value(),
    );
    props.insert(
        "requestMediaKeySystemAccess".to_string(),
        crate::encrypted_media_web::request_media_key_system_access_value(),
    );
    props.insert(
        "serviceWorker".to_string(),
        crate::service_worker_web::service_worker_container_value(),
    );
    props.insert(
        "getBattery".to_string(),
        crate::battery_web::get_battery_value(),
    );
    props.insert(
        "getGamepads".to_string(),
        crate::gamepad_web::get_gamepads_value(),
    );
    props.insert("locks".to_string(), crate::locks_web::lock_manager_value());
    props.insert(
        "permissions".to_string(),
        crate::permissions_web::permissions_value(),
    );
    props.insert(
        "storage".to_string(),
        crate::storage_manager_web::storage_manager_value(),
    );
    props.insert(
        "storageBuckets".to_string(),
        crate::storage_buckets_web::storage_bucket_manager_value(),
    );
    props.insert(
        "userActivation".to_string(),
        crate::user_activation_web::user_activation_value(),
    );
    #[cfg(feature = "web-media-advanced")]
    {
        props.insert(
            "mediaSession".to_string(),
            crate::media_session_web::media_session_value(),
        );
        props.insert(
            "mediaCapabilities".to_string(),
            crate::media_capabilities_web::media_capabilities_value(),
        );
    }
    props.insert(
        "credentials".to_string(),
        crate::credentials_web::credentials_container_value(),
    );
    props.insert(
        "login".to_string(),
        crate::navigator_web::navigator_login_value(),
    );
    props.insert(
        "managed".to_string(),
        crate::navigator_web::navigator_managed_data_value(),
    );
    let connection = crate::network_information_web::network_information_value();
    props.insert("connection".to_string(), connection.clone());
    props.insert("mozConnection".to_string(), connection.clone());
    props.insert("webkitConnection".to_string(), connection);
    props.insert(
        "wakeLock".to_string(),
        crate::wake_lock_web::wake_lock_value(),
    );
    props.insert("canShare".to_string(), crate::web_share::can_share_value());
    props.insert("share".to_string(), crate::web_share::share_value());
    props.insert(
        "setAppBadge".to_string(),
        crate::badging_web::set_app_badge_value(),
    );
    props.insert(
        "clearAppBadge".to_string(),
        crate::badging_web::clear_app_badge_value(),
    );
    props.insert(
        "sendBeacon".to_string(),
        func(|_, _| {
            warn_host_api("navigator.sendBeacon()", "false");
            Value::Bool(false)
        }),
    );
    props.insert(
        "vibrate".to_string(),
        func(|_, _| {
            warn_host_api("navigator.vibrate()", "false");
            Value::Bool(false)
        }),
    );

    props.insert(
        "clipboard".to_string(),
        crate::clipboard_web::clipboard_value(),
    );
    props.insert(
        "geolocation".to_string(),
        crate::geolocation_web::geolocation_value(),
    );
    #[cfg(feature = "web-media-advanced")]
    props.insert(
        "mediaDevices".to_string(),
        crate::media_devices_web::media_devices_value(),
    );
    props.insert(
        "bluetooth".to_string(),
        crate::bluetooth_web::bluetooth_value(),
    );
    props.insert(
        "serial".to_string(),
        crate::device_access_web::serial_value(),
    );
    props.insert("hid".to_string(), crate::device_access_web::hid_value());
    props.insert("usb".to_string(), crate::device_access_web::usb_value());
    props.insert(
        "keyboard".to_string(),
        crate::window_environment_web::keyboard_value(),
    );
    props.insert(
        "virtualKeyboard".to_string(),
        crate::window_environment_web::virtual_keyboard_value(),
    );
    props.insert(
        "devicePosture".to_string(),
        crate::window_environment_web::device_posture_value(),
    );
    props.insert(
        "windowControlsOverlay".to_string(),
        crate::window_environment_web::window_controls_overlay_value(),
    );
    props.insert(
        "scheduling".to_string(),
        crate::window_environment_web::scheduling_value(),
    );
    props.insert(
        "presentation".to_string(),
        crate::presentation_web::presentation_value(),
    );
    props.insert("ink".to_string(), crate::experimental_web::ink_value());
    props.insert(
        "protectedAudience".to_string(),
        crate::experimental_web::protected_audience_value(),
    );
    #[cfg(feature = "web-graphics-advanced")]
    {
        props.insert("gpu".to_string(), crate::webgpu_web::gpu_value());
        props.insert("xr".to_string(), crate::webxr_web::xr_system_value());
    }

    let navigator = Value::object(props);
    w3cos_core::class::set_prototype_of(
        &navigator,
        &crate::navigator_web::navigator_class().get_property("prototype"),
    );
    navigator
}

fn location_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    for (name, getter) in [
        ("href", crate::history::get_href as fn() -> String),
        ("origin", crate::history::get_origin),
        ("protocol", crate::history::get_protocol),
        ("host", crate::history::get_host),
        ("hostname", crate::history::get_hostname),
        ("port", crate::history::get_port),
        ("pathname", crate::history::get_pathname),
        ("search", crate::history::get_search),
        ("hash", crate::history::get_hash),
    ] {
        props.insert(
            format!("__w3cos_getter_{name}"),
            func(move |_, _| Value::string(&getter())),
        );
    }
    props.insert("ancestorOrigins".to_string(), dom_string_list(Vec::new()));
    props.insert(
        "assign".to_string(),
        func(|_, args| {
            crate::history::location_assign(&arg(&args, 0).to_js_string());
            Value::Undefined
        }),
    );
    props.insert(
        "replace".to_string(),
        func(|_, args| {
            crate::history::location_replace(&arg(&args, 0).to_js_string());
            Value::Undefined
        }),
    );
    props.insert(
        "reload".to_string(),
        func(|_, _| {
            LOCATION_RELOAD_WARNED.with(|warned| {
                if !warned.replace(true) {
                    eprintln!(
                        "W3COS warning: location.reload() preserves the current native document; \
                         host-level document reconstruction is not available"
                    );
                }
            });
            Value::Undefined
        }),
    );
    for component in [
        "href", "protocol", "host", "hostname", "port", "pathname", "search", "hash",
    ] {
        props.insert(
            format!("__w3cos_setter_{component}"),
            func(move |_, args| {
                let value = arg(&args, 0).to_js_string();
                if component == "href" {
                    crate::history::location_assign(&value);
                } else {
                    crate::history::set_location_component(component, &value);
                }
                Value::Undefined
            }),
        );
    }
    props.insert(
        "toString".to_string(),
        func(|_, _| Value::string(&crate::history::get_href())),
    );
    let value = Value::object(props);
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("Location"));
    value
}

fn dom_string_list(items: Vec<String>) -> Value {
    let items = Rc::new(items);
    let mut props = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        props.insert(index.to_string(), Value::string(item));
    }
    props.insert("length".to_string(), Value::Number(items.len() as f64));
    let item_values = Rc::clone(&items);
    props.insert(
        "item".to_string(),
        func(move |_, args| {
            item_values
                .get(arg(&args, 0).to_u32() as usize)
                .map(|item| Value::string(item))
                .unwrap_or(Value::Null)
        }),
    );
    props.insert(
        "contains".to_string(),
        func(move |_, args| {
            let candidate = arg(&args, 0).to_js_string();
            Value::Bool(items.iter().any(|item| item == &candidate))
        }),
    );
    let value = Value::object(props);
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::dom_constructors::prototype("DOMStringList"),
    );
    value
}

fn performance_navigation_value() -> Value {
    let value = Value::object(HashMap::from([
        ("type".into(), Value::Number(0.0)),
        ("redirectCount".into(), Value::Number(0.0)),
        ("TYPE_NAVIGATE".into(), Value::Number(0.0)),
        ("TYPE_RELOAD".into(), Value::Number(1.0)),
        ("TYPE_BACK_FORWARD".into(), Value::Number(2.0)),
        ("TYPE_RESERVED".into(), Value::Number(255.0)),
    ]));
    value.set_property(
        "toJSON",
        func(|this, _| {
            Value::object(HashMap::from([
                ("type".into(), this.get_property("type")),
                ("redirectCount".into(), this.get_property("redirectCount")),
            ]))
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::dom_constructors::prototype("PerformanceNavigation"),
    );
    value
}

fn legacy_performance_timing_fields() -> HashMap<String, Value> {
    let origin = performance_time_origin();
    let mut fields = HashMap::new();
    for name in [
        "navigationStart",
        "fetchStart",
        "domainLookupStart",
        "domainLookupEnd",
        "connectStart",
        "connectEnd",
        "requestStart",
        "responseStart",
        "responseEnd",
        "domLoading",
        "domInteractive",
        "domContentLoadedEventStart",
        "domContentLoadedEventEnd",
        "domComplete",
        "loadEventStart",
        "loadEventEnd",
    ] {
        fields.insert(name.into(), Value::Number(origin));
    }
    for name in [
        "unloadEventStart",
        "unloadEventEnd",
        "redirectStart",
        "redirectEnd",
        "secureConnectionStart",
    ] {
        fields.insert(name.into(), Value::Number(0.0));
    }
    fields
}

fn performance_timing_value() -> Value {
    let value = Value::object(legacy_performance_timing_fields());
    value.set_property(
        "toJSON",
        func(|_, _| Value::object(legacy_performance_timing_fields())),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::dom_constructors::prototype("PerformanceTiming"),
    );
    value
}

fn performance_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "now".to_string(),
        func(|_, _| Value::Number(performance_now())),
    );
    props.insert(
        "timeOrigin".to_string(),
        Value::Number(performance_time_origin()),
    );
    props.insert(
        "mark".to_string(),
        func(|_, args| crate::observers_web::performance_mark(&args, performance_now())),
    );
    props.insert(
        "measure".to_string(),
        func(|_, args| crate::observers_web::performance_measure(&args, performance_now())),
    );
    props.insert(
        "clearMarks".to_string(),
        func(|_, args| {
            let name = args.first().map(Value::to_js_string);
            crate::observers_web::performance_clear("mark", name.as_deref());
            Value::Undefined
        }),
    );
    props.insert(
        "clearMeasures".to_string(),
        func(|_, args| {
            let name = args.first().map(Value::to_js_string);
            crate::observers_web::performance_clear("measure", name.as_deref());
            Value::Undefined
        }),
    );
    props.insert(
        "getEntries".to_string(),
        func(|_, _| crate::observers_web::performance_get_entries(None, None)),
    );
    props.insert(
        "getEntriesByName".to_string(),
        func(|_, args| {
            let name = args.first().map(Value::to_js_string).unwrap_or_default();
            let kind = args.get(1).map(Value::to_js_string);
            crate::observers_web::performance_get_entries(Some(&name), kind.as_deref())
        }),
    );
    props.insert(
        "getEntriesByType".to_string(),
        func(|_, args| {
            let kind = args.first().map(Value::to_js_string).unwrap_or_default();
            crate::observers_web::performance_get_entries(None, Some(&kind))
        }),
    );
    props.insert(
        "clearResourceTimings".to_string(),
        func(|_, _| {
            static WARNING: std::sync::Once = std::sync::Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: performance.clearResourceTimings() is a compatible no-op \
                     until network resource timing entries are recorded"
                );
            });
            Value::Undefined
        }),
    );
    props.insert(
        "setResourceTimingBufferSize".to_string(),
        func(|_, _| {
            static WARNING: std::sync::Once = std::sync::Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: performance.setResourceTimingBufferSize() preserves the \
                     API shape; resource timing collection is not yet connected"
                );
            });
            Value::Undefined
        }),
    );
    props.insert(
        "measureUserAgentSpecificMemory".to_string(),
        func(|_, _| {
            static WARNING: std::sync::Once = std::sync::Once::new();
            WARNING.call_once(|| {
                eprintln!(
                    "[w3cos] warning: performance.measureUserAgentSpecificMemory() requires a \
                     host allocator telemetry adapter"
                );
            });
            w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                "Memory telemetry is unavailable",
                "NotSupportedError",
            )])
        }),
    );
    props.insert("eventCounts".to_string(), event_counts_value());
    props.insert("interactionCount".to_string(), Value::Number(0.0));
    props.insert("memory".to_string(), Value::Undefined);
    props.insert("navigation".to_string(), performance_navigation_value());
    props.insert("timing".to_string(), performance_timing_value());
    props.insert("onresourcetimingbufferfull".to_string(), Value::Null);
    props.insert(
        "toJSON".to_string(),
        func(|_, _| {
            Value::object(HashMap::from([(
                "timeOrigin".to_string(),
                Value::Number(performance_time_origin()),
            )]))
        }),
    );
    let value = Value::object(props);
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("Performance"));
    value
}

fn event_count_entries() -> Vec<(String, u64)> {
    EVENT_COUNTS.with(|counts| {
        EVENT_COUNT_TYPES
            .iter()
            .map(|event_type| {
                (
                    (*event_type).to_string(),
                    counts.borrow().get(*event_type).copied().unwrap_or(0),
                )
            })
            .collect()
    })
}

fn event_counts_value() -> Value {
    let value = Value::object(HashMap::new());
    value.set_property(
        "__w3cos_getter_size",
        func(|_, _| Value::Number(EVENT_COUNT_TYPES.len() as f64)),
    );
    value.set_property(
        "get",
        func(|_, args| {
            let event_type = arg(&args, 0).to_js_string();
            EVENT_COUNTS.with(|counts| {
                counts
                    .borrow()
                    .get(&event_type)
                    .copied()
                    .map(|count| Value::Number(count as f64))
                    .unwrap_or(Value::Undefined)
            })
        }),
    );
    value.set_property(
        "has",
        func(|_, args| {
            let event_type = arg(&args, 0).to_js_string();
            Value::Bool(EVENT_COUNTS.with(|counts| counts.borrow().contains_key(&event_type)))
        }),
    );
    value.set_property(
        "keys",
        func(|_, _| {
            Value::array(
                EVENT_COUNT_TYPES
                    .iter()
                    .map(|event_type| Value::string(event_type))
                    .collect(),
            )
        }),
    );
    value.set_property(
        "values",
        func(|_, _| {
            Value::array(
                event_count_entries()
                    .into_iter()
                    .map(|(_, count)| Value::Number(count as f64))
                    .collect(),
            )
        }),
    );
    value.set_property(
        "entries",
        func(|_, _| {
            Value::array(
                event_count_entries()
                    .into_iter()
                    .map(|(event_type, count)| {
                        Value::array(vec![Value::from(event_type), Value::Number(count as f64)])
                    })
                    .collect(),
            )
        }),
    );
    let value_for_each = value.clone();
    value.set_property(
        "forEach",
        func(move |_, args| {
            let callback = arg(&args, 0);
            for (event_type, count) in event_count_entries() {
                callback.call(
                    Value::Undefined,
                    vec![
                        Value::Number(count as f64),
                        Value::from(event_type),
                        value_for_each.clone(),
                    ],
                );
            }
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("EventCounts"));
    value
}

pub(crate) fn viewport() -> (f64, f64, f64) {
    VIEWPORT.with(|v| v.get())
}

pub(crate) fn media_query_matches(query: &str) -> bool {
    crate::media::parse_media_query(query)
        .map(|cond| {
            let (w, h, dpr) = viewport();
            crate::media::matches_media(
                &cond,
                &crate::media::Viewport::new(w as f32, h as f32, dpr as f32),
            )
        })
        .unwrap_or(false)
}

fn refresh_media_query_lists() {
    MEDIA_QUERY_LISTS.with(|lists| {
        for list in lists.borrow().iter() {
            let query = list.get_property("media").to_js_string();
            let previous = list.get_property("__w3cos_last_match").to_bool();
            let current = media_query_matches(&query);
            if current == previous {
                continue;
            }
            list.set_property("__w3cos_last_match", Value::Bool(current));
            let event = w3cos_core::class::construct(
                &crate::web_events::event_subclass_class("MediaQueryListEvent"),
                vec![
                    Value::string("change"),
                    Value::object(HashMap::from([
                        ("matches".into(), Value::Bool(current)),
                        ("media".into(), Value::string(&query)),
                    ])),
                ],
            );
            list.call_method("dispatchEvent", vec![event]);
        }
    });
}

/// Set the viewport size reported by `window.innerWidth/innerHeight`,
/// `screen`, and `matchMedia`. Default 1024x768.
pub fn set_viewport(width: f64, height: f64) {
    VIEWPORT.with(|v| {
        let (_, _, dpr) = v.get();
        v.set((width, height, dpr));
    });
    if let Some(window) = WINDOW_VALUE.with(|value| value.borrow().clone()) {
        let event = w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![Value::string("resize")],
        );
        window
            .get_property("visualViewport")
            .call_method("dispatchEvent", vec![event]);
    }
    refresh_media_query_lists();
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::refresh_stylesheet_media_queries();
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::refresh_responsive_images();
    crate::observers_web::refresh_intersection_observers();
}

/// Set the devicePixelRatio reported by the window. Default 1.0.
pub fn set_device_pixel_ratio(dpr: f64) {
    VIEWPORT.with(|v| {
        let (w, h, _) = v.get();
        v.set((w, h, dpr));
    });
    refresh_media_query_lists();
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::refresh_stylesheet_media_queries();
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::refresh_responsive_images();
}

/// Update document visibility from a platform lifecycle adapter.
pub fn set_document_visibility(state: &str) -> bool {
    if !matches!(state, "visible" | "hidden") {
        return false;
    }
    let changed = DOCUMENT_VISIBILITY.with(|current| {
        if current.borrow().as_str() == state {
            false
        } else {
            *current.borrow_mut() = state.to_string();
            true
        }
    });
    if !changed {
        return true;
    }
    crate::observers_web::record_visibility_state(state, performance_now());
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string("visibilitychange")],
    );
    document_value().call_method("dispatchEvent", vec![event]);
    true
}

/// Update the live `navigator.maxTouchPoints` value reported to JavaScript.
///
/// Mobile hosts should replace their conservative startup fallback when the
/// input adapter can report an exact simultaneous-contact count.
pub fn set_max_touch_points(points: u32) {
    MAX_TOUCH_POINTS.with(|value| value.set(points));
}

fn storage_value(persistent: bool) -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "getItem".to_string(),
        func(move |_, args| {
            let key = arg(&args, 0).to_js_string();
            let value = if persistent {
                crate::storage::get_item(&key)
            } else {
                SESSION_STORAGE.with(|s| s.borrow().get(&key).cloned())
            };
            value.map(|v| Value::from(v)).unwrap_or(Value::Null)
        }),
    );
    props.insert(
        "setItem".to_string(),
        func(move |_, args| {
            let key = arg(&args, 0).to_js_string();
            let value = arg(&args, 1).to_js_string();
            if persistent {
                crate::storage::set_item(&key, &value);
            } else {
                SESSION_STORAGE.with(|s| s.borrow_mut().insert(key, value));
            }
            Value::Undefined
        }),
    );
    props.insert(
        "removeItem".to_string(),
        func(move |_, args| {
            let key = arg(&args, 0).to_js_string();
            if persistent {
                crate::storage::remove_item(&key);
            } else {
                SESSION_STORAGE.with(|s| s.borrow_mut().remove(&key));
            }
            Value::Undefined
        }),
    );
    props.insert(
        "clear".to_string(),
        func(move |_, _| {
            if persistent {
                crate::storage::clear();
            } else {
                SESSION_STORAGE.with(|s| s.borrow_mut().clear());
            }
            Value::Undefined
        }),
    );
    props.insert(
        "key".to_string(),
        func(move |_, args| {
            let idx = arg(&args, 0).to_u32() as usize;
            if persistent {
                crate::storage::key(idx)
                    .map(|k| Value::from(k))
                    .unwrap_or(Value::Null)
            } else {
                SESSION_STORAGE.with(|s| {
                    s.borrow()
                        .keys()
                        .nth(idx)
                        .map(|k| Value::string(k))
                        .unwrap_or(Value::Null)
                })
            }
        }),
    );
    props.insert(
        "__w3cos_getter_length".to_string(),
        func(move |_, _| {
            Value::Number(if persistent {
                crate::storage::length() as f64
            } else {
                SESSION_STORAGE.with(|s| s.borrow().len() as f64)
            })
        }),
    );
    let value = Value::object(props);
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("Storage"));
    value
}

fn crypto_key_class() -> Value {
    CRYPTO_KEY_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = func(|_, _| {
            w3cos_core::throw_value(w3cos_core::error_instance(
                "TypeError",
                vec![Value::string("Illegal constructor: CryptoKey")],
            ))
        });
        class.set_property("name", Value::string("CryptoKey"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in ["algorithm", "extractable", "type", "usages"] {
            prototype.set_property(property, Value::Undefined);
        }
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn dom_error_class() -> Value {
    DOM_ERROR_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = func(|this, args| {
            this.set_property(
                "name",
                Value::string(&args.first().map(Value::to_js_string).unwrap_or_default()),
            );
            this.set_property(
                "message",
                Value::string(&args.get(1).map(Value::to_js_string).unwrap_or_default()),
            );
            Value::Undefined
        });
        class.set_property("name", Value::string("DOMError"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        prototype.set_property("message", Value::Undefined);
        prototype.set_property("name", Value::Undefined);
        class.set_property("prototype", prototype);
        *slot.borrow_mut() = Some(class.clone());
        class
    })
}

fn crypto_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();
    props.insert(
        "getRandomValues".to_string(),
        func(|_, args| {
            let arr = arg(&args, 0);
            let length = arr.get_property("length").to_u32() as usize;
            let mut bytes = vec![0u8; length];
            if getrandom::fill(&mut bytes).is_ok() {
                for (index, byte) in bytes.into_iter().enumerate() {
                    arr.set_property(&index.to_string(), Value::Number(byte as f64));
                }
            }
            arr
        }),
    );
    props.insert(
        "randomUUID".to_string(),
        func(|_, _| {
            let mut bytes = [0u8; 16];
            if getrandom::fill(&mut bytes).is_err() {
                return Value::Undefined;
            }
            bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
            bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant
            Value::string(&format!(
                "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
            ))
        }),
    );
    props.insert("subtle".to_string(), subtle_crypto_value());
    let value = Value::object(props);
    w3cos_core::class::set_prototype_of(&value, &crate::dom_constructors::prototype("Crypto"));
    value
}

fn subtle_crypto_value() -> Value {
    let mut props = HashMap::new();
    props.insert(
        "digest".to_string(),
        func(|_, args| {
            let algorithm = arg(&args, 0);
            let algorithm_name = if algorithm.is_object() {
                algorithm.get_property("name").to_js_string()
            } else {
                algorithm.to_js_string()
            };
            let algorithm_name = algorithm_name.to_ascii_uppercase();
            let algorithm = match algorithm_name.as_str() {
                "SHA-1" => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
                "SHA-256" => &ring::digest::SHA256,
                "SHA-384" => &ring::digest::SHA384,
                "SHA-512" => &ring::digest::SHA512,
                _ => {
                    return w3cos_core::promise::reject(vec![
                        w3cos_core::web::dom_exception_instance(
                            &format!("Unrecognized digest algorithm: {algorithm_name}"),
                            "NotSupportedError",
                        ),
                    ]);
                }
            };
            let input = arg(&args, 1);
            let Some(bytes) = w3cos_core::binary::bytes_of(&input) else {
                return w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                    "crypto.subtle.digest() requires an ArrayBuffer or ArrayBufferView",
                    "TypeError",
                )]);
            };
            let digest = ring::digest::digest(algorithm, &bytes);
            w3cos_core::promise::resolve(vec![w3cos_core::binary::array_buffer_value(
                digest.as_ref().to_vec(),
            )])
        }),
    );
    for method in [
        "decrypt",
        "deriveBits",
        "deriveKey",
        "encrypt",
        "exportKey",
        "generateKey",
        "importKey",
        "sign",
        "unwrapKey",
        "verify",
        "wrapKey",
    ] {
        props.insert(
            method.to_string(),
            func(move |_, _| {
                static WARNING: std::sync::Once = std::sync::Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: crypto.subtle preserves the Web Crypto Promise API, \
                         but cryptographic operations require a configured native provider"
                    );
                });
                w3cos_core::promise::reject(vec![w3cos_core::web::dom_exception_instance(
                    &format!("crypto.subtle.{method}() is unavailable"),
                    "NotSupportedError",
                )])
            }),
        );
    }
    let value = Value::object(props);
    w3cos_core::class::set_prototype_of(
        &value,
        &crate::dom_constructors::prototype("SubtleCrypto"),
    );
    value
}

fn match_media_value(query: &str) -> Value {
    let matches = media_query_matches(query);
    let mut props: HashMap<String, Value> = HashMap::new();
    let query_for_getter = query.to_string();
    props.insert(
        "__w3cos_getter_matches".to_string(),
        func(move |_, _| Value::Bool(media_query_matches(&query_for_getter))),
    );
    props.insert("__w3cos_last_match".to_string(), Value::Bool(matches));
    props.insert("media".to_string(), Value::string(query));
    props.insert("onchange".to_string(), Value::Null);
    let value = Value::object(props);
    crate::web_events::event_target_class().call(value.clone(), vec![]);
    let value_for_add = value.clone();
    value.set_property(
        "addListener",
        func(move |_, args| {
            value_for_add.call_method(
                "addEventListener",
                vec![Value::string("change"), arg(&args, 0)],
            );
            Value::Undefined
        }),
    );
    let value_for_remove = value.clone();
    value.set_property(
        "removeListener",
        func(move |_, args| {
            value_for_remove.call_method(
                "removeEventListener",
                vec![Value::string("change"), arg(&args, 0)],
            );
            Value::Undefined
        }),
    );
    w3cos_core::class::set_prototype_of(
        &value,
        &media_query_list_class().get_property("prototype"),
    );
    MEDIA_QUERY_LISTS.with(|lists| lists.borrow_mut().push(value.clone()));
    value
}

fn media_query_list_class() -> Value {
    MEDIA_QUERY_LIST_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: MediaQueryList"),
                ),
            ])))
        });
        class.set_property("name", Value::string("MediaQueryList"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for method in ["addListener", "removeListener"] {
            prototype.set_property(method, func(|_, _| Value::Undefined));
        }
        for property in ["matches", "media", "onchange"] {
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

fn visual_viewport_class() -> Value {
    VISUAL_VIEWPORT_CLASS.with(|slot| {
        if let Some(class) = slot.borrow().clone() {
            return class;
        }
        let class = Value::function(|_, _| {
            w3cos_core::throw_value(Value::object(HashMap::from([
                ("name".into(), Value::string("TypeError")),
                (
                    "message".into(),
                    Value::string("Illegal constructor: VisualViewport"),
                ),
            ])))
        });
        class.set_property("name", Value::string("VisualViewport"));
        let prototype = Value::object(HashMap::new());
        prototype.set_property("constructor", class.clone());
        for property in [
            "height",
            "offsetLeft",
            "offsetTop",
            "onresize",
            "onscroll",
            "onscrollend",
            "pageLeft",
            "pageTop",
            "scale",
            "width",
        ] {
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

/// The global `window` value (memoized thread-local singleton).
pub fn window_value() -> Value {
    if let Some(v) = WINDOW_VALUE.with(|w| w.borrow().clone()) {
        return v;
    }
    let value = build_window_value();
    WINDOW_VALUE.with(|w| *w.borrow_mut() = Some(value.clone()));
    value
}

fn build_window_value() -> Value {
    let mut props: HashMap<String, Value> = HashMap::new();

    props.insert("document".to_string(), document_value());
    props.insert("String".to_string(), string_compat_class());
    props.insert("Number".to_string(), number_compat_class());
    props.insert("Boolean".to_string(), boolean_compat_class());
    props.insert("Promise".to_string(), promise_constructor_value());
    props.insert("BarProp".to_string(), bar_prop_class());
    for name in [
        "locationbar",
        "menubar",
        "personalbar",
        "scrollbars",
        "statusbar",
        "toolbar",
    ] {
        props.insert(name.to_string(), bar_prop_value());
    }
    props.insert("navigator".to_string(), navigator_value());
    props.insert("location".to_string(), location_value());
    props.insert("performance".to_string(), performance_value());
    props.insert("crypto".to_string(), crypto_value());
    props.insert("localStorage".to_string(), storage_value(true));
    props.insert("sessionStorage".to_string(), storage_value(false));
    props.insert(
        "indexedDB".to_string(),
        crate::indexed_db_web::factory_value(),
    );
    props.insert(
        "IDBKeyRange".to_string(),
        crate::indexed_db_web::key_range_constructor_value(),
    );
    for name in crate::indexed_db_web::IDB_INTERFACE_NAMES {
        if *name != "IDBKeyRange" {
            props.insert(
                (*name).to_string(),
                crate::indexed_db_web::interface_class(name),
            );
        }
    }
    props.insert("WebSocket".to_string(), crate::websocket::websocket_class());
    props.insert(
        "EventSource".to_string(),
        crate::eventsource::event_source_class(),
    );
    props.insert(
        "XMLHttpRequest".to_string(),
        crate::xhr::xml_http_request_class(),
    );
    props.insert(
        "XMLHttpRequestEventTarget".to_string(),
        crate::xhr::xml_http_request_event_target_class(),
    );
    props.insert(
        "XMLHttpRequestUpload".to_string(),
        crate::xhr::xml_http_request_upload_class(),
    );
    for name in ["Image", "Audio", "Option"] {
        props.insert(name.to_string(), legacy_element_factory_class(name));
    }
    props.insert(
        "Notification".to_string(),
        crate::notification_web::notification_class(),
    );
    #[cfg(feature = "web-media-advanced")]
    {
        props.insert(
            "MediaStream".to_string(),
            crate::media_devices_web::media_stream_class(),
        );
        props.insert(
            "MediaStreamTrack".to_string(),
            crate::media_devices_web::media_stream_track_class(),
        );
    }
    #[cfg(all(feature = "web-graphics-advanced", feature = "web-media-advanced"))]
    {
        props.insert(
            "MediaStreamTrackGenerator".to_string(),
            crate::media_devices_web::media_stream_track_generator_class(),
        );
        props.insert(
            "MediaStreamTrackProcessor".to_string(),
            crate::media_devices_web::media_stream_track_processor_class(),
        );
    }
    #[cfg(feature = "web-media-advanced")]
    {
        for name in [
            "AudioPlaybackStats",
            "MediaStreamTrackAudioStats",
            "MediaStreamTrackVideoStats",
        ] {
            props.insert(
                name.to_string(),
                crate::media_devices_web::media_stats_class(name),
            );
        }
        props.insert(
            "MediaRecorder".to_string(),
            crate::media_recording_web::media_recorder_class(),
        );
        props.insert(
            "ImageCapture".to_string(),
            crate::media_recording_web::image_capture_class(),
        );
        props.insert(
            "CaptureController".to_string(),
            crate::media_recording_web::capture_controller_class(),
        );
        props.insert(
            "CropTarget".to_string(),
            crate::media_recording_web::crop_target_class(),
        );
        props.insert(
            "RestrictionTarget".to_string(),
            crate::media_recording_web::restriction_target_class(),
        );
        props.insert(
            "BrowserCaptureMediaStreamTrack".to_string(),
            crate::media_recording_web::browser_capture_media_stream_track_class(),
        );
        props.insert(
            "MediaSource".to_string(),
            crate::media_source_web::media_source_class(),
        );
        props.insert(
            "MediaSourceHandle".to_string(),
            crate::media_source_web::media_source_handle_class(),
        );
        props.insert(
            "SourceBuffer".to_string(),
            crate::media_source_web::source_buffer_class(),
        );
        props.insert(
            "SourceBufferList".to_string(),
            crate::media_source_web::source_buffer_list_class(),
        );
    }
    for name in [
        "PaymentAddress",
        "PaymentManager",
        "PaymentRequest",
        "PaymentResponse",
    ] {
        props.insert(name.to_string(), crate::payment_web::class_for(name));
    }
    #[cfg(feature = "web-graphics-advanced")]
    {
        for name in [
            "AudioData",
            "AudioDecoder",
            "AudioEncoder",
            "EncodedAudioChunk",
            "EncodedVideoChunk",
            "VideoColorSpace",
            "VideoDecoder",
            "VideoEncoder",
            "VideoFrame",
        ] {
            props.insert(name.to_string(), crate::webcodecs_web::class_for(name));
        }
        for name in ["ImageDecoder", "ImageTrack", "ImageTrackList"] {
            props.insert(name.to_string(), crate::image_decoder_web::class_for(name));
        }
    }
    #[cfg(feature = "web-media-advanced")]
    for (name, class) in crate::webrtc_web::classes() {
        props.insert(name.to_string(), class);
    }
    for name in crate::experimental_web::INTERFACES {
        props.insert(
            (*name).to_string(),
            crate::experimental_web::class_for(name),
        );
    }
    for name in crate::web_transport_web::INTERFACES {
        props.insert(
            (*name).to_string(),
            crate::web_transport_web::class_for(name),
        );
    }
    #[cfg(feature = "web-graphics-advanced")]
    {
        for name in crate::webxr_web::INTERFACES {
            props.insert(name.to_string(), crate::webxr_web::class_for(name));
        }
        for name in crate::webgpu_web::INTERFACES {
            props.insert(name.to_string(), crate::webgpu_web::class_for(name));
        }
        for name in crate::webgl_web::INTERFACES {
            props.insert(name.to_string(), crate::webgl_web::class_for(name));
        }
        for name in crate::webgpu_web::CONSTANTS {
            props.insert(name.to_string(), crate::webgpu_web::constant_value(name));
        }
    }
    props.insert(
        "sharedStorage".to_string(),
        crate::experimental_web::shared_storage_value(),
    );
    props.insert(
        "viewport".to_string(),
        crate::experimental_web::viewport_value(),
    );
    props.insert(
        "queryLocalFonts".to_string(),
        crate::experimental_web::query_local_fonts_value(),
    );
    props.insert(
        "fetchLater".to_string(),
        crate::experimental_web::fetch_later_value(),
    );
    #[cfg(feature = "web-media-advanced")]
    for name in [
        "AnalyserNode",
        "AudioBuffer",
        "AudioBufferSourceNode",
        "AudioContext",
        "AudioDestinationNode",
        "AudioListener",
        "AudioNode",
        "AudioParam",
        "AudioParamMap",
        "AudioScheduledSourceNode",
        "AudioSinkInfo",
        "AudioWorklet",
        "AudioWorkletNode",
        "BaseAudioContext",
        "BiquadFilterNode",
        "ChannelMergerNode",
        "ChannelSplitterNode",
        "ConstantSourceNode",
        "ConvolverNode",
        "DelayNode",
        "DynamicsCompressorNode",
        "GainNode",
        "IIRFilterNode",
        "MediaElementAudioSourceNode",
        "MediaStreamAudioDestinationNode",
        "MediaStreamAudioSourceNode",
        "OfflineAudioContext",
        "OscillatorNode",
        "PannerNode",
        "PeriodicWave",
        "ScriptProcessorNode",
        "StereoPannerNode",
        "WaveShaperNode",
        "Worklet",
    ] {
        props.insert(name.to_string(), crate::audio_web::class_for(name));
    }
    #[cfg(feature = "web-media-advanced")]
    {
        props.insert(
            "MediaDeviceInfo".to_string(),
            crate::media_devices_web::media_device_info_class(),
        );
        props.insert(
            "InputDeviceInfo".to_string(),
            crate::media_devices_web::input_device_info_class(),
        );
        props.insert(
            "MediaDevices".to_string(),
            crate::media_devices_web::media_devices_class(),
        );
        props.insert(
            "OverconstrainedError".to_string(),
            crate::media_devices_web::overconstrained_error_class(),
        );
    }
    let speech_recognition = crate::speech_web::speech_recognition_class();
    props.insert("SpeechRecognition".to_string(), speech_recognition.clone());
    props.insert("webkitSpeechRecognition".to_string(), speech_recognition);
    let speech_grammar = crate::speech_web::speech_grammar_class();
    props.insert("SpeechGrammar".to_string(), speech_grammar.clone());
    props.insert("webkitSpeechGrammar".to_string(), speech_grammar);
    let speech_grammar_list = crate::speech_web::speech_grammar_list_class();
    props.insert("SpeechGrammarList".to_string(), speech_grammar_list.clone());
    props.insert("webkitSpeechGrammarList".to_string(), speech_grammar_list);
    props.insert(
        "SpeechRecognitionPhrase".to_string(),
        crate::speech_web::speech_recognition_phrase_class(),
    );
    props.insert(
        "SpeechSynthesis".to_string(),
        crate::speech_synthesis_web::speech_synthesis_class(),
    );
    props.insert(
        "SpeechSynthesisUtterance".to_string(),
        crate::speech_synthesis_web::speech_synthesis_utterance_class(),
    );
    props.insert(
        "SpeechSynthesisVoice".to_string(),
        crate::speech_synthesis_web::speech_synthesis_voice_class(),
    );
    props.insert(
        "speechSynthesis".to_string(),
        crate::speech_synthesis_web::speech_synthesis_value(),
    );
    props.insert(
        "ClipboardItem".to_string(),
        crate::clipboard_web::clipboard_item_class(),
    );
    props.insert(
        "Clipboard".to_string(),
        crate::clipboard_web::clipboard_class(),
    );
    props.insert(
        "DataTransferItem".to_string(),
        crate::clipboard_web::data_transfer_item_class(),
    );
    props.insert(
        "DataTransferItemList".to_string(),
        crate::clipboard_web::data_transfer_item_list_class(),
    );
    props.insert(
        "FileList".to_string(),
        crate::clipboard_web::file_list_class(),
    );
    props.insert(
        "Geolocation".to_string(),
        crate::geolocation_web::geolocation_class(),
    );
    props.insert(
        "GeolocationCoordinates".to_string(),
        crate::geolocation_web::coordinates_class(),
    );
    props.insert(
        "GeolocationPosition".to_string(),
        crate::geolocation_web::position_class(),
    );
    props.insert(
        "GeolocationPositionError".to_string(),
        crate::geolocation_web::position_error_class(),
    );
    props.insert(
        "DataTransfer".to_string(),
        crate::clipboard_web::data_transfer_class(),
    );
    props.insert("Worker".to_string(), crate::worker_web::worker_class());
    props.insert(
        "SharedWorker".to_string(),
        crate::worker_web::shared_worker_class(),
    );
    props.insert(
        "MessagePort".to_string(),
        crate::worker_web::message_port_class(),
    );
    props.insert(
        "MessageChannel".to_string(),
        crate::worker_web::message_channel_class(),
    );
    props.insert(
        "BroadcastChannel".to_string(),
        crate::worker_web::broadcast_channel_class(),
    );
    props.insert(
        "CustomElementRegistry".to_string(),
        crate::custom_elements_web::custom_element_registry_class(),
    );
    props.insert(
        "CustomStateSet".to_string(),
        crate::custom_elements_web::custom_state_set_class(),
    );
    props.insert(
        "ElementInternals".to_string(),
        crate::custom_elements_web::element_internals_class(),
    );
    props.insert(
        "CSSPseudoElement".to_string(),
        crate::custom_elements_web::css_pseudo_element_class(),
    );
    props.insert(
        "EditContext".to_string(),
        crate::edit_context_web::edit_context_class(),
    );
    props.insert(
        "TextFormat".to_string(),
        crate::edit_context_web::text_format_class(),
    );
    props.insert(
        "customElements".to_string(),
        crate::custom_elements_web::custom_elements_value(),
    );
    props.insert("Cache".to_string(), crate::cache_web::cache_class());
    props.insert(
        "CacheStorage".to_string(),
        crate::cache_web::cache_storage_class(),
    );
    props.insert(
        "caches".to_string(),
        crate::cache_web::cache_storage_value(),
    );
    props.insert(
        "scheduler".to_string(),
        crate::scheduler_web::scheduler_value(),
    );
    props.insert(
        "cookieStore".to_string(),
        crate::cookie_store_web::cookie_store_value(),
    );
    props.insert(
        "CookieStore".to_string(),
        crate::cookie_store_web::cookie_store_class(),
    );
    props.insert(
        "CookieChangeEvent".to_string(),
        crate::cookie_store_web::cookie_change_event_class(),
    );
    props.insert("Lock".to_string(), crate::locks_web::lock_class());
    props.insert(
        "LockManager".to_string(),
        crate::locks_web::lock_manager_class(),
    );
    props.insert(
        "TaskController".to_string(),
        crate::scheduler_web::task_controller_class(),
    );
    props.insert(
        "TaskSignal".to_string(),
        crate::scheduler_web::task_signal_class(),
    );
    props.insert(
        "Scheduler".to_string(),
        crate::scheduler_web::scheduler_class(),
    );
    props.insert(
        "WakeLock".to_string(),
        crate::wake_lock_web::wake_lock_class(),
    );
    props.insert(
        "WakeLockSentinel".to_string(),
        crate::wake_lock_web::wake_lock_sentinel_class(),
    );
    props.insert(
        "PermissionStatus".to_string(),
        crate::permissions_web::permission_status_class(),
    );
    props.insert(
        "Permissions".to_string(),
        crate::permissions_web::permissions_class(),
    );
    props.insert(
        "Bluetooth".to_string(),
        crate::bluetooth_web::bluetooth_class(),
    );
    for name in crate::bluetooth_web::BLUETOOTH_INTERFACE_NAMES {
        props.insert(
            (*name).to_string(),
            crate::bluetooth_web::interface_class(name),
        );
    }
    props.insert(
        "NetworkInformation".to_string(),
        crate::network_information_web::network_information_class(),
    );
    props.insert(
        "StorageManager".to_string(),
        crate::storage_manager_web::storage_manager_class(),
    );
    props.insert(
        "StorageBucketManager".to_string(),
        crate::storage_buckets_web::storage_bucket_manager_class(),
    );
    props.insert(
        "StorageBucket".to_string(),
        crate::storage_buckets_web::storage_bucket_class(),
    );
    props.insert(
        "UserActivation".to_string(),
        crate::user_activation_web::user_activation_class(),
    );
    #[cfg(feature = "web-media-advanced")]
    {
        props.insert(
            "MediaMetadata".to_string(),
            crate::media_session_web::media_metadata_class(),
        );
        props.insert(
            "ChapterInformation".to_string(),
            crate::media_session_web::chapter_information_class(),
        );
        props.insert(
            "MediaSession".to_string(),
            crate::media_session_web::media_session_class(),
        );
    }
    props.insert(
        "BatteryManager".to_string(),
        crate::battery_web::battery_manager_class(),
    );
    #[cfg(feature = "web-media-advanced")]
    props.insert(
        "MediaCapabilities".to_string(),
        crate::media_capabilities_web::media_capabilities_class(),
    );
    props.insert(
        "Navigator".to_string(),
        crate::navigator_web::navigator_class(),
    );
    props.insert(
        "NavigatorLogin".to_string(),
        crate::navigator_web::navigator_login_class(),
    );
    props.insert(
        "NavigatorManagedData".to_string(),
        crate::navigator_web::navigator_managed_data_class(),
    );
    props.insert(
        "Serial".to_string(),
        crate::device_access_web::serial_class(),
    );
    props.insert(
        "SerialPort".to_string(),
        crate::device_access_web::serial_port_class(),
    );
    props.insert("HID".to_string(), crate::device_access_web::hid_class());
    props.insert(
        "HIDDevice".to_string(),
        crate::device_access_web::hid_device_class(),
    );
    props.insert(
        "HIDInputReportEvent".to_string(),
        crate::device_access_web::hid_input_report_event_class(),
    );
    props.insert("USB".to_string(), crate::device_access_web::usb_class());
    props.insert(
        "USBDevice".to_string(),
        crate::device_access_web::usb_device_class(),
    );
    for name in crate::device_access_web::USB_RECORD_NAMES {
        props.insert(
            (*name).to_string(),
            crate::device_access_web::usb_record_class(name),
        );
    }
    props.insert(
        "USBConnectionEvent".to_string(),
        crate::device_access_web::usb_connection_event_class(),
    );
    props.insert(
        "Keyboard".to_string(),
        crate::window_environment_web::keyboard_class(),
    );
    props.insert(
        "KeyboardLayoutMap".to_string(),
        crate::window_environment_web::keyboard_layout_map_class(),
    );
    props.insert(
        "VirtualKeyboard".to_string(),
        crate::window_environment_web::virtual_keyboard_class(),
    );
    props.insert(
        "DevicePosture".to_string(),
        crate::window_environment_web::device_posture_class(),
    );
    props.insert(
        "WindowControlsOverlay".to_string(),
        crate::window_environment_web::window_controls_overlay_class(),
    );
    props.insert(
        "Scheduling".to_string(),
        crate::window_environment_web::scheduling_class(),
    );
    props.insert(
        "Presentation".to_string(),
        crate::presentation_web::presentation_class(),
    );
    props.insert(
        "PresentationRequest".to_string(),
        crate::presentation_web::presentation_request_class(),
    );
    props.insert(
        "PresentationAvailability".to_string(),
        crate::presentation_web::presentation_availability_class(),
    );
    props.insert(
        "PresentationConnection".to_string(),
        crate::presentation_web::presentation_connection_class(),
    );
    props.insert(
        "PresentationConnectionList".to_string(),
        crate::presentation_web::presentation_connection_list_class(),
    );
    props.insert(
        "PresentationReceiver".to_string(),
        crate::presentation_web::presentation_receiver_class(),
    );
    props.insert(
        "PresentationConnectionAvailableEvent".to_string(),
        crate::presentation_web::presentation_connection_available_event_class(),
    );
    props.insert(
        "PresentationConnectionCloseEvent".to_string(),
        crate::presentation_web::presentation_connection_close_event_class(),
    );
    props.insert(
        "IdleDetector".to_string(),
        crate::user_mediated_web::idle_detector_class(),
    );
    props.insert(
        "EyeDropper".to_string(),
        crate::user_mediated_web::eye_dropper_class(),
    );
    props.insert(
        "CloseWatcher".to_string(),
        crate::close_watcher_web::close_watcher_class(),
    );
    props.insert(
        "NDEFReader".to_string(),
        crate::web_nfc::ndef_reader_class(),
    );
    props.insert(
        "NDEFMessage".to_string(),
        crate::web_nfc::ndef_message_class(),
    );
    props.insert(
        "NDEFRecord".to_string(),
        crate::web_nfc::ndef_record_class(),
    );
    props.insert(
        "NDEFReadingEvent".to_string(),
        crate::web_nfc::ndef_reading_event_class(),
    );
    props.insert("Plugin".to_string(), crate::navigator_web::plugin_class());
    props.insert(
        "PluginArray".to_string(),
        crate::navigator_web::plugin_array_class(),
    );
    props.insert(
        "MimeType".to_string(),
        crate::navigator_web::mime_type_class(),
    );
    props.insert(
        "MimeTypeArray".to_string(),
        crate::navigator_web::mime_type_array_class(),
    );
    props.insert(
        "MIDIAccess".to_string(),
        crate::midi_web::midi_access_class(),
    );
    props.insert("MIDIPort".to_string(), crate::midi_web::midi_port_class());
    props.insert("MIDIInput".to_string(), crate::midi_web::midi_input_class());
    props.insert(
        "MIDIOutput".to_string(),
        crate::midi_web::midi_output_class(),
    );
    props.insert(
        "MIDIInputMap".to_string(),
        crate::midi_web::midi_input_map_class(),
    );
    props.insert(
        "MIDIOutputMap".to_string(),
        crate::midi_web::midi_output_map_class(),
    );
    props.insert(
        "MIDIConnectionEvent".to_string(),
        crate::midi_web::midi_connection_event_class(),
    );
    props.insert(
        "MIDIMessageEvent".to_string(),
        crate::midi_web::midi_message_event_class(),
    );
    props.insert(
        "MediaKeyMessageEvent".to_string(),
        crate::encrypted_media_web::media_key_message_event_class(),
    );
    props.insert(
        "MediaKeySession".to_string(),
        crate::encrypted_media_web::media_key_session_class(),
    );
    props.insert(
        "MediaKeyStatusMap".to_string(),
        crate::encrypted_media_web::media_key_status_map_class(),
    );
    props.insert(
        "MediaKeySystemAccess".to_string(),
        crate::encrypted_media_web::media_key_system_access_class(),
    );
    props.insert(
        "MediaKeys".to_string(),
        crate::encrypted_media_web::media_keys_class(),
    );
    props.insert(
        "ServiceWorker".to_string(),
        crate::service_worker_web::service_worker_class(),
    );
    props.insert(
        "ServiceWorkerContainer".to_string(),
        crate::service_worker_web::service_worker_container_class(),
    );
    props.insert(
        "ServiceWorkerRegistration".to_string(),
        crate::service_worker_web::service_worker_registration_class(),
    );
    for name in ["PushManager", "PushSubscription", "PushSubscriptionOptions"] {
        props.insert(name.to_string(), crate::push_web::class_for(name));
    }
    for name in [
        "BackgroundFetchManager",
        "CookieStoreManager",
        "NavigationPreloadManager",
        "PeriodicSyncManager",
        "SyncManager",
    ] {
        props.insert(
            name.to_string(),
            crate::service_worker_web::companion_manager_class(name),
        );
    }
    for name in ["BackgroundFetchRecord", "BackgroundFetchRegistration"] {
        props.insert(
            name.to_string(),
            crate::service_worker_web::background_fetch_class(name),
        );
    }
    props.insert(
        "Credential".to_string(),
        crate::credentials_web::credential_class(),
    );
    props.insert(
        "PasswordCredential".to_string(),
        crate::credentials_web::password_credential_class(),
    );
    props.insert(
        "FederatedCredential".to_string(),
        crate::credentials_web::federated_credential_class(),
    );
    props.insert(
        "CredentialsContainer".to_string(),
        crate::credentials_web::credentials_container_class(),
    );
    props.insert(
        "DigitalCredential".to_string(),
        crate::credentials_web::digital_credential_class(),
    );
    props.insert(
        "IdentityCredential".to_string(),
        crate::credentials_web::identity_credential_class(),
    );
    props.insert(
        "IdentityCredentialError".to_string(),
        crate::credentials_web::identity_credential_error_class(),
    );
    props.insert(
        "IdentityProvider".to_string(),
        crate::credentials_web::identity_provider_class(),
    );
    props.insert(
        "AuthenticatorResponse".to_string(),
        crate::credentials_web::authenticator_response_class(),
    );
    props.insert(
        "AuthenticatorAssertionResponse".to_string(),
        crate::credentials_web::authenticator_assertion_response_class(),
    );
    props.insert(
        "AuthenticatorAttestationResponse".to_string(),
        crate::credentials_web::authenticator_attestation_response_class(),
    );
    props.insert(
        "PublicKeyCredential".to_string(),
        crate::credentials_web::public_key_credential_class(),
    );
    props.insert(
        "OTPCredential".to_string(),
        crate::credentials_web::otp_credential_class(),
    );
    props.insert("Gamepad".to_string(), crate::gamepad_web::gamepad_class());
    props.insert(
        "GamepadButton".to_string(),
        crate::gamepad_web::gamepad_button_class(),
    );
    props.insert(
        "GamepadEvent".to_string(),
        crate::gamepad_web::gamepad_event_class(),
    );
    props.insert(
        "GamepadHapticActuator".to_string(),
        crate::gamepad_web::gamepad_haptic_actuator_class(),
    );
    props.insert(
        "DeviceOrientationEvent".to_string(),
        crate::orientation_web::device_orientation_event_class(),
    );
    props.insert(
        "DeviceMotionEvent".to_string(),
        crate::orientation_web::device_motion_event_class(),
    );
    props.insert(
        "DeviceMotionEventAcceleration".to_string(),
        crate::orientation_web::device_motion_acceleration_class(),
    );
    props.insert(
        "DeviceMotionEventRotationRate".to_string(),
        crate::orientation_web::device_motion_rotation_rate_class(),
    );
    props.insert("Sensor".to_string(), crate::sensors_web::sensor_class());
    props.insert(
        "SensorErrorEvent".to_string(),
        crate::sensors_web::sensor_error_event_class(),
    );
    props.insert(
        "Accelerometer".to_string(),
        crate::sensors_web::accelerometer_class(),
    );
    props.insert(
        "Gyroscope".to_string(),
        crate::sensors_web::gyroscope_class(),
    );
    props.insert(
        "Magnetometer".to_string(),
        crate::sensors_web::magnetometer_class(),
    );
    props.insert(
        "GravitySensor".to_string(),
        crate::sensors_web::gravity_sensor_class(),
    );
    props.insert(
        "LinearAccelerationSensor".to_string(),
        crate::sensors_web::linear_acceleration_sensor_class(),
    );
    props.insert(
        "OrientationSensor".to_string(),
        crate::sensors_web::orientation_sensor_class(),
    );
    props.insert(
        "AbsoluteOrientationSensor".to_string(),
        crate::sensors_web::absolute_orientation_sensor_class(),
    );
    props.insert(
        "RelativeOrientationSensor".to_string(),
        crate::sensors_web::relative_orientation_sensor_class(),
    );
    props.insert(
        "ReadableStream".to_string(),
        crate::streams_web::readable_stream_class(),
    );
    props.insert(
        "ReadableStreamDefaultReader".to_string(),
        crate::streams_web::readable_stream_default_reader_class(),
    );
    props.insert(
        "ReadableStreamDefaultController".to_string(),
        crate::streams_web::readable_stream_default_controller_class(),
    );
    props.insert(
        "ReadableByteStreamController".to_string(),
        crate::streams_web::readable_byte_stream_controller_class(),
    );
    props.insert(
        "ReadableStreamBYOBReader".to_string(),
        crate::streams_web::readable_stream_byob_reader_class(),
    );
    props.insert(
        "ReadableStreamBYOBRequest".to_string(),
        crate::streams_web::readable_stream_byob_request_class(),
    );
    props.insert(
        "WritableStream".to_string(),
        crate::streams_web::writable_stream_class(),
    );
    props.insert(
        "WritableStreamDefaultWriter".to_string(),
        crate::streams_web::writable_stream_default_writer_class(),
    );
    props.insert(
        "WritableStreamDefaultController".to_string(),
        crate::streams_web::writable_stream_default_controller_class(),
    );
    props.insert(
        "TransformStream".to_string(),
        crate::streams_web::transform_stream_class(),
    );
    props.insert(
        "TransformStreamDefaultController".to_string(),
        crate::streams_web::transform_stream_default_controller_class(),
    );
    props.insert(
        "CountQueuingStrategy".to_string(),
        crate::streams_web::count_queuing_strategy_class(),
    );
    props.insert(
        "ByteLengthQueuingStrategy".to_string(),
        crate::streams_web::byte_length_queuing_strategy_class(),
    );
    props.insert(
        "TextEncoderStream".to_string(),
        crate::streams_web::text_encoder_stream_class(),
    );
    props.insert(
        "TextDecoderStream".to_string(),
        crate::streams_web::text_decoder_stream_class(),
    );
    props.insert(
        "CompressionStream".to_string(),
        crate::streams_web::compression_stream_class(),
    );
    props.insert(
        "DecompressionStream".to_string(),
        crate::streams_web::decompression_stream_class(),
    );
    props.insert("IdleDeadline".to_string(), idle_deadline_class());
    props.insert(
        "FontFace".to_string(),
        crate::font_loading_web::font_face_class(),
    );
    props.insert(
        "FontFaceSet".to_string(),
        crate::font_loading_web::font_face_set_class(),
    );
    props.insert("DOMParser".to_string(), dom_parser_class());
    props.insert("XMLSerializer".to_string(), xml_serializer_class());
    props.insert("CSS".to_string(), css_namespace_value());
    props.insert(
        "Highlight".to_string(),
        crate::highlight_web::highlight_class(),
    );
    props.insert(
        "HighlightRegistry".to_string(),
        crate::highlight_web::highlight_registry_class(),
    );
    for name in crate::css_typed_om_web::CLASS_NAMES {
        props.insert(name.to_string(), crate::css_typed_om_web::class(name));
    }
    for name in crate::css_rules_web::CLASS_NAMES {
        props.insert(name.to_string(), crate::css_rules_web::class(name));
    }
    props.insert(
        "CSSStyleDeclaration".to_string(),
        css_style_declaration_class(),
    );
    props.insert("CSSStyleSheet".to_string(), css_style_sheet_class());
    props.insert("StyleSheet".to_string(), style_sheet_class());
    props.insert("StyleSheetList".to_string(), style_sheet_list_class());
    props.insert("MediaList".to_string(), media_list_class());
    props.insert("URL".to_string(), w3cos_core::web::url_class());
    props.insert(
        "URLSearchParams".to_string(),
        w3cos_core::web::url_search_params_class(),
    );
    props.insert(
        "URLPattern".to_string(),
        w3cos_core::web::url_pattern_class(),
    );
    props.insert("NodeFilter".to_string(), node_filter_value());
    props.insert("NodeList".to_string(), dom_collection_class("NodeList"));
    props.insert(
        "HTMLCollection".to_string(),
        dom_collection_class("HTMLCollection"),
    );
    for name in [
        "DOMPointReadOnly",
        "DOMPoint",
        "DOMRectReadOnly",
        "DOMRect",
        "DOMRectList",
        "DOMQuad",
        "DOMMatrixReadOnly",
        "DOMMatrix",
    ] {
        props.insert(name.to_string(), crate::geometry_web::class(name));
    }
    for name in ["SVGPoint", "SVGRect", "SVGMatrix"] {
        props.insert(
            name.to_string(),
            crate::svg_values_web::geometry_alias_class(name),
        );
    }
    for name in crate::svg_values_web::SVG_VALUE_CLASS_NAMES {
        props.insert(name.to_string(), crate::svg_values_web::class(name));
    }
    props.insert(
        "DOMException".to_string(),
        crate::unsupported::dom_exception_class(),
    );
    props.insert("DOMError".to_string(), dom_error_class());
    props.insert("CryptoKey".to_string(), crypto_key_class());
    props.insert("CaretPosition".to_string(), caret_position_class());
    props.insert("MathMLElement".to_string(), math_ml_element_class());
    props.insert("Window".to_string(), window_class());
    for name in [
        "External",
        "DocumentPictureInPicture",
        "FeaturePolicy",
        "MediaError",
        "NavigatorUAData",
        "Origin",
        "PictureInPictureWindow",
        "QuotaExceededError",
        "RadioNodeList",
        "ReportBody",
        "RemotePlayback",
        "TimeRanges",
        "WebSocketError",
    ] {
        props.insert(name.to_string(), crate::compat_web::class(name));
    }
    props.insert("external".to_string(), crate::compat_web::external_value());
    props.insert(
        "documentPictureInPicture".to_string(),
        crate::compat_web::document_picture_in_picture_value(),
    );
    props.insert(
        "OffscreenCanvasRenderingContext2D".to_string(),
        crate::canvas_web::offscreen_canvas_rendering_context_2d_class(),
    );
    props.insert(
        "WebKitCSSMatrix".to_string(),
        crate::geometry_web::class("DOMMatrix"),
    );
    for name in [
        "FileSystemHandle",
        "FileSystemFileHandle",
        "FileSystemDirectoryHandle",
        "FileSystemObserver",
        "FileSystemWritableFileStream",
    ] {
        props.insert(name.to_string(), crate::file_system_web::class_for(name));
    }
    for name in [
        "TextTrack",
        "TextTrackCue",
        "TextTrackCueList",
        "TextTrackList",
        "VTTCue",
        "VideoPlaybackQuality",
    ] {
        props.insert(name.to_string(), crate::text_tracks_web::class_for(name));
    }
    for name in [
        "AnimationTrigger",
        "Animation",
        "AnimationEffect",
        "AnimationTimeline",
        "CSSAnimation",
        "CSSTransition",
        "DocumentTimeline",
        "KeyframeEffect",
        "ScrollTimeline",
        "TimelineTrigger",
        "TimelineTriggerRange",
        "TimelineTriggerRangeList",
        "ViewTimeline",
    ] {
        props.insert(name.to_string(), crate::animations_web::class_for(name));
    }
    props.insert(
        "XPathEvaluator".to_string(),
        crate::xpath_web::xpath_evaluator_class(),
    );
    props.insert(
        "XPathExpression".to_string(),
        crate::xpath_web::xpath_expression_class(),
    );
    props.insert(
        "XPathResult".to_string(),
        crate::xpath_web::xpath_result_class(),
    );
    props.insert(
        "XSLTProcessor".to_string(),
        crate::xslt_web::xslt_processor_class(),
    );
    for name in [
        "Error",
        "EvalError",
        "RangeError",
        "ReferenceError",
        "SyntaxError",
        "TypeError",
        "URIError",
        "AggregateError",
    ] {
        props.insert(name.to_string(), w3cos_core::error_class(name));
    }
    props.insert("BigInt".to_string(), w3cos_core::bigint::bigint_class());
    props.insert("RegExp".to_string(), w3cos_core::regexp::regexp_class());
    props.insert(
        "WeakMap".to_string(),
        w3cos_core::collections::weak_map_class(),
    );
    props.insert(
        "WeakSet".to_string(),
        w3cos_core::collections::weak_set_class(),
    );
    props.insert("WeakRef".to_string(), w3cos_core::weak::weak_ref_class());
    props.insert(
        "FinalizationRegistry".to_string(),
        w3cos_core::weak::finalization_registry_class(),
    );
    props.insert(
        "encodeURI".to_string(),
        Value::function(|_, args| crate::uri_codec::encode_uri(args)),
    );
    props.insert(
        "encodeURIComponent".to_string(),
        Value::function(|_, args| crate::uri_codec::encode_uri_component(args)),
    );
    props.insert(
        "decodeURI".to_string(),
        Value::function(|_, args| crate::uri_codec::decode_uri(args)),
    );
    props.insert(
        "decodeURIComponent".to_string(),
        Value::function(|_, args| crate::uri_codec::decode_uri_component(args)),
    );
    for name in crate::unsupported::UNSUPPORTED_CONSTRUCTORS {
        props.insert(
            (*name).to_string(),
            crate::unsupported::unsupported_constructor(name),
        );
    }
    props.insert("eval".to_string(), eval_compat_value());
    props.insert(
        "escape".to_string(),
        func(|_, args| Value::string(&legacy_escape(&arg(&args, 0).to_js_string()))),
    );
    props.insert(
        "unescape".to_string(),
        func(|_, args| Value::string(&legacy_unescape(&arg(&args, 0).to_js_string()))),
    );
    props.insert("Function".to_string(), function_compat_class());
    props.insert("Report".to_string(), crate::reporting_web::report_class());
    for name in ["CSPViolationReportBody", "IntegrityViolationReportBody"] {
        props.insert(
            name.to_string(),
            crate::reporting_web::report_body_class(name),
        );
    }
    props.insert(
        "ReportingObserver".to_string(),
        crate::reporting_web::reporting_observer_class(),
    );
    props.insert(
        "Sanitizer".to_string(),
        crate::sanitizer_web::sanitizer_class(),
    );
    props.insert(
        "trustedTypes".to_string(),
        crate::trusted_types_web::factory_value(),
    );
    for name in [
        "TrustedHTML",
        "TrustedScript",
        "TrustedScriptURL",
        "TrustedTypePolicy",
        "TrustedTypePolicyFactory",
    ] {
        props.insert(
            name.to_string(),
            crate::trusted_types_web::trusted_class(name),
        );
    }
    props.insert(
        "reportError".to_string(),
        crate::reporting_web::report_error_value(),
    );
    props.insert("Headers".to_string(), crate::fetch::headers_class());
    props.insert("Request".to_string(), crate::fetch::request_class());
    props.insert("Response".to_string(), crate::fetch::response_class());
    props.insert(
        "fetch".to_string(),
        Value::function(|_, arguments| crate::fetch::fetch_value(arguments)),
    );
    props.insert(
        "AbortController".to_string(),
        crate::fetch::abort_controller_class(),
    );
    props.insert(
        "AbortSignal".to_string(),
        crate::fetch::abort_signal_class(),
    );
    props.insert(
        "TextEncoder".to_string(),
        crate::text_encoding::text_encoder_class(),
    );
    props.insert(
        "TextDecoder".to_string(),
        w3cos_core::web::text_decoder_class(),
    );
    props.insert(
        "ArrayBuffer".to_string(),
        w3cos_core::binary::array_buffer_class(),
    );
    props.insert(
        "SharedArrayBuffer".to_string(),
        w3cos_core::binary::shared_array_buffer_class(),
    );
    props.insert("Atomics".to_string(), w3cos_core::binary::atomics_value());
    props.insert(
        "DataView".to_string(),
        w3cos_core::binary::data_view_class(),
    );
    props.insert("Blob".to_string(), crate::files::blob_class());
    props.insert("File".to_string(), crate::files::file_class());
    props.insert("FileReader".to_string(), crate::files::file_reader_class());
    props.insert("FormData".to_string(), crate::form_data::form_data_class());
    props.insert(
        "ImageData".to_string(),
        crate::canvas_web::image_data_class(),
    );
    props.insert(
        "CanvasGradient".to_string(),
        crate::canvas_web::canvas_gradient_class(),
    );
    props.insert(
        "CanvasPattern".to_string(),
        crate::canvas_web::canvas_pattern_class(),
    );
    props.insert(
        "CanvasRenderingContext2D".to_string(),
        crate::canvas_web::canvas_rendering_context_2d_class(),
    );
    props.insert(
        "ImageBitmap".to_string(),
        crate::canvas_web::image_bitmap_class(),
    );
    props.insert(
        "ImageBitmapRenderingContext".to_string(),
        crate::canvas_web::image_bitmap_rendering_context_class(),
    );
    props.insert(
        "CanvasCaptureMediaStreamTrack".to_string(),
        crate::canvas_web::canvas_capture_media_stream_track_class(),
    );
    props.insert(
        "TextMetrics".to_string(),
        crate::canvas_web::text_metrics_class(),
    );
    props.insert("Path2D".to_string(), crate::canvas_web::path_2d_class());
    props.insert(
        "OffscreenCanvas".to_string(),
        crate::canvas_web::offscreen_canvas_class(),
    );
    props.insert(
        "ResizeObserver".to_string(),
        crate::observers_web::resize_observer_class(),
    );
    props.insert(
        "ResizeObserverEntry".to_string(),
        crate::observers_web::resize_observer_entry_class(),
    );
    props.insert(
        "ResizeObserverSize".to_string(),
        crate::observers_web::resize_observer_size_class(),
    );
    props.insert(
        "MutationObserver".to_string(),
        crate::observers_web::mutation_observer_class(),
    );
    props.insert(
        "WebKitMutationObserver".to_string(),
        crate::observers_web::mutation_observer_class(),
    );
    props.insert(
        "MutationRecord".to_string(),
        crate::observers_web::mutation_record_class(),
    );
    props.insert(
        "IntersectionObserver".to_string(),
        crate::observers_web::intersection_observer_class(),
    );
    props.insert(
        "IntersectionObserverEntry".to_string(),
        crate::observers_web::intersection_observer_entry_class(),
    );
    props.insert(
        "PerformanceObserver".to_string(),
        crate::observers_web::performance_observer_class(),
    );
    for name in ["PerformanceEntry", "PerformanceMark", "PerformanceMeasure"] {
        props.insert(
            name.to_string(),
            crate::observers_web::performance_entry_class(name),
        );
    }
    props.insert(
        "PerformanceObserverEntryList".to_string(),
        crate::observers_web::performance_entry_list_class(),
    );
    props.insert(
        "PerformanceLongTaskTiming".to_string(),
        crate::observers_web::performance_long_task_class(),
    );
    props.insert(
        "TaskAttributionTiming".to_string(),
        crate::observers_web::task_attribution_class(),
    );
    props.insert(
        "VisibilityStateEntry".to_string(),
        crate::observers_web::visibility_state_entry_class(),
    );
    for name in [
        "LargestContentfulPaint",
        "LayoutShift",
        "LayoutShiftAttribution",
        "PerformanceElementTiming",
        "PerformanceEventTiming",
        "PerformanceLongAnimationFrameTiming",
        "PerformanceNavigationTiming",
        "PerformancePaintTiming",
        "PerformanceResourceTiming",
        "PerformanceScriptTiming",
        "PerformanceServerTiming",
        "PerformanceTimingConfidence",
    ] {
        props.insert(
            name.to_string(),
            crate::observers_web::performance_timeline_class(name),
        );
    }
    for name in ["NotRestoredReasonDetails", "NotRestoredReasons"] {
        props.insert(
            name.to_string(),
            crate::observers_web::navigation_diagnostic_class(name),
        );
    }
    for name in w3cos_core::binary::TYPED_ARRAY_NAMES {
        props.insert(
            (*name).to_string(),
            w3cos_core::binary::typed_array_class(name),
        );
    }
    props.insert("Event".to_string(), crate::web_events::event_class());
    props.insert(
        "CustomEvent".to_string(),
        crate::web_events::custom_event_class(),
    );
    props.insert(
        "EventTarget".to_string(),
        crate::web_events::event_target_class(),
    );
    props.insert(
        "Observable".to_string(),
        crate::observable_web::observable_class(),
    );
    props.insert(
        "Subscriber".to_string(),
        crate::observable_web::subscriber_class(),
    );
    props.insert("MediaQueryList".to_string(), media_query_list_class());
    props.insert("VisualViewport".to_string(), visual_viewport_class());
    props.insert("Touch".to_string(), crate::web_events::touch_class());
    props.insert(
        "TouchList".to_string(),
        crate::web_events::touch_list_class(),
    );
    props.insert(
        "InputDeviceCapabilities".to_string(),
        crate::web_events::input_device_capabilities_class(),
    );
    for name in crate::web_events::EVENT_SUBCLASS_NAMES {
        props.insert(
            (*name).to_string(),
            crate::web_events::event_subclass_class(name),
        );
    }
    for name in crate::dom_constructors::DOM_CONSTRUCTOR_NAMES {
        props.insert(
            (*name).to_string(),
            crate::dom_constructors::constructor(name),
        );
    }
    if let Some(document_class) = props.get("Document").cloned() {
        document_class.set_property(
            "parseHTML",
            Value::function(|_, args| {
                crate::jsdom::sanitized_document_value(&arg(&args, 0).to_js_string())
            }),
        );
        document_class.set_property(
            "parseHTMLUnsafe",
            Value::function(|_, args| {
                static WARNING: std::sync::Once = std::sync::Once::new();
                WARNING.call_once(|| {
                    eprintln!(
                        "[w3cos] warning: Document.parseHTMLUnsafe creates inert DOM; script \
                         execution and declarative shadow-root activation remain unavailable"
                    );
                });
                crate::jsdom::unsafe_document_value(&arg(&args, 0).to_js_string())
            }),
        );
    }
    props.insert("closed".to_string(), Value::Bool(false));
    props.insert("isSecureContext".to_string(), Value::Bool(true));
    props.insert("crossOriginIsolated".to_string(), Value::Bool(false));
    props.insert("origin".to_string(), Value::string("w3cos://app"));
    props.insert("name".to_string(), Value::string(""));
    props.insert("status".to_string(), Value::string(""));
    props.insert(
        "__w3cos_getter_frames".to_string(),
        func(|_, _| frame_windows_value()),
    );
    props.insert(
        "__w3cos_getter_length".to_string(),
        func(|_, _| frame_windows_value().get_property("length")),
    );
    props.insert(
        "navigation".to_string(),
        crate::navigation_web::navigation_value(),
    );
    props.insert(
        "launchQueue".to_string(),
        crate::launch_handler_web::launch_queue_value(),
    );
    props.insert(
        "LaunchQueue".to_string(),
        crate::launch_handler_web::launch_queue_class(),
    );
    props.insert(
        "LaunchParams".to_string(),
        crate::launch_handler_web::launch_params_class(),
    );
    props.insert(
        "ViewTransition".to_string(),
        crate::view_transition_web::view_transition_class(),
    );
    props.insert(
        "ViewTransitionTypeSet".to_string(),
        crate::view_transition_web::type_set_class(),
    );
    props.insert(
        "PageRevealEvent".to_string(),
        crate::view_transition_web::page_reveal_event_class(),
    );
    props.insert(
        "PageSwapEvent".to_string(),
        crate::view_transition_web::page_swap_event_class(),
    );
    props.insert(
        "BarcodeDetector".to_string(),
        crate::barcode_detection_web::barcode_detector_class(),
    );
    props.insert(
        "PressureObserver".to_string(),
        crate::pressure_web::pressure_observer_class(),
    );
    props.insert(
        "PressureRecord".to_string(),
        crate::pressure_web::pressure_record_class(),
    );
    props.insert(
        "FragmentDirective".to_string(),
        crate::fragment_directive_web::fragment_directive_class(),
    );
    props.insert(
        "Navigation".to_string(),
        crate::navigation_web::navigation_class(),
    );
    props.insert(
        "NavigationHistoryEntry".to_string(),
        crate::navigation_web::history_entry_class(),
    );
    props.insert(
        "NavigationDestination".to_string(),
        crate::navigation_web::destination_class(),
    );
    props.insert(
        "NavigateEvent".to_string(),
        crate::navigation_web::navigate_event_class(),
    );
    props.insert(
        "NavigationCurrentEntryChangeEvent".to_string(),
        crate::navigation_web::current_entry_change_event_class(),
    );
    props.insert(
        "NavigationTransition".to_string(),
        crate::navigation_web::transition_class(),
    );
    props.insert(
        "NavigationActivation".to_string(),
        crate::navigation_web::activation_class(),
    );

    // Session History API. Values are live so navigation performed by a
    // platform gesture is immediately observable by application code.
    {
        let mut history: HashMap<String, Value> = HashMap::new();
        history.insert(
            "__w3cos_getter_length".to_string(),
            func(|_, _| Value::Number(crate::history::get_length() as f64)),
        );
        history.insert(
            "__w3cos_getter_state".to_string(),
            func(|_, _| {
                crate::history::get_state()
                    .map(|state| Value::string(&state))
                    .unwrap_or(Value::Null)
            }),
        );
        history.insert("scrollRestoration".to_string(), Value::string("auto"));
        history.insert(
            "pushState".to_string(),
            func(|_, args| {
                let state = arg(&args, 0);
                let state = (!state.is_nullish()).then(|| state.to_js_string());
                crate::history::push_state(
                    state.as_deref(),
                    &arg(&args, 1).to_js_string(),
                    &arg(&args, 2).to_js_string(),
                );
                Value::Undefined
            }),
        );
        history.insert(
            "replaceState".to_string(),
            func(|_, args| {
                let state = arg(&args, 0);
                let state = (!state.is_nullish()).then(|| state.to_js_string());
                crate::history::replace_state(
                    state.as_deref(),
                    &arg(&args, 1).to_js_string(),
                    &arg(&args, 2).to_js_string(),
                );
                Value::Undefined
            }),
        );
        history.insert(
            "back".to_string(),
            func(|_, _| {
                crate::history::back();
                Value::Undefined
            }),
        );
        history.insert(
            "forward".to_string(),
            func(|_, _| {
                crate::history::forward();
                Value::Undefined
            }),
        );
        history.insert(
            "go".to_string(),
            func(|_, args| {
                crate::history::go(arg(&args, 0).to_number() as i32);
                Value::Undefined
            }),
        );
        let history = Value::object(history);
        w3cos_core::class::set_prototype_of(
            &history,
            &crate::dom_constructors::prototype("History"),
        );
        props.insert("history".to_string(), history);
    }

    props.insert(
        "screen".to_string(),
        crate::screen_details_web::screen_value(),
    );
    props.insert(
        "getScreenDetails".to_string(),
        func(|_, _| crate::screen_details_web::get_screen_details()),
    );
    props.insert(
        "Screen".to_string(),
        crate::screen_details_web::screen_class(),
    );
    props.insert(
        "ScreenOrientation".to_string(),
        crate::screen_details_web::screen_orientation_class(),
    );
    props.insert(
        "ScreenDetailed".to_string(),
        crate::screen_details_web::screen_detailed_class(),
    );
    props.insert(
        "ScreenDetails".to_string(),
        crate::screen_details_web::screen_details_class(),
    );

    // visualViewport
    {
        let mut vv: HashMap<String, Value> = HashMap::new();
        vv.insert(
            "__w3cos_getter_width".to_string(),
            func(|_, _| Value::Number(viewport().0)),
        );
        vv.insert(
            "__w3cos_getter_height".to_string(),
            func(|_, _| Value::Number(viewport().1)),
        );
        vv.insert("scale".to_string(), Value::Number(1.0));
        vv.insert("onresize".to_string(), Value::Null);
        vv.insert("onscroll".to_string(), Value::Null);
        vv.insert("onscrollend".to_string(), Value::Null);
        for (name, horizontal) in [
            ("offsetLeft", true),
            ("offsetTop", false),
            ("pageLeft", true),
            ("pageTop", false),
        ] {
            vv.insert(
                format!("__w3cos_getter_{name}"),
                func(move |_, _| {
                    let (x, y) = WINDOW_SCROLL.with(Cell::get);
                    Value::Number(if horizontal { x } else { y })
                }),
            );
        }
        let viewport_value = Value::object(vv);
        crate::web_events::event_target_class().call(viewport_value.clone(), vec![]);
        w3cos_core::class::set_prototype_of(
            &viewport_value,
            &visual_viewport_class().get_property("prototype"),
        );
        props.insert("visualViewport".to_string(), viewport_value);
    }

    // Live viewport getters.
    props.insert(
        "__w3cos_getter_innerWidth".to_string(),
        func(|_, _| Value::Number(viewport().0)),
    );
    props.insert(
        "__w3cos_getter_innerHeight".to_string(),
        func(|_, _| Value::Number(viewport().1)),
    );
    props.insert(
        "__w3cos_getter_outerWidth".to_string(),
        func(|_, _| Value::Number(viewport().0)),
    );
    props.insert(
        "__w3cos_getter_outerHeight".to_string(),
        func(|_, _| Value::Number(viewport().1)),
    );
    props.insert(
        "__w3cos_getter_devicePixelRatio".to_string(),
        func(|_, _| Value::Number(viewport().2)),
    );
    for key in [
        "scrollX",
        "scrollY",
        "pageXOffset",
        "pageYOffset",
        "screenX",
        "screenY",
    ] {
        let horizontal = matches!(key, "scrollX" | "pageXOffset" | "screenX");
        props.insert(
            format!("__w3cos_getter_{key}"),
            func(move |_, _| {
                let (x, y) = WINDOW_SCROLL.with(Cell::get);
                Value::Number(if horizontal { x } else { y })
            }),
        );
    }
    props.insert(
        "__w3cos_getter_document".to_string(),
        func(|_, _| document_value()),
    );
    props.insert(
        "__w3cos_getter_defaultView".to_string(),
        func(|_, _| window_value()),
    );
    for key in ["self", "window", "top", "parent", "globalThis"] {
        props.insert(format!("__w3cos_getter_{key}"), func(|_, _| window_value()));
    }

    // Methods.
    props.insert(
        "getComputedStyle".to_string(),
        func(|_, args| match node_id_of(&arg(&args, 0)) {
            Some(node) => {
                let pseudo = arg(&args, 1).to_js_string();
                computed_style_value(
                    node,
                    matches!(pseudo.as_str(), "::before" | "::after").then_some(pseudo),
                )
            }
            None => Value::object(HashMap::new()),
        }),
    );
    props.insert(
        "requestAnimationFrame".to_string(),
        func(|_, args| {
            let cb = arg(&args, 0);
            let id = NEXT_RAF_ID.with(|c| {
                let id = c.get();
                c.set(id + 1);
                id
            });
            RAF_QUEUE.with(|q| q.borrow_mut().push((id, cb)));
            Value::Number(id as f64)
        }),
    );
    props.insert(
        "cancelAnimationFrame".to_string(),
        func(|_, args| {
            let id = arg(&args, 0).to_u32();
            RAF_QUEUE.with(|q| q.borrow_mut().retain(|(rid, _)| *rid != id));
            Value::Undefined
        }),
    );
    props.insert(
        "setTimeout".to_string(),
        func(|_, args| {
            let cb = arg(&args, 0);
            let ms = arg(&args, 1).to_number().max(0.0) as u64;
            let rest: Vec<Value> = args.iter().skip(2).cloned().collect();
            Value::Number(js_set_timer(cb, ms, rest, false) as f64)
        }),
    );
    props.insert(
        "setInterval".to_string(),
        func(|_, args| {
            let cb = arg(&args, 0);
            let ms = arg(&args, 1).to_number().max(0.0) as u64;
            let rest: Vec<Value> = args.iter().skip(2).cloned().collect();
            Value::Number(js_set_timer(cb, ms, rest, true) as f64)
        }),
    );
    props.insert(
        "clearTimeout".to_string(),
        func(|_, args| {
            js_clear_timer(arg(&args, 0).to_u32());
            Value::Undefined
        }),
    );
    props.insert(
        "clearInterval".to_string(),
        func(|_, args| {
            js_clear_timer(arg(&args, 0).to_u32());
            Value::Undefined
        }),
    );
    props.insert(
        "queueMicrotask".to_string(),
        func(|_, args| {
            queue_microtask_value(arg(&args, 0));
            Value::Undefined
        }),
    );
    props.insert(
        "requestIdleCallback".to_string(),
        func(|_, args| {
            let cb = arg(&args, 0);
            let timeout = arg(&args, 1).get_property("timeout").to_number();
            let timeout = timeout.is_finite().then_some(timeout.max(0.0));
            let scheduled = Instant::now();
            let callback = func(move |_, _| {
                let did_timeout = timeout
                    .is_some_and(|timeout| scheduled.elapsed().as_secs_f64() * 1000.0 >= timeout);
                cb.call(Value::Undefined, vec![idle_deadline_value(did_timeout)])
            });
            Value::Number(js_set_timer(callback, 0, vec![], false) as f64)
        }),
    );
    props.insert(
        "cancelIdleCallback".to_string(),
        func(|_, args| {
            js_clear_timer(arg(&args, 0).to_u32());
            Value::Undefined
        }),
    );
    props.insert(
        "setImmediate".to_string(),
        func(|_, args| {
            let cb = arg(&args, 0);
            Value::Number(js_set_timer(cb, 0, vec![], false) as f64)
        }),
    );
    props.insert(
        "matchMedia".to_string(),
        func(|_, args| match_media_value(&arg(&args, 0).to_js_string())),
    );
    props.insert("getSelection".to_string(), func(|_, _| selection_value()));
    for name in [
        "moveTo", "moveBy", "resizeTo", "resizeBy", "focus", "blur", "print", "close", "stop",
    ] {
        props.insert(
            name.to_string(),
            func(move |_, _| {
                warn_host_api(name, "undefined");
                Value::Undefined
            }),
        );
    }
    for name in ["scrollTo", "scroll", "scrollBy"] {
        props.insert(
            name.to_string(),
            func(move |_, args| {
                let (current_x, current_y) = WINDOW_SCROLL.with(Cell::get);
                let first = arg(&args, 0);
                let (mut x, mut y) = if first.is_object() {
                    (
                        first.get_property("left").to_number(),
                        first.get_property("top").to_number(),
                    )
                } else {
                    (first.to_number(), arg(&args, 1).to_number())
                };
                if name == "scrollBy" {
                    x += current_x;
                    y += current_y;
                }
                WINDOW_SCROLL.with(|offset| offset.set((x, y)));
                if let Some(window) = WINDOW_VALUE.with(|value| value.borrow().clone()) {
                    let event = w3cos_core::class::construct(
                        &crate::web_events::event_class(),
                        vec![Value::string("scroll")],
                    );
                    window
                        .get_property("visualViewport")
                        .call_method("dispatchEvent", vec![event]);
                }
                Value::Undefined
            }),
        );
    }
    props.insert(
        "open".to_string(),
        func(|_, _| {
            let document = document_value()
                .get_property("implementation")
                .call_method("createHTMLDocument", vec![Value::Undefined]);
            document.set_property("URL", Value::string("about:blank"));
            document.set_property("documentURI", Value::string("about:blank"));
            let popup = Value::object(HashMap::from([
                ("document".to_string(), document.clone()),
                ("closed".to_string(), Value::Bool(false)),
                ("opener".to_string(), window_value()),
            ]));
            popup.set_property("window", popup.clone());
            popup.set_property("self", popup.clone());
            document.set_property("defaultView", popup.clone());
            popup
        }),
    );
    props.insert(
        "alert".to_string(),
        func(|_, _| {
            warn_host_api("window.alert()", "undefined");
            Value::Undefined
        }),
    );
    props.insert(
        "confirm".to_string(),
        func(|_, _| {
            warn_host_api("window.confirm()", "false");
            Value::Bool(false)
        }),
    );
    props.insert(
        "prompt".to_string(),
        func(|_, _| {
            warn_host_api("window.prompt()", "null");
            Value::Null
        }),
    );
    props.insert(
        "addEventListener".to_string(),
        func(|_, args| {
            js_add_event_listener(
                0,
                &arg(&args, 0).to_js_string(),
                arg(&args, 1),
                arg(&args, 2),
            );
            Value::Undefined
        }),
    );
    props.insert(
        "removeEventListener".to_string(),
        func(|_, args| {
            js_remove_event_listener(0, &arg(&args, 0).to_js_string());
            Value::Undefined
        }),
    );
    props.insert(
        "dispatchEvent".to_string(),
        func(|_, args| Value::Bool(js_dispatch_event(0, arg(&args, 0)))),
    );
    props.insert(
        "postMessage".to_string(),
        func(|target, args| {
            let data = arg(&args, 0);
            let target_origin = arg(&args, 1).to_js_string();
            let own_origin = target
                .get_property("location")
                .get_property("origin")
                .to_js_string();
            if target_origin == "*" || target_origin == own_origin {
                queue_window_message(target, data);
            }
            Value::Undefined
        }),
    );

    let handler = ProxyBuilder::new()
        .get(|target, key, _| {
            let has_stored_property = target
                .as_object()
                .is_some_and(|object| object.borrow().has_direct(key));
            if has_stored_property {
                target.get_property(key)
            } else {
                window_named_property(key).unwrap_or(Value::Undefined)
            }
        })
        .has(|target, key| {
            target
                .as_object()
                .is_some_and(|object| object.borrow().has_direct(key))
                || window_named_property(key).is_some()
        })
        .build();
    let window = Value::Object(Rc::new(RefCell::new(JsObject::with_proxy(props, handler))));
    w3cos_core::class::set_prototype_of(&window, &window_class().get_property("prototype"));
    window
}

fn promise_constructor_value() -> Value {
    let constructor = func(|_, args| w3cos_core::promise::new(args));
    constructor.set_property(
        "resolve",
        func(|_, args| w3cos_core::promise::resolve(args)),
    );
    constructor.set_property("reject", func(|_, args| w3cos_core::promise::reject(args)));
    constructor.set_property("all", func(|_, args| w3cos_core::promise::all(args)));
    constructor.set_property("race", func(|_, args| w3cos_core::promise::race(args)));
    constructor
}

// ── Timers / microtasks (bridge-side stores; see module docs) ─────────────

fn js_set_timer(callback: Value, ms: u64, args: Vec<Value>, repeating: bool) -> u32 {
    let id = NEXT_TIMER_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    });
    let interval = if repeating {
        Some(Duration::from_millis(ms.max(1)))
    } else {
        None
    };
    JS_TIMERS.with(|t| {
        t.borrow_mut().push(JsTimer {
            id,
            callback,
            args,
            fire_at: Instant::now() + Duration::from_millis(ms),
            interval,
        })
    });
    id
}

pub(crate) fn schedule_timeout_value(callback: Value, delay_ms: u64) -> u32 {
    js_set_timer(callback, delay_ms, Vec::new(), false)
}

fn js_clear_timer(id: u32) {
    JS_TIMERS.with(|t| t.borrow_mut().retain(|timer| timer.id != id));
}

/// Fire all due `setTimeout`/`setInterval` callbacks. Returns the number of
/// callbacks invoked.
pub fn tick_timers() -> usize {
    let mut fired: Vec<(Value, Vec<Value>)> = Vec::new();
    JS_TIMERS.with(|t| {
        let mut timers = t.borrow_mut();
        let now = Instant::now();
        let mut i = 0;
        while i < timers.len() {
            if now >= timers[i].fire_at {
                fired.push((timers[i].callback.clone(), timers[i].args.clone()));
                if let Some(interval) = timers[i].interval {
                    timers[i].fire_at = now + interval;
                    i += 1;
                } else {
                    timers.remove(i);
                }
            } else {
                i += 1;
            }
        }
    });
    let ran = fired.len();
    for (cb, args) in fired {
        cb.call(Value::Undefined, args);
    }
    ran
}

/// Run one animation-frame callback batch at a rendering opportunity.
///
/// Taking the queue before invoking callbacks gives every callback in this
/// batch the same timestamp and defers callbacks registered by a callback to
/// the next frame, matching browser rAF batching semantics.
pub fn run_animation_frame() -> usize {
    tick_css_motions();
    let callbacks: Vec<Value> = RAF_QUEUE
        .with(|q| std::mem::take(&mut *q.borrow_mut()))
        .into_iter()
        .map(|(_, callback)| callback)
        .collect();
    if callbacks.is_empty() {
        return 0;
    }

    let timestamp = Value::Number(performance_now());
    let ran = callbacks.len();
    for callback in callbacks {
        callback.call(Value::Undefined, vec![timestamp.clone()]);
    }
    ran
}

/// True when a rendering opportunity is needed for queued rAF callbacks.
pub fn has_pending_animation_frame() -> bool {
    RAF_QUEUE.with(|q| !q.borrow().is_empty())
        || CSS_MOTIONS.with(|motions| !motions.borrow().is_empty())
}

/// Queue a microtask (also used internally for thenable callbacks).
pub fn queue_microtask_value(callback: Value) {
    w3cos_core::promise::queue_microtask(callback);
}

/// Update the live document readiness state and synchronously dispatch the
/// matching `readystatechange` event when it changes.
pub(crate) fn set_document_ready_state(state: &str) {
    let changed = DOCUMENT_READY_STATE.with(|current| {
        if current.borrow().as_str() == state {
            false
        } else {
            *current.borrow_mut() = state.to_string();
            true
        }
    });
    if changed {
        dispatch_lifecycle_event(&document_value(), "readystatechange");
    }
}

/// Dispatch a document lifecycle event through the same DOM EventTarget path
/// used by authored `dispatchEvent` calls.
pub(crate) fn dispatch_document_lifecycle_event(event_type: &str) {
    dispatch_lifecycle_event(&document_value(), event_type);
}

/// Dispatch a window lifecycle event through the live Window EventTarget.
pub(crate) fn dispatch_window_lifecycle_event(event_type: &str) {
    dispatch_lifecycle_event(&window_value(), event_type);
}

pub(crate) fn dispatch_element_lifecycle_event(node: u32, event_type: &str) {
    dispatch_lifecycle_event(&element_value(node), event_type);
}

pub(crate) fn dispatch_frame_window_lifecycle_event(node: u32, event_type: &str) {
    if let Some(frame_window) = get_expando(node, "contentWindow") {
        dispatch_lifecycle_event(&frame_window, event_type);
    }
}

fn dispatch_lifecycle_event(target: &Value, event_type: &str) {
    let event = w3cos_core::class::construct(
        &crate::web_events::event_class(),
        vec![Value::string(event_type)],
    );
    target.call_method("dispatchEvent", vec![event.clone()]);
    let handler = target.get_property(&format!("on{event_type}"));
    if handler.is_callable() {
        handler.call(target.clone(), vec![event]);
    }
}

fn queue_window_message(target: Value, data: Value) {
    queue_microtask_value(Value::function(move |_, _| {
        let event = w3cos_core::class::construct(
            &crate::web_events::event_subclass_class("MessageEvent"),
            vec![
                Value::string("message"),
                Value::object(HashMap::from([
                    ("data".to_string(), data.clone()),
                    ("origin".to_string(), Value::string("null")),
                    ("source".to_string(), Value::Null),
                    ("ports".to_string(), Value::array(vec![])),
                ])),
            ],
        );
        target.call_method("dispatchEvent", vec![event.clone()]);
        let handler = target.get_property("onmessage");
        if handler.is_callable() {
            handler.call(target.clone(), vec![event]);
        }
        Value::Undefined
    }));
}

/// Drain pending native event snapshots and the microtask queue (repeating
/// until both are empty, since handlers may enqueue more work). Returns the
/// total number of callbacks invoked. The frame loop should call this once
/// per frame. `w3cos_core::promise` reaction jobs are drained on every
/// iteration so promise callbacks interleave with bridge microtasks/events.
pub fn drain_microtasks() -> usize {
    let mut ran = 0;
    loop {
        #[cfg(feature = "dynamic-js")]
        {
            ran += crate::dynamic_script::poll_script_fetches();
        }
        ran += crate::websocket::poll_js_events();
        ran += crate::eventsource::poll_js_events();
        ran += crate::worker_web::poll_js_events();
        ran += crate::speech_web::poll_js_events();
        ran += deliver_pending_events();
        ran += w3cos_core::promise::drain_microtasks();
        let batch: Vec<Value> = MICROTASKS.with(|m| std::mem::take(&mut *m.borrow_mut()));
        if batch.is_empty() {
            let events_left = PENDING_EVENTS.with(|q| !q.borrow().is_empty());
            let promises_left = w3cos_core::promise::queue_count() > 0;
            if !events_left && !promises_left {
                break;
            }
            continue;
        }
        for cb in batch {
            cb.call(Value::Undefined, vec![]);
            ran += 1;
        }
    }
    ran
}

/// Complete immediately runnable framework work before the first DOM snapshot.
///
/// Framework schedulers may defer their initial commit through a zero-delay
/// host task. Native DOM windows take their first component-tree snapshot
/// before the platform event loop starts, so that task must get a bounded
/// bootstrap checkpoint or the first frame contains only the empty mount node.
pub(crate) fn drain_bootstrap_tasks(max_turns: usize) -> usize {
    let mut total = 0;
    for _ in 0..max_turns {
        let ran = drain_microtasks() + tick_timers() + drain_microtasks();
        total += ran;
        if ran == 0 {
            break;
        }
    }
    total
}

/// True when the bridge has work for the frame loop: pending JS timers,
/// rAF callbacks, microtasks, or undelivered native events.
pub fn has_pending_work() -> bool {
    let timers = JS_TIMERS.with(|t| !t.borrow().is_empty());
    let raf = RAF_QUEUE.with(|q| !q.borrow().is_empty());
    let micro =
        MICROTASKS.with(|m| !m.borrow().is_empty()) || w3cos_core::promise::queue_count() > 0;
    let events = PENDING_EVENTS.with(|q| !q.borrow().is_empty());
    let native_io = crate::websocket::has_pending_js_sockets()
        || crate::eventsource::has_pending_js_sources()
        || {
            #[cfg(feature = "dynamic-js")]
            {
                crate::dynamic_script::has_pending_script_fetches()
            }
            #[cfg(not(feature = "dynamic-js"))]
            {
                false
            }
        };
    timers || raf || micro || events || native_io
}

/// Earliest non-rendering deadline the bridge needs to be woken at: the
/// soonest pending JS timer, or a polling opportunity for native WebSockets.
/// rAF cadence is owned by the window rendering scheduler.
pub fn next_timer_deadline() -> Option<Instant> {
    let timer = JS_TIMERS.with(|t| t.borrow().iter().map(|timer| timer.fire_at).min());
    let network_sources =
        crate::websocket::has_pending_js_sockets() || crate::eventsource::has_pending_js_sources();
    let script_deadline = {
        #[cfg(feature = "dynamic-js")]
        {
            crate::dynamic_script::next_script_fetch_deadline()
        }
        #[cfg(not(feature = "dynamic-js"))]
        {
            None
        }
    };
    let socket_deadline = network_sources.then(|| Instant::now() + Duration::from_millis(16));
    [
        timer,
        socket_deadline,
        script_deadline,
        crate::speech::next_deadline(),
    ]
    .into_iter()
    .flatten()
    .min()
}

/// Reset all bridge state. Pair with [`crate::dom::reset_document`] in tests:
/// node ids are recycled by a fresh document, so the element-value memo and
/// every other node-keyed cache must be dropped too.
pub fn reset_bridge() {
    #[cfg(feature = "dynamic-js")]
    crate::dynamic_script::reset_document_loader();
    advance_bridge_realm_generation();
    w3cos_core::promise::advance_realm_generation();
    ELEMENT_VALUES.with(|c| c.borrow_mut().clear());
    ELEMENT_PROPS.with(|c| c.borrow_mut().clear());
    PROCESSING_INSTRUCTION_ATTRIBUTES.with(|cache| cache.borrow_mut().clear());
    ATTRIBUTE_VALUES.with(|cache| cache.borrow_mut().clear());
    SELECTOR_ID_CACHE.with(|cache| cache.borrow_mut().clear());
    SELECTOR_ID_CACHE_GENERATION.with(|generation| generation.set(0));
    STYLE_CACHE.with(|c| c.borrow_mut().clear());
    CSS_MOTIONS.with(|motions| motions.borrow_mut().clear());
    LISTENERS.with(|l| l.borrow_mut().clear());
    NATIVELY_REGISTERED.with(|r| r.borrow_mut().clear());
    PENDING_EVENTS.with(|q| q.borrow_mut().clear());
    ACTIVE_TOUCHES.with(|touches| touches.borrow_mut().clear());
    ACTIVE_POINTERS.with(|pointers| pointers.borrow_mut().clear());
    POINTER_CAPTURE.with(|capture| capture.borrow_mut().clear());
    SHADOW_ROOTS.with(|roots| roots.borrow_mut().clear());
    CUSTOM_EVENT_TYPES.with(|m| m.borrow_mut().clear());
    CUSTOM_EVENT_NAMES.with(|m| m.borrow_mut().clear());
    MICROTASKS.with(|m| m.borrow_mut().clear());
    JS_TIMERS.with(|t| t.borrow_mut().clear());
    NEXT_TIMER_ID.with(|c| c.set(1));
    RAF_QUEUE.with(|q| q.borrow_mut().clear());
    NEXT_RAF_ID.with(|c| c.set(1));
    VIEWPORT.with(|v| v.set((1024.0, 768.0, 1.0)));
    WINDOW_SCROLL.with(|offset| offset.set((0.0, 0.0)));
    FULLSCREEN_NODE.with(|node| *node.borrow_mut() = None);
    DOCUMENT_VISIBILITY.with(|state| *state.borrow_mut() = "visible".to_string());
    DOCUMENT_READY_STATE.with(|state| *state.borrow_mut() = "complete".to_string());
    EVENT_COUNTS.with(|counts| *counts.borrow_mut() = initial_event_counts());
    MEDIA_QUERY_LISTS.with(|lists| lists.borrow_mut().clear());
    AUTHOR_STYLE_SHEETS.with(|sheets| sheets.borrow_mut().clear());
    LIVE_RANGES.with(|ranges| ranges.borrow_mut().clear());
    ACTIVE_ELEMENT.with(|a| *a.borrow_mut() = None);
    HTML_ID.with(|h| *h.borrow_mut() = None);
    HEAD_ID.with(|h| *h.borrow_mut() = None);
    CANVAS_CONTEXTS.with(|c| c.borrow_mut().clear());
    SESSION_STORAGE.with(|s| s.borrow_mut().clear());
    crate::websocket::reset_js_websockets();
    crate::eventsource::reset_js_event_sources();
    crate::xhr::reset_realm();
    crate::clipboard_web::reset_realm();
    crate::credentials_web::reset_realm();
    crate::permissions_web::reset_realm();
    crate::close_watcher_web::reset_realm();
    crate::network_information_web::reset_realm();
    crate::wake_lock_web::reset_realm();
    crate::storage_manager_web::reset_realm();
    crate::storage_buckets_web::reset_realm();
    crate::pressure_web::reset_realm();
    crate::presentation_web::reset_realm();
    crate::barcode_detection_web::reset_realm();
    crate::notification_web::reset_realm();
    crate::edit_context_web::reset_realm();
    crate::user_mediated_web::reset_realm();
    crate::observable_web::reset_realm();
    crate::worker_web::reset_realm();
    crate::canvas_web::reset_realm();
    crate::xpath_web::reset_realm();
    crate::sanitizer_web::reset_realm();
    crate::bluetooth_web::reset_realm();
    crate::speech_web::reset();
    crate::speech_synthesis_web::reset();
    crate::geolocation_web::reset();
    #[cfg(feature = "web-media-advanced")]
    {
        crate::media_devices_web::reset();
        crate::media_session_web::reset();
        crate::media_capabilities_web::reset();
        crate::media_recording_web::reset();
        crate::media_source_web::reset();
    }
    crate::payment_web::reset();
    #[cfg(feature = "web-graphics-advanced")]
    crate::webcodecs_web::reset();
    crate::navigator_web::reset();
    crate::midi_web::reset();
    crate::service_worker_web::reset();
    crate::push_web::reset();
    crate::xslt_web::reset();
    crate::battery_web::reset();
    crate::gamepad_web::reset();
    crate::orientation_web::reset();
    crate::sensors_web::reset();
    crate::font_loading_web::reset();
    crate::custom_elements_web::reset();
    crate::highlight_web::reset();
    crate::cache_web::reset();
    crate::locks_web::reset();
    crate::scheduler_web::reset();
    crate::reporting_web::reset();
    crate::compat_web::reset();
    crate::text_tracks_web::reset();
    crate::file_system_web::reset();
    crate::animations_web::reset();
    #[cfg(feature = "web-media-advanced")]
    crate::audio_web::reset();
    #[cfg(feature = "web-graphics-advanced")]
    crate::image_decoder_web::reset();
    #[cfg(feature = "web-media-advanced")]
    crate::webrtc_web::reset();
    crate::experimental_web::reset();
    crate::web_transport_web::reset();
    #[cfg(feature = "web-graphics-advanced")]
    {
        crate::webxr_web::reset();
        crate::webgpu_web::reset();
        crate::webgl_web::reset();
    }
    crate::cookie_store_web::reset_document_context();
    crate::trusted_types_web::reset();
    crate::user_activation_web::reset();
    crate::launch_handler_web::reset_realm();
    crate::navigation_web::reset_realm();
    crate::screen_details_web::reset_realm();
    crate::window_environment_web::reset_realm();
    crate::fragment_directive_web::reset_realm();
    crate::view_transition_web::reset_realm();
    crate::observers_web::reset_resize_observers();
    crate::observers_web::reset_mutation_observers();
    crate::observers_web::reset_intersection_observers();
    crate::observers_web::reset_performance_timeline();
    crate::fetch::reset_realm();
    crate::streams_web::reset_realm();
    crate::web_events::reset_realm();
    reset_realm_class_caches();
    // Release globals last: subsystem reset hooks may still need to inspect
    // their old Realm-owned values while tearing down native resources.
    WINDOW_VALUE.with(|value| {
        value.borrow_mut().take();
    });
    DOCUMENT_VALUE.with(|value| {
        value.borrow_mut().take();
    });
    SELECTION_VALUE.with(|value| {
        value.borrow_mut().take();
    });
    w3cos_core::page_arena::reset();
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn setup() {
        dom::reset_document();
        reset_bridge();
        crate::history::reset();
    }

    fn create_in_body(tag: &str) -> Value {
        let doc = document_value();
        let el = doc.call_method("createElement", vec![Value::string(tag)]);
        doc.get_property("body")
            .call_method("appendChild", vec![el.clone()]);
        el
    }

    fn wrapper_weak(value: &Value) -> WeakRealmObject {
        weak_realm_object(value)
    }

    #[test]
    fn create_element_cache_does_not_pin_unreferenced_wrapper() {
        setup();
        let el = document_value().call_method("createElement", vec![Value::string("div")]);
        let weak = wrapper_weak(&el);
        drop(el);
        assert!(
            weak.upgrade().is_none(),
            "detached createElement wrapper must drop without reset_bridge"
        );
    }

    #[test]
    fn css_transition_exposes_midpoint_event_and_web_animation_facade() {
        setup();
        w3cos_dom::stylesheet::clear_rules();
        w3cos_dom::stylesheet::register_rule(
            "#item",
            &[
                ("left", "0px"),
                ("transition", "left 60s steps(1, jump-both)"),
            ],
        );
        let item = create_in_body("div");
        item.set_property("id", Value::string("item"));
        let starts = Rc::new(Cell::new(0_u32));
        let starts_for_listener = Rc::clone(&starts);
        item.call_method(
            "addEventListener",
            vec![
                Value::string("transitionstart"),
                func(move |_, _| {
                    starts_for_listener.set(starts_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );

        item.get_property("style")
            .set_property("left", Value::string("400px"));
        run_animation_frame();

        assert_eq!(starts.get(), 1);
        assert_eq!(
            window_value()
                .call_method("getComputedStyle", vec![item.clone()])
                .get_property("left")
                .to_js_string(),
            "200px"
        );
        assert_eq!(
            item.call_method("getAnimations", Vec::new())
                .get_property("length")
                .to_u32(),
            1
        );
        assert!(
            item.get_property("style")
                .as_object()
                .is_some_and(|style| style.borrow().has("transform"))
        );
    }

    #[test]
    fn remove_child_drops_unreferenced_wrapper() {
        setup();
        let el = create_in_body("div");
        let weak = wrapper_weak(&el);
        document_value()
            .get_property("body")
            .call_method("removeChild", vec![el.clone()]);
        drop(el);
        assert!(
            weak.upgrade().is_none(),
            "removed node wrapper must drop when JS does not hold it"
        );
    }

    #[test]
    fn remove_child_keeps_identity_while_js_holds_wrapper() {
        setup();
        let el = create_in_body("div");
        document_value()
            .get_property("body")
            .call_method("removeChild", vec![el.clone()]);
        let id = node_id_of(&el).expect("node id");
        let again = element_value(id);
        assert!(
            el.as_object()
                .zip(again.as_object())
                .is_some_and(|(a, b)| a.ptr_eq(&b)),
            "held detached wrapper must keep === identity"
        );
    }

    #[test]
    fn page_arena_drops_interned_strings_on_reset_bridge() {
        setup();
        let unique = "page-arena-nav-key";
        let interned = w3cos_core::JsString::intern(unique);
        assert!(interned.page_handle().is_some());
        assert_eq!(interned.heap_strong_count(), None);
        let cloned = interned.clone();
        assert!(interned.ptr_eq(&cloned));
        assert!(w3cos_core::page_arena::live_handles() >= 1);
        assert!(w3cos_core::page_arena::allocated_bytes() >= unique.len());
        reset_bridge();
        assert_eq!(w3cos_core::page_arena::live_handles(), 0);
        assert_eq!(w3cos_core::page_arena::allocated_bytes(), 0);
    }

    #[test]
    fn subtle_crypto_digest_supports_sha256_array_buffer_views() {
        setup();
        let subtle = window_value().get_property("crypto").get_property("subtle");
        let input = w3cos_core::binary::typed_array_value(
            b"hello"
                .iter()
                .map(|byte| Value::Number(*byte as f64))
                .collect(),
        );
        let digest = subtle.call_method("digest", vec![Value::string("SHA-256"), input]);
        let Some(w3cos_core::promise::PromiseStatus::Fulfilled(buffer)) =
            w3cos_core::promise::status(&digest)
        else {
            panic!("digest promise did not fulfill");
        };
        assert_eq!(
            w3cos_core::binary::bytes_of(&buffer).unwrap(),
            vec![
                0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9,
                0xe2, 0x9e, 0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62,
                0x93, 0x8b, 0x98, 0x24,
            ]
        );
    }

    #[test]
    fn trusted_native_input_updates_user_activation_during_dispatch() {
        setup();
        let target = create_in_body("button");
        let target_id = node_id_of(&target).expect("button node id");
        let activation = window_value()
            .get_property("navigator")
            .get_property("userActivation");
        let observed = Rc::new(RefCell::new(Vec::<bool>::new()));
        let observed_for_listener = Rc::clone(&observed);
        let activation_for_listener = activation.clone();
        target.call_method(
            "addEventListener",
            vec![
                Value::string("keydown"),
                func(move |_, args| {
                    assert!(args[0].get_property("isTrusted").to_bool());
                    observed_for_listener
                        .borrow_mut()
                        .push(activation_for_listener.get_property("isActive").to_bool());
                    Value::Undefined
                }),
            ],
        );

        assert!(!activation.get_property("isActive").to_bool());
        assert!(!activation.get_property("hasBeenActive").to_bool());
        assert!(!dispatch_native_key(
            target_id, "Enter", "Enter", false, false, false, false, false, true,
        ));
        assert_eq!(&*observed.borrow(), &[true]);
        assert!(!activation.get_property("isActive").to_bool());
        assert!(activation.get_property("hasBeenActive").to_bool());
    }

    #[test]
    fn dom_values_have_browser_shaped_constructor_identity() {
        setup();
        let window = window_value();
        let document = document_value();
        let div = document.call_method("createElement", vec![Value::string("div")]);
        let input = document.call_method("createElement", vec![Value::string("input")]);
        let svg = document.call_method(
            "createElementNS",
            vec![
                Value::string("http://www.w3.org/2000/svg"),
                Value::string("svg"),
            ],
        );
        let fragment = document.call_method("createDocumentFragment", vec![]);
        let text = document.call_method("createTextNode", vec![Value::string("text")]);
        let range = document.call_method("createRange", vec![]);
        let selection = document.call_method("getSelection", vec![]);

        for constructor in ["HTMLDivElement", "HTMLElement", "Element", "Node"] {
            assert!(w3cos_core::class::instance_of(
                &div,
                &window.get_property(constructor)
            ));
        }
        assert!(!w3cos_core::class::instance_of(
            &div,
            &window.get_property("HTMLSpanElement")
        ));
        assert!(w3cos_core::class::instance_of(
            &input,
            &window.get_property("HTMLInputElement")
        ));
        assert!(w3cos_core::class::instance_of(
            &svg,
            &window.get_property("SVGElement")
        ));
        assert!(w3cos_core::class::instance_of(
            &svg,
            &window.get_property("Element")
        ));
        assert!(!w3cos_core::class::instance_of(
            &svg,
            &window.get_property("HTMLElement")
        ));
        assert!(w3cos_core::class::instance_of(
            &fragment,
            &window.get_property("DocumentFragment")
        ));
        assert!(w3cos_core::class::instance_of(
            &fragment,
            &window.get_property("Node")
        ));
        assert!(w3cos_core::class::instance_of(
            &text,
            &window.get_property("Node")
        ));
        assert!(w3cos_core::class::instance_of(
            &range,
            &window.get_property("Range")
        ));
        assert!(w3cos_core::class::instance_of(
            &selection,
            &window.get_property("Selection")
        ));

        let constructed_range = w3cos_core::class::construct(&window.get_property("Range"), vec![]);
        assert!(constructed_range.get_property("setStart").is_function());
        assert!(w3cos_core::class::instance_of(
            &constructed_range,
            &window.get_property("Range")
        ));
    }

    #[test]
    fn xml_node_factories_preserve_node_shape_and_serialization() {
        setup();
        let window = window_value();
        let implementation = document_value().get_property("implementation");
        assert!(w3cos_core::class::instance_of(
            &implementation,
            &window.get_property("DOMImplementation")
        ));

        let xml = implementation.call_method(
            "createDocument",
            vec![
                Value::string("urn:w3cos:test"),
                Value::string("root"),
                Value::Null,
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &xml,
            &window.get_property("XMLDocument")
        ));
        assert!(w3cos_core::class::instance_of(
            &document_value(),
            &window.get_property("HTMLDocument")
        ));
        let cdata = xml.call_method("createCDATASection", vec![Value::string("a < b")]);
        let instruction = xml.call_method(
            "createProcessingInstruction",
            vec![
                Value::string("xml-stylesheet"),
                Value::string("href=\"theme.css\""),
            ],
        );
        let root = xml.get_property("documentElement");
        root.call_method("appendChild", vec![cdata.clone()]);
        root.call_method("appendChild", vec![instruction.clone()]);

        assert_eq!(cdata.get_property("nodeType").to_number(), 4.0);
        assert_eq!(
            cdata.get_property("nodeName").to_js_string(),
            "#cdata-section"
        );
        assert_eq!(instruction.get_property("nodeType").to_number(), 7.0);
        assert_eq!(
            instruction.get_property("target").to_js_string(),
            "xml-stylesheet"
        );
        assert!(w3cos_core::class::instance_of(
            &cdata,
            &window.get_property("CDATASection")
        ));
        assert!(w3cos_core::class::instance_of(
            &instruction,
            &window.get_property("ProcessingInstruction")
        ));
        assert_eq!(
            root.get_property("outerHTML").to_js_string(),
            "<root><![CDATA[a < b]]><?xml-stylesheet href=\"theme.css\"?></root>"
        );

        let doctype = implementation.call_method(
            "createDocumentType",
            vec![
                Value::string("html"),
                Value::string("-//W3C//DTD HTML 5.0//EN"),
                Value::string("about:legacy-compat"),
            ],
        );
        assert_eq!(doctype.get_property("nodeType").to_number(), 10.0);
        assert_eq!(doctype.get_property("name").to_js_string(), "html");
        assert_eq!(
            doctype.get_property("publicId").to_js_string(),
            "-//W3C//DTD HTML 5.0//EN"
        );
        assert!(w3cos_core::class::instance_of(
            &doctype,
            &window.get_property("DocumentType")
        ));
    }

    #[test]
    fn dom_implementation_documents_keep_their_metadata_and_owner_document() {
        setup();
        let implementation = document_value().get_property("implementation");
        let document =
            implementation.call_method("createHTMLDocument", vec![Value::string("W3COS title")]);

        assert_eq!(
            document.get_property("compatMode"),
            Value::string("CSS1Compat")
        );
        assert_eq!(
            document.get_property("characterSet"),
            Value::string("UTF-8")
        );
        assert_eq!(document.get_property("charset"), Value::string("UTF-8"));
        assert_eq!(
            document.get_property("inputEncoding"),
            Value::string("UTF-8")
        );
        assert!(document.get_property("location").is_null());
        assert_eq!(
            document
                .get_property("childNodes")
                .get_property("length")
                .to_u32(),
            2
        );

        let doctype = document.get_property("doctype");
        assert_eq!(doctype.get_property("name"), Value::string("html"));
        assert!(doctype.get_property("ownerDocument") == document);

        let div = document.call_method("createElement", vec![Value::string("DIV")]);
        assert_eq!(div.get_property("localName"), Value::string("div"));
        assert!(div.get_property("ownerDocument") == document);

        let anchor = document.call_method("createElement", vec![Value::string("a")]);
        anchor.set_property("href", Value::string("http://example.org/?ä"));
        assert_eq!(
            anchor.get_property("href"),
            Value::string("http://example.org/?%C3%A4")
        );
    }

    #[test]
    fn virtual_document_replace_children_validates_and_updates_the_live_child_list() {
        setup();
        let implementation = document_value().get_property("implementation");
        let document = implementation.call_method("createHTMLDocument", vec![Value::Undefined]);
        let anchor = document.call_method("createElement", vec![Value::string("a")]);
        document.call_method("replaceChildren", vec![anchor.clone()]);
        assert_eq!(
            document
                .get_property("childNodes")
                .get_property("length")
                .to_u32(),
            1
        );
        assert!(document.get_property("childNodes").get_property("0") == anchor);
        assert!(document.get_property("documentElement") == anchor);

        let fragment = document.call_method("createDocumentFragment", vec![]);
        let single = document.call_method("createElement", vec![Value::string("main")]);
        fragment.call_method("appendChild", vec![single.clone()]);
        document.call_method("replaceChildren", vec![fragment]);
        assert_eq!(
            document
                .get_property("childNodes")
                .get_property("length")
                .to_u32(),
            1
        );
        assert!(document.get_property("childNodes").get_property("0") == single);

        let invalid_fragment = document.call_method("createDocumentFragment", vec![]);
        invalid_fragment.call_method(
            "appendChild",
            vec![document.call_method("createElement", vec![Value::string("a")])],
        );
        invalid_fragment.call_method(
            "appendChild",
            vec![document.call_method("createElement", vec![Value::string("b")])],
        );
        for invalid in [
            invalid_fragment,
            document.call_method("createTextNode", vec![Value::string("text")]),
            implementation.call_method("createHTMLDocument", vec![Value::Undefined]),
        ] {
            let error = w3cos_core::catch_js(|| {
                document.call_method("replaceChildren", vec![invalid.clone()])
            })
            .expect_err("invalid document replacement must throw");
            assert_eq!(
                error.get_property("name").to_js_string(),
                "HierarchyRequestError"
            );
        }

        document.call_method("replaceChildren", vec![]);
        assert_eq!(
            document
                .get_property("childNodes")
                .get_property("length")
                .to_u32(),
            0
        );
        assert!(document.get_property("documentElement").is_null());
    }

    #[test]
    fn insert_adjacent_rejects_siblings_of_a_virtual_document_element() {
        setup();
        let implementation = document_value().get_property("implementation");
        let created_document =
            implementation.call_method("createHTMLDocument", vec![Value::Undefined]);
        let root = created_document.get_property("documentElement");
        let child = document_value().call_method("createElement", vec![Value::string("aside")]);

        for (method, value) in [
            ("insertAdjacentElement", child),
            ("insertAdjacentText", Value::string("text")),
        ] {
            let error = w3cos_core::catch_js(|| {
                root.call_method(method, vec![Value::string("beforebegin"), value])
            })
            .expect_err("a document element cannot gain an element sibling");
            assert_eq!(
                error.get_property("name").to_js_string(),
                "HierarchyRequestError"
            );
        }
    }

    #[test]
    fn blank_popup_document_adopts_a_moved_subtree() {
        setup();
        let document = document_value();
        let adopted = create_in_body("div");
        adopted.call_method(
            "setAttribute",
            vec![Value::string("data-route"), Value::string("inbox")],
        );
        let adopted_attribute = adopted
            .get_property("attributes")
            .get_property("0");
        assert!(adopted.get_property("ownerDocument") == document);
        assert!(adopted_attribute.get_property("ownerDocument") == document);

        let popup = window_value().call_method("open", vec![]);
        let popup_document = popup.get_property("document");
        popup_document
            .get_property("body")
            .call_method("appendChild", vec![document.get_property("body")]);

        assert!(adopted.get_property("ownerDocument") == popup_document);
        assert!(adopted_attribute.get_property("ownerDocument") == popup_document);
        assert!(popup_document.get_property("defaultView") == popup);
    }

    #[test]
    fn document_type_accepts_legacy_names_and_binds_to_its_document() {
        setup();
        let global_document = document_value();
        let global_implementation = global_document.get_property("implementation");
        for name in ["", "1foo", "@foo", "edi:<", "prefix::local"] {
            let doctype = global_implementation.call_method(
                "createDocumentType",
                vec![
                    Value::string(name),
                    Value::string("public"),
                    Value::string("system"),
                ],
            );
            assert_eq!(doctype.get_property("name"), Value::string(name));
            assert!(doctype.get_property("ownerDocument") == global_document);
        }

        let created_document =
            global_implementation.call_method("createHTMLDocument", vec![Value::Undefined]);
        let implementation = created_document.get_property("implementation");
        let doctype = implementation.call_method(
            "createDocumentType",
            vec![Value::string("html"), Value::string(""), Value::string("")],
        );
        assert!(doctype.get_property("ownerDocument") == created_document);
    }

    #[test]
    fn document_type_rejects_child_insertion() {
        setup();
        let document = document_value();
        let doctype = document.get_property("implementation").call_method(
            "createDocumentType",
            vec![
                Value::string("html"),
                Value::string(""),
                Value::string(""),
            ],
        );
        let text = document.call_method("createTextNode", vec![Value::string("invalid")]);
        let error = w3cos_core::catch_js(|| doctype.call_method("appendChild", vec![text]))
            .expect_err("document types cannot contain children");
        assert_eq!(
            error.get_property("name").to_js_string(),
            "HierarchyRequestError"
        );
    }

    #[test]
    fn insert_before_validates_web_idl_parent_ancestry_and_reference_in_order() {
        setup();
        let document = document_value();
        let leaf = document.call_method("createTextNode", vec![Value::string("leaf")]);
        let type_error = w3cos_core::catch_js(|| {
            leaf.call_method("insertBefore", vec![Value::Null, Value::Null])
        })
        .expect_err("the first argument must be a Node before parent validation");
        assert_eq!(type_error.get_property("name").to_js_string(), "TypeError");

        let child = document.call_method("createTextNode", vec![Value::string("child")]);
        let hierarchy_error = w3cos_core::catch_js(|| {
            leaf.call_method("insertBefore", vec![child, Value::Null])
        })
        .expect_err("character data cannot contain children");
        assert_eq!(
            hierarchy_error.get_property("name").to_js_string(),
            "HierarchyRequestError"
        );

        let parent = document.call_method("createElement", vec![Value::string("div")]);
        let foreign_reference =
            document.call_method("createElement", vec![Value::string("span")]);
        let doctype = document.get_property("implementation").call_method(
            "createDocumentType",
            vec![Value::string("html"), Value::string(""), Value::string("")],
        );
        let not_found = w3cos_core::catch_js(|| {
            parent.call_method("insertBefore", vec![doctype, foreign_reference])
        })
        .expect_err("reference membership precedes inserted child position checks");
        assert_eq!(not_found.get_property("name").to_js_string(), "NotFoundError");

        let insert_before = crate::dom_constructors::prototype("Node")
            .get_property("insertBefore");
        assert_eq!(insert_before.get_property("length"), Value::Number(2.0));
        let prototype_not_found = w3cos_core::catch_js(|| {
            insert_before.call(
                parent.clone(),
                vec![
                    document.call_method("createElement", vec![Value::string("em")]),
                    document.call_method("createElement", vec![Value::string("strong")]),
                ],
            )
        })
        .expect_err("Node.prototype.insertBefore must preserve its receiver");
        assert_eq!(
            prototype_not_found.get_property("name").to_js_string(),
            "NotFoundError"
        );

        let created_document = document.get_property("implementation").call_method(
            "createHTMLDocument",
            vec![Value::string("prototype dispatch")],
        );
        assert_eq!(
            created_document
                .get_property("insertBefore")
                .get_property("length"),
            Value::Number(2.0)
        );
        let document_hierarchy_error = w3cos_core::catch_js(|| {
            insert_before.call(
                created_document.clone(),
                vec![
                    created_document.call_method(
                        "createTextNode",
                        vec![Value::string("invalid")],
                    ),
                    Value::Null,
                ],
            )
        })
        .expect_err("Node.prototype.insertBefore must dispatch virtual documents");
        assert_eq!(
            document_hierarchy_error
                .get_property("name")
                .to_js_string(),
            "HierarchyRequestError"
        );

        let foreign_document = document.get_property("implementation").call_method(
            "createHTMLDocument",
            vec![Value::string("foreign")],
        );
        let document_reference_error = w3cos_core::catch_js(|| {
            insert_before.call(
                parent,
                vec![
                    foreign_document,
                    document.call_method("createElement", vec![Value::string("aside")]),
                ],
            )
        })
        .expect_err("reference membership precedes document child type validation");
        assert_eq!(
            document_reference_error.get_property("name").to_js_string(),
            "NotFoundError"
        );
    }

    #[test]
    fn character_data_constructors_treat_missing_or_undefined_data_as_empty() {
        setup();
        for value in [text_value(vec![]), text_value(vec![Value::Undefined])]
            .into_iter()
            .chain([comment_value(vec![]), comment_value(vec![Value::Undefined])])
        {
            assert_eq!(value.get_property("data"), Value::string(""));
            assert_eq!(value.get_property("nodeValue"), Value::string(""));
        }
    }

    #[test]
    fn document_and_document_fragment_constructors_create_live_nodes() {
        setup();
        let window = window_value();
        let document = w3cos_core::class::construct(&window.get_property("Document"), vec![]);
        assert!(w3cos_core::class::instance_of(
            &document,
            &window.get_property("Document")
        ));
        assert!(!w3cos_core::class::instance_of(
            &document,
            &window.get_property("XMLDocument")
        ));
        assert!(document.get_property("documentElement").is_null());
        assert_eq!(
            document.get_property("contentType"),
            Value::string("application/xml")
        );
        assert_eq!(
            document
                .get_property("childNodes")
                .get_property("length")
                .to_u32(),
            0
        );
        let cdata =
            document.call_method("createCDATASection", vec![Value::string("character data")]);
        assert_eq!(cdata.get_property("nodeType").to_u32(), 4);
        assert!(cdata.get_property("ownerDocument") == document);
        let instruction = document.call_method(
            "createProcessingInstruction",
            vec![Value::string("target"), Value::string("data")],
        );
        assert_eq!(instruction.get_property("nodeType").to_u32(), 7);
        assert!(instruction.get_property("ownerDocument") == document);

        let fragment =
            w3cos_core::class::construct(&window.get_property("DocumentFragment"), vec![]);
        let text = document.call_method("createTextNode", vec![Value::string("text")]);
        fragment.call_method("appendChild", vec![text.clone()]);
        assert!(fragment.get_property("firstChild") == text);
        assert!(fragment.get_property("ownerDocument") == document_value());
    }

    #[test]
    fn html_script_element_reflects_loading_properties_without_dynamic_runtime() {
        setup();
        let document = document_value();
        let script = document.call_method("createElement", vec![Value::string("script")]);

        assert_eq!(script.get_property("async"), Value::Bool(true));
        script.set_property("src", Value::string("/chunk.js"));
        script.set_property("type", Value::string("module"));
        script.set_property("crossOrigin", Value::string("anonymous"));
        script.set_property("integrity", Value::string("sha256-example"));
        script.set_property("referrerPolicy", Value::string("no-referrer"));
        script.set_property("async", Value::Bool(false));
        script.set_property("defer", Value::Bool(true));
        script.set_property("noModule", Value::Bool(true));
        script.set_property("text", Value::string("export {};"));

        assert_eq!(script.get_property("src"), Value::string("/chunk.js"));
        assert_eq!(script.get_property("type"), Value::string("module"));
        assert_eq!(
            script.get_property("crossOrigin"),
            Value::string("anonymous")
        );
        assert_eq!(
            script.get_property("integrity"),
            Value::string("sha256-example")
        );
        assert_eq!(
            script.get_property("referrerPolicy"),
            Value::string("no-referrer")
        );
        assert_eq!(script.get_property("async"), Value::Bool(false));
        assert_eq!(script.get_property("defer"), Value::Bool(true));
        assert_eq!(script.get_property("noModule"), Value::Bool(true));
        assert_eq!(script.get_property("text"), Value::string("export {};"));
        assert_eq!(
            script.call_method("hasAttribute", vec![Value::string("async")]),
            Value::Bool(false)
        );
        assert_eq!(
            script.call_method("hasAttribute", vec![Value::string("defer")]),
            Value::Bool(true)
        );
        script.call_method(
            "setAttribute",
            vec![Value::string("async"), Value::string("")],
        );
        assert_eq!(script.get_property("async"), Value::Bool(true));
        script.call_method("removeAttribute", vec![Value::string("async")]);
        assert_eq!(script.get_property("async"), Value::Bool(false));
    }

    #[test]
    fn legacy_element_factories_return_live_typed_html_elements() {
        setup();
        let window = window_value();
        let image = w3cos_core::class::construct(
            &window.get_property("Image"),
            vec![Value::Number(320.0), Value::Number(180.0)],
        );
        assert!(w3cos_core::class::instance_of(
            &image,
            &window.get_property("HTMLImageElement")
        ));
        assert_eq!(image.get_property("width").to_number(), 320.0);
        assert_eq!(image.get_property("height").to_number(), 180.0);
        image.set_property("srcset", Value::string("small.png 1x, large.png 2x"));
        image.set_property("sizes", Value::string("50vw"));
        assert_eq!(
            image.get_property("srcset").to_js_string(),
            "small.png 1x, large.png 2x"
        );
        assert_eq!(image.get_property("sizes").to_js_string(), "50vw");
        assert_eq!(
            image.call_method("getAttribute", vec![Value::string("srcset")]),
            Value::string("small.png 1x, large.png 2x")
        );

        let source = document_value().call_method("createElement", vec![Value::string("source")]);
        source.set_property("srcset", Value::string("wide.webp 2x"));
        source.set_property("media", Value::string("(min-width: 700px)"));
        source.set_property("type", Value::string("image/webp"));
        assert_eq!(source.get_property("srcset").to_js_string(), "wide.webp 2x");
        assert_eq!(
            source.get_property("media").to_js_string(),
            "(min-width: 700px)"
        );
        assert_eq!(source.get_property("type").to_js_string(), "image/webp");

        let audio = w3cos_core::class::construct(
            &window.get_property("Audio"),
            vec![Value::string("tone.ogg")],
        );
        assert!(w3cos_core::class::instance_of(
            &audio,
            &window.get_property("HTMLAudioElement")
        ));
        assert_eq!(audio.get_property("src").to_js_string(), "tone.ogg");

        let option = w3cos_core::class::construct(
            &window.get_property("Option"),
            vec![
                Value::string("Fast"),
                Value::string("fast"),
                Value::Bool(true),
                Value::Bool(false),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &option,
            &window.get_property("HTMLOptionElement")
        ));
        assert_eq!(option.get_property("text").to_js_string(), "Fast");
        assert_eq!(option.get_property("value").to_js_string(), "fast");
        assert!(option.get_property("defaultSelected").to_bool());
        assert!(!option.get_property("selected").to_bool());

        for name in [
            "locationbar",
            "menubar",
            "personalbar",
            "scrollbars",
            "statusbar",
            "toolbar",
        ] {
            let bar = window.get_property(name);
            assert!(w3cos_core::class::instance_of(
                &bar,
                &window.get_property("BarProp")
            ));
            assert!(!bar.get_property("visible").to_bool());
        }
    }

    #[test]
    fn inserting_an_empty_source_synchronously_sets_media_network_state() {
        setup();
        let document = document_value();
        let video = document.call_method("createElement", vec![Value::string("video")]);
        let source = document.call_method("createElement", vec![Value::string("source")]);

        assert_eq!(video.get_property("networkState").to_u32(), 0);
        video.call_method("appendChild", vec![source]);
        assert_eq!(video.get_property("networkState").to_u32(), 3);
    }

    #[test]
    fn shadow_roots_are_queryable_connected_and_obey_open_closed_modes() {
        setup();
        let window = window_value();
        let document = document_value();
        let host = create_in_body("div");
        let root = host.call_method(
            "attachShadow",
            vec![Value::object(HashMap::from([
                ("mode".to_string(), Value::string("open")),
                ("delegatesFocus".to_string(), Value::Bool(true)),
            ]))],
        );

        assert!(host.get_property("shadowRoot") == root);
        assert!(root.get_property("host") == host);
        assert_eq!(root.get_property("mode").to_js_string(), "open");
        assert!(root.get_property("delegatesFocus").to_bool());
        assert!(w3cos_core::class::instance_of(
            &root,
            &window.get_property("ShadowRoot")
        ));
        assert!(w3cos_core::class::instance_of(
            &root,
            &window.get_property("DocumentFragment")
        ));

        root.set_property(
            "innerHTML",
            Value::string("<button id=\"inside\">Go</button>"),
        );
        let button = root.call_method("querySelector", vec![Value::string("button")]);
        assert_eq!(button.get_property("textContent").to_js_string(), "Go");
        assert!(button.call_method("getRootNode", vec![]) == root);
        assert!(
            button.call_method(
                "getRootNode",
                vec![Value::object(HashMap::from([(
                    "composed".to_string(),
                    Value::Bool(true),
                )]))],
            ) == document
        );
        assert!(button.get_property("isConnected").to_bool());

        let closed_host = create_in_body("section");
        let closed_root = closed_host.call_method(
            "attachShadow",
            vec![Value::object(HashMap::from([(
                "mode".to_string(),
                Value::string("closed"),
            )]))],
        );
        assert!(closed_host.get_property("shadowRoot").is_null());
        assert_eq!(closed_root.get_property("mode").to_js_string(), "closed");
    }

    #[test]
    fn window_named_properties_and_document_name_lists_follow_the_document_tree() {
        setup();
        let window = window_value();
        let document = document_value();
        let image = document.call_method("createElement", vec![Value::string("img")]);
        image.set_property("name", Value::string("target"));
        document
            .get_property("body")
            .call_method("appendChild", vec![image.clone()]);

        let list = document.call_method("getElementsByName", vec![Value::string("target")]);
        assert!(window.get_property("target") == image);
        assert_eq!(list.get_property("length").to_u32(), 1);

        let host = create_in_body("div");
        let root = host.call_method(
            "attachShadow",
            vec![Value::object(HashMap::from([(
                "mode".to_string(),
                Value::string("open"),
            )]))],
        );
        root.call_method("appendChild", vec![image]);

        assert!(window.get_property("target").is_undefined());
        assert_eq!(list.get_property("length").to_u32(), 0);
    }

    #[test]
    fn live_node_list_iterator_observes_nodes_appended_after_iteration_starts() {
        setup();
        let document = document_value();
        let parent = document.call_method("createElement", vec![Value::string("div")]);
        let first = document.call_method("createElement", vec![Value::string("span")]);
        parent.call_method("appendChild", vec![first.clone()]);
        let list = parent.get_property("childNodes");
        assert!(
            list.as_object()
                .is_some_and(|object| object.borrow().has("__w3cos_symbol_iterator"))
        );

        let mut iterator = list.iter();
        assert!(iterator.next().is_some_and(|value| value == first));
        let second = document.call_method("createElement", vec![Value::string("b")]);
        parent.call_method("appendChild", vec![second.clone()]);
        assert!(iterator.next().is_some_and(|value| value == second));
        assert!(iterator.next().is_none());
    }

    #[test]
    fn composed_events_cross_shadow_boundary_and_retarget_to_host() {
        setup();
        let host = create_in_body("div");
        let root = host.call_method(
            "attachShadow",
            vec![Value::object(HashMap::from([(
                "mode".to_string(),
                Value::string("open"),
            )]))],
        );
        root.set_property("innerHTML", Value::string("<button>Go</button>"));
        let button = root.call_method("querySelector", vec![Value::string("button")]);
        let calls = Rc::new(RefCell::new(Vec::<String>::new()));

        for (target, label) in [
            (root.clone(), "root"),
            (host.clone(), "host"),
            (document_value().get_property("body"), "body"),
        ] {
            let calls = calls.clone();
            let host = host.clone();
            target.call_method(
                "addEventListener",
                vec![
                    Value::string("shadow-test"),
                    func(move |_, args| {
                        let event = arg(&args, 0);
                        let retargeted = event.get_property("target") == host;
                        let path_len = event
                            .call_method("composedPath", vec![])
                            .get_property("length")
                            .to_number();
                        calls
                            .borrow_mut()
                            .push(format!("{label}:{retargeted}:{path_len}"));
                        Value::Undefined
                    }),
                ],
            );
        }

        let event = w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![
                Value::string("shadow-test"),
                Value::object(HashMap::from([
                    ("bubbles".to_string(), Value::Bool(true)),
                    ("composed".to_string(), Value::Bool(true)),
                ])),
            ],
        );
        button.call_method("dispatchEvent", vec![event]);
        assert_eq!(
            calls.borrow().as_slice(),
            ["root:false:5", "host:true:5", "body:true:5"]
        );

        calls.borrow_mut().clear();
        let event = w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![
                Value::string("shadow-test"),
                Value::object(HashMap::from([
                    ("bubbles".to_string(), Value::Bool(true)),
                    ("composed".to_string(), Value::Bool(false)),
                ])),
            ],
        );
        button.call_method("dispatchEvent", vec![event]);
        assert_eq!(calls.borrow().as_slice(), ["root:false:2"]);
    }

    #[test]
    fn window_history_is_live_and_dispatches_popstate() {
        setup();
        let window = window_value();
        let history = window.get_property("history");
        let popstate_calls = Rc::new(Cell::new(0));
        let calls = popstate_calls.clone();
        let states = Rc::new(RefCell::new(Vec::<String>::new()));
        let states_for_handler = Rc::clone(&states);
        window.call_method(
            "addEventListener",
            vec![
                Value::string("popstate"),
                func(move |_, args| {
                    let event = args[0].clone();
                    assert!(w3cos_core::class::instance_of(
                        &event,
                        &window_value().get_property("PopStateEvent")
                    ));
                    states_for_handler
                        .borrow_mut()
                        .push(event.get_property("state").to_js_string());
                    calls.set(calls.get() + 1);
                    Value::Undefined
                }),
            ],
        );

        history.call_method(
            "pushState",
            vec![
                Value::string("route-state"),
                Value::string(""),
                Value::string("/?task=42"),
            ],
        );
        assert_eq!(
            window
                .get_property("location")
                .get_property("search")
                .to_js_string(),
            "?task=42"
        );
        assert_eq!(history.get_property("length").to_number(), 2.0);

        history.call_method("back", vec![]);
        assert_eq!(
            window
                .get_property("location")
                .get_property("search")
                .to_js_string(),
            ""
        );
        assert_eq!(popstate_calls.get(), 1);
        assert_eq!(states.borrow().as_slice(), ["null"]);

        history.call_method("forward", vec![]);
        assert_eq!(popstate_calls.get(), 2);
        assert_eq!(states.borrow().as_slice(), ["null", "route-state"]);
        assert_eq!(history.get_property("state").to_js_string(), "route-state");
    }

    #[test]
    fn location_navigation_and_hashchange_are_live() {
        setup();
        crate::history::reset();
        let window = window_value();
        let location = window.get_property("location");
        let history = window.get_property("history");
        let hashes = Rc::new(RefCell::new(Vec::<String>::new()));
        let hashes_for_handler = Rc::clone(&hashes);
        window.call_method(
            "addEventListener",
            vec![
                Value::string("hashchange"),
                func(move |_, args| {
                    let event = args[0].clone();
                    assert!(w3cos_core::class::instance_of(
                        &event,
                        &window_value().get_property("HashChangeEvent")
                    ));
                    hashes_for_handler.borrow_mut().push(format!(
                        "{}>{}",
                        event.get_property("oldURL").to_js_string(),
                        event.get_property("newURL").to_js_string()
                    ));
                    Value::Undefined
                }),
            ],
        );

        location.set_property("hash", Value::string("section"));
        assert_eq!(location.get_property("hash").to_js_string(), "#section");
        assert_eq!(history.get_property("length").to_number(), 2.0);
        assert_eq!(hashes.borrow().len(), 1);

        location.set_property("pathname", Value::string("orders"));
        assert_eq!(location.get_property("pathname").to_js_string(), "/orders");
        assert_eq!(history.get_property("length").to_number(), 3.0);

        location.call_method("replace", vec![Value::string("/replaced?q=1#next")]);
        assert_eq!(
            location.get_property("pathname").to_js_string(),
            "/replaced"
        );
        assert_eq!(location.get_property("search").to_js_string(), "?q=1");
        assert_eq!(location.get_property("hash").to_js_string(), "#next");
        assert_eq!(history.get_property("length").to_number(), 3.0);
        assert_eq!(hashes.borrow().len(), 2);

        location.call_method("assign", vec![Value::string("/assigned")]);
        assert_eq!(
            location.get_property("pathname").to_js_string(),
            "/assigned"
        );
        assert_eq!(history.get_property("length").to_number(), 4.0);
        assert_eq!(hashes.borrow().len(), 3);

        location.set_property("href", Value::string("/via-href?ok=1"));
        assert_eq!(
            location.get_property("pathname").to_js_string(),
            "/via-href"
        );
        assert_eq!(location.get_property("search").to_js_string(), "?ok=1");
        assert_eq!(history.get_property("length").to_number(), 5.0);
    }

    #[test]
    fn create_element_and_append_child_shapes_dom() {
        setup();
        let doc = document_value();
        let div = doc.call_method("createElement", vec![Value::string("div")]);
        let span = doc.call_method("createElement", vec![Value::string("span")]);
        doc.get_property("body")
            .call_method("appendChild", vec![div.clone()]);
        div.call_method("appendChild", vec![span.clone()]);

        let body = dom::body_id();
        assert_eq!(dom::children(body).len(), 1);
        let div_id = node_id_of(&div).unwrap();
        let span_id = node_id_of(&span).unwrap();
        assert_eq!(dom::children(body)[0], div_id);
        assert_eq!(dom::children(div_id), vec![span_id]);
        assert_eq!(dom::tag_name(span_id), "span");
        assert_eq!(dom::parent_node(span_id), Some(div_id));
    }

    #[test]
    fn element_identity_is_memoized() {
        setup();
        let doc = document_value();
        let div = doc.call_method("createElement", vec![Value::string("div")]);
        let id = node_id_of(&div).unwrap();
        let again = element_value(id);
        assert!(div == again, "same node must yield the same Rc (===)");

        doc.get_property("body")
            .call_method("appendChild", vec![div.clone()]);
        let parent = div.get_property("parentNode");
        let body = doc.get_property("body");
        assert!(
            parent == body,
            "parentNode must be identical to document.body"
        );
    }

    #[test]
    fn text_content_roundtrip_via_proxy() {
        setup();
        let div = create_in_body("div");
        div.set_property("textContent", Value::string("hello"));
        assert_eq!(div.get_property("textContent").to_js_string(), "hello");
        assert_eq!(dom::inner_text(node_id_of(&div).unwrap()), "hello");

        // Setting textContent replaces children (spec behavior).
        let child = document_value().call_method("createElement", vec![Value::string("b")]);
        div.call_method("appendChild", vec![child]);
        div.set_property("textContent", Value::string("only-text"));
        assert_eq!(
            div.get_property("childNodes")
                .get_property("length")
                .to_number(),
            0.0
        );
        assert_eq!(div.get_property("textContent").to_js_string(), "only-text");
    }

    #[test]
    fn text_node_via_document() {
        setup();
        let doc = document_value();
        let text = doc.call_method("createTextNode", vec![Value::string("some text")]);
        assert_eq!(text.get_property("nodeType").to_number(), 3.0);
        assert_eq!(text.get_property("nodeValue").to_js_string(), "some text");
        text.set_property("nodeValue", Value::string("changed"));
        assert_eq!(text.get_property("data").to_js_string(), "changed");
    }

    #[test]
    fn style_set_and_get() {
        setup();
        let div = create_in_body("div");
        let style = div.get_property("style");
        style.set_property("fontSize", Value::string("14px"));
        style.set_property("lineHeight", Value::string("20px"));
        assert_eq!(style.get_property("fontSize").to_js_string(), "14px");
        // Unknown-to-typed-style property survives via the bridge cache.
        assert_eq!(style.get_property("lineHeight").to_js_string(), "20px");
        assert_eq!(
            style
                .call_method("getPropertyValue", vec![Value::string("font-size")])
                .to_js_string(),
            "14px"
        );
        // cssText serialization + parse.
        let css = style.get_property("cssText").to_js_string();
        assert!(css.contains("font-size: 14px"), "cssText was: {css}");
        assert!(css.contains("line-height: 20px"), "cssText was: {css}");
        style.set_property("cssText", Value::string("color: red; display: flex"));
        assert_eq!(style.get_property("color").to_js_string(), "red");
        assert_eq!(style.get_property("display").to_js_string(), "flex");
        // Typed style actually reached the document (drives layout).
        let id = node_id_of(&div).unwrap();
        let typed = dom::with_document(|d| {
            Element::new(NodeId::from_u32(id))
                .style(d)
                .get_property("display")
        });
        assert_eq!(typed, "flex");
    }

    #[test]
    fn typed_style_maps_roundtrip_values_and_update_inline_style() {
        setup();
        let window = window_value();
        let div = create_in_body("div");
        let map = div.get_property("attributeStyleMap");
        assert!(w3cos_core::class::instance_of(
            &map,
            &window.get_property("StylePropertyMap")
        ));

        let width = w3cos_core::class::construct(
            &window.get_property("CSSUnitValue"),
            vec![Value::Number(12.0), Value::string("px")],
        );
        map.call_method("set", vec![Value::string("width"), width]);
        assert_eq!(
            div.get_property("style")
                .get_property("width")
                .to_js_string(),
            "12px"
        );
        assert_eq!(map.get_property("size").to_number(), 1.0);
        let stored = map.call_method("get", vec![Value::string("width")]);
        assert!(w3cos_core::class::instance_of(
            &stored,
            &window.get_property("CSSUnitValue")
        ));
        assert_eq!(stored.get_property("value").to_number(), 12.0);
        assert_eq!(stored.get_property("unit").to_js_string(), "px");

        let computed = div.call_method("computedStyleMap", vec![]);
        assert!(w3cos_core::class::instance_of(
            &computed,
            &window.get_property("StylePropertyMapReadOnly")
        ));
        assert_eq!(
            computed
                .call_method("get", vec![Value::string("width")])
                .get_property("value")
                .to_number(),
            12.0
        );

        map.call_method("delete", vec![Value::string("width")]);
        assert_eq!(map.get_property("size").to_number(), 0.0);
        assert!(
            map.call_method("get", vec![Value::string("width")])
                .is_undefined()
        );
    }

    #[test]
    fn element_edit_context_tracks_attachment_and_detachment() {
        setup();
        let element = create_in_body("div");
        let context = w3cos_core::class::construct(
            &window_value().get_property("EditContext"),
            vec![Value::object(HashMap::from([(
                "text".into(),
                Value::string("draft"),
            )]))],
        );
        element.set_property("editContext", context.clone());
        assert!(element.get_property("editContext") == context);
        assert!(context.get_property("attachedElements").get_property("0") == element);
        element.set_property("editContext", Value::Null);
        assert!(element.get_property("editContext").is_null());
        assert_eq!(
            context
                .get_property("attachedElements")
                .get_property("length")
                .to_u32(),
            0
        );
    }

    #[test]
    fn css_namespace_and_constructable_stylesheets_match_cssom_shape() {
        setup();
        let window = window_value();
        let css = window.get_property("CSS");
        assert!(
            css.call_method(
                "supports",
                vec![Value::string("display"), Value::string("grid")],
            )
            .to_bool()
        );
        assert!(
            !css.call_method(
                "supports",
                vec![Value::string("made-up-property"), Value::string("value")],
            )
            .to_bool()
        );
        assert!(
            css.call_method(
                "supports",
                vec![Value::string(
                    "(display: grid) and (not (made-up-property: value))"
                )],
            )
            .to_bool()
        );
        assert!(
            css.call_method(
                "supports",
                vec![Value::string(
                    "(made-up-property: value) or ((display: flex) and (color: red))"
                )],
            )
            .to_bool()
        );
        assert!(
            !css.call_method(
                "supports",
                vec![Value::string(
                    "(display: grid) and (made-up-property: value)"
                )],
            )
            .to_bool()
        );
        assert_eq!(
            css.call_method("escape", vec![Value::string("0a b")])
                .to_js_string(),
            "\\30 a\\ b"
        );

        let element = create_in_body("div");
        let style = element.get_property("style");
        assert!(w3cos_core::class::instance_of(
            &style,
            &window.get_property("CSSStyleDeclaration")
        ));

        let sheet = w3cos_core::class::construct(&window.get_property("CSSStyleSheet"), vec![]);
        sheet.call_method(
            "replaceSync",
            vec![Value::string("a { color: red; } b { display: grid; }")],
        );
        assert_eq!(
            sheet
                .get_property("cssRules")
                .get_property("length")
                .to_u32(),
            2
        );
        assert!(w3cos_core::class::instance_of(
            &sheet,
            &window.get_property("CSSStyleSheet")
        ));
        assert!(w3cos_core::class::instance_of(
            &sheet,
            &window.get_property("StyleSheet")
        ));
        let media = sheet.get_property("media");
        assert!(w3cos_core::class::instance_of(
            &media,
            &window.get_property("MediaList")
        ));
        media.call_method("appendMedium", vec![Value::string("screen")]);
        media.call_method("appendMedium", vec![Value::string("print")]);
        assert_eq!(
            media.get_property("mediaText").to_js_string(),
            "screen, print"
        );
        assert_eq!(
            media
                .call_method("item", vec![Value::Number(1.0)])
                .to_js_string(),
            "print"
        );
        media.set_property("mediaText", Value::string("screen and (min-width: 1px)"));
        assert_eq!(media.get_property("length").to_number(), 1.0);
        let sheets = document_value().get_property("styleSheets");
        assert!(w3cos_core::class::instance_of(
            &sheets,
            &window.get_property("StyleSheetList")
        ));
        assert_eq!(sheets.get_property("length").to_number(), 0.0);
        assert_eq!(
            sheets.call_method("item", vec![Value::Number(0.0)]),
            Value::Null
        );
        document_value().set_property("adoptedStyleSheets", js_array(vec![sheet.clone()]));
        assert!(
            document_value()
                .get_property("adoptedStyleSheets")
                .get_property("0")
                == sheet
        );
    }

    #[test]
    fn legacy_escape_and_dynamic_code_boundaries_are_browser_shaped() {
        setup();
        let window = window_value();
        let source = "A B✓😀";
        let escaped = window
            .call_method("escape", vec![Value::string(source)])
            .to_js_string();
        assert_eq!(escaped, "A%20B%u2713%uD83D%uDE00");
        assert_eq!(
            window
                .call_method("unescape", vec![Value::string(&escaped)])
                .to_js_string(),
            source
        );
        assert_eq!(
            window
                .call_method("eval", vec![Value::Number(7.0)])
                .to_number(),
            7.0
        );
        #[cfg(feature = "dynamic-js")]
        assert_eq!(
            window
                .call_method("eval", vec![Value::string("1 + 1")])
                .to_number(),
            2.0
        );
        #[cfg(not(feature = "dynamic-js"))]
        assert!(window
            .call_method("eval", vec![Value::string("1 + 1")])
            .is_undefined());
        let dynamic = w3cos_core::class::construct(
            &window.get_property("Function"),
            vec![Value::string("return 1")],
        );
        assert!(dynamic.is_function());
        #[cfg(feature = "dynamic-js")]
        assert_eq!(dynamic.call(Value::Undefined, vec![]).to_number(), 1.0);
        #[cfg(not(feature = "dynamic-js"))]
        assert!(dynamic.call(Value::Undefined, vec![]).is_undefined());
        assert!(window.get_property("Report").is_function());
    }

    #[test]
    fn class_list_works() {
        setup();
        let div = create_in_body("div");
        let cl = div.get_property("classList");
        cl.call_method("add", vec![Value::string("foo"), Value::string("bar")]);
        assert!(
            cl.call_method("contains", vec![Value::string("foo")])
                .to_bool()
        );
        assert_eq!(div.get_property("className").to_js_string(), "foo bar");
        assert_eq!(cl.get_property("length").to_number(), 2.0);
        let toggled = cl.call_method("toggle", vec![Value::string("foo")]);
        assert!(!toggled.to_bool());
        assert!(
            !cl.call_method("contains", vec![Value::string("foo")])
                .to_bool()
        );
        // classList identity is stable.
        assert!(cl == div.get_property("classList"));
        div.set_property("classList", Value::string("replacement"));
        assert!(cl == div.get_property("classList"));
        assert!(w3cos_core::class::instance_of(
            &cl,
            &window_value().get_property("DOMTokenList")
        ));
        let visited = Rc::new(RefCell::new(Vec::new()));
        let callback_values = Rc::clone(&visited);
        cl.call_method(
            "forEach",
            vec![func(move |_, args| {
                callback_values
                    .borrow_mut()
                    .push(arg(&args, 0).to_js_string());
                Value::Undefined
            })],
        );
        assert_eq!(visited.borrow().as_slice(), ["bar"]);
    }

    #[test]
    fn class_list_reflects_raw_attribute_and_validates_atomic_mutations() {
        setup();
        let div = create_in_body("div");
        let class_list = div.get_property("classList");
        div.call_method(
            "setAttribute",
            vec![Value::string("class"), Value::string("   a  a b")],
        );

        assert_eq!(
            class_list.call_method("toString", vec![]).to_js_string(),
            "   a  a b"
        );
        assert_eq!(class_list.get_property("value").to_js_string(), "   a  a b");
        assert_eq!(class_list.get_property("0").to_js_string(), "a");
        assert_eq!(class_list.get_property("1").to_js_string(), "b");

        class_list.call_method("add", vec![Value::string("c")]);
        assert_eq!(
            div.call_method("getAttribute", vec![Value::string("class")])
                .to_js_string(),
            "a b c"
        );

        let error = w3cos_core::catch_js(|| {
            class_list.call_method("add", vec![Value::string("d"), Value::string("bad token")])
        })
        .expect_err("an invalid token must throw before the class attribute changes");
        assert_eq!(
            error.get_property("name").to_js_string(),
            "InvalidCharacterError"
        );
        assert_eq!(
            div.call_method("getAttribute", vec![Value::string("class")])
                .to_js_string(),
            "a b c"
        );

        assert!(
            class_list
                .call_method("replace", vec![Value::string("b"), Value::string("d")])
                .to_bool()
        );
        assert_eq!(
            div.call_method("getAttribute", vec![Value::string("class")])
                .to_js_string(),
            "a d c"
        );

        class_list.set_property("value", Value::string("c b a"));
        assert!(
            class_list
                .call_method("replace", vec![Value::string("c"), Value::string("a")])
                .to_bool()
        );
        assert_eq!(
            div.call_method("getAttribute", vec![Value::string("class")])
                .to_js_string(),
            "a b"
        );

        let error = w3cos_core::catch_js(|| {
            class_list.call_method("replace", vec![Value::string(" "), Value::string("")])
        })
        .expect_err("an empty replacement token takes precedence over whitespace validation");
        assert_eq!(error.get_property("name").to_js_string(), "SyntaxError");

        class_list.set_property("value", Value::string("  x x y "));
        assert_eq!(class_list.get_property("value").to_js_string(), "  x x y ");
        assert_eq!(class_list.get_property("length").to_u32(), 2);

        let error = w3cos_core::catch_js(|| {
            class_list.call_method("supports", vec![Value::string("anything")])
        })
        .expect_err("classList.supports must throw");
        assert_eq!(error.get_property("name").to_js_string(), "TypeError");
    }

    #[test]
    fn performance_event_counts_track_only_trusted_interaction_events() {
        setup();
        let button = create_in_body("button");
        let counts = window_value()
            .get_property("performance")
            .get_property("eventCounts");
        assert!(w3cos_core::class::instance_of(
            &counts,
            &window_value().get_property("EventCounts")
        ));
        assert_eq!(counts.get_property("size").to_u32(), 36);
        assert_eq!(
            counts
                .call_method("get", vec![Value::string("click")])
                .to_u32(),
            0
        );

        let synthetic = w3cos_core::class::construct(
            &crate::web_events::event_class(),
            vec![Value::string("click")],
        );
        button.call_method("dispatchEvent", vec![synthetic]);
        assert_eq!(
            counts
                .call_method("get", vec![Value::string("click")])
                .to_u32(),
            0
        );

        dispatch_native_click(node_id_of(&button).expect("button node"));
        assert_eq!(
            counts
                .call_method("get", vec![Value::string("click")])
                .to_u32(),
            1
        );
    }

    #[test]
    fn trusted_summary_click_toggles_parent_details_open_state() {
        setup();
        let details = create_in_body("details");
        let summary = document_value().call_method("createElement", vec![Value::string("summary")]);
        details.call_method("appendChild", vec![summary.clone()]);
        let details_id = node_id_of(&details).expect("details node");
        let summary_id = node_id_of(&summary).expect("summary node");

        assert!(!dom::has_attribute(details_id, "open"));
        assert!(!dispatch_native_click(summary_id));
        assert!(dom::has_attribute(details_id, "open"));
        assert!(!dispatch_native_click(summary_id));
        assert!(!dom::has_attribute(details_id, "open"));
    }

    #[test]
    fn dataset_is_a_live_dom_string_map() {
        setup();
        let div = create_in_body("div");
        let dataset = div.get_property("dataset");
        assert!(dataset == div.get_property("dataset"));
        assert!(w3cos_core::class::instance_of(
            &dataset,
            &window_value().get_property("DOMStringMap")
        ));
        dataset.set_property("userId", Value::string("42"));
        assert_eq!(
            div.call_method("getAttribute", vec![Value::string("data-user-id")])
                .to_js_string(),
            "42"
        );
        div.call_method(
            "setAttribute",
            vec![Value::string("data-route-name"), Value::string("inbox")],
        );
        assert_eq!(dataset.get_property("routeName").to_js_string(), "inbox");
        dataset.delete_property("userId");
        assert!(
            !div.call_method("hasAttribute", vec![Value::string("data-user-id")])
                .to_bool()
        );
    }

    #[test]
    fn attributes_roundtrip() {
        setup();
        let div = create_in_body("div");
        div.call_method(
            "setAttribute",
            vec![Value::string("id"), Value::string("main")],
        );
        div.call_method(
            "setAttribute",
            vec![Value::string("data-x"), Value::string("1")],
        );
        assert_eq!(
            div.call_method("getAttribute", vec![Value::string("id")])
                .to_js_string(),
            "main"
        );
        assert!(
            div.call_method("hasAttribute", vec![Value::string("data-x")])
                .to_bool()
        );
        assert_eq!(div.get_property("id").to_js_string(), "main");
        div.set_property("id", Value::string("renamed"));
        assert_eq!(
            dom::get_attribute(node_id_of(&div).unwrap(), "id").as_deref(),
            Some("renamed")
        );
        div.call_method("removeAttribute", vec![Value::string("data-x")]);
        assert!(
            div.call_method("getAttribute", vec![Value::string("data-x")])
                .is_null()
        );
        let ds = div.get_property("dataset");
        let _ = ds; // dataset built without panic
    }

    #[test]
    fn document_get_element_by_id_and_query_selector() {
        setup();
        let doc = document_value();
        let div = create_in_body("div");
        div.call_method(
            "setAttribute",
            vec![Value::string("id"), Value::string("app")],
        );
        div.get_property("classList")
            .call_method("add", vec![Value::string("container")]);

        let by_id = doc.call_method("getElementById", vec![Value::string("app")]);
        assert!(by_id == div);
        let by_sel = doc.call_method("querySelector", vec![Value::string("#app")]);
        assert!(by_sel == div);
        let by_class = doc.call_method("querySelector", vec![Value::string(".container")]);
        assert!(by_class == div);
        let by_tag = doc.call_method("querySelector", vec![Value::string("div")]);
        assert!(by_tag == div);
        let all = doc.call_method("querySelectorAll", vec![Value::string("div")]);
        assert_eq!(all.get_property("length").to_number(), 1.0);
        let missing = doc.call_method("querySelector", vec![Value::string("#nope")]);
        assert!(missing.is_null());

        let empty = create_in_body("div");
        empty.set_property("id", Value::string(""));
        assert!(
            doc.call_method("getElementById", vec![Value::string("")])
                .is_null()
        );
        let undefined = create_in_body("div");
        undefined.set_property("id", Value::string("undefined"));
        assert!(doc.call_method("getElementById", vec![Value::Undefined]) == undefined);

        let live_attr = create_in_body("div");
        live_attr.set_property("id", Value::string("before"));
        live_attr
            .get_property("attributes")
            .get_property("0")
            .set_property("value", Value::string("after"));
        assert!(
            doc.call_method("getElementById", vec![Value::string("before")])
                .is_null()
        );
        assert!(doc.call_method("getElementById", vec![Value::string("after")]) == live_attr);

        let replaced = create_in_body("div");
        replaced.set_property("id", Value::string("old-outer"));
        replaced.set_property("outerHTML", Value::string("<div id='new-outer'></div>"));
        assert!(
            doc.call_method("getElementById", vec![Value::string("old-outer")])
                .is_null()
        );
        assert!(
            !doc.call_method("getElementById", vec![Value::string("new-outer")])
                .is_null()
        );
    }

    #[test]
    fn query_selector_requires_an_argument_and_rejects_invalid_syntax() {
        setup();
        let document = document_value();
        let element = create_in_body("div");
        let fragment = document.call_method("createDocumentFragment", vec![]);

        for root in [document, element, fragment] {
            let missing = w3cos_core::catch_js(|| root.call_method("querySelector", vec![]))
                .expect_err("a missing selector must throw");
            assert_eq!(missing.get_property("name").to_js_string(), "TypeError");

            let invalid = w3cos_core::catch_js(|| {
                root.call_method("querySelectorAll", vec![Value::string("div,")])
            })
            .expect_err("invalid selector syntax must throw");
            assert_eq!(invalid.get_property("name").to_js_string(), "SyntaxError");
        }
    }

    #[test]
    fn query_selector_distinguishes_no_namespace_elements() {
        setup();
        let document = document_value();
        let root = create_in_body("div");
        let html_child = document.call_method("createElement", vec![Value::string("div")]);
        let no_namespace_child = document.call_method(
            "createElementNS",
            vec![Value::Null, Value::string("div")],
        );
        root.call_method("appendChild", vec![html_child]);
        root.call_method("appendChild", vec![no_namespace_child.clone()]);

        let matches = root.call_method("querySelectorAll", vec![Value::string("|div")]);
        assert_eq!(matches.get_property("length").to_u32(), 1);
        assert!(matches.get_property("0") == no_namespace_child);
    }

    #[test]
    fn live_xhtml_document_creates_cdata_and_unicode_processing_instructions() {
        setup();
        let document = document_value();
        document.set_property("contentType", Value::string("application/xhtml+xml"));
        let cdata = document.call_method("createCDATASection", vec![Value::string("a < b")]);
        assert_eq!(cdata.get_property("nodeType"), Value::Number(4.0));
        assert!(cdata.get_property("ownerDocument") == document);
        let instruction = document.call_method(
            "createProcessingInstruction",
            vec![Value::string("A·A"), Value::string("x")],
        );
        assert_eq!(instruction.get_property("target"), Value::string("A·A"));
        assert!(instruction.get_property("ownerDocument") == document);

        let stylesheet_instruction = document.call_method(
            "createProcessingInstruction",
            vec![
                Value::string("xml-stylesheet"),
                Value::string("href=\"data:text/css,&#x41;&amp;&apos;\" type=\"text/css\""),
            ],
        );
        assert_eq!(
            stylesheet_instruction
                .get_property("sheet")
                .get_property("href"),
            Value::string("data:text/css,A&'")
        );
    }

    #[test]
    fn non_markup_frame_documents_preserve_response_content_type() {
        setup();
        for content_type in [
            "text/css",
            "text/plain",
            "image/bmp",
            "image/gif",
            "image/jpeg",
            "image/png",
        ] {
            let document = parse_frame_document(
                "not parsed as HTML",
                content_type,
                "https://example.test/resource",
            );
            assert_eq!(
                document.get_property("contentType"),
                Value::string(content_type)
            );
            assert!(document.get_property("documentElement").is_null());
        }
    }

    #[test]
    fn xml_frame_documents_project_their_doctype_before_the_root_element() {
        setup();
        let document = parse_frame_document(
            "<!DOCTYPE foo [<!ELEMENT foo EMPTY>]><foo/>",
            "application/xml",
            "https://example.test/frame.xml",
        );
        let doctype = document.get_property("doctype");
        assert_eq!(doctype.get_property("nodeType"), Value::Number(10.0));
        assert_eq!(doctype.get_property("name"), Value::string("foo"));
        assert!(document.get_property("firstChild") == doctype);
        assert!(doctype.get_property("nextSibling") == document.get_property("documentElement"));
    }

    #[test]
    fn scoped_query_selector_with_descendant_selector() {
        setup();
        let doc = document_value();
        let outer = create_in_body("div");
        outer
            .get_property("classList")
            .call_method("add", vec![Value::string("outer")]);
        let inner = doc.call_method("createElement", vec![Value::string("span")]);
        inner
            .get_property("classList")
            .call_method("add", vec![Value::string("leaf")]);
        outer.call_method("appendChild", vec![inner.clone()]);
        // A second .leaf outside `outer` must not match the scoped query.
        let stray = create_in_body("span");
        stray
            .get_property("classList")
            .call_method("add", vec![Value::string("leaf")]);

        let found = outer.call_method("querySelector", vec![Value::string(".leaf")]);
        assert!(found == inner);
        let all = outer.call_method("querySelectorAll", vec![Value::string(".leaf")]);
        assert_eq!(all.get_property("length").to_number(), 1.0);
        // Descendant combinator.
        let chained = doc.call_method("querySelector", vec![Value::string(".outer .leaf")]);
        assert!(chained == inner);
        // matches() / closest()
        assert!(
            inner
                .call_method("matches", vec![Value::string("span.leaf")])
                .to_bool()
        );
        assert!(
            !inner
                .call_method("matches", vec![Value::string("div")])
                .to_bool()
        );
        let closest = inner.call_method("closest", vec![Value::string(".outer")]);
        assert!(closest == outer);
        // contains()
        assert!(outer.call_method("contains", vec![inner.clone()]).to_bool());
        assert!(!inner.call_method("contains", vec![outer.clone()]).to_bool());
    }

    #[test]
    fn node_tree_relation_methods_cover_documents_siblings_and_disconnected_roots() {
        setup();
        let document = document_value();
        let parent = create_in_body("div");
        let first = document.call_method("createElement", vec![Value::string("span")]);
        let second = document.call_method("createElement", vec![Value::string("span")]);
        parent.call_method("appendChild", vec![first.clone()]);
        parent.call_method("appendChild", vec![second.clone()]);

        assert!(document.get_property("nextSibling").is_null());
        assert!(document.get_property("previousSibling").is_null());
        assert!(document.get_property("ownerDocument").is_null());
        assert!(document.call_method("hasChildNodes", vec![]).to_bool());
        assert_eq!(document.get_property("charset"), Value::string("UTF-8"));
        assert_eq!(
            document.get_property("inputEncoding"),
            Value::string("UTF-8")
        );
        assert!(document.call_method("contains", vec![parent.clone()]).to_bool());
        assert!(parent.call_method("contains", vec![first.clone()]).to_bool());
        assert!(!first.call_method("contains", vec![parent.clone()]).to_bool());
        assert_eq!(
            parent
                .call_method("compareDocumentPosition", vec![first.clone()])
                .to_u32(),
            0x10 | 0x04
        );
        assert_eq!(
            first
                .call_method("compareDocumentPosition", vec![parent.clone()])
                .to_u32(),
            0x08 | 0x02
        );
        assert_eq!(
            first
                .call_method("compareDocumentPosition", vec![second.clone()])
                .to_u32(),
            0x04
        );
        assert_eq!(
            second
                .call_method("compareDocumentPosition", vec![first.clone()])
                .to_u32(),
            0x02
        );
        let doctype = document.get_property("doctype");
        if !doctype.is_nullish() {
            assert!(
                document
                    .get_property("documentElement")
                    .get_property("previousSibling")
                    .strict_eq(&doctype)
            );
            assert!(
                doctype
                    .get_property("nextSibling")
                    .strict_eq(&document.get_property("documentElement"))
            );
            assert_eq!(
                doctype
                    .call_method("compareDocumentPosition", vec![parent.clone()])
                    .to_u32(),
                0x04
            );
            assert_eq!(
                parent
                    .call_method("compareDocumentPosition", vec![doctype])
                    .to_u32(),
                0x02
            );
        }

        let detached = document.call_method("createElement", vec![Value::string("aside")]);
        let forward = first
            .call_method("compareDocumentPosition", vec![detached.clone()])
            .to_u32();
        let reverse = detached
            .call_method("compareDocumentPosition", vec![first])
            .to_u32();
        assert_eq!(forward & (0x01 | 0x20), 0x01 | 0x20);
        assert_eq!(reverse & (0x01 | 0x20), 0x01 | 0x20);
        assert_eq!(forward & (0x02 | 0x04), (reverse & (0x02 | 0x04)) ^ 0x06);
    }

    #[test]
    fn child_node_after_converts_values_and_preserves_argument_order() {
        setup();
        let document = document_value();
        let parent = create_in_body("div");
        let child = document.call_method("createElement", vec![Value::string("test")]);
        let x = document.call_method("createElement", vec![Value::string("x")]);
        let y = document.call_method("createElement", vec![Value::string("y")]);
        parent.call_method("appendChild", vec![child.clone()]);
        parent.call_method("appendChild", vec![x.clone()]);
        parent.call_method("appendChild", vec![y.clone()]);

        child.call_method(
            "after",
            vec![y, x, Value::Null, Value::Undefined, Value::string("text")],
        );

        assert_eq!(
            parent.get_property("innerHTML").to_js_string(),
            "<test></test><y></y><x></x>nullundefinedtext"
        );
    }

    #[test]
    fn parent_node_prepend_converts_values_and_preserves_argument_order() {
        setup();
        let document = document_value();
        let parent = create_in_body("div");
        let existing = document.call_method("createElement", vec![Value::string("existing")]);
        let first = document.call_method("createElement", vec![Value::string("first")]);
        parent.call_method("appendChild", vec![existing]);

        parent.call_method("prepend", vec![first, Value::Null, Value::string("text")]);

        assert_eq!(
            parent.get_property("innerHTML").to_js_string(),
            "<first></first>nulltext<existing></existing>"
        );
    }

    #[test]
    fn child_node_replace_with_keeps_context_when_it_is_an_argument() {
        setup();
        let document = document_value();
        let parent = create_in_body("div");
        let child = document.call_method("createElement", vec![Value::string("test")]);
        let sibling = document.call_method("createElement", vec![Value::string("x")]);
        parent.call_method("appendChild", vec![child.clone()]);
        parent.call_method("appendChild", vec![sibling.clone()]);
        parent.call_method(
            "appendChild",
            vec![document.call_method("createTextNode", vec![Value::string("tail")])],
        );

        child
            .clone()
            .call_method("replaceWith", vec![sibling, child]);

        assert_eq!(
            parent.get_property("innerHTML").to_js_string(),
            "<x></x><test></test>tail"
        );
    }

    #[test]
    fn query_selector_attribute_matching_includes_empty_values() {
        setup();
        let root = create_in_body("div");
        root.call_method(
            "setAttribute",
            vec![Value::string("id"), Value::string("root")],
        );
        root.set_property(
            "innerHTML",
            Value::string("<div id=\"\"></div><div id></div><div></div>"),
        );
        assert!(window_value().get_property("root") == root);
        assert!(
            window_value()
                .as_object()
                .is_some_and(|window| window.borrow().has("root"))
        );

        assert_eq!(
            root.call_method("querySelectorAll", vec![Value::string("[id]")])
                .get_property("length")
                .to_u32(),
            2
        );
        assert_eq!(
            root.call_method("querySelectorAll", vec![Value::string("[id='']")])
                .get_property("length")
                .to_u32(),
            2
        );
    }

    #[test]
    fn query_selector_keeps_whitespace_inside_quoted_attribute_values() {
        setup();
        let document = document_value();
        let link = create_in_body("a");
        link.call_method(
            "setAttribute",
            vec![
                Value::string("title"),
                Value::string("test with - dash and space"),
            ],
        );

        let selector = Value::string("a[title='test with - dash and space']");
        assert!(document.call_method("querySelector", vec![selector.clone()]) == link);
        assert_eq!(
            document
                .call_method("querySelectorAll", vec![selector.clone()])
                .get_property("length")
                .to_u32(),
            1
        );
        let matches = document.call_method("querySelectorAll", vec![selector]);
        assert!(matches.get_property("0") == link);
        assert!(
            matches
                .call_method("hasOwnProperty", vec![Value::string("0")])
                .to_bool()
        );
        assert!(
            !matches
                .call_method("hasOwnProperty", vec![Value::string("length")])
                .to_bool()
        );
    }

    #[test]
    fn node_lists_are_static_and_html_collections_are_live_and_named() {
        setup();
        let window = window_value();
        let document = document_value();
        let body = document.get_property("body");
        let children = body.get_property("children");
        let child_nodes = body.get_property("childNodes");
        let articles = document.call_method("getElementsByTagName", vec![Value::string("article")]);
        let initial_query =
            document.call_method("querySelectorAll", vec![Value::string("article")]);

        assert!(w3cos_core::class::instance_of(
            &children,
            &window.get_property("HTMLCollection")
        ));
        assert!(w3cos_core::class::instance_of(
            &child_nodes,
            &window.get_property("NodeList")
        ));
        assert!(w3cos_core::class::instance_of(
            &initial_query,
            &window.get_property("NodeList")
        ));

        let article = document.call_method("createElement", vec![Value::string("article")]);
        article.set_property("id", Value::string("primary"));
        body.call_method("appendChild", vec![article.clone()]);

        assert_eq!(children.get_property("length").to_u32(), 1);
        assert_eq!(child_nodes.get_property("length").to_u32(), 1);
        assert_eq!(articles.get_property("length").to_u32(), 1);
        assert_eq!(initial_query.get_property("length").to_u32(), 0);
        assert!(children.call_method("namedItem", vec![Value::string("primary")]) == article);
        assert!(articles.call_method("item", vec![Value::Number(0.0)]) == article);
        assert!(!articles.try_set_property("0", Value::string("blocked")));
        let strict_error = w3cos_core::catch_js(|| {
            w3cos_core::intrinsics::set_property_strict(
                &articles,
                &Value::string("0"),
                Value::string("blocked"),
            )
        })
        .expect_err("strict assignment to a collection index must throw");
        assert_eq!(
            strict_error.get_property("name").to_js_string(),
            "TypeError"
        );
        assert!(strict_error.get_property("constructor") == window.get_property("TypeError"));

        let calls = Rc::new(Cell::new(0_u32));
        let callback_calls = calls.clone();
        document
            .call_method("querySelectorAll", vec![Value::string("article")])
            .call_method(
                "forEach",
                vec![func(move |_, args| {
                    assert_eq!(arg(&args, 1).to_u32(), 0);
                    callback_calls.set(callback_calls.get() + 1);
                    Value::Undefined
                })],
            );
        assert_eq!(calls.get(), 1);
        assert_eq!(articles.iter().count(), 1);

        body.call_method("removeChild", vec![article]);
        assert_eq!(articles.get_property("length").to_u32(), 0);

        let class_matches = document.call_method(
            "getElementsByClassName",
            vec![Value::string("featured current")],
        );
        let classified = document.call_method("createElement", vec![Value::string("aside")]);
        classified.set_property("className", Value::string("current featured extra"));
        body.call_method("appendChild", vec![classified.clone()]);
        assert_eq!(class_matches.get_property("length").to_u32(), 1);
        body.call_method("removeChild", vec![classified]);
        assert_eq!(class_matches.get_property("length").to_u32(), 0);

        let foreign = document.call_method(
            "createElementNS",
            vec![Value::string("test"), Value::string("ST")],
        );
        let foreign_by_tag =
            document.call_method("getElementsByTagName", vec![Value::string("ST")]);
        let foreign_by_namespace = document.call_method(
            "getElementsByTagNameNS",
            vec![Value::string("test"), Value::string("ST")],
        );
        body.call_method("appendChild", vec![foreign.clone()]);
        assert_eq!(foreign_by_tag.get_property("length").to_u32(), 1);
        assert_eq!(foreign_by_namespace.get_property("length").to_u32(), 1);
        assert!(foreign_by_namespace.get_property("0") == foreign);
        body.call_method("removeChild", vec![foreign]);
        assert_eq!(foreign_by_tag.get_property("length").to_u32(), 0);
        assert_eq!(foreign_by_namespace.get_property("length").to_u32(), 0);
    }

    #[test]
    fn table_row_and_cell_collections_are_live_and_delete_row_mutates_the_tree() {
        setup();
        let document = document_value();
        let table = create_in_body("table");
        let body = document.call_method("createElement", vec![Value::string("tbody")]);
        let first_row = document.call_method("createElement", vec![Value::string("tr")]);
        let first_cell = document.call_method("createElement", vec![Value::string("td")]);
        first_row.call_method("appendChild", vec![first_cell.clone()]);
        body.call_method("appendChild", vec![first_row]);
        table.call_method("appendChild", vec![body.clone()]);

        let bodies = table.get_property("tBodies");
        let table_rows = table.get_property("rows");
        let section_rows = body.get_property("rows");
        assert_eq!(bodies.get_property("length").to_u32(), 1);
        assert_eq!(table_rows.get_property("length").to_u32(), 1);
        assert_eq!(section_rows.get_property("length").to_u32(), 1);
        assert!(
            section_rows
                .get_property("0")
                .get_property("cells")
                .get_property("0")
                == first_cell
        );

        let second_row = document.call_method("createElement", vec![Value::string("tr")]);
        body.call_method("appendChild", vec![second_row]);
        assert_eq!(table_rows.get_property("length").to_u32(), 2);
        assert_eq!(section_rows.get_property("length").to_u32(), 2);

        table.call_method("deleteRow", vec![Value::Number(0.0)]);
        assert_eq!(table_rows.get_property("length").to_u32(), 1);
        assert_eq!(section_rows.get_property("length").to_u32(), 1);
    }

    #[test]
    fn class_name_collections_use_html_space_and_quirks_ascii_case_rules() {
        setup();
        let document = document_value();
        set_document_compat_mode(true);

        let mixed_case = create_in_body("div");
        mixed_case.set_property("className", Value::string("a A"));
        let quirks_matches =
            document.call_method("getElementsByClassName", vec![Value::string("A a")]);
        assert_eq!(quirks_matches.get_property("length").to_u32(), 1);
        assert!(quirks_matches.get_property("0") == mixed_case);

        let non_breaking_space = create_in_body("span");
        non_breaking_space.set_property("className", Value::string("\u{00a0}"));
        let unicode_matches =
            document.call_method("getElementsByClassName", vec![Value::string("\u{00a0}")]);
        assert_eq!(unicode_matches.get_property("length").to_u32(), 1);
        assert!(unicode_matches.get_property("0") == non_breaking_space);
    }

    #[test]
    fn deep_clone_preserves_svg_node_identity_metadata() {
        setup();
        let document = document_value();
        let svg = document.call_method(
            "createElementNS",
            vec![
                Value::string("http://www.w3.org/2000/svg"),
                Value::string("svg"),
            ],
        );
        let use_element = document.call_method(
            "createElementNS",
            vec![
                Value::string("http://www.w3.org/2000/svg"),
                Value::string("use"),
            ],
        );
        svg.call_method("appendChild", vec![use_element]);

        let clone = svg.call_method("cloneNode", vec![Value::Bool(true)]);
        assert_eq!(
            clone.get_property("namespaceURI"),
            Value::string("http://www.w3.org/2000/svg")
        );
        assert_eq!(clone.get_property("localName"), Value::string("svg"));
        assert_eq!(
            clone
                .get_property("firstElementChild")
                .get_property("namespaceURI"),
            Value::string("http://www.w3.org/2000/svg")
        );
        assert!(clone.get_property("ownerDocument") == document);
    }

    #[test]
    fn detached_attribute_clone_preserves_namespace_and_value() {
        setup();
        let document = document_value();
        let attribute = document.call_method(
            "createAttributeNS",
            vec![Value::string("urn:test"), Value::string("prefix:name")],
        );
        attribute.set_property("value", Value::string("value"));
        let clone = attribute.call_method("cloneNode", vec![]);

        assert!(w3cos_core::class::instance_of(
            &clone,
            &window_value().get_property("Attr")
        ));
        assert!(!clone.strict_eq(&attribute));
        assert_eq!(clone.get_property("nodeType"), Value::Number(2.0));
        assert_eq!(clone.get_property("namespaceURI"), Value::string("urn:test"));
        assert_eq!(clone.get_property("prefix"), Value::string("prefix"));
        assert_eq!(clone.get_property("localName"), Value::string("name"));
        assert_eq!(clone.get_property("value"), Value::string("value"));
    }

    #[test]
    fn deep_document_clone_preserves_doctype_metadata_and_child_ownership() {
        setup();
        let implementation = document_value().get_property("implementation");
        let doctype = implementation.call_method(
            "createDocumentType",
            vec![
                Value::string("name"),
                Value::string("publicId"),
                Value::string("systemId"),
            ],
        );
        let xml_document = implementation.call_method(
            "createDocument",
            vec![Value::string("namespace"), Value::string(""), doctype],
        );
        let xml_clone = xml_document.call_method("cloneNode", vec![Value::Bool(true)]);
        assert_eq!(
            xml_clone.get_property("childNodes").get_property("length"),
            Value::Number(1.0)
        );
        assert_eq!(
            xml_clone.get_property("doctype").get_property("publicId"),
            Value::string("publicId")
        );
        assert_eq!(
            xml_clone.get_property("doctype").get_property("systemId"),
            Value::string("systemId")
        );

        let html_document =
            implementation.call_method("createHTMLDocument", vec![Value::Undefined]);
        let html_clone = html_document.call_method("cloneNode", vec![Value::Bool(true)]);
        assert_eq!(
            html_clone.get_property("childNodes").get_property("length"),
            Value::Number(2.0)
        );
        assert!(
            html_clone
                .get_property("documentElement")
                .get_property("ownerDocument")
                == html_clone
        );

        let parser =
            w3cos_core::class::construct(&window_value().get_property("DOMParser"), vec![]);
        let parsed_document = parser.call_method(
            "parseFromString",
            vec![
                Value::string("<!DOCTYPE html><html></html>"),
                Value::string("text/html"),
            ],
        );
        let parsed_clone = parsed_document.call_method("cloneNode", vec![Value::Bool(true)]);
        assert_eq!(
            parsed_clone
                .get_property("childNodes")
                .get_property("length"),
            Value::Number(2.0)
        );
        assert_eq!(
            parsed_clone.get_property("doctype").get_property("name"),
            Value::string("html")
        );
    }

    #[test]
    fn parsed_xhtml_document_preserves_tree_and_native_clone_identity() {
        setup();
        let document = parse_frame_document(
            "<!DOCTYPE html><html xmlns='http://www.w3.org/1999/xhtml'><head/><body><div id='root'><span id='child'/></div></body></html>",
            "application/xhtml+xml",
            "https://example.test/frame.xhtml#child",
        );
        document.set_property(
            "location",
            Value::object(HashMap::from([(
                "hash".to_string(),
                Value::string("#child"),
            )])),
        );
        let root = document.call_method("getElementById", vec![Value::string("root")]);
        assert_eq!(root.get_property("nodeType"), Value::Number(1.0));
        assert_eq!(document.get_property("body").get_property("nodeName"), Value::string("body"));
        assert!(document.call_method("querySelector", vec![Value::string(":root")])
            == document.get_property("documentElement"));
        assert_eq!(
            document
                .call_method("querySelectorAll", vec![Value::string(":target")])
                .get_property("length"),
            Value::Number(1.0)
        );

        let clone = root.call_method("cloneNode", vec![Value::Bool(true)]);
        assert_eq!(clone.get_property("nodeType"), Value::Number(1.0));
        assert_eq!(clone.get_property("childElementCount"), Value::Number(1.0));
        assert!(clone
            .call_method("querySelector", vec![Value::string(":target")])
            .is_null());
        let fragment = document.call_method("createDocumentFragment", vec![]);
        assert!(fragment.call_method("appendChild", vec![clone.clone()]) == clone);
    }

    #[test]
    fn is_equal_node_compares_properties_attributes_and_descendants() {
        setup();
        let implementation = document_value().get_property("implementation");
        let left_doctype = implementation.call_method(
            "createDocumentType",
            vec![
                Value::string("html"),
                Value::string("public"),
                Value::string("system"),
            ],
        );
        let right_doctype = implementation.call_method(
            "createDocumentType",
            vec![
                Value::string("html"),
                Value::string("public"),
                Value::string("system"),
            ],
        );
        assert!(
            left_doctype
                .call_method("isEqualNode", vec![right_doctype.clone()])
                .to_bool()
        );
        right_doctype.set_property("systemId", Value::string("different"));
        assert!(
            !left_doctype
                .call_method("isEqualNode", vec![right_doctype])
                .to_bool()
        );

        let left = document_value().call_method(
            "createElementNS",
            vec![Value::string("urn:test"), Value::string("prefix:root")],
        );
        let right = document_value().call_method(
            "createElementNS",
            vec![Value::string("urn:test"), Value::string("prefix:root")],
        );
        for element in [&left, &right] {
            element.call_method(
                "setAttributeNS",
                vec![
                    Value::string("urn:attribute"),
                    Value::string("prefix:value"),
                    Value::string("same"),
                ],
            );
            element.call_method(
                "appendChild",
                vec![document_value()
                    .call_method("createComment", vec![Value::string("child")])],
            );
        }
        assert!(left.call_method("isEqualNode", vec![right.clone()]).to_bool());
        right
            .get_property("firstChild")
            .set_property("data", Value::string("different"));
        assert!(!left.call_method("isEqualNode", vec![right]).to_bool());

        let left_document = implementation.call_method("createHTMLDocument", vec![]);
        let right_document = implementation.call_method("createHTMLDocument", vec![]);
        assert!(
            left_document
                .call_method("isEqualNode", vec![right_document.clone()])
                .to_bool()
        );
        right_document
            .get_property("body")
            .call_method("appendChild", vec![right_document.call_method(
                "createElement",
                vec![Value::string("div")],
            )]);
        assert!(
            !left_document
                .call_method("isEqualNode", vec![right_document])
                .to_bool()
        );
    }

    #[test]
    fn namespace_lookup_uses_element_identity_declarations_and_ancestor_context() {
        setup();
        let document = document_value();
        let fragment = document.call_method("createDocumentFragment", vec![]);
        assert!(
            fragment
                .call_method("lookupNamespaceURI", vec![Value::Null])
                .is_null()
        );
        assert!(
            fragment
                .call_method("isDefaultNamespace", vec![Value::Null])
                .to_bool()
        );

        let element = document.call_method(
            "createElementNS",
            vec![Value::string("fooNamespace"), Value::string("prefix:elem")],
        );
        assert_eq!(
            element.call_method("lookupNamespaceURI", vec![Value::string("prefix")]),
            Value::string("fooNamespace")
        );
        assert_eq!(
            element.call_method("lookupPrefix", vec![Value::string("fooNamespace")]),
            Value::string("prefix")
        );
        assert_eq!(
            element.call_method("lookupNamespaceURI", vec![Value::string("xml")]),
            Value::string(crate::html_parser_state::XML_NAMESPACE)
        );
        element.call_method(
            "setAttributeNS",
            vec![
                Value::string(crate::html_parser_state::XMLNS_NAMESPACE),
                Value::string("xmlns:bar"),
                Value::string("barURI"),
            ],
        );
        element.call_method(
            "setAttributeNS",
            vec![
                Value::string(crate::html_parser_state::XMLNS_NAMESPACE),
                Value::string("xmlns"),
                Value::string("bazURI"),
            ],
        );
        assert_eq!(
            element.call_method("lookupNamespaceURI", vec![Value::Null]),
            Value::string("bazURI")
        );
        assert_eq!(
            element.call_method("lookupNamespaceURI", vec![Value::string("bar")]),
            Value::string("barURI")
        );
        assert_eq!(
            element.call_method("lookupPrefix", vec![Value::string("barURI")]),
            Value::string("bar")
        );
        assert!(
            element
                .call_method("lookupPrefix", vec![Value::string("bazURI")])
                .is_null()
        );

        let comment = document.call_method("createComment", vec![Value::string("comment")]);
        element.call_method("appendChild", vec![comment.clone()]);
        assert_eq!(
            comment.call_method("lookupNamespaceURI", vec![Value::Null]),
            Value::string("bazURI")
        );
        assert_eq!(
            comment.call_method("lookupPrefix", vec![Value::string("barURI")]),
            Value::string("bar")
        );

        let attribute = document.call_method("createAttribute", vec![Value::string("foo")]);
        assert!(
            attribute
                .call_method("lookupNamespaceURI", vec![Value::string("xml")])
                .is_null()
        );
        document
            .get_property("body")
            .call_method("setAttributeNode", vec![attribute.clone()]);
        assert_eq!(
            attribute.call_method("lookupNamespaceURI", vec![Value::string("xml")]),
            Value::string(crate::html_parser_state::XML_NAMESPACE)
        );
    }

    #[test]
    fn node_list_reads_an_overridden_length_accessor() {
        setup();
        let list = node_list(vec![Value::Null; 3]);
        let descriptor = Value::object(HashMap::from([
            ("configurable".to_string(), Value::Bool(true)),
            ("get".to_string(), func(|_, _| Value::Number(1.0))),
        ]));
        w3cos_core::object_value().call_method(
            "defineProperty",
            vec![list.clone(), Value::string("length"), descriptor],
        );
        assert_eq!(list.get_property("length"), Value::Number(1.0));
    }

    #[test]
    fn node_list_generic_index_of_uses_observable_length_and_host_snapshot() {
        setup();
        let first = Value::object(HashMap::new());
        let needle = Value::object(HashMap::new());
        let list = node_list(vec![first, needle.clone()]);
        let index_of = w3cos_core::array_value()
            .get_property("prototype")
            .get_property("indexOf");
        assert_eq!(
            index_of.call(list.clone(), vec![needle.clone()]),
            Value::Number(1.0)
        );

        let prototype = Value::object(HashMap::new());
        w3cos_core::object_value().call_method(
            "defineProperty",
            vec![
                prototype.clone(),
                Value::string("length"),
                Value::object(HashMap::from([(
                    "get".to_string(),
                    func(|_, _| Value::Number(1.0)),
                )])),
            ],
        );
        w3cos_core::object_value().call_method("setPrototypeOf", vec![list.clone(), prototype]);
        assert_eq!(list.get_property("length"), Value::Number(1.0));
        assert_eq!(index_of.call(list, vec![needle]), Value::Number(-1.0));
    }

    #[test]
    fn document_fragment_get_element_by_id_and_node_base_uri_are_live() {
        setup();
        let document = document_value();
        let fragment = document.call_method("createDocumentFragment", vec![]);
        let element = document.call_method("createElement", vec![Value::string("div")]);
        element.set_property("id", Value::string("target"));
        fragment.call_method("appendChild", vec![element.clone()]);

        assert!(fragment.call_method("getElementById", vec![Value::string("target")]) == element);
        assert!(
            fragment
                .call_method("getElementById", vec![Value::string("")])
                .is_null()
        );
        assert_eq!(
            element.get_property("baseURI"),
            document.get_property("URL")
        );
        assert_eq!(
            document
                .call_method("createAttribute", vec![Value::string("class")])
                .get_property("baseURI"),
            document.get_property("URL")
        );
    }

    #[test]
    fn window_frames_initializes_connected_iframe_browsing_contexts() {
        setup();
        create_in_body("iframe");

        let frames = window_value().get_property("frames");
        assert_eq!(frames.get_property("length"), Value::Number(1.0));
        assert_eq!(
            frames
                .get_property("0")
                .get_property("document")
                .get_property("nodeType"),
            Value::Number(9.0)
        );
    }

    #[test]
    fn frame_document_adoption_preserves_collection_htmlness() {
        setup();
        let document = document_value();
        let parent = document.call_method("createElement", vec![Value::string("div")]);
        let child1 = document.call_method(
            "createElementNS",
            vec![
                Value::string(crate::html_parser_state::HTML_NAMESPACE),
                Value::string("a"),
            ],
        );
        let child2 = document.call_method(
            "createElementNS",
            vec![
                Value::string(crate::html_parser_state::HTML_NAMESPACE),
                Value::string("A"),
            ],
        );
        let child3 = document.call_method(
            "createElementNS",
            vec![Value::string(""), Value::string("a")],
        );
        let child4 = document.call_method(
            "createElementNS",
            vec![Value::string(""), Value::string("A")],
        );
        for child in [&child1, &child2, &child3, &child4] {
            parent.call_method("appendChild", vec![child.clone()]);
        }
        let old_list = parent.call_method("getElementsByTagName", vec![Value::string("A")]);
        assert_eq!(old_list.get_property("length").to_u32(), 2);
        assert!(
            old_list
                .call_method("hasOwnProperty", vec![Value::Number(0.0)])
                .to_bool()
        );
        assert!(
            old_list
                .call_method("hasOwnProperty", vec![Value::Number(1.0)])
                .to_bool()
        );
        assert!(old_list.get_property("0") == child1);
        assert!(old_list.get_property("1") == child4);

        let iframe = create_in_body("iframe");
        let iframe_node = node_id_of(&iframe).unwrap();
        assert_eq!(
            iframe
                .get_property("contentDocument")
                .get_property("nodeType"),
            Value::Number(9.0)
        );
        let frame_document = parse_frame_document(
            "<root/>",
            "application/xml",
            "https://example.test/frame.xml",
        );
        assert!(
            frame_document
                .get_property("firstChild")
                .get_property("parentNode")
                == frame_document
        );
        install_frame_document(
            iframe_node,
            frame_document.clone(),
            "https://example.test/frame.xml",
        );
        assert_eq!(
            window_value()
                .get_property("frames")
                .get_property("length")
                .to_u32(),
            1
        );
        assert!(
            window_value()
                .get_property("frames")
                .get_property("0")
                .get_property("document")
                == frame_document
        );

        frame_document
            .get_property("documentElement")
            .call_method("appendChild", vec![parent.clone()]);
        assert!(parent.get_property("ownerDocument") == frame_document);
        assert_eq!(old_list.get_property("length").to_u32(), 2);
        assert!(old_list.get_property("0") == child1);
        assert!(old_list.get_property("1") == child4);
        let new_list = parent.call_method("getElementsByTagName", vec![Value::string("A")]);
        assert_eq!(new_list.get_property("length").to_u32(), 2);
        assert!(new_list.get_property("0") == child2);
        assert!(new_list.get_property("1") == child4);
        assert!(child3.get_property("ownerDocument") == frame_document);
    }

    #[test]
    fn parent_move_before_reparents_and_frame_body_projects_for_rendering() {
        setup();
        let document = document_value();
        let source = document.call_method("createElement", vec![Value::string("select")]);
        let target = document.call_method("createElement", vec![Value::string("select")]);
        let option = document.call_method("createElement", vec![Value::string("option")]);
        source.call_method("appendChild", vec![option.clone()]);
        document
            .get_property("body")
            .call_method("appendChild", vec![source]);
        document
            .get_property("body")
            .call_method("appendChild", vec![target.clone()]);
        target.call_method("moveBefore", vec![option.clone(), Value::Null]);
        assert!(option.get_property("parentNode") == target);

        let iframe = create_in_body("iframe");
        let iframe_node = node_id_of(&iframe).unwrap();
        let frame_document = parse_frame_document(
            "<body><span>FRAME-CONTENT</span></body>",
            "text/html",
            "about:blank",
        );
        let frame_span = frame_document.call_method("querySelector", vec![Value::string("span")]);
        assert!(frame_span.get_property("isConnected").to_bool());
        install_frame_document(iframe_node, frame_document, "about:blank");
        fn text_content(component: &w3cos_std::Component) -> String {
            let mut text = match &component.kind {
                w3cos_std::component::ComponentKind::Text { content } => content.clone(),
                _ => String::new(),
            };
            for child in &component.children {
                text.push_str(&text_content(child));
            }
            text
        }
        assert!(text_content(&dom::to_component_tree()).contains("FRAME-CONTENT"));
        document
            .get_property("body")
            .call_method("removeChild", vec![iframe]);
        assert!(frame_span.get_property("isConnected").to_bool());
    }

    #[test]
    fn popover_open_selector_survives_move_before() {
        setup();
        let document = document_value();
        let body = document.get_property("body");
        let old_parent = document.call_method("createElement", vec![Value::string("section")]);
        let new_parent = document.call_method("createElement", vec![Value::string("section")]);
        let popover = document.call_method("createElement", vec![Value::string("div")]);
        popover.call_method(
            "setAttribute",
            vec![Value::string("popover"), Value::string("")],
        );
        old_parent.call_method("appendChild", vec![popover.clone()]);
        body.call_method("appendChild", vec![old_parent]);
        body.call_method("appendChild", vec![new_parent.clone()]);

        popover.call_method("showPopover", vec![]);
        assert!(
            document.call_method("querySelector", vec![Value::string(":popover-open")])
                == popover
        );

        new_parent.call_method("moveBefore", vec![popover.clone(), Value::Null]);
        assert!(
            document.call_method("querySelector", vec![Value::string(":popover-open")])
                == popover
        );
    }

    #[test]
    fn modal_dialog_selector_survives_move_before() {
        setup();
        let document = document_value();
        let body = document.get_property("body");
        let old_parent = document.call_method("createElement", vec![Value::string("section")]);
        let new_parent = document.call_method("createElement", vec![Value::string("section")]);
        let dialog = document.call_method("createElement", vec![Value::string("dialog")]);
        old_parent.call_method("appendChild", vec![dialog.clone()]);
        body.call_method("appendChild", vec![old_parent]);
        body.call_method("appendChild", vec![new_parent.clone()]);

        dialog.call_method("showModal", vec![]);
        assert!(
            dialog
                .call_method("matches", vec![Value::string(":modal")])
                .to_bool()
        );

        new_parent.call_method("moveBefore", vec![dialog.clone(), Value::Null]);
        assert!(
            dialog
                .call_method("matches", vec![Value::string(":modal")])
                .to_bool()
        );
    }

    #[test]
    fn move_before_requires_a_shared_shadow_including_root() {
        setup();
        let document = document_value();
        let connected_destination = create_in_body("div");
        assert_eq!(
            connected_destination
                .get_property("moveBefore")
                .get_property("length")
                .to_u32(),
            2
        );
        let disconnected_origin =
            document.call_method("createElement", vec![Value::string("section")]);
        let disconnected_child =
            document.call_method("createElement", vec![Value::string("p")]);
        disconnected_origin.call_method("appendChild", vec![disconnected_child.clone()]);

        let error = w3cos_core::catch_js(|| {
            connected_destination.call_method(
                "moveBefore",
                vec![disconnected_child.clone(), Value::Null],
            )
        })
        .expect_err("moving between connected and disconnected roots must throw");
        assert_eq!(
            error.get_property("name").to_js_string(),
            "HierarchyRequestError"
        );

        let disconnected_destination =
            document.call_method("createElement", vec![Value::string("aside")]);
        let error = w3cos_core::catch_js(|| {
            disconnected_destination.call_method(
                "moveBefore",
                vec![disconnected_child.clone(), Value::Null],
            )
        })
        .expect_err("moving between unrelated disconnected roots must throw");
        assert_eq!(
            error.get_property("name").to_js_string(),
            "HierarchyRequestError"
        );

        disconnected_origin.call_method("appendChild", vec![disconnected_destination.clone()]);
        disconnected_destination.call_method(
            "moveBefore",
            vec![disconnected_child.clone(), Value::Null],
        );
        assert!(disconnected_child.get_property("parentNode") == disconnected_destination);
    }

    #[test]
    fn virtual_document_move_before_accepts_comments_but_rejects_elements() {
        setup();
        let implementation = document_value().get_property("implementation");
        let document =
            implementation.call_method("createHTMLDocument", vec![Value::Undefined]);
        assert!(document.get_property("isConnected").to_bool());
        let body = document.get_property("body");
        let comment = document.call_method("createComment", vec![Value::string("comment")]);
        body.call_method("appendChild", vec![comment.clone()]);

        document.call_method("moveBefore", vec![comment.clone(), Value::Null]);
        assert!(comment.get_property("parentNode") == document);
        assert!(document.get_property("lastChild") == comment);

        let error = w3cos_core::catch_js(|| {
            document.call_method("moveBefore", vec![body.clone(), Value::Null])
        })
        .expect_err("moving an Element directly into a Document must throw");
        assert_eq!(
            error.get_property("name").to_js_string(),
            "HierarchyRequestError"
        );
    }

    #[test]
    fn compiled_move_before_enforces_required_and_node_arguments() {
        setup();
        crate::dynamic_script::ScriptLoader::new(crate::dynamic_script::ScriptPolicy::default())
            .execute_source(
                r#"
var moving = document.createTextNode("moving");
try {
  document.body.moveBefore(moving);
  window.__moveBeforeMissingThrew = false;
} catch (error) {
  window.__moveBeforeMissingThrew = true;
  window.__moveBeforeMissingName = error.name;
  window.__moveBeforeMissingConstructor = error.constructor === TypeError;
}
try {
  document.body.moveBefore(moving, { invalid: true });
  window.__moveBeforeInvalidReferenceThrew = false;
} catch (error) {
  window.__moveBeforeInvalidReferenceThrew = true;
  window.__moveBeforeInvalidReferenceName = error.name;
  window.__moveBeforeInvalidReferenceConstructor = error.constructor === TypeError;
}
"#,
                "inline:move-before-webidl",
            )
            .unwrap();
        let window = window_value();
        for prefix in ["__moveBeforeMissing", "__moveBeforeInvalidReference"] {
            assert!(window.get_property(&format!("{prefix}Threw")).to_bool());
            assert_eq!(
                window.get_property(&format!("{prefix}Name")).to_js_string(),
                "TypeError"
            );
            assert!(
                window
                    .get_property(&format!("{prefix}Constructor"))
                    .to_bool()
            );
        }
    }

    #[test]
    fn tree_mutation_methods() {
        setup();
        let doc = document_value();
        let a = create_in_body("div");
        a.call_method(
            "setAttribute",
            vec![Value::string("id"), Value::string("a")],
        );
        let b = doc.call_method("createElement", vec![Value::string("div")]);
        let c = doc.call_method("createElement", vec![Value::string("div")]);
        let body = doc.get_property("body");
        body.call_method("insertBefore", vec![b.clone(), a.clone()]);
        let body_id = dom::body_id();
        assert_eq!(node_id_of(&b).unwrap(), dom::children(body_id)[0]);
        body.call_method("replaceChild", vec![c.clone(), b.clone()]);
        assert_eq!(node_id_of(&c).unwrap(), dom::children(body_id)[0]);
        let clone = a.call_method("cloneNode", vec![Value::Bool(true)]);
        assert!(clone != a);
        assert_eq!(clone.get_property("id").to_js_string(), "a");
        c.call_method("remove", vec![]);
        assert_eq!(dom::children(body_id).len(), 1);
        let removed = body.call_method("removeChild", vec![a.clone()]);
        assert!(removed == a);
        assert_eq!(dom::children(body_id).len(), 0);
    }

    #[test]
    fn character_data_remove_has_child_node_shape_and_detaches() {
        setup();
        let document = document_value();
        let text = document.call_method("createTextNode", vec![Value::string("text")]);
        let parent = document.call_method("createElement", vec![Value::string("div")]);
        let remove = text.get_property("remove");
        assert!(Value::string("remove").js_in(&text).to_bool());
        assert!(remove.is_function());
        assert_eq!(remove.get_property("length"), Value::Number(0.0));
        assert!(text.get_property("parentNode").is_null());
        assert!(text.call_method("remove", vec![]).is_undefined());
        parent.call_method("appendChild", vec![text.clone()]);
        assert!(text.get_property("parentNode") == parent);
        assert!(text.call_method("remove", vec![]).is_undefined());
        assert!(text.get_property("parentNode").is_null());
        assert_eq!(
            parent
                .get_property("childNodes")
                .get_property("length")
                .to_u32(),
            0
        );
    }

    #[test]
    fn document_import_adopt_and_named_node_map_are_live() {
        setup();
        let doc = document_value();
        let body = doc.get_property("body");
        let source = doc.call_method("createElement", vec![Value::string("section")]);
        source.call_method(
            "setAttribute",
            vec![Value::string("data-route"), Value::string("inbox")],
        );
        source.call_method(
            "appendChild",
            vec![doc.call_method("createTextNode", vec![Value::string("message")])],
        );
        body.call_method("appendChild", vec![source.clone()]);

        let attributes = source.get_property("attributes");
        let first = attributes.call_method("item", vec![Value::Number(0.0)]);
        assert_eq!(first.get_property("name").to_js_string(), "data-route");
        assert_eq!(first.get_property("value").to_js_string(), "inbox");
        assert_eq!(
            attributes
                .call_method("getNamedItem", vec![Value::string("data-route")])
                .get_property("value")
                .to_js_string(),
            "inbox"
        );
        assert!(
            attributes
                .call_method("item", vec![Value::Number(1.0)])
                .is_null()
        );

        let imported = doc.call_method("importNode", vec![source.clone(), Value::Bool(true)]);
        assert!(imported != source);
        assert!(imported.get_property("parentNode").is_null());
        assert_eq!(
            imported.get_property("textContent").to_js_string(),
            "message"
        );
        assert_eq!(
            imported
                .call_method("getAttribute", vec![Value::string("data-route")])
                .to_js_string(),
            "inbox"
        );

        let adopted = doc.call_method("adoptNode", vec![source.clone()]);
        assert!(adopted == source);
        assert!(source.get_property("parentNode").is_null());
    }

    #[test]
    fn named_node_map_preserves_inserted_attribute_identity_and_reserved_members() {
        setup();
        let document = document_value();
        let element = document.call_method("createElement", vec![Value::string("div")]);
        let attributes = element.get_property("attributes");
        let attribute = document.call_method("createAttribute", vec![Value::string("route")]);

        assert!(
            attributes
                .call_method("setNamedItem", vec![attribute.clone()])
                .is_null()
        );
        assert!(attributes.get_property("route") == attribute);
        assert_eq!(attributes.get_property("length").to_u32(), 1);
        assert!(
            attributes.get_property("item")
                == crate::dom_constructors::prototype("NamedNodeMap").get_property("item")
        );
        assert!(
            attributes.call_method("removeNamedItem", vec![Value::string("route")]) == attribute
        );
        assert!(attributes.get_property("route").is_undefined());
        assert_eq!(attributes.get_property("length").to_u32(), 0);

        element.call_method(
            "setAttributeNS",
            vec![
                Value::string("urn:reserved"),
                Value::string("toString"),
                Value::string("attribute"),
            ],
        );
        assert!(
            element
                .get_property("attributes")
                .get_property("toString")
                .is_function()
        );
    }

    #[test]
    fn inner_html_read() {
        setup();
        let doc = document_value();
        let div = create_in_body("div");
        let span = doc.call_method("createElement", vec![Value::string("span")]);
        span.set_property("textContent", Value::string("x"));
        div.call_method("appendChild", vec![span]);
        let html = div.get_property("innerHTML").to_js_string();
        assert_eq!(html, "<span>x</span>");
        div.set_property("innerHTML", Value::string(""));
        assert_eq!(
            div.get_property("childNodes")
                .get_property("length")
                .to_number(),
            0.0
        );
    }

    #[test]
    fn inner_html_parses_nested_markup_and_adjacent_siblings() {
        setup();
        let div = create_in_body("div");
        div.set_property(
            "innerHTML",
            Value::string(
                r#"<div class="view-line" style="top: 19px"><span data-x="1">&lt;x&gt;</span></div>"#,
            ),
        );

        let line = div.call_method("querySelector", vec![Value::string(".view-line")]);
        assert!(!line.is_null());
        assert_eq!(
            line.get_property("style")
                .get_property("top")
                .to_js_string(),
            "19px"
        );
        let span = line.call_method("querySelector", vec![Value::string("span")]);
        assert_eq!(span.get_property("textContent").to_js_string(), "<x>");
        assert_eq!(
            span.call_method("getAttribute", vec![Value::string("data-x")])
                .to_js_string(),
            "1"
        );

        line.call_method(
            "insertAdjacentHTML",
            vec![
                Value::string("afterend"),
                Value::string(r#"<div class="view-line">second</div>"#),
            ],
        );
        assert_eq!(
            div.call_method("querySelectorAll", vec![Value::string(".view-line")])
                .get_property("length")
                .to_number(),
            2.0
        );
        assert_eq!(div.get_property("textContent").to_js_string(), "<x>second");
    }

    #[test]
    fn event_listener_fires_via_native_dispatch() {
        setup();
        let btn = create_in_body("button");
        let btn_id = node_id_of(&btn).unwrap();

        let seen: Rc<RefCell<Vec<(String, f64, f64)>>> = Rc::new(RefCell::new(Vec::new()));
        let seen2 = seen.clone();
        let handler = func(move |_, args| {
            let ev = arg(&args, 0);
            seen2.borrow_mut().push((
                ev.get_property("type").to_js_string(),
                ev.get_property("clientX").to_number(),
                ev.get_property("clientY").to_number(),
            ));
            // The event target must be a real element value.
            assert!(node_id_of(&ev.get_property("target")).is_some());
            assert!(w3cos_core::class::instance_of(
                &ev,
                &window_value().get_property("MouseEvent")
            ));
            assert!(w3cos_core::class::instance_of(
                &ev,
                &window_value().get_property("Event")
            ));
            Value::Undefined
        });
        btn.call_method("addEventListener", vec![Value::string("click"), handler]);

        // Fire through the w3cos-dom dispatch path (as native input would).
        dom::with_document_mut(|doc| {
            let mut ev = Event::click(NodeId::from_u32(btn_id), 12.0, 34.0);
            doc.dispatch_event_bubbling(&mut ev);
        });
        // Delivery is deferred to the drain step.
        assert!(seen.borrow().is_empty());
        let delivered = drain_microtasks();
        assert_eq!(delivered, 1);
        assert_eq!(
            seen.borrow().as_slice(),
            &[("click".to_string(), 12.0, 34.0)]
        );
    }

    #[test]
    fn inner_html_reuses_streaming_tree_builder_and_keeps_scripts_inert() {
        setup();
        let host = create_in_body("section");
        host.set_property(
            "innerHTML",
            Value::string(
                "<b>one<div id=block>two</b>three</div>\
                 <svg><lineargradient id=gradient viewbox='0 0 1 1'/></svg>\
                 <script>document.body.setAttribute('data-fragment-script', 'ran')</script>",
            ),
        );

        let bold = host.call_method("querySelectorAll", vec![Value::string("b")]);
        assert_eq!(bold.get_property("length").to_u32(), 2);
        assert_eq!(
            host.call_method("querySelector", vec![Value::string("#block")])
                .get_property("textContent")
                .to_js_string(),
            "twothree"
        );
        let gradient = host.call_method("querySelector", vec![Value::string("#gradient")]);
        assert_eq!(
            gradient.get_property("localName").to_js_string(),
            "linearGradient"
        );
        assert_eq!(
            gradient
                .call_method("getAttribute", vec![Value::string("viewBox")])
                .to_js_string(),
            "0 0 1 1"
        );
        assert_eq!(
            document_value()
                .get_property("body")
                .call_method("getAttribute", vec![Value::string("data-fragment-script")]),
            Value::Null
        );

        host.call_method(
            "setHTML",
            vec![Value::string(
                "<script>bad()</script><a id=safe onclick=bad() href='javascript:bad()'>ok</a>",
            )],
        );
        assert!(
            host.call_method("querySelector", vec![Value::string("script")])
                .is_null()
        );
        let safe = host.call_method("querySelector", vec![Value::string("#safe")]);
        assert_eq!(
            safe.call_method("getAttribute", vec![Value::string("onclick")]),
            Value::Null
        );
        assert_eq!(
            safe.call_method("getAttribute", vec![Value::string("href")]),
            Value::Null
        );
        assert_eq!(safe.get_property("textContent").to_js_string(), "ok");
    }

    #[test]
    fn native_touch_dispatches_pointer_and_touch_lifecycles() {
        setup();
        let target = create_in_body("div");
        let target_id = node_id_of(&target).unwrap();
        let touch_log = Rc::new(RefCell::new(Vec::<String>::new()));
        let primary_log = Rc::new(RefCell::new(Vec::<bool>::new()));

        let primary_for_handler = Rc::clone(&primary_log);
        target.call_method(
            "addEventListener",
            vec![
                Value::string("pointerdown"),
                func(move |_, args| {
                    primary_for_handler
                        .borrow_mut()
                        .push(args[0].get_property("isPrimary").to_bool());
                    Value::Undefined
                }),
            ],
        );
        for event_type in ["touchstart", "touchmove", "touchend", "touchcancel"] {
            let log = Rc::clone(&touch_log);
            let expected_target = target.clone();
            target.call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    func(move |_, args| {
                        let event = args[0].clone();
                        let touches = event.get_property("touches");
                        let target_touches = event.get_property("targetTouches");
                        let changed = event.get_property("changedTouches");
                        let changed_touch = changed.call_method("item", vec![Value::Number(0.0)]);
                        assert!(changed_touch.get_property("target") == expected_target);
                        assert!(w3cos_core::class::instance_of(
                            &changed_touch,
                            &window_value().get_property("Touch")
                        ));
                        assert!(w3cos_core::class::instance_of(
                            &event,
                            &window_value().get_property("TouchEvent")
                        ));
                        for list in [&touches, &target_touches, &changed] {
                            assert!(w3cos_core::class::instance_of(
                                list,
                                &window_value().get_property("TouchList")
                            ));
                        }
                        assert!(event.get_property("isTrusted").to_bool());
                        log.borrow_mut().push(format!(
                            "{}:{}:{}:{}:{}:{}",
                            event.get_property("type").to_js_string(),
                            touches.get_property("length").to_number(),
                            target_touches.get_property("length").to_number(),
                            changed.get_property("length").to_number(),
                            changed_touch.get_property("identifier").to_number(),
                            event.get_property("cancelable").to_bool()
                        ));
                        if event.get_property("type").to_js_string() == "touchmove" {
                            event.call_method("preventDefault", vec![]);
                        }
                        Value::Undefined
                    }),
                ],
            );
        }

        assert!(!dispatch_native_pointer(
            target_id, "down", 10.0, 20.0, 11, "touch", 0, 1, 0.5, true, false, false, false,
            false,
        ));
        assert!(!dispatch_native_pointer(
            target_id, "down", 30.0, 40.0, 12, "touch", 0, 1, 0.7, true, false, false, false,
            false,
        ));
        assert!(dispatch_native_pointer(
            target_id, "move", 15.0, 25.0, 11, "touch", -1, 1, 0.6, true, false, false, false,
            false,
        ));
        assert!(!dispatch_native_pointer(
            target_id, "up", 15.0, 25.0, 11, "touch", 0, 0, 0.0, true, false, false, false, false,
        ));
        assert!(!dispatch_native_pointer(
            target_id, "cancel", 30.0, 40.0, 12, "touch", -1, 0, 0.0, true, false, false, false,
            false,
        ));

        assert_eq!(primary_log.borrow().as_slice(), &[true, false]);
        assert_eq!(
            touch_log.borrow().as_slice(),
            &[
                "touchstart:1:1:1:11:true",
                "touchstart:2:2:1:12:true",
                "touchmove:2:2:1:11:true",
                "touchend:1:1:1:11:true",
                "touchcancel:0:0:1:12:false",
            ]
        );
    }

    #[test]
    fn hit_tested_touch_uses_layout_boxes_and_retargets_active_contacts() {
        setup();
        let target = create_in_body("div");
        let target_id = node_id_of(&target).unwrap();
        let miss_log = Rc::new(RefCell::new(Vec::<String>::new()));
        let hit_log = Rc::new(RefCell::new(Vec::<String>::new()));
        dom::with_document_mut(|tree| {
            tree.set_layout_rect(
                NodeId::from_u32(target_id),
                w3cos_dom::DOMRect::new(10.0, 10.0, 40.0, 40.0),
            );
        });
        let log = Rc::clone(&hit_log);
        target.call_method(
            "addEventListener",
            vec![
                Value::string("touchstart"),
                func(move |_, args| {
                    log.borrow_mut()
                        .push(args[0].get_property("type").to_js_string());
                    Value::Undefined
                }),
            ],
        );
        let log = Rc::clone(&hit_log);
        target.call_method(
            "addEventListener",
            vec![
                Value::string("touchend"),
                func(move |_, args| {
                    log.borrow_mut()
                        .push(args[0].get_property("type").to_js_string());
                    Value::Undefined
                }),
            ],
        );
        document_value().call_method(
            "addEventListener",
            vec![
                Value::string("touchstart"),
                func({
                    let miss_log = Rc::clone(&miss_log);
                    move |_, args| {
                        miss_log
                            .borrow_mut()
                            .push(args[0].get_property("type").to_js_string());
                        Value::Undefined
                    }
                }),
            ],
        );

        assert!(!dispatch_hit_tested_touch("down", 0.0, 0.0, 21, 0.5));
        assert!(!dispatch_hit_tested_touch("down", 1000.0, 1000.0, 21, 0.5));
        assert!(miss_log.borrow().is_empty());
        assert!(!dispatch_hit_tested_touch("down", 20.0, 20.0, 21, 0.5));
        assert!(!dispatch_hit_tested_touch("move", 0.0, 0.0, 21, 0.5));
        assert!(!dispatch_hit_tested_touch("up", 0.0, 0.0, 21, 0.0));
        assert_eq!(hit_log.borrow().as_slice(), &["touchstart", "touchend"]);
    }

    #[test]
    fn pointer_capture_retargets_until_implicit_release() {
        setup();
        let first = create_in_body("div");
        let second = create_in_body("div");
        let first_id = node_id_of(&first).unwrap();
        let second_id = node_id_of(&second).unwrap();
        let log = Rc::new(RefCell::new(Vec::<String>::new()));

        let down_log = Rc::clone(&log);
        let first_for_down = first.clone();
        first.call_method(
            "addEventListener",
            vec![
                Value::string("pointerdown"),
                func(move |_, args| {
                    down_log.borrow_mut().push("down".to_string());
                    first_for_down
                        .call_method("setPointerCapture", vec![args[0].get_property("pointerId")]);
                    assert!(
                        first_for_down
                            .call_method("hasPointerCapture", vec![Value::Number(41.0)])
                            .to_bool()
                    );
                    Value::Undefined
                }),
            ],
        );
        for event_type in [
            "gotpointercapture",
            "pointermove",
            "pointerup",
            "lostpointercapture",
        ] {
            let event_log = Rc::clone(&log);
            let expected_target = first.clone();
            first.call_method(
                "addEventListener",
                vec![
                    Value::string(event_type),
                    func(move |_, args| {
                        let event = args[0].clone();
                        assert!(event.get_property("target") == expected_target);
                        assert_eq!(event.get_property("pointerId").to_number(), 41.0);
                        event_log
                            .borrow_mut()
                            .push(event.get_property("type").to_js_string());
                        Value::Undefined
                    }),
                ],
            );
        }

        assert!(!dispatch_native_pointer(
            first_id, "down", 1.0, 2.0, 41, "pen", 0, 1, 0.5, true, false, false, false, false,
        ));
        assert!(!dispatch_native_pointer(
            second_id, "move", 20.0, 30.0, 41, "pen", -1, 1, 0.6, true, false, false, false, false,
        ));
        assert!(!dispatch_native_pointer(
            second_id, "up", 20.0, 30.0, 41, "pen", 0, 0, 0.0, true, false, false, false, false,
        ));

        assert!(
            !first
                .call_method("hasPointerCapture", vec![Value::Number(41.0)])
                .to_bool()
        );
        assert_eq!(
            log.borrow().as_slice(),
            &[
                "down",
                "gotpointercapture",
                "pointermove",
                "pointerup",
                "lostpointercapture",
            ]
        );
    }

    #[test]
    fn submit_button_dispatches_submit_on_ancestor_form() {
        setup();
        let doc = document_value();
        let form = create_in_body("form");
        let button = doc.call_method("createElement", vec![Value::string("button")]);
        let caption = doc.call_method("createElement", vec![Value::string("span")]);
        button.call_method("appendChild", vec![caption.clone()]);
        form.call_method("appendChild", vec![button.clone()]);

        let submissions = Rc::new(Cell::new(0));
        let submissions2 = submissions.clone();
        form.call_method(
            "addEventListener",
            vec![
                Value::string("submit"),
                func(move |_, args| {
                    submissions2.set(submissions2.get() + 1);
                    arg(&args, 0).call_method("preventDefault", vec![]);
                    Value::Undefined
                }),
            ],
        );

        let button_id = node_id_of(&button).unwrap();
        let caption_id = node_id_of(&caption).unwrap();
        assert_eq!(dispatch_native_submit_for_control(button_id), Some(true));
        assert_eq!(submissions.get(), 1);
        assert_eq!(dispatch_native_submit_for_control(caption_id), Some(true));
        assert_eq!(submissions.get(), 2);

        button.call_method(
            "setAttribute",
            vec![Value::string("type"), Value::string("button")],
        );
        assert_eq!(dispatch_native_submit_for_control(button_id), None);
        assert_eq!(dispatch_native_submit_for_control(caption_id), None);
        assert_eq!(submissions.get(), 2);
    }

    #[test]
    fn form_associated_controls_expose_their_live_form_owner() {
        setup();
        let document = document_value();
        let form = create_in_body("form");
        form.call_method(
            "setAttribute",
            vec![Value::string("id"), Value::string("owner")],
        );
        let nested_button = document.call_method("createElement", vec![Value::string("button")]);
        form.call_method("appendChild", vec![nested_button.clone()]);
        assert!(nested_button.get_property("form").strict_eq(&form));

        let external_button =
            document.call_method("createElement", vec![Value::string("button")]);
        external_button.call_method(
            "setAttribute",
            vec![Value::string("form"), Value::string("owner")],
        );
        document
            .get_property("body")
            .call_method("appendChild", vec![external_button.clone()]);
        assert!(external_button.get_property("form").strict_eq(&form));
    }

    #[test]
    fn duplicate_form_ids_resolve_the_owner_in_tree_order_after_move_before() {
        setup();
        let document = document_value();
        let body = document.get_property("body");
        let form1 = document.call_method("createElement", vec![Value::string("form")]);
        form1.set_property("id", Value::string("owner"));
        body.call_method("appendChild", vec![form1.clone()]);
        let button = document.call_method("createElement", vec![Value::string("button")]);
        button.call_method(
            "setAttribute",
            vec![Value::string("form"), Value::string("owner")],
        );
        body.call_method("appendChild", vec![button.clone()]);
        let form2 = document.call_method("createElement", vec![Value::string("form")]);
        form2.set_property("id", Value::string("owner"));
        body.call_method("appendChild", vec![form2.clone()]);

        assert!(button.get_property("form").strict_eq(&form1));
        body.call_method("moveBefore", vec![form2.clone(), form1]);
        assert!(button.get_property("form").strict_eq(&form2));
    }

    #[test]
    fn option_selectedness_and_selected_index_follow_move_before() {
        setup();
        let document = document_value();
        let body = document.get_property("body");
        let select = document.call_method("createElement", vec![Value::string("select")]);
        let group = document.call_method("createElement", vec![Value::string("optgroup")]);
        let option_a = document.call_method("createElement", vec![Value::string("option")]);
        let option_b = document.call_method("createElement", vec![Value::string("option")]);
        let option_c = document.call_method("createElement", vec![Value::string("option")]);
        select.call_method("appendChild", vec![option_a.clone()]);
        group.call_method("appendChild", vec![option_b.clone()]);
        select.call_method("appendChild", vec![group]);
        select.call_method("appendChild", vec![option_c.clone()]);
        body.call_method("appendChild", vec![select.clone()]);

        assert!(option_a.get_property("selected").to_bool());
        assert_eq!(select.get_property("selectedIndex"), Value::Number(0.0));
        body.call_method("moveBefore", vec![option_a.clone(), Value::Null]);
        assert!(option_a.get_property("selected").to_bool());
        assert!(option_b.get_property("selected").to_bool());
        body.call_method("moveBefore", vec![option_b.clone(), Value::Null]);
        assert!(option_b.get_property("selected").to_bool());
        assert!(option_c.get_property("selected").to_bool());
        select.call_method("moveBefore", vec![option_a.clone(), option_c.clone()]);
        assert!(option_a.get_property("selected").to_bool());
        assert!(!option_c.get_property("selected").to_bool());
        assert_eq!(select.get_property("selectedIndex"), Value::Number(0.0));

        option_c.set_property("selected", Value::Bool(true));
        assert_eq!(select.get_property("selectedIndex"), Value::Number(1.0));
        select.call_method("moveBefore", vec![option_c, option_a]);
        assert_eq!(select.get_property("selectedIndex"), Value::Number(0.0));
    }

    #[test]
    fn moved_display_contents_child_becomes_a_sized_flex_item() {
        setup();
        let document = document_value();
        let body = document.get_property("body");
        let parent = document.call_method("createElement", vec![Value::string("div")]);
        parent.set_property("id", Value::string("new_parent"));
        let parent_style = parent.get_property("style");
        parent_style.set_property("width", Value::string("100px"));
        parent_style.set_property("display", Value::string("flex"));
        let old_parent = document.call_method("createElement", vec![Value::string("div")]);
        let wrapper = document.call_method("createElement", vec![Value::string("div")]);
        wrapper.set_property("id", Value::string("mv"));
        wrapper
            .get_property("style")
            .set_property("display", Value::string("contents"));
        let child = document.call_method("createElement", vec![Value::string("div")]);
        let child_style = child.get_property("style");
        child_style.set_property("display", Value::string("inline"));
        child_style.set_property("background", Value::string("green"));
        child_style.set_property("height", Value::string("100px"));
        child_style.set_property("flexGrow", Value::string("1"));
        let child_id = node_id_of(&child).unwrap();
        wrapper.call_method("appendChild", vec![child]);
        old_parent.call_method("appendChild", vec![wrapper]);
        body.call_method("appendChild", vec![parent]);
        body.call_method("appendChild", vec![old_parent]);

        crate::dynamic_script::ScriptLoader::new(crate::dynamic_script::ScriptPolicy::default())
            .execute_source("new_parent.moveBefore(mv, null);", "inline:flex-move")
            .unwrap();

        let tree = crate::dom::to_component_tree();
        let flat = crate::layout::pre_flatten(&tree);
        let child_index = flat
            .iter()
            .position(|entry| {
                matches!(
                    entry.on_click,
                    w3cos_std::EventAction::NativeHost { id, .. }
                        if *id == u64::from(child_id)
                )
            })
            .expect("promoted child component");
        let rect = crate::layout::compute(&tree, 800.0, 600.0)
            .unwrap()
            .into_iter()
            .find_map(|(rect, index)| (index == child_index).then_some(rect))
            .expect("promoted child layout");
        assert_eq!((rect.width, rect.height), (100.0, 100.0));
    }

    #[test]
    fn live_range_snaps_to_old_parent_when_ancestor_moves() {
        setup();
        let document = document_value();
        let body = document.get_property("body");
        let old_parent = document.call_method("createElement", vec![Value::string("div")]);
        let movable = document.call_method("createElement", vec![Value::string("div")]);
        let start = document.call_method("createElement", vec![Value::string("span")]);
        let middle = document.call_method("createElement", vec![Value::string("span")]);
        let end = document.call_method("createElement", vec![Value::string("span")]);
        let new_parent = document.call_method("createElement", vec![Value::string("div")]);
        movable.call_method("appendChild", vec![start.clone()]);
        movable.call_method("appendChild", vec![middle.clone()]);
        old_parent.call_method("appendChild", vec![movable.clone()]);
        old_parent.call_method("appendChild", vec![end.clone()]);
        body.call_method("appendChild", vec![old_parent.clone()]);
        body.call_method("appendChild", vec![new_parent.clone()]);

        let range = document.call_method("createRange", vec![]);
        range.call_method("setStart", vec![start, Value::Number(0.0)]);
        range.call_method("setEnd", vec![end.clone(), Value::Number(0.0)]);
        assert_eq!(
            range.call_method("intersectsNode", vec![middle.clone()]),
            Value::Bool(true)
        );

        new_parent.call_method("moveBefore", vec![movable, Value::Null]);

        assert!(range.get_property("startContainer").strict_eq(&old_parent));
        assert!(range.get_property("endContainer").strict_eq(&end));
        assert_eq!(
            range.call_method("intersectsNode", vec![middle]),
            Value::Bool(false)
        );
    }

    #[test]
    fn document_create_event_uses_legacy_aliases_and_empty_initial_state() {
        setup();
        let document = document_value();
        for (alias, interface) in [
            ("EVENTS", "Event"),
            ("MouseEvents", "MouseEvent"),
            ("uievents", "UIEvent"),
            ("CustomEvent", "CustomEvent"),
        ] {
            let event = document.call_method("createEvent", vec![Value::string(alias)]);
            assert!(w3cos_core::class::instance_of(
                &event,
                &window_value().get_property(interface)
            ));
            assert_eq!(event.get_property("type"), Value::string(""));
            assert_eq!(event.get_property("target"), Value::Null);
            assert_eq!(event.get_property("eventPhase"), Value::Number(0.0));
            assert_eq!(event.get_property("bubbles"), Value::Bool(false));
            assert_eq!(event.get_property("cancelable"), Value::Bool(false));
            assert_eq!(event.get_property("defaultPrevented"), Value::Bool(false));
            assert_eq!(event.get_property("isTrusted"), Value::Bool(false));
        }
    }

    #[test]
    fn dispatch_event_sync_bubbles_and_cancels() {
        setup();
        let doc = document_value();
        let parent = create_in_body("div");
        let child = doc.call_method("createElement", vec![Value::string("span")]);
        parent.call_method("appendChild", vec![child.clone()]);

        let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let log2 = log.clone();
        child.call_method(
            "addEventListener",
            vec![
                Value::string("custom-event"),
                func(move |_, args| {
                    let ev = arg(&args, 0);
                    assert_eq!(ev.get_property("customMarker").to_js_string(), "preserved");
                    ev.set_property("listenerMarker", Value::Bool(true));
                    log2.borrow_mut().push(format!(
                        "child:{}:{}",
                        ev.get_property("type").to_js_string(),
                        ev.get_property("detail").to_js_string()
                    ));
                    Value::Undefined
                }),
            ],
        );
        let log3 = log.clone();
        parent.call_method(
            "addEventListener",
            vec![
                Value::string("custom-event"),
                func(move |_, args| {
                    let ev = arg(&args, 0);
                    log3.borrow_mut().push("parent".to_string());
                    ev.call_method("stopPropagation", vec![]);
                    Value::Undefined
                }),
            ],
        );
        let log4 = log.clone();
        doc.call_method(
            "addEventListener",
            vec![
                Value::string("custom-event"),
                func(move |_, _| {
                    log4.borrow_mut().push("document".to_string());
                    Value::Undefined
                }),
            ],
        );

        let mut ev_props = HashMap::new();
        ev_props.insert("type".to_string(), Value::string("custom-event"));
        ev_props.insert("detail".to_string(), Value::string("payload"));
        ev_props.insert("bubbles".to_string(), Value::Bool(true));
        ev_props.insert("customMarker".to_string(), Value::string("preserved"));
        let ev = Value::object(ev_props);
        let not_canceled = child
            .call_method("dispatchEvent", vec![ev.clone()])
            .to_bool();
        assert!(not_canceled);
        assert!(ev.get_property("listenerMarker").to_bool());
        // Synchronous: child (target) then parent (bubble); stopPropagation on
        // parent prevents the document listener.
        assert_eq!(
            log.borrow().as_slice(),
            &[
                "child:custom-event:payload".to_string(),
                "parent".to_string()
            ]
        );

        // preventDefault makes dispatchEvent return false.
        child.call_method(
            "addEventListener",
            vec![
                Value::string("cancel-me"),
                func(move |_, args| {
                    arg(&args, 0).call_method("preventDefault", vec![]);
                    Value::Undefined
                }),
            ],
        );
        let event_constructor = window_value().get_property("Event");
        let event = w3cos_core::class::construct(
            &event_constructor,
            vec![
                Value::string("cancel-me"),
                Value::object(HashMap::from([(
                    "cancelable".to_string(),
                    Value::Bool(true),
                )])),
            ],
        );
        let canceled = !child
            .call_method("dispatchEvent", vec![event.clone()])
            .to_bool();
        assert!(canceled);
        assert!(event.get_property("defaultPrevented").to_bool());
    }

    #[test]
    fn remove_event_listener_stops_delivery() {
        setup();
        let btn = create_in_body("button");
        let btn_id = node_id_of(&btn).unwrap();
        let count = Rc::new(Cell::new(0));
        let count2 = count.clone();
        btn.call_method(
            "addEventListener",
            vec![
                Value::string("click"),
                func(move |_, _| {
                    count2.set(count2.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        btn.call_method("removeEventListener", vec![Value::string("click")]);
        dom::with_document_mut(|doc| {
            let mut ev = Event::click(NodeId::from_u32(btn_id), 0.0, 0.0);
            doc.dispatch_event_bubbling(&mut ev);
        });
        drain_microtasks();
        assert_eq!(count.get(), 0);
    }

    #[test]
    fn set_timeout_via_window() {
        setup();
        let win = window_value();
        let fired = Rc::new(Cell::new(0));
        let fired2 = fired.clone();
        let id = win
            .call_method(
                "setTimeout",
                vec![
                    func(move |_, args| {
                        fired2.set(fired2.get() + 1);
                        // Extra args are passed through.
                        assert_eq!(arg(&args, 0).to_js_string(), "x");
                        Value::Undefined
                    }),
                    Value::Number(5.0),
                    Value::string("x"),
                ],
            )
            .to_u32();
        assert!(id > 0);
        assert!(has_pending_work());
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(tick_timers(), 1);
        assert_eq!(fired.get(), 1);
        // One-shot: no further fires.
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(tick_timers(), 0);

        // clearTimeout cancels.
        let id2 = win.call_method(
            "setTimeout",
            vec![func(|_, _| Value::Undefined), Value::Number(1.0)],
        );
        win.call_method("clearTimeout", vec![id2]);
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(tick_timers(), 0);
    }

    #[test]
    fn idle_callback_receives_deadline_and_can_be_cancelled() {
        setup();
        let window = window_value();
        let calls = Rc::new(Cell::new(0));
        let calls_for_callback = Rc::clone(&calls);
        window.call_method(
            "requestIdleCallback",
            vec![
                func(move |_, args| {
                    let deadline = args[0].clone();
                    assert!(w3cos_core::class::instance_of(
                        &deadline,
                        &window_value().get_property("IdleDeadline")
                    ));
                    assert!(deadline.get_property("didTimeout").to_bool());
                    let remaining = deadline.call_method("timeRemaining", vec![]).to_number();
                    assert!((0.0..=50.0).contains(&remaining));
                    calls_for_callback.set(calls_for_callback.get() + 1);
                    Value::Undefined
                }),
                Value::object(HashMap::from([("timeout".to_string(), Value::Number(0.0))])),
            ],
        );
        let cancelled = window.call_method(
            "requestIdleCallback",
            vec![func(|_, _| panic!("cancelled idle callback must not run"))],
        );
        window.call_method("cancelIdleCallback", vec![cancelled]);
        assert_eq!(tick_timers(), 1);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn set_interval_repeats_and_clears() {
        setup();
        let win = window_value();
        let fired = Rc::new(Cell::new(0));
        let fired2 = fired.clone();
        let id = win.call_method(
            "setInterval",
            vec![
                func(move |_, _| {
                    fired2.set(fired2.get() + 1);
                    Value::Undefined
                }),
                Value::Number(2.0),
            ],
        );
        std::thread::sleep(Duration::from_millis(10));
        let _ = tick_timers();
        assert!(fired.get() >= 1);
        win.call_method("clearInterval", vec![id]);
        let before = fired.get();
        std::thread::sleep(Duration::from_millis(6));
        let _ = tick_timers();
        assert_eq!(fired.get(), before);
    }

    #[test]
    fn request_animation_frame_via_window() {
        setup();
        let win = window_value();
        let timestamps = Rc::new(RefCell::new(Vec::new()));
        let timestamps1 = timestamps.clone();
        win.call_method(
            "requestAnimationFrame",
            vec![func(move |_, args| {
                timestamps1.borrow_mut().push(arg(&args, 0).to_number());
                Value::Undefined
            })],
        );
        let timestamps2 = timestamps.clone();
        win.call_method(
            "requestAnimationFrame",
            vec![func(move |_, args| {
                timestamps2.borrow_mut().push(arg(&args, 0).to_number());
                Value::Undefined
            })],
        );

        assert_eq!(tick_timers(), 0, "timer ticks must not execute rAF");
        assert!(has_pending_animation_frame());
        assert_eq!(run_animation_frame(), 2);
        assert_eq!(timestamps.borrow().len(), 2);
        assert!(timestamps.borrow()[0] >= 0.0);
        assert_eq!(
            timestamps.borrow()[0],
            timestamps.borrow()[1],
            "one rAF batch must share one timestamp"
        );
        assert!(!has_pending_animation_frame());
    }

    #[test]
    fn animation_frame_requested_during_callback_waits_for_next_frame() {
        setup();
        let frames = Rc::new(RefCell::new(Vec::new()));
        let first_frames = frames.clone();
        window_value().call_method(
            "requestAnimationFrame",
            vec![func(move |_, args| {
                first_frames
                    .borrow_mut()
                    .push((1, arg(&args, 0).to_number()));
                let second_frames = first_frames.clone();
                window_value().call_method(
                    "requestAnimationFrame",
                    vec![func(move |_, args| {
                        second_frames
                            .borrow_mut()
                            .push((2, arg(&args, 0).to_number()));
                        Value::Undefined
                    })],
                );
                Value::Undefined
            })],
        );

        assert_eq!(run_animation_frame(), 1);
        assert_eq!(frames.borrow().len(), 1);
        assert_eq!(frames.borrow()[0].0, 1);
        assert!(has_pending_animation_frame());
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(run_animation_frame(), 1);
        assert_eq!(frames.borrow().len(), 2);
        assert_eq!(frames.borrow()[1].0, 2);
        assert!(
            frames.borrow()[1].1 >= frames.borrow()[0].1,
            "rAF timestamps must be monotonic across frames"
        );
    }

    #[test]
    fn bridge_reset_invalidates_old_realm_callback_queues() {
        setup();
        let calls = Rc::new(Cell::new(0_u32));
        let window = window_value();

        let microtask_calls = Rc::clone(&calls);
        window.call_method(
            "queueMicrotask",
            vec![func(move |_, _| {
                microtask_calls.set(microtask_calls.get() + 1);
                Value::Undefined
            })],
        );
        let timer_calls = Rc::clone(&calls);
        window.call_method(
            "setTimeout",
            vec![
                func(move |_, _| {
                    timer_calls.set(timer_calls.get() + 1);
                    Value::Undefined
                }),
                Value::Number(0.0),
            ],
        );
        let animation_calls = Rc::clone(&calls);
        window.call_method(
            "requestAnimationFrame",
            vec![func(move |_, _| {
                animation_calls.set(animation_calls.get() + 1);
                Value::Undefined
            })],
        );

        let queued_promise_calls = Rc::clone(&calls);
        w3cos_core::promise::resolve(vec![Value::Undefined]).call_method(
            "then",
            vec![func(move |_, _| {
                queued_promise_calls.set(queued_promise_calls.get() + 1);
                Value::Undefined
            })],
        );
        let resolve_slot = Rc::new(RefCell::new(Value::Undefined));
        let executor_slot = Rc::clone(&resolve_slot);
        let pending = w3cos_core::promise::new(vec![func(move |_, arguments| {
            *executor_slot.borrow_mut() = arguments.first().cloned().unwrap_or(Value::Undefined);
            Value::Undefined
        })]);
        let pending_promise_calls = Rc::clone(&calls);
        pending.call_method(
            "then",
            vec![func(move |_, _| {
                pending_promise_calls.set(pending_promise_calls.get() + 1);
                Value::Undefined
            })],
        );

        reset_bridge();
        resolve_slot
            .borrow()
            .call(Value::Undefined, vec![Value::Undefined]);
        let _ = tick_timers();
        let _ = run_animation_frame();
        let _ = drain_microtasks();

        assert_eq!(
            calls.get(),
            0,
            "callbacks owned by the previous Realm must not run after reset"
        );
    }

    #[test]
    fn bridge_reset_releases_and_rebuilds_realm_global_wrappers() {
        setup();
        let document = document_value();
        let window = window_value();
        let selection = selection_value();
        let document_identity = document.identity_hash();
        let window_identity = window.identity_hash();
        let selection_identity = selection.identity_hash();
        let screen = window.get_property("screen");
        let orientation = screen.get_property("orientation");
        let navigation = window.get_property("navigation");
        let virtual_keyboard = window
            .get_property("navigator")
            .get_property("virtualKeyboard");
        let fragment_directive = document.get_property("fragmentDirective");
        let screen_identity = screen.identity_hash();
        let orientation_identity = orientation.identity_hash();
        let navigation_identity = navigation.identity_hash();
        let virtual_keyboard_identity = virtual_keyboard.identity_hash();
        let fragment_directive_identity = fragment_directive.identity_hash();
        document.set_property("__page_marker", Value::string("document"));
        window.set_property("__page_marker", Value::string("window"));
        selection.set_property("__page_marker", Value::string("selection"));
        for value in [
            &screen,
            &orientation,
            &navigation,
            &virtual_keyboard,
            &fragment_directive,
        ] {
            value.set_property("__page_marker", Value::Bool(true));
        }
        window.call_method("scrollTo", vec![Value::Number(12.0), Value::Number(34.0)]);
        document
            .get_property("body")
            .call_method("requestFullscreen", vec![]);

        let document_weak = weak_realm_object(&document);
        let window_weak = weak_realm_object(&window);
        let selection_weak = weak_realm_object(&selection);
        drop(document);
        drop(window);
        drop(selection);

        reset_bridge();

        assert!(
            document_weak.upgrade().is_none(),
            "the bridge must not retain the previous document wrapper"
        );
        assert!(
            window_weak.upgrade().is_none(),
            "the bridge must not retain the previous window wrapper"
        );
        assert!(
            selection_weak.upgrade().is_none(),
            "the bridge must not retain the previous selection wrapper"
        );

        let next_document = document_value();
        let next_window = window_value();
        let next_selection = selection_value();
        let next_screen = next_window.get_property("screen");
        let next_orientation = next_screen.get_property("orientation");
        let next_navigation = next_window.get_property("navigation");
        let next_virtual_keyboard = next_window
            .get_property("navigator")
            .get_property("virtualKeyboard");
        let next_fragment_directive = next_document.get_property("fragmentDirective");
        assert_ne!(next_document.identity_hash(), document_identity);
        assert_ne!(next_window.identity_hash(), window_identity);
        assert_ne!(next_selection.identity_hash(), selection_identity);
        assert_ne!(next_screen.identity_hash(), screen_identity);
        assert_ne!(next_orientation.identity_hash(), orientation_identity);
        assert_ne!(next_navigation.identity_hash(), navigation_identity);
        assert_ne!(
            next_virtual_keyboard.identity_hash(),
            virtual_keyboard_identity
        );
        assert_ne!(
            next_fragment_directive.identity_hash(),
            fragment_directive_identity
        );
        assert!(next_document.get_property("__page_marker").is_undefined());
        assert!(next_window.get_property("__page_marker").is_undefined());
        assert!(next_selection.get_property("__page_marker").is_undefined());
        for value in [
            &next_screen,
            &next_orientation,
            &next_navigation,
            &next_virtual_keyboard,
            &next_fragment_directive,
        ] {
            assert!(value.get_property("__page_marker").is_undefined());
        }
        assert_eq!(next_window.get_property("scrollX"), Value::Number(0.0));
        assert_eq!(next_window.get_property("scrollY"), Value::Number(0.0));
        assert!(next_document.get_property("fullscreenElement").is_null());
        assert!(
            next_window
                .get_property("document")
                .strict_eq(&next_document)
        );
        assert!(
            next_document
                .get_property("defaultView")
                .strict_eq(&next_window)
        );
    }

    #[test]
    fn bridge_reset_invalidates_retained_node_facades_before_id_reuse() {
        setup();
        let old_document = document_value();
        let old_element = old_document.call_method("createElement", vec![Value::string("div")]);
        old_document
            .get_property("body")
            .call_method("appendChild", vec![old_element.clone()]);
        old_element.set_property("id", Value::string("old-page"));
        let recycled_id = node_id_of(&old_element).expect("old element node id");
        let old_set_attribute = old_element.get_property("setAttribute");
        let old_style = old_element.get_property("style");
        let old_dataset = old_element.get_property("dataset");
        let old_class_list = old_element.get_property("classList");
        let old_children = old_document.get_property("body").get_property("children");
        let old_node_constructor = window_value().get_property("Node");
        let old_node_constructor_identity = old_node_constructor.identity_hash();
        old_node_constructor
            .get_property("prototype")
            .set_property("__page_marker", Value::Bool(true));

        dom::reset_document();
        reset_bridge();

        let next_document = document_value();
        let next_element =
            next_document.call_method("createElement", vec![Value::string("section")]);
        next_document
            .get_property("body")
            .call_method("appendChild", vec![next_element.clone()]);
        assert_eq!(
            node_id_of(&next_element),
            Some(recycled_id),
            "the regression must exercise a node id reused by the new document"
        );

        assert_eq!(node_id_of(&old_element), None);
        assert!(old_element.get_property("nodeType").is_undefined());
        assert!(old_children.get_property("length").is_undefined());
        assert!(old_children.get_property("0").is_undefined());

        old_element.set_property("id", Value::string("stale-direct-write"));
        old_set_attribute.call(
            old_element.clone(),
            vec![Value::string("data-stale-method"), Value::string("yes")],
        );
        old_style.set_property("color", Value::string("red"));
        old_dataset.set_property("stale", Value::string("yes"));
        old_class_list.call_method("add", vec![Value::string("stale-class")]);

        assert_eq!(next_element.get_property("id"), Value::string(""));
        assert!(
            next_element
                .call_method("getAttribute", vec![Value::string("data-stale-method")])
                .is_null()
        );
        assert_eq!(
            next_element.get_property("style").get_property("color"),
            Value::string("#000000")
        );
        assert!(
            next_element
                .call_method("getAttribute", vec![Value::string("data-stale")])
                .is_null()
        );
        assert_eq!(next_element.get_property("className"), Value::string(""));

        next_element.set_property("id", Value::string("new-page"));
        assert_eq!(
            next_element.get_property("id"),
            Value::string("new-page"),
            "the current Realm must retain ordinary DOM behavior"
        );

        let next_node_constructor = window_value().get_property("Node");
        assert_ne!(
            next_node_constructor.identity_hash(),
            old_node_constructor_identity
        );
        assert!(
            next_node_constructor
                .get_property("prototype")
                .get_property("__page_marker")
                .is_undefined(),
            "authored prototype mutations must not leak into the next Realm"
        );
    }

    #[test]
    fn microtasks_and_resolved_web_api_promises() {
        setup();
        let win = window_value();
        let order: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
        let o1 = order.clone();
        win.call_method(
            "queueMicrotask",
            vec![func(move |_, _| {
                o1.borrow_mut().push(1);
                Value::Undefined
            })],
        );
        let o2 = order.clone();
        win.call_method(
            "queueMicrotask",
            vec![func(move |_, _| {
                o2.borrow_mut().push(2);
                Value::Undefined
            })],
        );
        assert!(order.borrow().is_empty());
        assert_eq!(drain_microtasks(), 2);
        assert_eq!(order.borrow().as_slice(), &[1, 2]);

        let log = Rc::new(RefCell::new(Vec::new()));
        let rejected = Rc::new(Cell::new(false));
        let promise = resolved_thenable(Value::Number(7.0));
        let rejected_for_handler = Rc::clone(&rejected);
        let caught = promise.call_method(
            "catch",
            vec![func(move |_, _| {
                rejected_for_handler.set(true);
                Value::Undefined
            })],
        );
        assert!(caught.is_object());
        let log_for_then = Rc::clone(&log);
        let chained = promise.call_method(
            "then",
            vec![func(move |_, args| {
                let value = arg(&args, 0).to_number();
                log_for_then.borrow_mut().push(value);
                Value::Number(value + 1.0)
            })],
        );
        assert!(chained.is_object());
        let log_for_finally = Rc::clone(&log);
        let finaled = chained.call_method(
            "finally",
            vec![func(move |_, _| {
                log_for_finally.borrow_mut().push(99.0);
                Value::Undefined
            })],
        );
        let log_for_tail = Rc::clone(&log);
        assert!(
            finaled
                .call_method(
                    "then",
                    vec![func(move |_, args| {
                        log_for_tail.borrow_mut().push(arg(&args, 0).to_number());
                        Value::Undefined
                    })],
                )
                .is_object()
        );
        assert_eq!(drain_microtasks(), 4);
        assert!(!rejected.get());
        assert_eq!(log.borrow().as_slice(), &[7.0, 99.0, 8.0]);
    }

    #[test]
    fn performance_now_increases() {
        setup();
        let win = window_value();
        let perf = win.get_property("performance");
        let t1 = perf.call_method("now", vec![]).to_number();
        // Burn a little time.
        let mut acc = 0u64;
        for i in 0..100_000u64 {
            acc = acc.wrapping_add(i);
        }
        std::hint::black_box(acc);
        let t2 = perf.call_method("now", vec![]).to_number();
        assert!(t1 >= 0.0);
        assert!(t2 >= t1, "t1={t1} t2={t2}");
    }

    #[test]
    fn mutation_observer_batches_dom_records_with_filters_and_old_values() {
        setup();
        let window = window_value();
        let document = document_value();
        let host = create_in_body("section");
        let delivered = Rc::new(RefCell::new(Vec::<Value>::new()));
        let callback_receivers = Rc::new(RefCell::new(Vec::<Value>::new()));
        let delivered_for_callback = Rc::clone(&delivered);
        let receivers_for_callback = Rc::clone(&callback_receivers);
        let observer = w3cos_core::class::construct(
            &window.get_property("MutationObserver"),
            vec![func(move |this, args| {
                receivers_for_callback.borrow_mut().push(this);
                delivered_for_callback
                    .borrow_mut()
                    .extend(arg(&args, 0).iter());
                Value::Undefined
            })],
        );
        observer.call_method(
            "observe",
            vec![
                host.clone(),
                Value::object(HashMap::from([
                    ("attributes".to_string(), Value::Bool(true)),
                    ("attributeOldValue".to_string(), Value::Bool(true)),
                    (
                        "attributeFilter".to_string(),
                        js_array(vec![Value::string("data-x")]),
                    ),
                    ("childList".to_string(), Value::Bool(true)),
                    ("characterData".to_string(), Value::Bool(true)),
                    ("characterDataOldValue".to_string(), Value::Bool(true)),
                    ("subtree".to_string(), Value::Bool(true)),
                ])),
            ],
        );

        host.call_method(
            "setAttribute",
            vec![Value::string("data-x"), Value::string("one")],
        );
        host.call_method(
            "setAttribute",
            vec![Value::string("data-y"), Value::string("ignored")],
        );
        let child = document.call_method("createElement", vec![Value::string("span")]);
        host.call_method("appendChild", vec![child.clone()]);
        let text = document.call_method("createTextNode", vec![Value::string("before")]);
        child.call_method("appendChild", vec![text.clone()]);
        text.set_property("textContent", Value::string("after"));

        assert!(delivered.borrow().is_empty());
        assert_eq!(drain_microtasks(), 1);
        assert!(callback_receivers.borrow()[0] == observer);
        let records = delivered.borrow();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].get_property("type").to_js_string(), "attributes");
        assert_eq!(
            records[0].get_property("attributeName").to_js_string(),
            "data-x"
        );
        assert!(records[0].get_property("oldValue").is_null());
        assert_eq!(records[1].get_property("type").to_js_string(), "childList");
        assert!(w3cos_core::class::instance_of(
            &records[1].get_property("addedNodes"),
            &window.get_property("NodeList")
        ));
        assert_eq!(
            records[3].get_property("type").to_js_string(),
            "characterData"
        );
        assert_eq!(records[3].get_property("oldValue").to_js_string(), "before");
        drop(records);

        host.call_method(
            "setAttribute",
            vec![Value::string("data-x"), Value::string("two")],
        );
        let pending = observer.call_method("takeRecords", vec![]);
        assert_eq!(pending.get_property("length").to_u32(), 1);
        assert_eq!(
            pending
                .get_property("0")
                .get_property("oldValue")
                .to_js_string(),
            "one"
        );
        assert_eq!(drain_microtasks(), 1);
        assert_eq!(delivered.borrow().len(), 4);

        observer.call_method("disconnect", vec![]);
        host.call_method(
            "setAttribute",
            vec![Value::string("data-x"), Value::string("three")],
        );
        assert_eq!(drain_microtasks(), 0);
        assert_eq!(
            observer
                .call_method("takeRecords", vec![])
                .get_property("length")
                .to_u32(),
            0
        );
    }

    #[test]
    fn mutation_observer_records_reflected_properties_before_tree_changes() {
        setup();
        let window = window_value();
        let host = create_in_body("p");
        host.set_property("id", Value::string("n00"));
        let observer = w3cos_core::class::construct(
            &window.get_property("MutationObserver"),
            vec![func(|_, _| Value::Undefined)],
        );
        observer.call_method(
            "observe",
            vec![
                host.clone(),
                Value::object(HashMap::from([
                    ("attributes".to_string(), Value::Bool(true)),
                    ("attributeOldValue".to_string(), Value::Bool(true)),
                    ("childList".to_string(), Value::Bool(true)),
                    ("characterData".to_string(), Value::Bool(true)),
                    ("characterDataOldValue".to_string(), Value::Bool(true)),
                    ("subtree".to_string(), Value::Bool(true)),
                ])),
            ],
        );

        host.set_property("id", Value::string("foo"));
        host.set_property("id", Value::string("bar"));
        host.set_property("className", Value::string("bar"));
        host.set_property("textContent", Value::string("old data"));
        host.get_property("firstChild")
            .set_property("data", Value::string("new data"));

        let records = observer.call_method("takeRecords", vec![]);
        assert_eq!(records.get_property("length").to_u32(), 5);
        assert_eq!(
            records
                .get_property("0")
                .get_property("type")
                .to_js_string(),
            "attributes"
        );
        assert_eq!(
            records
                .get_property("0")
                .get_property("oldValue")
                .to_js_string(),
            "n00"
        );
        assert_eq!(
            records
                .get_property("3")
                .get_property("type")
                .to_js_string(),
            "childList"
        );
        assert_eq!(
            records
                .get_property("2")
                .get_property("type")
                .to_js_string(),
            "attributes"
        );
        assert_eq!(
            records
                .get_property("2")
                .get_property("attributeName")
                .to_js_string(),
            "class"
        );
        assert_eq!(
            records
                .get_property("4")
                .get_property("type")
                .to_js_string(),
            "characterData"
        );
    }

    #[test]
    fn tree_walker_and_node_iterator_follow_filters_and_live_dom() {
        setup();
        let window = window_value();
        let document = document_value();
        let node_filter = window.get_property("NodeFilter");
        assert_eq!(node_filter.get_property("FILTER_ACCEPT").to_u32(), 1);
        assert_eq!(node_filter.get_property("FILTER_REJECT").to_u32(), 2);
        assert_eq!(node_filter.get_property("FILTER_SKIP").to_u32(), 3);

        let host = create_in_body("section");
        let skipped = document.call_method("createElement", vec![Value::string("div")]);
        let span = document.call_method("createElement", vec![Value::string("span")]);
        span.call_method(
            "appendChild",
            vec![document.call_method("createTextNode", vec![Value::string("A")])],
        );
        skipped.call_method("appendChild", vec![span.clone()]);
        host.call_method("appendChild", vec![skipped]);
        let rejected = document.call_method("createElement", vec![Value::string("aside")]);
        let hidden = document.call_method("createElement", vec![Value::string("b")]);
        hidden.call_method(
            "appendChild",
            vec![document.call_method("createTextNode", vec![Value::string("H")])],
        );
        rejected.call_method("appendChild", vec![hidden]);
        host.call_method("appendChild", vec![rejected]);
        let paragraph = document.call_method("createElement", vec![Value::string("p")]);
        paragraph.call_method(
            "appendChild",
            vec![document.call_method("createTextNode", vec![Value::string("B")])],
        );
        host.call_method("appendChild", vec![paragraph.clone()]);

        let filter = func(|_, args| {
            match arg(&args, 0)
                .get_property("tagName")
                .to_js_string()
                .as_str()
            {
                "DIV" => Value::Number(3.0),
                "ASIDE" => Value::Number(2.0),
                _ => Value::Number(1.0),
            }
        });
        let walker = document.call_method(
            "createTreeWalker",
            vec![
                host.clone(),
                node_filter.get_property("SHOW_ELEMENT"),
                filter,
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &walker,
            &window.get_property("TreeWalker")
        ));
        assert!(walker.get_property("root") == host);
        assert!(walker.get_property("currentNode") == host);
        assert!(walker.call_method("nextNode", vec![]) == span);
        assert!(walker.call_method("nextNode", vec![]) == paragraph);
        assert!(walker.call_method("nextNode", vec![]).is_null());
        walker.set_property("currentNode", host.clone());
        assert!(walker.call_method("firstChild", vec![]) == span);
        walker.set_property("currentNode", host.clone());
        assert!(walker.call_method("lastChild", vec![]) == paragraph);

        let iterator = document.call_method(
            "createNodeIterator",
            vec![
                host.clone(),
                node_filter.get_property("SHOW_TEXT"),
                Value::Null,
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &iterator,
            &window.get_property("NodeIterator")
        ));
        assert_eq!(
            iterator
                .call_method("nextNode", vec![])
                .get_property("textContent")
                .to_js_string(),
            "A"
        );
        assert_eq!(
            iterator
                .call_method("nextNode", vec![])
                .get_property("textContent")
                .to_js_string(),
            "H"
        );
        let text_b = iterator.call_method("nextNode", vec![]);
        assert_eq!(text_b.get_property("textContent").to_js_string(), "B");
        assert!(iterator.call_method("previousNode", vec![]) == text_b);
        assert!(iterator.call_method("nextNode", vec![]) == text_b);

        paragraph.call_method(
            "appendChild",
            vec![document.call_method("createTextNode", vec![Value::string("C")])],
        );
        assert_eq!(
            iterator
                .call_method("nextNode", vec![])
                .get_property("textContent")
                .to_js_string(),
            "C"
        );
        let tail_text = document.call_method("createTextNode", vec![Value::string("D")]);
        host.call_method("appendChild", vec![tail_text.clone()]);
        assert_eq!(
            iterator
                .call_method("previousNode", vec![])
                .get_property("textContent")
                .to_js_string(),
            "C"
        );
        host.call_method("removeChild", vec![paragraph]);
        assert!(iterator.call_method("nextNode", vec![]) == tail_text);

        let document_iterator = document.call_method(
            "createNodeIterator",
            vec![document.clone(), node_filter.get_property("SHOW_DOCUMENT")],
        );
        assert!(document_iterator.get_property("root") == document);
        assert!(document_iterator.call_method("nextNode", vec![]) == document);
    }

    #[test]
    fn performance_timeline_and_observer_deliver_entries() {
        setup();
        let window = window_value();
        let performance = window.get_property("performance");
        let observer_log = Rc::new(RefCell::new(Vec::<String>::new()));
        let log = Rc::clone(&observer_log);
        let observer = w3cos_core::class::construct(
            &window.get_property("PerformanceObserver"),
            vec![func(move |_, args| {
                let list = args[0].clone();
                let entries = list.call_method("getEntries", vec![]);
                log.borrow_mut().push(format!(
                    "{}:{}:{}",
                    entries.get_property("length").to_number(),
                    list.call_method("getEntriesByType", vec![Value::string("mark")])
                        .get_property("length")
                        .to_number(),
                    list.call_method("getEntriesByName", vec![Value::string("span")])
                        .get_property("length")
                        .to_number()
                ));
                Value::Undefined
            })],
        );
        observer.call_method(
            "observe",
            vec![Value::object(HashMap::from([(
                "entryTypes".to_string(),
                js_array(vec![Value::string("mark"), Value::string("measure")]),
            )]))],
        );

        let detail = Value::object(HashMap::from([("id".to_string(), Value::Number(7.0))]));
        let start = performance.call_method(
            "mark",
            vec![
                Value::string("start"),
                Value::object(HashMap::from([
                    ("startTime".to_string(), Value::Number(5.0)),
                    ("detail".to_string(), detail.clone()),
                ])),
            ],
        );
        performance.call_method(
            "mark",
            vec![
                Value::string("end"),
                Value::object(HashMap::from([(
                    "startTime".to_string(),
                    Value::Number(12.0),
                )])),
            ],
        );
        let measure = performance.call_method(
            "measure",
            vec![
                Value::string("span"),
                Value::string("start"),
                Value::string("end"),
            ],
        );
        assert_eq!(start.get_property("entryType").to_js_string(), "mark");
        assert!(!(start.get_property("detail") == detail));
        assert_eq!(
            start.get_property("detail").get_property("id").to_number(),
            7.0
        );
        assert_eq!(measure.get_property("startTime").to_number(), 5.0);
        assert_eq!(measure.get_property("duration").to_number(), 7.0);
        assert!(observer_log.borrow().is_empty());
        assert_eq!(
            performance
                .call_method("getEntries", vec![])
                .get_property("length")
                .to_number(),
            3.0
        );

        assert_eq!(drain_microtasks(), 1);
        assert_eq!(observer_log.borrow().as_slice(), &["3:2:1"]);
        assert_eq!(
            observer
                .call_method("takeRecords", vec![])
                .get_property("length")
                .to_number(),
            0.0
        );

        let buffered_count = Rc::new(Cell::new(0));
        let buffered_for_callback = Rc::clone(&buffered_count);
        let buffered_observer = w3cos_core::class::construct(
            &window.get_property("PerformanceObserver"),
            vec![func(move |_, args| {
                buffered_for_callback.set(
                    args[0]
                        .call_method("getEntries", vec![])
                        .get_property("length")
                        .to_u32(),
                );
                Value::Undefined
            })],
        );
        buffered_observer.call_method(
            "observe",
            vec![Value::object(HashMap::from([
                ("type".to_string(), Value::string("measure")),
                ("buffered".to_string(), Value::Bool(true)),
            ]))],
        );
        assert_eq!(drain_microtasks(), 1);
        assert_eq!(buffered_count.get(), 1);

        performance.call_method("clearMarks", vec![Value::string("start")]);
        assert_eq!(
            performance
                .call_method("getEntriesByType", vec![Value::string("mark")])
                .get_property("length")
                .to_number(),
            1.0
        );
        performance.call_method("clearMeasures", vec![]);
        assert_eq!(
            performance
                .call_method("getEntriesByType", vec![Value::string("measure")])
                .get_property("length")
                .to_number(),
            0.0
        );
    }

    #[test]
    fn window_viewport_and_match_media() {
        setup();
        let win = window_value();
        let visual_viewport = win.get_property("visualViewport");
        assert!(w3cos_core::class::instance_of(
            &visual_viewport,
            &win.get_property("VisualViewport")
        ));
        assert!(w3cos_core::class::instance_of(
            &visual_viewport,
            &win.get_property("EventTarget")
        ));
        let resize_count = Rc::new(Cell::new(0));
        let resize_count_for_handler = Rc::clone(&resize_count);
        visual_viewport.call_method(
            "addEventListener",
            vec![
                Value::string("resize"),
                Value::function(move |_, _| {
                    resize_count_for_handler.set(resize_count_for_handler.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        assert_eq!(win.get_property("innerWidth").to_number(), 1024.0);
        assert_eq!(win.get_property("innerHeight").to_number(), 768.0);
        assert_eq!(visual_viewport.get_property("width").to_number(), 1024.0);
        assert_eq!(visual_viewport.get_property("height").to_number(), 768.0);
        assert!(visual_viewport.get_property("onresize").is_null());
        assert_eq!(win.get_property("devicePixelRatio").to_number(), 1.0);
        set_viewport(1440.0, 900.0);
        set_device_pixel_ratio(2.0);
        assert_eq!(win.get_property("innerWidth").to_number(), 1440.0);
        assert_eq!(win.get_property("innerHeight").to_number(), 900.0);
        assert_eq!(visual_viewport.get_property("width").to_number(), 1440.0);
        assert_eq!(visual_viewport.get_property("height").to_number(), 900.0);
        assert_eq!(resize_count.get(), 1);
        assert_eq!(win.get_property("devicePixelRatio").to_number(), 2.0);

        let mql = win.call_method("matchMedia", vec![Value::string("(min-width: 600px)")]);
        assert!(mql.get_property("matches").to_bool());
        assert!(w3cos_core::class::instance_of(
            &mql,
            &win.get_property("MediaQueryList")
        ));
        let mql_prototype = win.get_property("MediaQueryList").get_property("prototype");
        assert!(mql_prototype.get_property("addListener").is_function());
        assert!(mql_prototype.get_property("removeListener").is_function());
        for property in ["matches", "media", "onchange"] {
            assert!(mql_prototype.get_property(property).is_undefined());
        }
        let mql2 = win.call_method("matchMedia", vec![Value::string("(max-width: 600px)")]);
        assert!(!mql2.get_property("matches").to_bool());
        let changes = Rc::new(Cell::new(0));
        let changes_for_listener = Rc::clone(&changes);
        mql.call_method(
            "addListener",
            vec![func(move |_, args| {
                let event = arg(&args, 0);
                assert!(!event.get_property("matches").to_bool());
                assert_eq!(
                    event.get_property("media").to_js_string(),
                    "(min-width: 600px)"
                );
                assert!(w3cos_core::class::instance_of(
                    &event,
                    &window_value().get_property("MediaQueryListEvent")
                ));
                changes_for_listener.set(changes_for_listener.get() + 1);
                Value::Undefined
            })],
        );
        set_viewport(500.0, 900.0);
        assert!(!mql.get_property("matches").to_bool());
        assert!(mql2.get_property("matches").to_bool());
        assert_eq!(changes.get(), 1);
    }

    #[test]
    fn window_document_and_self_references() {
        setup();
        let win = window_value();
        assert!(w3cos_core::class::instance_of(
            &win,
            &win.get_property("Window")
        ));
        assert_eq!(
            win.get_property("Window")
                .get_property("PERSISTENT")
                .to_u32(),
            1
        );
        let doc = win.get_property("document");
        assert!(doc == document_value());
        assert!(win.get_property("self") == win);
        assert!(win.get_property("window") == win);
        assert!(doc.get_property("defaultView") == win);
        let nav = win.get_property("navigator");
        assert_eq!(nav.get_property("maxTouchPoints").to_number(), 0.0);
        set_max_touch_points(5);
        assert_eq!(nav.get_property("maxTouchPoints").to_number(), 5.0);
        set_max_touch_points(2);
        assert_eq!(nav.get_property("maxTouchPoints").to_number(), 2.0);
        set_max_touch_points(0);
        assert_eq!(nav.get_property("language").to_js_string(), "en-US");
        let loc = win.get_property("location");
        assert_eq!(loc.get_property("href").to_js_string(), "w3cos://app/");
    }

    #[test]
    fn document_structure_getters() {
        setup();
        let doc = document_value();
        let de = doc.get_property("documentElement");
        assert_eq!(de.get_property("tagName").to_js_string(), "HTML");
        let head = doc.get_property("head");
        assert_eq!(head.get_property("tagName").to_js_string(), "HEAD");
        let body = doc.get_property("body");
        assert_eq!(body.get_property("tagName").to_js_string(), "BODY");
        // body lives under <html> after the lazy restructure.
        assert!(body.get_property("parentNode") == de);
        assert!(doc.get_property("activeElement") == body);
        assert_eq!(doc.get_property("readyState").to_js_string(), "complete");
        assert_eq!(
            doc.get_property("visibilityState").to_js_string(),
            "visible"
        );
        assert!(!doc.get_property("hidden").to_bool());
        assert!(doc.get_property("fonts").is_object());
    }

    #[test]
    fn dom_parser_and_xml_serializer_create_queryable_documents() {
        setup();
        let window = window_value();
        let parser = w3cos_core::class::construct(&window.get_property("DOMParser"), vec![]);
        assert!(w3cos_core::class::instance_of(
            &parser,
            &window.get_property("DOMParser")
        ));
        let html = parser.call_method(
            "parseFromString",
            vec![
                Value::string(
                    "<title>Inbox</title><main><article id='message'>Hello &amp; bye</article></main>",
                ),
                Value::string("text/html"),
            ],
        );
        assert_eq!(html.get_property("nodeType").to_number(), 9.0);
        assert_eq!(
            html.call_method("querySelector", vec![Value::string("#message")])
                .get_property("textContent")
                .to_js_string(),
            "Hello & bye"
        );
        assert_eq!(
            html.get_property("documentElement")
                .get_property("tagName")
                .to_js_string(),
            "HTML"
        );

        let xml = parser.call_method(
            "parseFromString",
            vec![
                Value::string("<root id='r'><item>value</item></root>"),
                Value::string("application/xml"),
            ],
        );
        assert_eq!(
            xml.get_property("documentElement")
                .get_property("tagName")
                .to_js_string(),
            "root"
        );
        assert_eq!(
            xml.call_method("getElementById", vec![Value::string("r")])
                .get_property("tagName")
                .to_js_string(),
            "root"
        );
        assert!(
            xml.get_property("documentElement")
                .get_property("parentNode")
                == xml
        );
        let serializer =
            w3cos_core::class::construct(&window.get_property("XMLSerializer"), vec![]);
        assert!(w3cos_core::class::instance_of(
            &serializer,
            &window.get_property("XMLSerializer")
        ));
        assert_eq!(
            serializer
                .call_method("serializeToString", vec![xml])
                .to_js_string(),
            "<root id=\"r\"><item>value</item></root>"
        );
    }

    #[test]
    fn css_font_loading_api_registers_and_tracks_faces() {
        setup();
        let window = window_value();
        let document = document_value();
        let fonts = document.get_property("fonts");
        let face = w3cos_core::class::construct(
            &window.get_property("FontFace"),
            vec![
                Value::string("W3cosFixture"),
                w3cos_core::binary::array_buffer_value(vec![0, 1, 2, 3]),
                Value::object(HashMap::from([
                    ("weight".to_string(), Value::string("700")),
                    ("style".to_string(), Value::string("italic")),
                    ("display".to_string(), Value::string("swap")),
                ])),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &face,
            &window.get_property("FontFace")
        ));
        assert!(w3cos_core::class::instance_of(
            &fonts,
            &window.get_property("FontFaceSet")
        ));
        assert!(w3cos_core::class::instance_of(
            &fonts,
            &window.get_property("EventTarget")
        ));
        assert_eq!(face.get_property("status").to_js_string(), "unloaded");
        assert!(
            !fonts
                .call_method("check", vec![Value::string("italic 700 12px W3cosFixture")])
                .to_bool()
        );

        let returned = fonts.call_method("add", vec![face.clone()]);
        assert!(returned == fonts);
        assert_eq!(fonts.get_property("size").to_number(), 1.0);
        assert!(fonts.call_method("has", vec![face.clone()]).to_bool());
        assert_eq!(
            fonts
                .call_method("values", vec![])
                .get_property("length")
                .to_number(),
            1.0
        );

        let loading_events = Rc::new(Cell::new(0));
        let loading_events_for_callback = Rc::clone(&loading_events);
        let ready_during_load = Rc::new(Cell::new(false));
        let ready_during_load_for_callback = Rc::clone(&ready_during_load);
        fonts.call_method(
            "addEventListener",
            vec![
                Value::string("loading"),
                func(move |this, args| {
                    loading_events_for_callback.set(
                        loading_events_for_callback.get()
                            + usize::from(
                                this.get_property("status").to_js_string() == "loading"
                                    && args[0]
                                        .get_property("fontfaces")
                                        .get_property("length")
                                        .to_number()
                                        == 1.0,
                            ),
                    );
                    let ready_during_load_for_callback = Rc::clone(&ready_during_load_for_callback);
                    this.get_property("ready").call_method(
                        "then",
                        vec![func(move |_, _| {
                            ready_during_load_for_callback.set(true);
                            Value::Undefined
                        })],
                    );
                    Value::Undefined
                }),
            ],
        );
        let loading_done_events = Rc::new(Cell::new(0));
        let loading_done_events_for_callback = Rc::clone(&loading_done_events);
        fonts.call_method(
            "addEventListener",
            vec![
                Value::string("loadingdone"),
                func(move |this, args| {
                    loading_done_events_for_callback.set(
                        loading_done_events_for_callback.get()
                            + usize::from(
                                this.get_property("status").to_js_string() == "loaded"
                                    && args[0]
                                        .get_property("fontfaces")
                                        .get_property("length")
                                        .to_number()
                                        == 1.0,
                            ),
                    );
                    Value::Undefined
                }),
            ],
        );

        let loaded = Rc::new(Cell::new(false));
        let loaded_for_callback = Rc::clone(&loaded);
        face.call_method("load", vec![]).call_method(
            "then",
            vec![func(move |_, args| {
                loaded_for_callback.set(args[0].get_property("status").to_js_string() == "loaded");
                Value::Undefined
            })],
        );
        assert_eq!(loading_events.get(), 1);
        assert_eq!(loading_done_events.get(), 1);
        assert!(!ready_during_load.get());
        drain_microtasks();
        assert!(loaded.get());
        assert!(ready_during_load.get());
        assert_eq!(face.get_property("status").to_js_string(), "loaded");
        assert!(
            fonts
                .call_method("check", vec![Value::string("italic 700 12px W3cosFixture")])
                .to_bool()
        );
        let promise_checks = Rc::new(Cell::new(0));
        let loaded_check = Rc::clone(&promise_checks);
        face.get_property("loaded").call_method(
            "then",
            vec![func(move |_, args| {
                assert_eq!(args[0].get_property("status").to_js_string(), "loaded");
                loaded_check.set(loaded_check.get() + 1);
                Value::Undefined
            })],
        );
        let set_load_check = Rc::clone(&promise_checks);
        fonts
            .call_method("load", vec![Value::string("italic 700 12px W3cosFixture")])
            .call_method(
                "then",
                vec![func(move |_, args| {
                    assert_eq!(args[0].get_property("length").to_number(), 1.0);
                    set_load_check.set(set_load_check.get() + 1);
                    Value::Undefined
                })],
            );
        let ready_check = Rc::clone(&promise_checks);
        fonts.get_property("ready").call_method(
            "then",
            vec![func(move |_, args| {
                assert!(w3cos_core::class::instance_of(
                    &args[0],
                    &window_value().get_property("FontFaceSet")
                ));
                ready_check.set(ready_check.get() + 1);
                Value::Undefined
            })],
        );
        drain_microtasks();
        assert_eq!(promise_checks.get(), 3);

        assert!(fonts.call_method("delete", vec![face.clone()]).to_bool());
        assert_eq!(fonts.get_property("size").to_number(), 0.0);
        assert!(
            !fonts
                .call_method("check", vec![Value::string("italic 700 12px W3cosFixture")])
                .to_bool()
        );
    }

    #[test]
    fn css_font_loading_api_reports_failed_cycles_and_resolves_ready() {
        setup();
        let window = window_value();
        let fonts = document_value().get_property("fonts");
        let face = w3cos_core::class::construct(
            &window.get_property("FontFace"),
            vec![
                Value::string("W3cosUnavailableNetworkFixture"),
                Value::string("url(\"https://example.test/font.woff2\")"),
            ],
        );
        fonts.call_method("add", vec![face.clone()]);

        let errors = Rc::new(Cell::new(0));
        let errors_for_callback = Rc::clone(&errors);
        fonts.call_method(
            "addEventListener",
            vec![
                Value::string("loadingerror"),
                func(move |this, args| {
                    if this.get_property("status").to_js_string() == "loaded"
                        && args[0]
                            .get_property("fontfaces")
                            .get_property("length")
                            .to_number()
                            == 1.0
                    {
                        errors_for_callback.set(errors_for_callback.get() + 1);
                    }
                    Value::Undefined
                }),
            ],
        );
        let ready = Rc::new(Cell::new(false));
        let ready_from_loading = Rc::clone(&ready);
        fonts.call_method(
            "addEventListener",
            vec![
                Value::string("loading"),
                func(move |this, _| {
                    let ready_from_loading = Rc::clone(&ready_from_loading);
                    this.get_property("ready").call_method(
                        "then",
                        vec![func(move |_, _| {
                            ready_from_loading.set(true);
                            Value::Undefined
                        })],
                    );
                    Value::Undefined
                }),
            ],
        );
        let rejected = Rc::new(Cell::new(false));
        let rejected_for_callback = Rc::clone(&rejected);
        face.call_method("load", vec![]).call_method(
            "catch",
            vec![func(move |_, _| {
                rejected_for_callback.set(true);
                Value::Undefined
            })],
        );

        assert_eq!(face.get_property("status").to_js_string(), "error");
        assert_eq!(fonts.get_property("status").to_js_string(), "loaded");
        assert_eq!(errors.get(), 1);
        assert!(!ready.get());
        drain_microtasks();
        assert!(ready.get());
        assert!(rejected.get());
    }

    #[test]
    fn constraint_validation_exposes_live_state_and_invalid_events() {
        setup();
        let document = document_value();
        let form = document.call_method("createElement", vec![Value::string("form")]);
        let input = document.call_method("createElement", vec![Value::string("input")]);
        input.set_property("required", Value::Bool(true));
        form.call_method("appendChild", vec![input.clone()]);
        let validity = input.get_property("validity");
        assert!(w3cos_core::class::instance_of(
            &validity,
            &window_value().get_property("ValidityState")
        ));
        assert!(input.get_property("willValidate").to_bool());
        assert!(validity.get_property("valueMissing").to_bool());
        assert!(!validity.get_property("valid").to_bool());

        let invalid_events = Rc::new(Cell::new(0));
        let invalid_events_for_listener = Rc::clone(&invalid_events);
        input.call_method(
            "addEventListener",
            vec![
                Value::string("invalid"),
                func(move |_, _| {
                    invalid_events_for_listener.set(invalid_events_for_listener.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        assert!(!form.call_method("checkValidity", vec![]).to_bool());
        assert_eq!(invalid_events.get(), 1);

        input.set_property("value", Value::string("ready"));
        assert!(validity.get_property("valid").to_bool());
        assert!(form.call_method("checkValidity", vec![]).to_bool());
        input.call_method("setCustomValidity", vec![Value::string("blocked")]);
        assert!(validity.get_property("customError").to_bool());
        assert_eq!(
            input.get_property("validationMessage").to_js_string(),
            "blocked"
        );
        input.call_method("setCustomValidity", vec![Value::string("")]);
        assert!(validity.get_property("valid").to_bool());

        input.set_property("type", Value::string("number"));
        input.set_property("min", Value::string("2"));
        input.set_property("max", Value::string("8"));
        input.set_property("step", Value::string("2"));
        input.set_property("value", Value::string("5"));
        assert!(validity.get_property("stepMismatch").to_bool());
        input.set_property("value", Value::string("9"));
        assert!(validity.get_property("rangeOverflow").to_bool());
    }

    #[test]
    fn legacy_dom_error_and_crypto_key_have_standard_identities() {
        setup();
        let window = window_value();
        let dom_error = w3cos_core::class::construct(
            &window.get_property("DOMError"),
            vec![Value::string("LegacyError"), Value::string("details")],
        );
        assert_eq!(dom_error.get_property("name").to_js_string(), "LegacyError");
        assert_eq!(dom_error.get_property("message").to_js_string(), "details");
        for property in ["algorithm", "extractable", "type", "usages"] {
            assert!(
                window
                    .get_property("CryptoKey")
                    .get_property("prototype")
                    .get_property(property)
                    .is_undefined()
            );
        }
    }

    #[test]
    fn caret_position_uses_layout_hit_testing_and_text_offsets() {
        setup();
        let document = document_value();
        let div = document.call_method("createElement", vec![Value::string("div")]);
        let text = document.call_method("createTextNode", vec![Value::string("abcd")]);
        div.call_method("appendChild", vec![text.clone()]);
        document
            .get_property("body")
            .call_method("appendChild", vec![div.clone()]);
        let rect = w3cos_dom::DOMRect::new(0.0, 0.0, 100.0, 20.0);
        let layout_nodes = [
            document.get_property("documentElement"),
            document.get_property("body"),
            div.clone(),
            text.clone(),
        ]
        .map(|value| NodeId::from_u32(node_id_of(&value).unwrap()));
        dom::with_document_mut(|tree| {
            for node in layout_nodes {
                tree.set_layout_rect(node, rect);
            }
        });
        let caret = document.call_method(
            "caretPositionFromPoint",
            vec![Value::Number(50.0), Value::Number(10.0)],
        );
        assert!(w3cos_core::class::instance_of(
            &caret,
            &caret_position_class()
        ));
        assert!(caret.get_property("offsetNode") == text);
        assert_eq!(caret.get_property("offset").to_number(), 2.0);
        assert_eq!(
            caret
                .call_method("getClientRect", vec![])
                .get_property("width")
                .to_number(),
            100.0
        );
    }

    #[test]
    fn element_animate_is_visible_through_element_and_document_queries() {
        setup();
        let document = document_value();
        let element = document.call_method("createElement", vec![Value::string("div")]);
        document
            .get_property("body")
            .call_method("appendChild", vec![element.clone()]);
        let animation = element.call_method(
            "animate",
            vec![
                Value::array(vec![
                    Value::object(HashMap::from([("opacity".into(), Value::Number(0.0))])),
                    Value::object(HashMap::from([("opacity".into(), Value::Number(1.0))])),
                ]),
                Value::Number(200.0),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &animation,
            &window_value().get_property("Animation")
        ));
        assert_eq!(
            animation.get_property("playState").to_js_string(),
            "running"
        );
        assert_eq!(
            element
                .call_method("getAnimations", Vec::new())
                .get_property("length")
                .to_u32(),
            1
        );
        assert_eq!(
            document
                .call_method("getAnimations", Vec::new())
                .get_property("length")
                .to_u32(),
            1
        );
    }

    #[test]
    fn mathml_namespace_creates_mathml_element_instances() {
        setup();
        let element = document_value().call_method(
            "createElementNS",
            vec![
                Value::string("http://www.w3.org/1998/Math/MathML"),
                Value::string("math"),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &element,
            &window_value().get_property("MathMLElement")
        ));
        assert_eq!(
            element.get_property("namespaceURI").to_js_string(),
            "http://www.w3.org/1998/Math/MathML"
        );
    }

    #[test]
    fn canvas_2d_context() {
        setup();
        let doc = document_value();
        let canvas = doc.call_method("createElement", vec![Value::string("canvas")]);
        canvas.call_method(
            "setAttribute",
            vec![Value::string("width"), Value::string("200")],
        );
        canvas.call_method(
            "setAttribute",
            vec![Value::string("height"), Value::string("100")],
        );
        assert_eq!(canvas.get_property("width").to_number(), 200.0);
        assert_eq!(canvas.get_property("height").to_number(), 100.0);

        let ctx = canvas.call_method("getContext", vec![Value::string("2d")]);
        assert!(ctx.is_object() || ctx.is_function());
        assert!(w3cos_core::class::instance_of(
            &ctx,
            &crate::canvas_web::canvas_rendering_context_2d_class()
        ));
        assert!(ctx == canvas.call_method("getContext", vec![Value::string("2d")]));
        ctx.set_property("fillStyle", Value::string("#ff0000"));
        assert_eq!(ctx.get_property("fillStyle").to_js_string(), "#ff0000");
        ctx.call_method(
            "setLineDash",
            vec![js_array(vec![Value::Number(3.0), Value::Number(2.0)])],
        );
        assert_eq!(
            ctx.call_method("getLineDash", vec![])
                .iter()
                .map(|value| value.to_number())
                .collect::<Vec<_>>(),
            vec![3.0, 2.0]
        );
        ctx.set_property("lineDashOffset", Value::Number(1.5));
        assert_eq!(ctx.get_property("lineDashOffset").to_number(), 1.5);
        ctx.call_method(
            "fillRect",
            vec![
                Value::Number(0.0),
                Value::Number(0.0),
                Value::Number(50.0),
                Value::Number(50.0),
            ],
        );
        let metrics = ctx.call_method("measureText", vec![Value::string("hello")]);
        assert!(metrics.get_property("width").to_number() >= 0.0);
        assert!(w3cos_core::class::instance_of(
            &metrics,
            &crate::canvas_web::text_metrics_class()
        ));
        let gradient = ctx.call_method(
            "createLinearGradient",
            vec![
                Value::Number(0.0),
                Value::Number(0.0),
                Value::Number(10.0),
                Value::Number(10.0),
            ],
        );
        assert!(w3cos_core::class::instance_of(
            &gradient,
            &crate::canvas_web::canvas_gradient_class()
        ));
        gradient.call_method(
            "addColorStop",
            vec![Value::Number(0.5), Value::string("#f00")],
        );
        ctx.set_property("fillStyle", gradient);
        let pattern = ctx.call_method(
            "createPattern",
            vec![canvas.clone(), Value::string("repeat-x")],
        );
        assert!(w3cos_core::class::instance_of(
            &pattern,
            &crate::canvas_web::canvas_pattern_class()
        ));
        let offscreen = w3cos_core::class::construct(
            &crate::canvas_web::offscreen_canvas_class(),
            vec![Value::Number(4.0), Value::Number(3.0)],
        );
        let bitmap = offscreen.call_method("transferToImageBitmap", vec![]);
        assert!(w3cos_core::class::instance_of(
            &bitmap,
            &crate::canvas_web::image_bitmap_class()
        ));
        assert_eq!(bitmap.get_property("width").to_number(), 4.0);
        let bitmap_context =
            canvas.call_method("getContext", vec![Value::string("bitmaprenderer")]);
        assert!(w3cos_core::class::instance_of(
            &bitmap_context,
            &crate::canvas_web::image_bitmap_rendering_context_class()
        ));
        bitmap_context.call_method("transferFromImageBitmap", vec![bitmap.clone()]);
        assert!(bitmap.get_property("__w3cos_closed").to_bool());
        let capture = canvas.call_method("captureStream", vec![Value::Number(30.0)]);
        let capture_track = capture
            .call_method("getVideoTracks", vec![])
            .get_property("0");
        assert!(w3cos_core::class::instance_of(
            &capture_track,
            &crate::canvas_web::canvas_capture_media_stream_track_class()
        ));
        assert!(w3cos_core::class::instance_of(
            &capture_track,
            &crate::media_devices_web::media_stream_track_class()
        ));
        assert!(capture_track.get_property("canvas") == canvas);
        let img = ctx.call_method(
            "getImageData",
            vec![
                Value::Number(0.0),
                Value::Number(0.0),
                Value::Number(10.0),
                Value::Number(10.0),
            ],
        );
        assert_eq!(img.get_property("width").to_number(), 10.0);
        assert_eq!(
            img.get_property("data").get_property("length").to_number(),
            400.0
        );

        let source = doc.call_method("createElement", vec![Value::string("canvas")]);
        source.set_property("width", Value::Number(2.0));
        source.set_property("height", Value::Number(1.0));
        let source_ctx = source.call_method("getContext", vec![Value::string("2d")]);
        source_ctx.set_property("fillStyle", Value::string("#ff0000"));
        source_ctx.call_method(
            "fillRect",
            vec![
                Value::Number(0.0),
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(1.0),
            ],
        );
        source_ctx.set_property("fillStyle", Value::string("#0000ff"));
        source_ctx.call_method(
            "fillRect",
            vec![
                Value::Number(1.0),
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(1.0),
            ],
        );
        ctx.call_method(
            "drawImage",
            vec![
                source.clone(),
                Value::Number(60.0),
                Value::Number(0.0),
                Value::Number(20.0),
                Value::Number(10.0),
            ],
        );
        let scaled = ctx.call_method(
            "getImageData",
            vec![
                Value::Number(60.0),
                Value::Number(0.0),
                Value::Number(20.0),
                Value::Number(1.0),
            ],
        );
        let scaled_bytes: Vec<u32> = scaled
            .get_property("data")
            .iter()
            .map(|value| value.to_u32())
            .collect();
        assert_eq!(&scaled_bytes[0..4], &[255, 0, 0, 255]);
        assert_eq!(&scaled_bytes[76..80], &[0, 0, 255, 255]);

        ctx.call_method(
            "drawImage",
            vec![
                source,
                Value::Number(1.0),
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(1.0),
                Value::Number(80.0),
                Value::Number(0.0),
                Value::Number(5.0),
                Value::Number(5.0),
            ],
        );
        let cropped = ctx.call_method(
            "getImageData",
            vec![
                Value::Number(80.0),
                Value::Number(0.0),
                Value::Number(1.0),
                Value::Number(1.0),
            ],
        );
        let cropped_bytes: Vec<u32> = cropped
            .get_property("data")
            .iter()
            .map(|value| value.to_u32())
            .collect();
        assert_eq!(cropped_bytes, [0, 0, 255, 255]);

        let webgl = canvas.call_method("getContext", vec![Value::string("webgl")]);
        assert!(w3cos_core::class::instance_of(
            &webgl,
            &crate::webgl_web::class_for("WebGLRenderingContext")
        ));
    }

    #[test]
    fn range_and_selection() {
        setup();
        let doc = document_value();
        let div = create_in_body("div");
        let text = doc.call_method("createTextNode", vec![Value::string("Hello World")]);
        div.call_method("appendChild", vec![text.clone()]);

        let range = doc.call_method("createRange", vec![]);
        range.call_method("setStart", vec![text.clone(), Value::Number(0.0)]);
        range.call_method("setEnd", vec![text.clone(), Value::Number(5.0)]);
        assert!(!range.get_property("collapsed").to_bool());
        assert_eq!(
            range.call_method("toString", vec![]).to_js_string(),
            "Hello"
        );
        assert!(range.get_property("startContainer") == text);

        let sel = win_selection();
        sel.call_method("removeAllRanges", vec![]);
        assert_eq!(sel.get_property("rangeCount").to_number(), 0.0);
        sel.call_method("addRange", vec![range]);
        assert_eq!(sel.get_property("rangeCount").to_number(), 1.0);
        assert_eq!(sel.call_method("toString", vec![]).to_js_string(), "Hello");
        let r0 = sel.call_method("getRangeAt", vec![Value::Number(0.0)]);
        assert_eq!(r0.get_property("startOffset").to_number(), 0.0);
        sel.call_method("removeAllRanges", vec![]);
    }

    #[cfg(not(feature = "web-graphics-advanced"))]
    #[test]
    fn advanced_web_graphics_globals_are_absent_when_pruned() {
        setup();
        let window = window_value();
        for name in [
            "GPUAdapter",
            "WebGLRenderingContext",
            "XRSession",
            "VideoDecoder",
            "ImageDecoder",
            "MediaStreamTrackProcessor",
        ] {
            assert!(
                window.get_property(name).is_undefined(),
                "{name} must not be exposed without web-graphics-advanced"
            );
        }
        let navigator = window.get_property("navigator");
        assert!(navigator.get_property("gpu").is_undefined());
        assert!(navigator.get_property("xr").is_undefined());

        let canvas = document_value().call_method("createElement", vec![Value::string("canvas")]);
        assert!(
            canvas
                .call_method("getContext", vec![Value::string("webgl")])
                .is_null()
        );
    }

    #[cfg(not(feature = "web-media-advanced"))]
    #[test]
    fn advanced_media_is_pruned_without_removing_speech_recognition() {
        setup();
        let window = window_value();
        for name in [
            "AudioContext",
            "MediaRecorder",
            "MediaSource",
            "MediaStream",
            "RTCPeerConnection",
        ] {
            assert!(window.get_property(name).is_undefined(), "{name}");
        }
        assert!(window.get_property("SpeechRecognition").is_function());
        let navigator = window.get_property("navigator");
        assert!(navigator.get_property("mediaDevices").is_undefined());
        assert!(navigator.get_property("mediaSession").is_undefined());
    }

    #[test]
    fn range_fragments_insert_and_surround_preserve_dom_nodes() {
        setup();
        let window = window_value();
        let document = document_value();
        let container = create_in_body("div");
        let text = document.call_method("createTextNode", vec![Value::string("hello world")]);
        container.call_method("appendChild", vec![text.clone()]);
        let range = document.call_method("createRange", vec![]);
        range.call_method("setStart", vec![text.clone(), Value::Number(0.0)]);
        range.call_method("setEnd", vec![text.clone(), Value::Number(5.0)]);

        let cloned = range.call_method("cloneContents", vec![]);
        assert!(w3cos_core::class::instance_of(
            &cloned,
            &window.get_property("DocumentFragment")
        ));
        assert_eq!(cloned.get_property("textContent").to_js_string(), "hello");
        assert_eq!(
            text.get_property("textContent").to_js_string(),
            "hello world"
        );

        let extracted = range.call_method("extractContents", vec![]);
        assert_eq!(
            extracted.get_property("textContent").to_js_string(),
            "hello"
        );
        assert_eq!(text.get_property("textContent").to_js_string(), " world");
        assert!(range.get_property("collapsed").to_bool());

        let emphasis = document.call_method("createElement", vec![Value::string("em")]);
        emphasis.set_property("textContent", Value::string("X"));
        range.call_method("insertNode", vec![emphasis]);
        assert_eq!(
            container.get_property("textContent").to_js_string(),
            "X world"
        );

        let host = create_in_body("section");
        host.set_property("innerHTML", Value::string("<span>A</span><span>B</span>"));
        let surround = document.call_method("createRange", vec![]);
        surround.call_method("setStart", vec![host.clone(), Value::Number(0.0)]);
        surround.call_method("setEnd", vec![host.clone(), Value::Number(2.0)]);
        let first = host
            .get_property("children")
            .call_method("item", vec![Value::Number(0.0)]);
        let second = host
            .get_property("children")
            .call_method("item", vec![Value::Number(1.0)]);
        dom::with_document_mut(|doc| {
            doc.set_layout_rect(
                NodeId::from_u32(node_id_of(&first).unwrap()),
                w3cos_dom::DOMRect::new(10.0, 20.0, 40.0, 10.0),
            );
            doc.set_layout_rect(
                NodeId::from_u32(node_id_of(&second).unwrap()),
                w3cos_dom::DOMRect::new(50.0, 25.0, 50.0, 25.0),
            );
        });
        let client_rects = surround.call_method("getClientRects", vec![]);
        assert_eq!(client_rects.get_property("length").to_u32(), 2);
        assert!(w3cos_core::class::instance_of(
            &client_rects,
            &crate::geometry_web::class("DOMRectList")
        ));
        let bounds = surround.call_method("getBoundingClientRect", vec![]);
        assert_eq!(bounds.get_property("x").to_number(), 10.0);
        assert_eq!(bounds.get_property("y").to_number(), 20.0);
        assert_eq!(bounds.get_property("width").to_number(), 90.0);
        assert_eq!(bounds.get_property("height").to_number(), 30.0);
        let wrapper = document.call_method("createElement", vec![Value::string("strong")]);
        surround.call_method("surroundContents", vec![wrapper.clone()]);
        assert_eq!(
            wrapper
                .get_property("children")
                .get_property("length")
                .to_u32(),
            2
        );
        assert!(surround.get_property("startContainer") == host);
        assert_eq!(surround.get_property("startOffset").to_u32(), 0);
        assert_eq!(surround.get_property("endOffset").to_u32(), 1);

        let contextual = surround.call_method(
            "createContextualFragment",
            vec![Value::string("<i>context</i>")],
        );
        assert_eq!(
            contextual
                .call_method("querySelector", vec![Value::string("i")])
                .get_property("textContent")
                .to_js_string(),
            "context"
        );
    }

    fn win_selection() -> Value {
        window_value().call_method("getSelection", vec![])
    }

    #[test]
    fn bounding_client_rect_flushes_pending_style_and_layout() {
        setup();
        let div = create_in_body("div");
        div.get_property("style")
            .set_property("width", Value::string("100px"));
        let rect = div.call_method("getBoundingClientRect", vec![]);
        assert_eq!(rect.get_property("width").to_number(), 100.0);

        div.get_property("style")
            .set_property("width", Value::string("200px"));
        let rect = div.call_method("getBoundingClientRect", vec![]);
        assert_eq!(rect.get_property("width").to_number(), 200.0);
    }

    #[test]
    fn focus_and_active_element() {
        setup();
        let doc = document_value();
        let input = create_in_body("input");
        input.call_method("focus", vec![]);
        assert!(doc.get_property("activeElement") == input);
        input.call_method("blur", vec![]);
        assert!(doc.get_property("activeElement") == doc.get_property("body"));
    }

    #[test]
    fn input_value_and_checked_map_to_attributes() {
        setup();
        let input = create_in_body("input");
        input.set_property("value", Value::string("typed"));
        assert_eq!(input.get_property("value").to_js_string(), "typed");
        assert_eq!(
            dom::get_attribute(node_id_of(&input).unwrap(), "value").as_deref(),
            Some("typed")
        );
        input.set_property("checked", Value::Bool(true));
        assert!(input.get_property("checked").to_bool());
        input.set_property("checked", Value::Bool(false));
        assert!(!input.get_property("checked").to_bool());
    }

    #[test]
    fn boolean_control_properties_remove_reflected_attributes_when_cleared() {
        setup();
        let button = create_in_body("button");
        button.call_method(
            "setAttribute",
            vec![Value::string("disabled"), Value::string("")],
        );
        assert!(button.get_property("disabled").to_bool());

        button.set_property("disabled", Value::Bool(false));

        assert!(!button.get_property("disabled").to_bool());
        assert!(
            !button
                .call_method("hasAttribute", vec![Value::string("disabled")])
                .to_bool()
        );

        let input = create_in_body("input");
        input.set_property("readOnly", Value::Bool(true));
        assert!(input.get_property("readOnly").to_bool());
        input.set_property("readOnly", Value::Bool(false));
        assert!(!input.get_property("readOnly").to_bool());
    }

    #[test]
    fn input_type_property_reflects_to_the_type_attribute() {
        setup();
        let input = create_in_body("input");
        assert_eq!(input.get_property("type").to_js_string(), "text");

        input.set_property("type", Value::string("password"));

        assert_eq!(input.get_property("type").to_js_string(), "password");
        assert_eq!(
            dom::get_attribute(node_id_of(&input).unwrap(), "type").as_deref(),
            Some("password")
        );
    }

    #[test]
    fn text_control_edit_uses_utf16_selection() {
        setup();
        let input = create_in_body("textarea");
        input.set_property("value", Value::string("a😀c"));
        let node = node_id_of(&input).unwrap();

        assert_eq!(
            text_control_value_after_edit(node, "X", "insertText"),
            "Xa😀c"
        );

        input.set_property("selectionStart", Value::Number(1.0));
        input.set_property("selectionEnd", Value::Number(3.0));
        assert_eq!(
            text_control_value_after_edit(node, "X", "insertText"),
            "aXc"
        );

        input.set_property("selectionStart", Value::Number(3.0));
        input.set_property("selectionEnd", Value::Number(3.0));
        assert_eq!(
            text_control_value_after_edit(node, "", "deleteContentBackward"),
            "ac"
        );
    }

    #[test]
    fn text_control_selection_and_set_range_text_follow_utf16_rules() {
        setup();
        let input = create_in_body("textarea");
        input.set_property("value", Value::string("a😀cd"));

        input.call_method(
            "setSelectionRange",
            vec![
                Value::Number(1.0),
                Value::Number(3.0),
                Value::string("backward"),
            ],
        );
        assert_eq!(input.get_property("selectionStart").to_number(), 1.0);
        assert_eq!(input.get_property("selectionEnd").to_number(), 3.0);
        assert_eq!(
            input.get_property("selectionDirection").to_js_string(),
            "backward"
        );

        input.call_method("setRangeText", vec![Value::string("XY")]);
        assert_eq!(input.get_property("value").to_js_string(), "aXYcd");
        assert_eq!(input.get_property("selectionStart").to_number(), 1.0);
        assert_eq!(input.get_property("selectionEnd").to_number(), 3.0);
        assert_eq!(
            input.get_property("selectionDirection").to_js_string(),
            "backward"
        );

        input.call_method(
            "setRangeText",
            vec![
                Value::string("Z"),
                Value::Number(1.0),
                Value::Number(3.0),
                Value::string("select"),
            ],
        );
        assert_eq!(input.get_property("value").to_js_string(), "aZcd");
        assert_eq!(input.get_property("selectionStart").to_number(), 1.0);
        assert_eq!(input.get_property("selectionEnd").to_number(), 2.0);

        input.call_method("select", vec![]);
        assert_eq!(input.get_property("selectionStart").to_number(), 0.0);
        assert_eq!(input.get_property("selectionEnd").to_number(), 4.0);

        input.call_method(
            "setSelectionRange",
            vec![Value::Number(4.0), Value::Number(2.0)],
        );
        assert_eq!(input.get_property("selectionStart").to_number(), 2.0);
        assert_eq!(input.get_property("selectionEnd").to_number(), 2.0);
        assert_eq!(
            input.get_property("selectionDirection").to_js_string(),
            "none"
        );
    }

    #[test]
    fn local_storage_roundtrip() {
        setup();
        let win = window_value();
        let ls = win.get_property("localStorage");
        let key = "jsdom-test-key";
        ls.call_method("removeItem", vec![Value::string(key)]);
        assert!(
            ls.call_method("getItem", vec![Value::string(key)])
                .is_null()
        );
        ls.call_method("setItem", vec![Value::string(key), Value::string("v1")]);
        assert_eq!(
            ls.call_method("getItem", vec![Value::string(key)])
                .to_js_string(),
            "v1"
        );
        ls.call_method("removeItem", vec![Value::string(key)]);
        let ss = win.get_property("sessionStorage");
        ss.call_method("setItem", vec![Value::string("k"), Value::string("v")]);
        assert_eq!(
            ss.call_method("getItem", vec![Value::string("k")])
                .to_js_string(),
            "v"
        );
        assert_eq!(ss.get_property("length").to_number(), 1.0);
    }

    #[test]
    fn owner_document_and_root_node() {
        setup();
        let div = create_in_body("div");
        assert!(div.get_property("ownerDocument") == document_value());
        assert!(div.call_method("getRootNode", vec![]) == document_value());
        assert!(div.get_property("isConnected").to_bool());
        let detached = document_value().call_method("createElement", vec![Value::string("div")]);
        assert!(!detached.get_property("isConnected").to_bool());
    }

    #[test]
    fn get_computed_style_returns_inline_style() {
        setup();
        let win = window_value();
        let div = create_in_body("div");
        div.get_property("style")
            .set_property("width", Value::string("42px"));
        div.get_property("style")
            .set_property("backgroundColor", Value::string("#008000"));
        let cs = win.call_method("getComputedStyle", vec![div]);
        assert_eq!(cs.get_property("width").to_js_string(), "42px");
        assert_eq!(
            cs.get_property("backgroundColor").to_js_string(),
            "rgb(0, 128, 0)"
        );
        assert_eq!(
            serialize_computed_style_property("color", "rgba(255, 0, 0, 0.5)"),
            "rgba(255, 0, 0, 0.502)"
        );
    }

    #[test]
    fn inline_style_canonicalizes_and_rejects_color_values() {
        setup();
        let div = create_in_body("div");
        let style = div.get_property("style");

        style.set_property("color", Value::string("#009"));
        assert_eq!(
            style.call_method("getPropertyValue", vec![Value::string("color")]),
            Value::string("rgb(0, 0, 153)")
        );

        style.set_property("color", Value::string("rgb(1%, 40%, 101%)"));
        assert_eq!(
            style.call_method("getPropertyValue", vec![Value::string("color")]),
            Value::string("rgb(3, 102, 255)")
        );

        style.set_property("color", Value::string("#00g"));
        assert_eq!(
            style.call_method("getPropertyValue", vec![Value::string("color")]),
            Value::string("rgb(3, 102, 255)")
        );
    }

    #[test]
    fn computed_style_exposes_discrete_css2_properties_and_wide_keywords() {
        setup();
        let parent = create_in_body("div");
        let child = document_value().call_method("createElement", vec![Value::string("div")]);
        parent.call_method("appendChild", vec![child.clone()]);
        let parent_style = parent.get_property("style");
        let child_style = child.get_property("style");

        assert!(css_property_supported("float", "initial"));
        assert_eq!(
            window_value()
                .call_method("getComputedStyle", vec![child.clone()])
                .get_property("float"),
            Value::string("none")
        );
        parent_style.set_property("cssFloat", Value::string("left"));
        child_style.set_property("cssFloat", Value::string("inherit"));
        assert_eq!(
            window_value()
                .call_method("getComputedStyle", vec![child.clone()])
                .get_property("float"),
            Value::string("left")
        );
        child_style.set_property("cssFloat", Value::string("unset"));
        assert_eq!(
            window_value()
                .call_method("getComputedStyle", vec![child])
                .get_property("float"),
            Value::string("none")
        );
    }

    #[test]
    fn scroll_offsets() {
        setup();
        let div = create_in_body("div");
        div.set_property("scrollTop", Value::Number(33.0));
        div.set_property("scrollLeft", Value::Number(7.0));
        assert_eq!(div.get_property("scrollTop").to_number(), 33.0);
        assert_eq!(div.get_property("scrollLeft").to_number(), 7.0);
    }

    #[test]
    fn scroll_extent_includes_descendant_layout_beyond_client_box() {
        setup();
        let document = document_value();
        let container = create_in_body("div");
        let target = document.call_method("createElement", vec![Value::string("div")]);
        container.call_method("appendChild", vec![target.clone()]);
        let container_id = node_id_of(&container).unwrap();
        let target_id = node_id_of(&target).unwrap();
        dom::with_document_mut(|document| {
            document.set_layout_rect(
                NodeId::from_u32(container_id),
                w3cos_dom::DOMRect::new(10.0, 20.0, 100.0, 100.0),
            );
            document.set_layout_rect(
                NodeId::from_u32(target_id),
                w3cos_dom::DOMRect::new(10.0, 250.0, 180.0, 40.0),
            );
        });
        assert_eq!(container.get_property("clientWidth").to_number(), 100.0);
        assert_eq!(container.get_property("clientHeight").to_number(), 100.0);
        assert_eq!(container.get_property("scrollWidth").to_number(), 180.0);
        assert_eq!(container.get_property("scrollHeight").to_number(), 270.0);
    }

    #[test]
    fn scroll_extent_flushes_layout_before_first_paint() {
        setup();
        set_viewport(100.0, 100.0);
        let document = document_value();
        let container = create_in_body("div");
        let container_style = container.get_property("style");
        container_style.set_property("width", Value::string("100px"));
        container_style.set_property("height", Value::string("100px"));
        container_style.set_property("overflowY", Value::string("auto"));
        let target = document.call_method("createElement", vec![Value::string("div")]);
        let target_style = target.get_property("style");
        target_style.set_property("width", Value::string("100px"));
        target_style.set_property("height", Value::string("270px"));
        container.call_method("appendChild", vec![target]);

        assert_eq!(container.get_property("scrollHeight").to_number(), 270.0);
    }

    #[test]
    fn scroll_into_view_aligns_nearest_scroll_container() {
        setup();
        let document = document_value();
        let container = create_in_body("div");
        container
            .get_property("style")
            .set_property("overflowY", Value::string("auto"));
        let target = document.call_method("createElement", vec![Value::string("button")]);
        container.call_method("appendChild", vec![target.clone()]);
        let container_id = node_id_of(&container).unwrap();
        let target_id = node_id_of(&target).unwrap();
        dom::with_document_mut(|document| {
            document.set_layout_rect(
                NodeId::from_u32(container_id),
                w3cos_dom::DOMRect::new(0.0, 0.0, 100.0, 100.0),
            );
            document.set_layout_rect(
                NodeId::from_u32(target_id),
                w3cos_dom::DOMRect::new(0.0, 250.0, 80.0, 20.0),
            );
        });
        let scroll_events = Rc::new(Cell::new(0));
        let events_for_handler = Rc::clone(&scroll_events);
        container.call_method(
            "addEventListener",
            vec![
                Value::string("scroll"),
                func(move |_, _| {
                    events_for_handler.set(events_for_handler.get() + 1);
                    Value::Undefined
                }),
            ],
        );
        target.call_method(
            "scrollIntoView",
            vec![Value::object(HashMap::from([
                ("block".to_string(), Value::string("end")),
                ("container".to_string(), Value::string("nearest")),
            ]))],
        );
        assert_eq!(container.get_property("scrollTop").to_number(), 170.0);
        assert_eq!(scroll_events.get(), 1);
    }
}
